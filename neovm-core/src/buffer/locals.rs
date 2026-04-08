use std::collections::HashMap;

use crate::emacs_core::symbol::Obarray;
use crate::emacs_core::value::{RuntimeBindingValue, Value, ValueKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BufferLocalStorageKind {
    AlwaysLocal,
    ConditionalSlot,
    LispBinding,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ConditionalSlotSpec {
    name: &'static str,
    permanent: bool,
}

// GNU buffer.c init_buffer_once: slots marked with -1 in buffer_local_flags are
// always buffer-local in every live buffer.
//
// Phase 10C: names that have migrated to `Buffer::slots[]` via
// `BUFFER_SLOT_INFO` are not in this list -- they live exclusively
// in the slot table and the BufferLocals path no longer mirrors them.
// Only `buffer-undo-list` remains here because the undo state has its
// own dedicated `SharedUndoState` storage and is not a simple slot.
const ALWAYS_LOCAL_BUFFER_LOCAL_NAMES: &[&str] = &[
    "buffer-undo-list",
];

// GNU buffer.c init_buffer_once: slots assigned an idx in buffer_local_flags
// are only buffer-local when the buffer's local flag is set.
const CONDITIONAL_SLOT_BUFFER_LOCAL_SPECS: &[ConditionalSlotSpec] = &[
    ConditionalSlotSpec {
        name: "mode-line-format",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "abbrev-mode",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "overwrite-mode",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "auto-fill-function",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "selective-display",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "selective-display-ellipses",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "tab-width",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "truncate-lines",
        permanent: true,
    },
    ConditionalSlotSpec {
        name: "word-wrap",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "ctl-arrow",
        permanent: false,
    },
    // Phase 10D step 4: `fill-column` migrated to BUFFER_SLOT_INFO
    // (`buffer.rs:BUFFER_SLOT_FILL_COLUMN`). The slot table is the
    // single source of truth; the legacy entry would shadow the
    // FORWARDED dispatch.
    ConditionalSlotSpec {
        name: "left-margin",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "local-abbrev-table",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "buffer-display-table",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "syntax-table-object",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "cache-long-scans",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "category-table",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "case-fold-search",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "indent-tabs-mode",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "bidi-display-reordering",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "bidi-paragraph-direction",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "bidi-paragraph-separate-re",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "bidi-paragraph-start-re",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "buffer-file-coding-system",
        permanent: true,
    },
    ConditionalSlotSpec {
        name: "left-margin-width",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "right-margin-width",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "left-fringe-width",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "right-fringe-width",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "fringes-outside-margins",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "scroll-bar-width",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "scroll-bar-height",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "vertical-scroll-bar",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "horizontal-scroll-bar",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "indicate-empty-lines",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "indicate-buffer-boundaries",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "fringe-indicator-alist",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "fringe-cursor-alist",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "scroll-up-aggressively",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "scroll-down-aggressively",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "header-line-format",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "tab-line-format",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "cursor-type",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "line-spacing",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "text-conversion-style",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "cursor-in-non-selected-windows",
        permanent: false,
    },
    ConditionalSlotSpec {
        name: "case-table",
        permanent: false,
    },
];

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
    } else if CONDITIONAL_SLOT_BUFFER_LOCAL_SPECS
        .iter()
        .any(|spec| spec.name == name)
    {
        BufferLocalStorageKind::ConditionalSlot
    } else {
        BufferLocalStorageKind::LispBinding
    }
}

fn conditional_slot_is_permanent(name: &str) -> bool {
    CONDITIONAL_SLOT_BUFFER_LOCAL_SPECS
        .iter()
        .find(|spec| spec.name == name)
        .is_some_and(|spec| spec.permanent)
}

/// GNU buffer.c splits per-buffer state into:
/// - always-local slot-backed vars
/// - conditional slot-backed vars whose local flag may be set per buffer
/// - ordinary Lisp locals in local_var_alist
///
/// This structure mirrors that ownership split without requiring GNU's C
/// buffer layout.
#[derive(Clone)]
pub struct BufferLocals {
    always_local_bindings: HashMap<String, RuntimeBindingValue>,
    slot_bindings: HashMap<String, RuntimeBindingValue>,
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
            slot_bindings: HashMap::new(),
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

        for spec in CONDITIONAL_SLOT_BUFFER_LOCAL_SPECS {
            if let Some(binding) = binding_map.remove(spec.name) {
                locals.set_raw_binding(spec.name, binding);
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
            self.slot_bindings.clear();
        } else {
            self.slot_bindings
                .retain(|name, _| conditional_slot_is_permanent(name));
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
            BufferLocalStorageKind::ConditionalSlot => {
                self.slot_bindings.insert(name.to_string(), binding);
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
            BufferLocalStorageKind::ConditionalSlot => self.slot_bindings.get(name).copied(),
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
            BufferLocalStorageKind::ConditionalSlot => self
                .slot_bindings
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
            .chain(self.slot_bindings.values_mut())
            .chain(self.lisp_bindings.iter_mut().map(|(_, binding)| binding))
            .filter_map(|binding| match binding {
                RuntimeBindingValue::Bound(value) => Some(value),
                RuntimeBindingValue::Void => None,
            })
    }

    pub fn has_local(&self, name: &str) -> bool {
        match buffer_local_storage_kind(name) {
            BufferLocalStorageKind::AlwaysLocal => true,
            BufferLocalStorageKind::ConditionalSlot => self.slot_bindings.contains_key(name),
            BufferLocalStorageKind::LispBinding => self
                .lisp_bindings
                .iter()
                .any(|(existing_name, _)| existing_name == name),
        }
    }

    pub fn remove(&mut self, name: &str) -> Option<RuntimeBindingValue> {
        match buffer_local_storage_kind(name) {
            BufferLocalStorageKind::AlwaysLocal => None,
            BufferLocalStorageKind::ConditionalSlot => self.slot_bindings.remove(name),
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

        for spec in CONDITIONAL_SLOT_BUFFER_LOCAL_SPECS {
            if let Some(binding) = self.slot_bindings.get(spec.name).copied() {
                bindings.push((spec.name.to_string(), binding));
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
            .chain(self.slot_bindings.values())
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
