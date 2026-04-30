//! Shared heap payload types used by both the tagged runtime and pdump code.
//!
//! Keeping them behind a neutral module boundary lets the tagged runtime and
//! dump/load code share the same payload structs without reviving old heap
//! module boundaries.

use crate::buffer::{BufferId, TextPropertyTable};
use crate::emacs_core::emacs_char;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A Lisp string.
///
/// Backing store is `Vec<u8>` using Emacs internal encoding (a UTF-8 superset).
/// For standard Unicode text the bytes are valid UTF-8; raw bytes 0x80-0xFF
/// are encoded as overlong two-byte sequences (C0/C1 lead byte) which makes
/// `as_str()` return `None` for those strings.
///
/// - **Multibyte:** `size_byte >= 0`.  `size` = char count, `size_byte` = byte count.
/// - **Unibyte:**   `size_byte == -1`. `size` = byte count (each byte is one char).
pub struct LispString {
    /// Character count (cached).
    size: usize,
    /// Byte count for multibyte strings, or -1 for unibyte.
    size_byte: i64,
    /// GNU Lisp_String-compatible interval ownership: string text properties
    /// belong to the string object, not to a side table.
    intervals: TextPropertyTable,
    /// Direct string byte pointer, like GNU's `Lisp_String.u.s.data`.
    ///
    /// Neomacs still keeps Rust ownership metadata in `storage`, but ordinary
    /// reads go through this pointer and `sbytes`, matching GNU's SDATA/SBYTES
    /// access model.
    data: *const u8,
    storage: LispStringStorage,
}

enum LispStringStorage {
    Owned(Vec<u8>),
    /// Bytes owned by a mapped pdump image.  Mutation first copies these bytes
    /// into ordinary Rust storage, matching GNU's writable object header plus
    /// cold string-data split in pdumper.c.
    Mapped {
        ptr: *const u8,
        len: usize,
    },
}

// Mapped string storage is immutable by shared reference, and all mutation
// paths copy into `Owned` storage before returning `&mut Vec<u8>`.
unsafe impl Send for LispStringStorage {}
unsafe impl Sync for LispStringStorage {}
// `data` always points into `storage`, or into an immutable mapped pdump
// region.  Moving the Rust owner does not move Vec allocations, and mutation
// requires `&mut self`.
unsafe impl Send for LispString {}
unsafe impl Sync for LispString {}

impl LispStringStorage {
    fn ptr(&self) -> *const u8 {
        match self {
            Self::Owned(data) => data.as_ptr(),
            Self::Mapped { ptr, .. } => *ptr,
        }
    }

    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Owned(data) => data,
            Self::Mapped { ptr, len } => {
                if *len == 0 {
                    &[]
                } else {
                    unsafe { std::slice::from_raw_parts(*ptr, *len) }
                }
            }
        }
    }

    fn ensure_owned(&mut self) -> &mut Vec<u8> {
        if let Self::Mapped { .. } = self {
            let data = self.as_slice().to_vec();
            *self = Self::Owned(data);
        }
        match self {
            Self::Owned(data) => data,
            Self::Mapped { .. } => unreachable!("mapped string storage was copied to owned bytes"),
        }
    }

    fn into_owned_bytes(self) -> Vec<u8> {
        match self {
            Self::Owned(data) => data,
            Self::Mapped { ptr, len } => {
                if len == 0 {
                    Vec::new()
                } else {
                    unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()
                }
            }
        }
    }
}

impl LispString {
    // -- Constructors --------------------------------------------------------

    fn from_storage(storage: LispStringStorage, size: usize, size_byte: i64) -> Self {
        let data = storage.ptr();
        Self {
            size,
            size_byte,
            intervals: TextPropertyTable::new(),
            data,
            storage,
        }
    }

    fn refresh_data_ptr(&mut self) {
        self.data = self.storage.ptr();
    }

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
        Self::from_storage(LispStringStorage::Owned(data), size, size_byte)
    }

    /// Reconstruct a `LispString` from pdump data with pre-computed fields.
    /// The caller is responsible for passing consistent `data`, `size`, and
    /// `size_byte` values (as stored in the dump file).
    pub fn from_dump(data: Vec<u8>, size: usize, size_byte: i64) -> Self {
        Self::from_storage(LispStringStorage::Owned(data), size, size_byte)
    }

    /// Build a Lisp string whose bytes live in a mapped pdump image.
    ///
    /// # Safety
    /// `ptr..ptr+len` must remain mapped and immutable for the lifetime of the
    /// returned `LispString`, unless mutation first calls `data_mut`/similar and
    /// copies the bytes into owned storage.
    pub(crate) unsafe fn from_mapped_bytes(
        ptr: *const u8,
        len: usize,
        size: usize,
        size_byte: i64,
    ) -> Self {
        Self::from_storage(LispStringStorage::Mapped { ptr, len }, size, size_byte)
    }

    /// Create a unibyte string.  Each byte is one character; `size_byte` = -1.
    pub fn from_unibyte(data: Vec<u8>) -> Self {
        let size = data.len();
        Self::from_storage(LispStringStorage::Owned(data), size, -1)
    }

    /// Create a multibyte string from valid UTF-8.
    /// Standard Unicode == Emacs encoding, so just copy the bytes.
    pub fn from_utf8(s: &str) -> Self {
        let data = s.as_bytes().to_vec();
        let size = s.chars().count();
        let size_byte = data.len() as i64;
        Self::from_storage(LispStringStorage::Owned(data), size, size_byte)
    }

    // -- Accessors -----------------------------------------------------------

    /// Raw byte access.
    pub fn as_bytes(&self) -> &[u8] {
        let len = self.sbytes();
        if len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.data, len) }
        }
    }

    /// Try to view the data as a UTF-8 `&str`.
    /// Returns `None` if the bytes contain non-UTF-8 sequences (e.g. overlong
    /// C0/C1 raw-byte encodings from `.elc` files).
    ///
    /// Prefer `as_bytes()` for byte-level equality: two different non-UTF-8
    /// strings both return `None`, so `as_utf8_str() == as_utf8_str()` would
    /// silently treat them as equal.
    pub fn as_utf8_str(&self) -> Option<&str> {
        std::str::from_utf8(self.as_bytes()).ok()
    }

    /// Character count.
    pub fn schars(&self) -> usize {
        self.size
    }

    /// Byte count.  For multibyte strings this is `data.len()`; for unibyte
    /// strings this is also `data.len()` (= size, since each byte is one char).
    pub fn sbytes(&self) -> usize {
        if self.size_byte < 0 {
            self.size
        } else {
            self.size_byte as usize
        }
    }

    /// Whether this is a multibyte string (`size_byte >= 0`).
    pub fn is_multibyte(&self) -> bool {
        self.size_byte >= 0
    }

    /// Text-property interval tree attached to this string, like GNU's
    /// `Lisp_String.u.s.intervals`.
    pub fn intervals(&self) -> &TextPropertyTable {
        &self.intervals
    }

    /// Mutable text-property interval tree attached to this string.
    pub fn intervals_mut(&mut self) -> &mut TextPropertyTable {
        &mut self.intervals
    }

    /// Backward-compat accessor matching the old `pub multibyte` field.
    pub fn multibyte(&self) -> bool {
        self.is_multibyte()
    }

    pub(crate) fn byte_len(&self) -> usize {
        self.sbytes()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.sbytes() == 0
    }

    pub(crate) fn is_ascii(&self) -> bool {
        self.as_bytes().is_ascii()
    }

    /// Get a mutable reference to the underlying bytes.
    /// After mutation the caller MUST call `recompute_size()` to keep the
    /// cached `size` / `size_byte` consistent.
    pub fn data_mut(&mut self) -> &mut Vec<u8> {
        let data = self.storage.ensure_owned();
        self.data = data.as_ptr();
        data
    }

    /// Recompute cached `size` (and `size_byte`) from the current data.
    pub fn recompute_size(&mut self) {
        if self.size_byte >= 0 {
            // multibyte
            let data = self.storage.as_slice();
            let size = emacs_char::chars_in_multibyte(data);
            let size_byte = data.len() as i64;
            self.size = size;
            self.size_byte = size_byte;
        } else {
            // unibyte
            self.size = self.storage.as_slice().len();
        }
        self.refresh_data_ptr();
    }

    /// Backward-compat shim that returns a `&mut String`-like interface.
    /// This wraps the bytes in a temporary `String` and writes it back.
    /// IMPORTANT: This only works when the data is valid UTF-8.
    pub fn make_mut(&mut self) -> StringMutGuard<'_> {
        // Build a String from the current bytes (must be valid UTF-8 for this
        // compat path).
        let old = std::mem::replace(&mut self.storage, LispStringStorage::Owned(Vec::new()))
            .into_owned_bytes();
        self.refresh_data_ptr();
        let s = String::from_utf8(old).unwrap_or_else(|e| {
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
        if end > self.as_bytes().len() || start > end {
            return None;
        }
        let slice = &self.as_bytes()[start..end];
        if self.size_byte >= 0 {
            // multibyte
            Some(Self::from_emacs_bytes(slice.to_vec()))
        } else {
            Some(Self::from_unibyte(slice.to_vec()))
        }
    }

    pub fn concat(&self, other: &Self) -> Self {
        let mut data = self.as_bytes().to_vec();
        data.extend_from_slice(other.as_bytes());
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
        self.storage = LispStringStorage::Owned(s.as_bytes().to_vec());
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
        self.owner.storage =
            LispStringStorage::Owned(std::mem::take(&mut self.string).into_bytes());
        self.owner.recompute_size();
    }
}

impl Clone for LispString {
    fn clone(&self) -> Self {
        let mut cloned = Self {
            size: self.size,
            size_byte: self.size_byte,
            intervals: self.intervals.clone(),
            data: std::ptr::null(),
            storage: LispStringStorage::Owned(self.as_bytes().to_vec()),
        };
        cloned.refresh_data_ptr();
        cloned
    }
}

impl std::fmt::Debug for LispString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text: String = self
            .as_utf8_str()
            .map(|s| s.to_owned())
            .unwrap_or_else(|| format!("<{} bytes>", self.as_bytes().len()));
        f.debug_struct("LispString")
            .field("text", &text)
            .field("multibyte", &self.is_multibyte())
            .finish()
    }
}

impl PartialEq for LispString {
    fn eq(&self, other: &Self) -> bool {
        self.is_multibyte() == other.is_multibyte() && self.as_bytes() == other.as_bytes()
    }
}

impl Eq for LispString {}

impl std::hash::Hash for LispString {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_bytes().hash(state);
        self.size_byte.hash(state);
    }
}

impl Serialize for LispString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("LispString", 3)?;
        state.serialize_field("data", self.as_bytes())?;
        state.serialize_field("size", &self.size)?;
        state.serialize_field("size_byte", &self.size_byte)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for LispString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct LispStringOwned {
            data: Vec<u8>,
            size: usize,
            size_byte: i64,
        }

        let owned = LispStringOwned::deserialize(deserializer)?;
        Ok(Self::from_dump(owned.data, owned.size, owned.size_byte))
    }
}

#[cfg(test)]
mod tests {
    use super::LispString;

    #[test]
    fn mapped_lisp_string_borrows_until_mutation() {
        let bytes = b"abc".to_vec();
        let mut string =
            unsafe { LispString::from_mapped_bytes(bytes.as_ptr(), bytes.len(), 3, 3) };

        assert_eq!(string.as_bytes(), b"abc");
        string.data_mut().push(b'd');
        string.recompute_size();

        drop(bytes);
        assert_eq!(string.as_bytes(), b"abcd");
        assert_eq!(string.schars(), 4);
    }

    #[test]
    fn mapped_lisp_string_clone_is_owned() {
        let bytes = b"abc".to_vec();
        let string = unsafe { LispString::from_mapped_bytes(bytes.as_ptr(), bytes.len(), 3, 3) };
        let cloned = string.clone();

        drop(bytes);
        assert_eq!(cloned.as_bytes(), b"abc");
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
    pub insertion_type: bool,
    pub marker_id: Option<u64>,
    /// Byte offset in buffer (authoritative after T6/T7).
    pub bytepos: usize,
    /// Char offset in buffer (authoritative after T6/T7).
    pub charpos: usize,
    /// Intrusive link to next marker in the owning buffer's chain.
    /// `null` if not on a chain. GC sweep order: `unchain_dead_markers`
    /// walks these BEFORE `sweep_objects` frees unmarked markers.
    pub next_marker: *mut crate::tagged::header::MarkerObj,
}

#[cfg(test)]
#[path = "heap_types_marker_test.rs"]
mod marker_test;
