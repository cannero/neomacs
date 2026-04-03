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
