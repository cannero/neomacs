use super::*;

fn fresh_eval() -> super::super::eval::Context {
    reset_category_thread_locals();
    super::super::eval::Context::new()
}

#[test]
fn make_category_table_matches_gnu_shape() {
    reset_category_thread_locals();
    let table = builtin_make_category_table(vec![]).unwrap();
    assert!(builtin_category_table_p(vec![table]).unwrap().is_truthy());

    let default =
        super::super::chartable::builtin_char_table_range(vec![table, Value::NIL]).unwrap();
    assert!(
        super::super::chartable::builtin_bool_vector_p(vec![default])
            .unwrap()
            .is_truthy()
    );
    let docs =
        super::super::chartable::builtin_char_table_extra_slot(vec![table, Value::fixnum(0)]).unwrap();
    if !docs.is_vector() {
        panic!("expected docstring vector");
    };
    assert_eq!(with_heap(|h| h.get_vector(docs_arc).len()), 95);
    assert!(
        super::super::chartable::builtin_char_table_extra_slot(vec![table, Value::fixnum(1)])
            .unwrap()
            .is_nil()
    );
}

#[test]
fn copy_category_table_deep_copies_docstrings_and_sets() {
    let mut eval = fresh_eval();
    let table = builtin_make_category_table(vec![]).unwrap();
    builtin_define_category(
        &mut eval,
        vec![Value::char('!'), Value::string("bang"), table],
    )
    .unwrap();
    builtin_modify_category_entry(&mut eval, vec![Value::char('A'), Value::char('!'), table])
        .unwrap();

    let copy = builtin_copy_category_table(vec![table]).unwrap();
    builtin_define_category(
        &mut eval,
        vec![Value::char('"'), Value::string("quote"), copy],
    )
    .unwrap();
    builtin_modify_category_entry(&mut eval, vec![Value::char('B'), Value::char('!'), copy])
        .unwrap();

    assert!(
        builtin_category_docstring(&mut eval, vec![Value::char('"'), table])
            .unwrap()
            .is_nil()
    );
    assert_eq!(
        builtin_category_set_mnemonics(vec![
            super::super::chartable::builtin_char_table_range(vec![table, Value::char('B')])
                .unwrap(),
        ])
        .unwrap(),
        Value::string("")
    );
    assert_eq!(
        builtin_category_set_mnemonics(vec![
            super::super::chartable::builtin_char_table_range(vec![copy, Value::char('B')])
                .unwrap(),
        ])
        .unwrap(),
        Value::string("!")
    );

    let table_docs =
        super::super::chartable::builtin_char_table_extra_slot(vec![table, Value::fixnum(0)]).unwrap();
    let copy_docs =
        super::super::chartable::builtin_char_table_extra_slot(vec![copy, Value::fixnum(0)]).unwrap();
    let (ValueKind::Veclike(VecLikeType::Vector), ValueKind::Veclike(VecLikeType::Vector)) = (table_docs, copy_docs)
    else {
        panic!("expected category docstring vectors");
    };
    assert_ne!(table_docs_arc, copy_docs_arc);
}

#[test]
fn define_category_redefinition_matches_gnu_error() {
    let mut eval = fresh_eval();
    let table = builtin_make_category_table(vec![]).unwrap();
    builtin_define_category(
        &mut eval,
        vec![Value::char('a'), Value::string("one"), table],
    )
    .unwrap();
    let err = builtin_define_category(
        &mut eval,
        vec![Value::char('a'), Value::string("two"), table],
    )
    .unwrap_err();
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Category ‘a’ is already defined")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn get_unused_category_scans_ascii_graphics() {
    let mut eval = fresh_eval();
    let table = builtin_make_category_table(vec![]).unwrap();
    assert_eq!(
        builtin_get_unused_category(&mut eval, vec![table]).unwrap(),
        Value::char(' ')
    );
    builtin_define_category(
        &mut eval,
        vec![Value::char(' '), Value::string("space"), table],
    )
    .unwrap();
    assert_eq!(
        builtin_get_unused_category(&mut eval, vec![table]).unwrap(),
        Value::char('!')
    );
}

#[test]
fn set_category_table_nil_returns_current_table() {
    let mut eval = fresh_eval();
    let current = builtin_category_table(&mut eval, vec![]).unwrap();
    let out = builtin_set_category_table(&mut eval, vec![Value::NIL]).unwrap();
    assert_eq!(current, out);
}

#[test]
fn modify_category_entry_honors_optional_table_argument() {
    let mut eval = fresh_eval();
    let table = builtin_make_category_table(vec![]).unwrap();
    builtin_define_category(
        &mut eval,
        vec![Value::char('!'), Value::string("bang"), table],
    )
    .unwrap();
    builtin_modify_category_entry(
        &mut eval,
        vec![
            Value::cons(Value::fixnum('A' as i64), Value::fixnum('C' as i64)),
            Value::char('!'),
            table,
        ],
    )
    .unwrap();

    for ch in ['A', 'B', 'C'] {
        let set = super::super::chartable::builtin_char_table_range(vec![table, Value::char(ch)])
            .unwrap();
        assert_eq!(
            builtin_category_set_mnemonics(vec![set]).unwrap(),
            Value::string("!")
        );
    }
    let current = builtin_category_table(&mut eval, vec![]).unwrap();
    let current_set =
        super::super::chartable::builtin_char_table_range(vec![current, Value::char('A')]).unwrap();
    assert_eq!(
        builtin_category_set_mnemonics(vec![current_set]).unwrap(),
        Value::string("")
    );
}
