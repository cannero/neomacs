//! Pure Rust search primitives for text buffers.
//!
//! Implements the core search operations from Emacs's `search.c`:
//! - Naive (simple) string search in both directions
//! - Boyer-Moore fast literal string search with optional case folding
//! - Regex search integration via the `core::regex` engine
//! - Match data tracking (`SearchState`)
//! - LRU cache for compiled regex patterns (`RegexpCache`)
//! - Word boundary utilities
//!
//! All routines operate on byte slices (`&[u8]`) for literal matching
//! and `&str` for regex matching. No unsafe code is used.

use super::regex::{self, DefaultCharProperties, PatternBuffer, Registers};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that may occur during search operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchError {
    /// The search pattern exceeds the maximum supported length.
    PatternTooLong,
    /// The regex pattern failed to compile.
    InvalidRegex(String),
    /// A byte position was outside the valid range.
    PositionOutOfRange,
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchError::PatternTooLong => write!(f, "Pattern too long"),
            SearchError::InvalidRegex(msg) => write!(f, "Invalid regex: {}", msg),
            SearchError::PositionOutOfRange => write!(f, "Position out of range"),
        }
    }
}

impl std::error::Error for SearchError {}

// ---------------------------------------------------------------------------
// Direction
// ---------------------------------------------------------------------------

/// Direction of a search operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDirection {
    Forward,
    Backward,
}

// ---------------------------------------------------------------------------
// Match state
// ---------------------------------------------------------------------------

/// The result of a successful search, tracking match positions.
///
/// Analogous to Emacs match-data: the overall match extent plus
/// sub-group captures for regex matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchState {
    /// Byte position where the match begins.
    pub match_start: usize,
    /// Byte position where the match ends (exclusive).
    pub match_end: usize,
    /// Sub-group captures. Each entry is `Some((start, end))` for a
    /// matched group, or `None` for an unmatched optional group.
    pub groups: Vec<Option<(usize, usize)>>,
    /// Whether a match was found.
    pub matched: bool,
}

impl SearchState {
    /// Create a new `SearchState` for a successful literal match.
    fn literal(start: usize, end: usize) -> Self {
        SearchState {
            match_start: start,
            match_end: end,
            groups: Vec::new(),
            matched: true,
        }
    }

    /// Create a `SearchState` from regex `Registers`.
    fn from_registers(regs: &Registers) -> Option<Self> {
        if regs.starts[0] < 0 {
            return None;
        }
        let match_start = regs.starts[0] as usize;
        let match_end = regs.ends[0] as usize;
        let mut groups = Vec::new();
        for i in 1..regs.num_regs {
            if regs.starts[i] >= 0 {
                groups.push(Some((regs.starts[i] as usize, regs.ends[i] as usize)));
            } else {
                groups.push(None);
            }
        }
        Some(SearchState {
            match_start,
            match_end,
            groups,
            matched: true,
        })
    }
}

// ---------------------------------------------------------------------------
// Case-folding helpers
// ---------------------------------------------------------------------------

/// Canonicalise a byte to lowercase ASCII (leaves non-ASCII unchanged).
#[inline]
fn fold_byte(b: u8) -> u8 {
    if b.is_ascii_uppercase() { b + 32 } else { b }
}

// ---------------------------------------------------------------------------
// Simple (naive) search
// ---------------------------------------------------------------------------

/// Naive byte-by-byte string search.
///
/// Searches for `needle` inside `haystack` starting from `start` in the
/// given `direction`. Returns `(match_start, match_end)` byte positions
/// on success.
pub struct SimpleSearch;

impl SimpleSearch {
    /// Search for `needle` in `haystack` from position `start`.
    ///
    /// For `Forward`, `start` is the first byte position to try.
    /// For `Backward`, `start` is one past the last byte position to
    /// try (i.e., the search begins at `start - 1` and moves left).
    ///
    /// Returns `Some((match_start, match_end))` on success.
    pub fn search(
        haystack: &[u8],
        needle: &[u8],
        start: usize,
        direction: SearchDirection,
    ) -> Option<(usize, usize)> {
        if needle.is_empty() {
            return Some((start, start));
        }
        let hlen = haystack.len();
        let nlen = needle.len();
        if nlen > hlen {
            return None;
        }

        match direction {
            SearchDirection::Forward => {
                if start > hlen {
                    return None;
                }
                let last_start = hlen.saturating_sub(nlen);
                let mut pos = start;
                while pos <= last_start {
                    if haystack[pos..pos + nlen] == *needle {
                        return Some((pos, pos + nlen));
                    }
                    pos += 1;
                }
                None
            }
            SearchDirection::Backward => {
                if nlen > hlen {
                    return None;
                }
                // start is the exclusive upper bound
                let upper = if start > hlen { hlen } else { start };
                if upper < nlen {
                    return None;
                }
                let mut pos = upper - nlen;
                loop {
                    if haystack[pos..pos + nlen] == *needle {
                        return Some((pos, pos + nlen));
                    }
                    if pos == 0 {
                        break;
                    }
                    pos -= 1;
                }
                None
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Boyer-Moore search
// ---------------------------------------------------------------------------

/// Boyer-Moore string search with optional ASCII case folding.
///
/// Pre-computes the bad-character shift table for fast skipping.
/// A simplified good-suffix heuristic is also used via the
/// `stride_for_teases` approach from Emacs.
pub struct BoyerMooreSearch {
    /// The search pattern (already case-folded if `case_fold` is true).
    pattern: Vec<u8>,
    /// Bad-character shift table (256 entries, one per byte value).
    bad_char: [usize; 256],
    /// Stride to use on a false hit at the last character position
    /// (the "stride for teases" from Emacs).
    stride_for_teases: usize,
    /// Simple translation table for case-folded search. Maps each byte
    /// to its canonical form for comparison.
    simple_translate: [u8; 256],
    /// Whether case folding is active.
    case_fold: bool,
}

impl BoyerMooreSearch {
    /// Compile a Boyer-Moore searcher for `pattern`.
    ///
    /// If `case_fold` is true, ASCII case differences are ignored
    /// during matching.
    pub fn new(pattern: &[u8], case_fold: bool) -> Self {
        let pat: Vec<u8> = if case_fold {
            pattern.iter().map(|&b| fold_byte(b)).collect()
        } else {
            pattern.to_vec()
        };

        let pat_len = pat.len();

        // Initialise simple_translate to identity.
        let mut simple_translate = [0u8; 256];
        for i in 0..256 {
            simple_translate[i] = i as u8;
        }
        if case_fold {
            for i in b'A'..=b'Z' {
                simple_translate[i as usize] = i + 32;
            }
        }

        // Bad character table: default shift is the full pattern length.
        let mut bad_char = [pat_len; 256];
        let mut stride_for_teases = pat_len;

        // Forward direction table: for each position, the shift is
        // (pat_len - 1 - i) for the character at that position.
        for i in 0..pat_len {
            let b = pat[i];
            let shift = pat_len - 1 - i;
            bad_char[b as usize] = shift;

            // If case folding, also set the shift for the opposite case.
            if case_fold {
                if b.is_ascii_lowercase() {
                    bad_char[(b - 32) as usize] = shift;
                } else if b.is_ascii_uppercase() {
                    bad_char[(b + 32) as usize] = shift;
                }
            }

            // Record the stride for the last character.
            if i == pat_len - 1 {
                // The stride_for_teases is the shift the last character
                // *would have had* if it were not the last character.
                // For i == pat_len - 1, shift is 0. We want the previous
                // value for that byte.
                // This is set above for the previous occurrence.
            }
        }

        // Compute stride_for_teases: the bad-char shift of the last
        // pattern character *before* we set it to 0. Since we process
        // left to right, the table already holds the correct value before
        // the last iteration, but we have overwritten it. Re-compute:
        if pat_len > 0 {
            let last = pat[pat_len - 1];
            // Check if the last byte appears earlier in the pattern.
            let mut found = false;
            for i in 0..pat_len - 1 {
                if simple_translate[pat[i] as usize] == simple_translate[last as usize] {
                    stride_for_teases = pat_len - 1 - i;
                    found = true;
                    // Don't break: we want the rightmost earlier occurrence
                    // (smallest stride), but the last assignment wins,
                    // giving us the leftmost (largest stride). Actually
                    // we want the second-to-last occurrence, so continue
                    // to get the rightmost which gives the smallest stride.
                }
            }
            if !found {
                stride_for_teases = pat_len;
            }
        }

        BoyerMooreSearch {
            pattern: pat,
            bad_char,
            stride_for_teases,
            simple_translate,
            case_fold,
        }
    }

    /// Search forward in `text` starting from byte position `start`.
    ///
    /// Returns `Some((match_start, match_end))` on success.
    pub fn search_forward(&self, text: &[u8], start: usize) -> Option<(usize, usize)> {
        let pat = &self.pattern;
        let pat_len = pat.len();
        if pat_len == 0 {
            return Some((start, start));
        }
        let text_len = text.len();
        if start + pat_len > text_len {
            return None;
        }

        let tr = &self.simple_translate;

        // pos points at where the last pattern byte would align.
        let mut pos = start + pat_len - 1;

        while pos < text_len {
            let tb = tr[text[pos] as usize];
            let pb = tr[pat[pat_len - 1] as usize];
            if tb != pb {
                let shift = self.bad_char[text[pos] as usize];
                if shift == 0 {
                    // Shouldn't happen if table is correct, but be safe.
                    pos += 1;
                } else {
                    pos += shift;
                }
                continue;
            }

            // Last byte matches. Check the rest of the pattern.
            let mut matched = true;
            for j in (0..pat_len - 1).rev() {
                let offset = pos - (pat_len - 1) + j;
                if tr[text[offset] as usize] != tr[pat[j] as usize] {
                    matched = false;
                    break;
                }
            }

            if matched {
                let match_start = pos + 1 - pat_len;
                return Some((match_start, match_start + pat_len));
            }

            // False alarm — use stride_for_teases.
            pos += self.stride_for_teases.max(1);
        }

        None
    }

    /// Search backward in `text` from byte position `start` (exclusive upper bound).
    ///
    /// Returns `Some((match_start, match_end))` on success.
    pub fn search_backward(&self, text: &[u8], start: usize) -> Option<(usize, usize)> {
        let pat = &self.pattern;
        let pat_len = pat.len();
        if pat_len == 0 {
            return Some((start, start));
        }
        let upper = if start > text.len() {
            text.len()
        } else {
            start
        };
        if upper < pat_len {
            return None;
        }

        let tr = &self.simple_translate;

        // Scan from right to left. `pos` is the candidate match start.
        let mut pos = upper - pat_len;

        loop {
            // Quick check: does the first byte match?
            if tr[text[pos] as usize] == tr[pat[0] as usize] {
                // Check the whole pattern.
                let mut matched = true;
                for j in 1..pat_len {
                    if tr[text[pos + j] as usize] != tr[pat[j] as usize] {
                        matched = false;
                        break;
                    }
                }
                if matched {
                    return Some((pos, pos + pat_len));
                }
            }

            if pos == 0 {
                break;
            }
            pos -= 1;
        }

        None
    }
}

// ---------------------------------------------------------------------------
// TextSearcher — unified search interface
// ---------------------------------------------------------------------------

/// Unified search interface for literal and regex searches.
pub struct TextSearcher;

impl TextSearcher {
    /// Search forward for a literal byte pattern.
    ///
    /// Searches `text[start..limit]` for `pattern`.
    /// If `case_fold` is true, ASCII case differences are ignored.
    pub fn search_forward_literal(
        text: &[u8],
        pattern: &[u8],
        start: usize,
        limit: usize,
        case_fold: bool,
    ) -> Option<SearchState> {
        if pattern.is_empty() {
            return Some(SearchState::literal(start, start));
        }
        let limit = limit.min(text.len());
        if start >= limit {
            return None;
        }

        let region = &text[..limit];

        if case_fold {
            let bm = BoyerMooreSearch::new(pattern, true);
            bm.search_forward(region, start)
                .map(|(s, e)| SearchState::literal(s, e))
        } else {
            // For exact match, use Boyer-Moore for patterns >= 4 bytes,
            // naive for very short patterns.
            if pattern.len() >= 4 {
                let bm = BoyerMooreSearch::new(pattern, false);
                bm.search_forward(region, start)
                    .map(|(s, e)| SearchState::literal(s, e))
            } else {
                SimpleSearch::search(region, pattern, start, SearchDirection::Forward)
                    .map(|(s, e)| SearchState::literal(s, e))
            }
        }
    }

    /// Search backward for a literal byte pattern.
    ///
    /// Searches `text[limit..start]` for the last occurrence of `pattern`.
    /// `start` is the exclusive upper bound; `limit` is the inclusive lower bound.
    pub fn search_backward_literal(
        text: &[u8],
        pattern: &[u8],
        start: usize,
        limit: usize,
        case_fold: bool,
    ) -> Option<SearchState> {
        if pattern.is_empty() {
            return Some(SearchState::literal(start, start));
        }
        let start = start.min(text.len());
        if start <= limit {
            return None;
        }

        let region = &text[limit..start];

        if case_fold {
            let bm = BoyerMooreSearch::new(pattern, true);
            bm.search_backward(region, region.len())
                .map(|(s, e)| SearchState::literal(s + limit, e + limit))
        } else {
            if pattern.len() >= 4 {
                let bm = BoyerMooreSearch::new(pattern, false);
                bm.search_backward(region, region.len())
                    .map(|(s, e)| SearchState::literal(s + limit, e + limit))
            } else {
                SimpleSearch::search(region, pattern, region.len(), SearchDirection::Backward)
                    .map(|(s, e)| SearchState::literal(s + limit, e + limit))
            }
        }
    }

    /// Search forward for a regex pattern.
    ///
    /// Uses the Emacs-compatible regex engine from `core::regex`.
    /// Searches `text[start..limit]` (limit is clamped to text length).
    pub fn search_forward_regex(
        text: &str,
        pattern: &str,
        start: usize,
        limit: usize,
    ) -> Result<Option<SearchState>, SearchError> {
        let limit = limit.min(text.len());
        if start > limit {
            return Err(SearchError::PositionOutOfRange);
        }

        let compiled =
            regex::compile(pattern, true).map_err(|e| SearchError::InvalidRegex(e.to_string()))?;

        let range = (limit as i64) - (start as i64);
        let props = DefaultCharProperties;
        let num_regs = compiled.num_groups + 1;
        let mut regs = Registers::new(num_regs);
        let mut regs_opt: Option<&mut Registers> = Some(&mut regs);

        let result = regex::search(&compiled, text, start, range, &props, &mut regs_opt);

        match result {
            Some(_end_pos) => Ok(SearchState::from_registers(&regs)),
            None => Ok(None),
        }
    }

    /// Count non-overlapping occurrences of `pattern` in `text[start..end]`.
    ///
    /// If `case_fold` is true, ASCII case differences are ignored.
    pub fn count_matches(
        text: &[u8],
        pattern: &[u8],
        start: usize,
        end: usize,
        case_fold: bool,
    ) -> usize {
        if pattern.is_empty() {
            return 0;
        }
        let end = end.min(text.len());
        if start >= end {
            return 0;
        }

        let region = &text[..end];
        let mut count = 0usize;
        let mut pos = start;

        if case_fold {
            let bm = BoyerMooreSearch::new(pattern, true);
            while let Some((_s, e)) = bm.search_forward(region, pos) {
                count += 1;
                pos = e; // advance past the match (non-overlapping)
            }
        } else {
            let bm = BoyerMooreSearch::new(pattern, false);
            while let Some((_s, e)) = bm.search_forward(region, pos) {
                count += 1;
                pos = e;
            }
        }

        count
    }
}

// ---------------------------------------------------------------------------
// Regex cache
// ---------------------------------------------------------------------------

/// Entry in the regex cache.
struct CacheEntry {
    /// The source pattern string.
    pattern: String,
    /// The compiled pattern buffer.
    compiled: PatternBuffer,
}

/// LRU cache for compiled regex patterns.
///
/// Analogous to `searchbufs[]` in Emacs's `search.c`.
/// Stores the most recently used compiled patterns to avoid
/// redundant compilation. Uses a simple Vec-based LRU scheme.
pub struct RegexpCache {
    /// Cached entries, ordered by recency (most recent at end).
    entries: Vec<CacheEntry>,
    /// Maximum number of entries.
    capacity: usize,
}

impl RegexpCache {
    /// Create a new cache with the given capacity.
    pub fn new(capacity: usize) -> Self {
        RegexpCache {
            entries: Vec::with_capacity(capacity),
            capacity: capacity.max(1),
        }
    }

    /// Look up a pattern in the cache, compiling it if necessary.
    ///
    /// On success, returns a reference to the compiled `PatternBuffer`.
    /// The accessed entry is promoted to the most-recently-used position.
    pub fn get_or_compile(&mut self, pattern: &str) -> Result<&PatternBuffer, SearchError> {
        // Search for existing entry.
        if let Some(idx) = self.entries.iter().position(|e| e.pattern == pattern) {
            // Move to end (most recently used).
            let entry = self.entries.remove(idx);
            self.entries.push(entry);
            return self
                .entries
                .last()
                .map(|e| &e.compiled)
                .ok_or_else(|| SearchError::InvalidRegex("internal cache error".into()));
        }

        // Compile new pattern.
        let compiled =
            regex::compile(pattern, true).map_err(|e| SearchError::InvalidRegex(e.to_string()))?;

        // Evict if at capacity.
        if self.entries.len() >= self.capacity {
            self.entries.remove(0);
        }

        self.entries.push(CacheEntry {
            pattern: pattern.to_string(),
            compiled,
        });

        self.entries
            .last()
            .map(|e| &e.compiled)
            .ok_or_else(|| SearchError::InvalidRegex("internal cache error".into()))
    }

    /// Number of entries currently in the cache.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Clear all cached patterns.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The maximum capacity of the cache.
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

// ---------------------------------------------------------------------------
// Word boundary utilities
// ---------------------------------------------------------------------------

/// Check if a character is a "word character" (alphanumeric or underscore,
/// or Unicode letter/number).
#[inline]
pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Check if position `pos` in `text` is at a word boundary.
///
/// A word boundary exists between a word character and a non-word
/// character (or at the start/end of the text). The text is
/// interpreted as UTF-8.
pub fn word_boundary_p(text: &[u8], pos: usize) -> bool {
    if pos > text.len() {
        return false;
    }

    let left_is_word = if pos == 0 {
        false
    } else {
        // Look at the character ending just before `pos`.
        match char_before(text, pos) {
            Some(c) => is_word_char(c),
            None => false,
        }
    };

    let right_is_word = if pos >= text.len() {
        false
    } else {
        match char_at(text, pos) {
            Some(c) => is_word_char(c),
            None => false,
        }
    };

    left_is_word != right_is_word
}

/// Move forward or backward by `count` words in `text` from `pos`.
///
/// Positive `count` moves forward; negative moves backward.
/// Returns the new byte position.
pub fn forward_word(text: &[u8], pos: usize, count: i32) -> usize {
    if count == 0 {
        return pos;
    }

    let _text_str = match std::str::from_utf8(text) {
        Ok(s) => s,
        Err(_) => return pos, // Not valid UTF-8; bail out.
    };

    if count > 0 {
        let mut p = pos;
        for _ in 0..count {
            // Skip non-word characters.
            while p < text.len() {
                match char_at(text, p) {
                    Some(c) if !is_word_char(c) => {
                        p += c.len_utf8();
                    }
                    _ => break,
                }
            }
            // Skip word characters.
            while p < text.len() {
                match char_at(text, p) {
                    Some(c) if is_word_char(c) => {
                        p += c.len_utf8();
                    }
                    _ => break,
                }
            }
        }
        p.min(text.len())
    } else {
        let mut p = pos;
        let abs_count = (-count) as u32;
        for _ in 0..abs_count {
            // Skip non-word characters backward.
            while p > 0 {
                match char_before(text, p) {
                    Some(c) if !is_word_char(c) => {
                        p -= c.len_utf8();
                    }
                    _ => break,
                }
            }
            // Skip word characters backward.
            while p > 0 {
                match char_before(text, p) {
                    Some(c) if is_word_char(c) => {
                        p -= c.len_utf8();
                    }
                    _ => break,
                }
            }
        }
        p
    }
}

/// Decode the UTF-8 character starting at byte position `pos`.
fn char_at(text: &[u8], pos: usize) -> Option<char> {
    if pos >= text.len() {
        return None;
    }
    let remaining = &text[pos..];
    std::str::from_utf8(remaining)
        .ok()
        .and_then(|s| s.chars().next())
}

/// Decode the UTF-8 character ending just before byte position `pos`.
fn char_before(text: &[u8], pos: usize) -> Option<char> {
    if pos == 0 || pos > text.len() {
        return None;
    }
    // Walk back up to 4 bytes to find the start of the character.
    let start = if pos >= 4 { pos - 4 } else { 0 };
    let slice = &text[start..pos];
    std::str::from_utf8(slice)
        .ok()
        .and_then(|s| s.chars().next_back())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // 1. SimpleSearch — forward
    // -----------------------------------------------------------------------

    #[test]
    fn simple_forward_basic() {
        let hay = b"hello world";
        let r = SimpleSearch::search(hay, b"world", 0, SearchDirection::Forward);
        assert_eq!(r, Some((6, 11)));
    }

    #[test]
    fn simple_forward_at_start() {
        let hay = b"abcdef";
        let r = SimpleSearch::search(hay, b"abc", 0, SearchDirection::Forward);
        assert_eq!(r, Some((0, 3)));
    }

    #[test]
    fn simple_forward_at_end() {
        let hay = b"abcdef";
        let r = SimpleSearch::search(hay, b"def", 0, SearchDirection::Forward);
        assert_eq!(r, Some((3, 6)));
    }

    #[test]
    fn simple_forward_not_found() {
        let hay = b"abcdef";
        let r = SimpleSearch::search(hay, b"xyz", 0, SearchDirection::Forward);
        assert_eq!(r, None);
    }

    #[test]
    fn simple_forward_empty_needle() {
        let hay = b"abc";
        let r = SimpleSearch::search(hay, b"", 2, SearchDirection::Forward);
        assert_eq!(r, Some((2, 2)));
    }

    #[test]
    fn simple_forward_with_offset() {
        let hay = b"aabaa";
        let r = SimpleSearch::search(hay, b"a", 1, SearchDirection::Forward);
        assert_eq!(r, Some((1, 2)));
    }

    // -----------------------------------------------------------------------
    // 2. SimpleSearch — backward
    // -----------------------------------------------------------------------

    #[test]
    fn simple_backward_basic() {
        let hay = b"hello world";
        let r = SimpleSearch::search(hay, b"hello", hay.len(), SearchDirection::Backward);
        assert_eq!(r, Some((0, 5)));
    }

    #[test]
    fn simple_backward_last_occurrence() {
        let hay = b"abcabc";
        let r = SimpleSearch::search(hay, b"abc", hay.len(), SearchDirection::Backward);
        assert_eq!(r, Some((3, 6)));
    }

    #[test]
    fn simple_backward_not_found() {
        let hay = b"abcdef";
        let r = SimpleSearch::search(hay, b"xyz", hay.len(), SearchDirection::Backward);
        assert_eq!(r, None);
    }

    #[test]
    fn simple_backward_from_middle() {
        let hay = b"abcabc";
        // Search backward from position 3 (exclusive), so only "abc" at 0..3 is visible.
        let r = SimpleSearch::search(hay, b"abc", 3, SearchDirection::Backward);
        assert_eq!(r, Some((0, 3)));
    }

    // -----------------------------------------------------------------------
    // 3. BoyerMooreSearch — forward, no case fold
    // -----------------------------------------------------------------------

    #[test]
    fn bm_forward_basic() {
        let bm = BoyerMooreSearch::new(b"world", false);
        let text = b"hello world";
        assert_eq!(bm.search_forward(text, 0), Some((6, 11)));
    }

    #[test]
    fn bm_forward_at_start() {
        let bm = BoyerMooreSearch::new(b"hello", false);
        let text = b"hello world";
        assert_eq!(bm.search_forward(text, 0), Some((0, 5)));
    }

    #[test]
    fn bm_forward_not_found() {
        let bm = BoyerMooreSearch::new(b"xyz", false);
        let text = b"hello world";
        assert_eq!(bm.search_forward(text, 0), None);
    }

    #[test]
    fn bm_forward_repeated_pattern() {
        let bm = BoyerMooreSearch::new(b"ab", false);
        let text = b"ababab";
        assert_eq!(bm.search_forward(text, 0), Some((0, 2)));
        assert_eq!(bm.search_forward(text, 2), Some((2, 4)));
        assert_eq!(bm.search_forward(text, 4), Some((4, 6)));
        assert_eq!(bm.search_forward(text, 6), None);
    }

    #[test]
    fn bm_forward_single_byte() {
        let bm = BoyerMooreSearch::new(b"x", false);
        let text = b"abcxdef";
        assert_eq!(bm.search_forward(text, 0), Some((3, 4)));
    }

    // -----------------------------------------------------------------------
    // 4. BoyerMooreSearch — backward
    // -----------------------------------------------------------------------

    #[test]
    fn bm_backward_basic() {
        let bm = BoyerMooreSearch::new(b"hello", false);
        let text = b"hello world";
        assert_eq!(bm.search_backward(text, text.len()), Some((0, 5)));
    }

    #[test]
    fn bm_backward_last_occurrence() {
        let bm = BoyerMooreSearch::new(b"ab", false);
        let text = b"ababab";
        assert_eq!(bm.search_backward(text, text.len()), Some((4, 6)));
    }

    #[test]
    fn bm_backward_not_found() {
        let bm = BoyerMooreSearch::new(b"xyz", false);
        let text = b"hello world";
        assert_eq!(bm.search_backward(text, text.len()), None);
    }

    // -----------------------------------------------------------------------
    // 5. BoyerMooreSearch — case folding
    // -----------------------------------------------------------------------

    #[test]
    fn bm_case_fold_forward() {
        let bm = BoyerMooreSearch::new(b"HELLO", true);
        let text = b"say hello there";
        assert_eq!(bm.search_forward(text, 0), Some((4, 9)));
    }

    #[test]
    fn bm_case_fold_backward() {
        let bm = BoyerMooreSearch::new(b"hello", true);
        let text = b"HELLO world HELLO";
        assert_eq!(bm.search_backward(text, text.len()), Some((12, 17)));
    }

    #[test]
    fn bm_case_fold_mixed() {
        let bm = BoyerMooreSearch::new(b"HeLLo", true);
        let text = b"hElLo";
        assert_eq!(bm.search_forward(text, 0), Some((0, 5)));
    }

    #[test]
    fn bm_no_case_fold_exact() {
        let bm = BoyerMooreSearch::new(b"Hello", false);
        let text = b"hello Hello";
        // Should skip the lowercase "hello" and match "Hello".
        assert_eq!(bm.search_forward(text, 0), Some((6, 11)));
    }

    // -----------------------------------------------------------------------
    // 6. BoyerMooreSearch — empty pattern
    // -----------------------------------------------------------------------

    #[test]
    fn bm_empty_pattern() {
        let bm = BoyerMooreSearch::new(b"", false);
        assert_eq!(bm.search_forward(b"abc", 0), Some((0, 0)));
        assert_eq!(bm.search_backward(b"abc", 3), Some((3, 3)));
    }

    // -----------------------------------------------------------------------
    // 7. TextSearcher — forward literal
    // -----------------------------------------------------------------------

    #[test]
    fn text_search_forward_literal() {
        let text = b"the quick brown fox";
        let r = TextSearcher::search_forward_literal(text, b"brown", 0, text.len(), false);
        let state = r.unwrap();
        assert!(state.matched);
        assert_eq!(state.match_start, 10);
        assert_eq!(state.match_end, 15);
    }

    #[test]
    fn text_search_forward_literal_case_fold() {
        let text = b"The Quick BROWN Fox";
        let r = TextSearcher::search_forward_literal(text, b"brown", 0, text.len(), true);
        let state = r.unwrap();
        assert!(state.matched);
        assert_eq!(state.match_start, 10);
        assert_eq!(state.match_end, 15);
    }

    // -----------------------------------------------------------------------
    // 8. TextSearcher — backward literal
    // -----------------------------------------------------------------------

    #[test]
    fn text_search_backward_literal() {
        let text = b"abc abc abc";
        let r = TextSearcher::search_backward_literal(text, b"abc", text.len(), 0, false);
        let state = r.unwrap();
        assert!(state.matched);
        assert_eq!(state.match_start, 8);
        assert_eq!(state.match_end, 11);
    }

    #[test]
    fn text_search_backward_literal_case_fold() {
        let text = b"abc ABC abc";
        let r = TextSearcher::search_backward_literal(text, b"ABC", text.len(), 0, true);
        let state = r.unwrap();
        // Should find the last "abc" at position 8 (case-folded match).
        assert!(state.matched);
        assert_eq!(state.match_start, 8);
        assert_eq!(state.match_end, 11);
    }

    // -----------------------------------------------------------------------
    // 9. TextSearcher — count_matches
    // -----------------------------------------------------------------------

    #[test]
    fn text_search_count_matches() {
        let text = b"abcabcabc";
        let count = TextSearcher::count_matches(text, b"abc", 0, text.len(), false);
        assert_eq!(count, 3);
    }

    #[test]
    fn text_search_count_matches_case_fold() {
        let text = b"AbCaBcABC";
        let count = TextSearcher::count_matches(text, b"abc", 0, text.len(), true);
        assert_eq!(count, 3);
    }

    #[test]
    fn text_search_count_matches_none() {
        let text = b"abcdef";
        let count = TextSearcher::count_matches(text, b"xyz", 0, text.len(), false);
        assert_eq!(count, 0);
    }

    #[test]
    fn text_search_count_empty_pattern() {
        let text = b"abc";
        let count = TextSearcher::count_matches(text, b"", 0, text.len(), false);
        assert_eq!(count, 0);
    }

    // -----------------------------------------------------------------------
    // 10. TextSearcher — forward regex
    // -----------------------------------------------------------------------

    #[test]
    fn text_search_forward_regex_basic() {
        let text = "hello world 123";
        let r = TextSearcher::search_forward_regex(text, "world", 0, text.len());
        assert!(r.is_ok());
        let state = r.unwrap().unwrap();
        assert!(state.matched);
        assert_eq!(state.match_start, 6);
        assert_eq!(state.match_end, 11);
    }

    #[test]
    fn text_search_forward_regex_not_found() {
        let text = "hello world";
        let r = TextSearcher::search_forward_regex(text, "xyz", 0, text.len());
        assert!(r.is_ok());
        assert!(r.unwrap().is_none());
    }

    // -----------------------------------------------------------------------
    // 11. SearchState — construction
    // -----------------------------------------------------------------------

    #[test]
    fn search_state_literal() {
        let s = SearchState::literal(5, 10);
        assert!(s.matched);
        assert_eq!(s.match_start, 5);
        assert_eq!(s.match_end, 10);
        assert!(s.groups.is_empty());
    }

    // -----------------------------------------------------------------------
    // 12. RegexpCache — basic operation
    // -----------------------------------------------------------------------

    #[test]
    fn cache_basic() {
        let mut cache = RegexpCache::new(5);
        assert!(cache.is_empty());

        let r = cache.get_or_compile("hello");
        assert!(r.is_ok());
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn cache_hit() {
        let mut cache = RegexpCache::new(5);
        cache.get_or_compile("abc").unwrap();
        cache.get_or_compile("def").unwrap();

        // Second access of "abc" should be a cache hit.
        cache.get_or_compile("abc").unwrap();
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn cache_eviction() {
        let mut cache = RegexpCache::new(3);
        cache.get_or_compile("a").unwrap();
        cache.get_or_compile("b").unwrap();
        cache.get_or_compile("c").unwrap();
        assert_eq!(cache.len(), 3);

        // Adding a 4th entry should evict the oldest ("a").
        cache.get_or_compile("d").unwrap();
        assert_eq!(cache.len(), 3);

        // "a" should no longer be cached (would need recompilation).
        // We can't check directly, but the len should still be 3.
    }

    #[test]
    fn cache_lru_promotion() {
        let mut cache = RegexpCache::new(3);
        cache.get_or_compile("a").unwrap();
        cache.get_or_compile("b").unwrap();
        cache.get_or_compile("c").unwrap();

        // Access "a" to promote it.
        cache.get_or_compile("a").unwrap();

        // Now add "d" — should evict "b" (the oldest after promotion).
        cache.get_or_compile("d").unwrap();
        assert_eq!(cache.len(), 3);
    }

    #[test]
    fn cache_invalid_regex() {
        let mut cache = RegexpCache::new(5);
        let r = cache.get_or_compile("\\(unclosed");
        assert!(r.is_err());
        // Invalid patterns should not be cached.
        assert!(cache.is_empty());
    }

    #[test]
    fn cache_clear() {
        let mut cache = RegexpCache::new(5);
        cache.get_or_compile("a").unwrap();
        cache.get_or_compile("b").unwrap();
        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn cache_capacity() {
        let cache = RegexpCache::new(10);
        assert_eq!(cache.capacity(), 10);
    }

    // -----------------------------------------------------------------------
    // 13. Word boundary
    // -----------------------------------------------------------------------

    #[test]
    fn word_boundary_at_start() {
        let text = b"hello";
        assert!(word_boundary_p(text, 0)); // before 'h' (word char, no left)
    }

    #[test]
    fn word_boundary_at_end() {
        let text = b"hello";
        assert!(word_boundary_p(text, 5)); // after 'o' (word char, no right)
    }

    #[test]
    fn word_boundary_space_word() {
        let text = b"hello world";
        assert!(word_boundary_p(text, 5)); // between 'o' and ' '
        assert!(word_boundary_p(text, 6)); // between ' ' and 'w'
    }

    #[test]
    fn word_boundary_inside_word() {
        let text = b"hello";
        assert!(!word_boundary_p(text, 1)); // between 'h' and 'e'
        assert!(!word_boundary_p(text, 2)); // between 'e' and 'l'
    }

    #[test]
    fn word_boundary_inside_spaces() {
        let text = b"   ";
        assert!(!word_boundary_p(text, 1)); // between ' ' and ' '
    }

    #[test]
    fn word_boundary_underscore() {
        // Underscore is a word character.
        let text = b"a_b";
        assert!(!word_boundary_p(text, 1)); // between 'a' and '_'
        assert!(!word_boundary_p(text, 2)); // between '_' and 'b'
    }

    // -----------------------------------------------------------------------
    // 14. forward_word
    // -----------------------------------------------------------------------

    #[test]
    fn forward_word_basic() {
        let text = b"hello world foo";
        let pos = forward_word(text, 0, 1);
        assert_eq!(pos, 5); // end of "hello"
    }

    #[test]
    fn forward_word_two() {
        let text = b"hello world foo";
        let pos = forward_word(text, 0, 2);
        assert_eq!(pos, 11); // end of "world"
    }

    #[test]
    fn forward_word_from_middle() {
        let text = b"hello world";
        let pos = forward_word(text, 3, 1);
        // Starting inside "hello" (at 'l'), skip word chars to end of "hello" (5),
        // but forward_word first skips non-word, then word.
        // At pos=3 ('l'), 'l' is a word char, so the first loop (skip non-word) does nothing,
        // then skip word chars: l, o -> pos=5. Done.
        // Wait, re-read the code: forward_word skips non-word first, then word.
        // At pos=3, char is 'l' (word char), so skip non-word does nothing.
        // Then skip word: l(3), o(4) -> pos becomes 5.
        assert_eq!(pos, 5);
    }

    #[test]
    fn forward_word_backward() {
        let text = b"hello world";
        let pos = forward_word(text, 11, -1);
        assert_eq!(pos, 6); // start of "world"
    }

    #[test]
    fn forward_word_backward_two() {
        let text = b"hello world foo";
        let pos = forward_word(text, 15, -2);
        assert_eq!(pos, 6); // start of "world"
    }

    #[test]
    fn forward_word_zero() {
        let text = b"hello";
        assert_eq!(forward_word(text, 3, 0), 3);
    }

    // -----------------------------------------------------------------------
    // 15. is_word_char
    // -----------------------------------------------------------------------

    #[test]
    fn word_char_ascii() {
        assert!(is_word_char('a'));
        assert!(is_word_char('Z'));
        assert!(is_word_char('0'));
        assert!(is_word_char('_'));
        assert!(!is_word_char(' '));
        assert!(!is_word_char('.'));
        assert!(!is_word_char('-'));
    }

    #[test]
    fn word_char_unicode() {
        assert!(is_word_char('\u{00E9}')); // e-acute
        assert!(is_word_char('\u{4E16}')); // CJK character
        assert!(!is_word_char('\u{2019}')); // right single quotation mark
    }

    // -----------------------------------------------------------------------
    // 16. SearchError
    // -----------------------------------------------------------------------

    #[test]
    fn search_error_display() {
        let e = SearchError::PatternTooLong;
        assert_eq!(format!("{}", e), "Pattern too long");

        let e = SearchError::InvalidRegex("bad".into());
        assert!(format!("{}", e).contains("bad"));

        let e = SearchError::PositionOutOfRange;
        assert_eq!(format!("{}", e), "Position out of range");
    }

    // -----------------------------------------------------------------------
    // 17. Edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn search_needle_larger_than_haystack() {
        let hay = b"ab";
        assert_eq!(
            SimpleSearch::search(hay, b"abcdef", 0, SearchDirection::Forward),
            None
        );
        assert_eq!(
            SimpleSearch::search(hay, b"abcdef", hay.len(), SearchDirection::Backward),
            None
        );
    }

    #[test]
    fn bm_pattern_larger_than_text() {
        let bm = BoyerMooreSearch::new(b"longpattern", false);
        assert_eq!(bm.search_forward(b"short", 0), None);
        assert_eq!(bm.search_backward(b"short", 5), None);
    }

    #[test]
    fn search_at_exact_boundary() {
        let text = b"abcdef";
        // Forward search starting exactly where the pattern is.
        let r = SimpleSearch::search(text, b"def", 3, SearchDirection::Forward);
        assert_eq!(r, Some((3, 6)));

        // Backward search with start just past the pattern.
        let r = SimpleSearch::search(text, b"abc", 3, SearchDirection::Backward);
        assert_eq!(r, Some((0, 3)));
    }

    // -----------------------------------------------------------------------
    // 18. TextSearcher — limit bounds
    // -----------------------------------------------------------------------

    #[test]
    fn text_search_forward_respects_limit() {
        let text = b"abc abc abc";
        // Limit search to first 7 bytes: "abc abc"
        let r = TextSearcher::search_forward_literal(text, b"abc", 4, 7, false);
        assert_eq!(r, Some(SearchState::literal(4, 7)));
    }

    #[test]
    fn text_search_forward_limit_too_small() {
        let text = b"abc abc abc";
        // Limit is 2 — not enough room for "abc".
        let r = TextSearcher::search_forward_literal(text, b"abc", 0, 2, false);
        assert!(r.is_none());
    }

    #[test]
    fn text_search_backward_respects_limit() {
        let text = b"abc abc abc";
        // Search backward from position 7 with lower limit 4.
        let r = TextSearcher::search_backward_literal(text, b"abc", 7, 4, false);
        assert_eq!(r, Some(SearchState::literal(4, 7)));
    }

    // -----------------------------------------------------------------------
    // 19. BoyerMoore long patterns
    // -----------------------------------------------------------------------

    #[test]
    fn bm_long_pattern() {
        let pattern = b"the quick brown fox jumps over the lazy dog";
        let text = b">> the quick brown fox jumps over the lazy dog <<";
        let bm = BoyerMooreSearch::new(pattern, false);
        let r = bm.search_forward(text, 0);
        assert_eq!(r, Some((3, 3 + pattern.len())));
    }

    #[test]
    fn bm_long_pattern_case_fold() {
        let pattern = b"THE QUICK BROWN FOX";
        let text = b"the quick brown fox jumps";
        let bm = BoyerMooreSearch::new(pattern, true);
        let r = bm.search_forward(text, 0);
        assert_eq!(r, Some((0, 19)));
    }

    // -----------------------------------------------------------------------
    // 20. Word boundary with UTF-8 multibyte
    // -----------------------------------------------------------------------

    #[test]
    fn word_boundary_utf8() {
        // "cafe\u{0301}" = "cafe" + combining acute accent, but let's use
        // a simpler case: "a b" with a multibyte non-word char in between.
        let text = "a\u{00A0}b".as_bytes(); // non-breaking space U+00A0
        assert!(word_boundary_p(text, 0)); // before 'a'
        assert!(word_boundary_p(text, 1)); // between 'a' and NBSP
        // NBSP is U+00A0 encoded as 0xC2 0xA0 (2 bytes).
        assert!(word_boundary_p(text, 3)); // between NBSP and 'b'
        assert!(word_boundary_p(text, 4)); // after 'b'
    }

    // -----------------------------------------------------------------------
    // 21. forward_word with leading spaces
    // -----------------------------------------------------------------------

    #[test]
    fn forward_word_leading_spaces() {
        let text = b"   hello";
        let pos = forward_word(text, 0, 1);
        assert_eq!(pos, 8); // skip 3 spaces, then skip "hello"
    }

    // -----------------------------------------------------------------------
    // 22. BoyerMoore single-character pattern
    // -----------------------------------------------------------------------

    #[test]
    fn bm_single_char_case_fold() {
        let bm = BoyerMooreSearch::new(b"A", true);
        let text = b"xxaxx";
        assert_eq!(bm.search_forward(text, 0), Some((2, 3)));
    }

    // -----------------------------------------------------------------------
    // 23. TextSearcher count with partial overlap
    // -----------------------------------------------------------------------

    #[test]
    fn text_search_count_no_overlap() {
        // "aaa" with pattern "aa": non-overlapping should find 1 match.
        let text = b"aaa";
        let count = TextSearcher::count_matches(text, b"aa", 0, text.len(), false);
        assert_eq!(count, 1);
    }

    // -----------------------------------------------------------------------
    // 24. Backward search finds last match
    // -----------------------------------------------------------------------

    #[test]
    fn backward_search_last_match() {
        let text = b"xxABCxxABCxx";
        let r = TextSearcher::search_backward_literal(text, b"ABC", text.len(), 0, false);
        let state = r.unwrap();
        assert_eq!(state.match_start, 7);
        assert_eq!(state.match_end, 10);
    }

    // -----------------------------------------------------------------------
    // 25. SearchState from_registers with groups
    // -----------------------------------------------------------------------

    #[test]
    fn search_state_from_registers() {
        let mut regs = Registers::new(3);
        regs.starts[0] = 5;
        regs.ends[0] = 15;
        regs.starts[1] = 6;
        regs.ends[1] = 10;
        regs.starts[2] = -1;
        regs.ends[2] = -1;

        let state = SearchState::from_registers(&regs).unwrap();
        assert!(state.matched);
        assert_eq!(state.match_start, 5);
        assert_eq!(state.match_end, 15);
        assert_eq!(state.groups.len(), 2);
        assert_eq!(state.groups[0], Some((6, 10)));
        assert_eq!(state.groups[1], None);
    }

    #[test]
    fn search_state_from_registers_no_match() {
        let regs = Registers::new(1);
        assert!(SearchState::from_registers(&regs).is_none());
    }

    // -----------------------------------------------------------------------
    // 26. fold_byte
    // -----------------------------------------------------------------------

    #[test]
    fn test_fold_byte() {
        assert_eq!(fold_byte(b'A'), b'a');
        assert_eq!(fold_byte(b'Z'), b'z');
        assert_eq!(fold_byte(b'a'), b'a');
        assert_eq!(fold_byte(b'0'), b'0');
        assert_eq!(fold_byte(b' '), b' ');
        assert_eq!(fold_byte(0xFF), 0xFF);
    }

    // -----------------------------------------------------------------------
    // 27. BoyerMoore with all same characters
    // -----------------------------------------------------------------------

    #[test]
    fn bm_all_same_chars() {
        let bm = BoyerMooreSearch::new(b"aaa", false);
        let text = b"aaaaa";
        assert_eq!(bm.search_forward(text, 0), Some((0, 3)));
        assert_eq!(bm.search_forward(text, 1), Some((1, 4)));
        assert_eq!(bm.search_forward(text, 3), None);
    }

    // -----------------------------------------------------------------------
    // 28. Word boundary at position beyond text length
    // -----------------------------------------------------------------------

    #[test]
    fn word_boundary_out_of_range() {
        let text = b"abc";
        assert!(!word_boundary_p(text, 100));
    }

    // -----------------------------------------------------------------------
    // 29. forward_word past end of text
    // -----------------------------------------------------------------------

    #[test]
    fn forward_word_past_end() {
        let text = b"abc";
        // Trying to move 10 words forward should clamp to text end.
        let pos = forward_word(text, 0, 10);
        assert_eq!(pos, 3);
    }

    // -----------------------------------------------------------------------
    // 30. TextSearcher forward literal — short pattern uses SimpleSearch
    // -----------------------------------------------------------------------

    #[test]
    fn text_search_short_pattern_exact() {
        let text = b"aXbXcXd";
        let r = TextSearcher::search_forward_literal(text, b"X", 0, text.len(), false);
        let state = r.unwrap();
        assert_eq!(state.match_start, 1);
        assert_eq!(state.match_end, 2);
    }
}
