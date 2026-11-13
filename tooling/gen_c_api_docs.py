#!/usr/bin/env python3
"""Generate the Quercus engine C API reference from the checked-in C header.

The C header (`crates/quercus-engine/include/quercus_engine.h`) is itself
generated from the Rust SDK's `extern "C"` surface by
`tooling/gen_c_header.py`. This tool consumes that header as the single source
of truth for the C ABI and emits a human-readable Markdown reference:

    docs/c-api/reference.md

Doc prose is harvested from the Rust SDK (`crates/quercus-engine-rs/src`):
for every `quercus_*` symbol we look for a wrapper `fn` whose body calls that
symbol and reuse the contiguous `///` block above that wrapper. `///` blocks
on `#[repr(C)]`/plain `struct` and `enum` mirrors are used for the type
sections. When no Rust doc exists we fall back to a neutral sentence that
marks the entry undocumented rather than inventing semantics.

Usage:
    python3 tooling/gen_c_api_docs.py            # print to stdout
    python3 tooling/gen_c_api_docs.py --write    # write docs/c-api/reference.md
    python3 tooling/gen_c_api_docs.py --check    # fail when the file is stale

The reference is checked in; CI runs `--check` to catch drift.
"""

from __future__ import annotations

import argparse
import hashlib
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
HEADER = ROOT / "crates" / "quercus-engine" / "include" / "quercus_engine.h"
SDK = ROOT / "crates" / "quercus-engine-rs" / "src"
OUTPUT = ROOT / "docs" / "c-api" / "reference.md"

# Counts asserted by the header's own provenance line and by the task that
# introduced this reference. Any drift means either the header or this parser
# is wrong; fail loudly instead of publishing an incomplete reference.
EXPECTED_FUNCTIONS = 743
EXPECTED_NAMED_STRUCTS = 60
EXPECTED_ENUMS = 14
EXPECTED_CALLBACKS = 10
EXPECTED_ALIASES = 3

# Canonical module order. `quercus_<module>_...` is the grouping key; any
# additional module present in the header is still documented, appended after
# the canonical list in alphabetical order (today: `folder`, `video`).
MODULE_ORDER = [
    "core",
    "editor",
    "project",
    "footage",
    "sequence",
    "track",
    "tracklist",
    "clip",
    "block",
    "effect",
    "timeline",
    "undo",
    "node",
    "codec",
    "audio",
    "otio",
    "ofx",
    "plugin",
    "render",
    "storage",
    "task",
    "thumbnail",
    "waveform",
    "worker",
    "generator",
]

INTEGER_TYPES = (
    "uint8_t",
    "uint16_t",
    "uint32_t",
    "uint64_t",
    "int8_t",
    "int16_t",
    "int32_t",
    "int64_t",
    "size_t",
)

ALIAS_NOTES = {
    "QuercusRational": "Alias for `Rational`; the engine-side spelling used in prose.",
    "QuercusTimeRange": "Alias for `TimeRange`; the engine-side spelling used in prose.",
    "QuercusNodeValue": "Alias for `NodeValue`; the engine-side spelling used in prose.",
}


# --------------------------------------------------------------------------
# Header parsing
# --------------------------------------------------------------------------


def strip_comments(text: str) -> str:
    """Remove C comments while preserving newlines (keeps line-based parsing sane)."""

    def blank(match: re.Match[str]) -> str:
        return re.sub(r"[^\n]", " ", match.group(0))

    return re.sub(r"/\*.*?\*/", blank, text, flags=re.S)


def normalize_ws(text: str) -> str:
    return re.sub(r"\s+", " ", text).strip()


def split_declarations(text: str) -> list[str]:
    """Split C source into top-level declarations at `;` outside braces/parens."""
    declarations: list[str] = []
    buffer: list[str] = []
    paren = 0
    brace = 0
    for ch in text:
        if ch == ";" and paren == 0 and brace == 0:
            declaration = "".join(buffer).strip()
            if declaration:
                declarations.append(declaration)
            buffer = []
            continue
        buffer.append(ch)
        if ch == "(":
            paren += 1
        elif ch == ")":
            paren = max(0, paren - 1)
        elif ch == "{":
            brace += 1
        elif ch == "}":
            brace = max(0, brace - 1)
    tail = "".join(buffer).strip()
    if tail:
        declarations.append(tail)
    return declarations


def parse_typedef_enums(text: str) -> list[dict]:
    """Parse `typedef enum Name { ... } Name;` definitions."""
    enums = []
    pattern = re.compile(
        r"typedef\s+enum\s+(\w+)\s*\{(.*?)\}\s*(\w+)\s*;", re.S
    )
    for match in pattern.finditer(text):
        name, body, alias = match.groups()
        values = []
        for item in body.split(","):
            item = normalize_ws(item)
            if item:
                values.append(item)
        enums.append(
            {
                "kind": "enum",
                "name": name,
                "alias": alias,
                "storage": None,
                "values": values,
                "raw": normalize_ws(match.group(0)),
            }
        )
    return enums


def parse_define_enums(text: str) -> list[dict]:
    """Parse integer typedefs whose values live in adjacent `#define`s.

    The header uses this form for `QuercusEditOp` (opcode constants), so the
    enumerators are preprocessor macros rather than C enumerators.
    """
    lines = text.splitlines()
    enums = []
    index = 0
    while index < len(lines):
        match = re.match(
            r"\s*typedef\s+(" + "|".join(INTEGER_TYPES) + r")\s+(\w+)\s*;\s*$",
            lines[index],
        )
        if not match:
            index += 1
            continue
        storage, name = match.groups()
        defines = []
        cursor = index + 1
        while cursor < len(lines):
            define_match = re.match(
                r"\s*#define\s+(QUERCUS_\w+)\s+(-?\d+)(?:[uUlL]*)\s*$", lines[cursor]
            )
            if not define_match:
                break
            defines.append(normalize_ws(lines[cursor]))
            cursor += 1
        if defines:
            enums.append(
                {
                    "kind": "define",
                    "name": name,
                    "alias": name,
                    "storage": storage,
                    "values": defines,
                    "raw": "",
                }
            )
            index = cursor
        else:
            index += 1
    return enums


def parse_structs(declarations: list[str]) -> tuple[list[dict], list[dict], list[str]]:
    """Return (named struct definitions, inline struct typedefs, forward names)."""
    named: list[dict] = []
    inline: list[dict] = []
    forwards: list[str] = []
    for declaration in declarations:
        inline_match = re.match(
            r"typedef\s+struct\s+(\w+)\s*\{(.*)\}\s*(\w+)\s*$", declaration, re.S
        )
        if inline_match:
            name, body, alias = inline_match.groups()
            inline.append(
                {
                    "name": name,
                    "alias": alias,
                    "fields": parse_fields(body),
                    "raw": normalize_ws(declaration) + ";",
                }
            )
            continue
        forward_match = re.match(r"typedef\s+struct\s+(\w+)\s+(\w+)\s*$", declaration)
        if forward_match:
            forwards.append(forward_match.group(1))
            continue
        named_match = re.match(r"^struct\s+(\w+)\s*\{(.*)\}$", declaration, re.S)
        if named_match:
            name, body = named_match.groups()
            named.append({"name": name, "fields": parse_fields(body)})
    return named, inline, forwards


def parse_fields(body: str) -> list[str]:
    fields = []
    for part in body.split(";"):
        field = normalize_ws(part)
        if field:
            fields.append(field)
    return fields


def parse_callbacks(declarations: list[str]) -> list[dict]:
    callbacks = []
    for declaration in declarations:
        match = re.match(
            r"typedef\s+(.+?)\s*\(\s*\*\s*(\w+)\s*\)\s*\((.*)\)\s*$",
            declaration,
            re.S,
        )
        if not match:
            continue
        returns, name, params = match.groups()
        callbacks.append(
            {
                "name": name,
                "returns": normalize_ws(returns),
                "params": normalize_ws(params),
                "raw": "typedef {} (*{})({});".format(
                    normalize_ws(returns), name, normalize_ws(params)
                ),
            }
        )
    return callbacks


def parse_aliases(declarations: list[str], skip: set[str]) -> list[dict]:
    aliases = []
    for declaration in declarations:
        match = re.match(r"typedef\s+(\w+)\s+(\w+)\s*$", declaration)
        if not match:
            continue
        target, name = match.groups()
        if name in skip:
            continue
        aliases.append({"name": name, "target": target})
    return aliases


def parse_functions(declarations: list[str]) -> list[dict]:
    functions = []
    for declaration in declarations:
        if "quercus_" not in declaration or "(" not in declaration:
            continue
        if declaration.startswith(
            ("typedef", "struct ", "enum ", "static_assert", "_Static_assert")
        ):
            continue
        match = re.search(r"\b(quercus_[a-z0-9_]+)\s*\(", declaration)
        if not match:
            continue
        prototype = normalize_ws(declaration) + ";"
        symbol = match.group(1)
        tokens = symbol.split("_")
        if len(tokens) < 3:
            raise SystemExit(
                "error: cannot derive a module from function symbol {!r}".format(symbol)
            )
        functions.append(
            {
                "symbol": symbol,
                "module": tokens[1],
                "prototype": prototype,
                "returns": normalize_ws(declaration[: match.start()]),
            }
        )
    return functions


def parse_header_comments(text: str) -> dict[str, str]:
    """Map a declaration name to a leading `/* ... */` comment, when present."""
    comments: dict[str, str] = {}
    for match in re.finditer(r"/\*(.*?)\*/", text, re.S):
        comment = normalize_ws(match.group(1))
        rest = text[match.end() :].lstrip()
        following = rest.split("\n", 1)[0]
        symbol = re.search(r"\b(quercus_[a-z0-9_]+)\s*\(", following)
        if symbol:
            comments.setdefault(symbol.group(1), comment)
            continue
        named = re.match(r"typedef\s+(?:struct|enum)\s+(\w+)", following)
        if named:
            comments.setdefault(named.group(1), comment)
            continue
        named = re.match(r"struct\s+(\w+)", following)
        if named:
            comments.setdefault(named.group(1), comment)
    return comments


# --------------------------------------------------------------------------
# Rust SDK doc harvesting
# --------------------------------------------------------------------------


def preceding_doc_lines(lines: list[str], index: int) -> list[str]:
    """Return contiguous `///` lines immediately above `index`, skipping attrs."""
    docs: list[str] = []
    cursor = index - 1
    while cursor >= 0:
        stripped = lines[cursor].strip()
        if stripped.startswith("///"):
            docs.append(stripped[3:].lstrip())
            cursor -= 1
            continue
        if stripped.startswith("#["):
            cursor -= 1
            continue
        break
    docs.reverse()
    return docs


def skip_string(text: str, index: int) -> int:
    """Return the index just past a Rust string/char literal starting at `index`."""
    quote = text[index]
    cursor = index + 1
    while cursor < len(text):
        if text[cursor] == "\\":
            cursor += 2
            continue
        if text[cursor] == quote:
            return cursor + 1
        if text[cursor] == "\n" and quote != "\n":
            # Unterminated (e.g. a lifetime); stop at the newline.
            return cursor
        cursor += 1
    return cursor


def find_body(text: str, start: int) -> tuple[int, int] | None:
    """Locate a `{ ... }` body after `start`; return (open, close) or None."""
    cursor = start
    while cursor < len(text):
        ch = text[cursor]
        if ch == ";":
            # An `extern` declaration, not a wrapper function.
            return None
        if text.startswith("//", cursor):
            newline = text.find("\n", cursor)
            if newline == -1:
                return None
            cursor = newline + 1
            continue
        if ch == '"':
            cursor = skip_string(text, cursor)
            continue
        if ch == "'":
            # Lifetimes (`'a`) and char literals both use apostrophes; a lone
            # identifier after it is a lifetime and must not be skipped.
            if cursor + 1 < len(text) and (text[cursor + 1].isalpha() or text[cursor + 1] == "_"):
                closing = text.find("'", cursor + 1)
                if closing != -1 and closing - cursor <= 3:
                    cursor = closing + 1
                    continue
                cursor += 1
                continue
            cursor = skip_string(text, cursor)
            continue
        if ch == "{":
            break
        cursor += 1
    else:
        return None
    open_index = cursor
    depth = 0
    cursor = open_index
    while cursor < len(text):
        if text.startswith("//", cursor):
            newline = text.find("\n", cursor)
            if newline == -1:
                return open_index, len(text) - 1
            cursor = newline + 1
            continue
        ch = text[cursor]
        if ch == '"':
            cursor = skip_string(text, cursor)
            continue
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return open_index, cursor
        cursor += 1
    return open_index, len(text) - 1


def harvest_sdk_docs() -> tuple[dict[str, str], dict[str, str], dict[str, str]]:
    """Return (function summaries, type summaries, callback summaries)."""
    function_candidates: dict[str, list[tuple]] = {}
    type_docs: dict[str, str] = {}
    callback_docs: dict[str, str] = {}

    files = sorted(SDK.rglob("*.rs"), key=lambda path: str(path))
    for file_index, path in enumerate(files):
        lines = path.read_text(encoding="utf-8").splitlines()
        text = "\n".join(lines)
        for index, line in enumerate(lines):
            fn_match = re.match(
                r"\s*(?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?(?:unsafe\s+)?(?:async\s+)?fn\s+(\w+)",
                line,
            )
            if fn_match:
                docs = preceding_doc_lines(lines, index)
                if docs:
                    located = find_body(text, sum(len(item) + 1 for item in lines[:index]))
                    if located is not None:
                        open_index, close_index = located
                        body = text[open_index : close_index + 1]
                        symbols = set(re.findall(r"\bquercus_[a-z0-9_]+\b", body))
                        if symbols:
                            summary = summarize_doc(docs)
                            for symbol in sorted(symbols):
                                function_candidates.setdefault(symbol, []).append(
                                    (
                                        file_index,
                                        index,
                                        fn_match.group(1),
                                        summary,
                                        len(symbols),
                                    )
                                )
                continue

            type_match = re.match(
                r"\s*(?:pub(?:\([^)]*\))?\s+)?(struct|enum)\s+(\w+)", line
            )
            if type_match:
                docs = preceding_doc_lines(lines, index)
                if docs:
                    type_docs.setdefault(type_match.group(2), summarize_doc(docs))
                continue

            alias_match = re.match(
                r"\s*(?:pub(?:\([^)]*\))?\s+)?type\s+(\w+)", line
            )
            if alias_match:
                docs = preceding_doc_lines(lines, index)
                if docs:
                    callback_docs.setdefault(alias_match.group(1), summarize_doc(docs))

    function_docs: dict[str, str] = {}
    for symbol, candidates in function_candidates.items():
        # Prefer a wrapper whose body calls exactly this symbol, then source
        # order. This keeps summaries specific when helpers call several ABI
        # entry points.
        symbol_tokens = set(symbol.split("_")[1:])

        def score(item: tuple) -> tuple:
            wrapper_tokens = set(item[2].split("_"))
            overlap = len(wrapper_tokens & symbol_tokens)
            return (item[4] != 1, -overlap, item[0], item[1])

        best = min(candidates, key=score)
        function_docs[symbol] = best[3]
    return function_docs, type_docs, callback_docs


def summarize_doc(lines: list[str]) -> str:
    """Collapse a `///` block's first paragraph to one readable line."""
    paragraph: list[str] = []
    for line in lines:
        stripped = line.strip()
        if not stripped:
            if paragraph:
                break
            continue
        paragraph.append(stripped)
        # A one-sentence first line is enough; stop when the paragraph ends.
        if stripped.endswith((".", "!", "?")) and len(" ".join(paragraph)) > 40:
            break
    summary = normalize_ws(" ".join(paragraph))
    # Turn rustdoc intra-doc links (`[`foo`]`) into plain code spans; the
    # generated Markdown has no rustdoc link targets.
    summary = re.sub(r"\[`([^`]+)`\]", r"`\1`", summary)
    return summary


# --------------------------------------------------------------------------
# Generation
# --------------------------------------------------------------------------


def build_header_facts() -> dict:
    header_text = HEADER.read_text(encoding="utf-8")
    header_hash = hashlib.sha256(header_text.encode("utf-8")).hexdigest()

    cleaned_lines = [
        line
        for line in strip_comments(header_text).splitlines()
        if not line.lstrip().startswith("#")
        and not re.match(r'\s*extern\s+"C"\s*\{\s*$', line)
    ]
    cleaned = "\n".join(cleaned_lines)

    typedef_enums = parse_typedef_enums(cleaned)
    define_enums = parse_define_enums(header_text)
    enums = typedef_enums + define_enums

    declarations = split_declarations(cleaned)
    named_structs, inline_structs, forwards = parse_structs(declarations)
    callbacks = parse_callbacks(declarations)
    define_enum_names = {enum["name"] for enum in define_enums}
    aliases = parse_aliases(declarations, skip=define_enum_names)
    functions = parse_functions(declarations)

    validate(header_text, functions, named_structs, inline_structs, forwards, enums, callbacks, aliases)

    function_docs, type_docs, callback_docs = harvest_sdk_docs()
    header_comments = parse_header_comments(header_text)

    modules: dict[str, list[dict]] = {}
    for function in functions:
        modules.setdefault(function["module"], []).append(function)
    for items in modules.values():
        items.sort(key=lambda item: item["symbol"])

    ordered_modules = [module for module in MODULE_ORDER if module in modules]
    ordered_modules += sorted(module for module in modules if module not in MODULE_ORDER)

    documented_functions = sum(1 for fn in functions if fn["symbol"] in function_docs)
    return {
        "header_text": header_text,
        "header_hash": header_hash,
        "header_lines": len(header_text.splitlines()),
        "functions": functions,
        "modules": modules,
        "ordered_modules": ordered_modules,
        "named_structs": named_structs,
        "inline_structs": inline_structs,
        "enums": enums,
        "callbacks": callbacks,
        "aliases": aliases,
        "function_docs": function_docs,
        "type_docs": type_docs,
        "callback_docs": callback_docs,
        "header_comments": header_comments,
        "documented_functions": documented_functions,
    }


def validate(header_text, functions, named_structs, inline_structs, forwards, enums, callbacks, aliases) -> None:
    errors: list[str] = []
    if len(functions) != EXPECTED_FUNCTIONS:
        errors.append(
            "functions: expected {}, parsed {}".format(EXPECTED_FUNCTIONS, len(functions))
        )
    symbols = [fn["symbol"] for fn in functions]
    duplicates = sorted({symbol for symbol in symbols if symbols.count(symbol) > 1})
    if duplicates:
        errors.append("duplicate function symbols: {}".format(", ".join(duplicates)))
    if len(named_structs) != EXPECTED_NAMED_STRUCTS:
        errors.append(
            "named POD structs: expected {}, parsed {}".format(
                EXPECTED_NAMED_STRUCTS, len(named_structs)
            )
        )
    named = {struct["name"] for struct in named_structs}
    if set(forwards) != named:
        missing = sorted(named - set(forwards))
        extra = sorted(set(forwards) - named)
        if missing:
            errors.append("structs defined but not forward-declared: {}".format(", ".join(missing)))
        if extra:
            errors.append("forward declarations without definitions: {}".format(", ".join(extra)))
    if len(enums) != EXPECTED_ENUMS:
        errors.append("enums: expected {}, parsed {}".format(EXPECTED_ENUMS, len(enums)))
    if len(callbacks) != EXPECTED_CALLBACKS:
        errors.append(
            "callback typedefs: expected {}, parsed {}".format(
                EXPECTED_CALLBACKS, len(callbacks)
            )
        )
    if len(aliases) != EXPECTED_ALIASES:
        errors.append("type aliases: expected {}, parsed {}".format(EXPECTED_ALIASES, len(aliases)))
    for required in ("QuercusRational", "QuercusTimeRange"):
        if required not in {alias["name"] for alias in aliases}:
            errors.append("missing required alias {}".format(required))
    if not inline_structs:
        errors.append("expected the inline QuercusMat3 struct definition")
    if not re.search(r"\b743\s+functions\b", header_text):
        errors.append("header provenance line no longer advertises 743 functions")
    if errors:
        print("error: C header no longer matches the reference generator:", file=sys.stderr)
        for error in errors:
            print("  - " + error, file=sys.stderr)
        print(
            "Fix the parser/tooling or update the expected counts deliberately; "
            "do not publish a partial reference.",
            file=sys.stderr,
        )
        raise SystemExit(1)


def fenced(language: str, code: str) -> list[str]:
    return ["```" + language, code, "```"]


def fallback_function(symbol: str) -> str:
    return "No Rust SDK doc comment is attached to this C ABI entry point."


def fallback_type(kind: str, name: str) -> str:
    return "No Rust SDK doc comment is available for this {}.".format(kind.lower())


def enum_block(enum: dict) -> list[str]:
    lines: list[str] = []
    if enum["kind"] == "define":
        lines.append("Underlying C type: `{}`; values are `#define` constants.".format(enum["storage"]))
        lines.append("")
        code = ["typedef {} {};".format(enum["storage"], enum["name"]), ""]
        code.extend(enum["values"])
        lines.extend(fenced("c", "\n".join(code)))
    else:
        lines.append("C `enum` (storage type is chosen by the compiler).")
        lines.append("")
        code = ["typedef enum {} {{".format(enum["name"])]
        for index, value in enumerate(enum["values"]):
            suffix = "," if index + 1 < len(enum["values"]) else ""
            code.append("    " + value + suffix)
        code.append("}} {};".format(enum["alias"]))
        lines.extend(fenced("c", "\n".join(code)))
    return lines


def struct_block(struct: dict) -> list[str]:
    code = ["struct {} {{".format(struct["name"])]
    for field in struct["fields"]:
        code.append("    " + field + ";")
    code.append("};")
    return fenced("c", "\n".join(code))


def render(facts: dict) -> str:
    functions = facts["functions"]
    modules = facts["modules"]
    ordered_modules = facts["ordered_modules"]
    named_structs = facts["named_structs"]
    inline_structs = facts["inline_structs"]
    enums = facts["enums"]
    callbacks = facts["callbacks"]
    aliases = facts["aliases"]
    function_docs = facts["function_docs"]
    type_docs = facts["type_docs"]
    callback_docs = facts["callback_docs"]
    header_comments = facts["header_comments"]

    documented = facts["documented_functions"]
    undoc = len(functions) - documented

    out: list[str] = []
    out.append("# Quercus Engine C API Reference")
    out.append("")
    out.append(
        "> **Generated file — do not edit.** Regenerate with "
        "`python3 tooling/gen_c_api_docs.py --write`."
    )
    out.append("")
    out.append("| Property | Value |")
    out.append("| --- | --- |")
    out.append("| Source header | `crates/quercus-engine/include/quercus_engine.h` |")
    out.append("| Header SHA-256 | `{}` |".format(facts["header_hash"]))
    out.append("| Header lines | {} |".format(facts["header_lines"]))
    out.append("| Functions | {} |".format(len(functions)))
    out.append("| Modules | {} |".format(len(ordered_modules)))
    out.append("| Named POD structs | {} |".format(len(named_structs)))
    out.append(
        "| Additional inline structs | {} ({}) |".format(
            len(inline_structs), ", ".join("`{}`".format(s["name"]) for s in inline_structs)
        )
    )
    out.append("| Enums | {} |".format(len(enums)))
    out.append("| Callback typedefs | {} |".format(len(callbacks)))
    out.append("| Type aliases | {} |".format(len(aliases)))
    out.append("")
    out.append(
        "`cargo doc` (rustdoc) remains the reference for the **safe Rust SDK** "
        "(`quercus-engine-rs`), including wrapper types, constants, and examples. "
        "This page is a separate, generated reference for the **C ABI** declared "
        "by the checked-in C header, so C/C++ hosts and hand-written FFI "
        "bindings have one authoritative listing of every exported function and "
        "POD type. It intentionally describes the C surface, not Rust API "
        "semantics beyond what the SDK doc comments state."
    )
    out.append("")
    out.append(
        "For host build, linking, and threading guidance, see "
        "[`docs/c-api/README.md`](README.md)."
    )
    out.append("")
    out.append("## Regenerating and checking")
    out.append("")
    out.append("```sh")
    out.append("python3 tooling/gen_c_api_docs.py --write   # regenerate this file")
    out.append("python3 tooling/gen_c_api_docs.py --check   # fail if it is stale")
    out.append("python3 tooling/gen_c_header.py --check     # verify the source header")
    out.append("```")
    out.append("")
    out.append("## Conventions")
    out.append("")
    out.append(
        "- Time values are seconds: `double` where the prototype says `double`, "
        "and exact `Rational` (`num`/`den`) values where it says `Rational`."
    )
    out.append(
        "- `QuercusRational` is an alias for `Rational`, `QuercusTimeRange` is an "
        "alias for `TimeRange`, and `QuercusNodeValue` is an alias for `NodeValue`. "
        "The `Quercus*` spellings are the engine-side names; the C ABI uses the "
        "shorter names."
    )
    out.append(
        "- Opaque handles are `void*` / `const void*`. Ownership and lifetime rules "
        "are not encoded in C types; follow the owning Rust SDK documentation."
    )
    out.append(
        "- Byte buffers are passed as a pointer plus an explicit length wherever "
        "the prototype shows a `(data, len)` pair. They are not implicitly "
        "NUL-terminated unless a `char*` parameter says otherwise."
    )
    out.append(
        "- Boolean parameters and returns are C `bool` (`<stdbool.h>`) and cross "
        "the ABI as 0/1."
    )
    out.append("")
    out.append("## Contents")
    out.append("")
    out.append("| Module | Functions |")
    out.append("| --- | ---: |")
    for module in ordered_modules:
        out.append(
            "| [`{}`](#module-{}) | {} |".format(module, module, len(modules[module]))
        )
    out.append("| **Total** | **{}** |".format(len(functions)))
    out.append("")

    # ---------------------------------------------------------------- enums
    out.append("## Enums ({})".format(len(enums)))
    out.append("")
    out.append(
        "The header declares enums in two forms: C `typedef enum` definitions and "
        "integer typedefs whose values are macros (`QuercusEditOp`). Enumerator "
        "order and values are reproduced verbatim from the header."
    )
    out.append("")
    for enum in sorted(enums, key=lambda item: item["name"]):
        name = enum["name"]
        out.append("### `{}`".format(name))
        out.append("")
        summary = (
            type_docs.get(name)
            or header_comments.get(name)
            or fallback_type("Enum", name)
        )
        out.append(summary)
        out.append("")
        out.extend(enum_block(enum))
        out.append("")

    # -------------------------------------------------------------- structs
    out.append("## Structs ({} named POD structs)".format(len(named_structs)))
    out.append("")
    out.append(
        "All structs are plain data (`#[repr(C)]` in Rust). Field declarations "
        "are reproduced verbatim from the header. The header also type-checks the "
        "layout of `Rational` (16 bytes), `TimeRange` (32 bytes), `NodeValue` "
        "(56 bytes), and `StorageProjectHandle` (32 bytes) with static assertions."
    )
    out.append("")
    for struct in sorted(named_structs, key=lambda item: item["name"]):
        name = struct["name"]
        out.append("### `{}`".format(name))
        out.append("")
        summary = (
            type_docs.get(name)
            or header_comments.get(name)
            or fallback_type("POD struct", name)
        )
        out.append(summary)
        out.append("")
        out.extend(struct_block(struct))
        out.append("")

    for struct in sorted(inline_structs, key=lambda item: item["name"]):
        out.append("### `{}`".format(struct["name"]))
        out.append("")
        out.append("Inline struct definition (declared and defined in one typedef).")
        out.append("")
        summary = (
            type_docs.get(struct["name"])
            or header_comments.get(struct["name"])
            or fallback_type("POD struct", struct["name"])
        )
        out.append(summary)
        out.append("")
        out.extend(fenced("c", struct["raw"]))
        out.append("")

    # -------------------------------------------------------------- aliases
    out.append("## Type aliases ({})".format(len(aliases)))
    out.append("")
    for alias in sorted(aliases, key=lambda item: item["name"]):
        out.append("### `{}`".format(alias["name"]))
        out.append("")
        out.append(ALIAS_NOTES.get(alias["name"], "Alias for `{}`.".format(alias["target"])))
        out.append("")
        out.extend(
            fenced("c", "typedef {} {};".format(alias["target"], alias["name"]))
        )
        out.append("")

    # ------------------------------------------------------------ callbacks
    out.append("## Callback typedefs ({})".format(len(callbacks)))
    out.append("")
    out.append(
        "Callbacks cross the ABI as plain C function pointers. `userdata` is "
        "passed through unchanged; it is never dereferenced by the engine."
    )
    out.append("")
    for callback in sorted(callbacks, key=lambda item: item["name"]):
        name = callback["name"]
        out.append("### `{}`".format(name))
        out.append("")
        summary = (
            callback_docs.get(name)
            or header_comments.get(name)
            or fallback_type("Callback typedef", name)
        )
        out.append(summary)
        out.append("")
        out.extend(fenced("c", callback["raw"]))
        out.append("")

    # ------------------------------------------------------------- modules
    for module in ordered_modules:
        items = modules[module]
        # Explicit anchor keeps the Contents links stable across Markdown
        # implementations, which otherwise derive slugs from the full heading.
        out.append('<a id="module-{}"></a>'.format(module))
        out.append("## Module: `{}` ({} functions)".format(module, len(items)))
        out.append("")
        for function in items:
            symbol = function["symbol"]
            summary = (
                function_docs.get(symbol)
                or header_comments.get(symbol)
                or fallback_function(symbol)
            )
            # Symbol and one-line summary share the heading line; the exact C
            # prototype follows in its own fenced block. This keeps the full
            # 743-function reference compact and scannable.
            out.append("### `{}` — {}".format(symbol, summary))
            out.extend(fenced("c", function["prototype"]))

    # ------------------------------------------------------------ coverage
    out.append("## Documentation coverage")
    out.append("")
    out.append(
        "Summaries are harvested from contiguous `///` blocks in the Rust SDK by "
        "matching wrapper functions to the `quercus_*` symbols they call. Where "
        "no doc comment exists, a neutral fallback marks the entry undocumented; "
        "no behavior is inferred from the name. Harvested summaries quote the "
        "Rust wrapper, so they may use Rust vocabulary (`Option`, `None`, "
        "`Result`); the C prototype on each entry is authoritative for the ABI."
    )
    out.append("")
    out.append("| Item | Documented | Total |")
    out.append("| --- | ---: | ---: |")
    out.append(
        "| Functions (Rust SDK summary) | {} | {} |".format(documented, len(functions))
    )
    out.append("| Functions (neutral fallback) | {} | {} |".format(undoc, len(functions)))
    type_total = len(named_structs) + len(inline_structs) + len(enums) + len(callbacks)
    type_documented = sum(
        1
        for item in list(named_structs) + list(inline_structs)
        if item["name"] in type_docs or item["name"] in header_comments
    )
    type_documented += sum(
        1 for enum in enums if enum["name"] in type_docs or enum["name"] in header_comments
    )
    type_documented += sum(
        1
        for callback in callbacks
        if callback["name"] in callback_docs or callback["name"] in header_comments
    )
    out.append(
        "| Types and callbacks (Rust SDK summary) | {} | {} |".format(
            type_documented, type_total
        )
    )
    out.append("")

    while out and out[-1] == "":
        out.pop()
    return "\n".join(out) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help="write the checked-in reference")
    parser.add_argument("--check", action="store_true", help="fail when the reference is stale")
    args = parser.parse_args()

    facts = build_header_facts()
    reference = render(facts)

    if args.write:
        OUTPUT.parent.mkdir(parents=True, exist_ok=True)
        OUTPUT.write_text(reference, encoding="utf-8")
        print(
            "wrote {} ({} lines, {} functions)".format(
                OUTPUT.relative_to(ROOT), len(reference.splitlines()), len(facts["functions"])
            )
        )
        return 0
    if args.check:
        current = OUTPUT.read_text(encoding="utf-8") if OUTPUT.exists() else ""
        if current != reference:
            print(
                "docs/c-api/reference.md is stale; run "
                "python3 tooling/gen_c_api_docs.py --write",
                file=sys.stderr,
            )
            return 1
        print("docs/c-api/reference.md is up to date")
        return 0
    sys.stdout.write(reference)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
