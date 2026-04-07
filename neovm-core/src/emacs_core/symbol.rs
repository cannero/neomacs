//! Obarray and symbol interning.
//!
//! In Emacs, symbols are unique objects stored in an "obarray" (hash table).
//! Each symbol has:
//! - A name (string)
//! - A value cell (variable binding)
//! - A function cell (function binding)
//! - A property list (plist)
//! - A `special` flag (for dynamic binding in lexical scope)
//!
//! # Redirect machinery (GNU `Lisp_Symbol::redirect`)
//!
//! Mirrors GNU Emacs's `enum symbol_redirect` (`src/lisp.h:771-777`). Every
//! symbol has a [`SymbolRedirect`] tag that determines how its value cell is
//! interpreted:
//!
//! | Tag         | `val` payload                  | GNU equivalent      |
//! | ----------- | ------------------------------ | ------------------- |
//! | `Plainval`  | direct [`Value`] (or UNBOUND)  | `SYMBOL_PLAINVAL`   |
//! | `Varalias`  | aliased [`SymId`]              | `SYMBOL_VARALIAS`   |
//! | `Localized` | `*mut LispBufferLocalValue`    | `SYMBOL_LOCALIZED`  |
//! | `Forwarded` | `*const LispFwd`               | `SYMBOL_FORWARDED`  |
//!
//! Phase 1 of the symbol-redirect refactor (`drafts/symbol-redirect-plan.md`)
//! introduces the new shape but every existing symbol still routes through
//! `Plainval`. The `BufferLocal` and `Forwarded` paths still also live on
//! the legacy `SymbolValue` enum during the transition; Phases 4-8 cut them
//! over to the redirect dispatch and Phase 10 deletes the legacy enum.

use super::intern::{SymId, intern, is_canonical_id, lookup_interned, resolve_sym};
use super::value::{Value, ValueKind};
use crate::gc_trace::GcTrace;
use rustc_hash::FxHashMap;

// ===========================================================================
// Redirect machinery — mirrors GNU `lisp.h:771-829`
// ===========================================================================

/// Two-bit `redirect` tag. Mirrors GNU `enum symbol_redirect`
/// (`src/lisp.h:771-777`). Discriminant for [`SymbolVal`].
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum SymbolRedirect {
    /// Value is in `val.plain`. GNU `SYMBOL_PLAINVAL`.
    #[default]
    Plainval = 0,
    /// Value is really in another symbol. GNU `SYMBOL_VARALIAS`.
    Varalias = 1,
    /// Value is in a buffer-local cache. GNU `SYMBOL_LOCALIZED`.
    Localized = 2,
    /// Value is in a static C-side variable. GNU `SYMBOL_FORWARDED`.
    Forwarded = 3,
}

/// Two-bit `trapped_write` flag. Mirrors GNU `enum symbol_trapped_write`
/// (`src/lisp.h:780-785`).
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum SymbolTrappedWrite {
    /// Normal symbol. GNU `SYMBOL_UNTRAPPED_WRITE`.
    #[default]
    Untrapped = 0,
    /// Constant — write attempts signal `setting-constant`. GNU `SYMBOL_NOWRITE`.
    NoWrite = 1,
    /// Variable watchers fire on every write. GNU `SYMBOL_TRAPPED_WRITE`.
    Trapped = 2,
}

/// Two-bit `interned` flag. Mirrors GNU `enum symbol_interned`
/// (`src/lisp.h:782-787`).
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum SymbolInterned {
    /// Uninterned (e.g. `make-symbol`). GNU `SYMBOL_UNINTERNED`.
    #[default]
    Uninterned = 0,
    /// Interned in some obarray. GNU `SYMBOL_INTERNED`.
    Interned = 1,
    /// Interned in the *initial* obarray (the global one). GNU
    /// `SYMBOL_INTERNED_IN_INITIAL_OBARRAY`. Used for keywords.
    InternedInInitial = 2,
}

/// Packed flags byte for a [`LispSymbol`]. Mirrors the bit-packed first byte
/// of GNU `Lisp_Symbol::s` (`src/lisp.h:786-792`).
///
/// Bit layout:
/// ```text
///   bits 0..2 : SymbolRedirect
///   bits 2..4 : SymbolTrappedWrite
///   bits 4..6 : SymbolInterned
///   bit  6    : declared_special
///   bit  7    : reserved
/// ```
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Default)]
pub struct SymbolFlags(u8);

impl SymbolFlags {
    const REDIRECT_MASK: u8 = 0b0000_0011;
    const TRAPPED_WRITE_SHIFT: u8 = 2;
    const TRAPPED_WRITE_MASK: u8 = 0b0000_1100;
    const INTERNED_SHIFT: u8 = 4;
    const INTERNED_MASK: u8 = 0b0011_0000;
    const DECLARED_SPECIAL_BIT: u8 = 0b0100_0000;

    #[inline]
    pub fn redirect(self) -> SymbolRedirect {
        // Safety: SymbolRedirect is `#[repr(u8)]` with values 0..=3 and the
        // mask restricts to 2 bits.
        unsafe { std::mem::transmute(self.0 & Self::REDIRECT_MASK) }
    }

    #[inline]
    pub fn set_redirect(&mut self, r: SymbolRedirect) {
        self.0 = (self.0 & !Self::REDIRECT_MASK) | (r as u8);
    }

    #[inline]
    pub fn trapped_write(self) -> SymbolTrappedWrite {
        let raw = (self.0 & Self::TRAPPED_WRITE_MASK) >> Self::TRAPPED_WRITE_SHIFT;
        // Safety: 2-bit value, valid SymbolTrappedWrite discriminants.
        unsafe { std::mem::transmute(raw) }
    }

    #[inline]
    pub fn set_trapped_write(&mut self, t: SymbolTrappedWrite) {
        self.0 = (self.0 & !Self::TRAPPED_WRITE_MASK)
            | ((t as u8) << Self::TRAPPED_WRITE_SHIFT);
    }

    #[inline]
    pub fn interned(self) -> SymbolInterned {
        let raw = (self.0 & Self::INTERNED_MASK) >> Self::INTERNED_SHIFT;
        // Safety: 2-bit value, valid SymbolInterned discriminants.
        unsafe { std::mem::transmute(raw) }
    }

    #[inline]
    pub fn set_interned(&mut self, i: SymbolInterned) {
        self.0 = (self.0 & !Self::INTERNED_MASK) | ((i as u8) << Self::INTERNED_SHIFT);
    }

    #[inline]
    pub fn declared_special(self) -> bool {
        self.0 & Self::DECLARED_SPECIAL_BIT != 0
    }

    #[inline]
    pub fn set_declared_special(&mut self, v: bool) {
        if v {
            self.0 |= Self::DECLARED_SPECIAL_BIT;
        } else {
            self.0 &= !Self::DECLARED_SPECIAL_BIT;
        }
    }
}

/// One-word value cell for a symbol, reinterpreted by the [`SymbolFlags`]
/// `redirect` tag. Mirrors GNU `union { Lisp_Object value; struct
/// Lisp_Symbol *alias; struct Lisp_Buffer_Local_Value *blv; lispfwd fwd; }`
/// at `src/lisp.h:797-802`.
#[repr(C)]
#[derive(Copy, Clone)]
pub union SymbolVal {
    /// Live when redirect == Plainval. The value, or [`Value::NIL`] for
    /// "still unbound" (Phase 1 keeps an explicit "bound" bit on the side
    /// in [`LispSymbol::value`] until the legacy [`SymbolValue`] is removed
    /// in Phase 4-10).
    pub plain: Value,
    /// Live when redirect == Varalias. The aliased symbol id.
    pub alias: SymId,
    /// Live when redirect == Localized. Pointer to a heap-allocated
    /// per-symbol BLV cache. Null until Phase 4 wires up the LOCALIZED
    /// dispatch.
    pub blv: *mut LispBufferLocalValue,
    /// Live when redirect == Forwarded. Pointer to a 'static forwarder
    /// descriptor. Null until Phase 8 introduces forwarded variables.
    pub fwd: *const crate::emacs_core::forward::LispFwd,
}

impl Default for SymbolVal {
    fn default() -> Self {
        // Plainval / NIL is the safe initial state.
        Self { plain: Value::NIL }
    }
}

impl std::fmt::Debug for SymbolVal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Without the redirect tag we can't safely interpret the union;
        // print the raw bits for diagnostics.
        let raw: usize = unsafe { std::mem::transmute_copy(self) };
        write!(f, "SymbolVal({:#x})", raw)
    }
}

/// Per-symbol buffer-local cache. Mirrors GNU `struct
/// Lisp_Buffer_Local_Value` at `src/lisp.h:3116-3137`.
///
/// Phase 1 only declares the type; allocation and dispatch through it
/// land in Phases 4-6.
#[repr(C)]
#[derive(Clone, Debug)]
pub struct LispBufferLocalValue {
    /// True if `make-variable-buffer-local` was called: any subsequent
    /// `set` creates a per-buffer binding. GNU `local_if_set`.
    pub local_if_set: bool,
    /// True if the loaded binding (`valcell`) was actually found in the
    /// buffer's `local_var_alist`, vs. the default. GNU `found`.
    pub found: bool,
    /// Optional forwarder for variables that have BOTH a per-buffer
    /// binding *and* a static C slot (e.g. `case-fold-search`). Must not
    /// be a `BufferObj` or `KboardObj`.
    pub fwd: Option<&'static crate::emacs_core::forward::LispFwd>,
    /// Buffer for which `valcell` was loaded, or `Value::NIL` for the
    /// global default. GNU `where`.
    pub where_buf: Value,
    /// `(SYMBOL . DEFAULT-VALUE)` cons. GNU `defcell`.
    pub defcell: Value,
    /// `(SYMBOL . CURRENT-VALUE)` cons. Equal to `defcell` when no
    /// per-buffer binding is loaded. GNU `valcell`.
    pub valcell: Value,
}

// ===========================================================================
// Legacy value-cell enum — to be removed in Phase 4-10
// ===========================================================================

/// Legacy variant of [`LispSymbol::value`] retained until Phases 4-10
/// migrate every code path through the redirect dispatch. New code should
/// read [`LispSymbol::redirect`] / [`LispSymbol::plain`] etc. instead.
///
/// Mirrors GNU Emacs's `symbol_redirect` enum (`SYMBOL_PLAINVAL`,
/// `SYMBOL_VARALIAS`, `SYMBOL_LOCALIZED`, `SYMBOL_FORWARDED`).
#[derive(Clone, Debug)]
pub enum SymbolValue {
    /// Direct value (GNU: SYMBOL_PLAINVAL).
    Plain(Option<Value>),
    /// Alias to another symbol (GNU: SYMBOL_VARALIAS).
    Alias(SymId),
    /// Buffer-local variable (GNU: SYMBOL_LOCALIZED).
    BufferLocal {
        default: Option<Value>,
        local_if_set: bool,
    },
    /// Forwarded to Rust variable (GNU: SYMBOL_FORWARDED) — placeholder.
    Forwarded,
}

impl Default for SymbolValue {
    fn default() -> Self {
        SymbolValue::Plain(None)
    }
}

// ===========================================================================
// LispSymbol — per-symbol metadata stored in the obarray
// ===========================================================================

/// Per-symbol metadata stored in the obarray. Mirrors GNU `struct
/// Lisp_Symbol` at `src/lisp.h:786-829`, modulo Phase 1 transitional
/// fields that will be removed in later phases.
///
/// Renamed from `SymbolData` as part of the symbol-redirect refactor
/// (Phase 1). The legacy [`SymbolValue`] field stays alongside the new
/// [`SymbolFlags`] / [`SymbolVal`] fields during the transition; reads
/// and writes go through both, kept in sync, until Phase 4-10 removes
/// the legacy field.
#[derive(Clone, Debug)]
pub struct LispSymbol {
    /// The symbol's name.
    pub name: SymId,
    /// Packed flags: redirect tag, trapped-write tag, interned tag,
    /// declared-special bit. Mirrors the first byte of GNU
    /// `Lisp_Symbol::s` (`lisp.h:786-792`).
    pub flags: SymbolFlags,
    /// One-word value cell. Reinterpreted by `flags.redirect()`.
    pub val: SymbolVal,
    /// LEGACY value cell — see [`SymbolValue`]. Kept in sync with
    /// `flags + val` during Phase 1; will be removed in Phase 10.
    pub value: SymbolValue,
    /// Function cell (None = void-function).
    pub function: Option<Value>,
    /// Property list (flat alternating key-value pairs stored as HashMap).
    pub plist: FxHashMap<SymId, Value>,
    /// Whether this symbol is declared `special` (always dynamically bound).
    /// LEGACY mirror of `flags.declared_special()`. Removed in Phase 10.
    pub special: bool,
    /// Whether this symbol is a constant (defconst). LEGACY — will collapse
    /// into `flags.trapped_write() == NoWrite` in Phase 10.
    pub constant: bool,
    /// Whether this symbol is interned in the global obarray.
    interned_global: bool,
    /// Whether `fmakunbound` explicitly masked the symbol's fallback function.
    function_unbound: bool,
}

/// Type alias for backward compatibility with code that still uses the
/// pre-refactor name. Removed in Phase 10.
pub type SymbolData = LispSymbol;

impl LispSymbol {
    pub fn new(name: SymId) -> Self {
        let mut flags = SymbolFlags::default();
        flags.set_redirect(SymbolRedirect::Plainval);
        Self {
            name,
            flags,
            val: SymbolVal { plain: Value::NIL },
            value: SymbolValue::Plain(None),
            function: None,
            plist: FxHashMap::default(),
            special: false,
            constant: false,
            interned_global: false,
            function_unbound: false,
        }
    }

    /// Read the redirect tag.
    #[inline]
    pub fn redirect(&self) -> SymbolRedirect {
        self.flags.redirect()
    }

    /// Read the value cell as a plain `Value`. Caller must have verified
    /// the redirect is `Plainval`.
    #[inline]
    pub fn plain(&self) -> Value {
        debug_assert_eq!(self.redirect(), SymbolRedirect::Plainval);
        unsafe { self.val.plain }
    }

    /// Write the value cell as a plain `Value`. Caller must have set the
    /// redirect to `Plainval` (or be initializing a fresh symbol).
    #[inline]
    pub fn set_plain(&mut self, v: Value) {
        debug_assert_eq!(self.redirect(), SymbolRedirect::Plainval);
        self.val = SymbolVal { plain: v };
    }

    /// Read the alias target. Caller must have verified the redirect is
    /// `Varalias`.
    #[inline]
    pub fn alias_target(&self) -> SymId {
        debug_assert_eq!(self.redirect(), SymbolRedirect::Varalias);
        unsafe { self.val.alias }
    }

    /// Switch this symbol to `Varalias` and store the target id.
    #[inline]
    pub fn set_alias_target(&mut self, target: SymId) {
        self.flags.set_redirect(SymbolRedirect::Varalias);
        self.val = SymbolVal { alias: target };
    }
}

/// The obarray — a table of interned symbols.
///
/// This is the central symbol registry. `intern` looks up or creates symbols,
/// ensuring that `(eq 'foo 'foo)` is always true.
#[derive(Clone, Debug)]
pub struct Obarray {
    symbols: Vec<Option<SymbolData>>,
    global_member_count: usize,
    function_epoch: u64,
}

impl Default for Obarray {
    fn default() -> Self {
        Self::new()
    }
}

impl Obarray {
    fn is_canonical_symbol_id(id: SymId) -> bool {
        is_canonical_id(id)
    }

    fn slot_index(id: SymId) -> usize {
        id.0 as usize
    }

    fn slot(&self, id: SymId) -> Option<&SymbolData> {
        self.symbols
            .get(Self::slot_index(id))
            .and_then(Option::as_ref)
    }

    fn slot_mut(&mut self, id: SymId) -> Option<&mut SymbolData> {
        self.symbols
            .get_mut(Self::slot_index(id))
            .and_then(Option::as_mut)
    }

    fn ensure_slot(&mut self, id: SymId) -> &mut SymbolData {
        let idx = Self::slot_index(id);
        if self.symbols.len() <= idx {
            self.symbols.resize_with(idx + 1, || None);
        }
        self.symbols[idx].get_or_insert_with(|| SymbolData::new(id))
    }

    fn mark_global_member(&mut self, id: SymId) {
        let added = {
            let sym = self.ensure_slot(id);
            if sym.interned_global {
                return;
            }
            sym.interned_global = true;
            let name = resolve_sym(id);
            if name.starts_with(':') {
                // Match GNU lread.c intern_sym: keywords interned in the
                // initial obarray are self-evaluating constants and are marked
                // declared-special.
                sym.special = true;
                sym.constant = true;
                if matches!(sym.value, SymbolValue::Plain(None)) {
                    let kw = Value::keyword_id(id);
                    sym.value = SymbolValue::Plain(Some(kw));
                    sym.flags.set_redirect(SymbolRedirect::Plainval);
                    sym.val = SymbolVal { plain: kw };
                }
            }
            true
        };
        if added {
            self.global_member_count += 1;
        }
    }

    fn clear_global_member(&mut self, id: SymId) -> bool {
        let Some(sym) = self.slot_mut(id) else {
            return false;
        };
        if !sym.interned_global {
            return false;
        }
        sym.interned_global = false;
        self.global_member_count = self.global_member_count.saturating_sub(1);
        true
    }

    fn ensure_global_member_if_canonical(&mut self, id: SymId) {
        if Self::is_canonical_symbol_id(id) {
            self.mark_global_member(id);
        }
    }

    fn is_global_member(&self, id: SymId) -> bool {
        self.slot(id).is_some_and(|sym| sym.interned_global)
    }

    fn value_from_symbol_id(&self, id: SymId) -> Value {
        let name = resolve_sym(id);
        if self.is_global_member(id) {
            if name == "nil" {
                return Value::NIL;
            }
            if name == "t" {
                return Value::T;
            }
            if name.starts_with(':') {
                return Value::keyword_id(id);
            }
        }
        Value::symbol(id)
    }

    pub fn new() -> Self {
        let mut ob = Self {
            symbols: Vec::new(),
            global_member_count: 0,
            function_epoch: 0,
        };

        // Pre-intern fundamental symbols. Both `t` and `nil` are
        // self-referential constants in GNU.
        let t_id = intern("t");
        {
            let t_sym = ob.ensure_slot(t_id);
            t_sym.value = SymbolValue::Plain(Some(Value::T));
            t_sym.flags.set_redirect(SymbolRedirect::Plainval);
            t_sym.val = SymbolVal { plain: Value::T };
            t_sym.constant = true;
            t_sym.special = true;
        }
        ob.mark_global_member(t_id);

        let nil_id = intern("nil");
        {
            let nil_sym = ob.ensure_slot(nil_id);
            nil_sym.value = SymbolValue::Plain(Some(Value::NIL));
            nil_sym.flags.set_redirect(SymbolRedirect::Plainval);
            nil_sym.val = SymbolVal { plain: Value::NIL };
            nil_sym.constant = true;
            nil_sym.special = true;
        }
        ob.mark_global_member(nil_id);

        ob
    }

    /// Intern a symbol: look up by name, creating if absent.
    /// Returns the symbol name (which is the key for identity).
    pub fn intern(&mut self, name: &str) -> String {
        let id = intern(name);
        self.ensure_symbol_id(id);
        self.mark_global_member(id);
        name.to_string()
    }

    /// Materialize a canonical symbol in the global obarray.
    ///
    /// GNU does this as part of interning into the initial obarray. Neomacs
    /// keeps string interning separate from obarray storage, so runtime paths
    /// that operate on canonical symbols can explicitly request the same
    /// initial-obarray semantics here.
    pub fn ensure_interned_global_id(&mut self, id: SymId) {
        self.ensure_global_member_if_canonical(id);
    }

    /// Look up a symbol without creating it. Returns None if not interned.
    pub fn intern_soft(&self, name: &str) -> Option<&SymbolData> {
        let id = lookup_interned(name)?;
        self.slot(id).filter(|sym| sym.interned_global)
    }

    /// Get symbol data (mutable). Interns the symbol if needed.
    pub fn get_or_intern(&mut self, name: &str) -> &mut SymbolData {
        let id = intern(name);
        self.mark_global_member(id);
        self.ensure_symbol_id(id)
    }

    /// Get symbol data (immutable).
    pub fn get(&self, name: &str) -> Option<&SymbolData> {
        let id = lookup_interned(name)?;
        self.slot(id).filter(|sym| sym.interned_global)
    }

    /// Get symbol data (mutable).
    pub fn get_mut(&mut self, name: &str) -> Option<&mut SymbolData> {
        let id = lookup_interned(name)?;
        self.slot_mut(id).filter(|sym| sym.interned_global)
    }

    /// Ensure symbol storage exists for an arbitrary symbol id.
    pub fn ensure_symbol_id(&mut self, id: SymId) -> &mut SymbolData {
        self.ensure_slot(id)
    }

    /// Get symbol data by identity.
    pub fn get_by_id(&self, id: SymId) -> Option<&SymbolData> {
        self.slot(id)
    }

    /// Get mutable symbol data by identity.
    pub fn get_mut_by_id(&mut self, id: SymId) -> Option<&mut SymbolData> {
        self.slot_mut(id)
    }

    /// Get the value cell of a symbol.
    pub fn symbol_value(&self, name: &str) -> Option<&Value> {
        self.symbol_value_id(intern(name))
    }

    /// Get the value cell of a symbol by identity.
    /// Follows `Alias` chains (with cycle detection, max 50 hops).
    pub fn symbol_value_id(&self, id: SymId) -> Option<&Value> {
        let mut current = id;
        for _ in 0..50 {
            match self.slot(current)?.value {
                SymbolValue::Plain(ref v) => return v.as_ref(),
                SymbolValue::Alias(target) => current = target,
                SymbolValue::BufferLocal { ref default, .. } => return default.as_ref(),
                SymbolValue::Forwarded => return None,
            }
        }
        None // alias cycle — give up
    }

    /// Set the value cell of a symbol. Interns if needed.
    pub fn set_symbol_value(&mut self, name: &str, value: Value) {
        let id = intern(name);
        self.mark_global_member(id);
        self.set_symbol_value_id_inner(id, value);
    }

    /// Set the value cell of a symbol by identity.
    pub fn set_symbol_value_id(&mut self, id: SymId, value: Value) {
        self.ensure_global_member_if_canonical(id);
        self.set_symbol_value_id_inner(id, value);
    }

    /// Inner helper: follow aliases and write the value at the resolved target.
    ///
    /// Phase 1: keeps `value: SymbolValue` and `flags + val` in sync. The
    /// new redirect machinery is the eventual source of truth (Phase 4-10);
    /// for now both representations carry the same data.
    fn set_symbol_value_id_inner(&mut self, id: SymId, value: Value) {
        let target = self.resolve_alias_for_write(id);
        let sym = self.ensure_symbol_id(target);
        match sym.value {
            SymbolValue::Plain(_) => {
                sym.value = SymbolValue::Plain(Some(value));
                sym.flags.set_redirect(SymbolRedirect::Plainval);
                sym.val = SymbolVal { plain: value };
            }
            SymbolValue::BufferLocal {
                ref mut default, ..
            } => {
                *default = Some(value);
                // The new shape doesn't yet model BufferLocal — Phase 4
                // introduces the BLV. For now keep the redirect on
                // Plainval pointing at the default.
                sym.flags.set_redirect(SymbolRedirect::Plainval);
                sym.val = SymbolVal { plain: value };
            }
            SymbolValue::Forwarded => { /* no-op placeholder */ }
            SymbolValue::Alias(_) => {
                // resolve_alias_for_write should have resolved this, but
                // as a safety fallback write as Plain.
                sym.value = SymbolValue::Plain(Some(value));
                sym.flags.set_redirect(SymbolRedirect::Plainval);
                sym.val = SymbolVal { plain: value };
            }
        }
    }

    /// Visit each stored symbol value cell that currently holds a `Value`.
    pub fn for_each_value_cell_mut(&mut self, mut f: impl FnMut(&mut Value)) {
        for sym in self.symbols.iter_mut().flatten() {
            match &mut sym.value {
                SymbolValue::Plain(Some(value)) => f(value),
                SymbolValue::BufferLocal {
                    default: Some(value),
                    ..
                } => f(value),
                SymbolValue::Plain(None)
                | SymbolValue::BufferLocal { default: None, .. }
                | SymbolValue::Alias(_)
                | SymbolValue::Forwarded => {}
            }
        }
    }

    /// Follow alias chain for a mutable write, returning the resolved SymId.
    /// Max 50 hops to prevent infinite loops.
    fn resolve_alias_for_write(&mut self, id: SymId) -> SymId {
        let mut current = id;
        for _ in 0..50 {
            match self.slot(current) {
                Some(s) => match s.value {
                    SymbolValue::Alias(target) => current = target,
                    _ => return current,
                },
                None => return current,
            }
        }
        current // cycle — write to the last hop
    }

    /// Get the function cell of a symbol.
    pub fn symbol_function(&self, name: &str) -> Option<&Value> {
        self.symbol_function_id(intern(name))
    }

    /// Get the function cell of a symbol by identity.
    pub fn symbol_function_id(&self, id: SymId) -> Option<&Value> {
        let sym = self.slot(id)?;
        if sym.function_unbound {
            return None;
        }
        sym.function.as_ref()
    }

    /// Get the function cell of a symbol from its Value representation.
    /// Uses the SymId directly, which works correctly for both interned
    /// and uninterned symbols (unlike `symbol_function(name)` which
    /// re-interns the name and would miss uninterned symbol function cells).
    pub fn symbol_function_of_value(&self, value: &Value) -> Option<&Value> {
        match value.kind() {
            ValueKind::Symbol(id) => self.symbol_function_id(id),
            ValueKind::Nil => self.symbol_function("nil"),
            ValueKind::T => self.symbol_function("t"),
            _ => None,
        }
    }

    /// Set the function cell of a symbol (fset). Interns if needed.
    pub fn set_symbol_function(&mut self, name: &str, function: Value) {
        let id = intern(name);
        self.mark_global_member(id);
        let sym = self.ensure_symbol_id(id);
        sym.function = Some(function);
        sym.function_unbound = false;
        self.function_epoch = self.function_epoch.wrapping_add(1);
    }

    /// Set the function cell of a symbol by identity.
    pub fn set_symbol_function_id(&mut self, id: SymId, function: Value) {
        self.ensure_global_member_if_canonical(id);
        let sym = self.ensure_symbol_id(id);
        sym.function = Some(function);
        sym.function_unbound = false;
        self.function_epoch = self.function_epoch.wrapping_add(1);
    }

    /// Remove the function cell (fmakunbound).
    pub fn fmakunbound(&mut self, name: &str) {
        self.fmakunbound_id(intern(name));
    }

    /// Remove the function cell by identity.
    pub fn fmakunbound_id(&mut self, id: SymId) {
        self.ensure_global_member_if_canonical(id);
        let sym = self.ensure_symbol_id(id);
        let mut changed = !sym.function_unbound;
        sym.function_unbound = true;
        changed |= sym.function.take().is_some();
        if changed {
            self.function_epoch = self.function_epoch.wrapping_add(1);
        }
    }

    /// Remove function cell without marking as explicitly unbound.
    /// Used for init-time masking of lazily-materialized builtins.
    pub fn clear_function_silent(&mut self, name: &str) {
        self.clear_function_silent_id(intern(name));
    }

    /// Remove function cell without marking as explicitly unbound, by identity.
    pub fn clear_function_silent_id(&mut self, id: SymId) {
        if let Some(sym) = self.slot_mut(id) {
            if sym.function.take().is_some() {
                self.function_epoch = self.function_epoch.wrapping_add(1);
            }
        }
    }

    /// Remove the value cell (makunbound).
    pub fn makunbound(&mut self, name: &str) {
        self.makunbound_id(intern(name));
    }

    /// Remove the value cell by identity.
    /// Follows alias chains (max 50 hops).
    pub fn makunbound_id(&mut self, id: SymId) {
        self.ensure_global_member_if_canonical(id);
        let target = self.resolve_alias_for_write(id);
        if let Some(sym) = self.slot_mut(target) {
            if !sym.constant {
                match sym.value {
                    SymbolValue::Plain(_) => sym.value = SymbolValue::Plain(None),
                    SymbolValue::BufferLocal {
                        ref mut default, ..
                    } => *default = None,
                    SymbolValue::Forwarded => { /* no-op */ }
                    SymbolValue::Alias(_) => sym.value = SymbolValue::Plain(None),
                }
                // Mirror into the new shape: Plainval / NIL is the
                // "no value" state until we have a proper UNBOUND
                // sentinel (planned for Phase 4 once SymbolValue is
                // gone).
                sym.flags.set_redirect(SymbolRedirect::Plainval);
                sym.val = SymbolVal { plain: Value::NIL };
            }
        }
    }

    /// Check if a symbol is bound (has a value cell).
    pub fn boundp(&self, name: &str) -> bool {
        self.boundp_id(intern(name))
    }

    /// Check if a symbol is bound by identity.
    /// Follows alias chains (max 50 hops).
    pub fn boundp_id(&self, id: SymId) -> bool {
        let mut current = id;
        for _ in 0..50 {
            match self.slot(current) {
                Some(s) => match &s.value {
                    SymbolValue::Plain(v) => return v.is_some(),
                    SymbolValue::Alias(target) => current = *target,
                    SymbolValue::BufferLocal { default, .. } => return default.is_some(),
                    SymbolValue::Forwarded => return false,
                },
                None => return false,
            }
        }
        false // cycle
    }

    /// Check if a symbol has a function cell.
    pub fn fboundp(&self, name: &str) -> bool {
        self.fboundp_id(intern(name))
    }

    /// Check if a symbol has a function cell by identity.
    pub fn fboundp_id(&self, id: SymId) -> bool {
        self.slot(id)
            .filter(|sym| !sym.function_unbound)
            .and_then(|s| s.function.as_ref())
            .is_some_and(|f| !f.is_nil())
    }

    /// Get a property from the symbol's plist.
    pub fn get_property(&self, name: &str, prop: &str) -> Option<&Value> {
        self.get_property_id(intern(name), intern(prop))
    }

    /// Get a property from the symbol's plist by identity.
    pub fn get_property_id(&self, symbol: SymId, prop: SymId) -> Option<&Value> {
        self.slot(symbol).and_then(|s| s.plist.get(&prop))
    }

    /// Set a property on the symbol's plist.
    pub fn put_property(&mut self, name: &str, prop: &str, value: Value) {
        let symbol = intern(name);
        self.mark_global_member(symbol);
        let sym = self.ensure_symbol_id(symbol);
        sym.plist.insert(intern(prop), value);
    }

    /// Set a property on the symbol's plist by identity.
    pub fn put_property_id(&mut self, symbol: SymId, prop: SymId, value: Value) {
        self.ensure_global_member_if_canonical(symbol);
        let sym = self.ensure_symbol_id(symbol);
        sym.plist.insert(prop, value);
    }

    /// Replace the complete plist for a symbol by identity.
    pub fn replace_symbol_plist_id<I>(&mut self, symbol: SymId, entries: I)
    where
        I: IntoIterator<Item = (SymId, Value)>,
    {
        self.ensure_global_member_if_canonical(symbol);
        let sym = self.ensure_symbol_id(symbol);
        sym.plist.clear();
        sym.plist.extend(entries);
    }

    /// Get the symbol's full plist as a flat list.
    pub fn symbol_plist(&self, name: &str) -> Value {
        self.symbol_plist_id(intern(name))
    }

    /// Get the symbol's full plist as a flat list by identity.
    pub fn symbol_plist_id(&self, id: SymId) -> Value {
        match self.slot(id) {
            Some(sym) if !sym.plist.is_empty() => {
                let mut items = Vec::new();
                for (k, v) in &sym.plist {
                    items.push(self.value_from_symbol_id(*k));
                    items.push(*v);
                }
                Value::list(items)
            }
            _ => Value::NIL,
        }
    }

    /// Mark a symbol as special (dynamically bound).
    pub fn make_special(&mut self, name: &str) {
        let id = intern(name);
        self.mark_global_member(id);
        self.ensure_symbol_id(id).special = true;
    }

    /// Mark a symbol as special by identity.
    pub fn make_special_id(&mut self, id: SymId) {
        self.ensure_global_member_if_canonical(id);
        self.ensure_symbol_id(id).special = true;
    }

    /// Clear the special flag on a symbol.
    pub fn make_non_special(&mut self, name: &str) {
        let id = intern(name);
        self.mark_global_member(id);
        self.ensure_symbol_id(id).special = false;
    }

    /// Clear the special flag on a symbol by identity.
    pub fn make_non_special_id(&mut self, id: SymId) {
        self.ensure_global_member_if_canonical(id);
        self.ensure_symbol_id(id).special = false;
    }

    /// Check if a symbol is special.
    pub fn is_special(&self, name: &str) -> bool {
        self.is_special_id(intern(name))
    }

    /// Check if a symbol is special by identity.
    pub fn is_special_id(&self, id: SymId) -> bool {
        self.slot(id).is_some_and(|s| s.special)
    }

    /// Check if a symbol is a constant.
    pub fn is_constant(&self, name: &str) -> bool {
        self.is_constant_id(intern(name))
    }

    /// Check if a symbol is a constant by identity.
    pub fn is_constant_id(&self, id: SymId) -> bool {
        (Self::is_canonical_symbol_id(id) && resolve_sym(id).starts_with(':'))
            || self.slot(id).is_some_and(|s| s.constant)
    }

    /// Mark a symbol as a hard constant (like SYMBOL_NOWRITE in GNU Emacs).
    pub fn set_constant(&mut self, name: &str) {
        let id = intern(name);
        self.set_constant_id(id);
    }

    /// Mark a symbol as a hard constant (like SYMBOL_NOWRITE in GNU Emacs) by identity.
    pub fn set_constant_id(&mut self, id: SymId) {
        self.ensure_global_member_if_canonical(id);
        self.ensure_symbol_id(id).constant = true;
    }

    // ------------------------------------------------------------------
    // SymbolValue-aware helpers (buffer-local / alias introspection)
    // ------------------------------------------------------------------

    /// Mark a symbol as a buffer-local variable in the obarray.
    /// Preserves any existing default value from `Plain` or `BufferLocal`.
    ///
    /// Phase 1: this still sets the legacy `SymbolValue::BufferLocal`
    /// marker. The new redirect machinery does not yet route through
    /// `Localized`; Phase 4 wires the BLV cache and Phase 6 cuts
    /// `make-local-variable` / `make-variable-buffer-local` over to it.
    pub fn make_buffer_local(&mut self, name: &str, local_if_set: bool) {
        let id = intern(name);
        self.mark_global_member(id);
        let sym = self.ensure_symbol_id(id);
        let old_default = match &sym.value {
            SymbolValue::Plain(v) => v.clone(),
            SymbolValue::BufferLocal { default, .. } => default.clone(),
            _ => None,
        };
        sym.value = SymbolValue::BufferLocal {
            default: old_default.clone(),
            local_if_set,
        };
        // Mirror into the new shape: Phase 1 still uses Plainval; the
        // default lives in `val.plain`.
        sym.flags.set_redirect(SymbolRedirect::Plainval);
        sym.val = SymbolVal {
            plain: old_default.unwrap_or(Value::NIL),
        };
    }

    /// Install a variable-alias edge: reading/writing `id` will redirect to `target`.
    ///
    /// Phase 1: maintains both the legacy enum and the new redirect tag.
    /// Phase 3 cuts callers over to the redirect-only path.
    pub fn make_alias(&mut self, id: SymId, target: SymId) {
        let sym = self.ensure_symbol_id(id);
        sym.value = SymbolValue::Alias(target);
        sym.set_alias_target(target);
    }

    /// Check whether a symbol is a buffer-local variable in the obarray.
    pub fn is_buffer_local(&self, name: &str) -> bool {
        self.is_buffer_local_id(intern(name))
    }

    /// Check whether a symbol is a buffer-local variable by identity.
    pub fn is_buffer_local_id(&self, id: SymId) -> bool {
        self.slot(id)
            .is_some_and(|s| matches!(s.value, SymbolValue::BufferLocal { .. }))
    }

    /// Check whether a symbol is an alias by identity.
    pub fn is_alias_id(&self, id: SymId) -> bool {
        self.slot(id)
            .is_some_and(|s| matches!(s.value, SymbolValue::Alias(_)))
    }

    /// Get the default value of a symbol, following aliases.
    /// For `Plain` and `BufferLocal` this is the direct/default value;
    /// for `Alias` it follows the chain; for `Forwarded` it returns `None`.
    pub fn default_value_id(&self, id: SymId) -> Option<&Value> {
        let mut current = id;
        for _ in 0..50 {
            match self.slot(current)?.value {
                SymbolValue::Plain(ref v) => return v.as_ref(),
                SymbolValue::BufferLocal { ref default, .. } => return default.as_ref(),
                SymbolValue::Alias(target) => current = target,
                SymbolValue::Forwarded => return None,
            }
        }
        None
    }

    /// Follow function indirection (defalias chains).
    /// Returns the final function value, following symbol aliases.
    pub fn indirect_function(&self, name: &str) -> Option<Value> {
        self.indirect_function_id(intern(name))
    }

    /// Follow function indirection (defalias chains) by canonical symbol id.
    /// Returns the final function value, following symbol aliases.
    pub fn indirect_function_id(&self, id: SymId) -> Option<Value> {
        let mut current_id = id;
        let mut depth = 0;
        loop {
            if depth > 100 {
                return None; // Circular alias chain
            }
            let func = self.slot(current_id)?.function.as_ref()?;
            match func.kind() {
                ValueKind::Symbol(id) => {
                    current_id = id;
                    depth += 1;
                }
                _ => return Some(*func),
            }
        }
    }

    /// Number of interned symbols.
    pub fn len(&self) -> usize {
        self.global_member_count
    }

    pub fn is_empty(&self) -> bool {
        self.global_member_count == 0
    }

    /// All interned symbol names.
    pub fn all_symbols(&self) -> Vec<&str> {
        self.symbols
            .iter()
            .flatten()
            .filter(|sym| sym.interned_global)
            .map(|sym| resolve_sym(sym.name))
            .collect()
    }

    /// Remove a symbol from the obarray.  Returns `true` if it was present.
    pub fn unintern(&mut self, name: &str) -> bool {
        let id = intern(name);
        let removed_symbol = self.clear_global_member(id);
        if removed_symbol {
            self.function_epoch = self.function_epoch.wrapping_add(1);
        }
        removed_symbol
    }

    /// Monotonic epoch for function-cell mutations.
    pub fn function_epoch(&self) -> u64 {
        self.function_epoch
    }

    /// True when `fmakunbound` explicitly masked this symbol's fallback function definition.
    pub fn is_function_unbound(&self, name: &str) -> bool {
        self.is_function_unbound_id(intern(name))
    }

    /// True when `fmakunbound` explicitly masked this symbol's fallback function definition.
    pub fn is_function_unbound_id(&self, id: SymId) -> bool {
        self.slot(id).is_some_and(|sym| sym.function_unbound)
    }

    // -----------------------------------------------------------------------
    // pdump accessors
    // -----------------------------------------------------------------------

    /// Iterate over all (SymId, &SymbolData) pairs (for pdump serialization).
    pub(crate) fn iter_symbols(&self) -> impl Iterator<Item = (SymId, &SymbolData)> {
        self.symbols.iter().enumerate().filter_map(|(idx, slot)| {
            debug_assert!(idx <= u32::MAX as usize, "symbol index overflow");
            slot.as_ref().map(|sym| (SymId(idx as u32), sym))
        })
    }

    /// Iterate over ids interned in the global obarray.
    pub(crate) fn global_member_ids(&self) -> impl Iterator<Item = SymId> + '_ {
        self.iter_symbols()
            .filter(|(_, sym)| sym.interned_global)
            .map(|(id, _)| id)
    }

    /// Iterate over fmakunbound'd symbol ids (for pdump serialization).
    pub(crate) fn function_unbound_ids(&self) -> impl Iterator<Item = SymId> + '_ {
        self.iter_symbols()
            .filter(|(_, sym)| sym.function_unbound)
            .map(|(id, _)| id)
    }

    /// Reconstruct an Obarray from pdump data.
    pub(crate) fn from_dump(
        symbols: Vec<(SymId, SymbolData)>,
        global_members: Vec<SymId>,
        function_unbound: Vec<SymId>,
        function_epoch: u64,
    ) -> Self {
        let mut ob = Self {
            symbols: Vec::new(),
            global_member_count: 0,
            function_epoch,
        };
        for (id, mut sym) in symbols {
            sym.interned_global = false;
            sym.function_unbound = false;
            *ob.ensure_slot(id) = sym;
        }
        for id in global_members {
            ob.mark_global_member(id);
        }
        for id in function_unbound {
            ob.ensure_slot(id).function_unbound = true;
        }
        ob
    }
}

impl GcTrace for Obarray {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        for sym in self.symbols.iter().flatten() {
            match &sym.value {
                SymbolValue::Plain(Some(v)) => roots.push(*v),
                SymbolValue::BufferLocal {
                    default: Some(v), ..
                } => roots.push(*v),
                SymbolValue::Plain(None)
                | SymbolValue::BufferLocal { default: None, .. }
                | SymbolValue::Alias(_)
                | SymbolValue::Forwarded => {}
            }
            if let Some(ref f) = sym.function {
                roots.push(*f);
            }
            for pval in sym.plist.values() {
                roots.push(*pval);
            }
        }
    }
}
#[cfg(test)]
#[path = "symbol_test.rs"]
mod tests;
