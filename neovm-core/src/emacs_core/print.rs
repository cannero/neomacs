//! Value printing (Lisp representation).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt::Write as _;

use super::chartable::{bool_vector_length, char_table_external_slots};
use super::intern::{SymId, lookup_interned_lisp_string, resolve_sym, resolve_sym_lisp_string};
use super::string_escape::{format_lisp_string_bytes_emacs, format_lisp_string_emacs};
use super::value::{
    HashKey, HashTableTest, StringTextPropertyRun, Value, get_string_text_properties_for_value,
    list_to_vec,
};
use crate::emacs_core::value::{ValueKind, VecLikeType};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PrintOptions {
    pub print_gensym: bool,
    pub print_circle: bool,
    pub print_escape_newlines: bool,
    pub print_escape_nonascii: bool,
    pub print_escape_multibyte: bool,
    pub print_escape_control_characters: bool,
    pub print_level: Option<i64>,
    pub print_length: Option<i64>,
    pub print_continuous_numbering: bool,
    pub print_number_table: Option<Value>,
    backquote_output_level: usize,
}

impl PrintOptions {
    pub const fn with_print_gensym(print_gensym: bool) -> Self {
        Self {
            print_gensym,
            print_circle: false,
            print_escape_newlines: false,
            print_escape_nonascii: false,
            print_escape_multibyte: false,
            print_escape_control_characters: false,
            print_level: None,
            print_length: None,
            print_continuous_numbering: false,
            print_number_table: None,
            backquote_output_level: 0,
        }
    }

    /// Full constructor for all print options.
    pub fn new(
        print_gensym: bool,
        print_circle: bool,
        print_level: Option<i64>,
        print_length: Option<i64>,
    ) -> Self {
        Self {
            print_gensym,
            print_circle,
            print_escape_newlines: false,
            print_escape_nonascii: false,
            print_escape_multibyte: false,
            print_escape_control_characters: false,
            print_level,
            print_length,
            print_continuous_numbering: false,
            print_number_table: None,
            backquote_output_level: 0,
        }
    }

    pub(crate) fn enter_backquote(self) -> Self {
        Self {
            backquote_output_level: self.backquote_output_level + 1,
            ..self
        }
    }

    pub(crate) fn exit_backquote(self) -> Self {
        Self {
            backquote_output_level: self.backquote_output_level.saturating_sub(1),
            ..self
        }
    }

    pub(crate) fn allow_unquote_shorthand(self) -> bool {
        self.backquote_output_level > 0
    }
}

// ---------------------------------------------------------------------------
// Print-circle state (two-pass algorithm)
// ---------------------------------------------------------------------------

/// State for the print-circle two-pass algorithm.
/// Keys are object identity values (SymId).
pub struct PrintCircleState {
    /// Maps object identity -> label status:
    /// 0 = seen once (removed after pass 1)
    /// negative = assigned label, not yet printed
    /// positive = already printed with this label
    number_table: HashMap<u64, i64>,
    next_label: i64,
}

impl PrintCircleState {
    fn new() -> Self {
        Self {
            number_table: HashMap::new(),
            next_label: 0,
        }
    }
}

thread_local! {
    static PRINT_NUMBER_INDEX: Cell<i64> = const { Cell::new(0) };
    static PRINT_CALL_DEPTH: Cell<usize> = const { Cell::new(0) };
}

fn reset_print_number_index() {
    PRINT_NUMBER_INDEX.with(|index| index.set(0));
}

fn next_print_number_index() -> i64 {
    PRINT_NUMBER_INDEX.with(|index| {
        let next = index.get() + 1;
        index.set(next);
        next
    })
}

/// Combined print state used by the stateful print path.
pub(crate) struct PrintState<'a> {
    pub options: PrintOptions,
    pub circle: Option<&'a mut PrintCircleState>,
    pub buffers: Option<&'a crate::buffer::BufferManager>,
    pub depth: i64,
    object_stack: Vec<u64>,
}

/// Check if a value is a candidate for circle detection.
/// Matches GNU Emacs's `print_circle_candidate_p`.
fn is_print_circle_candidate(value: &Value, print_gensym: bool) -> bool {
    match value.kind() {
        ValueKind::Cons => true,
        ValueKind::Veclike(VecLikeType::Vector) => {
            // Non-empty vectors only
            value.as_vector_data().map_or(false, |v| !v.is_empty())
        }
        ValueKind::Veclike(VecLikeType::Record) => true,
        ValueKind::Veclike(VecLikeType::HashTable) => true,
        ValueKind::Veclike(VecLikeType::Lambda) => true,
        ValueKind::Veclike(VecLikeType::Macro) => true,
        ValueKind::Veclike(VecLikeType::ByteCode) => true,
        ValueKind::String => {
            // Non-empty strings only
            value.as_utf8_str().map_or(false, |s| !s.is_empty())
        }
        ValueKind::Symbol(id) if print_gensym => {
            // Uninterned symbols only
            let name = resolve_sym_lisp_string(id);
            lookup_interned_lisp_string(name) != Some(id)
        }
        _ => false,
    }
}

fn is_uninterned_symbol(value: &Value) -> bool {
    match value.kind() {
        ValueKind::Symbol(id) => {
            let name = resolve_sym_lisp_string(id);
            lookup_interned_lisp_string(name) != Some(id)
        }
        _ => false,
    }
}

/// Return a unique identity key for a circle-candidate value.
fn object_identity_key(value: &Value) -> Option<u64> {
    match value.kind() {
        ValueKind::Cons
        | ValueKind::Veclike(VecLikeType::Vector)
        | ValueKind::Veclike(VecLikeType::Record)
        | ValueKind::Veclike(VecLikeType::HashTable)
        | ValueKind::String
        | ValueKind::Veclike(VecLikeType::Lambda)
        | ValueKind::Veclike(VecLikeType::Macro)
        | ValueKind::Veclike(VecLikeType::ByteCode) => Some(value.0 as u64),
        ValueKind::Symbol(id) => {
            // Use a distinct namespace to avoid collisions with heap pointer keys.
            // Set the high bit to separate from pointer keys.
            Some((1u64 << 63) | (id.0 as u64))
        }
        _ => None,
    }
}

fn active_print_number_table(options: &PrintOptions) -> Option<Value> {
    if !options.print_continuous_numbering {
        return None;
    }
    options
        .print_number_table
        .filter(|table| table.is_hash_table())
}

pub(crate) struct PrintNumberingGuard;

pub(crate) fn enter_print_call(options: &PrintOptions) -> PrintNumberingGuard {
    PRINT_CALL_DEPTH.with(|depth| {
        let current = depth.get();
        if current == 0
            && (!options.print_continuous_numbering || active_print_number_table(options).is_none())
        {
            reset_print_number_index();
        }
        depth.set(current + 1);
    });
    PrintNumberingGuard
}

impl Drop for PrintNumberingGuard {
    fn drop(&mut self) {
        PRINT_CALL_DEPTH.with(|depth| {
            depth.set(depth.get().saturating_sub(1));
        });
    }
}

fn print_number_table_key(table_value: Value, value: &Value) -> Option<HashKey> {
    let table = table_value.as_hash_table()?;
    Some(value.to_hash_key(&table.test))
}

fn get_print_number_table_entry(table_value: Value, value: &Value) -> Option<(HashKey, Value)> {
    let key = print_number_table_key(table_value, value)?;
    let entry = {
        let table = table_value.as_hash_table()?;
        table.data.get(&key).copied().unwrap_or(Value::NIL)
    };
    Some((key, entry))
}

fn put_print_number_table_entry(
    table_value: Value,
    key: HashKey,
    key_value: Value,
    entry_value: Value,
) {
    let _ = table_value.with_hash_table_mut(|table| {
        let inserting_new_key = !table.data.contains_key(&key);
        table.data.insert(key.clone(), entry_value);
        if inserting_new_key {
            table.key_snapshots.insert(key.clone(), key_value);
            table.insertion_order.push(key);
        }
    });
}

fn remove_print_number_table_t_entries(table_value: Value) {
    let _ = table_value.with_hash_table_mut(|table| {
        let keys: Vec<HashKey> = table
            .data
            .iter()
            .filter_map(|(key, value)| {
                if *value == Value::T {
                    Some(key.clone())
                } else {
                    None
                }
            })
            .collect();
        for key in keys {
            table.data.remove(&key);
            table.key_snapshots.remove(&key);
            table.insertion_order.retain(|existing| existing != &key);
        }
    });
}

fn print_number_entry_is_symbol(entry: Value) -> bool {
    matches!(
        entry.kind(),
        ValueKind::Nil | ValueKind::T | ValueKind::Symbol(_)
    )
}

fn print_number_entry_is_cdr_label(entry: Value) -> bool {
    !entry.is_nil() && entry != Value::T
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrintNumberTableAction {
    PrintedReference,
    PrintedPrefix,
}

fn write_print_number_table_entry(
    value: &Value,
    out: &mut String,
    options: &PrintOptions,
) -> Option<PrintNumberTableAction> {
    if !options.print_circle {
        return None;
    }

    let table_value = active_print_number_table(options)?;
    let (key, entry) = get_print_number_table_entry(table_value, value)?;

    match entry.kind() {
        ValueKind::String => {
            let s = entry.as_lisp_string().unwrap();
            out.push_str(&crate::emacs_core::emacs_char::to_utf8_lossy(s.as_bytes()));
            Some(PrintNumberTableAction::PrintedReference)
        }
        ValueKind::Fixnum(n) if n < 0 => {
            write!(out, "#{}=", -n).unwrap();
            put_print_number_table_entry(table_value, key, *value, Value::fixnum(-n));
            Some(PrintNumberTableAction::PrintedPrefix)
        }
        ValueKind::Fixnum(n) if n > 0 => {
            write!(out, "#{n}#").unwrap();
            Some(PrintNumberTableAction::PrintedReference)
        }
        _ => None,
    }
}

/// Preprocess pass: walk the value tree to find shared/circular structures.
/// Uses an explicit stack (not recursive) matching GNU Emacs.
fn print_preprocess(value: &Value, state: &mut PrintCircleState, options: PrintOptions) {
    let mut stack: Vec<Value> = vec![*value];
    while let Some(obj) = stack.pop() {
        if !is_print_circle_candidate(&obj, options.print_gensym) {
            continue;
        }
        let key = match object_identity_key(&obj) {
            Some(k) => k,
            None => continue,
        };
        if let Some(status) = state.number_table.get(&key) {
            if *status == 0 {
                // Seen second time -- assign label
                let label = if options.print_continuous_numbering {
                    next_print_number_index()
                } else {
                    state.next_label += 1;
                    state.next_label
                };
                state.number_table.insert(key, -label);
            }
            // Already labeled or already seen multiple times -- skip children
            continue;
        }
        if options.print_continuous_numbering && is_uninterned_symbol(&obj) {
            state.number_table.insert(key, -next_print_number_index());
            continue;
        }
        // First time seen -- mark and process children
        state.number_table.insert(key, 0);
        match obj.kind() {
            ValueKind::Cons => {
                let pair_car = obj.cons_car();
                let pair_cdr = obj.cons_cdr();
                // Push cdr first so car is processed first (stack is LIFO)
                stack.push(pair_cdr);
                stack.push(pair_car);
            }
            ValueKind::Veclike(VecLikeType::Vector) => {
                let items = obj.as_vector_data().unwrap().clone();
                for item in items.iter().rev() {
                    stack.push(*item);
                }
            }
            ValueKind::Veclike(VecLikeType::Record) => {
                let items = obj.as_record_data().unwrap().clone();
                for item in items.iter().rev() {
                    stack.push(*item);
                }
            }
            ValueKind::Veclike(VecLikeType::HashTable) => {
                let table = obj.as_hash_table().unwrap().clone();
                for key_hk in table.insertion_order.iter().rev() {
                    if let Some(val) = table.data.get(key_hk) {
                        stack.push(*val);
                        let key_val = super::hashtab::hash_key_to_visible_value(&table, key_hk);
                        stack.push(key_val);
                    }
                }
            }
            ValueKind::Veclike(VecLikeType::Lambda) | ValueKind::Veclike(VecLikeType::Macro) => {
                if let Some(doc) = obj.closure_doc_value() {
                    stack.push(doc);
                }
                if let Some(env) = obj.closure_env().flatten() {
                    stack.push(env);
                }
                if let Some(body) = obj.closure_body_value() {
                    stack.push(body);
                }
            }
            ValueKind::Veclike(VecLikeType::ByteCode) => {
                let _ = with_bytecode_literal_slots(&obj, |slots| {
                    for item in slots.iter().rev() {
                        stack.push(*item);
                    }
                });
            }
            _ => {}
        }
    }
    // Remove entries seen only once
    state.number_table.retain(|_, v| *v != 0);
}

fn print_preprocess_external(value: &Value, table_value: Value, options: PrintOptions) {
    let mut stack: Vec<Value> = vec![*value];
    while let Some(obj) = stack.pop() {
        if !is_print_circle_candidate(&obj, options.print_gensym) {
            continue;
        }
        let Some((key, entry)) = get_print_number_table_entry(table_value, &obj) else {
            continue;
        };
        if !entry.is_nil() || (options.print_continuous_numbering && is_uninterned_symbol(&obj)) {
            if print_number_entry_is_symbol(entry) {
                let label = next_print_number_index();
                put_print_number_table_entry(table_value, key, obj, Value::fixnum(-label));
            }
            continue;
        }

        put_print_number_table_entry(table_value, key, obj, Value::T);
        match obj.kind() {
            ValueKind::Cons => {
                let pair_car = obj.cons_car();
                let pair_cdr = obj.cons_cdr();
                if !pair_cdr.is_nil() {
                    stack.push(pair_cdr);
                }
                stack.push(pair_car);
            }
            ValueKind::Veclike(VecLikeType::Vector) => {
                let items = obj.as_vector_data().unwrap().clone();
                for item in items.iter().rev() {
                    stack.push(*item);
                }
            }
            ValueKind::Veclike(VecLikeType::Record) => {
                let items = obj.as_record_data().unwrap().clone();
                for item in items.iter().rev() {
                    stack.push(*item);
                }
            }
            ValueKind::Veclike(VecLikeType::HashTable) => {
                let table = obj.as_hash_table().unwrap().clone();
                for key_hk in table.insertion_order.iter().rev() {
                    if let Some(val) = table.data.get(key_hk) {
                        stack.push(*val);
                        let key_val = super::hashtab::hash_key_to_visible_value(&table, key_hk);
                        stack.push(key_val);
                    }
                }
            }
            _ => {}
        }
    }

    remove_print_number_table_t_entries(table_value);
}

/// Entry point for stateful printing (circle/level/length aware).
/// Returns the printed representation as a String.
pub(crate) fn print_value_stateful(value: &Value, options: PrintOptions) -> String {
    print_value_stateful_with_buffers(value, None, options)
}

pub(crate) fn print_value_stateful_with_buffers(
    value: &Value,
    buffers: Option<&crate::buffer::BufferManager>,
    options: PrintOptions,
) -> String {
    let _print_guard = enter_print_call(&options);
    let mut out = String::new();
    let number_table = active_print_number_table(&options);
    if options.print_circle {
        let mut circle = PrintCircleState::new();
        if let Some(table_value) = number_table {
            print_preprocess_external(value, table_value, options);
        } else {
            print_preprocess(value, &mut circle, options);
        }
        let mut state = PrintState {
            options,
            circle: if number_table.is_some() {
                None
            } else {
                Some(&mut circle)
            },
            buffers,
            depth: 0,
            object_stack: Vec::new(),
        };
        write_value_stateful(value, &mut out, &mut state);
    } else {
        let mut state = PrintState {
            options,
            circle: None,
            buffers,
            depth: 0,
            object_stack: Vec::new(),
        };
        write_value_stateful(value, &mut out, &mut state);
    }
    out
}

pub(crate) fn default_cycle_candidate_key(value: &Value) -> Option<u64> {
    match value.kind() {
        ValueKind::Cons => object_identity_key(value),
        ValueKind::Veclike(VecLikeType::Vector) => value
            .as_vector_data()
            .is_some_and(|items| !items.is_empty())
            .then(|| object_identity_key(value))
            .flatten(),
        ValueKind::Veclike(VecLikeType::Record)
        | ValueKind::Veclike(VecLikeType::HashTable)
        | ValueKind::Veclike(VecLikeType::Lambda)
        | ValueKind::Veclike(VecLikeType::Macro)
        | ValueKind::Veclike(VecLikeType::ByteCode) => object_identity_key(value),
        _ => None,
    }
}

fn default_cycle_stack_index(value: &Value, state: &PrintState) -> Option<usize> {
    if state.circle.is_some() {
        return None;
    }
    let key = default_cycle_candidate_key(value)?;
    state.object_stack.iter().position(|entry| *entry == key)
}

fn push_default_cycle_object(value: &Value, state: &mut PrintState) -> bool {
    if state.circle.is_some() {
        return false;
    }
    let Some(key) = default_cycle_candidate_key(value) else {
        return false;
    };
    state.object_stack.push(key);
    true
}

fn with_default_cycle_guard(
    value: &Value,
    out: &mut String,
    state: &mut PrintState,
    render: impl FnOnce(&mut String, &mut PrintState),
) {
    if let Some(index) = default_cycle_stack_index(value, state) {
        write!(out, "#{index}").unwrap();
        return;
    }
    let pushed = push_default_cycle_object(value, state);
    render(out, state);
    if pushed {
        state.object_stack.pop();
    }
}

/// Core stateful print routine. Writes the printed representation of `value`
/// into `out`, respecting print-circle, print-level, and print-length.
fn write_value_stateful(value: &Value, out: &mut String, state: &mut PrintState) {
    if let Some(handle) = print_special_handle(value, state.buffers) {
        out.push_str(&handle);
        return;
    }

    // Circle check: if this object is a circle candidate, handle #N= / #N#
    if is_print_circle_candidate(value, state.options.print_gensym) {
        let mut printed_number_table_prefix = false;
        match write_print_number_table_entry(value, out, &state.options) {
            Some(PrintNumberTableAction::PrintedReference) => return,
            Some(PrintNumberTableAction::PrintedPrefix) => {
                printed_number_table_prefix = true;
            }
            None => {}
        }

        if !printed_number_table_prefix && let Some(ref mut circle) = state.circle {
            if let Some(key) = object_identity_key(value) {
                if let Some(label) = circle.number_table.get_mut(&key) {
                    if *label < 0 {
                        // First occurrence: emit #N= prefix
                        write!(out, "#{}=", -(*label)).unwrap();
                        *label = -(*label); // flip to positive
                    } else if *label > 0 {
                        // Subsequent: emit #N# and return
                        write!(out, "#{}#", *label).unwrap();
                        return;
                    }
                    // label == 0: not shared, fall through to normal print
                }
            }
        }
    }

    match value.kind() {
        ValueKind::Nil => out.push_str("nil"),
        ValueKind::T => out.push_str("t"),
        ValueKind::Fixnum(v) => write!(out, "{}", v).unwrap(),
        ValueKind::Float => out.push_str(&format_float(value.xfloat())),
        ValueKind::Symbol(id) => out.push_str(&format_symbol(id, state.options)),
        ValueKind::String => {
            let ls = value.as_lisp_string().unwrap();
            match get_string_text_properties_for_value(*value) {
                Some(runs) => out.push_str(&format_lisp_propertized_string_emacs(
                    ls,
                    &runs,
                    state.options,
                )),
                None => out.push_str(&format_lisp_string_emacs(ls, &state.options)),
            }
        }
        ValueKind::Cons => {
            with_default_cycle_guard(value, out, state, |out, state| {
                // Level check for containers
                if let Some(level) = state.options.print_level {
                    if state.depth >= level {
                        out.push_str("#");
                        return;
                    }
                }
                // Try shorthand (quote, function, backquote, etc.)
                if let Some(shorthand) = write_list_shorthand_stateful(value, state) {
                    out.push_str(&shorthand);
                    return;
                }
                state.depth += 1;
                out.push('(');
                write_cons_stateful(value, out, state);
                out.push(')');
                state.depth -= 1;
            });
        }
        ValueKind::Veclike(VecLikeType::Vector) => {
            if let Some(nbits) = bool_vector_length(value) {
                out.push_str(&format_bool_vector(value, nbits as usize));
                return;
            }
            if let Some(slots) = char_table_external_slots(value) {
                with_default_cycle_guard(value, out, state, |out, state| {
                    // Level check for char-table
                    if let Some(level) = state.options.print_level {
                        if state.depth >= level {
                            out.push_str("#");
                            return;
                        }
                    }
                    state.depth += 1;
                    out.push_str("#^[");
                    for (idx, item) in slots.iter().enumerate() {
                        if let Some(length) = state.options.print_length {
                            if idx as i64 >= length {
                                if idx > 0 {
                                    out.push(' ');
                                }
                                out.push_str("...");
                                break;
                            }
                        }
                        if idx > 0 {
                            out.push(' ');
                        }
                        write_value_stateful(item, out, state);
                    }
                    out.push(']');
                    state.depth -= 1;
                });
                return;
            }
            with_default_cycle_guard(value, out, state, |out, state| {
                // Level check
                if let Some(level) = state.options.print_level {
                    if state.depth >= level {
                        out.push_str("#");
                        return;
                    }
                }
                state.depth += 1;
                out.push('[');
                let items = value.as_vector_data().unwrap().clone();
                for (idx, item) in items.iter().enumerate() {
                    if let Some(length) = state.options.print_length {
                        if idx as i64 >= length {
                            if idx > 0 {
                                out.push(' ');
                            }
                            out.push_str("...");
                            break;
                        }
                    }
                    if idx > 0 {
                        out.push(' ');
                    }
                    write_value_stateful(item, out, state);
                }
                out.push(']');
                state.depth -= 1;
            });
        }
        ValueKind::Veclike(VecLikeType::Record) => {
            with_default_cycle_guard(value, out, state, |out, state| {
                // Level check
                if let Some(level) = state.options.print_level {
                    if state.depth >= level {
                        out.push_str("#");
                        return;
                    }
                }
                state.depth += 1;
                out.push_str("#s(");
                let items = value.as_record_data().unwrap().clone();
                for (idx, item) in items.iter().enumerate() {
                    if let Some(length) = state.options.print_length {
                        if idx as i64 >= length {
                            if idx > 0 {
                                out.push(' ');
                            }
                            out.push_str("...");
                            break;
                        }
                    }
                    if idx > 0 {
                        out.push(' ');
                    }
                    write_value_stateful(item, out, state);
                }
                out.push(')');
                state.depth -= 1;
            });
        }
        ValueKind::Veclike(VecLikeType::HashTable) => {
            with_default_cycle_guard(value, out, state, |out, state| {
                // Level check
                if let Some(level) = state.options.print_level {
                    if state.depth >= level {
                        out.push_str("#");
                        return;
                    }
                }
                state.depth += 1;
                write_hash_table_stateful(value, out, state);
                state.depth -= 1;
            });
        }
        ValueKind::Veclike(VecLikeType::Lambda) => {
            write_lambda_stateful(value, out, state);
        }
        ValueKind::Veclike(VecLikeType::Macro) => {
            write_macro_stateful(value, out, state);
        }
        ValueKind::Subr(id) => write!(out, "#<subr {}>", resolve_sym(id)).unwrap(),
        ValueKind::Veclike(VecLikeType::Subr) => {
            let id = value.as_subr_id().unwrap();
            write!(out, "#<subr {}>", resolve_sym(id)).unwrap()
        }
        ValueKind::Veclike(VecLikeType::ByteCode) => {
            write_bytecode_literal_stateful(value, out, state);
        }
        ValueKind::Veclike(VecLikeType::Marker) => out.push_str(
            &print_special_handle(value, state.buffers).unwrap_or_else(|| "#<marker>".to_string()),
        ),
        ValueKind::Veclike(VecLikeType::Overlay) => out.push_str(
            &print_special_handle(value, state.buffers).unwrap_or_else(|| "#<overlay>".to_string()),
        ),
        ValueKind::Veclike(VecLikeType::Buffer) => {
            let bid = value.as_buffer_id().unwrap();
            if let Some(buffers) = state.buffers {
                if let Some(buf) = buffers.get(bid) {
                    write!(out, "#<buffer {}>", buf.name_runtime_string_owned()).unwrap();
                } else if buffers.dead_buffer_last_name_value(bid).is_some() {
                    out.push_str("#<killed buffer>");
                } else {
                    write!(out, "#<buffer {}>", bid.0).unwrap();
                }
            } else {
                write!(out, "#<buffer {}>", bid.0).unwrap();
            }
        }
        ValueKind::Veclike(VecLikeType::Window) => {
            let wid = value.as_window_id().unwrap();
            write!(out, "#<window {}>", wid).unwrap();
        }
        ValueKind::Veclike(VecLikeType::Frame) => {
            let fid = value.as_frame_id().unwrap();
            out.push_str(&format_frame_handle(fid));
        }
        ValueKind::Veclike(VecLikeType::Timer) => {
            let tid = value.as_timer_id().unwrap();
            write!(out, "#<timer {}>", tid).unwrap();
        }
        ValueKind::Veclike(VecLikeType::Bignum) => {
            // GNU `print_object` formats bignums via `mpz_get_str`
            // (`src/print.c` PRINT_INTEGER branch). `rug::Integer`'s
            // Display delegates to the same routine.
            write!(out, "{}", value.as_bignum().unwrap()).unwrap();
        }
        ValueKind::Veclike(VecLikeType::SymbolWithPos) => {
            // GNU prints symbol-with-pos as the bare symbol name.
            // Full implementation in Task 7.
            if let Some(sym) = value.as_symbol_with_pos_sym() {
                write_value_stateful(&sym, out, state);
            } else {
                out.push_str("#<symbol-with-pos>");
            }
        }
        ValueKind::Unbound => out.push_str("#<unbound>"),
        ValueKind::Unknown => write!(out, "#<unknown {:#x}>", value.0).unwrap(),
    }
}

fn with_bytecode_literal_slots<R>(value: &Value, f: impl FnOnce(&[Value]) -> R) -> Option<R> {
    let bc = value.get_bytecode_data()?.clone();
    let saved_roots = crate::emacs_core::eval::save_scratch_gc_roots();

    let arglist = bc.arglist;
    crate::emacs_core::eval::push_scratch_gc_root(arglist);

    let code = if let Some(bytes) = &bc.gnu_bytecode_bytes {
        Value::heap_string(crate::heap_types::LispString::from_unibyte(bytes.clone()))
    } else {
        Value::NIL
    };
    crate::emacs_core::eval::push_scratch_gc_root(code);

    let constants = if let Some(env) = bc.env {
        env
    } else {
        Value::vector(bc.constants.clone())
    };
    crate::emacs_core::eval::push_scratch_gc_root(constants);

    let depth = Value::fixnum(bc.max_stack as i64);
    let doc = bc
        .doc_form
        .or_else(|| bc.docstring.as_ref().map(|d| Value::heap_string(d.clone())))
        .unwrap_or(Value::NIL);
    crate::emacs_core::eval::push_scratch_gc_root(doc);

    let interactive = bc.interactive.unwrap_or(Value::NIL);
    crate::emacs_core::eval::push_scratch_gc_root(interactive);

    let slot_count = bc.observable_closure_slot_count();
    let mut slots = vec![arglist, code, constants, depth];
    if slot_count > 4 {
        slots.push(doc);
    }
    if slot_count > 5 {
        slots.push(interactive);
    }
    if slot_count > 6 {
        let extra_count = slot_count - 6;
        for idx in 0..extra_count {
            slots.push(bc.extra_slots.get(idx).copied().unwrap_or(Value::NIL));
        }
    }
    let result = f(&slots);
    crate::emacs_core::eval::restore_scratch_gc_roots(saved_roots);
    Some(result)
}

fn write_bytecode_literal_stateful(value: &Value, out: &mut String, state: &mut PrintState) {
    with_default_cycle_guard(value, out, state, |out, state| {
        let _ = with_bytecode_literal_slots(value, |slots| {
            if let Some(level) = state.options.print_level {
                if state.depth >= level {
                    out.push_str("#");
                    return;
                }
            }
            state.depth += 1;
            out.push_str("#[");
            for (idx, item) in slots.iter().enumerate() {
                if let Some(length) = state.options.print_length {
                    if idx as i64 >= length {
                        if idx > 0 {
                            out.push(' ');
                        }
                        out.push_str("...");
                        break;
                    }
                }
                if idx > 0 {
                    out.push(' ');
                }
                write_value_stateful(item, out, state);
            }
            out.push(']');
            state.depth -= 1;
        });
    });
}

fn format_bytecode_literal(value: &Value, options: PrintOptions) -> String {
    with_bytecode_literal_slots(value, |slots| {
        let parts: Vec<String> = slots
            .iter()
            .map(|item| print_value_with_options(item, options))
            .collect();
        format!("#[{}]", parts.join(" "))
    })
    .unwrap_or_else(|| "#<bytecode>".to_string())
}

fn write_closure_body_forms_stateful(body: Value, out: &mut String, state: &mut PrintState) {
    let Some(forms) = list_to_vec(&body) else {
        write_value_stateful(&body, out, state);
        return;
    };
    if forms.is_empty() {
        out.push_str("nil");
    } else {
        for (idx, form) in forms.iter().enumerate() {
            if idx > 0 {
                out.push(' ');
            }
            write_value_stateful(form, out, state);
        }
    }
}

fn write_interpreted_closure_stateful(value: &Value, out: &mut String, state: &mut PrintState) {
    out.push_str("#[");
    out.push_str(
        &value
            .closure_params()
            .map_or_else(|| "nil".to_string(), format_params),
    );
    out.push(' ');
    if let Some(body) = value.closure_body_value() {
        write_value_stateful(&body, out, state);
    } else {
        out.push_str("nil");
    }
    out.push(' ');
    let env = value.closure_env().flatten().expect("closure env");
    if env == Value::NIL {
        out.push_str("(t)");
    } else {
        write_value_stateful(&env, out, state);
    }
    if let Some(doc_value) = value.closure_doc_value()
        && !doc_value.is_nil()
    {
        out.push_str(" nil ");
        if doc_value.is_string() {
            let ls = doc_value.as_lisp_string().unwrap();
            out.push_str(&format_lisp_string_emacs(ls, &PrintOptions::default()));
        } else {
            write_value_stateful(&doc_value, out, state);
        }
    }
    out.push(']');
}

fn write_lambda_stateful(value: &Value, out: &mut String, state: &mut PrintState) {
    with_lambda_print_guard(value, out, state, |out, state| {
        with_default_cycle_guard(value, out, state, |out, state| {
            if value.closure_env().flatten().is_some() {
                write_interpreted_closure_stateful(value, out, state);
            } else if let Some(list_form) = crate::emacs_core::builtins::lambda_to_cons_list(value)
            {
                write_value_stateful(&list_form, out, state);
            } else {
                let params = value
                    .closure_params()
                    .map_or_else(|| "nil".to_string(), format_params);
                write!(out, "(lambda {} ", params).unwrap();
                if let Some(body) = value.closure_body_value() {
                    write_closure_body_forms_stateful(body, out, state);
                } else {
                    out.push_str("nil");
                }
                out.push(')');
            }
        });
    });
}

fn write_macro_stateful(value: &Value, out: &mut String, state: &mut PrintState) {
    with_default_cycle_guard(value, out, state, |out, state| {
        let params = value
            .closure_params()
            .map_or_else(|| "nil".to_string(), format_params);
        write!(out, "(macro {} ", params).unwrap();
        if let Some(body) = value.closure_body_value() {
            write_closure_body_forms_stateful(body, out, state);
        } else {
            out.push_str("nil");
        }
        out.push(')');
    });
}

fn append_bytecode_literal_bytes(value: &Value, out: &mut Vec<u8>, options: PrintOptions) {
    if with_bytecode_literal_slots(value, |slots| {
        out.extend_from_slice(b"#[");
        for (idx, item) in slots.iter().enumerate() {
            if idx > 0 {
                out.push(b' ');
            }
            append_print_value_bytes(item, out, options);
        }
        out.push(b']');
    })
    .is_none()
    {
        out.extend_from_slice(b"#<bytecode>");
    }
}

/// Try to produce a shorthand form (quote, function, backquote, etc.) using
/// stateful printing. Returns `Some(string)` on success.
fn write_list_shorthand_stateful(value: &Value, state: &mut PrintState) -> Option<String> {
    let items = list_to_vec(value)?;
    if items.len() != 2 {
        return None;
    }

    let head = match items[0].kind() {
        ValueKind::Symbol(id) => resolve_sym(id),
        _ => return None,
    };

    if head == "make-hash-table-from-literal" {
        if let Some(payload) = quote_payload_stateful(&items[1]) {
            let mut out = String::from("#s");
            write_value_stateful(&payload, &mut out, state);
            return Some(out);
        }
        return None;
    }

    let (prefix, nested_options) = match head {
        "quote" => ("'", state.options),
        "function" => ("#'", state.options),
        "`" => ("`", state.options.enter_backquote()),
        "," => {
            if !state.options.allow_unquote_shorthand() {
                return None;
            }
            (",", state.options.exit_backquote())
        }
        ",@" => {
            if !state.options.allow_unquote_shorthand() {
                return None;
            }
            (",@", state.options.exit_backquote())
        }
        _ => return None,
    };

    let saved_options = state.options;
    state.options = nested_options;
    let mut out = String::from(prefix);
    write_value_stateful(&items[1], &mut out, state);
    state.options = saved_options;
    Some(out)
}

fn quote_payload_stateful(value: &Value) -> Option<Value> {
    let items = list_to_vec(value)?;
    if items.len() != 2 {
        return None;
    }
    match items[0].kind() {
        ValueKind::Symbol(id) if resolve_sym(id) == "quote" => Some(items[1]),
        _ => None,
    }
}

/// Print a cons cell (list elements) with stateful print support.
fn write_cons_stateful(value: &Value, out: &mut String, state: &mut PrintState) {
    let mut cursor = *value;
    let mut first = true;
    let mut count: i64 = 0;
    let stack_len = state.object_stack.len();
    loop {
        match cursor.kind() {
            ValueKind::Cons => {
                if !first && state.circle.is_none() {
                    if let Some(index) = default_cycle_stack_index(&cursor, state) {
                        out.push_str(" . ");
                        write!(out, "#{index}").unwrap();
                        state.object_stack.truncate(stack_len);
                        return;
                    }
                    push_default_cycle_object(&cursor, state);
                }
                // Length check
                if let Some(length) = state.options.print_length {
                    if count >= length {
                        if !first {
                            out.push(' ');
                        }
                        out.push_str("...");
                        state.object_stack.truncate(stack_len);
                        return;
                    }
                }
                if !first {
                    out.push(' ');
                }
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();

                // Circle check on the cdr (for detecting shared tails)
                // But first, print the car
                write_value_stateful(&pair_car, out, state);
                cursor = pair_cdr;
                first = false;
                count += 1;

                // Check if cdr is a cons that has a circle label
                if cursor.is_cons() {
                    if let Some(table_value) = active_print_number_table(&state.options) {
                        if let Some((_key, entry)) =
                            get_print_number_table_entry(table_value, &cursor)
                            && print_number_entry_is_cdr_label(entry)
                        {
                            out.push_str(" . ");
                            write_value_stateful(&cursor, out, state);
                            return;
                        }
                    } else if let Some(ref circle) = state.circle
                        && let Some(key) = object_identity_key(&cursor)
                        && let Some(label) = circle.number_table.get(&key)
                        && *label != 0
                    {
                        // This cons is shared/circular -- print as dotted pair
                        out.push_str(" . ");
                        write_value_stateful(&cursor, out, state);
                        return;
                    }
                }
            }
            ValueKind::Nil => {
                state.object_stack.truncate(stack_len);
                return;
            }
            _ => {
                if !first {
                    out.push_str(" . ");
                }
                write_value_stateful(&cursor, out, state);
                state.object_stack.truncate(stack_len);
                return;
            }
        }
    }
}

/// Print a hash table with stateful support.
fn write_hash_table_stateful(value: &Value, out: &mut String, state: &mut PrintState) {
    let table = value.as_hash_table().unwrap().clone();
    out.push_str("#s(hash-table");

    match table.test {
        HashTableTest::Eq => out.push_str(" test eq"),
        HashTableTest::Equal => out.push_str(" test equal"),
        HashTableTest::Eql => {}
    }

    if let Some(ref weakness) = table.weakness {
        let name = match weakness {
            super::value::HashTableWeakness::Key => "key",
            super::value::HashTableWeakness::Value => "value",
            super::value::HashTableWeakness::KeyOrValue => "key-or-value",
            super::value::HashTableWeakness::KeyAndValue => "key-and-value",
        };
        out.push_str(" weakness ");
        out.push_str(name);
    }

    if !table.data.is_empty() {
        out.push_str(" data (");
        let mut first = true;
        let mut count: i64 = 0;
        for key in &table.insertion_order {
            if let Some(val) = table.data.get(key) {
                if let Some(length) = state.options.print_length {
                    if count >= length {
                        if !first {
                            out.push(' ');
                        }
                        out.push_str("...");
                        break;
                    }
                }
                if !first {
                    out.push(' ');
                }
                let key_val = super::hashtab::hash_key_to_visible_value(&table, key);
                write_value_stateful(&key_val, out, state);
                out.push(' ');
                write_value_stateful(val, out, state);
                first = false;
                count += 1;
            }
        }
        out.push(')');
    }

    out.push(')');
}

thread_local! {
    static PRINT_OBJECT_STACK: RefCell<Vec<PrintObjectRef>> = const { RefCell::new(Vec::new()) };
    static PRINT_BYTES_OBJECT_STACK: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrintObjectRef {
    Lambda(usize),
}

fn with_print_object_guard<R>(
    object: PrintObjectRef,
    on_cycle: impl FnOnce(usize) -> R,
    render: impl FnOnce() -> R,
) -> R {
    PRINT_OBJECT_STACK.with(|stack| {
        if let Some(index) = stack.borrow().iter().position(|entry| *entry == object) {
            return on_cycle(index);
        }

        stack.borrow_mut().push(object);
        let rendered = render();
        stack.borrow_mut().pop();
        rendered
    })
}

fn with_lambda_print_guard(
    value: &Value,
    out: &mut String,
    state: &mut PrintState,
    render: impl FnOnce(&mut String, &mut PrintState),
) {
    let object = PrintObjectRef::Lambda(value.0);
    PRINT_OBJECT_STACK.with(|stack| {
        if let Some(index) = stack.borrow().iter().position(|entry| *entry == object) {
            write!(out, "#{index}").unwrap();
            return;
        }

        stack.borrow_mut().push(object);
        render(out, state);
        stack.borrow_mut().pop();
    });
}

fn append_bytes_cycle_ref_if_any(value: &Value, out: &mut Vec<u8>) -> bool {
    let Some(key) = default_cycle_candidate_key(value) else {
        return false;
    };
    PRINT_BYTES_OBJECT_STACK.with(|stack| {
        if let Some(index) = stack.borrow().iter().position(|entry| *entry == key) {
            out.extend_from_slice(format!("#{index}").as_bytes());
            true
        } else {
            false
        }
    })
}

fn push_bytes_cycle_object(value: &Value) -> bool {
    let Some(key) = default_cycle_candidate_key(value) else {
        return false;
    };
    PRINT_BYTES_OBJECT_STACK.with(|stack| stack.borrow_mut().push(key));
    true
}

fn pop_bytes_cycle_object(pushed: bool) {
    if pushed {
        PRINT_BYTES_OBJECT_STACK.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

fn bytes_object_stack_len() -> usize {
    PRINT_BYTES_OBJECT_STACK.with(|stack| stack.borrow().len())
}

fn truncate_bytes_object_stack(len: usize) {
    PRINT_BYTES_OBJECT_STACK.with(|stack| stack.borrow_mut().truncate(len));
}

fn bytes_cycle_stack_index(value: &Value) -> Option<usize> {
    let key = default_cycle_candidate_key(value)?;
    PRINT_BYTES_OBJECT_STACK.with(|stack| stack.borrow().iter().position(|entry| *entry == key))
}

fn format_marker_handle(
    value: &Value,
    buffers: Option<&crate::buffer::BufferManager>,
) -> Option<String> {
    if !super::marker::is_marker(value) {
        return None;
    }

    if !value.is_marker() {
        return None;
    };
    let marker = value.as_marker_data().unwrap().clone();
    let buffer_name = marker
        .buffer
        .and_then(|buffer_id| buffers.and_then(|manager| manager.get(buffer_id)))
        .map(|buffer| buffer.name_runtime_string_owned());

    let mut out = String::from("#<marker ");
    if marker.insertion_type {
        out.push_str("(moves after insertion) ");
    }
    // T7: read authoritative charpos (1-based Lisp shape). A marker with
    // no buffer prints as "in no buffer"; otherwise include its current
    // chain-tracked position.
    if let Some(name) = buffer_name.as_deref() {
        out.push_str(&format!("at {} in {}", marker.charpos + 1, name));
    } else {
        out.push_str("in no buffer");
    }
    out.push('>');
    Some(out)
}

fn format_overlay_handle(
    value: &Value,
    buffers: Option<&crate::buffer::BufferManager>,
) -> Option<String> {
    if !value.is_overlay() {
        return None;
    };

    let overlay = value.as_overlay_data().unwrap().clone();
    let Some(buffer_id) = overlay.buffer else {
        return Some("#<overlay in no buffer>".to_string());
    };

    let Some(buffers) = buffers else {
        return Some(format!(
            "#<overlay from {} to {}>",
            overlay.start, overlay.end
        ));
    };

    let Some(buffer) = buffers.get(buffer_id) else {
        return Some("#<overlay in no buffer>".to_string());
    };

    Some(format!(
        "#<overlay from {} to {} in {}>",
        buffer.text.emacs_byte_to_char(overlay.start) + 1,
        buffer.text.emacs_byte_to_char(overlay.end) + 1,
        buffer.name_runtime_string_owned()
    ))
}

fn print_special_handle(
    value: &Value,
    buffers: Option<&crate::buffer::BufferManager>,
) -> Option<String> {
    super::terminal::pure::print_terminal_handle(value)
        .or_else(|| format_marker_handle(value, buffers))
        .or_else(|| format_overlay_handle(value, buffers))
}

fn format_frame_handle(id: u64) -> String {
    if id >= crate::window::FRAME_ID_BASE {
        let ordinal = id - crate::window::FRAME_ID_BASE + 1;
        format!("#<frame F{} 0x{:x}>", ordinal, id)
    } else {
        format!("#<frame {}>", id)
    }
}

fn format_lisp_propertized_string_emacs(
    ls: &crate::heap_types::LispString,
    runs: &[StringTextPropertyRun],
    options: PrintOptions,
) -> String {
    let mut out = String::from("#(");
    out.push_str(&format_lisp_string_emacs(ls, &options));
    for run in runs {
        out.push(' ');
        out.push_str(&run.start.to_string());
        out.push(' ');
        out.push_str(&run.end.to_string());
        out.push(' ');
        out.push_str(&print_value_with_options(&run.plist, options));
    }
    out.push(')');
    out
}

/// Print a `Value` as a Lisp string, with buffer-manager awareness for
/// proper buffer name / killed-buffer rendering.
pub fn print_value_with_buffers(value: &Value, buffers: &crate::buffer::BufferManager) -> String {
    print_value_with_buffers_and_options(value, buffers, PrintOptions::default())
}

pub fn print_value_with_buffers_and_options(
    value: &Value,
    buffers: &crate::buffer::BufferManager,
    options: PrintOptions,
) -> String {
    print_value_stateful_with_buffers(value, Some(buffers), options)
}

fn print_list_shorthand_with_buffers(
    value: &Value,
    buffers: &crate::buffer::BufferManager,
    options: PrintOptions,
) -> Option<String> {
    let items = list_to_vec(value)?;
    if items.len() != 2 {
        return None;
    }
    let head = match items[0].kind() {
        ValueKind::Symbol(id) => resolve_sym(id),
        _ => return None,
    };
    if head == "make-hash-table-from-literal" {
        if let Some(payload) = quote_payload(&items[1]) {
            return Some(format!(
                "#s{}",
                print_value_with_buffers_and_options(&payload, buffers, options)
            ));
        }
        return None;
    }
    let (prefix, nested_options) = match head {
        "quote" => ("'", options),
        "function" => ("#'", options),
        "`" => ("`", options.enter_backquote()),
        "," => {
            if !options.allow_unquote_shorthand() {
                return None;
            }
            (",", options.exit_backquote())
        }
        ",@" => {
            if !options.allow_unquote_shorthand() {
                return None;
            }
            (",@", options.exit_backquote())
        }
        _ => return None,
    };
    Some(format!(
        "{prefix}{}",
        print_value_with_buffers_and_options(&items[1], buffers, nested_options)
    ))
}

fn print_cons_with_buffers(
    value: &Value,
    out: &mut String,
    buffers: &crate::buffer::BufferManager,
    options: PrintOptions,
) {
    let mut cursor = *value;
    let mut first = true;
    loop {
        match cursor.kind() {
            ValueKind::Cons => {
                if !first {
                    out.push(' ');
                }
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                out.push_str(&print_value_with_buffers_and_options(
                    &pair_car, buffers, options,
                ));
                cursor = pair_cdr;
                first = false;
            }
            ValueKind::Nil => return,
            _ => {
                if !first {
                    out.push_str(" . ");
                }
                out.push_str(&print_value_with_buffers_and_options(
                    &cursor, buffers, options,
                ));
                return;
            }
        }
    }
}

/// Print a `Value` as a Lisp string.
pub fn print_value(value: &Value) -> String {
    print_value_with_options(value, PrintOptions::default())
}

pub fn print_value_with_options(value: &Value, options: PrintOptions) -> String {
    print_value_stateful(value, options)
}

/// Print a `Value` as a Lisp byte sequence.
///
/// This preserves non-UTF-8 byte payloads encoded via NeoVM string sentinels.
pub fn print_value_bytes(value: &Value) -> Vec<u8> {
    print_value_bytes_with_options(value, PrintOptions::default())
}

pub fn print_value_bytes_with_options(value: &Value, options: PrintOptions) -> Vec<u8> {
    let _print_guard = enter_print_call(&options);
    // Delegate to the stateful printer when circle/level/length are active.
    if options.print_circle || options.print_level.is_some() || options.print_length.is_some() {
        return print_value_stateful(value, options).into_bytes();
    }
    let mut out = Vec::new();
    append_print_value_bytes(value, &mut out, options);
    out
}

fn append_print_value_bytes(value: &Value, out: &mut Vec<u8>, options: PrintOptions) {
    if let Some(handle) = print_special_handle(value, None) {
        out.extend_from_slice(handle.as_bytes());
        return;
    }
    match value.kind() {
        ValueKind::Nil => out.extend_from_slice(b"nil"),
        ValueKind::T => out.extend_from_slice(b"t"),
        ValueKind::Fixnum(v) => out.extend_from_slice(v.to_string().as_bytes()),
        ValueKind::Float => out.extend_from_slice(format_float(value.xfloat()).as_bytes()),
        ValueKind::Symbol(id) => append_symbol_bytes(id, out, options),
        ValueKind::String => {
            let ls = value.as_lisp_string().unwrap();
            let str_bytes = format_lisp_string_bytes_emacs(ls, &options);
            if let Some(runs) = get_string_text_properties_for_value(*value) {
                out.extend_from_slice(b"#(");
                out.extend_from_slice(&str_bytes);
                for run in runs {
                    out.push(b' ');
                    out.extend_from_slice(run.start.to_string().as_bytes());
                    out.push(b' ');
                    out.extend_from_slice(run.end.to_string().as_bytes());
                    out.push(b' ');
                    append_print_value_bytes(&run.plist, out, options);
                }
                out.push(b')');
            } else {
                out.extend_from_slice(&str_bytes);
            }
        }
        ValueKind::Cons => {
            if append_bytes_cycle_ref_if_any(value, out) {
                return;
            }
            let pushed = push_bytes_cycle_object(value);
            if let Some(shorthand) = print_list_shorthand_bytes(value, options) {
                out.extend_from_slice(&shorthand);
                pop_bytes_cycle_object(pushed);
                return;
            }
            out.push(b'(');
            print_cons_bytes(value, out, options);
            out.push(b')');
            pop_bytes_cycle_object(pushed);
        }
        ValueKind::Veclike(VecLikeType::Vector) => {
            if let Some(nbits) = bool_vector_length(value) {
                append_bool_vector_bytes(value, nbits as usize, out);
                return;
            }
            if append_bytes_cycle_ref_if_any(value, out) {
                return;
            }
            let pushed = push_bytes_cycle_object(value);
            if let Some(slots) = char_table_external_slots(value) {
                out.extend_from_slice(b"#^[");
                for (idx, item) in slots.iter().enumerate() {
                    if idx > 0 {
                        out.push(b' ');
                    }
                    append_print_value_bytes(item, out, options);
                }
                out.push(b']');
                pop_bytes_cycle_object(pushed);
                return;
            }
            out.push(b'[');
            let items = value.as_vector_data().unwrap().clone();
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push(b' ');
                }
                append_print_value_bytes(item, out, options);
            }
            out.push(b']');
            pop_bytes_cycle_object(pushed);
        }
        ValueKind::Veclike(VecLikeType::Record) => {
            if append_bytes_cycle_ref_if_any(value, out) {
                return;
            }
            let pushed = push_bytes_cycle_object(value);
            out.extend_from_slice(b"#s(");
            let items = value.as_record_data().unwrap().clone();
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push(b' ');
                }
                append_print_value_bytes(item, out, options);
            }
            out.push(b')');
            pop_bytes_cycle_object(pushed);
        }
        ValueKind::Veclike(VecLikeType::HashTable) => {
            if append_bytes_cycle_ref_if_any(value, out) {
                return;
            }
            let pushed = push_bytes_cycle_object(value);
            append_hash_table_bytes(value, out, options);
            pop_bytes_cycle_object(pushed);
        }
        ValueKind::Veclike(VecLikeType::Lambda) => {
            if append_bytes_cycle_ref_if_any(value, out) {
                return;
            }
            let pushed = push_bytes_cycle_object(value);
            let text = with_print_object_guard(
                PrintObjectRef::Lambda(value.0),
                |index| format!("#{index}"),
                || {
                    if value.closure_env().flatten().is_some() {
                        format_interpreted_closure(value, options)
                    } else {
                        if let Some(list_form) =
                            crate::emacs_core::builtins::lambda_to_cons_list(value)
                        {
                            return print_value_with_options(&list_form, options);
                        }
                        let params = value
                            .closure_params()
                            .map_or_else(|| "nil".to_string(), format_params);
                        let body = value
                            .closure_body_value()
                            .map(|body| format_closure_body_forms(body, options))
                            .unwrap_or_else(|| "nil".to_string());
                        format!("(lambda {} {})", params, body)
                    }
                },
            );
            out.extend_from_slice(text.as_bytes());
            pop_bytes_cycle_object(pushed);
        }
        ValueKind::Veclike(VecLikeType::Macro) => {
            if append_bytes_cycle_ref_if_any(value, out) {
                return;
            }
            let pushed = push_bytes_cycle_object(value);
            let params = value
                .closure_params()
                .map_or_else(|| "nil".to_string(), format_params);
            let body = value
                .closure_body_value()
                .map(|body| format_closure_body_forms(body, options))
                .unwrap_or_else(|| "nil".to_string());
            out.extend_from_slice(format!("(macro {} {})", params, body).as_bytes());
            pop_bytes_cycle_object(pushed);
        }
        ValueKind::Subr(id) => {
            out.extend_from_slice(format!("#<subr {}>", resolve_sym(id)).as_bytes())
        }
        ValueKind::Veclike(VecLikeType::Subr) => {
            let id = value.as_subr_id().unwrap();
            out.extend_from_slice(format!("#<subr {}>", resolve_sym(id)).as_bytes())
        }
        ValueKind::Veclike(VecLikeType::ByteCode) => {
            if append_bytes_cycle_ref_if_any(value, out) {
                return;
            }
            let pushed = push_bytes_cycle_object(value);
            append_bytecode_literal_bytes(value, out, options);
            pop_bytes_cycle_object(pushed);
        }
        ValueKind::Veclike(VecLikeType::Marker) => out.extend_from_slice(
            print_special_handle(value, None)
                .unwrap_or_else(|| "#<marker>".to_string())
                .as_bytes(),
        ),
        ValueKind::Veclike(VecLikeType::Overlay) => out.extend_from_slice(
            print_special_handle(value, None)
                .unwrap_or_else(|| "#<overlay>".to_string())
                .as_bytes(),
        ),
        ValueKind::Veclike(VecLikeType::Buffer) => {
            out.extend_from_slice(
                format!("#<buffer {}>", value.as_buffer_id().unwrap().0).as_bytes(),
            );
        }
        ValueKind::Veclike(VecLikeType::Window) => {
            out.extend_from_slice(
                format!("#<window {}>", value.as_window_id().unwrap()).as_bytes(),
            );
        }
        ValueKind::Veclike(VecLikeType::Frame) => {
            out.extend_from_slice(format_frame_handle(value.as_frame_id().unwrap()).as_bytes());
        }
        ValueKind::Veclike(VecLikeType::Timer) => {
            out.extend_from_slice(format!("#<timer {}>", value.as_timer_id().unwrap()).as_bytes());
        }
        ValueKind::Veclike(VecLikeType::Bignum) => {
            out.extend_from_slice(value.as_bignum().unwrap().to_string().as_bytes());
        }
        ValueKind::Veclike(VecLikeType::SymbolWithPos) => {
            // GNU prints symbol-with-pos as the bare symbol name.
            // Full implementation in Task 7.
            if let Some(sym) = value.as_symbol_with_pos_sym() {
                append_print_value_bytes(&sym, out, options);
            } else {
                out.extend_from_slice(b"#<symbol-with-pos>");
            }
        }
        ValueKind::Unbound => {
            out.extend_from_slice(b"#<unbound>");
        }
        ValueKind::Unknown => {
            out.extend_from_slice(format!("#<unknown {:#x}>", value.0).as_bytes());
        }
    }
}

fn format_symbol(id: super::intern::SymId, options: PrintOptions) -> String {
    String::from_utf8_lossy(&symbol_bytes(id, options)).into_owned()
}

fn append_symbol_bytes(id: super::intern::SymId, out: &mut Vec<u8>, options: PrintOptions) {
    out.extend_from_slice(&symbol_bytes(id, options));
}

fn format_symbol_name(name: &str) -> String {
    if name.is_empty() {
        return "##".to_string();
    }
    let mut out = String::with_capacity(name.len());
    for (idx, ch) in name.chars().enumerate() {
        let needs_escape = matches!(
            ch,
            ' ' | '\t'
                | '\n'
                | '\r'
                | '\u{0c}'
                | '('
                | ')'
                | '['
                | ']'
                | '"'
                | '\\'
                | ';'
                | '#'
                | '\''
                | '`'
                | ','
        ) || (idx == 0 && matches!(ch, '.' | '?'));
        if needs_escape {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn symbol_bytes(id: super::intern::SymId, options: PrintOptions) -> Vec<u8> {
    let name = resolve_sym_lisp_string(id);
    let canonical = lookup_interned_lisp_string(name);
    let mut out = Vec::new();
    if canonical == Some(id) {
        append_symbol_name_bytes(name, &mut out);
    } else if options.print_gensym {
        out.extend_from_slice(b"#:");
        if !name.is_empty() {
            append_symbol_name_bytes(name, &mut out);
        }
    } else {
        append_symbol_name_bytes(name, &mut out);
    }
    out
}

fn append_symbol_name_bytes(name: &crate::heap_types::LispString, out: &mut Vec<u8>) {
    let bytes = name.as_bytes();
    if bytes.is_empty() {
        out.extend_from_slice(b"##");
        return;
    }

    for (idx, byte) in bytes.iter().copied().enumerate() {
        let needs_escape = matches!(
            byte,
            b' ' | b'\t'
                | b'\n'
                | b'\r'
                | 0x0c
                | b'('
                | b')'
                | b'['
                | b']'
                | b'"'
                | b'\\'
                | b';'
                | b'#'
                | b'\''
                | b'`'
                | b','
        ) || (idx == 0 && matches!(byte, b'.' | b'?'));
        if needs_escape {
            out.push(b'\\');
        }
        if !name.is_multibyte() && byte >= 0x80 {
            let mut buf = [0u8; crate::emacs_core::emacs_char::MAX_MULTIBYTE_LENGTH];
            let len = crate::emacs_core::emacs_char::char_string(
                crate::emacs_core::emacs_char::byte8_to_char(byte),
                &mut buf,
            );
            out.extend_from_slice(&buf[..len]);
        } else {
            out.push(byte);
        }
    }
}

pub(crate) fn format_float(f: f64) -> String {
    const NAN_QUIET_BIT: u64 = 1u64 << 51;
    const NAN_PAYLOAD_MASK: u64 = (1u64 << 51) - 1;

    if f.is_nan() {
        let bits = f.to_bits();
        let frac = bits & ((1u64 << 52) - 1);
        if (frac & NAN_QUIET_BIT) != 0 {
            let payload = frac & NAN_PAYLOAD_MASK;
            return if f.is_sign_negative() {
                format!("-{}.0e+NaN", payload)
            } else {
                format!("{}.0e+NaN", payload)
            };
        }
        return if f.is_sign_negative() {
            "-0.0e+NaN".to_string()
        } else {
            "0.0e+NaN".to_string()
        };
    }
    if f.is_infinite() {
        return if f > 0.0 {
            "1.0e+INF".to_string()
        } else {
            "-1.0e+INF".to_string()
        };
    }
    format_float_dtoastr(f)
}

/// Format a finite float matching GNU Emacs's `dtoastr` / `float_to_string`:
/// use `%g`-style formatting with the minimum precision (starting from DBL_DIG=15)
/// that round-trips through strtod, then ensure a decimal point or exponent is present.
fn format_float_dtoastr(f: f64) -> String {
    let abs_f = f.abs();
    let start_prec = if abs_f != 0.0 && abs_f < f64::MIN_POSITIVE {
        1
    } else {
        15 // DBL_DIG
    };
    for prec in start_prec..=20 {
        // %g: uses %e if exponent < -4 or >= precision, otherwise %f.
        // %g also trims trailing zeros.
        let s = format!("{:.prec$e}", f, prec = prec - 1);
        // Parse back and check round-trip
        if let Ok(parsed) = s.parse::<f64>() {
            if parsed.to_bits() == f.to_bits() {
                // Convert from Rust's scientific notation to %g-style output
                return rust_sci_to_emacs_g(f, &s, prec);
            }
        }
    }
    // Fallback: maximum precision
    let s = format!("{:.20e}", f);
    rust_sci_to_emacs_g(f, &s, 21)
}

/// Convert Rust scientific notation string to GNU Emacs %g-style output.
/// %g rules: use fixed notation unless exponent >= precision or exponent < -4.
/// %g trims trailing zeros (but keeps at least one digit after decimal point
/// for Emacs's post-processing).
fn rust_sci_to_emacs_g(f: f64, sci: &str, prec: usize) -> String {
    // Parse the exponent from Rust's scientific notation (e.g., "3.14e2")
    let (mantissa_str, exp_str) = sci.split_once('e').unwrap_or((sci, "0"));
    let exp: i32 = exp_str.parse().unwrap_or(0);

    // %g uses fixed notation when -4 <= exp < prec
    let result = if exp >= -4 && exp < prec as i32 {
        // Fixed notation
        format_g_fixed(f, mantissa_str, exp, prec)
    } else {
        // Scientific notation with Emacs-style exponent formatting
        format_g_scientific(mantissa_str, exp, prec)
    };

    // Emacs post-processing: ensure decimal point or exponent is present
    ensure_decimal_point(result)
}

/// Format as fixed-point notation for %g, trimming trailing zeros.
fn format_g_fixed(f: f64, _mantissa: &str, exp: i32, prec: usize) -> String {
    // %g precision = total significant digits.
    // digits_after_dot = prec - exp - 1 (works for both positive and negative exp)
    let digits_after_dot = (prec as i32 - exp - 1).max(0) as usize;
    let s = format!("{:.digits$}", f, digits = digits_after_dot);
    trim_trailing_zeros_g(&s)
}

/// Format as scientific notation for %g, trimming trailing zeros.
fn format_g_scientific(mantissa: &str, exp: i32, _prec: usize) -> String {
    // Trim trailing zeros from mantissa
    let trimmed = trim_trailing_zeros_g(mantissa);
    // Emacs uses e+XX / e-XX with at least 2-digit exponent for |exp| < 100,
    // but %g in glibc actually uses minimal digits. Let's match C's %g.
    if exp >= 0 {
        format!("{}e+{:02}", trimmed, exp)
    } else {
        format!("{}e-{:02}", trimmed, -exp)
    }
}

/// Trim trailing zeros after decimal point (%g style).
/// "3.1400" -> "3.14", "3.0000" -> "3", "100" -> "100"
fn trim_trailing_zeros_g(s: &str) -> String {
    if !s.contains('.') {
        return s.to_string();
    }
    let trimmed = s.trim_end_matches('0');
    let trimmed = trimmed.trim_end_matches('.');
    trimmed.to_string()
}

/// Ensure the output has a decimal point with trailing digit (Emacs requirement).
/// If no decimal point or exponent, append ".0".
fn ensure_decimal_point(mut s: String) -> String {
    // Check if there's already a decimal point or exponent
    let has_dot_or_exp = s.bytes().any(|b| b == b'.' || b == b'e' || b == b'E');
    if !has_dot_or_exp {
        s.push_str(".0");
    } else if s.ends_with('.') {
        s.push('0');
    }
    s
}

fn format_params(params: &super::value::LambdaParams) -> String {
    let mut parts = Vec::new();
    for p in &params.required {
        parts.push(resolve_sym(*p).to_string());
    }
    if !params.optional.is_empty() {
        parts.push("&optional".to_string());
        for p in &params.optional {
            parts.push(resolve_sym(*p).to_string());
        }
    }
    if let Some(rest) = params.rest {
        parts.push("&rest".to_string());
        parts.push(resolve_sym(rest).to_string());
    }
    if parts.is_empty() {
        "nil".to_string()
    } else {
        format!("({})", parts.join(" "))
    }
}

fn format_closure_body_forms(body: Value, options: PrintOptions) -> String {
    let Some(forms) = list_to_vec(&body) else {
        return print_value_with_options(&body, options);
    };
    if forms.is_empty() {
        "nil".to_string()
    } else {
        forms
            .iter()
            .map(|form| print_value_with_options(form, options))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn format_interpreted_closure(value: &Value, options: PrintOptions) -> String {
    let mut slots = Vec::with_capacity(5);
    slots.push(
        value
            .closure_params()
            .map_or_else(|| "nil".to_string(), format_params),
    );
    slots.push(
        value
            .closure_body_value()
            .map(|body| print_value_with_options(&body, options))
            .unwrap_or_else(|| "nil".to_string()),
    );
    let env = value.closure_env().flatten().expect("closure env");
    slots.push(if env == Value::NIL {
        "(t)".to_string()
    } else {
        print_value_with_options(&env, options)
    });
    if let Some(doc_value) = value.closure_doc_value()
        && !doc_value.is_nil()
    {
        slots.push("nil".to_string());
        slots.push(if doc_value.is_string() {
            let ls = doc_value.as_lisp_string().unwrap();
            format_lisp_string_emacs(ls, &PrintOptions::default())
        } else {
            print_value_with_options(&doc_value, options)
        });
    }
    format!("#[{}]", slots.join(" "))
}

fn print_list_shorthand(value: &Value, options: PrintOptions) -> Option<String> {
    let items = list_to_vec(value)?;
    if items.len() != 2 {
        return None;
    }

    let head = match items[0].kind() {
        ValueKind::Symbol(id) => resolve_sym(id),
        _ => return None,
    };

    if head == "make-hash-table-from-literal" {
        if let Some(payload) = quote_payload(&items[1]) {
            return Some(format!("#s{}", print_value_with_options(&payload, options)));
        }
        return None;
    }

    let (prefix, nested_options) = match head {
        "quote" => ("'", options),
        "function" => ("#'", options),
        "`" => ("`", options.enter_backquote()),
        "," => {
            if !options.allow_unquote_shorthand() {
                return None;
            }
            (",", options.exit_backquote())
        }
        ",@" => {
            if !options.allow_unquote_shorthand() {
                return None;
            }
            (",@", options.exit_backquote())
        }
        _ => return None,
    };

    Some(format!(
        "{prefix}{}",
        print_value_with_options(&items[1], nested_options)
    ))
}

fn print_list_shorthand_bytes(value: &Value, options: PrintOptions) -> Option<Vec<u8>> {
    let items = list_to_vec(value)?;
    if items.len() != 2 {
        return None;
    }

    let head = match items[0].kind() {
        ValueKind::Symbol(id) => resolve_sym(id),
        _ => return None,
    };

    if head == "make-hash-table-from-literal" {
        let payload = quote_payload(&items[1])?;
        let mut out = Vec::new();
        out.extend_from_slice(b"#s");
        append_print_value_bytes(&payload, &mut out, options);
        return Some(out);
    }

    let (prefix, nested_options): (&[u8], PrintOptions) = match head {
        "quote" => (b"'", options),
        "function" => (b"#'", options),
        "`" => (b"`", options.enter_backquote()),
        "," => {
            if !options.allow_unquote_shorthand() {
                return None;
            }
            (b",", options.exit_backquote())
        }
        ",@" => {
            if !options.allow_unquote_shorthand() {
                return None;
            }
            (b",@", options.exit_backquote())
        }
        _ => return None,
    };

    let mut out = Vec::new();
    out.extend_from_slice(prefix);
    append_print_value_bytes(&items[1], &mut out, nested_options);
    Some(out)
}

fn quote_payload(value: &Value) -> Option<Value> {
    let items = list_to_vec(value)?;
    if items.len() != 2 {
        return None;
    }
    match items[0].kind() {
        ValueKind::Symbol(id) if resolve_sym(id) == "quote" => Some(items[1]),
        _ => None,
    }
}

fn print_cons(value: &Value, out: &mut String, options: PrintOptions) {
    let mut cursor = *value;
    let mut first = true;
    loop {
        match cursor.kind() {
            ValueKind::Cons => {
                if !first {
                    out.push(' ');
                }
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                out.push_str(&print_value_with_options(&pair_car, options));
                cursor = pair_cdr;
                first = false;
            }
            ValueKind::Nil => return,
            _ => {
                if !first {
                    out.push_str(" . ");
                }
                out.push_str(&print_value_with_options(&cursor, options));
                return;
            }
        }
    }
}

fn print_cons_bytes(value: &Value, out: &mut Vec<u8>, options: PrintOptions) {
    let mut cursor = *value;
    let mut first = true;
    let stack_len = bytes_object_stack_len();
    loop {
        match cursor.kind() {
            ValueKind::Cons => {
                if !first {
                    if let Some(index) = bytes_cycle_stack_index(&cursor) {
                        out.extend_from_slice(b" . ");
                        out.extend_from_slice(format!("#{index}").as_bytes());
                        truncate_bytes_object_stack(stack_len);
                        return;
                    }
                    push_bytes_cycle_object(&cursor);
                }
                if !first {
                    out.push(b' ');
                }
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                append_print_value_bytes(&pair_car, out, options);
                cursor = pair_cdr;
                first = false;
            }
            ValueKind::Nil => {
                truncate_bytes_object_stack(stack_len);
                return;
            }
            _ => {
                if !first {
                    out.extend_from_slice(b" . ");
                }
                append_print_value_bytes(&cursor, out, options);
                truncate_bytes_object_stack(stack_len);
                return;
            }
        }
    }
}
// -- Bool-vector printing ---------------------------------------------------

/// Format a bool-vector as `#&N"..."`.
fn format_bool_vector(value: &Value, nbits: usize) -> String {
    let mut out = Vec::new();
    append_bool_vector_bytes(value, nbits, &mut out);
    String::from_utf8_lossy(&out).into_owned()
}

/// Append bool-vector bytes as `#&N"..."`.
fn append_bool_vector_bytes(value: &Value, nbits: usize, out: &mut Vec<u8>) {
    let items = match value.kind() {
        ValueKind::Veclike(VecLikeType::Vector) => value.as_vector_data().unwrap().clone(),
        _ => return,
    };
    // items[0] = tag, items[1] = size, items[2..] = individual bit values
    let nbytes = (nbits + 7) / 8;

    out.extend_from_slice(b"#&");
    out.extend_from_slice(nbits.to_string().as_bytes());
    out.push(b'"');

    for byte_idx in 0..nbytes {
        let mut byte_val: u8 = 0;
        for bit_idx in 0..8 {
            let overall_bit = byte_idx * 8 + bit_idx;
            if overall_bit >= nbits {
                break;
            }
            let is_set = match items.get(2 + overall_bit) {
                Some(v) => match v.kind() {
                    ValueKind::Fixnum(n) => n != 0,
                    _ => v.is_truthy(),
                },
                None => false,
            };
            if is_set {
                byte_val |= 1 << bit_idx; // LSB first
            }
        }
        match byte_val {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b if b > 0x7F => {
                // Octal escape for high bytes, matching GNU Emacs
                out.extend_from_slice(format!("\\{:03o}", b).as_bytes());
            }
            _ => out.push(byte_val),
        }
    }

    out.push(b'"');
}

// -- Hash-table printing ----------------------------------------------------

fn format_hash_table(value: &Value, options: PrintOptions) -> String {
    let table = value.as_hash_table().unwrap().clone();
    let mut out = String::from("#s(hash-table");

    // GNU Emacs omits test when it's eql (the default).
    match table.test {
        HashTableTest::Eq => out.push_str(" test eq"),
        HashTableTest::Equal => out.push_str(" test equal"),
        HashTableTest::Eql => {} // default, omitted
    }

    // GNU Emacs omits weakness when there is none.
    if let Some(ref weakness) = table.weakness {
        let name = match weakness {
            super::value::HashTableWeakness::Key => "key",
            super::value::HashTableWeakness::Value => "value",
            super::value::HashTableWeakness::KeyOrValue => "key-or-value",
            super::value::HashTableWeakness::KeyAndValue => "key-and-value",
        };
        out.push_str(" weakness ");
        out.push_str(name);
    }

    // GNU Emacs omits data when the table is empty.
    if !table.data.is_empty() {
        out.push_str(" data (");
        let mut first = true;
        for key in &table.insertion_order {
            if let Some(val) = table.data.get(key) {
                if !first {
                    out.push(' ');
                }
                let key_val = super::hashtab::hash_key_to_visible_value(&table, key);
                out.push_str(&print_value_with_options(&key_val, options));
                out.push(' ');
                out.push_str(&print_value_with_options(val, options));
                first = false;
            }
        }
        out.push(')');
    }

    out.push(')');
    out
}

fn append_hash_table_bytes(value: &Value, out: &mut Vec<u8>, options: PrintOptions) {
    let table = value.as_hash_table().unwrap().clone();
    out.extend_from_slice(b"#s(hash-table");

    match table.test {
        HashTableTest::Eq => out.extend_from_slice(b" test eq"),
        HashTableTest::Equal => out.extend_from_slice(b" test equal"),
        HashTableTest::Eql => {}
    }

    if let Some(ref weakness) = table.weakness {
        let name = match weakness {
            super::value::HashTableWeakness::Key => "key",
            super::value::HashTableWeakness::Value => "value",
            super::value::HashTableWeakness::KeyOrValue => "key-or-value",
            super::value::HashTableWeakness::KeyAndValue => "key-and-value",
        };
        out.extend_from_slice(b" weakness ");
        out.extend_from_slice(name.as_bytes());
    }

    if !table.data.is_empty() {
        out.extend_from_slice(b" data (");
        let mut first = true;
        for key in &table.insertion_order {
            if let Some(val) = table.data.get(key) {
                if !first {
                    out.push(b' ');
                }
                let key_val = super::hashtab::hash_key_to_visible_value(&table, key);
                append_print_value_bytes(&key_val, out, options);
                out.push(b' ');
                append_print_value_bytes(val, out, options);
                first = false;
            }
        }
        out.push(b')');
    }

    out.push(b')');
}

#[cfg(test)]
#[path = "print_test.rs"]
mod tests;
