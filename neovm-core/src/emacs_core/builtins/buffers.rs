use super::*;

// ===========================================================================
// Buffer operations (require evaluator for BufferManager access)
// ===========================================================================

use crate::buffer::{BufferId, BufferManager};
use crate::emacs_core::filelock;
use crate::emacs_core::misc;
use crate::emacs_core::value::{
    ValueKind, VecLikeType, get_string_text_properties_table_for_value,
    set_string_text_properties_table_for_value,
};
use crate::window::FrameManager;

#[derive(Clone, Copy)]
pub(crate) struct MakeIndirectBufferPlan {
    pub(crate) id: BufferId,
    pub(crate) saved_current: Option<BufferId>,
    pub(crate) run_clone_hook: bool,
}

pub(super) fn expect_buffer_id(value: &Value) -> Result<BufferId, Flow> {
    match value.kind() {
        ValueKind::Veclike(VecLikeType::Buffer) => Ok(value.as_buffer_id().unwrap()),
        _other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("bufferp"), *value],
        )),
    }
}

fn expect_buffer_name_string(value: &Value) -> Result<String, Flow> {
    value.as_runtime_string_owned().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *value],
        )
    })
}

fn find_buffer_by_name_arg(
    buffers: &BufferManager,
    value: &Value,
) -> Result<Option<BufferId>, Flow> {
    let name = expect_buffer_name_string(value)?;
    Ok(buffers.find_buffer_by_name(&name))
}

fn delete_quit_restore_popup_windows_showing_buffer(
    frames: &mut FrameManager,
    buffer_id: BufferId,
) -> bool {
    let mut deleted_any = false;
    let quit_restore_key = Value::symbol("quit-restore");
    let buffer_value = Value::make_buffer(buffer_id);

    for frame_id in frames.frame_list() {
        let Some(window_ids) = frames.get(frame_id).map(|frame| frame.window_list()) else {
            continue;
        };

        for window_id in window_ids {
            let should_delete = {
                let Some(frame) = frames.get(frame_id) else {
                    continue;
                };
                if frame.minibuffer_window == Some(window_id) || frame.window_list().len() <= 1 {
                    false
                } else if frame
                    .find_window(window_id)
                    .and_then(|window| window.buffer_id())
                    != Some(buffer_id)
                {
                    false
                } else {
                    match frames.window_parameter(window_id, &quit_restore_key) {
                        Some(quit_restore) => {
                            match crate::emacs_core::value::list_to_vec(&quit_restore) {
                                Some(items) => {
                                    items.len() >= 4
                                        && items[0].as_symbol_name() == Some("window")
                                        && items[1].as_symbol_name() == Some("window")
                                        && eq_value(&items[3], &buffer_value)
                                }
                                None => false,
                            }
                        }
                        None => false,
                    }
                }
            };

            if should_delete && frames.delete_window(frame_id, window_id) {
                deleted_any = true;
            }
        }
    }

    deleted_any
}

fn sync_current_buffer_to_selected_window(eval: &mut super::eval::Context) {
    let Some(frame_id) = eval.frames.selected_frame().map(|frame| frame.id) else {
        return;
    };
    let selected_buffer_id = eval
        .frames
        .get(frame_id)
        .and_then(|frame| frame.find_window(frame.selected_window))
        .and_then(|window| window.buffer_id());
    if let Some(buffer_id) = selected_buffer_id {
        let _ = eval.buffers.switch_current(buffer_id);
    }
}

fn point_char_pos(buf: &crate::buffer::Buffer, byte_pos: usize) -> i64 {
    buf.text.emacs_byte_to_char(byte_pos) as i64 + 1
}

pub(crate) fn normalize_narrow_region_in_buffers(
    buffers: &BufferManager,
    current_id: BufferId,
    start: i64,
    end: i64,
) -> Result<(usize, usize), Flow> {
    let buf = buffers
        .get(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let mut s = start;
    let mut e = end;
    if e < s {
        std::mem::swap(&mut s, &mut e);
    }
    let full_min = 1_i64;
    let full_max = buf.total_chars() as i64 + 1;
    if s < full_min || s > full_max || e < full_min || e > full_max {
        return Err(signal(
            "args-out-of-range",
            vec![Value::fixnum(start), Value::fixnum(end)],
        ));
    }
    if let Some((begv_char, zv_char)) = buffers.current_labeled_restriction_char_bounds(current_id)
    {
        let labeled_min = begv_char as i64 + 1;
        let labeled_max = zv_char as i64 + 1;
        s = s.clamp(labeled_min, labeled_max);
        e = e.clamp(labeled_min, labeled_max);
    }
    let start_char = if s > 0 { s as usize - 1 } else { 0 };
    let end_char = if e > 0 { e as usize - 1 } else { 0 };
    Ok((
        buf.text.char_to_emacs_byte(start_char),
        buf.text.char_to_emacs_byte(end_char),
    ))
}

pub(crate) fn expect_integer_or_marker_in_buffers(
    buffers: &BufferManager,
    value: &Value,
) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        _other if value.is_marker() => {
            crate::emacs_core::marker::marker_position_as_int_with_buffers(buffers, value)
        }
        _other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *value],
        )),
    }
}

fn canonicalize_or_self(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

pub(crate) fn run_buffer_list_update_hook(eval: &mut super::eval::Context) -> EvalResult {
    builtin_run_hooks(eval, vec![Value::symbol("buffer-list-update-hook")])
}

pub(crate) fn builtin_get_buffer_create(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("get-buffer-create", &args, 1)?;
    expect_max_args("get-buffer-create", &args, 2)?;
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::Buffer) => Ok(args[0]),
        _ => {
            let name = expect_string(&args[0])?;
            if let Some(id) = eval.buffers.find_buffer_by_name(&name) {
                Ok(Value::make_buffer(id))
            } else {
                let inhibit_buffer_hooks = args.get(1).is_some_and(|value| !value.is_nil());
                let id = eval
                    .buffers
                    .create_buffer_with_hook_inhibition(&name, inhibit_buffer_hooks);
                if !inhibit_buffer_hooks {
                    run_buffer_list_update_hook(eval)?;
                }
                Ok(Value::make_buffer(id))
            }
        }
    }
}

/// (make-indirect-buffer BASE-BUFFER NAME &optional CLONE INHIBIT-BUFFER-HOOKS) → buffer
pub(crate) fn builtin_make_indirect_buffer(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let plan = prepare_make_indirect_buffer_in_manager(&mut eval.buffers, args)?;
    finish_make_indirect_buffer_hooks(eval, plan)
}

pub(crate) fn prepare_make_indirect_buffer_in_manager(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> Result<MakeIndirectBufferPlan, Flow> {
    expect_range_args("make-indirect-buffer", &args, 2, 4)?;

    let base_id = match args[0].kind() {
        ValueKind::Veclike(VecLikeType::Buffer) => {
            let id = args[0].as_buffer_id().unwrap();
            if buffers.get(id).is_none() {
                return Err(signal(
                    "error",
                    vec![Value::string("Base buffer has been killed")],
                ));
            }
            id
        }
        ValueKind::String => {
            let name = expect_buffer_name_string(&args[0])?;
            buffers.find_buffer_by_name(&name).ok_or_else(|| {
                signal(
                    "error",
                    vec![Value::string(format!("No such buffer: `{name}`"))],
                )
            })?
        }
        _other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), args[0]],
            ));
        }
    };

    let name = expect_string(&args[1])?;
    if name.is_empty() {
        return Err(signal(
            "error",
            vec![Value::string("Empty string for buffer name is not allowed")],
        ));
    }
    if buffers.find_buffer_by_name(&name).is_some() {
        return Err(signal(
            "error",
            vec![Value::string(format!("Buffer name `{name}` is in use"))],
        ));
    }

    let clone = args.get(2).is_some_and(|value| !value.is_nil());
    let inhibit_buffer_hooks = args.get(3).is_some_and(|value| !value.is_nil());
    let id = buffers
        .create_indirect_buffer_with_hook_inhibition(base_id, &name, clone, inhibit_buffer_hooks)
        .ok_or_else(|| {
            signal(
                "error",
                vec![Value::string("Failed to create indirect buffer")],
            )
        })?;

    Ok(MakeIndirectBufferPlan {
        id,
        saved_current: buffers.current_buffer_id(),
        run_clone_hook: clone,
    })
}

pub(crate) fn finish_make_indirect_buffer_hooks(
    eval: &mut super::eval::Context,
    plan: MakeIndirectBufferPlan,
) -> EvalResult {
    if plan.run_clone_hook {
        eval.switch_current_buffer(plan.id)?;
        let clone_result =
            builtin_run_hooks(eval, vec![Value::symbol("clone-indirect-buffer-hook")]);
        if let Some(saved_id) = plan.saved_current {
            eval.restore_current_buffer_if_live(saved_id);
        }
        clone_result?;
    }
    if !eval.buffers.buffer_hooks_inhibited(plan.id) {
        run_buffer_list_update_hook(eval)?;
    }
    Ok(Value::make_buffer(plan.id))
}

pub(crate) fn builtin_get_buffer(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let buffers = &eval.buffers;
    expect_args("get-buffer", &args, 1)?;
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::Buffer) => Ok(args[0]),
        ValueKind::String => Ok(find_buffer_by_name_arg(buffers, &args[0])?
            .map(Value::make_buffer)
            .unwrap_or(Value::NIL)),
        _other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[0]],
        )),
    }
}

pub(crate) fn builtin_find_buffer(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let obarray = eval.obarray();
    let dynamic: &[OrderedRuntimeBindingMap] = &[];
    let buffers = &eval.buffers;
    expect_args("find-buffer", &args, 2)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let target_value = args[1];

    let name_id = intern(name);
    let fallback_value = dynamic
        .iter()
        .rev()
        .find_map(|frame| frame.get(&name_id).cloned())
        .or_else(|| obarray.symbol_value(name).cloned())
        .ok_or_else(|| signal("void-variable", vec![Value::symbol(name)]))?;

    let mut scan_order = Vec::new();
    let current_id = buffers.current_buffer().map(|buf| buf.id);
    if let Some(id) = current_id {
        scan_order.push(id);
    }
    for id in buffers.buffer_list() {
        if Some(id) != current_id {
            scan_order.push(id);
        }
    }

    let key = Value::from_sym_id(name_id);
    for id in scan_order {
        let Some(buf) = buffers.get(id) else {
            continue;
        };
        // Phase 10E: prefer the buffer's local_var_alist (LOCALIZED
        // per-buffer storage), then fall back to the legacy
        // get_buffer_local lookup (slot or lisp_bindings), then to
        // the global default. Mirrors GNU `find_buffer` (`buffer.c`)
        // walking the alist directly.
        let observed = buf
            .find_in_local_var_alist(key)
            .or_else(|| buf.get_buffer_local(name))
            .unwrap_or(fallback_value);
        if eq_value(&observed, &target_value) {
            return Ok(Value::make_buffer(id));
        }
    }

    Ok(Value::NIL)
}

pub(crate) fn builtin_delete_all_overlays(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let buffers = &mut eval.buffers;
    expect_max_args("delete-all-overlays", &args, 1)?;
    let target = if args.is_empty() || args[0].is_nil() {
        buffers.current_buffer().map(|buf| buf.id)
    } else {
        Some(expect_buffer_id(&args[0])?)
    };

    let Some(target_id) = target else {
        return Ok(Value::NIL);
    };
    if buffers.get(target_id).is_none() {
        // GNU Emacs treats dead buffers as a no-op.
        return Ok(Value::NIL);
    }
    let _ = buffers.delete_all_buffer_overlays(target_id);
    Ok(Value::NIL)
}

pub(crate) fn builtin_buffer_live_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let buffers = &eval.buffers;
    expect_args("buffer-live-p", &args, 1)?;
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::Buffer) => {
            let id = args[0].as_buffer_id().unwrap();
            Ok(Value::bool_val(buffers.get(id).is_some()))
        }
        _ => Ok(Value::NIL),
    }
}

pub(crate) fn builtin_get_file_buffer(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("get-file-buffer", &args, 1)?;
    let filename = expect_string(&args[0])?;
    let resolved =
        super::fileio::resolve_filename_in_state(&eval.obarray, &[], &eval.buffers, &filename);
    let resolved_true = canonicalize_or_self(&resolved);

    for id in eval.buffers.buffer_list() {
        let Some(buf) = eval.buffers.get(id) else {
            continue;
        };
        let Some(file_name) = buf.file_name_runtime_string_owned() else {
            continue;
        };

        let candidate =
            super::fileio::resolve_filename_in_state(&eval.obarray, &[], &eval.buffers, &file_name);
        if candidate == resolved {
            return Ok(Value::make_buffer(id));
        }
        if canonicalize_or_self(&candidate) == resolved_true {
            return Ok(Value::make_buffer(id));
        }
    }

    Ok(Value::NIL)
}

pub(crate) fn builtin_kill_buffer(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_max_args("kill-buffer", &args, 1)?;
    let id = match args.first() {
        None => match eval.buffers.current_buffer() {
            Some(buf) => buf.id,
            None => return Ok(Value::NIL),
        },
        Some(arg) => match arg.kind() {
            ValueKind::Nil => match eval.buffers.current_buffer() {
                Some(buf) => buf.id,
                None => return Ok(Value::NIL),
            },
            ValueKind::Veclike(VecLikeType::Buffer) => {
                let bid = arg.as_buffer_id().unwrap();
                if eval.buffers.get(bid).is_none() {
                    return Ok(Value::NIL);
                }
                bid
            }
            ValueKind::String => {
                let name = expect_buffer_name_string(arg)?;
                match eval.buffers.find_buffer_by_name(&name) {
                    Some(id) => id,
                    None => {
                        return Err(signal(
                            "error",
                            vec![Value::string(format!("No buffer named {name}"))],
                        ));
                    }
                }
            }
            _other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), *arg],
                ));
            }
        },
    };

    let saved_current = eval.buffers.current_buffer_id();
    let inhibit_buffer_hooks = eval.buffers.buffer_hooks_inhibited(id);
    let _ = eval.buffers.switch_current(id);
    let query_result = if inhibit_buffer_hooks {
        Value::T
    } else {
        let query_sym = crate::emacs_core::hook_runtime::hook_symbol_by_name(
            eval,
            "kill-buffer-query-functions",
        );
        let query_value = crate::emacs_core::hook_runtime::hook_value_by_id(eval, query_sym)
            .unwrap_or(Value::NIL);
        crate::emacs_core::hook_runtime::run_hook_value_until_failure(
            eval,
            query_sym,
            query_value,
            &[],
            true,
        )?
    };
    if let Some(buffer_id) = saved_current {
        eval.restore_current_buffer_if_live(buffer_id);
    }
    if query_result.is_nil() {
        return Ok(Value::NIL);
    }
    if eval.buffers.get(id).is_none() {
        return Ok(Value::T);
    }

    let _ = eval.buffers.switch_current(id);
    if !inhibit_buffer_hooks {
        let hook_sym =
            crate::emacs_core::hook_runtime::hook_symbol_by_name(eval, "kill-buffer-hook");
        let hook_value =
            crate::emacs_core::hook_runtime::hook_value_by_id(eval, hook_sym).unwrap_or(Value::NIL);
        crate::emacs_core::hook_runtime::run_hook_value(eval, hook_sym, hook_value, &[], true)?;
    }
    if let Some(buffer_id) = saved_current {
        eval.restore_current_buffer_if_live(buffer_id);
    }
    if eval.buffers.get(id).is_none() {
        return Ok(Value::T);
    }

    if eval
        .visible_variable_value_or_nil("kill-buffer-quit-windows")
        .is_truthy()
        && delete_quit_restore_popup_windows_showing_buffer(&mut eval.frames, id)
    {
        sync_current_buffer_to_selected_window(eval);
    }

    let current_before = eval.buffers.current_buffer().map(|buf| buf.id);
    let killed_ids = eval
        .buffers
        .collect_killed_buffer_ids(id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;
    let killed_set = killed_ids
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    let current_will_die = current_before.is_some_and(|current| killed_set.contains(&current));
    let replacement = if current_will_die {
        let other = other_buffer_impl(
            &mut eval.buffers,
            vec![Value::make_buffer(current_before.expect("current buffer"))],
        )?;
        match other.as_buffer_id() {
            Some(next) if next != id => Some(next),
            _ => None,
        }
    } else {
        None
    };

    let killed_ids = eval
        .buffers
        .kill_buffer_collect(id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    // Ensure dead-buffer windows continue to point at a live fallback buffer.
    let scratch = if let Some(scratch) = eval.buffers.find_buffer_by_name("*scratch*") {
        scratch
    } else {
        eval.buffers.create_buffer("*scratch*")
    };
    for killed_id in &killed_ids {
        eval.frames.replace_buffer_in_windows(*killed_id, scratch);
    }

    if current_will_die {
        if let Some(next) = replacement {
            if eval.buffers.get(next).is_some() {
                eval.buffers.switch_current(next);
            }
        }
        if eval.buffers.current_buffer().is_none() {
            if let Some(next) = eval.buffers.buffer_list().into_iter().next() {
                eval.buffers.switch_current(next);
            } else {
                eval.buffers.switch_current(scratch);
            }
        }
    }

    if !inhibit_buffer_hooks {
        run_buffer_list_update_hook(eval)?;
    }

    Ok(Value::T)
}

pub(crate) fn builtin_set_buffer(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("set-buffer", &args, 1)?;
    let id = match args[0].kind() {
        ValueKind::Veclike(VecLikeType::Buffer) => {
            let bid = args[0].as_buffer_id().unwrap();
            if eval.buffers.get(bid).is_none() {
                return Err(signal(
                    "error",
                    vec![Value::string("Selecting deleted buffer")],
                ));
            }
            bid
        }
        ValueKind::String => {
            let s = expect_buffer_name_string(&args[0])?;
            eval.buffers.find_buffer_by_name(&s).ok_or_else(|| {
                signal("error", vec![Value::string(format!("No buffer named {s}"))])
            })?
        }
        _other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), args[0]],
            ));
        }
    };
    eval.switch_current_buffer(id)?;
    Ok(Value::make_buffer(id))
}

pub(crate) fn builtin_current_buffer(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let buffers = &eval.buffers;
    expect_args("current-buffer", &args, 0)?;
    match buffers.current_buffer() {
        Some(buf) => Ok(Value::make_buffer(buf.id)),
        None => Ok(Value::NIL),
    }
}

pub(crate) fn builtin_buffer_name(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let buffers = &eval.buffers;
    expect_max_args("buffer-name", &args, 1)?;
    let id = if args.is_empty() || args[0].is_nil() {
        match buffers.current_buffer() {
            Some(b) => b.id,
            None => return Ok(Value::NIL),
        }
    } else {
        expect_buffer_id(&args[0])?
    };
    match buffers.get(id) {
        Some(buf) => Ok(buf.name_value()),
        None => Ok(Value::NIL),
    }
}

pub(crate) fn builtin_buffer_file_name(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let buffers = &eval.buffers;
    expect_max_args("buffer-file-name", &args, 1)?;
    let id = if args.is_empty() || args[0].is_nil() {
        match buffers.current_buffer() {
            Some(b) => b.id,
            None => return Ok(Value::NIL),
        }
    } else {
        expect_buffer_id(&args[0])?
    };
    Ok(buffers
        .get(id)
        .and_then(|buf| buf.buffer_local_value("buffer-file-name"))
        .unwrap_or(Value::NIL))
}

pub(crate) fn builtin_buffer_base_buffer(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let buffers = &eval.buffers;
    expect_max_args("buffer-base-buffer", &args, 1)?;
    let target = if args.is_empty() || args[0].is_nil() {
        match buffers.current_buffer() {
            Some(buf) => buf.id,
            None => return Ok(Value::NIL),
        }
    } else {
        expect_buffer_id(&args[0])?
    };

    Ok(buffers
        .get(target)
        .and_then(|buf| buf.base_buffer)
        .map(Value::make_buffer)
        .unwrap_or(Value::NIL))
}

pub(crate) fn builtin_buffer_last_name(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let buffers = &eval.buffers;
    expect_max_args("buffer-last-name", &args, 1)?;
    let target = if args.is_empty() || args[0].is_nil() {
        match buffers.current_buffer() {
            Some(buf) => buf.id,
            None => return Ok(Value::NIL),
        }
    } else {
        expect_buffer_id(&args[0])?
    };

    if let Some(buf) = buffers.get(target) {
        if buf.has_name("*scratch*") {
            return Ok(Value::NIL);
        }
        return Ok(buf.name_value());
    }
    if let Some(name) = buffers.dead_buffer_last_name_value(target) {
        return Ok(name);
    }
    Ok(Value::NIL)
}

/// (buffer-substring START END) → string
pub(crate) fn builtin_buffer_substring(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("buffer-substring", &args, 2)?;
    let start = expect_integer_or_marker_in_buffers(&eval.buffers, &args[0])?;
    let end = expect_integer_or_marker_in_buffers(&eval.buffers, &args[1])?;
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = eval
        .buffers
        .get(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let point_min = buf.point_min_char() as i64 + 1;
    let point_max = buf.point_max_char() as i64 + 1;
    if start < point_min || start > point_max || end < point_min || end > point_max {
        return Err(signal(
            "args-out-of-range",
            vec![Value::make_buffer(buf.id), args[0], args[1]],
        ));
    }
    let start = start as usize;
    let end = end as usize;
    let byte_start = buf.lisp_pos_to_accessible_byte(start as i64);
    let byte_end = buf.lisp_pos_to_accessible_byte(end as i64);
    let (byte_lo, byte_hi) = if byte_start <= byte_end {
        (byte_start, byte_end)
    } else {
        (byte_end, byte_start)
    };
    Ok(buffer_slice_value(buf, byte_lo, byte_hi))
}

pub(crate) fn builtin_buffer_string(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("buffer-string", &args, 0)?;
    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let byte_start = buf.point_min();
    let byte_end = buf.point_max();
    Ok(buffer_slice_value(buf, byte_start, byte_end))
}

fn resolve_buffer_designator_allow_nil_current(
    eval: &mut super::eval::Context,
    arg: &Value,
) -> Result<Option<BufferId>, Flow> {
    match arg.kind() {
        ValueKind::Nil => eval
            .buffers
            .current_buffer()
            .map(|buf| Some(buf.id))
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")])),
        ValueKind::Veclike(VecLikeType::Buffer) => {
            let id = arg.as_buffer_id().unwrap();
            if eval.buffers.get(id).is_some() {
                Ok(Some(id))
            } else {
                Err(signal(
                    "error",
                    vec![Value::string("Selecting deleted buffer")],
                ))
            }
        }
        ValueKind::String => {
            let name = expect_buffer_name_string(arg)?;
            eval.buffers
                .find_buffer_by_name(&name)
                .map(Some)
                .ok_or_else(|| {
                    signal(
                        "error",
                        vec![Value::string(format!("No buffer named {name}"))],
                    )
                })
        }
        _other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *arg],
        )),
    }
}

fn buffer_slice_for_char_region(
    eval: &super::eval::Context,
    buffer_id: Option<BufferId>,
    start: i64,
    end: i64,
) -> String {
    let Some(buffer_id) = buffer_id else {
        return String::new();
    };
    let Some(buf) = eval.buffers.get(buffer_id) else {
        return String::new();
    };

    let (from, to) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    let from_char = if from > 0 { from as usize - 1 } else { 0 };
    let to_char = if to > 0 { to as usize - 1 } else { 0 };
    let char_count = buf.total_chars();
    let from_byte = buf.text.char_to_emacs_byte(from_char.min(char_count));
    let to_byte = buf.text.char_to_emacs_byte(to_char.min(char_count));
    super::runtime_string_from_lisp_string(&buf.buffer_substring_lisp_string(from_byte, to_byte))
}

fn checked_buffer_slice_for_char_region(
    eval: &super::eval::Context,
    buffer_id: Option<BufferId>,
    start: i64,
    end: i64,
    start_arg: Value,
    end_arg: Value,
) -> Result<String, Flow> {
    let Some(buffer_id) = buffer_id else {
        return Ok(String::new());
    };
    let Some(buf) = eval.buffers.get(buffer_id) else {
        return Ok(String::new());
    };

    let point_min = buf.point_min_char() as i64 + 1;
    let point_max = buf.point_max_char() as i64 + 1;
    if start < point_min || start > point_max || end < point_min || end > point_max {
        return Err(signal("args-out-of-range", vec![start_arg, end_arg]));
    }

    let (from, to) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    let from_byte = buf.lisp_pos_to_accessible_byte(from);
    let to_byte = buf.lisp_pos_to_accessible_byte(to);
    Ok(super::runtime_string_from_lisp_string(
        &buf.buffer_substring_lisp_string(from_byte, to_byte),
    ))
}

pub(crate) fn resolve_buffer_designator_allow_nil_current_in_manager(
    buffers: &BufferManager,
    arg: &Value,
) -> Result<Option<BufferId>, Flow> {
    match arg.kind() {
        ValueKind::Nil => buffers
            .current_buffer()
            .map(|buf| Some(buf.id))
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")])),
        ValueKind::Veclike(VecLikeType::Buffer) => {
            let id = arg.as_buffer_id().unwrap();
            if buffers.get(id).is_some() {
                Ok(Some(id))
            } else {
                Err(signal(
                    "error",
                    vec![Value::string("Selecting deleted buffer")],
                ))
            }
        }
        ValueKind::String => {
            let name = expect_buffer_name_string(arg)?;
            buffers.find_buffer_by_name(&name).map(Some).ok_or_else(|| {
                signal(
                    "error",
                    vec![Value::string(format!("No buffer named {name}"))],
                )
            })
        }
        _other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *arg],
        )),
    }
}

fn checked_buffer_slice_for_char_region_in_manager(
    buffers: &BufferManager,
    buffer_id: Option<BufferId>,
    start: i64,
    end: i64,
    start_arg: Value,
    end_arg: Value,
) -> Result<String, Flow> {
    let Some(buffer_id) = buffer_id else {
        return Ok(String::new());
    };
    let Some(buf) = buffers.get(buffer_id) else {
        return Ok(String::new());
    };

    let point_min = buf.point_min_char() as i64 + 1;
    let point_max = buf.point_max_char() as i64 + 1;
    if start < point_min || start > point_max || end < point_min || end > point_max {
        return Err(signal("args-out-of-range", vec![start_arg, end_arg]));
    }

    let (from, to) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    let from_byte = buf.lisp_pos_to_accessible_byte(from);
    let to_byte = buf.lisp_pos_to_accessible_byte(to);
    Ok(super::runtime_string_from_lisp_string(
        &buf.buffer_substring_lisp_string(from_byte, to_byte),
    ))
}

fn checked_buffer_substring_for_char_region_in_manager(
    buffers: &BufferManager,
    buffer_id: Option<BufferId>,
    start: i64,
    end: i64,
    start_arg: Value,
    end_arg: Value,
) -> Result<Value, Flow> {
    let Some(buffer_id) = buffer_id else {
        return Ok(Value::heap_string(
            crate::heap_types::LispString::from_emacs_bytes(Vec::new()),
        ));
    };
    let Some(buf) = buffers.get(buffer_id) else {
        return Ok(Value::heap_string(
            crate::heap_types::LispString::from_emacs_bytes(Vec::new()),
        ));
    };

    let point_min = buf.point_min_char() as i64 + 1;
    let point_max = buf.point_max_char() as i64 + 1;
    if start < point_min || start > point_max || end < point_min || end > point_max {
        return Err(signal("args-out-of-range", vec![start_arg, end_arg]));
    }

    let (from, to) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    let from_byte = buf.lisp_pos_to_accessible_byte(from);
    let to_byte = buf.lisp_pos_to_accessible_byte(to);
    Ok(buffer_slice_value(buf, from_byte, to_byte))
}

fn compare_buffer_substring_strings(left: &str, right: &str, case_fold: bool) -> i64 {
    let mut pos = 1i64;
    let mut left_iter = left.chars();
    let mut right_iter = right.chars();

    loop {
        match (left_iter.next(), right_iter.next()) {
            (Some(a), Some(b)) => {
                let a = if case_fold {
                    a.to_lowercase().next().unwrap_or(a)
                } else {
                    a
                };
                let b = if case_fold {
                    b.to_lowercase().next().unwrap_or(b)
                } else {
                    b
                };
                if a != b {
                    return if a < b { -pos } else { pos };
                }
                pos += 1;
            }
            (Some(_), None) => return pos,
            (None, Some(_)) => return -pos,
            (None, None) => return 0,
        }
    }
}

pub(crate) fn builtin_buffer_line_statistics(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let buffers = &eval.buffers;
    expect_max_args("buffer-line-statistics", &args, 1)?;
    let buffer_id = if args.is_empty() {
        resolve_buffer_designator_allow_nil_current_in_manager(buffers, &Value::NIL)?
    } else {
        resolve_buffer_designator_allow_nil_current_in_manager(buffers, &args[0])?
    };

    let text = buffer_id
        .and_then(|id| {
            buffers.get(id).map(|buf| {
                super::runtime_string_from_lisp_string(
                    &buf.buffer_substring_lisp_string(buf.point_min(), buf.point_max()),
                )
            })
        })
        .unwrap_or_default();

    if text.is_empty() {
        return Ok(Value::list(vec![
            Value::fixnum(0),
            Value::fixnum(0),
            Value::make_float(0.0),
        ]));
    }

    let mut line_count = 0usize;
    let mut max_len = 0usize;
    let mut total_len = 0usize;
    for line in text.lines() {
        line_count += 1;
        let width = line.len();
        max_len = max_len.max(width);
        total_len += width;
    }

    if line_count == 0 {
        return Ok(Value::list(vec![
            Value::fixnum(0),
            Value::fixnum(0),
            Value::make_float(0.0),
        ]));
    }

    Ok(Value::list(vec![
        Value::fixnum(line_count as i64),
        Value::fixnum(max_len as i64),
        Value::make_float(total_len as f64 / line_count as f64),
    ]))
}

fn replace_region_contents_type_predicate() -> Value {
    Value::list(vec![
        Value::symbol("or"),
        Value::symbol("stringp"),
        Value::symbol("bufferp"),
        Value::symbol("vectorp"),
    ])
}

fn replace_region_source_value_in_state(
    buffers: &BufferManager,
    source: &Value,
    current_id: BufferId,
) -> Result<Value, Flow> {
    match source.kind() {
        ValueKind::String => Ok(*source),
        ValueKind::Veclike(VecLikeType::Buffer) => {
            let id = source.as_buffer_id().unwrap();
            if id == current_id {
                return Err(signal(
                    "error",
                    vec![Value::string("Cannot replace a buffer with itself")],
                ));
            }
            let Some(buf) = buffers.get(id) else {
                return Err(signal(
                    "error",
                    vec![Value::string("Selecting deleted buffer")],
                ));
            };
            checked_buffer_substring_for_char_region_in_manager(
                buffers,
                Some(id),
                buf.point_min_char() as i64 + 1,
                buf.point_max_char() as i64 + 1,
                Value::fixnum(buf.point_min_char() as i64 + 1),
                Value::fixnum(buf.point_max_char() as i64 + 1),
            )
        }
        ValueKind::Veclike(VecLikeType::Vector) => {
            let items = source.as_vector_data().unwrap().clone();
            if items.len() != 3 {
                return Err(signal(
                    "wrong-type-argument",
                    vec![replace_region_contents_type_predicate(), *source],
                ));
            }
            let buffer_id = expect_buffer_id(&items[0])?;
            if buffer_id == current_id {
                return Err(signal(
                    "error",
                    vec![Value::string("Cannot replace a buffer with itself")],
                ));
            }
            let start = expect_integer_or_marker_in_buffers(buffers, &items[1])?;
            let end = expect_integer_or_marker_in_buffers(buffers, &items[2])?;
            checked_buffer_substring_for_char_region_in_manager(
                buffers,
                Some(buffer_id),
                start,
                end,
                items[1],
                items[2],
            )
        }
        _other => Err(signal(
            "wrong-type-argument",
            vec![replace_region_contents_type_predicate(), *source],
        )),
    }
}

pub(crate) fn builtin_buffer_swap_text(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let buffers = &mut eval.buffers;
    expect_args("buffer-swap-text", &args, 1)?;
    let other_id = expect_buffer_id(&args[0])?;
    if buffers.get(other_id).is_none() {
        return Ok(Value::NIL);
    }

    let current_id = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?
        .id;

    if current_id == other_id {
        return Ok(Value::NIL);
    }

    let (current_text, current_multibyte) = buffers
        .get(current_id)
        .map(|buf| {
            (
                buf.buffer_substring_lisp_string(buf.point_min(), buf.point_max()),
                buf.get_multibyte(),
            )
        })
        .unwrap_or_else(|| (lisp_string_from_buffer_bytes(Vec::new(), true), true));
    let (other_text, other_multibyte) = buffers
        .get(other_id)
        .map(|buf| {
            (
                buf.buffer_substring_lisp_string(buf.point_min(), buf.point_max()),
                buf.get_multibyte(),
            )
        })
        .unwrap_or_else(|| (lisp_string_from_buffer_bytes(Vec::new(), true), true));

    let current_replacement =
        buffer_insert_lisp_string_from_lisp_string(&other_text, current_multibyte);
    let other_replacement =
        buffer_insert_lisp_string_from_lisp_string(&current_text, other_multibyte);

    let _ = buffers.replace_buffer_contents_lisp_string(current_id, &current_replacement);
    let _ = buffers.replace_buffer_contents_lisp_string(other_id, &other_replacement);

    Ok(Value::NIL)
}

pub(crate) fn builtin_insert_buffer_substring(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("insert-buffer-substring", &args, 1, 3)?;
    let buffer_id =
        resolve_buffer_designator_allow_nil_current_in_manager(&mut eval.buffers, &args[0])?;
    let (default_start, default_end) = buffer_id
        .and_then(|id| {
            eval.buffers.get(id).map(|buf| {
                (
                    buf.point_min_char() as i64 + 1,
                    buf.point_max_char() as i64 + 1,
                )
            })
        })
        .unwrap_or((1, 1));
    let start = if args.len() > 1 && !args[1].is_nil() {
        expect_integer_or_marker_in_buffers(&mut eval.buffers, &args[1])?
    } else {
        default_start
    };
    let end = if args.len() > 2 && !args[2].is_nil() {
        expect_integer_or_marker_in_buffers(&mut eval.buffers, &args[2])?
    } else {
        default_end
    };

    let text = checked_buffer_substring_for_char_region_in_manager(
        &mut eval.buffers,
        buffer_id,
        start,
        end,
        Value::fixnum(start),
        Value::fixnum(end),
    )?;
    builtin_insert(eval, vec![text])
}

pub(crate) fn builtin_kill_all_local_variables(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("kill-all-local-variables", &args, 0, 1)?;
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let kill_permanent = args.first().copied().unwrap_or(Value::NIL).is_truthy();

    // GNU buffer.c reset_buffer_local_variables:
    // - preserves most always-local slots
    // - resets only a small fixed reset-on-kill-all subset
    // - clears conditional slot locals unless they are permanent-local
    // - walks local_var_alist for LOCALIZED entries (Phase 10E)
    let _ =
        eval.buffers
            .clear_buffer_local_properties(current_id, &mut eval.obarray, kill_permanent);
    Ok(Value::NIL)
}

/// `(ntake N LIST)` -> LIST
pub(crate) fn builtin_ntake(args: Vec<Value>) -> EvalResult {
    expect_args("ntake", &args, 2)?;
    let n = expect_int(&args[0])?;
    if n <= 0 {
        return Ok(Value::NIL);
    }

    let head = args[1];
    if head.is_nil() {
        return Ok(Value::NIL);
    }
    if !head.is_cons() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), head],
        ));
    }

    let mut cursor = head;
    for _ in 1..n {
        match cursor.kind() {
            ValueKind::Cons => {
                let next = cursor.cons_cdr();
                match next.kind() {
                    ValueKind::Cons => cursor = next,
                    ValueKind::Nil => return Ok(head),
                    _other => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("listp"), next],
                        ));
                    }
                }
            }
            ValueKind::Nil => return Ok(head),
            _other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), cursor],
                ));
            }
        }
    }

    match cursor.kind() {
        ValueKind::Cons => {
            cursor.set_cdr(Value::NIL);
            Ok(head)
        }
        ValueKind::Nil => Ok(head),
        _other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), cursor],
        )),
    }
}

/// `(replace-buffer-contents SOURCE &optional MAX-SECS MAX-COSTS)` -> t
pub(crate) fn builtin_replace_buffer_contents(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("replace-buffer-contents", &args, 1, 3)?;
    let source_id = resolve_buffer_designator_allow_nil_current(eval, &args[0])?;

    let read_only_buffer_name = eval.buffers.current_buffer().and_then(|buf| {
        if buffer_read_only_active(eval, buf) {
            Some(buf.name_value())
        } else {
            None
        }
    });
    if let Some(name) = read_only_buffer_name {
        return Err(signal("buffer-read-only", vec![name]));
    }

    let current_id = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?
        .id;
    let target_multibyte = eval
        .buffers
        .get(current_id)
        .map(|buf| buf.get_multibyte())
        .unwrap_or(true);
    let source_text = source_id
        .and_then(|id| {
            eval.buffers
                .get(id)
                .map(|buf| buf.buffer_substring_lisp_string(buf.point_min(), buf.point_max()))
        })
        .map(|text| buffer_insert_lisp_string_from_lisp_string(&text, target_multibyte))
        .unwrap_or_else(|| lisp_string_from_buffer_bytes(Vec::new(), target_multibyte));

    let old_len_bytes = eval
        .buffers
        .get(current_id)
        .map(|buf| buf.text.len())
        .unwrap_or(0);
    let old_len = super::editfns::current_buffer_byte_span_char_len(eval, 0, old_len_bytes);
    super::editfns::signal_before_change(eval, 0, old_len_bytes)?;
    let _ = eval
        .buffers
        .replace_buffer_contents_lisp_string(current_id, &source_text);
    let new_len = eval
        .buffers
        .get(current_id)
        .map(|buf| buf.text.len())
        .unwrap_or(0);
    super::editfns::signal_after_change(eval, 0, new_len, old_len)?;

    Ok(Value::T)
}

pub(crate) fn builtin_replace_region_contents(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("replace-region-contents", &args, 3, 6)?;
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let start = expect_integer_or_marker_in_buffers(&mut eval.buffers, &args[0])?;
    let end = expect_integer_or_marker_in_buffers(&mut eval.buffers, &args[1])?;
    let source_value =
        replace_region_source_value_in_state(&mut eval.buffers, &args[2], current_id)?;

    let read_only_buffer_name = eval.buffers.current_buffer().and_then(|buf| {
        if super::editfns::buffer_read_only_active_in_state(&eval.obarray, &[], buf) {
            Some(buf.name_value())
        } else {
            None
        }
    });
    if let Some(name) = read_only_buffer_name {
        return Err(signal("buffer-read-only", vec![name]));
    }

    let (lo, hi) = {
        let buf = eval
            .buffers
            .get(current_id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        let start_byte = super::editfns::lisp_pos_to_byte(buf, start);
        let end_byte = super::editfns::lisp_pos_to_byte(buf, end);
        if start_byte <= end_byte {
            (start_byte, end_byte)
        } else {
            (end_byte, start_byte)
        }
    };
    // Signal before the combined delete+insert operation.
    super::editfns::signal_before_change(eval, lo, hi)?;
    let old_len = super::editfns::current_buffer_byte_span_char_len(eval, lo, hi);
    let _ = eval.buffers.delete_buffer_region(current_id, lo, hi);
    let _ = eval.buffers.goto_buffer_byte(current_id, lo);
    // The insert builtins already call signal hooks internally, but the
    // surrounding before/after pair covers the whole replace operation.
    // To avoid double-firing, we use insert_pieces_in_state directly.
    let target_multibyte = current_buffer_multibyte(&eval.buffers)?;
    let source_pieces = collect_insert_pieces(&[source_value], target_multibyte)?;
    let new_len: usize = source_pieces.iter().map(|p| p.text.sbytes()).sum();
    let inherit = args.get(5).is_some_and(|value| value.is_truthy());
    insert_pieces_in_state(
        &eval.obarray,
        &[],
        &mut eval.buffers,
        source_pieces,
        false,
        inherit,
    )?;
    super::editfns::signal_after_change(eval, lo, lo + new_len, old_len)?;

    Ok(Value::T)
}

pub(crate) fn builtin_set_buffer_multibyte(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-buffer-multibyte", &args, 1)?;
    let flag = args[0];
    let target_multibyte = !flag.is_nil();
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    let (already_multibyte, narrowed, base_buffer, shared_ids) = {
        let current = eval
            .buffers
            .get(current_id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        (
            current.get_multibyte(),
            current.begv_byte > 0 || current.zv_byte < current.text.len(),
            current.base_buffer,
            eval.buffers.shared_text_buffer_ids(current_id),
        )
    };
    let old_undo_list = eval
        .buffers
        .get(current_id)
        .map(|buffer| buffer.get_undo_list())
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    if base_buffer.is_some() {
        return Err(signal(
            "error",
            vec![Value::string(
                "Cannot do `set-buffer-multibyte' on an indirect buffer",
            )],
        ));
    }

    if already_multibyte == target_multibyte {
        return Ok(flag);
    }

    if narrowed {
        return Err(signal(
            "error",
            vec![Value::string("Changing multibyteness in a narrowed buffer")],
        ));
    }

    #[derive(Clone, Copy)]
    struct OverlaySnapshot {
        overlay: Value,
        start_byte: usize,
        end_byte: usize,
    }

    struct BufferSnapshot {
        id: BufferId,
        pt_byte: usize,
        begv_byte: usize,
        zv_byte: usize,
        mark_byte: Option<usize>,
        last_window_start_byte: usize,
        overlays: Vec<OverlaySnapshot>,
    }

    let snapshots = {
        let mut snapshots = Vec::with_capacity(shared_ids.len());
        for id in &shared_ids {
            let buffer = eval
                .buffers
                .get(*id)
                .ok_or_else(|| signal("error", vec![Value::string("Missing shared buffer")]))?;
            let overlays = buffer
                .overlays
                .dump_overlays()
                .into_iter()
                .filter_map(|overlay| {
                    let data = overlay.as_overlay_data()?;
                    Some(OverlaySnapshot {
                        overlay,
                        start_byte: buffer.text.storage_byte_to_emacs_byte(data.start),
                        end_byte: buffer.text.storage_byte_to_emacs_byte(data.end),
                    })
                })
                .collect();
            snapshots.push(BufferSnapshot {
                id: *id,
                pt_byte: buffer
                    .text
                    .storage_byte_to_emacs_byte(buffer.pt_byte.min(buffer.text.len())),
                begv_byte: buffer
                    .text
                    .storage_byte_to_emacs_byte(buffer.begv_byte.min(buffer.text.len())),
                zv_byte: buffer
                    .text
                    .storage_byte_to_emacs_byte(buffer.zv_byte.min(buffer.text.len())),
                mark_byte: buffer.mark_byte.map(|mark| {
                    buffer
                        .text
                        .storage_byte_to_emacs_byte(mark.min(buffer.text.len()))
                }),
                last_window_start_byte: buffer
                    .text
                    .storage_byte_to_emacs_byte(buffer.last_window_start.min(buffer.text.len())),
                overlays,
            });
        }
        snapshots
    };

    let source_value = {
        let buffer = eval
            .buffers
            .get(current_id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        buffer_slice_value(buffer, 0, buffer.total_bytes())
    };
    let (converted_value, mode) = convert_buffer_string_for_multibyte(source_value, flag)?;
    let piece = buffer_insert_piece_from_string(converted_value, target_multibyte)?;
    let new_storage = piece.text;
    let new_total_bytes = new_storage.sbytes();

    let shared_text = {
        let buffer = eval
            .buffers
            .get(current_id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        buffer.text.shared_clone()
    };
    let old_props = shared_text.text_props_snapshot();

    let new_props = match mode {
        BufferMultibyteConversionMode::ToMultibyte => old_props,
        BufferMultibyteConversionMode::AsUnibyte | BufferMultibyteConversionMode::AsMultibyte => {
            remap_text_property_table(&old_props, |char_pos| {
                let byte_pos = shared_text.char_to_byte(char_pos);
                let logical_byte = shared_text.storage_byte_to_emacs_byte(byte_pos);
                let boundary = lisp_string_advance_byte_to_boundary(
                    &new_storage,
                    logical_byte.min(new_total_bytes),
                );
                lisp_string_byte_to_char(&new_storage, boundary)
            })
        }
    };

    // T7: the Vec<MarkerEntry> parallel bookkeeping is gone. Walk the
    // intrusive chain and remap each marker's (bytepos, charpos) through
    // the same boundary arithmetic that the old snapshot+replace code
    // applied to its Vec copy.
    shared_text.remap_markers_through(|old_byte| {
        let logical_byte = shared_text.storage_byte_to_emacs_byte(old_byte);
        let boundary =
            lisp_string_advance_byte_to_boundary(&new_storage, logical_byte.min(new_total_bytes));
        let new_char = lisp_string_byte_to_char(&new_storage, boundary);
        (boundary, new_char)
    });
    shared_text.replace_lisp_string(&new_storage, new_props);

    for snapshot in snapshots {
        let buf = eval
            .buffers
            .get_mut(snapshot.id)
            .ok_or_else(|| signal("error", vec![Value::string("Missing shared buffer")]))?;

        let map_boundary = |logical_byte: usize| {
            lisp_string_advance_byte_to_boundary(&new_storage, logical_byte.min(new_total_bytes))
        };

        let pt_byte = map_boundary(snapshot.pt_byte);
        let begv_byte = map_boundary(snapshot.begv_byte);
        let zv_byte = map_boundary(snapshot.zv_byte);
        let mark_byte = snapshot.mark_byte.map(map_boundary);
        let last_window_start_byte = map_boundary(snapshot.last_window_start_byte);

        buf.pt = lisp_string_byte_to_char(&new_storage, pt_byte);
        buf.pt_byte = pt_byte;

        buf.begv = lisp_string_byte_to_char(&new_storage, begv_byte);
        buf.begv_byte = begv_byte;

        buf.zv = lisp_string_byte_to_char(&new_storage, zv_byte);
        buf.zv_byte = zv_byte;

        if let Some(mark_byte) = mark_byte {
            buf.mark = Some(lisp_string_byte_to_char(&new_storage, mark_byte));
            buf.mark_byte = Some(mark_byte);
        } else {
            buf.mark = None;
            buf.mark_byte = None;
        }

        buf.last_window_start = last_window_start_byte;

        for overlay in snapshot.overlays {
            let start_byte = map_boundary(overlay.start_byte);
            let end_byte = map_boundary(overlay.end_byte);
            buf.overlays
                .move_overlay(overlay.overlay, start_byte, end_byte);
        }

        buf.set_multibyte_value(target_multibyte);
        buf.set_buffer_local(
            "enable-multibyte-characters",
            if target_multibyte {
                Value::T
            } else {
                Value::NIL
            },
        );
    }

    if !old_undo_list.is_t() {
        let restore_flag = if flag.is_nil() { Value::T } else { Value::NIL };
        let undo_entry = Value::list(vec![
            Value::symbol("apply"),
            Value::symbol("set-buffer-multibyte"),
            restore_flag,
        ]);
        let _ = eval
            .buffers
            .configure_buffer_undo_list(current_id, Value::cons(undo_entry, old_undo_list));
    }
    Ok(flag)
}

/// `(split-window-internal OLD PIXEL-SIZE SIDE NORMAL-SIZE &optional REFER)`
///
/// GNU `src/window.c::Fsplit_window_internal` honors all five
/// arguments. The fourth argument NORMAL-SIZE seeds the new
/// window's `normal_lines`/`normal_cols` slot so future
/// proportional resizes preserve the requested ratio. The fifth
/// argument REFER lets `set-window-configuration` revive a
/// previously-deleted window by id, restoring its parameters,
/// dedication, and history alists.
///
/// Window audit Critical 5 in `drafts/window-system-audit.md`:
/// neomacs accepts both arguments for arity compatibility but
/// drops them on the floor. NORMAL-SIZE is observable as soon as
/// audit Critical 7 lands the per-window normal-size fields; REFER
/// is observable when window.el's `display-buffer` falls back to
/// reviving a deleted window inside `set-window-configuration`.
///
/// Both fixes are deferred until the structural prereqs land
/// (per-window normal_lines/cols storage and a deleted-window
/// revival registry).
pub(crate) fn builtin_split_window_internal(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("split-window-internal", &args, 4, 5)?;
    if !args[1].is_nil() {
        let _ = expect_fixnum(&args[1])?;
    }
    if !args[2].is_nil() && !args[2].is_symbol() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[2]],
        ));
    }

    // NORMAL-SIZE and REFER are accepted for arity compatibility
    // and ignored — see the docstring above and window audit
    // Critical 5 in drafts/window-system-audit.md.
    let _ = &args[3];
    if let Some(refer) = args.get(4) {
        let _ = refer;
    }
    let result = super::window_cmds::split_window_internal_impl_in_state(
        &mut eval.frames,
        &mut eval.buffers,
        args[0],
        args[1],
        args[2],
    )?;
    // Run window-configuration-change-hook after successful split.
    let _ = super::hooks::builtin_run_window_configuration_change_hook(eval, vec![]);
    Ok(result)
}

pub(crate) fn builtin_buffer_text_pixel_size(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let buffers = &eval.buffers;
    expect_range_args("buffer-text-pixel-size", &args, 0, 4)?;

    let buffer_id = if args.is_empty() {
        resolve_buffer_designator_allow_nil_current_in_manager(buffers, &Value::NIL)?
    } else {
        resolve_buffer_designator_allow_nil_current_in_manager(buffers, &args[0])?
    };

    if args.len() > 1 {
        let window = &args[1];
        if !window.is_nil() && !window.is_window() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("window-live-p"), *window],
            ));
        }
    }

    let limit_from_value = |value: &Value| -> Result<Option<usize>, Flow> {
        match value.kind() {
            ValueKind::Nil | ValueKind::T => Ok(None),
            ValueKind::Fixnum(n) if n >= 0 => Ok(Some(n as usize)),
            _ => Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("natnump"), *value],
            )),
        }
    };

    let x_limit = if args.len() > 2 {
        limit_from_value(&args[2])?
    } else {
        None
    };
    let y_limit = if args.len() > 3 {
        limit_from_value(&args[3])?
    } else {
        None
    };

    let text = if let Some(id) = buffer_id {
        if let Some(buf) = buffers.get(id) {
            super::runtime_string_from_lisp_string(
                &buf.buffer_substring_lisp_string(buf.point_min(), buf.point_max()),
            )
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    if text.is_empty() {
        return Ok(Value::cons(Value::fixnum(0), Value::fixnum(0)));
    }

    let mut height = 0usize;
    let mut width = 0usize;
    for line in text.lines() {
        if y_limit.is_some_and(|limit| height >= limit) {
            break;
        }

        let mut line_width = 0usize;
        for ch in line.chars() {
            if ch == '\t' {
                let tab_width = 8usize;
                line_width += tab_width - (line_width % tab_width);
            } else {
                line_width += crate::encoding::char_width(ch);
            }

            if let Some(limit) = x_limit {
                if line_width >= limit {
                    line_width = limit;
                    break;
                }
            }
        }

        height += 1;
        width = width.max(line_width);
    }

    if height == 0 {
        return Ok(Value::cons(Value::fixnum(0), Value::fixnum(0)));
    }
    Ok(Value::cons(
        Value::fixnum(width as i64),
        Value::fixnum(height as i64),
    ))
}

/// `(compare-buffer-substrings BUF1 START1 END1 BUF2 START2 END2)` -> integer
pub(crate) fn builtin_compare_buffer_substrings(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let case_fold = super::misc_eval::dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|value| !value.is_nil())
        .unwrap_or(true);
    builtin_compare_buffer_substrings_with_case_fold(case_fold, &eval.buffers, args)
}

pub(crate) fn builtin_compare_buffer_substrings_with_case_fold(
    case_fold: bool,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("compare-buffer-substrings", &args, 6)?;

    let left_buffer = resolve_buffer_designator_allow_nil_current_in_manager(buffers, &args[0])?;
    let right_buffer = resolve_buffer_designator_allow_nil_current_in_manager(buffers, &args[3])?;

    let left_start = if args[1].is_nil() {
        left_buffer
            .and_then(|id| buffers.get(id).map(|buf| buf.point_min_char() as i64 + 1))
            .unwrap_or(1)
    } else {
        expect_integer_or_marker_in_buffers(buffers, &args[1])?
    };
    let left_end = if args[2].is_nil() {
        left_buffer
            .and_then(|id| buffers.get(id).map(|buf| buf.point_max_char() as i64 + 1))
            .unwrap_or(1)
    } else {
        expect_integer_or_marker_in_buffers(buffers, &args[2])?
    };
    let right_start = if args[4].is_nil() {
        right_buffer
            .and_then(|id| buffers.get(id).map(|buf| buf.point_min_char() as i64 + 1))
            .unwrap_or(1)
    } else {
        expect_integer_or_marker_in_buffers(buffers, &args[4])?
    };
    let right_end = if args[5].is_nil() {
        right_buffer
            .and_then(|id| buffers.get(id).map(|buf| buf.point_max_char() as i64 + 1))
            .unwrap_or(1)
    } else {
        expect_integer_or_marker_in_buffers(buffers, &args[5])?
    };

    let left = checked_buffer_slice_for_char_region_in_manager(
        buffers,
        left_buffer,
        left_start,
        left_end,
        args[1],
        args[2],
    )?;
    let right = checked_buffer_slice_for_char_region_in_manager(
        buffers,
        right_buffer,
        right_start,
        right_end,
        args[4],
        args[5],
    )?;
    Ok(Value::fixnum(compare_buffer_substring_strings(
        &left, &right, case_fold,
    )))
}

pub(crate) fn builtin_compute_motion(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let obarray = &eval.obarray;
    let buffers = &eval.buffers;
    expect_args("compute-motion", &args, 7)?;

    let from = expect_integer_or_marker(&args[0])?;
    if !args[1].is_cons() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("consp"), args[1]],
        ));
    }
    let to = expect_integer_or_marker(&args[2])?;
    if !args[3].is_nil() && !args[3].is_cons() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("consp"), args[3]],
        ));
    }
    if !args[4].is_nil() {
        let _ = expect_fixnum(&args[4])?;
    }
    if !args[5].is_nil() && !args[5].is_cons() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("consp"), args[5]],
        ));
    }
    if !args[6].is_nil() && !args[6].is_window() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("window-live-p"), args[6]],
        ));
    }

    // Extract FROMPOS (HPOS . VPOS).
    let (from_hpos, from_vpos) = extract_cons_ints(args[1])?;

    // Extract TOPOS (HPOS . VPOS) or nil.
    let (to_hpos, to_vpos) = if args[3].is_nil() {
        (i64::MAX, i64::MAX)
    } else {
        extract_cons_ints(args[3])?
    };

    // Extract WIDTH.
    let width = if args[4].is_nil() {
        80i64 // default window width
    } else {
        expect_fixnum(&args[4])?
    };

    // Get buffer text.
    let Some(buf) = buffers.current_buffer() else {
        return Ok(Value::list(vec![
            Value::fixnum(from),
            Value::fixnum(from_hpos),
            Value::fixnum(from_vpos),
            Value::fixnum(0),
            Value::NIL,
        ]));
    };
    let text = buf.text.to_string();
    let begv = buf.begv_byte;
    let zv = buf.zv_byte;
    let tab_width = crate::buffer::buffer::lookup_buffer_slot("tab-width")
        .map(|info| buf.slots[info.offset])
        .or_else(|| buf.get_buffer_local("tab-width"))
        .or_else(|| obarray.symbol_value("tab-width").copied())
        .and_then(|value: Value| match value.kind() {
            ValueKind::Fixnum(n) if n > 0 => Some(n as usize),
            _ => None,
        })
        .unwrap_or(8);

    // Convert 1-based char positions to byte offsets.
    let max_chars = buf.text.char_count();
    let from_byte = buf
        .text
        .char_to_emacs_byte(((from - 1).max(0) as usize).min(max_chars));
    let to_byte = buf
        .text
        .char_to_emacs_byte(((to - 1).max(0) as usize).min(max_chars));

    let from_pos = from_byte.clamp(begv, zv);
    let to_pos = to_byte.clamp(begv, zv);

    let mut hpos = from_hpos;
    let mut vpos = from_vpos;
    let mut prev_hpos = from_hpos;
    let mut contin = false;
    let mut pos = from_pos;

    let bytes = text.as_bytes();
    let tw = tab_width.max(1) as i64;

    while pos < to_pos {
        // Check TOPOS stop condition.
        if vpos > to_vpos || (vpos == to_vpos && hpos >= to_hpos) {
            break;
        }

        prev_hpos = hpos;
        let ch = if pos < bytes.len() {
            // Decode UTF-8 character.
            let b = bytes[pos];
            if b < 0x80 {
                pos += 1;
                b as char
            } else {
                let s = &text[pos..];
                let c = s.chars().next().unwrap_or('\u{FFFD}');
                pos += c.len_utf8();
                c
            }
        } else {
            break;
        };

        match ch {
            '\n' => {
                vpos += 1;
                hpos = 0;
                contin = false;
            }
            '\t' => {
                hpos += tw - (hpos % tw);
            }
            _ => {
                hpos += crate::encoding::char_width(ch) as i64;
            }
        }

        // Line continuation (wrapping).
        if hpos >= width && ch != '\n' {
            vpos += 1;
            contin = true;
            hpos -= width;
        }
    }

    // Convert byte pos back to 1-based char position.
    let final_charpos = buf.text.emacs_byte_to_char(pos.min(zv)) as i64 + 1;

    Ok(Value::list(vec![
        Value::fixnum(final_charpos),
        Value::fixnum(hpos),
        Value::fixnum(vpos),
        Value::fixnum(prev_hpos),
        if contin { Value::T } else { Value::NIL },
    ]))
}

/// Extract two integers from a cons cell (CAR . CDR).
fn extract_cons_ints(val: Value) -> Result<(i64, i64), Flow> {
    match val.kind() {
        ValueKind::Cons => {
            let car = val.cons_car();
            let cdr = val.cons_cdr();
            let a = match car.kind() {
                ValueKind::Fixnum(n) => n,
                _ => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("integerp"), car],
                    ));
                }
            };
            let b = match cdr.kind() {
                ValueKind::Fixnum(n) => n,
                _ => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("integerp"), cdr],
                    ));
                }
            };
            Ok((a, b))
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("consp"), val],
        )),
    }
}

pub(crate) fn builtin_coordinates_in_window_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let frames = &mut eval.frames;
    let buffers = &mut eval.buffers;
    expect_args("coordinates-in-window-p", &args, 2)?;

    let (x, y) = if args[0].is_cons() {
        let car = args[0].cons_car();
        let cdr = args[0].cons_cdr();
        let x = match car.kind() {
            ValueKind::Fixnum(n) => n as f64,
            ValueKind::Float => car.xfloat(),
            _other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("numberp"), car],
                ));
            }
        };
        let y = match cdr.kind() {
            ValueKind::Fixnum(n) => n as f64,
            ValueKind::Float => cdr.xfloat(),
            _other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("numberp"), cdr],
                ));
            }
        };
        (x, y)
    } else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("consp"), args[0]],
        ));
    };

    let window_arg = args[1];
    let width = match super::window_cmds::window_total_width_impl(
        frames,
        buffers,
        vec![window_arg],
    )?
    .kind()
    {
        ValueKind::Fixnum(n) => n as f64,
        _ => 0.0,
    };
    let height =
        match super::window_cmds::window_total_height_impl(frames, buffers, vec![window_arg])?
            .kind()
        {
            ValueKind::Fixnum(n) => n as f64,
            _ => 0.0,
        };

    if x >= 0.0 && y >= 0.0 && x < width && y < height {
        Ok(args[0])
    } else {
        Ok(Value::NIL)
    }
}

pub(crate) fn builtin_constrain_to_field(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("constrain-to-field", &args, 2, 5)?;
    let current = &mut eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let point_min = current.point_min_char() as i64 + 1;
    let orig_point = if args[0].is_nil() {
        Some(current.point_char() as i64 + 1)
    } else {
        None
    };
    let mut new_pos = if let Some(point) = orig_point {
        point
    } else {
        expect_integer_or_marker_in_buffers(&mut eval.buffers, &args[0])?
    };
    let old_pos = expect_integer_or_marker_in_buffers(&mut eval.buffers, &args[1])?;
    let escape_from_edge = args.get(2).is_some_and(|value| value.is_truthy());
    let only_in_line = args.get(3).is_some_and(|value| value.is_truthy());

    let old_capture_allowed = if let Some(capture_prop) =
        args.get(4).filter(|value| !value.is_nil())
    {
        let old_capture = crate::emacs_core::builtins::misc_eval::builtin_get_pos_property_impl(
            &eval.obarray,
            &[],
            &mut eval.buffers,
            vec![Value::fixnum(old_pos), *capture_prop],
        )?;
        old_capture.is_nil()
            && (old_pos <= point_min
                || char_property_in_current_buffer(
                    &eval.obarray,
                    &mut eval.buffers,
                    old_pos,
                    *capture_prop,
                )?
                .is_nil()
                || char_property_in_current_buffer(
                    &eval.obarray,
                    &mut eval.buffers,
                    old_pos - 1,
                    *capture_prop,
                )?
                .is_nil())
    } else {
        true
    };

    let field_boundaries_present = !char_property_in_current_buffer(
        &eval.obarray,
        &mut eval.buffers,
        new_pos,
        Value::symbol("field"),
    )?
    .is_nil()
        || !char_property_in_current_buffer(
            &eval.obarray,
            &mut eval.buffers,
            old_pos,
            Value::symbol("field"),
        )?
        .is_nil()
        || (new_pos > point_min
            && !char_property_in_current_buffer(
                &eval.obarray,
                &mut eval.buffers,
                new_pos - 1,
                Value::symbol("field"),
            )?
            .is_nil())
        || (old_pos > point_min
            && !char_property_in_current_buffer(
                &eval.obarray,
                &mut eval.buffers,
                old_pos - 1,
                Value::symbol("field"),
            )?
            .is_nil());

    let inhibit_field_text_motion = super::misc_eval::dynamic_or_global_symbol_value_in_state(
        &eval.obarray,
        &[],
        "inhibit-field-text-motion",
    )
    .is_some_and(|value| !value.is_nil());

    if !inhibit_field_text_motion
        && new_pos != old_pos
        && field_boundaries_present
        && old_capture_allowed
    {
        let forward = new_pos > old_pos;
        let field_bound = if forward {
            expect_int(&builtin_field_end(
                eval,
                vec![
                    Value::fixnum(old_pos),
                    Value::bool_val(escape_from_edge),
                    Value::fixnum(new_pos),
                ],
            )?)?
        } else {
            expect_int(&builtin_field_beginning(
                eval,
                vec![
                    Value::fixnum(old_pos),
                    Value::bool_val(escape_from_edge),
                    Value::fixnum(new_pos),
                ],
            )?)?
        };

        let should_constrain = if field_bound < new_pos {
            forward
        } else {
            !forward
        };
        let same_line = !only_in_line
            || !current_buffer_has_newline_between_positions(
                &mut eval.buffers,
                new_pos,
                field_bound,
            )?;
        if should_constrain && same_line {
            new_pos = field_bound;
        }
    }

    if let Some(orig_point) = orig_point
        && new_pos != orig_point
    {
        let current_id = eval
            .buffers
            .current_buffer_id()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        let buf = &mut eval
            .buffers
            .get(current_id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        let byte_pos = super::editfns::lisp_pos_to_byte(buf, new_pos);
        let _ = eval.buffers.goto_buffer_byte(current_id, byte_pos);
    }

    Ok(Value::fixnum(new_pos))
}

fn char_property_in_current_buffer(
    obarray: &crate::emacs_core::symbol::Obarray,
    buffers: &BufferManager,
    pos: i64,
    property: Value,
) -> Result<Value, Flow> {
    crate::emacs_core::textprop::builtin_get_char_property_in_state(
        obarray,
        buffers,
        vec![Value::fixnum(pos), property],
    )
}

fn current_buffer_has_newline_between_positions(
    buffers: &BufferManager,
    left: i64,
    right: i64,
) -> Result<bool, Flow> {
    let Some(current_id) = buffers.current_buffer_id() else {
        return Err(signal("error", vec![Value::string("No current buffer")]));
    };
    let text = checked_buffer_slice_for_char_region_in_manager(
        buffers,
        Some(current_id),
        left.min(right),
        left.max(right),
        Value::fixnum(left.min(right)),
        Value::fixnum(left.max(right)),
    )?;
    Ok(text.contains('\n'))
}

fn resolve_field_position_in_buffers(
    buffers: &BufferManager,
    position_value: Option<&Value>,
) -> Result<(i64, i64, i64), Flow> {
    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let point_min = buf.point_min_char() as i64 + 1;
    let point_max = buf.point_max_char() as i64 + 1;
    let pos = match position_value {
        None => buf.text.emacs_byte_to_char(buf.pt_byte) as i64 + 1,
        Some(value) if value.is_nil() => buf.text.emacs_byte_to_char(buf.pt_byte) as i64 + 1,
        Some(value) => expect_integer_or_marker_in_buffers(buffers, value)?,
    };
    if pos < point_min || pos > point_max {
        return Err(signal("args-out-of-range", vec![Value::fixnum(pos)]));
    }
    Ok((pos, point_min, point_max))
}

fn field_property_after_char_in_buffers(
    obarray: &crate::emacs_core::symbol::Obarray,
    buffers: &BufferManager,
    pos: i64,
) -> Result<Value, Flow> {
    let value = crate::emacs_core::textprop::builtin_get_char_property_and_overlay_in_state(
        obarray,
        buffers,
        vec![Value::fixnum(pos), Value::symbol("field")],
    )?;
    match value.kind() {
        ValueKind::Cons => Ok(value.cons_car()),
        _other => Err(signal("error", vec![value])),
    }
}

fn field_property_at_position_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &BufferManager,
    pos: i64,
) -> Result<Value, Flow> {
    crate::emacs_core::builtins::misc_eval::builtin_get_pos_property_impl(
        obarray,
        dynamic,
        buffers,
        vec![Value::fixnum(pos), Value::symbol("field")],
    )
}

fn previous_field_change_in_buffers(
    obarray: &crate::emacs_core::symbol::Obarray,
    buffers: &BufferManager,
    pos: i64,
    limit: Option<i64>,
) -> Result<i64, Flow> {
    let mut args = vec![Value::fixnum(pos), Value::symbol("field")];
    if let Some(limit) = limit {
        args.push(Value::NIL);
        args.push(Value::fixnum(limit));
    }
    expect_int(
        &crate::emacs_core::builtins::misc_eval::builtin_previous_single_char_property_change_in_buffers(
            obarray, buffers, args,
        )?,
    )
}

fn next_field_change_in_buffers(
    obarray: &crate::emacs_core::symbol::Obarray,
    buffers: &BufferManager,
    pos: i64,
    limit: Option<i64>,
) -> Result<i64, Flow> {
    let mut args = vec![Value::fixnum(pos), Value::symbol("field")];
    if let Some(limit) = limit {
        args.push(Value::NIL);
        args.push(Value::fixnum(limit));
    }
    expect_int(
        &crate::emacs_core::builtins::misc_eval::builtin_next_single_char_property_change_in_buffers(
            obarray, buffers, args,
        )?,
    )
}

fn find_field_bounds_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &BufferManager,
    position_value: Option<&Value>,
    merge_at_boundary: bool,
    beg_limit: Option<i64>,
    end_limit: Option<i64>,
) -> Result<(i64, i64), Flow> {
    let (pos, point_min, _point_max) = resolve_field_position_in_buffers(buffers, position_value)?;
    let after_field = field_property_after_char_in_buffers(obarray, buffers, pos)?;
    let before_field = if pos > point_min {
        field_property_after_char_in_buffers(obarray, buffers, pos - 1)?
    } else {
        after_field
    };

    let mut at_field_start = false;
    let mut at_field_end = false;
    if !merge_at_boundary {
        let field = field_property_at_position_in_state(obarray, dynamic, buffers, pos)?;
        if !eq_value(&field, &after_field) {
            at_field_end = true;
        }
        if !eq_value(&field, &before_field) {
            at_field_start = true;
        }
        if field.is_nil() && at_field_start && at_field_end {
            at_field_start = false;
            at_field_end = false;
        }
    }

    let boundary = Value::symbol("boundary");
    let beg = if at_field_start {
        pos
    } else {
        let mut cursor = pos;
        if merge_at_boundary && eq_value(&before_field, &boundary) {
            cursor = previous_field_change_in_buffers(obarray, buffers, cursor, beg_limit)?;
        }
        previous_field_change_in_buffers(obarray, buffers, cursor, beg_limit)?
    };
    let end = if at_field_end {
        pos
    } else {
        let mut cursor = pos;
        if merge_at_boundary && eq_value(&after_field, &boundary) {
            cursor = next_field_change_in_buffers(obarray, buffers, cursor, end_limit)?;
        }
        next_field_change_in_buffers(obarray, buffers, cursor, end_limit)?
    };

    Ok((beg, end))
}

pub(crate) fn builtin_field_beginning(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("field-beginning", &args, 3)?;
    let limit = match args.get(2) {
        Some(limit_value) if !limit_value.is_nil() => {
            let limit = expect_integer_or_marker_in_buffers(&eval.buffers, limit_value)?;
            if limit <= 0 {
                return Err(signal("args-out-of-range", vec![Value::fixnum(limit)]));
            }
            Some(limit)
        }
        _ => None,
    };
    let (beg, _) = find_field_bounds_in_state(
        &eval.obarray,
        &[],
        &eval.buffers,
        args.first(),
        args.get(1).is_some_and(|value| value.is_truthy()),
        limit,
        None,
    )?;
    Ok(Value::fixnum(beg))
}

pub(crate) fn builtin_field_end(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_max_args("field-end", &args, 3)?;
    let limit = match args.get(2) {
        Some(limit_value) if !limit_value.is_nil() => Some(expect_integer_or_marker_in_buffers(
            &eval.buffers,
            limit_value,
        )?),
        _ => None,
    };
    let (_, end) = find_field_bounds_in_state(
        &eval.obarray,
        &[],
        &eval.buffers,
        args.first(),
        args.get(1).is_some_and(|value| value.is_truthy()),
        None,
        limit,
    )?;
    Ok(Value::fixnum(end))
}

pub(crate) fn builtin_field_string(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("field-string", &args, 1)?;
    let (beg, end) = find_field_bounds_in_state(
        &eval.obarray,
        &[],
        &eval.buffers,
        args.first(),
        false,
        None,
        None,
    )?;
    builtin_buffer_substring(eval, vec![Value::fixnum(beg), Value::fixnum(end)])
}

pub(crate) fn builtin_field_string_no_properties(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("field-string-no-properties", &args, 1)?;
    let (beg, end) = find_field_bounds_in_state(
        &eval.obarray,
        &[],
        &eval.buffers,
        args.first(),
        false,
        None,
        None,
    )?;
    super::editfns::builtin_buffer_substring_no_properties(
        eval,
        vec![Value::fixnum(beg), Value::fixnum(end)],
    )
}

pub(crate) fn builtin_delete_field(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("delete-field", &args, 1)?;
    let (beg, end) = find_field_bounds_in_state(
        &eval.obarray,
        &[],
        &mut eval.buffers,
        args.first(),
        false,
        None,
        None,
    )?;
    super::editfns::builtin_delete_region(eval, vec![Value::fixnum(beg), Value::fixnum(end)])
}

/// `(clear-string STRING)` -> nil
/// Zeroes out every byte in STRING (fills with null characters).
pub(crate) fn builtin_clear_string(args: Vec<Value>) -> EvalResult {
    expect_args("clear-string", &args, 1)?;
    let _ = expect_strict_string(&args[0])?;
    if args[0].is_string() {
        let _ = args[0].with_lisp_string_mut(|lisp_str| {
            let len = lisp_str.schars();
            // Fill with len null bytes (same as GNU Emacs memset 0)
            let nulls = "\0".repeat(len);
            *lisp_str = crate::heap_types::LispString::new(nulls, lisp_str.is_multibyte());
        });
    }
    Ok(Value::NIL)
}

/// `(command-error-default-function DATA CONTEXT CALLER)` -> nil
pub(crate) fn builtin_command_error_default_function(args: Vec<Value>) -> EvalResult {
    expect_args("command-error-default-function", &args, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_point(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("point", &args, 0)?;
    builtin_point_0(eval)
}

pub(crate) fn builtin_point_0(eval: &mut super::eval::Context) -> EvalResult {
    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    Ok(Value::fixnum(buf.point_char() as i64 + 1))
}

pub(crate) fn builtin_point_min(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("point-min", &args, 0)?;
    builtin_point_min_0(eval)
}

pub(crate) fn builtin_point_min_0(eval: &mut super::eval::Context) -> EvalResult {
    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    Ok(Value::fixnum(buf.point_min_char() as i64 + 1))
}

pub(crate) fn builtin_point_max(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("point-max", &args, 0)?;
    builtin_point_max_0(eval)
}

pub(crate) fn builtin_point_max_0(eval: &mut super::eval::Context) -> EvalResult {
    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    Ok(Value::fixnum(buf.point_max_char() as i64 + 1))
}

pub(crate) fn builtin_goto_char(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("goto-char", &args, 1)?;
    builtin_goto_char_1(eval, args[0])
}

pub(crate) fn builtin_goto_char_1(eval: &mut super::eval::Context, arg: Value) -> EvalResult {
    let pos = expect_integer_or_marker_in_buffers(&eval.buffers, &arg)?;
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let (old_byte, byte_pos) = {
        let buf = eval
            .buffers
            .get(current_id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        (buf.pt_byte, buf.lisp_pos_to_accessible_byte(pos))
    };
    // Adjust for intangible text property
    let direction = if byte_pos >= old_byte { 1 } else { -1 };
    let adjusted = super::navigation::adjust_for_intangible(eval, byte_pos, direction);
    let _ = eval.buffers.goto_buffer_byte(current_id, adjusted);
    // Run point motion hooks
    super::navigation::check_point_motion_hooks(eval, old_byte, adjusted)?;
    Ok(arg)
}

struct InsertPiece {
    text: crate::heap_types::LispString,
    text_props: Option<crate::buffer::text_props::TextPropertyTable>,
}

fn current_buffer_multibyte(buffers: &BufferManager) -> Result<bool, Flow> {
    let current_id = buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    buffers
        .get(current_id)
        .map(|buf| buf.get_multibyte())
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))
}

fn lisp_string_byte_to_char(string: &crate::heap_types::LispString, byte_pos: usize) -> usize {
    let clamped = byte_pos.min(string.sbytes());
    if string.is_multibyte() {
        crate::emacs_core::emacs_char::byte_to_char_pos(string.as_bytes(), clamped)
    } else {
        clamped
    }
}

fn lisp_string_char_to_byte(string: &crate::heap_types::LispString, char_pos: usize) -> usize {
    let clamped = char_pos.min(string.schars());
    if string.is_multibyte() {
        crate::emacs_core::emacs_char::char_to_byte_pos(string.as_bytes(), clamped)
    } else {
        clamped
    }
}

fn lisp_string_advance_byte_to_boundary(
    string: &crate::heap_types::LispString,
    byte_pos: usize,
) -> usize {
    let clamped = byte_pos.min(string.sbytes());
    if !string.is_multibyte() {
        return clamped;
    }

    let bytes = string.as_bytes();
    let mut pos = 0usize;
    while pos < clamped && pos < bytes.len() {
        let (_, len) = crate::emacs_core::emacs_char::string_char(&bytes[pos..]);
        if pos + len >= clamped {
            return pos + len;
        }
        pos += len;
    }
    clamped
}

fn remap_text_property_table(
    table: &crate::buffer::text_props::TextPropertyTable,
    char_map: impl Fn(usize) -> usize,
) -> crate::buffer::text_props::TextPropertyTable {
    let intervals = table
        .intervals_snapshot()
        .into_iter()
        .filter_map(|interval| {
            let start = char_map(interval.start);
            let end = char_map(interval.end);
            (start < end).then_some(crate::buffer::text_props::PropertyInterval {
                start,
                end,
                properties: interval.properties,
                key_order: interval.key_order,
            })
        })
        .collect();
    crate::buffer::text_props::TextPropertyTable::from_dump(intervals)
}

fn buffer_insert_char_codes(
    string: &crate::heap_types::LispString,
    target_multibyte: bool,
) -> Vec<u32> {
    let mut codes = super::lisp_string_char_codes(string);
    if target_multibyte {
        if !string.is_multibyte() {
            for code in &mut codes {
                if *code > 0x7F {
                    *code = crate::emacs_core::emacs_char::unibyte_to_char(*code as u8);
                }
            }
        }
    } else {
        for code in &mut codes {
            *code &= 0xFF;
        }
    }
    codes
}

fn encode_char_code_for_buffer_bytes(code: u32, multibyte: bool) -> Option<Vec<u8>> {
    if code > crate::emacs_core::emacs_char::MAX_CHAR {
        return None;
    }
    if multibyte {
        let mut buf = [0u8; crate::emacs_core::emacs_char::MAX_MULTIBYTE_LENGTH];
        let len = crate::emacs_core::emacs_char::char_string(code, &mut buf);
        Some(buf[..len].to_vec())
    } else {
        Some(vec![(code & 0xFF) as u8])
    }
}

fn buffer_insert_lisp_string_from_lisp_string(
    string: &crate::heap_types::LispString,
    target_multibyte: bool,
) -> crate::heap_types::LispString {
    let codes = buffer_insert_char_codes(string, target_multibyte);
    if target_multibyte {
        let mut bytes = Vec::new();
        for code in codes {
            bytes.extend_from_slice(
                &encode_char_code_for_buffer_bytes(code, true)
                    .expect("valid Emacs character code must encode into buffer bytes"),
            );
        }
        lisp_string_from_buffer_bytes(bytes, true)
    } else {
        let bytes: Vec<u8> = codes
            .into_iter()
            .map(|code| {
                assert!(
                    code <= 0xFF,
                    "unibyte insertion produced non-byte character code {code:#X}"
                );
                code as u8
            })
            .collect();
        lisp_string_from_buffer_bytes(bytes, false)
    }
}

fn buffer_insert_piece_from_string(
    value: Value,
    target_multibyte: bool,
) -> Result<InsertPiece, Flow> {
    let source = value
        .as_lisp_string()
        .ok_or_else(|| signal("wrong-type-argument", vec![Value::symbol("stringp"), value]))?;
    let text = buffer_insert_lisp_string_from_lisp_string(source, target_multibyte);
    let text_props = get_string_text_properties_table_for_value(value).and_then(|table| {
        if table.is_empty() {
            return None;
        }
        Some(table)
    });
    Ok(InsertPiece { text, text_props })
}

pub(crate) fn lisp_string_from_buffer_bytes(
    bytes: Vec<u8>,
    multibyte: bool,
) -> crate::heap_types::LispString {
    if multibyte {
        crate::heap_types::LispString::from_emacs_bytes(bytes)
    } else {
        crate::heap_types::LispString::from_unibyte(bytes)
    }
}

pub(crate) fn buffer_slice_value(
    buf: &crate::buffer::Buffer,
    start_byte: usize,
    end_byte: usize,
) -> Value {
    let mut bytes = Vec::new();
    buf.copy_emacs_bytes_to(start_byte, end_byte, &mut bytes);
    let string = lisp_string_from_buffer_bytes(bytes, buf.get_multibyte());
    let value = Value::heap_string(string);
    if !buf.text.text_props_is_empty() {
        let sliced = buf.text.text_props_slice(start_byte, end_byte);
        if !sliced.is_empty() {
            set_string_text_properties_table_for_value(value, sliced);
        }
    }
    value
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum BufferMultibyteConversionMode {
    AsUnibyte,
    AsMultibyte,
    ToMultibyte,
}

fn remap_string_text_props_for_conversion(
    source: Value,
    target: Value,
    mode: BufferMultibyteConversionMode,
) {
    let Some(table) = get_string_text_properties_table_for_value(source) else {
        return;
    };
    if table.is_empty() {
        return;
    }
    let remapped = match mode {
        BufferMultibyteConversionMode::ToMultibyte => table,
        BufferMultibyteConversionMode::AsUnibyte | BufferMultibyteConversionMode::AsMultibyte => {
            let source_string = source.as_lisp_string().expect("source string");
            let target_string = target.as_lisp_string().expect("target string");
            remap_text_property_table(&table, |char_pos| {
                let source_byte = lisp_string_char_to_byte(source_string, char_pos);
                let boundary = lisp_string_advance_byte_to_boundary(target_string, source_byte);
                lisp_string_byte_to_char(target_string, boundary)
            })
        }
    };
    if !remapped.is_empty() {
        set_string_text_properties_table_for_value(target, remapped);
    }
}

fn convert_buffer_string_for_multibyte(
    source: Value,
    flag: Value,
) -> Result<(Value, BufferMultibyteConversionMode), Flow> {
    let (converted, mode) = if flag.is_nil() {
        (
            misc::builtin_string_as_unibyte(vec![source])?,
            BufferMultibyteConversionMode::AsUnibyte,
        )
    } else if flag.as_symbol_name() == Some("to") {
        (
            misc::builtin_string_to_multibyte(vec![source])?,
            BufferMultibyteConversionMode::ToMultibyte,
        )
    } else {
        (
            misc::builtin_string_as_multibyte(vec![source])?,
            BufferMultibyteConversionMode::AsMultibyte,
        )
    };
    if converted != source {
        remap_string_text_props_for_conversion(source, converted, mode);
    }
    Ok((converted, mode))
}

fn collect_insert_pieces(args: &[Value], target_multibyte: bool) -> Result<Vec<InsertPiece>, Flow> {
    let mut pieces = Vec::with_capacity(args.len());
    for arg in args {
        match arg.kind() {
            ValueKind::String => {
                pieces.push(buffer_insert_piece_from_string(*arg, target_multibyte)?);
            }
            ValueKind::Fixnum(c) => {
                let code = u32::try_from(c).ok();
                let text = code
                    .and_then(|code| encode_char_code_for_buffer_bytes(code, target_multibyte))
                    .map(|bytes| lisp_string_from_buffer_bytes(bytes, target_multibyte))
                    .ok_or_else(|| {
                        signal(
                            "wrong-type-argument",
                            vec![Value::symbol("char-or-string-p"), *arg],
                        )
                    })?;
                pieces.push(InsertPiece {
                    text,
                    text_props: None,
                });
            }
            _other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("char-or-string-p"), *arg],
                ));
            }
        }
    }
    Ok(pieces)
}

pub(crate) fn apply_inherited_text_properties(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &mut BufferManager,
    current_id: BufferId,
    old_pt: usize,
    text_len: usize,
) {
    if text_len == 0 {
        return;
    }

    let props = buffers
        .get(current_id)
        .map(|buf| {
            super::misc_eval::inherited_text_properties_for_inserted_range_in_state(
                obarray, dynamic, buf, old_pt, text_len,
            )
        })
        .unwrap_or_default();
    if props.is_empty() {
        return;
    }

    // put_property prepends new properties to interval order, so apply the
    // merged GNU plist in reverse to preserve the final plist shape.
    for (name, value) in props.iter().rev() {
        let _ =
            buffers.put_buffer_text_property(current_id, old_pt, old_pt + text_len, *name, *value);
    }
}

pub(crate) fn builtin_insert(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let target_multibyte = current_buffer_multibyte(&eval.buffers)?;
    let pieces = collect_insert_pieces(&args, target_multibyte)?;
    let total_len: usize = pieces.iter().map(|p| p.text.sbytes()).sum();
    if total_len == 0 {
        return Ok(Value::NIL);
    }
    let insert_pos = eval
        .buffers
        .current_buffer()
        .map(|buf| buf.pt_byte)
        .unwrap_or(0);
    super::editfns::signal_before_change(eval, insert_pos, insert_pos)?;
    insert_pieces_in_state(&eval.obarray, &[], &mut eval.buffers, pieces, false, false)?;
    super::editfns::signal_after_change(eval, insert_pos, insert_pos + total_len, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_insert_and_inherit(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let target_multibyte = current_buffer_multibyte(&eval.buffers)?;
    let pieces = collect_insert_pieces(&args, target_multibyte)?;
    let total_len: usize = pieces.iter().map(|p| p.text.sbytes()).sum();
    if total_len == 0 {
        return Ok(Value::NIL);
    }
    let insert_pos = eval
        .buffers
        .current_buffer()
        .map(|buf| buf.pt_byte)
        .unwrap_or(0);
    super::editfns::signal_before_change(eval, insert_pos, insert_pos)?;
    insert_pieces_in_state(&eval.obarray, &[], &mut eval.buffers, pieces, false, true)?;
    super::editfns::signal_after_change(eval, insert_pos, insert_pos + total_len, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_insert_before_markers_and_inherit(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let target_multibyte = current_buffer_multibyte(&eval.buffers)?;
    let pieces = collect_insert_pieces(&args, target_multibyte)?;
    let total_len: usize = pieces.iter().map(|p| p.text.sbytes()).sum();
    if total_len == 0 {
        return Ok(Value::NIL);
    }
    let insert_pos = eval
        .buffers
        .current_buffer()
        .map(|buf| buf.pt_byte)
        .unwrap_or(0);
    super::editfns::signal_before_change(eval, insert_pos, insert_pos)?;
    insert_pieces_in_state(&eval.obarray, &[], &mut eval.buffers, pieces, true, true)?;
    super::editfns::signal_after_change(eval, insert_pos, insert_pos + total_len, 0)?;
    Ok(Value::NIL)
}

fn insert_pieces_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &mut BufferManager,
    pieces: Vec<InsertPiece>,
    before_markers: bool,
    inherit: bool,
) -> EvalResult {
    if pieces.iter().all(|piece| piece.text.is_empty()) {
        return Ok(Value::NIL);
    }

    let current_id = buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    if buffers
        .get(current_id)
        .is_some_and(|buf| super::editfns::buffer_read_only_active_in_state(obarray, dynamic, buf))
    {
        return Err(signal(
            "buffer-read-only",
            vec![Value::make_buffer(current_id)],
        ));
    }

    for piece in pieces {
        if piece.text.is_empty() {
            continue;
        }
        let insert_pos = buffers.get(current_id).map(|buf| buf.pt_byte).unwrap_or(0);
        if before_markers {
            let _ = buffers.insert_lisp_string_into_buffer_before_markers(current_id, &piece.text);
        } else {
            let _ = buffers.insert_lisp_string_into_buffer(current_id, &piece.text);
        }
        if inherit {
            apply_inherited_text_properties(
                obarray,
                dynamic,
                buffers,
                current_id,
                insert_pos,
                piece.text.sbytes(),
            );
        }
        if let Some(str_table) = piece.text_props {
            if inherit {
                let _ = buffers
                    .merge_missing_buffer_text_properties(current_id, &str_table, insert_pos);
            } else {
                let _ = buffers.append_buffer_text_properties(current_id, &str_table, insert_pos);
            }
        }
    }
    Ok(Value::NIL)
}

pub(super) fn insert_char_code_from_value(value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(c) => Ok(c as i64),
        _other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *value],
        )),
    }
}

pub(crate) fn builtin_insert_char(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_range_args("insert-char", &args, 1, 3)?;
    let char_code = insert_char_code_from_value(&args[0])?;
    let count = match args.get(1) {
        None => 1,
        Some(value) if value.is_nil() => 1,
        Some(value) => expect_fixnum(value)?,
    };

    if count <= 0 {
        return Ok(Value::NIL);
    }

    let multibyte = current_buffer_multibyte(&eval.buffers)?;
    let unit = if let Some(bytes) = encode_char_code_for_buffer_bytes(char_code as u32, multibyte) {
        bytes
    } else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), args[0]],
        ));
    };
    let mut bytes = Vec::with_capacity(unit.len() * count as usize);
    for _ in 0..count {
        bytes.extend_from_slice(&unit);
    }
    let to_insert = lisp_string_from_buffer_bytes(bytes, multibyte);
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    if eval.buffers.get(current_id).is_some_and(|buf| {
        super::editfns::buffer_read_only_active_in_state(&eval.obarray, &[], buf)
    }) {
        return Err(signal(
            "buffer-read-only",
            vec![Value::make_buffer(current_id)],
        ));
    }

    let insert_pos = eval
        .buffers
        .get(current_id)
        .map(|buf| buf.pt_byte)
        .unwrap_or(0);
    let text_len = to_insert.sbytes();
    super::editfns::signal_before_change(eval, insert_pos, insert_pos)?;
    let _ = eval
        .buffers
        .insert_lisp_string_into_buffer(current_id, &to_insert);
    if args.get(2).is_some_and(|value| value.is_truthy()) {
        apply_inherited_text_properties(
            &eval.obarray,
            &[],
            &mut eval.buffers,
            current_id,
            insert_pos,
            text_len,
        );
    }
    super::editfns::signal_after_change(eval, insert_pos, insert_pos + text_len, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_insert_byte(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_range_args("insert-byte", &args, 2, 3)?;
    let byte = expect_fixnum(&args[0])?;
    if !(0..=255).contains(&byte) {
        return Err(signal(
            "args-out-of-range",
            vec![Value::fixnum(byte), Value::fixnum(0), Value::fixnum(255)],
        ));
    }
    let count = expect_fixnum(&args[1])?;
    if count <= 0 {
        return Ok(Value::NIL);
    }

    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let multibyte = eval
        .buffers
        .get(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?
        .get_multibyte();
    if eval.buffers.get(current_id).is_some_and(|buf| {
        super::editfns::buffer_read_only_active_in_state(&eval.obarray, &[], buf)
    }) {
        return Err(signal(
            "buffer-read-only",
            vec![Value::make_buffer(current_id)],
        ));
    }

    let unit = encode_char_code_for_buffer_bytes(
        if multibyte {
            crate::emacs_core::emacs_char::unibyte_to_char(byte as u8)
        } else {
            byte as u32
        },
        multibyte,
    )
    .expect("insert-byte must produce a valid buffer encoding");
    let mut bytes = Vec::with_capacity(unit.len() * count as usize);
    for _ in 0..count {
        bytes.extend_from_slice(&unit);
    }
    let to_insert = lisp_string_from_buffer_bytes(bytes, multibyte);
    let insert_pos = eval
        .buffers
        .get(current_id)
        .map(|buf| buf.pt_byte)
        .unwrap_or(0);
    let text_len = to_insert.sbytes();
    super::editfns::signal_before_change(eval, insert_pos, insert_pos)?;
    let _ = eval
        .buffers
        .insert_lisp_string_into_buffer(current_id, &to_insert);
    if args.get(2).is_some_and(|value| value.is_truthy()) {
        apply_inherited_text_properties(
            &eval.obarray,
            &[],
            &mut eval.buffers,
            current_id,
            insert_pos,
            text_len,
        );
    }
    super::editfns::signal_after_change(eval, insert_pos, insert_pos + text_len, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_subst_char_in_region(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("subst-char-in-region", &args, 4, 5)?;

    let start = expect_integer_or_marker_in_buffers(&mut eval.buffers, &args[0])?;
    let end = expect_integer_or_marker_in_buffers(&mut eval.buffers, &args[1])?;
    let from_code = expect_character_code(&args[2])?;
    let to_code = expect_character_code(&args[3])?;
    let noundo = args.get(4).is_some_and(|value| !value.is_nil());

    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let target_multibyte = eval
        .buffers
        .get(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?
        .get_multibyte();
    let from_bytes = encode_char_code_for_buffer_bytes(from_code as u32, target_multibyte)
        .ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), args[2]],
            )
        })?;
    let to_bytes =
        encode_char_code_for_buffer_bytes(to_code as u32, target_multibyte).ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), args[3]],
            )
        })?;

    // GNU editfns.c:3051+ uses CHAR_BYTES (Emacs internal encoding length)
    // for this check, not storage-form length. The two agree for standard
    // Unicode but diverge for raw bytes (C0/C1 overlong vs PUA sentinel)
    // and nonunicode codepoints.
    if from_bytes.len() != to_bytes.len() {
        return Err(signal(
            "error",
            vec![Value::string(
                "Characters in `subst-char-in-region' have different byte-lengths",
            )],
        ));
    }

    let (byte_start, byte_end, needs_change) = {
        let buf = &mut eval
            .buffers
            .get(current_id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        let point_min = buf.point_min_char() as i64 + 1;
        let point_max = buf.point_max_char() as i64 + 1;
        if start < point_min || start > point_max || end < point_min || end > point_max {
            return Err(signal(
                "args-out-of-range",
                vec![Value::make_buffer(buf.id), args[0], args[1]],
            ));
        }

        let lo = start.min(end) as usize;
        let hi = start.max(end) as usize;
        let start_char = lo.saturating_sub(1);
        let end_char = hi.saturating_sub(1);
        let byte_start = buf.text.char_to_emacs_byte(start_char);
        let byte_end = buf.text.char_to_emacs_byte(end_char);
        let needs_change = from_code != to_code
            && byte_start < byte_end
            && buf
                .text
                .range_contains_char_code(byte_start, byte_end, from_code as u32);
        (byte_start, byte_end, needs_change)
    };
    if !needs_change {
        return Ok(Value::NIL);
    }

    if eval.buffers.get(current_id).is_some_and(|buf| {
        super::editfns::buffer_read_only_active_in_state(&eval.obarray, &[], buf)
    }) {
        return Err(signal(
            "buffer-read-only",
            vec![Value::make_buffer(current_id)],
        ));
    }

    // subst-char-in-region replaces characters of the same byte length,
    // so the region size does not change.
    let region_len = super::editfns::current_buffer_byte_span_char_len(eval, byte_start, byte_end);
    super::editfns::signal_before_change(eval, byte_start, byte_end)?;
    let _ = &mut eval.buffers.subst_char_in_buffer_region(
        current_id,
        byte_start,
        byte_end,
        from_code as u32,
        &to_bytes,
        noundo,
    );
    super::editfns::signal_after_change(eval, byte_start, byte_end, region_len)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_buffer_enable_undo(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("buffer-enable-undo"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    let id = if args.is_empty() || args[0].is_nil() {
        eval.buffers
            .current_buffer()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?
            .id
    } else {
        match args[0].kind() {
            ValueKind::Veclike(VecLikeType::Buffer) => {
                let bid = args[0].as_buffer_id().unwrap();
                if eval.buffers.get(bid).is_none() {
                    return Ok(Value::NIL);
                }
                bid
            }
            ValueKind::String => {
                let name = expect_buffer_name_string(&args[0])?;
                eval.buffers.find_buffer_by_name(&name).ok_or_else(|| {
                    signal(
                        "error",
                        vec![Value::string(format!("No buffer named {name}"))],
                    )
                })?
            }
            _other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), args[0]],
                ));
            }
        }
    };
    eval.buffers
        .configure_buffer_undo_list(id, Value::NIL)
        .ok_or_else(|| signal("error", vec![Value::string("Selecting deleted buffer")]))?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_buffer_disable_undo(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("buffer-disable-undo"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    let id = if args.is_empty() || args[0].is_nil() {
        eval.buffers
            .current_buffer()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?
            .id
    } else {
        match args[0].kind() {
            ValueKind::Veclike(VecLikeType::Buffer) => {
                let bid = args[0].as_buffer_id().unwrap();
                if eval.buffers.get(bid).is_none() {
                    return Err(signal(
                        "error",
                        vec![Value::string("Selecting deleted buffer")],
                    ));
                }
                bid
            }
            ValueKind::String => {
                let name = expect_buffer_name_string(&args[0])?;
                match eval.buffers.find_buffer_by_name(&name) {
                    Some(id) => id,
                    None => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("stringp"), Value::NIL],
                        ));
                    }
                }
            }
            _other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), args[0]],
                ));
            }
        }
    };
    eval.buffers
        .configure_buffer_undo_list(id, Value::T)
        .ok_or_else(|| signal("error", vec![Value::string("Selecting deleted buffer")]))?;
    Ok(Value::T)
}

pub(crate) fn builtin_buffer_size(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_max_args("buffer-size", &args, 1)?;
    if args.is_empty() || args[0].is_nil() {
        let buf = eval
            .buffers
            .current_buffer()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        return Ok(Value::fixnum(buf.text.char_count() as i64));
    }

    let id = expect_buffer_id(&args[0])?;
    if let Some(buf) = eval.buffers.get(id) {
        Ok(Value::fixnum(buf.text.char_count() as i64))
    } else {
        Ok(Value::fixnum(0))
    }
}

pub(crate) fn builtin_narrow_to_region(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("narrow-to-region", &args, 2)?;
    let start = expect_integer_or_marker_in_buffers(&eval.buffers, &args[0])?;
    let end = expect_integer_or_marker_in_buffers(&eval.buffers, &args[1])?;
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let (byte_start, byte_end) =
        normalize_narrow_region_in_buffers(&eval.buffers, current_id, start, end)?;
    let _ = eval
        .buffers
        .narrow_buffer_to_region(current_id, byte_start, byte_end);
    Ok(Value::NIL)
}

pub(crate) fn builtin_widen(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("widen", &args, 0)?;
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = eval.buffers.widen_buffer(current_id);
    Ok(Value::NIL)
}

pub(crate) fn builtin_buffer_modified_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("buffer-modified-p", &args, 1)?;
    if args.is_empty() || args[0].is_nil() {
        let buf = eval
            .buffers
            .current_buffer()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        return Ok(buf.modified_state_value());
    }

    let id = expect_buffer_id(&args[0])?;
    if let Some(buf) = eval.buffers.get(id) {
        Ok(buf.modified_state_value())
    } else {
        Ok(Value::NIL)
    }
}

pub(crate) fn builtin_set_buffer_modified_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-buffer-modified-p", &args, 1)?;
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let was_modified = eval
        .buffers
        .modified_state_root_id(current_id)
        .and_then(|root_id| eval.buffers.get(root_id))
        .is_some_and(|buffer| buffer.modified_state_value().is_truthy());
    filelock::sync_modified_buffer_file_lock(eval, current_id, was_modified, args[0])?;
    let _ = eval
        .buffers
        .restore_buffer_modified_state(current_id, args[0]);
    super::misc_pure::builtin_force_mode_line_update(eval, vec![Value::NIL])
}

pub(crate) fn builtin_restore_buffer_modified_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("restore-buffer-modified-p", &args, 1)?;
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let was_modified = eval
        .buffers
        .modified_state_root_id(current_id)
        .and_then(|root_id| eval.buffers.get(root_id))
        .is_some_and(|buffer| buffer.modified_state_value().is_truthy());
    filelock::sync_modified_buffer_file_lock(eval, current_id, was_modified, args[0])?;
    eval.buffers
        .restore_buffer_modified_state(current_id, args[0])
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))
}

fn optional_buffer_tick_target_in_manager(
    buffers: &BufferManager,
    name: &str,
    args: &[Value],
) -> Result<Option<BufferId>, Flow> {
    expect_max_args(name, args, 1)?;
    if args.is_empty() || args[0].is_nil() {
        Ok(buffers.current_buffer().map(|buf| buf.id))
    } else {
        Ok(Some(expect_buffer_id(&args[0])?))
    }
}

pub(crate) fn builtin_buffer_modified_tick(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let target =
        optional_buffer_tick_target_in_manager(&eval.buffers, "buffer-modified-tick", &args)?;
    if let Some(id) = target
        && let Some(buf) = eval.buffers.get(id)
    {
        return Ok(Value::fixnum(buf.modified_tick()));
    }
    Ok(Value::fixnum(1))
}

pub(crate) fn builtin_buffer_chars_modified_tick(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let target =
        optional_buffer_tick_target_in_manager(&eval.buffers, "buffer-chars-modified-tick", &args)?;
    if let Some(id) = target
        && let Some(buf) = eval.buffers.get(id)
    {
        return Ok(Value::fixnum(buf.chars_modified_tick()));
    }
    Ok(Value::fixnum(1))
}

pub(crate) fn builtin_internal_set_buffer_modified_tick(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("internal--set-buffer-modified-tick", &args, 1, 2)?;
    let tick = expect_fixnum(&args[0])?;
    let target = if let Some(buffer) = args.get(1) {
        if buffer.is_nil() {
            eval.buffers.current_buffer_id()
        } else {
            Some(expect_buffer_id(buffer)?)
        }
    } else {
        eval.buffers.current_buffer_id()
    }
    .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    eval.buffers
        .set_buffer_modified_tick(target, tick)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_recent_auto_save_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("recent-auto-save-p", &args, 0)?;
    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    Ok(Value::bool_val(buf.recent_auto_save_p()))
}

pub(crate) fn builtin_set_buffer_auto_saved(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-buffer-auto-saved", &args, 0)?;
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    eval.buffers
        .set_buffer_auto_saved(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_buffer_list(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_max_args("buffer-list", &args, 1)?;
    let ids = args
        .first()
        .and_then(|value| match value.kind() {
            ValueKind::Veclike(VecLikeType::Frame) => {
                let fid = crate::window::FrameId(value.as_frame_id().unwrap());
                let frame = eval.frames.get(fid)?;
                let mut ids = Vec::new();
                for window_id in frame.window_list() {
                    let Some(buffer_id) = frame
                        .find_window(window_id)
                        .and_then(|window| window.buffer_id())
                    else {
                        continue;
                    };
                    if !ids.contains(&buffer_id) {
                        ids.push(buffer_id);
                    }
                }
                for buffer_id in eval.buffers.buffer_list() {
                    if !ids.contains(&buffer_id) {
                        ids.push(buffer_id);
                    }
                }
                Some(ids)
            }
            _ => None,
        })
        .unwrap_or_else(|| eval.buffers.buffer_list());
    let vals: Vec<Value> = ids.into_iter().map(Value::make_buffer).collect();
    Ok(Value::list(vals))
}

fn other_buffer_designator(
    buffers: &crate::buffer::BufferManager,
    value: Option<&Value>,
) -> Option<crate::buffer::BufferId> {
    let v = value?;
    match v.kind() {
        ValueKind::Veclike(VecLikeType::Buffer) => {
            let id = v.as_buffer_id().unwrap();
            if buffers.get(id).is_some() {
                Some(id)
            } else {
                None
            }
        }
        ValueKind::String => {
            let name = v
                .as_runtime_string_owned()
                .expect("ValueKind::String must carry LispString payload");
            buffers.find_buffer_by_name(&name)
        }
        _ => None,
    }
}

fn is_hidden_buffer(buffers: &crate::buffer::BufferManager, id: crate::buffer::BufferId) -> bool {
    buffers
        .get(id)
        .map(|buf| buf.name_starts_with_space())
        .unwrap_or(true)
}

pub(crate) fn builtin_other_buffer(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    other_buffer_impl(&mut eval.buffers, args)
}

pub(crate) fn other_buffer_impl(
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("other-buffer", &args, 3)?;

    let current_id = buffers.current_buffer_id();
    let avoid_id = other_buffer_designator(buffers, args.first());
    let visible_ok = args.get(1).is_some_and(|arg| !arg.is_nil());
    let mut notsogood = None;

    for id in buffers.buffer_list() {
        if Some(id) == avoid_id || is_hidden_buffer(buffers, id) {
            continue;
        }
        if visible_ok || Some(id) != current_id {
            return Ok(Value::make_buffer(id));
        }
        if notsogood.is_none() {
            notsogood = Some(id);
        }
    }

    if let Some(id) = notsogood {
        return Ok(Value::make_buffer(id));
    }

    let scratch = buffers
        .find_buffer_by_name("*scratch*")
        .unwrap_or_else(|| buffers.create_buffer("*scratch*"));
    Ok(Value::make_buffer(scratch))
}

pub(crate) fn builtin_generate_new_buffer_name(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("generate-new-buffer-name", &args, 1)?;
    expect_max_args("generate-new-buffer-name", &args, 2)?;
    if args.len() == 2
        && !(args[1].is_nil()
            || args[1].is_t()
            || args[1].is_string()
            || args[1].is_symbol()
            || args[1].as_keyword_id().is_some())
    {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[1]],
        ));
    }
    let base = expect_string(&args[0])?;
    let ignore = args.get(1).and_then(|v| v.as_utf8_str());
    Ok(Value::string(
        eval.buffers
            .generate_new_buffer_name_ignoring(&base, ignore),
    ))
}

/// (bufferp OBJECT) → t or nil
pub(crate) fn builtin_bufferp(args: Vec<Value>) -> EvalResult {
    expect_args("bufferp", &args, 1)?;
    Ok(Value::bool_val(args[0].is_buffer()))
}

pub(crate) fn builtin_char_after(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_max_args("char-after", &args, 1)?;
    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let byte_pos = if args.is_empty() || args[0].is_nil() {
        (buf.point() < buf.zv_byte).then_some(buf.point())
    } else {
        let pos = expect_integer_or_marker_in_buffers(&eval.buffers, &args[0])?;
        if pos <= 0 {
            return Ok(Value::NIL);
        }
        let point_min = point_char_pos(buf, buf.begv_byte);
        let point_max = point_char_pos(buf, buf.zv_byte);
        if pos < point_min || pos >= point_max {
            return Ok(Value::NIL);
        }
        Some(buf.lisp_pos_to_accessible_byte(pos))
    };
    match byte_pos.and_then(|pos| buf.char_code_after(pos)) {
        Some(code) => Ok(Value::fixnum(code as i64)),
        None => Ok(Value::NIL),
    }
}

pub(crate) fn builtin_char_before(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_max_args("char-before", &args, 1)?;
    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let byte_pos = if args.is_empty() || args[0].is_nil() {
        (buf.point() > buf.begv_byte).then_some(buf.point())
    } else {
        let pos = expect_integer_or_marker_in_buffers(&eval.buffers, &args[0])?;
        if pos <= 0 {
            return Ok(Value::NIL);
        }
        let point_min = point_char_pos(buf, buf.begv_byte);
        let point_max = point_char_pos(buf, buf.zv_byte);
        if pos <= point_min || pos > point_max {
            return Ok(Value::NIL);
        }
        Some(buf.lisp_pos_to_accessible_byte(pos))
    };
    match byte_pos.and_then(|pos| buf.char_code_before(pos)) {
        Some(code) => Ok(Value::fixnum(code as i64)),
        None => Ok(Value::NIL),
    }
}

fn is_unibyte_storage_string(s: &str) -> bool {
    // A unibyte storage string has only ASCII chars (0x00..=0x7F) and
    // sentinel chars (0xE300..=0xE3FF) for bytes 0x80..=0xFF.
    // No other Unicode codepoints should appear.
    !s.is_empty()
        && s.chars().all(|ch| {
            let cp = ch as u32;
            cp <= 0x7F || (0xE300..=0xE3FF).contains(&cp)
        })
}

fn get_byte_from_multibyte_char_code(code: u32) -> EvalResult {
    if code <= 0x7F {
        return Ok(Value::fixnum(code as i64));
    }
    if (0x3FFF80..=0x3FFFFF).contains(&code) {
        return Ok(Value::fixnum((code - 0x3FFF00) as i64));
    }
    Err(signal(
        "error",
        vec![Value::string(format!(
            "Not an ASCII nor an 8-bit character: {code}"
        ))],
    ))
}

pub(crate) fn builtin_byte_to_position(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("byte-to-position", &args, 1)?;
    let byte_pos = expect_fixnum(&args[0])?;
    if byte_pos <= 0 {
        return Ok(Value::NIL);
    }

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    let byte_len = buf.text.emacs_byte_len();
    let byte_pos0 = (byte_pos - 1) as usize;
    if byte_pos0 > byte_len {
        return Ok(Value::NIL);
    }

    let mut boundary = byte_pos0;
    if buf.text.is_multibyte() && boundary < byte_len {
        while boundary > 0
            && buf
                .text
                .emacs_byte_at(boundary)
                .is_some_and(|byte| (byte & 0xC0) == 0x80)
        {
            boundary -= 1;
        }
    }

    Ok(Value::fixnum(
        buf.text.emacs_byte_to_char(boundary) as i64 + 1,
    ))
}

pub(crate) fn builtin_position_bytes(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("position-bytes", &args, 1)?;
    let pos = expect_integer_or_marker_in_buffers(&eval.buffers, &args[0])?;

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    let max_char_pos = buf.text.char_count() as i64 + 1;
    if pos <= 0 || pos > max_char_pos {
        return Ok(Value::NIL);
    }

    let byte_pos = buf.text.char_to_emacs_byte((pos - 1) as usize);
    Ok(Value::fixnum(byte_pos as i64 + 1))
}

pub(crate) fn builtin_get_byte(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_max_args("get-byte", &args, 2)?;

    // STRING path: POSITION is a zero-based character index.
    if args.get(1).is_some_and(|v| !v.is_nil()) {
        let string_value = args[1];
        // Validate that arg is a string (without extracting as &str, which
        // would fail for non-UTF-8 unibyte strings).
        if !args[1].is_string() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), args[1]],
            ));
        }
        let pos = if args.is_empty() || args[0].is_nil() {
            0usize
        } else {
            expect_wholenump(&args[0])? as usize
        };

        let string = args[1].as_lisp_string().expect("string");
        let char_len = string.schars();
        if pos >= char_len && !string.is_empty() {
            return Err(signal(
                "args-out-of-range",
                vec![string_value, Value::fixnum(pos as i64)],
            ));
        }

        // Emacs returns 0 for the terminating NUL when indexing an empty string.
        if char_len == 0 {
            return Ok(Value::fixnum(0));
        }

        if !string.is_multibyte() {
            // Unibyte: direct byte access
            return Ok(Value::fixnum((string.as_bytes()[pos] & 0xFF) as i64));
        }
        // Use lisp_string_char_codes which handles sentinel translation
        let codes = super::lisp_string_char_codes(string);
        let code = codes[pos];
        return get_byte_from_multibyte_char_code(code);
    }

    // Buffer path: POSITION is a 1-based character position.
    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    let byte_pos = if args.is_empty() || args[0].is_nil() {
        buf.point()
    } else {
        let pos = expect_integer_or_marker_in_buffers(&eval.buffers, &args[0])?;
        let point_min = buf.point_min_char() as i64 + 1;
        let point_max = buf.point_max_char() as i64 + 1;
        if pos < point_min || pos >= point_max {
            return Err(signal(
                "args-out-of-range",
                vec![args[0], Value::fixnum(point_min), Value::fixnum(point_max)],
            ));
        }
        buf.lisp_pos_to_accessible_byte(pos)
    };

    if byte_pos >= buf.text.len() {
        return Ok(Value::fixnum(0));
    }

    if !buf.get_multibyte() {
        let code = match buf.char_code_after(byte_pos) {
            Some(code) => code,
            None => return Ok(Value::fixnum(0)),
        };
        assert!(
            code <= 0xFF,
            "unibyte buffer storage contained non-byte character code {code:#X}"
        );
        return Ok(Value::fixnum(code as i64));
    }

    let code = match buf.char_code_after(byte_pos) {
        Some(code) => code,
        None => return Ok(Value::fixnum(0)),
    };

    get_byte_from_multibyte_char_code(code)
}

pub(crate) fn builtin_buffer_local_value(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    use crate::emacs_core::intern::intern;
    use crate::emacs_core::symbol::SymbolRedirect;

    expect_args("buffer-local-value", &args, 2)?;
    let original_arg = args[0];
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let resolved = crate::emacs_core::builtins::symbols::resolve_variable_alias_name_in_obarray(
        eval.obarray(),
        name,
    )?;
    let resolved_id = intern(&resolved);
    let id = expect_buffer_id(&args[1])?;
    let buf = eval
        .buffers
        .get(id)
        .ok_or_else(|| signal("error", vec![Value::string("No such buffer")]))?;

    // Phase 10E: route LOCALIZED reads through the BLV machinery
    // (immutable walker — buffer-local-value never swaps the cache).
    // Mirrors GNU `Fbuffer_local_value` SYMBOL_LOCALIZED arm at
    // `data.c:1696-1740` which uses `blv_value` (returning the
    // already-loaded valcell.cdr if `where == buf`, else walks
    // `BVAR(buf, local_var_alist)`), then signals void-variable if
    // the result is `Qunbound`.
    if let Some(sym_slot) = eval.obarray().get_by_id(resolved_id)
        && sym_slot.redirect() == SymbolRedirect::Localized
    {
        let target_buf = Value::make_buffer(buf.id);
        if let Some(value) =
            eval.obarray()
                .read_localized(resolved_id, target_buf, buf.local_var_alist)
        {
            if value.is_unbound() {
                return Err(signal("void-variable", vec![original_arg]));
            }
            return Ok(value);
        }
    }

    match buf.get_buffer_local_binding(&resolved) {
        Some(binding) => binding
            .as_value()
            .or_else(|| {
                (resolved == "buffer-undo-list")
                    .then(|| buf.buffer_local_value(&resolved))
                    .flatten()
            })
            .ok_or_else(|| signal("void-variable", vec![original_arg])),
        None if crate::buffer::buffer::lookup_buffer_slot(&resolved).is_some() => buf
            .buffer_local_value(&resolved)
            .ok_or_else(|| signal("void-variable", vec![original_arg])),
        None if resolved == "nil" => Ok(Value::NIL),
        None if resolved == "t" => Ok(Value::T),
        None if resolved.starts_with(':') => Ok(Value::symbol(resolved)),
        None => eval
            .obarray()
            .symbol_value(&resolved)
            .cloned()
            .ok_or_else(|| signal("void-variable", vec![original_arg])),
    }
}
