use super::*;
use crate::emacs_core::load::{apply_runtime_startup_state, create_bootstrap_evaluator_cached};
use crate::emacs_core::value::ValueKind;
use crate::emacs_core::{format_eval_result, parse_forms};

fn bootstrap_eval(src: &str) -> Vec<String> {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    let forms = parse_forms(src).expect("parse");
    ev.eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect()
}

// --- Seq.el pure operations ---

#[test]
fn seq_reverse_list() {
    let list = Value::list(vec![Value::fixnum(1), Value::fixnum(2), Value::fixnum(3)]);
    let result = builtin_seq_reverse(vec![list]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items[0].as_int(), Some(3));
    assert_eq!(items[2].as_int(), Some(1));
}

#[test]
fn seq_reverse_string() {
    let s = Value::string("abc");
    let result = builtin_seq_reverse(vec![s]).unwrap();
    assert_eq!(result.as_str(), Some("cba"));
}

#[test]
fn cl_first_list() {
    let list = Value::list(vec![Value::symbol("a"), Value::symbol("b")]);
    let result = builtin_cl_first(vec![list]).unwrap();
    assert!(result.is_symbol_named("a"));
}

#[test]
fn cl_first_nil() {
    let result = builtin_cl_first(vec![Value::NIL]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn cl_first_wrong_type() {
    assert!(builtin_cl_first(vec![Value::fixnum(1)]).is_err());
}

#[test]
fn cl_second_list() {
    let list = Value::list(vec![Value::symbol("a"), Value::symbol("b")]);
    let result = builtin_cl_second(vec![list]).unwrap();
    assert!(result.is_symbol_named("b"));
}

#[test]
fn cl_second_nil() {
    let result = builtin_cl_second(vec![Value::NIL]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn cl_second_wrong_type() {
    assert!(builtin_cl_second(vec![Value::fixnum(1)]).is_err());
}

#[test]
fn cl_third_list() {
    let list = Value::list(vec![
        Value::symbol("a"),
        Value::symbol("b"),
        Value::symbol("c"),
    ]);
    let result = builtin_cl_third(vec![list]).unwrap();
    assert!(result.is_symbol_named("c"));
}

#[test]
fn cl_third_nil() {
    let result = builtin_cl_third(vec![Value::NIL]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn cl_third_wrong_type() {
    assert!(builtin_cl_third(vec![Value::fixnum(1)]).is_err());
}

#[test]
fn cl_fourth_list() {
    let list = Value::list(vec![
        Value::symbol("a"),
        Value::symbol("b"),
        Value::symbol("c"),
        Value::symbol("d"),
    ]);
    let result = builtin_cl_fourth(vec![list]).unwrap();
    assert!(result.is_symbol_named("d"));
}

#[test]
fn cl_fourth_nil() {
    let result = builtin_cl_fourth(vec![Value::NIL]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn cl_fourth_wrong_type() {
    assert!(builtin_cl_fourth(vec![Value::fixnum(1)]).is_err());
}

#[test]
fn cl_fifth_list() {
    let list = Value::list(vec![
        Value::symbol("a"),
        Value::symbol("b"),
        Value::symbol("c"),
        Value::symbol("d"),
        Value::symbol("e"),
    ]);
    let result = builtin_cl_fifth(vec![list]).unwrap();
    assert!(result.is_symbol_named("e"));
}

#[test]
fn cl_fifth_nil() {
    let result = builtin_cl_fifth(vec![Value::NIL]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn cl_fifth_wrong_type() {
    assert!(builtin_cl_fifth(vec![Value::fixnum(1)]).is_err());
}

#[test]
fn cl_sixth_list() {
    let list = Value::list(vec![
        Value::symbol("a"),
        Value::symbol("b"),
        Value::symbol("c"),
        Value::symbol("d"),
        Value::symbol("e"),
        Value::symbol("f"),
    ]);
    let result = builtin_cl_sixth(vec![list]).unwrap();
    assert!(result.is_symbol_named("f"));
}

#[test]
fn cl_sixth_nil() {
    let result = builtin_cl_sixth(vec![Value::NIL]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn cl_sixth_wrong_type() {
    assert!(builtin_cl_sixth(vec![Value::fixnum(1)]).is_err());
}

#[test]
fn cl_seventh_list() {
    let list = Value::list(vec![
        Value::symbol("a"),
        Value::symbol("b"),
        Value::symbol("c"),
        Value::symbol("d"),
        Value::symbol("e"),
        Value::symbol("f"),
        Value::symbol("g"),
    ]);
    let result = builtin_cl_seventh(vec![list]).unwrap();
    assert!(result.is_symbol_named("g"));
}

#[test]
fn cl_seventh_nil() {
    let result = builtin_cl_seventh(vec![Value::NIL]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn cl_seventh_wrong_type() {
    assert!(builtin_cl_seventh(vec![Value::fixnum(1)]).is_err());
}

#[test]
fn cl_eighth_list() {
    let list = Value::list(vec![
        Value::symbol("a"),
        Value::symbol("b"),
        Value::symbol("c"),
        Value::symbol("d"),
        Value::symbol("e"),
        Value::symbol("f"),
        Value::symbol("g"),
        Value::symbol("h"),
    ]);
    let result = builtin_cl_eighth(vec![list]).unwrap();
    assert!(result.is_symbol_named("h"));
}

#[test]
fn cl_eighth_nil() {
    let result = builtin_cl_eighth(vec![Value::NIL]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn cl_eighth_wrong_type() {
    assert!(builtin_cl_eighth(vec![Value::fixnum(1)]).is_err());
}

#[test]
fn cl_ninth_list() {
    let list = Value::list(vec![
        Value::symbol("a"),
        Value::symbol("b"),
        Value::symbol("c"),
        Value::symbol("d"),
        Value::symbol("e"),
        Value::symbol("f"),
        Value::symbol("g"),
        Value::symbol("h"),
        Value::symbol("i"),
    ]);
    let result = builtin_cl_ninth(vec![list]).unwrap();
    assert!(result.is_symbol_named("i"));
}

#[test]
fn cl_ninth_nil() {
    let result = builtin_cl_ninth(vec![Value::NIL]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn cl_ninth_wrong_type() {
    assert!(builtin_cl_ninth(vec![Value::fixnum(1)]).is_err());
}

#[test]
fn cl_tenth_list() {
    let list = Value::list(vec![
        Value::symbol("a"),
        Value::symbol("b"),
        Value::symbol("c"),
        Value::symbol("d"),
        Value::symbol("e"),
        Value::symbol("f"),
        Value::symbol("g"),
        Value::symbol("h"),
        Value::symbol("i"),
        Value::symbol("j"),
    ]);
    let result = builtin_cl_tenth(vec![list]).unwrap();
    assert!(result.is_symbol_named("j"));
}

#[test]
fn cl_tenth_nil() {
    let result = builtin_cl_tenth(vec![Value::NIL]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn cl_tenth_wrong_type() {
    assert!(builtin_cl_tenth(vec![Value::fixnum(1)]).is_err());
}

#[test]
fn cl_rest_list() {
    let list = Value::list(vec![
        Value::symbol("a"),
        Value::symbol("b"),
        Value::symbol("c"),
    ]);
    let result = builtin_cl_rest(vec![list]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 2);
    assert!(items[0].is_symbol_named("b"));
}

#[test]
fn cl_rest_nil() {
    let result = builtin_cl_rest(vec![Value::NIL]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn cl_rest_wrong_type() {
    assert!(builtin_cl_rest(vec![Value::fixnum(1)]).is_err());
}

#[test]
fn cl_evenp_true() {
    let result = builtin_cl_evenp(vec![Value::fixnum(2)]).unwrap();
    assert!(result.is_truthy());
}

#[test]
fn cl_evenp_false() {
    let result = builtin_cl_evenp(vec![Value::fixnum(3)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn cl_evenp_wrong_type() {
    assert!(builtin_cl_evenp(vec![Value::string("x")]).is_err());
}

#[test]
fn cl_oddp_true() {
    let result = builtin_cl_oddp(vec![Value::fixnum(3)]).unwrap();
    assert!(result.is_truthy());
}

#[test]
fn cl_oddp_false() {
    let result = builtin_cl_oddp(vec![Value::fixnum(2)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn cl_oddp_wrong_type() {
    assert!(builtin_cl_oddp(vec![Value::string("x")]).is_err());
}

#[test]
fn cl_plusp_true() {
    let result = builtin_cl_plusp(vec![Value::fixnum(1)]).unwrap();
    assert!(result.is_truthy());
}

#[test]
fn cl_plusp_false() {
    let result = builtin_cl_plusp(vec![Value::fixnum(0)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn cl_plusp_wrong_type() {
    assert!(builtin_cl_plusp(vec![Value::string("x")]).is_err());
}

#[test]
fn cl_minusp_true() {
    let result = builtin_cl_minusp(vec![Value::fixnum(-1)]).unwrap();
    assert!(result.is_truthy());
}

#[test]
fn cl_minusp_false() {
    let result = builtin_cl_minusp(vec![Value::fixnum(0)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn cl_minusp_wrong_type() {
    assert!(builtin_cl_minusp(vec![Value::string("x")]).is_err());
}

#[test]
fn cl_subseq_list() {
    let results = bootstrap_eval(
        r#"
        (progn (require 'cl-lib) (cl-subseq '(a b c) 1 3))
        (progn (require 'cl-lib) (cl-concatenate 'list '(a b) '(c)))
        (progn (require 'cl-lib) (cl-remove-duplicates '(a b a c b)))
        "#,
    );
    assert_eq!(results[0], "OK (b c)");
    assert_eq!(results[1], "OK (a b c)");
    assert_eq!(results[2], "OK (a c b)");
}

#[test]
fn cl_subseq_wrong_arity() {
    let results = bootstrap_eval(
        r#"
        (condition-case err
            (progn (require 'cl-lib) (cl-subseq 0))
          (wrong-number-of-arguments (car err)))
        (condition-case err
            (progn (require 'cl-lib) (cl-concatenate 0 nil))
          (error (car err)))
        (condition-case err
            (progn (require 'cl-lib) (cl-remove-duplicates nil nil))
          (error (car err)))
        "#,
    );
    assert_eq!(results[0], "OK wrong-number-of-arguments");
    assert_eq!(results[1], "OK error");
    assert_eq!(results[2], "OK error");
}

#[test]
fn cl_subseq_wrong_type() {
    // GNU signals (error "Unsupported sequence: 0"), not wrong-type-argument.
    // Verified against GNU Emacs --batch.
    let results = bootstrap_eval(
        r#"
        (condition-case err
            (progn (require 'cl-lib) (cl-subseq 0 0))
          (error (car err)))
        "#,
    );
    assert_eq!(results[0], "OK error");
}

#[test]
fn cl_member_found_tail() {
    let result = builtin_cl_member(vec![
        Value::symbol("b"),
        Value::list(vec![
            Value::symbol("a"),
            Value::symbol("b"),
            Value::symbol("c"),
        ]),
    ])
    .unwrap();
    assert_eq!(
        result,
        Value::list(vec![Value::symbol("b"), Value::symbol("c")])
    );
}

#[test]
fn cl_member_not_found() {
    let result = builtin_cl_member(vec![
        Value::symbol("z"),
        Value::list(vec![Value::symbol("a"), Value::symbol("b")]),
    ])
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn cl_member_wrong_arity() {
    assert!(builtin_cl_member(vec![Value::symbol("a")]).is_err());
}

#[test]
fn cl_coerce_list_to_vector() {
    let result = builtin_cl_coerce(vec![
        Value::list(vec![Value::symbol("a"), Value::symbol("b")]),
        Value::symbol("vector"),
    ])
    .unwrap();
    assert_eq!(
        result,
        Value::vector(vec![Value::symbol("a"), Value::symbol("b")])
    );
}

#[test]
fn cl_coerce_wrong_type_name() {
    assert!(builtin_cl_coerce(vec![Value::NIL, Value::fixnum(0)]).is_err());
}

#[test]
fn cl_adjoin_prepends_when_missing() {
    let result = builtin_cl_adjoin(vec![
        Value::symbol("a"),
        Value::list(vec![Value::symbol("b"), Value::symbol("c")]),
    ])
    .unwrap();
    assert_eq!(
        result,
        Value::list(vec![
            Value::symbol("a"),
            Value::symbol("b"),
            Value::symbol("c")
        ])
    );
}

#[test]
fn cl_adjoin_keeps_existing() {
    let list = Value::list(vec![Value::symbol("a"), Value::symbol("b")]);
    let result = builtin_cl_adjoin(vec![Value::symbol("a"), list]).unwrap();
    assert_eq!(result, list);
}

#[test]
fn cl_adjoin_wrong_arity() {
    assert!(builtin_cl_adjoin(vec![Value::symbol("a")]).is_err());
}

#[test]
fn cl_remove_filters_equal_items() {
    let result = builtin_cl_remove(vec![
        Value::symbol("a"),
        Value::list(vec![
            Value::symbol("a"),
            Value::symbol("b"),
            Value::symbol("a"),
        ]),
    ])
    .unwrap();
    assert_eq!(result, Value::list(vec![Value::symbol("b")]));
}

#[test]
fn cl_remove_wrong_arity() {
    assert!(builtin_cl_remove(vec![Value::symbol("a")]).is_err());
}

#[test]
fn seq_drop_test() {
    let list = Value::list(vec![Value::fixnum(1), Value::fixnum(2), Value::fixnum(3)]);
    let result = builtin_seq_drop(vec![list, Value::fixnum(2)]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].as_int(), Some(3));
}

#[test]
fn seq_take_test() {
    let list = Value::list(vec![Value::fixnum(1), Value::fixnum(2), Value::fixnum(3)]);
    let result = builtin_seq_take(vec![list, Value::fixnum(2)]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 2);
}

#[test]
fn seq_subseq_test() {
    let vec = Value::vector(vec![
        Value::fixnum(10),
        Value::fixnum(20),
        Value::fixnum(30),
        Value::fixnum(40),
    ]);
    let result = builtin_seq_subseq(vec![vec, Value::fixnum(1), Value::fixnum(3)]).unwrap();
    if result.is_vector() {
        let v = result.as_vector_data().unwrap().clone();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].as_int(), Some(20));
        assert_eq!(v[1].as_int(), Some(30));
    } else {
        panic!("expected vector");
    }
}

#[test]
fn seq_concatenate_test() {
    let l1 = Value::list(vec![Value::fixnum(1)]);
    let l2 = Value::list(vec![Value::fixnum(2)]);
    let result = builtin_seq_concatenate(vec![Value::symbol("list"), l1, l2]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 2);
}

#[test]
fn seq_empty_p_test() {
    assert!(builtin_seq_empty_p(vec![Value::NIL]).unwrap().is_t());
    assert!(builtin_seq_empty_p(vec![Value::string("")]).unwrap().is_t());
    assert!(
        builtin_seq_empty_p(vec![Value::list(vec![Value::fixnum(1)])])
            .unwrap()
            .is_nil()
    );
}

#[test]
fn seq_min_max_test() {
    let list = Value::list(vec![Value::fixnum(3), Value::fixnum(1), Value::fixnum(2)]);
    assert_eq!(builtin_seq_min(vec![list]).unwrap().as_int(), Some(1));
    assert_eq!(builtin_seq_max(vec![list]).unwrap().as_int(), Some(3));
}

// --- Eval-dependent tests (using Context) ---

#[test]
fn seq_reduce_with_eval() {
    let mut evaluator = super::super::eval::Context::new();
    let func = Value::subr(intern("+"));
    let seq = Value::list(vec![Value::fixnum(10), Value::fixnum(20)]);
    let result = builtin_seq_reduce(&mut evaluator, vec![func, seq, Value::fixnum(0)]).unwrap();
    assert_eq!(result.as_int(), Some(30));
}

#[test]
fn seq_count_with_eval() {
    let mut evaluator = super::super::eval::Context::new();
    let func = Value::subr(intern("numberp"));
    let seq = Value::list(vec![Value::fixnum(1), Value::string("a"), Value::fixnum(2)]);
    let result = builtin_seq_count(&mut evaluator, vec![func, seq]).unwrap();
    assert_eq!(result.as_int(), Some(2));
}

#[test]
fn cl_position_with_eval() {
    let mut evaluator = super::super::eval::Context::new();
    let seq = Value::list(vec![
        Value::symbol("a"),
        Value::symbol("b"),
        Value::symbol("c"),
    ]);
    let result = builtin_cl_position(&mut evaluator, vec![Value::symbol("b"), seq]).unwrap();
    assert_eq!(result.as_int(), Some(1));
}

#[test]
fn cl_position_wrong_arity() {
    let mut evaluator = super::super::eval::Context::new();
    assert!(builtin_cl_position(&mut evaluator, vec![Value::symbol("a")]).is_err());
}

#[test]
fn cl_reduce_with_eval() {
    let results = bootstrap_eval(
        r#"
        (progn (require 'cl-lib) (cl-reduce #'+ '(1 2 3) :initial-value 0))
        (progn (require 'cl-lib) (cl-count 1 '(1 "x" 1)))
        (progn (require 'cl-lib) (cl-count-if #'numberp '(1 "x" 2)))
        (progn (require 'cl-lib) (cl-some #'numberp '("x" 2)))
        (progn (require 'cl-lib) (cl-every #'numberp '(1 2 3)))
        "#,
    );
    assert_eq!(results[0], "OK 6");
    assert_eq!(results[1], "OK 2");
    assert_eq!(results[2], "OK 2");
    assert_eq!(results[3], "OK t");
    assert_eq!(results[4], "OK t");
}

#[test]
fn cl_reduce_without_initial_value_bootstrap() {
    let results = bootstrap_eval(
        r#"
        (progn (require 'cl-lib) (cl-reduce #'+ '(1 2 3)))
        (progn (require 'cl-lib) (cl-reduce #'+ '(42)))
        "#,
    );
    assert_eq!(results[0], "OK 6");
    assert_eq!(results[1], "OK 42");
}

#[test]
fn cl_count_some_every_bootstrap() {
    let results = bootstrap_eval(
        r#"
        (progn (require 'cl-lib) (cl-count 1 '(1 "x" 1)))
        (progn (require 'cl-lib) (cl-count-if #'numberp '(1 "x" 2)))
        (progn (require 'cl-lib) (cl-some #'numberp '("x" 2)))
        (progn (require 'cl-lib) (cl-every #'numberp '(1 2 3)))
        "#,
    );
    assert_eq!(results[0], "OK 2");
    assert_eq!(results[1], "OK 2");
    assert_eq!(results[2], "OK t");
    assert_eq!(results[3], "OK t");
}

#[test]
fn cl_notany_with_eval() {
    let mut evaluator = super::super::eval::Context::new();
    let func = Value::subr(intern("numberp"));
    let seq = Value::list(vec![Value::string("x"), Value::string("y")]);
    let result = builtin_cl_notany(&mut evaluator, vec![func, seq]).unwrap();
    assert!(result.is_truthy());
}

#[test]
fn cl_notevery_with_eval() {
    let mut evaluator = super::super::eval::Context::new();
    let func = Value::subr(intern("numberp"));
    let seq = Value::list(vec![Value::fixnum(1), Value::string("x")]);
    let result = builtin_cl_notevery(&mut evaluator, vec![func, seq]).unwrap();
    assert!(result.is_truthy());
}

#[test]
fn cl_gensym_default_prefix() {
    let result = builtin_cl_gensym(vec![]).unwrap();
    match result.kind() {
        ValueKind::Symbol(id) => assert!(resolve_sym(id).starts_with('G')),
        other => panic!("expected symbol, got {other:?}"),
    }
}

#[test]
fn cl_gensym_custom_prefix() {
    let result = builtin_cl_gensym(vec![Value::string("P")]).unwrap();
    match result.kind() {
        ValueKind::Symbol(id) => assert!(resolve_sym(id).starts_with('P')),
        other => panic!("expected symbol, got {other:?}"),
    }
}

#[test]
fn cl_gensym_wrong_type() {
    assert!(builtin_cl_gensym(vec![Value::fixnum(1)]).is_err());
}

#[test]
fn cl_find_found() {
    let result = builtin_cl_find(vec![
        Value::symbol("b"),
        Value::list(vec![
            Value::symbol("a"),
            Value::symbol("b"),
            Value::symbol("c"),
        ]),
    ])
    .unwrap();
    assert_eq!(result, Value::symbol("b"));
}

#[test]
fn cl_find_not_found() {
    let result = builtin_cl_find(vec![
        Value::symbol("z"),
        Value::list(vec![
            Value::symbol("a"),
            Value::symbol("b"),
            Value::symbol("c"),
        ]),
    ])
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn cl_find_wrong_arity() {
    assert!(builtin_cl_find(vec![Value::symbol("a")]).is_err());
}

#[test]
fn cl_find_if_with_eval() {
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_cl_find_if(
        &mut evaluator,
        vec![
            Value::subr(intern("numberp")),
            Value::list(vec![Value::string("x"), Value::fixnum(2)]),
        ],
    )
    .unwrap();
    assert_eq!(result, Value::fixnum(2));
}

#[test]
fn cl_find_if_wrong_arity() {
    let mut evaluator = super::super::eval::Context::new();
    assert!(builtin_cl_find_if(&mut evaluator, vec![Value::subr(intern("numberp"))]).is_err());
}

#[test]
fn cl_subsetp_true() {
    let result = builtin_cl_subsetp(vec![
        Value::list(vec![Value::symbol("a"), Value::symbol("b")]),
        Value::list(vec![
            Value::symbol("b"),
            Value::symbol("a"),
            Value::symbol("c"),
        ]),
    ])
    .unwrap();
    assert!(result.is_truthy());
}

#[test]
fn cl_subsetp_false() {
    let result = builtin_cl_subsetp(vec![
        Value::list(vec![Value::symbol("a"), Value::symbol("z")]),
        Value::list(vec![Value::symbol("a"), Value::symbol("b")]),
    ])
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn cl_subsetp_wrong_arity() {
    assert!(builtin_cl_subsetp(vec![Value::NIL]).is_err());
}

#[test]
fn cl_intersection_basic() {
    let result = builtin_cl_intersection(vec![
        Value::list(vec![
            Value::symbol("a"),
            Value::symbol("b"),
            Value::symbol("c"),
        ]),
        Value::list(vec![
            Value::symbol("b"),
            Value::symbol("d"),
            Value::symbol("c"),
        ]),
    ])
    .unwrap();
    assert_eq!(
        result,
        Value::list(vec![Value::symbol("b"), Value::symbol("c")])
    );
}

#[test]
fn cl_intersection_no_overlap() {
    let result = builtin_cl_intersection(vec![
        Value::list(vec![Value::symbol("a")]),
        Value::list(vec![Value::symbol("z")]),
    ])
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn cl_intersection_wrong_arity() {
    assert!(builtin_cl_intersection(vec![Value::NIL]).is_err());
}

#[test]
fn cl_set_difference_basic() {
    let result = builtin_cl_set_difference(vec![
        Value::list(vec![
            Value::symbol("a"),
            Value::symbol("b"),
            Value::symbol("c"),
            Value::symbol("d"),
        ]),
        Value::list(vec![Value::symbol("b"), Value::symbol("d")]),
    ])
    .unwrap();
    assert_eq!(
        result,
        Value::list(vec![Value::symbol("a"), Value::symbol("c")])
    );
}

#[test]
fn cl_set_difference_all_removed() {
    let result = builtin_cl_set_difference(vec![
        Value::list(vec![Value::symbol("a")]),
        Value::list(vec![Value::symbol("a"), Value::symbol("b")]),
    ])
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn cl_set_difference_wrong_arity() {
    assert!(builtin_cl_set_difference(vec![Value::NIL]).is_err());
}

#[test]
fn cl_union_basic() {
    let result = builtin_cl_union(vec![
        Value::list(vec![Value::symbol("a"), Value::symbol("b")]),
        Value::list(vec![Value::symbol("b"), Value::symbol("c")]),
    ])
    .unwrap();
    assert_eq!(
        result,
        Value::list(vec![
            Value::symbol("a"),
            Value::symbol("b"),
            Value::symbol("c")
        ])
    );
}

#[test]
fn cl_union_empty_left() {
    let result = builtin_cl_union(vec![Value::NIL, Value::list(vec![Value::symbol("c")])]).unwrap();
    assert_eq!(result, Value::list(vec![Value::symbol("c")]));
}

#[test]
fn cl_union_wrong_arity() {
    assert!(builtin_cl_union(vec![Value::NIL]).is_err());
}

#[test]
fn cl_substitute_basic() {
    let result = builtin_cl_substitute(vec![
        Value::symbol("x"),
        Value::symbol("b"),
        Value::list(vec![
            Value::symbol("a"),
            Value::symbol("b"),
            Value::symbol("c"),
            Value::symbol("b"),
        ]),
    ])
    .unwrap();
    assert_eq!(
        result,
        Value::list(vec![
            Value::symbol("a"),
            Value::symbol("x"),
            Value::symbol("c"),
            Value::symbol("x"),
        ])
    );
}

#[test]
fn cl_substitute_wrong_arity() {
    assert!(builtin_cl_substitute(vec![Value::NIL]).is_err());
}

#[test]
fn cl_sort_with_eval() {
    let mut evaluator = super::super::eval::Context::new();
    let seq = Value::list(vec![Value::fixnum(3), Value::fixnum(1), Value::fixnum(2)]);
    let result = builtin_cl_sort(&mut evaluator, vec![seq, Value::subr(intern("<"))]).unwrap();
    assert_eq!(
        result,
        Value::list(vec![Value::fixnum(1), Value::fixnum(2), Value::fixnum(3)])
    );
}

#[test]
fn cl_stable_sort_with_eval() {
    let mut evaluator = super::super::eval::Context::new();
    let seq = Value::list(vec![Value::fixnum(3), Value::fixnum(1), Value::fixnum(2)]);
    let result =
        builtin_cl_stable_sort(&mut evaluator, vec![seq, Value::subr(intern("<"))]).unwrap();
    assert_eq!(
        result,
        Value::list(vec![Value::fixnum(1), Value::fixnum(2), Value::fixnum(3)])
    );
}

#[test]
fn cl_remove_if_with_eval() {
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_cl_remove_if(
        &mut evaluator,
        vec![
            Value::subr(intern("numberp")),
            Value::list(vec![Value::fixnum(1), Value::string("x"), Value::fixnum(2)]),
        ],
    )
    .unwrap();
    assert_eq!(result, Value::list(vec![Value::string("x")]));
}

#[test]
fn cl_remove_if_not_with_eval() {
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_cl_remove_if_not(
        &mut evaluator,
        vec![
            Value::subr(intern("numberp")),
            Value::list(vec![Value::fixnum(1), Value::string("x"), Value::fixnum(2)]),
        ],
    )
    .unwrap();
    assert_eq!(
        result,
        Value::list(vec![Value::fixnum(1), Value::fixnum(2)])
    );
}

#[test]
fn cl_map_list_with_eval() {
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_cl_map(
        &mut evaluator,
        vec![
            Value::symbol("list"),
            Value::subr(intern("1+")),
            Value::list(vec![Value::fixnum(1), Value::fixnum(2), Value::fixnum(3)]),
        ],
    )
    .unwrap();
    assert_eq!(
        result,
        Value::list(vec![Value::fixnum(2), Value::fixnum(3), Value::fixnum(4)])
    );
}

#[test]
fn cl_map_string_with_eval() {
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_cl_map(
        &mut evaluator,
        vec![
            Value::symbol("string"),
            Value::subr(intern("identity")),
            Value::string("ab"),
        ],
    )
    .unwrap();
    assert_eq!(result, Value::string("ab"));
}

#[test]
fn cl_map_unsupported_type() {
    let mut evaluator = super::super::eval::Context::new();
    assert!(
        builtin_cl_map(
            &mut evaluator,
            vec![
                Value::symbol("hash-table"),
                Value::subr(intern("identity")),
                Value::list(vec![Value::fixnum(1)]),
            ],
        )
        .is_err()
    );
}
