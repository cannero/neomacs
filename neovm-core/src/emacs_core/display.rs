//! Frame/display property builtins.
//!
//! Provides stub implementations for display and terminal query functions.
//! Since Neomacs is always a GUI application, most display queries return
//! sensible defaults for a modern graphical display.

use super::error::{EvalResult, Flow, signal};
use super::intern::intern;
use super::terminal::pure::{
    is_terminal_handle, make_alist, terminal_designator_p, terminal_handle_id,
    terminal_runtime_color_cells, terminal_runtime_supports_color,
};
use super::value::*;
use crate::window::{FrameId, WindowId};

/// Clear cached thread-local display values (must be called when heap changes).
pub fn reset_display_thread_locals() {
    super::terminal::pure::reset_terminal_thread_locals();
    super::dispnew::pure::reset_dispnew_thread_locals();
}

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

pub(crate) fn expect_max_args(name: &str, args: &[Value], max: usize) -> Result<(), Flow> {
    if args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn expect_range_args(
    name: &str,
    args: &[Value],
    min: usize,
    max: usize,
) -> Result<(), Flow> {
    if args.len() < min || args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn expect_symbol_key(value: &Value) -> Result<Value, Flow> {
    match value {
        Value::Nil | Value::True | Value::Symbol(_) | Value::Keyword(_) => Ok(*value),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), *other],
        )),
    }
}

fn dynamic_or_global_symbol_value(eval: &super::eval::Context, name: &str) -> Option<Value> {
    eval.obarray.symbol_value(name).cloned()
}

fn dynamic_or_global_symbol_value_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    _dynamic: &[crate::emacs_core::value::OrderedRuntimeBindingMap],
    name: &str,
) -> Option<Value> {
    obarray.symbol_value(name).cloned()
}

fn global_window_system_symbol(eval: &super::eval::Context) -> Option<Value> {
    dynamic_or_global_symbol_value(eval, "initial-window-system")
        .filter(|value| !value.is_nil())
        .or_else(|| dynamic_or_global_symbol_value(eval, "window-system"))
}

fn global_window_system_symbol_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[crate::emacs_core::value::OrderedRuntimeBindingMap],
) -> Option<Value> {
    dynamic_or_global_symbol_value_in_state(obarray, dynamic, "initial-window-system")
        .filter(|value| !value.is_nil())
        .or_else(|| dynamic_or_global_symbol_value_in_state(obarray, dynamic, "window-system"))
}

fn selected_frame_window_system_symbol(eval: &super::eval::Context) -> Option<Value> {
    eval.frames
        .selected_frame()
        .and_then(|frame| frame.effective_window_system())
}

fn selected_frame_window_system_symbol_in_state(
    frames: &crate::window::FrameManager,
) -> Option<Value> {
    frames
        .selected_frame()
        .and_then(|frame| frame.effective_window_system())
}

pub(crate) fn live_frame_designator_p_in_state(
    frames: &crate::window::FrameManager,
    value: &Value,
) -> bool {
    match value {
        Value::Int(id) if *id >= 0 => frames.get(FrameId(*id as u64)).is_some(),
        Value::Frame(id) => frames.get(FrameId(*id)).is_some(),
        _ => false,
    }
}

fn frame_window_system_symbol(
    eval: &mut super::eval::Context,
    frame: Option<&Value>,
) -> Result<Option<Value>, Flow> {
    frame_window_system_symbol_in_state(&mut eval.frames, &mut eval.buffers, frame)
}

fn frame_window_system_symbol_in_state(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    frame: Option<&Value>,
) -> Result<Option<Value>, Flow> {
    let frame_id = super::window_cmds::resolve_frame_id_in_state(frames, buffers, frame, "framep")?;
    Ok(frames
        .get(frame_id)
        .and_then(|frame| frame.effective_window_system()))
}

fn invalid_get_device_terminal_error(value: &Value) -> Flow {
    signal(
        "error",
        vec![Value::string(format!(
            "Invalid argument {} in ‘get-device-terminal’",
            super::print::print_value(value)
        ))],
    )
}

fn display_does_not_exist_error(display: &str) -> Flow {
    signal(
        "error",
        vec![Value::string(format!("Display {display} does not exist"))],
    )
}

fn format_get_device_terminal_arg_eval(eval: &super::eval::Context, value: &Value) -> String {
    let window_id = match value {
        Value::Window(id) => Some(WindowId(*id)),
        _ => None,
    };

    if let Some(window_id) = window_id {
        if let Some(frame_id) = eval.frames.find_window_frame_id(window_id) {
            if let Some(frame) = eval.frames.get(frame_id) {
                if let Some(window) = frame.find_window(window_id) {
                    if let Some(buffer_id) = window.buffer_id() {
                        if let Some(buffer) = eval.buffers.get(buffer_id) {
                            return format!("#<window {} on {}>", window_id.0, buffer.name);
                        }
                    }
                    return format!("#<window {} on {}>", window_id.0, frame.name);
                }
            }
        }
    }

    super::print::print_value(value)
}

fn invalid_get_device_terminal_error_eval(eval: &super::eval::Context, value: &Value) -> Flow {
    signal(
        "error",
        vec![Value::string(format!(
            "Invalid argument {} in ‘get-device-terminal’",
            format_get_device_terminal_arg_eval(eval, value)
        ))],
    )
}

fn terminal_not_x_display_error(value: &Value) -> Option<Flow> {
    terminal_handle_id(value).map(|id| {
        signal(
            "error",
            vec![Value::string(format!("Terminal {id} is not an X display"))],
        )
    })
}

pub(crate) fn expect_frame_designator(value: &Value) -> Result<(), Flow> {
    match value {
        Value::Int(id) if *id >= 0 => Ok(()),
        Value::Frame(_) => Ok(()),
        v if v.is_nil() => Ok(()),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), *value],
        )),
    }
}

fn expect_display_designator(value: &Value) -> Result<(), Flow> {
    match value {
        Value::Nil => Ok(()),
        display if terminal_designator_p(display) => Ok(()),
        Value::Str(_) => {
            let display = value.as_str().unwrap();
            Err(display_does_not_exist_error(display))
        }
        _ => Err(invalid_get_device_terminal_error(value)),
    }
}

pub(crate) fn expect_display_designator_in_state(
    frames: &crate::window::FrameManager,
    value: &Value,
) -> Result<(), Flow> {
    match value {
        Value::Nil => Ok(()),
        display if terminal_designator_p(display) => Ok(()),
        display if live_frame_designator_p_in_state(frames, display) => Ok(()),
        Value::Str(_) => {
            let display = value.as_str().unwrap();
            Err(display_does_not_exist_error(display))
        }
        _ => Err(invalid_get_device_terminal_error(value)),
    }
}

pub(crate) fn live_frame_designator_p(eval: &mut super::eval::Context, value: &Value) -> bool {
    live_frame_designator_p_in_state(&eval.frames, value)
}

fn expect_display_designator_eval(
    eval: &mut super::eval::Context,
    value: &Value,
) -> Result<(), Flow> {
    if value.is_nil() || terminal_designator_p(value) || live_frame_designator_p(eval, value) {
        return Ok(());
    }
    if let Value::Str(_) = value {
        let display = value.as_str().unwrap();
        return Err(display_does_not_exist_error(display));
    }
    Err(invalid_get_device_terminal_error_eval(eval, value))
}

fn expect_optional_display_designator_eval(
    eval: &mut super::eval::Context,
    name: &str,
    args: &[Value],
) -> Result<(), Flow> {
    expect_max_args(name, args, 1)?;
    if let Some(display) = args.first() {
        expect_display_designator_eval(eval, display)?;
    }
    Ok(())
}

fn frame_not_live_error(value: &Value) -> Flow {
    let printable = match value {
        Value::Str(_) => value.as_str().unwrap().to_string(),
        _ => super::print::print_value(value),
    };
    signal(
        "error",
        vec![Value::string(format!("{printable} is not a live frame"))],
    )
}

fn frame_not_live_error_eval(_eval: &super::eval::Context, value: &Value) -> Flow {
    let printable = match value {
        Value::Str(_) => value.as_str().unwrap().to_string(),
        _ => format_get_device_terminal_arg_eval(_eval, value),
    };
    signal(
        "error",
        vec![Value::string(format!("{printable} is not a live frame"))],
    )
}

fn x_windows_not_initialized_error() -> Flow {
    signal(
        "error",
        vec![Value::string("X windows are not in use or not initialized")],
    )
}

fn x_window_system_frame_error() -> Flow {
    signal(
        "error",
        vec![Value::string("Window system frame should be used")],
    )
}

fn x_selection_unavailable_error() -> Flow {
    signal(
        "error",
        vec![Value::string("X selection unavailable for this frame")],
    )
}

fn x_display_open_error(display: &str) -> Flow {
    signal(
        "error",
        vec![Value::string(format!("Display {display} can’t be opened"))],
    )
}

fn x_display_query_first_arg_error(value: &Value) -> Flow {
    match value {
        Value::Nil => x_windows_not_initialized_error(),
        Value::Str(_) => x_display_open_error(value.as_str().unwrap()),
        Value::Frame(_) => x_window_system_frame_error(),
        other => {
            if let Some(err) = terminal_not_x_display_error(other) {
                err
            } else {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("frame-live-p"), *other],
                )
            }
        }
    }
}

fn window_system_not_initialized_error() -> Flow {
    signal(
        "error",
        vec![Value::string(
            "Window system is not in use or not initialized",
        )],
    )
}

pub fn gui_window_system_symbol() -> &'static str {
    "neo"
}

pub(crate) fn gui_window_system_active_value(value: Value) -> bool {
    value == Value::symbol(gui_window_system_symbol()) || value == Value::symbol("x")
}

pub(crate) fn x_window_system_active(eval: &super::eval::Context) -> bool {
    let host_window_system =
        selected_frame_window_system_symbol(eval).or_else(|| global_window_system_symbol(eval));
    host_window_system.is_some_and(gui_window_system_active_value)
}

pub(crate) fn x_window_system_active_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[crate::emacs_core::value::OrderedRuntimeBindingMap],
) -> bool {
    let host_window_system = global_window_system_symbol_in_state(obarray, dynamic);
    host_window_system.is_some_and(gui_window_system_active_value)
}

pub(crate) fn display_window_system_symbol_eval(
    eval: &mut super::eval::Context,
    display: Option<&Value>,
) -> Result<Option<Value>, Flow> {
    match display {
        None | Some(Value::Nil) => {
            Ok(selected_frame_window_system_symbol(eval)
                .or_else(|| global_window_system_symbol(eval)))
        }
        Some(display) if terminal_designator_p(display) => Ok(None),
        Some(display) if live_frame_designator_p(eval, display) => {
            frame_window_system_symbol(eval, Some(display))
        }
        Some(Value::Str(_)) => Err(display_does_not_exist_error(
            display.unwrap().as_str().unwrap(),
        )),
        Some(other) => Err(invalid_get_device_terminal_error_eval(eval, other)),
    }
}

fn frame_window_system_symbol_read_only_in_state(
    frames: &crate::window::FrameManager,
    frame: Option<&Value>,
) -> Result<Option<Value>, Flow> {
    match frame {
        None | Some(Value::Nil) => Ok(selected_frame_window_system_symbol_in_state(frames)),
        Some(Value::Int(id)) if *id >= 0 => Ok(frames
            .get(FrameId(*id as u64))
            .and_then(|frame| frame.effective_window_system())),
        Some(Value::Frame(id)) => Ok(frames
            .get(FrameId(*id))
            .and_then(|frame| frame.effective_window_system())),
        Some(other) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("framep"), *other],
        )),
    }
}

pub(crate) fn display_window_system_symbol_in_state(
    frames: &crate::window::FrameManager,
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[crate::emacs_core::value::OrderedRuntimeBindingMap],
    display: Option<&Value>,
) -> Result<Option<Value>, Flow> {
    match display {
        None | Some(Value::Nil) => Ok(frame_window_system_symbol_read_only_in_state(
            frames, display,
        )?
        .or_else(|| global_window_system_symbol_in_state(obarray, dynamic))),
        Some(display) if terminal_designator_p(display) => Ok(None),
        Some(display) if live_frame_designator_p_in_state(frames, display) => {
            frame_window_system_symbol_read_only_in_state(frames, Some(display))
        }
        Some(Value::Str(_)) => Err(display_does_not_exist_error(
            display.unwrap().as_str().unwrap(),
        )),
        Some(other) => Err(invalid_get_device_terminal_error(other)),
    }
}

const GUI_X_DISPLAY_PLANES: i64 = 24;
const GUI_X_DISPLAY_COLOR_CELLS: i64 = 16_777_216;
const GUI_X_VISUAL_CLASS: &str = "true-color";

fn gui_x_query_target_eval(
    eval: &mut super::eval::Context,
    name: &str,
    args: &[Value],
) -> Result<bool, Flow> {
    expect_max_args(name, args, 1)?;
    if !display_window_system_symbol_eval(eval, args.first())?
        .is_some_and(gui_window_system_active_value)
    {
        return Ok(false);
    }
    Ok(match args.first() {
        None | Some(Value::Nil) => true,
        Some(display) => live_frame_designator_p(eval, display),
    })
}

fn gui_x_query_target_in_state(
    frames: &crate::window::FrameManager,
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[crate::emacs_core::value::OrderedRuntimeBindingMap],
    name: &str,
    args: &[Value],
) -> Result<bool, Flow> {
    expect_max_args(name, args, 1)?;
    if !display_window_system_symbol_in_state(frames, obarray, dynamic, args.first())?
        .is_some_and(gui_window_system_active_value)
    {
        return Ok(false);
    }
    Ok(match args.first() {
        None | Some(Value::Nil) => true,
        Some(display) => live_frame_designator_p_in_state(frames, display),
    })
}

fn expect_optional_window_system_frame_arg(value: &Value) -> Result<(), Flow> {
    if value.is_nil() || matches!(value, Value::Frame(_)) {
        Ok(())
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), *value],
        ))
    }
}

fn expect_optional_window_system_frame_arg_in_state(
    frames: &crate::window::FrameManager,
    value: &Value,
) -> Result<(), Flow> {
    if value.is_nil()
        || matches!(value, Value::Frame(_))
        || live_frame_designator_p_in_state(frames, value)
    {
        Ok(())
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), *value],
        ))
    }
}

fn parse_geometry_unsigned(bytes: &[u8], index: &mut usize) -> Option<i64> {
    let start = *index;
    while *index < bytes.len() && bytes[*index].is_ascii_digit() {
        *index += 1;
    }
    if *index == start {
        return None;
    }
    std::str::from_utf8(&bytes[start..*index])
        .ok()?
        .parse::<i64>()
        .ok()
}

fn parse_geometry_signed_offset(bytes: &[u8], index: &mut usize) -> Option<i64> {
    if *index >= bytes.len() {
        return None;
    }
    let sign = match bytes[*index] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    *index += 1;
    Some(sign * parse_geometry_unsigned(bytes, index)?)
}

fn parse_x_geometry(spec: &str) -> Option<Value> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }

    let bytes = spec.as_bytes();
    let mut index = 0usize;
    if bytes[index] == b'=' {
        index += 1;
        if index >= bytes.len() {
            return None;
        }
    }

    let mut width = None;
    let mut height = None;
    let mut left = None;
    let mut top = None;

    let geometry_start = index;
    if let Some(parsed_width) = parse_geometry_unsigned(bytes, &mut index) {
        if index < bytes.len() && bytes[index] == b'x' {
            index += 1;
            let parsed_height = parse_geometry_unsigned(bytes, &mut index)?;
            width = Some(parsed_width);
            height = Some(parsed_height);
        } else {
            index = geometry_start;
        }
    } else if index < bytes.len() && bytes[index] == b'x' {
        return None;
    }

    if index < bytes.len() {
        let parsed_left = parse_geometry_signed_offset(bytes, &mut index)?;
        left = Some(parsed_left);
        if index < bytes.len() {
            let parsed_top = parse_geometry_signed_offset(bytes, &mut index)?;
            top = Some(parsed_top);
        }
    }

    if index != bytes.len() {
        return None;
    }

    if width.is_none() && height.is_none() && left.is_none() && top.is_none() {
        return None;
    }

    let mut pairs = Vec::new();
    if let Some(h) = height {
        pairs.push(Value::cons(Value::symbol("height"), Value::Int(h)));
    }
    if let Some(w) = width {
        pairs.push(Value::cons(Value::symbol("width"), Value::Int(w)));
    }
    if let Some(y) = top {
        pairs.push(Value::cons(Value::symbol("top"), Value::Int(y)));
    }
    if let Some(x) = left {
        pairs.push(Value::cons(Value::symbol("left"), Value::Int(x)));
    }
    Some(Value::list(pairs))
}

fn display_optional_capability_p(name: &str, args: &[Value]) -> EvalResult {
    expect_max_args(name, args, 1)?;
    match args.first() {
        None | Some(Value::Nil) => Ok(Value::Nil),
        Some(display) if is_terminal_handle(display) => Ok(Value::Nil),
        Some(Value::Str(_)) => {
            let display = args[0].as_str().unwrap();
            Err(signal(
                "error",
                vec![Value::string(format!("Display {display} does not exist"))],
            ))
        }
        Some(other) => Err(invalid_get_device_terminal_error(other)),
    }
}

fn display_optional_capability_p_eval(
    eval: &mut super::eval::Context,
    name: &str,
    args: &[Value],
) -> EvalResult {
    expect_max_args(name, args, 1)?;
    match args.first() {
        None | Some(Value::Nil) => Ok(Value::Nil),
        Some(display) if is_terminal_handle(display) => Ok(Value::Nil),
        Some(display) if live_frame_designator_p(eval, display) => Ok(Value::Nil),
        Some(Value::Str(_)) => {
            let display = args[0].as_str().unwrap();
            Err(signal(
                "error",
                vec![Value::string(format!("Display {display} does not exist"))],
            ))
        }
        Some(other) => Err(invalid_get_device_terminal_error_eval(eval, other)),
    }
}

fn x_optional_display_query_error(name: &str, args: &[Value]) -> EvalResult {
    expect_max_args(name, args, 1)?;
    match args.first() {
        None | Some(Value::Nil) => Err(x_windows_not_initialized_error()),
        Some(display) if is_terminal_handle(display) => {
            if let Some(err) = terminal_not_x_display_error(display) {
                Err(err)
            } else {
                Err(invalid_get_device_terminal_error(display))
            }
        }
        Some(Value::Str(_)) => {
            let display = args[0].as_str().unwrap();
            Err(signal(
                "error",
                vec![Value::string(format!("Display {display} can’t be opened"))],
            ))
        }
        Some(other) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), *other],
        )),
    }
}

fn x_optional_display_query_error_eval(
    eval: &mut super::eval::Context,
    name: &str,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args(name, &args, 1)?;
    if let Some(display) = args.first() {
        if live_frame_designator_p(eval, display) {
            return Err(x_window_system_frame_error());
        }
    }
    x_optional_display_query_error(name, &args)
}

pub(crate) fn x_optional_display_query_error_in_state(
    frames: &crate::window::FrameManager,
    name: &str,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args(name, &args, 1)?;
    if let Some(display) = args.first() {
        if live_frame_designator_p_in_state(frames, display) {
            return Err(x_window_system_frame_error());
        }
    }
    x_optional_display_query_error(name, &args)
}

// ---------------------------------------------------------------------------
// Display query builtins
// ---------------------------------------------------------------------------

/// Context-aware variant of `display-graphic-p`.
pub(crate) fn builtin_display_graphic_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_optional_display_designator_eval(eval, "display-graphic-p", &args)?;
    Ok(Value::bool(
        display_window_system_symbol_eval(eval, args.first())?
            .is_some_and(|value| value.is_symbol()),
    ))
}

/// Context-aware variant of `display-grayscale-p`.
pub(crate) fn builtin_display_grayscale_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    display_optional_capability_p_eval(eval, "display-grayscale-p", &args)
}

/// Context-aware variant of `display-mouse-p`.
pub(crate) fn builtin_display_mouse_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    display_optional_capability_p_eval(eval, "display-mouse-p", &args)
}

/// Context-aware variant of `display-popup-menus-p`.
pub(crate) fn builtin_display_popup_menus_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    display_optional_capability_p_eval(eval, "display-popup-menus-p", &args)
}

/// Context-aware variant of `display-symbol-keys-p`.
pub(crate) fn builtin_display_symbol_keys_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    display_optional_capability_p_eval(eval, "display-symbol-keys-p", &args)
}

/// Context-aware variant of `display-pixel-width`.
pub(crate) fn builtin_display_pixel_width(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_optional_display_designator_eval(eval, "display-pixel-width", &args)?;
    Ok(Value::Int(80))
}

/// Context-aware variant of `display-pixel-height`.
pub(crate) fn builtin_display_pixel_height(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_optional_display_designator_eval(eval, "display-pixel-height", &args)?;
    Ok(Value::Int(25))
}

/// Context-aware variant of `display-mm-width`.
pub(crate) fn builtin_display_mm_width(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_optional_display_designator_eval(eval, "display-mm-width", &args)?;
    Ok(Value::Nil)
}

/// Context-aware variant of `display-mm-height`.
pub(crate) fn builtin_display_mm_height(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_optional_display_designator_eval(eval, "display-mm-height", &args)?;
    Ok(Value::Nil)
}

/// Context-aware variant of `display-screens`.
pub(crate) fn builtin_display_screens(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_optional_display_designator_eval(eval, "display-screens", &args)?;
    Ok(Value::Int(1))
}

/// Context-aware variant of `display-color-cells`.
pub(crate) fn builtin_display_color_cells(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_optional_display_designator_eval(eval, "display-color-cells", &args)?;
    if display_window_system_symbol_eval(eval, args.first())?
        .is_some_and(gui_window_system_active_value)
    {
        Ok(Value::Int(16777216)) // 2^24 = 24-bit TrueColor
    } else if terminal_runtime_supports_color() {
        Ok(Value::Int(terminal_runtime_color_cells()))
    } else {
        Ok(Value::Int(0))
    }
}

/// Context-aware variant of `display-planes`.
pub(crate) fn builtin_display_planes(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_optional_display_designator_eval(eval, "display-planes", &args)?;
    if display_window_system_symbol_eval(eval, args.first())?
        .is_some_and(gui_window_system_active_value)
    {
        Ok(Value::Int(24))
    } else if terminal_runtime_supports_color() {
        Ok(Value::Int(if terminal_runtime_color_cells() >= 16777216 {
            24
        } else {
            8
        }))
    } else {
        Ok(Value::Int(3))
    }
}

/// Context-aware variant of `display-visual-class`.
pub(crate) fn builtin_display_visual_class(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_optional_display_designator_eval(eval, "display-visual-class", &args)?;
    if display_window_system_symbol_eval(eval, args.first())?
        .is_some_and(gui_window_system_active_value)
    {
        Ok(Value::symbol("true-color"))
    } else if terminal_runtime_supports_color() {
        Ok(Value::symbol("color"))
    } else {
        Ok(Value::symbol("static-gray"))
    }
}

/// Context-aware variant of `display-backing-store`.
pub(crate) fn builtin_display_backing_store(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_optional_display_designator_eval(eval, "display-backing-store", &args)?;
    Ok(Value::symbol("not-useful"))
}

/// Context-aware variant of `display-save-under`.
pub(crate) fn builtin_display_save_under(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_optional_display_designator_eval(eval, "display-save-under", &args)?;
    Ok(Value::symbol("not-useful"))
}

/// Context-aware variant of `display-selections-p`.
pub(crate) fn builtin_display_selections_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_optional_display_designator_eval(eval, "display-selections-p", &args)?;
    Ok(Value::Nil)
}

/// Context-aware variant of `window-system`.
pub(crate) fn builtin_window_system(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-system", &args, 1)?;
    match args.first() {
        None | Some(Value::Nil) => {
            if let Some(window_system) =
                selected_frame_window_system_symbol_in_state(&mut eval.frames)
            {
                return Ok(window_system);
            }
        }
        Some(_) => {
            if let Some(window_system) = frame_window_system_symbol_in_state(
                &mut eval.frames,
                &mut eval.buffers,
                args.first(),
            )? {
                return Ok(window_system);
            }
        }
    }
    Ok(
        dynamic_or_global_symbol_value_in_state(&eval.obarray, &[], "window-system")
            .unwrap_or(Value::Nil),
    )
}

/// Context-aware variant of `frame-edges`.
pub(crate) fn builtin_frame_edges(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_range_args("frame-edges", &args, 0, 2)?;
    if let Some(frame) = args.first() {
        if !frame.is_nil() && !live_frame_designator_p(eval, frame) {
            return Err(frame_not_live_error_eval(eval, frame));
        }
    }
    Ok(Value::list(vec![
        Value::Int(0),
        Value::Int(0),
        Value::Int(80),
        Value::Int(25),
    ]))
}

/// Context-aware variant of `display-images-p`.
pub(crate) fn builtin_display_images_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_optional_display_designator_eval(eval, "display-images-p", &args)?;
    Ok(Value::Nil)
}

/// Context-aware variant of `display-supports-face-attributes-p`.
///
/// Emacs accepts broad argument shapes here in batch mode and still returns
/// nil as long as arity is valid.
pub(crate) fn builtin_display_supports_face_attributes_p(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("display-supports-face-attributes-p", &args, 1, 2)?;
    Ok(Value::Nil)
}

// ---------------------------------------------------------------------------
// X display builtins (compatibility stubs)
// ---------------------------------------------------------------------------

/// (x-display-list) -> nil in batch-style vm context.
pub(crate) fn builtin_x_display_list(args: Vec<Value>) -> EvalResult {
    expect_max_args("x-display-list", &args, 0)?;
    Ok(Value::Nil)
}

/// (x-frame-edges &optional FRAME TYPE) -> nil in batch/no-X context.
pub(crate) fn builtin_x_frame_edges(args: Vec<Value>) -> EvalResult {
    expect_max_args("x-frame-edges", &args, 2)?;
    if let Some(frame) = args.first() {
        if !frame.is_nil() && !matches!(frame, Value::Frame(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *frame],
            ));
        }
    }
    Ok(Value::Nil)
}

/// (x-frame-geometry &optional FRAME) -> nil in batch/no-X context.
pub(crate) fn builtin_x_frame_geometry(args: Vec<Value>) -> EvalResult {
    expect_max_args("x-frame-geometry", &args, 1)?;
    if let Some(frame) = args.first() {
        if !frame.is_nil() && !matches!(frame, Value::Frame(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *frame],
            ));
        }
    }
    Ok(Value::Nil)
}

/// (x-frame-list-z-order &optional DISPLAY) -> error in batch/no-X context.
pub(crate) fn builtin_x_frame_list_z_order(args: Vec<Value>) -> EvalResult {
    expect_max_args("x-frame-list-z-order", &args, 1)?;
    match args.first() {
        None => Err(x_windows_not_initialized_error()),
        Some(display) => Err(x_display_query_first_arg_error(display)),
    }
}

/// (x-frame-restack FRAME1 FRAME2 &optional ABOVE) -> error in batch/no-X context.
///
/// Oracle batch behavior crashes on valid-arity runtime calls in this
/// environment, so we only expose arity/fboundp compatibility surface and a
/// conservative batch/no-X error result.
pub(crate) fn builtin_x_frame_restack(args: Vec<Value>) -> EvalResult {
    expect_range_args("x-frame-restack", &args, 2, 3)?;
    Err(x_window_system_frame_error())
}

/// (x-mouse-absolute-pixel-position) -> nil in batch/no-X context.
pub(crate) fn builtin_x_mouse_absolute_pixel_position(args: Vec<Value>) -> EvalResult {
    expect_args("x-mouse-absolute-pixel-position", &args, 0)?;
    Ok(Value::Nil)
}

/// (x-set-mouse-absolute-pixel-position X Y) -> nil in batch/no-X context.
pub(crate) fn builtin_x_set_mouse_absolute_pixel_position(args: Vec<Value>) -> EvalResult {
    expect_args("x-set-mouse-absolute-pixel-position", &args, 2)?;
    Ok(Value::Nil)
}

/// (x-send-client-message DISPLAY PROP VALUE-0 VALUE-1 VALUE-2 VALUE-3) -> error in batch/no-X context.
pub(crate) fn builtin_x_send_client_message(args: Vec<Value>) -> EvalResult {
    expect_args("x-send-client-message", &args, 6)?;
    Err(x_display_query_first_arg_error(&args[0]))
}

/// (x-popup-dialog POSITION CONTENTS &optional HEADER) -> nil/error in batch context.
pub(crate) fn builtin_x_popup_dialog(args: Vec<Value>) -> EvalResult {
    expect_range_args("x-popup-dialog", &args, 2, 3)?;

    if !matches!(args[0], Value::Frame(_)) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("windowp"), Value::Nil],
        ));
    }

    let contents = &args[1];
    if contents.is_nil() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), Value::Nil],
        ));
    }

    let (title, rest) = match contents {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            (pair.car, pair.cdr)
        }
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("listp"), *other],
            ));
        }
    };

    if !title.is_string() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), title],
        ));
    }

    if !rest.is_cons() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("consp"), rest],
        ));
    }

    Ok(Value::Nil)
}

/// (x-popup-menu POSITION MENU) -> nil/error in batch context.
pub(crate) fn builtin_x_popup_menu(args: Vec<Value>) -> EvalResult {
    expect_args("x-popup-menu", &args, 2)?;
    let position = &args[0];
    let menu = &args[1];

    if position.is_nil() {
        return Ok(Value::Nil);
    }

    let (position_car, position_cdr) = match position {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            (pair.car, pair.cdr)
        }
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("listp"), *other],
            ));
        }
    };

    if !position_car.is_list() {
        if matches!(position_car, Value::Int(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("listp"), position_car],
            ));
        }
        if menu.is_nil() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), Value::Nil],
            ));
        }
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("consp"), Value::True],
        ));
    }

    if !position_cdr.is_list() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), position_cdr],
        ));
    }

    if !position_car.is_nil() {
        let window_designator = match position_cdr {
            Value::Cons(cell) => {
                let pair = read_cons(cell);
                pair.car
            }
            _ => Value::Nil,
        };
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("windowp"), window_designator],
        ));
    }

    // This follows the menu descriptor shape expected by batch-mode oracle:
    // MENU = (TITLE . REST), REST either nil or (PANE . _)
    // PANE = (PANE-TITLE . PANE-ITEMS)
    if menu.is_nil() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), Value::Nil],
        ));
    }

    let (title, rest) = match menu {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            (pair.car, pair.cdr)
        }
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("listp"), *other],
            ));
        }
    };

    if !title.is_string() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), title],
        ));
    }

    if rest.is_nil() {
        return Ok(Value::Nil);
    }

    let pane = match rest {
        Value::Cons(cell) => {
            let pair = read_cons(cell);
            pair.car
        }
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("listp"), other],
            ));
        }
    };

    let (pane_title, pane_items) = match pane {
        Value::Cons(cell) => {
            let pair = read_cons(cell);
            (pair.car, pair.cdr)
        }
        Value::Nil => (Value::Nil, Value::Nil),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("listp"), other],
            ));
        }
    };

    if !pane_title.is_string() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), pane_title],
        ));
    }

    if !pane_items.is_cons() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("consp"), pane_items],
        ));
    }

    Ok(Value::Nil)
}

/// (x-synchronize DISPLAY &optional NO-OP) -> error in batch/no-X context.
pub(crate) fn builtin_x_synchronize(args: Vec<Value>) -> EvalResult {
    expect_range_args("x-synchronize", &args, 1, 2)?;
    Err(x_windows_not_initialized_error())
}

/// (x-translate-coordinates DISPLAY X Y &optional FRAME SOURCE-FRAME) -> error in batch/no-X context.
pub(crate) fn builtin_x_translate_coordinates(args: Vec<Value>) -> EvalResult {
    expect_range_args("x-translate-coordinates", &args, 1, 6)?;
    Err(x_display_query_first_arg_error(&args[0]))
}

/// (x-register-dnd-atom ATOM &optional OLD-ATOM) -> error in batch/no-X context.
pub(crate) fn builtin_x_register_dnd_atom(args: Vec<Value>) -> EvalResult {
    expect_range_args("x-register-dnd-atom", &args, 1, 2)?;
    Err(x_window_system_frame_error())
}

/// (x-export-frames &optional FRAME TYPE) -> error in batch/no-X context.
pub(crate) fn builtin_x_export_frames(args: Vec<Value>) -> EvalResult {
    expect_max_args("x-export-frames", &args, 2)?;
    match args.first() {
        None => Err(x_window_system_frame_error()),
        Some(frame) if frame.is_nil() || matches!(frame, Value::Frame(_)) => {
            Err(x_window_system_frame_error())
        }
        Some(other) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), *other],
        )),
    }
}

/// (x-focus-frame FRAME &optional NO-ACTIVATE) -> error in batch/no-X context.
pub(crate) fn builtin_x_focus_frame(args: Vec<Value>) -> EvalResult {
    expect_range_args("x-focus-frame", &args, 1, 2)?;
    let frame = &args[0];
    if frame.is_nil() || matches!(frame, Value::Frame(_)) {
        Err(x_window_system_frame_error())
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), *frame],
        ))
    }
}

/// (x-get-clipboard) -> nil in batch/no-X context.
pub(crate) fn builtin_x_get_clipboard(args: Vec<Value>) -> EvalResult {
    expect_args("x-get-clipboard", &args, 0)?;
    Ok(Value::Nil)
}

/// (x-get-modifier-masks &optional DISPLAY) -> error in batch/no-X context.
pub(crate) fn builtin_x_get_modifier_masks(args: Vec<Value>) -> EvalResult {
    expect_max_args("x-get-modifier-masks", &args, 1)?;
    match args.first() {
        None => Err(x_windows_not_initialized_error()),
        Some(display) if display.is_nil() => Err(x_windows_not_initialized_error()),
        Some(Value::Frame(_)) => Err(x_window_system_frame_error()),
        Some(display) => Err(x_display_query_first_arg_error(display)),
    }
}

/// (x-hide-tip) -> nil in batch/no-X context.
pub(crate) fn builtin_x_hide_tip(args: Vec<Value>) -> EvalResult {
    expect_args("x-hide-tip", &args, 0)?;
    Ok(Value::Nil)
}

/// (x-show-tip STRING &optional FRAME PARMS TIMEOUT DX DY) -> error in batch/no-X context.
pub(crate) fn builtin_x_show_tip(args: Vec<Value>) -> EvalResult {
    expect_range_args("x-show-tip", &args, 1, 6)?;
    if !args[0].is_string() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[0]],
        ));
    }
    Err(x_window_system_frame_error())
}

/// (x-setup-function-keys TERMINAL) -> nil/error in batch/no-X context.
pub(crate) fn builtin_x_setup_function_keys(args: Vec<Value>) -> EvalResult {
    expect_args("x-setup-function-keys", &args, 1)?;
    match &args[0] {
        Value::Frame(_) => Ok(Value::Nil),
        Value::Int(_) | Value::Str(_) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("terminal-live-p"), args[0]],
        )),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), *other],
        )),
    }
}

/// (x-internal-focus-input-context FRAME) -> nil in batch/no-X context.
pub(crate) fn builtin_x_internal_focus_input_context(args: Vec<Value>) -> EvalResult {
    expect_args("x-internal-focus-input-context", &args, 1)?;
    Ok(Value::Nil)
}

/// (x-wm-set-size-hint &optional FRAME) -> error in batch/no-X context.
pub(crate) fn builtin_x_wm_set_size_hint(args: Vec<Value>) -> EvalResult {
    expect_max_args("x-wm-set-size-hint", &args, 1)?;
    match args.first() {
        None => Err(x_window_system_frame_error()),
        Some(frame) if frame.is_nil() => Err(x_window_system_frame_error()),
        Some(Value::Frame(_)) => Err(x_window_system_frame_error()),
        Some(other) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), *other],
        )),
    }
}

/// (x-backspace-delete-keys-p &optional FRAME) -> error in batch/no-X context.
pub(crate) fn builtin_x_backspace_delete_keys_p(args: Vec<Value>) -> EvalResult {
    expect_max_args("x-backspace-delete-keys-p", &args, 1)?;
    if let Some(frame) = args.first() {
        expect_optional_window_system_frame_arg(frame)?;
    }
    Err(x_window_system_frame_error())
}

/// (x-family-fonts &optional FAMILY FRAME) -> nil in batch/no-X context.
pub(crate) fn builtin_x_family_fonts(args: Vec<Value>) -> EvalResult {
    expect_max_args("x-family-fonts", &args, 2)?;
    if let Some(frame) = args.get(1) {
        expect_optional_window_system_frame_arg(frame)?;
    }
    if let Some(family) = args.first() {
        if !family.is_nil() && !family.is_string() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *family],
            ));
        }
    }
    Ok(Value::Nil)
}

/// (x-get-atom-name ATOM &optional FRAME) -> error in batch/no-X context.
pub(crate) fn builtin_x_get_atom_name(args: Vec<Value>) -> EvalResult {
    expect_range_args("x-get-atom-name", &args, 1, 2)?;
    if let Some(frame) = args.get(1) {
        expect_optional_window_system_frame_arg(frame)?;
    }
    Err(x_window_system_frame_error())
}

pub(crate) fn builtin_x_get_resource(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("x-get-resource", &args, 2, 4)?;
    if x_window_system_active(eval) {
        return Ok(Value::Nil);
    }
    Err(window_system_not_initialized_error())
}

pub(crate) fn builtin_x_apply_session_resources(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("x-apply-session-resources", &args, 0)?;
    if x_window_system_active(eval) {
        return Ok(Value::Nil);
    }
    Err(window_system_not_initialized_error())
}

pub(crate) fn builtin_x_list_fonts(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("x-list-fonts", &args, 1, 5)?;
    if x_window_system_active(eval) {
        return Ok(Value::Nil);
    }
    Err(window_system_not_initialized_error())
}

/// (x-parse-geometry STRING) -> alist or nil.
pub(crate) fn builtin_x_parse_geometry(args: Vec<Value>) -> EvalResult {
    expect_args("x-parse-geometry", &args, 1)?;
    match &args[0] {
        Value::Str(id) => {
            let spec = with_heap(|h| h.get_string(*id).to_owned());
            Ok(parse_x_geometry(&spec).unwrap_or(Value::Nil))
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
    }
}

/// (x-change-window-property PROPERTY VALUE &optional FRAME TYPE FORMAT OUTER-P DELETE-P)
/// -> error in batch/no-X context.
pub(crate) fn builtin_x_change_window_property(args: Vec<Value>) -> EvalResult {
    expect_range_args("x-change-window-property", &args, 2, 7)?;
    if let Some(frame) = args.get(2) {
        expect_optional_window_system_frame_arg(frame)?;
    }
    Err(x_window_system_frame_error())
}

/// (x-delete-window-property PROPERTY &optional FRAME TYPE) -> error in batch/no-X context.
pub(crate) fn builtin_x_delete_window_property(args: Vec<Value>) -> EvalResult {
    expect_range_args("x-delete-window-property", &args, 1, 3)?;
    if let Some(frame) = args.get(1) {
        expect_optional_window_system_frame_arg(frame)?;
    }
    Err(x_window_system_frame_error())
}

/// (x-disown-selection-internal SELECTION &optional TYPE FRAME) -> nil.
pub(crate) fn builtin_x_disown_selection_internal(args: Vec<Value>) -> EvalResult {
    expect_range_args("x-disown-selection-internal", &args, 1, 3)?;
    Ok(Value::Nil)
}

/// (x-get-local-selection &optional SELECTION TYPE) -> nil/error.
pub(crate) fn builtin_x_get_local_selection(args: Vec<Value>) -> EvalResult {
    expect_max_args("x-get-local-selection", &args, 2)?;
    let selection = args.first().cloned().unwrap_or(Value::Nil);
    if !selection.is_cons() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("consp"), selection],
        ));
    }
    Ok(Value::Nil)
}

/// (x-get-selection-internal SELECTION TYPE &optional DATA-TYPE FRAME)
/// -> error in batch/no-X context.
pub(crate) fn builtin_x_get_selection_internal(args: Vec<Value>) -> EvalResult {
    expect_range_args("x-get-selection-internal", &args, 2, 4)?;
    Err(x_selection_unavailable_error())
}

/// (x-own-selection-internal SELECTION VALUE &optional FRAME)
/// -> error in batch/no-X context.
pub(crate) fn builtin_x_own_selection_internal(args: Vec<Value>) -> EvalResult {
    expect_range_args("x-own-selection-internal", &args, 2, 3)?;
    Err(x_selection_unavailable_error())
}

/// (gui-get-selection &optional TYPE DATA-TYPE) -> nil.
pub(crate) fn builtin_gui_get_selection(args: Vec<Value>) -> EvalResult {
    expect_max_args("gui-get-selection", &args, 2)?;
    Ok(Value::Nil)
}

/// (gui-get-primary-selection) -> error in batch/no-X context.
pub(crate) fn builtin_gui_get_primary_selection(args: Vec<Value>) -> EvalResult {
    expect_args("gui-get-primary-selection", &args, 0)?;
    Err(signal(
        "error",
        vec![Value::string("No selection is available")],
    ))
}

/// (gui-select-text TEXT) -> nil.
pub(crate) fn builtin_gui_select_text(args: Vec<Value>) -> EvalResult {
    expect_args("gui-select-text", &args, 1)?;
    Ok(Value::Nil)
}

/// (gui-selection-value) -> nil.
pub(crate) fn builtin_gui_selection_value(args: Vec<Value>) -> EvalResult {
    expect_args("gui-selection-value", &args, 0)?;
    Ok(Value::Nil)
}

/// (gui-set-selection TYPE VALUE) -> nil.
pub(crate) fn builtin_gui_set_selection(args: Vec<Value>) -> EvalResult {
    expect_args("gui-set-selection", &args, 2)?;
    Ok(Value::Nil)
}

/// (x-selection-exists-p &optional SELECTION TYPE) -> nil in batch/no-X context.
pub(crate) fn builtin_x_selection_exists_p(args: Vec<Value>) -> EvalResult {
    expect_max_args("x-selection-exists-p", &args, 2)?;
    if let Some(selection) = args.first() {
        if !selection.is_nil() {
            expect_symbol_key(selection)?;
        }
    }
    Ok(Value::Nil)
}

/// (x-selection-owner-p &optional SELECTION TYPE) -> nil in batch/no-X context.
pub(crate) fn builtin_x_selection_owner_p(args: Vec<Value>) -> EvalResult {
    expect_max_args("x-selection-owner-p", &args, 2)?;
    if let Some(selection) = args.first() {
        if !selection.is_nil() {
            expect_symbol_key(selection)?;
        }
    }
    Ok(Value::Nil)
}

/// (x-uses-old-gtk-dialog) -> nil
pub(crate) fn builtin_x_uses_old_gtk_dialog(args: Vec<Value>) -> EvalResult {
    expect_args("x-uses-old-gtk-dialog", &args, 0)?;
    Ok(Value::Nil)
}

/// (x-window-property PROPERTY &optional FRAME TYPE DELETE-P VECTOR-RET-P) -> error in batch/no-X context.
pub(crate) fn builtin_x_window_property(args: Vec<Value>) -> EvalResult {
    expect_range_args("x-window-property", &args, 1, 6)?;
    if let Some(frame) = args.get(1) {
        expect_optional_window_system_frame_arg(frame)?;
    }
    Err(x_window_system_frame_error())
}

/// (x-window-property-attributes PROPERTY &optional FRAME TYPE) -> error in batch/no-X context.
pub(crate) fn builtin_x_window_property_attributes(args: Vec<Value>) -> EvalResult {
    expect_range_args("x-window-property-attributes", &args, 1, 3)?;
    if let Some(frame) = args.get(1) {
        expect_optional_window_system_frame_arg(frame)?;
    }
    Err(x_window_system_frame_error())
}

/// Context-aware variant of `x-server-version`.
pub(crate) fn builtin_x_server_version(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    x_optional_display_query_error_eval(eval, "x-server-version", args)
}

/// Context-aware variant of `x-server-max-request-size`.
pub(crate) fn builtin_x_server_max_request_size(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    x_optional_display_query_error_eval(eval, "x-server-max-request-size", args)
}

/// Context-aware variant of `x-display-grayscale-p`.
pub(crate) fn builtin_x_display_grayscale_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    if gui_x_query_target_eval(eval, "x-display-grayscale-p", &args)? {
        return Ok(Value::True);
    }
    x_optional_display_query_error_eval(eval, "x-display-grayscale-p", args)
}

/// Context-aware variant of `x-display-backing-store`.
pub(crate) fn builtin_x_display_backing_store(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    x_optional_display_query_error_eval(eval, "x-display-backing-store", args)
}

/// Context-aware variant of `x-display-color-cells`.
pub(crate) fn builtin_x_display_color_cells(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    if gui_x_query_target_eval(eval, "x-display-color-cells", &args)? {
        return Ok(Value::Int(GUI_X_DISPLAY_COLOR_CELLS));
    }
    x_optional_display_query_error_eval(eval, "x-display-color-cells", args)
}

/// Context-aware variant of `x-display-mm-height`.
pub(crate) fn builtin_x_display_mm_height(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    x_optional_display_query_error_eval(eval, "x-display-mm-height", args)
}

/// Context-aware variant of `x-display-mm-width`.
pub(crate) fn builtin_x_display_mm_width(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    x_optional_display_query_error_eval(eval, "x-display-mm-width", args)
}

/// Context-aware variant of `x-display-monitor-attributes-list`.
pub(crate) fn builtin_x_display_monitor_attributes_list(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    x_optional_display_query_error_eval(eval, "x-display-monitor-attributes-list", args)
}

/// Context-aware variant of `x-display-planes`.
pub(crate) fn builtin_x_display_planes(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    if gui_x_query_target_eval(eval, "x-display-planes", &args)? {
        return Ok(Value::Int(GUI_X_DISPLAY_PLANES));
    }
    x_optional_display_query_error_eval(eval, "x-display-planes", args)
}

/// Context-aware variant of `x-display-save-under`.
pub(crate) fn builtin_x_display_save_under(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    x_optional_display_query_error_eval(eval, "x-display-save-under", args)
}

/// Context-aware variant of `x-display-screens`.
pub(crate) fn builtin_x_display_screens(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    x_optional_display_query_error_eval(eval, "x-display-screens", args)
}

/// Context-aware variant of `x-display-visual-class`.
pub(crate) fn builtin_x_display_visual_class(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    if gui_x_query_target_eval(eval, "x-display-visual-class", &args)? {
        return Ok(Value::symbol(GUI_X_VISUAL_CLASS));
    }
    x_optional_display_query_error_eval(eval, "x-display-visual-class", args)
}

/// Context-aware variant of `x-server-input-extension-version`.
pub(crate) fn builtin_x_server_input_extension_version(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    x_optional_display_query_error_eval(eval, "x-server-input-extension-version", args)
}

/// Context-aware variant of `x-server-vendor`.
pub(crate) fn builtin_x_server_vendor(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    x_optional_display_query_error_eval(eval, "x-server-vendor", args)
}

/// Context-aware variant of `x-display-set-last-user-time`.
///
/// In batch/no-X context, payload class follows USER-TIME argument designator
/// semantics, including live-frame and terminal handle message mapping.
pub(crate) fn builtin_x_display_set_last_user_time(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("x-display-set-last-user-time", &args, 1, 2)?;
    let query_args: Vec<Value> = args.get(1).cloned().into_iter().collect();
    x_optional_display_query_error_eval(eval, "x-display-set-last-user-time", query_args)
}

pub(crate) fn builtin_x_open_connection(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("x-open-connection", &args, 1, 3)?;
    if x_window_system_active(eval) {
        return Ok(Value::Nil);
    }
    match &args[0] {
        Value::Nil => Err(signal(
            "error",
            vec![Value::string("Display nil can’t be opened")],
        )),
        Value::Str(id) => {
            let display = with_heap(|h| h.get_string(*id).to_owned());
            Err(signal(
                "error",
                vec![Value::string(format!("Display {display} can’t be opened"))],
            ))
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
    }
}

/// Context-aware variant of `x-close-connection`.
///
/// Live frame designators map to batch-compatible frame-class errors.
pub(crate) fn builtin_x_close_connection(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("x-close-connection", &args, 1)?;
    if let Some(display) = args.first() {
        if live_frame_designator_p(eval, display) {
            return Err(signal(
                "error",
                vec![Value::string("Window system frame should be used")],
            ));
        }
    }
    match &args[0] {
        Value::Nil => Err(signal(
            "error",
            vec![Value::string("X windows are not in use or not initialized")],
        )),
        Value::Str(id) => {
            let display = with_heap(|h| h.get_string(*id).to_owned());
            Err(signal(
                "error",
                vec![Value::string(format!("Display {display} can’t be opened"))],
            ))
        }
        other => {
            if let Some(err) = terminal_not_x_display_error(other) {
                Err(err)
            } else {
                Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("frame-live-p"), *other],
                ))
            }
        }
    }
}

/// Context-aware variant of `x-display-pixel-width`.
///
/// Accepts live frame designators and maps them to the same batch/no-X error
/// class as nil/current-display queries.
pub(crate) fn builtin_x_display_pixel_width(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("x-display-pixel-width", &args, 1)?;
    if let Some(display) = args.first() {
        if live_frame_designator_p(eval, display) {
            return Err(signal(
                "error",
                vec![Value::string("Window system frame should be used")],
            ));
        }
    }
    match args.first() {
        None | Some(Value::Nil) => Err(signal(
            "error",
            vec![Value::string("X windows are not in use or not initialized")],
        )),
        Some(display) if is_terminal_handle(display) => {
            if let Some(err) = terminal_not_x_display_error(display) {
                Err(err)
            } else {
                Err(invalid_get_device_terminal_error(display))
            }
        }
        Some(Value::Str(_)) => {
            let display = args[0].as_str().unwrap();
            Err(signal(
                "error",
                vec![Value::string(format!("Display {display} can’t be opened"))],
            ))
        }
        Some(other) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), *other],
        )),
    }
}

/// Context-aware variant of `x-display-pixel-height`.
///
/// Accepts live frame designators and maps them to the same batch/no-X error
/// class as nil/current-display queries.
pub(crate) fn builtin_x_display_pixel_height(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("x-display-pixel-height", &args, 1)?;
    if let Some(display) = args.first() {
        if live_frame_designator_p(eval, display) {
            return Err(signal(
                "error",
                vec![Value::string("Window system frame should be used")],
            ));
        }
    }
    match args.first() {
        None | Some(Value::Nil) => Err(signal(
            "error",
            vec![Value::string("X windows are not in use or not initialized")],
        )),
        Some(display) if is_terminal_handle(display) => {
            if let Some(err) = terminal_not_x_display_error(display) {
                Err(err)
            } else {
                Err(invalid_get_device_terminal_error(display))
            }
        }
        Some(Value::Str(_)) => {
            let display = args[0].as_str().unwrap();
            Err(signal(
                "error",
                vec![Value::string(format!("Display {display} can’t be opened"))],
            ))
        }
        Some(other) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), *other],
        )),
    }
}

// ---------------------------------------------------------------------------
// Monitor attribute builtins
// ---------------------------------------------------------------------------

/// Context-aware variant of `display-monitor-attributes-list`.
///
/// This populates the `frames` slot from the live frame list.
pub(crate) fn builtin_display_monitor_attributes_list(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_optional_display_designator_eval(eval, "display-monitor-attributes-list", &args)?;

    let _ = super::window_cmds::ensure_selected_frame_id(eval);
    let frames = eval
        .frames
        .frame_list()
        .into_iter()
        .map(|fid| Value::Frame(fid.0))
        .collect::<Vec<_>>();
    Ok(Value::list(vec![make_monitor_alist(Value::list(frames))]))
}

/// Context-aware variant of `frame-monitor-attributes`.
///
/// This populates the `frames` slot from the live frame list.
pub(crate) fn builtin_frame_monitor_attributes(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_optional_display_designator_eval(eval, "frame-monitor-attributes", &args)?;

    let _ = super::window_cmds::ensure_selected_frame_id(eval);
    let frames = eval
        .frames
        .frame_list()
        .into_iter()
        .map(|fid| Value::Frame(fid.0))
        .collect::<Vec<_>>();
    Ok(make_monitor_alist(Value::list(frames)))
}

/// Build a single monitor alist with reasonable default values.
fn make_monitor_alist(frames: Value) -> Value {
    // geometry: (x y width height)
    let geometry = Value::list(vec![
        Value::Int(0),
        Value::Int(0),
        Value::Int(80),
        Value::Int(25),
    ]);

    // workarea: (x y width height)
    let workarea = Value::list(vec![
        Value::Int(0),
        Value::Int(0),
        Value::Int(80),
        Value::Int(25),
    ]);

    // mm-size: (width-mm height-mm)
    let mm_size = Value::list(vec![Value::Nil, Value::Nil]);

    make_alist(vec![
        (Value::symbol("geometry"), geometry),
        (Value::symbol("workarea"), workarea),
        (Value::symbol("mm-size"), mm_size),
        (Value::symbol("frames"), frames),
    ])
}
#[cfg(test)]
#[path = "display_test.rs"]
mod tests;
