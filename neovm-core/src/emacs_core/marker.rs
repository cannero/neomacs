//! Marker builtins for the Elisp interpreter.
//!
//! Markers track positions in buffers and adjust when text is inserted or
//! deleted before them. GNU Emacs exposes markers as first-class marker
//! objects, not vectors, so NeoVM keeps them as heap objects too.
//!
//! Pure builtins:
//!   `markerp`, `marker-position`, `marker-buffer`,
//!   `marker-insertion-type`, `set-marker-insertion-type`,
//!   `copy-marker`, `make-marker`
//!
//! Eval-dependent builtins:
//!   `set-marker`, `move-marker`, `point-marker`, `point-min-marker`,
//!   `point-max-marker`, `mark-marker`

use super::error::{EvalResult, Flow, signal};
use super::value::*;
use crate::buffer::{BufferId, BufferManager, InsertionType};

// ---------------------------------------------------------------------------
// Marker struct (for documentation / internal helpers)
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub(crate) struct Marker {
    pub buffer: Option<BufferId>,
    pub position: Option<i64>,
    pub insertion_type: bool, // true = advances when text inserted at marker pos
}

// ---------------------------------------------------------------------------
// Argument helpers
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

fn expect_range_args(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), Flow> {
    if args.len() < min || args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Marker value helpers
// ---------------------------------------------------------------------------

const MARK_MARKER_ID: u64 = i64::MAX as u64;

pub(crate) fn is_marker(v: &Value) -> bool {
    matches!(v, Value::Marker(_))
}

pub(crate) fn make_marker_value(
    buffer_id: Option<BufferId>,
    position: Option<i64>,
    insertion_type: bool,
) -> Value {
    make_marker_value_with_id(buffer_id, position, insertion_type, None)
}

pub(crate) fn make_marker_value_with_id(
    buffer_id: Option<BufferId>,
    position: Option<i64>,
    insertion_type: bool,
    marker_id: Option<u64>,
) -> Value {
    with_heap_mut(|heap| {
        Value::Marker(heap.alloc_marker(crate::gc::types::MarkerData {
            buffer: buffer_id,
            position,
            insertion_type,
            marker_id,
        }))
    })
}

pub(crate) fn make_registered_buffer_marker(
    buffers: &mut BufferManager,
    buffer_id: BufferId,
    position: i64,
    insertion_type: bool,
) -> Value {
    let byte_pos = match buffers.get(buffer_id) {
        Some(buffer) => lisp_pos_to_byte(buffer, position),
        None => 0,
    };
    let marker = make_marker_value(Some(buffer_id), Some(position), insertion_type);
    let marker_id = buffers.create_marker(
        buffer_id,
        byte_pos,
        if insertion_type {
            InsertionType::After
        } else {
            InsertionType::Before
        },
    );
    set_marker_id(&marker, marker_id);

    marker
}

pub(crate) fn marker_logical_fields(v: &Value) -> Option<(Option<BufferId>, Option<i64>, bool)> {
    let Value::Marker(id) = v else {
        return None;
    };
    let marker = with_heap(|heap| heap.get_marker(*id).clone());
    Some((marker.buffer, marker.position, marker.insertion_type))
}

pub(crate) fn marker_equal_hash_key(id: crate::gc::ObjId) -> HashKey {
    let marker = with_heap(|heap| heap.get_marker(id).clone());
    HashKey::Text(format!(
        "marker:{:?}:{:?}:{}",
        marker.buffer.map(|buffer| buffer.0),
        marker.position,
        marker.insertion_type
    ))
}

fn marker_id_value(v: &Value) -> Option<u64> {
    let Value::Marker(id) = v else {
        return None;
    };
    with_heap(|heap| heap.get_marker(*id).marker_id)
}

fn is_mark_marker(v: &Value) -> bool {
    marker_id_value(v) == Some(MARK_MARKER_ID)
}

fn set_marker_id(v: &Value, mid: u64) {
    if let Value::Marker(id) = v {
        with_heap_mut(|heap| {
            heap.get_marker_mut(*id).marker_id = Some(mid);
        });
    }
}

/// Assert that a value is a marker and return a wrong-type-argument error if
/// it is not.
fn expect_marker(_name: &str, v: &Value) -> Result<(), Flow> {
    if is_marker(v) {
        Ok(())
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("markerp"), *v],
        ))
    }
}

fn marker_position_value(v: &Value) -> Value {
    let Value::Marker(id) = v else {
        return Value::Nil;
    };
    with_heap(|heap| match heap.get_marker(*id).position {
        Some(position) => Value::Int(position),
        None => Value::Nil,
    })
}

/// Return marker position as an integer.
///
/// Signals `error` when marker is unset, matching Emacs behavior in position
/// contexts that require a concrete marker location.
pub(crate) fn marker_position_as_int(v: &Value) -> Result<i64, Flow> {
    expect_marker("marker-position", v)?;
    match marker_position_value(v) {
        Value::Int(n) => Ok(n),
        _ => Err(signal(
            "error",
            vec![Value::string("Marker does not point anywhere")],
        )),
    }
}

pub(crate) fn marker_position_as_int_with_buffers(
    buffers: &BufferManager,
    v: &Value,
) -> Result<i64, Flow> {
    expect_marker("marker-position", v)?;

    if is_mark_marker(v) {
        if let Some(buf_id) = marker_buffer_id(v)
            && let Some(buf) = buffers.get(buf_id)
        {
            return match buf.mark_char() {
                Some(char_pos) => Ok(char_pos as i64 + 1),
                None => Err(signal(
                    "error",
                    vec![Value::string("Marker does not point anywhere")],
                )),
            };
        }
    }

    if let Some(mid) = marker_id_value(v) {
        if let Some(buf_id) = marker_buffer_id(v)
            && let Some(buf) = buffers.get(buf_id)
            && let Some(marker_entry) = buf.marker_entry(mid)
        {
            return Ok(marker_entry.char_pos as i64 + 1);
        }
    }

    marker_position_as_int(v)
}

pub(crate) fn marker_position_as_int_eval(
    eval: &super::eval::Context,
    v: &Value,
) -> Result<i64, Flow> {
    marker_position_as_int_with_buffers(&eval.buffers, v)
}

fn marker_buffer_value(v: &Value) -> Value {
    let Value::Marker(id) = v else {
        return Value::Nil;
    };
    with_heap(|heap| match heap.get_marker(*id).buffer {
        Some(buffer_id) => Value::Buffer(buffer_id),
        None => Value::Nil,
    })
}

fn marker_insertion_type_value(v: &Value) -> Value {
    let Value::Marker(id) = v else {
        return Value::Nil;
    };
    with_heap(|heap| Value::bool(heap.get_marker(*id).insertion_type))
}

fn marker_buffer_id(v: &Value) -> Option<BufferId> {
    let Value::Marker(id) = v else {
        return None;
    };
    with_heap(|heap| heap.get_marker(*id).buffer)
}

fn lisp_pos_to_byte(buf: &crate::buffer::Buffer, lisp_pos: i64) -> usize {
    // GNU Emacs: set-marker clamps to the full buffer, not the narrowed region.
    buf.lisp_pos_to_full_buffer_byte(lisp_pos)
}

fn marker_targets_current_mark(marker: &Value) -> bool {
    is_mark_marker(marker)
}

// ---------------------------------------------------------------------------
// Pure builtins
// ---------------------------------------------------------------------------

/// (markerp OBJECT) -> t if OBJECT is a marker, nil otherwise
pub(crate) fn builtin_markerp(args: Vec<Value>) -> EvalResult {
    expect_args("markerp", &args, 1)?;
    Ok(Value::bool(is_marker(&args[0])))
}

/// Eval-dependent marker-position that reads adjusted positions from the buffer.
pub(crate) fn builtin_marker_position(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_marker_position_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_marker_position_in_buffers(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("marker-position", &args, 1)?;
    expect_marker("marker-position", &args[0])?;

    if let Some(mid) = marker_id_value(&args[0]) {
        if let Some(buf_id) = marker_buffer_id(&args[0])
            && let Some(buf) = buffers.get(buf_id)
            && let Some(marker_entry) = buf.marker_entry(mid)
        {
            return Ok(Value::Int(marker_entry.char_pos as i64 + 1));
        }
    }

    Ok(marker_position_value(&args[0]))
}

/// Context-aware marker-buffer that returns nil for killed buffers.
/// GNU returns nil when the marker's buffer has been killed.
pub(crate) fn builtin_marker_buffer(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("marker-buffer", &args, 1)?;
    expect_marker("marker-buffer", &args[0])?;
    if let Some(buffer_id) = marker_buffer_id(&args[0])
        && eval.buffers.get(buffer_id).is_some()
    {
        return Ok(Value::Buffer(buffer_id));
    }
    Ok(Value::Nil)
}

/// Buffer-aware marker-buffer for the VM fast dispatch path.
/// Returns nil for killed buffers (same as the eval-aware version).
pub(crate) fn builtin_marker_buffer_in_buffers(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("marker-buffer", &args, 1)?;
    expect_marker("marker-buffer", &args[0])?;
    if let Some(buffer_id) = marker_buffer_id(&args[0])
        && buffers.get(buffer_id).is_some()
    {
        return Ok(Value::Buffer(buffer_id));
    }
    Ok(Value::Nil)
}

/// (marker-insertion-type MARKER) -> t or nil
pub(crate) fn builtin_marker_insertion_type(args: Vec<Value>) -> EvalResult {
    expect_args("marker-insertion-type", &args, 1)?;
    expect_marker("marker-insertion-type", &args[0])?;
    Ok(marker_insertion_type_value(&args[0]))
}

/// Eval-dependent set-marker-insertion-type that also updates the buffer's
/// marker entry so insertion behavior changes immediately.
pub(crate) fn builtin_set_marker_insertion_type(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_set_marker_insertion_type_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_set_marker_insertion_type_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-marker-insertion-type", &args, 2)?;
    expect_marker("set-marker-insertion-type", &args[0])?;
    let new_type = args[1].is_truthy();
    if let Value::Marker(id) = &args[0] {
        with_heap_mut(|heap| {
            heap.get_marker_mut(*id).insertion_type = new_type;
        });
    }

    if let Some(mid) = marker_id_value(&args[0]) {
        let ins_type = if new_type {
            InsertionType::After
        } else {
            InsertionType::Before
        };
        buffers.update_marker_insertion_type(mid, ins_type);
    }

    Ok(args[1])
}

/// (make-marker) -> new empty marker (no buffer, no position)
pub(crate) fn builtin_make_marker(args: Vec<Value>) -> EvalResult {
    expect_args("make-marker", &args, 0)?;
    Ok(make_marker_value(None, None, false))
}

/// Eval-dependent copy-marker that registers the new marker in the buffer.
pub(crate) fn builtin_copy_marker(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_copy_marker_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_copy_marker_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("copy-marker", &args, 1, 2)?;
    let insertion_type = if args.len() > 1 {
        args[1].is_truthy()
    } else {
        false
    };

    match &args[0] {
        v if is_marker(v) => {
            let buffer_id = marker_buffer_id(v);

            let position = if is_mark_marker(v) {
                buffer_id
                    .and_then(|buf_id| buffers.get(buf_id))
                    .and_then(|buf| buf.mark_char().map(|m| m as i64 + 1))
            } else if let Some(mid) = marker_id_value(v) {
                buffer_id
                    .and_then(|buf_id| buffers.get(buf_id))
                    .and_then(|buf| buf.marker_entry(mid))
                    .map(|marker| marker.char_pos as i64 + 1)
            } else {
                match marker_position_value(v) {
                    Value::Int(n) => Some(n),
                    _ => None,
                }
            };

            let marker = make_marker_value(buffer_id, position, insertion_type);
            register_marker_in_buffers(buffers, &marker, buffer_id, position);
            Ok(marker)
        }
        Value::Int(n) => {
            let buffer_id = buffers.current_buffer().map(|b| b.id);
            let marker = make_marker_value(buffer_id, Some(*n), insertion_type);
            register_marker_in_buffers(buffers, &marker, buffer_id, Some(*n));
            Ok(marker)
        }
        Value::Nil => Ok(make_marker_value(None, None, insertion_type)),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *other],
        )),
    }
}

// ---------------------------------------------------------------------------
// Eval-dependent builtins
// ---------------------------------------------------------------------------

/// (set-marker MARKER POSITION &optional BUFFER) -> MARKER
///
/// Set the position and (optionally) the buffer of MARKER.  If POSITION is
/// nil, the marker is unset (points nowhere).  BUFFER defaults to the current
/// buffer.
pub(crate) fn builtin_set_marker(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_set_marker_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_set_marker_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("set-marker", &args, 2, 3)?;
    expect_marker("set-marker", &args[0])?;

    let targets_current_mark = marker_targets_current_mark(&args[0]);

    let buffer_id: Option<BufferId> = if args.len() > 2 && args[2].is_truthy() {
        match &args[2] {
            Value::Str(sid) => {
                let name = with_heap(|h| h.get_string(*sid).to_owned());
                buffers.find_buffer_by_name(&name)
            }
            Value::Buffer(id) => Some(*id),
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), *other],
                ));
            }
        }
    } else {
        // Default to current buffer
        buffers.current_buffer().map(|b| b.id)
    };

    // Resolve position
    let position: Option<i64> = match &args[1] {
        Value::Nil => None,
        Value::Int(n) => Some(*n),
        v if is_marker(v) => marker_position_as_int_with_buffers(buffers, v).ok(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("integer-or-marker-p"), *other],
            ));
        }
    };

    // GNU Emacs: when position is nil, the marker is detached from its buffer.
    let buffer_id = position.and(buffer_id);

    // Clamp position to the full buffer range (1 .. total_chars+1), matching
    // GNU Emacs which clamps to the whole buffer, ignoring narrowing.
    let position = match (position, buffer_id) {
        (Some(pos), Some(buf_id)) => {
            if let Some(buf) = buffers.get(buf_id) {
                let max_pos = buf.total_chars() as i64 + 1;
                Some(pos.clamp(1, max_pos))
            } else {
                Some(pos)
            }
        }
        (other, _) => other,
    };

    register_marker_in_buffers(buffers, &args[0], buffer_id, position);

    if let Value::Marker(id) = &args[0] {
        with_heap_mut(|heap| {
            let marker = heap.get_marker_mut(*id);
            marker.buffer = buffer_id;
            marker.position = position;
        });
    }

    if targets_current_mark {
        let target_buf_id = buffer_id.or_else(|| buffers.current_buffer().map(|buf| buf.id));
        if let Some(buf_id) = target_buf_id {
            match position {
                Some(pos) => {
                    if let Some(byte_pos) =
                        buffers.get(buf_id).map(|buf| lisp_pos_to_byte(buf, pos))
                    {
                        let _ = buffers.set_buffer_mark(buf_id, byte_pos);
                    }
                }
                None => {
                    let _ = buffers.clear_buffer_mark(buf_id);
                }
            }
        }
    }

    Ok(args[0])
}

/// (move-marker MARKER POSITION &optional BUFFER) -> MARKER
///
/// GNU Emacs exposes `move-marker` as the marker-moving primitive used by
/// Lisp code such as `indent.el`. Its observable behavior matches
/// `set-marker`, so reuse that implementation.
pub(crate) fn builtin_move_marker(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_move_marker_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_move_marker_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    builtin_set_marker_in_buffers(buffers, args)
}

/// Register a Lisp marker in the target buffer's marker list so that
/// insert/delete operations automatically adjust its position.
fn register_marker_in_buffer(
    eval: &mut super::eval::Context,
    marker: &Value,
    buffer_id: Option<BufferId>,
    position: Option<i64>,
) {
    register_marker_in_buffers(&mut eval.buffers, marker, buffer_id, position);
}

fn register_marker_in_buffers(
    buffers: &mut BufferManager,
    marker: &Value,
    buffer_id: Option<BufferId>,
    position: Option<i64>,
) {
    if is_mark_marker(marker) {
        return;
    }

    // Read insertion type from marker vector
    let insertion_type_val = marker_insertion_type_value(marker);
    let ins_type = if insertion_type_val.is_truthy() {
        crate::buffer::InsertionType::After
    } else {
        crate::buffer::InsertionType::Before
    };

    // Get or assign a marker-id
    let existing_mid = marker_id_value(marker);

    // Remove old registration from all buffers
    if let Some(mid) = existing_mid {
        buffers.remove_marker(mid);
    }

    if let (Some(buf_id), Some(pos)) = (buffer_id, position) {
        let mid = existing_mid.unwrap_or_else(|| buffers.allocate_marker_id());
        set_marker_id(marker, mid);
        if let Some(byte_pos) = buffers.get(buf_id).map(|buf| lisp_pos_to_byte(buf, pos)) {
            let _ = buffers.register_marker_id(buf_id, mid, byte_pos, ins_type);
        }
    }
}

/// (point-marker) -> marker at current point
pub(crate) fn builtin_point_marker(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_point_marker_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_point_marker_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("point-marker", &args, 0)?;
    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let pos = buf.point_char() as i64 + 1; // 1-based
    let buffer_id = buf.id;
    let marker = make_marker_value(Some(buffer_id), Some(pos), false);
    register_marker_in_buffers(buffers, &marker, Some(buffer_id), Some(pos));
    Ok(marker)
}

/// (point-min-marker) -> marker at point-min
pub(crate) fn builtin_point_min_marker(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_point_min_marker_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_point_min_marker_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("point-min-marker", &args, 0)?;
    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let pos = buf.point_min_char() as i64 + 1; // 1-based
    let buffer_id = buf.id;
    let marker = make_marker_value(Some(buffer_id), Some(pos), false);
    register_marker_in_buffers(buffers, &marker, Some(buffer_id), Some(pos));
    Ok(marker)
}

/// (point-max-marker) -> marker at point-max
pub(crate) fn builtin_point_max_marker(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_point_max_marker_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_point_max_marker_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("point-max-marker", &args, 0)?;
    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let pos = buf.point_max_char() as i64 + 1; // 1-based
    let buffer_id = buf.id;
    let marker = make_marker_value(Some(buffer_id), Some(pos), false);
    register_marker_in_buffers(buffers, &marker, Some(buffer_id), Some(pos));
    Ok(marker)
}

/// (mark-marker) -> marker at mark, or error if no mark set
pub(crate) fn builtin_mark_marker(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_mark_marker_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_mark_marker_in_buffers(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("mark-marker", &args, 0)?;
    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buffer_id = buf.id;
    match buf.mark_char() {
        Some(char_pos) => {
            let pos = char_pos as i64 + 1; // 1-based
            Ok(make_marker_value_with_id(
                Some(buffer_id),
                Some(pos),
                false,
                Some(MARK_MARKER_ID),
            ))
        }
        None => Ok(make_marker_value_with_id(
            Some(buffer_id),
            None,
            false,
            Some(MARK_MARKER_ID),
        )),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "marker_test.rs"]
mod tests;
