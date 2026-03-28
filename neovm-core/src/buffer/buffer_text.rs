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
use crate::gc::GcTrace;

use super::gap_buffer::GapBuffer;
use super::text_props::{PropertyInterval, TextPropertyTable};

#[derive(Clone)]
struct BufferTextStorage {
    gap: GapBuffer,
    char_count: usize,
    text_props: TextPropertyTable,
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
                char_count: 0,
                text_props: TextPropertyTable::new(),
            })),
        }
    }

    pub fn from_str(text: &str) -> Self {
        Self {
            storage: Rc::new(RefCell::new(BufferTextStorage {
                gap: GapBuffer::from_str(text),
                char_count: text.chars().count(),
                text_props: TextPropertyTable::new(),
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
        self.storage.borrow().char_count
    }

    pub fn byte_at(&self, pos: usize) -> u8 {
        self.storage.borrow().gap.byte_at(pos)
    }

    pub fn char_at(&self, pos: usize) -> Option<char> {
        self.storage.borrow().gap.char_at(pos)
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
        let mut storage = self.storage.borrow_mut();
        storage.gap.insert_str(pos, text);
        storage.char_count += text.chars().count();
    }

    pub fn delete_range(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let mut storage = self.storage.borrow_mut();
        let deleted_chars = storage.gap.byte_to_char(end) - storage.gap.byte_to_char(start);
        storage.gap.delete_range(start, end);
        storage.char_count -= deleted_chars;
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
        let gap = GapBuffer::from_dump(text);
        let char_count = gap.char_count();
        Self {
            storage: Rc::new(RefCell::new(BufferTextStorage {
                gap,
                char_count,
                text_props: TextPropertyTable::new(),
            })),
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
        let mut text = BufferText::from_str("ab");
        let shared = text.shared_clone();
        text.insert_str(2, "é");
        assert_eq!(text.char_count(), 3);
        assert_eq!(shared.char_count(), 3);
    }

    #[test]
    fn deep_clone_keeps_independent_char_count_cache() {
        let mut text = BufferText::from_str("ab");
        let cloned = text.clone();
        text.insert_str(2, "é");
        assert_eq!(text.char_count(), 3);
        assert_eq!(cloned.char_count(), 2);
    }
}
