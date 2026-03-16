//! Text property and overlay builtins for the Elisp interpreter.
//!
//! Bridges the buffer's `TextPropertyTable` and `OverlayList` to Elisp
//! functions like `put-text-property`, `make-overlay`, etc.

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::string_escape::{storage_byte_to_char, storage_char_len, storage_char_to_byte};
use super::value::*;
use crate::buffer::text_props::TextPropertyTable;
use crate::buffer::{BufferId, BufferManager};

// ---------------------------------------------------------------------------
// Helpers (local to this module)
// ---------------------------------------------------------------------------

fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_max_args(name: &str, args: &[Value], max: usize) -> Result<(), Flow> {
    if args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_int(value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        marker if super::marker::is_marker(marker) => super::marker::marker_position_as_int(marker),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *other],
        )),
    }
}

fn expect_int_eval(eval: &super::eval::Evaluator, value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        marker if super::marker::is_marker(marker) => {
            super::marker::marker_position_as_int_eval(eval, marker)
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *other],
        )),
    }
}

fn expect_integer_or_marker(value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        marker if super::marker::is_marker(marker) => super::marker::marker_position_as_int(marker),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *other],
        )),
    }
}

fn expect_integer_or_marker_eval(
    eval: &super::eval::Evaluator,
    value: &Value,
) -> Result<i64, Flow> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        marker if super::marker::is_marker(marker) => {
            super::marker::marker_position_as_int_eval(eval, marker)
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *other],
        )),
    }
}

fn expect_integer_or_marker_in_buffers(
    buffers: &BufferManager,
    value: &Value,
) -> Result<i64, Flow> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        marker if super::marker::is_marker(marker) => {
            super::marker::marker_position_as_int_with_buffers(buffers, marker)
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *other],
        )),
    }
}

/// Extract a symbol name (for property names).
fn expect_symbol_name(value: &Value) -> Result<String, Flow> {
    match value.as_symbol_name() {
        Some(s) => Ok(s.to_string()),
        None => match value {
            Value::Str(_) => Ok(value.as_str().unwrap().to_string()),
            Value::Keyword(id) => Ok(resolve_sym(*id).to_owned()),
            other => Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), *other],
            )),
        },
    }
}

pub fn register_bootstrap_vars(obarray: &mut crate::emacs_core::symbol::Obarray) {
    obarray.set_symbol_value("default-text-properties", Value::Nil);
    obarray.set_symbol_value("char-property-alias-alist", Value::Nil);
    obarray.set_symbol_value("inhibit-point-motion-hooks", Value::True);
    obarray.set_symbol_value(
        "text-property-default-nonsticky",
        Value::list(vec![
            Value::cons(Value::symbol("syntax-table"), Value::True),
            Value::cons(Value::symbol("display"), Value::True),
        ]),
    );
}

/// Convert a 1-based Elisp char position to a 0-based byte position,
/// clamping within the buffer.
fn elisp_pos_to_byte(buf: &crate::buffer::buffer::Buffer, pos: i64) -> usize {
    let char_pos = if pos > 0 { pos as usize - 1 } else { 0 };
    let clamped = char_pos.min(buf.text.char_count());
    buf.text.char_to_byte(clamped)
}

/// Convert a 0-based byte position to a 1-based Elisp char position.
fn byte_to_elisp_pos(buf: &crate::buffer::buffer::Buffer, byte_pos: usize) -> i64 {
    buf.text.byte_to_char(byte_pos) as i64 + 1
}

/// Resolve the optional OBJECT argument to a buffer id.
/// If nil or absent, uses the current buffer.
fn resolve_buffer_id(
    eval: &super::eval::Evaluator,
    object: Option<&Value>,
) -> Result<BufferId, Flow> {
    resolve_buffer_id_in_buffers(&eval.buffers, object)
}

fn resolve_buffer_id_in_buffers(
    buffers: &BufferManager,
    object: Option<&Value>,
) -> Result<BufferId, Flow> {
    match object {
        None | Some(Value::Nil) => buffers
            .current_buffer()
            .map(|b| b.id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")])),
        Some(Value::Buffer(id)) => Ok(*id),
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

fn make_overlay_value(ov_id: u64, buf_id: BufferId) -> Value {
    Value::cons(Value::Int(ov_id as i64), Value::Buffer(buf_id))
}

fn ensure_marker_points_into_buffer(
    buffers: &BufferManager,
    value: &Value,
    buffer_id: BufferId,
) -> Result<(), Flow> {
    let Some((Some(buffer_name), _, _)) = super::marker::marker_logical_fields(value) else {
        return Ok(());
    };
    let Some(marker_buffer_id) = buffers.find_buffer_by_name(&buffer_name) else {
        return Ok(());
    };
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
        Some(Value::Str(id)) => Some(*id),
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

/// Convert a HashMap<String, Value> to an Elisp plist (alternating symbols and values).
fn hashmap_to_plist(map: &std::collections::HashMap<String, Value>) -> Value {
    let mut items = Vec::new();
    for (key, val) in map {
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
    eval: &mut super::eval::Evaluator,
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
        return Ok(Value::Nil);
    }

    let buf_id = resolve_buffer_id_in_buffers(buffers, args.get(4))?;
    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let byte_beg = elisp_pos_to_byte(buf, beg);
    let byte_end = elisp_pos_to_byte(buf, end);
    let _ = buffers.put_buffer_text_property(buf_id, byte_beg, byte_end, &prop, val);
    Ok(Value::Nil)
}

/// (get-text-property POS PROP &optional OBJECT)
pub(crate) fn builtin_get_text_property(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_get_text_property_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_get_text_property_in_buffers(
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
            if let Some(v) = table.get_property(byte_pos, &prop) {
                return Ok(*v);
            }
        }
        return Ok(Value::Nil);
    }

    let buf_id = resolve_buffer_id_in_buffers(buffers, args.get(2))?;
    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let byte_pos = elisp_pos_to_byte(buf, pos);
    match buf.text_props.get_property(byte_pos, &prop) {
        Some(v) => {
            let val: Value = *v;
            Ok(val)
        }
        None => Ok(Value::Nil),
    }
}

/// (get-char-property POS PROP &optional OBJECT)
/// For strings, same as get-text-property (no overlays).
pub(crate) fn builtin_get_char_property(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_get_char_property_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_get_char_property_in_buffers(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("get-char-property", &args, 2)?;
    expect_max_args("get-char-property", &args, 3)?;
    // For strings, delegate directly (no overlays).
    // For buffers, also delegate (overlays not yet implemented here).
    builtin_get_text_property_in_buffers(buffers, args)
}

/// (add-text-properties BEG END PROPS &optional OBJECT)
pub(crate) fn builtin_add_text_properties(
    eval: &mut super::eval::Evaluator,
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
        for (name, val) in pairs {
            table.put_property(byte_beg, byte_end, &name, val);
        }
        save_string_props(str_id, table);
        return Ok(Value::True);
    }

    let buf_id = resolve_buffer_id_in_buffers(buffers, args.get(3))?;
    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let byte_beg = elisp_pos_to_byte(buf, beg);
    let byte_end = elisp_pos_to_byte(buf, end);
    for (name, val) in pairs {
        let _ = buffers.put_buffer_text_property(buf_id, byte_beg, byte_end, &name, val);
    }
    Ok(Value::True)
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
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("add-face-text-property", &args, 3)?;
    expect_max_args("add-face-text-property", &args, 5)?;
    let beg = expect_integer_or_marker_eval(eval, &args[0])?;
    let end = expect_integer_or_marker_eval(eval, &args[1])?;
    let new_face = args[2];
    let append = args.get(3).is_some_and(Value::is_truthy);

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
        return Ok(Value::Nil);
    }

    let buf_id = match object {
        None | Some(Value::Nil) => eval
            .buffers
            .current_buffer()
            .map(|b| b.id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")])),
        Some(Value::Buffer(id)) => Ok(*id),
        Some(other) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("buffer-or-string-p"), *other],
        )),
    }?;

    let buf = eval
        .buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;
    let byte_beg = elisp_pos_to_byte(buf, beg);
    let byte_end = elisp_pos_to_byte(buf, end);
    let existing = buf.text_props.get_property(byte_beg, "face").cloned();
    let merged = merge_face_property(existing, new_face, append);
    let _ = eval
        .buffers
        .put_buffer_text_property(buf_id, byte_beg, byte_end, "face", merged);
    Ok(Value::Nil)
}

/// (remove-text-properties BEG END PROPS &optional OBJECT)
pub(crate) fn builtin_remove_text_properties(
    eval: &mut super::eval::Evaluator,
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
        return Ok(if any_removed { Value::True } else { Value::Nil });
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
    Ok(if any_removed { Value::True } else { Value::Nil })
}

/// (set-text-properties BEG END PROPS &optional OBJECT)
pub(crate) fn builtin_set_text_properties(
    eval: &mut super::eval::Evaluator,
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
        return Ok(Value::True);
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
    Ok(Value::True)
}

/// (remove-list-of-text-properties BEG END LIST &optional OBJECT)
pub(crate) fn builtin_remove_list_of_text_properties(
    eval: &mut super::eval::Evaluator,
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
        return Ok(if changed { Value::True } else { Value::Nil });
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
            if buf.text_props.get_property(cursor, &name).is_some() {
                changed = true;
                break;
            }
            match buf.text_props.next_property_change(cursor) {
                Some(next) if next > cursor && next < byte_end => cursor = next,
                _ => break,
            }
        }
        let _ = buffers.remove_buffer_text_property(buf_id, byte_beg, byte_end, &name);
    }
    Ok(if changed { Value::True } else { Value::Nil })
}

/// (text-properties-at POS &optional OBJECT)
pub(crate) fn builtin_text_properties_at(
    eval: &mut super::eval::Evaluator,
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
            let props = table.get_properties(byte_pos);
            return Ok(hashmap_to_plist(&props));
        }
        return Ok(Value::Nil);
    }

    let buf_id = resolve_buffer_id_in_buffers(buffers, args.get(1))?;
    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let byte_pos = elisp_pos_to_byte(buf, pos);
    let props = buf.text_props.get_properties(byte_pos);
    Ok(hashmap_to_plist(&props))
}

/// (next-single-property-change POS PROP &optional OBJECT LIMIT)
pub(crate) fn builtin_next_single_property_change(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_next_single_property_change_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_next_single_property_change_in_buffers(
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
        let current_val = table.get_property(byte_pos, &prop).cloned();
        let str_len = s.len();
        let mut cursor = byte_pos;
        loop {
            match table.next_property_change(cursor) {
                Some(next) => {
                    if let Some(lim) = byte_limit {
                        if next >= lim {
                            return Ok(match limit_val {
                                Some(lv) => Value::Int(lv),
                                None => Value::Nil,
                            });
                        }
                    }
                    if next >= str_len {
                        break;
                    }
                    let new_val = table.get_property(next, &prop).cloned();
                    let changed = match (&current_val, &new_val) {
                        (None, None) => false,
                        (Some(a), Some(b)) => !equal_value(a, b, 0),
                        _ => true,
                    };
                    if changed {
                        return Ok(Value::Int(string_byte_to_elisp_pos(&s, next)));
                    }
                    cursor = next;
                }
                None => break,
            }
        }
        return Ok(match limit_val {
            Some(lv) => Value::Int(lv),
            None => Value::Nil,
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

    let current_val = buf.text_props.get_property(byte_pos, &prop).cloned();
    let buf_end = buf.point_max();
    let mut cursor = byte_pos;

    loop {
        match buf.text_props.next_property_change(cursor) {
            Some(next) => {
                if let Some(lim) = byte_limit {
                    if next >= lim {
                        return Ok(match limit_val {
                            Some(lv) => Value::Int(lv),
                            None => Value::Nil,
                        });
                    }
                }
                if next >= buf_end {
                    break;
                }
                let new_val = buf.text_props.get_property(next, &prop).cloned();
                let changed = match (&current_val, &new_val) {
                    (None, None) => false,
                    (Some(a), Some(b)) => !equal_value(a, b, 0),
                    _ => true,
                };
                if changed {
                    return Ok(Value::Int(byte_to_elisp_pos(buf, next)));
                }
                cursor = next;
            }
            None => break,
        }
    }

    Ok(match limit_val {
        Some(lv) => Value::Int(lv),
        None => Value::Nil,
    })
}

/// (previous-single-property-change POS PROP &optional OBJECT LIMIT)
pub(crate) fn builtin_previous_single_property_change(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_previous_single_property_change_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_previous_single_property_change_in_buffers(
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
        let current_val = table.get_property(ref_byte, &prop).cloned();
        let mut cursor = byte_pos;
        loop {
            match table.previous_property_change(cursor) {
                Some(prev) => {
                    if let Some(lim) = byte_limit {
                        if prev <= lim {
                            return Ok(match limit_val {
                                Some(lv) => Value::Int(lv),
                                None => Value::Nil,
                            });
                        }
                    }
                    let check = if prev > 0 { prev - 1 } else { 0 };
                    let new_val = table.get_property(check, &prop).cloned();
                    let changed = match (&current_val, &new_val) {
                        (None, None) => false,
                        (Some(a), Some(b)) => !equal_value(a, b, 0),
                        _ => true,
                    };
                    if changed {
                        return Ok(Value::Int(string_byte_to_elisp_pos(&s, prev)));
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
            Some(lv) => Value::Int(lv),
            None => Value::Nil,
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
    let current_val = buf.text_props.get_property(ref_byte, &prop).cloned();
    let mut cursor = byte_pos;

    loop {
        match buf.text_props.previous_property_change(cursor) {
            Some(prev) => {
                if let Some(lim) = byte_limit {
                    if prev <= lim {
                        return Ok(match limit_val {
                            Some(lv) => Value::Int(lv),
                            None => Value::Nil,
                        });
                    }
                }
                let check = if prev > 0 { prev - 1 } else { 0 };
                let new_val = buf.text_props.get_property(check, &prop).cloned();
                let changed = match (&current_val, &new_val) {
                    (None, None) => false,
                    (Some(a), Some(b)) => !equal_value(a, b, 0),
                    _ => true,
                };
                if changed {
                    return Ok(Value::Int(byte_to_elisp_pos(buf, prev)));
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
        Some(lv) => Value::Int(lv),
        None => Value::Nil,
    })
}

/// (next-property-change POS &optional OBJECT LIMIT)
pub(crate) fn builtin_next_property_change(
    eval: &mut super::eval::Evaluator,
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
                            Some(lv) => Value::Int(lv),
                            None => Value::Nil,
                        });
                    }
                }
                // If the change is at or past the end of the string, treat as no change
                if next >= str_byte_len {
                    return Ok(match limit_val {
                        Some(lv) => Value::Int(lv),
                        None => Value::Nil,
                    });
                }
                Ok(Value::Int(string_byte_to_elisp_pos(&s, next)))
            }
            None => Ok(match limit_val {
                Some(lv) => Value::Int(lv),
                None => Value::Nil,
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

    match buf.text_props.next_property_change(byte_pos) {
        Some(next) => {
            if let Some(lim) = byte_limit {
                if next >= lim {
                    return Ok(match limit_val {
                        Some(lv) => Value::Int(lv),
                        None => Value::Nil,
                    });
                }
            }
            // If the change is at or past buffer end, treat as no change
            if next >= buf_end {
                return Ok(match limit_val {
                    Some(lv) => Value::Int(lv),
                    None => Value::Nil,
                });
            }
            Ok(Value::Int(byte_to_elisp_pos(buf, next)))
        }
        None => Ok(match limit_val {
            Some(lv) => Value::Int(lv),
            None => Value::Nil,
        }),
    }
}

/// (text-property-any BEG END PROP VAL &optional OBJECT)
pub(crate) fn builtin_text_property_any(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_text_property_any_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_text_property_any_in_buffers(
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
            if let Some(found) = table.get_property(cursor, &prop) {
                if equal_value(found, val, 0) {
                    return Ok(Value::Int(string_byte_to_elisp_pos(&s, cursor)));
                }
            }
            match table.next_property_change(cursor) {
                Some(next) if next <= byte_end => cursor = next,
                _ => break,
            }
        }
        return Ok(Value::Nil);
    }

    let buf_id = resolve_buffer_id_in_buffers(buffers, args.get(4))?;
    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let byte_beg = elisp_pos_to_byte(buf, beg);
    let byte_end = elisp_pos_to_byte(buf, end);

    let mut cursor = byte_beg;
    while cursor < byte_end {
        if let Some(found) = buf.text_props.get_property(cursor, &prop) {
            if equal_value(found, val, 0) {
                return Ok(Value::Int(byte_to_elisp_pos(buf, cursor)));
            }
        }
        match buf.text_props.next_property_change(cursor) {
            Some(next) if next <= byte_end => {
                cursor = next;
            }
            _ => break,
        }
    }
    Ok(Value::Nil)
}

/// (text-property-not-all BEG END PROP VAL &optional OBJECT)
pub(crate) fn builtin_text_property_not_all(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_text_property_not_all_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_text_property_not_all_in_buffers(
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
            let matches = match table.get_property(cursor, &prop) {
                Some(found) => equal_value(found, val, 0),
                None => val.is_nil(),
            };
            if !matches {
                return Ok(Value::Int(string_byte_to_elisp_pos(&s, cursor)));
            }
            match table.next_property_change(cursor) {
                Some(next) if next > cursor && next < byte_end => cursor = next,
                _ => break,
            }
        }
        return Ok(Value::Nil);
    }

    let buf_id = resolve_buffer_id_in_buffers(buffers, args.get(4))?;
    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let byte_beg = elisp_pos_to_byte(buf, beg);
    let byte_end = elisp_pos_to_byte(buf, end);
    let mut cursor = byte_beg;

    while cursor < byte_end {
        let matches = match buf.text_props.get_property(cursor, &prop) {
            Some(found) => equal_value(found, val, 0),
            None => val.is_nil(),
        };
        if !matches {
            return Ok(Value::Int(byte_to_elisp_pos(buf, cursor)));
        }

        match buf.text_props.next_property_change(cursor) {
            Some(next) if next > cursor && next < byte_end => cursor = next,
            _ => break,
        }
    }

    Ok(Value::Nil)
}

/// (get-char-property-and-overlay POS PROP &optional OBJECT)
pub(crate) fn builtin_get_char_property_and_overlay(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_get_char_property_and_overlay_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_get_char_property_and_overlay_in_buffers(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("get-char-property-and-overlay", &args, 2)?;
    expect_max_args("get-char-property-and-overlay", &args, 3)?;
    let pos = expect_integer_or_marker_in_buffers(buffers, &args[0])?;
    let prop = expect_symbol_name(&args[1])?;

    // For strings, no overlays — just return (text-prop-value . nil)
    if is_string_object(args.get(2)).is_some() {
        let value = builtin_get_text_property_in_buffers(buffers, args)?;
        return Ok(Value::cons(value, Value::Nil));
    }

    let buf_id = resolve_buffer_id_in_buffers(buffers, args.get(2))?;

    if let Some(buf) = buffers.get(buf_id) {
        let byte_pos = elisp_pos_to_byte(buf, pos);
        let overlay_ids = buf.overlays.overlays_at(byte_pos);
        for ov_id in overlay_ids {
            if let Some(val) = buf.overlays.overlay_get(ov_id, &prop) {
                let overlay = make_overlay_value(ov_id, buf_id);
                return Ok(Value::cons(*val, overlay));
            }
        }
    }

    let value = builtin_get_char_property_in_buffers(buffers, args)?;
    Ok(Value::cons(value, Value::Nil))
}

/// (get-display-property POS PROP &optional OBJECT PROPERTIES)
pub(crate) fn builtin_get_display_property(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_get_display_property_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_get_display_property_in_buffers(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("get-display-property", &args, 2)?;
    expect_max_args("get-display-property", &args, 4)?;
    let prop = expect_symbol_name(&args[1])?;
    if prop != "display" {
        return Ok(Value::Nil);
    }
    let mut forwarded = vec![args[0], args[1]];
    if let Some(object) = args.get(2) {
        forwarded.push(*object);
    }
    builtin_get_char_property_in_buffers(buffers, forwarded)
}

// ===========================================================================
// Overlay builtins
// ===========================================================================

/// (next-overlay-change POS)
pub(crate) fn builtin_next_overlay_change(
    eval: &mut super::eval::Evaluator,
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
    let all_ids = buf.overlays.overlays_in(buf.point_min(), buf.point_max());
    let mut best: Option<usize> = None;
    for ov_id in all_ids {
        if let Some(start) = buf.overlays.overlay_start(ov_id) {
            if start > byte_pos {
                best = Some(best.map_or(start, |cur| cur.min(start)));
            }
        }
        if let Some(end) = buf.overlays.overlay_end(ov_id) {
            if end > byte_pos {
                best = Some(best.map_or(end, |cur| cur.min(end)));
            }
        }
    }

    match best {
        Some(next) => Ok(Value::Int(byte_to_elisp_pos(buf, next))),
        None => Ok(Value::Int(byte_to_elisp_pos(buf, buf.point_max()))),
    }
}

/// (previous-overlay-change POS)
pub(crate) fn builtin_previous_overlay_change(
    eval: &mut super::eval::Evaluator,
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
    let all_ids = buf.overlays.overlays_in(buf.point_min(), buf.point_max());
    let mut best: Option<usize> = None;
    for ov_id in all_ids {
        if let Some(start) = buf.overlays.overlay_start(ov_id) {
            if start < byte_pos {
                best = Some(best.map_or(start, |cur| cur.max(start)));
            }
        }
        if let Some(end) = buf.overlays.overlay_end(ov_id) {
            if end < byte_pos {
                best = Some(best.map_or(end, |cur| cur.max(end)));
            }
        }
    }

    match best {
        Some(prev) => Ok(Value::Int(byte_to_elisp_pos(buf, prev))),
        None => Ok(Value::Int(byte_to_elisp_pos(buf, buf.point_min()))),
    }
}

/// (make-overlay BEG END &optional BUFFER FRONT-ADVANCE REAR-ADVANCE)
pub(crate) fn builtin_make_overlay(
    eval: &mut super::eval::Evaluator,
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
    let ov_id = buf.overlays.make_overlay(byte_beg, byte_end);
    if front_advance {
        buf.overlays.set_front_advance(ov_id, true);
    }
    if rear_advance {
        buf.overlays.set_rear_advance(ov_id, true);
    }

    // Return a cons (overlay-id . buffer-id) to identify the overlay.
    Ok(make_overlay_value(ov_id, buf_id))
}

/// Extract overlay id and buffer id from an overlay value (cons of int . buffer).
fn expect_overlay(value: &Value) -> Result<(u64, BufferId), Flow> {
    if let Value::Cons(cell) = value {
        let pair = read_cons(*cell);
        if let (Value::Int(ov_id), Value::Buffer(buf_id)) = (&pair.car, &pair.cdr) {
            return Ok((*ov_id as u64, *buf_id));
        }
    }
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("overlayp"), *value],
    ))
}

/// (delete-overlay OVERLAY)
pub(crate) fn builtin_delete_overlay(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_delete_overlay_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_delete_overlay_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("delete-overlay", &args, 1)?;
    let (ov_id, buf_id) = expect_overlay(&args[0])?;
    let _ = buffers.delete_buffer_overlay(buf_id, ov_id);
    Ok(Value::Nil)
}

/// (overlay-put OVERLAY PROP VAL)
pub(crate) fn builtin_overlay_put(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_overlay_put_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_overlay_put_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("overlay-put", &args, 3)?;
    let (ov_id, buf_id) = expect_overlay(&args[0])?;
    let prop = expect_symbol_name(&args[1])?;
    let val = args[2];

    let _ = buffers.put_buffer_overlay_property(buf_id, ov_id, &prop, val);
    Ok(val)
}

/// (overlay-get OVERLAY PROP)
pub(crate) fn builtin_overlay_get(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_overlay_get_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_overlay_get_in_buffers(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("overlay-get", &args, 2)?;
    let (ov_id, buf_id) = expect_overlay(&args[0])?;
    let prop = expect_symbol_name(&args[1])?;

    if let Some(buf) = buffers.get(buf_id) {
        match buf.overlays.overlay_get(ov_id, &prop) {
            Some(v) => {
                let val: Value = *v;
                return Ok(val);
            }
            None => return Ok(Value::Nil),
        }
    }
    Ok(Value::Nil)
}

/// (overlayp OBJ)
pub(crate) fn builtin_overlayp(_eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    builtin_overlayp_pure(args)
}

pub(crate) fn builtin_overlayp_pure(args: Vec<Value>) -> EvalResult {
    expect_args("overlayp", &args, 1)?;
    if let Value::Cons(cell) = &args[0] {
        let pair = read_cons(*cell);
        if matches!((&pair.car, &pair.cdr), (Value::Int(_), Value::Buffer(_))) {
            return Ok(Value::True);
        }
    }
    Ok(Value::Nil)
}

/// (overlays-at POS &optional SORTED)
pub(crate) fn builtin_overlays_at(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
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
    let ids = buf.overlays.overlays_at(byte_pos);
    let overlays: Vec<Value> = ids
        .into_iter()
        .map(|id| make_overlay_value(id, buf_id))
        .collect();
    Ok(Value::list(overlays))
}

/// (overlays-in BEG END)
pub(crate) fn builtin_overlays_in(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
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
    let ids = buf.overlays.overlays_in(byte_beg, byte_end);
    let overlays: Vec<Value> = ids
        .into_iter()
        .map(|id| make_overlay_value(id, buf_id))
        .collect();
    Ok(Value::list(overlays))
}

/// (move-overlay OVERLAY BEG END &optional BUFFER)
pub(crate) fn builtin_move_overlay(
    eval: &mut super::eval::Evaluator,
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
    let (ov_id, buf_id) = expect_overlay(&args[0])?;
    ensure_marker_points_into_buffer(buffers, &args[1], buf_id)?;
    ensure_marker_points_into_buffer(buffers, &args[2], buf_id)?;
    let mut beg = expect_integer_or_marker_in_buffers(buffers, &args[1])?;
    let mut end = expect_integer_or_marker_in_buffers(buffers, &args[2])?;
    if beg > end {
        std::mem::swap(&mut beg, &mut end);
    }
    // Optional BUFFER argument — if given, we'd need to move between buffers.
    // For simplicity, we move within the same buffer.
    let _new_buf = args.get(3);

    let buf = buffers
        .get_mut(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let byte_beg = elisp_pos_to_byte(buf, beg);
    let byte_end = elisp_pos_to_byte(buf, end);
    buf.overlays.move_overlay(ov_id, byte_beg, byte_end);
    Ok(args[0])
}

/// (overlay-start OVERLAY)
pub(crate) fn builtin_overlay_start(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_overlay_start_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_overlay_start_in_buffers(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("overlay-start", &args, 1)?;
    let (ov_id, buf_id) = expect_overlay(&args[0])?;

    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    match buf.overlays.overlay_start(ov_id) {
        Some(byte_pos) => Ok(Value::Int(byte_to_elisp_pos(buf, byte_pos))),
        None => Ok(Value::Nil),
    }
}

/// (overlay-end OVERLAY)
pub(crate) fn builtin_overlay_end(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_overlay_end_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_overlay_end_in_buffers(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("overlay-end", &args, 1)?;
    let (ov_id, buf_id) = expect_overlay(&args[0])?;

    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    match buf.overlays.overlay_end(ov_id) {
        Some(byte_pos) => Ok(Value::Int(byte_to_elisp_pos(buf, byte_pos))),
        None => Ok(Value::Nil),
    }
}

/// (overlay-buffer OVERLAY)
pub(crate) fn builtin_overlay_buffer(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_overlay_buffer_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_overlay_buffer_in_buffers(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("overlay-buffer", &args, 1)?;
    let (ov_id, buf_id) = expect_overlay(&args[0])?;

    // Check if the overlay still exists in the buffer.
    if let Some(buf) = buffers.get(buf_id) {
        if buf.overlays.get(ov_id).is_some() {
            return Ok(Value::Buffer(buf_id));
        }
    }
    Ok(Value::Nil)
}

/// (overlay-properties OVERLAY)
pub(crate) fn builtin_overlay_properties(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_overlay_properties_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_overlay_properties_in_buffers(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("overlay-properties", &args, 1)?;
    let (ov_id, buf_id) = expect_overlay(&args[0])?;

    if let Some(buf) = buffers.get(buf_id) {
        if let Some(ov) = buf.overlays.get(ov_id) {
            return Ok(hashmap_to_plist(&ov.properties));
        }
    }
    Ok(Value::Nil)
}

/// (remove-overlays &optional BEG END NAME VAL)
pub(crate) fn builtin_remove_overlays(
    eval: &mut super::eval::Evaluator,
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
    let ids = buf.overlays.overlays_in(start_pos, end_pos);

    // Filter and delete.
    for ov_id in ids {
        let should_delete = match (&filter_name, &filter_val) {
            (Some(name), Some(val)) => buf
                .overlays
                .overlay_get(ov_id, name)
                .is_some_and(|v| equal_value(v, val, 0)),
            (Some(name), None) => buf.overlays.overlay_get(ov_id, name).is_some(),
            _ => true,
        };
        if should_delete {
            buf.overlays.delete_overlay(ov_id);
        }
    }

    Ok(Value::Nil)
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
#[path = "textprop_test.rs"]
mod tests;
