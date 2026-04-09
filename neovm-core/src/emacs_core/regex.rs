//! Regex engine and search primitives for the Elisp VM.
//!
//! Uses a direct translation of GNU Emacs's `regex-emacs.c` as the backend.
//! All pattern compilation, matching, and searching goes through the
//! `regex_emacs` module, ensuring 100% semantic compatibility with GNU.

use std::cell::RefCell;

use crate::buffer::{Buffer, BufferId};
use crate::emacs_core::casefiddle::apply_replace_match_case;
use crate::emacs_core::regex_emacs::{
    self, BufferSyntaxLookup, CompiledPattern, DefaultSyntaxLookup, MatchRegisters, SyntaxLookup,
};

pub(crate) const REPLACE_MATCH_SUBEXP_MISSING: &str = "replace-match subexpression does not exist";
const SEARCH_PATTERN_CACHE_SIZE: usize = 20;

/// Convert `MatchRegisters` (from the GNU-translated engine) into `MatchData`
/// (the public representation used by Elisp builtins).
fn match_data_from_registers(regs: &MatchRegisters, offset: usize) -> MatchData {
    let num_groups = regs.num_regs();
    let mut groups = Vec::with_capacity(num_groups);
    for i in 0..num_groups {
        if regs.start[i] >= 0 && regs.end[i] >= 0 {
            groups.push(Some((
                regs.start[i] as usize + offset,
                regs.end[i] as usize + offset,
            )));
        } else {
            groups.push(None);
        }
    }
    MatchData {
        groups,
        searched_string: None,
        searched_buffer: None,
    }
}

#[derive(Clone)]
enum CompiledSearchPattern {
    /// GNU-translated engine (primary path for all patterns).
    Emacs(CompiledPattern),
    /// Simple literal search (no regex engine needed).
    Literal(String),
}

pub(crate) struct IteratedStringMatches {
    pub capture_count: usize,
    pub matches: Vec<Vec<Option<(usize, usize)>>>,
}

thread_local! {
    // Cache entry is (posix, case_fold, pattern, compiled). The key
    // is extended with `posix` (audit #2) so a non-POSIX compile
    // cannot silently satisfy a POSIX request or vice versa.
    static SEARCH_PATTERN_CACHE: RefCell<Vec<(bool, bool, String, CompiledSearchPattern)>> =
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
    Heap(super::value::Value),
    Owned(String),
}

impl SearchedString {
    pub(crate) fn with_str<R>(&self, f: impl FnOnce(&str) -> R) -> R {
        match self {
            Self::Heap(val) => f(val.as_str().unwrap_or("")),
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
                        let mut closed_interval = false;
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
                                closed_interval = true;
                                break;
                            }
                            scan += 1;
                        }
                        if closed_interval {
                            continue;
                        }
                        out.push('{');
                        i += 1 + next_len;
                    }
                    '}' => {
                        out.push('}');
                        i += 1 + next_len;
                    }
                    // GNU regex.c: \` matches beginning of string (like \A in PCRE)
                    '`' => {
                        out.push_str("\\A");
                        i += 1 + next_len;
                    }
                    // GNU regex.c: \' matches end of string (like \z in PCRE)
                    '\'' => {
                        out.push_str("\\z");
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
                    'w' | 'W' | 'b' | 'B' | 'd' | 'D' | 'n' | 't' | 'r' => match next {
                        _ => {
                            out.push('\\');
                            out.push(next);
                            i += 1 + next_len;
                        }
                    },
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
                    | '6' | '7' | '8' | '9' | 'n' | 't' | 'r' => return false,
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

fn compile_search_pattern(pattern: &str, case_fold: bool) -> Result<CompiledSearchPattern, String> {
    compile_search_pattern_with_posix(pattern, case_fold, false)
}

/// Compile PATTERN for a `posix-*` search builtin.
///
/// GNU's `posix-looking-at`, `posix-search-forward`, `posix-search-backward`,
/// and `posix-string-match` all pass `posix=1` to the underlying
/// `looking_at_1` / `search_buffer` / `string_match_1` helpers
/// (see GNU `src/search.c:Fposix_looking_at` etc.). That flag is then
/// threaded through `compile_pattern` into `regex_compile` and
/// ultimately into `re_match_2_internal`, where the POSIX longest-
/// match algorithm (regex-emacs.c:4143-4344, 5278) kicks in.
///
/// Neomacs's `compile_search_pattern` used to hardcode `posix=false`
/// on the call to `regex_emacs::regex_compile`, which is audit
/// finding #2 in `drafts/regex-search-audit.md`. Callers that want
/// POSIX semantics must go through this helper. The pattern cache is
/// keyed on `(posix, case_fold, pattern)` so a non-POSIX entry never
/// satisfies a POSIX request or vice versa.
fn compile_search_pattern_with_posix(
    pattern: &str,
    case_fold: bool,
    posix: bool,
) -> Result<CompiledSearchPattern, String> {
    if let Some(cached) = crate::emacs_core::perf_trace::time_op(
        crate::emacs_core::perf_trace::HotpathOp::RegexCompileHit,
        || {
            SEARCH_PATTERN_CACHE.with(|cache| {
                let mut cache = cache.borrow_mut();
                let index = cache.iter().position(
                    |(cached_posix, cached_case_fold, cached_pattern, _)| {
                        *cached_posix == posix
                            && *cached_case_fold == case_fold
                            && cached_pattern == pattern
                    },
                )?;
                let entry = cache.remove(index);
                cache.insert(0, entry.clone());
                Some(entry.3)
            })
        },
    ) {
        return Ok(cached);
    }

    let compiled = crate::emacs_core::perf_trace::time_op(
        crate::emacs_core::perf_trace::HotpathOp::RegexCompileMiss,
        || {
            // Use the GNU-translated engine for all patterns.
            // Only optimize plain literals (no regex metacharacters).
            // A trivial literal is unaffected by POSIX vs non-POSIX
            // semantics because there is nothing to backtrack over,
            // so we can keep the Literal fast-path even when posix
            // is requested.
            if let Some(literal) = literal_from_trivial_regexp(pattern)
                && (!case_fold || literal.is_ascii())
            {
                Ok(CompiledSearchPattern::Literal(literal))
            } else {
                regex_emacs::regex_compile(pattern, posix, case_fold)
                    .map(CompiledSearchPattern::Emacs)
                    .map_err(|e| format!("Invalid regexp: {}", e.message))
            }
        },
    )?;

    SEARCH_PATTERN_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.insert(0, (posix, case_fold, pattern.to_string(), compiled.clone()));
        if cache.len() > SEARCH_PATTERN_CACHE_SIZE {
            cache.truncate(SEARCH_PATTERN_CACHE_SIZE);
        }
    });

    Ok(compiled)
}

fn compiled_capture_count(compiled: &CompiledSearchPattern) -> usize {
    match compiled {
        CompiledSearchPattern::Literal(_) => 1,
        CompiledSearchPattern::Emacs(cp) => cp.re_nsub + 1,
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
        CompiledSearchPattern::Emacs(cp) => {
            let syn = DefaultSyntaxLookup;
            let text_bytes = text.as_bytes();
            let range = (limit - start) as isize;
            let result =
                regex_emacs::re_search(cp, &text_bytes[..limit], start, range, &syn, start);
            result.map(|(_pos, regs)| match_data_from_registers(&regs, offset))
        }
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

fn literal_find_lisp_string(
    text: &crate::heap_types::LispString,
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

            literal_find(&text.as_str()[start..], literal, case_fold)
                .map(|(match_start, match_end)| (start + match_start, start + match_end))
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

fn next_search_char_boundary(text: &str, pos: usize) -> Option<usize> {
    if pos >= text.len() {
        return None;
    }
    text[pos..].chars().next().map(|ch| pos + ch.len_utf8())
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
        buf.goto_byte(match_end);
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
        buf.goto_byte(match_start);
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
    re_search_forward_with_posix(buf, pattern, bound, noerror, case_fold, false, match_data)
}

/// POSIX longest-match variant of [`re_search_forward`] used by
/// `posix-search-forward`. See GNU `src/search.c:Fposix_search_forward`.
pub fn re_search_forward_with_posix(
    buf: &mut Buffer,
    pattern: &str,
    bound: Option<usize>,
    noerror: bool,
    case_fold: bool,
    posix: bool,
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

    let md_opt = match compile_search_pattern_with_posix(pattern, case_fold, posix)? {
        CompiledSearchPattern::Literal(literal) => {
            literal_find(&text[start_rel..limit_rel], &literal, case_fold).map(
                |(rel_start, rel_end)| MatchData {
                    groups: vec![Some((start + rel_start, start + rel_end))],
                    searched_string: None,
                    searched_buffer: Some(buf.id),
                },
            )
        }
        CompiledSearchPattern::Emacs(cp) => {
            let syn = BufferSyntaxLookup {
                syntax_table: &buf.syntax_table,
            };
            let text_bytes = text.as_bytes();
            let range = (limit_rel - start_rel) as isize;
            regex_emacs::re_search(
                &cp,
                &text_bytes[..limit_rel],
                start_rel,
                range,
                &syn,
                start_rel,
            )
            .map(|(_pos, regs)| {
                let mut md = match_data_from_registers(&regs, region_start);
                md.searched_buffer = Some(buf.id);
                md
            })
        }
    };

    if let Some(md) = md_opt {
        let full_match = md.groups[0].unwrap();
        buf.goto_byte(full_match.1);
        *match_data = Some(md);
        Ok(Some(full_match.1))
    } else if noerror {
        Ok(None)
    } else {
        Err(format!("Search failed: \"{}\"", pattern))
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
    re_search_backward_with_posix(buf, pattern, bound, noerror, case_fold, false, match_data)
}

/// POSIX longest-match variant of [`re_search_backward`] used by
/// `posix-search-backward`. See GNU `src/search.c:Fposix_search_backward`.
pub fn re_search_backward_with_posix(
    buf: &mut Buffer,
    pattern: &str,
    bound: Option<usize>,
    noerror: bool,
    case_fold: bool,
    posix: bool,
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

    let md_opt = match compile_search_pattern_with_posix(pattern, case_fold, posix)? {
        CompiledSearchPattern::Literal(literal) => {
            literal_rfind(&text[limit_rel..start_rel], &literal, case_fold).map(
                |(rel_start, rel_end)| MatchData {
                    groups: vec![Some((limit + rel_start, limit + rel_end))],
                    searched_string: None,
                    searched_buffer: Some(buf.id),
                },
            )
        }
        CompiledSearchPattern::Emacs(cp) => {
            let syn = BufferSyntaxLookup {
                syntax_table: &buf.syntax_table,
            };
            let text_bytes = text.as_bytes();
            // Backward search: negative range means search backward
            let range = -((start_rel - limit_rel) as isize);
            regex_emacs::re_search(&cp, text_bytes, start_rel, range, &syn, start_rel).map(
                |(_pos, regs)| {
                    let mut md = match_data_from_registers(&regs, region_start);
                    md.searched_buffer = Some(buf.id);
                    md
                },
            )
        }
    };

    if let Some(md) = md_opt {
        let full_match = md.groups[0].unwrap();
        buf.goto_byte(full_match.0);
        *match_data = Some(md);
        Ok(Some(full_match.0))
    } else if noerror {
        Ok(None)
    } else {
        Err(format!("Search failed: \"{}\"", pattern))
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
    looking_at_with_posix(buf, pattern, case_fold, false, match_data)
}

/// POSIX longest-match variant of [`looking_at`] used by
/// `posix-looking-at`. See GNU `src/search.c:Fposix_looking_at`.
pub fn looking_at_with_posix(
    buf: &Buffer,
    pattern: &str,
    case_fold: bool,
    posix: bool,
    match_data: &mut Option<MatchData>,
) -> Result<bool, String> {
    let start = buf.pt;
    if start > buf.zv {
        return Ok(false);
    }

    let region_start = buf.begv;
    let text = buf.text.text_range(region_start, buf.zv);
    let start_rel = start - region_start;

    match compile_search_pattern_with_posix(pattern, case_fold, posix)? {
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
        CompiledSearchPattern::Emacs(cp) => {
            let syn = BufferSyntaxLookup {
                syntax_table: &buf.syntax_table,
            };
            let text_bytes = text.as_bytes();
            if let Some((_end, regs)) = regex_emacs::re_match(
                &cp,
                text_bytes,
                start_rel,
                text_bytes.len(),
                &syn,
                start_rel,
            ) {
                let mut md = match_data_from_registers(&regs, region_start);
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
        CompiledSearchPattern::Emacs(cp) => {
            let syn = DefaultSyntaxLookup;
            let text_bytes = string.as_bytes();
            if let Some((_end, regs)) =
                regex_emacs::re_match(&cp, text_bytes, 0, text_bytes.len(), &syn, 0)
            {
                let byte_md = match_data_from_registers(&regs, 0);
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
    string_match_full_with_case_fold_and_posix(pattern, string, start, case_fold, false, match_data)
}

/// POSIX longest-match variant of [`string_match_full_with_case_fold`]
/// used by `posix-string-match`. See GNU `src/search.c:Fposix_string_match`.
pub fn string_match_full_with_case_fold_and_posix(
    pattern: &str,
    string: &str,
    start: usize,
    case_fold: bool,
    posix: bool,
    match_data: &mut Option<MatchData>,
) -> Result<Option<usize>, String> {
    string_match_full_with_case_fold_source_posix(
        pattern,
        string,
        SearchedString::Owned(string.to_string()),
        start,
        case_fold,
        posix,
        match_data,
    )
}

pub(crate) fn string_match_full_with_case_fold_source_lisp(
    pattern: &str,
    string: &crate::heap_types::LispString,
    searched_string: SearchedString,
    start: usize,
    case_fold: bool,
    match_data: &mut Option<MatchData>,
) -> Result<Option<usize>, String> {
    string_match_full_with_case_fold_source_lisp_posix(
        pattern,
        string,
        searched_string,
        start,
        case_fold,
        false,
        match_data,
    )
}

/// POSIX longest-match variant of
/// [`string_match_full_with_case_fold_source_lisp`] used by
/// `posix-string-match` on Lisp strings. See GNU
/// `src/search.c:Fposix_string_match`.
pub(crate) fn string_match_full_with_case_fold_source_lisp_posix(
    pattern: &str,
    string: &crate::heap_types::LispString,
    searched_string: SearchedString,
    start: usize,
    case_fold: bool,
    posix: bool,
    match_data: &mut Option<MatchData>,
) -> Result<Option<usize>, String> {
    if start > string.byte_len() {
        return Ok(None);
    }

    match compile_search_pattern_with_posix(pattern, case_fold, posix)? {
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
    string_match_full_with_case_fold_source_posix(
        pattern,
        string,
        searched_string,
        start,
        case_fold,
        false,
        match_data,
    )
}

pub(crate) fn string_match_full_with_case_fold_source_posix(
    pattern: &str,
    string: &str,
    searched_string: SearchedString,
    start: usize,
    case_fold: bool,
    posix: bool,
    match_data: &mut Option<MatchData>,
) -> Result<Option<usize>, String> {
    if start > string.len() {
        return Ok(None);
    }

    string_match_full_with_case_fold_source_compiled(
        compile_search_pattern_with_posix(pattern, case_fold, posix)?,
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
    _case_fold: bool,
    match_data: &mut Option<MatchData>,
) -> Result<Option<usize>, String> {
    match compiled {
        CompiledSearchPattern::Literal(literal) => {
            let byte_match = literal_find(&string[start..], &literal, _case_fold)
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
        CompiledSearchPattern::Emacs(cp) => {
            let syn = DefaultSyntaxLookup;
            let text_bytes = string.as_bytes();
            let range = (text_bytes.len() - start) as isize;
            if let Some((_pos, regs)) =
                regex_emacs::re_search(&cp, text_bytes, start, range, &syn, start)
            {
                let byte_md = match_data_from_registers(&regs, 0);
                let char_md = string_char_match_data(searched_string, byte_md);
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

    buf.goto_byte(match_start);
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
        build_replacement(newtext, md, source, is_string_search)?
    };

    if !fixedcase {
        let matched = &source[byte_start..byte_end];
        replacement = apply_match_case(&replacement, matched);
    }

    Ok((byte_start, byte_end, replacement))
}

/// Build a replacement string handling `\&` (whole match) and
/// `\N` (group N, 1-9 only).
///
/// Error semantics mirror GNU `src/search.c:2545-2714` exactly:
///
/// - `\&` → the whole match (`md.groups[0]`). See search.c:2560
///   and search.c:2701.
/// - `\1`..`\9` → the Nth subgroup. `\0` is NOT accepted: GNU's
///   `Freplace_match` loop at search.c:2565 explicitly checks
///   `c >= '1' && c <= '9'`, mirrored at search.c:2703. Any `\0`
///   falls into the `"Invalid use of \\ in replacement text"`
///   error branch at search.c:2584 and 2713. This was audit
///   finding #11 in `drafts/regex-search-audit.md`: before this
///   fix, our `'0'..='9'` range accepted `\0` and returned the
///   whole match.
/// - `\\` → a literal backslash (search.c:2581-2582 and 2708-2709).
/// - `\?` → GNU's string path at search.c:2583 has an explicit
///   `else if (c != '?')` exception: when `c == '?'` neither
///   `substart >= 0` nor `delbackslash` is set, so `lastpos`
///   doesn't advance and the `\?` bytes fall through into the
///   next "middle" copy, effectively emitting the literal `\?`.
///   We mirror that here for both code paths (buffer/string).
/// - Any other `\X` → the same "Invalid use of `\\' in replacement
///   text" error. This was audit finding #12: before this fix, our
///   catch-all silently emitted the literal `\X`.
///
/// The caller (`compute_replacement`) propagates the error; the
/// outer search builtin signals a Lisp error with the GNU-shaped
/// message.
fn build_replacement(
    template: &str,
    md: &MatchData,
    source: &str,
    char_positions: bool,
) -> Result<String, String> {
    const INVALID_BACKSLASH_MSG: &str =
        "Invalid use of `\\' in replacement text";

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
                '1'..='9' => {
                    // GNU search.c:2549 — explicit `c >= '1' && c <= '9'`.
                    // `\0` intentionally falls through to the error arm.
                    let group = (next as u8 - b'0') as usize;
                    if let Some(Some((s, e))) = md.groups.get(group) {
                        if let Some(text) = extract_group(source, *s, *e, char_positions) {
                            out.push_str(text);
                        }
                    }
                    i += 1 + next_len;
                }
                '\\' => {
                    // GNU search.c:2581-2582, 2708-2709.
                    out.push('\\');
                    i += 1 + next_len;
                }
                '?' => {
                    // GNU search.c:2583 `else if (c != '?')`.
                    // `\?` is passed through literally in the
                    // string path; we honor that for both paths.
                    out.push('\\');
                    out.push('?');
                    i += 1 + next_len;
                }
                _ => {
                    // GNU search.c:2584, 2713 — any other backslash
                    // sequence (`\0`, `\n`, `\X`, …) signals an
                    // `error ("Invalid use of `\\' in replacement
                    // text")`.
                    return Err(INVALID_BACKSLASH_MSG.to_string());
                }
            }
        } else {
            let (ch, ch_len) = next_char_at(template, i).expect("byte index must be char boundary");
            out.push(ch);
            i += ch_len;
        }
    }

    Ok(out)
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
