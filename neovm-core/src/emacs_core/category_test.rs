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
        super::super::chartable::builtin_char_table_range(vec![table, Value::Nil]).unwrap();
    assert!(
        super::super::chartable::builtin_bool_vector_p(vec![default])
            .unwrap()
            .is_truthy()
    );
    let docs =
        super::super::chartable::builtin_char_table_extra_slot(vec![table, Value::Int(0)]).unwrap();
    let Value::Vector(docs_arc) = docs else {
        panic!("expected docstring vector");
    };
    assert_eq!(with_heap(|h| h.get_vector(docs_arc).len()), 95);
    assert!(
        super::super::chartable::builtin_char_table_extra_slot(vec![table, Value::Int(1)])
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
        vec![Value::Char('!'), Value::string("bang"), table],
    )
    .unwrap();
    builtin_modify_category_entry(&mut eval, vec![Value::Char('A'), Value::Char('!'), table])
        .unwrap();

    let copy = builtin_copy_category_table(vec![table]).unwrap();
    builtin_define_category(
        &mut eval,
        vec![Value::Char('"'), Value::string("quote"), copy],
    )
    .unwrap();
    builtin_modify_category_entry(&mut eval, vec![Value::Char('B'), Value::Char('!'), copy])
        .unwrap();

    assert!(
        builtin_category_docstring(&mut eval, vec![Value::Char('"'), table])
            .unwrap()
            .is_nil()
    );
    assert_eq!(
        builtin_category_set_mnemonics(vec![
            super::super::chartable::builtin_char_table_range(vec![table, Value::Char('B')])
                .unwrap(),
        ])
        .unwrap(),
        Value::string("")
    );
    assert_eq!(
        builtin_category_set_mnemonics(vec![
            super::super::chartable::builtin_char_table_range(vec![copy, Value::Char('B')])
                .unwrap(),
        ])
        .unwrap(),
        Value::string("!")
    );

    let table_docs =
        super::super::chartable::builtin_char_table_extra_slot(vec![table, Value::Int(0)]).unwrap();
    let copy_docs =
        super::super::chartable::builtin_char_table_extra_slot(vec![copy, Value::Int(0)]).unwrap();
    let (Value::Vector(table_docs_arc), Value::Vector(copy_docs_arc)) = (table_docs, copy_docs)
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
        vec![Value::Char('a'), Value::string("one"), table],
    )
    .unwrap();
    let err = builtin_define_category(
        &mut eval,
        vec![Value::Char('a'), Value::string("two"), table],
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
        Value::Char(' ')
    );
    builtin_define_category(
        &mut eval,
        vec![Value::Char(' '), Value::string("space"), table],
    )
    .unwrap();
    assert_eq!(
        builtin_get_unused_category(&mut eval, vec![table]).unwrap(),
        Value::Char('!')
    );
}

#[test]
fn set_category_table_nil_returns_current_table() {
    let mut eval = fresh_eval();
    let current = builtin_category_table(&mut eval, vec![]).unwrap();
    let out = builtin_set_category_table(&mut eval, vec![Value::Nil]).unwrap();
    assert_eq!(current, out);
}

#[test]
fn modify_category_entry_honors_optional_table_argument() {
    let mut eval = fresh_eval();
    let table = builtin_make_category_table(vec![]).unwrap();
    builtin_define_category(
        &mut eval,
        vec![Value::Char('!'), Value::string("bang"), table],
    )
    .unwrap();
    builtin_modify_category_entry(
        &mut eval,
        vec![
            Value::cons(Value::Int('A' as i64), Value::Int('C' as i64)),
            Value::Char('!'),
            table,
        ],
    )
    .unwrap();

    for ch in ['A', 'B', 'C'] {
        let set = super::super::chartable::builtin_char_table_range(vec![table, Value::Char(ch)])
            .unwrap();
        assert_eq!(
            builtin_category_set_mnemonics(vec![set]).unwrap(),
            Value::string("!")
        );
    }
    let current = builtin_category_table(&mut eval, vec![]).unwrap();
    let current_set =
        super::super::chartable::builtin_char_table_range(vec![current, Value::Char('A')]).unwrap();
    assert_eq!(
        builtin_category_set_mnemonics(vec![current_set]).unwrap(),
        Value::string("")
    );
}
