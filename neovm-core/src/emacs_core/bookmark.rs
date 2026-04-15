//! Bookmark system -- persistent named positions.
//!
//! Provides Emacs-compatible bookmark functionality:
//! - `bookmark-set` -- create or update a bookmark at the current position
//! - `bookmark-jump` -- retrieve bookmark data (position, buffer, context)
//! - `bookmark-delete` -- remove a bookmark
//! - `bookmark-rename` -- rename a bookmark
//! - `bookmark-all-names` -- list all bookmark names
//! - `bookmark-get-filename` -- get the filename for a bookmark
//! - `bookmark-get-position` -- get the position for a bookmark
//! - `bookmark-get-annotation` -- get the annotation for a bookmark
//! - `bookmark-set-annotation` -- set the annotation for a bookmark
//! - `bookmark-save` -- serialize bookmarks to a string
//! - `bookmark-load` -- deserialize bookmarks from a string

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::value::{Value, ValueKind};
use crate::heap_types::LispString;

// ---------------------------------------------------------------------------
// Bookmark types
// ---------------------------------------------------------------------------

/// A single bookmark entry.
#[derive(Clone, Debug)]
pub struct Bookmark {
    /// The bookmark name (human-readable label).
    pub name: LispString,
    /// The filename of the file the bookmark points to (if any).
    pub filename: Option<LispString>,
    /// The character position in the buffer/file.
    pub position: usize,
    /// Text after the bookmark position, used for relocating if the file
    /// has changed.
    pub front_context: Option<LispString>,
    /// Text before the bookmark position, used for relocating.
    pub rear_context: Option<LispString>,
    /// An optional annotation (user note).
    pub annotation: Option<LispString>,
    /// A handler function name for jump (nil means default handler).
    pub handler: Option<LispString>,
}

// ---------------------------------------------------------------------------
// BookmarkManager
// ---------------------------------------------------------------------------

/// Central registry for all bookmarks.
#[derive(Clone, Debug)]
pub struct BookmarkManager {
    bookmarks: HashMap<BookmarkKey, Bookmark>,
    /// Most recently used bookmark names (most recent first).
    recent: Vec<LispString>,
    /// True if bookmarks have been modified since last save.
    modified: bool,
}

impl Default for BookmarkManager {
    fn default() -> Self {
        Self::new()
    }
}

impl BookmarkManager {
    /// Create a new empty bookmark manager.
    pub fn new() -> Self {
        Self {
            bookmarks: HashMap::new(),
            recent: Vec::new(),
            modified: false,
        }
    }

    /// Set (create or update) a bookmark.  Pushes the name to the front
    /// of the recently-used list.
    pub fn set(&mut self, name: LispString, mut bookmark: Bookmark) {
        bookmark.name = name.clone();
        self.bookmarks.insert(bookmark_lookup_key(&name), bookmark);
        self.touch_recent(name);
        self.modified = true;
    }

    /// Get a bookmark by name.
    pub fn get(&self, name: &LispString) -> Option<&Bookmark> {
        self.bookmarks.get(&bookmark_lookup_key(name))
    }

    /// Delete a bookmark. Returns true if it existed.
    pub fn delete(&mut self, name: &LispString) -> bool {
        let removed = self.bookmarks.remove(&bookmark_lookup_key(name)).is_some();
        if removed {
            self.recent.retain(|n| n != name);
            self.modified = true;
        }
        removed
    }

    /// Rename a bookmark.  Returns true on success, false if the old name
    /// does not exist or the new name is already taken.
    pub fn rename(&mut self, old: &LispString, new_name: LispString) -> bool {
        let old_key = bookmark_lookup_key(old);
        let new_key = bookmark_lookup_key(&new_name);
        if !self.bookmarks.contains_key(&old_key) {
            return false;
        }
        if old_key != new_key && self.bookmarks.contains_key(&new_key) {
            return false;
        }
        if let Some(mut bm) = self.bookmarks.remove(&old_key) {
            bm.name = new_name.clone();
            self.bookmarks.insert(new_key, bm);
            // Update recent list
            for entry in &mut self.recent {
                if entry == old {
                    *entry = new_name.clone();
                }
            }
            self.modified = true;
            true
        } else {
            false
        }
    }

    /// Return a sorted list of all bookmark names.
    pub fn all_names(&self) -> Vec<LispString> {
        let mut names: Vec<LispString> =
            self.bookmarks.values().map(|bm| bm.name.clone()).collect();
        names.sort_by_key(bookmark_string_to_runtime);
        names
    }

    /// Return the most recently used bookmark names (most recent first).
    pub fn recent_names(&self) -> &[LispString] {
        &self.recent
    }

    /// Whether the bookmark set has been modified since last save.
    pub fn is_modified(&self) -> bool {
        self.modified
    }

    /// Mark bookmarks as saved (clear modified flag).
    pub fn mark_saved(&mut self) {
        self.modified = false;
    }

    /// Serialize all bookmarks to a string.
    ///
    /// Format: one bookmark per block, separated by form-feeds.
    /// Each block:
    /// ```text
    /// NAME\nFILENAME\nPOSITION\nFRONT_CONTEXT\nREAR_CONTEXT\nANNOTATION\nHANDLER
    /// ```
    /// Empty optional fields are represented as the empty string.
    pub fn save_to_string(&self) -> String {
        let mut out = String::new();
        let mut bookmarks: Vec<&Bookmark> = self.bookmarks.values().collect();
        bookmarks.sort_by_key(|bm| bookmark_string_to_runtime(&bm.name));
        for (i, bm) in bookmarks.iter().enumerate() {
            if i > 0 {
                out.push('\x0C'); // form feed separator
            }
            out.push_str(&bookmark_string_to_runtime(&bm.name));
            out.push('\n');
            out.push_str(&optional_bookmark_string_to_runtime(bm.filename.as_ref()));
            out.push('\n');
            out.push_str(&bm.position.to_string());
            out.push('\n');
            out.push_str(&optional_bookmark_string_to_runtime(
                bm.front_context.as_ref(),
            ));
            out.push('\n');
            out.push_str(&optional_bookmark_string_to_runtime(
                bm.rear_context.as_ref(),
            ));
            out.push('\n');
            out.push_str(&optional_bookmark_string_to_runtime(bm.annotation.as_ref()));
            out.push('\n');
            out.push_str(&optional_bookmark_string_to_runtime(bm.handler.as_ref()));
        }
        out
    }

    /// Deserialize bookmarks from a string produced by `save_to_string`.
    /// Replaces all current bookmarks.
    pub fn load_from_string(&mut self, data: &str) {
        self.bookmarks.clear();
        self.recent.clear();
        self.modified = false;

        if data.is_empty() {
            return;
        }

        for block in data.split('\x0C') {
            let lines: Vec<&str> = block.split('\n').collect();
            if lines.len() < 7 {
                continue; // malformed block, skip
            }
            let name = runtime_string_to_bookmark_string(lines[0]);
            if name.is_empty() {
                continue;
            }
            let filename =
                (!lines[1].is_empty()).then(|| runtime_string_to_bookmark_string(lines[1]));
            let position = lines[2].parse::<usize>().unwrap_or(1);
            let front_context =
                (!lines[3].is_empty()).then(|| runtime_string_to_bookmark_string(lines[3]));
            let rear_context =
                (!lines[4].is_empty()).then(|| runtime_string_to_bookmark_string(lines[4]));
            let annotation =
                (!lines[5].is_empty()).then(|| runtime_string_to_bookmark_string(lines[5]));
            let handler =
                (!lines[6].is_empty()).then(|| runtime_string_to_bookmark_string(lines[6]));
            let bm = Bookmark {
                name: name.clone(),
                filename,
                position,
                front_context,
                rear_context,
                annotation,
                handler,
            };
            self.bookmarks.insert(bookmark_lookup_key(&name), bm);
        }
    }

    /// Move `name` to the front of the recently-used list, removing
    /// duplicates.
    fn touch_recent(&mut self, name: LispString) {
        self.recent.retain(|n| n != &name);
        self.recent.insert(0, name);
    }

    // pdump accessors
    pub(crate) fn dump_bookmarks(&self) -> &HashMap<BookmarkKey, Bookmark> {
        &self.bookmarks
    }
    pub(crate) fn dump_recent(&self) -> &[LispString] {
        &self.recent
    }
    pub(crate) fn from_dump(
        bookmarks: HashMap<BookmarkKey, Bookmark>,
        recent: Vec<LispString>,
    ) -> Self {
        Self {
            bookmarks,
            recent,
            modified: false,
        }
    }
}

fn runtime_string_to_bookmark_string(text: &str) -> LispString {
    super::builtins::runtime_string_to_lisp_string(text, true)
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) struct BookmarkKey(LispString);

impl BookmarkKey {
    pub(crate) fn from_lisp_string(text: &LispString) -> Self {
        Self(runtime_string_to_bookmark_string(
            &bookmark_string_to_runtime(text),
        ))
    }

    pub(crate) fn as_lisp_string(&self) -> &LispString {
        &self.0
    }
}

fn bookmark_lookup_key(text: &LispString) -> BookmarkKey {
    BookmarkKey::from_lisp_string(text)
}

fn bookmark_string_to_runtime(text: &LispString) -> String {
    super::builtins::runtime_string_from_lisp_string(text)
}

fn optional_bookmark_string_to_runtime(text: Option<&LispString>) -> String {
    text.map(bookmark_string_to_runtime).unwrap_or_default()
}

// ===========================================================================
// Builtin helpers
// ===========================================================================
#[cfg(test)]

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

fn expect_lisp_string(value: &Value) -> Result<LispString, Flow> {
    match value.kind() {
        ValueKind::String => Ok(value
            .as_lisp_string()
            .expect("ValueKind::String must carry LispString payload")
            .clone()),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *value],
        )),
    }
}

#[allow(dead_code)]
fn expect_int(value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *value],
        )),
    }
}

// ===========================================================================
// Builtins (evaluator-dependent)
// ===========================================================================

/// (bookmark-set NAME &optional NO-OVERWRITE) -> nil
///
/// In batch/non-file buffers GNU Emacs signals:
///   (error "Buffer not visiting a file or directory")
/// This implementation mirrors that behavior.
pub(crate) fn builtin_bookmark_set(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("bookmark-set", &args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("bookmark-set"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }
    let name = expect_lisp_string(&args[0])?;
    let _no_overwrite = args.get(1);

    let (position, filename) = match eval.buffers.current_buffer() {
        Some(buffer) => (buffer.point(), buffer.file_name_lisp_string().cloned()),
        None => (1, None),
    };

    let filename = match filename {
        Some(path) => Some(path),
        None => {
            return Err(signal(
                "error",
                vec![Value::string("Buffer not visiting a file or directory")],
            ));
        }
    };

    let bm = Bookmark {
        name: name.clone(),
        filename,
        position,
        front_context: None,
        rear_context: None,
        annotation: None,
        handler: None,
    };
    eval.bookmarks.set(name, bm);
    Ok(Value::NIL)
}

/// (bookmark-jump NAME) -> alist with bookmark data
///
/// Returns an alist: ((filename . F) (position . P) (annotation . A))
/// or signals an error if the bookmark does not exist.
pub(crate) fn builtin_bookmark_jump(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    if args.is_empty() || args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("bookmark-jump"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    let name = match args[0].kind() {
        ValueKind::Nil => {
            return Err(signal(
                "error",
                vec![Value::string("No bookmark specified")],
            ));
        }
        ValueKind::String => args[0]
            .as_lisp_string()
            .expect("ValueKind::String must carry LispString payload")
            .clone(),
        _ => return Ok(Value::NIL),
    };

    match eval.bookmarks.get(&name) {
        Some(bm) => {
            let filename_val = match &bm.filename {
                Some(f) => Value::heap_string(f.clone()),
                None => Value::NIL,
            };
            let position_val = Value::fixnum(bm.position as i64);
            let annotation_val = match &bm.annotation {
                Some(a) => Value::heap_string(a.clone()),
                None => Value::NIL,
            };
            let alist = Value::list(vec![
                Value::cons(Value::symbol("filename"), filename_val),
                Value::cons(Value::symbol("position"), position_val),
                Value::cons(Value::symbol("annotation"), annotation_val),
            ]);
            Ok(alist)
        }
        None => Err(signal(
            "error",
            vec![Value::string(format!(
                "Invalid bookmark {}",
                bookmark_string_to_runtime(&name)
            ))],
        )),
    }
}

/// (bookmark-delete NAME &optional BATCH) -> nil
pub(crate) fn builtin_bookmark_delete(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    if args.is_empty() || args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("bookmark-delete"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    // GNU Emacs accepts non-string NAME payloads and simply returns nil.
    // Only string names are actionable for deletion.
    if args[0].is_string() {
        let name = args[0]
            .as_lisp_string()
            .expect("ValueKind::String must carry LispString payload")
            .clone();
        let _ = eval.bookmarks.delete(&name);
    }
    Ok(Value::NIL)
}

/// (bookmark-rename OLD NEW) -> nil
pub(crate) fn builtin_bookmark_rename(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    if args.is_empty() || args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("bookmark-rename"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    // Batch-mode prompt fallbacks in GNU Emacs become end-of-file.
    if args.len() == 1 || args.get(1).is_some_and(|v| v.is_nil()) {
        return Err(signal(
            "end-of-file",
            vec![Value::string("Error reading from stdin")],
        ));
    }

    let old = &args[0];
    let new_name = &args[1];

    if old.is_string() {
        let old_name = old
            .as_lisp_string()
            .expect("ValueKind::String must carry LispString payload")
            .clone();
        if eval.bookmarks.get(&old_name).is_none() {
            return Err(signal(
                "error",
                vec![Value::string(format!(
                    "Invalid bookmark {}",
                    bookmark_string_to_runtime(&old_name)
                ))],
            ));
        }

        let target = match new_name.kind() {
            ValueKind::String => new_name
                .as_lisp_string()
                .expect("ValueKind::String must carry LispString payload")
                .clone(),
            _ => {
                return Err(signal(
                    "error",
                    vec![Value::string(format!(
                        "Invalid bookmark {}",
                        bookmark_string_to_runtime(&old_name)
                    ))],
                ));
            }
        };

        if eval.bookmarks.rename(&old_name, target) {
            return Ok(Value::NIL);
        }
        return Err(signal(
            "error",
            vec![Value::string(format!(
                "Invalid bookmark {}",
                bookmark_string_to_runtime(&old_name)
            ))],
        ));
    }

    if old.is_cons() {
        if new_name.is_string() {
            let name = new_name
                .as_lisp_string()
                .expect("ValueKind::String must carry LispString payload");
            return Err(signal(
                "error",
                vec![Value::string(format!(
                    "Invalid bookmark {}",
                    bookmark_string_to_runtime(name)
                ))],
            ));
        }
        return Ok(Value::NIL);
    }

    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("consp"), Value::NIL],
    ))
}

/// (bookmark-all-names) -> list of bookmark names (sorted)
#[cfg(test)]
pub(crate) fn builtin_bookmark_all_names(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("bookmark-all-names", &args, 0)?;
    let names: Vec<Value> = eval
        .bookmarks
        .all_names()
        .into_iter()
        .map(Value::heap_string)
        .collect();
    Ok(Value::list(names))
}

/// (bookmark-get-filename BOOKMARK) -> filename string or nil
///
/// BOOKMARK may be a bookmark name or a bookmark record alist.
#[cfg(test)]
pub(crate) fn builtin_bookmark_get_filename(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("bookmark-get-filename", &args, 1)?;

    if let Some(items) = super::value::list_to_vec(&args[0]) {
        for item in &items {
            if item.is_cons() {
                let pair_car = item.cons_car();
                let pair_cdr = item.cons_cdr();
                if let Some(id) = pair_car.as_symbol_id() {
                    if resolve_sym(id) == "filename" {
                        return Ok(pair_cdr);
                    }
                }
            }
        }
        return Ok(Value::NIL);
    }

    let name = expect_lisp_string(&args[0])?;
    let filename = eval
        .bookmarks
        .get(&name)
        .and_then(|bm| bm.filename.as_ref())
        .map(|s| Value::heap_string(s.clone()))
        .unwrap_or(Value::NIL);
    Ok(filename)
}

/// (bookmark-get-position BOOKMARK) -> integer position or nil
///
/// BOOKMARK may be a bookmark name or a bookmark record alist.
#[cfg(test)]
pub(crate) fn builtin_bookmark_get_position(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("bookmark-get-position", &args, 1)?;

    if let Some(items) = super::value::list_to_vec(&args[0]) {
        for item in &items {
            if item.is_cons() {
                let pair_car = item.cons_car();
                let pair_cdr = item.cons_cdr();
                if let Some(id) = pair_car.as_symbol_id() {
                    if resolve_sym(id) == "position" {
                        return Ok(pair_cdr);
                    }
                }
            }
        }
        return Ok(Value::NIL);
    }

    let name = expect_lisp_string(&args[0])?;
    let position = eval
        .bookmarks
        .get(&name)
        .map(|bm| Value::fixnum(bm.position as i64))
        .unwrap_or(Value::NIL);
    Ok(position)
}

/// (bookmark-get-annotation BOOKMARK) -> annotation string or nil
///
/// BOOKMARK may be a bookmark name or a bookmark record alist.
#[cfg(test)]
pub(crate) fn builtin_bookmark_get_annotation(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("bookmark-get-annotation", &args, 1)?;

    if let Some(items) = super::value::list_to_vec(&args[0]) {
        for item in &items {
            if item.is_cons() {
                let pair_car = item.cons_car();
                let pair_cdr = item.cons_cdr();
                if let Some(id) = pair_car.as_symbol_id() {
                    if resolve_sym(id) == "annotation" {
                        return Ok(pair_cdr);
                    }
                }
            }
        }
        return Ok(Value::NIL);
    }

    let name = expect_lisp_string(&args[0])?;
    let annotation = eval
        .bookmarks
        .get(&name)
        .and_then(|bm| bm.annotation.as_ref())
        .map(|s| Value::heap_string(s.clone()))
        .unwrap_or(Value::NIL);
    Ok(annotation)
}

/// (bookmark-set-annotation BOOKMARK ANNOTATION) -> annotation string or nil
///
/// BOOKMARK is a bookmark name.  If missing, returns nil.
#[cfg(test)]
pub(crate) fn builtin_bookmark_set_annotation(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("bookmark-set-annotation", &args, 2)?;
    let name = expect_lisp_string(&args[0])?;
    let annotation = if args[1].is_nil() {
        None
    } else {
        Some(expect_lisp_string(&args[1])?)
    };

    if let Some(mut bm) = eval.bookmarks.get(&name).cloned() {
        bm.annotation = annotation.clone();
        eval.bookmarks.set(name, bm);
        if let Some(value) = annotation {
            Ok(Value::heap_string(value))
        } else {
            Ok(Value::NIL)
        }
    } else {
        Ok(Value::NIL)
    }
}

fn default_bookmark_file() -> String {
    if let Ok(home) = std::env::var("HOME") {
        return format!("{home}/.config/emacs/bookmarks");
    }
    ".config/emacs/bookmarks".to_string()
}

fn active_bookmark_default_file(eval: &super::eval::Context) -> String {
    if let Some(v) = eval.obarray.symbol_value("bookmark-default-file") {
        if v.is_string() {
            return v
                .as_runtime_string_owned()
                .expect("ValueKind::String must carry LispString payload");
        }
    }
    default_bookmark_file()
}

fn bookmark_timestamp_file(eval: &super::eval::Context) -> Option<String> {
    let value = eval.obarray.symbol_value("bookmark-bookmarks-timestamp")?;
    if !value.is_cons() {
        return None;
    };
    let pair_car = value.cons_car();
    let _pair_cdr = value.cons_cdr();
    pair_car.as_runtime_string_owned()
}

fn bookmark_save_stamp(path: &str) -> Value {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Value::list(vec![
        Value::string(path.to_string()),
        Value::fixnum(now.as_secs() as i64),
        Value::fixnum(0),
        Value::fixnum(now.subsec_micros() as i64),
        Value::fixnum(0),
    ])
}

fn set_bookmark_timestamp(eval: &mut super::eval::Context, file: &str) {
    eval.obarray
        .set_symbol_value("bookmark-bookmarks-timestamp", bookmark_save_stamp(file));
}

/// (bookmark-save &optional PARG FILE BATCH) -> nil or save-stamp list
pub(crate) fn builtin_bookmark_save(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("bookmark-save"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    let parg = args.first().cloned().unwrap_or(Value::NIL);
    let file_arg = args.get(1).cloned().unwrap_or(Value::NIL);
    let batch = args.get(2).cloned().unwrap_or(Value::NIL);

    if !file_arg.is_nil() && !file_arg.is_string() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), file_arg],
        ));
    }

    let make_default = !batch.is_nil();

    // Mirror bookmark-maybe-load-default-file: if we have never tracked a
    // bookmark file in this session, and there are no in-memory bookmarks,
    // eagerly load the default file if it exists.
    let configured_default = active_bookmark_default_file(eval);
    if bookmark_timestamp_file(eval).is_none()
        && eval.bookmarks.all_names().is_empty()
        && Path::new(&configured_default).is_file()
    {
        let _ = builtin_bookmark_load(
            eval,
            vec![
                Value::string(configured_default.clone()),
                Value::T,
                Value::T,
            ],
        )?;
    }

    let path = if file_arg.is_string() {
        file_arg
            .as_runtime_string_owned()
            .expect("ValueKind::String must carry LispString payload")
    } else {
        if !parg.is_nil() {
            return Err(signal(
                "end-of-file",
                vec![Value::string("Error reading from stdin")],
            ));
        }
        bookmark_timestamp_file(eval).unwrap_or(configured_default)
    };

    let data = eval.bookmarks.save_to_string();
    if let Some(parent) = Path::new(&path).parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&path, data);
    eval.bookmarks.mark_saved();

    if make_default {
        set_bookmark_timestamp(eval, &path);
        return Ok(bookmark_save_stamp(&path));
    }

    if bookmark_timestamp_file(eval)
        .as_deref()
        .is_some_and(|default| default == path)
    {
        set_bookmark_timestamp(eval, &path);
        return Ok(bookmark_save_stamp(&path));
    }

    Ok(Value::NIL)
}

/// (bookmark-load FILE &optional OVERWRITE NO-MSG BATCH) -> message string or nil
pub(crate) fn builtin_bookmark_load(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    if args.is_empty() || args.len() > 4 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("bookmark-load"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    let file = match args[0].kind() {
        ValueKind::String => args[0]
            .as_runtime_string_owned()
            .expect("ValueKind::String must carry LispString payload"),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), args[0]],
            ));
        }
    };

    let data = match fs::read_to_string(&file) {
        Ok(data) => data,
        Err(_) => {
            return Err(signal(
                "user-error",
                vec![Value::string(format!("Cannot read bookmark file {file}"))],
            ));
        }
    };

    eval.bookmarks.load_from_string(&data);

    let current_default =
        bookmark_timestamp_file(eval).unwrap_or_else(|| active_bookmark_default_file(eval));
    let set_default = args.get(3).is_some_and(|v| !v.is_nil()) || file == current_default;
    if set_default {
        set_bookmark_timestamp(eval, &file);
    }

    let no_msg = args.get(2).is_some_and(|v| !v.is_nil());
    if no_msg {
        return Ok(Value::NIL);
    }
    Ok(Value::string(format!(
        "Loading bookmarks from {file}...done"
    )))
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
#[path = "bookmark_test.rs"]
mod tests;
