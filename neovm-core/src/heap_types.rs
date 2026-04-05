//! Shared heap payload types used by both the tagged runtime and pdump code.
//!
//! Keeping them behind a neutral module boundary lets the tagged runtime and
//! dump/load code share the same payload structs without reviving old heap
//! module boundaries.

use crate::buffer::BufferId;

pub struct LispString {
    data: String,
    pub multibyte: bool,
}

impl LispString {
    pub fn new(text: String, multibyte: bool) -> Self {
        Self {
            data: text,
            multibyte,
        }
    }

    pub fn as_str(&self) -> &str {
        &self.data
    }

    pub(crate) fn byte_len(&self) -> usize {
        self.data.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub(crate) fn is_ascii(&self) -> bool {
        self.data.is_ascii()
    }

    pub fn make_mut(&mut self) -> &mut String {
        &mut self.data
    }

    pub fn slice(&self, start: usize, end: usize) -> Option<Self> {
        self.data.get(start..end).map(|s| Self {
            data: s.to_owned(),
            multibyte: self.multibyte,
        })
    }

    pub fn concat(&self, other: &Self) -> Self {
        let mut data = self.data.clone();
        data.push_str(&other.data);
        Self {
            data,
            multibyte: self.multibyte || other.multibyte,
        }
    }
}

impl Clone for LispString {
    fn clone(&self) -> Self {
        Self {
            data: self.data.clone(),
            multibyte: self.multibyte,
        }
    }
}

impl std::fmt::Debug for LispString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LispString")
            .field("text", &self.as_str())
            .field("multibyte", &self.multibyte)
            .finish()
    }
}

impl PartialEq for LispString {
    fn eq(&self, other: &Self) -> bool {
        self.multibyte == other.multibyte && self.as_str() == other.as_str()
    }
}

impl Eq for LispString {}

#[derive(Clone, Debug)]
pub struct OverlayData {
    pub plist: crate::emacs_core::value::Value,
    pub buffer: Option<BufferId>,
    pub start: usize,
    pub end: usize,
    pub front_advance: bool,
    pub rear_advance: bool,
}

#[derive(Clone, Debug)]
pub struct MarkerData {
    pub buffer: Option<BufferId>,
    pub position: Option<i64>,
    pub insertion_type: bool,
    pub marker_id: Option<u64>,
}
