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

/// Mirrors GNU `swap_in_symval_forwarding` (`src/data.c:1539-1571`).
///
/// Loads the BLV's `valcell` from the current buffer's
/// `local_var_alist` if `where_buf` doesn't already match. The Phase 4
/// shape doesn't yet support `Lisp_*Fwd` predicates or the
/// `local-flags` buffer slot — those land in Phase 8.
///
/// `current_buffer` is the buffer we're switching the cache to (a
/// `Value::buffer` or `Value::NIL` for the global default).
/// `local_var_alist` is `current_buffer`'s alist of `(sym . val)`
/// per-buffer bindings.
fn swap_in_blv(
    obarray: &mut Obarray,
    sym_id: SymId,
    current_buffer: Value,
    local_var_alist: Value,
) {
    let Some(blv) = obarray.blv_mut(sym_id) else {
        return;
    };
    if blv.where_buf == current_buffer {
        return; // cache already loaded for this buffer
    }
    // Find this symbol in the new buffer's alist.
    let key = Value::from_sym_id(sym_id);
    let found_cell = assq(key, local_var_alist);
    blv.where_buf = current_buffer;
    blv.found = !found_cell.is_nil();
    blv.valcell = if blv.found { found_cell } else { blv.defcell };
}

/// Walk an alist looking for the cons whose car is `eq` to `key`.
/// Returns the matching cons or `Value::NIL`. Mirrors GNU `Fassq`.
///
/// Free function rather than a method on `Value` because Phase 4 needs
/// it locally and we don't want to grow the public Value API for an
/// internal helper.
fn assq(key: Value, mut alist: Value) -> Value {
    while alist.is_cons() {
        let entry = alist.cons_car();
        if entry.is_cons() && super::value::eq_value(&entry.cons_car(), &key) {
            return entry;
        }
        alist = alist.cons_cdr();
    }
    Value::NIL
}

/// `bindflag` argument for [`Obarray::set_internal_localized`].
/// Mirrors GNU `enum Set_Internal_Bind` (`src/lisp.h:3590-3596`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SetInternalBind {
    /// Ordinary `(setq foo bar)`. Auto-creates a per-buffer binding
    /// when `local_if_set` is true.
    Set,
    /// `let`-binding initial assignment. Never auto-creates a new
    /// per-buffer binding (the existing one or the default is
    /// stashed in specpdl for unwind).
    Bind,
    /// `let`-binding unwind. Restores the previous value.
    Unbind,
}

/// Stub for GNU `let_shadows_buffer_binding_p`
/// (`src/eval.c:3559-3577`). Returns `true` if the symbol is
/// currently `let`-bound to a buffer-local binding shadowing the
/// per-buffer slot.
///
/// Phase 5 stub: always `false`. Phase 7 wires this against the
/// specpdl `LET_LOCAL` records.
pub fn let_shadows_buffer_binding_p(_sym_id: SymId) -> bool {
    false
}

/// Reasons [`Obarray::make_variable_alias`] can fail. Mirrors the
/// `xsignal` callsites in GNU `Fdefvaralias` (`src/eval.c:631-726`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MakeAliasError {
    /// `new_alias` is a constant — cannot be redirected.
    Constant,
    /// `new_alias` is currently `SYMBOL_FORWARDED` (a built-in C
    /// variable). GNU rejects with "Cannot make a built-in variable
    /// an alias".
    Forwarded,
    /// `new_alias` is currently `SYMBOL_LOCALIZED` (a buffer-local).
    /// GNU rejects with "Don't know how to make a buffer-local
    /// variable an alias".
    Localized,
    /// Following `base`'s alias chain reaches `new_alias` — would
    /// create `cyclic-variable-indirection`.
    Cycle,
}

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
///
/// Phase 4 of the symbol-redirect refactor adds a heap-allocated BLV
/// pool ([`Obarray::blvs`]) for `LOCALIZED` symbols. The Obarray owns
/// every BLV; symbols' [`SymbolVal::blv`] field stores a raw pointer
/// into the pool. The custom [`Clone`] impl deep-copies BLVs and
/// remaps the pointers in the cloned symbols, so `Obarray::clone()`
/// stays semantically a deep copy. The custom [`Drop`] impl frees the
/// heap allocations.
pub struct Obarray {
    symbols: Vec<Option<SymbolData>>,
    global_member_count: usize,
    function_epoch: u64,
    /// Heap-allocated BLVs for `SYMBOL_LOCALIZED` symbols. Each entry
    /// is a `Box::into_raw` pointer; freed in [`Obarray::drop`]. The
    /// pool is append-only — we never reuse a slot.
    blvs: Vec<*mut LispBufferLocalValue>,
}

impl std::fmt::Debug for Obarray {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Obarray")
            .field("global_member_count", &self.global_member_count)
            .field("function_epoch", &self.function_epoch)
            .field("blvs", &self.blvs.len())
            .finish_non_exhaustive()
    }
}

impl Drop for Obarray {
    fn drop(&mut self) {
        for ptr in self.blvs.drain(..) {
            // Safety: we created each pointer via `Box::into_raw` in
            // `make_symbol_localized` and never alias it elsewhere
            // (the only other reference lives inside a `LispSymbol`'s
            // `val.blv` field, which goes away with `self`).
            unsafe { drop(Box::from_raw(ptr)) };
        }
    }
}

impl Clone for Obarray {
    fn clone(&self) -> Self {
        // Deep-copy the BLV pool. Build a `old → new` map so we can
        // remap each LOCALIZED symbol's `val.blv` to its clone.
        let mut blvs: Vec<*mut LispBufferLocalValue> = Vec::with_capacity(self.blvs.len());
        let mut blv_map: rustc_hash::FxHashMap<usize, *mut LispBufferLocalValue> =
            rustc_hash::FxHashMap::default();
        for &orig in &self.blvs {
            // Safety: each entry was Box::into_raw'd by us and is
            // alive for the duration of `&self`.
            let cloned_box = Box::new(unsafe { (*orig).clone() });
            let cloned_ptr = Box::into_raw(cloned_box);
            blvs.push(cloned_ptr);
            blv_map.insert(orig as usize, cloned_ptr);
        }
        let mut symbols = self.symbols.clone();
        for slot in symbols.iter_mut().flatten() {
            if slot.flags.redirect() == SymbolRedirect::Localized {
                let orig = unsafe { slot.val.blv };
                if let Some(&new_ptr) = blv_map.get(&(orig as usize)) {
                    slot.val = SymbolVal { blv: new_ptr };
                }
            }
        }
        Self {
            symbols,
            global_member_count: self.global_member_count,
            function_epoch: self.function_epoch,
            blvs,
        }
    }
}

// Safety: Obarray contains raw pointers to its own heap allocations.
// They're owned by the obarray, so sending the obarray across threads
// (via Send) or sharing it via &Obarray (via Sync) is safe — the
// pointers don't escape and don't carry interior mutability.
unsafe impl Send for Obarray {}
unsafe impl Sync for Obarray {}

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
            blvs: Vec::new(),
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

    /// Allocate a fresh `LispBufferLocalValue` for `id`, flip the
    /// symbol's redirect to `Localized`, and store the BLV pointer in
    /// `val.blv`. Mirrors GNU `make_blv` (`src/data.c:2112-2140`).
    ///
    /// `default` becomes the cdr of `defcell` and `valcell` (initially
    /// the same cons, mirroring GNU's "valcell == defcell when no
    /// per-buffer binding loaded" invariant).
    ///
    /// If the symbol is already LOCALIZED, this is a no-op (returns
    /// the existing BLV pointer).
    pub fn make_symbol_localized(&mut self, id: SymId, default: Value) -> *mut LispBufferLocalValue {
        let target = self.resolve_alias_for_write(id);
        // Check existing state before mutating.
        if let Some(existing) = self.slot(target) {
            if existing.flags.redirect() == SymbolRedirect::Localized {
                return unsafe { existing.val.blv };
            }
        }
        // Build defcell = (sym . default). The same cons doubles as
        // valcell until per-buffer bindings are swapped in.
        let defcell = Value::cons(Value::from_sym_id(target), default);
        let blv = Box::new(LispBufferLocalValue {
            local_if_set: false,
            found: false,
            fwd: None,
            where_buf: Value::NIL,
            defcell,
            valcell: defcell,
        });
        let raw = Box::into_raw(blv);
        self.blvs.push(raw);
        let sym = self.ensure_symbol_id(target);
        sym.flags.set_redirect(SymbolRedirect::Localized);
        sym.val = SymbolVal { blv: raw };
        // Phase 4 keeps the legacy enum mirror in sync until Phase 10.
        sym.value = SymbolValue::BufferLocal {
            default: Some(default),
            local_if_set: false,
        };
        raw
    }

    /// Set the `local_if_set` flag on a LOCALIZED symbol's BLV. Used
    /// by `make-variable-buffer-local` (Phase 6) which differs from
    /// `make-local-variable` only in this flag. Phase 4 exposes the
    /// helper so the LOCALIZED tests can flip it directly.
    pub fn set_blv_local_if_set(&mut self, id: SymId, local_if_set: bool) {
        let target = self.resolve_alias_for_write(id);
        if let Some(sym) = self.slot(target) {
            if sym.flags.redirect() == SymbolRedirect::Localized {
                let blv = unsafe { &mut *sym.val.blv };
                blv.local_if_set = local_if_set;
            }
        }
    }

    /// Read a LOCALIZED symbol's BLV (immutable borrow). Returns
    /// `None` if the symbol is not LOCALIZED.
    pub fn blv(&self, id: SymId) -> Option<&LispBufferLocalValue> {
        let sym = self.slot(id)?;
        if sym.flags.redirect() != SymbolRedirect::Localized {
            return None;
        }
        // Safety: the symbol's val.blv was allocated by
        // make_symbol_localized and is owned by self.blvs. The
        // pointer stays valid for &self's lifetime because Drop
        // can't run while we hold &self.
        Some(unsafe { &*sym.val.blv })
    }

    /// Mutable BLV access. Used by `set_internal` (Phase 5) and
    /// `swap_in_symval_forwarding` (Phase 4).
    pub fn blv_mut(&mut self, id: SymId) -> Option<&mut LispBufferLocalValue> {
        let sym = self.slot(id)?;
        if sym.flags.redirect() != SymbolRedirect::Localized {
            return None;
        }
        // Safety: same rationale as `blv`. The mutable borrow follows
        // from `&mut self`.
        Some(unsafe { &mut *sym.val.blv })
    }

    /// Read a symbol's value via the redirect dispatch. Mirrors GNU
    /// `find_symbol_value` (`src/data.c:1584-1609`).
    ///
    /// **Note:** this variant takes only the obarray and is correct
    /// for PLAINVAL / VARALIAS / FORWARDED cases. The LOCALIZED case
    /// returns the BLV's *defcell* default; per-buffer dispatch
    /// requires the buffer-aware [`Self::find_symbol_value_in_buffer`]
    /// variant.
    ///
    /// Returns `None` for unbound (`void-variable` callsite signals).
    pub fn find_symbol_value(&self, id: SymId) -> Option<Value> {
        let mut current = id;
        for _ in 0..50 {
            let sym = self.slot(current)?;
            match sym.flags.redirect() {
                SymbolRedirect::Plainval => {
                    // Phase 2: read through the legacy `value` field for
                    // the bound check. The new `val.plain` mirror agrees
                    // (every internal mutator keeps both in sync). Phase 4
                    // collapses to `val.plain != Value::UNBOUND`.
                    match sym.value {
                        SymbolValue::Plain(v) => return v,
                        SymbolValue::BufferLocal { default, .. } => return default,
                        SymbolValue::Alias(target) => {
                            current = target;
                            continue;
                        }
                        SymbolValue::Forwarded => return None,
                    }
                }
                SymbolRedirect::Varalias => {
                    // Phase 1 still keeps the legacy `value` field too,
                    // but we follow the redirect-side chain since it's
                    // the eventual source of truth.
                    current = unsafe { sym.val.alias };
                    continue;
                }
                SymbolRedirect::Localized => {
                    // Phase 4: read the BLV's *currently loaded*
                    // valcell. Without a buffer context to swap in,
                    // we return whatever the cache currently holds —
                    // the caller is `find_symbol_value_in_buffer` for
                    // buffer-aware reads. For the bare obarray API
                    // we expose the default.
                    let blv = unsafe { &*sym.val.blv };
                    let cdr = blv.valcell.cons_cdr();
                    return Some(cdr);
                }
                SymbolRedirect::Forwarded => {
                    // Phase 8 wires this. For now defer to legacy enum.
                    match sym.value {
                        SymbolValue::Plain(v) => return v,
                        SymbolValue::BufferLocal { default, .. } => return default,
                        SymbolValue::Forwarded => return None,
                        SymbolValue::Alias(target) => {
                            current = target;
                            continue;
                        }
                    }
                }
            }
        }
        None // alias cycle
    }

    /// Buffer-aware variant of [`Self::find_symbol_value`]. Mirrors
    /// GNU `find_symbol_value` + `swap_in_symval_forwarding`
    /// (`src/data.c:1584-1571`).
    ///
    /// For LOCALIZED symbols, swaps the BLV cache to point at
    /// `current_buffer`'s per-buffer binding (if any) before reading.
    /// For other variants, this is identical to [`Self::find_symbol_value`].
    pub fn find_symbol_value_in_buffer(
        &mut self,
        id: SymId,
        current_buffer_id: Option<crate::buffer::BufferId>,
        current_buffer_value: Value,
        local_var_alist: Value,
    ) -> Option<Value> {
        let mut current = id;
        for _ in 0..50 {
            // Phase 4: only the LOCALIZED arm needs &mut self for the
            // cache swap. Borrow-check it carefully so the rest of the
            // walk can stay on a shared reference.
            let redirect = self.slot(current)?.flags.redirect();
            match redirect {
                SymbolRedirect::Plainval | SymbolRedirect::Forwarded => {
                    return self.find_symbol_value(current);
                }
                SymbolRedirect::Varalias => {
                    let next = unsafe { self.slot(current)?.val.alias };
                    current = next;
                    continue;
                }
                SymbolRedirect::Localized => {
                    // Swap-in: if `where_buf` doesn't match the
                    // current buffer, scan the new buffer's
                    // local_var_alist for `(sym . val)` and update
                    // valcell. Mirrors GNU
                    // `swap_in_symval_forwarding`.
                    swap_in_blv(self, current, current_buffer_value, local_var_alist);
                    let blv = self.blv(current)?;
                    return Some(blv.valcell.cons_cdr());
                }
            }
        }
        None
    }

    /// Write a symbol's value via the redirect dispatch. Mirrors GNU
    /// `set_internal` (`src/data.c:1644-1795`).
    ///
    /// Phase 2: thin wrapper over `set_symbol_value_id` that exposes
    /// the GNU name. Phase 5+ adds the LOCALIZED-aware logic and the
    /// `where`/`bindflag` parameters via [`Self::set_internal_localized`].
    pub fn set_internal(&mut self, id: SymId, value: Value) {
        self.set_symbol_value_id(id, value);
    }

    /// LOCALIZED arm of `set_internal`. Mirrors GNU
    /// `set_internal` lines 1687-1763 (`src/data.c`).
    ///
    /// Updates the BLV cache and (for `Set` writes) creates a new
    /// per-buffer binding when `local_if_set` is true and no current
    /// binding exists. Returns the (possibly new) `local_var_alist`
    /// for the target buffer; the caller is responsible for storing
    /// it back into the buffer.
    ///
    /// Parameters:
    /// - `sym_id`: the symbol being written.
    /// - `value`: the new value.
    /// - `target_buf`: the buffer the write is targeting (a
    ///   `Value::buffer` for explicit, or whatever the caller treats
    ///   as the "current" buffer Value). Used as the cache key.
    /// - `target_alist`: the target buffer's current
    ///   `local_var_alist`. May be updated.
    /// - `bindflag`: `Set` for ordinary `(setq)` writes, `Bind` for
    ///   `let` initial bindings (which never auto-create).
    /// - `let_shadows`: result of [`let_shadows_buffer_binding_p`]
    ///   for this symbol — Phase 7 wires this; Phase 5 callers pass
    ///   `false`.
    ///
    /// Returns the updated alist (consed if a new cell was created;
    /// unchanged otherwise).
    pub fn set_internal_localized(
        &mut self,
        sym_id: SymId,
        value: Value,
        target_buf: Value,
        target_alist: Value,
        bindflag: SetInternalBind,
        let_shadows: bool,
    ) -> Value {
        let mut new_alist = target_alist;
        let blv = match self.blv_mut(sym_id) {
            Some(blv) => blv,
            None => return new_alist,
        };

        // Step 1: swap-in. If `where_buf` doesn't match the target
        // buffer (or `valcell` is still pointing at the defcell),
        // reload the cache from the target buffer's alist.
        let need_swap =
            blv.where_buf != target_buf || super::value::eq_value(&blv.valcell, &blv.defcell);
        if need_swap {
            // GNU stores the previous binding's value back into the
            // *previous* valcell before swapping. The cons-cdr write
            // is implicit because we hold a reference into the
            // BLV-owned cells.
            let key = Value::from_sym_id(sym_id);
            let mut cell = assq(key, new_alist);
            blv.where_buf = target_buf;
            blv.found = true;

            if cell.is_nil() {
                // No existing binding for this buffer.
                let auto_create = bindflag == SetInternalBind::Set
                    && blv.local_if_set
                    && !let_shadows;
                if !auto_create {
                    // Fall through to writing the default.
                    blv.found = false;
                    cell = blv.defcell;
                } else {
                    // Cons up `(sym . current-default-cdr)` and
                    // prepend it to the buffer's local_var_alist.
                    let default_cdr = blv.defcell.cons_cdr();
                    cell = Value::cons(key, default_cdr);
                    new_alist = Value::cons(cell, new_alist);
                }
            }
            blv.valcell = cell;
        }

        // Step 2: actually write the new value into valcell's cdr.
        // The BLV's valcell is a shared cons whose cdr lives in the
        // tagged heap; mutate it via Value::set_cdr. Capture
        // valcell first so the BLV borrow ends before we touch the
        // cons cell.
        let valcell = blv.valcell;
        let _ = blv;
        valcell.set_cdr(value);

        new_alist
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

    /// Check whether a symbol is an alias by identity. Reads through the
    /// new redirect tag (Phase 3 of the symbol-redirect refactor).
    pub fn is_alias_id(&self, id: SymId) -> bool {
        self.slot(id)
            .is_some_and(|s| s.flags.redirect() == SymbolRedirect::Varalias)
    }

    /// Walk an alias chain to its terminus and return the resolved
    /// SymId. Mirrors GNU `indirect_variable` (`src/data.c:1284-1301`).
    /// Returns `None` if (and only if) a true cycle is detected via
    /// Floyd's tortoise/hare. Symbols that don't yet have a slot in
    /// the obarray are treated as "not an alias" and returned as-is —
    /// matching GNU's `XSYMBOL(sym)->u.s.redirect != SYMBOL_VARALIAS`
    /// fall-through path.
    pub fn indirect_variable_id(&self, id: SymId) -> Option<SymId> {
        let mut slow = id;
        let mut fast = id;
        loop {
            // Tortoise: advance one hop (or stop if not an alias).
            let Some(slow_sym) = self.slot(slow) else {
                return Some(slow); // no slot → not an alias
            };
            if slow_sym.flags.redirect() != SymbolRedirect::Varalias {
                return Some(slow);
            }
            slow = unsafe { slow_sym.val.alias };

            // Hare: advance two hops (or stop if not an alias).
            for _ in 0..2 {
                let Some(fast_sym) = self.slot(fast) else {
                    return Some(slow);
                };
                if fast_sym.flags.redirect() != SymbolRedirect::Varalias {
                    return Some(slow);
                }
                fast = unsafe { fast_sym.val.alias };
            }

            if slow == fast {
                return None; // cycle
            }
        }
    }

    /// Install a variable alias edge with full GNU semantics. Mirrors
    /// `Fdefvaralias` (`src/eval.c:631-726`):
    ///
    /// 1. `new_alias` must not be a constant.
    /// 2. `new_alias` must not currently be FORWARDED (a built-in C
    ///    variable).
    /// 3. `new_alias` must not currently be LOCALIZED (a buffer-local).
    /// 4. Walking from `base` along the alias chain must not pass through
    ///    `new_alias` (cycle detection).
    ///
    /// On success, flips `new_alias`'s redirect to `Varalias` pointing
    /// at `base` and marks both symbols `declared_special`. The legacy
    /// `value: SymbolValue::Alias` mirror stays in sync (deleted in
    /// Phase 10).
    ///
    /// Returns `Err(())` for cycle, constant, forwarded, or localized;
    /// the caller is responsible for translating into a Lisp signal.
    pub fn make_variable_alias(
        &mut self,
        new_alias: SymId,
        base: SymId,
    ) -> Result<(), MakeAliasError> {
        // Check current state of new_alias.
        if let Some(sym) = self.slot(new_alias) {
            if sym.constant {
                return Err(MakeAliasError::Constant);
            }
            match sym.flags.redirect() {
                SymbolRedirect::Forwarded => return Err(MakeAliasError::Forwarded),
                SymbolRedirect::Localized => return Err(MakeAliasError::Localized),
                _ => {}
            }
        }

        // Walk the base chain looking for new_alias.
        let mut current = base;
        loop {
            if current == new_alias {
                return Err(MakeAliasError::Cycle);
            }
            let Some(sym) = self.slot(current) else {
                break;
            };
            if sym.flags.redirect() != SymbolRedirect::Varalias {
                break;
            }
            current = unsafe { sym.val.alias };
        }

        // Install the alias edge. `make_alias` keeps both
        // representations in sync.
        self.make_alias(new_alias, base);
        self.make_special_id(new_alias);
        self.make_special_id(base);
        Ok(())
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
            blvs: Vec::new(),
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
