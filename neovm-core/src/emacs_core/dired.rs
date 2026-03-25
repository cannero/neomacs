//! Directory and file attribute builtins for the Elisp interpreter.
//!
//! Provides dired-related primitives:
//! - `directory-files-and-attributes`
//! - `file-name-completion`, `file-name-all-completions`
//! - `file-attributes`, `file-attributes-lessp`
//! - `system-users`, `system-groups`

use super::error::{EvalResult, Flow, signal};
use super::eval::Context;
use super::intern::{intern, resolve_sym};
use super::value::*;
use std::collections::{HashMap, VecDeque};
#[cfg(unix)]
use std::ffi::{CStr, CString};
use std::fs;
use std::io::ErrorKind;

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn expect_range_args(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), Flow> {
    if args.len() < min || args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_string(_name: &str, value: &Value) -> Result<String, Flow> {
    match value {
        Value::Str(id) => Ok(with_heap(|h| h.get_string(*id).to_owned())),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
    }
}

/// Ensure a directory path ends with '/'.
fn ensure_trailing_slash(dir: &str) -> String {
    if dir.ends_with('/') {
        dir.to_string()
    } else {
        format!("{}/", dir)
    }
}

fn file_error_symbol(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::NotFound => "file-missing",
        ErrorKind::AlreadyExists => "file-already-exists",
        ErrorKind::PermissionDenied => "permission-denied",
        _ => "file-error",
    }
}

fn signal_file_io(action: &str, path: &str, err: std::io::Error) -> Flow {
    signal(
        file_error_symbol(err.kind()),
        vec![
            Value::string(action),
            Value::string(err.to_string()),
            Value::string(path),
        ],
    )
}

#[cfg(unix)]
fn read_directory_names(dir: &str) -> Result<Vec<String>, Flow> {
    let dir_cstr = CString::new(dir).map_err(|_| {
        signal(
            "file-error",
            vec![
                Value::string("Opening directory"),
                Value::string("path contains interior NUL"),
                Value::string(dir),
            ],
        )
    })?;
    let dirp = unsafe { libc::opendir(dir_cstr.as_ptr()) };
    if dirp.is_null() {
        return Err(signal_file_io(
            "Opening directory",
            dir,
            std::io::Error::last_os_error(),
        ));
    }

    let mut names = Vec::new();
    loop {
        let entry = unsafe { libc::readdir(dirp) };
        if entry.is_null() {
            break;
        }
        let raw_name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        names.push(raw_name.to_string_lossy().into_owned());
    }

    let _ = unsafe { libc::closedir(dirp) };
    Ok(names)
}

#[cfg(not(unix))]
fn read_directory_names(dir: &str) -> Result<Vec<String>, Flow> {
    let entries = fs::read_dir(dir).map_err(|e| signal_file_io("Opening directory", dir, e))?;
    let mut names = vec![".".to_string(), "..".to_string()];
    for entry in entries {
        let entry = entry.map_err(|e| signal_file_io("Reading directory entry", dir, e))?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    Ok(names)
}

fn parse_wholenump_count(arg: Option<&Value>) -> Result<Option<usize>, Flow> {
    match arg {
        Some(Value::Int(n)) if *n >= 0 => Ok(Some(*n as usize)),
        Some(v @ Value::Int(_)) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("wholenump"), *v],
        )),
        Some(v) if v.is_truthy() => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("wholenump"), *v],
        )),
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Time helpers
// ---------------------------------------------------------------------------

/// Convert UNIX seconds + nanoseconds to Emacs (HIGH LOW USEC PSEC) format.
fn time_to_emacs_tuple(secs: i64, nanos: i64) -> Value {
    let mut s = secs;
    let mut ns = nanos;
    if ns < 0 {
        let borrow = ((-ns) + 999_999_999) / 1_000_000_000;
        s -= borrow;
        ns += borrow * 1_000_000_000;
    } else if ns >= 1_000_000_000 {
        s += ns / 1_000_000_000;
        ns %= 1_000_000_000;
    }

    let high = s >> 16;
    let low = s & 0xFFFF;
    let usec = ns / 1_000;
    let psec = (ns % 1_000) * 1_000;

    Value::list(vec![
        Value::Int(high),
        Value::Int(low),
        Value::Int(usec),
        Value::Int(psec),
    ])
}

/// Get UNIX seconds + nanoseconds from SystemTime.
#[cfg(not(unix))]
fn system_time_to_secs_nanos(time: std::time::SystemTime) -> Option<(i64, i64)> {
    let d = time.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some((d.as_secs() as i64, d.subsec_nanos() as i64))
}

#[cfg(unix)]
fn uid_to_name(uid: u32) -> Option<String> {
    unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let mut buf_len = 1024usize;

        loop {
            let mut buf = vec![0u8; buf_len];
            let rc = libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr().cast(), buf_len, &mut result);

            if rc == 0 {
                if result.is_null() || pwd.pw_name.is_null() {
                    return None;
                }
                return Some(CStr::from_ptr(pwd.pw_name).to_string_lossy().into_owned());
            }

            if rc == libc::ERANGE && buf_len < (1 << 20) {
                buf_len *= 2;
                continue;
            }

            return None;
        }
    }
}

#[cfg(unix)]
fn gid_to_name(gid: u32) -> Option<String> {
    unsafe {
        let mut grp: libc::group = std::mem::zeroed();
        let mut result: *mut libc::group = std::ptr::null_mut();
        let mut buf_len = 1024usize;

        loop {
            let mut buf = vec![0u8; buf_len];
            let rc = libc::getgrgid_r(gid, &mut grp, buf.as_mut_ptr().cast(), buf_len, &mut result);

            if rc == 0 {
                if result.is_null() || grp.gr_name.is_null() {
                    return None;
                }
                return Some(CStr::from_ptr(grp.gr_name).to_string_lossy().into_owned());
            }

            if rc == libc::ERANGE && buf_len < (1 << 20) {
                buf_len *= 2;
                continue;
            }

            return None;
        }
    }
}

// ---------------------------------------------------------------------------
// file-attributes core
// ---------------------------------------------------------------------------

/// Build the Emacs-compatible file-attributes list for a path.
///
/// Returns:
///   (TYPE NLINKS UID GID ATIME MTIME CTIME SIZE MODE GID-CHANGEP INODE DEVICE)
///
/// TYPE is:
///   t        for a directory
///   nil      for a regular file
///   string   for a symlink (the link target)
///
/// Times are in Emacs (HIGH LOW) format.
/// If ID-FORMAT is 'string, UID/GID are returned as strings; otherwise integers.
fn build_file_attributes(filename: &str, id_format_string: bool) -> Option<Value> {
    // Use symlink_metadata first to detect symlinks.
    let sym_meta = fs::symlink_metadata(filename).ok()?;

    // Determine file type.
    let file_type = if sym_meta.file_type().is_symlink() {
        // Read the symlink target.
        match fs::read_link(filename) {
            Ok(target) => Value::string(target.to_string_lossy().into_owned()),
            Err(_) => Value::string(""),
        }
    } else if sym_meta.is_dir() {
        Value::True
    } else {
        Value::Nil
    };

    // For symlinks, get the target metadata for size etc; fall back to symlink meta.
    let meta = if sym_meta.file_type().is_symlink() {
        fs::metadata(filename).unwrap_or_else(|_| sym_meta.clone())
    } else {
        sym_meta.clone()
    };

    // Number of hard links.
    #[cfg(unix)]
    let nlinks = {
        use std::os::unix::fs::MetadataExt;
        Value::Int(sym_meta.nlink() as i64)
    };
    #[cfg(not(unix))]
    let nlinks = Value::Int(1);

    // UID / GID.
    #[cfg(unix)]
    let (uid_val, gid_val) = {
        use std::os::unix::fs::MetadataExt;
        let uid = sym_meta.uid();
        let gid = sym_meta.gid();
        if id_format_string {
            (
                Value::string(uid_to_name(uid).unwrap_or_else(|| uid.to_string())),
                Value::string(gid_to_name(gid).unwrap_or_else(|| gid.to_string())),
            )
        } else {
            (Value::Int(uid as i64), Value::Int(gid as i64))
        }
    };
    #[cfg(not(unix))]
    let (uid_val, gid_val) = if id_format_string {
        (Value::string("0"), Value::string("0"))
    } else {
        (Value::Int(0), Value::Int(0))
    };

    // Access time.
    #[cfg(unix)]
    let atime = {
        use std::os::unix::fs::MetadataExt;
        time_to_emacs_tuple(sym_meta.atime(), sym_meta.atime_nsec())
    };
    #[cfg(not(unix))]
    let atime = meta
        .accessed()
        .ok()
        .and_then(system_time_to_secs_nanos)
        .map(|(secs, nanos)| time_to_emacs_tuple(secs, nanos))
        .unwrap_or(Value::Nil);

    // Modification time.
    #[cfg(unix)]
    let mtime = {
        use std::os::unix::fs::MetadataExt;
        time_to_emacs_tuple(meta.mtime(), meta.mtime_nsec())
    };
    #[cfg(not(unix))]
    let mtime = meta
        .modified()
        .ok()
        .and_then(system_time_to_secs_nanos)
        .map(|(secs, nanos)| time_to_emacs_tuple(secs, nanos))
        .unwrap_or(Value::Nil);

    // Status change time (ctime on Unix, creation time on other platforms).
    #[cfg(unix)]
    let ctime = {
        use std::os::unix::fs::MetadataExt;
        time_to_emacs_tuple(sym_meta.ctime(), sym_meta.ctime_nsec())
    };
    #[cfg(not(unix))]
    let ctime = meta
        .created()
        .ok()
        .and_then(system_time_to_secs_nanos)
        .map(|(secs, nanos)| time_to_emacs_tuple(secs, nanos))
        .unwrap_or(Value::Nil);

    // Size.
    let size = Value::Int(meta.len() as i64);

    // Mode string (like "drwxr-xr-x").
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        let mode_bits = sym_meta.permissions().mode();
        Value::string(format_mode_string(mode_bits, &sym_meta))
    };
    #[cfg(not(unix))]
    let mode = Value::string(if meta.is_dir() {
        "drwxr-xr-x"
    } else {
        "-rw-r--r--"
    });

    // GID-CHANGEP: Emacs commonly reports t on Unix filesystems.
    #[cfg(unix)]
    let gid_changep = Value::True;
    #[cfg(not(unix))]
    let gid_changep = Value::Nil;

    // Inode.
    #[cfg(unix)]
    let inode = {
        use std::os::unix::fs::MetadataExt;
        Value::Int(sym_meta.ino() as i64)
    };
    #[cfg(not(unix))]
    let inode = Value::Int(0);

    // Device.
    #[cfg(unix)]
    let device = {
        use std::os::unix::fs::MetadataExt;
        Value::Int(sym_meta.dev() as i64)
    };
    #[cfg(not(unix))]
    let device = Value::Int(0);

    Some(Value::list(vec![
        file_type,
        nlinks,
        uid_val,
        gid_val,
        atime,
        mtime,
        ctime,
        size,
        mode,
        gid_changep,
        inode,
        device,
    ]))
}

/// Format a Unix file mode string like "drwxr-xr-x" or "-rw-r--r--".
#[cfg(unix)]
fn format_mode_string(mode: u32, meta: &fs::Metadata) -> String {
    let mut s = String::with_capacity(10);

    // File type character.
    if meta.file_type().is_symlink() {
        s.push('l');
    } else if meta.is_dir() {
        s.push('d');
    } else {
        s.push('-');
    }

    // Owner permissions.
    s.push(if mode & 0o400 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o200 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o4000 != 0 {
        if mode & 0o100 != 0 { 's' } else { 'S' }
    } else if mode & 0o100 != 0 {
        'x'
    } else {
        '-'
    });

    // Group permissions.
    s.push(if mode & 0o040 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o020 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o2000 != 0 {
        if mode & 0o010 != 0 { 's' } else { 'S' }
    } else if mode & 0o010 != 0 {
        'x'
    } else {
        '-'
    });

    // Other permissions.
    s.push(if mode & 0o004 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o002 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o1000 != 0 {
        if mode & 0o001 != 0 { 't' } else { 'T' }
    } else if mode & 0o001 != 0 {
        'x'
    } else {
        '-'
    });

    s
}

// ---------------------------------------------------------------------------
// Pure builtins
// ---------------------------------------------------------------------------

/// (directory-files-and-attributes DIRECTORY &optional FULL-NAME MATCH-REGEXP NOSORT ID-FORMAT COUNT)
///
/// Like `directory-files` but each element is (NAME . ATTRIBUTES) where
/// ATTRIBUTES is the result of `file-attributes`.
pub(crate) fn builtin_directory_files_and_attributes(args: Vec<Value>) -> EvalResult {
    expect_range_args("directory-files-and-attributes", &args, 1, 6)?;
    let dir = expect_string("directory-files-and-attributes", &args[0])?;
    directory_files_and_attributes_with_dir(&args, dir)
}

pub(crate) fn builtin_directory_files_and_attributes_in_state(
    obarray: &super::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("directory-files-and-attributes", &args, 1, 6)?;
    let dir = super::fileio::resolve_filename_in_state(
        obarray,
        dynamic,
        buffers,
        &expect_string("directory-files-and-attributes", &args[0])?,
    );
    directory_files_and_attributes_with_dir(&args, dir)
}

/// Context-backed variant of `directory-files-and-attributes`.
/// Resolves relative DIRECTORY against dynamic/default `default-directory`.
pub(crate) fn builtin_directory_files_and_attributes_eval(
    eval: &Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_directory_files_and_attributes_in_state(
        &eval.obarray,
        eval.dynamic.as_slice(),
        &eval.buffers,
        args,
    )
}

fn directory_files_and_attributes_with_dir(args: &[Value], dir: String) -> EvalResult {
    let full_name = args.get(1).is_some_and(|v| v.is_truthy());
    let match_regexp = match args.get(2) {
        Some(v) if v.is_truthy() => Some(expect_string("directory-files-and-attributes", v)?),
        _ => None,
    };
    let nosort = args.get(3).is_some_and(|v| v.is_truthy());
    let id_format_string = args.get(4).is_some_and(|v| v.is_truthy());
    let count = parse_wholenump_count(args.get(5))?;
    if count == Some(0) {
        return Ok(Value::Nil);
    }

    let names = read_directory_names(&dir)?;

    let dir_with_slash = ensure_trailing_slash(&dir);
    let mut items: VecDeque<(String, String)> = VecDeque::new();
    let mut remaining = count.unwrap_or(usize::MAX);
    for name in names {
        if let Some(pattern) = match_regexp.as_deref() {
            let mut throwaway = None;
            let matched = super::regex::string_match_full_with_case_fold(
                pattern,
                &name,
                0,
                false,
                &mut throwaway,
            )
            .map_err(|msg| {
                signal(
                    "invalid-regexp",
                    vec![Value::string(format!(
                        "Invalid regexp \"{}\": {}",
                        pattern, msg
                    ))],
                )
            })?;
            if matched.is_none() {
                continue;
            }
        }

        let full_path = format!("{}{}", dir_with_slash, name);
        let display_name = if full_name { full_path.clone() } else { name };
        items.push_front((display_name, full_path));

        if remaining != usize::MAX {
            remaining -= 1;
            if remaining == 0 {
                break;
            }
        }
    }

    let mut items: Vec<(String, String)> = items.into_iter().collect();
    // Sort unless NOSORT is non-nil.
    if !nosort {
        items.sort_by(|a, b| a.0.cmp(&b.0));
    }

    // Build result list of (NAME . ATTRIBUTES) cons cells.
    let result: Vec<Value> = items
        .into_iter()
        .map(|(display_name, full_path)| {
            let attrs = build_file_attributes(&full_path, id_format_string).unwrap_or(Value::Nil);
            Value::cons(Value::string(display_name), attrs)
        })
        .collect();

    Ok(Value::list(result))
}

/// (file-name-completion FILE DIRECTORY &optional PREDICATE)
///
/// Complete file name FILE in DIRECTORY.
/// Returns the longest common completion prefix, or t if FILE is an exact
/// and unique match, or nil if no completions exist.
/// This pure-dispatch variant supports symbol predicates.
pub(crate) fn builtin_file_name_completion(args: Vec<Value>) -> EvalResult {
    expect_range_args("file-name-completion", &args, 2, 3)?;

    let file = expect_string("file-name-completion", &args[0])?;
    let directory = expect_string("file-name-completion", &args[1])?;
    let predicate = args.get(2);
    if file.contains('/') {
        return Ok(Value::Nil);
    }
    let completions = collect_file_name_completions(&file, &directory)?;
    let completions = filter_completions_by_symbol_predicate(predicate, &directory, completions)?;
    Ok(resolve_file_name_completion(&file, completions))
}

pub(crate) fn builtin_file_name_completion_in_state(
    obarray: &super::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    let plan = prepare_file_name_completion_in_state(obarray, dynamic, buffers, &args)?;
    let predicate = args.get(2);
    let completions =
        filter_completions_by_symbol_predicate(predicate, &plan.directory, plan.completions)?;
    Ok(resolve_file_name_completion(&plan.file, completions))
}

/// Context-backed variant of `file-name-completion`.
/// This supports arbitrary callable predicates and matches Emacs behavior of
/// binding `default-directory` to DIRECTORY while predicate is invoked.
pub(crate) fn builtin_file_name_completion_eval(
    eval: &mut Context,
    args: Vec<Value>,
) -> EvalResult {
    let plan = prepare_file_name_completion_in_state(
        &eval.obarray,
        eval.dynamic.as_slice(),
        &eval.buffers,
        &args,
    )?;
    let predicate = args.get(2);
    finish_file_name_completion_with_eval_predicate(
        eval,
        predicate,
        plan.directory,
        plan.file,
        plan.completions,
    )
}

/// (file-name-all-completions FILE DIRECTORY)
///
/// Return a list of all completions of FILE in DIRECTORY.
/// Each entry that is a directory has a trailing '/'.
pub(crate) fn builtin_file_name_all_completions(args: Vec<Value>) -> EvalResult {
    expect_range_args("file-name-all-completions", &args, 2, 2)?;

    let file = expect_string("file-name-all-completions", &args[0])?;
    let directory = expect_string("file-name-all-completions", &args[1])?;
    if file.contains('/') {
        return Ok(Value::Nil);
    }
    let completions = collect_file_name_completions(&file, &directory)?;
    Ok(Value::list(
        completions.into_iter().map(Value::string).collect(),
    ))
}

pub(crate) fn builtin_file_name_all_completions_in_state(
    obarray: &super::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("file-name-all-completions", &args, 2, 2)?;

    let file = expect_string("file-name-all-completions", &args[0])?;
    let directory = super::fileio::resolve_filename_in_state(
        obarray,
        dynamic,
        buffers,
        &expect_string("file-name-all-completions", &args[1])?,
    );
    if file.contains('/') {
        return Ok(Value::Nil);
    }
    let completions = collect_file_name_completions(&file, &directory)?;
    Ok(Value::list(
        completions.into_iter().map(Value::string).collect(),
    ))
}

/// Context-backed variant of `file-name-all-completions`.
/// Resolves relative DIRECTORY against dynamic/default `default-directory`.
pub(crate) fn builtin_file_name_all_completions_eval(
    eval: &Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_file_name_all_completions_in_state(
        &eval.obarray,
        eval.dynamic.as_slice(),
        &eval.buffers,
        args,
    )
}

fn collect_file_name_completions(file: &str, directory: &str) -> Result<Vec<String>, Flow> {
    let names = read_directory_names(directory)?;
    let mut completions = VecDeque::new();

    for name in names {
        if !name.starts_with(file) {
            continue;
        }

        let full_path = std::path::Path::new(directory).join(&name);
        if full_path.is_dir() {
            completions.push_front(format!("{}/", name));
        } else {
            completions.push_front(name);
        }
    }

    Ok(completions.into_iter().collect())
}

pub(crate) struct FileNameCompletionPlan {
    pub(crate) file: String,
    pub(crate) directory: String,
    pub(crate) completions: Vec<String>,
}

pub(crate) fn prepare_file_name_completion_in_state(
    obarray: &super::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    args: &[Value],
) -> Result<FileNameCompletionPlan, Flow> {
    expect_range_args("file-name-completion", args, 2, 3)?;

    let file = expect_string("file-name-completion", &args[0])?;
    let directory = super::fileio::resolve_filename_in_state(
        obarray,
        dynamic,
        buffers,
        &expect_string("file-name-completion", &args[1])?,
    );
    let completions = if file.contains('/') {
        Vec::new()
    } else {
        collect_file_name_completions(&file, &directory)?
    };

    Ok(FileNameCompletionPlan {
        file,
        directory,
        completions,
    })
}

pub(crate) fn finish_file_name_completion_with_eval_predicate(
    eval: &mut Context,
    predicate: Option<&Value>,
    directory: String,
    file: String,
    completions: Vec<String>,
) -> EvalResult {
    let Some(predicate) = predicate.copied() else {
        return Ok(resolve_file_name_completion(&file, completions));
    };
    if predicate.is_nil() {
        return Ok(resolve_file_name_completion(&file, completions));
    }

    let use_absolute_path = predicate_uses_absolute_file_argument(&eval.obarray, &predicate);
    let bound_directory = directory.clone();
    finish_file_name_completion_with_callable_predicate(
        use_absolute_path,
        directory,
        file,
        completions,
        |predicate_arg| {
            with_default_directory_binding(eval, bound_directory.as_str(), |eval| {
                eval.apply(predicate, vec![predicate_arg])
            })
        },
    )
}

pub(crate) fn predicate_uses_absolute_file_argument(
    obarray: &super::symbol::Obarray,
    predicate: &Value,
) -> bool {
    let Some(symbol) = predicate_callable_name(predicate) else {
        return false;
    };
    obarray.symbol_function(symbol).is_none() && is_builtin_path_predicate(symbol)
}

pub(crate) fn finish_file_name_completion_with_callable_predicate(
    use_absolute_path: bool,
    directory: String,
    file: String,
    completions: Vec<String>,
    mut predicate_call: impl FnMut(Value) -> Result<Value, Flow>,
) -> EvalResult {
    let completions = filter_completions_by_callable_predicate(
        use_absolute_path,
        directory.as_str(),
        completions,
        |predicate_arg| predicate_call(predicate_arg),
    )?;
    Ok(resolve_file_name_completion(&file, completions))
}

fn resolve_file_name_completion(file: &str, completions: Vec<String>) -> Value {
    if completions.is_empty() {
        return Value::Nil;
    }

    let filtered = filter_completion_candidates(file, completions);
    if filtered.is_empty() {
        return Value::string(file);
    }

    // If there is exactly one completion and it matches FILE exactly, return t.
    // For directory candidates ending in '/', Emacs returns the completion
    // string when FILE lacks the trailing slash (e.g. ".." -> "../").
    if filtered.len() == 1 {
        let comp = &filtered[0];
        if comp == file {
            return Value::True;
        }
        return Value::string(comp.clone());
    }

    // Find the longest common prefix among completions.
    let mut prefix = filtered[0].clone();
    for comp in &filtered[1..] {
        let common_len = prefix
            .chars()
            .zip(comp.chars())
            .take_while(|(a, b)| a == b)
            .count();
        prefix.truncate(
            prefix
                .char_indices()
                .nth(common_len)
                .map(|(i, _)| i)
                .unwrap_or(prefix.len()),
        );
    }

    // If the prefix equals the input exactly and there are multiple matches,
    // return the prefix (Emacs returns what was typed if ambiguous but valid prefix).
    Value::string(prefix)
}

fn filter_completion_candidates(file: &str, completions: Vec<String>) -> Vec<String> {
    completions
        .into_iter()
        .filter(|c| c != "./")
        .filter(|c| file.starts_with("..") || c != "../")
        .collect()
}

fn filter_completions_by_symbol_predicate(
    predicate: Option<&Value>,
    directory: &str,
    completions: Vec<String>,
) -> Result<Vec<String>, Flow> {
    let Some(predicate) = predicate else {
        return Ok(completions);
    };
    if predicate.is_nil() {
        return Ok(completions);
    }
    let Some(symbol) = predicate_callable_name(predicate) else {
        // Pure dispatch cannot evaluate lambda/object predicates.
        return Ok(completions);
    };

    let mut filtered = Vec::new();
    for candidate in completions {
        if symbol_predicate_matches_candidate(symbol, directory, &candidate)? {
            filtered.push(candidate);
        }
    }
    Ok(filtered)
}

fn symbol_predicate_matches_candidate(
    symbol: &str,
    directory: &str,
    candidate: &str,
) -> Result<bool, Flow> {
    if let Some(result) =
        super::builtins::dispatch_builtin_pure(symbol, vec![Value::string(candidate)])
    {
        let result = result?;
        if result.is_truthy() || !is_builtin_path_predicate(symbol) {
            return Ok(result.is_truthy());
        }

        let absolute = std::path::Path::new(directory).join(candidate);
        let absolute = absolute.to_string_lossy().into_owned();
        if let Some(result) =
            super::builtins::dispatch_builtin_pure(symbol, vec![Value::string(absolute)])
        {
            return Ok(result?.is_truthy());
        }
        return Ok(false);
    }

    // Fallback for pure mode: try absolute path to make path predicates useful
    // without evaluator-backed dynamic binding.
    let absolute = std::path::Path::new(directory).join(candidate);
    let absolute = absolute.to_string_lossy().into_owned();
    if let Some(result) =
        super::builtins::dispatch_builtin_pure(symbol, vec![Value::string(absolute)])
    {
        return Ok(result?.is_truthy());
    }

    // Preserve current behavior for unknown/non-callable predicates in pure mode.
    Ok(true)
}

fn filter_completions_by_callable_predicate(
    use_absolute_path: bool,
    directory: &str,
    completions: Vec<String>,
    mut predicate_call: impl FnMut(Value) -> Result<Value, Flow>,
) -> Result<Vec<String>, Flow> {
    let mut filtered = Vec::new();
    for candidate in completions {
        let predicate_arg =
            predicate_argument_for_callable_predicate(use_absolute_path, directory, &candidate);
        let keep = predicate_call(predicate_arg)?.is_truthy();
        if keep {
            filtered.push(candidate);
        }
    }
    Ok(filtered)
}

fn with_default_directory_binding<T>(
    eval: &mut Context,
    directory: &str,
    f: impl FnOnce(&mut Context) -> Result<T, Flow>,
) -> Result<T, Flow> {
    let mut frame = OrderedRuntimeBindingMap::new();
    frame.insert(intern("default-directory"), Value::string(directory));
    eval.dynamic.push(frame);
    let result = f(eval);
    eval.dynamic.pop();
    result
}

fn predicate_argument_for_callable_predicate(
    use_absolute_path: bool,
    directory: &str,
    candidate: &str,
) -> Value {
    if use_absolute_path {
        let absolute = std::path::Path::new(directory).join(candidate);
        return Value::string(absolute.to_string_lossy().into_owned());
    }

    Value::string(candidate)
}

fn is_builtin_path_predicate(name: &str) -> bool {
    matches!(
        name,
        "file-directory-p"
            | "file-exists-p"
            | "file-readable-p"
            | "file-writable-p"
            | "file-regular-p"
            | "file-symlink-p"
            | "file-executable-p"
    )
}

fn predicate_callable_name(predicate: &Value) -> Option<&str> {
    match predicate {
        Value::Symbol(id) | Value::Subr(id) => Some(resolve_sym(*id)),
        _ => None,
    }
}

/// (file-attributes FILENAME &optional ID-FORMAT)
///
/// Return a list of attributes of file FILENAME.
/// The list elements are:
///   0. TYPE (t=dir, nil=regular, string=symlink target)
///   1. Number of hard links
///   2. UID (integer or string if ID-FORMAT is 'string)
///   3. GID (integer or string if ID-FORMAT is 'string)
///   4. Last access time (HIGH LOW)
///   5. Last modification time (HIGH LOW)
///   6. Status change time (HIGH LOW)
///   7. Size in bytes
///   8. File modes as string (like "drwxr-xr-x")
///   9. GID-CHANGEP (always nil)
///  10. Inode number
///  11. Device number
pub(crate) fn builtin_file_attributes(args: Vec<Value>) -> EvalResult {
    expect_range_args("file-attributes", &args, 1, 2)?;

    let filename = expect_string("file-attributes", &args[0])?;
    let id_format_string = args.get(1).is_some_and(|v| v.is_truthy());

    match build_file_attributes(&filename, id_format_string) {
        Some(attrs) => Ok(attrs),
        None => Ok(Value::Nil),
    }
}

pub(crate) fn builtin_file_attributes_in_state(
    obarray: &super::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("file-attributes", &args, 1, 2)?;

    let filename = super::fileio::resolve_filename_in_state(
        obarray,
        dynamic,
        buffers,
        &expect_string("file-attributes", &args[0])?,
    );
    let id_format_string = args.get(1).is_some_and(|v| v.is_truthy());

    match build_file_attributes(&filename, id_format_string) {
        Some(attrs) => Ok(attrs),
        None => Ok(Value::Nil),
    }
}

/// Context-backed variant of `file-attributes`.
/// Resolves relative FILENAME against dynamic/default `default-directory`.
pub(crate) fn builtin_file_attributes_eval(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    builtin_file_attributes_in_state(&eval.obarray, eval.dynamic.as_slice(), &eval.buffers, args)
}

/// (file-attributes-lessp F1 F2)
///
/// Return t if the first element (filename) of F1 is less than that of F2.
/// F1 and F2 are each (NAME . ATTRIBUTES) cons cells as returned by
/// `directory-files-and-attributes`.
pub(crate) fn builtin_file_attributes_lessp(args: Vec<Value>) -> EvalResult {
    expect_range_args("file-attributes-lessp", &args, 2, 2)?;

    let name1 = extract_car_string("file-attributes-lessp", &args[0])?;
    let name2 = extract_car_string("file-attributes-lessp", &args[1])?;

    Ok(Value::bool(name1 < name2))
}

/// Extract the car of a cons cell as a string.
fn extract_car_string(_name: &str, val: &Value) -> Result<String, Flow> {
    match val {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            match &pair.car {
                Value::Str(id) => Ok(with_heap(|h| h.get_string(*id).to_owned())),
                other => Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), *other],
                )),
            }
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("consp"), *other],
        )),
    }
}

/// (system-users)
///
/// Return a list of user names on the system.
/// Reads `/etc/passwd` and returns account names in oracle-compatible order.
pub(crate) fn builtin_system_users(args: Vec<Value>) -> EvalResult {
    expect_range_args("system-users", &args, 0, 0)?;

    let mut users = read_colon_file_names(&system_users_passwd_path());
    if users.is_empty() {
        let fallback_user = std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .unwrap_or_else(|_| "unknown".to_string());
        users.push(fallback_user);
    }

    Ok(Value::list(
        users.into_iter().map(Value::string).collect::<Vec<_>>(),
    ))
}

/// (system-groups)
///
/// Return a list of group names on the system.
/// Reads `/etc/group` and returns group names in oracle-compatible order.
pub(crate) fn builtin_system_groups(args: Vec<Value>) -> EvalResult {
    expect_range_args("system-groups", &args, 0, 0)?;
    let groups = read_colon_file_names(&system_groups_path());
    if groups.is_empty() {
        return Ok(Value::Nil);
    }
    Ok(Value::list(
        groups.into_iter().map(Value::string).collect::<Vec<_>>(),
    ))
}

fn parse_colon_file_names(contents: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((name, _rest)) = trimmed.split_once(':') {
            let name = name.trim();
            if !name.is_empty() {
                names.push(name.to_string());
            }
        }
    }
    // Emacs' output order matches reverse file order.
    names.reverse();
    names
}

fn system_users_passwd_path() -> String {
    "/etc/passwd".to_string()
}

fn system_groups_path() -> String {
    "/etc/group".to_string()
}

fn read_colon_file_names(path: &str) -> Vec<String> {
    match fs::read_to_string(path) {
        Ok(contents) => parse_colon_file_names(&contents),
        Err(_) => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "dired_test.rs"]
mod tests;
