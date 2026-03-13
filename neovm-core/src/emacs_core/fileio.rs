//! File I/O primitives for the Elisp VM.
//!
//! Provides path manipulation, file predicates, read/write operations,
//! directory operations, and file attribute queries.

use std::collections::VecDeque;
#[cfg(unix)]
use std::ffi::{CStr, CString};
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

use super::error::{EvalResult, Flow, signal};
use super::eval::Evaluator;
use super::intern::{intern, resolve_sym};
use super::value::{Value, list_to_vec, with_heap};

// ===========================================================================
// Path operations (pure, no evaluator needed)
// ===========================================================================

/// Expand FILE relative to DEFAULT_DIR (or the current working directory).
/// Handles `~` expansion and absolute path detection.
pub fn expand_file_name(name: &str, default_dir: Option<&str>) -> String {
    // Handle ~ expansion
    let expanded = if name.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            let home_str = home.to_string_lossy();
            format!("{}{}", home_str, &name[1..])
        } else {
            name.to_string()
        }
    } else if name == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            home.to_string_lossy().into_owned()
        } else {
            name.to_string()
        }
    } else {
        name.to_string()
    };

    let path = Path::new(&expanded);
    let preserve_trailing_slash = expanded.ends_with('/');

    // If already absolute, just clean it up
    if path.is_absolute() {
        let mut cleaned = clean_path(&PathBuf::from(&expanded));
        if preserve_trailing_slash && !cleaned.ends_with('/') {
            cleaned.push('/');
        }
        return cleaned;
    }

    // Resolve relative to default_dir or cwd
    let base = if let Some(dir) = default_dir {
        // Recursively expand the default dir too (handles ~ in dir)
        let expanded_dir = expand_file_name(dir, None);
        PathBuf::from(expanded_dir)
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
    };

    let joined = base.join(&expanded);
    let mut cleaned = clean_path(&joined);
    if preserve_trailing_slash && !cleaned.ends_with('/') {
        cleaned.push('/');
    }
    cleaned
}

fn canonicalize_with_missing_suffix(path: &Path) -> PathBuf {
    if let Ok(canon) = fs::canonicalize(path) {
        return canon;
    }

    let mut prefix = path.to_path_buf();
    let mut suffix = VecDeque::new();
    loop {
        if let Ok(canon_prefix) = fs::canonicalize(&prefix) {
            let mut resolved = canon_prefix;
            for part in suffix {
                resolved.push(part);
            }
            return resolved;
        }

        let Some(name) = prefix.file_name().map(|s| s.to_os_string()) else {
            break;
        };
        suffix.push_front(name);
        if !prefix.pop() {
            break;
        }
    }

    path.to_path_buf()
}

/// Resolve FILENAME to a true name, preserving trailing slash marker semantics.
pub fn file_truename(filename: &str, default_dir: Option<&str>) -> String {
    let expanded = expand_file_name(filename, default_dir);
    let preserve_trailing_slash = expanded.ends_with('/');
    let mut resolved = canonicalize_with_missing_suffix(Path::new(&expanded))
        .to_string_lossy()
        .into_owned();

    if preserve_trailing_slash && resolved != "/" && !resolved.ends_with('/') {
        resolved.push('/');
    }

    resolved
}

/// Clean up a path by resolving `.` and `..` components without touching the
/// filesystem (no symlink resolution).
fn clean_path(path: &Path) -> String {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {} // skip "."
            std::path::Component::ParentDir => {
                // Pop the last component if possible
                if !components.is_empty() {
                    components.pop();
                }
            }
            other => components.push(other),
        }
    }
    let result: PathBuf = components.iter().collect();
    result.to_string_lossy().into_owned()
}

/// Return the directory part of FILENAME, or None if there is no directory part.
/// Like Emacs `file-name-directory`: includes the trailing slash.
pub fn file_name_directory(filename: &str) -> Option<String> {
    // Emacs: if the filename ends with /, the whole thing is the directory part
    if filename.ends_with('/') {
        return if filename.is_empty() {
            None
        } else {
            Some(filename.to_string())
        };
    }
    // Find the last /
    filename.rfind('/').map(|pos| filename[..=pos].to_string())
}

/// Return the non-directory part of FILENAME.
/// Like Emacs `file-name-nondirectory`.
pub fn file_name_nondirectory(filename: &str) -> String {
    // Emacs: if the filename ends with /, return ""
    if filename.ends_with('/') {
        return String::new();
    }
    match filename.rfind('/') {
        Some(pos) => filename[pos + 1..].to_string(),
        None => filename.to_string(),
    }
}

/// Return the extension of FILENAME.
/// When PERIOD is nil, returns extension without the leading dot, or nil if missing.
/// Return FILENAME as a directory name (must end in `/`).
/// Like Emacs `file-name-as-directory`.
pub fn file_name_as_directory(filename: &str) -> String {
    if filename.is_empty() {
        "./".to_string()
    } else if filename.ends_with('/') {
        filename.to_string()
    } else {
        format!("{filename}/")
    }
}

/// Return directory FILENAME in file-name form (without trailing slash).
/// Like Emacs `directory-file-name`.
pub fn directory_file_name(filename: &str) -> String {
    if filename.is_empty() {
        return String::new();
    }

    // Emacs keeps exactly two leading slashes as a distinct root marker.
    if filename.bytes().all(|b| b == b'/') {
        return if filename.len() == 2 {
            "//".to_string()
        } else {
            "/".to_string()
        };
    }

    filename.trim_end_matches('/').to_string()
}

/// Concatenate file name components with separator insertion between
/// non-empty components, skipping empty components.
/// Like Emacs `file-name-concat` after filtering nil/empty args.
pub fn file_name_concat(parts: &[&str]) -> String {
    let mut iter = parts.iter().copied().filter(|s| !s.is_empty());
    let Some(first) = iter.next() else {
        return String::new();
    };

    let mut out = first.to_string();
    for part in iter {
        if !out.ends_with('/') {
            out.push('/');
        }
        out.push_str(part);
    }
    out
}

/// Return true if FILENAME is an absolute file name.
/// On Unix this means it starts with `/` or `~`.
pub fn file_name_absolute_p(filename: &str) -> bool {
    filename.starts_with('/') || filename.starts_with('~')
}

/// Return true if NAME is a directory name (ends with a directory separator).
pub fn directory_name_p(name: &str) -> bool {
    name.ends_with('/')
}

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);
static DEFAULT_FILE_MODE_MASK: AtomicU32 = AtomicU32::new(0o022);
static DEFAULT_FILE_MODE_MASK_INIT: Once = Once::new();

fn init_default_file_mode_mask() {
    DEFAULT_FILE_MODE_MASK_INIT.call_once(|| {
        #[cfg(unix)]
        unsafe {
            let old = libc::umask(0);
            libc::umask(old);
            DEFAULT_FILE_MODE_MASK.store(old as u32, Ordering::Relaxed);
        }
    });
}

fn env_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn trim_embedded_absfilename(path: String) -> String {
    let mut current = path;
    loop {
        let bytes = current.as_bytes();
        let mut cut_at = None;
        let mut i = 1usize;
        while i < bytes.len() {
            if bytes[i - 1] == b'/' && (bytes[i] == b'/' || bytes[i] == b'~') {
                cut_at = Some(i);
                break;
            }
            i += 1;
        }
        if let Some(idx) = cut_at {
            current = current[idx..].to_string();
        } else {
            return current;
        }
    }
}

/// Substitute environment variables in FILENAME.
/// Mirrors Emacs `substitute-in-file-name` behavior for local path forms.
pub fn substitute_in_file_name(filename: &str) -> String {
    let bytes = filename.as_bytes();
    let mut out = String::with_capacity(filename.len());
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] != b'$' {
            // Safe because i is always at a valid UTF-8 boundary.
            let ch = filename[i..]
                .chars()
                .next()
                .expect("index points at valid char boundary");
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }

        if i + 1 >= bytes.len() {
            out.push('$');
            i += 1;
            continue;
        }

        match bytes[i + 1] {
            b'$' => {
                out.push('$');
                i += 2;
            }
            b'{' => {
                if let Some(rel_end) = bytes[i + 2..].iter().position(|&b| b == b'}') {
                    let end = i + 2 + rel_end;
                    let var = &filename[i + 2..end];
                    if let Ok(value) = std::env::var(var) {
                        out.push_str(&value);
                    } else {
                        out.push_str(&filename[i..=end]);
                    }
                    i = end + 1;
                } else {
                    // Unclosed ${... keeps '$' literal; rest passes through.
                    out.push('$');
                    i += 1;
                }
            }
            next if env_name_char(next) => {
                let mut end = i + 1;
                while end < bytes.len() && env_name_char(bytes[end]) {
                    end += 1;
                }
                let var = &filename[i + 1..end];
                if let Ok(value) = std::env::var(var) {
                    out.push_str(&value);
                } else {
                    out.push_str(&filename[i..end]);
                }
                i = end;
            }
            _ => {
                out.push('$');
                i += 1;
            }
        }
    }

    trim_embedded_absfilename(out)
}

// ===========================================================================
// File predicates (pure)
// ===========================================================================

/// Return true if FILENAME exists (file, directory, symlink, etc.).
pub fn file_exists_p(filename: &str) -> bool {
    Path::new(filename).exists()
}

/// Return true if FILENAME is readable.
pub fn file_readable_p(filename: &str) -> bool {
    // A file is "readable" if we can open it for reading.
    fs::File::open(filename).is_ok()
}

/// Return true if FILENAME is writable.
/// Checks if the file can be opened for writing, or if it doesn't exist,
/// whether the parent directory is writable.
pub fn file_writable_p(filename: &str) -> bool {
    let path = Path::new(filename);
    if path.exists() {
        fs::OpenOptions::new().write(true).open(filename).is_ok()
    } else {
        // File doesn't exist; check if parent directory is writable
        match path.parent() {
            Some(parent) if parent.exists() => {
                // Try to check write permission on the parent directory
                // by attempting to create a temp file
                let test_path = parent.join(".neovm_write_test");
                match fs::File::create(&test_path) {
                    Ok(_) => {
                        let _ = fs::remove_file(&test_path);
                        true
                    }
                    Err(_) => false,
                }
            }
            _ => false,
        }
    }
}

/// Return true if FILENAME is an accessible directory.
pub fn file_accessible_directory_p(filename: &str) -> bool {
    let path = Path::new(filename);
    if !path.is_dir() {
        return false;
    }

    #[cfg(unix)]
    {
        let Ok(c_path) = CString::new(filename) else {
            return false;
        };
        let mode = libc::R_OK | libc::X_OK;
        unsafe { libc::access(c_path.as_ptr(), mode) == 0 }
    }

    #[cfg(not(unix))]
    {
        return fs::read_dir(path).is_ok();
    }
}

/// Return true if FILENAME is executable by the current process.
pub fn file_executable_p(filename: &str) -> bool {
    #[cfg(unix)]
    {
        let Ok(c_path) = CString::new(filename) else {
            return false;
        };
        unsafe { libc::access(c_path.as_ptr(), libc::X_OK) == 0 }
    }

    #[cfg(not(unix))]
    {
        return Path::new(filename).exists();
    }
}

/// Return true if FILENAME is currently locked by Emacs lockfiles.
///
/// NeoVM currently does not implement lockfile probing, so this returns nil.
pub fn file_locked_p(_filename: &str) -> bool {
    false
}

/// Return filesystem capacity information for PATH.
///
/// The tuple layout matches Emacs `file-system-info`:
/// `(TOTAL-BYTES FREE-BYTES AVAILABLE-BYTES)`.
fn file_system_info(path: &str) -> Result<(i64, i64, i64), Flow> {
    #[cfg(unix)]
    {
        fn saturating_i64(v: u128) -> i64 {
            if v > i64::MAX as u128 {
                i64::MAX
            } else {
                v as i64
            }
        }

        let c_path = CString::new(path.as_bytes()).map_err(|_| {
            signal(
                "file-error",
                vec![
                    Value::string("Getting file system info"),
                    Value::string("embedded NUL in file name"),
                    Value::string(path),
                ],
            )
        })?;
        let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c_path.as_ptr(), &mut stats as *mut libc::statvfs) } != 0 {
            return Err(signal_file_io_path(
                std::io::Error::last_os_error(),
                "Getting file system info",
                path,
            ));
        }

        let block_size = if stats.f_frsize > 0 {
            stats.f_frsize as u128
        } else {
            stats.f_bsize as u128
        };
        let total = (stats.f_blocks as u128) * block_size;
        let free = (stats.f_bfree as u128) * block_size;
        let available = (stats.f_bavail as u128) * block_size;
        Ok((
            saturating_i64(total),
            saturating_i64(free),
            saturating_i64(available),
        ))
    }

    #[cfg(not(unix))]
    {
        let _ = path;
        Ok((0, 0, 0))
    }
}

/// Return true if FILENAME is a directory.
pub fn file_directory_p(filename: &str) -> bool {
    Path::new(filename).is_dir()
}

/// Return true if FILENAME is a regular file.
pub fn file_regular_p(filename: &str) -> bool {
    Path::new(filename).is_file()
}

/// Return true if FILENAME is a symbolic link.
pub fn file_symlink_p(filename: &str) -> bool {
    match fs::symlink_metadata(filename) {
        Ok(meta) => meta.file_type().is_symlink(),
        Err(_) => false,
    }
}

/// Return true if FILENAME is on a case-insensitive filesystem.
pub fn file_name_case_insensitive_p(filename: &str) -> bool {
    let mut probe = PathBuf::from(filename);
    while !probe.exists() {
        if !probe.pop() || probe.as_os_str().is_empty() {
            return false;
        }
    }
    #[cfg(windows)]
    {
        true
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Return true if FILE1 has a newer modification time than FILE2.
pub fn file_newer_than_file_p(file1: &str, file2: &str) -> bool {
    let meta1 = match fs::metadata(file1) {
        Ok(meta) => meta,
        Err(_) => return false,
    };
    let meta2 = match fs::metadata(file2) {
        Ok(meta) => meta,
        Err(_) => return true,
    };

    let mtime1 = match meta1.modified() {
        Ok(time) => time,
        Err(_) => return false,
    };
    let mtime2 = match meta2.modified() {
        Ok(time) => time,
        Err(_) => return true,
    };
    mtime1 > mtime2
}

// ===========================================================================
// File I/O operations
// ===========================================================================

/// Read the contents of FILENAME as a UTF-8 string.
pub fn read_file_contents(filename: &str) -> std::io::Result<String> {
    fs::read_to_string(filename)
}

/// Write CONTENT to FILENAME, optionally appending.
pub fn write_string_to_file(content: &str, filename: &str, append: bool) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = if append {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(filename)?
    } else {
        fs::File::create(filename)?
    };
    file.write_all(content.as_bytes())
}

// ===========================================================================
// Directory operations
// ===========================================================================

/// Return a list of file names in DIR.
/// If FULL is true, return absolute paths.
/// If MATCH_REGEX is Some, only include entries whose names match the regex.
/// If NOSORT is true, preserve filesystem enumeration order.
/// COUNT limits the number of accepted entries during enumeration.
#[cfg(unix)]
fn read_directory_names(dir: &str) -> Result<Vec<String>, DirectoryFilesError> {
    let dir_cstr = CString::new(dir).map_err(|_| DirectoryFilesError::Io {
        action: "Opening directory",
        err: std::io::Error::new(ErrorKind::InvalidInput, "path contains interior NUL"),
    })?;
    let dirp = unsafe { libc::opendir(dir_cstr.as_ptr()) };
    if dirp.is_null() {
        return Err(DirectoryFilesError::Io {
            action: "Opening directory",
            err: std::io::Error::last_os_error(),
        });
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
fn read_directory_names(dir: &str) -> Result<Vec<String>, DirectoryFilesError> {
    let entries = fs::read_dir(dir).map_err(|e| DirectoryFilesError::Io {
        action: "Opening directory",
        err: e,
    })?;
    let mut names = vec![".".to_string(), "..".to_string()];
    for entry in entries {
        let entry = entry.map_err(|e| DirectoryFilesError::Io {
            action: "Reading directory entry",
            err: e,
        })?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    Ok(names)
}

#[derive(Debug)]
enum DirectoryFilesError {
    Io {
        action: &'static str,
        err: std::io::Error,
    },
    InvalidRegexp(String),
}

fn directory_files(
    dir: &str,
    full: bool,
    match_regex: Option<&str>,
    nosort: bool,
    count: Option<usize>,
) -> Result<Vec<String>, DirectoryFilesError> {
    if count == Some(0) {
        return Ok(Vec::new());
    }

    let re = match match_regex {
        Some(pattern) => Some(Regex::new(pattern).map_err(|e| {
            DirectoryFilesError::InvalidRegexp(format!("Invalid regexp \"{}\": {}", pattern, e))
        })?),
        None => None,
    };

    let names = read_directory_names(dir)?;

    // Emacs builds this list via `cons` while scanning readdir output.
    // That makes NOSORT results reverse the traversal order and applies COUNT
    // before sort.
    let mut result = VecDeque::new();
    let mut remaining = count.unwrap_or(usize::MAX);
    let dir_with_slash = if dir.ends_with('/') {
        dir.to_string()
    } else {
        format!("{dir}/")
    };

    for name in names {
        if let Some(re) = re.as_ref() {
            if !re.is_match(&name) {
                continue;
            }
        }

        if full {
            result.push_front(format!("{dir_with_slash}{name}"));
        } else {
            result.push_front(name);
        }

        if remaining != usize::MAX {
            remaining -= 1;
            if remaining == 0 {
                break;
            }
        }
    }

    let mut result: Vec<String> = result.into_iter().collect();
    if !nosort {
        result.sort();
    }
    Ok(result)
}

/// Create directory DIR.  If PARENTS is true, create parent directories as needed.
pub fn make_directory(dir: &str, parents: bool) -> std::io::Result<()> {
    if parents {
        fs::create_dir_all(dir)
    } else {
        fs::create_dir(dir)
    }
}

// ===========================================================================
// File management
// ===========================================================================

/// Delete FILENAME.
pub fn delete_file(filename: &str) -> std::io::Result<()> {
    fs::remove_file(filename)
}

/// Rename file FROM to TO.
pub fn rename_file(from: &str, to: &str) -> std::io::Result<()> {
    fs::rename(from, to)
}

/// Copy file FROM to TO.
pub fn copy_file(from: &str, to: &str) -> std::io::Result<()> {
    fs::copy(from, to).map(|_| ())
}

/// Create an additional name (hard link) from OLDNAME to NEWNAME.
pub fn add_name_to_file(oldname: &str, newname: &str) -> std::io::Result<()> {
    fs::hard_link(oldname, newname)
}

// ===========================================================================
// File attributes
// ===========================================================================

/// Metadata about a file.
#[derive(Debug, Clone)]
pub struct FileAttributes {
    pub size: u64,
    pub nlinks: u64,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub modified: Option<f64>, // seconds since epoch
    pub modes: u32,
}

/// Return file attributes for FILENAME, or None if the file doesn't exist.
pub fn file_attributes(filename: &str) -> Option<FileAttributes> {
    let meta = fs::metadata(filename).ok()?;
    let symlink_meta = fs::symlink_metadata(filename).ok();

    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64());

    #[cfg(unix)]
    let modes = {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode()
    };
    #[cfg(not(unix))]
    let modes = if meta.permissions().readonly() {
        0o444
    } else {
        0o644
    };

    #[cfg(unix)]
    let nlinks = {
        use std::os::unix::fs::MetadataExt;
        meta.nlink()
    };
    #[cfg(not(unix))]
    let nlinks = 1;

    Some(FileAttributes {
        size: meta.len(),
        nlinks,
        is_dir: meta.is_dir(),
        is_symlink: symlink_meta.is_some_and(|m| m.file_type().is_symlink()),
        modified,
        modes,
    })
}

fn file_modes(filename: &str) -> Option<u32> {
    let meta = fs::symlink_metadata(filename).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(meta.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        Some(if meta.permissions().readonly() {
            0o444
        } else {
            0o644
        })
    }
}

// ===========================================================================
// Builtin wrappers — pure (no evaluator needed)
// ===========================================================================

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

fn expect_string_strict(value: &Value) -> Result<String, Flow> {
    match value {
        Value::Str(id) => Ok(with_heap(|h| h.get_string(*id).to_owned())),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
    }
}

fn expect_temp_prefix(value: &Value) -> Result<String, Flow> {
    match value {
        Value::Str(id) => Ok(with_heap(|h| h.get_string(*id).to_owned())),
        Value::Nil | Value::Cons(_) | Value::Vector(_) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *value],
        )),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), *other],
        )),
    }
}

fn expect_fixnum(value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("fixnump"), *other],
        )),
    }
}

fn normalize_secs_nanos(mut secs: i64, mut nanos: i64) -> (i64, i64) {
    if nanos >= 1_000_000_000 {
        secs += nanos / 1_000_000_000;
        nanos %= 1_000_000_000;
    } else if nanos < 0 {
        let borrow = ((-nanos) + 999_999_999) / 1_000_000_000;
        secs -= borrow;
        nanos += borrow * 1_000_000_000;
    }
    (secs, nanos)
}

fn parse_timestamp_arg(value: &Value) -> Result<(i64, i64), Flow> {
    match value {
        Value::Int(n) => Ok((*n, 0)),
        Value::Float(f, _) => {
            let secs = f.floor() as i64;
            let nanos = ((f - f.floor()) * 1_000_000_000.0).round() as i64;
            Ok(normalize_secs_nanos(secs, nanos))
        }
        Value::Cons(_) => {
            let items = list_to_vec(value).ok_or_else(|| {
                signal("wrong-type-argument", vec![Value::symbol("listp"), *value])
            })?;
            if items.len() < 2 {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), *value],
                ));
            }
            let high = items[0].as_int().ok_or_else(|| {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), items[0]],
                )
            })?;
            let low = items[1].as_int().ok_or_else(|| {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), items[1]],
                )
            })?;
            let usec = if items.len() > 2 {
                items[2].as_int().unwrap_or(0)
            } else {
                0
            };
            let secs = high * 65_536 + low;
            let nanos = usec * 1_000;
            Ok(normalize_secs_nanos(secs, nanos))
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("numberp"), *other],
        )),
    }
}

fn validate_file_truename_counter(counter: &Value) -> Result<(), Flow> {
    if counter.is_nil() {
        return Ok(());
    }
    if !counter.is_list() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), *counter],
        ));
    }
    if let Value::Cons(cell) = counter {
        let first = with_heap(|h| h.cons_car(*cell));
        if !matches!(first, Value::Int(_) | Value::Float(_, _) | Value::Char(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("number-or-marker-p"), first],
            ));
        }
    }
    Ok(())
}

fn temporary_file_directory_for_eval(eval: &Evaluator) -> Option<String> {
    let name_id = intern("temporary-file-directory");
    for frame in eval.dynamic.iter().rev() {
        if let Some(value) = frame.get(&name_id) {
            if let Value::Str(id) = value {
                return Some(with_heap(|h| h.get_string(*id).to_owned()));
            }
        }
    }
    match eval.obarray.symbol_value("temporary-file-directory") {
        Some(Value::Str(id)) => Some(with_heap(|h| h.get_string(*id).to_owned())),
        _ => None,
    }
}

fn make_temp_file_impl(
    temp_dir: &str,
    prefix: &str,
    dir_flag: bool,
    suffix: &str,
    text: Option<&str>,
) -> Result<String, Flow> {
    let base = PathBuf::from(temp_dir);

    for _ in 0..256 {
        let nonce = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let candidate = base.join(format!("{prefix}{now:x}{nonce:x}{suffix}"));
        let candidate_str = candidate.to_string_lossy().into_owned();

        if dir_flag {
            match fs::create_dir(&candidate) {
                Ok(()) => {
                    if let Some(contents) = text {
                        let mut file = fs::OpenOptions::new()
                            .write(true)
                            .open(&candidate)
                            .map_err(|err| {
                                signal_file_io_path(err, "Writing to", &candidate_str)
                            })?;
                        file.write_all(contents.as_bytes()).map_err(|err| {
                            signal_file_io_path(err, "Writing to", &candidate_str)
                        })?;
                    }
                    return Ok(candidate_str);
                }
                Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
                Err(err) => {
                    return Err(signal_file_io_path(
                        err,
                        "Creating directory",
                        &candidate_str,
                    ));
                }
            }
        } else {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(mut file) => {
                    if let Some(contents) = text {
                        file.write_all(contents.as_bytes()).map_err(|err| {
                            signal_file_io_path(err, "Writing to", &candidate_str)
                        })?;
                    }
                    return Ok(candidate_str);
                }
                Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(signal_file_io_path(err, "Creating file", &candidate_str)),
            }
        }
    }

    Err(signal(
        "file-error",
        vec![Value::string("Cannot create temporary file")],
    ))
}

fn split_nearby_temp_prefix(prefix: &str) -> Option<(String, String)> {
    let path = Path::new(prefix);
    if !path.is_absolute() {
        return None;
    }
    let file_name = path.file_name()?.to_string_lossy().into_owned();
    if file_name.is_empty() {
        return None;
    }
    let parent = path.parent()?;
    if parent.as_os_str().is_empty() || parent == Path::new(".") {
        return None;
    }
    Some((parent.to_string_lossy().into_owned(), file_name))
}

fn make_temp_name_suffix() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let nonce = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let mut value = now ^ nonce.rotate_left(7);
    let mut out = [b'a'; 6];
    for slot in &mut out {
        let idx = (value % ALPHABET.len() as u64) as usize;
        *slot = ALPHABET[idx];
        value = value / ALPHABET.len() as u64 + 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// (expand-file-name NAME &optional DEFAULT-DIRECTORY) -> string
pub(crate) fn builtin_expand_file_name(args: Vec<Value>) -> EvalResult {
    expect_min_args("expand-file-name", &args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("expand-file-name"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let name = expect_string_strict(&args[0])?;
    let default_dir = if let Some(arg) = args.get(1) {
        match arg {
            Value::Nil => None,
            Value::Str(id) => Some(with_heap(|h| h.get_string(*id).to_owned())),
            // Emacs treats non-string DEFAULT-DIRECTORY as root.
            _ => Some("/".to_string()),
        }
    } else {
        None
    };
    Ok(Value::string(expand_file_name(
        &name,
        default_dir.as_deref(),
    )))
}

/// Evaluator-aware variant of `expand-file-name` that falls back to dynamic
/// `default-directory` when DEFAULT-DIRECTORY is omitted or nil.
pub(crate) fn builtin_expand_file_name_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("expand-file-name", &args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("expand-file-name"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let name = expect_string_strict(&args[0])?;
    let default_dir = if let Some(arg) = args.get(1) {
        match arg {
            Value::Nil => default_directory_for_eval(eval),
            Value::Str(id) => Some(with_heap(|h| h.get_string(*id).to_owned())),
            // Emacs treats non-string DEFAULT-DIRECTORY as root.
            _ => Some("/".to_string()),
        }
    } else {
        default_directory_for_eval(eval)
    };

    Ok(Value::string(expand_file_name(
        &name,
        default_dir.as_deref(),
    )))
}

/// (make-temp-file PREFIX &optional DIR-FLAG SUFFIX TEXT) -> string
pub(crate) fn builtin_make_temp_file(args: Vec<Value>) -> EvalResult {
    expect_min_args("make-temp-file", &args, 1)?;
    if args.len() > 4 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("make-temp-file"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let prefix = expect_temp_prefix(&args[0])?;
    let dir_flag = args.get(1).is_some_and(|value| value.is_truthy());
    let suffix = match args.get(2) {
        None | Some(Value::Nil) => String::new(),
        Some(value) => expect_string_strict(value)?,
    };
    let text = match args.get(3) {
        None | Some(Value::Nil) => None,
        Some(Value::Str(id)) => Some(with_heap(|h| h.get_string(*id).to_owned())),
        Some(_) => None,
    };
    let temp_dir = std::env::temp_dir().to_string_lossy().into_owned();

    let path = make_temp_file_impl(&temp_dir, &prefix, dir_flag, &suffix, text.as_deref())?;
    Ok(Value::string(path))
}

/// (make-temp-name PREFIX) -> string
pub(crate) fn builtin_make_temp_name(args: Vec<Value>) -> EvalResult {
    expect_args("make-temp-name", &args, 1)?;
    let prefix = expect_string_strict(&args[0])?;
    Ok(Value::string(format!(
        "{prefix}{}",
        make_temp_name_suffix()
    )))
}

/// (next-read-file-uses-dialog-p) -> nil
pub(crate) fn builtin_next_read_file_uses_dialog_p(args: Vec<Value>) -> EvalResult {
    expect_args("next-read-file-uses-dialog-p", &args, 0)?;
    Ok(Value::Nil)
}

/// (unhandled-file-name-directory FILENAME) -> directory string
pub(crate) fn builtin_unhandled_file_name_directory(args: Vec<Value>) -> EvalResult {
    expect_args("unhandled-file-name-directory", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    Ok(Value::string(file_name_as_directory(&filename)))
}

/// (get-truename-buffer FILENAME) -> buffer or nil
pub(crate) fn builtin_get_truename_buffer(args: Vec<Value>) -> EvalResult {
    expect_args("get-truename-buffer", &args, 1)?;
    let _filename = &args[0];
    Ok(Value::Nil)
}

/// Evaluator-aware variant of `make-temp-file` that honors dynamic
/// `temporary-file-directory`.
pub(crate) fn builtin_make_temp_file_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("make-temp-file", &args, 1)?;
    if args.len() > 4 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("make-temp-file"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let prefix = expect_temp_prefix(&args[0])?;
    let dir_flag = args.get(1).is_some_and(|value| value.is_truthy());
    let suffix = match args.get(2) {
        None | Some(Value::Nil) => String::new(),
        Some(value) => expect_string_strict(value)?,
    };
    let text = match args.get(3) {
        None | Some(Value::Nil) => None,
        Some(Value::Str(id)) => Some(with_heap(|h| h.get_string(*id).to_owned())),
        Some(_) => None,
    };
    let temp_dir = temporary_file_directory_for_eval(eval)
        .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned());

    let path = make_temp_file_impl(&temp_dir, &prefix, dir_flag, &suffix, text.as_deref())?;
    Ok(Value::string(path))
}

/// (make-nearby-temp-file PREFIX &optional DIR-FLAG SUFFIX) -> string
pub(crate) fn builtin_make_nearby_temp_file(args: Vec<Value>) -> EvalResult {
    expect_min_args("make-nearby-temp-file", &args, 1)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("make-nearby-temp-file"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let prefix = expect_temp_prefix(&args[0])?;
    let dir_flag = args.get(1).is_some_and(|value| value.is_truthy());
    let suffix = match args.get(2) {
        None | Some(Value::Nil) => String::new(),
        Some(value) => expect_string_strict(value)?,
    };
    let fallback_temp_dir = std::env::temp_dir().to_string_lossy().into_owned();
    let (temp_dir, file_prefix) =
        split_nearby_temp_prefix(&prefix).unwrap_or_else(|| (fallback_temp_dir, prefix.clone()));

    let path = make_temp_file_impl(&temp_dir, &file_prefix, dir_flag, &suffix, None)?;
    Ok(Value::string(path))
}

/// Evaluator-aware variant of `make-nearby-temp-file` that resolves relative
/// directory-containing prefixes against dynamic/default `default-directory`
/// and honors dynamic `temporary-file-directory` fallback.
pub(crate) fn builtin_make_nearby_temp_file_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("make-nearby-temp-file", &args, 1)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("make-nearby-temp-file"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let prefix = expect_temp_prefix(&args[0])?;
    let dir_flag = args.get(1).is_some_and(|value| value.is_truthy());
    let suffix = match args.get(2) {
        None | Some(Value::Nil) => String::new(),
        Some(value) => expect_string_strict(value)?,
    };
    let fallback_temp_dir = temporary_file_directory_for_eval(eval)
        .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned());
    let (temp_dir, file_prefix) =
        split_nearby_temp_prefix(&prefix).unwrap_or_else(|| (fallback_temp_dir, prefix.clone()));

    let path = make_temp_file_impl(&temp_dir, &file_prefix, dir_flag, &suffix, None)?;
    Ok(Value::string(path))
}

/// (file-truename FILENAME &optional COUNTER PREV-DIRS) -> string
pub(crate) fn builtin_file_truename(args: Vec<Value>) -> EvalResult {
    expect_min_args("file-truename", &args, 1)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("file-truename"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let filename = expect_string_strict(&args[0])?;
    if let Some(counter) = args.get(1) {
        validate_file_truename_counter(counter)?;
    }

    Ok(Value::string(file_truename(&filename, None)))
}

/// Evaluator-aware variant of `file-truename` that resolves relative
/// filenames against dynamic/default `default-directory`.
pub(crate) fn builtin_file_truename_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("file-truename", &args, 1)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("file-truename"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let filename = expect_string_strict(&args[0])?;
    if let Some(counter) = args.get(1) {
        validate_file_truename_counter(counter)?;
    }

    Ok(Value::string(file_truename(
        &filename,
        default_directory_for_eval(eval).as_deref(),
    )))
}

/// (file-name-directory FILENAME) -> string or nil
pub(crate) fn builtin_file_name_directory(args: Vec<Value>) -> EvalResult {
    expect_args("file-name-directory", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    match file_name_directory(&filename) {
        Some(dir) => Ok(Value::string(dir)),
        None => Ok(Value::Nil),
    }
}

/// (file-name-nondirectory FILENAME) -> string
pub(crate) fn builtin_file_name_nondirectory(args: Vec<Value>) -> EvalResult {
    expect_args("file-name-nondirectory", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    Ok(Value::string(file_name_nondirectory(&filename)))
}

/// (file-name-as-directory FILENAME) -> string
pub(crate) fn builtin_file_name_as_directory(args: Vec<Value>) -> EvalResult {
    expect_args("file-name-as-directory", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    Ok(Value::string(file_name_as_directory(&filename)))
}

/// (directory-file-name FILENAME) -> string
pub(crate) fn builtin_directory_file_name(args: Vec<Value>) -> EvalResult {
    expect_args("directory-file-name", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    Ok(Value::string(directory_file_name(&filename)))
}

/// (file-name-concat DIRECTORY &rest COMPONENTS) -> string
pub(crate) fn builtin_file_name_concat(args: Vec<Value>) -> EvalResult {
    expect_min_args("file-name-concat", &args, 1)?;

    let mut parts = Vec::new();
    for value in args {
        match value {
            Value::Nil => {}
            Value::Str(id) => {
                let s = with_heap(|h| h.get_string(id).to_owned());
                if !s.is_empty() {
                    parts.push(s);
                }
            }
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), other],
                ));
            }
        }
    }

    let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
    Ok(Value::string(file_name_concat(&refs)))
}

/// (file-name-absolute-p FILENAME) -> t or nil
pub(crate) fn builtin_file_name_absolute_p(args: Vec<Value>) -> EvalResult {
    expect_args("file-name-absolute-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    Ok(Value::bool(file_name_absolute_p(&filename)))
}

/// (directory-name-p NAME) -> t or nil
pub(crate) fn builtin_directory_name_p(args: Vec<Value>) -> EvalResult {
    expect_args("directory-name-p", &args, 1)?;
    let name = expect_string_strict(&args[0])?;
    Ok(Value::bool(directory_name_p(&name)))
}

/// (substitute-in-file-name FILENAME) -> string
pub(crate) fn builtin_substitute_in_file_name(args: Vec<Value>) -> EvalResult {
    expect_args("substitute-in-file-name", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    Ok(Value::string(substitute_in_file_name(&filename)))
}

fn default_directory_for_eval(eval: &Evaluator) -> Option<String> {
    let name_id = intern("default-directory");
    for frame in eval.dynamic.iter().rev() {
        if let Some(value) = frame.get(&name_id) {
            return match value {
                Value::Str(id) => Some(with_heap(|h| h.get_string(*id).to_owned())),
                _ => None,
            };
        }
    }
    match eval.obarray.symbol_value("default-directory") {
        Some(Value::Str(id)) => Some(with_heap(|h| h.get_string(*id).to_owned())),
        _ => None,
    }
}

pub(crate) fn resolve_filename_for_eval(eval: &Evaluator, filename: &str) -> String {
    if filename.is_empty() || Path::new(filename).is_absolute() {
        return filename.to_string();
    }
    let default_dir = default_directory_for_eval(eval);
    expand_file_name(filename, default_dir.as_deref())
}

fn file_error_symbol(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::NotFound => "file-missing",
        ErrorKind::AlreadyExists => "file-already-exists",
        ErrorKind::PermissionDenied => "permission-denied",
        _ => "file-error",
    }
}

fn signal_file_io_error(err: std::io::Error, context: String) -> Flow {
    let symbol = file_error_symbol(err.kind());
    signal(symbol, vec![Value::string(format!("{context}: {err}"))])
}

fn signal_file_io_path(err: std::io::Error, action: &str, path: &str) -> Flow {
    signal_file_io_error(err, format!("{action} {path}"))
}

fn signal_file_io_paths(err: std::io::Error, action: &str, from: &str, to: &str) -> Flow {
    signal_file_io_error(err, format!("{action} {from} to {to}"))
}

fn signal_directory_files_error(err: DirectoryFilesError, dir: &str) -> Flow {
    match err {
        DirectoryFilesError::Io { action, err } => signal_file_io_path(err, action, dir),
        DirectoryFilesError::InvalidRegexp(msg) => {
            signal("invalid-regexp", vec![Value::string(msg)])
        }
    }
}

fn signal_file_action_error(err: std::io::Error, action: &str, path: &str) -> Flow {
    signal(
        file_error_symbol(err.kind()),
        vec![
            Value::string(action),
            Value::string(err.to_string()),
            Value::string(path),
        ],
    )
}

fn set_file_times_compat(
    filename: &str,
    timestamp: Option<(i64, i64)>,
    nofollow: bool,
) -> Result<(), Flow> {
    #[cfg(unix)]
    {
        let c_path = CString::new(filename.as_bytes()).map_err(|_| {
            signal(
                "file-error",
                vec![
                    Value::string("Setting file times"),
                    Value::string("embedded NUL in file name"),
                    Value::string(filename),
                ],
            )
        })?;

        let mut ts = [
            libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
        ];
        if let Some((secs, nanos)) = timestamp {
            ts[0].tv_sec = secs as libc::time_t;
            ts[1].tv_sec = secs as libc::time_t;
            ts[0].tv_nsec = nanos as libc::c_long;
            ts[1].tv_nsec = nanos as libc::c_long;
        } else {
            ts[0].tv_nsec = libc::UTIME_NOW as libc::c_long;
            ts[1].tv_nsec = libc::UTIME_NOW as libc::c_long;
        }
        let flags = if nofollow {
            libc::AT_SYMLINK_NOFOLLOW
        } else {
            0
        };
        let result =
            unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), ts.as_ptr(), flags) };
        if result != 0 {
            return Err(signal_file_action_error(
                std::io::Error::last_os_error(),
                "Setting file times",
                filename,
            ));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (timestamp, nofollow);
        Err(signal(
            "file-error",
            vec![
                Value::string("Setting file times"),
                Value::string("set-file-times is unsupported on this platform"),
                Value::string(filename),
            ],
        ))
    }
}

fn delete_file_compat(filename: &str) -> Result<(), Flow> {
    match delete_file(filename) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(signal_file_io_path(err, "Deleting", filename)),
    }
}

/// `(access-file FILE OPERATION)` -- verify FILE is accessible for OPERATION.
pub(crate) fn builtin_access_file(args: Vec<Value>) -> EvalResult {
    expect_args("access-file", &args, 2)?;
    let filename = expect_string_strict(&args[0])?;
    let operation = expect_string_strict(&args[1])?;
    match fs::metadata(&filename) {
        Ok(_) => Ok(Value::Nil),
        Err(err) => Err(signal_file_action_error(err, &operation, &filename)),
    }
}

/// Evaluator-aware variant of `access-file` that resolves relative paths
/// against dynamic/default `default-directory`.
pub(crate) fn builtin_access_file_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("access-file", &args, 2)?;
    let filename = resolve_filename_for_eval(eval, &expect_string_strict(&args[0])?);
    let operation = expect_string_strict(&args[1])?;
    match fs::metadata(&filename) {
        Ok(_) => Ok(Value::Nil),
        Err(err) => Err(signal_file_action_error(err, &operation, &filename)),
    }
}

/// (file-exists-p FILENAME) -> t or nil
pub(crate) fn builtin_file_exists_p(args: Vec<Value>) -> EvalResult {
    expect_args("file-exists-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    Ok(Value::bool(file_exists_p(&filename)))
}

/// Evaluator-aware variant of `file-exists-p` that resolves relative paths
/// against dynamic/default `default-directory`.
pub(crate) fn builtin_file_exists_p_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("file-exists-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    Ok(Value::bool(file_exists_p(&filename)))
}

/// (file-readable-p FILENAME) -> t or nil
pub(crate) fn builtin_file_readable_p(args: Vec<Value>) -> EvalResult {
    expect_args("file-readable-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    Ok(Value::bool(file_readable_p(&filename)))
}

/// Evaluator-aware variant of `file-readable-p` that resolves relative paths
/// against dynamic/default `default-directory`.
pub(crate) fn builtin_file_readable_p_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("file-readable-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    Ok(Value::bool(file_readable_p(&filename)))
}

/// (file-writable-p FILENAME) -> t or nil
pub(crate) fn builtin_file_writable_p(args: Vec<Value>) -> EvalResult {
    expect_args("file-writable-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    Ok(Value::bool(file_writable_p(&filename)))
}

/// Evaluator-aware variant of `file-writable-p` that resolves relative paths
/// against dynamic/default `default-directory`.
pub(crate) fn builtin_file_writable_p_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("file-writable-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    Ok(Value::bool(file_writable_p(&filename)))
}

/// (file-accessible-directory-p FILENAME) -> t or nil
pub(crate) fn builtin_file_accessible_directory_p(args: Vec<Value>) -> EvalResult {
    expect_args("file-accessible-directory-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    Ok(Value::bool(file_accessible_directory_p(&filename)))
}

/// Evaluator-aware variant of `file-accessible-directory-p` that resolves
/// relative paths against dynamic/default `default-directory`.
pub(crate) fn builtin_file_accessible_directory_p_eval(
    eval: &Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("file-accessible-directory-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    Ok(Value::bool(file_accessible_directory_p(&filename)))
}

/// (file-executable-p FILENAME) -> t or nil
pub(crate) fn builtin_file_executable_p(args: Vec<Value>) -> EvalResult {
    expect_args("file-executable-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    Ok(Value::bool(file_executable_p(&filename)))
}

/// Evaluator-aware variant of `file-executable-p` that resolves relative paths
/// against dynamic/default `default-directory`.
pub(crate) fn builtin_file_executable_p_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("file-executable-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    Ok(Value::bool(file_executable_p(&filename)))
}

/// (file-acl FILENAME) -> ACL string or nil
pub(crate) fn builtin_file_acl(args: Vec<Value>) -> EvalResult {
    expect_args("file-acl", &args, 1)?;
    Ok(Value::Nil)
}

/// Evaluator-aware variant of `file-acl`.
pub(crate) fn builtin_file_acl_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("file-acl", &args, 1)?;
    if let Some(filename) = args.first().and_then(|value| value.as_str()) {
        let _ = resolve_filename_for_eval(eval, filename);
    }
    Ok(Value::Nil)
}

/// (set-file-acl FILENAME ACL) -> nil
pub(crate) fn builtin_set_file_acl(args: Vec<Value>) -> EvalResult {
    expect_args("set-file-acl", &args, 2)?;
    let _filename = &args[0];
    let _acl = &args[1];
    Ok(Value::Nil)
}

/// (file-locked-p FILENAME) -> locker info or nil
pub(crate) fn builtin_file_locked_p(args: Vec<Value>) -> EvalResult {
    expect_args("file-locked-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    Ok(Value::bool(file_locked_p(&filename)))
}

/// Evaluator-aware variant of `file-locked-p` that resolves relative paths
/// against dynamic/default `default-directory`.
pub(crate) fn builtin_file_locked_p_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("file-locked-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    Ok(Value::bool(file_locked_p(&filename)))
}

/// (file-selinux-context FILENAME) -> (user role type range)
pub(crate) fn builtin_file_selinux_context(args: Vec<Value>) -> EvalResult {
    expect_args("file-selinux-context", &args, 1)?;
    let _filename = expect_string_strict(&args[0])?;
    Ok(Value::list(vec![
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ]))
}

/// Evaluator-aware variant of `file-selinux-context`.
pub(crate) fn builtin_file_selinux_context_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("file-selinux-context", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let _filename = resolve_filename_for_eval(eval, &filename);
    Ok(Value::list(vec![
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ]))
}

/// (set-file-selinux-context FILENAME CONTEXT) -> nil
pub(crate) fn builtin_set_file_selinux_context(args: Vec<Value>) -> EvalResult {
    expect_args("set-file-selinux-context", &args, 2)?;
    let _filename = expect_string_strict(&args[0])?;
    let _context = &args[1];
    Ok(Value::Nil)
}

/// (file-system-info PATH) -> (total free avail) in bytes
pub(crate) fn builtin_file_system_info(args: Vec<Value>) -> EvalResult {
    expect_args("file-system-info", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let (total, free, avail) = file_system_info(&filename)?;
    Ok(Value::list(vec![
        Value::Int(total),
        Value::Int(free),
        Value::Int(avail),
    ]))
}

/// Evaluator-aware variant of `file-system-info` that resolves relative paths
/// against dynamic/default `default-directory`.
pub(crate) fn builtin_file_system_info_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("file-system-info", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    let (total, free, avail) = file_system_info(&filename)?;
    Ok(Value::list(vec![
        Value::Int(total),
        Value::Int(free),
        Value::Int(avail),
    ]))
}

/// (file-directory-p FILENAME) -> t or nil
pub(crate) fn builtin_file_directory_p(args: Vec<Value>) -> EvalResult {
    expect_args("file-directory-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    Ok(Value::bool(file_directory_p(&filename)))
}

/// Evaluator-aware variant of `file-directory-p` that resolves relative paths
/// against dynamic/default `default-directory`.
pub(crate) fn builtin_file_directory_p_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("file-directory-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    Ok(Value::bool(file_directory_p(&filename)))
}

/// (file-regular-p FILENAME) -> t or nil
pub(crate) fn builtin_file_regular_p(args: Vec<Value>) -> EvalResult {
    expect_args("file-regular-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    Ok(Value::bool(file_regular_p(&filename)))
}

/// Evaluator-aware variant of `file-regular-p` that resolves relative paths
/// against dynamic/default `default-directory`.
pub(crate) fn builtin_file_regular_p_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("file-regular-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    Ok(Value::bool(file_regular_p(&filename)))
}

/// (file-symlink-p FILENAME) -> t or nil
pub(crate) fn builtin_file_symlink_p(args: Vec<Value>) -> EvalResult {
    expect_args("file-symlink-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    Ok(Value::bool(file_symlink_p(&filename)))
}

/// Evaluator-aware variant of `file-symlink-p` that resolves relative paths
/// against dynamic/default `default-directory`.
pub(crate) fn builtin_file_symlink_p_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("file-symlink-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    Ok(Value::bool(file_symlink_p(&filename)))
}

/// (file-name-case-insensitive-p FILENAME) -> t or nil
pub(crate) fn builtin_file_name_case_insensitive_p(args: Vec<Value>) -> EvalResult {
    expect_args("file-name-case-insensitive-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = expand_file_name(&filename, None);
    Ok(Value::bool(file_name_case_insensitive_p(&filename)))
}

/// Evaluator-aware variant of `file-name-case-insensitive-p` that resolves
/// relative paths against dynamic/default `default-directory`.
pub(crate) fn builtin_file_name_case_insensitive_p_eval(
    eval: &Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("file-name-case-insensitive-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let default_dir = default_directory_for_eval(eval);
    let filename = expand_file_name(&filename, default_dir.as_deref());
    Ok(Value::bool(file_name_case_insensitive_p(&filename)))
}

/// (file-newer-than-file-p FILE1 FILE2) -> t or nil
pub(crate) fn builtin_file_newer_than_file_p(args: Vec<Value>) -> EvalResult {
    expect_args("file-newer-than-file-p", &args, 2)?;
    let file1 = expect_string_strict(&args[0])?;
    let file2 = expect_string_strict(&args[1])?;
    let file1 = expand_file_name(&file1, None);
    let file2 = expand_file_name(&file2, None);
    Ok(Value::bool(file_newer_than_file_p(&file1, &file2)))
}

/// Evaluator-aware variant of `file-newer-than-file-p` that resolves
/// relative paths against dynamic/default `default-directory`.
pub(crate) fn builtin_file_newer_than_file_p_eval(
    eval: &Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("file-newer-than-file-p", &args, 2)?;
    let file1 = expect_string_strict(&args[0])?;
    let file2 = expect_string_strict(&args[1])?;
    let default_dir = default_directory_for_eval(eval);
    let file1 = expand_file_name(&file1, default_dir.as_deref());
    let file2 = expand_file_name(&file2, default_dir.as_deref());
    Ok(Value::bool(file_newer_than_file_p(&file1, &file2)))
}

/// (file-modes FILENAME &optional FLAG) -> integer or nil
pub(crate) fn builtin_file_modes(args: Vec<Value>) -> EvalResult {
    expect_min_args("file-modes", &args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("file-modes"), Value::Int(args.len() as i64)],
        ));
    }
    let filename = expect_string_strict(&args[0])?;
    match file_modes(&filename) {
        Some(mode) => Ok(Value::Int(mode as i64)),
        None => Ok(Value::Nil),
    }
}

/// Evaluator-aware variant of `file-modes` that resolves relative paths
/// against dynamic/default `default-directory`.
pub(crate) fn builtin_file_modes_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("file-modes", &args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("file-modes"), Value::Int(args.len() as i64)],
        ));
    }
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    match file_modes(&filename) {
        Some(mode) => Ok(Value::Int(mode as i64)),
        None => Ok(Value::Nil),
    }
}

/// (set-file-modes FILENAME MODE &optional FLAG) -> nil
pub(crate) fn builtin_set_file_modes(args: Vec<Value>) -> EvalResult {
    expect_min_args("set-file-modes", &args, 2)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("set-file-modes"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let filename = expect_string_strict(&args[0])?;
    let mode = expect_fixnum(&args[1])?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(mode as u32);
        fs::set_permissions(&filename, perms)
            .map_err(|err| signal_file_action_error(err, "Doing chmod", &filename))?;
    }
    #[cfg(not(unix))]
    {
        let mut perms = fs::metadata(&filename)
            .map_err(|err| signal_file_action_error(err, "Doing chmod", &filename))?
            .permissions();
        let writable = (mode & 0o222) != 0;
        perms.set_readonly(!writable);
        fs::set_permissions(&filename, perms)
            .map_err(|err| signal_file_action_error(err, "Doing chmod", &filename))?;
    }
    Ok(Value::Nil)
}

/// Evaluator-aware variant of `set-file-modes` that resolves relative paths
/// against dynamic/default `default-directory`.
pub(crate) fn builtin_set_file_modes_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("set-file-modes", &args, 2)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("set-file-modes"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    let mode = expect_fixnum(&args[1])?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(mode as u32);
        fs::set_permissions(&filename, perms)
            .map_err(|err| signal_file_action_error(err, "Doing chmod", &filename))?;
    }
    #[cfg(not(unix))]
    {
        let mut perms = fs::metadata(&filename)
            .map_err(|err| signal_file_action_error(err, "Doing chmod", &filename))?
            .permissions();
        let writable = (mode & 0o222) != 0;
        perms.set_readonly(!writable);
        fs::set_permissions(&filename, perms)
            .map_err(|err| signal_file_action_error(err, "Doing chmod", &filename))?;
    }
    Ok(Value::Nil)
}

/// (set-file-times FILENAME &optional TIMESTAMP FLAG) -> t
pub(crate) fn builtin_set_file_times(args: Vec<Value>) -> EvalResult {
    expect_min_args("set-file-times", &args, 1)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("set-file-times"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let filename = expect_string_strict(&args[0])?;
    let filename = expand_file_name(&filename, None);
    let timestamp = if args.len() > 1 && !args[1].is_nil() {
        Some(parse_timestamp_arg(&args[1])?)
    } else {
        None
    };
    // Emacs currently treats all non-nil values like `nofollow`.
    let nofollow = args.get(2).is_some_and(|flag| !flag.is_nil());
    set_file_times_compat(&filename, timestamp, nofollow)?;
    Ok(Value::True)
}

/// Evaluator-aware variant of `set-file-times` that resolves relative paths
/// against dynamic/default `default-directory`.
pub(crate) fn builtin_set_file_times_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("set-file-times", &args, 1)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("set-file-times"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let filename = expect_string_strict(&args[0])?;
    let default_dir = default_directory_for_eval(eval);
    let filename = expand_file_name(&filename, default_dir.as_deref());
    let timestamp = if args.len() > 1 && !args[1].is_nil() {
        Some(parse_timestamp_arg(&args[1])?)
    } else {
        None
    };
    // Emacs currently treats all non-nil values like `nofollow`.
    let nofollow = args.get(2).is_some_and(|flag| !flag.is_nil());
    set_file_times_compat(&filename, timestamp, nofollow)?;
    Ok(Value::True)
}

fn validate_optional_buffer_arg(eval: &Evaluator, arg: Option<&Value>) -> Result<(), Flow> {
    if let Some(bufferish) = arg {
        match bufferish {
            Value::Nil => Ok(()),
            Value::Buffer(id) => {
                if eval.buffers.get(*id).is_some() {
                    Ok(())
                } else {
                    Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("bufferp"), *bufferish],
                    ))
                }
            }
            _ => Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("bufferp"), *bufferish],
            )),
        }?
    }
    Ok(())
}

fn validate_set_visited_file_modtime_arg(arg: &Value) -> Result<(), Flow> {
    match arg {
        Value::Int(_) | Value::Char(_) => Err(signal(
            "args-out-of-range",
            vec![*arg, Value::Int(-1), Value::Int(0)],
        )),
        Value::Str(_) => Err(signal(
            "error",
            vec![Value::string("Invalid time specification")],
        )),
        Value::Float(_, _) | Value::Cons(_) => Ok(()),
        _ => Err(signal(
            "error",
            vec![Value::string("Invalid time specification")],
        )),
    }
}

/// (visited-file-modtime) -> 0
pub(crate) fn builtin_visited_file_modtime(args: Vec<Value>) -> EvalResult {
    expect_args("visited-file-modtime", &args, 0)?;
    Ok(Value::Int(0))
}

/// (verify-visited-file-modtime &optional BUFFER) -> t
pub(crate) fn builtin_verify_visited_file_modtime(
    eval: &mut Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("verify-visited-file-modtime", &args, 1)?;
    validate_optional_buffer_arg(eval, args.first())?;
    Ok(Value::True)
}

/// (set-visited-file-modtime &optional TIME-LIST) -> nil
pub(crate) fn builtin_set_visited_file_modtime(
    eval: &mut Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("set-visited-file-modtime", &args, 1)?;
    if let Some(arg) = args.first() {
        if !arg.is_nil() {
            validate_set_visited_file_modtime_arg(arg)?;
            return Ok(Value::Nil);
        }
    }

    let file_name = eval
        .buffers
        .current_buffer()
        .and_then(|buf| buf.file_name.clone());
    if file_name.is_none() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), Value::Nil],
        ));
    }

    Ok(Value::Nil)
}

/// (set-default-file-modes MODE) -> nil
pub(crate) fn builtin_set_default_file_modes(args: Vec<Value>) -> EvalResult {
    expect_args("set-default-file-modes", &args, 1)?;
    init_default_file_mode_mask();
    let mode = expect_fixnum(&args[0])?;
    let new_mask = (!mode) & 0o777;
    #[cfg(unix)]
    unsafe {
        libc::umask(new_mask as libc::mode_t);
    }
    DEFAULT_FILE_MODE_MASK.store(new_mask as u32, Ordering::Relaxed);
    Ok(Value::Nil)
}

/// (default-file-modes) -> integer
pub(crate) fn builtin_default_file_modes(args: Vec<Value>) -> EvalResult {
    expect_args("default-file-modes", &args, 0)?;
    init_default_file_mode_mask();
    let mask = DEFAULT_FILE_MODE_MASK.load(Ordering::Relaxed) as i64;
    Ok(Value::Int((!mask) & 0o777))
}

/// (delete-file FILENAME &optional TRASH) -> nil
pub(crate) fn builtin_delete_file(args: Vec<Value>) -> EvalResult {
    expect_min_args("delete-file", &args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("delete-file"), Value::Int(args.len() as i64)],
        ));
    }
    let filename = expect_string_strict(&args[0])?;
    delete_file_compat(&filename)?;
    Ok(Value::Nil)
}

/// Evaluator-aware variant of `delete-file` that resolves relative paths
/// against dynamic/default `default-directory`.
pub(crate) fn builtin_delete_file_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("delete-file", &args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("delete-file"), Value::Int(args.len() as i64)],
        ));
    }
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    delete_file_compat(&filename)?;
    Ok(Value::Nil)
}

/// (delete-file-internal FILENAME) -> nil
pub(crate) fn builtin_delete_file_internal(args: Vec<Value>) -> EvalResult {
    expect_args("delete-file-internal", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    delete_file_compat(&filename)?;
    Ok(Value::Nil)
}

/// Evaluator-aware variant of `delete-file-internal` that resolves relative
/// paths against dynamic/default `default-directory`.
pub(crate) fn builtin_delete_file_internal_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("delete-file-internal", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    delete_file_compat(&filename)?;
    Ok(Value::Nil)
}

/// (delete-directory DIRECTORY &optional RECURSIVE TRASH) -> nil
pub(crate) fn builtin_delete_directory(args: Vec<Value>) -> EvalResult {
    expect_min_args("delete-directory", &args, 1)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("delete-directory"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let directory = expect_string_strict(&args[0])?;
    let recursive = args.get(1).is_some_and(|value| value.is_truthy());
    let result = if recursive {
        fs::remove_dir_all(&directory)
    } else {
        fs::remove_dir(&directory)
    };
    result.map_err(|err| signal_file_io_path(err, "Removing directory", &directory))?;
    Ok(Value::Nil)
}

/// (delete-directory-internal DIRECTORY) -> nil
pub(crate) fn builtin_delete_directory_internal(args: Vec<Value>) -> EvalResult {
    expect_args("delete-directory-internal", &args, 1)?;
    let directory = expect_string_strict(&args[0])?;
    fs::remove_dir(&directory)
        .map_err(|err| signal_file_io_path(err, "Removing directory", &directory))?;
    Ok(Value::Nil)
}

/// Evaluator-aware variant of `delete-directory-internal` that resolves
/// relative paths against dynamic/default `default-directory`.
pub(crate) fn builtin_delete_directory_internal_eval(
    eval: &Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("delete-directory-internal", &args, 1)?;
    let directory = expect_string_strict(&args[0])?;
    let directory = resolve_filename_for_eval(eval, &directory);
    fs::remove_dir(&directory)
        .map_err(|err| signal_file_io_path(err, "Removing directory", &directory))?;
    Ok(Value::Nil)
}

/// Evaluator-aware variant of `delete-directory` that resolves relative paths
/// against dynamic/default `default-directory`.
pub(crate) fn builtin_delete_directory_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("delete-directory", &args, 1)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("delete-directory"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let directory = expect_string_strict(&args[0])?;
    let directory = resolve_filename_for_eval(eval, &directory);
    let recursive = args.get(1).is_some_and(|value| value.is_truthy());
    let result = if recursive {
        fs::remove_dir_all(&directory)
    } else {
        fs::remove_dir(&directory)
    };
    result.map_err(|err| signal_file_io_path(err, "Removing directory", &directory))?;
    Ok(Value::Nil)
}

/// (make-symbolic-link TARGET LINKNAME &optional OK-IF-ALREADY-EXISTS) -> nil
pub(crate) fn builtin_make_symbolic_link(args: Vec<Value>) -> EvalResult {
    expect_min_args("make-symbolic-link", &args, 2)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("make-symbolic-link"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let target = expect_string_strict(&args[0])?;
    let linkname = expect_string_strict(&args[1])?;
    let ok_if_exists = args.get(2).is_some_and(|value| value.is_truthy());

    #[cfg(unix)]
    {
        if ok_if_exists && fs::symlink_metadata(&linkname).is_ok() {
            fs::remove_file(&linkname)
                .map_err(|err| signal_file_io_path(err, "Removing old name", &linkname))?;
        }
        std::os::unix::fs::symlink(&target, &linkname)
            .map_err(|err| signal_file_io_path(err, "Making symbolic link", &linkname))?;
        Ok(Value::Nil)
    }

    #[cfg(not(unix))]
    {
        let _ = (target, linkname, ok_if_exists);
        Err(signal(
            "file-error",
            vec![Value::string(
                "Symbolic links are unsupported on this platform",
            )],
        ))
    }
}

/// Evaluator-aware variant of `make-symbolic-link` that resolves relative
/// target/link paths against dynamic/default `default-directory`.
pub(crate) fn builtin_make_symbolic_link_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("make-symbolic-link", &args, 2)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("make-symbolic-link"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let target = resolve_filename_for_eval(eval, &expect_string_strict(&args[0])?);
    let linkname = resolve_filename_for_eval(eval, &expect_string_strict(&args[1])?);
    let ok_if_exists = args.get(2).is_some_and(|value| value.is_truthy());

    #[cfg(unix)]
    {
        if ok_if_exists && fs::symlink_metadata(&linkname).is_ok() {
            fs::remove_file(&linkname)
                .map_err(|err| signal_file_io_path(err, "Removing old name", &linkname))?;
        }
        std::os::unix::fs::symlink(&target, &linkname)
            .map_err(|err| signal_file_io_path(err, "Making symbolic link", &linkname))?;
        Ok(Value::Nil)
    }

    #[cfg(not(unix))]
    {
        let _ = (target, linkname, ok_if_exists);
        Err(signal(
            "file-error",
            vec![Value::string(
                "Symbolic links are unsupported on this platform",
            )],
        ))
    }
}

/// (rename-file FROM TO) -> nil
pub(crate) fn builtin_rename_file(args: Vec<Value>) -> EvalResult {
    expect_min_args("rename-file", &args, 2)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("rename-file"), Value::Int(args.len() as i64)],
        ));
    }
    let from = expect_string_strict(&args[0])?;
    let to = expect_string_strict(&args[1])?;
    let ok_if_exists = args.get(2).is_some_and(|value| value.is_truthy());
    if fs::symlink_metadata(&to).is_ok() {
        if ok_if_exists {
            fs::remove_file(&to).map_err(|e| signal_file_io_path(e, "Removing old name", &to))?;
        } else {
            return Err(signal(
                "file-already-exists",
                vec![
                    Value::string("Renaming"),
                    Value::string(format!("File exists: {to}")),
                    Value::string(&from),
                    Value::string(&to),
                ],
            ));
        }
    }
    rename_file(&from, &to).map_err(|e| signal_file_io_paths(e, "Renaming", &from, &to))?;
    Ok(Value::Nil)
}

/// Evaluator-aware variant of `rename-file` that resolves relative paths
/// against dynamic/default `default-directory`.
pub(crate) fn builtin_rename_file_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("rename-file", &args, 2)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("rename-file"), Value::Int(args.len() as i64)],
        ));
    }
    let from = resolve_filename_for_eval(eval, &expect_string_strict(&args[0])?);
    let to = resolve_filename_for_eval(eval, &expect_string_strict(&args[1])?);
    let ok_if_exists = args.get(2).is_some_and(|value| value.is_truthy());
    if fs::symlink_metadata(&to).is_ok() {
        if ok_if_exists {
            fs::remove_file(&to).map_err(|e| signal_file_io_path(e, "Removing old name", &to))?;
        } else {
            return Err(signal(
                "file-already-exists",
                vec![
                    Value::string("Renaming"),
                    Value::string(format!("File exists: {to}")),
                    Value::string(&from),
                    Value::string(&to),
                ],
            ));
        }
    }
    rename_file(&from, &to).map_err(|e| signal_file_io_paths(e, "Renaming", &from, &to))?;
    Ok(Value::Nil)
}

/// (copy-file FROM TO) -> nil
pub(crate) fn builtin_copy_file(args: Vec<Value>) -> EvalResult {
    expect_min_args("copy-file", &args, 2)?;
    if args.len() > 6 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("copy-file"), Value::Int(args.len() as i64)],
        ));
    }
    let from = expect_string_strict(&args[0])?;
    let to = expect_string_strict(&args[1])?;
    let ok_if_exists = args.get(2).is_some_and(|value| value.is_truthy());
    if fs::symlink_metadata(&to).is_ok() && !ok_if_exists {
        return Err(signal(
            "file-already-exists",
            vec![
                Value::string("Copying"),
                Value::string(format!("File exists: {to}")),
                Value::string(&from),
                Value::string(&to),
            ],
        ));
    }
    copy_file(&from, &to).map_err(|e| signal_file_io_paths(e, "Copying", &from, &to))?;
    Ok(Value::Nil)
}

/// Evaluator-aware variant of `copy-file` that resolves relative paths
/// against dynamic/default `default-directory`.
pub(crate) fn builtin_copy_file_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("copy-file", &args, 2)?;
    if args.len() > 6 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("copy-file"), Value::Int(args.len() as i64)],
        ));
    }
    let from = resolve_filename_for_eval(eval, &expect_string_strict(&args[0])?);
    let to = resolve_filename_for_eval(eval, &expect_string_strict(&args[1])?);
    let ok_if_exists = args.get(2).is_some_and(|value| value.is_truthy());
    if fs::symlink_metadata(&to).is_ok() && !ok_if_exists {
        return Err(signal(
            "file-already-exists",
            vec![
                Value::string("Copying"),
                Value::string(format!("File exists: {to}")),
                Value::string(&from),
                Value::string(&to),
            ],
        ));
    }
    copy_file(&from, &to).map_err(|e| signal_file_io_paths(e, "Copying", &from, &to))?;
    Ok(Value::Nil)
}

/// (add-name-to-file OLDNAME NEWNAME &optional OK-IF-ALREADY-EXISTS) -> nil
pub(crate) fn builtin_add_name_to_file(args: Vec<Value>) -> EvalResult {
    expect_min_args("add-name-to-file", &args, 2)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("add-name-to-file"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let oldname = expect_string_strict(&args[0])?;
    let newname = expect_string_strict(&args[1])?;
    let ok_if_exists = args.get(2).is_some_and(|value| value.is_truthy());
    if ok_if_exists && fs::symlink_metadata(&newname).is_ok() {
        fs::remove_file(&newname)
            .map_err(|err| signal_file_io_path(err, "Removing old name", &newname))?;
    }
    add_name_to_file(&oldname, &newname)
        .map_err(|err| signal_file_io_paths(err, "Adding new name", &oldname, &newname))?;
    Ok(Value::Nil)
}

/// Evaluator-aware variant of `add-name-to-file` that resolves relative paths
/// against dynamic/default `default-directory`.
pub(crate) fn builtin_add_name_to_file_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("add-name-to-file", &args, 2)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("add-name-to-file"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let oldname = resolve_filename_for_eval(eval, &expect_string_strict(&args[0])?);
    let newname = resolve_filename_for_eval(eval, &expect_string_strict(&args[1])?);
    let ok_if_exists = args.get(2).is_some_and(|value| value.is_truthy());
    if ok_if_exists && fs::symlink_metadata(&newname).is_ok() {
        fs::remove_file(&newname)
            .map_err(|err| signal_file_io_path(err, "Removing old name", &newname))?;
    }
    add_name_to_file(&oldname, &newname)
        .map_err(|err| signal_file_io_paths(err, "Adding new name", &oldname, &newname))?;
    Ok(Value::Nil)
}

/// (make-directory-internal DIR) -> nil
pub(crate) fn builtin_make_directory_internal(args: Vec<Value>) -> EvalResult {
    expect_args("make-directory-internal", &args, 1)?;
    let dir = expect_string_strict(&args[0])?;
    make_directory(&dir, false).map_err(|e| signal_file_io_path(e, "Creating directory", &dir))?;
    Ok(Value::Nil)
}

/// Evaluator-aware variant of `make-directory-internal` that resolves relative
/// paths against dynamic/default `default-directory`.
pub(crate) fn builtin_make_directory_internal_eval(
    eval: &Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("make-directory-internal", &args, 1)?;
    let dir = expect_string_strict(&args[0])?;
    let dir = resolve_filename_for_eval(eval, &dir);
    make_directory(&dir, false).map_err(|e| signal_file_io_path(e, "Creating directory", &dir))?;
    Ok(Value::Nil)
}

/// (find-file-name-handler FILENAME OPERATION) -> handler or nil
pub(crate) fn builtin_find_file_name_handler(args: Vec<Value>) -> EvalResult {
    expect_args("find-file-name-handler", &args, 2)?;
    let _filename = expect_string_strict(&args[0])?;
    let _operation = &args[1];
    Ok(Value::Nil)
}

/// Evaluator-aware variant of `find-file-name-handler`.
pub(crate) fn builtin_find_file_name_handler_eval(
    eval: &Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("find-file-name-handler", &args, 2)?;
    let filename = expect_string_strict(&args[0])?;
    let _filename = resolve_filename_for_eval(eval, &filename);
    let _operation = &args[1];
    Ok(Value::Nil)
}

/// (directory-files DIR &optional FULL MATCH NOSORT COUNT) -> list of strings
pub(crate) fn builtin_directory_files(args: Vec<Value>) -> EvalResult {
    expect_min_args("directory-files", &args, 1)?;
    if args.len() > 5 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("directory-files"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let dir = expect_string_strict(&args[0])?;
    let full = args.get(1).is_some_and(|v| v.is_truthy());
    let match_pattern = if let Some(val) = args.get(2) {
        if val.is_truthy() {
            Some(expect_string_strict(val)?)
        } else {
            None
        }
    } else {
        None
    };
    let nosort = args.get(3).is_some_and(|v| v.is_truthy());
    let count = if let Some(val) = args.get(4) {
        match val {
            Value::Int(n) if *n >= 0 => Some(*n as usize),
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("natnump"), *other],
                ));
            }
        }
    } else {
        None
    };

    let files = directory_files(&dir, full, match_pattern.as_deref(), nosort, count)
        .map_err(|e| signal_directory_files_error(e, &dir))?;
    Ok(Value::list(files.into_iter().map(Value::string).collect()))
}

/// Evaluator-aware variant of `directory-files` that resolves relative DIR
/// against dynamic/default `default-directory`.
pub(crate) fn builtin_directory_files_eval(eval: &Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("directory-files", &args, 1)?;
    if args.len() > 5 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("directory-files"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let dir = resolve_filename_for_eval(eval, &expect_string_strict(&args[0])?);
    let full = args.get(1).is_some_and(|v| v.is_truthy());
    let match_pattern = if let Some(val) = args.get(2) {
        if val.is_truthy() {
            Some(expect_string_strict(val)?)
        } else {
            None
        }
    } else {
        None
    };
    let nosort = args.get(3).is_some_and(|v| v.is_truthy());
    let count = if let Some(val) = args.get(4) {
        match val {
            Value::Int(n) if *n >= 0 => Some(*n as usize),
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("natnump"), *other],
                ));
            }
        }
    } else {
        None
    };

    let files = directory_files(&dir, full, match_pattern.as_deref(), nosort, count)
        .map_err(|e| signal_directory_files_error(e, &dir))?;
    Ok(Value::list(files.into_iter().map(Value::string).collect()))
}

// ===========================================================================
// Evaluator-dependent builtins
// ===========================================================================

fn expect_int(value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *other],
        )),
    }
}

fn expect_file_offset(value: &Value) -> Result<i64, Flow> {
    let offset = expect_int(value)?;
    if offset < 0 {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("file-offset"), *value],
        ));
    }
    Ok(offset)
}

/// (insert-file-contents FILENAME &optional VISIT BEG END REPLACE) -> (FILENAME LENGTH)
///
/// Read file FILENAME and insert its contents into the current buffer at point.
/// Returns a list of the absolute filename and the number of characters inserted.
pub(crate) fn builtin_insert_file_contents(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("insert-file-contents", &args, 1)?;
    expect_max_args("insert-file-contents", &args, 5)?;
    let filename = expect_string(&args[0])?;
    let resolved = resolve_filename_for_eval(eval, &filename);
    let visit = args.get(1).is_some_and(|v| v.is_truthy());

    // Read file contents
    let contents_bytes =
        fs::read(&resolved).map_err(|e| signal_file_io_path(e, "Opening input file", &resolved))?;
    let file_len = contents_bytes.len() as i64;

    let begin = if args.get(2).is_some_and(|v| !v.is_nil()) {
        expect_file_offset(args.get(2).expect("checked above"))?
    } else {
        0
    };
    let mut end = if args.get(3).is_some_and(|v| !v.is_nil()) {
        expect_file_offset(args.get(3).expect("checked above"))?
    } else {
        file_len
    };

    if begin > file_len {
        return Err(signal(
            "file-error",
            vec![
                Value::string("Read error"),
                Value::string("Bad address"),
                Value::string(resolved),
            ],
        ));
    }
    if end > file_len {
        end = file_len;
    }
    if end < begin {
        end = begin;
    }

    let slice = &contents_bytes[begin as usize..end as usize];
    let contents = String::from_utf8_lossy(slice).to_string();

    let char_count = contents.chars().count() as i64;

    // Insert into current buffer
    let buf = eval
        .buffers
        .current_buffer_mut()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    buf.insert(&contents);

    if visit {
        buf.file_name = Some(resolved.clone());
        buf.set_modified(false);
    }

    Ok(Value::list(vec![
        Value::string(resolved),
        Value::Int(char_count),
    ]))
}

/// (write-region START END FILENAME &optional APPEND VISIT) -> nil
///
/// Write the region between START and END to FILENAME.
/// If START is nil, writes the entire buffer.
pub(crate) fn builtin_write_region(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("write-region", &args, 3)?;
    expect_max_args("write-region", &args, 7)?;
    let filename = expect_string(&args[2])?;
    let resolved = resolve_filename_for_eval(eval, &filename);
    let append = args.get(3).is_some_and(|v| v.is_truthy());
    let visit = args.get(4).is_some_and(|v| v.is_truthy());

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    // Extract the text region
    let content = if args[0].is_nil() && args[1].is_nil() {
        // Write entire buffer
        buf.buffer_string()
    } else {
        let start = expect_int(&args[0])?;
        let end = expect_int(&args[1])?;
        let point_min = buf.text.byte_to_char(buf.point_min()) as i64 + 1;
        let point_max = buf.text.byte_to_char(buf.point_max()) as i64 + 1;
        if start < point_min || start > point_max || end < point_min || end > point_max {
            return Err(signal(
                "args-out-of-range",
                vec![Value::Buffer(buf.id), Value::Int(start), Value::Int(end)],
            ));
        }
        let (char_start, char_end) = if start <= end {
            (start as usize - 1, end as usize - 1)
        } else {
            (end as usize - 1, start as usize - 1)
        };
        let byte_start = buf.text.char_to_byte(char_start.min(buf.text.char_count()));
        let byte_end = buf.text.char_to_byte(char_end.min(buf.text.char_count()));
        buf.buffer_substring(byte_start, byte_end)
    };

    write_string_to_file(&content, &resolved, append)
        .map_err(|e| signal_file_io_path(e, "Writing to", &resolved))?;

    if visit {
        // Need mutable access to set file_name and modified flag
        let buf_mut = eval
            .buffers
            .current_buffer_mut()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        buf_mut.file_name = Some(resolved);
        buf_mut.set_modified(false);
    }

    Ok(Value::Nil)
}

/// (find-file-noselect FILENAME &optional NOWARN RAWFILE) -> buffer
///
/// Read file FILENAME into a buffer and return the buffer.
/// If a buffer visiting FILENAME already exists, return it.
/// Does not select the buffer.
pub(crate) fn builtin_find_file_noselect(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("find-file-noselect", &args, 1)?;
    expect_max_args("find-file-noselect", &args, 4)?;
    let filename = expect_string(&args[0])?;
    let abs_path = resolve_filename_for_eval(eval, &filename);

    // Check if there's already a buffer visiting this file
    for buf_id in eval.buffers.buffer_list() {
        if let Some(buf) = eval.buffers.get(buf_id) {
            if buf.file_name.as_deref() == Some(&abs_path) {
                return Ok(Value::Buffer(buf_id));
            }
        }
    }

    // Derive buffer name from file name
    let buf_name = file_name_nondirectory(&abs_path);
    let unique_name = eval.buffers.generate_new_buffer_name(&buf_name);
    let buf_id = eval.buffers.create_buffer(&unique_name);

    // If the file exists, read its contents into the new buffer
    if file_exists_p(&abs_path) {
        let contents = read_file_contents(&abs_path)
            .map_err(|e| signal_file_io_path(e, "Opening input file", &abs_path))?;

        // Save and restore current buffer around the insert
        let saved_current = eval
            .buffers
            .buffer_list()
            .into_iter()
            .find(|&id| eval.buffers.current_buffer().is_some_and(|b| b.id == id));

        eval.buffers.set_current(buf_id);
        if let Some(buf) = eval.buffers.get_mut(buf_id) {
            buf.insert(&contents);
            // Move point to the beginning
            buf.goto_char(0);
            buf.file_name = Some(abs_path);
            buf.set_modified(false);
        }

        // Restore the previous current buffer
        if let Some(prev_id) = saved_current {
            eval.buffers.set_current(prev_id);
        }
    } else {
        // File doesn't exist — create an empty buffer with the file name set
        if let Some(buf) = eval.buffers.get_mut(buf_id) {
            buf.file_name = Some(abs_path);
        }
    }

    Ok(Value::Buffer(buf_id))
}

// ===========================================================================
// Bootstrap variables
// ===========================================================================

pub fn register_bootstrap_vars(obarray: &mut crate::emacs_core::symbol::Obarray) {
    let temporary_file_directory = std::env::temp_dir().to_string_lossy().to_string();
    obarray.set_symbol_value("file-name-coding-system", Value::Nil);
    obarray.set_symbol_value("default-file-name-coding-system", Value::Nil);
    obarray.set_symbol_value("file-name-handler-alist", Value::Nil);
    obarray.set_symbol_value("set-auto-coding-function", Value::Nil);
    obarray.set_symbol_value("after-insert-file-functions", Value::Nil);
    obarray.set_symbol_value("write-region-annotate-functions", Value::Nil);
    obarray.set_symbol_value("write-region-post-annotation-function", Value::Nil);
    obarray.set_symbol_value("write-region-annotations-so-far", Value::Nil);
    obarray.set_symbol_value("inhibit-file-name-handlers", Value::Nil);
    obarray.set_symbol_value("inhibit-file-name-operation", Value::Nil);
    obarray.set_symbol_value("directory-abbrev-alist", Value::Nil);
    obarray.set_symbol_value("auto-save-list-file-name", Value::Nil);
    obarray.set_symbol_value("auto-save-list-file-prefix", Value::Nil);
    obarray.set_symbol_value("auto-save-visited-file-name", Value::Nil);
    obarray.set_symbol_value("auto-save-include-big-deletions", Value::Nil);
    obarray.set_symbol_value("write-region-inhibit-fsync", Value::Nil);
    obarray.set_symbol_value("delete-by-moving-to-trash", Value::Nil);
    obarray.set_symbol_value("auto-save-file-name-transforms", Value::Nil);
    obarray.set_symbol_value(
        "temporary-file-directory",
        Value::string(temporary_file_directory),
    );
    obarray.set_symbol_value("create-lockfiles", Value::True);
    // files.el: defvar for vc-hooks.el and locate-dominating-file
    obarray.set_symbol_value(
        "locate-dominating-stop-dir-regexp",
        Value::string(r"\`\(?:[\\/][\\/][^\\/]+[\\/]\|/\(?:net\|afs\|\.\.\.\)/\)\'"),
    );
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
#[path = "fileio_test.rs"]
mod tests;
