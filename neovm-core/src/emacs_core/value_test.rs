use super::*;
use crate::emacs_core::Expr;
use crate::emacs_core::marker::make_marker_value_with_id;

/// Helper: set up a temporary heap for tests that use Value constructors.
/// With the tagged-pointer runtime the test fallback heap is auto-created,
/// so this wrapper is now a simple pass-through.
fn with_test_heap<R>(f: impl FnOnce() -> R) -> R {
    f()
}

#[test]
fn value_constructors() {
    with_test_heap(|| {
        assert!(Value::NIL.is_nil());
        assert!(Value::T.is_truthy());
        assert!(Value::fixnum(42).is_integer());
        assert!(Value::make_float(3.14).is_float());
        assert!(Value::string("hello").is_string());
        assert!(Value::char('a').is_char());
        assert!(Value::symbol("foo").is_symbol());
        assert!(Value::keyword(":bar").is_keyword());
    });
}

#[test]
fn list_round_trip() {
    with_test_heap(|| {
        let lst = Value::list(vec![Value::fixnum(1), Value::fixnum(2), Value::fixnum(3)]);
        let vec = list_to_vec(&lst).unwrap();
        assert_eq!(vec.len(), 3);
    });
}

#[test]
fn eq_identity() {
    with_test_heap(|| {
        assert!(eq_value(&Value::NIL, &Value::NIL));
        assert!(eq_value(&Value::fixnum(42), &Value::fixnum(42)));
        assert!(!eq_value(&Value::fixnum(1), &Value::fixnum(2)));
        assert!(eq_value(&Value::char('a'), &Value::fixnum(97)));
        assert!(eq_value(&Value::fixnum(97), &Value::char('a')));
        assert!(eq_value(&Value::symbol("foo"), &Value::symbol("foo")));
    });
}

#[test]
fn keyword_identity_is_consistent_across_constructors() {
    with_test_heap(|| {
        let keyword_from_symbol_ctor = Value::symbol(":kw");
        let keyword_from_keyword_ctor = Value::keyword(":kw");

        // Value::symbol(":kw") auto-detects the colon and produces Keyword variant
        assert!(keyword_from_symbol_ctor.is_keyword());
        assert!(eq_value(
            &keyword_from_symbol_ctor,
            &keyword_from_keyword_ctor
        ));

        // Direct Value::Symbol(intern(":kw")) bypasses auto-detection and produces
        // a Symbol variant. In GNU Emacs, :kw and kw are different symbols —
        // Symbol and Keyword variants must NOT be eq.
        let legacy_symbol_variant = Value::symbol(intern(":kw"));
        assert!(!eq_value(&keyword_from_symbol_ctor, &legacy_symbol_variant));
        assert!(!equal_value(
            &keyword_from_symbol_ctor,
            &legacy_symbol_variant,
            0
        ));

        for test in [HashTableTest::Eq, HashTableTest::Eql, HashTableTest::Equal] {
            let left = keyword_from_symbol_ctor.to_hash_key(&test);
            let right = legacy_symbol_variant.to_hash_key(&test);
            assert_ne!(left, right);
        }
    });
}

#[test]
fn equal_structural() {
    with_test_heap(|| {
        let a = Value::list(vec![Value::fixnum(1), Value::fixnum(2)]);
        let b = Value::list(vec![Value::fixnum(1), Value::fixnum(2)]);
        assert!(equal_value(&a, &b, 0));
        assert!(!eq_value(&a, &b));
    });
}

#[test]
fn string_equality() {
    with_test_heap(|| {
        let a = Value::string("hello");
        let b = Value::string("hello");
        assert!(equal_value(&a, &b, 0));
        // eq compares ObjId identity — different allocations
        assert!(!eq_value(&a, &b));
    });
}

#[test]
fn marker_equal_ignores_internal_tracking_id() {
    with_test_heap(|| {
        let left = make_marker_value_with_id(None, Some(4), false, Some(1));
        let right = make_marker_value_with_id(None, Some(4), false, Some(2));
        let different = make_marker_value_with_id(None, Some(5), false, Some(1));

        assert!(equal_value(&left, &right, 0));
        assert!(!equal_value(&left, &different, 0));
    });
}

#[test]
fn closure_equal_is_structural() {
    with_test_heap(|| {
        let env_a = Value::list(vec![Value::cons(Value::symbol("n"), Value::fixnum(5))]);
        let env_b = Value::list(vec![Value::cons(Value::symbol("n"), Value::fixnum(5))]);
        let env_c = Value::list(vec![Value::cons(Value::symbol("n"), Value::fixnum(10))]);

        let make = |env| {
            Value::make_lambda(LambdaData {
                params: LambdaParams::simple(vec![intern("x")]),
                body: vec![Expr::List(vec![
                    Expr::Symbol(intern("+")),
                    Expr::Symbol(intern("n")),
                    Expr::Symbol(intern("x")),
                ])]
                .into(),
                env: Some(env),
                docstring: None,
                doc_form: None,
                interactive: None,
            })
        };

        let left = make(env_a);
        let same = make(env_b);
        let different = make(env_c);

        assert!(!eq_value(&left, &same));
        assert!(equal_value(&left, &same, 0));
        assert!(!equal_value(&left, &different, 0));
        assert_eq!(
            left.to_hash_key(&HashTableTest::Equal),
            same.to_hash_key(&HashTableTest::Equal)
        );
    });
}

#[test]
fn recursive_closure_equal_and_hash_are_structural() {
    with_test_heap(|| {
        let make_recursive = || {
            let binding = Value::cons(Value::symbol("f"), Value::NIL);
            let env = Value::list(vec![binding]);
            let closure = Value::make_lambda(LambdaData {
                params: LambdaParams::simple(vec![]),
                body: vec![Expr::Symbol(intern("f"))].into(),
                env: Some(env),
                docstring: None,
                doc_form: None,
                interactive: None,
            });
            binding.set_cdr(closure);
            closure
        };

        let left = make_recursive();
        let right = make_recursive();

        assert!(!eq_value(&left, &right));
        assert!(equal_value(&left, &right, 0));
        assert_eq!(
            left.to_hash_key(&HashTableTest::Equal),
            right.to_hash_key(&HashTableTest::Equal)
        );
    });
}

#[test]
fn hash_key_char_int_equivalence() {
    for test in [HashTableTest::Eq, HashTableTest::Eql, HashTableTest::Equal] {
        let char_key = Value::char('a').to_hash_key(&test);
        let int_key = Value::fixnum(97).to_hash_key(&test);
        assert_eq!(char_key, int_key);
    }
}

#[test]
fn lambda_params_arity() {
    let p = LambdaParams {
        required: vec![intern("a"), intern("b")],
        optional: vec![intern("c")],
        rest: None,
    };
    assert_eq!(p.min_arity(), 2);
    assert_eq!(p.max_arity(), Some(3));

    let p2 = LambdaParams {
        required: vec![intern("a")],
        optional: vec![],
        rest: Some(intern("rest")),
    };
    assert_eq!(p2.min_arity(), 1);
    assert_eq!(p2.max_arity(), None);
}

#[test]
fn cons_accessors() {
    with_test_heap(|| {
        let c = Value::cons(Value::fixnum(1), Value::fixnum(2));
        assert_eq!(c.cons_car(), Value::fixnum(1));
        assert_eq!(c.cons_cdr(), Value::fixnum(2));
        c.set_car(Value::fixnum(10));
        assert_eq!(c.cons_car(), Value::fixnum(10));
    });
}

#[test]
fn value_is_copy_and_16_bytes() {
    // Value is Copy — this assignment would fail to compile if not.
    let a = Value::fixnum(42);
    let b = a; // copy, not move
    let _ = a; // still usable after copy
    let _ = b;

    assert_eq!(
        std::mem::size_of::<Value>(),
        16,
        "Value should be 16 bytes (discriminant + largest variant)"
    );
}

#[test]
fn float_equality() {
    use super::equal_value;
    use crate::emacs_core::value::{ValueKind, VecLikeType};
    with_test_heap(|| {
        // 1.0 == 1.0
        assert!(equal_value(
            &Value::make_float(1.0),
            &Value::make_float(1.0),
            0
        ));
        // Emacs equal: NaN == NaN (bitwise comparison via to_bits)
        assert!(equal_value(
            &Value::make_float(f64::NAN),
            &Value::make_float(f64::NAN),
            0
        ));
        // Inf == Inf
        assert!(equal_value(
            &Value::make_float(f64::INFINITY),
            &Value::make_float(f64::INFINITY),
            0
        ));
        // Different values are not equal
        assert!(!equal_value(
            &Value::make_float(1.0),
            &Value::make_float(2.0),
            0
        ));
        // Int and Float are not equal under equal_value
        assert!(!equal_value(&Value::fixnum(1), &Value::make_float(1.0), 0));
    });
}

#[test]
fn vector_operations() {
    with_test_heap(|| {
        let v = Value::vector(vec![
            Value::fixnum(10),
            Value::fixnum(20),
            Value::fixnum(30),
        ]);
        assert!(v.is_vector());
        let items = v.as_vector_data().unwrap().clone();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], Value::fixnum(10));
        assert_eq!(items[1], Value::fixnum(20));
        assert_eq!(items[2], Value::fixnum(30));
    });
}

#[test]
fn list_length_proper() {
    with_test_heap(|| {
        let list = Value::list(vec![Value::fixnum(1), Value::fixnum(2), Value::fixnum(3)]);
        assert_eq!(super::list_length(&list), Some(3));
        assert_eq!(super::list_length(&Value::NIL), Some(0));
    });
}

#[test]
fn list_length_dotted() {
    with_test_heap(|| {
        // (1 . 2) — improper list
        let dotted = Value::cons(Value::fixnum(1), Value::fixnum(2));
        assert_eq!(super::list_length(&dotted), None);
    });
}

#[test]
fn as_int_as_float() {
    assert_eq!(Value::fixnum(42).as_int(), Some(42));
    assert_eq!(Value::make_float(3.14).as_int(), None);
    assert_eq!(Value::make_float(3.14).as_float(), Some(3.14));
    assert_eq!(Value::fixnum(42).as_float(), None);
    // as_number_f64 coerces both
    assert_eq!(Value::fixnum(7).as_number_f64(), Some(7.0));
    assert_eq!(Value::make_float(2.5).as_number_f64(), Some(2.5));
    assert_eq!(Value::NIL.as_number_f64(), None);
}

#[test]
fn type_predicates() {
    with_test_heap(|| {
        assert!(Value::fixnum(1).is_integer());
        assert!(Value::fixnum(1).is_number());
        assert!(!Value::fixnum(1).is_float());

        assert!(Value::make_float(1.0).is_float());
        assert!(Value::make_float(1.0).is_number());
        assert!(!Value::make_float(1.0).is_integer());

        assert!(Value::string("hi").is_string());
        assert!(!Value::string("hi").is_integer());

        let c = Value::cons(Value::fixnum(1), Value::NIL);
        assert!(c.is_cons());
        assert!(c.is_list());

        assert!(Value::NIL.is_list());
        assert!(!Value::NIL.is_cons());

        assert!(Value::vector(vec![]).is_vector());
        assert!(Value::symbol("foo").is_symbol());
        assert!(Value::keyword("bar").is_keyword());
        assert!(Value::char('x').is_char());
    });
}
