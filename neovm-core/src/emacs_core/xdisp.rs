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
use crate::buffer::{Buffer, BufferId, TextPropertyTable};
use crate::encoding::char_to_byte_pos;
use crate::window::{DisplayPointSnapshot, FrameId, Window, WindowId};

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

fn expect_args_range(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), Flow> {
    if args.len() < min || args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_integer_or_marker(arg: &Value) -> Result<(), Flow> {
    match arg.kind() {
        ValueKind::Fixnum(_) => Ok(()),
        _other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *arg],
        )),
    }
}

fn expect_fixnum_arg(name: &str, arg: &Value) -> Result<(), Flow> {
    match arg.kind() {
        ValueKind::Fixnum(_) => Ok(()),
        _other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol(name), *arg],
        )),
    }
}

fn emacs_char_count(bytes: &[u8], multibyte: bool) -> usize {
    if multibyte {
        crate::emacs_core::emacs_char::chars_in_multibyte(bytes)
    } else {
        bytes.len()
    }
}

fn prefix_line_and_column(buf: &Buffer, end_byte: usize) -> (usize, usize) {
    let mut bytes = Vec::new();
    buf.copy_emacs_bytes_to(0, end_byte.min(buf.total_bytes()), &mut bytes);
    let line = bytes.iter().filter(|&&b| b == b'\n').count() + 1;
    let tail = match bytes.iter().rposition(|&b| b == b'\n') {
        Some(idx) => &bytes[idx + 1..],
        None => &bytes[..],
    };
    let col = emacs_char_count(tail, buf.get_multibyte());
    (line, col)
}

fn region_text_metrics(bytes: &[u8], multibyte: bool) -> (usize, usize) {
    let mut max_cols = 0usize;
    let mut lines = 1usize;
    let mut cur_col = 0usize;

    let mut visit = |code: u32| {
        if code == '\n' as u32 {
            lines += 1;
            max_cols = max_cols.max(cur_col);
            cur_col = 0;
        } else if code == '\t' as u32 {
            cur_col = (cur_col + 8) & !7;
        } else {
            cur_col += 1;
        }
    };

    if multibyte {
        let mut pos = 0usize;
        while pos < bytes.len() {
            let (code, len) = crate::emacs_core::emacs_char::string_char(&bytes[pos..]);
            visit(code);
            pos += len;
        }
    } else {
        for &byte in bytes {
            visit(byte as u32);
        }
    }

    (lines, max_cols.max(cur_col))
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
        if !buffer.is_nil() && !buffer.is_buffer() {
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
pub(crate) fn format_mode_line_from_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    frames: &crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    processes: &crate::emacs_core::process::ProcessManager,
    args: Vec<Value>,
) -> Result<Option<Value>, Flow> {
    expect_args_range("format-mode-line", &args, 1, 4)?;
    validate_optional_window_designator_in_state(frames, args.get(2), "windowp")?;
    validate_optional_buffer_designator_in_state(buffers, args.get(3))?;

    let target_buffer = resolve_mode_line_buffer_in_state(frames, args.get(2), args.get(3));
    let saved_buffer = buffers.current_buffer_id();
    if let Some(buffer_id) = target_buffer {
        buffers.switch_current(buffer_id);
    }

    if args[0].is_nil() {
        if let Some(buffer_id) = saved_buffer {
            buffers.switch_current(buffer_id);
        }
        return Ok(Some(Value::string("")));
    }

    let format_val = args[0];
    let face_spec = resolve_mode_line_face_spec(&args);
    let pctx = build_mode_line_percent_context(frames, &*buffers, None, obarray, args.get(2));
    let mut result = ModeLineRendered::default();
    let needs_eval = format_mode_line_recursive_in_state(
        obarray,
        dynamic,
        &*buffers,
        processes,
        &pctx,
        &format_val,
        &mut result,
        0,
        false,
    );

    if let Some(buffer_id) = saved_buffer {
        buffers.switch_current(buffer_id);
    }

    if needs_eval {
        Ok(None)
    } else {
        Ok(Some(result.into_value(face_spec)))
    }
}

pub(crate) fn builtin_format_mode_line_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    finish_format_mode_line_in_eval(eval, &args)
}

/// Render a mode-line format in GNU's `MODE_LINE_DISPLAY` mode.
///
/// This mirrors GNU's `display_mode_line` (xdisp.c:27911-27935) rather
/// than its `Fformat_mode_line` string API: the walker runs with
/// `mode_line_target = MODE_LINE_DISPLAY`, which makes `%-` expand to
/// dashes that fill the remaining row width (as opposed to the literal
/// `"--"` that string mode returns). The layout engine calls this
/// entry point directly (bypassing the Lisp-facing
/// `format-mode-line` builtin) when it needs a fully rendered TTY/GUI
/// mode-line row.
///
/// Arguments:
/// - `eval`       : evaluator context (for risky-local lookup and
///                  `:eval` evaluation).
/// - `format_val` : the mode-line format expression — the buffer's
///                  `mode-line-format` slot value, already
///                  resolved.
/// - `window`     : target window (for %-spec position info).
/// - `buffer`     : target buffer (for buffer-local percent specs).
/// - `target_cols`: the row width in character cells. `%-` fills to
///                  this width.
///
/// Returns the fully rendered mode-line as a propertized string. On
/// any evaluator error the function returns an empty string; callers
/// that need to distinguish failure from empty should check `as_str`.
pub fn format_mode_line_for_display(
    eval: &mut super::eval::Context,
    format_val: Value,
    window: Value,
    buffer: Value,
    target_cols: usize,
) -> Value {
    let args = [format_val, Value::NIL, window, buffer];
    if validate_optional_window_designator(eval, args.get(2), "windowp").is_err() {
        return Value::string("");
    }
    if validate_optional_buffer_designator(eval, args.get(3)).is_err() {
        return Value::string("");
    }
    let target_buffer = resolve_mode_line_buffer(eval, args.get(2), args.get(3));
    let saved_buffer = eval.buffers.current_buffer_id();
    if let Some(buffer_id) = target_buffer
        && eval.switch_current_buffer(buffer_id).is_err()
    {
        return Value::string("");
    }

    let result_value = if format_val.is_nil() {
        Value::string("")
    } else {
        let face_spec = resolve_mode_line_face_spec(&args);
        let mut pctx = build_mode_line_percent_context(
            &eval.frames,
            &eval.buffers,
            Some(&eval.coding_systems),
            &eval.obarray,
            args.get(2),
        );
        pctx.target_width = Some(target_cols);
        let mut rendered = ModeLineRendered::default();
        format_mode_line_recursive(eval, &pctx, &format_val, &mut rendered, 0, false);
        rendered.into_value(face_spec)
    };

    if let Some(buffer_id) = saved_buffer {
        eval.restore_current_buffer_if_live(buffer_id);
    }
    result_value
}

pub(crate) fn finish_format_mode_line_in_eval(
    eval: &mut super::eval::Context,
    args: &[Value],
) -> EvalResult {
    expect_args_range("format-mode-line", args, 1, 4)?;
    validate_optional_window_designator(eval, args.get(2), "windowp")?;
    validate_optional_buffer_designator(eval, args.get(3))?;

    let target_buffer = resolve_mode_line_buffer(eval, args.get(2), args.get(3));
    let saved_buffer = eval.buffers.current_buffer_id();
    if let Some(buffer_id) = target_buffer {
        eval.switch_current_buffer(buffer_id)?;
    }

    let result = if args[0].is_nil() {
        Value::string("")
    } else {
        let format_val = args[0];
        let face_spec = resolve_mode_line_face_spec(args);
        let pctx = build_mode_line_percent_context(
            &eval.frames,
            &eval.buffers,
            Some(&eval.coding_systems),
            &eval.obarray,
            args.get(2),
        );
        let mut result = ModeLineRendered::default();
        format_mode_line_recursive(eval, &pctx, &format_val, &mut result, 0, false);
        result.into_value(face_spec)
    };

    if let Some(buffer_id) = saved_buffer {
        eval.restore_current_buffer_if_live(buffer_id);
    }
    Ok(result)
}

pub(crate) fn finish_format_mode_line_in_state_with_eval(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    frames: &crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    processes: &crate::emacs_core::process::ProcessManager,
    args: &[Value],
    mut eval_form: impl FnMut(&Value, &crate::buffer::BufferManager) -> Result<Value, Flow>,
) -> EvalResult {
    expect_args_range("format-mode-line", args, 1, 4)?;
    validate_optional_window_designator_in_state(frames, args.get(2), "windowp")?;
    validate_optional_buffer_designator_in_state(buffers, args.get(3))?;

    let target_buffer = resolve_mode_line_buffer_in_state(frames, args.get(2), args.get(3));
    let saved_buffer = buffers.current_buffer_id();
    if let Some(buffer_id) = target_buffer {
        buffers.switch_current(buffer_id);
    }

    let result = if args[0].is_nil() {
        Value::string("")
    } else {
        let format_val = args[0];
        let face_spec = resolve_mode_line_face_spec(args);
        let pctx = build_mode_line_percent_context(frames, &*buffers, None, obarray, args.get(2));
        let mut result = ModeLineRendered::default();
        format_mode_line_recursive_in_state_with_eval(
            obarray,
            dynamic,
            &*buffers,
            processes,
            &pctx,
            &format_val,
            &mut result,
            0,
            false,
            &mut eval_form,
        )?;
        result.into_value(face_spec)
    };

    if let Some(buffer_id) = saved_buffer {
        buffers.switch_current(buffer_id);
    }
    Ok(result)
}

pub(crate) fn builtin_format_mode_line_in_vm_runtime(
    shared: &mut crate::emacs_core::eval::Context,
    args: &[Value],
) -> EvalResult {
    expect_args_range("format-mode-line", args, 1, 4)?;
    validate_optional_window_designator_in_state(&shared.frames, args.get(2), "windowp")?;
    validate_optional_buffer_designator_in_state(&shared.buffers, args.get(3))?;

    let target_buffer = resolve_mode_line_buffer_in_state(&shared.frames, args.get(2), args.get(3));
    let saved_buffer = shared.buffers.current_buffer_id();
    if let Some(buffer_id) = target_buffer {
        shared.switch_current_buffer(buffer_id)?;
    }

    let result = if args[0].is_nil() {
        Value::string("")
    } else {
        let format_val = args[0];
        let face_spec = resolve_mode_line_face_spec(&args);
        let pctx = build_mode_line_percent_context(
            &shared.frames,
            &shared.buffers,
            Some(&shared.coding_systems),
            &shared.obarray,
            args.get(2),
        );
        let mut result = ModeLineRendered::default();
        format_mode_line_recursive_in_vm_runtime(
            shared,
            args,
            &pctx,
            &format_val,
            &mut result,
            0,
            false,
        )?;
        result.into_value(face_spec)
    };

    if let Some(buffer_id) = saved_buffer {
        shared.restore_current_buffer_if_live(buffer_id);
    }
    Ok(result)
}

fn mode_line_symbol_value_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    _dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    name: &str,
) -> Option<Value> {
    if let Some(buf) = buffers.current_buffer()
        && let Some(value) = buf.get_buffer_local(name)
    {
        return Some(value);
    }

    obarray.symbol_value(name).copied()
}

fn mode_line_human_readable_size(mut quotient: usize) -> String {
    const POWER_LETTER: [char; 11] = ['\0', 'k', 'M', 'G', 'T', 'P', 'E', 'Z', 'Y', 'R', 'Q'];

    let mut tenths = None;
    let mut exponent = 0_usize;

    if quotient >= 1000 {
        let mut remainder: usize;
        loop {
            remainder = quotient % 1000;
            quotient /= 1000;
            exponent += 1;
            if quotient < 1000 {
                break;
            }
        }

        if quotient <= 9 {
            let rounded_tenths = remainder / 100;
            if remainder % 100 >= 50 {
                if rounded_tenths < 9 {
                    tenths = Some(rounded_tenths + 1);
                } else {
                    quotient += 1;
                    if quotient < 10 {
                        tenths = Some(0);
                    } else {
                        tenths = None;
                    }
                }
            } else {
                tenths = Some(rounded_tenths);
            }
        } else if remainder >= 500 {
            if quotient < 999 {
                quotient += 1;
            } else {
                quotient = 1;
                exponent += 1;
                tenths = Some(0);
            }
        }
    }

    let mut rendered = quotient.to_string();
    if let Some(tenths) = tenths {
        rendered.push('.');
        rendered.push(char::from(b'0' + tenths as u8));
    }
    let suffix = POWER_LETTER[exponent];
    if suffix != '\0' {
        rendered.push(suffix);
    }
    rendered
}

fn mode_line_process_status_in_state(
    buffers: &crate::buffer::BufferManager,
    processes: &crate::emacs_core::process::ProcessManager,
) -> &'static str {
    let Some(buffer_id) = buffers.current_buffer_id() else {
        return "no process";
    };
    let Some(process_id) = processes.find_by_buffer_id(buffer_id) else {
        return "no process";
    };
    let Some(process) = processes.get_any(process_id) else {
        return "no process";
    };
    crate::emacs_core::process::process_public_status_symbol(process)
        .as_symbol_name()
        .unwrap_or("no process")
}

fn mode_line_symbol_is_risky(obarray: &crate::emacs_core::symbol::Obarray, name: &str) -> bool {
    obarray
        .get_property(name, "risky-local-variable")
        .is_some_and(|value| !value.is_nil())
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

/// Window and frame context for GNU-compatible mode-line percent specs.
///
/// Corresponds to the `struct window *w` and `struct frame *f` parameters
/// in GNU's `decode_mode_spec` (xdisp.c:29083).
#[derive(Clone)]
struct ModeLinePercentContext {
    /// Window start position (character offset of first visible character).
    /// Corresponds to `marker_position(w->start)` in GNU.
    window_start: usize,
    /// Window end position (last visible character position).
    /// In GNU this is `BUF_Z(b) - w->window_end_pos`.
    window_end: usize,
    /// Frame name for `%F`.  GNU: `f->title` then `f->name` then "Emacs".
    frame_name: Option<Value>,
    /// Coding system mnemonic character for `%z`/`%Z`.
    /// GNU: `CODING_ATTR_MNEMONIC` from the coding system spec.
    coding_mnemonic: char,
    /// Terminal output coding mnemonic (TTY only). For `%z` on TTY
    /// frames, GNU outputs 3 chars: terminal + keyboard + buffer.
    terminal_coding_mnemonic: char,
    /// Keyboard input coding mnemonic (TTY only).
    keyboard_coding_mnemonic: char,
    /// True when the selected frame is a TTY (no window-system).
    is_tty_frame: bool,
    /// EOL type string for `%Z` (`:`, `\`, `/`, or undecided).
    eol_indicator: Option<Value>,
    /// When `Some(n)`, the walker is running in GNU's
    /// `MODE_LINE_DISPLAY` mode with a target column width of `n`.
    /// In that mode, the `%-` percent construct expands to enough
    /// dashes to fill the remaining row width (GNU
    /// `xdisp.c:29154-29172`: `lots_of_dashes`). Callers that want
    /// GNU's `MODE_LINE_STRING` behavior (the Lisp-facing
    /// `format-mode-line` builtin) leave this `None`, and `%-`
    /// returns the literal two-dash string `"--"`.
    ///
    /// This mirrors GNU's internal `mode_line_target` state: the
    /// string API `Fformat_mode_line` uses `MODE_LINE_STRING`, while
    /// `display_mode_line` uses `MODE_LINE_DISPLAY`. The same walker
    /// serves both; the only difference is the dispatch for `%-`.
    target_width: Option<usize>,
}

impl Default for ModeLinePercentContext {
    fn default() -> Self {
        Self {
            window_start: 0,
            window_end: 0,
            frame_name: None,
            coding_mnemonic: '-',
            terminal_coding_mnemonic: '\0',
            keyboard_coding_mnemonic: '\0',
            is_tty_frame: false,
            eol_indicator: None,
            target_width: None,
        }
    }
}

/// Build a `ModeLinePercentContext` from frame/window/buffer state.
fn build_mode_line_percent_context(
    frames: &crate::window::FrameManager,
    buffers: &crate::buffer::BufferManager,
    coding_systems: Option<&crate::emacs_core::coding::CodingSystemManager>,
    obarray: &crate::emacs_core::symbol::Obarray,
    window_arg: Option<&Value>,
) -> ModeLinePercentContext {
    let mut ctx = ModeLinePercentContext {
        coding_mnemonic: '-',
        ..Default::default()
    };

    // --- Frame name (GNU: f->title, f->name, "Emacs") ---
    if let Some(frame) = frames.selected_frame() {
        let title = frame.title_value();
        if title.is_string() {
            ctx.frame_name = Some(title);
        } else if frame.explicit_name || frame.effective_window_system().is_none() {
            let name = frame.name_value();
            if name.is_string() {
                ctx.frame_name = Some(name);
            }
        }
    }

    // --- Window start/end (GNU: w->start, BUF_Z(b) - w->window_end_pos) ---
    let resolved_window = resolve_mode_line_window(frames, window_arg);
    let context_buffer = resolved_window
        .and_then(|window| window.buffer_id())
        .and_then(|buffer_id| buffers.get(buffer_id))
        .or_else(|| buffers.current_buffer());
    if let Some(window) = resolved_window {
        if let crate::window::Window::Leaf {
            window_start,
            window_end_pos,
            ..
        } = window
        {
            // Window positions are 1-indexed (Elisp convention); convert to
            // 0-indexed to match buffer begv/zv.
            ctx.window_start = window_start.saturating_sub(1);
            if let Some(buf) = context_buffer {
                ctx.window_end = buf
                    .point_max_char()
                    .saturating_add(1)
                    .saturating_sub(*window_end_pos);
            } else {
                ctx.window_end = ctx.window_start;
            }
        }
    } else if let Some(buf) = context_buffer {
        // Fallback: use buffer positions when no window is available.
        ctx.window_start = 0;
        ctx.window_end = buf.point_max_char();
    }

    // --- TTY detection (GNU: FRAME_WINDOW_P) ---
    if let Some(frame) = frames.selected_frame() {
        ctx.is_tty_frame = frame.effective_window_system().is_none();
    }

    // --- Coding system mnemonic (GNU: decode_mode_spec_coding) ---
    let cs_name = context_buffer
        .and_then(|b| b.get_buffer_local("buffer-file-coding-system"))
        .and_then(|v| v.as_symbol_id());
    if let Some(name) = cs_name {
        ctx.coding_mnemonic = coding_system_mnemonic_char(name);
        ctx.eol_indicator = coding_system_eol_indicator_value(obarray, name);
    }

    // --- Terminal and keyboard coding mnemonics (TTY only) ---
    // GNU xdisp.c:29494: on TTY, %z outputs 3 chars —
    // terminal-coding-system mnemonic, keyboard-coding-system mnemonic,
    // and buffer-file-coding-system mnemonic.
    if ctx.is_tty_frame {
        if let Some(coding_systems) = coding_systems {
            ctx.terminal_coding_mnemonic =
                coding_system_mnemonic_char(coding_systems.terminal_coding_sym());
            ctx.keyboard_coding_mnemonic =
                coding_system_mnemonic_char(coding_systems.keyboard_coding_sym());
        } else {
            let term_cs = obarray
                .symbol_value("terminal-coding-system")
                .and_then(|v| v.as_symbol_id());
            ctx.terminal_coding_mnemonic = term_cs.map(coding_system_mnemonic_char).unwrap_or('-');

            let kbd_cs = obarray
                .symbol_value("keyboard-coding-system")
                .and_then(|v| v.as_symbol_id());
            ctx.keyboard_coding_mnemonic = kbd_cs.map(coding_system_mnemonic_char).unwrap_or('-');
        }
    }

    ctx
}

/// Resolve the WINDOW argument to an actual Window reference.
fn resolve_mode_line_window<'a>(
    frames: &'a crate::window::FrameManager,
    window_arg: Option<&Value>,
) -> Option<&'a crate::window::Window> {
    // Try explicit window argument first.
    if let Some(windowish) = window_arg {
        if !windowish.is_nil() {
            let wid = if let Some(id) = windowish.as_window_id() {
                Some(crate::window::WindowId(id))
            } else if let Some(id) = windowish.as_fixnum().filter(|&id| id >= 0) {
                Some(crate::window::WindowId(id as u64))
            } else {
                None
            };
            if let Some(wid) = wid {
                for fid in frames.frame_list() {
                    if let Some(frame) = frames.get(fid) {
                        if let Some(window) = frame.find_window(wid) {
                            return Some(window);
                        }
                    }
                }
            }
        }
    }

    // Fall back to selected window of selected frame.
    if let Some(frame) = frames.selected_frame() {
        let selected = frame.selected_window;
        return frame.find_window(selected);
    }

    None
}

/// Derive coding system mnemonic character from coding system name.
///
/// Matches GNU `decode_mode_spec_coding` heuristics for common systems.
fn coding_system_mnemonic_char(cs_name: crate::emacs_core::intern::SymId) -> char {
    let cs_name = crate::emacs_core::intern::resolve_sym(cs_name);
    let base = cs_name
        .strip_suffix("-unix")
        .or_else(|| cs_name.strip_suffix("-dos"))
        .or_else(|| cs_name.strip_suffix("-mac"))
        .unwrap_or(cs_name);
    match base {
        "utf-8" | "utf-8-emacs" | "utf-8-auto" | "prefer-utf-8" | "mule-utf-8" => 'U',
        "undecided" => '-',
        "raw-text" => '=',
        "no-conversion" | "binary" => '0',
        "us-ascii" | "ascii" => '.',
        "iso-8859-1" | "iso-latin-1" | "latin-1" => '1',
        "iso-8859-2" | "iso-latin-2" | "latin-2" => '2',
        "iso-8859-3" | "latin-3" => '3',
        "iso-8859-4" | "latin-4" => '4',
        "iso-8859-5" | "latin-5" => '5',
        "iso-2022-jp" | "junet" => 'J',
        "euc-jp" => 'E',
        "shift_jis" | "sjis" => 'S',
        "iso-2022-kr" => 'K',
        "euc-kr" => 'e',
        "gb2312" | "euc-cn" | "cn-gb" => 'C',
        "big5" => 'B',
        _ => '-',
    }
}

/// Derive EOL type indicator from coding system name, using the
/// `eol-mnemonic-*` variables from the obarray (matches GNU semantics).
fn coding_system_eol_indicator_value(
    obarray: &crate::emacs_core::symbol::Obarray,
    cs_name: crate::emacs_core::intern::SymId,
) -> Option<Value> {
    let cs_name = crate::emacs_core::intern::resolve_sym(cs_name);
    let var_name = if cs_name.ends_with("-dos") {
        "eol-mnemonic-dos"
    } else if cs_name.ends_with("-mac") {
        "eol-mnemonic-mac"
    } else if cs_name.ends_with("-unix") {
        "eol-mnemonic-unix"
    } else {
        "eol-mnemonic-undecided"
    };
    obarray
        .symbol_value(var_name)
        .copied()
        .filter(|value| value.is_string() || value.as_char().is_some())
}

/// Check if a directory path looks like a Tramp remote path.
///
/// Tramp paths match `/METHOD:...` where METHOD is a lowercase alpha string.
fn is_remote_directory(dir: &str) -> bool {
    if !dir.starts_with('/') {
        return false;
    }
    let rest = &dir[1..];
    if let Some(colon_pos) = rest.find(':') {
        colon_pos >= 2 && rest[..colon_pos].bytes().all(|b| b.is_ascii_lowercase())
    } else {
        false
    }
}

fn mode_line_runtime_string(value: &Value) -> Option<String> {
    value.as_runtime_string_owned()
}

/// Compute GNU `percent99` — percentage capped at 99, rounded up.
fn percent99(n: usize, d: usize) -> usize {
    if d == 0 {
        return 0;
    }
    let pct = (d - 1 + 100 * n) / d;
    pct.min(99)
}

#[derive(Clone, Default)]
struct ModeLineRendered {
    text: String,
    text_props: TextPropertyTable,
}

#[derive(Clone, Copy)]
struct ModeLineFaceSpec {
    no_props: bool,
    face: Option<Value>,
}

impl ModeLineRendered {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            text_props: TextPropertyTable::new(),
        }
    }

    fn append_rendered(&mut self, other: &Self) {
        let byte_offset = self.text.len();
        self.text.push_str(&other.text);
        self.text_props
            .append_shifted(&other.text_props, byte_offset);
    }

    fn append_string_value_preserving_props(&mut self, value: &Value) {
        match value.as_lisp_string() {
            Some(string) => {
                let text = crate::emacs_core::builtins::runtime_string_from_lisp_string(string);
                let byte_offset = self.text.len();
                self.text.push_str(&text);
                if let Some(props) = get_string_text_properties_table_for_value(*value) {
                    self.text_props.append_shifted(&props, byte_offset);
                }
            }
            None => {
                let Some(text) = value.as_str() else {
                    return;
                };
                self.text.push_str(text);
            }
        }
    }

    fn append_string_or_char_value_preserving_props(&mut self, value: &Value) {
        if value.is_string() {
            self.append_string_value_preserving_props(value);
        } else if let Some(ch) = value.as_char() {
            self.text.push(ch);
        }
    }

    fn append_string_char_slice_preserving_props(
        &mut self,
        value: &Value,
        start_char: usize,
        end_char: usize,
    ) {
        if start_char >= end_char {
            return;
        }
        match value.as_lisp_string() {
            Some(string) => {
                let text = crate::emacs_core::builtins::runtime_string_from_lisp_string(string);
                let byte_start = char_to_byte_pos(&text, start_char);
                let byte_end = char_to_byte_pos(&text, end_char);
                let byte_offset = self.text.len();
                self.text.push_str(
                    &text
                        .chars()
                        .skip(start_char)
                        .take(end_char - start_char)
                        .collect::<String>(),
                );
                if let Some(props) = get_string_text_properties_table_for_value(*value) {
                    self.text_props
                        .append_shifted(&props.slice(byte_start, byte_end), byte_offset);
                }
            }
            None => {
                let Some(text) = value.as_str() else {
                    return;
                };
                let byte_start = char_to_byte_pos(text, start_char);
                let byte_end = char_to_byte_pos(text, end_char);
                let byte_offset = self.text.len();
                self.text.push_str(
                    &text
                        .chars()
                        .skip(start_char)
                        .take(end_char - start_char)
                        .collect::<String>(),
                );
                if value.is_string() {
                    if let Some(props) = get_string_text_properties_table_for_value(*value) {
                        self.text_props
                            .append_shifted(&props.slice(byte_start, byte_end), byte_offset);
                    }
                }
            }
        }
    }

    fn push_plain_char(&mut self, ch: char) {
        self.text.push(ch);
    }

    fn char_len(&self) -> usize {
        self.text.chars().count()
    }

    fn slice_chars(&self, precision: usize) -> Self {
        let byte_end = char_to_byte_pos(&self.text, precision);
        Self {
            text: self.text.chars().take(precision).collect(),
            text_props: self.text_props.slice(0, byte_end),
        }
    }

    fn pad_plain_spaces(&mut self, padding_chars: usize) {
        if padding_chars == 0 {
            return;
        }
        self.text.extend(std::iter::repeat_n(' ', padding_chars));
    }

    fn overlay_properties(&mut self, props: Value) {
        if self.text.is_empty() {
            return;
        }
        let Some(items) = list_to_vec(&props) else {
            return;
        };
        for chunk in items.chunks(2) {
            if chunk.len() != 2 {
                continue;
            }
            self.text_props
                .put_property(0, self.text.len(), chunk[0], chunk[1]);
        }
    }

    fn overlay_property_map(&mut self, props: std::collections::HashMap<Value, Value>) {
        if self.text.is_empty() || props.is_empty() {
            return;
        }
        for (name, value) in props {
            self.text_props
                .put_property(0, self.text.len(), name, value);
        }
    }

    fn apply_default_face(&mut self, face: Value) {
        if self.text.is_empty() {
            return;
        }

        let end = self.text.len();
        let intervals = self.text_props.intervals_snapshot();
        let mut cursor = 0;

        for interval in intervals {
            let start = interval.start.min(end);
            let interval_end = interval.end.min(end);

            if cursor < start {
                self.text_props
                    .put_property(cursor, start, Value::symbol("face"), face);
            }

            if start < interval_end {
                let merged_face = interval
                    .properties
                    .get(&Value::symbol("face"))
                    .copied()
                    .map(|existing| Value::list(vec![existing, face]))
                    .unwrap_or(face);
                self.text_props.put_property(
                    start,
                    interval_end,
                    Value::symbol("face"),
                    merged_face,
                );
                cursor = interval_end;
            }

            if cursor >= end {
                break;
            }
        }

        if cursor < end {
            self.text_props
                .put_property(cursor, end, Value::symbol("face"), face);
        }
    }

    fn into_value(mut self, face_spec: ModeLineFaceSpec) -> Value {
        let multibyte = crate::emacs_core::string_escape::decode_storage_char_codes(&self.text)
            .into_iter()
            .any(|code| code > 0xFF);
        if face_spec.no_props {
            return Value::heap_string(crate::emacs_core::builtins::runtime_string_to_lisp_string(
                &self.text, multibyte,
            ));
        }
        if let Some(face) = face_spec.face {
            self.apply_default_face(face);
        }
        let value = Value::heap_string(crate::emacs_core::builtins::runtime_string_to_lisp_string(
            &self.text, multibyte,
        ));
        if value.is_string() {
            set_string_text_properties_table_for_value(value, self.text_props);
        }
        value
    }
}

fn resolve_mode_line_face_spec(args: &[Value]) -> ModeLineFaceSpec {
    let face = args.get(1).copied().unwrap_or(Value::NIL);
    let no_props = face.is_fixnum();
    let face = if no_props || face.is_nil() || face.is_symbol_named("default") {
        None
    } else {
        Some(face)
    };
    ModeLineFaceSpec { no_props, face }
}

fn append_mode_line_rendered_segment(
    result: &mut ModeLineRendered,
    rendered: &ModeLineRendered,
    field_width: i64,
    precision: i64,
) {
    let mut segment = if precision > 0 {
        rendered.slice_chars(precision as usize)
    } else {
        rendered.clone()
    };
    let rendered_len = segment.char_len() as i64;
    if field_width > 0 && rendered_len < field_width {
        segment.pad_plain_spaces((field_width - rendered_len) as usize);
    }
    result.append_rendered(&segment);
}

fn append_mode_line_percent_string_spec(
    result: &mut ModeLineRendered,
    spec: &str,
    props_at_percent: &std::collections::HashMap<Value, Value>,
    field_width: i64,
) {
    let mut segment = ModeLineRendered::plain(spec);
    segment.overlay_property_map(props_at_percent.clone());
    append_mode_line_rendered_segment(result, &segment, field_width, 0);
}

fn append_mode_line_percent_lisp_text_spec(
    result: &mut ModeLineRendered,
    value: &Value,
    props_at_percent: &std::collections::HashMap<Value, Value>,
    field_width: i64,
) {
    let mut segment = ModeLineRendered::default();
    segment.append_string_or_char_value_preserving_props(value);
    segment.overlay_property_map(props_at_percent.clone());
    append_mode_line_rendered_segment(result, &segment, field_width, 0);
}

fn append_mode_line_string_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    processes: &crate::emacs_core::process::ProcessManager,
    command_loop_depth: usize,
    pctx: &ModeLinePercentContext,
    result: &mut ModeLineRendered,
    value: &Value,
    literal: bool,
) {
    let text = if let Some(string) = value.as_lisp_string() {
        crate::emacs_core::builtins::runtime_string_from_lisp_string(string)
    } else if let Some(text) = value.as_str() {
        text.to_owned()
    } else {
        return;
    };
    if literal || !text.contains('%') {
        result.append_string_value_preserving_props(value);
    } else {
        expand_mode_line_percent_in_state(
            obarray,
            dynamic,
            buffers,
            processes,
            command_loop_depth,
            pctx,
            value,
            result,
        );
    }
}

/// Recursively process a mode-line format spec, appending output to `result`.
///
/// FORMAT can be:
/// - A string: expand %-constructs (%b, %f, %*, %l, %c, %p, etc.)
/// - A symbol: look up its value, recursively format
/// - A list: process each element in sequence
/// - `(:eval FORM)`: evaluate FORM, use result as format
/// - `(:propertize ELT PROPS...)`: process ELT and apply text properties
/// - A cons `(SYMBOL . REST)`: if SYMBOL's value is non-nil, process REST
fn format_mode_line_recursive(
    eval: &mut super::eval::Context,
    pctx: &ModeLinePercentContext,
    format: &Value,
    result: &mut ModeLineRendered,
    depth: usize,
    risky: bool,
) {
    if depth > 20 {
        return; // Guard against infinite recursion
    }

    match format.kind() {
        ValueKind::Nil => {}

        ValueKind::String => append_mode_line_string_in_state(
            &eval.obarray,
            &[],
            &eval.buffers,
            &eval.processes,
            eval.recursive_command_loop_depth(),
            pctx,
            result,
            format,
            false,
        ),

        ValueKind::Fixnum(n) => {
            let _ = n;
        }

        _ if format.is_symbol() => {
            // GNU xdisp.c:28438-28468 (display_mode_element, Lisp_Symbol
            // branch): resolve the symbol's value and recurse. There is
            // no special case for mode-line-front-space or
            // mode-line-end-spaces — GNU treats every mode-line symbol
            // the same way. Previously this branch short-circuited those
            // two names to a single hardcoded space, which silently
            // discarded the `(:eval (unless (display-graphic-p) "-%-"))`
            // dash-fill construct that bindings.el installs on TTY.
            if let Some(name) = format.as_symbol_name() {
                if let Some(val) =
                    mode_line_symbol_value_in_state(&eval.obarray, &[], &eval.buffers, name)
                    && !val.is_nil()
                {
                    if val.is_string() {
                        append_mode_line_string_in_state(
                            &eval.obarray,
                            &[],
                            &eval.buffers,
                            &eval.processes,
                            eval.recursive_command_loop_depth(),
                            pctx,
                            result,
                            &val,
                            true,
                        );
                    } else {
                        format_mode_line_recursive(
                            eval,
                            pctx,
                            &val,
                            result,
                            depth + 1,
                            risky || !mode_line_symbol_is_risky(&eval.obarray, name),
                        );
                    }
                }
            }
        }

        _ if format.is_cons() => {
            let car = format.cons_car();
            let cdr = format.cons_cdr();

            if car.is_symbol_named(":eval") {
                if risky {
                    return;
                }
                if cdr.is_cons() {
                    let form_val = cdr.cons_car();
                    if let Ok(val) = eval.eval_value(&form_val) {
                        format_mode_line_recursive(eval, pctx, &val, result, depth + 1, risky);
                    }
                }
                return;
            }

            if car.is_symbol_named(":propertize") {
                if risky {
                    return;
                }
                if cdr.is_cons() {
                    let elt = cdr.cons_car();
                    let mut nested = ModeLineRendered::default();
                    format_mode_line_recursive(eval, pctx, &elt, &mut nested, depth + 1, risky);
                    nested.overlay_properties(cdr.cons_cdr());
                    result.append_rendered(&nested);
                }
                return;
            }

            if let Some(lim) = car.as_fixnum() {
                let mut nested = ModeLineRendered::default();
                format_mode_line_recursive(eval, pctx, &cdr, &mut nested, depth + 1, risky);
                append_mode_line_rendered_segment(
                    result,
                    &nested,
                    if lim > 0 { lim } else { 0 },
                    if lim < 0 { -lim } else { 0 },
                );
                return;
            }

            if car.is_symbol() && !car.is_symbol_named("t") {
                if let Some(sym_name) = car.as_symbol_name()
                    && mode_line_symbol_value_in_state(&eval.obarray, &[], &eval.buffers, sym_name)
                        .is_some_and(|value| value.is_truthy())
                    && let Some(branch) = mode_line_conditional_branch(cdr, true)
                {
                    format_mode_line_recursive(eval, pctx, &branch, result, depth + 1, risky);
                } else if let Some(branch) = mode_line_conditional_branch(cdr, false) {
                    format_mode_line_recursive(eval, pctx, &branch, result, depth + 1, risky);
                }
                return;
            }

            if let Some(elements) = list_to_vec(format) {
                for elem in &elements {
                    format_mode_line_recursive(eval, pctx, elem, result, depth + 1, risky);
                }
            }
        }

        _ => {
            result.append_string_value_preserving_props(format);
        }
    }
}

fn format_mode_line_recursive_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    processes: &crate::emacs_core::process::ProcessManager,
    pctx: &ModeLinePercentContext,
    format: &Value,
    result: &mut ModeLineRendered,
    depth: usize,
    risky: bool,
) -> bool {
    if depth > 20 {
        return false;
    }

    match format.kind() {
        ValueKind::Nil => {}

        ValueKind::String => append_mode_line_string_in_state(
            obarray, dynamic, buffers, processes, 0, pctx, result, format, false,
        ),

        ValueKind::Fixnum(_) => {}

        _ if format.is_symbol() => {
            // GNU xdisp.c:28438-28468 — symbol branch of display_mode_element.
            // No special case for mode-line-front-space or
            // mode-line-end-spaces; see the note on the equivalent
            // branch in `format_mode_line_recursive`.
            if let Some(name) = format.as_symbol_name() {
                if let Some(val) = mode_line_symbol_value_in_state(obarray, dynamic, buffers, name)
                    && !val.is_nil()
                {
                    if val.is_string() {
                        append_mode_line_string_in_state(
                            obarray, dynamic, buffers, processes, 0, pctx, result, &val, true,
                        );
                    } else if format_mode_line_recursive_in_state(
                        obarray,
                        dynamic,
                        buffers,
                        processes,
                        pctx,
                        &val,
                        result,
                        depth + 1,
                        risky || !mode_line_symbol_is_risky(obarray, name),
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
                if risky {
                    return false;
                }
                return true;
            }

            if car.is_symbol_named(":propertize") {
                if risky {
                    return false;
                }
                if cdr.is_cons() {
                    let elt = cdr.cons_car();
                    let mut nested = ModeLineRendered::default();
                    let needs_eval = format_mode_line_recursive_in_state(
                        obarray,
                        dynamic,
                        buffers,
                        processes,
                        pctx,
                        &elt,
                        &mut nested,
                        depth + 1,
                        risky,
                    );
                    nested.overlay_properties(cdr.cons_cdr());
                    result.append_rendered(&nested);
                    return needs_eval;
                }
                return false;
            }

            if let Some(lim) = car.as_fixnum() {
                let mut nested = ModeLineRendered::default();
                let needs_eval = format_mode_line_recursive_in_state(
                    obarray,
                    dynamic,
                    buffers,
                    processes,
                    pctx,
                    &cdr,
                    &mut nested,
                    depth + 1,
                    risky,
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
                        processes,
                        pctx,
                        &branch,
                        result,
                        depth + 1,
                        risky,
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
                        processes,
                        pctx,
                        elem,
                        result,
                        depth + 1,
                        risky,
                    ) {
                        return true;
                    }
                }
            }
        }

        _ => {
            result.append_string_value_preserving_props(format);
        }
    }

    false
}

fn format_mode_line_recursive_in_state_with_eval(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    processes: &crate::emacs_core::process::ProcessManager,
    pctx: &ModeLinePercentContext,
    format: &Value,
    result: &mut ModeLineRendered,
    depth: usize,
    risky: bool,
    eval_form: &mut impl FnMut(&Value, &crate::buffer::BufferManager) -> Result<Value, Flow>,
) -> Result<(), Flow> {
    if depth > 20 {
        return Ok(());
    }

    match format.kind() {
        ValueKind::Nil => {}

        ValueKind::String => append_mode_line_string_in_state(
            obarray, dynamic, buffers, processes, 0, pctx, result, format, false,
        ),

        ValueKind::Fixnum(_) => {}

        _ if format.is_symbol() => {
            // GNU xdisp.c:28438-28468 — symbol branch of display_mode_element.
            // No special case for mode-line-front-space or
            // mode-line-end-spaces; they are ordinary symbols whose
            // value must be resolved and recursed on.
            if let Some(name) = format.as_symbol_name() {
                if let Some(val) = mode_line_symbol_value_in_state(obarray, dynamic, buffers, name)
                    && !val.is_nil()
                {
                    if val.is_string() {
                        append_mode_line_string_in_state(
                            obarray, dynamic, buffers, processes, 0, pctx, result, &val, true,
                        );
                    } else {
                        format_mode_line_recursive_in_state_with_eval(
                            obarray,
                            dynamic,
                            buffers,
                            processes,
                            pctx,
                            &val,
                            result,
                            depth + 1,
                            risky || !mode_line_symbol_is_risky(obarray, name),
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
                if risky {
                    return Ok(());
                }
                if cdr.is_cons() {
                    let form_val = cdr.cons_car();
                    let val = eval_form(&form_val, buffers)?;
                    format_mode_line_recursive_in_state_with_eval(
                        obarray,
                        dynamic,
                        buffers,
                        processes,
                        pctx,
                        &val,
                        result,
                        depth + 1,
                        risky,
                        eval_form,
                    )?;
                }
                return Ok(());
            }

            if car.is_symbol_named(":propertize") {
                if risky {
                    return Ok(());
                }
                if cdr.is_cons() {
                    let elt = cdr.cons_car();
                    let mut nested = ModeLineRendered::default();
                    format_mode_line_recursive_in_state_with_eval(
                        obarray,
                        dynamic,
                        buffers,
                        processes,
                        pctx,
                        &elt,
                        &mut nested,
                        depth + 1,
                        risky,
                        eval_form,
                    )?;
                    nested.overlay_properties(cdr.cons_cdr());
                    result.append_rendered(&nested);
                }
                return Ok(());
            }

            if let Some(lim) = car.as_fixnum() {
                let mut nested = ModeLineRendered::default();
                format_mode_line_recursive_in_state_with_eval(
                    obarray,
                    dynamic,
                    buffers,
                    processes,
                    pctx,
                    &cdr,
                    &mut nested,
                    depth + 1,
                    risky,
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
                        processes,
                        pctx,
                        &branch,
                        result,
                        depth + 1,
                        risky,
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
                        processes,
                        pctx,
                        elem,
                        result,
                        depth + 1,
                        risky,
                        eval_form,
                    )?;
                }
            }
        }

        _ => {
            result.append_string_value_preserving_props(format);
        }
    }

    Ok(())
}

fn format_mode_line_recursive_in_vm_runtime(
    shared: &mut crate::emacs_core::eval::Context,
    args_roots: &[Value],
    pctx: &ModeLinePercentContext,
    format: &Value,
    result: &mut ModeLineRendered,
    depth: usize,
    risky: bool,
) -> Result<(), Flow> {
    if depth > 20 {
        return Ok(());
    }

    match format.kind() {
        ValueKind::Nil => {}

        ValueKind::String => append_mode_line_string_in_state(
            &shared.obarray,
            &[],
            &shared.buffers,
            &shared.processes,
            shared.recursive_command_loop_depth(),
            pctx,
            result,
            format,
            false,
        ),

        ValueKind::Fixnum(_) => {}

        _ if format.is_symbol() => {
            // GNU xdisp.c:28438-28468 — symbol branch of display_mode_element.
            // No special case for mode-line-front-space or
            // mode-line-end-spaces; see the equivalent comment in
            // `format_mode_line_recursive`.
            if let Some(name) = format.as_symbol_name() {
                let value = {
                    let obarray = &shared.obarray;
                    let dynamic = &[];
                    let buffers = &shared.buffers;
                    mode_line_symbol_value_in_state(obarray, dynamic, buffers, name)
                };
                if let Some(val) = value
                    && !val.is_nil()
                {
                    if val.is_string() {
                        append_mode_line_string_in_state(
                            &shared.obarray,
                            &[],
                            &shared.buffers,
                            &shared.processes,
                            shared.recursive_command_loop_depth(),
                            pctx,
                            result,
                            &val,
                            true,
                        );
                    } else {
                        format_mode_line_recursive_in_vm_runtime(
                            shared,
                            args_roots,
                            pctx,
                            &val,
                            result,
                            depth + 1,
                            risky || !mode_line_symbol_is_risky(&shared.obarray, name),
                        )?;
                    }
                }
            }
        }

        _ if format.is_cons() => {
            let car = format.cons_car();
            let cdr = format.cons_cdr();

            if car.is_symbol_named(":eval") {
                if risky {
                    return Ok(());
                }
                if cdr.is_cons() {
                    let form_val = cdr.cons_car();
                    let val = shared.with_gc_scope_result(|eval| {
                        for root in args_roots {
                            eval.push_eval_root(*root);
                        }
                        eval.push_eval_root(form_val);
                        eval.eval_value(&form_val)
                    })?;
                    format_mode_line_recursive_in_vm_runtime(
                        shared,
                        args_roots,
                        pctx,
                        &val,
                        result,
                        depth + 1,
                        risky,
                    )?;
                }
                return Ok(());
            }

            if car.is_symbol_named(":propertize") {
                if risky {
                    return Ok(());
                }
                if cdr.is_cons() {
                    let elt = cdr.cons_car();
                    let mut nested = ModeLineRendered::default();
                    format_mode_line_recursive_in_vm_runtime(
                        shared,
                        args_roots,
                        pctx,
                        &elt,
                        &mut nested,
                        depth + 1,
                        risky,
                    )?;
                    nested.overlay_properties(cdr.cons_cdr());
                    result.append_rendered(&nested);
                }
                return Ok(());
            }

            if let Some(lim) = car.as_fixnum() {
                let mut nested = ModeLineRendered::default();
                format_mode_line_recursive_in_vm_runtime(
                    shared,
                    args_roots,
                    pctx,
                    &cdr,
                    &mut nested,
                    depth + 1,
                    risky,
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
                        let obarray = &shared.obarray;
                        let dynamic = &[];
                        let buffers = &shared.buffers;
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
                            args_roots,
                            pctx,
                            &branch,
                            result,
                            depth + 1,
                            risky,
                        )?;
                    }
                }
                return Ok(());
            }

            if let Some(elements) = list_to_vec(format) {
                for elem in &elements {
                    format_mode_line_recursive_in_vm_runtime(
                        shared,
                        args_roots,
                        pctx,
                        elem,
                        result,
                        depth + 1,
                        risky,
                    )?;
                }
            }
        }

        _ => {
            result.append_string_value_preserving_props(format);
        }
    }

    Ok(())
}

fn expand_mode_line_percent_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    processes: &crate::emacs_core::process::ProcessManager,
    command_loop_depth: usize,
    pctx: &ModeLinePercentContext,
    value: &Value,
    result: &mut ModeLineRendered,
) {
    let fmt_storage = if let Some(string) = value.as_lisp_string() {
        crate::emacs_core::builtins::runtime_string_from_lisp_string(string)
    } else if let Some(text) = value.as_str() {
        text.to_owned()
    } else {
        return;
    };
    let fmt_str = fmt_storage.as_str();
    let buf = buffers.current_buffer();
    let buf_name = buf
        .map(|b| b.name_runtime_string_owned())
        .unwrap_or_else(|| "*scratch*".to_string());
    let file_name_storage = buf.and_then(|b| b.file_name_value().as_runtime_string_owned());
    let file_name = file_name_storage.as_deref().unwrap_or("");
    let modified = buf.map(|b| b.is_modified()).unwrap_or(false);
    let read_only = buf.is_some_and(|b| {
        crate::emacs_core::editfns::buffer_read_only_active_in_state(obarray, dynamic, b)
    });
    let narrowed = buf.is_some_and(|b| b.begv_byte > 0 || b.zv_byte < b.total_bytes());

    let (line_num, col_num) = if let Some(b) = buf {
        prefix_line_and_column(b, b.pt_byte)
    } else {
        (1, 0)
    };

    let chars: Vec<char> = fmt_str.chars().collect();
    let mut index = 0;
    let mut literal_start = 0;

    while index < chars.len() {
        if chars[index] != '%' {
            index += 1;
            continue;
        }

        if literal_start < index {
            result.append_string_char_slice_preserving_props(value, literal_start, index);
        }

        let percent_char_pos = index;
        index += 1;

        let mut field_width = 0_i64;
        while index < chars.len() && chars[index].is_ascii_digit() {
            let digit = chars[index] as u8;
            field_width = field_width * 10 + i64::from(digit - b'0');
            index += 1;
        }

        let props_at_percent = if value.is_string() {
            get_string_text_properties_table_for_value(*value)
                .map(|table| table.get_properties(char_to_byte_pos(fmt_str, percent_char_pos)))
                .unwrap_or_default()
        } else {
            Default::default()
        };

        match chars.get(index).copied() {
            Some('b') => {
                append_mode_line_percent_string_spec(
                    result,
                    &buf_name,
                    &props_at_percent,
                    field_width,
                );
                index += 1;
            }
            Some('f') => {
                append_mode_line_percent_string_spec(
                    result,
                    file_name,
                    &props_at_percent,
                    field_width,
                );
                index += 1;
            }
            Some('i') => {
                let size = buf
                    .map(|buffer| buffer.zv_byte.saturating_sub(buffer.begv_byte))
                    .unwrap_or(0);
                append_mode_line_percent_string_spec(
                    result,
                    &size.to_string(),
                    &props_at_percent,
                    field_width,
                );
                index += 1;
            }
            Some('I') => {
                let size = buf
                    .map(|buffer| buffer.zv_byte.saturating_sub(buffer.begv_byte))
                    .unwrap_or(0);
                append_mode_line_percent_string_spec(
                    result,
                    &mode_line_human_readable_size(size),
                    &props_at_percent,
                    field_width,
                );
                index += 1;
            }
            Some('F') => {
                // GNU xdisp.c:29208 — f->title, f->name, or "Emacs".
                if let Some(frame_name) = pctx.frame_name {
                    append_mode_line_percent_lisp_text_spec(
                        result,
                        &frame_name,
                        &props_at_percent,
                        field_width,
                    );
                } else {
                    append_mode_line_percent_string_spec(
                        result,
                        "Emacs",
                        &props_at_percent,
                        field_width,
                    );
                }
                index += 1;
            }
            Some('*') => {
                append_mode_line_percent_string_spec(
                    result,
                    if read_only {
                        "%"
                    } else if modified {
                        "*"
                    } else {
                        "-"
                    },
                    &props_at_percent,
                    field_width,
                );
                index += 1;
            }
            Some('+') => {
                append_mode_line_percent_string_spec(
                    result,
                    if modified {
                        "*"
                    } else if read_only {
                        "%"
                    } else {
                        "-"
                    },
                    &props_at_percent,
                    field_width,
                );
                index += 1;
            }
            Some('&') => {
                append_mode_line_percent_string_spec(
                    result,
                    if modified { "*" } else { "-" },
                    &props_at_percent,
                    field_width,
                );
                index += 1;
            }
            Some('-') => {
                // GNU xdisp.c:29154-29172 — `%-` dispatch depends on
                // mode_line_target. MODE_LINE_STRING (the default,
                // used by `(format-mode-line FORMAT)`) returns the
                // literal two-dash string "--". MODE_LINE_DISPLAY
                // (used by the redisplay walker) returns
                // `lots_of_dashes` — enough dashes to fill the
                // remaining row width; GNU's caller trims at
                // `it->last_visible_x`.
                //
                // We model this with `pctx.target_width`:
                //   None    -> MODE_LINE_STRING, emit "--"
                //   Some(w) -> MODE_LINE_DISPLAY, emit `w - current`
                //              dashes. The entry point that enables
                //              display mode is
                //              `format_mode_line_for_display` below,
                //              used by the layout engine for TTY and
                //              GUI mode-line rendering.
                // `%-` needs to read `result.char_len()` to compute
                // the dash-fill width (in MODE_LINE_DISPLAY mode),
                // but `append_spec` holds a captured mutable borrow
                // on `result`. Drop the closure here by calling
                // `append_mode_line_rendered_segment` directly with
                // the pre-computed dash string.
                let dash_string: String = match pctx.target_width {
                    None => "--".to_string(),
                    Some(target) => {
                        let current = result.char_len();
                        if target > current {
                            "-".repeat(target - current)
                        } else {
                            "--".to_string()
                        }
                    }
                };
                let mut segment = ModeLineRendered::plain(&dash_string);
                segment.overlay_property_map(props_at_percent.clone());
                append_mode_line_rendered_segment(result, &segment, field_width, 0);
                index += 1;
            }
            Some('%') => {
                append_mode_line_percent_string_spec(result, "%", &props_at_percent, field_width);
                index += 1;
            }
            Some('n') => {
                append_mode_line_percent_string_spec(
                    result,
                    if narrowed { " Narrow" } else { "" },
                    &props_at_percent,
                    field_width,
                );
                index += 1;
            }
            Some('s') => {
                append_mode_line_percent_string_spec(
                    result,
                    mode_line_process_status_in_state(buffers, processes),
                    &props_at_percent,
                    field_width,
                );
                index += 1;
            }
            Some('l') => {
                append_mode_line_percent_string_spec(
                    result,
                    &line_num.to_string(),
                    &props_at_percent,
                    field_width,
                );
                index += 1;
            }
            Some('c') => {
                append_mode_line_percent_string_spec(
                    result,
                    &col_num.to_string(),
                    &props_at_percent,
                    field_width,
                );
                index += 1;
            }
            Some('C') => {
                // GNU: 1-indexed column number at point.
                append_mode_line_percent_string_spec(
                    result,
                    &(col_num + 1).to_string(),
                    &props_at_percent,
                    field_width,
                );
                index += 1;
            }
            Some('m') => {
                // GNU: major mode name from buffer-local `mode-name`.
                if let Some(mode_name) =
                    mode_line_symbol_value_in_state(obarray, dynamic, buffers, "mode-name")
                        .filter(|value| value.is_string())
                {
                    append_mode_line_percent_lisp_text_spec(
                        result,
                        &mode_name,
                        &props_at_percent,
                        field_width,
                    );
                } else {
                    append_mode_line_percent_string_spec(
                        result,
                        "",
                        &props_at_percent,
                        field_width,
                    );
                }
                index += 1;
            }
            Some('p') => {
                // GNU xdisp.c:29406 — percentage of buffer above window top.
                // pos = marker_position(w->start), checks window_end_pos.
                let text = if let Some(b) = buf {
                    let pos = pctx.window_start;
                    let botpos = pctx.window_end;
                    let begv = b.point_min_char();
                    let zv = b.point_max_char().max(b.point_min_char());
                    if botpos >= zv {
                        if pos <= begv {
                            "All".to_owned()
                        } else {
                            "Bottom".to_owned()
                        }
                    } else if pos <= begv {
                        "Top".to_owned()
                    } else {
                        format!("{}%", percent99(pos - begv, zv - begv))
                    }
                } else {
                    String::new()
                };
                append_mode_line_percent_string_spec(result, &text, &props_at_percent, field_width);
                index += 1;
            }
            Some('P') => {
                // GNU xdisp.c:29425 — percentage of buffer above window bottom.
                let text = if let Some(b) = buf {
                    let toppos = pctx.window_start;
                    let botpos = pctx.window_end;
                    let begv = b.point_min_char();
                    let zv = b.point_max_char().max(b.point_min_char());
                    if botpos >= zv {
                        if toppos <= begv {
                            "All".to_owned()
                        } else {
                            "Bottom".to_owned()
                        }
                    } else {
                        let pct = percent99(botpos.saturating_sub(begv), zv.saturating_sub(begv));
                        if toppos <= begv {
                            format!("{}%", pct)
                        } else {
                            format!("Top{}%", pct)
                        }
                    }
                } else {
                    String::new()
                };
                append_mode_line_percent_string_spec(result, &text, &props_at_percent, field_width);
                index += 1;
            }
            Some('o') => {
                // GNU xdisp.c:29386 — degree of travel of window through buffer.
                let text = if let Some(b) = buf {
                    let toppos = pctx.window_start;
                    let botpos = pctx.window_end;
                    let begv = b.point_min_char();
                    let zv = b.point_max_char().max(b.point_min_char());
                    if botpos >= zv {
                        if toppos <= begv {
                            "All".to_owned()
                        } else {
                            "Bottom".to_owned()
                        }
                    } else if toppos <= begv {
                        "Top".to_owned()
                    } else {
                        let top_dist = toppos - begv;
                        let bot_dist = zv - botpos;
                        format!("{}%", percent99(top_dist, top_dist + bot_dist))
                    }
                } else {
                    String::new()
                };
                append_mode_line_percent_string_spec(result, &text, &props_at_percent, field_width);
                index += 1;
            }
            Some('q') => {
                // GNU xdisp.c:29445 — percentage offsets of top and bottom of window.
                let text = if let Some(b) = buf {
                    let toppos = pctx.window_start;
                    let botpos = pctx.window_end;
                    let begv = b.point_min_char();
                    let zv = b.point_max_char().max(b.point_min_char());
                    if toppos <= begv && botpos >= zv {
                        "All   ".to_owned()
                    } else {
                        let range = zv.saturating_sub(begv);
                        let top_pct = if toppos <= begv {
                            0
                        } else {
                            percent99(toppos - begv, range)
                        };
                        let bot_pct = if botpos >= zv {
                            100
                        } else {
                            percent99(botpos.saturating_sub(begv), range)
                        };
                        if top_pct == bot_pct {
                            format!("{}%", top_pct)
                        } else {
                            format!("{}-{}%", top_pct, bot_pct)
                        }
                    }
                } else {
                    String::new()
                };
                append_mode_line_percent_string_spec(result, &text, &props_at_percent, field_width);
                index += 1;
            }
            Some('z') => {
                // GNU xdisp.c:29494 — coding system mnemonic without EOL indicator.
                // On TTY frames GNU includes terminal + keyboard + buffer coding
                // mnemonics, regardless of MODE_LINE_STRING vs MODE_LINE_DISPLAY.
                if pctx.is_tty_frame {
                    append_mode_line_percent_string_spec(
                        result,
                        &format!(
                            "{}{}{}",
                            pctx.terminal_coding_mnemonic,
                            pctx.keyboard_coding_mnemonic,
                            pctx.coding_mnemonic,
                        ),
                        &props_at_percent,
                        field_width,
                    );
                } else {
                    append_mode_line_percent_string_spec(
                        result,
                        &pctx.coding_mnemonic.to_string(),
                        &props_at_percent,
                        field_width,
                    );
                }
                index += 1;
            }
            Some('@') => {
                // GNU xdisp.c:29477 — "@" if default-directory is remote, "-" otherwise.
                let remote =
                    mode_line_symbol_value_in_state(obarray, dynamic, buffers, "default-directory")
                        .and_then(|v| mode_line_runtime_string(&v))
                        .map(|dir| is_remote_directory(&dir))
                        .unwrap_or(false);
                append_mode_line_percent_string_spec(
                    result,
                    if remote { "@" } else { "-" },
                    &props_at_percent,
                    field_width,
                );
                index += 1;
            }
            Some('Z') => {
                // GNU xdisp.c:29496 — coding system mnemonic WITH EOL indicator.
                let mut segment = if pctx.is_tty_frame {
                    ModeLineRendered::plain(format!(
                        "{}{}{}",
                        pctx.terminal_coding_mnemonic,
                        pctx.keyboard_coding_mnemonic,
                        pctx.coding_mnemonic,
                    ))
                } else {
                    ModeLineRendered::plain(pctx.coding_mnemonic.to_string())
                };
                if let Some(eol_indicator) = pctx.eol_indicator {
                    segment.append_string_or_char_value_preserving_props(&eol_indicator);
                } else {
                    segment.push_plain_char(':');
                }
                segment.overlay_property_map(props_at_percent.clone());
                append_mode_line_rendered_segment(result, &segment, field_width, 0);
                index += 1;
            }
            Some(c @ ('[' | ']')) => {
                let repeated = match (c, command_loop_depth) {
                    ('[', depth) if depth > 5 => "[[[... ".to_string(),
                    (']', depth) if depth > 5 => " ...]]]".to_string(),
                    (bracket, depth) => std::iter::repeat_n(bracket, depth).collect(),
                };
                append_mode_line_percent_string_spec(
                    result,
                    &repeated,
                    &props_at_percent,
                    field_width,
                );
                index += 1;
            }
            Some('e') => {
                append_mode_line_percent_string_spec(result, "", &props_at_percent, field_width);
                index += 1;
            }
            Some(' ') => {
                append_mode_line_percent_string_spec(result, " ", &props_at_percent, field_width);
                index += 1;
            }
            Some(c) => {
                let mut unknown = String::from("%");
                unknown.push(c);
                append_mode_line_percent_string_spec(
                    result,
                    &unknown,
                    &props_at_percent,
                    field_width,
                );
                index += 1;
            }
            None => {
                append_mode_line_percent_string_spec(result, "%", &props_at_percent, field_width)
            }
        }

        literal_start = index;
    }

    if literal_start < chars.len() {
        result.append_string_char_slice_preserving_props(value, literal_start, chars.len());
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
    match args[0].kind() {
        ValueKind::Fixnum(v) => {
            if v == 0 {
                Err(signal("args-out-of-range", vec![Value::fixnum(v)]))
            } else if v < 0 {
                Ok(Value::symbol("t"))
            } else {
                Ok(Value::NIL)
            }
        }
        ValueKind::Nil => Ok(Value::NIL),
        _ => Ok(Value::symbol("t")),
    }
}

/// (line-pixel-height) -> integer
///
/// Batch-compatible behavior returns 1.
pub(crate) fn builtin_line_pixel_height(args: Vec<Value>) -> EvalResult {
    expect_args("line-pixel-height", &args, 0)?;
    Ok(Value::fixnum(1))
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

    Ok(Value::cons(Value::fixnum(0), Value::fixnum(0)))
}

/// `(window-text-pixel-size &optional WINDOW FROM TO X-LIMIT Y-LIMIT MODE)` evaluator-backed variant.
///
/// Computes approximate pixel dimensions of text in the window region.
/// Uses the frame's character width/height as a monospace approximation.
pub(crate) fn builtin_window_text_pixel_size_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args_range("window-text-pixel-size", &args, 0, 7)?;
    validate_optional_window_designator_in_state(&eval.frames, args.first(), "window-live-p")?;

    // Get frame metrics
    let frame = eval.frames.selected_frame();
    let char_w = frame.map(|f| f.char_width).unwrap_or(8.0);
    let char_h = frame.map(|f| f.char_height).unwrap_or(16.0);

    let wid = args
        .first()
        .and_then(|v| v.as_window_id().map(crate::window::WindowId))
        .or_else(|| frame.map(|f| f.selected_window));

    let Some(wid) = wid else {
        return Ok(Value::cons(Value::fixnum(0), Value::fixnum(0)));
    };

    // Find buffer for this window
    let buf_id = frame
        .and_then(|f| f.find_window(wid))
        .and_then(|w| w.buffer_id());

    let Some(buf_id) = buf_id else {
        return Ok(Value::cons(Value::fixnum(0), Value::fixnum(0)));
    };

    let buf = eval.buffers.get(buf_id);
    let Some(buf) = buf else {
        return Ok(Value::cons(Value::fixnum(0), Value::fixnum(0)));
    };

    // Determine FROM/TO range
    let from_pos = args
        .get(1)
        .and_then(|v| if v.is_nil() { None } else { v.as_int() })
        .map(|i| (i.max(1) - 1) as usize)
        .unwrap_or(0);
    let to_pos = args
        .get(2)
        .and_then(|v| if v.is_nil() { None } else { v.as_int() })
        .map(|i| (i.max(1) - 1) as usize)
        .unwrap_or(buf.total_bytes());

    // Count lines and max columns in the region
    let mut bytes = Vec::new();
    buf.copy_emacs_bytes_to(from_pos, to_pos.min(buf.total_bytes()), &mut bytes);
    let (lines, max_cols) = region_text_metrics(&bytes, buf.get_multibyte());

    let width = (max_cols as f32 * char_w).ceil() as i64;
    let height = (lines as f32 * char_h).ceil() as i64;

    Ok(Value::cons(Value::fixnum(width), Value::fixnum(height)))
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
        if !pos.is_nil() && !pos.is_t() && !pos.is_symbol_named("t") {
            expect_integer_or_marker(pos)?;
        }
    }
    Ok(Value::NIL)
}

/// `(pos-visible-in-window-p &optional POS WINDOW PARTIALLY)` evaluator-backed variant.
///
/// Mirror GNU Emacs: return t if POS is visible in WINDOW, nil otherwise.
/// Checks if position is between window-start and an estimated window-end.
pub(crate) fn builtin_pos_visible_in_window_p_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    pos_visible_in_window_p_impl(&mut eval.frames, &mut eval.buffers, args)
}

fn pos_visible_in_window_p_impl(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args_range("pos-visible-in-window-p", &args, 0, 3)?;
    validate_optional_window_designator_in_state(&*frames, args.get(1), "window-live-p")?;
    let partially = args.get(2).is_some_and(|v| v.is_truthy());
    if let Some((_, metrics)) =
        resolve_exact_visible_metrics(frames, buffers, args.get(1), args.first())?
    {
        if !partially {
            return Ok(Value::T);
        }
        return Ok(Value::list(vec![
            Value::fixnum(metrics.x),
            Value::fixnum(metrics.y),
        ]));
    }
    let Some(ctx) = resolve_live_window_display_context(frames, buffers, args.get(1))? else {
        return Ok(Value::NIL);
    };
    let Some(pos_lisp) = resolve_pos_visible_target_lisp_pos(&ctx, args.first())? else {
        return Ok(Value::NIL);
    };
    let Some(metrics) = approximate_pos_visible_metrics(&ctx, pos_lisp) else {
        return Ok(Value::NIL);
    };
    if !partially && !metrics.fully_visible {
        return Ok(Value::NIL);
    }
    if !partially {
        return Ok(Value::T);
    }
    let mut out = vec![Value::fixnum(metrics.x), Value::fixnum(metrics.y)];
    if !metrics.fully_visible {
        out.extend([
            Value::fixnum(metrics.rtop),
            Value::fixnum(metrics.rbot),
            Value::fixnum(metrics.row_height),
            Value::fixnum(metrics.vpos),
        ]);
    }
    Ok(Value::list(out))
}

/// `(window-line-height &optional LINE WINDOW)` evaluator-backed variant.
///
/// GNU Emacs returns `(HEIGHT VPOS YPOS OFFBOT)` for a live GUI window.  We
/// approximate this from the current frame/window geometry so commands in
/// `simple.el` can reason about visual line movement without batch fallbacks.
pub(crate) fn builtin_window_line_height(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    window_line_height_impl(&mut eval.frames, &mut eval.buffers, args)
}

fn window_line_height_impl(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args_range("window-line-height", &args, 0, 2)?;
    validate_optional_window_designator_in_state(&*frames, args.get(1), "window-live-p")?;
    if let Some((fid, wid)) = resolve_live_window_identity(frames, args.get(1))? {
        if let Some(frame) = frames.get(fid) {
            if let Some(snapshot) = frame.window_display_snapshot(wid) {
                let line_spec = args.first().copied().unwrap_or(Value::NIL);
                let exact_row = if line_spec.is_nil() {
                    resolve_exact_visible_metrics(frames, buffers, args.get(1), None)?
                        .and_then(|(_, metrics)| snapshot.row_metrics(metrics.row))
                } else if line_spec.is_symbol_named("mode-line")
                    || line_spec.is_symbol_named("header-line")
                    || line_spec.is_symbol_named("tab-line")
                {
                    None
                } else {
                    let line_num = match line_spec.kind() {
                        ValueKind::Fixnum(n) => n,
                        _other => {
                            return Err(signal(
                                "wrong-type-argument",
                                vec![Value::symbol("integerp"), line_spec],
                            ));
                        }
                    };
                    let row = if line_num < 0 {
                        snapshot.rows.len() as i64 + line_num
                    } else {
                        line_num
                    };
                    snapshot.row_metrics(row)
                };
                if let Some(row) = exact_row {
                    return Ok(Value::list(vec![
                        Value::fixnum(row.height),
                        Value::fixnum(row.row),
                        Value::fixnum(row.y),
                        Value::fixnum(0),
                    ]));
                }
            }
        }
    }
    let Some(ctx) = resolve_live_window_display_context(frames, buffers, args.get(1))? else {
        return Ok(Value::NIL);
    };

    let line_spec = args.first().copied().unwrap_or(Value::NIL);
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
        let line_num = match line_spec.kind() {
            ValueKind::Fixnum(n) => n,
            _other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), line_spec],
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
        return Ok(Value::NIL);
    };
    Ok(Value::list(vec![
        Value::fixnum(metrics.height),
        Value::fixnum(metrics.vpos),
        Value::fixnum(metrics.ypos),
        Value::fixnum(metrics.offbot),
    ]))
}

/// (move-point-visually DIRECTION) -> boolean
///
/// Batch semantics: direction is validated as a fixnum and the command
/// signals `args-out-of-range` in non-window contexts.
pub(crate) fn builtin_move_point_visually(args: Vec<Value>) -> EvalResult {
    expect_args("move-point-visually", &args, 1)?;
    match args[0].kind() {
        ValueKind::Fixnum(v) => Err(signal(
            "args-out-of-range",
            vec![Value::fixnum(v), Value::fixnum(v)],
        )),
        _other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("fixnump"), args[0]],
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
    Ok(Value::NIL)
}

/// (current-bidi-paragraph-direction &optional BUFFER) -> symbol
///
/// Get the bidi paragraph direction. Returns the symbol 'left-to-right.
pub(crate) fn builtin_current_bidi_paragraph_direction(args: Vec<Value>) -> EvalResult {
    expect_args_range("current-bidi-paragraph-direction", &args, 0, 1)?;
    if let Some(bufferish) = args.first() {
        if !bufferish.is_nil() && !bufferish.is_buffer() {
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
    Ok(Value::NIL)
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
        return Ok(Value::NIL);
    }
    if !third.is_string() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *third],
        ));
    }
    Ok(Value::NIL)
}

/// (move-to-window-line ARG) -> integer or nil
///
/// Move point to the beginning of the ARG-th screen line from the top of
/// the selected window.  If ARG is nil, move to the middle line.  If ARG
/// is negative, count from the bottom.  Returns the window line number
/// (0-indexed from the top).
pub(crate) fn builtin_move_to_window_line(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("move-to-window-line", &args, 1)?;

    // Find selected window's window-start, buffer, and bounds.
    let frame = eval
        .frames
        .selected_frame()
        .ok_or_else(|| signal("error", vec![Value::string("No selected frame")]))?;
    let wid = frame.selected_window;
    let is_mini = frame.minibuffer_window == Some(wid);
    let ch = frame.char_height.max(1.0);
    let (ws, buf_id, bounds_height) = match frame.find_window(wid) {
        Some(Window::Leaf {
            window_start,
            buffer_id,
            bounds,
            ..
        }) => (*window_start, *buffer_id, bounds.height),
        _ => {
            return Err(signal(
                "error",
                vec![Value::string("Selected window is not a leaf window")],
            ));
        }
    };

    let buf = eval
        .buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("No buffer in selected window")]))?;

    // Determine visible body lines for this window.
    let total_body_lines = {
        let total_lines = (bounds_height / ch) as usize;
        if is_mini {
            total_lines
        } else {
            total_lines.saturating_sub(1) // subtract mode line
        }
    };
    let total_body_lines = total_body_lines.max(1);

    // Determine target line number (0-indexed from window top).
    let target_line: usize = if args[0].is_nil() {
        total_body_lines / 2
    } else {
        let n = match args[0].kind() {
            ValueKind::Fixnum(v) => v,
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), args[0]],
                ));
            }
        };
        if n >= 0 {
            (n as usize).min(total_body_lines.saturating_sub(1))
        } else {
            let from_bottom = (-n) as usize;
            total_body_lines.saturating_sub(from_bottom)
        }
    };

    // Walk from window-start forward, counting newlines, to find the
    // character position at the start of `target_line`.
    let text = buf.text.to_string();
    let char_count = buf.text.char_count();
    let start_char = ws.saturating_sub(1); // window_start is 1-based
    let mut lines_seen: usize = 0;
    let mut target_char_pos = start_char; // fallback: stay at window-start

    if target_line == 0 {
        target_char_pos = start_char;
    } else {
        let mut char_idx = 0usize;
        for (_, c) in text.char_indices().skip(start_char) {
            if c == '\n' {
                lines_seen += 1;
                if lines_seen == target_line {
                    target_char_pos = start_char + char_idx + 1;
                    break;
                }
            }
            char_idx += 1;
        }
        // If we exhausted the text before reaching target_line, go to buffer end.
        if lines_seen < target_line {
            target_char_pos = char_count;
        }
    }

    // Convert 0-based char pos to 1-based Lisp pos, then to byte pos.
    let lisp_pos = (target_char_pos + 1) as i64;
    let byte_pos = eval
        .buffers
        .get(buf_id)
        .map(|b| b.lisp_pos_to_byte(lisp_pos))
        .unwrap_or(0);
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = eval.buffers.goto_buffer_byte(current_id, byte_pos);

    Ok(Value::fixnum(target_line as i64))
}

/// (tool-bar-height &optional FRAME PIXELWISE) -> integer
///
/// Get the height of the tool bar. Returns 0 (no tool bar).
pub(crate) fn builtin_tool_bar_height(args: Vec<Value>) -> EvalResult {
    expect_args_range("tool-bar-height", &args, 0, 2)?;
    // Return 0 (no tool bar)
    Ok(Value::fixnum(0))
}

/// `(tool-bar-height &optional FRAME PIXELWISE)` evaluator-backed variant.
///
/// Accepts nil or a live frame designator for FRAME.
pub(crate) fn builtin_tool_bar_height_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args_range("tool-bar-height", &args, 0, 2)?;
    let fid = match args.first().filter(|frame| !frame.is_nil()) {
        Some(frame) => super::window_cmds::resolve_frame_id_in_state(
            &mut eval.frames,
            &mut eval.buffers,
            Some(frame),
            "framep",
        )?,
        None => super::window_cmds::ensure_selected_frame_id_in_state(
            &mut eval.frames,
            &mut eval.buffers,
        ),
    };
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    let lines = frame
        .frame_parameter_int("tool-bar-lines")
        .unwrap_or(0)
        .max(0);
    if args.get(1).is_some_and(|pixelwise| !pixelwise.is_nil()) {
        Ok(Value::fixnum(frame.tool_bar_height as i64))
    } else {
        Ok(Value::fixnum(lines))
    }
}

/// (tab-bar-height &optional FRAME PIXELWISE) -> integer
///
/// Get the height of the tab bar. Returns 0 (no tab bar).
pub(crate) fn builtin_tab_bar_height(args: Vec<Value>) -> EvalResult {
    expect_args_range("tab-bar-height", &args, 0, 2)?;
    // Return 0 (no tab bar)
    Ok(Value::fixnum(0))
}

/// `(tab-bar-height &optional FRAME PIXELWISE)` evaluator-backed variant.
///
/// Accepts nil or a live frame designator for FRAME.
pub(crate) fn builtin_tab_bar_height_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args_range("tab-bar-height", &args, 0, 2)?;
    let fid = match args.first().filter(|frame| !frame.is_nil()) {
        Some(frame) => super::window_cmds::resolve_frame_id_in_state(
            &mut eval.frames,
            &mut eval.buffers,
            Some(frame),
            "framep",
        )?,
        None => super::window_cmds::ensure_selected_frame_id_in_state(
            &mut eval.frames,
            &mut eval.buffers,
        ),
    };
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    let lines = frame
        .frame_parameter_int("tab-bar-lines")
        .unwrap_or(0)
        .max(0);
    if args.get(1).is_some_and(|pixelwise| !pixelwise.is_nil()) {
        Ok(Value::fixnum(frame.tab_bar_height as i64))
    } else {
        Ok(Value::fixnum(lines))
    }
}

/// (line-number-display-width &optional ON-DISPLAY) -> integer
///
/// Get the width of the line number display. Returns 0 (no line numbers).
pub(crate) fn builtin_line_number_display_width(args: Vec<Value>) -> EvalResult {
    expect_args_range("line-number-display-width", &args, 0, 1)?;
    // Return 0 (no line numbers)
    Ok(Value::fixnum(0))
}

/// (long-line-optimizations-p) -> boolean
///
/// Check if long-line optimizations are enabled. Returns nil.
pub(crate) fn builtin_long_line_optimizations_p(args: Vec<Value>) -> EvalResult {
    expect_args("long-line-optimizations-p", &args, 0)?;
    // Return nil (optimizations not enabled)
    Ok(Value::NIL)
}

fn validate_optional_frame_designator(
    eval: &super::eval::Context,
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
    if let Some(id) = frameish.as_frame_id() {
        if frames.get(FrameId(id)).is_some() {
            return Ok(());
        }
    } else if let Some(id) = frameish.as_fixnum().filter(|&id| id >= 0) {
        if frames.get(FrameId(id as u64)).is_some() {
            return Ok(());
        }
    }
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("framep"), *frameish],
    ))
}

fn validate_optional_window_designator(
    eval: &super::eval::Context,
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
    let wid = if let Some(id) = windowish.as_window_id() {
        Some(WindowId(id))
    } else if let Some(id) = windowish.as_fixnum().filter(|&id| id >= 0) {
        Some(WindowId(id as u64))
    } else {
        None
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
    eval: &super::eval::Context,
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
    if let Some(id) = bufferish.as_buffer_id() {
        if buffers.get(id).is_some() {
            return Ok(());
        }
    }
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("bufferp"), *bufferish],
    ))
}

fn resolve_optional_window_buffer(
    eval: &super::eval::Context,
    value: Option<&Value>,
) -> Option<BufferId> {
    let windowish = value?;
    if windowish.is_nil() {
        return None;
    }

    let wid = if let Some(id) = windowish.as_window_id() {
        Some(WindowId(id))
    } else if let Some(id) = windowish.as_fixnum().filter(|&id| id >= 0) {
        Some(WindowId(id as u64))
    } else {
        None
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

    let wid = if let Some(id) = windowish.as_window_id() {
        Some(WindowId(id))
    } else if let Some(id) = windowish.as_fixnum().filter(|&id| id >= 0) {
        Some(WindowId(id as u64))
    } else {
        None
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
    eval: &super::eval::Context,
    window: Option<&Value>,
    buffer: Option<&Value>,
) -> Option<BufferId> {
    if let Some(buf_val) = buffer {
        if let Some(id) = buf_val.as_buffer_id() {
            return Some(id);
        }
    }
    resolve_optional_window_buffer(eval, window)
}

fn resolve_mode_line_buffer_in_state(
    frames: &crate::window::FrameManager,
    window: Option<&Value>,
    buffer: Option<&Value>,
) -> Option<BufferId> {
    if let Some(buf_val) = buffer {
        if let Some(id) = buf_val.as_buffer_id() {
            return Some(id);
        }
    }
    resolve_optional_window_buffer_in_state(frames, window)
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
    let window_point = if frame.selected_window == wid {
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
    let wid = if let Some(id) = windowish.as_window_id() {
        WindowId(id)
    } else if let Some(id) = windowish.as_fixnum().filter(|&id| id >= 0) {
        WindowId(id as u64)
    } else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("window-live-p"), *windowish],
        ));
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
        Some(value) if value.is_t() || value.is_symbol_named("t") => {
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

#[derive(Clone, Copy)]
struct ExactVisibleMetrics {
    point: usize,
    x: i64,
    y: i64,
    width: i64,
    height: i64,
    row: i64,
    col: i64,
}

fn exact_metrics_from_point(point: &DisplayPointSnapshot) -> ExactVisibleMetrics {
    ExactVisibleMetrics {
        point: point.buffer_pos,
        x: point.x,
        y: point.y,
        width: point.width.max(1),
        height: point.height.max(1),
        row: point.row,
        col: point.col,
    }
}

fn resolve_exact_visible_metrics(
    frames: &crate::window::FrameManager,
    buffers: &crate::buffer::BufferManager,
    window: Option<&Value>,
    pos: Option<&Value>,
) -> Result<Option<(WindowId, ExactVisibleMetrics)>, Flow> {
    let Some((fid, wid)) = resolve_live_window_identity(frames, window)? else {
        return Ok(None);
    };
    let Some(frame) = frames.get(fid) else {
        return Ok(None);
    };
    let Some(snapshot) = frame.window_display_snapshot(wid) else {
        return Ok(None);
    };
    let Some(ctx) = resolve_live_window_display_context(frames, buffers, window)? else {
        return Ok(None);
    };
    let Some(pos_lisp) = resolve_pos_visible_target_lisp_pos(&ctx, pos)? else {
        return Ok(None);
    };
    let Some(point) = snapshot.point_for_buffer_pos(pos_lisp) else {
        return Ok(None);
    };
    Ok(Some((wid, exact_metrics_from_point(point))))
}

fn make_text_area_position(window_id: WindowId, metrics: ExactVisibleMetrics) -> Value {
    Value::list(vec![
        Value::make_window(window_id.0),
        Value::fixnum(metrics.point as i64),
        Value::cons(Value::fixnum(metrics.x), Value::fixnum(metrics.y)),
        Value::fixnum(0),
        Value::NIL,
        Value::fixnum(metrics.point as i64),
        Value::cons(Value::fixnum(metrics.col), Value::fixnum(metrics.row)),
        Value::NIL,
        Value::cons(Value::fixnum(0), Value::fixnum(0)),
        Value::cons(Value::fixnum(metrics.width), Value::fixnum(metrics.height)),
    ])
}

fn resolve_posn_at_xy_window(
    frames: &crate::window::FrameManager,
    frame_or_window: Option<&Value>,
) -> Result<Option<(FrameId, WindowId, bool)>, Flow> {
    let Some(frameish) = frame_or_window else {
        return Ok(frames
            .selected_frame()
            .map(|frame| (frame.id, frame.selected_window, true)));
    };
    if frameish.is_nil() {
        return Ok(frames
            .selected_frame()
            .map(|frame| (frame.id, frame.selected_window, true)));
    }
    if let Some(windowish) = resolve_live_window_identity(frames, Some(frameish))? {
        return Ok(Some((windowish.0, windowish.1, true)));
    }
    let fid = if let Some(id) = frameish.as_frame_id() {
        FrameId(id)
    } else if let Some(id) = frameish.as_fixnum().filter(|&id| id >= 0) {
        FrameId(id as u64)
    } else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("framep"), *frameish],
        ));
    };
    let Some(frame) = frames.get(fid) else {
        return Ok(None);
    };
    Ok(Some((fid, frame.selected_window, false)))
}

/// `(posn-at-point &optional POS WINDOW)` evaluator-backed variant.
pub(crate) fn builtin_posn_at_point(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args_range("posn-at-point", &args, 0, 2)?;
    validate_optional_window_designator_in_state(&eval.frames, args.get(1), "window-live-p")?;
    let Some((window_id, metrics)) = resolve_exact_visible_metrics(
        &mut eval.frames,
        &mut eval.buffers,
        args.get(1),
        args.first(),
    )?
    else {
        return Ok(Value::NIL);
    };
    Ok(make_text_area_position(window_id, metrics))
}

/// `(posn-at-x-y X Y &optional FRAME-OR-WINDOW WHOLE)` evaluator-backed variant.
pub(crate) fn builtin_posn_at_x_y(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    posn_at_x_y_impl(&mut eval.frames, &mut eval.buffers, args)
}

fn posn_at_x_y_impl(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args_range("posn-at-x-y", &args, 2, 4)?;
    let x_val = args.first().unwrap();
    let x = match x_val.kind() {
        ValueKind::Fixnum(v) => v,
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("fixnump"), *x_val],
            ));
        }
    };
    let y_val = args.get(1).unwrap();
    let y = match y_val.kind() {
        ValueKind::Fixnum(v) => v,
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("fixnump"), *y_val],
            ));
        }
    };
    let whole = args.get(3).is_some_and(|v| v.is_truthy());
    let Some((fid, wid, window_relative_input)) = resolve_posn_at_xy_window(frames, args.get(2))?
    else {
        return Ok(Value::NIL);
    };
    let Some(frame) = frames.get(fid) else {
        return Ok(Value::NIL);
    };
    let Some(snapshot) = frame.window_display_snapshot(wid) else {
        return Ok(Value::NIL);
    };
    let Some(window_ref) = frame.find_window(wid) else {
        return Ok(Value::NIL);
    };

    let (query_x, query_y) = if window_relative_input {
        let rel_x = if whole {
            x - snapshot.text_area_left_offset
        } else {
            x
        };
        (rel_x, y)
    } else {
        let bounds = window_ref.bounds();
        (
            x - bounds.x.round() as i64 - snapshot.text_area_left_offset,
            y - bounds.y.round() as i64,
        )
    };

    let Some(point) = snapshot.point_at_coords(query_x, query_y) else {
        return Ok(Value::NIL);
    };
    Ok(make_text_area_position(
        wid,
        exact_metrics_from_point(point),
    ))
}

// ---------------------------------------------------------------------------
// Bootstrap variables
// ---------------------------------------------------------------------------

pub fn register_bootstrap_vars(obarray: &mut crate::emacs_core::symbol::Obarray) {
    obarray.set_symbol_value("redisplay--inhibit-bidi", Value::T);
    obarray.set_symbol_value("inhibit-redisplay", Value::NIL);
    obarray.set_symbol_value("blink-matching-delay", Value::fixnum(1));
    obarray.set_symbol_value("blink-matching-paren", Value::T);
    obarray.set_symbol_value("mouse-autoselect-window", Value::NIL);
    obarray.set_symbol_value("auto-resize-tab-bars", Value::T);
    obarray.set_symbol_value("auto-raise-tab-bar-buttons", Value::T);
    obarray.set_symbol_value("auto-resize-tool-bars", Value::T);
    obarray.set_symbol_value("auto-raise-tool-bar-buttons", Value::T);
    obarray.set_symbol_value("tab-bar-truncate", Value::NIL);
    obarray.set_symbol_value("tab-bar-border", Value::symbol("internal-border-width"));
    obarray.set_symbol_value("tab-bar-button-margin", Value::fixnum(1));
    obarray.set_symbol_value("tab-bar-button-relief", Value::fixnum(1));
    obarray.set_symbol_value("tool-bar-border", Value::symbol("internal-border-width"));
    obarray.set_symbol_value("tool-bar-button-margin", Value::fixnum(4));
    obarray.set_symbol_value("tool-bar-button-relief", Value::fixnum(1));
    obarray.set_symbol_value("tool-bar-style", Value::NIL);
    obarray.set_symbol_value("global-font-lock-mode", Value::NIL);
    obarray.set_symbol_value("display-line-numbers", Value::NIL);
    obarray.set_symbol_value("display-line-numbers-type", Value::T);
    obarray.set_symbol_value("display-line-numbers-width", Value::NIL);
    obarray.set_symbol_value("display-line-numbers-current-absolute", Value::T);
    obarray.set_symbol_value("display-line-numbers-widen", Value::NIL);
    obarray.set_symbol_value("display-fill-column-indicator", Value::NIL);
    obarray.set_symbol_value("display-fill-column-indicator-column", Value::NIL);
    obarray.set_symbol_value("display-fill-column-indicator-character", Value::NIL);
    obarray.set_symbol_value("visible-bell", Value::NIL);
    obarray.set_symbol_value("no-redraw-on-reenter", Value::NIL);
    obarray.set_symbol_value("cursor-in-echo-area", Value::NIL);
    obarray.set_symbol_value("truncate-partial-width-windows", Value::fixnum(50));
    obarray.set_symbol_value("mode-line-in-non-selected-windows", Value::T);
    obarray.set_symbol_value("line-number-display-limit", Value::NIL);
    obarray.set_symbol_value("highlight-nonselected-windows", Value::NIL);
    obarray.set_symbol_value("message-truncate-lines", Value::NIL);
    obarray.set_symbol_value("scroll-step", Value::fixnum(0));
    obarray.set_symbol_value("scroll-conservatively", Value::fixnum(0));
    obarray.set_symbol_value("scroll-margin", Value::fixnum(0));
    obarray.set_symbol_value("hscroll-margin", Value::fixnum(5));
    obarray.set_symbol_value("hscroll-step", Value::fixnum(0));
    obarray.set_symbol_value("auto-hscroll-mode", Value::T);
    obarray.set_symbol_value("void-text-area-pointer", Value::symbol("arrow"));
    obarray.set_symbol_value("inhibit-message", Value::NIL);
    obarray.set_symbol_value("make-cursor-line-fully-visible", Value::T);
    obarray.set_symbol_value("x-stretch-cursor", Value::NIL);
    // GNU `src/xdisp.c:38708` (`DEFVAR_BOOL ("inhibit-try-cursor-movement", ...)`)
    // controls the `try_cursor_movement` redisplay optimization. neomacs has
    // no equivalent optimization (the layout engine recomputes per frame),
    // so this knob is currently inert — but the symbol must exist so Lisp
    // code that does `(boundp 'inhibit-try-cursor-movement)` or
    // `(setq inhibit-try-cursor-movement ...)` does not raise void-variable.
    // Cursor audit Finding 7 in `drafts/cursor-audit.md`.
    obarray.set_symbol_value("inhibit-try-cursor-movement", Value::NIL);
    obarray.set_symbol_value("show-trailing-whitespace", Value::NIL);
    obarray.set_symbol_value("show-paren-context-when-offscreen", Value::NIL);
    obarray.set_symbol_value("nobreak-char-display", Value::T);
    obarray.set_symbol_value("overlay-arrow-variable-list", Value::NIL);
    obarray.set_symbol_value("overlay-arrow-string", Value::string("=>"));
    obarray.set_symbol_value("overlay-arrow-position", Value::NIL);
    // Mirror GNU Emacs: set char-table-extra-slots property for all subtypes
    // that need extra slots. Fmake_char_table reads this property to allocate
    // the correct number of extra slots.
    // See: casetab.c:249, category.c:426, character.c:1143, coding.c:11737,
    //      fontset.c:2158-2160, xdisp.c:31594, keymap.c:3346, syntax.c:3659
    obarray.put_property("case-table", "char-table-extra-slots", Value::fixnum(3));
    obarray.put_property("category-table", "char-table-extra-slots", Value::fixnum(2));
    obarray.put_property(
        "char-script-table",
        "char-table-extra-slots",
        Value::fixnum(1),
    );
    obarray.put_property(
        "translation-table",
        "char-table-extra-slots",
        Value::fixnum(2),
    );
    obarray.put_property("fontset", "char-table-extra-slots", Value::fixnum(8));
    obarray.put_property("fontset-info", "char-table-extra-slots", Value::fixnum(1));
    obarray.put_property(
        "glyphless-char-display",
        "char-table-extra-slots",
        Value::fixnum(1),
    );
    obarray.put_property("keymap", "char-table-extra-slots", Value::fixnum(0));
    obarray.put_property("syntax-table", "char-table-extra-slots", Value::fixnum(0));
    obarray.set_symbol_value(
        "char-script-table",
        make_char_table_with_extra_slots(Value::symbol("char-script-table"), Value::NIL, 1),
    );
    obarray.set_symbol_value("pre-redisplay-function", Value::NIL);
    obarray.set_symbol_value("pre-redisplay-functions", Value::NIL);

    // auto-fill-chars: a char-table for characters which invoke auto-filling.
    // Official Emacs (character.c) creates it with sub-type `auto-fill-chars`
    // and sets space and newline to t.
    let auto_fill = make_char_table_value(Value::symbol("auto-fill-chars"), Value::NIL);
    // Set space and newline entries to t.  We use set-char-table-range
    // via the underlying data: store single-char entries.
    use super::chartable::ct_set_single;
    ct_set_single(&auto_fill, ' ' as i64, Value::T);
    ct_set_single(&auto_fill, '\n' as i64, Value::T);
    obarray.set_symbol_value("auto-fill-chars", auto_fill);

    // char-width-table: a char-table for character display widths.
    // Official Emacs (character.c) creates it with default 1.
    obarray.set_symbol_value(
        "char-width-table",
        make_char_table_value(Value::symbol("char-width-table"), Value::fixnum(1)),
    );

    // translation-table-vector: vector recording all translation tables.
    // Official Emacs (character.c) creates a 16-element nil vector.
    obarray.set_symbol_value(
        "translation-table-vector",
        Value::vector(vec![Value::NIL; 16]),
    );

    // translation-hash-table-vector: vector of translation hash tables.
    // Official Emacs (ccl.c) initializes to nil.
    obarray.set_symbol_value("translation-hash-table-vector", Value::NIL);

    // printable-chars: a char-table of printable characters.
    // Official Emacs (character.c) creates it with default t.
    obarray.set_symbol_value(
        "printable-chars",
        make_char_table_value(Value::symbol("printable-chars"), Value::T),
    );

    // default-process-coding-system: cons of coding systems for process I/O.
    // Official Emacs (coding.c) initializes to nil.
    obarray.set_symbol_value("default-process-coding-system", Value::NIL);

    // ambiguous-width-chars: char-table for characters whose width can be 1 or 2.
    // Official Emacs (character.c) creates empty char-table; populated by characters.el.
    obarray.set_symbol_value(
        "ambiguous-width-chars",
        make_char_table_value(Value::NIL, Value::NIL),
    );

    // text-property-default-nonsticky: alist of properties vs non-stickiness.
    // Official Emacs (textprop.c) initializes to ((syntax-table . t) (display . t)).
    obarray.set_symbol_value(
        "text-property-default-nonsticky",
        Value::list(vec![
            Value::cons(Value::symbol("syntax-table"), Value::T),
            Value::cons(Value::symbol("display"), Value::T),
        ]),
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "xdisp_test.rs"]
mod tests;
