use super::*;
use crate::emacs_core::expr::Expr;
use crate::emacs_core::load::{apply_runtime_startup_state, create_bootstrap_evaluator_cached};
use crate::emacs_core::value::{LambdaData, LambdaParams};
use crate::emacs_core::{format_eval_result, parse_forms};

fn install_variable_watcher_probe(eval: &mut crate::emacs_core::eval::Evaluator, callback: &str) {
    let lambda = Value::make_lambda(LambdaData {
        params: LambdaParams {
            required: vec![
                intern("symbol"),
                intern("newval"),
                intern("operation"),
                intern("where"),
            ],
            optional: Vec::new(),
            rest: None,
        },
        body: vec![
            Expr::List(vec![
                Expr::Symbol(intern("setq")),
                Expr::Symbol(intern("vm-watcher-last-op")),
                Expr::Symbol(intern("operation")),
            ]),
            Expr::List(vec![
                Expr::Symbol(intern("setq")),
                Expr::Symbol(intern("vm-watcher-last-symbol")),
                Expr::Symbol(intern("symbol")),
            ]),
            Expr::List(vec![
                Expr::Symbol(intern("setq")),
                Expr::Symbol(intern("vm-watcher-last-value")),
                Expr::Symbol(intern("newval")),
            ]),
            Expr::Symbol(intern("newval")),
        ]
        .into(),
        env: None,
        docstring: None,
        doc_form: None,
    });
    eval.obarray_mut().set_symbol_function(callback, lambda);
}

fn install_noarg_hook_probe(
    eval: &mut crate::emacs_core::eval::Evaluator,
    callback: &str,
    body: Vec<Expr>,
) {
    let lambda = Value::make_lambda(LambdaData {
        params: LambdaParams::simple(vec![]),
        body: body.into(),
        env: None,
        docstring: None,
        doc_form: None,
    });
    eval.obarray_mut().set_symbol_function(callback, lambda);
}

fn create_unique_test_buffer(eval: &mut crate::emacs_core::eval::Evaluator, name: &str) -> Value {
    let unique_name = eval.buffers.generate_new_buffer_name(name);
    Value::Buffer(eval.buffers.create_buffer(&unique_name))
}

fn bootstrap_eval_all(src: &str) -> Vec<String> {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    let forms = parse_forms(src).expect("parse");
    ev.eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect()
}

#[test]
fn pure_dispatch_typed_add_still_works() {
    let result = dispatch_builtin_pure("+", vec![Value::Int(2), Value::Int(3)])
        .expect("builtin + should resolve")
        .expect("builtin + should evaluate");
    assert_eq!(result, Value::Int(5));
}

#[test]
fn pure_dispatch_typed_percent_and_mod_follow_emacs_sign_rules() {
    let percent = dispatch_builtin_pure("%", vec![Value::Int(-5), Value::Int(2)])
        .expect("builtin % should resolve")
        .expect("builtin % should evaluate");
    let mod_name = dispatch_builtin_pure("mod", vec![Value::Int(-5), Value::Int(2)])
        .expect("builtin mod should resolve")
        .expect("builtin mod should evaluate");
    assert_eq!(percent, Value::Int(-1));
    assert_eq!(mod_name, Value::Int(1));
}

#[test]
fn pure_dispatch_typed_mod_zero_remainder_with_negative_divisor_stays_zero() {
    let int_mod = dispatch_builtin_pure("mod", vec![Value::Int(0), Value::Int(-3)])
        .expect("builtin mod should resolve")
        .expect("builtin mod should evaluate");
    assert_eq!(int_mod, Value::Int(0));

    let float_mod = dispatch_builtin_pure(
        "mod",
        vec![
            Value::Float(0.5, next_float_id()),
            Value::Float(-0.5, next_float_id()),
        ],
    )
    .expect("builtin mod should resolve")
    .expect("builtin mod should evaluate");
    match float_mod {
        Value::Float(f, _) => {
            assert_eq!(f, 0.0);
            assert!(!f.is_sign_negative(), "expected +0.0");
        }
        other => panic!("expected float, got {other:?}"),
    }

    let neg_zero_mod = dispatch_builtin_pure(
        "mod",
        vec![
            Value::Float(-0.5, next_float_id()),
            Value::Float(-0.5, next_float_id()),
        ],
    )
    .expect("builtin mod should resolve")
    .expect("builtin mod should evaluate");
    match neg_zero_mod {
        Value::Float(f, _) => {
            assert_eq!(f, 0.0);
            assert!(f.is_sign_negative(), "expected -0.0");
        }
        other => panic!("expected float, got {other:?}"),
    }
}

#[test]
fn pure_dispatch_typed_max_min_preserve_selected_operand_type() {
    let max_int = dispatch_builtin_pure(
        "max",
        vec![Value::Float(-2.5, next_float_id()), Value::Int(1)],
    )
    .expect("builtin max should resolve")
    .expect("builtin max should evaluate");
    assert_eq!(max_int, Value::Int(1));

    let min_int = dispatch_builtin_pure(
        "min",
        vec![Value::Int(1), Value::Float(1.0, next_float_id())],
    )
    .expect("builtin min should resolve")
    .expect("builtin min should evaluate");
    assert_eq!(min_int, Value::Int(1));

    let max_float = dispatch_builtin_pure(
        "max",
        vec![Value::Float(1.0, next_float_id()), Value::Int(1)],
    )
    .expect("builtin max should resolve")
    .expect("builtin max should evaluate");
    assert_eq!(max_float, Value::Float(1.0, next_float_id()));
}

#[test]
fn pure_dispatch_typed_numeric_primitives_accept_markers() {
    let marker = crate::emacs_core::marker::make_marker_value(Some("*scratch*"), Some(4), false);

    let max_with_marker = dispatch_builtin_pure("max", vec![Value::Int(1), marker])
        .expect("builtin max should resolve")
        .expect("builtin max should evaluate");
    assert_eq!(max_with_marker, Value::Int(4));

    let marker = crate::emacs_core::marker::make_marker_value(Some("*scratch*"), Some(4), false);
    let min_with_marker = dispatch_builtin_pure("min", vec![Value::Int(10), marker])
        .expect("builtin min should resolve")
        .expect("builtin min should evaluate");
    assert_eq!(min_with_marker, Value::Int(4));

    let left_marker =
        crate::emacs_core::marker::make_marker_value(Some("*scratch*"), Some(2), false);
    let right_marker =
        crate::emacs_core::marker::make_marker_value(Some("*scratch*"), Some(5), false);
    let lt_with_markers = dispatch_builtin_pure("<", vec![left_marker, right_marker])
        .expect("builtin < should resolve")
        .expect("builtin < should evaluate");
    assert_eq!(lt_with_markers, Value::True);

    let marker = crate::emacs_core::marker::make_marker_value(Some("*scratch*"), Some(4), false);
    let add1_with_marker = dispatch_builtin_pure("1+", vec![marker])
        .expect("builtin 1+ should resolve")
        .expect("builtin 1+ should evaluate");
    assert_eq!(add1_with_marker, Value::Int(5));

    let marker = crate::emacs_core::marker::make_marker_value(Some("*scratch*"), Some(4), false);
    let sub1_with_marker = dispatch_builtin_pure("1-", vec![marker])
        .expect("builtin 1- should resolve")
        .expect("builtin 1- should evaluate");
    assert_eq!(sub1_with_marker, Value::Int(3));
}

#[test]
fn pure_dispatch_typed_percent_rejects_float_args() {
    let err = dispatch_builtin_pure("%", vec![Value::Float(1.5, next_float_id()), Value::Int(2)])
        .expect("builtin % should resolve")
        .expect_err("builtin % should reject non-integer args");
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![
                    Value::symbol("integer-or-marker-p"),
                    Value::Float(1.5, next_float_id())
                ]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn pure_dispatch_typed_log_bitops_reject_with_integer_or_marker_p() {
    for name in ["logand", "logior", "logxor"] {
        let err = dispatch_builtin_pure(
            name,
            vec![Value::Int(1), Value::Float(2.0, next_float_id())],
        )
        .expect("builtin should resolve")
        .expect_err("bit operation should reject non-integer args");
        match err {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(
                    sig.data,
                    vec![
                        Value::symbol("integer-or-marker-p"),
                        Value::Float(2.0, next_float_id())
                    ]
                );
            }
            other => panic!("unexpected flow: {other:?}"),
        }
    }
}

#[test]
fn pure_dispatch_typed_numeric_symbol_rejections_use_number_or_marker_p() {
    let symbol_arg = Value::symbol("a");
    let cases = [
        ("+", vec![Value::Int(1), symbol_arg]),
        ("mod", vec![Value::Int(1), symbol_arg]),
        ("logand", vec![Value::Int(1), symbol_arg]),
        ("=", vec![Value::Int(1), symbol_arg]),
    ];

    for (name, args) in cases {
        let err = dispatch_builtin_pure(name, args)
            .expect("builtin should resolve")
            .expect_err("numeric builtin should reject non-numeric symbols");
        match err {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(
                    sig.data,
                    vec![Value::symbol("number-or-marker-p"), symbol_arg]
                );
            }
            other => panic!("unexpected flow: {other:?}"),
        }
    }
}

#[test]
fn pure_dispatch_typed_div_float_zero_uses_ieee_results() {
    let pos_inf = dispatch_builtin_pure(
        "/",
        vec![
            Value::Float(1.0, next_float_id()),
            Value::Float(0.0, next_float_id()),
        ],
    )
    .expect("builtin / should resolve")
    .expect("float division should evaluate");
    match pos_inf {
        Value::Float(f, _) => assert!(f.is_infinite() && f.is_sign_positive()),
        other => panic!("expected float, got {other:?}"),
    }

    let neg_inf = dispatch_builtin_pure(
        "/",
        vec![
            Value::Float(-1.0, next_float_id()),
            Value::Float(0.0, next_float_id()),
        ],
    )
    .expect("builtin / should resolve")
    .expect("float division should evaluate");
    match neg_inf {
        Value::Float(f, _) => assert!(f.is_infinite() && f.is_sign_negative()),
        other => panic!("expected float, got {other:?}"),
    }

    let neg_nan = dispatch_builtin_pure(
        "/",
        vec![
            Value::Float(0.0, next_float_id()),
            Value::Float(0.0, next_float_id()),
        ],
    )
    .expect("builtin / should resolve")
    .expect("float division should evaluate");
    match neg_nan {
        Value::Float(f, _) => assert!(f.is_nan() && f.is_sign_negative()),
        other => panic!("expected float, got {other:?}"),
    }
}

#[test]
fn pure_dispatch_typed_ash_handles_extreme_negative_shift_counts() {
    let right = dispatch_builtin_pure("ash", vec![Value::Int(3), Value::Int(i64::MIN)])
        .expect("builtin ash should resolve")
        .expect("builtin ash should evaluate");
    assert_eq!(right, Value::Int(0));

    let right_neg = dispatch_builtin_pure("ash", vec![Value::Int(-3), Value::Int(i64::MIN)])
        .expect("builtin ash should resolve")
        .expect("builtin ash should evaluate");
    assert_eq!(right_neg, Value::Int(-1));
}

#[test]
fn pure_dispatch_typed_abs_min_fixnum_signals_overflow_error() {
    let err = dispatch_builtin_pure("abs", vec![Value::Int(i64::MIN)])
        .expect("builtin abs should resolve")
        .expect_err("abs on i64::MIN should not panic");
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "overflow-error"),
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn pure_dispatch_typed_eq_returns_truthy_for_same_symbol() {
    let sym = Value::symbol("typed-dispatch-test");
    let result = dispatch_builtin_pure("eq", vec![sym, sym])
        .expect("builtin eq should resolve")
        .expect("builtin eq should evaluate");
    assert!(result.is_truthy());
}

#[test]
fn pure_dispatch_typed_append_concatenates_lists() {
    let left = Value::list(vec![Value::Int(1), Value::Int(2)]);
    let right = Value::list(vec![Value::Int(3), Value::Int(4)]);
    let result = dispatch_builtin_pure("append", vec![left, right])
        .expect("builtin append should resolve")
        .expect("builtin append should evaluate");
    assert_eq!(
        result,
        Value::list(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4)
        ])
    );
}

#[test]
fn pure_dispatch_typed_append_flattens_bytecode_slots() {
    let bc = Value::make_bytecode(crate::emacs_core::bytecode::ByteCodeFunction::new(
        LambdaParams::simple(vec![intern("x")]),
    ));
    let result = dispatch_builtin_pure("append", vec![bc, Value::Nil])
        .expect("builtin append should resolve")
        .expect("builtin append should evaluate");
    let slots = list_to_vec(&result).expect("bytecode append should produce a proper list");
    assert_eq!(slots.len(), 4);
    assert!(matches!(slots[0], Value::Cons(_) | Value::Nil));
    assert!(matches!(slots[1], Value::Nil | Value::Str(_)));
    assert!(matches!(slots[2], Value::Vector(_)));
    assert!(matches!(slots[3], Value::Int(_)));
}

#[test]
fn pure_dispatch_typed_length_predicates_accept_bytecode_functions() {
    let bc = Value::make_bytecode(crate::emacs_core::bytecode::ByteCodeFunction::new(
        LambdaParams::simple(vec![intern("x")]),
    ));
    let eq = dispatch_builtin_pure("length=", vec![bc, Value::Int(4)])
        .expect("builtin length= should resolve")
        .expect("builtin length= should evaluate");
    assert!(eq.is_truthy());
    let gt = dispatch_builtin_pure("length>", vec![bc, Value::Int(3)])
        .expect("builtin length> should resolve")
        .expect("builtin length> should evaluate");
    assert!(gt.is_truthy());
}

#[test]
fn pure_dispatch_typed_length_tracks_bytecode_doc_slot() {
    let mut bc =
        crate::emacs_core::bytecode::ByteCodeFunction::new(LambdaParams::simple(vec![intern("x")]));
    bc.docstring = Some("doc".into());
    let bc = Value::make_bytecode(bc);

    let len = dispatch_builtin_pure("length", vec![bc])
        .expect("builtin length should resolve")
        .expect("builtin length should evaluate");

    assert_eq!(len, Value::Int(5));
}

#[test]
fn pure_dispatch_typed_vconcat_flattens_bytecode_slots() {
    let bc = Value::make_bytecode(crate::emacs_core::bytecode::ByteCodeFunction::new(
        LambdaParams::simple(vec![intern("x")]),
    ));
    let result = dispatch_builtin_pure("vconcat", vec![bc])
        .expect("builtin vconcat should resolve")
        .expect("builtin vconcat should evaluate");
    let Value::Vector(id) = result else {
        panic!("expected vector result, got {result:?}");
    };
    let slots = with_heap(|h| h.get_vector(id).clone());
    assert_eq!(slots.len(), 4);
    assert!(matches!(slots[0], Value::Cons(_) | Value::Nil));
    assert!(matches!(slots[1], Value::Nil | Value::Str(_)));
    assert!(matches!(slots[2], Value::Vector(_)));
    assert!(matches!(slots[3], Value::Int(_)));
}

#[test]
fn pure_dispatch_typed_length_tracks_interpreted_closure_slot_count() {
    let bare = Value::make_lambda(LambdaData {
        params: LambdaParams::simple(vec![intern("x")]),
        body: vec![Expr::Symbol(intern("x"))].into(),
        env: Some(Value::Nil),
        docstring: None,
        doc_form: None,
    });
    let with_doc = Value::make_lambda(LambdaData {
        params: LambdaParams::simple(vec![intern("x")]),
        body: vec![Expr::Symbol(intern("x"))].into(),
        env: Some(Value::Nil),
        docstring: Some("doc".into()),
        doc_form: None,
    });

    let bare_len = dispatch_builtin_pure("length", vec![bare])
        .expect("builtin length should resolve")
        .expect("builtin length should evaluate");
    let doc_len = dispatch_builtin_pure("length", vec![with_doc])
        .expect("builtin length should resolve")
        .expect("builtin length should evaluate");

    assert_eq!(bare_len, Value::Int(3));
    assert_eq!(doc_len, Value::Int(5));
}

#[test]
fn compiled_literal_reifier_turns_interpreted_closure_vectors_callable() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let closure_vec = Value::vector(vec![
        Value::list(vec![Value::symbol("x")]),
        Value::list(vec![Value::list(vec![
            Value::symbol("+"),
            Value::symbol("x"),
            Value::Int(1),
        ])]),
        Value::Nil,
    ]);

    let converted = super::symbols::try_convert_nested_compiled_literal(closure_vec);
    assert!(matches!(converted, Value::Lambda(_)));

    let out = eval
        .apply(converted, vec![Value::Int(41)])
        .expect("converted closure should be callable");
    assert_eq!(out, Value::Int(42));
}

#[test]
fn pure_dispatch_typed_string_equal_aliases_match() {
    let a = Value::string("neo");
    let b = Value::string("neo");
    let full = dispatch_builtin_pure("string-equal", vec![a, b])
        .expect("builtin string-equal should resolve")
        .expect("builtin string-equal should evaluate");
    let short = dispatch_builtin_pure("string=", vec![a, b])
        .expect("builtin string= should resolve")
        .expect("builtin string= should evaluate");
    assert_eq!(full, short);
    assert!(full.is_truthy());
}

#[test]
fn pure_dispatch_typed_string_comparisons_accept_symbol_designators() {
    let less = dispatch_builtin_pure("string<", vec![Value::symbol("foo"), Value::string("g")])
        .expect("builtin string< should resolve")
        .expect("builtin string< should evaluate");
    assert!(less.is_truthy());

    let equal = dispatch_builtin_pure("string-equal", vec![Value::True, Value::string("t")])
        .expect("builtin string-equal should resolve")
        .expect("builtin string-equal should evaluate");
    assert!(equal.is_truthy());

    let greater = dispatch_builtin_pure("string>", vec![Value::Nil, Value::string("a")])
        .expect("builtin string> should resolve")
        .expect("builtin string> should evaluate");
    assert!(greater.is_truthy());

    let err = dispatch_builtin_pure("string>", vec![Value::Int(7), Value::string("a")])
        .expect("builtin string> should resolve")
        .expect_err("string> should reject non string/symbol designators");
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::Int(7)],);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn pure_dispatch_typed_downcase_unicode_edge_payloads_match_oracle() {
    let cases = [
        (304, 304),
        (7305, 7305),
        (8490, 8490),
        (42955, 42955),
        (42956, 42956),
        (42958, 42958),
        (42962, 42962),
        (42964, 42964),
        (42970, 42970),
        (42972, 42972),
        (68944, 68944),
        (68965, 68965),
        (93856, 93856),
        (93880, 93880),
        (66560, 66600),
    ];

    for (input, expected) in cases {
        let result = dispatch_builtin_pure("downcase", vec![Value::Int(input)])
            .expect("builtin downcase should resolve")
            .expect("builtin downcase should evaluate");
        assert_eq!(
            result,
            Value::Int(expected),
            "downcase({input}) should equal {expected}"
        );
    }

    let dotted_i = dispatch_builtin_pure("downcase", vec![Value::Char('\u{0130}')])
        .expect("builtin downcase should resolve")
        .expect("builtin downcase should evaluate");
    assert_eq!(dotted_i, Value::Char('\u{0130}'));

    let kelvin = dispatch_builtin_pure("downcase", vec![Value::string("\u{212A}")])
        .expect("builtin downcase should resolve")
        .expect("builtin downcase should evaluate");
    assert_eq!(kelvin, Value::string("\u{212A}"));

    let dotted_i_string = dispatch_builtin_pure("downcase", vec![Value::string("\u{0130}")])
        .expect("builtin downcase should resolve")
        .expect("builtin downcase should evaluate");
    assert_eq!(dotted_i_string, Value::string("i\u{307}"));

    let preserve_latin = dispatch_builtin_pure("downcase", vec![Value::string("\u{A7CB}")])
        .expect("builtin downcase should resolve")
        .expect("builtin downcase should evaluate");
    assert_eq!(preserve_latin, Value::string("\u{A7CB}"));

    let preserve_cyrillic_sup = dispatch_builtin_pure("downcase", vec![Value::string("\u{10D50}")])
        .expect("builtin downcase should resolve")
        .expect("builtin downcase should evaluate");
    assert_eq!(preserve_cyrillic_sup, Value::string("\u{10D50}"));

    let preserve_adlam = dispatch_builtin_pure("downcase", vec![Value::string("\u{16EA0}")])
        .expect("builtin downcase should resolve")
        .expect("builtin downcase should evaluate");
    assert_eq!(preserve_adlam, Value::string("\u{16EA0}"));

    let negative = dispatch_builtin_pure("downcase", vec![Value::Int(-1)])
        .expect("builtin downcase should resolve")
        .expect_err("builtin downcase should reject negative integer designators");
    match negative {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("char-or-string-p"), Value::Int(-1)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn pure_dispatch_typed_upcase_unicode_edge_payloads_match_oracle() {
    let cases = [
        (223, 7838),
        (305, 305),
        (7306, 7306),
        (8064, 8072),
        (8071, 8079),
        (8080, 8088),
        (8087, 8095),
        (8096, 8104),
        (8103, 8111),
        (8115, 8124),
        (8131, 8140),
        (8179, 8188),
        (42957, 42957),
        (68976, 68976),
        (68997, 68997),
        (93883, 93883),
        (93907, 93907),
        (97, 65),
    ];

    for (input, expected) in cases {
        let result = dispatch_builtin_pure("upcase", vec![Value::Int(input)])
            .expect("builtin upcase should resolve")
            .expect("builtin upcase should evaluate");
        assert_eq!(
            result,
            Value::Int(expected),
            "upcase({input}) should equal {expected}"
        );
    }

    let sharp_s = dispatch_builtin_pure("upcase", vec![Value::Char('ß')])
        .expect("builtin upcase should resolve")
        .expect("builtin upcase should evaluate");
    assert_eq!(sharp_s, Value::Char('\u{1E9E}'));

    let sharp_s_string = dispatch_builtin_pure("upcase", vec![Value::string("ß")])
        .expect("builtin upcase should resolve")
        .expect("builtin upcase should evaluate");
    assert_eq!(sharp_s_string, Value::string("SS"));

    let dotless_i_string = dispatch_builtin_pure("upcase", vec![Value::string("\u{0131}")])
        .expect("builtin upcase should resolve")
        .expect("builtin upcase should evaluate");
    assert_eq!(dotless_i_string, Value::string("\u{0131}"));

    let preserve_latin = dispatch_builtin_pure("upcase", vec![Value::string("\u{019B}")])
        .expect("builtin upcase should resolve")
        .expect("builtin upcase should evaluate");
    assert_eq!(preserve_latin, Value::string("\u{019B}"));

    let preserve_cyrillic_sup = dispatch_builtin_pure("upcase", vec![Value::string("\u{10D70}")])
        .expect("builtin upcase should resolve")
        .expect("builtin upcase should evaluate");
    assert_eq!(preserve_cyrillic_sup, Value::string("\u{10D70}"));

    let preserve_adlam = dispatch_builtin_pure("upcase", vec![Value::string("\u{16EBB}")])
        .expect("builtin upcase should resolve")
        .expect("builtin upcase should evaluate");
    assert_eq!(preserve_adlam, Value::string("\u{16EBB}"));

    let negative = dispatch_builtin_pure("upcase", vec![Value::Int(-1)])
        .expect("builtin upcase should resolve")
        .expect_err("builtin upcase should reject negative integer designators");
    match negative {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("char-or-string-p"), Value::Int(-1)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn keymapp_accepts_lisp_keymap_cons_cells() {
    let mut eval = super::super::eval::Evaluator::new();

    let proper = Value::list(vec![Value::symbol("keymap")]);
    assert_eq!(
        builtin_keymapp(&mut eval, vec![proper]).unwrap(),
        Value::True
    );

    let proper_with_entry = Value::cons(
        Value::symbol("keymap"),
        Value::cons(
            Value::cons(Value::Int(97), Value::symbol("ignore")),
            Value::Nil,
        ),
    );
    assert_eq!(
        builtin_keymapp(&mut eval, vec![proper_with_entry]).unwrap(),
        Value::True
    );

    let improper = Value::cons(Value::symbol("keymap"), Value::symbol("tail"));
    assert_eq!(
        builtin_keymapp(&mut eval, vec![improper]).unwrap(),
        Value::True
    );

    let non_keymap = Value::list(vec![Value::symbol("foo"), Value::symbol("keymap")]);
    assert_eq!(
        builtin_keymapp(&mut eval, vec![non_keymap]).unwrap(),
        Value::Nil
    );
}

#[test]
fn keymapp_rejects_non_keymap_integer_designators() {
    let mut eval = super::super::eval::Evaluator::new();
    let keymap = builtin_make_sparse_keymap(&mut eval, vec![]).unwrap();
    assert_eq!(
        builtin_keymapp(&mut eval, vec![keymap]).unwrap(),
        Value::True
    );
    assert_eq!(
        builtin_keymapp(&mut eval, vec![Value::Int(16)]).unwrap(),
        Value::Nil
    );
    assert_eq!(
        builtin_keymapp(&mut eval, vec![Value::Int(999_999)]).unwrap(),
        Value::Nil
    );
}

#[test]
fn accessible_keymaps_reports_root_and_prefix_paths() {
    let mut eval = super::super::eval::Evaluator::new();
    let root = builtin_make_sparse_keymap(&mut eval, vec![]).unwrap();
    let child = builtin_make_sparse_keymap(&mut eval, vec![]).unwrap();
    builtin_define_key(
        &mut eval,
        vec![root, Value::string("\x18"), child], // \C-x = 0x18
    )
    .unwrap();

    let all = builtin_accessible_keymaps(&mut eval, vec![root]).unwrap();
    let all_items = list_to_vec(&all).expect("accessible-keymaps should return list");
    assert_eq!(all_items.len(), 2);

    let first = match &all_items[0] {
        Value::Cons(cell) => read_cons(*cell),
        other => panic!("expected cons cell, got {other:?}"),
    };
    assert_eq!(first.car, Value::vector(vec![]));
    assert_eq!(
        builtin_keymapp(&mut eval, vec![first.cdr]).unwrap(),
        Value::True
    );

    let filtered =
        builtin_accessible_keymaps(&mut eval, vec![root, Value::vector(vec![Value::Int(24)])])
            .unwrap();
    let filtered_items = list_to_vec(&filtered).expect("filtered accessible-keymaps list");
    assert_eq!(filtered_items.len(), 1);
    let only = match &filtered_items[0] {
        Value::Cons(cell) => read_cons(*cell),
        other => panic!("expected cons cell, got {other:?}"),
    };
    assert_eq!(only.car, Value::vector(vec![Value::Int(24)]));

    let no_match =
        builtin_accessible_keymaps(&mut eval, vec![root, Value::vector(vec![Value::Int(97)])])
            .unwrap();
    assert!(no_match.is_nil());
}

#[test]
fn accessible_keymaps_prefix_type_errors_match_oracle_shape() {
    let mut eval = super::super::eval::Evaluator::new();
    let map = builtin_make_sparse_keymap(&mut eval, vec![]).unwrap();

    let sequence_err = builtin_accessible_keymaps(&mut eval, vec![map, Value::True]).unwrap_err();
    match sequence_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("sequencep"), Value::True]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let array_err =
        builtin_accessible_keymaps(&mut eval, vec![map, Value::list(vec![Value::symbol("a")])])
            .unwrap_err();
    match array_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![
                    Value::symbol("arrayp"),
                    Value::list(vec![Value::symbol("a")])
                ]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn key_description_renders_super_prefixed_symbol_events_with_expected_angles() {
    let super_only = builtin_key_description(vec![Value::vector(vec![Value::symbol("s-f1")])])
        .expect("key-description should succeed");
    assert_eq!(super_only, Value::string("s-<f1>"));

    let ctrl_super = builtin_key_description(vec![Value::vector(vec![Value::symbol("C-s-f1")])])
        .expect("key-description should succeed");
    assert_eq!(ctrl_super, Value::string("C-s-<f1>"));

    let single = builtin_single_key_description(vec![Value::symbol("s-f1")])
        .expect("single-key-description should succeed");
    assert_eq!(single, Value::string("s-<f1>"));
}

#[test]
fn key_description_symbol_modifier_edges_match_emacs() {
    assert_eq!(
        builtin_single_key_description(vec![Value::symbol("M-a")])
            .expect("single-key-description should succeed"),
        Value::string("<M-a>")
    );
    assert_eq!(
        builtin_single_key_description(vec![Value::symbol("C-a")])
            .expect("single-key-description should succeed"),
        Value::string("<C-a>")
    );
    assert_eq!(
        builtin_single_key_description(vec![Value::symbol("C-M-a")])
            .expect("single-key-description should succeed"),
        Value::string("C-<M-a>")
    );
    assert_eq!(
        builtin_single_key_description(vec![Value::symbol("M-a"), Value::True])
            .expect("single-key-description should succeed"),
        Value::string("M-a")
    );
    assert_eq!(
        builtin_key_description(vec![Value::vector(vec![Value::symbol("M-a")])])
            .expect("key-description should succeed"),
        Value::string("<M-a>")
    );
    assert_eq!(
        builtin_key_description(vec![Value::vector(vec![Value::symbol("C-s-f1")])])
            .expect("key-description should succeed"),
        Value::string("C-s-<f1>")
    );
}

#[test]
fn key_description_integer_modifier_and_nonunicode_edges_match_emacs() {
    assert_eq!(
        builtin_single_key_description(vec![Value::Int(0x40_0000)])
            .expect("single-key-description should succeed"),
        Value::string("A-C-@")
    );
    assert_eq!(
        builtin_single_key_description(vec![Value::Int(58_720_257)])
            .expect("single-key-description should succeed"),
        Value::string("C-H-S-s-a")
    );
    assert_eq!(
        builtin_single_key_description(vec![Value::Int(264_241_249)])
            .expect("single-key-description should succeed"),
        Value::string("A-C-H-M-S-s-a")
    );
    assert_eq!(
        builtin_single_key_description(vec![Value::Int(134_217_737)])
            .expect("single-key-description should succeed"),
        Value::string("C-M-i")
    );
    assert_eq!(
        builtin_single_key_description(vec![Value::Int(138_412_041)])
            .expect("single-key-description should succeed"),
        Value::string("A-C-M-i")
    );
    assert_eq!(
        builtin_single_key_description(vec![Value::Int(201_326_601)])
            .expect("single-key-description should succeed"),
        Value::string("C-M-i")
    );

    let single_nonunicode = builtin_single_key_description(vec![Value::Int(0x11_0000)])
        .expect("single-key-description should support nonunicode char code");
    assert_eq!(
        decode_storage_char_codes(
            single_nonunicode
                .as_str()
                .expect("single-key-description should return string")
        ),
        vec![0x11_0000]
    );

    let key_nonunicode = builtin_key_description(vec![Value::vector(vec![Value::Int(0x20_0000)])])
        .expect("key-description should support nonunicode char code");
    assert_eq!(
        decode_storage_char_codes(
            key_nonunicode
                .as_str()
                .expect("key-description should return string")
        ),
        vec![0x20_0000]
    );

    assert_eq!(
        builtin_key_description(vec![Value::vector(vec![Value::Int(0x40_0000)])])
            .expect("key-description should succeed"),
        Value::string("A-C-@")
    );
    assert_eq!(
        builtin_key_description(vec![Value::vector(vec![Value::Int(134_217_737)])])
            .expect("key-description should succeed"),
        Value::string("C-M-i")
    );
    assert_eq!(
        builtin_key_description(vec![Value::vector(vec![Value::Int(201_326_601)])])
            .expect("key-description should succeed"),
        Value::string("C-M-i")
    );
}

#[test]
fn eval_get_file_buffer_matches_visited_paths() {
    let mut eval = super::super::eval::Evaluator::new();
    let id = eval.buffers.create_buffer("gfb");

    let path = std::env::temp_dir().join(format!("neovm-gfb-{}-{}", std::process::id(), "eval"));
    std::fs::write(&path, b"gfb").expect("write test file");
    let file = path.to_string_lossy().to_string();
    eval.buffers.get_mut(id).unwrap().file_name = Some(file.clone());

    let exact = builtin_get_file_buffer(&mut eval, vec![Value::string(&file)]).unwrap();
    assert_eq!(exact, Value::Buffer(id));

    let truename = std::fs::canonicalize(&path)
        .expect("canonicalize file")
        .to_string_lossy()
        .to_string();
    let true_match = builtin_get_file_buffer(&mut eval, vec![Value::string(truename)]).unwrap();
    assert_eq!(true_match, Value::Buffer(id));

    let default_dir = format!("{}/", path.parent().unwrap().to_string_lossy());
    let basename = path.file_name().unwrap().to_string_lossy().to_string();
    eval.obarray
        .set_symbol_value("default-directory", Value::string(default_dir));
    let relative = builtin_get_file_buffer(&mut eval, vec![Value::string(basename)]).unwrap();
    assert_eq!(relative, Value::Buffer(id));

    let _ = std::fs::remove_file(path);
}

#[test]
fn eval_get_file_buffer_type_and_missing_paths() {
    let mut eval = super::super::eval::Evaluator::new();
    let missing = builtin_get_file_buffer(
        &mut eval,
        vec![Value::string("/tmp/neovm-no-such-file-for-gfb")],
    )
    .unwrap();
    assert!(missing.is_nil());
    assert!(builtin_get_file_buffer(&mut eval, vec![Value::Int(1)]).is_err());
}

#[test]
fn eval_builtin_rejects_too_many_args() {
    let mut eval = super::super::eval::Evaluator::new();
    let err = builtin_eval(
        &mut eval,
        vec![Value::Int(1), Value::Nil, Value::symbol("ignored")],
    )
    .expect_err("eval should reject more than two arguments");
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(sig.data, vec![Value::symbol("eval"), Value::Int(3)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn eval_buffer_live_p_tracks_killed_buffers() {
    let mut eval = super::super::eval::Evaluator::new();
    let buf = builtin_get_buffer_create(&mut eval, vec![Value::string("*blp*")]).unwrap();
    let live = builtin_buffer_live_p(&mut eval, vec![buf]).unwrap();
    assert_eq!(live, Value::True);

    let _ = builtin_kill_buffer(&mut eval, vec![buf]).unwrap();
    let dead = builtin_buffer_live_p(&mut eval, vec![buf]).unwrap();
    assert_eq!(dead, Value::Nil);
}

#[test]
fn kill_buffer_optional_arg_and_error_semantics() {
    let mut eval = super::super::eval::Evaluator::new();
    let a = builtin_get_buffer_create(&mut eval, vec![Value::string("*kb-opt-a*")]).unwrap();
    let b = builtin_get_buffer_create(&mut eval, vec![Value::string("*kb-opt-b*")]).unwrap();
    let _ = builtin_set_buffer(&mut eval, vec![a]).unwrap();

    // Optional argument omitted kills current buffer and selects another.
    let killed_current = builtin_kill_buffer(&mut eval, vec![]).unwrap();
    assert_eq!(killed_current, Value::True);
    assert_eq!(
        builtin_buffer_live_p(&mut eval, vec![a]).unwrap(),
        Value::Nil
    );
    assert!(matches!(
        builtin_current_buffer(&mut eval, vec![]).unwrap(),
        Value::Buffer(_)
    ));

    // Missing buffer name signals `(error "No buffer named ...")`.
    let missing = builtin_kill_buffer(&mut eval, vec![Value::string("*kb-opt-missing*")])
        .expect_err("kill-buffer should signal on missing name");
    match missing {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("No buffer named *kb-opt-missing*")]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    // Dead buffer object returns nil.
    let dead = create_unique_test_buffer(&mut eval, "*kb-opt-dead*");
    assert_eq!(
        builtin_kill_buffer(&mut eval, vec![dead]).unwrap(),
        Value::True
    );
    assert_eq!(
        builtin_kill_buffer(&mut eval, vec![dead]).unwrap(),
        Value::Nil
    );

    // Non-buffer/non-string designators signal `wrong-type-argument`.
    let type_err = builtin_kill_buffer(&mut eval, vec![Value::Int(1)])
        .expect_err("kill-buffer should reject non-string designator");
    match type_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::Int(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let _ = builtin_kill_buffer(&mut eval, vec![b]).unwrap();
}

#[test]
fn set_buffer_rejects_deleted_buffer_object() {
    let mut eval = super::super::eval::Evaluator::new();
    let dead = create_unique_test_buffer(&mut eval, "*sb-dead*");
    let _ = builtin_kill_buffer(&mut eval, vec![dead]).unwrap();

    let err = builtin_set_buffer(&mut eval, vec![dead])
        .expect_err("set-buffer should reject deleted buffer objects");
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data, vec![Value::string("Selecting deleted buffer")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn eval_buffer_live_p_non_buffer_objects_return_nil() {
    let mut eval = super::super::eval::Evaluator::new();
    let by_name = builtin_buffer_live_p(&mut eval, vec![Value::string("*scratch*")]).unwrap();
    assert_eq!(by_name, Value::Nil);
    let nil_arg = builtin_buffer_live_p(&mut eval, vec![Value::Nil]).unwrap();
    assert_eq!(nil_arg, Value::Nil);
}

#[test]
fn get_buffer_create_accepts_optional_second_arg() {
    let mut eval = super::super::eval::Evaluator::new();
    let first =
        builtin_get_buffer_create(&mut eval, vec![Value::string("*gbc-opt*"), Value::Int(7)])
            .unwrap();
    let second =
        builtin_get_buffer_create(&mut eval, vec![Value::string("*gbc-opt*"), Value::Nil]).unwrap();
    assert_eq!(first, second);

    let err = builtin_get_buffer_create(
        &mut eval,
        vec![Value::string("*gbc-opt*"), Value::Nil, Value::Nil],
    )
    .expect_err("get-buffer-create should reject more than two args");
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("get-buffer-create"), Value::Int(3)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn buffer_creation_helpers_reject_missing_required_name_arg() {
    let mut eval = super::super::eval::Evaluator::new();

    let err = builtin_get_buffer_create(&mut eval, vec![])
        .expect_err("get-buffer-create should reject missing required arg");
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("get-buffer-create"), Value::Int(0)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let err = builtin_generate_new_buffer_name(&mut eval, vec![])
        .expect_err("generate-new-buffer-name should reject missing required arg");
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("generate-new-buffer-name"), Value::Int(0)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn get_buffer_rejects_non_string_non_buffer_designators() {
    let mut eval = super::super::eval::Evaluator::new();
    for bad in [Value::Int(1), Value::Nil, Value::symbol("foo")] {
        let err = builtin_get_buffer(&mut eval, vec![bad])
            .expect_err("get-buffer should reject non-string/non-buffer args");
        match err {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(sig.data, vec![Value::symbol("stringp"), bad]);
            }
            other => panic!("unexpected flow: {other:?}"),
        }
    }

    let dead = create_unique_test_buffer(&mut eval, "*gb-dead*");
    let _ = builtin_kill_buffer(&mut eval, vec![dead]).unwrap();
    assert_eq!(builtin_get_buffer(&mut eval, vec![dead]).unwrap(), dead);
}

#[test]
fn generate_new_buffer_name_optional_arg_matches_expected_types() {
    let mut eval = super::super::eval::Evaluator::new();
    let _ = builtin_get_buffer_create(&mut eval, vec![Value::string("*gnbn-opt*")]).unwrap();

    let with_nil =
        builtin_generate_new_buffer_name(&mut eval, vec![Value::string("*gnbn-opt*"), Value::Nil])
            .unwrap();
    let with_true =
        builtin_generate_new_buffer_name(&mut eval, vec![Value::string("*gnbn-opt*"), Value::True])
            .unwrap();
    let with_symbol = builtin_generate_new_buffer_name(
        &mut eval,
        vec![Value::string("*gnbn-opt*"), Value::symbol("ignored")],
    )
    .unwrap();
    let with_keyword = builtin_generate_new_buffer_name(
        &mut eval,
        vec![
            Value::string("*gnbn-opt*"),
            Value::Keyword(intern("ignored")),
        ],
    )
    .unwrap();
    let with_string = builtin_generate_new_buffer_name(
        &mut eval,
        vec![Value::string("*gnbn-opt*"), Value::string("*gnbn-opt*<9>")],
    )
    .unwrap();

    assert_eq!(with_nil, Value::string("*gnbn-opt*<2>"));
    assert_eq!(with_true, Value::string("*gnbn-opt*<2>"));
    assert_eq!(with_symbol, Value::string("*gnbn-opt*<2>"));
    assert_eq!(with_keyword, Value::string("*gnbn-opt*<2>"));
    assert_eq!(with_string, Value::string("*gnbn-opt*<2>"));

    let err = builtin_generate_new_buffer_name(
        &mut eval,
        vec![
            Value::string("*gnbn-opt*"),
            Value::list(vec![Value::Int(1)]),
        ],
    )
    .expect_err("generate-new-buffer-name should reject non string/symbol optional arg");
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("stringp"), Value::list(vec![Value::Int(1)])]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn buffer_size_and_modified_p_return_defaults_for_deleted_buffer_objects() {
    let mut eval = super::super::eval::Evaluator::new();

    let dead_for_size = create_unique_test_buffer(&mut eval, "*bs-dead*");
    let _ = builtin_kill_buffer(&mut eval, vec![dead_for_size]).unwrap();
    let size = builtin_buffer_size(&mut eval, vec![dead_for_size]).unwrap();
    assert_eq!(size, Value::Int(0));

    let dead_for_modified = create_unique_test_buffer(&mut eval, "*bm-dead*");
    let _ = builtin_kill_buffer(&mut eval, vec![dead_for_modified]).unwrap();
    let modified = builtin_buffer_modified_p(&mut eval, vec![dead_for_modified]).unwrap();
    assert_eq!(modified, Value::Nil);
}

#[test]
fn buffer_base_buffer_and_last_name_semantics() {
    let mut eval = super::super::eval::Evaluator::new();
    let base_id = eval.buffers.current_buffer().unwrap().id;
    let indirect_id = eval.buffers.create_buffer("*indirect*");
    eval.buffers.get_mut(indirect_id).unwrap().base_buffer = Some(base_id);

    assert_eq!(
        builtin_buffer_base_buffer(&mut eval, vec![]).unwrap(),
        Value::Nil
    );
    assert_eq!(
        builtin_buffer_last_name(&mut eval, vec![]).unwrap(),
        Value::Nil
    );
    assert_eq!(
        builtin_buffer_base_buffer(&mut eval, vec![Value::Nil]).unwrap(),
        Value::Nil
    );
    assert_eq!(
        builtin_buffer_base_buffer(&mut eval, vec![Value::Buffer(indirect_id)]).unwrap(),
        Value::Buffer(base_id)
    );
    assert_eq!(
        builtin_buffer_last_name(&mut eval, vec![Value::Nil]).unwrap(),
        Value::Nil
    );

    let base_type = builtin_buffer_base_buffer(&mut eval, vec![Value::symbol("x")])
        .expect_err("buffer-base-buffer should reject non-buffer, non-nil optional arg");
    match base_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("bufferp"), Value::symbol("x")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let last_type = builtin_buffer_last_name(&mut eval, vec![Value::symbol("x")])
        .expect_err("buffer-last-name should reject non-buffer, non-nil optional arg");
    match last_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("bufferp"), Value::symbol("x")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let base_arity = builtin_buffer_base_buffer(&mut eval, vec![Value::Nil, Value::Nil])
        .expect_err("buffer-base-buffer should reject >1 args");
    match base_arity {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("buffer-base-buffer"), Value::Int(2)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let last_arity = builtin_buffer_last_name(&mut eval, vec![Value::Nil, Value::Nil])
        .expect_err("buffer-last-name should reject >1 args");
    match last_arity {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("buffer-last-name"), Value::Int(2)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let dead = create_unique_test_buffer(&mut eval, "*bln-dead*");
    let live_name = builtin_buffer_name(&mut eval, vec![dead]).unwrap();
    let _ = builtin_kill_buffer(&mut eval, vec![dead]).unwrap();

    assert_eq!(
        builtin_buffer_base_buffer(&mut eval, vec![dead]).unwrap(),
        Value::Nil
    );
    assert_eq!(
        builtin_buffer_last_name(&mut eval, vec![dead]).unwrap(),
        live_name
    );
}

#[test]
fn make_indirect_buffer_shares_text_and_flattens_base_buffer_chain() {
    let mut eval = super::super::eval::Evaluator::new();
    let base = create_unique_test_buffer(&mut eval, "*mib-base*");
    let Value::Buffer(base_id) = base else {
        panic!("expected buffer object");
    };

    let _ = builtin_set_buffer(&mut eval, vec![base]).unwrap();
    builtin_insert(&mut eval, vec![Value::string("abcd")]).unwrap();

    let indirect =
        builtin_make_indirect_buffer(&mut eval, vec![base, Value::string("*mib-indirect*")])
            .expect("make-indirect-buffer should create a buffer");
    let Value::Buffer(indirect_id) = indirect else {
        panic!("expected buffer object");
    };

    assert_eq!(
        builtin_buffer_base_buffer(&mut eval, vec![indirect]).unwrap(),
        base
    );
    assert!(
        eval.buffers
            .get(base_id)
            .unwrap()
            .text
            .shares_storage_with(&eval.buffers.get(indirect_id).unwrap().text)
    );

    let _ = eval.buffers.goto_buffer_byte(base_id, 0);
    let _ = eval.buffers.insert_into_buffer(base_id, "zz");
    assert_eq!(
        eval.buffers.get(indirect_id).unwrap().buffer_string(),
        "zzabcd"
    );

    let second =
        builtin_make_indirect_buffer(&mut eval, vec![indirect, Value::string("*mib-indirect-2*")])
            .expect("second indirect buffer should be created");
    assert_eq!(
        builtin_buffer_base_buffer(&mut eval, vec![second]).unwrap(),
        base
    );
}

#[test]
fn make_indirect_buffer_rejects_duplicate_and_empty_names() {
    let mut eval = super::super::eval::Evaluator::new();
    let base = create_unique_test_buffer(&mut eval, "*mib-errors-base*");

    let duplicate = builtin_make_indirect_buffer(&mut eval, vec![base, Value::string("*scratch*")])
        .expect_err("duplicate indirect name should error");
    match duplicate {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "error"),
        other => panic!("unexpected flow: {other:?}"),
    }

    let empty = builtin_make_indirect_buffer(&mut eval, vec![base, Value::string("")])
        .expect_err("empty indirect name should error");
    match empty {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "error"),
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn make_indirect_buffer_clone_and_hook_semantics_follow_buffer_c() {
    let mut eval = super::super::eval::Evaluator::new();
    let base = create_unique_test_buffer(&mut eval, "*mib-clone-base*");
    let Value::Buffer(base_id) = base else {
        panic!("expected buffer object");
    };

    let _ = builtin_set_buffer(&mut eval, vec![base]).unwrap();
    let _ =
        eval.buffers
            .set_buffer_local_property(base_id, "major-mode", Value::symbol("neo-mode"));
    let _ = eval
        .buffers
        .set_buffer_local_property(base_id, "mode-name", Value::string("Neo"));

    install_noarg_hook_probe(
        &mut eval,
        "mib-clone-hook",
        vec![Expr::List(vec![
            Expr::Symbol(intern("setq")),
            Expr::Symbol(intern("mib-last-clone-buffer")),
            Expr::List(vec![Expr::Symbol(intern("buffer-name"))]),
        ])],
    );
    install_noarg_hook_probe(
        &mut eval,
        "mib-buffer-list-hook",
        vec![Expr::List(vec![
            Expr::Symbol(intern("setq")),
            Expr::Symbol(intern("mib-buffer-list-ran")),
            Expr::Symbol(intern("t")),
        ])],
    );
    eval.obarray_mut().set_symbol_value(
        "clone-indirect-buffer-hook",
        Value::list(vec![Value::symbol("mib-clone-hook")]),
    );
    eval.obarray_mut().set_symbol_value(
        "buffer-list-update-hook",
        Value::list(vec![Value::symbol("mib-buffer-list-hook")]),
    );
    eval.obarray_mut()
        .set_symbol_value("mib-last-clone-buffer", Value::Nil);
    eval.obarray_mut()
        .set_symbol_value("mib-buffer-list-ran", Value::Nil);

    let cloned = builtin_make_indirect_buffer(
        &mut eval,
        vec![base, Value::string("*mib-clone*"), Value::True],
    )
    .expect("clone indirect buffer");
    let Value::Buffer(cloned_id) = cloned else {
        panic!("expected buffer object");
    };

    assert_eq!(
        eval.buffers
            .get(cloned_id)
            .and_then(|buf| buf.get_buffer_local("major-mode"))
            .copied(),
        Some(Value::symbol("neo-mode"))
    );
    assert_eq!(
        eval.buffers.current_buffer_id(),
        Some(base_id),
        "make-indirect-buffer should restore the previous current buffer"
    );
    assert_eq!(
        eval.obarray()
            .symbol_value("mib-last-clone-buffer")
            .and_then(Value::as_str),
        Some("*mib-clone*")
    );
    assert_eq!(
        eval.obarray().symbol_value("mib-buffer-list-ran"),
        Some(&Value::True)
    );

    eval.obarray_mut()
        .set_symbol_value("mib-last-clone-buffer", Value::Nil);
    eval.obarray_mut()
        .set_symbol_value("mib-buffer-list-ran", Value::Nil);

    let _ = builtin_make_indirect_buffer(
        &mut eval,
        vec![
            base,
            Value::string("*mib-clone-inhibit*"),
            Value::True,
            Value::True,
        ],
    )
    .expect("clone indirect buffer with inhibited buffer hooks");

    assert_eq!(
        eval.obarray()
            .symbol_value("mib-last-clone-buffer")
            .and_then(Value::as_str),
        Some("*mib-clone-inhibit*"),
        "clone-indirect-buffer-hook should still run"
    );
    assert_eq!(
        eval.obarray().symbol_value("mib-buffer-list-ran"),
        Some(&Value::Nil),
        "buffer-list-update-hook should be inhibited"
    );
}

#[test]
fn make_indirect_buffer_clone_nil_resets_buffer_state() {
    let mut eval = super::super::eval::Evaluator::new();
    let base = create_unique_test_buffer(&mut eval, "*mib-clone-nil-base*");
    let Value::Buffer(base_id) = base else {
        panic!("expected buffer object");
    };

    let _ = builtin_set_buffer(&mut eval, vec![base]).unwrap();
    let _ =
        eval.buffers
            .set_buffer_local_property(base_id, "major-mode", Value::symbol("neo-mode"));
    let _ = eval
        .buffers
        .set_buffer_local_property(base_id, "mode-name", Value::string("Neo"));

    let indirect =
        builtin_make_indirect_buffer(&mut eval, vec![base, Value::string("*mib-default*")])
            .expect("indirect buffer without clone");
    let Value::Buffer(indirect_id) = indirect else {
        panic!("expected buffer object");
    };

    let indirect_buf = eval.buffers.get(indirect_id).expect("indirect buffer");
    assert_eq!(
        indirect_buf.get_buffer_local("major-mode").copied(),
        Some(Value::symbol("fundamental-mode"))
    );
    assert_eq!(
        indirect_buf
            .get_buffer_local("mode-name")
            .and_then(Value::as_str),
        Some("Fundamental")
    );
}

#[test]
fn buffer_modified_tick_semantics() {
    let mut eval = super::super::eval::Evaluator::new();

    assert_eq!(
        builtin_buffer_modified_tick(&mut eval, vec![]).unwrap(),
        Value::Int(1)
    );
    assert_eq!(
        builtin_buffer_chars_modified_tick(&mut eval, vec![]).unwrap(),
        Value::Int(1)
    );

    builtin_insert(&mut eval, vec![Value::string("x")]).unwrap();
    assert_eq!(
        builtin_buffer_modified_tick(&mut eval, vec![]).unwrap(),
        Value::Int(2)
    );
    assert_eq!(
        builtin_buffer_chars_modified_tick(&mut eval, vec![]).unwrap(),
        Value::Int(2)
    );

    builtin_set_buffer_modified_p(&mut eval, vec![Value::Nil]).unwrap();
    assert_eq!(
        builtin_buffer_modified_tick(&mut eval, vec![]).unwrap(),
        Value::Int(2)
    );
    assert_eq!(
        builtin_buffer_chars_modified_tick(&mut eval, vec![]).unwrap(),
        Value::Int(2)
    );

    builtin_delete_region(&mut eval, vec![Value::Int(1), Value::Int(2)]).unwrap();
    assert_eq!(
        builtin_buffer_modified_tick(&mut eval, vec![]).unwrap(),
        Value::Int(3)
    );
    assert_eq!(
        builtin_buffer_chars_modified_tick(&mut eval, vec![]).unwrap(),
        Value::Int(3)
    );

    let dead = create_unique_test_buffer(&mut eval, "*ticks-dead*");
    let _ = builtin_kill_buffer(&mut eval, vec![dead]).unwrap();
    assert_eq!(
        builtin_buffer_modified_tick(&mut eval, vec![dead]).unwrap(),
        Value::Int(1)
    );
    assert_eq!(
        builtin_buffer_chars_modified_tick(&mut eval, vec![dead]).unwrap(),
        Value::Int(1)
    );

    let type_error = builtin_buffer_modified_tick(&mut eval, vec![Value::symbol("x")])
        .expect_err("buffer-modified-tick should reject non-buffer optional arg");
    match type_error {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("bufferp"), Value::symbol("x")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let arity_error = builtin_buffer_chars_modified_tick(&mut eval, vec![Value::Nil, Value::Nil])
        .expect_err("buffer-chars-modified-tick should reject >1 args");
    match arity_error {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("buffer-chars-modified-tick"), Value::Int(2)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn insert_honors_inhibit_read_only_override() {
    let mut eval = super::super::eval::Evaluator::new();
    eval.obarray
        .set_symbol_value("buffer-read-only", Value::True);
    eval.obarray
        .set_symbol_value("inhibit-read-only", Value::True);

    builtin_insert(&mut eval, vec![Value::string("ok")]).expect("insert should bypass read-only");

    let buf = eval.buffers.current_buffer().expect("current buffer");
    assert_eq!(buf.buffer_string(), "ok");
    assert_eq!(buf.point_char() as i64 + 1, 3);
}

#[test]
fn insert_inherit_variants_reuse_insert_semantics() {
    let mut eval = super::super::eval::Evaluator::new();
    assert_eq!(
        builtin_insert_and_inherit(
            &mut eval,
            vec![Value::string("a"), Value::Char('b'), Value::Int('c' as i64)],
        )
        .unwrap(),
        Value::Nil
    );
    assert_eq!(
        builtin_insert_before_markers_and_inherit(&mut eval, vec![Value::string("d")]).unwrap(),
        Value::Nil
    );
    assert_eq!(
        builtin_buffer_string(&mut eval, vec![]).unwrap(),
        Value::string("abcd")
    );

    let type_error = builtin_insert_and_inherit(&mut eval, vec![Value::list(vec![Value::Int(1)])])
        .expect_err("insert-and-inherit should reject non char/string values");
    match type_error {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![
                    Value::symbol("char-or-string-p"),
                    Value::list(vec![Value::Int(1)])
                ]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn insert_copies_string_text_properties_into_buffer() {
    let mut eval = super::super::eval::Evaluator::new();
    let text = Value::string("xy");
    let str_id = match text {
        Value::Str(id) => id,
        other => panic!("expected string value, got {other:?}"),
    };

    let mut table = crate::buffer::text_props::TextPropertyTable::new();
    table.put_property(0, 2, "face", Value::symbol("bold"));
    crate::emacs_core::value::set_string_text_properties_table(str_id, table);

    assert_eq!(builtin_insert(&mut eval, vec![text]).unwrap(), Value::Nil);

    let buf = eval.buffers.current_buffer().expect("current buffer");
    assert_eq!(buf.buffer_string(), "xy");
    assert_eq!(
        buf.text_props.get_property(0, "face"),
        Some(&Value::symbol("bold"))
    );
    assert_eq!(
        buf.text_props.get_property(1, "face"),
        Some(&Value::symbol("bold"))
    );
}

#[test]
fn insert_and_inherit_copies_previous_text_properties() {
    let mut eval = super::super::eval::Evaluator::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.insert("ab");
        buf.text_props
            .put_property(1, 2, "face", Value::symbol("bold"));
    }

    assert_eq!(
        builtin_insert_and_inherit(&mut eval, vec![Value::string("X")]).unwrap(),
        Value::Nil
    );

    let buf = eval.buffers.current_buffer().expect("current buffer");
    assert_eq!(buf.buffer_string(), "abX");
    assert_eq!(
        buf.text_props.get_property(2, "face"),
        Some(&Value::symbol("bold"))
    );
}

#[test]
fn delete_all_overlays_clears_current_buffer() {
    let mut eval = super::super::eval::Evaluator::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.insert("hello");
        buf.overlays.make_overlay(0, 2);
        buf.overlays.make_overlay(1, 4);
        assert_eq!(buf.overlays.len(), 2);
    }

    assert_eq!(
        builtin_delete_all_overlays(&mut eval, vec![]).unwrap(),
        Value::Nil
    );

    let buf = eval.buffers.current_buffer().expect("current buffer");
    assert_eq!(buf.overlays.len(), 0);
}

#[test]
fn insert_buffer_substring_inserts_source_region() {
    let mut eval = super::super::eval::Evaluator::new();
    let source_id = eval.buffers.create_buffer("*ibs-source*");
    eval.buffers.set_current(source_id);
    builtin_insert(&mut eval, vec![Value::string("abcdef")]).unwrap();

    let dest_id = eval.buffers.create_buffer("*ibs-dest*");
    eval.buffers.set_current(dest_id);
    builtin_insert(&mut eval, vec![Value::string("start:")]).unwrap();

    assert_eq!(
        builtin_insert_buffer_substring(
            &mut eval,
            vec![Value::Buffer(source_id), Value::Int(2), Value::Int(5)],
        )
        .unwrap(),
        Value::Nil
    );
    assert_eq!(
        eval.buffers
            .get(dest_id)
            .expect("destination buffer should exist")
            .buffer_string(),
        "start:bcd"
    );

    let bad_designator = builtin_insert_buffer_substring(&mut eval, vec![Value::Int(9)])
        .expect_err("insert-buffer-substring should reject non-buffer designators");
    match bad_designator {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::Int(9)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let bad_start = builtin_insert_buffer_substring(
        &mut eval,
        vec![Value::Buffer(source_id), Value::string("x")],
    )
    .expect_err("insert-buffer-substring should reject non integer-or-marker START");
    match bad_start {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("integer-or-marker-p"), Value::string("x")]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn insert_buffer_substring_defaults_to_source_accessible_region() {
    let mut eval = super::super::eval::Evaluator::new();
    let source_id = eval.buffers.create_buffer("*ibs-source-defaults*");
    eval.buffers.set_current(source_id);
    builtin_insert(&mut eval, vec![Value::string("abcdef")]).unwrap();
    let _ = eval.buffers.narrow_buffer_to_region(source_id, 1, 4);

    let dest_id = eval.buffers.create_buffer("*ibs-dest-defaults*");
    eval.buffers.set_current(dest_id);

    assert_eq!(
        builtin_insert_buffer_substring(&mut eval, vec![Value::Buffer(source_id)]).unwrap(),
        Value::Nil
    );
    assert_eq!(
        eval.buffers
            .get(dest_id)
            .expect("destination buffer should exist")
            .buffer_string(),
        "bcd"
    );
}

#[test]
fn insert_buffer_substring_signals_when_bounds_escape_source_narrowing() {
    let mut eval = super::super::eval::Evaluator::new();
    let source_id = eval.buffers.create_buffer("*ibs-source-range*");
    eval.buffers.set_current(source_id);
    builtin_insert(&mut eval, vec![Value::string("abcdef")]).unwrap();
    let _ = eval.buffers.narrow_buffer_to_region(source_id, 1, 4);

    let err =
        builtin_insert_buffer_substring(&mut eval, vec![Value::Buffer(source_id), Value::Int(1)])
            .expect_err("insert-buffer-substring should reject out-of-range narrowed START");
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "args-out-of-range");
            assert_eq!(sig.data, vec![Value::Int(1), Value::Int(5)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn insert_buffer_substring_rejects_deleted_buffer_object() {
    let mut eval = super::super::eval::Evaluator::new();
    let dead = create_unique_test_buffer(&mut eval, "*ibs-dead*");
    let _ = builtin_kill_buffer(&mut eval, vec![dead]).unwrap();

    let err = builtin_insert_buffer_substring(&mut eval, vec![dead])
        .expect_err("insert-buffer-substring should reject deleted buffer objects");
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data, vec![Value::string("Selecting deleted buffer")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn kill_all_local_variables_clears_buffer_locals() {
    let mut eval = super::super::eval::Evaluator::new();
    {
        let buf = eval.buffers.current_buffer_mut().unwrap();
        buf.set_buffer_local("tab-width", Value::Int(8));
        buf.set_buffer_local("fill-column", Value::Int(80));
    }

    assert_eq!(
        builtin_kill_all_local_variables(&mut eval, vec![]).unwrap(),
        Value::Nil
    );

    let buf = eval.buffers.current_buffer().unwrap();
    assert!(buf.get_buffer_local("tab-width").is_none());
    assert!(buf.get_buffer_local("fill-column").is_none());
    assert_eq!(buf.get_buffer_local("buffer-read-only"), Some(&Value::Nil));
}

#[test]
fn ntake_destructively_truncates_lists() {
    let list = Value::list(vec![
        Value::Int(1),
        Value::Int(2),
        Value::Int(3),
        Value::Int(4),
    ]);
    let kept = builtin_ntake(vec![Value::Int(2), list]).unwrap();
    assert_eq!(kept, Value::list(vec![Value::Int(1), Value::Int(2)]));
    assert_eq!(
        list_to_vec(&list).expect("list should stay proper after ntake"),
        vec![Value::Int(1), Value::Int(2)]
    );

    let unchanged = Value::list(vec![Value::Int(5), Value::Int(6)]);
    assert_eq!(
        builtin_ntake(vec![Value::Int(10), unchanged]).unwrap(),
        unchanged
    );
    assert_eq!(
        builtin_ntake(vec![Value::Int(0), list]).unwrap(),
        Value::Nil
    );

    let type_error = builtin_ntake(vec![Value::Int(1), Value::Int(3)])
        .expect_err("ntake should reject non-list arguments");
    match type_error {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("listp"), Value::Int(3)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn replace_buffer_contents_and_set_buffer_multibyte_runtime_semantics() {
    let mut eval = super::super::eval::Evaluator::new();
    let source_id = eval.buffers.create_buffer("*rbc-source*");
    eval.buffers.set_current(source_id);
    builtin_insert(&mut eval, vec![Value::string("source-text")]).unwrap();

    let dest_id = eval.buffers.create_buffer("*rbc-dest*");
    eval.buffers.set_current(dest_id);
    builtin_insert(&mut eval, vec![Value::string("dest-text")]).unwrap();

    assert_eq!(
        builtin_replace_buffer_contents_eval(&mut eval, vec![Value::Buffer(source_id)]).unwrap(),
        Value::True
    );
    assert_eq!(
        eval.buffers
            .get(dest_id)
            .expect("destination buffer should exist")
            .buffer_string(),
        "source-text"
    );

    assert_eq!(
        builtin_set_buffer_multibyte_eval(&mut eval, vec![Value::Nil]).unwrap(),
        Value::Nil
    );
    assert!(!eval.buffers.current_buffer().unwrap().multibyte);

    assert_eq!(
        builtin_set_buffer_multibyte_eval(&mut eval, vec![Value::symbol("foo")]).unwrap(),
        Value::symbol("foo")
    );
    assert!(eval.buffers.current_buffer().unwrap().multibyte);
}

#[test]
fn compare_buffer_substrings_nil_bounds_use_accessible_region() {
    let mut eval = super::super::eval::Evaluator::new();
    let left_id = eval.buffers.create_buffer("*cbs-left*");
    eval.buffers.set_current(left_id);
    builtin_insert(&mut eval, vec![Value::string("xaBCy")]).unwrap();
    let _ = eval.buffers.narrow_buffer_to_region(left_id, 1, 4);

    let right_id = eval.buffers.create_buffer("*cbs-right*");
    eval.buffers.set_current(right_id);
    builtin_insert(&mut eval, vec![Value::string("zaBCw")]).unwrap();
    let _ = eval.buffers.narrow_buffer_to_region(right_id, 1, 4);

    assert_eq!(
        builtin_compare_buffer_substrings(
            &mut eval,
            vec![
                Value::Buffer(left_id),
                Value::Nil,
                Value::Nil,
                Value::Buffer(right_id),
                Value::Nil,
                Value::Nil,
            ],
        )
        .unwrap(),
        Value::Int(0)
    );
}

#[test]
fn compare_buffer_substrings_signals_when_bounds_escape_narrowing() {
    let mut eval = super::super::eval::Evaluator::new();
    let left_id = eval.buffers.create_buffer("*cbs-left-range*");
    eval.buffers.set_current(left_id);
    builtin_insert(&mut eval, vec![Value::string("xaBCy")]).unwrap();
    let _ = eval.buffers.narrow_buffer_to_region(left_id, 1, 4);

    let right_id = eval.buffers.create_buffer("*cbs-right-range*");
    eval.buffers.set_current(right_id);
    builtin_insert(&mut eval, vec![Value::string("zaBCw")]).unwrap();
    let _ = eval.buffers.narrow_buffer_to_region(right_id, 1, 4);

    let err = builtin_compare_buffer_substrings(
        &mut eval,
        vec![
            Value::Buffer(left_id),
            Value::Int(1),
            Value::Nil,
            Value::Buffer(right_id),
            Value::Nil,
            Value::Nil,
        ],
    )
    .expect_err("compare-buffer-substrings should reject out-of-range narrowed START");
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "args-out-of-range");
            assert_eq!(sig.data, vec![Value::Int(1), Value::Nil]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn compare_buffer_substrings_rejects_deleted_buffer_object() {
    let mut eval = super::super::eval::Evaluator::new();
    let dead = create_unique_test_buffer(&mut eval, "*cbs-dead*");
    let _ = builtin_kill_buffer(&mut eval, vec![dead]).unwrap();

    let live = create_unique_test_buffer(&mut eval, "*cbs-live*");
    eval.buffers
        .set_current(expect_buffer_id(&live).expect("buffer id"));
    builtin_insert(&mut eval, vec![Value::string("abc")]).unwrap();

    let err = builtin_compare_buffer_substrings(
        &mut eval,
        vec![dead, Value::Nil, Value::Nil, live, Value::Nil, Value::Nil],
    )
    .expect_err("compare-buffer-substrings should reject deleted buffer objects");
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data, vec![Value::string("Selecting deleted buffer")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn replace_region_contents_replaces_from_string_and_buffer_sources() {
    let mut eval = super::super::eval::Evaluator::new();
    builtin_insert(&mut eval, vec![Value::string("abXXef")]).unwrap();

    assert_eq!(
        builtin_replace_region_contents_eval(
            &mut eval,
            vec![
                Value::Int(3),
                Value::Int(5),
                Value::string("cd"),
                Value::Int(0)
            ]
        )
        .unwrap(),
        Value::True
    );
    assert_eq!(
        eval.buffers.current_buffer().unwrap().buffer_string(),
        "abcdef"
    );

    let source_id = eval.buffers.create_buffer("*rrc-source*");
    eval.buffers.set_current(source_id);
    builtin_insert(&mut eval, vec![Value::string("1234")]).unwrap();

    let dest_id = eval.buffers.create_buffer("*rrc-dest*");
    eval.buffers.set_current(dest_id);
    builtin_insert(&mut eval, vec![Value::string("abYYef")]).unwrap();

    assert_eq!(
        builtin_replace_region_contents_eval(
            &mut eval,
            vec![Value::Int(3), Value::Int(5), Value::Buffer(source_id)]
        )
        .unwrap(),
        Value::True
    );
    assert_eq!(
        eval.buffers.current_buffer().unwrap().buffer_string(),
        "ab1234ef"
    );
}

#[test]
fn replace_region_contents_accepts_vector_buffer_slices() {
    let mut eval = super::super::eval::Evaluator::new();
    let source_id = eval.buffers.create_buffer("*rrc-slice-source*");
    eval.buffers.set_current(source_id);
    builtin_insert(&mut eval, vec![Value::string("1234")]).unwrap();

    let dest_id = eval.buffers.create_buffer("*rrc-slice-dest*");
    eval.buffers.set_current(dest_id);
    builtin_insert(&mut eval, vec![Value::string("abZZef")]).unwrap();

    assert_eq!(
        builtin_replace_region_contents_eval(
            &mut eval,
            vec![
                Value::Int(3),
                Value::Int(5),
                Value::vector(vec![Value::Buffer(source_id), Value::Int(2), Value::Int(4)])
            ]
        )
        .unwrap(),
        Value::True
    );
    assert_eq!(
        eval.buffers
            .get(dest_id)
            .expect("destination buffer should exist")
            .buffer_string(),
        "ab23ef"
    );
}

#[test]
fn split_window_internal_validates_core_argument_types() {
    let mut eval = super::super::eval::Evaluator::new();
    let split = builtin_split_window_internal(
        &mut eval,
        vec![Value::Nil, Value::Nil, Value::symbol("below"), Value::Nil],
    )
    .unwrap();
    assert!(matches!(split, Value::Window(_)));

    let window_type = builtin_split_window_internal(
        &mut eval,
        vec![
            Value::symbol("not-a-window"),
            Value::Nil,
            Value::symbol("below"),
            Value::Nil,
        ],
    )
    .expect_err("split-window-internal should reject non-window objects");
    match window_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("windowp"), Value::symbol("not-a-window")]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let size_type = builtin_split_window_internal(
        &mut eval,
        vec![
            Value::Nil,
            Value::string("bad"),
            Value::symbol("below"),
            Value::Nil,
        ],
    )
    .expect_err("split-window-internal should reject non-fixnum sizes");
    match size_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("fixnump"), Value::string("bad")]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let side_type = builtin_split_window_internal(
        &mut eval,
        vec![Value::Nil, Value::Nil, Value::Int(9), Value::Nil],
    )
    .expect_err("split-window-internal should reject non-symbol SIDE");
    match side_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("symbolp"), Value::Int(9)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn barf_bury_char_equal_cl_type_and_cancel_semantics() {
    let mut eval = super::super::eval::Evaluator::new();

    assert!(
        builtin_char_equal(&eval, vec![Value::Int(97), Value::Int(65)])
            .unwrap()
            .is_truthy()
    );
    eval.obarray
        .set_symbol_value("case-fold-search", Value::Nil);
    assert!(
        builtin_char_equal(&eval, vec![Value::Int(97), Value::Int(65)])
            .unwrap()
            .is_nil()
    );
    eval.obarray
        .set_symbol_value("case-fold-search", Value::True);

    let char_type = builtin_char_equal(&eval, vec![Value::Int(1), Value::string("a")])
        .expect_err("char-equal should reject non-character args");
    match char_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("characterp"), Value::string("a")]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    assert_eq!(
        builtin_cl_type_of(vec![Value::Nil]).unwrap(),
        Value::symbol("null")
    );
    assert_eq!(
        builtin_cl_type_of(vec![Value::True]).unwrap(),
        Value::symbol("boolean")
    );
    assert_eq!(
        builtin_cl_type_of(vec![Value::Int(1)]).unwrap(),
        Value::symbol("fixnum")
    );
    assert_eq!(
        builtin_cl_type_of(vec![Value::Float(1.0, next_float_id())]).unwrap(),
        Value::symbol("float")
    );
    assert_eq!(
        builtin_cl_type_of(vec![Value::string("x")]).unwrap(),
        Value::symbol("string")
    );
    assert_eq!(
        builtin_cl_type_of(vec![Value::symbol("foo")]).unwrap(),
        Value::symbol("symbol")
    );
    assert_eq!(
        builtin_cl_type_of(vec![Value::cons(Value::Int(1), Value::Int(2))]).unwrap(),
        Value::symbol("cons")
    );
    assert_eq!(
        builtin_cl_type_of(vec![Value::vector(vec![Value::Int(1)])]).unwrap(),
        Value::symbol("vector")
    );
    assert_eq!(
        builtin_cl_type_of(vec![Value::hash_table(HashTableTest::Equal)]).unwrap(),
        Value::symbol("hash-table")
    );
    assert_eq!(
        builtin_cl_type_of(vec![Value::Subr(intern("car"))]).unwrap(),
        Value::symbol("primitive-function")
    );
    let lambda = Value::make_lambda(LambdaData {
        params: LambdaParams::simple(vec![intern("x")]),
        body: Vec::new().into(),
        env: None,
        docstring: None,
        doc_form: None,
    });
    assert_eq!(
        builtin_cl_type_of(vec![lambda]).unwrap(),
        Value::symbol("interpreted-function")
    );

    assert_eq!(builtin_cancel_kbd_macro_events(vec![]).unwrap(), Value::Nil);
    let cancel_arity = builtin_cancel_kbd_macro_events(vec![Value::Nil])
        .expect_err("cancel-kbd-macro-events should reject args");
    match cancel_arity {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("cancel-kbd-macro-events"), Value::Int(1)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let barf_buffer = builtin_get_buffer_create(&mut eval, vec![Value::string("*barf*")])
        .expect("create buffer for barf-if-buffer-read-only tests");
    let _ = builtin_set_buffer(&mut eval, vec![barf_buffer]).expect("select barf test buffer");

    assert_eq!(
        builtin_barf_if_buffer_read_only(&mut eval, vec![Value::Int(0)]).unwrap(),
        Value::Nil
    );
    if let Some(buf) = eval.buffers.current_buffer_mut() {
        buf.set_buffer_local("buffer-read-only", Value::True);
    }
    let barf_read_only = builtin_barf_if_buffer_read_only(&mut eval, vec![])
        .expect_err("barf-if-buffer-read-only should signal on read-only buffers");
    match barf_read_only {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "buffer-read-only"),
        other => panic!("unexpected flow: {other:?}"),
    }

    let barf_range = builtin_barf_if_buffer_read_only(&mut eval, vec![Value::Int(0)])
        .expect_err("barf-if-buffer-read-only should check lower-bound positions");
    match barf_range {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "args-out-of-range");
            assert_eq!(sig.data, vec![Value::Int(0), Value::Int(0)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let barf_type = builtin_barf_if_buffer_read_only(&mut eval, vec![Value::string("x")])
        .expect_err("barf-if-buffer-read-only should reject non-fixnum positions");
    match barf_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("fixnump"), Value::string("x")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let barf_arity = builtin_barf_if_buffer_read_only(&mut eval, vec![Value::Nil, Value::Nil])
        .expect_err("barf-if-buffer-read-only should reject >1 args");
    match barf_arity {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("barf-if-buffer-read-only"), Value::Int(2)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
    if let Some(buf) = eval.buffers.current_buffer_mut() {
        buf.set_buffer_local("buffer-read-only", Value::Nil);
    }

    let buffer = create_unique_test_buffer(&mut eval, "*bury*");
    assert_eq!(
        builtin_bury_buffer_internal(&mut eval, vec![buffer]).unwrap(),
        Value::Nil
    );
    let bury_type = builtin_bury_buffer_internal(&mut eval, vec![Value::symbol("x")])
        .expect_err("bury-buffer-internal should reject non-buffer values");
    match bury_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("bufferp"), Value::symbol("x")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
    let bury_arity = builtin_bury_buffer_internal(&mut eval, vec![])
        .expect_err("bury-buffer-internal should reject wrong arity");
    match bury_arity {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("bury-buffer-internal"), Value::Int(0)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn byte_position_and_clear_bitmap_semantics() {
    let mut eval = super::super::eval::Evaluator::new();

    assert_eq!(
        builtin_byte_to_position(&mut eval, vec![Value::Int(1)]).unwrap(),
        Value::Int(1)
    );
    assert_eq!(
        builtin_byte_to_position(&mut eval, vec![Value::Int(2)]).unwrap(),
        Value::Nil
    );

    builtin_erase_buffer(&mut eval, vec![]).unwrap();
    builtin_insert(&mut eval, vec![Value::string("a\u{00E9}")]).unwrap();

    assert_eq!(
        builtin_byte_to_position(&mut eval, vec![Value::Int(1)]).unwrap(),
        Value::Int(1)
    );
    assert_eq!(
        builtin_byte_to_position(&mut eval, vec![Value::Int(2)]).unwrap(),
        Value::Int(2)
    );
    assert_eq!(
        builtin_byte_to_position(&mut eval, vec![Value::Int(3)]).unwrap(),
        Value::Int(2)
    );
    assert_eq!(
        builtin_byte_to_position(&mut eval, vec![Value::Int(4)]).unwrap(),
        Value::Int(3)
    );
    assert_eq!(
        builtin_byte_to_position(&mut eval, vec![Value::Int(5)]).unwrap(),
        Value::Nil
    );
    assert_eq!(
        builtin_byte_to_position(&mut eval, vec![Value::Int(0)]).unwrap(),
        Value::Nil
    );
    assert_eq!(
        builtin_byte_to_position(&mut eval, vec![Value::Int(-1)]).unwrap(),
        Value::Nil
    );

    let byte_to_position_type = builtin_byte_to_position(&mut eval, vec![Value::string("x")])
        .expect_err("byte-to-position should enforce fixnum input");
    match byte_to_position_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("fixnump"), Value::string("x")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let byte_to_position_arity =
        builtin_byte_to_position(&mut eval, vec![Value::Int(1), Value::Int(2)])
            .expect_err("byte-to-position should reject wrong arity");
    match byte_to_position_arity {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("byte-to-position"), Value::Int(2)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let byte_to_string = builtin_byte_to_string(vec![Value::Int(255)]).unwrap();
    assert_eq!(
        builtin_get_byte(&mut eval, vec![Value::Int(0), byte_to_string]).unwrap(),
        Value::Int(255)
    );

    let byte_to_string_type = builtin_byte_to_string(vec![Value::symbol("x")])
        .expect_err("byte-to-string should enforce fixnum input");
    match byte_to_string_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("fixnump"), Value::symbol("x")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let byte_to_string_range = builtin_byte_to_string(vec![Value::Int(256)])
        .expect_err("byte-to-string should reject bytes above 255");
    match byte_to_string_range {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data, vec![Value::string("Invalid byte")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    assert_eq!(builtin_bitmap_spec_p(vec![Value::Nil]).unwrap(), Value::Nil);
    let bitmap_arity =
        builtin_bitmap_spec_p(vec![]).expect_err("bitmap-spec-p should reject wrong arity");
    match bitmap_arity {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("bitmap-spec-p"), Value::Int(0)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    assert_eq!(builtin_clear_face_cache(vec![]).unwrap(), Value::Nil);
    assert_eq!(
        builtin_clear_face_cache(vec![Value::symbol("all")]).unwrap(),
        Value::Nil
    );
    let clear_face_arity = builtin_clear_face_cache(vec![Value::Nil, Value::Nil])
        .expect_err("clear-face-cache should reject >1 args");
    match clear_face_arity {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("clear-face-cache"), Value::Int(2)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    assert_eq!(
        builtin_clear_buffer_auto_save_failure(vec![]).unwrap(),
        Value::Nil
    );
    let clear_auto_save_arity = builtin_clear_buffer_auto_save_failure(vec![Value::Nil])
        .expect_err("clear-buffer-auto-save-failure should reject args");
    match clear_auto_save_arity {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![
                    Value::symbol("clear-buffer-auto-save-failure"),
                    Value::Int(1)
                ]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn buffer_undo_designators_match_deleted_and_missing_buffer_semantics() {
    let mut eval = super::super::eval::Evaluator::new();

    let enable_missing_name =
        builtin_buffer_enable_undo(&mut eval, vec![Value::string("*undo-enable-missing*")])
            .expect_err("buffer-enable-undo missing string should signal");
    match enable_missing_name {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("No buffer named *undo-enable-missing*")]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let disable_missing_name =
        builtin_buffer_disable_undo(&mut eval, vec![Value::string("*undo-disable-missing*")])
            .expect_err("buffer-disable-undo missing string should signal wrong-type-argument");
    match disable_missing_name {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::Nil]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let dead_for_enable = create_unique_test_buffer(&mut eval, "*undo-enable-deleted*");
    let _ = builtin_kill_buffer(&mut eval, vec![dead_for_enable]).unwrap();
    let enable_deleted = builtin_buffer_enable_undo(&mut eval, vec![dead_for_enable]).unwrap();
    assert_eq!(enable_deleted, Value::Nil);

    let dead_for_disable = create_unique_test_buffer(&mut eval, "*undo-disable-deleted*");
    let _ = builtin_kill_buffer(&mut eval, vec![dead_for_disable]).unwrap();
    let disable_deleted = builtin_buffer_disable_undo(&mut eval, vec![dead_for_disable])
        .expect_err("buffer-disable-undo should reject deleted buffer objects");
    match disable_deleted {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data, vec![Value::string("Selecting deleted buffer")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn other_buffer_prefers_live_alternative_and_enforces_arity() {
    let mut eval = super::super::eval::Evaluator::new();
    let _ = builtin_get_buffer_create(&mut eval, vec![Value::string("*Messages*")]).unwrap();
    let avoid = builtin_get_buffer_create(&mut eval, vec![Value::string("*ob-avoid*")])
        .expect("create avoid buffer");

    let other = builtin_other_buffer(&mut eval, vec![avoid]).expect("other-buffer");
    assert!(matches!(other, Value::Buffer(_)));
    assert_ne!(other, avoid);

    let from_non_buffer =
        builtin_other_buffer(&mut eval, vec![Value::Int(1)]).expect("other-buffer int");
    assert!(matches!(from_non_buffer, Value::Buffer(_)));

    let from_missing_name = builtin_other_buffer(&mut eval, vec![Value::string("*missing*")])
        .expect("other-buffer missing name");
    assert!(matches!(from_missing_name, Value::Buffer(_)));

    let err = builtin_other_buffer(
        &mut eval,
        vec![Value::Nil, Value::Nil, Value::Nil, Value::Nil],
    )
    .expect_err("other-buffer should reject more than three args");
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(sig.data, vec![Value::symbol("other-buffer"), Value::Int(4)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn featurep_accepts_optional_subfeature_arg() {
    let mut eval = super::super::eval::Evaluator::new();
    eval.set_variable(
        "features",
        Value::list(vec![Value::symbol("vm-featurep-present")]),
    );
    eval.obarray_mut().put_property(
        "vm-featurep-present",
        "subfeatures",
        Value::list(vec![Value::symbol("vm-sub"), Value::Int(1)]),
    );

    let base = builtin_featurep(&mut eval, vec![Value::symbol("vm-featurep-present")]).unwrap();
    assert_eq!(base, Value::True);

    let with_nil = builtin_featurep(
        &mut eval,
        vec![Value::symbol("vm-featurep-present"), Value::Nil],
    )
    .unwrap();
    assert_eq!(with_nil, Value::True);

    let with_sub = builtin_featurep(
        &mut eval,
        vec![
            Value::symbol("vm-featurep-present"),
            Value::symbol("vm-sub"),
        ],
    )
    .unwrap();
    assert_eq!(with_sub, Value::True);

    let with_other = builtin_featurep(
        &mut eval,
        vec![
            Value::symbol("vm-featurep-present"),
            Value::symbol("vm-other"),
        ],
    )
    .unwrap();
    assert_eq!(with_other, Value::Nil);
}

#[test]
fn featurep_subfeatures_property_must_be_list() {
    let mut eval = super::super::eval::Evaluator::new();
    eval.set_variable(
        "features",
        Value::list(vec![Value::symbol("vm-featurep-present")]),
    );
    eval.obarray_mut()
        .put_property("vm-featurep-present", "subfeatures", Value::Int(1));

    let err = builtin_featurep(
        &mut eval,
        vec![
            Value::symbol("vm-featurep-present"),
            Value::symbol("vm-sub"),
        ],
    )
    .expect_err("featurep should signal listp when subfeatures is not a list");
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("listp"), Value::Int(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn featurep_rejects_more_than_two_args() {
    let mut eval = super::super::eval::Evaluator::new();
    let err = builtin_featurep(
        &mut eval,
        vec![
            Value::symbol("vm-featurep-present"),
            Value::Nil,
            Value::symbol("extra"),
        ],
    )
    .expect_err("featurep should reject more than two arguments");
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(sig.data, vec![Value::symbol("featurep"), Value::Int(3)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn pure_dispatch_typed_string_constructor_builds_string() {
    let result = dispatch_builtin_pure(
        "string",
        vec![Value::Int(65), Value::Int(66), Value::Char('C')],
    )
    .expect("builtin string should resolve")
    .expect("builtin string should evaluate");
    assert_eq!(result, Value::string("ABC"));
}

#[test]
fn pure_dispatch_typed_propertize_validates_and_returns_string() {
    let result = dispatch_builtin_pure(
        "propertize",
        vec![
            Value::string("x"),
            Value::symbol("face"),
            Value::symbol("bold"),
        ],
    )
    .expect("builtin propertize should resolve")
    .expect("builtin propertize should evaluate");
    assert_eq!(result, Value::string("x"));
}

#[test]
fn pure_dispatch_typed_propertize_non_string_signals_stringp() {
    let result = dispatch_builtin_pure("propertize", vec![Value::Int(1)])
        .expect("builtin propertize should resolve")
        .expect_err("propertize should reject non-string first arg");
    match result {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::Int(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn pure_dispatch_typed_propertize_odd_property_list_signals_arity() {
    let result = dispatch_builtin_pure(
        "propertize",
        vec![Value::string("x"), Value::symbol("face")],
    )
    .expect("builtin propertize should resolve")
    .expect_err("propertize should reject odd property argument count");
    match result {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(sig.data, vec![Value::symbol("propertize"), Value::Int(2)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn pure_dispatch_typed_propertize_accepts_non_symbol_property_keys() {
    let result = dispatch_builtin_pure(
        "propertize",
        vec![Value::string("x"), Value::Int(1), Value::symbol("v")],
    )
    .expect("builtin propertize should resolve")
    .expect("builtin propertize should evaluate");
    assert_eq!(result, Value::string("x"));
}

#[test]
fn pure_dispatch_typed_unibyte_string_round_trips_bytes() {
    let s = dispatch_builtin_pure(
        "unibyte-string",
        vec![Value::Int(65), Value::Int(255), Value::Int(66)],
    )
    .expect("builtin unibyte-string should resolve")
    .expect("builtin unibyte-string should evaluate");

    let len = dispatch_builtin_pure("string-bytes", vec![s])
        .expect("builtin string-bytes should resolve")
        .expect("builtin string-bytes should evaluate");
    assert_eq!(len, Value::Int(3));

    let a = dispatch_builtin_pure("aref", vec![s, Value::Int(0)])
        .expect("builtin aref should resolve")
        .expect("builtin aref should evaluate");
    assert_eq!(a, Value::Int(65));

    let ff = dispatch_builtin_pure("aref", vec![s, Value::Int(1)])
        .expect("builtin aref should resolve")
        .expect("builtin aref should evaluate");
    assert_eq!(ff, Value::Int(255));

    let b = dispatch_builtin_pure("aref", vec![s, Value::Int(2)])
        .expect("builtin aref should resolve")
        .expect("builtin aref should evaluate");
    assert_eq!(b, Value::Int(66));
}

#[test]
fn pure_dispatch_typed_unibyte_string_validates_range_and_type() {
    let out_of_range = dispatch_builtin_pure("unibyte-string", vec![Value::Int(256)])
        .expect("builtin unibyte-string should resolve")
        .expect_err("expected args-out-of-range");
    match out_of_range {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "args-out-of-range");
            assert_eq!(
                sig.data,
                vec![Value::Int(256), Value::Int(0), Value::Int(255)]
            );
        }
        other => panic!("expected signal flow, got {other:?}"),
    }

    let wrong_type = dispatch_builtin_pure("unibyte-string", vec![Value::string("x")])
        .expect("builtin unibyte-string should resolve")
        .expect_err("expected wrong-type-argument");
    match wrong_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("integerp"), Value::string("x")]
            );
        }
        other => panic!("expected signal flow, got {other:?}"),
    }
}

#[test]
fn pure_dispatch_typed_vector_builds_vector() {
    let result = dispatch_builtin_pure("vector", vec![Value::Int(7), Value::Int(9)])
        .expect("builtin vector should resolve")
        .expect("builtin vector should evaluate");
    assert_eq!(result, Value::vector(vec![Value::Int(7), Value::Int(9)]));
}

#[test]
fn pure_dispatch_typed_make_vector_validates_wholenump_length() {
    let ok = dispatch_builtin_pure("make-vector", vec![Value::Int(3), Value::symbol("x")])
        .expect("builtin make-vector should resolve")
        .expect("builtin make-vector should evaluate");
    assert_eq!(
        ok,
        Value::vector(vec![
            Value::symbol("x"),
            Value::symbol("x"),
            Value::symbol("x")
        ])
    );

    for bad_len in [
        Value::Int(-1),
        Value::Float(1.5, next_float_id()),
        Value::symbol("foo"),
    ] {
        let err = dispatch_builtin_pure("make-vector", vec![bad_len, Value::Nil])
            .expect("builtin make-vector should resolve")
            .expect_err("invalid lengths should signal");
        match err {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(sig.data, vec![Value::symbol("wholenump"), bad_len]);
            }
            other => panic!("expected signal flow, got {other:?}"),
        }
    }
}

#[test]
fn pure_dispatch_typed_aref_bool_vector_returns_boolean_bits() {
    let bv = Value::vector(vec![
        Value::symbol("--bool-vector--"),
        Value::Int(4),
        Value::Int(0),
        Value::Int(0),
        Value::Int(0),
        Value::Int(0),
    ]);

    let initial = dispatch_builtin_pure("aref", vec![bv, Value::Int(2)])
        .expect("builtin aref should resolve")
        .expect("builtin aref should evaluate");
    assert!(initial.is_nil());

    let _ = dispatch_builtin_pure("aset", vec![bv, Value::Int(2), Value::True])
        .expect("builtin aset should resolve")
        .expect("builtin aset should evaluate");

    let updated = dispatch_builtin_pure("aref", vec![bv, Value::Int(2)])
        .expect("builtin aref should resolve")
        .expect("builtin aref should evaluate");
    assert!(updated.is_truthy());
}

#[test]
fn pure_dispatch_typed_aref_aset_char_table_uses_character_index_semantics() {
    let ct = Value::vector(vec![
        Value::symbol("--char-table--"),
        Value::Nil,
        Value::Nil,
        Value::symbol("syntax-table"),
        Value::Int(0),
    ]);

    let initial = dispatch_builtin_pure("aref", vec![ct, Value::Int(0)])
        .expect("builtin aref should resolve")
        .expect("builtin aref should evaluate");
    assert_eq!(initial, Value::Nil);

    let _ = dispatch_builtin_pure("aset", vec![ct, Value::Int(0x3F_FFFF), Value::Int(9)])
        .expect("builtin aset should resolve")
        .expect("builtin aset should evaluate");

    let edge = dispatch_builtin_pure("aref", vec![ct, Value::Int(0x3F_FFFF)])
        .expect("builtin aref should resolve")
        .expect("builtin aref should evaluate");
    assert_eq!(edge, Value::Int(9));

    let elt = dispatch_builtin_pure("elt", vec![ct, Value::Int(0x3F_FFFF)])
        .expect("builtin elt should resolve")
        .expect("builtin elt should evaluate");
    assert_eq!(elt, Value::Int(9));

    let negative = dispatch_builtin_pure("aref", vec![ct, Value::Int(-1)])
        .expect("builtin aref should resolve")
        .expect_err("negative char-table index should fail");
    match negative {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("characterp"), Value::Int(-1)],);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let too_large = dispatch_builtin_pure("aset", vec![ct, Value::Int(0x40_0000), Value::Int(1)])
        .expect("builtin aset should resolve")
        .expect_err("out-of-range char-table index should fail");
    match too_large {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("characterp"), Value::Int(0x40_0000)],
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn pure_dispatch_typed_length_family_uses_bool_vector_logical_length() {
    let bv = Value::vector(vec![
        Value::symbol("--bool-vector--"),
        Value::Int(3),
        Value::Int(1),
        Value::Int(0),
        Value::Int(1),
    ]);

    let len = dispatch_builtin_pure("length", vec![bv])
        .expect("builtin length should resolve")
        .expect("builtin length should evaluate");
    assert_eq!(len, Value::Int(3));

    let lt = dispatch_builtin_pure("length<", vec![bv, Value::Int(4)])
        .expect("builtin length< should resolve")
        .expect("builtin length< should evaluate");
    assert_eq!(lt, Value::True);

    let eq = dispatch_builtin_pure("length=", vec![bv, Value::Int(3)])
        .expect("builtin length= should resolve")
        .expect("builtin length= should evaluate");
    assert_eq!(eq, Value::True);

    let gt = dispatch_builtin_pure("length>", vec![bv, Value::Int(2)])
        .expect("builtin length> should resolve")
        .expect("builtin length> should evaluate");
    assert_eq!(gt, Value::True);
}

#[test]
fn pure_dispatch_typed_length_family_uses_char_table_logical_length() {
    let ct = Value::vector(vec![
        Value::symbol("--char-table--"),
        Value::Nil,
        Value::Nil,
        Value::symbol("syntax-table"),
        Value::Int(0),
    ]);

    let len = dispatch_builtin_pure("length", vec![ct])
        .expect("builtin length should resolve")
        .expect("builtin length should evaluate");
    assert_eq!(len, Value::Int(0x3F_FFFF));

    let lt = dispatch_builtin_pure("length<", vec![ct, Value::Int(100)])
        .expect("builtin length< should resolve")
        .expect("builtin length< should evaluate");
    assert_eq!(lt, Value::Nil);

    let eq = dispatch_builtin_pure("length=", vec![ct, Value::Int(0x3F_FFFF)])
        .expect("builtin length= should resolve")
        .expect("builtin length= should evaluate");
    assert_eq!(eq, Value::True);

    let gt = dispatch_builtin_pure("length>", vec![ct, Value::Int(0)])
        .expect("builtin length> should resolve")
        .expect("builtin length> should evaluate");
    assert_eq!(gt, Value::True);
}

#[test]
fn pure_dispatch_typed_aset_string_returns_new_element_and_computes_replacement() {
    let result = dispatch_builtin_pure(
        "aset",
        vec![Value::string("abc"), Value::Int(1), Value::Int(120)],
    )
    .expect("builtin aset should resolve")
    .expect("builtin aset should evaluate");
    assert_eq!(result, Value::Int(120));

    let replacement =
        aset_string_replacement(&Value::string("abc"), &Value::Int(1), &Value::Int(120))
            .expect("string replacement should succeed");
    assert_eq!(replacement, Value::string("axc"));
}

#[test]
fn pure_dispatch_typed_aset_string_errors_match_oracle() {
    let out_of_range = dispatch_builtin_pure(
        "aset",
        vec![Value::string("abc"), Value::Int(-1), Value::Int(120)],
    )
    .expect("builtin aset should resolve")
    .expect_err("aset should reject negative index");
    match out_of_range {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "args-out-of-range");
            assert_eq!(sig.data, vec![Value::string("abc"), Value::Int(-1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let wrong_type = dispatch_builtin_pure(
        "aset",
        vec![Value::string("abc"), Value::Int(1), Value::Nil],
    )
    .expect("builtin aset should resolve")
    .expect_err("aset should validate replacement character");
    match wrong_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("characterp"), Value::Nil]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn pure_dispatch_typed_char_string_conversions_work() {
    let as_code = dispatch_builtin_pure("string-to-char", vec![Value::string("A")])
        .expect("builtin string-to-char should resolve")
        .expect("builtin string-to-char should evaluate");
    assert_eq!(as_code, Value::Int(65));

    let as_string = dispatch_builtin_pure("char-to-string", vec![Value::Int(65)])
        .expect("builtin char-to-string should resolve")
        .expect("builtin char-to-string should evaluate");
    assert_eq!(as_string, Value::string("A"));
}

#[test]
fn pure_dispatch_typed_hash_table_round_trip() {
    let table = dispatch_builtin_pure(
        "make-hash-table",
        vec![Value::keyword(":test"), Value::symbol("equal")],
    )
    .expect("builtin make-hash-table should resolve")
    .expect("builtin make-hash-table should evaluate");

    dispatch_builtin_pure(
        "puthash",
        vec![Value::string("answer"), Value::Int(42), table],
    )
    .expect("builtin puthash should resolve")
    .expect("builtin puthash should evaluate");

    let value = dispatch_builtin_pure("gethash", vec![Value::string("answer"), table])
        .expect("builtin gethash should resolve")
        .expect("builtin gethash should evaluate");
    assert_eq!(value, Value::Int(42));

    let count = dispatch_builtin_pure("hash-table-count", vec![table])
        .expect("builtin hash-table-count should resolve")
        .expect("builtin hash-table-count should evaluate");
    assert_eq!(count, Value::Int(1));
}

#[test]
fn pure_dispatch_typed_hash_table_extended_builtins_round_trip() {
    let alias = Value::symbol("neovm--pure-dispatch-eq-test-alias");
    dispatch_builtin_pure(
        "define-hash-table-test",
        vec![alias, Value::symbol("eq"), Value::symbol("sxhash-eq")],
    )
    .expect("define-hash-table-test should resolve")
    .expect("define-hash-table-test should evaluate");

    let table = dispatch_builtin_pure("make-hash-table", vec![Value::keyword(":test"), alias])
        .expect("make-hash-table should resolve")
        .expect("make-hash-table should evaluate");

    let test_name = dispatch_builtin_pure("hash-table-test", vec![table])
        .expect("hash-table-test should resolve")
        .expect("hash-table-test should evaluate");
    assert_eq!(test_name, alias.clone());

    let size = dispatch_builtin_pure("hash-table-size", vec![table])
        .expect("hash-table-size should resolve")
        .expect("hash-table-size should evaluate");
    assert_eq!(size, Value::Int(0));

    let weakness = dispatch_builtin_pure("hash-table-weakness", vec![table])
        .expect("hash-table-weakness should resolve")
        .expect("hash-table-weakness should evaluate");
    assert_eq!(weakness, Value::Nil);

    let rehash_size = dispatch_builtin_pure("hash-table-rehash-size", vec![table])
        .expect("hash-table-rehash-size should resolve")
        .expect("hash-table-rehash-size should evaluate");
    assert_eq!(rehash_size, Value::Float(1.5, next_float_id()));

    let rehash_threshold = dispatch_builtin_pure("hash-table-rehash-threshold", vec![table])
        .expect("hash-table-rehash-threshold should resolve")
        .expect("hash-table-rehash-threshold should evaluate");
    assert_eq!(rehash_threshold, Value::Float(0.8125, next_float_id()));

    let sxhash = dispatch_builtin_pure("sxhash-eq", vec![Value::symbol("k")])
        .expect("sxhash-eq should resolve")
        .expect("sxhash-eq should evaluate");
    assert!(matches!(sxhash, Value::Int(_)));

    let buckets_before = dispatch_builtin_pure("internal--hash-table-buckets", vec![table])
        .expect("internal--hash-table-buckets should resolve")
        .expect("internal--hash-table-buckets should evaluate");
    assert_eq!(buckets_before, Value::Nil);

    let _ = dispatch_builtin_pure("puthash", vec![Value::symbol("k"), Value::Int(1), table])
        .expect("puthash should resolve")
        .expect("puthash should evaluate");

    let buckets_after = dispatch_builtin_pure("internal--hash-table-buckets", vec![table])
        .expect("internal--hash-table-buckets should resolve")
        .expect("internal--hash-table-buckets should evaluate");
    assert!(!buckets_after.is_nil());

    let histogram = dispatch_builtin_pure("internal--hash-table-histogram", vec![table])
        .expect("internal--hash-table-histogram should resolve")
        .expect("internal--hash-table-histogram should evaluate");
    assert!(!histogram.is_nil());

    let index_size = dispatch_builtin_pure("internal--hash-table-index-size", vec![table])
        .expect("internal--hash-table-index-size should resolve")
        .expect("internal--hash-table-index-size should evaluate");
    assert!(matches!(index_size, Value::Int(n) if n >= 1));

    let copied = dispatch_builtin_pure("copy-hash-table", vec![table])
        .expect("copy-hash-table should resolve")
        .expect("copy-hash-table should evaluate");
    let copied_test = dispatch_builtin_pure("hash-table-test", vec![copied])
        .expect("hash-table-test should resolve for copied table")
        .expect("hash-table-test should evaluate for copied table");
    assert_eq!(copied_test, alias);
}

#[test]
fn pure_dispatch_typed_define_hash_table_test_registers_alias() {
    let alias = Value::symbol("neovm--eq-test-alias");

    let defined = dispatch_builtin_pure(
        "define-hash-table-test",
        vec![alias, Value::symbol("eq"), Value::symbol("sxhash-eq")],
    )
    .expect("define-hash-table-test should resolve")
    .expect("define-hash-table-test should evaluate");
    assert_eq!(
        defined,
        Value::list(vec![Value::symbol("eq"), Value::symbol("sxhash-eq")])
    );

    let table = dispatch_builtin_pure("make-hash-table", vec![Value::keyword(":test"), alias])
        .expect("make-hash-table should resolve")
        .expect("make-hash-table should evaluate");
    let observed = crate::emacs_core::hashtab::builtin_hash_table_test(vec![table])
        .expect("hash-table-test should evaluate");
    assert_eq!(observed, alias);

    let Value::HashTable(table) = table else {
        panic!("expected hash table");
    };
    assert!(matches!(
        with_heap(|h| h.get_hash_table(table).test.clone()),
        HashTableTest::Eq
    ));
}

#[test]
fn define_hash_table_test_alias_is_thread_local() {
    let alias_name = "neovm--cross-thread-eq-test-alias";
    // Register alias on a spawned thread
    std::thread::spawn(move || {
        let _ = builtin_define_hash_table_test(vec![
            Value::symbol(alias_name),
            Value::symbol("eq"),
            Value::symbol("sxhash-eq"),
        ])
        .expect("define-hash-table-test should evaluate in worker thread");
        // Verify it IS visible on the same thread
        assert!(lookup_hash_table_test_alias(alias_name).is_some());
    })
    .join()
    .expect("worker thread should complete");

    // Alias registered on other thread should NOT be visible here
    assert!(lookup_hash_table_test_alias(alias_name).is_none());
}

#[test]
fn define_hash_table_test_alias_redefinition_updates_mapping() {
    let alias_name = "neovm--eq-test-alias-redefined";

    builtin_define_hash_table_test(vec![
        Value::symbol(alias_name),
        Value::symbol("eq"),
        Value::symbol("sxhash-eq"),
    ])
    .expect("initial define-hash-table-test should evaluate");
    let first = dispatch_builtin_pure(
        "make-hash-table",
        vec![Value::keyword(":test"), Value::symbol(alias_name)],
    )
    .expect("make-hash-table should resolve")
    .expect("make-hash-table should evaluate");
    let first_name = crate::emacs_core::hashtab::builtin_hash_table_test(vec![first])
        .expect("hash-table-test should evaluate for initial alias mapping");
    assert_eq!(first_name, Value::symbol(alias_name));

    let Value::HashTable(first) = first else {
        panic!("expected hash table");
    };
    assert!(matches!(
        with_heap(|h| h.get_hash_table(first).test.clone()),
        HashTableTest::Eq
    ));

    builtin_define_hash_table_test(vec![
        Value::symbol(alias_name),
        Value::symbol("equal"),
        Value::symbol("sxhash-equal"),
    ])
    .expect("redefined hash-table test should evaluate");
    let second = dispatch_builtin_pure(
        "make-hash-table",
        vec![Value::keyword(":test"), Value::symbol(alias_name)],
    )
    .expect("make-hash-table should resolve after alias redefinition")
    .expect("make-hash-table should evaluate after alias redefinition");
    let second_name = crate::emacs_core::hashtab::builtin_hash_table_test(vec![second])
        .expect("hash-table-test should evaluate after alias redefinition");
    assert_eq!(second_name, Value::symbol(alias_name));

    let Value::HashTable(second) = second else {
        panic!("expected hash table");
    };
    assert!(matches!(
        with_heap(|h| h.get_hash_table(second).test.clone()),
        HashTableTest::Equal
    ));
}

#[test]
fn pure_dispatch_typed_plist_and_symbol_round_trip() {
    let plist = dispatch_builtin_pure(
        "plist-put",
        vec![Value::Nil, Value::keyword(":lang"), Value::string("rust")],
    )
    .expect("builtin plist-put should resolve")
    .expect("builtin plist-put should evaluate");

    let lang = dispatch_builtin_pure("plist-get", vec![plist, Value::keyword(":lang")])
        .expect("builtin plist-get should resolve")
        .expect("builtin plist-get should evaluate");
    assert_eq!(lang, Value::string("rust"));

    let sym = dispatch_builtin_pure("make-symbol", vec![Value::string("neo-vm")])
        .expect("builtin make-symbol should resolve")
        .expect("builtin make-symbol should evaluate");
    let name = dispatch_builtin_pure("symbol-name", vec![sym])
        .expect("builtin symbol-name should resolve")
        .expect("builtin symbol-name should evaluate");
    assert_eq!(name, Value::string("neo-vm"));
}

#[test]
fn pure_dispatch_typed_math_ops_work() {
    let sqrt = dispatch_builtin_pure("sqrt", vec![Value::Int(4)])
        .expect("builtin sqrt should resolve")
        .expect("builtin sqrt should evaluate");
    assert_eq!(sqrt, Value::Float(2.0, next_float_id()));

    let expt = dispatch_builtin_pure("expt", vec![Value::Int(2), Value::Int(8)])
        .expect("builtin expt should resolve")
        .expect("builtin expt should evaluate");
    assert_eq!(expt, Value::Int(256));

    let nan_check = dispatch_builtin_pure("isnan", vec![Value::Float(f64::NAN, next_float_id())])
        .expect("builtin isnan should resolve")
        .expect("builtin isnan should evaluate");
    assert!(nan_check.is_truthy());
}

#[test]
fn pure_dispatch_typed_expt_and_isnan_type_errors_match_oracle() {
    let expt_base = dispatch_builtin_pure("expt", vec![Value::symbol("a"), Value::Int(2)])
        .expect("builtin expt should resolve")
        .expect_err("expt should reject non-numeric base");
    match expt_base {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("numberp"), Value::symbol("a")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let expt_exp = dispatch_builtin_pure("expt", vec![Value::Int(2), Value::symbol("a")])
        .expect("builtin expt should resolve")
        .expect_err("expt should reject non-numeric exponent");
    match expt_exp {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("numberp"), Value::symbol("a")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let isnan_non_float = dispatch_builtin_pure("isnan", vec![Value::Int(1)])
        .expect("builtin isnan should resolve")
        .expect_err("isnan should reject non-floats");
    match isnan_non_float {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("floatp"), Value::Int(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn pure_dispatch_typed_round_half_ties_to_even() {
    let positive_half = dispatch_builtin_pure("round", vec![Value::Float(2.5, next_float_id())])
        .expect("builtin round should resolve")
        .expect("builtin round should evaluate");
    assert_eq!(positive_half, Value::Int(2));

    let negative_half = dispatch_builtin_pure("round", vec![Value::Float(-2.5, next_float_id())])
        .expect("builtin round should resolve")
        .expect("builtin round should evaluate");
    assert_eq!(negative_half, Value::Int(-2));

    let zero_half = dispatch_builtin_pure("round", vec![Value::Float(0.5, next_float_id())])
        .expect("builtin round should resolve")
        .expect("builtin round should evaluate");
    assert_eq!(zero_half, Value::Int(0));

    let negative_zero_half =
        dispatch_builtin_pure("round", vec![Value::Float(-0.5, next_float_id())])
            .expect("builtin round should resolve")
            .expect("builtin round should evaluate");
    assert_eq!(negative_zero_half, Value::Int(0));
}

#[test]
fn pure_dispatch_typed_string_width_and_bytes_work() {
    let width = dispatch_builtin_pure("string-width", vec![Value::string("ab")])
        .expect("builtin string-width should resolve")
        .expect("builtin string-width should evaluate");
    assert_eq!(width, Value::Int(2));

    let bytes = dispatch_builtin_pure("string-bytes", vec![Value::string("ab")])
        .expect("builtin string-bytes should resolve")
        .expect("builtin string-bytes should evaluate");
    assert_eq!(bytes, Value::Int(2));
}

#[test]
fn pure_dispatch_typed_extended_list_ops_work() {
    let seq = Value::list(vec![
        Value::Int(1),
        Value::Int(2),
        Value::Int(3),
        Value::Int(4),
    ]);

    let truncated = dispatch_builtin_pure(
        "ntake",
        vec![
            Value::Int(2),
            Value::list(vec![Value::Int(7), Value::Int(8), Value::Int(9)]),
        ],
    )
    .expect("builtin ntake should resolve")
    .expect("builtin ntake should evaluate");
    assert_eq!(truncated, Value::list(vec![Value::Int(7), Value::Int(8)]));
}

#[test]
fn pure_dispatch_obarray_make_and_clear_use_vector_semantics() {
    let made = dispatch_builtin_pure("obarray-make", vec![Value::Int(3)])
        .expect("builtin obarray-make should resolve")
        .expect("builtin obarray-make should evaluate");
    let Value::Vector(created) = &made else {
        panic!("obarray-make should return vector");
    };
    let created = with_heap(|h| h.get_vector(*created).clone());
    assert_eq!(created.len(), 3);
    assert!(created.iter().all(Value::is_nil));

    let default = dispatch_builtin_pure("obarray-make", vec![])
        .expect("builtin obarray-make should resolve")
        .expect("builtin obarray-make should evaluate");
    let Value::Vector(default) = &default else {
        panic!("obarray-make default should return vector");
    };
    assert_eq!(with_heap(|h| h.get_vector(*default).len()), 1511);

    let table = Value::vector(vec![Value::Nil, Value::list(vec![Value::symbol("x")])]);
    let cleared = dispatch_builtin_pure("obarray-clear", vec![table])
        .expect("builtin obarray-clear should resolve")
        .expect("builtin obarray-clear should evaluate");
    assert!(cleared.is_nil());
    let Value::Vector(cleared) = &table else {
        panic!("table should stay vector");
    };
    assert!(
        with_heap(|h| h.get_vector(*cleared).clone())
            .iter()
            .all(Value::is_nil)
    );

    let wrong_type = dispatch_builtin_pure("obarray-clear", vec![Value::Int(1)])
        .expect("builtin obarray-clear should resolve")
        .expect_err("obarray-clear should reject non-obarray arguments");
    match wrong_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("obarrayp"), Value::Int(1)]);
        }
        other => panic!("expected signal, got: {other:?}"),
    }
}

#[test]
fn eval_dispatch_obarrayp_accepts_custom_obarrays() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let table = crate::emacs_core::builtins::symbols::builtin_obarray_make(vec![Value::Int(3)])
        .expect("obarray-make should evaluate");
    let result =
        crate::emacs_core::builtins::symbols::builtin_obarrayp_eval(&mut eval, vec![table])
            .expect("obarrayp should evaluate");
    assert!(result.is_truthy());

    let non_obarray = Value::vector(vec![Value::Int(0); 3]);
    let result =
        crate::emacs_core::builtins::symbols::builtin_obarrayp_eval(&mut eval, vec![non_obarray])
            .expect("obarrayp should evaluate");
    assert!(result.is_nil());
}

#[test]
fn pure_dispatch_make_temp_file_internal_delegates_make_temp_file() {
    let created = dispatch_builtin_pure(
        "make-temp-file-internal",
        vec![
            Value::string("neovm-mtfi-"),
            Value::Nil,
            Value::string(".tmp"),
            Value::Nil,
        ],
    )
    .expect("builtin make-temp-file-internal should resolve")
    .expect("builtin make-temp-file-internal should evaluate");
    let path = created
        .as_str()
        .expect("make-temp-file-internal should return file path");
    assert!(path.contains("neovm-mtfi-"));
    assert!(path.ends_with(".tmp"));
    assert!(std::path::Path::new(path).exists());
    std::fs::remove_file(path).expect("temp file should be removable");

    let mode_err = dispatch_builtin_pure(
        "make-temp-file-internal",
        vec![
            Value::string("neovm-mtfi-mode-"),
            Value::Nil,
            Value::string(".tmp"),
            Value::string("bad"),
        ],
    )
    .expect("builtin make-temp-file-internal should resolve")
    .expect_err("make-temp-file-internal should reject non-fixnum mode");
    match mode_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("fixnump"), Value::string("bad")]
            );
        }
        other => panic!("expected signal, got: {other:?}"),
    }
}

#[test]
fn pure_dispatch_minibuffer_and_frame_placeholders_match_compat_contracts() {
    let prompt_end = dispatch_builtin_pure("minibuffer-prompt-end", vec![])
        .expect("builtin minibuffer-prompt-end should resolve")
        .expect("builtin minibuffer-prompt-end should evaluate");
    assert_eq!(prompt_end, Value::Int(1));

    for (name, args) in vec![
        ("minibuffer-innermost-command-loop-p", vec![]),
        ("next-frame", vec![]),
        ("next-frame", vec![Value::Nil, Value::Nil]),
        ("previous-frame", vec![]),
        ("previous-frame", vec![Value::Nil, Value::Nil]),
        ("raise-frame", vec![]),
        ("raise-frame", vec![Value::Nil]),
        ("suspend-emacs", vec![]),
        ("suspend-emacs", vec![Value::string("hold")]),
    ] {
        let value = dispatch_builtin_pure(name, args)
            .expect("builtin placeholder should resolve")
            .expect("builtin placeholder should evaluate");
        assert!(value.is_nil(), "{name} should return nil");
    }

    let vertical_motion = dispatch_builtin_pure("vertical-motion", vec![Value::Int(3)])
        .expect("builtin vertical-motion should resolve")
        .expect("builtin vertical-motion should evaluate");
    assert_eq!(vertical_motion, Value::Int(3));

    let redisplay = dispatch_builtin_pure("redisplay", vec![])
        .expect("builtin redisplay should resolve")
        .expect("builtin redisplay should evaluate");
    assert_eq!(redisplay, Value::True);

    let redisplay_force = dispatch_builtin_pure("redisplay", vec![Value::True])
        .expect("builtin redisplay should resolve with optional arg")
        .expect("builtin redisplay should evaluate with optional arg");
    assert_eq!(redisplay_force, Value::True);
}

#[test]
fn pure_dispatch_buffer_placeholder_mutators_match_compat_contracts() {
    // rename-buffer is now an eval builtin — test it through the evaluator
    {
        let mut eval = crate::emacs_core::eval::Evaluator::new();
        let buf_id = eval.buffers.create_buffer("old-name");
        eval.buffers.set_current(buf_id);
        let renamed = dispatch_builtin(&mut eval, "rename-buffer", vec![Value::string("new-name")])
            .expect("builtin rename-buffer should resolve")
            .expect("builtin rename-buffer should evaluate");
        assert_eq!(renamed, Value::string("new-name"));
        assert_eq!(eval.buffers.get(buf_id).unwrap().name, "new-name");
    }

    let major_mode = dispatch_builtin_pure(
        "set-buffer-major-mode",
        vec![Value::Buffer(crate::buffer::BufferId(1))],
    )
    .expect("builtin set-buffer-major-mode should resolve")
    .expect("builtin set-buffer-major-mode should evaluate");
    assert!(major_mode.is_nil());

    let redisplay = dispatch_builtin_pure(
        "set-buffer-redisplay",
        vec![Value::Nil, Value::Int(1), Value::Int(1), Value::Int(0)],
    )
    .expect("builtin set-buffer-redisplay should resolve")
    .expect("builtin set-buffer-redisplay should evaluate");
    assert!(redisplay.is_nil());
}

#[test]
fn pure_dispatch_unicode_and_re_placeholders_match_compat_contracts() {
    let unicode = dispatch_builtin_pure(
        "put-unicode-property-internal",
        vec![Value::Nil, Value::Int(0), Value::Int(1)],
    )
    .expect("builtin put-unicode-property-internal should resolve")
    .expect("builtin put-unicode-property-internal should evaluate");
    assert!(unicode.is_nil());

    let re_default = dispatch_builtin_pure("re--describe-compiled", vec![Value::string("x")])
        .expect("builtin re--describe-compiled should resolve")
        .expect("builtin re--describe-compiled should evaluate");
    assert!(re_default.is_nil());

    let re_indent = dispatch_builtin_pure(
        "re--describe-compiled",
        vec![Value::string("x"), Value::Int(2)],
    )
    .expect("builtin re--describe-compiled should resolve with indent")
    .expect("builtin re--describe-compiled should evaluate with indent");
    assert!(re_indent.is_nil());
}

#[test]
fn pure_dispatch_map_placeholders_match_compat_contracts() {
    let map_charset_chars = dispatch_builtin_pure(
        "map-charset-chars",
        vec![Value::Nil, Value::symbol("unicode"), Value::Nil],
    )
    .expect("builtin map-charset-chars should resolve")
    .expect("builtin map-charset-chars should evaluate");
    assert!(map_charset_chars.is_nil());

    let map_keymap = dispatch_builtin_pure(
        "map-keymap",
        vec![Value::Nil, Value::list(vec![Value::symbol("keymap")])],
    )
    .expect("builtin map-keymap should resolve")
    .expect("builtin map-keymap should evaluate");
    assert!(map_keymap.is_nil());

    let map_keymap_internal = dispatch_builtin_pure(
        "map-keymap-internal",
        vec![Value::Nil, Value::list(vec![Value::symbol("keymap")])],
    )
    .expect("builtin map-keymap-internal should resolve")
    .expect("builtin map-keymap-internal should evaluate");
    assert!(map_keymap_internal.is_nil());

    let mapbacktrace = dispatch_builtin_pure("mapbacktrace", vec![Value::symbol("ignore")])
        .expect("builtin mapbacktrace should resolve")
        .expect("builtin mapbacktrace should evaluate");
    assert!(mapbacktrace.is_nil());
}

#[test]
fn pure_dispatch_record_and_state_placeholders_match_compat_contracts() {
    let make_record = dispatch_builtin_pure(
        "make-record",
        vec![Value::symbol("tag"), Value::Int(0), Value::Int(0)],
    )
    .expect("builtin make-record should resolve")
    .expect("builtin make-record should evaluate");
    assert!(make_record.is_record());

    let marker_last_position = dispatch_builtin_pure(
        "marker-last-position",
        vec![crate::emacs_core::marker::make_marker_value(
            None, None, false,
        )],
    )
    .expect("builtin marker-last-position should resolve")
    .expect("builtin marker-last-position should evaluate");
    assert_eq!(marker_last_position, Value::Int(0));

    // match-data--translate is now dispatched in eval path (needs &mut eval)
    assert!(dispatch_builtin_pure("match-data--translate", vec![Value::Int(1)]).is_none());

    let newline_cache_check = dispatch_builtin_pure("newline-cache-check", vec![])
        .expect("builtin newline-cache-check should resolve")
        .expect("builtin newline-cache-check should evaluate");
    assert!(newline_cache_check.is_nil());

    let old_selected_frame = dispatch_builtin_pure("old-selected-frame", vec![])
        .expect("builtin old-selected-frame should resolve")
        .expect("builtin old-selected-frame should evaluate");
    assert!(old_selected_frame.is_nil());
}

#[test]
fn pure_dispatch_frame_menu_mouse_placeholders_match_compat_contracts() {
    let frame_invisible =
        dispatch_builtin_pure("make-frame-invisible", vec![Value::Nil, Value::True])
            .expect("builtin make-frame-invisible should resolve")
            .expect("builtin make-frame-invisible should evaluate");
    assert!(frame_invisible.is_nil());

    let terminal_frame = dispatch_builtin_pure("make-terminal-frame", vec![Value::Nil])
        .expect("builtin make-terminal-frame should resolve")
        .expect_err("builtin make-terminal-frame should signal unknown terminal type");
    match terminal_frame {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "error"),
        other => panic!("expected signal, got {other:?}"),
    }

    let menu_at = dispatch_builtin_pure(
        "menu-bar-menu-at-x-y",
        vec![Value::Int(0), Value::Int(0), Value::Nil],
    )
    .expect("builtin menu-bar-menu-at-x-y should resolve")
    .expect("builtin menu-bar-menu-at-x-y should evaluate");
    assert!(menu_at.is_nil());

    let menu_active = dispatch_builtin_pure("menu-or-popup-active-p", vec![])
        .expect("builtin menu-or-popup-active-p should resolve")
        .expect("builtin menu-or-popup-active-p should evaluate");
    assert!(menu_active.is_nil());

    let mouse_pixel = dispatch_builtin_pure("mouse-pixel-position", vec![])
        .expect("builtin mouse-pixel-position should resolve")
        .expect("builtin mouse-pixel-position should evaluate");
    assert!(mouse_pixel.is_nil());

    let mouse_position = dispatch_builtin_pure("mouse-position", vec![])
        .expect("builtin mouse-position should resolve")
        .expect("builtin mouse-position should evaluate");
    assert!(mouse_position.is_nil());
}

#[test]
fn pure_dispatch_native_comp_placeholders_match_compat_contracts() {
    let available = dispatch_builtin_pure("native-comp-available-p", vec![])
        .expect("builtin native-comp-available-p should resolve")
        .expect("builtin native-comp-available-p should evaluate");
    assert!(available.is_truthy());

    let unit_file = dispatch_builtin_pure(
        "native-comp-unit-file",
        vec![Value::vector(vec![Value::keyword("native-comp-unit")])],
    )
    .expect("builtin native-comp-unit-file should resolve")
    .expect("builtin native-comp-unit-file should evaluate");
    assert!(unit_file.is_nil());

    let unit_set_file = dispatch_builtin_pure(
        "native-comp-unit-set-file",
        vec![
            Value::vector(vec![Value::keyword("native-comp-unit")]),
            Value::string("foo.eln"),
        ],
    )
    .expect("builtin native-comp-unit-set-file should resolve")
    .expect("builtin native-comp-unit-set-file should evaluate");
    assert!(unit_set_file.is_nil());

    let native_elisp_load =
        dispatch_builtin_pure("native-elisp-load", vec![Value::string("foo.eln")])
            .expect("builtin native-elisp-load should resolve")
            .expect_err("native-elisp-load should signal missing native file");
    match native_elisp_load {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "native-lisp-load-failed");
            assert_eq!(
                sig.data,
                vec![
                    Value::string("file does not exists"),
                    Value::string("foo.eln")
                ]
            );
        }
        other => panic!("expected signal, got: {other:?}"),
    }

    let new_fontset =
        dispatch_builtin_pure("new-fontset", vec![Value::string("x"), Value::string("y")])
            .expect("builtin new-fontset should resolve")
            .expect_err("new-fontset should reject non-XLFD names");
    match new_fontset {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Fontset name must be in XLFD format")]
            );
        }
        other => panic!("expected signal, got: {other:?}"),
    }
}

#[test]
fn pure_dispatch_open_overlay_placeholders_match_compat_contracts() {
    let open_font = dispatch_builtin_pure(
        "open-font",
        vec![Value::vector(vec![Value::keyword("font-entity")])],
    )
    .expect("builtin open-font should resolve")
    .expect("builtin open-font should evaluate");
    assert!(open_font.is_nil());

    let open_dribble = dispatch_builtin_pure("open-dribble-file", vec![Value::string("x.log")])
        .expect("builtin open-dribble-file should resolve")
        .expect("builtin open-dribble-file should evaluate");
    assert!(open_dribble.is_nil());

    let intervals = dispatch_builtin_pure("object-intervals", vec![Value::string("x")])
        .expect("builtin object-intervals should resolve")
        .expect("builtin object-intervals should evaluate");
    assert!(intervals.is_nil());

    let char_table =
        crate::emacs_core::chartable::make_char_table_value(Value::symbol("test-only"), Value::Nil);

    let optimized = dispatch_builtin_pure(
        "optimize-char-table",
        vec![char_table, Value::symbol("test-only")],
    )
    .expect("builtin optimize-char-table should resolve")
    .expect("builtin optimize-char-table should evaluate");
    assert!(optimized.is_nil());

    let overlays = dispatch_builtin_pure("overlay-lists", vec![])
        .expect("builtin overlay-lists should resolve")
        .expect("builtin overlay-lists should evaluate");
    assert!(overlays.is_nil());

    let recentered = dispatch_builtin_pure("overlay-recenter", vec![Value::Int(0)])
        .expect("builtin overlay-recenter should resolve")
        .expect("builtin overlay-recenter should evaluate");
    assert!(recentered.is_nil());
}

#[test]
fn pure_dispatch_profiler_placeholders_match_compat_contracts() {
    let cpu_log = dispatch_builtin_pure("profiler-cpu-log", vec![])
        .expect("builtin profiler-cpu-log should resolve")
        .expect("builtin profiler-cpu-log should evaluate");
    assert!(cpu_log.is_nil());

    let cpu_running = dispatch_builtin_pure("profiler-cpu-running-p", vec![])
        .expect("builtin profiler-cpu-running-p should resolve")
        .expect("builtin profiler-cpu-running-p should evaluate");
    assert!(cpu_running.is_nil());

    let cpu_start = dispatch_builtin_pure("profiler-cpu-start", vec![Value::Int(1)])
        .expect("builtin profiler-cpu-start should resolve")
        .expect("builtin profiler-cpu-start should evaluate");
    assert!(cpu_start.is_nil());

    let cpu_stop = dispatch_builtin_pure("profiler-cpu-stop", vec![])
        .expect("builtin profiler-cpu-stop should resolve")
        .expect("builtin profiler-cpu-stop should evaluate");
    assert!(cpu_stop.is_nil());

    let mem_log = dispatch_builtin_pure("profiler-memory-log", vec![])
        .expect("builtin profiler-memory-log should resolve")
        .expect("builtin profiler-memory-log should evaluate");
    assert!(mem_log.is_nil());

    let mem_running = dispatch_builtin_pure("profiler-memory-running-p", vec![])
        .expect("builtin profiler-memory-running-p should resolve")
        .expect("builtin profiler-memory-running-p should evaluate");
    assert!(mem_running.is_nil());

    let mem_start = dispatch_builtin_pure("profiler-memory-start", vec![])
        .expect("builtin profiler-memory-start should resolve")
        .expect("builtin profiler-memory-start should evaluate");
    assert!(mem_start.is_nil());

    let mem_stop = dispatch_builtin_pure("profiler-memory-stop", vec![])
        .expect("builtin profiler-memory-stop should resolve")
        .expect("builtin profiler-memory-stop should evaluate");
    assert!(mem_stop.is_nil());
}

#[test]
fn pure_dispatch_position_placeholders_match_compat_contracts() {
    let pdumper = dispatch_builtin_pure("pdumper-stats", vec![])
        .expect("builtin pdumper-stats should resolve")
        .expect("builtin pdumper-stats should evaluate");
    assert!(pdumper.is_nil());

    let position_symbol =
        dispatch_builtin_pure("position-symbol", vec![Value::symbol("x"), Value::Nil])
            .expect("builtin position-symbol should resolve")
            .expect("builtin position-symbol should evaluate");
    assert!(position_symbol.is_nil());

    let posn_at_point = dispatch_builtin_pure("posn-at-point", vec![])
        .expect("builtin posn-at-point should resolve")
        .expect("builtin posn-at-point should evaluate");
    assert!(posn_at_point.is_nil());

    let posn_at_xy = dispatch_builtin_pure("posn-at-x-y", vec![Value::Int(0), Value::Int(0)])
        .expect("builtin posn-at-x-y should resolve")
        .expect("builtin posn-at-x-y should evaluate");
    assert!(posn_at_xy.is_nil());

    let play_sound = dispatch_builtin_pure("play-sound-internal", vec![Value::Nil])
        .expect("builtin play-sound-internal should resolve")
        .expect("builtin play-sound-internal should evaluate");
    assert!(play_sound.is_nil());
}

#[test]
fn pure_dispatch_record_query_placeholders_match_compat_contracts() {
    let record = dispatch_builtin_pure("record", vec![Value::symbol("tag"), Value::Int(1)])
        .expect("builtin record should resolve")
        .expect("builtin record should evaluate");
    assert!(record.is_record());

    let record_arity = dispatch_builtin_pure("record", vec![])
        .expect("builtin record should resolve")
        .expect_err("record should reject empty slot lists");
    match record_arity {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(sig.data, vec![Value::symbol("record"), Value::Int(0)]);
        }
        other => panic!("expected signal, got: {other:?}"),
    }

    let recordp = dispatch_builtin_pure("recordp", vec![Value::Nil])
        .expect("builtin recordp should resolve")
        .expect("builtin recordp should evaluate");
    assert!(recordp.is_nil());

    let query_font = dispatch_builtin_pure("query-font", vec![Value::Nil])
        .expect("builtin query-font should resolve")
        .expect("builtin query-font should evaluate");
    assert!(query_font.is_nil());

    let query_fontset = dispatch_builtin_pure("query-fontset", vec![Value::Nil])
        .expect("builtin query-fontset should resolve")
        .expect("builtin query-fontset should evaluate");
    assert!(query_fontset.is_nil());

    let read_pos = dispatch_builtin_pure("read-positioning-symbols", vec![])
        .expect("builtin read-positioning-symbols should resolve")
        .expect("builtin read-positioning-symbols should evaluate");
    assert!(read_pos.is_nil());

    let recent_auto_save = dispatch_builtin_pure("recent-auto-save-p", vec![])
        .expect("builtin recent-auto-save-p should resolve")
        .expect("builtin recent-auto-save-p should evaluate");
    assert!(recent_auto_save.is_nil());
}

#[test]
fn pure_dispatch_reconsider_redirect_placeholders_match_compat_contracts() {
    let reconsider = dispatch_builtin_pure("reconsider-frame-fonts", vec![Value::Nil])
        .expect("builtin reconsider-frame-fonts should resolve")
        .expect_err("reconsider-frame-fonts should require a window system frame");
    match reconsider {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Window system frame should be used")]
            );
        }
        other => panic!("expected signal, got: {other:?}"),
    }

    let redirect_dbg = dispatch_builtin_pure("redirect-debugging-output", vec![Value::Nil])
        .expect("builtin redirect-debugging-output should resolve")
        .expect("builtin redirect-debugging-output should evaluate");
    assert!(redirect_dbg.is_nil());

    let redirect_focus = dispatch_builtin_pure("redirect-frame-focus", vec![Value::Nil])
        .expect("builtin redirect-frame-focus should resolve")
        .expect("builtin redirect-frame-focus should evaluate");
    assert!(redirect_focus.is_nil());

    let remove_pos = dispatch_builtin_pure("remove-pos-from-symbol", vec![Value::symbol("x")])
        .expect("builtin remove-pos-from-symbol should resolve")
        .expect("builtin remove-pos-from-symbol should evaluate");
    assert_eq!(remove_pos, Value::symbol("x"));

    let resize_mini = dispatch_builtin_pure("resize-mini-window-internal", vec![Value::Window(1)])
        .expect("builtin resize-mini-window-internal should resolve")
        .expect_err("resize-mini-window-internal should reject non-minibuffer windows");
    match resize_mini {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Not a valid minibuffer window")]
            );
        }
        other => panic!("expected signal, got: {other:?}"),
    }

    let restore_modified = dispatch_builtin_pure("restore-buffer-modified-p", vec![Value::Nil])
        .expect("builtin restore-buffer-modified-p should resolve")
        .expect("builtin restore-buffer-modified-p should evaluate");
    assert!(restore_modified.is_nil());

    let set_command_keys =
        dispatch_builtin_pure("set--this-command-keys", vec![Value::string("x")])
            .expect("builtin set--this-command-keys should resolve")
            .expect("builtin set--this-command-keys should evaluate");
    assert!(set_command_keys.is_nil());

    let set_auto_saved = dispatch_builtin_pure("set-buffer-auto-saved", vec![])
        .expect("builtin set-buffer-auto-saved should resolve")
        .expect("builtin set-buffer-auto-saved should evaluate");
    assert!(set_auto_saved.is_nil());
}

#[test]
fn pure_dispatch_set_window_placeholder_cluster_matches_compat_contracts() {
    let set_charset = dispatch_builtin_pure(
        "set-charset-plist",
        vec![Value::symbol("unicode"), Value::list(vec![])],
    )
    .expect("builtin set-charset-plist should resolve")
    .expect("builtin set-charset-plist should evaluate");
    assert_eq!(set_charset, Value::list(vec![]));

    let set_fontset = dispatch_builtin_pure(
        "set-fontset-font",
        vec![Value::symbol("name"), Value::symbol("target"), Value::Nil],
    )
    .expect("builtin set-fontset-font should resolve")
    .expect("builtin set-fontset-font should evaluate");
    assert!(set_fontset.is_nil());

    let set_state = dispatch_builtin_pure("set-frame-window-state-change", vec![])
        .expect("builtin set-frame-window-state-change should resolve")
        .expect("builtin set-frame-window-state-change should evaluate");
    assert!(set_state.is_nil());

    let set_fringe = dispatch_builtin_pure(
        "set-fringe-bitmap-face",
        vec![Value::symbol("left-triangle")],
    )
    .expect("builtin set-fringe-bitmap-face should resolve")
    .expect("builtin set-fringe-bitmap-face should evaluate");
    assert!(set_fringe.is_nil());

    let minibuffer_window_id =
        crate::window::MINIBUFFER_WINDOW_ID_BASE + crate::window::FRAME_ID_BASE;
    let set_mini = dispatch_builtin_pure(
        "set-minibuffer-window",
        vec![Value::Window(minibuffer_window_id)],
    )
    .expect("builtin set-minibuffer-window should resolve")
    .expect("builtin set-minibuffer-window should evaluate");
    assert!(set_mini.is_nil());

    let set_mouse_pixel = dispatch_builtin_pure(
        "set-mouse-pixel-position",
        vec![Value::Frame(1), Value::Int(0), Value::Int(0)],
    )
    .expect("builtin set-mouse-pixel-position should resolve")
    .expect("builtin set-mouse-pixel-position should evaluate");
    assert!(set_mouse_pixel.is_nil());

    let set_mouse = dispatch_builtin_pure(
        "set-mouse-position",
        vec![Value::Frame(1), Value::Int(0), Value::Int(0)],
    )
    .expect("builtin set-mouse-position should resolve")
    .expect("builtin set-mouse-position should evaluate");
    assert!(set_mouse.is_nil());

    let set_combination = dispatch_builtin_pure(
        "set-window-combination-limit",
        vec![Value::Window(1), Value::Nil],
    )
    .expect("builtin set-window-combination-limit should resolve")
    .expect_err("set-window-combination-limit should reject leaf windows");
    match set_combination {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string(
                    "Combination limit is meaningful for internal windows only",
                )]
            );
        }
        other => panic!("expected signal, got: {other:?}"),
    }

    let set_new_normal = dispatch_builtin_pure("set-window-new-normal", vec![Value::Nil])
        .expect("builtin set-window-new-normal should resolve")
        .expect("builtin set-window-new-normal should evaluate");
    assert!(set_new_normal.is_nil());

    let set_new_pixel = dispatch_builtin_pure(
        "set-window-new-pixel",
        vec![Value::Nil, Value::Int(1), Value::Int(2)],
    )
    .expect("builtin set-window-new-pixel should resolve")
    .expect("builtin set-window-new-pixel should evaluate");
    assert_eq!(set_new_pixel, Value::Int(1));

    let set_new_total = dispatch_builtin_pure(
        "set-window-new-total",
        vec![Value::Nil, Value::Int(1), Value::Int(2)],
    )
    .expect("builtin set-window-new-total should resolve")
    .expect("builtin set-window-new-total should evaluate");
    assert_eq!(set_new_total, Value::Int(1));
}

#[test]
fn pure_dispatch_sort_subr_placeholder_cluster_matches_compat_contracts() {
    let sort_charsets = dispatch_builtin_pure("sort-charsets", vec![Value::list(vec![])])
        .expect("builtin sort-charsets should resolve")
        .expect("builtin sort-charsets should evaluate");
    assert!(sort_charsets.is_nil());

    let split_char = dispatch_builtin_pure("split-char", vec![Value::Int(65)])
        .expect("builtin split-char should resolve")
        .expect("builtin split-char should evaluate");
    assert!(split_char.is_nil());

    let string_distance = dispatch_builtin_pure(
        "string-distance",
        vec![Value::string("a"), Value::string("b")],
    )
    .expect("builtin string-distance should resolve")
    .expect("builtin string-distance should evaluate");
    assert_eq!(string_distance, Value::Int(1));

    let subst = dispatch_builtin_pure(
        "subst-char-in-region",
        vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(97),
            Value::Int(98),
            Value::Nil,
        ],
    )
    .expect("builtin subst-char-in-region should resolve")
    .expect("builtin subst-char-in-region should evaluate");
    assert!(subst.is_nil());

    let subr_unit = dispatch_builtin_pure("subr-native-comp-unit", vec![Value::Nil])
        .expect("builtin subr-native-comp-unit should resolve")
        .expect("builtin subr-native-comp-unit should evaluate");
    assert!(subr_unit.is_nil());

    let subr_lambda_list = dispatch_builtin_pure("subr-native-lambda-list", vec![Value::Nil])
        .expect("builtin subr-native-lambda-list should resolve")
        .expect("builtin subr-native-lambda-list should evaluate");
    assert!(subr_lambda_list.is_nil());

    let subr_type = dispatch_builtin_pure("subr-type", vec![Value::Nil])
        .expect("builtin subr-type should resolve")
        .expect("builtin subr-type should evaluate");
    assert!(subr_type.is_nil());

    let single_keys = dispatch_builtin_pure("this-single-command-keys", vec![])
        .expect("builtin this-single-command-keys should resolve")
        .expect("builtin this-single-command-keys should evaluate");
    assert!(single_keys.is_nil());

    let single_raw_keys = dispatch_builtin_pure("this-single-command-raw-keys", vec![])
        .expect("builtin this-single-command-raw-keys should resolve")
        .expect("builtin this-single-command-raw-keys should evaluate");
    assert!(single_raw_keys.is_nil());
}

#[test]
fn pure_dispatch_tty_tool_bar_placeholder_cluster_matches_compat_contracts() {
    let thread_blocker = dispatch_builtin_pure("thread--blocker", vec![Value::Nil])
        .expect("builtin thread--blocker should resolve")
        .expect("builtin thread--blocker should evaluate");
    assert!(thread_blocker.is_nil());

    let tool_bar_style = dispatch_builtin_pure("tool-bar-get-system-style", vec![])
        .expect("builtin tool-bar-get-system-style should resolve")
        .expect("builtin tool-bar-get-system-style should evaluate");
    assert!(tool_bar_style.is_nil());

    let tool_bar_width = dispatch_builtin_pure("tool-bar-pixel-width", vec![])
        .expect("builtin tool-bar-pixel-width should resolve")
        .expect("builtin tool-bar-pixel-width should evaluate");
    assert_eq!(tool_bar_width, Value::Int(0));

    let translate = dispatch_builtin_pure(
        "translate-region-internal",
        vec![Value::Int(1), Value::Int(2), Value::Nil],
    )
    .expect("builtin translate-region-internal should resolve")
    .expect("builtin translate-region-internal should evaluate");
    assert!(translate.is_nil());

    let transpose = dispatch_builtin_pure(
        "transpose-regions",
        vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Nil,
        ],
    )
    .expect("builtin transpose-regions should resolve")
    .expect("builtin transpose-regions should evaluate");
    assert!(transpose.is_nil());

    let tty_buf = dispatch_builtin_pure("tty--output-buffer-size", vec![])
        .expect("builtin tty--output-buffer-size should resolve")
        .expect("builtin tty--output-buffer-size should evaluate");
    assert_eq!(tty_buf, Value::Int(0));

    let tty_set = dispatch_builtin_pure("tty--set-output-buffer-size", vec![Value::Int(4096)])
        .expect("builtin tty--set-output-buffer-size should resolve")
        .expect("builtin tty--set-output-buffer-size should evaluate");
    assert!(tty_set.is_nil());

    let tty_suppress =
        dispatch_builtin_pure("tty-suppress-bold-inverse-default-colors", vec![Value::Nil])
            .expect("builtin tty-suppress-bold-inverse-default-colors should resolve")
            .expect("builtin tty-suppress-bold-inverse-default-colors should evaluate");
    assert!(tty_suppress.is_nil());
}

#[test]
fn pure_dispatch_unicode_value_placeholder_cluster_matches_compat_contracts() {
    let unencodable = dispatch_builtin_pure(
        "unencodable-char-position",
        vec![Value::Int(1), Value::Int(2), Value::symbol("utf-8")],
    )
    .expect("builtin unencodable-char-position should resolve")
    .expect("builtin unencodable-char-position should evaluate");
    assert!(unencodable.is_nil());

    let unicode_table = dispatch_builtin_pure(
        "unicode-property-table-internal",
        vec![Value::symbol("foo")],
    )
    .expect("builtin unicode-property-table-internal should resolve")
    .expect("builtin unicode-property-table-internal should evaluate");
    assert!(unicode_table.is_nil());

    let unify = dispatch_builtin_pure(
        "unify-charset",
        vec![Value::symbol("from"), Value::symbol("to"), Value::Nil],
    )
    .expect("builtin unify-charset should resolve")
    .expect("builtin unify-charset should evaluate");
    assert!(unify.is_nil());

    let unix_sync = dispatch_builtin_pure("unix-sync", vec![])
        .expect("builtin unix-sync should resolve")
        .expect("builtin unix-sync should evaluate");
    assert!(unix_sync.is_nil());

    let value_lt = dispatch_builtin_pure("value<", vec![Value::Int(1), Value::Int(2)])
        .expect("builtin value< should resolve")
        .expect("builtin value< should evaluate");
    assert!(value_lt.is_truthy());

    let binding_locus = dispatch_builtin_pure("variable-binding-locus", vec![Value::symbol("x")])
        .expect("builtin variable-binding-locus should resolve")
        .expect("builtin variable-binding-locus should evaluate");
    assert!(binding_locus.is_nil());
}

#[test]
fn pure_dispatch_x_display_placeholder_cluster_matches_compat_contracts() {
    let x_begin_drag = dispatch_builtin_pure("x-begin-drag", vec![Value::Nil])
        .expect("builtin x-begin-drag should resolve")
        .expect("builtin x-begin-drag should evaluate");
    assert!(x_begin_drag.is_nil());

    let x_create_frame = dispatch_builtin_pure("x-create-frame", vec![Value::list(vec![])])
        .expect("builtin x-create-frame should resolve")
        .expect("builtin x-create-frame should evaluate");
    assert!(x_create_frame.is_nil());

    let x_double_buffered = dispatch_builtin_pure("x-double-buffered-p", vec![])
        .expect("builtin x-double-buffered-p should resolve")
        .expect("builtin x-double-buffered-p should evaluate");
    assert!(x_double_buffered.is_nil());

    let x_menu_open = dispatch_builtin_pure("x-menu-bar-open-internal", vec![])
        .expect("builtin x-menu-bar-open-internal should resolve")
        .expect("builtin x-menu-bar-open-internal should evaluate");
    assert!(x_menu_open.is_nil());

    let xw_defined = dispatch_builtin_pure(
        "xw-color-defined-p",
        vec![Value::string("black"), Value::Nil],
    )
    .expect("builtin xw-color-defined-p should resolve")
    .expect("builtin xw-color-defined-p should evaluate");
    assert!(xw_defined.is_nil());

    let xw_values = dispatch_builtin_pure("xw-color-values", vec![Value::string("black")])
        .expect("builtin xw-color-values should resolve")
        .expect("builtin xw-color-values should evaluate");
    assert!(xw_values.is_nil());

    let xw_display_color = dispatch_builtin_pure("xw-display-color-p", vec![])
        .expect("builtin xw-display-color-p should resolve")
        .expect("builtin xw-display-color-p should evaluate");
    assert!(xw_display_color.is_nil());
}

#[test]
fn pure_dispatch_minibuffer_lock_placeholder_cluster_matches_compat_contracts() {
    let innermost = dispatch_builtin_pure("innermost-minibuffer-p", vec![])
        .expect("builtin innermost-minibuffer-p should resolve")
        .expect("builtin innermost-minibuffer-p should evaluate");
    assert!(innermost.is_nil());

    let interactive_ignore =
        dispatch_builtin_pure("interactive-form", vec![Value::symbol("ignore")])
            .expect("builtin interactive-form should resolve")
            .expect("builtin interactive-form should evaluate");
    assert_eq!(
        interactive_ignore,
        Value::list(vec![Value::symbol("interactive"), Value::Nil])
    );

    let local_if_set = dispatch_builtin_pure("local-variable-if-set-p", vec![Value::symbol("x")])
        .expect("builtin local-variable-if-set-p should resolve")
        .expect("builtin local-variable-if-set-p should evaluate");
    assert!(local_if_set.is_nil());

    let lock_buffer = dispatch_builtin_pure("lock-buffer", vec![])
        .expect("builtin lock-buffer should resolve")
        .expect("builtin lock-buffer should evaluate");
    assert!(lock_buffer.is_nil());

    let lock_file = dispatch_builtin_pure("lock-file", vec![Value::string("/tmp/x")])
        .expect("builtin lock-file should resolve")
        .expect("builtin lock-file should evaluate");
    assert!(lock_file.is_nil());

    let lossage_size = dispatch_builtin_pure("lossage-size", vec![])
        .expect("builtin lossage-size should resolve")
        .expect("builtin lossage-size should evaluate");
    assert_eq!(lossage_size, Value::Int(300));

    let unlock_buffer = dispatch_builtin_pure("unlock-buffer", vec![])
        .expect("builtin unlock-buffer should resolve")
        .expect("builtin unlock-buffer should evaluate");
    assert!(unlock_buffer.is_nil());

    let unlock_file = dispatch_builtin_pure("unlock-file", vec![Value::string("/tmp/x")])
        .expect("builtin unlock-file should resolve")
        .expect("builtin unlock-file should evaluate");
    assert!(unlock_file.is_nil());
}

#[test]
fn interactive_form_eval_resolves_symbol_lambda_and_alias() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let lambda = Value::list(vec![
        Value::symbol("lambda"),
        Value::Nil,
        Value::list(vec![Value::symbol("interactive"), Value::string("p")]),
        Value::Int(1),
    ]);
    eval.obarray_mut()
        .set_symbol_function("vm-interactive-form-lambda", lambda);
    eval.obarray_mut().set_symbol_function(
        "vm-interactive-form-alias",
        Value::symbol("vm-interactive-form-lambda"),
    );

    let expected = Value::list(vec![Value::symbol("interactive"), Value::string("p")]);
    assert_eq!(
        builtin_interactive_form_eval(&mut eval, vec![Value::symbol("vm-interactive-form-lambda")])
            .expect("interactive-form should read lambda interactive spec"),
        expected
    );
    assert_eq!(
        builtin_interactive_form_eval(&mut eval, vec![Value::symbol("vm-interactive-form-alias")])
            .expect("interactive-form should follow function aliases"),
        expected
    );
    assert_eq!(
        builtin_interactive_form_eval(&mut eval, vec![lambda])
            .expect("interactive-form should parse quoted lambda designators"),
        expected
    );
}

#[test]
fn interactive_form_eval_skips_docstring_before_interactive_spec() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let lambda_with_doc = Value::list(vec![
        Value::symbol("lambda"),
        Value::Nil,
        Value::string("doc"),
        Value::list(vec![Value::symbol("interactive"), Value::string("P")]),
        Value::Int(1),
    ]);
    eval.obarray_mut()
        .set_symbol_function("vm-interactive-form-doc", lambda_with_doc);

    assert_eq!(
        builtin_interactive_form_eval(&mut eval, vec![Value::symbol("vm-interactive-form-doc")])
            .expect("interactive-form should inspect lambda body after docstring"),
        Value::list(vec![Value::symbol("interactive"), Value::string("P")])
    );
}

#[test]
fn interactive_form_eval_returns_nil_for_non_interactive_lambda() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let lambda = Value::list(vec![Value::symbol("lambda"), Value::Nil, Value::Int(1)]);
    eval.obarray_mut()
        .set_symbol_function("vm-interactive-form-plain", lambda);

    assert!(
        builtin_interactive_form_eval(&mut eval, vec![Value::symbol("vm-interactive-form-plain")])
            .expect("interactive-form should evaluate")
            .is_nil()
    );
    assert!(
        builtin_interactive_form_eval(&mut eval, vec![lambda])
            .expect("interactive-form should evaluate")
            .is_nil()
    );
    assert!(
        builtin_interactive_form_eval(&mut eval, vec![Value::Int(0)])
            .expect("interactive-form should evaluate")
            .is_nil()
    );
}

#[test]
fn interactive_form_eval_preserves_noarg_and_explicit_nil_shapes() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let noarg_lambda = Value::list(vec![
        Value::symbol("lambda"),
        Value::Nil,
        Value::list(vec![Value::symbol("interactive")]),
        Value::Int(1),
    ]);
    let nil_lambda = Value::list(vec![
        Value::symbol("lambda"),
        Value::Nil,
        Value::list(vec![Value::symbol("interactive"), Value::Nil]),
        Value::Int(1),
    ]);
    eval.obarray_mut()
        .set_symbol_function("vm-interactive-form-noarg", noarg_lambda);
    eval.obarray_mut()
        .set_symbol_function("vm-interactive-form-nil", nil_lambda);

    assert_eq!(
        builtin_interactive_form_eval(&mut eval, vec![noarg_lambda])
            .expect("interactive-form should evaluate"),
        Value::list(vec![Value::symbol("interactive"), Value::Nil])
    );
    assert_eq!(
        builtin_interactive_form_eval(&mut eval, vec![Value::symbol("vm-interactive-form-noarg")])
            .expect("interactive-form should evaluate"),
        Value::list(vec![Value::symbol("interactive"), Value::Nil])
    );
    assert_eq!(
        builtin_interactive_form_eval(&mut eval, vec![nil_lambda])
            .expect("interactive-form should evaluate"),
        Value::list(vec![Value::symbol("interactive"), Value::Nil])
    );
    assert_eq!(
        builtin_interactive_form_eval(&mut eval, vec![Value::symbol("vm-interactive-form-nil")])
            .expect("interactive-form should evaluate"),
        Value::list(vec![Value::symbol("interactive"), Value::Nil])
    );
}

#[test]
fn interactive_form_eval_signals_listp_for_improper_lambda_shapes() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let dotted_interactive = Value::list(vec![
        Value::symbol("lambda"),
        Value::Nil,
        Value::cons(Value::symbol("interactive"), Value::string("p")),
        Value::Int(1),
    ]);
    let dotted_body = Value::cons(
        Value::symbol("lambda"),
        Value::cons(Value::Nil, Value::cons(Value::Int(1), Value::Int(2))),
    );
    let doc_dotted_body = Value::cons(
        Value::symbol("lambda"),
        Value::cons(Value::Nil, Value::cons(Value::string("doc"), Value::Int(2))),
    );
    let doc_interactive_dotted_tail = Value::cons(
        Value::symbol("lambda"),
        Value::cons(
            Value::Nil,
            Value::cons(
                Value::string("doc"),
                Value::cons(
                    Value::list(vec![Value::symbol("interactive")]),
                    Value::Int(2),
                ),
            ),
        ),
    );

    let dotted_interactive_err =
        builtin_interactive_form_eval(&mut eval, vec![dotted_interactive]).unwrap_err();
    match dotted_interactive_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("listp"), Value::string("p")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let dotted_body_err = builtin_interactive_form_eval(&mut eval, vec![dotted_body]).unwrap_err();
    match dotted_body_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![
                    Value::symbol("listp"),
                    Value::cons(Value::Int(1), Value::Int(2))
                ]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let doc_dotted_body_err =
        builtin_interactive_form_eval(&mut eval, vec![doc_dotted_body]).unwrap_err();
    match doc_dotted_body_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![
                    Value::symbol("listp"),
                    Value::cons(Value::string("doc"), Value::Int(2))
                ]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    assert_eq!(
        builtin_interactive_form_eval(&mut eval, vec![doc_interactive_dotted_tail])
            .expect("interactive-form should stop at first interactive form"),
        Value::list(vec![Value::symbol("interactive"), Value::Nil])
    );
}

#[test]
fn pure_dispatch_internal_placeholder_cluster_matches_compat_contracts() {
    let track_mouse = dispatch_builtin_pure("internal--track-mouse", vec![Value::Nil])
        .expect("builtin internal--track-mouse should resolve")
        .expect("builtin internal--track-mouse should evaluate");
    assert!(track_mouse.is_nil());

    let char_font = dispatch_builtin_pure("internal-char-font", vec![Value::Int(65)])
        .expect("builtin internal-char-font should resolve")
        .expect("builtin internal-char-font should evaluate");
    assert!(char_font.is_nil());

    let char_font_with_nil_position =
        dispatch_builtin_pure("internal-char-font", vec![Value::Nil, Value::Int(65)])
            .expect("builtin internal-char-font should resolve nil-position probe")
            .expect("builtin internal-char-font should accept nil position with char probe");
    assert!(char_font_with_nil_position.is_nil());

    let complete_buffer = dispatch_builtin_pure(
        "internal-complete-buffer",
        vec![Value::string("a"), Value::Int(1), Value::Int(2)],
    )
    .expect("builtin internal-complete-buffer should resolve")
    .expect("builtin internal-complete-buffer should evaluate");
    assert!(complete_buffer.is_nil());

    let describe_syntax =
        dispatch_builtin_pure("internal-describe-syntax-value", vec![Value::Int(0)])
            .expect("builtin internal-describe-syntax-value should resolve")
            .expect("builtin internal-describe-syntax-value should evaluate");
    assert_eq!(describe_syntax, Value::Int(0));

    let parse_modifiers = dispatch_builtin_pure(
        "internal-event-symbol-parse-modifiers",
        vec![Value::symbol("C-x")],
    )
    .expect("builtin internal-event-symbol-parse-modifiers should resolve")
    .expect("builtin internal-event-symbol-parse-modifiers should evaluate");
    assert_eq!(
        parse_modifiers,
        Value::list(vec![Value::symbol("x"), Value::symbol("control")])
    );

    let handle_focus_in = dispatch_builtin_pure("internal-handle-focus-in", vec![Value::Nil])
        .expect("builtin internal-handle-focus-in should resolve")
        .expect_err("builtin internal-handle-focus-in should signal on invalid events");
    match handle_focus_in {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data, vec![Value::string("invalid focus-in event")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let set_face_attr = dispatch_builtin_pure(
        "internal-set-lisp-face-attribute-from-resource",
        vec![
            Value::symbol("face"),
            Value::keyword(":height"),
            Value::string("value"),
        ],
    )
    .expect("builtin internal-set-lisp-face-attribute-from-resource should resolve")
    .expect("builtin internal-set-lisp-face-attribute-from-resource should evaluate");
    assert_eq!(set_face_attr, Value::symbol("face"));

    let stack_stats = dispatch_builtin_pure("internal-stack-stats", vec![])
        .expect("builtin internal-stack-stats should resolve")
        .expect("builtin internal-stack-stats should evaluate");
    assert!(stack_stats.is_nil());

    let subr_doc = dispatch_builtin_pure("internal-subr-documentation", vec![Value::Nil])
        .expect("builtin internal-subr-documentation should resolve")
        .expect("builtin internal-subr-documentation should evaluate");
    assert_eq!(subr_doc, Value::True);
}

#[test]
fn internal_make_var_non_special_clears_special_flag() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    eval.obarray_mut().make_special("x");
    assert!(eval.obarray().is_special("x"));

    let result = dispatch_builtin(
        &mut eval,
        "internal-make-var-non-special",
        vec![Value::symbol("x")],
    )
    .expect("builtin internal-make-var-non-special should resolve")
    .expect("builtin internal-make-var-non-special should evaluate");

    assert!(result.is_nil());
    assert!(!eval.obarray().is_special("x"));
}

#[test]
fn pure_dispatch_memory_module_placeholder_cluster_matches_compat_contracts() {
    let malloc_info = dispatch_builtin_pure("malloc-info", vec![])
        .expect("builtin malloc-info should resolve")
        .expect("builtin malloc-info should evaluate");
    assert!(malloc_info.is_nil());

    let malloc_trim = dispatch_builtin_pure("malloc-trim", vec![])
        .expect("builtin malloc-trim should resolve")
        .expect("builtin malloc-trim should evaluate");
    assert_eq!(malloc_trim, Value::True);

    let malloc_trim_nil = dispatch_builtin_pure("malloc-trim", vec![Value::Nil])
        .expect("builtin malloc-trim should resolve with nil pad")
        .expect("builtin malloc-trim should evaluate with nil pad");
    assert_eq!(malloc_trim_nil, Value::True);

    let malloc_trim_zero = dispatch_builtin_pure("malloc-trim", vec![Value::Int(0)])
        .expect("builtin malloc-trim should resolve with integer pad")
        .expect("builtin malloc-trim should evaluate with integer pad");
    assert_eq!(malloc_trim_zero, Value::True);

    for bad in [
        Value::Int(-1),
        Value::True,
        Value::vector(vec![Value::Int(1)]),
    ] {
        let err = dispatch_builtin_pure("malloc-trim", vec![bad])
            .expect("builtin malloc-trim should resolve for bad pad")
            .expect_err("malloc-trim should reject non-wholenump pad");
        match err {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(sig.data, vec![Value::symbol("wholenump"), bad]);
            }
            other => panic!("expected signal, got: {other:?}"),
        }
    }

    let memory_info = dispatch_builtin_pure("memory-info", vec![])
        .expect("builtin memory-info should resolve")
        .expect("builtin memory-info should evaluate");
    let items = list_to_vec(&memory_info).expect("memory-info should return list");
    assert_eq!(items.len(), 4);
    assert!(items.iter().all(|item| matches!(item, Value::Int(_))));

    let module_path = "__neovm_missing_module__.so";
    let module_load_err = dispatch_builtin_pure("module-load", vec![Value::string(module_path)])
        .expect("builtin module-load should resolve")
        .expect_err("builtin module-load should signal on missing path");
    match module_load_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "module-open-failed");
            assert_eq!(sig.data.first(), Some(&Value::string(module_path)));
            assert!(
                matches!(sig.data.get(1), Some(Value::Str(_))),
                "module-open-failed should include string error message payload"
            );
        }
        other => panic!("expected signal, got: {other:?}"),
    }

    let module_load_type_err = dispatch_builtin_pure("module-load", vec![Value::Nil])
        .expect("builtin module-load should resolve")
        .expect_err("module-load should reject non-string path");
    match module_load_type_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::Nil]);
        }
        other => panic!("expected signal, got: {other:?}"),
    }
}

#[test]
fn pure_dispatch_dump_portable_placeholder_cluster_matches_compat_contracts() {
    let dump_portable = dispatch_builtin_pure(
        "dump-emacs-portable",
        vec![Value::string("dump.pdmp"), Value::Nil],
    )
    .expect("builtin dump-emacs-portable should resolve")
    .expect("builtin dump-emacs-portable should evaluate");
    assert!(dump_portable.is_nil());

    let sort_predicate = dispatch_builtin_pure(
        "dump-emacs-portable--sort-predicate",
        vec![Value::Nil, Value::Nil],
    )
    .expect("builtin dump-emacs-portable--sort-predicate should resolve")
    .expect("builtin dump-emacs-portable--sort-predicate should evaluate");
    assert!(sort_predicate.is_nil());

    let sort_predicate_copied = dispatch_builtin_pure(
        "dump-emacs-portable--sort-predicate-copied",
        vec![Value::Nil, Value::Nil],
    )
    .expect("builtin dump-emacs-portable--sort-predicate-copied should resolve")
    .expect("builtin dump-emacs-portable--sort-predicate-copied should evaluate");
    assert!(sort_predicate_copied.is_nil());
}

#[test]
fn pure_dispatch_coding_placeholder_cluster_matches_compat_contracts() {
    let byte_code = dispatch_builtin_pure(
        "byte-code",
        vec![Value::string(""), Value::vector(vec![]), Value::Int(0)],
    )
    .expect("builtin byte-code should resolve")
    .expect("builtin byte-code should evaluate");
    assert!(byte_code.is_nil());

    let decode_region = dispatch_builtin_pure(
        "decode-coding-region",
        vec![
            Value::Int(1),
            Value::Int(2),
            Value::symbol("utf-8"),
            Value::Nil,
        ],
    )
    .expect("builtin decode-coding-region should resolve")
    .expect("builtin decode-coding-region should evaluate");
    assert!(decode_region.is_nil());

    let encode_region = dispatch_builtin_pure(
        "encode-coding-region",
        vec![
            Value::Int(1),
            Value::Int(2),
            Value::symbol("utf-8"),
            Value::Nil,
        ],
    )
    .expect("builtin encode-coding-region should resolve")
    .expect("builtin encode-coding-region should evaluate");
    assert!(encode_region.is_nil());

    let find_operation =
        dispatch_builtin_pure("find-operation-coding-system", vec![Value::symbol("write")])
            .expect("builtin find-operation-coding-system should resolve")
            .expect("builtin find-operation-coding-system should evaluate");
    assert!(find_operation.is_nil());

    let handler_bind = dispatch_builtin_pure(
        "handler-bind-1",
        vec![Value::list(vec![]), Value::symbol("body")],
    )
    .expect("builtin handler-bind-1 should resolve")
    .expect("builtin handler-bind-1 should evaluate");
    assert!(handler_bind.is_nil());
}

#[test]
fn pure_dispatch_def_keymap_placeholder_cluster_matches_compat_contracts() {
    assert!(
        dispatch_builtin_pure(
            "defconst-1",
            vec![Value::symbol("foo"), Value::Int(1), Value::string("doc")],
        )
        .is_none()
    );

    assert!(dispatch_builtin_pure("defvar-1", vec![Value::symbol("foo"), Value::Int(1)]).is_none());

    let iso_charset = dispatch_builtin_pure(
        "iso-charset",
        vec![Value::Int(1), Value::Int(2), Value::Int(3)],
    )
    .expect("builtin iso-charset should resolve")
    .expect("builtin iso-charset should evaluate");
    assert!(iso_charset.is_nil());

    let keyelt = dispatch_builtin_pure("keymap--get-keyelt", vec![Value::Nil, Value::Nil])
        .expect("builtin keymap--get-keyelt should resolve")
        .expect("builtin keymap--get-keyelt should evaluate");
    assert!(keyelt.is_nil());

    let keyelt_true = dispatch_builtin_pure("keymap--get-keyelt", vec![Value::True, Value::Nil])
        .expect("builtin keymap--get-keyelt should resolve")
        .expect("builtin keymap--get-keyelt should evaluate");
    assert!(keyelt_true.is_truthy());

    let keymap_prompt = dispatch_builtin_pure("keymap-prompt", vec![Value::Nil])
        .expect("builtin keymap-prompt should resolve")
        .expect("builtin keymap-prompt should evaluate");
    assert!(keymap_prompt.is_nil());
}

#[test]
fn defvar_1_binds_only_when_default_is_unbound() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let result = builtin_defvar_1_eval(
        &mut eval,
        vec![
            Value::symbol("vm-defvar-1"),
            Value::Int(7),
            Value::string("doc"),
        ],
    )
    .expect("defvar-1 should succeed");
    assert_eq!(result, Value::symbol("vm-defvar-1"));
    assert_eq!(
        eval.obarray()
            .symbol_value_id(intern("vm-defvar-1"))
            .copied(),
        Some(Value::Int(7))
    );

    let result =
        builtin_defvar_1_eval(&mut eval, vec![Value::symbol("vm-defvar-1"), Value::Int(9)])
            .expect("second defvar-1 should succeed");
    assert_eq!(result, Value::symbol("vm-defvar-1"));
    assert_eq!(
        eval.obarray()
            .symbol_value_id(intern("vm-defvar-1"))
            .copied(),
        Some(Value::Int(7))
    );
}

#[test]
fn defconst_1_sets_constant_value_and_risky_local_property() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let result = builtin_defconst_1_eval(
        &mut eval,
        vec![
            Value::symbol("vm-defconst-1"),
            Value::Int(11),
            Value::string("doc"),
        ],
    )
    .expect("defconst-1 should succeed");

    assert_eq!(result, Value::symbol("vm-defconst-1"));
    let symbol = intern("vm-defconst-1");
    assert_eq!(
        eval.obarray().symbol_value_id(symbol).copied(),
        Some(Value::Int(11))
    );
    assert!(eval.obarray().is_constant_id(symbol));
    assert_eq!(
        eval.obarray()
            .get_property_id(symbol, intern("risky-local-variable"))
            .copied(),
        Some(Value::True)
    );
}

#[test]
fn pure_dispatch_define_coding_system_internal_not_in_pure_path() {
    // define-coding-system-internal is now dispatched via the eval-aware
    // path (it needs &mut CodingSystemManager from the Evaluator).
    // The pure dispatch returns None for it.
    let result = dispatch_builtin_pure("define-coding-system-internal", vec![Value::Nil; 13]);
    assert!(result.is_none(), "should not resolve in pure dispatch");
}

#[test]
fn pure_dispatch_process_placeholder_cluster_matches_compat_contracts() {
    let kill_emacs = dispatch_builtin_pure("kill-emacs", vec![])
        .expect("builtin kill-emacs should resolve")
        .expect("builtin kill-emacs should evaluate");
    assert!(kill_emacs.is_nil());

    let lower_frame = dispatch_builtin_pure("lower-frame", vec![])
        .expect("builtin lower-frame should resolve")
        .expect("builtin lower-frame should evaluate");
    assert!(lower_frame.is_nil());

    let lread_substitute = dispatch_builtin_pure(
        "lread--substitute-object-in-subtree",
        vec![Value::Int(1), Value::Int(2), Value::Int(3)],
    )
    .expect("builtin lread--substitute-object-in-subtree should resolve")
    .expect("builtin lread--substitute-object-in-subtree should evaluate");
    assert!(lread_substitute.is_nil());
}

#[test]
fn pure_dispatch_make_placeholder_cluster_matches_compat_contracts() {
    let make_byte_code = dispatch_builtin_pure(
        "make-byte-code",
        vec![
            Value::Int(0),
            Value::string(""),
            Value::vector(vec![]),
            Value::Int(0),
        ],
    )
    .expect("builtin make-byte-code should resolve")
    .expect("builtin make-byte-code should evaluate");
    assert!(
        matches!(make_byte_code, Value::ByteCode(_)),
        "make-byte-code should return a ByteCode value, got {:?}",
        make_byte_code
    );

    let hash_literal = Value::list(vec![
        Value::symbol("make-hash-table-from-literal"),
        Value::list(vec![
            Value::symbol("quote"),
            Value::list(vec![
                Value::symbol("hash-table"),
                Value::symbol("test"),
                Value::symbol("eq"),
                Value::symbol("data"),
                Value::list(vec![Value::symbol("foo"), Value::Int(42)]),
            ]),
        ]),
    ]);
    let make_byte_code_with_hash = dispatch_builtin_pure(
        "make-byte-code",
        vec![
            Value::Int(0),
            Value::string("\u{00C0}\u{0087}"),
            Value::vector(vec![hash_literal]),
            Value::Int(1),
        ],
    )
    .expect("builtin make-byte-code with hash literal should resolve")
    .expect("builtin make-byte-code with hash literal should evaluate");
    let bc = make_byte_code_with_hash
        .get_bytecode_data()
        .expect("make-byte-code should produce bytecode data");
    let Value::HashTable(table_id) = bc.constants[0] else {
        panic!("expected hash-table constant, got {:?}", bc.constants[0]);
    };
    let entry = with_heap(|heap| {
        let table = heap.get_hash_table(table_id);
        let key = Value::symbol("foo").to_hash_key(&table.test);
        table.data.get(&key).copied()
    });
    assert_eq!(entry, Some(Value::Int(42)));

    let make_char = dispatch_builtin_pure("make-char", vec![Value::Int(1)])
        .expect("builtin make-char should resolve")
        .expect("builtin make-char should evaluate");
    assert!(make_char.is_nil());

    // make-closure requires a bytecode prototype; nil signals wrong-type-argument
    let make_closure_result = dispatch_builtin_pure("make-closure", vec![Value::Nil])
        .expect("builtin make-closure should resolve");
    assert!(
        make_closure_result.is_err(),
        "make-closure with nil should signal error"
    );

    let make_finalizer = dispatch_builtin_pure("make-finalizer", vec![Value::symbol("ignore")])
        .expect("builtin make-finalizer should resolve")
        .expect("builtin make-finalizer should evaluate");
    assert!(make_finalizer.is_nil());

    assert!(
        dispatch_builtin_pure(
            "make-indirect-buffer",
            vec![Value::symbol("buf"), Value::string("name")],
        )
        .is_none(),
        "make-indirect-buffer should dispatch via eval-aware path"
    );

    let make_interpreted = dispatch_builtin_pure(
        "make-interpreted-closure",
        vec![Value::list(vec![]), Value::list(vec![]), Value::Nil],
    )
    .expect("builtin make-interpreted-closure should resolve")
    .expect("builtin make-interpreted-closure should evaluate");
    // make-interpreted-closure now returns a Lambda value (not nil)
    assert!(matches!(make_interpreted, Value::Lambda(_)));
}

#[test]
fn pure_dispatch_treesit_placeholder_cluster_matches_compat_contracts() {
    let available = dispatch_builtin_pure("treesit-available-p", vec![])
        .expect("builtin treesit-available-p should resolve")
        .expect("builtin treesit-available-p should evaluate");
    assert!(available.is_nil());

    let compiled_query = dispatch_builtin_pure("treesit-compiled-query-p", vec![Value::Nil])
        .expect("builtin treesit-compiled-query-p should resolve")
        .expect("builtin treesit-compiled-query-p should evaluate");
    assert!(compiled_query.is_nil());

    let induce_sparse =
        dispatch_builtin_pure("treesit-induce-sparse-tree", vec![Value::Nil, Value::Nil])
            .expect("builtin treesit-induce-sparse-tree should resolve")
            .expect("builtin treesit-induce-sparse-tree should evaluate");
    assert!(induce_sparse.is_nil());

    let language_abi = dispatch_builtin_pure("treesit-language-abi-version", vec![])
        .expect("builtin treesit-language-abi-version should resolve")
        .expect("builtin treesit-language-abi-version should evaluate");
    assert!(language_abi.is_nil());

    let language_available = dispatch_builtin_pure(
        "treesit-language-available-p",
        vec![Value::symbol("rust"), Value::Nil],
    )
    .expect("builtin treesit-language-available-p should resolve")
    .expect("builtin treesit-language-available-p should evaluate");
    assert!(language_available.is_nil());

    let library_abi = dispatch_builtin_pure("treesit-library-abi-version", vec![])
        .expect("builtin treesit-library-abi-version should resolve")
        .expect("builtin treesit-library-abi-version should evaluate");
    assert!(library_abi.is_nil());

    let node_check = dispatch_builtin_pure("treesit-node-check", vec![Value::Nil, Value::Nil])
        .expect("builtin treesit-node-check should resolve")
        .expect("builtin treesit-node-check should evaluate");
    assert!(node_check.is_nil());

    let node_child = dispatch_builtin_pure("treesit-node-child", vec![Value::Nil, Value::Int(0)])
        .expect("builtin treesit-node-child should resolve")
        .expect("builtin treesit-node-child should evaluate");
    assert!(node_child.is_nil());

    let node_child_by_field = dispatch_builtin_pure(
        "treesit-node-child-by-field-name",
        vec![Value::Nil, Value::string("name")],
    )
    .expect("builtin treesit-node-child-by-field-name should resolve")
    .expect("builtin treesit-node-child-by-field-name should evaluate");
    assert!(node_child_by_field.is_nil());

    let node_child_count =
        dispatch_builtin_pure("treesit-node-child-count", vec![Value::Nil, Value::Nil])
            .expect("builtin treesit-node-child-count should resolve")
            .expect("builtin treesit-node-child-count should evaluate");
    assert!(node_child_count.is_nil());

    let node_descendant = dispatch_builtin_pure(
        "treesit-node-descendant-for-range",
        vec![Value::Nil, Value::Int(0), Value::Int(1), Value::Nil],
    )
    .expect("builtin treesit-node-descendant-for-range should resolve")
    .expect("builtin treesit-node-descendant-for-range should evaluate");
    assert!(node_descendant.is_nil());
}

#[test]
fn make_byte_code_from_parts_preserves_non_string_doc_slot_as_doc_form() {
    let value = make_byte_code_from_parts(
        &Value::list(vec![]),
        &Value::string(""),
        &Value::vector(vec![]),
        &Value::Int(0),
        Some(&Value::symbol("advice")),
        None,
    )
    .expect("byte-code constructor should accept oclosure type slot");

    let bytecode = value
        .get_bytecode_data()
        .expect("constructor should return a bytecode function");
    assert_eq!(bytecode.docstring, None);
    assert_eq!(bytecode.doc_form, Some(Value::symbol("advice")));
}

#[test]
fn pure_dispatch_treesit_node_placeholder_cluster_matches_compat_contracts() {
    let node_end = dispatch_builtin_pure("treesit-node-end", vec![Value::Nil])
        .expect("builtin treesit-node-end should resolve")
        .expect("builtin treesit-node-end should evaluate");
    assert!(node_end.is_nil());

    let node_eq = dispatch_builtin_pure("treesit-node-eq", vec![Value::Nil, Value::Nil])
        .expect("builtin treesit-node-eq should resolve")
        .expect("builtin treesit-node-eq should evaluate");
    assert!(node_eq.is_nil());

    let field_name = dispatch_builtin_pure(
        "treesit-node-field-name-for-child",
        vec![Value::Nil, Value::Int(0)],
    )
    .expect("builtin treesit-node-field-name-for-child should resolve")
    .expect("builtin treesit-node-field-name-for-child should evaluate");
    assert!(field_name.is_nil());

    let first_child_for_pos = dispatch_builtin_pure(
        "treesit-node-first-child-for-pos",
        vec![Value::Nil, Value::Int(0), Value::Nil],
    )
    .expect("builtin treesit-node-first-child-for-pos should resolve")
    .expect("builtin treesit-node-first-child-for-pos should evaluate");
    assert!(first_child_for_pos.is_nil());

    let match_p = dispatch_builtin_pure("treesit-node-match-p", vec![Value::Nil, Value::Nil])
        .expect("builtin treesit-node-match-p should resolve")
        .expect("builtin treesit-node-match-p should evaluate");
    assert!(match_p.is_nil());

    let next_sibling = dispatch_builtin_pure("treesit-node-next-sibling", vec![Value::Nil])
        .expect("builtin treesit-node-next-sibling should resolve")
        .expect("builtin treesit-node-next-sibling should evaluate");
    assert!(next_sibling.is_nil());

    let node_p = dispatch_builtin_pure("treesit-node-p", vec![Value::Nil])
        .expect("builtin treesit-node-p should resolve")
        .expect("builtin treesit-node-p should evaluate");
    assert!(node_p.is_nil());

    let parent = dispatch_builtin_pure("treesit-node-parent", vec![Value::Nil])
        .expect("builtin treesit-node-parent should resolve")
        .expect("builtin treesit-node-parent should evaluate");
    assert!(parent.is_nil());

    let parser = dispatch_builtin_pure("treesit-node-parser", vec![Value::Nil])
        .expect("builtin treesit-node-parser should resolve")
        .expect("builtin treesit-node-parser should evaluate");
    assert!(parser.is_nil());

    let prev_sibling = dispatch_builtin_pure("treesit-node-prev-sibling", vec![Value::Nil])
        .expect("builtin treesit-node-prev-sibling should resolve")
        .expect("builtin treesit-node-prev-sibling should evaluate");
    assert!(prev_sibling.is_nil());

    let start = dispatch_builtin_pure("treesit-node-start", vec![Value::Nil])
        .expect("builtin treesit-node-start should resolve")
        .expect("builtin treesit-node-start should evaluate");
    assert!(start.is_nil());

    let node_string = dispatch_builtin_pure("treesit-node-string", vec![Value::Nil])
        .expect("builtin treesit-node-string should resolve")
        .expect("builtin treesit-node-string should evaluate");
    assert!(node_string.is_nil());

    let node_type = dispatch_builtin_pure("treesit-node-type", vec![Value::Nil])
        .expect("builtin treesit-node-type should resolve")
        .expect("builtin treesit-node-type should evaluate");
    assert!(node_type.is_nil());
}

#[test]
fn pure_dispatch_typed_ignore_accepts_any_arity() {
    let zero = dispatch_builtin_pure("ignore", vec![])
        .expect("builtin ignore should resolve")
        .expect("builtin ignore should evaluate");
    assert!(zero.is_nil());

    let many = dispatch_builtin_pure(
        "ignore",
        vec![Value::Int(1), Value::string("x"), Value::symbol("foo")],
    )
    .expect("builtin ignore should resolve")
    .expect("builtin ignore should evaluate");
    assert!(many.is_nil());
}

#[test]
fn match_data_round_trip_with_nil_groups() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    builtin_set_match_data_eval(
        &mut eval,
        vec![Value::list(vec![
            Value::Int(0),
            Value::Int(2),
            Value::Nil,
            Value::Nil,
            Value::Int(5),
            Value::Int(7),
        ])],
    )
    .expect("set-match-data should succeed");

    let md = builtin_match_data_eval(&mut eval, vec![]).expect("match-data should succeed");
    assert_eq!(
        md,
        Value::list(vec![
            Value::Int(0),
            Value::Int(2),
            Value::Nil,
            Value::Nil,
            Value::Int(5),
            Value::Int(7)
        ])
    );
}

#[test]
fn match_beginning_end_return_nil_without_match_data() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    builtin_set_match_data_eval(&mut eval, vec![Value::Nil]).expect("set-match-data nil");

    let beg = builtin_match_beginning(&mut eval, vec![Value::Int(0)])
        .expect("match-beginning should not error");
    let end =
        builtin_match_end(&mut eval, vec![Value::Int(0)]).expect("match-end should not error");
    assert!(beg.is_nil());
    assert!(end.is_nil());
}

#[test]
fn negative_match_group_signals_args_out_of_range() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let match_string_err = builtin_match_string(&mut eval, vec![Value::Int(-1)])
        .expect_err("negative subgroup should signal");
    match match_string_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "args-out-of-range");
            assert_eq!(sig.data, vec![Value::Int(-1), Value::Int(0)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let match_beginning_err = builtin_match_beginning(&mut eval, vec![Value::Int(-1)])
        .expect_err("negative subgroup should signal");
    match match_beginning_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "args-out-of-range");
            assert_eq!(sig.data, vec![Value::Int(-1), Value::Int(0)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let match_end_err = builtin_match_end(&mut eval, vec![Value::Int(-1)])
        .expect_err("negative subgroup should signal");
    match match_end_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "args-out-of-range");
            assert_eq!(sig.data, vec![Value::Int(-1), Value::Int(0)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn buffer_region_negative_bounds_signal_without_panicking() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    builtin_insert(&mut eval, vec![Value::string("abc")]).expect("insert should succeed");
    let current = builtin_current_buffer(&mut eval, vec![]).expect("current-buffer should work");

    let substring_err = builtin_buffer_substring(&mut eval, vec![Value::Int(-1), Value::Int(2)])
        .expect_err("negative start should signal");
    match substring_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "args-out-of-range");
            assert_eq!(sig.data, vec![current, Value::Int(-1), Value::Int(2)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let delete_err = builtin_delete_region(&mut eval, vec![Value::Int(-1), Value::Int(2)])
        .expect_err("negative start should signal");
    match delete_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "args-out-of-range");
            assert_eq!(sig.data, vec![current, Value::Int(-1), Value::Int(2)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let narrow_err = builtin_narrow_to_region(&mut eval, vec![Value::Int(-1), Value::Int(2)])
        .expect_err("negative start should signal");
    match narrow_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "args-out-of-range");
            assert_eq!(sig.data, vec![Value::Int(-1), Value::Int(2)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    assert_eq!(
        builtin_char_after(&mut eval, vec![Value::Int(-1)]).expect("char-after should succeed"),
        Value::Nil
    );
    assert_eq!(
        builtin_char_before(&mut eval, vec![Value::Int(0)]).expect("char-before should succeed"),
        Value::Nil
    );
}

#[test]
fn string_match_start_handles_nil_and_negative_offsets() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let with_nil = builtin_string_match_eval(
        &mut eval,
        vec![Value::string("a"), Value::string("ba"), Value::Nil],
    )
    .expect("string-match with nil start");
    assert_eq!(with_nil, Value::Int(1));

    let with_negative = builtin_string_match_eval(
        &mut eval,
        vec![Value::string("a"), Value::string("ba"), Value::Int(-1)],
    )
    .expect("string-match with negative start");
    assert_eq!(with_negative, Value::Int(1));

    let out_of_range = builtin_string_match_eval(
        &mut eval,
        vec![Value::string("a"), Value::string("ba"), Value::Int(3)],
    );
    assert!(out_of_range.is_err());
}

#[test]
fn search_match_runtime_arity_edges_match_oracle_contracts() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let search_over_arity = builtin_search_forward(
        &mut eval,
        vec![
            Value::string("a"),
            Value::Nil,
            Value::Nil,
            Value::Int(1),
            Value::Nil,
        ],
    );
    assert!(matches!(
        search_over_arity,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));

    let regex_over_arity = builtin_re_search_forward(
        &mut eval,
        vec![
            Value::string("a"),
            Value::Nil,
            Value::Nil,
            Value::Int(1),
            Value::Nil,
        ],
    );
    assert!(matches!(
        regex_over_arity,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));

    let looking_at_optional_second =
        builtin_looking_at(&mut eval, vec![Value::string("a"), Value::True]);
    assert!(looking_at_optional_second.is_ok());

    let looking_at_over_arity =
        builtin_looking_at(&mut eval, vec![Value::string("a"), Value::Nil, Value::Nil]);
    assert!(matches!(
        looking_at_over_arity,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));

    let looking_at_p_over_arity =
        builtin_looking_at_p(&mut eval, vec![Value::string("a"), Value::Nil]);
    assert!(matches!(
        looking_at_p_over_arity,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));

    let looking_at_p_bad_type = builtin_looking_at_p(&mut eval, vec![Value::Int(1)]);
    assert!(matches!(
        looking_at_p_bad_type,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));

    let match_string_over_arity = builtin_match_string(
        &mut eval,
        vec![Value::Int(0), Value::string("a"), Value::Nil],
    );
    assert!(matches!(
        match_string_over_arity,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));

    let replace_match_over_arity = builtin_replace_match(
        &mut eval,
        vec![
            Value::string("x"),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
        ],
    );
    assert!(matches!(
        replace_match_over_arity,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));

    let string_match_over_arity = builtin_string_match_eval(
        &mut eval,
        vec![
            Value::string("a"),
            Value::string("a"),
            Value::Int(0),
            Value::Nil,
            Value::Nil,
        ],
    );
    assert!(matches!(
        string_match_over_arity,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));

    let string_match_p_over_arity = builtin_string_match_p_eval(
        &mut eval,
        vec![
            Value::string("a"),
            Value::string("a"),
            Value::Int(0),
            Value::Nil,
        ],
    );
    assert!(matches!(
        string_match_p_over_arity,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn set_match_data_rejects_non_list() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let result = builtin_set_match_data_eval(&mut eval, vec![Value::Int(1)]);
    assert!(result.is_err());
}

#[test]
fn looking_at_inhibit_modify_preserves_match_data() {
    use crate::emacs_core::eval::Evaluator;

    let mut eval = Evaluator::new();
    {
        let buffer = eval.buffers.current_buffer_mut().expect("scratch buffer");
        buffer.insert("abc");
        buffer.goto_char(0);
    }

    let baseline = Value::list(vec![Value::Int(10), Value::Int(11)]);
    builtin_set_match_data_eval(&mut eval, vec![baseline]).expect("setting baseline match-data");
    let result = builtin_looking_at(&mut eval, vec![Value::string("a"), Value::True]);
    assert!(result.is_ok());

    let observed = builtin_match_data_eval(&mut eval, vec![]).expect("read match-data");
    assert_eq!(observed, baseline);
}

#[test]
fn looking_at_updates_match_data_when_allowed() {
    use crate::emacs_core::eval::Evaluator;

    let mut eval = Evaluator::new();
    {
        let buffer = eval.buffers.current_buffer_mut().expect("scratch buffer");
        buffer.insert("abc");
        buffer.goto_char(0);
    }

    builtin_set_match_data_eval(&mut eval, vec![Value::Nil]).expect("clear match-data");
    let result = builtin_looking_at(&mut eval, vec![Value::string("a"), Value::Nil]);
    assert!(result.is_ok());

    let observed = builtin_match_data_eval(&mut eval, vec![]).expect("read match-data");
    assert_eq!(observed, Value::list(vec![Value::Int(0), Value::Int(1)]));
}

#[test]
fn looking_at_p_preserves_match_data() {
    use crate::emacs_core::eval::Evaluator;

    let mut eval = Evaluator::new();
    {
        let buffer = eval.buffers.current_buffer_mut().expect("scratch buffer");
        buffer.insert("abc");
        buffer.goto_char(0);
    }

    let baseline = Value::list(vec![Value::Int(1), Value::Int(2)]);
    builtin_set_match_data_eval(&mut eval, vec![baseline]).expect("seed baseline");
    let _ = builtin_looking_at_p(&mut eval, vec![Value::string("z")])
        .expect("looking-at-p handles non-match");
    let observed = builtin_match_data_eval(&mut eval, vec![]).expect("read match-data");
    assert_eq!(observed, baseline);
}

#[test]
fn string_match_inhibit_modify_preserves_match_data() {
    use crate::emacs_core::eval::Evaluator;

    let mut eval = Evaluator::new();
    let baseline = Value::list(vec![Value::Int(10), Value::Int(11)]);
    builtin_set_match_data_eval(&mut eval, vec![baseline]).expect("seed baseline");

    let result = builtin_string_match_eval(
        &mut eval,
        vec![
            Value::string("\\(foo\\)\\(bar\\)"),
            Value::string("foobar"),
            Value::Nil,
            Value::True,
        ],
    )
    .expect("string-match with inhibit-modify");
    assert_eq!(result, Value::Int(0));

    let observed = builtin_match_data_eval(&mut eval, vec![]).expect("read match-data");
    assert_eq!(observed, baseline);
}

#[test]
fn replace_match_missing_subexp_signals_error() {
    use crate::emacs_core::eval::Evaluator;

    let mut eval = Evaluator::new();
    builtin_string_match_eval(
        &mut eval,
        vec![Value::string("\\(foo\\)"), Value::string("foo")],
    )
    .expect("seed match data");

    let result = builtin_replace_match(
        &mut eval,
        vec![
            Value::string("bar"),
            Value::Nil,
            Value::Nil,
            Value::string("foo"),
            Value::Int(2),
        ],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "error"
                && sig.data
                    == vec![
                        Value::string("replace-match subexpression does not exist"),
                        Value::Int(2),
                    ]
    ));
}

#[test]
fn looking_at_p_respects_case_fold_search() {
    use crate::emacs_core::eval::Evaluator;

    let mut eval = Evaluator::new();
    {
        let buffer = eval.buffers.current_buffer_mut().expect("scratch buffer");
        buffer.insert("A");
        buffer.goto_char(0);
    }

    eval.set_variable("case-fold-search", Value::Nil);
    let sensitive = builtin_looking_at(&mut eval, vec![Value::string("a"), Value::Nil])
        .expect("looking-at with case-fold-search=nil");
    let sensitive_p = builtin_looking_at_p(&mut eval, vec![Value::string("a")])
        .expect("looking-at-p with case-fold-search=nil");
    assert!(sensitive.is_nil());
    assert!(sensitive_p.is_nil());

    eval.set_variable("case-fold-search", Value::True);
    let insensitive = builtin_looking_at(&mut eval, vec![Value::string("a"), Value::Nil])
        .expect("looking-at with case-fold-search=t");
    let insensitive_p = builtin_looking_at_p(&mut eval, vec![Value::string("a")])
        .expect("looking-at-p with case-fold-search=t");
    assert!(insensitive.is_truthy());
    assert!(insensitive_p.is_truthy());
}

#[test]
fn dispatch_builtin_resolves_typed_only_pure_names() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let result = dispatch_builtin(
        &mut eval,
        "string-equal",
        vec![Value::string("neo"), Value::string("neo")],
    )
    .expect("dispatch_builtin should resolve string-equal")
    .expect("dispatch_builtin should evaluate string-equal");
    assert!(result.is_truthy());
}

#[test]
fn dispatch_builtin_pure_defers_evaluator_window_accessors_and_mutators() {
    assert!(dispatch_builtin_pure("functionp", vec![]).is_none());
    assert!(dispatch_builtin_pure("copy-file", vec![]).is_none());
    assert!(dispatch_builtin_pure("defvaralias", vec![]).is_none());
    assert!(dispatch_builtin_pure("delete-file", vec![]).is_none());
    assert!(dispatch_builtin_pure("display-color-p", vec![]).is_none());
    assert!(dispatch_builtin_pure("format", vec![]).is_none());
    assert!(dispatch_builtin_pure("format-message", vec![]).is_none());
    assert!(dispatch_builtin_pure("indirect-variable", vec![]).is_none());
    assert!(dispatch_builtin_pure("insert-and-inherit", vec![]).is_none());
    assert!(dispatch_builtin_pure("insert-before-markers-and-inherit", vec![]).is_none());
    assert!(dispatch_builtin_pure("insert-buffer-substring", vec![]).is_none());
    assert!(dispatch_builtin_pure("kill-all-local-variables", vec![]).is_none());
    assert!(dispatch_builtin_pure("make-directory", vec![]).is_none());
    assert!(dispatch_builtin_pure("make-temp-file", vec![]).is_none());
    assert!(dispatch_builtin_pure("macroexpand", vec![]).is_none());
    assert!(dispatch_builtin_pure("message", vec![]).is_none());
    assert!(dispatch_builtin_pure("message-box", vec![]).is_none());
    assert!(dispatch_builtin_pure("message-or-box", vec![]).is_none());
    assert!(dispatch_builtin_pure("error", vec![]).is_none());
    assert!(dispatch_builtin_pure("princ", vec![]).is_none());
    assert!(dispatch_builtin_pure("prin1", vec![]).is_none());
    assert!(dispatch_builtin_pure("prin1-to-string", vec![]).is_none());
    assert!(dispatch_builtin_pure("print", vec![]).is_none());
    assert!(dispatch_builtin_pure("rename-file", vec![]).is_none());
    assert!(dispatch_builtin_pure("replace-buffer-contents", vec![]).is_none());
    assert!(dispatch_builtin_pure("set-buffer-multibyte", vec![]).is_none());
    assert!(dispatch_builtin_pure("split-window-internal", vec![]).is_none());
    assert!(dispatch_builtin_pure("setplist", vec![]).is_none());
    assert!(dispatch_builtin_pure("terminal-live-p", vec![]).is_none());
    assert!(dispatch_builtin_pure("terminal-name", vec![]).is_none());
    assert!(dispatch_builtin_pure("terpri", vec![]).is_none());
    assert!(dispatch_builtin_pure("undo-boundary", vec![]).is_none());
    assert!(dispatch_builtin_pure("write-char", vec![]).is_none());
    assert!(dispatch_builtin_pure("assoc", vec![]).is_none());
    assert!(dispatch_builtin_pure("alist-get", vec![]).is_none());
    assert!(dispatch_builtin_pure("plist-member", vec![]).is_none());
    assert!(dispatch_builtin_pure("old-selected-window", vec![]).is_none());
    assert!(dispatch_builtin_pure("frame-old-selected-window", vec![]).is_none());
    assert!(dispatch_builtin_pure("set-frame-selected-window", vec![]).is_none());
    assert!(dispatch_builtin_pure("window-system", vec![]).is_none());
    assert!(dispatch_builtin_pure("frame-edges", vec![]).is_none());
    assert!(dispatch_builtin_pure("window-at", vec![]).is_none());
    assert!(dispatch_builtin_pure("window-bump-use-time", vec![]).is_none());
    assert!(dispatch_builtin_pure("window-list-1", vec![]).is_none());
    assert!(dispatch_builtin_pure("add-variable-watcher", vec![]).is_none());
    assert!(dispatch_builtin_pure("remove-variable-watcher", vec![]).is_none());
    assert!(dispatch_builtin_pure("get-variable-watchers", vec![]).is_none());
}

#[test]
fn dispatch_builtin_pure_handles_treesit_parser_query_and_search_placeholders() {
    let parser = dispatch_builtin_pure("treesit-parser-buffer", vec![Value::Nil])
        .expect("treesit-parser-buffer should resolve")
        .expect("treesit-parser-buffer should evaluate");
    assert_eq!(parser, Value::Nil);

    let search = dispatch_builtin_pure(
        "treesit-search-forward",
        vec![Value::Nil, Value::Nil, Value::Nil, Value::Nil],
    )
    .expect("treesit-search-forward should resolve")
    .expect("treesit-search-forward should evaluate");
    assert_eq!(search, Value::Nil);

    let err = dispatch_builtin_pure("treesit-query-compile", vec![Value::Nil])
        .expect("treesit-query-compile should resolve")
        .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn dispatch_builtin_pure_handles_inotify_watch_lifecycle() {
    let watch = dispatch_builtin_pure(
        "inotify-add-watch",
        vec![Value::string("/tmp"), Value::Nil, Value::symbol("ignore")],
    )
    .expect("inotify-add-watch should resolve")
    .expect("inotify-add-watch should evaluate");
    let active = dispatch_builtin_pure("inotify-valid-p", vec![watch])
        .expect("inotify-valid-p should resolve")
        .expect("inotify-valid-p should evaluate");
    assert_eq!(active, Value::True);

    let removed = dispatch_builtin_pure("inotify-rm-watch", vec![watch])
        .expect("inotify-rm-watch should resolve")
        .expect("inotify-rm-watch should evaluate");
    assert_eq!(removed, Value::True);

    let inactive = dispatch_builtin_pure("inotify-valid-p", vec![watch])
        .expect("inotify-valid-p should resolve")
        .expect("inotify-valid-p should evaluate");
    assert_eq!(inactive, Value::Nil);
}

#[test]
fn dispatch_builtin_pure_handles_sqlite_lifecycle_and_closed_handle_guard() {
    let db = dispatch_builtin_pure("sqlite-open", vec![])
        .expect("sqlite-open should resolve")
        .expect("sqlite-open should evaluate");
    let sqlitep = dispatch_builtin_pure("sqlitep", vec![db])
        .expect("sqlitep should resolve")
        .expect("sqlitep should evaluate");
    assert_eq!(sqlitep, Value::True);

    let closed = dispatch_builtin_pure("sqlite-close", vec![db])
        .expect("sqlite-close should resolve")
        .expect("sqlite-close should evaluate");
    assert_eq!(closed, Value::True);

    let err = dispatch_builtin_pure("sqlite-execute", vec![db, Value::string("select 1")])
        .expect("sqlite-execute should resolve")
        .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn dispatch_builtin_pure_handles_fillarray_and_find_coding_region_internal() {
    let vector = Value::vector(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    let filled = dispatch_builtin_pure("fillarray", vec![vector, Value::Int(9)])
        .expect("fillarray should resolve")
        .expect("fillarray should evaluate");
    let Value::Vector(values) = filled else {
        panic!("expected vector");
    };
    assert_eq!(
        &*with_heap(|h| h.get_vector(values).clone()),
        &[Value::Int(9), Value::Int(9), Value::Int(9)]
    );

    assert!(
        dispatch_builtin_pure(
            "find-coding-systems-region-internal",
            vec![Value::Int(0), Value::Int(1)]
        )
        .is_none(),
        "find-coding-systems-region-internal should use the eval-aware path"
    );

    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let coding = dispatch_builtin(
        &mut eval,
        "find-coding-systems-region-internal",
        vec![Value::string("hello"), Value::Nil],
    )
    .expect("find-coding-systems-region-internal should resolve")
    .expect("find-coding-systems-region-internal should evaluate");
    assert_eq!(coding, Value::True);
}

#[test]
fn dispatch_builtin_pure_handles_fringe_display_and_debug_output_placeholders() {
    let bitmap = dispatch_builtin_pure(
        "define-fringe-bitmap",
        vec![Value::symbol("neo"), Value::vector(vec![Value::Int(0)])],
    )
    .expect("define-fringe-bitmap should resolve")
    .expect("define-fringe-bitmap should evaluate");
    assert_eq!(bitmap, Value::symbol("neo"));

    let destroy = dispatch_builtin_pure("destroy-fringe-bitmap", vec![Value::symbol("neo")])
        .expect("destroy-fringe-bitmap should resolve")
        .expect("destroy-fringe-bitmap should evaluate");
    assert_eq!(destroy, Value::Nil);

    let line = dispatch_builtin_pure("display--line-is-continued-p", vec![])
        .expect("display--line-is-continued-p should resolve")
        .expect("display--line-is-continued-p should evaluate");
    assert_eq!(line, Value::Nil);

    let update = dispatch_builtin_pure(
        "display--update-for-mouse-movement",
        vec![Value::Int(0), Value::Int(0)],
    )
    .expect("display--update-for-mouse-movement should resolve")
    .expect("display--update-for-mouse-movement should evaluate");
    assert_eq!(update, Value::Nil);

    let autosave = dispatch_builtin_pure("do-auto-save", vec![])
        .expect("do-auto-save should resolve")
        .expect("do-auto-save should evaluate");
    assert_eq!(autosave, Value::Nil);

    let err = dispatch_builtin_pure("external-debugging-output", vec![Value::Int(-1)])
        .expect("external-debugging-output should resolve")
        .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "error"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn dispatch_builtin_pure_handles_internal_labeled_and_modified_tick_placeholders() {
    assert!(
        dispatch_builtin_pure(
            "internal--define-uninitialized-variable",
            vec![Value::symbol("neo-var"), Value::Nil],
        )
        .is_none(),
        "internal--define-uninitialized-variable should require evaluator context"
    );

    let narrow = dispatch_builtin_pure(
        "internal--labeled-narrow-to-region",
        vec![Value::Int(0), Value::Int(1), Value::symbol("tag")],
    )
    .expect("internal--labeled-narrow-to-region should resolve")
    .expect("internal--labeled-narrow-to-region should evaluate");
    assert_eq!(narrow, Value::Nil);

    let widen = dispatch_builtin_pure("internal--labeled-widen", vec![Value::symbol("tag")])
        .expect("internal--labeled-widen should resolve")
        .expect("internal--labeled-widen should evaluate");
    assert_eq!(widen, Value::Nil);

    let buckets = dispatch_builtin_pure("internal--obarray-buckets", vec![Value::vector(vec![])])
        .expect("internal--obarray-buckets should resolve")
        .expect("internal--obarray-buckets should evaluate");
    assert_eq!(buckets, Value::Nil);

    let tick = dispatch_builtin_pure(
        "internal--set-buffer-modified-tick",
        vec![Value::Int(0), Value::Nil],
    )
    .expect("internal--set-buffer-modified-tick should resolve")
    .expect("internal--set-buffer-modified-tick should evaluate");
    assert_eq!(tick, Value::Nil);
}

#[test]
fn internal_define_uninitialized_variable_marks_special_and_sets_doc() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let result = dispatch_builtin(
        &mut eval,
        "internal--define-uninitialized-variable",
        vec![Value::symbol("neo-var"), Value::string("Neo doc")],
    )
    .expect("internal--define-uninitialized-variable should resolve")
    .expect("internal--define-uninitialized-variable should evaluate");
    assert_eq!(result, Value::Nil);
    assert!(eval.obarray().is_special("neo-var"));
    assert_eq!(
        eval.obarray()
            .get_property("neo-var", "variable-documentation")
            .copied(),
        Some(Value::string("Neo doc"))
    );
}

#[test]
fn internal_labeled_narrow_to_region_clamps_within_current_restriction() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let buf_id = eval.buffers.create_buffer(" *vm-labeled-narrow*");
    eval.buffers.set_current(buf_id);
    let _ = eval.buffers.insert_into_buffer(buf_id, "abcdef");
    let _ = eval.buffers.narrow_buffer_to_region(buf_id, 1, 4);

    let narrowed = dispatch_builtin(
        &mut eval,
        "internal--labeled-narrow-to-region",
        vec![Value::Int(0), Value::Int(99), Value::symbol("vm-tag")],
    )
    .expect("internal--labeled-narrow-to-region should resolve")
    .expect("internal--labeled-narrow-to-region should evaluate");
    assert_eq!(narrowed, Value::Nil);

    let buf = eval.buffers.get(buf_id).expect("buffer should stay live");
    assert_eq!(buf.point_min_char() as i64 + 1, 2);
    assert_eq!(buf.point_max_char() as i64 + 1, 5);
}

#[test]
fn dispatch_builtin_pure_handles_window_resize_and_frame_switch_placeholders() {
    let save = dispatch_builtin_pure("handle-save-session", vec![Value::symbol("event")])
        .expect("handle-save-session should resolve")
        .expect("handle-save-session should evaluate");
    assert_eq!(save, Value::Nil);

    let frame = dispatch_builtin_pure("handle-switch-frame", vec![Value::Frame(1)])
        .expect("handle-switch-frame should resolve")
        .expect("handle-switch-frame should evaluate");
    assert_eq!(frame, Value::Nil);

    let divider = dispatch_builtin_pure("window-bottom-divider-width", vec![])
        .expect("window-bottom-divider-width should resolve")
        .expect("window-bottom-divider-width should evaluate");
    assert_eq!(divider, Value::Int(0));

    let resize = dispatch_builtin_pure("window-resize-apply-total", vec![])
        .expect("window-resize-apply-total should resolve")
        .expect("window-resize-apply-total should evaluate");
    assert_eq!(resize, Value::True);
}

#[test]
fn dispatch_builtin_pure_handles_window_placeholder_accessors() {
    let root_window_id = 1_u64;
    let minibuffer_window_id =
        crate::window::MINIBUFFER_WINDOW_ID_BASE + crate::window::FRAME_ID_BASE;

    let left = dispatch_builtin_pure("window-left-child", vec![Value::Nil])
        .expect("window-left-child should resolve")
        .expect("window-left-child should evaluate");
    assert_eq!(left, Value::Nil);

    let next = dispatch_builtin_pure("window-next-sibling", vec![])
        .expect("window-next-sibling should resolve")
        .expect("window-next-sibling should evaluate");
    assert_eq!(next, Value::Window(minibuffer_window_id));

    let next_root =
        dispatch_builtin_pure("window-next-sibling", vec![Value::Window(root_window_id)])
            .expect("window-next-sibling should resolve")
            .expect("window-next-sibling should evaluate");
    assert_eq!(next_root, Value::Window(minibuffer_window_id));

    let prev_minibuf = dispatch_builtin_pure(
        "window-prev-sibling",
        vec![Value::Window(minibuffer_window_id)],
    )
    .expect("window-prev-sibling should resolve")
    .expect("window-prev-sibling should evaluate");
    assert_eq!(prev_minibuf, Value::Window(root_window_id));

    let prev_root =
        dispatch_builtin_pure("window-prev-sibling", vec![Value::Window(root_window_id)])
            .expect("window-prev-sibling should resolve")
            .expect("window-prev-sibling should evaluate");
    assert_eq!(prev_root, Value::Nil);

    let line_height = dispatch_builtin_pure(
        "window-line-height",
        vec![Value::Int(0), Value::symbol("window")],
    )
    .expect("window-line-height should resolve")
    .expect("window-line-height should evaluate");
    assert_eq!(line_height, Value::Nil);

    let normal = dispatch_builtin_pure("window-normal-size", vec![])
        .expect("window-normal-size should resolve")
        .expect("window-normal-size should evaluate");
    assert_eq!(normal, Value::Float(1.0, next_float_id()));

    let old_body = dispatch_builtin_pure("window-old-body-pixel-height", vec![])
        .expect("window-old-body-pixel-height should resolve")
        .expect("window-old-body-pixel-height should evaluate");
    assert_eq!(old_body, Value::Int(0));

    let tab = dispatch_builtin_pure("window-tab-line-height", vec![])
        .expect("window-tab-line-height should resolve")
        .expect("window-tab-line-height should evaluate");
    assert_eq!(tab, Value::Int(0));

    let err = dispatch_builtin_pure("window-right-divider-width", vec![Value::Int(1)])
        .expect("window-right-divider-width should resolve")
        .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn dispatch_builtin_pure_handles_gpm_help_and_init_image_placeholders() {
    let err = dispatch_builtin_pure("gpm-mouse-start", vec![])
        .expect("gpm-mouse-start should resolve")
        .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "error"),
        other => panic!("expected signal, got {other:?}"),
    }

    let stop = dispatch_builtin_pure("gpm-mouse-stop", vec![])
        .expect("gpm-mouse-stop should resolve")
        .expect("gpm-mouse-stop should evaluate");
    assert_eq!(stop, Value::Nil);

    let help = dispatch_builtin_pure(
        "help--describe-vector",
        vec![
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
        ],
    )
    .expect("help--describe-vector should resolve")
    .expect("help--describe-vector should evaluate");
    assert_eq!(help, Value::Nil);

    let init = dispatch_builtin_pure("init-image-library", vec![Value::symbol("png")])
        .expect("init-image-library should resolve")
        .expect("init-image-library should evaluate");
    assert_eq!(init, Value::Nil);
}

#[test]
fn dispatch_builtin_pure_handles_frame_placeholder_accessors() {
    let face_table = dispatch_builtin_pure("frame--face-hash-table", vec![])
        .expect("frame--face-hash-table should resolve")
        .expect("frame--face-hash-table should evaluate");
    let Value::HashTable(table) = face_table else {
        panic!("expected hash table");
    };
    assert!(matches!(
        with_heap(|h| h.get_hash_table(table).test.clone()),
        HashTableTest::Eq
    ));

    let was_invisible =
        dispatch_builtin_pure("frame--set-was-invisible", vec![Value::Nil, Value::True])
            .expect("frame--set-was-invisible should resolve")
            .expect("frame--set-was-invisible should evaluate");
    assert_eq!(was_invisible, Value::True);

    let changed = dispatch_builtin_pure("frame-or-buffer-changed-p", vec![])
        .expect("frame-or-buffer-changed-p should resolve")
        .expect("frame-or-buffer-changed-p should evaluate");
    assert_eq!(changed, Value::True);

    let changed_nil = dispatch_builtin_pure("frame-or-buffer-changed-p", vec![Value::Nil])
        .expect("frame-or-buffer-changed-p should resolve")
        .expect("frame-or-buffer-changed-p should evaluate");
    assert_eq!(changed_nil, Value::Nil);

    let scale = dispatch_builtin_pure("frame-scale-factor", vec![])
        .expect("frame-scale-factor should resolve")
        .expect("frame-scale-factor should evaluate");
    assert_eq!(scale, Value::Float(1.0, next_float_id()));

    let pointer = dispatch_builtin_pure("frame-pointer-visible-p", vec![])
        .expect("frame-pointer-visible-p should resolve")
        .expect("frame-pointer-visible-p should evaluate");
    assert_eq!(pointer, Value::True);

    let err = dispatch_builtin_pure("frame-or-buffer-changed-p", vec![Value::Int(1)])
        .expect("frame-or-buffer-changed-p should resolve")
        .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn dispatch_builtin_pure_handles_describe_and_delete_terminal_placeholders() {
    let bindings = dispatch_builtin_pure(
        "describe-buffer-bindings",
        vec![Value::Buffer(crate::buffer::BufferId(0))],
    )
    .expect("describe-buffer-bindings should resolve")
    .expect("describe-buffer-bindings should evaluate");
    assert_eq!(bindings, Value::Nil);

    let seq_err = dispatch_builtin_pure(
        "describe-buffer-bindings",
        vec![Value::Buffer(crate::buffer::BufferId(0)), Value::Int(1)],
    )
    .expect("describe-buffer-bindings should resolve")
    .unwrap_err();
    match seq_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }

    let vec_err = dispatch_builtin_pure(
        "describe-vector",
        vec![
            Value::vector(vec![Value::Int(1)]),
            Value::symbol("display-buffer"),
        ],
    )
    .expect("describe-vector should resolve")
    .unwrap_err();
    match vec_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "void-function"),
        other => panic!("expected signal, got {other:?}"),
    }

    let delete_err = dispatch_builtin_pure("delete-terminal", vec![])
        .expect("delete-terminal should resolve")
        .unwrap_err();
    match delete_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "error"),
        other => panic!("expected signal, got {other:?}"),
    }

    let deleted = dispatch_builtin_pure("delete-terminal", vec![Value::symbol("tty")])
        .expect("delete-terminal should resolve")
        .expect("delete-terminal should evaluate");
    assert_eq!(deleted, Value::Nil);
}

#[test]
fn dispatch_builtin_pure_handles_fringe_gap_and_garbage_placeholders() {
    let fringe = dispatch_builtin_pure("fringe-bitmaps-at-pos", vec![Value::Nil, Value::Nil])
        .expect("fringe-bitmaps-at-pos should resolve")
        .expect("fringe-bitmaps-at-pos should evaluate");
    assert_eq!(fringe, Value::Nil);

    let gap_pos = dispatch_builtin_pure("gap-position", vec![])
        .expect("gap-position should resolve")
        .expect("gap-position should evaluate");
    assert_eq!(gap_pos, Value::Int(1));

    let gap_size = dispatch_builtin_pure("gap-size", vec![])
        .expect("gap-size should resolve")
        .expect("gap-size should evaluate");
    assert_eq!(gap_size, Value::Int(2001));

    let gc = dispatch_builtin_pure("garbage-collect-maybe", vec![Value::Int(0)])
        .expect("garbage-collect-maybe should resolve")
        .expect("garbage-collect-maybe should evaluate");
    assert_eq!(gc, Value::Nil);

    let prop_err = dispatch_builtin_pure(
        "get-unicode-property-internal",
        vec![Value::Nil, Value::Int(0)],
    )
    .expect("get-unicode-property-internal should resolve")
    .unwrap_err();
    match prop_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn dispatch_builtin_pure_handles_gnutls_query_and_error_placeholders() {
    let available = dispatch_builtin_pure("gnutls-available-p", vec![])
        .expect("gnutls-available-p should resolve")
        .expect("gnutls-available-p should evaluate");
    assert_eq!(available, Value::list(vec![Value::symbol("gnutls")]));

    let ciphers = dispatch_builtin_pure("gnutls-ciphers", vec![])
        .expect("gnutls-ciphers should resolve")
        .expect("gnutls-ciphers should evaluate");
    assert_eq!(ciphers, Value::list(vec![Value::symbol("AES-256-GCM")]));

    let digests = dispatch_builtin_pure("gnutls-digests", vec![])
        .expect("gnutls-digests should resolve")
        .expect("gnutls-digests should evaluate");
    assert_eq!(digests, Value::list(vec![Value::symbol("SHA256")]));

    let macs = dispatch_builtin_pure("gnutls-macs", vec![])
        .expect("gnutls-macs should resolve")
        .expect("gnutls-macs should evaluate");
    assert_eq!(macs, Value::list(vec![Value::symbol("AEAD")]));

    let errorp = dispatch_builtin_pure("gnutls-errorp", vec![Value::Int(0)])
        .expect("gnutls-errorp should resolve")
        .expect("gnutls-errorp should evaluate");
    assert_eq!(errorp, Value::True);

    let success = dispatch_builtin_pure("gnutls-error-string", vec![Value::Int(0)])
        .expect("gnutls-error-string should resolve")
        .expect("gnutls-error-string should evaluate");
    assert_eq!(success, Value::string("Success."));

    let fatal_err = dispatch_builtin_pure("gnutls-error-fatalp", vec![Value::Nil])
        .expect("gnutls-error-fatalp should resolve")
        .unwrap_err();
    match fatal_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "error"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn dispatch_builtin_pure_handles_gnutls_runtime_placeholders() {
    let peer_warning =
        dispatch_builtin_pure("gnutls-peer-status-warning-describe", vec![Value::Nil])
            .expect("gnutls-peer-status-warning-describe should resolve")
            .expect("gnutls-peer-status-warning-describe should evaluate");
    assert_eq!(peer_warning, Value::Nil);

    let bye_err = dispatch_builtin_pure("gnutls-bye", vec![Value::Nil, Value::Nil])
        .expect("gnutls-bye should resolve")
        .unwrap_err();
    match bye_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }

    let cert_err = dispatch_builtin_pure("gnutls-format-certificate", vec![Value::Nil])
        .expect("gnutls-format-certificate should resolve")
        .unwrap_err();
    match cert_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }

    let digest_err =
        dispatch_builtin_pure("gnutls-hash-digest", vec![Value::Nil, Value::string("a")])
            .expect("gnutls-hash-digest should resolve")
            .unwrap_err();
    match digest_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "error"),
        other => panic!("expected signal, got {other:?}"),
    }

    let mac = dispatch_builtin_pure(
        "gnutls-hash-mac",
        vec![
            Value::symbol("SHA256"),
            Value::string("k"),
            Value::string("a"),
        ],
    )
    .expect("gnutls-hash-mac should resolve")
    .expect("gnutls-hash-mac should evaluate");
    assert_eq!(mac, Value::string("mac"));

    let enc = dispatch_builtin_pure(
        "gnutls-symmetric-encrypt",
        vec![
            Value::symbol("AES-128-GCM"),
            Value::string("k"),
            Value::string("iv"),
            Value::string("data"),
            Value::string("aad"),
        ],
    )
    .expect("gnutls-symmetric-encrypt should resolve")
    .expect("gnutls-symmetric-encrypt should evaluate");
    assert_eq!(enc, Value::Nil);
}

#[test]
fn dispatch_builtin_pure_handles_font_face_placeholders() {
    let face = dispatch_builtin_pure("face-attributes-as-vector", vec![Value::Nil])
        .expect("face-attributes-as-vector should resolve")
        .expect("face-attributes-as-vector should evaluate");
    let Value::Vector(values) = face else {
        panic!("expected vector");
    };
    assert_eq!(
        with_heap(|h| h.get_vector(values).len()),
        FACE_ATTRIBUTES_VECTOR_LEN
    );

    let font_object = Value::vector(vec![Value::keyword("font-object")]);
    let font_spec = Value::vector(vec![Value::keyword("font-spec")]);

    let attrs = dispatch_builtin_pure("font-face-attributes", vec![font_object])
        .expect("font-face-attributes should resolve")
        .expect("font-face-attributes should evaluate");
    let Value::Vector(values) = attrs else {
        panic!("expected vector");
    };
    assert_eq!(
        with_heap(|h| h.get_vector(values).len()),
        FACE_ATTRIBUTES_VECTOR_LEN
    );

    let glyphs = dispatch_builtin_pure(
        "font-get-glyphs",
        vec![font_object, Value::Int(0), Value::Int(1)],
    )
    .expect("font-get-glyphs should resolve")
    .expect("font-get-glyphs should evaluate");
    assert_eq!(glyphs, Value::Nil);

    let has_char =
        dispatch_builtin_pure("font-has-char-p", vec![font_spec, Value::Int('a' as i64)])
            .expect("font-has-char-p should resolve")
            .expect("font-has-char-p should evaluate");
    assert_eq!(has_char, Value::Nil);

    let match_err = dispatch_builtin_pure("font-match-p", vec![Value::Nil, font_spec])
        .expect("font-match-p should resolve")
        .unwrap_err();
    match match_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }

    let at_err = dispatch_builtin_pure("font-at", vec![Value::Int(1)])
        .expect("font-at should resolve")
        .unwrap_err();
    match at_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "error"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn dispatch_builtin_pure_handles_fontset_placeholders() {
    let system = dispatch_builtin_pure("font-get-system-font", vec![])
        .expect("font-get-system-font should resolve")
        .expect("font-get-system-font should evaluate");
    assert_eq!(system, Value::Nil);

    let normal = dispatch_builtin_pure("font-get-system-normal-font", vec![])
        .expect("font-get-system-normal-font should resolve")
        .expect("font-get-system-normal-font should evaluate");
    assert_eq!(normal, Value::Nil);

    let fontset = dispatch_builtin_pure(
        "fontset-font",
        vec![Value::symbol("fontset-default"), Value::Int('a' as i64)],
    )
    .expect("fontset-font should resolve")
    .expect("fontset-font should evaluate");
    assert_eq!(fontset, Value::Nil);

    let info_err = dispatch_builtin_pure("fontset-info", vec![Value::symbol("fontset-default")])
        .expect("fontset-info should resolve")
        .unwrap_err();
    match info_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "error"),
        other => panic!("expected signal, got {other:?}"),
    }

    let list = dispatch_builtin_pure("fontset-list", vec![])
        .expect("fontset-list should resolve")
        .expect("fontset-list should evaluate");
    assert_eq!(
        list,
        Value::list(vec![Value::string(
            "-*-*-*-*-*-*-*-*-*-*-*-*-fontset-default"
        )])
    );

    let fontset_err = dispatch_builtin_pure(
        "fontset-font",
        vec![Value::symbol("fontset-default"), Value::Nil],
    )
    .expect("fontset-font should resolve")
    .unwrap_err();
    match fontset_err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn prin1_to_string_prints_canonical_threading_handles_as_opaque() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let thread = dispatch_builtin(&mut eval, "current-thread", vec![])
        .expect("current-thread should resolve")
        .expect("current-thread should evaluate");
    let thread_text = dispatch_builtin(&mut eval, "prin1-to-string", vec![thread])
        .expect("prin1-to-string should resolve for thread")
        .expect("prin1-to-string should evaluate for thread");
    assert_eq!(thread_text, Value::string("#<thread 0>"));

    let mutex = dispatch_builtin(&mut eval, "make-mutex", vec![])
        .expect("make-mutex should resolve")
        .expect("make-mutex should evaluate");
    let mutex_text = dispatch_builtin(&mut eval, "prin1-to-string", vec![mutex])
        .expect("prin1-to-string should resolve for mutex")
        .expect("prin1-to-string should evaluate for mutex");
    assert_eq!(mutex_text, Value::string("#<mutex 1>"));

    let condvar = dispatch_builtin(&mut eval, "make-condition-variable", vec![mutex])
        .expect("make-condition-variable should resolve")
        .expect("make-condition-variable should evaluate");
    let condvar_text = dispatch_builtin(&mut eval, "prin1-to-string", vec![condvar])
        .expect("prin1-to-string should resolve for condvar")
        .expect("prin1-to-string should evaluate for condvar");
    assert_eq!(condvar_text, Value::string("#<condvar 1>"));
}

#[test]
fn prin1_to_string_keeps_forged_threading_handles_as_cons() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let forged_thread = Value::cons(Value::symbol("thread"), Value::Int(0));
    let thread_text = dispatch_builtin(&mut eval, "prin1-to-string", vec![forged_thread])
        .expect("prin1-to-string should resolve for forged thread")
        .expect("prin1-to-string should evaluate for forged thread");
    assert_eq!(thread_text, Value::string("(thread . 0)"));

    let forged_mutex = Value::cons(Value::symbol("mutex"), Value::Int(1));
    let mutex_text = dispatch_builtin(&mut eval, "prin1-to-string", vec![forged_mutex])
        .expect("prin1-to-string should resolve for forged mutex")
        .expect("prin1-to-string should evaluate for forged mutex");
    assert_eq!(mutex_text, Value::string("(mutex . 1)"));

    let forged_condvar = Value::cons(Value::symbol("condition-variable"), Value::Int(1));
    let condvar_text = dispatch_builtin(&mut eval, "prin1-to-string", vec![forged_condvar])
        .expect("prin1-to-string should resolve for forged condvar")
        .expect("prin1-to-string should evaluate for forged condvar");
    assert_eq!(condvar_text, Value::string("(condition-variable . 1)"));
}

#[test]
fn prin1_to_string_supports_noescape_for_strings() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let value = Value::string("a\nb");

    let escaped = dispatch_builtin(&mut eval, "prin1-to-string", vec![value])
        .expect("prin1-to-string should resolve")
        .expect("prin1-to-string should evaluate");
    // GNU Emacs default: print-escape-newlines is nil, so \n passes
    // through literally in prin1-to-string.
    assert_eq!(escaped, Value::string("\"a\nb\""));

    let noescape = dispatch_builtin(&mut eval, "prin1-to-string", vec![value, Value::True])
        .expect("prin1-to-string should resolve with noescape")
        .expect("prin1-to-string should evaluate with noescape");
    assert_eq!(noescape, Value::string("a\nb"));
}

#[test]
fn prin1_to_string_respects_print_gensym_binding() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let sym = Value::Symbol(intern_uninterned("vm-print-gensym"));

    let default_text = dispatch_builtin(&mut eval, "prin1-to-string", vec![sym])
        .expect("prin1-to-string should resolve")
        .expect("prin1-to-string should evaluate");
    assert_eq!(default_text, Value::string("vm-print-gensym"));

    eval.set_variable("print-gensym", Value::True);
    let gensym_text = dispatch_builtin(&mut eval, "prin1-to-string", vec![sym])
        .expect("prin1-to-string should resolve with print-gensym")
        .expect("prin1-to-string should evaluate with print-gensym");
    assert_eq!(gensym_text, Value::string("#:vm-print-gensym"));
}

#[test]
fn prin1_to_string_ignores_extra_args_for_compat() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let result = dispatch_builtin(
        &mut eval,
        "prin1-to-string",
        vec![Value::Int(1), Value::Nil, Value::Nil],
    )
    .expect("prin1-to-string should resolve with extra args")
    .expect("prin1-to-string should evaluate with extra args");
    assert_eq!(result, Value::string("1"));
}

#[test]
fn format_prints_thread_handles_as_opaque_in_eval_dispatch() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let thread = dispatch_builtin(&mut eval, "current-thread", vec![])
        .expect("current-thread should resolve")
        .expect("current-thread should evaluate");

    let upper = dispatch_builtin(&mut eval, "format", vec![Value::string("%S"), thread])
        .expect("format should resolve for %S")
        .expect("format should evaluate for %S");
    assert!(upper.as_str().is_some_and(|s| s.starts_with("#<thread")));

    let lower = dispatch_builtin(&mut eval, "format", vec![Value::string("%s"), thread])
        .expect("format should resolve for %s")
        .expect("format should evaluate for %s");
    assert!(lower.as_str().is_some_and(|s| s.starts_with("#<thread")));
}

#[test]
fn message_prints_thread_handles_as_opaque_in_eval_dispatch() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let thread = dispatch_builtin(&mut eval, "current-thread", vec![])
        .expect("current-thread should resolve")
        .expect("current-thread should evaluate");
    let message = dispatch_builtin(&mut eval, "message", vec![Value::string("%S"), thread])
        .expect("message should resolve")
        .expect("message should evaluate");
    assert!(message.as_str().is_some_and(|s| s.starts_with("#<thread")));
}

#[test]
fn format_and_message_render_terminal_handles_in_eval_dispatch() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let terminals = dispatch_builtin(&mut eval, "terminal-list", vec![])
        .expect("terminal-list should resolve")
        .expect("terminal-list should evaluate");
    let terminal = list_to_vec(&terminals)
        .and_then(|items| items.into_iter().next())
        .expect("terminal-list should return at least one terminal");

    let mut assert_prefix = |builtin: &str, spec: &str| {
        let rendered = dispatch_builtin(&mut eval, builtin, vec![Value::string(spec), terminal])
            .expect("builtin should resolve")
            .expect("builtin should evaluate");
        assert!(
            rendered
                .as_str()
                .is_some_and(|s| s.starts_with("#<terminal")),
            "expected {builtin} {spec} output to start with #<terminal, got: {rendered:?}"
        );
    };

    assert_prefix("format", "%s");
    assert_prefix("message", "%s");
    assert_prefix("format", "%S");
    assert_prefix("message", "%S");
}

#[test]
fn format_and_message_render_mutex_condvar_handles_in_eval_dispatch() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let mutex = dispatch_builtin(&mut eval, "make-mutex", vec![])
        .expect("make-mutex should resolve")
        .expect("make-mutex should evaluate");
    let condvar = dispatch_builtin(&mut eval, "make-condition-variable", vec![mutex])
        .expect("make-condition-variable should resolve")
        .expect("make-condition-variable should evaluate");

    let mut assert_prefix = |builtin: &str, spec: &str, value: Value, prefix: &str| {
        let rendered = dispatch_builtin(&mut eval, builtin, vec![Value::string(spec), value])
            .expect("builtin should resolve")
            .expect("builtin should evaluate");
        assert!(
            rendered.as_str().is_some_and(|s| s.starts_with(prefix)),
            "expected {builtin} {spec} output to start with {prefix}, got: {rendered:?}"
        );
    };

    assert_prefix("format", "%s", mutex, "#<mutex");
    assert_prefix("message", "%s", mutex, "#<mutex");
    assert_prefix("format", "%S", mutex, "#<mutex");
    assert_prefix("message", "%S", mutex, "#<mutex");

    assert_prefix("format", "%s", condvar, "#<condvar");
    assert_prefix("message", "%s", condvar, "#<condvar");
    assert_prefix("format", "%S", condvar, "#<condvar");
    assert_prefix("message", "%S", condvar, "#<condvar");
}

#[test]
fn format_and_message_render_killed_buffer_handles_in_eval_dispatch() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let buffer = create_unique_test_buffer(&mut eval, "*format-killed-buffer*");
    let _ = dispatch_builtin(&mut eval, "kill-buffer", vec![buffer])
        .expect("kill-buffer should resolve")
        .expect("kill-buffer should evaluate");

    let formatted = dispatch_builtin(&mut eval, "format", vec![Value::string("%S"), buffer])
        .expect("format should resolve")
        .expect("format should evaluate");
    assert_eq!(formatted, Value::string("#<killed buffer>"));

    let message = dispatch_builtin(&mut eval, "message", vec![Value::string("%S"), buffer])
        .expect("message should resolve")
        .expect("message should evaluate");
    assert_eq!(message, Value::string("#<killed buffer>"));
}

#[test]
fn format_and_message_render_live_buffer_handles_in_eval_dispatch() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let buffer = create_unique_test_buffer(&mut eval, "*format-live-buffer*");

    let formatted = dispatch_builtin(&mut eval, "format", vec![Value::string("%S"), buffer])
        .expect("format should resolve")
        .expect("format should evaluate");
    assert!(
        formatted
            .as_str()
            .is_some_and(|s| s.starts_with("#<buffer *format-live-buffer")),
        "expected live buffer name in format output: {formatted:?}"
    );

    let message = dispatch_builtin(&mut eval, "message", vec![Value::string("%S"), buffer])
        .expect("message should resolve")
        .expect("message should evaluate");
    assert!(
        message
            .as_str()
            .is_some_and(|s| s.starts_with("#<buffer *format-live-buffer")),
        "expected live buffer name in message output: {message:?}"
    );
}

#[test]
fn format_and_message_percent_s_render_live_buffer_names_in_eval_dispatch() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let expected = "*format-live-s-buffer*";
    let buffer = create_unique_test_buffer(&mut eval, expected);

    let formatted = dispatch_builtin(&mut eval, "format", vec![Value::string("%s"), buffer])
        .expect("format should resolve")
        .expect("format should evaluate");
    assert_eq!(formatted, Value::string(expected));

    let message = dispatch_builtin(&mut eval, "message", vec![Value::string("%s"), buffer])
        .expect("message should resolve")
        .expect("message should evaluate");
    assert_eq!(message, Value::string(expected));
}

#[test]
fn format_and_message_percent_s_render_killed_buffer_handles_in_eval_dispatch() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let buffer = create_unique_test_buffer(&mut eval, "*format-killed-s-buffer*");
    let _ = dispatch_builtin(&mut eval, "kill-buffer", vec![buffer])
        .expect("kill-buffer should resolve")
        .expect("kill-buffer should evaluate");

    let formatted = dispatch_builtin(&mut eval, "format", vec![Value::string("%s"), buffer])
        .expect("format should resolve")
        .expect("format should evaluate");
    assert_eq!(formatted, Value::string("#<killed buffer>"));

    let message = dispatch_builtin(&mut eval, "message", vec![Value::string("%s"), buffer])
        .expect("message should resolve")
        .expect("message should evaluate");
    assert_eq!(message, Value::string("#<killed buffer>"));
}

#[test]
fn format_and_message_render_frame_window_handles_in_eval_dispatch() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let frame = dispatch_builtin(&mut eval, "selected-frame", vec![])
        .expect("selected-frame should resolve")
        .expect("selected-frame should evaluate");
    let window = dispatch_builtin(&mut eval, "selected-window", vec![])
        .expect("selected-window should resolve")
        .expect("selected-window should evaluate");

    {
        let mut assert_prefix = |builtin: &str, spec: &str, value: Value, prefix: &str| {
            let rendered = dispatch_builtin(&mut eval, builtin, vec![Value::string(spec), value])
                .expect("builtin should resolve")
                .expect("builtin should evaluate");
            assert!(
                rendered.as_str().is_some_and(|s| s.starts_with(prefix)),
                "expected {builtin} {spec} output to start with {prefix}, got: {rendered:?}"
            );
        };

        assert_prefix("format", "%S", frame, "#<frame");
        assert_prefix("message", "%S", frame, "#<frame");
        assert_prefix("format", "%s", frame, "#<frame");
        assert_prefix("message", "%s", frame, "#<frame");
    }

    {
        let mut assert_contains = |builtin: &str, spec: &str, value: Value, snippet: &str| {
            let rendered = dispatch_builtin(&mut eval, builtin, vec![Value::string(spec), value])
                .expect("builtin should resolve")
                .expect("builtin should evaluate");
            assert!(
                rendered.as_str().is_some_and(|s| s.contains(snippet)),
                "expected {builtin} {spec} output to contain {snippet}, got: {rendered:?}"
            );
        };

        assert_contains("format", "%S", window, "on *scratch*>");
        assert_contains("message", "%S", window, "on *scratch*>");
        assert_contains("format", "%s", window, "on *scratch*>");
        assert_contains("message", "%s", window, "on *scratch*>");
    }
}

#[test]
fn format_message_renders_opaque_handles_in_eval_dispatch() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let thread = dispatch_builtin(&mut eval, "current-thread", vec![])
        .expect("current-thread should resolve")
        .expect("current-thread should evaluate");
    let terminals = dispatch_builtin(&mut eval, "terminal-list", vec![])
        .expect("terminal-list should resolve")
        .expect("terminal-list should evaluate");
    let terminal = list_to_vec(&terminals)
        .and_then(|items| items.into_iter().next())
        .expect("terminal-list should return at least one terminal");
    let frame = dispatch_builtin(&mut eval, "selected-frame", vec![])
        .expect("selected-frame should resolve")
        .expect("selected-frame should evaluate");
    let window = dispatch_builtin(&mut eval, "selected-window", vec![])
        .expect("selected-window should resolve")
        .expect("selected-window should evaluate");

    let mut assert_prefix = |spec: &str, value: Value, prefix: &str| {
        let rendered = dispatch_builtin(
            &mut eval,
            "format-message",
            vec![Value::string(spec), value],
        )
        .expect("format-message should resolve")
        .expect("format-message should evaluate");
        assert!(
            rendered.as_str().is_some_and(|s| s.starts_with(prefix)),
            "expected format-message {spec} output to start with {prefix}, got: {rendered:?}"
        );
    };

    assert_prefix("%S", thread, "#<thread");
    assert_prefix("%s", thread, "#<thread");
    assert_prefix("%S", terminal, "#<terminal");
    assert_prefix("%S", frame, "#<frame");
    assert_prefix("%S", window, "#<window");
    assert!(
        dispatch_builtin(
            &mut eval,
            "format-message",
            vec![Value::string("%S"), window]
        )
        .expect("format-message should resolve")
        .expect("format-message should evaluate")
        .as_str()
        .is_some_and(|s| s.contains("on *scratch*>")),
        "expected format-message window output to include buffer context"
    );

    let live_name = "*format-message-live-buffer*";
    let live_buffer = create_unique_test_buffer(&mut eval, live_name);
    let live_upper = dispatch_builtin(
        &mut eval,
        "format-message",
        vec![Value::string("%S"), live_buffer],
    )
    .expect("format-message should resolve")
    .expect("format-message should evaluate");
    assert!(
        live_upper
            .as_str()
            .is_some_and(|s| s.starts_with("#<buffer *format-message-live-buffer")),
        "expected live buffer in format-message %S output: {live_upper:?}"
    );
    let live_lower = dispatch_builtin(
        &mut eval,
        "format-message",
        vec![Value::string("%s"), live_buffer],
    )
    .expect("format-message should resolve")
    .expect("format-message should evaluate");
    assert_eq!(live_lower, Value::string(live_name));
    let _ = dispatch_builtin(&mut eval, "kill-buffer", vec![live_buffer])
        .expect("kill-buffer should resolve")
        .expect("kill-buffer should evaluate");

    let killed_buffer = create_unique_test_buffer(&mut eval, "*format-message-killed-buffer*");
    let _ = dispatch_builtin(&mut eval, "kill-buffer", vec![killed_buffer])
        .expect("kill-buffer should resolve")
        .expect("kill-buffer should evaluate");
    let killed_upper = dispatch_builtin(
        &mut eval,
        "format-message",
        vec![Value::string("%S"), killed_buffer],
    )
    .expect("format-message should resolve")
    .expect("format-message should evaluate");
    assert_eq!(killed_upper, Value::string("#<killed buffer>"));
    let killed_lower = dispatch_builtin(
        &mut eval,
        "format-message",
        vec![Value::string("%s"), killed_buffer],
    )
    .expect("format-message should resolve")
    .expect("format-message should evaluate");
    assert_eq!(killed_lower, Value::string("#<killed buffer>"));
}

#[test]
fn error_message_string_preserves_percent_s_handle_semantics() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let render_error_message =
        |eval: &mut crate::emacs_core::eval::Evaluator, spec: &str, value: Value| -> String {
            let signaled = dispatch_builtin(eval, "error", vec![Value::string(spec), value])
                .expect("error should resolve")
                .expect_err("error should signal");
            let (symbol, data) = match signaled {
                Flow::Signal(sig) => (sig.symbol, sig.data),
                other => panic!("expected signal flow, got: {other:?}"),
            };
            let mut err_data = Vec::with_capacity(data.len() + 1);
            err_data.push(Value::Symbol(symbol));
            err_data.extend(data);
            let rendered =
                dispatch_builtin(eval, "error-message-string", vec![Value::list(err_data)])
                    .expect("error-message-string should resolve")
                    .expect("error-message-string should evaluate");
            rendered
                .as_str()
                .expect("error-message-string should return a string")
                .to_string()
        };

    let live_name = "*ems-live-lower*";
    let live_buffer = create_unique_test_buffer(&mut eval, live_name);
    assert_eq!(
        render_error_message(&mut eval, "%s", live_buffer),
        live_name
    );
    let _ = dispatch_builtin(&mut eval, "kill-buffer", vec![live_buffer])
        .expect("kill-buffer should resolve")
        .expect("kill-buffer should evaluate");
    assert_eq!(
        render_error_message(&mut eval, "%s", live_buffer),
        "#<killed buffer>".to_string()
    );

    let thread = dispatch_builtin(&mut eval, "current-thread", vec![])
        .expect("current-thread should resolve")
        .expect("current-thread should evaluate");
    assert!(render_error_message(&mut eval, "%s", thread).starts_with("#<thread"));

    let mutex = dispatch_builtin(&mut eval, "make-mutex", vec![])
        .expect("make-mutex should resolve")
        .expect("make-mutex should evaluate");
    assert!(render_error_message(&mut eval, "%s", mutex).starts_with("#<mutex"));
    let condvar = dispatch_builtin(&mut eval, "make-condition-variable", vec![mutex])
        .expect("make-condition-variable should resolve")
        .expect("make-condition-variable should evaluate");
    assert!(render_error_message(&mut eval, "%s", condvar).starts_with("#<condvar"));
}

#[test]
fn message_box_wrappers_render_opaque_handles_in_eval_dispatch() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    for (builtin, symbol) in [
        ("message-box", "message-box"),
        ("message-or-box", "message-or-box"),
    ] {
        let err = dispatch_builtin(&mut eval, builtin, vec![])
            .expect("wrapper should resolve")
            .expect_err("wrapper should signal on missing format argument");
        match err {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
                assert_eq!(sig.data, vec![Value::symbol(symbol), Value::Int(0)]);
            }
            other => panic!("expected signal, got: {other:?}"),
        }
    }

    let message_box_nil = dispatch_builtin(&mut eval, "message-box", vec![Value::Nil])
        .expect("message-box should resolve")
        .expect("message-box should evaluate");
    assert!(message_box_nil.is_nil());
    let message_or_box_nil = dispatch_builtin(&mut eval, "message-or-box", vec![Value::Nil])
        .expect("message-or-box should resolve")
        .expect("message-or-box should evaluate");
    assert!(message_or_box_nil.is_nil());

    for builtin in ["message-box", "message-or-box"] {
        let wrong_type = dispatch_builtin(&mut eval, builtin, vec![Value::Int(1)])
            .expect("wrapper should resolve")
            .expect_err("wrapper should signal for non-string format");
        match wrong_type {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::Int(1)]);
            }
            other => panic!("expected signal, got: {other:?}"),
        }

        let missing = dispatch_builtin(
            &mut eval,
            builtin,
            vec![Value::string("%s %s"), Value::Int(1)],
        )
        .expect("wrapper should resolve")
        .expect_err("wrapper should signal when format args are missing");
        match missing {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "error");
                assert_eq!(
                    sig.data,
                    vec![Value::string("Not enough arguments for format string")]
                );
            }
            other => panic!("expected signal, got: {other:?}"),
        }

        let negative_char = dispatch_builtin(
            &mut eval,
            builtin,
            vec![Value::string("%c"), Value::Int(-1)],
        )
        .expect("wrapper should resolve")
        .expect_err("wrapper should reject negative character code");
        match negative_char {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(sig.data, vec![Value::symbol("characterp"), Value::Int(-1)]);
            }
            other => panic!("expected signal, got: {other:?}"),
        }

        let overflow_char = dispatch_builtin(
            &mut eval,
            builtin,
            vec![Value::string("%c"), Value::Int(0x40_0000)],
        )
        .expect("wrapper should resolve")
        .expect_err("wrapper should reject out-of-range character code");
        match overflow_char {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(
                    sig.data,
                    vec![Value::symbol("characterp"), Value::Int(0x40_0000)]
                );
            }
            other => panic!("expected signal, got: {other:?}"),
        }
    }

    let high_chars = [
        ("message-box", 0x11_0000_i64),
        ("message-or-box", 0x20_0000_i64),
    ];
    for (builtin, value) in high_chars {
        let rendered = dispatch_builtin(
            &mut eval,
            builtin,
            vec![Value::string("%c"), Value::Int(value)],
        )
        .expect("wrapper should resolve")
        .expect("wrapper should evaluate");
        let text = rendered.as_str().expect("wrapper should return a string");
        assert_eq!(decode_storage_char_codes(text), vec![value as u32]);
    }

    let _ = dispatch_builtin(
        &mut eval,
        "message-box",
        vec![Value::string("mbox-current")],
    )
    .expect("message-box should resolve")
    .expect("message-box should evaluate");
    let current_after_box = dispatch_builtin(&mut eval, "current-message", vec![])
        .expect("current-message should resolve")
        .expect("current-message should evaluate");
    assert!(current_after_box.is_nil());
    let _ = dispatch_builtin(
        &mut eval,
        "message-or-box",
        vec![Value::string("morbox-current")],
    )
    .expect("message-or-box should resolve")
    .expect("message-or-box should evaluate");
    let current_after_or_box = dispatch_builtin(&mut eval, "current-message", vec![])
        .expect("current-message should resolve")
        .expect("current-message should evaluate");
    assert!(current_after_or_box.is_nil());

    let thread = dispatch_builtin(&mut eval, "current-thread", vec![])
        .expect("current-thread should resolve")
        .expect("current-thread should evaluate");
    let terminals = dispatch_builtin(&mut eval, "terminal-list", vec![])
        .expect("terminal-list should resolve")
        .expect("terminal-list should evaluate");
    let terminal = list_to_vec(&terminals)
        .and_then(|items| items.into_iter().next())
        .expect("terminal-list should return at least one terminal");
    let frame = dispatch_builtin(&mut eval, "selected-frame", vec![])
        .expect("selected-frame should resolve")
        .expect("selected-frame should evaluate");
    let window = dispatch_builtin(&mut eval, "selected-window", vec![])
        .expect("selected-window should resolve")
        .expect("selected-window should evaluate");
    let mutex = dispatch_builtin(&mut eval, "make-mutex", vec![])
        .expect("make-mutex should resolve")
        .expect("make-mutex should evaluate");
    let condvar = dispatch_builtin(&mut eval, "make-condition-variable", vec![mutex])
        .expect("make-condition-variable should resolve")
        .expect("make-condition-variable should evaluate");

    let mut assert_prefix = |builtin: &str, spec: &str, value: Value, prefix: &str| {
        let rendered = dispatch_builtin(&mut eval, builtin, vec![Value::string(spec), value])
            .expect("builtin should resolve")
            .expect("builtin should evaluate");
        assert!(
            rendered.as_str().is_some_and(|s| s.starts_with(prefix)),
            "expected {builtin} {spec} output to start with {prefix}, got: {rendered:?}"
        );
    };

    for builtin in ["message-box", "message-or-box"] {
        assert_prefix(builtin, "%S", thread, "#<thread");
        assert_prefix(builtin, "%s", thread, "#<thread");
        assert_prefix(builtin, "%S", terminal, "#<terminal");
        assert_prefix(builtin, "%s", terminal, "#<terminal");
        assert_prefix(builtin, "%S", mutex, "#<mutex");
        assert_prefix(builtin, "%s", mutex, "#<mutex");
        assert_prefix(builtin, "%S", condvar, "#<condvar");
        assert_prefix(builtin, "%s", condvar, "#<condvar");
        assert_prefix(builtin, "%S", frame, "#<frame");
        assert_prefix(builtin, "%s", frame, "#<frame");
        assert_prefix(builtin, "%S", window, "#<window");
        assert_prefix(builtin, "%s", window, "#<window");
    }

    let live_name = "*message-box-live-buffer*";
    let live_buffer = create_unique_test_buffer(&mut eval, live_name);
    let live_upper = dispatch_builtin(
        &mut eval,
        "message-box",
        vec![Value::string("%S"), live_buffer],
    )
    .expect("message-box should resolve")
    .expect("message-box should evaluate");
    assert!(
        live_upper
            .as_str()
            .is_some_and(|s| s.starts_with("#<buffer *message-box-live-buffer")),
        "expected live buffer in message-box %S output: {live_upper:?}"
    );
    let live_box_lower = dispatch_builtin(
        &mut eval,
        "message-box",
        vec![Value::string("%s"), live_buffer],
    )
    .expect("message-box should resolve")
    .expect("message-box should evaluate");
    assert_eq!(live_box_lower, Value::string(live_name));
    let live_or_upper = dispatch_builtin(
        &mut eval,
        "message-or-box",
        vec![Value::string("%S"), live_buffer],
    )
    .expect("message-or-box should resolve")
    .expect("message-or-box should evaluate");
    assert!(
        live_or_upper
            .as_str()
            .is_some_and(|s| s.starts_with("#<buffer *message-box-live-buffer")),
        "expected live buffer in message-or-box %S output: {live_or_upper:?}"
    );
    let live_lower = dispatch_builtin(
        &mut eval,
        "message-or-box",
        vec![Value::string("%s"), live_buffer],
    )
    .expect("message-or-box should resolve")
    .expect("message-or-box should evaluate");
    assert_eq!(live_lower, Value::string(live_name));
    let _ = dispatch_builtin(&mut eval, "kill-buffer", vec![live_buffer])
        .expect("kill-buffer should resolve")
        .expect("kill-buffer should evaluate");

    let killed_buffer = create_unique_test_buffer(&mut eval, "*message-box-killed-buffer*");
    let _ = dispatch_builtin(&mut eval, "kill-buffer", vec![killed_buffer])
        .expect("kill-buffer should resolve")
        .expect("kill-buffer should evaluate");
    let killed_upper = dispatch_builtin(
        &mut eval,
        "message-box",
        vec![Value::string("%S"), killed_buffer],
    )
    .expect("message-box should resolve")
    .expect("message-box should evaluate");
    assert_eq!(killed_upper, Value::string("#<killed buffer>"));
    let killed_box_lower = dispatch_builtin(
        &mut eval,
        "message-box",
        vec![Value::string("%s"), killed_buffer],
    )
    .expect("message-box should resolve")
    .expect("message-box should evaluate");
    assert_eq!(killed_box_lower, Value::string("#<killed buffer>"));
    let killed_or_upper = dispatch_builtin(
        &mut eval,
        "message-or-box",
        vec![Value::string("%S"), killed_buffer],
    )
    .expect("message-or-box should resolve")
    .expect("message-or-box should evaluate");
    assert_eq!(killed_or_upper, Value::string("#<killed buffer>"));
    let killed_lower = dispatch_builtin(
        &mut eval,
        "message-or-box",
        vec![Value::string("%s"), killed_buffer],
    )
    .expect("message-or-box should resolve")
    .expect("message-or-box should evaluate");
    assert_eq!(killed_lower, Value::string("#<killed buffer>"));
}

#[test]
fn message_nil_returns_nil() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let raw = builtin_message(vec![Value::Nil]).expect("message should accept nil");
    assert!(raw.is_nil());

    let eval_result =
        builtin_message_eval(&mut eval, vec![Value::Nil]).expect("message eval should accept nil");
    assert!(eval_result.is_nil());
}

#[test]
fn make_string_nonunicode_char_code_bounds_match_oracle() {
    let overflow = dispatch_builtin_pure("make-string", vec![Value::Int(1), Value::Int(0x40_0000)])
        .expect("make-string should resolve")
        .expect_err("make-string should reject out-of-range character code");
    match overflow {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("characterp"), Value::Int(0x40_0000)]
            );
        }
        other => panic!("expected signal, got: {other:?}"),
    }

    let repeated = dispatch_builtin_pure("make-string", vec![Value::Int(2), Value::Int(0x11_0000)])
        .expect("make-string should resolve")
        .expect("make-string should evaluate");
    let repeated_text = repeated
        .as_str()
        .expect("make-string should return string output");
    assert_eq!(
        decode_storage_char_codes(repeated_text),
        vec![0x11_0000, 0x11_0000]
    );

    let high = dispatch_builtin_pure("make-string", vec![Value::Int(1), Value::Int(0x20_0000)])
        .expect("make-string should resolve")
        .expect("make-string should evaluate");
    let high_text = high
        .as_str()
        .expect("make-string should return string output");
    assert_eq!(decode_storage_char_codes(high_text), vec![0x20_0000]);
}

#[test]
fn make_string_matches_emacs_ascii_boundary() {
    let ascii = dispatch_builtin_pure("make-string", vec![Value::Int(3), Value::Int('a' as i64)])
        .expect("make-string should resolve")
        .expect("ascii make-string should evaluate");
    let byte_200 = dispatch_builtin_pure("make-string", vec![Value::Int(2), Value::Int(200)])
        .expect("make-string should resolve")
        .expect("byte-200 make-string should evaluate");

    let ascii_multibyte = dispatch_builtin_pure("multibyte-string-p", vec![ascii])
        .expect("multibyte-string-p should resolve")
        .expect("ascii multibyte-string-p should evaluate");
    let byte_200_multibyte = dispatch_builtin_pure("multibyte-string-p", vec![byte_200])
        .expect("multibyte-string-p should resolve")
        .expect("byte-200 multibyte-string-p should evaluate");

    assert_eq!(ascii_multibyte, Value::Nil);
    assert_eq!(byte_200_multibyte, Value::True);
}

#[test]
fn text_char_description_nonunicode_char_code_bounds_match_oracle() {
    let high = dispatch_builtin_pure("text-char-description", vec![Value::Int(0x11_0000)])
        .expect("text-char-description should resolve")
        .expect("text-char-description should evaluate");
    let high_text = high
        .as_str()
        .expect("text-char-description should return string output");
    assert_eq!(decode_storage_char_codes(high_text), vec![0x11_0000]);

    let higher = dispatch_builtin_pure("text-char-description", vec![Value::Int(0x20_0000)])
        .expect("text-char-description should resolve")
        .expect("text-char-description should evaluate");
    let higher_text = higher
        .as_str()
        .expect("text-char-description should return string output");
    assert_eq!(decode_storage_char_codes(higher_text), vec![0x20_0000]);

    let overflow = dispatch_builtin_pure("text-char-description", vec![Value::Int(0x40_0000)])
        .expect("text-char-description should resolve")
        .expect_err("text-char-description should reject out-of-range character code");
    match overflow {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("characterp"), Value::Int(0x40_0000)]
            );
        }
        other => panic!("expected signal, got: {other:?}"),
    }
}

#[test]
fn assoc_delete_all_supports_default_equal_and_optional_test() {
    let results = bootstrap_eval_all(
        r#"
        (assoc-delete-all "foo" '(("foo" . 1) ignored ("bar" . 2) ("foo" . 3)))
        (let* ((key "foo")
               (alist (list (cons key 9) (cons (copy-sequence "foo") 10))))
          (assoc-delete-all key alist 'eq))
        (condition-case err (assoc-delete-all nil nil nil nil) (error (car err)))
        "#,
    );
    assert_eq!(results[0], r#"OK (ignored ("bar" . 2))"#);
    assert_eq!(results[1], r#"OK (("foo" . 10))"#);
    assert_eq!(results[2], "OK wrong-number-of-arguments");
}

#[test]
fn insert_char_nonunicode_char_code_bounds_match_oracle() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    builtin_erase_buffer(&mut eval, vec![]).expect("erase-buffer should succeed");
    builtin_insert_char(&mut eval, vec![Value::Int(0x11_0000), Value::Int(1)])
        .expect("insert-char should accept nonunicode char code");
    let first = builtin_buffer_string(&mut eval, vec![])
        .expect("buffer-string should evaluate")
        .as_str()
        .expect("buffer-string should return text")
        .to_string();
    assert_eq!(decode_storage_char_codes(&first), vec![0x11_0000]);

    builtin_erase_buffer(&mut eval, vec![]).expect("erase-buffer should succeed");
    builtin_insert_char(&mut eval, vec![Value::Int(0x20_0000), Value::Int(2)])
        .expect("insert-char should repeat nonunicode char code");
    let second = builtin_buffer_string(&mut eval, vec![])
        .expect("buffer-string should evaluate")
        .as_str()
        .expect("buffer-string should return text")
        .to_string();
    assert_eq!(
        decode_storage_char_codes(&second),
        vec![0x20_0000, 0x20_0000]
    );

    let overflow = builtin_insert_char(&mut eval, vec![Value::Int(0x40_0000), Value::Int(1)])
        .expect_err("insert-char should reject out-of-range character code");
    match overflow {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("characterp"), Value::Int(0x40_0000)]
            );
        }
        other => panic!("expected signal, got: {other:?}"),
    }
}

#[test]
fn insert_nonunicode_integer_arguments_match_oracle() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    builtin_erase_buffer(&mut eval, vec![]).expect("erase-buffer should succeed");
    builtin_insert(&mut eval, vec![Value::Int(0x11_0000)])
        .expect("insert should accept nonunicode integer char code");
    let first = builtin_buffer_string(&mut eval, vec![])
        .expect("buffer-string should evaluate")
        .as_str()
        .expect("buffer-string should return text")
        .to_string();
    assert_eq!(decode_storage_char_codes(&first), vec![0x11_0000]);

    builtin_erase_buffer(&mut eval, vec![]).expect("erase-buffer should succeed");
    builtin_insert(
        &mut eval,
        vec![Value::Int(0x20_0000), Value::Int(0x20_0000)],
    )
    .expect("insert should repeat nonunicode integer char codes");
    let second = builtin_buffer_string(&mut eval, vec![])
        .expect("buffer-string should evaluate")
        .as_str()
        .expect("buffer-string should return text")
        .to_string();
    assert_eq!(
        decode_storage_char_codes(&second),
        vec![0x20_0000, 0x20_0000]
    );

    let overflow = builtin_insert(&mut eval, vec![Value::Int(0x40_0000)])
        .expect_err("insert should reject out-of-range integer char code");
    match overflow {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("char-or-string-p"), Value::Int(0x40_0000)]
            );
        }
        other => panic!("expected signal, got: {other:?}"),
    }
}

#[test]
fn format_message_and_message_signal_strict_format_errors() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    for builtin in ["format", "format-message"] {
        for bad in [Value::Int(1), Value::Nil, Value::symbol("foo")] {
            let err = dispatch_builtin(&mut eval, builtin, vec![bad])
                .expect("builtin should resolve")
                .expect_err("builtin should signal for non-string format");
            match err {
                Flow::Signal(sig) => {
                    assert_eq!(sig.symbol_name(), "wrong-type-argument");
                    assert_eq!(sig.data, vec![Value::symbol("stringp"), bad]);
                }
                other => panic!("expected signal, got: {other:?}"),
            }
        }
    }

    for bad in [Value::Int(1), Value::symbol("foo")] {
        let err = dispatch_builtin(&mut eval, "message", vec![bad])
            .expect("message should resolve")
            .expect_err("message should signal for non-string/non-nil format");
        match err {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(sig.data, vec![Value::symbol("stringp"), bad]);
            }
            other => panic!("expected signal, got: {other:?}"),
        }
    }

    for builtin in ["format", "format-message", "message"] {
        let err = dispatch_builtin(
            &mut eval,
            builtin,
            vec![Value::string("%s %s"), Value::Int(1)],
        )
        .expect("builtin should resolve")
        .expect_err("builtin should signal when format args are missing");
        match err {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "error");
                assert_eq!(
                    sig.data,
                    vec![Value::string("Not enough arguments for format string")]
                );
            }
            other => panic!("expected signal, got: {other:?}"),
        }
    }

    for builtin in ["format", "format-message", "message"] {
        for spec in ["%d", "%f", "%c"] {
            let err = dispatch_builtin(
                &mut eval,
                builtin,
                vec![Value::string(spec), Value::string("x")],
            )
            .expect("builtin should resolve")
            .expect_err("builtin should signal on spec/type mismatch");
            match err {
                Flow::Signal(sig) => {
                    assert_eq!(sig.symbol_name(), "error");
                    assert_eq!(
                        sig.data,
                        vec![Value::string(
                            "Format specifier doesn’t match argument type"
                        )]
                    );
                }
                other => panic!("expected signal, got: {other:?}"),
            }
        }
    }

    for builtin in ["format", "format-message", "message"] {
        let err = dispatch_builtin(
            &mut eval,
            builtin,
            vec![Value::string("%c"), Value::Int(-1)],
        )
        .expect("builtin should resolve")
        .expect_err("builtin should reject negative character code");
        match err {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(sig.data, vec![Value::symbol("characterp"), Value::Int(-1)]);
            }
            other => panic!("expected signal, got: {other:?}"),
        }
    }

    for builtin in ["format", "format-message", "message"] {
        let err = dispatch_builtin(
            &mut eval,
            builtin,
            vec![Value::string("%c"), Value::Int(0x40_0000)],
        )
        .expect("builtin should resolve")
        .expect_err("builtin should reject out-of-range character code");
        match err {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(
                    sig.data,
                    vec![Value::symbol("characterp"), Value::Int(0x40_0000)]
                );
            }
            other => panic!("expected signal, got: {other:?}"),
        }
    }

    let high_chars = [
        ("format", 0x11_0000_i64),
        ("message", 0x11_0000_i64),
        ("format-message", 0x20_0000_i64),
    ];
    for (builtin, value) in high_chars {
        let rendered = dispatch_builtin(
            &mut eval,
            builtin,
            vec![Value::string("%c"), Value::Int(value)],
        )
        .expect("builtin should resolve")
        .expect("builtin should evaluate");
        let text = rendered.as_str().expect("builtin should return a string");
        assert_eq!(decode_storage_char_codes(text), vec![value as u32]);
    }
}

#[test]
fn user_error_signals_user_error_symbol_and_formatted_message() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let err = dispatch_builtin(
        &mut eval,
        "user-error",
        vec![Value::string("oops %s"), Value::string("now")],
    )
    .expect("user-error should resolve")
    .expect_err("user-error should signal");

    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "user-error");
            assert_eq!(sig.data, vec![Value::string("oops now")]);
        }
        other => panic!("expected signal, got: {other:?}"),
    }
}

#[test]
fn user_error_requires_message_argument() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let err = dispatch_builtin(&mut eval, "user-error", vec![])
        .expect("user-error should resolve")
        .expect_err("user-error should reject missing message");

    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(sig.data, vec![Value::symbol("user-error"), Value::Int(0)]);
        }
        other => panic!("expected signal, got: {other:?}"),
    }
}

#[test]
fn internal_save_selected_window_helpers_restore_selected_window() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let selected = dispatch_builtin(&mut eval, "selected-window", vec![])
        .expect("selected-window should resolve")
        .expect("selected-window should evaluate");
    let split = dispatch_builtin(
        &mut eval,
        "split-window-internal",
        vec![selected, Value::Nil, Value::Nil, Value::Nil],
    )
    .expect("split-window-internal should resolve")
    .expect("split-window-internal should evaluate");
    let state = dispatch_builtin(&mut eval, "internal--before-save-selected-window", vec![])
        .expect("before helper should resolve")
        .expect("before helper should evaluate");

    let _ = dispatch_builtin(&mut eval, "select-window", vec![split])
        .expect("select-window should resolve")
        .expect("select-window should evaluate");
    let switched = dispatch_builtin(&mut eval, "selected-window", vec![])
        .expect("selected-window should resolve")
        .expect("selected-window should evaluate");
    assert_eq!(switched, split);

    let restored = dispatch_builtin(
        &mut eval,
        "internal--after-save-selected-window",
        vec![state],
    )
    .expect("after helper should resolve")
    .expect("after helper should evaluate");
    assert!(restored.is_nil());

    let selected_again = dispatch_builtin(&mut eval, "selected-window", vec![])
        .expect("selected-window should resolve")
        .expect("selected-window should evaluate");
    assert_eq!(selected_again, selected);
}

#[test]
fn functionp_eval_matches_symbol_and_lambda_form_semantics() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let builtin_symbol = builtin_functionp_eval(&mut eval, vec![Value::symbol("message")])
        .expect("functionp should accept builtin symbol");
    assert!(builtin_symbol.is_truthy());

    let quoted_lambda = Value::list(vec![
        Value::symbol("lambda"),
        Value::list(vec![Value::symbol("x")]),
        Value::symbol("x"),
    ]);
    let lambda_result = builtin_functionp_eval(&mut eval, vec![quoted_lambda])
        .expect("functionp should accept quoted lambda list");
    assert!(lambda_result.is_truthy());

    let improper_lambda = Value::cons(Value::symbol("lambda"), Value::Int(1));
    let improper_result = builtin_functionp_eval(&mut eval, vec![improper_lambda])
        .expect("functionp should accept improper lambda forms");
    assert!(improper_result.is_truthy());

    // In official Emacs, (closure ENV PARAMS BODY...) cons lists ARE functions.
    let quoted_closure = Value::list(vec![
        Value::symbol("closure"),
        Value::list(vec![Value::True]),
        Value::list(vec![Value::symbol("x")]),
        Value::symbol("x"),
    ]);
    let closure_result = builtin_functionp_eval(&mut eval, vec![quoted_closure])
        .expect("functionp should accept quoted closure lists");
    assert!(closure_result.is_truthy());

    let special_symbol = builtin_functionp_eval(&mut eval, vec![Value::symbol("if")])
        .expect("functionp should reject special-form symbols");
    assert!(special_symbol.is_nil());

    let macro_symbol = builtin_functionp_eval(&mut eval, vec![Value::symbol("when")])
        .expect("functionp should reject macro symbols");
    assert!(macro_symbol.is_nil());
    let save_match_data_symbol =
        builtin_functionp_eval(&mut eval, vec![Value::symbol("save-match-data")])
            .expect("functionp should reject save-match-data macro symbol");
    assert!(save_match_data_symbol.is_nil());
    let save_mark_and_excursion_symbol =
        builtin_functionp_eval(&mut eval, vec![Value::symbol("save-mark-and-excursion")])
            .expect("functionp should reject save-mark-and-excursion macro symbol");
    assert!(save_mark_and_excursion_symbol.is_nil());
    let save_window_excursion_symbol =
        builtin_functionp_eval(&mut eval, vec![Value::symbol("save-window-excursion")])
            .expect("functionp should reject save-window-excursion macro symbol");
    assert!(save_window_excursion_symbol.is_nil());
    let save_selected_window_symbol =
        builtin_functionp_eval(&mut eval, vec![Value::symbol("save-selected-window")])
            .expect("functionp should reject save-selected-window macro symbol");
    assert!(save_selected_window_symbol.is_nil());
    let with_local_quit_symbol =
        builtin_functionp_eval(&mut eval, vec![Value::symbol("with-local-quit")])
            .expect("functionp should reject with-local-quit macro symbol");
    assert!(with_local_quit_symbol.is_nil());
    let with_temp_message_symbol =
        builtin_functionp_eval(&mut eval, vec![Value::symbol("with-temp-message")])
            .expect("functionp should reject with-temp-message macro symbol");
    assert!(with_temp_message_symbol.is_nil());
    let with_demoted_errors_symbol =
        builtin_functionp_eval(&mut eval, vec![Value::symbol("with-demoted-errors")])
            .expect("functionp should reject with-demoted-errors macro symbol");
    assert!(with_demoted_errors_symbol.is_nil());
    let bound_and_true_p_symbol =
        builtin_functionp_eval(&mut eval, vec![Value::symbol("bound-and-true-p")])
            .expect("functionp should reject bound-and-true-p macro symbol");
    assert!(bound_and_true_p_symbol.is_nil());
    let declare_symbol = builtin_functionp_eval(&mut eval, vec![Value::symbol("declare")])
        .expect("functionp should reject declare symbol");
    assert!(declare_symbol.is_nil());
    let inline_symbol = builtin_functionp_eval(&mut eval, vec![Value::symbol("inline")])
        .expect("functionp should reject inline symbol");
    assert!(inline_symbol.is_nil());
    let throw_symbol = builtin_functionp_eval(&mut eval, vec![Value::symbol("throw")])
        .expect("functionp should accept throw symbol");
    assert!(throw_symbol.is_truthy());
    for name in ["funcall", "defalias", "provide", "require"] {
        let result = builtin_functionp_eval(&mut eval, vec![Value::symbol(name)])
            .unwrap_or_else(|_| panic!("functionp should accept {name} symbol"));
        assert!(result.is_truthy(), "expected {name} to satisfy functionp");
    }
    let macro_marker_cons = builtin_functionp_eval(
        &mut eval,
        vec![Value::cons(Value::symbol("macro"), Value::True)],
    )
    .expect("functionp should reject dotted macro marker cons");
    assert!(macro_marker_cons.is_nil());
    let macro_marker_list = builtin_functionp_eval(
        &mut eval,
        vec![Value::list(vec![Value::symbol("macro"), Value::True])],
    )
    .expect("functionp should reject macro marker lists");
    assert!(macro_marker_list.is_nil());

    let special_subr = builtin_functionp_eval(&mut eval, vec![Value::Subr(intern("if"))])
        .expect("functionp should reject special-form subr objects");
    assert!(special_subr.is_nil());

    let autoload_function_forms = crate::emacs_core::parser::parse_forms(
        r#"(autoload 'vm-test-auto-fn "vm-test-file" nil t)"#,
    )
    .expect("autoload function form should parse");
    for form in &autoload_function_forms {
        eval.eval(form).expect("autoload function should register");
    }
    let autoload_function_symbol =
        builtin_functionp_eval(&mut eval, vec![Value::symbol("vm-test-auto-fn")])
            .expect("functionp should recognize autoload function symbol");
    assert!(autoload_function_symbol.is_truthy());
    let autoload_function_cell = *eval
        .obarray()
        .symbol_function("vm-test-auto-fn")
        .expect("autoload function cell exists");
    let autoload_function_cell = builtin_functionp_eval(&mut eval, vec![autoload_function_cell])
        .expect("functionp should reject raw autoload function cell object");
    assert!(autoload_function_cell.is_nil());

    let autoload_macro_forms = crate::emacs_core::parser::parse_forms(
        r#"(autoload 'vm-test-auto-macro "vm-test-file" nil nil 'macro)"#,
    )
    .expect("autoload macro form should parse");
    for form in &autoload_macro_forms {
        eval.eval(form).expect("autoload macro should register");
    }
    let autoload_macro_symbol =
        builtin_functionp_eval(&mut eval, vec![Value::symbol("vm-test-auto-macro")])
            .expect("functionp should reject autoload macro symbol");
    assert!(autoload_macro_symbol.is_nil());
}

#[test]
fn functionp_eval_resolves_t_and_keyword_symbol_designators() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let keyword = Value::keyword(":vm-functionp-keyword");
    let orig_t = builtin_symbol_function(&mut eval, vec![Value::True])
        .expect("symbol-function should read t cell");
    let orig_keyword = builtin_symbol_function(&mut eval, vec![keyword])
        .expect("symbol-function should read keyword cell");

    builtin_fset(&mut eval, vec![Value::True, Value::symbol("car")])
        .expect("fset should bind t function cell");
    builtin_fset(&mut eval, vec![keyword, Value::symbol("car")])
        .expect("fset should bind keyword function cell");

    let t_result =
        builtin_functionp_eval(&mut eval, vec![Value::True]).expect("functionp should accept t");
    assert!(t_result.is_truthy());
    let keyword_result = builtin_functionp_eval(&mut eval, vec![keyword])
        .expect("functionp should accept keyword designator");
    assert!(keyword_result.is_truthy());

    builtin_fset(&mut eval, vec![Value::True, orig_t]).expect("restore t function cell");
    builtin_fset(&mut eval, vec![keyword, orig_keyword]).expect("restore keyword function cell");
}

#[test]
fn fmakunbound_masks_builtin_special_and_evaluator_callables() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    builtin_fmakunbound(&mut eval, vec![Value::symbol("car")])
        .expect("fmakunbound should accept builtin name");
    let car_bound = builtin_fboundp(&mut eval, vec![Value::symbol("car")])
        .expect("fboundp should accept builtin name");
    assert!(car_bound.is_nil());
    let car_fn = builtin_symbol_function(&mut eval, vec![Value::symbol("car")])
        .expect("symbol-function should return nil after fmakunbound");
    assert!(car_fn.is_nil());
    let car_functionp = builtin_functionp_eval(&mut eval, vec![Value::symbol("car")])
        .expect("functionp should accept symbol");
    assert!(car_functionp.is_nil());

    builtin_fmakunbound(&mut eval, vec![Value::symbol("if")])
        .expect("fmakunbound should accept special form name");
    let if_bound = builtin_fboundp(&mut eval, vec![Value::symbol("if")])
        .expect("fboundp should accept special form name");
    assert!(if_bound.is_nil());
    let if_fn = builtin_symbol_function(&mut eval, vec![Value::symbol("if")])
        .expect("symbol-function should return nil after fmakunbound special form");
    assert!(if_fn.is_nil());

    builtin_fmakunbound(&mut eval, vec![Value::symbol("throw")])
        .expect("fmakunbound should accept evaluator callable name");
    let throw_bound = builtin_fboundp(&mut eval, vec![Value::symbol("throw")])
        .expect("fboundp should accept evaluator callable name");
    assert!(throw_bound.is_nil());
    let throw_fn = builtin_symbol_function(&mut eval, vec![Value::symbol("throw")])
        .expect("symbol-function should return nil after fmakunbound evaluator callable");
    assert!(throw_fn.is_nil());
    let throw_functionp = builtin_functionp_eval(&mut eval, vec![Value::symbol("throw")])
        .expect("functionp should accept symbol");
    assert!(throw_functionp.is_nil());
    for name in ["funcall", "defalias", "provide", "require"] {
        builtin_fmakunbound(&mut eval, vec![Value::symbol(name)])
            .unwrap_or_else(|_| panic!("fmakunbound should accept {name}"));
        let bound = builtin_fboundp(&mut eval, vec![Value::symbol(name)])
            .unwrap_or_else(|_| panic!("fboundp should accept {name}"));
        assert!(
            bound.is_nil(),
            "expected {name} to be unbound after fmakunbound"
        );
        let fn_cell = builtin_symbol_function(&mut eval, vec![Value::symbol(name)])
            .unwrap_or_else(|_| panic!("symbol-function should accept {name}"));
        assert!(
            fn_cell.is_nil(),
            "expected symbol-function {name} to be nil"
        );
        let functionp = builtin_functionp_eval(&mut eval, vec![Value::symbol(name)])
            .unwrap_or_else(|_| panic!("functionp should accept {name}"));
        assert!(
            functionp.is_nil(),
            "expected functionp {name} to be nil after fmakunbound"
        );
    }
}

#[test]
fn fset_nil_clears_fboundp_for_regular_and_fallback_names() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let regular = builtin_fset(&mut eval, vec![Value::symbol("vm-fsetnil"), Value::Nil])
        .expect("fset should accept nil definition payload");
    assert!(regular.is_nil());
    let regular_bound = builtin_fboundp(&mut eval, vec![Value::symbol("vm-fsetnil")])
        .expect("fboundp should accept symbol");
    assert!(regular_bound.is_nil());
    let regular_fn = builtin_symbol_function(&mut eval, vec![Value::symbol("vm-fsetnil")])
        .expect("symbol-function should accept symbol");
    assert!(regular_fn.is_nil());

    let fallback = builtin_fset(&mut eval, vec![Value::symbol("length"), Value::Nil])
        .expect("fset should accept nil for fallback builtin name");
    assert!(fallback.is_nil());
    let fallback_bound = builtin_fboundp(&mut eval, vec![Value::symbol("length")])
        .expect("fboundp should honor explicit nil function cell");
    assert!(fallback_bound.is_nil());
}

#[test]
fn func_arity_eval_resolves_symbol_designators_and_nil_cells() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let keyword = Value::keyword(":vm-func-arity-keyword");
    let vm_nil = Value::symbol("vm-func-arity-nil-cell");
    let orig_t = builtin_symbol_function(&mut eval, vec![Value::True])
        .expect("symbol-function should read t function cell");
    let orig_keyword = builtin_symbol_function(&mut eval, vec![keyword])
        .expect("symbol-function should read keyword function cell");
    let orig_vm_nil = builtin_symbol_function(&mut eval, vec![vm_nil])
        .expect("symbol-function should read symbol function cell");

    builtin_fset(&mut eval, vec![Value::True, Value::symbol("car")])
        .expect("fset should bind t function cell");
    builtin_fset(&mut eval, vec![keyword, Value::symbol("car")])
        .expect("fset should bind keyword function cell");
    builtin_fset(&mut eval, vec![vm_nil, Value::Nil])
        .expect("fset should bind explicit nil function cell");

    let t_arity = builtin_func_arity_eval(&mut eval, vec![Value::True])
        .expect("func-arity should resolve t designator");
    match &t_arity {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert_eq!(pair.car, Value::Int(1));
            assert_eq!(pair.cdr, Value::Int(1));
        }
        other => panic!("expected cons arity pair, got {other:?}"),
    }

    let keyword_arity = builtin_func_arity_eval(&mut eval, vec![keyword])
        .expect("func-arity should resolve keyword designator");
    match &keyword_arity {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert_eq!(pair.car, Value::Int(1));
            assert_eq!(pair.cdr, Value::Int(1));
        }
        other => panic!("expected cons arity pair, got {other:?}"),
    }

    let nil_cell_err = builtin_func_arity_eval(&mut eval, vec![vm_nil])
        .expect_err("func-arity should signal void-function for nil function cell");
    match nil_cell_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "void-function");
            assert_eq!(sig.data, vec![vm_nil]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    builtin_fset(&mut eval, vec![Value::True, orig_t]).expect("restore t function cell");
    builtin_fset(&mut eval, vec![keyword, orig_keyword]).expect("restore keyword function cell");
    builtin_fset(&mut eval, vec![vm_nil, orig_vm_nil]).expect("restore symbol function cell");
}

#[test]
fn indirect_function_follows_t_and_keyword_alias_values() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let keyword = Value::keyword(":vm-indirect-keyword-alias");
    let t_alias = Value::symbol("vm-indirect-through-t");
    let keyword_alias = Value::symbol("vm-indirect-through-keyword");
    let orig_t = builtin_symbol_function(&mut eval, vec![Value::True])
        .expect("symbol-function should read t function cell");
    let orig_keyword = builtin_symbol_function(&mut eval, vec![keyword])
        .expect("symbol-function should read keyword function cell");
    let orig_t_alias = builtin_symbol_function(&mut eval, vec![t_alias])
        .expect("symbol-function should read alias function cell");
    let orig_keyword_alias = builtin_symbol_function(&mut eval, vec![keyword_alias])
        .expect("symbol-function should read alias function cell");

    builtin_fset(&mut eval, vec![Value::True, Value::symbol("car")])
        .expect("fset should bind t function cell");
    builtin_fset(&mut eval, vec![keyword, Value::symbol("car")])
        .expect("fset should bind keyword function cell");
    builtin_fset(&mut eval, vec![t_alias, Value::True])
        .expect("fset should bind alias to t symbol designator");
    builtin_fset(&mut eval, vec![keyword_alias, keyword])
        .expect("fset should bind alias to keyword designator");

    let resolved_t_alias = builtin_indirect_function(&mut eval, vec![t_alias])
        .expect("indirect-function should resolve alias through t");
    assert_eq!(resolved_t_alias, Value::Subr(intern("car")));
    let resolved_keyword_alias = builtin_indirect_function(&mut eval, vec![keyword_alias])
        .expect("indirect-function should resolve alias through keyword");
    assert_eq!(resolved_keyword_alias, Value::Subr(intern("car")));

    builtin_fset(&mut eval, vec![Value::True, orig_t]).expect("restore t function cell");
    builtin_fset(&mut eval, vec![keyword, orig_keyword]).expect("restore keyword function cell");
    builtin_fset(&mut eval, vec![t_alias, orig_t_alias]).expect("restore alias function cell");
    builtin_fset(&mut eval, vec![keyword_alias, orig_keyword_alias])
        .expect("restore alias function cell");
}

#[test]
fn macrop_eval_resolves_keyword_designators() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let keyword = Value::keyword(":vm-macrop-keyword");
    let orig_keyword = builtin_symbol_function(&mut eval, vec![keyword])
        .expect("symbol-function should read keyword function cell");
    let when_macro = builtin_symbol_function(&mut eval, vec![Value::symbol("when")])
        .expect("symbol-function should read when macro");

    builtin_fset(&mut eval, vec![keyword, when_macro])
        .expect("fset should bind keyword function cell");
    let keyword_result = builtin_macrop_eval(&mut eval, vec![keyword])
        .expect("macrop should resolve keyword designator");
    assert!(keyword_result.is_truthy());

    builtin_fset(&mut eval, vec![keyword, orig_keyword]).expect("restore keyword function cell");
}

#[test]
fn macroexpand_runtime_environment_overrides_and_shadows_global_macros() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let env_lambda = Value::list(vec![Value::list(vec![
        Value::symbol("vm-env"),
        Value::symbol("lambda"),
        Value::list(vec![Value::symbol("x")]),
        Value::list(vec![
            Value::symbol("list"),
            Value::list(vec![Value::symbol("quote"), Value::symbol("when")]),
            Value::symbol("x"),
            Value::Int(1),
        ]),
    ])]);
    let expanded = builtin_macroexpand_eval(
        &mut eval,
        vec![
            Value::list(vec![Value::symbol("vm-env"), Value::True]),
            env_lambda,
        ],
    )
    .expect("macroexpand should apply lambda environment expanders");
    assert_eq!(
        expanded,
        Value::list(vec![
            Value::symbol("if"),
            Value::True,
            Value::list(vec![Value::symbol("progn"), Value::Int(1)]),
        ])
    );

    let shadow = builtin_macroexpand_eval(
        &mut eval,
        vec![
            Value::list(vec![Value::symbol("when"), Value::True, Value::Int(1)]),
            Value::list(vec![Value::list(vec![Value::symbol("when")])]),
        ],
    )
    .expect("environment shadow entries should suppress global macro expansion");
    assert_eq!(
        shadow,
        Value::list(vec![Value::symbol("when"), Value::True, Value::Int(1)])
    );
}

#[test]
fn macroexpand_runtime_environment_type_and_payload_edges_match_oracle() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let atom_ignores_bad_env =
        builtin_macroexpand_eval(&mut eval, vec![Value::symbol("x"), Value::Int(1)])
            .expect("non-list forms should ignore non-list environments");
    assert_eq!(atom_ignores_bad_env, Value::symbol("x"));

    let nonsymbol_head_ignores_bad_env = builtin_macroexpand_eval(
        &mut eval,
        vec![
            Value::list(vec![
                Value::list(vec![Value::symbol("lambda")]),
                Value::Int(1),
            ]),
            Value::Int(1),
        ],
    )
    .expect("list forms without symbol heads should ignore non-list env");
    assert_eq!(
        nonsymbol_head_ignores_bad_env,
        Value::list(vec![
            Value::list(vec![Value::symbol("lambda")]),
            Value::Int(1)
        ])
    );

    let env_type_err = builtin_macroexpand_eval(
        &mut eval,
        vec![
            Value::list(vec![Value::symbol("foo"), Value::Int(1)]),
            Value::Int(1),
        ],
    )
    .expect_err("symbol-headed forms should validate environment list-ness");
    match env_type_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("listp"), Value::Int(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let invalid_env_function = builtin_macroexpand_eval(
        &mut eval,
        vec![
            Value::list(vec![Value::symbol("vm-f"), Value::Int(1)]),
            Value::list(vec![Value::cons(Value::symbol("vm-f"), Value::Int(42))]),
        ],
    )
    .expect_err("environment entries with non-callables should surface invalid-function");
    match invalid_env_function {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "invalid-function");
            assert_eq!(sig.data, vec![Value::Int(42)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn macroexpand_runtime_improper_lists_match_oracle_error_behavior() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let not_macro = builtin_macroexpand_eval(
        &mut eval,
        vec![Value::cons(Value::symbol("foo"), Value::Int(1))],
    )
    .expect("non-macro improper forms should pass through unchanged");
    assert_eq!(not_macro, Value::cons(Value::symbol("foo"), Value::Int(1)));

    let improper_macro = builtin_macroexpand_eval(
        &mut eval,
        vec![Value::cons(Value::symbol("when"), Value::Int(1))],
    )
    .expect_err("macro expansion should reject improper argument lists");
    match improper_macro {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("listp"), Value::Int(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn indirect_function_nil_and_non_symbol_behavior() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let noerror = builtin_indirect_function(
        &mut eval,
        vec![Value::symbol("definitely-not-a-function"), Value::True],
    )
    .expect("indirect-function should return nil when noerror is non-nil");
    assert!(noerror.is_nil());

    let unresolved =
        builtin_indirect_function(&mut eval, vec![Value::symbol("definitely-not-a-function")])
            .expect("indirect-function should return nil for unresolved function");
    assert!(unresolved.is_nil());

    let nil_input = builtin_indirect_function(&mut eval, vec![Value::Nil])
        .expect("indirect-function should return nil for nil input");
    assert!(nil_input.is_nil());

    let true_input = builtin_indirect_function(&mut eval, vec![Value::True])
        .expect("indirect-function should treat t as a symbol and return nil");
    assert!(true_input.is_nil());

    let keyword_input = builtin_indirect_function(
        &mut eval,
        vec![Value::Keyword(intern(":vm-indirect-keyword"))],
    )
    .expect("indirect-function should treat keywords as symbols and return nil");
    assert!(keyword_input.is_nil());

    let passthrough = builtin_indirect_function(&mut eval, vec![Value::Int(42)])
        .expect("non-symbol should be returned as-is");
    assert_eq!(passthrough, Value::Int(42));
}

#[test]
fn indirect_function_rejects_overflow_arity() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let err = builtin_indirect_function(
        &mut eval,
        vec![Value::symbol("ignore"), Value::Nil, Value::Nil],
    )
    .expect_err("indirect-function should reject more than two arguments");
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn indirect_function_resolves_deep_alias_chain_without_depth_cutoff() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let depth = 120;

    for idx in 0..depth {
        let name = format!("vm-test-deep-alias-{idx}");
        let target = if idx == depth - 1 {
            Value::symbol("car")
        } else {
            Value::symbol(format!("vm-test-deep-alias-{}", idx + 1))
        };
        eval.obarray_mut().set_symbol_function(&name, target);
    }

    let resolved =
        builtin_indirect_function(&mut eval, vec![Value::symbol("vm-test-deep-alias-0")])
            .expect("indirect-function should resolve deep alias chains");
    assert_eq!(resolved, Value::Subr(intern("car")));
}

#[test]
fn fset_rejects_self_alias_cycle() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let err = builtin_fset(
        &mut eval,
        vec![
            Value::symbol("vm-test-fset-cycle-self"),
            Value::symbol("vm-test-fset-cycle-self"),
        ],
    )
    .expect_err("fset should reject self-referential alias cycles");
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "cyclic-function-indirection");
            assert_eq!(sig.data, vec![Value::symbol("vm-test-fset-cycle-self")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let unresolved =
        builtin_indirect_function(&mut eval, vec![Value::symbol("vm-test-fset-cycle-self")])
            .expect("indirect-function should still resolve");
    assert!(unresolved.is_nil());
}

#[test]
fn fset_rejects_two_node_alias_cycle() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let first = builtin_fset(
        &mut eval,
        vec![
            Value::symbol("vm-test-fset-cycle-a"),
            Value::symbol("vm-test-fset-cycle-b"),
        ],
    )
    .expect("first alias should be accepted");
    assert_eq!(first, Value::symbol("vm-test-fset-cycle-b"));

    let err = builtin_fset(
        &mut eval,
        vec![
            Value::symbol("vm-test-fset-cycle-b"),
            Value::symbol("vm-test-fset-cycle-a"),
        ],
    )
    .expect_err("fset should reject second edge that closes alias cycle");
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "cyclic-function-indirection");
            assert_eq!(sig.data, vec![Value::symbol("vm-test-fset-cycle-b")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn fset_rejects_keyword_and_t_alias_cycles() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let first = builtin_fset(
        &mut eval,
        vec![Value::keyword(":vmk2"), Value::keyword(":vmk3")],
    )
    .expect("first keyword alias should be accepted");
    assert_eq!(first, Value::keyword(":vmk3"));

    let keyword_cycle = builtin_fset(
        &mut eval,
        vec![Value::keyword(":vmk3"), Value::keyword(":vmk2")],
    )
    .expect_err("second keyword edge should close cycle");
    match keyword_cycle {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "cyclic-function-indirection");
            assert_eq!(sig.data, vec![Value::symbol(":vmk3")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    builtin_fset(&mut eval, vec![Value::True, Value::keyword(":vmk")])
        .expect("fset should allow binding t to keyword alias");

    let t_cycle = builtin_fset(&mut eval, vec![Value::keyword(":vmk"), Value::True])
        .expect_err("keyword->t edge should be rejected when t->keyword exists");
    match t_cycle {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "cyclic-function-indirection");
            assert_eq!(sig.data, vec![Value::symbol(":vmk")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn fset_nil_signals_setting_constant() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let err = builtin_fset(&mut eval, vec![Value::Nil, Value::symbol("car")])
        .expect_err("fset should reject writing nil's function cell");
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "setting-constant");
            assert_eq!(sig.data, vec![Value::symbol("nil")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn fset_t_accepts_symbol_cell_updates() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let result = builtin_fset(&mut eval, vec![Value::True, Value::symbol("car")])
        .expect("fset should allow writing t's function cell");
    assert_eq!(result, Value::symbol("car"));

    let resolved = builtin_indirect_function(&mut eval, vec![Value::True])
        .expect("indirect-function should resolve t after fset");
    assert_eq!(resolved, Value::Subr(intern("car")));
}

#[test]
fn keyword_symbols_are_bound_and_special_constants() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let keyword = Value::keyword(":vm-bound-keyword");

    let bound = builtin_boundp(&mut eval, vec![keyword]).expect("boundp should accept keyword");
    assert!(bound.is_truthy());

    let default_bound = builtin_default_boundp(&mut eval, vec![keyword])
        .expect("default-boundp should accept keyword");
    assert!(default_bound.is_truthy());

    let special = builtin_special_variable_p(&mut eval, vec![keyword])
        .expect("special-variable-p should accept keyword");
    assert!(special.is_truthy());
}

#[test]
fn defvaralias_and_indirect_variable_follow_runtime_aliases() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let aliased = builtin_defvaralias_eval(
        &mut eval,
        vec![
            Value::symbol("vm-defvaralias-new"),
            Value::symbol("vm-defvaralias-old"),
            Value::string("vm-doc"),
        ],
    )
    .expect("defvaralias should succeed");
    assert_eq!(aliased, Value::symbol("vm-defvaralias-old"));

    let doc = builtin_get(
        &mut eval,
        vec![
            Value::symbol("vm-defvaralias-new"),
            Value::symbol("variable-documentation"),
        ],
    )
    .expect("get should return variable doc");
    assert_eq!(doc, Value::string("vm-doc"));

    let direct =
        builtin_indirect_variable_eval(&mut eval, vec![Value::symbol("vm-defvaralias-new")])
            .expect("indirect-variable should resolve aliases");
    assert_eq!(direct, Value::symbol("vm-defvaralias-old"));

    let special_new =
        builtin_special_variable_p(&mut eval, vec![Value::symbol("vm-defvaralias-new")])
            .expect("special-variable-p should accept alias");
    assert!(special_new.is_truthy());
    let special_old =
        builtin_special_variable_p(&mut eval, vec![Value::symbol("vm-defvaralias-old")])
            .expect("special-variable-p should mark target special");
    assert!(special_old.is_truthy());

    let set_value = builtin_set(
        &mut eval,
        vec![Value::symbol("vm-defvaralias-new"), Value::Int(7)],
    )
    .expect("set should assign through aliases");
    assert_eq!(set_value, Value::Int(7));
    let old_value = builtin_symbol_value(&mut eval, vec![Value::symbol("vm-defvaralias-old")])
        .expect("symbol-value should read aliased target");
    assert_eq!(old_value, Value::Int(7));

    builtin_defvaralias_eval(
        &mut eval,
        vec![
            Value::symbol("vm-defvaralias-new"),
            Value::symbol("vm-defvaralias-old"),
        ],
    )
    .expect("defvaralias without doc should clear variable-documentation");
    let cleared_doc = builtin_get(
        &mut eval,
        vec![
            Value::symbol("vm-defvaralias-new"),
            Value::symbol("variable-documentation"),
        ],
    )
    .expect("get should read cleared documentation");
    assert!(cleared_doc.is_nil());

    let unbound = builtin_makunbound(&mut eval, vec![Value::symbol("vm-defvaralias-new")])
        .expect("makunbound should clear target through alias");
    assert_eq!(unbound, Value::symbol("vm-defvaralias-new"));
    let bound_old = builtin_boundp(&mut eval, vec![Value::symbol("vm-defvaralias-old")])
        .expect("boundp should read aliased target");
    assert!(bound_old.is_nil());
}

#[test]
fn variable_watchers_observe_set_setq_and_makunbound() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    install_variable_watcher_probe(&mut eval, "vm-watcher-probe");

    super::super::advice::builtin_add_variable_watcher(
        &mut eval,
        vec![
            Value::symbol("vm-watcher-target"),
            Value::symbol("vm-watcher-probe"),
        ],
    )
    .expect("add-variable-watcher should register callback");

    builtin_set(
        &mut eval,
        vec![Value::symbol("vm-watcher-target"), Value::Int(7)],
    )
    .expect("set should trigger watcher");
    let set_op = builtin_symbol_value(&mut eval, vec![Value::symbol("vm-watcher-last-op")])
        .expect("watcher should record operation");
    let set_val = builtin_symbol_value(&mut eval, vec![Value::symbol("vm-watcher-last-value")])
        .expect("watcher should record value");
    assert_eq!(set_op, Value::symbol("set"));
    assert_eq!(set_val, Value::Int(7));

    eval.eval_expr(&Expr::List(vec![
        Expr::Symbol(intern("setq")),
        Expr::Symbol(intern("vm-watcher-target")),
        Expr::Int(11),
    ]))
    .expect("setq should trigger watcher");
    let setq_op = builtin_symbol_value(&mut eval, vec![Value::symbol("vm-watcher-last-op")])
        .expect("watcher should record setq operation");
    let setq_val = builtin_symbol_value(&mut eval, vec![Value::symbol("vm-watcher-last-value")])
        .expect("watcher should record setq value");
    assert_eq!(setq_op, Value::symbol("set"));
    assert_eq!(setq_val, Value::Int(11));

    builtin_makunbound(&mut eval, vec![Value::symbol("vm-watcher-target")])
        .expect("makunbound should trigger watcher");
    let unbind_op = builtin_symbol_value(&mut eval, vec![Value::symbol("vm-watcher-last-op")])
        .expect("watcher should record makunbound operation");
    let unbind_val = builtin_symbol_value(&mut eval, vec![Value::symbol("vm-watcher-last-value")])
        .expect("watcher should record makunbound value");
    assert_eq!(unbind_op, Value::symbol("makunbound"));
    assert!(unbind_val.is_nil());
}

#[test]
fn variable_watchers_observe_set_default_toplevel_value() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    install_variable_watcher_probe(&mut eval, "vm-watcher-probe");

    super::super::advice::builtin_add_variable_watcher(
        &mut eval,
        vec![
            Value::symbol("vm-watcher-default-target"),
            Value::symbol("vm-watcher-probe"),
        ],
    )
    .expect("add-variable-watcher should register callback");

    builtin_set_default_toplevel_value(
        &mut eval,
        vec![Value::symbol("vm-watcher-default-target"), Value::Int(23)],
    )
    .expect("set-default-toplevel-value should trigger watcher");
    let op = builtin_symbol_value(&mut eval, vec![Value::symbol("vm-watcher-last-op")])
        .expect("watcher should record operation");
    let val = builtin_symbol_value(&mut eval, vec![Value::symbol("vm-watcher-last-value")])
        .expect("watcher should record value");
    assert_eq!(op, Value::symbol("set"));
    assert_eq!(val, Value::Int(23));
}

#[test]
fn defvaralias_triggers_variable_watchers_and_clears_alias_entry() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    install_variable_watcher_probe(&mut eval, "vm-defvaralias-watch-probe");

    super::super::advice::builtin_add_variable_watcher(
        &mut eval,
        vec![
            Value::symbol("vm-defvaralias-watch-new"),
            Value::symbol("vm-defvaralias-watch-probe"),
        ],
    )
    .expect("add-variable-watcher should register callback");

    builtin_defvaralias_eval(
        &mut eval,
        vec![
            Value::symbol("vm-defvaralias-watch-new"),
            Value::symbol("vm-defvaralias-watch-old"),
        ],
    )
    .expect("defvaralias should trigger watcher callback");

    let symbol = builtin_symbol_value(&mut eval, vec![Value::symbol("vm-watcher-last-symbol")])
        .expect("watcher should record watched symbol");
    let op = builtin_symbol_value(&mut eval, vec![Value::symbol("vm-watcher-last-op")])
        .expect("watcher should record defvaralias operation");
    let value = builtin_symbol_value(&mut eval, vec![Value::symbol("vm-watcher-last-value")])
        .expect("watcher should record aliased target");
    assert_eq!(symbol, Value::symbol("vm-defvaralias-watch-new"));
    assert_eq!(op, Value::symbol("defvaralias"));
    assert_eq!(value, Value::symbol("vm-defvaralias-watch-old"));

    let remaining = super::super::advice::builtin_get_variable_watchers(
        &mut eval,
        vec![Value::symbol("vm-defvaralias-watch-new")],
    )
    .expect("get-variable-watchers should return alias watcher list");
    assert!(remaining.is_nil());
}

#[test]
fn defvaralias_raw_plist_errors_skip_variable_watcher_callbacks() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    install_variable_watcher_probe(&mut eval, "vm-defvaralias-watch-probe");

    super::super::advice::builtin_add_variable_watcher(
        &mut eval,
        vec![
            Value::symbol("vm-defvaralias-watch-bad"),
            Value::symbol("vm-defvaralias-watch-probe"),
        ],
    )
    .expect("add-variable-watcher should register callback");

    builtin_setplist_eval(
        &mut eval,
        vec![Value::symbol("vm-defvaralias-watch-bad"), Value::Int(1)],
    )
    .expect("setplist should install malformed raw plist");

    let err = builtin_defvaralias_eval(
        &mut eval,
        vec![
            Value::symbol("vm-defvaralias-watch-bad"),
            Value::symbol("vm-defvaralias-watch-target"),
        ],
    )
    .expect_err("defvaralias should preserve plistp error");
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("plistp"), Value::Int(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let callback_state = builtin_boundp(&mut eval, vec![Value::symbol("vm-watcher-last-op")])
        .expect("boundp should report watcher state symbol");
    assert!(callback_state.is_nil());
}

#[test]
fn defvaralias_repoint_notifies_previous_alias_target_watchers() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    install_variable_watcher_probe(&mut eval, "vm-defvaralias-repoint-watch");

    builtin_defvaralias_eval(
        &mut eval,
        vec![
            Value::symbol("vm-defvaralias-repoint-alias"),
            Value::symbol("vm-defvaralias-repoint-old"),
        ],
    )
    .expect("first defvaralias should install initial alias");

    super::super::advice::builtin_add_variable_watcher(
        &mut eval,
        vec![
            Value::symbol("vm-defvaralias-repoint-alias"),
            Value::symbol("vm-defvaralias-repoint-watch"),
        ],
    )
    .expect("add-variable-watcher should resolve alias to old target");

    builtin_defvaralias_eval(
        &mut eval,
        vec![
            Value::symbol("vm-defvaralias-repoint-alias"),
            Value::symbol("vm-defvaralias-repoint-new"),
        ],
    )
    .expect("second defvaralias should trigger previous-target watcher");

    let symbol = builtin_symbol_value(&mut eval, vec![Value::symbol("vm-watcher-last-symbol")])
        .expect("watcher should record previous alias target");
    let op = builtin_symbol_value(&mut eval, vec![Value::symbol("vm-watcher-last-op")])
        .expect("watcher should record operation");
    let value = builtin_symbol_value(&mut eval, vec![Value::symbol("vm-watcher-last-value")])
        .expect("watcher should record new alias target");
    assert_eq!(symbol, Value::symbol("vm-defvaralias-repoint-old"));
    assert_eq!(op, Value::symbol("defvaralias"));
    assert_eq!(value, Value::symbol("vm-defvaralias-repoint-new"));
}

#[test]
fn defvaralias_rejects_invalid_inputs_and_cycles() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let constant_err = builtin_defvaralias_eval(
        &mut eval,
        vec![Value::symbol("nil"), Value::symbol("vm-defvaralias-x")],
    )
    .expect_err("defvaralias should reject constant aliases");
    match constant_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Cannot make a constant an alias: nil")]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let type_err = builtin_defvaralias_eval(
        &mut eval,
        vec![Value::symbol("vm-defvaralias-bad"), Value::Int(1)],
    )
    .expect_err("defvaralias should validate OLD-BASE");
    match type_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("symbolp"), Value::Int(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    builtin_defvaralias_eval(
        &mut eval,
        vec![
            Value::symbol("vm-defvaralias-a"),
            Value::symbol("vm-defvaralias-b"),
        ],
    )
    .expect("first alias edge should succeed");
    let cycle_err = builtin_defvaralias_eval(
        &mut eval,
        vec![
            Value::symbol("vm-defvaralias-b"),
            Value::symbol("vm-defvaralias-a"),
        ],
    )
    .expect_err("second alias edge should be rejected as a cycle");
    match cycle_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "cyclic-variable-indirection");
            assert_eq!(sig.data, vec![Value::symbol("vm-defvaralias-a")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn setplist_runtime_controls_get_put_and_symbol_plist_edges() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let initial_plist = Value::list(vec![
        Value::symbol("a"),
        Value::Int(1),
        Value::symbol("b"),
        Value::Int(2),
    ]);
    let stored =
        builtin_setplist_eval(&mut eval, vec![Value::symbol("vm-setplist"), initial_plist])
            .expect("setplist should store plist values");
    assert_eq!(stored, initial_plist);

    let read_plist = builtin_symbol_plist_fn(&mut eval, vec![Value::symbol("vm-setplist")])
        .expect("symbol-plist should return stored raw plist");
    assert_eq!(
        read_plist,
        Value::list(vec![
            Value::symbol("a"),
            Value::Int(1),
            Value::symbol("b"),
            Value::Int(2),
        ])
    );

    let lookup = builtin_get(
        &mut eval,
        vec![Value::symbol("vm-setplist"), Value::symbol("a")],
    )
    .expect("get should read entries from raw plist");
    assert_eq!(lookup, Value::Int(1));

    let put = builtin_put(
        &mut eval,
        vec![
            Value::symbol("vm-setplist"),
            Value::symbol("a"),
            Value::Int(5),
        ],
    )
    .expect("put should update raw plist entries");
    assert_eq!(put, Value::Int(5));
    let updated = builtin_symbol_plist_fn(&mut eval, vec![Value::symbol("vm-setplist")])
        .expect("symbol-plist should reflect updated plist values");
    assert_eq!(
        updated,
        Value::list(vec![
            Value::symbol("a"),
            Value::Int(5),
            Value::symbol("b"),
            Value::Int(2),
        ])
    );

    builtin_setplist_eval(&mut eval, vec![Value::symbol("vm-setplist"), Value::Int(1)])
        .expect("setplist should accept non-list plist values");
    let non_list = builtin_symbol_plist_fn(&mut eval, vec![Value::symbol("vm-setplist")])
        .expect("symbol-plist should return raw non-list values");
    assert_eq!(non_list, Value::Int(1));

    let missing = builtin_get(
        &mut eval,
        vec![Value::symbol("vm-setplist"), Value::symbol("a")],
    )
    .expect("get should treat non-list plist as missing keys");
    assert!(missing.is_nil());

    let put_err = builtin_put(
        &mut eval,
        vec![
            Value::symbol("vm-setplist"),
            Value::symbol("a"),
            Value::Int(8),
        ],
    )
    .expect_err("put should fail on non-plist raw values");
    match put_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("plistp"), Value::Int(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn put_promotes_symbol_properties_to_live_raw_plists() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let sym = Value::symbol("vm-live-plist");

    builtin_put(
        &mut eval,
        vec![sym, Value::symbol("type"), Value::symbol("float")],
    )
    .expect("first put should succeed");
    builtin_put(
        &mut eval,
        vec![sym, Value::symbol("doc"), Value::string("A z value")],
    )
    .expect("second put should succeed");

    let plist = builtin_symbol_plist_fn(&mut eval, vec![sym])
        .expect("symbol-plist should return a live plist object");

    builtin_put(&mut eval, vec![sym, Value::symbol("type"), Value::Nil])
        .expect("put should mutate the live plist in place");
    builtin_put(&mut eval, vec![sym, Value::symbol("doc"), Value::Nil])
        .expect("put should mutate the live plist in place");

    assert_eq!(
        plist,
        Value::list(vec![
            Value::symbol("type"),
            Value::Nil,
            Value::symbol("doc"),
            Value::Nil,
        ])
    );
}

#[test]
fn register_code_conversion_map_publishes_symbol_properties() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let map = Value::vector(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);

    let map_id = dispatch_builtin(
        &mut eval,
        "register-code-conversion-map",
        vec![Value::symbol("vm-ccl-map-prop"), map],
    )
    .expect("register-code-conversion-map should dispatch")
    .expect("register-code-conversion-map should succeed");
    let map_id_value = match map_id {
        Value::Int(id) => {
            assert!(id >= 0);
            Value::Int(id)
        }
        other => panic!("expected integer map id, got {other:?}"),
    };

    let published_map = builtin_get(
        &mut eval,
        vec![
            Value::symbol("vm-ccl-map-prop"),
            Value::symbol("code-conversion-map"),
        ],
    )
    .expect("get should read published conversion map");
    assert_eq!(published_map, map);

    let published_id = builtin_get(
        &mut eval,
        vec![
            Value::symbol("vm-ccl-map-prop"),
            Value::symbol("code-conversion-map-id"),
        ],
    )
    .expect("get should read published conversion map id");
    assert_eq!(published_id, map_id_value);

    let sym_value = builtin_symbol_value(&mut eval, vec![Value::symbol("vm-ccl-map-prop")])
        .expect_err("register-code-conversion-map should not bind symbol value");
    match sym_value {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "void-variable");
            assert_eq!(sig.data, vec![Value::symbol("vm-ccl-map-prop")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn register_ccl_program_publishes_symbol_properties() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let program = Value::vector(vec![Value::Int(10), Value::Int(0), Value::Int(0)]);

    let program_id = dispatch_builtin(
        &mut eval,
        "register-ccl-program",
        vec![Value::symbol("vm-ccl-program-prop"), program],
    )
    .expect("register-ccl-program should dispatch")
    .expect("register-ccl-program should succeed");
    let program_id_value = match program_id {
        Value::Int(id) => {
            assert!(id > 0);
            Value::Int(id)
        }
        other => panic!("expected integer program id, got {other:?}"),
    };

    let published_id = builtin_get(
        &mut eval,
        vec![
            Value::symbol("vm-ccl-program-prop"),
            Value::symbol("ccl-program-idx"),
        ],
    )
    .expect("get should read published CCL program id");
    assert_eq!(published_id, program_id_value);

    let unpublished_program = builtin_get(
        &mut eval,
        vec![
            Value::symbol("vm-ccl-program-prop"),
            Value::symbol("ccl-program"),
        ],
    )
    .expect("get should return nil for ccl-program property");
    assert_eq!(unpublished_program, Value::Nil);

    let sym_value = builtin_symbol_value(&mut eval, vec![Value::symbol("vm-ccl-program-prop")])
        .expect_err("register-ccl-program should not bind symbol value");
    match sym_value {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "void-variable");
            assert_eq!(sig.data, vec![Value::symbol("vm-ccl-program-prop")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn ccl_symbol_designators_follow_plist_idx_gates() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let _ = builtin_put(
        &mut eval,
        vec![
            Value::symbol("vm-ccl-manual-programp"),
            Value::symbol("ccl-program-idx"),
            Value::Int(1),
        ],
    )
    .expect("put should seed ccl-program-idx");
    let manual_programp = dispatch_builtin(
        &mut eval,
        "ccl-program-p",
        vec![Value::symbol("vm-ccl-manual-programp")],
    )
    .expect("ccl-program-p should dispatch")
    .expect("ccl-program-p should evaluate symbol plist idx");
    assert_eq!(manual_programp, Value::True);

    let first_id = dispatch_builtin(
        &mut eval,
        "register-ccl-program",
        vec![
            Value::symbol("vm-ccl-plist-gate"),
            Value::vector(vec![Value::Int(10), Value::Int(0), Value::Int(0)]),
        ],
    )
    .expect("initial register-ccl-program should dispatch")
    .expect("initial register-ccl-program should succeed");

    let _ = builtin_setplist_eval(
        &mut eval,
        vec![Value::symbol("vm-ccl-plist-gate"), Value::Nil],
    )
    .expect("setplist should clear symbol plist");
    let second_id = dispatch_builtin(
        &mut eval,
        "register-ccl-program",
        vec![
            Value::symbol("vm-ccl-plist-gate"),
            Value::vector(vec![
                Value::Int(10),
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
            ]),
        ],
    )
    .expect("re-register should dispatch")
    .expect("re-register should keep existing id");
    assert_eq!(second_id, first_id);

    let missing_idx = builtin_get(
        &mut eval,
        vec![
            Value::symbol("vm-ccl-plist-gate"),
            Value::symbol("ccl-program-idx"),
        ],
    )
    .expect("get should read plist gate");
    assert_eq!(missing_idx, Value::Nil);

    let gated_programp = dispatch_builtin(
        &mut eval,
        "ccl-program-p",
        vec![Value::symbol("vm-ccl-plist-gate")],
    )
    .expect("ccl-program-p should dispatch")
    .expect("ccl-program-p should gate on plist idx");
    assert_eq!(gated_programp, Value::Nil);

    let execute_err = dispatch_builtin(
        &mut eval,
        "ccl-execute",
        vec![
            Value::symbol("vm-ccl-plist-gate"),
            Value::vector(vec![
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
            ]),
        ],
    )
    .expect("ccl-execute should dispatch")
    .expect_err("ccl-execute should treat gated symbol as invalid program");
    match execute_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data, vec![Value::string("Invalid CCL program")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let execute_on_string_err = dispatch_builtin(
        &mut eval,
        "ccl-execute-on-string",
        vec![
            Value::symbol("vm-ccl-plist-gate"),
            Value::vector(vec![
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
            ]),
            Value::string("abc"),
        ],
    )
    .expect("ccl-execute-on-string should dispatch")
    .expect_err("ccl-execute-on-string should treat gated symbol as invalid program");
    match execute_on_string_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data, vec![Value::string("Invalid CCL program")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let _ = builtin_setplist_eval(
        &mut eval,
        vec![Value::symbol("vm-ccl-plist-gate"), Value::Int(1)],
    )
    .expect("setplist should allow malformed plist");
    let malformed_reregister = dispatch_builtin(
        &mut eval,
        "register-ccl-program",
        vec![
            Value::symbol("vm-ccl-plist-gate"),
            Value::vector(vec![
                Value::Int(10),
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
            ]),
        ],
    )
    .expect("malformed re-register should dispatch")
    .expect("malformed re-register should return existing id");
    assert_eq!(malformed_reregister, first_id);
}

#[test]
fn register_code_conversion_map_existing_symbol_plist_edges() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let first_id = dispatch_builtin(
        &mut eval,
        "register-code-conversion-map",
        vec![
            Value::symbol("vm-map-plist-edge"),
            Value::vector(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
        ],
    )
    .expect("register-code-conversion-map should dispatch")
    .expect("initial register-code-conversion-map should succeed");
    assert_eq!(first_id, Value::Int(0));

    let _ = builtin_setplist_eval(
        &mut eval,
        vec![Value::symbol("vm-map-plist-edge"), Value::Nil],
    )
    .expect("setplist should clear plist");

    let second_id = dispatch_builtin(
        &mut eval,
        "register-code-conversion-map",
        vec![
            Value::symbol("vm-map-plist-edge"),
            Value::vector(vec![Value::Int(4), Value::Int(5), Value::Int(6)]),
        ],
    )
    .expect("register-code-conversion-map should dispatch after plist clear")
    .expect("register-code-conversion-map should keep id after plist clear");
    assert_eq!(second_id, first_id);

    let republished_map = builtin_get(
        &mut eval,
        vec![
            Value::symbol("vm-map-plist-edge"),
            Value::symbol("code-conversion-map"),
        ],
    )
    .expect("get should read republished map");
    assert_eq!(
        republished_map,
        Value::vector(vec![Value::Int(4), Value::Int(5), Value::Int(6)])
    );
    let republished_id = builtin_get(
        &mut eval,
        vec![
            Value::symbol("vm-map-plist-edge"),
            Value::symbol("code-conversion-map-id"),
        ],
    )
    .expect("get should read republished id");
    assert_eq!(republished_id, first_id);

    let _ = builtin_setplist_eval(
        &mut eval,
        vec![Value::symbol("vm-map-plist-edge"), Value::Int(1)],
    )
    .expect("setplist should seed malformed plist");
    let malformed = dispatch_builtin(
        &mut eval,
        "register-code-conversion-map",
        vec![
            Value::symbol("vm-map-plist-edge"),
            Value::vector(vec![Value::Int(7), Value::Int(8), Value::Int(9)]),
        ],
    )
    .expect("register-code-conversion-map malformed path should dispatch")
    .expect_err("malformed plist should preserve plistp error");
    match malformed {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("plistp"), Value::Int(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let hidden_id = builtin_get(
        &mut eval,
        vec![
            Value::symbol("vm-map-plist-edge"),
            Value::symbol("code-conversion-map-id"),
        ],
    )
    .expect("get should read hidden id after malformed plist");
    assert_eq!(hidden_id, Value::Nil);

    let next_id = dispatch_builtin(
        &mut eval,
        "register-code-conversion-map",
        vec![
            Value::symbol("vm-map-plist-edge-next"),
            Value::vector(vec![Value::Int(9), Value::Int(8), Value::Int(7)]),
        ],
    )
    .expect("register-code-conversion-map next should dispatch")
    .expect("register-code-conversion-map next should succeed");
    assert_eq!(next_id, Value::Int(1));
}

#[test]
fn ccl_registration_plist_errors_preserve_oracle_id_side_effects() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    let baseline_program_id = dispatch_builtin(
        &mut eval,
        "register-ccl-program",
        vec![
            Value::symbol("vm-ccl-program-id-baseline"),
            Value::vector(vec![Value::Int(10), Value::Int(0), Value::Int(0)]),
        ],
    )
    .expect("register-ccl-program baseline should dispatch")
    .expect("register-ccl-program baseline should succeed")
    .as_int()
    .expect("baseline program id should be integer");

    builtin_setplist_eval(
        &mut eval,
        vec![Value::symbol("vm-ccl-program-id-bad"), Value::Int(1)],
    )
    .expect("setplist should seed malformed plist");
    let program_err = dispatch_builtin(
        &mut eval,
        "register-ccl-program",
        vec![
            Value::symbol("vm-ccl-program-id-bad"),
            Value::vector(vec![Value::Int(10), Value::Int(0), Value::Int(0)]),
        ],
    )
    .expect("register-ccl-program error path should dispatch")
    .expect_err("register-ccl-program should fail on malformed plist");
    match program_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("plistp"), Value::Int(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
    let bad_program_designator = dispatch_builtin(
        &mut eval,
        "ccl-program-p",
        vec![Value::symbol("vm-ccl-program-id-bad")],
    )
    .expect("ccl-program-p should dispatch")
    .expect("ccl-program-p should return predicate value");
    assert_eq!(bad_program_designator, Value::Nil);

    let next_program_id = dispatch_builtin(
        &mut eval,
        "register-ccl-program",
        vec![
            Value::symbol("vm-ccl-program-id-next"),
            Value::vector(vec![Value::Int(10), Value::Int(0), Value::Int(0)]),
        ],
    )
    .expect("register-ccl-program next should dispatch")
    .expect("register-ccl-program next should succeed")
    .as_int()
    .expect("next program id should be integer");
    assert_eq!(next_program_id, baseline_program_id + 2);

    let baseline_map_id = dispatch_builtin(
        &mut eval,
        "register-code-conversion-map",
        vec![
            Value::symbol("vm-ccl-map-id-baseline"),
            Value::vector(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
        ],
    )
    .expect("register-code-conversion-map baseline should dispatch")
    .expect("register-code-conversion-map baseline should succeed")
    .as_int()
    .expect("baseline map id should be integer");

    builtin_setplist_eval(
        &mut eval,
        vec![Value::symbol("vm-ccl-map-id-bad"), Value::Int(1)],
    )
    .expect("setplist should seed malformed plist");
    let map_err = dispatch_builtin(
        &mut eval,
        "register-code-conversion-map",
        vec![
            Value::symbol("vm-ccl-map-id-bad"),
            Value::vector(vec![Value::Int(4), Value::Int(5), Value::Int(6)]),
        ],
    )
    .expect("register-code-conversion-map error path should dispatch")
    .expect_err("register-code-conversion-map should fail on malformed plist");
    match map_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("plistp"), Value::Int(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let next_map_id = dispatch_builtin(
        &mut eval,
        "register-code-conversion-map",
        vec![
            Value::symbol("vm-ccl-map-id-next"),
            Value::vector(vec![Value::Int(7), Value::Int(8), Value::Int(9)]),
        ],
    )
    .expect("register-code-conversion-map next should dispatch")
    .expect("register-code-conversion-map next should succeed")
    .as_int()
    .expect("next map id should be integer");
    assert_eq!(next_map_id, baseline_map_id + 1);
}

#[test]
fn variable_alias_to_constant_reports_alias_in_setting_constant_errors() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    builtin_defvaralias_eval(
        &mut eval,
        vec![
            Value::symbol("vm-alias-constant"),
            Value::symbol("nil"),
            Value::Nil,
        ],
    )
    .expect("defvaralias should allow aliasing to nil");

    let set_err = builtin_set(
        &mut eval,
        vec![Value::symbol("vm-alias-constant"), Value::Int(1)],
    )
    .expect_err("set should reject writes through nil aliases");
    match set_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "setting-constant");
            assert_eq!(sig.data, vec![Value::symbol("vm-alias-constant")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let default_err = builtin_set_default_toplevel_value(
        &mut eval,
        vec![Value::symbol("vm-alias-constant"), Value::Int(1)],
    )
    .expect_err("set-default-toplevel-value should reject nil aliases");
    match default_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "setting-constant");
            assert_eq!(sig.data, vec![Value::symbol("vm-alias-constant")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let unbind_err = builtin_makunbound(&mut eval, vec![Value::symbol("vm-alias-constant")])
        .expect_err("makunbound should reject nil aliases");
    match unbind_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "setting-constant");
            assert_eq!(sig.data, vec![Value::symbol("vm-alias-constant")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn defvaralias_raises_plistp_errors_when_symbol_plist_is_non_list() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    builtin_setplist_eval(
        &mut eval,
        vec![Value::symbol("vm-defvaralias-bad-plist"), Value::Int(1)],
    )
    .expect("setplist should seed malformed symbol plist value");

    let err = builtin_defvaralias_eval(
        &mut eval,
        vec![
            Value::symbol("vm-defvaralias-bad-plist"),
            Value::symbol("vm-defvaralias-target"),
            Value::string("doc"),
        ],
    )
    .expect_err("defvaralias should preserve put-style plistp failures");
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("plistp"), Value::Int(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let unresolved =
        builtin_indirect_variable_eval(&mut eval, vec![Value::symbol("vm-defvaralias-bad-plist")])
            .expect("failed defvaralias should still install alias edges");
    assert_eq!(unresolved, Value::symbol("vm-defvaralias-target"));
}

#[test]
fn neovm_precompile_file_builtin_writes_cache_and_returns_path() {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-builtin-precompile-ok-{unique}"));
    fs::create_dir_all(&dir).expect("create temp dir");
    let source = dir.join("probe.el");
    fs::write(
        &source,
        ";;; -*- lexical-binding: t; -*-\n(setq vm-builtins-precompile 1)\n",
    )
    .expect("write source");

    let result =
        builtin_neovm_precompile_file(&mut eval, vec![Value::string(source.to_string_lossy())])
            .expect("precompile builtin should succeed");
    let cache_path = result
        .as_str()
        .expect("result should be a string path")
        .to_string();
    assert!(
        cache_path.ends_with(".neoc"),
        "cache path should end with .neoc"
    );
    assert!(
        std::path::Path::new(&cache_path).exists(),
        "cache file should exist"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn neovm_precompile_file_builtin_rejects_compiled_inputs() {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-builtin-precompile-reject-{unique}"));
    fs::create_dir_all(&dir).expect("create temp dir");
    let compiled = dir.join("probe.elc");
    fs::write(&compiled, "compiled").expect("write compiled artifact");

    let err =
        builtin_neovm_precompile_file(&mut eval, vec![Value::string(compiled.to_string_lossy())])
            .expect_err("precompile builtin should reject .elc");
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "file-error"),
        other => panic!("unexpected flow: {other:?}"),
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn get_byte_string_semantics_match_oracle_edges() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    assert_eq!(
        builtin_get_byte(&mut eval, vec![Value::Int(0), Value::string("abc")]).unwrap(),
        Value::Int(97)
    );
    assert_eq!(
        builtin_get_byte(&mut eval, vec![Value::Int(1), Value::string("abc")]).unwrap(),
        Value::Int(98)
    );
    assert_eq!(
        builtin_get_byte(&mut eval, vec![Value::Nil, Value::string("abc")]).unwrap(),
        Value::Int(97)
    );

    let out_of_range =
        builtin_get_byte(&mut eval, vec![Value::Int(3), Value::string("abc")]).unwrap_err();
    match out_of_range {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "args-out-of-range");
            assert_eq!(sig.data, vec![Value::string("abc"), Value::Int(3)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let negative =
        builtin_get_byte(&mut eval, vec![Value::Int(-1), Value::string("abc")]).unwrap_err();
    match negative {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("wholenump"), Value::Int(-1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let non_ascii = builtin_get_byte(&mut eval, vec![Value::Int(0), Value::string("é")])
        .expect_err("multibyte non-byte8 should signal");
    match non_ascii {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Not an ASCII nor an 8-bit character: 233")]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn get_byte_buffer_semantics_match_oracle_edges() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    builtin_erase_buffer(&mut eval, vec![]).unwrap();
    builtin_insert(&mut eval, vec![Value::string("abc")]).unwrap();

    assert_eq!(builtin_get_byte(&mut eval, vec![]).unwrap(), Value::Int(0));
    assert_eq!(
        builtin_get_byte(&mut eval, vec![Value::Int(1)]).unwrap(),
        Value::Int(97)
    );
    assert_eq!(
        builtin_get_byte(&mut eval, vec![Value::Int(2)]).unwrap(),
        Value::Int(98)
    );
    assert_eq!(
        builtin_get_byte(&mut eval, vec![Value::Int(3)]).unwrap(),
        Value::Int(99)
    );

    builtin_goto_char(&mut eval, vec![Value::Int(2)]).unwrap();
    assert_eq!(
        builtin_get_byte(&mut eval, vec![Value::Nil]).unwrap(),
        Value::Int(98)
    );

    let zero = builtin_get_byte(&mut eval, vec![Value::Int(0)]).unwrap_err();
    match zero {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "args-out-of-range");
            assert_eq!(sig.data, vec![Value::Int(0), Value::Int(1), Value::Int(4)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let end = builtin_get_byte(&mut eval, vec![Value::Int(4)]).unwrap_err();
    match end {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "args-out-of-range");
            assert_eq!(sig.data, vec![Value::Int(4), Value::Int(1), Value::Int(4)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn get_byte_unibyte_string_returns_raw_byte_values() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let s = builtin_unibyte_string(vec![Value::Int(255), Value::Int(65)]).unwrap();

    assert_eq!(
        builtin_get_byte(&mut eval, vec![Value::Int(0), s]).unwrap(),
        Value::Int(255)
    );
    assert_eq!(
        builtin_get_byte(&mut eval, vec![Value::Int(1), s]).unwrap(),
        Value::Int(65)
    );
}
