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

//! Command-line option parser, mirroring
//! `src/common/src/commandlineparser.h` and
//! `include/common/commandlineparser.h`.
//!
//! A parser owns a list of registered options and positional arguments;
//! option/argument handles returned at registration time are owned by the
//! parser and stay valid until it is destroyed.

use std::ffi::CString;
use std::io::{self, Write};

use crate::error::{Error, Result};

/// `olive::CommandLineParser` — owns registered options and positional
/// arguments.
pub struct CommandLineParser {
	/// Application name shown by `print_help`.
	app_name: String,
	/// Application version shown by `print_help`.
	app_version: String,
	/// Registered named options.
	///
	/// Boxed so that the option objects keep a stable address when the
	/// vector grows; option handles borrowed by the C ABI point at the
	/// boxed value. // CPP-PARITY: the C++ code heap-allocates each
	/// `Option` (`new Option()`); `Vec<Box<..>>` reproduces that stability
	/// against reallocation.
	options: Vec<Box<CommandLineOption>>,
	/// Registered positional arguments (see `options` for the boxing note).
	positionals: Vec<Box<CommandLinePositionalArgument>>,
}

impl CommandLineParser {
	/// New empty parser.
	pub fn new() -> Self {
		// CPP-PARITY: `app_name_` defaults to "quercus" in the C++ header
		// (`std::string app_name_ = "quercus"`), `app_version_` to "".
		Self {
			app_name: "quercus".to_string(),
			app_version: String::new(),
			options: Vec::new(),
			positionals: Vec::new(),
		}
	}

	/// Set the application name and version printed at the top of the help
	/// text.
	///
	/// `name` and `version` replace the previous values (`new()` starts with
	/// `"quercus"` and an empty version). `print_help` writes them as
	/// `"<name> <version>"` on the first line, so an empty version still leaves
	/// the separating space; they are not used in the `Usage:` line, which comes
	/// from the argv[0] basename instead.
	pub fn set_app_info(&mut self, name: &str, version: &str) {
		self.app_name = name.to_string();
		self.app_version = version.to_string();
	}

	/// Register a named option with one or more aliases.
	///
	/// The option is appended to the parser's list and stays owned by the parser;
	/// callers retrieve it by index (the C ABI layer uses `option_count` right
	/// after registration). Duplicate names are not rejected, and `process`
	/// matches the first registered option whose alias matches, so registration
	/// order breaks ties.
	///
	/// # Arguments
	///
	/// * `names` - Alias strings as C strings, without the leading `-` (e.g.
	///   `["h", "help"]`); matched case-insensitively. Must be valid UTF-8.
	/// * `description` - Help text, printed under the option by `print_help`.
	/// * `takes_arg` - Whether `process` consumes the following argv entry as the
	///   option's value.
	/// * `arg_placeholder` - Name shown as `<PLACEHOLDER>` in help; an empty
	///   string prints no placeholder (used for flags).
	/// * `hidden` - Whether to omit the option from `print_help` output; it is
	///   still parsed.
	///
	/// # Errors
	///
	/// Returns `Error::Invalid` if any entry of `names` is not valid UTF-8 (the C
	/// ABI rejects null/empty names before this point).
	pub fn add_option(
		&mut self,
		names: &[CString],
		description: &str,
		takes_arg: bool,
		arg_placeholder: &str,
		hidden: bool,
	) -> Result<()> {
		let mut strings = Vec::with_capacity(names.len());
		for n in names {
			// CPP-PARITY: C++ stores raw byte strings and accepts any bytes;
			// Rust `String` is UTF-8, so a non-UTF-8 option name is rejected
			// here with E_INVALID. The C ABI `add_option` already rejects
			// null/empty names before reaching this point.
			strings.push(n.to_str().map_err(|_| Error::Invalid)?.to_string());
		}

		self.options.push(Box::new(CommandLineOption {
			names: strings,
			description: description.to_string(),
			takes_arg,
			arg_placeholder: arg_placeholder.to_string(),
			hidden,
			is_set: false,
			setting: None,
		}));
		Ok(())
	}

	/// Register a positional argument, in the order they appear on the command
	/// line.
	///
	/// `name` is printed in the usage line inside square brackets (`[name]`);
	/// `description` is stored for help output; `required` is recorded but not
	/// enforced — `process` never checks it, so a missing required argument is
	/// silently left unset. Registration order defines which argv entry fills
	/// which argument.
	///
	/// # Returns
	///
	/// Always `Ok(())`; the `Result` only mirrors the C API's try/catch boundary.
	/// There is no handle: the argument is owned by the parser and can be read
	/// later through the `pub(crate)` index accessors.
	pub fn add_positional_argument(
		&mut self,
		name: &str,
		description: &str,
		required: bool,
	) -> Result<()> {
		self.positionals
			.push(Box::new(CommandLinePositionalArgument {
				name: name.to_string(),
				description: description.to_string(),
				required,
				setting: None,
			}));
		Ok(())
	}

	/// Parse an argv-style argument list and record which options and positional
	/// arguments were supplied.
	///
	/// `argv[0]` is the program name and is skipped. Each later entry is
	/// classified as follows:
	///
	/// * an entry starting with `-` is an option: the first `-` is stripped and
	///   the remainder is matched case-insensitively against every registered
	///   names alias. Only one dash is stripped, so a GNU-style `--name` is looked
	///   up as `-name` and matches only an option literally registered under that
	///   spelling. A match sets `is_set`; if the option takes an argument and
	///   another entry follows, that entry is stored as the option's setting and
	///   also consumed. Unknown options are reported to stderr and ignored (the
	///   parse still succeeds).
	/// * anything else fills the next free positional argument in registration
	///   order; entries beyond the registered count are reported to stderr and
	///   ignored.
	///
	/// Non-UTF-8 entries cannot match anything and are also reported as unknown
	/// parameters. `required` positional arguments are *not* enforced, so a
	/// missing required argument does not fail.
	///
	/// # Returns
	///
	/// Always `Ok(())`; the `Result` only mirrors the C API's try/catch boundary.
	/// Calls are cumulative: re-parsing does not clear previous `is_set`/
	/// `setting` values.
	pub fn process(&mut self, argv: &[CString]) -> Result<()> {
		let mut positional_index = 0usize;
		let mut i = 1usize; // CPP-PARITY: argv[0] is the program name, skipped.

		while i < argv.len() {
			let arg_str = match argv[i].to_str() {
				Ok(s) => s.to_string(),
				Err(_) => {
					// CPP-PARITY: C++ compares raw bytes; a non-UTF-8 arg
					// cannot match a known option/positional here, so treat
					// it as an unknown parameter (best effort for the log).
					eprintln!("Unknown parameter: {}", argv[i].to_string_lossy());
					i += 1;
					continue;
				}
			};

			if !arg_str.is_empty() && arg_str.starts_with('-') {
				// Must be an option. Skip past the first dash.
				let arg_basename = &arg_str[1..];

				let mut matched_known = false;
				let mut consume_next = false;

				// CPP-PARITY: the C++ `goto found_flag` is a labelled break
				// out of the double loop; the match is case-insensitive.
				'find: for opt in self.options.iter_mut() {
					for name in &opt.names {
						if name.eq_ignore_ascii_case(arg_basename) {
							opt.is_set = true;
							if opt.takes_arg && i + 1 < argv.len() {
								// CPP-PARITY: the argument value is stored as
								// a byte string in C++; Rust only keeps it when
								// it is valid UTF-8, otherwise it is left unset.
								opt.setting = argv[i + 1].to_str().ok().map(str::to_owned);
								consume_next = true;
							}
							matched_known = true;
							break 'find;
						}
					}
				}

				if !matched_known {
					eprintln!("Unknown parameter: {}", arg_str);
				}
				// CPP-PARITY: when a `takes_arg` option consumes the following
				// argument, C++ advances `i` twice (the inner `i++` plus the
				// loop increment); otherwise once.
				i += if consume_next { 2 } else { 1 };
			} else {
				// Must be a positional argument.
				if positional_index < self.positionals.len() {
					self.positionals[positional_index].setting = Some(arg_str);
					positional_index += 1;
				} else {
					eprintln!("Unknown parameter: {}", arg_str);
				}
				i += 1;
			}
		}

		Ok(())
	}

	/// Write the usage/help text for this parser to stdout.
	///
	/// The output is the application name and version line, the copyright line, a
	/// `Usage: <basename> [options] [arg] ...` line built from the registered
	/// positional arguments, then one block per non-hidden option (`-name, -alias
	/// <PLACEHOLDER>`) followed by its description, and a trailing blank line.
	///
	/// `filename` is the program path from argv[0]; only the part after the last
	/// `/` is printed, so the usage line shows the program name rather than the
	/// full path. It is not otherwise interpreted.
	///
	/// # Errors
	///
	/// Returns `Error::Failed` if writing to stdout fails (for example a closed
	/// pipe). Help is rendered through a locked stdout handle so the blocks are
	/// not interleaved with other writers.
	pub fn print_help(&self, filename: &str) -> Result<()> {
		let stdout = io::stdout();
		let mut out = stdout.lock();
		self.write_help(&mut out, filename)
	}

	/// Render the usage/help text into `out`, mirroring
	/// `CommandLineParser::print_help`. Split out so tests can capture the
	/// output in a buffer.
	fn write_help<W: Write>(&self, out: &mut W, filename: &str) -> Result<()> {
		let r = (|| -> io::Result<()> {
			// CPP-PARITY: `printf("%s %s\n")` always emits the separating
			// space, so "quercus " + version + "\n" when the version is empty.
			writeln!(out, "{} {}", self.app_name, self.app_version)?;
			writeln!(out, "Copyright (C) 2018-2022 Quercus Video Editor Team")?;

			// Build the "[name] [name] ..." positional list.
			let mut positional_args = String::new();
			for (i, p) in self.positionals.iter().enumerate() {
				if i > 0 {
					positional_args.push(' ');
				}
				positional_args.push('[');
				positional_args.push_str(&p.name);
				positional_args.push(']');
			}

			// CPP-PARITY: on POSIX the basename is everything after the last
			// '/'; without a slash the whole string is used.
			let basename = match filename.rfind('/') {
				Some(pos) => &filename[pos + 1..],
				None => filename,
			};

			writeln!(out, "Usage: {} [options] {}\n", basename, positional_args)?;

			for opt in &self.options {
				if opt.hidden {
					continue;
				}

				let mut all_args = String::new();
				for (i, name) in opt.names.iter().enumerate() {
					if i > 0 {
						all_args.push_str(", ");
					}
					all_args.push('-');
					all_args.push_str(name);
				}

				if opt.arg_placeholder.is_empty() {
					writeln!(out, "    {}", all_args)?;
				} else {
					writeln!(out, "    {} <{}>", all_args, opt.arg_placeholder)?;
				}
				writeln!(out, "        {}\n", opt.description)?;
			}

			writeln!(out)?;
			Ok(())
		})();
		r.map_err(|e| Error::Failed(e.to_string()))
	}

	// The six accessors below are `pub(crate)` so the C ABI layer
	// (`crate::ffi::commandlineparser`) can hand out stable borrowed handles
	// to the boxed options/arguments; they are currently unused until that
	// layer is implemented.
	#[allow(dead_code)]
	/// Number of registered options. `pub(crate)`: used by the C ABI layer
	/// to look up the option just appended by [`Self::add_option`].
	pub(crate) fn option_count(&self) -> usize {
		self.options.len()
	}

	/// Borrow a registered option by index. `pub(crate)`: hands the C ABI
	/// layer a stable pointer to the boxed option for a borrowed handle.
	#[allow(dead_code)]
	pub(crate) fn option(&self, index: usize) -> Option<&CommandLineOption> {
		self.options.get(index).map(|b| b.as_ref())
	}

	/// Mutably borrow a registered option by index.
	#[allow(dead_code)]
	pub(crate) fn option_mut(&mut self, index: usize) -> Option<&mut CommandLineOption> {
		self.options.get_mut(index).map(|b| b.as_mut())
	}

	/// Number of registered positional arguments.
	#[allow(dead_code)]
	pub(crate) fn positional_count(&self) -> usize {
		self.positionals.len()
	}

	/// Borrow a registered positional argument by index.
	#[allow(dead_code)]
	pub(crate) fn positional(&self, index: usize) -> Option<&CommandLinePositionalArgument> {
		self.positionals.get(index).map(|b| b.as_ref())
	}

	/// Mutably borrow a registered positional argument by index.
	#[allow(dead_code)]
	pub(crate) fn positional_mut(
		&mut self,
		index: usize,
	) -> Option<&mut CommandLinePositionalArgument> {
		self.positionals.get_mut(index).map(|b| b.as_mut())
	}
}

impl Default for CommandLineParser {
	fn default() -> Self {
		Self::new()
	}
}

/// `olive::CommandLineOption` — a registered named option.
pub struct CommandLineOption {
	/// Option name strings (without leading dash).
	names: Vec<String>,
	/// Help text.
	description: String,
	/// Whether the option consumes the following argument.
	takes_arg: bool,
	/// Placeholder shown in help when `takes_arg`.
	arg_placeholder: String,
	/// Whether to omit from help output.
	hidden: bool,
	/// Whether the option was present on the command line.
	is_set: bool,
	/// The argument value (present only when set).
	setting: Option<String>,
}

impl CommandLineOption {
	/// Whether the option was present on the command line.
	pub fn is_set(&self) -> bool {
		self.is_set
	}

	/// The option's argument value, if one was set.
	pub fn get_setting(&self) -> Result<&str> {
		match &self.setting {
			Some(v) => Ok(v.as_str()),
			// CPP-PARITY: C++ `get_setting()` returns the (possibly empty)
			// stored string and never fails; the "was it supplied" question
			// is answered by [`Self::is_set`]. Rust mirrors this by returning
			// an empty string when nothing was stored.
			None => Ok(""),
		}
	}

	/// Set the option's argument value from outside parsing.
	///
	/// `value` is stored as the option's setting, replacing any value `process`
	/// had captured, and `get_setting` returns it afterwards. Note that this does
	/// **not** mark the option as present: `is_set` is only set by `process`
	/// matching the option on the command line.
	///
	/// # Returns
	///
	/// Always `Ok(())`; the `Result` only mirrors the C API's try/catch boundary.
	pub fn set_setting(&mut self, value: &str) -> Result<()> {
		self.setting = Some(value.to_string());
		Ok(())
	}
}

/// `olive::CommandLinePositionalArgument` — a registered positional
/// argument.
pub struct CommandLinePositionalArgument {
	/// Argument name.
	name: String,
	/// Help text.
	#[allow(dead_code)] // CPP-PARITY: stored but never read in C++ print_help/process.
	description: String,
	/// Whether the argument is required.
	#[allow(dead_code)] // CPP-PARITY: stored but never consumed in the C++ source.
	required: bool,
	/// The argument value (present only when parsed).
	setting: Option<String>,
}

impl CommandLinePositionalArgument {
	/// The argument's value, if one was parsed.
	pub fn get_setting(&self) -> Result<&str> {
		match &self.setting {
			Some(v) => Ok(v.as_str()),
			None => Ok(""),
		}
	}

	/// Set the positional argument's value from outside parsing.
	///
	/// `value` replaces whatever `process` had stored (including `None`), which
	/// lets the C ABI layer supply a value for arguments it handles itself.
	/// `get_setting` then returns it. This does not mark anything as required or
	/// validated.
	///
	/// # Returns
	///
	/// Always `Ok(())`; the `Result` only mirrors the C API's try/catch boundary.
	pub fn set_setting(&mut self, value: &str) -> Result<()> {
		self.setting = Some(value.to_string());
		Ok(())
	}
}

#[cfg(test)]
mod tests;
