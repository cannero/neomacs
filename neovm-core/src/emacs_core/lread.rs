//! Reader-internals builtins: read, read-from-string,
//! eval-buffer, eval-region, read-char, read-event, read-char-exclusive,
//! get-load-suffixes, locate-file, locate-file-internal, read-coding-system,
//! read-non-nil-coding-system.

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::value::*;
use std::path::Path;

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_max_args(name: &str, args: &[Value], max: usize) -> Result<(), Flow> {
    if args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_integer_or_marker(value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *other],
        )),
    }
}

fn expect_string(value: &Value) -> Result<String, Flow> {
    match value {
        Value::Str(id) => Ok(with_heap(|h| h.get_string(*id).to_owned())),
        Value::Symbol(id) => Ok(resolve_sym(*id).to_owned()),
        Value::Nil => Ok("nil".to_string()),
        Value::True => Ok("t".to_string()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
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

fn eval_forms_from_source(eval: &mut super::eval::Evaluator, source: &str) -> EvalResult {
    let (source, shebang_only_line) = strip_reader_prefix(source);
    if shebang_only_line {
        return Err(signal("end-of-file", vec![]));
    }
    if source.is_empty() {
        return Ok(Value::Nil);
    }
    let forms = super::parser::parse_forms(source).map_err(|e| {
        signal(
            "invalid-read-syntax",
            vec![Value::string(e.message.clone())],
        )
    })?;
    for form in forms {
        eval.eval(&form)?;
        eval.gc_safe_point();
    }
    Ok(Value::Nil)
}

fn eval_buffer_source_text(
    eval: &super::eval::Evaluator,
    arg: Option<&Value>,
) -> Result<String, Flow> {
    let buffer_id = match arg {
        None | Some(Value::Nil) => eval
            .buffers
            .current_buffer()
            .map(|b| b.id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?,
        Some(Value::Buffer(id)) => *id,
        Some(Value::Str(id)) => {
            let name = with_heap(|h| h.get_string(*id).to_owned());
            eval.buffers
                .find_buffer_by_name(&name)
                .ok_or_else(|| signal("error", vec![Value::string("No such buffer")]))?
        }
        Some(other) => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };
    eval.buffers
        .get(buffer_id)
        .map(|buffer| buffer.buffer_string())
        .ok_or_else(|| signal("error", vec![Value::string("No such buffer")]))
}

/// `(eval-buffer &optional BUFFER PRINTFLAG FILENAME UNIBYTE DO-ALLOW-PRINT)`
///
/// Evaluate all forms from BUFFER (or current buffer) and return nil.
pub(crate) fn builtin_eval_buffer(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("eval-buffer", &args, 5)?;
    let source = eval_buffer_source_text(eval, args.first())?;
    eval_forms_from_source(eval, &source)
}

/// `(eval-region START END &optional PRINTFLAG READ-FUNCTION)`
///
/// Evaluate forms in the [START, END) region of the current buffer.
pub(crate) fn builtin_eval_region(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("eval-region", &args, 2)?;
    expect_max_args("eval-region", &args, 4)?;

    let (source, start_char_pos, end_char_pos) = {
        let buffer = eval
            .buffers
            .current_buffer()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

        let point_char_pos = buffer.text.byte_to_char(buffer.point()) as i64 + 1;
        let max_char_pos = buffer.text.byte_to_char(buffer.point_max()) as i64 + 1;

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
            return Ok(Value::Nil);
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
        return Ok(Value::Nil);
    }
    eval_forms_from_source(eval, &source)
}

fn event_to_int(event: &Value) -> Option<i64> {
    match event {
        Value::Int(n) => Some(*n),
        Value::Char(c) => Some(*c as i64),
        _ => None,
    }
}

fn expect_optional_prompt_string(args: &[Value]) -> Result<(), Flow> {
    if args.is_empty() || args[0].is_nil() || matches!(args[0], Value::Str(_)) {
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
pub(crate) fn builtin_read_event(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("read-event"), Value::Int(args.len() as i64)],
        ));
    }
    expect_optional_prompt_string(&args)?;
    let seconds_is_nil_or_omitted = args.get(2).is_none_or(Value::is_nil);

    // 1. Check unread-command-events first (both batch and interactive)
    if let Some(event) = eval.pop_unread_command_event() {
        if eval.read_command_keys().is_empty() && seconds_is_nil_or_omitted {
            eval.set_read_command_keys(vec![event]);
        }
        if let Some(n) = event_to_int(&event) {
            return Ok(Value::Int(n));
        }
        return Ok(event);
    }

    // 2. Interactive mode: block on input channel
    if eval.input_rx.is_some() {
        let event = eval.read_char()?;
        if eval.read_command_keys().is_empty() && seconds_is_nil_or_omitted {
            eval.set_read_command_keys(vec![event]);
        }
        if let Some(n) = event_to_int(&event) {
            return Ok(Value::Int(n));
        }
        return Ok(event);
    }

    // 3. Batch mode: no input available
    Ok(Value::Nil)
}

/// `(read-char-exclusive &optional PROMPT INHERIT-INPUT-METHOD SECONDS)`
///
/// Read a character from the command input, discarding non-character events.
/// In batch mode, consumes `unread-command-events` until a character is found.
/// In interactive mode, blocks on the input channel, skipping non-character events.
pub(crate) fn builtin_read_char_exclusive(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("read-char-exclusive"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    expect_optional_prompt_string(&args)?;
    let seconds_is_nil_or_omitted = args.get(2).is_none_or(Value::is_nil);

    // 1. Check unread-command-events first (both batch and interactive)
    while let Some(event) = eval.pop_unread_command_event() {
        if let Some(n) = event_to_int(&event) {
            if eval.read_command_keys().is_empty() && seconds_is_nil_or_omitted {
                eval.set_read_command_keys(vec![event]);
            }
            return Ok(Value::Int(n));
        }
        // Skip non-character events.
    }

    // 2. Interactive mode: block on input channel, skip non-character events
    if eval.input_rx.is_some() {
        loop {
            let event = eval.read_char()?;
            if let Some(n) = event_to_int(&event) {
                if eval.read_command_keys().is_empty() && seconds_is_nil_or_omitted {
                    eval.set_read_command_keys(vec![event]);
                }
                return Ok(Value::Int(n));
            }
            // Non-character event: discard and keep reading
        }
    }

    // 3. Batch mode: no character found
    Ok(Value::Nil)
}

// ---------------------------------------------------------------------------
// Pure builtins
// ---------------------------------------------------------------------------

/// `(get-load-suffixes)`
///
/// Return a list of suffixes that `load` tries when searching for files.
pub(crate) fn builtin_get_load_suffixes(args: Vec<Value>) -> EvalResult {
    expect_max_args("get-load-suffixes", &args, 0)?;
    Ok(Value::list(vec![
        Value::string(".so"),
        Value::string(".so.gz"),
        Value::string(".elc"),
        Value::string(".elc.gz"),
        Value::string(".el"),
        Value::string(".el.gz"),
    ]))
}

/// `(locate-file FILENAME PATH SUFFIXES &optional PREDICATE)`
///
/// Search PATH for FILENAME with each suffix in SUFFIXES.
pub(crate) fn builtin_locate_file(args: Vec<Value>) -> EvalResult {
    expect_min_args("locate-file", &args, 3)?;
    expect_max_args("locate-file", &args, 4)?;
    let filename = expect_string(&args[0])?;
    let path = parse_path_argument(&args[1])?;
    let suffixes = parse_suffixes_argument(&args[2])?;
    Ok(
        match locate_file_with_path_and_suffixes(&filename, &path, &suffixes, args.get(3))? {
            Some(found) => Value::string(found),
            None => Value::Nil,
        },
    )
}

/// `(locate-file-internal FILENAME PATH SUFFIXES &optional PREDICATE)`
///
/// Internal variant of `locate-file`; currently uses the same lookup behavior.
pub(crate) fn builtin_locate_file_internal(args: Vec<Value>) -> EvalResult {
    expect_min_args("locate-file-internal", &args, 3)?;
    expect_max_args("locate-file-internal", &args, 4)?;
    let filename = expect_string(&args[0])?;
    let path = parse_path_argument(&args[1])?;
    let suffixes = parse_suffixes_argument(&args[2])?;
    Ok(
        match locate_file_with_path_and_suffixes(&filename, &path, &suffixes, args.get(3))? {
            Some(found) => Value::string(found),
            None => Value::Nil,
        },
    )
}

/// `(read-coding-system PROMPT &optional DEFAULT-CODING-SYSTEM)`
///
/// In batch mode, this prompts for input and signals end-of-file.
pub(crate) fn builtin_read_coding_system(args: Vec<Value>) -> EvalResult {
    expect_min_args("read-coding-system", &args, 1)?;
    expect_max_args("read-coding-system", &args, 2)?;
    if !matches!(args[0], Value::Str(_)) {
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
                Value::Int(args.len() as i64),
            ],
        ));
    }
    if !matches!(args[0], Value::Str(_)) {
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
        match entry {
            Value::Nil => path.push(".".to_string()),
            Value::Str(id) => path.push(with_heap(|h| h.get_string(id).to_owned())),
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), other],
                ));
            }
        }
    }
    Ok(path)
}

fn parse_suffixes_argument(value: &Value) -> Result<Vec<String>, Flow> {
    let mut suffixes = Vec::new();
    for entry in expect_list(value)? {
        match entry {
            Value::Nil => suffixes.push(String::new()),
            Value::Str(id) => suffixes.push(with_heap(|h| h.get_string(id).to_owned())),
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), other],
                ));
            }
        }
    }
    Ok(suffixes)
}

fn locate_file_with_path_and_suffixes(
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

    let absolute = Path::new(filename).is_absolute();
    if absolute || path.is_empty() {
        for suffix in &effective_suffixes {
            let candidate = format!("{filename}{suffix}");
            if Path::new(&candidate).exists() && predicate_matches_candidate(predicate, &candidate)?
            {
                return Ok(Some(candidate));
            }
        }
        return Ok(None);
    }

    for dir in path {
        let base = Path::new(dir).join(filename);
        let base = base.to_string_lossy();
        for suffix in &effective_suffixes {
            let candidate = format!("{base}{suffix}");
            if Path::new(&candidate).exists() && predicate_matches_candidate(predicate, &candidate)?
            {
                return Ok(Some(candidate));
            }
        }
    }

    Ok(None)
}

fn predicate_matches_candidate(predicate: Option<&Value>, candidate: &str) -> Result<bool, Flow> {
    let Some(predicate) = predicate else {
        return Ok(true);
    };
    if predicate.is_nil() {
        return Ok(true);
    }

    let Some(symbol) = predicate.as_symbol_name() else {
        // We currently only support symbol predicates in pure dispatch;
        // unknown predicate object shapes default to accepting candidate.
        return Ok(true);
    };
    let Some(result) =
        super::builtins::dispatch_builtin_pure(symbol, vec![Value::string(candidate)])
    else {
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
