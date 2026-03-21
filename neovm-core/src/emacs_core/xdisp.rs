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
use crate::buffer::{BufferId, TextPropertyTable};
use crate::encoding::char_to_byte_pos;
use crate::window::{DisplayPointSnapshot, FrameId, Window, WindowId};

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
    processes: &crate::emacs_core::process::ProcessManager,
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
    let face_spec = resolve_mode_line_face_spec(&args);
    let pctx = build_mode_line_percent_context(frames, &*buffers, obarray, args.get(2));
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
        buffers.set_current(buffer_id);
    }

    if needs_eval {
        Ok(None)
    } else {
        Ok(Some(result.into_value(face_spec)))
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
        let face_spec = resolve_mode_line_face_spec(args);
        let pctx = build_mode_line_percent_context(
            &eval.frames,
            &eval.buffers,
            &eval.obarray,
            args.get(2),
        );
        let mut result = ModeLineRendered::default();
        format_mode_line_recursive(eval, &pctx, &format_val, &mut result, 0, false);
        result.into_value(face_spec)
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
        buffers.set_current(buffer_id);
    }

    let result = if args[0].is_nil() {
        Value::string("")
    } else {
        let format_val = args[0];
        let face_spec = resolve_mode_line_face_spec(args);
        let pctx = build_mode_line_percent_context(frames, &*buffers, obarray, args.get(2));
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
        let face_spec = resolve_mode_line_face_spec(&args);
        let pctx = build_mode_line_percent_context(
            &*shared.frames,
            &*shared.buffers,
            &*shared.obarray,
            args.get(2),
        );
        let mut result = ModeLineRendered::default();
        format_mode_line_recursive_in_vm_runtime(
            shared,
            vm_gc_roots,
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
    let Some(buffer_name) = buffers.current_buffer().map(|buffer| buffer.name.as_str()) else {
        return "no process";
    };
    let Some(process_id) = processes.find_by_buffer_name(buffer_name) else {
        return "no process";
    };
    let Some(process) = processes.get_any(process_id) else {
        return "no process";
    };
    match process.status {
        crate::emacs_core::process::ProcessStatus::Run => match process.kind {
            crate::emacs_core::process::ProcessKind::Network => "listen",
            crate::emacs_core::process::ProcessKind::Pipe => "open",
            _ => "run",
        },
        crate::emacs_core::process::ProcessStatus::Stop => "stop",
        crate::emacs_core::process::ProcessStatus::Exit(_) => "exit",
        crate::emacs_core::process::ProcessStatus::Signal(_) => match process.kind {
            crate::emacs_core::process::ProcessKind::Real => "signal",
            _ => "closed",
        },
        crate::emacs_core::process::ProcessStatus::Connect => "connect",
        crate::emacs_core::process::ProcessStatus::Failed => "failed",
    }
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
#[derive(Clone, Default)]
struct ModeLinePercentContext {
    /// Window start position (character offset of first visible character).
    /// Corresponds to `marker_position(w->start)` in GNU.
    window_start: usize,
    /// Window end position (last visible character position).
    /// In GNU this is `BUF_Z(b) - w->window_end_pos`.
    window_end: usize,
    /// Frame name for `%F`.  GNU: `f->title` then `f->name` then "Emacs".
    frame_name: String,
    /// Coding system mnemonic character for `%z`/`%Z`.
    /// GNU: `CODING_ATTR_MNEMONIC` from the coding system spec.
    coding_mnemonic: char,
    /// EOL type string for `%Z` (`:`, `\`, `/`, or undecided).
    eol_indicator: String,
}

/// Build a `ModeLinePercentContext` from frame/window/buffer state.
fn build_mode_line_percent_context(
    frames: &crate::window::FrameManager,
    buffers: &crate::buffer::BufferManager,
    obarray: &crate::emacs_core::symbol::Obarray,
    window_arg: Option<&Value>,
) -> ModeLinePercentContext {
    let mut ctx = ModeLinePercentContext {
        coding_mnemonic: '-',
        eol_indicator: ":".to_string(),
        ..Default::default()
    };

    // --- Frame name (GNU: f->title, f->name, "Emacs") ---
    if let Some(frame) = frames.selected_frame() {
        ctx.frame_name = if !frame.title.is_empty() {
            frame.title.clone()
        } else if !frame.name.is_empty() {
            frame.name.clone()
        } else {
            "Neomacs".to_string()
        };
    } else {
        ctx.frame_name = "Neomacs".to_string();
    }

    // --- Window start/end (GNU: w->start, BUF_Z(b) - w->window_end_pos) ---
    let resolved_window = resolve_mode_line_window(frames, window_arg);
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
            if let Some(buf) = buffers.current_buffer() {
                ctx.window_end = buf
                    .point_max_char()
                    .saturating_add(1)
                    .saturating_sub(*window_end_pos)
                    .saturating_sub(1);
            } else {
                ctx.window_end = ctx.window_start;
            }
        }
    } else if let Some(buf) = buffers.current_buffer() {
        // Fallback: use buffer positions when no window is available.
        ctx.window_start = 0;
        ctx.window_end = buf.point_max_char();
    }

    // --- Coding system mnemonic (GNU: decode_mode_spec_coding) ---
    let cs_name = buffers
        .current_buffer()
        .and_then(|b| b.get_buffer_local("buffer-file-coding-system"))
        .and_then(|v| v.as_symbol_name().map(|s| s.to_string()));
    if let Some(ref name) = cs_name {
        ctx.coding_mnemonic = coding_system_mnemonic_char(name);
        ctx.eol_indicator = coding_system_eol_indicator(obarray, name);
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
            let wid = match windowish {
                Value::Window(id) => Some(crate::window::WindowId(*id)),
                Value::Int(id) if *id >= 0 => Some(crate::window::WindowId(*id as u64)),
                _ => None,
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
fn coding_system_mnemonic_char(cs_name: &str) -> char {
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
fn coding_system_eol_indicator(
    obarray: &crate::emacs_core::symbol::Obarray,
    cs_name: &str,
) -> String {
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
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| ":".to_string())
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
        let Some(text) = value.as_str() else {
            return;
        };
        let byte_offset = self.text.len();
        self.text.push_str(text);
        if let Value::Str(id) = value
            && let Some(props) = get_string_text_properties_table(*id)
        {
            self.text_props.append_shifted(&props, byte_offset);
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
        if let Value::Str(id) = value
            && let Some(props) = get_string_text_properties_table(*id)
        {
            self.text_props
                .append_shifted(&props.slice(byte_start, byte_end), byte_offset);
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
            if let Some(name) = chunk[0].as_symbol_name() {
                self.text_props
                    .put_property(0, self.text.len(), name, chunk[1]);
            }
        }
    }

    fn overlay_property_map(&mut self, props: std::collections::HashMap<String, Value>) {
        if self.text.is_empty() || props.is_empty() {
            return;
        }
        for (name, value) in props {
            self.text_props
                .put_property(0, self.text.len(), &name, value);
        }
    }

    fn apply_default_face(&mut self, face: Value) {
        if self.text.is_empty() {
            return;
        }

        let end = self.text.len();
        let intervals = self.text_props.intervals().to_vec();
        let mut cursor = 0;

        for interval in intervals {
            let start = interval.start.min(end);
            let interval_end = interval.end.min(end);

            if cursor < start {
                self.text_props.put_property(cursor, start, "face", face);
            }

            if start < interval_end {
                let merged_face = interval
                    .properties
                    .get("face")
                    .copied()
                    .map(|existing| Value::list(vec![existing, face]))
                    .unwrap_or(face);
                self.text_props
                    .put_property(start, interval_end, "face", merged_face);
                cursor = interval_end;
            }

            if cursor >= end {
                break;
            }
        }

        if cursor < end {
            self.text_props.put_property(cursor, end, "face", face);
        }
    }

    fn into_value(mut self, face_spec: ModeLineFaceSpec) -> Value {
        if face_spec.no_props {
            return Value::string(self.text);
        }
        if let Some(face) = face_spec.face {
            self.apply_default_face(face);
        }
        let value = Value::string(self.text);
        if let Value::Str(id) = value {
            set_string_text_properties_table(id, self.text_props);
        }
        value
    }
}

fn resolve_mode_line_face_spec(args: &[Value]) -> ModeLineFaceSpec {
    let face = args.get(1).copied().unwrap_or(Value::Nil);
    let no_props = matches!(face, Value::Int(_));
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
    let Some(text) = value.as_str() else {
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
    eval: &mut super::eval::Evaluator,
    pctx: &ModeLinePercentContext,
    format: &Value,
    result: &mut ModeLineRendered,
    depth: usize,
    risky: bool,
) {
    if depth > 20 {
        return; // Guard against infinite recursion
    }

    match format {
        Value::Nil => {}

        Value::Str(_) => append_mode_line_string_in_state(
            &eval.obarray,
            &eval.dynamic,
            &eval.buffers,
            &eval.processes,
            eval.command_loop.recursive_depth,
            pctx,
            result,
            format,
            false,
        ),

        Value::Int(n) => {
            let _ = n;
        }

        _ if format.is_symbol() => {
            if let Some(name) = format.as_symbol_name() {
                if name == "mode-line-front-space" || name == "mode-line-end-spaces" {
                    result.push_plain_char(' ');
                    return;
                }
                if let Some(val) = mode_line_symbol_value_in_state(
                    &eval.obarray,
                    eval.dynamic.as_slice(),
                    &eval.buffers,
                    name,
                ) && !val.is_nil()
                {
                    if val.as_str().is_some() {
                        append_mode_line_string_in_state(
                            &eval.obarray,
                            &eval.dynamic,
                            &eval.buffers,
                            &eval.processes,
                            eval.command_loop.recursive_depth,
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

            if let Value::Int(lim) = car {
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
                    && mode_line_symbol_value_in_state(
                        &eval.obarray,
                        eval.dynamic.as_slice(),
                        &eval.buffers,
                        sym_name,
                    )
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

    match format {
        Value::Nil => {}

        Value::Str(_) => append_mode_line_string_in_state(
            obarray, dynamic, buffers, processes, 0, pctx, result, format, false,
        ),

        Value::Int(_) => {}

        _ if format.is_symbol() => {
            if let Some(name) = format.as_symbol_name() {
                if name == "mode-line-front-space" || name == "mode-line-end-spaces" {
                    result.push_plain_char(' ');
                    return false;
                }
                if let Some(val) = mode_line_symbol_value_in_state(obarray, dynamic, buffers, name)
                    && !val.is_nil()
                {
                    if val.as_str().is_some() {
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

            if let Value::Int(lim) = car {
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

    match format {
        Value::Nil => {}

        Value::Str(_) => append_mode_line_string_in_state(
            obarray, dynamic, buffers, processes, 0, pctx, result, format, false,
        ),

        Value::Int(_) => {}

        _ if format.is_symbol() => {
            if let Some(name) = format.as_symbol_name() {
                if name == "mode-line-front-space" || name == "mode-line-end-spaces" {
                    result.push_plain_char(' ');
                    return Ok(());
                }
                if let Some(val) = mode_line_symbol_value_in_state(obarray, dynamic, buffers, name)
                    && !val.is_nil()
                {
                    if val.as_str().is_some() {
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

            if let Value::Int(lim) = car {
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
    shared: &mut crate::emacs_core::eval::VmSharedState<'_>,
    vm_gc_roots: &[Value],
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

    match format {
        Value::Nil => {}

        Value::Str(_) => append_mode_line_string_in_state(
            &*shared.obarray,
            shared.dynamic.as_slice(),
            &*shared.buffers,
            &*shared.processes,
            shared.recursive_command_loop_depth(),
            pctx,
            result,
            format,
            false,
        ),

        Value::Int(_) => {}

        _ if format.is_symbol() => {
            if let Some(name) = format.as_symbol_name() {
                if name == "mode-line-front-space" || name == "mode-line-end-spaces" {
                    result.push_plain_char(' ');
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
                    if val.as_str().is_some() {
                        append_mode_line_string_in_state(
                            &*shared.obarray,
                            shared.dynamic.as_slice(),
                            &*shared.buffers,
                            &*shared.processes,
                            shared.recursive_command_loop_depth(),
                            pctx,
                            result,
                            &val,
                            true,
                        );
                    } else {
                        format_mode_line_recursive_in_vm_runtime(
                            shared,
                            vm_gc_roots,
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
                        vm_gc_roots,
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

            if let Value::Int(lim) = car {
                let mut nested = ModeLineRendered::default();
                format_mode_line_recursive_in_vm_runtime(
                    shared,
                    vm_gc_roots,
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
                        vm_gc_roots,
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
    let Some(fmt_str) = value.as_str() else {
        return;
    };
    let buf = buffers.current_buffer();
    let buf_name = buf.map(|b| b.name.as_str()).unwrap_or("*scratch*");
    let file_name = buf.and_then(|b| b.file_name.as_deref()).unwrap_or("");
    let modified = buf.map(|b| b.is_modified()).unwrap_or(false);
    let read_only = buf.is_some_and(|b| {
        crate::emacs_core::editfns::buffer_read_only_active_in_state(obarray, dynamic, b)
    });
    let narrowed = buf.is_some_and(|b| b.begv > 0 || b.zv < b.text.len());

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

        let props_at_percent = if let Value::Str(id) = value {
            get_string_text_properties_table(*id)
                .map(|table| table.get_properties(char_to_byte_pos(fmt_str, percent_char_pos)))
                .unwrap_or_default()
        } else {
            Default::default()
        };

        let mut append_spec = |spec: &str| {
            let mut segment = ModeLineRendered::plain(spec);
            segment.overlay_property_map(props_at_percent.clone());
            append_mode_line_rendered_segment(result, &segment, field_width, 0);
        };

        match chars.get(index).copied() {
            Some('b') => {
                append_spec(buf_name);
                index += 1;
            }
            Some('f') => {
                append_spec(file_name);
                index += 1;
            }
            Some('i') => {
                let size = buf
                    .map(|buffer| buffer.zv.saturating_sub(buffer.begv))
                    .unwrap_or(0);
                append_spec(&size.to_string());
                index += 1;
            }
            Some('I') => {
                let size = buf
                    .map(|buffer| buffer.zv.saturating_sub(buffer.begv))
                    .unwrap_or(0);
                append_spec(&mode_line_human_readable_size(size));
                index += 1;
            }
            Some('F') => {
                // GNU xdisp.c:29208 — f->title, f->name, or "Emacs".
                append_spec(&pctx.frame_name);
                index += 1;
            }
            Some('*') => {
                append_spec(if read_only {
                    "%"
                } else if modified {
                    "*"
                } else {
                    "-"
                });
                index += 1;
            }
            Some('+') => {
                append_spec(if modified {
                    "*"
                } else if read_only {
                    "%"
                } else {
                    "-"
                });
                index += 1;
            }
            Some('&') => {
                append_spec(if modified { "*" } else { "-" });
                index += 1;
            }
            Some('-') => {
                append_spec("--");
                index += 1;
            }
            Some('%') => {
                append_spec("%");
                index += 1;
            }
            Some('n') => {
                append_spec(if narrowed { " Narrow" } else { "" });
                index += 1;
            }
            Some('s') => {
                append_spec(mode_line_process_status_in_state(buffers, processes));
                index += 1;
            }
            Some('l') => {
                append_spec(&line_num.to_string());
                index += 1;
            }
            Some('c') => {
                append_spec(&col_num.to_string());
                index += 1;
            }
            Some('C') => {
                // GNU: 1-indexed column number at point.
                append_spec(&(col_num + 1).to_string());
                index += 1;
            }
            Some('m') => {
                // GNU: major mode name from buffer-local `mode-name`.
                let mode_name =
                    mode_line_symbol_value_in_state(obarray, dynamic, buffers, "mode-name")
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .unwrap_or_default();
                append_spec(&mode_name);
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
                append_spec(&text);
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
                append_spec(&text);
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
                append_spec(&text);
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
                append_spec(&text);
                index += 1;
            }
            Some('z') => {
                // GNU xdisp.c:29494 — coding system mnemonic without EOL indicator.
                append_spec(&pctx.coding_mnemonic.to_string());
                index += 1;
            }
            Some('@') => {
                // GNU xdisp.c:29477 — "@" if default-directory is remote, "-" otherwise.
                let remote =
                    mode_line_symbol_value_in_state(obarray, dynamic, buffers, "default-directory")
                        .and_then(|v| v.as_str().map(is_remote_directory))
                        .unwrap_or(false);
                append_spec(if remote { "@" } else { "-" });
                index += 1;
            }
            Some('Z') => {
                // GNU xdisp.c:29496 — coding system mnemonic WITH EOL indicator.
                append_spec(&format!("{}{}", pctx.coding_mnemonic, pctx.eol_indicator));
                index += 1;
            }
            Some(c @ ('[' | ']')) => {
                let repeated = match (c, command_loop_depth) {
                    ('[', depth) if depth > 5 => "[[[... ".to_string(),
                    (']', depth) if depth > 5 => " ...]]]".to_string(),
                    (bracket, depth) => std::iter::repeat_n(bracket, depth).collect(),
                };
                append_spec(&repeated);
                index += 1;
            }
            Some('e') => {
                append_spec("");
                index += 1;
            }
            Some(' ') => {
                append_spec(" ");
                index += 1;
            }
            Some(c) => {
                let mut unknown = String::from("%");
                unknown.push(c);
                append_spec(&unknown);
                index += 1;
            }
            None => append_spec("%"),
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
    let partially = args.get(2).is_some_and(Value::is_truthy);
    if let Some((_, metrics)) =
        resolve_exact_visible_metrics(frames, buffers, args.get(1), args.first())?
    {
        if !partially {
            return Ok(Value::True);
        }
        return Ok(Value::list(vec![
            Value::Int(metrics.x),
            Value::Int(metrics.y),
        ]));
    }
    let Some(ctx) = resolve_live_window_display_context(frames, buffers, args.get(1))? else {
        return Ok(Value::Nil);
    };
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
    if let Some((fid, wid)) = resolve_live_window_identity(frames, args.get(1))? {
        if let Some(frame) = frames.get(fid) {
            if let Some(snapshot) = frame.window_display_snapshot(wid) {
                let line_spec = args.first().copied().unwrap_or(Value::Nil);
                let exact_row = if line_spec.is_nil() {
                    resolve_exact_visible_metrics(frames, buffers, args.get(1), None)?
                        .and_then(|(_, metrics)| snapshot.row_metrics(metrics.row))
                } else if line_spec.is_symbol_named("mode-line")
                    || line_spec.is_symbol_named("header-line")
                    || line_spec.is_symbol_named("tab-line")
                {
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
                        snapshot.rows.len() as i64 + line_num
                    } else {
                        line_num
                    };
                    snapshot.row_metrics(row)
                };
                if let Some(row) = exact_row {
                    return Ok(Value::list(vec![
                        Value::Int(row.height),
                        Value::Int(row.row),
                        Value::Int(row.y),
                        Value::Int(0),
                    ]));
                }
            }
        }
    }
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
        Value::Window(window_id.0),
        Value::Int(metrics.point as i64),
        Value::cons(Value::Int(metrics.x), Value::Int(metrics.y)),
        Value::Int(0),
        Value::Nil,
        Value::Int(metrics.point as i64),
        Value::cons(Value::Int(metrics.col), Value::Int(metrics.row)),
        Value::Nil,
        Value::cons(Value::Int(0), Value::Int(0)),
        Value::cons(Value::Int(metrics.width), Value::Int(metrics.height)),
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
    let fid = match frameish {
        Value::Frame(id) => FrameId(*id),
        Value::Int(id) if *id >= 0 => FrameId(*id as u64),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("framep"), *other],
            ));
        }
    };
    let Some(frame) = frames.get(fid) else {
        return Ok(None);
    };
    Ok(Some((fid, frame.selected_window, false)))
}

/// `(posn-at-point &optional POS WINDOW)` evaluator-backed variant.
pub(crate) fn builtin_posn_at_point_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_posn_at_point_in_state(&mut eval.frames, &mut eval.buffers, args)
}

pub(crate) fn builtin_posn_at_point_in_state(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args_range("posn-at-point", &args, 0, 2)?;
    validate_optional_window_designator_in_state(&*frames, args.get(1), "window-live-p")?;
    let Some((window_id, metrics)) =
        resolve_exact_visible_metrics(frames, buffers, args.get(1), args.first())?
    else {
        return Ok(Value::Nil);
    };
    Ok(make_text_area_position(window_id, metrics))
}

/// `(posn-at-x-y X Y &optional FRAME-OR-WINDOW WHOLE)` evaluator-backed variant.
pub(crate) fn builtin_posn_at_x_y_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_posn_at_x_y_in_state(&mut eval.frames, &mut eval.buffers, args)
}

pub(crate) fn builtin_posn_at_x_y_in_state(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args_range("posn-at-x-y", &args, 2, 4)?;
    let x = match args.first() {
        Some(Value::Int(v)) => *v,
        Some(Value::Char(v)) => *v as i64,
        Some(other) => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("fixnump"), *other],
            ));
        }
        None => unreachable!(),
    };
    let y = match args.get(1) {
        Some(Value::Int(v)) => *v,
        Some(Value::Char(v)) => *v as i64,
        Some(other) => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("fixnump"), *other],
            ));
        }
        None => unreachable!(),
    };
    let whole = args.get(3).is_some_and(Value::is_truthy);
    let Some((fid, wid, window_relative_input)) = resolve_posn_at_xy_window(frames, args.get(2))?
    else {
        return Ok(Value::Nil);
    };
    let Some(frame) = frames.get(fid) else {
        return Ok(Value::Nil);
    };
    let Some(snapshot) = frame.window_display_snapshot(wid) else {
        return Ok(Value::Nil);
    };
    let Some(window_ref) = frame.find_window(wid) else {
        return Ok(Value::Nil);
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
        return Ok(Value::Nil);
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
    obarray.set_symbol_value("redisplay--inhibit-bidi", Value::True);
    obarray.set_symbol_value("blink-matching-delay", Value::Int(1));
    obarray.set_symbol_value("blink-matching-paren", Value::True);
    obarray.set_symbol_value("mouse-autoselect-window", Value::Nil);
    obarray.set_symbol_value("auto-resize-tab-bars", Value::True);
    obarray.set_symbol_value("auto-raise-tab-bar-buttons", Value::True);
    obarray.set_symbol_value("auto-resize-tool-bars", Value::True);
    obarray.set_symbol_value("auto-raise-tool-bar-buttons", Value::True);
    obarray.set_symbol_value("tab-bar-truncate", Value::Nil);
    obarray.set_symbol_value("tab-bar-border", Value::symbol("internal-border-width"));
    obarray.set_symbol_value("tab-bar-button-margin", Value::Int(1));
    obarray.set_symbol_value("tab-bar-button-relief", Value::Int(1));
    obarray.set_symbol_value("tool-bar-border", Value::symbol("internal-border-width"));
    obarray.set_symbol_value("tool-bar-button-margin", Value::Int(4));
    obarray.set_symbol_value("tool-bar-button-relief", Value::Int(1));
    obarray.set_symbol_value("tool-bar-style", Value::Nil);
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
