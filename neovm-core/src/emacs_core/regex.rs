//! Regex engine and search primitives for the Elisp VM.
//!
//! Uses the `regex` crate as the backend.  Translates basic Emacs regex
//! syntax to Rust regex syntax before compiling patterns.

use regex::Regex;

use crate::buffer::Buffer;

pub(crate) const REPLACE_MATCH_SUBEXP_MISSING: &str = "replace-match subexpression does not exist";

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
    pub searched_string: Option<String>,
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
        //  - `\` is literal in Emacs but an escape in Rust → double it
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
                // In Emacs, `\` inside [...] is literal.  Escape for Rust.
                out.push_str("\\\\");
                i += 1;
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

fn match_data_from_captures(caps: &regex::Captures<'_>, offset: usize) -> MatchData {
    let mut groups = Vec::with_capacity(caps.len());
    for i in 0..caps.len() {
        groups.push(caps.get(i).map(|m| (m.start() + offset, m.end() + offset)));
    }
    MatchData {
        groups,
        searched_string: None,
    }
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

    let found = if case_fold {
        let escaped = regex::escape(pattern);
        let re =
            Regex::new(&format!("(?i:{escaped})")).map_err(|e| format!("Invalid regexp: {}", e))?;
        re.find(&text).map(|m| (m.start(), m.end()))
    } else {
        text.find(pattern).map(|pos| (pos, pos + pattern.len()))
    };

    if let Some((rel_start, rel_end)) = found {
        let match_start = start + rel_start;
        let match_end = start + rel_end;
        buf.pt = match_end;
        *match_data = Some(MatchData {
            groups: vec![Some((match_start, match_end))],
            searched_string: None,
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

    let found = if case_fold {
        let escaped = regex::escape(pattern);
        let re =
            Regex::new(&format!("(?i:{escaped})")).map_err(|e| format!("Invalid regexp: {}", e))?;
        re.find_iter(&text).last().map(|m| (m.start(), m.end()))
    } else {
        text.rfind(pattern).map(|pos| (pos, pos + pattern.len()))
    };

    if let Some((rel_start, rel_end)) = found {
        let match_start = limit + rel_start;
        let match_end = limit + rel_end;
        buf.pt = match_start;
        *match_data = Some(MatchData {
            groups: vec![Some((match_start, match_end))],
            searched_string: None,
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
    let re = compile_emacs_regex_case_fold(pattern, case_fold)?;
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

    if let Some(mut md) = find_forward_match_data(&re, &text, start_rel, limit_rel, region_start) {
        md.searched_string = None;
        let full_match = md.groups[0].unwrap();
        buf.pt = full_match.1;
        *match_data = Some(md);
        Ok(Some(full_match.1))
    } else {
        if noerror {
            return Ok(None);
        }
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
    let re = compile_emacs_regex_case_fold(pattern, case_fold)?;
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

    if let Some(mut md) = find_backward_match_data(&re, &text, start_rel, limit_rel, region_start) {
        md.searched_string = None;
        let full_match = md.groups[0].unwrap();
        buf.pt = full_match.0;
        *match_data = Some(md);
        Ok(Some(full_match.0))
    } else {
        if noerror {
            return Ok(None);
        }
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
    let re = compile_emacs_regex_case_fold(pattern, case_fold)?;

    let start = buf.pt;
    if start > buf.zv {
        return Ok(false);
    }

    let region_start = buf.begv;
    let text = buf.text.text_range(region_start, buf.zv);
    let start_rel = start - region_start;

    if let Some(caps) = re.captures_at(&text, start_rel) {
        let mut md = match_data_from_captures(&caps, region_start);
        if md.groups[0].unwrap().0 != start {
            return Ok(false);
        }
        md.searched_string = None;
        *match_data = Some(md);
        Ok(true)
    } else {
        Ok(false)
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
    let re = compile_emacs_regex_case_fold(pattern, case_fold)?;

    if start > string.len() {
        return Ok(None);
    }

    if let Some(caps) = re.captures_at(string, start) {
        let byte_md = match_data_from_captures(&caps, 0);
        // Convert byte positions to character positions for string searches.
        // This matches official Emacs behavior where string match data
        // uses character positions, allowing match-data--translate to
        // correctly adjust positions with character-based deltas.
        let char_groups: Vec<Option<(usize, usize)>> = byte_md
            .groups
            .iter()
            .map(|g| {
                g.map(|(bs, be)| {
                    let cs = string.get(..bs).map_or(0, |s| s.chars().count());
                    let ce = string.get(..be).map_or(0, |s| s.chars().count());
                    (cs, ce)
                })
            })
            .collect();
        let result_pos = char_groups[0].unwrap().0;
        *match_data = Some(MatchData {
            groups: char_groups,
            searched_string: Some(string.to_string()),
        });
        Ok(Some(result_pos))
    } else {
        Ok(None)
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

    buf.pt = match_start;
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
    let mut first_is_upper = false;
    let mut saw_first_cased = false;
    let mut has_upper = false;
    let mut has_lower = false;

    for ch in matched.chars() {
        if ch.is_uppercase() {
            has_upper = true;
            if !saw_first_cased {
                first_is_upper = true;
                saw_first_cased = true;
            }
        } else if ch.is_lowercase() {
            has_lower = true;
            if !saw_first_cased {
                first_is_upper = false;
                saw_first_cased = true;
            }
        }
    }

    if has_upper && !has_lower {
        return replacement.chars().flat_map(char::to_uppercase).collect();
    }

    if first_is_upper {
        let mut out = String::with_capacity(replacement.len());
        let mut uppered = false;
        for ch in replacement.chars() {
            if !uppered && ch.is_lowercase() {
                for uc in ch.to_uppercase() {
                    out.push(uc);
                }
                uppered = true;
            } else {
                out.push(ch);
            }
        }
        return out;
    }

    replacement.to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "regex_test.rs"]
mod tests;
