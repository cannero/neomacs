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
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *value],
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
/// Context-dependent variant used during normal execution: inserts an
/// undo boundary into the current buffer's undo list.
pub(crate) fn builtin_undo_boundary(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("undo-boundary", &args, 0)?;
    let Some(current_id) = ctx.buffers.current_buffer_id() else {
        return Err(signal("error", vec![Value::string("No current buffer")]));
    };
    let _ = ctx.buffers.add_undo_boundary(current_id);
    Ok(Value::NIL)
}

/// (primitive-undo COUNT LIST) -> remainder of LIST
///
/// Undo COUNT undo-groups from LIST, applying each entry to the current
/// buffer.  Returns the unconsumed tail of LIST.
///
/// Matches GNU Emacs's `primitive-undo` (simple.el:3642-3777).
///
/// Entry types handled:
/// - Integer POS: `(goto-char POS)`
/// - `(BEG . END)` both ints: delete the region (undo an insertion)
/// - `(TEXT . POS)` string+int: insert TEXT at |POS| (undo a deletion)
/// - `(t . MODTIME)`: restore buffer-modified state
/// - `(nil PROP VAL BEG . END)`: restore text property (TODO: partial)
/// - `(MARKER . OFFSET)`: adjust marker (skipped)
/// - `(apply FUN . ARGS)`: call FUN with ARGS
pub(crate) fn builtin_primitive_undo(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("primitive-undo", &args, 2)?;

    let count = expect_int(&args[0])?;
    expect_list_like(&args[1])?;

    if count <= 0 {
        return Ok(args[1]);
    }

    let Some(buf_id) = ctx.buffers.current_buffer_id() else {
        return Err(signal("error", vec![Value::string("No current buffer")]));
    };

    // Save and set inhibit-read-only to t during undo.
    let saved_inhibit = ctx.obarray.symbol_value("inhibit-read-only").copied();
    ctx.obarray
        .set_symbol_value("inhibit-read-only", Value::T);

    // Mark undo as in-progress so that the buffer edits we make
    // do NOT record new undo entries (they are reverse-operations).
    let previous_undoing = ctx
        .buffers
        .get(buf_id)
        .map(|b| b.undo_state.in_progress())
        .unwrap_or(false);
    if let Some(buf) = ctx.buffers.get_mut(buf_id) {
        buf.undo_state.set_in_progress(true);
    }

    let result = primitive_undo_inner(ctx, buf_id, count, args[1]);

    // Restore undo-in-progress flag.
    if let Some(buf) = ctx.buffers.get_mut(buf_id) {
        buf.undo_state.set_in_progress(previous_undoing);
    }

    // Restore inhibit-read-only.
    match saved_inhibit {
        Some(v) => ctx.obarray.set_symbol_value("inhibit-read-only", v),
        None => ctx
            .obarray
            .set_symbol_value("inhibit-read-only", ValueKind::Nil),
    }

    result
}

/// Inner loop: process COUNT undo groups from LIST, return unconsumed tail.
fn primitive_undo_inner(
    ctx: &mut super::eval::Context,
    buf_id: crate::buffer::BufferId,
    count: i64,
    mut list: Value,
) -> EvalResult {
    let mut groups_done = 0i64;

    while groups_done < count && list.is_cons() {
        // Skip leading nil boundaries.
        while list.is_cons() && list.cons_car().is_nil() {
            list = list.cons_cdr();
        }

        if !list.is_cons() {
            break;
        }

        // Process one undo group (entries until next nil or end).
        while list.is_cons() {
            let entry = list.cons_car();
            list = list.cons_cdr();

            if entry.is_nil() {
                // Hit boundary — end of this group.
                break;
            }

            // Integer POS: goto-char
            if let Some(pos1) = entry.as_fixnum() {
                let pos = (pos1 - 1).max(0) as usize;
                let clamped = ctx
                    .buffers
                    .get(buf_id)
                    .map(|b| pos.min(b.text.len()))
                    .unwrap_or(0);
                ctx.buffers.goto_buffer_byte(buf_id, clamped);
                continue;
            }

            if !entry.is_cons() {
                // Unknown non-cons, non-int entry — skip.
                continue;
            }

            let car = entry.cons_car();
            let cdr = entry.cons_cdr();

            match (car.kind(), cdr.kind()) {
                // (BEG . END) both integers — undo an insertion by deleting.
                (ValueKind::Fixnum(beg1), ValueKind::Fixnum(end1)) => {
                    let beg = (beg1 - 1).max(0) as usize;
                    let end = (end1 - 1).max(0) as usize;
                    if let Some(buf) = ctx.buffers.get(buf_id) {
                        let clamped_end = end.min(buf.text.len());
                        let clamped_beg = beg.min(clamped_end);
                        ctx.buffers
                            .delete_buffer_region(buf_id, clamped_beg, clamped_end);
                    }
                }
                // (TEXT . POS) string + int — undo a deletion by re-inserting.
                (ValueKind::String, ValueKind::Fixnum(pos1)) => {
                    let text = car.as_str_owned().unwrap_or_default();
                    let pos = (pos1.abs() - 1).max(0) as usize;
                    if let Some(buf) = ctx.buffers.get(buf_id) {
                        let clamped = pos.min(buf.text.len());
                        ctx.buffers.goto_buffer_byte(buf_id, clamped);
                        ctx.buffers.insert_into_buffer(buf_id, &text);
                        // If POS was negative, point should be at end of
                        // inserted text (which insert_into_buffer already does).
                        // If positive, move point back to start of insertion.
                        if pos1 > 0 {
                            ctx.buffers.goto_buffer_byte(buf_id, clamped);
                        }
                    }
                }
                // (t . MODTIME) — restore buffer-modified state.
                (ValueKind::T, ValueKind::Fixnum(modtime)) => {
                    if modtime == 0 {
                        // modtime 0 means mark buffer as unmodified.
                        let _ = ctx.buffers.set_buffer_modified_flag(buf_id, false);
                    }
                    // Non-zero modtimes would compare against file modtime;
                    // for now we just skip those.
                }
                // (nil PROP VAL BEG . END) — restore text property.
                (ValueKind::Nil, _) => {
                    // cdr is (PROP VAL BEG . END)
                    if cdr.is_cons() {
                        let prop = cdr.cons_car();
                        let rest1 = cdr.cons_cdr();
                        if rest1.is_cons() {
                            let val = rest1.cons_car();
                            let rest2 = rest1.cons_cdr();
                            if rest2.is_cons() {
                                let beg_val = rest2.cons_car();
                                let end_val = rest2.cons_cdr();
                                if let (Value::fixnum(b), Value::fixnum(e)) = (beg_val, end_val) {
                                    let byte_beg = (b - 1).max(0) as usize;
                                    let byte_end = (e - 1).max(0) as usize;
                                    if let Some(prop_name) = prop.as_symbol_name() {
                                        if val.is_nil() {
                                            let _ = ctx.buffers.remove_buffer_text_property(
                                                buf_id, byte_beg, byte_end, prop_name,
                                            );
                                        } else {
                                            let _ = ctx.buffers.put_buffer_text_property(
                                                buf_id, byte_beg, byte_end, prop_name, val,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // (apply FUN . ARGS) — call FUN with ARGS.
                _ if car.as_symbol_name() == Some("apply") => {
                    if cdr.is_cons() {
                        let fun = cdr.cons_car();
                        let rest = cdr.cons_cdr();
                        let mut fargs = Vec::new();
                        let mut cursor = rest;
                        while cursor.is_cons() {
                            fargs.push(cursor.cons_car());
                            cursor = cursor.cons_cdr();
                        }
                        // Best-effort: ignore errors from undo apply calls.
                        let _ = ctx.funcall_general(fun, fargs);
                    }
                }
                // (MARKER . OFFSET) — adjust marker; skip for now.
                (ValueKind::Veclike(VecLikeType::Marker), ValueKind::Fixnum(_)) => {
                    // Marker adjustment is rarely critical; skip.
                }
                _ => {
                    // Unknown entry type — skip.
                }
            }
        }
        groups_done += 1;
    }

    Ok(list)
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
