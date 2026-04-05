//! Reader-internals builtins: read, read-from-string,
//! eval-buffer, eval-region, read-char, read-event, read-char-exclusive,
//! get-load-suffixes, locate-file, locate-file-internal, read-coding-system,
//! read-non-nil-coding-system.

use super::error::{EvalResult, Flow, signal};
use super::intern::{intern, resolve_sym};
use super::value::*;
use std::path::Path;

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

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

fn expect_integer_or_marker(value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *value],
        )),
    }
}

fn expect_string(value: &Value) -> Result<String, Flow> {
    match value.kind() {
        ValueKind::String => Ok(value.as_str().unwrap().to_owned()),
        ValueKind::Symbol(id) => Ok(resolve_sym(id).to_owned()),
        ValueKind::Nil => Ok("nil".to_string()),
        ValueKind::T => Ok("t".to_string()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *value],
        )),
    }
}

// ---------------------------------------------------------------------------
// Eval-dependent builtins
// ---------------------------------------------------------------------------

fn strip_reader_prefix(source: &str) -> (&str, bool) {
    if !source.starts_with("#!") {
        return (source, false);
    }
    match source.find('\n') {
        Some(index) => (&source[index + 1..], false),
        None => ("", true),
    }
}


pub(crate) fn eval_forms_from_source(eval: &mut super::eval::Context, source: &str) -> EvalResult {
    // Use eager macro expansion matching GNU Emacs's eval-buffer which calls
    // readevalloop → readevalloop_eager_expand_eval. Without this, macros
    // inside defun bodies won't be expanded when files are loaded through
    // load-source-file-function (load-with-code-conversion).
    //
    // Uses the Value-native reader (no Expr intermediate) with streaming
    // read-eval, matching the approach used for file loading.
    let macroexpand_fn = super::load::get_eager_macroexpand_fn(eval);
    eval_forms_from_source_streaming(eval, source, macroexpand_fn)
}

/// Streaming read-eval loop for source strings, using the Value reader.
fn eval_forms_from_source_streaming(
    eval: &mut super::eval::Context,
    source: &str,
    macroexpand_fn: Option<Value>,
) -> EvalResult {
    let (source, shebang_only_line) = strip_reader_prefix(source);
    if shebang_only_line {
        return Err(signal("end-of-file", vec![]));
    }
    if source.is_empty() {
        return Ok(Value::NIL);
    }

    let mut pos = 0;
    loop {
        let read_result = super::value_reader::read_one(source, pos).map_err(|e| {
            signal(
                "invalid-read-syntax",
                vec![Value::string(format!("Read error: {}", e.message))],
            )
        })?;
        let Some((form, next_pos)) = read_result else {
            break;
        };
        pos = next_pos;

        let saved_temp_roots = eval.save_temp_roots();
        eval.push_temp_root(form);

        let eval_result = if let Some(mexp_fn) = macroexpand_fn {
            super::load::eager_expand_eval(eval, form, mexp_fn).map_err(|e| match e {
                super::error::EvalError::Signal {
                    symbol,
                    data,
                    raw_data,
                } => super::error::Flow::Signal(super::error::SignalData {
                    symbol,
                    data,
                    raw_data,
                    suppress_signal_hook: false,
                    selected_resume: None,
                    search_complete: false,
                }),
                super::error::EvalError::UncaughtThrow { tag, value } => {
                    super::error::Flow::Throw { tag, value }
                }
            })
        } else {
            eval.eval_sub(form)
        };

        eval.restore_temp_roots(saved_temp_roots);
        eval_result?;

        if let Some(mexp_fn) = macroexpand_fn {
            eval.gc_safe_point_exact_with_extra_roots(&[mexp_fn]);
        } else {
            eval.gc_safe_point_exact();
        }
    }

    Ok(Value::NIL)
}

fn map_eval_error_to_flow(err: super::error::EvalError) -> Flow {
    match err {
        super::error::EvalError::Signal {
            symbol,
            data,
            raw_data,
        } => Flow::Signal(super::error::SignalData {
            symbol,
            data,
            raw_data,
            suppress_signal_hook: false,
            selected_resume: None,
            search_complete: false,
        }),
        super::error::EvalError::UncaughtThrow { tag, value } => Flow::Throw { tag, value },
    }
}

pub(crate) fn eval_buffer_source_text_in_state(
    buffers: &crate::buffer::BufferManager,
    arg: Option<&Value>,
) -> Result<String, Flow> {
    let buffer_id = resolve_eval_buffer_id_in_state(buffers, arg)?;
    buffers
        .get(buffer_id)
        .map(|buffer| buffer.buffer_string())
        .ok_or_else(|| signal("error", vec![Value::string("No such buffer")]))
}

fn resolve_eval_buffer_id_in_state(
    buffers: &crate::buffer::BufferManager,
    arg: Option<&Value>,
) -> Result<crate::buffer::BufferId, Flow> {
    match arg {
        None => Ok(buffers
            .current_buffer()
            .map(|b| b.id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?),
        Some(v) if v.is_nil() => Ok(buffers
            .current_buffer()
            .map(|b| b.id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?),
        Some(v) if v.is_buffer() => Ok(v.as_buffer_id().unwrap()),
        Some(v) if v.is_string() => Ok({
            let name = v.as_str().unwrap().to_owned();
            buffers
                .find_buffer_by_name(&name)
                .ok_or_else(|| signal("error", vec![Value::string("No such buffer")]))?
        }),
        Some(other) => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    }
}

fn eval_buffer_filename_in_state(
    buffers: &crate::buffer::BufferManager,
    buffer_id: crate::buffer::BufferId,
    arg: Option<&Value>,
) -> Result<Option<String>, Flow> {
    match arg {
        None => Ok(buffers
            .get(buffer_id)
            .and_then(|buffer| buffer.file_name.clone())),
        Some(v) if v.is_nil() => Ok(buffers
            .get(buffer_id)
            .and_then(|buffer| buffer.file_name.clone())),
        Some(value) => Ok(Some(expect_string(value)?)),
    }
}

fn record_eval_buffer_load_history(eval: &mut super::eval::Context, filename: &str) {
    let path = Path::new(filename);
    let path_str = path.to_string_lossy().to_string();
    let entry = Value::cons(Value::string(path_str.clone()), Value::NIL);
    let history = eval
        .obarray()
        .symbol_value("load-history")
        .cloned()
        .unwrap_or(Value::NIL);
    let filtered_history = Value::list(
        list_to_vec(&history)
            .unwrap_or_default()
            .into_iter()
            .filter(|existing| {
                if existing.is_cons() {
                    existing
                        .cons_car()
                        .as_str()
                        .is_none_or(|loaded| loaded != path_str)
                } else {
                    true
                }
            })
            .collect(),
    );
    eval.set_variable("load-history", Value::cons(entry, filtered_history));
}

pub(crate) fn eval_region_source_text_in_state(
    buffers: &crate::buffer::BufferManager,
    args: &[Value],
) -> Result<String, Flow> {
    expect_min_args("eval-region", args, 2)?;
    expect_max_args("eval-region", args, 4)?;

    let (source, start_char_pos, end_char_pos) = {
        let buffer = buffers
            .current_buffer()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

        let point_char_pos = buffer.point_char() as i64 + 1;
        let max_char_pos = buffer.point_max_char() as i64 + 1;

        let raw_start = if args[0].is_nil() {
            point_char_pos
        } else {
            expect_integer_or_marker(&args[0])?
        };
        let raw_end = if args[1].is_nil() {
            point_char_pos
        } else {
            expect_integer_or_marker(&args[1])?
        };

        if raw_start < 1 || raw_start > max_char_pos || raw_end < 1 || raw_end > max_char_pos {
            return Err(signal("args-out-of-range", vec![args[0], args[1]]));
        }

        if raw_start >= raw_end {
            return Ok(String::new());
        }

        let start_byte = buffer.text.char_to_byte((raw_start - 1) as usize);
        let end_byte = buffer.text.char_to_byte((raw_end - 1) as usize);
        (
            buffer.buffer_substring(start_byte, end_byte),
            raw_start,
            raw_end,
        )
    };

    if start_char_pos >= end_char_pos {
        return Ok(String::new());
    }
    Ok(source)
}

/// `(eval-buffer &optional BUFFER PRINTFLAG FILENAME UNIBYTE DO-ALLOW-PRINT)`
///
/// Evaluate all forms from BUFFER (or current buffer) and return nil.
pub(crate) fn builtin_eval_buffer(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_max_args("eval-buffer", &args, 5)?;
    let buffer_id = resolve_eval_buffer_id_in_state(&eval.buffers, args.first())?;
    let source = eval_buffer_source_text_in_state(&eval.buffers, args.first())?;
    let filename = eval_buffer_filename_in_state(&eval.buffers, buffer_id, args.get(2))?;

    let specpdl_count = eval.specpdl.len();
    let old_lexical = eval.lexical_binding();
    let old_lexenv = eval.lexenv;

    eval.with_gc_scope_result(|ctx| {
        ctx.root(old_lexenv);

        let buffer_value = Value::make_buffer(buffer_id);
        let prior_eval_buffer_list = ctx.visible_variable_value_or_nil("eval-buffer-list");
        ctx.root(buffer_value);
        ctx.root(prior_eval_buffer_list);
        let eval_buffer_list = Value::cons(buffer_value, prior_eval_buffer_list);
        ctx.root(eval_buffer_list);
        ctx.specbind(intern("eval-buffer-list"), eval_buffer_list);

        let do_allow_print = args.get(4).is_some_and(|v| v.is_truthy());
        let standard_output = if args.get(1).is_none_or(|v| v.is_nil()) && !do_allow_print {
            Value::symbol("symbolp")
        } else {
            args.get(1).copied().unwrap_or(Value::NIL)
        };
        ctx.specbind(intern("standard-output"), standard_output);

        if let Some(filename) = filename.as_ref() {
            let filename_value = Value::string(filename.clone());
            ctx.root(filename_value);
            let current_load_list = Value::cons(filename_value, Value::NIL);
            ctx.root(current_load_list);
            ctx.specbind(intern("current-load-list"), current_load_list);
        }

        let lexical_binding = if let Some(binding) = ctx
            .buffers
            .get(buffer_id)
            .and_then(|buffer| buffer.get_buffer_local_binding("lexical-binding"))
            .and_then(|binding| binding.as_value())
        {
            binding.is_truthy()
        } else {
            match super::load::source_lexical_binding_for_load(ctx, &source, Some(buffer_value)) {
                Ok(enabled) => enabled,
                Err(err) => return Err(map_eval_error_to_flow(err)),
            }
        };
        ctx.set_lexical_binding(lexical_binding);
        ctx.lexenv = if lexical_binding {
            Value::list(vec![Value::T])
        } else {
            Value::NIL
        };

        let loading_source_file = ctx
            .visible_variable_value_or_nil("load-in-progress")
            .is_truthy()
            && filename.is_some();
        let result = if loading_source_file {
            let path = Path::new(
                filename
                    .as_deref()
                    .expect("load-in-progress eval-buffer must have filename"),
            );
            super::load::eval_decoded_source_file_in_context(ctx, path, &source, lexical_binding)
                .map_err(map_eval_error_to_flow)
        } else {
            let result = eval_forms_from_source(ctx, &source);
            if result.is_ok()
                && let Some(filename) = filename.as_deref()
            {
                record_eval_buffer_load_history(ctx, filename);
            }
            result
        };

        ctx.set_lexical_binding(old_lexical);
        ctx.lexenv = old_lexenv;
        ctx.unbind_to(specpdl_count);

        result
    })
}

/// `(eval-region START END &optional PRINTFLAG READ-FUNCTION)`
///
/// Evaluate forms in the [START, END) region of the current buffer.
pub(crate) fn builtin_eval_region(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let source = eval_region_source_text_in_state(&eval.buffers, &args)?;
    if source.is_empty() {
        return Ok(Value::NIL);
    }
    eval_forms_from_source(eval, &source)
}

pub(crate) fn builtin_eval_buffer_in_vm_runtime(
    shared: &mut super::eval::Context,
    vm_gc_roots: &[Value],
    args: &[Value],
) -> EvalResult {
    let source = eval_buffer_source_text_in_state(&shared.buffers, args.first())?;
    eval_forms_from_source_in_vm_runtime_streaming(shared, vm_gc_roots, args, &source)
}

pub(crate) fn builtin_eval_region_in_vm_runtime(
    shared: &mut super::eval::Context,
    vm_gc_roots: &[Value],
    args: &[Value],
) -> EvalResult {
    let source = eval_region_source_text_in_state(&shared.buffers, args)?;
    if source.is_empty() {
        return Ok(Value::NIL);
    }
    eval_forms_from_source_in_vm_runtime_streaming(shared, vm_gc_roots, args, &source)
}

/// Streaming read-eval for VM runtime callers that need extra GC roots.
fn eval_forms_from_source_in_vm_runtime_streaming(
    shared: &mut super::eval::Context,
    vm_gc_roots: &[Value],
    args: &[Value],
    source: &str,
) -> EvalResult {
    let (source, shebang_only_line) = strip_reader_prefix(source);
    if shebang_only_line {
        return Err(signal("end-of-file", vec![]));
    }
    if source.is_empty() {
        return Ok(Value::NIL);
    }

    let mut pos = 0;
    loop {
        let read_result = super::value_reader::read_one(source, pos).map_err(|e| {
            signal(
                "invalid-read-syntax",
                vec![Value::string(format!("Read error: {}", e.message))],
            )
        })?;
        let Some((form, next_pos)) = read_result else {
            break;
        };
        pos = next_pos;

        shared.with_extra_gc_roots(vm_gc_roots, args, move |eval| {
            eval.push_temp_root(form);
            eval.eval_sub(form)
        })?;
        shared.gc_safe_point_exact_with_extra_root_slices(&[vm_gc_roots, args]);
    }

    Ok(Value::NIL)
}

fn event_to_int(event: &Value) -> Option<i64> {
    match event.kind() {
        ValueKind::Fixnum(n) => Some(n),
        _ => None,
    }
}

fn expect_optional_prompt_string(args: &[Value]) -> Result<(), Flow> {
    if args.is_empty() || args[0].is_nil() || args[0].is_string() {
        return Ok(());
    }
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("stringp"), args[0]],
    ))
}

/// `(read-event &optional PROMPT INHERIT-INPUT-METHOD SECONDS)`
///
/// Read an event from the command input.
/// In batch mode, reads from `unread-command-events`, returns nil if empty.
/// In interactive mode, blocks on the input channel via `read_char()`.
pub(crate) fn builtin_read_event(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    if let Some(value) = builtin_read_event_in_runtime(eval, &args)? {
        return Ok(value);
    }

    finish_read_event_in_eval(eval, &args)
}

pub(crate) fn finish_read_event_in_eval(
    eval: &mut super::eval::Context,
    args: &[Value],
) -> EvalResult {
    finish_read_event_interactive_in_runtime(eval, args)
}

pub(crate) fn finish_read_event_interactive_in_runtime(
    runtime: &mut impl super::reader::KeyboardInputRuntime,
    args: &[Value],
) -> EvalResult {
    if runtime.has_input_receiver() {
        let timeout = super::reader::parse_optional_read_seconds_arg(args.get(2))?;
        let Some(event) = runtime.read_char_with_timeout(timeout)? else {
            return Ok(Value::NIL);
        };
        let seconds_is_nil_or_omitted = args.get(2).is_none_or(|v| v.is_nil());
        if runtime.read_command_keys().is_empty() && seconds_is_nil_or_omitted {
            runtime.set_read_command_keys(vec![event]);
        }
        if let Some(n) = event_to_int(&event) {
            return Ok(Value::fixnum(n));
        }
        return Ok(event);
    }

    Ok(Value::NIL)
}

/// `(read-char-exclusive &optional PROMPT INHERIT-INPUT-METHOD SECONDS)`
///
/// Read a character from the command input, discarding non-character events.
/// In batch mode, consumes `unread-command-events` until a character is found.
/// In interactive mode, blocks on the input channel, skipping non-character events.
pub(crate) fn builtin_read_char_exclusive(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    if let Some(value) = builtin_read_char_exclusive_in_runtime(eval, &args)? {
        return Ok(value);
    }

    finish_read_char_exclusive_in_eval(eval, &args)
}

pub(crate) fn finish_read_char_exclusive_in_eval(
    eval: &mut super::eval::Context,
    args: &[Value],
) -> EvalResult {
    finish_read_char_exclusive_interactive_in_runtime(eval, args)
}

pub(crate) fn finish_read_char_exclusive_interactive_in_runtime(
    runtime: &mut impl super::reader::KeyboardInputRuntime,
    args: &[Value],
) -> EvalResult {
    if runtime.has_input_receiver() {
        let timeout = super::reader::parse_optional_read_seconds_arg(args.get(2))?;
        let deadline = timeout.map(|timeout| std::time::Instant::now() + timeout);
        loop {
            let remaining = deadline
                .map(|deadline| deadline.saturating_duration_since(std::time::Instant::now()));
            let Some(event) = runtime.read_char_with_timeout(remaining)? else {
                return Ok(Value::NIL);
            };
            let seconds_is_nil_or_omitted = args.get(2).is_none_or(|v| v.is_nil());
            if let Some(n) = event_to_int(&event) {
                if runtime.read_command_keys().is_empty() && seconds_is_nil_or_omitted {
                    runtime.set_read_command_keys(vec![event]);
                }
                return Ok(Value::fixnum(n));
            }
        }
    }

    Ok(Value::NIL)
}

pub(crate) fn builtin_read_event_in_runtime(
    runtime: &mut impl super::reader::KeyboardInputRuntime,
    args: &[Value],
) -> Result<Option<Value>, Flow> {
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("read-event"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }
    expect_optional_prompt_string(args)?;
    let seconds_is_nil_or_omitted = args.get(2).is_none_or(|v| v.is_nil());

    if let Some(event) = runtime.pop_unread_command_event() {
        if runtime.read_command_keys().is_empty() && seconds_is_nil_or_omitted {
            runtime.set_read_command_keys(vec![event]);
        }
        if let Some(n) = event_to_int(&event) {
            return Ok(Some(Value::fixnum(n)));
        }
        return Ok(Some(event));
    }

    if runtime.has_input_receiver() {
        Ok(None)
    } else {
        Ok(Some(Value::NIL))
    }
}

pub(crate) fn builtin_read_char_exclusive_in_runtime(
    runtime: &mut impl super::reader::KeyboardInputRuntime,
    args: &[Value],
) -> Result<Option<Value>, Flow> {
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("read-char-exclusive"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }
    expect_optional_prompt_string(args)?;
    let seconds_is_nil_or_omitted = args.get(2).is_none_or(|v| v.is_nil());

    while let Some(event) = runtime.pop_unread_command_event() {
        if let Some(n) = event_to_int(&event) {
            if runtime.read_command_keys().is_empty() && seconds_is_nil_or_omitted {
                runtime.set_read_command_keys(vec![event]);
            }
            return Ok(Some(Value::fixnum(n)));
        }
    }

    if runtime.has_input_receiver() {
        Ok(None)
    } else {
        Ok(Some(Value::NIL))
    }
}

// ---------------------------------------------------------------------------
// Pure builtins
// ---------------------------------------------------------------------------

/// `(get-load-suffixes)`
///
/// Return a list of suffixes that `load` tries when searching for files.
/// GNU lread.c: combines `load-suffixes` with `load-file-rep-suffixes`.
pub(crate) fn builtin_get_load_suffixes(args: Vec<Value>) -> EvalResult {
    expect_max_args("get-load-suffixes", &args, 0)?;
    // GNU: combines load-suffixes (".elc" ".el") with
    // load-file-rep-suffixes (""). Result: (".elc" ".el" "").
    Ok(Value::list(vec![
        Value::string(".elc"),
        Value::string(".el"),
        Value::string(""),
    ]))
}

/// `(locate-file FILENAME PATH SUFFIXES &optional PREDICATE)`
///
/// Search PATH for FILENAME with each suffix in SUFFIXES.
pub(crate) fn builtin_locate_file(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("locate-file", &args, 3)?;
    expect_max_args("locate-file", &args, 4)?;
    let filename = expect_string(&args[0])?;
    let path = parse_path_argument(&args[1])?;
    let suffixes = parse_suffixes_argument(&args[2])?;
    Ok(
        match locate_file_with_path_and_suffixes(eval, &filename, &path, &suffixes, args.get(3))? {
            Some(found) => Value::string(found),
            None => Value::NIL,
        },
    )
}

/// `(locate-file-internal FILENAME PATH SUFFIXES &optional PREDICATE)`
///
/// Internal variant of `locate-file`; currently uses the same lookup behavior.
pub(crate) fn builtin_locate_file_internal(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("locate-file-internal", &args, 2)?;
    expect_max_args("locate-file-internal", &args, 4)?;
    let filename = expect_string(&args[0])?;
    let path = parse_path_argument(&args[1])?;
    // GNU Emacs: SUFFIXES is optional (nil when omitted)
    let suffixes = if args.len() > 2 {
        parse_suffixes_argument(&args[2])?
    } else {
        Vec::new()
    };
    Ok(
        match locate_file_with_path_and_suffixes(eval, &filename, &path, &suffixes, args.get(3))? {
            Some(found) => Value::string(found),
            None => Value::NIL,
        },
    )
}

/// `(read-coding-system PROMPT &optional DEFAULT-CODING-SYSTEM)`
///
/// In batch mode, this prompts for input and signals end-of-file.
pub(crate) fn builtin_read_coding_system(args: Vec<Value>) -> EvalResult {
    expect_min_args("read-coding-system", &args, 1)?;
    expect_max_args("read-coding-system", &args, 2)?;
    if !args[0].is_string() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[0]],
        ));
    }
    Err(signal(
        "end-of-file",
        vec![Value::string("Error reading from stdin")],
    ))
}

/// `(read-non-nil-coding-system PROMPT)`
///
/// In batch mode, this prompts for input and signals end-of-file.
pub(crate) fn builtin_read_non_nil_coding_system(args: Vec<Value>) -> EvalResult {
    if args.len() != 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("read-non-nil-coding-system"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }
    if !args[0].is_string() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[0]],
        ));
    }
    Err(signal(
        "end-of-file",
        vec![Value::string("Error reading from stdin")],
    ))
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn expect_list(value: &Value) -> Result<Vec<Value>, Flow> {
    list_to_vec(value)
        .ok_or_else(|| signal("wrong-type-argument", vec![Value::symbol("listp"), *value]))
}

fn parse_path_argument(value: &Value) -> Result<Vec<String>, Flow> {
    let mut path = Vec::new();
    for entry in expect_list(value)? {
        match entry.kind() {
            ValueKind::Nil => path.push(".".to_string()),
            ValueKind::String => path.push(entry.as_str().unwrap().to_owned()),
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), entry],
                ));
            }
        }
    }
    Ok(path)
}

fn parse_suffixes_argument(value: &Value) -> Result<Vec<String>, Flow> {
    let mut suffixes = Vec::new();
    for entry in expect_list(value)? {
        match entry.kind() {
            ValueKind::Nil => suffixes.push(String::new()),
            ValueKind::String => suffixes.push(entry.as_str().unwrap().to_owned()),
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), entry],
                ));
            }
        }
    }
    Ok(suffixes)
}

fn locate_file_with_path_and_suffixes(
    eval: &mut super::eval::Context,
    filename: &str,
    path: &[String],
    suffixes: &[String],
    predicate: Option<&Value>,
) -> Result<Option<String>, Flow> {
    let effective_suffixes: Vec<&str> = if suffixes.is_empty() {
        vec![""]
    } else {
        suffixes.iter().map(|s| s.as_str()).collect()
    };

    let absolute = crate::emacs_core::fileio::file_name_absolute_p(filename);
    if absolute || path.is_empty() {
        let expanded = crate::emacs_core::fileio::expand_file_name(filename, None);
        for suffix in &effective_suffixes {
            let candidate = format!("{expanded}{suffix}");
            if Path::new(&candidate).exists()
                && predicate_matches_candidate(eval, predicate, &candidate)?
            {
                return Ok(Some(candidate));
            }
        }
        return Ok(None);
    }

    for dir in path {
        let base = crate::emacs_core::fileio::expand_file_name(filename, Some(dir));
        for suffix in &effective_suffixes {
            let candidate = format!("{base}{suffix}");
            if Path::new(&candidate).exists()
                && predicate_matches_candidate(eval, predicate, &candidate)?
            {
                return Ok(Some(candidate));
            }
        }
    }

    Ok(None)
}

fn predicate_matches_candidate(
    eval: &mut super::eval::Context,
    predicate: Option<&Value>,
    candidate: &str,
) -> Result<bool, Flow> {
    let Some(predicate) = predicate else {
        return Ok(true);
    };
    if predicate.is_nil() {
        return Ok(true);
    }

    let Some(symbol) = predicate.as_symbol_name() else {
        // We currently only support symbol predicates via dispatch_subr;
        // unknown predicate object shapes default to accepting candidate.
        return Ok(true);
    };
    let Some(result) = eval.dispatch_subr(symbol, vec![Value::string(candidate)]) else {
        // Emacs locate-file tolerates non-callable predicate values in practice.
        // Keep search behavior instead of surfacing an execution error here.
        return Ok(true);
    };
    Ok(result?.is_truthy())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "lread_test.rs"]
mod tests;
