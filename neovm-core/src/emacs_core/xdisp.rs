//! Display engine builtins for the Elisp interpreter.
//!
//! Implements display-related functions from Emacs `xdisp.c`:
//! - `format-mode-line` — format a mode line string
//! - `invisible-p` — check if a position or property is invisible
//! - `line-pixel-height` — get line height in pixels
//! - `window-text-pixel-size` — calculate text pixel dimensions
//! - `pos-visible-in-window-p` — check if position is visible
//! - `move-point-visually` — move point in visual order
//! - `lookup-image-map` — lookup image map coordinates
//! - `current-bidi-paragraph-direction` — get bidi paragraph direction
//! - `move-to-window-line` — move to a specific window line
//! - `tool-bar-height` — get tool bar height
//! - `tab-bar-height` — get tab bar height
//! - `line-number-display-width` — get line number display width
//! - `long-line-optimizations-p` — check if long-line optimizations are enabled

use super::chartable::{make_char_table_value, make_char_table_with_extra_slots};
use super::error::{EvalResult, Flow, signal};
use super::intern::intern;
use super::value::*;
use crate::buffer::BufferId;
use crate::window::{FrameId, Window, WindowId};

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

fn expect_args_range(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), Flow> {
    if args.len() < min || args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_integer_or_marker(arg: &Value) -> Result<(), Flow> {
    match arg {
        Value::Int(_) | Value::Char(_) => Ok(()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *other],
        )),
    }
}

fn expect_fixnum_arg(name: &str, arg: &Value) -> Result<(), Flow> {
    match arg {
        Value::Int(_) | Value::Char(_) => Ok(()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol(name), *other],
        )),
    }
}

// ---------------------------------------------------------------------------
// Pure builtins
// ---------------------------------------------------------------------------

/// (format-mode-line &optional FORMAT FACE WINDOW BUFFER) -> string
///
/// Batch-compatible behavior: accepts 1..4 args and returns an empty string.
pub(crate) fn builtin_format_mode_line(args: Vec<Value>) -> EvalResult {
    expect_args_range("format-mode-line", &args, 1, 4)?;
    if let Some(window) = args.get(2) {
        if !window.is_nil() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("windowp"), *window],
            ));
        }
    }
    if let Some(buffer) = args.get(3) {
        if !buffer.is_nil() && !matches!(buffer, Value::Buffer(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("bufferp"), *buffer],
            ));
        }
    }
    Ok(Value::string(""))
}

/// `(format-mode-line &optional FORMAT FACE WINDOW BUFFER)` evaluator-backed variant.
///
/// Handles string formats with %-construct expansion and list-based format
/// specs by recursively processing elements (symbols, strings, :eval, :propertize,
/// and conditional cons cells).
pub(crate) fn builtin_format_mode_line_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    frames: &crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> Result<Option<Value>, Flow> {
    expect_args_range("format-mode-line", &args, 1, 4)?;
    validate_optional_window_designator_in_state(frames, args.get(2), "windowp")?;
    validate_optional_buffer_designator_in_state(buffers, args.get(3))?;

    let target_buffer = resolve_mode_line_buffer_in_state(frames, args.get(2), args.get(3));
    let saved_buffer = buffers.current_buffer_id();
    if let Some(buffer_id) = target_buffer {
        buffers.set_current(buffer_id);
    }

    if args[0].is_nil() {
        if let Some(buffer_id) = saved_buffer {
            buffers.set_current(buffer_id);
        }
        return Ok(Some(Value::string("")));
    }

    let format_val = args[0];
    let mut result = String::new();
    let needs_eval = format_mode_line_recursive_in_state(
        obarray,
        dynamic,
        &*buffers,
        &format_val,
        &mut result,
        0,
    );

    if let Some(buffer_id) = saved_buffer {
        buffers.set_current(buffer_id);
    }

    if needs_eval {
        Ok(None)
    } else {
        Ok(Some(Value::string(&result)))
    }
}

pub(crate) fn builtin_format_mode_line_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    finish_format_mode_line_in_eval(eval, &args)
}

pub(crate) fn finish_format_mode_line_in_eval(
    eval: &mut super::eval::Evaluator,
    args: &[Value],
) -> EvalResult {
    expect_args_range("format-mode-line", args, 1, 4)?;
    validate_optional_window_designator(eval, args.get(2), "windowp")?;
    validate_optional_buffer_designator(eval, args.get(3))?;

    let target_buffer = resolve_mode_line_buffer(eval, args.get(2), args.get(3));
    let saved_buffer = eval.buffers.current_buffer_id();
    if let Some(buffer_id) = target_buffer {
        eval.buffers.set_current(buffer_id);
    }

    let result = if args[0].is_nil() {
        Value::string("")
    } else {
        let format_val = args[0];
        let mut result = String::new();
        format_mode_line_recursive(eval, &format_val, &mut result, 0);
        Value::string(&result)
    };

    if let Some(buffer_id) = saved_buffer {
        eval.buffers.set_current(buffer_id);
    }
    Ok(result)
}

pub(crate) fn finish_format_mode_line_in_state_with_eval(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    frames: &crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    args: &[Value],
    mut eval_form: impl FnMut(&Value, &crate::buffer::BufferManager) -> Result<Value, Flow>,
) -> EvalResult {
    expect_args_range("format-mode-line", args, 1, 4)?;
    validate_optional_window_designator_in_state(frames, args.get(2), "windowp")?;
    validate_optional_buffer_designator_in_state(buffers, args.get(3))?;

    let target_buffer = resolve_mode_line_buffer_in_state(frames, args.get(2), args.get(3));
    let saved_buffer = buffers.current_buffer_id();
    if let Some(buffer_id) = target_buffer {
        buffers.set_current(buffer_id);
    }

    let result = if args[0].is_nil() {
        Value::string("")
    } else {
        let format_val = args[0];
        let mut result = String::new();
        format_mode_line_recursive_in_state_with_eval(
            obarray,
            dynamic,
            &*buffers,
            &format_val,
            &mut result,
            0,
            &mut eval_form,
        )?;
        Value::string(&result)
    };

    if let Some(buffer_id) = saved_buffer {
        buffers.set_current(buffer_id);
    }
    Ok(result)
}

pub(crate) fn builtin_format_mode_line_in_vm_runtime(
    shared: &mut crate::emacs_core::eval::VmSharedState<'_>,
    vm_gc_roots: &[Value],
    args: &[Value],
) -> EvalResult {
    expect_args_range("format-mode-line", args, 1, 4)?;
    validate_optional_window_designator_in_state(&*shared.frames, args.get(2), "windowp")?;
    validate_optional_buffer_designator_in_state(&*shared.buffers, args.get(3))?;

    let target_buffer =
        resolve_mode_line_buffer_in_state(&*shared.frames, args.get(2), args.get(3));
    let saved_buffer = shared.buffers.current_buffer_id();
    if let Some(buffer_id) = target_buffer {
        shared.buffers.set_current(buffer_id);
    }

    let result = if args[0].is_nil() {
        Value::string("")
    } else {
        let format_val = args[0];
        let mut result = String::new();
        format_mode_line_recursive_in_vm_runtime(
            shared,
            vm_gc_roots,
            args,
            &format_val,
            &mut result,
            0,
        )?;
        Value::string(&result)
    };

    if let Some(buffer_id) = saved_buffer {
        shared.buffers.set_current(buffer_id);
    }
    Ok(result)
}

fn mode_line_symbol_value_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    name: &str,
) -> Option<Value> {
    let name_id = intern(name);
    for frame in dynamic.iter().rev() {
        if let Some(value) = frame.get(&name_id) {
            return Some(*value);
        }
    }

    if let Some(buf) = buffers.current_buffer()
        && let Some(value) = buf.get_buffer_local(name)
    {
        return Some(*value);
    }

    obarray.symbol_value(name).copied()
}

fn mode_line_conditional_branch(cdr: Value, branch_is_then: bool) -> Option<Value> {
    if !cdr.is_cons() {
        return None;
    }
    if branch_is_then {
        return Some(cdr.cons_car());
    }
    let else_tail = cdr.cons_cdr();
    if else_tail.is_cons() {
        Some(else_tail.cons_car())
    } else {
        None
    }
}

fn append_mode_line_rendered_segment(
    result: &mut String,
    rendered: &str,
    field_width: i64,
    precision: i64,
) {
    let mut segment = if precision > 0 {
        rendered
            .chars()
            .take(precision as usize)
            .collect::<String>()
    } else {
        rendered.to_owned()
    };
    let rendered_len = segment.chars().count() as i64;
    if field_width > 0 && rendered_len < field_width {
        segment.extend(std::iter::repeat_n(
            ' ',
            (field_width - rendered_len) as usize,
        ));
    }
    result.push_str(&segment);
}

fn append_mode_line_string_in_state(
    buffers: &crate::buffer::BufferManager,
    result: &mut String,
    value: &str,
    literal: bool,
) {
    if literal {
        result.push_str(value);
    } else {
        expand_mode_line_percent_in_state(buffers, value, result);
    }
}

/// Recursively process a mode-line format spec, appending output to `result`.
///
/// FORMAT can be:
/// - A string: expand %-constructs (%b, %f, %*, %l, %c, %p, etc.)
/// - A symbol: look up its value, recursively format
/// - A list: process each element in sequence
/// - `(:eval FORM)`: evaluate FORM, use result as format
/// - `(:propertize ELT PROPS...)`: process ELT (ignore text properties)
/// - A cons `(SYMBOL . REST)`: if SYMBOL's value is non-nil, process REST
fn format_mode_line_recursive(
    eval: &mut super::eval::Evaluator,
    format: &Value,
    result: &mut String,
    depth: usize,
) {
    if depth > 20 {
        return; // Guard against infinite recursion
    }

    match format {
        Value::Nil => {}

        Value::Str(_) => {
            if let Some(fmt_str) = format.as_str() {
                append_mode_line_string_in_state(&eval.buffers, result, fmt_str, false);
            }
        }

        Value::Int(n) => {
            // Integer in mode-line-format: if positive, specifies minimum
            // field width; if negative, max width with truncation.
            // The actual padding/truncation is applied to subsequent elements
            // which we don't track here, so just ignore the width spec.
            let _ = n;
        }

        _ if format.is_symbol() => {
            if let Some(name) = format.as_symbol_name() {
                if name == "mode-line-front-space" || name == "mode-line-end-spaces" {
                    result.push(' ');
                    return;
                }
                if let Some(val) = mode_line_symbol_value_in_state(
                    &eval.obarray,
                    eval.dynamic.as_slice(),
                    &eval.buffers,
                    name,
                ) && !val.is_nil()
                {
                    if let Some(text) = val.as_str() {
                        append_mode_line_string_in_state(&eval.buffers, result, text, true);
                    } else {
                        format_mode_line_recursive(eval, &val, result, depth + 1);
                    }
                }
            }
        }

        _ if format.is_cons() => {
            let car = format.cons_car();
            let cdr = format.cons_cdr();

            // (:eval FORM)
            if car.is_symbol_named(":eval") {
                if cdr.is_cons() {
                    let form_val = cdr.cons_car();
                    if let Ok(val) = eval.eval_value(&form_val) {
                        format_mode_line_recursive(eval, &val, result, depth + 1);
                    }
                }
                return;
            }

            // (:propertize ELT PROPS...) — process ELT, ignore properties
            if car.is_symbol_named(":propertize") {
                if cdr.is_cons() {
                    let elt = cdr.cons_car();
                    format_mode_line_recursive(eval, &elt, result, depth + 1);
                }
                return;
            }

            if let Value::Int(lim) = car {
                let mut nested = String::new();
                format_mode_line_recursive(eval, &cdr, &mut nested, depth + 1);
                append_mode_line_rendered_segment(
                    result,
                    &nested,
                    if lim > 0 { lim } else { 0 },
                    if lim < 0 { -lim } else { 0 },
                );
                return;
            }

            // Check if car is a symbol — conditional semantics:
            // (SYMBOL . REST) where if SYMBOL's value is non-nil, process REST
            if car.is_symbol() && !car.is_symbol_named("t") {
                if let Some(sym_name) = car.as_symbol_name()
                    && mode_line_symbol_value_in_state(
                        &eval.obarray,
                        eval.dynamic.as_slice(),
                        &eval.buffers,
                        sym_name,
                    )
                    .is_some_and(|value| value.is_truthy())
                    && let Some(branch) = mode_line_conditional_branch(cdr, true)
                {
                    format_mode_line_recursive(eval, &branch, result, depth + 1);
                } else if let Some(branch) = mode_line_conditional_branch(cdr, false) {
                    format_mode_line_recursive(eval, &branch, result, depth + 1);
                }
                return;
            }

            // Regular list: process each element
            if let Some(elements) = list_to_vec(format) {
                for elem in &elements {
                    format_mode_line_recursive(eval, elem, result, depth + 1);
                }
            }
        }

        _ => {
            // Unknown format type — try to get a string representation
            if let Some(s) = format.as_str() {
                result.push_str(s);
            }
        }
    }
}

fn format_mode_line_recursive_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    format: &Value,
    result: &mut String,
    depth: usize,
) -> bool {
    if depth > 20 {
        return false;
    }

    match format {
        Value::Nil => {}

        Value::Str(_) => {
            if let Some(fmt_str) = format.as_str() {
                append_mode_line_string_in_state(buffers, result, fmt_str, false);
            }
        }

        Value::Int(_) => {}

        _ if format.is_symbol() => {
            if let Some(name) = format.as_symbol_name() {
                if name == "mode-line-front-space" || name == "mode-line-end-spaces" {
                    result.push(' ');
                    return false;
                }
                if let Some(val) = mode_line_symbol_value_in_state(obarray, dynamic, buffers, name)
                    && !val.is_nil()
                {
                    if let Some(text) = val.as_str() {
                        append_mode_line_string_in_state(buffers, result, text, true);
                    } else if format_mode_line_recursive_in_state(
                        obarray,
                        dynamic,
                        buffers,
                        &val,
                        result,
                        depth + 1,
                    ) {
                        return true;
                    }
                }
            }
        }

        _ if format.is_cons() => {
            let car = format.cons_car();
            let cdr = format.cons_cdr();

            if car.is_symbol_named(":eval") {
                return true;
            }

            if car.is_symbol_named(":propertize") {
                if cdr.is_cons() {
                    let elt = cdr.cons_car();
                    return format_mode_line_recursive_in_state(
                        obarray,
                        dynamic,
                        buffers,
                        &elt,
                        result,
                        depth + 1,
                    );
                }
                return false;
            }

            if let Value::Int(lim) = car {
                let mut nested = String::new();
                let needs_eval = format_mode_line_recursive_in_state(
                    obarray,
                    dynamic,
                    buffers,
                    &cdr,
                    &mut nested,
                    depth + 1,
                );
                append_mode_line_rendered_segment(
                    result,
                    &nested,
                    if lim > 0 { lim } else { 0 },
                    if lim < 0 { -lim } else { 0 },
                );
                return needs_eval;
            }

            if car.is_symbol() && !car.is_symbol_named("t") {
                let branch = if let Some(sym_name) = car.as_symbol_name()
                    && mode_line_symbol_value_in_state(obarray, dynamic, buffers, sym_name)
                        .is_some_and(|value| value.is_truthy())
                {
                    mode_line_conditional_branch(cdr, true)
                } else {
                    mode_line_conditional_branch(cdr, false)
                };
                if let Some(branch) = branch {
                    return format_mode_line_recursive_in_state(
                        obarray,
                        dynamic,
                        buffers,
                        &branch,
                        result,
                        depth + 1,
                    );
                }
                return false;
            }

            if let Some(elements) = list_to_vec(format) {
                for elem in &elements {
                    if format_mode_line_recursive_in_state(
                        obarray,
                        dynamic,
                        buffers,
                        elem,
                        result,
                        depth + 1,
                    ) {
                        return true;
                    }
                }
            }
        }

        _ => {
            if let Some(s) = format.as_str() {
                result.push_str(s);
            }
        }
    }

    false
}

fn format_mode_line_recursive_in_state_with_eval(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    format: &Value,
    result: &mut String,
    depth: usize,
    eval_form: &mut impl FnMut(&Value, &crate::buffer::BufferManager) -> Result<Value, Flow>,
) -> Result<(), Flow> {
    if depth > 20 {
        return Ok(());
    }

    match format {
        Value::Nil => {}

        Value::Str(_) => {
            if let Some(fmt_str) = format.as_str() {
                append_mode_line_string_in_state(buffers, result, fmt_str, false);
            }
        }

        Value::Int(_) => {}

        _ if format.is_symbol() => {
            if let Some(name) = format.as_symbol_name() {
                if name == "mode-line-front-space" || name == "mode-line-end-spaces" {
                    result.push(' ');
                    return Ok(());
                }
                if let Some(val) = mode_line_symbol_value_in_state(obarray, dynamic, buffers, name)
                    && !val.is_nil()
                {
                    if let Some(text) = val.as_str() {
                        append_mode_line_string_in_state(buffers, result, text, true);
                    } else {
                        format_mode_line_recursive_in_state_with_eval(
                            obarray,
                            dynamic,
                            buffers,
                            &val,
                            result,
                            depth + 1,
                            eval_form,
                        )?;
                    }
                }
            }
        }

        _ if format.is_cons() => {
            let car = format.cons_car();
            let cdr = format.cons_cdr();

            if car.is_symbol_named(":eval") {
                if cdr.is_cons() {
                    let form_val = cdr.cons_car();
                    let val = eval_form(&form_val, buffers)?;
                    format_mode_line_recursive_in_state_with_eval(
                        obarray,
                        dynamic,
                        buffers,
                        &val,
                        result,
                        depth + 1,
                        eval_form,
                    )?;
                }
                return Ok(());
            }

            if car.is_symbol_named(":propertize") {
                if cdr.is_cons() {
                    let elt = cdr.cons_car();
                    format_mode_line_recursive_in_state_with_eval(
                        obarray,
                        dynamic,
                        buffers,
                        &elt,
                        result,
                        depth + 1,
                        eval_form,
                    )?;
                }
                return Ok(());
            }

            if let Value::Int(lim) = car {
                let mut nested = String::new();
                format_mode_line_recursive_in_state_with_eval(
                    obarray,
                    dynamic,
                    buffers,
                    &cdr,
                    &mut nested,
                    depth + 1,
                    eval_form,
                )?;
                append_mode_line_rendered_segment(
                    result,
                    &nested,
                    if lim > 0 { lim } else { 0 },
                    if lim < 0 { -lim } else { 0 },
                );
                return Ok(());
            }

            if car.is_symbol() && !car.is_symbol_named("t") {
                let branch = if let Some(sym_name) = car.as_symbol_name()
                    && mode_line_symbol_value_in_state(obarray, dynamic, buffers, sym_name)
                        .is_some_and(|value| value.is_truthy())
                {
                    mode_line_conditional_branch(cdr, true)
                } else {
                    mode_line_conditional_branch(cdr, false)
                };
                if let Some(branch) = branch {
                    format_mode_line_recursive_in_state_with_eval(
                        obarray,
                        dynamic,
                        buffers,
                        &branch,
                        result,
                        depth + 1,
                        eval_form,
                    )?;
                }
                return Ok(());
            }

            if let Some(elements) = list_to_vec(format) {
                for elem in &elements {
                    format_mode_line_recursive_in_state_with_eval(
                        obarray,
                        dynamic,
                        buffers,
                        elem,
                        result,
                        depth + 1,
                        eval_form,
                    )?;
                }
            }
        }

        _ => {
            if let Some(s) = format.as_str() {
                result.push_str(s);
            }
        }
    }

    Ok(())
}

fn format_mode_line_recursive_in_vm_runtime(
    shared: &mut crate::emacs_core::eval::VmSharedState<'_>,
    vm_gc_roots: &[Value],
    args_roots: &[Value],
    format: &Value,
    result: &mut String,
    depth: usize,
) -> Result<(), Flow> {
    if depth > 20 {
        return Ok(());
    }

    match format {
        Value::Nil => {}

        Value::Str(_) => {
            if let Some(fmt_str) = format.as_str() {
                append_mode_line_string_in_state(&*shared.buffers, result, fmt_str, false);
            }
        }

        Value::Int(_) => {}

        _ if format.is_symbol() => {
            if let Some(name) = format.as_symbol_name() {
                if name == "mode-line-front-space" || name == "mode-line-end-spaces" {
                    result.push(' ');
                    return Ok(());
                }
                let value = {
                    let obarray = &*shared.obarray;
                    let dynamic = shared.dynamic.as_slice();
                    let buffers = &*shared.buffers;
                    mode_line_symbol_value_in_state(obarray, dynamic, buffers, name)
                };
                if let Some(val) = value
                    && !val.is_nil()
                {
                    if let Some(text) = val.as_str() {
                        append_mode_line_string_in_state(&*shared.buffers, result, text, true);
                    } else {
                        format_mode_line_recursive_in_vm_runtime(
                            shared,
                            vm_gc_roots,
                            args_roots,
                            &val,
                            result,
                            depth + 1,
                        )?;
                    }
                }
            }
        }

        _ if format.is_cons() => {
            let car = format.cons_car();
            let cdr = format.cons_cdr();

            if car.is_symbol_named(":eval") {
                if cdr.is_cons() {
                    let form_val = cdr.cons_car();
                    let mut extra_roots = args_roots.to_vec();
                    extra_roots.push(form_val);
                    let val = shared.with_parent_evaluator_vm_roots(
                        vm_gc_roots,
                        &extra_roots,
                        move |eval| eval.eval_value(&form_val),
                    )?;
                    format_mode_line_recursive_in_vm_runtime(
                        shared,
                        vm_gc_roots,
                        args_roots,
                        &val,
                        result,
                        depth + 1,
                    )?;
                }
                return Ok(());
            }

            if car.is_symbol_named(":propertize") {
                if cdr.is_cons() {
                    let elt = cdr.cons_car();
                    format_mode_line_recursive_in_vm_runtime(
                        shared,
                        vm_gc_roots,
                        args_roots,
                        &elt,
                        result,
                        depth + 1,
                    )?;
                }
                return Ok(());
            }

            if let Value::Int(lim) = car {
                let mut nested = String::new();
                format_mode_line_recursive_in_vm_runtime(
                    shared,
                    vm_gc_roots,
                    args_roots,
                    &cdr,
                    &mut nested,
                    depth + 1,
                )?;
                append_mode_line_rendered_segment(
                    result,
                    &nested,
                    if lim > 0 { lim } else { 0 },
                    if lim < 0 { -lim } else { 0 },
                );
                return Ok(());
            }

            if car.is_symbol() && !car.is_symbol_named("t") {
                if let Some(sym_name) = car.as_symbol_name() {
                    let value = {
                        let obarray = &*shared.obarray;
                        let dynamic = shared.dynamic.as_slice();
                        let buffers = &*shared.buffers;
                        mode_line_symbol_value_in_state(obarray, dynamic, buffers, sym_name)
                    };
                    let branch = if value.is_some_and(|value| value.is_truthy()) {
                        mode_line_conditional_branch(cdr, true)
                    } else {
                        mode_line_conditional_branch(cdr, false)
                    };
                    if let Some(branch) = branch {
                        format_mode_line_recursive_in_vm_runtime(
                            shared,
                            vm_gc_roots,
                            args_roots,
                            &branch,
                            result,
                            depth + 1,
                        )?;
                    }
                }
                return Ok(());
            }

            if let Some(elements) = list_to_vec(format) {
                for elem in &elements {
                    format_mode_line_recursive_in_vm_runtime(
                        shared,
                        vm_gc_roots,
                        args_roots,
                        elem,
                        result,
                        depth + 1,
                    )?;
                }
            }
        }

        _ => {
            if let Some(s) = format.as_str() {
                result.push_str(s);
            }
        }
    }

    Ok(())
}

fn expand_mode_line_percent_in_state(
    buffers: &crate::buffer::BufferManager,
    fmt_str: &str,
    result: &mut String,
) {
    let buf = buffers.current_buffer();
    let buf_name = buf.map(|b| b.name.as_str()).unwrap_or("*scratch*");
    let file_name = buf.and_then(|b| b.file_name.as_deref()).unwrap_or("");
    let modified = buf.map(|b| b.is_modified()).unwrap_or(false);

    let (line_num, col_num) = if let Some(b) = buf {
        let pt = b.pt;
        let text = b.text.to_string();
        let before = &text[..pt.min(text.len())];
        let line = before.chars().filter(|&c| c == '\n').count() + 1;
        let col = before.rfind('\n').map(|nl| pt - nl - 1).unwrap_or(pt);
        (line, col)
    } else {
        (1, 0)
    };

    let mut chars = fmt_str.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            if chars.peek() == Some(&'-') {
                chars.next();
            }
            while chars.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                chars.next();
            }
            match chars.next() {
                Some('b') => result.push_str(buf_name),
                Some('f') => result.push_str(file_name),
                Some('F') => result.push_str("Neomacs"),
                Some('*') => result.push(if modified { '*' } else { '-' }),
                Some('+') => result.push(if modified { '+' } else { '-' }),
                Some('-') => result.push('-'),
                Some('%') => result.push('%'),
                Some('n') => {}
                Some('l') => result.push_str(&line_num.to_string()),
                Some('c') => result.push_str(&col_num.to_string()),
                Some('p') | Some('P') => {
                    if let Some(b) = buf {
                        let total = b.text.len();
                        if total == 0 {
                            result.push_str("All");
                        } else {
                            let pct = (b.pt * 100) / total;
                            if pct == 0 {
                                result.push_str("Top");
                            } else if pct >= 99 {
                                result.push_str("Bot");
                            } else {
                                result.push_str(&format!("{}%", pct));
                            }
                        }
                    }
                }
                Some('z') => result.push_str("U"),
                Some('@') => result.push('-'),
                Some('Z') => result.push_str("U"),
                Some('[') | Some(']') => {}
                Some('e') => {}
                Some(' ') => result.push(' '),
                Some(c) => {
                    result.push('%');
                    result.push(c);
                }
                None => result.push('%'),
            }
        } else {
            result.push(ch);
        }
    }
}

/// (invisible-p POS-OR-PROP) -> boolean
///
/// Batch semantics mirror current oracle behavior:
/// - numeric positions > 0 are visible (nil),
/// - position 0 is out-of-range,
/// - negative numeric positions are invisible (t),
/// - nil is visible (nil),
/// - all other property values are treated as invisible (t).
pub(crate) fn builtin_invisible_p(args: Vec<Value>) -> EvalResult {
    expect_args("invisible-p", &args, 1)?;
    match &args[0] {
        Value::Int(v) => {
            if *v == 0 {
                Err(signal("args-out-of-range", vec![Value::Int(*v)]))
            } else if *v < 0 {
                Ok(Value::symbol("t"))
            } else {
                Ok(Value::Nil)
            }
        }
        Value::Char(ch) => {
            if *ch == '\0' {
                Err(signal("args-out-of-range", vec![Value::Char(*ch)]))
            } else {
                Ok(Value::Nil)
            }
        }
        Value::Nil => Ok(Value::Nil),
        _ => Ok(Value::symbol("t")),
    }
}

/// (line-pixel-height) -> integer
///
/// Batch-compatible behavior returns 1.
pub(crate) fn builtin_line_pixel_height(args: Vec<Value>) -> EvalResult {
    expect_args("line-pixel-height", &args, 0)?;
    Ok(Value::Int(1))
}

/// (window-text-pixel-size &optional WINDOW FROM TO X-LIMIT Y-LIMIT MODE) -> (WIDTH . HEIGHT)
///
/// Batch-compatible behavior returns `(0 . 0)` and enforces argument
/// validation for WINDOW / FROM / TO.
pub(crate) fn builtin_window_text_pixel_size(args: Vec<Value>) -> EvalResult {
    expect_args_range("window-text-pixel-size", &args, 0, 7)?;

    if let Some(window) = args.first() {
        if !window.is_nil() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("window-live-p"), *window],
            ));
        }
    }
    if let Some(from) = args.get(1) {
        if !from.is_nil() {
            expect_integer_or_marker(from)?;
        }
    }
    if let Some(to) = args.get(2) {
        if !to.is_nil() {
            expect_integer_or_marker(to)?;
        }
    }

    Ok(Value::cons(Value::Int(0), Value::Int(0)))
}

/// `(window-text-pixel-size &optional WINDOW FROM TO X-LIMIT Y-LIMIT MODE)` evaluator-backed variant.
///
/// Batch mode returns `(0 . 0)` and validates optional WINDOW / FROM / TO
/// designators against evaluator state.
pub(crate) fn builtin_window_text_pixel_size_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_window_text_pixel_size_in_state(&mut eval.frames, &mut eval.buffers, args)
}

pub(crate) fn builtin_window_text_pixel_size_in_state(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args_range("window-text-pixel-size", &args, 0, 7)?;
    validate_optional_window_designator_in_state(&*frames, args.first(), "window-live-p")?;
    if let Some(from) = args.get(1) {
        if !from.is_nil() {
            expect_integer_or_marker(from)?;
        }
    }
    if let Some(to) = args.get(2) {
        if !to.is_nil() {
            expect_integer_or_marker(to)?;
        }
    }
    Ok(Value::cons(Value::Int(0), Value::Int(0)))
}

/// (pos-visible-in-window-p &optional POS WINDOW PARTIALLY) -> boolean
///
/// Batch-compatible behavior: no window visibility is reported, so this
/// returns nil.
pub(crate) fn builtin_pos_visible_in_window_p(args: Vec<Value>) -> EvalResult {
    expect_args_range("pos-visible-in-window-p", &args, 0, 3)?;
    if let Some(window) = args.get(1) {
        if !window.is_nil() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("window-live-p"), *window],
            ));
        }
    }
    // POS can be nil (point), t (end of buffer), or an integer/marker.
    if let Some(pos) = args.first() {
        if !pos.is_nil() && !matches!(pos, Value::True) && !pos.is_symbol_named("t") {
            expect_integer_or_marker(pos)?;
        }
    }
    Ok(Value::Nil)
}

/// `(pos-visible-in-window-p &optional POS WINDOW PARTIALLY)` evaluator-backed variant.
///
/// Mirror GNU Emacs: return t if POS is visible in WINDOW, nil otherwise.
/// Checks if position is between window-start and an estimated window-end.
pub(crate) fn builtin_pos_visible_in_window_p_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_pos_visible_in_window_p_in_state(&mut eval.frames, &mut eval.buffers, args)
}

pub(crate) fn builtin_pos_visible_in_window_p_in_state(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args_range("pos-visible-in-window-p", &args, 0, 3)?;
    validate_optional_window_designator_in_state(&*frames, args.get(1), "window-live-p")?;
    let Some(ctx) = resolve_live_window_display_context(frames, buffers, args.get(1))? else {
        return Ok(Value::Nil);
    };
    let partially = args.get(2).is_some_and(Value::is_truthy);
    let Some(pos_lisp) = resolve_pos_visible_target_lisp_pos(&ctx, args.first())? else {
        return Ok(Value::Nil);
    };
    let Some(metrics) = approximate_pos_visible_metrics(&ctx, pos_lisp) else {
        return Ok(Value::Nil);
    };
    if !partially && !metrics.fully_visible {
        return Ok(Value::Nil);
    }
    if !partially {
        return Ok(Value::True);
    }
    let mut out = vec![Value::Int(metrics.x), Value::Int(metrics.y)];
    if !metrics.fully_visible {
        out.extend([
            Value::Int(metrics.rtop),
            Value::Int(metrics.rbot),
            Value::Int(metrics.row_height),
            Value::Int(metrics.vpos),
        ]);
    }
    Ok(Value::list(out))
}

/// `(window-line-height &optional LINE WINDOW)` evaluator-backed variant.
///
/// GNU Emacs returns `(HEIGHT VPOS YPOS OFFBOT)` for a live GUI window.  We
/// approximate this from the current frame/window geometry so commands in
/// `simple.el` can reason about visual line movement without batch fallbacks.
pub(crate) fn builtin_window_line_height_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_window_line_height_in_state(&mut eval.frames, &mut eval.buffers, args)
}

pub(crate) fn builtin_window_line_height_in_state(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args_range("window-line-height", &args, 0, 2)?;
    validate_optional_window_designator_in_state(&*frames, args.get(1), "window-live-p")?;
    let Some(ctx) = resolve_live_window_display_context(frames, buffers, args.get(1))? else {
        return Ok(Value::Nil);
    };

    let line_spec = args.first().copied().unwrap_or(Value::Nil);
    let metrics = if line_spec.is_nil() {
        let current_pos = current_window_point_lisp(&ctx);
        approximate_pos_visible_metrics(&ctx, current_pos)
            .map(ApproxVisibleMetrics::as_window_line_height)
    } else if line_spec.is_symbol_named("mode-line") {
        if ctx.is_minibuffer {
            None
        } else {
            Some(WindowLineMetrics {
                height: ctx.char_height,
                vpos: 0,
                ypos: ctx.body_height,
                offbot: 0,
            })
        }
    } else if line_spec.is_symbol_named("header-line") || line_spec.is_symbol_named("tab-line") {
        None
    } else {
        let line_num = match line_spec {
            Value::Int(n) => n,
            Value::Char(ch) => ch as i64,
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), other],
                ));
            }
        };
        let row = if line_num < 0 {
            ctx.body_lines + line_num
        } else {
            line_num
        };
        if row < 0 || row >= ctx.body_lines {
            None
        } else {
            Some(window_row_metrics(&ctx, row))
        }
    };

    let Some(metrics) = metrics else {
        return Ok(Value::Nil);
    };
    Ok(Value::list(vec![
        Value::Int(metrics.height),
        Value::Int(metrics.vpos),
        Value::Int(metrics.ypos),
        Value::Int(metrics.offbot),
    ]))
}

/// (move-point-visually DIRECTION) -> boolean
///
/// Batch semantics: direction is validated as a fixnum and the command
/// signals `args-out-of-range` in non-window contexts.
pub(crate) fn builtin_move_point_visually(args: Vec<Value>) -> EvalResult {
    expect_args("move-point-visually", &args, 1)?;
    match &args[0] {
        Value::Int(v) => Err(signal(
            "args-out-of-range",
            vec![Value::Int(*v), Value::Int(*v)],
        )),
        Value::Char(ch) => Err(signal(
            "args-out-of-range",
            vec![Value::Char(*ch), Value::Char(*ch)],
        )),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("fixnump"), *other],
        )),
    }
}

/// (lookup-image-map MAP X Y) -> symbol or nil
///
/// Lookup an image map at coordinates. Stub implementation
/// returns nil while preserving arity validation.
pub(crate) fn builtin_lookup_image_map(args: Vec<Value>) -> EvalResult {
    expect_args("lookup-image-map", &args, 3)?;
    if !args[0].is_nil() {
        expect_fixnum_arg("fixnump", &args[1])?;
        expect_fixnum_arg("fixnump", &args[2])?;
    }
    Ok(Value::Nil)
}

/// (current-bidi-paragraph-direction &optional BUFFER) -> symbol
///
/// Get the bidi paragraph direction. Returns the symbol 'left-to-right.
pub(crate) fn builtin_current_bidi_paragraph_direction(args: Vec<Value>) -> EvalResult {
    expect_args_range("current-bidi-paragraph-direction", &args, 0, 1)?;
    if let Some(bufferish) = args.first() {
        if !bufferish.is_nil() && !matches!(bufferish, Value::Buffer(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("bufferp"), *bufferish],
            ));
        }
    }
    // Return 'left-to-right
    Ok(Value::symbol("left-to-right"))
}

/// `(bidi-resolved-levels &optional PARAGRAPH-DIRECTION)` -> nil
///
/// Batch compatibility: this currently returns nil and only enforces the
/// `fixnump` argument contract when PARAGRAPH-DIRECTION is non-nil.
pub(crate) fn builtin_bidi_resolved_levels(args: Vec<Value>) -> EvalResult {
    expect_args_range("bidi-resolved-levels", &args, 0, 1)?;
    if let Some(direction) = args.first() {
        if !direction.is_nil() {
            expect_fixnum_arg("fixnump", direction)?;
        }
    }
    Ok(Value::Nil)
}

/// `(bidi-find-overridden-directionality STRING/START END/START STRING/END
/// &optional DIRECTION)` -> nil
///
/// Batch compatibility mirrors oracle argument guards:
/// - when arg3 is a string, this path accepts arg1/arg2 without additional
///   type checks and returns nil;
/// - when arg3 is nil, arg1 and arg2 must satisfy `integer-or-marker-p`.
pub(crate) fn builtin_bidi_find_overridden_directionality(args: Vec<Value>) -> EvalResult {
    expect_args_range("bidi-find-overridden-directionality", &args, 3, 4)?;
    let third = &args[2];
    if third.is_nil() {
        expect_integer_or_marker(&args[0])?;
        expect_integer_or_marker(&args[1])?;
        return Ok(Value::Nil);
    }
    if !matches!(third, Value::Str(_)) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *third],
        ));
    }
    Ok(Value::Nil)
}

/// (move-to-window-line ARG) -> integer or nil
///
/// Batch semantics: in non-window contexts this command errors with the
/// standard unrelated-buffer message.
pub(crate) fn builtin_move_to_window_line(args: Vec<Value>) -> EvalResult {
    expect_args("move-to-window-line", &args, 1)?;
    Err(signal(
        "error",
        vec![Value::string(
            "move-to-window-line called from unrelated buffer",
        )],
    ))
}

/// (tool-bar-height &optional FRAME PIXELWISE) -> integer
///
/// Get the height of the tool bar. Returns 0 (no tool bar).
pub(crate) fn builtin_tool_bar_height(args: Vec<Value>) -> EvalResult {
    expect_args_range("tool-bar-height", &args, 0, 2)?;
    // Return 0 (no tool bar)
    Ok(Value::Int(0))
}

/// `(tool-bar-height &optional FRAME PIXELWISE)` evaluator-backed variant.
///
/// Accepts nil or a live frame designator for FRAME.
pub(crate) fn builtin_tool_bar_height_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_tool_bar_height_in_state(&mut eval.frames, &mut eval.buffers, args)
}

pub(crate) fn builtin_tool_bar_height_in_state(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args_range("tool-bar-height", &args, 0, 2)?;
    if let Some(frame) = args.first().filter(|frame| !frame.is_nil()) {
        let _ =
            super::window_cmds::resolve_frame_id_in_state(frames, buffers, Some(frame), "framep")?;
    }
    Ok(Value::Int(0))
}

/// (tab-bar-height &optional FRAME PIXELWISE) -> integer
///
/// Get the height of the tab bar. Returns 0 (no tab bar).
pub(crate) fn builtin_tab_bar_height(args: Vec<Value>) -> EvalResult {
    expect_args_range("tab-bar-height", &args, 0, 2)?;
    // Return 0 (no tab bar)
    Ok(Value::Int(0))
}

/// `(tab-bar-height &optional FRAME PIXELWISE)` evaluator-backed variant.
///
/// Accepts nil or a live frame designator for FRAME.
pub(crate) fn builtin_tab_bar_height_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_tab_bar_height_in_state(&mut eval.frames, &mut eval.buffers, args)
}

pub(crate) fn builtin_tab_bar_height_in_state(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args_range("tab-bar-height", &args, 0, 2)?;
    if let Some(frame) = args.first().filter(|frame| !frame.is_nil()) {
        let _ =
            super::window_cmds::resolve_frame_id_in_state(frames, buffers, Some(frame), "framep")?;
    }
    Ok(Value::Int(0))
}

/// (line-number-display-width &optional ON-DISPLAY) -> integer
///
/// Get the width of the line number display. Returns 0 (no line numbers).
pub(crate) fn builtin_line_number_display_width(args: Vec<Value>) -> EvalResult {
    expect_args_range("line-number-display-width", &args, 0, 1)?;
    // Return 0 (no line numbers)
    Ok(Value::Int(0))
}

/// (long-line-optimizations-p) -> boolean
///
/// Check if long-line optimizations are enabled. Returns nil.
pub(crate) fn builtin_long_line_optimizations_p(args: Vec<Value>) -> EvalResult {
    expect_args("long-line-optimizations-p", &args, 0)?;
    // Return nil (optimizations not enabled)
    Ok(Value::Nil)
}

fn validate_optional_frame_designator(
    eval: &super::eval::Evaluator,
    value: Option<&Value>,
) -> Result<(), Flow> {
    validate_optional_frame_designator_in_state(&eval.frames, value)
}

fn validate_optional_frame_designator_in_state(
    frames: &crate::window::FrameManager,
    value: Option<&Value>,
) -> Result<(), Flow> {
    let Some(frameish) = value else {
        return Ok(());
    };
    if frameish.is_nil() {
        return Ok(());
    }
    match frameish {
        Value::Int(id) if *id >= 0 => {
            if frames.get(FrameId(*id as u64)).is_some() {
                return Ok(());
            }
        }
        Value::Frame(id) => {
            if frames.get(FrameId(*id)).is_some() {
                return Ok(());
            }
        }
        _ => {}
    }
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("framep"), *frameish],
    ))
}

fn validate_optional_window_designator(
    eval: &super::eval::Evaluator,
    value: Option<&Value>,
    predicate: &str,
) -> Result<(), Flow> {
    validate_optional_window_designator_in_state(&eval.frames, value, predicate)
}

fn validate_optional_window_designator_in_state(
    frames: &crate::window::FrameManager,
    value: Option<&Value>,
    predicate: &str,
) -> Result<(), Flow> {
    let Some(windowish) = value else {
        return Ok(());
    };
    if windowish.is_nil() {
        return Ok(());
    }
    let wid = match windowish {
        Value::Window(id) => Some(WindowId(*id)),
        Value::Int(id) if *id >= 0 => Some(WindowId(*id as u64)),
        _ => None,
    };
    if let Some(wid) = wid {
        for fid in frames.frame_list() {
            if let Some(frame) = frames.get(fid) {
                if frame.find_window(wid).is_some() {
                    return Ok(());
                }
            }
        }
    }
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol(predicate), *windowish],
    ))
}

fn validate_optional_buffer_designator(
    eval: &super::eval::Evaluator,
    value: Option<&Value>,
) -> Result<(), Flow> {
    validate_optional_buffer_designator_in_state(&eval.buffers, value)
}

fn validate_optional_buffer_designator_in_state(
    buffers: &crate::buffer::BufferManager,
    value: Option<&Value>,
) -> Result<(), Flow> {
    let Some(bufferish) = value else {
        return Ok(());
    };
    if bufferish.is_nil() {
        return Ok(());
    }
    if let Value::Buffer(id) = bufferish {
        if buffers.get(*id).is_some() {
            return Ok(());
        }
    }
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("bufferp"), *bufferish],
    ))
}

fn resolve_optional_window_buffer(
    eval: &super::eval::Evaluator,
    value: Option<&Value>,
) -> Option<BufferId> {
    let windowish = value?;
    if windowish.is_nil() {
        return None;
    }

    let wid = match windowish {
        Value::Window(id) => Some(WindowId(*id)),
        Value::Int(id) if *id >= 0 => Some(WindowId(*id as u64)),
        _ => None,
    }?;

    for fid in eval.frames.frame_list() {
        let Some(frame) = eval.frames.get(fid) else {
            continue;
        };
        if let Some(window) = frame.find_window(wid) {
            return window.buffer_id();
        }
    }

    None
}

fn resolve_optional_window_buffer_in_state(
    frames: &crate::window::FrameManager,
    value: Option<&Value>,
) -> Option<BufferId> {
    let windowish = value?;
    if windowish.is_nil() {
        return None;
    }

    let wid = match windowish {
        Value::Window(id) => Some(WindowId(*id)),
        Value::Int(id) if *id >= 0 => Some(WindowId(*id as u64)),
        _ => None,
    }?;

    for fid in frames.frame_list() {
        let Some(frame) = frames.get(fid) else {
            continue;
        };
        if let Some(window) = frame.find_window(wid) {
            return window.buffer_id();
        }
    }

    None
}

fn resolve_mode_line_buffer(
    eval: &super::eval::Evaluator,
    window: Option<&Value>,
    buffer: Option<&Value>,
) -> Option<BufferId> {
    match buffer {
        Some(Value::Buffer(id)) => Some(*id),
        _ => resolve_optional_window_buffer(eval, window),
    }
}

fn resolve_mode_line_buffer_in_state(
    frames: &crate::window::FrameManager,
    window: Option<&Value>,
    buffer: Option<&Value>,
) -> Option<BufferId> {
    match buffer {
        Some(Value::Buffer(id)) => Some(*id),
        _ => resolve_optional_window_buffer_in_state(frames, window),
    }
}

#[derive(Clone)]
struct ApproxWindowDisplayContext {
    body_height: i64,
    body_lines: i64,
    char_width: i64,
    char_height: i64,
    window_start: usize,
    window_point: usize,
    chars: Vec<char>,
    is_minibuffer: bool,
}

#[derive(Clone, Copy)]
struct ApproxVisibleMetrics {
    x: i64,
    y: i64,
    rtop: i64,
    rbot: i64,
    row_height: i64,
    vpos: i64,
    fully_visible: bool,
}

#[derive(Clone, Copy)]
struct WindowLineMetrics {
    height: i64,
    vpos: i64,
    ypos: i64,
    offbot: i64,
}

impl ApproxVisibleMetrics {
    fn as_window_line_height(self) -> WindowLineMetrics {
        WindowLineMetrics {
            height: self.row_height,
            vpos: self.vpos,
            ypos: self.y,
            offbot: self.rbot,
        }
    }
}

fn resolve_live_window_display_context(
    frames: &crate::window::FrameManager,
    buffers: &crate::buffer::BufferManager,
    window: Option<&Value>,
) -> Result<Option<ApproxWindowDisplayContext>, Flow> {
    let Some((fid, wid)) = resolve_live_window_identity(frames, window)? else {
        return Ok(None);
    };
    let Some(frame) = frames.get(fid) else {
        return Ok(None);
    };
    let Some(window_ref) = frame.find_window(wid) else {
        return Ok(None);
    };
    let Some(buffer_id) = window_ref.buffer_id() else {
        return Ok(None);
    };
    let Some(buffer) = buffers.get(buffer_id) else {
        return Ok(None);
    };

    let Window::Leaf {
        bounds,
        window_start,
        point,
        ..
    } = window_ref
    else {
        return Ok(None);
    };

    let char_width = frame.char_width.max(1.0).round() as i64;
    let char_height = frame.char_height.max(1.0).round() as i64;
    let body_top = bounds.y.max(0.0) as i64;
    let body_bottom = (bounds.y + bounds.height).max(0.0) as i64
        - if frame.minibuffer_window == Some(wid) {
            0
        } else {
            char_height
        };
    let body_height = (body_bottom - body_top).max(1);
    let body_lines = ((body_height + char_height - 1) / char_height).max(1);
    let chars = buffer.text.to_string().chars().collect::<Vec<_>>();
    let window_point =
        if frame.selected_window == wid && buffers.current_buffer_id() == Some(buffer_id) {
            buffer.point_char().saturating_add(1)
        } else {
            (*point).max(1)
        };

    Ok(Some(ApproxWindowDisplayContext {
        body_height,
        body_lines,
        char_width,
        char_height,
        window_start: (*window_start).max(1),
        window_point,
        chars,
        is_minibuffer: frame.minibuffer_window == Some(wid),
    }))
}

fn resolve_live_window_identity(
    frames: &crate::window::FrameManager,
    window: Option<&Value>,
) -> Result<Option<(FrameId, WindowId)>, Flow> {
    let Some(windowish) = window else {
        return Ok(frames
            .selected_frame()
            .map(|frame| (frame.id, frame.selected_window)));
    };
    if windowish.is_nil() {
        return Ok(frames
            .selected_frame()
            .map(|frame| (frame.id, frame.selected_window)));
    }
    let wid = match windowish {
        Value::Window(id) => WindowId(*id),
        Value::Int(id) if *id >= 0 => WindowId(*id as u64),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("window-live-p"), *other],
            ));
        }
    };
    for fid in frames.frame_list() {
        if frames
            .get(fid)
            .is_some_and(|frame| frame.find_window(wid).is_some())
        {
            return Ok(Some((fid, wid)));
        }
    }
    Ok(None)
}

fn resolve_pos_visible_target_lisp_pos(
    ctx: &ApproxWindowDisplayContext,
    pos: Option<&Value>,
) -> Result<Option<usize>, Flow> {
    match pos {
        Some(value) if matches!(value, Value::True) || value.is_symbol_named("t") => {
            Ok(Some(last_visible_row_start_lisp_pos(ctx)))
        }
        Some(value) if !value.is_nil() => {
            expect_integer_or_marker(value)?;
            let lisp_pos = value.as_int().unwrap_or(0).max(1) as usize;
            Ok(Some(lisp_pos.min(ctx.chars.len().saturating_add(1))))
        }
        _ => Ok(Some(current_window_point_lisp(ctx))),
    }
}

fn current_window_point_lisp(ctx: &ApproxWindowDisplayContext) -> usize {
    ctx.window_point
        .max(1)
        .min(ctx.chars.len().saturating_add(1))
}

fn last_visible_row_start_lisp_pos(ctx: &ApproxWindowDisplayContext) -> usize {
    let row_start = nth_visible_row_start_char(
        &ctx.chars,
        ctx.window_start.saturating_sub(1),
        ctx.body_lines.saturating_sub(1),
    );
    row_start
        .saturating_add(1)
        .min(ctx.chars.len().saturating_add(1))
}

fn nth_visible_row_start_char(chars: &[char], mut start_char: usize, rows: i64) -> usize {
    start_char = start_char.min(chars.len());
    for _ in 0..rows.max(0) {
        if start_char >= chars.len() {
            return chars.len();
        }
        match chars[start_char..].iter().position(|ch| *ch == '\n') {
            Some(offset) => start_char += offset + 1,
            None => return chars.len(),
        }
    }
    start_char
}

fn row_col_for_lisp_pos(chars: &[char], start_char: usize, lisp_pos: usize) -> Option<(i64, i64)> {
    if lisp_pos == 0 {
        return None;
    }
    let target = lisp_pos.saturating_sub(1).min(chars.len());
    let mut row = 0_i64;
    let mut col = 0_i64;
    let mut idx = start_char.min(chars.len());
    while idx < target {
        if chars[idx] == '\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
        idx += 1;
    }
    Some((row, col))
}

fn approximate_pos_visible_metrics(
    ctx: &ApproxWindowDisplayContext,
    pos_lisp: usize,
) -> Option<ApproxVisibleMetrics> {
    if pos_lisp < ctx.window_start {
        return None;
    }
    let start_char = ctx.window_start.saturating_sub(1);
    let (row, col) = row_col_for_lisp_pos(&ctx.chars, start_char, pos_lisp)?;
    if row < 0 || row >= ctx.body_lines {
        return None;
    }
    let row_metrics = window_row_metrics(ctx, row);
    Some(ApproxVisibleMetrics {
        x: col.saturating_mul(ctx.char_width),
        y: row_metrics.ypos,
        rtop: 0,
        rbot: row_metrics.offbot,
        row_height: row_metrics.height,
        vpos: row_metrics.vpos,
        fully_visible: row_metrics.offbot == 0,
    })
}

fn window_row_metrics(ctx: &ApproxWindowDisplayContext, row: i64) -> WindowLineMetrics {
    let ypos = row.saturating_mul(ctx.char_height);
    let row_bottom = (row + 1).saturating_mul(ctx.char_height);
    let offbot = (row_bottom - ctx.body_height).max(0);
    WindowLineMetrics {
        height: (ctx.char_height - offbot).max(1),
        vpos: row,
        ypos,
        offbot,
    }
}

// ---------------------------------------------------------------------------
// Bootstrap variables
// ---------------------------------------------------------------------------

pub fn register_bootstrap_vars(obarray: &mut crate::emacs_core::symbol::Obarray) {
    obarray.set_symbol_value("redisplay--inhibit-bidi", Value::True);
    obarray.set_symbol_value("blink-matching-delay", Value::Int(1));
    obarray.set_symbol_value("blink-matching-paren", Value::True);
    obarray.set_symbol_value("global-font-lock-mode", Value::Nil);
    obarray.set_symbol_value("display-line-numbers", Value::Nil);
    obarray.set_symbol_value("display-line-numbers-type", Value::True);
    obarray.set_symbol_value("display-line-numbers-width", Value::Nil);
    obarray.set_symbol_value("display-line-numbers-current-absolute", Value::True);
    obarray.set_symbol_value("display-line-numbers-widen", Value::Nil);
    obarray.set_symbol_value("display-fill-column-indicator", Value::Nil);
    obarray.set_symbol_value("display-fill-column-indicator-column", Value::Nil);
    obarray.set_symbol_value("display-fill-column-indicator-character", Value::Nil);
    obarray.set_symbol_value("visible-bell", Value::Nil);
    obarray.set_symbol_value("no-redraw-on-reenter", Value::Nil);
    obarray.set_symbol_value("cursor-in-echo-area", Value::Nil);
    obarray.set_symbol_value("truncate-partial-width-windows", Value::Int(50));
    obarray.set_symbol_value("mode-line-in-non-selected-windows", Value::True);
    obarray.set_symbol_value("line-number-display-limit", Value::Nil);
    obarray.set_symbol_value("highlight-nonselected-windows", Value::Nil);
    obarray.set_symbol_value("message-truncate-lines", Value::Nil);
    obarray.set_symbol_value("scroll-step", Value::Int(0));
    obarray.set_symbol_value("scroll-conservatively", Value::Int(0));
    obarray.set_symbol_value("scroll-margin", Value::Int(0));
    obarray.set_symbol_value("hscroll-margin", Value::Int(5));
    obarray.set_symbol_value("hscroll-step", Value::Int(0));
    obarray.set_symbol_value("auto-hscroll-mode", Value::True);
    obarray.set_symbol_value("void-text-area-pointer", Value::symbol("arrow"));
    obarray.set_symbol_value("inhibit-message", Value::Nil);
    obarray.set_symbol_value("make-cursor-line-fully-visible", Value::True);
    obarray.set_symbol_value("x-stretch-cursor", Value::Nil);
    obarray.set_symbol_value("show-trailing-whitespace", Value::Nil);
    obarray.set_symbol_value("show-paren-context-when-offscreen", Value::Nil);
    obarray.set_symbol_value("nobreak-char-display", Value::True);
    obarray.set_symbol_value("overlay-arrow-variable-list", Value::Nil);
    obarray.set_symbol_value("overlay-arrow-string", Value::string("=>"));
    obarray.set_symbol_value("overlay-arrow-position", Value::Nil);
    // Mirror GNU Emacs: set char-table-extra-slots property for all subtypes
    // that need extra slots. Fmake_char_table reads this property to allocate
    // the correct number of extra slots.
    // See: casetab.c:249, category.c:426, character.c:1143, coding.c:11737,
    //      fontset.c:2158-2160, xdisp.c:31594, keymap.c:3346, syntax.c:3659
    obarray.put_property("case-table", "char-table-extra-slots", Value::Int(3));
    obarray.put_property("category-table", "char-table-extra-slots", Value::Int(2));
    obarray.put_property("char-script-table", "char-table-extra-slots", Value::Int(1));
    obarray.put_property("translation-table", "char-table-extra-slots", Value::Int(2));
    obarray.put_property("fontset", "char-table-extra-slots", Value::Int(8));
    obarray.put_property("fontset-info", "char-table-extra-slots", Value::Int(1));
    obarray.put_property(
        "glyphless-char-display",
        "char-table-extra-slots",
        Value::Int(1),
    );
    obarray.put_property("keymap", "char-table-extra-slots", Value::Int(0));
    obarray.put_property("syntax-table", "char-table-extra-slots", Value::Int(0));
    obarray.set_symbol_value(
        "char-script-table",
        make_char_table_with_extra_slots(Value::symbol("char-script-table"), Value::Nil, 1),
    );
    obarray.set_symbol_value("pre-redisplay-function", Value::Nil);
    obarray.set_symbol_value("pre-redisplay-functions", Value::Nil);

    // auto-fill-chars: a char-table for characters which invoke auto-filling.
    // Official Emacs (character.c) creates it with sub-type `auto-fill-chars`
    // and sets space and newline to t.
    let auto_fill = make_char_table_value(Value::symbol("auto-fill-chars"), Value::Nil);
    // Set space and newline entries to t.  We use set-char-table-range
    // via the underlying data: store single-char entries.
    use super::chartable::ct_set_single;
    ct_set_single(&auto_fill, ' ' as i64, Value::True);
    ct_set_single(&auto_fill, '\n' as i64, Value::True);
    obarray.set_symbol_value("auto-fill-chars", auto_fill);

    // char-width-table: a char-table for character display widths.
    // Official Emacs (character.c) creates it with default 1.
    obarray.set_symbol_value(
        "char-width-table",
        make_char_table_value(Value::symbol("char-width-table"), Value::Int(1)),
    );

    // translation-table-vector: vector recording all translation tables.
    // Official Emacs (character.c) creates a 16-element nil vector.
    obarray.set_symbol_value(
        "translation-table-vector",
        Value::vector(vec![Value::Nil; 16]),
    );

    // translation-hash-table-vector: vector of translation hash tables.
    // Official Emacs (ccl.c) initializes to nil.
    obarray.set_symbol_value("translation-hash-table-vector", Value::Nil);

    // printable-chars: a char-table of printable characters.
    // Official Emacs (character.c) creates it with default t.
    obarray.set_symbol_value(
        "printable-chars",
        make_char_table_value(Value::symbol("printable-chars"), Value::True),
    );

    // default-process-coding-system: cons of coding systems for process I/O.
    // Official Emacs (coding.c) initializes to nil.
    obarray.set_symbol_value("default-process-coding-system", Value::Nil);

    // ambiguous-width-chars: char-table for characters whose width can be 1 or 2.
    // Official Emacs (character.c) creates empty char-table; populated by characters.el.
    obarray.set_symbol_value(
        "ambiguous-width-chars",
        make_char_table_value(Value::Nil, Value::Nil),
    );

    // text-property-default-nonsticky: alist of properties vs non-stickiness.
    // Official Emacs (textprop.c) initializes to ((syntax-table . t) (display . t)).
    obarray.set_symbol_value(
        "text-property-default-nonsticky",
        Value::list(vec![
            Value::cons(Value::symbol("syntax-table"), Value::True),
            Value::cons(Value::symbol("display"), Value::True),
        ]),
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "xdisp_test.rs"]
mod tests;
