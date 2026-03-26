//! Undo system -- buffer undo/redo functionality.
//!
//! Provides Emacs-compatible undo functionality:
//! - `undo-boundary` -- insert an undo boundary marker
//! - `primitive-undo` -- undo entries from an undo list
//! - `undo` -- undo the last change in the current buffer

use super::error::{EvalResult, Flow, signal};
use super::value::*;

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
/// Context-dependent variant used during normal execution: inserts an
/// undo boundary into the current buffer's undo list.
pub(crate) fn builtin_undo_boundary_eval(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_undo_boundary_in_state(eval, args)
}

pub(crate) fn builtin_undo_boundary_in_state(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("undo-boundary", &args, 0)?;
    let Some(current_id) = ctx.buffers.current_buffer_id() else {
        return Err(signal("error", vec![Value::string("No current buffer")]));
    };
    let _ = ctx.buffers.add_undo_boundary(current_id);
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
pub(crate) fn builtin_undo(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("undo", &args, 0)?;
    expect_max_args("undo", &args, 1)?;

    // If ARG is provided, verify it's an integer
    let mut count = 1i64;
    if let Some(arg) = args.first() {
        count = expect_int(arg)?;
    }

    let Some(current_id) = eval.buffers.current_buffer_id() else {
        return Err(signal("error", vec![Value::string("No current buffer")]));
    };
    let outcome = eval
        .buffers
        .undo_buffer(current_id, count)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    if outcome.skipped_apply {
        return Ok(Value::string("Undo"));
    }

    if !outcome.applied_any {
        let msg = if outcome.had_any_records {
            "No further undo information"
        } else {
            "No undo information in this buffer"
        };
        return Err(signal("user-error", vec![Value::string(msg)]));
    }

    if outcome.had_boundary {
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
