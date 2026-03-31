use super::*;
use crate::emacs_core::hook_runtime;

// ===========================================================================
// Hook system
// ===========================================================================

pub(crate) fn builtin_run_hooks(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    hook_runtime::run_named_hooks(eval, &args)
}

pub(crate) fn builtin_run_hook_with_args(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("run-hook-with-args", &args, 1)?;
    hook_runtime::run_named_hook_with_args(eval, &args)
}

pub(crate) fn builtin_run_hook_with_args_until_success(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("run-hook-with-args-until-success", &args, 1)?;
    hook_runtime::run_named_hook_with_args_until_success(eval, &args)
}

pub(crate) fn builtin_run_hook_with_args_until_failure(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("run-hook-with-args-until-failure", &args, 1)?;
    hook_runtime::run_named_hook_with_args_until_failure(eval, &args)
}

pub(crate) fn builtin_run_hook_wrapped(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("run-hook-wrapped", &args, 2)?;
    hook_runtime::run_named_hook_wrapped(eval, &args)
}

pub(crate) fn builtin_run_hook_query_error_with_timeout(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("run-hook-query-error-with-timeout", &args, 1)?;
    let hook_sym = hook_runtime::resolve_hook_symbol(eval, args[0])?;
    let hook_value = hook_runtime::hook_value_by_id(eval, hook_sym).unwrap_or(Value::NIL);
    hook_runtime::run_hook_query_error_with_timeout(eval, hook_sym, hook_value)
}

fn expect_optional_live_frame_designator(
    value: &Value,
    eval: &super::eval::Context,
) -> Result<(), Flow> {
    expect_optional_live_frame_designator_in_state(value, &eval.frames)
}

fn expect_optional_live_frame_designator_in_state(
    value: &Value,
    frames: &crate::window::FrameManager,
) -> Result<(), Flow> {
    if value.is_nil() {
        return Ok(());
    }
    if value.is_frame() /* TODO(tagged): `id` was Value::Frame(id), now use accessor */ {
        if frames.get(crate::window::FrameId(*id)).is_some() {
            return Ok(());
        }
    }
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("frame-live-p"), *value],
    ))
}

#[derive(Clone, Copy)]
struct HookCallerContextState {
    selected_frame_id: Option<crate::window::FrameId>,
    selected_window_id: Option<crate::window::WindowId>,
    current_buffer_id: Option<crate::buffer::BufferId>,
}

fn save_hook_caller_context(eval: &super::eval::Context) -> HookCallerContextState {
    let selected_frame_id = eval.frames.selected_frame().map(|frame| frame.id);
    let selected_window_id = selected_frame_id
        .and_then(|frame_id| eval.frames.get(frame_id).map(|frame| frame.selected_window));
    HookCallerContextState {
        selected_frame_id,
        selected_window_id,
        current_buffer_id: eval.buffers.current_buffer_id(),
    }
}

fn window_buffer_id_in_state(
    eval: &super::eval::Context,
    frame_id: crate::window::FrameId,
    window_id: crate::window::WindowId,
) -> Option<crate::buffer::BufferId> {
    eval.frames
        .get(frame_id)
        .and_then(|frame| frame.find_window(window_id))
        .and_then(|window| window.buffer_id())
}

fn select_frame_window_for_hook_context(
    eval: &mut super::eval::Context,
    frame_id: crate::window::FrameId,
    window_id: crate::window::WindowId,
) {
    let _ = eval.frames.select_frame(frame_id);
    eval.sync_keyboard_terminal_owner();
    if let Some(frame) = eval.frames.get_mut(frame_id) {
        let _ = frame.select_window(window_id);
    }
    if let Some(buffer_id) = window_buffer_id_in_state(eval, frame_id, window_id) {
        let _ = eval.switch_current_buffer(buffer_id);
    }
}

fn restore_hook_caller_context(eval: &mut super::eval::Context, saved: HookCallerContextState) {
    if let Some(frame_id) = saved
        .selected_frame_id
        .filter(|frame_id| eval.frames.get(*frame_id).is_some())
    {
        let _ = eval.frames.select_frame(frame_id);
        eval.sync_keyboard_terminal_owner();
        if let Some(window_id) = saved.selected_window_id
            && let Some(frame) = eval.frames.get_mut(frame_id)
        {
            let _ = frame.select_window(window_id);
        }
    }
    if let Some(buffer_id) = saved.current_buffer_id {
        eval.restore_current_buffer_if_live(buffer_id);
    }
}

#[derive(Clone, Copy)]
struct LiveWindowHookState {
    window_id: crate::window::WindowId,
    buffer_id: crate::buffer::BufferId,
    bounds: crate::window::Rect,
}

#[derive(Clone)]
struct FrameWindowHookPlan {
    frame_id: crate::window::FrameId,
    frame_buffer_change: bool,
    frame_size_change: bool,
    frame_selected_window_change: bool,
    frame_state_change: bool,
    local_buffer_windows: Vec<crate::window::WindowId>,
    local_size_windows: Vec<crate::window::WindowId>,
    local_selection_windows: Vec<crate::window::WindowId>,
    local_state_windows: Vec<crate::window::WindowId>,
}

fn push_unique_window(
    windows: &mut Vec<crate::window::WindowId>,
    window_id: crate::window::WindowId,
) {
    if !windows.contains(&window_id) {
        windows.push(window_id);
    }
}

fn live_windows_for_hook_plan(frame: &crate::window::Frame) -> Vec<LiveWindowHookState> {
    let mut windows = Vec::new();
    for window_id in frame.window_list() {
        let Some(window) = frame.find_window(window_id) else {
            continue;
        };
        let Some(buffer_id) = window.buffer_id() else {
            continue;
        };
        windows.push(LiveWindowHookState {
            window_id,
            buffer_id,
            bounds: *window.bounds(),
        });
    }
    if let Some(minibuffer_window) = frame.minibuffer_window
        && let Some(window) = frame.find_window(minibuffer_window)
        && let Some(buffer_id) = window.buffer_id()
    {
        windows.push(LiveWindowHookState {
            window_id: minibuffer_window,
            buffer_id,
            bounds: *window.bounds(),
        });
    }
    windows
}

fn frame_window_hook_record_from_live_state(
    frame: &crate::window::Frame,
    was_selected_frame: bool,
) -> crate::window::FrameWindowHookRecord {
    let windows = live_windows_for_hook_plan(frame)
        .into_iter()
        .map(|window| {
            (
                window.window_id,
                crate::window::WindowHookSnapshot {
                    buffer_id: window.buffer_id,
                    bounds: window.bounds,
                },
            )
        })
        .collect();
    crate::window::FrameWindowHookRecord {
        windows,
        selected_window: Some(frame.selected_window),
        was_selected_frame,
    }
}

fn run_window_local_hook_values(
    eval: &mut super::eval::Context,
    frame_id: crate::window::FrameId,
    window_ids: &[crate::window::WindowId],
    hook_name: &str,
    hook_sym: crate::emacs_core::intern::SymId,
) -> EvalResult {
    if window_ids.is_empty() {
        return Ok(Value::NIL);
    }

    let saved = save_hook_caller_context(eval);
    let result = (|| -> EvalResult {
        for window_id in window_ids {
            let Some(buffer_id) = window_buffer_id_in_state(eval, frame_id, *window_id) else {
                continue;
            };
            let has_local_hook = eval
                .buffers
                .get(buffer_id)
                .and_then(|buffer| buffer.get_buffer_local_binding(hook_name))
                .is_some();
            if !has_local_hook {
                continue;
            }
            select_frame_window_for_hook_context(eval, frame_id, *window_id);
            let Some(local_hook_value) = eval
                .buffers
                .current_buffer()
                .and_then(|buffer| buffer.buffer_local_value(hook_name))
            else {
                continue;
            };
            let _ = hook_runtime::safe_run_hook_value(
                eval,
                hook_sym,
                local_hook_value,
                &[Value::make_window(window_id.0)],
                false,
            )?;
        }
        Ok(Value::NIL)
    })();
    restore_hook_caller_context(eval, saved);
    result
}

fn run_window_default_hook_value(
    eval: &mut super::eval::Context,
    frame_id: crate::window::FrameId,
    run_hook: bool,
    hook_sym: crate::emacs_core::intern::SymId,
) -> EvalResult {
    if !run_hook {
        return Ok(Value::NIL);
    }
    let global_hook_value = eval
        .obarray
        .default_value_id(hook_sym)
        .copied()
        .unwrap_or(Value::NIL);
    if global_hook_value.is_nil() {
        return Ok(Value::NIL);
    }

    let saved = save_hook_caller_context(eval);
    let result = (|| -> EvalResult {
        let selected_window = eval.frames.get(frame_id).map(|frame| frame.selected_window);
        if let Some(selected_window) = selected_window {
            select_frame_window_for_hook_context(eval, frame_id, selected_window);
        } else {
            let _ = eval.frames.select_frame(frame_id);
            eval.sync_keyboard_terminal_owner();
        }
        let _ = hook_runtime::safe_run_hook_value(
            eval,
            hook_sym,
            global_hook_value,
            &[Value::make_frame(frame_id.0)],
            false,
        )?;
        Ok(Value::NIL)
    })();
    restore_hook_caller_context(eval, saved);
    result
}

pub(crate) fn run_redisplay_window_change_hooks(eval: &mut super::eval::Context) -> EvalResult {
    let frame_ids = eval.frames.frame_list();
    let selected_frame_id = eval.frames.selected_frame().map(|frame| frame.id);
    let mut plans = Vec::new();

    for frame_id in &frame_ids {
        let Some(frame) = eval.frames.get(*frame_id) else {
            continue;
        };
        let previous_record = frame.window_hook_record.clone();
        let current_windows = live_windows_for_hook_plan(frame);
        let selected_window = Some(frame.selected_window);
        let frame_selected_window_change = previous_record.selected_window != selected_window;
        let frame_selected_change =
            previous_record.was_selected_frame != (selected_frame_id == Some(*frame_id));
        let window_deleted = previous_record.windows.keys().any(|window_id| {
            !current_windows
                .iter()
                .any(|window| window.window_id == *window_id)
        });

        let mut local_buffer_windows = Vec::new();
        let mut local_size_windows = Vec::new();
        let mut local_selection_windows = Vec::new();
        let mut local_state_windows = Vec::new();

        for window in &current_windows {
            let previous = previous_record.windows.get(&window.window_id);
            let buffer_changed = previous.is_none()
                || previous.is_some_and(|entry| entry.buffer_id != window.buffer_id);
            let size_changed =
                previous.is_none() || previous.is_some_and(|entry| entry.bounds != window.bounds);
            let selection_changed = frame_selected_window_change
                && (previous_record.selected_window == Some(window.window_id)
                    || selected_window == Some(window.window_id));

            if buffer_changed {
                push_unique_window(&mut local_buffer_windows, window.window_id);
                push_unique_window(&mut local_size_windows, window.window_id);
                push_unique_window(&mut local_state_windows, window.window_id);
            }
            if size_changed {
                push_unique_window(&mut local_size_windows, window.window_id);
                push_unique_window(&mut local_state_windows, window.window_id);
            }
            if selection_changed {
                push_unique_window(&mut local_selection_windows, window.window_id);
                push_unique_window(&mut local_state_windows, window.window_id);
            }
        }

        let frame_buffer_change = !local_buffer_windows.is_empty();
        let frame_size_change = !local_size_windows.is_empty();
        let frame_state_change = frame.window_state_change
            || frame_selected_change
            || frame_selected_window_change
            || frame_buffer_change
            || frame_size_change
            || window_deleted;

        plans.push(FrameWindowHookPlan {
            frame_id: *frame_id,
            frame_buffer_change,
            frame_size_change,
            frame_selected_window_change,
            frame_state_change,
            local_buffer_windows,
            local_size_windows,
            local_selection_windows,
            local_state_windows,
        });
    }

    let window_buffer_change_functions =
        hook_runtime::hook_symbol_by_name(eval, "window-buffer-change-functions");
    let window_size_change_functions =
        hook_runtime::hook_symbol_by_name(eval, "window-size-change-functions");
    let window_selection_change_functions =
        hook_runtime::hook_symbol_by_name(eval, "window-selection-change-functions");
    let window_state_change_functions =
        hook_runtime::hook_symbol_by_name(eval, "window-state-change-functions");
    let window_state_change_hook =
        hook_runtime::hook_symbol_by_name(eval, "window-state-change-hook");

    let mut run_window_state_change_hook = false;
    for plan in &plans {
        if eval.frames.get(plan.frame_id).is_none() {
            continue;
        }
        run_window_local_hook_values(
            eval,
            plan.frame_id,
            &plan.local_buffer_windows,
            "window-buffer-change-functions",
            window_buffer_change_functions,
        )?;
        run_window_default_hook_value(
            eval,
            plan.frame_id,
            plan.frame_buffer_change,
            window_buffer_change_functions,
        )?;

        run_window_local_hook_values(
            eval,
            plan.frame_id,
            &plan.local_size_windows,
            "window-size-change-functions",
            window_size_change_functions,
        )?;
        run_window_default_hook_value(
            eval,
            plan.frame_id,
            plan.frame_size_change || plan.frame_buffer_change,
            window_size_change_functions,
        )?;

        run_window_local_hook_values(
            eval,
            plan.frame_id,
            &plan.local_selection_windows,
            "window-selection-change-functions",
            window_selection_change_functions,
        )?;
        run_window_default_hook_value(
            eval,
            plan.frame_id,
            plan.frame_selected_window_change,
            window_selection_change_functions,
        )?;

        run_window_local_hook_values(
            eval,
            plan.frame_id,
            &plan.local_state_windows,
            "window-state-change-functions",
            window_state_change_functions,
        )?;
        run_window_default_hook_value(
            eval,
            plan.frame_id,
            plan.frame_state_change,
            window_state_change_functions,
        )?;
        run_window_state_change_hook |= plan.frame_state_change;
    }

    if run_window_state_change_hook {
        let _ = hook_runtime::safe_run_named_hook(eval, window_state_change_hook, &[])?;
    }

    let selected_frame_id = eval.frames.selected_frame().map(|frame| frame.id);
    for frame_id in frame_ids {
        let was_selected_frame = selected_frame_id == Some(frame_id);
        if let Some(frame) = eval.frames.get_mut(frame_id) {
            frame.window_hook_record =
                frame_window_hook_record_from_live_state(frame, was_selected_frame);
            frame.window_state_change = false;
        }
    }

    Ok(Value::NIL)
}

pub(super) fn expect_optional_live_window_designator(
    value: &Value,
    eval: &super::eval::Context,
) -> Result<(), Flow> {
    if value.is_nil() {
        return Ok(());
    }
    if value.is_window() /* TODO(tagged): `id` was Value::Window(id), now use accessor */ {
        if eval.frames.is_live_window_id(crate::window::WindowId(*id)) {
            return Ok(());
        }
    }
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("window-live-p"), *value],
    ))
}

const WINDOW_CONFIGURATION_TAG: &str = "window-configuration";
const SAVE_SELECTED_WINDOW_STATE_TAG: &str = "save-selected-window--state";

#[derive(Clone)]
struct WindowConfigurationSnapshot {
    frame_id: crate::window::FrameId,
    root_window: crate::window::Window,
    selected_window: crate::window::WindowId,
    minibuffer_window: Option<crate::window::WindowId>,
    minibuffer_leaf: Option<crate::window::Window>,
}

fn normalize_selected_window_point_in_snapshot(
    snapshot: &mut WindowConfigurationSnapshot,
    buffers: &crate::buffer::BufferManager,
) {
    let selected_buffer_id = snapshot
        .root_window
        .find(snapshot.selected_window)
        .or_else(|| {
            snapshot
                .minibuffer_leaf
                .as_ref()
                .filter(|window| window.id() == snapshot.selected_window)
        })
        .and_then(|window| window.buffer_id());
    let Some(buffer_id) = selected_buffer_id else {
        return;
    };
    let Some(point) = buffers
        .get(buffer_id)
        .map(|buffer| buffer.point_char().saturating_add(1))
    else {
        return;
    };

    if let Some(crate::window::Window::Leaf {
        point: window_point,
        ..
    }) = snapshot.root_window.find_mut(snapshot.selected_window)
    {
        *window_point = point;
        return;
    }

    if let Some(crate::window::Window::Leaf {
        point: window_point,
        ..
    }) = snapshot
        .minibuffer_leaf
        .as_mut()
        .filter(|window| window.id() == snapshot.selected_window)
    {
        *window_point = point;
    }
}

thread_local! {
    static WINDOW_CONFIGURATION_SNAPSHOTS: RefCell<HashMap<i64, WindowConfigurationSnapshot>> =
        RefCell::new(HashMap::new());
}

pub(super) fn reset_hooks_thread_locals() {
    WINDOW_CONFIGURATION_SNAPSHOTS.with(|slot| slot.borrow_mut().clear());
}

fn window_configuration_parts_from_value(value: &Value) -> Option<(Value, i64)> {
    if !value.is_vector() /* TODO(tagged): `data` was Value::Vector(data), rewrite let-else */ {
        return None;
    };
    let items = with_heap(|h| h.get_vector(*data).clone());
    if items.len() != 3 || items[0].as_symbol_name() != Some(WINDOW_CONFIGURATION_TAG) {
        return None;
    }
    match (items[1].kind(), items[2].kind()) {
        (ValueKind::Veclike(VecLikeType::Frame), ValueKind::Fixnum(serial)) => Some((items[1], serial)),
        _ => None,
    }
}

fn window_configuration_frame_from_value(value: &Value) -> Option<Value> {
    window_configuration_parts_from_value(value).map(|(frame, _)| frame)
}

fn next_window_configuration_serial() -> i64 {
    use std::sync::atomic::{AtomicU64, Ordering};
use super::value::{ValueKind, VecLikeType};
    static NEXT_WINDOW_CONFIGURATION_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_WINDOW_CONFIGURATION_ID.fetch_add(1, Ordering::Relaxed) as i64
}

fn make_window_configuration_value(frame: Value, serial: i64) -> Value {
    Value::vector(vec![
        Value::symbol(WINDOW_CONFIGURATION_TAG),
        frame,
        Value::fixnum(serial),
    ])
}

pub(crate) fn builtin_window_configuration_p(args: Vec<Value>) -> EvalResult {
    expect_args("window-configuration-p", &args, 1)?;
    Ok(Value::bool_val(
        window_configuration_frame_from_value(&args[0]).is_some(),
    ))
}

pub(crate) fn builtin_window_configuration_frame(args: Vec<Value>) -> EvalResult {
    expect_args("window-configuration-frame", &args, 1)?;
    window_configuration_frame_from_value(&args[0]).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("window-configuration-p"), args[0]],
        )
    })
}

pub(crate) fn builtin_window_configuration_equal_p(args: Vec<Value>) -> EvalResult {
    expect_args("window-configuration-equal-p", &args, 2)?;
    if window_configuration_frame_from_value(&args[0]).is_none() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("window-configuration-p"), args[0]],
        ));
    }
    if window_configuration_frame_from_value(&args[1]).is_none() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("window-configuration-p"), args[1]],
        ));
    }
    Ok(Value::bool_val(equal_value(&args[0], &args[1], 0)))
}

pub(crate) fn builtin_current_window_configuration(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("current-window-configuration", &args, 1)?;

    let frame = if let Some(frame) = args.first() {
        expect_optional_live_frame_designator_in_state(frame, &mut eval.frames)?;
        if frame.is_nil() {
            super::window_cmds::selected_frame_impl(&mut eval.frames, &mut eval.buffers, vec![])?
        } else {
            *frame
        }
    } else {
        super::window_cmds::selected_frame_impl(&mut eval.frames, &mut eval.buffers, vec![])?
    };

    if !frame.is_frame() /* TODO(tagged): `frame_raw_id` was Value::Frame(frame_raw_id), rewrite let-else */ {
        return Ok(make_window_configuration_value(
            frame,
            next_window_configuration_serial(),
        ));
    };
    let frame_id = crate::window::FrameId(frame_raw_id);
    if let Some(frame_state) = eval.frames.get(frame_id) {
        let mut snapshot = WindowConfigurationSnapshot {
            frame_id,
            root_window: frame_state.root_window.clone(),
            selected_window: frame_state.selected_window,
            minibuffer_window: frame_state.minibuffer_window,
            minibuffer_leaf: frame_state.minibuffer_leaf.clone(),
        };
        normalize_selected_window_point_in_snapshot(&mut snapshot, &mut eval.buffers);
        let serial = next_window_configuration_serial();
        WINDOW_CONFIGURATION_SNAPSHOTS.with(|slot| {
            let mut store = slot.borrow_mut();
            store.insert(serial, snapshot);
            if store.len() > 4096 {
                if let Some(oldest) = store.keys().min().copied() {
                    store.remove(&oldest);
                }
            }
        });
        return Ok(make_window_configuration_value(frame, serial));
    }

    Ok(make_window_configuration_value(
        frame,
        next_window_configuration_serial(),
    ))
}

pub(crate) fn builtin_set_window_configuration(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("set-window-configuration", &args, 1, 3)?;
    let Some((_frame, serial)) = window_configuration_parts_from_value(&args[0]) else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("window-configuration-p"), args[0]],
        ));
    };

    let snapshot = WINDOW_CONFIGURATION_SNAPSHOTS.with(|slot| slot.borrow().get(&serial).cloned());

    if let Some(snapshot) = snapshot {
        let selected_window_state = if let Some(frame) = eval.frames.get_mut(snapshot.frame_id) {
            frame.root_window = snapshot.root_window;
            frame.selected_window = snapshot.selected_window;
            frame.minibuffer_window = snapshot.minibuffer_window;
            frame.minibuffer_leaf = snapshot.minibuffer_leaf;
            frame
                .find_window(frame.selected_window)
                .and_then(|window| match window {
                    crate::window::Window::Leaf {
                        buffer_id, point, ..
                    } => Some((*buffer_id, *point)),
                    crate::window::Window::Internal { .. } => None,
                })
        } else {
            None
        };
        if let Some((buffer_id, point)) = selected_window_state {
            eval.switch_current_buffer(buffer_id)?;
            if let Some(buffer) = eval.buffers.get(buffer_id) {
                let byte_pos = buffer.lisp_pos_to_byte(point as i64);
                let _ = eval.buffers.goto_buffer_byte(buffer_id, byte_pos);
            }
        }
    }

    eval.redisplay();
    // Run window-configuration-change-hook after restoring configuration.
    let _ = builtin_run_window_configuration_change_hook(eval, vec![]);
    Ok(Value::T)
}

fn save_selected_window_state_from_value(
    value: &Value,
) -> Option<(Value, Value, Option<crate::buffer::BufferId>)> {
    if !value.is_vector() /* TODO(tagged): `data` was Value::Vector(data), rewrite let-else */ {
        return None;
    };
    let items = with_heap(|h| h.get_vector(*data).clone());
    if items.len() != 4 || items[0].as_symbol_name() != Some(SAVE_SELECTED_WINDOW_STATE_TAG) {
        return None;
    }
    let frame = items[1];
    let window = items[2];
    let buffer_id = match items[3].kind() {
        ValueKind::Veclike(VecLikeType::Buffer) => Some(id),
        _ => None,
    };
    Some((frame, window, buffer_id))
}

pub(super) fn builtin_internal_before_save_selected_window(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("internal--before-save-selected-window", &args, 0)?;
    let frame = super::window_cmds::builtin_selected_frame(eval, vec![])?;
    let window = super::window_cmds::builtin_selected_window(eval, vec![])?;
    let buffer = eval
        .buffers
        .current_buffer()
        .map(|buffer| Value::make_buffer(buffer.id))
        .unwrap_or(Value::NIL);
    Ok(Value::vector(vec![
        Value::symbol(SAVE_SELECTED_WINDOW_STATE_TAG),
        frame,
        window,
        buffer,
    ]))
}

pub(super) fn builtin_internal_after_save_selected_window(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("internal--after-save-selected-window", &args, 1)?;
    let Some((saved_frame, saved_window, saved_buffer)) =
        save_selected_window_state_from_value(&args[0])
    else {
        return Err(signal(
            "wrong-type-argument",
            vec![
                Value::symbol("vectorp"),
                args.first().cloned().unwrap_or(Value::NIL),
            ],
        ));
    };

    let _ = super::window_cmds::builtin_select_frame(eval, vec![saved_frame, Value::NIL]);
    let _ = super::window_cmds::builtin_select_window(eval, vec![saved_window, Value::NIL]);
    if let Some(buffer_id) = saved_buffer {
        if eval.buffers.get(buffer_id).is_some() {
            eval.restore_current_buffer_if_live(buffer_id);
        }
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_run_window_configuration_change_hook(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("run-window-configuration-change-hook", &args, 1)?;
    if let Some(frame) = args.first() {
        expect_optional_live_frame_designator(frame, eval)?;
    }
    let frame = match args.first().copied().unwrap_or(Value::Nil).kind() {
        ValueKind::Nil => {
            super::window_cmds::selected_frame_impl(&mut eval.frames, &mut eval.buffers, vec![])?
        }
        value => value,
    };
    if !frame.is_frame() /* TODO(tagged): `frame_raw_id` was Value::Frame(frame_raw_id), rewrite let-else */ {
        return Ok(Value::NIL);
    };
    let frame_id = crate::window::FrameId(frame_raw_id);
    let Some(frame_state) = eval.frames.get(frame_id) else {
        return Ok(Value::NIL);
    };

    let hook_sym = hook_runtime::hook_symbol_by_name(eval, "window-configuration-change-hook");
    let global_hook_value = eval
        .obarray
        .default_value_id(hook_sym)
        .copied()
        .unwrap_or(Value::NIL);
    let selected_window = frame_state.selected_window;
    let window_ids = frame_state.window_list();
    let hook_name = crate::emacs_core::intern::resolve_sym(hook_sym);
    let saved = save_hook_caller_context(eval);

    let result = (|| -> EvalResult {
        select_frame_window_for_hook_context(eval, frame_id, selected_window);
        for window_id in &window_ids {
            let Some(buffer_id) = window_buffer_id_in_state(eval, frame_id, *window_id) else {
                continue;
            };
            let has_local_hook = eval
                .buffers
                .get(buffer_id)
                .and_then(|buffer| buffer.get_buffer_local_binding(hook_name))
                .is_some();
            if !has_local_hook {
                continue;
            }
            select_frame_window_for_hook_context(eval, frame_id, *window_id);
            let Some(local_hook_value) = eval
                .buffers
                .current_buffer()
                .and_then(|buffer| buffer.buffer_local_value(hook_name))
            else {
                continue;
            };
            let _ = hook_runtime::run_hook_value(eval, hook_sym, local_hook_value, &[], false)?;
            select_frame_window_for_hook_context(eval, frame_id, selected_window);
        }
        let _ = hook_runtime::run_hook_value(eval, hook_sym, global_hook_value, &[], false)?;
        Ok(Value::NIL)
    })();

    restore_hook_caller_context(eval, saved);
    result
}

pub(crate) fn builtin_run_window_scroll_functions(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("run-window-scroll-functions", &args, 1)?;
    if let Some(window) = args.first() {
        expect_optional_live_window_designator(window, eval)?;
    }

    let window_arg = match args.first().copied().unwrap_or(Value::Nil).kind() {
        ValueKind::Nil => super::window_cmds::builtin_selected_window(eval, vec![])?,
        value => value,
    };
    if !window_arg.is_window() /* TODO(tagged): `window_raw_id` was Value::Window(window_raw_id), rewrite let-else */ {
        return Ok(Value::NIL);
    };
    let window_id = crate::window::WindowId(window_raw_id);
    let frame_id = eval.frames.find_window_frame_id(window_id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("window-live-p"), window_arg],
        )
    })?;
    let window_start = super::window_cmds::builtin_window_start(eval, vec![window_arg])?;
    let hook_sym = hook_runtime::hook_symbol_by_name(eval, "window-scroll-functions");
    let hook_value = hook_runtime::hook_value_by_id(eval, hook_sym).unwrap_or(Value::NIL);
    let saved_buffer_id = eval.buffers.current_buffer_id();
    if let Some(buffer_id) = window_buffer_id_in_state(eval, frame_id, window_id) {
        let _ = eval.switch_current_buffer(buffer_id);
    }
    let result = hook_runtime::run_hook_value(
        eval,
        hook_sym,
        hook_value,
        &[window_arg, window_start],
        true,
    );
    if let Some(buffer_id) = saved_buffer_id {
        eval.restore_current_buffer_if_live(buffer_id);
    }
    result
}

pub(crate) fn builtin_featurep(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("featurep", &args, 1)?;
    expect_max_args("featurep", &args, 2)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    if !crate::emacs_core::eval::feature_present_in_state(&eval.obarray, &mut eval.features, name) {
        return Ok(Value::NIL);
    }

    let Some(subfeature) = args.get(1) else {
        return Ok(Value::T);
    };
    if subfeature.is_nil() {
        return Ok(Value::T);
    }

    let subfeatures = eval
        .obarray
        .get_property(name, "subfeatures")
        .cloned()
        .unwrap_or(Value::NIL);
    let items = list_to_vec(&subfeatures).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), subfeatures],
        )
    })?;
    Ok(Value::bool_val(items.iter().any(|item| item == subfeature)))
}
