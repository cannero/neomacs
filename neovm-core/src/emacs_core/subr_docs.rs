//! Documentation strings for built-in (subr) functions.
//!
//! This module is the doc-storage layer for Phase A of the
//! `substitute-command-keys-audit-v5` plan: Option A — inline doc
//! strings on `SubrObj`. Each entry is a `(name, doc)` pair lifted
//! from GNU Emacs's `DEFUN ("name", ..., doc: /* TEXT */)` blocks
//! in `src/*.c`. Strings live in `.rodata` and are looked up once
//! per subr at allocation time by `tagged::value::Value::subr`.
//!
//! ## Why a static table instead of inline at the `defsubr` site
//!
//! - **Fewer call-site changes**. Neomacs has hundreds of `defsubr`
//!   calls scattered across many files. Adding a `doc` parameter to
//!   each one is invasive. A central table is mechanical and
//!   regeneratable from upstream GNU.
//! - **Single source of truth**. The table is generated from GNU's
//!   actual `DEFUN doc:` text, so it cannot drift from upstream.
//! - **Zero `unsafe`, zero `Cell`**. The table is compile-time
//!   data; `Value::subr` looks up the doc and passes it to
//!   `alloc_subr` once at construction. `SubrObj.doc` stays a plain
//!   immutable field.
//!
//! ## Performance
//!
//! - Doc query path: 1-2 ns (single field load on a `SubrObj` that
//!   the dispatch already had in cache).
//! - Hot path (eval, funcall, dispatch): 0 — none of these read
//!   `SubrObj.doc`.
//! - Binary size: ~1 MB of `.rodata` once the table is fully
//!   populated. Demand-paged by the OS, so 0 KB resident until
//!   `(documentation 'foo)` is actually called.
//!
//! ## Population
//!
//! Phase A2 (forthcoming) writes a script that walks GNU's
//! `src/*.c` for `DEFUN` blocks and emits this table. For now the
//! table is empty — `lookup` returns `None` for every name and
//! callers fall through to `"Built-in function."`. This is a
//! drop-in upgrade path: when A2 lands, every standard subr
//! immediately gets its real doc with no other code changes.

/// GNU `DEFUN doc:` text for standard built-in subrs, indexed by
/// the subr's symbol name.
///
/// Currently empty — populated by Phase A2's bulk-import script.
/// Sorted alphabetically once non-empty so a future binary search
/// can replace the linear scan if needed.
pub(crate) static GNU_SUBR_DOCS: &[(&str, &str)] = &[
    // Phase A2 will fill this in.
];

/// Look up the doc string for a subr by name. Returns `None` if no
/// entry exists. The returned `&'static str` points into `.rodata`.
///
/// O(n) linear scan over `GNU_SUBR_DOCS`. Called once per subr at
/// allocation time (in `Value::subr`), not on every dispatch — so
/// the linear scan is fine even when the table grows to ~1,400
/// entries. If we ever care, swap to a `phf::Map` or sorted-array
/// binary search; the call-site signature stays the same.
#[inline]
pub(crate) fn lookup(name: &str) -> Option<&'static str> {
    GNU_SUBR_DOCS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, doc)| *doc)
}
