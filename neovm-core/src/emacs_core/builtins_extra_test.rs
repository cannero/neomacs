use super::*;
use crate::emacs_core::intern::intern;
use crate::emacs_core::load::{apply_runtime_startup_state, create_bootstrap_evaluator};
use crate::emacs_core::value::{LambdaData, LambdaParams, next_float_id};
use crate::emacs_core::{format_eval_result, parse_forms};

fn bootstrap_eval(src: &str) -> Vec<String> {
    let mut ev = create_bootstrap_evaluator().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    let forms = parse_forms(src).expect("parse");
    ev.eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect()
}

#[test]
fn remove_family_bootstrap_matches_gnu_subr() {
    let results = bootstrap_eval(
        r#"
        (subrp (symbol-function 'remove))
        (subrp (symbol-function 'remq))
        (subrp (symbol-function 'flatten-tree))
        (remove 2 '(1 2 3 2))
        (remq 'a '(a b a c))
        (flatten-tree '(1 (2 . 3) nil (4 5 (6)) 7))
        "#,
    );
    assert_eq!(results[0], "OK nil");
    assert_eq!(results[1], "OK nil");
    assert_eq!(results[2], "OK nil");
    assert_eq!(results[3], "OK (1 3)");
    assert_eq!(results[4], "OK (b c)");
    assert_eq!(results[5], "OK (1 2 3 4 5 6 7)");
}

#[test]
fn take_from_list() {
    let list = Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    let result = builtin_take(vec![Value::Int(2), list]).unwrap();
    let items = super::super::value::list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 2);
}

#[test]
fn string_empty_blank() {
    let results = bootstrap_eval(
        r#"
        (string-empty-p "")
        (string-empty-p "a")
        (string-blank-p "  ")
        (string-blank-p "x")
        "#,
    );
    assert_eq!(results[0], "OK t");
    assert_eq!(results[1], "OK nil");
    assert_eq!(results[2], "OK 0");
    assert_eq!(results[3], "OK nil");
}

#[test]
fn string_replace_bootstrap_matches_gnu_subr() {
    let results = bootstrap_eval(
        r#"
        (subrp (symbol-function 'string-replace))
        (string-replace "world" "rust" "hello world")
        (string-replace "x" "y" "no match")
        (condition-case err (string-replace "" "-" "abc") (error (car err)))
        "#,
    );
    assert_eq!(results[0], "OK nil");
    assert_eq!(results[1], r#"OK "hello rust""#);
    assert_eq!(results[2], r#"OK "no match""#);
    // wrong-length-argument is a subtype of error, so condition-case
    // catches it and (car err) returns the error symbol.
    assert_eq!(results[3], "OK wrong-length-argument");
}

#[test]
fn string_search() {
    let result =
        builtin_string_search(vec![Value::string("world"), Value::string("hello world")]).unwrap();
    assert_eq!(result.as_int(), Some(6));

    let result = builtin_string_search(vec![Value::string("xyz"), Value::string("hello")]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn proper_list_p() {
    let list = Value::list(vec![Value::Int(1), Value::Int(2)]);
    // proper-list-p returns the length of the list (2), not t
    assert_eq!(builtin_proper_list_p(vec![list]).unwrap(), Value::Int(2),);
    assert!(builtin_proper_list_p(vec![Value::Int(5)]).unwrap().is_nil(),);
}

#[test]
fn closurep_true_for_lambda_values() {
    let lambda = Value::make_lambda(LambdaData {
        params: LambdaParams::simple(vec![intern("x")]),
        body: vec![].into(),
        env: None,
        docstring: None,
        doc_form: None,
    });
    assert!(builtin_closurep(vec![lambda]).unwrap().is_truthy());
    assert!(builtin_closurep(vec![Value::Int(1)]).unwrap().is_nil());
}

#[test]
fn bare_symbol_and_predicate_semantics() {
    assert_eq!(
        builtin_bare_symbol(vec![Value::symbol("alpha")]).unwrap(),
        Value::symbol("alpha")
    );
    assert_eq!(
        builtin_bare_symbol(vec![Value::keyword(":k")]).unwrap(),
        Value::keyword(":k")
    );
    assert_eq!(builtin_bare_symbol(vec![Value::Nil]).unwrap(), Value::Nil);

    assert!(
        builtin_bare_symbol_p(vec![Value::symbol("alpha")])
            .unwrap()
            .is_truthy()
    );
    assert!(
        builtin_bare_symbol_p(vec![Value::keyword(":k")])
            .unwrap()
            .is_truthy()
    );
    assert!(builtin_bare_symbol_p(vec![Value::Nil]).unwrap().is_truthy());
    assert!(builtin_bare_symbol_p(vec![Value::Int(1)]).unwrap().is_nil());

    let err = builtin_bare_symbol(vec![Value::Int(1)]).unwrap_err();
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data[1], Value::Int(1));
        }
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn byteorder_shape_and_arity() {
    let byteorder = builtin_byteorder(vec![]).unwrap();
    assert!(matches!(byteorder, Value::Int(108) | Value::Int(66)));

    let err = builtin_byteorder(vec![Value::Nil]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn assoc_string_and_car_less_than_car_semantics() {
    let mut heap = crate::gc::heap::LispHeap::new();
    crate::emacs_core::value::set_current_heap(&mut heap);

    let result = builtin_assoc_string(vec![
        Value::string("A"),
        Value::list(vec![
            Value::cons(Value::string("a"), Value::Int(1)),
            Value::cons(Value::string("b"), Value::Int(2)),
        ]),
        Value::True,
    ])
    .unwrap();
    let Value::Cons(result_cell) = result else {
        panic!("expected dotted pair result");
    };
    let result_pair = read_cons(result_cell);
    assert_eq!(result_pair.car, Value::string("a"));
    assert_eq!(result_pair.cdr, Value::Int(1));

    let symbol_alist = Value::list(vec![
        Value::cons(Value::symbol("foo"), Value::Int(1)),
        Value::cons(Value::keyword(":k"), Value::Int(2)),
    ]);
    let symbol_hit = builtin_assoc_string(vec![Value::string("foo"), symbol_alist]).unwrap();
    let Value::Cons(symbol_cell) = symbol_hit else {
        panic!("expected dotted pair result");
    };
    let symbol_pair = read_cons(symbol_cell);
    assert_eq!(symbol_pair.car, Value::symbol("foo"));
    assert_eq!(symbol_pair.cdr, Value::Int(1));

    let nil_tail = Value::cons(
        Value::cons(Value::string("x"), Value::Int(1)),
        Value::Int(2),
    );
    assert!(
        builtin_assoc_string(vec![Value::string("x"), nil_tail])
            .unwrap()
            .is_truthy()
    );
    assert!(
        builtin_assoc_string(vec![Value::string("y"), Value::Int(1)])
            .unwrap()
            .is_nil()
    );

    let key_err = builtin_assoc_string(vec![Value::Int(1), Value::Nil]).unwrap_err();
    match key_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }

    assert!(
        builtin_car_less_than_car(vec![
            Value::cons(Value::Int(1), Value::symbol("a")),
            Value::cons(Value::Int(2), Value::symbol("b")),
        ])
        .unwrap()
        .is_truthy()
    );
    assert!(
        builtin_car_less_than_car(vec![
            Value::cons(Value::Float(3.0, next_float_id()), Value::symbol("a")),
            Value::cons(Value::Int(2), Value::symbol("b")),
        ])
        .unwrap()
        .is_nil()
    );

    let list_err =
        builtin_car_less_than_car(vec![Value::Int(1), Value::cons(Value::Int(2), Value::Nil)])
            .unwrap_err();
    match list_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }

    let number_err = builtin_car_less_than_car(vec![
        Value::cons(Value::symbol("x"), Value::Nil),
        Value::cons(Value::Int(1), Value::Nil),
    ])
    .unwrap_err();
    match number_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn number_predicates() {
    assert!(matches!(
        builtin_zerop(vec![Value::Int(0)]).unwrap(),
        Value::True
    ));
    assert!(builtin_zerop(vec![Value::Int(1)]).unwrap().is_nil());
    assert!(matches!(
        builtin_natnump(vec![Value::Int(5)]).unwrap(),
        Value::True
    ));
    assert!(builtin_natnump(vec![Value::Int(-1)]).unwrap().is_nil());
}

#[test]
fn fixnum_predicates_bootstrap_match_gnu_subr() {
    let results = bootstrap_eval(
        r#"
        (subrp (symbol-function 'fixnump))
        (subrp (symbol-function 'bignump))
        (list (fixnump 0)
              (fixnump most-positive-fixnum)
              (fixnump 1.0)
              (fixnump nil))
        (list (bignump 0)
              (bignump most-positive-fixnum)
              (bignump 1.0)
              (bignump nil))
        "#,
    );
    assert_eq!(results[0], "OK nil");
    assert_eq!(results[1], "OK nil");
    assert_eq!(results[2], "OK (t t nil nil)");
    assert_eq!(results[3], "OK (nil nil nil nil)");
}

#[test]
fn seq_uniq() {
    let results = bootstrap_eval(
        r#"
        (seq-uniq '(1 2 1 3))
        (seq-uniq '("Hello" "hello" "HELLO") #'string-equal-ignore-case)
        "#,
    );
    assert_eq!(results[0], "OK (1 2 3)");
    assert_eq!(results[1], "OK (\"Hello\")");
}

#[test]
fn seq_length_list_and_string() {
    let results = bootstrap_eval(
        r#"
        (seq-length '(1 2 3))
        (seq-length "hello")
        (seq-into '(1 2 3) 'vector)
        (seq-into [?h ?i] 'string)
        "#,
    );
    assert_eq!(results[0], "OK 3");
    assert_eq!(results[1], "OK 5");
    assert_eq!(results[2], "OK [1 2 3]");
    assert_eq!(results[3], "OK \"hi\"");
}

#[test]
fn seq_length_wrong_type_errors() {
    let results = bootstrap_eval(
        r#"
        (condition-case err
            (seq-length 42)
          (wrong-type-argument (car err)))
        (condition-case err
            (seq-into '(1 2 3) 'hash-table)
          (error (car err)))
        "#,
    );
    assert_eq!(results[0], "OK wrong-type-argument");
    assert_eq!(results[1], "OK error");
}

#[test]
fn user_info() {
    // These should not panic, just return strings.
    assert!(builtin_user_login_name(vec![]).unwrap().is_string());
    assert!(builtin_user_real_login_name(vec![]).unwrap().is_string());
    assert!(builtin_user_full_name(vec![]).unwrap().is_string());
    assert!(builtin_system_name(vec![]).unwrap().is_string());
    assert!(system_configuration_value().is_string());
    assert!(system_configuration_options_value().is_string());
    assert!(system_configuration_features_value().is_string());
    assert!(
        operating_system_release_value().is_nil() || operating_system_release_value().is_string()
    );
    assert!(builtin_emacs_version(vec![]).unwrap().is_string());
}

#[test]
fn user_identity_optional_args() {
    let login_for_uid = builtin_user_login_name(vec![Value::Int(current_uid())]).unwrap();
    assert!(login_for_uid.is_nil() || login_for_uid.is_string());

    let by_uid = builtin_user_full_name(vec![Value::Int(current_uid())]).unwrap();
    assert!(by_uid.is_nil() || by_uid.is_string());

    let login = builtin_user_login_name(vec![]).unwrap();
    let by_login = builtin_user_full_name(vec![login]).unwrap();
    assert!(by_login.is_nil() || by_login.is_string());
}

#[test]
fn user_identity_arity_contracts() {
    let login_name_err = builtin_user_login_name(vec![Value::Int(1), Value::Int(2)]).unwrap_err();
    match login_name_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected signal, got {other:?}"),
    }

    let real_login_err = builtin_user_real_login_name(vec![Value::Int(1)]).unwrap_err();
    match real_login_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected signal, got {other:?}"),
    }

    let full_name_err = builtin_user_full_name(vec![Value::Int(1), Value::Int(2)]).unwrap_err();
    match full_name_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn user_identity_type_contracts() {
    let login_name_err = builtin_user_login_name(vec![Value::string("root")]).unwrap_err();
    match login_name_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "error"),
        other => panic!("expected signal, got {other:?}"),
    }

    let full_name_err = builtin_user_full_name(vec![Value::list(vec![Value::Int(1)])]).unwrap_err();
    match full_name_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "error"),
        other => panic!("expected signal, got {other:?}"),
    }

    let negative_uid_login = builtin_user_login_name(vec![Value::Int(-1)]).unwrap_err();
    match negative_uid_login {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "error"),
        other => panic!("expected signal, got {other:?}"),
    }

    let negative_uid_full_name = builtin_user_full_name(vec![Value::Int(-1)]).unwrap_err();
    match negative_uid_full_name {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "error"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn emacs_pid() {
    let pid = builtin_emacs_pid(vec![]).unwrap();
    assert!(matches!(pid, Value::Int(n) if n > 0));
}

#[test]
fn runtime_identity_arity_contracts() {
    let system_name_err = builtin_system_name(vec![Value::Nil]).unwrap_err();
    match system_name_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected signal, got {other:?}"),
    }

    let version_with_nil = builtin_emacs_version(vec![Value::Nil]).unwrap();
    assert!(version_with_nil.is_string());

    let version_with_non_nil = builtin_emacs_version(vec![Value::True]).unwrap();
    assert!(version_with_non_nil.is_nil());

    let version_err = builtin_emacs_version(vec![Value::Nil, Value::Nil]).unwrap_err();
    match version_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected signal, got {other:?}"),
    }

    let pid_err = builtin_emacs_pid(vec![Value::Nil]).unwrap_err();
    match pid_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn garbage_collect_shape_and_arity() {
    let gc = builtin_garbage_collect(vec![]).unwrap();
    let buckets = super::super::value::list_to_vec(&gc).expect("gc list");
    assert_eq!(buckets.len(), 9);
    let names = buckets
        .iter()
        .map(|bucket| {
            let bucket_items = super::super::value::list_to_vec(bucket).expect("bucket list");
            match bucket_items.first() {
                Some(Value::Symbol(id)) => resolve_sym(*id).to_owned(),
                other => panic!("expected bucket symbol, got {other:?}"),
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "conses".to_string(),
            "symbols".to_string(),
            "strings".to_string(),
            "string-bytes".to_string(),
            "vectors".to_string(),
            "vector-slots".to_string(),
            "floats".to_string(),
            "intervals".to_string(),
            "buffers".to_string(),
        ]
    );
    for bucket in &buckets {
        let bucket_items = super::super::value::list_to_vec(bucket).expect("bucket list");
        assert!(bucket_items.len() >= 2);
        assert!(matches!(bucket_items[0], Value::Symbol(_)));
        assert!(
            bucket_items[1..]
                .iter()
                .all(|item| matches!(item, Value::Int(_)))
        );
    }

    let err = builtin_garbage_collect(vec![Value::Int(1)]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn memory_use_counts_shape_and_arity() {
    let counts = builtin_memory_use_counts(vec![]).unwrap();
    let items = super::super::value::list_to_vec(&counts).expect("counts list");
    assert_eq!(items.len(), 7);
    assert!(items.iter().all(|item| matches!(item, Value::Int(_))));

    let err = builtin_memory_use_counts(vec![Value::Int(1)]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected signal, got {other:?}"),
    }
}
