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
use crate::heap_types::LispString;
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
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_lisp_string(_name: &str, value: &Value) -> Result<LispString, Flow> {
    match value.kind() {
        ValueKind::String => Ok(value
            .as_lisp_string()
            .expect("ValueKind::String must carry LispString payload")
            .clone()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *value],
        )),
    }
}

fn dired_runtime_string(value: &LispString) -> String {
    super::builtins::runtime_string_from_lisp_string(value)
}

fn runtime_file_name_to_lisp_string(text: &str) -> LispString {
    super::builtins::runtime_string_to_lisp_string(text, !text.is_ascii())
}

fn runtime_file_name_value(text: &str) -> Value {
    Value::heap_string(runtime_file_name_to_lisp_string(text))
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
        Some(v) if v.is_fixnum() && v.as_fixnum().unwrap() >= 0 => {
            Ok(Some(v.as_fixnum().unwrap() as usize))
        }
        Some(v) if v.is_fixnum() => Err(signal(
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
        Value::fixnum(high),
        Value::fixnum(low),
        Value::fixnum(usec),
        Value::fixnum(psec),
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
        Value::T
    } else {
        Value::NIL
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
        Value::fixnum(sym_meta.nlink() as i64)
    };
    #[cfg(not(unix))]
    let nlinks = Value::fixnum(1);

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
            (Value::fixnum(uid as i64), Value::fixnum(gid as i64))
        }
    };
    #[cfg(not(unix))]
    let (uid_val, gid_val) = if id_format_string {
        (Value::string("0"), Value::string("0"))
    } else {
        (Value::fixnum(0), Value::fixnum(0))
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
        .unwrap_or(Value::NIL);

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
        .unwrap_or(Value::NIL);

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
        .unwrap_or(Value::NIL);

    // Size.
    let size = Value::fixnum(meta.len() as i64);

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
    let gid_changep = Value::T;
    #[cfg(not(unix))]
    let gid_changep = Value::NIL;

    // Inode.
    #[cfg(unix)]
    let inode = {
        use std::os::unix::fs::MetadataExt;
        Value::fixnum(sym_meta.ino() as i64)
    };
    #[cfg(not(unix))]
    let inode = Value::fixnum(0);

    // Device.
    #[cfg(unix)]
    let device = {
        use crate::emacs_core::value::ValueKind;
        use std::os::unix::fs::MetadataExt;
        Value::fixnum(sym_meta.dev() as i64)
    };
    #[cfg(not(unix))]
    let device = Value::fixnum(0);

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

/// Context-backed variant of `directory-files-and-attributes`.
/// Resolves relative DIRECTORY against dynamic/default `default-directory`.
pub(crate) fn builtin_directory_files_and_attributes(
    eval: &mut Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("directory-files-and-attributes", &args, 1, 6)?;
    let dir = expect_lisp_string("directory-files-and-attributes", &args[0])?;
    let dir = super::fileio::resolve_filename_in_state(
        &eval.obarray,
        &[],
        &eval.buffers,
        &dired_runtime_string(&dir),
    );
    directory_files_and_attributes_with_dir(&args, dir)
}

fn directory_files_and_attributes_with_dir(args: &[Value], dir: String) -> EvalResult {
    let full_name = args.get(1).is_some_and(|v| v.is_truthy());
    let match_regexp = match args.get(2) {
        Some(v) if v.is_truthy() => Some(expect_lisp_string("directory-files-and-attributes", v)?),
        _ => None,
    };
    let nosort = args.get(3).is_some_and(|v| v.is_truthy());
    // GNU Emacs: return string names unless ID-FORMAT is nil or 'integer.
    let id_format_string = args
        .get(4)
        .is_some_and(|v| v.is_truthy() && v.as_symbol_name().map_or(true, |s| s != "integer"));
    let count = parse_wholenump_count(args.get(5))?;
    if count == Some(0) {
        return Ok(Value::NIL);
    }

    let names = read_directory_names(&dir)?;

    let dir_with_slash = ensure_trailing_slash(&dir);
    let mut items: VecDeque<(String, String)> = VecDeque::new();
    let mut remaining = count.unwrap_or(usize::MAX);
    for name in names {
        if let Some(pattern) = match_regexp.as_ref() {
            let pattern_runtime = dired_runtime_string(pattern);
            let mut throwaway = None;
            let matched = super::regex::string_match_full_with_case_fold(
                &pattern_runtime,
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
                        pattern_runtime, msg
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
            let attrs = build_file_attributes(&full_path, id_format_string).unwrap_or(Value::NIL);
            Value::cons(runtime_file_name_value(&display_name), attrs)
        })
        .collect();

    Ok(Value::list(result))
}

/// Context-backed variant of `file-name-completion`.
/// This supports arbitrary callable predicates and matches Emacs behavior of
/// binding `default-directory` to DIRECTORY while predicate is invoked.
pub(crate) fn builtin_file_name_completion(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    let plan = prepare_file_name_completion_in_state(&eval.obarray, &[], &eval.buffers, &args)?;
    let predicate = args.get(2);
    finish_file_name_completion_with_eval_predicate(
        eval,
        predicate,
        plan.directory,
        plan.file,
        plan.completions,
        plan.ignore_case,
    )
}

/// Context-backed variant of `file-name-all-completions`.
/// Resolves relative DIRECTORY against dynamic/default `default-directory`.
pub(crate) fn builtin_file_name_all_completions(
    eval: &mut Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("file-name-all-completions", &args, 2, 2)?;

    let file = expect_lisp_string("file-name-all-completions", &args[0])?;
    let file_runtime = dired_runtime_string(&file);
    let directory = expect_lisp_string("file-name-all-completions", &args[1])?;
    let directory = super::fileio::resolve_filename_in_state(
        &eval.obarray,
        &[],
        &eval.buffers,
        &dired_runtime_string(&directory),
    );
    if file_runtime.contains('/') {
        return Ok(Value::NIL);
    }
    let ignore_case = get_completion_ignore_case(&eval.obarray);
    // GNU Emacs: file-name-all-completions does NOT filter by
    // completion-ignored-extensions (the "all_flag" path).
    let completions = collect_file_name_completions(&file_runtime, &directory, ignore_case)?;
    Ok(Value::list(
        completions
            .into_iter()
            .map(|completion| runtime_file_name_value(&completion))
            .collect(),
    ))
}

fn collect_file_name_completions(
    file: &str,
    directory: &str,
    ignore_case: bool,
) -> Result<Vec<String>, Flow> {
    let names = read_directory_names(directory)?;
    let mut completions = VecDeque::new();

    for name in names {
        let matches = if ignore_case {
            name.to_lowercase().starts_with(&file.to_lowercase())
        } else {
            name.starts_with(file)
        };
        if !matches {
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

/// Extract the list of ignored extensions from the `completion-ignored-extensions` variable.
fn get_ignored_extensions(obarray: &super::symbol::Obarray) -> Vec<LispString> {
    let Some(val) = obarray.symbol_value("completion-ignored-extensions") else {
        return Vec::new();
    };
    let val = *val;
    let Some(items) = list_to_vec(&val) else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|v| v.as_lisp_string().cloned())
        .collect()
}

/// Check whether `completion-ignore-case` is truthy.
fn get_completion_ignore_case(obarray: &super::symbol::Obarray) -> bool {
    obarray
        .symbol_value("completion-ignore-case")
        .is_some_and(|v| v.is_truthy())
}

/// Apply `completion-ignored-extensions` filtering to a set of completions.
///
/// This follows GNU Emacs semantics:
/// - If a file name (not exact match with FILE) ends with an ignored extension,
///   it can be excluded.
/// - If a directory name ends with an ignored extension that itself ends in '/',
///   it can be excluded.
/// - "." and ".." directories are always excludable.
/// - If there is at least one non-excludable match, all excludable matches are
///   dropped. If ALL matches are excludable, they are all kept (the "includeall"
///   fallback).
fn filter_by_ignored_extensions(
    file: &str,
    completions: Vec<String>,
    ignored_extensions: &[LispString],
    ignore_case: bool,
) -> Vec<String> {
    if completions.is_empty() {
        return completions;
    }

    let file_len = file.len();

    // Classify each completion as excludable or not.
    let mut classified: Vec<(String, bool)> = Vec::with_capacity(completions.len());
    for comp in completions {
        let is_dir = comp.ends_with('/');
        // The base name (without trailing '/' for directories)
        let base = if is_dir {
            &comp[..comp.len() - 1]
        } else {
            comp.as_str()
        };

        let mut can_exclude = false;

        // "." and ".." are always excludable
        if base == "." || base == ".." {
            can_exclude = true;
        } else if base.len() > file_len {
            // Only check ignored-extensions when the name is longer than FILE
            // (i.e., not an exact match).
            for ext in ignored_extensions {
                let ext_runtime = dired_runtime_string(ext);
                if is_dir {
                    // For directories, only match extensions that end in '/'.
                    if !ext_runtime.ends_with('/') {
                        continue;
                    }
                    let ext_base = &ext_runtime[..ext_runtime.len() - 1]; // strip trailing '/'
                    if ext_base.is_empty() {
                        continue;
                    }
                    let matches = if ignore_case {
                        base.to_lowercase().ends_with(&ext_base.to_lowercase())
                    } else {
                        base.ends_with(ext_base)
                    };
                    if matches {
                        can_exclude = true;
                        break;
                    }
                } else {
                    // For files, match extensions (which should not end in '/').
                    if ext_runtime.ends_with('/') {
                        continue;
                    }
                    let matches = if ignore_case {
                        base.to_lowercase().ends_with(&ext_runtime.to_lowercase())
                    } else {
                        base.ends_with(ext_runtime.as_str())
                    };
                    if matches {
                        can_exclude = true;
                        break;
                    }
                }
            }
        }

        classified.push((comp, can_exclude));
    }

    // GNU Emacs "includeall" logic:
    // If there's at least one non-excludable match, drop all excludable ones.
    // Otherwise (all are excludable), keep them all.
    let has_non_excludable = classified.iter().any(|(_, excl)| !excl);

    if has_non_excludable {
        classified
            .into_iter()
            .filter(|(_, excl)| !excl)
            .map(|(comp, _)| comp)
            .collect()
    } else {
        classified.into_iter().map(|(comp, _)| comp).collect()
    }
}

pub(crate) struct FileNameCompletionPlan {
    pub(crate) file: LispString,
    pub(crate) directory: LispString,
    pub(crate) completions: Vec<LispString>,
    pub(crate) ignore_case: bool,
}

pub(crate) fn prepare_file_name_completion_in_state(
    obarray: &super::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    args: &[Value],
) -> Result<FileNameCompletionPlan, Flow> {
    expect_range_args("file-name-completion", args, 2, 3)?;

    let file = expect_lisp_string("file-name-completion", &args[0])?;
    let file_runtime = dired_runtime_string(&file);
    let directory_arg = expect_lisp_string("file-name-completion", &args[1])?;
    let directory = super::fileio::resolve_filename_in_state(
        obarray,
        dynamic,
        buffers,
        &dired_runtime_string(&directory_arg),
    );
    let directory = runtime_file_name_to_lisp_string(&directory);
    let ignore_case = get_completion_ignore_case(obarray);
    let ignored_extensions = get_ignored_extensions(obarray);
    let completions = if file_runtime.contains('/') {
        Vec::new()
    } else {
        let directory_runtime = dired_runtime_string(&directory);
        let raw = collect_file_name_completions(&file_runtime, &directory_runtime, ignore_case)?;
        // Apply completion-ignored-extensions filtering for file-name-completion
        // (but not for file-name-all-completions, per GNU Emacs).
        filter_by_ignored_extensions(&file_runtime, raw, &ignored_extensions, ignore_case)
            .into_iter()
            .map(|completion| runtime_file_name_to_lisp_string(&completion))
            .collect()
    };

    Ok(FileNameCompletionPlan {
        file,
        directory,
        completions,
        ignore_case,
    })
}

pub(crate) fn finish_file_name_completion_with_eval_predicate(
    eval: &mut Context,
    predicate: Option<&Value>,
    directory: LispString,
    file: LispString,
    completions: Vec<LispString>,
    ignore_case: bool,
) -> EvalResult {
    let Some(predicate) = predicate.copied() else {
        return Ok(resolve_file_name_completion(
            &file,
            completions,
            ignore_case,
        ));
    };
    if predicate.is_nil() {
        return Ok(resolve_file_name_completion(
            &file,
            completions,
            ignore_case,
        ));
    }

    let use_absolute_path = predicate_uses_absolute_file_argument(&eval.obarray, &predicate);
    let bound_directory = directory.clone();
    finish_file_name_completion_with_callable_predicate(
        use_absolute_path,
        directory,
        file,
        completions,
        ignore_case,
        |predicate_arg| {
            with_default_directory_binding(eval, &bound_directory, |eval| {
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
    directory: LispString,
    file: LispString,
    completions: Vec<LispString>,
    ignore_case: bool,
    mut predicate_call: impl FnMut(Value) -> Result<Value, Flow>,
) -> EvalResult {
    let completions = filter_completions_by_callable_predicate(
        use_absolute_path,
        &directory,
        completions,
        |predicate_arg| predicate_call(predicate_arg),
    )?;
    Ok(resolve_file_name_completion(
        &file,
        completions,
        ignore_case,
    ))
}

fn resolve_file_name_completion(
    file: &LispString,
    completions: Vec<LispString>,
    ignore_case: bool,
) -> Value {
    if completions.is_empty() {
        return Value::NIL;
    }

    let filtered = filter_completion_candidates(file, completions);
    if filtered.is_empty() {
        return Value::heap_string(file.clone());
    }

    // If there is exactly one completion and it matches FILE exactly, return t.
    // For directory candidates ending in '/', Emacs returns the completion
    // string when FILE lacks the trailing slash (e.g. ".." -> "../").
    if filtered.len() == 1 {
        let comp = &filtered[0];
        let file_runtime = dired_runtime_string(file);
        let comp_runtime = dired_runtime_string(comp);
        let eq = if ignore_case {
            comp_runtime.eq_ignore_ascii_case(&file_runtime)
        } else {
            comp_runtime == file_runtime
        };
        if eq {
            return Value::T;
        }
        return Value::heap_string(comp.clone());
    }

    // Find the longest common prefix among completions.
    // When completion-ignore-case is set, use case-insensitive comparison
    // but preserve the case of the first match (which GNU Emacs refines to
    // prefer the match whose case matches the input).
    let mut prefix = dired_runtime_string(&filtered[0]);
    for comp in &filtered[1..] {
        let comp_runtime = dired_runtime_string(comp);
        let common_len = if ignore_case {
            prefix
                .chars()
                .zip(comp_runtime.chars())
                .take_while(|(a, b)| a.to_lowercase().eq(b.to_lowercase()))
                .count()
        } else {
            prefix
                .chars()
                .zip(comp_runtime.chars())
                .take_while(|(a, b)| a == b)
                .count()
        };
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
    Value::heap_string(runtime_file_name_to_lisp_string(&prefix))
}

fn filter_completion_candidates(
    file: &LispString,
    completions: Vec<LispString>,
) -> Vec<LispString> {
    let file_runtime = dired_runtime_string(file);
    completions
        .into_iter()
        .filter(|completion| dired_runtime_string(completion) != "./")
        .filter(|completion| {
            file_runtime.starts_with("..") || dired_runtime_string(completion) != "../"
        })
        .collect()
}

fn filter_completions_by_symbol_predicate(
    eval: &mut Context,
    predicate: Option<&Value>,
    directory: &LispString,
    completions: Vec<LispString>,
) -> Result<Vec<LispString>, Flow> {
    let Some(predicate) = predicate else {
        return Ok(completions);
    };
    if predicate.is_nil() {
        return Ok(completions);
    }
    let Some(symbol) = predicate_callable_name(predicate) else {
        // Cannot evaluate lambda/object predicates via dispatch_subr.
        return Ok(completions);
    };

    let mut filtered = Vec::new();
    for candidate in completions {
        if symbol_predicate_matches_candidate(eval, symbol, directory, &candidate)? {
            filtered.push(candidate);
        }
    }
    Ok(filtered)
}

fn symbol_predicate_matches_candidate(
    eval: &mut Context,
    symbol: &str,
    directory: &LispString,
    candidate: &LispString,
) -> Result<bool, Flow> {
    if let Some(result) = eval.dispatch_subr(symbol, vec![Value::heap_string(candidate.clone())]) {
        let result = result?;
        if result.is_truthy() || !is_builtin_path_predicate(symbol) {
            return Ok(result.is_truthy());
        }

        let directory_runtime = dired_runtime_string(directory);
        let candidate_runtime = dired_runtime_string(candidate);
        let absolute = std::path::Path::new(&directory_runtime).join(&candidate_runtime);
        let absolute = absolute.to_string_lossy().into_owned();
        if let Some(result) = eval.dispatch_subr(symbol, vec![runtime_file_name_value(&absolute)]) {
            return Ok(result?.is_truthy());
        }
        return Ok(false);
    }

    // Fallback: try absolute path to make path predicates useful.
    let directory_runtime = dired_runtime_string(directory);
    let candidate_runtime = dired_runtime_string(candidate);
    let absolute = std::path::Path::new(&directory_runtime).join(&candidate_runtime);
    let absolute = absolute.to_string_lossy().into_owned();
    if let Some(result) = eval.dispatch_subr(symbol, vec![runtime_file_name_value(&absolute)]) {
        return Ok(result?.is_truthy());
    }

    // Preserve current behavior for unknown/non-callable predicates.
    Ok(true)
}

fn filter_completions_by_callable_predicate(
    use_absolute_path: bool,
    directory: &LispString,
    completions: Vec<LispString>,
    mut predicate_call: impl FnMut(Value) -> Result<Value, Flow>,
) -> Result<Vec<LispString>, Flow> {
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
    directory: &LispString,
    f: impl FnOnce(&mut Context) -> Result<T, Flow>,
) -> Result<T, Flow> {
    let count = eval.specpdl.len();
    eval.specbind(
        intern("default-directory"),
        Value::heap_string(directory.clone()),
    );
    let result = f(eval);
    eval.unbind_to(count);
    result
}

fn predicate_argument_for_callable_predicate(
    use_absolute_path: bool,
    directory: &LispString,
    candidate: &LispString,
) -> Value {
    if use_absolute_path {
        let directory_runtime = dired_runtime_string(directory);
        let candidate_runtime = dired_runtime_string(candidate);
        let absolute = std::path::Path::new(&directory_runtime).join(&candidate_runtime);
        return runtime_file_name_value(absolute.to_string_lossy().as_ref());
    }

    Value::heap_string(candidate.clone())
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
    match predicate.kind() {
        ValueKind::Symbol(id) => Some(resolve_sym(id)),
        ValueKind::Subr(id) => Some(resolve_sym(id)),
        ValueKind::Veclike(VecLikeType::Subr) => {
            let id = predicate.as_subr_id().unwrap();
            Some(resolve_sym(id))
        }
        _ => None,
    }
}

/// Context-backed variant of `file-attributes`.
/// Resolves relative FILENAME against dynamic/default `default-directory`.
pub(crate) fn builtin_file_attributes(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    expect_range_args("file-attributes", &args, 1, 2)?;

    // GNU Emacs (dired.c:1003-1006): If the filename is not a string
    // (e.g., nil from buffer-file-name on a non-file buffer), return nil
    // instead of signaling an error.
    let filename = match args[0].as_lisp_string() {
        Some(string) => dired_runtime_string(string),
        None if args[0].is_nil() => return Ok(Value::NIL),
        None => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), args[0]],
            ));
        }
    };
    let filename =
        super::fileio::resolve_filename_in_state(&eval.obarray, &[], &eval.buffers, &filename);
    // GNU Emacs: return string names unless ID-FORMAT is nil or 'integer.
    let id_format_string = args
        .get(1)
        .is_some_and(|v| v.is_truthy() && v.as_symbol_name().map_or(true, |s| s != "integer"));

    match build_file_attributes(&filename, id_format_string) {
        Some(attrs) => Ok(attrs),
        None => Ok(Value::NIL),
    }
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

    Ok(Value::bool_val(name1 < name2))
}

/// Extract the car of a cons cell as a string.
fn extract_car_string(_name: &str, val: &Value) -> Result<String, Flow> {
    match val.kind() {
        ValueKind::Cons => {
            let pair_car = val.cons_car();
            match pair_car.kind() {
                ValueKind::String => Ok(dired_runtime_string(
                    pair_car
                        .as_lisp_string()
                        .expect("ValueKind::String must carry LispString payload"),
                )),
                other => Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), *val],
                )),
            }
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("consp"), *val],
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
        return Ok(Value::NIL);
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
