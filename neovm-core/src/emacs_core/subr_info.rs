//! Subr/primitive introspection builtins.
//!
//! Provides type predicates and introspection for callable objects:
//! - `subrp`, `subr-name`, `subr-arity`
//! - `commandp`, `functionp`, `byte-code-function-p`, `closurep`
//! - `interpreted-function-p`, `special-form-p`, `macrop`
//! - `func-arity`, `indirect-function`

use super::error::{EvalResult, Flow, signal};
use super::intern::{intern, lookup_interned, resolve_sym};
use super::value::*;

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

// ---------------------------------------------------------------------------
// Evaluator/public callable classification
// ---------------------------------------------------------------------------

/// Returns true if `name` is recognized by the evaluator's special-form
/// dispatch path.
///
/// This list mirrors `Evaluator::try_special_form()` in `eval.rs`.
pub(crate) fn is_evaluator_special_form_name(name: &str) -> bool {
    matches!(
        name,
        "quote"
            | "function"
            | "let"
            | "let*"
            | "setq"
            | "setq-local"
            | "if"
            | "and"
            | "or"
            | "cond"
            | "while"
            | "progn"
            | "prog1"
            | "lambda"
            | "defun"
            | "defvar"
            | "defconst"
            | "defmacro"
            | "funcall"
            | "catch"
            | "throw"
            | "unwind-protect"
            | "condition-case"
            | "interactive"
            | "declare"
            | "when"
            | "unless"
            | "bound-and-true-p"
            | "defalias"
            | "provide"
            | "require"
            | "save-excursion"
            | "save-window-excursion"
            | "save-selected-window"

            | "save-restriction"

            | "with-local-quit"
            | "with-temp-message"
            | "with-demoted-errors"
            | "with-current-buffer"
            | "ignore-errors"
            | "dotimes"
            | "dolist"
            // Custom / defcustom
            | "defcustom"
            | "defgroup"
            | "setq-default"
            | "defvar-local"
            // Autoload
            | "autoload"
            | "eval-when-compile"
            | "eval-and-compile"
            // Error hierarchy
            | "define-error"
            // Reader/printer
            | "with-output-to-string"
            // Threading
            | "with-mutex"
            // Misc
            | "with-temp-buffer"
            | "save-current-buffer"
            | "track-mouse"
            | "with-syntax-table"
            // Mode definition
            | "define-minor-mode"
            | "define-derived-mode"
            | "define-generic-mode"
    )
}

/// Returns true for special forms exposed by `special-form-p`.
///
/// Emacs distinguishes evaluator internals from public special forms:
/// many evaluator-recognized constructs are macros/functions in user-visible
/// introspection.
fn is_public_special_form_name(name: &str) -> bool {
    matches!(
        name,
        "quote"
            | "function"
            | "let"
            | "let*"
            | "setq"
            | "if"
            | "and"
            | "or"
            | "cond"
            | "while"
            | "progn"
            | "prog1"
            | "defvar"
            | "defconst"
            | "catch"
            | "unwind-protect"
            | "condition-case"
            | "interactive"
            | "save-excursion"
            | "save-restriction"
            | "save-current-buffer"
    )
}

pub(crate) fn is_special_form(name: &str) -> bool {
    is_public_special_form_name(name)
}

/// Returns true for evaluator special forms that should NOT be expanded
/// by `macroexpand`.  These are forms where NeoVM has a Rust handler that
/// conflicts with the Elisp macro definition (e.g. `pcase.el` defines
/// `(defmacro pcase ...)` but NeoVM handles `pcase` directly in Rust).
///
/// This is distinct from fallback macros like `when`/`unless`/`pcase-let`
/// which ARE intentionally expanded by macroexpand.
pub(crate) fn is_evaluator_sf_skip_macroexpand(name: &str) -> bool {
    // NOTE: pcase-let, pcase-let*, pcase-dolist are NOT here because
    // they have fallback macro handlers in macroexpand_known_fallback_macro.
    matches!(
        name,
        "define-minor-mode" | "define-derived-mode" | "define-generic-mode"
    )
}

pub(crate) fn is_evaluator_macro_name(name: &str) -> bool {
    let is_macro = has_fallback_macro(name) || name == "declare";
    debug_assert!(!is_macro || is_evaluator_special_form_name(name));
    is_macro
}

pub(crate) fn is_evaluator_callable_name(name: &str) -> bool {
    // These are evaluator-dispatched entries that still behave as normal
    // callable symbols in introspection (`fboundp`/`functionp`/`symbol-function`).
    matches!(name, "throw")
}

#[derive(Clone, Copy)]
struct FallbackMacroSpec {
    min: usize,
    max: Option<usize>,
}

fn fallback_macro_spec(name: &str) -> Option<FallbackMacroSpec> {
    match name {
        "when" | "unless" | "dotimes" | "dolist" | "with-mutex" => {
            Some(FallbackMacroSpec { min: 1, max: None })
        }
        "with-current-buffer" | "with-syntax-table" => {
            Some(FallbackMacroSpec { min: 1, max: None })
        }
        "ignore-errors"
        | "setq-local"
        | "with-temp-buffer"
        | "with-output-to-string"
        | "track-mouse"
        | "save-window-excursion"
        | "save-selected-window"
        | "with-local-quit"
        | "declare"
        | "eval-when-compile"
        | "eval-and-compile" => Some(FallbackMacroSpec { min: 0, max: None }),
        "with-temp-message" => Some(FallbackMacroSpec { min: 1, max: None }),
        "with-demoted-errors" => Some(FallbackMacroSpec { min: 1, max: None }),
        "bound-and-true-p" => Some(FallbackMacroSpec {
            min: 1,
            max: Some(1),
        }),
        "defvar-local" => Some(FallbackMacroSpec {
            min: 2,
            max: Some(3),
        }),
        _ => None,
    }
}

pub(crate) fn has_fallback_macro(name: &str) -> bool {
    fallback_macro_spec(name).is_some()
}

fn fallback_macro_params(spec: FallbackMacroSpec) -> LambdaParams {
    let required = (0..spec.min)
        .map(|idx| intern(&format!("arg{idx}")))
        .collect();
    let (optional, rest) = match spec.max {
        None => (Vec::new(), Some(intern("rest"))),
        Some(max) => {
            debug_assert!(max >= spec.min);
            let optional_count = max.saturating_sub(spec.min);
            let optional = (0..optional_count)
                .map(|idx| intern(&format!("arg{}", spec.min + idx)))
                .collect();
            (optional, None)
        }
    };

    LambdaParams {
        required,
        optional,
        rest,
    }
}

/// Return a placeholder macro object for evaluator-integrated macro names.
///
/// This keeps `fboundp`/`symbol-function`/`indirect-function`/`macrop`
/// introspection aligned with Emacs for core macros even when they are not
/// materialized via Elisp bootstrap code in the function cell.
pub(crate) fn fallback_macro_value(name: &str) -> Option<Value> {
    let spec = fallback_macro_spec(name)?;
    Some(Value::make_macro(LambdaData {
        params: fallback_macro_params(spec),
        body: vec![].into(),
        env: None,
        docstring: None,
        doc_form: None,
    }))
}

// ---------------------------------------------------------------------------
// Arity helpers
// ---------------------------------------------------------------------------

/// Build a cons cell `(MIN . MAX)` representing arity.
/// `max` of `None` means "many" (unbounded &rest), represented by the
/// symbol `many`.
fn arity_cons(min: usize, max: Option<usize>) -> Value {
    let min_val = Value::Int(min as i64);
    let max_val = match max {
        Some(n) => Value::Int(n as i64),
        None => Value::symbol("many"),
    };
    Value::cons(min_val, max_val)
}

fn arity_unevalled(min: usize) -> Value {
    Value::cons(Value::Int(min as i64), Value::symbol("unevalled"))
}

fn is_cxr_subr_name(name: &str) -> bool {
    let Some(inner) = name.strip_prefix('c').and_then(|s| s.strip_suffix('r')) else {
        return false;
    };
    !inner.is_empty() && inner.chars().all(|ch| ch == 'a' || ch == 'd')
}

fn subr_arity_value(name: &str) -> Value {
    match name {
        // Oracle-compatible overrides for core subrs used in vm-compat.
        n if is_cxr_subr_name(n) => arity_cons(1, Some(1)),
        "message"
        | "message-box"
        | "message-or-box"
        | "format"
        | "format-message"
        | "/"
        | "<"
        | "<="
        | "="
        | ">"
        | ">="
        | "apply"
        | "funcall"
        | "funcall-interactively" => arity_cons(1, None),
        "funcall-with-delayed-message" => arity_cons(3, Some(3)),
        "cons" => arity_cons(2, Some(2)),
        "1+" | "1-" | "abs" => arity_cons(1, Some(1)),
        "%" | "/=" | "ash" | "length<" | "length=" | "length>" => arity_cons(2, Some(2)),
        "copy-alist" | "copy-hash-table" | "copy-keymap" | "copy-sequence" => {
            arity_cons(1, Some(1))
        }
        "copy-marker" => arity_cons(0, Some(2)),
        "copy-category-table" => arity_cons(0, Some(1)),
        "copy-syntax-table" => arity_cons(0, Some(1)),
        "copy-file" => arity_cons(2, Some(6)),
        "make-directory-internal" => arity_cons(1, Some(1)),
        "make-temp-name" => arity_cons(1, Some(1)),
        "make-symbolic-link" | "rename-file" => arity_cons(2, Some(3)),
        "file-name-absolute-p"
        | "file-name-as-directory"
        | "file-name-directory"
        | "file-name-nondirectory"
        | "file-name-case-insensitive-p" => arity_cons(1, Some(1)),
        "file-name-all-completions" => arity_cons(2, Some(2)),
        "file-name-completion" => arity_cons(2, Some(3)),
        "file-name-concat" => arity_cons(1, None),
        "get-truename-buffer" | "unhandled-file-name-directory" => arity_cons(1, Some(1)),
        "find-buffer" => arity_cons(2, Some(2)),
        "find-file-name-handler" => arity_cons(2, Some(2)),
        "insert-file-contents" => arity_cons(1, Some(5)),
        "load" => arity_cons(1, Some(5)),
        "provide" => arity_cons(1, Some(2)),
        "require" => arity_cons(1, Some(3)),
        "locate-file-internal" => arity_cons(2, Some(4)),
        "file-attributes" | "file-modes" => arity_cons(1, Some(2)),
        "file-accessible-directory-p"
        | "file-acl"
        | "file-directory-p"
        | "file-executable-p"
        | "file-exists-p"
        | "file-locked-p"
        | "file-readable-p"
        | "file-regular-p"
        | "file-selinux-context"
        | "file-system-info"
        | "file-symlink-p"
        | "file-writable-p" => arity_cons(1, Some(1)),
        "file-newer-than-file-p" => arity_cons(2, Some(2)),
        "goto-char" => arity_cons(1, Some(1)),
        "beginning-of-line" | "end-of-line" | "forward-char" | "backward-char" | "forward-word"
        | "forward-line" => arity_cons(0, Some(1)),
        "current-buffer" | "buffer-string" | "point" | "point-max" | "point-min" | "bobp"
        | "eobp" | "bolp" | "eolp" | "erase-buffer" | "widen" => arity_cons(0, Some(0)),
        "barf-if-buffer-read-only" => arity_cons(0, Some(1)),
        "mark-marker" | "point-marker" | "point-max-marker" | "point-min-marker" => {
            arity_cons(0, Some(0))
        }
        "file-group-gid" | "group-gid" | "group-real-gid" | "last-nonminibuffer-frame" => {
            arity_cons(0, Some(0))
        }
        "group-name" => arity_cons(1, Some(1)),
        "following-char" | "garbage-collect" | "get-load-suffixes" | "byteorder" => {
            arity_cons(0, Some(0))
        }
        "buffer-file-name"
        | "buffer-base-buffer"
        | "buffer-last-name"
        | "buffer-name"
        | "buffer-size"
        | "buffer-chars-modified-tick"
        | "buffer-modified-p"
        | "buffer-modified-tick"
        | "buffer-list"
        | "buffer-enable-undo"
        | "buffer-hash"
        | "buffer-local-variables"
        | "buffer-line-statistics" => arity_cons(0, Some(1)),
        "other-buffer" => arity_cons(0, Some(3)),
        "bury-buffer-internal" => arity_cons(1, Some(1)),
        "marker-buffer" | "marker-insertion-type" | "marker-position" | "markerp" => {
            arity_cons(1, Some(1))
        }
        "get-byte" => arity_cons(0, Some(2)),
        "get-buffer" | "get-file-buffer" => arity_cons(1, Some(1)),
        "get-buffer-create" | "generate-new-buffer-name" => arity_cons(1, Some(2)),
        "buffer-live-p" | "buffer-swap-text" => arity_cons(1, Some(1)),
        "buffer-local-value" | "buffer-substring" | "buffer-substring-no-properties" => {
            arity_cons(2, Some(2))
        }
        "char-after"
        | "char-before"
        | "charset-after"
        | "charset-id-internal"
        | "charset-priority-list" => arity_cons(0, Some(1)),
        "char-category-set"
        | "char-or-string-p"
        | "char-resolve-modifiers"
        | "char-syntax"
        | "char-width"
        | "char-table-p"
        | "char-table-parent"
        | "char-table-subtype"
        | "charset-plist"
        | "charsetp"
        | "closurep"
        | "default-boundp"
        | "default-value"
        | "default-toplevel-value"
        | "decode-big5-char"
        | "decode-sjis-char"
        | "encode-big5-char"
        | "encode-sjis-char"
        | "native-comp-function-p" => arity_cons(1, Some(1)),
        "char-charset" => arity_cons(1, Some(2)),
        "char-table-extra-slot"
        | "char-table-range"
        | "define-charset-alias"
        | "get-unused-iso-final-char" => arity_cons(2, Some(2)),
        "declare-equiv-charset" => arity_cons(4, Some(4)),
        "decode-char" => arity_cons(2, Some(2)),
        "fceiling" | "ffloor" | "frexp" | "fround" | "framep" | "ftruncate" => {
            arity_cons(1, Some(1))
        }
        "ldexp" => arity_cons(2, Some(2)),
        "sqrt" | "sin" | "cos" | "tan" | "asin" | "acos" | "exp" | "isnan" => {
            arity_cons(1, Some(1))
        }
        "atan" | "log" => arity_cons(1, Some(2)),
        "expt" => arity_cons(2, Some(2)),
        "random" => arity_cons(0, Some(1)),
        "logb" | "lognot" => arity_cons(1, Some(1)),
        "bignump"
        | "boundp"
        | "byte-code-function-p"
        | "car-safe"
        | "cdr-safe"
        | "integer-or-marker-p"
        | "bare-symbol"
        | "bare-symbol-p" => arity_cons(1, Some(1)),
        "identity"
        | "length"
        | "interpreted-function-p"
        | "invisible-p"
        | "functionp"
        | "prefix-numeric-value" => arity_cons(1, Some(1)),
        "fboundp" | "func-arity" | "symbol-function" | "symbol-value" | "fmakunbound"
        | "makunbound" => arity_cons(1, Some(1)),
        "make-symbol" | "symbol-name" | "symbol-plist" => arity_cons(1, Some(1)),
        "intern" | "intern-soft" | "indirect-function" | "unintern" => arity_cons(1, Some(2)),
        "fset" | "set" | "get" | "set-marker-insertion-type" => arity_cons(2, Some(2)),
        "defalias" => arity_cons(2, Some(3)),
        "put" => arity_cons(3, Some(3)),
        "set-marker" => arity_cons(2, Some(3)),
        "increment-register" | "register-ccl-program" | "register-code-conversion-map" => {
            arity_cons(2, Some(2))
        }
        "insert-byte" => arity_cons(2, Some(3)),
        "insert-char" => arity_cons(1, Some(3)),
        "hash-table-p"
        | "clrhash"
        | "hash-table-count"
        | "internal--hash-table-buckets"
        | "internal--hash-table-histogram"
        | "internal--hash-table-index-size"
        | "sxhash-eq"
        | "sxhash-eql"
        | "sxhash-equal"
        | "sxhash-equal-including-properties" => arity_cons(1, Some(1)),
        "gethash" => arity_cons(2, Some(3)),
        "puthash" => arity_cons(3, Some(3)),
        "remhash" | "maphash" => arity_cons(2, Some(2)),
        "hash-table-test"
        | "hash-table-size"
        | "hash-table-rehash-size"
        | "hash-table-rehash-threshold"
        | "hash-table-weakness" => arity_cons(1, Some(1)),
        "max" | "min" => arity_cons(1, None),
        "assq" | "car-less-than-car" | "member" | "memq" | "memql" | "rassoc" | "rassq" => {
            arity_cons(2, Some(2))
        }
        "mod" | "make-list" | "mapc" | "mapcan" | "mapcar" | "nth" | "nthcdr" => {
            arity_cons(2, Some(2))
        }
        "delete" | "delq" | "elt" => arity_cons(2, Some(2)),
        "mapconcat" => arity_cons(2, Some(3)),
        "assoc" | "assoc-string" => arity_cons(2, Some(3)),
        "nconc" => arity_cons(0, None),
        "nreverse" | "proper-list-p" | "reverse" | "safe-length" => arity_cons(1, Some(1)),
        "backward-prefix-chars" => arity_cons(0, Some(0)),
        "backward-kill-word"
        | "capitalize"
        | "capitalize-word"
        | "downcase-word"
        | "kill-local-variable" => arity_cons(1, Some(1)),
        "terpri" => arity_cons(0, Some(2)),
        "local-variable-p" => arity_cons(1, Some(2)),
        "locale-info" => arity_cons(1, Some(1)),
        "max-char" => arity_cons(0, Some(1)),
        "memory-use-counts" | "make-marker" => arity_cons(0, Some(0)),
        "make-local-variable" | "make-variable-buffer-local" => arity_cons(1, Some(1)),
        "mapatoms" => arity_cons(1, Some(2)),
        "map-char-table" => arity_cons(2, Some(2)),
        "capitalize-region" => arity_cons(2, Some(3)),
        "downcase-region" => arity_cons(2, Some(3)),
        "kill-buffer" => arity_cons(0, Some(1)),
        "add-name-to-file" => arity_cons(2, Some(3)),
        "add-face-text-property" => arity_cons(3, Some(5)),
        "add-text-properties" | "set-text-properties" => arity_cons(3, Some(4)),
        "put-text-property" | "text-property-any" | "text-property-not-all" => {
            arity_cons(4, Some(5))
        }
        "remove-text-properties" | "remove-list-of-text-properties" | "move-overlay" => {
            arity_cons(3, Some(4))
        }
        "get-text-property" | "get-char-property" | "get-pos-property" => arity_cons(2, Some(3)),
        "get-char-property-and-overlay" => arity_cons(2, Some(3)),
        "get-display-property" => arity_cons(2, Some(4)),
        "text-properties-at" | "overlays-at" => arity_cons(1, Some(2)),
        "next-single-property-change"
        | "next-single-char-property-change"
        | "previous-single-property-change"
        | "previous-single-char-property-change" => arity_cons(2, Some(4)),
        "next-property-change" | "previous-property-change" => arity_cons(1, Some(3)),
        "next-char-property-change" => arity_cons(1, Some(2)),
        "previous-char-property-change" => arity_cons(1, Some(2)),
        "make-overlay" => arity_cons(2, Some(5)),
        "overlay-put" => arity_cons(3, Some(3)),
        "overlay-get" | "overlays-in" => arity_cons(2, Some(2)),
        "overlay-start" | "overlay-end" | "overlay-buffer" | "overlay-properties" | "overlayp" => {
            arity_cons(1, Some(1))
        }
        "next-overlay-change" | "previous-overlay-change" => arity_cons(1, Some(1)),
        "prin1" | "prin1-to-string" => arity_cons(1, Some(3)),
        "princ" | "print" => arity_cons(1, Some(2)),
        "propertize" => arity_cons(1, None),
        "face-attribute-relative-p" | "font-get" | "internal-merge-in-global-face" => {
            arity_cons(2, Some(2))
        }
        "face-font" | "font-xlfd-name" => arity_cons(1, Some(3)),
        "close-font" => arity_cons(1, Some(2)),
        "face-id"
        | "fontp"
        | "internal-make-lisp-face"
        | "internal-lisp-face-empty-p"
        | "internal-lisp-face-p" => arity_cons(1, Some(2)),
        "font-put" => arity_cons(3, Some(3)),
        "internal-copy-lisp-face" => arity_cons(4, Some(4)),
        "internal-face-x-get-resource"
        | "internal-get-lisp-face-attribute"
        | "internal-lisp-face-equal-p" => arity_cons(2, Some(3)),
        "internal-lisp-face-attribute-values"
        | "internal-set-alternative-font-family-alist"
        | "internal-set-alternative-font-registry-alist"
        | "internal-set-font-selection-order" => arity_cons(1, Some(1)),
        "internal-set-lisp-face-attribute" => arity_cons(3, Some(4)),
        "internal--define-uninitialized-variable" => arity_cons(1, Some(2)),
        "internal--labeled-narrow-to-region" => arity_cons(3, Some(3)),
        "internal--labeled-widen" => arity_cons(1, Some(1)),
        "internal--obarray-buckets" => arity_cons(1, Some(1)),
        "internal--set-buffer-modified-tick" => arity_cons(1, Some(2)),
        "internal--track-mouse" => arity_cons(1, Some(1)),
        "internal-char-font" => arity_cons(1, Some(2)),
        "internal-complete-buffer" => arity_cons(3, Some(3)),
        "internal-describe-syntax-value" => arity_cons(1, Some(1)),
        "internal-event-symbol-parse-modifiers" => arity_cons(1, Some(1)),
        "internal-handle-focus-in" => arity_cons(1, Some(1)),
        "internal-make-var-non-special" => arity_cons(1, Some(1)),
        "internal-set-lisp-face-attribute-from-resource" => arity_cons(3, Some(4)),
        "internal-stack-stats" => arity_cons(0, Some(0)),
        "internal-subr-documentation" => arity_cons(1, Some(1)),
        "dump-emacs-portable" => arity_cons(1, Some(2)),
        "dump-emacs-portable--sort-predicate" => arity_cons(2, Some(2)),
        "dump-emacs-portable--sort-predicate-copied" => arity_cons(2, Some(2)),
        "malloc-info" => arity_cons(0, Some(0)),
        "malloc-trim" => arity_cons(0, Some(1)),
        "marker-last-position" => arity_cons(1, Some(1)),
        "match-data--translate" => arity_cons(1, Some(1)),
        "memory-info" => arity_cons(0, Some(0)),
        "make-frame-invisible" => arity_cons(0, Some(2)),
        "make-terminal-frame" => arity_cons(1, Some(1)),
        "menu-bar-menu-at-x-y" => arity_cons(2, Some(3)),
        "menu-or-popup-active-p" => arity_cons(0, Some(0)),
        "module-load" => arity_cons(1, Some(1)),
        "mouse-pixel-position" => arity_cons(0, Some(0)),
        "mouse-position" => arity_cons(0, Some(0)),
        "newline-cache-check" => arity_cons(0, Some(1)),
        "native-comp-available-p" => arity_cons(0, Some(0)),
        "native-comp-unit-file" => arity_cons(1, Some(1)),
        "native-comp-unit-set-file" => arity_cons(2, Some(2)),
        "native-elisp-load" => arity_cons(1, Some(2)),
        "new-fontset" => arity_cons(2, Some(2)),
        "object-intervals" => arity_cons(1, Some(1)),
        "old-selected-frame" => arity_cons(0, Some(0)),
        "old-selected-window" => arity_cons(0, Some(0)),
        "open-dribble-file" => arity_cons(1, Some(1)),
        "open-font" => arity_cons(1, Some(3)),
        "optimize-char-table" => arity_cons(1, Some(2)),
        "overlay-lists" => arity_cons(0, Some(0)),
        "overlay-recenter" => arity_cons(1, Some(1)),
        "pdumper-stats" => arity_cons(0, Some(0)),
        "play-sound-internal" => arity_cons(1, Some(1)),
        "position-symbol" => arity_cons(2, Some(2)),
        "posn-at-point" => arity_cons(0, Some(2)),
        "posn-at-x-y" => arity_cons(2, Some(4)),
        "profiler-cpu-log" => arity_cons(0, Some(0)),
        "profiler-cpu-running-p" => arity_cons(0, Some(0)),
        "profiler-cpu-start" => arity_cons(1, Some(1)),
        "profiler-cpu-stop" => arity_cons(0, Some(0)),
        "profiler-memory-log" => arity_cons(0, Some(0)),
        "profiler-memory-running-p" => arity_cons(0, Some(0)),
        "profiler-memory-start" => arity_cons(0, Some(0)),
        "profiler-memory-stop" => arity_cons(0, Some(0)),
        "query-font" => arity_cons(1, Some(1)),
        "query-fontset" => arity_cons(1, Some(2)),
        "read-positioning-symbols" => arity_cons(0, Some(1)),
        "recent-auto-save-p" => arity_cons(0, Some(0)),
        "record" => arity_cons(1, None),
        "recordp" => arity_cons(1, Some(1)),
        "reconsider-frame-fonts" => arity_cons(1, Some(1)),
        "redirect-debugging-output" => arity_cons(1, Some(2)),
        "redirect-frame-focus" => arity_cons(1, Some(2)),
        "remove-pos-from-symbol" => arity_cons(1, Some(1)),
        "resize-mini-window-internal" => arity_cons(1, Some(1)),
        "restore-buffer-modified-p" => arity_cons(1, Some(1)),
        "set--this-command-keys" => arity_cons(1, Some(1)),
        "set-buffer-auto-saved" => arity_cons(0, Some(0)),
        "set-buffer-redisplay" => arity_cons(4, Some(4)),
        "set-charset-plist" => arity_cons(2, Some(2)),
        "set-fontset-font" => arity_cons(3, Some(5)),
        "set-frame-selected-window" => arity_cons(2, Some(3)),
        "set-frame-window-state-change" => arity_cons(0, Some(2)),
        "set-fringe-bitmap-face" => arity_cons(1, Some(2)),
        "set-minibuffer-window" => arity_cons(1, Some(1)),
        "set-mouse-pixel-position" => arity_cons(3, Some(3)),
        "set-mouse-position" => arity_cons(3, Some(3)),
        "set-window-combination-limit" => arity_cons(2, Some(2)),
        "set-window-new-normal" => arity_cons(1, Some(2)),
        "set-window-new-pixel" => arity_cons(2, Some(3)),
        "set-window-new-total" => arity_cons(2, Some(3)),
        "sort-charsets" => arity_cons(1, Some(1)),
        "split-char" => arity_cons(1, Some(1)),
        "string-distance" => arity_cons(2, Some(3)),
        "subst-char-in-region" => arity_cons(4, Some(5)),
        "subr-native-comp-unit" => arity_cons(1, Some(1)),
        "subr-native-lambda-list" => arity_cons(1, Some(1)),
        "subr-type" => arity_cons(1, Some(1)),
        "this-single-command-keys" => arity_cons(0, Some(0)),
        "this-single-command-raw-keys" => arity_cons(0, Some(0)),
        "thread--blocker" => arity_cons(1, Some(1)),
        "tool-bar-get-system-style" => arity_cons(0, Some(0)),
        "tool-bar-pixel-width" => arity_cons(0, Some(1)),
        "translate-region-internal" => arity_cons(3, Some(3)),
        "transpose-regions" => arity_cons(4, Some(5)),
        "tty--output-buffer-size" => arity_cons(0, Some(1)),
        "tty--set-output-buffer-size" => arity_cons(1, Some(2)),
        "tty-suppress-bold-inverse-default-colors" => arity_cons(1, Some(1)),
        "unencodable-char-position" => arity_cons(3, Some(5)),
        "unicode-property-table-internal" => arity_cons(1, Some(1)),
        "unify-charset" => arity_cons(1, Some(3)),
        "unix-sync" => arity_cons(0, Some(0)),
        "value<" => arity_cons(2, Some(2)),
        "variable-binding-locus" => arity_cons(1, Some(1)),
        "byte-code" => arity_cons(3, Some(3)),
        "decode-coding-region" => arity_cons(3, Some(4)),
        "defconst-1" => arity_cons(2, Some(3)),
        "define-coding-system-internal" => arity_cons(13, None),
        "defvar-1" => arity_cons(2, Some(3)),
        "defvaralias" => arity_cons(2, Some(3)),
        "encode-coding-region" => arity_cons(3, Some(4)),
        "find-operation-coding-system" => arity_cons(1, None),
        "handler-bind-1" => arity_cons(1, None),
        "indirect-variable" => arity_cons(1, Some(1)),
        "insert-and-inherit" => arity_cons(0, None),
        "insert-before-markers-and-inherit" => arity_cons(0, None),
        "insert-buffer-substring" => arity_cons(1, Some(3)),
        "iso-charset" => arity_cons(3, Some(3)),
        "keymap--get-keyelt" => arity_cons(2, Some(2)),
        "keymap-prompt" => arity_cons(1, Some(1)),
        "kill-all-local-variables" => arity_cons(0, Some(1)),
        "kill-emacs" => arity_cons(0, Some(2)),
        "lower-frame" => arity_cons(0, Some(1)),
        "lread--substitute-object-in-subtree" => arity_cons(3, Some(3)),
        "macroexpand" => arity_cons(1, Some(2)),
        "make-byte-code" => arity_cons(4, None),
        "make-char" => arity_cons(1, Some(5)),
        "make-closure" => arity_cons(1, None),
        "make-finalizer" => arity_cons(1, Some(1)),
        "make-indirect-buffer" => arity_cons(2, Some(4)),
        "make-interpreted-closure" => arity_cons(3, Some(5)),
        "make-record" => arity_cons(3, Some(3)),
        "make-temp-file-internal" => arity_cons(4, Some(4)),
        "map-charset-chars" => arity_cons(2, Some(5)),
        "map-keymap" => arity_cons(2, Some(3)),
        "map-keymap-internal" => arity_cons(2, Some(2)),
        "mapbacktrace" => arity_cons(1, Some(2)),
        "minibuffer-innermost-command-loop-p" => arity_cons(0, Some(1)),
        "minibuffer-prompt-end" => arity_cons(0, Some(0)),
        "next-frame" => arity_cons(0, Some(2)),
        "ntake" => arity_cons(2, Some(2)),
        "obarray-clear" => arity_cons(1, Some(1)),
        "obarray-make" => arity_cons(0, Some(1)),
        "previous-frame" => arity_cons(0, Some(2)),
        "put-unicode-property-internal" => arity_cons(3, Some(3)),
        "raise-frame" => arity_cons(0, Some(1)),
        "re--describe-compiled" => arity_cons(1, Some(2)),
        "redisplay" => arity_cons(0, Some(1)),
        "rename-buffer" => arity_cons(1, Some(2)),
        "set-buffer-major-mode" => arity_cons(1, Some(1)),
        "set-buffer-multibyte" => arity_cons(1, Some(1)),
        "setplist" => arity_cons(2, Some(2)),
        "split-window-internal" => arity_cons(4, Some(5)),
        "suspend-emacs" => arity_cons(0, Some(1)),
        "vertical-motion" => arity_cons(1, Some(3)),
        "x-begin-drag" => arity_cons(1, Some(6)),
        "x-create-frame" => arity_cons(1, Some(1)),
        "x-double-buffered-p" => arity_cons(0, Some(1)),
        "x-menu-bar-open-internal" => arity_cons(0, Some(1)),
        "xw-color-defined-p" => arity_cons(1, Some(2)),
        "xw-color-values" => arity_cons(1, Some(2)),
        "xw-display-color-p" => arity_cons(0, Some(1)),
        "merge-face-attribute" => arity_cons(3, Some(3)),
        "lookup-image-map" => arity_cons(3, Some(3)),
        "looking-at" | "posix-looking-at" => arity_cons(1, Some(2)),
        "match-beginning" | "match-end" => arity_cons(1, Some(1)),
        "match-data" => arity_cons(0, Some(3)),
        "replace-match" => arity_cons(1, Some(5)),
        "replace-region-contents" => arity_cons(3, Some(6)),
        "string-match" | "posix-string-match" => arity_cons(2, Some(4)),
        "string-as-multibyte"
        | "string-as-unibyte"
        | "string-bytes"
        | "string-make-multibyte"
        | "string-make-unibyte"
        | "string-to-multibyte"
        | "string-to-syntax"
        | "string-to-unibyte"
        | "substitute-in-file-name"
        | "syntax-class-to-char"
        | "syntax-table-p" => arity_cons(1, Some(1)),
        "string-collate-equalp" | "string-collate-lessp" => arity_cons(2, Some(4)),
        "make-string" => arity_cons(2, Some(3)),
        "string-search" => arity_cons(2, Some(3)),
        "string-version-lessp" => arity_cons(2, Some(2)),
        "string-width" => arity_cons(1, Some(3)),
        "clear-rectangle" => arity_cons(2, Some(3)),
        "sort" => arity_cons(1, None),
        "self-insert-command"
        | "single-key-description"
        | "skip-chars-backward"
        | "skip-chars-forward"
        | "skip-syntax-backward"
        | "skip-syntax-forward" => arity_cons(1, Some(2)),
        "shell-command-to-string"
        | "store-kbd-macro-event"
        | "unibyte-char-to-multibyte"
        | "multibyte-char-to-unibyte"
        | "multibyte-string-p"
        | "upcase-initials"
        | "upcase-word"
        | "use-global-map"
        | "use-local-map" => arity_cons(1, Some(1)),
        "subr-arity" | "subr-name" | "subrp" => arity_cons(1, Some(1)),
        "signal" | "take" => arity_cons(2, Some(2)),
        "secure-hash" => arity_cons(2, Some(5)),
        "secure-hash-algorithms" => arity_cons(0, Some(0)),
        "combine-after-change-execute"
        | "this-command-keys"
        | "this-command-keys-vector"
        | "undo-boundary" => arity_cons(0, Some(0)),
        "clear-this-command-keys" => arity_cons(0, Some(1)),
        "transpose-chars" => arity_cons(1, Some(1)),
        "treesit-available-p" => arity_cons(0, Some(0)),
        "treesit-compiled-query-p" => arity_cons(1, Some(1)),
        "treesit-induce-sparse-tree" => arity_cons(2, Some(4)),
        "treesit-language-abi-version" => arity_cons(0, Some(1)),
        "treesit-language-available-p" => arity_cons(1, Some(2)),
        "treesit-library-abi-version" => arity_cons(0, Some(1)),
        "treesit-node-check" => arity_cons(2, Some(2)),
        "treesit-node-child" => arity_cons(2, Some(3)),
        "treesit-node-child-by-field-name" => arity_cons(2, Some(2)),
        "treesit-node-child-count" => arity_cons(1, Some(2)),
        "treesit-node-descendant-for-range" => arity_cons(3, Some(4)),
        "treesit-node-end" => arity_cons(1, Some(1)),
        "treesit-node-eq" => arity_cons(2, Some(2)),
        "treesit-node-field-name-for-child" => arity_cons(2, Some(2)),
        "treesit-node-first-child-for-pos" => arity_cons(2, Some(3)),
        "treesit-node-match-p" => arity_cons(2, Some(3)),
        "treesit-node-next-sibling" => arity_cons(1, Some(2)),
        "treesit-node-p" => arity_cons(1, Some(1)),
        "treesit-node-parent" => arity_cons(1, Some(1)),
        "treesit-node-parser" => arity_cons(1, Some(1)),
        "treesit-node-prev-sibling" => arity_cons(1, Some(2)),
        "treesit-node-start" => arity_cons(1, Some(1)),
        "treesit-node-string" => arity_cons(1, Some(1)),
        "treesit-node-type" => arity_cons(1, Some(1)),
        "treesit-parser-add-notifier" => arity_cons(2, Some(2)),
        "treesit-parser-buffer" => arity_cons(1, Some(1)),
        "treesit-parser-create" => arity_cons(1, Some(4)),
        "treesit-parser-delete" => arity_cons(1, Some(1)),
        "treesit-parser-included-ranges" => arity_cons(1, Some(1)),
        "treesit-parser-language" => arity_cons(1, Some(1)),
        "treesit-parser-list" => arity_cons(0, Some(3)),
        "treesit-parser-notifiers" => arity_cons(1, Some(1)),
        "treesit-parser-p" => arity_cons(1, Some(1)),
        "treesit-parser-remove-notifier" => arity_cons(2, Some(2)),
        "treesit-parser-root-node" => arity_cons(1, Some(1)),
        "treesit-parser-set-included-ranges" => arity_cons(2, Some(2)),
        "treesit-parser-tag" => arity_cons(1, Some(1)),
        "treesit-pattern-expand" => arity_cons(1, Some(1)),
        "treesit-query-capture" => arity_cons(2, Some(5)),
        "treesit-query-compile" => arity_cons(2, Some(3)),
        "treesit-query-expand" => arity_cons(1, Some(1)),
        "treesit-query-language" => arity_cons(1, Some(1)),
        "treesit-query-p" => arity_cons(1, Some(1)),
        "treesit-search-forward" => arity_cons(2, Some(4)),
        "treesit-search-subtree" => arity_cons(2, Some(5)),
        "treesit-subtree-stat" => arity_cons(1, Some(1)),
        "upcase-initials-region" | "upcase-region" => arity_cons(2, Some(3)),
        "where-is-internal" => arity_cons(1, Some(5)),
        "text-char-description" | "threadp" | "yes-or-no-p" | "logcount" => arity_cons(1, Some(1)),
        "syntax-table"
        | "standard-case-table"
        | "standard-category-table"
        | "standard-syntax-table" => arity_cons(0, Some(0)),
        "libxml-available-p" | "zlib-available-p" => arity_cons(0, Some(0)),
        "libxml-parse-html-region" | "libxml-parse-xml-region" => arity_cons(0, Some(4)),
        "zlib-decompress-region" => arity_cons(2, Some(3)),
        "search-forward"
        | "search-backward"
        | "re-search-forward"
        | "re-search-backward"
        | "posix-search-forward"
        | "posix-search-backward" => arity_cons(1, Some(4)),
        "add-variable-watcher" => arity_cons(2, Some(2)),
        "remove-variable-watcher" | "narrow-to-region" => arity_cons(2, Some(2)),
        "save-buffer" | "scroll-down" | "scroll-up" => arity_cons(0, Some(1)),
        "load-average" => arity_cons(0, Some(1)),
        "select-window" | "minor-mode-key-binding" => arity_cons(1, Some(2)),
        "select-frame" => arity_cons(1, Some(2)),
        "selected-frame" => arity_cons(0, Some(0)),
        "set-charset-priority" => arity_cons(1, None),
        "write-char" => arity_cons(1, Some(2)),
        "write-region" => arity_cons(3, Some(7)),
        // advice-add, advice-remove, advice-member-p: handled by nadvice.el
        "autoload" => arity_cons(2, Some(5)),
        "autoload-do-load" => arity_cons(1, Some(3)),
        "Snarf-documentation" => arity_cons(1, Some(1)),
        "documentation" => arity_cons(1, Some(2)),
        "documentation-stringp" => arity_cons(1, Some(1)),
        "documentation-property" => arity_cons(2, Some(3)),
        "decode-coding-string" | "encode-coding-string" => arity_cons(2, Some(4)),
        "decode-time" => arity_cons(0, Some(3)),
        "detect-coding-region" => arity_cons(2, Some(3)),
        "detect-coding-string" => arity_cons(1, Some(2)),
        "encode-char" => arity_cons(2, Some(2)),
        "encode-time" => arity_cons(1, None),
        "format-time-string" => arity_cons(1, Some(3)),
        "format-mode-line" => arity_cons(1, Some(4)),
        "indent-to" | "move-to-column" => arity_cons(1, Some(2)),
        "indent-region" => arity_cons(2, Some(3)),
        "indent-for-tab-command" => arity_cons(0, Some(1)),
        "indent-according-to-mode" => arity_cons(0, Some(1)),
        "reindent-then-newline-and-indent" | "back-to-indentation" => arity_cons(0, Some(0)),
        "backtrace--frames-from-thread" => arity_cons(1, Some(1)),
        "backtrace--locals" => arity_cons(1, Some(2)),
        "backtrace-debug" | "backtrace-eval" => arity_cons(2, Some(3)),
        "backtrace-frame--internal" => arity_cons(3, Some(3)),
        "run-hook-with-args"
        | "run-hook-with-args-until-failure"
        | "run-hook-with-args-until-success" => arity_cons(1, None),
        "run-hook-wrapped" => arity_cons(2, None),
        "run-window-configuration-change-hook" | "run-window-scroll-functions" => {
            arity_cons(0, Some(1))
        }
        "base64-decode-region" => arity_cons(2, Some(4)),
        "base64-encode-region" | "base64url-encode-region" => arity_cons(2, Some(3)),
        "base64-decode-string" => arity_cons(1, Some(3)),
        "base64-encode-string" | "base64url-encode-string" => arity_cons(1, Some(2)),
        "md5" => arity_cons(1, Some(5)),
        "bool-vector" => arity_cons(0, None),
        "bool-vector-p" | "bool-vector-count-population" => arity_cons(1, Some(1)),
        "bool-vector-count-consecutive" => arity_cons(3, Some(3)),
        "bool-vector-not" => arity_cons(1, Some(2)),
        "bool-vector-subsetp" => arity_cons(2, Some(2)),
        "make-bool-vector" => arity_cons(2, Some(2)),
        "bitmap-spec-p" => arity_cons(1, Some(1)),
        "bool-vector-exclusive-or"
        | "bool-vector-intersection"
        | "bool-vector-set-difference"
        | "bool-vector-union" => arity_cons(2, Some(3)),
        "arrayp" | "atom" | "bufferp" | "char-to-string" | "consp" | "downcase" | "float"
        | "floatp" | "integerp" | "keywordp" | "listp" | "nlistp" | "null" | "number-to-string"
        | "numberp" | "sequencep" | "string-to-char" | "stringp" | "symbolp" | "type-of"
        | "cl-type-of" | "upcase" | "vectorp" => arity_cons(1, Some(1)),
        "ceiling" | "characterp" | "floor" | "round" | "string-to-number" | "truncate" => {
            arity_cons(1, Some(2))
        }
        "byte-to-position" | "byte-to-string" | "position-bytes" => arity_cons(1, Some(1)),
        "substring" | "substring-no-properties" => arity_cons(1, Some(3)),
        "aref" | "char-equal" | "eq" | "eql" | "equal" | "function-equal" | "make-vector"
        | "string-equal" | "string-lessp" | "string>" | "throw" => arity_cons(2, Some(2)),
        "aset" => arity_cons(3, Some(3)),
        "clear-composition-cache" => arity_cons(0, Some(0)),
        "composition-sort-rules" => arity_cons(1, Some(1)),
        "compute-motion" => arity_cons(7, Some(7)),
        "compose-region-internal" => arity_cons(2, Some(4)),
        "compose-string-internal" => arity_cons(3, Some(5)),
        "composition-get-gstring" => arity_cons(4, Some(4)),
        "find-composition-internal" => arity_cons(4, Some(4)),
        "call-interactively" => arity_cons(1, Some(3)),
        "command-error-default-function" | "ngettext" => arity_cons(3, Some(3)),
        "compare-strings" => arity_cons(6, Some(7)),
        "compare-buffer-substrings" => arity_cons(6, Some(6)),
        "comp--init-ctxt"
        | "comp--release-ctxt"
        | "comp-libgccjit-version"
        | "comp-native-compiler-options-effective-p"
        | "comp-native-driver-options-effective-p" => arity_cons(0, Some(0)),
        "comp--compile-ctxt-to-file0"
        | "comp--subr-signature"
        | "comp-el-to-eln-rel-filename"
        | "dbus-get-unique-name" => arity_cons(1, Some(1)),
        "comp-el-to-eln-filename" | "dbus--init-bus" => arity_cons(1, Some(2)),
        "comp--install-trampoline" => arity_cons(2, Some(2)),
        "comp--late-register-subr" | "comp--register-lambda" | "comp--register-subr" => {
            arity_cons(7, Some(7))
        }
        "dbus-message-internal" => arity_cons(4, None),
        "define-fringe-bitmap" => arity_cons(2, Some(5)),
        "destroy-fringe-bitmap" | "external-debugging-output" => arity_cons(1, Some(1)),
        "display--line-is-continued-p" => arity_cons(0, Some(0)),
        "display--update-for-mouse-movement" | "fillarray" => arity_cons(2, Some(2)),
        "do-auto-save" | "delete-terminal" => arity_cons(0, Some(2)),
        "describe-buffer-bindings" => arity_cons(1, Some(3)),
        "describe-vector" => arity_cons(1, Some(2)),
        "face-attributes-as-vector" => arity_cons(1, Some(1)),
        "font-at" => arity_cons(1, Some(3)),
        "font-face-attributes" | "font-info" | "fontset-info" => arity_cons(1, Some(2)),
        "font-get-glyphs" => arity_cons(3, Some(4)),
        "font-get-system-font" | "font-get-system-normal-font" | "fontset-list" => {
            arity_cons(0, Some(0))
        }
        "font-has-char-p" | "fontset-font" => arity_cons(2, Some(3)),
        "font-match-p" | "font-shape-gstring" | "font-variation-glyphs" => arity_cons(2, Some(2)),
        "frame--set-was-invisible" | "frame-after-make-frame" | "frame-ancestor-p" => {
            arity_cons(2, Some(2))
        }
        "frame--face-hash-table"
        | "frame-bottom-divider-width"
        | "frame-child-frame-border-width"
        | "frame-focus"
        | "frame-font-cache"
        | "frame-fringe-width"
        | "frame-internal-border-width"
        | "frame-old-selected-window"
        | "frame-or-buffer-changed-p"
        | "frame-parent"
        | "frame-pointer-visible-p"
        | "frame-scale-factor"
        | "frame-scroll-bar-height"
        | "frame-scroll-bar-width"
        | "frame-window-state-change"
        | "frame-right-divider-width" => arity_cons(0, Some(1)),
        "fringe-bitmaps-at-pos" => arity_cons(0, Some(2)),
        "gap-position" | "gap-size" => arity_cons(0, Some(0)),
        "gnutls-available-p" | "gnutls-ciphers" | "gnutls-digests" | "gnutls-macs"
        | "gpm-mouse-start" | "gpm-mouse-stop" | "sqlite-available-p" | "sqlite-version" => {
            arity_cons(0, Some(0))
        }
        "gnutls-asynchronous-parameters" | "gnutls-bye" | "gnutls-hash-digest" => {
            arity_cons(2, Some(2))
        }
        "gnutls-boot" | "gnutls-hash-mac" | "inotify-add-watch" => arity_cons(3, Some(3)),
        "garbage-collect-maybe" | "get-variable-watchers" => arity_cons(1, Some(1)),
        "gnutls-deinit" | "gnutls-format-certificate" | "gnutls-get-initstage" => {
            arity_cons(1, Some(1))
        }
        "gnutls-error-fatalp"
        | "gnutls-error-string"
        | "gnutls-errorp"
        | "gnutls-peer-status"
        | "gnutls-peer-status-warning-describe"
        | "inotify-rm-watch"
        | "inotify-valid-p" => arity_cons(1, Some(1)),
        "gnutls-symmetric-decrypt" | "gnutls-symmetric-encrypt" => arity_cons(4, Some(5)),
        "handle-save-session"
        | "handle-switch-frame"
        | "init-image-library"
        | "interactive-form"
        | "lock-file"
        | "sqlite-close"
        | "sqlite-columns"
        | "sqlite-commit"
        | "sqlite-finalize"
        | "sqlite-more-p"
        | "sqlite-next"
        | "sqlite-rollback"
        | "sqlite-transaction"
        | "sqlitep"
        | "unlock-file" => arity_cons(1, Some(1)),
        "help--describe-vector" => arity_cons(7, Some(7)),
        "innermost-minibuffer-p" | "lock-buffer" | "lossage-size" | "sqlite-open" => {
            arity_cons(0, Some(1))
        }
        "sqlite-execute" => arity_cons(2, Some(3)),
        "sqlite-execute-batch" | "sqlite-load-extension" | "sqlite-pragma" => {
            arity_cons(2, Some(2))
        }
        "sqlite-select" => arity_cons(2, Some(4)),
        "window-at" => arity_cons(2, Some(3)),
        "window-combination-limit" => arity_cons(1, Some(1)),
        "window-line-height"
        | "window-normal-size"
        | "window-resize-apply"
        | "window-resize-apply-total" => arity_cons(0, Some(2)),
        "window-lines-pixel-dimensions" => arity_cons(0, Some(6)),
        "window-list-1" => arity_cons(0, Some(3)),
        "window-bottom-divider-width"
        | "window-bump-use-time"
        | "window-left-child"
        | "window-new-normal"
        | "window-new-pixel"
        | "window-new-total"
        | "window-next-sibling"
        | "window-old-body-pixel-height"
        | "window-old-body-pixel-width"
        | "window-old-pixel-height"
        | "window-old-pixel-width"
        | "window-parent"
        | "window-pixel-left"
        | "window-pixel-top"
        | "window-prev-sibling"
        | "window-right-divider-width"
        | "window-scroll-bar-height"
        | "window-scroll-bar-width"
        | "window-tab-line-height"
        | "window-top-child" => arity_cons(0, Some(1)),
        "local-variable-if-set-p" => arity_cons(1, Some(2)),
        "unlock-buffer" => arity_cons(0, Some(0)),
        "get-unicode-property-internal" => arity_cons(2, Some(2)),
        "define-hash-table-test" => arity_cons(3, Some(3)),
        "find-coding-systems-region-internal" => arity_cons(2, Some(3)),
        "completing-read" => arity_cons(2, Some(8)),
        "try-completion" | "test-completion" => arity_cons(2, Some(3)),
        "all-completions" => arity_cons(2, Some(4)),
        "read" => arity_cons(0, Some(1)),
        "read-char" | "read-char-exclusive" | "read-event" => arity_cons(0, Some(3)),
        "read-string" => arity_cons(1, Some(5)),
        "read-variable" | "read-command" => arity_cons(1, Some(2)),
        "read-from-string" => arity_cons(1, Some(3)),
        "read-buffer" => arity_cons(1, Some(4)),
        "read-coding-system" => arity_cons(1, Some(2)),
        "read-from-minibuffer" => arity_cons(1, Some(7)),
        "read-key-sequence" | "read-key-sequence-vector" => arity_cons(1, Some(6)),
        "read-non-nil-coding-system" => arity_cons(1, Some(1)),
        "json-insert" | "json-parse-string" | "json-serialize" => arity_cons(1, None),
        "keymap-parent" | "keymapp" => arity_cons(1, Some(1)),
        "accessible-keymaps" => arity_cons(1, Some(2)),
        "global-key-binding" | "key-description" => arity_cons(1, Some(2)),
        "lookup-key" => arity_cons(2, Some(3)),
        "set-keymap-parent" => arity_cons(2, Some(2)),
        "key-binding" => arity_cons(1, Some(4)),
        "keyboard-coding-system" => arity_cons(0, Some(1)),
        "make-keymap" | "make-sparse-keymap" => arity_cons(0, Some(1)),
        "current-active-maps" => arity_cons(0, Some(2)),
        "current-bidi-paragraph-direction" | "bidi-resolved-levels" => arity_cons(0, Some(1)),
        "bidi-find-overridden-directionality" => arity_cons(3, Some(4)),
        "current-case-table"
        | "current-column"
        | "current-global-map"
        | "current-indentation"
        | "current-local-map"
        | "current-message"
        | "current-minor-mode-maps" => arity_cons(0, Some(0)),
        "set-buffer"
        | "set-buffer-modified-p"
        | "set-case-table"
        | "set-category-table"
        | "set-default-file-modes"
        | "set-standard-case-table"
        | "set-syntax-table"
        | "set-time-zone-rule" => arity_cons(1, Some(1)),
        "set-default"
        | "set-default-toplevel-value"
        | "set-window-dedicated-p"
        | "setcar"
        | "setcdr" => arity_cons(2, Some(2)),
        "set-char-table-parent" => arity_cons(2, Some(2)),
        "set-char-table-extra-slot" | "set-char-table-range" => arity_cons(3, Some(3)),
        "set-file-modes" => arity_cons(2, Some(3)),
        "set-file-acl" | "set-file-selinux-context" => arity_cons(2, Some(2)),
        "set-file-times" => arity_cons(1, Some(3)),
        "set-keyboard-coding-system"
        | "set-keyboard-coding-system-internal"
        | "set-match-data"
        | "set-text-conversion-style" => arity_cons(1, Some(2)),
        "set-terminal-coding-system-internal" => arity_cons(1, Some(2)),
        "set-safe-terminal-coding-system-internal" => arity_cons(1, Some(1)),
        "set-visited-file-modtime" | "verify-visited-file-modtime" => arity_cons(0, Some(1)),
        "visited-file-modtime" => arity_cons(0, Some(0)),
        "scan-lists" => arity_cons(3, Some(3)),
        "scan-sexps" => arity_cons(2, Some(2)),
        "current-time-string" | "current-time-zone" => arity_cons(0, Some(2)),
        "time-add" | "time-equal-p" | "time-less-p" | "time-subtract" => arity_cons(2, Some(2)),
        "time-convert" => arity_cons(1, Some(2)),
        "system-name" => arity_cons(0, Some(0)),
        "system-groups" | "system-users" => arity_cons(0, Some(0)),
        "tab-bar-height" | "tool-bar-height" => arity_cons(0, Some(2)),
        "user-real-login-name" | "user-real-uid" | "user-uid" => arity_cons(0, Some(0)),
        "user-full-name" | "user-login-name" => arity_cons(0, Some(1)),
        "line-beginning-position"
        | "line-end-position"
        | "line-number-display-width"
        | "pos-bol"
        | "pos-eol" => arity_cons(0, Some(1)),
        "line-number-at-pos" => arity_cons(0, Some(2)),
        "line-pixel-height" | "long-line-optimizations-p" => arity_cons(0, Some(0)),
        "beginning-of-buffer" | "end-of-buffer" => arity_cons(0, Some(1)),
        "next-line" | "previous-line" => arity_cons(0, Some(1)),
        "scroll-left" | "scroll-right" => arity_cons(0, Some(2)),
        "recenter" => arity_cons(0, Some(2)),
        "recursion-depth" | "region-beginning" | "region-end" => arity_cons(0, Some(0)),
        "delete-frame" | "delete-other-windows-internal" => arity_cons(0, Some(2)),
        "next-window" | "previous-window" | "pos-visible-in-window-p" => arity_cons(0, Some(3)),
        "other-window-for-scrolling" => arity_cons(0, Some(0)),
        "coordinates-in-window-p" => arity_cons(2, Some(2)),
        "goto-line" | "move-to-window-line" | "move-point-visually" => arity_cons(1, Some(1)),
        "modify-frame-parameters" => arity_cons(2, Some(2)),
        "make-frame-visible" | "iconify-frame" => arity_cons(0, Some(1)),
        "get-unused-category" => arity_cons(0, Some(1)),
        "make-category-set" | "category-set-mnemonics" | "forward-comment" | "natnump" => {
            arity_cons(1, Some(1))
        }
        "make-category-table" | "preceding-char" => arity_cons(0, Some(0)),
        "make-char-table" => arity_cons(1, Some(2)),
        "modify-category-entry" => arity_cons(2, Some(4)),
        "modify-syntax-entry" | "plist-get" | "plist-member" => arity_cons(2, Some(3)),
        "plist-put" => arity_cons(3, Some(4)),
        "constrain-to-field" => arity_cons(2, Some(5)),
        "field-beginning" | "field-end" => arity_cons(0, Some(3)),
        "field-string" | "field-string-no-properties" => arity_cons(0, Some(1)),
        "parse-partial-sexp" => arity_cons(2, Some(6)),
        "matching-paren" => arity_cons(1, Some(1)),
        "file-attributes-lessp" => arity_cons(2, Some(2)),
        "float-time" => arity_cons(0, Some(1)),
        "force-mode-line-update" | "force-window-update" => arity_cons(0, Some(1)),
        "featurep" => arity_cons(1, Some(2)),
        "commandp" => arity_cons(1, Some(2)),
        "command-execute" => arity_cons(1, Some(4)),
        "command-modes" => arity_cons(1, Some(1)),
        "command-remapping" => arity_cons(1, Some(3)),
        "sleep-for" => arity_cons(1, Some(2)),
        "current-cpu-time"
        | "current-idle-time"
        | "current-time"
        | "flush-standard-output"
        | "get-internal-run-time"
        | "invocation-directory"
        | "invocation-name" => arity_cons(0, Some(0)),
        "text-quoting-style" => arity_cons(0, Some(0)),
        "daemon-initialized" | "daemonp" => arity_cons(0, Some(0)),
        "category-table" | "clear-buffer-auto-save-failure" | "clear-charset-maps" => {
            arity_cons(0, Some(0))
        }
        "find-charset-region" => arity_cons(2, Some(3)),
        "find-charset-string" => arity_cons(1, Some(2)),
        "case-table-p"
        | "category-table-p"
        | "ccl-program-p"
        | "check-coding-system"
        | "clear-string"
        | "module-function-p"
        | "number-or-marker-p"
        | "obarrayp"
        | "special-variable-p"
        | "symbol-with-pos-p"
        | "symbol-with-pos-pos"
        | "user-ptrp"
        | "vector-or-char-table-p" => arity_cons(1, Some(1)),
        "coding-system-aliases"
        | "coding-system-base"
        | "coding-system-eol-type"
        | "coding-system-p" => arity_cons(1, Some(1)),
        "check-coding-systems-region" => arity_cons(3, Some(3)),
        "coding-system-plist" => arity_cons(1, Some(1)),
        "coding-system-put" => arity_cons(3, Some(3)),
        "coding-system-priority-list" => arity_cons(0, Some(1)),
        "color-distance" => arity_cons(2, Some(4)),
        "color-gray-p" => arity_cons(1, Some(2)),
        "color-supported-p" => arity_cons(1, Some(3)),
        "color-values-from-color-spec" => arity_cons(1, Some(1)),
        "category-docstring" => arity_cons(1, Some(2)),
        "ccl-execute" => arity_cons(2, Some(2)),
        "ccl-execute-on-string" => arity_cons(3, Some(5)),
        "and"
        | "cond"
        | "interactive"
        | "or"
        | "progn"
        | "save-current-buffer"
        | "save-excursion"
        | "save-restriction"
        | "setq" => arity_unevalled(0),
        "catch" | "defvar" | "function" | "let" | "let*" | "prog1" | "quote" | "unwind-protect"
        | "while" => arity_unevalled(1),
        "condition-case" | "defconst" | "if" => arity_unevalled(2),
        "start-kbd-macro" => arity_cons(1, Some(2)),
        "cancel-kbd-macro-events" => arity_cons(0, Some(0)),
        "end-kbd-macro" | "call-last-kbd-macro" => arity_cons(0, Some(2)),
        "execute-kbd-macro" => arity_cons(1, Some(3)),
        "delete-char" => arity_cons(1, Some(2)),
        "delete-all-overlays" => arity_cons(0, Some(1)),
        "delete-and-extract-region" => arity_cons(2, Some(2)),
        "delete-field" => arity_cons(0, Some(1)),
        "delete-region" => arity_cons(2, Some(2)),
        "delete-overlay" => arity_cons(1, Some(1)),
        "delete-window-internal" => arity_cons(1, Some(1)),
        "delete-directory-internal" => arity_cons(1, Some(1)),
        "delete-file-internal" => arity_cons(1, Some(1)),
        "access-file" => arity_cons(2, Some(2)),
        "default-file-modes" => arity_cons(0, Some(0)),
        "directory-file-name" | "directory-name-p" => arity_cons(1, Some(1)),
        "directory-files" => arity_cons(1, Some(5)),
        "directory-files-and-attributes" => arity_cons(1, Some(6)),
        "define-category" => arity_cons(2, Some(3)),
        "define-charset-internal" => arity_cons(17, None),
        "define-coding-system-alias" => arity_cons(2, Some(2)),
        "define-key" => arity_cons(3, Some(4)),
        "expand-file-name" => arity_cons(1, Some(2)),
        "event-convert-list" | "error-message-string" => arity_cons(1, Some(1)),
        "copysign" | "equal-including-properties" => arity_cons(2, Some(2)),
        "emacs-pid" => arity_cons(0, Some(0)),
        "eval" => arity_cons(1, Some(2)),
        "eval-buffer" => arity_cons(0, Some(5)),
        "eval-region" => arity_cons(2, Some(4)),
        "recent-keys" => arity_cons(0, Some(1)),
        "input-pending-p" => arity_cons(0, Some(1)),
        "discard-input" => arity_cons(0, Some(0)),
        "current-input-mode" => arity_cons(0, Some(0)),
        "set-input-mode" => arity_cons(3, Some(4)),
        "set-input-meta-mode" => arity_cons(1, Some(2)),
        "set-input-interrupt-mode" | "set-quit-char" => arity_cons(1, Some(1)),
        "set-output-flow-control" => arity_cons(1, Some(2)),
        "waiting-for-user-input-p" => arity_cons(0, Some(0)),
        "minibufferp" => arity_cons(0, Some(2)),
        "next-read-file-uses-dialog-p" => arity_cons(0, Some(0)),
        "recursive-edit"
        | "top-level"
        | "exit-recursive-edit"
        | "abort-minibuffers"
        | "abort-recursive-edit"
        | "minibuffer-depth"
        | "minibuffer-prompt"
        | "minibuffer-contents"
        | "minibuffer-contents-no-properties" => arity_cons(0, Some(0)),
        "regexp-quote" => arity_cons(1, Some(1)),
        "x-apply-session-resources" | "x-clipboard-yank" => arity_cons(0, Some(0)),
        "open-termscript"
        | "x-close-connection"
        | "x-device-class"
        | "x-get-input-coding-system"
        | "x-internal-focus-input-context"
        | "x-parse-geometry"
        | "x-preedit-text"
        | "x-setup-function-keys" => arity_cons(1, Some(1)),
        "x-get-local-selection" => arity_cons(0, Some(2)),
        "send-string-to-terminal"
        | "display-supports-face-attributes-p"
        | "x-focus-frame"
        | "x-get-atom-name"
        | "x-register-dnd-atom"
        | "x-synchronize"
        | "x-display-set-last-user-time" => arity_cons(1, Some(2)),
        "x-own-selection-internal" => arity_cons(2, Some(3)),
        "x-get-selection-internal" => arity_cons(2, Some(4)),
        "x-change-window-property" => arity_cons(2, Some(7)),
        "x-send-client-message" => arity_cons(6, Some(6)),
        "x-set-mouse-absolute-pixel-position" => arity_cons(2, Some(2)),
        "x-popup-menu" => arity_cons(2, Some(2)),
        "x-delete-window-property"
        | "x-disown-selection-internal"
        | "x-window-property-attributes" => arity_cons(1, Some(3)),
        "x-open-connection" => arity_cons(1, Some(3)),
        "x-popup-dialog" | "x-frame-restack" => arity_cons(2, Some(3)),
        "x-get-resource" => arity_cons(2, Some(4)),
        "x-list-fonts" => arity_cons(1, Some(5)),
        "x-show-tip" | "x-window-property" | "x-translate-coordinates" => arity_cons(1, Some(6)),
        "internal-show-cursor" => arity_cons(2, Some(2)),
        "clear-face-cache" => arity_cons(0, Some(1)),
        "clear-font-cache" => arity_cons(0, Some(0)),
        "clear-image-cache" => arity_cons(0, Some(2)),
        "image-cache-size" => arity_cons(0, Some(0)),
        "find-font" => arity_cons(1, Some(2)),
        "font-family-list" => arity_cons(0, Some(1)),
        "image-flush" | "image-mask-p" | "image-metadata" => arity_cons(1, Some(2)),
        "imagep" => arity_cons(1, Some(1)),
        "image-size" => arity_cons(1, Some(3)),
        "image-transforms-p" => arity_cons(0, Some(1)),
        "list-fonts" => arity_cons(1, Some(4)),
        "frame-parameter"
        | "set-window-point"
        | "set-window-hscroll"
        | "set-window-display-table"
        | "set-window-cursor-type"
        | "set-window-prev-buffers"
        | "set-window-next-buffers" => arity_cons(2, Some(2)),
        "set-window-buffer" | "set-window-start" | "set-window-margins" => arity_cons(2, Some(3)),
        "set-window-configuration" => arity_cons(1, Some(3)),
        "set-window-vscroll" => arity_cons(2, Some(4)),
        "set-window-fringes" => arity_cons(2, Some(5)),
        "set-window-scroll-bars" => arity_cons(1, Some(6)),
        "buffer-text-pixel-size" => arity_cons(0, Some(4)),
        "window-text-pixel-size" => arity_cons(0, Some(7)),
        // Process primitives
        "accept-process-output" => arity_cons(0, Some(4)),
        "call-process" => arity_cons(1, None),
        "call-process-region" => arity_cons(3, None),
        "continue-process" | "interrupt-process" | "kill-process" | "quit-process"
        | "stop-process" => arity_cons(0, Some(2)),
        "delete-process" => arity_cons(0, Some(1)),
        "format-network-address" => arity_cons(1, Some(2)),
        "get-buffer-process" => arity_cons(1, Some(1)),
        "get-process" => arity_cons(1, Some(1)),
        "internal-default-interrupt-process" => arity_cons(0, Some(2)),
        "internal-default-process-filter" | "internal-default-process-sentinel" => {
            arity_cons(2, Some(2))
        }
        "internal-default-signal-process" => arity_cons(2, Some(3)),
        "list-system-processes" => arity_cons(0, Some(0)),
        "make-process" => arity_cons(0, None),
        "make-network-process" | "make-pipe-process" | "make-serial-process" => arity_cons(0, None),
        "network-interface-info" => arity_cons(1, Some(1)),
        "network-interface-list" => arity_cons(0, Some(2)),
        "network-lookup-address-info" => arity_cons(1, Some(3)),
        "num-processors" => arity_cons(0, Some(1)),
        "print--preprocess" => arity_cons(1, Some(1)),
        "process-buffer"
        | "process-attributes"
        | "process-coding-system"
        | "process-command"
        | "process-datagram-address"
        | "process-exit-status"
        | "process-filter"
        | "process-id"
        | "process-inherit-coding-system-flag"
        | "process-mark"
        | "process-name"
        | "process-plist"
        | "process-query-on-exit-flag"
        | "process-sentinel"
        | "process-status"
        | "process-thread"
        | "process-type"
        | "processp" => arity_cons(1, Some(1)),
        "process-kill-buffer-query-function" => arity_cons(0, Some(1)),
        "process-file" => arity_cons(1, None),
        "process-running-child-p" | "process-send-eof" => arity_cons(0, Some(1)),
        "signal-process" => arity_cons(2, Some(3)),
        "signal-names" => arity_cons(0, Some(0)),
        "process-contact" => arity_cons(1, Some(3)),
        "process-list" => arity_cons(0, Some(0)),
        "process-send-region" => arity_cons(3, Some(3)),
        "process-send-string" => arity_cons(2, Some(2)),
        "process-tty-name" => arity_cons(1, Some(2)),
        "set-process-coding-system" => arity_cons(1, Some(3)),
        "set-binary-mode" => arity_cons(2, Some(2)),
        "set-process-filter"
        | "set-process-buffer"
        | "set-process-datagram-address"
        | "set-process-inherit-coding-system-flag"
        | "set-process-plist"
        | "set-process-query-on-exit-flag"
        | "set-process-thread"
        | "set-process-sentinel" => arity_cons(2, Some(2)),
        "set-process-window-size" => arity_cons(3, Some(3)),
        "set-network-process-option" => arity_cons(3, Some(4)),
        "serial-process-configure" => arity_cons(0, None),
        "getenv-internal" => arity_cons(1, Some(2)),
        // Display/terminal query primitives
        "terminal-name"
        | "terminal-parameters"
        | "frame-terminal"
        | "tty-type"
        | "tty-top-frame"
        | "tty-display-color-p"
        | "tty-display-color-cells"
        | "tty-no-underline"
        | "window-system"
        | "x-display-pixel-width"
        | "x-display-pixel-height"
        | "x-display-backing-store"
        | "x-display-color-cells"
        | "x-display-mm-height"
        | "x-display-mm-width"
        | "x-display-monitor-attributes-list"
        | "x-display-planes"
        | "x-display-save-under"
        | "x-display-screens"
        | "x-display-visual-class"
        | "x-get-modifier-masks"
        | "x-wm-set-size-hint"
        | "x-server-version"
        | "x-server-input-extension-version"
        | "x-server-max-request-size"
        | "x-server-vendor"
        | "x-display-grayscale-p"
        | "x-backspace-delete-keys-p"
        | "x-frame-geometry"
        | "x-frame-list-z-order"
        | "redraw-frame"
        | "ding"
        | "internal-show-cursor-p"
        | "controlling-tty-p"
        | "suspend-tty"
        | "resume-tty"
        | "terminal-coding-system"
        | "frame-char-height"
        | "frame-char-width"
        | "frame-first-window"
        | "frame-native-height"
        | "frame-native-width"
        | "frame-position"
        | "frame-root-window"
        | "frame-parameters"
        | "frame-selected-window"
        | "frame-text-cols"
        | "frame-text-height"
        | "frame-text-lines"
        | "frame-text-width"
        | "frame-total-cols"
        | "frame-total-lines"
        | "current-window-configuration"
        | "minibuffer-window"
        | "window-buffer"
        | "window-cursor-type"
        | "window-display-table"
        | "window-frame"
        | "window-fringes"
        | "window-header-line-height"
        | "window-dedicated-p"
        | "window-hscroll"
        | "window-left-column"
        | "window-margins"
        | "window-minibuffer-p"
        | "window-mode-line-height"
        | "window-pixel-height"
        | "window-pixel-width"
        | "window-point"
        | "window-next-buffers"
        | "window-old-buffer"
        | "window-old-point"
        | "window-parameters"
        | "window-prev-buffers"
        | "window-scroll-bars"
        | "window-start"
        | "window-use-time"
        | "window-top-line" => arity_cons(0, Some(1)),
        "terminal-list"
        | "x-clear-preedit-text"
        | "x-display-list"
        | "x-hide-tip"
        | "x-mouse-absolute-pixel-position"
        | "redraw-display"
        | "frame-list"
        | "visible-frame-list"
        | "x-uses-old-gtk-dialog"
        | "x-win-suspend-error"
        | "selected-window"
        | "active-minibuffer-window"
        | "minibuffer-selected-window" => arity_cons(0, Some(0)),
        "frame-edges"
        | "window-body-height"
        | "window-body-width"
        | "window-end"
        | "window-text-height"
        | "window-text-width"
        | "window-total-height"
        | "window-total-width"
        | "window-vscroll"
        | "x-export-frames"
        | "x-frame-edges"
        | "x-family-fonts"
        | "x-selection-exists-p"
        | "x-selection-owner-p"
        | "get-buffer-window" => arity_cons(0, Some(2)),
        "set-frame-height" | "set-frame-width" => arity_cons(2, Some(4)),
        "set-frame-size" => arity_cons(3, Some(4)),
        "set-frame-position" => arity_cons(3, Some(3)),
        "window-list" => arity_cons(0, Some(3)),
        "terminal-live-p"
        | "frame-live-p"
        | "frame-visible-p"
        | "window-live-p"
        | "window-configuration-p"
        | "window-valid-p"
        | "windowp" => arity_cons(1, Some(1)),
        "window-configuration-equal-p" => arity_cons(2, Some(2)),
        "window-configuration-frame" => arity_cons(1, Some(1)),
        "window-parameter" => arity_cons(2, Some(2)),
        "set-window-parameter" => arity_cons(3, Some(3)),
        "terminal-parameter" => arity_cons(2, Some(2)),
        "set-terminal-parameter" => arity_cons(3, Some(3)),
        // Threading primitives
        "thread-join"
        | "thread-name"
        | "thread-live-p"
        | "mutexp"
        | "mutex-name"
        | "mutex-lock"
        | "mutex-unlock"
        | "condition-variable-p"
        | "condition-name"
        | "condition-mutex"
        | "condition-wait" => arity_cons(1, Some(1)),
        "thread-yield" | "current-thread" | "all-threads" => arity_cons(0, Some(0)),
        "thread-signal" => arity_cons(3, Some(3)),
        "thread-last-error" | "make-mutex" => arity_cons(0, Some(1)),
        "make-thread" | "make-condition-variable" | "condition-notify" => arity_cons(1, Some(2)),
        _ => arity_cons(0, None),
    }
}

pub(crate) fn dispatch_subr_arity_value(name: &str) -> Value {
    subr_arity_value(name)
}

fn is_macro_object(value: &Value) -> bool {
    match value {
        Value::Macro(_) => true,
        Value::Cons(cell) => read_cons(*cell).car.as_symbol_name() == Some("macro"),
        _ => false,
    }
}

fn autoload_macro_marker(value: &Value) -> Option<Value> {
    if !super::autoload::is_autoload_value(value) {
        return None;
    }

    let items = list_to_vec(value)?;
    let autoload_type = items.get(4)?;
    if autoload_type.as_symbol_name() == Some("macro") {
        Some(Value::list(vec![Value::symbol("macro"), Value::True]))
    } else if matches!(autoload_type, Value::True) {
        // GNU Emacs uses `t` as a legacy macro marker for some startup
        // autoloads (notably `pcase-dolist`), and `macrop` returns `(t)`.
        Some(Value::list(vec![Value::True]))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Pure builtins (no evaluator access)
// ---------------------------------------------------------------------------

/// `(subr-name SUBR)` -- return the name of a subroutine as a string.
pub(crate) fn builtin_subr_name(args: Vec<Value>) -> EvalResult {
    expect_args("subr-name", &args, 1)?;
    match &args[0] {
        Value::Subr(id) => Ok(Value::string(resolve_sym(*id))),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("subrp"), *other],
        )),
    }
}

/// `(subr-arity SUBR)` -- return (MIN . MAX) cons cell for argument counts.
///
/// Built-in subrs are dispatched by name and we do not yet have complete
/// per-subr metadata in NeoVM, so this is a partial compatibility table:
/// known shapes are special-cased and other subrs default to `(0 . many)`.
pub(crate) fn builtin_subr_arity(args: Vec<Value>) -> EvalResult {
    expect_args("subr-arity", &args, 1)?;
    match &args[0] {
        Value::Subr(id) => Ok(subr_arity_value(resolve_sym(*id))),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("subrp"), *other],
        )),
    }
}

/// `(subr-native-elisp-p OBJECT)` -- return t if OBJECT is a native-compiled
/// Elisp subr.
///
/// NeoVM does not currently model native-compiled Elisp subrs, so this always
/// returns nil.
pub(crate) fn builtin_subr_native_elisp_p(args: Vec<Value>) -> EvalResult {
    Ok(Value::Nil)
}

/// `(native-comp-function-p OBJECT)` -- return t if OBJECT is a native-compiled
/// function object.
///
/// NeoVM does not currently model native-compiled function objects, so this
/// always returns nil.
pub(crate) fn builtin_native_comp_function_p(args: Vec<Value>) -> EvalResult {
    expect_args("native-comp-function-p", &args, 1)?;
    Ok(Value::Nil)
}

/// `(subr-primitive-p OBJECT)` -- return t if OBJECT is a primitive subr.
pub(crate) fn builtin_subr_primitive_p(args: Vec<Value>) -> EvalResult {
    expect_args("subr-primitive-p", &args, 1)?;
    Ok(Value::bool(matches!(&args[0], Value::Subr(_))))
}

/// `(interpreted-function-p OBJECT)` -- return t if OBJECT is an interpreted
/// function (a Lambda that is NOT byte-compiled).
///
/// In our VM, any `Value::Lambda` is interpreted (as opposed to
/// `Value::ByteCode`).
pub(crate) fn builtin_interpreted_function_p(args: Vec<Value>) -> EvalResult {
    expect_args("interpreted-function-p", &args, 1)?;
    Ok(Value::bool(matches!(&args[0], Value::Lambda(_))))
}

/// `(special-form-p OBJECT)` -- return t if OBJECT is a symbol that names a
/// special form.
///
/// Accepts a symbol (including nil/t) and checks it against the evaluator's
/// special-form table.
pub(crate) fn builtin_special_form_p(args: Vec<Value>) -> EvalResult {
    expect_args("special-form-p", &args, 1)?;
    let result = match &args[0] {
        Value::Symbol(id) => {
            let name = resolve_sym(*id);
            lookup_interned(name).is_some_and(|canonical| canonical == *id)
                && is_public_special_form_name(name)
        }
        Value::Subr(id) => is_public_special_form_name(resolve_sym(*id)),
        _ => false,
    };
    Ok(Value::bool(result))
}

/// `(macrop OBJECT)` -- return t if OBJECT is a macro.
pub(crate) fn builtin_macrop(args: Vec<Value>) -> EvalResult {
    expect_args("macrop", &args, 1)?;
    if let Some(marker) = autoload_macro_marker(&args[0]) {
        return Ok(marker);
    }
    Ok(Value::bool(is_macro_object(&args[0])))
}

/// `(commandp FUNCTION &optional FOR-CALL-INTERACTIVELY)` -- return t if
/// FUNCTION is an interactive command.
///
/// In our simplified VM, any callable value (lambda, subr, bytecode) is
/// treated as a potential command.  A more complete implementation would
/// check for an `interactive` declaration.
pub(crate) fn builtin_commandp(args: Vec<Value>) -> EvalResult {
    expect_min_args("commandp", &args, 1)?;
    expect_max_args("commandp", &args, 2)?;
    Ok(Value::bool(args[0].is_function()))
}

/// `(func-arity FUNCTION)` -- return (MIN . MAX) for any callable.
///
/// Works for lambdas (reads `LambdaParams`), byte-code (reads `params`),
/// and subrs (returns `(0 . many)` as a conservative default).
pub(crate) fn builtin_func_arity(args: Vec<Value>) -> EvalResult {
    expect_args("func-arity", &args, 1)?;
    if super::autoload::is_autoload_value(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        ));
    }
    match &args[0] {
        Value::Lambda(_) => {
            let ld = args[0].get_lambda_data().unwrap();
            let min = ld.params.min_arity();
            let max = ld.params.max_arity();
            Ok(arity_cons(min, max))
        }
        Value::ByteCode(_) => {
            let bc = args[0].get_bytecode_data().unwrap();
            let min = bc.params.min_arity();
            let max = bc.params.max_arity();
            Ok(arity_cons(min, max))
        }
        Value::Subr(id) => Ok(subr_arity_value(resolve_sym(*id))),
        Value::Macro(_) => {
            let ld = args[0].get_lambda_data().unwrap();
            let min = ld.params.min_arity();
            let max = ld.params.max_arity();
            Ok(arity_cons(min, max))
        }
        other => Err(signal("invalid-function", vec![*other])),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "subr_info_test.rs"]
mod tests;
