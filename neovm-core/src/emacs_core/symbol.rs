//! Obarray and symbol interning.
//!
//! In Emacs, symbols are unique objects stored in an "obarray" (hash table).
//! Each symbol has:
//! - A name (string)
//! - A value cell (variable binding)
//! - A function cell (function binding)
//! - A property list (plist)
//! - A `special` flag (for dynamic binding in lexical scope)

use super::intern::{SymId, intern, lookup_interned, resolve_sym};
use super::value::{Value, ValueKind};
use crate::gc::GcTrace;
use std::collections::{HashMap, HashSet};

/// Describes how a symbol's value cell is stored, matching GNU Emacs's
/// `symbol_redirect` enum (`SYMBOL_PLAINVAL`, `SYMBOL_VARALIAS`,
/// `SYMBOL_LOCALIZED`, `SYMBOL_FORWARDED`).
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

/// Per-symbol metadata stored in the obarray.
#[derive(Clone, Debug)]
pub struct SymbolData {
    /// The symbol's name.
    pub name: SymId,
    /// Value cell — see [`SymbolValue`] for the indirection variants.
    pub value: SymbolValue,
    /// Function cell (None = void-function).
    pub function: Option<Value>,
    /// Property list (flat alternating key-value pairs stored as HashMap).
    pub plist: HashMap<SymId, Value>,
    /// Whether this symbol is declared `special` (always dynamically bound).
    pub special: bool,
    /// Whether this symbol is a constant (defconst).
    pub constant: bool,
}

impl SymbolData {
    pub fn new(name: SymId) -> Self {
        Self {
            name,
            value: SymbolValue::Plain(None),
            function: None,
            plist: HashMap::new(),
            special: false,
            constant: false,
        }
    }
}

/// The obarray — a table of interned symbols.
///
/// This is the central symbol registry. `intern` looks up or creates symbols,
/// ensuring that `(eq 'foo 'foo)` is always true.
#[derive(Clone, Debug)]
pub struct Obarray {
    symbols: HashMap<SymId, SymbolData>,
    global_members: HashSet<SymId>,
    function_unbound: HashSet<SymId>,
    function_epoch: u64,
}

impl Default for Obarray {
    fn default() -> Self {
        Self::new()
    }
}

impl Obarray {
    fn is_canonical_symbol_id(id: SymId) -> bool {
        lookup_interned(resolve_sym(id)).is_some_and(|canonical| canonical == id)
    }

    fn ensure_global_member_if_canonical(&mut self, id: SymId) {
        if Self::is_canonical_symbol_id(id) {
            self.global_members.insert(id);
        }
    }

    fn value_from_symbol_id(id: SymId) -> Value {
        let name = resolve_sym(id);
        if Self::is_canonical_symbol_id(id) {
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
            symbols: HashMap::new(),
            global_members: HashSet::new(),
            function_unbound: HashSet::new(),
            function_epoch: 0,
        };

        // Pre-intern fundamental symbols
        let t_id = intern("t");
        let mut t_sym = SymbolData::new(t_id);
        t_sym.value = SymbolValue::Plain(Some(Value::T));
        t_sym.constant = true;
        t_sym.special = true;
        ob.symbols.insert(t_id, t_sym);
        ob.global_members.insert(t_id);

        let nil_id = intern("nil");
        let mut nil_sym = SymbolData::new(nil_id);
        nil_sym.value = SymbolValue::Plain(Some(Value::NIL));
        nil_sym.constant = true;
        nil_sym.special = true;
        ob.symbols.insert(nil_id, nil_sym);
        ob.global_members.insert(nil_id);

        ob
    }

    /// Intern a symbol: look up by name, creating if absent.
    /// Returns the symbol name (which is the key for identity).
    pub fn intern(&mut self, name: &str) -> String {
        let id = intern(name);
        self.ensure_symbol_id(id);
        self.global_members.insert(id);
        name.to_string()
    }

    /// Look up a symbol without creating it. Returns None if not interned.
    pub fn intern_soft(&self, name: &str) -> Option<&SymbolData> {
        let id = lookup_interned(name)?;
        self.global_members
            .contains(&id)
            .then(|| self.symbols.get(&id))
            .flatten()
    }

    /// Get symbol data (mutable). Interns the symbol if needed.
    pub fn get_or_intern(&mut self, name: &str) -> &mut SymbolData {
        let id = intern(name);
        self.global_members.insert(id);
        self.ensure_symbol_id(id)
    }

    /// Get symbol data (immutable).
    pub fn get(&self, name: &str) -> Option<&SymbolData> {
        let id = lookup_interned(name)?;
        self.global_members
            .contains(&id)
            .then(|| self.symbols.get(&id))
            .flatten()
    }

    /// Get symbol data (mutable).
    pub fn get_mut(&mut self, name: &str) -> Option<&mut SymbolData> {
        let id = lookup_interned(name)?;
        self.global_members
            .contains(&id)
            .then(|| self.symbols.get_mut(&id))
            .flatten()
    }

    /// Ensure symbol storage exists for an arbitrary symbol id.
    pub fn ensure_symbol_id(&mut self, id: SymId) -> &mut SymbolData {
        self.symbols
            .entry(id)
            .or_insert_with(|| SymbolData::new(id))
    }

    /// Get symbol data by identity.
    pub fn get_by_id(&self, id: SymId) -> Option<&SymbolData> {
        self.symbols.get(&id)
    }

    /// Get mutable symbol data by identity.
    pub fn get_mut_by_id(&mut self, id: SymId) -> Option<&mut SymbolData> {
        self.symbols.get_mut(&id)
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
            match self.symbols.get(&current)?.value {
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
        self.global_members.insert(id);
        self.set_symbol_value_id_inner(id, value);
    }

    /// Set the value cell of a symbol by identity.
    pub fn set_symbol_value_id(&mut self, id: SymId, value: Value) {
        self.ensure_global_member_if_canonical(id);
        self.set_symbol_value_id_inner(id, value);
    }

    /// Inner helper: follow aliases and write the value at the resolved target.
    fn set_symbol_value_id_inner(&mut self, id: SymId, value: Value) {
        let target = self.resolve_alias_for_write(id);
        let sym = self.ensure_symbol_id(target);
        match sym.value {
            SymbolValue::Plain(_) => sym.value = SymbolValue::Plain(Some(value)),
            SymbolValue::BufferLocal {
                ref mut default, ..
            } => *default = Some(value),
            SymbolValue::Forwarded => { /* no-op placeholder */ }
            SymbolValue::Alias(_) => {
                // resolve_alias_for_write should have resolved this, but
                // as a safety fallback write as Plain.
                sym.value = SymbolValue::Plain(Some(value));
            }
        }
    }

    /// Visit each stored symbol value cell that currently holds a `Value`.
    pub fn for_each_value_cell_mut(&mut self, mut f: impl FnMut(&mut Value)) {
        for sym in self.symbols.values_mut() {
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
            match self.symbols.get(&current) {
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
        if self.function_unbound.contains(&id) {
            return None;
        }
        self.symbols.get(&id).and_then(|s| s.function.as_ref())
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
        self.global_members.insert(id);
        let sym = self.ensure_symbol_id(id);
        sym.function = Some(function);
        self.function_unbound.remove(&id);
        self.function_epoch = self.function_epoch.wrapping_add(1);
    }

    /// Set the function cell of a symbol by identity.
    pub fn set_symbol_function_id(&mut self, id: SymId, function: Value) {
        self.ensure_global_member_if_canonical(id);
        let sym = self.ensure_symbol_id(id);
        sym.function = Some(function);
        self.function_unbound.remove(&id);
        self.function_epoch = self.function_epoch.wrapping_add(1);
    }

    /// Remove the function cell (fmakunbound).
    pub fn fmakunbound(&mut self, name: &str) {
        self.fmakunbound_id(intern(name));
    }

    /// Remove the function cell by identity.
    pub fn fmakunbound_id(&mut self, id: SymId) {
        self.ensure_global_member_if_canonical(id);
        let mut changed = self.function_unbound.insert(id);
        if let Some(sym) = self.symbols.get_mut(&id) {
            changed |= sym.function.take().is_some();
        }
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
        if let Some(sym) = self.symbols.get_mut(&id) {
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
        if let Some(sym) = self.symbols.get_mut(&target) {
            if !sym.constant {
                match sym.value {
                    SymbolValue::Plain(_) => sym.value = SymbolValue::Plain(None),
                    SymbolValue::BufferLocal {
                        ref mut default, ..
                    } => *default = None,
                    SymbolValue::Forwarded => { /* no-op */ }
                    SymbolValue::Alias(_) => sym.value = SymbolValue::Plain(None),
                }
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
            match self.symbols.get(&current) {
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
        if self.function_unbound.contains(&id) {
            return false;
        }
        self.symbols
            .get(&id)
            .and_then(|s| s.function.as_ref())
            .is_some_and(|f| !f.is_nil())
    }

    /// Get a property from the symbol's plist.
    pub fn get_property(&self, name: &str, prop: &str) -> Option<&Value> {
        self.get_property_id(intern(name), intern(prop))
    }

    /// Get a property from the symbol's plist by identity.
    pub fn get_property_id(&self, symbol: SymId, prop: SymId) -> Option<&Value> {
        self.symbols.get(&symbol).and_then(|s| s.plist.get(&prop))
    }

    /// Set a property on the symbol's plist.
    pub fn put_property(&mut self, name: &str, prop: &str, value: Value) {
        let symbol = intern(name);
        self.global_members.insert(symbol);
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
        match self.symbols.get(&id) {
            Some(sym) if !sym.plist.is_empty() => {
                let mut items = Vec::new();
                for (k, v) in &sym.plist {
                    items.push(Self::value_from_symbol_id(*k));
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
        self.global_members.insert(id);
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
        self.global_members.insert(id);
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
        self.symbols.get(&id).is_some_and(|s| s.special)
    }

    /// Check if a symbol is a constant.
    pub fn is_constant(&self, name: &str) -> bool {
        self.is_constant_id(intern(name))
    }

    /// Check if a symbol is a constant by identity.
    pub fn is_constant_id(&self, id: SymId) -> bool {
        (Self::is_canonical_symbol_id(id) && resolve_sym(id).starts_with(':'))
            || self.symbols.get(&id).is_some_and(|s| s.constant)
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
    pub fn make_buffer_local(&mut self, name: &str, local_if_set: bool) {
        let id = intern(name);
        self.global_members.insert(id);
        let sym = self.ensure_symbol_id(id);
        let old_default = match &sym.value {
            SymbolValue::Plain(v) => v.clone(),
            SymbolValue::BufferLocal { default, .. } => default.clone(),
            _ => None,
        };
        sym.value = SymbolValue::BufferLocal {
            default: old_default,
            local_if_set,
        };
    }

    /// Install a variable-alias edge: reading/writing `id` will redirect to `target`.
    pub fn make_alias(&mut self, id: SymId, target: SymId) {
        let sym = self.ensure_symbol_id(id);
        sym.value = SymbolValue::Alias(target);
    }

    /// Check whether a symbol is a buffer-local variable in the obarray.
    pub fn is_buffer_local(&self, name: &str) -> bool {
        self.is_buffer_local_id(intern(name))
    }

    /// Check whether a symbol is a buffer-local variable by identity.
    pub fn is_buffer_local_id(&self, id: SymId) -> bool {
        self.symbols
            .get(&id)
            .is_some_and(|s| matches!(s.value, SymbolValue::BufferLocal { .. }))
    }

    /// Check whether a symbol is an alias by identity.
    pub fn is_alias_id(&self, id: SymId) -> bool {
        self.symbols
            .get(&id)
            .is_some_and(|s| matches!(s.value, SymbolValue::Alias(_)))
    }

    /// Get the default value of a symbol, following aliases.
    /// For `Plain` and `BufferLocal` this is the direct/default value;
    /// for `Alias` it follows the chain; for `Forwarded` it returns `None`.
    pub fn default_value_id(&self, id: SymId) -> Option<&Value> {
        let mut current = id;
        for _ in 0..50 {
            match self.symbols.get(&current)?.value {
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
        let mut current_id = intern(name);
        let mut depth = 0;
        loop {
            if depth > 100 {
                return None; // Circular alias chain
            }
            let func = self.symbols.get(&current_id)?.function.as_ref()?;
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
        self.global_members.len()
    }

    pub fn is_empty(&self) -> bool {
        self.global_members.is_empty()
    }

    /// All interned symbol names.
    pub fn all_symbols(&self) -> Vec<&str> {
        self.global_members
            .iter()
            .map(|id| resolve_sym(*id))
            .collect()
    }

    /// Remove a symbol from the obarray.  Returns `true` if it was present.
    pub fn unintern(&mut self, name: &str) -> bool {
        let id = intern(name);
        let removed_symbol = self.global_members.remove(&id);
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
        self.function_unbound.contains(&id)
    }

    // -----------------------------------------------------------------------
    // pdump accessors
    // -----------------------------------------------------------------------

    /// Iterate over all (SymId, &SymbolData) pairs (for pdump serialization).
    pub(crate) fn iter_symbols(&self) -> impl Iterator<Item = (&SymId, &SymbolData)> {
        self.symbols.iter()
    }

    /// Access the set of ids interned in the global obarray.
    pub(crate) fn global_members(&self) -> &HashSet<SymId> {
        &self.global_members
    }

    /// Access the set of fmakunbound'd symbol ids (for pdump serialization).
    pub(crate) fn function_unbound_set(&self) -> &HashSet<SymId> {
        &self.function_unbound
    }

    /// Reconstruct an Obarray from pdump data.
    pub(crate) fn from_dump(
        symbols: HashMap<SymId, SymbolData>,
        global_members: HashSet<SymId>,
        function_unbound: HashSet<SymId>,
        function_epoch: u64,
    ) -> Self {
        Self {
            symbols,
            global_members,
            function_unbound,
            function_epoch,
        }
    }
}

impl GcTrace for Obarray {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        for sym in self.symbols.values() {
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
