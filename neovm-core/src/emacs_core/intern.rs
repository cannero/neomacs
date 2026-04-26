//! Process-global symbol registry backed by a separate string atom table.
//!
//! `SymId` is Lisp symbol identity and must stay stable across evaluator
//! creation/destruction so values can be formatted, compared, and moved
//! between contexts without keeping an old `Context` alive just for name
//! resolution. The runtime therefore uses a single append-only process symbol
//! registry.
//!
//! Name atoms are tracked separately via [`NameId`]. This mirrors GNU's model
//! more closely: a symbol is an object with a name, not just "slot N in the
//! string interner".

use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use std::borrow::Cow;
use std::cell::RefCell;
use std::sync::OnceLock;

use crate::heap_types::LispString;

/// A compact handle to a Lisp symbol object. Copy, 4 bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
#[repr(transparent)]
pub struct SymId(pub(crate) u32);

/// A compact handle to a deduplicated symbol-name atom. Runtime-local only.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(transparent)]
pub struct NameId(pub(crate) u32);

pub const NIL_SYM_ID: SymId = SymId(0);
pub const T_SYM_ID: SymId = SymId(1);
pub const UNBOUND_SYM_ID: SymId = SymId(2);

/// Append-only string interner used only for symbol names.
pub struct StringInterner {
    strings: Vec<&'static LispString>,
    map: FxHashMap<&'static LispString, NameId>,
}

impl Default for StringInterner {
    fn default() -> Self {
        Self::new()
    }
}

impl StringInterner {
    fn normalize_symbol_name_lisp_string<'a>(s: &'a LispString) -> Cow<'a, LispString> {
        if s.is_ascii() && s.is_multibyte() {
            Cow::Owned(LispString::from_unibyte(s.as_bytes().to_vec()))
        } else {
            Cow::Borrowed(s)
        }
    }

    pub fn new() -> Self {
        Self {
            strings: Vec::new(),
            map: FxHashMap::default(),
        }
    }

    fn name_atom_from_str(s: &str) -> LispString {
        if s.is_ascii() {
            LispString::from_unibyte(s.as_bytes().to_vec())
        } else {
            LispString::from_utf8(s)
        }
    }

    /// Intern a symbol-name atom, returning its unique id.
    pub fn intern(&mut self, s: &str) -> NameId {
        let atom = Self::name_atom_from_str(s);
        self.intern_lisp_string(&atom)
    }

    /// Intern a symbol-name atom from an exact Lisp string representation.
    pub fn intern_lisp_string(&mut self, s: &LispString) -> NameId {
        let normalized = Self::normalize_symbol_name_lisp_string(s);
        if let Some(&idx) = self.map.get(normalized.as_ref()) {
            return idx;
        }
        let idx = NameId(self.strings.len() as u32);
        let leaked = Box::leak(Box::new(normalized.into_owned())) as &'static LispString;
        self.strings.push(leaked);
        self.map.insert(leaked, idx);
        idx
    }

    /// Look up a symbol-name atom without interning it.
    pub fn lookup(&self, s: &str) -> Option<NameId> {
        let atom = Self::name_atom_from_str(s);
        self.lookup_lisp_string(&atom)
    }

    /// Look up a symbol-name atom without interning it.
    pub fn lookup_lisp_string(&self, s: &LispString) -> Option<NameId> {
        let normalized = Self::normalize_symbol_name_lisp_string(s);
        self.map.get(normalized.as_ref()).copied()
    }

    /// Resolve a name id back to its string. Panics if id is invalid.
    #[inline]
    pub fn resolve(&self, id: NameId) -> &'static str {
        self.resolve_lisp_string(id)
            .as_utf8_str()
            .unwrap_or_else(|| panic!("symbol name {:?} is not valid UTF-8", id))
    }

    /// Resolve a name id back to its exact Lisp-string storage.
    #[inline]
    pub fn resolve_lisp_string(&self, id: NameId) -> &'static LispString {
        self.strings[id.0 as usize]
    }
}

#[derive(Clone, Copy, Debug)]
struct SymbolSlot {
    name: NameId,
    canonical: bool,
}

pub(crate) struct DumpedSymbolTable {
    pub names: Vec<LispString>,
    pub symbol_names: Vec<u32>,
    pub canonical: Vec<bool>,
}

#[derive(Debug)]
pub(crate) struct RestoredDumpSymbolTable {
    pub names: Vec<NameId>,
    pub symbols: Vec<SymId>,
}

/// Process-global append-only registry of Lisp symbols.
struct SymbolRegistry {
    names: StringInterner,
    symbols: Vec<SymbolSlot>,
    canonical_by_name: FxHashMap<NameId, SymId>,
}

impl Default for SymbolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolRegistry {
    fn new() -> Self {
        let mut registry = Self {
            names: StringInterner::new(),
            symbols: Vec::new(),
            canonical_by_name: FxHashMap::default(),
        };
        let nil_name = registry.names.intern("nil");
        let nil_id = registry.alloc_symbol(nil_name, true);
        debug_assert_eq!(nil_id, NIL_SYM_ID);

        let t_name = registry.names.intern("t");
        let t_id = registry.alloc_symbol(t_name, true);
        debug_assert_eq!(t_id, T_SYM_ID);

        let unbound_name = registry.names.intern("unbound");
        let unbound_id = registry.alloc_symbol(unbound_name, false);
        debug_assert_eq!(unbound_id, UNBOUND_SYM_ID);

        registry
    }

    fn alloc_symbol(&mut self, name: NameId, canonical: bool) -> SymId {
        let id = SymId(self.symbols.len() as u32);
        self.symbols.push(SymbolSlot { name, canonical });
        if canonical {
            self.canonical_by_name.insert(name, id);
        }
        id
    }

    fn slot(&self, id: SymId) -> Option<&SymbolSlot> {
        self.symbols.get(id.0 as usize)
    }

    fn intern(&mut self, s: &str) -> SymId {
        let name = self.names.intern(s);
        if let Some(existing) = self.canonical_by_name.get(&name) {
            return *existing;
        }
        self.alloc_symbol(name, true)
    }

    fn intern_lisp_string(&mut self, s: &LispString) -> SymId {
        let name = self.names.intern_lisp_string(s);
        if let Some(existing) = self.canonical_by_name.get(&name) {
            return *existing;
        }
        self.alloc_symbol(name, true)
    }

    fn intern_uninterned(&mut self, s: &str) -> SymId {
        let name = self.names.intern(s);
        self.alloc_symbol(name, false)
    }

    fn intern_uninterned_lisp_string(&mut self, s: &LispString) -> SymId {
        let name = self.names.intern_lisp_string(s);
        self.alloc_symbol(name, false)
    }

    fn lookup(&self, s: &str) -> Option<SymId> {
        let name = self.names.lookup(s)?;
        self.canonical_by_name.get(&name).copied()
    }

    fn lookup_lisp_string(&self, s: &LispString) -> Option<SymId> {
        let name = self.names.lookup_lisp_string(s)?;
        self.canonical_by_name.get(&name).copied()
    }

    #[inline]
    fn is_canonical_id(&self, id: SymId) -> bool {
        self.slot(id).map(|slot| slot.canonical).unwrap_or(false)
    }

    #[inline]
    fn resolve(&self, id: SymId) -> &'static str {
        let slot = self
            .slot(id)
            .unwrap_or_else(|| panic!("invalid symbol id {:?}", id));
        self.names.resolve(slot.name)
    }

    #[inline]
    fn resolve_lisp_string(&self, id: SymId) -> &'static LispString {
        let slot = self
            .slot(id)
            .unwrap_or_else(|| panic!("invalid symbol id {:?}", id));
        self.names.resolve_lisp_string(slot.name)
    }

    #[inline]
    fn name_id(&self, id: SymId) -> NameId {
        self.slot(id)
            .unwrap_or_else(|| panic!("invalid symbol id {:?}", id))
            .name
    }

    #[inline]
    fn resolve_name(&self, id: NameId) -> &'static str {
        self.names.resolve(id)
    }

    #[inline]
    fn resolve_name_lisp_string(&self, id: NameId) -> &'static LispString {
        self.names.resolve_lisp_string(id)
    }

    fn dump_symbol_table(&self) -> DumpedSymbolTable {
        let names = self
            .names
            .strings
            .iter()
            .map(|name| (*name).clone())
            .collect();
        let mut symbol_names = Vec::with_capacity(self.symbols.len());
        let mut canonical = Vec::with_capacity(self.symbols.len());
        for slot in &self.symbols {
            symbol_names.push(slot.name.0);
            canonical.push(slot.canonical);
        }
        DumpedSymbolTable {
            names,
            symbol_names,
            canonical,
        }
    }

    fn restore_dump_symbol_table(
        &mut self,
        names: &[LispString],
        symbol_names: &[u32],
        canonical: Option<&[bool]>,
    ) -> Result<RestoredDumpSymbolTable, String> {
        let mut name_remap = Vec::with_capacity(names.len());
        for name in names {
            name_remap.push(self.names.intern_lisp_string(name));
        }

        let derived_flags;
        let canonical = match canonical {
            Some(flags) if flags.len() == symbol_names.len() => flags,
            Some(flags) if flags.is_empty() => {
                derived_flags = derive_legacy_canonical_flags_from_names(names, symbol_names)?;
                &derived_flags
            }
            None => {
                derived_flags = derive_legacy_canonical_flags_from_names(names, symbol_names)?;
                &derived_flags
            }
            Some(flags) => {
                return Err(format!(
                    "pdump symbol metadata is inconsistent: {} symbols but {} canonical flags",
                    symbol_names.len(),
                    flags.len()
                ));
            }
        };

        if symbol_names.len() != canonical.len() {
            return Err(format!(
                "pdump symbol metadata is inconsistent: {} symbols but {} canonical flags",
                symbol_names.len(),
                canonical.len()
            ));
        }

        let mut dump_canonical_slots: FxHashMap<NameId, usize> = FxHashMap::default();

        let symbol_remap = symbol_names
            .iter()
            .copied()
            .zip(canonical.iter().copied())
            .enumerate()
            .map(|(slot, (dump_name_id, is_canonical))| {
                let runtime_name = name_remap
                    .get(dump_name_id as usize)
                    .copied()
                    .ok_or_else(|| {
                        format!(
                            "pdump symbol metadata is inconsistent: symbol name id {} out of range for {} names",
                            dump_name_id,
                            names.len()
                        )
                    })?;
                if is_canonical {
                    if let Some(previous_slot) = dump_canonical_slots.insert(runtime_name, slot) {
                        return Err(format!(
                            "pdump symbol metadata is inconsistent: canonical symbol slots {} and {} both name {}",
                            previous_slot,
                            slot,
                            self.names.resolve(runtime_name)
                        ));
                    }
                    Ok::<SymId, String>(
                        self.canonical_by_name
                            .get(&runtime_name)
                            .copied()
                            .unwrap_or_else(|| self.alloc_symbol(runtime_name, true)),
                    )
                } else {
                    Ok::<SymId, String>(self.alloc_symbol(runtime_name, false))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(RestoredDumpSymbolTable {
            names: name_remap,
            symbols: symbol_remap,
        })
    }

    #[inline]
    fn canonical_symbol_for_name(&self, name: NameId) -> Option<SymId> {
        self.canonical_by_name.get(&name).copied()
    }
}

fn derive_legacy_canonical_flags_from_names(
    names: &[LispString],
    symbol_names: &[u32],
) -> Result<Vec<bool>, String> {
    let mut seen = FxHashMap::default();
    symbol_names
        .iter()
        .copied()
        .map(|dump_name_id| {
            let name = names.get(dump_name_id as usize).ok_or_else(|| {
                format!(
                    "pdump symbol metadata is inconsistent: symbol name id {} out of range for {} names",
                    dump_name_id,
                    names.len()
                )
            })?;
            Ok(seen.insert(name.clone(), ()).is_none())
        })
        .collect()
}

fn global_symbol_registry() -> &'static RwLock<SymbolRegistry> {
    static GLOBAL_SYMBOL_REGISTRY: OnceLock<RwLock<SymbolRegistry>> = OnceLock::new();
    GLOBAL_SYMBOL_REGISTRY.get_or_init(|| RwLock::new(SymbolRegistry::new()))
}

pub(crate) fn dump_runtime_interner() -> DumpedSymbolTable {
    let registry = global_symbol_registry().read();
    registry.dump_symbol_table()
}

pub(crate) fn restore_runtime_interner(
    names: &[LispString],
    symbol_names: &[u32],
    canonical: Option<&[bool]>,
) -> Result<RestoredDumpSymbolTable, String> {
    let mut registry = global_symbol_registry().write();
    registry.restore_dump_symbol_table(names, symbol_names, canonical)
}

/// Intern a string using the global runtime symbol registry.
#[inline]
pub fn intern(s: &str) -> SymId {
    let mut registry = global_symbol_registry().write();
    registry.intern(s)
}

/// Intern an exact Lisp-string name using the global runtime symbol registry.
#[inline]
pub fn intern_lisp_string(s: &LispString) -> SymId {
    let mut registry = global_symbol_registry().write();
    registry.intern_lisp_string(s)
}

/// Create an uninterned symbol using the global runtime symbol registry.
/// Always creates a new unique SymId, never reuses an existing one.
#[inline]
pub fn intern_uninterned(s: &str) -> SymId {
    let mut registry = global_symbol_registry().write();
    registry.intern_uninterned(s)
}

/// Create an uninterned symbol using an exact Lisp-string name.
#[inline]
pub fn intern_uninterned_lisp_string(s: &LispString) -> SymId {
    let mut registry = global_symbol_registry().write();
    registry.intern_uninterned_lisp_string(s)
}

/// Look up the canonical interned symbol id for a string without interning it.
#[inline]
pub fn lookup_interned(s: &str) -> Option<SymId> {
    let registry = global_symbol_registry().read();
    registry.lookup(s)
}

#[inline]
pub fn lookup_interned_lisp_string(s: &LispString) -> Option<SymId> {
    let registry = global_symbol_registry().read();
    registry.lookup_lisp_string(s)
}

#[inline]
pub fn is_canonical_id(id: SymId) -> bool {
    if let Some(is_canonical) = thread_local_is_canonical(id) {
        return is_canonical;
    }
    let registry = global_symbol_registry().read();
    let is_canonical = registry.is_canonical_id(id);
    drop(registry);
    thread_local_record_canonical(id, is_canonical);
    is_canonical
}

#[inline]
pub(crate) fn is_keyword_id(id: SymId) -> bool {
    if let Some(is_keyword) = thread_local_keyword(id) {
        return is_keyword;
    }
    let registry = global_symbol_registry().read();
    let is_keyword = registry
        .slot(id)
        .map(|slot| {
            slot.canonical
                && registry
                    .names
                    .resolve_lisp_string(slot.name)
                    .as_bytes()
                    .first()
                    .is_some_and(|byte| *byte == b':')
        })
        .unwrap_or(false);
    drop(registry);
    thread_local_record_keyword(id, is_keyword);
    is_keyword
}

#[inline]
pub fn resolve_sym_metadata(id: SymId) -> (&'static str, bool) {
    if let (Some(name), Some(is_canonical)) =
        (thread_local_resolve(id), thread_local_is_canonical(id))
    {
        return (name, is_canonical);
    }
    let registry = global_symbol_registry().read();
    let name = registry.resolve(id);
    let is_canonical = registry.is_canonical_id(id);
    drop(registry);
    thread_local_record(id, name);
    thread_local_record_canonical(id, is_canonical);
    (name, is_canonical)
}

#[inline]
pub(crate) fn symbol_name_id(id: SymId) -> NameId {
    if let Some(name_id) = thread_local_name_id(id) {
        return name_id;
    }
    let registry = global_symbol_registry().read();
    let name_id = registry.name_id(id);
    drop(registry);
    thread_local_record_name_id(id, name_id);
    name_id
}

#[inline]
pub(crate) fn resolve_name(id: NameId) -> &'static str {
    let registry = global_symbol_registry().read();
    registry.resolve_name(id)
}

#[inline]
pub(crate) fn resolve_name_lisp_string(id: NameId) -> &'static LispString {
    let registry = global_symbol_registry().read();
    registry.resolve_name_lisp_string(id)
}

#[inline]
pub(crate) fn canonical_symbol_for_name(id: NameId) -> Option<SymId> {
    let registry = global_symbol_registry().read();
    registry.canonical_symbol_for_name(id)
}

/// Resolve a SymId to its string using the global runtime symbol registry.
#[inline]
pub fn resolve_sym(id: SymId) -> &'static str {
    if let Some(s) = thread_local_resolve(id) {
        return s;
    }
    let registry = global_symbol_registry().read();
    let s = registry.resolve(id);
    drop(registry);
    thread_local_record(id, s);
    s
}

/// Resolve a SymId to its exact Lisp-string name using the global runtime
/// symbol registry.
#[inline]
pub fn resolve_sym_lisp_string(id: SymId) -> &'static LispString {
    let registry = global_symbol_registry().read();
    registry.resolve_lisp_string(id)
}

// ---------------------------------------------------------------------------
// Thread-local lockless cache for SymId -> &'static str
// ---------------------------------------------------------------------------
//
// `resolve_sym` is called from many bytecode hot paths (e.g. `is_keyword`,
// debug formatting) and acquiring the global RwLock — even with parking_lot
// — is many extra atomic ops per call. Once a SymId is interned, the
// underlying `&'static str` is permanently valid, so the (id -> str) mapping
// is monotonic and stable for the lifetime of the process.

thread_local! {
    static SYM_NAME_CACHE: RefCell<Vec<Option<&'static str>>> = const { RefCell::new(Vec::new()) };
    static SYM_NAME_ID_CACHE: RefCell<Vec<Option<NameId>>> = const { RefCell::new(Vec::new()) };
    static SYM_CANONICAL_CACHE: RefCell<Vec<Option<bool>>> = const { RefCell::new(Vec::new()) };
    static SYM_KEYWORD_CACHE: RefCell<Vec<Option<bool>>> = const { RefCell::new(Vec::new()) };
}

#[inline]
fn thread_local_resolve(id: SymId) -> Option<&'static str> {
    SYM_NAME_CACHE.with(|cache| {
        let cache = cache.borrow();
        cache.get(id.0 as usize).and_then(|slot| *slot)
    })
}

#[inline]
fn thread_local_record(id: SymId, name: &'static str) {
    SYM_NAME_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let idx = id.0 as usize;
        if cache.len() <= idx {
            cache.resize(idx + 1, None);
        }
        cache[idx] = Some(name);
    });
}

#[inline]
fn thread_local_name_id(id: SymId) -> Option<NameId> {
    SYM_NAME_ID_CACHE.with(|cache| {
        let cache = cache.borrow();
        cache.get(id.0 as usize).and_then(|slot| *slot)
    })
}

#[inline]
fn thread_local_record_name_id(id: SymId, name_id: NameId) {
    SYM_NAME_ID_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let idx = id.0 as usize;
        if cache.len() <= idx {
            cache.resize(idx + 1, None);
        }
        cache[idx] = Some(name_id);
    });
}

#[inline]
fn thread_local_is_canonical(id: SymId) -> Option<bool> {
    SYM_CANONICAL_CACHE.with(|cache| {
        let cache = cache.borrow();
        cache.get(id.0 as usize).and_then(|slot| *slot)
    })
}

#[inline]
fn thread_local_record_canonical(id: SymId, is_canonical: bool) {
    SYM_CANONICAL_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let idx = id.0 as usize;
        if cache.len() <= idx {
            cache.resize(idx + 1, None);
        }
        cache[idx] = Some(is_canonical);
    });
}

#[inline]
fn thread_local_keyword(id: SymId) -> Option<bool> {
    SYM_KEYWORD_CACHE.with(|cache| {
        let cache = cache.borrow();
        cache.get(id.0 as usize).and_then(|slot| *slot)
    })
}

#[inline]
fn thread_local_record_keyword(id: SymId, is_keyword: bool) {
    SYM_KEYWORD_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let idx = id.0 as usize;
        if cache.len() <= idx {
            cache.resize(idx + 1, None);
        }
        cache[idx] = Some(is_keyword);
    });
}

/// Resolve a SymId to its string using the global runtime symbol registry.
///
/// Returns `None` if the id is outside the current symbol range instead of
/// panicking. This is useful at serialization boundaries where we want a
/// structured error instead of aborting the process on malformed runtime data.
#[inline]
pub fn try_resolve_sym(id: SymId) -> Option<&'static str> {
    let registry = global_symbol_registry().read();
    registry
        .slot(id)
        .map(|slot| registry.names.resolve(slot.name))
}

#[inline]
pub fn try_resolve_sym_lisp_string(id: SymId) -> Option<&'static LispString> {
    let registry = global_symbol_registry().read();
    registry
        .slot(id)
        .map(|slot| registry.names.resolve_lisp_string(slot.name))
}

#[cfg(test)]
#[path = "intern_test.rs"]
mod tests;
