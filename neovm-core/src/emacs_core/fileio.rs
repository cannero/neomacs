//! File I/O primitives for the Elisp VM.
//!
//! Provides path manipulation, file predicates, read/write operations,
//! directory operations, and file attribute queries.

use std::collections::VecDeque;
#[cfg(unix)]
use std::ffi::{CStr, CString};
use std::fs;
use std::io::{ErrorKind, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::error::{EvalResult, Flow, signal};
use super::eval::Context;
use super::intern::{intern, resolve_sym};
use super::symbol::Obarray;
use super::value::{OrderedRuntimeBindingMap, Value, ValueKind, VecLikeType, list_to_vec};

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
/// Return true if FILENAME is a symbolic link. Used by predicates that
/// only need a yes/no answer (e.g. internal helpers); the public
/// `file-symlink-p` builtin returns the link target instead.
pub fn file_symlink_p(filename: &str) -> bool {
    match fs::symlink_metadata(filename) {
        Ok(meta) => meta.file_type().is_symlink(),
        Err(_) => false,
    }
}

/// Return the symbolic link target of FILENAME as a String, mirroring
/// GNU Emacs `Ffile_symlink_p` (`fileio.c:3160`):
///
///   "Return non-nil if file FILENAME is the name of a symbolic link.
///    The value is the link target, as a string.
///    Return nil if FILENAME does not exist or is not a symbolic link,
///    or there was trouble determining whether the file is a symbolic link.
///    This function does not check whether the link target exists."
///
/// The returned target is whatever the OS `readlink` syscall produces;
/// it may be relative.  We do NOT canonicalize it (GNU's
/// `emacs_readlinkat` likewise does not).
pub fn file_symlink_target(filename: &str) -> Option<String> {
    let meta = fs::symlink_metadata(filename).ok()?;
    if !meta.file_type().is_symlink() {
        return None;
    }
    fs::read_link(filename)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
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
    let mode = if append {
        FileWriteMode::Append
    } else {
        FileWriteMode::Truncate
    };
    let file = write_bytes_to_file_with_mode(content.as_bytes(), filename, mode)?;
    drop(file);
    Ok(())
}

enum FileWriteMode {
    Truncate,
    Append,
    Seek(u64),
}

/// Write raw bytes to a file, returning the open `File` handle so the caller
/// can optionally `sync_all()` before the handle is dropped.
fn write_bytes_to_file_with_mode(
    content: &[u8],
    filename: &str,
    mode: FileWriteMode,
) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true);
    match mode {
        FileWriteMode::Truncate => {
            options.truncate(true);
        }
        FileWriteMode::Append => {
            options.append(true);
        }
        FileWriteMode::Seek(_) => {}
    }
    let mut file = options.open(filename)?;
    if let FileWriteMode::Seek(offset) = mode {
        file.seek(SeekFrom::Start(offset))?;
    }
    file.write_all(content)?;
    Ok(file)
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
        if let Some(pattern) = match_regex {
            let mut throwaway = None;
            let matched = super::regex::string_match_full_with_case_fold(
                pattern,
                &name,
                0,
                false,
                &mut throwaway,
            )
            .map_err(|msg| {
                DirectoryFilesError::InvalidRegexp(format!(
                    "Invalid regexp \"{}\": {}",
                    pattern, msg
                ))
            })?;
            if matched.is_none() {
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
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

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

fn expect_string(value: &Value) -> Result<String, Flow> {
    match value.kind() {
        ValueKind::String => Ok(super::builtins::lisp_string_to_runtime_string(*value)),
        ValueKind::Symbol(id) => Ok(resolve_sym(id).to_owned()),
        ValueKind::Nil => Ok("nil".to_string()),
        ValueKind::T => Ok("t".to_string()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *value],
        )),
    }
}

fn expect_string_strict(value: &Value) -> Result<String, Flow> {
    match value.kind() {
        ValueKind::String => Ok(super::builtins::lisp_string_to_runtime_string(*value)),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *value],
        )),
    }
}

fn expect_temp_prefix(value: &Value) -> Result<String, Flow> {
    match value.kind() {
        ValueKind::String => Ok(super::builtins::lisp_string_to_runtime_string(*value)),
        ValueKind::Nil | ValueKind::Cons | ValueKind::Veclike(VecLikeType::Vector) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *value],
        )),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), *value],
        )),
    }
}

fn expect_fixnum(value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("fixnump"), *value],
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
    match value.kind() {
        ValueKind::Fixnum(n) => Ok((n, 0)),
        ValueKind::Float => {
            let f = value.as_float().unwrap();
            let secs = f.floor() as i64;
            let nanos = ((f - f.floor()) * 1_000_000_000.0).round() as i64;
            Ok(normalize_secs_nanos(secs, nanos))
        }
        ValueKind::Cons => {
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
            vec![Value::symbol("numberp"), *value],
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
    if counter.is_cons() {
        let first = counter.cons_car();
        // Mirrors GNU `NUMBERP` which accepts bignums in addition
        // to fixnums and floats.
        if !(first.is_number() || first.as_char().is_some()) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("number-or-marker-p"), first],
            ));
        }
    }
    Ok(())
}

fn temporary_file_directory_for_eval(eval: &Context) -> Option<String> {
    let val = eval.obarray.symbol_value("temporary-file-directory")?;
    val.is_string()
        .then(|| super::builtins::lisp_string_to_runtime_string(*val))
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

/// `(expand-file-name NAME &optional DEFAULT-DIRECTORY)` — falls back
/// to dynamic `default-directory` when DEFAULT-DIRECTORY is omitted
/// or nil.
pub(crate) fn builtin_expand_file_name(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "expand-file-name", &args)? {
        return Ok(result);
    }
    expect_min_args("expand-file-name", &args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("expand-file-name"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }
    let name = expect_string_strict(&args[0])?;
    let default_dir = if let Some(arg) = args.get(1) {
        match arg.kind() {
            ValueKind::Nil => default_directory_in_state(&eval.obarray, &[], &eval.buffers),
            ValueKind::String => Some(super::builtins::lisp_string_to_runtime_string(*arg)),
            _ => Some("/".to_string()),
        }
    } else {
        default_directory_in_state(&eval.obarray, &[], &eval.buffers)
    };

    let result = expand_file_name(&name, default_dir.as_deref());
    // Preserve the multibyte flag of the input: if the input name was
    // unibyte (or the result is pure ASCII), return unibyte. This
    // matches GNU Emacs where expand-file-name preserves the encoding
    // and avoids "default-directory must be unibyte" errors during dump.
    let input_multibyte = args[0].string_is_multibyte();
    if input_multibyte {
        Ok(Value::multibyte_string(result))
    } else {
        Ok(Value::unibyte_string(result))
    }
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
    Ok(Value::NIL)
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
    Ok(Value::NIL)
}

/// Context-aware variant of `make-temp-file` that honors dynamic
/// `temporary-file-directory`.
pub(crate) fn builtin_make_temp_file(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("make-temp-file", &args, 1)?;
    if args.len() > 4 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("make-temp-file"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    let prefix = expect_temp_prefix(&args[0])?;
    let dir_flag = args.get(1).is_some_and(|value| value.is_truthy());
    let suffix = match args.get(2) {
        None => String::new(),
        Some(v) if v.is_nil() => String::new(),
        Some(value) => expect_string_strict(value)?,
    };
    let text = match args.get(3) {
        None => None,
        Some(v) if v.is_nil() => None,
        Some(v) if v.is_string() => Some(super::builtins::lisp_string_to_runtime_string(*v)),
        Some(_) => None,
    };
    let temp_dir = temporary_file_directory_for_eval(eval)
        .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned());

    let path = make_temp_file_impl(&temp_dir, &prefix, dir_flag, &suffix, text.as_deref())?;
    Ok(Value::string(path))
}

/// Context-aware variant of `make-nearby-temp-file` that resolves relative
/// directory-containing prefixes against dynamic/default `default-directory`
/// and honors dynamic `temporary-file-directory` fallback.
pub(crate) fn builtin_make_nearby_temp_file(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("make-nearby-temp-file", &args, 1)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("make-nearby-temp-file"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    let prefix = expect_temp_prefix(&args[0])?;
    let dir_flag = args.get(1).is_some_and(|value| value.is_truthy());
    let suffix = match args.get(2) {
        None => String::new(),
        Some(v) if v.is_nil() => String::new(),
        Some(value) => expect_string_strict(value)?,
    };
    let fallback_temp_dir = temporary_file_directory_for_eval(eval)
        .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned());
    let (temp_dir, file_prefix) =
        split_nearby_temp_prefix(&prefix).unwrap_or_else(|| (fallback_temp_dir, prefix.clone()));

    let path = make_temp_file_impl(&temp_dir, &file_prefix, dir_flag, &suffix, None)?;
    Ok(Value::string(path))
}

/// `(file-truename FILENAME)` — resolves FILENAME against
/// dynamic/default `default-directory` and follows symlinks.
pub(crate) fn builtin_file_truename(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "file-truename", &args)? {
        return Ok(result);
    }
    expect_min_args("file-truename", &args, 1)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("file-truename"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    let filename = expect_string_strict(&args[0])?;
    if let Some(counter) = args.get(1) {
        validate_file_truename_counter(counter)?;
    }

    Ok(Value::string(file_truename(
        &filename,
        default_directory_in_state(&eval.obarray, &[], &eval.buffers).as_deref(),
    )))
}

/// (file-name-directory FILENAME) -> string or nil
pub(crate) fn builtin_file_name_directory(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "file-name-directory", &args)? {
        return Ok(result);
    }
    expect_args("file-name-directory", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    match file_name_directory(&filename) {
        Some(dir) => Ok(Value::string(dir)),
        None => Ok(Value::NIL),
    }
}

/// (file-name-nondirectory FILENAME) -> string
pub(crate) fn builtin_file_name_nondirectory(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "file-name-nondirectory", &args)? {
        return Ok(result);
    }
    expect_args("file-name-nondirectory", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    Ok(Value::string(file_name_nondirectory(&filename)))
}

/// (file-name-as-directory FILENAME) -> string
pub(crate) fn builtin_file_name_as_directory(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "file-name-as-directory", &args)? {
        return Ok(result);
    }
    expect_args("file-name-as-directory", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    // Preserve multibyte flag of input
    if args[0].string_is_multibyte() {
        Ok(Value::multibyte_string(file_name_as_directory(&filename)))
    } else {
        Ok(Value::unibyte_string(file_name_as_directory(&filename)))
    }
}

/// (directory-file-name FILENAME) -> string
pub(crate) fn builtin_directory_file_name(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "directory-file-name", &args)? {
        return Ok(result);
    }
    expect_args("directory-file-name", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    if args[0].string_is_multibyte() {
        Ok(Value::multibyte_string(directory_file_name(&filename)))
    } else {
        Ok(Value::unibyte_string(directory_file_name(&filename)))
    }
}

/// (file-name-concat DIRECTORY &rest COMPONENTS) -> string
pub(crate) fn builtin_file_name_concat(args: Vec<Value>) -> EvalResult {
    expect_min_args("file-name-concat", &args, 1)?;

    let mut parts = Vec::new();
    for value in args {
        match value.kind() {
            ValueKind::Nil => {}
            ValueKind::String => {
                let s = super::builtins::lisp_string_to_runtime_string(value);
                if !s.is_empty() {
                    parts.push(s);
                }
            }
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), value],
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
    Ok(Value::bool_val(file_name_absolute_p(&filename)))
}

/// (directory-name-p NAME) -> t or nil
pub(crate) fn builtin_directory_name_p(args: Vec<Value>) -> EvalResult {
    expect_args("directory-name-p", &args, 1)?;
    let name = expect_string_strict(&args[0])?;
    Ok(Value::bool_val(directory_name_p(&name)))
}

/// (substitute-in-file-name FILENAME) -> string
pub(crate) fn builtin_substitute_in_file_name(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "substitute-in-file-name", &args)? {
        return Ok(result);
    }
    expect_args("substitute-in-file-name", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    Ok(Value::string(substitute_in_file_name(&filename)))
}

pub(crate) fn default_directory_in_state(
    obarray: &Obarray,
    _dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
) -> Option<String> {
    if let Some(buf) = buffers.current_buffer() {
        if let Some(val) = buf.get_buffer_local("default-directory") {
            if val.is_string() {
                return Some(super::builtins::lisp_string_to_runtime_string(val));
            }
        }
    }
    match obarray.symbol_value("default-directory") {
        Some(val) if val.is_string() => Some(super::builtins::lisp_string_to_runtime_string(*val)),
        _ => None,
    }
}

fn default_directory_for_eval(eval: &Context) -> Option<String> {
    default_directory_in_state(&eval.obarray, &[], &eval.buffers)
}

pub(crate) fn resolve_filename_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    filename: &str,
) -> String {
    if filename.is_empty() || Path::new(filename).is_absolute() {
        return filename.to_string();
    }
    let default_dir = default_directory_in_state(obarray, dynamic, buffers);
    expand_file_name(filename, default_dir.as_deref())
}

pub(crate) fn resolve_filename_for_eval(eval: &Context, filename: &str) -> String {
    resolve_filename_in_state(&eval.obarray, &[], &eval.buffers, filename)
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

/// `(access-file FILENAME STRING)`
pub(crate) fn builtin_access_file(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "access-file", &args)? {
        return Ok(result);
    }
    expect_args("access-file", &args, 2)?;
    let filename = resolve_filename_for_eval(eval, &expect_string_strict(&args[0])?);
    let operation = expect_string_strict(&args[1])?;
    match fs::metadata(&filename) {
        Ok(_) => Ok(Value::NIL),
        Err(err) => Err(signal_file_action_error(err, &operation, &filename)),
    }
}

/// Context-aware variant of `file-exists-p` that resolves relative paths
/// against dynamic/default `default-directory`.
pub(crate) fn builtin_file_exists_p(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "file-exists-p", &args)? {
        return Ok(result);
    }
    expect_args("file-exists-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    Ok(Value::bool_val(file_exists_p(&filename)))
}

/// `(file-readable-p FILENAME)` — resolves FILENAME against
/// dynamic/default `default-directory`.
pub(crate) fn builtin_file_readable_p(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "file-readable-p", &args)? {
        return Ok(result);
    }
    expect_args("file-readable-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    Ok(Value::bool_val(file_readable_p(&filename)))
}

/// `(file-writable-p FILENAME)`
pub(crate) fn builtin_file_writable_p(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "file-writable-p", &args)? {
        return Ok(result);
    }
    expect_args("file-writable-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    Ok(Value::bool_val(file_writable_p(&filename)))
}

/// `(file-accessible-directory-p FILENAME)`
pub(crate) fn builtin_file_accessible_directory_p(
    eval: &mut Context,
    args: Vec<Value>,
) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "file-accessible-directory-p", &args)? {
        return Ok(result);
    }
    expect_args("file-accessible-directory-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    Ok(Value::bool_val(file_accessible_directory_p(&filename)))
}

/// `(file-executable-p FILENAME)`
pub(crate) fn builtin_file_executable_p(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "file-executable-p", &args)? {
        return Ok(result);
    }
    expect_args("file-executable-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    Ok(Value::bool_val(file_executable_p(&filename)))
}

/// `(file-acl FILENAME)` — stub returning nil. Native ACL support
/// is not yet implemented in NeoMacs; the dispatch path lets a
/// handler intercept first.
pub(crate) fn builtin_file_acl(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "file-acl", &args)? {
        return Ok(result);
    }
    expect_args("file-acl", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let _filename = resolve_filename_for_eval(eval, &filename);
    Ok(Value::NIL)
}

/// (set-file-acl FILENAME ACL) -> nil
pub(crate) fn builtin_set_file_acl(args: Vec<Value>) -> EvalResult {
    expect_args("set-file-acl", &args, 2)?;
    let _filename = &args[0];
    let _acl = &args[1];
    Ok(Value::NIL)
}

/// `(file-locked-p FILENAME)`
pub(crate) fn builtin_file_locked_p(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "file-locked-p", &args)? {
        return Ok(result);
    }
    expect_args("file-locked-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    Ok(Value::bool_val(file_locked_p(&filename)))
}

/// `(file-selinux-context FILENAME)` — stub returning a four-element
/// nil list, matching GNU's "no SELinux on this system" shape.
pub(crate) fn builtin_file_selinux_context(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "file-selinux-context", &args)? {
        return Ok(result);
    }
    expect_args("file-selinux-context", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let _filename = resolve_filename_for_eval(eval, &filename);
    Ok(Value::list(vec![
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
    ]))
}

/// (set-file-selinux-context FILENAME CONTEXT) -> nil
pub(crate) fn builtin_set_file_selinux_context(args: Vec<Value>) -> EvalResult {
    expect_args("set-file-selinux-context", &args, 2)?;
    let _filename = expect_string_strict(&args[0])?;
    let _context = &args[1];
    Ok(Value::NIL)
}

/// `(file-system-info FILENAME)`
pub(crate) fn builtin_file_system_info(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "file-system-info", &args)? {
        return Ok(result);
    }
    expect_args("file-system-info", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    let (total, free, avail) = file_system_info(&filename)?;
    Ok(Value::list(vec![
        Value::fixnum(total),
        Value::fixnum(free),
        Value::fixnum(avail),
    ]))
}

/// `(file-directory-p FILENAME)`
pub(crate) fn builtin_file_directory_p(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "file-directory-p", &args)? {
        return Ok(result);
    }
    expect_args("file-directory-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    Ok(Value::bool_val(file_directory_p(&filename)))
}

/// Context-aware variant of `file-regular-p` that resolves relative paths
/// against dynamic/default `default-directory`.
/// `(file-regular-p FILENAME)`
pub(crate) fn builtin_file_regular_p(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "file-regular-p", &args)? {
        return Ok(result);
    }
    expect_args("file-regular-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    Ok(Value::bool_val(file_regular_p(&filename)))
}

/// `(file-symlink-p FILENAME)`
///
/// Mirrors GNU `Ffile_symlink_p` (`src/fileio.c:3160`): returns the
/// link target as a string when FILENAME is a symbolic link, nil
/// otherwise. Previously this returned `Value::bool_val(...)` (audit
/// §10.3) which was a data-type bug — code that uses the result as a
/// path was always broken because it got `t` instead of a string.
pub(crate) fn builtin_file_symlink_p(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "file-symlink-p", &args)? {
        return Ok(result);
    }
    expect_args("file-symlink-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    Ok(match file_symlink_target(&filename) {
        Some(target) => Value::string(target),
        None => Value::NIL,
    })
}

/// `(file-name-case-insensitive-p FILENAME)`
pub(crate) fn builtin_file_name_case_insensitive_p(
    eval: &mut Context,
    args: Vec<Value>,
) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "file-name-case-insensitive-p", &args)? {
        return Ok(result);
    }
    expect_args("file-name-case-insensitive-p", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    Ok(Value::bool_val(file_name_case_insensitive_p(&filename)))
}

/// `(file-newer-than-file-p FILE1 FILE2)`
pub(crate) fn builtin_file_newer_than_file_p(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler_two_arg(eval, "file-newer-than-file-p", &args)? {
        return Ok(result);
    }
    expect_args("file-newer-than-file-p", &args, 2)?;
    let file1 = expect_string_strict(&args[0])?;
    let file2 = expect_string_strict(&args[1])?;
    let file1 = resolve_filename_for_eval(eval, &file1);
    let file2 = resolve_filename_for_eval(eval, &file2);
    Ok(Value::bool_val(file_newer_than_file_p(&file1, &file2)))
}

/// `(file-modes FILENAME &optional FLAG)` — returns the file's
/// mode bits as an integer, or nil if FILENAME is missing.
pub(crate) fn builtin_file_modes(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "file-modes", &args)? {
        return Ok(result);
    }
    expect_min_args("file-modes", &args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("file-modes"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    match file_modes(&filename) {
        Some(mode) => Ok(Value::fixnum(mode as i64)),
        None => Ok(Value::NIL),
    }
}

/// `(set-file-modes FILENAME MODE &optional FLAG)`
pub(crate) fn builtin_set_file_modes(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "set-file-modes", &args)? {
        return Ok(result);
    }
    expect_min_args("set-file-modes", &args, 2)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("set-file-modes"),
                Value::fixnum(args.len() as i64),
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
    Ok(Value::NIL)
}

/// `(set-file-times FILENAME &optional TIME FLAG)`
pub(crate) fn builtin_set_file_times(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "set-file-times", &args)? {
        return Ok(result);
    }
    expect_min_args("set-file-times", &args, 1)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("set-file-times"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    let timestamp = if args.len() > 1 && !args[1].is_nil() {
        Some(parse_timestamp_arg(&args[1])?)
    } else {
        None
    };
    let nofollow = args.get(2).is_some_and(|flag| !flag.is_nil());
    set_file_times_compat(&filename, timestamp, nofollow)?;
    Ok(Value::T)
}

fn validate_optional_buffer_arg_in_state(
    buffers: &crate::buffer::BufferManager,
    arg: Option<&Value>,
) -> Result<(), Flow> {
    if let Some(bufferish) = arg {
        match bufferish.kind() {
            ValueKind::Nil => Ok(()),
            ValueKind::Veclike(VecLikeType::Buffer) => {
                if let Some(buf_id) = bufferish.as_buffer_id() {
                    if buffers.get(buf_id).is_some() {
                        Ok(())
                    } else {
                        Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("bufferp"), *bufferish],
                        ))
                    }
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
    match arg.kind() {
        ValueKind::Fixnum(_) => Err(signal(
            "args-out-of-range",
            vec![*arg, Value::fixnum(-1), Value::fixnum(0)],
        )),
        ValueKind::String => Err(signal(
            "error",
            vec![Value::string("Invalid time specification")],
        )),
        ValueKind::Float | ValueKind::Cons => Ok(()),
        _ => Err(signal(
            "error",
            vec![Value::string("Invalid time specification")],
        )),
    }
}

/// (visited-file-modtime) -> 0
pub(crate) fn builtin_visited_file_modtime(args: Vec<Value>) -> EvalResult {
    expect_args("visited-file-modtime", &args, 0)?;
    Ok(Value::fixnum(0))
}

/// `(verify-visited-file-modtime &optional BUFFER)` — stub returning t.
pub(crate) fn builtin_verify_visited_file_modtime(
    eval: &mut Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("verify-visited-file-modtime", &args, 1)?;
    validate_optional_buffer_arg_in_state(&eval.buffers, args.first())?;
    Ok(Value::T)
}

/// `(set-visited-file-modtime &optional TIME-LIST)` — stub returning nil.
pub(crate) fn builtin_set_visited_file_modtime(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    expect_max_args("set-visited-file-modtime", &args, 1)?;
    if let Some(arg) = args.first() {
        if !arg.is_nil() {
            validate_set_visited_file_modtime_arg(arg)?;
            return Ok(Value::NIL);
        }
    }

    let file_name = eval
        .buffers
        .current_buffer()
        .and_then(|buf| buf.file_name_owned());
    if file_name.is_none() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), Value::NIL],
        ));
    }

    Ok(Value::NIL)
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
    Ok(Value::NIL)
}

/// (default-file-modes) -> integer
pub(crate) fn builtin_default_file_modes(args: Vec<Value>) -> EvalResult {
    expect_args("default-file-modes", &args, 0)?;
    init_default_file_mode_mask();
    let mask = DEFAULT_FILE_MODE_MASK.load(Ordering::Relaxed) as i64;
    Ok(Value::fixnum((!mask) & 0o777))
}

/// Context-aware variant of `delete-file` that resolves relative paths
/// against dynamic/default `default-directory`.
pub(crate) fn builtin_delete_file(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "delete-file", &args)? {
        return Ok(result);
    }
    expect_min_args("delete-file", &args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("delete-file"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    delete_file_compat(&filename)?;
    Ok(Value::NIL)
}

/// `(delete-file-internal FILENAME)` — internal primitive used by
/// the elisp `delete-file` wrapper.
pub(crate) fn builtin_delete_file_internal(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "delete-file", &args)? {
        return Ok(result);
    }
    expect_args("delete-file-internal", &args, 1)?;
    let filename = expect_string_strict(&args[0])?;
    let filename = resolve_filename_for_eval(eval, &filename);
    delete_file_compat(&filename)?;
    Ok(Value::NIL)
}

/// `(delete-directory-internal DIRECTORY)`
pub(crate) fn builtin_delete_directory_internal(
    eval: &mut Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("delete-directory-internal", &args, 1)?;
    let directory = expect_string_strict(&args[0])?;
    let directory = resolve_filename_for_eval(eval, &directory);
    fs::remove_dir(&directory)
        .map_err(|err| signal_file_io_path(err, "Removing directory", &directory))?;
    Ok(Value::NIL)
}

/// Context-aware variant of `delete-directory` that resolves relative paths
/// against dynamic/default `default-directory`.
pub(crate) fn builtin_delete_directory(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "delete-directory", &args)? {
        return Ok(result);
    }
    expect_min_args("delete-directory", &args, 1)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("delete-directory"),
                Value::fixnum(args.len() as i64),
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
    Ok(Value::NIL)
}

/// `(make-symbolic-link TARGET LINKNAME &optional OK-IF-EXISTS)`
pub(crate) fn builtin_make_symbolic_link(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler_two_arg(eval, "make-symbolic-link", &args)? {
        return Ok(result);
    }
    expect_min_args("make-symbolic-link", &args, 2)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("make-symbolic-link"),
                Value::fixnum(args.len() as i64),
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
        Ok(Value::NIL)
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

/// `(rename-file FROM TO &optional OK-IF-EXISTS)`
pub(crate) fn builtin_rename_file(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler_two_arg(eval, "rename-file", &args)? {
        return Ok(result);
    }
    expect_min_args("rename-file", &args, 2)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("rename-file"),
                Value::fixnum(args.len() as i64),
            ],
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
    Ok(Value::NIL)
}

/// `(copy-file FROM TO &optional OK-IF-EXISTS KEEP-TIME PRESERVE-UID-GID PRESERVE-PERMISSIONS)`
pub(crate) fn builtin_copy_file(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler_two_arg(eval, "copy-file", &args)? {
        return Ok(result);
    }
    expect_min_args("copy-file", &args, 2)?;
    if args.len() > 6 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("copy-file"), Value::fixnum(args.len() as i64)],
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
    Ok(Value::NIL)
}

/// `(add-name-to-file OLDNAME NEWNAME &optional OK-IF-EXISTS)`
pub(crate) fn builtin_add_name_to_file(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler_two_arg(eval, "add-name-to-file", &args)? {
        return Ok(result);
    }
    expect_min_args("add-name-to-file", &args, 2)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("add-name-to-file"),
                Value::fixnum(args.len() as i64),
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
    Ok(Value::NIL)
}

/// `(make-directory-internal DIRECTORY)` — internal primitive for the
/// elisp `make-directory` wrapper. GNU dispatches the handler at the
/// `make-directory` level via Qmake_directory; we mirror that so
/// callers that go through the internal entry point still see the
/// handler.
pub(crate) fn builtin_make_directory_internal(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "make-directory", &args)? {
        return Ok(result);
    }
    expect_args("make-directory-internal", &args, 1)?;
    let dir = expect_string_strict(&args[0])?;
    let dir = resolve_filename_for_eval(eval, &dir);
    make_directory(&dir, false).map_err(|e| signal_file_io_path(e, "Creating directory", &dir))?;
    Ok(Value::NIL)
}

/// `(find-file-name-handler FILENAME OPERATION)` — public elisp
/// surface for the [`find_file_name_handler`] dispatch helper.
pub(crate) fn builtin_find_file_name_handler(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    expect_args("find-file-name-handler", &args, 2)?;
    let filename = expect_string_strict(&args[0])?;
    let operation = args[1];
    Ok(find_file_name_handler(&eval.obarray, &filename, operation))
}

/// Walk `file-name-handler-alist` looking for a handler matching FILENAME
/// for OPERATION. Mirrors GNU `Ffind_file_name_handler`
/// (`src/fileio.c:371`).
///
/// The alist is a list of `(REGEXP . HANDLER)` cons cells. For each
/// matching entry the highest match position wins (using `>` not `>=`,
/// so the *first* match at any given position is preferred). When
/// `OPERATION` equals `inhibit-file-name-operation`, handlers listed
/// in `inhibit-file-name-handlers` are skipped — that is how a handler
/// can call standard primitives without recursing into itself.
///
/// If a handler symbol carries a non-nil `'operations` property, the
/// handler is only used when `OPERATION` is in that list. This lets
/// handlers declare a restricted operation set without writing
/// trampolines for everything else.
pub(crate) fn find_file_name_handler(obarray: &Obarray, filename: &str, operation: Value) -> Value {
    // Read the alist. If unbound or non-list, no handlers apply.
    let alist = match obarray.symbol_value("file-name-handler-alist") {
        Some(v) if v.is_cons() => *v,
        _ => return Value::NIL,
    };

    // Compute the inhibit list lazily — only consulted when operation
    // matches inhibit-file-name-operation.
    let mut inhibited: Option<Value> = None;
    if let Some(inh_op) = obarray.symbol_value("inhibit-file-name-operation").copied() {
        if !inh_op.is_nil() && super::value::eq_value(&inh_op, &operation) {
            inhibited = obarray.symbol_value("inhibit-file-name-handlers").copied();
        }
    }

    // Walk the alist exactly like GNU's loop, picking the entry with
    // the strictly-greatest match position.
    let mut best: Value = Value::NIL;
    let mut best_pos: i64 = -1;
    let mut cursor = alist;
    while cursor.is_cons() {
        let entry = cursor.cons_car();
        cursor = cursor.cons_cdr();
        if !entry.is_cons() {
            continue;
        }
        let regexp_val = entry.cons_car();
        let handler = entry.cons_cdr();
        let Some(regexp) = regexp_val.as_str() else {
            continue;
        };

        // If the handler is a symbol with a non-nil `operations`
        // property, restrict to listed operations. Mirrors GNU's
        // `Fget (handler, Qoperations)` check at fileio.c:409.
        if let Some(handler_sym) = handler.as_symbol_id() {
            let ops_sym = super::intern::intern("operations");
            if let Some(ops) = obarray
                .get_property_id(handler_sym, ops_sym)
                .copied()
                .filter(|v| !v.is_nil())
            {
                let mut op_cursor = ops;
                let mut found = false;
                while op_cursor.is_cons() {
                    if super::value::eq_value(&op_cursor.cons_car(), &operation) {
                        found = true;
                        break;
                    }
                    op_cursor = op_cursor.cons_cdr();
                }
                if !found {
                    continue;
                }
            }
        }

        // Match the regexp against the filename.
        let mut match_data: Option<crate::emacs_core::regex::MatchData> = None;
        let match_pos = match super::regex::string_match_full(regexp, filename, 0, &mut match_data)
        {
            Ok(Some(pos)) => pos as i64,
            _ => continue,
        };

        if match_pos > best_pos {
            // Skip if this handler is inhibited for the current operation.
            if let Some(inh) = inhibited {
                let mut inh_cursor = inh;
                let mut skip = false;
                while inh_cursor.is_cons() {
                    if super::value::eq_value(&inh_cursor.cons_car(), &handler) {
                        skip = true;
                        break;
                    }
                    inh_cursor = inh_cursor.cons_cdr();
                }
                if skip {
                    continue;
                }
            }
            best = handler;
            best_pos = match_pos;
        }
    }
    best
}

/// Convenience for builtins that have an `eval` context. Looks up a
/// handler for `(filename, operation)` and, if one is installed,
/// invokes it as `(funcall handler operation arg1 arg2 ...)` and
/// returns the result wrapped in `Some`. Returns `None` if no handler
/// matched, in which case the caller should fall back to its native
/// implementation.
///
/// `operation_name` is the symbol the handler will receive as its
/// first argument (e.g. `"file-exists-p"`). It must match the GNU
/// operation symbol exactly.
pub(crate) fn dispatch_file_handler(
    eval: &mut Context,
    operation_name: &str,
    args: &[Value],
) -> Result<Option<Value>, super::error::Flow> {
    // Every operation we wire up takes the filename in args[0]. Two-
    // argument file ops (copy-file, rename-file, add-name-to-file,
    // make-symbolic-link) need to consult the handler for *both*
    // names; those have a separate helper below.
    let Some(first) = args.first() else {
        return Ok(None);
    };
    let Some(filename) = first.as_str() else {
        return Ok(None);
    };
    let operation_sym = Value::symbol(operation_name);
    let handler = find_file_name_handler(&eval.obarray, filename, operation_sym);
    if handler.is_nil() {
        return Ok(None);
    }
    // Build (operation arg1 arg2 ...) and funcall the handler.
    let mut call_args = Vec::with_capacity(args.len() + 1);
    call_args.push(operation_sym);
    call_args.extend_from_slice(args);
    let result = eval.funcall_general(handler, call_args)?;
    Ok(Some(result))
}

/// Two-argument variant for builtins like `copy-file` and `rename-file`.
/// Mirrors GNU's pattern of consulting the handler for the source
/// first and falling back to the destination.
pub(crate) fn dispatch_file_handler_two_arg(
    eval: &mut Context,
    operation_name: &str,
    args: &[Value],
) -> Result<Option<Value>, super::error::Flow> {
    if args.len() < 2 {
        return Ok(None);
    }
    let operation_sym = Value::symbol(operation_name);
    // Source file first.
    if let Some(src) = args[0].as_str() {
        let handler = find_file_name_handler(&eval.obarray, src, operation_sym);
        if !handler.is_nil() {
            let mut call_args = Vec::with_capacity(args.len() + 1);
            call_args.push(operation_sym);
            call_args.extend_from_slice(args);
            return Ok(Some(eval.funcall_general(handler, call_args)?));
        }
    }
    // Destination file second.
    if let Some(dst) = args[1].as_str() {
        let handler = find_file_name_handler(&eval.obarray, dst, operation_sym);
        if !handler.is_nil() {
            let mut call_args = Vec::with_capacity(args.len() + 1);
            call_args.push(operation_sym);
            call_args.extend_from_slice(args);
            return Ok(Some(eval.funcall_general(handler, call_args)?));
        }
    }
    Ok(None)
}

/// `(directory-files DIRECTORY &optional FULL MATCH NOSORT COUNT)`
pub(crate) fn builtin_directory_files(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "directory-files", &args)? {
        return Ok(result);
    }
    expect_min_args("directory-files", &args, 1)?;
    if args.len() > 5 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("directory-files"),
                Value::fixnum(args.len() as i64),
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
        match val.kind() {
            ValueKind::Fixnum(n) if n >= 0 => Some(n as usize),
            _other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("natnump"), *val],
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
// Context-dependent builtins
// ===========================================================================

fn expect_int(value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *value],
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

fn current_buffer_id_or_error(
    buffers: &crate::buffer::BufferManager,
) -> Result<crate::buffer::BufferId, Flow> {
    buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))
}

fn replace_accessible_portion_in_current_buffer(
    buffers: &mut crate::buffer::BufferManager,
    current_id: crate::buffer::BufferId,
    text: &str,
) -> Result<(), Flow> {
    let (start, end, old_point) = {
        let buf = buffers
            .get(current_id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        (buf.point_min_byte(), buf.point_max_byte(), buf.point_byte())
    };
    if start < end {
        buffers
            .delete_buffer_region(current_id, start, end)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    }
    buffers
        .goto_buffer_byte(current_id, start)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    if !text.is_empty() {
        buffers
            .insert_into_buffer(current_id, text)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    }
    let replacement_end = start + text.len();
    let restored_point = if old_point <= start {
        old_point
    } else {
        replacement_end.min(start + (old_point - start))
    };
    buffers
        .goto_buffer_byte(current_id, restored_point)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    Ok(())
}

fn insert_file_contents_into_current_buffer_in_state(
    buffers: &mut crate::buffer::BufferManager,
    current_id: crate::buffer::BufferId,
    contents: &str,
    replace_requested: bool,
) -> Result<(), Flow> {
    if replace_requested {
        replace_accessible_portion_in_current_buffer(buffers, current_id, contents)
    } else {
        // GNU Emacs: insert-file-contents inserts text at point but does NOT
        // advance point past the inserted text (unlike regular `insert`).
        // It calls TEMP_SET_PT_BOTH(BEG, BEG_BYTE) to keep point at the
        // beginning of the inserted region.
        let pt_before = buffers
            .get(current_id)
            .map(|b| (b.pt_byte, b.pt))
            .unwrap_or((0, 0));
        buffers
            .insert_into_buffer(current_id, contents)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        // Restore point to before the insertion (matching GNU).
        if let Some(buf) = buffers.get_mut(current_id) {
            buf.pt_byte = pt_before.0;
            buf.pt = pt_before.1;
        }
        Ok(())
    }
}

fn expect_inserted_char_count(value: &Value) -> Result<i64, Flow> {
    let inserted = expect_int(value)?;
    if inserted < 0 {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *value],
        ));
    }
    Ok(inserted)
}

fn run_after_insert_file_pipeline(
    eval: &mut Context,
    current_id: crate::buffer::BufferId,
    visit: bool,
    replace_requested: bool,
    inserted_chars: i64,
) -> Result<i64, Flow> {
    let visit_value = if visit { Value::T } else { Value::NIL };
    let mut inserted = inserted_chars;

    if eval.obarray.fboundp("after-insert-file-set-coding") {
        let result = eval.funcall_general(
            Value::symbol("after-insert-file-set-coding"),
            vec![Value::fixnum(inserted), visit_value],
        )?;
        if !result.is_nil() {
            inserted = expect_inserted_char_count(&result)?;
        }
    }

    if inserted <= 0 || !eval.obarray.fboundp("format-decode") {
        return Ok(inserted);
    }

    let (saved_pt, saved_pt_char, point_min, chars_modiff_before) = eval
        .buffers
        .get(current_id)
        .map(|buf| {
            (
                buf.pt_byte,
                buf.pt,
                buf.point_min_byte(),
                buf.chars_modified_tick(),
            )
        })
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let specpdl_count = eval.specpdl.len();
    eval.specbind(intern("inhibit-point-motion-hooks"), Value::T);
    eval.specbind(intern("inhibit-modification-hooks"), Value::T);
    eval.specbind(intern("buffer-undo-list"), Value::T);

    let pipeline_result = (|| -> Result<i64, Flow> {
        if replace_requested {
            eval.buffers
                .goto_buffer_byte(current_id, point_min)
                .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        }

        let format_result = eval.funcall_general(
            Value::symbol("format-decode"),
            vec![Value::NIL, Value::fixnum(inserted), visit_value],
        )?;
        if !format_result.is_nil() {
            inserted = expect_inserted_char_count(&format_result)?;
        }

        let hook_sym = intern("after-insert-file-functions");
        let hook_value = eval.visible_variable_value_or_nil("after-insert-file-functions");
        let hook_functions = crate::emacs_core::hook_runtime::collect_hook_functions_in_state(
            eval, hook_sym, hook_value, true,
        );
        if !hook_functions.is_empty() {
            let mut roots = hook_functions.clone();
            roots.push(Value::fixnum(inserted));
            inserted = eval.with_gc_scope_result(|ctx| {
                for root in &roots {
                    ctx.root(*root);
                }
                let mut inserted_now = inserted;
                for function in &hook_functions {
                    let result = ctx.apply(*function, vec![Value::fixnum(inserted_now)])?;
                    if !result.is_nil() {
                        inserted_now = expect_inserted_char_count(&result)?;
                    }
                }
                Ok(inserted_now)
            })?;
        }

        Ok(inserted)
    })();

    eval.restore_current_buffer_if_live(current_id);
    let chars_modiff_after = eval
        .buffers
        .get(current_id)
        .map(|buf| buf.chars_modified_tick())
        .unwrap_or(chars_modiff_before + 1);
    if replace_requested
        && chars_modiff_after == chars_modiff_before
        && let Some(buf) = eval.buffers.get_mut(current_id)
    {
        buf.pt_byte = saved_pt;
        buf.pt = saved_pt_char;
    }
    eval.unbind_to(specpdl_count);

    pipeline_result
}

fn write_region_content_in_state(
    buffers: &crate::buffer::BufferManager,
    current_id: crate::buffer::BufferId,
    start: &Value,
    end: Option<&Value>,
) -> Result<crate::heap_types::LispString, Flow> {
    if start.is_string() {
        return start.as_lisp_string().cloned().ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *start],
            )
        });
    }

    let buf = buffers
        .get(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    if start.is_nil() {
        return Ok(buf.buffer_substring_lisp_string(buf.point_min(), buf.point_max()));
    }

    let end = end.unwrap_or(&Value::NIL);
    let start = expect_int(start)?;
    let end = expect_int(end)?;
    let point_min = buf.point_min_char() as i64 + 1;
    let point_max = buf.point_max_char() as i64 + 1;
    if start < point_min || start > point_max || end < point_min || end > point_max {
        return Err(signal(
            "args-out-of-range",
            vec![
                Value::make_buffer(buf.id),
                Value::fixnum(start),
                Value::fixnum(end),
            ],
        ));
    }
    let (char_start, char_end) = if start <= end {
        (start as usize - 1, end as usize - 1)
    } else {
        (end as usize - 1, start as usize - 1)
    };
    let byte_start = buf.lisp_pos_to_accessible_byte(char_start as i64 + 1);
    let byte_end = buf.lisp_pos_to_accessible_byte(char_end as i64 + 1);
    Ok(buf.buffer_substring_lisp_string(byte_start, byte_end))
}

fn decode_insert_file_contents(
    bytes: &[u8],
    multibyte: bool,
    source_load_context: bool,
    coding_system_for_read: Option<&str>,
) -> Result<(String, String), Flow> {
    let is_utf8_like_source_coding = |coding: &str| {
        let family = coding
            .strip_suffix("-unix")
            .or_else(|| coding.strip_suffix("-dos"))
            .or_else(|| coding.strip_suffix("-mac"))
            .unwrap_or(coding);
        matches!(family, "utf-8" | "utf-8-emacs")
    };

    let Some(coding) =
        coding_system_for_read.filter(|coding| !coding.is_empty() && *coding != "nil")
    else {
        if source_load_context && multibyte {
            return Ok((
                crate::emacs_core::load::decode_emacs_utf8(bytes),
                "utf-8-emacs".to_string(),
            ));
        }

        if !multibyte {
            return Ok((
                crate::emacs_core::string_escape::bytes_to_unibyte_storage_string(bytes),
                "no-conversion".to_string(),
            ));
        }

        let decoded = crate::encoding::decode_bytes(bytes, "utf-8-emacs");
        return Ok((decoded, "utf-8-emacs".to_string()));
    };

    if source_load_context && multibyte && is_utf8_like_source_coding(coding) {
        return Ok((
            crate::emacs_core::load::decode_emacs_utf8(bytes),
            coding.to_string(),
        ));
    }

    let decoded = crate::encoding::builtin_decode_coding_string(vec![
        Value::heap_string(crate::heap_types::LispString::from_unibyte(bytes.to_vec())),
        Value::symbol(coding),
    ])?;

    match decoded.kind() {
        ValueKind::String => Ok((
            super::builtins::lisp_string_to_runtime_string(decoded),
            coding.to_string(),
        )),
        other => Err(signal(
            "error",
            vec![Value::string(format!(
                "decode-coding-string returned non-string: {other:?}"
            ))],
        )),
    }
}

/// `(insert-file-contents FILENAME &optional VISIT BEG END REPLACE)`
///
/// Read file FILENAME and insert its contents into the current buffer
/// at point. Returns a list of `(FILENAME LENGTH)`. Mirrors GNU's
/// `Finsert_file_contents` (`src/fileio.c`).
pub(crate) fn builtin_insert_file_contents(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    if let Some(result) = dispatch_file_handler(eval, "insert-file-contents", &args)? {
        return Ok(result);
    }
    expect_min_args("insert-file-contents", &args, 1)?;
    expect_max_args("insert-file-contents", &args, 5)?;

    let coding_val = eval.visible_variable_value_or_nil("coding-system-for-read");
    let coding_system_for_read: Option<String> = match coding_val.kind() {
        ValueKind::Nil => None,
        ValueKind::Symbol(id) => Some(resolve_sym(id).to_owned()),
        ValueKind::String => Some(super::builtins::lisp_string_to_runtime_string(coding_val)),
        _ => None,
    };
    let source_load_context = eval
        .visible_variable_value_or_nil("set-auto-coding-for-load")
        .is_truthy();

    let filename = expect_string_strict(&args[0])?;
    let resolved = resolve_filename_for_eval(eval, &filename);
    let visit = args.get(1).is_some_and(|v| v.is_truthy());
    let replace_requested = args.get(4).is_some_and(|v| !v.is_nil());

    // Snapshot buffer state before the file read for modification hooks.
    let pre_state = eval.buffers.current_buffer().map(|buf| {
        if replace_requested {
            (
                buf.point_min_byte(),
                buf.point_max_byte(),
                super::editfns::byte_span_char_len(buf, buf.point_min_byte(), buf.point_max_byte()),
            )
        } else {
            (buf.pt_byte, buf.pt_byte, 0)
        }
    });
    if let Some((beg, end, _old_len)) = pre_state {
        super::editfns::signal_before_change(eval, beg, end)?;
    }

    let current_id = current_buffer_id_or_error(&eval.buffers)?;
    {
        let buf = eval
            .buffers
            .get(current_id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        if visit
            && (!args.get(2).is_none_or(|v| v.is_nil()) || !args.get(3).is_none_or(|v| v.is_nil()))
        {
            return Err(signal(
                "error",
                vec![Value::string("Attempt to visit less than an entire file")],
            ));
        }
        if visit && buf.base_buffer.is_some() {
            return Err(signal(
                "error",
                vec![Value::string(
                    "Cannot do file visiting in an indirect buffer",
                )],
            ));
        }
        if visit && !replace_requested && !buf.text.is_empty() {
            return Err(signal(
                "error",
                vec![Value::string(
                    "Cannot do file visiting in a non-empty buffer",
                )],
            ));
        }
        if crate::emacs_core::editfns::buffer_read_only_active_in_state(&eval.obarray, &[], buf) {
            return Err(signal(
                "buffer-read-only",
                vec![Value::make_buffer(current_id)],
            ));
        }
    }

    let contents_bytes =
        fs::read(&resolved).map_err(|e| signal_file_io_path(e, "Opening input file", &resolved))?;
    let file_len = contents_bytes.len() as i64;

    let begin = if args.get(2).is_some_and(|v| !v.is_nil()) {
        expect_file_offset(args.get(2).expect("checked above"))?
    } else {
        0
    };
    let mut end_off = if args.get(3).is_some_and(|v| !v.is_nil()) {
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
    if end_off > file_len {
        end_off = file_len;
    }
    if end_off < begin {
        end_off = begin;
    }

    let slice = &contents_bytes[begin as usize..end_off as usize];
    let multibyte = eval
        .buffers
        .get(current_id)
        .map(|buffer| buffer.get_multibyte())
        .unwrap_or(true);
    let (contents, used_coding) = decode_insert_file_contents(
        slice,
        multibyte,
        source_load_context,
        coding_system_for_read.as_deref(),
    )?;
    let decoded_char_count = contents.chars().count() as i64;

    insert_file_contents_into_current_buffer_in_state(
        &mut eval.buffers,
        current_id,
        &contents,
        replace_requested,
    )?;

    let inserted_char_count = run_after_insert_file_pipeline(
        eval,
        current_id,
        visit,
        replace_requested,
        decoded_char_count,
    )?;

    if visit {
        let _ = eval
            .buffers
            .set_buffer_file_name(current_id, Some(resolved.clone()));
        let _ = eval.buffers.set_buffer_modified_flag(current_id, false);
    }

    let value = Value::list(vec![
        Value::string(resolved),
        Value::fixnum(inserted_char_count),
    ]);
    eval.set_variable("last-coding-system-used", Value::symbol(&used_coding));

    // Fire after-change hooks.
    if let Some((beg, _old_end, old_len)) = pre_state {
        let new_end = eval
            .buffers
            .current_buffer()
            .map(|buf| {
                if replace_requested {
                    buf.point_max_byte()
                } else {
                    buf.pt_byte
                }
            })
            .unwrap_or(beg);
        super::editfns::signal_after_change(eval, beg, new_end, old_len)?;
    }

    Ok(value)
}

// ===========================================================================
// Backup file support
// ===========================================================================

/// Find the next numbered backup version for FILENAME.
/// Scans `filename.~1~`, `filename.~2~`, ... and returns `max + 1`.
fn next_backup_version_number(filename: &str) -> u32 {
    let path = Path::new(filename);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let base_name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let prefix = format!("{base_name}.~");
    let mut max_ver: u32 = 0;
    if let Ok(entries) = fs::read_dir(parent) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(rest) = name.strip_prefix(&prefix) {
                if let Some(num_str) = rest.strip_suffix('~') {
                    if let Ok(n) = num_str.parse::<u32>() {
                        max_ver = max_ver.max(n);
                    }
                }
            }
        }
    }
    max_ver + 1
}

/// Compute the backup file name for FILENAME, respecting `backup-directory-alist`
/// and `version-control`.
fn compute_backup_file_name(obarray: &Obarray, filename: &str) -> String {
    // Check backup-directory-alist for redirection
    let backup_dir = lookup_backup_directory(obarray, filename);

    let use_numbered = match obarray.symbol_value("version-control") {
        Some(v) if v.is_symbol_named("never") => false,
        Some(v) if v.is_nil() => {
            // nil => use numbered if the file already has numbered backups
            next_backup_version_number(filename) > 1
        }
        Some(v) if v.is_truthy() => true,
        _ => false,
    };

    if use_numbered {
        let ver = next_backup_version_number(filename);
        match backup_dir {
            Some(dir) => {
                let base = file_name_nondirectory(filename);
                format!("{dir}/{base}.~{ver}~")
            }
            None => format!("{filename}.~{ver}~"),
        }
    } else {
        match backup_dir {
            Some(dir) => {
                let base = file_name_nondirectory(filename);
                format!("{dir}/{base}~")
            }
            None => format!("{filename}~"),
        }
    }
}

/// Look up FILENAME in `backup-directory-alist`.  Each entry is
/// `(REGEXP . DIRECTORY)`.  Returns `Some(directory)` for the first match,
/// or `None` if no entry matches.
fn lookup_backup_directory(obarray: &Obarray, filename: &str) -> Option<String> {
    let alist_val = obarray.symbol_value("backup-directory-alist")?;
    let entries = list_to_vec(alist_val)?;
    for entry in &entries {
        if entry.is_cons() {
            let car = entry.cons_car();
            let cdr = entry.cons_cdr();
            let pattern = match car.kind() {
                ValueKind::String => super::builtins::lisp_string_to_runtime_string(car),
                _ => continue,
            };
            let dir = match cdr.kind() {
                ValueKind::String => super::builtins::lisp_string_to_runtime_string(cdr),
                _ => continue,
            };
            // Simple substring match (GNU uses regex, but for now substring is
            // a pragmatic approximation that covers the common `"."` catch-all).
            if pattern == "." || filename.contains(&pattern) {
                // Ensure the backup directory exists
                let dir_path = expand_file_name(&dir, None);
                let _ = fs::create_dir_all(&dir_path);
                return Some(dir_path);
            }
        }
    }
    None
}

/// Create a backup of FILENAME before saving, if appropriate.
///
/// Checks `make-backup-files`, `backup-inhibited`, and the buffer's
/// `buffer-backed-up` flag.  On success (or when backup is skipped),
/// sets `buffer-backed-up` to `t`.
fn backup_file_before_save(
    obarray: &Obarray,
    buffers: &mut crate::buffer::BufferManager,
    buffer_id: crate::buffer::BufferId,
    filename: &str,
) {
    // 1. Check make-backup-files (default t)
    if let Some(v) = obarray.symbol_value("make-backup-files") {
        if v.is_nil() {
            return;
        }
    }

    // 2. Check backup-inhibited
    if let Some(v) = obarray.symbol_value("backup-inhibited") {
        if v.is_truthy() {
            return;
        }
    }

    // 3. Check buffer-backed-up flag — skip if already backed up
    if let Some(buf) = buffers.get(buffer_id) {
        if let Some(v) = buf.get_buffer_local("buffer-backed-up") {
            if v.is_truthy() {
                return;
            }
        }
    }

    // 4. Only backup if the file already exists on disk
    if !Path::new(filename).exists() {
        return;
    }

    // 5. Compute backup name and copy
    let backup_name = compute_backup_file_name(obarray, filename);
    if fs::copy(filename, &backup_name).is_ok() {
        // 6. Set buffer-backed-up to t so we don't back up again until next change
        if let Some(buf) = buffers.get_mut(buffer_id) {
            buf.set_buffer_local("buffer-backed-up", Value::T);
        }
    }
}

// (write-region body now lives inline in builtin_write_region below.)

/// Resolve the coding system to use for writing.
///
/// Priority:
/// 1. `coding-system-for-write` (if bound and non-nil)
/// 2. `buffer-file-coding-system` (buffer-local)
/// 3. Fallback to `"utf-8"`
fn resolve_write_coding_system(
    obarray: &Obarray,
    buffers: &crate::buffer::BufferManager,
    buffer_id: crate::buffer::BufferId,
) -> String {
    // 1. Check coding-system-for-write
    if let Some(val) = obarray.symbol_value("coding-system-for-write") {
        if let Some(name) = coding_system_value_to_name(val) {
            return name;
        }
    }

    // 2. Check buffer-file-coding-system (buffer-local)
    if let Some(buf) = buffers.get(buffer_id) {
        if let Some(val) = buf.get_buffer_local("buffer-file-coding-system") {
            if let Some(name) = coding_system_value_to_name(&val) {
                return name;
            }
        }
    }

    // 3. Default
    "utf-8".to_string()
}

/// Extract a coding system name from a `Value` (symbol or string).
/// Returns `None` for nil / unrecognized types.
fn coding_system_value_to_name(val: &Value) -> Option<String> {
    match val.kind() {
        ValueKind::Nil => None,
        ValueKind::Symbol(id) => {
            let name = resolve_sym(id).to_owned();
            if name == "nil" { None } else { Some(name) }
        }
        ValueKind::String => {
            let name = super::builtins::lisp_string_to_runtime_string(*val);
            if name.is_empty() || name == "nil" {
                None
            } else {
                Some(name)
            }
        }
        _ => None,
    }
}

/// `(write-region START END FILENAME &optional APPEND VISIT LOCKNAME MUSTBENEW)`
///
/// Write the region between START and END to FILENAME. If START is
/// nil, writes the entire buffer. Mirrors GNU `Fwrite_region`
/// (`src/fileio.c`).
pub(crate) fn builtin_write_region(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    // The filename is at args[2], not args[0]. Mirrors GNU
    // `Fwrite_region`'s `Ffind_file_name_handler (filename, Qwrite_region)`
    // dispatch.
    if let Some(filename_val) = args.get(2) {
        if let Some(filename) = filename_val.as_str() {
            let op = Value::symbol("write-region");
            let handler = find_file_name_handler(&eval.obarray, filename, op);
            if !handler.is_nil() {
                let mut call_args = Vec::with_capacity(args.len() + 1);
                call_args.push(op);
                call_args.extend_from_slice(&args);
                return eval.funcall_general(handler, call_args);
            }
        }
    }

    expect_min_args("write-region", &args, 3)?;
    expect_max_args("write-region", &args, 7)?;
    let filename = expect_string_strict(&args[2])?;
    let resolved = resolve_filename_for_eval(eval, &filename);
    let append_mode = match args.get(3) {
        Some(value) if value.is_fixnum() || value.is_char() => {
            FileWriteMode::Seek(expect_file_offset(value)? as u64)
        }
        Some(value) if value.is_truthy() => FileWriteMode::Append,
        _ => FileWriteMode::Truncate,
    };
    let visit_path = match args.get(4) {
        Some(v) if v.is_t() => Some(resolved.clone()),
        Some(v) if v.is_string() => {
            Some(resolve_filename_for_eval(eval, &expect_string_strict(v)?))
        }
        _ => None,
    };
    let current_id = current_buffer_id_or_error(&eval.buffers)?;

    if visit_path.is_some() {
        let buf = eval
            .buffers
            .get(current_id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        if buf.base_buffer.is_some() {
            return Err(signal(
                "error",
                vec![Value::string(
                    "Cannot do file visiting in an indirect buffer",
                )],
            ));
        }
    }

    // --- Backup before save ---
    // Only for truncate mode (not append/seek) when visiting the file.
    if matches!(append_mode, FileWriteMode::Truncate) {
        backup_file_before_save(&eval.obarray, &mut eval.buffers, current_id, &resolved);
    }

    let content = write_region_content_in_state(&eval.buffers, current_id, &args[0], args.get(1))?;

    // --- Encode using the appropriate coding system ---
    // Priority: coding-system-for-write > buffer-file-coding-system > utf-8
    let coding_system = resolve_write_coding_system(&eval.obarray, &eval.buffers, current_id);
    let encoded_bytes = crate::encoding::encode_lisp_string(&content, &coding_system);

    // --- Write encoded bytes and handle fsync ---
    let file = write_bytes_to_file_with_mode(&encoded_bytes, &resolved, append_mode)
        .map_err(|e| signal_file_io_path(e, "Writing to", &resolved))?;

    // fsync after write unless write-region-inhibit-fsync is non-nil.
    let inhibit_fsync = eval
        .obarray
        .symbol_value("write-region-inhibit-fsync")
        .is_some_and(|v| v.is_truthy());
    if !inhibit_fsync {
        file.sync_all()
            .map_err(|e| signal_file_io_path(e, "Writing to", &resolved))?;
    }
    drop(file);

    if let Some(visit_path) = visit_path {
        let _ = eval
            .buffers
            .set_buffer_file_name(current_id, Some(visit_path));
        let _ = eval.buffers.set_buffer_modified_flag(current_id, false);
    }

    eval.set_variable("last-coding-system-used", Value::symbol(&coding_system));
    Ok(Value::NIL)
}

/// (find-file-noselect FILENAME &optional NOWARN RAWFILE) -> buffer
///
/// Read file FILENAME into a buffer and return the buffer.
/// If a buffer visiting FILENAME already exists, return it.
/// Does not select the buffer.
pub(crate) fn builtin_find_file_noselect(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("find-file-noselect", &args, 1)?;
    expect_max_args("find-file-noselect", &args, 4)?;
    let filename = expect_string(&args[0])?;
    let abs_path = resolve_filename_for_eval(eval, &filename);

    // Check if there's already a buffer visiting this file
    for buf_id in eval.buffers.buffer_list() {
        if let Some(buf) = eval.buffers.get(buf_id) {
            if buf.get_file_name() == Some(&abs_path) {
                return Ok(Value::make_buffer(buf_id));
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
        let saved_current = eval.buffers.current_buffer_id();

        eval.switch_current_buffer(buf_id)?;
        let content_len = contents.len();
        super::editfns::signal_before_change(eval, 0, 0)?;
        let _ = eval.buffers.insert_into_buffer(buf_id, &contents);
        super::editfns::signal_after_change(eval, 0, content_len, 0)?;
        let _ = eval.buffers.goto_buffer_byte(buf_id, 0);
        let _ = eval
            .buffers
            .set_buffer_file_name(buf_id, Some(abs_path.clone()));
        let _ = eval.buffers.set_buffer_modified_flag(buf_id, false);

        // Restore the previous current buffer
        if let Some(prev_id) = saved_current {
            eval.restore_current_buffer_if_live(prev_id);
        }
    } else {
        // File doesn't exist — create an empty buffer with the file name set
        let _ = eval.buffers.set_buffer_file_name(buf_id, Some(abs_path));
    }

    Ok(Value::make_buffer(buf_id))
}

// ===========================================================================
// Auto-save support
// ===========================================================================

/// Compute the auto-save file name for a buffer.
///
/// For visited files: `#filename#` in the same directory.
/// For non-visited buffers: `#*buffername*#` in the auto-save-list-file-prefix
/// directory (or temporary-file-directory as fallback).
fn make_auto_save_file_name_for_buffer(obarray: &Obarray, buf: &crate::buffer::Buffer) -> String {
    if let Some(file_name) = buf.get_file_name() {
        // Visited file: #dir/filename# -> dir/#filename#
        let dir = file_name_directory(file_name).unwrap_or_default();
        let base = file_name_nondirectory(file_name);
        format!("{dir}#{base}#")
    } else {
        // Non-visited buffer: #*buffername*# in prefix dir or temp dir
        let dir = obarray
            .symbol_value("auto-save-list-file-prefix")
            .and_then(|v| v.as_runtime_string_owned())
            .and_then(|s| {
                if s.is_empty() {
                    None
                } else {
                    file_name_directory(&s)
                }
            })
            .or_else(|| {
                obarray
                    .symbol_value("temporary-file-directory")
                    .and_then(|v| v.as_runtime_string_owned())
            })
            .unwrap_or_else(|| "/tmp/".to_string());
        let safe_name = buf.name.replace('/', "!");
        format!("{dir}#*{safe_name}*#")
    }
}

/// `(make-auto-save-file-name)` -> string
///
/// Return the file name to use for auto-saves of the current buffer.
/// Sets `buffer-auto-save-file-name` as a side effect.
pub(crate) fn builtin_make_auto_save_file_name(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("make-auto-save-file-name", &args, 0)?;
    expect_max_args("make-auto-save-file-name", &args, 0)?;
    let current_id = current_buffer_id_or_error(&eval.buffers)?;
    let auto_name = {
        let buf = eval
            .buffers
            .get(current_id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        make_auto_save_file_name_for_buffer(&eval.obarray, buf)
    };

    // Set buffer-auto-save-file-name as side effect
    if let Some(buf) = eval.buffers.get_mut(current_id) {
        buf.set_buffer_local("buffer-auto-save-file-name", Value::string(&auto_name));
        buf.set_auto_save_file_name_value(Some(auto_name.clone()));
    }

    Ok(Value::string(auto_name))
}

/// `(do-auto-save &optional NO-MESSAGE CURRENT)` -> nil
///
/// Auto-save all buffers that need it.
/// If NO-MESSAGE is non-nil, suppress the "Auto-saving..." message.
/// If CURRENT is non-nil, only auto-save the current buffer.
pub(crate) fn builtin_do_auto_save(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("do-auto-save", &args, 0)?;
    expect_max_args("do-auto-save", &args, 2)?;

    let _no_message = args.first().is_some_and(|v| v.is_truthy());
    let current_only = args.get(1).is_some_and(|v| v.is_truthy());

    // Run auto-save-hook before saving
    // (We skip hook running here since the stub infrastructure doesn't easily
    // support calling back into eval from here. The hook will be called by
    // Elisp wrappers if needed.)

    let auto_save_visited = eval
        .obarray
        .symbol_value("auto-save-visited-file-name")
        .is_some_and(|v| v.is_truthy());

    // Collect buffer ids to process
    let buffer_ids: Vec<crate::buffer::BufferId> = if current_only {
        eval.buffers.current_buffer_id().into_iter().collect()
    } else {
        eval.buffers.buffer_list()
    };

    for buf_id in buffer_ids {
        // Gather info from the buffer (immutable borrow)
        let (should_save, auto_save_name, file_name, content) = {
            let Some(buf) = eval.buffers.get(buf_id) else {
                continue;
            };

            // Skip internal buffers (name starts with space)
            if buf.name.starts_with(' ') {
                continue;
            }

            // Skip indirect buffers
            if buf.base_buffer.is_some() {
                continue;
            }

            // Check buffer-saved-size: if negative, auto-save is disabled for
            // this buffer
            if let Some(saved_size_val) = buf.get_buffer_local("buffer-saved-size") {
                if let Some(n) = saved_size_val.as_fixnum() {
                    if n < 0 {
                        continue;
                    }
                }
            }

            // Buffer must be modified since last auto-save
            // (modified_tick > autosave_modified_tick means unsaved changes)
            if buf.autosave_modified_tick >= buf.modified_tick() {
                continue;
            }

            // Buffer must actually be modified
            if !buf.is_modified() {
                continue;
            }

            // Determine the auto-save target
            let auto_name = buf.auto_save_file_name_owned();
            let visit_name = buf.file_name_owned();

            // Get buffer content (entire buffer, not just accessible region)
            let text = buf.text.text_range(0, buf.text.len());

            (true, auto_name, visit_name, text)
        };

        if !should_save {
            continue;
        }

        // Determine which file to write to
        let target = if auto_save_visited {
            // Save to visited file if auto-save-visited-file-name is set
            file_name.clone()
        } else {
            auto_save_name.clone()
        };

        let Some(target_path) = target else {
            // No auto-save file name and no visited file -- generate one
            let auto_name = {
                let buf = eval.buffers.get(buf_id).unwrap();
                make_auto_save_file_name_for_buffer(&eval.obarray, buf)
            };
            // Set the auto-save name on the buffer
            if let Some(buf) = eval.buffers.get_mut(buf_id) {
                buf.set_buffer_local("buffer-auto-save-file-name", Value::string(&auto_name));
                buf.set_auto_save_file_name_value(Some(auto_name.clone()));
            }
            // Write content
            let _ = write_string_to_file(&content, &auto_name, false);
            let _ = eval.buffers.set_buffer_auto_saved(buf_id);
            // Update buffer-saved-size
            let size = content.len() as i64;
            if let Some(buf) = eval.buffers.get_mut(buf_id) {
                buf.set_buffer_local("buffer-saved-size", Value::fixnum(size));
            }
            continue;
        };

        // Write the buffer content to the target file
        if write_string_to_file(&content, &target_path, false).is_ok() {
            // Mark the buffer as auto-saved
            let _ = eval.buffers.set_buffer_auto_saved(buf_id);
            // Update buffer-saved-size
            let size = content.len() as i64;
            if let Some(buf) = eval.buffers.get_mut(buf_id) {
                buf.set_buffer_local("buffer-saved-size", Value::fixnum(size));
            }
        }
    }

    Ok(Value::NIL)
}

// ===========================================================================
// Bootstrap variables
// ===========================================================================

pub fn register_bootstrap_vars(obarray: &mut crate::emacs_core::symbol::Obarray) {
    let temporary_file_directory = std::env::temp_dir().to_string_lossy().to_string();
    obarray.set_symbol_value("file-name-coding-system", Value::NIL);
    obarray.set_symbol_value("default-file-name-coding-system", Value::NIL);
    obarray.set_symbol_value("set-auto-coding-for-load", Value::NIL);
    obarray.set_symbol_value("file-name-handler-alist", Value::NIL);
    obarray.set_symbol_value("set-auto-coding-function", Value::NIL);
    obarray.set_symbol_value("after-insert-file-functions", Value::NIL);
    obarray.set_symbol_value("write-region-annotate-functions", Value::NIL);
    obarray.set_symbol_value("write-region-post-annotation-function", Value::NIL);
    obarray.set_symbol_value("write-region-annotations-so-far", Value::NIL);
    obarray.set_symbol_value("inhibit-file-name-handlers", Value::NIL);
    obarray.set_symbol_value("inhibit-file-name-operation", Value::NIL);
    obarray.set_symbol_value("directory-abbrev-alist", Value::NIL);
    obarray.set_symbol_value("auto-save-list-file-name", Value::NIL);
    obarray.set_symbol_value("auto-save-list-file-prefix", Value::NIL);
    obarray.set_symbol_value("auto-save-visited-file-name", Value::NIL);
    obarray.set_symbol_value("auto-save-include-big-deletions", Value::NIL);
    obarray.set_symbol_value("small-temporary-file-directory", Value::NIL);
    obarray.set_symbol_value("write-region-inhibit-fsync", Value::NIL);
    obarray.set_symbol_value("delete-by-moving-to-trash", Value::NIL);
    obarray.set_symbol_value("auto-save-file-name-transforms", Value::NIL);
    obarray.set_symbol_value(
        "temporary-file-directory",
        Value::string(temporary_file_directory),
    );
    obarray.set_symbol_value("create-lockfiles", Value::T);

    // Backup-related variables
    obarray.set_symbol_value("make-backup-files", Value::T);
    obarray.set_symbol_value("backup-inhibited", Value::NIL);
    obarray.set_symbol_value("version-control", Value::NIL);
    obarray.set_symbol_value("backup-directory-alist", Value::NIL);
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
