#!/usr/bin/env python3
"""Extract GNU Emacs DEFUN doc: text into a Rust source file.

Walks every `.c` file in GNU Emacs's `src/` directory looking for
DEFUN declarations of the form:

    DEFUN ("name", Fname, Sname, MIN, MAX, INTERACTIVE,
           doc: /* DOCSTRING TEXT
    POSSIBLY MULTI-LINE */)
      (ARG_DECLS)

For each match, emits a `(name, doc)` tuple to a Rust file:

    pub(crate) static GNU_SUBR_DOCS: &[(&str, &str)] = &[
        ("name", "DOCSTRING TEXT\nPOSSIBLY MULTI-LINE"),
        ...
    ];

Output is sorted alphabetically by name (so a future binary search
can replace the linear scan).

The script is intentionally simple — it doesn't try to handle every
GNU C corner case, just the canonical DEFUN shape that covers
~99% of subrs. Edge cases (#ifdef-gated DEFUNs, manual doc strings
set in syms_of_*) are left as TODO comments.

Usage:
    scripts/extract_gnu_defun_docs.py \\
        --gnu-src /path/to/emacs-mirror/emacs/src \\
        --output  neovm-core/src/emacs_core/subr_docs/gnu_table.rs
"""

import argparse
import os
import re
import sys
from pathlib import Path


# DEFUN ("name", Fname, Sname, MIN, MAX, INTERACTIVE,
#        doc: /* docstring */)
# We capture name and MAX args. MAX is either an integer (fixed
# arity), `MANY' (variadic, expects usage: in doc), or `UNEVALLED'
# (special form, no fn line emitted).
DEFUN_HEAD = re.compile(
    r'^\s*DEFUN\s*\(\s*"([^"]+)"\s*,'
    r'\s*F[A-Za-z0-9_]+\s*,'
    r'\s*S[A-Za-z0-9_]+\s*,'
    r'\s*([A-Z0-9_]+)\s*,'  # MIN args
    r'\s*([A-Z0-9_]+)\s*,'  # MAX args
    r'\s*[^,]+\s*,'        # interactive spec
    r'\s*$',
    re.MULTILINE,
)

# usage: line at start of a doc-body line: `usage: (FUNCNAME ARG1 ARG2 ...)'.
# GNU make-docfile rewrites this to `(fn ARG1 ARG2 ...)'.
USAGE_LINE_RE = re.compile(
    r'^[ \t]*usage:\s*\(\s*[^ \t)]*\s*([^)]*)\)',
    re.MULTILINE,
)


def find_doc_block(text: str, start: int) -> tuple[str | None, int]:
    """Find the `doc: /* ... */)` block starting at or after `start`.
    Returns (doc_text, end_offset) or (None, start) on failure.
    """
    # Find the `doc:` marker
    doc_marker = text.find("doc: /*", start)
    if doc_marker == -1:
        return None, start
    # Look ahead for the closing `*/`
    body_start = doc_marker + len("doc: /*")
    body_end = text.find("*/", body_start)
    if body_end == -1:
        return None, start
    body = text[body_start:body_end]
    # GNU's `doc: /* TEXT` block has a single leading space after `/*`
    # for readability (matching make-docfile.c's stripping). Drop it.
    if body.startswith(" "):
        body = body[1:]
    # GNU make-docfile.c also strips a trailing space before `*/'.
    if body.endswith(" "):
        body = body[:-1]
    doc = body.rstrip()
    return doc, body_end + 2


def parse_c_arglist(c_args: str) -> list[str]:
    """Extract C parameter names from `(register Lisp_Object foo, ...)'.
    Mirrors GNU make-docfile.c::write_c_args. Skips storage qualifiers
    and `void'. Returns identifier names in declaration order.
    """
    # Strip outer parens
    s = c_args.strip()
    if s.startswith("("):
        s = s[1:]
    if s.endswith(")"):
        s = s[:-1]
    args = []
    for arg_chunk in s.split(","):
        arg_chunk = arg_chunk.strip()
        if not arg_chunk or arg_chunk == "void":
            continue
        # The parameter NAME is the last identifier in the chunk
        # (e.g. "register Lisp_Object foo" -> "foo").
        # Tokenize on whitespace and pointer/array decorators.
        toks = re.findall(r'[A-Za-z_][A-Za-z0-9_]*', arg_chunk)
        if not toks:
            continue
        last = toks[-1]
        if last == "void":
            continue
        args.append(last)
    return args


def format_fn_line(args: list[str]) -> str:
    """Render `(fn ARG1 ARG2 ...)' GNU-style: uppercase, `_' -> `-',
    `defalt' -> `DEFAULT'. Empty args list still emits `(fn)'.
    """
    parts = []
    for a in args:
        if a == "defalt":
            parts.append("DEFAULT")
        else:
            parts.append(a.upper().replace("_", "-"))
    if parts:
        return "(fn " + " ".join(parts) + ")"
    return "(fn)"


def find_c_arglist_after(text: str, start: int) -> tuple[str | None, int]:
    """Find the C function arg list `(...)` starting at or after `start`.
    Used after the `*/)` closing of a DEFUN to read the actual C
    parameter declaration. Returns (raw_arglist_text, end_offset) or
    (None, start).
    """
    # Skip past the closing `)` of DEFUN(...)
    pos = text.find(")", start)
    if pos == -1:
        return None, start
    pos += 1
    # Skip whitespace and comments
    while pos < len(text) and text[pos] in " \t\r\n":
        pos += 1
    if pos >= len(text) or text[pos] != "(":
        return None, start
    # Match balanced parens
    depth = 0
    open_pos = pos
    while pos < len(text):
        c = text[pos]
        if c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
            if depth == 0:
                return text[open_pos:pos + 1], pos + 1
        pos += 1
    return None, start


def rewrite_usage_line(doc: str) -> tuple[str, bool]:
    """Replace a `usage: (FUNCNAME ARGS...)' line with `(fn ARGS...)'.
    Returns the rewritten doc and a flag indicating whether a usage
    line was found. Mirrors GNU make-docfile.c::scan_keyword_or_put_char.
    """
    m = USAGE_LINE_RE.search(doc)
    if not m:
        return doc, False
    args = m.group(1).strip()
    fn_line = "(fn " + args + ")" if args else "(fn)"
    return doc[:m.start()] + fn_line + doc[m.end():], True


def extract_defuns(src: str) -> list[tuple[str, str]]:
    """Extract `(name, doc)' pairs from a single C source file."""
    results = []
    pos = 0
    while True:
        m = DEFUN_HEAD.search(src, pos)
        if not m:
            break
        name = m.group(1)
        max_args = m.group(3)
        # Look for doc: starting from end of the matched DEFUN head
        doc, doc_end = find_doc_block(src, m.end())
        if doc is None:
            pos = m.end()
            continue

        # Check for `usage:' line first; if present, rewrite it.
        doc, saw_usage = rewrite_usage_line(doc)

        # GNU make-docfile rules for the (fn ARGS) suffix:
        #   - UNEVALLED: special form, no (fn) line.
        #   - MANY: variadic, the doc must contain a `usage:' line
        #     (already rewritten above). Don't auto-generate.
        #   - Numeric (0-8): read the C arg list and append `(fn ARGS)'.
        if max_args.isdigit() and not saw_usage:
            c_arglist, next_pos = find_c_arglist_after(src, doc_end)
            if c_arglist is not None:
                args = parse_c_arglist(c_arglist)
                fn_line = format_fn_line(args)
                doc = doc.rstrip() + "\n\n" + fn_line
                pos = next_pos
            else:
                pos = doc_end
        else:
            pos = doc_end

        results.append((name, doc))
    return results


def rust_string_literal(s: str) -> str:
    """Format a Rust string literal that handles `\\`, `"`, and newlines.
    Uses raw string syntax `r#"..."#` when possible to preserve grave
    quotes verbatim, falling back to escaped form if the raw delimiter
    appears in the body."""
    # Try plain raw string first
    if '"#' not in s and not any(ord(c) < 32 and c != "\n" and c != "\t" for c in s):
        # Use r#"..."# raw string. Need to find a hash count not in the body.
        for hashes in ["#", "##", "###"]:
            if f'"{hashes}' not in s:
                return f'r{hashes}"{s}"{hashes}'
    # Fall back to escaped form
    escaped = (
        s.replace("\\", "\\\\")
        .replace('"', '\\"')
        .replace("\n", "\\n")
        .replace("\t", "\\t")
    )
    return f'"{escaped}"'


def emit_rust(entries: list[tuple[str, str]], output: Path) -> None:
    entries_sorted = sorted(entries, key=lambda kv: kv[0])
    lines = [
        "// AUTO-GENERATED by scripts/extract_gnu_defun_docs.py — DO NOT EDIT.",
        "//",
        "// Source: GNU Emacs `src/*.c` DEFUN doc: text.",
        "// Re-run the extractor against an updated GNU mirror to refresh.",
        "//",
        "// Each entry is `(name, raw_grave_quoted_doc)' lifted verbatim",
        "// from the corresponding `DEFUN (\"name\", ..., doc: /* TEXT */)'",
        "// block. Strings preserve GNU's grave-quote convention so that",
        "// `substitute-command-keys' can convert them per the user's",
        "// `text-quoting-style' at display time.",
        "",
        "pub(crate) static GNU_SUBR_DOCS: &[(&str, &str)] = &[",
    ]
    for name, doc in entries_sorted:
        name_lit = rust_string_literal(name)
        doc_lit = rust_string_literal(doc)
        lines.append(f"    ({name_lit}, {doc_lit}),")
    lines.append("];")
    lines.append("")
    output.write_text("\n".join(lines))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--gnu-src",
        type=Path,
        required=True,
        help="Path to GNU Emacs's src/ directory",
    )
    parser.add_argument(
        "--output",
        type=Path,
        required=True,
        help="Path to write the generated Rust file",
    )
    args = parser.parse_args()

    if not args.gnu_src.is_dir():
        print(f"error: {args.gnu_src} is not a directory", file=sys.stderr)
        return 1

    all_entries: list[tuple[str, str]] = []
    seen_names: set[str] = set()
    for c_file in sorted(args.gnu_src.glob("*.c")):
        try:
            src = c_file.read_text(encoding="utf-8", errors="replace")
        except OSError as e:
            print(f"warning: cannot read {c_file}: {e}", file=sys.stderr)
            continue
        entries = extract_defuns(src)
        for name, doc in entries:
            if name in seen_names:
                # Multiple DEFUNs for the same name across files
                # (e.g. xterm.c vs w32term.c). Keep the first one
                # we see; warn but don't fail.
                print(
                    f"note: duplicate DEFUN '{name}' in {c_file.name}, "
                    f"keeping the earlier definition",
                    file=sys.stderr,
                )
                continue
            seen_names.add(name)
            all_entries.append((name, doc))

    args.output.parent.mkdir(parents=True, exist_ok=True)
    emit_rust(all_entries, args.output)
    print(
        f"extracted {len(all_entries)} DEFUN docs from "
        f"{args.gnu_src} -> {args.output}",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
