//! Interactive search and query-replace.
//!
//! Implements:
//! - Incremental search (isearch) state machine
//! - Search history
//! - Search highlighting (overlay tracking)
//! - Query-replace with interactive responses
//! - Regular expression search variants
//! - Lazy highlight (deferred search matches)

use std::collections::VecDeque;

use super::error::{EvalResult, Flow, signal};
use super::intern::intern;
use super::value::{Value, with_heap, ValueKind};
use crate::buffer::Buffer;

// ---------------------------------------------------------------------------
// Argument helpers (local copies, matching builtins.rs convention)
// ---------------------------------------------------------------------------

fn expect_min_max_args(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), Flow> {
    if args.len() < min || args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_string(val: &Value) -> Result<String, Flow> {
    match val.kind() {
        ValueKind::String => Ok(val.as_str().unwrap().to_owned()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *val],
        )),
    }
}

fn expect_integer_or_marker(val: &Value) -> Result<i64, Flow> {
    match val.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        ValueKind::Char(c) => Ok(c as i64),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *val],
        )),
    }
}

fn expect_sequence_string(val: &Value) -> Result<String, Flow> {
    match val.kind() {
        ValueKind::String => Ok(val.as_str().unwrap().to_owned()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), *val],
        )),
    }
}

fn lisp_pos_to_byte(buf: &crate::buffer::Buffer, raw: i64) -> usize {
    buf.lisp_pos_to_accessible_byte(raw)
}

fn replacement_region_bounds(
    buf: &crate::buffer::Buffer,
    start_arg: Option<&Value>,
    end_arg: Option<&Value>,
    backward: bool,
    region_noncontiguous: bool,
) -> Result<(usize, usize), Flow> {
    if region_noncontiguous {
        let mark = buf.mark().ok_or_else(|| {
            signal(
                "error",
                vec![Value::string(
                    "The mark is not set now, so there is no region",
                )],
            )
        })?;
        let pt = buf.point();
        return Ok((pt.min(mark), pt.max(mark)));
    }

    let start = match start_arg {
        Some(v) if !v.is_nil() => lisp_pos_to_byte(buf, expect_integer_or_marker(v)?),
        _ if backward => buf.point_min(),
        _ => buf.point(),
    };
    let end = match end_arg {
        Some(v) if !v.is_nil() => lisp_pos_to_byte(buf, expect_integer_or_marker(v)?),
        _ if backward => buf.point(),
        _ => buf.point_max(),
    };
    if start <= end {
        Ok((start, end))
    } else {
        Ok((end, start))
    }
}

fn line_operation_region_bounds(
    buf: &crate::buffer::Buffer,
    start_arg: Option<&Value>,
    end_arg: Option<&Value>,
) -> Result<(usize, usize), Flow> {
    let start = match start_arg {
        Some(v) if !v.is_nil() => lisp_pos_to_byte(buf, expect_integer_or_marker(v)?),
        _ => buf.point(),
    };
    let end = match end_arg {
        Some(v) if !v.is_nil() => lisp_pos_to_byte(buf, expect_integer_or_marker(v)?),
        _ => buf.point_max(),
    };
    if start <= end {
        Ok((start, end))
    } else {
        Ok((end, start))
    }
}

fn line_start_at_or_before(source: &str, at: usize) -> usize {
    let pos = at.min(source.len());
    match source[..pos].rfind('\n') {
        Some(idx) => idx + 1,
        None => 0,
    }
}

fn dynamic_or_global_symbol_value(eval: &super::eval::Context, name: &str) -> Option<Value> {
    eval.obarray.symbol_value(name).cloned()
}

fn buffer_read_only_active(eval: &super::eval::Context, buf: &Buffer) -> bool {
    if buf.read_only {
        return true;
    }

    if let Some(value) = buf.get_buffer_local("buffer-read-only") {
        return value.is_truthy();
    }

    eval.obarray
        .symbol_value("buffer-read-only")
        .is_some_and(|value| value.is_truthy())
}

fn case_fold_for_pattern(eval: &super::eval::Context, pattern: &str) -> bool {
    let case_fold_search_enabled = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|value| !value.is_nil())
        .unwrap_or(true);
    if !case_fold_search_enabled {
        return false;
    }
    // Emacs honors `search-upper-case`: nil disables smart-case and keeps
    // folding even when PATTERN contains uppercase characters.
    let smart_case_enabled = dynamic_or_global_symbol_value(eval, "search-upper-case")
        .map(|value| !value.is_nil())
        .unwrap_or(true);
    if !smart_case_enabled {
        return true;
    }
    resolve_case_fold(None, pattern)
}

fn case_replace_enabled(eval: &super::eval::Context) -> bool {
    dynamic_or_global_symbol_value(eval, "case-replace")
        .map(|value| !value.is_nil())
        .unwrap_or(true)
}

fn replace_lax_whitespace_enabled(eval: &super::eval::Context) -> bool {
    dynamic_or_global_symbol_value(eval, "replace-lax-whitespace")
        .map(|value| !value.is_nil())
        .unwrap_or(false)
}

fn resolve_search_whitespace_regexp(eval: &super::eval::Context) -> Option<String> {
    let raw = match dynamic_or_global_symbol_value(eval, "search-whitespace-regexp") {
        Some(ValueKind::String) => with_heap(|h| h.get_string(id).to_owned()),
        Some(ValueKind::Nil) | None => "[ \t\n\r]+".to_string(),
        Some(_) => return None,
    };
    Some(raw)
}

fn quote_emacs_regexp_literal(literal: &str) -> String {
    let mut result = String::with_capacity(literal.len() + 8);
    for ch in literal.chars() {
        match ch {
            '.' | '*' | '+' | '?' | '[' | '^' | '$' | '\\' => {
                result.push('\\');
                result.push(ch);
            }
            _ => result.push(ch),
        }
    }
    result
}

fn build_lax_whitespace_pattern(pattern: &str, whitespace_regex: &str) -> String {
    let mut raw = String::new();
    let mut literal = String::new();
    let mut in_space_run = false;

    for ch in pattern.chars() {
        if ch == ' ' {
            if !literal.is_empty() {
                raw.push_str(&quote_emacs_regexp_literal(&literal));
                literal.clear();
            }
            if !in_space_run {
                raw.push_str("\\(");
                raw.push_str(whitespace_regex);
                raw.push_str("\\)");
                in_space_run = true;
            }
        } else {
            in_space_run = false;
            literal.push(ch);
        }
    }

    if !literal.is_empty() {
        raw.push_str(&quote_emacs_regexp_literal(&literal));
    }

    raw
}

fn string_matches_regexp(text: &str, pattern: &str, case_fold: bool) -> Result<bool, Flow> {
    let mut match_data = None;
    super::regex::string_match_full_with_case_fold(pattern, text, 0, case_fold, &mut match_data)
        .map(|matched| matched.is_some())
        .map_err(|e| {
            signal(
                "invalid-regexp",
                vec![Value::string(format!("Invalid regexp: {e}"))],
            )
        })
}

fn count_string_regexp_matches(text: &str, pattern: &str, case_fold: bool) -> Result<i64, Flow> {
    let iterated = super::regex::iterate_string_matches_with_case_fold(pattern, text, 0, case_fold)
        .map_err(|e| {
            signal(
                "invalid-regexp",
                vec![Value::string(format!("Invalid regexp: {e}"))],
            )
        })?;
    Ok(iterated
        .matches
        .into_iter()
        .filter_map(|groups| groups.first().and_then(|group| *group))
        .filter(|(match_start, match_end)| {
            !(*match_start == *match_end && *match_start >= text.len())
        })
        .count() as i64)
}

// ---------------------------------------------------------------------------
// Search direction
// ---------------------------------------------------------------------------

/// Direction of an incremental search.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchDirection {
    Forward,
    Backward,
}

// ---------------------------------------------------------------------------
// IsearchState — tracks incremental search session
// ---------------------------------------------------------------------------

/// Full state for one active incremental search session.
pub struct IsearchState {
    /// Whether the search session is still running.
    pub active: bool,
    /// Current search direction.
    pub direction: SearchDirection,
    /// The string being searched for (built up character by character).
    pub search_string: String,
    /// Whether this is a regexp search.
    pub regexp: bool,
    /// Case folding: `None` = auto-detect, `Some(true)` = fold, `Some(false)` = exact.
    pub case_fold: Option<bool>,
    /// Whether the search has wrapped around the buffer.
    pub wrapped: bool,
    /// Whether the last incremental search step succeeded.
    pub success: bool,
    /// Byte position of the start of the current match (if any).
    pub match_start: Option<usize>,
    /// Byte position of the end of the current match (if any).
    pub match_end: Option<usize>,
    /// Point position when the search was started (for abort restoration).
    pub origin: usize,
    /// Position where wrapping resets to.
    pub barrier: usize,
    /// Index into the history ring, if navigating history.
    pub history_index: Option<usize>,
    /// All visible matches for lazy-highlight overlays: `(start, end)` pairs.
    pub lazy_matches: Vec<(usize, usize)>,
}

// ---------------------------------------------------------------------------
// SearchHistory
// ---------------------------------------------------------------------------

/// Ring of previous search strings, kept separately for literal and regexp.
pub struct SearchHistory {
    strings: VecDeque<String>,
    regexp_strings: VecDeque<String>,
    max_length: usize,
}

impl SearchHistory {
    /// Create an empty history with default capacity of 100 entries per ring.
    pub fn new() -> Self {
        Self {
            strings: VecDeque::new(),
            regexp_strings: VecDeque::new(),
            max_length: 100,
        }
    }

    /// Push a search string onto the appropriate ring.
    /// Duplicates are moved to the front rather than stored twice.
    pub fn push(&mut self, string: String, regexp: bool) {
        let ring = if regexp {
            &mut self.regexp_strings
        } else {
            &mut self.strings
        };
        // Remove duplicate if present
        if let Some(pos) = ring.iter().position(|s| *s == string) {
            ring.remove(pos);
        }
        ring.push_front(string);
        if ring.len() > self.max_length {
            ring.pop_back();
        }
    }

    /// Get the search string at `index` (0 = most recent).
    pub fn get(&self, index: usize, regexp: bool) -> Option<&str> {
        let ring = if regexp {
            &self.regexp_strings
        } else {
            &self.strings
        };
        ring.get(index).map(|s| s.as_str())
    }

    /// Number of entries in the chosen ring.
    pub fn len(&self, regexp: bool) -> usize {
        if regexp {
            self.regexp_strings.len()
        } else {
            self.strings.len()
        }
    }

    /// Borrow the underlying deque.
    pub fn strings(&self, regexp: bool) -> &VecDeque<String> {
        if regexp {
            &self.regexp_strings
        } else {
            &self.strings
        }
    }
}

impl Default for SearchHistory {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// IsearchManager
// ---------------------------------------------------------------------------

/// Manages the lifecycle of incremental search sessions.
pub struct IsearchManager {
    state: Option<IsearchState>,
    history: SearchHistory,
    last_search_string: Option<String>,
    last_search_regexp: bool,
}

impl IsearchManager {
    pub fn new() -> Self {
        Self {
            state: None,
            history: SearchHistory::new(),
            last_search_string: None,
            last_search_regexp: false,
        }
    }

    // -- Start/end ----------------------------------------------------------

    /// Begin a new incremental search session.
    pub fn begin_search(&mut self, direction: SearchDirection, regexp: bool, origin: usize) {
        self.state = Some(IsearchState {
            active: true,
            direction,
            search_string: String::new(),
            regexp,
            case_fold: None, // auto
            wrapped: false,
            success: true,
            match_start: None,
            match_end: None,
            origin,
            barrier: origin,
            history_index: None,
            lazy_matches: Vec::new(),
        });
    }

    /// End the search session normally, optionally saving the string to history.
    pub fn end_search(&mut self, save_to_history: bool) {
        if let Some(state) = self.state.take() {
            if save_to_history && !state.search_string.is_empty() {
                self.history.push(state.search_string.clone(), state.regexp);
            }
            if !state.search_string.is_empty() {
                self.last_search_regexp = state.regexp;
                self.last_search_string = Some(state.search_string);
            }
        }
    }

    /// Abort the search session.  Returns the original point position so that
    /// the caller can restore it.
    pub fn abort_search(&mut self) -> usize {
        let origin = self.state.as_ref().map(|s| s.origin).unwrap_or(0);
        self.state = None;
        origin
    }

    // -- Modify search ------------------------------------------------------

    /// Append a character to the search string.
    pub fn add_char(&mut self, ch: char) {
        if let Some(state) = self.state.as_mut() {
            state.search_string.push(ch);
            state.history_index = None;
        }
    }

    /// Remove the last character from the search string.
    pub fn delete_char(&mut self) {
        if let Some(state) = self.state.as_mut() {
            state.search_string.pop();
            state.history_index = None;
        }
    }

    /// Replace the search string wholesale.
    pub fn set_string(&mut self, s: String) {
        if let Some(state) = self.state.as_mut() {
            state.search_string = s;
            state.history_index = None;
        }
    }

    /// Toggle between literal and regexp search.
    pub fn toggle_regexp(&mut self) {
        if let Some(state) = self.state.as_mut() {
            state.regexp = !state.regexp;
        }
    }

    /// Toggle case-fold cycling: auto -> fold -> exact -> auto.
    pub fn toggle_case_fold(&mut self) {
        if let Some(state) = self.state.as_mut() {
            state.case_fold = match state.case_fold {
                None => Some(true),
                Some(true) => Some(false),
                Some(false) => None,
            };
        }
    }

    /// Reverse the search direction.
    pub fn reverse_direction(&mut self) {
        if let Some(state) = self.state.as_mut() {
            state.direction = match state.direction {
                SearchDirection::Forward => SearchDirection::Backward,
                SearchDirection::Backward => SearchDirection::Forward,
            };
        }
    }

    // -- Search operations --------------------------------------------------

    /// Perform one incremental search step in the current direction, starting
    /// from the current match position (or the barrier after a wrap).
    ///
    /// `text` is the full buffer contents.  Returns match `(start, end)` if
    /// found.  The caller is responsible for moving point.
    pub fn search_next(&mut self, text: &str) -> Option<(usize, usize)> {
        let state = self.state.as_mut()?;
        if state.search_string.is_empty() {
            state.success = true;
            state.match_start = None;
            state.match_end = None;
            return None;
        }

        let case_fold = resolve_case_fold(state.case_fold, &state.search_string);

        // Determine the starting position for the next search step.
        let from = match state.direction {
            SearchDirection::Forward => state.match_end.unwrap_or(state.barrier),
            SearchDirection::Backward => state.match_start.unwrap_or(state.barrier),
        };

        let forward = state.direction == SearchDirection::Forward;

        if let Some((start, end)) = find_match(
            text,
            &state.search_string,
            from,
            forward,
            state.regexp,
            case_fold,
        ) {
            state.success = true;
            state.match_start = Some(start);
            state.match_end = Some(end);
            return Some((start, end));
        }

        // Not found from current position — try wrapping.
        if !state.wrapped {
            state.wrapped = true;
            let wrap_from = if forward { 0 } else { text.len() };
            if let Some((start, end)) = find_match(
                text,
                &state.search_string,
                wrap_from,
                forward,
                state.regexp,
                case_fold,
            ) {
                state.success = true;
                state.match_start = Some(start);
                state.match_end = Some(end);
                return Some((start, end));
            }
        }

        state.success = false;
        None
    }

    /// Re-run the search from the origin for the current search string (used
    /// after each character addition/deletion to update the match).
    ///
    /// `text` is the full buffer contents.  Returns match `(start, end)` if
    /// found.
    pub fn search_update(&mut self, text: &str) -> Option<(usize, usize)> {
        let state = self.state.as_mut()?;
        if state.search_string.is_empty() {
            state.success = true;
            state.match_start = None;
            state.match_end = None;
            state.wrapped = false;
            return None;
        }

        let case_fold = resolve_case_fold(state.case_fold, &state.search_string);
        let forward = state.direction == SearchDirection::Forward;

        // Search from origin first.
        if let Some((start, end)) = find_match(
            text,
            &state.search_string,
            state.origin,
            forward,
            state.regexp,
            case_fold,
        ) {
            state.success = true;
            state.wrapped = false;
            state.match_start = Some(start);
            state.match_end = Some(end);
            return Some((start, end));
        }

        // Wrap around.
        let wrap_from = if forward { 0 } else { text.len() };
        if let Some((start, end)) = find_match(
            text,
            &state.search_string,
            wrap_from,
            forward,
            state.regexp,
            case_fold,
        ) {
            state.success = true;
            state.wrapped = true;
            state.match_start = Some(start);
            state.match_end = Some(end);
            return Some((start, end));
        }

        state.success = false;
        state.wrapped = false;
        state.match_start = None;
        state.match_end = None;
        None
    }

    /// Compute all matches within the visible region for lazy-highlight
    /// overlays.
    pub fn compute_lazy_matches(&mut self, text: &str, visible_start: usize, visible_end: usize) {
        let state = match self.state.as_mut() {
            Some(s) => s,
            None => return,
        };

        state.lazy_matches.clear();

        if state.search_string.is_empty() {
            return;
        }

        let case_fold = resolve_case_fold(state.case_fold, &state.search_string);
        let start = visible_start.min(text.len());
        let end = visible_end.min(text.len());

        if start >= end {
            return;
        }

        let region = &text[start..end];

        if state.regexp {
            if let Ok(iterated) = super::regex::iterate_string_matches_with_case_fold(
                &state.search_string,
                region,
                0,
                case_fold,
            ) {
                for groups in iterated.matches {
                    let Some((match_start, match_end)) = groups.first().and_then(|group| *group)
                    else {
                        continue;
                    };
                    if match_start == match_end {
                        continue;
                    }
                    state
                        .lazy_matches
                        .push((start + match_start, start + match_end));
                }
            }
        } else {
            let haystack = if case_fold {
                region.to_lowercase()
            } else {
                region.to_string()
            };
            let needle = if case_fold {
                state.search_string.to_lowercase()
            } else {
                state.search_string.clone()
            };
            let mut search_from = 0;
            while let Some(pos) = haystack[search_from..].find(&needle) {
                let ms = start + search_from + pos;
                let me = ms + needle.len();
                state.lazy_matches.push((ms, me));
                search_from += pos + needle.len();
            }
        }
    }

    // -- History navigation -------------------------------------------------

    /// Move to the previous (older) history entry.
    pub fn history_previous(&mut self) {
        let state = match self.state.as_mut() {
            Some(s) => s,
            None => return,
        };
        let ring_len = self.history.len(state.regexp);
        if ring_len == 0 {
            return;
        }
        let new_index = match state.history_index {
            None => 0,
            Some(i) => {
                if i + 1 < ring_len {
                    i + 1
                } else {
                    return;
                }
            }
        };
        if let Some(s) = self.history.get(new_index, state.regexp) {
            state.search_string = s.to_string();
            state.history_index = Some(new_index);
        }
    }

    /// Move to the next (newer) history entry.
    pub fn history_next(&mut self) {
        let state = match self.state.as_mut() {
            Some(s) => s,
            None => return,
        };
        match state.history_index {
            None => {}
            Some(0) => {
                state.search_string.clear();
                state.history_index = None;
            }
            Some(i) => {
                let new_index = i - 1;
                if let Some(s) = self.history.get(new_index, state.regexp) {
                    state.search_string = s.to_string();
                    state.history_index = Some(new_index);
                }
            }
        }
    }

    /// Yank the word (or character) at point into the search string.
    ///
    /// `text` is the full buffer text; `point` is the current cursor position.
    /// Appends text from `point` up to the next word boundary.
    pub fn yank_word_or_char(&mut self, text: &str, point: usize) {
        let state = match self.state.as_mut() {
            Some(s) => s,
            None => return,
        };

        if point >= text.len() {
            return;
        }

        let rest = &text[point..];
        let mut end = 0;
        let mut chars = rest.chars();

        // Grab at least one char; then continue while alphanumeric.
        if let Some(ch) = chars.next() {
            end += ch.len_utf8();
            if ch.is_alphanumeric() || ch == '_' {
                for ch2 in chars {
                    if ch2.is_alphanumeric() || ch2 == '_' {
                        end += ch2.len_utf8();
                    } else {
                        break;
                    }
                }
            }
        }

        state.search_string.push_str(&rest[..end]);
        state.history_index = None;
    }

    // -- State queries ------------------------------------------------------

    /// Whether an incremental search is currently active.
    pub fn is_active(&self) -> bool {
        self.state.as_ref().is_some_and(|s| s.active)
    }

    /// Borrow the current state (if any).
    pub fn state(&self) -> Option<&IsearchState> {
        self.state.as_ref()
    }

    /// Build the minibuffer prompt string for the current search.
    pub fn prompt(&self) -> String {
        let state = match self.state.as_ref() {
            Some(s) => s,
            None => return String::new(),
        };

        let mut parts = Vec::new();

        if !state.success {
            parts.push("Failing");
        }
        if state.wrapped {
            parts.push("Wrapped");
        }
        if state.regexp {
            parts.push("Regexp");
        }

        let dir = match state.direction {
            SearchDirection::Forward => "I-search",
            SearchDirection::Backward => "I-search backward",
        };
        parts.push(dir);

        let prompt = parts.join(" ");
        format!("{}: {}", prompt, state.search_string)
    }
}

impl Default for IsearchManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Query-replace response
// ---------------------------------------------------------------------------

/// Possible user responses during a query-replace session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryReplaceResponse {
    /// `y` / `SPC` — replace this match.
    Yes,
    /// `n` / `DEL` — skip this match.
    No,
    /// `!` — replace all remaining matches without asking.
    ReplaceAll,
    /// `q` / `RET` — stop replacing.
    Quit,
    /// `e` — edit the replacement string for this match.
    Edit,
    /// `d` — delete the match text without inserting the replacement.
    Delete,
    /// `u` — undo the last replacement.
    Undo,
    /// `?` — show help text.
    Help,
}

// ---------------------------------------------------------------------------
// QueryReplaceUndo
// ---------------------------------------------------------------------------

/// Record of a single replacement for undo purposes.
#[derive(Clone, Debug)]
pub struct QueryReplaceUndo {
    /// Byte position where the replacement was made.
    pub position: usize,
    /// Original matched text.
    pub original: String,
    /// Replacement text that was inserted.
    pub replacement: String,
}

// ---------------------------------------------------------------------------
// QueryReplaceState
// ---------------------------------------------------------------------------

/// Full state for one active query-replace session.
pub struct QueryReplaceState {
    /// Pattern to search for.
    pub from_string: String,
    /// Replacement string.
    pub to_string: String,
    /// Whether `from_string` is a regular expression.
    pub regexp: bool,
    /// Whether to match only whole delimited words.
    pub delimited: bool,
    /// Case folding override: `None` = auto, `Some(true)` = fold, `Some(false)` = exact.
    pub case_fold: Option<bool>,
    /// Whether to preserve the case pattern of the matched text.
    pub preserve_case: bool,
    /// Optional region restriction (start byte).
    pub region_start: Option<usize>,
    /// Optional region restriction (end byte).
    pub region_end: Option<usize>,
    /// Current match being presented to the user: `(start, end)`.
    pub current_match: Option<(usize, usize)>,
    /// Number of replacements made so far.
    pub replaced_count: usize,
    /// Number of matches skipped so far.
    pub skipped_count: usize,
    /// Stack of undoable replacements (most recent last).
    pub undo_stack: Vec<QueryReplaceUndo>,
}

// ---------------------------------------------------------------------------
// QueryReplaceAction
// ---------------------------------------------------------------------------

/// Action the caller should take after a query-replace response.
#[derive(Clone, Debug)]
pub enum QueryReplaceAction {
    /// Replace the region `[start, end)` with the given string.
    Replace(usize, usize, String),
    /// Skip the current match.
    Skip,
    /// The session is finished.
    Done(QueryReplaceSummary),
    /// Display this help text to the user.
    ShowHelp(String),
    /// The user asked to edit the replacement — caller should prompt for input.
    NeedInput,
}

// ---------------------------------------------------------------------------
// QueryReplaceSummary
// ---------------------------------------------------------------------------

/// Summary statistics returned when a query-replace session ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryReplaceSummary {
    pub replaced: usize,
    pub skipped: usize,
}

// ---------------------------------------------------------------------------
// QueryReplaceManager
// ---------------------------------------------------------------------------

/// Manages query-replace sessions.
pub struct QueryReplaceManager {
    state: Option<QueryReplaceState>,
}

impl QueryReplaceManager {
    pub fn new() -> Self {
        Self { state: None }
    }

    /// Begin a new query-replace session (whole buffer).
    pub fn begin(&mut self, from: String, to: String, regexp: bool) {
        self.state = Some(QueryReplaceState {
            from_string: from,
            to_string: to,
            regexp,
            delimited: false,
            case_fold: None,
            preserve_case: true,
            region_start: None,
            region_end: None,
            current_match: None,
            replaced_count: 0,
            skipped_count: 0,
            undo_stack: Vec::new(),
        });
    }

    /// Begin a query-replace session restricted to a region.
    pub fn begin_in_region(
        &mut self,
        from: String,
        to: String,
        regexp: bool,
        start: usize,
        end: usize,
    ) {
        self.begin(from, to, regexp);
        if let Some(state) = self.state.as_mut() {
            state.region_start = Some(start);
            state.region_end = Some(end);
        }
    }

    /// Find the next match at or after `from_pos`.
    ///
    /// `text` is the full buffer contents.  Returns `(start, end)` of the
    /// match, also storing it in `current_match`.  Returns `None` when there
    /// are no more matches.
    pub fn find_next(&mut self, text: &str, from_pos: usize) -> Option<(usize, usize)> {
        let state = self.state.as_mut()?;

        let limit = state.region_end.unwrap_or(text.len()).min(text.len());
        let start = from_pos.max(state.region_start.unwrap_or(0));

        if start > limit {
            state.current_match = None;
            return None;
        }

        let case_fold = resolve_case_fold(state.case_fold, &state.from_string);
        let result = find_match(
            text,
            &state.from_string,
            start,
            true,
            state.regexp,
            case_fold,
        );

        if let Some((ms, me)) = result {
            if me <= limit {
                state.current_match = Some((ms, me));
                return Some((ms, me));
            }
        }

        state.current_match = None;
        None
    }

    /// Apply the user's response to the current match.
    pub fn respond(&mut self, response: QueryReplaceResponse) -> QueryReplaceAction {
        let state = match self.state.as_mut() {
            Some(s) => s,
            None => {
                return QueryReplaceAction::Done(QueryReplaceSummary {
                    replaced: 0,
                    skipped: 0,
                });
            }
        };

        match response {
            QueryReplaceResponse::Yes => {
                if let Some((start, end)) = state.current_match {
                    let matched_text = String::new(); // caller fills this in
                    let replacement = state.to_string.clone();
                    let replacement = if state.preserve_case {
                        // We cannot access the matched text here; the caller
                        // should use `compute_replacement` before calling
                        // `respond`.  Return the raw replacement.
                        replacement
                    } else {
                        replacement
                    };
                    state.replaced_count += 1;
                    state.undo_stack.push(QueryReplaceUndo {
                        position: start,
                        original: matched_text,
                        replacement: replacement.clone(),
                    });
                    state.current_match = None;
                    QueryReplaceAction::Replace(start, end, replacement)
                } else {
                    QueryReplaceAction::Skip
                }
            }
            QueryReplaceResponse::No => {
                state.skipped_count += 1;
                state.current_match = None;
                QueryReplaceAction::Skip
            }
            QueryReplaceResponse::ReplaceAll => {
                // Signal the caller to replace the current match and all
                // remaining ones.  We handle the *current* match here; the
                // caller should loop `find_next` + `respond(Yes)` for the rest.
                if let Some((start, end)) = state.current_match {
                    let replacement = state.to_string.clone();
                    state.replaced_count += 1;
                    state.undo_stack.push(QueryReplaceUndo {
                        position: start,
                        original: String::new(),
                        replacement: replacement.clone(),
                    });
                    state.current_match = None;
                    QueryReplaceAction::Replace(start, end, replacement)
                } else {
                    QueryReplaceAction::Skip
                }
            }
            QueryReplaceResponse::Quit => {
                let summary = QueryReplaceSummary {
                    replaced: state.replaced_count,
                    skipped: state.skipped_count,
                };
                self.state = None;
                QueryReplaceAction::Done(summary)
            }
            QueryReplaceResponse::Edit => QueryReplaceAction::NeedInput,
            QueryReplaceResponse::Delete => {
                if let Some((start, end)) = state.current_match {
                    state.replaced_count += 1;
                    state.undo_stack.push(QueryReplaceUndo {
                        position: start,
                        original: String::new(),
                        replacement: String::new(),
                    });
                    state.current_match = None;
                    // Replace with empty string = delete
                    QueryReplaceAction::Replace(start, end, String::new())
                } else {
                    QueryReplaceAction::Skip
                }
            }
            QueryReplaceResponse::Undo => {
                // Return the last undo entry via Done-like mechanism.
                // The actual undo application is done by `undo_last`.
                QueryReplaceAction::Skip
            }
            QueryReplaceResponse::Help => {
                let help = concat!(
                    "y/SPC - replace this match\n",
                    "n/DEL - skip this match\n",
                    "! - replace all remaining matches\n",
                    "q/RET - quit\n",
                    "e - edit replacement\n",
                    "d - delete match (no replacement)\n",
                    "u - undo last replacement\n",
                    "? - show this help",
                );
                QueryReplaceAction::ShowHelp(help.to_string())
            }
        }
    }

    /// Compute the replacement text for a given matched string.
    ///
    /// Handles `preserve_case` logic.  For regexp replacements the caller
    /// should additionally process `\&` and `\N` references (see
    /// `regex::build_replacement`).
    pub fn compute_replacement(&self, matched: &str) -> String {
        let state = match self.state.as_ref() {
            Some(s) => s,
            None => return String::new(),
        };

        if state.preserve_case {
            preserve_case(&state.to_string, matched)
        } else {
            state.to_string.clone()
        }
    }

    /// Pop and return the most recent undo entry.
    pub fn undo_last(&mut self) -> Option<QueryReplaceUndo> {
        let state = self.state.as_mut()?;
        let entry = state.undo_stack.pop();
        if entry.is_some() {
            // Decrement replaced count since we are undoing.
            state.replaced_count = state.replaced_count.saturating_sub(1);
        }
        entry
    }

    /// End the session and return a summary.
    pub fn finish(&mut self) -> QueryReplaceSummary {
        let state = match self.state.take() {
            Some(s) => s,
            None => {
                return QueryReplaceSummary {
                    replaced: 0,
                    skipped: 0,
                };
            }
        };
        QueryReplaceSummary {
            replaced: state.replaced_count,
            skipped: state.skipped_count,
        }
    }

    /// Whether a query-replace session is currently active.
    pub fn is_active(&self) -> bool {
        self.state.is_some()
    }

    /// Borrow the current state (if any).
    pub fn state(&self) -> Option<&QueryReplaceState> {
        self.state.as_ref()
    }

    /// Build the minibuffer prompt for the current session.
    pub fn prompt(&self) -> String {
        let state = match self.state.as_ref() {
            Some(s) => s,
            None => return String::new(),
        };
        let kind = if state.regexp {
            "Query replacing regexp"
        } else {
            "Query replacing"
        };
        format!(
            "{} {} with {}: (y/n/!/q/?)",
            kind, state.from_string, state.to_string
        )
    }
}

impl Default for QueryReplaceManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Helper: resolve case folding
// ---------------------------------------------------------------------------

/// Determine effective case folding.
///
/// When `override_val` is `None` (auto), we fold if the search string is
/// entirely lowercase (Emacs `isearch-no-upper-case-p` heuristic).
fn resolve_case_fold(override_val: Option<bool>, search_string: &str) -> bool {
    match override_val {
        Some(v) => v,
        None => {
            // Auto: fold if no uppercase letters in the search string.
            !search_string.chars().any(|c| c.is_uppercase())
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: build regex pattern with optional case-insensitive flag
// ---------------------------------------------------------------------------

fn build_regex_pattern(pattern: &str, case_fold: bool) -> String {
    let translated = super::regex::translate_emacs_regex(pattern);
    if case_fold {
        format!("(?i){}", translated)
    } else {
        translated
    }
}

// ---------------------------------------------------------------------------
// Helper: find_match — general-purpose substring/regex search
// ---------------------------------------------------------------------------

/// Search for `pattern` in `text`.
///
/// - `from`:    byte offset to start searching from.
/// - `forward`: direction of search.
/// - `regexp`:  treat `pattern` as an Emacs regular expression.
/// - `case_fold`: perform case-insensitive matching.
///
/// Returns `(match_start, match_end)` byte offsets into `text`, or `None`.
fn find_match(
    text: &str,
    pattern: &str,
    from: usize,
    forward: bool,
    regexp: bool,
    case_fold: bool,
) -> Option<(usize, usize)> {
    if pattern.is_empty() {
        return None;
    }

    let text_len = text.len();

    if regexp {
        if forward {
            let start = from.min(text_len);
            let iterated = super::regex::iterate_string_matches_with_case_fold(
                pattern, text, start, case_fold,
            )
            .ok()?;
            iterated
                .matches
                .into_iter()
                .find_map(|groups| groups.first().and_then(|group| *group))
        } else {
            let end = from.min(text_len);
            let iterated = super::regex::iterate_string_matches_with_case_fold(
                pattern,
                &text[..end],
                0,
                case_fold,
            )
            .ok()?;
            iterated
                .matches
                .into_iter()
                .filter_map(|groups| groups.first().and_then(|group| *group))
                .last()
        }
    } else {
        // Literal search.
        if forward {
            let start = from.min(text_len);
            let region = &text[start..];
            if case_fold {
                let hay = region.to_lowercase();
                let needle = pattern.to_lowercase();
                let pos = hay.find(&needle)?;
                Some((start + pos, start + pos + needle.len()))
            } else {
                let pos = region.find(pattern)?;
                Some((start + pos, start + pos + pattern.len()))
            }
        } else {
            let end = from.min(text_len);
            let region = &text[..end];
            if case_fold {
                let hay = region.to_lowercase();
                let needle = pattern.to_lowercase();
                let pos = hay.rfind(&needle)?;
                Some((pos, pos + needle.len()))
            } else {
                let pos = region.rfind(pattern)?;
                Some((pos, pos + pattern.len()))
            }
        }
    }
}

fn is_delimited_word_char(ch: char) -> bool {
    ch.is_alphanumeric()
}

fn is_delimited_match(text: &str, start: usize, end: usize) -> bool {
    let left = text.get(..start).and_then(|s| s.chars().next_back());
    let right = text.get(end..).and_then(|s| s.chars().next());
    let left_ok = match left {
        Some(ch) => !is_delimited_word_char(ch),
        None => true,
    };
    let right_ok = match right {
        Some(ch) => !is_delimited_word_char(ch),
        None => true,
    };
    left_ok && right_ok
}

// ---------------------------------------------------------------------------
// Helper: case-preserving replacement
// ---------------------------------------------------------------------------

/// Produce a replacement string that preserves the case pattern of the
/// matched text.
///
/// Rules (matching Emacs `replace-match` behavior):
/// - If `matched` is all-uppercase, upcase the entire replacement.
/// - If `matched` starts with an uppercase letter and the rest is lowercase
///   (capitalized), uppercase the first char of replacement and keep the rest.
/// - Otherwise return `replacement` unmodified.
fn preserve_case(replacement: &str, matched: &str) -> String {
    super::casefiddle::apply_replace_match_case(replacement, matched)
}

fn expand_emacs_replacement(rep: &str, groups: &[Option<(usize, usize)>], source: &str) -> String {
    let mut out = String::with_capacity(rep.len());
    let mut chars = rep.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }

        let Some(next) = chars.next() else {
            out.push('\\');
            break;
        };

        match next {
            '&' => {
                if let Some(Some((start, end))) = groups.first()
                    && let Some(text) = source.get(*start..*end)
                {
                    out.push_str(text);
                }
            }
            '1'..='9' => {
                let idx = next.to_digit(10).unwrap() as usize;
                if let Some(Some((start, end))) = groups.get(idx)
                    && let Some(text) = source.get(*start..*end)
                {
                    out.push_str(text);
                }
            }
            '\\' => out.push('\\'),
            other => out.push(other),
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Builtin functions (stubs for evaluator dispatch)
// ---------------------------------------------------------------------------

fn replace_string_eval_impl(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
    query_style_point: bool,
) -> EvalResult {
    expect_min_max_args("replace-string", &args, 2, 7)?;
    let from = expect_sequence_string(&args[0])?;
    let to = expect_string(&args[1])?;
    let delimited = args.get(2).is_some_and(|v| !v.is_nil());
    let backward = args.get(5).is_some_and(|v| !v.is_nil());
    let region_noncontiguous = args.get(6).is_some_and(|v| !v.is_nil());
    if region_noncontiguous && !backward {
        let point_max = {
            let buf = eval
                .buffers
                .current_buffer()
                .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
            if buf.mark().is_none() {
                return Err(signal(
                    "error",
                    vec![Value::string(
                        "The mark is not set now, so there is no region",
                    )],
                ));
            }
            buf.point_max()
        };
        let current_id = eval
            .buffers
            .current_buffer_id()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        let _ = eval.buffers.goto_buffer_byte(current_id, point_max);
        return Ok(Value::NIL);
    }
    let (start, end, source, read_only, buffer_name) = {
        let buf = eval
            .buffers
            .current_buffer()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        let (start, end) = replacement_region_bounds(
            buf,
            args.get(3),
            args.get(4),
            backward,
            region_noncontiguous,
        )?;
        (
            start,
            end,
            buf.buffer_substring(start, end),
            buffer_read_only_active(eval, buf),
            buf.name.clone(),
        )
    };

    if from.is_empty() {
        if source.is_empty() {
            return Ok(Value::NIL);
        }
        if read_only {
            return Err(signal("buffer-read-only", vec![Value::string(buffer_name)]));
        }
        let mut out = String::with_capacity(source.len() + to.len() * source.chars().count());
        if backward {
            for ch in source.chars() {
                out.push(ch);
                out.push_str(&to);
            }
        } else {
            for ch in source.chars() {
                out.push_str(&to);
                out.push(ch);
            }
        }
        if out == source {
            return Ok(Value::NIL);
        }
        let current_id = eval
            .buffers
            .current_buffer_id()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        let old_len = super::editfns::current_buffer_byte_span_char_len(eval, start, end);
        let new_len = out.len();
        super::editfns::signal_before_change(eval, start, end)?;
        let _ = eval.buffers.delete_buffer_region(current_id, start, end);
        let _ = eval.buffers.goto_buffer_byte(current_id, start);
        let _ = eval.buffers.insert_into_buffer(current_id, &out);
        super::editfns::signal_after_change(eval, start, start + new_len, old_len)?;
        if backward {
            if let Some(first) = source.chars().next() {
                let _ = eval
                    .buffers
                    .goto_buffer_byte(current_id, start + first.len_utf8());
            } else {
                let _ = eval.buffers.goto_buffer_byte(current_id, start);
            }
        } else if query_style_point {
            if let Some(last) = source.chars().last() {
                let _ = eval.buffers.goto_buffer_byte(
                    current_id,
                    start + out.len().saturating_sub(last.len_utf8()),
                );
            } else {
                let _ = eval.buffers.goto_buffer_byte(current_id, start);
            }
        } else {
            let _ = eval.buffers.goto_buffer_byte(current_id, start + out.len());
        }
        return Ok(Value::NIL);
    }

    let case_fold = case_fold_for_pattern(eval, &from);
    let preserve_match_case = case_fold && case_replace_enabled(eval);
    let lax_whitespace_regex = if replace_lax_whitespace_enabled(eval) && from.contains(' ') {
        resolve_search_whitespace_regexp(eval)
    } else {
        None
    };
    let mut out = String::with_capacity(source.len());
    let mut replaced = 0usize;
    let mut backward_point = None;
    let mut query_forward_point = None;

    if let Some(whitespace_regex) = lax_whitespace_regex {
        let pattern = build_lax_whitespace_pattern(&from, &whitespace_regex);
        let iterated =
            super::regex::iterate_string_matches_with_case_fold(&pattern, &source, 0, case_fold)
                .map_err(|e| {
                    signal(
                        "invalid-regexp",
                        vec![Value::string(format!("Invalid regexp: {e}"))],
                    )
                })?;
        let mut last = 0usize;
        for groups in iterated.matches {
            let Some((m_start, m_end)) = groups.first().and_then(|group| *group) else {
                continue;
            };
            if delimited && !is_delimited_match(&source, m_start, m_end) {
                continue;
            }
            out.push_str(&source[last..m_start]);
            let matched = &source[m_start..m_end];
            if preserve_match_case {
                out.push_str(&preserve_case(&to, matched));
            } else {
                out.push_str(&to);
            }
            query_forward_point = Some(out.len());
            if backward && backward_point.is_none() {
                backward_point = Some(m_start);
            }
            replaced += 1;
            last = m_end;
        }
        out.push_str(&source[last..]);
    } else {
        let mut cursor = 0usize;
        while let Some((m_start, m_end)) =
            find_match(&source, &from, cursor, true, false, case_fold)
        {
            if delimited && !is_delimited_match(&source, m_start, m_end) {
                out.push_str(&source[cursor..m_end]);
                cursor = m_end;
                continue;
            }
            out.push_str(&source[cursor..m_start]);
            let matched = &source[m_start..m_end];
            if preserve_match_case {
                out.push_str(&preserve_case(&to, matched));
            } else {
                out.push_str(&to);
            }
            query_forward_point = Some(out.len());
            if backward && backward_point.is_none() {
                backward_point = Some(m_start);
            }
            replaced += 1;
            cursor = m_end;
        }
        out.push_str(&source[cursor..]);
    }

    if replaced == 0 {
        return Ok(Value::NIL);
    }
    if read_only {
        return Err(signal("buffer-read-only", vec![Value::string(buffer_name)]));
    }

    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let old_len = super::editfns::current_buffer_byte_span_char_len(eval, start, end);
    let new_len = out.len();
    super::editfns::signal_before_change(eval, start, end)?;
    let _ = eval.buffers.delete_buffer_region(current_id, start, end);
    let _ = eval.buffers.goto_buffer_byte(current_id, start);
    let _ = eval.buffers.insert_into_buffer(current_id, &out);
    super::editfns::signal_after_change(eval, start, start + new_len, old_len)?;
    if backward {
        if let Some(pos) = backward_point {
            let _ = eval.buffers.goto_buffer_byte(current_id, start + pos);
        } else {
            let _ = eval.buffers.goto_buffer_byte(current_id, start);
        }
    } else if query_style_point {
        if let Some(pos) = query_forward_point {
            let _ = eval.buffers.goto_buffer_byte(current_id, start + pos);
        } else {
            let _ = eval.buffers.goto_buffer_byte(current_id, start);
        }
    } else {
        let _ = eval.buffers.goto_buffer_byte(current_id, start + out.len());
    }

    Ok(Value::NIL)
}

/// `(replace-string FROM-STRING TO-STRING &optional DELIMITED START END BACKWARD REGION-NONCONTIGUOUS-P)` —
/// evaluator-backed non-interactive replace subset.
pub(crate) fn builtin_replace_string(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    replace_string_eval_impl(eval, args, false)
}

fn replace_regexp_eval_impl(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
    query_style_point: bool,
) -> EvalResult {
    expect_min_max_args("replace-regexp", &args, 2, 7)?;
    let from = expect_sequence_string(&args[0])?;
    let to = expect_string(&args[1])?;
    let delimited = args.get(2).is_some_and(|v| !v.is_nil());
    let backward = args.get(5).is_some_and(|v| !v.is_nil());
    let region_noncontiguous = args.get(6).is_some_and(|v| !v.is_nil());
    if region_noncontiguous && !backward {
        let point_max = {
            let buf = eval
                .buffers
                .current_buffer()
                .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
            if buf.mark().is_none() {
                return Err(signal(
                    "error",
                    vec![Value::string(
                        "The mark is not set now, so there is no region",
                    )],
                ));
            }
            buf.point_max()
        };
        let current_id = eval
            .buffers
            .current_buffer_id()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        let _ = eval.buffers.goto_buffer_byte(current_id, point_max);
        return Ok(Value::NIL);
    }

    let (start, end, source, read_only, buffer_name) = {
        let buf = eval
            .buffers
            .current_buffer()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        let (start, end) = replacement_region_bounds(
            buf,
            args.get(3),
            args.get(4),
            backward,
            region_noncontiguous,
        )?;
        (
            start,
            end,
            buf.buffer_substring(start, end),
            buffer_read_only_active(eval, buf),
            buf.name.clone(),
        )
    };

    let case_fold = case_fold_for_pattern(eval, &from);
    let preserve_match_case = case_fold && case_replace_enabled(eval);
    let iterated = super::regex::iterate_string_matches_with_case_fold(
        &from, &source, 0, case_fold,
    )
    .map_err(|e| {
        signal(
            "invalid-regexp",
            vec![Value::string(format!("Invalid regexp: {e}"))],
        )
    })?;

    let mut out = String::with_capacity(source.len());
    let mut last = 0usize;
    let mut replaced = 0usize;
    let mut backward_point = None;
    let mut query_forward_point = None;
    for groups in iterated.matches {
        let Some((match_start, match_end)) = groups.first().and_then(|group| *group) else {
            continue;
        };
        if delimited && !is_delimited_match(&source, match_start, match_end) {
            continue;
        }
        if match_start == match_end {
            if backward {
                // Backward path inserts after each character and at region end, not at start.
                if match_start == 0 {
                    continue;
                }
            } else {
                // Forward path inserts before each character, not at end.
                if match_start >= source.len() {
                    continue;
                }
            }
            out.push_str(&source[last..match_start]);
            let expanded = expand_emacs_replacement(&to, &groups, &source);
            if preserve_match_case {
                out.push_str(&preserve_case(&expanded, &source[match_start..match_end]));
            } else {
                out.push_str(&expanded);
            }
            query_forward_point = Some(out.len());
            last = match_start;
            if backward && backward_point.is_none() {
                backward_point = Some(match_start);
            }
            replaced += 1;
            continue;
        }

        out.push_str(&source[last..match_start]);
        let expanded = expand_emacs_replacement(&to, &groups, &source);
        if preserve_match_case {
            out.push_str(&preserve_case(&expanded, &source[match_start..match_end]));
        } else {
            out.push_str(&expanded);
        }
        query_forward_point = Some(out.len());
        last = match_end;
        if backward && backward_point.is_none() {
            backward_point = Some(match_start);
        }
        replaced += 1;
    }
    out.push_str(&source[last..]);

    if replaced == 0 {
        return Ok(Value::NIL);
    }
    if read_only {
        return Err(signal("buffer-read-only", vec![Value::string(buffer_name)]));
    }

    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let old_len = super::editfns::current_buffer_byte_span_char_len(eval, start, end);
    let new_len = out.len();
    super::editfns::signal_before_change(eval, start, end)?;
    let _ = eval.buffers.delete_buffer_region(current_id, start, end);
    let _ = eval.buffers.goto_buffer_byte(current_id, start);
    let _ = eval.buffers.insert_into_buffer(current_id, &out);
    super::editfns::signal_after_change(eval, start, start + new_len, old_len)?;
    if backward {
        if let Some(pos) = backward_point {
            let _ = eval.buffers.goto_buffer_byte(current_id, start + pos);
        } else {
            let _ = eval.buffers.goto_buffer_byte(current_id, start);
        }
    } else if query_style_point {
        if let Some(pos) = query_forward_point {
            let _ = eval.buffers.goto_buffer_byte(current_id, start + pos);
        } else {
            let _ = eval.buffers.goto_buffer_byte(current_id, start);
        }
    } else {
        let _ = eval.buffers.goto_buffer_byte(current_id, start + out.len());
    }

    Ok(Value::NIL)
}

/// `(replace-regexp REGEXP TO-STRING &optional DELIMITED START END BACKWARD REGION-NONCONTIGUOUS-P)` —
/// evaluator-backed non-interactive regexp replacement subset.
pub(crate) fn builtin_replace_regexp(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    replace_regexp_eval_impl(eval, args, false)
}

/// `(query-replace FROM TO &optional DELIMITED START END BACKWARD REGION-NONCONTIGUOUS-P)` —
/// evaluator-backed batch-safe subset.
///
/// Current subset behavior performs unconditional replacement across the target
/// region, matching batch automation use-cases.
pub(crate) fn builtin_query_replace(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_max_args("query-replace", &args, 2, 7)?;
    replace_string_eval_impl(eval, args, true)
}

/// `(query-replace-regexp FROM TO &optional DELIMITED START END BACKWARD REGION-NONCONTIGUOUS-P)` —
/// evaluator-backed batch-safe subset.
///
/// Current subset behavior performs unconditional regexp replacement across the
/// target region, matching batch automation use-cases.
pub(crate) fn builtin_query_replace_regexp(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_max_args("query-replace-regexp", &args, 2, 7)?;
    match replace_regexp_eval_impl(eval, args, true) {
        // Batch `query-replace-regexp` does not signal invalid regexp payloads;
        // it reports and returns nil in non-interactive compatibility mode.
        Err(Flow::Signal(sig)) if sig.symbol_name() == "invalid-regexp" => Ok(Value::NIL),
        other => other,
    }
}

/// `(keep-lines REGEXP &optional RSTART REND INTERACTIVE)` —
/// evaluator-backed non-interactive line filtering subset.
pub(crate) fn builtin_keep_lines(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_max_args("keep-lines", &args, 1, 4)?;
    let regexp = expect_sequence_string(&args[0])?;

    let (point_min, start, end, source) = {
        let buf = eval
            .buffers
            .current_buffer()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        let (start, end) = line_operation_region_bounds(buf, args.get(1), args.get(2))?;
        (
            buf.point_min(),
            start,
            end,
            buf.buffer_substring(buf.point_min(), buf.point_max()),
        )
    };

    let case_fold = case_fold_for_pattern(eval, &regexp);

    let rel_start = start.saturating_sub(point_min).min(source.len());
    let rel_end = end.saturating_sub(point_min).min(source.len());
    let mut rel_cursor = line_start_at_or_before(&source, rel_start);
    let mut delete_ranges: Vec<(usize, usize)> = Vec::new();

    while rel_cursor < source.len() {
        let abs_line_start = point_min + rel_cursor;
        if abs_line_start >= point_min + rel_end {
            break;
        }

        let line_tail = &source[rel_cursor..];
        let line_len = match line_tail.find('\n') {
            Some(idx) => idx + 1,
            None => line_tail.len(),
        };
        let rel_line_end = rel_cursor + line_len;
        let line = if source.as_bytes().get(rel_line_end.wrapping_sub(1)) == Some(&b'\n') {
            &source[rel_cursor..rel_line_end - 1]
        } else {
            &source[rel_cursor..rel_line_end]
        };

        let keep_line = match string_matches_regexp(line, &regexp, case_fold) {
            Ok(matched) => matched,
            Err(Flow::Signal(sig)) if sig.symbol_name() == "invalid-regexp" => {
                return Ok(Value::NIL);
            }
            Err(err) => return Err(err),
        };
        if !keep_line {
            delete_ranges.push((point_min + rel_cursor, point_min + rel_line_end));
        }
        rel_cursor = rel_line_end;
    }

    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    if !delete_ranges.is_empty() {
        // Signal once for the whole affected region.
        let region_start = delete_ranges.last().map(|(s, _)| *s).unwrap_or(start);
        let region_end = delete_ranges.first().map(|(_, e)| *e).unwrap_or(start);
        let total_deleted: usize = delete_ranges.iter().map(|(s, e)| e - s).sum();
        let old_len =
            super::editfns::current_buffer_byte_span_char_len(eval, region_start, region_end);
        super::editfns::signal_before_change(eval, region_start, region_end)?;
        for (del_start, del_end) in delete_ranges.into_iter().rev() {
            let _ = eval
                .buffers
                .delete_buffer_region(current_id, del_start, del_end);
        }
        super::editfns::signal_after_change(
            eval,
            region_start,
            region_end - total_deleted,
            old_len,
        )?;
    }
    let _ = eval.buffers.goto_buffer_byte(current_id, start);

    Ok(Value::NIL)
}

/// `(flush-lines REGEXP &optional RSTART REND INTERACTIVE)` —
/// evaluator-backed non-interactive line filtering subset.
pub(crate) fn builtin_flush_lines(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_max_args("flush-lines", &args, 1, 4)?;
    let regexp = expect_sequence_string(&args[0])?;

    let (point_min, start, end, source) = {
        let buf = eval
            .buffers
            .current_buffer()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        let (start, end) = line_operation_region_bounds(buf, args.get(1), args.get(2))?;
        (
            buf.point_min(),
            start,
            end,
            buf.buffer_substring(buf.point_min(), buf.point_max()),
        )
    };

    let case_fold = case_fold_for_pattern(eval, &regexp);

    let rel_start = start.saturating_sub(point_min).min(source.len());
    let rel_end = end.saturating_sub(point_min).min(source.len());
    let mut rel_cursor = line_start_at_or_before(&source, rel_start);
    let mut delete_ranges: Vec<(usize, usize)> = Vec::new();

    while rel_cursor < source.len() {
        let abs_line_start = point_min + rel_cursor;
        if abs_line_start >= point_min + rel_end {
            break;
        }

        let line_tail = &source[rel_cursor..];
        let line_len = match line_tail.find('\n') {
            Some(idx) => idx + 1,
            None => line_tail.len(),
        };
        let rel_line_end = rel_cursor + line_len;
        let line = if source.as_bytes().get(rel_line_end.wrapping_sub(1)) == Some(&b'\n') {
            &source[rel_cursor..rel_line_end - 1]
        } else {
            &source[rel_cursor..rel_line_end]
        };

        if string_matches_regexp(line, &regexp, case_fold)? {
            delete_ranges.push((point_min + rel_cursor, point_min + rel_line_end));
        }
        rel_cursor = rel_line_end;
    }

    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    if !delete_ranges.is_empty() {
        let region_start = delete_ranges.first().map(|(s, _)| *s).unwrap_or(start);
        let region_end = delete_ranges.last().map(|(_, e)| *e).unwrap_or(start);
        let total_deleted: usize = delete_ranges.iter().map(|(s, e)| e - s).sum();
        let old_len =
            super::editfns::current_buffer_byte_span_char_len(eval, region_start, region_end);
        super::editfns::signal_before_change(eval, region_start, region_end)?;
        for (del_start, del_end) in delete_ranges.into_iter().rev() {
            let _ = eval
                .buffers
                .delete_buffer_region(current_id, del_start, del_end);
        }
        super::editfns::signal_after_change(
            eval,
            region_start,
            region_end - total_deleted,
            old_len,
        )?;
    }
    let _ = eval.buffers.goto_buffer_byte(current_id, start);

    // Emacs returns integer 0 from flush-lines regardless of match count.
    Ok(Value::fixnum(0))
}

/// `(how-many REGEXP &optional RSTART REND INTERACTIVE)` —
/// evaluator-backed regexp match counting subset.
pub(crate) fn builtin_how_many(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_max_args("how-many", &args, 1, 4)?;
    let regexp = expect_sequence_string(&args[0])?;

    let source = {
        let buf = eval
            .buffers
            .current_buffer()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        let (start, end) = line_operation_region_bounds(buf, args.get(1), args.get(2))?;
        buf.buffer_substring(start, end)
    };

    if regexp.is_empty() {
        return Ok(Value::fixnum(source.chars().count() as i64));
    }

    let case_fold = case_fold_for_pattern(eval, &regexp);
    Ok(Value::fixnum(count_string_regexp_matches(
        &source, &regexp, case_fold,
    )?))
}

/// `(count-matches REGEXP &optional START END INTERACTIVE)` —
/// evaluator-backed regexp match counting subset.
pub(crate) fn builtin_count_matches(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_max_args("count-matches", &args, 1, 4)?;
    let regexp = expect_sequence_string(&args[0])?;

    let source = {
        let buf = eval
            .buffers
            .current_buffer()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        let (start, end) = line_operation_region_bounds(buf, args.get(1), args.get(2))?;
        buf.buffer_substring(start, end)
    };

    if regexp.is_empty() {
        return Ok(Value::fixnum(source.chars().count() as i64));
    }

    let case_fold = case_fold_for_pattern(eval, &regexp);
    Ok(Value::fixnum(count_string_regexp_matches(
        &source, &regexp, case_fold,
    )?))
}

/// `(isearch-forward)` — interactive command; returns batch-mode error in
/// non-interactive contexts.
pub(crate) fn builtin_isearch_forward(args: Vec<Value>) -> EvalResult {
    expect_min_max_args("isearch-forward", &args, 0, 2)?;
    Err(signal(
        "error",
        vec![Value::string(
            "move-to-window-line called from unrelated buffer",
        )],
    ))
}

/// `(isearch-backward)` — interactive command; returns batch-mode error in
/// non-interactive contexts.
pub(crate) fn builtin_isearch_backward(args: Vec<Value>) -> EvalResult {
    expect_min_max_args("isearch-backward", &args, 0, 2)?;
    Err(signal(
        "error",
        vec![Value::string(
            "move-to-window-line called from unrelated buffer",
        )],
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "isearch_test.rs"]
mod tests;
