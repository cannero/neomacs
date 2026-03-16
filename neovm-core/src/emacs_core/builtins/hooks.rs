use super::*;

// ===========================================================================
// Hook system (need evaluator)
// ===========================================================================

fn symbol_dynamic_buffer_or_global_value(
    eval: &super::eval::Evaluator,
    name: &str,
) -> Option<Value> {
    let name_id = intern(name);
    for frame in eval.dynamic.iter().rev() {
        if let Some(value) = frame.get(&name_id) {
            return Some(*value);
        }
    }
    if let Some(buf) = eval.buffers.current_buffer() {
        if let Some(value) = buf.get_buffer_local(name) {
            return Some(*value);
        }
    }
    eval.obarray().symbol_value(name).cloned()
}

enum HookControl {
    Continue,
    Return(Value),
}

fn walk_hook_value_with<F>(
    eval: &mut super::eval::Evaluator,
    hook_name: &str,
    hook_value: Value,
    inherit_global: bool,
    callback: &mut F,
) -> Result<HookControl, Flow>
where
    F: FnMut(&mut super::eval::Evaluator, Value) -> Result<HookControl, Flow>,
{
    match hook_value {
        Value::Nil => Ok(HookControl::Continue),
        Value::Cons(_) => {
            // Oracle-compatible traversal: iterate cons cells, ignore improper
            // list tails, and treat `t` as "also run the global value".
            let mut cursor = hook_value;
            let mut saw_global_marker = false;
            while let Value::Cons(cell) = cursor {
                let (func, next) = {
                    let pair = read_cons(cell);
                    (pair.car, pair.cdr)
                };
                if func.as_symbol_name() == Some("t") {
                    saw_global_marker = true;
                } else {
                    match callback(eval, func)? {
                        HookControl::Continue => {}
                        HookControl::Return(value) => return Ok(HookControl::Return(value)),
                    }
                }
                cursor = next;
            }

            if saw_global_marker && inherit_global {
                let global_value = eval
                    .obarray()
                    .symbol_value(hook_name)
                    .cloned()
                    .unwrap_or(Value::Nil);
                return walk_hook_value_with(eval, hook_name, global_value, false, callback);
            }
            Ok(HookControl::Continue)
        }
        value => callback(eval, value),
    }
}

fn run_hook_value(
    eval: &mut super::eval::Evaluator,
    hook_name: &str,
    hook_value: Value,
    hook_args: &[Value],
    inherit_global: bool,
) -> Result<(), Flow> {
    let mut callback = |eval: &mut super::eval::Evaluator, value: Value| {
        eval.apply(value, hook_args.to_vec())?;
        Ok(HookControl::Continue)
    };
    match walk_hook_value_with(eval, hook_name, hook_value, inherit_global, &mut callback)? {
        HookControl::Continue | HookControl::Return(_) => Ok(()),
    }
}

pub(crate) fn builtin_run_hooks(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    for hook_sym in &args {
        let hook_name = hook_sym.as_symbol_name().ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), *hook_sym],
            )
        })?;
        let hook_value =
            symbol_dynamic_buffer_or_global_value(eval, hook_name).unwrap_or(Value::Nil);
        run_hook_value(eval, hook_name, hook_value, &[], true)?;
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_run_hook_with_args(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("run-hook-with-args", &args, 1)?;
    let hook_name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let hook_args: Vec<Value> = args[1..].to_vec();
    let hook_value = symbol_dynamic_buffer_or_global_value(eval, hook_name).unwrap_or(Value::Nil);
    run_hook_value(eval, hook_name, hook_value, &hook_args, true)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_run_hook_with_args_until_success(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("run-hook-with-args-until-success", &args, 1)?;
    let hook_name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let hook_args: Vec<Value> = args[1..].to_vec();
    let hook_value = symbol_dynamic_buffer_or_global_value(eval, hook_name).unwrap_or(Value::Nil);
    let mut callback = |eval: &mut super::eval::Evaluator, func: Value| {
        let value = eval.apply(func, hook_args.clone())?;
        if value.is_truthy() {
            Ok(HookControl::Return(value))
        } else {
            Ok(HookControl::Continue)
        }
    };
    match walk_hook_value_with(eval, hook_name, hook_value, true, &mut callback)? {
        HookControl::Continue => Ok(Value::Nil),
        HookControl::Return(value) => Ok(value),
    }
}

pub(crate) fn builtin_run_hook_with_args_until_failure(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("run-hook-with-args-until-failure", &args, 1)?;
    let hook_name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let hook_args: Vec<Value> = args[1..].to_vec();
    let hook_value = symbol_dynamic_buffer_or_global_value(eval, hook_name).unwrap_or(Value::Nil);
    let mut callback = |eval: &mut super::eval::Evaluator, func: Value| {
        let value = eval.apply(func, hook_args.clone())?;
        if value.is_nil() {
            Ok(HookControl::Return(Value::Nil))
        } else {
            Ok(HookControl::Continue)
        }
    };
    match walk_hook_value_with(eval, hook_name, hook_value, true, &mut callback)? {
        HookControl::Continue => Ok(Value::True),
        HookControl::Return(value) => Ok(value),
    }
}

pub(crate) fn builtin_run_hook_wrapped(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("run-hook-wrapped", &args, 2)?;
    let hook_name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let wrapper = args[1];
    let wrapped_args: Vec<Value> = args[2..].to_vec();
    let hook_value = symbol_dynamic_buffer_or_global_value(eval, hook_name).unwrap_or(Value::Nil);
    let mut callback = |eval: &mut super::eval::Evaluator, func: Value| {
        let mut call_args = Vec::with_capacity(wrapped_args.len() + 1);
        call_args.push(func);
        call_args.extend(wrapped_args.clone());
        eval.apply(wrapper, call_args)?;
        Ok(HookControl::Continue)
    };
    let _ = walk_hook_value_with(eval, hook_name, hook_value, true, &mut callback)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_run_hook_query_error_with_timeout(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("run-hook-query-error-with-timeout", &args, 1)?;
    let hook_name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let hook_value = symbol_dynamic_buffer_or_global_value(eval, hook_name).unwrap_or(Value::Nil);
    match run_hook_value(eval, hook_name, hook_value, &[], true) {
        Ok(()) => Ok(Value::Nil),
        Err(Flow::Signal(_)) => Err(signal(
            "end-of-file",
            vec![Value::string("Error reading from stdin")],
        )),
        Err(flow) => Err(flow),
    }
}

fn expect_optional_live_frame_designator(
    value: &Value,
    eval: &super::eval::Evaluator,
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
    eval: &super::eval::Evaluator,
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

pub(super) fn builtin_window_configuration_p(args: Vec<Value>) -> EvalResult {
    expect_args("window-configuration-p", &args, 1)?;
    Ok(Value::bool(
        window_configuration_frame_from_value(&args[0]).is_some(),
    ))
}

pub(super) fn builtin_window_configuration_frame(args: Vec<Value>) -> EvalResult {
    expect_args("window-configuration-frame", &args, 1)?;
    window_configuration_frame_from_value(&args[0]).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("window-configuration-p"), args[0]],
        )
    })
}

pub(super) fn builtin_window_configuration_equal_p(args: Vec<Value>) -> EvalResult {
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
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_current_window_configuration_in_state(&mut eval.frames, &mut eval.buffers, args)
}

pub(crate) fn builtin_current_window_configuration_in_state(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("current-window-configuration", &args, 1)?;

    let frame = if let Some(frame) = args.first() {
        expect_optional_live_frame_designator_in_state(frame, frames)?;
        if frame.is_nil() {
            super::window_cmds::builtin_selected_frame_in_state(frames, buffers, vec![])?
        } else {
            *frame
        }
    } else {
        super::window_cmds::builtin_selected_frame_in_state(frames, buffers, vec![])?
    };

    let Value::Frame(frame_raw_id) = frame else {
        return Ok(make_window_configuration_value(
            frame,
            next_window_configuration_serial(),
        ));
    };
    let frame_id = crate::window::FrameId(frame_raw_id);
    if let Some(frame_state) = frames.get(frame_id) {
        let snapshot = WindowConfigurationSnapshot {
            frame_id,
            root_window: frame_state.root_window.clone(),
            selected_window: frame_state.selected_window,
            minibuffer_window: frame_state.minibuffer_window,
            minibuffer_leaf: frame_state.minibuffer_leaf.clone(),
        };
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
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_set_window_configuration_in_state(&mut eval.frames, &mut eval.buffers, args)
}

pub(crate) fn builtin_set_window_configuration_in_state(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
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
        let selected_buffer = if let Some(frame) = frames.get_mut(snapshot.frame_id) {
            frame.root_window = snapshot.root_window;
            frame.selected_window = snapshot.selected_window;
            frame.minibuffer_window = snapshot.minibuffer_window;
            frame.minibuffer_leaf = snapshot.minibuffer_leaf;
            frame
                .find_window(frame.selected_window)
                .and_then(|w| w.buffer_id())
        } else {
            None
        };
        if let Some(buffer_id) = selected_buffer {
            buffers.set_current(buffer_id);
        }
    }

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
    eval: &mut super::eval::Evaluator,
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
    eval: &mut super::eval::Evaluator,
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
            eval.buffers.set_current(buffer_id);
        }
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_run_window_configuration_change_hook(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("run-window-configuration-change-hook", &args, 1)?;
    if let Some(frame) = args.first() {
        expect_optional_live_frame_designator(frame, eval)?;
    }
    let hook_name = "window-configuration-change-hook";
    let hook_value = symbol_dynamic_buffer_or_global_value(eval, hook_name).unwrap_or(Value::Nil);
    run_hook_value(eval, hook_name, hook_value, &[], true)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_run_window_scroll_functions(
    eval: &mut super::eval::Evaluator,
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
    let hook_value = symbol_dynamic_buffer_or_global_value(eval, hook_name).unwrap_or(Value::Nil);
    run_hook_value(
        eval,
        hook_name,
        hook_value,
        &[window_arg, window_start],
        true,
    )?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_featurep(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("featurep", &args, 1)?;
    expect_max_args("featurep", &args, 2)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    if !eval.feature_present(name) {
        return Ok(Value::Nil);
    }

    let Some(subfeature) = args.get(1) else {
        return Ok(Value::True);
    };
    if subfeature.is_nil() {
        return Ok(Value::True);
    }

    let subfeatures = eval
        .obarray()
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
