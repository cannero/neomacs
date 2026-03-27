//! Documentation and help support builtins.
//!
//! Provides:
//! - `documentation` — retrieve docstring from a function
//! - `documentation-property` — retrieve documentation property
//! - `Snarf-documentation` — internal DOC file loader compatibility shim
//! - `substitute-command-keys` — process special sequences in docstrings

use super::error::{EvalResult, Flow, signal};
use super::intern::{intern, resolve_sym};
use super::value::*;
use std::fs::File;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

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

fn expect_min_max_args(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), Flow> {
    if args.len() < min || args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Eval-dependent builtins
// ---------------------------------------------------------------------------

/// `(documentation FUNCTION &optional RAW)` -- return the docstring of FUNCTION.
///
/// Looks up FUNCTION in the obarray's function cell. If the function cell
/// holds a `Lambda` (or `Macro`) with a docstring, returns it as a string.
/// Otherwise returns nil.  Unless RAW is non-nil, string results are passed
/// through `substitute-command-keys`, matching GNU Emacs.
pub(crate) fn builtin_documentation(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let raw = args.get(1).is_some_and(Value::is_truthy);
    let obarray = eval.obarray() as *const super::symbol::Obarray;
    // Safety: the evaluator owns the obarray for the duration of this call.
    let plan = documentation_plan(unsafe { &*obarray }, args)?;
    finish_documentation_result(
        execute_documentation_plan(plan, |value| eval.eval_value(&value))?,
        raw,
    )
}

enum DocumentationPlan {
    Final(Value),
    Eval(Value),
}

fn execute_documentation_plan(
    plan: DocumentationPlan,
    mut eval_value: impl FnMut(Value) -> EvalResult,
) -> EvalResult {
    match plan {
        DocumentationPlan::Final(value) => Ok(value),
        DocumentationPlan::Eval(value) => eval_value(value),
    }
}

fn finish_documentation_result(value: Value, raw: bool) -> EvalResult {
    if raw || !matches!(value, Value::Str(_)) {
        Ok(value)
    } else {
        builtin_substitute_command_keys(vec![value])
    }
}

fn documentation_plan(
    obarray: &super::symbol::Obarray,
    args: Vec<Value>,
) -> Result<DocumentationPlan, Flow> {
    expect_min_max_args("documentation", &args, 1, 2)?;
    let lisp_directory = obarray
        .symbol_value("lisp-directory")
        .and_then(Value::as_str_owned);

    // For symbols, Emacs consults the `function-documentation` property first.
    // This can produce docs even when the function cell is non-callable.
    if let Some(name) = args[0].as_symbol_name() {
        let name = name.to_string();
        if let Some(prop) = obarray
            .get_property(&name, "function-documentation")
            .cloned()
        {
            return documentation_plan_from_property_value(lisp_directory.as_deref(), prop);
        }

        let mut func_val = super::builtins::symbols::symbol_function_impl(
            obarray,
            vec![Value::symbol(name.clone())],
        )?;
        if let Some(alias_name) = func_val.as_symbol_name() {
            let indirect = super::builtins::symbols::indirect_function_impl(
                obarray,
                vec![Value::symbol(alias_name)],
            )?;
            if !indirect.is_nil() {
                func_val = indirect;
            }
        }
        if func_val.is_nil() {
            return Err(signal("void-function", vec![Value::symbol(name)]));
        }

        return function_doc_or_error(func_val).map(DocumentationPlan::Final);
    }

    function_doc_or_error(args[0]).map(DocumentationPlan::Final)
}

pub(crate) fn builtin_documentation_in_vm_runtime(
    shared: &mut super::eval::Context,
    vm_gc_roots: &[Value],
    args: Vec<Value>,
) -> EvalResult {
    let raw = args.get(1).is_some_and(Value::is_truthy);
    let args_roots = args.clone();
    let plan = documentation_plan(&shared.obarray, args)?;
    finish_documentation_result(
        execute_documentation_plan(plan, |value| {
            let mut extra_roots = args_roots.clone();
            extra_roots.push(value);
            shared.with_extra_gc_roots(vm_gc_roots, &extra_roots, move |eval| {
                eval.eval_value(&value)
            })
        })?,
        raw,
    )
}

fn function_doc_or_error(func_val: Value) -> EvalResult {
    if let Some(result) = quoted_lambda_documentation(&func_val) {
        return result;
    }
    if let Some(result) = quoted_macro_invalid_designator(&func_val) {
        return result;
    }

    match func_val {
        Value::Lambda(_) | Value::Macro(_) => {
            let data = func_val.get_lambda_data().unwrap();
            match &data.docstring {
                Some(doc) => Ok(Value::string(doc.clone())),
                None => Ok(Value::Nil),
            }
        }
        Value::Subr(id) => Ok(Value::string(
            subr_documentation_stub(resolve_sym(id)).unwrap_or("Built-in function."),
        )),
        Value::Str(_) | Value::Vector(_) => Ok(Value::string("Keyboard macro.")),
        Value::ByteCode(_) => {
            let bc = func_val.get_bytecode_data().unwrap();
            Ok(bc
                .docstring
                .as_ref()
                .map_or(Value::Nil, |doc| Value::string(doc.clone())))
        }
        other => Err(signal("invalid-function", vec![other])),
    }
}

fn subr_documentation_stub(name: &str) -> Option<&'static str> {
    match name {
        "car" => Some(
            "Return the car of LIST.  If LIST is nil, return nil.\n\
Error if LIST is not nil and not a cons cell.  See also ‘car-safe’.\n\
\n\
See Info node ‘(elisp)Cons Cells’ for a discussion of related basic\n\
Lisp concepts such as car, cdr, cons cell and list.\n\
\n\
(fn LIST)",
        ),
        "cdr" => Some(
            "Return the cdr of LIST.  If LIST is nil, return nil.\n\
Error if LIST is not nil and not a cons cell.  See also ‘cdr-safe’.\n\
\n\
See Info node ‘(elisp)Cons Cells’ for a discussion of related basic\n\
Lisp concepts such as cdr, car, cons cell and list.\n\
\n\
(fn LIST)",
        ),
        "cons" => Some("Create a new cons, give it CAR and CDR as components, and return it."),
        "list" => Some("Return a newly created list with specified arguments as elements."),
        "eq" => Some("Return t if the two args are the same Lisp object."),
        "equal" => Some("Return t if two Lisp objects have similar structure and contents."),
        "length" => Some("Return the length of vector, list or string SEQUENCE."),
        "append" => Some("Concatenate all the arguments and make the result a list."),
        "mapcar" => {
            Some("Apply FUNCTION to each element of SEQUENCE, and make a list of the results.")
        }
        "assoc" => Some("Return non-nil if KEY is equal to the car of an element of ALIST."),
        "member" => {
            Some("Return non-nil if ELT is an element of LIST.  Comparison done with ‘equal’.")
        }
        "symbol-name" => Some("Return SYMBOL’s name, a string."),
        "if" => Some(
            "If COND yields non-nil, do THEN, else do ELSE...\n\
Returns the value of THEN or the value of the last of the ELSE’s.\n\
THEN must be one expression, but ELSE... can be zero or more expressions.\n\
If COND yields nil, and there are no ELSE’s, the value is nil.\n\
\n\
(fn COND THEN ELSE...)",
        ),
        _ => None,
    }
}

fn quoted_lambda_documentation(function: &Value) -> Option<EvalResult> {
    let Value::Cons(cell) = function else {
        return None;
    };

    let pair = read_cons(*cell);
    if pair.car.as_symbol_name() != Some("lambda") {
        return None;
    }

    let mut tail = pair.cdr;

    let Value::Cons(param_cell) = tail else {
        return Some(Err(signal("invalid-function", vec![*function])));
    };
    let params_and_body = read_cons(param_cell);
    tail = params_and_body.cdr;

    match tail {
        Value::Nil => Some(Ok(Value::Nil)),
        Value::Cons(body_cell) => {
            let body = read_cons(body_cell);
            if let Some(doc) = body.car.as_str() {
                Some(Ok(Value::string(doc)))
            } else {
                Some(Ok(Value::Nil))
            }
        }
        other => Some(Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), other],
        ))),
    }
}

fn quoted_macro_invalid_designator(function: &Value) -> Option<EvalResult> {
    let Value::Cons(cell) = function else {
        return None;
    };

    let pair = read_cons(*cell);
    if pair.car.as_symbol_name() != Some("macro") {
        return None;
    }

    let payload = pair.cdr;
    if payload.is_nil() {
        return Some(Err(signal("void-function", vec![Value::Nil])));
    }

    // GNU extracts the docstring from the function part of (macro . fn),
    // rather than signaling invalid-function.
    Some(function_doc_or_error(payload))
}

fn documentation_plan_from_property_value(
    lisp_directory: Option<&str>,
    value: Value,
) -> Result<DocumentationPlan, Flow> {
    if let Some(text) = value.as_str() {
        return Ok(DocumentationPlan::Final(Value::string(text)));
    }

    if let Some((file, position)) = compiled_doc_ref(&value) {
        return load_compiled_doc_string(lisp_directory, &file, position)
            .map(DocumentationPlan::Final);
    }

    // Integer doc offsets require DOC-file lookup; return nil when unresolved.
    if matches!(value, Value::Int(_)) {
        return Ok(DocumentationPlan::Final(Value::Nil));
    }

    Ok(DocumentationPlan::Eval(value))
}

fn compiled_doc_ref(value: &Value) -> Option<(String, i64)> {
    let Value::Cons(cell) = value else {
        return None;
    };
    let pair = read_cons(*cell);
    Some((pair.car.as_str_owned()?, pair.cdr.as_int()?))
}

fn resolve_compiled_doc_path(lisp_directory: Option<&str>, file: &str) -> PathBuf {
    let path = Path::new(file);
    if path.is_absolute() {
        return path.to_path_buf();
    }

    if let Some(dir) = lisp_directory {
        return Path::new(dir).join(path);
    }

    path.to_path_buf()
}

fn compiled_doc_prefix_is_valid(prefix: &[u8]) -> bool {
    if prefix.is_empty() {
        return false;
    }

    let mut test = 1_usize;
    if prefix[prefix.len() - test] == 0x1f {
        return true;
    }
    if prefix[prefix.len() - test] != b' ' {
        return false;
    }
    test += 1;
    while prefix.len() >= test && prefix[prefix.len() - test].is_ascii_digit() {
        test += 1;
    }
    if prefix.len() < test || prefix[prefix.len() - test] != b'@' {
        return false;
    }
    test += 1;
    prefix.len() >= test && prefix[prefix.len() - test] == b'#'
}

fn decode_compiled_doc_bytes(bytes: &[u8]) -> EvalResult {
    let mut out = Vec::with_capacity(bytes.len());
    let mut pos = 0_usize;
    while pos < bytes.len() {
        if bytes[pos] != 0x01 {
            out.push(bytes[pos]);
            pos += 1;
            continue;
        }

        pos += 1;
        let Some(&escaped) = bytes.get(pos) else {
            return Err(signal(
                "error",
                vec![Value::string(
                    "Invalid data in documentation file -- dangling ^A escape",
                )],
            ));
        };
        match escaped {
            0x01 => out.push(0x01),
            b'0' => out.push(0x00),
            b'_' => out.push(0x1f),
            other => {
                return Err(signal(
                    "error",
                    vec![Value::string(format!(
                        "Invalid data in documentation file -- ^A followed by code {:03o}",
                        other
                    ))],
                ));
            }
        }
        pos += 1;
    }

    Ok(Value::string(super::load::decode_emacs_utf8(&out)))
}

fn load_compiled_doc_string(lisp_directory: Option<&str>, file: &str, position: i64) -> EvalResult {
    let position = position.unsigned_abs();
    let resolved = resolve_compiled_doc_path(lisp_directory, file);
    let mut handle = match File::open(&resolved) {
        Ok(file_handle) => file_handle,
        Err(err) if matches!(err.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Ok(Value::string(format!(
                "Cannot open doc string file \"{file}\"\n"
            )));
        }
        Err(err) => {
            return Err(signal(
                "file-error",
                vec![
                    Value::string("Read error on documentation file"),
                    Value::string(format!("{}: {}", resolved.display(), err)),
                ],
            ));
        }
    };

    let prefix_len = usize::try_from(position.min(1024)).unwrap_or(1024);
    let start = position.saturating_sub(prefix_len as u64);
    handle.seek(SeekFrom::Start(start)).map_err(|_| {
        signal(
            "error",
            vec![Value::string(format!(
                "Position {position} out of range in doc string file \"{file}\""
            ))],
        )
    })?;

    let offset = prefix_len;
    let mut buffer = Vec::with_capacity(prefix_len + 8192);
    let mut chunk = [0_u8; 8192];
    let end_index = loop {
        let read = handle.read(&mut chunk).map_err(|err| {
            signal(
                "file-error",
                vec![
                    Value::string("Read error on documentation file"),
                    Value::string(format!("{}: {}", resolved.display(), err)),
                ],
            )
        })?;
        if read == 0 {
            break None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > offset
            && let Some(pos) = buffer[offset..].iter().position(|&byte| byte == 0x1f)
        {
            break Some(offset + pos);
        }
    };

    let Some(end_index) = end_index else {
        return Ok(Value::Nil);
    };

    if offset == 0 || buffer.len() < offset || !compiled_doc_prefix_is_valid(&buffer[..offset]) {
        return Ok(Value::Nil);
    }

    decode_compiled_doc_bytes(&buffer[offset..end_index])
}

fn startup_variable_doc_offset_symbol(sym: &str, prop: &str, value: &Value) -> bool {
    prop == "variable-documentation"
        && matches!(value, Value::Int(_))
        && startup_variable_doc_stub(sym).is_some()
}

pub(crate) static STARTUP_VARIABLE_DOC_STUBS: &[(&str, &str)] = &[
    ("abbrev-mode", "Non-nil if Abbrev mode is enabled."),
    (
        "after-change-functions",
        "List of functions to call after each text change.",
    ),
    (
        "after-delete-frame-functions",
        "Functions run after deleting a frame.",
    ),
    (
        "after-init-time",
        "Value of `current-time' after loading the init files.",
    ),
    (
        "after-insert-file-functions",
        "A list of functions to be called at the end of `insert-file-contents'.",
    ),
    (
        "after-load-alist",
        "An alist of functions to be evalled when particular files are loaded.",
    ),
    (
        "alternate-fontname-alist",
        "Alist of fontname vs list of the alternate fontnames.",
    ),
    (
        "ambiguous-width-chars",
        "A char-table for characters whose width (columns) can be 1 or 2.",
    ),
    (
        "attempt-orderly-shutdown-on-fatal-signal",
        "If non-nil, attempt orderly shutdown on fatal signals.",
    ),
    (
        "attempt-stack-overflow-recovery",
        "If non-nil, attempt to recover from C stack overflows.",
    ),
    (
        "auto-composition-emoji-eligible-codepoints",
        "List of codepoints for which auto-composition will check for an emoji font.",
    ),
    (
        "auto-composition-function",
        "Function to call to compose characters automatically.",
    ),
    (
        "auto-composition-mode",
        "Non-nil if Auto-Composition mode is enabled.",
    ),
    (
        "auto-fill-function",
        "Function called (if non-nil) to perform auto-fill.",
    ),
    (
        "auto-fill-chars",
        "A char-table for characters which invoke auto-filling.",
    ),
    (
        "auto-hscroll-mode",
        "Allow or disallow automatic horizontal scrolling of windows.",
    ),
    (
        "auto-raise-tab-bar-buttons",
        "Non-nil means raise tab-bar buttons when the mouse moves over them.",
    ),
    (
        "auto-raise-tool-bar-buttons",
        "Non-nil means raise tool-bar buttons when the mouse moves over them.",
    ),
    (
        "auto-resize-tab-bars",
        "Non-nil means automatically resize tab-bars.",
    ),
    (
        "auto-resize-tool-bars",
        "Non-nil means automatically resize tool-bars.",
    ),
    (
        "auto-save-include-big-deletions",
        "If non-nil, auto-save even if a large part of the text is deleted.",
    ),
    (
        "auto-save-interval",
        "Number of input events between auto-saves.",
    ),
    (
        "auto-save-list-file-name",
        "File name in which to write a list of all auto save file names.",
    ),
    (
        "auto-save-no-message",
        "Non-nil means do not print any message when auto-saving.",
    ),
    (
        "auto-save-timeout",
        "Number of seconds idle time before auto-save.",
    ),
    (
        "auto-save-visited-file-name",
        "Non-nil says auto-save a buffer in the file it is visiting, when practical.",
    ),
    (
        "auto-window-vscroll",
        "Non-nil means to automatically adjust `window-vscroll' to view tall lines.",
    ),
    (
        "backtrace-on-error-noninteractive",
        "Non-nil means print backtrace on error in batch mode.",
    ),
    (
        "backtrace-on-redisplay-error",
        "Non-nil means create a backtrace if a lisp error occurs in redisplay.",
    ),
    ("baud-rate", "The output baud rate of the terminal."),
    (
        "before-change-functions",
        "List of functions to call before each text change.",
    ),
    (
        "before-init-time",
        "Value of `current-time' before Emacs begins initialization.",
    ),
    (
        "binary-as-unsigned",
        "Non-nil means `format' %x and %o treat integers as unsigned.",
    ),
    (
        "bidi-display-reordering",
        "Non-nil means reorder bidirectional text for display in the visual order.",
    ),
    (
        "bidi-inhibit-bpa",
        "Non-nil means inhibit the Bidirectional Parentheses Algorithm.",
    ),
    (
        "bidi-paragraph-direction",
        "If non-nil, forces directionality of text paragraphs in the buffer.",
    ),
    (
        "bidi-paragraph-separate-re",
        "If non-nil, a regexp matching a line that separates paragraphs.",
    ),
    (
        "bidi-paragraph-start-re",
        "If non-nil, a regexp matching a line that starts OR separates paragraphs.",
    ),
    (
        "blink-cursor-alist",
        "Alist specifying how to blink the cursor off.",
    ),
    (
        "buffer-access-fontified-property",
        "Property which (if non-nil) indicates text has been fontified.",
    ),
    (
        "buffer-access-fontify-functions",
        "List of functions called by `buffer-substring' to fontify if necessary.",
    ),
    (
        "buffer-auto-save-file-format",
        "Format in which to write auto-save files.",
    ),
    (
        "buffer-auto-save-file-name",
        "Name of file for auto-saving current buffer.",
    ),
    (
        "buffer-backed-up",
        "Non-nil if this buffer's file has been backed up.",
    ),
    (
        "buffer-display-count",
        "A number incremented each time this buffer is displayed in a window.",
    ),
    (
        "buffer-display-table",
        "Display table that controls display of the contents of current buffer.",
    ),
    (
        "buffer-display-time",
        "Time stamp updated each time this buffer is displayed in a window.",
    ),
    (
        "buffer-file-coding-system",
        "Coding system to be used for encoding the buffer contents on saving.",
    ),
    (
        "buffer-file-format",
        "List of formats to use when saving this buffer.",
    ),
    (
        "buffer-file-name",
        "Name of file visited in current buffer, or nil if not visiting a file.",
    ),
    (
        "buffer-file-truename",
        "Abbreviated truename of file visited in current buffer, or nil if none.",
    ),
    (
        "buffer-invisibility-spec",
        "Invisibility spec of this buffer.",
    ),
    (
        "buffer-list-update-hook",
        "Hook run when the buffer list changes.",
    ),
    ("buffer-read-only", "Non-nil if this buffer is read-only."),
    (
        "buffer-saved-size",
        "Length of current buffer when last read in, saved or auto-saved.",
    ),
    (
        "build-files",
        "A list of files used to build this Emacs binary.",
    ),
    (
        "byte-boolean-vars",
        "List of all DEFVAR_BOOL variables, used by the byte code optimizer.",
    ),
    (
        "bytecomp-version-regexp",
        "Regular expression matching safe to load compiled Lisp files.",
    ),
    (
        "buffer-undo-list",
        "List of undo entries in current buffer.",
    ),
    (
        "cache-long-scans",
        "Non-nil means that Emacs should use caches in attempt to speedup buffer scans.",
    ),
    ("cairo-version-string", "Version info for cairo."),
    (
        "cannot-suspend",
        "Non-nil means to always spawn a subshell instead of suspending.",
    ),
    (
        "case-fold-search",
        "Non-nil if searches and matches should ignore case.",
    ),
    (
        "case-symbols-as-words",
        "If non-nil, case functions treat symbol syntax as part of words.",
    ),
    (
        "change-major-mode-hook",
        "Normal hook run before changing the major mode of a buffer.",
    ),
    (
        "char-code-property-alist",
        "Alist of character property name vs char-table containing property values.",
    ),
    (
        "char-property-alias-alist",
        "Alist of alternative properties for properties without a value.",
    ),
    ("char-script-table", "Char table of script symbols."),
    (
        "char-width-table",
        "A char-table for width (columns) of each character.",
    ),
    ("charset-list", "List of all charsets ever defined."),
    (
        "charset-map-path",
        "List of directories to search for charset map files.",
    ),
    (
        "charset-revision-table",
        "Alist of charsets vs revision numbers.",
    ),
    (
        "clear-message-function",
        "If non-nil, function to clear echo-area messages.",
    ),
    (
        "clone-indirect-buffer-hook",
        "Normal hook to run in the new buffer at the end of `make-indirect-buffer'.",
    ),
    (
        "code-conversion-map-vector",
        "Vector of code conversion maps.",
    ),
    (
        "coding-category-list",
        "List of coding-categories (symbols) ordered by priority.",
    ),
    ("coding-system-alist", "Alist of coding system names."),
    (
        "coding-system-for-read",
        "Specify the coding system for read operations.",
    ),
    (
        "coding-system-for-write",
        "Specify the coding system for write operations.",
    ),
    ("coding-system-list", "List of coding systems."),
    ("coding-system-require-warning", "Internal use only."),
    (
        "combine-after-change-calls",
        "Used internally by the function `combine-after-change-calls' macro.",
    ),
    (
        "comment-use-syntax-ppss",
        "Non-nil means `forward-comment' can use `syntax-ppss' internally.",
    ),
    ("comp-abi-hash", "String signing the .eln files ABI."),
    ("comp-ctxt", "The compiler context."),
    (
        "comp-deferred-pending-h",
        "Hash table symbol-name -> function-value.",
    ),
    (
        "comp-eln-to-el-h",
        "Hash table eln-filename -> el-filename.",
    ),
    (
        "comp-file-preloaded-p",
        "When non-nil, assume the file being compiled to be preloaded.",
    ),
    (
        "comp-installed-trampolines-h",
        "Hash table subr-name -> installed trampoline.",
    ),
    (
        "comp-loaded-comp-units-h",
        "Hash table recording all loaded compilation units, file -> CU.",
    ),
    (
        "comp-native-version-dir",
        "Directory in use to disambiguate eln compatibility.",
    ),
    (
        "comp-no-native-file-h",
        "Files for which no deferred compilation should be performed.",
    ),
    (
        "comp-sanitizer-active",
        "If non-nil, enable runtime execution of native-compiler sanitizer.",
    ),
    (
        "comp-subr-arities-h",
        "Hash table recording the arity of Lisp primitives.",
    ),
    ("comp-subr-list", "List of all defined subrs."),
    (
        "compose-chars-after-function",
        "Function to adjust composition of buffer text.",
    ),
    (
        "composition-break-at-point",
        "If non-nil, prevent auto-composition of characters around point.",
    ),
    (
        "composition-function-table",
        "Char-table of functions for automatic character composition.",
    ),
    (
        "configure-info-directory",
        "For internal use by the build procedure only.",
    ),
    (
        "cons-cells-consed",
        "Number of cons cells that have been consed so far.",
    ),
    (
        "create-lockfiles",
        "Non-nil means use lockfiles to avoid editing collisions.",
    ),
    (
        "cross-disabled-images",
        "Non-nil means always draw a cross over disabled images.",
    ),
    (
        "ctags-program-name",
        "Name of the `ctags' program distributed with Emacs.",
    ),
    (
        "ctl-arrow",
        "Non-nil means display control chars with uparrow '^'.",
    ),
    (
        "command-debug-status",
        "Debugging status of current interactive command.",
    ),
    (
        "command-error-function",
        "Function to output error messages.",
    ),
    (
        "command-history",
        "List of recent commands that read arguments from terminal.",
    ),
    (
        "command-line-args",
        "Args passed by shell to Emacs, as a list of strings.",
    ),
    (
        "comment-end-can-be-escaped",
        "Non-nil means an escaped ender inside a comment doesn't end the comment.",
    ),
    (
        "completion-ignore-case",
        "Non-nil means don't consider case significant in completion.",
    ),
    (
        "completion-ignored-extensions",
        "Completion ignores file names ending in any string in this list.",
    ),
    (
        "completion-regexp-list",
        "List of regexps that should restrict possible completions.",
    ),
    (
        "current-iso639-language",
        "ISO639 language mnemonic symbol for the current language environment.",
    ),
    (
        "current-key-remap-sequence",
        "The key sequence currently being remap, or nil.",
    ),
    ("current-load-list", "Used for internal purposes by `load'."),
    (
        "current-minibuffer-command",
        "This is like `this-command`, but bound recursively.",
    ),
    (
        "current-prefix-arg",
        "The value of the prefix argument for this editing command.",
    ),
    (
        "current-time-list",
        "Whether `current-time' should return list or (TICKS . HZ) form.",
    ),
    (
        "cursor-in-echo-area",
        "Non-nil means put cursor in minibuffer, at end of any message there.",
    ),
    (
        "cursor-in-non-selected-windows",
        "Non-nil means show a cursor in non-selected windows.",
    ),
    (
        "cursor-type",
        "Cursor to use when this buffer is in the selected window.",
    ),
    (
        "data-directory",
        "Directory of machine-independent files that come with GNU Emacs.",
    ),
    (
        "dbus-compiled-version",
        "The version of D-Bus Emacs is compiled against.",
    ),
    (
        "dbus-debug",
        "If non-nil, debug messages of D-Bus bindings are raised.",
    ),
    (
        "dbus-message-type-error",
        "Message type of an error reply message.",
    ),
    (
        "dbus-message-type-invalid",
        "This value is never a valid message type.",
    ),
    (
        "dbus-message-type-method-call",
        "Message type of a method call message.",
    ),
    (
        "dbus-message-type-method-return",
        "Message type of a method return message.",
    ),
    (
        "dbus-message-type-signal",
        "Message type of a signal message.",
    ),
    (
        "dbus-registered-objects-table",
        "Hash table of registered functions for D-Bus.",
    ),
    (
        "dbus-runtime-version",
        "The version of D-Bus Emacs runs with.",
    ),
    (
        "deactivate-mark",
        "Whether to deactivate the mark after an editing command.",
    ),
    (
        "debug-ignored-errors",
        "List of errors for which the debugger should not be called.",
    ),
    ("debug-on-event", "Enter debugger on this event."),
    (
        "debug-on-message",
        "If non-nil, debug if a message matching this regexp is displayed.",
    ),
    (
        "debug-on-next-call",
        "Non-nil means enter debugger before next `eval', `apply' or `funcall'.",
    ),
    (
        "debug-on-error",
        "Non-nil means enter debugger if an error is signaled.",
    ),
    (
        "debug-on-quit",
        "Non-nil means enter debugger if quit is signaled (C-g, for example).",
    ),
    (
        "debug-on-signal",
        "Non-nil means call the debugger regardless of condition handlers.",
    ),
    ("debugger", "Function to call to invoke debugger."),
    (
        "debugger-may-continue",
        "Non-nil means debugger may continue execution.",
    ),
    (
        "debugger-stack-frame-as-list",
        "Non-nil means display call stack frames as lists.",
    ),
    (
        "default-directory",
        "Name of default directory of current buffer.",
    ),
    (
        "default-frame-alist",
        "Alist of default values of frame parameters for frame creation.",
    ),
    (
        "default-frame-scroll-bars",
        "Default position of vertical scroll bars on this window-system.",
    ),
    (
        "default-file-name-coding-system",
        "Default coding system for encoding file names.",
    ),
    (
        "default-minibuffer-frame",
        "Minibuffer-less frames by default use this frame's minibuffer.",
    ),
    (
        "default-process-coding-system",
        "Cons of coding systems used for process I/O by default.",
    ),
    (
        "default-text-properties",
        "Property-list used as default values.",
    ),
    (
        "defining-kbd-macro",
        "Non-nil while a keyboard macro is being defined.  Don't set this!",
    ),
    (
        "delayed-warnings-list",
        "List of warnings to be displayed after this command.",
    ),
    (
        "delete-auto-save-files",
        "Non-nil means delete auto-save file when a buffer is saved.",
    ),
    (
        "delete-by-moving-to-trash",
        "Specifies whether to use the system's trash can.",
    ),
    (
        "delete-exited-processes",
        "Non-nil means delete processes immediately when they exit.",
    ),
    (
        "delete-frame-functions",
        "Functions run before deleting a frame.",
    ),
    (
        "delete-terminal-functions",
        "Special hook run when a terminal is deleted.",
    ),
    (
        "describe-bindings-check-shadowing-in-ranges",
        "If non-nil, consider command shadowing when describing ranges of keys.",
    ),
    (
        "disable-ascii-optimization",
        "If non-nil, Emacs does not optimize code decoder for ASCII files.",
    ),
    (
        "disable-inhibit-text-conversion",
        "Don't disable text conversion inside `read-key-sequence`.",
    ),
    (
        "disable-point-adjustment",
        "If non-nil, suppress point adjustment after executing a command.",
    ),
    (
        "display-fill-column-indicator",
        "Non-nil means display the fill column indicator.",
    ),
    (
        "display-fill-column-indicator-character",
        "Character to draw the indicator when `display-fill-column-indicator` is non-nil.",
    ),
    (
        "display-fill-column-indicator-column",
        "Column for indicator when `display-fill-column-indicator` is non-nil.",
    ),
    (
        "display-hourglass",
        "Non-nil means show an hourglass pointer, when Emacs is busy.",
    ),
    (
        "display-line-numbers-current-absolute",
        "Non-nil means display absolute number of current line.",
    ),
    (
        "display-line-numbers-major-tick",
        "If an integer N > 0, highlight line number of every Nth line.",
    ),
    (
        "display-line-numbers-minor-tick",
        "If an integer N > 0, highlight line number of every Nth line.",
    ),
    (
        "display-line-numbers-offset",
        "A signed integer added to each absolute line number.",
    ),
    (
        "display-line-numbers-widen",
        "Non-nil means display line numbers disregarding any narrowing.",
    ),
    (
        "display-line-numbers-width",
        "Minimum width of space reserved for line number display.",
    ),
    (
        "display-monitors-changed-functions",
        "Abnormal hook run when the monitor configuration changes.",
    ),
    (
        "display-pixels-per-inch",
        "Pixels per inch value for non-window system displays.",
    ),
    (
        "display-raw-bytes-as-hex",
        "Non-nil means display raw bytes in hexadecimal format.",
    ),
    (
        "display-line-numbers",
        "Non-nil means display line numbers.",
    ),
    (
        "doc-directory",
        "Directory containing the DOC file that comes with GNU Emacs.",
    ),
    (
        "double-click-fuzz",
        "Maximum mouse movement between clicks to make a double-click.",
    ),
    (
        "double-click-time",
        "Maximum time between mouse clicks to make a double-click.",
    ),
    ("dump-mode", "Non-nil when Emacs is dumping itself."),
    (
        "dynamic-library-alist",
        "Alist of dynamic libraries vs external files implementing them.",
    ),
    (
        "dynamic-library-suffixes",
        "A list of suffixes for loadable dynamic libraries.",
    ),
    (
        "ebrowse-program-name",
        "Name of the `ebrowse` program distributed with Emacs.",
    ),
    (
        "echo-area-clear-hook",
        "Normal hook run when clearing the echo area.",
    ),
    (
        "echo-keystrokes",
        "Nonzero means echo unfinished commands after this many seconds of pause.",
    ),
    (
        "echo-keystrokes-help",
        "Whether to append help text to echoed commands.",
    ),
    (
        "emacs-copyright",
        "Short copyright string for this version of Emacs.",
    ),
    ("emacs-version", "Version numbers of this version of Emacs."),
    (
        "emacsclient-program-name",
        "Program name for an external `emacsclient` helper, if available.",
    ),
    (
        "emulation-mode-map-alists",
        "List of keymap alists to use for emulation modes.",
    ),
    (
        "enable-character-translation",
        "Non-nil enables character translation while encoding and decoding.",
    ),
    (
        "enable-disabled-menus-and-buttons",
        "If non-nil, don't ignore events produced by disabled menu items and tool-bar.",
    ),
    (
        "enable-multibyte-characters",
        "Non-nil means the buffer contents are regarded as multi-byte characters.",
    ),
    (
        "enable-recursive-minibuffers",
        "Non-nil means to allow minibuffer commands while in the minibuffer.",
    ),
    (
        "eol-mnemonic-dos",
        "String displayed in mode line for DOS-like (CRLF) end-of-line format.",
    ),
    (
        "eol-mnemonic-mac",
        "String displayed in mode line for MAC-like (CR) end-of-line format.",
    ),
    (
        "eol-mnemonic-undecided",
        "String displayed in mode line when end-of-line format is not yet determined.",
    ),
    (
        "eol-mnemonic-unix",
        "String displayed in mode line for UNIX-like (LF) end-of-line format.",
    ),
    (
        "etags-program-name",
        "Program name for an external `etags` helper, if available.",
    ),
    (
        "eval-buffer-list",
        "List of buffers being read from by calls to `eval-buffer` and `eval-region`.",
    ),
    (
        "exec-directory",
        "Directory for executables for Emacs to invoke.",
    ),
    (
        "executing-kbd-macro",
        "Currently executing keyboard macro (string or vector).",
    ),
    (
        "executing-kbd-macro-index",
        "Index in currently executing keyboard macro; undefined if none executing.",
    ),
    (
        "exec-path",
        "List of directories to search programs to run in subprocesses.",
    ),
    (
        "exec-suffixes",
        "List of suffixes to try to find executable file names.",
    ),
    (
        "extra-keyboard-modifiers",
        "A mask of additional modifier keys to use with every keyboard character.",
    ),
    (
        "face--new-frame-defaults",
        "Hash table of global face definitions (for internal use only.)",
    ),
    (
        "face-default-stipple",
        "Default stipple pattern used on monochrome displays.",
    ),
    (
        "face-filters-always-match",
        "Non-nil means that face filters are always deemed to match.",
    ),
    (
        "face-font-lax-matched-attributes",
        "Whether to match some face attributes in lax manner when realizing faces.",
    ),
    (
        "face-font-rescale-alist",
        "Alist of fonts vs the rescaling factors.",
    ),
    ("face-ignored-fonts", "List of ignored fonts."),
    (
        "face-near-same-color-threshold",
        "Threshold for using distant-foreground color instead of foreground.",
    ),
    ("face-remapping-alist", "Alist of face remappings."),
    (
        "fast-but-imprecise-scrolling",
        "When non-nil, accelerate scrolling operations.",
    ),
    (
        "fast-read-process-output",
        "Non-nil to optimize the insertion of process output.",
    ),
    (
        "features",
        "A list of symbols which are the features of the executing Emacs.",
    ),
    (
        "file-coding-system-alist",
        "Alist to decide a coding system to use for a file I/O operation.",
    ),
    (
        "file-name-coding-system",
        "Coding system for encoding file names.",
    ),
    (
        "file-name-handler-alist",
        "Alist of elements (REGEXP . HANDLER) for file names handled specially.",
    ),
    (
        "fill-column",
        "Column beyond which automatic line-wrapping should happen.",
    ),
    (
        "find-word-boundary-function-table",
        "Char table of functions to search for the word boundary.",
    ),
    (
        "first-change-hook",
        "A list of functions to call before changing a buffer which is unmodified.",
    ),
    (
        "float-output-format",
        "The format descriptor string used to print floats.",
    ),
    (
        "floats-consed",
        "Number of floats that have been consed so far.",
    ),
    (
        "focus-follows-mouse",
        "Non-nil if window system changes focus when you move the mouse.",
    ),
    (
        "font-ccl-encoder-alist",
        "Alist of fontname patterns vs corresponding CCL program.",
    ),
    (
        "font-encoding-alist",
        "Alist of fontname patterns vs the corresponding encoding and repertory info.",
    ),
    (
        "font-encoding-charset-alist",
        "Alist of charsets vs the charsets to determine the preferred font encoding.",
    ),
    (
        "font-log",
        "A list that logs font-related actions and results, for debugging.",
    ),
    (
        "font-slant-table",
        "Vector of font slant symbols vs the corresponding numeric values.",
    ),
    (
        "font-use-system-font",
        "Non-nil means to apply the system defined font dynamically.",
    ),
    ("font-weight-table", "Vector of valid font weight values."),
    (
        "font-width-table",
        "Alist of font width symbols vs the corresponding numeric values.",
    ),
    (
        "fontification-functions",
        "List of functions to call to fontify regions of text.",
    ),
    (
        "fontset-alias-alist",
        "Alist of fontset names vs the aliases.",
    ),
    (
        "force-load-messages",
        "Non-nil means force printing messages when loading Lisp files.",
    ),
    (
        "frame-alpha-lower-limit",
        "The lower limit of the frame opacity (alpha transparency).",
    ),
    (
        "frame-inhibit-implied-resize",
        "Whether frames should be resized implicitly.",
    ),
    (
        "frame-internal-parameters",
        "Frame parameters specific to every frame.",
    ),
    (
        "frame-resize-pixelwise",
        "Non-nil means resize frames pixelwise.",
    ),
    ("frame-size-history", "History of frame size adjustments."),
    (
        "frame-title-format",
        "Template for displaying the title bar of visible frames.",
    ),
    ("fringe-bitmaps", "List of fringe bitmap symbols."),
    (
        "fringe-cursor-alist",
        "Mapping from logical to physical fringe cursor bitmaps.",
    ),
    (
        "fringe-indicator-alist",
        "Mapping from logical to physical fringe indicator bitmaps.",
    ),
    (
        "fringes-outside-margins",
        "Non-nil means to display fringes outside display margins.",
    ),
    (
        "function-key-map",
        "The parent keymap of all `local-function-key-map' instances.",
    ),
    (
        "garbage-collection-messages",
        "Non-nil means display messages at start and end of garbage collection.",
    ),
    (
        "gc-cons-percentage",
        "Portion of the heap used for allocation.",
    ),
    (
        "gc-cons-threshold",
        "Number of bytes of consing between garbage collections.",
    ),
    (
        "gc-elapsed",
        "Accumulated time elapsed in garbage collections.",
    ),
    (
        "gcs-done",
        "Accumulated number of garbage collections done.",
    ),
    (
        "global-disable-point-adjustment",
        "If non-nil, always suppress point adjustments.",
    ),
    (
        "global-mode-string",
        "String (or mode line construct) included (normally) in `mode-line-misc-info'.",
    ),
    (
        "glyph-table",
        "Table defining how to output a glyph code to the frame.",
    ),
    (
        "glyphless-char-display",
        "Char-table defining glyphless characters.",
    ),
    (
        "gnutls-log-level",
        "Logging level used by the GnuTLS functions.",
    ),
    (
        "header-line-format",
        "Analogous to `mode-line-format', but controls the header line.",
    ),
    ("help-char", "Character to recognize as meaning Help."),
    (
        "help-event-list",
        "List of input events to recognize as meaning Help.",
    ),
    (
        "help-form",
        "Form to execute when character `help-char' is read.",
    ),
    (
        "hexl-program-name",
        "Name of the `hexl' program distributed with Emacs.",
    ),
    (
        "highlight-nonselected-windows",
        "Non-nil means highlight active region even in nonselected windows.",
    ),
    (
        "history-add-new-input",
        "Non-nil means to add new elements in history.",
    ),
    (
        "history-delete-duplicates",
        "Non-nil means to delete duplicates in history.",
    ),
    (
        "history-length",
        "Maximum length of history lists before truncation takes place.",
    ),
    (
        "horizontal-scroll-bar",
        "Position of this buffer's horizontal scroll bar.",
    ),
    (
        "hourglass-delay",
        "Seconds to wait before displaying an hourglass pointer when Emacs is busy.",
    ),
    (
        "hscroll-margin",
        "How many columns away from the window edge point is allowed to get.",
    ),
    (
        "hscroll-step",
        "How many columns to scroll the window when point gets too close to the edge.",
    ),
    (
        "icon-title-format",
        "Template for displaying the title bar of an iconified frame.",
    ),
    (
        "iconify-child-frame",
        "How to handle iconification of child frames.",
    ),
    (
        "ignore-relative-composition",
        "Char table of characters which are not composed relatively.",
    ),
    (
        "image-cache-eviction-delay",
        "Maximum time after which images are removed from the cache.",
    ),
    (
        "image-scaling-factor",
        "When displaying images, apply this scaling factor before displaying.",
    ),
    ("image-types", "List of potentially supported image types."),
    (
        "indent-tabs-mode",
        "Indentation can insert tabs if this is non-nil.",
    ),
    (
        "indicate-buffer-boundaries",
        "Visually indicate buffer boundaries and scrolling.",
    ),
    (
        "indicate-empty-lines",
        "Visually indicate unused (\"empty\") screen lines after the buffer end.",
    ),
    (
        "inherit-process-coding-system",
        "Non-nil means process buffer inherits coding system of process output.",
    ),
    (
        "inhibit--record-char",
        "If non-nil, don't record input events.",
    ),
    (
        "inhibit-bidi-mirroring",
        "Non-nil means don't mirror characters even when bidi context requires that.",
    ),
    ("inhibit-changing-match-data", "Internal use only."),
    (
        "inhibit-compacting-font-caches",
        "If non-nil, don't compact font caches during GC.",
    ),
    (
        "inhibit-debugger",
        "Non-nil means never enter the debugger.",
    ),
    (
        "inhibit-eol-conversion",
        "Non-nil means always inhibit code conversion of end-of-line format.",
    ),
    (
        "inhibit-eval-during-redisplay",
        "Non-nil means don't eval Lisp during redisplay.",
    ),
    (
        "inhibit-field-text-motion",
        "Non-nil means text motion commands don't notice fields.",
    ),
    (
        "inhibit-file-name-handlers",
        "A list of file name handlers that temporarily should not be used.",
    ),
    (
        "inhibit-file-name-operation",
        "The operation for which `inhibit-file-name-handlers' is applicable.",
    ),
    (
        "inhibit-free-realized-faces",
        "Non-nil means don't free realized faces.  Internal use only.",
    ),
    (
        "inhibit-interaction",
        "Non-nil means any user interaction will signal an error.",
    ),
    (
        "inhibit-iso-escape-detection",
        "If non-nil, Emacs ignores ISO-2022 escape sequences during code detection.",
    ),
    (
        "inhibit-load-charset-map",
        "Inhibit loading of charset maps.  Used when dumping Emacs.",
    ),
    (
        "inhibit-menubar-update",
        "Non-nil means don't update menu bars.  Internal use only.",
    ),
    (
        "inhibit-message",
        "Non-nil means calls to `message' are not displayed.",
    ),
    (
        "inhibit-modification-hooks",
        "Non-nil means don't run any of the hooks that respond to buffer changes.",
    ),
    (
        "inhibit-mouse-event-check",
        "Whether the interactive spec \"e\" requires a mouse gesture event.",
    ),
    (
        "inhibit-null-byte-detection",
        "If non-nil, Emacs ignores null bytes on code detection.",
    ),
    (
        "inhibit-point-motion-hooks",
        "If non-nil, don't run `point-left' and `point-entered' text properties.",
    ),
    (
        "inhibit-quit",
        "Non-nil inhibits C-g quitting from happening immediately.",
    ),
    (
        "inhibit-read-only",
        "Non-nil means disregard read-only status of buffers or characters.",
    ),
    (
        "inhibit-redisplay",
        "Non-nil means don't actually do any redisplay.",
    ),
    (
        "inhibit-x-resources",
        "If non-nil, X resources, Windows Registry settings, and NS defaults are not used.",
    ),
    (
        "initial-environment",
        "List of environment variables inherited from the parent process.",
    ),
    (
        "initial-window-system",
        "Name of the window system that Emacs uses for the first frame.",
    ),
    (
        "input-decode-map",
        "Keymap that decodes input escape sequences.",
    ),
    (
        "input-method-function",
        "If non-nil, the function that implements the current input method.",
    ),
    (
        "input-method-previous-message",
        "When `input-method-function' is called, hold the previous echo area message.",
    ),
    (
        "input-pending-p-filter-events",
        "If non-nil, `input-pending-p' ignores some input events.",
    ),
    (
        "installation-directory",
        "A directory within which to look for runtime support files such as `etc'.",
    ),
    (
        "integer-width",
        "Maximum number N of bits in safely-calculated integers.",
    ),
    (
        "internal--daemon-sockname",
        "Name of external socket passed to Emacs, or nil if none.",
    ),
    (
        "internal--text-quoting-flag",
        "If nil, a nil `text-quoting-style' is treated as `grave'.",
    ),
    (
        "internal--top-level-message",
        "Message displayed by `normal-top-level'.",
    ),
    (
        "internal-doc-file-name",
        "Name of file containing documentation strings of built-in symbols.",
    ),
    (
        "internal-make-interpreted-closure-function",
        "Function to filter the env when constructing a closure.",
    ),
    (
        "internal-when-entered-debugger",
        "The number of keyboard events as of last time `debugger' was called.",
    ),
    (
        "interrupt-process-functions",
        "List of functions to be called for `interrupt-process'.",
    ),
    (
        "intervals-consed",
        "Number of intervals that have been consed so far.",
    ),
    (
        "inverse-video",
        "Non-nil means invert the entire frame display.",
    ),
    (
        "invocation-directory",
        "The directory in which the Emacs executable was found, to run it.",
    ),
    (
        "invocation-name",
        "The program name that was used to run Emacs.",
    ),
    (
        "kbd-macro-termination-hook",
        "Normal hook run whenever a keyboard macro terminates.",
    ),
    (
        "key-translation-map",
        "Keymap of key translations that can override keymaps.",
    ),
    (
        "keyboard-translate-table",
        "Translate table for local keyboard input, or nil.",
    ),
    (
        "kill-buffer-delete-auto-save-files",
        "If non-nil, offer to delete any autosave file when killing a buffer.",
    ),
    (
        "kill-buffer-query-functions",
        "List of functions called with no args to query before killing a buffer.",
    ),
    ("kill-emacs-hook", "Hook run when `kill-emacs' is called."),
    (
        "large-hscroll-threshold",
        "Horizontal scroll of truncated lines above which to use redisplay shortcuts.",
    ),
    (
        "last-code-conversion-error",
        "Error status of the last code conversion.",
    ),
    (
        "last-coding-system-used",
        "Coding system used in the latest file or process I/O.",
    ),
    (
        "last-command-event",
        "Last input event of a key sequence that called a command.",
    ),
    (
        "last-event-device",
        "The name of the input device of the most recently read event.",
    ),
    (
        "last-event-frame",
        "The frame in which the most recently read event occurred.",
    ),
    ("last-input-event", "Last input event."),
    (
        "last-kbd-macro",
        "Last kbd macro defined, as a string or vector; nil if none defined.",
    ),
    (
        "last-nonmenu-event",
        "Last input event in a command, except for mouse menu events.",
    ),
    (
        "last-prefix-arg",
        "The value of the prefix argument for the previous editing command.",
    ),
    (
        "last-repeatable-command",
        "Last command that may be repeated.",
    ),
    (
        "latin-extra-code-table",
        "Table of extra Latin codes in the range 128..159 (inclusive).",
    ),
    (
        "left-fringe-width",
        "Width of this buffer's left fringe (in pixels).",
    ),
    (
        "left-margin",
        "Column for the default `indent-line-function' to indent to.",
    ),
    (
        "left-margin-width",
        "Width in columns of left marginal area for display of a buffer.",
    ),
    (
        "libgnutls-version",
        "The version of libgnutls that Emacs was compiled with.",
    ),
    (
        "line-number-display-limit",
        "Maximum buffer size for which line number should be displayed.",
    ),
    (
        "line-number-display-limit-width",
        "Maximum line width (in characters) for line number display.",
    ),
    (
        "line-prefix",
        "Prefix prepended to all non-continuation lines at display time.",
    ),
    (
        "line-spacing",
        "Additional space to put between lines when displaying a buffer.",
    ),
    (
        "lisp-eval-depth-reserve",
        "Extra depth that can be allocated to handle errors.",
    ),
    (
        "load-convert-to-unibyte",
        "Non-nil means `read' converts strings to unibyte whenever possible.",
    ),
    (
        "load-dangerous-libraries",
        "Non-nil means load dangerous compiled Lisp files.",
    ),
    (
        "load-file-rep-suffixes",
        "List of suffixes that indicate representations of the same file.",
    ),
    (
        "load-force-doc-strings",
        "Non-nil means `load' should force-load all dynamic doc strings.",
    ),
    ("load-in-progress", "Non-nil if inside of `load'."),
    (
        "load-no-native",
        "Non-nil means not to load native code unless explicitly requested.",
    ),
    (
        "load-read-function",
        "Function used for reading expressions.",
    ),
    (
        "load-source-file-function",
        "Function called in `load' to load an Emacs Lisp source file.",
    ),
    (
        "load-suffixes",
        "List of suffixes for Emacs Lisp files and dynamic modules.",
    ),
    (
        "load-true-file-name",
        "Full name of file being loaded by `load'.",
    ),
    (
        "local-abbrev-table",
        "Local (mode-specific) abbrev table of current buffer.",
    ),
    (
        "local-function-key-map",
        "Keymap that translates key sequences to key sequences during input.",
    ),
    (
        "local-minor-modes",
        "Minor modes currently active in the current buffer.",
    ),
    (
        "locale-coding-system",
        "Coding system to use with system messages.",
    ),
    (
        "long-line-optimizations-bol-search-limit",
        "Limit for beginning of line search in buffers with long lines.",
    ),
    (
        "long-line-optimizations-region-size",
        "Region size for narrowing in buffers with long lines.",
    ),
    (
        "long-line-threshold",
        "Line length above which to use redisplay shortcuts.",
    ),
    (
        "lread--unescaped-character-literals",
        "List of deprecated unescaped character literals encountered by `read'.",
    ),
    (
        "lucid--menu-grab-keyboard",
        "If non-nil, grab keyboard during menu operations.",
    ),
    (
        "macroexp--dynvars",
        "List of variables declared dynamic in the current scope.",
    ),
    ("main-thread", "The main thread of Emacs."),
    (
        "make-cursor-line-fully-visible",
        "Whether to scroll the window if the cursor line is not fully visible.",
    ),
    (
        "make-pointer-invisible",
        "If non-nil, make mouse pointer invisible while typing.",
    ),
    (
        "make-window-start-visible",
        "Whether to ensure `window-start' position is never invisible.",
    ),
    (
        "mark-active",
        "Non-nil means the mark and region are currently active in this buffer.",
    ),
    (
        "mark-even-if-inactive",
        "Non-nil means you can use the mark even when inactive.",
    ),
    ("max-image-size", "Maximum size of images."),
    (
        "max-lisp-eval-depth",
        "Limit on depth in `eval', `apply' and `funcall' before error.",
    ),
    (
        "max-mini-window-height",
        "Maximum height for resizing mini-windows (the minibuffer and the echo area).",
    ),
    (
        "max-redisplay-ticks",
        "Maximum number of redisplay ticks before aborting redisplay of a window.",
    ),
    (
        "maximum-scroll-margin",
        "Maximum effective value of `scroll-margin'.",
    ),
    (
        "memory-full",
        "Non-nil means Emacs cannot get much more Lisp memory.",
    ),
    (
        "memory-signal-data",
        "Precomputed `signal' argument for memory-full error.",
    ),
    (
        "menu-bar-final-items",
        "List of menu bar items to move to the end of the menu bar.",
    ),
    ("menu-bar-mode", "Non-nil if Menu-Bar mode is enabled."),
    (
        "menu-bar-update-hook",
        "Normal hook run to update the menu bar definitions.",
    ),
    (
        "menu-prompt-more-char",
        "Character to see next line of menu prompt.",
    ),
    (
        "menu-prompting",
        "Non-nil means prompt with menus when appropriate.",
    ),
    (
        "menu-updating-frame",
        "Frame for which we are updating a menu.",
    ),
    (
        "message-log-max",
        "Maximum number of lines to keep in the message log buffer.",
    ),
    (
        "message-truncate-lines",
        "If non-nil, messages are truncated when displaying the echo area.",
    ),
    (
        "messages-buffer-name",
        "The name of the buffer where messages are logged.",
    ),
    ("meta-prefix-char", "Meta-prefix character code."),
    (
        "minibuffer-allow-text-properties",
        "Non-nil means `read-from-minibuffer' should not discard text properties.",
    ),
    (
        "minibuffer-auto-raise",
        "Non-nil means entering the minibuffer raises the minibuffer's frame.",
    ),
    (
        "minibuffer-completing-file-name",
        "Non-nil means completing file names.",
    ),
    (
        "minibuffer-completion-confirm",
        "Whether to demand confirmation of completion before exiting minibuffer.",
    ),
    (
        "minibuffer-completion-predicate",
        "Within call to `completing-read', this holds the PREDICATE argument.",
    ),
    (
        "minibuffer-completion-table",
        "Alist or obarray used for completion in the minibuffer.",
    ),
    (
        "minibuffer-exit-hook",
        "Normal hook run whenever a minibuffer is exited.",
    ),
    (
        "minibuffer-follows-selected-frame",
        "t means the active minibuffer always displays on the selected frame.",
    ),
    (
        "minibuffer-help-form",
        "Value that `help-form' takes on inside the minibuffer.",
    ),
    (
        "minibuffer-history-position",
        "Current position of redoing in the history list.",
    ),
    (
        "minibuffer-history-variable",
        "History list symbol to add minibuffer values to.",
    ),
    (
        "minibuffer-local-map",
        "Default keymap to use when reading from the minibuffer.",
    ),
    (
        "minibuffer-message-timeout",
        "How long to display an echo-area message when the minibuffer is active.",
    ),
    (
        "minibuffer-prompt-properties",
        "Text properties that are added to minibuffer prompts.",
    ),
    (
        "minibuffer-scroll-window",
        "Non-nil means it is the window that C-M-v in minibuffer should scroll.",
    ),
    (
        "minibuffer-setup-hook",
        "Normal hook run just after entry to minibuffer.",
    ),
    (
        "minor-mode-map-alist",
        "Alist of keymaps to use for minor modes.",
    ),
    (
        "minor-mode-overriding-map-alist",
        "Alist of keymaps to use for minor modes, in current major mode.",
    ),
    (
        "mode-line-compact",
        "Non-nil means that mode lines should be compact.",
    ),
    (
        "mode-line-in-non-selected-windows",
        "Non-nil means to use `mode-line-inactive' face in non-selected windows.",
    ),
    ("mode-name", "Pretty name of current buffer's major mode."),
    (
        "module-file-suffix",
        "Suffix of loadable module file, or nil if modules are not supported.",
    ),
    (
        "most-negative-fixnum",
        "The least integer that is represented efficiently.",
    ),
    (
        "most-positive-fixnum",
        "The greatest integer that is represented efficiently.",
    ),
    (
        "mouse-autoselect-window",
        "Non-nil means autoselect window with mouse pointer.",
    ),
    (
        "mouse-fine-grained-tracking",
        "Non-nil for pixelwise mouse-movement.",
    ),
    (
        "mouse-highlight",
        "If non-nil, clickable text is highlighted when mouse is over it.",
    ),
    (
        "mouse-leave-buffer-hook",
        "Hook run when the user mouse-clicks in a window.",
    ),
    (
        "mouse-position-function",
        "If non-nil, function to transform normal value of `mouse-position'.",
    ),
    (
        "mouse-prefer-closest-glyph",
        "Non-nil means mouse click position is taken from glyph closest to click.",
    ),
    (
        "move-frame-functions",
        "Functions run after a frame was moved.",
    ),
    (
        "movemail-program-name",
        "Program name for an external `movemail` helper, if available.",
    ),
    (
        "multibyte-syntax-as-symbol",
        "Non-nil means `scan-sexps' treats all multibyte characters as symbol.",
    ),
    (
        "multiple-frames",
        "Non-nil if more than one frame is visible on this display.",
    ),
    (
        "mwheel-coalesce-scroll-events",
        "Non-nil means send a wheel event only for scrolling at least one screen line.",
    ),
    (
        "native-comp-eln-load-path",
        "List of directories to look for natively-compiled *.eln files.",
    ),
    (
        "native-comp-enable-subr-trampolines",
        "If non-nil, enable generation of trampolines for calling primitives.",
    ),
    (
        "native-comp-jit-compilation",
        "If non-nil, compile loaded .elc files asynchronously.",
    ),
    (
        "network-coding-system-alist",
        "Alist to decide a coding system to use for a network I/O operation.",
    ),
    (
        "next-screen-context-lines",
        "Number of lines of continuity when scrolling by screenfuls.",
    ),
    (
        "no-redraw-on-reenter",
        "Non-nil means no need to redraw entire frame after suspending.",
    ),
    (
        "nobreak-char-ascii-display",
        "Control display of non-ASCII space and hyphen chars.",
    ),
    (
        "nobreak-char-display",
        "Control highlighting of non-ASCII space and hyphen chars.",
    ),
    (
        "num-input-keys",
        "Number of complete key sequences read as input so far.",
    ),
    (
        "num-nonmacro-input-events",
        "Number of input events read from the keyboard so far.",
    ),
    ("obarray", "Symbol table for use by ‘intern’ and ‘read’."),
    (
        "open-paren-in-column-0-is-defun-start",
        "Non-nil means an open paren in column 0 denotes the start of a defun.",
    ),
    (
        "operating-system-release",
        "The kernel version of the operating system on which Emacs is running.",
    ),
    (
        "otf-script-alist",
        "Alist of OpenType script tags vs the corresponding script names.",
    ),
    (
        "other-window-scroll-buffer",
        "If this is a live buffer, C-M-v should scroll its window.",
    ),
    (
        "other-window-scroll-default",
        "Function that provides the window to scroll by C-M-v.",
    ),
    (
        "overflow-newline-into-fringe",
        "Non-nil means that newline may flow into the right fringe.",
    ),
    (
        "overlay-arrow-position",
        "Marker for where to display an arrow on top of the buffer text.",
    ),
    (
        "overlay-arrow-string",
        "String to display as an arrow in text-mode frames.",
    ),
    (
        "overlay-arrow-variable-list",
        "List of variables (symbols) which hold markers for overlay arrows.",
    ),
    (
        "overline-margin",
        "Space between overline and text, in pixels.",
    ),
    (
        "overriding-local-map",
        "Keymap that replaces (overrides) local keymaps.",
    ),
    (
        "overriding-local-map-menu-flag",
        "Non-nil means ‘overriding-local-map’ applies to the menu bar.",
    ),
    (
        "overriding-plist-environment",
        "An alist that overrides the plists of the symbols which it lists.",
    ),
    (
        "overriding-terminal-local-map",
        "Per-terminal keymap that takes precedence over all other keymaps.",
    ),
    (
        "overriding-text-conversion-style",
        "Non-buffer local version of ‘text-conversion-style’.",
    ),
    (
        "overwrite-mode",
        "Non-nil if self-insertion should replace existing text.",
    ),
    (
        "parse-sexp-ignore-comments",
        "Non-nil means ‘forward-sexp’, etc., should treat comments as whitespace.",
    ),
    (
        "parse-sexp-lookup-properties",
        "Non-nil means ‘forward-sexp’, etc., obey ‘syntax-table’ property.",
    ),
    (
        "path-separator",
        "String containing the character that separates directories in",
    ),
    (
        "pdumper-fingerprint",
        "The fingerprint of this Emacs binary.",
    ),
    (
        "point-before-scroll",
        "Value of point before the last series of scroll operations, or nil.",
    ),
    (
        "polling-period",
        "Interval between polling for input during Lisp execution.",
    ),
    (
        "post-command-hook",
        "Normal hook run after each command is executed.",
    ),
    (
        "post-gc-hook",
        "Hook run after garbage collection has finished.",
    ),
    (
        "post-select-region-hook",
        "Abnormal hook run after the region is selected.",
    ),
    (
        "post-self-insert-hook",
        "Hook run at the end of ‘self-insert-command’.",
    ),
    (
        "pre-command-hook",
        "Normal hook run before each command is executed.",
    ),
    (
        "pre-redisplay-function",
        "Function run just before redisplay.",
    ),
    (
        "prefix-arg",
        "The value of the prefix argument for the next editing command.",
    ),
    (
        "prefix-help-command",
        "Command to run when ‘help-char’ character follows a prefix key.",
    ),
    (
        "preloaded-file-list",
        "List of files that were preloaded (when dumping Emacs).",
    ),
    (
        "print-charset-text-property",
        "A flag to control printing of ‘charset’ text property on printing a string.",
    ),
    (
        "print-circle",
        "Non-nil means print recursive structures using #N= and #N# syntax.",
    ),
    (
        "print-continuous-numbering",
        "Non-nil means number continuously across print calls.",
    ),
    (
        "print-escape-control-characters",
        "Non-nil means print control characters in strings as ‘\\OOO’.",
    ),
    (
        "print-escape-multibyte",
        "Non-nil means print multibyte characters in strings as \\xXXXX.",
    ),
    (
        "print-escape-newlines",
        "Non-nil means print newlines in strings as ‘\\n’.",
    ),
    (
        "print-escape-nonascii",
        "Non-nil means print unibyte non-ASCII chars in strings as \\OOO.",
    ),
    (
        "print-gensym",
        "Non-nil means print uninterned symbols so they will read as uninterned.",
    ),
    (
        "print-integers-as-characters",
        "Non-nil means integers are printed using characters syntax.",
    ),
    (
        "print-number-table",
        "A vector used internally to produce ‘#N=’ labels and ‘#N#’ references.",
    ),
    (
        "print-quoted",
        "Non-nil means print quoted forms with reader syntax.",
    ),
    (
        "print-symbols-bare",
        "A flag to control printing of symbols with position.",
    ),
    (
        "print-unreadable-function",
        "If non-nil, a function to call when printing unreadable objects.",
    ),
    (
        "printable-chars",
        "A char-table for each printable character.",
    ),
    (
        "process-adaptive-read-buffering",
        "If non-nil, improve receive buffering by delaying after short reads.",
    ),
    (
        "process-coding-system-alist",
        "Alist to decide a coding system to use for a process I/O operation.",
    ),
    (
        "process-connection-type",
        "Control type of device used to communicate with subprocesses.",
    ),
    (
        "process-environment",
        "List of overridden environment variables for subprocesses to inherit.",
    ),
    (
        "process-error-pause-time",
        "The number of seconds to pause after handling process errors.",
    ),
    (
        "process-prioritize-lower-fds",
        "Whether to start checking for subprocess output from first file descriptor.",
    ),
    (
        "profiler-log-size",
        "Number of distinct call-stacks that can be recorded in a profiler log.",
    ),
    (
        "profiler-max-stack-depth",
        "Number of elements from the call-stack recorded in the log.",
    ),
    (
        "pure-bytes-used",
        "Number of bytes of shareable Lisp data allocated so far.",
    ),
    (
        "purify-flag",
        "Non-nil means loading Lisp code in order to dump an executable.",
    ),
    (
        "query-all-font-backends",
        "If non-nil, attempt to query all available font backends.",
    ),
    (
        "quit-flag",
        "Non-nil causes ‘eval’ to abort, unless ‘inhibit-quit’ is non-nil.",
    ),
    (
        "rcs2log-program-name",
        "Name of the ‘rcs2log’ program distributed with Emacs.",
    ),
    (
        "read-buffer-completion-ignore-case",
        "Non-nil means completion ignores case when reading a buffer name.",
    ),
    (
        "read-buffer-function",
        "If this is non-nil, ‘read-buffer’ does its work by calling this function.",
    ),
    (
        "read-circle",
        "Non-nil means read recursive structures using #N= and #N# syntax.",
    ),
    (
        "read-expression-history",
        "A history list for arguments that are Lisp expressions to evaluate.",
    ),
    (
        "read-hide-char",
        "Whether to hide input characters in noninteractive mode.",
    ),
    (
        "read-minibuffer-restore-windows",
        "Non-nil means restore window configurations on exit from minibuffer.",
    ),
    (
        "read-process-output-max",
        "Maximum number of bytes to read from subprocess in a single chunk.",
    ),
    (
        "read-symbol-shorthands",
        "Alist of known symbol-name shorthands.",
    ),
    (
        "real-last-command",
        "Same as ‘last-command’, but never altered by Lisp code.",
    ),
    (
        "real-this-command",
        "This is like ‘this-command’, except that commands should never modify it.",
    ),
    (
        "recenter-redisplay",
        "Non-nil means ‘recenter’ redraws entire frame.",
    ),
    ("record-all-keys", "Non-nil means record all keys you type."),
    (
        "redisplay--all-windows-cause",
        "Code of the cause for redisplaying all windows.",
    ),
    (
        "redisplay--inhibit-bidi",
        "Non-nil means it is not safe to attempt bidi reordering for display.",
    ),
    (
        "redisplay--mode-lines-cause",
        "Code of the cause for redisplaying mode lines.",
    ),
    (
        "redisplay-adhoc-scroll-in-resize-mini-windows",
        "If nil always use normal scrolling in minibuffer windows.",
    ),
    (
        "redisplay-dont-pause",
        "Nil means display update is paused when input is detected.",
    ),
    (
        "redisplay-skip-fontification-on-input",
        "Skip ‘fontification_functions‘ when there is input pending.",
    ),
    (
        "redisplay-skip-initial-frame",
        "Non-nil means skip redisplay of the initial frame.",
    ),
    (
        "region-extract-function",
        "Function to get the region’s content.",
    ),
    (
        "report-emacs-bug-address",
        "Address of mailing list for GNU Emacs bugs.",
    ),
    (
        "resize-mini-frames",
        "Non-nil means resize minibuffer-only frames automatically.",
    ),
    (
        "resize-mini-windows",
        "How to resize mini-windows (the minibuffer and the echo area).",
    ),
    (
        "resume-tty-functions",
        "Functions run after resuming a tty.",
    ),
    (
        "right-fringe-width",
        "Width of this buffer’s right fringe (in pixels).",
    ),
    (
        "right-margin-width",
        "Width in columns of right marginal area for display of a buffer.",
    ),
    (
        "ring-bell-function",
        "Non-nil means call this function to ring the bell.",
    ),
    (
        "saved-region-selection",
        "Contents of active region prior to buffer modification.",
    ),
    ("scalable-fonts-allowed", "Allowed scalable fonts."),
    (
        "script-representative-chars",
        "Alist of scripts vs the representative characters.",
    ),
    (
        "scroll-bar-adjust-thumb-portion",
        "Adjust scroll bars for overscrolling for Gtk+, Motif and Haiku.",
    ),
    (
        "scroll-bar-height",
        "Height of this buffer’s horizontal scroll bars in pixels.",
    ),
    (
        "scroll-bar-width",
        "Width of this buffer’s vertical scroll bars in pixels.",
    ),
    (
        "scroll-conservatively",
        "Scroll up to this many lines, to bring point back on screen.",
    ),
    (
        "scroll-down-aggressively",
        "How far to scroll windows downward.",
    ),
    (
        "scroll-margin",
        "Number of lines of margin at the top and bottom of a window.",
    ),
    (
        "scroll-minibuffer-conservatively",
        "Non-nil means scroll conservatively in minibuffer windows.",
    ),
    (
        "scroll-preserve-screen-position",
        "Controls if scroll commands move point to keep its screen position unchanged.",
    ),
    (
        "scroll-step",
        "The number of lines to try scrolling a window by when point moves out.",
    ),
    (
        "scroll-up-aggressively",
        "How far to scroll windows upward.",
    ),
    (
        "search-spaces-regexp",
        "Regexp to substitute for bunches of spaces in regexp search.",
    ),
    (
        "select-safe-coding-system-function",
        "Function to call to select safe coding system for encoding a text.",
    ),
    (
        "selection-converter-alist",
        "An alist associating X Windows selection-types with functions.",
    ),
    (
        "selection-inhibit-update-commands",
        "List of commands which should not update the selection.",
    ),
    ("selective-display", "Non-nil enables selective display."),
    (
        "selective-display-ellipses",
        "Non-nil means display ... on previous line when a line is invisible.",
    ),
    (
        "set-auto-coding-function",
        "If non-nil, a function to call to decide a coding system of file.",
    ),
    (
        "set-message-function",
        "If non-nil, function to handle display of echo-area messages.",
    ),
    (
        "shared-game-score-directory",
        "Directory of score files for games which come with GNU Emacs.",
    ),
    (
        "show-help-function",
        "If non-nil, the function that implements the display of help.",
    ),
    (
        "show-trailing-whitespace",
        "Non-nil means highlight trailing whitespace.",
    ),
    (
        "signal-hook-function",
        "If non-nil, this is a function for ‘signal’ to call.",
    ),
    (
        "signal-process-functions",
        "List of functions to be called for ‘signal-process’.",
    ),
    (
        "source-directory",
        "Directory in which Emacs sources were found when Emacs was built.",
    ),
    (
        "special-event-map",
        "Keymap defining bindings for special events to execute at low level.",
    ),
    (
        "standard-display-table",
        "Display table to use for buffers that specify none.",
    ),
    ("standard-input", "Stream for read to get input from."),
    (
        "standard-translation-table-for-decode",
        "Table for translating characters while decoding.",
    ),
    (
        "standard-translation-table-for-encode",
        "Table for translating characters while encoding.",
    ),
    (
        "string-chars-consed",
        "Number of string characters that have been consed so far.",
    ),
    (
        "strings-consed",
        "Number of strings that have been consed so far.",
    ),
    (
        "suspend-tty-functions",
        "Functions run after suspending a tty.",
    ),
    (
        "symbols-consed",
        "Number of symbols that have been consed so far.",
    ),
    (
        "symbols-with-pos-enabled",
        "If non-nil, a symbol with position ordinarily behaves as its bare symbol.",
    ),
    (
        "syntax-propertize--done",
        "Position up to which syntax-table properties have been set.",
    ),
    (
        "system-configuration",
        "Value is string indicating configuration Emacs was built for.",
    ),
    (
        "system-configuration-features",
        "String listing some of the main features this Emacs was compiled with.",
    ),
    (
        "system-configuration-options",
        "String containing the configuration options Emacs was built with.",
    ),
    (
        "system-key-alist",
        "Alist of system-specific X windows key symbols.",
    ),
    ("system-messages-locale", "System locale for messages."),
    (
        "system-name",
        "The host name of the machine Emacs is running on.",
    ),
    ("system-time-locale", "System locale for time."),
    (
        "system-type",
        "The value is a symbol indicating the type of operating system you are using.",
    ),
    (
        "system-uses-terminfo",
        "Non-nil means the system uses terminfo rather than termcap.",
    ),
    (
        "tab-bar--dragging-in-progress",
        "Non-nil when maybe dragging tab bar item.",
    ),
    ("tab-bar-border", "Border below tab-bar in pixels."),
    (
        "tab-bar-button-margin",
        "Margin around tab-bar buttons in pixels.",
    ),
    (
        "tab-bar-button-relief",
        "Relief thickness of tab-bar buttons.",
    ),
    (
        "tab-bar-position",
        "Specify on which side from the tool bar the tab bar shall be.",
    ),
    (
        "tab-bar-separator-image-expression",
        "Expression evaluating to the image spec for a tab-bar separator.",
    ),
    (
        "tab-line-format",
        "Analogous to ‘mode-line-format’, but controls the tab line.",
    ),
    (
        "tab-width",
        "Distance between tab stops (for display of tab characters), in columns.",
    ),
    (
        "temp-buffer-show-function",
        "Non-nil means call as function to display a help buffer.",
    ),
    (
        "temporary-file-directory",
        "The directory for writing temporary files.",
    ),
    (
        "terminal-frame",
        "The initial frame-object, which represents Emacs’s stdout.",
    ),
    (
        "text-conversion-edits",
        "List of buffers last edited as a result of text conversion.",
    ),
    (
        "text-conversion-face",
        "Face in which to display temporary edits by an input method.",
    ),
    (
        "text-conversion-style",
        "How the on screen keyboard’s input method should insert in this buffer.",
    ),
    (
        "text-property-default-nonsticky",
        "Alist of properties vs the corresponding non-stickiness.",
    ),
    (
        "text-quoting-style",
        "Style to use for single quotes in help and messages.",
    ),
    (
        "this-command-keys-shift-translated",
        "Non-nil if the key sequence activating this command was shift-translated.",
    ),
    (
        "this-original-command",
        "The command bound to the current key sequence before remapping.",
    ),
    (
        "throw-on-input",
        "If non-nil, any keyboard input throws to this symbol.",
    ),
    (
        "timer-idle-list",
        "List of active idle-time timers in order of increasing time.",
    ),
    (
        "timer-list",
        "List of active absolute time timers in order of increasing time.",
    ),
    ("tool-bar-border", "Border below tool-bar in pixels."),
    (
        "tool-bar-button-margin",
        "Margin around tool-bar buttons in pixels.",
    ),
    (
        "tool-bar-button-relief",
        "Relief thickness of tool-bar buttons.",
    ),
    (
        "tool-bar-max-label-size",
        "Maximum number of characters a label can have to be shown.",
    ),
    (
        "tool-bar-separator-image-expression",
        "Expression evaluating to the image spec for a tool-bar separator.",
    ),
    ("tool-bar-style", "Tool bar style to use."),
    (
        "tooltip-reuse-hidden-frame",
        "Non-nil means reuse hidden tooltip frames.",
    ),
    ("top-level", "Form to evaluate when Emacs starts up."),
    (
        "track-mouse",
        "Non-nil means generate motion events for mouse motion.",
    ),
    (
        "translate-upper-case-key-bindings",
        "If non-nil, interpret upper case keys as lower case (when applicable).",
    ),
    (
        "translation-hash-table-vector",
        "Vector containing all translation hash tables ever defined.",
    ),
    (
        "translation-table-for-input",
        "Char table for translating self-inserting characters.",
    ),
    (
        "translation-table-vector",
        "Vector recording all translation tables ever defined.",
    ),
    (
        "treesit-extra-load-path",
        "Additional directories to look for tree-sitter language definitions.",
    ),
    (
        "treesit-load-name-override-list",
        "An override list for unconventional tree-sitter libraries.",
    ),
    ("treesit-thing-settings", "A list defining things."),
    (
        "truncate-partial-width-windows",
        "Non-nil means truncate lines in windows narrower than the frame.",
    ),
    (
        "tty-defined-color-alist",
        "An alist of defined terminal colors and their RGB values.",
    ),
    (
        "tty-erase-char",
        "The ERASE character as set by the user with stty.",
    ),
    (
        "tty-menu-calls-mouse-position-function",
        "Non-nil means TTY menu code will call ‘mouse-position-function’.",
    ),
    (
        "underline-minimum-offset",
        "Minimum distance between baseline and underline.",
    ),
    (
        "undo-inhibit-record-point",
        "Non-nil means do not record ‘point’ in ‘buffer-undo-list’.",
    ),
    (
        "undo-outer-limit",
        "Outer limit on size of undo information for one command.",
    ),
    (
        "undo-outer-limit-function",
        "Function to call when an undo list exceeds ‘undo-outer-limit’.",
    ),
    (
        "unibyte-display-via-language-environment",
        "Non-nil means display unibyte text according to language environment.",
    ),
    (
        "unicode-category-table",
        "Char table of Unicode’s \"General Category\".",
    ),
    (
        "unread-post-input-method-events",
        "List of events to be processed as input by input methods.",
    ),
    (
        "use-default-ascent",
        "Char table of characters whose ascent values should be ignored.",
    ),
    (
        "use-default-font-for-symbols",
        "If non-nil, use the default face’s font for symbols and punctuation.",
    ),
    (
        "use-dialog-box",
        "Non-nil means mouse commands use dialog boxes to ask questions.",
    ),
    (
        "use-file-dialog",
        "Non-nil means mouse commands use a file dialog to ask for files.",
    ),
    (
        "use-short-answers",
        "Non-nil means ‘yes-or-no-p’ uses shorter answers \"y\" or \"n\".",
    ),
    (
        "use-system-tooltips",
        "Use the toolkit to display tooltips.",
    ),
    (
        "user-init-file",
        "File name, including directory, of user’s initialization file.",
    ),
    (
        "user-login-name",
        "The user’s name, taken from environment variables if possible.",
    ),
    (
        "user-real-login-name",
        "The user’s name, based upon the real uid only.",
    ),
    (
        "values",
        "List of values of all expressions which were read, evaluated and printed.",
    ),
    (
        "vector-cells-consed",
        "Number of vector cells that have been consed so far.",
    ),
    (
        "vertical-centering-font-regexp",
        "Regexp matching font names that require vertical centering on display.",
    ),
    (
        "vertical-scroll-bar",
        "Position of this buffer’s vertical scroll bar.",
    ),
    (
        "visible-bell",
        "Non-nil means try to flash the frame to represent a bell.",
    ),
    (
        "visible-cursor",
        "Non-nil means to make the cursor very visible.",
    ),
    (
        "void-text-area-pointer",
        "The pointer shape to show in void text areas.",
    ),
    (
        "where-is-preferred-modifier",
        "Preferred modifier key to use for ‘where-is’.",
    ),
    (
        "while-no-input-ignore-events",
        "Ignored events from ‘while-no-input’.",
    ),
    (
        "window-buffer-change-functions",
        "Functions called during redisplay when window buffers have changed.",
    ),
    (
        "window-combination-limit",
        "If non-nil, splitting a window makes a new parent window.",
    ),
    (
        "window-combination-resize",
        "If t, resize window combinations proportionally.",
    ),
    (
        "window-configuration-change-hook",
        "Functions called during redisplay when window configuration has changed.",
    ),
    (
        "window-persistent-parameters",
        "Alist of persistent window parameters.",
    ),
    (
        "window-point-insertion-type",
        "Insertion type of marker to use for ‘window-point’.",
    ),
    (
        "window-resize-pixelwise",
        "Non-nil means resize windows pixelwise.",
    ),
    (
        "window-restore-killed-buffer-windows",
        "Control restoring windows whose buffer was killed.",
    ),
    (
        "window-scroll-functions",
        "List of functions to call before redisplaying a window with scrolling.",
    ),
    (
        "window-selection-change-functions",
        "Functions called during redisplay when the selected window has changed.",
    ),
    (
        "window-size-change-functions",
        "Functions called during redisplay when window sizes have changed.",
    ),
    (
        "window-state-change-functions",
        "Functions called during redisplay when the window state changed.",
    ),
    (
        "window-state-change-hook",
        "Functions called during redisplay when the window state changed.",
    ),
    (
        "word-combining-categories",
        "List of pair (cons) of categories to determine word boundary.",
    ),
    (
        "word-separating-categories",
        "List of pair (cons) of categories to determine word boundary.",
    ),
    (
        "word-wrap-by-category",
        "Non-nil means also wrap after characters of a certain category.",
    ),
    (
        "words-include-escapes",
        "Non-nil means ‘forward-word’, etc., should treat escape chars part of words.",
    ),
    (
        "wrap-prefix",
        "Prefix prepended to all continuation lines at display time.",
    ),
    (
        "write-region-annotate-functions",
        "A list of functions to be called at the start of ‘write-region’.",
    ),
    (
        "write-region-annotations-so-far",
        "When an annotation function is called, this holds the previous annotations.",
    ),
    (
        "write-region-inhibit-fsync",
        "Non-nil means don’t call fsync in ‘write-region’.",
    ),
    (
        "write-region-post-annotation-function",
        "Function to call after ‘write-region’ completes.",
    ),
    (
        "x-allow-focus-stealing",
        "How to bypass window manager focus stealing prevention.",
    ),
    (
        "x-alt-keysym",
        "Which modifier value Emacs reports when Alt is depressed.",
    ),
    (
        "x-auto-preserve-selections",
        "Whether or not to transfer selection ownership when deleting a frame.",
    ),
    (
        "x-bitmap-file-path",
        "List of directories to search for window system bitmap files.",
    ),
    (
        "x-color-cache-bucket-size",
        "Max number of buckets allowed per display in the internal color cache.",
    ),
    (
        "x-ctrl-keysym",
        "Which modifier value Emacs reports when Ctrl is depressed.",
    ),
    (
        "x-cursor-fore-pixel",
        "A string indicating the foreground color of the cursor box.",
    ),
    (
        "x-detect-server-trust",
        "If non-nil, Emacs should detect whether or not it is trusted by X.",
    ),
    (
        "x-dnd-disable-motif-drag",
        "Disable the Motif drag protocol during DND.",
    ),
    (
        "x-dnd-disable-motif-protocol",
        "Disable the Motif drag-and-drop protocols.",
    ),
    (
        "x-dnd-fix-motif-leave",
        "Work around Motif bug during drag-and-drop.",
    ),
    (
        "x-dnd-movement-function",
        "Function called upon mouse movement on a frame during drag-and-drop.",
    ),
    (
        "x-dnd-native-test-function",
        "Function that determines return value of drag-and-drop on Emacs frames.",
    ),
    (
        "x-dnd-preserve-selection-data",
        "Preserve selection data after ‘x-begin-drag’ returns.",
    ),
    ("x-dnd-targets-list", "List of drag-and-drop targets."),
    (
        "x-dnd-unsupported-drop-function",
        "Function called when trying to drop on an unsupported window.",
    ),
    (
        "x-dnd-use-unsupported-drop",
        "Enable the emulation of drag-and-drop based on the primary selection.",
    ),
    (
        "x-dnd-wheel-function",
        "Function called upon wheel movement on a frame during drag-and-drop.",
    ),
    (
        "x-fast-protocol-requests",
        "Whether or not X protocol-related functions should wait for errors.",
    ),
    (
        "x-fast-selection-list",
        "List of selections for which ‘x-selection-exists-p’ should be fast.",
    ),
    (
        "x-frame-normalize-before-maximize",
        "Non-nil means normalize frame before maximizing.",
    ),
    (
        "x-gtk-file-dialog-help-text",
        "If non-nil, the GTK file chooser will show additional help text.",
    ),
    (
        "x-gtk-resize-child-frames",
        "If non-nil, resize child frames specially with GTK builds.",
    ),
    (
        "x-gtk-show-hidden-files",
        "If non-nil, the GTK file chooser will by default show hidden files.",
    ),
    (
        "x-gtk-use-native-input",
        "Non-nil means to use GTK for input method support.",
    ),
    (
        "x-gtk-use-old-file-dialog",
        "Non-nil means prompt with the old GTK file selection dialog.",
    ),
    (
        "x-gtk-use-window-move",
        "Non-nil means rely on gtk_window_move to set frame positions.",
    ),
    (
        "x-hourglass-pointer-shape",
        "The shape of the pointer when Emacs is busy.",
    ),
    (
        "x-hyper-keysym",
        "Which modifier value Emacs reports when Hyper is depressed.",
    ),
    (
        "x-input-coding-function",
        "Function used to determine the coding system used by input methods.",
    ),
    (
        "x-input-coding-system",
        "Coding system used for input from X input methods.",
    ),
    (
        "x-input-grab-touch-events",
        "Non-nil means to actively grab touch events.",
    ),
    (
        "x-keysym-table",
        "Hash table of character codes indexed by X keysym codes.",
    ),
    (
        "x-lax-frame-positioning",
        "If non-nil, Emacs won’t compensate for WM geometry behavior.",
    ),
    (
        "x-lost-selection-functions",
        "A list of functions to be called when Emacs loses an X selection.",
    ),
    ("x-max-tooltip-size", "Maximum size for tooltips."),
    (
        "x-meta-keysym",
        "Which modifier value Emacs reports when Meta is depressed.",
    ),
    (
        "x-mouse-click-focus-ignore-position",
        "Non-nil means that a mouse click to focus a frame does not move point.",
    ),
    (
        "x-mouse-click-focus-ignore-time",
        "Number of milliseconds for which to ignore buttons after focus change.",
    ),
    (
        "x-no-window-manager",
        "Non-nil if no X window manager is in use.",
    ),
    (
        "x-pixel-size-width-font-regexp",
        "Regexp matching a font name whose width is the same as ‘PIXEL_SIZE’.",
    ),
    (
        "x-pointer-shape",
        "The shape of the pointer when over text.",
    ),
    (
        "x-pre-popup-menu-hook",
        "Hook run before ‘x-popup-menu’ displays a popup menu.",
    ),
    (
        "x-quit-keysym",
        "Keysyms which will cause Emacs to quit if rapidly pressed twice.",
    ),
    (
        "x-resource-class",
        "The class Emacs uses to look up X resources.",
    ),
    (
        "x-resource-name",
        "The name Emacs uses to look up X resources.",
    ),
    (
        "x-scroll-event-delta-factor",
        "A scale to apply to pixel deltas reported in scroll events.",
    ),
    (
        "x-select-enable-clipboard-manager",
        "Whether to enable X clipboard manager support.",
    ),
    (
        "x-selection-alias-alist",
        "List of selections to alias to another.",
    ),
    (
        "x-selection-timeout",
        "Number of milliseconds to wait for a selection reply.",
    ),
    (
        "x-sensitive-text-pointer-shape",
        "The shape of the pointer when over mouse-sensitive text.",
    ),
    (
        "x-sent-selection-functions",
        "A list of functions to be called when Emacs answers a selection request.",
    ),
    (
        "x-session-id",
        "The session id Emacs got from the session manager for this session.",
    ),
    (
        "x-session-previous-id",
        "The previous session id Emacs got from session manager.",
    ),
    (
        "x-set-frame-visibility-more-laxly",
        "Non-nil means set frame visibility more laxly.",
    ),
    (
        "x-show-tooltip-timeout",
        "The default timeout (in seconds) for ‘x-show-tip’.",
    ),
    (
        "x-stretch-cursor",
        "Non-nil means draw block cursor as wide as the glyph under it.",
    ),
    (
        "x-super-keysym",
        "Which modifier value Emacs reports when Super is depressed.",
    ),
    (
        "x-toolkit-scroll-bars",
        "Which toolkit scroll bars Emacs uses, if any.",
    ),
    (
        "x-treat-local-requests-remotely",
        "Whether to treat local selection requests as remote ones.",
    ),
    (
        "x-underline-at-descent-line",
        "Non-nil means to draw the underline at the same place as the descent line.",
    ),
    (
        "x-use-fast-mouse-position",
        "How to make ‘mouse-position’ faster.",
    ),
    (
        "x-use-underline-position-properties",
        "Non-nil means make use of UNDERLINE_POSITION font properties.",
    ),
    ("x-wait-for-event-timeout", "How long to wait for X events."),
    (
        "x-window-bottom-edge-cursor",
        "Pointer shape indicating a bottom x-window edge can be dragged.",
    ),
    (
        "x-window-bottom-left-corner-cursor",
        "Pointer shape indicating a bottom left x-window corner can be dragged.",
    ),
    (
        "x-window-bottom-right-corner-cursor",
        "Pointer shape indicating a bottom right x-window corner can be dragged.",
    ),
    (
        "x-window-horizontal-drag-cursor",
        "Pointer shape to use for indicating a window can be dragged horizontally.",
    ),
    (
        "x-window-left-edge-cursor",
        "Pointer shape indicating a left x-window edge can be dragged.",
    ),
    (
        "x-window-right-edge-cursor",
        "Pointer shape indicating a right x-window edge can be dragged.",
    ),
    (
        "x-window-top-edge-cursor",
        "Pointer shape indicating a top x-window edge can be dragged.",
    ),
    (
        "x-window-top-left-corner-cursor",
        "Pointer shape indicating a top left x-window corner can be dragged.",
    ),
    (
        "x-window-top-right-corner-cursor",
        "Pointer shape indicating a top right x-window corner can be dragged.",
    ),
    (
        "x-window-vertical-drag-cursor",
        "Pointer shape to use for indicating a window can be dragged vertically.",
    ),
    (
        "xft-ignore-color-fonts",
        "Non-nil means don’t query fontconfig for color fonts, since they often",
    ),
    ("xft-settings", "Font settings applied to Xft."),
    (
        "yes-or-no-prompt",
        "String to append when ‘yes-or-no-p’ asks a question.",
    ),
    ("last-command", "The last command executed."),
    (
        "lexical-binding",
        "Whether to use lexical binding when evaluating code.",
    ),
    (
        "load-file-name",
        "Full name of file being loaded by `load'.",
    ),
    (
        "load-history",
        "Alist mapping loaded file names to symbols and features.",
    ),
    (
        "load-path",
        "List of directories to search for files to load.\n\
Each element is a string (directory file name) or nil (meaning\n\
`default-directory').\n\
This list is consulted by the `require' function.\n\
Initialized during startup as described in Info node `(elisp)Library Search'.\n\
Use `directory-file-name' when adding items to this path.  However, Lisp\n\
programs that process this list should tolerate directories both with\n\
and without trailing slashes.",
    ),
    (
        "load-prefer-newer",
        "Non-nil means `load' prefers the newest version of a file.",
    ),
    ("major-mode", "Symbol for current buffer's major mode."),
    (
        "mode-line-format",
        "Template for displaying mode line for a window's buffer.",
    ),
    (
        "noninteractive",
        "Non-nil means Emacs is running without interactive terminal.",
    ),
    (
        "print-length",
        "Maximum length of list to print before abbreviating.",
    ),
    (
        "print-level",
        "Maximum depth of list nesting to print before abbreviating.",
    ),
    (
        "select-active-regions",
        "If non-nil, any active region automatically sets the primary selection.",
    ),
    ("shell-file-name", "File name to load inferior shells from."),
    (
        "standard-output",
        "Output stream `print' uses by default for outputting a character.",
    ),
    ("tab-bar-mode", "Non-nil if Tab-Bar mode is enabled."),
    ("this-command", "The command now being executed."),
    ("tool-bar-mode", "Non-nil if Tool-Bar mode is enabled."),
    (
        "transient-mark-mode",
        "Non-nil if Transient Mark mode is enabled.",
    ),
    (
        "truncate-lines",
        "Non-nil means do not display continuation lines.",
    ),
    (
        "undo-limit",
        "Keep no more undo information once it exceeds this size.",
    ),
    (
        "undo-strong-limit",
        "Don't keep more than this much size of undo information.",
    ),
    (
        "unread-command-events",
        "List of events to be read as the command input.",
    ),
    (
        "unread-input-method-events",
        "List of events to be processed as input by input methods.",
    ),
    ("user-full-name", "The full name of the user logged in."),
    (
        "window-system",
        "Name of window system through which the selected frame is displayed.",
    ),
    (
        "word-wrap",
        "Non-nil means to use word-wrapping for continuation lines.",
    ),
];

pub(crate) static STARTUP_VARIABLE_DOC_STRING_PROPERTIES: &[(&str, &str)] = &[
    (
        "abbrev--suggest-saved-recommendations",
        "Keeps the list of expansions that have abbrevs defined.",
    ),
    (
        "abbrev-all-caps",
        "Non-nil means expand multi-word abbrevs in all caps if the abbrev was so.",
    ),
    (
        "abbrev-expand-function",
        "Function that ‘expand-abbrev’ uses to perform abbrev expansion.",
    ),
    (
        "abbrev-expand-functions",
        "Wrapper hook around ‘abbrev--default-expand’.",
    ),
    (
        "abbrev-file-name",
        "Default name of file from which to read and where to save abbrevs.",
    ),
    ("abbrev-map", "Keymap for abbrev commands."),
    (
        "abbrev-minor-mode-table-alist",
        "Alist of abbrev tables to use for minor modes.",
    ),
    (
        "abbrev-mode-hook",
        "Hook run after entering or leaving ‘abbrev-mode’.",
    ),
    (
        "abbrev-start-location",
        "Buffer position for ‘expand-abbrev’ to use as the start of the abbrev.",
    ),
    (
        "abbrev-start-location-buffer",
        "Buffer that ‘abbrev-start-location’ has been set for.",
    ),
    (
        "abbrev-suggest",
        "Non-nil means suggest using abbrevs to save typing.",
    ),
    (
        "abbrev-suggest-hint-threshold",
        "Threshold for when to suggest to use an abbrev to save typing.",
    ),
    (
        "abbrev-table-name-list",
        "List of symbols whose values are abbrev tables.",
    ),
    (
        "abbreviated-home-dir",
        "Regexp matching the user’s homedir at the beginning of file name.",
    ),
    (
        "abbrevs-changed",
        "Non-nil if any word abbrevs were defined or altered.",
    ),
    (
        "activate-mark-hook",
        "Hook run when the mark becomes active.",
    ),
    (
        "after-change-major-mode-hook",
        "Normal hook run at the very end of major mode functions.",
    ),
    (
        "after-focus-change-function",
        "Function called after frame focus may have changed.",
    ),
    (
        "after-init-hook",
        "Normal hook run after initializing the Emacs session.",
    ),
    (
        "after-load-functions",
        "Special hook run after loading a file.",
    ),
    (
        "after-make-frame-functions",
        "Functions to run after ‘make-frame’ created a new frame.",
    ),
    (
        "after-pdump-load-hook",
        "Normal hook run after loading the .pdmp file.",
    ),
    (
        "after-revert-hook",
        "Normal hook for ‘revert-buffer’ to run after reverting.",
    ),
    (
        "after-save-hook",
        "Normal hook that is run after a buffer is saved to its file.",
    ),
    (
        "after-set-visited-file-name-hook",
        "Normal hook run just after setting visited file name of current buffer.",
    ),
    (
        "after-setting-font-hook",
        "Functions to run after a frame’s font has been changed.",
    ),
    (
        "allout-auto-activation",
        "Configure allout outline mode auto-activation.",
    ),
    (
        "allout-widgets-auto-activation",
        "Activate to enable allout icon graphics wherever allout mode is active.",
    ),
    (
        "amalgamating-undo-limit",
        "The maximum number of changes to possibly amalgamate when undoing changes.",
    ),
    (
        "android-fonts-enumerated",
        "Whether or not fonts have been enumerated already.",
    ),
    (
        "Buffer-menu-buffer-list",
        "The current list of buffers or function to return buffers.",
    ),
    (
        "Buffer-menu-del-char",
        "Character used to flag buffers for deletion.",
    ),
    (
        "Buffer-menu-files-only",
        "Non-nil if the current Buffer Menu lists only file buffers.",
    ),
    (
        "Buffer-menu-filter-predicate",
        "Function to filter out buffers in the buffer list.",
    ),
    (
        "Buffer-menu-group-by",
        "If non-nil, functions to call to divide buffer-menu buffers into groups.",
    ),
    (
        "Buffer-menu-group-sort-by",
        "If non-nil, function to sort buffer-menu groups by name.",
    ),
    (
        "Buffer-menu-marker-char",
        "The mark character for marked buffers.",
    ),
    (
        "Buffer-menu-mode-abbrev-table",
        "Abbrev table for `Buffer-menu-mode'.",
    ),
    (
        "Buffer-menu-mode-hook",
        "Hook run after entering `Buffer-menu-mode'.",
    ),
    (
        "Buffer-menu-mode-map",
        "Local keymap for `Buffer-menu-mode' buffers.",
    ),
    (
        "Buffer-menu-mode-menu",
        "Menu for `Buffer-menu-mode' buffers.",
    ),
    (
        "Buffer-menu-mode-syntax-table",
        "Syntax table for `Buffer-menu-mode'.",
    ),
    (
        "Buffer-menu-mode-width",
        "Width of mode name column in the Buffer Menu.",
    ),
    (
        "Buffer-menu-name-width",
        "Width of buffer name column in the Buffer Menu.",
    ),
    (
        "Buffer-menu-show-internal",
        "Non-nil if the current Buffer Menu lists internal buffers.",
    ),
    (
        "Buffer-menu-size-width",
        "Width of buffer size column in the Buffer Menu.",
    ),
    (
        "Buffer-menu-use-frame-buffer-list",
        "If non-nil, the Buffer Menu uses the selected frame's buffer list.",
    ),
    (
        "Buffer-menu-use-header-line",
        "If non-nil, use the header line to display Buffer Menu column titles.",
    ),
    (
        "Info-default-directory-list",
        "Default list of directories to search for Info documentation files.",
    ),
    (
        "Info-split-threshold",
        "The number of characters by which `Info-split' splits an info file.",
    ),
    (
        "ad-default-compilation-action",
        "Defines whether to compile advised definitions during activation.",
    ),
    (
        "ad-redefinition-action",
        "Defines what to do with redefinitions during Advice de/activation.",
    ),
    (
        "adaptive-fill-first-line-regexp",
        "Regexp specifying whether to set fill prefix from a one-line paragraph.",
    ),
    (
        "adaptive-fill-function",
        "Function to call to choose a fill prefix for a paragraph.",
    ),
    (
        "adaptive-fill-mode",
        "Non-nil means determine a paragraph's fill prefix from its text.",
    ),
    (
        "adaptive-fill-regexp",
        "Regexp to match text at start of line that constitutes indentation.",
    ),
    (
        "add-log-current-defun-function",
        "If non-nil, function to guess name of surrounding function.",
    ),
    (
        "add-log-full-name",
        "Full name of user, for inclusion in ChangeLog daily headers.",
    ),
    (
        "add-log-mailing-address",
        "Email addresses of user, for inclusion in ChangeLog headers.",
    ),
    (
        "advice--how-alist",
        "List of descriptions of how to add a function.",
    ),
    (
        "arabic-shaper-ZWNJ-handling",
        "How to handle ZWNJ (Zero-width Non-Joiner) in Arabic text rendering.",
    ),
    ("argi", "Current command-line argument."),
    ("argv", "List of command-line args not yet processed."),
    (
        "ascii-case-table",
        "Case table for the ASCII character set.",
    ),
    (
        "async-shell-command-buffer",
        "What to do when the output buffer is used by another shell command.",
    ),
    (
        "async-shell-command-display-buffer",
        "Whether to display the command buffer immediately.",
    ),
    (
        "async-shell-command-mode",
        "Major mode to use for the output of asynchronous `shell-command'.",
    ),
    (
        "async-shell-command-width",
        "Number of display columns available for asynchronous shell command output.",
    ),
    (
        "auth-source-cache-expiry",
        "How many seconds passwords are cached, or nil to disable expiring.",
    ),
    (
        "auto-coding-alist",
        "Alist of filename patterns vs corresponding coding systems.",
    ),
    (
        "auto-coding-file-name",
        "Variable holding the name of the file for `auto-coding-functions'.",
    ),
    (
        "auto-coding-functions",
        "A list of functions which attempt to determine a coding system.",
    ),
    (
        "auto-coding-regexp-alist",
        "Alist of patterns vs corresponding coding systems.",
    ),
    (
        "auto-composition-mode-hook",
        "Hook run after entering or leaving `auto-composition-mode'.",
    ),
    (
        "auto-compression-mode",
        "Non-nil if Auto-Compression mode is enabled.",
    ),
    (
        "auto-compression-mode-hook",
        "Hook run after entering or leaving `auto-compression-mode'.",
    ),
    (
        "auto-encryption-mode",
        "Non-nil if Auto-Encryption mode is enabled.",
    ),
    (
        "auto-encryption-mode-hook",
        "Hook run after entering or leaving `auto-encryption-mode'.",
    ),
    (
        "auto-fill-inhibit-regexp",
        "Regexp to match lines that should not be auto-filled.",
    ),
    (
        "auto-fill-mode-hook",
        "Hook run after entering or leaving `auto-fill-mode'.",
    ),
    (
        "auto-image-file-mode",
        "Non-nil if Auto-Image-File mode is enabled.",
    ),
    (
        "auto-insert-mode",
        "Non-nil if Auto-Insert mode is enabled.",
    ),
    (
        "auto-lower-mode-hook",
        "Hook run after entering or leaving `auto-lower-mode'.",
    ),
    (
        "auto-mode-alist",
        "Alist of file name patterns vs corresponding major mode functions.",
    ),
    (
        "auto-mode-case-fold",
        "Non-nil means to try second pass through `auto-mode-alist'.",
    ),
    (
        "auto-mode-interpreter-regexp",
        "Regexp matching interpreters, for file mode determination.",
    ),
    (
        "auto-raise-mode-hook",
        "Hook run after entering or leaving `auto-raise-mode'.",
    ),
    ("auto-save--timer", "Timer for `auto-save-visited-mode'."),
    (
        "auto-save-default",
        "Non-nil says by default do auto-saving of every file-visiting buffer.",
    ),
    (
        "auto-save-file-name-transforms",
        "Transforms to apply to buffer file name before making auto-save file name.",
    ),
    ("auto-save-hook", "Normal hook run just before auto-saving."),
    (
        "auto-save-list-file-prefix",
        "Prefix for generating `auto-save-list-file-name'.",
    ),
    (
        "auto-save-mode-hook",
        "Hook run after entering or leaving `auto-save-mode'.",
    ),
    (
        "auto-save-visited-interval",
        "Interval in seconds for `auto-save-visited-mode'.",
    ),
    (
        "auto-save-visited-mode",
        "Non-nil if Auto-Save-Visited mode is enabled.",
    ),
    (
        "auto-save-visited-mode-hook",
        "Hook run after entering or leaving `auto-save-visited-mode'.",
    ),
    (
        "auto-save-visited-predicate",
        "Predicate function for `auto-save-visited-mode'.",
    ),
    (
        "backquote-backquote-symbol",
        "Symbol used to represent a backquote or nested backquote.",
    ),
    (
        "backquote-splice-symbol",
        "Symbol used to represent a splice inside a backquote.",
    ),
    (
        "backquote-unquote-symbol",
        "Symbol used to represent an unquote inside a backquote.",
    ),
    (
        "backup-by-copying",
        "Non-nil means always use copying to create backup files.",
    ),
    (
        "backup-by-copying-when-linked",
        "Non-nil means use copying to create backups for files with multiple names.",
    ),
    (
        "backup-by-copying-when-mismatch",
        "Non-nil means create backups by copying if this preserves owner or group.",
    ),
    (
        "backup-by-copying-when-privileged-mismatch",
        "Non-nil means create backups by copying to preserve a privileged owner.",
    ),
    (
        "backup-directory-alist",
        "Alist of file name patterns and backup directory names.",
    ),
    (
        "backup-enable-predicate",
        "Predicate that looks at a file name and decides whether to make backups.",
    ),
    ("backup-inhibited", "If non-nil, backups will be inhibited."),
    (
        "backward-delete-char-untabify-method",
        "The method for untabifying when deleting backward.",
    ),
    (
        "bad-packages-alist",
        "Alist of packages known to cause problems in this version of Emacs.",
    ),
    (
        "bdf-directory-list",
        "List of directories to search for `BDF' font files.",
    ),
    (
        "before-hack-local-variables-hook",
        "Normal hook run before setting file-local variables.",
    ),
    (
        "before-init-hook",
        "Normal hook run after handling urgent options but before loading init files.",
    ),
    (
        "before-make-frame-hook",
        "Functions to run before `make-frame' creates a new frame.",
    ),
    (
        "before-revert-hook",
        "Normal hook for `revert-buffer' to run before reverting.",
    ),
    (
        "before-save-hook",
        "Normal hook that is run before a buffer is saved to its file.",
    ),
    (
        "beginning-of-defun-function",
        "If non-nil, function for `beginning-of-defun-raw' to call.",
    ),
    (
        "bengali-composable-pattern",
        "Regexp matching a composable sequence of Bengali characters.",
    ),
    (
        "bidi-control-characters",
        "List of bidirectional control characters.",
    ),
    (
        "bidi-directional-controls-chars",
        "Character set that matches bidirectional formatting control characters.",
    ),
    (
        "bidi-directional-non-controls-chars",
        "Character set that matches any character except bidirectional controls.",
    ),
    (
        "binary-overwrite-mode-hook",
        "Hook run after entering or leaving `binary-overwrite-mode'.",
    ),
    (
        "binhex-begin-line",
        "Regular expression matching the start of a BinHex encoded region.",
    ),
    (
        "blink-cursor-blinks",
        "How many times to blink before using a solid cursor on NS, X, and MS-Windows.",
    ),
    (
        "blink-cursor-blinks-done",
        "Number of blinks done since we started blinking on NS, X, and MS-Windows.",
    ),
    (
        "blink-cursor-delay",
        "Seconds of idle time before the first blink of the cursor.",
    ),
    (
        "blink-cursor-idle-timer",
        "Timer started after `blink-cursor-delay' seconds of Emacs idle time.",
    ),
    (
        "blink-cursor-interval",
        "Length of cursor blink interval in seconds.",
    ),
    (
        "blink-cursor-mode",
        "Non-nil if Blink-Cursor mode is enabled.",
    ),
    (
        "blink-cursor-mode-hook",
        "Hook run after entering or leaving `blink-cursor-mode'.",
    ),
    (
        "blink-cursor-timer",
        "Timer started from `blink-cursor-start'.",
    ),
    (
        "blink-matching--overlay",
        "Overlay used to highlight the matching paren.",
    ),
    (
        "blink-matching-check-function",
        "Function to check parentheses mismatches.",
    ),
    (
        "blink-matching-delay",
        "Time in seconds to delay after showing a matching paren.",
    ),
    (
        "blink-matching-paren",
        "Non-nil means show matching open-paren when close-paren is inserted.",
    ),
    (
        "blink-matching-paren-distance",
        "If non-nil, maximum distance to search backwards for matching open-paren.",
    ),
    (
        "blink-matching-paren-dont-ignore-comments",
        "If nil, `blink-matching-paren' ignores comments.",
    ),
    (
        "blink-matching-paren-highlight-offscreen",
        "If non-nil, highlight matched off-screen open paren in the echo area.",
    ),
    (
        "blink-matching-paren-on-screen",
        "Non-nil means show matching open-paren when it is on screen.",
    ),
    (
        "blink-paren-function",
        "Function called, if non-nil, whenever a close parenthesis is inserted.",
    ),
    (
        "bookmark-map",
        "Keymap containing bindings to bookmark functions.",
    ),
    (
        "break-hardlink-on-save",
        "Whether to allow breaking hardlinks when saving files.",
    ),
    (
        "browse-url-browser-function",
        "Function to display the current buffer in a WWW browser.",
    ),
    (
        "browse-url-default-handlers",
        "Like `browse-url-handlers' but populated by Emacs and packages.",
    ),
    (
        "buffer-auto-revert-by-notification",
        "Whether a buffer can rely on notification in Auto-Revert mode.",
    ),
    (
        "buffer-file-coding-system-explicit",
        "The file coding system explicitly specified for the current buffer.",
    ),
    (
        "buffer-file-number",
        "The inode number and the device of the file visited in the current buffer.",
    ),
    (
        "buffer-file-numbers-unique",
        "Non-nil means that `buffer-file-number' uniquely identifies files.",
    ),
    (
        "buffer-file-read-only",
        "Non-nil if visited file was read-only when visited.",
    ),
    (
        "buffer-navigation-repeat-map",
        "Keymap to repeat `next-buffer' and `previous-buffer'.  Used in `repeat-mode'.",
    ),
    (
        "buffer-offer-save",
        "Non-nil in a buffer means always offer to save buffer on exiting Emacs.",
    ),
    (
        "buffer-quit-function",
        "Function to call to \"quit\" the current buffer, or nil if none.",
    ),
    (
        "buffer-save-without-query",
        "Non-nil means `save-some-buffers' should save this buffer without asking.",
    ),
    (
        "buffer-stale-function",
        "Function to check whether a buffer needs reverting.",
    ),
    (
        "buffers-menu-buffer-name-length",
        "Maximum length of the buffer name on the Buffers menu.",
    ),
    (
        "buffers-menu-max-size",
        "Maximum number of entries which may appear on the Buffers menu.",
    ),
    (
        "buffers-menu-show-directories",
        "If non-nil, show directories in the Buffers menu for buffers that have them.",
    ),
    (
        "buffers-menu-show-status",
        "If non-nil, show modified/read-only status of buffers in the Buffers menu.",
    ),
    (
        "button-buffer-map",
        "Keymap useful for buffers containing buttons.",
    ),
    ("button-map", "Keymap used by buttons."),
    ("button-mode", "Non-nil if Button mode is enabled."),
    (
        "button-mode-hook",
        "Hook run after entering or leaving `button-mode'.",
    ),
    (
        "byte-compile-form-stack",
        "Dynamic list of successive enclosing forms.",
    ),
    (
        "byte-count-to-string-function",
        "Function that turns a number of bytes into a human-readable string.",
    ),
    (
        "byte-run--ssp-seen",
        "Which conses/vectors/records have been processed in strip-symbol-positions?",
    ),
    (
        "c-guess-guessed-basic-offset",
        "Currently guessed basic-offset.",
    ),
    (
        "c-guess-guessed-offsets-alist",
        "Currently guessed offsets-alist.",
    ),
    (
        "called-interactively-p-functions",
        "Special hook called to skip special frames in `called-interactively-p'.",
    ),
    (
        "case-replace",
        "Non-nil means `query-replace' should preserve case in replacements.",
    ),
    (
        "ccl-encode-ethio-font",
        "CCL program to encode an Ethiopic code to code point of Ethiopic font.",
    ),
    (
        "cconv--dynbound-variables",
        "List of variables known to be dynamically bound.",
    ),
    (
        "cconv-liftwhen",
        "Try to do lambda lifting if the number of arguments + free variables",
    ),
    (
        "cd-path",
        "Value of the CDPATH environment variable, as a list.",
    ),
    (
        "change-major-mode-after-body-hook",
        "Normal hook run in major mode functions, before the mode hooks.",
    ),
    (
        "change-major-mode-with-file-name",
        "Non-nil means \\[write-file] should set the major mode from the file name.",
    ),
    (
        "char-acronym-table",
        "Char table of acronyms for non-graphic characters.",
    ),
    (
        "char-code-property-table",
        "Char-table containing a property list of each character code.",
    ),
    (
        "charset-script-alist",
        "Alist of charsets vs the corresponding most appropriate scripts.",
    ),
    (
        "choose-completion-deselect-if-after",
        "If non-nil, don't choose a completion candidate if point is right after it.",
    ),
    (
        "choose-completion-string-functions",
        "Functions that may override the normal insertion of a completion choice.",
    ),
    (
        "cjk-ambiguous-chars-are-wide",
        "Whether the \"ambiguous-width\" characters take 2 columns on display.",
    ),
    (
        "cl--generic-combined-method-memoization",
        "Table storing previously built combined-methods.",
    ),
    (
        "cl-custom-print-functions",
        "This is a list of functions that format user objects for printing.",
    ),
    (
        "cl-font-lock-built-in-mode",
        "Non-nil if Cl-Font-Lock-Built-In mode is enabled.",
    ),
    (
        "cl-old-struct-compat-mode",
        "Non-nil if Cl-Old-Struct-Compat mode is enabled.",
    ),
    ("clean-mode-abbrev-table", "Abbrev table for `clean-mode'."),
    ("clean-mode-hook", "Hook run after entering `clean-mode'."),
    ("clean-mode-map", "Keymap for `clean-mode'."),
    ("clean-mode-syntax-table", "Syntax table for `clean-mode'."),
    (
        "clone-buffer-hook",
        "Normal hook to run in the new buffer at the end of `clone-buffer'.",
    ),
    (
        "coding-system-iso-2022-flags",
        "List of symbols that control ISO-2022 encoder/decoder.",
    ),
    (
        "colon-double-space",
        "Non-nil means put two spaces after a colon when filling.",
    ),
    (
        "color-luminance-dark-limit",
        "The relative luminance below which a color is considered \"dark\".",
    ),
    (
        "color-name-rgb-alist",
        "An alist of X color names and associated 16-bit RGB values.",
    ),
    (
        "column-number-indicator-zero-based",
        "When non-nil, mode line displays column numbers zero-based.",
    ),
    (
        "column-number-mode",
        "Non-nil if Column-Number mode is enabled.",
    ),
    (
        "column-number-mode-hook",
        "Hook run after entering or leaving `column-number-mode'.",
    ),
    (
        "comint-file-name-prefix",
        "Prefix prepended to absolute file names taken from process input.",
    ),
    (
        "comint-output-filter-functions",
        "Functions to call after output is inserted into the buffer.",
    ),
    (
        "command-line-args-left",
        "List of command-line args not yet processed.",
    ),
    (
        "command-line-default-directory",
        "Default directory to use for command line arguments.",
    ),
    (
        "command-line-functions",
        "List of functions to process unrecognized command-line arguments.",
    ),
    ("command-line-ns-option-alist", "Alist of NS options."),
    (
        "command-line-processed",
        "Non-nil once command line has been processed.",
    ),
    ("command-line-x-option-alist", "Alist of X Windows options."),
    ("command-switch-alist", "Alist of command-line switches."),
    (
        "comment-add",
        "How many more comment chars should be inserted by `comment-region'.",
    ),
    (
        "comment-auto-fill-only-comments",
        "Non-nil means to only auto-fill inside comments.",
    ),
    (
        "comment-column",
        "Column to indent right-margin comments to.",
    ),
    (
        "comment-combine-change-calls",
        "If non-nil (the default), use `combine-change-calls' around",
    ),
    (
        "comment-continue",
        "Continuation string to insert for multiline comments.",
    ),
    (
        "comment-empty-lines",
        "If nil, `comment-region' does not comment out empty lines.",
    ),
    ("comment-end", "String to insert to end a new comment."),
    (
        "comment-end-skip",
        "Regexp to match the end of a comment plus everything back to its body.",
    ),
    (
        "comment-fill-column",
        "Column to use for `comment-indent'.  If nil, use `fill-column' instead.",
    ),
    (
        "comment-indent-function",
        "Function to compute desired indentation for a comment.",
    ),
    (
        "comment-inline-offset",
        "Inline comments have to be preceded by at least this many spaces.",
    ),
    (
        "comment-insert-comment-function",
        "Function to insert a comment when a line doesn't contain one.",
    ),
    (
        "comment-line-break-function",
        "Mode-specific function that line breaks and continues a comment.",
    ),
    (
        "comment-multi-line",
        "Non-nil means `comment-indent-new-line' continues comments.",
    ),
    (
        "comment-padding",
        "Padding string that `comment-region' puts between comment chars and text.",
    ),
    (
        "comment-quote-nested",
        "Non-nil if nested comments should be quoted.",
    ),
    (
        "comment-quote-nested-function",
        "Function to quote nested comments in a region.",
    ),
    ("comment-region-function", "Function to comment a region."),
    (
        "comment-start",
        "String to insert to start a new comment, or nil if no comment syntax.",
    ),
    (
        "comment-start-skip",
        "Regexp to match the start of a comment plus everything up to its body.",
    ),
    ("comment-style", "Style to be used for `comment-region'."),
    ("comment-styles", "Comment region style definitions."),
    (
        "comment-use-global-state",
        "Non-nil means that the global syntactic context is used.",
    ),
    (
        "comment-use-syntax",
        "Non-nil if syntax-tables can be used instead of regexps.",
    ),
    (
        "compilation-ask-about-save",
        "Non-nil means \\[compile] asks which buffers to save before compiling.",
    ),
    (
        "compilation-buffer-name-function",
        "Function to compute the name of a compilation buffer.",
    ),
    (
        "compilation-disable-input",
        "If non-nil, send end-of-file as compilation process input.",
    ),
    (
        "compilation-finish-functions",
        "Functions to call when a compilation process finishes.",
    ),
    (
        "compilation-mode-hook",
        "List of hook functions run by `compilation-mode'.",
    ),
    (
        "compilation-process-setup-function",
        "Function to call to customize the compilation process.",
    ),
    (
        "compilation-search-path",
        "List of directories to search for source files named in error messages.",
    ),
    (
        "compilation-start-hook",
        "Hook run after starting a new compilation process.",
    ),
    (
        "compilation-window-height",
        "Number of lines in a compilation window.",
    ),
    (
        "compile-command",
        "Last shell command used to do a compilation; default for next compilation.",
    ),
    (
        "completing-read-function",
        "The function called by `completing-read' to do its work.",
    ),
    (
        "completion--capf-misbehave-funs",
        "List of functions found on `completion-at-point-functions' that misbehave.",
    ),
    (
        "completion--capf-safe-funs",
        "List of well-behaved functions found on `completion-at-point-functions'.",
    ),
    (
        "completion--flex-score-last-md",
        "Helper variable for `completion--flex-score'.",
    ),
    (
        "completion-at-point-functions",
        "Special hook to find the completion table for the entity at point.",
    ),
    (
        "completion-auto-deselect",
        "If non-nil, deselect current completion candidate when you type in minibuffer.",
    ),
    (
        "completion-auto-help",
        "Non-nil means automatically provide help for invalid completion input.",
    ),
    (
        "completion-auto-select",
        "If non-nil, automatically select the window showing the *Completions* buffer.",
    ),
    (
        "completion-auto-wrap",
        "Non-nil means to wrap around when selecting completion candidates.",
    ),
    (
        "completion-base-position",
        "Position of the base of the text corresponding to the shown completions.",
    ),
    (
        "completion-category-defaults",
        "Default settings for specific completion categories.",
    ),
    (
        "completion-category-overrides",
        "List of category-specific user overrides for completion metadata.",
    ),
    (
        "completion-cycle-threshold",
        "Number of completion candidates below which cycling is used.",
    ),
    (
        "completion-extra-properties",
        "Property list of extra properties of the current completion job.",
    ),
    (
        "completion-fail-discreetly",
        "If non-nil, stay quiet when there is no match.",
    ),
    (
        "completion-flex-nospace",
        "Non-nil if `flex' completion rejects spaces in search pattern.",
    ),
    (
        "completion-in-region-function",
        "Function to perform the job of `completion-in-region'.",
    ),
    (
        "completion-in-region-functions",
        "Wrapper hook around `completion--in-region'.",
    ),
    (
        "completion-in-region-mode--predicate",
        "Copy of the value of `completion-in-region-mode-predicate'.",
    ),
    (
        "completion-in-region-mode-hook",
        "Hook run after entering or leaving `completion-in-region-mode'.",
    ),
    (
        "completion-in-region-mode-map",
        "Keymap activated during `completion-in-region'.",
    ),
    (
        "completion-in-region-mode-predicate",
        "Predicate to tell `completion-in-region-mode' when to exit.",
    ),
    (
        "completion-lazy-hilit",
        "If non-nil, request lazy highlighting of completion candidates.",
    ),
    (
        "completion-lazy-hilit-fn",
        "Fontification function set by lazy-highlighting completions styles.",
    ),
    (
        "completion-list-insert-choice-function",
        "Function to use to insert the text chosen in *Completions*.",
    ),
    (
        "completion-list-mode-abbrev-table",
        "Abbrev table for `completion-list-mode'.",
    ),
    (
        "completion-list-mode-hook",
        "Hook run after entering `completion-list-mode'.",
    ),
    (
        "completion-list-mode-map",
        "Local map for completion list buffers.",
    ),
    (
        "completion-list-mode-syntax-table",
        "Syntax table for `completion-list-mode'.",
    ),
    (
        "completion-no-auto-exit",
        "Non-nil means `choose-completion-string' should never exit the minibuffer.",
    ),
    (
        "completion-pcm--delim-wild-regex",
        "Regular expression matching delimiters controlling the partial-completion.",
    ),
    (
        "completion-pcm--regexp",
        "Regexp from PCM pattern in `completion-pcm--hilit-commonality'.",
    ),
    (
        "completion-pcm-complete-word-inserts-delimiters",
        "Treat the SPC or - inserted by `minibuffer-complete-word' as delimiters.",
    ),
    (
        "completion-pcm-word-delimiters",
        "A string of characters treated as word delimiters for completion.",
    ),
    (
        "completion-reference-buffer",
        "Record the buffer that was current when the completion list was requested.",
    ),
    (
        "completion-setup-hook",
        "Normal hook run at the end of setting up a completion list buffer.",
    ),
    (
        "completion-show-help",
        "Non-nil means show help message in *Completions* buffer.",
    ),
    (
        "completion-show-inline-help",
        "If non-nil, print helpful inline messages during completion.",
    ),
    ("completion-styles", "List of completion styles to use."),
    (
        "completion-styles-alist",
        "List of available completion styles.",
    ),
    (
        "completions-detailed",
        "When non-nil, display completions with details added as prefix/suffix.",
    ),
    (
        "completions-format",
        "Define the appearance and sorting of completions.",
    ),
    (
        "completions-group",
        "Enable grouping of completion candidates in the *Completions* buffer.",
    ),
    (
        "completions-group-format",
        "Format string used for the group title.",
    ),
    (
        "completions-group-sort",
        "Sort groups in the *Completions* buffer.",
    ),
    (
        "completions-header-format",
        "If non-nil, the format string for completions heading line.",
    ),
    (
        "completions-highlight-face",
        "A face name to highlight the current completion candidate.",
    ),
    (
        "completions-max-height",
        "Maximum height for *Completions* buffer window.",
    ),
    (
        "completions-sort",
        "Sort candidates in the *Completions* buffer.",
    ),
    (
        "compose-mail-user-agent-warnings",
        "If non-nil, `compose-mail' warns about changes in `mail-user-agent'.",
    ),
    (
        "confirm-kill-emacs",
        "How to ask for confirmation when leaving Emacs.",
    ),
    (
        "confirm-kill-processes",
        "Non-nil if Emacs should confirm killing processes on exit.",
    ),
    (
        "confirm-nonexistent-file-or-buffer",
        "Whether confirmation is requested before visiting a new file or buffer.",
    ),
    (
        "context-menu-entry",
        "Menu item that creates the context menu and can be bound to a mouse key.",
    ),
    (
        "context-menu-filter-function",
        "Function that can filter the list produced by `context-menu-functions'.",
    ),
    (
        "context-menu-functions",
        "List of functions that produce the contents of the context menu.",
    ),
    (
        "context-menu-mode",
        "Non-nil if Context-Menu mode is enabled.",
    ),
    (
        "context-menu-mode-hook",
        "Hook run after entering or leaving `context-menu-mode'.",
    ),
    ("context-menu-mode-map", "Context Menu mode map."),
    (
        "copy-directory-create-symlink",
        "This option influences the handling of symbolic links in `copy-directory'.",
    ),
    (
        "copy-region-blink-delay",
        "Time in seconds to delay after showing the other end of the region.",
    ),
    (
        "copy-region-blink-predicate",
        "Whether the cursor must be blinked after a copy.",
    ),
    (
        "cpp-font-lock-keywords",
        "Font lock keywords for C preprocessor directives.",
    ),
    (
        "cpp-font-lock-keywords-source-depth",
        "Regular expression depth of `cpp-font-lock-keywords-source-directives'.",
    ),
    (
        "cpp-font-lock-keywords-source-directives",
        "Regular expression used in `cpp-font-lock-keywords'.",
    ),
    (
        "ctext-non-standard-encodings",
        "List of non-standard encoding names used in extended segments of CTEXT.",
    ),
    (
        "ctext-non-standard-encodings-alist",
        "Alist of non-standard encoding names vs the corresponding usages in CTEXT.",
    ),
    (
        "ctext-standard-encodings",
        "List of approved standard encodings (i.e. charsets) of X's Compound Text.",
    ),
    ("ctl-x-4-map", "Keymap for subcommands of C-x 4."),
    ("ctl-x-5-map", "Keymap for frame commands."),
    ("ctl-x-map", "Default keymap for \\`C-x' commands."),
    ("ctl-x-r-map", "Keymap for subcommands of \\`C-x r'."),
    ("ctl-x-x-map", "Keymap for subcommands of \\`C-x x'."),
    ("cua-mode", "Non-nil if Cua mode is enabled."),
    (
        "current-input-method",
        "The current input method for multilingual text.",
    ),
    (
        "current-input-method-title",
        "Title string of the current input method shown in mode line.",
    ),
    (
        "current-language-environment",
        "The last language environment specified with `set-language-environment'.",
    ),
    (
        "current-locale-environment",
        "The currently set locale environment.",
    ),
    (
        "current-transient-input-method",
        "Current input method temporarily enabled by `activate-transient-input-method'.",
    ),
    (
        "cursor-face-highlight-mode",
        "Non-nil if Cursor-Face-Highlight mode is enabled.",
    ),
    (
        "cursor-face-highlight-mode-hook",
        "Hook run after entering or leaving `cursor-face-highlight-mode'.",
    ),
    (
        "cursor-face-highlight-nonselected-window",
        "Non-nil means highlight text with `cursor-face' even in nonselected windows.",
    ),
    (
        "cursor-sensor-inhibit",
        "When non-nil, suspend `cursor-sensor-mode' and `cursor-intangible-mode'.",
    ),
    (
        "custom--inhibit-theme-enable",
        "Whether the custom-theme-set-* functions act immediately.",
    ),
    (
        "custom-browse-sort-alphabetically",
        "If non-nil, sort customization group alphabetically in `custom-browse'.",
    ),
    (
        "custom-buffer-sort-alphabetically",
        "Whether to sort customization groups alphabetically in Custom buffer.",
    ),
    (
        "custom-current-group-alist",
        "Alist of (FILE . GROUP) indicating the current group to use for FILE.",
    ),
    (
        "custom-define-hook",
        "Hook called after defining each customize option.",
    ),
    (
        "custom-delayed-init-variables",
        "List of variables whose initialization is pending until startup.",
    ),
    (
        "custom-dont-initialize",
        "Non-nil means `defcustom' should not initialize the variable.",
    ),
    (
        "custom-enabled-themes",
        "List of enabled Custom Themes, highest precedence first.",
    ),
    ("custom-face-attributes", "Alist of face attributes."),
    (
        "custom-file",
        "File used for storing customization information.",
    ),
    (
        "custom-known-themes",
        "Themes that have been defined with `deftheme'.",
    ),
    (
        "custom-load-recursion",
        "Hack to avoid recursive dependencies.",
    ),
    (
        "custom-local-buffer",
        "Non-nil, in a Customization buffer, means customize a specific buffer.",
    ),
    (
        "custom-menu-sort-alphabetically",
        "If non-nil, sort each customization group alphabetically in menus.",
    ),
    (
        "custom-safe-themes",
        "Themes that are considered safe to load.",
    ),
    (
        "custom-theme-directory",
        "Default user directory for storing custom theme files.",
    ),
    (
        "custom-theme-load-path",
        "List of directories to search for custom theme files.",
    ),
    (
        "customize-package-emacs-version-alist",
        "Alist mapping versions of a package to Emacs versions.",
    ),
    (
        "cvs-dired-action",
        "The action to be performed when opening a CVS directory.",
    ),
    (
        "cvs-dired-use-hook",
        "Whether or not opening a CVS directory should run PCL-CVS.",
    ),
    ("cvs-global-menu", "Global menu used by PCL-CVS."),
    (
        "cycle-spacing--context",
        "Stored context used in consecutive calls to `cycle-spacing' command.",
    ),
    (
        "cycle-spacing-actions",
        "List of actions cycled through by `cycle-spacing'.",
    ),
    (
        "deactivate-current-input-method-function",
        "Function to call for deactivating the current input method.",
    ),
    (
        "deactivate-mark-hook",
        "Hook run when the mark becomes inactive.",
    ),
    (
        "default-input-method",
        "Default input method for multilingual text (a string).",
    ),
    (
        "default-justification",
        "Method of justifying text not otherwise specified.",
    ),
    (
        "default-keyboard-coding-system",
        "Default value of the keyboard coding system.",
    ),
    (
        "default-korean-keyboard",
        "The kind of Korean keyboard for Korean (Hangul) input method.",
    ),
    (
        "default-sendmail-coding-system",
        "Default coding system for encoding the outgoing mail.",
    ),
    (
        "default-terminal-coding-system",
        "Default value for the terminal coding system.",
    ),
    (
        "default-transient-input-method",
        "Default transient input method.",
    ),
    (
        "definition-prefixes",
        "Hash table mapping prefixes to the files in which they're used.",
    ),
    (
        "defun-declarations-alist",
        "List associating function properties to their macro expansion.",
    ),
    (
        "defun-prompt-regexp",
        "If non-nil, a regexp to ignore before a defun.",
    ),
    (
        "degrees-to-radians",
        "Degrees to radian conversion constant.",
    ),
    (
        "delay-mode-hooks",
        "If non-nil, `run-mode-hooks' should delay running the hooks.",
    ),
    (
        "delayed-after-hook-functions",
        "List of delayed :after-hook forms waiting to be run.",
    ),
    (
        "delayed-mode-hooks",
        "List of delayed mode hooks waiting to be run.",
    ),
    (
        "delayed-warnings-hook",
        "Normal hook run to process and display delayed warnings.",
    ),
    (
        "delete-active-region",
        "Whether single-char deletion commands delete an active region.",
    ),
    (
        "delete-old-versions",
        "If t, delete excess numbered backup files silently.",
    ),
    (
        "delete-pair-blink-delay",
        "Time in seconds to delay after showing a paired character to delete.",
    ),
    (
        "delete-selection-mode",
        "Non-nil if Delete-Selection mode is enabled.",
    ),
    (
        "delete-trailing-lines",
        "If non-nil, \\[delete-trailing-whitespace] deletes trailing lines.",
    ),
    (
        "delete-window-choose-selected",
        "How to choose a frame's selected window after window deletion.",
    ),
    (
        "describe-bindings-outline",
        "Non-nil enables outlines in the output buffer of `describe-bindings'.",
    ),
    (
        "describe-bindings-outline-rules",
        "Visibility rules for outline sections of `describe-bindings'.",
    ),
    (
        "describe-bindings-show-prefix-commands",
        "Non-nil means show prefix commands in the output of `describe-bindings'.",
    ),
    (
        "describe-current-input-method-function",
        "Function to call for describing the current input method.",
    ),
    (
        "desktop-buffer-mode-handlers",
        "Alist of major mode specific functions to restore a desktop buffer.",
    ),
    (
        "desktop-locals-to-save",
        "List of local variables to save for each buffer.",
    ),
    (
        "desktop-minor-mode-handlers",
        "Alist of functions to restore non-standard minor modes.",
    ),
    (
        "desktop-save-buffer",
        "When non-nil, save buffer status in desktop file.",
    ),
    (
        "desktop-save-mode",
        "Non-nil if Desktop-Save mode is enabled.",
    ),
    (
        "devanagari-composable-pattern",
        "Regexp matching a composable sequence of Devanagari characters.",
    ),
    (
        "diff-add-log-use-relative-names",
        "Use relative file names when generating ChangeLog skeletons.",
    ),
    ("diff-command", "The command to use to run diff."),
    (
        "diff-switches",
        "A string or list of strings specifying switches to be passed to diff.",
    ),
    (
        "dir-local-variables-alist",
        "Alist of directory-local variable settings in the current buffer.",
    ),
    (
        "dir-locals-class-alist",
        "Alist mapping directory-local variable classes (symbols) to variable lists.",
    ),
    (
        "dir-locals-directory-cache",
        "List of cached directory roots for directory-local variable classes.",
    ),
    (
        "dir-locals-file",
        "File that contains directory-local variables.",
    ),
    (
        "directory-abbrev-alist",
        "Alist of abbreviations for file directories.",
    ),
    (
        "directory-files-no-dot-files-regexp",
        "Regexp matching any file name except \".\" and \"..\".",
    ),
    (
        "directory-free-space-args",
        "Options to use when running `directory-free-space-program'.",
    ),
    (
        "directory-free-space-program",
        "Program to get the amount of free space on a file system.",
    ),
    (
        "directory-listing-before-filename-regexp",
        "Regular expression to match up to the file name in a directory listing.",
    ),
    (
        "dired-directory",
        "The directory name or wildcard spec that this Dired directory lists.",
    ),
    (
        "dired-kept-versions",
        "When cleaning directory, number of versions of numbered backups to keep.",
    ),
    (
        "dired-listing-switches",
        "Switches passed to `ls' for Dired.  MUST contain the `l' option.",
    ),
    (
        "disable-theme-functions",
        "Abnormal hook that is run after a theme has been disabled.",
    ),
    (
        "disabled-command-function",
        "Function to call to handle disabled commands.",
    ),
    (
        "display-battery-mode",
        "Non-nil if Display-Battery mode is enabled.",
    ),
    (
        "display-buffer--action-custom-type",
        "Custom type for `display-buffer' actions.",
    ),
    (
        "display-buffer--action-function-custom-type",
        "Custom type for `display-buffer' action functions.",
    ),
    (
        "display-buffer--other-frame-action",
        "A `display-buffer' action for displaying in another frame.",
    ),
    (
        "display-buffer--same-window-action",
        "A `display-buffer' action for displaying in the same window.",
    ),
    (
        "display-buffer-alist",
        "Alist of user-defined conditional actions for `display-buffer'.",
    ),
    (
        "display-buffer-base-action",
        "User-specified default action for `display-buffer'.",
    ),
    (
        "display-buffer-fallback-action",
        "Default fallback action for `display-buffer'.",
    ),
    (
        "display-buffer-mark-dedicated",
        "If non-nil, `display-buffer' marks the windows it creates as dedicated.",
    ),
    (
        "display-buffer-overriding-action",
        "Overriding action for buffer display.",
    ),
    (
        "display-buffer-reuse-frames",
        "Non-nil means `display-buffer' should reuse frames.",
    ),
    (
        "display-comint-buffer-action",
        "`display-buffer' action for displaying comint buffers.",
    ),
    (
        "display-format-alist",
        "Alist of patterns to decode display names.",
    ),
    (
        "display-mm-dimensions-alist",
        "Alist for specifying screen dimensions in millimeters.",
    ),
    (
        "display-tex-shell-buffer-action",
        "`display-buffer' action for displaying TeX shell buffers.",
    ),
    (
        "display-time-day-and-date",
        "Non-nil means \\[display-time] should display day and date as well as time.",
    ),
    (
        "display-time-mode",
        "Non-nil if Display-Time mode is enabled.",
    ),
    (
        "dnd-direct-save-remote-files",
        "Whether or not to perform a direct save of remote files.",
    ),
    (
        "dnd-indicate-insertion-point",
        "Whether or not point should follow the position of the mouse.",
    ),
    (
        "dnd-last-dragged-remote-file",
        "If non-nil, the name of a local copy of the last remote file that was dragged.",
    ),
    (
        "dnd-open-file-other-window",
        "If non-nil, always use `find-file-other-window' to open dropped files.",
    ),
    (
        "dnd-open-remote-file-function",
        "The function to call when opening a file on a remote machine.",
    ),
    (
        "dnd-protocol-alist",
        "The functions to call for different protocols when a drop is made.",
    ),
    (
        "dnd-scroll-margin",
        "The scroll margin inside a window underneath the cursor during drag-and-drop.",
    ),
    (
        "dnd-unescape-file-uris",
        "Whether to unescape file: URIs before they are opened.",
    ),
    (
        "dynamic-completion-mode",
        "Non-nil if Dynamic-Completion mode is enabled.",
    ),
    (
        "early-init-file",
        "File name, including directory, of user's early init file.",
    ),
    (
        "easy-menu-avoid-duplicate-keys",
        "Dynamically scoped var to register already used keys in a menu.",
    ),
    (
        "edebug-all-defs",
        "If non-nil, evaluating defining forms instruments for Edebug.",
    ),
    (
        "edebug-all-forms",
        "Non-nil means evaluation of all forms will instrument for Edebug.",
    ),
    (
        "edit-abbrevs-mode-abbrev-table",
        "Abbrev table for `edit-abbrevs-mode'.",
    ),
    (
        "edit-abbrevs-mode-hook",
        "Hook run after entering `edit-abbrevs-mode'.",
    ),
    ("edit-abbrevs-mode-map", "Keymap used in `edit-abbrevs'."),
    (
        "edit-abbrevs-mode-syntax-table",
        "Syntax table for `edit-abbrevs-mode'.",
    ),
    (
        "edit-tab-stops-buffer",
        "Buffer whose tab stops are being edited.",
    ),
    ("edit-tab-stops-map", "Keymap used in `edit-tab-stops'."),
    (
        "editorconfig-mode",
        "Non-nil if Editorconfig mode is enabled.",
    ),
    (
        "eldoc--doc-buffer",
        "Buffer displaying latest ElDoc-produced docs.",
    ),
    (
        "eldoc--enthusiasm-curbing-timer",
        "Timer used by the `eldoc-documentation-enthusiast' strategy.",
    ),
    (
        "eldoc--last-request-state",
        "Tuple containing information about last ElDoc request.",
    ),
    (
        "eldoc--make-callback",
        "Helper for function `eldoc--make-callback'.",
    ),
    (
        "eldoc-argument-case",
        "Case to display argument names of functions, as a symbol.",
    ),
    (
        "eldoc-current-idle-delay",
        "Idle time delay currently in use by timer.",
    ),
    (
        "eldoc-display-functions",
        "Hook of functions tasked with displaying ElDoc results.",
    ),
    (
        "eldoc-doc-buffer-separator",
        "String used to separate items in Eldoc documentation buffer.",
    ),
    (
        "eldoc-documentation-function",
        "How to collect and display results of `eldoc-documentation-functions'.",
    ),
    (
        "eldoc-documentation-functions",
        "Hook of functions that produce doc strings.",
    ),
    (
        "eldoc-documentation-strategy",
        "How to collect and display results of `eldoc-documentation-functions'.",
    ),
    (
        "eldoc-echo-area-display-truncation-message",
        "If non-nil, provide verbose help when a message has been truncated.",
    ),
    (
        "eldoc-echo-area-prefer-doc-buffer",
        "Prefer ElDoc's documentation buffer if it is displayed in some window.",
    ),
    (
        "eldoc-echo-area-use-multiline-p",
        "Allow long ElDoc doc strings to resize echo area display.",
    ),
    (
        "eldoc-idle-delay",
        "Number of seconds of idle time to wait before displaying documentation.",
    ),
    ("eldoc-last-data", "Bookkeeping; elements are as follows:"),
    (
        "eldoc-message-commands",
        "Commands after which it is appropriate to print in the echo area.",
    ),
    (
        "eldoc-message-commands-table-size",
        "Used by `eldoc-add-command' to initialize `eldoc-message-commands' obarray.",
    ),
    (
        "eldoc-message-function",
        "The function used by `eldoc--message' to display messages.",
    ),
    (
        "eldoc-minor-mode-string",
        "String to display in mode line when ElDoc Mode is enabled; nil for none.",
    ),
    ("eldoc-mode", "Non-nil if Eldoc mode is enabled."),
    (
        "eldoc-mode-hook",
        "Hook run after entering or leaving `eldoc-mode'.",
    ),
    (
        "eldoc-print-after-edit",
        "If non-nil, eldoc info is only shown after editing commands.",
    ),
    ("eldoc-timer", "ElDoc's timer object."),
    (
        "electric-indent-chars",
        "Characters that should cause automatic reindentation.",
    ),
    (
        "electric-indent-functions",
        "Special hook run to decide whether to auto-indent.",
    ),
    (
        "electric-indent-functions-without-reindent",
        "List of indent functions that can't reindent.",
    ),
    (
        "electric-indent-inhibit",
        "If non-nil, reindentation is not appropriate for this buffer.",
    ),
    (
        "electric-indent-local-mode-hook",
        "Hook run after entering or leaving `electric-indent-local-mode'.",
    ),
    (
        "electric-indent-mode",
        "Non-nil if Electric-Indent mode is enabled.",
    ),
    (
        "electric-indent-mode-hook",
        "Hook run after entering or leaving `electric-indent-mode'.",
    ),
    (
        "electric-layout-allow-duplicate-newlines",
        "If non-nil, allow duplication of `before' newlines.",
    ),
    (
        "electric-layout-local-mode-hook",
        "Hook run after entering or leaving `electric-layout-local-mode'.",
    ),
    (
        "electric-layout-mode",
        "Non-nil if Electric-Layout mode is enabled.",
    ),
    (
        "electric-layout-mode-hook",
        "Hook run after entering or leaving `electric-layout-mode'.",
    ),
    (
        "electric-layout-rules",
        "List of rules saying where to automatically insert newlines.",
    ),
    (
        "electric-pair-mode",
        "Non-nil if Electric-Pair mode is enabled.",
    ),
    (
        "electric-quote-chars",
        "Curved quote characters for `electric-quote-mode'.",
    ),
    (
        "electric-quote-comment",
        "Non-nil means to use electric quoting in program comments.",
    ),
    (
        "electric-quote-context-sensitive",
        "Non-nil means to replace \\=' with an electric quote depending on context.",
    ),
    (
        "electric-quote-inhibit-functions",
        "List of functions that should inhibit electric quoting.",
    ),
    (
        "electric-quote-local-mode-hook",
        "Hook run after entering or leaving `electric-quote-local-mode'.",
    ),
    (
        "electric-quote-mode",
        "Non-nil if Electric-Quote mode is enabled.",
    ),
    (
        "electric-quote-mode-hook",
        "Hook run after entering or leaving `electric-quote-mode'.",
    ),
    (
        "electric-quote-paragraph",
        "Non-nil means to use electric quoting in text paragraphs.",
    ),
    (
        "electric-quote-replace-consecutive",
        "Non-nil means to replace a pair of single quotes with a double quote.",
    ),
    (
        "electric-quote-replace-double",
        "Non-nil means to replace \" with an electric double quote.",
    ),
    (
        "electric-quote-string",
        "Non-nil means to use electric quoting in program strings.",
    ),
    ("elisp--eldoc-last-data", "Bookkeeping."),
    (
        "elisp--local-macroenv",
        "Environment to use while tentatively expanding macros.",
    ),
    (
        "elisp-byte-code-mode-abbrev-table",
        "Abbrev table for `elisp-byte-code-mode'.",
    ),
    (
        "elisp-byte-code-mode-hook",
        "Hook run after entering `elisp-byte-code-mode'.",
    ),
    (
        "elisp-byte-code-mode-map",
        "Keymap for `elisp-byte-code-mode'.",
    ),
    (
        "elisp-byte-code-mode-syntax-table",
        "Syntax table for `elisp-byte-code-mode'.",
    ),
    (
        "elisp-flymake--byte-compile-process",
        "Buffer-local process started for byte-compiling the buffer.",
    ),
    (
        "elisp-flymake-byte-compile-load-path",
        "Like `load-path' but used by `elisp-flymake-byte-compile'.",
    ),
    (
        "elisp-xref-find-def-functions",
        "List of functions run from `elisp--xref-find-definitions' to add more xrefs.",
    ),
    (
        "emacs-build-number",
        "The build number of this version of Emacs.",
    ),
    (
        "emacs-build-system",
        "Name of the system on which Emacs was built, or nil if not available.",
    ),
    (
        "emacs-build-time",
        "Time at which Emacs was dumped out, or nil if not available.",
    ),
    (
        "emacs-lisp-byte-code-comment-re",
        "Regular expression matching a dynamic doc string comment.",
    ),
    (
        "emacs-lisp-docstring-fill-column",
        "Value of `fill-column' to use when filling a docstring.",
    ),
    (
        "emacs-lisp-mode-abbrev-table",
        "Abbrev table for Emacs Lisp mode.",
    ),
    (
        "emacs-lisp-mode-hook",
        "Hook run when entering Emacs Lisp mode.",
    ),
    ("emacs-lisp-mode-map", "Keymap for Emacs Lisp mode."),
    ("emacs-lisp-mode-menu", "Menu for Emacs Lisp mode."),
    (
        "emacs-lisp-mode-syntax-table",
        "Syntax table used in `emacs-lisp-mode'.",
    ),
    (
        "emacs-major-version",
        "Major version number of this version of Emacs.",
    ),
    (
        "emacs-minor-version",
        "Minor version number of this version of Emacs.",
    ),
    (
        "emacs-repository-branch",
        "String giving the repository branch from which this Emacs was built.",
    ),
    (
        "emacs-repository-version",
        "String giving the repository revision from which this Emacs was built.",
    ),
    (
        "emacs-save-session-functions",
        "Special hook run when a save-session event occurs.",
    ),
    (
        "emacs-startup-hook",
        "Normal hook run after loading init files and handling the command line.",
    ),
    (
        "enable-connection-local-variables",
        "Non-nil means enable use of connection-local variables.",
    ),
    (
        "enable-dir-local-variables",
        "Non-nil means enable use of directory-local variables.",
    ),
    (
        "enable-kinsoku",
        "Non-nil means enable \"kinsoku\" processing on filling paragraphs.",
    ),
    (
        "enable-local-eval",
        "Control processing of the \"variable\" `eval' in a file's local variables.",
    ),
    (
        "enable-local-variables",
        "Control use of local variables in files you visit.",
    ),
    (
        "enable-remote-dir-locals",
        "Non-nil means dir-local variables will be applied to remote files.",
    ),
    (
        "enable-theme-functions",
        "Abnormal hook that is run after a theme has been enabled.",
    ),
    (
        "end-of-defun-function",
        "Function for `end-of-defun' to call.",
    ),
    (
        "end-of-defun-moves-to-eol",
        "Whether `end-of-defun' moves to eol before doing anything else.",
    ),
    (
        "epa-file-encrypt-to",
        "Recipient(s) used for encrypting files.",
    ),
    (
        "epa-file-inhibit-auto-save",
        "If non-nil, disable auto-saving when opening an encrypted file.",
    ),
    (
        "epa-file-name-regexp",
        "Regexp which matches filenames to be encrypted with GnuPG.",
    ),
    (
        "epa-global-mail-mode",
        "Non-nil if Epa-Global-Mail mode is enabled.",
    ),
    ("esc-map", "Default keymap for ESC (meta) commands."),
    (
        "escaped-string-quote",
        "String to insert before a string quote character in a string to escape it.",
    ),
    (
        "etags-regen-mode",
        "Non-nil if Etags-Regen mode is enabled.",
    ),
    (
        "eval-expression-debug-on-error",
        "If non-nil set `debug-on-error' to t in `eval-expression'.",
    ),
    (
        "eval-expression-minibuffer-setup-hook",
        "Hook run by `eval-expression' when entering the minibuffer.",
    ),
    (
        "eval-expression-print-length",
        "Value for `print-length' while printing value in `eval-expression'.",
    ),
    (
        "eval-expression-print-level",
        "Value for `print-level' while printing value in `eval-expression'.",
    ),
    (
        "eval-expression-print-maximum-character",
        "The largest integer that will be displayed as a character.",
    ),
    (
        "even-window-sizes",
        "If non-nil `display-buffer' will try to even window sizes.",
    ),
    (
        "eww-suggest-uris",
        "List of functions called to form the list of default URIs for `eww'.",
    ),
    (
        "exit-language-environment-hook",
        "Normal hook run after exiting from some language environment.",
    ),
    (
        "extended-command-suggest-shorter",
        "If non-nil, show a shorter \\[execute-extended-command] invocation when there is one.",
    ),
    (
        "extended-command-versions",
        "Alist of prompts and what the extended command predicate should be.",
    ),
    (
        "face-attribute-name-alist",
        "An alist of descriptive names for face attributes.",
    ),
    (
        "face-font-family-alternatives",
        "Alist of alternative font family names.",
    ),
    (
        "face-font-registry-alternatives",
        "Alist of alternative font registry names.",
    ),
    (
        "face-font-selection-order",
        "A list specifying how face font selection chooses fonts.",
    ),
    (
        "face-name-history",
        "History list for some commands that read face names.",
    ),
    (
        "face-x-resources",
        "List of X resources and classes for face attributes.",
    ),
    (
        "fancy-about-text",
        "A list of texts to show in the middle part of the About screen.",
    ),
    (
        "fancy-splash-image",
        "The image to show in the splash screens, or nil for defaults.",
    ),
    (
        "fancy-startup-text",
        "A list of texts to show in the middle part of splash screens.",
    ),
    (
        "ff-special-constructs",
        "List of special constructs recognized by `ff-treat-as-special'.",
    ),
    (
        "ffap-file-finder",
        "The command called by `find-file-at-point' to find a file.",
    ),
    ("fido-mode", "Non-nil if Fido mode is enabled."),
    (
        "fido-vertical-mode",
        "Non-nil if Fido-Vertical mode is enabled.",
    ),
    (
        "file-auto-mode-skip",
        "Regexp of lines to skip when looking for file-local settings.",
    ),
    (
        "file-has-changed-p--hash-table",
        "Internal variable used by `file-has-changed-p'.",
    ),
    (
        "file-local-variables-alist",
        "Alist of file-local variable settings in the current buffer.",
    ),
    (
        "file-name-at-point-functions",
        "List of functions to try in sequence to get a file name at point.",
    ),
    (
        "file-name-history",
        "History list of file names entered in the minibuffer.",
    ),
    (
        "file-name-invalid-regexp",
        "Regexp recognizing file names that aren't allowed by the filesystem.",
    ),
    (
        "file-name-shadow-mode",
        "Non-nil if File-Name-Shadow mode is enabled.",
    ),
    (
        "file-name-shadow-mode-hook",
        "Hook run after entering or leaving `file-name-shadow-mode'.",
    ),
    (
        "file-name-shadow-properties",
        "Properties given to the `shadowed' part of a filename in the minibuffer.",
    ),
    (
        "file-name-shadow-tty-properties",
        "Properties given to the `shadowed' part of a filename in the minibuffer.",
    ),
    (
        "file-name-version-regexp",
        "Regular expression matching the backup/version part of a file name.",
    ),
    (
        "file-precious-flag",
        "Non-nil means protect against I/O errors while saving files.",
    ),
    (
        "file-preserve-symlinks-on-save",
        "If non-nil, saving a buffer visited via a symlink won't overwrite the symlink.",
    ),
    (
        "fill-find-break-point-function-table",
        "Char-table of special functions to find line breaking point.",
    ),
    (
        "fill-forward-paragraph-function",
        "Function to move over paragraphs used by the filling code.",
    ),
    (
        "fill-indent-according-to-mode",
        "Whether or not filling should try to use the major mode's indentation.",
    ),
    (
        "fill-individual-varying-indent",
        "Controls criterion for a new paragraph in `fill-individual-paragraphs'.",
    ),
    (
        "fill-nobreak-invisible",
        "Non-nil means that fill commands do not break lines in invisible text.",
    ),
    (
        "fill-nobreak-predicate",
        "List of predicates for recognizing places not to break a line.",
    ),
    (
        "fill-nospace-between-words-table",
        "Char-table of characters that don't use space between words.",
    ),
    (
        "fill-paragraph-function",
        "Mode-specific function to fill a paragraph, or nil if there is none.",
    ),
    (
        "fill-paragraph-handle-comment",
        "Non-nil means paragraph filling will try to pay attention to comments.",
    ),
    (
        "fill-prefix",
        "String for filling to insert at front of new line, or nil for none.",
    ),
    (
        "fill-separate-heterogeneous-words-with-space",
        "Non-nil means to use a space to separate words of a different kind.",
    ),
    (
        "filter-buffer-substring-function",
        "Function to perform the filtering in `filter-buffer-substring'.",
    ),
    (
        "filter-buffer-substring-functions",
        "This variable is a wrapper hook around `buffer-substring--filter'.",
    ),
    (
        "find-alternate-file-dont-kill-client",
        "If non-nil, `server-buffer-done' should not delete the client.",
    ),
    (
        "find-directory-functions",
        "List of functions to try in sequence to visit a directory.",
    ),
    (
        "find-file-existing-other-name",
        "Non-nil means find a file under alternative names, in existing buffers.",
    ),
    (
        "find-file-hook",
        "List of functions to be called after a buffer is loaded from a file.",
    ),
    (
        "find-file-literally",
        "Non-nil if this buffer was made by `find-file-literally' or equivalent.",
    ),
    (
        "find-file-not-found-functions",
        "List of functions to be called for `find-file' on nonexistent file.",
    ),
    (
        "find-file-run-dired",
        "Non-nil means allow `find-file' to visit directories.",
    ),
    (
        "find-file-suppress-same-file-warnings",
        "Non-nil means suppress warning messages for symlinked files.",
    ),
    (
        "find-file-visit-truename",
        "Non-nil means visiting a file uses its truename as the visited-file name.",
    ),
    (
        "find-file-wildcards",
        "Non-nil means file-visiting commands should handle wildcards.",
    ),
    ("find-program", "The default find program."),
    ("find-sibling-rules", "Rules for finding \"sibling\" files."),
    (
        "find-tag-default-function",
        "A function of no arguments used by \\[find-tag] to pick a default tag.",
    ),
    (
        "find-tag-hook",
        "Hook to be run by \\[find-tag] after finding a tag.  See `run-hooks'.",
    ),
    (
        "fit-frame-to-buffer",
        "Non-nil means `fit-window-to-buffer' can fit a frame to its buffer.",
    ),
    (
        "fit-frame-to-buffer-margins",
        "Margins around frame for `fit-frame-to-buffer'.",
    ),
    (
        "fit-frame-to-buffer-sizes",
        "Size boundaries of frame for `fit-frame-to-buffer'.",
    ),
    (
        "fit-window-to-buffer-horizontally",
        "Non-nil means `fit-window-to-buffer' can resize windows horizontally.",
    ),
    (
        "flex-score-match-tightness",
        "Controls how the `flex' completion style scores its matches.",
    ),
    ("float-e", "The value of e (2.7182818...)."),
    ("float-pi", "The value of Pi (3.1415926...)."),
    ("flyspell-mode", "Non-nil if Flyspell mode is enabled."),
    ("focus-in-hook", "Normal hook run when a frame gains focus."),
    (
        "focus-out-hook",
        "Normal hook run when all frames lost input focus.",
    ),
    ("font-lock-builtin-face", "Face name to use for builtins."),
    (
        "font-lock-comment-delimiter-face",
        "Face name to use for comment delimiters.",
    ),
    (
        "font-lock-comment-end-skip",
        "If non-nil, Font Lock mode uses this instead of `comment-end-skip'.",
    ),
    ("font-lock-comment-face", "Face name to use for comments."),
    (
        "font-lock-comment-start-skip",
        "If non-nil, Font Lock mode uses this instead of `comment-start-skip'.",
    ),
    (
        "font-lock-constant-face",
        "Face name to use for constant and label names.",
    ),
    (
        "font-lock-defaults",
        "Defaults for Font Lock mode specified by the major mode.",
    ),
    ("font-lock-doc-face", "Face name to use for documentation."),
    (
        "font-lock-doc-markup-face",
        "Face name to use for documentation mark-up.",
    ),
    (
        "font-lock-dont-widen",
        "If non-nil, font-lock will work on the non-widened buffer.",
    ),
    (
        "font-lock-ensure-function",
        "Function to make sure a region has been fontified.",
    ),
    (
        "font-lock-extend-after-change-region-function",
        "A function that determines the region to refontify after a change.",
    ),
    (
        "font-lock-extend-region-functions",
        "Special hook run just before proceeding to fontify a region.",
    ),
    (
        "font-lock-extra-managed-props",
        "Additional text properties managed by font-lock.",
    ),
    (
        "font-lock-flush-function",
        "Function to use to mark a region for refontification.",
    ),
    (
        "font-lock-fontify-buffer-function",
        "Function to use for fontifying the buffer.",
    ),
    (
        "font-lock-fontify-region-function",
        "Function to use for fontifying a region.",
    ),
    (
        "font-lock-fontify-syntactically-function",
        "Function to use for syntactically fontifying a region.",
    ),
    (
        "font-lock-function",
        "A function which is called when `font-lock-mode' is toggled.",
    ),
    (
        "font-lock-function-name-face",
        "Face name to use for function names.",
    ),
    (
        "font-lock-global-modes",
        "Modes for which Font Lock mode is automagically turned on.",
    ),
    (
        "font-lock-ignore",
        "Rules to selectively disable fontifications due to `font-lock-keywords'.",
    ),
    ("font-lock-keyword-face", "Face name to use for keywords."),
    (
        "font-lock-keywords",
        "A list of keywords and corresponding font-lock highlighting rules.",
    ),
    (
        "font-lock-keywords-alist",
        "Alist of additional `font-lock-keywords' elements for major modes.",
    ),
    (
        "font-lock-keywords-case-fold-search",
        "Non-nil means the patterns in `font-lock-keywords' are case-insensitive.",
    ),
    (
        "font-lock-keywords-only",
        "Non-nil means Font Lock should not fontify comments or strings.",
    ),
    (
        "font-lock-major-mode",
        "Major mode for which the font-lock settings have been setup.",
    ),
    (
        "font-lock-mark-block-function",
        "Non-nil means use this function to mark a block of text.",
    ),
    (
        "font-lock-maximum-decoration",
        "Maximum decoration level for fontification.",
    ),
    ("font-lock-mode", "Non-nil if Font-Lock mode is enabled."),
    (
        "font-lock-mode-hook",
        "Hook run after entering or leaving `font-lock-mode'.",
    ),
    (
        "font-lock-multiline",
        "Whether font-lock should cater to multiline keywords.",
    ),
    (
        "font-lock-negation-char-face",
        "Face name to use for easy to overlook negation.",
    ),
    (
        "font-lock-preprocessor-face",
        "Face name to use for preprocessor directives.",
    ),
    (
        "font-lock-removed-keywords-alist",
        "Alist of `font-lock-keywords' elements to be removed for major modes.",
    ),
    ("font-lock-string-face", "Face name to use for strings."),
    ("font-lock-support-mode", "Support mode for Font Lock mode."),
    (
        "font-lock-syntactic-face-function",
        "Function to determine which face to use when fontifying syntactically.",
    ),
    (
        "font-lock-syntactic-keywords",
        "A list of the syntactic keywords to put syntax properties on.",
    ),
    (
        "font-lock-syntactically-fontified",
        "Point up to which `font-lock-syntactic-keywords' has been applied.",
    ),
    (
        "font-lock-syntax-table",
        "Non-nil means use this syntax table for fontifying.",
    ),
    (
        "font-lock-type-face",
        "Face name to use for type and class names.",
    ),
    (
        "font-lock-unfontify-buffer-function",
        "Function to use for unfontifying the buffer.",
    ),
    (
        "font-lock-unfontify-region-function",
        "Function to use for unfontifying a region.",
    ),
    (
        "font-lock-variable-name-face",
        "Face name to use for variable names.",
    ),
    (
        "font-lock-verbose",
        "If non-nil, means show status messages for buffer fontification.",
    ),
    (
        "font-lock-warning-face",
        "Face name to use for things that should stand out.",
    ),
    (
        "format-alist",
        "List of information about understood file formats.",
    ),
    (
        "forward-sentence-function",
        "Function to be used to calculate sentence movements.",
    ),
    (
        "forward-sexp-function",
        "If non-nil, `forward-sexp' delegates to this function.",
    ),
    (
        "frame-auto-hide-function",
        "Function called to automatically hide frames.",
    ),
    ("frame-background-mode", "The brightness of the background."),
    (
        "frame-inherited-parameters",
        "Parameters `make-frame' copies from the selected to the new frame.",
    ),
    (
        "frame-notice-user-settings",
        "Non-nil means function `frame-notice-user-settings' wasn't run yet.",
    ),
    (
        "frameset-filter-alist",
        "Alist of frame parameters and filtering functions.",
    ),
    (
        "frameset-persistent-filter-alist",
        "Parameters to filter for persistent framesets.",
    ),
    (
        "frameset-session-filter-alist",
        "Minimum set of parameters to filter for live (on-session) framesets.",
    ),
    (
        "fringe-mode",
        "Default appearance of fringes on all frames.",
    ),
    (
        "fringe-mode-explicit",
        "Non-nil means `set-fringe-mode' should really do something.",
    ),
    (
        "fringe-styles",
        "Alist mapping fringe mode names to fringe widths.",
    ),
    (
        "from--tty-menu-p",
        "Non-nil means the current command was invoked from a TTY menu.",
    ),
    (
        "fundamental-mode-abbrev-table",
        "The abbrev table of mode-specific abbrevs for Fundamental Mode.",
    ),
    (
        "gdb-enable-debug",
        "Non-nil if Gdb-Enable-Debug mode is enabled.",
    ),
    (
        "generic-mode-list",
        "A list of mode names for `generic-mode'.",
    ),
    (
        "gensym-counter",
        "Number used to construct the name of the next symbol created by `gensym'.",
    ),
    (
        "global-abbrev-table",
        "The abbrev table whose abbrevs affect all buffers.",
    ),
    (
        "global-auto-composition-mode-hook",
        "Hook run after entering or leaving `global-auto-composition-mode'.",
    ),
    (
        "global-auto-revert-mode",
        "Non-nil if Global Auto-Revert mode is enabled.",
    ),
    (
        "global-completion-preview-mode",
        "Non-nil if Global Completion-Preview mode is enabled.",
    ),
    (
        "global-completion-preview-modes",
        "Which major modes `completion-preview-mode' is switched on in.",
    ),
    (
        "global-cwarn-mode",
        "Non-nil if Global Cwarn mode is enabled.",
    ),
    (
        "global-display-fill-column-indicator-mode",
        "Non-nil if Global Display-Fill-Column-Indicator mode is enabled.",
    ),
    (
        "global-display-fill-column-indicator-modes",
        "Which major modes `display-fill-column-indicator-mode' is switched on in.",
    ),
    (
        "global-display-line-numbers-mode",
        "Non-nil if Global Display-Line-Numbers mode is enabled.",
    ),
    ("global-ede-mode", "Non-nil if Global Ede mode is enabled."),
    (
        "global-eldoc-mode",
        "Non-nil if Global Eldoc mode is enabled.",
    ),
    (
        "global-eldoc-mode-hook",
        "Hook run after entering or leaving `global-eldoc-mode'.",
    ),
    (
        "global-font-lock-mode",
        "Non-nil if Global Font-Lock mode is enabled.",
    ),
    (
        "global-font-lock-mode-hook",
        "Hook run after entering or leaving `global-font-lock-mode'.",
    ),
    (
        "global-goto-address-mode",
        "Non-nil if Global Goto-Address mode is enabled.",
    ),
    (
        "global-hi-lock-mode",
        "Non-nil if Global Hi-Lock mode is enabled.",
    ),
    (
        "global-highlight-changes-mode",
        "Non-nil if Global Highlight-Changes mode is enabled.",
    ),
    (
        "global-hl-line-mode",
        "Non-nil if Global Hl-Line mode is enabled.",
    ),
    (
        "global-map",
        "Default global keymap mapping Emacs keyboard input into commands.",
    ),
    (
        "global-mark-ring",
        "The list of saved global marks, most recent first.",
    ),
    (
        "global-mark-ring-max",
        "Maximum size of global mark ring.  Start discarding off end if gets this big.",
    ),
    (
        "global-minor-modes",
        "A list of the currently enabled global minor modes.",
    ),
    (
        "global-prettify-symbols-mode",
        "Non-nil if Global Prettify-Symbols mode is enabled.",
    ),
    (
        "global-prettify-symbols-mode-hook",
        "Hook run after entering or leaving `global-prettify-symbols-mode'.",
    ),
    (
        "global-reveal-mode",
        "Non-nil if Global Reveal mode is enabled.",
    ),
    (
        "global-so-long-mode",
        "Non-nil if Global So-Long mode is enabled.",
    ),
    (
        "global-subword-mode",
        "Non-nil if Global Subword mode is enabled.",
    ),
    (
        "global-superword-mode",
        "Non-nil if Global Superword mode is enabled.",
    ),
    (
        "global-tab-line-mode",
        "Non-nil if Global Tab-Line mode is enabled.",
    ),
    (
        "global-visual-line-mode",
        "Non-nil if Global Visual-Line mode is enabled.",
    ),
    (
        "global-visual-line-mode-hook",
        "Hook run after entering or leaving `global-visual-line-mode'.",
    ),
    (
        "global-visual-wrap-prefix-mode",
        "Non-nil if Global Visual-Wrap-Prefix mode is enabled.",
    ),
    (
        "global-whitespace-mode",
        "Non-nil if Global Whitespace mode is enabled.",
    ),
    (
        "global-whitespace-newline-mode",
        "Non-nil if Global Whitespace-Newline mode is enabled.",
    ),
    (
        "global-window-tool-bar-mode",
        "Non-nil if Global Window-Tool-Bar mode is enabled.",
    ),
    (
        "global-word-wrap-whitespace-mode",
        "Non-nil if Global Word-Wrap-Whitespace mode is enabled.",
    ),
    (
        "glyphless-char-display-control",
        "List of directives to control display of glyphless characters.",
    ),
    (
        "goal-column",
        "Semipermanent goal column for vertical motion, as set by \\[set-goal-column], or nil.",
    ),
    (
        "goto-line-history",
        "History of values entered with `goto-line'.",
    ),
    (
        "goto-line-history-local",
        "If this option is nil, `goto-line-history' is shared between all buffers.",
    ),
    ("goto-map", "Keymap for navigation commands."),
    ("gpm-mouse-mode", "Non-nil if Gpm-Mouse mode is enabled."),
    ("grep-command", "The default grep command for \\[grep]."),
    (
        "grep-find-command",
        "The default find command for \\[grep-find].",
    ),
    ("grep-find-history", "History list for `grep-find'."),
    ("grep-find-use-xargs", "How to invoke find and grep."),
    (
        "grep-highlight-matches",
        "Use special markers to highlight grep matches.",
    ),
    ("grep-history", "History list for grep."),
    ("grep-match-face", "Face name to use for grep matches."),
    (
        "grep-program",
        "The default grep program for `grep-command' and `grep-find-command'.",
    ),
    ("grep-regexp-alist", "Regexp used to match grep hits."),
    (
        "grep-setup-hook",
        "List of hook functions run by `grep-process-setup' (see `run-hooks').",
    ),
    (
        "grep-window-height",
        "Number of lines in a grep window.  If nil, use `compilation-window-height'.",
    ),
    (
        "gud-tooltip-mode",
        "Non-nil if Gud-Tooltip mode is enabled.",
    ),
    (
        "gui--last-selected-text-clipboard",
        "The value of the CLIPBOARD selection last seen.",
    ),
    (
        "gui--last-selected-text-primary",
        "The value of the PRIMARY selection last seen.",
    ),
    (
        "gui--last-selection-timestamp-clipboard",
        "The timestamp of the CLIPBOARD selection last seen.",
    ),
    (
        "gui--last-selection-timestamp-primary",
        "The timestamp of the PRIMARY selection last seen.",
    ),
    (
        "gui-last-cut-in-clipboard",
        "Whether or not the last call to `interprogram-cut-function' owned CLIPBOARD.",
    ),
    (
        "gui-last-cut-in-primary",
        "Whether or not the last call to `interprogram-cut-function' owned PRIMARY.",
    ),
    (
        "gujarati-composable-pattern",
        "Regexp matching a composable sequence of Gujarati characters.",
    ),
    (
        "gurmukhi-composable-pattern",
        "Regexp matching a composable sequence of Gurmukhi characters.",
    ),
    (
        "hack-dir-local-get-variables-functions",
        "Special hook to compute the set of dir-local variables.",
    ),
    (
        "hack-local-variables-hook",
        "Normal hook run after processing a file's local variables specs.",
    ),
    (
        "hack-read-symbol-shorthands-function",
        "Holds function to compute `read-symbol-shorthands'.",
    ),
    (
        "hard-newline",
        "Propertized string representing a hard newline character.",
    ),
    (
        "header-line-indent",
        "String of spaces to indent the beginning of header-line due to line numbers.",
    ),
    (
        "header-line-indent-width",
        "The width of the current line number display in the window.",
    ),
    (
        "help-at-pt-display-when-idle",
        "Automatically show local help on point-over.",
    ),
    (
        "help-buffer-under-preparation",
        "Whether a *Help* buffer is being prepared.",
    ),
    (
        "help-enable-autoload",
        "Whether Help commands can perform autoloading.",
    ),
    (
        "help-for-help-buffer-name",
        "Name of the `help-for-help' buffer.",
    ),
    (
        "help-link-key-to-documentation",
        "Non-nil means link keys to their command in *Help* buffers.",
    ),
    ("help-map", "Keymap for characters following the Help key."),
    ("help-quick-sections", "Data structure for `help-quick'."),
    (
        "help-quick-use-map",
        "Keymap that `help-quick' should use to lookup bindings.",
    ),
    (
        "help-return-method",
        "What to do to \"exit\" the help buffer.",
    ),
    (
        "help-uni-confusables",
        "An alist of confusable characters to give hints about.",
    ),
    (
        "help-uni-confusables-regexp",
        "Regexp matching any character listed in `help-uni-confusables'.",
    ),
    (
        "help-window-keep-selected",
        "If non-nil, navigation commands in the *Help* buffer will reuse the window.",
    ),
    (
        "help-window-old-frame",
        "Frame selected at the time `with-help-window' is invoked.",
    ),
    (
        "help-window-point-marker",
        "Marker to override default `window-point' in help windows.",
    ),
    (
        "help-window-select",
        "Non-nil means select help window for viewing.",
    ),
    (
        "hippie-expand-try-functions-list",
        "The list of expansion functions tried in order by `hippie-expand'.",
    ),
    ("holiday-bahai-holidays", "Bahá’í holidays."),
    ("holiday-christian-holidays", "Christian holidays."),
    (
        "holiday-general-holidays",
        "General holidays.  Default value is for the United States.",
    ),
    ("holiday-hebrew-holidays", "Jewish holidays."),
    ("holiday-islamic-holidays", "Islamic holidays."),
    ("holiday-local-holidays", "Local holidays."),
    ("holiday-oriental-holidays", "Oriental holidays."),
    ("holiday-other-holidays", "User defined holidays."),
    ("holiday-solar-holidays", "Sun-related holidays."),
    (
        "horizontal-scroll-bar-mode",
        "Non-nil if Horizontal-Scroll-Bar mode is enabled.",
    ),
    (
        "horizontal-scroll-bar-mode-hook",
        "Hook run after entering or leaving `horizontal-scroll-bar-mode'.",
    ),
    (
        "hs-special-modes-alist",
        "Alist for initializing the hideshow variables for different modes.",
    ),
    ("icomplete-mode", "Non-nil if Icomplete mode is enabled."),
    (
        "icomplete-vertical-mode",
        "Non-nil if Icomplete-Vertical mode is enabled.",
    ),
    (
        "icon-map-list",
        "A list of alists that map icon file names to stock/named icons.",
    ),
    (
        "idle-update-delay",
        "Idle time delay before updating various things on the screen.",
    ),
    (
        "ido-mode",
        "Determines for which buffer/file Ido should be enabled.",
    ),
    (
        "ignore-window-parameters",
        "If non-nil, standard functions ignore window parameters.",
    ),
    (
        "ignored-local-variable-values",
        "List of variable-value pairs that should always be ignored.",
    ),
    (
        "ignored-local-variables",
        "Variables to be ignored in a file's local variable spec.",
    ),
    (
        "image-default-frame-delay",
        "Default interval in seconds between frames of a multi-frame image.",
    ),
    (
        "image-file-name-extensions",
        "A list of image-file filename extensions.",
    ),
    (
        "image-file-name-regexps",
        "List of regexps matching image-file filenames.",
    ),
    (
        "image-format-suffixes",
        "An alist associating image types with file name suffixes.",
    ),
    (
        "image-load-path",
        "List of locations in which to search for image files.",
    ),
    ("image-map", "Map put into text properties on images."),
    (
        "image-minimum-frame-delay",
        "Minimum interval in seconds between frames of an animated image.",
    ),
    (
        "image-recompute-map-p",
        "Recompute image map when scaling, rotating, or flipping an image.",
    ),
    (
        "image-slice-map",
        "Map put into text properties on sliced images.",
    ),
    (
        "image-transform-smoothing",
        "Whether to do smoothing when applying transforms to images.",
    ),
    (
        "image-type-auto-detectable",
        "Alist of (IMAGE-TYPE . AUTODETECT) pairs used to auto-detect image files.",
    ),
    (
        "image-type-file-name-regexps",
        "Alist of (REGEXP . IMAGE-TYPE) pairs used to identify image files.",
    ),
    (
        "image-type-header-regexps",
        "Alist of (REGEXP . IMAGE-TYPE) pairs used to auto-detect image types.",
    ),
    (
        "image-use-external-converter",
        "If non-nil, `create-image' will use external converters for exotic formats.",
    ),
    (
        "imagemagick--file-regexp",
        "File extension regexp for ImageMagick files, if any.",
    ),
    (
        "imagemagick-enabled-types",
        "List of ImageMagick types to treat as images.",
    ),
    (
        "imagemagick-types-inhibit",
        "List of ImageMagick types that should never be treated as images.",
    ),
    (
        "imenu-case-fold-search",
        "Defines whether `imenu--generic-function' should fold case when matching.",
    ),
    (
        "imenu-create-index-function",
        "The function to use for creating an index alist of the current buffer.",
    ),
    (
        "imenu-default-goto-function",
        "The default function called when selecting an Imenu item.",
    ),
    (
        "imenu-extract-index-name-function",
        "Function for extracting the index item name, given a position.",
    ),
    (
        "imenu-generic-expression",
        "List of definition matchers for creating an Imenu index.",
    ),
    (
        "imenu-name-lookup-function",
        "Function to compare string with index item.",
    ),
    (
        "imenu-prev-index-position-function",
        "Function for finding the next index position.",
    ),
    (
        "imenu-sort-function",
        "The function to use for sorting the index mouse-menu.",
    ),
    (
        "imenu-submenus-on-top",
        "Flag specifying whether items with sublists should be kept at top.",
    ),
    (
        "imenu-syntax-alist",
        "Alist of syntax table modifiers to use while in `imenu--generic-function'.",
    ),
    (
        "indent-line-function",
        "Function to indent the current line.",
    ),
    (
        "indent-line-ignored-functions",
        "Values that are ignored by `indent-according-to-mode'.",
    ),
    (
        "indent-region-function",
        "Short cut function to indent region using `indent-according-to-mode'.",
    ),
    (
        "indent-rigidly-map",
        "Transient keymap for adjusting indentation interactively.",
    ),
    (
        "indent-tabs-mode-hook",
        "Hook run after entering or leaving `indent-tabs-mode'.",
    ),
    (
        "inhibit-auto-fill",
        "Non-nil means to do as if `auto-fill-mode' was disabled.",
    ),
    (
        "inhibit-default-init",
        "Non-nil inhibits loading the `default' library.",
    ),
    (
        "inhibit-local-variables-ignore-case",
        "Non-nil means `inhibit-local-variables-p' ignores case.",
    ),
    (
        "inhibit-local-variables-regexps",
        "List of regexps matching file names in which to ignore local variables.",
    ),
    (
        "inhibit-local-variables-suffixes",
        "List of regexps matching suffixes to remove from file names.",
    ),
    (
        "inhibit-message-regexps",
        "List of regexps that inhibit messages by the function `inhibit-message'.",
    ),
    (
        "inhibit-startup-buffer-menu",
        "Non-nil inhibits display of buffer list when more than 2 files are loaded.",
    ),
    (
        "inhibit-startup-echo-area-message",
        "Non-nil inhibits the initial startup echo area message.",
    ),
    (
        "inhibit-startup-hooks",
        "Non-nil means don't run some startup hooks, because we already did.",
    ),
    (
        "inhibit-startup-screen",
        "Non-nil inhibits the startup screen.",
    ),
    (
        "init-file-had-error",
        "Non-nil if there was an error loading the user's init file.",
    ),
    (
        "init-file-user",
        "Identity of user whose init file is or was read.",
    ),
    (
        "initial-buffer-choice",
        "Buffer to show after starting Emacs.",
    ),
    (
        "initial-frame-alist",
        "Alist of parameters for the initial window-system (a.k.a. \"GUI\") frame.",
    ),
    (
        "initial-major-mode",
        "Major mode command symbol to use for the initial `*scratch*' buffer.",
    ),
    (
        "initial-scratch-message",
        "Initial documentation displayed in *scratch* buffer at startup.",
    ),
    (
        "input-method-activate-hook",
        "Normal hook run just after an input method is activated.",
    ),
    (
        "input-method-after-insert-chunk-hook",
        "Normal hook run just after an input method insert some chunk of text.",
    ),
    (
        "input-method-alist",
        "Alist of input method names vs how to use them.",
    ),
    (
        "input-method-deactivate-hook",
        "Normal hook run just after an input method is deactivated.",
    ),
    (
        "input-method-exit-on-first-char",
        "This flag controls when an input method returns.",
    ),
    (
        "input-method-exit-on-invalid-key",
        "This flag controls the behavior of an input method on invalid key input.",
    ),
    (
        "input-method-highlight-flag",
        "If this flag is non-nil, input methods highlight partially-entered text.",
    ),
    (
        "input-method-history",
        "History list of input methods read from the minibuffer.",
    ),
    (
        "input-method-use-echo-area",
        "This flag controls how an input method shows an intermediate key sequence.",
    ),
    (
        "input-method-verbose-flag",
        "A flag to control extra guidance given by input methods.",
    ),
    (
        "insert-default-directory",
        "Non-nil means when reading a filename start with default dir in minibuffer.",
    ),
    (
        "insert-directory-program",
        "Absolute or relative name of the `ls'-like program.",
    ),
    (
        "insert-pair-alist",
        "Alist of paired characters inserted by `insert-pair'.",
    ),
    (
        "interpreter-mode-alist",
        "Alist mapping interpreter names to major modes.",
    ),
    (
        "interprogram-cut-function",
        "Function to call to make a killed region available to other programs.",
    ),
    (
        "interprogram-paste-function",
        "Function to call to get text cut from other programs.",
    ),
    (
        "isearch-allow-motion",
        "Whether to allow movement between isearch matches by cursor motion commands.",
    ),
    (
        "isearch-allow-prefix",
        "Whether prefix arguments are allowed during incremental search.",
    ),
    (
        "isearch-allow-scroll",
        "Whether scrolling is allowed during incremental search.",
    ),
    (
        "isearch-barrier",
        "Recorded minimum/maximal point for the current search.",
    ),
    ("isearch-cmds", "Stack of search status elements."),
    (
        "isearch-filter-predicate",
        "Predicate to filter hits of Isearch and replace commands.",
    ),
    (
        "isearch-fold-quotes-mode",
        "Non-nil if Isearch-Fold-Quotes mode is enabled.",
    ),
    (
        "isearch-fold-quotes-mode-hook",
        "Hook run after entering or leaving `isearch-fold-quotes-mode'.",
    ),
    (
        "isearch-forward-thing-at-point",
        "A list of symbols to try to get the \"thing\" at point.",
    ),
    (
        "isearch-help-map",
        "Keymap for characters following the Help key for Isearch mode.",
    ),
    (
        "isearch-hide-immediately",
        "If non-nil, re-hide an invisible match right away.",
    ),
    (
        "isearch-lax-whitespace",
        "If non-nil, a space will match a sequence of whitespace chars.",
    ),
    (
        "isearch-lazy-count",
        "Show match numbers in the search prompt.",
    ),
    (
        "isearch-lazy-highlight",
        "Controls the lazy-highlighting during incremental search.",
    ),
    (
        "isearch-menu-bar-commands",
        "List of commands that can open a menu during Isearch.",
    ),
    ("isearch-menu-bar-map", "Menu for `isearch-mode'."),
    (
        "isearch-message-function",
        "Function to call to display the search prompt.",
    ),
    (
        "isearch-message-properties",
        "Text properties that are added to the isearch prompt.",
    ),
    (
        "isearch-mode-end-hook",
        "Function(s) to call after terminating an incremental search.",
    ),
    (
        "isearch-mode-end-hook-quit",
        "Non-nil while running `isearch-mode-end-hook' if the user quits the search.",
    ),
    (
        "isearch-mode-hook",
        "Function(s) to call after starting up an incremental search.",
    ),
    ("isearch-mode-map", "Keymap for `isearch-mode'."),
    (
        "isearch-motion-changes-direction",
        "Whether motion commands during incremental search change search direction.",
    ),
    (
        "isearch-mouse-commands",
        "List of mouse commands that are allowed during Isearch.",
    ),
    (
        "isearch-new-regexp-function",
        "Holds the next `isearch-regexp-function' inside `with-isearch-suspended'.",
    ),
    (
        "isearch-push-state-function",
        "Function to save a function restoring the mode-specific Isearch state",
    ),
    (
        "isearch-regexp-function",
        "Regexp-based search mode for words/symbols.",
    ),
    (
        "isearch-regexp-lax-whitespace",
        "If non-nil, a space will match a sequence of whitespace chars.",
    ),
    (
        "isearch-repeat-on-direction-change",
        "Whether a direction change should move to another match.",
    ),
    (
        "isearch-resume-in-command-history",
        "If non-nil, `isearch-resume' commands are added to the command history.",
    ),
    (
        "isearch-search-fun-function",
        "Non-default value overrides the behavior of `isearch-search-fun-default'.",
    ),
    (
        "isearch-text-conversion-style",
        "Value of `text-conversion-style' before Isearch mode",
    ),
    (
        "isearch-tool-bar-old-map",
        "Variable holding the old local value of `tool-bar-map', if any.",
    ),
    (
        "isearch-update-post-hook",
        "Function(s) to call after isearch has found matches in the buffer.",
    ),
    (
        "isearch-wrap-function",
        "Function to call to wrap the search when search is failed.",
    ),
    (
        "isearch-wrap-pause",
        "Define the behavior of wrapping when there are no more matches.",
    ),
    (
        "isearch-yank-on-move",
        "Motion keys yank text to the search string while you move the cursor.",
    ),
    (
        "iso-transl-char-map",
        "Alist of character translations for entering ISO characters.",
    ),
    ("iso-transl-ctl-x-8-map", "Keymap for C-x 8 prefix."),
    (
        "iso-transl-dead-key-alist",
        "Mapping of ASCII characters to their corresponding dead-key symbols.",
    ),
    (
        "ispell-html-skip-alists",
        "Lists of start and end keys to skip in HTML buffers.",
    ),
    ("ispell-menu-map", "Key map for ispell menu."),
    (
        "ispell-personal-dictionary",
        "File name of your personal spelling dictionary, or nil.",
    ),
    (
        "ispell-skip-region-alist",
        "Alist expressing beginning and end of regions not to spell check.",
    ),
    (
        "ispell-tex-skip-alists",
        "Lists of regions to be skipped in TeX mode.",
    ),
    (
        "jit-lock--antiblink-grace-timer",
        "Idle timer for fontifying unterminated string or comment, or nil.",
    ),
    (
        "jit-lock--antiblink-line-beginning-position",
        "Last line beginning position after last command (a marker).",
    ),
    (
        "jit-lock--antiblink-string-or-comment",
        "Non-nil if in string or comment after last command (a boolean).",
    ),
    (
        "jit-lock-after-change-extend-region-functions",
        "Hook that can extend the text to refontify after a change.",
    ),
    (
        "jit-lock-antiblink-grace",
        "Delay after which to refontify unterminated strings and comments.",
    ),
    (
        "jit-lock-chunk-size",
        "Jit-lock asks to fontify chunks of at most this many characters at a time.",
    ),
    (
        "jit-lock-context-time",
        "Idle time after which text is contextually refontified, if applicable.",
    ),
    (
        "jit-lock-context-timer",
        "Timer for context fontification in Just-in-time Lock mode.",
    ),
    (
        "jit-lock-context-unfontify-pos",
        "Consider text after this position as contextually unfontified.",
    ),
    (
        "jit-lock-contextually",
        "If non-nil, fontification should be syntactically true.",
    ),
    (
        "jit-lock-debug-mode",
        "Non-nil if Jit-Lock-Debug mode is enabled.",
    ),
    (
        "jit-lock-debug-mode-hook",
        "Hook run after entering or leaving `jit-lock-debug-mode'.",
    ),
    (
        "jit-lock-defer-buffers",
        "List of buffers with pending deferred fontification.",
    ),
    (
        "jit-lock-defer-time",
        "Idle time after which deferred fontification should take place.",
    ),
    (
        "jit-lock-defer-timer",
        "Timer for deferred fontification in Just-in-time Lock mode.",
    ),
    (
        "jit-lock-functions",
        "Special hook run to do the actual fontification.",
    ),
    (
        "jit-lock-mode",
        "Non-nil means Just-in-time Lock mode is active.",
    ),
    (
        "jit-lock-stealth-buffers",
        "List of buffers that are being fontified stealthily.",
    ),
    (
        "jit-lock-stealth-load",
        "Load in percentage above which stealth fontification is suspended.",
    ),
    (
        "jit-lock-stealth-nice",
        "Time in seconds to pause between chunks of stealth fontification.",
    ),
    (
        "jit-lock-stealth-repeat-timer",
        "Timer for repeated stealth fontification in Just-in-time Lock mode.",
    ),
    (
        "jit-lock-stealth-time",
        "Time in seconds to wait before beginning stealth fontification.",
    ),
    (
        "jit-lock-stealth-timer",
        "Timer for stealth fontification in Just-in-time Lock mode.",
    ),
    (
        "jit-lock-stealth-verbose",
        "If non-nil, means stealth fontification should show status messages.",
    ),
    (
        "jka-compr-compression-info-list",
        "List of vectors that describe available compression techniques.",
    ),
    (
        "jka-compr-compression-info-list--internal",
        "Stored value of `jka-compr-compression-info-list'.",
    ),
    (
        "jka-compr-file-name-handler-entry",
        "`file-name-handler-alist' entry used by jka-compr I/O functions.",
    ),
    (
        "jka-compr-inhibit",
        "Non-nil means inhibit automatic uncompression temporarily.",
    ),
    (
        "jka-compr-load-suffixes",
        "List of compression related suffixes to try when loading files.",
    ),
    (
        "jka-compr-load-suffixes--internal",
        "Stored value of `jka-compr-load-suffixes'.",
    ),
    (
        "jka-compr-mode-alist-additions",
        "List of pairs added to `auto-mode-alist' when installing jka-compr.",
    ),
    (
        "jka-compr-mode-alist-additions--internal",
        "Stored value of `jka-compr-mode-alist-additions'.",
    ),
    (
        "jka-compr-verbose",
        "If non-nil, output messages whenever compressing or uncompressing files.",
    ),
    (
        "kannada-composable-pattern",
        "Regexp matching a composable sequence of Kannada characters.",
    ),
    (
        "kept-new-versions",
        "Number of newest versions to keep when a new numbered backup is made.",
    ),
    (
        "kept-old-versions",
        "Number of oldest versions to keep when a new numbered backup is made.",
    ),
    (
        "key-substitution-in-progress",
        "Used internally by `substitute-key-definition'.",
    ),
    (
        "keyboard-coding-system",
        "Specify coding system for keyboard input.",
    ),
    ("keyboard-type", "The brand of keyboard you are using."),
    (
        "keypad-numlock-setup",
        "Specifies the keypad setup for unshifted keypad keys when NumLock is on.",
    ),
    (
        "keypad-numlock-shifted-setup",
        "Specifies the keypad setup for shifted keypad keys when NumLock is off.",
    ),
    (
        "keypad-setup",
        "Specifies the keypad setup for unshifted keypad keys when NumLock is off.",
    ),
    (
        "keypad-shifted-setup",
        "Specifies the keypad setup for shifted keypad keys when NumLock is off.",
    ),
    (
        "kill-append-merge-undo",
        "Amalgamate appending kills with the last kill for undo.",
    ),
    ("kill-buffer-hook", "Hook run when a buffer is killed."),
    (
        "kill-do-not-save-duplicates",
        "If non-nil, don't add a string to `kill-ring' if it duplicates the last one.",
    ),
    (
        "kill-emacs-query-functions",
        "Functions to call with no arguments to query about killing Emacs.",
    ),
    (
        "kill-read-only-ok",
        "Non-nil means don't signal an error for killing read-only text.",
    ),
    (
        "kill-ring-deindent-mode",
        "Non-nil if Kill-Ring-Deindent mode is enabled.",
    ),
    (
        "kill-ring-max",
        "Maximum length of kill ring before oldest elements are thrown away.",
    ),
    (
        "kill-transform-function",
        "Function to call to transform a string before it's put on the kill ring.",
    ),
    (
        "kill-whole-line",
        "If non-nil, `kill-line' with no arg at start of line kills the whole line.",
    ),
    (
        "kkc-after-update-conversion-functions",
        "Functions to run after a conversion is selected in `japanese' input method.",
    ),
    (
        "language-info-alist",
        "Alist of language environment definitions.",
    ),
    (
        "language-info-custom-alist",
        "Customizations of language environment parameters.",
    ),
    (
        "large-file-warning-threshold",
        "Maximum size of file above which a confirmation is requested.",
    ),
    (
        "last-abbrev",
        "The abbrev-symbol of the last abbrev expanded.  See `abbrev-symbol'.",
    ),
    (
        "last-abbrev-location",
        "The location of the start of the last abbrev that was expanded.",
    ),
    (
        "last-abbrev-text",
        "The exact text of the last abbrev that was expanded.",
    ),
    (
        "last-coding-system-specified",
        "Most recent coding system explicitly specified by the user when asked.",
    ),
    ("latex-block-names", "User defined LaTeX block names."),
    (
        "latex-inputenc-coding-alist",
        "Mapping from LaTeX encodings in \"inputenc.sty\" to Emacs coding systems.",
    ),
    ("latex-run-command", "Command used to run LaTeX subjob."),
    (
        "latin1-display",
        "Set up Latin-1/ASCII display for ISO8859 character sets.",
    ),
    (
        "latin1-display-ucs-per-lynx",
        "Set up Latin-1/ASCII display for Unicode characters.",
    ),
    (
        "lazy-count-invisible-format",
        "Format of the number of invisible matches for the prompt.",
    ),
    (
        "lazy-count-prefix-format",
        "Format of the current/total number of matches for the prompt prefix.",
    ),
    (
        "lazy-count-suffix-format",
        "Format of the current/total number of matches for the prompt suffix.",
    ),
    (
        "lazy-count-update-hook",
        "Hook run after new lazy count results are computed.",
    ),
    (
        "lazy-highlight-buffer",
        "Controls the lazy-highlighting of the full buffer.",
    ),
    (
        "lazy-highlight-buffer-max-at-a-time",
        "Maximum matches to highlight at a time (for `lazy-highlight-buffer').",
    ),
    (
        "lazy-highlight-cleanup",
        "Controls whether to remove extra highlighting after a search.",
    ),
    (
        "lazy-highlight-initial-delay",
        "Seconds to wait before beginning to lazily highlight all matches.",
    ),
    (
        "lazy-highlight-interval",
        "Seconds between lazily highlighting successive matches.",
    ),
    (
        "lazy-highlight-max-at-a-time",
        "Maximum matches to highlight at a time (for `lazy-highlight').",
    ),
    (
        "lazy-highlight-no-delay-length",
        "For search strings at least this long, lazy highlight starts immediately.",
    ),
    (
        "leim-list-entry-regexp",
        "Regexp matching head of each entry in LEIM list file.",
    ),
    ("leim-list-file-name", "Name of LEIM list file."),
    (
        "leim-list-header",
        "Header to be inserted in LEIM list file.",
    ),
    (
        "line-move-ignore-invisible",
        "Non-nil means commands that move by lines ignore invisible newlines.",
    ),
    (
        "line-move-visual",
        "When non-nil, `line-move' moves point by visual lines.",
    ),
    (
        "line-number-mode",
        "Non-nil if Line-Number mode is enabled.",
    ),
    (
        "line-number-mode-hook",
        "Hook run after entering or leaving `line-number-mode'.",
    ),
    (
        "lisp-body-indent",
        "Number of columns to indent the second line of a `(def...)' form.",
    ),
    (
        "lisp-cl-font-lock-keywords",
        "Default expressions to highlight in Lisp modes.",
    ),
    (
        "lisp-cl-font-lock-keywords-1",
        "Subdued level highlighting for Lisp modes.",
    ),
    (
        "lisp-cl-font-lock-keywords-2",
        "Gaudy level highlighting for Lisp modes.",
    ),
    (
        "lisp-data-mode-abbrev-table",
        "Abbrev table for `lisp-data-mode'.",
    ),
    (
        "lisp-data-mode-hook",
        "Hook run after entering `lisp-data-mode'.",
    ),
    ("lisp-data-mode-map", "Keymap for `lisp-data-mode'."),
    (
        "lisp-data-mode-syntax-table",
        "Parent syntax table used in Lisp modes.",
    ),
    (
        "lisp-directory",
        "Directory where Emacs's own *.el and *.elc Lisp files are installed.",
    ),
    (
        "lisp-doc-string-elt-property",
        "The symbol property that holds the docstring position info.",
    ),
    (
        "lisp-el-font-lock-keywords",
        "Default expressions to highlight in Emacs Lisp mode.",
    ),
    (
        "lisp-el-font-lock-keywords-1",
        "Subdued level highlighting for Emacs Lisp mode.",
    ),
    (
        "lisp-el-font-lock-keywords-2",
        "Gaudy level highlighting for Emacs Lisp mode.",
    ),
    (
        "lisp-el-font-lock-keywords-for-backtraces",
        "Default highlighting from Emacs Lisp mode used in Backtrace mode.",
    ),
    (
        "lisp-el-font-lock-keywords-for-backtraces-1",
        "Subdued highlighting from Emacs Lisp mode used in Backtrace mode.",
    ),
    (
        "lisp-el-font-lock-keywords-for-backtraces-2",
        "Gaudy highlighting from Emacs Lisp mode used in Backtrace mode.",
    ),
    (
        "lisp-imenu-generic-expression",
        "Imenu generic expression for Lisp mode.  See `imenu-generic-expression'.",
    ),
    (
        "lisp-indent-function",
        "A function to be called by `calculate-lisp-indent'.",
    ),
    (
        "lisp-indent-offset",
        "If non-nil, indent second line of expressions that many more columns.",
    ),
    (
        "lisp-interaction-mode-hook",
        "Hook run when entering Lisp Interaction mode.",
    ),
    (
        "lisp-interaction-mode-map",
        "Keymap for Lisp Interaction mode.",
    ),
    (
        "lisp-interaction-mode-menu",
        "Menu for Lisp Interaction mode.",
    ),
    (
        "lisp-interaction-mode-syntax-table",
        "Syntax table for `lisp-interaction-mode'.",
    ),
    ("lisp-mode-abbrev-table", "Abbrev table for Lisp mode."),
    (
        "lisp-mode-autoload-regexp",
        "Regexp to match autoload cookies.",
    ),
    ("lisp-mode-hook", "Hook run when entering Lisp mode."),
    ("lisp-mode-map", "Keymap for ordinary Lisp mode."),
    ("lisp-mode-menu", "Menu for ordinary Lisp mode."),
    (
        "lisp-mode-shared-map",
        "Keymap for commands shared by all sorts of Lisp modes.",
    ),
    (
        "lisp-mode-syntax-table",
        "Syntax table used in `lisp-mode'.",
    ),
    (
        "lisp-prettify-symbols-alist",
        "Alist of symbol/\"pretty\" characters to be displayed.",
    ),
    (
        "list-buffers-directory",
        "String to display in buffer listings for buffers not visiting a file.",
    ),
    (
        "list-directory-brief-switches",
        "Switches for `list-directory' to pass to `ls' for brief listing.",
    ),
    (
        "list-directory-verbose-switches",
        "Switches for `list-directory' to pass to `ls' for verbose listing.",
    ),
    (
        "list-faces-sample-text",
        "Text string to display as the sample text for `list-faces-display'.",
    ),
    (
        "list-matching-lines-buffer-name-face",
        "Face used by \\[list-matching-lines] to show the names of buffers.",
    ),
    (
        "list-matching-lines-current-line-face",
        "Face used by \\[list-matching-lines] to highlight the current line.",
    ),
    (
        "list-matching-lines-default-context-lines",
        "Default number of context lines included around `list-matching-lines' matches.",
    ),
    (
        "list-matching-lines-face",
        "Face used by \\[list-matching-lines] to show the text that matches.",
    ),
    (
        "list-matching-lines-jump-to-current-line",
        "If non-nil, \\[list-matching-lines] shows the current line highlighted.",
    ),
    (
        "list-matching-lines-prefix-face",
        "Face used by \\[list-matching-lines] to show the prefix column.",
    ),
    (
        "load--bin-dest-dir",
        "Store the original value passed by \"--bin-dest\" during dump.",
    ),
    (
        "load--eln-dest-dir",
        "Store the original value passed by \"--eln-dest\" during dump.",
    ),
    (
        "local-enable-local-variables",
        "Like `enable-local-variables', except for major mode in a -*- line.",
    ),
    (
        "locale-charset-alist",
        "Coding system alist keyed on locale-style charset name.",
    ),
    (
        "locale-charset-language-names",
        "List of pairs of locale regexps and charset language names.",
    ),
    (
        "locale-language-names",
        "Alist of locale regexps vs the corresponding languages and coding systems.",
    ),
    (
        "locale-preferred-coding-systems",
        "List of pairs of locale regexps and preferred coding systems.",
    ),
    (
        "locale-translation-file-name",
        "File name for the system's file of locale-name aliases, or nil if none.",
    ),
    (
        "locate-dominating-stop-dir-regexp",
        "Regexp of directory names that stop the search in `locate-dominating-file'.",
    ),
    (
        "locate-ls-subdir-switches",
        "`ls' switches for inserting subdirectories in `*Locate*' buffers.",
    ),
    ("lock-file-mode", "Non-nil if Lock-File mode is enabled."),
    (
        "lock-file-mode-hook",
        "Hook run after entering or leaving `lock-file-mode'.",
    ),
    (
        "lock-file-name-transforms",
        "Transforms to apply to buffer file name before making a lock file name.",
    ),
    (
        "lost-selection-last-region-buffer",
        "The last buffer from which the region was selected.",
    ),
    (
        "lost-selection-mode",
        "Non-nil if Lost-Selection mode is enabled.",
    ),
    (
        "lost-selection-mode-hook",
        "Hook run after entering or leaving `lost-selection-mode'.",
    ),
    ("lpr-command", "Name of program for printing a file."),
    (
        "lpr-lp-system",
        "Non-nil if running on a system type that uses the \"lp\" command.",
    ),
    (
        "lpr-switches",
        "List of strings to pass as extra options for the printer program.",
    ),
    (
        "lpr-windows-system",
        "Non-nil if running on MS-DOS or MS Windows.",
    ),
    (
        "ls-lisp-support-shell-wildcards",
        "Non-nil means ls-lisp treats file patterns as shell wildcards.",
    ),
    (
        "macro-declarations-alist",
        "List associating properties of macros to their macro expansion.",
    ),
    (
        "macroexp--pending-eager-loads",
        "Stack of files currently undergoing eager macro-expansion.",
    ),
    (
        "macroexp-inhibit-compiler-macros",
        "Inhibit application of compiler macros if non-nil.",
    ),
    (
        "magic-fallback-mode-alist",
        "Like `magic-mode-alist' but has lower priority than `auto-mode-alist'.",
    ),
    (
        "magic-mode-alist",
        "Alist of buffer beginnings vs. corresponding major mode functions.",
    ),
    (
        "magic-mode-regexp-match-limit",
        "Upper limit on `magic-mode-alist' regexp matches.",
    ),
    (
        "mail-abbrevs-mode",
        "Non-nil if Mail-Abbrevs mode is enabled.",
    ),
    ("mail-aliases", "Alist of mail address aliases,"),
    (
        "mail-archive-file-name",
        "Name of file to write all outgoing messages in, or nil for none.",
    ),
    (
        "mail-citation-hook",
        "Hook for modifying a citation just inserted in the mail buffer.",
    ),
    (
        "mail-citation-prefix-regexp",
        "Regular expression to match a citation prefix plus whitespace.",
    ),
    (
        "mail-complete-style",
        "Specifies how \\[mail-complete] formats the full name when it completes.",
    ),
    (
        "mail-default-directory",
        "Value of `default-directory' for Mail mode buffers.",
    ),
    (
        "mail-default-headers",
        "A string containing header lines, to be inserted in outgoing messages.",
    ),
    (
        "mail-default-reply-to",
        "Address to insert as default Reply-To field of outgoing messages.",
    ),
    (
        "mail-dont-reply-to-names",
        "Regexp specifying addresses to prune from a reply message.",
    ),
    (
        "mail-encode-mml",
        "If non-nil, mail-user-agent's `sendfunc' command should mml-encode",
    ),
    ("mail-from-style", "Specifies how \"From:\" fields look."),
    (
        "mail-header-separator",
        "Line used to separate headers from text in messages being composed.",
    ),
    (
        "mail-hist-keep-history",
        "Non-nil means keep a history for headers and text of outgoing mail.",
    ),
    (
        "mail-host-address",
        "The name of this machine, for use in constructing email addresses.",
    ),
    (
        "mail-indentation-spaces",
        "Number of spaces to insert at the beginning of each cited line.",
    ),
    (
        "mail-interactive",
        "Non-nil means when sending a message wait for and display errors.",
    ),
    (
        "mail-mailing-lists",
        "List of mailing list addresses the user is subscribed to.",
    ),
    (
        "mail-personal-alias-file",
        "If non-nil, the name of the user's personal mail alias file.",
    ),
    (
        "mail-self-blind",
        "Non-nil means insert Bcc to self in messages to be sent.",
    ),
    (
        "mail-setup-hook",
        "Normal hook, run each time a new outgoing message is initialized.",
    ),
    (
        "mail-signature",
        "Text inserted at end of mail buffer when a message is initialized.",
    ),
    (
        "mail-signature-file",
        "File containing the text inserted at end of mail buffer.",
    ),
    (
        "mail-specify-envelope-from",
        "If non-nil, specify the envelope-from address when sending mail.",
    ),
    (
        "mail-use-rfc822",
        "If non-nil, use a full, hairy RFC 822 (or later) parser on mail addresses.",
    ),
    (
        "mail-user-agent",
        "Your preference for a mail composition package.",
    ),
    (
        "mail-yank-prefix",
        "Prefix insert on lines of yanked message being replied to.",
    ),
    (
        "major-mode-remap-alist",
        "Alist mapping file-specified modes to alternative modes.",
    ),
    (
        "major-mode-remap-defaults",
        "Alist mapping file-specified modes to alternative modes.",
    ),
    (
        "make-backup-file-name-function",
        "A function that `make-backup-file-name' uses to create backup file names.",
    ),
    (
        "make-backup-files",
        "Non-nil means make a backup of a file the first time it is saved.",
    ),
    (
        "malayalam-composable-pattern",
        "Regexp matching a composable sequence of Malayalam characters.",
    ),
    (
        "mark-ring",
        "The list of former marks of the current buffer, most recent first.",
    ),
    (
        "mark-ring-max",
        "Maximum size of mark ring.  Start discarding off end if gets this big.",
    ),
    (
        "max-specpdl-size",
        "Former limit on specbindings, now without effect.",
    ),
    (
        "menu-bar-buffers-menu-command-entries",
        "Entries to be included at the end of the \"Buffers\" menu.",
    ),
    (
        "menu-bar-close-window",
        "Whether or not to close the current window from the menu bar.",
    ),
    (
        "menu-bar-last-search-type",
        "Type of last non-incremental search command called from the menu.",
    ),
    (
        "menu-bar-mode-hook",
        "Hook run after entering or leaving `menu-bar-mode'.",
    ),
    (
        "menu-bar-select-buffer-function",
        "Function to select the buffer chosen from the `Buffers' menu-bar menu.",
    ),
    ("menu-bar-separator", "Separator for menus."),
    (
        "messages-buffer-mode-abbrev-table",
        "Abbrev table for `messages-buffer-mode'.",
    ),
    (
        "messages-buffer-mode-hook",
        "Hook run after entering `messages-buffer-mode'.",
    ),
    (
        "messages-buffer-mode-map",
        "Keymap for `messages-buffer-mode'.",
    ),
    (
        "messages-buffer-mode-syntax-table",
        "Syntax table for `messages-buffer-mode'.",
    ),
    ("midnight-mode", "Non-nil if Midnight mode is enabled."),
    (
        "minibuffer--original-buffer",
        "Buffer that was current when `completing-read' was called.",
    ),
    (
        "minibuffer--regexp-primed",
        "Non-nil when minibuffer contents change.",
    ),
    (
        "minibuffer--regexp-prompt-regexp",
        "Regular expression compiled from `minibuffer-regexp-prompts'.",
    ),
    (
        "minibuffer--require-match",
        "Value of REQUIRE-MATCH passed to `completing-read'.",
    ),
    (
        "minibuffer-beginning-of-buffer-movement",
        "Control how the \\<minibuffer-local-map>\\[minibuffer-beginning-of-buffer] command in the minibuffer behaves.",
    ),
    (
        "minibuffer-completion-auto-choose",
        "Non-nil means to automatically insert completions to the minibuffer.",
    ),
    (
        "minibuffer-completion-base",
        "The base for the current completion.",
    ),
    (
        "minibuffer-confirm-exit-commands",
        "List of commands which cause an immediately following",
    ),
    (
        "minibuffer-default",
        "The current default value or list of default values in the minibuffer.",
    ),
    (
        "minibuffer-default-add-done",
        "When nil, add more elements to the end of the list of default values.",
    ),
    (
        "minibuffer-default-add-function",
        "Function run by `goto-history-element' before consuming default values.",
    ),
    (
        "minibuffer-default-prompt-format",
        "Format string used to output \"default\" values.",
    ),
    (
        "minibuffer-depth-indicate-mode",
        "Non-nil if Minibuffer-Depth-Indicate mode is enabled.",
    ),
    (
        "minibuffer-electric-default-mode",
        "Non-nil if Minibuffer-Electric-Default mode is enabled.",
    ),
    (
        "minibuffer-frame-alist",
        "Alist of parameters for the initial minibuffer frame.",
    ),
    ("minibuffer-history", "Default minibuffer history list."),
    (
        "minibuffer-history-case-insensitive-variables",
        "Minibuffer history variables for which matching should ignore case.",
    ),
    (
        "minibuffer-history-sexp-flag",
        "Control whether history list elements are expressions or strings.",
    ),
    (
        "minibuffer-inactive-mode-abbrev-table",
        "Abbrev table for `minibuffer-inactive-mode'.",
    ),
    (
        "minibuffer-inactive-mode-hook",
        "Hook run after entering `minibuffer-inactive-mode'.",
    ),
    (
        "minibuffer-inactive-mode-map",
        "Keymap for use in the minibuffer when it is not active.",
    ),
    (
        "minibuffer-inactive-mode-syntax-table",
        "Syntax table for `minibuffer-inactive-mode'.",
    ),
    (
        "minibuffer-lazy-count-format",
        "Format of the total number of matches for the prompt prefix.",
    ),
    (
        "minibuffer-local-completion-map",
        "Local keymap for minibuffer input with completion.",
    ),
    (
        "minibuffer-local-filename-completion-map",
        "Local keymap for minibuffer input with completion for filenames.",
    ),
    (
        "minibuffer-local-filename-syntax",
        "Syntax table used when reading a file name in the minibuffer.",
    ),
    (
        "minibuffer-local-isearch-map",
        "Keymap for editing Isearch strings in the minibuffer.",
    ),
    (
        "minibuffer-local-must-match-map",
        "Local keymap for minibuffer input with completion, for exact match.",
    ),
    (
        "minibuffer-local-ns-map",
        "Local keymap for the minibuffer when spaces are not allowed.",
    ),
    (
        "minibuffer-local-shell-command-map",
        "Keymap used for completing shell commands in minibuffer.",
    ),
    (
        "minibuffer-message-clear-timeout",
        "How long to display an echo-area message when the minibuffer is active.",
    ),
    (
        "minibuffer-message-properties",
        "Text properties added to the text shown by `minibuffer-message'.",
    ),
    (
        "minibuffer-mode-abbrev-table",
        "Abbrev table for `minibuffer-mode'.",
    ),
    (
        "minibuffer-mode-hook",
        "Hook run after entering `minibuffer-mode'.",
    ),
    ("minibuffer-mode-map", "Keymap for `minibuffer-mode'."),
    (
        "minibuffer-on-screen-keyboard-displayed",
        "Whether or not the on-screen keyboard has been displayed.",
    ),
    (
        "minibuffer-on-screen-keyboard-timer",
        "Timer run upon exiting the minibuffer.",
    ),
    (
        "minibuffer-regexp-mode",
        "Non-nil if Minibuffer-Regexp mode is enabled.",
    ),
    (
        "minibuffer-regexp-mode-hook",
        "Hook run after entering or leaving `minibuffer-regexp-mode'.",
    ),
    (
        "minibuffer-regexp-prompts",
        "List of regular expressions that trigger `minibuffer-regexp-mode' features.",
    ),
    (
        "minibuffer-text-before-history",
        "Text that was in this minibuffer before any history commands.",
    ),
    (
        "minibuffer-visible-completions",
        "Whether candidates shown in *Completions* can be navigated from minibuffer.",
    ),
    (
        "minibuffer-visible-completions--always-bind",
        "If non-nil, force the `minibuffer-visible-completions' bindings on.",
    ),
    (
        "minibuffer-visible-completions-map",
        "Local keymap for minibuffer input with visible completions.",
    ),
    (
        "minor-mode-alist",
        "Alist saying how to show minor modes in the mode line.",
    ),
    ("minor-mode-list", "List of all minor mode functions."),
    (
        "mode-line-buffer-identification",
        "Mode line construct for identifying the buffer being displayed.",
    ),
    (
        "mode-line-buffer-identification-keymap",
        "Keymap for what is displayed by `mode-line-buffer-identification'.",
    ),
    (
        "mode-line-client",
        "Mode line construct for identifying emacsclient frames.",
    ),
    (
        "mode-line-coding-system-map",
        "Local keymap for the coding-system part of the mode line.",
    ),
    (
        "mode-line-column-line-number-mode-map",
        "Keymap to display on column and line numbers.",
    ),
    (
        "mode-line-default-help-echo",
        "Default help text for the mode line.",
    ),
    (
        "mode-line-defining-kbd-macro",
        "String displayed in the mode line in keyboard macro recording mode.",
    ),
    (
        "mode-line-end-spaces",
        "Mode line construct to put at the end of the mode line.",
    ),
    (
        "mode-line-format-right-align",
        "Mode line construct to right align all following constructs.",
    ),
    (
        "mode-line-frame-identification",
        "Mode line construct to describe the current frame.",
    ),
    (
        "mode-line-front-space",
        "Mode line construct to put at the front of the mode line.",
    ),
    (
        "mode-line-major-mode-keymap",
        "Keymap to display on major mode.",
    ),
    (
        "mode-line-minor-mode-keymap",
        "Keymap to display on minor modes.",
    ),
    (
        "mode-line-misc-info",
        "Mode line construct for miscellaneous information.",
    ),
    (
        "mode-line-mode-menu",
        "Menu of mode operations in the mode line.",
    ),
    (
        "mode-line-modes",
        "Mode line construct for displaying major and minor modes.",
    ),
    (
        "mode-line-modified",
        "Mode line construct for displaying whether current buffer is modified.",
    ),
    (
        "mode-line-mule-info",
        "Mode line construct to report the multilingual environment.",
    ),
    (
        "mode-line-percent-position",
        "Specification of \"percentage offset\" of window through buffer.",
    ),
    (
        "mode-line-position",
        "Mode line construct for displaying the position in the buffer.",
    ),
    (
        "mode-line-position-column-format",
        "Format used to display column numbers in the mode line.",
    ),
    (
        "mode-line-position-column-line-format",
        "Format used to display combined line/column numbers in the mode line.",
    ),
    (
        "mode-line-position-line-format",
        "Format used to display line numbers in the mode line.",
    ),
    (
        "mode-line-process",
        "Mode line construct for displaying info on process status.",
    ),
    (
        "mode-line-remote",
        "Mode line construct to indicate a remote buffer.",
    ),
    (
        "mode-line-right-align-edge",
        "Where function `mode-line-format-right-align' should align to.",
    ),
    (
        "mode-line-window-dedicated",
        "Mode line construct to describe the current window.",
    ),
    (
        "mode-line-window-dedicated-keymap",
        "Keymap for what is displayed by `mode-line-window-dedicated'.",
    ),
    (
        "mode-require-final-newline",
        "Whether to add a newline at end of file, in certain major modes.",
    ),
    (
        "mode-specific-map",
        "Keymap for characters following \\`C-c'.",
    ),
    (
        "modifier-bar-mode",
        "Non-nil if Modifier-Bar mode is enabled.",
    ),
    (
        "modifier-bar-mode-hook",
        "Hook run after entering or leaving `modifier-bar-mode'.",
    ),
    (
        "modifier-bar-modifier-list",
        "List of modifiers that are currently applied.",
    ),
    (
        "mounted-file-systems",
        "File systems that ought to be mounted.",
    ),
    (
        "mouse--rectangle-track-cursor",
        "Whether the mouse tracks the cursor when selecting a rectangle.",
    ),
    (
        "mouse-1-click-follows-link",
        "Non-nil means that clicking Mouse-1 on a link follows the link.",
    ),
    (
        "mouse-1-click-in-non-selected-windows",
        "If non-nil, a Mouse-1 click also follows links in non-selected windows.",
    ),
    (
        "mouse-1-double-click-prefer-symbols",
        "If non-nil, double-clicking Mouse-1 attempts to select the symbol at click.",
    ),
    (
        "mouse-autoselect-window-position",
        "Last mouse position recorded by delayed window autoselection.",
    ),
    (
        "mouse-autoselect-window-position-1",
        "First mouse position recorded by delayed window autoselection.",
    ),
    (
        "mouse-autoselect-window-state",
        "When non-nil, special state of delayed window autoselection.",
    ),
    (
        "mouse-autoselect-window-timer",
        "Timer used by delayed window autoselection.",
    ),
    (
        "mouse-autoselect-window-window",
        "Last window recorded by delayed window autoselection.",
    ),
    ("mouse-avoidance-mode", "Activate Mouse Avoidance mode."),
    (
        "mouse-buffer-menu-maxlen",
        "Number of buffers in one pane (submenu) of the buffer menu.",
    ),
    (
        "mouse-buffer-menu-mode-groups",
        "How to group various major modes together in \\[mouse-buffer-menu].",
    ),
    (
        "mouse-buffer-menu-mode-mult",
        "Group the buffers by the major mode groups on \\[mouse-buffer-menu]?",
    ),
    (
        "mouse-drag-and-drop-region",
        "If non-nil, dragging the mouse drags the region, if it exists.",
    ),
    (
        "mouse-drag-and-drop-region-cross-program",
        "If non-nil, allow dragging text to other programs.",
    ),
    (
        "mouse-drag-and-drop-region-cut-when-buffers-differ",
        "If non-nil, cut text also when source and destination buffers differ.",
    ),
    (
        "mouse-drag-and-drop-region-scroll-margin",
        "If non-nil, the scroll margin inside a window when dragging text.",
    ),
    (
        "mouse-drag-and-drop-region-show-cursor",
        "If non-nil, move point with mouse cursor during dragging.",
    ),
    (
        "mouse-drag-and-drop-region-show-tooltip",
        "If non-nil, text is shown by a tooltip in a graphic display.",
    ),
    (
        "mouse-drag-copy-region",
        "If non-nil, copy to kill ring upon mouse adjustments of the region.",
    ),
    (
        "mouse-drag-mode-line-buffer",
        "If non-nil, allow dragging files from the mode line.",
    ),
    (
        "mouse-scroll-delay",
        "The pause between scroll steps caused by mouse drags, in seconds.",
    ),
    (
        "mouse-scroll-min-lines",
        "The minimum number of lines scrolled by dragging mouse out of window.",
    ),
    (
        "mouse-secondary-overlay",
        "An overlay which records the current secondary selection.",
    ),
    (
        "mouse-select-region-move-to-beginning",
        "Effect of selecting a region extending backward from double click.",
    ),
    (
        "mouse-wheel--installed-bindings-alist",
        "Alist of all installed mouse wheel key bindings.",
    ),
    (
        "mouse-wheel-buttons",
        "How to remap mouse button numbers to wheel events.",
    ),
    (
        "mouse-wheel-click-event",
        "Event that should be temporarily inhibited after mouse scrolling.",
    ),
    (
        "mouse-wheel-down-event",
        "Event used for scrolling down, beside `wheel-up', if any.",
    ),
    (
        "mouse-wheel-flip-direction",
        "Swap direction of `wheel-right' and `wheel-left'.",
    ),
    (
        "mouse-wheel-follow-mouse",
        "Whether the mouse wheel should scroll the window that the mouse is over.",
    ),
    (
        "mouse-wheel-inhibit-click-time",
        "Time in seconds to inhibit clicking on mouse wheel button after scroll.",
    ),
    (
        "mouse-wheel-left-event",
        "Event used for scrolling left, beside `wheel-left', if any.",
    ),
    (
        "mouse-wheel-mode",
        "Non-nil if Mouse-Wheel mode is enabled.",
    ),
    (
        "mouse-wheel-mode-hook",
        "Hook run after entering or leaving `mouse-wheel-mode'.",
    ),
    (
        "mouse-wheel-progressive-speed",
        "If nil, scrolling speed is proportional to the wheel speed.",
    ),
    (
        "mouse-wheel-right-event",
        "Event used for scrolling right, beside `wheel-right', if any.",
    ),
    (
        "mouse-wheel-scroll-amount",
        "Amount to scroll windows by when spinning the mouse wheel.",
    ),
    (
        "mouse-wheel-scroll-amount-horizontal",
        "Amount to scroll windows horizontally.",
    ),
    (
        "mouse-wheel-tilt-scroll",
        "Enable horizontal scrolling by tilting mouse wheel or via touchpad.",
    ),
    (
        "mouse-wheel-up-event",
        "Event used for scrolling up, beside `wheel-down', if any.",
    ),
    (
        "mouse-yank-at-point",
        "If non-nil, mouse yank commands yank at point instead of at click.",
    ),
    ("msb-mode", "Non-nil if Msb mode is enabled."),
    (
        "mule-keymap",
        "Keymap for Mule (Multilingual environment) specific commands.",
    ),
    (
        "mule-menu-keymap",
        "Keymap for Mule (Multilingual environment) menu specific commands.",
    ),
    (
        "mule-version",
        "Version number and name of this version of MULE (multilingual environment).",
    ),
    (
        "mule-version-date",
        "Distribution date of this version of MULE (multilingual environment).",
    ),
    (
        "multi-isearch-buffer-list",
        "Sequence of buffers visited by multiple buffers Isearch.",
    ),
    (
        "multi-isearch-current-buffer",
        "The buffer where the search is currently searching.",
    ),
    (
        "multi-isearch-file-list",
        "Sequence of files visited by multiple file buffers Isearch.",
    ),
    (
        "multi-isearch-next-buffer-current-function",
        "The currently active function to get the next buffer to search.",
    ),
    (
        "multi-isearch-next-buffer-function",
        "Function to call to get the next buffer to search.",
    ),
    (
        "multi-message-max",
        "Max size of the list of accumulated messages.",
    ),
    (
        "multi-message-timeout",
        "Number of seconds between messages before clearing the accumulated list.",
    ),
    (
        "multi-query-replace-map",
        "Keymap that defines additional bindings for multi-buffer replacements.",
    ),
    (
        "mwheel-inhibit-click-event-timer",
        "Timer running while mouse wheel click event is inhibited.",
    ),
    (
        "mwheel-scroll-down-function",
        "Function that does the job of scrolling downward.",
    ),
    (
        "mwheel-scroll-left-function",
        "Function that does the job of scrolling left.",
    ),
    (
        "mwheel-scroll-right-function",
        "Function that does the job of scrolling right.",
    ),
    (
        "mwheel-scroll-up-function",
        "Function that does the job of scrolling upward.",
    ),
    ("narrow-map", "Keymap for narrowing commands."),
    (
        "narrow-to-defun-include-comments",
        "If non-nil, `narrow-to-defun' will also show comments preceding the defun.",
    ),
    (
        "next-error--message-highlight-overlay",
        "Overlay highlighting the current error message in the `next-error' buffer.",
    ),
    (
        "next-error-buffer",
        "The buffer-local value of the most recent `next-error' buffer.",
    ),
    (
        "next-error-find-buffer-function",
        "Function called to find a `next-error' capable buffer.",
    ),
    (
        "next-error-follow-minor-mode",
        "Non-nil if Next-Error-Follow minor mode is enabled.",
    ),
    (
        "next-error-follow-minor-mode-hook",
        "Hook run after entering or leaving `next-error-follow-minor-mode'.",
    ),
    (
        "next-error-found-function",
        "Function called when a next locus is found and displayed.",
    ),
    (
        "next-error-function",
        "Function to use to find the next error in the current buffer.",
    ),
    (
        "next-error-highlight",
        "Highlighting of locations in the selected buffer.",
    ),
    (
        "next-error-highlight-no-select",
        "Highlighting of locations in non-selected source buffers.",
    ),
    (
        "next-error-hook",
        "List of hook functions run by `next-error' after visiting source file.",
    ),
    (
        "next-error-last-buffer",
        "The most recent `next-error' buffer.",
    ),
    (
        "next-error-message-highlight",
        "If non-nil, highlight the current error message in the `next-error' buffer.",
    ),
    (
        "next-error-move-function",
        "Function to use to move to an error locus.",
    ),
    (
        "next-error-recenter",
        "Display the line in the visited source file recentered as specified.",
    ),
    (
        "next-error-repeat-map",
        "Keymap to repeat `next-error' and `previous-error'.  Used in `repeat-mode'.",
    ),
    (
        "next-error-verbose",
        "If non-nil, `next-error' always outputs the current error buffer.",
    ),
    (
        "next-line-add-newlines",
        "If non-nil, `next-line' inserts newline to avoid `end of buffer' error.",
    ),
    (
        "next-selection-coding-system",
        "Coding system for the next communication with other programs.",
    ),
    (
        "non-essential",
        "Whether the currently executing code is performing an essential task.",
    ),
    (
        "normal-auto-fill-function",
        "The function to use for `auto-fill-function' if Auto Fill mode is turned on.",
    ),
    (
        "normal-erase-is-backspace",
        "Set the default behavior of the Delete and Backspace keys.",
    ),
    (
        "normal-erase-is-backspace-mode-hook",
        "Hook run after entering or leaving `normal-erase-is-backspace-mode'.",
    ),
    ("null-device", "The system null device."),
    (
        "obarray-cache",
        "If non-nil, a hash table of cached obarray-related information.",
    ),
    (
        "occur-collect-regexp-history",
        "History of regexp for occur's collect operation.",
    ),
    (
        "occur-edit-mode-abbrev-table",
        "Abbrev table for `occur-edit-mode'.",
    ),
    (
        "occur-edit-mode-hook",
        "Hook run after entering `occur-edit-mode'.",
    ),
    ("occur-edit-mode-map", "Keymap for `occur-edit-mode'."),
    (
        "occur-edit-mode-syntax-table",
        "Syntax table for `occur-edit-mode'.",
    ),
    (
        "occur-excluded-properties",
        "Text properties to discard when copying lines to the *Occur* buffer.",
    ),
    (
        "occur-highlight-overlays",
        "Overlays used to temporarily highlight occur matches.",
    ),
    (
        "occur-hook",
        "Hook run by Occur when there are any matches.",
    ),
    ("occur-menu-map", "Menu for `occur-mode'."),
    ("occur-mode-abbrev-table", "Abbrev table for `occur-mode'."),
    (
        "occur-mode-find-occurrence-hook",
        "Hook run by Occur after locating an occurrence.",
    ),
    ("occur-mode-hook", "Hook run when entering Occur mode."),
    ("occur-mode-map", "Keymap for `occur-mode'."),
    ("occur-mode-syntax-table", "Syntax table for `occur-mode'."),
    (
        "occur-revert-arguments",
        "Arguments to pass to `occur-1' to revert an Occur mode buffer.",
    ),
    (
        "only-global-abbrevs",
        "Non-nil means user plans to use only global abbrevs.",
    ),
    (
        "oriya-composable-pattern",
        "Regexp matching a composable sequence of Oriya characters.",
    ),
    (
        "other-window-repeat-map",
        "Keymap to repeat `other-window'.  Used in `repeat-mode'.",
    ),
    (
        "out-of-memory-warning-percentage",
        "Warn if file size exceeds this percentage of available free memory.",
    ),
    (
        "overwrite-mode-binary",
        "The string displayed in the mode line when in binary overwrite mode.",
    ),
    (
        "overwrite-mode-hook",
        "Hook run after entering or leaving `overwrite-mode'.",
    ),
    (
        "overwrite-mode-textual",
        "The string displayed in the mode line when in overwrite mode.",
    ),
    (
        "package--activated",
        "Non-nil if `package-activate-all' has been run.",
    ),
    (
        "package--builtin-versions",
        "Alist giving the version of each versioned builtin package.",
    ),
    (
        "package-activated-list",
        "List of the names of currently activated packages.",
    ),
    (
        "package-directory-list",
        "List of additional directories containing Emacs Lisp packages.",
    ),
    (
        "package-enable-at-startup",
        "Whether to make installed packages available when Emacs starts.",
    ),
    (
        "package-quickstart-file",
        "Location of the file used to speed up activation of packages at startup.",
    ),
    (
        "package-user-dir",
        "Directory containing the user's Emacs Lisp packages.",
    ),
    (
        "page-delimiter",
        "Regexp describing line-beginnings that separate pages.",
    ),
    (
        "page-navigation-repeat-map",
        "Keymap to repeat `forward-page' and `backward-page'.  Used in `repeat-mode'.",
    ),
    (
        "paragraph-ignore-fill-prefix",
        "Non-nil means the paragraph commands are not affected by `fill-prefix'.",
    ),
    (
        "paragraph-indent-minor-mode",
        "Non-nil if Paragraph-Indent minor mode is enabled.",
    ),
    (
        "paragraph-indent-minor-mode-hook",
        "Hook run after entering or leaving `paragraph-indent-minor-mode'.",
    ),
    (
        "paragraph-indent-text-mode-hook",
        "Hook run after entering `paragraph-indent-text-mode'.",
    ),
    (
        "paragraph-indent-text-mode-map",
        "Keymap for `paragraph-indent-text-mode'.",
    ),
    (
        "paragraph-separate",
        "Regexp for beginning of a line that separates paragraphs.",
    ),
    (
        "paragraph-start",
        "Regexp for beginning of a line that starts OR separates paragraphs.",
    ),
    (
        "parens-require-spaces",
        "If non-nil, add whitespace as needed when inserting parentheses.",
    ),
    ("password-cache", "Whether to cache passwords."),
    (
        "password-cache-expiry",
        "How many seconds passwords are cached, or nil to disable expiring.",
    ),
    (
        "password-colon-equivalents",
        "List of characters equivalent to trailing colon in \"password\" prompts.",
    ),
    (
        "password-word-equivalents",
        "List of words equivalent to \"password\".",
    ),
    (
        "pending-undo-list",
        "Within a run of consecutive undo commands, list remaining to be undone.",
    ),
    (
        "permanently-enabled-local-variables",
        "A list of file-local variables that are always enabled.",
    ),
    (
        "personal-keybindings",
        "List of bindings performed by `bind-key'.",
    ),
    ("pi", "Obsolete since Emacs-23.3.  Use `float-pi' instead."),
    (
        "pixel-scroll-mode",
        "Non-nil if Pixel-Scroll mode is enabled.",
    ),
    (
        "pixel-scroll-precision-mode",
        "Non-nil if Pixel-Scroll-Precision mode is enabled.",
    ),
    (
        "pop-up-frame-alist",
        "Alist of parameters for automatically generated new frames.",
    ),
    (
        "pop-up-frame-function",
        "Function used by `display-buffer' for creating a new frame.",
    ),
    (
        "pop-up-frames",
        "Whether `display-buffer' should make a separate frame.",
    ),
    (
        "pop-up-windows",
        "Non-nil means `display-buffer' should make a new window.",
    ),
    (
        "post-text-conversion-hook",
        "Hook run after text is inserted by an input method.",
    ),
    ("pre-redisplay-functions", "Hook run just before redisplay."),
    (
        "prefix-command-echo-keystrokes-functions",
        "Abnormal hook that constructs the description of the current prefix state.",
    ),
    (
        "prefix-command-preserve-state-hook",
        "Normal hook run when a command needs to preserve the prefix.",
    ),
    ("prettify-symbols-alist", "Alist of symbol prettifications."),
    (
        "prettify-symbols-compose-predicate",
        "A predicate for deciding if the currently matched symbol is to be composed.",
    ),
    (
        "prettify-symbols-mode",
        "Non-nil if Prettify-Symbols mode is enabled.",
    ),
    (
        "prettify-symbols-mode-hook",
        "Hook run after entering or leaving `prettify-symbols-mode'.",
    ),
    (
        "prettify-symbols-unprettify-at-point",
        "If non-nil, show the non-prettified version of a symbol when point is on it.",
    ),
    (
        "previous-transient-input-method",
        "The input method that was active before enabling the transient input method.",
    ),
    (
        "printer-name",
        "The name of a local printer to which data is sent for printing.",
    ),
    (
        "process-file-return-signal-string",
        "Whether to return a string describing the signal interrupting a process.",
    ),
    (
        "process-file-side-effects",
        "Whether a call of `process-file' changes remote files.",
    ),
    (
        "process-menu-mode-abbrev-table",
        "Abbrev table for `process-menu-mode'.",
    ),
    (
        "process-menu-mode-hook",
        "Hook run after entering `process-menu-mode'.",
    ),
    ("process-menu-mode-map", "Keymap for `process-menu-mode'."),
    (
        "process-menu-mode-syntax-table",
        "Syntax table for `process-menu-mode'.",
    ),
    (
        "prog-indentation-context",
        "When non-nil, provides context for indenting embedded code chunks.",
    ),
    ("prog-mode-abbrev-table", "Abbrev table for `prog-mode'."),
    (
        "prog-mode-hook",
        "Normal hook run when entering programming modes.",
    ),
    ("prog-mode-map", "Keymap used for programming modes."),
    ("prog-mode-syntax-table", "Syntax table for `prog-mode'."),
    (
        "progress-reporter--pulse-characters",
        "Characters to use for pulsing progress reporters.",
    ),
    (
        "project-mode-line",
        "Whether to show current project name and Project menu on the mode line.",
    ),
    ("project-prefix-map", "Keymap for project commands."),
    (
        "ps-page-dimensions-database",
        "List associating a symbolic paper type to its width, height and doc media.",
    ),
    ("ps-paper-type", "Specify the size of paper to format for."),
    (
        "ps-print-color-p",
        "Specify how buffer's text color is printed.",
    ),
    (
        "pure-space-overflow",
        "Non-nil if building Emacs overflowed pure space.",
    ),
    (
        "query-about-changed-file",
        "If non-nil, query the user when re-visiting a file that has changed.",
    ),
    (
        "query-replace-defaults",
        "Default values of FROM-STRING and TO-STRING for `query-replace'.",
    ),
    (
        "query-replace-from-history-variable",
        "History list to use for the FROM argument of `query-replace' commands.",
    ),
    (
        "query-replace-from-to-separator",
        "String that separates FROM and TO in the history of replacement pairs.",
    ),
    (
        "query-replace-help",
        "Help message while in `query-replace'.",
    ),
    (
        "query-replace-highlight",
        "Non-nil means to highlight matches during query replacement.",
    ),
    (
        "query-replace-highlight-submatches",
        "Whether to highlight regexp subexpressions during query replacement.",
    ),
    (
        "query-replace-history",
        "Default history list for `query-replace' commands.",
    ),
    (
        "query-replace-lazy-highlight",
        "Controls the lazy-highlighting during query replacements.",
    ),
    (
        "query-replace-map",
        "Keymap of responses to questions posed by commands like `query-replace'.",
    ),
    (
        "query-replace-read-from-default",
        "Function to get default non-regexp value for `query-replace-read-from'.",
    ),
    (
        "query-replace-read-from-regexp-default",
        "Function to get default regexp value for `query-replace-read-from'.",
    ),
    (
        "query-replace-show-replacement",
        "Non-nil means show substituted replacement text in the minibuffer.",
    ),
    (
        "query-replace-skip-read-only",
        "Non-nil means `query-replace' and friends ignore read-only matches.",
    ),
    (
        "query-replace-to-history-variable",
        "History list to use for the TO argument of `query-replace' commands.",
    ),
    (
        "quit-window-hook",
        "Hook run before performing any other actions in the `quit-window' command.",
    ),
    (
        "radians-to-degrees",
        "Radian to degree conversion constant.",
    ),
    (
        "rcirc-track-minor-mode",
        "Non-nil if Rcirc-Track minor mode is enabled.",
    ),
    ("read--expression-map", "Keymap used by `read--expression'."),
    (
        "read-answer-short",
        "If non-nil, the `read-answer' function accepts single-character answers.",
    ),
    (
        "read-char-by-name-sort",
        "How to sort characters for `read-char-by-name' completion.",
    ),
    (
        "read-char-choice-use-read-key",
        "If non-nil, use `read-key' when reading a character by `read-char-choice'.",
    ),
    (
        "read-char-from-minibuffer-map",
        "Keymap for the `read-char-from-minibuffer' function.",
    ),
    (
        "read-char-history",
        "The default history for the `read-char-from-minibuffer' function.",
    ),
    (
        "read-extended-command-mode",
        "Non-nil if Read-Extended-Command mode is enabled.",
    ),
    (
        "read-extended-command-mode-hook",
        "Hook run after entering or leaving `read-extended-command-mode'.",
    ),
    (
        "read-extended-command-mode-map",
        "Local keymap added to the current map when reading an extended command.",
    ),
    (
        "read-extended-command-predicate",
        "Predicate to use to determine which commands to include when completing.",
    ),
    (
        "read-face-name-sample-text",
        "Text string to display as the sample text for `read-face-name'.",
    ),
    (
        "read-file-name-completion-ignore-case",
        "Non-nil means when reading a file name completion ignores case.",
    ),
    (
        "read-file-name-function",
        "The function called by `read-file-name' to do its work.",
    ),
    ("read-key-empty-map", "Used internally by `read-key'."),
    ("read-key-full-map", "Used internally by `read-key'."),
    (
        "read-mail-command",
        "Your preference for a mail reading package.",
    ),
    (
        "read-number-history",
        "The default history for the `read-number' function.",
    ),
    (
        "read-only-mode-hook",
        "Hook run after entering or leaving `read-only-mode'.",
    ),
    (
        "read-quoted-char-radix",
        "Radix for \\[quoted-insert] and other uses of `read-quoted-char'.",
    ),
    (
        "read-regexp-defaults-function",
        "Function that provides default regexp(s) for `read-regexp'.",
    ),
    (
        "recenter-last-op",
        "Indicates the last recenter operation performed.",
    ),
    (
        "recenter-positions",
        "Cycling order for `recenter-top-bottom'.",
    ),
    ("recentf-mode", "Non-nil if Recentf mode is enabled."),
    (
        "redisplay-highlight-region-function",
        "Function to move the region-highlight overlay.",
    ),
    (
        "redisplay-unhighlight-region-function",
        "Function to remove the region-highlight overlay.",
    ),
    (
        "reference-point-alist",
        "Alist of symbols vs integer codes of glyph reference points.",
    ),
    (
        "regexp-history",
        "History list for some commands that read regular expressions.",
    ),
    (
        "regexp-search-ring",
        "List of regular expression search string sequences.",
    ),
    (
        "regexp-search-ring-max",
        "Maximum length of regexp search ring before oldest elements are thrown away.",
    ),
    (
        "regexp-search-ring-yank-pointer",
        "Index in `regexp-search-ring' of last string reused.",
    ),
    (
        "regexp-unmatchable",
        "Standard regexp guaranteed not to match any string at all.",
    ),
    (
        "region-insert-function",
        "Function to insert the region's content.",
    ),
    (
        "register--read-with-preview-function",
        "Function to use for reading a register name with preview.",
    ),
    (
        "register-alist",
        "Alist of elements (NAME . CONTENTS), one for each Emacs register.",
    ),
    (
        "register-preview-default-keys",
        "Default keys for setting a new register.",
    ),
    (
        "register-preview-delay",
        "If non-nil, time to wait in seconds before popping up register preview window.",
    ),
    (
        "register-preview-display-buffer-alist",
        "Window configuration for the register preview buffer.",
    ),
    (
        "register-preview-function",
        "Function to format a register for previewing.",
    ),
    (
        "register-separator",
        "Register containing the text to put between collected texts, or nil if none.",
    ),
    (
        "register-use-preview",
        "Whether register commands show preview of registers with non-nil values.",
    ),
    (
        "remote-file-name-access-timeout",
        "Timeout (in seconds) for `access-file'.",
    ),
    (
        "remote-file-name-inhibit-auto-save",
        "When nil, `auto-save-mode' will auto-save remote files.",
    ),
    (
        "remote-file-name-inhibit-auto-save-visited",
        "When nil, `auto-save-visited-mode' will auto-save remote files.",
    ),
    (
        "remote-file-name-inhibit-cache",
        "Whether to use the remote file-name cache for read access.",
    ),
    (
        "remote-file-name-inhibit-delete-by-moving-to-trash",
        "Whether remote files shall be moved to the Trash.",
    ),
    (
        "remote-file-name-inhibit-locks",
        "Whether to create file locks for remote files.",
    ),
    (
        "remote-shell-program",
        "Program to use to execute commands on a remote host (i.e. ssh).",
    ),
    (
        "reorder-enders",
        "Regular expression for characters that end forced-reordered text.",
    ),
    (
        "reorder-starters",
        "Regular expression for characters that start forced-reordered text.",
    ),
    (
        "repeat-map",
        "The value of the repeating transient map for the next command.",
    ),
    ("repeat-mode", "Non-nil if Repeat mode is enabled."),
    (
        "replace-char-fold",
        "Non-nil means replacement commands should do character folding in matches.",
    ),
    ("replace-count", "Number of replacements done so far."),
    (
        "replace-lax-whitespace",
        "Non-nil means `query-replace' matches a sequence of whitespace chars.",
    ),
    (
        "replace-re-search-function",
        "Function to use when searching for regexps to replace.",
    ),
    (
        "replace-regexp-function",
        "Function to convert the FROM string of query-replace commands to a regexp.",
    ),
    (
        "replace-regexp-lax-whitespace",
        "Non-nil means `query-replace-regexp' matches a sequence of whitespace chars.",
    ),
    (
        "replace-search-function",
        "Function to use when searching for strings to replace.",
    ),
    (
        "replace-update-post-hook",
        "Function(s) to call after `query-replace' has found a match in the buffer.",
    ),
    (
        "repunctuate-sentences-filter",
        "The default filter used by `repunctuate-sentences'.",
    ),
    (
        "require-final-newline",
        "Whether to add a newline automatically at the end of the file.",
    ),
    (
        "resize-temp-buffer-window-inhibit",
        "Non-nil means `resize-temp-buffer-window' should not resize.",
    ),
    (
        "resize-window-repeat-map",
        "Keymap to repeat window resizing commands.",
    ),
    (
        "revert-buffer-function",
        "Function to use to revert this buffer.",
    ),
    (
        "revert-buffer-in-progress-p",
        "Non-nil if a `revert-buffer' operation is in progress, nil otherwise.",
    ),
    (
        "revert-buffer-insert-file-contents-function",
        "Function to use to insert contents when reverting this buffer.",
    ),
    (
        "revert-buffer-quick-short-answers",
        "How much confirmation to be done by the `revert-buffer-quick' command.",
    ),
    (
        "revert-buffer-restore-functions",
        "Functions to preserve buffer state during `revert-buffer'.",
    ),
    (
        "revert-buffer-with-fine-grain-max-seconds",
        "Maximum time that `revert-buffer-with-fine-grain' should use.",
    ),
    (
        "revert-without-query",
        "Specify which files should be reverted without query.",
    ),
    (
        "rfn-eshadow-setup-minibuffer-hook",
        "Minibuffer setup functions from other packages.",
    ),
    (
        "rfn-eshadow-update-overlay-hook",
        "Customer overlay functions from other packages.",
    ),
    (
        "rmail-displayed-headers",
        "Regexp to match Header fields that Rmail should display.",
    ),
    (
        "rmail-file-coding-system",
        "Coding system used in RMAIL file.",
    ),
    ("rmail-file-name", "Name of user's primary mail file."),
    (
        "rmail-highlighted-headers",
        "Regexp to match Header fields that Rmail should normally highlight.",
    ),
    (
        "rmail-ignored-headers",
        "Regexp to match header fields that Rmail should normally hide.",
    ),
    (
        "rmail-insert-mime-forwarded-message-function",
        "Function to insert a message in MIME format so it can be forwarded.",
    ),
    (
        "rmail-mode-hook",
        "List of functions to call when Rmail is invoked.",
    ),
    (
        "rmail-primary-inbox-list",
        "List of files that are inboxes for your primary mail file `rmail-file-name'.",
    ),
    (
        "rmail-retry-ignored-headers",
        "Headers that should be stripped when retrying a failed message.",
    ),
    (
        "rmail-secondary-file-directory",
        "Directory for additional secondary Rmail files.",
    ),
    (
        "rmail-secondary-file-regexp",
        "Regexp for which files are secondary Rmail files.",
    ),
    (
        "rmail-show-message-hook",
        "List of functions to call when Rmail displays a message.",
    ),
    (
        "rmail-spool-directory",
        "Name of directory used by system mailer for delivering new mail.",
    ),
    (
        "rmail-user-mail-address-regexp",
        "Regexp matching user mail addresses.",
    ),
    ("ruler-mode", "Non-nil if Ruler mode is enabled."),
    (
        "safe-local-eval-forms",
        "Expressions that are considered safe in an `eval:' local variable.",
    ),
    (
        "safe-local-variable-directories",
        "A list of directories where local variables are always enabled.",
    ),
    (
        "safe-local-variable-values",
        "List of variable-value pairs that are considered safe.",
    ),
    (
        "same-window-buffer-names",
        "List of names of buffers that should appear in the \"same\" window.",
    ),
    (
        "same-window-regexps",
        "List of regexps saying which buffers should appear in the \"same\" window.",
    ),
    (
        "save-abbrevs",
        "Non-nil means save word abbrevs too when files are saved.",
    ),
    (
        "save-buffer-coding-system",
        "If non-nil, use this coding system for saving the buffer.",
    ),
    (
        "save-interprogram-paste-before-kill",
        "Whether to save existing clipboard text into kill ring before replacing it.",
    ),
    ("save-place-mode", "Non-nil if Save-Place mode is enabled."),
    (
        "save-silently",
        "If non-nil, avoid messages when saving files.",
    ),
    (
        "save-some-buffers-action-alist",
        "ACTION-ALIST argument used in call to `map-y-or-n-p'.",
    ),
    (
        "save-some-buffers-default-predicate",
        "Default predicate for `save-some-buffers'.",
    ),
    (
        "save-some-buffers-functions",
        "Functions to be run by `save-some-buffers' after saving the buffers.",
    ),
    ("savehist-mode", "Non-nil if Savehist mode is enabled."),
    ("scroll-all-mode", "Non-nil if Scroll-All mode is enabled."),
    (
        "scroll-bar-mode",
        "Specify whether to have vertical scroll bars, and on which side.",
    ),
    (
        "scroll-bar-mode-explicit",
        "Non-nil means `set-scroll-bar-mode' should really do something.",
    ),
    (
        "scroll-bar-mode-hook",
        "Hook run after entering or leaving `scroll-bar-mode'.",
    ),
    (
        "scroll-error-top-bottom",
        "Move point to top/bottom of buffer before signaling a scrolling error.",
    ),
    (
        "search-default-mode",
        "Default mode to use when starting isearch.",
    ),
    (
        "search-exit-option",
        "Defines what control characters do in incremental search.",
    ),
    (
        "search-highlight",
        "Non-nil means incremental search highlights the current match.",
    ),
    (
        "search-highlight-submatches",
        "Whether to highlight regexp subexpressions of the current regexp match.",
    ),
    (
        "search-invisible",
        "If t incremental search/query-replace can match hidden text.",
    ),
    ("search-map", "Keymap for search related commands."),
    (
        "search-nonincremental-instead",
        "If non-nil, do a nonincremental search instead of exiting immediately.",
    ),
    ("search-ring", "List of search string sequences."),
    (
        "search-ring-max",
        "Maximum length of search ring before oldest elements are thrown away.",
    ),
    (
        "search-ring-update",
        "Non-nil if advancing or retreating in the search ring should cause search.",
    ),
    (
        "search-ring-yank-pointer",
        "Index in `search-ring' of last string reused.",
    ),
    (
        "search-slow-speed",
        "Highest terminal speed at which to use \"slow\" style incremental search.",
    ),
    (
        "search-slow-window-lines",
        "Number of lines in slow search display windows.",
    ),
    (
        "search-upper-case",
        "If non-nil, upper case chars disable case fold searching.",
    ),
    (
        "search-whitespace-regexp",
        "If non-nil, regular expression to match a sequence of whitespace chars.",
    ),
    (
        "secondary-tool-bar-map",
        "Optional secondary keymap for the tool bar.",
    ),
    (
        "select-enable-clipboard",
        "Non-nil means cutting and pasting uses the clipboard.",
    ),
    (
        "select-enable-primary",
        "Non-nil means cutting and pasting uses the primary selection.",
    ),
    (
        "select-safe-coding-system-accept-default-p",
        "If non-nil, a function to control the behavior of coding system selection.",
    ),
    (
        "selection-coding-system",
        "Coding system for communicating with other programs.",
    ),
    (
        "self-insert-uses-region-functions",
        "Special hook to tell if `self-insert-command' will use the region.",
    ),
    (
        "semantic-default-submodes",
        "List of auxiliary Semantic minor modes enabled by `semantic-mode'.",
    ),
    ("semantic-mode", "Non-nil if Semantic mode is enabled."),
    (
        "send-mail-function",
        "Function to call to send the current buffer as mail.",
    ),
    (
        "sendmail-coding-system",
        "Coding system for encoding the outgoing mail.",
    ),
    ("sentence-end", "Regexp describing the end of a sentence."),
    (
        "sentence-end-base",
        "Regexp matching the basic end of a sentence, not including following space.",
    ),
    (
        "sentence-end-double-space",
        "Non-nil means a single space does not end a sentence.",
    ),
    (
        "sentence-end-without-period",
        "Non-nil means a sentence will end without a period.",
    ),
    (
        "sentence-end-without-space",
        "String of characters that end sentence without following spaces.",
    ),
    ("server-mode", "Non-nil if Server mode is enabled."),
    (
        "set-auto-coding-for-load",
        "Non-nil means respect a \"unibyte: t\" entry in file local variables.",
    ),
    (
        "set-auto-mode--last",
        "Remember the mode we have set via `set-auto-mode-0'.",
    ),
    (
        "set-language-environment-hook",
        "Normal hook run after some language environment is set.",
    ),
    (
        "set-mark-command-repeat-pop",
        "Non-nil means repeating \\[set-mark-command] after popping mark pops it again.",
    ),
    (
        "set-message-functions",
        "List of functions to handle display of echo-area messages.",
    ),
    (
        "set-transient-map-timeout",
        "Timeout in seconds for deactivation of a transient keymap.",
    ),
    (
        "set-transient-map-timer",
        "Timer for `set-transient-map-timeout'.",
    ),
    (
        "set-variable-value-history",
        "History of values entered with `set-variable'.",
    ),
    (
        "shell-command-buffer-name",
        "Name of the output buffer for shell commands.",
    ),
    (
        "shell-command-buffer-name-async",
        "Name of the output buffer for asynchronous shell commands.",
    ),
    (
        "shell-command-default-error-buffer",
        "Buffer name for `shell-command' and `shell-command-on-region' error output.",
    ),
    (
        "shell-command-dont-erase-buffer",
        "Whether to erase the output buffer before executing shell command.",
    ),
    (
        "shell-command-history",
        "History list for some commands that read shell commands.",
    ),
    (
        "shell-command-prompt-show-cwd",
        "If non-nil, show current directory when prompting for a shell command.",
    ),
    (
        "shell-command-saved-pos",
        "Record of point positions in output buffers after command completion.",
    ),
    (
        "shell-command-switch",
        "Switch used to have the shell execute its command line argument.",
    ),
    (
        "shell-dumb-shell-regexp",
        "Regexp to match shells that don't save their command history, and",
    ),
    (
        "shift-select-mode",
        "When non-nil, shifted motion keys activate the mark momentarily.",
    ),
    (
        "show-paren--overlay",
        "Overlay used to highlight the matching paren.",
    ),
    (
        "show-paren--overlay-1",
        "Overlay used to highlight the paren at point.",
    ),
    (
        "show-paren-context-when-offscreen",
        "If non-nil, show context around the opening paren if it is offscreen.",
    ),
    (
        "show-paren-data-function",
        "Function to find the opener/closer \"near\" point and its match.",
    ),
    (
        "show-paren-delay",
        "Time in seconds to delay before showing a matching paren.",
    ),
    (
        "show-paren-highlight-openparen",
        "Non-nil turns on openparen highlighting when matching forward.",
    ),
    (
        "show-paren-local-mode-hook",
        "Hook run after entering or leaving `show-paren-local-mode'.",
    ),
    ("show-paren-mode", "Non-nil if Show-Paren mode is enabled."),
    (
        "show-paren-mode-hook",
        "Hook run after entering or leaving `show-paren-mode'.",
    ),
    (
        "show-paren-predicate",
        "Whether to use `show-paren-mode' in a buffer.",
    ),
    (
        "show-paren-priority",
        "Priority of paren highlighting overlays.",
    ),
    (
        "show-paren-ring-bell-on-mismatch",
        "If non-nil, beep if mismatched paren is detected.",
    ),
    (
        "show-paren-style",
        "Style used when showing a matching paren.",
    ),
    (
        "show-paren-when-point-in-periphery",
        "If non-nil, show parens when point is in the line's periphery.",
    ),
    (
        "show-paren-when-point-inside-paren",
        "If non-nil, show parens when point is just inside one.",
    ),
    (
        "site-run-file",
        "File containing site-wide run-time initializations.",
    ),
    (
        "size-indication-mode",
        "Non-nil if Size-Indication mode is enabled.",
    ),
    (
        "size-indication-mode-hook",
        "Hook run after entering or leaving `size-indication-mode'.",
    ),
    (
        "skeleton-filter-function",
        "Function for transforming a skeleton proxy's aliases' variable value.",
    ),
    ("slitex-run-command", "Command used to run SliTeX subjob."),
    (
        "small-temporary-file-directory",
        "The directory for writing small temporary files.",
    ),
    (
        "sort-coding-systems-predicate",
        "If non-nil, a predicate function to sort coding systems.",
    ),
    (
        "special-display-buffer-names",
        "List of names of buffers that should be displayed specially.",
    ),
    (
        "special-display-frame-alist",
        "Alist of parameters for special frames.",
    ),
    (
        "special-display-function",
        "Function to call for displaying special buffers.",
    ),
    (
        "special-display-regexps",
        "List of regexps saying which buffers should be displayed specially.",
    ),
    (
        "special-mode-abbrev-table",
        "Abbrev table for `special-mode'.",
    ),
    (
        "special-mode-hook",
        "Hook run after entering `special-mode'.",
    ),
    ("special-mode-map", "Keymap for `special-mode'."),
    (
        "special-mode-syntax-table",
        "Syntax table for `special-mode'.",
    ),
    ("splash-screen-keymap", "Keymap for splash screen buffer."),
    (
        "split-height-threshold",
        "Minimum height for splitting windows sensibly.",
    ),
    (
        "split-string-default-separators",
        "The default value of separators for `split-string'.",
    ),
    (
        "split-width-threshold",
        "Minimum width for splitting windows sensibly.",
    ),
    (
        "split-window-keep-point",
        "If non-nil, \\[split-window-below] preserves point in the new window.",
    ),
    (
        "split-window-preferred-function",
        "Function called by `display-buffer' routines to split a window.",
    ),
    (
        "standard-fontset-spec",
        "String of fontset spec of the standard fontset.",
    ),
    (
        "standard-indent",
        "Default number of columns for margin-changing functions to indent.",
    ),
    (
        "startup--original-eln-load-path",
        "Original value of `native-comp-eln-load-path'.",
    ),
    ("strokes-mode", "Non-nil if Strokes mode is enabled."),
    (
        "suggest-key-bindings",
        "Non-nil means show the equivalent keybinding when \\[execute-extended-command] has one.",
    ),
    (
        "suspend-hook",
        "Normal hook run by `suspend-emacs', before suspending.",
    ),
    (
        "suspend-resume-hook",
        "Normal hook run by `suspend-emacs', after Emacs is continued.",
    ),
    (
        "switch-to-buffer-in-dedicated-window",
        "Allow switching to buffer in strongly dedicated windows.",
    ),
    (
        "switch-to-buffer-obey-display-actions",
        "If non-nil, `switch-to-buffer' runs `pop-to-buffer-same-window' instead.",
    ),
    (
        "switch-to-buffer-preserve-window-point",
        "If non-nil, `switch-to-buffer' tries to preserve `window-point'.",
    ),
    (
        "switch-to-prev-buffer-skip",
        "Buffers `switch-to-prev-buffer' should skip.",
    ),
    (
        "switch-to-prev-buffer-skip-regexp",
        "Buffers that `switch-to-prev-buffer' and `switch-to-next-buffer' should skip.",
    ),
    (
        "switch-to-visible-buffer",
        "If non-nil, allow switching to an already visible buffer.",
    ),
    (
        "syntax-begin-function",
        "Function to move back outside of any comment/string/paren.",
    ),
    (
        "syntax-ppss-max-span",
        "Threshold below which cache info is deemed unnecessary.",
    ),
    (
        "syntax-ppss-narrow",
        "Same as `syntax-ppss-wide' but for a narrowed buffer.",
    ),
    (
        "syntax-ppss-narrow-start",
        "Start position of the narrowing for `syntax-ppss-narrow'.",
    ),
    (
        "syntax-ppss-stats",
        "Statistics about which case is more/less frequent in `syntax-ppss'.",
    ),
    (
        "syntax-ppss-table",
        "Syntax-table to use during `syntax-ppss', if any.",
    ),
    ("syntax-ppss-wide", "Cons of two elements (LAST . CACHE)."),
    (
        "syntax-propertize-extend-region-functions",
        "Special hook run just before proceeding to propertize a region.",
    ),
    (
        "syntax-propertize-function",
        "Mode-specific function to apply `syntax-table' text properties.",
    ),
    (
        "syntax-wholeline-max",
        "Maximum line length for syntax operations.",
    ),
    (
        "tab-always-indent",
        "Controls the operation of the TAB key.",
    ),
    (
        "tab-bar--auto-width-hash",
        "Memoization table for `tab-bar-auto-width'.",
    ),
    (
        "tab-bar-auto-width",
        "Automatically resize width of tabs on tab bar to fill available tab-bar space.",
    ),
    (
        "tab-bar-auto-width-faces",
        "Resize tabs only with these faces.",
    ),
    (
        "tab-bar-auto-width-max",
        "Maximum width for automatic resizing of width of tab-bar tabs.",
    ),
    (
        "tab-bar-auto-width-min",
        "Minimum width of tabs for automatic resizing under `tab-bar-auto-width'.",
    ),
    (
        "tab-bar-back-button",
        "Button for going back in tab history.",
    ),
    (
        "tab-bar-close-button",
        "Button for closing the clicked tab.",
    ),
    (
        "tab-bar-close-button-show",
        "Defines where to show the close tab button.",
    ),
    (
        "tab-bar-close-last-tab-choice",
        "What to do when the last tab is closed.",
    ),
    (
        "tab-bar-close-tab-select",
        "Which tab to make current after closing the specified tab.",
    ),
    (
        "tab-bar-closed-tabs",
        "A list of closed tabs to be able to undo their closing.",
    ),
    ("tab-bar-format", "Template for displaying tab bar items."),
    (
        "tab-bar-forward-button",
        "Button for going forward in tab history.",
    ),
    (
        "tab-bar-history-back",
        "History of back changes in every tab per frame.",
    ),
    (
        "tab-bar-history-done-command",
        "Command handled by `window-configuration-change-hook'.",
    ),
    (
        "tab-bar-history-forward",
        "History of forward changes in every tab per frame.",
    ),
    (
        "tab-bar-history-limit",
        "The number of history elements to keep.",
    ),
    (
        "tab-bar-history-mode",
        "Non-nil if Tab-Bar-History mode is enabled.",
    ),
    (
        "tab-bar-history-mode-hook",
        "Hook run after entering or leaving `tab-bar-history-mode'.",
    ),
    (
        "tab-bar-history-old",
        "Window configuration before the current command.",
    ),
    (
        "tab-bar-history-omit",
        "When non-nil, omit window-configuration changes from the current command.",
    ),
    (
        "tab-bar-history-pre-command",
        "Command set to `this-command' by `pre-command-hook'.",
    ),
    (
        "tab-bar-map",
        "Keymap for the commands used on the tab bar.",
    ),
    ("tab-bar-menu-bar-button", "Button for the menu bar."),
    (
        "tab-bar-minibuffer-restore-tab",
        "Tab number for `tab-bar-minibuffer-restore-tab'.",
    ),
    (
        "tab-bar-mode-hook",
        "Hook run after entering or leaving `tab-bar-mode'.",
    ),
    ("tab-bar-mode-map", "Tab Bar mode map."),
    (
        "tab-bar-move-repeat-map",
        "Keymap to repeat tab move commands `tab-move' and `tab-bar-move-tab-backward'.",
    ),
    ("tab-bar-new-button", "Button for creating a new tab."),
    (
        "tab-bar-new-button-show",
        "If non-nil, show the \"New tab\" button in the tab bar.",
    ),
    (
        "tab-bar-new-tab-choice",
        "Defines what to show in a new tab.",
    ),
    (
        "tab-bar-new-tab-group",
        "Defines what group to assign to a new tab.",
    ),
    ("tab-bar-new-tab-to", "Where to create a new tab."),
    (
        "tab-bar-select-restore-context",
        "If this is non-nil, try to restore window points from their contexts.",
    ),
    (
        "tab-bar-select-restore-windows",
        "Function called when selecting a tab to handle windows whose buffer was killed.",
    ),
    (
        "tab-bar-select-tab-modifiers",
        "List of modifier keys for selecting tab-bar tabs by their numbers.",
    ),
    ("tab-bar-separator", "String that delimits tabs."),
    ("tab-bar-show", "Defines when to show the tab bar."),
    (
        "tab-bar-switch-repeat-map",
        "Keymap to repeat tab switch commands `tab-next' and `tab-previous'.",
    ),
    (
        "tab-bar-tab-face-function",
        "Function to define a tab face.",
    ),
    (
        "tab-bar-tab-group-face-function",
        "Function to define a tab group face.",
    ),
    (
        "tab-bar-tab-group-format-function",
        "Function to format a tab group name.",
    ),
    (
        "tab-bar-tab-group-function",
        "Function to get a tab group name.",
    ),
    (
        "tab-bar-tab-hints",
        "Show absolute numbers on tabs in the tab bar before the tab name.",
    ),
    (
        "tab-bar-tab-name-format-function",
        "Function to format a tab name.",
    ),
    (
        "tab-bar-tab-name-format-functions",
        "Functions called to modify the tab name.",
    ),
    ("tab-bar-tab-name-function", "Function to get a tab name."),
    (
        "tab-bar-tab-name-truncated-max",
        "Maximum length of the tab name from the current buffer.",
    ),
    (
        "tab-bar-tab-post-change-group-functions",
        "List of functions to call after changing a tab group.",
    ),
    (
        "tab-bar-tab-post-open-functions",
        "List of functions to call after creating a new tab.",
    ),
    (
        "tab-bar-tab-post-select-functions",
        "List of functions to call after selecting a tab.",
    ),
    (
        "tab-bar-tab-pre-close-functions",
        "List of functions to call before closing a tab.",
    ),
    (
        "tab-bar-tab-prevent-close-functions",
        "List of functions to call to determine whether to close a tab.",
    ),
    (
        "tab-bar-tabs-function",
        "Function to get a list of tabs to display in the tab bar.",
    ),
    (
        "tab-first-completion",
        "Governs the behavior of TAB completion on the first press of the key.",
    ),
    ("tab-prefix-map", "Keymap for tab-bar related commands."),
    (
        "tab-stop-list",
        "List of tab stop positions used by `tab-to-tab-stop'.",
    ),
    (
        "tab-switcher-mode-abbrev-table",
        "Abbrev table for `tab-switcher-mode'.",
    ),
    (
        "tab-switcher-mode-hook",
        "Hook run after entering `tab-switcher-mode'.",
    ),
    (
        "tab-switcher-mode-map",
        "Local keymap for `tab-switcher-mode' buffers.",
    ),
    (
        "tab-switcher-mode-syntax-table",
        "Syntax table for `tab-switcher-mode'.",
    ),
    (
        "tabulated-list--header-string",
        "Holds the header if `tabulated-list-use-header-line' is nil.",
    ),
    (
        "tabulated-list-entries",
        "Entries displayed in the current Tabulated List buffer.",
    ),
    (
        "tabulated-list-format",
        "The format of the current Tabulated List mode buffer.",
    ),
    (
        "tabulated-list-groups",
        "Groups displayed in the current Tabulated List buffer.",
    ),
    (
        "tabulated-list-gui-sort-indicator-asc",
        "Indicator for columns sorted in ascending order, for GUI frames.",
    ),
    (
        "tabulated-list-gui-sort-indicator-desc",
        "Indicator for columns sorted in descending order, for GUI frames.",
    ),
    (
        "tabulated-list-mode-abbrev-table",
        "Abbrev table for `tabulated-list-mode'.",
    ),
    (
        "tabulated-list-mode-hook",
        "Hook run after entering `tabulated-list-mode'.",
    ),
    (
        "tabulated-list-mode-map",
        "Local keymap for `tabulated-list-mode' buffers.",
    ),
    (
        "tabulated-list-mode-syntax-table",
        "Syntax table for `tabulated-list-mode'.",
    ),
    (
        "tabulated-list-padding",
        "Number of characters preceding each Tabulated List mode entry.",
    ),
    (
        "tabulated-list-printer",
        "Function for inserting a Tabulated List entry at point.",
    ),
    (
        "tabulated-list-revert-hook",
        "Hook run before reverting a Tabulated List buffer.",
    ),
    (
        "tabulated-list-sort-button-map",
        "Local keymap for `tabulated-list-mode' sort buttons.",
    ),
    (
        "tabulated-list-sort-key",
        "Sort key for the current Tabulated List mode buffer.",
    ),
    (
        "tabulated-list-tty-sort-indicator-asc",
        "Indicator for columns sorted in ascending order, for text-mode frames.",
    ),
    (
        "tabulated-list-tty-sort-indicator-desc",
        "Indicator for columns sorted in ascending order, for text-mode frames.",
    ),
    (
        "tabulated-list-use-header-line",
        "Whether the Tabulated List buffer should use a header line.",
    ),
    (
        "tags-add-tables",
        "Control whether to add a new tags table to the current list.",
    ),
    (
        "tags-case-fold-search",
        "Whether tags operations should be case-sensitive.",
    ),
    (
        "tags-compression-info-list",
        "List of extensions tried by etags when `auto-compression-mode' is on.",
    ),
    ("tags-file-name", "File name of tags table."),
    (
        "tags-table-list",
        "List of file names of tags tables to search.",
    ),
    (
        "tamil-composable-pattern",
        "Regexp matching a composable sequence of Tamil characters.",
    ),
    (
        "telugu-composable-pattern",
        "Regexp matching a composable sequence of Telugu characters.",
    ),
    (
        "temp-buffer-max-height",
        "Maximum height of a window displaying a temporary buffer.",
    ),
    (
        "temp-buffer-max-width",
        "Maximum width of a window displaying a temporary buffer.",
    ),
    (
        "temp-buffer-resize-mode",
        "Non-nil if Temp-Buffer-Resize mode is enabled.",
    ),
    (
        "temp-buffer-resize-mode-hook",
        "Hook run after entering or leaving `temp-buffer-resize-mode'.",
    ),
    (
        "temp-buffer-setup-hook",
        "Normal hook run by `with-output-to-temp-buffer' at the start.",
    ),
    (
        "temp-buffer-show-hook",
        "Normal hook run by `with-output-to-temp-buffer' after displaying the buffer.",
    ),
    (
        "temp-buffer-window-setup-hook",
        "Normal hook run by `with-temp-buffer-window' before buffer display.",
    ),
    (
        "temp-buffer-window-show-hook",
        "Normal hook run by `with-temp-buffer-window' after buffer display.",
    ),
    (
        "temporary-goal-column",
        "Current goal column for vertical motion.",
    ),
    ("term-file-aliases", "Alist of terminal type aliases."),
    (
        "term-file-prefix",
        "If non-nil, Emacs startup performs terminal-specific initialization.",
    ),
    (
        "term-setup-hook",
        "Normal hook run immediately after `emacs-startup-hook'.",
    ),
    (
        "tex-alt-dvi-print-command",
        "Command used by \\[tex-print] with a prefix arg to print a .dvi file.",
    ),
    (
        "tex-bibtex-command",
        "Command used by `tex-bibtex-file' to gather bibliographic data.",
    ),
    (
        "tex-close-quote",
        "String inserted by typing \\[tex-insert-quote] to close a quotation.",
    ),
    (
        "tex-default-mode",
        "Mode to enter for a new file that might be either TeX or LaTeX.",
    ),
    (
        "tex-directory",
        "Directory in which temporary files are written.",
    ),
    (
        "tex-dvi-print-command",
        "Command used by \\[tex-print] to print a .dvi file.",
    ),
    (
        "tex-dvi-view-command",
        "Command used by \\[tex-view] to display a `.dvi' file.",
    ),
    (
        "tex-first-line-header-regexp",
        "Regexp for matching a first line which `tex-region' should include.",
    ),
    (
        "tex-main-file",
        "The main TeX source file which includes this buffer's file.",
    ),
    (
        "tex-offer-save",
        "If non-nil, ask about saving modified buffers before \\[tex-file] is run.",
    ),
    (
        "tex-open-quote",
        "String inserted by typing \\[tex-insert-quote] to open a quotation.",
    ),
    ("tex-run-command", "Command used to run TeX subjob."),
    (
        "tex-shell-file-name",
        "If non-nil, the shell file name to run in the subshell used to run TeX.",
    ),
    (
        "tex-show-queue-command",
        "Command used by \\[tex-show-print-queue] to show the print queue.",
    ),
    (
        "tex-start-commands",
        "TeX commands to use when starting TeX.",
    ),
    ("tex-start-options", "TeX options to use when starting TeX."),
    (
        "texinfo-close-quote",
        "String inserted by typing \\[texinfo-insert-quote] to close a quotation.",
    ),
    (
        "texinfo-open-quote",
        "String inserted by typing \\[texinfo-insert-quote] to open a quotation.",
    ),
    ("text-mode-abbrev-table", "Abbrev table for `text-mode'."),
    (
        "text-mode-hook",
        "Normal hook run when entering Text mode and many related modes.",
    ),
    (
        "text-mode-ispell-word-completion",
        "How Text mode provides Ispell word completion.",
    ),
    ("text-mode-map", "Keymap for `text-mode'."),
    ("text-mode-menu", "Menu for `text-mode'."),
    (
        "text-mode-syntax-table",
        "Syntax table used while in `text-mode'.",
    ),
    (
        "text-mode-variant",
        "Non-nil if this buffer's major mode is a variant of Text mode.",
    ),
    (
        "three-step-help",
        "Non-nil means give more info about Help command in three steps.",
    ),
    (
        "tibetan-composable-pattern",
        "Regexp matching a composable sequence of Tibetan characters.",
    ),
    (
        "tibetan-precomposed-regexp",
        "Regexp string to match a romanized Tibetan complex consonant.",
    ),
    (
        "tibetan-precomposition-rule-regexp",
        "Regexp string to match a sequence of Tibetan consonantic components.",
    ),
    (
        "tibetan-regexp",
        "Regexp matching a Tibetan transcription of a composable Tibetan sequence.",
    ),
    (
        "timeclock-mode-line-display",
        "Non-nil if Timeclock-Mode-Line-Display mode is enabled.",
    ),
    (
        "timer-duration-words",
        "Alist mapping temporal words to durations in seconds.",
    ),
    ("timer-event-last", "Last timer that was run."),
    ("timer-event-last-1", "Next-to-last timer that was run."),
    ("timer-event-last-2", "Third-to-last timer that was run."),
    (
        "timer-max-repeats",
        "Maximum number of times to repeat a timer, if many repeats are delayed.",
    ),
    (
        "toggle-input-method-active",
        "Non-nil inside `toggle-input-method'.",
    ),
    (
        "toggle-window-dedicated-flag",
        "What dedicated flag should `toggle-window-dedicated' use by default.",
    ),
    (
        "tool-bar-always-show-default",
        "If non-nil, `tool-bar-mode' only shows the default tool bar.",
    ),
    (
        "tool-bar-images-pixel-height",
        "Height in pixels of images in the tool-bar.",
    ),
    ("tool-bar-map", "Keymap for the tool bar."),
    (
        "tool-bar-mode-hook",
        "Hook run after entering or leaving `tool-bar-mode'.",
    ),
    (
        "tool-bar-position",
        "Specify on which side the tool bar shall be.",
    ),
    (
        "tooltip-delay",
        "Seconds to wait before displaying a tooltip the first time.",
    ),
    (
        "tooltip-frame-parameters",
        "Frame parameters used for tooltips.",
    ),
    (
        "tooltip-functions",
        "Functions to call to display tooltips.",
    ),
    (
        "tooltip-help-message",
        "The last help message received via `show-help-function'.",
    ),
    (
        "tooltip-hide-delay",
        "Hide tooltips automatically after this many seconds.",
    ),
    (
        "tooltip-hide-time",
        "Time when the last tooltip was hidden.",
    ),
    (
        "tooltip-last-mouse-motion-event",
        "A copy of the last mouse motion event seen.",
    ),
    ("tooltip-mode", "Non-nil if Tooltip mode is enabled."),
    (
        "tooltip-mode-hook",
        "Hook run after entering or leaving `tooltip-mode'.",
    ),
    (
        "tooltip-previous-message",
        "The previous content of the echo area.",
    ),
    (
        "tooltip-recent-seconds",
        "Display tooltips if changing tip items within this many seconds.",
    ),
    (
        "tooltip-resize-echo-area",
        "If non-nil, using the echo area for tooltips will resize the echo area.",
    ),
    (
        "tooltip-short-delay",
        "Seconds to wait between subsequent tooltips on different items.",
    ),
    (
        "tooltip-timeout-id",
        "The id of the timeout started when Emacs becomes idle.",
    ),
    (
        "tooltip-x-offset",
        "X offset, in pixels, for the display of tooltips.",
    ),
    (
        "tooltip-y-offset",
        "Y offset, in pixels, for the display of tooltips.",
    ),
    (
        "touch-screen-aux-tool",
        "The ancillary tool being tracked, or nil.",
    ),
    (
        "touch-screen-current-timer",
        "Timer used to track long-presses.",
    ),
    (
        "touch-screen-current-tool",
        "The touch point currently being tracked, or nil.",
    ),
    (
        "touch-screen-delay",
        "Delay in seconds before Emacs considers a touch to be a long-press.",
    ),
    (
        "touch-screen-display-keyboard",
        "If non-nil, always display the on screen keyboard.",
    ),
    (
        "touch-screen-enable-hscroll",
        "If non-nil, hscroll can be changed from the touch screen.",
    ),
    (
        "touch-screen-events-received",
        "Whether a touch screen event has ever been translated.",
    ),
    (
        "touch-screen-extend-selection",
        "If non-nil, restart drag-to-select upon a tap on point or mark.",
    ),
    (
        "touch-screen-keyboard-function",
        "Function that decides whether to display the on screen keyboard.",
    ),
    (
        "touch-screen-precision-scroll",
        "Whether or not to use precision scrolling for touch screens.",
    ),
    (
        "touch-screen-preview-select",
        "If non-nil, display a preview while selecting text.",
    ),
    (
        "touch-screen-set-point-commands",
        "List of commands known to set the point.",
    ),
    (
        "touch-screen-translate-prompt",
        "Prompt given to the touch screen translation function.",
    ),
    (
        "touch-screen-word-select",
        "Whether or not to select whole words while dragging to select.",
    ),
    (
        "touch-screen-word-select-bounds",
        "The start and end positions of the word last selected.",
    ),
    (
        "touch-screen-word-select-initial-word",
        "The start and end positions of the first word to be selected.",
    ),
    (
        "trace-buffer",
        "Trace output will by default go to that buffer.",
    ),
    (
        "track-eol",
        "Non-nil means vertical motion starting at end of line keeps to ends of lines.",
    ),
    (
        "tramp-archive-compression-suffixes",
        "List of suffixes which indicate a compressed file.",
    ),
    (
        "tramp-archive-enabled",
        "Non-nil when file archive support is available.",
    ),
    (
        "tramp-archive-suffixes",
        "List of suffixes which indicate a file archive.",
    ),
    (
        "tramp-autoload-file-name-regexp",
        "Regular expression matching file names handled by Tramp autoload.",
    ),
    (
        "tramp-file-name-regexp",
        "Regular expression matching file names handled by Tramp.",
    ),
    (
        "tramp-foreign-file-name-handler-alist",
        "Alist of elements (FUNCTION . HANDLER) for foreign methods handled specially.",
    ),
    (
        "tramp-ignored-file-name-regexp",
        "Regular expression matching file names that are not under Tramp's control.",
    ),
    (
        "tramp-initial-file-name-regexp",
        "Value for `tramp-file-name-regexp' for autoload.",
    ),
    ("tramp-mode", "Whether Tramp is enabled."),
    (
        "transient-mark-mode-hook",
        "Hook run after entering or leaving `transient-mark-mode'.",
    ),
    (
        "transpose-sexps-function",
        "If non-nil, `transpose-sexps' delegates to this function.",
    ),
    (
        "trash-directory",
        "Directory for `move-file-to-trash' to move files and directories to.",
    ),
    (
        "tty-color-mode-alist",
        "An alist of supported standard tty color modes and their aliases.",
    ),
    (
        "tty-menu--initial-menu-x",
        "X coordinate of the first menu-bar menu dropped by F10.",
    ),
    (
        "tty-menu-navigation-map",
        "Keymap used while processing TTY menus.",
    ),
    (
        "tty-menu-open-use-tmm",
        "If non-nil, \\[menu-bar-open] on a TTY will invoke `tmm-menubar'.",
    ),
    (
        "tty-select-active-regions",
        "If non-nil, update PRIMARY window-system selection on text-mode frames.",
    ),
    (
        "tty-setup-hook",
        "Hook run after running the initialization function of a new text terminal.",
    ),
    (
        "tty-standard-colors",
        "An alist of 8 standard tty colors, their indices and RGB values.",
    ),
    (
        "tutorial-directory",
        "Directory containing the Emacs TUTORIAL files.",
    ),
    ("type-break-mode", "Non-nil if Type-Break mode is enabled."),
    (
        "ucs-names",
        "Hash table of cached CHAR-NAME keys to CHAR-CODE values.",
    ),
    (
        "uncomment-region-function",
        "Function to uncomment a region.",
    ),
    (
        "undelete-frame--deleted-frames",
        "Internal variable used by `undelete-frame--save-deleted-frame'.",
    ),
    (
        "undelete-frame-mode",
        "Non-nil if Undelete-Frame mode is enabled.",
    ),
    (
        "undelete-frame-mode-hook",
        "Hook run after entering or leaving `undelete-frame-mode'.",
    ),
    (
        "undo--combining-change-calls",
        "Non-nil when `combine-change-calls-1' is running.",
    ),
    (
        "undo-ask-before-discard",
        "If non-nil ask about discarding undo info for the current command.",
    ),
    (
        "undo-auto--last-boundary-cause",
        "Describe the cause of the last `undo-boundary'.",
    ),
    (
        "undo-auto--this-command-amalgamating",
        "Non-nil if `this-command' should be amalgamated.",
    ),
    (
        "undo-auto--undoably-changed-buffers",
        "List of buffers that have changed recently.",
    ),
    (
        "undo-auto-current-boundary-timer",
        "Current timer which will run `undo-auto--boundary-timer' or nil.",
    ),
    (
        "undo-equiv-table",
        "Table mapping redo records to the corresponding undo one.",
    ),
    (
        "undo-extra-outer-limit",
        "If non-nil, an extra level of size that's ok in an undo item.",
    ),
    ("undo-in-progress", "Non-nil while performing an undo."),
    (
        "undo-in-region",
        "Non-nil if `pending-undo-list' is not just a tail of `buffer-undo-list'.",
    ),
    (
        "undo-no-redo",
        "If t, `undo' doesn't go through redo entries.",
    ),
    (
        "undo-repeat-map",
        "Keymap to repeat `undo' commands.  Used in `repeat-mode'.",
    ),
    (
        "uniquify-after-kill-buffer-p",
        "If non-nil, rerationalize buffer names after a buffer has been killed.",
    ),
    (
        "uniquify-buffer-name-style",
        "How to construct unique buffer names for files with the same base name.",
    ),
    (
        "uniquify-dirname-transform",
        "Function to transform buffer's directory name when uniquifying buffer's name.",
    ),
    (
        "uniquify-ignore-buffers-re",
        "Regular expression matching buffer names that should not be uniquified.",
    ),
    (
        "uniquify-list-buffers-directory-modes",
        "List of modes for which uniquify should obey `list-buffers-directory'.",
    ),
    (
        "uniquify-managed",
        "Non-nil if the name of this buffer is managed by uniquify.",
    ),
    (
        "uniquify-min-dir-content",
        "Minimum number of directory name components included in buffer name.",
    ),
    (
        "uniquify-separator",
        "String separator for buffer name components.",
    ),
    (
        "uniquify-strip-common-suffix",
        "If non-nil, strip common directory suffixes of conflicting files.",
    ),
    (
        "uniquify-trailing-separator-p",
        "If non-nil, add a file name separator to Dired buffer names.",
    ),
    (
        "universal-argument-map",
        "Keymap used while processing \\[universal-argument].",
    ),
    (
        "untrusted-content",
        "Non-nil means that current buffer originated from an untrusted source.",
    ),
    (
        "update-leim-list-functions",
        "List of functions to call to update LEIM list file.",
    ),
    (
        "url-debug",
        "What types of debug messages from the URL library to show.",
    ),
    (
        "url-handler-mode",
        "Non-nil if Url-Handler mode is enabled.",
    ),
    (
        "url-ircs-default-port",
        "Default port for IRCS connections.",
    ),
    (
        "url-tramp-protocols",
        "List of URL protocols for which the work is handled by Tramp.",
    ),
    (
        "use-dialog-box-override",
        "Whether `use-dialog-box-p' should always return t.",
    ),
    (
        "use-empty-active-region",
        "Whether \"region-aware\" commands should act on empty regions.",
    ),
    (
        "use-hard-newlines",
        "Non-nil if Use-Hard-Newlines mode is enabled.",
    ),
    (
        "use-hard-newlines-hook",
        "Hook run after entering or leaving `use-hard-newlines'.",
    ),
    (
        "user-emacs-directory",
        "Directory beneath which additional per-user Emacs-specific files are placed.",
    ),
    (
        "user-emacs-directory-warning",
        "Non-nil means warn if unable to access or create `user-emacs-directory'.",
    ),
    (
        "user-mail-address",
        "The email address of the current user.",
    ),
    (
        "vc-before-checkin-hook",
        "Normal hook (list of functions) run before a commit or a file checkin.",
    ),
    (
        "vc-bzr-admin-checkout-format-file",
        "Name of the format file in a .bzr directory.",
    ),
    (
        "vc-bzr-admin-dirname",
        "Name of the directory containing Bzr repository status files.",
    ),
    (
        "vc-checkin-hook",
        "Normal hook (list of functions) run after commit or file checkin.",
    ),
    (
        "vc-checkout-hook",
        "Normal hook (list of functions) run after checking out a file.",
    ),
    (
        "vc-consult-headers",
        "If non-nil, identify work files by searching for version headers.",
    ),
    ("vc-dir-buffers", "List of `vc-dir' buffers."),
    (
        "vc-directory-exclusion-list",
        "List of directory names to be ignored when walking directory trees.",
    ),
    (
        "vc-display-status",
        "If non-nil, display revision number and lock status in mode line.",
    ),
    ("vc-file-prop-obarray", "Obarray for per-file properties."),
    (
        "vc-follow-symlinks",
        "What to do if visiting a symbolic link to a file under version control.",
    ),
    (
        "vc-handled-backends",
        "List of version control backends for which VC will be used.",
    ),
    (
        "vc-ignore-dir-regexp",
        "Regexp matching directory names that are not under VC's control.",
    ),
    (
        "vc-make-backup-files",
        "If non-nil, backups of registered files are made as with other files.",
    ),
    (
        "vc-rcs-master-templates",
        "Where to look for RCS master files.",
    ),
    (
        "vc-sccs-master-templates",
        "Where to look for SCCS master files.",
    ),
    (
        "vc-src-master-templates",
        "Where to look for SRC master files.",
    ),
    (
        "vc-use-short-revision",
        "If non-nil, VC backend functions should return short revisions if possible.",
    ),
    (
        "version-control",
        "Control use of version-numbered backup files.",
    ),
    (
        "version-regexp-alist",
        "Specify association between non-numeric version and its priority.",
    ),
    (
        "version-separator",
        "Specify the string used to separate the version elements.",
    ),
    ("view-mode", "Non-nil if View mode is enabled."),
    (
        "view-read-only",
        "Non-nil means buffers visiting files read-only do so in view mode.",
    ),
    (
        "vis-mode-saved-buffer-invisibility-spec",
        "Saved value of `buffer-invisibility-spec' when Visible mode is on.",
    ),
    ("visible-mode", "Non-nil if Visible mode is enabled."),
    (
        "visible-mode-hook",
        "Hook run after entering or leaving `visible-mode'.",
    ),
    (
        "visual-line-fringe-indicators",
        "How fringe indicators are shown for wrapped lines in `visual-line-mode'.",
    ),
    (
        "visual-line-mode",
        "Non-nil if Visual-Line mode is enabled.",
    ),
    (
        "visual-line-mode-hook",
        "Hook run after entering or leaving `visual-line-mode'.",
    ),
    (
        "visual-order-cursor-movement",
        "If non-nil, moving cursor with arrow keys follows the visual order.",
    ),
    (
        "warning-fill-prefix",
        "Non-nil means fill each warning text using this string as `fill-prefix'.",
    ),
    (
        "warning-prefix-function",
        "Function to generate warning prefixes.",
    ),
    (
        "warning-series",
        "Non-nil means treat multiple `display-warning' calls as a series.",
    ),
    (
        "warning-suppress-types",
        "List of warning types not to display immediately.",
    ),
    (
        "warning-type-format",
        "Format for displaying the warning type in the warning message.",
    ),
    (
        "what-cursor-show-names",
        "Whether to show character names in `what-cursor-position'.",
    ),
    (
        "which-function-mode",
        "Non-nil if Which-Function mode is enabled.",
    ),
    ("which-key-mode", "Non-nil if Which-Key mode is enabled."),
    (
        "widen-automatically",
        "Non-nil means it is ok for commands to call `widen' when they want to.",
    ),
    (
        "widget-keymap",
        "Keymap containing useful binding for buffers containing widgets.",
    ),
    ("windmove-mode", "Non-nil if Windmove mode is enabled."),
    (
        "window--sides-inhibit-check",
        "Non-nil means inhibit any checks on side windows.",
    ),
    (
        "window--sides-shown",
        "Non-nil if this buffer was shown in a side window once.",
    ),
    (
        "window-adjust-process-window-size-function",
        "Control how Emacs chooses inferior process window sizes.",
    ),
    (
        "window-area-factor",
        "Factor by which the window area should be over-estimated.",
    ),
    (
        "window-divider-default-bottom-width",
        "Default width of dividers on bottom of windows.",
    ),
    (
        "window-divider-default-places",
        "Default positions of window dividers.",
    ),
    (
        "window-divider-default-right-width",
        "Default width of dividers on the right of windows.",
    ),
    (
        "window-divider-mode",
        "Non-nil if Window-Divider mode is enabled.",
    ),
    (
        "window-divider-mode-hook",
        "Hook run after entering or leaving `window-divider-mode'.",
    ),
    (
        "window-min-height",
        "The minimum total height, in lines, of any window.",
    ),
    (
        "window-min-width",
        "The minimum total width, in columns, of any window.",
    ),
    ("window-prefix-map", "Keymap for subcommands of \\`C-x w'."),
    (
        "window-safe-min-height",
        "The absolute minimum number of lines of any window.",
    ),
    (
        "window-safe-min-width",
        "The absolute minimum number of columns of a window.",
    ),
    (
        "window-setup-hook",
        "Normal hook run after loading init files and handling the command line.",
    ),
    (
        "window-sides-reversed",
        "Whether top/bottom side windows appear in reverse order.",
    ),
    (
        "window-sides-slots",
        "Number of available side window slots on each side of a frame.",
    ),
    (
        "window-sides-vertical",
        "If non-nil, left and right side windows occupy full frame height.",
    ),
    (
        "window-size-fixed",
        "Non-nil in a buffer means windows displaying the buffer are fixed-size.",
    ),
    (
        "window-state-put-kept-windows",
        "Helper variable for `window-state-put'.",
    ),
    (
        "window-state-put-list",
        "Helper variable for `window-state-put'.",
    ),
    (
        "window-state-put-selected-window",
        "Helper variable for `window-state-put'.",
    ),
    (
        "window-state-put-stale-windows",
        "Helper variable for `window-state-put'.",
    ),
    (
        "window-system-default-frame-alist",
        "Window-system dependent default frame parameters.",
    ),
    ("winner-mode", "Non-nil if Winner mode is enabled."),
    (
        "with-timeout-timers",
        "List of all timers used by currently pending `with-timeout' calls.",
    ),
    (
        "woman-locale",
        "String specifying a manual page locale, or nil.",
    ),
    (
        "word-move-empty-char-table",
        "Used in `forward-word-strictly' and `backward-word-strictly'",
    ),
    (
        "write-contents-functions",
        "List of functions to be called before writing out a buffer to a file.",
    ),
    (
        "write-file-functions",
        "List of functions to be called before saving a buffer to a file.",
    ),
    (
        "x-alternatives-map",
        "Keymap of possible alternative meanings for some keys.",
    ),
    (
        "x-colors",
        "List of basic colors available on color displays.",
    ),
    (
        "x-display-cursor-at-start-of-preedit-string",
        "If non-nil, display the cursor at the start of any pre-edit text.",
    ),
    (
        "x-display-name",
        "The name of the window display on which Emacs was started.",
    ),
    (
        "x-dnd-click-count",
        "Alist of button numbers to click counters during drag-and-drop.",
    ),
    (
        "x-dnd-copy-types",
        "List of data types offered by programs that don't support `private'.",
    ),
    ("x-dnd-current-state", "The current state for a drop."),
    (
        "x-dnd-debug-errors",
        "Whether or not to signal protocol errors during drag-and-drop.",
    ),
    (
        "x-dnd-direct-save-function",
        "Function called when a file is dropped via XDS protocol.",
    ),
    (
        "x-dnd-known-types",
        "The types accepted by default for dropped data.",
    ),
    (
        "x-dnd-motif-message-types",
        "Mapping from numbers to Motif DND message types.",
    ),
    (
        "x-dnd-motif-to-action",
        "Mapping from number to operation for Motif DND.",
    ),
    (
        "x-dnd-offix-id-to-name",
        "Alist of OffiX data types to their names.",
    ),
    (
        "x-dnd-offix-old-kde-to-name",
        "Alist of old KDE data types to their names.",
    ),
    (
        "x-dnd-test-function",
        "Function to be used by drag-and-drop to determine whether to accept a drop.",
    ),
    (
        "x-dnd-types-alist",
        "Functions to call to handle drag-and-drop of known types.",
    ),
    (
        "x-dnd-use-offix-drop",
        "If non-nil, use the OffiX protocol to drop files and text.",
    ),
    (
        "x-dnd-xdnd-to-action",
        "Mapping from XDND action types to Lisp symbols.",
    ),
    (
        "x-dnd-xds-current-file",
        "The file name for which a direct save is currently being performed.",
    ),
    (
        "x-dnd-xds-performed",
        "Whether or not the drop target made a request for `XdndDirectSave0'.",
    ),
    (
        "x-dnd-xds-source-frame",
        "The frame from which a direct save is currently being performed.",
    ),
    (
        "x-dnd-xds-testing",
        "Whether or not XDS is being tested from ERT.",
    ),
    ("x-fixed-font-alist", "X fonts suitable for use in Emacs."),
    (
        "x-font-name-charset-alist",
        "This variable has no meaning starting with Emacs 22.1.",
    ),
    (
        "x-gtk-stock-map",
        "How icons for tool bars are mapped to Gtk+ stock items.",
    ),
    (
        "x-initialized",
        "Non-nil if the X window system has been initialized.",
    ),
    (
        "x-preedit-overlay",
        "The overlay currently used to display preedit text from a compose sequence.",
    ),
    (
        "x-select-request-type",
        "Data type request for X selection.",
    ),
    (
        "xargs-program",
        "The default xargs program for `grep-find-command'.",
    ),
    (
        "xterm-mouse-mode",
        "Non-nil if Xterm-Mouse mode is enabled.",
    ),
    (
        "y-or-n-p-history-variable",
        "History list symbol to add `y-or-n-p' answers to.",
    ),
    (
        "y-or-n-p-map",
        "Keymap that defines additional bindings for `y-or-n-p' answers.",
    ),
    (
        "y-or-n-p-use-read-key",
        "Use `read-key' when reading answers to \"y or n\" questions by `y-or-n-p'.",
    ),
    (
        "yank-excluded-properties",
        "Text properties to discard when yanking.",
    ),
    (
        "yank-from-kill-ring-rotate",
        "Whether using `yank-from-kill-ring' should rotate `kill-ring-yank-pointer'.",
    ),
    (
        "yank-handled-properties",
        "List of special text property handling conditions for yanking.",
    ),
    (
        "yank-menu-length",
        "Text of items in `yank-menu' longer than this will be truncated.",
    ),
    (
        "yank-menu-max-items",
        "Maximum number of entries to display in the `yank-menu'.",
    ),
    (
        "yank-pop-change-selection",
        "Whether rotating the kill ring changes the window system selection.",
    ),
    (
        "yank-transform-functions",
        "Hook run on strings to be yanked.",
    ),
    (
        "yank-undo-function",
        "If non-nil, function used by `yank-pop' to delete last stretch of yanked text.",
    ),
    ("kill-ring", "List of killed text sequences."),
    (
        "kill-ring-yank-pointer",
        "The tail of the kill ring whose car is the last thing yanked.",
    ),
];

fn startup_variable_doc_stub(sym: &str) -> Option<&'static str> {
    STARTUP_VARIABLE_DOC_STUBS
        .iter()
        .find_map(|(name, doc)| (*name == sym).then_some(*doc))
}

fn startup_variable_doc_string_symbol(sym: &str, prop: &str, value: &Value) -> bool {
    prop == "variable-documentation"
        && value.as_str().is_some()
        && STARTUP_VARIABLE_DOC_STRING_PROPERTIES
            .iter()
            .any(|(name, _)| *name == sym)
}

fn startup_doc_quote_style_display(doc: &str) -> String {
    let mut out = String::with_capacity(doc.len());
    let mut backtick_open = false;
    let mut escaped_backtick_open = false;
    let mut chars = doc.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.peek().copied() {
                Some('`') => {
                    chars.next();
                    escaped_backtick_open = true;
                    backtick_open = false;
                    continue;
                }
                Some('\'') if escaped_backtick_open => {
                    chars.next();
                    escaped_backtick_open = false;
                    continue;
                }
                _ => {
                    out.push(ch);
                    continue;
                }
            }
        }

        if escaped_backtick_open {
            if ch == '\'' {
                escaped_backtick_open = false;
            } else {
                out.push(ch);
            }
            continue;
        }

        match ch {
            '`' => {
                if backtick_open {
                    out.push('\u{2019}');
                    backtick_open = false;
                } else {
                    out.push('\u{2018}');
                    backtick_open = true;
                }
            }
            '\'' => {
                out.push('\u{2019}');
                if backtick_open {
                    backtick_open = false;
                }
            }
            _ => out.push(ch),
        }
    }

    out
}

fn startup_doc_quote_style_raw(doc: &str) -> String {
    doc.chars()
        .map(|ch| match ch {
            '\u{2018}' => '`',
            '\u{2019}' => '\'',
            _ => ch,
        })
        .collect()
}

/// `(documentation-property SYMBOL PROP &optional RAW)` -- return the
/// documentation property PROP of SYMBOL.
///
/// Context-aware implementation:
/// - validates SYMBOL as a symbol designator (`symbolp`)
/// - returns nil when PROP is not a symbol (matching Emacs `get`-like behavior)
/// - unresolved integer doc offsets return nil
/// - non-integer values are evaluated as Lisp and returned
/// - unless RAW is non-nil, string results are passed through
///   `substitute-command-keys`
pub(crate) fn builtin_documentation_property(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let raw = args.get(2).is_some_and(Value::is_truthy);
    let obarray = eval.obarray() as *const super::symbol::Obarray;
    // Safety: the evaluator owns the obarray for the duration of this call.
    let plan = documentation_property_plan(unsafe { &*obarray }, args)?;
    finish_documentation_result(
        execute_documentation_plan(plan, |value| eval.eval_value(&value))?,
        raw,
    )
}

fn documentation_property_plan(
    obarray: &super::symbol::Obarray,
    args: Vec<Value>,
) -> Result<DocumentationPlan, Flow> {
    expect_min_max_args("documentation-property", &args, 2, 3)?;
    let lisp_directory = obarray
        .symbol_value("lisp-directory")
        .and_then(Value::as_str_owned);

    let sym = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;

    let Some(prop) = args[1].as_symbol_name() else {
        return Ok(DocumentationPlan::Final(Value::Nil));
    };
    let raw = args.get(2).is_some_and(Value::is_truthy);

    match obarray.get_property(sym, prop).cloned() {
        Some(value) if startup_variable_doc_offset_symbol(sym, prop, &value) => {
            let base_doc = startup_variable_doc_stub(sym)
                .map(ToString::to_string)
                .unwrap_or_else(|| format!("{sym} is a variable defined in `C source code`."));
            let doc = if raw {
                startup_doc_quote_style_raw(&base_doc)
            } else {
                startup_doc_quote_style_display(&base_doc)
            };
            Ok(DocumentationPlan::Final(Value::string(doc)))
        }
        Some(value) if startup_variable_doc_string_symbol(sym, prop, &value) => {
            let text = value
                .as_str()
                .expect("startup string variable-documentation should be string");
            let doc = if raw {
                startup_doc_quote_style_raw(text)
            } else {
                startup_doc_quote_style_display(text)
            };
            Ok(DocumentationPlan::Final(Value::string(doc)))
        }
        Some(value) => documentation_plan_from_property_value(lisp_directory.as_deref(), value),
        _ => Ok(DocumentationPlan::Final(Value::Nil)),
    }
}

pub(crate) fn builtin_documentation_property_in_vm_runtime(
    shared: &mut super::eval::Context,
    vm_gc_roots: &[Value],
    args: Vec<Value>,
) -> EvalResult {
    let raw = args.get(2).is_some_and(Value::is_truthy);
    let args_roots = args.clone();
    let plan = documentation_property_plan(&shared.obarray, args)?;
    finish_documentation_result(
        execute_documentation_plan(plan, |value| {
            let mut extra_roots = args_roots.clone();
            extra_roots.push(value);
            shared.with_extra_gc_roots(vm_gc_roots, &extra_roots, move |eval| {
                eval.eval_value(&value)
            })
        })?,
        raw,
    )
}

// ---------------------------------------------------------------------------
// Pure builtins
// ---------------------------------------------------------------------------

/// `(Snarf-documentation FILENAME)` -- load documentation strings from
/// the internal DOC file.
///
/// Compatibility implementation: accepts the canonical `"DOC"` token and
/// preserves observed GNU Emacs error classes for invalid and missing paths.
/// It does not load or parse an on-disk DOC table yet.
fn snarf_doc_path_invalid(filename: &str) -> bool {
    if filename.is_empty() {
        return true;
    }

    let mut segments = filename
        .split('/')
        .filter(|segment| !segment.is_empty())
        .peekable();
    if segments.peek().is_none() {
        return true;
    }

    segments.all(|segment| segment == "." || segment == "..")
}

pub(crate) fn builtin_snarf_documentation(args: Vec<Value>) -> EvalResult {
    expect_args("Snarf-documentation", &args, 1)?;
    let filename = match args[0].as_str() {
        Some(name) => name,
        None => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), args[0]],
            ));
        }
    };

    // In batch compatibility mode, allow the canonical DOC token while
    // preserving observable error classes for invalid/missing names.
    if filename == "DOC" {
        return Ok(Value::Nil);
    }

    if filename.starts_with("DOC/") {
        return Err(signal(
            "file-error",
            vec![
                Value::string("Read error"),
                Value::string(format!("/usr/share/emacs/etc/{filename}")),
            ],
        ));
    }

    if snarf_doc_path_invalid(filename) {
        return Err(signal(
            "error",
            vec![Value::string("DOC file invalid at position 0")],
        ));
    }

    Err(signal(
        "file-missing",
        vec![
            Value::string("Opening doc string file"),
            Value::string("No such file or directory"),
            Value::string(format!("/usr/share/emacs/etc/{filename}")),
        ],
    ))
}

/// `(substitute-command-keys STRING)` -- process special documentation
/// sequences in STRING.
///
/// Recognized sequences:
/// - `\\[COMMAND]` — replaced with the key binding for COMMAND (stripped here)
/// - `\\{KEYMAP}` — replaced with a description of the keymap (stripped here)
/// - `\\<KEYMAP>` — sets the keymap for subsequent `\\[...]` (stripped here)
/// - `\\=` — quote the next character (prevents interpretation)
///
/// This implementation strips the special sequences, returning the plain
/// text content.  A full implementation would resolve key bindings and
/// format keymap descriptions.
pub(crate) fn builtin_substitute_command_keys(args: Vec<Value>) -> EvalResult {
    expect_args("substitute-command-keys", &args, 1)?;

    let input = match args[0].as_str() {
        Some(s) => s,
        None => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), args[0]],
            ));
        }
    };

    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.peek() {
                Some('[') => {
                    // \\[COMMAND] — skip until closing ']'
                    chars.next(); // consume '['
                    let mut command = String::new();
                    for c in chars.by_ref() {
                        if c == ']' {
                            break;
                        }
                        command.push(c);
                    }
                    // Replace with a placeholder showing the command name.
                    result.push_str(&format!("M-x {}", command));
                }
                Some('{') => {
                    // \\{KEYMAP} — skip until closing '}'
                    chars.next(); // consume '{'
                    for c in chars.by_ref() {
                        if c == '}' {
                            break;
                        }
                    }
                    // Omit keymap description entirely.
                }
                Some('<') => {
                    // \\<KEYMAP> — skip until closing '>'
                    chars.next(); // consume '<'
                    for c in chars.by_ref() {
                        if c == '>' {
                            break;
                        }
                    }
                    // Silently consumed (sets keymap context).
                }
                Some('=') => {
                    // \\= — quote next character literally.
                    chars.next(); // consume '='
                    if let Some(next) = chars.next() {
                        result.push(next);
                    }
                }
                Some('\\') => {
                    // Literal backslash (\\\\).
                    chars.next();
                    result.push('\\');
                }
                _ => {
                    // Not a recognized sequence; keep the backslash.
                    result.push(ch);
                }
            }
        } else {
            result.push(ch);
        }
    }

    Ok(Value::string(result))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "doc_test.rs"]
mod tests;
