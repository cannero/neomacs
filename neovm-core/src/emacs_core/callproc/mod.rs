//! GNU-style synchronous subprocess owner, corresponding to `callproc.c`.

use std::fs::OpenOptions;
use std::io::Write;
use std::process::{Command, Stdio};

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::value::{Value, ValueKind, VecLikeType, list_to_vec};
use crate::buffer::BufferManager;

fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ));
    }
    Ok(())
}

fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ));
    }
    Ok(())
}

fn maybe_redisplay_sync_output(
    eval: &mut super::eval::Context,
    destination: &Value,
    display: bool,
) -> Result<(), Flow> {
    if display && destination_writes_to_buffer_in_state(&eval.buffers, destination)? {
        eval.redisplay();
    }
    Ok(())
}

#[derive(Clone, Debug)]
enum OutputTarget {
    Discard,
    Buffer(Value),
    File(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StderrTarget {
    Discard,
    ToStdoutTarget,
    File,
}

#[derive(Clone, Debug)]
struct DestinationSpec {
    stdout: OutputTarget,
    stderr: StderrTarget,
    stderr_file: Option<String>,
    no_wait: bool,
}

fn signal_wrong_type_string(value: Value) -> Flow {
    signal("wrong-type-argument", vec![Value::symbol("stringp"), value])
}

fn callproc_owned_runtime_string(value: Value) -> String {
    value
        .as_runtime_string_owned()
        .expect("ValueKind::String must carry LispString payload")
}

fn is_file_keyword(value: &Value) -> bool {
    value.as_keyword_id().map_or(false, |k| {
        let n = resolve_sym(k);
        n == ":file" || n == "file"
    })
}

fn parse_file_target(items: &[Value]) -> Result<OutputTarget, Flow> {
    let file_value = items.get(1).cloned().unwrap_or(Value::NIL);
    let file = super::process::expect_string_strict(&file_value)?;
    Ok(OutputTarget::File(file))
}

fn parse_real_buffer_destination_in_state(
    buffers: &BufferManager,
    value: &Value,
) -> Result<(OutputTarget, bool), Flow> {
    match value.kind() {
        ValueKind::Fixnum(_) => Ok((OutputTarget::Discard, true)),
        ValueKind::Nil => Ok((OutputTarget::Discard, false)),
        ValueKind::T | ValueKind::String => Ok((OutputTarget::Buffer(*value), false)),
        ValueKind::Veclike(VecLikeType::Buffer) => {
            if buffers.get(value.as_buffer_id().unwrap()).is_none() {
                Err(signal(
                    "error",
                    vec![Value::string("Selecting deleted buffer")],
                ))
            } else {
                Ok((OutputTarget::Buffer(*value), false))
            }
        }
        ValueKind::Cons => {
            let items = list_to_vec(value).ok_or_else(|| signal_wrong_type_string(*value))?;
            let first = items.first().cloned().unwrap_or(Value::NIL);
            if is_file_keyword(&first) {
                Ok((parse_file_target(&items)?, false))
            } else {
                Err(signal_wrong_type_string(first))
            }
        }
        other => Err(signal_wrong_type_string(*value)),
    }
}

fn parse_stderr_destination(value: &Value) -> Result<(StderrTarget, Option<String>), Flow> {
    match value.kind() {
        ValueKind::Nil => Ok((StderrTarget::Discard, None)),
        ValueKind::T => Ok((StderrTarget::ToStdoutTarget, None)),
        ValueKind::String => Ok((
            StderrTarget::File,
            Some(callproc_owned_runtime_string(*value)),
        )),
        other => Err(signal_wrong_type_string(*value)),
    }
}

fn parse_call_process_destination(
    buffers: &BufferManager,
    destination: &Value,
) -> Result<DestinationSpec, Flow> {
    if destination.is_cons() {
        let items =
            list_to_vec(destination).ok_or_else(|| signal_wrong_type_string(*destination))?;
        let first = items.first().cloned().unwrap_or(Value::NIL);
        if is_file_keyword(&first) {
            let stdout = parse_file_target(&items)?;
            return Ok(DestinationSpec {
                stdout,
                stderr: StderrTarget::ToStdoutTarget,
                stderr_file: None,
                no_wait: false,
            });
        }
        let second = items.get(1).cloned().unwrap_or(Value::NIL);
        let (stdout, no_wait) = parse_real_buffer_destination_in_state(buffers, &first)?;
        let (stderr, stderr_file) = parse_stderr_destination(&second)?;
        return Ok(DestinationSpec {
            stdout,
            stderr,
            stderr_file,
            no_wait,
        });
    }

    let (stdout, no_wait) = parse_real_buffer_destination_in_state(buffers, destination)?;
    let stderr = match destination.kind() {
        ValueKind::Nil | ValueKind::Fixnum(_) => StderrTarget::Discard,
        _ => StderrTarget::ToStdoutTarget,
    };
    Ok(DestinationSpec {
        stdout,
        stderr,
        stderr_file: None,
        no_wait,
    })
}

fn destination_writes_to_buffer_in_state(
    buffers: &BufferManager,
    destination: &Value,
) -> Result<bool, Flow> {
    let spec = parse_call_process_destination(buffers, destination)?;
    Ok(matches!(spec.stdout, OutputTarget::Buffer(_)))
}

fn insert_process_output_in_state(
    buffers: &mut BufferManager,
    destination: &Value,
    output: &str,
) -> Result<(), Flow> {
    match destination.kind() {
        ValueKind::String => {
            let name_str = callproc_owned_runtime_string(*destination);
            let id = buffers
                .find_buffer_by_name(&name_str)
                .unwrap_or_else(|| buffers.create_buffer(&name_str));
            buffers.insert_into_buffer(id, output).ok_or_else(|| {
                signal(
                    "error",
                    vec![Value::string("No such live buffer for process output")],
                )
            })?;
            Ok(())
        }
        ValueKind::Veclike(VecLikeType::Buffer) => {
            buffers
                .insert_into_buffer(destination.as_buffer_id().unwrap(), output)
                .ok_or_else(|| signal("error", vec![Value::string("Selecting deleted buffer")]))?;
            Ok(())
        }
        _ => {
            if let Some(current_id) = buffers.current_buffer_id() {
                let _ = buffers.insert_into_buffer(current_id, output);
            }
            Ok(())
        }
    }
}

fn write_output_target_in_state(
    buffers: &mut BufferManager,
    target: &OutputTarget,
    output: &[u8],
    append: bool,
) -> Result<(), Flow> {
    match target {
        OutputTarget::Discard => Ok(()),
        OutputTarget::Buffer(destination) => {
            let text = String::from_utf8_lossy(output).into_owned();
            insert_process_output_in_state(buffers, destination, &text)
        }
        OutputTarget::File(path) => {
            if append {
                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .map_err(|e| {
                        super::process::signal_process_io("Writing process output", Some(path), e)
                    })?;
                file.write_all(output).map_err(|e| {
                    super::process::signal_process_io("Writing process output", Some(path), e)
                })
            } else {
                std::fs::write(path, output).map_err(|e| {
                    super::process::signal_process_io("Writing process output", Some(path), e)
                })
            }
        }
    }
}

fn route_captured_output_in_state(
    buffers: &mut BufferManager,
    destination: &DestinationSpec,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<(), Flow> {
    write_output_target_in_state(buffers, &destination.stdout, stdout, false)?;
    match destination.stderr {
        StderrTarget::Discard => Ok(()),
        StderrTarget::ToStdoutTarget => {
            write_output_target_in_state(buffers, &destination.stdout, stderr, true)
        }
        StderrTarget::File => {
            let path = destination
                .stderr_file
                .as_ref()
                .ok_or_else(|| signal("error", vec![Value::string("Missing stderr file target")]))?
                .clone();
            write_output_target_in_state(buffers, &OutputTarget::File(path), stderr, false)
        }
    }
}

fn configure_call_process_stdin(command: &mut Command, infile: Option<&str>) -> Result<(), Flow> {
    match infile {
        None => {
            command.stdin(Stdio::null());
            Ok(())
        }
        Some(path) => {
            let file = std::fs::File::open(path).map_err(|e| {
                super::process::signal_process_io("Opening process input file", Some(path), e)
            })?;
            command.stdin(Stdio::from(file));
            Ok(())
        }
    }
}

fn run_process_command_in_state(
    buffers: &mut BufferManager,
    program: &str,
    infile: Option<String>,
    destination: &Value,
    cmd_args: &[String],
) -> EvalResult {
    let destination_spec = parse_call_process_destination(buffers, destination)?;

    if destination_spec.no_wait {
        let mut command = Command::new(program);
        command.args(cmd_args).stdout(Stdio::null());
        configure_call_process_stdin(&mut command, infile.as_deref())?;
        match destination_spec.stderr {
            StderrTarget::Discard | StderrTarget::ToStdoutTarget => {
                command.stderr(Stdio::null());
            }
            StderrTarget::File => {
                let path = destination_spec.stderr_file.as_ref().ok_or_else(|| {
                    signal("error", vec![Value::string("Missing stderr file target")])
                })?;
                let file = OpenOptions::new()
                    .create(true)
                    .truncate(true)
                    .write(true)
                    .open(path)
                    .map_err(|e| {
                        super::process::signal_process_io("Writing process output", Some(path), e)
                    })?;
                command.stderr(Stdio::from(file));
            }
        };

        let mut child = command.spawn().map_err(|e| {
            super::process::signal_process_io("Searching for program", Some(program), e)
        })?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        return Ok(Value::NIL);
    }

    let mut command = Command::new(program);
    command
        .args(cmd_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_call_process_stdin(&mut command, infile.as_deref())?;
    let output = command.output().map_err(|e| {
        super::process::signal_process_io("Searching for program", Some(program), e)
    })?;

    let exit_code = output.status.code().unwrap_or(-1);
    route_captured_output_in_state(buffers, &destination_spec, &output.stdout, &output.stderr)?;
    Ok(Value::fixnum(exit_code as i64))
}

fn run_process_capture_output(program: &str, cmd_args: &[String]) -> Result<(i32, Vec<u8>), Flow> {
    let mut command = Command::new(program);
    command
        .args(cmd_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let output = command.output().map_err(|e| {
        super::process::signal_process_io("Searching for program", Some(program), e)
    })?;
    Ok((output.status.code().unwrap_or(-1), output.stdout))
}

fn parse_optional_infile(args: &[Value], index: usize) -> Result<Option<String>, Flow> {
    if args.len() > index && !args[index].is_nil() {
        Ok(Some(super::process::expect_string_strict(&args[index])?))
    } else {
        Ok(None)
    }
}

fn parse_sequence_args(args: &[Value]) -> Result<Vec<String>, Flow> {
    args.iter()
        .map(super::process::sequence_value_to_env_string)
        .collect()
}

fn signal_process_lines_status_error(program: &str, status: i32) -> Flow {
    signal(
        "error",
        vec![Value::string(format!(
            "{program} exited with status {status}"
        ))],
    )
}

fn shell_quote_argument(arg: &str) -> String {
    let mut out = String::from("'");
    for ch in arg.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn shell_command_with_args(command: &str, args: &[String]) -> String {
    if args.is_empty() {
        return command.to_string();
    }
    let quoted = args
        .iter()
        .map(|arg| shell_quote_argument(arg))
        .collect::<Vec<_>>()
        .join(" ");
    if command.is_empty() {
        quoted
    } else {
        format!("{command} {quoted}")
    }
}

fn builtin_call_process_impl(buffers: &mut BufferManager, args: Vec<Value>) -> EvalResult {
    expect_min_args("call-process", &args, 1)?;
    let program = super::process::expect_string_strict(&args[0])?;
    let infile = parse_optional_infile(&args, 1)?;
    let destination = args.get(2).unwrap_or(&Value::NIL);
    let cmd_args = if args.len() > 4 {
        super::process::parse_string_args_strict(&args[4..])?
    } else {
        Vec::new()
    };
    run_process_command_in_state(buffers, &program, infile, destination, &cmd_args)
}

fn builtin_call_process_region_impl(buffers: &mut BufferManager, args: Vec<Value>) -> EvalResult {
    expect_min_args("call-process-region", &args, 3)?;
    let program = super::process::expect_string_strict(&args[2])?;

    let delete = args.len() > 3 && args[3].is_truthy();
    let destination = if args.len() > 4 {
        &args[4]
    } else {
        &Value::NIL
    };
    let destination_spec = parse_call_process_destination(buffers, destination)?;

    let cmd_args = if args.len() > 6 {
        super::process::parse_string_args_strict(&args[6..])?
    } else {
        Vec::new()
    };

    let region_text = match args[0].kind() {
        ValueKind::Nil => {
            let (text, maybe_delete_range) = {
                let buf = buffers
                    .current_buffer()
                    .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
                let len = buf.text.len();
                (buf.text.text_range(0, len), (0usize, len))
            };
            if delete {
                let current_id = buffers
                    .current_buffer_id()
                    .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
                let _ = buffers.delete_buffer_region(
                    current_id,
                    maybe_delete_range.0,
                    maybe_delete_range.1,
                );
            }
            text
        }
        ValueKind::String => {
            if delete {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integer-or-marker-p"), args[0]],
                ));
            }
            callproc_owned_runtime_string(args[0])
        }
        _ => {
            let start = super::process::expect_int_or_marker(&args[0])?;
            let end = super::process::expect_int_or_marker(&args[1])?;
            let (text, region_beg, region_end) = {
                let buf = buffers
                    .current_buffer()
                    .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
                let (region_beg, region_end) =
                    super::process::checked_region_bytes(buf, start, end)?;
                (
                    buf.text.text_range(region_beg, region_end),
                    region_beg,
                    region_end,
                )
            };

            if delete {
                let current_id = buffers
                    .current_buffer_id()
                    .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
                let _ = buffers.delete_buffer_region(current_id, region_beg, region_end);
            }

            text
        }
    };

    if destination_spec.no_wait {
        let mut command = Command::new(&program);
        command
            .args(&cmd_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null());
        match destination_spec.stderr {
            StderrTarget::Discard | StderrTarget::ToStdoutTarget => {
                command.stderr(Stdio::null());
            }
            StderrTarget::File => {
                let path = destination_spec.stderr_file.as_ref().ok_or_else(|| {
                    signal("error", vec![Value::string("Missing stderr file target")])
                })?;
                let file = OpenOptions::new()
                    .create(true)
                    .truncate(true)
                    .write(true)
                    .open(path)
                    .map_err(|e| {
                        super::process::signal_process_io("Writing process output", Some(path), e)
                    })?;
                command.stderr(Stdio::from(file));
            }
        };

        let mut child = command.spawn().map_err(|e| {
            super::process::signal_process_io("Searching for program", Some(&program), e)
        })?;

        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(region_text.as_bytes());
        }

        std::thread::spawn(move || {
            let _ = child.wait();
        });

        return Ok(Value::NIL);
    }

    let mut child = Command::new(&program)
        .args(&cmd_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            super::process::signal_process_io("Searching for program", Some(&program), e)
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(region_text.as_bytes());
    }

    let output = child
        .wait_with_output()
        .map_err(|e| super::process::signal_process_io("Process error", None, e))?;

    let exit_code = output.status.code().unwrap_or(-1);
    route_captured_output_in_state(buffers, &destination_spec, &output.stdout, &output.stderr)?;
    Ok(Value::fixnum(exit_code as i64))
}

pub(crate) fn builtin_call_process(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let destination = args.get(2).copied().unwrap_or(Value::NIL);
    let display = args.get(3).is_some_and(|v| v.is_truthy());
    let result = builtin_call_process_impl(&mut eval.buffers, args)?;
    maybe_redisplay_sync_output(eval, &destination, display)?;
    Ok(result)
}

pub(crate) fn builtin_call_process_shell_command(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("call-process-shell-command", &args, 1)?;
    let command = super::process::sequence_value_to_env_string(&args[0])?;
    let infile = parse_optional_infile(&args, 1)?;
    let destination = args.get(2).copied().unwrap_or(Value::NIL);
    let display = args.get(3).is_some_and(|v| v.is_truthy());
    let cmd_args = if args.len() > 4 {
        parse_sequence_args(&args[4..])?
    } else {
        Vec::new()
    };
    let shell_command = shell_command_with_args(&command, &cmd_args);
    let shell_args = vec!["-c".to_string(), shell_command];
    let result =
        run_process_command_in_state(&mut eval.buffers, "sh", infile, &destination, &shell_args)?;
    maybe_redisplay_sync_output(eval, &destination, display)?;
    Ok(result)
}

pub(crate) fn builtin_process_file(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("process-file", &args, 1)?;
    let program = super::process::expect_string_strict(&args[0])?;
    let infile = parse_optional_infile(&args, 1)?;
    let destination = args.get(2).copied().unwrap_or(Value::NIL);
    let display = args.get(3).is_some_and(|v| v.is_truthy());
    let cmd_args = if args.len() > 4 {
        super::process::parse_string_args_strict(&args[4..])?
    } else {
        Vec::new()
    };
    let result =
        run_process_command_in_state(&mut eval.buffers, &program, infile, &destination, &cmd_args)?;
    maybe_redisplay_sync_output(eval, &destination, display)?;
    Ok(result)
}

pub(crate) fn builtin_process_file_shell_command(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("process-file-shell-command", &args, 1)?;
    let command = super::process::sequence_value_to_env_string(&args[0])?;
    let infile = parse_optional_infile(&args, 1)?;
    let destination = args.get(2).copied().unwrap_or(Value::NIL);
    let display = args.get(3).is_some_and(|v| v.is_truthy());
    let cmd_args = if args.len() > 4 {
        parse_sequence_args(&args[4..])?
    } else {
        Vec::new()
    };
    let shell_command = shell_command_with_args(&command, &cmd_args);
    let shell_args = vec!["-c".to_string(), shell_command];
    let result =
        run_process_command_in_state(&mut eval.buffers, "sh", infile, &destination, &shell_args)?;
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
    let (status, stdout) = run_process_capture_output(&program, &cmd_args)?;
    if status != 0 {
        return Err(signal_process_lines_status_error(&program, status));
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
    let (_, stdout) = run_process_capture_output(&program, &cmd_args)?;
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
    let (status, stdout) = run_process_capture_output(&program, &cmd_args)?;
    let lines = parse_output_lines(&stdout);

    if !status_handler.is_nil() {
        let _ = eval.apply(status_handler, vec![Value::fixnum(status as i64)])?;
    } else if status != 0 {
        return Err(signal_process_lines_status_error(&program, status));
    }

    Ok(lines)
}

pub(crate) fn builtin_call_process_region(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("call-process-region", &args, 3)?;
    let destination = args.get(4).copied().unwrap_or(Value::NIL);
    let display = args.get(5).is_some_and(|v| v.is_truthy());
    let result = builtin_call_process_region_impl(&mut eval.buffers, args)?;
    maybe_redisplay_sync_output(eval, &destination, display)?;
    Ok(result)
}

fn parse_output_lines(stdout: &[u8]) -> Value {
    let mut text = String::from_utf8_lossy(stdout).into_owned();
    if text.ends_with('\n') {
        text.pop();
    }
    if text.is_empty() {
        Value::NIL
    } else {
        Value::list(text.split('\n').map(Value::string).collect())
    }
}
