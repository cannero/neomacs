//! Shared heap payload types used by both the tagged runtime and pdump code.
//!
//! Keeping them behind a neutral module boundary lets the tagged runtime and
//! dump/load code share the same payload structs without reviving old heap
//! module boundaries.

use crate::buffer::BufferId;
use crate::emacs_core::emacs_char;
use serde::{Deserialize, Serialize};

/// A Lisp string.
///
/// Backing store is `Vec<u8>` using Emacs internal encoding (a UTF-8 superset).
/// For standard Unicode text the bytes are valid UTF-8; raw bytes 0x80-0xFF
/// are encoded as overlong two-byte sequences (C0/C1 lead byte) which makes
/// `as_str()` return `None` for those strings.
///
/// - **Multibyte:** `size_byte >= 0`.  `size` = char count, `size_byte` = byte count.
/// - **Unibyte:**   `size_byte == -1`. `size` = byte count (each byte is one char).
#[derive(Serialize, Deserialize)]
pub struct LispString {
    data: Vec<u8>,
    /// Character count (cached).
    size: usize,
    /// Byte count for multibyte strings, or -1 for unibyte.
    size_byte: i64,
}

impl LispString {
    // -- Constructors --------------------------------------------------------

    /// Backward-compat shim: create from a Rust `String` + multibyte flag.
    /// For multibyte, the bytes are already valid UTF-8 (standard Unicode ==
    /// Emacs encoding for Unicode codepoints).  For unibyte, each byte is one
    /// character.
    pub fn new(text: String, multibyte: bool) -> Self {
        if multibyte {
            Self::from_utf8(&text)
        } else {
            Self::from_unibyte(text.into_bytes())
        }
    }

    /// Create a multibyte string from raw Emacs-internal-encoding bytes.
    /// The caller must ensure the bytes are valid Emacs encoding.
    pub fn from_emacs_bytes(data: Vec<u8>) -> Self {
        let size = emacs_char::chars_in_multibyte(&data);
        let size_byte = data.len() as i64;
        Self {
            data,
            size,
            size_byte,
        }
    }

    /// Reconstruct a `LispString` from pdump data with pre-computed fields.
    /// The caller is responsible for passing consistent `data`, `size`, and
    /// `size_byte` values (as stored in the dump file).
    pub fn from_dump(data: Vec<u8>, size: usize, size_byte: i64) -> Self {
        Self {
            data,
            size,
            size_byte,
        }
    }

    /// Create a unibyte string.  Each byte is one character; `size_byte` = -1.
    pub fn from_unibyte(data: Vec<u8>) -> Self {
        let size = data.len();
        Self {
            data,
            size,
            size_byte: -1,
        }
    }

    /// Create a multibyte string from valid UTF-8.
    /// Standard Unicode == Emacs encoding, so just copy the bytes.
    pub fn from_utf8(s: &str) -> Self {
        let data = s.as_bytes().to_vec();
        let size = s.chars().count();
        let size_byte = data.len() as i64;
        Self {
            data,
            size,
            size_byte,
        }
    }

    // -- Accessors -----------------------------------------------------------

    /// Raw byte access.
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Try to view the data as a UTF-8 `&str`.
    /// Returns `None` if the bytes contain non-UTF-8 sequences (e.g. overlong
    /// C0/C1 raw-byte encodings from `.elc` files).
    ///
    /// Prefer `as_bytes()` for byte-level equality: two different non-UTF-8
    /// strings both return `None`, so `as_utf8_str() == as_utf8_str()` would
    /// silently treat them as equal.
    pub fn as_utf8_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.data).ok()
    }

    /// Character count.
    pub fn schars(&self) -> usize {
        self.size
    }

    /// Byte count.  For multibyte strings this is `data.len()`; for unibyte
    /// strings this is also `data.len()` (= size, since each byte is one char).
    pub fn sbytes(&self) -> usize {
        self.data.len()
    }

    /// Whether this is a multibyte string (`size_byte >= 0`).
    pub fn is_multibyte(&self) -> bool {
        self.size_byte >= 0
    }

    /// Backward-compat accessor matching the old `pub multibyte` field.
    pub fn multibyte(&self) -> bool {
        self.is_multibyte()
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

    /// Get a mutable reference to the underlying bytes.
    /// After mutation the caller MUST call `recompute_size()` to keep the
    /// cached `size` / `size_byte` consistent.
    pub fn data_mut(&mut self) -> &mut Vec<u8> {
        &mut self.data
    }

    /// Recompute cached `size` (and `size_byte`) from the current data.
    pub fn recompute_size(&mut self) {
        if self.size_byte >= 0 {
            // multibyte
            self.size = emacs_char::chars_in_multibyte(&self.data);
            self.size_byte = self.data.len() as i64;
        } else {
            // unibyte
            self.size = self.data.len();
        }
    }

    /// Backward-compat shim that returns a `&mut String`-like interface.
    /// This wraps the bytes in a temporary `String` and writes it back.
    /// IMPORTANT: This only works when the data is valid UTF-8.
    pub fn make_mut(&mut self) -> StringMutGuard<'_> {
        // Build a String from the current bytes (must be valid UTF-8 for this
        // compat path).
        let s = String::from_utf8(std::mem::take(&mut self.data)).unwrap_or_else(|e| {
            // Fallback: lossy conversion for non-UTF-8 data
            let bytes = e.into_bytes();
            String::from_utf8_lossy(&bytes).into_owned()
        });
        StringMutGuard {
            string: s,
            owner: self,
        }
    }

    /// Byte-index slice (returns None if out of bounds).
    pub fn slice(&self, start: usize, end: usize) -> Option<Self> {
        if end > self.data.len() || start > end {
            return None;
        }
        let slice = &self.data[start..end];
        if self.size_byte >= 0 {
            // multibyte
            Some(Self::from_emacs_bytes(slice.to_vec()))
        } else {
            Some(Self::from_unibyte(slice.to_vec()))
        }
    }

    pub fn concat(&self, other: &Self) -> Self {
        let mut data = self.data.clone();
        data.extend_from_slice(&other.data);
        let multibyte = self.is_multibyte() || other.is_multibyte();
        if multibyte {
            Self::from_emacs_bytes(data)
        } else {
            Self::from_unibyte(data)
        }
    }

    /// Replace the entire contents with a UTF-8 string, preserving the
    /// multibyte/unibyte flag.
    pub fn set_from_str(&mut self, s: &str) {
        self.data = s.as_bytes().to_vec();
        self.recompute_size();
    }
}

/// Guard that allows `&mut String` operations and writes back to `LispString`
/// on drop.  This is the backward-compat shim for `make_mut()`.
pub struct StringMutGuard<'a> {
    string: String,
    owner: &'a mut LispString,
}

impl std::ops::Deref for StringMutGuard<'_> {
    type Target = String;
    fn deref(&self) -> &String {
        &self.string
    }
}

impl std::ops::DerefMut for StringMutGuard<'_> {
    fn deref_mut(&mut self) -> &mut String {
        &mut self.string
    }
}

impl Drop for StringMutGuard<'_> {
    fn drop(&mut self) {
        self.owner.data = std::mem::take(&mut self.string).into_bytes();
        self.owner.recompute_size();
    }
}

impl Clone for LispString {
    fn clone(&self) -> Self {
        Self {
            data: self.data.clone(),
            size: self.size,
            size_byte: self.size_byte,
        }
    }
}

impl std::fmt::Debug for LispString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text: String = self
            .as_utf8_str()
            .map(|s| s.to_owned())
            .unwrap_or_else(|| format!("<{} bytes>", self.data.len()));
        f.debug_struct("LispString")
            .field("text", &text)
            .field("multibyte", &self.is_multibyte())
            .finish()
    }
}

impl PartialEq for LispString {
    fn eq(&self, other: &Self) -> bool {
        self.is_multibyte() == other.is_multibyte() && self.data == other.data
    }
}

impl Eq for LispString {}

impl std::hash::Hash for LispString {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.data.hash(state);
        self.size_byte.hash(state);
    }
}

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
