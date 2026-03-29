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
    let hook_value = hook_runtime::hook_value_by_id(eval, hook_sym).unwrap_or(Value::Nil);
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
    if let Value::Frame(id) = value {
        if frames.get(crate::window::FrameId(*id)).is_some() {
            return Ok(());
        }
    }
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("frame-live-p"), *value],
    ))
}

pub(super) fn expect_optional_live_window_designator(
    value: &Value,
    eval: &super::eval::Context,
) -> Result<(), Flow> {
    if value.is_nil() {
        return Ok(());
    }
    if let Value::Window(id) = value {
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
    let Value::Vector(data) = value else {
        return None;
    };
    let items = with_heap(|h| h.get_vector(*data).clone());
    if items.len() != 3 || items[0].as_symbol_name() != Some(WINDOW_CONFIGURATION_TAG) {
        return None;
    }
    match (&items[1], &items[2]) {
        (Value::Frame(_), Value::Int(serial)) => Some((items[1], *serial)),
        _ => None,
    }
}

fn window_configuration_frame_from_value(value: &Value) -> Option<Value> {
    window_configuration_parts_from_value(value).map(|(frame, _)| frame)
}

fn next_window_configuration_serial() -> i64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_WINDOW_CONFIGURATION_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_WINDOW_CONFIGURATION_ID.fetch_add(1, Ordering::Relaxed) as i64
}

fn make_window_configuration_value(frame: Value, serial: i64) -> Value {
    Value::vector(vec![
        Value::symbol(WINDOW_CONFIGURATION_TAG),
        frame,
        Value::Int(serial),
    ])
}

pub(crate) fn builtin_window_configuration_p(args: Vec<Value>) -> EvalResult {
    expect_args("window-configuration-p", &args, 1)?;
    Ok(Value::bool(
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
    Ok(Value::bool(equal_value(&args[0], &args[1], 0)))
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

    let Value::Frame(frame_raw_id) = frame else {
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
    Ok(Value::True)
}

fn save_selected_window_state_from_value(
    value: &Value,
) -> Option<(Value, Value, Option<crate::buffer::BufferId>)> {
    let Value::Vector(data) = value else {
        return None;
    };
    let items = with_heap(|h| h.get_vector(*data).clone());
    if items.len() != 4 || items[0].as_symbol_name() != Some(SAVE_SELECTED_WINDOW_STATE_TAG) {
        return None;
    }
    let frame = items[1];
    let window = items[2];
    let buffer_id = match items[3] {
        Value::Buffer(id) => Some(id),
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
        .map(|buffer| Value::Buffer(buffer.id))
        .unwrap_or(Value::Nil);
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
                args.first().cloned().unwrap_or(Value::Nil),
            ],
        ));
    };

    let _ = super::window_cmds::builtin_select_frame(eval, vec![saved_frame, Value::Nil]);
    let _ = super::window_cmds::builtin_select_window(eval, vec![saved_window, Value::Nil]);
    if let Some(buffer_id) = saved_buffer {
        if eval.buffers.get(buffer_id).is_some() {
            eval.restore_current_buffer_if_live(buffer_id);
        }
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_run_window_configuration_change_hook(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("run-window-configuration-change-hook", &args, 1)?;
    if let Some(frame) = args.first() {
        expect_optional_live_frame_designator(frame, eval)?;
    }
    let hook_name = "window-configuration-change-hook";
    let hook_sym = hook_runtime::hook_symbol_by_name(eval, hook_name);
    let hook_value = hook_runtime::hook_value_by_id(eval, hook_sym).unwrap_or(Value::Nil);
    hook_runtime::run_hook_value(eval, hook_sym, hook_value, &[], true)
}

pub(crate) fn builtin_run_window_scroll_functions(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("run-window-scroll-functions", &args, 1)?;
    if let Some(window) = args.first() {
        expect_optional_live_window_designator(window, eval)?;
    }

    let window_arg = args.first().cloned().unwrap_or(Value::Nil);
    let window_start = if window_arg.is_nil() {
        super::window_cmds::builtin_window_start(eval, vec![])?
    } else {
        super::window_cmds::builtin_window_start(eval, vec![window_arg])?
    };

    let hook_name = "window-scroll-functions";
    let hook_sym = hook_runtime::hook_symbol_by_name(eval, hook_name);
    let hook_value = hook_runtime::hook_value_by_id(eval, hook_sym).unwrap_or(Value::Nil);
    hook_runtime::run_hook_value(
        eval,
        hook_sym,
        hook_value,
        &[window_arg, window_start],
        true,
    )
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
        return Ok(Value::Nil);
    }

    let Some(subfeature) = args.get(1) else {
        return Ok(Value::True);
    };
    if subfeature.is_nil() {
        return Ok(Value::True);
    }

    let subfeatures = eval
        .obarray
        .get_property(name, "subfeatures")
        .cloned()
        .unwrap_or(Value::Nil);
    let items = list_to_vec(&subfeatures).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), subfeatures],
        )
    })?;
    Ok(Value::bool(items.iter().any(|item| item == subfeature)))
}
