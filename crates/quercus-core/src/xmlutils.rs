// Quercus Video Editor - Non-Linear Video Editor
// Copyright (C) 2026 Quercus Team
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! Streaming XML reader/writer, mirroring `src/common/src/xmlutils.h`
//! and `include/common/xmlutils.h`. The C++-only
//! `get_native` / `wrap_native` entry points deal with
//! `olive::XmlStreamReader` / `olive::XmlStreamWriter` and are served by
//! the C++ adapter layer, not here.

use crate::error::{Error, Result};
use std::str;

use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::BytesStart;
use quick_xml::events::Event as QxEvent;
use quick_xml::Reader as QxReader;

// CPP-PARITY: The C++ reader is backed by expat, which parses the whole
// document into an event list at construction. This Rust port parses the
// document with quick-xml and folds its event stream (elements, attributes,
// character data; comments, processing instructions and CDATA handled,
// namespace processing not performed) into the same expat-shaped event list
// so that navigation and error reporting match expat.

/// One token type, mirroring `olive::XmlStreamReader::TokenType`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TokenType {
	/// A parse error occurred.
	Invalid,
	StartElement,
	EndElement,
	Characters,
	EndDocument,
}

/// A single XML attribute (name/value pair), `olive::XmlStreamAttribute`.
#[derive(Clone, Debug)]
struct Attribute {
	name: String,
	value: String,
}

/// One parsed SAX-style event, `olive::XmlStreamReader::Event`.
#[derive(Clone, Debug)]
struct Event {
	token_type: TokenType,
	name: String,
	text: String,
	attributes: Vec<Attribute>,
}

/// `olive::XmlStreamReader` — a handle-wrapped streaming XML reader.
pub struct XmlReader {
	/// Parsed event list (the whole document is parsed at construction).
	events: Vec<Event>,
	/// Index of the next event to consume.
	pos: usize,
	/// Current token type.
	token: TokenType,
	/// Current element name.
	name: String,
	/// Current character data.
	text: String,
	/// Current attributes.
	attributes: Vec<Attribute>,
	/// Whether the document failed to parse.
	has_error: bool,
}

impl XmlReader {
	/// Create a streaming reader over a complete document.
	///
	/// `data` is parsed up front into an expat-equivalent event list (see
	/// [`parse_document`]). Malformed input does not fail here: the reader is
	/// always returned and positions itself on the token stream, while the parse
	/// error is reported later through [`XmlReader::has_error`] and makes further
	/// reads yield `TokenType::Invalid`.
	pub fn new(data: &str) -> Result<Self> {
		let (events, has_error) = parse_document(data);
		// CPP-PARITY: The C++ constructor reports parse failures through
		// has_error() rather than throwing, so `new` always yields a usable
		// reader whose has_error flag the caller inspects afterwards. The
		// C++ error_string() is not part of the Rust domain surface (the
		// c_api only exposes has_error), so the message is not retained.
		Ok(Self {
			events,
			pos: 0,
			token: TokenType::Invalid,
			name: String::new(),
			text: String::new(),
			attributes: Vec::new(),
			has_error,
		})
	}

	/// Advance to the next event token (mirrors `read_next()`).
	fn read_next(&mut self) -> TokenType {
		self.attributes.clear();
		self.text.clear();
		self.name.clear();

		if self.has_error {
			self.token = TokenType::Invalid;
			return self.token;
		}

		if self.pos >= self.events.len() {
			self.token = TokenType::EndDocument;
			return self.token;
		}

		let ev = &self.events[self.pos];
		self.pos += 1;
		self.token = ev.token_type;
		self.name = ev.name.clone();
		self.text = ev.text.clone();
		self.attributes = ev.attributes.clone();
		self.token
	}

	/// Whether the reader is currently on a start element.
	fn is_start_element(&self) -> bool {
		self.token == TokenType::StartElement
	}

	/// Whether the reader is currently on an end element.
	fn is_end_element(&self) -> bool {
		self.token == TokenType::EndElement
	}

	/// Advance until the next start element, an end element, or the end of
	/// the document. Returns whether the reader is now positioned on a
	/// start element.
	pub fn read_next_start_element(&mut self) -> Result<bool> {
		// CPP-PARITY: mirrors the free function xml_read_next_start_element():
		// stops (returns false) on an end element or end of document.
		loop {
			let token = self.read_next();
			if token == TokenType::Invalid || token == TokenType::EndDocument {
				return Ok(false);
			}
			if self.is_end_element() {
				return Ok(false);
			}
			if self.is_start_element() {
				return Ok(true);
			}
		}
	}

	/// Name of the current element token.
	pub fn name(&self) -> Result<String> {
		Ok(self.name.clone())
	}

	/// Read the concatenated character data of the current element. Must
	/// be called on a start element; consumes up to the matching end
	/// element.
	pub fn read_element_text(&mut self) -> Result<String> {
		if !self.is_start_element() {
			// CPP-PARITY: C++ returns an empty string when not on a start
			// element.
			return Ok(String::new());
		}

		let mut result = String::new();
		let mut depth = 1;
		loop {
			let token = self.read_next();
			if token == TokenType::Invalid || token == TokenType::EndDocument {
				break;
			}
			if token == TokenType::StartElement {
				depth += 1;
			} else if token == TokenType::EndElement {
				depth -= 1;
				if depth == 0 {
					break;
				}
			} else if token == TokenType::Characters && depth == 1 {
				result.push_str(&self.text);
			}
		}
		Ok(result)
	}

	/// Skip the current element and all of its children.
	pub fn skip_current_element(&mut self) -> Result<()> {
		if !self.is_start_element() {
			return Ok(());
		}

		let mut depth = 1;
		loop {
			let token = self.read_next();
			if token == TokenType::Invalid || token == TokenType::EndDocument {
				break;
			}
			if token == TokenType::StartElement {
				depth += 1;
			} else if token == TokenType::EndElement {
				depth -= 1;
				if depth == 0 {
					break;
				}
			}
		}
		Ok(())
	}

	/// Number of attributes on the current start element.
	pub fn attribute_count(&self) -> Result<i32> {
		Ok(self.attributes.len() as i32)
	}

	/// Name of the attribute at `index` on the current start element.
	pub fn attribute_name(&self, index: i32) -> Result<String> {
		if index < 0 || index as usize >= self.attributes.len() {
			// CPP-PARITY: the c_api returns E_NOT_FOUND for an out-of-range
			// attribute index.
			return Err(Error::NotFound);
		}
		Ok(self.attributes[index as usize].name.clone())
	}

	/// Value of the attribute at `index` on the current start element.
	pub fn attribute_value(&self, index: i32) -> Result<String> {
		if index < 0 || index as usize >= self.attributes.len() {
			return Err(Error::NotFound);
		}
		Ok(self.attributes[index as usize].value.clone())
	}

	/// Whether the document failed to parse.
	pub fn has_error(&self) -> Result<bool> {
		Ok(self.has_error)
	}
}

/// `olive::XmlStreamWriter` — a handle-wrapped streaming XML writer.
pub struct XmlWriter {
	/// Document built so far.
	output: String,
	/// Open element name stack.
	stack: Vec<String>,
	/// A `'>'` is pending for the open start tag.
	open_start_tag: bool,
}

impl XmlWriter {
	/// Create a streaming XML writer.
	pub fn new() -> Self {
		Self {
			output: String::new(),
			stack: Vec::new(),
			open_start_tag: false,
		}
	}

	/// Open a new element in the output.
	///
	/// Flushes a pending `>` for the previous start tag, appends `<name` and pushes
	/// `name` on the open-element stack; the tag is closed later by
	/// [`XmlWriter::write_characters`] or [`XmlWriter::write_end_element`] (which
	/// may turn it into `<name/>`). `name` is written verbatim, without validation
	/// or escaping. Returns `Ok`; the fallible signature mirrors the C++ writer.
	pub fn write_start_element(&mut self, name: &str) -> Result<()> {
		if self.open_start_tag {
			self.output.push('>');
			self.open_start_tag = false;
		}
		self.output.push('<');
		self.output.push_str(name);
		self.stack.push(name.to_string());
		self.open_start_tag = true;
		Ok(())
	}

	/// Write an attribute onto the currently open start tag.
	///
	/// `name` is written verbatim (not validated); `value` is escaped with
	/// [`escape_attribute`], which also escapes `"`. When no start tag is open the
	/// call is a no-op returning `Ok(())` (C++ parity), so a misordered write does
	/// not corrupt the output.
	pub fn write_attribute(&mut self, name: &str, value: &str) -> Result<()> {
		// CPP-PARITY: no-op when not inside an open start tag.
		if !self.open_start_tag {
			return Ok(());
		}
		self.output.push(' ');
		self.output.push_str(name);
		self.output.push_str("=\"");
		self.output.push_str(&escape_attribute(value));
		self.output.push('"');
		Ok(())
	}

	/// Append character data to the output.
	///
	/// Closes a pending start tag first, then escapes `text` with [`escape_text`]
	/// (only `&`, `<` and `>` are substituted). Quotes are not escaped here; use
	/// [`XmlWriter::write_attribute`] for attribute values.
	pub fn write_characters(&mut self, text: &str) -> Result<()> {
		if self.open_start_tag {
			self.output.push('>');
			self.open_start_tag = false;
		}
		self.output.push_str(&escape_text(text));
		Ok(())
	}

	/// Write an empty element with character data (`<name>text</name>`).
	pub fn write_text_element(&mut self, name: &str, text: &str) -> Result<()> {
		self.write_start_element(name)?;
		self.write_characters(text)?;
		self.write_end_element()?;
		Ok(())
	}

	/// Close the innermost open element.
	///
	/// Pops the name stack: if its start tag is still open, it becomes an empty
	/// element (`<name/>`); otherwise a `</name>` end tag is appended. A call with
	/// an empty stack is a no-op returning `Ok(())` (C++ parity). Mismatched
	/// nesting is not detected; callers must close elements in order.
	pub fn write_end_element(&mut self) -> Result<()> {
		// CPP-PARITY: no-op when the element stack is empty.
		let name = match self.stack.pop() {
			Some(n) => n,
			None => return Ok(()),
		};

		if self.open_start_tag {
			self.output.push_str("/>");
			self.open_start_tag = false;
		} else {
			self.output.push_str("</");
			self.output.push_str(&name);
			self.output.push('>');
		}
		Ok(())
	}

	/// Close every element still open.
	///
	/// Pops and closes the writer's element stack until it is empty, so callers
	/// that did not pair every [`XmlWriter::write_start_element`] with an end
	/// element still get well-formed output. Does not append an XML declaration or
	/// prolog.
	pub fn write_end_document(&mut self) -> Result<()> {
		while !self.stack.is_empty() {
			self.write_end_element()?;
		}
		Ok(())
	}

	/// The document written so far.
	pub fn output(&self) -> Result<String> {
		Ok(self.output.clone())
	}
}

/// Escape character data for XML output.
///
/// Replaces `&`, `<` and `>` with `&amp;`, `&lt;` and `&gt;`; quotes are left
/// alone (only [`escape_attribute`] escapes `"`). Used by
/// [`XmlWriter::write_characters`].
fn escape_text(s: &str) -> String {
	let mut out = String::with_capacity(s.len());
	for c in s.chars() {
		match c {
			'&' => out.push_str("&amp;"),
			'<' => out.push_str("&lt;"),
			'>' => out.push_str("&gt;"),
			_ => out.push(c),
		}
	}
	out
}

/// Escape an attribute value for XML output.
///
/// Replaces `&`, `<`, `>` and `"` with their predefined entities so the value
/// can be emitted inside double quotes. Single quotes are not escaped (values
/// are always written with `"`). Used by [`XmlWriter::write_attribute`].
fn escape_attribute(s: &str) -> String {
	let mut out = String::with_capacity(s.len());
	for c in s.chars() {
		match c {
			'&' => out.push_str("&amp;"),
			'<' => out.push_str("&lt;"),
			'>' => out.push_str("&gt;"),
			'"' => out.push_str("&quot;"),
			_ => out.push(c),
		}
	}
	out
}

/// Whether `c` is XML whitespace.
///
/// True for space, tab, carriage return and line feed (the four characters XML
/// treats as whitespace); used to decide whether prolog/epilog text is merely
/// pretty-printing and can be ignored.
fn is_ws(c: char) -> bool {
	c == ' ' || c == '\t' || c == '\r' || c == '\n'
}

/// Convert a quick-xml `QName` (raw bytes) to an owned UTF-8 string. The
/// input is always valid UTF-8 (the reader was built from a `&str`).
fn qname_string(q: impl AsRef<[u8]>) -> String {
	str::from_utf8(q.as_ref())
		.map(str::to_owned)
		.unwrap_or_default()
}

/// Append character data, merging consecutive Characters events
/// (mirrors `on_characters` in C++).
fn push_characters(events: &mut Vec<Event>, text: &str) {
	if text.is_empty() {
		return;
	}
	if let Some(last) = events.last_mut() {
		if last.token_type == TokenType::Characters {
			last.text.push_str(text);
			return;
		}
	}
	events.push(Event {
		token_type: TokenType::Characters,
		name: String::new(),
		text: text.to_string(),
		attributes: Vec::new(),
	});
}

/// Push a `StartElement` event (and its attributes) for a quick-xml start
/// or empty tag. On success returns the element name with the stack already
/// pushed; on a parse error returns `None` with `has_error` semantics left
/// to the caller (nothing was pushed).
fn push_start_element(
	e: &BytesStart,
	events: &mut Vec<Event>,
	stack: &mut Vec<String>,
	saw_start: &mut bool,
) -> Option<String> {
	let name = qname_string(e.name());
	let mut attributes: Vec<Attribute> = Vec::new();
	for attr in e.attributes() {
		let attr = match attr {
			Ok(a) => a,
			Err(_) => return None,
		};
		if attr.value.contains(&b'<') {
			// CPP-PARITY: expat rejects a raw '<' inside an attribute
			// value (XML_ERROR_INVALID_TOKEN).
			return None;
		}
		let value = match attr.normalized_value(quick_xml::XmlVersion::Implicit1_0) {
			Ok(v) => v.into_owned(),
			Err(_) => return None,
		};
		attributes.push(Attribute {
			name: qname_string(attr.key),
			value,
		});
	}
	if stack.is_empty() && *saw_start {
		// CPP-PARITY: expat reports "junk after document element" for a
		// second root element.
		return None;
	}
	*saw_start = true;
	stack.push(name.clone());
	events.push(Event {
		token_type: TokenType::StartElement,
		name: name.clone(),
		text: String::new(),
		attributes,
	});
	Some(name)
}

/// Parse `data` with quick-xml into an expat-equivalent event list.
///
/// Returns `(events, has_error)`. The list contains `StartElement`,
/// `EndElement` and `Characters` tokens only: an empty element yields a
/// Start/End pair, character entities are resolved, CDATA is passed through
/// literally, and comments, processing instructions and the DOCTYPE are
/// skipped. `has_error` is set for malformed XML, mismatched or unclosed tags,
/// content outside the root element, and unknown entities; events parsed before
/// the first error are kept.
///
/// # Returns
///
/// * `events` - event list; may be partial when `has_error` is true.
/// * `has_error` - whether parsing failed (the reader then reports `TokenType::Invalid`).
fn parse_document(data: &str) -> (Vec<Event>, bool) {
	let mut events: Vec<Event> = Vec::new();
	let mut stack: Vec<String> = Vec::new();
	let mut saw_start = false;
	let mut has_error = false;

	let mut reader = QxReader::from_str(data);
	loop {
		let ev = match reader.read_event() {
			Ok(e) => e,
			Err(_) => {
				has_error = true;
				break;
			}
		};
		match ev {
			QxEvent::Start(e) => {
				if push_start_element(&e, &mut events, &mut stack, &mut saw_start).is_none() {
					has_error = true;
					break;
				}
			}
			QxEvent::Empty(e) => {
				let name = match push_start_element(&e, &mut events, &mut stack, &mut saw_start) {
					Some(n) => n,
					None => {
						has_error = true;
						break;
					}
				};
				stack.pop();
				// CPP-PARITY: expat emits StartElement then EndElement for
				// `<name/>`.
				events.push(Event {
					token_type: TokenType::EndElement,
					name,
					text: String::new(),
					attributes: Vec::new(),
				});
			}
			QxEvent::End(e) => {
				let name = qname_string(e.name());
				match stack.last() {
					Some(top) if *top == name => {
						stack.pop();
						events.push(Event {
							token_type: TokenType::EndElement,
							name,
							text: String::new(),
							attributes: Vec::new(),
						});
					}
					_ => {
						// CPP-PARITY: expat reports a mismatched tag. (quick-xml
						// also rejects this via check_end_names.)
						has_error = true;
						break;
					}
				}
			}
			QxEvent::Text(t) => {
				let text = t.decode().map(|c| c.into_owned()).unwrap_or_default();
				if stack.is_empty() {
					// CPP-PARITY: expat never reports character data outside the
					// root element; whitespace in the prolog/epilog is silently
					// allowed, anything else is an error ("junk after document
					// element").
					if !text.chars().all(is_ws) {
						has_error = true;
						break;
					}
				} else {
					push_characters(&mut events, &text);
				}
			}
			QxEvent::CData(c) => {
				if stack.is_empty() {
					// CPP-PARITY: expat rejects a CDATA section outside the root
					// element ("junk after document element").
					has_error = true;
					break;
				}
				// CPP-PARITY: CDATA content is literal, no entity resolution.
				let text = c.decode().map(|c| c.into_owned()).unwrap_or_default();
				push_characters(&mut events, &text);
			}
			QxEvent::GeneralRef(r) => {
				if stack.is_empty() {
					has_error = true;
					break;
				}
				if r.is_char_ref() {
					// Numeric character reference: `&#65;` / `&#x41;`.
					match r.resolve_char_ref() {
						Ok(Some(ch)) => {
							let mut s = String::new();
							s.push(ch);
							push_characters(&mut events, &s);
						}
						_ => {
							has_error = true;
							break;
						}
					}
				} else {
					let name = r.decode().map(|c| c.into_owned()).unwrap_or_default();
					match resolve_predefined_entity(&name) {
						Some(s) => push_characters(&mut events, s),
						None => {
							// Undefined entity: expat treats these as fatal.
							has_error = true;
							break;
						}
					}
				}
			}
			// Comments, declarations, processing instructions and DOCTYPE are
			// skipped (expat does not surface them through the character or
			// element handlers).
			QxEvent::Eof => break,
			_ => {}
		}
	}

	if !has_error && (!saw_start || !stack.is_empty()) {
		// CPP-PARITY: expat reports "no element found" for a document without
		// a root and an unclosed element otherwise.
		has_error = true;
	}

	(events, has_error)
}

#[cfg(test)]
mod tests;
