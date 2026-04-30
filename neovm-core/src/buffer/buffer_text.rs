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

use super::buffer::{BufferId, InsertionType};
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

struct BufferTextStorage {
    layout: BufferTextLayout,
    gap: GapBuffer,
    modified_tick: i64,
    chars_modified_tick: i64,
    save_modified_tick: i64,
    text_props: TextPropertyTable,
    /// Head of the intrusive per-buffer marker chain (GNU `buffer->own_text.markers`).
    /// Authoritative since T6; the parallel `Vec<MarkerEntry>` was deleted in T7.
    markers_head: *mut crate::tagged::header::MarkerObj,
    /// Interior-mutable last-query cache for char↔byte conversion.
    pos_cache: Cell<PositionCache>,
    /// Internal (non-Lisp-visible) anchor positions populated on long scans.
    /// Invalidated wholesale when `(total_chars, total_bytes)` advances.
    anchor_cache: RefCell<Vec<(usize, usize)>>,
    /// `(epoch_chars, epoch_bytes)` at which the anchor_cache is valid.
    /// Mismatch triggers a wholesale clear on next read.
    anchor_cache_key: Cell<(usize, usize)>,
}

impl Clone for BufferTextStorage {
    fn clone(&self) -> Self {
        Self {
            layout: self.layout.clone(),
            gap: self.gap.clone(),
            modified_tick: self.modified_tick,
            chars_modified_tick: self.chars_modified_tick,
            save_modified_tick: self.save_modified_tick,
            text_props: self.text_props.clone(),
            // Chain head intentionally not cloned: chain pointers are unique
            // per TaggedHeap; a cloned buffer starts with an empty chain and
            // rebuilds it via register_marker.
            markers_head: std::ptr::null_mut(),
            pos_cache: self.pos_cache.clone(),
            anchor_cache: self.anchor_cache.clone(),
            anchor_cache_key: self.anchor_cache_key.clone(),
        }
    }
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
    fn from_gap(gap: GapBuffer) -> Self {
        Self {
            storage: Rc::new(RefCell::new(BufferTextStorage {
                layout: Self::layout_from_gap(&gap),
                gap,
                modified_tick: 1,
                chars_modified_tick: 1,
                save_modified_tick: 1,
                text_props: TextPropertyTable::new(),
                markers_head: std::ptr::null_mut(),
                pos_cache: Cell::new(PositionCache::default()),
                anchor_cache: RefCell::new(Vec::new()),
                anchor_cache_key: Cell::new((0, 0)),
            })),
        }
    }

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
        Self::from_gap(GapBuffer::new())
    }

    pub fn from_str(text: &str) -> Self {
        Self::from_gap(GapBuffer::from_str(text))
    }

    pub fn from_lisp_string(text: &crate::heap_types::LispString) -> Self {
        Self::from_gap(GapBuffer::from_emacs_bytes(
            text.as_bytes(),
            text.is_multibyte(),
        ))
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

    pub fn has_contiguous_emacs_bytes(&self, start: usize, end: usize) -> bool {
        self.storage
            .borrow()
            .gap
            .has_contiguous_emacs_bytes(start, end)
    }

    pub fn with_contiguous_emacs_bytes<R>(
        &self,
        start: usize,
        end: usize,
        f: impl FnOnce(&[u8]) -> R,
    ) -> Option<R> {
        self.storage
            .borrow()
            .gap
            .with_contiguous_emacs_bytes(start, end, f)
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
        Self::from_gap(GapBuffer::from_dump(text, multibyte))
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
        // Walk buffer bytes directly, avoiding the storage-form conversion
        // previously done through text_range(). For multibyte buffers each
        // Emacs char is decoded via emacs_char::string_char; for unibyte
        // buffers each byte is one "character" in the range 0..=0xFF.
        let mut bytes = Vec::with_capacity(end - start);
        self.storage
            .borrow()
            .gap
            .copy_bytes_to(start, end, &mut bytes);
        if self.is_multibyte() {
            let mut pos = 0;
            while pos < bytes.len() {
                let (c, len) = crate::emacs_core::emacs_char::string_char(&bytes[pos..]);
                if c == code {
                    return true;
                }
                pos += len.max(1);
            }
            false
        } else {
            if code > 0xFF {
                return false;
            }
            bytes.iter().any(|&b| b as u32 == code)
        }
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

    pub fn replace_storage(&self, text: &str, multibyte: bool, text_props: TextPropertyTable) {
        let bytes =
            crate::emacs_core::string_escape::storage_string_to_buffer_bytes(text, multibyte);
        let string = if multibyte {
            crate::heap_types::LispString::from_emacs_bytes(bytes)
        } else {
            crate::heap_types::LispString::from_unibyte(bytes)
        };
        self.replace_lisp_string(&string, text_props);
    }
    pub fn replace_lisp_string(
        &self,
        text: &crate::heap_types::LispString,
        text_props: TextPropertyTable,
    ) {
        let mut storage = self.storage.borrow_mut();
        storage.gap = GapBuffer::from_emacs_bytes(text.as_bytes(), text.is_multibyte());
        storage.layout = Self::layout_from_gap(&storage.gap);
        storage.text_props = text_props;
        // Wholesale content replacement: invalidate position caches. If the new
        // content happens to have the same (total_chars, total_bytes) as the old,
        // a stale pos_cache entry could otherwise return a wrong bytepos.
        storage.pos_cache.set(PositionCache::default());
        storage.anchor_cache.borrow_mut().clear();
        storage.anchor_cache_key.set((0, 0));
    }

    /// Walk the intrusive marker chain and remap each marker's (bytepos,
    /// charpos) through the caller-supplied closure. Used by
    /// `set-buffer-multibyte` to translate marker positions across a
    /// wholesale gap-buffer replacement (the boundary arithmetic lives in
    /// the caller; this helper only handles chain traversal).
    ///
    /// The closure receives the marker's current `bytepos` and returns
    /// the new `(bytepos, charpos)` pair.
    pub fn remap_markers_through<F>(&self, mut remap: F)
    where
        F: FnMut(usize) -> (usize, usize),
    {
        let storage = self.storage.borrow();
        let mut curr = storage.markers_head;
        // SAFETY: `curr` walks live chain-owned MarkerObj pointers from
        // `markers_head` until null. Each non-null node was spliced in via
        // `chain_splice_at_head`, so its `data.next_marker` is a valid
        // chain link or null.
        unsafe {
            while !curr.is_null() {
                let data = &mut (*curr).data;
                let (new_byte, new_char) = remap(data.bytepos);
                data.bytepos = new_byte;
                data.charpos = new_char;
                curr = data.next_marker;
            }
        }
    }

    pub fn text_props_put_property(
        &self,
        start: usize,
        end: usize,
        name: Value,
        value: Value,
    ) -> bool {
        // GNU intervals are character-indexed; BufferText owns the conversion
        // from buffer byte offsets into interval positions.
        let start = self.buf_bytepos_to_charpos(start);
        let end = self.buf_bytepos_to_charpos(end);
        self.storage
            .borrow_mut()
            .text_props
            .put_property(start, end, name, value)
    }

    pub fn text_props_get_property(&self, pos: usize, name: Value) -> Option<Value> {
        let pos = self.buf_bytepos_to_charpos(pos);
        self.storage
            .borrow()
            .text_props
            .get_property(pos, name)
            .copied()
    }

    pub fn text_props_get_properties(&self, pos: usize) -> HashMap<Value, Value> {
        let pos = self.buf_bytepos_to_charpos(pos);
        self.storage.borrow().text_props.get_properties(pos)
    }

    pub fn text_props_get_properties_ordered(&self, pos: usize) -> Vec<(Value, Value)> {
        let pos = self.buf_bytepos_to_charpos(pos);
        self.storage.borrow().text_props.get_properties_ordered(pos)
    }

    pub fn text_props_remove_property(&self, start: usize, end: usize, name: Value) -> bool {
        let start = self.buf_bytepos_to_charpos(start);
        let end = self.buf_bytepos_to_charpos(end);
        self.storage
            .borrow_mut()
            .text_props
            .remove_property(start, end, name)
    }

    pub fn text_props_remove_all(&self, start: usize, end: usize) {
        let start = self.buf_bytepos_to_charpos(start);
        let end = self.buf_bytepos_to_charpos(end);
        self.storage
            .borrow_mut()
            .text_props
            .remove_all_properties(start, end);
    }

    pub fn text_props_next_change(&self, pos: usize) -> Option<usize> {
        let char_pos = self.buf_bytepos_to_charpos(pos);
        let next = {
            self.storage
                .borrow()
                .text_props
                .next_property_change(char_pos)
        };
        next.map(|next| self.buf_charpos_to_bytepos(next))
    }

    pub fn text_props_previous_change(&self, pos: usize) -> Option<usize> {
        let char_pos = self.buf_bytepos_to_charpos(pos);
        let prev = {
            self.storage
                .borrow()
                .text_props
                .previous_property_change(char_pos)
        };
        prev.map(|prev| self.buf_charpos_to_bytepos(prev))
    }

    pub fn text_props_append_shifted(&self, other: &TextPropertyTable, byte_offset: usize) {
        let char_offset = self.buf_bytepos_to_charpos(byte_offset);
        self.storage
            .borrow_mut()
            .text_props
            .append_shifted(other, char_offset);
    }

    pub fn text_props_merge_missing_shifted(&self, other: &TextPropertyTable, byte_offset: usize) {
        let char_offset = {
            let storage = self.storage.borrow();
            storage.gap.byte_to_char(byte_offset)
        };
        self.storage
            .borrow_mut()
            .text_props
            .merge_missing_shifted(other, char_offset);
    }

    pub fn text_props_slice(&self, start: usize, end: usize) -> TextPropertyTable {
        let start = self.buf_bytepos_to_charpos(start);
        let end = self.buf_bytepos_to_charpos(end);
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

    /// Register a marker in this buffer. Updates `MarkerData` fields
    /// authoritatively (buffer/bytepos/charpos/marker_id/insertion_type)
    /// and splices the marker into this buffer's intrusive chain at head.
    ///
    /// **Precondition:** `marker_ptr.data.next_marker` is null, i.e. the
    /// marker is not currently on any chain. Callers re-binding a marker
    /// must `chain_unlink` from the old buffer first; the
    /// `debug_assert!` in `chain_splice_at_head` catches violations.
    pub fn register_marker(
        &self,
        marker_ptr: *mut crate::tagged::header::MarkerObj,
        buffer_id: BufferId,
        marker_id: u64,
        byte_pos: usize,
        char_pos: usize,
        insertion_type: InsertionType,
    ) {
        // Update MarkerData so its fields are authoritative before the
        // chain ever exposes this marker.
        //
        // SAFETY: `marker_ptr` is a live MarkerObj allocated via
        // `TaggedHeap::alloc_marker`; writes through a raw pointer are
        // sound for the heap's lifetime. The chain precondition is
        // enforced by `chain_splice_at_head`'s debug_assert below.
        unsafe {
            (*marker_ptr).data.buffer = Some(buffer_id);
            (*marker_ptr).data.marker_id = Some(marker_id);
            (*marker_ptr).data.bytepos = byte_pos;
            (*marker_ptr).data.charpos = char_pos;
            (*marker_ptr).data.last_position_valid = true;
            (*marker_ptr).data.insertion_type = insertion_type == InsertionType::After;
        }
        self.chain_splice_at_head(marker_ptr);
    }

    /// Walk the intrusive marker chain head→tail and invoke `f` on each
    /// live `MarkerData` by reference. Read-only counterpart to
    /// `chain_walk_mut`; used by pdump (v26) to serialize the chain
    /// without materializing an intermediate Vec.
    ///
    /// SAFETY: walks live chain-owned MarkerObj pointers from
    /// `storage.markers_head` until null; each `(*curr).data` reference
    /// stays valid for the duration of the call because the GC sweep
    /// runs `unchain_dead_markers` between mark and free.
    pub fn chain_walk_data<F: FnMut(&crate::heap_types::MarkerData)>(&self, mut f: F) {
        let storage = self.storage.borrow();
        let mut curr = storage.markers_head;
        unsafe {
            while !curr.is_null() {
                let data = &(*curr).data;
                f(data);
                curr = data.next_marker;
            }
        }
    }

    /// Walk this buffer's intrusive marker chain and return the raw
    /// MarkerObj pointer for the first node whose `marker_id` matches,
    /// or null when none found. Used by pdump load (v26) to resolve
    /// `BufferStateMarkers` (pt/begv/zv) ids back to chain pointers
    /// after the chain has been reconstructed.
    ///
    /// Pointer lifetime: the returned `*mut MarkerObj` is only valid while
    /// `self`'s chain still holds it. Any subsequent splice/unlink on this
    /// buffer's chain, or a GC cycle that runs `unchain_dead_markers`, may
    /// detach the node from the chain — callers must use the pointer
    /// before doing anything that could mutate the chain, and must not
    /// re-enter the chain (or invoke arbitrary Lisp) between lookup and
    /// use.
    pub fn chain_find_by_id(&self, marker_id: u64) -> *mut crate::tagged::header::MarkerObj {
        let storage = self.storage.borrow();
        let mut curr = storage.markers_head;
        // SAFETY: chain walks live chain-owned MarkerObj pointers from
        // `storage.markers_head` until null.
        unsafe {
            while !curr.is_null() {
                if (*curr).data.marker_id == Some(marker_id) {
                    return curr;
                }
                curr = (*curr).data.next_marker;
            }
        }
        std::ptr::null_mut()
    }

    /// Walk the intrusive chain and return the MarkerData-derived fields
    /// `(bytepos, charpos, insertion_type)` for the marker with the given
    /// id, or `None` if no live chain node carries that id.
    ///
    /// Production code should prefer reading `MarkerData` directly off a
    /// Lisp `Value`. This helper exists for internal buffer-manager
    /// callers (e.g. `clone_marker_in_buffer`) that track markers by id
    /// without holding the Lisp value.
    pub fn marker_chain_lookup(&self, marker_id: u64) -> Option<(usize, usize, InsertionType)> {
        let storage = self.storage.borrow();
        let mut curr = storage.markers_head;
        // SAFETY: chain walks live chain-owned MarkerObj pointers until null.
        unsafe {
            while !curr.is_null() {
                let data = &(*curr).data;
                if data.marker_id == Some(marker_id) {
                    let ins = if data.insertion_type {
                        InsertionType::After
                    } else {
                        InsertionType::Before
                    };
                    return Some((data.bytepos, data.charpos, ins));
                }
                curr = data.next_marker;
            }
        }
        None
    }

    pub fn remove_marker(&self, marker_id: u64) {
        // Post-T8: `unchain_dead_markers` splices unmarked MarkerObjs
        // out of this buffer's chain between the mark and sweep GC
        // phases, so a chain walk between GC cycles never dereferences
        // a freed allocation. Walk the chain directly and splice the
        // matching node.
        let marker_ptr: Option<*mut crate::tagged::header::MarkerObj> = {
            let storage = self.storage.borrow();
            let mut curr = storage.markers_head;
            let mut found = None;
            // SAFETY: chain walks live chain-owned MarkerObj pointers
            // from `storage.markers_head` until null.
            unsafe {
                while !curr.is_null() {
                    if (*curr).data.marker_id == Some(marker_id) {
                        found = Some(curr);
                        break;
                    }
                    curr = (*curr).data.next_marker;
                }
            }
            found
        };
        if let Some(ptr) = marker_ptr {
            self.chain_unlink(ptr);
            // SAFETY: `ptr` was read from this buffer's chain; chain-
            // owned allocations stay live until the next GC sweep.
            // `chain_unlink` left it detached; field writes are sound.
            unsafe {
                (*ptr).data.buffer = None;
                // GNU `unchain_marker` (marker.c:684) preserves charpos so
                // `marker-last-position` can still report the marker's last
                // attached location.  `last_position_valid` stays true.
            }
        }
    }

    pub fn update_marker_insertion_type(&self, marker_id: u64, insertion_type: InsertionType) {
        let storage = self.storage.borrow();
        let mut curr = storage.markers_head;
        // SAFETY: chain walks live chain-owned MarkerObj pointers until null.
        unsafe {
            while !curr.is_null() {
                if (*curr).data.marker_id == Some(marker_id) {
                    (*curr).data.insertion_type = insertion_type == InsertionType::After;
                    return;
                }
                curr = (*curr).data.next_marker;
            }
        }
    }

    /// Return true iff a marker with `marker_id` is currently spliced
    /// into this buffer's chain. Used by BufferManager to pick the
    /// correct buffer when updating insertion type across buffers.
    pub fn has_marker(&self, marker_id: u64) -> bool {
        let storage = self.storage.borrow();
        let mut curr = storage.markers_head;
        // SAFETY: chain walks live chain-owned MarkerObj pointers until null.
        unsafe {
            while !curr.is_null() {
                if (*curr).data.marker_id == Some(marker_id) {
                    return true;
                }
                curr = (*curr).data.next_marker;
            }
        }
        false
    }

    pub fn adjust_markers_for_insert(&self, insert_pos: usize, byte_len: usize, char_len: usize) {
        if byte_len == 0 {
            return;
        }
        let storage = self.storage.borrow();
        let mut curr = storage.markers_head;
        // SAFETY: `curr` walks live chain-owned MarkerObj pointers from
        // `markers_head` until null. Each non-null node was spliced in via
        // `chain_splice_at_head`, so its `data.next_marker` is a valid
        // chain link or null.
        unsafe {
            while !curr.is_null() {
                let data = &mut (*curr).data;
                if data.bytepos > insert_pos {
                    data.bytepos += byte_len;
                    data.charpos += char_len;
                } else if data.bytepos == insert_pos && data.insertion_type {
                    // insertion_type == true means "after" in GNU terms.
                    data.bytepos += byte_len;
                    data.charpos += char_len;
                }
                curr = data.next_marker;
            }
        }
    }

    /// Like `adjust_markers_for_insert`, but ignores `insertion_type` for
    /// markers AT `insert_pos`. Used by the GNU-equivalent replace path,
    /// where markers that ended up at `from_byte` (after the prior delete
    /// collapsed inside-region markers there) must NOT advance past the
    /// inserted text — matching GNU `adjust_markers_for_replace`
    /// (insdel.c:341).
    pub fn adjust_markers_for_insert_strict_after(
        &self,
        insert_pos: usize,
        byte_len: usize,
        char_len: usize,
    ) {
        if byte_len == 0 {
            return;
        }
        let storage = self.storage.borrow();
        let mut curr = storage.markers_head;
        // SAFETY: same invariant as adjust_markers_for_insert.
        unsafe {
            while !curr.is_null() {
                let data = &mut (*curr).data;
                if data.bytepos > insert_pos {
                    data.bytepos += byte_len;
                    data.charpos += char_len;
                }
                curr = data.next_marker;
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
        let storage = self.storage.borrow();
        let mut curr = storage.markers_head;
        // SAFETY: same invariant as adjust_markers_for_insert.
        unsafe {
            while !curr.is_null() {
                let data = &mut (*curr).data;
                if data.bytepos >= end {
                    data.bytepos -= byte_len;
                    data.charpos -= char_len;
                } else if data.bytepos > start {
                    data.bytepos = start;
                    data.charpos = start_char;
                }
                curr = data.next_marker;
            }
        }
    }

    pub fn advance_markers_at(&self, pos: usize, byte_len: usize, char_len: usize) {
        if byte_len == 0 {
            return;
        }
        let storage = self.storage.borrow();
        let mut curr = storage.markers_head;
        // SAFETY: same invariant as adjust_markers_for_insert.
        unsafe {
            while !curr.is_null() {
                let data = &mut (*curr).data;
                if data.bytepos == pos {
                    data.bytepos += byte_len;
                    data.charpos += char_len;
                }
                curr = data.next_marker;
            }
        }
    }

    /// Unlink every chain node whose `MarkerData.buffer` is in `killed`.
    /// Sole entry point for kill-buffer marker cleanup: covers both the
    /// kill-root case (killed_set contains the root and all its
    /// indirects, so every marker on the shared chain matches) and the
    /// kill-indirect case (killed_set is just the dying indirect; root
    /// and sibling-indirect markers stay attached).
    pub fn remove_markers_for_buffers(&self, killed: &std::collections::HashSet<BufferId>) {
        let mut storage = self.storage.borrow_mut();
        let mut prev_slot: *mut *mut crate::tagged::header::MarkerObj = &mut storage.markers_head;
        // SAFETY: analogous to `chain_unlink`. Every non-null `*prev_slot`
        // was installed via `chain_splice_at_head`, i.e. a live GC-managed
        // MarkerObj with a valid `data.next_marker` link.
        unsafe {
            while !(*prev_slot).is_null() {
                let curr = *prev_slot;
                let data = &mut (*curr).data;
                let belongs_to_killed = data.buffer.map(|id| killed.contains(&id)).unwrap_or(false);
                if belongs_to_killed {
                    *prev_slot = data.next_marker;
                    data.next_marker = std::ptr::null_mut();
                    data.buffer = None;
                    // Preserve charpos/bytepos and last_position_valid so
                    // `marker-last-position` keeps GNU semantics across
                    // kill-buffer (cf. unchain_marker, marker.c:684).
                } else {
                    prev_slot = &mut data.next_marker;
                }
            }
        }
    }

    /// Raw pointer to the `markers_head` slot inside this buffer's
    /// storage. ONLY for GC use — bypasses RefCell's runtime borrow
    /// checks. Callers must hold exclusive access to the tagged heap and
    /// have no outstanding storage borrows (GC is stop-the-world).
    ///
    /// Used by `TaggedHeap::unchain_dead_markers` to splice unmarked
    /// MarkerObj nodes out of the intrusive per-buffer chain before
    /// `sweep_objects` frees them.
    pub unsafe fn markers_head_slot_raw(&self) -> *mut *mut crate::tagged::header::MarkerObj {
        let storage_ptr: *mut BufferTextStorage = self.storage.as_ptr();
        unsafe { &mut (*storage_ptr).markers_head as *mut _ }
    }

    /// Splice `marker` at the head of this buffer's marker chain.
    /// Overwrites `marker.next_marker` with the old head.
    /// Caller sets `marker.buffer` / `marker.bytepos` / `marker.charpos` —
    /// this helper only manipulates chain topology.
    ///
    /// **Precondition:** `marker.next_marker` must be null (marker is not
    /// currently on any chain). Violating this silently truncates the
    /// other chain. `debug_assert!` enforces it in debug builds.
    pub fn chain_splice_at_head(&self, marker: *mut crate::tagged::header::MarkerObj) {
        let mut storage = self.storage.borrow_mut();
        let old_head = storage.markers_head;
        unsafe {
            // SAFETY: `marker` must be a live MarkerObj allocated via
            // TaggedHeap::alloc_marker and not currently on any other chain
            // (see precondition above). Writing through the pointer is sound
            // because the heap retains ownership for the lifetime of the
            // MarkerObj.
            debug_assert!(
                (*marker).data.next_marker.is_null(),
                "chain_splice_at_head: marker is already on a chain"
            );
            (*marker).data.next_marker = old_head;
        }
        storage.markers_head = marker;
    }

    /// Unlink `marker` from this buffer's chain. Silent no-op if not present.
    /// Does NOT clear `marker.buffer` / positions — caller owns semantic cleanup.
    ///
    /// Unlike GNU `unchain_marker` (marker.c:684), which hard-asserts that
    /// the marker is in the chain, we tolerate absent markers. This is
    /// defensive: callers currently include code paths that may be
    /// double-invoked during GC sweep and kill-buffer cleanup in T8/T9.
    pub fn chain_unlink(&self, marker: *mut crate::tagged::header::MarkerObj) {
        let mut storage = self.storage.borrow_mut();
        let mut prev_slot: *mut *mut crate::tagged::header::MarkerObj = &mut storage.markers_head;
        // SAFETY: `prev_slot` walks the intrusive chain starting at
        // `storage.markers_head`. Every non-null `*prev_slot` is a
        // `*mut MarkerObj` previously installed via `chain_splice_at_head`,
        // i.e. a live GC-managed allocation whose `.data.next_marker` is
        // the next chain slot. We never read past a null terminator, and
        // mutations only rewrite chain-owned `next_marker` fields.
        unsafe {
            while !(*prev_slot).is_null() {
                let curr = *prev_slot;
                if curr == marker {
                    *prev_slot = (*curr).data.next_marker;
                    (*curr).data.next_marker = std::ptr::null_mut();
                    return;
                }
                prev_slot = &mut (*curr).data.next_marker;
            }
        }
    }

    /// Walk the chain from head to tail, collecting raw pointers in order.
    /// Test-only helper.
    #[cfg(test)]
    pub fn chain_walk_collect(&self) -> Vec<*mut crate::tagged::header::MarkerObj> {
        let storage = self.storage.borrow();
        let mut out = Vec::new();
        let mut curr = storage.markers_head;
        // SAFETY: Same invariant as `chain_unlink` — `curr` walks live
        // chain-owned MarkerObj pointers from `storage.markers_head`
        // until a null terminator.
        unsafe {
            while !curr.is_null() {
                out.push(curr);
                curr = (*curr).data.next_marker;
            }
        }
        out
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
        // T7: marker chain walk. The chain carries the same (char, byte)
        // pairs that the deleted Vec<MarkerEntry> used to.
        //
        // SAFETY: `curr` walks live chain-owned MarkerObj pointers from
        // `storage.markers_head` until null. Each non-null node was
        // spliced in via `chain_splice_at_head`, so its `data.next_marker`
        // is a valid chain link or null.
        let mut curr = storage.markers_head;
        unsafe {
            while !curr.is_null() {
                let data = &(*curr).data;
                consider_anchor(
                    target,
                    (data.charpos, data.bytepos),
                    &mut best_below,
                    &mut best_above,
                );
                if best_above.0.saturating_sub(target) < distance
                    || target.saturating_sub(best_below.0) < distance
                {
                    break;
                }
                distance = distance.saturating_add(POSITION_DISTANCE_INCR);
                curr = data.next_marker;
            }
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
        // T7: marker chain walk. See sibling comment in
        // `buf_charpos_to_bytepos` for the SAFETY rationale.
        let mut curr = storage.markers_head;
        unsafe {
            while !curr.is_null() {
                let data = &(*curr).data;
                consider_anchor_byte(
                    target,
                    (data.bytepos, data.charpos),
                    &mut best_below,
                    &mut best_above,
                );
                if best_above.0.saturating_sub(target) < distance
                    || target.saturating_sub(best_below.0) < distance
                {
                    break;
                }
                distance = distance.saturating_add(POSITION_DISTANCE_INCR);
                curr = data.next_marker;
            }
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

#[cfg(test)]
#[path = "buffer_text_chain_test.rs"]
mod chain_test;
