use std::collections::HashMap;

use crate::emacs_core::symbol::Obarray;
use crate::emacs_core::value::{RuntimeBindingValue, Value, ValueKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BufferLocalStorageKind {
    AlwaysLocal,
    LispBinding,
}

// GNU buffer.c init_buffer_once: slots marked with -1 in buffer_local_flags are
// always buffer-local in every live buffer.
//
// Phase 10C/D: names that have migrated to `Buffer::slots[]` via
// `BUFFER_SLOT_INFO` are not in this list — they live exclusively
// in the slot table and the BufferLocals path no longer mirrors them.
// Only `buffer-undo-list` remains here because the undo state has its
// own dedicated `SharedUndoState` storage and is not a simple slot.
const ALWAYS_LOCAL_BUFFER_LOCAL_NAMES: &[&str] = &[
    "buffer-undo-list",
];

// Phase 10 final cleanup (post-Phase 10D step 5): every entry that
// was previously in `CONDITIONAL_SLOT_BUFFER_LOCAL_SPECS` is now
// either:
//   - migrated to BUFFER_SLOT_INFO (the bulk of the table:
//     fill-column, tab-width, mode-line-format, etc.), or
//   - routed through `LispBinding` storage (the 5 GNU-side
//     non-DEFVAR_PER_BUFFER holdouts: case-fold-search,
//     indent-tabs-mode, syntax-table-object, category-table,
//     case-table).
//
// The legacy `BufferLocalStorageKind::ConditionalSlot` variant and
// the per-buffer `slot_bindings` HashMap are gone with this change.

fn always_local_default_binding(name: &str) -> Option<RuntimeBindingValue> {
    let value = match name {
        "buffer-undo-list" => Value::NIL,
        _ => return None,
    };
    Some(RuntimeBindingValue::Bound(value))
}

fn always_local_kill_all_resets(name: &str) -> bool {
    matches!(
        name,
        "major-mode" | "mode-name" | "buffer-invisibility-spec"
    )
}

fn buffer_local_storage_kind(name: &str) -> BufferLocalStorageKind {
    if ALWAYS_LOCAL_BUFFER_LOCAL_NAMES.contains(&name) {
        BufferLocalStorageKind::AlwaysLocal
    } else {
        BufferLocalStorageKind::LispBinding
    }
}

/// GNU buffer.c splits per-buffer state into:
/// - always-local slot-backed vars
/// - conditional slot-backed vars whose local flag may be set per buffer
/// - ordinary Lisp locals in local_var_alist
///
/// Phase 10 final cleanup: BufferLocals now only owns
///   - the single `buffer-undo-list` always-local entry
///     (`buffer-undo-list` lives in its own `SharedUndoState`
///     storage but is mirrored here for the legacy
///     `RuntimeBindingValue` API)
///   - the `local_map` keymap slot
///   - a `lisp_bindings` vec of catch-all per-buffer locals for
///     names that aren't yet routed through `local_var_alist`
/// The `slot_bindings` HashMap and `ConditionalSlot` storage kind
/// are gone — every conditional BUFFER_OBJFWD slot lives in
/// `Buffer::slots[]` (Phase 10D), and the 5 GNU-side
/// non-DEFVAR_PER_BUFFER holdouts route through `lisp_bindings`.
#[derive(Clone)]
pub struct BufferLocals {
    always_local_bindings: HashMap<String, RuntimeBindingValue>,
    lisp_bindings: Vec<(String, RuntimeBindingValue)>,
    local_map: Value,
}

impl Default for BufferLocals {
    fn default() -> Self {
        Self::new()
    }
}

impl BufferLocals {
    pub fn new() -> Self {
        let mut always_local_bindings = HashMap::new();
        for name in ALWAYS_LOCAL_BUFFER_LOCAL_NAMES {
            if let Some(binding) = always_local_default_binding(name) {
                always_local_bindings.insert((*name).to_string(), binding);
            }
        }
        Self {
            always_local_bindings,
            lisp_bindings: Vec::new(),
            local_map: Value::NIL,
        }
    }

    pub fn from_dump(
        bindings: Vec<(String, RuntimeBindingValue)>,
        ordered_names: &[String],
        local_map: Value,
    ) -> Self {
        let mut binding_map: HashMap<String, RuntimeBindingValue> = bindings.into_iter().collect();
        let mut locals = Self::new();
        locals.local_map = local_map;

        for name in ordered_names {
            if let Some(binding) = binding_map.remove(name) {
                locals.set_raw_binding(name, binding);
            }
        }

        for name in ALWAYS_LOCAL_BUFFER_LOCAL_NAMES {
            if let Some(binding) = binding_map.remove(*name) {
                locals.set_raw_binding(name, binding);
            }
        }

        let mut remaining: Vec<_> = binding_map.into_iter().collect();
        remaining.sort_by(|left, right| left.0.cmp(&right.0));
        for (name, binding) in remaining {
            locals.set_raw_binding(&name, binding);
        }

        locals
    }

    pub fn kill_all_local_variables(&mut self, obarray: &Obarray, kill_permanent: bool) {
        for name in ALWAYS_LOCAL_BUFFER_LOCAL_NAMES {
            if always_local_kill_all_resets(name)
                && let Some(binding) = always_local_default_binding(name)
            {
                self.always_local_bindings
                    .insert((*name).to_string(), binding);
            }
        }

        if kill_permanent {
            self.lisp_bindings.clear();
        } else {
            self.lisp_bindings = self
                .lisp_bindings
                .drain(..)
                .filter_map(|(name, binding)| {
                    let permanent = obarray
                        .get_property(&name, "permanent-local")
                        .copied()
                        .filter(|value| !value.is_nil())?;
                    let preserved = if permanent.is_symbol_named("permanent-local-hook") {
                        preserve_partial_permanent_local_hook_binding(obarray, binding)
                    } else {
                        binding
                    };
                    Some((name, preserved))
                })
                .collect();
        }
        self.local_map = Value::NIL;
    }

    pub fn set_raw_binding(&mut self, name: &str, binding: RuntimeBindingValue) {
        match buffer_local_storage_kind(name) {
            BufferLocalStorageKind::AlwaysLocal => {
                self.always_local_bindings.insert(name.to_string(), binding);
            }
            BufferLocalStorageKind::LispBinding => {
                if let Some((_, existing)) = self
                    .lisp_bindings
                    .iter_mut()
                    .find(|(existing_name, _)| existing_name == name)
                {
                    *existing = binding;
                } else {
                    self.lisp_bindings.push((name.to_string(), binding));
                }
            }
        }
    }

    pub fn raw_binding(&self, name: &str) -> Option<RuntimeBindingValue> {
        match buffer_local_storage_kind(name) {
            BufferLocalStorageKind::AlwaysLocal => self.always_local_bindings.get(name).copied(),
            BufferLocalStorageKind::LispBinding => self
                .lisp_bindings
                .iter()
                .find(|(existing_name, _)| existing_name == name)
                .map(|(_, binding)| *binding),
        }
    }

    pub fn raw_value_ref(&self, name: &str) -> Option<&Value> {
        match buffer_local_storage_kind(name) {
            BufferLocalStorageKind::AlwaysLocal => self
                .always_local_bindings
                .get(name)
                .and_then(RuntimeBindingValue::as_ref),
            BufferLocalStorageKind::LispBinding => self
                .lisp_bindings
                .iter()
                .find(|(existing_name, _)| existing_name == name)
                .and_then(|(_, binding)| binding.as_ref()),
        }
    }

    pub fn bound_values_mut(&mut self) -> impl Iterator<Item = &mut Value> {
        self.always_local_bindings
            .values_mut()
            .chain(self.lisp_bindings.iter_mut().map(|(_, binding)| binding))
            .filter_map(|binding| match binding {
                RuntimeBindingValue::Bound(value) => Some(value),
                RuntimeBindingValue::Void => None,
            })
    }

    pub fn has_local(&self, name: &str) -> bool {
        match buffer_local_storage_kind(name) {
            BufferLocalStorageKind::AlwaysLocal => true,
            BufferLocalStorageKind::LispBinding => self
                .lisp_bindings
                .iter()
                .any(|(existing_name, _)| existing_name == name),
        }
    }

    pub fn remove(&mut self, name: &str) -> Option<RuntimeBindingValue> {
        match buffer_local_storage_kind(name) {
            BufferLocalStorageKind::AlwaysLocal => None,
            BufferLocalStorageKind::LispBinding => {
                let index = self
                    .lisp_bindings
                    .iter()
                    .position(|(existing_name, _)| existing_name == name)?;
                Some(self.lisp_bindings.remove(index).1)
            }
        }
    }

    pub fn ordered_runtime_bindings(&self) -> Vec<(String, RuntimeBindingValue)> {
        let mut bindings = Vec::new();

        for name in ALWAYS_LOCAL_BUFFER_LOCAL_NAMES {
            if let Some(binding) = self.always_local_bindings.get(*name).copied() {
                bindings.push(((*name).to_string(), binding));
            }
        }

        bindings.extend(self.lisp_bindings.iter().cloned());
        bindings
    }

    pub fn ordered_binding_names(&self) -> Vec<String> {
        self.ordered_runtime_bindings()
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    pub fn local_map(&self) -> Value {
        self.local_map
    }

    pub fn set_local_map(&mut self, keymap: Value) {
        self.local_map = keymap;
    }

    pub fn trace_roots(&self, roots: &mut Vec<Value>) {
        roots.push(self.local_map);
        for binding in self
            .always_local_bindings
            .values()
            .chain(self.lisp_bindings.iter().map(|(_, binding)| binding))
        {
            if let RuntimeBindingValue::Bound(value) = binding {
                roots.push(*value);
            }
        }
    }
}

fn preserve_partial_permanent_local_hook_binding(
    obarray: &Obarray,
    binding: RuntimeBindingValue,
) -> RuntimeBindingValue {
    match binding {
        RuntimeBindingValue::Bound(value) => {
            if !value.is_cons() {
                return RuntimeBindingValue::Bound(value);
            }

            let mut preserved = Vec::new();
            let mut cursor = value;
            while cursor.is_cons() {
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                let elt = pair_car;
                if elt.is_symbol_named("t")
                    || elt.as_symbol_name().is_some_and(|name| {
                        obarray
                            .get_property(name, "permanent-local-hook")
                            .is_some_and(|prop| !prop.is_nil())
                    })
                {
                    preserved.push(elt);
                }
                cursor = pair_cdr;
            }

            RuntimeBindingValue::Bound(Value::list(preserved))
        }
        RuntimeBindingValue::Void => RuntimeBindingValue::Void,
    }
}
