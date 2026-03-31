//! Text property and overlay builtins for the Elisp interpreter.
//!
//! Bridges the buffer's `TextPropertyTable` and `OverlayList` to Elisp
//! functions like `put-text-property`, `make-overlay`, etc.

use super::builtins::builtin_copy_sequence;
use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::string_escape::{storage_byte_to_char, storage_char_len, storage_char_to_byte};
use super::symbol::Obarray;
use super::value::*;
use crate::buffer::overlay::plist_put_eq;
use crate::buffer::text_props::TextPropertyTable;
use crate::buffer::{BufferId, BufferManager};

pub(crate) fn init_textprop_vars(
    obarray: &mut crate::emacs_core::symbol::Obarray,
    custom: &mut crate::emacs_core::custom::CustomManager,
) {
    obarray.set_symbol_value("default-text-properties", Value::NIL);
    obarray.make_special("default-text-properties");

    obarray.set_symbol_value("char-property-alias-alist", Value::NIL);
    obarray.make_special("char-property-alias-alist");

    obarray.set_symbol_value("inhibit-point-motion-hooks", Value::T);
    obarray.make_special("inhibit-point-motion-hooks");

    obarray.set_symbol_value(
        "text-property-default-nonsticky",
        Value::list(vec![
            Value::cons(Value::symbol("syntax-table"), Value::T),
            Value::cons(Value::symbol("display"), Value::T),
        ]),
    );
    obarray.make_special("text-property-default-nonsticky");
    custom.make_variable_buffer_local("text-property-default-nonsticky");
    obarray.make_buffer_local("text-property-default-nonsticky", true);
}

// ---------------------------------------------------------------------------
// Helpers (local to this module)
// ---------------------------------------------------------------------------

fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_max_args(name: &str, args: &[Value], max: usize) -> Result<(), Flow> {
    if args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_int(value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        ValueKind::Char(c) => Ok(c as i64),
        marker if super::marker::is_marker(marker) => super::marker::marker_position_as_int(marker),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *value],
        )),
    }
}

fn expect_int_eval(eval: &super::eval::Context, value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        ValueKind::Char(c) => Ok(c as i64),
        marker if super::marker::is_marker(marker) => {
            super::marker::marker_position_as_int_eval(eval, marker)
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *value],
        )),
    }
}

fn expect_integer_or_marker(value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        ValueKind::Char(c) => Ok(c as i64),
        marker if super::marker::is_marker(marker) => super::marker::marker_position_as_int(marker),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *value],
        )),
    }
}

fn expect_integer_or_marker_eval(eval: &super::eval::Context, value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        ValueKind::Char(c) => Ok(c as i64),
        marker if super::marker::is_marker(marker) => {
            super::marker::marker_position_as_int_eval(eval, marker)
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *value],
        )),
    }
}

fn expect_integer_or_marker_in_buffers(
    buffers: &BufferManager,
    value: &Value,
) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        ValueKind::Char(c) => Ok(c as i64),
        marker if super::marker::is_marker(marker) => {
            super::marker::marker_position_as_int_with_buffers(buffers, marker)
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *value],
        )),
    }
}

/// Extract a symbol name (for property names).
pub(crate) fn expect_symbol_name(value: &Value) -> Result<String, Flow> {
    match value.as_symbol_name() {
        Some(s) => Ok(s.to_string()),
        None => match value {
            ValueKind::String => Ok(value.as_str().unwrap().to_string()),
            ValueKind::Keyword(id) => Ok(resolve_sym(*id).to_owned()),
            other => Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), *other],
            )),
        },
    }
}

pub fn register_bootstrap_vars(obarray: &mut crate::emacs_core::symbol::Obarray) {
    obarray.set_symbol_value("default-text-properties", Value::NIL);
    obarray.set_symbol_value("char-property-alias-alist", Value::NIL);
    obarray.set_symbol_value("inhibit-point-motion-hooks", Value::T);
    obarray.set_symbol_value(
        "text-property-default-nonsticky",
        Value::list(vec![
            Value::cons(Value::symbol("syntax-table"), Value::T),
            Value::cons(Value::symbol("display"), Value::T),
        ]),
    );
}

fn current_textprop_variable_value(
    obarray: &Obarray,
    buffers: &BufferManager,
    name: &str,
) -> Option<Value> {
    if let Some(buf) = buffers.current_buffer()
        && let Some(binding) = buf.get_buffer_local_binding(name)
    {
        return binding.as_value();
    }
    obarray.symbol_value(name).copied()
}

fn plist_get_named_value(plist: Value, prop_name: &str) -> Option<Value> {
    let mut tail = plist;
    loop {
        if !tail.is_cons() {
            return None;
        };
        let pair = read_cons(cell);  // TODO(tagged): replace read_cons with cons accessors
        if !pair.cdr.is_cons() {
            return None;
        };
        if pair.car.as_symbol_name() == Some(prop_name) {
            return Some(read_cons(value_cell).car);  // TODO(tagged): replace read_cons with cons accessors
        }
        tail = read_cons(value_cell).cdr;  // TODO(tagged): replace read_cons with cons accessors
    }
}

fn assq_rest(list: Value, prop_name: &str) -> Option<Value> {
    let mut cursor = list;
    while cursor.is_cons() {
        let pair = read_cons(cell);  // TODO(tagged): replace read_cons with cons accessors
        if pair.car.is_cons() {
            let entry = read_cons(entry_cell);  // TODO(tagged): replace read_cons with cons accessors
            if entry.car.as_symbol_name() == Some(prop_name) {
                return Some(entry.cdr);
            }
        }
        cursor = pair.cdr;
    }
    None
}

fn lookup_char_property_from_direct<F>(
    obarray: &Obarray,
    buffers: &BufferManager,
    mut direct_get: F,
    prop: &str,
    textprop: bool,
) -> Value
where
    F: FnMut(&str) -> Option<Value>,
{
    if let Some(value) = direct_get(prop) {
        return value;
    }

    let mut fallback = Value::NIL;

    if let Some(category) = direct_get("category")
        && let Some(category_name) = category.as_symbol_name()
        && let Some(value) = obarray.get_property(category_name, prop).copied()
    {
        fallback = value;
    }

    if !fallback.is_nil() {
        return fallback;
    }

    if let Some(aliases) =
        current_textprop_variable_value(obarray, buffers, "char-property-alias-alist")
            .and_then(|value| assq_rest(value, prop))
    {
        let mut cursor = aliases;
        while cursor.is_cons() {
            let pair = read_cons(cell);  // TODO(tagged): replace read_cons with cons accessors
            if let Some(alias_name) = pair.car.as_symbol_name()
                && let Some(value) = direct_get(alias_name)
                && !value.is_nil()
            {
                return value;
            }
            cursor = pair.cdr;
        }
    }

    if textprop
        && let Some(defaults) =
            current_textprop_variable_value(obarray, buffers, "default-text-properties")
        && defaults.is_cons()
        && let Some(value) = plist_get_named_value(defaults, prop)
    {
        return value;
    }

    fallback
}

fn lookup_string_text_property(
    obarray: &Obarray,
    buffers: &BufferManager,
    table: &TextPropertyTable,
    byte_pos: usize,
    prop: &str,
) -> Value {
    lookup_char_property_from_direct(
        obarray,
        buffers,
        |name| table.get_property(byte_pos, name).copied(),
        prop,
        true,
    )
}

pub(crate) fn lookup_buffer_text_property(
    obarray: &Obarray,
    buffers: &BufferManager,
    buf: &crate::buffer::buffer::Buffer,
    byte_pos: usize,
    prop: &str,
) -> Value {
    lookup_char_property_from_direct(
        obarray,
        buffers,
        |name| buf.text.text_props_get_property(byte_pos, name),
        prop,
        true,
    )
}

fn lookup_overlay_property(
    obarray: &Obarray,
    buffers: &BufferManager,
    overlay: crate::gc::ObjId,
    prop: &str,
) -> Value {
    lookup_char_property_from_direct(
        obarray,
        buffers,
        |name| with_heap(|h| plist_get_named_value(h.get_overlay(overlay).plist, name)),
        prop,
        false,
    )
}

/// Convert a 1-based Elisp char position to a 0-based byte position,
/// clamping within the buffer.
fn elisp_pos_to_byte(buf: &crate::buffer::buffer::Buffer, pos: i64) -> usize {
    let char_pos = if pos > 0 { pos as usize - 1 } else { 0 };
    let clamped = char_pos.min(buf.text.char_count());
    buf.text.char_to_byte(clamped)
}

/// Validate that BEG and END are within the buffer's accessible range.
/// GNU Emacs signals `args-out-of-range` if positions are outside [point-min, point-max].
fn validate_buffer_range(
    buf: &crate::buffer::buffer::Buffer,
    beg: i64,
    end: i64,
) -> Result<(), Flow> {
    let point_min = buf.point_min_char() as i64 + 1; // 1-based
    let point_max = buf.text.char_count() as i64 + 1; // 1-based, exclusive end
    if beg < point_min || beg > point_max || end < point_min || end > point_max {
        return Err(signal(
            "args-out-of-range",
            vec![Value::fixnum(beg), Value::fixnum(end)],
        ));
    }
    Ok(())
}

/// Convert a 0-based byte position to a 1-based Elisp char position.
fn byte_to_elisp_pos(buf: &crate::buffer::buffer::Buffer, byte_pos: usize) -> i64 {
    buf.text.byte_to_char(byte_pos) as i64 + 1
}

/// Resolve the optional OBJECT argument to a buffer id.
/// If nil or absent, uses the current buffer.
fn resolve_buffer_id(
    eval: &super::eval::Context,
    object: Option<&Value>,
) -> Result<BufferId, Flow> {
    resolve_buffer_id_in_buffers(&eval.buffers, object)
}

fn resolve_buffer_id_in_buffers(
    buffers: &BufferManager,
    object: Option<&Value>,
) -> Result<BufferId, Flow> {
    match object {
        None | Some(ValueKind::Nil) => buffers
            .current_buffer()
            .map(|b| b.id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")])),
        Some(ValueKind::Veclike(VecLikeType::Buffer)) => Ok(*id),
        Some(other) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("bufferp"), *other],
        )),
    }
}

fn current_buffer_id_in_buffers(buffers: &BufferManager) -> Result<BufferId, Flow> {
    buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))
}

fn expect_overlay(value: &Value) -> Result<crate::gc::ObjId, Flow> {
    match value.kind() {
        ValueKind::Veclike(VecLikeType::Overlay) => Ok(*id),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("overlayp"), *value],
        )),
    }
}

fn resolve_overlay_buffer_id(overlay: crate::gc::ObjId) -> Option<BufferId> {
    with_heap(|h| h.get_overlay(overlay).buffer)
}

fn ensure_marker_points_into_buffer(
    buffers: &BufferManager,
    value: &Value,
    buffer_id: BufferId,
) -> Result<(), Flow> {
    let Some((Some(marker_buffer_id), _, _)) = super::marker::marker_logical_fields(value) else {
        return Ok(());
    };
    if buffers.get(marker_buffer_id).is_none() {
        return Ok(());
    }
    if marker_buffer_id == buffer_id {
        return Ok(());
    }
    Err(signal(
        "error",
        vec![Value::string("Marker points into wrong buffer"), *value],
    ))
}

/// Check if the OBJECT argument is a string.  Returns Some(ObjId) if so.
pub(crate) fn is_string_object(object: Option<&Value>) -> Option<crate::gc::types::ObjId> {
    match object {
        Some(ValueKind::String) => Some(*id),
        _ => None,
    }
}

/// Convert a 0-based Elisp string char position to a byte offset.
pub(crate) fn string_elisp_pos_to_byte(s: &str, pos: i64) -> usize {
    let char_pos = if pos < 0 { 0usize } else { pos as usize };
    let clamped = char_pos.min(storage_char_len(s));
    storage_char_to_byte(s, clamped)
}

/// Convert a byte offset to a 0-based Elisp string char position.
pub(crate) fn string_byte_to_elisp_pos(s: &str, byte_pos: usize) -> i64 {
    storage_byte_to_char(s, byte_pos) as i64
}

/// Write back a modified TextPropertyTable to string text properties.
pub(crate) fn save_string_props(id: crate::gc::types::ObjId, table: TextPropertyTable) {
    set_string_text_properties_table(id, table);
}

/// Iterate a plist (alternating key value key value ...) from a list or vec.
/// Returns pairs of (property-name, value).
fn plist_pairs(plist: &Value) -> Result<Vec<(String, Value)>, Flow> {
    let items = list_to_vec(plist)
        .ok_or_else(|| signal("wrong-type-argument", vec![Value::symbol("listp"), *plist]))?;
    if items.len() % 2 != 0 {
        return Err(signal(
            "error",
            vec![Value::string("Odd number of elements in property list")],
        ));
    }
    let mut pairs = Vec::new();
    for chunk in items.chunks(2) {
        let name = expect_symbol_name(&chunk[0])?;
        pairs.push((name, chunk[1]));
    }
    Ok(pairs)
}

/// Convert ordered property pairs to an Elisp plist.
/// Preserves the order from the property interval (matching GNU Emacs behavior).
fn ordered_pairs_to_plist(pairs: &[(String, Value)]) -> Value {
    let mut items = Vec::new();
    for (key, val) in pairs {
        items.push(Value::symbol(key.clone()));
        items.push(*val);
    }
    Value::list(items)
}

// ===========================================================================
// Text property builtins
// ===========================================================================

/// (put-text-property BEG END PROP VAL &optional OBJECT)
pub(crate) fn builtin_put_text_property(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_put_text_property_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_put_text_property_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("put-text-property", &args, 4)?;
    expect_max_args("put-text-property", &args, 5)?;
    let beg = expect_integer_or_marker_in_buffers(buffers, &args[0])?;
    let end = expect_integer_or_marker_in_buffers(buffers, &args[1])?;
    let prop = expect_symbol_name(&args[2])?;
    let val = args[3];

    if let Some(str_id) = is_string_object(args.get(4)) {
        let s = with_heap(|h| h.get_string(str_id).to_owned());
        let mut table = get_string_text_properties_table(str_id).unwrap_or_default();
        let byte_beg = string_elisp_pos_to_byte(&s, beg);
        let byte_end = string_elisp_pos_to_byte(&s, end);
        table.put_property(byte_beg, byte_end, &prop, val);
        save_string_props(str_id, table);
        return Ok(Value::NIL);
    }

    let buf_id = resolve_buffer_id_in_buffers(buffers, args.get(4))?;
    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let byte_beg = elisp_pos_to_byte(buf, beg);
    let byte_end = elisp_pos_to_byte(buf, end);
    let _ = buffers.put_buffer_text_property(buf_id, byte_beg, byte_end, &prop, val);
    Ok(Value::NIL)
}

/// (get-text-property POS PROP &optional OBJECT)
pub(crate) fn builtin_get_text_property(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_get_text_property_in_state(&eval.obarray, &eval.buffers, args)
}

pub(crate) fn builtin_get_text_property_in_state(
    obarray: &Obarray,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("get-text-property", &args, 2)?;
    expect_max_args("get-text-property", &args, 3)?;
    let pos = expect_integer_or_marker_in_buffers(buffers, &args[0])?;
    let prop = expect_symbol_name(&args[1])?;

    if let Some(str_id) = is_string_object(args.get(2)) {
        let s = with_heap(|h| h.get_string(str_id).to_owned());
        if let Some(table) = get_string_text_properties_table(str_id) {
            let byte_pos = string_elisp_pos_to_byte(&s, pos);
            return Ok(lookup_string_text_property(
                obarray, buffers, &table, byte_pos, &prop,
            ));
        }
        return Ok(lookup_char_property_from_direct(
            obarray,
            buffers,
            |_| None,
            &prop,
            true,
        ));
    }

    let buf_id = resolve_buffer_id_in_buffers(buffers, args.get(2))?;
    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let byte_pos = elisp_pos_to_byte(buf, pos);
    Ok(lookup_buffer_text_property(
        obarray, buffers, buf, byte_pos, &prop,
    ))
}

pub(crate) fn buffer_overlay_property_at_byte_pos(
    obarray: &Obarray,
    buffers: &BufferManager,
    buf: &crate::buffer::buffer::Buffer,
    byte_pos: usize,
    prop: &str,
) -> Option<(Value, crate::gc::ObjId)> {
    let mut overlays = buf.overlays.overlays_at(byte_pos);
    buf.overlays
        .sort_overlay_ids_by_priority_desc(&mut overlays);
    for overlay in overlays {
        let value = lookup_overlay_property(obarray, buffers, overlay, prop);
        if !value.is_nil() {
            return Some((value, overlay));
        }
    }
    None
}

pub(crate) fn buffer_overlay_property_for_inserted_char_at_byte_pos(
    buf: &crate::buffer::buffer::Buffer,
    byte_pos: usize,
    prop: &str,
) -> Option<(Value, crate::gc::ObjId)> {
    let overlay_id = buf
        .overlays
        .highest_priority_overlay_for_inserted_char(byte_pos, prop)?;
    let value = buf.overlays.overlay_get_named(overlay_id, prop)?;
    Some((value, overlay_id))
}

/// (get-char-property POS PROP &optional OBJECT)
/// For strings, same as get-text-property (no overlays).
pub(crate) fn builtin_get_char_property(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_get_char_property_in_state(&eval.obarray, &eval.buffers, args)
}

pub(crate) fn builtin_get_char_property_in_state(
    obarray: &Obarray,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("get-char-property", &args, 2)?;
    expect_max_args("get-char-property", &args, 3)?;
    let pos = expect_integer_or_marker_in_buffers(buffers, &args[0])?;
    let prop = expect_symbol_name(&args[1])?;

    if is_string_object(args.get(2)).is_some() {
        return builtin_get_text_property_in_state(obarray, buffers, args);
    }

    let buf_id = resolve_buffer_id_in_buffers(buffers, args.get(2))?;
    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;
    let byte_pos = elisp_pos_to_byte(buf, pos);

    if let Some((value, _overlay_id)) =
        buffer_overlay_property_at_byte_pos(obarray, buffers, buf, byte_pos, &prop)
    {
        return Ok(value);
    }

    Ok(lookup_buffer_text_property(
        obarray, buffers, buf, byte_pos, &prop,
    ))
}

/// (add-text-properties BEG END PROPS &optional OBJECT)
pub(crate) fn builtin_add_text_properties(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_add_text_properties_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_add_text_properties_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("add-text-properties", &args, 3)?;
    expect_max_args("add-text-properties", &args, 4)?;
    let beg = expect_integer_or_marker_in_buffers(buffers, &args[0])?;
    let end = expect_integer_or_marker_in_buffers(buffers, &args[1])?;
    let pairs = plist_pairs(&args[2])?;

    if let Some(str_id) = is_string_object(args.get(3)) {
        let s = with_heap(|h| h.get_string(str_id).to_owned());
        let mut table = get_string_text_properties_table(str_id).unwrap_or_default();
        let byte_beg = string_elisp_pos_to_byte(&s, beg);
        let byte_end = string_elisp_pos_to_byte(&s, end);
        let mut any_changed = false;
        for (name, val) in pairs {
            if table.put_property(byte_beg, byte_end, &name, val) {
                any_changed = true;
            }
        }
        save_string_props(str_id, table);
        return Ok(if any_changed { Value::T } else { Value::NIL });
    }

    let buf_id = resolve_buffer_id_in_buffers(buffers, args.get(3))?;
    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let byte_beg = elisp_pos_to_byte(buf, beg);
    let byte_end = elisp_pos_to_byte(buf, end);
    let mut any_changed = false;
    for (name, val) in pairs {
        if buffers
            .put_buffer_text_property(buf_id, byte_beg, byte_end, &name, val)
            .unwrap_or(false)
        {
            any_changed = true;
        }
    }
    Ok(if any_changed { Value::T } else { Value::NIL })
}

fn merge_face_property(existing: Option<Value>, new_face: Value, append: bool) -> Value {
    let Some(existing_value) = existing else {
        return new_face;
    };
    if existing_value.is_nil() {
        return new_face;
    }

    if let Some(mut items) = list_to_vec(&existing_value) {
        if append {
            items.push(new_face);
        } else {
            items.insert(0, new_face);
        }
        Value::list(items)
    } else if append {
        Value::list(vec![existing_value, new_face])
    } else {
        Value::list(vec![new_face, existing_value])
    }
}

/// `(add-face-text-property START END FACE &optional APPENDP OBJECT)`
pub(crate) fn builtin_add_face_text_property(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_add_face_text_property_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_add_face_text_property_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("add-face-text-property", &args, 3)?;
    expect_max_args("add-face-text-property", &args, 5)?;
    let beg = expect_integer_or_marker_in_buffers(buffers, &args[0])?;
    let end = expect_integer_or_marker_in_buffers(buffers, &args[1])?;
    let new_face = args[2];
    let append = args.get(3).is_some_and(|v| v.is_truthy());

    let object = args.get(4);

    if let Some(str_id) = is_string_object(object) {
        let s = with_heap(|h| h.get_string(str_id).to_owned());
        let mut table = get_string_text_properties_table(str_id).unwrap_or_default();
        let byte_beg = string_elisp_pos_to_byte(&s, beg);
        let byte_end = string_elisp_pos_to_byte(&s, end);
        let existing = table.get_property(byte_beg, "face").cloned();
        let merged = merge_face_property(existing, new_face, append);
        table.put_property(byte_beg, byte_end, "face", merged);
        save_string_props(str_id, table);
        return Ok(Value::NIL);
    }

    let buf_id = match object {
        None | Some(ValueKind::Nil) => buffers
            .current_buffer()
            .map(|b| b.id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")])),
        Some(ValueKind::Veclike(VecLikeType::Buffer)) => Ok(*id),
        Some(other) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("buffer-or-string-p"), *other],
        )),
    }?;

    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;
    let byte_beg = elisp_pos_to_byte(buf, beg);
    let byte_end = elisp_pos_to_byte(buf, end);
    let existing = buf.text.text_props_get_property(byte_beg, "face");
    let merged = merge_face_property(existing, new_face, append);
    let _ = buffers.put_buffer_text_property(buf_id, byte_beg, byte_end, "face", merged);
    Ok(Value::NIL)
}

/// (remove-text-properties BEG END PROPS &optional OBJECT)
pub(crate) fn builtin_remove_text_properties(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_remove_text_properties_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_remove_text_properties_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("remove-text-properties", &args, 3)?;
    expect_max_args("remove-text-properties", &args, 4)?;
    let beg = expect_integer_or_marker_in_buffers(buffers, &args[0])?;
    let end = expect_integer_or_marker_in_buffers(buffers, &args[1])?;
    let pairs = plist_pairs(&args[2])?;

    if let Some(str_id) = is_string_object(args.get(3)) {
        let s = with_heap(|h| h.get_string(str_id).to_owned());
        let mut table = get_string_text_properties_table(str_id).unwrap_or_default();
        let byte_beg = string_elisp_pos_to_byte(&s, beg);
        let byte_end = string_elisp_pos_to_byte(&s, end);
        let mut any_removed = false;
        for (name, _val) in pairs {
            if table.remove_property(byte_beg, byte_end, &name) {
                any_removed = true;
            }
        }
        save_string_props(str_id, table);
        return Ok(if any_removed { Value::T } else { Value::NIL });
    }

    let buf_id = resolve_buffer_id_in_buffers(buffers, args.get(3))?;
    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let byte_beg = elisp_pos_to_byte(buf, beg);
    let byte_end = elisp_pos_to_byte(buf, end);
    let mut any_removed = false;
    for (name, _val) in pairs {
        if buffers
            .remove_buffer_text_property(buf_id, byte_beg, byte_end, &name)
            .unwrap_or(false)
        {
            any_removed = true;
        }
    }
    Ok(if any_removed { Value::T } else { Value::NIL })
}

/// (set-text-properties BEG END PROPS &optional OBJECT)
pub(crate) fn builtin_set_text_properties(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_set_text_properties_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_set_text_properties_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("set-text-properties", &args, 3)?;
    expect_max_args("set-text-properties", &args, 4)?;
    let beg = expect_integer_or_marker_in_buffers(buffers, &args[0])?;
    let end = expect_integer_or_marker_in_buffers(buffers, &args[1])?;
    // set-text-properties accepts nil for PROPS (= remove all)
    let pairs = if args[2].is_nil() {
        Vec::new()
    } else {
        plist_pairs(&args[2])?
    };

    if let Some(str_id) = is_string_object(args.get(3)) {
        let s = with_heap(|h| h.get_string(str_id).to_owned());
        let mut table = get_string_text_properties_table(str_id).unwrap_or_default();
        let byte_beg = string_elisp_pos_to_byte(&s, beg);
        let byte_end = string_elisp_pos_to_byte(&s, end);
        table.remove_all_properties(byte_beg, byte_end);
        for (name, val) in pairs {
            table.put_property(byte_beg, byte_end, &name, val);
        }
        save_string_props(str_id, table);
        return Ok(Value::T);
    }

    let buf_id = resolve_buffer_id_in_buffers(buffers, args.get(3))?;
    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let byte_beg = elisp_pos_to_byte(buf, beg);
    let byte_end = elisp_pos_to_byte(buf, end);
    let _ = buffers.clear_buffer_text_properties(buf_id, byte_beg, byte_end);
    for (name, val) in pairs {
        let _ = buffers.put_buffer_text_property(buf_id, byte_beg, byte_end, &name, val);
    }
    Ok(Value::T)
}

/// (remove-list-of-text-properties BEG END LIST &optional OBJECT)
pub(crate) fn builtin_remove_list_of_text_properties(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_remove_list_of_text_properties_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_remove_list_of_text_properties_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("remove-list-of-text-properties", &args, 3)?;
    expect_max_args("remove-list-of-text-properties", &args, 4)?;
    let beg = expect_integer_or_marker_in_buffers(buffers, &args[0])?;
    let end = expect_integer_or_marker_in_buffers(buffers, &args[1])?;
    let names = list_to_vec(&args[2])
        .ok_or_else(|| signal("wrong-type-argument", vec![Value::symbol("listp"), args[2]]))?;

    if let Some(str_id) = is_string_object(args.get(3)) {
        let s = with_heap(|h| h.get_string(str_id).to_owned());
        let mut table = get_string_text_properties_table(str_id).unwrap_or_default();
        let byte_beg = string_elisp_pos_to_byte(&s, beg);
        let byte_end = string_elisp_pos_to_byte(&s, end);
        let mut changed = false;
        for name_val in names {
            let name = expect_symbol_name(&name_val)?;
            if table.get_property(byte_beg, &name).is_some() {
                changed = true;
            }
            table.remove_property(byte_beg, byte_end, &name);
        }
        save_string_props(str_id, table);
        return Ok(if changed { Value::T } else { Value::NIL });
    }

    let buf_id = resolve_buffer_id_in_buffers(buffers, args.get(3))?;
    let (byte_beg, byte_end) = {
        let buf = buffers
            .get(buf_id)
            .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;
        (elisp_pos_to_byte(buf, beg), elisp_pos_to_byte(buf, end))
    };

    let mut changed = false;
    for name_val in names {
        let name = expect_symbol_name(&name_val)?;
        let mut cursor = byte_beg;
        while cursor < byte_end {
            let Some(buf) = buffers.get(buf_id) else {
                break;
            };
            if buf.text.text_props_get_property(cursor, &name).is_some() {
                changed = true;
                break;
            }
            match buf.text.text_props_next_change(cursor) {
                Some(next) if next > cursor && next < byte_end => cursor = next,
                _ => break,
            }
        }
        let _ = buffers.remove_buffer_text_property(buf_id, byte_beg, byte_end, &name);
    }
    Ok(if changed { Value::T } else { Value::NIL })
}

/// (text-properties-at POS &optional OBJECT)
pub(crate) fn builtin_text_properties_at(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_text_properties_at_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_text_properties_at_in_buffers(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("text-properties-at", &args, 1)?;
    expect_max_args("text-properties-at", &args, 2)?;
    let pos = expect_integer_or_marker_in_buffers(buffers, &args[0])?;

    if let Some(str_id) = is_string_object(args.get(1)) {
        let s = with_heap(|h| h.get_string(str_id).to_owned());
        if let Some(table) = get_string_text_properties_table(str_id) {
            let byte_pos = string_elisp_pos_to_byte(&s, pos);
            let props = table.get_properties_ordered(byte_pos);
            return Ok(ordered_pairs_to_plist(&props));
        }
        return Ok(Value::NIL);
    }

    let buf_id = resolve_buffer_id_in_buffers(buffers, args.get(1))?;
    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let byte_pos = elisp_pos_to_byte(buf, pos);
    let props = buf.text.text_props_get_properties_ordered(byte_pos);
    Ok(ordered_pairs_to_plist(&props))
}

/// (next-single-property-change POS PROP &optional OBJECT LIMIT)
pub(crate) fn builtin_next_single_property_change(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_next_single_property_change_in_state(&eval.obarray, &eval.buffers, args)
}

pub(crate) fn builtin_next_single_property_change_in_state(
    obarray: &Obarray,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("next-single-property-change", &args, 2)?;
    expect_max_args("next-single-property-change", &args, 4)?;
    let pos = expect_integer_or_marker_in_buffers(buffers, &args[0])?;
    let prop = expect_symbol_name(&args[1])?;

    if let Some(str_id) = is_string_object(args.get(2)) {
        let s = with_heap(|h| h.get_string(str_id).to_owned());
        let table = get_string_text_properties_table(str_id).unwrap_or_default();
        let byte_pos = string_elisp_pos_to_byte(&s, pos);
        let (byte_limit, limit_val) = match args.get(3) {
            Some(v) if !v.is_nil() => {
                let lim_int = expect_int(v)?;
                (Some(string_elisp_pos_to_byte(&s, lim_int)), Some(lim_int))
            }
            _ => (None, None),
        };
        let current_val = lookup_string_text_property(obarray, buffers, &table, byte_pos, &prop);
        let str_len = s.len();
        let mut cursor = byte_pos;
        loop {
            match table.next_property_change(cursor) {
                Some(next) => {
                    if let Some(lim) = byte_limit {
                        if next >= lim {
                            return Ok(match limit_val {
                                Some(lv) => Value::fixnum(lv),
                                None => Value::NIL,
                            });
                        }
                    }
                    if next >= str_len {
                        break;
                    }
                    let new_val =
                        lookup_string_text_property(obarray, buffers, &table, next, &prop);
                    let changed = !equal_value(&current_val, &new_val, 0);
                    if changed {
                        return Ok(Value::fixnum(string_byte_to_elisp_pos(&s, next)));
                    }
                    cursor = next;
                }
                None => break,
            }
        }
        return Ok(match limit_val {
            Some(lv) => Value::fixnum(lv),
            None => Value::NIL,
        });
    }

    let buf_id = resolve_buffer_id_in_buffers(buffers, args.get(2))?;

    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let byte_pos = elisp_pos_to_byte(buf, pos);
    let (byte_limit, limit_val) = match args.get(3) {
        Some(v) if !v.is_nil() => {
            let lim_int = expect_int(v)?;
            (Some(elisp_pos_to_byte(buf, lim_int)), Some(lim_int))
        }
        _ => (None, None),
    };

    let current_val = lookup_buffer_text_property(obarray, buffers, buf, byte_pos, &prop);
    let buf_end = buf.point_max();
    let mut cursor = byte_pos;

    loop {
        match buf.text.text_props_next_change(cursor) {
            Some(next) => {
                if let Some(lim) = byte_limit {
                    if next >= lim {
                        return Ok(match limit_val {
                            Some(lv) => Value::fixnum(lv),
                            None => Value::NIL,
                        });
                    }
                }
                if next >= buf_end {
                    break;
                }
                let new_val = lookup_buffer_text_property(obarray, buffers, buf, next, &prop);
                let changed = !equal_value(&current_val, &new_val, 0);
                if changed {
                    return Ok(Value::fixnum(byte_to_elisp_pos(buf, next)));
                }
                cursor = next;
            }
            None => break,
        }
    }

    Ok(match limit_val {
        Some(lv) => Value::fixnum(lv),
        None => Value::NIL,
    })
}

/// (previous-single-property-change POS PROP &optional OBJECT LIMIT)
pub(crate) fn builtin_previous_single_property_change(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_previous_single_property_change_in_state(&eval.obarray, &eval.buffers, args)
}

pub(crate) fn builtin_previous_single_property_change_in_state(
    obarray: &Obarray,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("previous-single-property-change", &args, 2)?;
    expect_max_args("previous-single-property-change", &args, 4)?;
    let pos = expect_integer_or_marker_in_buffers(buffers, &args[0])?;
    let prop = expect_symbol_name(&args[1])?;

    if let Some(str_id) = is_string_object(args.get(2)) {
        let s = with_heap(|h| h.get_string(str_id).to_owned());
        let table = get_string_text_properties_table(str_id).unwrap_or_default();
        let byte_pos = string_elisp_pos_to_byte(&s, pos);
        let (byte_limit, limit_val) = match args.get(3) {
            Some(v) if !v.is_nil() => {
                let lim_int = expect_int(v)?;
                (Some(string_elisp_pos_to_byte(&s, lim_int)), Some(lim_int))
            }
            _ => (None, None),
        };
        let ref_byte = if byte_pos > 0 { byte_pos - 1 } else { 0 };
        let current_val = lookup_string_text_property(obarray, buffers, &table, ref_byte, &prop);
        let mut cursor = byte_pos;
        loop {
            match table.previous_property_change(cursor) {
                Some(prev) => {
                    if let Some(lim) = byte_limit {
                        if prev <= lim {
                            return Ok(match limit_val {
                                Some(lv) => Value::fixnum(lv),
                                None => Value::NIL,
                            });
                        }
                    }
                    let check = if prev > 0 { prev - 1 } else { 0 };
                    let new_val =
                        lookup_string_text_property(obarray, buffers, &table, check, &prop);
                    let changed = !equal_value(&current_val, &new_val, 0);
                    if changed {
                        return Ok(Value::fixnum(string_byte_to_elisp_pos(&s, prev)));
                    }
                    if prev == 0 {
                        break;
                    }
                    cursor = if prev < cursor { prev } else { prev - 1 };
                }
                None => break,
            }
        }
        return Ok(match limit_val {
            Some(lv) => Value::fixnum(lv),
            None => Value::NIL,
        });
    }

    let buf_id = resolve_buffer_id_in_buffers(buffers, args.get(2))?;

    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let byte_pos = elisp_pos_to_byte(buf, pos);
    let (byte_limit, limit_val) = match args.get(3) {
        Some(v) if !v.is_nil() => {
            let lim_int = expect_int(v)?;
            (Some(elisp_pos_to_byte(buf, lim_int)), Some(lim_int))
        }
        _ => (None, None),
    };

    let ref_byte = if byte_pos > 0 { byte_pos - 1 } else { 0 };
    let current_val = lookup_buffer_text_property(obarray, buffers, buf, ref_byte, &prop);
    let mut cursor = byte_pos;

    loop {
        match buf.text.text_props_previous_change(cursor) {
            Some(prev) => {
                if let Some(lim) = byte_limit {
                    if prev <= lim {
                        return Ok(match limit_val {
                            Some(lv) => Value::fixnum(lv),
                            None => Value::NIL,
                        });
                    }
                }
                let check = if prev > 0 { prev - 1 } else { 0 };
                let new_val = lookup_buffer_text_property(obarray, buffers, buf, check, &prop);
                let changed = !equal_value(&current_val, &new_val, 0);
                if changed {
                    return Ok(Value::fixnum(byte_to_elisp_pos(buf, prev)));
                }
                if prev == 0 {
                    break;
                }
                cursor = if prev < cursor { prev } else { prev - 1 };
            }
            None => break,
        }
    }

    Ok(match limit_val {
        Some(lv) => Value::fixnum(lv),
        None => Value::NIL,
    })
}

/// (next-property-change POS &optional OBJECT LIMIT)
pub(crate) fn builtin_next_property_change(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_next_property_change_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_next_property_change_in_buffers(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("next-property-change", &args, 1)?;
    expect_max_args("next-property-change", &args, 3)?;
    let pos = expect_integer_or_marker_in_buffers(buffers, &args[0])?;

    if let Some(str_id) = is_string_object(args.get(1)) {
        let s = with_heap(|h| h.get_string(str_id).to_owned());
        let table = get_string_text_properties_table(str_id).unwrap_or_default();
        let byte_pos = string_elisp_pos_to_byte(&s, pos);
        let limit_arg = args.get(2);
        // Keep original limit value for returning (don't clamp to string length)
        let (byte_limit, limit_val) = match limit_arg {
            Some(v) if !v.is_nil() => {
                let lim_int = expect_int(v)?;
                (Some(string_elisp_pos_to_byte(&s, lim_int)), Some(lim_int))
            }
            _ => (None, None),
        };
        let str_byte_len = s.len();
        return match table.next_property_change(byte_pos) {
            Some(next) => {
                if let Some(lim) = byte_limit {
                    if next >= lim {
                        return Ok(match limit_val {
                            Some(lv) => Value::fixnum(lv),
                            None => Value::NIL,
                        });
                    }
                }
                // If the change is at or past the end of the string, treat as no change
                if next >= str_byte_len {
                    return Ok(match limit_val {
                        Some(lv) => Value::fixnum(lv),
                        None => Value::NIL,
                    });
                }
                Ok(Value::fixnum(string_byte_to_elisp_pos(&s, next)))
            }
            None => Ok(match limit_val {
                Some(lv) => Value::fixnum(lv),
                None => Value::NIL,
            }),
        };
    }

    let buf_id = resolve_buffer_id_in_buffers(buffers, args.get(1))?;
    let limit_arg = args.get(2);

    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let byte_pos = elisp_pos_to_byte(buf, pos);
    // Keep original limit value for returning
    let (byte_limit, limit_val) = match limit_arg {
        Some(v) if !v.is_nil() => {
            let lim_int = expect_int(v)?;
            (Some(elisp_pos_to_byte(buf, lim_int)), Some(lim_int))
        }
        _ => (None, None),
    };
    let buf_end = buf.point_max();

    match buf.text.text_props_next_change(byte_pos) {
        Some(next) => {
            if let Some(lim) = byte_limit {
                if next >= lim {
                    return Ok(match limit_val {
                        Some(lv) => Value::fixnum(lv),
                        None => Value::NIL,
                    });
                }
            }
            // If the change is at or past buffer end, treat as no change
            if next >= buf_end {
                return Ok(match limit_val {
                    Some(lv) => Value::fixnum(lv),
                    None => Value::NIL,
                });
            }
            Ok(Value::fixnum(byte_to_elisp_pos(buf, next)))
        }
        None => Ok(match limit_val {
            Some(lv) => Value::fixnum(lv),
            None => Value::NIL,
        }),
    }
}

/// (text-property-any BEG END PROP VAL &optional OBJECT)
pub(crate) fn builtin_text_property_any(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_text_property_any_in_state(&eval.obarray, &eval.buffers, args)
}

pub(crate) fn builtin_text_property_any_in_state(
    obarray: &Obarray,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("text-property-any", &args, 4)?;
    expect_max_args("text-property-any", &args, 5)?;
    let beg = expect_integer_or_marker_in_buffers(buffers, &args[0])?;
    let end = expect_integer_or_marker_in_buffers(buffers, &args[1])?;
    let prop = expect_symbol_name(&args[2])?;
    let val = &args[3];

    if let Some(str_id) = is_string_object(args.get(4)) {
        let s = with_heap(|h| h.get_string(str_id).to_owned());
        let table = get_string_text_properties_table(str_id).unwrap_or_default();
        let byte_beg = string_elisp_pos_to_byte(&s, beg);
        let byte_end = string_elisp_pos_to_byte(&s, end);
        let mut cursor = byte_beg;
        while cursor < byte_end {
            let found = lookup_string_text_property(obarray, buffers, &table, cursor, &prop);
            if equal_value(&found, val, 0) {
                return Ok(Value::fixnum(string_byte_to_elisp_pos(&s, cursor)));
            }
            match table.next_property_change(cursor) {
                Some(next) if next <= byte_end => cursor = next,
                _ => break,
            }
        }
        return Ok(Value::NIL);
    }

    let buf_id = resolve_buffer_id_in_buffers(buffers, args.get(4))?;
    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    validate_buffer_range(buf, beg, end)?;
    let byte_beg = elisp_pos_to_byte(buf, beg);
    let byte_end = elisp_pos_to_byte(buf, end);

    let mut cursor = byte_beg;
    while cursor < byte_end {
        let found = lookup_buffer_text_property(obarray, buffers, buf, cursor, &prop);
        if equal_value(&found, val, 0) {
            return Ok(Value::fixnum(byte_to_elisp_pos(buf, cursor)));
        }
        match buf.text.text_props_next_change(cursor) {
            Some(next) if next <= byte_end => {
                cursor = next;
            }
            _ => break,
        }
    }
    Ok(Value::NIL)
}

/// (text-property-not-all BEG END PROP VAL &optional OBJECT)
pub(crate) fn builtin_text_property_not_all(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_text_property_not_all_in_state(&eval.obarray, &eval.buffers, args)
}

pub(crate) fn builtin_text_property_not_all_in_state(
    obarray: &Obarray,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("text-property-not-all", &args, 4)?;
    expect_max_args("text-property-not-all", &args, 5)?;
    let beg = expect_integer_or_marker_in_buffers(buffers, &args[0])?;
    let end = expect_integer_or_marker_in_buffers(buffers, &args[1])?;
    let prop = expect_symbol_name(&args[2])?;
    let val = &args[3];

    if let Some(str_id) = is_string_object(args.get(4)) {
        let s = with_heap(|h| h.get_string(str_id).to_owned());
        let table = get_string_text_properties_table(str_id).unwrap_or_default();
        let byte_beg = string_elisp_pos_to_byte(&s, beg);
        let byte_end = string_elisp_pos_to_byte(&s, end);
        let mut cursor = byte_beg;
        while cursor < byte_end {
            let found = lookup_string_text_property(obarray, buffers, &table, cursor, &prop);
            let matches = equal_value(&found, val, 0);
            if !matches {
                return Ok(Value::fixnum(string_byte_to_elisp_pos(&s, cursor)));
            }
            match table.next_property_change(cursor) {
                Some(next) if next > cursor && next < byte_end => cursor = next,
                _ => break,
            }
        }
        return Ok(Value::NIL);
    }

    let buf_id = resolve_buffer_id_in_buffers(buffers, args.get(4))?;
    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    validate_buffer_range(buf, beg, end)?;
    let byte_beg = elisp_pos_to_byte(buf, beg);
    let byte_end = elisp_pos_to_byte(buf, end);
    let mut cursor = byte_beg;

    while cursor < byte_end {
        let found = lookup_buffer_text_property(obarray, buffers, buf, cursor, &prop);
        let matches = equal_value(&found, val, 0);
        if !matches {
            return Ok(Value::fixnum(byte_to_elisp_pos(buf, cursor)));
        }

        match buf.text.text_props_next_change(cursor) {
            Some(next) if next > cursor && next < byte_end => cursor = next,
            _ => break,
        }
    }

    Ok(Value::NIL)
}

/// (get-char-property-and-overlay POS PROP &optional OBJECT)
pub(crate) fn builtin_get_char_property_and_overlay(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_get_char_property_and_overlay_in_state(&eval.obarray, &eval.buffers, args)
}

pub(crate) fn builtin_get_char_property_and_overlay_in_state(
    obarray: &Obarray,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("get-char-property-and-overlay", &args, 2)?;
    expect_max_args("get-char-property-and-overlay", &args, 3)?;
    let pos = expect_integer_or_marker_in_buffers(buffers, &args[0])?;
    let prop = expect_symbol_name(&args[1])?;

    // For strings, no overlays — just return (text-prop-value . nil)
    if is_string_object(args.get(2)).is_some() {
        let value = builtin_get_text_property_in_state(obarray, buffers, args)?;
        return Ok(Value::cons(value, Value::NIL));
    }

    let buf_id = resolve_buffer_id_in_buffers(buffers, args.get(2))?;

    if let Some(buf) = buffers.get(buf_id) {
        let byte_pos = elisp_pos_to_byte(buf, pos);
        if let Some((value, ov_id)) =
            buffer_overlay_property_at_byte_pos(obarray, buffers, buf, byte_pos, &prop)
        {
            return Ok(Value::cons(value, Value::Overlay(ov_id)));
        }
    }

    let value = builtin_get_char_property_in_state(obarray, buffers, args)?;
    Ok(Value::cons(value, Value::NIL))
}

/// (get-display-property POS PROP &optional OBJECT PROPERTIES)
pub(crate) fn builtin_get_display_property(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_get_display_property_in_state(&eval.obarray, &eval.buffers, args)
}

pub(crate) fn builtin_get_display_property_in_state(
    obarray: &Obarray,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("get-display-property", &args, 2)?;
    expect_max_args("get-display-property", &args, 4)?;
    let prop = expect_symbol_name(&args[1])?;
    if prop != "display" {
        return Ok(Value::NIL);
    }
    let mut forwarded = vec![args[0], args[1]];
    if let Some(object) = args.get(2) {
        forwarded.push(*object);
    }
    builtin_get_char_property_in_state(obarray, buffers, forwarded)
}

// ===========================================================================
// Overlay builtins
// ===========================================================================

/// (next-overlay-change POS)
pub(crate) fn builtin_next_overlay_change(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_next_overlay_change_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_next_overlay_change_in_buffers(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("next-overlay-change", &args, 1)?;
    let pos = expect_integer_or_marker_in_buffers(buffers, &args[0])?;
    let buf_id = current_buffer_id_in_buffers(buffers)?;
    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let byte_pos = elisp_pos_to_byte(buf, pos);
    match buf.overlays.next_boundary_after(byte_pos) {
        Some(next) => Ok(Value::fixnum(byte_to_elisp_pos(buf, next))),
        None => Ok(Value::fixnum(byte_to_elisp_pos(buf, buf.point_max()))),
    }
}

/// (previous-overlay-change POS)
pub(crate) fn builtin_previous_overlay_change(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_previous_overlay_change_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_previous_overlay_change_in_buffers(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("previous-overlay-change", &args, 1)?;
    let pos = expect_integer_or_marker_in_buffers(buffers, &args[0])?;
    let buf_id = current_buffer_id_in_buffers(buffers)?;
    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let byte_pos = elisp_pos_to_byte(buf, pos);
    match buf.overlays.previous_boundary_before(byte_pos) {
        Some(prev) => Ok(Value::fixnum(byte_to_elisp_pos(buf, prev))),
        None => Ok(Value::fixnum(byte_to_elisp_pos(buf, buf.point_min()))),
    }
}

/// (make-overlay BEG END &optional BUFFER FRONT-ADVANCE REAR-ADVANCE)
pub(crate) fn builtin_make_overlay(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_make_overlay_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_make_overlay_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("make-overlay", &args, 2)?;
    expect_max_args("make-overlay", &args, 5)?;
    let buf_id = resolve_buffer_id_in_buffers(buffers, args.get(2))?;
    ensure_marker_points_into_buffer(buffers, &args[0], buf_id)?;
    ensure_marker_points_into_buffer(buffers, &args[1], buf_id)?;
    let mut beg = expect_integer_or_marker_in_buffers(buffers, &args[0])?;
    let mut end = expect_integer_or_marker_in_buffers(buffers, &args[1])?;
    if beg > end {
        std::mem::swap(&mut beg, &mut end);
    }
    let front_advance = args.get(3).is_some_and(|v| v.is_truthy());
    let rear_advance = args.get(4).is_some_and(|v| v.is_truthy());

    let buf = buffers
        .get_mut(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let byte_beg = elisp_pos_to_byte(buf, beg);
    let byte_end = elisp_pos_to_byte(buf, end);
    let overlay = with_heap_mut(|h| {
        h.alloc_overlay(crate::gc::types::OverlayData {
            plist: Value::NIL,
            buffer: Some(buf_id),
            start: byte_beg,
            end: byte_end,
            front_advance,
            rear_advance,
        })
    });
    buf.overlays.insert_overlay(overlay);
    Ok(Value::Overlay(overlay))
}

/// (delete-overlay OVERLAY)
pub(crate) fn builtin_delete_overlay(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_delete_overlay_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_delete_overlay_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("delete-overlay", &args, 1)?;
    let overlay = expect_overlay(&args[0])?;
    if let Some(buf_id) = resolve_overlay_buffer_id(overlay) {
        let _ = buffers.delete_buffer_overlay(buf_id, overlay);
    }
    Ok(Value::NIL)
}

/// (overlay-put OVERLAY PROP VAL)
pub(crate) fn builtin_overlay_put(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_overlay_put_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_overlay_put_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("overlay-put", &args, 3)?;
    let overlay = expect_overlay(&args[0])?;
    let val = args[2];
    let changed = if let Some(buf_id) = resolve_overlay_buffer_id(overlay) {
        buffers
            .get_mut(buf_id)
            .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?
            .overlays
            .overlay_put(overlay, args[1], val)
    } else {
        with_heap_mut(|h| {
            let object = h.get_overlay_mut(overlay);
            let (plist, changed) = plist_put_eq(object.plist, args[1], val);
            object.plist = plist;
            changed
        })
    };
    if let Some(buf_id) = resolve_overlay_buffer_id(overlay) {
        if changed {
            let evaporate = args[1].is_symbol_named("evaporate") && val.is_truthy();
            let is_empty = buffers
                .get(buf_id)
                .and_then(|buf| {
                    let start = buf.overlays.overlay_start(overlay)?;
                    let end = buf.overlays.overlay_end(overlay)?;
                    Some(start == end)
                })
                .unwrap_or(false);
            if evaporate && is_empty {
                let _ = buffers.delete_buffer_overlay(buf_id, overlay);
            }
        }
    }
    Ok(val)
}

/// (overlay-get OVERLAY PROP)
pub(crate) fn builtin_overlay_get(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_overlay_get_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_overlay_get_in_buffers(
    _buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("overlay-get", &args, 2)?;
    let overlay = expect_overlay(&args[0])?;
    if let Some(val) =
        with_heap(|h| crate::buffer::overlay::plist_get_eq(h.get_overlay(overlay).plist, &args[1]))
    {
        return Ok(val);
    }
    Ok(Value::NIL)
}

/// (overlayp OBJ)
pub(crate) fn builtin_overlayp(_eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_overlayp_pure(args)
}

pub(crate) fn builtin_overlayp_pure(args: Vec<Value>) -> EvalResult {
    expect_args("overlayp", &args, 1)?;
    if matches!(args[0], ValueKind::Veclike(VecLikeType::Overlay)) {
        return Ok(Value::T);
    }
    Ok(Value::NIL)
}

/// (overlays-at POS &optional SORTED)
pub(crate) fn builtin_overlays_at(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_overlays_at_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_overlays_at_in_buffers(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("overlays-at", &args, 1)?;
    expect_max_args("overlays-at", &args, 2)?;
    let pos = expect_integer_or_marker_in_buffers(buffers, &args[0])?;
    let buf_id = current_buffer_id_in_buffers(buffers)?;
    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let byte_pos = elisp_pos_to_byte(buf, pos);
    let mut ids = buf.overlays.overlays_at(byte_pos);
    if args.get(1).is_some_and(|value| value.is_truthy()) {
        buf.overlays.sort_overlay_ids_by_priority_desc(&mut ids);
    }
    let overlays: Vec<Value> = ids.into_iter().map(Value::Overlay).collect();
    Ok(Value::list(overlays))
}

/// (overlays-in BEG END)
pub(crate) fn builtin_overlays_in(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_overlays_in_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_overlays_in_in_buffers(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("overlays-in", &args, 2)?;
    let beg = expect_integer_or_marker_in_buffers(buffers, &args[0])?;
    let end = expect_integer_or_marker_in_buffers(buffers, &args[1])?;
    let buf_id = current_buffer_id_in_buffers(buffers)?;
    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let byte_beg = elisp_pos_to_byte(buf, beg);
    let byte_end = elisp_pos_to_byte(buf, end);
    let ids = buf
        .overlays
        .overlays_in_region(byte_beg, byte_end, buf.point_max_byte());
    let overlays: Vec<Value> = ids.into_iter().map(Value::Overlay).collect();
    Ok(Value::list(overlays))
}

/// (move-overlay OVERLAY BEG END &optional BUFFER)
pub(crate) fn builtin_move_overlay(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_move_overlay_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_move_overlay_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("move-overlay", &args, 3)?;
    expect_max_args("move-overlay", &args, 4)?;
    let overlay = expect_overlay(&args[0])?;
    let old_buf_id = resolve_overlay_buffer_id(overlay);

    // Resolve target buffer: use BUFFER arg if given, otherwise same buffer.
    let new_buf_id = if let Some(buf_arg) = args.get(3) {
        if buf_arg.is_truthy() {
            resolve_buffer_id_in_buffers(buffers, Some(buf_arg))?
        } else {
            old_buf_id.unwrap_or_else(|| buffers.current_buffer_id().expect("current buffer"))
        }
    } else {
        old_buf_id.unwrap_or_else(|| buffers.current_buffer_id().expect("current buffer"))
    };

    ensure_marker_points_into_buffer(buffers, &args[1], new_buf_id)?;
    ensure_marker_points_into_buffer(buffers, &args[2], new_buf_id)?;
    let mut beg = expect_integer_or_marker_in_buffers(buffers, &args[1])?;
    let mut end = expect_integer_or_marker_in_buffers(buffers, &args[2])?;
    if beg > end {
        std::mem::swap(&mut beg, &mut end);
    }

    if old_buf_id == Some(new_buf_id) {
        // Same buffer: just move within the buffer.
        let buf = buffers
            .get_mut(new_buf_id)
            .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;
        let byte_beg = elisp_pos_to_byte(buf, beg);
        let byte_end = elisp_pos_to_byte(buf, end);
        buf.overlays.move_overlay(overlay, byte_beg, byte_end);
        Ok(args[0])
    } else {
        if let Some(old_buf_id) = old_buf_id
            && let Some(buf) = buffers.get_mut(old_buf_id)
        {
            buf.overlays.detach_overlay(overlay);
        }

        let new_buf = buffers
            .get_mut(new_buf_id)
            .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;
        let byte_beg = elisp_pos_to_byte(new_buf, beg);
        let byte_end = elisp_pos_to_byte(new_buf, end);
        with_heap_mut(|h| {
            let object = h.get_overlay_mut(overlay);
            object.buffer = Some(new_buf_id);
            object.start = byte_beg;
            object.end = byte_end;
        });
        new_buf.overlays.insert_overlay(overlay);
        Ok(args[0])
    }
}

/// (overlay-start OVERLAY)
pub(crate) fn builtin_overlay_start(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_overlay_start_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_overlay_start_in_buffers(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("overlay-start", &args, 1)?;
    let overlay = expect_overlay(&args[0])?;
    let Some(buf_id) = resolve_overlay_buffer_id(overlay) else {
        return Ok(Value::NIL);
    };
    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    match buf.overlays.overlay_start(overlay) {
        Some(byte_pos) => Ok(Value::fixnum(byte_to_elisp_pos(buf, byte_pos))),
        None => Ok(Value::NIL),
    }
}

/// (overlay-end OVERLAY)
pub(crate) fn builtin_overlay_end(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_overlay_end_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_overlay_end_in_buffers(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("overlay-end", &args, 1)?;
    let overlay = expect_overlay(&args[0])?;
    let Some(buf_id) = resolve_overlay_buffer_id(overlay) else {
        return Ok(Value::NIL);
    };
    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    match buf.overlays.overlay_end(overlay) {
        Some(byte_pos) => Ok(Value::fixnum(byte_to_elisp_pos(buf, byte_pos))),
        None => Ok(Value::NIL),
    }
}

/// (overlay-buffer OVERLAY)
pub(crate) fn builtin_overlay_buffer(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_overlay_buffer_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_overlay_buffer_in_buffers(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("overlay-buffer", &args, 1)?;
    let overlay = expect_overlay(&args[0])?;
    if let Some(buf_id) = resolve_overlay_buffer_id(overlay)
        && buffers.get(buf_id).is_some()
    {
        return Ok(Value::make_buffer(buf_id));
    }
    Ok(Value::NIL)
}

/// (overlay-properties OVERLAY)
pub(crate) fn builtin_overlay_properties(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_overlay_properties_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_overlay_properties_in_buffers(
    _buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("overlay-properties", &args, 1)?;
    let overlay = expect_overlay(&args[0])?;
    builtin_copy_sequence(vec![with_heap(|h| h.get_overlay(overlay).plist)])
}

/// (remove-overlays &optional BEG END NAME VAL)
pub(crate) fn builtin_remove_overlays(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("remove-overlays", &args, 4)?;
    let buf_id = eval
        .buffers
        .current_buffer()
        .map(|b| b.id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    let (start_pos, end_pos) = {
        let buf = eval
            .buffers
            .get(buf_id)
            .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;
        let start = if args.is_empty() || args[0].is_nil() {
            buf.point_min()
        } else {
            elisp_pos_to_byte(buf, expect_int_eval(eval, &args[0])?)
        };
        let end = if args.len() < 2 || args[1].is_nil() {
            buf.point_max()
        } else {
            elisp_pos_to_byte(buf, expect_int_eval(eval, &args[1])?)
        };
        (start, end)
    };

    let buf = eval
        .buffers
        .get_mut(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let filter_name = if args.len() >= 3 && !args[2].is_nil() {
        Some(expect_symbol_name(&args[2])?)
    } else {
        None
    };

    let filter_val = if args.len() >= 4 && !args[3].is_nil() {
        Some(args[3])
    } else {
        None
    };

    // Collect overlay ids in range.
    let ids = buf
        .overlays
        .overlays_in_region(start_pos, end_pos, buf.point_max_byte());

    // Filter and delete.
    for overlay in ids {
        let should_delete = match (&filter_name, &filter_val) {
            (Some(name), Some(val)) => buf
                .overlays
                .overlay_get_named(overlay, name)
                .is_some_and(|v| equal_value(&v, val, 0)),
            (Some(name), None) => buf.overlays.overlay_get_named(overlay, name).is_some(),
            _ => true,
        };
        if should_delete {
            buf.overlays.delete_overlay(overlay);
        }
    }

    Ok(Value::NIL)
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
#[path = "textprop_test.rs"]
mod tests;
