//! Editing-function builtins — point/mark queries, insertion, deletion,
//! substring extraction, and miscellaneous user/system info.
//!
//! Emacs Lisp uses **1-based character positions** while the internal
//! `Buffer` stores **0-based Emacs-byte positions**.  Every Lisp↔Buffer boundary
//! must convert:
//!
//! - Lisp char pos  →  byte pos:  `buf.text.char_to_emacs_byte(lisp_pos - 1)`
//! - byte pos       →  Lisp char: `buf.text.emacs_byte_to_char(byte_pos) + 1`

use super::error::{EvalResult, Flow, signal};
use super::intern::intern;
use super::symbol::Obarray;
use super::value::*;
use crate::buffer::{Buffer, BufferManager};
use crate::emacs_core::value::ValueKind;
#[cfg(unix)]
use std::ffi::CStr;

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

/// Extract an integer (or char-as-integer) from a Value, signalling
/// `wrong-type-argument` on type mismatch.
fn expect_integer(_name: &str, val: &Value) -> Result<i64, Flow> {
    val.as_int().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *val],
        )
    })
}

/// Convert a Lisp 1-based character position to a 0-based byte position,
/// clamping to the accessible region `[begv, zv]`.
pub(crate) fn lisp_pos_to_byte(buf: &crate::buffer::Buffer, lisp_pos: i64) -> usize {
    buf.lisp_pos_to_accessible_byte(lisp_pos)
}

fn dynamic_buffer_or_global_symbol_value(
    obarray: &Obarray,
    _dynamic: &[OrderedRuntimeBindingMap],
    buf: Option<&Buffer>,
    name: &str,
) -> Option<Value> {
    if let Some(buf) = buf
        && let Some(value) = buf.get_buffer_local(name)
    {
        return Some(value);
    }

    obarray.symbol_value(name).copied()
}

pub(crate) fn buffer_read_only_active_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buf: &Buffer,
) -> bool {
    if let Some(value) = buf.get_buffer_local("inhibit-read-only")
        && value.is_truthy()
    {
        return false;
    }

    if obarray
        .symbol_value("inhibit-read-only")
        .is_some_and(|value| value.is_truthy())
    {
        return false;
    }

    if buf.get_read_only() {
        return true;
    }

    dynamic_buffer_or_global_symbol_value(obarray, dynamic, Some(buf), "buffer-read-only")
        .is_some_and(|value| value.is_truthy())
}

pub(crate) fn ensure_current_buffer_writable_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &BufferManager,
) -> Result<(), Flow> {
    if let Some(buf) = buffers.current_buffer()
        && buffer_read_only_active_in_state(obarray, dynamic, buf)
    {
        return Err(signal("buffer-read-only", vec![Value::make_buffer(buf.id)]));
    }
    Ok(())
}

fn ensure_current_buffer_writable(eval: &super::eval::Context) -> Result<(), Flow> {
    ensure_current_buffer_writable_in_state(&eval.obarray, &[], &eval.buffers)
}

pub(crate) fn byte_span_char_len(buf: &crate::buffer::Buffer, beg: usize, end: usize) -> usize {
    let lo = beg.min(end);
    let hi = beg.max(end);
    buf.text
        .emacs_byte_to_char(hi)
        .saturating_sub(buf.text.emacs_byte_to_char(lo))
}

pub(crate) fn current_buffer_byte_span_char_len(
    ctx: &crate::emacs_core::eval::Context,
    beg: usize,
    end: usize,
) -> usize {
    ctx.buffers
        .current_buffer()
        .map(|buf| byte_span_char_len(buf, beg, end))
        .unwrap_or_else(|| end.abs_diff(beg))
}

// ---------------------------------------------------------------------------
// Buffer modification hooks — GNU Emacs signal_before_change / signal_after_change
// ---------------------------------------------------------------------------

/// Check whether `inhibit-modification-hooks` is non-nil.
fn inhibit_modification_hooks(ctx: &crate::emacs_core::eval::Context) -> bool {
    let sym =
        crate::emacs_core::hook_runtime::hook_symbol_by_name(ctx, "inhibit-modification-hooks");
    crate::emacs_core::hook_runtime::hook_value_by_id(ctx, sym).is_some_and(|v| v.is_truthy())
}

fn run_named_hook_reset_on_error(
    ctx: &mut crate::emacs_core::eval::Context,
    hook_name: &str,
    hook_args: &[Value],
) -> Result<(), Flow> {
    let hook_sym = crate::emacs_core::hook_runtime::hook_symbol_by_name(ctx, hook_name);
    let hook_value =
        crate::emacs_core::hook_runtime::hook_value_by_id(ctx, hook_sym).unwrap_or(Value::NIL);
    if hook_value.is_nil() {
        return Ok(());
    }
    match crate::emacs_core::hook_runtime::run_hook_value(
        ctx, hook_sym, hook_value, hook_args, true,
    ) {
        Ok(_) => Ok(()),
        Err(flow) => {
            let _ = ctx.set_runtime_binding_by_id(hook_sym, Value::NIL);
            Err(flow)
        }
    }
}

fn run_named_hook_without_reset(
    ctx: &mut crate::emacs_core::eval::Context,
    hook_name: &str,
    hook_args: &[Value],
) -> Result<(), Flow> {
    let hook_sym = crate::emacs_core::hook_runtime::hook_symbol_by_name(ctx, hook_name);
    let hook_value =
        crate::emacs_core::hook_runtime::hook_value_by_id(ctx, hook_sym).unwrap_or(Value::NIL);
    if hook_value.is_nil() {
        return Ok(());
    }
    let _ = crate::emacs_core::hook_runtime::run_hook_value(
        ctx, hook_sym, hook_value, hook_args, true,
    )?;
    Ok(())
}

/// GNU `signal_before_change(beg, end)` — run `before-change-functions` and
/// overlay `modification-hooks` before a buffer modification.
///
/// `beg` and `end` are **byte positions** (0-based).  They are converted to
/// 1-based character positions for the Lisp hooks.
pub(crate) fn signal_before_change(
    ctx: &mut crate::emacs_core::eval::Context,
    beg: usize,
    end: usize,
) -> Result<(), Flow> {
    if let Some(current_id) = ctx.buffers.current_buffer_id() {
        let undo_enabled = ctx
            .buffers
            .get(current_id)
            .is_some_and(|buf| !buf.get_undo_list().is_t());
        if undo_enabled && ctx.obarray.fboundp("undo-auto--undoable-change") {
            ctx.apply(Value::symbol("undo-auto--undoable-change"), vec![])?;
        }
    }

    if inhibit_modification_hooks(ctx) {
        return Ok(());
    }

    let Some(current_id) = ctx.buffers.current_buffer_id() else {
        return Ok(());
    };

    if ctx.treesit.has_editable_tree(current_id)
        && let Some(buf) = ctx.buffers.get(current_id)
    {
        let source = buf.buffer_substring_lisp_string(buf.begv_byte, buf.zv_byte);
        ctx.treesit
            .begin_buffer_edit(current_id, &source, beg.min(end), beg.max(end));
    }

    // Convert byte positions to 1-based character positions.
    let (lisp_beg, lisp_end) = {
        let Some(buf) = ctx.buffers.get(current_id) else {
            return Ok(());
        };
        let beg_char = buf.text.emacs_byte_to_char(beg) as i64 + 1;
        let end_char = buf.text.emacs_byte_to_char(end) as i64 + 1;
        (beg_char, end_char)
    };

    let hook_args = vec![Value::fixnum(lisp_beg), Value::fixnum(lisp_end)];
    let run_first_change = ctx
        .buffers
        .get(current_id)
        .is_some_and(|buf| buf.modified_state_value().is_nil());
    let overlay_hooks = collect_overlay_modification_hooks(ctx, beg, end);
    let specpdl_count = ctx.specpdl.len();
    ctx.specbind(intern("inhibit-modification-hooks"), Value::T);
    let result = (|| -> Result<(), Flow> {
        if run_first_change {
            run_named_hook_without_reset(ctx, "first-change-hook", &[])?;
        }
        run_named_hook_reset_on_error(ctx, "before-change-functions", &hook_args)?;

        if !overlay_hooks.is_empty() {
            let overlay_arg = Value::T; // `t` signals "before change" to overlay hooks
            let roots = ctx.save_specpdl_roots();
            for (func, ov_val) in &overlay_hooks {
                ctx.push_specpdl_root(*func);
                ctx.push_specpdl_root(*ov_val);
            }
            let apply_result = (|| -> Result<(), Flow> {
                for (func, ov_val) in &overlay_hooks {
                    ctx.apply(
                        *func,
                        vec![
                            *ov_val,
                            overlay_arg,
                            Value::fixnum(lisp_beg),
                            Value::fixnum(lisp_end),
                        ],
                    )?;
                }
                Ok(())
            })();
            ctx.restore_specpdl_roots(roots);
            apply_result?;
        }

        Ok(())
    })();
    ctx.unbind_to(specpdl_count);
    result
}

/// GNU `signal_after_change(beg, end, old_len)` — run `after-change-functions`
/// and overlay hooks after a buffer modification.
///
/// `beg` and `end` are **byte positions** (0-based, in the *new* buffer state).
/// `old_len` is the character length of the old text that was replaced.
pub(crate) fn signal_after_change(
    ctx: &mut crate::emacs_core::eval::Context,
    beg: usize,
    end: usize,
    old_len: usize,
) -> Result<(), Flow> {
    if inhibit_modification_hooks(ctx) {
        return Ok(());
    }

    let Some(current_id) = ctx.buffers.current_buffer_id() else {
        return Ok(());
    };

    ctx.treesit.note_buffer_change(current_id, beg);
    if ctx.treesit.has_pending_edit(current_id)
        && let Some(buf) = ctx.buffers.get(current_id)
    {
        let source = buf.buffer_substring_lisp_string(buf.begv_byte, buf.zv_byte);
        ctx.treesit.finish_buffer_edit(current_id, &source, end);
    }

    // Convert byte positions to 1-based character positions.
    let (lisp_beg, lisp_end, lisp_old_len) = {
        let Some(buf) = ctx.buffers.get(current_id) else {
            return Ok(());
        };
        let beg_char = buf.text.emacs_byte_to_char(beg) as i64 + 1;
        let end_char = buf.text.emacs_byte_to_char(end) as i64 + 1;
        (beg_char, end_char, old_len as i64)
    };

    let hook_args = vec![
        Value::fixnum(lisp_beg),
        Value::fixnum(lisp_end),
        Value::fixnum(lisp_old_len),
    ];

    let specpdl_count = ctx.specpdl.len();
    ctx.specbind(intern("inhibit-modification-hooks"), Value::T);
    let result = (|| -> Result<(), Flow> {
        run_named_hook_reset_on_error(ctx, "after-change-functions", &hook_args)?;

        // --- Run overlay hooks ---
        // insert-in-front-hooks: overlays whose start == beg
        // insert-behind-hooks:   overlays whose end == beg (before insertion point)
        // modification-hooks:    overlays covering [beg, end)
        run_overlay_after_change_hooks(ctx, beg, end, lisp_beg, lisp_end, lisp_old_len)?;

        Ok(())
    })();
    ctx.unbind_to(specpdl_count);
    result
}

/// Collect `modification-hooks` property functions from overlays overlapping
/// the region `[beg, end)`.  Returns `(hook_function, overlay_as_value)` pairs.
fn collect_overlay_modification_hooks(
    ctx: &crate::emacs_core::eval::Context,
    beg: usize,
    end: usize,
) -> Vec<(Value, Value)> {
    let Some(current_id) = ctx.buffers.current_buffer_id() else {
        return Vec::new();
    };
    let Some(buf) = ctx.buffers.get(current_id) else {
        return Vec::new();
    };

    let search_end = if beg == end { end + 1 } else { end };
    let overlay_ids = buf.overlays.overlays_in(beg, search_end);
    let mut result = Vec::new();
    for ov_id in overlay_ids {
        if let Some(hooks_val) = buf
            .overlays
            .overlay_get_named(ov_id, Value::symbol("modification-hooks"))
        {
            for func in value_list_iter(hooks_val) {
                result.push((func, ov_id));
            }
        }
    }
    result
}

/// Run overlay `insert-in-front-hooks`, `insert-behind-hooks`, and
/// `modification-hooks` after a change.
fn run_overlay_after_change_hooks(
    ctx: &mut crate::emacs_core::eval::Context,
    beg: usize,
    end: usize,
    lisp_beg: i64,
    lisp_end: i64,
    lisp_old_len: i64,
) -> Result<(), Flow> {
    let Some(current_id) = ctx.buffers.current_buffer_id() else {
        return Ok(());
    };

    // Collect all overlay hooks we need to run, then release the borrow on ctx.
    let hooks: Vec<(Value, Value, &'static str)> = {
        let Some(buf) = ctx.buffers.get(current_id) else {
            return Ok(());
        };
        let mut hooks = Vec::new();

        // insert-in-front-hooks: overlays starting at beg
        let front_overlays = buf.overlays.overlays_at(beg);
        for ov_id in &front_overlays {
            let ov_start = buf.overlays.overlay_start(*ov_id);
            if ov_start == Some(beg) {
                if let Some(hook_val) = buf
                    .overlays
                    .overlay_get_named(*ov_id, Value::symbol("insert-in-front-hooks"))
                {
                    for func in value_list_iter(hook_val) {
                        hooks.push((func, *ov_id, "front"));
                    }
                }
            }
        }

        // insert-behind-hooks: overlays ending at beg
        let search_end = if beg == end { end + 1 } else { end };
        let region_overlays = buf
            .overlays
            .overlays_in(if beg > 0 { beg - 1 } else { 0 }, search_end);
        for ov_id in &region_overlays {
            let ov_end = buf.overlays.overlay_end(*ov_id);
            if ov_end == Some(beg) {
                if let Some(hook_val) = buf
                    .overlays
                    .overlay_get_named(*ov_id, Value::symbol("insert-behind-hooks"))
                {
                    for func in value_list_iter(hook_val) {
                        hooks.push((func, *ov_id, "behind"));
                    }
                }
            }
        }

        // modification-hooks: overlays covering [beg, end)
        let mod_overlays = buf.overlays.overlays_in(beg, search_end);
        for ov_id in &mod_overlays {
            if let Some(hook_val) = buf
                .overlays
                .overlay_get_named(*ov_id, Value::symbol("modification-hooks"))
            {
                for func in value_list_iter(hook_val) {
                    hooks.push((func, *ov_id, "mod"));
                }
            }
        }

        hooks
    };

    if hooks.is_empty() {
        return Ok(());
    }

    let after_flag = Value::NIL; // nil signals "after change" to overlay hooks
    let roots = ctx.save_specpdl_roots();
    for (func, ov_val, _) in &hooks {
        ctx.push_specpdl_root(*func);
        ctx.push_specpdl_root(*ov_val);
    }
    let apply_result = (|| -> Result<(), Flow> {
        for (func, ov_val, _) in &hooks {
            ctx.apply(
                *func,
                vec![
                    *ov_val,
                    after_flag,
                    Value::fixnum(lisp_beg),
                    Value::fixnum(lisp_end),
                    Value::fixnum(lisp_old_len),
                ],
            )?;
        }
        Ok(())
    })();
    ctx.restore_specpdl_roots(roots);
    apply_result
}

/// Iterate over a Lisp list, yielding each car.
fn value_list_iter(list: Value) -> Vec<Value> {
    let mut result = Vec::new();
    let mut cursor = list;
    while cursor.is_cons() {
        let pair_car = cursor.cons_car();
        let pair_cdr = cursor.cons_cdr();
        result.push(pair_car);
        cursor = pair_cdr;
    }
    // If it's a single non-nil, non-cons value, treat it as a single-element list.
    if result.is_empty() && !list.is_nil() && !list.is_cons() {
        result.push(list);
    }
    result
}

fn expect_integer_or_marker_in_buffers(
    buffers: &BufferManager,
    value: &Value,
) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        _ if value.is_marker() => {
            super::marker::marker_position_as_int_with_buffers(buffers, value)
        }
        _other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *value],
        )),
    }
}

fn current_buffer_accessible_char_region_in_buffers(
    buffers: &BufferManager,
    start_arg: &Value,
    end_arg: &Value,
) -> Result<Option<(usize, usize)>, Flow> {
    let Some(buf) = buffers.current_buffer() else {
        return Ok(None);
    };

    let start = expect_integer_or_marker_in_buffers(buffers, start_arg)?;
    let end = expect_integer_or_marker_in_buffers(buffers, end_arg)?;
    let point_min = buf.point_min_char() as i64 + 1;
    let point_max = buf.point_max_char() as i64 + 1;
    if start < point_min || start > point_max || end < point_min || end > point_max {
        return Err(signal(
            "args-out-of-range",
            vec![Value::make_buffer(buf.id), *start_arg, *end_arg],
        ));
    }

    let (from, to) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    Ok(Some((
        buf.lisp_pos_to_accessible_byte(from),
        buf.lisp_pos_to_accessible_byte(to),
    )))
}

// ---------------------------------------------------------------------------
// Eval-dependent builtins (need &mut Context for buffer access)
// ---------------------------------------------------------------------------

/// Collect the insertable text from a mixed list of strings and characters.
///
/// Returns raw Emacs-internal-encoding bytes. String args contribute their
/// `LispString.as_bytes()` directly (promoted via overlong C0/C1 for
/// unibyte 0x80..0xFF bytes). Character args are encoded via
/// `emacs_char::char_string`. The caller is responsible for wrapping the
/// result into a `LispString` before handing it to buffer insertion.
pub(crate) fn collect_insert_text(_name: &str, args: &[Value]) -> Result<Vec<u8>, Flow> {
    use crate::emacs_core::emacs_char;
    let mut bytes: Vec<u8> = Vec::new();
    for arg in args {
        match arg.kind() {
            ValueKind::String => {
                let ls = arg.as_lisp_string().ok_or_else(|| {
                    signal("wrong-type-argument", vec![Value::symbol("stringp"), *arg])
                })?;
                if ls.is_multibyte() {
                    bytes.extend_from_slice(ls.as_bytes());
                } else {
                    // Unibyte string: each byte is a raw byte value. Promote
                    // 0x80..0xFF to overlong C0/C1 Emacs encoding so the
                    // concatenated result is a well-formed multibyte byte
                    // stream.
                    for &b in ls.as_bytes() {
                        if b < 0x80 {
                            bytes.push(b);
                        } else {
                            bytes.push(0xC0 | ((b >> 6) & 0x01));
                            bytes.push(0x80 | (b & 0x3F));
                        }
                    }
                }
                continue;
            }
            ValueKind::Fixnum(_) => {
                let code = super::builtins::expect_character_code(arg)? as u32;
                if code > emacs_char::MAX_CHAR {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("characterp"), *arg],
                    ));
                }
                let mut buf = [0u8; emacs_char::MAX_MULTIBYTE_LENGTH];
                let len = emacs_char::char_string(code, &mut buf);
                bytes.extend_from_slice(&buf[..len]);
                continue;
            }
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("char-or-string-p"), *arg],
                ));
            }
        }
    }
    Ok(bytes)
}

/// `(insert-before-markers &rest ARGS)` — insert at point, advancing ALL
/// markers at that position past the inserted text (regardless of their
/// InsertionType).
pub(crate) fn builtin_insert_before_markers(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let bytes = collect_insert_text("insert-before-markers", &args)?;
    if bytes.is_empty() {
        return Ok(Value::NIL);
    }
    ensure_current_buffer_writable_in_state(&ctx.obarray, &[], &ctx.buffers)?;
    if let Some(id) = ctx.buffers.current_buffer_id() {
        let insert_pos = ctx.buffers.get(id).map(|buf| buf.pt_byte).unwrap_or(0);
        let byte_len = bytes.len();
        let ls = crate::heap_types::LispString::from_emacs_bytes(bytes);
        signal_before_change(ctx, insert_pos, insert_pos)?;
        let _ = ctx
            .buffers
            .insert_lisp_string_into_buffer_before_markers(id, &ls);
        signal_after_change(ctx, insert_pos, insert_pos + byte_len, 0)?;
    }
    Ok(Value::NIL)
}

/// `(delete-char N &optional KILLFLAG)` — delete N characters forward.
pub(crate) fn builtin_delete_char(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("delete-char", &args, 1)?;
    expect_max_args("delete-char", &args, 2)?;
    let n = expect_integer("delete-char", &args[0])?;
    ensure_current_buffer_writable_in_state(&ctx.obarray, &[], &ctx.buffers)?;
    if n.unsigned_abs() < 2 {
        ctx.apply(Value::symbol("undo-auto-amalgamate"), vec![])?;
    }
    if let Some(current_id) = ctx.buffers.current_buffer_id() {
        let Some((start, end)) = ({
            let Some(buf) = ctx.buffers.get(current_id) else {
                return Ok(Value::NIL);
            };
            let pt = buf.pt_byte;
            if n > 0 {
                // Delete N characters forward from point.
                let mut end = pt;
                for _ in 0..n {
                    if end >= buf.zv_byte {
                        return Err(signal("end-of-buffer", vec![]));
                    }
                    match buf.char_after_storage_len(end) {
                        Some(char_len) => end += char_len,
                        None => {
                            return Err(signal("end-of-buffer", vec![]));
                        }
                    }
                }
                Some((pt, end))
            } else if n < 0 {
                // Delete |N| characters backward from point.
                let mut start = pt;
                for _ in 0..(-n) {
                    if start <= buf.begv_byte {
                        return Err(signal("beginning-of-buffer", vec![]));
                    }
                    match buf.char_before_storage_len(start) {
                        Some(char_len) => start -= char_len,
                        None => {
                            return Err(signal("beginning-of-buffer", vec![]));
                        }
                    }
                }
                Some((start, pt))
            } else {
                None
            }
        }) else {
            return Ok(Value::NIL);
        };
        let old_len = current_buffer_byte_span_char_len(ctx, start, end);
        signal_before_change(ctx, start, end)?;
        let _ = ctx.buffers.delete_buffer_region(current_id, start, end);
        signal_after_change(ctx, start, start, old_len)?;
    }
    Ok(Value::NIL)
}

/// `(delete-region START END)` — delete text in the accessible current buffer.
pub(crate) fn builtin_delete_region(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("delete-region", &args, 2)?;
    let Some((start_byte, end_byte)) =
        current_buffer_accessible_char_region_in_buffers(&ctx.buffers, &args[0], &args[1])?
    else {
        return Ok(Value::NIL);
    };
    if start_byte == end_byte {
        return Ok(Value::NIL);
    }

    let Some(current_id) = ctx.buffers.current_buffer_id() else {
        return Ok(Value::NIL);
    };
    let read_only = ctx
        .buffers
        .get(current_id)
        .is_some_and(|buf| buffer_read_only_active_in_state(&ctx.obarray, &[], buf));
    if read_only {
        return Err(signal(
            "buffer-read-only",
            vec![Value::make_buffer(current_id)],
        ));
    }

    let old_len = current_buffer_byte_span_char_len(ctx, start_byte, end_byte);
    signal_before_change(ctx, start_byte, end_byte)?;
    let _ = ctx
        .buffers
        .delete_buffer_region(current_id, start_byte, end_byte);
    signal_after_change(ctx, start_byte, start_byte, old_len)?;
    Ok(Value::NIL)
}

/// `(delete-and-extract-region START END)` — delete text and return it.
pub(crate) fn builtin_delete_and_extract_region(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("delete-and-extract-region", &args, 2)?;
    let Some((start_byte, end_byte)) =
        current_buffer_accessible_char_region_in_buffers(&ctx.buffers, &args[0], &args[1])?
    else {
        return Ok(Value::string(""));
    };
    if start_byte == end_byte {
        return Ok(Value::string(""));
    }

    let Some(current_id) = ctx.buffers.current_buffer_id() else {
        return Ok(Value::string(""));
    };
    let deleted = {
        let Some(buf) = ctx.buffers.get(current_id) else {
            return Ok(Value::string(""));
        };
        if buffer_read_only_active_in_state(&ctx.obarray, &[], buf) {
            return Err(signal(
                "buffer-read-only",
                vec![Value::make_buffer(current_id)],
            ));
        }
        buf.buffer_substring_value(start_byte, end_byte)
    };

    let old_len = current_buffer_byte_span_char_len(ctx, start_byte, end_byte);
    signal_before_change(ctx, start_byte, end_byte)?;
    let _ = ctx
        .buffers
        .delete_buffer_region(current_id, start_byte, end_byte);
    signal_after_change(ctx, start_byte, start_byte, old_len)?;
    Ok(deleted)
}

/// `(erase-buffer)` — delete all text and remove any narrowing restriction.
pub(crate) fn builtin_erase_buffer(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("erase-buffer", &args, 0)?;
    let Some(current_id) = ctx.buffers.current_buffer_id() else {
        return Ok(Value::NIL);
    };
    let buf_len = ctx
        .buffers
        .get(current_id)
        .map(|buf| buf.text.len())
        .unwrap_or(0);
    let old_len = current_buffer_byte_span_char_len(ctx, 0, buf_len);
    if buf_len > 0 {
        signal_before_change(ctx, 0, buf_len)?;
    }
    erase_buffer_impl(&ctx.obarray, &[], &mut ctx.buffers, vec![])?;
    if buf_len > 0 {
        signal_after_change(ctx, 0, 0, old_len)?;
    }
    Ok(Value::NIL)
}

pub(crate) fn erase_buffer_impl(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("erase-buffer", &args, 0)?;
    let Some(current_id) = buffers.current_buffer_id() else {
        return Ok(Value::NIL);
    };

    let should_signal_read_only = buffers.get(current_id).is_some_and(|buf| {
        !buf.text.is_empty() && buffer_read_only_active_in_state(obarray, dynamic, buf)
    });
    if should_signal_read_only {
        return Err(signal(
            "buffer-read-only",
            vec![Value::make_buffer(current_id)],
        ));
    }

    let _ = buffers.clear_buffer_labeled_restrictions(current_id);
    let len = {
        let Some(buf) = buffers.get_mut(current_id) else {
            return Ok(Value::NIL);
        };
        buf.widen();
        buf.text.len()
    };
    if len > 0 {
        let _ = buffers.delete_buffer_region(current_id, 0, len);
    }
    if let Some(buf) = buffers.get_mut(current_id) {
        buf.goto_byte(0);
    }
    Ok(Value::NIL)
}

/// `(buffer-substring-no-properties START END)` — same as buffer-substring
/// (text properties not yet implemented at the Lisp value level).
pub(crate) fn builtin_buffer_substring_no_properties(
    ctx: &crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("buffer-substring-no-properties", &args, 2)?;
    let Some((start_byte, end_byte)) =
        current_buffer_accessible_char_region_in_buffers(&ctx.buffers, &args[0], &args[1])?
    else {
        return Ok(Value::heap_string(
            crate::heap_types::LispString::from_emacs_bytes(Vec::new()),
        ));
    };
    let Some(buf) = ctx.buffers.current_buffer() else {
        return Ok(Value::heap_string(
            crate::heap_types::LispString::from_emacs_bytes(Vec::new()),
        ));
    };
    let mut bytes = Vec::new();
    buf.copy_emacs_bytes_to(start_byte, end_byte, &mut bytes);
    Ok(Value::heap_string(
        crate::emacs_core::builtins::lisp_string_from_buffer_bytes(bytes, buf.get_multibyte()),
    ))
}

/// `(following-char)` — return character after point (0 if at end).
pub(crate) fn builtin_following_char(
    ctx: &crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("following-char", &args, 0)?;
    following_char_value(ctx)
}

pub(crate) fn builtin_following_char_0(ctx: &mut crate::emacs_core::eval::Context) -> EvalResult {
    following_char_value(ctx)
}

fn following_char_value(ctx: &crate::emacs_core::eval::Context) -> EvalResult {
    match ctx.buffers.current_buffer() {
        Some(buf) => match (buf.pt_byte < buf.zv_byte)
            .then(|| buf.char_code_after(buf.pt_byte))
            .flatten()
        {
            Some(code) => Ok(Value::fixnum(code as i64)),
            None => Ok(Value::fixnum(0)),
        },
        None => Ok(Value::fixnum(0)),
    }
}

/// `(preceding-char)` — return character before point (0 if at beginning).
pub(crate) fn builtin_preceding_char(
    ctx: &crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("preceding-char", &args, 0)?;
    match ctx.buffers.current_buffer() {
        Some(buf) => match (buf.pt_byte > buf.begv_byte)
            .then(|| buf.char_code_before(buf.pt_byte))
            .flatten()
        {
            Some(code) => Ok(Value::fixnum(code as i64)),
            None => Ok(Value::fixnum(0)),
        },
        None => Ok(Value::fixnum(0)),
    }
}

// ---------------------------------------------------------------------------
// Pure builtins (no evaluator needed)
// ---------------------------------------------------------------------------

/// `(user-uid)` — return effective user ID.
/// Uses the `id -u` command on Unix; falls back to 1000.
pub(crate) fn builtin_user_uid(args: Vec<Value>) -> EvalResult {
    expect_args("user-uid", &args, 0)?;
    Ok(Value::fixnum(get_uid()))
}

/// `(file-user-uid)` — return the UID used for file ownership.
pub(crate) fn builtin_file_user_uid(args: Vec<Value>) -> EvalResult {
    expect_args("file-user-uid", &args, 0)?;
    Ok(Value::fixnum(get_uid()))
}

/// `(user-real-uid)` — return real user ID.
pub(crate) fn builtin_user_real_uid(args: Vec<Value>) -> EvalResult {
    expect_args("user-real-uid", &args, 0)?;
    Ok(Value::fixnum(get_uid()))
}

/// `(group-gid)` — return the effective group ID.
pub(crate) fn builtin_group_gid(args: Vec<Value>) -> EvalResult {
    expect_args("group-gid", &args, 0)?;
    Ok(Value::fixnum(get_gid()))
}

/// `(file-group-gid)` — return the GID used for file ownership.
pub(crate) fn builtin_file_group_gid(args: Vec<Value>) -> EvalResult {
    expect_args("file-group-gid", &args, 0)?;
    Ok(Value::fixnum(get_gid()))
}

/// `(group-real-gid)` — return the real group ID.
pub(crate) fn builtin_group_real_gid(args: Vec<Value>) -> EvalResult {
    expect_args("group-real-gid", &args, 0)?;
    Ok(Value::fixnum(get_gid()))
}

/// `(group-name GID)` — return the group name for numeric GID.
pub(crate) fn builtin_group_name(args: Vec<Value>) -> EvalResult {
    expect_args("group-name", &args, 1)?;
    let gid = match args[0].kind() {
        ValueKind::Fixnum(n) => n,
        _ => {
            return Err(signal(
                "error",
                vec![Value::string("Invalid GID specification")],
            ));
        }
    };
    if gid < 0 || gid > u32::MAX as i64 {
        return Err(signal(
            "error",
            vec![Value::string("Invalid GID specification")],
        ));
    }
    let Some(name) = lookup_group_name(gid as u32) else {
        return Err(signal(
            "error",
            vec![Value::string("Invalid GID specification")],
        ));
    };
    Ok(Value::string(name))
}

/// `(load-average &optional USE-FLOATS)` — return load averages.
///
/// With USE-FLOATS non-nil, returns 3 floats.
/// With USE-FLOATS nil/omitted, returns 3 integers scaled by 100.
pub(crate) fn builtin_load_average(args: Vec<Value>) -> EvalResult {
    expect_max_args("load-average", &args, 1)?;
    let use_floats = args.first().is_some_and(|value| value.is_truthy());
    let loads = read_load_average().unwrap_or([0.0, 0.0, 0.0]);
    if use_floats {
        Ok(Value::list(vec![
            Value::make_float(loads[0]),
            Value::make_float(loads[1]),
            Value::make_float(loads[2]),
        ]))
    } else {
        Ok(Value::list(vec![
            Value::fixnum((loads[0] * 100.0) as i64),
            Value::fixnum((loads[1] * 100.0) as i64),
            Value::fixnum((loads[2] * 100.0) as i64),
        ]))
    }
}

/// `(logcount INTEGER)` — return the number of 1 bits for nonnegative integers,
/// or the number of 0 bits in two's-complement form for negative integers.
pub(crate) fn builtin_logcount(args: Vec<Value>) -> EvalResult {
    expect_args("logcount", &args, 1)?;
    let n = match args[0].kind() {
        ValueKind::Fixnum(v) => v,
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("integerp"), args[0]],
            ));
        }
    };
    let bits = if n >= 0 {
        (n as u64).count_ones() as i64
    } else {
        ((!n) as u64).count_ones() as i64
    };
    Ok(Value::fixnum(bits))
}

// ---------------------------------------------------------------------------
// OS helpers (avoid libc dependency)
// ---------------------------------------------------------------------------

/// Retrieve the effective UID via `id -u`, falling back to 1000.
fn get_uid() -> i64 {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(1000)
}

/// Retrieve the effective GID via `id -g`, falling back to 1000.
fn get_gid() -> i64 {
    std::process::Command::new("id")
        .arg("-g")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(1000)
}

#[cfg(unix)]
fn lookup_group_name(gid: u32) -> Option<String> {
    let group = unsafe { libc::getgrgid(gid as libc::gid_t) };
    if group.is_null() {
        return None;
    }
    let name_ptr = unsafe { (*group).gr_name };
    if name_ptr.is_null() {
        return None;
    }
    Some(
        unsafe { CStr::from_ptr(name_ptr) }
            .to_string_lossy()
            .into_owned(),
    )
}

#[cfg(not(unix))]
fn lookup_group_name(_gid: u32) -> Option<String> {
    None
}

#[cfg(unix)]
fn read_load_average() -> Option<[f64; 3]> {
    let mut values = [0.0f64; 3];
    let result = unsafe { libc::getloadavg(values.as_mut_ptr(), 3) };
    if result == 3 { Some(values) } else { None }
}

#[cfg(not(unix))]
fn read_load_average() -> Option<[f64; 3]> {
    None
}
// ---------------------------------------------------------------------------
// translate-region-internal (mirrors GNU editfns.c:2506)
// ---------------------------------------------------------------------------

/// `(translate-region-internal START END TABLE)`
///
/// Translate every character between START and END through TABLE.
/// TABLE may be a string (Nth char in TABLE is the mapping for char N) or
/// a char-table whose `purpose` is `translation-table`.
///
/// Returns the number of characters changed.
///
/// Mirrors GNU `Ftranslate_region_internal` (editfns.c:2506) using a
/// whole-region read/translate/replace strategy (rather than GNU's
/// in-place gap mutation). The behaviour for simple char→char and
/// char→string/vector mappings matches GNU. The multi-character
/// `(([FROM-CHAR ...] . TO) ...)` form is currently treated as identity
/// (no lookahead) — this is a known pragmatic deviation, marked TODO.
pub(crate) fn builtin_translate_region_internal(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    use super::chartable::{ct_lookup, is_char_table};
    use super::emacs_char::{
        MAX_CHAR, MAX_MULTIBYTE_LENGTH, byte8_to_char, char_string, chars_in_multibyte,
        string_char_advance,
    };

    expect_args("translate-region-internal", &args, 3)?;
    let table = &args[2];

    // ----- Validate TABLE ----------------------------------------------------
    let table_str = table.as_lisp_string();
    let is_str_table = table_str.is_some();
    let is_ct_table = is_char_table(table);
    if !is_str_table && !is_ct_table {
        return Err(signal(
            "error",
            vec![Value::string("Not a translation table")],
        ));
    }
    if is_ct_table {
        let vec = table.as_vector_data().unwrap();
        // CT_SUBTYPE slot is the `purpose' symbol (chartab.c:528).
        let purpose = vec[3];
        let translation_sym = Value::symbol("translation-table");
        if !super::value::eq_value(&purpose, &translation_sym) {
            return Err(signal(
                "error",
                vec![Value::string("Not a translation table")],
            ));
        }
    }

    // ----- Resolve region in the current buffer ------------------------------
    let (buffer_id, start_byte, end_byte) =
        super::fns::normalize_current_buffer_region_bounds_in_manager(
            &eval.buffers,
            &args[0],
            &args[1],
        )?;
    if start_byte == end_byte {
        return Ok(Value::fixnum(0));
    }
    let multibyte = eval
        .buffers
        .get(buffer_id)
        .map(|b| b.get_multibyte())
        .unwrap_or(true);

    // Read the whole region up front (whole-region replace strategy).
    let source = super::fns::read_buffer_region_bytes_in_manager(
        &eval.buffers,
        buffer_id,
        start_byte,
        end_byte,
    )?;

    // ----- String-table prep -------------------------------------------------
    let table_string_info: Option<(Vec<u8>, bool)> = table_str.map(|s| {
        let mut bytes = s.as_bytes().to_vec();
        let mut mb = s.is_multibyte();
        // GNU: if buffer is unibyte but table is multibyte, convert table to
        // unibyte (string_make_unibyte). Our mapping below indexes by byte
        // for unibyte tables; flatten by taking the byte view, which already
        // happens for unibyte-only tables. For a multibyte table on a unibyte
        // buffer we set mb=false and let the byte index lookup take over.
        if !multibyte && mb {
            mb = false;
        }
        // In the unibyte-buffer × multibyte-table case, leave bytes alone:
        // the unibyte-source path indexes by byte so it stays consistent.
        let _ = &mut bytes;
        (bytes, mb)
    });
    let translatable_chars: i64 = if let Some((bytes, _)) = table_string_info.as_ref() {
        std::cmp::min(MAX_CHAR as i64 + 1, bytes.len() as i64)
    } else {
        MAX_CHAR as i64 + 1
    };

    // ----- Walk the region, build the translated bytes -----------------------
    let mut out: Vec<u8> = Vec::with_capacity(source.len());
    let mut characters_changed: i64 = 0;
    let mut p: usize = 0;
    while p < source.len() {
        let (oc, len) = if multibyte {
            let mut q = p;
            let c = string_char_advance(&source, &mut q);
            (c as i64, q - p)
        } else {
            (source[p] as i64, 1)
        };

        // Default: no translation.
        let mut nc: i64 = oc;
        let mut new_bytes: Option<Vec<u8>> = None;

        if oc < translatable_chars {
            if let Some((tt, table_mb)) = table_string_info.as_ref() {
                if *table_mb {
                    // Find char index `oc` within the multibyte table bytes.
                    let mut bp = 0usize;
                    let mut idx: i64 = 0;
                    while idx < oc && bp < tt.len() {
                        let (_c, l) = super::emacs_char::string_char(&tt[bp..]);
                        bp += l.max(1);
                        idx += 1;
                    }
                    if bp < tt.len() {
                        let mut qq = bp;
                        let c = string_char_advance(tt, &mut qq);
                        nc = c as i64;
                        new_bytes = Some(tt[bp..qq].to_vec());
                    }
                } else if (oc as usize) < tt.len() {
                    let b = tt[oc as usize];
                    nc = b as i64;
                    if b >= 0x80 && multibyte {
                        // BYTE8_STRING: encode raw byte as a 2-byte multibyte.
                        let mut buf = [0u8; MAX_MULTIBYTE_LENGTH];
                        let n = char_string(byte8_to_char(b), &mut buf);
                        new_bytes = Some(buf[..n].to_vec());
                    } else {
                        new_bytes = Some(vec![b]);
                    }
                }
            } else {
                // char-table case.
                let val = ct_lookup(table, oc)?;
                if let Some(c) = val.as_fixnum() {
                    if (0..=MAX_CHAR as i64).contains(&c) {
                        nc = c;
                        let mut buf = [0u8; MAX_MULTIBYTE_LENGTH];
                        let n = char_string(c as u32, &mut buf);
                        new_bytes = Some(buf[..n].to_vec());
                    }
                } else if val.is_vector() {
                    // [TO_CHAR ...] — concatenate the chars.
                    nc = -1;
                    if let Some(items) = val.as_vector_data() {
                        let mut bytes = Vec::new();
                        for ch in items.iter() {
                            if let Some(c) = ch.as_fixnum() {
                                if (0..=MAX_CHAR as i64).contains(&c) {
                                    let mut buf = [0u8; MAX_MULTIBYTE_LENGTH];
                                    let n = char_string(c as u32, &mut buf);
                                    bytes.extend_from_slice(&buf[..n]);
                                }
                            }
                        }
                        new_bytes = Some(bytes);
                    }
                } else if val.is_cons() {
                    // (([FROM-CHAR ...] . TO) ...) — multi-char source
                    // pattern. Lookahead-based matching not yet implemented;
                    // skip translation (identity).
                    // TODO: port `check_translation` (editfns.c:2448).
                    nc = oc;
                    new_bytes = None;
                }
            }
        }

        if nc != oc && nc >= 0 {
            // Single-char-to-something replacement.
            if let Some(b) = new_bytes {
                out.extend_from_slice(&b);
            } else {
                out.extend_from_slice(&source[p..p + len]);
            }
            characters_changed += 1;
        } else if nc < 0 {
            // Vector form: char(s) → multiple chars.
            if let Some(b) = new_bytes {
                let added = if multibyte {
                    chars_in_multibyte(&b) as i64
                } else {
                    b.len() as i64
                };
                out.extend_from_slice(&b);
                characters_changed += added;
            } else {
                out.extend_from_slice(&source[p..p + len]);
            }
        } else {
            // Identity.
            out.extend_from_slice(&source[p..p + len]);
        }
        p += len.max(1);
    }

    // ----- Write back if anything changed ------------------------------------
    if characters_changed > 0 {
        let replacement = if multibyte {
            crate::heap_types::LispString::from_emacs_bytes(out)
        } else {
            crate::heap_types::LispString::from_unibyte(out)
        };
        super::fns::replace_buffer_region_lisp_string(
            eval,
            buffer_id,
            start_byte,
            end_byte,
            &replacement,
        )?;
    }

    Ok(Value::fixnum(characters_changed))
}

#[cfg(test)]
#[path = "editfns_test.rs"]
mod tests;
