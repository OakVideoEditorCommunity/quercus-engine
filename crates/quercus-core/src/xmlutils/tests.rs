use super::*;

#[test]
fn writer_basic_document() {
	let mut w = XmlWriter::new();
	w.write_start_element("root").unwrap();
	w.write_attribute("a", "1").unwrap();
	w.write_attribute("b", "x\"y").unwrap();
	w.write_text_element("child", "hi & bye <there>").unwrap();
	w.write_end_element().unwrap();
	w.write_end_document().unwrap();
	assert_eq!(
		w.output().unwrap(),
		"<root a=\"1\" b=\"x&quot;y\"><child>hi &amp; bye &lt;there&gt;</child></root>"
	);
}

#[test]
fn writer_empty_and_noop() {
	let mut w = XmlWriter::new();
	// No open start tag: attribute is a no-op.
	w.write_attribute("a", "b").unwrap();
	// No stack: end element is a no-op.
	w.write_end_element().unwrap();
	w.write_characters("x").unwrap();
	w.write_end_document().unwrap();
	assert_eq!(w.output().unwrap(), "x");
}

#[test]
fn writer_self_closing_empty_element() {
	let mut w = XmlWriter::new();
	w.write_start_element("a").unwrap();
	w.write_attribute("k", "v").unwrap();
	w.write_end_element().unwrap();
	assert_eq!(w.output().unwrap(), "<a k=\"v\"/>");
}

#[test]
fn reader_basic_attributes_and_text() {
	let doc = "<root a=\"1\" b=\"2\"><child>x</child><empty/></root>";
	let mut r = XmlReader::new(doc).unwrap();
	assert!(!r.has_error().unwrap());
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.name().unwrap(), "root");
	assert_eq!(r.attribute_count().unwrap(), 2);
	assert_eq!(r.attribute_name(0).unwrap(), "a");
	assert_eq!(r.attribute_value(0).unwrap(), "1");
	assert_eq!(r.attribute_name(1).unwrap(), "b");
	assert_eq!(r.attribute_value(1).unwrap(), "2");

	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.name().unwrap(), "child");
	assert_eq!(r.read_element_text().unwrap(), "x");

	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.name().unwrap(), "empty");
	assert_eq!(r.attribute_count().unwrap(), 0);

	assert!(!r.read_next_start_element().unwrap());
}

#[test]
fn reader_nested_text_only_direct_children() {
	let doc = "<a><b>inner</b>outer</a>";
	let mut r = XmlReader::new(doc).unwrap();
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.name().unwrap(), "a");
	assert_eq!(r.read_element_text().unwrap(), "outer");
}

#[test]
fn reader_skip_current_element() {
	let doc = "<root><skip><deep>d</deep></skip><keep>v</keep></root>";
	let mut r = XmlReader::new(doc).unwrap();
	assert!(r.read_next_start_element().unwrap());
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.name().unwrap(), "skip");
	r.skip_current_element().unwrap();
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.name().unwrap(), "keep");
	assert_eq!(r.read_element_text().unwrap(), "v");
}

#[test]
fn reader_comments_pi_and_entities() {
	let doc = "<!-- c --><root a=\"x&quot;y\">&amp; &lt; &gt;</root>";
	let mut r = XmlReader::new(doc).unwrap();
	assert!(!r.has_error().unwrap());
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.name().unwrap(), "root");
	assert_eq!(r.attribute_value(0).unwrap(), "x\"y");
	assert_eq!(r.read_element_text().unwrap(), "& < >");
}

#[test]
fn reader_cdata() {
	let doc = "<a><![CDATA[x < y & z]]></a>";
	let mut r = XmlReader::new(doc).unwrap();
	assert!(!r.has_error().unwrap());
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.read_element_text().unwrap(), "x < y & z");
}

#[test]
fn reader_whitespace_between_elements() {
	// Indentation/whitespace becomes character data, which
	// read_element_text() collects at depth 1.
	let doc = "<a>\n  <b/>\n</a>";
	let mut r = XmlReader::new(doc).unwrap();
	assert!(!r.has_error().unwrap());
	assert!(r.read_next_start_element().unwrap());
	let text = r.read_element_text().unwrap();
	assert!(text.contains("\n  ") || text.trim().is_empty());
}

#[test]
fn reader_mismatched_tag_is_error() {
	let mut r = XmlReader::new("<a></b>").unwrap();
	assert!(r.has_error().unwrap());
	// Once in error state, navigation stops returning start elements.
	assert!(!r.read_next_start_element().unwrap());
}

#[test]
fn reader_unclosed_element_is_error() {
	let r = XmlReader::new("<a>").unwrap();
	assert!(r.has_error().unwrap());
}

#[test]
fn reader_empty_document_is_error() {
	let r = XmlReader::new("").unwrap();
	assert!(r.has_error().unwrap());
}

#[test]
fn reader_undefined_entity_is_error() {
	let r = XmlReader::new("<a>&foo;</a>").unwrap();
	assert!(r.has_error().unwrap());
}

#[test]
fn reader_attribute_out_of_range_is_not_found() {
	let mut r = XmlReader::new("<a x=\"1\"></a>").unwrap();
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.attribute_count().unwrap(), 1);
	assert!(r.attribute_name(5).is_err());
	assert!(r.attribute_name(-1).is_err());
	assert!(r.attribute_value(1).is_err());
}

#[test]
fn reader_name_before_read_is_empty() {
	let r = XmlReader::new("<a></a>").unwrap();
	assert_eq!(r.name().unwrap(), "");
	assert_eq!(r.attribute_count().unwrap(), 0);
}

#[test]
fn round_trip_writer_then_reader() {
	let mut w = XmlWriter::new();
	w.write_start_element("root").unwrap();
	w.write_text_element("a", "1").unwrap();
	w.write_text_element("b", "2").unwrap();
	w.write_end_element().unwrap();
	let xml = w.output().unwrap();
	assert_eq!(xml, "<root><a>1</a><b>2</b></root>");

	let mut r = XmlReader::new(&xml).unwrap();
	assert!(!r.has_error().unwrap());
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.name().unwrap(), "root");
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.name().unwrap(), "a");
	assert_eq!(r.read_element_text().unwrap(), "1");
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.name().unwrap(), "b");
	assert_eq!(r.read_element_text().unwrap(), "2");
	assert!(!r.read_next_start_element().unwrap());
}

// ---- Reader: attribute handling ----

/// Attribute order is preserved exactly as written (expat reports
/// attributes in document order via the `atts` array).
#[test]
fn reader_attribute_order_preserved() {
	let doc = "<e zeta=\"1\" alpha=\"2\" mid=\"3\" omega=\"4\"/>";
	let mut r = XmlReader::new(doc).unwrap();
	assert!(!r.has_error().unwrap());
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.attribute_count().unwrap(), 4);
	for (i, name) in ["zeta", "alpha", "mid", "omega"].iter().enumerate() {
		assert_eq!(r.attribute_name(i as i32).unwrap(), *name);
		assert_eq!(r.attribute_value(i as i32).unwrap(), format!("{}", i + 1));
	}
}

/// Single-quoted attribute values are accepted (expat allows both
/// quote styles).
#[test]
fn reader_single_quoted_attribute() {
	let doc = "<e a='v1' b='x&quot;y'/>";
	let mut r = XmlReader::new(doc).unwrap();
	assert!(!r.has_error().unwrap());
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.attribute_value(0).unwrap(), "v1");
	assert_eq!(r.attribute_value(1).unwrap(), "x\"y");
}

/// Numeric character references (decimal and hex) resolve in both text
/// and attribute values.
#[test]
fn reader_numeric_character_references() {
	let doc = "<e a=\"&#65;&#x42;\">&#x43;&#68;</e>";
	let mut r = XmlReader::new(doc).unwrap();
	assert!(!r.has_error().unwrap());
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.attribute_value(0).unwrap(), "AB");
	assert_eq!(r.read_element_text().unwrap(), "CD");
}

/// All five predefined entities resolve.
#[test]
fn reader_predefined_entities() {
	let doc = "<e>&amp;&lt;&gt;&quot;&apos;</e>";
	let mut r = XmlReader::new(doc).unwrap();
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.read_element_text().unwrap(), "&<>\"'");
}

/// A duplicate attribute is a well-formedness error in expat
/// (XML_ERROR_DUPLICATE_ATTRIBUTE).
#[test]
fn reader_duplicate_attribute_is_error() {
	let r = XmlReader::new("<e a=\"1\" a=\"2\"/>").unwrap();
	assert!(r.has_error().unwrap());
}

/// A raw '<' inside an attribute value is rejected by expat
/// (XML_ERROR_INVALID_TOKEN).
#[test]
fn reader_lt_in_attribute_value_is_error() {
	let r = XmlReader::new("<e a=\"x<y\"/>").unwrap();
	assert!(r.has_error().unwrap());
}

// ---- Reader: character data semantics ----

/// Character data split by an entity reference is merged into one
/// logical text run (C++ `on_characters` appends to the previous
/// Characters event).
#[test]
fn reader_text_merges_across_entity_references() {
	let doc = "<e>one &amp; two &lt; three</e>";
	let mut r = XmlReader::new(doc).unwrap();
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.read_element_text().unwrap(), "one & two < three");
}

/// CDATA merges with adjacent plain character data, mirroring the C++
/// merge of consecutive Characters events.
#[test]
fn reader_cdata_merges_with_adjacent_text() {
	let doc = "<e>plain<![CDATA[<raw&>]]>tail</e>";
	let mut r = XmlReader::new(doc).unwrap();
	assert!(!r.has_error().unwrap());
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.read_element_text().unwrap(), "plain<raw&>tail");
}

/// A comment inside character data splits nothing observable: the
/// surrounding text is still concatenated by read_element_text.
#[test]
fn reader_comment_inside_text() {
	let doc = "<e>ab<!-- hidden -->cd</e>";
	let mut r = XmlReader::new(doc).unwrap();
	assert!(!r.has_error().unwrap());
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.read_element_text().unwrap(), "abcd");
}

/// Whitespace in the prolog/epilog is allowed and produces no events;
/// non-whitespace outside the root is an expat error.
#[test]
fn reader_whitespace_outside_root_is_ignored() {
	let doc = "  \n\t<a>x</a>\n  ";
	let mut r = XmlReader::new(doc).unwrap();
	assert!(!r.has_error().unwrap());
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.name().unwrap(), "a");
	assert_eq!(r.read_element_text().unwrap(), "x");
}

#[test]
fn reader_text_before_root_is_error() {
	let r = XmlReader::new("junk<a/>").unwrap();
	assert!(r.has_error().unwrap());
}

#[test]
fn reader_text_after_root_is_error() {
	let r = XmlReader::new("<a/>junk").unwrap();
	assert!(r.has_error().unwrap());
}

/// expat enforces a single root element ("junk after document
/// element").
#[test]
fn reader_second_root_element_is_error() {
	let r = XmlReader::new("<a/><b/>").unwrap();
	assert!(r.has_error().unwrap());

	let r = XmlReader::new("<a></a><b></b>").unwrap();
	assert!(r.has_error().unwrap());
}

/// Comments and processing instructions after the root element are
/// legal in the epilog.
#[test]
fn reader_comment_and_pi_after_root_ok() {
	let doc = "<a/><!-- tail --><?pi data?>";
	let mut r = XmlReader::new(doc).unwrap();
	assert!(!r.has_error().unwrap());
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.name().unwrap(), "a");
	assert!(!r.read_next_start_element().unwrap());
}

/// An XML declaration and DOCTYPE are skipped like any PI/declaration.
#[test]
fn reader_xml_declaration_and_doctype_skipped() {
	let doc = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE root><root>v</root>";
	let mut r = XmlReader::new(doc).unwrap();
	assert!(!r.has_error().unwrap());
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.name().unwrap(), "root");
	assert_eq!(r.read_element_text().unwrap(), "v");
}

// ---- Reader: navigation semantics ----

/// read_element_text on anything but a start element returns an empty
/// string (CPP-PARITY: C++ returns std::string()).
#[test]
fn reader_read_element_text_off_start_element_is_empty() {
	// Before any read (Invalid token).
	let mut r = XmlReader::new("<a>x</a>").unwrap();
	assert_eq!(r.read_element_text().unwrap(), "");

	// On an end element.
	let mut r = XmlReader::new("<a><b/></a>").unwrap();
	assert!(r.read_next_start_element().unwrap()); // <a>
	assert!(r.read_next_start_element().unwrap()); // <b>
	assert!(!r.read_next_start_element().unwrap()); // stops on </a>
	assert_eq!(r.read_element_text().unwrap(), "");
}

/// skip_current_element off a start element is a no-op.
#[test]
fn reader_skip_off_start_element_is_noop() {
	let mut r = XmlReader::new("<a><b/></a>").unwrap();
	// Invalid token: no-op, next read still finds <a>.
	r.skip_current_element().unwrap();
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.name().unwrap(), "a");
}

/// skip_current_element with nested elements of the same name must
/// consume to the matching end tag, not the first one.
#[test]
fn reader_skip_nested_same_name() {
	let doc = "<root><a><a><a>deep</a></a></a><b>v</b></root>";
	let mut r = XmlReader::new(doc).unwrap();
	assert!(r.read_next_start_element().unwrap()); // root
	assert!(r.read_next_start_element().unwrap()); // outer a
	r.skip_current_element().unwrap();
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.name().unwrap(), "b");
}

/// A self-closing element can be consumed by read_element_text (expat
/// emits StartElement+EndElement, so the depth immediately hits 0).
#[test]
fn reader_read_element_text_on_self_closing() {
	let doc = "<a><b/></a>";
	let mut r = XmlReader::new(doc).unwrap();
	assert!(r.read_next_start_element().unwrap());
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.name().unwrap(), "b");
	assert_eq!(r.read_element_text().unwrap(), "");
	// The end element of <a> remains.
	assert!(!r.read_next_start_element().unwrap());
}

/// read_next_start_element stops (returns false) at the first end
/// element even if more start elements follow at a deeper level.
#[test]
fn reader_read_next_start_element_stops_at_end_element() {
	let doc = "<a><b><c/></b></a>";
	let mut r = XmlReader::new(doc).unwrap();
	assert!(r.read_next_start_element().unwrap()); // a
	assert!(r.read_next_start_element().unwrap()); // b
	assert!(r.read_next_start_element().unwrap()); // c
												// Next is </b>: returns false and stays put.
	assert!(!r.read_next_start_element().unwrap());
}

/// Deeply nested mixed content: read_element_text concatenates only
/// the direct-depth character runs, in order.
#[test]
fn reader_mixed_content_direct_text_in_order() {
	let doc = "<p>t1<b>x</b>t2<i>y</i>t3</p>";
	let mut r = XmlReader::new(doc).unwrap();
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.read_element_text().unwrap(), "t1t2t3");
}

/// After has_error, every navigation attempt fails and attribute
/// access stays empty.
#[test]
fn reader_error_state_is_sticky() {
	let mut r = XmlReader::new("<a></b>").unwrap();
	assert!(r.has_error().unwrap());
	assert!(!r.read_next_start_element().unwrap());
	assert!(!r.read_next_start_element().unwrap());
	assert_eq!(r.name().unwrap(), "");
	assert_eq!(r.attribute_count().unwrap(), 0);
}

// ---- Reader: malformed input matrix (all must set has_error) ----

#[test]
fn reader_malformed_matrix() {
	let cases = [
		"",                      // no element found
		"   \n  ",               // whitespace only: no element
		"<a>",                   // unclosed root
		"<a><b></a></b>",        // mismatched nesting
		"<a></a></a>",           // end tag without start
		"<a></>",                // empty end tag name
		"<a>< /a>",              // '<' not followed by name
		"<a",                    // unclosed start tag
		"<a b>",                 // attribute missing '='
		"<a b=1>",               // attribute value not quoted
		"<a b=\"v>",             // unclosed attribute value
		"<a></a",                // unclosed end tag
		"<a><!-- c</a>",         // unclosed comment
		"<a><![CDATA[x</a>",     // unclosed CDATA
		"<?pi<a></a>",           // unclosed PI
		"<a>&undefined;</a>",    // undefined entity
		"<a>&#xZZ;</a>",         // bad hex char reference
		"<a>&#99999999999;</a>", // out-of-range char reference
		"<a b=\"&foo;\"/>",      // undefined entity in attribute
		"<a/>tail",              // junk after document element
		"<a/><b/>",              // second root element
		"<a a=\"1\" a=\"2\"/>",  // duplicate attribute
		"<a b=\"<\"/>",          // '<' in attribute value
		"<a/ ><b/>",             // self-close then second root
	];
	for doc in cases {
		let r = XmlReader::new(doc).unwrap();
		assert!(r.has_error().unwrap(), "expected error for {:?}", doc);
	}
}

/// Well-formed matrix: none of these may set has_error.
#[test]
fn reader_wellformed_matrix() {
	let cases = [
		"<a/>",
		"<a></a>",
		"<a>text</a>",
		"<a b=\"v\" c='w'/>",
		"<a><b><c>deep</c></b></a>",
		"<?xml version=\"1.0\"?><a/>",
		"<!-- c --><a/><!-- c -->",
		"<a><![CDATA[x]]></a>",
		"<a>&amp;&#65;&#x41;</a>",
		"<a\n\tb = \"v\"\n/>", // whitespace around attribute '='
		"<a></a >",            // whitespace before '>' of end tag
		"<a>x</a  >",
		" \n<a> </a>\n ",
		"<ns:a xmlns:ns=\"urn:x\" ns:b=\"v\"/>", // namespaces not processed
		"<a>]]</a>",                             // single ']' is fine
	];
	for doc in cases {
		let r = XmlReader::new(doc).unwrap();
		assert!(!r.has_error().unwrap(), "unexpected error for {:?}", doc);
	}
}

// ---- Writer ----

/// write_end_document auto-closes every open element, innermost first.
#[test]
fn writer_end_document_auto_closes() {
	let mut w = XmlWriter::new();
	w.write_start_element("a").unwrap();
	w.write_start_element("b").unwrap();
	w.write_characters("t").unwrap();
	w.write_start_element("c").unwrap();
	w.write_end_document().unwrap();
	assert_eq!(w.output().unwrap(), "<a><b>t<c/></b></a>");
}

/// Writing characters closes a pending start tag; attributes written
/// afterwards go nowhere (CPP-PARITY: write_attribute is a no-op when
/// no start tag is open).
#[test]
fn writer_attribute_after_text_is_dropped() {
	let mut w = XmlWriter::new();
	w.write_start_element("a").unwrap();
	w.write_characters("x").unwrap();
	w.write_attribute("late", "v").unwrap();
	w.write_end_element().unwrap();
	assert_eq!(w.output().unwrap(), "<a>x</a>");
}

/// escape_text escapes exactly & < >; escape_attribute additionally
/// escapes the double quote. Other characters (', \n, \t) pass through.
#[test]
fn writer_escaping_matrix() {
	let mut w = XmlWriter::new();
	w.write_start_element("a").unwrap();
	w.write_attribute("k", "&<>\"'\n").unwrap();
	w.write_characters("&<>\"'\t").unwrap();
	w.write_end_element().unwrap();
	assert_eq!(
		w.output().unwrap(),
		"<a k=\"&amp;&lt;&gt;&quot;'\n\">&amp;&lt;&gt;\"'\t</a>"
	);
}

/// write_end_element past the balanced depth is a silent no-op and the
/// stack keeps working afterwards.
#[test]
fn writer_extra_end_element_noop_then_continue() {
	let mut w = XmlWriter::new();
	w.write_start_element("a").unwrap();
	w.write_end_element().unwrap();
	w.write_end_element().unwrap(); // no-op
	w.write_start_element("b").unwrap();
	w.write_end_document().unwrap();
	assert_eq!(w.output().unwrap(), "<a/><b/>");
}

/// write_text_element with empty text still closes the start tag
/// (write_characters flushes the pending '>'), so the element is not
/// self-closing. CPP-PARITY: C++ write_characters("") does the same.
#[test]
fn writer_text_element_empty_text() {
	let mut w = XmlWriter::new();
	w.write_text_element("a", "").unwrap();
	assert_eq!(w.output().unwrap(), "<a></a>");
}

/// write_text_element nests correctly inside an open start tag.
#[test]
fn writer_text_element_inside_open_tag() {
	let mut w = XmlWriter::new();
	w.write_start_element("root").unwrap();
	w.write_attribute("v", "1").unwrap();
	w.write_text_element("child", "x").unwrap();
	w.write_end_document().unwrap();
	assert_eq!(w.output().unwrap(), "<root v=\"1\"><child>x</child></root>");
}

/// Characters written before any start element land verbatim (escaped)
/// in the output; C++ does the same.
#[test]
fn writer_characters_outside_any_element() {
	let mut w = XmlWriter::new();
	w.write_characters("a & b").unwrap();
	assert_eq!(w.output().unwrap(), "a &amp; b");
}

/// Round-trip with special characters: writer escapes, reader
/// resolves, values come back identical.
#[test]
fn round_trip_special_characters() {
	let tricky = "a & b < c > d \"e\" 'f'\n\tg";
	let mut w = XmlWriter::new();
	w.write_start_element("root").unwrap();
	w.write_attribute("attr", tricky).unwrap();
	w.write_text_element("child", tricky).unwrap();
	w.write_end_document().unwrap();
	let xml = w.output().unwrap();

	let mut r = XmlReader::new(&xml).unwrap();
	assert!(!r.has_error().unwrap());
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.name().unwrap(), "root");
	assert_eq!(r.attribute_count().unwrap(), 1);
	assert_eq!(r.attribute_name(0).unwrap(), "attr");
	// CPP-PARITY: expat applies XML 1.0 attribute-value normalization
	// (§3.3.3): a literal tab/newline/CR inside an attribute value
	// becomes a space. quick-xml does the same, so the attribute value
	// reads back with two spaces where the input had "\n\t", while the
	// character data asserted below keeps the "\n\t" intact.
	let normalized_attr = "a & b < c > d \"e\" 'f'  g";
	assert_eq!(r.attribute_value(0).unwrap(), normalized_attr);
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.read_element_text().unwrap(), tricky);
}

/// Non-ASCII (UTF-8) content round-trips unchanged.
#[test]
fn round_trip_utf8() {
	let text = "héllo wörld — 中文";
	let mut w = XmlWriter::new();
	w.write_text_element("t", text).unwrap();
	let xml = w.output().unwrap();

	let mut r = XmlReader::new(&xml).unwrap();
	assert!(!r.has_error().unwrap());
	assert!(r.read_next_start_element().unwrap());
	assert_eq!(r.read_element_text().unwrap(), text);
}
