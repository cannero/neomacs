//! Cons-list plist helpers shared across overlays, strings, images, and
//! (after P2) symbols.
//!
//! Mirrors GNU `plist-get` / `plist-put` / `plist-member` semantics
//! (`fns.c`). Comparison uses `eq` (via `eq_value`) as GNU does.

use crate::emacs_core::error::{Flow, signal};
use crate::emacs_core::eval::{
    push_scratch_gc_root, restore_scratch_gc_roots, save_scratch_gc_roots,
};
use crate::emacs_core::value::{Value, eq_value, eq_value_swp};

fn plist_entry(prop: Value, value: Value, tail: Value) -> Value {
    let saved = save_scratch_gc_roots();
    push_scratch_gc_root(prop);
    push_scratch_gc_root(value);
    push_scratch_gc_root(tail);

    let value_cell = Value::cons(value, tail);
    push_scratch_gc_root(value_cell);
    let entry = Value::cons(prop, value_cell);

    restore_scratch_gc_roots(saved);
    entry
}

/// Walk `plist` looking for `prop`. Returns the associated value or None.
/// Matches GNU `Fplist_get` when keys compare by eq.
pub fn plist_get(plist: Value, prop: &Value) -> Option<Value> {
    plist_get_swp(plist, prop, false)
}

/// Walk `plist` looking for `prop`, using GNU's symbol-with-position aware
/// `EQ` semantics when `symbols_with_pos_enabled` is true.
pub fn plist_get_swp(plist: Value, prop: &Value, symbols_with_pos_enabled: bool) -> Option<Value> {
    let mut tail = plist;
    loop {
        if !tail.is_cons() {
            return None;
        }
        let key = tail.cons_car();
        let rest = tail.cons_cdr();
        if !rest.is_cons() {
            return None;
        }
        if eq_value_swp(&key, prop, symbols_with_pos_enabled) {
            return Some(rest.cons_car());
        }
        tail = rest.cons_cdr();
    }
}

/// Put `value` under `prop` in `plist`. If `prop` is already in the list,
/// mutate the existing value cell in place (matching GNU `Fplist_put`).
/// Otherwise append `(prop value)` to the end of the list (also matching
/// GNU, which walks to the tail and splices). Returns `(new_plist, changed)`
/// where `changed` indicates whether the effective binding changed (for
/// modification-tick bookkeeping).
///
/// On a malformed plist (walk runs off a non-cons non-nil tail), signals
/// `wrong-type-argument plistp plist`. Matches GNU `Fplist_put`
/// (`fns.c:2703-2727`).
pub fn plist_put(plist: Value, prop: Value, value: Value) -> Result<(Value, bool), Flow> {
    plist_put_swp(plist, prop, value, false)
}

/// `plist_put` variant whose key comparison mirrors GNU `EQ` while
/// `symbols-with-pos-enabled` is non-nil.
pub fn plist_put_swp(
    plist: Value,
    prop: Value,
    value: Value,
    symbols_with_pos_enabled: bool,
) -> Result<(Value, bool), Flow> {
    // Empty plist: create a fresh two-element list.
    if !plist.is_cons() {
        if !plist.is_nil() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("plistp"), plist],
            ));
        }
        let changed = !value.is_nil();
        return Ok((plist_entry(prop, value, Value::NIL), changed));
    }
    let mut tail = plist;
    let mut last_value_cell: Option<Value> = None;
    loop {
        if !tail.is_cons() {
            // End of walk. If it's nil, append. If not, malformed plist.
            if !tail.is_nil() {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("plistp"), plist],
                ));
            }
            // Append (prop value) to the tail of `plist`.
            let new_tail = plist_entry(prop, value, Value::NIL);
            if let Some(lvc) = last_value_cell {
                lvc.set_cdr(new_tail);
            }
            return Ok((plist, !value.is_nil()));
        }
        let key = tail.cons_car();
        let rest = tail.cons_cdr();
        if !rest.is_cons() {
            // Odd-length plist (non-cons tail after key). Signal as malformed.
            if !rest.is_nil() {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("plistp"), plist],
                ));
            }
            // rest is nil — odd-length plist. GNU treats as malformed too — signal.
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("plistp"), plist],
            ));
        }
        if eq_value_swp(&key, &prop, symbols_with_pos_enabled) {
            let changed = !eq_value_swp(&rest.cons_car(), &value, symbols_with_pos_enabled);
            rest.set_car(value);
            return Ok((plist, changed));
        }
        last_value_cell = Some(rest);
        tail = rest.cons_cdr();
    }
}

/// Validate that `plist` is a proper plist (NIL or an even-length cons
/// chain with a NIL tail). Signals `(wrong-type-argument plistp plist)`
/// on any malformed tail.
///
/// Used by callers that must fail on a malformed plist BEFORE performing
/// unrelated side effects (e.g. allocating a registration ID), so the
/// error path leaves no partial state behind. GNU does equivalent
/// validation at the top of many plist-mutating operations.
pub fn plist_check(plist: Value) -> Result<(), Flow> {
    let mut tail = plist;
    loop {
        if tail.is_nil() {
            return Ok(());
        }
        if !tail.is_cons() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("plistp"), plist],
            ));
        }
        let rest = tail.cons_cdr();
        if !rest.is_cons() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("plistp"), plist],
            ));
        }
        tail = rest.cons_cdr();
    }
}

/// Return the sub-list of `plist` starting at the first match for `prop`,
/// or NIL if not found. Matches GNU `Fplist_member`.
pub fn plist_member(plist: Value, prop: &Value) -> Value {
    let mut tail = plist;
    loop {
        if !tail.is_cons() {
            return Value::NIL;
        }
        let key = tail.cons_car();
        let rest = tail.cons_cdr();
        if !rest.is_cons() {
            return Value::NIL;
        }
        if eq_value(&key, prop) {
            return tail;
        }
        tail = rest.cons_cdr();
    }
}
