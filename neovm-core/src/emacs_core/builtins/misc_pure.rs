use super::*;

// ===========================================================================
// Misc
// ===========================================================================

pub(crate) fn builtin_identity(args: Vec<Value>) -> EvalResult {
    expect_args("identity", &args, 1)?;
    Ok(args[0])
}

pub(crate) fn builtin_prefix_numeric_value(args: Vec<Value>) -> EvalResult {
    expect_args("prefix-numeric-value", &args, 1)?;
    let numeric = match args[0].kind() {
        ValueKind::Nil => 1,
        ValueKind::Symbol(id) if resolve_sym(id) == "-" => -1,
        ValueKind::Fixnum(n) => n,
        ValueKind::Char(c) => c as i64,
        ValueKind::Cons => args[0].cons_car().as_int().unwrap_or(1),
        _ => 1,
    };
    Ok(Value::fixnum(numeric))
}

pub(crate) fn builtin_ignore(_args: Vec<Value>) -> EvalResult {
    Ok(Value::NIL)
}

pub(crate) fn builtin_message(ctx: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("message", &args, 1)?;
    // GNU Emacs: nil or empty string clears the echo area and returns as-is.
    if args[0].is_nil() {
        ctx.clear_current_message();
        return Ok(Value::NIL);
    }
    if args[0].is_string() {
        if with_heap(|h| h.get_string(*id).is_empty()) {
            ctx.clear_current_message();
            return Ok(args[0]);
        }
    }
    // GNU Emacs's `message` ALWAYS calls `format-message` on the args,
    // even for a single string argument.  This converts %% -> % and
    // applies text-quoting (curly quotes).
    let msg = match super::strings::builtin_format_message(ctx, args.clone())?.kind() {
        ValueKind::String => super::strings::builtin_format_message(ctx, args.clone())?.as_str().unwrap().to_owned(),
        _ => String::new(),
    };
    ctx.set_current_message(Some(msg.clone()));
    eprintln!("{}", msg);
    ctx.redisplay();
    Ok(Value::string(msg))
}

pub(crate) fn builtin_message_box(ctx: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("message-box", &args, 1)?;
    if args[0].is_nil() {
        return Ok(Value::NIL);
    }
    // GNU Emacs: always calls format-message, even for single-arg.
    let msg = match super::strings::builtin_format_message(ctx, args.clone())?.kind() {
        ValueKind::String => super::strings::builtin_format_message(ctx, args.clone())?.as_str().unwrap().to_owned(),
        _ => String::new(),
    };
    eprintln!("{}", msg);
    Ok(Value::string(msg))
}

pub(crate) fn builtin_message_or_box(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("message-or-box", &args, 1)?;
    if args[0].is_nil() {
        return Ok(Value::NIL);
    }
    // GNU Emacs: always calls format-message, even for single-arg.
    let msg = match super::strings::builtin_format_message(ctx, args.clone())?.kind() {
        ValueKind::String => super::strings::builtin_format_message(ctx, args.clone())?.as_str().unwrap().to_owned(),
        _ => String::new(),
    };
    eprintln!("{}", msg);
    Ok(Value::string(msg))
}

pub(crate) fn builtin_current_message(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("current-message", &args, 0)?;
    Ok(match ctx.current_message_text() {
        Some(message) => Value::string(message),
        None => Value::NIL,
    })
}

pub(crate) fn builtin_daemonp(args: Vec<Value>) -> EvalResult {
    expect_args("daemonp", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_daemon_initialized(args: Vec<Value>) -> EvalResult {
    expect_args("daemon-initialized", &args, 0)?;
    Err(signal(
        "error",
        vec![Value::string(
            "This function can only be called if emacs is run as a daemon",
        )],
    ))
}

pub(crate) fn builtin_documentation_stringp(args: Vec<Value>) -> EvalResult {
    expect_args("documentation-stringp", &args, 1)?;
    let is_compiled_ref = match args[0].kind() {
        ValueKind::Cons => {
            let pair_car = args[0].cons_car();
            let pair_cdr = args[0].cons_cdr();
            pair_car.as_str().is_some() && pair_cdr.as_int().is_some()
        }
        _ => false,
    };
    Ok(Value::bool_val(
        matches!(args[0], ValueKind::String | Value::fixnum(_)) || is_compiled_ref,
    ))
}

pub(crate) fn builtin_flush_standard_output(args: Vec<Value>) -> EvalResult {
    expect_args("flush-standard-output", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_force_mode_line_update(args: Vec<Value>) -> EvalResult {
    expect_max_args("force-mode-line-update", &args, 1)?;
    Ok(args.first().cloned().unwrap_or(Value::NIL))
}

pub(crate) fn builtin_get_internal_run_time(args: Vec<Value>) -> EvalResult {
    expect_args("get-internal-run-time", &args, 0)?;
    use std::time::{SystemTime, UNIX_EPOCH};
use crate::emacs_core::value::{ValueKind};
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let usecs = dur.subsec_micros() as i64;
    Ok(Value::list(vec![
        Value::fixnum(secs >> 16),
        Value::fixnum(secs & 0xFFFF),
        Value::fixnum(usecs),
        Value::fixnum(0),
    ]))
}

pub(crate) fn builtin_invocation_directory(args: Vec<Value>) -> EvalResult {
    expect_args("invocation-directory", &args, 0)?;
    let mut dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|parent| parent.to_path_buf()))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".to_string());
    if !dir.ends_with('/') {
        dir.push('/');
    }
    Ok(Value::string(dir))
}

pub(crate) fn builtin_invocation_name(args: Vec<Value>) -> EvalResult {
    expect_args("invocation-name", &args, 0)?;
    let name = std::env::current_exe()
        .ok()
        .and_then(|p| {
            p.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "emacs".to_string());
    Ok(Value::string(name))
}

pub(crate) fn builtin_error(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("error", &args, 1)?;
    let msg = match builtin_format_message(eval, args)?.kind() {
        ValueKind::String => builtin_format_message(eval, args)?.as_str().unwrap().to_owned(),
        _ => "error".to_string(),
    };
    Err(signal("error", vec![Value::string(msg)]))
}

pub(crate) fn builtin_user_error(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("user-error", &args, 1)?;
    let msg = match builtin_format_message(eval, args)?.kind() {
        ValueKind::String => builtin_format_message(eval, args)?.as_str().unwrap().to_owned(),
        _ => "user-error".to_string(),
    };
    Err(signal("user-error", vec![Value::string(msg)]))
}

pub(crate) fn builtin_secure_hash_algorithms(args: Vec<Value>) -> EvalResult {
    expect_args("secure-hash-algorithms", &args, 0)?;
    Ok(Value::list(vec![
        Value::symbol("md5"),
        Value::symbol("sha1"),
        Value::symbol("sha224"),
        Value::symbol("sha256"),
        Value::symbol("sha384"),
        Value::symbol("sha512"),
    ]))
}

pub(crate) fn builtin_symbol_name(args: Vec<Value>) -> EvalResult {
    expect_args("symbol-name", &args, 1)?;
    match args[0].as_symbol_name() {
        Some(name) => Ok(Value::string(name)),
        None => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )),
    }
}

pub(crate) fn builtin_make_symbol(args: Vec<Value>) -> EvalResult {
    expect_args("make-symbol", &args, 1)?;
    let name = expect_string(&args[0])?;
    Ok(Value::symbol(intern_uninterned(&name)))
}
