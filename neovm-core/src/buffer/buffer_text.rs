//! Buffer text storage.
//!
//! GNU Emacs separates per-buffer metadata from the underlying text object.
//! `BufferText` is the first local seam toward that design. Today it is a thin
//! wrapper around `GapBuffer`; later it can absorb shared text state, char/byte
//! caches, and interval ownership without forcing another tree-wide field-type
//! rewrite.

use std::fmt;

use super::gap_buffer::GapBuffer;

#[derive(Clone, Default)]
pub struct BufferText {
    storage: GapBuffer,
}

impl BufferText {
    pub fn new() -> Self {
        Self {
            storage: GapBuffer::new(),
        }
    }

    pub fn from_str(text: &str) -> Self {
        Self {
            storage: GapBuffer::from_str(text),
        }
    }

    pub fn len(&self) -> usize {
        self.storage.len()
    }

    pub fn is_empty(&self) -> bool {
        self.storage.is_empty()
    }

    pub fn char_count(&self) -> usize {
        self.storage.char_count()
    }

    pub fn byte_at(&self, pos: usize) -> u8 {
        self.storage.byte_at(pos)
    }

    pub fn char_at(&self, pos: usize) -> Option<char> {
        self.storage.char_at(pos)
    }

    pub fn text_range(&self, start: usize, end: usize) -> String {
        self.storage.text_range(start, end)
    }

    pub fn copy_bytes_to(&self, start: usize, end: usize, out: &mut Vec<u8>) {
        self.storage.copy_bytes_to(start, end, out);
    }

    pub fn to_string(&self) -> String {
        self.storage.to_string()
    }

    pub fn insert_str(&mut self, pos: usize, text: &str) {
        self.storage.insert_str(pos, text);
    }

    pub fn delete_range(&mut self, start: usize, end: usize) {
        self.storage.delete_range(start, end);
    }

    pub fn byte_to_char(&self, byte_pos: usize) -> usize {
        self.storage.byte_to_char(byte_pos)
    }

    pub fn char_to_byte(&self, char_pos: usize) -> usize {
        self.storage.char_to_byte(char_pos)
    }

    pub(crate) fn dump_text(&self) -> Vec<u8> {
        self.storage.dump_text()
    }

    pub(crate) fn from_dump(text: Vec<u8>) -> Self {
        Self {
            storage: GapBuffer::from_dump(text),
        }
    }
}

impl fmt::Display for BufferText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.storage.fmt(f)
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
