//! A raw Emacs-byte gap buffer for efficient text editing.
//!
//! The gap buffer stores text in a contiguous `Vec<u8>` with a movable "gap"
//! (unused region) that makes insertions and deletions near the gap O(1)
//! amortized. The gap is relocated to the edit site before each mutation so
//! that sequential edits in the same neighborhood avoid large copies.
//!
//! All public positions are byte positions into the logical text (i.e. the
//! text with the gap removed) unless a parameter is explicitly named
//! `char_pos`. The underlying bytes are Emacs internal bytes, not sentinel-
//! encoded Rust strings.

use std::fmt;

/// Default initial gap size in bytes.
const DEFAULT_GAP_SIZE: usize = 64;

/// Growth factor when the gap must be expanded — the new gap will be at least
/// this many bytes, or the requested size, whichever is larger.
const MIN_GAP_GROW: usize = 64;

/// A gap buffer holding raw Emacs bytes.
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
    /// Whether the logical text should be interpreted as a multibyte buffer.
    multibyte: bool,
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
        Self::new_with_multibyte(true)
    }

    pub fn new_with_multibyte(multibyte: bool) -> Self {
        Self {
            buf: vec![0u8; DEFAULT_GAP_SIZE],
            multibyte,
            gap_start: 0,
            gap_end: DEFAULT_GAP_SIZE,
            gap_start_chars: 0,
            total_chars: 0,
            gap_start_bytes: 0,
            total_bytes: 0,
        }
    }

    /// Create a gap buffer pre-loaded with raw Emacs bytes.
    pub fn from_emacs_bytes(text: &[u8], multibyte: bool) -> Self {
        let gap = DEFAULT_GAP_SIZE;
        let char_count = emacs_char_count_bytes(text, multibyte);
        let byte_count = text.len();
        let mut buf = Vec::with_capacity(text.len() + gap);
        buf.extend_from_slice(text);
        buf.resize(text.len() + gap, 0);
        Self {
            buf,
            multibyte,
            gap_start: text.len(),
            gap_end: text.len() + gap,
            gap_start_chars: char_count,
            total_chars: char_count,
            gap_start_bytes: byte_count,
            total_bytes: byte_count,
        }
    }

    /// Create a gap buffer pre-loaded with the contents of `s`.
    pub fn from_str(s: &str) -> Self {
        let multibyte = !s.chars().any(|ch| {
            let code = ch as u32;
            (0xE300..=0xE3FF).contains(&code)
        });
        let bytes = crate::emacs_core::string_escape::storage_string_to_buffer_bytes(s, multibyte);
        Self::from_emacs_bytes(&bytes, multibyte)
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

    pub fn is_multibyte(&self) -> bool {
        self.multibyte
    }

    pub fn set_multibyte(&mut self, multibyte: bool) {
        if self.multibyte == multibyte {
            return;
        }
        self.multibyte = multibyte;
        let mut logical = Vec::with_capacity(self.len());
        self.copy_bytes_to(0, self.len(), &mut logical);
        self.gap_start_chars = emacs_char_count_bytes(&logical[..self.gap_start], self.multibyte);
        self.total_chars = emacs_char_count_bytes(&logical, self.multibyte);
        self.gap_start_bytes = self.gap_start;
        self.total_bytes = logical.len();
    }

    /// Number of logical Emacs characters in the buffer storage.
    pub fn char_count(&self) -> usize {
        self.total_chars
    }

    /// Number of logical Emacs bytes in the buffer.
    pub fn emacs_byte_len(&self) -> usize {
        self.total_bytes
    }

    /// GNU `GPT`: character position of the gap.
    pub fn gpt(&self) -> usize {
        self.gap_start_chars
    }

    /// GNU `Z`: character position of the end of buffer text.
    pub fn z(&self) -> usize {
        self.total_chars
    }

    /// GNU `GPT_BYTE`: logical Emacs byte position of the gap.
    pub fn gpt_byte(&self) -> usize {
        self.gap_start_bytes
    }

    /// GNU `Z_BYTE`: logical Emacs byte position of the end of buffer text.
    pub fn z_byte(&self) -> usize {
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
        (pos < self.total_bytes).then(|| self.byte_at(pos))
    }

    /// Return the `char` whose first byte starts at logical byte position `pos`,
    /// or `None` if `pos >= self.len()`.
    ///
    /// # Panics
    ///
    /// Panics if `pos` does not lie on a UTF-8 character boundary.
    pub fn char_at(&self, pos: usize) -> Option<char> {
        self.char_code_at(pos).and_then(char::from_u32)
    }

    /// Return the Emacs character code whose first byte begins at logical
    /// byte position `pos`, or `None` if `pos >= self.len()`.
    pub fn char_code_at(&self, pos: usize) -> Option<u32> {
        if pos >= self.len() {
            return None;
        }
        assert!(
            self.is_char_boundary(pos),
            "char_code_at: byte position {pos} is not a character boundary"
        );
        if !self.multibyte {
            return Some(self.byte_at(pos) as u32);
        }

        let mut tmp = [0u8; crate::emacs_core::emacs_char::MAX_MULTIBYTE_LENGTH];
        let available = (self.len() - pos).min(tmp.len());
        for (i, slot) in tmp[..available].iter_mut().enumerate() {
            *slot = self.byte_at(pos + i);
        }
        Some(crate::emacs_core::emacs_char::string_char(&tmp[..available]).0)
    }

    /// Convert a character position to a logical Emacs byte offset.
    pub fn char_to_emacs_byte(&self, char_pos: usize) -> usize {
        self.char_to_byte(char_pos)
    }

    /// Convert a logical Emacs byte offset to a character position.
    pub fn emacs_byte_to_char(&self, byte_pos: usize) -> usize {
        self.byte_to_char(byte_pos)
    }

    /// Convert a storage-byte boundary to the corresponding logical Emacs byte offset.
    pub fn storage_byte_to_emacs_byte(&self, byte_pos: usize) -> usize {
        byte_pos.min(self.len())
    }

    /// Convert a logical Emacs byte boundary to the corresponding storage-byte offset.
    pub fn emacs_byte_to_storage_byte(&self, byte_pos: usize) -> usize {
        byte_pos.min(self.total_bytes)
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
        self.copy_bytes_to(start, end, &mut out);
        crate::emacs_core::string_escape::emacs_bytes_to_storage_string(&out, self.multibyte)
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

    /// Copy logical Emacs bytes in the range `[start, end)` into `out`.
    ///
    /// `out` is cleared first, then the Emacs bytes are appended.
    ///
    /// # Panics
    /// Panics if `start > end` or `end > self.emacs_byte_len()`.
    pub fn copy_emacs_bytes_to(&self, start: usize, end: usize, out: &mut Vec<u8>) {
        assert!(
            start <= end,
            "copy_emacs_bytes_to: start ({start}) > end ({end})"
        );
        assert!(
            end <= self.total_bytes,
            "copy_emacs_bytes_to: end ({end}) > emacs len ({})",
            self.total_bytes
        );
        out.clear();
        if start == end {
            return;
        }
        out.reserve(end - start);

        self.copy_bytes_to(start, end, out);
    }

    /// Return the full buffer contents as a `String`.
    pub fn to_string(&self) -> String {
        self.text_range(0, self.len())
    }

    // -----------------------------------------------------------------------
    // Mutation
    // -----------------------------------------------------------------------

    /// Insert raw Emacs bytes at logical byte position `pos`.
    pub fn insert_emacs_bytes(&mut self, pos: usize, bytes: &[u8]) {
        assert!(
            pos <= self.len(),
            "insert_emacs_bytes: position {pos} out of range (len {})",
            self.len()
        );
        if bytes.is_empty() {
            return;
        }
        debug_assert!(
            pos == self.len() || self.is_char_boundary(pos),
            "insert_emacs_bytes: position {pos} is not on an Emacs character boundary"
        );

        let inserted_chars = emacs_char_count_bytes(bytes, self.multibyte);
        let inserted_bytes = bytes.len();
        self.move_gap_to(pos);
        self.ensure_gap(inserted_bytes);

        self.buf[self.gap_start..self.gap_start + inserted_bytes].copy_from_slice(bytes);
        self.gap_start += inserted_bytes;
        self.gap_start_chars += inserted_chars;
        self.total_chars += inserted_chars;
        self.gap_start_bytes += inserted_bytes;
        self.total_bytes += inserted_bytes;
    }

    /// Insert `s` at logical byte position `pos`.
    ///
    /// After the call the gap sits immediately after the newly inserted text,
    /// so consecutive inserts at the same position are fast.
    ///
    /// # Panics
    ///
    /// Panics if `pos > self.len()` or `pos` is not on a UTF-8 boundary.
    pub fn insert_str(&mut self, pos: usize, s: &str) {
        if s.is_empty() {
            return;
        }
        let bytes =
            crate::emacs_core::string_escape::storage_string_to_buffer_bytes(s, self.multibyte);
        self.insert_emacs_bytes(pos, &bytes);
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
            "delete_range: start ({start}) is not on an Emacs character boundary"
        );
        debug_assert!(
            end == self.len() || self.is_char_boundary(end),
            "delete_range: end ({end}) is not on an Emacs character boundary"
        );

        // Move the gap so that it starts at `start`, then extend it to swallow
        // the bytes up to `end`.
        self.move_gap_to(start);
        let deleted_chars = emacs_char_count_bytes(
            &self.buf[self.gap_end..self.gap_end + (end - start)],
            self.multibyte,
        );
        let deleted_bytes = end - start;
        // After move_gap_to(start), gap_start == start and the bytes that were
        // logically at [start, end) now sit at buf[gap_end .. gap_end + (end - start)].
        self.gap_end += end - start;
        self.total_chars -= deleted_chars;
        self.total_bytes -= deleted_bytes;
    }

    /// Overwrite the logical byte range `[start, end)` with raw Emacs bytes.
    pub fn replace_same_len_emacs_bytes(&mut self, start: usize, end: usize, replacement: &[u8]) {
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
            "replace_same_len_range: replacement Emacs-byte length ({}) must match replaced length ({})",
            replacement.len(),
            end - start
        );
        if start == end {
            return;
        }
        debug_assert!(
            self.is_char_boundary(start),
            "replace_same_len_range: start ({start}) is not on an Emacs character boundary"
        );
        debug_assert!(
            end == self.len() || self.is_char_boundary(end),
            "replace_same_len_range: end ({end}) is not on an Emacs character boundary"
        );

        self.move_gap_to(end);
        let old_chars = emacs_char_count_bytes(&self.buf[start..end], self.multibyte);
        let new_chars = emacs_char_count_bytes(replacement, self.multibyte);
        let old_bytes = end - start;
        let new_bytes = replacement.len();
        self.buf[start..end].copy_from_slice(replacement);
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

    /// Overwrite the logical byte range `[start, end)` with `replacement`.
    ///
    /// `replacement` must have the exact same Emacs-byte length as the replaced
    /// region, and both boundaries must lie on Emacs character boundaries.
    pub fn replace_same_len_range(&mut self, start: usize, end: usize, replacement: &str) {
        let replacement_bytes = crate::emacs_core::string_escape::storage_string_to_buffer_bytes(
            replacement,
            self.multibyte,
        );
        self.replace_same_len_emacs_bytes(start, end, &replacement_bytes);
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
                emacs_char_count_bytes(&self.buf[pos..self.gap_start], self.multibyte);
            let moved_bytes = count;
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
            let moved_chars =
                emacs_char_count_bytes(&self.buf[src_start..src_start + count], self.multibyte);
            let moved_bytes = count;
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
    /// Panics if `byte_pos > self.len()` or is not on an Emacs character
    /// boundary.
    pub fn byte_to_char(&self, byte_pos: usize) -> usize {
        assert!(
            byte_pos <= self.len(),
            "byte_to_char: byte_pos ({byte_pos}) > len ({})",
            self.len()
        );
        if byte_pos <= self.gap_start {
            return emacs_byte_to_char_in_slice(
                &self.buf[..self.gap_start],
                byte_pos,
                self.multibyte,
                "byte_to_char pre-gap",
            );
        }

        let rel_pos = byte_pos - self.gap_start;
        self.gap_start_chars
            + emacs_byte_to_char_in_slice(
                &self.buf[self.gap_end..],
                rel_pos,
                self.multibyte,
                "byte_to_char post-gap",
            )
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
            return emacs_char_to_byte_in_slice(
                &self.buf[..self.gap_start],
                char_pos,
                self.multibyte,
            );
        }

        if char_pos <= self.total_chars {
            return self.gap_start
                + emacs_char_to_byte_in_slice(
                    &self.buf[self.gap_end..],
                    char_pos - self.gap_start_chars,
                    self.multibyte,
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

    /// Check whether `pos` falls on a logical Emacs-character boundary in the
    /// text.
    fn is_char_boundary(&self, pos: usize) -> bool {
        if pos == 0 || pos >= self.len() {
            return true;
        }
        if pos < self.gap_start {
            return is_emacs_char_boundary(&self.buf[..self.gap_start], pos, self.multibyte);
        }

        let rel_pos = pos - self.gap_start;
        is_emacs_char_boundary(&self.buf[self.gap_end..], rel_pos, self.multibyte)
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
    pub(crate) fn from_dump(text: Vec<u8>, multibyte: bool) -> Self {
        let len = text.len();
        let char_count = emacs_char_count_bytes(&text, multibyte);
        let byte_count = text.len();
        Self {
            buf: text,
            multibyte,
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

#[inline]
fn emacs_char_count_bytes(bytes: &[u8], multibyte: bool) -> usize {
    if multibyte {
        crate::emacs_core::emacs_char::chars_in_multibyte(bytes)
    } else {
        bytes.len()
    }
}

#[inline]
fn emacs_char_to_byte_in_slice(bytes: &[u8], char_pos: usize, multibyte: bool) -> usize {
    if multibyte {
        crate::emacs_core::emacs_char::char_to_byte_pos(bytes, char_pos)
    } else {
        char_pos.min(bytes.len())
    }
}

#[inline]
fn emacs_byte_to_char_in_slice(
    bytes: &[u8],
    byte_pos: usize,
    multibyte: bool,
    context: &str,
) -> usize {
    if !multibyte {
        return byte_pos.min(bytes.len());
    }
    assert!(
        is_emacs_char_boundary(bytes, byte_pos, multibyte),
        "{context}: byte_pos ({byte_pos}) is not an Emacs character boundary",
    );
    crate::emacs_core::emacs_char::byte_to_char_pos(bytes, byte_pos)
}

#[inline]
fn is_emacs_char_boundary(bytes: &[u8], byte_pos: usize, multibyte: bool) -> bool {
    if byte_pos > bytes.len() {
        return false;
    }
    if !multibyte {
        return true;
    }
    let mut pos = 0usize;
    while pos < byte_pos {
        let (_, len) = crate::emacs_core::emacs_char::string_char(&bytes[pos..]);
        pos += len;
    }
    pos == byte_pos
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
        assert_eq!(buf.char_to_byte(1), 1);
        assert_eq!(buf.char_to_byte(2), 2);
        assert_eq!(buf.char_to_byte(3), 3);
        assert_eq!(buf.byte_to_char(0), 0);
        assert_eq!(buf.byte_to_char(1), 1);
        assert_eq!(buf.byte_to_char(2), 2);
        assert_eq!(buf.byte_to_char(3), 3);
    }

    #[test]
    fn byte_char_conversion_unibyte_storage_sentinels_after_gap_move() {
        crate::test_utils::init_test_tracing();
        let storage =
            crate::emacs_core::string_escape::bytes_to_unibyte_storage_string(&[0x80, b'A', 0xFF]);
        let mut buf = GapBuffer::from_str(&storage);
        buf.move_gap_to(2);
        assert_eq!(buf.char_count(), 3);
        assert_eq!(buf.char_to_byte(0), 0);
        assert_eq!(buf.char_to_byte(1), 1);
        assert_eq!(buf.char_to_byte(2), 2);
        assert_eq!(buf.char_to_byte(3), 3);
        assert_eq!(buf.byte_to_char(0), 0);
        assert_eq!(buf.byte_to_char(1), 1);
        assert_eq!(buf.byte_to_char(2), 2);
        assert_eq!(buf.byte_to_char(3), 3);
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

    #[test]
    fn copy_emacs_bytes_to_unibyte_storage_sentinels() {
        crate::test_utils::init_test_tracing();
        let storage = crate::emacs_core::string_escape::bytes_to_unibyte_storage_string(&[
            0xFF, b'\n', 0x80, b'A',
        ]);
        let mut gb = GapBuffer::from_str(&storage);
        let gap_pos = gb.emacs_byte_to_storage_byte(2);
        gb.move_gap_to(gap_pos);

        let mut out = Vec::new();
        gb.copy_emacs_bytes_to(0, 4, &mut out);
        assert_eq!(out, vec![0xFF, b'\n', 0x80, b'A']);

        gb.copy_emacs_bytes_to(1, 3, &mut out);
        assert_eq!(out, vec![b'\n', 0x80]);
    }
}
