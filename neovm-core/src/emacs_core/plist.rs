//! Cons-list plist helpers shared across overlays, strings, images, and
//! (after P2) symbols.
//!
//! Mirrors GNU `plist-get` / `plist-put` / `plist-member` semantics
//! (`fns.c`). Comparison uses `eq` (via `eq_value`) as GNU does.

use crate::emacs_core::value::{eq_value, Value};

/// Walk `plist` looking for `prop`. Returns the associated value or None.
/// Matches GNU `Fplist_get` when keys compare by eq.
pub fn plist_get(plist: Value, prop: &Value) -> Option<Value> {
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
        if eq_value(&key, prop) {
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
pub fn plist_put(plist: Value, prop: Value, value: Value) -> (Value, bool) {
    // Empty plist: create a fresh two-element list.
    if !plist.is_cons() {
        let changed = !value.is_nil();
        return (Value::list(vec![prop, value]), changed);
    }
    let mut tail = plist;
    let mut last_value_cell: Option<Value> = None;
    loop {
        if !tail.is_cons() {
            // End of list — key not found; append after last_value_cell.
            let new_tail = Value::cons(prop, Value::cons(value, Value::NIL));
            if let Some(lvc) = last_value_cell {
                lvc.set_cdr(new_tail);
            }
            return (plist, !value.is_nil());
        }
        let key = tail.cons_car();
        let rest = tail.cons_cdr();
        if !rest.is_cons() {
            // Malformed plist (odd length) — append at the end.
            let new_tail = Value::cons(prop, Value::cons(value, Value::NIL));
            if let Some(lvc) = last_value_cell {
                lvc.set_cdr(new_tail);
            }
            return (plist, !value.is_nil());
        }
        if eq_value(&key, &prop) {
            let changed = !eq_value(&rest.cons_car(), &value);
            rest.set_car(value);
            return (plist, changed);
        }
        last_value_cell = Some(rest);
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
