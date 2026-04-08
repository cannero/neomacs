//! Documentation strings for built-in (DEFVAR_*) variables.
//!
//! Phase A7-A10 of the substitute-command-keys-audit-v5 R5 plan.
//! Companion to `subr_docs/' (which holds DEFUN docs). Each entry
//! is a `(name, doc)' pair lifted verbatim from a GNU Emacs
//! `DEFVAR_LISP("name", Vsymbol, doc: /* TEXT */)' block (or
//! DEFVAR_INT/BOOL/KBOARD/PER_BUFFER variant) in `src/*.c'.
//!
//! ## Architecture
//!
//! - `gnu_table.rs' is **auto-generated** by
//!   `scripts/extract_gnu_defvar_docs.py' from upstream GNU's
//!   `src/*.c'. To refresh, run the script against an updated GNU
//!   mirror.
//! - `lookup(name)' does a linear scan over the table. Lookups
//!   happen only on `(documentation-property 'foo
//!   'variable-documentation)' queries, which are user-initiated
//!   and rare. Linear scan is fine; ~820 entries today.
//!
//! ## Why grave-quoted strings (not curly)
//!
//! Same reason as `subr_docs/': GNU's `DEFVAR_* doc:' text uses
//! ASCII grave accents (`` ` `` and `'`). `substitute-command-keys'
//! converts them to curly quotes at display time per the user's
//! `text-quoting-style'. Pre-substituting here would lock in
//! `'curve' regardless of preference.
//!
//! ## Lookup precedence
//!
//! `documentation_property_plan' consults sources in this order:
//!   1. Symbol's `variable-documentation' plist property (set by
//!      Lisp `defvar' or by `Snarf-documentation' in GNU's case)
//!   2. `STARTUP_VARIABLE_DOC_STUBS' / `_STRING_PROPERTIES' (the
//!      legacy hand-typed tables, shrinking in Phase A10)
//!   3. `var_docs::lookup(name)' (this module — covers all
//!      upstream GNU DEFVAR_* variables)
//!   4. nil (no doc available)

pub(crate) mod gnu_table;

/// Look up the doc string for a built-in variable by name.
/// Returns `None` if no entry exists. The returned `&'static str`
/// points into `.rodata`.
///
/// O(n) linear scan over `gnu_table::GNU_VAR_DOCS`. Called only on
/// documentation-query paths, never from `eval`/`funcall`/dispatch.
#[inline]
pub(crate) fn lookup(name: &str) -> Option<&'static str> {
    gnu_table::GNU_VAR_DOCS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, doc)| *doc)
}
