//! Obarray and symbol interning.
//!
//! In Emacs, symbols are unique objects stored in an "obarray" (hash table).
//! Each symbol has:
//! - A name (string)
//! - A value cell (variable binding)
//! - A function cell (function binding)
//! - A property list (plist)
//! - A `special` flag (for dynamic binding in lexical scope)

use super::intern::{SymId, intern, resolve_sym};
use super::value::Value;
use crate::gc::GcTrace;
use std::collections::{HashMap, HashSet};

/// Per-symbol metadata stored in the obarray.
#[derive(Clone, Debug)]
pub struct SymbolData {
    /// The symbol's name.
    pub name: SymId,
    /// Value cell (None = void/unbound).
    pub value: Option<Value>,
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
            value: None,
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
    function_unbound: HashSet<SymId>,
    function_epoch: u64,
}

impl Default for Obarray {
    fn default() -> Self {
        Self::new()
    }
}

impl Obarray {
    pub fn new() -> Self {
        let mut ob = Self {
            symbols: HashMap::new(),
            function_unbound: HashSet::new(),
            function_epoch: 0,
        };

        // Pre-intern fundamental symbols
        let t_id = intern("t");
        let mut t_sym = SymbolData::new(t_id);
        t_sym.value = Some(Value::True);
        t_sym.constant = true;
        t_sym.special = true;
        ob.symbols.insert(t_id, t_sym);

        let nil_id = intern("nil");
        let mut nil_sym = SymbolData::new(nil_id);
        nil_sym.value = Some(Value::Nil);
        nil_sym.constant = true;
        nil_sym.special = true;
        ob.symbols.insert(nil_id, nil_sym);

        ob
    }

    /// Intern a symbol: look up by name, creating if absent.
    /// Returns the symbol name (which is the key for identity).
    pub fn intern(&mut self, name: &str) -> String {
        let id = intern(name);
        if !self.symbols.contains_key(&id) {
            self.symbols.insert(id, SymbolData::new(id));
        }
        name.to_string()
    }

    /// Look up a symbol without creating it. Returns None if not interned.
    pub fn intern_soft(&self, name: &str) -> Option<&SymbolData> {
        self.symbols.get(&intern(name))
    }

    /// Get symbol data (mutable). Interns the symbol if needed.
    pub fn get_or_intern(&mut self, name: &str) -> &mut SymbolData {
        let id = intern(name);
        if !self.symbols.contains_key(&id) {
            self.symbols.insert(id, SymbolData::new(id));
        }
        self.symbols.get_mut(&id).unwrap()
    }

    /// Get symbol data (immutable).
    pub fn get(&self, name: &str) -> Option<&SymbolData> {
        self.symbols.get(&intern(name))
    }

    /// Get symbol data (mutable).
    pub fn get_mut(&mut self, name: &str) -> Option<&mut SymbolData> {
        self.symbols.get_mut(&intern(name))
    }

    /// Get the value cell of a symbol.
    pub fn symbol_value(&self, name: &str) -> Option<&Value> {
        self.symbols
            .get(&intern(name))
            .and_then(|s| s.value.as_ref())
    }

    /// Set the value cell of a symbol. Interns if needed.
    pub fn set_symbol_value(&mut self, name: &str, value: Value) {
        let sym = self.get_or_intern(name);
        sym.value = Some(value);
    }

    /// Get the function cell of a symbol.
    pub fn symbol_function(&self, name: &str) -> Option<&Value> {
        if self.function_unbound.contains(&intern(name)) {
            return None;
        }
        self.symbols
            .get(&intern(name))
            .and_then(|s| s.function.as_ref())
    }

    /// Set the function cell of a symbol (fset). Interns if needed.
    pub fn set_symbol_function(&mut self, name: &str, function: Value) {
        let sym = self.get_or_intern(name);
        sym.function = Some(function);
        self.function_unbound.remove(&intern(name));
        self.function_epoch = self.function_epoch.wrapping_add(1);
    }

    /// Remove the function cell (fmakunbound).
    pub fn fmakunbound(&mut self, name: &str) {
        let id = intern(name);
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
        let id = intern(name);
        if let Some(sym) = self.symbols.get_mut(&id) {
            if sym.function.take().is_some() {
                self.function_epoch = self.function_epoch.wrapping_add(1);
            }
        }
    }

    /// Remove the value cell (makunbound).
    pub fn makunbound(&mut self, name: &str) {
        if let Some(sym) = self.symbols.get_mut(&intern(name)) {
            if !sym.constant {
                sym.value = None;
            }
        }
    }

    /// Check if a symbol is bound (has a value cell).
    pub fn boundp(&self, name: &str) -> bool {
        self.symbols
            .get(&intern(name))
            .is_some_and(|s| s.value.is_some())
    }

    /// Check if a symbol has a function cell.
    pub fn fboundp(&self, name: &str) -> bool {
        let id = intern(name);
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
        self.symbols
            .get(&intern(name))
            .and_then(|s| s.plist.get(&intern(prop)))
    }

    /// Set a property on the symbol's plist.
    pub fn put_property(&mut self, name: &str, prop: &str, value: Value) {
        let sym = self.get_or_intern(name);
        sym.plist.insert(intern(prop), value);
    }

    /// Get the symbol's full plist as a flat list.
    pub fn symbol_plist(&self, name: &str) -> Value {
        match self.symbols.get(&intern(name)) {
            Some(sym) if !sym.plist.is_empty() => {
                let mut items = Vec::new();
                for (k, v) in &sym.plist {
                    items.push(Value::symbol(resolve_sym(*k)));
                    items.push(*v);
                }
                Value::list(items)
            }
            _ => Value::Nil,
        }
    }

    /// Mark a symbol as special (dynamically bound).
    pub fn make_special(&mut self, name: &str) {
        self.get_or_intern(name).special = true;
    }

    /// Check if a symbol is special.
    pub fn is_special(&self, name: &str) -> bool {
        self.symbols.get(&intern(name)).is_some_and(|s| s.special)
    }

    /// Check if a symbol is a constant.
    pub fn is_constant(&self, name: &str) -> bool {
        name.starts_with(':') || self.symbols.get(&intern(name)).is_some_and(|s| s.constant)
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
            match func {
                Value::Symbol(id) => {
                    current_id = *id;
                    depth += 1;
                }
                _ => return Some(*func),
            }
        }
    }

    /// Number of interned symbols.
    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }

    /// All interned symbol names.
    pub fn all_symbols(&self) -> Vec<&str> {
        self.symbols.keys().map(|id| resolve_sym(*id)).collect()
    }

    /// Remove a symbol from the obarray.  Returns `true` if it was present.
    pub fn unintern(&mut self, name: &str) -> bool {
        let id = intern(name);
        let removed_symbol = self.symbols.remove(&id).is_some();
        let removed_unbound = self.function_unbound.remove(&id);
        if removed_symbol || removed_unbound {
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
        self.function_unbound.contains(&intern(name))
    }

    // -----------------------------------------------------------------------
    // pdump accessors
    // -----------------------------------------------------------------------

    /// Iterate over all (SymId, &SymbolData) pairs (for pdump serialization).
    pub(crate) fn iter_symbols(&self) -> impl Iterator<Item = (&SymId, &SymbolData)> {
        self.symbols.iter()
    }

    /// Access the set of fmakunbound'd symbol ids (for pdump serialization).
    pub(crate) fn function_unbound_set(&self) -> &HashSet<SymId> {
        &self.function_unbound
    }

    /// Reconstruct an Obarray from pdump data.
    pub(crate) fn from_dump(
        symbols: HashMap<SymId, SymbolData>,
        function_unbound: HashSet<SymId>,
        function_epoch: u64,
    ) -> Self {
        Self {
            symbols,
            function_unbound,
            function_epoch,
        }
    }
}

impl GcTrace for Obarray {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        for sym in self.symbols.values() {
            if let Some(ref v) = sym.value {
                roots.push(*v);
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
