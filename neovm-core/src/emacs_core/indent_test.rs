use super::*;

#[test]
fn current_indentation_returns_zero() {
    let result = builtin_current_indentation(vec![]).unwrap();
    assert_eq!(result.as_int(), Some(0));
}

#[test]
fn indent_to_returns_column() {
    let result = builtin_indent_to(vec![Value::Int(42)]).unwrap();
    assert_eq!(result.as_int(), Some(42));
}

#[test]
fn indent_to_with_minimum() {
    let result = builtin_indent_to(vec![Value::Int(10), Value::Int(4)]).unwrap();
    assert_eq!(result.as_int(), Some(10));
}

#[test]
fn current_column_returns_zero() {
    let result = builtin_current_column(vec![]).unwrap();
    assert_eq!(result.as_int(), Some(0));
}

#[test]
fn move_to_column_returns_column() {
    let result = builtin_move_to_column(vec![Value::Int(15)]).unwrap();
    assert_eq!(result.as_int(), Some(15));
}

#[test]
fn move_to_column_with_force() {
    let result = builtin_move_to_column(vec![Value::Int(8), Value::True]).unwrap();
    assert_eq!(result.as_int(), Some(8));
}

#[test]
fn eval_column_and_indentation_subset() {
    let mut ev = super::super::eval::Evaluator::new();
    let forms = super::super::parser::parse_forms(
        r#"
        (with-temp-buffer
          (insert "abc")
          (goto-char (+ (point-min) 2))
          (current-column))
        (with-temp-buffer
          (insert "  abc")
          (goto-char (point-max))
          (current-indentation))
        (with-temp-buffer
          (insert "a\tb")
          (goto-char (point-min))
          (move-to-column 5)
          (list (point) (current-column)))
        "#,
    )
    .expect("parse forms");

    let col = ev.eval(&forms[0]).expect("eval current-column");
    assert_eq!(col, Value::Int(2));

    let indent = ev.eval(&forms[1]).expect("eval current-indentation");
    assert_eq!(indent, Value::Int(2));

    let move_result = ev.eval(&forms[2]).expect("eval move-to-column");
    let items = list_to_vec(&move_result).expect("list result");
    assert_eq!(items, vec![Value::Int(3), Value::Int(8)]);
}

#[test]
fn eval_move_to_column_wholenump_validation() {
    let mut ev = super::super::eval::Evaluator::new();
    let err = builtin_move_to_column_eval(&mut ev, vec![Value::string("x")]).unwrap_err();
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("wholenump"), Value::string("x")]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn eval_move_to_column_force_subset() {
    let mut ev = super::super::eval::Evaluator::new();
    let forms = super::super::parser::parse_forms(
        r#"
        (with-temp-buffer
          (insert "abc")
          (goto-char (point-min))
          (list (move-to-column 10 t) (point) (append (buffer-string) nil)))
        (with-temp-buffer
          (insert "a\tb")
          (goto-char (point-min))
          (list (move-to-column 5 t) (point) (append (buffer-string) nil)))
        "#,
    )
    .expect("parse forms");

    let first = ev.eval(&forms[0]).expect("eval first force case");
    let first_items = list_to_vec(&first).expect("first list");
    assert_eq!(first_items[0], Value::Int(10));
    assert_eq!(first_items[1], Value::Int(7));
    assert_eq!(
        list_to_vec(&first_items[2]).expect("first buffer bytes"),
        vec![
            Value::Int(97),
            Value::Int(98),
            Value::Int(99),
            Value::Int(9),
            Value::Int(32),
            Value::Int(32),
        ]
    );

    let second = ev.eval(&forms[1]).expect("eval second force case");
    let second_items = list_to_vec(&second).expect("second list");
    assert_eq!(second_items[0], Value::Int(5));
    assert_eq!(second_items[1], Value::Int(6));
    assert_eq!(
        list_to_vec(&second_items[2]).expect("second buffer bytes"),
        vec![
            Value::Int(97),
            Value::Int(32),
            Value::Int(32),
            Value::Int(32),
            Value::Int(32),
            Value::Int(9),
            Value::Int(98),
        ]
    );
}

#[test]
fn eval_back_to_indentation_subset() {
    let mut ev = super::super::eval::Evaluator::new();
    let forms = super::super::parser::parse_forms(
        r#"
        (with-temp-buffer
          (insert "  abc")
          (goto-char (point-max))
          (back-to-indentation)
          (point))
        (with-temp-buffer
          (insert "   ")
          (goto-char (point-max))
          (back-to-indentation)
          (point))
        (with-temp-buffer
          (insert (string 9 97 98 99))
          (goto-char (point-max))
          (back-to-indentation)
          (point))
        (with-temp-buffer
          (insert (string 10 32 32 97 98 99))
          (goto-char (point-max))
          (back-to-indentation)
          (point))
        "#,
    )
    .expect("parse forms");

    let first = ev.eval(&forms[0]).expect("eval nonblank line");
    assert_eq!(first, Value::Int(3));

    let second = ev.eval(&forms[1]).expect("eval whitespace-only line");
    assert_eq!(second, Value::Int(4));

    let third = ev.eval(&forms[2]).expect("eval tab-indent line");
    assert_eq!(third, Value::Int(2));

    let fourth = ev.eval(&forms[3]).expect("eval indented second line");
    assert_eq!(fourth, Value::Int(4));
}

#[test]
fn eval_indent_region_column_subset() {
    let mut ev = super::super::eval::Evaluator::new();
    let forms = super::super::parser::parse_forms(
        r#"
        (with-temp-buffer
          (insert (string 97 10 32 32 98 10 10 9 99))
          (indent-region (point-min) (point-max) 2)
          (append (buffer-string) nil))
        (with-temp-buffer
          (insert (string 97 10 32 32 98))
          (indent-region (point-min) (point-max))
          (append (buffer-string) nil))
        (with-temp-buffer
          (insert (string 97 10 98))
          (indent-region (point-max) (point-min) 1)
          (append (buffer-string) nil))
        (with-temp-buffer
          (insert "a")
          (indent-region (point-min) (point-max) "x"))
        "#,
    )
    .expect("parse forms");

    let first = ev.eval(&forms[0]).expect("eval indent-region column");
    assert_eq!(
        list_to_vec(&first).expect("first byte list"),
        vec![
            Value::Int(32),
            Value::Int(32),
            Value::Int(97),
            Value::Int(10),
            Value::Int(32),
            Value::Int(32),
            Value::Int(98),
            Value::Int(10),
            Value::Int(10),
            Value::Int(32),
            Value::Int(32),
            Value::Int(99),
        ]
    );

    let second = ev.eval(&forms[1]).expect("eval indent-region nil column");
    assert_eq!(
        list_to_vec(&second).expect("second byte list"),
        vec![Value::Int(97), Value::Int(10), Value::Int(98),]
    );

    let third = ev
        .eval(&forms[2])
        .expect("eval indent-region swapped bounds");
    assert_eq!(
        list_to_vec(&third).expect("third byte list"),
        vec![Value::Int(97), Value::Int(10), Value::Int(98),]
    );

    let fourth = ev
        .eval(&forms[3])
        .expect("eval indent-region non-numeric column");
    assert_eq!(fourth, Value::True);
}

#[test]
fn eval_indent_mode_subset() {
    let mut ev = super::super::eval::Evaluator::new();
    let forms = super::super::parser::parse_forms(
        r#"
        (with-temp-buffer
          (insert (string 32 32 97))
          (goto-char (point-max))
          (indent-according-to-mode)
          (append (buffer-string) nil))
        (with-temp-buffer
          (insert (string 32 32 97))
          (goto-char (point-max))
          (indent-according-to-mode)
          (point))
        (with-temp-buffer
          (insert (string 32 32 97))
          (goto-char (point-max))
          (reindent-then-newline-and-indent)
          (append (buffer-string) nil))
        (with-temp-buffer
          (insert (string 32 32 97))
          (goto-char (point-max))
          (reindent-then-newline-and-indent)
          (point))
        "#,
    )
    .expect("parse forms");

    let first = ev.eval(&forms[0]).expect("eval indent-according-to-mode");
    assert_eq!(
        list_to_vec(&first).expect("first byte list"),
        vec![Value::Int(97)]
    );

    let second = ev
        .eval(&forms[1])
        .expect("eval indent-according-to-mode point");
    assert_eq!(second, Value::Int(2));

    let third = ev
        .eval(&forms[2])
        .expect("eval reindent-then-newline-and-indent");
    assert_eq!(
        list_to_vec(&third).expect("third byte list"),
        vec![Value::Int(97), Value::Int(10)]
    );

    let fourth = ev
        .eval(&forms[3])
        .expect("eval reindent-then-newline-and-indent point");
    assert_eq!(fourth, Value::Int(3));
}

#[test]
fn reindent_then_newline_and_indent_normalizes_split_whitespace() {
    let mut ev = super::super::eval::Evaluator::new();
    let forms = super::super::parser::parse_forms(
        r#"(with-temp-buffer
             (insert "a b")
             (goto-char 3)
             (list (reindent-then-newline-and-indent)
                   (point)
                   (append (buffer-string) nil)))"#,
    )
    .expect("parse forms");
    let value = ev.eval(&forms[0]).expect("eval");
    let printed = super::super::print::print_value(&value);
    assert_eq!(printed, "(nil 3 (97 10 98))");
}

#[test]
fn wrong_arg_count_errors() {
    // current-indentation takes no args
    assert!(builtin_current_indentation(vec![Value::Int(1)]).is_err());
    // indent-to requires at least 1 arg
    assert!(builtin_indent_to(vec![]).is_err());
    // indent-to accepts at most 2 args
    assert!(builtin_indent_to(vec![Value::Int(1), Value::Int(2), Value::Int(3)]).is_err());
    // current-column takes no args
    assert!(builtin_current_column(vec![Value::Int(1)]).is_err());
}

#[test]
fn indent_to_rejects_non_integer() {
    assert!(builtin_indent_to(vec![Value::string("foo")]).is_err());
}

#[test]
fn init_indent_vars_sets_defaults() {
    let mut obarray = super::super::symbol::Obarray::new();
    init_indent_vars(&mut obarray);

    assert_eq!(obarray.symbol_value("tab-width").unwrap().as_int(), Some(8));
    assert!(
        obarray
            .symbol_value("indent-tabs-mode")
            .unwrap()
            .is_truthy()
    );
    assert_eq!(
        obarray.symbol_value("standard-indent").unwrap().as_int(),
        Some(4)
    );
    assert!(obarray.symbol_value("tab-stop-list").unwrap().is_nil());

    // All should be special (dynamically bound)
    assert!(obarray.is_special("tab-width"));
    assert!(obarray.is_special("indent-tabs-mode"));
    assert!(obarray.is_special("standard-indent"));
    assert!(obarray.is_special("tab-stop-list"));
}

#[test]
fn indent_for_tab_command_inserts_tab() {
    let mut ev = super::super::eval::Evaluator::new();
    let forms = super::super::parser::parse_forms(
        r#"(with-temp-buffer
             (insert "x")
             (goto-char 1)
             (indent-for-tab-command)
             (buffer-string))"#,
    )
    .expect("parse forms");
    let value = ev.eval(&forms[0]).expect("eval");
    assert_eq!(value.as_str(), Some("\tx"));
}

#[test]
fn eval_indent_to_inserts_padding_and_returns_column() {
    let mut ev = super::super::eval::Evaluator::new();
    let forms = super::super::parser::parse_forms(
        r#"(with-temp-buffer
             (insert "abcdef")
             (goto-char (point-max))
             (list (current-column)
                   (indent-to 2)
                   (current-column)))
           (with-temp-buffer
             (list (current-column)
                   (indent-to 2 5)
                   (current-column)))"#,
    )
    .expect("parse forms");

    let first = ev.eval(&forms[0]).expect("first indent-to");
    assert_eq!(super::super::print::print_value(&first), "(6 6 6)");

    let second = ev.eval(&forms[1]).expect("second indent-to");
    assert_eq!(super::super::print::print_value(&second), "(0 5 5)");
}

#[test]
fn eval_indent_to_rejects_non_fixnump_minimum() {
    let mut ev = super::super::eval::Evaluator::new();
    let forms = super::super::parser::parse_forms(
        r#"(with-temp-buffer (condition-case err (indent-to 4 nil) (error err)))
           (with-temp-buffer (condition-case err (indent-to 4 "x") (error err)))
           (with-temp-buffer (condition-case err (indent-to 4 t) (error err)))
           (with-temp-buffer (condition-case err (indent-to "x") (error err)))"#,
    )
    .expect("parse forms");

    let results = ev.eval_forms(&forms);
    let printed: Vec<String> = results
        .iter()
        .map(super::super::format_eval_result)
        .collect();

    assert_eq!(printed[0], "OK 4");
    assert_eq!(printed[1], r#"OK (wrong-type-argument fixnump "x")"#);
    assert_eq!(printed[2], "OK (wrong-type-argument fixnump t)");
    assert_eq!(printed[3], r#"OK (wrong-type-argument fixnump "x")"#);
}

#[test]
fn indent_for_tab_command_normalizes_leading_whitespace_at_point() {
    let mut ev = super::super::eval::Evaluator::new();
    let forms = super::super::parser::parse_forms(
        r#"(with-temp-buffer
             (insert "  x")
             (goto-char 3)
             (list (indent-for-tab-command) (point) (append (buffer-string) nil)))"#,
    )
    .expect("parse forms");
    let value = ev.eval(&forms[0]).expect("eval");
    let printed = super::super::print::print_value(&value);
    assert_eq!(printed, "(nil 2 (9 120))");
}
