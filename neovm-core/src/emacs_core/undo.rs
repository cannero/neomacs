//! Undo system -- buffer undo/redo functionality.
//!
//! Provides Emacs-compatible undo functionality:
//! - `undo-boundary` -- insert an undo boundary marker
//! - `primitive-undo` -- undo entries from an undo list
//! - `undo` -- undo the last change in the current buffer

use super::error::{EvalResult, Flow, signal};
use super::value::*;
use crate::buffer::undo::UndoRecord;

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
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *other],
        )),
    }
}

fn expect_list_like(value: &Value) -> Result<(), Flow> {
    if value.is_nil() || value.is_cons() {
        Ok(())
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), *value],
        ))
    }
}

// ---------------------------------------------------------------------------
// Pure builtins
// ---------------------------------------------------------------------------

/// (undo-boundary) -> nil
///
/// Insert an undo boundary marker in the current buffer's undo list.
/// This separates consecutive edits into distinct undoable actions.
///
pub(crate) fn builtin_undo_boundary(args: Vec<Value>) -> EvalResult {
    expect_args("undo-boundary", &args, 0)?;
    Ok(Value::Nil)
}

/// (undo-boundary) -> nil
///
/// Evaluator-dependent variant used during normal execution: inserts an
/// undo boundary into the current buffer's undo list.
pub(crate) fn builtin_undo_boundary_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("undo-boundary", &args, 0)?;
    let Some(buffer) = eval.buffers.current_buffer_mut() else {
        return Err(signal("error", vec![Value::string("No current buffer")]));
    };
    buffer.undo_list.boundary();
    Ok(Value::Nil)
}

/// (primitive-undo COUNT LIST) -> remainder of LIST
///
/// Undo COUNT entries from the undo list LIST.
/// Returns the remainder of the list after removing COUNT entries.
///
/// Each entry in LIST should be a marker for an undoable action.
/// In a full implementation, each entry would be applied in reverse order.
///
pub(crate) fn builtin_primitive_undo(args: Vec<Value>) -> EvalResult {
    expect_args("primitive-undo", &args, 2)?;

    // Verify COUNT is an integer
    let _count = expect_int(&args[0])?;
    expect_list_like(&args[1])?;

    // NeoVM's higher-level `undo` applies buffer edits directly.
    // `primitive-undo` currently acts as a type-checked list passthrough.
    Ok(args[1])
}

// ---------------------------------------------------------------------------
// Eval-dependent builtins
// ---------------------------------------------------------------------------

/// (undo &optional ARG) -> nil
///
/// Undo the last change in the current buffer.
/// ARG is the number of undo commands to execute (default 1).
///
/// In a full implementation, this would:
/// 1. Get the current buffer's undo list
/// 2. Apply primitive-undo to reverse the specified number of actions
/// 3. Update buffer state accordingly
///
pub(crate) fn builtin_undo(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("undo", &args, 0)?;
    expect_max_args("undo", &args, 1)?;

    // If ARG is provided, verify it's an integer
    let mut count = 1i64;
    if let Some(arg) = args.first() {
        count = expect_int(arg)?;
    }

    let Some(buffer) = eval.buffers.current_buffer_mut() else {
        return Err(signal("error", vec![Value::string("No current buffer")]));
    };

    let had_any_records = !buffer.undo_list.is_empty();
    let had_boundary = buffer.undo_list.contains_boundary();
    let had_trailing_boundary = buffer.undo_list.has_trailing_boundary();

    // Emacs returns "Undo" for non-positive ARG when there are grouped
    // (boundary-separated) undo entries, without applying a change.
    if count <= 0 && had_boundary {
        return Ok(Value::string("Undo"));
    }
    // For non-positive ARG without boundary markers, Emacs still consumes one
    // undo group and reports "No further undo information".
    if count <= 0 {
        count = 1;
    }

    let previous_undoing = buffer.undo_list.undoing;
    buffer.undo_list.undoing = true;
    let mut applied_any = false;
    let groups_to_undo = if had_trailing_boundary {
        count as usize
    } else {
        (count as usize).saturating_add(1)
    };

    for _ in 0..groups_to_undo {
        let group = buffer.undo_list.pop_undo_group();
        if group.is_empty() {
            break;
        }
        applied_any = true;

        for record in group {
            match record {
                UndoRecord::Insert { pos, len } => {
                    let end = pos.saturating_add(len).min(buffer.text.len());
                    buffer.delete_region(pos.min(end), end);
                }
                UndoRecord::Delete { pos, text } => {
                    let clamped = pos.min(buffer.text.len());
                    buffer.goto_char(clamped);
                    buffer.insert(&text);
                }
                UndoRecord::CursorMove { pos } => {
                    buffer.goto_char(pos.min(buffer.text.len()));
                }
                UndoRecord::PropertyChange { .. }
                | UndoRecord::FirstChange { .. }
                | UndoRecord::Boundary => {
                    // Text property undo entries are intentionally ignored for now.
                }
            }
        }
    }

    buffer.undo_list.undoing = previous_undoing;
    if !applied_any {
        let msg = if had_any_records {
            "No further undo information"
        } else {
            "No undo information in this buffer"
        };
        return Err(signal("user-error", vec![Value::string(msg)]));
    }

    if had_boundary {
        Ok(Value::string("Undo"))
    } else {
        Err(signal(
            "user-error",
            vec![Value::string("No further undo information")],
        ))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "undo_test.rs"]
mod tests;
