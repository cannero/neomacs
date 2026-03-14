//! Buffer text storage.
//!
//! GNU Emacs separates per-buffer metadata from the underlying text object.
//! `BufferText` is the first local seam toward that design. Today it is a thin
//! wrapper around `GapBuffer`; later it can absorb shared text state, char/byte
//! caches, and interval ownership without forcing another tree-wide field-type
//! rewrite.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use super::gap_buffer::GapBuffer;

pub struct BufferText {
    storage: Rc<RefCell<GapBuffer>>,
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
            storage: Rc::new(RefCell::new(GapBuffer::new())),
        }
    }

    pub fn from_str(text: &str) -> Self {
        Self {
            storage: Rc::new(RefCell::new(GapBuffer::from_str(text))),
        }
    }

    pub fn len(&self) -> usize {
        self.storage.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.storage.borrow().is_empty()
    }

    pub fn char_count(&self) -> usize {
        self.storage.borrow().char_count()
    }

    pub fn byte_at(&self, pos: usize) -> u8 {
        self.storage.borrow().byte_at(pos)
    }

    pub fn char_at(&self, pos: usize) -> Option<char> {
        self.storage.borrow().char_at(pos)
    }

    pub fn text_range(&self, start: usize, end: usize) -> String {
        self.storage.borrow().text_range(start, end)
    }

    pub fn copy_bytes_to(&self, start: usize, end: usize, out: &mut Vec<u8>) {
        self.storage.borrow().copy_bytes_to(start, end, out);
    }

    pub fn to_string(&self) -> String {
        self.storage.borrow().to_string()
    }

    pub fn insert_str(&mut self, pos: usize, text: &str) {
        self.storage.borrow_mut().insert_str(pos, text);
    }

    pub fn delete_range(&mut self, start: usize, end: usize) {
        self.storage.borrow_mut().delete_range(start, end);
    }

    pub fn byte_to_char(&self, byte_pos: usize) -> usize {
        self.storage.borrow().byte_to_char(byte_pos)
    }

    pub fn char_to_byte(&self, char_pos: usize) -> usize {
        self.storage.borrow().char_to_byte(char_pos)
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
        self.storage.borrow().dump_text()
    }

    pub(crate) fn from_dump(text: Vec<u8>) -> Self {
        Self {
            storage: Rc::new(RefCell::new(GapBuffer::from_dump(text))),
        }
    }
}

impl fmt::Display for BufferText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.storage.borrow().fmt(f)
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
