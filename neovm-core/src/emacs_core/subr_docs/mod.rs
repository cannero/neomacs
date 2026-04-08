//! Documentation strings for built-in (subr) functions.
//!
//! Phase A of the substitute-command-keys-audit-v5 R5 plan
//! (Option A inline storage). Each entry is a `(name, doc)` pair
//! lifted verbatim from a GNU Emacs `DEFUN ("name", ..., doc: /*
//! TEXT */)` block in `src/*.c`. Strings live in `.rodata` and are
//! looked up by name from `function_doc_or_error` and
//! `builtin_internal_subr_documentation`.
//!
//! ## Architecture
//!
//! - `gnu_table.rs` is **auto-generated** by
//!   `scripts/extract_gnu_defun_docs.py` from upstream GNU's
//!   `src/*.c`. To refresh, run the script against an updated GNU
//!   mirror — the diff is mechanical and reviewable.
//! - `lookup(name)` does a linear scan over the table. The table is
//!   ~1,700 entries today, lookups happen rarely (only on
//!   `(documentation 'foo)` queries), and the doc-query path is not
//!   on any hot loop. Linear scan is fine; if it ever shows up in a
//!   profile, swap to `phf::Map` or sorted-array binary search
//!   without changing the call-site signature.
//!
//! ## Why no `SubrObj.doc` field
//!
//! Storing the doc as a `&'static str` field on `SubrObj` would
//! save ~10 ns per query (one cache-line load instead of a linear
//! scan), but reading the field from a `Value` requires
//! `unsafe { &*(ptr as *const SubrObj) }`. The user explicitly
//! asked to avoid `unsafe`. The 10 ns difference is invisible for
//! doc queries (which run at ~10/sec, not 10⁹/sec), so the central
//! table is the better trade.
//!
//! ## Why grave-quoted strings (not curly)
//!
//! GNU's `DEFUN doc:` text uses ASCII grave accents (`` ` `` and
//! `'`) for quotes. `substitute-command-keys` (in `lisp/help.el`)
//! converts them to ‘ ’ at display time per the user's
//! `text-quoting-style`. Pre-substituting here would lock in
//! `'curve` regardless of preference (audit v5 §2.4).

pub(crate) mod gnu_table;

/// Look up the doc string for a subr by name. Returns `None` if no
/// entry exists. The returned `&'static str` points into `.rodata`.
///
/// O(n) linear scan over `gnu_table::GNU_SUBR_DOCS`. Called only on
/// documentation-query paths, never from `eval`/`funcall`/dispatch.
#[inline]
pub(crate) fn lookup(name: &str) -> Option<&'static str> {
    gnu_table::GNU_SUBR_DOCS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, doc)| *doc)
}
