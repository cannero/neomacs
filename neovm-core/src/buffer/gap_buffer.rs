//! A NeoVM storage-aware gap buffer for efficient text editing.
//!
//! The gap buffer stores text in a contiguous `Vec<u8>` with a movable "gap"
//! (unused region) that makes insertions and deletions near the gap O(1)
//! amortized. The gap is relocated to the edit site before each mutation so
//! that sequential edits in the same neighborhood avoid large copies.
//!
//! All public positions are **byte** positions into the logical text (i.e. the
//! text with the gap removed) unless a parameter is explicitly named
//! `char_pos`. Byte/character conversions follow NeoVM's internal storage
//! format, including sentinel sequences for non-Unicode Emacs characters.

use std::fmt;

/// Default initial gap size in bytes.
const DEFAULT_GAP_SIZE: usize = 64;

/// Growth factor when the gap must be expanded — the new gap will be at least
/// this many bytes, or the requested size, whichever is larger.
const MIN_GAP_GROW: usize = 64;

/// A gap buffer holding NeoVM internal string storage.
///
/// Internally the backing store looks like:
///
/// ```text
///  [ text-before-gap | gap (unused) | text-after-gap ]
///    0..gap_start      gap_start..gap_end  gap_end..buf.len()
/// ```
///
/// The *logical* text is the concatenation of `buf[..gap_start]` and
/// `buf[gap_end..]`.
#[derive(Clone)]
pub struct GapBuffer {
    /// Raw backing store.
    buf: Vec<u8>,
    /// Byte index where the gap begins (first unused byte).
    gap_start: usize,
    /// Byte index one past the last gap byte (first byte of text after gap).
    gap_end: usize,
    /// Number of logical Emacs characters before the gap.
    gap_start_chars: usize,
    /// Number of logical Emacs characters in the buffer.
    total_chars: usize,
    /// Number of logical Emacs bytes before the gap.
    gap_start_bytes: usize,
    /// Number of logical Emacs bytes in the buffer.
    total_bytes: usize,
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

impl GapBuffer {
    /// Create an empty gap buffer with a default-sized gap.
    pub fn new() -> Self {
        Self {
            buf: vec![0u8; DEFAULT_GAP_SIZE],
            gap_start: 0,
            gap_end: DEFAULT_GAP_SIZE,
            gap_start_chars: 0,
            total_chars: 0,
            gap_start_bytes: 0,
            total_bytes: 0,
        }
    }

    /// Create a gap buffer pre-loaded with the contents of `s`.
    pub fn from_str(s: &str) -> Self {
        let text = s.as_bytes();
        let gap = DEFAULT_GAP_SIZE;
        let char_count = crate::emacs_core::string_escape::storage_char_len(s);
        let byte_count = crate::emacs_core::string_escape::storage_byte_len(s);
        let mut buf = Vec::with_capacity(text.len() + gap);
        buf.extend_from_slice(text);
        buf.resize(text.len() + gap, 0);
        // Place gap at the end so that appending is cheap.
        Self {
            buf,
            gap_start: text.len(),
            gap_end: text.len() + gap,
            gap_start_chars: char_count,
            total_chars: char_count,
            gap_start_bytes: byte_count,
            total_bytes: byte_count,
        }
    }

    // -----------------------------------------------------------------------
    // Queries
    // -----------------------------------------------------------------------

    /// Total length of the logical text in **bytes** (excluding the gap).
    #[inline]
    pub fn len(&self) -> usize {
        self.buf.len() - self.gap_size()
    }

    /// Whether the buffer contains no text.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Number of logical Emacs characters in the buffer storage.
    pub fn char_count(&self) -> usize {
        self.total_chars
    }

    /// Number of logical Emacs bytes in the buffer.
    pub fn emacs_byte_len(&self) -> usize {
        self.total_bytes
    }

    /// Size of the gap in bytes.
    #[inline]
    pub fn gap_size(&self) -> usize {
        self.gap_end - self.gap_start
    }

    // -----------------------------------------------------------------------
    // Single-element access
    // -----------------------------------------------------------------------

    /// Return the byte at logical position `pos`.
    ///
    /// # Panics
    ///
    /// Panics if `pos >= self.len()`.
    pub fn byte_at(&self, pos: usize) -> u8 {
        assert!(
            pos < self.len(),
            "byte_at: position {pos} out of range (len {})",
            self.len()
        );
        if pos < self.gap_start {
            self.buf[pos]
        } else {
            self.buf[pos + self.gap_size()]
        }
    }

    /// Return the logical Emacs byte at `pos`, or `None` if out of range.
    pub fn emacs_byte_at(&self, pos: usize) -> Option<u8> {
        if pos >= self.total_bytes {
            return None;
        }

        if pos < self.gap_start_bytes {
            let text = storage_slice_to_str(&self.buf[..self.gap_start], "emacs_byte_at pre-gap");
            return crate::emacs_core::string_escape::storage_logical_byte_at(text, pos);
        }

        let rel_pos = pos - self.gap_start_bytes;
        let text = storage_slice_to_str(&self.buf[self.gap_end..], "emacs_byte_at post-gap");
        crate::emacs_core::string_escape::storage_logical_byte_at(text, rel_pos)
    }

    /// Return the `char` whose first byte starts at logical byte position `pos`,
    /// or `None` if `pos >= self.len()`.
    ///
    /// # Panics
    ///
    /// Panics if `pos` does not lie on a UTF-8 character boundary.
    pub fn char_at(&self, pos: usize) -> Option<char> {
        if pos >= self.len() {
            return None;
        }
        // Decode the character spanning up to 4 bytes starting at `pos`.
        let first = self.byte_at(pos);
        let char_len = utf8_char_len(first);
        assert!(
            pos + char_len <= self.len(),
            "char_at: incomplete UTF-8 sequence at byte position {pos}"
        );
        let mut tmp = [0u8; 4];
        for i in 0..char_len {
            tmp[i] = self.byte_at(pos + i);
        }
        let s = std::str::from_utf8(&tmp[..char_len]).expect("char_at: invalid UTF-8 sequence");
        s.chars().next()
    }

    /// Return the Emacs character code whose first storage byte begins at
    /// logical byte position `pos`, or `None` if `pos >= self.len()`.
    pub fn char_code_at(&self, pos: usize) -> Option<u32> {
        if pos >= self.len() {
            return None;
        }
        assert!(
            self.is_char_boundary(pos),
            "char_code_at: byte position {pos} is not a storage character boundary"
        );
        if pos < self.gap_start {
            let text = storage_slice_to_str(&self.buf[..self.gap_start], "char_code_at pre-gap");
            let char_idx = crate::emacs_core::string_escape::storage_byte_to_char(text, pos);
            return crate::emacs_core::string_escape::decode_storage_char_codes(text)
                .get(char_idx)
                .copied();
        }

        let rel_pos = pos - self.gap_start;
        let text = storage_slice_to_str(&self.buf[self.gap_end..], "char_code_at post-gap");
        let char_idx = crate::emacs_core::string_escape::storage_byte_to_char(text, rel_pos);
        crate::emacs_core::string_escape::decode_storage_char_codes(text)
            .get(char_idx)
            .copied()
    }

    /// Convert a character position to a logical Emacs byte offset.
    pub fn char_to_emacs_byte(&self, char_pos: usize) -> usize {
        if char_pos == 0 {
            return 0;
        }

        if char_pos <= self.gap_start_chars {
            let text =
                storage_slice_to_str(&self.buf[..self.gap_start], "char_to_emacs_byte pre-gap");
            return crate::emacs_core::string_escape::storage_char_to_logical_byte(text, char_pos);
        }

        if char_pos <= self.total_chars {
            let text =
                storage_slice_to_str(&self.buf[self.gap_end..], "char_to_emacs_byte post-gap");
            return self.gap_start_bytes
                + crate::emacs_core::string_escape::storage_char_to_logical_byte(
                    text,
                    char_pos - self.gap_start_chars,
                );
        }

        tracing::debug!(
            "char_to_emacs_byte: char_pos ({char_pos}) exceeds char_count ({}), clamping",
            self.total_chars
        );
        self.total_bytes
    }

    /// Convert a logical Emacs byte offset to a character position.
    pub fn emacs_byte_to_char(&self, byte_pos: usize) -> usize {
        assert!(
            byte_pos <= self.total_bytes,
            "emacs_byte_to_char: byte_pos ({byte_pos}) > len ({})",
            self.total_bytes
        );
        if byte_pos <= self.gap_start_bytes {
            let text =
                storage_slice_to_str(&self.buf[..self.gap_start], "emacs_byte_to_char pre-gap");
            return crate::emacs_core::string_escape::storage_logical_byte_to_char(text, byte_pos);
        }

        let rel_pos = byte_pos - self.gap_start_bytes;
        let text = storage_slice_to_str(&self.buf[self.gap_end..], "emacs_byte_to_char post-gap");
        self.gap_start_chars
            + crate::emacs_core::string_escape::storage_logical_byte_to_char(text, rel_pos)
    }

    /// Convert a storage-byte boundary to the corresponding logical Emacs byte offset.
    pub fn storage_byte_to_emacs_byte(&self, byte_pos: usize) -> usize {
        assert!(
            byte_pos <= self.len(),
            "storage_byte_to_emacs_byte: byte_pos ({byte_pos}) > len ({})",
            self.len()
        );
        if byte_pos <= self.gap_start {
            let text = storage_slice_to_str(
                &self.buf[..self.gap_start],
                "storage_byte_to_emacs_byte pre-gap",
            );
            return crate::emacs_core::string_escape::storage_byte_to_logical_byte(text, byte_pos);
        }

        let rel_pos = byte_pos - self.gap_start;
        let text = storage_slice_to_str(
            &self.buf[self.gap_end..],
            "storage_byte_to_emacs_byte post-gap",
        );
        self.gap_start_bytes
            + crate::emacs_core::string_escape::storage_byte_to_logical_byte(text, rel_pos)
    }

    // -----------------------------------------------------------------------
    // Range extraction
    // -----------------------------------------------------------------------

    /// Extract the text in the logical byte range `[start, end)` as a `String`.
    ///
    /// # Panics
    ///
    /// Panics if `start > end` or `end > self.len()`.
    pub fn text_range(&self, start: usize, end: usize) -> String {
        assert!(start <= end, "text_range: start ({start}) > end ({end})");
        assert!(
            end <= self.len(),
            "text_range: end ({end}) > len ({})",
            self.len()
        );
        if start == end {
            return String::new();
        }

        let mut out = Vec::with_capacity(end - start);

        // The logical text is split into two physical segments:
        //   segment A: buf[0 .. gap_start]       (logical positions 0 .. gap_start)
        //   segment B: buf[gap_end .. buf.len()]  (logical positions gap_start .. len)
        //
        // We need to copy the intersection of [start, end) with each segment.

        // Intersection with segment A (logical 0..gap_start).
        if start < self.gap_start {
            let seg_end = end.min(self.gap_start);
            out.extend_from_slice(&self.buf[start..seg_end]);
        }

        // Intersection with segment B (logical gap_start..len).
        if end > self.gap_start {
            let seg_start = start.max(self.gap_start);
            let phys_start = seg_start + self.gap_size();
            let phys_end = end + self.gap_size();
            out.extend_from_slice(&self.buf[phys_start..phys_end]);
        }

        String::from_utf8(out).expect("text_range: extracted bytes are not valid UTF-8")
    }

    /// Copy bytes in the logical range `[start, end)` into `out`.
    ///
    /// `out` is cleared first, then the bytes are appended.
    /// This is more efficient than `text_range()` when you don't need
    /// a `String` — it avoids the UTF-8 validation and String allocation.
    ///
    /// # Panics
    /// Panics if `start > end` or `end > self.len()`.
    pub fn copy_bytes_to(&self, start: usize, end: usize, out: &mut Vec<u8>) {
        assert!(start <= end, "copy_bytes_to: start ({start}) > end ({end})");
        assert!(
            end <= self.len(),
            "copy_bytes_to: end ({end}) > len ({})",
            self.len()
        );
        out.clear();
        if start == end {
            return;
        }
        out.reserve(end - start);

        // Intersection with segment A (logical 0..gap_start).
        if start < self.gap_start {
            let seg_end = end.min(self.gap_start);
            out.extend_from_slice(&self.buf[start..seg_end]);
        }

        // Intersection with segment B (logical gap_start..len).
        if end > self.gap_start {
            let seg_start = start.max(self.gap_start);
            let phys_start = seg_start + self.gap_size();
            let phys_end = end + self.gap_size();
            out.extend_from_slice(&self.buf[phys_start..phys_end]);
        }
    }

    /// Return the full buffer contents as a `String`.
    pub fn to_string(&self) -> String {
        self.text_range(0, self.len())
    }

    // -----------------------------------------------------------------------
    // Mutation
    // -----------------------------------------------------------------------

    /// Insert `s` at logical byte position `pos`.
    ///
    /// After the call the gap sits immediately after the newly inserted text,
    /// so consecutive inserts at the same position are fast.
    ///
    /// # Panics
    ///
    /// Panics if `pos > self.len()` or `pos` is not on a UTF-8 boundary.
    pub fn insert_str(&mut self, pos: usize, s: &str) {
        assert!(
            pos <= self.len(),
            "insert_str: position {pos} out of range (len {})",
            self.len()
        );
        if s.is_empty() {
            return;
        }
        // Validate that `pos` falls on a char boundary (unless at the very end).
        debug_assert!(
            pos == self.len() || self.is_char_boundary(pos),
            "insert_str: position {pos} is not on a UTF-8 character boundary"
        );

        let bytes = s.as_bytes();
        let inserted_chars = crate::emacs_core::string_escape::storage_char_len(s);
        let inserted_bytes = crate::emacs_core::string_escape::storage_byte_len(s);
        self.move_gap_to(pos);
        self.ensure_gap(bytes.len());

        // Copy the new text into the gap.
        self.buf[self.gap_start..self.gap_start + bytes.len()].copy_from_slice(bytes);
        self.gap_start += bytes.len();
        self.gap_start_chars += inserted_chars;
        self.total_chars += inserted_chars;
        self.gap_start_bytes += inserted_bytes;
        self.total_bytes += inserted_bytes;
    }

    /// Delete the logical byte range `[start, end)`.
    ///
    /// # Panics
    ///
    /// Panics if `start > end`, `end > self.len()`, or either boundary is not
    /// on a UTF-8 character boundary.
    pub fn delete_range(&mut self, start: usize, end: usize) {
        assert!(start <= end, "delete_range: start ({start}) > end ({end})");
        assert!(
            end <= self.len(),
            "delete_range: end ({end}) > len ({})",
            self.len()
        );
        if start == end {
            return;
        }
        debug_assert!(
            self.is_char_boundary(start),
            "delete_range: start ({start}) is not on a UTF-8 character boundary"
        );
        debug_assert!(
            end == self.len() || self.is_char_boundary(end),
            "delete_range: end ({end}) is not on a UTF-8 character boundary"
        );

        // Move the gap so that it starts at `start`, then extend it to swallow
        // the bytes up to `end`.
        self.move_gap_to(start);
        let deleted_chars = storage_char_len_bytes(
            &self.buf[self.gap_end..self.gap_end + (end - start)],
            "delete_range",
        );
        let deleted_bytes = storage_byte_len_bytes(
            &self.buf[self.gap_end..self.gap_end + (end - start)],
            "delete_range",
        );
        // After move_gap_to(start), gap_start == start and the bytes that were
        // logically at [start, end) now sit at buf[gap_end .. gap_end + (end - start)].
        self.gap_end += end - start;
        self.total_chars -= deleted_chars;
        self.total_bytes -= deleted_bytes;
    }

    /// Overwrite the logical byte range `[start, end)` with `replacement`.
    ///
    /// `replacement` must have the exact same byte length as the replaced
    /// region, and both boundaries must lie on UTF-8 character boundaries.
    pub fn replace_same_len_range(&mut self, start: usize, end: usize, replacement: &str) {
        assert!(
            start <= end,
            "replace_same_len_range: start ({start}) > end ({end})"
        );
        assert!(
            end <= self.len(),
            "replace_same_len_range: end ({end}) > len ({})",
            self.len()
        );
        assert_eq!(
            replacement.len(),
            end - start,
            "replace_same_len_range: replacement byte length ({}) must match replaced length ({})",
            replacement.len(),
            end - start
        );
        if start == end {
            return;
        }
        debug_assert!(
            self.is_char_boundary(start),
            "replace_same_len_range: start ({start}) is not on a UTF-8 character boundary"
        );
        debug_assert!(
            end == self.len() || self.is_char_boundary(end),
            "replace_same_len_range: end ({end}) is not on a UTF-8 character boundary"
        );

        self.move_gap_to(end);
        let old_chars = storage_char_len_bytes(&self.buf[start..end], "replace_same_len_range old");
        let new_chars = crate::emacs_core::string_escape::storage_char_len(replacement);
        let old_bytes = storage_byte_len_bytes(&self.buf[start..end], "replace_same_len_range old");
        let new_bytes = crate::emacs_core::string_escape::storage_byte_len(replacement);
        self.buf[start..end].copy_from_slice(replacement.as_bytes());
        if old_chars != new_chars {
            let delta = new_chars as isize - old_chars as isize;
            self.gap_start_chars = self.gap_start_chars.saturating_add_signed(delta);
            self.total_chars = self.total_chars.saturating_add_signed(delta);
        }
        if old_bytes != new_bytes {
            let delta = new_bytes as isize - old_bytes as isize;
            self.gap_start_bytes = self.gap_start_bytes.saturating_add_signed(delta);
            self.total_bytes = self.total_bytes.saturating_add_signed(delta);
        }
    }

    // -----------------------------------------------------------------------
    // Gap management
    // -----------------------------------------------------------------------

    /// Move the gap so that `gap_start == pos`.
    ///
    /// This copies text bytes across the gap to reposition it.
    ///
    /// # Panics
    ///
    /// Panics if `pos > self.len()`.
    pub fn move_gap_to(&mut self, pos: usize) {
        assert!(
            pos <= self.len(),
            "move_gap_to: position {pos} out of range (len {})",
            self.len()
        );

        if pos == self.gap_start {
            return;
        }

        let gap = self.gap_size();

        if pos < self.gap_start {
            // Moving gap left: shift buf[pos..gap_start] to the right by `gap`.
            let count = self.gap_start - pos;
            let moved_chars =
                storage_char_len_bytes(&self.buf[pos..self.gap_start], "move_gap_to left");
            let moved_bytes =
                storage_byte_len_bytes(&self.buf[pos..self.gap_start], "move_gap_to left");
            // Use copy_within which handles overlapping regions.
            self.buf.copy_within(pos..pos + count, pos + gap);
            self.gap_start = pos;
            self.gap_end = pos + gap;
            self.gap_start_chars -= moved_chars;
            self.gap_start_bytes -= moved_bytes;
        } else {
            // Moving gap right: shift buf[gap_end..gap_end + (pos - gap_start)]
            // to the left by `gap`.
            let count = pos - self.gap_start;
            let src_start = self.gap_end;
            let dst_start = self.gap_start;
            let moved_chars = storage_char_len_bytes(
                &self.buf[src_start..src_start + count],
                "move_gap_to right",
            );
            let moved_bytes = storage_byte_len_bytes(
                &self.buf[src_start..src_start + count],
                "move_gap_to right",
            );
            self.buf
                .copy_within(src_start..src_start + count, dst_start);
            self.gap_start = pos;
            self.gap_end = pos + gap;
            self.gap_start_chars += moved_chars;
            self.gap_start_bytes += moved_bytes;
        }
    }

    /// Ensure the gap is at least `min_size` bytes. If it is already large
    /// enough this is a no-op; otherwise the backing buffer is reallocated.
    pub fn ensure_gap(&mut self, min_size: usize) {
        if self.gap_size() >= min_size {
            return;
        }
        let grow = (min_size - self.gap_size()).max(MIN_GAP_GROW);
        let old_gap_end = self.gap_end;
        let after_gap_len = self.buf.len() - old_gap_end;

        // Extend the backing buffer.
        self.buf.resize(self.buf.len() + grow, 0);

        // Shift the post-gap segment to the right by `grow` to widen the gap.
        if after_gap_len > 0 {
            self.buf
                .copy_within(old_gap_end..old_gap_end + after_gap_len, old_gap_end + grow);
        }
        self.gap_end += grow;
    }

    // -----------------------------------------------------------------------
    // Position conversion
    // -----------------------------------------------------------------------

    /// Convert a logical byte position to a logical character position.
    ///
    /// Returns the number of complete characters before `byte_pos`.
    ///
    /// # Panics
    ///
    /// Panics if `byte_pos > self.len()` or is not on a storage character
    /// boundary.
    pub fn byte_to_char(&self, byte_pos: usize) -> usize {
        assert!(
            byte_pos <= self.len(),
            "byte_to_char: byte_pos ({byte_pos}) > len ({})",
            self.len()
        );
        if byte_pos <= self.gap_start {
            let text = storage_slice_to_str(&self.buf[..self.gap_start], "byte_to_char pre-gap");
            assert!(
                byte_pos
                    == crate::emacs_core::string_escape::storage_char_to_byte(
                        text,
                        crate::emacs_core::string_escape::storage_byte_to_char(text, byte_pos),
                    ),
                "byte_to_char: byte_pos ({byte_pos}) is not a storage character boundary"
            );
            return crate::emacs_core::string_escape::storage_byte_to_char(text, byte_pos);
        }

        let rel_pos = byte_pos - self.gap_start;
        let text = storage_slice_to_str(&self.buf[self.gap_end..], "byte_to_char post-gap");
        assert!(
            rel_pos
                == crate::emacs_core::string_escape::storage_char_to_byte(
                    text,
                    crate::emacs_core::string_escape::storage_byte_to_char(text, rel_pos),
                ),
            "byte_to_char: byte_pos ({byte_pos}) is not a storage character boundary"
        );
        self.gap_start_chars + crate::emacs_core::string_escape::storage_byte_to_char(text, rel_pos)
    }

    /// Convert a char position to a logical byte position.
    ///
    /// `char_pos` is the number of characters from the start of the buffer.
    ///
    /// # Panics
    ///
    /// Panics if `char_pos > self.char_count()`.
    pub fn char_to_byte(&self, char_pos: usize) -> usize {
        if char_pos == 0 {
            return 0;
        }

        if char_pos <= self.gap_start_chars {
            let text = storage_slice_to_str(&self.buf[..self.gap_start], "char_to_byte pre-gap");
            return crate::emacs_core::string_escape::storage_char_to_byte(text, char_pos);
        }

        if char_pos <= self.total_chars {
            let text = storage_slice_to_str(&self.buf[self.gap_end..], "char_to_byte post-gap");
            return self.gap_start
                + crate::emacs_core::string_escape::storage_char_to_byte(
                    text,
                    char_pos - self.gap_start_chars,
                );
        }

        // Clamp to end of buffer instead of panicking — this can happen
        // when window_start / point are stale after buffer modification.
        tracing::debug!(
            "char_to_byte: char_pos ({char_pos}) exceeds char_count ({}), clamping",
            self.total_chars
        );
        return self.len();
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Check whether `pos` falls on a logical storage-character boundary in the
    /// text.
    fn is_char_boundary(&self, pos: usize) -> bool {
        if pos == 0 || pos >= self.len() {
            return true;
        }
        if pos < self.gap_start {
            let text =
                storage_slice_to_str(&self.buf[..self.gap_start], "is_char_boundary pre-gap");
            let char_idx = crate::emacs_core::string_escape::storage_byte_to_char(text, pos);
            return crate::emacs_core::string_escape::storage_char_to_byte(text, char_idx) == pos;
        }

        let rel_pos = pos - self.gap_start;
        let text = storage_slice_to_str(&self.buf[self.gap_end..], "is_char_boundary post-gap");
        let char_idx = crate::emacs_core::string_escape::storage_byte_to_char(text, rel_pos);
        crate::emacs_core::string_escape::storage_char_to_byte(text, char_idx) == rel_pos
    }

    // pdump accessors
    /// Extract the logical text content as a byte vector (for pdump).
    pub(crate) fn dump_text(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.len());
        out.extend_from_slice(&self.buf[..self.gap_start]);
        out.extend_from_slice(&self.buf[self.gap_end..]);
        out
    }
    /// Reconstruct from text bytes (for pdump load).
    pub(crate) fn from_dump(text: Vec<u8>) -> Self {
        let len = text.len();
        let char_count = storage_char_len_bytes(&text, "from_dump");
        let byte_count = storage_byte_len_bytes(&text, "from_dump");
        Self {
            buf: text,
            gap_start: len,
            gap_end: len,
            gap_start_chars: char_count,
            total_chars: char_count,
            gap_start_bytes: byte_count,
            total_bytes: byte_count,
        }
    }
}

// ---------------------------------------------------------------------------
// Trait implementations
// ---------------------------------------------------------------------------

impl Default for GapBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for GapBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_string())
    }
}

impl fmt::Debug for GapBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GapBuffer")
            .field("len", &self.len())
            .field("char_count", &self.total_chars)
            .field("gap_start", &self.gap_start)
            .field("gap_start_chars", &self.gap_start_chars)
            .field("gap_start_bytes", &self.gap_start_bytes)
            .field("gap_end", &self.gap_end)
            .field("gap_size", &self.gap_size())
            .field("emacs_byte_len", &self.total_bytes)
            .field("text", &self.to_string())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Free helper functions
// ---------------------------------------------------------------------------

/// Return the byte length of a UTF-8 character given its first byte.
///
/// Panics on an invalid leading byte (continuation byte or 0xFF/0xFE).
#[inline]
fn utf8_char_len(first_byte: u8) -> usize {
    match first_byte {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => panic!("utf8_char_len: invalid UTF-8 leading byte 0x{first_byte:02X}"),
    }
}

/// Returns `true` if `b` is a valid start byte (not a continuation byte).
#[inline]
fn is_utf8_start_byte(b: u8) -> bool {
    // Continuation bytes are 0x80..=0xBF (bit pattern 10xxxxxx).
    (b & 0xC0) != 0x80
}

#[inline]
fn storage_slice_to_str<'a>(bytes: &'a [u8], context: &str) -> &'a str {
    std::str::from_utf8(bytes)
        .unwrap_or_else(|_| panic!("{context}: storage bytes are not valid UTF-8"))
}

#[inline]
fn storage_char_len_bytes(bytes: &[u8], context: &str) -> usize {
    crate::emacs_core::string_escape::storage_char_len(storage_slice_to_str(bytes, context))
}

#[inline]
fn storage_byte_len_bytes(bytes: &[u8], context: &str) -> usize {
    crate::emacs_core::string_escape::storage_byte_len(storage_slice_to_str(bytes, context))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Construction & basic queries
    // -----------------------------------------------------------------------

    #[test]
    fn new_buffer_is_empty() {
        crate::test_utils::init_test_tracing();
        let buf = GapBuffer::new();
        assert_eq!(buf.len(), 0);
        assert!(buf.is_empty());
        assert_eq!(buf.char_count(), 0);
        assert_eq!(buf.to_string(), "");
    }

    #[test]
    fn from_str_ascii() {
        crate::test_utils::init_test_tracing();
        let buf = GapBuffer::from_str("hello");
        assert_eq!(buf.len(), 5);
        assert_eq!(buf.char_count(), 5);
        assert_eq!(buf.to_string(), "hello");
    }

    #[test]
    fn from_str_empty() {
        crate::test_utils::init_test_tracing();
        let buf = GapBuffer::from_str("");
        assert_eq!(buf.len(), 0);
        assert!(buf.is_empty());
        assert_eq!(buf.to_string(), "");
    }

    // -----------------------------------------------------------------------
    // insert_str
    // -----------------------------------------------------------------------

    #[test]
    fn insert_at_beginning() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("world");
        buf.insert_str(0, "hello ");
        assert_eq!(buf.to_string(), "hello world");
    }

    #[test]
    fn insert_at_end() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("hello");
        buf.insert_str(5, " world");
        assert_eq!(buf.to_string(), "hello world");
    }

    #[test]
    fn insert_in_middle() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("helo");
        buf.insert_str(2, "l");
        assert_eq!(buf.to_string(), "hello");
    }

    #[test]
    fn insert_into_empty_buffer() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::new();
        buf.insert_str(0, "abc");
        assert_eq!(buf.to_string(), "abc");
        assert_eq!(buf.len(), 3);
    }

    #[test]
    fn insert_empty_string_is_noop() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("hello");
        buf.insert_str(3, "");
        assert_eq!(buf.to_string(), "hello");
    }

    #[test]
    fn multiple_sequential_inserts() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::new();
        buf.insert_str(0, "a");
        buf.insert_str(1, "b");
        buf.insert_str(2, "c");
        buf.insert_str(3, "d");
        assert_eq!(buf.to_string(), "abcd");
    }

    #[test]
    fn insert_larger_than_gap() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::new();
        let long = "x".repeat(256);
        buf.insert_str(0, &long);
        assert_eq!(buf.to_string(), long);
        assert_eq!(buf.len(), 256);
    }

    // -----------------------------------------------------------------------
    // delete_range
    // -----------------------------------------------------------------------

    #[test]
    fn delete_from_beginning() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("hello world");
        buf.delete_range(0, 6);
        assert_eq!(buf.to_string(), "world");
    }

    #[test]
    fn delete_from_end() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("hello world");
        buf.delete_range(5, 11);
        assert_eq!(buf.to_string(), "hello");
    }

    #[test]
    fn delete_from_middle() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("hello world");
        buf.delete_range(5, 6); // delete the space
        assert_eq!(buf.to_string(), "helloworld");
    }

    #[test]
    fn delete_everything() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("hello");
        buf.delete_range(0, 5);
        assert_eq!(buf.to_string(), "");
        assert!(buf.is_empty());
    }

    #[test]
    fn delete_empty_range_is_noop() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("hello");
        buf.delete_range(2, 2);
        assert_eq!(buf.to_string(), "hello");
    }

    #[test]
    fn delete_then_insert() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("hello world");
        buf.delete_range(5, 11);
        buf.insert_str(5, " rust");
        assert_eq!(buf.to_string(), "hello rust");
    }

    // -----------------------------------------------------------------------
    // byte_at / char_at
    // -----------------------------------------------------------------------

    #[test]
    fn byte_at_ascii() {
        crate::test_utils::init_test_tracing();
        let buf = GapBuffer::from_str("abcde");
        assert_eq!(buf.byte_at(0), b'a');
        assert_eq!(buf.byte_at(4), b'e');
    }

    #[test]
    fn byte_at_after_gap_move() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("abcde");
        buf.move_gap_to(2);
        // Logical content unchanged.
        assert_eq!(buf.byte_at(0), b'a');
        assert_eq!(buf.byte_at(2), b'c');
        assert_eq!(buf.byte_at(4), b'e');
    }

    #[test]
    fn char_at_ascii() {
        crate::test_utils::init_test_tracing();
        let buf = GapBuffer::from_str("hello");
        assert_eq!(buf.char_at(0), Some('h'));
        assert_eq!(buf.char_at(4), Some('o'));
        assert_eq!(buf.char_at(5), None);
    }

    #[test]
    #[should_panic]
    fn byte_at_out_of_range_panics() {
        crate::test_utils::init_test_tracing();
        let buf = GapBuffer::from_str("hi");
        buf.byte_at(2);
    }

    // -----------------------------------------------------------------------
    // text_range
    // -----------------------------------------------------------------------

    #[test]
    fn text_range_full() {
        crate::test_utils::init_test_tracing();
        let buf = GapBuffer::from_str("hello world");
        assert_eq!(buf.text_range(0, 11), "hello world");
    }

    #[test]
    fn text_range_prefix() {
        crate::test_utils::init_test_tracing();
        let buf = GapBuffer::from_str("hello world");
        assert_eq!(buf.text_range(0, 5), "hello");
    }

    #[test]
    fn text_range_suffix() {
        crate::test_utils::init_test_tracing();
        let buf = GapBuffer::from_str("hello world");
        assert_eq!(buf.text_range(6, 11), "world");
    }

    #[test]
    fn text_range_spanning_gap() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("hello world");
        buf.move_gap_to(5);
        // Range spans the gap.
        assert_eq!(buf.text_range(3, 8), "lo wo");
    }

    #[test]
    fn text_range_empty() {
        crate::test_utils::init_test_tracing();
        let buf = GapBuffer::from_str("hello");
        assert_eq!(buf.text_range(2, 2), "");
    }

    // -----------------------------------------------------------------------
    // move_gap_to
    // -----------------------------------------------------------------------

    #[test]
    fn move_gap_to_start() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("hello");
        buf.move_gap_to(0);
        assert_eq!(buf.to_string(), "hello");
    }

    #[test]
    fn move_gap_to_end() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("hello");
        buf.move_gap_to(5);
        assert_eq!(buf.to_string(), "hello");
    }

    #[test]
    fn move_gap_around() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("abcdef");
        buf.move_gap_to(3);
        assert_eq!(buf.to_string(), "abcdef");
        buf.move_gap_to(0);
        assert_eq!(buf.to_string(), "abcdef");
        buf.move_gap_to(6);
        assert_eq!(buf.to_string(), "abcdef");
        buf.move_gap_to(2);
        assert_eq!(buf.to_string(), "abcdef");
    }

    // -----------------------------------------------------------------------
    // ensure_gap
    // -----------------------------------------------------------------------

    #[test]
    fn ensure_gap_grows() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("hello");
        let old_gap = buf.gap_size();
        buf.ensure_gap(old_gap + 100);
        assert!(buf.gap_size() >= old_gap + 100);
        // Content must be preserved.
        assert_eq!(buf.to_string(), "hello");
    }

    #[test]
    fn ensure_gap_noop_when_large_enough() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("hello");
        let old_gap = buf.gap_size();
        buf.ensure_gap(1);
        assert_eq!(buf.gap_size(), old_gap);
    }

    // -----------------------------------------------------------------------
    // Multibyte / UTF-8 (CJK, emoji)
    // -----------------------------------------------------------------------

    #[test]
    fn multibyte_cjk() {
        crate::test_utils::init_test_tracing();
        // Each CJK character is 3 bytes in UTF-8.
        let text = "\u{4F60}\u{597D}\u{4E16}\u{754C}"; // 你好世界
        let buf = GapBuffer::from_str(text);
        assert_eq!(buf.len(), 12); // 4 chars * 3 bytes
        assert_eq!(buf.char_count(), 4);
        assert_eq!(buf.to_string(), text);

        // char_at at byte boundaries
        assert_eq!(buf.char_at(0), Some('\u{4F60}')); // 你
        assert_eq!(buf.char_at(3), Some('\u{597D}')); // 好
        assert_eq!(buf.char_at(6), Some('\u{4E16}')); // 世
        assert_eq!(buf.char_at(9), Some('\u{754C}')); // 界
    }

    #[test]
    fn multibyte_emoji() {
        crate::test_utils::init_test_tracing();
        // Emoji are 4 bytes in UTF-8.
        let text = "\u{1F600}\u{1F60D}"; // two emoji
        let buf = GapBuffer::from_str(text);
        assert_eq!(buf.len(), 8);
        assert_eq!(buf.char_count(), 2);
        assert_eq!(buf.char_at(0), Some('\u{1F600}'));
        assert_eq!(buf.char_at(4), Some('\u{1F60D}'));
    }

    #[test]
    fn insert_multibyte_in_middle() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("ab");
        buf.insert_str(1, "\u{1F600}"); // insert emoji between a and b
        assert_eq!(buf.to_string(), "a\u{1F600}b");
        assert_eq!(buf.len(), 6); // 1 + 4 + 1
        assert_eq!(buf.char_count(), 3);
    }

    #[test]
    fn delete_multibyte_char() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("a\u{4F60}b"); // a你b
        // Delete the CJK char (bytes 1..4).
        buf.delete_range(1, 4);
        assert_eq!(buf.to_string(), "ab");
    }

    #[test]
    fn text_range_multibyte_spanning_gap() {
        crate::test_utils::init_test_tracing();
        let text = "\u{4F60}\u{597D}\u{4E16}\u{754C}"; // 你好世界
        let mut buf = GapBuffer::from_str(text);
        buf.move_gap_to(6); // gap between 好 and 世
        assert_eq!(buf.text_range(0, 6), "\u{4F60}\u{597D}");
        assert_eq!(buf.text_range(6, 12), "\u{4E16}\u{754C}");
        assert_eq!(buf.text_range(3, 9), "\u{597D}\u{4E16}");
    }

    #[test]
    fn mixed_ascii_and_multibyte() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("hello\u{4E16}\u{754C}!");
        // "hello世界!" — 5 + 3 + 3 + 1 = 12 bytes, 8 chars
        assert_eq!(buf.len(), 12);
        assert_eq!(buf.char_count(), 8);

        buf.insert_str(5, " ");
        assert_eq!(buf.to_string(), "hello \u{4E16}\u{754C}!");
        assert_eq!(buf.len(), 13);

        buf.delete_range(6, 12); // delete "世界"
        assert_eq!(buf.to_string(), "hello !");
    }

    // -----------------------------------------------------------------------
    // byte_to_char / char_to_byte
    // -----------------------------------------------------------------------

    #[test]
    fn byte_char_roundtrip_ascii() {
        crate::test_utils::init_test_tracing();
        let buf = GapBuffer::from_str("hello");
        for i in 0..=5 {
            assert_eq!(buf.byte_to_char(i), i);
            assert_eq!(buf.char_to_byte(i), i);
        }
    }

    #[test]
    fn byte_to_char_cjk() {
        crate::test_utils::init_test_tracing();
        let buf = GapBuffer::from_str("\u{4F60}\u{597D}\u{4E16}"); // 你好世
        assert_eq!(buf.byte_to_char(0), 0);
        assert_eq!(buf.byte_to_char(3), 1);
        assert_eq!(buf.byte_to_char(6), 2);
        assert_eq!(buf.byte_to_char(9), 3);
    }

    #[test]
    fn char_to_byte_cjk() {
        crate::test_utils::init_test_tracing();
        let buf = GapBuffer::from_str("\u{4F60}\u{597D}\u{4E16}"); // 你好世
        assert_eq!(buf.char_to_byte(0), 0);
        assert_eq!(buf.char_to_byte(1), 3);
        assert_eq!(buf.char_to_byte(2), 6);
        assert_eq!(buf.char_to_byte(3), 9);
    }

    #[test]
    fn byte_char_roundtrip_mixed() {
        crate::test_utils::init_test_tracing();
        // "a你b" — byte offsets: a=0, 你=1..4, b=4
        let buf = GapBuffer::from_str("a\u{4F60}b");
        assert_eq!(buf.byte_to_char(0), 0); // before 'a'
        assert_eq!(buf.byte_to_char(1), 1); // before '你'
        assert_eq!(buf.byte_to_char(4), 2); // before 'b'
        assert_eq!(buf.byte_to_char(5), 3); // end

        assert_eq!(buf.char_to_byte(0), 0);
        assert_eq!(buf.char_to_byte(1), 1);
        assert_eq!(buf.char_to_byte(2), 4);
        assert_eq!(buf.char_to_byte(3), 5);
    }

    #[test]
    fn byte_char_conversion_with_gap_in_middle() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("a\u{4F60}b\u{597D}c");
        // Move gap to middle of the text.
        buf.move_gap_to(4); // between 你 and b
        // Conversions should be unaffected by gap position.
        assert_eq!(buf.byte_to_char(0), 0);
        assert_eq!(buf.byte_to_char(1), 1);
        assert_eq!(buf.byte_to_char(4), 2);
        assert_eq!(buf.byte_to_char(5), 3);
        assert_eq!(buf.byte_to_char(8), 4);

        assert_eq!(buf.char_to_byte(0), 0);
        assert_eq!(buf.char_to_byte(1), 1);
        assert_eq!(buf.char_to_byte(2), 4);
        assert_eq!(buf.char_to_byte(3), 5);
        assert_eq!(buf.char_to_byte(4), 8);
    }

    #[test]
    fn byte_char_conversion_empty() {
        crate::test_utils::init_test_tracing();
        let buf = GapBuffer::new();
        assert_eq!(buf.byte_to_char(0), 0);
        assert_eq!(buf.char_to_byte(0), 0);
    }

    #[test]
    fn byte_to_char_emoji() {
        crate::test_utils::init_test_tracing();
        let buf = GapBuffer::from_str("x\u{1F600}y"); // x😀y
        // byte offsets: x=0, 😀=1..5, y=5
        assert_eq!(buf.byte_to_char(0), 0);
        assert_eq!(buf.byte_to_char(1), 1);
        assert_eq!(buf.byte_to_char(5), 2);
        assert_eq!(buf.byte_to_char(6), 3);
    }

    #[test]
    fn byte_char_conversion_unibyte_storage_sentinels() {
        crate::test_utils::init_test_tracing();
        let storage =
            crate::emacs_core::string_escape::bytes_to_unibyte_storage_string(&[0x80, b'A', 0xFF]);
        let buf = GapBuffer::from_str(&storage);
        assert_eq!(buf.char_count(), 3);
        assert_eq!(buf.char_to_byte(0), 0);
        assert_eq!(buf.char_to_byte(1), 3);
        assert_eq!(buf.char_to_byte(2), 4);
        assert_eq!(buf.char_to_byte(3), 7);
        assert_eq!(buf.byte_to_char(0), 0);
        assert_eq!(buf.byte_to_char(3), 1);
        assert_eq!(buf.byte_to_char(4), 2);
        assert_eq!(buf.byte_to_char(7), 3);
    }

    #[test]
    fn byte_char_conversion_unibyte_storage_sentinels_after_gap_move() {
        crate::test_utils::init_test_tracing();
        let storage =
            crate::emacs_core::string_escape::bytes_to_unibyte_storage_string(&[0x80, b'A', 0xFF]);
        let mut buf = GapBuffer::from_str(&storage);
        buf.move_gap_to(4);
        assert_eq!(buf.char_count(), 3);
        assert_eq!(buf.char_to_byte(0), 0);
        assert_eq!(buf.char_to_byte(1), 3);
        assert_eq!(buf.char_to_byte(2), 4);
        assert_eq!(buf.char_to_byte(3), 7);
        assert_eq!(buf.byte_to_char(0), 0);
        assert_eq!(buf.byte_to_char(3), 1);
        assert_eq!(buf.byte_to_char(4), 2);
        assert_eq!(buf.byte_to_char(7), 3);
    }

    // -----------------------------------------------------------------------
    // Edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn repeated_insert_delete_cycle() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::new();
        for i in 0..100 {
            let s = format!("{i}");
            buf.insert_str(buf.len(), &s);
        }
        let full = buf.to_string();
        assert!(!full.is_empty());

        // Delete everything one byte at a time from the front.
        while !buf.is_empty() {
            buf.delete_range(0, 1);
        }
        assert!(buf.is_empty());
        assert_eq!(buf.to_string(), "");
    }

    #[test]
    fn gap_moves_correctly_after_multiple_operations() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("the quick brown fox");
        buf.delete_range(4, 10); // delete "quick "
        assert_eq!(buf.to_string(), "the brown fox");
        buf.insert_str(4, "slow ");
        assert_eq!(buf.to_string(), "the slow brown fox");
        buf.delete_range(9, 15); // delete "brown "
        assert_eq!(buf.to_string(), "the slow fox");
        buf.insert_str(9, "red ");
        assert_eq!(buf.to_string(), "the slow red fox");
    }

    #[test]
    fn insert_at_every_position() {
        crate::test_utils::init_test_tracing();
        for pos in 0..=5 {
            let mut buf = GapBuffer::from_str("hello");
            buf.insert_str(pos, "X");
            assert_eq!(buf.len(), 6);
            assert_eq!(buf.byte_at(pos), b'X');
        }
    }

    #[test]
    fn display_trait() {
        crate::test_utils::init_test_tracing();
        let buf = GapBuffer::from_str("display test");
        let s = format!("{buf}");
        assert_eq!(s, "display test");
    }

    #[test]
    fn debug_trait_contains_text() {
        crate::test_utils::init_test_tracing();
        let buf = GapBuffer::from_str("dbg");
        let dbg = format!("{buf:?}");
        assert!(dbg.contains("dbg"));
        assert!(dbg.contains("GapBuffer"));
    }

    #[test]
    fn default_is_empty() {
        crate::test_utils::init_test_tracing();
        let buf = GapBuffer::default();
        assert!(buf.is_empty());
    }

    #[test]
    fn clone_is_independent() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("original");
        let clone = buf.clone();
        buf.insert_str(0, "X");
        assert_eq!(buf.to_string(), "Xoriginal");
        assert_eq!(clone.to_string(), "original");
    }

    #[test]
    #[should_panic]
    fn insert_past_end_panics() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("hi");
        buf.insert_str(3, "x");
    }

    #[test]
    #[should_panic]
    fn delete_past_end_panics() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("hi");
        buf.delete_range(0, 3);
    }

    #[test]
    #[should_panic]
    fn delete_inverted_range_panics() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("hello");
        buf.delete_range(3, 1);
    }

    #[test]
    #[should_panic]
    fn text_range_past_end_panics() {
        crate::test_utils::init_test_tracing();
        let buf = GapBuffer::from_str("hi");
        buf.text_range(0, 3);
    }

    #[test]
    #[should_panic]
    fn move_gap_past_end_panics() {
        crate::test_utils::init_test_tracing();
        let mut buf = GapBuffer::from_str("hi");
        buf.move_gap_to(3);
    }

    #[test]
    #[should_panic]
    fn byte_to_char_past_end_panics() {
        crate::test_utils::init_test_tracing();
        let buf = GapBuffer::from_str("hi");
        buf.byte_to_char(3);
    }

    #[test]
    fn char_to_byte_past_end_clamps() {
        crate::test_utils::init_test_tracing();
        let buf = GapBuffer::from_str("hi");
        // char_to_byte clamps to buffer end instead of panicking
        // when char_pos exceeds char_count (for stale positions).
        assert_eq!(buf.char_to_byte(3), buf.len());
        assert_eq!(buf.char_to_byte(100), buf.len());
    }

    // -----------------------------------------------------------------------
    // copy_bytes_to
    // -----------------------------------------------------------------------

    #[test]
    fn copy_bytes_to_basic() {
        crate::test_utils::init_test_tracing();
        let gb = GapBuffer::from_str("Hello, world!");
        let mut out = Vec::new();
        gb.copy_bytes_to(0, 5, &mut out);
        assert_eq!(&out, b"Hello");

        gb.copy_bytes_to(7, 13, &mut out);
        assert_eq!(&out, b"world!");
    }

    #[test]
    fn copy_bytes_to_spanning_gap() {
        crate::test_utils::init_test_tracing();
        let mut gb = GapBuffer::from_str("abcdef");
        gb.move_gap_to(3); // gap after "abc"
        let mut out = Vec::new();
        gb.copy_bytes_to(1, 5, &mut out); // "bcde" — spans gap
        assert_eq!(&out, b"bcde");
    }

    #[test]
    fn copy_bytes_to_empty_range() {
        crate::test_utils::init_test_tracing();
        let gb = GapBuffer::from_str("test");
        let mut out = vec![1, 2, 3]; // pre-existing contents
        gb.copy_bytes_to(2, 2, &mut out);
        assert!(out.is_empty());
    }
}
