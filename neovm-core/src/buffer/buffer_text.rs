//! Buffer text storage.
//!
//! GNU Emacs separates per-buffer metadata from the underlying text object.
//! `BufferText` is the first local seam toward that design. Today it is a thin
//! wrapper around `GapBuffer`; later it can absorb shared text state, char/byte
//! caches, and interval ownership without forcing another tree-wide field-type
//! rewrite.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

use crate::emacs_core::value::Value;
use crate::gc_trace::GcTrace;

use super::buffer::{BufferId, InsertionType, MarkerEntry};
use super::gap_buffer::GapBuffer;
use super::text_props::{PropertyInterval, TextPropertyTable};

#[derive(Clone)]
struct BufferTextStorage {
    gap: GapBuffer,
    text_props: TextPropertyTable,
    markers: Vec<MarkerEntry>,
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
    pub fn new() -> Self {
        Self {
            storage: Rc::new(RefCell::new(BufferTextStorage {
                gap: GapBuffer::new(),
                text_props: TextPropertyTable::new(),
                markers: Vec::new(),
            })),
        }
    }

    pub fn from_str(text: &str) -> Self {
        Self {
            storage: Rc::new(RefCell::new(BufferTextStorage {
                gap: GapBuffer::from_str(text),
                text_props: TextPropertyTable::new(),
                markers: Vec::new(),
            })),
        }
    }

    pub fn len(&self) -> usize {
        self.storage.borrow().gap.len()
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

    pub fn to_string(&self) -> String {
        self.storage.borrow().gap.to_string()
    }

    pub fn insert_str(&mut self, pos: usize, text: &str) {
        if text.is_empty() {
            return;
        }
        self.storage.borrow_mut().gap.insert_str(pos, text);
    }

    pub fn delete_range(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        self.storage.borrow_mut().gap.delete_range(start, end);
    }

    pub fn replace_same_len_range(&mut self, start: usize, end: usize, replacement: &str) {
        if start >= end {
            return;
        }
        self.storage
            .borrow_mut()
            .gap
            .replace_same_len_range(start, end, replacement);
    }

    pub fn byte_to_char(&self, byte_pos: usize) -> usize {
        self.storage.borrow().gap.byte_to_char(byte_pos)
    }

    pub fn char_to_byte(&self, char_pos: usize) -> usize {
        self.storage.borrow().gap.char_to_byte(char_pos)
    }

    pub fn emacs_byte_to_char(&self, byte_pos: usize) -> usize {
        self.storage.borrow().gap.emacs_byte_to_char(byte_pos)
    }

    pub fn char_to_emacs_byte(&self, char_pos: usize) -> usize {
        self.storage.borrow().gap.char_to_emacs_byte(char_pos)
    }

    pub fn storage_byte_to_emacs_byte(&self, byte_pos: usize) -> usize {
        self.storage
            .borrow()
            .gap
            .storage_byte_to_emacs_byte(byte_pos)
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

    pub(crate) fn from_dump(text: Vec<u8>) -> Self {
        Self {
            storage: Rc::new(RefCell::new(BufferTextStorage {
                gap: GapBuffer::from_dump(text),
                text_props: TextPropertyTable::new(),
                markers: Vec::new(),
            })),
        }
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
        self.storage
            .borrow_mut()
            .gap
            .replace_same_len_range(start, end, &replacement);
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
        text_props: TextPropertyTable,
        markers: Vec<MarkerEntry>,
    ) {
        let mut storage = self.storage.borrow_mut();
        storage.gap = GapBuffer::from_str(text);
        storage.text_props = text_props;
        storage.markers = markers;
    }

    pub fn text_props_put_property(
        &self,
        start: usize,
        end: usize,
        name: &str,
        value: Value,
    ) -> bool {
        self.storage
            .borrow_mut()
            .text_props
            .put_property(start, end, name, value)
    }

    pub fn text_props_get_property(&self, pos: usize, name: &str) -> Option<Value> {
        self.storage
            .borrow()
            .text_props
            .get_property(pos, name)
            .copied()
    }

    pub fn text_props_get_properties(&self, pos: usize) -> HashMap<String, Value> {
        self.storage.borrow().text_props.get_properties(pos)
    }

    pub fn text_props_get_properties_ordered(&self, pos: usize) -> Vec<(String, Value)> {
        self.storage.borrow().text_props.get_properties_ordered(pos)
    }

    pub fn text_props_remove_property(&self, start: usize, end: usize, name: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::BufferText;

    #[test]
    fn char_count_tracks_multibyte_inserts_and_deletes() {
        crate::test_utils::init_test_tracing();
        let mut text = BufferText::from_str("ééz");
        assert_eq!(text.char_count(), 3);

        text.insert_str('é'.len_utf8(), "ß");
        assert_eq!(text.char_count(), 4);

        text.delete_range(2, 4);
        assert_eq!(text.char_count(), 3);
        assert_eq!(text.to_string(), "ééz");
    }

    #[test]
    fn shared_clone_observes_cached_char_count_updates() {
        crate::test_utils::init_test_tracing();
        let mut text = BufferText::from_str("ab");
        let shared = text.shared_clone();
        text.insert_str(2, "é");
        assert_eq!(text.char_count(), 3);
        assert_eq!(shared.char_count(), 3);
    }

    #[test]
    fn deep_clone_keeps_independent_char_count_cache() {
        crate::test_utils::init_test_tracing();
        let mut text = BufferText::from_str("ab");
        let cloned = text.clone();
        text.insert_str(2, "é");
        assert_eq!(text.char_count(), 3);
        assert_eq!(cloned.char_count(), 2);
    }
}
