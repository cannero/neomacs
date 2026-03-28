use super::*;
use crate::emacs_core::Expr;
use crate::emacs_core::marker::make_marker_value_with_id;

/// Helper: set up a temporary heap for tests that use Value constructors.
fn with_test_heap<R>(f: impl FnOnce() -> R) -> R {
    let mut heap = LispHeap::new();
    set_current_heap(&mut heap);
    let result = f();
    clear_current_heap();
    result
}

#[test]
fn value_constructors() {
    with_test_heap(|| {
        assert!(Value::Nil.is_nil());
        assert!(Value::t().is_truthy());
        assert!(Value::Int(42).is_integer());
        assert!(Value::Float(3.14, next_float_id()).is_float());
        assert!(Value::string("hello").is_string());
        assert!(Value::Char('a').is_char());
        assert!(Value::symbol("foo").is_symbol());
        assert!(Value::keyword(":bar").is_keyword());
    });
}

#[test]
fn list_round_trip() {
    with_test_heap(|| {
        let lst = Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let vec = list_to_vec(&lst).unwrap();
        assert_eq!(vec.len(), 3);
    });
}

#[test]
fn eq_identity() {
    with_test_heap(|| {
        assert!(eq_value(&Value::Nil, &Value::Nil));
        assert!(eq_value(&Value::Int(42), &Value::Int(42)));
        assert!(!eq_value(&Value::Int(1), &Value::Int(2)));
        assert!(eq_value(&Value::Char('a'), &Value::Int(97)));
        assert!(eq_value(&Value::Int(97), &Value::Char('a')));
        assert!(eq_value(&Value::symbol("foo"), &Value::symbol("foo")));
    });
}

#[test]
fn keyword_identity_is_consistent_across_constructors() {
    with_test_heap(|| {
        let keyword_from_symbol_ctor = Value::symbol(":kw");
        let keyword_from_keyword_ctor = Value::keyword(":kw");

        // Value::symbol(":kw") auto-detects the colon and produces Keyword variant
        assert!(matches!(keyword_from_symbol_ctor, Value::Keyword(_)));
        assert!(eq_value(
            &keyword_from_symbol_ctor,
            &keyword_from_keyword_ctor
        ));

        // Direct Value::Symbol(intern(":kw")) bypasses auto-detection and produces
        // a Symbol variant. In GNU Emacs, :kw and kw are different symbols —
        // Symbol and Keyword variants must NOT be eq.
        let legacy_symbol_variant = Value::Symbol(intern(":kw"));
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
        let a = Value::list(vec![Value::Int(1), Value::Int(2)]);
        let b = Value::list(vec![Value::Int(1), Value::Int(2)]);
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
        let left = make_marker_value_with_id(Some("*scratch*"), Some(4), false, Some(1));
        let right = make_marker_value_with_id(Some("*scratch*"), Some(4), false, Some(2));
        let different = make_marker_value_with_id(Some("*scratch*"), Some(5), false, Some(1));

        assert!(equal_value(&left, &right, 0));
        assert!(!equal_value(&left, &different, 0));
    });
}

#[test]
fn closure_equal_is_structural() {
    with_test_heap(|| {
        let env_a = Value::list(vec![Value::cons(Value::symbol("n"), Value::Int(5))]);
        let env_b = Value::list(vec![Value::cons(Value::symbol("n"), Value::Int(5))]);
        let env_c = Value::list(vec![Value::cons(Value::symbol("n"), Value::Int(10))]);

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
            let binding = Value::cons(Value::symbol("f"), Value::Nil);
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
        let char_key = Value::Char('a').to_hash_key(&test);
        let int_key = Value::Int(97).to_hash_key(&test);
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
        let c = Value::cons(Value::Int(1), Value::Int(2));
        assert_eq!(c.cons_car(), Value::Int(1));
        assert_eq!(c.cons_cdr(), Value::Int(2));
        c.set_car(Value::Int(10));
        assert_eq!(c.cons_car(), Value::Int(10));
    });
}

#[test]
fn value_is_copy_and_16_bytes() {
    // Value is Copy — this assignment would fail to compile if not.
    let a = Value::Int(42);
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
    with_test_heap(|| {
        // 1.0 == 1.0
        assert!(equal_value(
            &Value::Float(1.0, next_float_id()),
            &Value::Float(1.0, next_float_id()),
            0
        ));
        // Emacs equal: NaN == NaN (bitwise comparison via to_bits)
        assert!(equal_value(
            &Value::Float(f64::NAN, next_float_id()),
            &Value::Float(f64::NAN, next_float_id()),
            0
        ));
        // Inf == Inf
        assert!(equal_value(
            &Value::Float(f64::INFINITY, next_float_id()),
            &Value::Float(f64::INFINITY, next_float_id()),
            0
        ));
        // Different values are not equal
        assert!(!equal_value(
            &Value::Float(1.0, next_float_id()),
            &Value::Float(2.0, next_float_id()),
            0
        ));
        // Int and Float are not equal under equal_value
        assert!(!equal_value(
            &Value::Int(1),
            &Value::Float(1.0, next_float_id()),
            0
        ));
    });
}

#[test]
fn vector_operations() {
    with_test_heap(|| {
        let v = Value::vector(vec![Value::Int(10), Value::Int(20), Value::Int(30)]);
        assert!(v.is_vector());
        let items = super::with_heap(|h| {
            let id = match v {
                Value::Vector(id) => id,
                _ => panic!(),
            };
            h.get_vector(id).clone()
        });
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], Value::Int(10));
        assert_eq!(items[1], Value::Int(20));
        assert_eq!(items[2], Value::Int(30));
    });
}

#[test]
fn list_length_proper() {
    with_test_heap(|| {
        let list = Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        assert_eq!(super::list_length(&list), Some(3));
        assert_eq!(super::list_length(&Value::Nil), Some(0));
    });
}

#[test]
fn list_length_dotted() {
    with_test_heap(|| {
        // (1 . 2) — improper list
        let dotted = Value::cons(Value::Int(1), Value::Int(2));
        assert_eq!(super::list_length(&dotted), None);
    });
}

#[test]
fn as_int_as_float() {
    assert_eq!(Value::Int(42).as_int(), Some(42));
    assert_eq!(Value::Float(3.14, next_float_id()).as_int(), None);
    assert_eq!(Value::Float(3.14, next_float_id()).as_float(), Some(3.14));
    assert_eq!(Value::Int(42).as_float(), None);
    // as_number_f64 coerces both
    assert_eq!(Value::Int(7).as_number_f64(), Some(7.0));
    assert_eq!(
        Value::Float(2.5, next_float_id()).as_number_f64(),
        Some(2.5)
    );
    assert_eq!(Value::Nil.as_number_f64(), None);
}

#[test]
fn type_predicates() {
    with_test_heap(|| {
        assert!(Value::Int(1).is_integer());
        assert!(Value::Int(1).is_number());
        assert!(!Value::Int(1).is_float());

        assert!(Value::Float(1.0, next_float_id()).is_float());
        assert!(Value::Float(1.0, next_float_id()).is_number());
        assert!(!Value::Float(1.0, next_float_id()).is_integer());

        assert!(Value::string("hi").is_string());
        assert!(!Value::string("hi").is_integer());

        let c = Value::cons(Value::Int(1), Value::Nil);
        assert!(c.is_cons());
        assert!(c.is_list());

        assert!(Value::Nil.is_list());
        assert!(!Value::Nil.is_cons());

        assert!(Value::vector(vec![]).is_vector());
        assert!(Value::symbol("foo").is_symbol());
        assert!(Value::keyword("bar").is_keyword());
        assert!(Value::Char('x').is_char());
    });
}
