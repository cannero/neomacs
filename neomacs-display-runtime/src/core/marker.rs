//! Pure Rust marker system.
//!
//! Implements the core of Emacs's marker mechanism (cf. `marker.c` and
//! `insdel.c`). Markers track both character and byte positions within
//! a buffer and are automatically adjusted when text is inserted or
//! deleted.
//!
//! # Insertion types
//!
//! Each marker has an [`InsertionType`] that controls its behavior when
//! text is inserted exactly at the marker's position:
//! - [`InsertionType::Before`]: the marker stays *before* the new text
//!   (i.e., the marker does not move).
//! - [`InsertionType::After`]: the marker moves to *after* the new text.
//!
//! # Position cache
//!
//! [`CharByteCache`] avoids O(n) UTF-8 scanning for every position
//! conversion by caching the most recent (charpos, bytepos) pair and
//! scanning forward or backward from it.
//!
//! # Top-level manager
//!
//! [`MarkerManager`] ties per-buffer [`MarkerList`]s together with a
//! shared [`CharByteCache`], providing a convenient single entry point
//! for creating, removing, querying, and bulk-adjusting markers.

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Insertion type
// ---------------------------------------------------------------------------

/// Controls whether a marker advances when text is inserted at its
/// exact position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InsertionType {
    /// Marker stays *before* inserted text (does not move).
    Before,
    /// Marker moves to *after* inserted text.
    After,
}

impl Default for InsertionType {
    fn default() -> Self {
        InsertionType::Before
    }
}

// ---------------------------------------------------------------------------
// Marker
// ---------------------------------------------------------------------------

/// A single buffer position marker.
///
/// Stores both character and byte positions so that either can be
/// retrieved in O(1) without scanning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    /// Character (logical) position in the buffer.
    pub charpos: usize,
    /// Byte position in the buffer (UTF-8).
    pub bytepos: usize,
    /// How the marker behaves on insertion at its position.
    pub insertion_type: InsertionType,
    /// The buffer this marker belongs to.
    pub buffer_id: u64,
}

impl Marker {
    /// Create a new marker.
    pub fn new(
        charpos: usize,
        bytepos: usize,
        insertion_type: InsertionType,
        buffer_id: u64,
    ) -> Self {
        Self {
            charpos,
            bytepos,
            insertion_type,
            buffer_id,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal marker data (held inside MarkerList's HashMap)
// ---------------------------------------------------------------------------

/// Internal representation stored in the marker map.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MarkerData {
    charpos: usize,
    bytepos: usize,
    insertion_type: InsertionType,
    buffer_id: u64,
}

impl MarkerData {
    fn to_marker(&self, _id: u64) -> Marker {
        Marker {
            charpos: self.charpos,
            bytepos: self.bytepos,
            insertion_type: self.insertion_type,
            buffer_id: self.buffer_id,
        }
    }

    #[allow(dead_code)]
    fn from_marker(m: &Marker) -> Self {
        Self {
            charpos: m.charpos,
            bytepos: m.bytepos,
            insertion_type: m.insertion_type,
            buffer_id: m.buffer_id,
        }
    }
}

// ---------------------------------------------------------------------------
// MarkerList
// ---------------------------------------------------------------------------

/// Manages a collection of markers for a single buffer.
///
/// Markers are stored in a `HashMap<u64, MarkerData>` keyed by a
/// monotonically increasing marker ID, giving O(1) lookup, insertion,
/// and removal.
#[derive(Debug, Clone)]
pub struct MarkerList {
    markers: HashMap<u64, MarkerData>,
    next_id: u64,
    buffer_id: u64,
}

impl MarkerList {
    /// Create a new, empty marker list for the given buffer.
    pub fn new(buffer_id: u64) -> Self {
        Self {
            markers: HashMap::new(),
            next_id: 1,
            buffer_id,
        }
    }

    /// Add a marker and return its unique ID.
    pub fn add_marker(
        &mut self,
        charpos: usize,
        bytepos: usize,
        insertion_type: InsertionType,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.markers.insert(
            id,
            MarkerData {
                charpos,
                bytepos,
                insertion_type,
                buffer_id: self.buffer_id,
            },
        );
        id
    }

    /// Remove the marker with the given ID.
    ///
    /// Returns `true` if the marker existed and was removed.
    pub fn remove_marker(&mut self, id: u64) -> bool {
        self.markers.remove(&id).is_some()
    }

    /// Remove all markers.
    pub fn clear(&mut self) {
        self.markers.clear();
    }

    /// Return the number of markers.
    pub fn len(&self) -> usize {
        self.markers.len()
    }

    /// Whether the list is empty.
    pub fn is_empty(&self) -> bool {
        self.markers.is_empty()
    }

    /// Look up a marker by ID.
    pub fn get(&self, id: u64) -> Option<Marker> {
        self.markers.get(&id).map(|d| d.to_marker(id))
    }

    /// Set the position of an existing marker.
    ///
    /// Returns `true` if the marker was found and updated.
    pub fn set_position(&mut self, id: u64, charpos: usize, bytepos: usize) -> bool {
        if let Some(data) = self.markers.get_mut(&id) {
            data.charpos = charpos;
            data.bytepos = bytepos;
            true
        } else {
            false
        }
    }

    /// Set the insertion type of an existing marker.
    ///
    /// Returns `true` if the marker was found and updated.
    pub fn set_insertion_type(&mut self, id: u64, insertion_type: InsertionType) -> bool {
        if let Some(data) = self.markers.get_mut(&id) {
            data.insertion_type = insertion_type;
            true
        } else {
            false
        }
    }

    /// Find all marker IDs at the given character position.
    pub fn markers_at(&self, charpos: usize) -> Vec<u64> {
        self.markers
            .iter()
            .filter(|(_, d)| d.charpos == charpos)
            .map(|(&id, _)| id)
            .collect()
    }

    /// Iterate over all (id, Marker) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (u64, Marker)> + '_ {
        self.markers.iter().map(|(&id, d)| (id, d.to_marker(id)))
    }

    // -- Adjustment for text mutations --------------------------------------

    /// Adjust all markers after an insertion.
    ///
    /// Text was inserted in the range `[from_char..to_char)` /
    /// `[from_byte..to_byte)`. Mirrors `adjust_markers_for_insert` in
    /// `insdel.c`.
    ///
    /// When `before_markers` is true, *all* markers at the insertion
    /// point are moved forward regardless of their insertion type.
    pub fn adjust_for_insert(
        &mut self,
        from_char: usize,
        from_byte: usize,
        to_char: usize,
        to_byte: usize,
        before_markers: bool,
    ) {
        let nchars = to_char - from_char;
        let nbytes = to_byte - from_byte;

        for data in self.markers.values_mut() {
            if data.bytepos == from_byte {
                // Marker is exactly at the insertion point.
                if data.insertion_type == InsertionType::After || before_markers {
                    data.bytepos = to_byte;
                    data.charpos = to_char;
                }
            } else if data.bytepos > from_byte {
                // Marker is after the insertion — shift forward.
                data.bytepos += nbytes;
                data.charpos += nchars;
            }
        }
    }

    /// Adjust all markers after a deletion.
    ///
    /// Text in `[from_char..to_char)` / `[from_byte..to_byte)` was
    /// deleted. Mirrors `adjust_markers_for_delete` in `insdel.c`.
    pub fn adjust_for_delete(
        &mut self,
        from_char: usize,
        from_byte: usize,
        to_char: usize,
        to_byte: usize,
    ) {
        let delta_chars = to_char - from_char;
        let delta_bytes = to_byte - from_byte;

        for data in self.markers.values_mut() {
            if data.charpos > to_char {
                // Marker is after the deleted region — shift back.
                data.charpos -= delta_chars;
                data.bytepos -= delta_bytes;
            } else if data.charpos > from_char {
                // Marker is inside the deleted region — clamp to start.
                data.charpos = from_char;
                data.bytepos = from_byte;
            }
        }
    }

    /// Adjust all markers after a replacement.
    ///
    /// Text in `[from_char..from_char+old_chars)` /
    /// `[from_byte..from_byte+old_bytes)` was replaced with new text of
    /// length `new_chars` / `new_bytes`. Mirrors
    /// `adjust_markers_for_replace` in `insdel.c`.
    pub fn adjust_for_replace(
        &mut self,
        from_char: usize,
        from_byte: usize,
        old_chars: usize,
        old_bytes: usize,
        new_chars: usize,
        new_bytes: usize,
    ) {
        if old_chars == 0 {
            // Pure insertion — delegate.
            self.adjust_for_insert(
                from_char,
                from_byte,
                from_char + new_chars,
                from_byte + new_bytes,
                false,
            );
            return;
        }

        let prev_to_byte = from_byte + old_bytes;
        let diff_chars = new_chars as isize - old_chars as isize;
        let diff_bytes = new_bytes as isize - old_bytes as isize;

        for data in self.markers.values_mut() {
            if data.bytepos >= prev_to_byte {
                // After the replaced region — shift by delta.
                data.bytepos = (data.bytepos as isize + diff_bytes) as usize;
                data.charpos = (data.charpos as isize + diff_chars) as usize;
            } else if data.bytepos > from_byte {
                // Inside the old region — clamp to from.
                data.charpos = from_char;
                data.bytepos = from_byte;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// CharByteCache
// ---------------------------------------------------------------------------

/// A cache for charpos <-> bytepos conversions.
///
/// Stores the most recent known (charpos, bytepos) pair so that
/// subsequent conversions can scan from a nearby point instead of
/// always starting from byte 0.
///
/// The cache is invalidated (reset) whenever the buffer is modified.
#[derive(Debug, Clone)]
pub struct CharByteCache {
    /// Last known character position (0-based).
    cached_charpos: usize,
    /// Last known byte position (0-based).
    cached_bytepos: usize,
    /// Whether the cache holds a valid entry.
    valid: bool,
}

impl CharByteCache {
    /// Create a new, empty cache.
    pub fn new() -> Self {
        Self {
            cached_charpos: 0,
            cached_bytepos: 0,
            valid: false,
        }
    }

    /// Invalidate the cache (call after any buffer modification).
    pub fn invalidate(&mut self) {
        self.valid = false;
    }

    /// Convert a character position to a byte position.
    ///
    /// `text` must be valid UTF-8. The function scans from the cached
    /// position (or from the beginning / end, whichever is closer) to
    /// find the byte offset of the `charpos`-th character.
    pub fn charpos_to_bytepos(&mut self, charpos: usize, text: &[u8]) -> usize {
        // Fast path: ASCII-only or single-byte buffer.
        let text_chars = char_count(text);
        if text.len() == text_chars {
            // Each char is one byte.
            self.update(charpos, charpos);
            return charpos;
        }

        // Determine the best starting point: beginning, end, or cache.
        let (mut cur_char, mut cur_byte) = (0usize, 0usize);
        let (end_char, end_byte) = (text_chars, text.len());

        // Distance from start.
        let mut best_dist = charpos;

        // Check end.
        let dist_from_end = end_char.saturating_sub(charpos);
        if dist_from_end < best_dist {
            best_dist = dist_from_end;
            cur_char = end_char;
            cur_byte = end_byte;
        }

        // Check cache.
        if self.valid {
            let dist_from_cache = if self.cached_charpos > charpos {
                self.cached_charpos - charpos
            } else {
                charpos - self.cached_charpos
            };
            if dist_from_cache < best_dist {
                cur_char = self.cached_charpos;
                cur_byte = self.cached_bytepos;
            }
        }

        // Scan forward or backward.
        if cur_char < charpos {
            while cur_char < charpos {
                cur_byte += utf8_char_len(text[cur_byte]);
                cur_char += 1;
            }
        } else {
            while cur_char > charpos {
                cur_byte = prev_char_boundary(text, cur_byte);
                cur_char -= 1;
            }
        }

        self.update(cur_char, cur_byte);
        cur_byte
    }

    /// Convert a byte position to a character position.
    ///
    /// `text` must be valid UTF-8 and `bytepos` must lie on a
    /// character boundary.
    pub fn bytepos_to_charpos(&mut self, bytepos: usize, text: &[u8]) -> usize {
        // Fast path.
        let text_chars = char_count(text);
        if text.len() == text_chars {
            self.update(bytepos, bytepos);
            return bytepos;
        }

        let (mut cur_char, mut cur_byte) = (0usize, 0usize);
        let (end_char, end_byte) = (text_chars, text.len());

        let mut best_dist = bytepos;

        let dist_from_end = end_byte.saturating_sub(bytepos);
        if dist_from_end < best_dist {
            best_dist = dist_from_end;
            cur_char = end_char;
            cur_byte = end_byte;
        }

        if self.valid {
            let dist_from_cache = if self.cached_bytepos > bytepos {
                self.cached_bytepos - bytepos
            } else {
                bytepos - self.cached_bytepos
            };
            if dist_from_cache < best_dist {
                cur_char = self.cached_charpos;
                cur_byte = self.cached_bytepos;
            }
        }

        if cur_byte < bytepos {
            while cur_byte < bytepos {
                cur_byte += utf8_char_len(text[cur_byte]);
                cur_char += 1;
            }
        } else {
            while cur_byte > bytepos {
                cur_byte = prev_char_boundary(text, cur_byte);
                cur_char -= 1;
            }
        }

        self.update(cur_char, cur_byte);
        cur_char
    }

    /// Update the cached position.
    fn update(&mut self, charpos: usize, bytepos: usize) {
        self.cached_charpos = charpos;
        self.cached_bytepos = bytepos;
        self.valid = true;
    }
}

impl Default for CharByteCache {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// UTF-8 helpers
// ---------------------------------------------------------------------------

/// Return the length of the UTF-8 character whose leading byte is `b`.
#[inline]
fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b < 0xE0 {
        2
    } else if b < 0xF0 {
        3
    } else {
        4
    }
}

/// Step backward to the previous character boundary in `text`, given a
/// current position `pos` (which must be > 0).
#[inline]
fn prev_char_boundary(text: &[u8], mut pos: usize) -> usize {
    debug_assert!(pos > 0);
    pos -= 1;
    while pos > 0 && (text[pos] & 0xC0) == 0x80 {
        pos -= 1;
    }
    pos
}

/// Count the number of UTF-8 characters in `text`.
fn char_count(text: &[u8]) -> usize {
    // Count bytes that are NOT continuation bytes.
    text.iter().filter(|&&b| (b & 0xC0) != 0x80).count()
}

// ---------------------------------------------------------------------------
// MarkerManager
// ---------------------------------------------------------------------------

/// Top-level manager combining per-buffer [`MarkerList`]s with a
/// [`CharByteCache`] for position conversion.
///
/// Provides a single entry point for all marker operations.
#[derive(Debug, Clone)]
pub struct MarkerManager {
    /// Per-buffer marker lists, keyed by buffer ID.
    lists: HashMap<u64, MarkerList>,
    /// Per-buffer position caches, keyed by buffer ID.
    caches: HashMap<u64, CharByteCache>,
}

impl MarkerManager {
    /// Create a new, empty manager.
    pub fn new() -> Self {
        Self {
            lists: HashMap::new(),
            caches: HashMap::new(),
        }
    }

    /// Ensure a buffer entry exists and return a mutable reference to
    /// its marker list.
    fn list_mut(&mut self, buffer_id: u64) -> &mut MarkerList {
        self.lists
            .entry(buffer_id)
            .or_insert_with(|| MarkerList::new(buffer_id))
    }

    /// Return a reference to the marker list for a buffer (if any).
    pub fn list(&self, buffer_id: u64) -> Option<&MarkerList> {
        self.lists.get(&buffer_id)
    }

    /// Return a mutable reference to the cache for a buffer.
    fn cache_mut(&mut self, buffer_id: u64) -> &mut CharByteCache {
        self.caches
            .entry(buffer_id)
            .or_insert_with(CharByteCache::new)
    }

    // -- Marker CRUD --------------------------------------------------------

    /// Add a marker to the given buffer. Returns the marker ID.
    pub fn add_marker(
        &mut self,
        buffer_id: u64,
        charpos: usize,
        bytepos: usize,
        insertion_type: InsertionType,
    ) -> u64 {
        self.list_mut(buffer_id)
            .add_marker(charpos, bytepos, insertion_type)
    }

    /// Remove a marker from the given buffer.
    pub fn remove_marker(&mut self, buffer_id: u64, marker_id: u64) -> bool {
        if let Some(list) = self.lists.get_mut(&buffer_id) {
            list.remove_marker(marker_id)
        } else {
            false
        }
    }

    /// Get a marker by ID from a specific buffer.
    pub fn get_marker(&self, buffer_id: u64, marker_id: u64) -> Option<Marker> {
        self.lists.get(&buffer_id).and_then(|l| l.get(marker_id))
    }

    /// Remove all markers for a specific buffer.
    pub fn clear_buffer(&mut self, buffer_id: u64) {
        if let Some(list) = self.lists.get_mut(&buffer_id) {
            list.clear();
        }
        if let Some(cache) = self.caches.get_mut(&buffer_id) {
            cache.invalidate();
        }
    }

    /// Remove a buffer entirely (markers + cache).
    pub fn remove_buffer(&mut self, buffer_id: u64) {
        self.lists.remove(&buffer_id);
        self.caches.remove(&buffer_id);
    }

    // -- Bulk adjustment ----------------------------------------------------

    /// Adjust all markers in `buffer_id` after an insertion.
    pub fn adjust_for_insert(
        &mut self,
        buffer_id: u64,
        from_char: usize,
        from_byte: usize,
        to_char: usize,
        to_byte: usize,
        before_markers: bool,
    ) {
        self.list_mut(buffer_id).adjust_for_insert(
            from_char,
            from_byte,
            to_char,
            to_byte,
            before_markers,
        );
        self.cache_mut(buffer_id).invalidate();
    }

    /// Adjust all markers in `buffer_id` after a deletion.
    pub fn adjust_for_delete(
        &mut self,
        buffer_id: u64,
        from_char: usize,
        from_byte: usize,
        to_char: usize,
        to_byte: usize,
    ) {
        self.list_mut(buffer_id)
            .adjust_for_delete(from_char, from_byte, to_char, to_byte);
        self.cache_mut(buffer_id).invalidate();
    }

    // -- Position conversion ------------------------------------------------

    /// Convert a character position to a byte position using the cache.
    pub fn charpos_to_bytepos(&mut self, buffer_id: u64, charpos: usize, text: &[u8]) -> usize {
        self.cache_mut(buffer_id).charpos_to_bytepos(charpos, text)
    }

    /// Convert a byte position to a character position using the cache.
    pub fn bytepos_to_charpos(&mut self, buffer_id: u64, bytepos: usize, text: &[u8]) -> usize {
        self.cache_mut(buffer_id).bytepos_to_charpos(bytepos, text)
    }

    // -- Iteration ----------------------------------------------------------

    /// Iterate over all markers in a buffer.
    pub fn markers(&self, buffer_id: u64) -> impl Iterator<Item = (u64, Marker)> + '_ {
        self.lists
            .get(&buffer_id)
            .into_iter()
            .flat_map(|l| l.iter())
    }

    /// Find all marker IDs at a given character position in a buffer.
    pub fn markers_at(&self, buffer_id: u64, charpos: usize) -> Vec<u64> {
        self.lists
            .get(&buffer_id)
            .map(|l| l.markers_at(charpos))
            .unwrap_or_default()
    }
}

impl Default for MarkerManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Helpers ------------------------------------------------------------

    const BUF: u64 = 1;

    fn make_list() -> MarkerList {
        MarkerList::new(BUF)
    }

    // 1. Add and retrieve a marker ------------------------------------------

    #[test]
    fn test_add_and_get_marker() {
        let mut ml = make_list();
        let id = ml.add_marker(10, 10, InsertionType::Before);
        let m = ml.get(id).unwrap();
        assert_eq!(m.charpos, 10);
        assert_eq!(m.bytepos, 10);
        assert_eq!(m.insertion_type, InsertionType::Before);
        assert_eq!(m.buffer_id, BUF);
    }

    // 2. Remove a marker ----------------------------------------------------

    #[test]
    fn test_remove_marker() {
        let mut ml = make_list();
        let id = ml.add_marker(5, 5, InsertionType::Before);
        assert_eq!(ml.len(), 1);
        assert!(ml.remove_marker(id));
        assert_eq!(ml.len(), 0);
        assert!(ml.get(id).is_none());
    }

    // 3. Remove nonexistent marker returns false ----------------------------

    #[test]
    fn test_remove_nonexistent() {
        let mut ml = make_list();
        assert!(!ml.remove_marker(999));
    }

    // 4. Clear all markers --------------------------------------------------

    #[test]
    fn test_clear() {
        let mut ml = make_list();
        ml.add_marker(0, 0, InsertionType::Before);
        ml.add_marker(5, 5, InsertionType::After);
        ml.add_marker(10, 10, InsertionType::Before);
        ml.clear();
        assert!(ml.is_empty());
        assert_eq!(ml.len(), 0);
    }

    // 5. markers_at finds correct markers -----------------------------------

    #[test]
    fn test_markers_at() {
        let mut ml = make_list();
        let id1 = ml.add_marker(10, 10, InsertionType::Before);
        let _id2 = ml.add_marker(20, 20, InsertionType::Before);
        let id3 = ml.add_marker(10, 10, InsertionType::After);

        let at_10 = ml.markers_at(10);
        assert_eq!(at_10.len(), 2);
        assert!(at_10.contains(&id1));
        assert!(at_10.contains(&id3));

        let at_20 = ml.markers_at(20);
        assert_eq!(at_20.len(), 1);

        let at_99 = ml.markers_at(99);
        assert!(at_99.is_empty());
    }

    // 6. Adjust for insert — marker after insertion point -------------------

    #[test]
    fn test_adjust_insert_marker_after() {
        let mut ml = make_list();
        let id = ml.add_marker(10, 10, InsertionType::Before);

        // Insert 5 chars / 5 bytes at position 5.
        ml.adjust_for_insert(5, 5, 10, 10, false);

        let m = ml.get(id).unwrap();
        assert_eq!(m.charpos, 15);
        assert_eq!(m.bytepos, 15);
    }

    // 7. Adjust for insert — InsertionType::Before at insertion point -------

    #[test]
    fn test_adjust_insert_before_type_at_point() {
        let mut ml = make_list();
        let id = ml.add_marker(5, 5, InsertionType::Before);

        // Insert 3 chars at byte position 5.
        ml.adjust_for_insert(5, 5, 8, 8, false);

        let m = ml.get(id).unwrap();
        // InsertionType::Before => marker does NOT move.
        assert_eq!(m.charpos, 5);
        assert_eq!(m.bytepos, 5);
    }

    // 8. Adjust for insert — InsertionType::After at insertion point --------

    #[test]
    fn test_adjust_insert_after_type_at_point() {
        let mut ml = make_list();
        let id = ml.add_marker(5, 5, InsertionType::After);

        ml.adjust_for_insert(5, 5, 8, 8, false);

        let m = ml.get(id).unwrap();
        // InsertionType::After => marker moves to after inserted text.
        assert_eq!(m.charpos, 8);
        assert_eq!(m.bytepos, 8);
    }

    // 9. Adjust for insert — before_markers forces all markers to move ------

    #[test]
    fn test_adjust_insert_before_markers_flag() {
        let mut ml = make_list();
        let id = ml.add_marker(5, 5, InsertionType::Before);

        ml.adjust_for_insert(5, 5, 8, 8, true);

        let m = ml.get(id).unwrap();
        // before_markers=true overrides InsertionType::Before.
        assert_eq!(m.charpos, 8);
        assert_eq!(m.bytepos, 8);
    }

    // 10. Adjust for insert — marker before insertion point (unchanged) ------

    #[test]
    fn test_adjust_insert_marker_before_point() {
        let mut ml = make_list();
        let id = ml.add_marker(3, 3, InsertionType::Before);

        ml.adjust_for_insert(5, 5, 10, 10, false);

        let m = ml.get(id).unwrap();
        assert_eq!(m.charpos, 3);
        assert_eq!(m.bytepos, 3);
    }

    // 11. Adjust for delete — marker after deleted region --------------------

    #[test]
    fn test_adjust_delete_marker_after() {
        let mut ml = make_list();
        let id = ml.add_marker(20, 20, InsertionType::Before);

        // Delete chars 5..10 (5 chars, 5 bytes).
        ml.adjust_for_delete(5, 5, 10, 10);

        let m = ml.get(id).unwrap();
        assert_eq!(m.charpos, 15);
        assert_eq!(m.bytepos, 15);
    }

    // 12. Adjust for delete — marker inside deleted region -------------------

    #[test]
    fn test_adjust_delete_marker_inside() {
        let mut ml = make_list();
        let id = ml.add_marker(7, 7, InsertionType::Before);

        ml.adjust_for_delete(5, 5, 10, 10);

        let m = ml.get(id).unwrap();
        // Clamped to start of deleted region.
        assert_eq!(m.charpos, 5);
        assert_eq!(m.bytepos, 5);
    }

    // 13. Adjust for delete — marker before deleted region (unchanged) -------

    #[test]
    fn test_adjust_delete_marker_before() {
        let mut ml = make_list();
        let id = ml.add_marker(3, 3, InsertionType::Before);

        ml.adjust_for_delete(5, 5, 10, 10);

        let m = ml.get(id).unwrap();
        assert_eq!(m.charpos, 3);
        assert_eq!(m.bytepos, 3);
    }

    // 14. Adjust for delete — marker at boundary of deleted region -----------

    #[test]
    fn test_adjust_delete_marker_at_boundary() {
        let mut ml = make_list();
        let id_start = ml.add_marker(5, 5, InsertionType::Before);
        let id_end = ml.add_marker(10, 10, InsertionType::Before);

        ml.adjust_for_delete(5, 5, 10, 10);

        let m_start = ml.get(id_start).unwrap();
        // charpos == from_char, NOT > from_char, so unchanged.
        assert_eq!(m_start.charpos, 5);

        let m_end = ml.get(id_end).unwrap();
        // charpos == to_char, but to_char > from_char, so it falls into the
        // "inside deleted region" case and gets clamped to from_char.
        assert_eq!(m_end.charpos, 5);
        assert_eq!(m_end.bytepos, 5);
    }

    // 15. Adjust for replace ------------------------------------------------

    #[test]
    fn test_adjust_replace() {
        let mut ml = make_list();
        let id_inside = ml.add_marker(7, 7, InsertionType::Before);
        let id_after = ml.add_marker(15, 15, InsertionType::Before);

        // Replace 5 chars at position 5 with 3 chars.
        // Old: [5..10), New: [5..8).
        ml.adjust_for_replace(5, 5, 5, 5, 3, 3);

        let m_inside = ml.get(id_inside).unwrap();
        // Inside old range => clamped to from.
        assert_eq!(m_inside.charpos, 5);

        let m_after = ml.get(id_after).unwrap();
        // After old range => shifted by diff (3 - 5 = -2).
        assert_eq!(m_after.charpos, 13);
        assert_eq!(m_after.bytepos, 13);
    }

    // 16. CharByteCache — ASCII text (charpos == bytepos) -------------------

    #[test]
    fn test_cache_ascii() {
        let mut cache = CharByteCache::new();
        let text = b"hello world";

        assert_eq!(cache.charpos_to_bytepos(5, text), 5);
        assert_eq!(cache.bytepos_to_charpos(5, text), 5);
    }

    // 17. CharByteCache — multi-byte UTF-8 ----------------------------------

    #[test]
    fn test_cache_multibyte() {
        let mut cache = CharByteCache::new();
        // "cafe" with e-acute: "caf\u{00e9}" = 63 61 66 C3 A9 (5 bytes, 4 chars)
        let text = "caf\u{00e9}".as_bytes();
        assert_eq!(text.len(), 5);

        // charpos 3 => the e-acute, which starts at byte 3.
        assert_eq!(cache.charpos_to_bytepos(3, text), 3);
        // charpos 4 (one past end of string chars) => byte 5.
        assert_eq!(cache.charpos_to_bytepos(4, text), 5);

        // Reverse.
        assert_eq!(cache.bytepos_to_charpos(3, text), 3);
        assert_eq!(cache.bytepos_to_charpos(5, text), 4);
    }

    // 18. CharByteCache — cache speeds up repeated lookups -------------------

    #[test]
    fn test_cache_reuse() {
        let mut cache = CharByteCache::new();
        // 3-byte CJK chars: each char = 3 bytes.
        let text = "\u{4e16}\u{754c}".as_bytes(); // 6 bytes, 2 chars
        assert_eq!(text.len(), 6);

        // First lookup populates cache.
        assert_eq!(cache.charpos_to_bytepos(1, text), 3);
        assert!(cache.valid);
        assert_eq!(cache.cached_charpos, 1);
        assert_eq!(cache.cached_bytepos, 3);

        // Nearby lookup should use cached position.
        assert_eq!(cache.charpos_to_bytepos(2, text), 6);
        assert_eq!(cache.cached_charpos, 2);
    }

    // 19. CharByteCache — invalidation resets state --------------------------

    #[test]
    fn test_cache_invalidate() {
        let mut cache = CharByteCache::new();
        let text = b"hello";

        cache.charpos_to_bytepos(3, text);
        assert!(cache.valid);

        cache.invalidate();
        assert!(!cache.valid);
    }

    // 20. CharByteCache — backward scanning ---------------------------------

    #[test]
    fn test_cache_backward_scan() {
        let mut cache = CharByteCache::new();
        // "\u{00e9}abc" = C3 A9 61 62 63 (5 bytes, 4 chars)
        let text = "\u{00e9}abc".as_bytes();
        assert_eq!(text.len(), 5);

        // First lookup: charpos 3 (the 'c') => byte 4.
        assert_eq!(cache.charpos_to_bytepos(3, text), 4);

        // Now look up charpos 1 (the 'a') => byte 2. Should scan backward
        // from cached (3, 4).
        assert_eq!(cache.charpos_to_bytepos(1, text), 2);
    }

    // 21. CharByteCache — bytepos_to_charpos backward -----------------------

    #[test]
    fn test_cache_bytepos_backward() {
        let mut cache = CharByteCache::new();
        // "\u{00e9}\u{00e9}" = C3 A9 C3 A9 (4 bytes, 2 chars)
        let text = "\u{00e9}\u{00e9}".as_bytes();
        assert_eq!(text.len(), 4);

        assert_eq!(cache.bytepos_to_charpos(4, text), 2);
        assert_eq!(cache.bytepos_to_charpos(0, text), 0);
    }

    // 22. MarkerManager — add and retrieve across buffers -------------------

    #[test]
    fn test_manager_multi_buffer() {
        let mut mgr = MarkerManager::new();

        let id1 = mgr.add_marker(1, 10, 10, InsertionType::Before);
        let id2 = mgr.add_marker(2, 20, 20, InsertionType::After);

        let m1 = mgr.get_marker(1, id1).unwrap();
        assert_eq!(m1.charpos, 10);
        assert_eq!(m1.buffer_id, 1);

        let m2 = mgr.get_marker(2, id2).unwrap();
        assert_eq!(m2.charpos, 20);
        assert_eq!(m2.buffer_id, 2);

        // Cross-buffer lookup: id2 was assigned by buffer 2's list.
        // Since each MarkerList has its own ID counter starting at 1,
        // id1==1 and id2==1. So get_marker(1, id2) finds buffer 1's
        // marker. Verify the positions differ to confirm isolation.
        let cross = mgr.get_marker(1, id2);
        if let Some(m) = cross {
            // Same numeric ID, but it's buffer 1's marker, not buffer 2's.
            assert_eq!(m.charpos, 10);
            assert_eq!(m.buffer_id, 1);
        }
        // A truly non-existent ID should fail.
        assert!(mgr.get_marker(1, 999).is_none());
    }

    // 23. MarkerManager — adjust_for_insert via manager ---------------------

    #[test]
    fn test_manager_adjust_insert() {
        let mut mgr = MarkerManager::new();
        let id = mgr.add_marker(BUF, 10, 10, InsertionType::Before);

        mgr.adjust_for_insert(BUF, 5, 5, 10, 10, false);

        let m = mgr.get_marker(BUF, id).unwrap();
        assert_eq!(m.charpos, 15);
    }

    // 24. MarkerManager — adjust_for_delete via manager ---------------------

    #[test]
    fn test_manager_adjust_delete() {
        let mut mgr = MarkerManager::new();
        let id = mgr.add_marker(BUF, 10, 10, InsertionType::Before);

        mgr.adjust_for_delete(BUF, 3, 3, 8, 8);

        let m = mgr.get_marker(BUF, id).unwrap();
        assert_eq!(m.charpos, 5);
        assert_eq!(m.bytepos, 5);
    }

    // 25. MarkerManager — clear_buffer removes markers but not the list -----

    #[test]
    fn test_manager_clear_buffer() {
        let mut mgr = MarkerManager::new();
        mgr.add_marker(BUF, 10, 10, InsertionType::Before);
        mgr.add_marker(BUF, 20, 20, InsertionType::Before);

        mgr.clear_buffer(BUF);

        assert_eq!(mgr.markers(BUF).count(), 0);

        // Can still add new markers after clearing.
        let id = mgr.add_marker(BUF, 5, 5, InsertionType::Before);
        assert!(mgr.get_marker(BUF, id).is_some());
    }

    // 26. MarkerManager — remove_buffer removes everything ------------------

    #[test]
    fn test_manager_remove_buffer() {
        let mut mgr = MarkerManager::new();
        mgr.add_marker(BUF, 10, 10, InsertionType::Before);
        mgr.remove_buffer(BUF);

        assert!(mgr.list(BUF).is_none());
        assert_eq!(mgr.markers(BUF).count(), 0);
    }

    // 27. MarkerManager — position conversion via manager -------------------

    #[test]
    fn test_manager_pos_conversion() {
        let mut mgr = MarkerManager::new();
        let text = "caf\u{00e9}".as_bytes();

        assert_eq!(mgr.charpos_to_bytepos(BUF, 4, text), 5);
        assert_eq!(mgr.bytepos_to_charpos(BUF, 5, text), 4);
    }

    // 28. MarkerManager — markers_at via manager ----------------------------

    #[test]
    fn test_manager_markers_at() {
        let mut mgr = MarkerManager::new();
        let id1 = mgr.add_marker(BUF, 10, 10, InsertionType::Before);
        mgr.add_marker(BUF, 20, 20, InsertionType::Before);
        let id3 = mgr.add_marker(BUF, 10, 10, InsertionType::After);

        let ids = mgr.markers_at(BUF, 10);
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&id1));
        assert!(ids.contains(&id3));
    }

    // 29. MarkerList — set_position -----------------------------------------

    #[test]
    fn test_set_position() {
        let mut ml = make_list();
        let id = ml.add_marker(5, 5, InsertionType::Before);

        assert!(ml.set_position(id, 20, 25));
        let m = ml.get(id).unwrap();
        assert_eq!(m.charpos, 20);
        assert_eq!(m.bytepos, 25);

        // Non-existent ID.
        assert!(!ml.set_position(999, 0, 0));
    }

    // 30. MarkerList — set_insertion_type -----------------------------------

    #[test]
    fn test_set_insertion_type() {
        let mut ml = make_list();
        let id = ml.add_marker(5, 5, InsertionType::Before);

        assert!(ml.set_insertion_type(id, InsertionType::After));
        let m = ml.get(id).unwrap();
        assert_eq!(m.insertion_type, InsertionType::After);

        assert!(!ml.set_insertion_type(999, InsertionType::Before));
    }

    // 31. MarkerList — multiple markers adjusted together --------------------

    #[test]
    fn test_bulk_adjust_insert() {
        let mut ml = make_list();
        let id1 = ml.add_marker(5, 5, InsertionType::Before);
        let id2 = ml.add_marker(5, 5, InsertionType::After);
        let id3 = ml.add_marker(10, 10, InsertionType::Before);
        let id4 = ml.add_marker(3, 3, InsertionType::Before);

        // Insert 3 chars at position 5.
        ml.adjust_for_insert(5, 5, 8, 8, false);

        assert_eq!(ml.get(id1).unwrap().charpos, 5); // Before, stays
        assert_eq!(ml.get(id2).unwrap().charpos, 8); // After, moves
        assert_eq!(ml.get(id3).unwrap().charpos, 13); // After insert, shifts
        assert_eq!(ml.get(id4).unwrap().charpos, 3); // Before insert, stays
    }

    // 32. MarkerList — multiple markers adjusted for delete ------------------

    #[test]
    fn test_bulk_adjust_delete() {
        let mut ml = make_list();
        let id1 = ml.add_marker(3, 3, InsertionType::Before);
        let id2 = ml.add_marker(7, 7, InsertionType::Before);
        let id3 = ml.add_marker(15, 15, InsertionType::Before);

        // Delete [5..10).
        ml.adjust_for_delete(5, 5, 10, 10);

        assert_eq!(ml.get(id1).unwrap().charpos, 3); // Before, stays
        assert_eq!(ml.get(id2).unwrap().charpos, 5); // Inside, clamped
        assert_eq!(ml.get(id3).unwrap().charpos, 10); // After, shifted
    }

    // 33. CharByteCache — 4-byte UTF-8 chars --------------------------------

    #[test]
    fn test_cache_4byte_utf8() {
        let mut cache = CharByteCache::new();
        // Emoji: U+1F600 = F0 9F 98 80 (4 bytes per char)
        let text = "\u{1F600}\u{1F601}".as_bytes();
        assert_eq!(text.len(), 8);

        assert_eq!(cache.charpos_to_bytepos(0, text), 0);
        assert_eq!(cache.charpos_to_bytepos(1, text), 4);
        assert_eq!(cache.charpos_to_bytepos(2, text), 8);

        assert_eq!(cache.bytepos_to_charpos(0, text), 0);
        assert_eq!(cache.bytepos_to_charpos(4, text), 1);
        assert_eq!(cache.bytepos_to_charpos(8, text), 2);
    }

    // 34. CharByteCache — empty text ----------------------------------------

    #[test]
    fn test_cache_empty_text() {
        let mut cache = CharByteCache::new();
        let text = b"";

        assert_eq!(cache.charpos_to_bytepos(0, text), 0);
        assert_eq!(cache.bytepos_to_charpos(0, text), 0);
    }

    // 35. InsertionType default is Before -----------------------------------

    #[test]
    fn test_insertion_type_default() {
        assert_eq!(InsertionType::default(), InsertionType::Before);
    }

    // 36. Marker IDs are unique and monotonically increasing -----------------

    #[test]
    fn test_marker_ids_unique() {
        let mut ml = make_list();
        let id1 = ml.add_marker(0, 0, InsertionType::Before);
        let id2 = ml.add_marker(0, 0, InsertionType::Before);
        let id3 = ml.add_marker(0, 0, InsertionType::Before);

        assert_ne!(id1, id2);
        assert_ne!(id2, id3);
        assert!(id1 < id2);
        assert!(id2 < id3);
    }

    // 37. Adjust insert with multi-byte chars and bytes ---------------------

    #[test]
    fn test_adjust_insert_multibyte() {
        let mut ml = make_list();
        // Marker at char 5, byte 7 (before insertion some multi-byte chars).
        let id = ml.add_marker(5, 7, InsertionType::Before);

        // Insert 2 chars / 4 bytes at (char 3, byte 4).
        ml.adjust_for_insert(3, 4, 5, 8, false);

        let m = ml.get(id).unwrap();
        // Marker was at byte 7 > from_byte 4, so shifted forward.
        assert_eq!(m.charpos, 7); // 5 + (5-3)=2
        assert_eq!(m.bytepos, 11); // 7 + (8-4)=4
    }

    // 38. Adjust delete with multi-byte chars and bytes ---------------------

    #[test]
    fn test_adjust_delete_multibyte() {
        let mut ml = make_list();
        let id = ml.add_marker(10, 15, InsertionType::Before);

        // Delete chars [3..6), bytes [4..10).
        ml.adjust_for_delete(3, 4, 6, 10);

        let m = ml.get(id).unwrap();
        // charpos 10 > to_char 6 => shifted by -(6-3)= -3.
        assert_eq!(m.charpos, 7);
        // bytepos 15 > to_byte 10 => shifted by -(10-4)= -6.
        assert_eq!(m.bytepos, 9);
    }

    // 39. MarkerList iter returns all markers --------------------------------

    #[test]
    fn test_iter() {
        let mut ml = make_list();
        ml.add_marker(1, 1, InsertionType::Before);
        ml.add_marker(2, 2, InsertionType::After);
        ml.add_marker(3, 3, InsertionType::Before);

        let all: Vec<_> = ml.iter().collect();
        assert_eq!(all.len(), 3);
    }

    // 40. MarkerManager default constructor ----------------------------------

    #[test]
    fn test_manager_default() {
        let mgr = MarkerManager::default();
        assert_eq!(mgr.markers(BUF).count(), 0);
    }
}
