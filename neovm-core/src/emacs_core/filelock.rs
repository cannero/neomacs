//! File locking primitives.
//!
//! GNU Emacs owns these in `filelock.c`, and `buffer.c` drives them from
//! `restore-buffer-modified-p` when a file-visiting buffer changes between
//! modified and unmodified states.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::error::{EvalResult, Flow, signal};
use super::fileio::resolve_filename_for_eval;
use super::value::{Value, with_heap, ValueKind};
use crate::buffer::BufferId;

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

fn expect_range_args(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), Flow> {
    if args.len() < min || args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_string_arg(value: &Value) -> Result<String, Flow> {
    match value.kind() {
        ValueKind::String => Ok(with_heap(|heap| heap.get_string(*id).to_owned())),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
    }
}

fn file_lock_error(context: &str, filename: &str, err: io::Error) -> Flow {
    signal(
        "file-error",
        vec![
            Value::string(context),
            Value::string(filename),
            Value::string(err.to_string()),
        ],
    )
}

fn current_user_name() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

fn current_host_name() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        unsafe {
            if libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) == 0 {
                let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
                if let Ok(host) = std::str::from_utf8(&buf[..end])
                    && !host.is_empty()
                {
                    return host.to_string();
                }
            }
        }
    }

    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown-host".to_string())
}

fn current_lock_info_string() -> String {
    format!(
        "{}@{}.{}",
        current_user_name(),
        current_host_name(),
        std::process::id()
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedLockInfo {
    user: String,
    host: String,
    pid: u32,
}

fn parse_lock_info(contents: &str) -> Option<ParsedLockInfo> {
    let trimmed = contents.trim();
    let (user, rest) = trimmed.split_once('@')?;
    let (host, pid_and_boot) = rest.rsplit_once('.')?;
    let pid_str = pid_and_boot.split(':').next()?;
    let pid = pid_str.parse().ok()?;
    Some(ParsedLockInfo {
        user: user.to_string(),
        host: host.to_string(),
        pid,
    })
}

enum LockOwner {
    None,
    Current,
    Other(String),
}

fn fallback_make_lock_file_name(filename: &str) -> Option<String> {
    let path = Path::new(filename);
    let name = path.file_name()?.to_string_lossy();
    let mut out = PathBuf::new();
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        out.push(parent);
    }
    out.push(format!(".#{name}"));
    Some(out.to_string_lossy().into_owned())
}

fn make_lock_file_name(
    eval: &mut super::eval::Context,
    filename: &str,
) -> Result<Option<String>, Flow> {
    let file = Value::string(filename);
    match eval.apply(Value::symbol("make-lock-file-name"), vec![file]) {
        Ok(ValueKind::Nil) => Ok(None),
        Ok(ValueKind::String) => Ok(Some(with_heap(|heap| heap.get_string(id).to_owned()))),
        Ok(other) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), other],
        )),
        Err(_) => Ok(fallback_make_lock_file_name(filename)),
    }
}

fn read_lock_contents(lock_path: &Path) -> io::Result<String> {
    match fs::read_link(lock_path) {
        Ok(target) => Ok(target.to_string_lossy().into_owned()),
        Err(link_err) => match fs::read_to_string(lock_path) {
            Ok(contents) => Ok(contents),
            Err(_) => Err(link_err),
        },
    }
}

fn current_lock_owner(lock_path: &Path) -> Result<LockOwner, io::Error> {
    if !lock_path.exists() {
        return Ok(LockOwner::None);
    }

    let contents = read_lock_contents(lock_path)?;
    let Some(info) = parse_lock_info(&contents) else {
        let owner = contents
            .split_once('@')
            .map(|(user, _)| user.to_string())
            .unwrap_or(contents);
        return Ok(LockOwner::Other(owner));
    };

    let ours = info.user == current_user_name()
        && info.host == current_host_name()
        && info.pid == std::process::id();
    if ours {
        Ok(LockOwner::Current)
    } else {
        Ok(LockOwner::Other(info.user))
    }
}

fn create_lock_file(lock_path: &Path, contents: &str, force: bool) -> io::Result<()> {
    if force {
        match fs::remove_file(lock_path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
    }

    #[cfg(unix)]
    {
        match std::os::unix::fs::symlink(contents, lock_path) {
            Ok(()) => return Ok(()),
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::AlreadyExists
                        | io::ErrorKind::Unsupported
                        | io::ErrorKind::PermissionDenied
                ) => {}
            Err(err) => return Err(err),
        }
    }

    fs::write(lock_path, contents)
}

fn lock_file_resolved(eval: &mut super::eval::Context, filename: &str) -> Result<Value, Flow> {
    if !eval
        .visible_variable_value_or_nil("create-lockfiles")
        .is_truthy()
    {
        return Ok(Value::NIL);
    }

    let Some(lock_name) = make_lock_file_name(eval, filename)? else {
        return Ok(Value::NIL);
    };
    let lock_path = PathBuf::from(lock_name);

    match current_lock_owner(&lock_path)
        .map_err(|err| file_lock_error("Testing file lock", filename, err))?
    {
        LockOwner::None | LockOwner::Current => {}
        LockOwner::Other(owner) => {
            let attack = eval
                .apply(
                    Value::symbol("ask-user-about-lock"),
                    vec![Value::string(filename), Value::string(owner)],
                )
                .unwrap_or(Value::NIL);
            if !attack.is_truthy() {
                return Ok(Value::NIL);
            }
            create_lock_file(&lock_path, &current_lock_info_string(), true)
                .map_err(|err| file_lock_error("Locking file", filename, err))?;
            return Ok(Value::NIL);
        }
    }

    create_lock_file(&lock_path, &current_lock_info_string(), false).map_err(|err| {
        if err.kind() == io::ErrorKind::AlreadyExists {
            file_lock_error("Locking file", filename, err)
        } else {
            file_lock_error("Locking file", filename, err)
        }
    })?;
    Ok(Value::NIL)
}

fn unlock_file_resolved(eval: &mut super::eval::Context, filename: &str) -> Result<Value, Flow> {
    let Some(lock_name) = make_lock_file_name(eval, filename)? else {
        return Ok(Value::NIL);
    };
    let lock_path = PathBuf::from(lock_name);

    match current_lock_owner(&lock_path)
        .map_err(|err| file_lock_error("Unlocking file", filename, err))?
    {
        LockOwner::None | LockOwner::Other(_) => Ok(Value::NIL),
        LockOwner::Current => match fs::remove_file(&lock_path) {
            Ok(()) => Ok(Value::NIL),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Value::NIL),
            Err(err) => Err(file_lock_error("Unlocking file", filename, err)),
        },
    }
}

fn current_buffer_file_lock_target(
    eval: &super::eval::Context,
    buffer_id: BufferId,
) -> Option<String> {
    let root_id = eval.buffers.modified_state_root_id(buffer_id)?;
    let buffer = eval.buffers.get(root_id)?;
    let file_name = buffer.buffer_local_value("buffer-file-name")?;
    let file_truename = buffer.buffer_local_value("buffer-file-truename")?;
    match (file_name.kind(), file_truename.kind()) {
        (ValueKind::String, ValueKind::String) => Some(with_heap(|heap| heap.get_string(id).to_owned())),
        _ => None,
    }
}

pub(crate) fn sync_modified_buffer_file_lock(
    eval: &mut super::eval::Context,
    buffer_id: BufferId,
    was_modified: bool,
    flag: Value,
) -> Result<(), Flow> {
    let Some(filename) = current_buffer_file_lock_target(eval, buffer_id) else {
        return Ok(());
    };

    let filename = resolve_filename_for_eval(eval, &filename);
    if !was_modified && !flag.is_nil() {
        let _ = lock_file_resolved(eval, &filename)?;
    } else if was_modified && flag.is_nil() {
        let _ = unlock_file_resolved(eval, &filename)?;
    }
    Ok(())
}

pub(crate) fn builtin_lock_file(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("lock-file", &args, 1)?;
    let filename = expect_string_arg(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    lock_file_resolved(eval, &filename)
}

pub(crate) fn builtin_unlock_file(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("unlock-file", &args, 1)?;
    let filename = expect_string_arg(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    unlock_file_resolved(eval, &filename)
}

pub(crate) fn builtin_file_locked_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("file-locked-p", &args, 1)?;
    let filename = expect_string_arg(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    let Some(lock_name) = make_lock_file_name(eval, &filename)? else {
        return Ok(Value::NIL);
    };
    let lock_path = PathBuf::from(lock_name);

    match current_lock_owner(&lock_path)
        .map_err(|err| file_lock_error("Testing file lock", &filename, err))?
    {
        LockOwner::None => Ok(Value::NIL),
        LockOwner::Current => Ok(Value::T),
        LockOwner::Other(user) => Ok(Value::string(user)),
    }
}

pub(crate) fn builtin_lock_buffer(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_range_args("lock-buffer", &args, 0, 1)?;
    let filename = if let Some(filename) = args.first() {
        if filename.is_nil() {
            None
        } else {
            Some(resolve_filename_for_eval(
                eval,
                &expect_string_arg(filename)?,
            ))
        }
    } else {
        let current = eval
            .buffers
            .current_buffer()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        current
            .buffer_local_value("buffer-file-truename")
            .and_then(|value| match value {
                Value::Str(id) /* TODO(tagged): convert Value::Str to new API */ => Some(with_heap(|heap| heap.get_string(id).to_owned())),
                _ => None,
            })
            .map(|filename| resolve_filename_for_eval(eval, &filename))
    };

    let modified = eval
        .buffers
        .current_buffer()
        .is_some_and(|buffer| buffer.modified_state_value().is_truthy());
    if modified && let Some(filename) = filename {
        let _ = lock_file_resolved(eval, &filename)?;
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_unlock_buffer(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("unlock-buffer", &args, 0)?;
    let Some(current) = eval.buffers.current_buffer() else {
        return Ok(Value::NIL);
    };
    if current.modified_state_value().is_truthy()
        && let Some(Value::Str(id) /* TODO(tagged): convert Value::Str to new API */) = current.buffer_local_value("buffer-file-truename")
    {
        let filename = with_heap(|heap| heap.get_string(id).to_owned());
        let filename = resolve_filename_for_eval(eval, &filename);
        let _ = unlock_file_resolved(eval, &filename)?;
    }
    Ok(Value::NIL)
}
