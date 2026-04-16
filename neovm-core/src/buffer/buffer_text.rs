//! Buffer text storage.
//!
//! GNU Emacs separates per-buffer metadata from the underlying text object.
//! `BufferText` is the first local seam toward that design. Today it is a thin
//! wrapper around `GapBuffer`; later it can absorb shared text state, char/byte
//! caches, and interval ownership without forcing another tree-wide field-type
//! rewrite.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

use crate::emacs_core::value::Value;
use crate::gc_trace::GcTrace;

use super::buffer::{BufferId, InsertionType, MarkerEntry};
use super::gap_buffer::GapBuffer;
use super::text_props::{PropertyInterval, TextPropertyTable};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BufferTextLayout {
    pub gpt: usize,
    pub z: usize,
    pub gpt_byte: usize,
    pub z_byte: usize,
    pub gap_size: usize,
}

/// Last successful char↔byte conversion. Reused on a subsequent query if the
/// buffer text has not changed since the entry was stored. Mirrors GNU
/// `marker.c:202-203` but uses a (total_chars, total_bytes) epoch rather than
/// `chars_modiff` so it works correctly even when called directly on
/// `BufferText` without going through the `insdel.rs` tick-bumping path.
#[derive(Clone, Copy, Default)]
struct PositionCache {
    /// Total char count when this entry was stored. 0 = invalid.
    epoch_chars: usize,
    /// Total byte length when this entry was stored. 0 = invalid (disambiguates
    /// a legitimately empty buffer from an uninitialised cache).
    epoch_bytes: usize,
    charpos: usize,
    bytepos: usize,
}

#[derive(Clone)]
struct BufferTextStorage {
    layout: BufferTextLayout,
    gap: GapBuffer,
    modified_tick: i64,
    chars_modified_tick: i64,
    save_modified_tick: i64,
    text_props: TextPropertyTable,
    markers: Vec<MarkerEntry>,
    /// Interior-mutable last-query cache for char↔byte conversion.
    pos_cache: Cell<PositionCache>,
    /// Internal (non-Lisp-visible) anchor positions populated on long scans.
    /// Invalidated wholesale when `(total_chars, total_bytes)` advances.
    anchor_cache: RefCell<Vec<(usize, usize)>>,
    /// `(epoch_chars, epoch_bytes)` at which the anchor_cache is valid.
    /// Mismatch triggers a wholesale clear on next read.
    anchor_cache_key: Cell<(usize, usize)>,
}

pub struct BufferText {
    storage: Rc<RefCell<BufferTextStorage>>,
}

impl Clone for BufferText {
    fn clone(&self) -> Self {
        let storage = self.storage.borrow().clone();
        Self {
            storage: Rc::new(RefCell::new(storage)),
        }
    }
}

impl Default for BufferText {
    fn default() -> Self {
        Self::new()
    }
}

impl BufferText {
    fn layout_from_gap(gap: &GapBuffer) -> BufferTextLayout {
        BufferTextLayout {
            gpt: gap.gpt(),
            z: gap.z(),
            gpt_byte: gap.gpt_byte(),
            z_byte: gap.z_byte(),
            gap_size: gap.gap_size(),
        }
    }

    pub fn new() -> Self {
        let gap = GapBuffer::new();
        Self {
            storage: Rc::new(RefCell::new(BufferTextStorage {
                layout: Self::layout_from_gap(&gap),
                gap,
                modified_tick: 1,
                chars_modified_tick: 1,
                save_modified_tick: 1,
                text_props: TextPropertyTable::new(),
                markers: Vec::new(),
                pos_cache: Cell::new(PositionCache::default()),
                anchor_cache: RefCell::new(Vec::new()),
                anchor_cache_key: Cell::new((0, 0)),
            })),
        }
    }

    pub fn from_str(text: &str) -> Self {
        let gap = GapBuffer::from_str(text);
        Self {
            storage: Rc::new(RefCell::new(BufferTextStorage {
                layout: Self::layout_from_gap(&gap),
                gap,
                modified_tick: 1,
                chars_modified_tick: 1,
                save_modified_tick: 1,
                text_props: TextPropertyTable::new(),
                markers: Vec::new(),
                pos_cache: Cell::new(PositionCache::default()),
                anchor_cache: RefCell::new(Vec::new()),
                anchor_cache_key: Cell::new((0, 0)),
            })),
        }
    }

    pub fn len(&self) -> usize {
        self.storage.borrow().gap.len()
    }

    pub fn is_multibyte(&self) -> bool {
        self.storage.borrow().gap.is_multibyte()
    }

    pub fn set_multibyte(&self, multibyte: bool) {
        let mut storage = self.storage.borrow_mut();
        storage.gap.set_multibyte(multibyte);
        storage.layout = Self::layout_from_gap(&storage.gap);
    }

    pub fn is_empty(&self) -> bool {
        self.storage.borrow().gap.is_empty()
    }

    pub fn char_count(&self) -> usize {
        self.storage.borrow().gap.char_count()
    }

    pub fn emacs_byte_len(&self) -> usize {
        self.storage.borrow().gap.emacs_byte_len()
    }

    pub fn layout(&self) -> BufferTextLayout {
        self.storage.borrow().layout
    }

    pub fn modified_tick(&self) -> i64 {
        self.storage.borrow().modified_tick
    }

    pub fn chars_modified_tick(&self) -> i64 {
        self.storage.borrow().chars_modified_tick
    }

    pub fn save_modified_tick(&self) -> i64 {
        self.storage.borrow().save_modified_tick
    }

    pub fn byte_at(&self, pos: usize) -> u8 {
        self.storage.borrow().gap.byte_at(pos)
    }

    pub fn emacs_byte_at(&self, pos: usize) -> Option<u8> {
        self.storage.borrow().gap.emacs_byte_at(pos)
    }

    pub fn char_at(&self, pos: usize) -> Option<char> {
        self.storage
            .borrow()
            .gap
            .char_code_at(pos)
            .and_then(char::from_u32)
    }

    pub fn char_code_at(&self, pos: usize) -> Option<u32> {
        self.storage.borrow().gap.char_code_at(pos)
    }

    pub fn text_range(&self, start: usize, end: usize) -> String {
        self.storage.borrow().gap.text_range(start, end)
    }

    pub fn copy_bytes_to(&self, start: usize, end: usize, out: &mut Vec<u8>) {
        self.storage.borrow().gap.copy_bytes_to(start, end, out);
    }

    pub fn copy_emacs_bytes_to(&self, start: usize, end: usize, out: &mut Vec<u8>) {
        self.storage
            .borrow()
            .gap
            .copy_emacs_bytes_to(start, end, out);
    }

    pub fn to_string(&self) -> String {
        self.storage.borrow().gap.to_string()
    }

    pub fn insert_str(&mut self, pos: usize, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut storage = self.storage.borrow_mut();
        storage.gap.insert_str(pos, text);
        storage.layout = Self::layout_from_gap(&storage.gap);
    }

    pub fn insert_emacs_bytes(&mut self, pos: usize, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut storage = self.storage.borrow_mut();
        storage.gap.insert_emacs_bytes(pos, bytes);
        storage.layout = Self::layout_from_gap(&storage.gap);
    }

    pub fn insert_emacs_bytes_both(&mut self, pos: usize, bytes: &[u8], nchars: usize) {
        if bytes.is_empty() {
            return;
        }
        let mut storage = self.storage.borrow_mut();
        storage.gap.insert_emacs_bytes_both(pos, bytes, nchars);
        storage.layout = Self::layout_from_gap(&storage.gap);
    }

    pub fn delete_range(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let mut storage = self.storage.borrow_mut();
        storage.gap.delete_range(start, end);
        storage.layout = Self::layout_from_gap(&storage.gap);
    }

    pub fn delete_range_both(&mut self, start: usize, end: usize, nchars: usize) {
        if start >= end {
            return;
        }
        let mut storage = self.storage.borrow_mut();
        storage.gap.delete_range_both(start, end, nchars);
        storage.layout = Self::layout_from_gap(&storage.gap);
    }

    pub fn replace_same_len_range(&mut self, start: usize, end: usize, replacement: &str) {
        if start >= end {
            return;
        }
        let mut storage = self.storage.borrow_mut();
        storage.gap.replace_same_len_range(start, end, replacement);
        storage.layout = Self::layout_from_gap(&storage.gap);
    }

    pub fn replace_same_len_emacs_bytes(&mut self, start: usize, end: usize, replacement: &[u8]) {
        if start >= end {
            return;
        }
        let mut storage = self.storage.borrow_mut();
        storage
            .gap
            .replace_same_len_emacs_bytes(start, end, replacement);
        storage.layout = Self::layout_from_gap(&storage.gap);
    }

    pub fn byte_to_char(&self, byte_pos: usize) -> usize {
        self.buf_bytepos_to_charpos(byte_pos)
    }

    pub fn char_to_byte(&self, char_pos: usize) -> usize {
        self.buf_charpos_to_bytepos(char_pos)
    }

    pub fn emacs_byte_to_char(&self, byte_pos: usize) -> usize {
        // Storage bytes == Emacs bytes in current NeoMacs, so this is an
        // alias. If that ever diverges, do the extra translation first here.
        self.buf_bytepos_to_charpos(byte_pos)
    }

    pub fn char_to_emacs_byte(&self, char_pos: usize) -> usize {
        self.buf_charpos_to_bytepos(char_pos)
    }

    pub fn storage_byte_to_emacs_byte(&self, byte_pos: usize) -> usize {
        self.storage
            .borrow()
            .gap
            .storage_byte_to_emacs_byte(byte_pos)
    }

    pub fn emacs_byte_to_storage_byte(&self, byte_pos: usize) -> usize {
        self.storage
            .borrow()
            .gap
            .emacs_byte_to_storage_byte(byte_pos)
    }

    pub fn shared_clone(&self) -> Self {
        Self {
            storage: Rc::clone(&self.storage),
        }
    }

    pub(crate) fn shares_storage_with(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.storage, &other.storage)
    }

    pub(crate) fn dump_text(&self) -> Vec<u8> {
        self.storage.borrow().gap.dump_text()
    }

    pub(crate) fn from_dump(text: Vec<u8>, multibyte: bool) -> Self {
        let gap = GapBuffer::from_dump(text, multibyte);
        Self {
            storage: Rc::new(RefCell::new(BufferTextStorage {
                layout: Self::layout_from_gap(&gap),
                gap,
                modified_tick: 1,
                chars_modified_tick: 1,
                save_modified_tick: 1,
                text_props: TextPropertyTable::new(),
                markers: Vec::new(),
                pos_cache: Cell::new(PositionCache::default()),
                anchor_cache: RefCell::new(Vec::new()),
                anchor_cache_key: Cell::new((0, 0)),
            })),
        }
    }

    pub fn set_modification_state(
        &self,
        modified_tick: i64,
        chars_modified_tick: i64,
        save_modified_tick: i64,
    ) {
        let mut storage = self.storage.borrow_mut();
        storage.modified_tick = modified_tick;
        storage.chars_modified_tick = chars_modified_tick;
        storage.save_modified_tick = save_modified_tick;
    }

    pub fn set_modified_tick(&self, tick: i64) {
        self.storage.borrow_mut().modified_tick = tick;
    }

    pub fn set_save_modified_tick(&self, tick: i64) {
        self.storage.borrow_mut().save_modified_tick = tick;
    }

    pub fn increment_modified_tick(&self, delta: i64) {
        self.storage.borrow_mut().modified_tick += delta;
    }

    pub fn record_char_modification(&self, delta: i64) {
        let mut storage = self.storage.borrow_mut();
        storage.modified_tick += delta;
        storage.chars_modified_tick = storage.modified_tick;
    }

    pub fn range_contains_char_code(&self, start: usize, end: usize, code: u32) -> bool {
        if start >= end {
            return false;
        }
        let text = self.storage.borrow().gap.text_range(start, end);
        crate::emacs_core::string_escape::storage_contains_char_code(&text, code)
    }

    pub fn replace_char_code_same_len_range(
        &mut self,
        start: usize,
        end: usize,
        from_code: u32,
        to_storage: &str,
    ) -> bool {
        if start >= end {
            return false;
        }
        let original = self.storage.borrow().gap.text_range(start, end);
        let Some(replacement) =
            crate::emacs_core::string_escape::replace_storage_char_code_same_len(
                &original, from_code, to_storage,
            )
        else {
            return false;
        };
        debug_assert_eq!(replacement.len(), original.len());
        let mut storage = self.storage.borrow_mut();
        storage.gap.replace_same_len_range(start, end, &replacement);
        storage.layout = Self::layout_from_gap(&storage.gap);
        true
    }

    pub fn text_props_is_empty(&self) -> bool {
        self.storage.borrow().text_props.is_empty()
    }

    pub fn text_props_snapshot(&self) -> TextPropertyTable {
        self.storage.borrow().text_props.clone()
    }

    pub fn text_props_replace(&self, table: TextPropertyTable) {
        self.storage.borrow_mut().text_props = table;
    }

    pub fn replace_storage(
        &self,
        text: &str,
        multibyte: bool,
        text_props: TextPropertyTable,
        markers: Vec<MarkerEntry>,
    ) {
        let bytes =
            crate::emacs_core::string_escape::storage_string_to_buffer_bytes(text, multibyte);
        let string = if multibyte {
            crate::heap_types::LispString::from_emacs_bytes(bytes)
        } else {
            crate::heap_types::LispString::from_unibyte(bytes)
        };
        self.replace_lisp_string(&string, text_props, markers);
    }

    pub fn replace_lisp_string(
        &self,
        text: &crate::heap_types::LispString,
        text_props: TextPropertyTable,
        markers: Vec<MarkerEntry>,
    ) {
        let mut storage = self.storage.borrow_mut();
        storage.gap = GapBuffer::from_emacs_bytes(text.as_bytes(), text.is_multibyte());
        storage.layout = Self::layout_from_gap(&storage.gap);
        storage.text_props = text_props;
        storage.markers = markers;
    }

    pub fn text_props_put_property(
        &self,
        start: usize,
        end: usize,
        name: Value,
        value: Value,
    ) -> bool {
        self.storage
            .borrow_mut()
            .text_props
            .put_property(start, end, name, value)
    }

    pub fn text_props_get_property(&self, pos: usize, name: Value) -> Option<Value> {
        self.storage
            .borrow()
            .text_props
            .get_property(pos, name)
            .copied()
    }

    pub fn text_props_get_properties(&self, pos: usize) -> HashMap<Value, Value> {
        self.storage.borrow().text_props.get_properties(pos)
    }

    pub fn text_props_get_properties_ordered(&self, pos: usize) -> Vec<(Value, Value)> {
        self.storage.borrow().text_props.get_properties_ordered(pos)
    }

    pub fn text_props_remove_property(&self, start: usize, end: usize, name: Value) -> bool {
        self.storage
            .borrow_mut()
            .text_props
            .remove_property(start, end, name)
    }

    pub fn text_props_remove_all(&self, start: usize, end: usize) {
        self.storage
            .borrow_mut()
            .text_props
            .remove_all_properties(start, end);
    }

    pub fn text_props_next_change(&self, pos: usize) -> Option<usize> {
        self.storage.borrow().text_props.next_property_change(pos)
    }

    pub fn text_props_previous_change(&self, pos: usize) -> Option<usize> {
        self.storage
            .borrow()
            .text_props
            .previous_property_change(pos)
    }

    pub fn text_props_append_shifted(&self, other: &TextPropertyTable, byte_offset: usize) {
        self.storage
            .borrow_mut()
            .text_props
            .append_shifted(other, byte_offset);
    }

    pub fn text_props_slice(&self, start: usize, end: usize) -> TextPropertyTable {
        self.storage.borrow().text_props.slice(start, end)
    }

    pub fn text_props_intervals_snapshot(&self) -> Vec<PropertyInterval> {
        self.storage.borrow().text_props.intervals_snapshot()
    }

    pub fn adjust_text_props_for_insert(&self, pos: usize, len: usize) {
        self.storage
            .borrow_mut()
            .text_props
            .adjust_for_insert(pos, len);
    }

    pub fn adjust_text_props_for_delete(&self, start: usize, end: usize) {
        self.storage
            .borrow_mut()
            .text_props
            .adjust_for_delete(start, end);
    }

    pub fn trace_text_prop_roots(&self, roots: &mut Vec<Value>) {
        self.storage.borrow().text_props.trace_roots(roots);
    }

    pub fn register_marker(
        &self,
        buffer_id: BufferId,
        marker_id: u64,
        byte_pos: usize,
        char_pos: usize,
        insertion_type: InsertionType,
    ) {
        let mut storage = self.storage.borrow_mut();
        storage.markers.retain(|marker| marker.id != marker_id);
        storage.markers.push(MarkerEntry {
            id: marker_id,
            buffer_id,
            byte_pos,
            char_pos,
            insertion_type,
        });
    }

    pub fn marker_entry(&self, marker_id: u64) -> Option<MarkerEntry> {
        self.storage
            .borrow()
            .markers
            .iter()
            .find(|marker| marker.id == marker_id)
            .cloned()
    }

    pub fn remove_marker(&self, marker_id: u64) {
        self.storage
            .borrow_mut()
            .markers
            .retain(|marker| marker.id != marker_id);
    }

    pub fn update_marker_insertion_type(&self, marker_id: u64, insertion_type: InsertionType) {
        let mut storage = self.storage.borrow_mut();
        let Some(marker) = storage
            .markers
            .iter_mut()
            .find(|marker| marker.id == marker_id)
        else {
            return;
        };
        marker.insertion_type = insertion_type;
    }

    pub fn adjust_markers_for_insert(&self, insert_pos: usize, byte_len: usize, char_len: usize) {
        if byte_len == 0 {
            return;
        }
        for marker in &mut self.storage.borrow_mut().markers {
            if marker.byte_pos > insert_pos {
                marker.byte_pos += byte_len;
                marker.char_pos += char_len;
            } else if marker.byte_pos == insert_pos && marker.insertion_type == InsertionType::After
            {
                marker.byte_pos += byte_len;
                marker.char_pos += char_len;
            }
        }
    }

    pub fn adjust_markers_for_delete(
        &self,
        start: usize,
        end: usize,
        start_char: usize,
        end_char: usize,
    ) {
        if start >= end {
            return;
        }
        let byte_len = end - start;
        let char_len = end_char - start_char;
        for marker in &mut self.storage.borrow_mut().markers {
            if marker.byte_pos >= end {
                marker.byte_pos -= byte_len;
                marker.char_pos -= char_len;
            } else if marker.byte_pos > start {
                marker.byte_pos = start;
                marker.char_pos = start_char;
            }
        }
    }

    pub fn advance_markers_at(&self, pos: usize, byte_len: usize, char_len: usize) {
        if byte_len == 0 {
            return;
        }
        for marker in &mut self.storage.borrow_mut().markers {
            if marker.byte_pos == pos {
                marker.byte_pos += byte_len;
                marker.char_pos += char_len;
            }
        }
    }

    pub fn clear_markers(&self) {
        self.storage.borrow_mut().markers.clear();
    }

    pub fn remove_markers_for_buffers(&self, killed: &std::collections::HashSet<BufferId>) {
        self.storage
            .borrow_mut()
            .markers
            .retain(|marker| !killed.contains(&marker.buffer_id));
    }

    pub fn marker_entries_snapshot(&self) -> Vec<MarkerEntry> {
        self.storage.borrow().markers.clone()
    }

    /// Convert a character position to a logical Emacs byte offset using an
    /// anchor-bracketed cached search. Mirrors GNU `buf_charpos_to_bytepos`
    /// (`src/marker.c:167`).
    pub fn buf_charpos_to_bytepos(&self, target: usize) -> usize {
        let storage = self.storage.borrow();
        let total_chars = storage.gap.char_count();
        let total_bytes = storage.gap.emacs_byte_len();

        if target >= total_chars {
            return storage.gap.len();
        }

        // Unibyte fast path: char == byte, no scan needed.
        if total_chars == total_bytes {
            return target;
        }

        // Wholesale-invalidate the anchor cache when the buffer changed.
        let current_key = (total_chars, total_bytes);
        if storage.anchor_cache_key.get() != current_key {
            storage.anchor_cache.borrow_mut().clear();
            storage.anchor_cache_key.set(current_key);
        }

        let mut best_below: (usize, usize) = (0, 0);
        let mut best_above: (usize, usize) = (total_chars, total_bytes);

        let gpt = storage.gap.gpt();
        let gpt_byte = storage.gap.gpt_byte();
        consider_anchor(target, (gpt, gpt_byte), &mut best_below, &mut best_above);

        let cached = storage.pos_cache.get();
        if cached.epoch_chars == total_chars
            && cached.epoch_bytes == total_bytes
            && (cached.epoch_chars != 0 || cached.epoch_bytes != 0)
        {
            consider_anchor(
                target,
                (cached.charpos, cached.bytepos),
                &mut best_below,
                &mut best_above,
            );
        }

        for &(cp, bp) in storage.anchor_cache.borrow().iter() {
            consider_anchor(target, (cp, bp), &mut best_below, &mut best_above);
        }

        let mut distance: usize = POSITION_DISTANCE_BASE;
        for m in &storage.markers {
            consider_anchor(target, (m.char_pos, m.byte_pos), &mut best_below, &mut best_above);
            if best_above.0.saturating_sub(target) < distance
                || target.saturating_sub(best_below.0) < distance
            {
                break;
            }
            distance = distance.saturating_add(POSITION_DISTANCE_INCR);
        }

        let walked_below = target.saturating_sub(best_below.0);
        let walked_above = best_above.0.saturating_sub(target);
        let result = if walked_below <= walked_above {
            scan_forward(&storage.gap, best_below, target)
        } else {
            scan_backward(&storage.gap, best_above, target)
        };

        // Mirror GNU marker.c:238-241: insert an anchor when the scan actually
        // walked more than POSITION_ANCHOR_STRIDE positions.
        let walked = walked_below.min(walked_above);
        if walked > POSITION_ANCHOR_STRIDE {
            storage.anchor_cache.borrow_mut().push((target, result));
        }

        storage.pos_cache.set(PositionCache {
            epoch_chars: total_chars,
            epoch_bytes: total_bytes,
            charpos: target,
            bytepos: result,
        });
        result
    }

    /// Convert a logical Emacs byte position to a character position. Symmetric
    /// to `buf_charpos_to_bytepos` — shares the same anchor + cache machinery.
    pub fn buf_bytepos_to_charpos(&self, target: usize) -> usize {
        let storage = self.storage.borrow();
        let total_chars = storage.gap.char_count();
        let total_bytes = storage.gap.emacs_byte_len();

        if target >= total_bytes {
            return total_chars;
        }

        // Unibyte fast path: char == byte, no scan needed.
        if total_chars == total_bytes {
            return target;
        }

        // Wholesale-invalidate the anchor cache when the buffer changed.
        let current_key = (total_chars, total_bytes);
        if storage.anchor_cache_key.get() != current_key {
            storage.anchor_cache.borrow_mut().clear();
            storage.anchor_cache_key.set(current_key);
        }

        // Bracket is expressed as (bytepos, charpos) for this direction.
        let mut best_below: (usize, usize) = (0, 0);
        let mut best_above: (usize, usize) = (total_bytes, total_chars);

        let gpt = storage.gap.gpt();
        let gpt_byte = storage.gap.gpt_byte();
        consider_anchor_byte(target, (gpt_byte, gpt), &mut best_below, &mut best_above);

        let cached = storage.pos_cache.get();
        if cached.epoch_chars == total_chars
            && cached.epoch_bytes == total_bytes
            && (cached.epoch_chars != 0 || cached.epoch_bytes != 0)
        {
            consider_anchor_byte(
                target,
                (cached.bytepos, cached.charpos),
                &mut best_below,
                &mut best_above,
            );
        }

        for &(cp, bp) in storage.anchor_cache.borrow().iter() {
            consider_anchor_byte(target, (bp, cp), &mut best_below, &mut best_above);
        }

        let mut distance: usize = POSITION_DISTANCE_BASE;
        for m in &storage.markers {
            consider_anchor_byte(target, (m.byte_pos, m.char_pos), &mut best_below, &mut best_above);
            if best_above.0.saturating_sub(target) < distance
                || target.saturating_sub(best_below.0) < distance
            {
                break;
            }
            distance = distance.saturating_add(POSITION_DISTANCE_INCR);
        }

        let walked_below = target.saturating_sub(best_below.0);
        let walked_above = best_above.0.saturating_sub(target);
        let result = if walked_below <= walked_above {
            scan_forward_bytes(&storage.gap, best_below, target)
        } else {
            scan_backward_bytes(&storage.gap, best_above, target)
        };

        // Mirror GNU marker.c:238-241: insert an anchor when the scan actually
        // walked more than POSITION_ANCHOR_STRIDE positions.
        // Store as (charpos, bytepos) like the char→byte direction to keep
        // anchor_cache entries in one canonical order.
        let walked = walked_below.min(walked_above);
        if walked > POSITION_ANCHOR_STRIDE {
            storage.anchor_cache.borrow_mut().push((result, target));
        }

        storage.pos_cache.set(PositionCache {
            epoch_chars: total_chars,
            epoch_bytes: total_bytes,
            charpos: result,
            bytepos: target,
        });
        result
    }

    #[cfg(test)]
    pub fn anchor_cache_len(&self) -> usize {
        self.storage.borrow().anchor_cache.borrow().len()
    }
}

impl fmt::Display for BufferText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.storage.borrow().gap.fmt(f)
    }
}

impl fmt::Debug for BufferText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BufferText")
            .field("len", &self.len())
            .field("chars", &self.char_count())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Position conversion helpers
// ---------------------------------------------------------------------------

/// GNU `marker.c:162` — initial bracket-bail distance.
const POSITION_DISTANCE_BASE: usize = 50;
/// GNU `marker.c:162` — bracket-bail distance grows by this per marker checked.
const POSITION_DISTANCE_INCR: usize = 50;
/// Auto-insert an anchor when a scan walks more than this many positions.
/// Mirrors GNU `marker.c:238-241` (5000-char threshold).
const POSITION_ANCHOR_STRIDE: usize = 5000;

/// Update `(best_below, best_above)` in place using a new `(charpos, bytepos)` anchor.
fn consider_anchor(
    target: usize,
    anchor: (usize, usize),
    best_below: &mut (usize, usize),
    best_above: &mut (usize, usize),
) {
    if anchor.0 <= target && anchor.0 > best_below.0 {
        *best_below = anchor;
    }
    if anchor.0 >= target && anchor.0 < best_above.0 {
        *best_above = anchor;
    }
}

/// Walk forward from `anchor = (charpos, bytepos)` to reach `target` chars.
/// Returns the byte position.
fn scan_forward(gap: &GapBuffer, anchor: (usize, usize), target: usize) -> usize {
    let (mut cp, mut bp) = anchor;
    while cp < target {
        if !gap.is_multibyte() {
            bp += 1;
            cp += 1;
            continue;
        }
        let mut tmp = [0u8; crate::emacs_core::emacs_char::MAX_MULTIBYTE_LENGTH];
        let available = (gap.len() - bp).min(tmp.len());
        for (i, slot) in tmp[..available].iter_mut().enumerate() {
            *slot = gap.byte_at(bp + i);
        }
        let (_, len) = crate::emacs_core::emacs_char::string_char(&tmp[..available]);
        bp += len;
        cp += 1;
    }
    bp
}

/// Walk backward from `anchor = (charpos, bytepos)` to reach `target` chars.
/// Returns the byte position.
fn scan_backward(gap: &GapBuffer, anchor: (usize, usize), target: usize) -> usize {
    let (mut cp, mut bp) = anchor;
    while cp > target {
        if !gap.is_multibyte() {
            bp -= 1;
            cp -= 1;
            continue;
        }
        let mut prev = bp - 1;
        while prev > 0 && (gap.byte_at(prev) & 0xC0) == 0x80 {
            prev -= 1;
        }
        bp = prev;
        cp -= 1;
    }
    bp
}

/// Update `(best_below, best_above)` in place using a new `(bytepos, charpos)` anchor.
fn consider_anchor_byte(
    target: usize,
    anchor: (usize, usize), // (bytepos, charpos)
    best_below: &mut (usize, usize),
    best_above: &mut (usize, usize),
) {
    if anchor.0 <= target && anchor.0 > best_below.0 {
        *best_below = anchor;
    }
    if anchor.0 >= target && anchor.0 < best_above.0 {
        *best_above = anchor;
    }
}

/// Walk forward from `anchor = (bytepos, charpos)` to reach `target` bytepos.
/// Returns the char position.
fn scan_forward_bytes(gap: &GapBuffer, anchor: (usize, usize), target: usize) -> usize {
    let (mut bp, mut cp) = anchor;
    while bp < target {
        if !gap.is_multibyte() {
            bp += 1;
            cp += 1;
            continue;
        }
        let mut tmp = [0u8; crate::emacs_core::emacs_char::MAX_MULTIBYTE_LENGTH];
        let available = (gap.len() - bp).min(tmp.len());
        for (i, slot) in tmp[..available].iter_mut().enumerate() {
            *slot = gap.byte_at(bp + i);
        }
        let (_, len) = crate::emacs_core::emacs_char::string_char(&tmp[..available]);
        bp += len;
        cp += 1;
    }
    cp
}

/// Walk backward from `anchor = (bytepos, charpos)` to reach `target` bytepos.
/// Returns the char position.
fn scan_backward_bytes(gap: &GapBuffer, anchor: (usize, usize), target: usize) -> usize {
    let (mut bp, mut cp) = anchor;
    while bp > target {
        if !gap.is_multibyte() {
            bp -= 1;
            cp -= 1;
            continue;
        }
        let mut prev = bp - 1;
        while prev > 0 && (gap.byte_at(prev) & 0xC0) == 0x80 {
            prev -= 1;
        }
        bp = prev;
        cp -= 1;
    }
    cp
}

#[cfg(test)]
#[path = "buffer_text_test.rs"]
mod tests;
