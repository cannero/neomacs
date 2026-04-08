use super::super::intern::intern;
use super::*;

#[test]
fn intern_creates_symbol() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    ob.intern("foo");
    assert!(ob.intern_soft("foo").is_some());
    assert!(ob.intern_soft("bar").is_none());
}

#[test]
fn symbol_value_cell() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    assert!(!ob.boundp("x"));
    ob.set_symbol_value("x", Value::fixnum(42));
    assert!(ob.boundp("x"));
    assert_eq!(ob.symbol_value("x").unwrap().as_int(), Some(42));
}

#[test]
fn symbol_function_cell() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    assert!(!ob.fboundp("f"));
    let start_epoch = ob.function_epoch();
    ob.set_symbol_function("f", Value::subr(intern("+")));
    assert!(ob.fboundp("f"));
    assert!(ob.function_epoch() > start_epoch);
    let after_set_epoch = ob.function_epoch();
    ob.fmakunbound("f");
    assert!(!ob.fboundp("f"));
    assert!(ob.function_epoch() > after_set_epoch);
}

#[test]
fn fmakunbound_masks_builtin_fallback_name() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    let start_epoch = ob.function_epoch();
    ob.fmakunbound("car");
    assert!(ob.is_function_unbound("car"));
    assert!(!ob.fboundp("car"));
    assert!(ob.symbol_function("car").is_none());
    assert!(ob.function_epoch() > start_epoch);

    ob.set_symbol_function("car", Value::subr(intern("car")));
    assert!(!ob.is_function_unbound("car"));
    assert!(ob.fboundp("car"));
}

#[test]
fn symbol_properties() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    ob.put_property("foo", "doc", Value::string("A function."));
    assert_eq!(
        ob.get_property("foo", "doc").unwrap().as_str(),
        Some("A function.")
    );
}

#[test]
fn special_flag() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    assert!(!ob.is_special("x"));
    ob.make_special("x");
    assert!(ob.is_special("x"));
}

#[test]
fn indirect_function_follows_chain() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    ob.set_symbol_function("real-fn", Value::subr(intern("+")));
    // alias -> real-fn
    ob.set_symbol_function("alias", Value::symbol(intern("real-fn")));
    let resolved = ob.indirect_function("alias").unwrap();
    assert!(
        resolved
            .as_subr_id()
            .map_or(false, |id| resolve_sym(id) == "+")
    );
}

#[test]
fn t_and_nil_are_preinterned() {
    crate::test_utils::init_test_tracing();
    let ob = Obarray::new();
    assert!(ob.is_constant("t"));
    assert!(ob.is_constant("nil"));
    assert!(ob.is_constant(":keyword"));
    assert!(ob.is_special("t"));
    assert!(ob.is_special("nil"));
}

#[test]
fn interning_keyword_materializes_gnu_self_evaluating_symbol_state() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    ob.intern(":vm-keyword");
    assert!(ob.is_constant(":vm-keyword"));
    assert!(ob.is_special(":vm-keyword"));
    assert_eq!(
        ob.symbol_value(":vm-keyword"),
        Some(&Value::keyword(":vm-keyword"))
    );
}

#[test]
fn makunbound_doesnt_touch_constants() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    ob.makunbound("t");
    assert!(ob.boundp("t")); // t is constant, can't unbind
}

#[test]
fn canonical_id_mutators_keep_symbol_globally_interned() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    let sym = intern("vm-ghost");

    ob.set_symbol_value_id(sym, Value::fixnum(1));
    assert!(ob.intern_soft("vm-ghost").is_some());
    assert!(ob.all_symbols().contains(&"vm-ghost"));

    ob.put_property_id(sym, intern("vm-prop"), Value::fixnum(2));
    assert_eq!(
        ob.get_property("vm-ghost", "vm-prop"),
        Some(&Value::fixnum(2))
    );

    ob.set_symbol_function_id(sym, Value::subr(intern("+")));
    assert!(ob.fboundp("vm-ghost"));

    ob.make_special_id(sym);
    assert!(ob.is_special("vm-ghost"));
}

#[test]
fn replace_symbol_plist_id_overwrites_existing_entries() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    let sym = intern("vm-plist");

    ob.put_property_id(sym, intern("stale"), Value::fixnum(1));
    ob.replace_symbol_plist_id(sym, [(intern("fresh"), Value::fixnum(2))]);

    assert_eq!(ob.get_property("vm-plist", "stale"), None);
    assert_eq!(
        ob.get_property("vm-plist", "fresh"),
        Some(&Value::fixnum(2))
    );
}

#[test]
fn for_each_value_cell_mut_updates_plain_and_buffer_local_values() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();

    ob.set_symbol_value("plain", Value::fixnum(1));
    ob.set_symbol_value("buffer-local", Value::fixnum(2));
    ob.make_buffer_local("buffer-local", true);
    ob.set_symbol_function("callable", Value::fixnum(99));
    ob.put_property("plist-holder", "meta", Value::fixnum(77));

    ob.for_each_value_cell_mut(|value| {
        if let Some(n) = value.as_fixnum() {
            *value = Value::fixnum(n + 10);
        }
    });

    assert_eq!(ob.symbol_value("plain"), Some(&Value::fixnum(11)));
    assert_eq!(ob.symbol_value("buffer-local"), Some(&Value::fixnum(12)));
    assert_eq!(ob.symbol_function("callable"), Some(&Value::fixnum(99)));
    assert_eq!(
        ob.get_property("plist-holder", "meta"),
        Some(&Value::fixnum(77))
    );
}

// ===========================================================================
// Symbol-redirect refactor — Phase 1 sanity tests
// ===========================================================================
//
// These cover the new SymbolRedirect / SymbolFlags / SymbolVal machinery
// introduced in `drafts/symbol-redirect-plan.md` Step 1. They do NOT yet
// exercise LOCALIZED or FORWARDED dispatch — those land in later phases.

/// `LispSymbol::new` produces a fresh PLAINVAL symbol with NIL in its
/// value cell. Mirrors GNU `init_symbol` (`alloc.c:3659-3673`).
#[test]
fn fresh_lisp_symbol_is_plainval_nil() {
    crate::test_utils::init_test_tracing();
    let id = intern("phase1-fresh");
    let sym = LispSymbol::new(id);
    assert_eq!(sym.redirect(), SymbolRedirect::Plainval);
    assert_eq!(sym.flags.trapped_write(), SymbolTrappedWrite::Untrapped);
    assert_eq!(sym.flags.interned(), SymbolInterned::Uninterned);
    assert!(!sym.flags.declared_special());
    assert_eq!(sym.plain(), Value::NIL);
}

/// `Obarray::set_symbol_value` keeps the legacy `SymbolValue::Plain`
/// representation in sync with the new `flags + val` shape during the
/// Phase 1 transition. Once both representations agree, Phase 4-10 can
/// delete the legacy enum without behavior drift.
#[test]
fn plainval_redirect_mirrors_legacy_value_field() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    ob.set_symbol_value("phase1-mirror", Value::fixnum(7));
    let id = intern("phase1-mirror");
    let sym = ob.get_by_id(id).expect("symbol just installed");
    assert_eq!(sym.redirect(), SymbolRedirect::Plainval);
    assert_eq!(sym.plain(), Value::fixnum(7));
    match &sym.value {
        SymbolValue::Plain(Some(v)) => assert_eq!(*v, Value::fixnum(7)),
        other => panic!("legacy value out of sync: {:?}", other),
    }
}

/// `make_alias` flips the redirect tag to `Varalias` AND keeps the
/// legacy enum in sync. Phase 3 of the refactor cuts the alias-following
/// hot path over to the redirect tag exclusively.
#[test]
fn varalias_redirect_mirrors_legacy_alias_field() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    let from_id = intern("phase1-alias-from");
    let to_id = intern("phase1-alias-to");
    ob.ensure_symbol_id(from_id);
    ob.ensure_symbol_id(to_id);
    ob.make_alias(from_id, to_id);
    let sym = ob.get_by_id(from_id).expect("symbol just installed");
    assert_eq!(sym.redirect(), SymbolRedirect::Varalias);
    assert_eq!(sym.alias_target(), to_id);
    match &sym.value {
        SymbolValue::Alias(target) => assert_eq!(*target, to_id),
        other => panic!("legacy value out of sync: {:?}", other),
    }
}

/// Pre-interned `t` and `nil` carry their canonical values in both the
/// legacy and the new shape. Mirrors GNU's setup of `Qnil` / `Qt` in
/// `alloc.c::init_alloc_once`.
#[test]
fn t_and_nil_have_consistent_redirect_state() {
    crate::test_utils::init_test_tracing();
    let ob = Obarray::new();
    let t = ob.get_by_id(intern("t")).expect("t pre-interned");
    let nil = ob.get_by_id(intern("nil")).expect("nil pre-interned");
    assert_eq!(t.redirect(), SymbolRedirect::Plainval);
    assert_eq!(t.plain(), Value::T);
    assert!(t.constant);
    assert_eq!(nil.redirect(), SymbolRedirect::Plainval);
    assert_eq!(nil.plain(), Value::NIL);
    assert!(nil.constant);
}

/// SymbolFlags packs into a single byte (matches GNU's bit layout).
#[test]
fn symbol_flags_pack_into_one_byte() {
    crate::test_utils::init_test_tracing();
    assert_eq!(std::mem::size_of::<SymbolFlags>(), 1);
}

// Phase 3 — VARALIAS via the new redirect tag.

/// `indirect_variable_id` walks a single-hop alias chain to its
/// terminus. Mirrors GNU `indirect_variable` (`src/data.c:1284-1301`).
#[test]
fn indirect_variable_id_follows_chain() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    let a = intern("phase3-alias-a");
    let b = intern("phase3-alias-b");
    let c = intern("phase3-alias-c");
    ob.ensure_symbol_id(a);
    ob.ensure_symbol_id(b);
    ob.ensure_symbol_id(c);
    // a → b → c
    ob.make_alias(a, b);
    ob.make_alias(b, c);
    assert_eq!(ob.indirect_variable_id(a), Some(c));
    assert_eq!(ob.indirect_variable_id(b), Some(c));
    assert_eq!(ob.indirect_variable_id(c), Some(c));
}

/// `indirect_variable_id` returns `None` on a cycle, detected via
/// Floyd's tortoise/hare. The cycle protection mirrors the cycle
/// guard in GNU's `find_symbol_value` `goto start` loop
/// (`src/data.c:1593-1595`).
#[test]
fn indirect_variable_id_detects_cycle() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    let a = intern("phase3-cycle-a");
    let b = intern("phase3-cycle-b");
    ob.ensure_symbol_id(a);
    ob.ensure_symbol_id(b);
    // a → b → a (cycle)
    ob.make_alias(a, b);
    ob.make_alias(b, a);
    assert_eq!(ob.indirect_variable_id(a), None);
    assert_eq!(ob.indirect_variable_id(b), None);
}

/// `make_variable_alias` rejects an attempt that would create a cycle.
/// Mirrors GNU `Fdefvaralias`'s "base chain looking for new_alias"
/// guard (`src/eval.c:631-726`).
#[test]
fn make_variable_alias_rejects_cycle() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    let a = intern("phase3-malias-a");
    let b = intern("phase3-malias-b");
    let c = intern("phase3-malias-c");
    ob.ensure_symbol_id(a);
    ob.ensure_symbol_id(b);
    ob.ensure_symbol_id(c);
    // a → b → c, then try to make c → a (cycle).
    ob.make_variable_alias(a, b).expect("a → b ok");
    ob.make_variable_alias(b, c).expect("b → c ok");
    let err = ob.make_variable_alias(c, a).unwrap_err();
    assert_eq!(err, MakeAliasError::Cycle);
}

/// `make_variable_alias` rejects an attempt to alias a constant.
#[test]
fn make_variable_alias_rejects_constant() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    let target = intern("phase3-malias-target");
    let nil_id = intern("nil"); // pre-interned constant
    ob.ensure_symbol_id(target);
    let err = ob.make_variable_alias(nil_id, target).unwrap_err();
    assert_eq!(err, MakeAliasError::Constant);
}

/// After `make_variable_alias`, both symbols are marked
/// `declared_special` (special).
#[test]
fn make_variable_alias_marks_both_special() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    let a = intern("phase3-malias-special-a");
    let b = intern("phase3-malias-special-b");
    ob.ensure_symbol_id(a);
    ob.ensure_symbol_id(b);
    ob.make_variable_alias(a, b).expect("a → b ok");
    assert!(ob.is_special_id(a));
    assert!(ob.is_special_id(b));
    assert!(ob.is_alias_id(a));
    assert!(!ob.is_alias_id(b));
}

// Phase 4 — LOCALIZED read path with BLV cache.

/// `make_symbol_localized` allocates a BLV with `defcell == valcell`,
/// flips the redirect to LOCALIZED, and stores the BLV pointer in
/// `val.blv`. Mirrors GNU `make_blv` (`src/data.c:2112-2140`).
#[test]
fn make_symbol_localized_allocates_blv() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    let id = intern("phase4-localized-x");
    ob.make_symbol_localized(id, Value::fixnum(42));
    let sym = ob.get_by_id(id).expect("symbol installed");
    assert_eq!(sym.redirect(), SymbolRedirect::Localized);
    let blv = ob.blv(id).expect("BLV pointer");
    // defcell == valcell initially.
    assert_eq!(blv.defcell, blv.valcell);
    // (sym . default)
    assert_eq!(blv.defcell.cons_cdr(), Value::fixnum(42));
    assert!(blv.where_buf.is_nil());
    assert!(!blv.found);
    assert!(!blv.local_if_set);
}

/// `find_symbol_value_in_buffer` for a LOCALIZED symbol with no
/// per-buffer binding returns the default. Mirrors GNU
/// `find_symbol_value` LOCALIZED arm.
#[test]
fn localized_returns_default_when_no_buffer_local() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    let id = intern("phase4-default-x");
    ob.make_symbol_localized(id, Value::fixnum(7));
    let buf_value = Value::NIL; // pretend no current buffer
    let alist = Value::NIL; // empty alist
    let v = ob.find_symbol_value_in_buffer(id, None, buf_value, alist, None, 0, None);
    assert_eq!(v, Some(Value::fixnum(7)));
}

/// `find_symbol_value_in_buffer` swaps the BLV cache to the buffer's
/// `local_var_alist` entry when one exists. Mirrors GNU
/// `swap_in_symval_forwarding` (`src/data.c:1539-1571`).
#[test]
fn localized_swap_in_reads_buffer_local_value() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    let id = intern("phase4-buflocal-x");
    ob.make_symbol_localized(id, Value::fixnum(0));
    // Build a fake buffer alist `((phase4-buflocal-x . 99))` and a
    // fake buffer value (we use a fixnum as a sentinel for "buffer A"
    // since the test doesn't need a real BufferManager).
    let cell = Value::cons(Value::from_sym_id(id), Value::fixnum(99));
    let alist = Value::cons(cell, Value::NIL);
    let buf_a = Value::fixnum(1);
    let v = ob.find_symbol_value_in_buffer(id, None, buf_a, alist, None, 0, None);
    assert_eq!(v, Some(Value::fixnum(99)));
    // The cache now records `where_buf == buf_a` and `found == true`.
    let blv = ob.blv(id).expect("BLV");
    assert_eq!(blv.where_buf, buf_a);
    assert!(blv.found);
}

/// Switching buffers reloads the BLV cache. A symbol with a binding
/// in buffer A returns A's value when current; switching to buffer B
/// (with no binding) returns the default.
#[test]
fn localized_blv_cache_invalidated_on_buffer_switch() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    let id = intern("phase4-switch-x");
    ob.make_symbol_localized(id, Value::fixnum(0));

    // Buffer A has a binding (sym . 42).
    let buf_a = Value::fixnum(1);
    let alist_a = Value::cons(
        Value::cons(Value::from_sym_id(id), Value::fixnum(42)),
        Value::NIL,
    );
    let v_a = ob.find_symbol_value_in_buffer(id, None, buf_a, alist_a, None, 0, None);
    assert_eq!(v_a, Some(Value::fixnum(42)));

    // Buffer B has no binding for this symbol → default.
    let buf_b = Value::fixnum(2);
    let alist_b = Value::NIL;
    let v_b = ob.find_symbol_value_in_buffer(id, None, buf_b, alist_b, None, 0, None);
    assert_eq!(v_b, Some(Value::fixnum(0)));
    let blv = ob.blv(id).expect("BLV");
    assert_eq!(blv.where_buf, buf_b);
    assert!(!blv.found);
}

// Phase 5 — LOCALIZED write path.

/// `set_internal_localized` with `local_if_set = true` and
/// `bindflag = Set` auto-creates a per-buffer binding when none
/// exists. Mirrors GNU set_internal lines 1687-1763 (`src/data.c`).
#[test]
fn set_localized_creates_buffer_local_when_local_if_set() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    let id = intern("phase5-autolocal-x");
    ob.make_symbol_localized(id, Value::fixnum(0));
    ob.set_blv_local_if_set(id, true);

    let buf = Value::fixnum(1);
    let mut alist = Value::NIL;
    alist = ob.set_internal_localized(
        id,
        Value::fixnum(42),
        buf,
        alist,
        SetInternalBind::Set,
        false, // let_shadows: false
    );
    // The alist now has one entry: (sym . 42).
    assert!(alist.is_cons());
    let head = alist.cons_car();
    assert!(head.is_cons());
    assert_eq!(head.cons_car(), Value::from_sym_id(id));
    assert_eq!(head.cons_cdr(), Value::fixnum(42));
    // Read it back via the buffer-aware path.
    let v = ob.find_symbol_value_in_buffer(id, None, buf, alist, None, 0, None);
    assert_eq!(v, Some(Value::fixnum(42)));
}

/// When `local_if_set` is false, `set_internal_localized` writes the
/// default cell instead of auto-creating a per-buffer binding.
#[test]
fn set_localized_writes_default_when_no_local_if_set() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    let id = intern("phase5-noautolocal-x");
    ob.make_symbol_localized(id, Value::fixnum(0));
    // local_if_set stays false (default).

    let buf = Value::fixnum(1);
    let alist = Value::NIL;
    let new_alist = ob.set_internal_localized(
        id,
        Value::fixnum(99),
        buf,
        alist,
        SetInternalBind::Set,
        false,
    );
    // Alist unchanged (no per-buffer binding created).
    assert_eq!(new_alist, Value::NIL);
    // The default value was updated to 99.
    let blv = ob.blv(id).expect("BLV");
    assert_eq!(blv.defcell.cons_cdr(), Value::fixnum(99));
}

/// When `let_shadows == true`, the auto-create branch is suppressed.
/// Mirrors GNU's `let_shadows_buffer_binding_p` guard.
#[test]
fn set_localized_does_not_create_when_let_shadows() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    let id = intern("phase5-letshadow-x");
    ob.make_symbol_localized(id, Value::fixnum(0));
    ob.set_blv_local_if_set(id, true);

    let buf = Value::fixnum(1);
    let alist = Value::NIL;
    let new_alist = ob.set_internal_localized(
        id,
        Value::fixnum(13),
        buf,
        alist,
        SetInternalBind::Set,
        true, // let_shadows: true
    );
    // No per-buffer binding created; defcell got the write.
    assert_eq!(new_alist, Value::NIL);
    let blv = ob.blv(id).expect("BLV");
    assert_eq!(blv.defcell.cons_cdr(), Value::fixnum(13));
}

/// `set_internal_localized` with `bindflag = Bind` (let-binding's
/// initial assignment) never auto-creates a per-buffer binding,
/// even when `local_if_set` is true. The let unwind machinery in
/// Phase 7 handles restoration.
#[test]
fn set_localized_bind_never_auto_creates() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    let id = intern("phase5-bind-x");
    ob.make_symbol_localized(id, Value::fixnum(0));
    ob.set_blv_local_if_set(id, true);

    let buf = Value::fixnum(1);
    let alist = Value::NIL;
    let new_alist = ob.set_internal_localized(
        id,
        Value::fixnum(7),
        buf,
        alist,
        SetInternalBind::Bind, // let-binding initial assignment
        false,
    );
    assert_eq!(new_alist, Value::NIL);
    let blv = ob.blv(id).expect("BLV");
    assert_eq!(blv.defcell.cons_cdr(), Value::fixnum(7));
}

// Phase 8a — FORWARDED via BUFFER_OBJFWD slot.

/// `install_buffer_objfwd` flips the redirect to `Forwarded` and
/// stores the descriptor pointer in `val.fwd`. Mirrors GNU
/// `defvar_per_buffer` (`buffer.c:4990-5012`).
#[test]
fn install_buffer_objfwd_flips_redirect() {
    crate::test_utils::init_test_tracing();
    use crate::emacs_core::forward::alloc_buffer_objfwd;
    let mut ob = Obarray::new();
    let id = intern("phase8-fwd-x");
    let predicate = intern("phase8-stringp"); // dummy
    let fwd = alloc_buffer_objfwd(0, -1, predicate, Value::fixnum(42));
    ob.install_buffer_objfwd(id, fwd);
    let sym = ob.get_by_id(id).expect("symbol installed");
    assert_eq!(sym.redirect(), SymbolRedirect::Forwarded);
    assert!(sym.flags.declared_special());
    assert!(sym.special);
}

/// `find_symbol_value_in_buffer` for a FORWARDED `BUFFER_OBJFWD`
/// reads from `current_buffer.slots[offset]`. Mirrors GNU
/// `do_symval_forwarding` (`data.c:1330-1352`) for the
/// `Lisp_Buffer_Objfwd` arm.
#[test]
fn find_symbol_value_forwarded_reads_buffer_slot() {
    crate::test_utils::init_test_tracing();
    use crate::emacs_core::forward::alloc_buffer_objfwd;
    let mut ob = Obarray::new();
    let id = intern("phase8-fwd-slot-x");
    let predicate = intern("phase8-stringp");
    let fwd = alloc_buffer_objfwd(3, -1, predicate, Value::fixnum(0));
    ob.install_buffer_objfwd(id, fwd);

    // Synthetic buffer slot table.
    let mut slots = vec![Value::NIL; 10];
    slots[3] = Value::fixnum(99);
    let v = ob.find_symbol_value_in_buffer(
        id,
        None,
        Value::NIL,
        Value::NIL,
        Some(&slots),
        0,
        None,
    );
    assert_eq!(v, Some(Value::fixnum(99)));
}

/// When no current-buffer slot table is provided (e.g. during
/// startup before any buffer exists), the FORWARDED arm returns
/// the forwarder's default.
#[test]
fn find_symbol_value_forwarded_returns_default_without_buffer() {
    crate::test_utils::init_test_tracing();
    use crate::emacs_core::forward::alloc_buffer_objfwd;
    let mut ob = Obarray::new();
    let id = intern("phase8-fwd-default-x");
    let predicate = intern("phase8-stringp");
    let fwd = alloc_buffer_objfwd(5, -1, predicate, Value::fixnum(7));
    ob.install_buffer_objfwd(id, fwd);
    let v = ob.find_symbol_value_in_buffer(id, None, Value::NIL, Value::NIL, None, 0, None);
    assert_eq!(v, Some(Value::fixnum(7)));
}

/// `Obarray::clone` deep-copies the BLV pool and remaps symbol
/// pointers, so a cloned obarray reads independently from the
/// original.
#[test]
fn clone_obarray_deep_copies_blvs() {
    crate::test_utils::init_test_tracing();
    let mut ob = Obarray::new();
    let id = intern("phase4-clone-x");
    ob.make_symbol_localized(id, Value::fixnum(11));
    let cloned = ob.clone();
    // Both obarrays read the same default initially.
    let v1 = ob.find_symbol_value(id);
    let v2 = cloned.find_symbol_value(id);
    assert_eq!(v1, Some(Value::fixnum(11)));
    assert_eq!(v2, Some(Value::fixnum(11)));
    // The cloned obarray's BLV pointer is a fresh allocation.
    let blv1 = ob.blv(id).expect("blv1");
    let blv2 = cloned.blv(id).expect("blv2");
    assert!(
        std::ptr::addr_of!(*blv1) != std::ptr::addr_of!(*blv2),
        "cloned BLV must be a distinct allocation"
    );
}

#[test]
fn uninterned_keyword_and_nil_names_are_not_canonical_constants() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    let nil_id = crate::emacs_core::intern::intern_uninterned("nil");
    let kw_id = crate::emacs_core::intern::intern_uninterned(":vm-k");

    assert!(!eval.obarray().is_constant_id(nil_id));
    assert!(!eval.obarray().is_constant_id(kw_id));

    eval.obarray_mut()
        .set_symbol_function_id(nil_id, Value::subr(intern("+")));
    assert!(eval.obarray().symbol_function_id(nil_id).is_some());
    assert!(eval.obarray().intern_soft("nil").is_some());
    assert!(eval.obarray().intern_soft(":vm-k").is_none());
}
