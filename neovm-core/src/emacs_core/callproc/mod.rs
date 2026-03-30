//! GNU-style synchronous subprocess owner, corresponding to `callproc.c`.
//!
//! Neomacs still shares some low-level argument and routing helpers with
//! `process.rs` during migration, but the synchronous entrypoints live here.

use super::error::{EvalResult, Flow, signal};
use super::value::Value;

fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ));
    }
    Ok(())
}

fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ));
    }
    Ok(())
}

fn maybe_redisplay_sync_output(
    eval: &mut super::eval::Context,
    destination: &Value,
    display: bool,
) -> Result<(), Flow> {
    if display && super::process::destination_writes_to_buffer_in_state(&eval.buffers, destination)?
    {
        eval.redisplay();
    }
    Ok(())
}

pub(crate) fn builtin_call_process(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let destination = args.get(2).copied().unwrap_or(Value::Nil);
    let display = args.get(3).is_some_and(Value::is_truthy);
    let result = super::process::builtin_call_process_impl(&mut eval.buffers, args)?;
    maybe_redisplay_sync_output(eval, &destination, display)?;
    Ok(result)
}

pub(crate) fn builtin_call_process_shell_command(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("call-process-shell-command", &args, 1)?;
    let command = super::process::sequence_value_to_env_string(&args[0])?;
    let infile = super::process::parse_optional_infile(&args, 1)?;
    let destination = args.get(2).copied().unwrap_or(Value::Nil);
    let display = args.get(3).is_some_and(Value::is_truthy);
    let cmd_args = if args.len() > 4 {
        super::process::parse_sequence_args(&args[4..])?
    } else {
        Vec::new()
    };
    let shell_command = super::process::shell_command_with_args(&command, &cmd_args);
    let shell_args = vec!["-c".to_string(), shell_command];
    let result = super::process::run_process_command_in_state(
        &mut eval.buffers,
        "sh",
        infile,
        &destination,
        &shell_args,
    )?;
    maybe_redisplay_sync_output(eval, &destination, display)?;
    Ok(result)
}

pub(crate) fn builtin_process_file(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("process-file", &args, 1)?;
    let program = super::process::expect_string_strict(&args[0])?;
    let infile = super::process::parse_optional_infile(&args, 1)?;
    let destination = args.get(2).copied().unwrap_or(Value::Nil);
    let display = args.get(3).is_some_and(Value::is_truthy);
    let cmd_args = if args.len() > 4 {
        super::process::parse_string_args_strict(&args[4..])?
    } else {
        Vec::new()
    };
    let result = super::process::run_process_command_in_state(
        &mut eval.buffers,
        &program,
        infile,
        &destination,
        &cmd_args,
    )?;
    maybe_redisplay_sync_output(eval, &destination, display)?;
    Ok(result)
}

pub(crate) fn builtin_process_file_shell_command(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("process-file-shell-command", &args, 1)?;
    let command = super::process::sequence_value_to_env_string(&args[0])?;
    let infile = super::process::parse_optional_infile(&args, 1)?;
    let destination = args.get(2).copied().unwrap_or(Value::Nil);
    let display = args.get(3).is_some_and(Value::is_truthy);
    let cmd_args = if args.len() > 4 {
        super::process::parse_sequence_args(&args[4..])?
    } else {
        Vec::new()
    };
    let shell_command = super::process::shell_command_with_args(&command, &cmd_args);
    let shell_args = vec!["-c".to_string(), shell_command];
    let result = super::process::run_process_command_in_state(
        &mut eval.buffers,
        "sh",
        infile,
        &destination,
        &shell_args,
    )?;
    maybe_redisplay_sync_output(eval, &destination, display)?;
    Ok(result)
}

pub(crate) fn builtin_process_lines(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("process-lines", &args, 1)?;
    let program = super::process::expect_string_strict(&args[0])?;
    let cmd_args = super::process::parse_string_args_strict(&args[1..])?;
    let (status, stdout) = super::process::run_process_capture_output(&program, &cmd_args)?;
    if status != 0 {
        return Err(super::process::signal_process_lines_status_error(
            &program, status,
        ));
    }
    Ok(parse_output_lines(&stdout))
}

pub(crate) fn builtin_process_lines_ignore_status(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("process-lines-ignore-status", &args, 1)?;
    let program = super::process::expect_string_strict(&args[0])?;
    let cmd_args = super::process::parse_string_args_strict(&args[1..])?;
    let (_, stdout) = super::process::run_process_capture_output(&program, &cmd_args)?;
    Ok(parse_output_lines(&stdout))
}

pub(crate) fn builtin_process_lines_handling_status(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("process-lines-handling-status", &args, 2)?;
    let program = super::process::expect_string_strict(&args[0])?;
    let status_handler = args[1];
    let cmd_args = super::process::parse_string_args_strict(&args[2..])?;
    let (status, stdout) = super::process::run_process_capture_output(&program, &cmd_args)?;
    let lines = parse_output_lines(&stdout);

    if !status_handler.is_nil() {
        let _ = eval.apply(status_handler, vec![Value::Int(status as i64)])?;
    } else if status != 0 {
        return Err(super::process::signal_process_lines_status_error(
            &program, status,
        ));
    }

    Ok(lines)
}

pub(crate) fn builtin_call_process_region(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("call-process-region", &args, 3)?;
    let destination = args.get(4).copied().unwrap_or(Value::Nil);
    let display = args.get(5).is_some_and(Value::is_truthy);
    let result = super::process::builtin_call_process_region_impl(&mut eval.buffers, args)?;
    maybe_redisplay_sync_output(eval, &destination, display)?;
    Ok(result)
}

fn parse_output_lines(stdout: &[u8]) -> Value {
    let mut text = String::from_utf8_lossy(stdout).into_owned();
    if text.ends_with('\n') {
        text.pop();
    }
    if text.is_empty() {
        Value::Nil
    } else {
        Value::list(text.split('\n').map(Value::string).collect())
    }
}
