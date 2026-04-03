//! String interner for symbol, keyword, and subr names.
//!
//! `SymId` must stay stable across evaluator creation/destruction so values can
//! be formatted, compared, and moved between contexts without keeping an old
//! `Context` alive just for name resolution. The runtime therefore uses a
//! single append-only process interner, while tests can still instantiate local
//! `StringInterner`s directly for unit coverage.

use rustc_hash::FxHashMap;
use std::sync::{OnceLock, RwLock};

/// A compact handle to an interned string. Copy, 4 bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SymId(pub(crate) u32);

pub const NIL_SYM_ID: SymId = SymId(0);
pub const T_SYM_ID: SymId = SymId(1);

/// Append-only string interner. Guarantees: same string → same SymId.
pub struct StringInterner {
    strings: Vec<&'static str>,
    map: FxHashMap<&'static str, u32>,
    canonical: Vec<bool>,
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
            map: FxHashMap::default(),
            canonical: Vec::new(),
        };
        // Pre-intern "nil" and "t" as SymId(0) and SymId(1) respectively.
        // TaggedValue::NIL = Symbol(0) and TaggedValue::T = Symbol(1)
        // rely on these exact assignments.
        let nil_id = interner.intern("nil");
        debug_assert_eq!(nil_id, NIL_SYM_ID);
        let t_id = interner.intern("t");
        debug_assert_eq!(t_id, T_SYM_ID);
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
        self.canonical.push(true);
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
        self.canonical.push(false);
        SymId(idx)
    }

    /// Look up the canonical interned id for a string without interning it.
    pub fn lookup(&self, s: &str) -> Option<SymId> {
        self.map.get(s).copied().map(SymId)
    }

    #[inline]
    pub fn is_canonical_id(&self, id: SymId) -> bool {
        self.canonical.get(id.0 as usize).copied().unwrap_or(false)
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
            map: FxHashMap::with_capacity_and_hasher(strings.len(), Default::default()),
            canonical: Vec::with_capacity(strings.len()),
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
        let canonical = match self.map.entry(leaked) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(idx);
                true
            }
            std::collections::hash_map::Entry::Occupied(_) => false,
        };
        self.canonical.push(canonical);
        SymId(idx)
    }
}

fn global_interner() -> &'static RwLock<StringInterner> {
    static GLOBAL_INTERNER: OnceLock<RwLock<StringInterner>> = OnceLock::new();
    GLOBAL_INTERNER.get_or_init(|| RwLock::new(StringInterner::new()))
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

#[inline]
pub fn is_canonical_id(id: SymId) -> bool {
    let interner = global_interner()
        .read()
        .expect("global interner poisoned during canonical-id lookup");
    interner.is_canonical_id(id)
}

#[inline]
pub fn resolve_sym_metadata(id: SymId) -> (&'static str, bool) {
    let interner = global_interner()
        .read()
        .expect("global interner poisoned during metadata lookup");
    (interner.resolve(id), interner.is_canonical_id(id))
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
