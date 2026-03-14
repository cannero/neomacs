//! Regex engine and search primitives for the Elisp VM.
//!
//! Uses the `regex` crate as the backend.  Translates basic Emacs regex
//! syntax to Rust regex syntax before compiling patterns.

use regex::Regex;
use std::cell::RefCell;

use crate::buffer::{Buffer, BufferId};
use crate::emacs_core::casefiddle::apply_replace_match_case;
use crate::emacs_core::value::with_heap;

pub(crate) const REPLACE_MATCH_SUBEXP_MISSING: &str = "replace-match subexpression does not exist";
const SEARCH_PATTERN_CACHE_SIZE: usize = 20;

#[derive(Clone)]
enum CompiledSearchPattern {
    Literal(String),
    Segmented(SegmentedPattern),
    Backref(BackrefPattern),
    Regex(Regex),
}

pub(crate) struct IteratedStringMatches {
    pub capture_count: usize,
    pub matches: Vec<Vec<Option<(usize, usize)>>>,
}

#[derive(Clone)]
struct SegmentedPattern {
    segments: Vec<SegmentedPatternPart>,
    capture_count: usize,
}

#[derive(Clone)]
enum SegmentedPatternPart {
    Literal(String),
    Capture(SegmentedCapture),
}

#[derive(Clone, Copy)]
enum SegmentedCapture {
    AnyLazy,
    NegatedCharPlus(char),
}

#[derive(Clone)]
struct BackrefPattern {
    expr: BackrefExpr,
    group_count: usize,
}

#[derive(Clone)]
struct BackrefExpr {
    branches: Vec<Vec<BackrefNode>>,
}

#[derive(Clone)]
struct BackrefNode {
    atom: BackrefAtom,
    repeat: Quantifier,
}

#[derive(Clone)]
enum BackrefAtom {
    Literal(char),
    AnyChar,
    CharClass(BackrefCharClass),
    NonAsciiCategory,
    Digit,
    NotDigit,
    WordChar,
    NotWordChar,
    SyntaxClass(BackrefSyntaxClass, bool),
    LineStart,
    LineEnd,
    WordBoundary,
    NotWordBoundary,
    SymbolStart,
    SymbolEnd,
    StartPoint,
    StartBuffer,
    EndBuffer,
    Group(usize, BackrefExpr),
    NonCapturing(BackrefExpr),
    Backref(usize),
}

#[derive(Clone, Copy)]
enum BackrefSyntaxClass {
    Whitespace,
    Word,
    Symbol,
    Punct,
    OpenDelim,
    CloseDelim,
    StringQuote,
}

#[derive(Clone)]
struct BackrefCharClass {
    negated: bool,
    items: Vec<BackrefCharClassItem>,
}

#[derive(Clone)]
enum BackrefCharClassItem {
    Literal(char),
    Range(char, char),
    Posix(BackrefPosixClass),
    NonAsciiCategory,
    Digit,
    NotDigit,
    WordChar,
    NotWordChar,
    SyntaxClass(BackrefSyntaxClass, bool),
}

#[derive(Clone, Copy)]
enum BackrefPosixClass {
    Alpha,
    Alnum,
    Digit,
    Space,
    Upper,
    Lower,
    Punct,
    Blank,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Quantifier {
    min: usize,
    max: Option<usize>,
    greedy: bool,
}

impl Quantifier {
    const ONE: Self = Self {
        min: 1,
        max: Some(1),
        greedy: true,
    };
}

#[derive(Clone)]
struct BackrefState {
    pos: usize,
    search_start: usize,
    groups: Vec<Option<(usize, usize)>>,
}

thread_local! {
    static SEARCH_PATTERN_CACHE: RefCell<Vec<(bool, String, CompiledSearchPattern)>> =
        const { RefCell::new(Vec::new()) };
}

// ---------------------------------------------------------------------------
// MatchData
// ---------------------------------------------------------------------------

/// Match data from the last successful search.
#[derive(Clone, Debug)]
pub struct MatchData {
    /// Full match and capture groups: (start_byte, end_byte) pairs.
    /// Index 0 = full match, 1+ = capture groups.
    pub groups: Vec<Option<(usize, usize)>>,
    /// The string that was searched (for `string-match`).
    /// `None` when the search was performed on a buffer.
    pub searched_string: Option<SearchedString>,
    /// The buffer that was searched, when match data came from a buffer search.
    pub searched_buffer: Option<BufferId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchedString {
    Heap(crate::gc::types::ObjId),
    Owned(String),
}

impl SearchedString {
    pub(crate) fn with_str<R>(&self, f: impl FnOnce(&str) -> R) -> R {
        match self {
            Self::Heap(id) => with_heap(|heap| f(heap.get_string(*id))),
            Self::Owned(text) => f(text),
        }
    }

    pub(crate) fn to_owned(&self) -> String {
        self.with_str(str::to_owned)
    }
}

impl MatchData {
    pub(crate) fn searched_string_text(&self) -> Option<String> {
        self.searched_string.as_ref().map(SearchedString::to_owned)
    }
}

// ---------------------------------------------------------------------------
// Emacs → Rust regex translation
// ---------------------------------------------------------------------------

/// Translate basic Emacs regex syntax to Rust regex syntax.
///
/// Key differences handled:
/// - Emacs `\(` `\)` for groups  →  Rust `(` `)`
/// - Emacs `\|` for alternation  →  Rust `|`
/// - Emacs `\{` `\}` for repetition  →  Rust `{` `}`
/// - Emacs `\1`..`\9` for back-references  →  not supported by `regex` crate,
///   but we translate the syntax anyway for completeness
/// - Emacs literal `(` `)` `{` `}` `|`  →  Rust `\(` `\)` `\{` `\}` `\|`
/// - Emacs `\w` (word char)  →  Rust `\w`
/// - Emacs `\W` (non-word char)  →  Rust `\W`
/// - Emacs `\b` (word boundary)  →  Rust `\b`
/// - Emacs `\B` (non-word boundary)  →  Rust `\B`
/// - Emacs `\s-` etc. (syntax classes)  →  simplified to `\s` (whitespace)
/// - Emacs `\<` `\>` (word boundaries)  →  Rust `\b`
/// - Emacs character classes inside `[...]` are kept as-is.
pub fn translate_emacs_regex(pattern: &str) -> String {
    fn next_char_at(s: &str, byte_idx: usize) -> Option<(char, usize)> {
        s.get(byte_idx..)
            .and_then(|tail| tail.chars().next().map(|ch| (ch, ch.len_utf8())))
    }

    fn push_rust_class_char(out: &mut String, ch: char) {
        match ch {
            '\\' => out.push_str("\\\\"),
            '[' => out.push_str("\\["),
            _ => out.push(ch),
        }
    }

    let mut out = String::with_capacity(pattern.len() + 8);
    let bytes = pattern.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_bracket = false;
    let mut bracket_negated = false;
    // Position in `out` where bracket content starts (after `[` / `[^`).
    // Used to detect empty classes after removing reversed ranges.
    let mut bracket_content_start: usize = 0;

    while i < len {
        let (ch, ch_len) = next_char_at(pattern, i).expect("byte index must be char boundary");

        // Non-ASCII literal bytes should be preserved as full UTF-8 scalar values.
        if !ch.is_ascii() {
            out.push(ch);
            i += ch_len;
            continue;
        }

        // Inside a character class [...], handle Emacs/Rust differences:
        //  - `\` is literal in Emacs and can still participate in ranges
        //  - Reversed ranges like `z-a` are empty in Emacs but error in Rust → remove
        //  - `]` at first position is literal in Emacs → escape it for Rust
        if in_bracket {
            if ch == ']' {
                in_bracket = false;
                if out.len() == bracket_content_start {
                    // Bracket has no content (all ranges were reversed/removed).
                    // [^] → matches anything, [] → matches nothing.
                    // Truncate the opening `[` or `[^` and emit a replacement.
                    let open_len = if bracket_negated { 2 } else { 1 };
                    out.truncate(bracket_content_start - open_len);
                    if bracket_negated {
                        out.push_str("[\\s\\S]");
                    } else {
                        // Empty positive class — can never match.
                        // Use a character class that accepts no character to
                        // avoid unsupported look-around constructs.
                        out.push_str("[^\\s\\S]");
                    }
                } else {
                    out.push(']');
                }
                i += 1;
                continue;
            }
            if ch == '\\' {
                if i + 1 < len && bytes[i + 1] == b']' {
                    // GNU Emacs does not treat \] inside [...] as a literal ].
                    // Keep the backslash as a literal class member and let the
                    // following ] close the class on the next iteration.
                    push_rust_class_char(&mut out, ch);
                    i += 1;
                    continue;
                }
                if i + 2 < len && bytes[i + 1] == b'-' && bytes[i + 2] != b']' {
                    let (end_ch, end_len) =
                        next_char_at(pattern, i + 2).expect("byte index must be char boundary");
                    if ch > end_ch {
                        // GNU Emacs treats `\-x` like a range from `\` to `x`.
                        // If the range is reversed, it is empty.
                        i += 1 + 1 + end_len;
                        continue;
                    }
                    push_rust_class_char(&mut out, ch);
                    out.push('-');
                    push_rust_class_char(&mut out, end_ch);
                    i += 1 + 1 + end_len;
                } else {
                    push_rust_class_char(&mut out, ch);
                    i += 1;
                }
                continue;
            }
            if ch == '[' {
                // In Emacs, `[` inside [...] is literal.  In Rust regex
                // it starts a nested character class.  Escape it.
                // Exception: POSIX classes like [:alpha:] — pass through.
                if i + 1 < len && bytes[i + 1] == b':' {
                    // Looks like a POSIX class `[:...:` — pass through.
                    out.push('[');
                } else {
                    out.push_str("\\[");
                }
                i += 1;
                continue;
            }
            // Check for ranges: if next is `-` and then a non-`]` char,
            // validate the range direction.
            if i + 2 < len && bytes[i + 1] == b'-' && bytes[i + 2] != b']' {
                let (end_ch, end_len) =
                    next_char_at(pattern, i + 2).expect("byte index must be char boundary");
                if ch > end_ch {
                    // Reversed range (e.g. z-a): empty in Emacs, skip entirely.
                    i += 1 + 1 + end_len;
                    continue;
                }
            }
            out.push(ch);
            i += ch_len;
            continue;
        }

        match ch {
            '[' => {
                in_bracket = true;
                bracket_negated = false;
                out.push('[');
                i += 1;
                // Handle `[^` — consume the negation prefix.
                if i < len && bytes[i] == b'^' {
                    out.push('^');
                    bracket_negated = true;
                    i += 1;
                }
                bracket_content_start = out.len();
                // `]` as first char (or first after `^`) is literal in Emacs.
                // In Rust regex it would close the class.  Escape it.
                if i < len && bytes[i] == b']' {
                    out.push_str("\\]");
                    i += 1;
                }
            }
            // Emacs uses literal `(`, `)`, `{`, `}`, `|` — escape them for Rust regex.
            '(' => {
                out.push_str("\\(");
                i += 1;
            }
            ')' => {
                out.push_str("\\)");
                i += 1;
            }
            '{' => {
                out.push_str("\\{");
                i += 1;
            }
            '}' => {
                out.push_str("\\}");
                i += 1;
            }
            '|' => {
                out.push_str("\\|");
                i += 1;
            }
            '\\' if i + 1 < len => {
                let (next, next_len) =
                    next_char_at(pattern, i + 1).expect("byte index must be char boundary");
                match next {
                    // Emacs group → Rust group
                    '(' => {
                        let group_idx = i + 1 + next_len;
                        if group_idx < len && bytes[group_idx] == b'?' {
                            if group_idx + 1 < len && bytes[group_idx + 1] == b':' {
                                out.push_str("(?:");
                                i = group_idx + 2;
                                continue;
                            }

                            let digits_start = group_idx + 1;
                            let mut digits_end = digits_start;
                            while digits_end < len && bytes[digits_end].is_ascii_digit() {
                                digits_end += 1;
                            }
                            if digits_end > digits_start
                                && digits_end < len
                                && bytes[digits_end] == b':'
                            {
                                out.push('(');
                                i = digits_end + 1;
                                continue;
                            }
                        }

                        out.push('(');
                        i += 1 + next_len;
                    }
                    ')' => {
                        out.push(')');
                        i += 1 + next_len;
                    }
                    // Emacs alternation → Rust alternation
                    '|' => {
                        out.push('|');
                        i += 1 + next_len;
                    }
                    // Emacs repetition braces → Rust repetition braces
                    '{' => {
                        let interval_start = i + 1 + next_len;
                        let mut scan = interval_start;
                        while scan < len {
                            if bytes[scan] == b'\\' && scan + 1 < len && bytes[scan + 1] == b'}' {
                                let interval = &pattern[interval_start..scan];
                                out.push('{');
                                if let Some(rest) = interval.strip_prefix(',') {
                                    out.push('0');
                                    out.push(',');
                                    out.push_str(rest);
                                } else {
                                    out.push_str(interval);
                                }
                                out.push('}');
                                i = scan + 2;
                                continue;
                            }
                            scan += 1;
                        }
                        out.push('{');
                        i += 1 + next_len;
                    }
                    '}' => {
                        out.push('}');
                        i += 1 + next_len;
                    }
                    // Word boundaries
                    '<' => {
                        out.push_str("\\b");
                        i += 1 + next_len;
                    }
                    '>' => {
                        out.push_str("\\b");
                        i += 1 + next_len;
                    }
                    '_' => {
                        i += 1 + next_len;
                        if i < len {
                            let (boundary_ch, boundary_len) =
                                next_char_at(pattern, i).expect("byte index must be char boundary");
                            match boundary_ch {
                                '<' | '>' => {
                                    i += boundary_len;
                                    out.push_str("\\b");
                                }
                                _ => {
                                    out.push('_');
                                }
                            }
                        } else {
                            out.push('_');
                        }
                    }
                    // Back-references (1-9) — not supported by `regex` crate,
                    // but translate the syntax for pattern acceptance.
                    '1'..='9' => {
                        // Rust `regex` doesn't support back-refs; drop silently.
                        // In practice, patterns using \1..\9 will fail to compile
                        // which is acceptable for now.
                        out.push('\\');
                        out.push(next);
                        i += 1 + next_len;
                    }
                    // Emacs syntax classes (\s-, \sw, etc.)
                    // Map to the closest Rust regex equivalents.
                    's' => {
                        i += 1 + next_len;
                        // Consume the syntax-class character and map appropriately
                        if i < len {
                            let (class_ch, class_len) =
                                next_char_at(pattern, i).expect("byte index must be char boundary");
                            match class_ch {
                                '-' | ' ' => {
                                    // \s- or \s  → whitespace
                                    i += class_len;
                                    out.push_str("\\s");
                                }
                                'w' => {
                                    // \sw → word constituent
                                    i += class_len;
                                    out.push_str("\\w");
                                }
                                '_' => {
                                    // \s_ → symbol constituent (word + underscore)
                                    i += class_len;
                                    out.push_str("[\\w_]");
                                }
                                '.' => {
                                    // \s. → punctuation
                                    i += class_len;
                                    out.push_str("[[:punct:]]");
                                }
                                '(' => {
                                    // \s( → open delimiter
                                    i += class_len;
                                    out.push_str("[\\[\\(\\{]");
                                }
                                ')' => {
                                    // \s) → close delimiter
                                    i += class_len;
                                    out.push_str("[\\]\\)\\}]");
                                }
                                '"' => {
                                    // \s" → string quote character
                                    i += class_len;
                                    out.push_str("[\"']");
                                }
                                '\'' | '<' | '>' | '!' | '|' | '/' => {
                                    // Other syntax classes — approximate as whitespace
                                    i += class_len;
                                    out.push_str("\\s");
                                }
                                _ => {
                                    // No valid syntax-class char follows; treat as bare \s
                                    out.push_str("\\s");
                                }
                            }
                        } else {
                            out.push_str("\\s");
                        }
                    }
                    'S' => {
                        i += 1 + next_len;
                        // Consume the syntax-class character and map appropriately
                        if i < len {
                            let (class_ch, class_len) =
                                next_char_at(pattern, i).expect("byte index must be char boundary");
                            match class_ch {
                                '-' | ' ' => {
                                    // \S- or \S  → non-whitespace
                                    i += class_len;
                                    out.push_str("\\S");
                                }
                                'w' => {
                                    // \Sw → non-word constituent
                                    i += class_len;
                                    out.push_str("\\W");
                                }
                                '_' => {
                                    // \S_ → non-symbol constituent
                                    i += class_len;
                                    out.push_str("[^\\w_]");
                                }
                                '.' => {
                                    // \S. → non-punctuation
                                    i += class_len;
                                    out.push_str("[^[:punct:]]");
                                }
                                '(' => {
                                    // \S( → non-open-delimiter
                                    i += class_len;
                                    out.push_str("[^\\[\\(\\{]");
                                }
                                ')' => {
                                    // \S) → non-close-delimiter
                                    i += class_len;
                                    out.push_str("[^\\]\\)\\}]");
                                }
                                '"' => {
                                    // \S" → non-string-quote
                                    i += class_len;
                                    out.push_str("[^\"']");
                                }
                                '\'' | '<' | '>' | '!' | '|' | '/' => {
                                    // Other syntax classes — approximate as non-whitespace
                                    i += class_len;
                                    out.push_str("\\S");
                                }
                                _ => {
                                    // No valid syntax-class char follows; treat as bare \S
                                    out.push_str("\\S");
                                }
                            }
                        } else {
                            out.push_str("\\S");
                        }
                    }
                    'c' => {
                        i += 1 + next_len;
                        if i < len {
                            let (_, class_len) =
                                next_char_at(pattern, i).expect("byte index must be char boundary");
                            i += class_len;
                        }
                        // GNU Emacs category regexps are implemented in C and depend on
                        // the active category table. Rust's `regex` backend has no
                        // equivalent dynamic character-category predicate, so approximate
                        // category escapes as non-ASCII until the native engine is ported.
                        out.push_str("[^\\x00-\\x7F]");
                    }
                    // \= (match at point) → \A (match at start of search region)
                    '=' => {
                        out.push_str("\\A");
                        i += 1 + next_len;
                    }
                    // Known escape sequences — pass through
                    'w' | 'W' | 'b' | 'B' | 'd' | 'D' | 'n' | 't' | 'r' | '`' | '\'' => {
                        match next {
                            // \` (beginning of buffer) and \' (end of buffer) → \A and \z
                            '`' => {
                                out.push_str("\\A");
                                i += 1 + next_len;
                            }
                            '\'' => {
                                out.push_str("\\z");
                                i += 1 + next_len;
                            }
                            _ => {
                                out.push('\\');
                                out.push(next);
                                i += 1 + next_len;
                            }
                        }
                    }
                    // Literal backslash
                    '\\' => {
                        out.push_str("\\\\");
                        i += 1 + next_len;
                    }
                    // Anything else after `\` — pass through the escape
                    _ => {
                        if next.is_ascii() {
                            out.push('\\');
                        }
                        out.push(next);
                        i += 1 + next_len;
                    }
                }
            }
            // Lone trailing backslash — pass through
            '\\' => {
                out.push('\\');
                i += 1;
            }
            // All other chars — pass through as-is
            _ => {
                out.push(ch);
                i += 1;
            }
        }
    }

    out
}

fn trivial_regexp_p(pattern: &str) -> bool {
    let mut chars = pattern.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '.' | '*' | '+' | '?' | '[' | '^' | '$' => return false,
            '\\' => {
                let Some(next) = chars.next() else {
                    return false;
                };
                match next {
                    '|' | '(' | ')' | '`' | '\'' | 'b' | 'B' | '<' | '>' | 'w' | 'W' | 's'
                    | 'S' | '=' | '{' | '}' | '_' | 'c' | 'C' | '1' | '2' | '3' | '4' | '5'
                    | '6' | '7' | '8' | '9' => return false,
                    _ => {}
                }
            }
            _ => {}
        }
    }
    true
}

fn literal_from_trivial_regexp(pattern: &str) -> Option<String> {
    if !trivial_regexp_p(pattern) {
        return None;
    }

    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            out.push(chars.next()?);
        } else {
            out.push(ch);
        }
    }
    Some(out)
}

fn parse_segmented_capture(body: &str) -> Option<SegmentedCapture> {
    if body == r"\(?:.\|\n\)*?" {
        return Some(SegmentedCapture::AnyLazy);
    }

    let mut chars = body.chars();
    if chars.next()? != '[' || chars.next()? != '^' {
        return None;
    }
    let forbidden = chars.next()?;
    if chars.next()? != ']' || chars.next()? != '+' || chars.next().is_some() {
        return None;
    }
    Some(SegmentedCapture::NegatedCharPlus(forbidden))
}

fn segmented_literal_escape(ch: char) -> Option<char> {
    match ch {
        '\\' | '[' | ']' | '{' | '}' | '(' | ')' | '|' | '.' | '*' | '+' | '?' | '^' | '$' => {
            Some(ch)
        }
        _ => None,
    }
}

fn parse_segmented_pattern(pattern: &str) -> Option<SegmentedPattern> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut idx = 0usize;
    let mut literal = String::new();
    let mut segments = Vec::new();
    let mut capture_count = 0usize;

    while idx < chars.len() {
        if chars[idx] == '\\' && idx + 1 < chars.len() && chars[idx + 1] == '(' {
            if !literal.is_empty() {
                segments.push(SegmentedPatternPart::Literal(std::mem::take(&mut literal)));
            }
            idx += 2;
            let start = idx;
            let mut nested_groups = 0usize;
            while idx + 1 < chars.len() {
                if chars[idx] == '\\' {
                    match chars[idx + 1] {
                        '(' => {
                            nested_groups += 1;
                            idx += 2;
                            continue;
                        }
                        ')' => {
                            if nested_groups == 0 {
                                break;
                            }
                            nested_groups -= 1;
                            idx += 2;
                            continue;
                        }
                        _ => {}
                    }
                }
                idx += 1;
            }
            if idx + 1 >= chars.len() {
                return None;
            }
            let body: String = chars[start..idx].iter().collect();
            let capture = parse_segmented_capture(&body)?;
            segments.push(SegmentedPatternPart::Capture(capture));
            capture_count += 1;
            idx += 2;
            continue;
        }

        if chars[idx] == '\\' && idx + 1 < chars.len() {
            literal.push(segmented_literal_escape(chars[idx + 1])?);
            idx += 2;
            continue;
        }

        literal.push(chars[idx]);
        idx += 1;
    }

    if !literal.is_empty() {
        segments.push(SegmentedPatternPart::Literal(literal));
    }

    if capture_count == 0 {
        return None;
    }
    if !matches!(segments.first(), Some(SegmentedPatternPart::Literal(_))) {
        return None;
    }
    if !segments
        .iter()
        .any(|part| matches!(part, SegmentedPatternPart::Capture(_)))
    {
        return None;
    }

    Some(SegmentedPattern {
        segments,
        capture_count,
    })
}

fn pattern_contains_backrefs(pattern: &str) -> bool {
    let mut chars = pattern.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('1'..='9') => return true,
                Some(_) => {}
                None => break,
            }
        }
    }
    false
}

fn pattern_supported_by_backref_engine(pattern: &str) -> bool {
    let chars: Vec<char> = pattern.chars().collect();
    let mut idx = 0usize;
    let mut in_class = false;
    let mut first_in_class = false;
    let mut pending_quantifier = false;

    while idx < chars.len() {
        let ch = chars[idx];
        if in_class {
            if ch == ']' && !first_in_class {
                in_class = false;
                pending_quantifier = false;
                idx += 1;
                continue;
            }
            first_in_class = false;

            if ch == '[' && idx + 1 < chars.len() && chars[idx + 1] == ':' {
                idx += 2;
                let start = idx;
                while idx + 1 < chars.len() && !(chars[idx] == ':' && chars[idx + 1] == ']') {
                    idx += 1;
                }
                if idx + 1 >= chars.len() {
                    return false;
                }
                let name: String = chars[start..idx].iter().collect();
                if !matches!(
                    name.as_str(),
                    "alpha" | "alnum" | "digit" | "space" | "upper" | "lower" | "punct" | "blank"
                ) {
                    return false;
                }
                pending_quantifier = false;
                idx += 2;
                continue;
            }

            if ch == '\\' {
                if matches!(chars.get(idx + 1), Some(']')) {
                    pending_quantifier = false;
                    idx += 1;
                    continue;
                }
                let Some(next) = chars.get(idx + 1) else {
                    return false;
                };
                match *next {
                    'd' | 'D' | 'w' | 'W' | 'n' | 't' | 'r' | '\\' | '[' | ']' | '-' | '^'
                    | '.' | '*' | '+' | '?' | '{' | '}' | '(' | ')' | '|' => {
                        pending_quantifier = false;
                        idx += 2;
                        continue;
                    }
                    's' | 'S' | 'c' => {
                        if chars.get(idx + 2).is_none() {
                            return false;
                        }
                        pending_quantifier = false;
                        idx += 3;
                        continue;
                    }
                    _ if !next.is_ascii() => {
                        pending_quantifier = false;
                        idx += 2;
                        continue;
                    }
                    _ => return false,
                }
            }

            pending_quantifier = false;
            idx += 1;
            continue;
        }

        match ch {
            '[' => {
                in_class = true;
                first_in_class = true;
                pending_quantifier = false;
                idx += 1;
            }
            '*' | '+' => {
                pending_quantifier = true;
                idx += 1;
            }
            '?' => {
                if pending_quantifier {
                    pending_quantifier = false;
                } else {
                    pending_quantifier = true;
                }
                idx += 1;
            }
            '\\' => {
                let Some(next) = chars.get(idx + 1).copied() else {
                    return false;
                };
                match next {
                    '(' => {
                        if chars.get(idx + 2) == Some(&'?') {
                            if chars.get(idx + 3) == Some(&':') {
                                idx += 4;
                            } else {
                                let mut digits_idx = idx + 3;
                                while matches!(chars.get(digits_idx), Some('0'..='9')) {
                                    digits_idx += 1;
                                }
                                if digits_idx == idx + 3 || chars.get(digits_idx) != Some(&':') {
                                    return false;
                                }
                                idx = digits_idx + 1;
                            }
                        } else {
                            idx += 2;
                        }
                    }
                    ')'
                    | '|'
                    | '1'..='9'
                    | 'w'
                    | 'W'
                    | 'b'
                    | 'B'
                    | 'd'
                    | 'D'
                    | 's'
                    | 'S'
                    | 'c'
                    | '_'
                    | '='
                    | 'n'
                    | 't'
                    | 'r'
                    | '`'
                    | '\''
                    | '<'
                    | '>'
                    | '\\'
                    | '.'
                    | '*'
                    | '+'
                    | '?'
                    | '['
                    | ']'
                    | '^'
                    | '$' => {
                        pending_quantifier = false;
                        if matches!(next, 's' | 'S' | 'c' | '_') && chars.get(idx + 2).is_none() {
                            return false;
                        }
                        idx += if matches!(next, 's' | 'S' | 'c' | '_') {
                            3
                        } else {
                            2
                        };
                    }
                    '{' => {
                        let mut scan = idx + 2;
                        let has_min = matches!(chars.get(scan), Some('0'..='9'));
                        while matches!(chars.get(scan), Some('0'..='9')) {
                            scan += 1;
                        }
                        if chars.get(scan) == Some(&',') {
                            scan += 1;
                            while matches!(chars.get(scan), Some('0'..='9')) {
                                scan += 1;
                            }
                        } else if !has_min {
                            return false;
                        }
                        if chars.get(scan) != Some(&'\\') || chars.get(scan + 1) != Some(&'}') {
                            return false;
                        }
                        pending_quantifier = true;
                        idx = scan + 2;
                    }
                    '}' => return false,
                    _ if !next.is_ascii() => {
                        pending_quantifier = false;
                        idx += 2;
                    }
                    _ => return false,
                }
            }
            _ => {
                pending_quantifier = false;
                idx += 1;
            }
        }
    }

    !in_class
}

struct BackrefParser<'a> {
    chars: Vec<char>,
    idx: usize,
    group_count: usize,
    _source: &'a str,
}

impl<'a> BackrefParser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            chars: source.chars().collect(),
            idx: 0,
            group_count: 0,
            _source: source,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.idx).copied()
    }

    fn peek_n(&self, n: usize) -> Option<char> {
        self.chars.get(self.idx + n).copied()
    }

    fn next(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.idx += 1;
        Some(ch)
    }

    fn parse(mut self) -> Option<BackrefPattern> {
        let expr = self.parse_expr(false)?;
        if self.idx != self.chars.len() {
            return None;
        }
        Some(BackrefPattern {
            expr,
            group_count: self.group_count,
        })
    }

    fn parse_expr(&mut self, in_group: bool) -> Option<BackrefExpr> {
        let mut branches = Vec::new();
        loop {
            branches.push(self.parse_branch(in_group)?);
            if self.peek() == Some('\\') && self.peek_n(1) == Some('|') {
                self.idx += 2;
                continue;
            }
            break;
        }
        Some(BackrefExpr { branches })
    }

    fn parse_branch(&mut self, in_group: bool) -> Option<Vec<BackrefNode>> {
        let mut nodes = Vec::new();
        loop {
            if self.idx >= self.chars.len() {
                break;
            }
            if in_group && self.peek() == Some('\\') && self.peek_n(1) == Some(')') {
                break;
            }
            if self.peek() == Some('\\') && self.peek_n(1) == Some('|') {
                break;
            }
            let atom = self.parse_atom()?;
            let repeat = self.parse_quantifier()?;
            nodes.push(BackrefNode { atom, repeat });
        }
        Some(nodes)
    }

    fn parse_atom(&mut self) -> Option<BackrefAtom> {
        let ch = self.next()?;
        match ch {
            '^' => Some(BackrefAtom::LineStart),
            '$' => Some(BackrefAtom::LineEnd),
            '.' => Some(BackrefAtom::AnyChar),
            '[' => self.parse_char_class(),
            '\\' => {
                let next = self.next()?;
                match next {
                    '(' => {
                        let mut noncapturing = false;
                        let mut group_index = None;
                        if self.peek() == Some('?') {
                            if self.peek_n(1) == Some(':') {
                                noncapturing = true;
                                self.idx += 2;
                            } else if let Some(explicit_index) = self.parse_explicit_group_index() {
                                self.group_count = self.group_count.max(explicit_index);
                                group_index = Some(explicit_index);
                            } else {
                                return None;
                            }
                        } else {
                            self.group_count += 1;
                            group_index = Some(self.group_count);
                        }
                        let expr = self.parse_expr(true)?;
                        if self.next() != Some('\\') || self.next() != Some(')') {
                            return None;
                        }
                        if noncapturing {
                            Some(BackrefAtom::NonCapturing(expr))
                        } else {
                            Some(BackrefAtom::Group(group_index?, expr))
                        }
                    }
                    '1'..='9' => Some(BackrefAtom::Backref(next.to_digit(10)? as usize)),
                    'c' => {
                        self.next()?;
                        Some(BackrefAtom::NonAsciiCategory)
                    }
                    'd' => Some(BackrefAtom::Digit),
                    'D' => Some(BackrefAtom::NotDigit),
                    'w' => Some(BackrefAtom::WordChar),
                    'W' => Some(BackrefAtom::NotWordChar),
                    's' => Some(BackrefAtom::SyntaxClass(
                        map_syntax_class(self.next()?),
                        false,
                    )),
                    'S' => Some(BackrefAtom::SyntaxClass(
                        map_syntax_class(self.next()?),
                        true,
                    )),
                    '_' => match self.next()? {
                        '<' => Some(BackrefAtom::SymbolStart),
                        '>' => Some(BackrefAtom::SymbolEnd),
                        _ => None,
                    },
                    'n' => Some(BackrefAtom::Literal('\n')),
                    't' => Some(BackrefAtom::Literal('\t')),
                    'r' => Some(BackrefAtom::Literal('\r')),
                    'b' => Some(BackrefAtom::WordBoundary),
                    'B' => Some(BackrefAtom::NotWordBoundary),
                    '=' => Some(BackrefAtom::StartPoint),
                    '`' => Some(BackrefAtom::StartBuffer),
                    '\'' => Some(BackrefAtom::EndBuffer),
                    '<' | '>' => Some(BackrefAtom::WordBoundary),
                    '\\' | '.' | '*' | '+' | '?' | '[' | ']' | '^' | '$' | '{' | '}' | '|'
                    | ')' => Some(BackrefAtom::Literal(next)),
                    _ if !next.is_ascii() => Some(BackrefAtom::Literal(next)),
                    _ => None,
                }
            }
            _ => Some(BackrefAtom::Literal(ch)),
        }
    }

    fn parse_char_class(&mut self) -> Option<BackrefAtom> {
        let mut negated = false;
        if self.peek() == Some('^') {
            self.idx += 1;
            negated = true;
        }

        let mut items = Vec::new();
        let mut first = true;
        while let Some(ch) = self.peek() {
            if ch == ']' && !first {
                self.idx += 1;
                return Some(BackrefAtom::CharClass(BackrefCharClass { negated, items }));
            }
            first = false;

            if ch == '\\' && self.peek_n(1) == Some(']') {
                self.idx += 1;
                items.push(BackrefCharClassItem::Literal('\\'));
                continue;
            }

            if ch == '[' && self.peek_n(1) == Some(':') {
                self.idx += 2;
                let start = self.idx;
                while self.peek() != Some(':') {
                    self.idx += 1;
                    if self.idx >= self.chars.len() {
                        return None;
                    }
                }
                let name: String = self.chars[start..self.idx].iter().collect();
                if self.peek() != Some(':') || self.peek_n(1) != Some(']') {
                    return None;
                }
                self.idx += 2;
                let posix = match name.as_str() {
                    "alpha" => BackrefPosixClass::Alpha,
                    "alnum" => BackrefPosixClass::Alnum,
                    "digit" => BackrefPosixClass::Digit,
                    "space" => BackrefPosixClass::Space,
                    "upper" => BackrefPosixClass::Upper,
                    "lower" => BackrefPosixClass::Lower,
                    "punct" => BackrefPosixClass::Punct,
                    "blank" => BackrefPosixClass::Blank,
                    _ => return None,
                };
                items.push(BackrefCharClassItem::Posix(posix));
                continue;
            }

            let (start_item, start_literal) = self.parse_char_class_item()?;

            if let Some(start_ch) = start_literal {
                if self.peek() == Some('-') && self.peek_n(1) != Some(']') {
                    self.idx += 1;
                    let (end_item, end_literal) = self.parse_char_class_item()?;
                    let end_ch = end_literal?;
                    if !matches!(end_item, BackrefCharClassItem::Literal(_)) {
                        return None;
                    }
                    items.push(BackrefCharClassItem::Range(start_ch, end_ch));
                    continue;
                }
                items.push(start_item);
            } else {
                items.push(start_item);
            }
        }

        None
    }

    fn parse_char_class_item(&mut self) -> Option<(BackrefCharClassItem, Option<char>)> {
        let ch = self.next()?;
        if ch != '\\' {
            return Some((BackrefCharClassItem::Literal(ch), Some(ch)));
        }

        let next = self.next()?;
        match next {
            'c' => {
                self.next()?;
                Some((BackrefCharClassItem::NonAsciiCategory, None))
            }
            'd' => Some((BackrefCharClassItem::Digit, None)),
            'D' => Some((BackrefCharClassItem::NotDigit, None)),
            'w' => Some((BackrefCharClassItem::WordChar, None)),
            'W' => Some((BackrefCharClassItem::NotWordChar, None)),
            's' => Some((
                BackrefCharClassItem::SyntaxClass(map_syntax_class(self.next()?), false),
                None,
            )),
            'S' => Some((
                BackrefCharClassItem::SyntaxClass(map_syntax_class(self.next()?), true),
                None,
            )),
            'n' => Some((BackrefCharClassItem::Literal('\n'), Some('\n'))),
            't' => Some((BackrefCharClassItem::Literal('\t'), Some('\t'))),
            'r' => Some((BackrefCharClassItem::Literal('\r'), Some('\r'))),
            _ => Some((BackrefCharClassItem::Literal(next), Some(next))),
        }
    }

    fn parse_quantifier(&mut self) -> Option<Quantifier> {
        let mut quantifier = match self.peek() {
            Some('*') => {
                self.idx += 1;
                Some(Quantifier {
                    min: 0,
                    max: None,
                    greedy: true,
                })
            }
            Some('+') => {
                self.idx += 1;
                Some(Quantifier {
                    min: 1,
                    max: None,
                    greedy: true,
                })
            }
            Some('?') => {
                self.idx += 1;
                Some(Quantifier {
                    min: 0,
                    max: Some(1),
                    greedy: true,
                })
            }
            Some('\\') if self.peek_n(1) == Some('{') => {
                let saved = self.idx;
                self.idx += 2;
                let min = if self.peek() == Some(',') {
                    0
                } else {
                    self.parse_usize()?
                };
                let max = if self.peek() == Some(',') {
                    self.idx += 1;
                    if self.peek() == Some('\\') && self.peek_n(1) == Some('}') {
                        None
                    } else {
                        Some(self.parse_usize()?)
                    }
                } else {
                    Some(min)
                };
                if self.peek() != Some('\\') || self.peek_n(1) != Some('}') {
                    self.idx = saved;
                    return Some(Quantifier::ONE);
                }
                self.idx += 2;
                Some(Quantifier {
                    min,
                    max,
                    greedy: true,
                })
            }
            _ => Some(Quantifier::ONE),
        }?;

        if quantifier != Quantifier::ONE && self.peek() == Some('?') {
            self.idx += 1;
            quantifier.greedy = false;
        }

        Some(quantifier)
    }

    fn parse_usize(&mut self) -> Option<usize> {
        let start = self.idx;
        while matches!(self.peek(), Some('0'..='9')) {
            self.idx += 1;
        }
        if self.idx == start {
            return None;
        }
        self.chars[start..self.idx]
            .iter()
            .collect::<String>()
            .parse()
            .ok()
    }

    fn parse_explicit_group_index(&mut self) -> Option<usize> {
        if self.peek() != Some('?') {
            return None;
        }

        let saved = self.idx;
        self.idx += 1;
        let number = self.parse_usize()?;
        if self.peek() != Some(':') {
            self.idx = saved;
            return None;
        }
        self.idx += 1;
        Some(number)
    }
}

fn compile_emacs_regex_case_fold(pattern: &str, case_fold: bool) -> Result<Regex, String> {
    let rust_pattern = translate_emacs_regex(pattern);
    // Emacs regexes always treat ^ and $ as matching at line boundaries,
    // which corresponds to Rust regex's multiline (?m) flag.
    let wrapped = if case_fold {
        format!("(?mi:{})", rust_pattern)
    } else {
        format!("(?m:{})", rust_pattern)
    };
    Regex::new(&wrapped).map_err(|e| format!("Invalid regexp: {}", e))
}

fn compile_search_pattern(pattern: &str, case_fold: bool) -> Result<CompiledSearchPattern, String> {
    if let Some(cached) = crate::emacs_core::perf_trace::time_op(
        crate::emacs_core::perf_trace::HotpathOp::RegexCompileHit,
        || {
            SEARCH_PATTERN_CACHE.with(|cache| {
                let mut cache = cache.borrow_mut();
                let index = cache
                    .iter()
                    .position(|(cached_case_fold, cached_pattern, _)| {
                        *cached_case_fold == case_fold && cached_pattern == pattern
                    })?;
                let entry = cache.remove(index);
                cache.insert(0, entry.clone());
                Some(entry.2)
            })
        },
    ) {
        return Ok(cached);
    }

    let compiled = crate::emacs_core::perf_trace::time_op(
        crate::emacs_core::perf_trace::HotpathOp::RegexCompileMiss,
        || {
            if let Some(segmented) = parse_segmented_pattern(pattern) {
                Ok(CompiledSearchPattern::Segmented(segmented))
            } else if let Some(literal) = literal_from_trivial_regexp(pattern)
                && (!case_fold || literal.is_ascii())
            {
                Ok(CompiledSearchPattern::Literal(literal))
            } else if pattern_supported_by_backref_engine(pattern) {
                if let Some(backref) = BackrefParser::new(pattern).parse() {
                    Ok(CompiledSearchPattern::Backref(backref))
                } else {
                    compile_emacs_regex_case_fold(pattern, case_fold)
                        .map(CompiledSearchPattern::Regex)
                }
            } else {
                compile_emacs_regex_case_fold(pattern, case_fold).map(CompiledSearchPattern::Regex)
            }
        },
    )?;

    SEARCH_PATTERN_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.insert(0, (case_fold, pattern.to_string(), compiled.clone()));
        if cache.len() > SEARCH_PATTERN_CACHE_SIZE {
            cache.truncate(SEARCH_PATTERN_CACHE_SIZE);
        }
    });

    Ok(compiled)
}

fn compiled_capture_count(compiled: &CompiledSearchPattern) -> usize {
    match compiled {
        CompiledSearchPattern::Literal(_) => 1,
        CompiledSearchPattern::Segmented(pattern) => pattern.capture_count + 1,
        CompiledSearchPattern::Backref(pattern) => pattern.group_count + 1,
        CompiledSearchPattern::Regex(re) => re.captures_len(),
    }
}

fn find_forward_match_data_compiled(
    compiled: &CompiledSearchPattern,
    text: &str,
    start: usize,
    limit: usize,
    offset: usize,
    case_fold: bool,
) -> Option<MatchData> {
    match compiled {
        CompiledSearchPattern::Literal(literal) => {
            let (match_start, match_end) = literal_find(&text[start..limit], literal, case_fold)?;
            Some(MatchData {
                groups: vec![Some((
                    offset + start + match_start,
                    offset + start + match_end,
                ))],
                searched_string: None,
                searched_buffer: None,
            })
        }
        CompiledSearchPattern::Segmented(pattern) => {
            find_forward_segmented_match_data(pattern, text, start, limit, offset, case_fold)
        }
        CompiledSearchPattern::Backref(pattern) => {
            find_forward_backref_match_data(pattern, text, start, limit, offset, case_fold)
        }
        CompiledSearchPattern::Regex(re) => find_forward_match_data(re, text, start, limit, offset),
    }
}

pub(crate) fn iterate_string_matches_with_case_fold(
    pattern: &str,
    string: &str,
    start: usize,
    case_fold: bool,
) -> Result<IteratedStringMatches, String> {
    let compiled = compile_search_pattern(pattern, case_fold)?;
    let capture_count = compiled_capture_count(&compiled);
    if start > string.len() {
        return Ok(IteratedStringMatches {
            capture_count,
            matches: Vec::new(),
        });
    }
    let mut matches = Vec::new();
    let mut search_at = start;

    while search_at <= string.len() {
        let Some(md) = find_forward_match_data_compiled(
            &compiled,
            string,
            search_at,
            string.len(),
            0,
            case_fold,
        ) else {
            break;
        };
        let Some((match_start, match_end)) = md.groups.first().and_then(|group| *group) else {
            break;
        };
        matches.push(md.groups);

        if match_end > search_at {
            search_at = match_end;
            continue;
        }

        let Some(next_at) = next_search_char_boundary(string, match_end) else {
            break;
        };
        if next_at <= search_at {
            break;
        }
        search_at = next_at;
        if match_start == match_end && search_at > string.len() {
            break;
        }
    }

    Ok(IteratedStringMatches {
        capture_count,
        matches,
    })
}

fn match_data_from_captures(caps: &regex::Captures<'_>, offset: usize) -> MatchData {
    let mut groups = Vec::with_capacity(caps.len());
    for i in 0..caps.len() {
        groups.push(caps.get(i).map(|m| (m.start() + offset, m.end() + offset)));
    }
    MatchData {
        groups,
        searched_string: None,
        searched_buffer: None,
    }
}

fn string_char_match_data(searched_string: SearchedString, byte_md: MatchData) -> MatchData {
    crate::emacs_core::perf_trace::time_op(
        crate::emacs_core::perf_trace::HotpathOp::RegexMatchDataChars,
        || {
            let char_groups = searched_string.with_str(|string| {
                if string.is_ascii() {
                    return byte_md.groups.clone();
                }

                byte_md
                    .groups
                    .iter()
                    .map(|g| {
                        g.map(|(bs, be)| {
                            let cs = string.get(..bs).map_or(0, |s| s.chars().count());
                            let ce = string.get(..be).map_or(0, |s| s.chars().count());
                            (cs, ce)
                        })
                    })
                    .collect()
            });

            MatchData {
                groups: char_groups,
                searched_string: Some(searched_string),
                searched_buffer: None,
            }
        },
    )
}

fn single_group_match_data(start: usize, end: usize) -> MatchData {
    MatchData {
        groups: vec![Some((start, end))],
        searched_string: None,
        searched_buffer: None,
    }
}

fn segmented_capture_accepts(
    capture: SegmentedCapture,
    text: &str,
    start: usize,
    end: usize,
    case_fold: bool,
) -> bool {
    match capture {
        SegmentedCapture::AnyLazy => true,
        SegmentedCapture::NegatedCharPlus(forbidden) => {
            if start == end {
                return false;
            }
            let slice = &text[start..end];
            if slice.is_ascii() && forbidden.is_ascii() {
                let forbidden = forbidden as u8;
                if case_fold {
                    return !slice
                        .as_bytes()
                        .iter()
                        .any(|b| b.eq_ignore_ascii_case(&forbidden));
                }
                return !slice.as_bytes().contains(&forbidden);
            }
            slice
                .chars()
                .all(|ch| !char_eq_case_fold(ch, forbidden, case_fold))
        }
    }
}

fn segmented_next_literal(pattern: &SegmentedPattern, from_index: usize) -> Option<&str> {
    pattern.segments[from_index + 1..]
        .iter()
        .find_map(|part| match part {
            SegmentedPatternPart::Literal(literal) => Some(literal.as_str()),
            SegmentedPatternPart::Capture(_) => None,
        })
}

fn segmented_match_at(
    pattern: &SegmentedPattern,
    text: &str,
    start: usize,
    limit: usize,
    case_fold: bool,
) -> Option<MatchData> {
    let mut pos = start;
    let mut groups = Vec::with_capacity(pattern.capture_count + 1);
    groups.push(None);
    let mut capture_index = 1usize;

    for (idx, part) in pattern.segments.iter().enumerate() {
        match part {
            SegmentedPatternPart::Literal(literal) => {
                pos = substring_starts_with(text, pos, literal, case_fold)?;
                if pos > limit {
                    return None;
                }
            }
            SegmentedPatternPart::Capture(capture) => {
                let next_literal = segmented_next_literal(pattern, idx)?;
                let mut search_from = pos;
                let capture_end = loop {
                    let (literal_start, _) =
                        literal_find(&text[search_from..limit], next_literal, case_fold)?;
                    let candidate_end = search_from + literal_start;
                    if segmented_capture_accepts(*capture, text, pos, candidate_end, case_fold) {
                        break candidate_end;
                    }
                    let Some((_, step)) = char_at(text, candidate_end) else {
                        return None;
                    };
                    search_from = candidate_end + step;
                };
                groups.push(Some((pos, capture_end)));
                capture_index += 1;
                pos = capture_end;
            }
        }
    }

    debug_assert_eq!(capture_index, pattern.capture_count + 1);
    groups[0] = Some((start, pos));
    Some(MatchData {
        groups,
        searched_string: None,
        searched_buffer: None,
    })
}

fn find_forward_segmented_match_data(
    pattern: &SegmentedPattern,
    text: &str,
    start: usize,
    limit: usize,
    offset: usize,
    case_fold: bool,
) -> Option<MatchData> {
    let first_literal = match pattern.segments.first()? {
        SegmentedPatternPart::Literal(literal) => literal,
        SegmentedPatternPart::Capture(_) => return None,
    };

    let mut search_from = start;
    while search_from <= limit {
        let (literal_start, _) = literal_find(&text[search_from..limit], first_literal, case_fold)?;
        let candidate = search_from + literal_start;
        if let Some(md) = segmented_match_at(pattern, text, candidate, limit, case_fold) {
            return Some(offset_match_data(md, offset));
        }
        let Some((_, step)) = char_at(text, candidate) else {
            break;
        };
        search_from = candidate + step;
    }
    None
}

fn find_backward_segmented_match_data(
    pattern: &SegmentedPattern,
    text: &str,
    start: usize,
    limit: usize,
    offset: usize,
    case_fold: bool,
) -> Option<MatchData> {
    let first_literal = match pattern.segments.first()? {
        SegmentedPatternPart::Literal(literal) => literal,
        SegmentedPatternPart::Capture(_) => return None,
    };

    let mut window_end = start;
    while window_end >= limit {
        let (literal_start, _) = literal_rfind(&text[limit..window_end], first_literal, case_fold)?;
        let candidate = limit + literal_start;
        if let Some(md) = segmented_match_at(pattern, text, candidate, start, case_fold) {
            return Some(offset_match_data(md, offset));
        }
        if candidate == 0 {
            break;
        }
        window_end = candidate;
    }
    None
}

fn ascii_case_fold_find(haystack: &str, needle: &str) -> Option<usize> {
    let needle_len = needle.len();
    if needle_len == 0 {
        return Some(0);
    }
    let haystack_bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();
    if needle_len > haystack_bytes.len() {
        return None;
    }

    haystack_bytes.windows(needle_len).position(|window| {
        window
            .iter()
            .zip(needle_bytes.iter())
            .all(|(lhs, rhs)| lhs.eq_ignore_ascii_case(rhs))
    })
}

fn ascii_case_fold_rfind(haystack: &str, needle: &str) -> Option<usize> {
    let needle_len = needle.len();
    if needle_len == 0 {
        return Some(haystack.len());
    }
    let haystack_bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();
    if needle_len > haystack_bytes.len() {
        return None;
    }

    haystack_bytes.windows(needle_len).rposition(|window| {
        window
            .iter()
            .zip(needle_bytes.iter())
            .all(|(lhs, rhs)| lhs.eq_ignore_ascii_case(rhs))
    })
}

fn build_unicode_folded_literal_index(text: &str) -> (Vec<char>, Vec<(usize, usize)>) {
    let mut folded = Vec::new();
    let mut mapping = Vec::new();
    for (byte_start, ch) in text.char_indices() {
        let byte_end = byte_start + ch.len_utf8();
        for folded_ch in ch.to_lowercase() {
            folded.push(folded_ch);
            mapping.push((byte_start, byte_end));
        }
    }
    (folded, mapping)
}

fn unicode_case_fold_literal_find(text: &str, literal: &str) -> Option<(usize, usize)> {
    let needle: Vec<char> = literal.chars().flat_map(|ch| ch.to_lowercase()).collect();
    if needle.is_empty() {
        return Some((0, 0));
    }
    let (haystack, mapping) = build_unicode_folded_literal_index(text);
    let start_char = haystack
        .windows(needle.len())
        .position(|window| window == needle.as_slice())?;
    let end_char = start_char + needle.len() - 1;
    Some((mapping[start_char].0, mapping[end_char].1))
}

fn unicode_case_fold_literal_rfind(text: &str, literal: &str) -> Option<(usize, usize)> {
    let needle: Vec<char> = literal.chars().flat_map(|ch| ch.to_lowercase()).collect();
    if needle.is_empty() {
        return Some((text.len(), text.len()));
    }
    let (haystack, mapping) = build_unicode_folded_literal_index(text);
    let start_char = haystack
        .windows(needle.len())
        .rposition(|window| window == needle.as_slice())?;
    let end_char = start_char + needle.len() - 1;
    Some((mapping[start_char].0, mapping[end_char].1))
}

fn literal_find(text: &str, literal: &str, case_fold: bool) -> Option<(usize, usize)> {
    crate::emacs_core::perf_trace::time_op(
        crate::emacs_core::perf_trace::HotpathOp::RegexLiteralFind,
        || {
            if case_fold && (!literal.is_ascii() || !text.is_ascii()) {
                return unicode_case_fold_literal_find(text, literal);
            }
            let start = if case_fold {
                ascii_case_fold_find(text, literal)?
            } else {
                text.find(literal)?
            };
            Some((start, start + literal.len()))
        },
    )
}

fn build_kmp_table(needle: &[u8]) -> Vec<usize> {
    let mut lps = vec![0; needle.len()];
    let mut len = 0;
    let mut i = 1;
    while i < needle.len() {
        if needle[i] == needle[len] {
            len += 1;
            lps[i] = len;
            i += 1;
        } else if len != 0 {
            len = lps[len - 1];
        } else {
            lps[i] = 0;
            i += 1;
        }
    }
    lps
}

fn literal_find_lisp_string(
    text: &crate::gc::types::LispString,
    literal: &str,
    start: usize,
    case_fold: bool,
) -> Option<(usize, usize)> {
    crate::emacs_core::perf_trace::time_op(
        crate::emacs_core::perf_trace::HotpathOp::RegexLiteralFind,
        || {
            if start > text.byte_len() {
                return None;
            }

            if case_fold && (!literal.is_ascii() || !text.is_ascii()) {
                return literal_find(&text.as_str()[start..], literal, case_fold)
                    .map(|(match_start, match_end)| (start + match_start, start + match_end));
            }

            let mut parts = Vec::new();
            text.append_parts_to(&mut parts);
            if parts.len() <= 1 {
                return literal_find(&text.as_str()[start..], literal, case_fold)
                    .map(|(match_start, match_end)| (start + match_start, start + match_end));
            }

            let needle: Vec<u8> = if case_fold {
                literal
                    .as_bytes()
                    .iter()
                    .map(|byte| byte.to_ascii_lowercase())
                    .collect()
            } else {
                literal.as_bytes().to_vec()
            };
            if needle.is_empty() {
                return Some((start, start));
            }

            let lps = build_kmp_table(&needle);
            let mut matched = 0usize;
            let mut global = 0usize;

            for part in parts {
                let bytes = part.as_str().as_bytes();
                let skip = start.saturating_sub(global).min(bytes.len());
                for (offset, byte) in bytes.iter().enumerate().skip(skip) {
                    let hay = if case_fold {
                        byte.to_ascii_lowercase()
                    } else {
                        *byte
                    };
                    while matched > 0 && hay != needle[matched] {
                        matched = lps[matched - 1];
                    }
                    if hay == needle[matched] {
                        matched += 1;
                        if matched == needle.len() {
                            let match_end = global + offset + 1;
                            return Some((match_end - needle.len(), match_end));
                        }
                    }
                }
                global += bytes.len();
            }

            None
        },
    )
}

fn literal_rfind(text: &str, literal: &str, case_fold: bool) -> Option<(usize, usize)> {
    crate::emacs_core::perf_trace::time_op(
        crate::emacs_core::perf_trace::HotpathOp::RegexLiteralFind,
        || {
            if case_fold && (!literal.is_ascii() || !text.is_ascii()) {
                return unicode_case_fold_literal_rfind(text, literal);
            }
            let start = if case_fold {
                ascii_case_fold_rfind(text, literal)?
            } else {
                text.rfind(literal)?
            };
            Some((start, start + literal.len()))
        },
    )
}

fn char_at(text: &str, pos: usize) -> Option<(char, usize)> {
    text.get(pos..)
        .and_then(|tail| tail.chars().next().map(|ch| (ch, ch.len_utf8())))
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn matches_posix_class(ch: char, class: BackrefPosixClass, case_fold: bool) -> bool {
    match class {
        BackrefPosixClass::Alpha => ch.is_alphabetic(),
        BackrefPosixClass::Alnum => ch.is_alphanumeric(),
        BackrefPosixClass::Digit => ch.is_ascii_digit(),
        BackrefPosixClass::Space => ch.is_whitespace(),
        BackrefPosixClass::Upper | BackrefPosixClass::Lower if case_fold => ch.is_alphabetic(),
        BackrefPosixClass::Upper => ch.is_uppercase(),
        BackrefPosixClass::Lower => ch.is_lowercase(),
        BackrefPosixClass::Punct => ch.is_ascii_punctuation(),
        BackrefPosixClass::Blank => matches!(ch, ' ' | '\t'),
    }
}

fn map_syntax_class(ch: char) -> BackrefSyntaxClass {
    match ch {
        '-' | ' ' | '\'' | '<' | '>' | '!' | '|' | '/' => BackrefSyntaxClass::Whitespace,
        'w' => BackrefSyntaxClass::Word,
        '_' => BackrefSyntaxClass::Symbol,
        '.' => BackrefSyntaxClass::Punct,
        '(' => BackrefSyntaxClass::OpenDelim,
        ')' => BackrefSyntaxClass::CloseDelim,
        '"' => BackrefSyntaxClass::StringQuote,
        _ => BackrefSyntaxClass::Whitespace,
    }
}

fn matches_syntax_class(ch: char, class: BackrefSyntaxClass) -> bool {
    match class {
        BackrefSyntaxClass::Whitespace => ch.is_whitespace(),
        BackrefSyntaxClass::Word => is_word_char(ch),
        BackrefSyntaxClass::Symbol => is_word_char(ch) || ch == '_',
        BackrefSyntaxClass::Punct => ch.is_ascii_punctuation(),
        BackrefSyntaxClass::OpenDelim => matches!(ch, '[' | '(' | '{'),
        BackrefSyntaxClass::CloseDelim => matches!(ch, ']' | ')' | '}'),
        BackrefSyntaxClass::StringQuote => matches!(ch, '"' | '\''),
    }
}

fn char_class_matches(class: &BackrefCharClass, ch: char, case_fold: bool) -> bool {
    let matched = class.items.iter().any(|item| match item {
        BackrefCharClassItem::Literal(expected) => char_eq_case_fold(ch, *expected, case_fold),
        BackrefCharClassItem::Range(start, end) => {
            let normalized = if case_fold && ch.is_ascii() {
                ch.to_ascii_lowercase()
            } else {
                ch
            };
            let range_start = if case_fold && start.is_ascii() {
                start.to_ascii_lowercase()
            } else {
                *start
            };
            let range_end = if case_fold && end.is_ascii() {
                end.to_ascii_lowercase()
            } else {
                *end
            };
            range_start <= normalized && normalized <= range_end
        }
        BackrefCharClassItem::Posix(posix) => matches_posix_class(ch, *posix, case_fold),
        BackrefCharClassItem::NonAsciiCategory => !ch.is_ascii(),
        BackrefCharClassItem::Digit => ch.is_ascii_digit(),
        BackrefCharClassItem::NotDigit => !ch.is_ascii_digit(),
        BackrefCharClassItem::WordChar => is_word_char(ch),
        BackrefCharClassItem::NotWordChar => !is_word_char(ch),
        BackrefCharClassItem::SyntaxClass(class, negated) => {
            matches_syntax_class(ch, *class) != *negated
        }
    });
    if class.negated { !matched } else { matched }
}

fn char_eq_case_fold(left: char, right: char, case_fold: bool) -> bool {
    if !case_fold {
        return left == right;
    }
    if left.is_ascii() && right.is_ascii() {
        return left.eq_ignore_ascii_case(&right);
    }
    left.to_lowercase().to_string() == right.to_lowercase().to_string()
}

fn substring_starts_with(text: &str, pos: usize, needle: &str, case_fold: bool) -> Option<usize> {
    if text.is_ascii() && needle.is_ascii() {
        let hay = text.get(pos..)?.as_bytes();
        let needle = needle.as_bytes();
        if hay.len() < needle.len() {
            return None;
        }
        let matched = if case_fold {
            hay[..needle.len()]
                .iter()
                .zip(needle.iter())
                .all(|(lhs, rhs)| lhs.eq_ignore_ascii_case(rhs))
        } else {
            &hay[..needle.len()] == needle
        };
        return matched.then_some(pos + needle.len());
    }

    let mut hay_pos = pos;
    for needle_ch in needle.chars() {
        let (hay_ch, hay_len) = char_at(text, hay_pos)?;
        if !char_eq_case_fold(hay_ch, needle_ch, case_fold) {
            return None;
        }
        hay_pos += hay_len;
    }
    Some(hay_pos)
}

fn word_boundary_at(text: &str, pos: usize) -> bool {
    let left = text[..pos].chars().next_back().is_some_and(is_word_char);
    let right = char_at(text, pos).is_some_and(|(ch, _)| is_word_char(ch));
    left != right
}

fn is_symbol_char(ch: char) -> bool {
    matches_syntax_class(ch, BackrefSyntaxClass::Symbol)
}

fn symbol_start_at(text: &str, pos: usize) -> bool {
    let left = text[..pos].chars().next_back().is_some_and(is_symbol_char);
    let right = char_at(text, pos).is_some_and(|(ch, _)| is_symbol_char(ch));
    !left && right
}

fn symbol_end_at(text: &str, pos: usize) -> bool {
    let left = text[..pos].chars().next_back().is_some_and(is_symbol_char);
    let right = char_at(text, pos).is_some_and(|(ch, _)| is_symbol_char(ch));
    left && !right
}

fn line_start_at(text: &str, pos: usize) -> bool {
    pos == 0 || text[..pos].chars().next_back() == Some('\n')
}

fn line_end_at(text: &str, pos: usize) -> bool {
    pos == text.len() || char_at(text, pos).is_some_and(|(ch, _)| ch == '\n')
}

fn match_backref_atom_once(
    atom: &BackrefAtom,
    text: &str,
    state: &BackrefState,
    case_fold: bool,
) -> Vec<BackrefState> {
    match atom {
        BackrefAtom::Literal(expected) => {
            let Some((ch, len)) = char_at(text, state.pos) else {
                return Vec::new();
            };
            if char_eq_case_fold(ch, *expected, case_fold) {
                vec![BackrefState {
                    pos: state.pos + len,
                    search_start: state.search_start,
                    groups: state.groups.clone(),
                }]
            } else {
                Vec::new()
            }
        }
        BackrefAtom::AnyChar => {
            let Some((ch, len)) = char_at(text, state.pos) else {
                return Vec::new();
            };
            if ch == '\n' {
                Vec::new()
            } else {
                vec![BackrefState {
                    pos: state.pos + len,
                    search_start: state.search_start,
                    groups: state.groups.clone(),
                }]
            }
        }
        BackrefAtom::CharClass(class) => {
            let Some((ch, len)) = char_at(text, state.pos) else {
                return Vec::new();
            };
            if char_class_matches(class, ch, case_fold) {
                vec![BackrefState {
                    pos: state.pos + len,
                    search_start: state.search_start,
                    groups: state.groups.clone(),
                }]
            } else {
                Vec::new()
            }
        }
        BackrefAtom::NonAsciiCategory => {
            let Some((ch, len)) = char_at(text, state.pos) else {
                return Vec::new();
            };
            if !ch.is_ascii() {
                vec![BackrefState {
                    pos: state.pos + len,
                    search_start: state.search_start,
                    groups: state.groups.clone(),
                }]
            } else {
                Vec::new()
            }
        }
        BackrefAtom::Digit => {
            let Some((ch, len)) = char_at(text, state.pos) else {
                return Vec::new();
            };
            if ch.is_ascii_digit() {
                vec![BackrefState {
                    pos: state.pos + len,
                    search_start: state.search_start,
                    groups: state.groups.clone(),
                }]
            } else {
                Vec::new()
            }
        }
        BackrefAtom::NotDigit => {
            let Some((ch, len)) = char_at(text, state.pos) else {
                return Vec::new();
            };
            if !ch.is_ascii_digit() {
                vec![BackrefState {
                    pos: state.pos + len,
                    search_start: state.search_start,
                    groups: state.groups.clone(),
                }]
            } else {
                Vec::new()
            }
        }
        BackrefAtom::WordChar => {
            let Some((ch, len)) = char_at(text, state.pos) else {
                return Vec::new();
            };
            if is_word_char(ch) {
                vec![BackrefState {
                    pos: state.pos + len,
                    search_start: state.search_start,
                    groups: state.groups.clone(),
                }]
            } else {
                Vec::new()
            }
        }
        BackrefAtom::NotWordChar => {
            let Some((ch, len)) = char_at(text, state.pos) else {
                return Vec::new();
            };
            if !is_word_char(ch) {
                vec![BackrefState {
                    pos: state.pos + len,
                    search_start: state.search_start,
                    groups: state.groups.clone(),
                }]
            } else {
                Vec::new()
            }
        }
        BackrefAtom::SyntaxClass(class, negated) => {
            let Some((ch, len)) = char_at(text, state.pos) else {
                return Vec::new();
            };
            let matched = matches_syntax_class(ch, *class);
            if matched != *negated {
                vec![BackrefState {
                    pos: state.pos + len,
                    search_start: state.search_start,
                    groups: state.groups.clone(),
                }]
            } else {
                Vec::new()
            }
        }
        BackrefAtom::LineStart => line_start_at(text, state.pos)
            .then(|| state.clone())
            .into_iter()
            .collect(),
        BackrefAtom::LineEnd => line_end_at(text, state.pos)
            .then(|| state.clone())
            .into_iter()
            .collect(),
        BackrefAtom::WordBoundary => word_boundary_at(text, state.pos)
            .then(|| state.clone())
            .into_iter()
            .collect(),
        BackrefAtom::NotWordBoundary => (!word_boundary_at(text, state.pos))
            .then(|| state.clone())
            .into_iter()
            .collect(),
        BackrefAtom::SymbolStart => symbol_start_at(text, state.pos)
            .then(|| state.clone())
            .into_iter()
            .collect(),
        BackrefAtom::SymbolEnd => symbol_end_at(text, state.pos)
            .then(|| state.clone())
            .into_iter()
            .collect(),
        BackrefAtom::StartPoint => (state.pos == state.search_start)
            .then(|| state.clone())
            .into_iter()
            .collect(),
        BackrefAtom::StartBuffer => (state.pos == 0)
            .then(|| state.clone())
            .into_iter()
            .collect(),
        BackrefAtom::EndBuffer => (state.pos == text.len())
            .then(|| state.clone())
            .into_iter()
            .collect(),
        BackrefAtom::Group(index, expr) => {
            let start = state.pos;
            let mut out = Vec::new();
            for mut inner in match_backref_expr(expr, text, state.clone(), case_fold) {
                inner.groups[*index] = Some((start, inner.pos));
                out.push(inner);
            }
            out
        }
        BackrefAtom::NonCapturing(expr) => match_backref_expr(expr, text, state.clone(), case_fold),
        BackrefAtom::Backref(index) => {
            let Some(Some((group_start, group_end))) = state.groups.get(*index) else {
                return Vec::new();
            };
            let capture = &text[*group_start..*group_end];
            let Some(end_pos) = substring_starts_with(text, state.pos, capture, case_fold) else {
                return Vec::new();
            };
            vec![BackrefState {
                pos: end_pos,
                search_start: state.search_start,
                groups: state.groups.clone(),
            }]
        }
    }
}

fn match_backref_repetition(
    node: &BackrefNode,
    text: &str,
    state: BackrefState,
    case_fold: bool,
) -> Vec<BackrefState> {
    fn rec(
        node: &BackrefNode,
        text: &str,
        state: BackrefState,
        case_fold: bool,
        count: usize,
        out: &mut Vec<BackrefState>,
    ) {
        if !node.repeat.greedy && count >= node.repeat.min {
            out.push(state.clone());
        }

        let can_repeat_more = node.repeat.max.is_none_or(|max| count < max);
        if can_repeat_more {
            for next in match_backref_atom_once(&node.atom, text, &state, case_fold) {
                if next.pos == state.pos {
                    if count + 1 >= node.repeat.min {
                        out.push(next);
                    }
                    continue;
                }
                rec(node, text, next, case_fold, count + 1, out);
            }
        }
        if node.repeat.greedy && count >= node.repeat.min {
            out.push(state);
        }
    }

    let mut out = Vec::new();
    rec(node, text, state, case_fold, 0, &mut out);
    out
}

fn match_backref_sequence(
    nodes: &[BackrefNode],
    text: &str,
    initial: BackrefState,
    case_fold: bool,
) -> Vec<BackrefState> {
    let mut states = vec![initial];
    for node in nodes {
        let mut next_states = Vec::new();
        for state in states {
            next_states.extend(match_backref_repetition(node, text, state, case_fold));
        }
        if next_states.is_empty() {
            return next_states;
        }
        states = next_states;
    }
    states
}

fn match_backref_expr(
    expr: &BackrefExpr,
    text: &str,
    initial: BackrefState,
    case_fold: bool,
) -> Vec<BackrefState> {
    let mut out = Vec::new();
    for branch in &expr.branches {
        out.extend(match_backref_sequence(
            branch,
            text,
            initial.clone(),
            case_fold,
        ));
    }
    out
}

fn backref_match_at(
    pattern: &BackrefPattern,
    text: &str,
    start: usize,
    search_start: usize,
    case_fold: bool,
) -> Option<MatchData> {
    let initial = BackrefState {
        pos: start,
        search_start,
        groups: vec![None; pattern.group_count + 1],
    };
    let mut state = match_backref_expr(&pattern.expr, text, initial, case_fold)
        .into_iter()
        .next()?;
    state.groups[0] = Some((start, state.pos));
    Some(MatchData {
        groups: state.groups,
        searched_string: None,
        searched_buffer: None,
    })
}

fn offset_match_data(mut md: MatchData, offset: usize) -> MatchData {
    for group in &mut md.groups {
        if let Some((start, end)) = group {
            *start += offset;
            *end += offset;
        }
    }
    md
}

fn find_forward_backref_match_data(
    pattern: &BackrefPattern,
    text: &str,
    start: usize,
    limit: usize,
    offset: usize,
    case_fold: bool,
) -> Option<MatchData> {
    let mut search_at = start;
    while search_at <= limit {
        if let Some(md) = backref_match_at(pattern, text, search_at, start, case_fold) {
            let full_match = md.groups[0]?;
            if full_match.1 <= limit {
                return Some(offset_match_data(md, offset));
            }
        }
        let next_at = next_search_char_boundary(text, search_at)?;
        if next_at <= search_at {
            return None;
        }
        search_at = next_at;
    }
    None
}

fn find_backward_backref_match_data(
    pattern: &BackrefPattern,
    text: &str,
    start: usize,
    limit: usize,
    offset: usize,
    case_fold: bool,
) -> Option<MatchData> {
    let mut search_at = limit;
    let mut last = None;
    while search_at <= start {
        if let Some(md) = backref_match_at(pattern, text, search_at, start, case_fold) {
            let full_match = md.groups[0]?;
            if full_match.1 <= start {
                last = Some(offset_match_data(md, offset));
            }
        }
        let Some(next_at) = next_search_char_boundary(text, search_at) else {
            break;
        };
        if next_at <= search_at {
            break;
        }
        search_at = next_at;
    }
    last
}

fn next_search_char_boundary(text: &str, pos: usize) -> Option<usize> {
    if pos >= text.len() {
        return None;
    }
    text[pos..].chars().next().map(|ch| pos + ch.len_utf8())
}

fn find_forward_match_data(
    re: &Regex,
    text: &str,
    start: usize,
    limit: usize,
    offset: usize,
) -> Option<MatchData> {
    let mut search_at = start;
    while search_at <= limit {
        let caps = re.captures_at(text, search_at)?;
        let full_match = caps.get(0)?;
        if full_match.start() > limit {
            return None;
        }
        if full_match.end() <= limit {
            return Some(match_data_from_captures(&caps, offset));
        }
        let Some(next_at) = next_search_char_boundary(text, full_match.start()) else {
            return None;
        };
        if next_at <= search_at {
            return None;
        }
        search_at = next_at;
    }
    None
}

fn find_backward_match_data(
    re: &Regex,
    text: &str,
    start: usize,
    limit: usize,
    offset: usize,
) -> Option<MatchData> {
    let mut search_at = limit;
    let mut last = None;

    while search_at <= start {
        let Some(caps) = re.captures_at(text, search_at) else {
            break;
        };
        let Some(full_match) = caps.get(0) else {
            break;
        };
        if full_match.start() > start {
            break;
        }
        if full_match.end() <= start {
            last = Some(match_data_from_captures(&caps, offset));
        }
        let Some(next_at) = next_search_char_boundary(text, full_match.start()) else {
            break;
        };
        if next_at <= search_at {
            break;
        }
        search_at = next_at;
    }

    last
}

// ---------------------------------------------------------------------------
// Buffer search primitives
// ---------------------------------------------------------------------------

/// Search forward from point for a literal string PATTERN.
///
/// If found, moves point to end of match and returns the new point position
/// (as a byte position).  If not found, behaviour depends on `noerror`:
/// - `noerror` false: signals `search-failed`
/// - `noerror` true: returns `None` without signaling
///
/// `bound` optionally limits the search to positions <= bound.
pub fn search_forward(
    buf: &mut Buffer,
    pattern: &str,
    bound: Option<usize>,
    noerror: bool,
    case_fold: bool,
    match_data: &mut Option<MatchData>,
) -> Result<Option<usize>, String> {
    let start = buf.pt;
    let limit = bound.unwrap_or(buf.zv).min(buf.zv);

    if start > limit {
        if noerror {
            return Ok(None);
        }
        return Err(format!("Search failed: \"{}\"", pattern));
    }

    let text = buf.text.text_range(start, limit);

    let found = literal_find(&text, pattern, case_fold);

    if let Some((rel_start, rel_end)) = found {
        let match_start = start + rel_start;
        let match_end = start + rel_end;
        buf.goto_char(match_end);
        *match_data = Some(MatchData {
            groups: vec![Some((match_start, match_end))],
            searched_string: None,
            searched_buffer: Some(buf.id),
        });
        Ok(Some(match_end))
    } else if noerror {
        // When noerror is t, don't move point.
        // When noerror is a value, move point to bound.
        Ok(None)
    } else {
        Err(format!("Search failed: \"{}\"", pattern))
    }
}

/// Search backward from point for a literal string PATTERN.
///
/// If found, moves point to beginning of match and returns the new point
/// position (as a byte position).
pub fn search_backward(
    buf: &mut Buffer,
    pattern: &str,
    bound: Option<usize>,
    noerror: bool,
    case_fold: bool,
    match_data: &mut Option<MatchData>,
) -> Result<Option<usize>, String> {
    let end = buf.pt;
    let limit = bound.unwrap_or(buf.begv).max(buf.begv);

    if end < limit {
        if noerror {
            return Ok(None);
        }
        return Err(format!("Search failed: \"{}\"", pattern));
    }

    let text = buf.text.text_range(limit, end);

    let found = literal_rfind(&text, pattern, case_fold);

    if let Some((rel_start, rel_end)) = found {
        let match_start = limit + rel_start;
        let match_end = limit + rel_end;
        buf.goto_char(match_start);
        *match_data = Some(MatchData {
            groups: vec![Some((match_start, match_end))],
            searched_string: None,
            searched_buffer: Some(buf.id),
        });
        Ok(Some(match_start))
    } else if noerror {
        Ok(None)
    } else {
        Err(format!("Search failed: \"{}\"", pattern))
    }
}

/// Search forward from point for a regex PATTERN.
///
/// If found, moves point to end of match and returns the new point position.
/// Updates match data with capture groups.
pub fn re_search_forward(
    buf: &mut Buffer,
    pattern: &str,
    bound: Option<usize>,
    noerror: bool,
    case_fold: bool,
    match_data: &mut Option<MatchData>,
) -> Result<Option<usize>, String> {
    let start = buf.pt;
    let limit = bound.unwrap_or(buf.zv).min(buf.zv);

    if start > limit {
        if noerror {
            return Ok(None);
        }
        return Err(format!("Search failed: \"{}\"", pattern));
    }

    let region_start = buf.begv;
    let text = buf.text.text_range(region_start, buf.zv);
    let start_rel = start - region_start;
    let limit_rel = limit - region_start;

    match compile_search_pattern(pattern, case_fold)? {
        CompiledSearchPattern::Literal(literal) => {
            if let Some((rel_start, rel_end)) =
                literal_find(&text[start_rel..limit_rel], &literal, case_fold)
            {
                let full_match = (start + rel_start, start + rel_end);
                buf.goto_char(full_match.1);
                *match_data = Some(MatchData {
                    groups: vec![Some(full_match)],
                    searched_string: None,
                    searched_buffer: Some(buf.id),
                });
                Ok(Some(full_match.1))
            } else if noerror {
                Ok(None)
            } else {
                Err(format!("Search failed: \"{}\"", pattern))
            }
        }
        CompiledSearchPattern::Segmented(segmented) => {
            if let Some(mut md) = find_forward_segmented_match_data(
                &segmented,
                &text,
                start_rel,
                limit_rel,
                region_start,
                case_fold,
            ) {
                md.searched_string = None;
                md.searched_buffer = Some(buf.id);
                let full_match = md.groups[0].unwrap();
                buf.goto_char(full_match.1);
                *match_data = Some(md);
                Ok(Some(full_match.1))
            } else if noerror {
                Ok(None)
            } else {
                Err(format!("Search failed: \"{}\"", pattern))
            }
        }
        CompiledSearchPattern::Backref(backref) => {
            if let Some(mut md) = find_forward_backref_match_data(
                &backref,
                &text,
                start_rel,
                limit_rel,
                region_start,
                case_fold,
            ) {
                md.searched_string = None;
                md.searched_buffer = Some(buf.id);
                let full_match = md.groups[0].unwrap();
                buf.goto_char(full_match.1);
                *match_data = Some(md);
                Ok(Some(full_match.1))
            } else if noerror {
                Ok(None)
            } else {
                Err(format!("Search failed: \"{}\"", pattern))
            }
        }
        CompiledSearchPattern::Regex(re) => {
            if let Some(mut md) =
                find_forward_match_data(&re, &text, start_rel, limit_rel, region_start)
            {
                md.searched_string = None;
                md.searched_buffer = Some(buf.id);
                let full_match = md.groups[0].unwrap();
                buf.goto_char(full_match.1);
                *match_data = Some(md);
                Ok(Some(full_match.1))
            } else if noerror {
                Ok(None)
            } else {
                Err(format!("Search failed: \"{}\"", pattern))
            }
        }
    }
}

/// Search backward from point for a regex PATTERN.
///
/// If found, moves point to beginning of match and returns the new point.
/// Updates match data with capture groups.
pub fn re_search_backward(
    buf: &mut Buffer,
    pattern: &str,
    bound: Option<usize>,
    noerror: bool,
    case_fold: bool,
    match_data: &mut Option<MatchData>,
) -> Result<Option<usize>, String> {
    let end = buf.pt;
    let limit = bound.unwrap_or(buf.begv).max(buf.begv);

    if end < limit {
        if noerror {
            return Ok(None);
        }
        return Err(format!("Search failed: \"{}\"", pattern));
    }

    let region_start = buf.begv;
    let text = buf.text.text_range(region_start, buf.zv);
    let start_rel = end - region_start;
    let limit_rel = limit - region_start;

    match compile_search_pattern(pattern, case_fold)? {
        CompiledSearchPattern::Literal(literal) => {
            if let Some((rel_start, rel_end)) =
                literal_rfind(&text[limit_rel..start_rel], &literal, case_fold)
            {
                let full_match = (limit + rel_start, limit + rel_end);
                buf.goto_char(full_match.0);
                *match_data = Some(MatchData {
                    groups: vec![Some(full_match)],
                    searched_string: None,
                    searched_buffer: Some(buf.id),
                });
                Ok(Some(full_match.0))
            } else if noerror {
                Ok(None)
            } else {
                Err(format!("Search failed: \"{}\"", pattern))
            }
        }
        CompiledSearchPattern::Segmented(segmented) => {
            if let Some(mut md) = find_backward_segmented_match_data(
                &segmented,
                &text,
                start_rel,
                limit_rel,
                region_start,
                case_fold,
            ) {
                md.searched_string = None;
                md.searched_buffer = Some(buf.id);
                let full_match = md.groups[0].unwrap();
                buf.goto_char(full_match.0);
                *match_data = Some(md);
                Ok(Some(full_match.0))
            } else if noerror {
                Ok(None)
            } else {
                Err(format!("Search failed: \"{}\"", pattern))
            }
        }
        CompiledSearchPattern::Backref(backref) => {
            if let Some(mut md) = find_backward_backref_match_data(
                &backref,
                &text,
                start_rel,
                limit_rel,
                region_start,
                case_fold,
            ) {
                md.searched_string = None;
                md.searched_buffer = Some(buf.id);
                let full_match = md.groups[0].unwrap();
                buf.goto_char(full_match.0);
                *match_data = Some(md);
                Ok(Some(full_match.0))
            } else if noerror {
                Ok(None)
            } else {
                Err(format!("Search failed: \"{}\"", pattern))
            }
        }
        CompiledSearchPattern::Regex(re) => {
            if let Some(mut md) =
                find_backward_match_data(&re, &text, start_rel, limit_rel, region_start)
            {
                md.searched_string = None;
                md.searched_buffer = Some(buf.id);
                let full_match = md.groups[0].unwrap();
                buf.goto_char(full_match.0);
                *match_data = Some(md);
                Ok(Some(full_match.0))
            } else if noerror {
                Ok(None)
            } else {
                Err(format!("Search failed: \"{}\"", pattern))
            }
        }
    }
}

/// Test if text after point matches PATTERN (without moving point).
///
/// Returns `true` if the regex matches starting exactly at point, and
/// updates match data.
pub fn looking_at(
    buf: &Buffer,
    pattern: &str,
    case_fold: bool,
    match_data: &mut Option<MatchData>,
) -> Result<bool, String> {
    let start = buf.pt;
    if start > buf.zv {
        return Ok(false);
    }

    let region_start = buf.begv;
    let text = buf.text.text_range(region_start, buf.zv);
    let start_rel = start - region_start;

    match compile_search_pattern(pattern, case_fold)? {
        CompiledSearchPattern::Literal(literal) => {
            let tail = &text[start_rel..];
            let matched = literal_find(tail, &literal, case_fold)
                .is_some_and(|(match_start, _)| match_start == 0);
            if !matched {
                return Ok(false);
            }
            let full_match = (start, start + literal.len());
            *match_data = Some(MatchData {
                groups: vec![Some(full_match)],
                searched_string: None,
                searched_buffer: Some(buf.id),
            });
            Ok(true)
        }
        CompiledSearchPattern::Segmented(segmented) => {
            if let Some(mut md) =
                segmented_match_at(&segmented, &text, start_rel, text.len(), case_fold)
            {
                md = offset_match_data(md, region_start);
                md.searched_string = None;
                md.searched_buffer = Some(buf.id);
                *match_data = Some(md);
                Ok(true)
            } else {
                Ok(false)
            }
        }
        CompiledSearchPattern::Backref(backref) => {
            if let Some(mut md) = backref_match_at(&backref, &text, start_rel, start_rel, case_fold)
            {
                if md.groups[0].unwrap().0 != start_rel {
                    return Ok(false);
                }
                md = offset_match_data(md, region_start);
                md.searched_string = None;
                md.searched_buffer = Some(buf.id);
                *match_data = Some(md);
                Ok(true)
            } else {
                Ok(false)
            }
        }
        CompiledSearchPattern::Regex(re) => {
            if let Some(caps) = re.captures_at(&text, start_rel) {
                let mut md = match_data_from_captures(&caps, region_start);
                if md.groups[0].unwrap().0 != start {
                    return Ok(false);
                }
                md.searched_string = None;
                md.searched_buffer = Some(buf.id);
                *match_data = Some(md);
                Ok(true)
            } else {
                Ok(false)
            }
        }
    }
}

/// Test whether STRING matches PATTERN starting at byte offset 0.
///
/// Returns `true` if the regex matches at the beginning of STRING and updates
/// match data using character positions, mirroring `looking-at` semantics on a
/// string-backed source.
pub fn looking_at_string(
    pattern: &str,
    string: &str,
    case_fold: bool,
    match_data: &mut Option<MatchData>,
) -> Result<bool, String> {
    match compile_search_pattern(pattern, case_fold)? {
        CompiledSearchPattern::Literal(literal) => {
            let matched = literal_find(string, &literal, case_fold)
                .is_some_and(|(match_start, _)| match_start == 0);
            if !matched {
                return Ok(false);
            }
            *match_data = Some(string_char_match_data(
                SearchedString::Owned(string.to_string()),
                single_group_match_data(0, literal.len()),
            ));
            Ok(true)
        }
        CompiledSearchPattern::Segmented(segmented) => {
            if let Some(md) = segmented_match_at(&segmented, string, 0, string.len(), case_fold) {
                *match_data = Some(string_char_match_data(
                    SearchedString::Owned(string.to_string()),
                    md,
                ));
                Ok(true)
            } else {
                Ok(false)
            }
        }
        CompiledSearchPattern::Backref(backref) => {
            if let Some(md) = backref_match_at(&backref, string, 0, 0, case_fold) {
                *match_data = Some(string_char_match_data(
                    SearchedString::Owned(string.to_string()),
                    md,
                ));
                Ok(true)
            } else {
                Ok(false)
            }
        }
        CompiledSearchPattern::Regex(re) => {
            if let Some(caps) = re.captures_at(string, 0) {
                let byte_md = match_data_from_captures(&caps, 0);
                if byte_md.groups[0].unwrap().0 != 0 {
                    return Ok(false);
                }
                *match_data = Some(string_char_match_data(
                    SearchedString::Owned(string.to_string()),
                    byte_md,
                ));
                Ok(true)
            } else {
                Ok(false)
            }
        }
    }
}

/// Match a regex against a string (not a buffer).
///
/// `start` is the byte offset within `string` to begin matching.
/// Returns the CHARACTER position of the start of the match (relative
/// to the whole string, not `start`), or `None` if no match.
/// Updates match data with capture groups in CHARACTER positions;
/// stores the searched string.
pub fn string_match_full_with_case_fold(
    pattern: &str,
    string: &str,
    start: usize,
    case_fold: bool,
    match_data: &mut Option<MatchData>,
) -> Result<Option<usize>, String> {
    string_match_full_with_case_fold_source(
        pattern,
        string,
        SearchedString::Owned(string.to_string()),
        start,
        case_fold,
        match_data,
    )
}

pub(crate) fn string_match_full_with_case_fold_source_lisp(
    pattern: &str,
    string: &crate::gc::types::LispString,
    searched_string: SearchedString,
    start: usize,
    case_fold: bool,
    match_data: &mut Option<MatchData>,
) -> Result<Option<usize>, String> {
    if start > string.byte_len() {
        return Ok(None);
    }

    match compile_search_pattern(pattern, case_fold)? {
        CompiledSearchPattern::Literal(literal) => {
            if let Some((byte_start, byte_end)) =
                literal_find_lisp_string(string, &literal, start, case_fold)
            {
                let char_md = string_char_match_data(
                    searched_string,
                    single_group_match_data(byte_start, byte_end),
                );
                let result_pos = char_md.groups[0].unwrap().0;
                *match_data = Some(char_md);
                Ok(Some(result_pos))
            } else {
                Ok(None)
            }
        }
        other => string_match_full_with_case_fold_source_compiled(
            other,
            string.as_str(),
            searched_string,
            start,
            case_fold,
            match_data,
        ),
    }
}

pub(crate) fn string_match_full_with_case_fold_source(
    pattern: &str,
    string: &str,
    searched_string: SearchedString,
    start: usize,
    case_fold: bool,
    match_data: &mut Option<MatchData>,
) -> Result<Option<usize>, String> {
    if start > string.len() {
        return Ok(None);
    }

    string_match_full_with_case_fold_source_compiled(
        compile_search_pattern(pattern, case_fold)?,
        string,
        searched_string,
        start,
        case_fold,
        match_data,
    )
}

fn string_match_full_with_case_fold_source_compiled(
    compiled: CompiledSearchPattern,
    string: &str,
    searched_string: SearchedString,
    start: usize,
    case_fold: bool,
    match_data: &mut Option<MatchData>,
) -> Result<Option<usize>, String> {
    match compiled {
        CompiledSearchPattern::Literal(literal) => {
            let byte_match = literal_find(&string[start..], &literal, case_fold)
                .map(|(match_start, match_end)| (start + match_start, start + match_end));
            if let Some((byte_start, byte_end)) = byte_match {
                let char_md = string_char_match_data(
                    searched_string,
                    single_group_match_data(byte_start, byte_end),
                );
                let result_pos = char_md.groups[0].unwrap().0;
                *match_data = Some(char_md);
                Ok(Some(result_pos))
            } else {
                Ok(None)
            }
        }
        CompiledSearchPattern::Segmented(segmented) => {
            if let Some(md) = find_forward_segmented_match_data(
                &segmented,
                string,
                start,
                string.len(),
                0,
                case_fold,
            ) {
                let char_md = string_char_match_data(searched_string, md);
                let result_pos = char_md.groups[0].unwrap().0;
                *match_data = Some(char_md);
                Ok(Some(result_pos))
            } else {
                Ok(None)
            }
        }
        CompiledSearchPattern::Backref(backref) => {
            if let Some(md) =
                find_forward_backref_match_data(&backref, string, start, string.len(), 0, case_fold)
            {
                let char_md = string_char_match_data(searched_string, md);
                let result_pos = char_md.groups[0].unwrap().0;
                *match_data = Some(char_md);
                Ok(Some(result_pos))
            } else {
                Ok(None)
            }
        }
        CompiledSearchPattern::Regex(re) => {
            if let Some(caps) = re.captures_at(string, start) {
                let char_md =
                    string_char_match_data(searched_string, match_data_from_captures(&caps, 0));
                let result_pos = char_md.groups[0].unwrap().0;
                *match_data = Some(char_md);
                Ok(Some(result_pos))
            } else {
                Ok(None)
            }
        }
    }
}

/// Match a regex against a string using Emacs default case-fold behavior.
pub fn string_match_full(
    pattern: &str,
    string: &str,
    start: usize,
    match_data: &mut Option<MatchData>,
) -> Result<Option<usize>, String> {
    string_match_full_with_case_fold(pattern, string, start, true, match_data)
}

/// Replace the last match in a buffer and return `nil`-style success.
pub fn replace_match_buffer(
    buf: &mut Buffer,
    newtext: &str,
    fixedcase: bool,
    literal: bool,
    subexp: usize,
    match_data: &Option<MatchData>,
) -> Result<(), String> {
    let source = buf.text.text_range(0, buf.text.len());
    let (match_start, match_end, replacement) =
        compute_replacement(newtext, fixedcase, literal, subexp, match_data, &source)?;

    buf.goto_char(match_start);
    buf.delete_region(match_start, match_end);
    buf.insert(&replacement);
    Ok(())
}

/// Replace the last match in SOURCE and return the resulting string.
pub fn replace_match_string(
    source: &str,
    newtext: &str,
    fixedcase: bool,
    literal: bool,
    subexp: usize,
    match_data: &Option<MatchData>,
) -> Result<String, String> {
    let (byte_start, byte_end, replacement) =
        compute_replacement(newtext, fixedcase, literal, subexp, match_data, source)?;
    if byte_end > source.len() || byte_start > byte_end {
        return Err(REPLACE_MATCH_SUBEXP_MISSING.to_string());
    }
    Ok(format!(
        "{}{}{}",
        &source[..byte_start],
        replacement,
        &source[byte_end..]
    ))
}

/// Convert a character position to a byte offset in a string.
pub fn char_pos_to_byte(s: &str, char_pos: usize) -> usize {
    s.char_indices()
        .nth(char_pos)
        .map(|(byte_pos, _)| byte_pos)
        .unwrap_or(s.len())
}

fn compute_replacement(
    newtext: &str,
    fixedcase: bool,
    literal: bool,
    subexp: usize,
    match_data: &Option<MatchData>,
    source: &str,
) -> Result<(usize, usize, String), String> {
    let md = match match_data {
        Some(md) => md,
        None => return Err(REPLACE_MATCH_SUBEXP_MISSING.to_string()),
    };

    let (match_start, match_end) = match md.groups.get(subexp) {
        Some(Some(pair)) => *pair,
        _ => return Err(REPLACE_MATCH_SUBEXP_MISSING.to_string()),
    };

    // When match data comes from a string search (searched_string is set),
    // positions are CHARACTER positions.  Convert to byte offsets for slicing.
    let is_string_search = md.searched_string.is_some();
    let (byte_start, byte_end) = if is_string_search {
        (
            char_pos_to_byte(source, match_start),
            char_pos_to_byte(source, match_end),
        )
    } else {
        (match_start, match_end)
    };

    if byte_end > source.len() || byte_start > byte_end {
        return Err(REPLACE_MATCH_SUBEXP_MISSING.to_string());
    }

    let mut replacement = if literal {
        newtext.to_string()
    } else {
        build_replacement(newtext, md, source, is_string_search)
    };

    if !fixedcase {
        let matched = &source[byte_start..byte_end];
        replacement = apply_match_case(&replacement, matched);
    }

    Ok((byte_start, byte_end, replacement))
}

/// Build a replacement string handling `\&` (whole match) and `\N` (group N).
fn build_replacement(template: &str, md: &MatchData, source: &str, char_positions: bool) -> String {
    fn next_char_at(s: &str, byte_idx: usize) -> Option<(char, usize)> {
        s.get(byte_idx..)
            .and_then(|tail| tail.chars().next().map(|ch| (ch, ch.len_utf8())))
    }

    /// Extract matched text from source using group positions.
    fn extract_group(source: &str, s: usize, e: usize, char_positions: bool) -> Option<&str> {
        if char_positions {
            let bs = char_pos_to_byte(source, s);
            let be = char_pos_to_byte(source, e);
            if be <= source.len() && bs <= be {
                Some(&source[bs..be])
            } else {
                None
            }
        } else if e <= source.len() && s <= e {
            Some(&source[s..e])
        } else {
            None
        }
    }

    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if bytes[i] == b'\\' && i + 1 < len {
            let (next, next_len) =
                next_char_at(template, i + 1).expect("byte index must be char boundary");
            match next {
                '&' => {
                    // Whole match
                    if let Some(Some((s, e))) = md.groups.first() {
                        if let Some(text) = extract_group(source, *s, *e, char_positions) {
                            out.push_str(text);
                        }
                    }
                    i += 1 + next_len;
                }
                '0'..='9' => {
                    let group = (next as u8 - b'0') as usize;
                    if let Some(Some((s, e))) = md.groups.get(group) {
                        if let Some(text) = extract_group(source, *s, *e, char_positions) {
                            out.push_str(text);
                        }
                    }
                    i += 1 + next_len;
                }
                '\\' => {
                    out.push('\\');
                    i += 1 + next_len;
                }
                _ => {
                    out.push('\\');
                    out.push(next);
                    i += 1 + next_len;
                }
            }
        } else {
            let (ch, ch_len) = next_char_at(template, i).expect("byte index must be char boundary");
            out.push(ch);
            i += ch_len;
        }
    }

    out
}

fn apply_match_case(replacement: &str, matched: &str) -> String {
    apply_replace_match_case(replacement, matched)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "regex_test.rs"]
mod tests;
