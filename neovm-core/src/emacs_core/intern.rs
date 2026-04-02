//! String interner for symbol, keyword, and subr names.
//!
//! `SymId` must stay stable across evaluator creation/destruction so values can
//! be formatted, compared, and moved between contexts without keeping an old
//! `Context` alive just for name resolution. The runtime therefore uses a
//! single append-only process interner, while tests can still instantiate local
//! `StringInterner`s directly for unit coverage.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

/// A compact handle to an interned string. Copy, 4 bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SymId(pub(crate) u32);

/// Append-only string interner. Guarantees: same string → same SymId.
pub struct StringInterner {
    strings: Vec<&'static str>,
    map: HashMap<&'static str, u32>,
}

impl Default for StringInterner {
    fn default() -> Self {
        Self::new()
    }
}

impl StringInterner {
    pub fn new() -> Self {
        let mut interner = Self {
            strings: Vec::new(),
            map: HashMap::new(),
        };
        // Pre-intern "nil" and "t" as SymId(0) and SymId(1) respectively.
        // TaggedValue::NIL = Symbol(0) and TaggedValue::T = Symbol(1)
        // rely on these exact assignments.
        let nil_id = interner.intern("nil");
        debug_assert_eq!(nil_id, SymId(0));
        let t_id = interner.intern("t");
        debug_assert_eq!(t_id, SymId(1));
        interner
    }

    /// Intern a string, returning its unique id.
    /// If the string was already interned, returns the existing id.
    pub fn intern(&mut self, s: &str) -> SymId {
        if let Some(&idx) = self.map.get(s) {
            return SymId(idx);
        }
        let idx = self.strings.len() as u32;
        let leaked = Box::leak(s.to_owned().into_boxed_str()) as &'static str;
        self.strings.push(leaked);
        self.map.insert(leaked, idx);
        SymId(idx)
    }

    /// Create an uninterned symbol with the given name.
    /// Always allocates a NEW SymId, even if the name already exists.
    /// The new SymId is NOT added to the dedup map, so `intern(name)`
    /// will still return the original interned SymId.
    /// This implements Emacs Lisp's `make-symbol` semantics.
    pub fn intern_uninterned(&mut self, s: &str) -> SymId {
        let idx = self.strings.len() as u32;
        let leaked = Box::leak(s.to_owned().into_boxed_str()) as &'static str;
        self.strings.push(leaked);
        // Deliberately NOT inserting into self.map
        SymId(idx)
    }

    /// Look up the canonical interned id for a string without interning it.
    pub fn lookup(&self, s: &str) -> Option<SymId> {
        self.map.get(s).copied().map(SymId)
    }

    /// Resolve a SymId back to its string. Panics if id is invalid.
    #[inline]
    pub fn resolve(&self, id: SymId) -> &'static str {
        self.strings[id.0 as usize]
    }

    /// Access all interned strings (for pdump serialization).
    pub(crate) fn strings(&self) -> &[&'static str] {
        &self.strings
    }

    /// Reconstruct a StringInterner from a list of strings (for pdump load).
    /// Rebuilds the dedup map. Strings that appear multiple times (uninterned
    /// symbols) are NOT added to the dedup map after the first occurrence.
    pub(crate) fn from_strings(strings: Vec<String>) -> Self {
        let mut interner = Self {
            strings: Vec::with_capacity(strings.len()),
            map: HashMap::with_capacity(strings.len()),
        };
        for s in strings {
            interner.push_preserving_slot(s);
        }
        interner
    }

    /// Extend this interner so its first `strings.len()` slots exactly match
    /// the provided serialized dump order.
    pub(crate) fn ensure_from_strings(&mut self, strings: &[String]) {
        for (idx, expected) in strings.iter().enumerate() {
            if let Some(existing) = self.strings.get(idx) {
                assert_eq!(
                    *existing,
                    expected.as_str(),
                    "global interner slot {idx} diverged from dump state"
                );
                continue;
            }
            let inserted = self.push_preserving_slot(expected.clone());
            debug_assert_eq!(inserted.0 as usize, idx);
        }
    }

    fn push_preserving_slot(&mut self, s: String) -> SymId {
        let idx = self.strings.len() as u32;
        let leaked = Box::leak(s.into_boxed_str()) as &'static str;
        self.strings.push(leaked);
        self.map.entry(leaked).or_insert(idx);
        SymId(idx)
    }
}

fn global_interner() -> &'static RwLock<StringInterner> {
    static GLOBAL_INTERNER: OnceLock<RwLock<StringInterner>> = OnceLock::new();
    GLOBAL_INTERNER.get_or_init(|| RwLock::new(StringInterner::new()))
}

/// Legacy compatibility shim. The runtime interner is now process-global.
#[inline]
pub fn set_current_interner(_interner: &mut StringInterner) {}

/// Legacy compatibility shim. The runtime interner is now process-global.
#[inline]
pub fn clear_current_interner() {}

/// Save/restore hook retained for older call sites.
#[inline]
pub(crate) fn with_saved_interner<R>(f: impl FnOnce() -> R) -> R {
    f()
}

/// The old pointer API is kept only for tests that assert the old lifecycle.
/// A null pointer now means "no evaluator-owned interner".
#[inline]
pub(crate) fn current_interner_ptr() -> *mut StringInterner {
    std::ptr::null_mut()
}

pub(crate) fn dump_runtime_interner() -> StringInterner {
    let interner = global_interner()
        .read()
        .expect("global interner poisoned during dump");
    StringInterner::from_strings(interner.strings().iter().map(|s| (*s).to_owned()).collect())
}

pub(crate) fn ensure_runtime_interner(strings: &[String]) {
    let mut interner = global_interner()
        .write()
        .expect("global interner poisoned during restore");
    interner.ensure_from_strings(strings);
}

/// Intern a string using the global runtime interner.
#[inline]
pub fn intern(s: &str) -> SymId {
    let mut interner = global_interner()
        .write()
        .expect("global interner poisoned during intern");
    interner.intern(s)
}

/// Create an uninterned symbol using the global runtime interner.
/// Always creates a new unique SymId, never reuses an existing one.
#[inline]
pub fn intern_uninterned(s: &str) -> SymId {
    let mut interner = global_interner()
        .write()
        .expect("global interner poisoned during uninterned symbol creation");
    interner.intern_uninterned(s)
}

/// Look up the canonical interned id for a string without interning it.
#[inline]
pub fn lookup_interned(s: &str) -> Option<SymId> {
    let interner = global_interner()
        .read()
        .expect("global interner poisoned during lookup");
    interner.lookup(s)
}

/// Resolve a SymId to its string using the global runtime interner.
///
#[inline]
pub fn resolve_sym(id: SymId) -> &'static str {
    let interner = global_interner()
        .read()
        .expect("global interner poisoned during resolve");
    interner.resolve(id)
}
#[cfg(test)]
#[path = "intern_test.rs"]
mod tests;
