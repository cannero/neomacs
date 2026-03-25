//! String interner for symbol, keyword, and subr names.
//!
//! Provides `SymId(u32)` — a compact, Copy handle into an append-only
//! `StringInterner`. This replaces the `String` in `Value::Symbol`,
//! `Value::Keyword`, and `Value::Subr`, making `Value` `Copy` and 16 bytes.
//!
//! Thread-local access follows the same pattern as `CURRENT_HEAP` in `value.rs`.

use std::cell::Cell;
use std::collections::HashMap;

/// A compact handle to an interned string. Copy, 4 bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SymId(pub(crate) u32);

/// Append-only string interner. Guarantees: same string → same SymId.
pub struct StringInterner {
    strings: Vec<String>,
    map: HashMap<String, u32>,
}

impl Default for StringInterner {
    fn default() -> Self {
        Self::new()
    }
}

impl StringInterner {
    pub fn new() -> Self {
        Self {
            strings: Vec::new(),
            map: HashMap::new(),
        }
    }

    /// Intern a string, returning its unique id.
    /// If the string was already interned, returns the existing id.
    pub fn intern(&mut self, s: &str) -> SymId {
        if let Some(&idx) = self.map.get(s) {
            return SymId(idx);
        }
        let idx = self.strings.len() as u32;
        self.strings.push(s.to_owned());
        self.map.insert(s.to_owned(), idx);
        SymId(idx)
    }

    /// Create an uninterned symbol with the given name.
    /// Always allocates a NEW SymId, even if the name already exists.
    /// The new SymId is NOT added to the dedup map, so `intern(name)`
    /// will still return the original interned SymId.
    /// This implements Emacs Lisp's `make-symbol` semantics.
    pub fn intern_uninterned(&mut self, s: &str) -> SymId {
        let idx = self.strings.len() as u32;
        self.strings.push(s.to_owned());
        // Deliberately NOT inserting into self.map
        SymId(idx)
    }

    /// Look up the canonical interned id for a string without interning it.
    pub fn lookup(&self, s: &str) -> Option<SymId> {
        self.map.get(s).copied().map(SymId)
    }

    /// Resolve a SymId back to its string. Panics if id is invalid.
    #[inline]
    pub fn resolve(&self, id: SymId) -> &str {
        &self.strings[id.0 as usize]
    }

    /// Access all interned strings (for pdump serialization).
    pub(crate) fn strings(&self) -> &[String] {
        &self.strings
    }

    /// Reconstruct a StringInterner from a list of strings (for pdump load).
    /// Rebuilds the dedup map. Strings that appear multiple times (uninterned
    /// symbols) are NOT added to the dedup map after the first occurrence.
    pub(crate) fn from_strings(strings: Vec<String>) -> Self {
        let mut map = HashMap::with_capacity(strings.len());
        for (idx, s) in strings.iter().enumerate() {
            // Only insert the first occurrence (dedup map semantics)
            map.entry(s.clone()).or_insert(idx as u32);
        }
        Self { strings, map }
    }
}

// ---------------------------------------------------------------------------
// Thread-local interner access
// ---------------------------------------------------------------------------

thread_local! {
    static CURRENT_INTERNER: Cell<*mut StringInterner> = const { Cell::new(std::ptr::null_mut()) };
    #[cfg(test)]
    static TEST_FALLBACK_INTERNER: std::cell::RefCell<Option<Box<StringInterner>>> = const { std::cell::RefCell::new(None) };
}

/// Set the current thread-local interner pointer.
/// Must be called before any `intern()` / `resolve_sym()` calls.
pub fn set_current_interner(interner: &mut StringInterner) {
    CURRENT_INTERNER.with(|h| h.set(interner as *mut StringInterner));
}

/// Clear the thread-local interner pointer.
pub fn clear_current_interner() {
    CURRENT_INTERNER.with(|h| h.set(std::ptr::null_mut()));
}

/// Save and restore the current interner pointer around a closure.
/// Used when a temporary Context is created that would overwrite the thread-local.
pub(crate) fn with_saved_interner<R>(f: impl FnOnce() -> R) -> R {
    let saved = CURRENT_INTERNER.with(|h| h.get());
    let result = f();
    CURRENT_INTERNER.with(|h| h.set(saved));
    result
}

pub(crate) fn current_interner_ptr() -> *mut StringInterner {
    CURRENT_INTERNER.with(|h| h.get())
}

/// Get raw pointer to the current interner.
/// In test mode, auto-creates a fallback interner if none is set.
#[inline]
fn current_interner_ptr_or_fallback() -> *mut StringInterner {
    CURRENT_INTERNER.with(|h| {
        let ptr = h.get();
        if !ptr.is_null() {
            return ptr;
        }
        #[cfg(test)]
        {
            TEST_FALLBACK_INTERNER.with(|fb| {
                let mut borrow = fb.borrow_mut();
                if borrow.is_none() {
                    *borrow = Some(Box::new(StringInterner::new()));
                }
                let interner_ref: &mut StringInterner = borrow.as_mut().unwrap();
                let ptr = interner_ref as *mut StringInterner;
                h.set(ptr);
                ptr
            })
        }
        #[cfg(not(test))]
        {
            panic!("current interner not set — call set_current_interner() first");
        }
    })
}

/// Intern a string using the thread-local interner. Convenience wrapper.
#[inline]
pub fn intern(s: &str) -> SymId {
    let ptr = current_interner_ptr_or_fallback();
    unsafe { &mut *ptr }.intern(s)
}

/// Create an uninterned symbol using the thread-local interner.
/// Always creates a new unique SymId, never reuses an existing one.
#[inline]
pub fn intern_uninterned(s: &str) -> SymId {
    let ptr = current_interner_ptr_or_fallback();
    unsafe { &mut *ptr }.intern_uninterned(s)
}

/// Look up the canonical interned id for a string without interning it.
#[inline]
pub fn lookup_interned(s: &str) -> Option<SymId> {
    let ptr = current_interner_ptr_or_fallback();
    unsafe { &*ptr }.lookup(s)
}

/// Resolve a SymId to its string using the thread-local interner.
///
/// # Safety
/// The returned `&str` borrows from the interner's internal `Vec<String>`.
/// Each `String`'s heap buffer is stable (append-only interner never removes
/// entries, and `String` data lives on the heap not inline in the Vec).
/// The interner outlives all Values (owned by Context).
/// Same unsafe-pointer pattern as `as_str()` / `get_lambda_data()` in value.rs.
#[inline]
pub fn resolve_sym(id: SymId) -> &'static str {
    let ptr = current_interner_ptr_or_fallback();
    let interner = unsafe { &*ptr };
    let s = interner.resolve(id);
    // Safety: The String's heap buffer is stable because:
    // 1. StringInterner is append-only (strings never removed)
    // 2. String data lives on the heap, not inline in the Vec
    // 3. The interner outlives all Values (owned by Context)
    unsafe { &*(s as *const str) }
}
#[cfg(test)]
#[path = "intern_test.rs"]
mod tests;
