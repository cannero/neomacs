//! Tagged pointer `Value` — a single `usize` encoding type + payload.
//!
//! # Tag layout (3 low bits, 8-byte aligned heap pointers)
//!
//! ```text
//! Tag   Type         Payload                         Fast check
//! 000   Symbol       sym_index << 3                  (v & 7) == 0
//! xx1   Fixnum       integer << 2   (tags 001,011,101,111 — see below)
//!                    Actually we use only 001/101:   (v & 3) == 1
//! 010   Cons         pointer | 2                     (v & 7) == 2
//! 011   Vectorlike   pointer | 3                     (v & 7) == 3
//! 100   String       pointer | 4                     (v & 7) == 4
//! 110   Float        pointer | 6                     (v & 7) == 6
//! 111   Immediate    sub-tag in bits 3-7             (v & 7) == 7
//! ```
//!
//! Fixnum uses tags 001 and 101 (both have `(v & 3) == 1`), giving
//! 62-bit signed integer range without heap allocation.
//!
//! Special values:
//! - `nil`  = Symbol(0) = `0x0` (intern "nil" as SymId(0))
//! - `t`    = Symbol(1) = `0x8` (intern "t" as SymId(1))
//!
//! Tag `111` is reserved. Characters are fixnums, keywords are ordinary
//! symbols, and subrs are `PVEC_SUBR`-like heap objects.

use std::cell::RefCell;
use std::fmt;

use crate::emacs_core::intern::{SymId, resolve_sym};

use super::header::{ConsCell, FloatObj, GcHeader, StringObj, VecLikeHeader, VecLikeType};

thread_local! {
    /// Current thread's canonical subr objects keyed by `SymId`.
    static CURRENT_SUBRS: RefCell<Vec<Option<TaggedValue>>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn reset_current_subrs() {
    CURRENT_SUBRS.with(|slot| slot.borrow_mut().clear());
}

pub(crate) fn snapshot_current_subrs() -> Vec<Option<TaggedValue>> {
    CURRENT_SUBRS.with(|slot| slot.borrow().clone())
}

pub(crate) fn current_subr_value(id: SymId) -> Option<TaggedValue> {
    CURRENT_SUBRS.with(|slot| slot.borrow().get(id.0 as usize).copied().flatten())
}

pub(crate) fn register_current_subr(id: SymId, value: TaggedValue) {
    CURRENT_SUBRS.with(|slot| {
        let mut registry = slot.borrow_mut();
        let index = id.0 as usize;
        if registry.len() <= index {
            registry.resize(index + 1, None);
        }
        registry[index] = Some(value);
    });
}

// ---------------------------------------------------------------------------
// Tag constants
// ---------------------------------------------------------------------------

const TAG_BITS: usize = 3;
const TAG_MASK: usize = 0b111;

const TAG_SYMBOL: usize = 0b000;
const TAG_CONS: usize = 0b010;
const TAG_VECLIKE: usize = 0b011;
const TAG_STRING: usize = 0b100;
const TAG_FLOAT: usize = 0b110;
// Tag 111 was previously used for immediates (char, keyword, subr).
// All three have been removed:
// - Characters are fixnums (GNU Emacs compat)
// - Keywords are symbols (GNU Emacs compat)
// - Subrs are PVEC_SUBR veclike objects (GNU Emacs compat)
// Tag 111 is now unused/reserved.
#[allow(dead_code)]
const TAG_IMMEDIATE: usize = 0b111;

// Fixnum uses two tags: 001 and 101. Both have (v & 3) == 1.
const FIXNUM_CHECK_MASK: usize = 0b11;
const FIXNUM_CHECK_VALUE: usize = 0b01;
const FIXNUM_SHIFT: u32 = 2; // integer stored in bits 2..63

// ---------------------------------------------------------------------------
// TaggedValue — the core type
// ---------------------------------------------------------------------------

/// A Lisp value encoded as a tagged pointer in a single machine word.
///
/// This is `Copy`, `Eq`, `Hash` — can be freely duplicated and compared.
/// Heap access is via direct pointer dereference (no ObjId indirection).
#[derive(Clone, Copy, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct TaggedValue(pub(crate) usize);

/// `PartialEq` uses structural comparison (`equal`), matching the behavior
/// of the old `Value` enum. This allows `assert_eq!` in tests to work
/// naturally.  For Emacs `eq` (pointer identity), use `eq_value()` or
/// `a.bits() == b.bits()`.
///
/// NOTE: This intentionally violates the `Hash`/`Eq` contract for heap types
/// (two structurally-equal objects may have different hashes). Do NOT use
/// `TaggedValue` as a `HashMap` key — use `HashKey` instead.
impl PartialEq for TaggedValue {
    fn eq(&self, other: &Self) -> bool {
        if self.0 == other.0 {
            return true;
        }
        crate::emacs_core::value::equal_value(self, other, 0)
    }
}

impl Eq for TaggedValue {}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------

impl TaggedValue {
    // -- Special values --

    /// The nil value. `nil = Symbol(0) = 0`.
    pub const NIL: Self = Self(0);

    /// The t (true) value. `t = Symbol(1) = 0x8`.
    pub const T: Self = Self(1 << TAG_BITS);

    // -- Fixnum --

    /// Create a fixnum (62-bit signed integer, no heap allocation).
    #[inline]
    pub fn fixnum(n: i64) -> Self {
        // Encode: (n << 2) | 1. The low 2 bits are `01`, matching tags 001 or 101.
        Self(((n as usize) << FIXNUM_SHIFT) | FIXNUM_CHECK_VALUE)
    }

    /// Maximum fixnum value (62-bit signed).
    pub const MOST_POSITIVE_FIXNUM: i64 = (1_i64 << (64 - FIXNUM_SHIFT - 1)) - 1;
    /// Minimum fixnum value (62-bit signed).
    pub const MOST_NEGATIVE_FIXNUM: i64 = -(1_i64 << (64 - FIXNUM_SHIFT - 1));

    // -- Symbol --

    /// Create a symbol value from a SymId.
    #[inline]
    pub fn from_sym_id(id: SymId) -> Self {
        Self((id.0 as usize) << TAG_BITS | TAG_SYMBOL)
    }

    // -- Cons --

    /// Create a cons value from a pointer to a ConsCell.
    ///
    /// # Safety
    /// `cell` must be a valid, 8-byte-aligned pointer to a live `ConsCell`.
    #[inline]
    pub unsafe fn from_cons_ptr(cell: *const ConsCell) -> Self {
        debug_assert!(!cell.is_null());
        debug_assert!(cell as usize & TAG_MASK == 0, "ConsCell not aligned");
        Self(cell as usize | TAG_CONS)
    }

    // -- String --

    /// Create a string value from a pointer to a StringObj.
    ///
    /// # Safety
    /// `obj` must be a valid, 8-byte-aligned pointer to a live `StringObj`.
    #[inline]
    pub unsafe fn from_string_ptr(obj: *const StringObj) -> Self {
        debug_assert!(!obj.is_null());
        debug_assert!(obj as usize & TAG_MASK == 0, "StringObj not aligned");
        Self(obj as usize | TAG_STRING)
    }

    // -- Float --

    /// Create a float value from a pointer to a FloatObj.
    ///
    /// # Safety
    /// `obj` must be a valid, 8-byte-aligned pointer to a live `FloatObj`.
    #[inline]
    pub unsafe fn from_float_ptr(obj: *const FloatObj) -> Self {
        debug_assert!(!obj.is_null());
        debug_assert!(obj as usize & TAG_MASK == 0, "FloatObj not aligned");
        Self(obj as usize | TAG_FLOAT)
    }

    // -- Vectorlike --

    /// Create a vectorlike value from a pointer to a VecLikeHeader.
    ///
    /// # Safety
    /// `obj` must be a valid, 8-byte-aligned pointer to a live veclike object.
    #[inline]
    pub unsafe fn from_veclike_ptr(obj: *const VecLikeHeader) -> Self {
        debug_assert!(!obj.is_null());
        debug_assert!(obj as usize & TAG_MASK == 0, "VecLikeHeader not aligned");
        Self(obj as usize | TAG_VECLIKE)
    }

    // -- Immediates --

    /// Create a char value. In GNU Emacs, characters ARE integers (fixnums).
    /// `?A` is just the integer 65.
    #[inline]
    pub fn char(c: char) -> Self {
        Self::fixnum(c as i64)
    }

    /// Create a keyword value from a SymId.
    /// In GNU Emacs, keywords are ordinary symbols with `:` prefix names.
    #[inline]
    pub fn from_kw_id(id: SymId) -> Self {
        Self::from_sym_id(id)
    }

    /// Create a subr (builtin function) value.
    /// In GNU Emacs, subrs are PVEC_SUBR heap objects. We allocate a SubrObj
    /// on the tagged heap.
    pub fn subr(id: SymId) -> Self {
        if let Some(value) = current_subr_value(id) {
            return value;
        }
        let (min_args, max_args) =
            crate::emacs_core::subr_info::lookup_compat_subr_arity(resolve_sym(id))
                .unwrap_or((0, None));
        let value = crate::tagged::gc::with_tagged_heap(|h| h.alloc_subr(id, None, min_args, max_args));
        register_current_subr(id, value);
        value
    }

    // ---------------------------------------------------------------------------
    // Tag checks — all compile to a single AND + CMP
    // ---------------------------------------------------------------------------

    /// Raw tag (low 3 bits).
    #[inline]
    pub fn tag(self) -> usize {
        self.0 & TAG_MASK
    }

    /// Raw bits (for hashing, pointer identity, etc.).
    #[inline]
    pub fn bits(self) -> usize {
        self.0
    }

    #[inline]
    pub fn is_nil(self) -> bool {
        self.0 == 0
    }

    /// Check for `t` (the canonical true value).
    #[inline]
    pub fn is_t(self) -> bool {
        self.0 == Self::T.0
    }

    #[inline]
    pub fn is_fixnum(self) -> bool {
        self.0 & FIXNUM_CHECK_MASK == FIXNUM_CHECK_VALUE
    }

    /// Check if this value is a symbol.
    /// In GNU Emacs, keywords are symbols (interned with `:` prefix).
    /// Check if this value is a symbol. Keywords are symbols (name starts
    /// with `:`). nil and t are also symbols.
    #[inline]
    pub fn is_symbol(self) -> bool {
        self.0 & TAG_MASK == TAG_SYMBOL
    }

    #[inline]
    pub fn is_cons(self) -> bool {
        self.0 & TAG_MASK == TAG_CONS
    }

    #[inline]
    pub fn is_string(self) -> bool {
        self.0 & TAG_MASK == TAG_STRING
    }

    #[inline]
    pub fn is_float(self) -> bool {
        self.0 & TAG_MASK == TAG_FLOAT
    }

    #[inline]
    pub fn is_veclike(self) -> bool {
        self.0 & TAG_MASK == TAG_VECLIKE
    }

    /// TAG_IMMEDIATE is no longer used (chars=fixnum, keywords=symbol, subrs=veclike).
    #[inline]
    #[deprecated(note = "No types use TAG_IMMEDIATE anymore")]
    pub fn is_immediate(self) -> bool {
        self.0 & TAG_MASK == TAG_IMMEDIATE
    }

    /// In GNU Emacs, characters are integers. `characterp` checks if the
    /// integer is in the valid Unicode codepoint range (0..=0x3FFFFF in GNU,
    /// 0..=0x10FFFF for valid Unicode).
    #[inline]
    pub fn is_char(self) -> bool {
        if let Some(n) = self.as_fixnum() {
            n >= 0 && n <= 0x3F_FFFF // GNU MAX_CHAR
        } else {
            false
        }
    }

    /// In GNU Emacs, keywords are symbols whose name starts with `:`.
    #[inline]
    pub fn is_keyword(self) -> bool {
        // Keywords are symbols with : prefix name
        if self.0 & TAG_MASK == TAG_SYMBOL {
            if let Some(name) = self.as_symbol_name() {
                return name.starts_with(':');
            }
        }
        false
    }

    /// Subrs are PVEC_SUBR veclike heap objects.
    #[inline]
    pub fn is_subr(self) -> bool {
        self.veclike_type() == Some(super::header::VecLikeType::Subr)
    }

    /// True if this value holds a heap pointer (needs GC tracing).
    #[inline]
    pub fn is_heap_object(self) -> bool {
        matches!(self.tag(), TAG_CONS | TAG_STRING | TAG_FLOAT | TAG_VECLIKE)
    }

    /// Check if this value is a list (nil or cons).
    #[inline]
    pub fn is_list(self) -> bool {
        self.is_nil() || self.is_cons()
    }

    // ---------------------------------------------------------------------------
    // Extractors
    // ---------------------------------------------------------------------------

    /// Extract fixnum value. Returns None if not a fixnum.
    #[inline]
    pub fn as_fixnum(self) -> Option<i64> {
        if self.is_fixnum() {
            Some((self.0 as i64) >> FIXNUM_SHIFT)
        } else {
            None
        }
    }

    /// Extract fixnum value without tag check. Caller must ensure `is_fixnum()`.
    #[inline]
    pub fn xfixnum(self) -> i64 {
        debug_assert!(self.is_fixnum());
        (self.0 as i64) >> FIXNUM_SHIFT
    }

    /// Extract SymId for a symbol (including keywords, which are symbols
    /// Extract SymId for a symbol (including keywords). Returns None if not a symbol.
    #[inline]
    pub fn as_symbol_id(self) -> Option<SymId> {
        if self.0 & TAG_MASK == TAG_SYMBOL {
            Some(SymId((self.0 >> TAG_BITS) as u32))
        } else {
            None
        }
    }

    /// Extract SymId without tag check. Caller must ensure `is_symbol()`.
    #[inline]
    pub fn xsymbol_id(self) -> SymId {
        debug_assert!(self.is_symbol());
        SymId((self.0 >> TAG_BITS) as u32)
    }

    /// Extract char. Characters are fixnums in the valid codepoint range.
    /// Returns None if not a character (not fixnum or out of range).
    #[inline]
    pub fn as_char(self) -> Option<char> {
        if let Some(n) = self.as_fixnum() {
            if n >= 0 && n <= 0x3F_FFFF {
                // GNU Emacs allows codepoints up to MAX_CHAR (0x3FFFFF)
                // which includes non-Unicode internal chars. For Rust char,
                // we can only convert valid Unicode codepoints.
                char::from_u32(n as u32)
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Extract keyword SymId. Returns None if not a keyword.
    /// Keywords are symbols with `:` prefix, so this extracts the symbol id.
    #[inline]
    pub fn as_keyword_id(self) -> Option<SymId> {
        if self.is_keyword() {
            self.as_symbol_id()
        } else {
            None
        }
    }

    /// Extract subr SymId. Returns None if not a subr.
    #[inline]
    pub fn as_subr_id(self) -> Option<SymId> {
        if self.is_subr() {
            let ptr = self.as_veclike_ptr().unwrap() as *const super::header::SubrObj;
            Some(unsafe { (*ptr).name })
        } else {
            None
        }
    }

    // -- Heap pointer extractors --

    /// Extract raw cons cell pointer. Returns None if not a cons.
    #[inline]
    pub fn as_cons_ptr(self) -> Option<*const ConsCell> {
        if self.is_cons() {
            Some((self.0 & !TAG_MASK) as *const ConsCell)
        } else {
            None
        }
    }

    /// Extract raw cons cell pointer without tag check.
    #[inline]
    pub fn xcons_ptr(self) -> *const ConsCell {
        debug_assert!(self.is_cons());
        (self.0 & !TAG_MASK) as *const ConsCell
    }

    /// Extract raw string object pointer. Returns None if not a string.
    #[inline]
    pub fn as_string_ptr(self) -> Option<*const StringObj> {
        if self.is_string() {
            Some((self.0 & !TAG_MASK) as *const StringObj)
        } else {
            None
        }
    }

    /// Extract raw float object pointer. Returns None if not a float.
    #[inline]
    pub fn as_float_ptr(self) -> Option<*const FloatObj> {
        if self.is_float() {
            Some((self.0 & !TAG_MASK) as *const FloatObj)
        } else {
            None
        }
    }

    /// Extract raw veclike header pointer. Returns None if not veclike.
    #[inline]
    pub fn as_veclike_ptr(self) -> Option<*const VecLikeHeader> {
        if self.is_veclike() {
            Some((self.0 & !TAG_MASK) as *const VecLikeHeader)
        } else {
            None
        }
    }

    /// Extract raw heap pointer (any heap type). Returns None if immediate.
    #[inline]
    pub fn heap_ptr(self) -> Option<*const u8> {
        if self.is_heap_object() {
            Some((self.0 & !TAG_MASK) as *const u8)
        } else {
            None
        }
    }

    // ---------------------------------------------------------------------------
    // Cons accessors (direct pointer deref, no heap indirection)
    // ---------------------------------------------------------------------------

    /// Get the car of a cons cell. Panics if not a cons.
    #[inline]
    pub fn cons_car(self) -> Self {
        unsafe { (*self.xcons_ptr()).car }
    }

    /// Get the cdr of a cons cell. Panics if not a cons.
    #[inline]
    pub fn cons_cdr(self) -> Self {
        unsafe { (*self.xcons_ptr()).cdr }
    }

    /// Set the car of a cons cell. Panics if not a cons.
    #[inline]
    pub fn set_car(self, val: Self) {
        unsafe { (*(self.xcons_ptr() as *mut ConsCell)).car = val }
    }

    /// Set the cdr of a cons cell. Panics if not a cons.
    #[inline]
    pub fn set_cdr(self, val: Self) {
        unsafe { (*(self.xcons_ptr() as *mut ConsCell)).cdr = val }
    }

    // ---------------------------------------------------------------------------
    // Float accessor
    // ---------------------------------------------------------------------------

    /// Get the f64 value of a float. Panics if not a float.
    #[inline]
    pub fn xfloat(self) -> f64 {
        debug_assert!(self.is_float());
        unsafe { (*(self.as_float_ptr().unwrap())).value }
    }

    // ---------------------------------------------------------------------------
    // Veclike accessors
    // ---------------------------------------------------------------------------

    /// Get the veclike sub-type. Returns None if not veclike.
    #[inline]
    pub fn veclike_type(self) -> Option<VecLikeType> {
        if self.is_veclike() {
            Some(unsafe { (*self.as_veclike_ptr().unwrap()).type_tag })
        } else {
            None
        }
    }

    // ---------------------------------------------------------------------------
    // Type dispatch enum (for exhaustive matching)
    // ---------------------------------------------------------------------------

    /// Decode into a `ValueKind` enum for exhaustive pattern matching.
    /// This provides Rust `match` ergonomics without the old `Value` enum.
    pub fn kind(self) -> ValueKind {
        match self.tag() {
            TAG_SYMBOL => {
                if self.is_nil() {
                    ValueKind::Nil
                } else if self.is_t() {
                    ValueKind::T
                } else {
                    ValueKind::Symbol(self.xsymbol_id())
                }
            }
            _ if self.is_fixnum() => ValueKind::Fixnum(self.xfixnum()),
            TAG_CONS => ValueKind::Cons,
            TAG_VECLIKE => {
                ValueKind::Veclike(unsafe { (*self.as_veclike_ptr().unwrap()).type_tag })
            }
            TAG_STRING => ValueKind::String,
            TAG_FLOAT => ValueKind::Float,
            TAG_IMMEDIATE => ValueKind::Unknown,
            _ => ValueKind::Unknown,
        }
    }
}

// ---------------------------------------------------------------------------
// Backward-compatible API (matches old Value enum methods)
// ---------------------------------------------------------------------------

impl TaggedValue {
    // -- Compat constructors that allocate on the thread-local heap --

    /// Create a symbol by interning a name string.
    pub fn symbol_by_name(s: impl AsRef<str>) -> Self {
        Self::from_sym_id(crate::emacs_core::intern::intern(s.as_ref()))
    }

    /// Create a keyword by interning a name string.
    pub fn keyword_by_name(s: impl AsRef<str>) -> Self {
        Self::from_kw_id(crate::emacs_core::intern::intern(s.as_ref()))
    }

    /// `Value::t()` — compat alias for `Value::T`.
    pub fn t() -> Self {
        Self::T
    }

    /// `Value::bool(b)` — convert bool to nil/t.
    pub fn bool_val(b: bool) -> Self {
        if b { Self::T } else { Self::NIL }
    }

    // -- Compat predicates --

    /// True if this value is "truthy" (not nil).
    #[inline]
    pub fn is_truthy(self) -> bool {
        !self.is_nil()
    }

    /// True for integers.
    /// True for integers (fixnums). Characters are also integers in GNU Emacs,
    /// and since chars are now encoded as fixnums, this is just `is_fixnum()`.
    #[inline]
    pub fn is_integer(self) -> bool {
        self.is_fixnum()
    }

    /// True for any number (fixnum or float).
    #[inline]
    pub fn is_number(self) -> bool {
        self.is_fixnum() || self.is_float()
    }

    /// True if this value is a vector (veclike with Vector type tag).
    #[inline]
    pub fn is_vector(self) -> bool {
        self.veclike_type() == Some(VecLikeType::Vector)
    }

    /// True if this value is a record (veclike with Record type tag).
    #[inline]
    pub fn is_record(self) -> bool {
        self.veclike_type() == Some(VecLikeType::Record)
    }

    /// True if this value is a hash table.
    #[inline]
    pub fn is_hash_table(self) -> bool {
        self.veclike_type() == Some(VecLikeType::HashTable)
    }

    /// True if this value is callable (lambda, macro, bytecode, subr).
    #[inline]
    pub fn is_function(self) -> bool {
        self.is_subr()
            || matches!(
                self.veclike_type(),
                Some(VecLikeType::Lambda | VecLikeType::ByteCode)
            )
    }

    /// Human-readable type name.
    pub fn type_name(self) -> &'static str {
        match self.kind() {
            ValueKind::Nil => "nil",
            ValueKind::T => "symbol",
            ValueKind::Fixnum(_) => "integer",
            ValueKind::Symbol(_) => "symbol",
            ValueKind::Cons => "cons",
            ValueKind::String => "string",
            ValueKind::Float => "float",
            ValueKind::Veclike(ty) => match ty {
                VecLikeType::Subr => "subr",
                VecLikeType::Vector => "vector",
                VecLikeType::HashTable => "hash-table",
                VecLikeType::Lambda => "closure",
                VecLikeType::Macro => "macro",
                VecLikeType::ByteCode => "byte-code",
                VecLikeType::Record => "record",
                VecLikeType::Overlay => "overlay",
                VecLikeType::Marker => "marker",
                VecLikeType::Buffer => "buffer",
                VecLikeType::Window => "window",
                VecLikeType::Frame => "frame",
                VecLikeType::Timer => "timer",
            },
            ValueKind::Unknown => "unknown",
        }
    }

    // -- Numeric extraction --

    /// Extract integer value (alias for as_fixnum).
    #[inline]
    pub fn as_int(self) -> Option<i64> {
        self.as_fixnum()
    }

    /// Extract float value. Returns None if not a float.
    #[inline]
    pub fn as_float(self) -> Option<f64> {
        if self.is_float() {
            Some(self.xfloat())
        } else {
            None
        }
    }

    /// Extract numeric value as f64 (works for both fixnum and float).
    #[inline]
    pub fn as_number_f64(self) -> Option<f64> {
        if let Some(n) = self.as_fixnum() {
            Some(n as f64)
        } else {
            self.as_float()
        }
    }

    // -- String extraction --

    /// Get the string content. Returns None if not a string.
    pub fn as_str(self) -> Option<&'static str> {
        if self.is_string() {
            let ptr = self.as_string_ptr().unwrap();
            // Safety: the string object is alive (caller must ensure no GC).
            // Lifetime is extended to 'static — same pattern as old Value::as_str.
            unsafe { Some((*ptr).data.as_str()) }
        } else {
            None
        }
    }

    /// Get symbol name. Returns None if not a symbol.
    /// For keywords (which are symbols in GNU Emacs), returns the keyword name
    /// (e.g., ":foo").
    pub fn as_symbol_name(self) -> Option<&'static str> {
        self.as_symbol_id()
            .map(|id| crate::emacs_core::intern::resolve_sym(id))
    }

    /// Check if this symbol has the given name.
    pub fn is_symbol_named(self, name: &str) -> bool {
        self.as_symbol_name() == Some(name)
    }
}

// ---------------------------------------------------------------------------
// ValueKind — exhaustive dispatch enum
// ---------------------------------------------------------------------------

/// Decoded value kind for `match` ergonomics.
/// Use `value.kind()` to get this.
#[derive(Debug, Clone, Copy)]
pub enum ValueKind {
    Nil,
    T,
    /// Integer (fixnum). In GNU Emacs, characters are also integers.
    Fixnum(i64),
    /// Symbol (including keywords — they're symbols with `:` prefix names).
    Symbol(SymId),
    Cons,
    String,
    Float,
    // NOTE: No Char variant. Characters are Fixnum in GNU Emacs.
    // NOTE: No Keyword variant. Keywords are Symbol in GNU Emacs.
    // NOTE: No Subr variant. Subrs are Veclike(VecLikeType::Subr) in GNU Emacs.
    Veclike(VecLikeType),
    Unknown,
}

// ---------------------------------------------------------------------------
// Debug / Display
// ---------------------------------------------------------------------------

impl fmt::Debug for TaggedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind() {
            ValueKind::Nil => write!(f, "nil"),
            ValueKind::T => write!(f, "t"),
            ValueKind::Fixnum(n) => write!(f, "{}", n),
            ValueKind::Symbol(id) => write!(f, "Symbol({:?})", id),
            ValueKind::Cons => write!(f, "Cons@{:#x}", self.0 & !TAG_MASK),
            ValueKind::String => write!(f, "String@{:#x}", self.0 & !TAG_MASK),
            ValueKind::Float => {
                write!(f, "Float({})", self.xfloat())
            }
            ValueKind::Veclike(ty) => write!(f, "{:?}@{:#x}", ty, self.0 & !TAG_MASK),
            ValueKind::Unknown => write!(f, "Unknown({:#x})", self.0),
        }
    }
}
