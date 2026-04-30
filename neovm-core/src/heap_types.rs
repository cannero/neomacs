//! Shared heap payload types used by both the tagged runtime and pdump code.
//!
//! Keeping them behind a neutral module boundary lets the tagged runtime and
//! dump/load code share the same payload structs without reviving old heap
//! module boundaries.

use crate::buffer::{BufferId, TextPropertyTable};
use crate::emacs_core::emacs_char;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// A Lisp string.
///
/// Backing bytes use Emacs internal encoding (a UTF-8 superset). For standard
/// Unicode text the bytes are valid UTF-8; raw bytes 0x80-0xFF are encoded as
/// overlong two-byte sequences (C0/C1 lead byte) which makes `as_str()` return
/// `None` for those strings. Like GNU, `SDATA` has a trailing NUL byte after
/// `SBYTES`; that terminator is not part of the Lisp string contents.
///
/// - **Multibyte:** `size_byte >= 0`.  `size` = char count, `size_byte` = byte count.
/// - **Unibyte:**   `size_byte < 0`. `size` = byte count (each byte is one char).
///   GNU distinguishes `-1` normally allocated, `-2` rodata, and `-3`
///   immovable bytecode storage.
#[repr(C)]
pub struct LispString {
    /// Character count (cached).
    size: usize,
    /// Byte count for multibyte strings, or GNU's negative unibyte marker.
    size_byte: i64,
    /// GNU Lisp_String-compatible interval ownership: string text properties
    /// belong to the string object, not to a side table.
    intervals: TextPropertyTable,
    /// Direct string byte pointer, like GNU's `Lisp_String.u.s.data`.
    ///
    data: *const u8,
    /// Sidecar ownership metadata. GNU's logical Lisp_String fields above stay
    /// first in the layout; this pointer is Neomacs runtime bookkeeping for
    /// owned/mapped/static byte storage.
    storage: Box<LispStringStorage>,
}

const SIZE_BYTE_UNIBYTE_NORMAL: i64 = -1;
const SIZE_BYTE_UNIBYTE_RODATA: i64 = -2;
const SIZE_BYTE_UNIBYTE_IMMOVABLE: i64 = -3;

enum LispStringStorage {
    /// Ordinary mutable string data. The vector stores `SBYTES + 1` bytes:
    /// logical payload followed by GNU's trailing NUL.
    Owned(Vec<u8>),
    /// Bytes in Rust/Emacs read-only storage.  This is the only storage class
    /// that may carry GNU's `size_byte == -2` rodata marker.
    Static {
        key: u64,
        ptr: *const u8,
        len: usize,
    },
    /// Bytes owned by a mapped pdump image.  Mutation first copies these bytes
    /// into ordinary Rust storage, matching GNU's writable object header plus
    /// cold string-data split in pdumper.c.
    Mapped { ptr: *const u8, len: usize },
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

#[derive(Clone, Copy)]
struct StaticRoDataEntry {
    ptr: usize,
    len: usize,
}

fn static_rodata_registry() -> &'static Mutex<HashMap<u64, StaticRoDataEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<u64, StaticRoDataEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn static_rodata_key(bytes_with_nul: &[u8]) -> u64 {
    // Stable FNV-1a over the exact executable rodata bytes, including GNU's
    // trailing NUL.
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes_with_nul {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn register_static_rodata(data_with_nul: &'static [u8]) -> u64 {
    let key = static_rodata_key(data_with_nul);
    let len = data_with_nul.len() - 1;
    let mut registry = static_rodata_registry()
        .lock()
        .expect("static rodata registry poisoned");
    if let Some(existing) = registry.get(&key) {
        let existing_bytes =
            unsafe { std::slice::from_raw_parts(existing.ptr as *const u8, existing.len + 1) };
        assert_eq!(
            existing_bytes, data_with_nul,
            "static rodata string key collision"
        );
    } else {
        registry.insert(
            key,
            StaticRoDataEntry {
                ptr: data_with_nul.as_ptr() as usize,
                len,
            },
        );
    }
    key
}

fn lookup_static_rodata(key: u64, len: usize) -> Option<*const u8> {
    let registry = static_rodata_registry()
        .lock()
        .expect("static rodata registry poisoned");
    let entry = registry.get(&key)?;
    if entry.len == len {
        Some(entry.ptr as *const u8)
    } else {
        None
    }
}

impl LispStringStorage {
    fn owned_from_payload(mut data: Vec<u8>) -> Self {
        data.push(0);
        Self::Owned(data)
    }

    fn payload_len(&self) -> usize {
        match self {
            Self::Owned(data) => data
                .len()
                .checked_sub(1)
                .expect("owned LispString storage must include trailing NUL"),
            Self::Static { len, .. } | Self::Mapped { len, .. } => *len,
        }
    }

    fn ptr(&self) -> *const u8 {
        match self {
            Self::Owned(data) => data.as_ptr(),
            Self::Static { ptr, .. } => *ptr,
            Self::Mapped { ptr, .. } => *ptr,
        }
    }

    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Owned(data) => {
                let len = data
                    .len()
                    .checked_sub(1)
                    .expect("owned LispString storage must include trailing NUL");
                &data[..len]
            }
            Self::Static { ptr, len, .. } => {
                if *len == 0 {
                    &[]
                } else {
                    unsafe { std::slice::from_raw_parts(*ptr, *len) }
                }
            }
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
        if !matches!(self, Self::Owned(_)) {
            let data = self.as_slice().to_vec();
            *self = Self::owned_from_payload(data);
        }
        match self {
            Self::Owned(data) => data,
            Self::Static { .. } | Self::Mapped { .. } => {
                unreachable!("non-owned string storage was copied to owned bytes")
            }
        }
    }

    fn is_static_rodata(&self) -> bool {
        matches!(self, Self::Static { .. })
    }

    fn static_rodata_key(&self) -> Option<u64> {
        match self {
            Self::Static { key, .. } => Some(*key),
            _ => None,
        }
    }

    fn has_trailing_nul(&self) -> bool {
        let ptr = self.ptr();
        if ptr.is_null() {
            return false;
        }
        unsafe { *ptr.add(self.payload_len()) == 0 }
    }
}

impl LispString {
    // -- Constructors --------------------------------------------------------

    fn from_storage(storage: LispStringStorage, size: usize, size_byte: i64) -> Self {
        let size_byte = if size_byte == SIZE_BYTE_UNIBYTE_RODATA && !storage.is_static_rodata() {
            SIZE_BYTE_UNIBYTE_NORMAL
        } else {
            size_byte
        };
        debug_assert!(
            size_byte >= 0
                || matches!(
                    size_byte,
                    SIZE_BYTE_UNIBYTE_NORMAL
                        | SIZE_BYTE_UNIBYTE_RODATA
                        | SIZE_BYTE_UNIBYTE_IMMOVABLE
                ),
            "invalid GNU Lisp_String size_byte {size_byte}"
        );
        debug_assert!(
            storage.has_trailing_nul(),
            "GNU Lisp_String data must be NUL-terminated after SBYTES"
        );
        debug_assert_eq!(
            storage.payload_len(),
            if size_byte < 0 {
                size
            } else {
                size_byte as usize
            },
            "LispString storage length must match GNU size/size_byte fields"
        );
        let data = storage.ptr();
        Self {
            size,
            size_byte,
            intervals: TextPropertyTable::new(),
            data,
            storage: Box::new(storage),
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
        Self::from_storage(LispStringStorage::owned_from_payload(data), size, size_byte)
    }

    /// Reconstruct a `LispString` from pdump data with pre-computed fields.
    /// The caller is responsible for passing consistent `data`, `size`, and
    /// `size_byte` values (as stored in the dump file).
    pub fn from_dump(data: Vec<u8>, size: usize, size_byte: i64) -> Self {
        Self::from_storage(LispStringStorage::owned_from_payload(data), size, size_byte)
    }

    /// Build a Lisp string whose bytes live in a mapped pdump image.
    ///
    /// # Safety
    /// `ptr..ptr+len+1` must remain mapped and immutable for the lifetime of
    /// the returned `LispString`, with `ptr[len] == 0`. Mutation first copies
    /// these bytes into owned storage.
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
        Self::from_storage(
            LispStringStorage::owned_from_payload(data),
            size,
            SIZE_BYTE_UNIBYTE_NORMAL,
        )
    }

    /// Create a unibyte string whose bytes live in static read-only storage.
    ///
    /// This mirrors GNU's `size_byte == -2` state for C string constants.  If
    /// later mutated, Neomacs copies the data and demotes it to ordinary
    /// unibyte storage because it no longer points at rodata.
    pub fn from_rodata_unibyte(data_with_nul: &'static [u8]) -> Self {
        assert!(
            data_with_nul.last().is_some_and(|byte| *byte == 0),
            "GNU rodata strings must include the trailing NUL"
        );
        let size = data_with_nul.len() - 1;
        let key = register_static_rodata(data_with_nul);
        Self::from_storage(
            LispStringStorage::Static {
                key,
                ptr: data_with_nul.as_ptr(),
                len: size,
            },
            size,
            SIZE_BYTE_UNIBYTE_RODATA,
        )
    }

    pub(crate) fn from_registered_rodata_unibyte(
        key: u64,
        len: usize,
        size: usize,
    ) -> Option<Self> {
        if size != len {
            return None;
        }
        let ptr = lookup_static_rodata(key, len)?;
        Some(Self::from_storage(
            LispStringStorage::Static { key, ptr, len },
            size,
            SIZE_BYTE_UNIBYTE_RODATA,
        ))
    }

    /// Create a multibyte string from valid UTF-8.
    /// Standard Unicode == Emacs encoding, so just copy the bytes.
    pub fn from_utf8(s: &str) -> Self {
        let data = s.as_bytes().to_vec();
        let size = s.chars().count();
        let size_byte = data.len() as i64;
        Self::from_storage(LispStringStorage::owned_from_payload(data), size, size_byte)
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

    /// Raw GNU `Lisp_String.u.s.size_byte` value.
    pub fn size_byte(&self) -> i64 {
        self.size_byte
    }

    pub(crate) fn rodata_key(&self) -> Option<u64> {
        self.storage.static_rodata_key()
    }

    /// True for GNU's `size_byte == -2`: unibyte bytes in read-only storage.
    pub fn is_rodata(&self) -> bool {
        self.size_byte == SIZE_BYTE_UNIBYTE_RODATA
    }

    /// True for GNU's `size_byte == -3`: unibyte bytes that must not move.
    pub fn is_immovable(&self) -> bool {
        self.size_byte == SIZE_BYTE_UNIBYTE_IMMOVABLE
    }

    /// Mirror GNU `pin_string`: mark a unibyte string as immovable bytecode
    /// storage.  Multibyte strings cannot be pinned this way.
    pub fn pin_immovable(&mut self) {
        debug_assert!(
            !self.is_multibyte(),
            "GNU pin_string only accepts unibyte strings"
        );
        self.storage.ensure_owned();
        self.size = self.storage.as_slice().len();
        self.size_byte = SIZE_BYTE_UNIBYTE_IMMOVABLE;
        self.refresh_data_ptr();
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

    /// Mutate the logical string bytes and restore GNU string invariants before
    /// returning: trailing NUL after `SBYTES`, direct data pointer, and cached
    /// character/byte sizes.
    pub fn mutate_bytes<R>(&mut self, f: impl FnOnce(&mut Vec<u8>) -> R) -> R {
        if self.is_rodata() {
            self.size_byte = SIZE_BYTE_UNIBYTE_NORMAL;
        }
        let data = self.storage.ensure_owned();
        debug_assert_eq!(
            data.last().copied(),
            Some(0),
            "owned LispString storage must include trailing NUL"
        );
        data.pop();
        let result = f(data);
        data.push(0);
        self.recompute_size();
        result
    }

    /// Recompute cached `size` (and `size_byte`) from the current data.
    fn recompute_size(&mut self) {
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
        *self.storage = LispStringStorage::owned_from_payload(s.as_bytes().to_vec());
        if self.is_rodata() {
            self.size_byte = SIZE_BYTE_UNIBYTE_NORMAL;
        }
        self.recompute_size();
    }

    pub(crate) fn has_trailing_nul(&self) -> bool {
        self.storage.has_trailing_nul()
    }
}

impl Clone for LispString {
    fn clone(&self) -> Self {
        let mut cloned = Self {
            size: self.size,
            size_byte: self.size_byte,
            intervals: self.intervals.clone(),
            data: std::ptr::null(),
            storage: Box::new(LispStringStorage::owned_from_payload(
                self.as_bytes().to_vec(),
            )),
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
        self.is_multibyte().hash(state);
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
    fn lisp_string_layout_keeps_gnu_fields_before_storage_sidecar() {
        assert_eq!(std::mem::offset_of!(LispString, size), 0);
        assert!(
            std::mem::offset_of!(LispString, size_byte) > std::mem::offset_of!(LispString, size)
        );
        assert!(
            std::mem::offset_of!(LispString, intervals)
                > std::mem::offset_of!(LispString, size_byte)
        );
        assert!(
            std::mem::offset_of!(LispString, data) > std::mem::offset_of!(LispString, intervals)
        );
        assert!(std::mem::offset_of!(LispString, storage) > std::mem::offset_of!(LispString, data));
        assert_eq!(
            std::mem::size_of::<Box<super::LispStringStorage>>(),
            std::mem::size_of::<usize>()
        );
    }

    #[test]
    fn mapped_lisp_string_borrows_until_mutation() {
        let bytes = b"abc\0".to_vec();
        let mut string = unsafe { LispString::from_mapped_bytes(bytes.as_ptr(), 3, 3, 3) };

        assert_eq!(string.as_bytes(), b"abc");
        assert!(string.has_trailing_nul());
        string.mutate_bytes(|bytes| bytes.push(b'd'));

        drop(bytes);
        assert_eq!(string.as_bytes(), b"abcd");
        assert_eq!(string.schars(), 4);
        assert_eq!(string.sbytes(), 4);
        assert!(string.has_trailing_nul());
    }

    #[test]
    fn mapped_lisp_string_clone_is_owned() {
        let bytes = b"abc\0".to_vec();
        let string = unsafe { LispString::from_mapped_bytes(bytes.as_ptr(), 3, 3, 3) };
        let cloned = string.clone();

        drop(bytes);
        assert_eq!(cloned.as_bytes(), b"abc");
        assert!(cloned.has_trailing_nul());
    }

    #[test]
    fn gnu_unibyte_size_byte_states_are_distinct() {
        let normal = LispString::from_unibyte(b"abc".to_vec());
        assert_eq!(normal.size_byte(), -1);
        assert!(!normal.is_multibyte());
        assert!(!normal.is_rodata());
        assert!(!normal.is_immovable());

        let rodata = LispString::from_rodata_unibyte(b"abc\0");
        assert_eq!(rodata.size_byte(), -2);
        assert!(!rodata.is_multibyte());
        assert!(rodata.is_rodata());
        assert!(!rodata.is_immovable());
        assert_eq!(rodata.as_bytes(), b"abc");
        assert!(rodata.has_trailing_nul());

        let mut immovable = LispString::from_unibyte(b"abc".to_vec());
        immovable.pin_immovable();
        assert_eq!(immovable.size_byte(), -3);
        assert!(!immovable.is_multibyte());
        assert!(!immovable.is_rodata());
        assert!(immovable.is_immovable());
    }

    #[test]
    fn rodata_unibyte_demotes_to_normal_on_mutation() {
        let mut string = LispString::from_rodata_unibyte(b"abc\0");
        string.mutate_bytes(|bytes| bytes[0] = b'X');

        assert_eq!(string.as_bytes(), b"Xbc");
        assert_eq!(string.size_byte(), -1);
        assert!(!string.is_rodata());
        assert!(string.has_trailing_nul());
    }

    #[test]
    fn mutate_bytes_recomputes_multibyte_size_and_preserves_nul() {
        let mut string = LispString::from_utf8("é");
        assert_eq!(string.schars(), 1);
        assert_eq!(string.sbytes(), 2);

        string.mutate_bytes(|bytes| bytes.extend_from_slice("x".as_bytes()));

        assert_eq!(string.as_bytes(), "éx".as_bytes());
        assert_eq!(string.schars(), 2);
        assert_eq!(string.sbytes(), 3);
        assert_eq!(string.size_byte(), 3);
        assert!(string.has_trailing_nul());
    }

    #[test]
    fn owned_and_dump_strings_have_gnu_trailing_nul_after_sbytes() {
        let strings = [
            LispString::from_utf8("abc"),
            LispString::from_unibyte(b"abc".to_vec()),
            LispString::from_dump(b"abc".to_vec(), 3, 3),
        ];

        for string in strings {
            assert_eq!(string.as_bytes(), b"abc");
            assert!(string.has_trailing_nul());
        }
    }

    #[test]
    fn owned_dump_data_cannot_claim_rodata_size_byte() {
        let string = LispString::from_dump(b"abc".to_vec(), 3, -2);

        assert_eq!(string.as_bytes(), b"abc");
        assert_eq!(string.size_byte(), -1);
        assert!(!string.is_rodata());
        assert!(string.has_trailing_nul());
    }

    #[test]
    fn equal_unibyte_storage_classes_hash_identically() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let normal = LispString::from_unibyte(b"abc".to_vec());
        let mut immovable = LispString::from_unibyte(b"abc".to_vec());
        immovable.pin_immovable();

        assert_eq!(normal, immovable);

        let mut normal_hash = DefaultHasher::new();
        normal.hash(&mut normal_hash);
        let mut immovable_hash = DefaultHasher::new();
        immovable.hash(&mut immovable_hash);
        assert_eq!(normal_hash.finish(), immovable_hash.finish());
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
