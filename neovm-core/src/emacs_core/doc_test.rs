use super::*;
use crate::emacs_core::builtins::builtin_documentation_stringp;
use crate::emacs_core::{Context, format_eval_result};
use crate::test_utils::{
    load_minimal_gnu_help_runtime, runtime_startup_context, runtime_startup_eval_all,
};

fn bootstrap_eval_all(src: &str) -> Vec<String> {
    runtime_startup_eval_all(src)
}

#[test]
fn raw_documentation_property_does_not_require_substitute_command_keys() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    eval.obarray_mut().put_property(
        "vm-doc-prop",
        "variable-documentation",
        Value::string("Press \\[save-buffer] to save."),
    );

    let result = builtin_documentation_property(
        &mut eval,
        vec![
            Value::symbol("vm-doc-prop"),
            Value::symbol("variable-documentation"),
        ],
    )
    .expect("raw documentation-property should succeed");
    assert_eq!(result.as_str(), Some("Press \\[save-buffer] to save."));
}

#[test]
fn runtime_documentation_property_uses_gnu_substitute_command_keys() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    load_minimal_gnu_help_runtime(&mut eval);
    eval.obarray_mut().put_property(
        "vm-doc-prop",
        "variable-documentation",
        Value::string("Press \\[save-buffer] to save."),
    );

    let result = builtin_documentation_property(
        &mut eval,
        vec![
            Value::symbol("vm-doc-prop"),
            Value::symbol("variable-documentation"),
        ],
    )
    .expect("runtime documentation-property should succeed");
    let text = result.as_str().expect("runtime doc should stay string");
    assert!(text.contains("save-buffer"));
    assert!(!text.contains("\\["));
}

// =======================================================================
// documentation-property (stub)
// =======================================================================

#[test]
fn documentation_property_returns_nil() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let result = builtin_documentation_property(
        &mut eval,
        vec![
            Value::symbol("foo"),
            Value::symbol("variable-documentation"),
        ],
    );
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn documentation_property_with_raw() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let result = builtin_documentation_property(
        &mut eval,
        vec![
            Value::symbol("foo"),
            Value::symbol("variable-documentation"),
            Value::T,
        ],
    );
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn documentation_property_wrong_type() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let result = builtin_documentation_property(
        &mut eval,
        vec![Value::fixnum(42), Value::symbol("variable-documentation")],
    );
    assert!(result.is_err());
}

#[test]
fn documentation_property_wrong_arity() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let result = builtin_documentation_property(&mut eval, vec![Value::symbol("foo")]);
    assert!(result.is_err());
}

// =======================================================================
// Snarf-documentation runtime/error semantics
// =======================================================================

#[test]
fn snarf_documentation_returns_nil() {
    crate::test_utils::init_test_tracing();
    let result = builtin_snarf_documentation(vec![Value::string("DOC")]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn snarf_documentation_wrong_type() {
    crate::test_utils::init_test_tracing();
    let result = builtin_snarf_documentation(vec![Value::fixnum(42)]);
    assert!(result.is_err());
}

#[test]
fn snarf_documentation_empty_path_errors() {
    crate::test_utils::init_test_tracing();
    let result = builtin_snarf_documentation(vec![Value::string("")]);
    match result {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "error"),
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn snarf_documentation_parent_dir_path_errors() {
    crate::test_utils::init_test_tracing();
    let result = builtin_snarf_documentation(vec![Value::string("../")]);
    match result {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "error"),
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn snarf_documentation_single_dot_path_errors() {
    crate::test_utils::init_test_tracing();
    let result = builtin_snarf_documentation(vec![Value::string(".")]);
    match result {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "error"),
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn snarf_documentation_root_path_errors() {
    crate::test_utils::init_test_tracing();
    let result = builtin_snarf_documentation(vec![Value::string("/")]);
    match result {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "error"),
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn snarf_documentation_doc_dir_path_file_error() {
    crate::test_utils::init_test_tracing();
    let result = builtin_snarf_documentation(vec![Value::string("DOC/")]);
    match result {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "file-error"),
        other => panic!("expected file-error signal, got {other:?}"),
    }
}

#[test]
fn snarf_documentation_doc_subpath_file_error() {
    crate::test_utils::init_test_tracing();
    let result = builtin_snarf_documentation(vec![Value::string("DOC/a")]);
    match result {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "file-error"),
        other => panic!("expected file-error signal, got {other:?}"),
    }
}

#[test]
fn snarf_documentation_missing_path_errors() {
    crate::test_utils::init_test_tracing();
    let result = builtin_snarf_documentation(vec![Value::string("NO_SUCH_DOC_FILE")]);
    match result {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "file-missing"),
        other => panic!("expected file-missing signal, got {other:?}"),
    }
}

#[test]
fn snarf_documentation_missing_dir_path_errors() {
    crate::test_utils::init_test_tracing();
    let result = builtin_snarf_documentation(vec![Value::string("NO_SUCH_DOC_DIR/")]);
    match result {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "file-missing"),
        other => panic!("expected file-missing signal, got {other:?}"),
    }
}

#[test]
fn snarf_documentation_wrong_arity() {
    crate::test_utils::init_test_tracing();
    let result = builtin_snarf_documentation(vec![]);
    assert!(result.is_err());
}

// =======================================================================
// help-function-arglist
// =======================================================================

#[test]
fn help_function_arglist_is_real_lisp_function_after_bootstrap() {
    crate::test_utils::init_test_tracing();
    let eval = runtime_startup_context();
    let function = eval
        .obarray
        .symbol_function("help-function-arglist")
        .expect("missing help-function-arglist bootstrapped function cell");
    assert!(!crate::emacs_core::autoload::is_autoload_value(&function));
}

#[test]
fn help_function_arglist_loads_from_gnu_help_el() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        r#"(list (help-function-arglist 'car)
                 (help-function-arglist 'car t)
                 (help-function-arglist 'describe-function)
                 (subrp (symbol-function 'help-function-arglist)))"#,
    );
    assert_eq!(
        results[0],
        r#"OK ((arg1) (list) "[Arg list not available until function definition is loaded.]" nil)"#
    );
}

#[test]
fn help_function_arglist_loaded_supports_lambda_forms() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        r#"(list (help-function-arglist '(lambda (x y) x))
                 (help-function-arglist '(lambda x x))
                 (help-function-arglist '(macro lambda)))"#,
    );
    assert_eq!(results[0], r#"OK ((x y) x nil)"#);
}

#[test]
fn help_function_arglist_loaded_wrong_arity_matches_gnu() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        r#"(condition-case err
               (help-function-arglist)
             (error (list 'err (car err))))"#,
    );
    assert_eq!(results[0], r#"OK (err wrong-number-of-arguments)"#);
}

// =======================================================================
// documentation (eval-dependent)
// =======================================================================

#[test]
fn documentation_lambda_with_docstring() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();

    // Set up a lambda with a docstring in the function cell.
    let lambda = Value::make_lambda(LambdaData {
        params: LambdaParams::simple(vec![intern("x")]),
        body: vec![].into(),
        env: None,
        docstring: Some("Add one to X.".to_string()),
        doc_form: None,
        interactive: None,
    });
    evaluator.obarray.set_symbol_function("my-fn", lambda);

    let result = builtin_documentation(&mut evaluator, vec![Value::symbol("my-fn")]);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().as_str(), Some("Add one to X."));
}

#[test]
fn documentation_lambda_no_docstring() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();

    let lambda = Value::make_lambda(LambdaData {
        params: LambdaParams::simple(vec![]),
        body: vec![].into(),
        env: None,
        docstring: None,
        doc_form: None,
        interactive: None,
    });
    evaluator.obarray.set_symbol_function("no-doc", lambda);

    let result = builtin_documentation(&mut evaluator, vec![Value::symbol("no-doc")]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn documentation_substitutes_command_keys_unless_raw() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    // `(documentation 'foo)` calls `substitute-command-keys` on the
    // raw doc when RAW is nil. That function lives in `lisp/help.el`,
    // not in C/Rust, so the test must load enough of the GNU runtime
    // for help.el's `defun substitute-command-keys` to be reachable.
    // Mirrors GNU loadup.el ordering, which loads help.el before any
    // documentation query.
    crate::test_utils::load_minimal_gnu_help_runtime(&mut evaluator);
    let lambda = Value::make_lambda(LambdaData {
        params: LambdaParams::simple(vec![]),
        body: vec![Value::symbol("t")],
        env: None,
        docstring: Some("Press \\[save-buffer] to save.".to_string()),
        doc_form: None,
        interactive: None,
    });
    evaluator.obarray.set_symbol_function("doc-raw-fn", lambda);

    let display = builtin_documentation(&mut evaluator, vec![Value::symbol("doc-raw-fn")]).unwrap();
    let raw =
        builtin_documentation(&mut evaluator, vec![Value::symbol("doc-raw-fn"), Value::T]).unwrap();

    let display = display.as_str().expect("display documentation string");
    let raw = raw.as_str().expect("raw documentation string");
    assert!(display.contains("save-buffer"));
    assert!(!display.contains("\\["));
    assert!(raw.contains("\\[save-buffer]"));
}

#[test]
fn documentation_unbound_function() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation(&mut evaluator, vec![Value::symbol("nonexistent")]);
    assert!(result.is_err());
}

#[test]
fn documentation_subr() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    evaluator
        .obarray
        .set_symbol_function("plus", Value::subr(intern("+")));

    let result = builtin_documentation(&mut evaluator, vec![Value::symbol("plus")]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_string());
}

#[test]
fn documentation_car_subr_uses_oracle_text_shape() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    evaluator
        .obarray
        .set_symbol_function("car", Value::subr(intern("car")));

    let result = builtin_documentation(&mut evaluator, vec![Value::symbol("car")]).unwrap();
    let text = result
        .as_str()
        .expect("documentation for car should return a string");
    assert!(text.starts_with("Return the car of LIST.  If LIST is nil, return nil."));
    assert_ne!(text, "Built-in function.");
}

#[test]
fn documentation_if_special_form_uses_oracle_text_shape() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    evaluator
        .obarray
        .set_symbol_function("if", Value::subr(intern("if")));

    let result = builtin_documentation(&mut evaluator, vec![Value::symbol("if")]).unwrap();
    let text = result
        .as_str()
        .expect("documentation for if should return a string");
    assert!(text.starts_with("If COND yields non-nil, do THEN, else do ELSE..."));
    assert_ne!(text, "Built-in function.");
}

#[test]
fn documentation_core_subr_stubs_use_oracle_first_line_shapes() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    // The shim now stores raw grave-quoted text matching GNU's actual
    // DEFUN doc: comments. With bare `Context::new()' there's no
    // substitute-command-keys yet, so the prefix is checked against
    // the raw form. Once help.el loads,
    // `substitute-command-keys' will rewrite the quotes per
    // `text-quoting-style' (see the post-shim integration test below).
    let probes = [
        (
            "cons",
            "Create a new cons, give it CAR and CDR as components, and return it.",
        ),
        (
            "list",
            "Return a newly created list with specified arguments as elements.",
        ),
        ("eq", "Return t if the two args are the same Lisp object."),
        (
            "equal",
            "Return t if two Lisp objects have similar structure and contents.",
        ),
        (
            "length",
            "Return the length of vector, list or string SEQUENCE.",
        ),
        (
            "append",
            "Concatenate all the arguments and make the result a list.",
        ),
        (
            "mapcar",
            "Apply FUNCTION to each element of SEQUENCE, and make a list of the results.",
        ),
        (
            "assoc",
            "Return non-nil if KEY is equal to the car of an element of ALIST.",
        ),
        (
            "member",
            "Return non-nil if ELT is an element of LIST.  Comparison done with `equal'.",
        ),
        ("symbol-name", "Return SYMBOL's name, a string."),
    ];

    for (name, expected_prefix) in probes {
        evaluator
            .obarray
            .set_symbol_function(name, Value::subr(intern(name)));
        let result = builtin_documentation(&mut evaluator, vec![Value::symbol(name)]).unwrap();
        let text = result
            .as_str()
            .expect("core subr documentation should return a string");
        assert!(
            text.starts_with(expected_prefix),
            "unexpected documentation text for {name}: {text:?}"
        );
        assert_ne!(text, "Built-in function.");
    }
}

#[test]
fn documentation_symbol_alias_to_builtin_returns_docstring() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    evaluator
        .obarray
        .set_symbol_function("alias-builtin", Value::symbol("car"));

    let result =
        builtin_documentation(&mut evaluator, vec![Value::symbol("alias-builtin")]).unwrap();
    let text = result
        .as_str()
        .expect("documentation alias to car should return a string");
    assert!(text.starts_with("Return the car of LIST.  If LIST is nil, return nil."));
    assert_ne!(text, "Built-in function.");
}

#[test]
fn documentation_prefers_function_documentation_property() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    evaluator
        .obarray
        .set_symbol_function("doc-prop", Value::fixnum(7));
    evaluator.obarray.put_property(
        "doc-prop",
        "function-documentation",
        Value::string("propdoc"),
    );

    let result = builtin_documentation(&mut evaluator, vec![Value::symbol("doc-prop")]);
    assert_eq!(result.unwrap().as_str(), Some("propdoc"));
}

#[test]
fn documentation_integer_function_documentation_property_returns_nil() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    evaluator
        .obarray
        .set_symbol_function("doc-prop", Value::fixnum(7));
    evaluator
        .obarray
        .put_property("doc-prop", "function-documentation", Value::fixnum(9));

    let result = builtin_documentation(&mut evaluator, vec![Value::symbol("doc-prop")]);
    assert!(result.unwrap().is_nil());
}

#[test]
fn documentation_list_function_documentation_property_is_evaluated() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    evaluator
        .obarray
        .set_symbol_function("doc-prop", Value::fixnum(7));
    evaluator.obarray.put_property(
        "doc-prop",
        "function-documentation",
        Value::list(vec![Value::symbol("identity"), Value::string("doc")]),
    );

    let result = builtin_documentation(&mut evaluator, vec![Value::symbol("doc-prop")]);
    assert_eq!(result.unwrap().as_str(), Some("doc"));
}

#[test]
fn documentation_symbol_function_documentation_property_is_evaluated() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    evaluator
        .obarray
        .set_symbol_function("doc-prop", Value::fixnum(7));
    evaluator
        .obarray
        .put_property("doc-prop", "function-documentation", Value::symbol("t"));

    let result = builtin_documentation(&mut evaluator, vec![Value::symbol("doc-prop")]);
    assert!(result.unwrap().is_truthy());
}

#[test]
fn documentation_vector_function_documentation_property_is_evaluated() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    evaluator
        .obarray
        .set_symbol_function("doc-prop", Value::fixnum(7));
    evaluator.obarray.put_property(
        "doc-prop",
        "function-documentation",
        Value::vector(vec![Value::fixnum(1), Value::fixnum(2)]),
    );

    let result = builtin_documentation(&mut evaluator, vec![Value::symbol("doc-prop")]);
    assert!(result.unwrap().is_vector());
}

#[test]
fn documentation_unbound_symbol_function_documentation_property_errors() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    evaluator
        .obarray
        .set_symbol_function("doc-prop", Value::fixnum(7));
    evaluator.obarray.put_property(
        "doc-prop",
        "function-documentation",
        Value::symbol("doc-prop-unbound"),
    );

    let result = builtin_documentation(&mut evaluator, vec![Value::symbol("doc-prop")]);
    match result {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "void-variable"),
        other => panic!("expected void-variable signal, got {other:?}"),
    }
}

#[test]
fn documentation_invalid_form_function_documentation_property_errors() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    evaluator
        .obarray
        .set_symbol_function("doc-prop", Value::fixnum(7));
    evaluator.obarray.put_property(
        "doc-prop",
        "function-documentation",
        Value::list(vec![Value::fixnum(1), Value::fixnum(2)]),
    );

    let result = builtin_documentation(&mut evaluator, vec![Value::symbol("doc-prop")]);
    match result {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "invalid-function"),
        other => panic!("expected invalid-function signal, got {other:?}"),
    }
}

#[test]
fn documentation_quoted_lambda_docstring() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let quoted = Value::list(vec![
        Value::symbol("lambda"),
        Value::list(vec![Value::symbol("x")]),
        Value::string("d"),
        Value::symbol("x"),
    ]);

    let result = builtin_documentation(&mut evaluator, vec![quoted]).unwrap();
    assert_eq!(result.as_str(), Some("d"));
}

#[test]
fn documentation_quoted_lambda_without_docstring_returns_nil() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let quoted = Value::list(vec![
        Value::symbol("lambda"),
        Value::list(vec![Value::symbol("x")]),
        Value::symbol("x"),
    ]);

    let result = builtin_documentation(&mut evaluator, vec![quoted]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn documentation_vector_designator_returns_keyboard_macro_doc() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result =
        builtin_documentation(&mut evaluator, vec![Value::vector(vec![Value::fixnum(1)])]).unwrap();
    assert_eq!(result.as_str(), Some("Keyboard macro."));
}

#[test]
fn documentation_string_designator_returns_keyboard_macro_doc() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation(&mut evaluator, vec![Value::string("abc")]).unwrap();
    assert_eq!(result.as_str(), Some("Keyboard macro."));
}

#[test]
fn documentation_quoted_macro_payload_matches_oracle_shape() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let quoted = Value::list(vec![
        Value::symbol("macro"),
        Value::list(vec![Value::symbol("x")]),
        Value::string("md"),
        Value::symbol("x"),
    ]);

    let result = builtin_documentation(&mut evaluator, vec![quoted]);
    match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "invalid-function");
            assert_eq!(
                sig.data.first(),
                Some(&Value::list(vec![
                    Value::list(vec![Value::symbol("x")]),
                    Value::string("md"),
                    Value::symbol("x"),
                ]))
            );
        }
        other => panic!("expected invalid-function signal, got {other:?}"),
    }
}

#[test]
fn documentation_empty_quoted_macro_errors_void_function_nil() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let quoted = Value::list(vec![Value::symbol("macro")]);

    let result = builtin_documentation(&mut evaluator, vec![quoted]);
    match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "void-function");
            assert!(sig.data.first().is_some_and(|v| v.is_nil()));
        }
        other => panic!("expected void-function signal, got {other:?}"),
    }
}

#[test]
fn documentation_non_symbol_non_function_errors_invalid_function() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation(
        &mut evaluator,
        vec![Value::list(vec![Value::fixnum(1), Value::fixnum(2)])],
    );
    assert!(result.is_err());
}

#[test]
fn documentation_wrong_arity() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation(&mut evaluator, vec![]);
    assert!(result.is_err());
}

#[test]
fn startup_doc_quote_style_display_handles_backtick_pairs() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        startup_doc_quote_style_display("`C source code`."),
        "‘C source code’."
    );
    assert_eq!(
        startup_doc_quote_style_display("`default-directory'"),
        "‘default-directory’"
    );
    assert_eq!(
        startup_doc_quote_style_display("Keymap for subcommands of \\`C-x 4'."),
        "Keymap for subcommands of C-x 4."
    );
}

#[test]
fn documentation_property_eval_returns_string_property() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    evaluator
        .obarray
        .put_property("doc-sym", "variable-documentation", Value::string("doc"));

    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("doc-sym"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert_eq!(result.as_str(), Some("doc"));
}

#[test]
fn documentation_property_eval_substitutes_command_keys_unless_raw() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    // See `documentation_substitutes_command_keys_unless_raw' for why
    // help.el must be loaded before exercising the substitute path.
    crate::test_utils::load_minimal_gnu_help_runtime(&mut evaluator);
    evaluator.obarray.put_property(
        "doc-sym",
        "variable-documentation",
        Value::string("Press \\[save-buffer] to save."),
    );

    let display = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("doc-sym"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    let raw = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("doc-sym"),
            Value::symbol("variable-documentation"),
            Value::T,
        ],
    )
    .unwrap();

    let display = display
        .as_str()
        .expect("display documentation-property string");
    let raw = raw.as_str().expect("raw documentation-property string");
    assert!(display.contains("save-buffer"));
    assert!(!display.contains("\\["));
    assert!(raw.contains("\\[save-buffer]"));
}

#[test]
fn documentation_property_eval_integer_property_returns_nil() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    evaluator
        .obarray
        .put_property("doc-sym", "variable-documentation", Value::fixnum(7));

    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("doc-sym"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn documentation_stringp_accepts_compiled_file_refs() {
    crate::test_utils::init_test_tracing();
    let doc_ref = Value::cons(Value::string("/tmp/docref.elc"), Value::fixnum(17));
    let result = builtin_documentation_stringp(vec![doc_ref]).unwrap();
    assert!(result.is_truthy());
}

#[test]
fn documentation_property_eval_reads_compiled_doc_ref() {
    crate::test_utils::init_test_tracing();
    let unique = format!(
        "neovm-docref-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_nanos()
    );
    let path = std::env::temp_dir().join(format!("{unique}.elc"));
    std::fs::write(&path, b"#@11 compiled doc\x1f").expect("write doc fixture");

    let mut evaluator = super::super::eval::Context::new();
    evaluator.obarray.put_property(
        "doc-sym",
        "variable-documentation",
        Value::cons(
            Value::string(path.to_string_lossy().into_owned()),
            Value::fixnum(5),
        ),
    );

    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("doc-sym"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();

    assert_eq!(result.as_str(), Some("compiled doc"));

    let _ = std::fs::remove_file(path);
}

#[test]
fn documentation_property_eval_load_path_integer_property_returns_string() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("load-path"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert!(
        result
            .as_str()
            .is_some_and(|s| s.contains("List of directories to search for files to load"))
    );
}

#[test]
fn documentation_property_eval_load_path_raw_t_preserves_ascii_quotes() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let display = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("load-path"),
            Value::symbol("variable-documentation"),
            Value::NIL,
        ],
    )
    .unwrap();
    let raw = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("load-path"),
            Value::symbol("variable-documentation"),
            Value::T,
        ],
    )
    .unwrap();
    let display = display
        .as_str()
        .expect("display documentation-property should return a string");
    let raw = raw
        .as_str()
        .expect("raw documentation-property should return a string");

    assert_ne!(display, raw);
    assert!(display.contains("‘default-directory’"));
    assert!(raw.contains("`default-directory'"));
}

#[test]
fn documentation_property_eval_ctl_x_4_map_raw_matches_display_when_no_markup() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let display = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("ctl-x-4-map"),
            Value::symbol("variable-documentation"),
            Value::NIL,
        ],
    )
    .unwrap();
    let raw = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("ctl-x-4-map"),
            Value::symbol("variable-documentation"),
            Value::T,
        ],
    )
    .unwrap();
    let display = display
        .as_str()
        .expect("display documentation-property should return a string");
    let raw = raw
        .as_str()
        .expect("raw documentation-property should return a string");

    assert!(display.contains("C-x 4"));
    assert!(!display.contains("\\`C-x 4'"));
    assert_eq!(raw, display);
}

#[test]
fn documentation_property_eval_case_fold_search_integer_property_returns_string() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("case-fold-search"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert!(
        result
            .as_str()
            .is_some_and(|s| s.contains("searches and matches should ignore case"))
    );
}

#[test]
fn documentation_property_eval_unread_command_events_integer_property_returns_string() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("unread-command-events"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert!(
        result
            .as_str()
            .is_some_and(|s| s.contains("events to be read as the command input"))
    );
}

#[test]
fn documentation_property_eval_auto_hscroll_mode_integer_property_returns_string() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("auto-hscroll-mode"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert!(
        result
            .as_str()
            .is_some_and(|s| s.contains("automatic horizontal scrolling of windows"))
    );
}

#[test]
fn documentation_property_eval_auto_composition_mode_integer_property_returns_string() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("auto-composition-mode"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert!(
        result
            .as_str()
            .is_some_and(|s| s.contains("Auto-Composition mode is enabled"))
    );
}

#[test]
fn documentation_property_eval_coding_system_alist_integer_property_returns_string() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("coding-system-alist"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert!(
        result
            .as_str()
            .is_some_and(|s| s.contains("Alist of coding system names"))
    );
}

#[test]
fn documentation_property_eval_debug_on_message_integer_property_returns_string() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("debug-on-message"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert!(
        result
            .as_str()
            .is_some_and(|s| s.contains("debug if a message matching this regexp is displayed"))
    );
}

#[test]
fn documentation_property_eval_display_hourglass_integer_property_returns_string() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("display-hourglass"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert!(
        result
            .as_str()
            .is_some_and(|s| s.contains("show an hourglass pointer"))
    );
}

#[test]
fn documentation_property_eval_exec_directory_integer_property_returns_string() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("exec-directory"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert!(
        result
            .as_str()
            .is_some_and(|s| s.contains("Directory for executables for Emacs to invoke"))
    );
}

#[test]
fn documentation_property_eval_frame_title_format_integer_property_returns_string() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("frame-title-format"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert!(
        result
            .as_str()
            .is_some_and(|s| s.contains("Template for displaying the title bar of visible frames"))
    );
}

#[test]
fn documentation_property_eval_header_line_format_integer_property_returns_string() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("header-line-format"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert!(
        result
            .as_str()
            .is_some_and(|s| s.contains("controls the header line"))
    );
}

#[test]
fn documentation_property_eval_input_method_function_integer_property_returns_string() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("input-method-function"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert!(
        result
            .as_str()
            .is_some_and(|s| s.contains("implements the current input method"))
    );
}

#[test]
fn documentation_property_eval_load_suffixes_integer_property_returns_string() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("load-suffixes"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert!(
        result
            .as_str()
            .is_some_and(|s| s.contains("suffixes for Emacs Lisp files and dynamic modules"))
    );
}

#[test]
fn documentation_property_eval_native_comp_eln_load_path_integer_property_returns_string() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("native-comp-eln-load-path"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    // The doc text now comes from the var_docs GNU table (Phase
    // A7-A10), which preserves GNU's actual wording: "native-
    // compiled *.eln files" rather than the legacy STUBS shim's
    // "natively-compiled". The legacy spelling was a transcription
    // artifact -- the GNU source uses "native-compiled".
    assert!(
        result
            .as_str()
            .is_some_and(|s| s.contains("native-compiled *.eln files"))
    );
}

#[test]
fn documentation_property_eval_process_environment_integer_property_returns_string() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("process-environment"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert!(
        result
            .as_str()
            .is_some_and(|s| s.contains("environment variables for subprocesses"))
    );
}

#[test]
fn documentation_property_eval_scroll_margin_integer_property_returns_string() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("scroll-margin"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert!(
        result
            .as_str()
            .is_some_and(|s| s.contains("margin at the top and bottom"))
    );
}

#[test]
fn documentation_property_eval_truncate_partial_width_windows_integer_property_returns_string() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("truncate-partial-width-windows"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert!(
        result
            .as_str()
            .is_some_and(|s| s.contains("windows narrower than the frame"))
    );
}

#[test]
fn documentation_property_eval_yes_or_no_prompt_integer_property_returns_string() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("yes-or-no-prompt"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert!(result.as_str().is_some_and(|s| s.contains("append when")));
}

#[test]
fn documentation_property_eval_debug_on_error_integer_property_returns_string() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("debug-on-error"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert!(
        result
            .as_str()
            .is_some_and(|s| s.contains("Non-nil means enter debugger if an error is signaled"))
    );
}

#[test]
fn documentation_property_eval_list_property_is_evaluated() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    evaluator.obarray.put_property(
        "doc-sym",
        "variable-documentation",
        Value::list(vec![Value::symbol("identity"), Value::string("doc")]),
    );

    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("doc-sym"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert_eq!(result.as_str(), Some("doc"));
}

#[test]
fn documentation_property_eval_symbol_property_is_evaluated() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    evaluator
        .obarray
        .put_property("doc-sym", "variable-documentation", Value::symbol("t"));

    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("doc-sym"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert!(result.is_truthy());
}

#[test]
fn documentation_property_eval_vector_property_is_evaluated() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    evaluator.obarray.put_property(
        "doc-sym",
        "variable-documentation",
        Value::vector(vec![Value::fixnum(1), Value::fixnum(2)]),
    );

    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("doc-sym"),
            Value::symbol("variable-documentation"),
        ],
    )
    .unwrap();
    assert!(result.is_vector());
}

#[test]
fn documentation_property_eval_unbound_symbol_property_errors() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    evaluator.obarray.put_property(
        "doc-sym",
        "variable-documentation",
        Value::symbol("doc-sym-unbound"),
    );

    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("doc-sym"),
            Value::symbol("variable-documentation"),
        ],
    );
    match result {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "void-variable"),
        other => panic!("expected void-variable signal, got {other:?}"),
    }
}

#[test]
fn documentation_property_eval_invalid_form_property_errors() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    evaluator.obarray.put_property(
        "doc-sym",
        "variable-documentation",
        Value::list(vec![Value::fixnum(1), Value::fixnum(2)]),
    );

    let result = builtin_documentation_property(
        &mut evaluator,
        vec![
            Value::symbol("doc-sym"),
            Value::symbol("variable-documentation"),
        ],
    );
    match result {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "invalid-function"),
        other => panic!("expected invalid-function signal, got {other:?}"),
    }
}

#[test]
fn documentation_property_eval_non_symbol_prop_returns_nil() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    evaluator
        .obarray
        .put_property("doc-sym", "x", Value::string("v"));

    let result = builtin_documentation_property(
        &mut evaluator,
        vec![Value::symbol("doc-sym"), Value::fixnum(1)],
    )
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn documentation_property_eval_non_symbol_target_errors() {
    crate::test_utils::init_test_tracing();
    let mut evaluator = super::super::eval::Context::new();
    let result = builtin_documentation_property(
        &mut evaluator,
        vec![Value::fixnum(1), Value::symbol("variable-documentation")],
    );
    assert!(result.is_err());
}
