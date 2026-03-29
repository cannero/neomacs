use super::*;

#[test]
fn compose_region_internal_min_args() {
    let mut eval = super::super::eval::Context::new();
    {
        let buffer = eval.buffers.current_buffer_mut().expect("current buffer");
        buffer.insert("0123456789");
    }
    let result = builtin_compose_region_internal(&mut eval, vec![Value::Int(1), Value::Int(10)]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn compose_region_internal_max_args() {
    let mut eval = super::super::eval::Context::new();
    {
        let buffer = eval.buffers.current_buffer_mut().expect("current buffer");
        buffer.insert("0123456789");
    }
    let result = builtin_compose_region_internal(
        &mut eval,
        vec![Value::Int(1), Value::Int(10), Value::Nil, Value::Nil],
    );
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn compose_region_internal_too_few_args() {
    let mut eval = super::super::eval::Context::new();
    let result = builtin_compose_region_internal(&mut eval, vec![Value::Int(1)]);
    assert!(result.is_err());
}

#[test]
fn compose_region_internal_too_many_args() {
    let mut eval = super::super::eval::Context::new();
    let result = builtin_compose_region_internal(
        &mut eval,
        vec![
            Value::Int(1),
            Value::Int(10),
            Value::Nil,
            Value::Nil,
            Value::Nil,
        ],
    );
    assert!(result.is_err());
}

#[test]
fn compose_region_internal_rejects_non_integer_positions() {
    let mut eval = super::super::eval::Context::new();
    let result =
        builtin_compose_region_internal(&mut eval, vec![Value::symbol("x"), Value::Int(10)]);
    assert!(result.is_err());
    let result =
        builtin_compose_region_internal(&mut eval, vec![Value::Int(1), Value::symbol("y")]);
    assert!(result.is_err());
}

#[test]
fn compose_region_internal_eval_range_checks() {
    let mut eval = super::super::eval::Context::new();
    {
        let buffer = eval.buffers.current_buffer_mut().expect("current buffer");
        buffer.insert("abc");
    }
    let ok = builtin_compose_region_internal(&mut eval, vec![Value::Int(1), Value::Int(3)]);
    assert!(ok.is_ok());

    let out_of_range =
        builtin_compose_region_internal(&mut eval, vec![Value::Int(0), Value::Int(0)]);
    assert!(out_of_range.is_err());
}

#[test]
fn compose_string_internal_returns_string() {
    let s = Value::string("hello");
    let result = builtin_compose_string_internal(vec![s, Value::Int(0), Value::Int(5)]);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().as_str(), Some("hello"));
}

#[test]
fn compose_string_internal_with_optional_args() {
    let s = Value::string("hello");
    let result = builtin_compose_string_internal(vec![
        s,
        Value::Int(0),
        Value::Int(5),
        Value::Nil,
        Value::Nil,
    ]);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().as_str(), Some("hello"));
}

#[test]
fn compose_string_internal_too_few_args() {
    let result = builtin_compose_string_internal(vec![Value::string("hi"), Value::Int(0)]);
    assert!(result.is_err());
}

#[test]
fn compose_string_internal_type_checks() {
    let non_string =
        builtin_compose_string_internal(vec![Value::Int(1), Value::Int(0), Value::Int(1)]);
    assert!(non_string.is_err());
    let bad_start = builtin_compose_string_internal(vec![
        Value::string("abc"),
        Value::symbol("x"),
        Value::Int(1),
    ]);
    assert!(bad_start.is_err());
    let bad_end = builtin_compose_string_internal(vec![
        Value::string("abc"),
        Value::Int(0),
        Value::symbol("y"),
    ]);
    assert!(bad_end.is_err());
}

#[test]
fn compose_string_internal_range_checks() {
    let ok =
        builtin_compose_string_internal(vec![Value::string("abc"), Value::Int(0), Value::Int(0)]);
    assert!(ok.is_ok());

    let start_gt_end =
        builtin_compose_string_internal(vec![Value::string("abc"), Value::Int(2), Value::Int(1)]);
    assert!(start_gt_end.is_err());

    let end_oob =
        builtin_compose_string_internal(vec![Value::string("abc"), Value::Int(0), Value::Int(4)]);
    assert!(end_oob.is_err());

    let start_neg =
        builtin_compose_string_internal(vec![Value::string("abc"), Value::Int(-1), Value::Int(1)]);
    assert!(start_neg.is_err());
}

#[test]
fn find_composition_internal_returns_nil() {
    let result = builtin_find_composition_internal(vec![
        Value::Int(1),
        Value::Int(100),
        Value::Nil,
        Value::Nil,
    ]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn find_composition_internal_wrong_arity() {
    let result = builtin_find_composition_internal(vec![Value::Int(1)]);
    assert!(result.is_err());
}

#[test]
fn find_composition_internal_type_checks() {
    let bad_pos = builtin_find_composition_internal(vec![
        Value::symbol("x"),
        Value::Int(10),
        Value::Nil,
        Value::Nil,
    ]);
    assert!(bad_pos.is_err());

    let bad_limit = builtin_find_composition_internal(vec![
        Value::Int(1),
        Value::symbol("y"),
        Value::Nil,
        Value::Nil,
    ]);
    assert!(bad_limit.is_err());

    let bad_string = builtin_find_composition_internal(vec![
        Value::Int(1),
        Value::Nil,
        Value::Int(1),
        Value::Nil,
    ]);
    assert!(bad_string.is_err());
}

#[test]
fn find_composition_internal_position_range_checks() {
    let zero =
        builtin_find_composition_internal(vec![Value::Int(0), Value::Nil, Value::Nil, Value::Nil]);
    assert!(zero.is_err());

    let negative =
        builtin_find_composition_internal(vec![Value::Int(-1), Value::Nil, Value::Nil, Value::Nil]);
    assert!(negative.is_err());
}

#[test]
fn composition_get_gstring_returns_vector_shape() {
    let mut heap = crate::gc::heap::LispHeap::new();
    crate::emacs_core::value::set_current_heap(&mut heap);

    let result = builtin_composition_get_gstring(vec![
        Value::Int(0),
        Value::Int(1),
        Value::Nil,
        Value::string("ab"),
    ]);
    assert!(result.is_ok());
    let Value::Vector(gs) = result.unwrap() else {
        panic!("expected vector gstring");
    };
    let gs = with_heap(|h| h.get_vector(gs).clone());
    assert!(!gs.is_empty());
    assert!(matches!(gs[0], Value::Vector(_)));
}

#[test]
fn composition_get_gstring_wrong_arity() {
    let result = builtin_composition_get_gstring(vec![Value::Int(0)]);
    assert!(result.is_err());
}

#[test]
fn composition_get_gstring_type_checks() {
    let bad_from = builtin_composition_get_gstring(vec![
        Value::symbol("x"),
        Value::Int(5),
        Value::Nil,
        Value::string("hello"),
    ]);
    assert!(bad_from.is_err());

    let bad_to = builtin_composition_get_gstring(vec![
        Value::Int(0),
        Value::symbol("y"),
        Value::Nil,
        Value::string("hello"),
    ]);
    assert!(bad_to.is_err());

    let bad_string = builtin_composition_get_gstring(vec![
        Value::Int(0),
        Value::Int(5),
        Value::Nil,
        Value::Int(1),
    ]);
    assert!(bad_string.is_err());
}

#[test]
fn composition_get_gstring_range_errors() {
    let from_gt_to = builtin_composition_get_gstring(vec![
        Value::Int(2),
        Value::Int(1),
        Value::Nil,
        Value::string("ab"),
    ]);
    assert!(from_gt_to.is_err());

    let zero_length = builtin_composition_get_gstring(vec![
        Value::Int(0),
        Value::Int(0),
        Value::Nil,
        Value::string("ab"),
    ]);
    assert!(zero_length.is_err());
}

#[test]
fn clear_composition_cache_no_args() {
    let result = builtin_clear_composition_cache(vec![]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn clear_composition_cache_too_many_args() {
    let result = builtin_clear_composition_cache(vec![Value::Nil]);
    assert!(result.is_err());
}

#[test]
fn composition_sort_rules_nil_returns_nil() {
    let result = builtin_composition_sort_rules(vec![Value::Nil]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn composition_sort_rules_rejects_non_lists() {
    let result = builtin_composition_sort_rules(vec![Value::vector(vec![Value::Int(1)])]);
    assert!(result.is_err());
}

#[test]
fn composition_sort_rules_rejects_invalid_rules() {
    let rules = Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    let result = builtin_composition_sort_rules(vec![rules]);
    assert!(result.is_err());
}

#[test]
fn composition_sort_rules_accepts_cons_rules() {
    let rules = Value::list(vec![Value::cons(Value::Int(1), Value::Int(2))]);
    let result = builtin_composition_sort_rules(vec![rules]).unwrap();
    assert_eq!(result, rules);
}

#[test]
fn composition_sort_rules_wrong_arity() {
    let result = builtin_composition_sort_rules(vec![]);
    assert!(result.is_err());
}
