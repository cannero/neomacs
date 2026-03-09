use super::*;
use crate::emacs_core::intern::resolve_sym;
use crate::emacs_core::string_escape::bytes_to_unibyte_storage_string;

#[test]
fn parse_integers() {
    let forms = parse_forms("42 -7 0").unwrap();
    assert_eq!(forms, vec![Expr::Int(42), Expr::Int(-7), Expr::Int(0)]);
}

#[test]
fn parse_floats() {
    let forms = parse_forms("3.14 1e10 .5 -2.5").unwrap();
    assert_eq!(
        forms,
        vec![
            Expr::Float(3.14),
            Expr::Float(1e10),
            Expr::Float(0.5),
            Expr::Float(-2.5),
        ]
    );
}

#[test]
fn parse_emacs_special_float_literals() {
    let forms = parse_forms(
        "0.0e+NaN -0.0e+NaN 1.0e+INF -1.0e+INF 0.0e+INF 0e+NaN 1.0E+INF 1.0e+NaN -2.0e+NaN .5e+NaN -.5e+NaN 1.5e+NaN 0.9e+NaN",
    )
    .unwrap();
    assert_eq!(forms.len(), 13);

    match forms[0] {
        Expr::Float(f) => assert!(f.is_nan() && !f.is_sign_negative()),
        _ => panic!("expected float NaN"),
    }
    match forms[1] {
        Expr::Float(f) => assert!(f.is_nan() && f.is_sign_negative()),
        _ => panic!("expected float negative NaN"),
    }
    match forms[2] {
        Expr::Float(f) => assert!(f.is_infinite() && f.is_sign_positive()),
        _ => panic!("expected +inf"),
    }
    match forms[3] {
        Expr::Float(f) => assert!(f.is_infinite() && f.is_sign_negative()),
        _ => panic!("expected -inf"),
    }
    match forms[4] {
        Expr::Float(f) => assert!(f.is_infinite() && f.is_sign_positive()),
        _ => panic!("expected +inf"),
    }
    match forms[5] {
        Expr::Float(f) => assert!(f.is_nan() && !f.is_sign_negative()),
        _ => panic!("expected float NaN"),
    }
    match forms[6] {
        Expr::Float(f) => assert!(f.is_infinite() && f.is_sign_positive()),
        _ => panic!("expected +inf"),
    }
    match forms[7] {
        Expr::Float(f) => assert!(f.is_nan() && !f.is_sign_negative()),
        _ => panic!("expected NaN payload literal"),
    }
    match forms[8] {
        Expr::Float(f) => assert!(f.is_nan() && f.is_sign_negative()),
        _ => panic!("expected negative NaN payload literal"),
    }
    match forms[9] {
        Expr::Float(f) => assert!(f.is_nan() && !f.is_sign_negative()),
        _ => panic!("expected leading-dot NaN payload literal"),
    }
    match forms[10] {
        Expr::Float(f) => assert!(f.is_nan() && f.is_sign_negative()),
        _ => panic!("expected negative leading-dot NaN payload literal"),
    }
    match forms[11] {
        Expr::Float(f) => assert!(f.is_nan() && !f.is_sign_negative()),
        _ => panic!("expected fractional NaN literal"),
    }
    match forms[12] {
        Expr::Float(f) => assert!(f.is_nan() && !f.is_sign_negative()),
        _ => panic!("expected subunit fractional NaN literal"),
    }
}

#[test]
fn parse_nan_payload_literals_render_to_oracle_shapes() {
    let forms = parse_forms(
        "1.0e+NaN -2.0e+NaN .5e+NaN -.5e+NaN 1.5e+NaN 0.9e+NaN .0e+NaN -.0e+NaN 9007199254740991.0e+NaN 2251799813685248.0e+NaN 4503599627370495.0e+NaN 4503599627370496.0e+NaN -4503599627370496.0e+NaN 9007199254740993.0e+NaN -9007199254740993.0e+NaN",
    )
    .unwrap();
    let rendered: Vec<String> = forms
        .iter()
        .map(crate::emacs_core::expr::print_expr)
        .collect();
    assert_eq!(
        rendered,
        vec![
            "1.0e+NaN",
            "-2.0e+NaN",
            "2251799813685246.0e+NaN",
            "-2251799813685246.0e+NaN",
            "1.0e+NaN",
            "0.0e+NaN",
            "2251799813685246.0e+NaN",
            "-2251799813685246.0e+NaN",
            "2251799813685247.0e+NaN",
            "0.0e+NaN",
            "2251799813685247.0e+NaN",
            "0.0e+NaN",
            "-0.0e+NaN",
            "1.0e+NaN",
            "-1.0e+NaN",
        ]
    );
}

#[test]
fn parse_special_float_plus_and_trailing_dot_literals() {
    let forms = parse_forms("+1.e+NaN -1.e+NaN +.0e+NaN +1.e+INF -.0e+INF +1E+NaN").unwrap();
    let rendered: Vec<String> = forms
        .iter()
        .map(crate::emacs_core::expr::print_expr)
        .collect();
    assert_eq!(
        rendered,
        vec![
            "1.0e+NaN",
            "-1.0e+NaN",
            "2251799813685246.0e+NaN",
            "1.0e+INF",
            "-1.0e+INF",
            "1.0e+NaN",
        ]
    );
}

#[test]
fn parse_invalid_nan_inf_spellings_as_symbols() {
    let forms = parse_forms("0.0e+inf 0.0e+nan 1.0eNaN 1.0eINF").unwrap();
    assert_eq!(
        forms,
        vec![
            Expr::Symbol(intern("0.0e+inf")),
            Expr::Symbol(intern("0.0e+nan")),
            Expr::Symbol(intern("1.0eNaN")),
            Expr::Symbol(intern("1.0eINF")),
        ]
    );
}

#[test]
fn parse_strings() {
    let forms = parse_forms(r#""hello" "world\n" "tab\there" "quote\"d""#).unwrap();
    assert_eq!(
        forms,
        vec![
            Expr::Str("hello".into()),
            Expr::Str("world\n".into()),
            Expr::Str("tab\there".into()),
            Expr::Str("quote\"d".into()),
        ]
    );
}

#[test]
fn parse_string_hex_escape() {
    let forms = parse_forms(r#""\x41""#).unwrap();
    assert_eq!(forms, vec![Expr::Str("A".into())]);
}

#[test]
fn parse_string_octal_raw_bytes_as_unibyte_storage() {
    let forms = parse_forms(r#""\303\251""#).unwrap();
    assert_eq!(
        forms,
        vec![Expr::Str(bytes_to_unibyte_storage_string(&[0xC3, 0xA9]))]
    );
}

#[test]
fn parse_string_two_digit_hex_raw_byte_as_unibyte_storage() {
    let forms = parse_forms(r#""\xE9""#).unwrap();
    assert_eq!(
        forms,
        vec![Expr::Str(bytes_to_unibyte_storage_string(&[0xE9]))]
    );
}

#[test]
fn parse_string_unicode_name_escape_u_plus() {
    let forms = parse_forms(r#""\N{U+2764}""#).unwrap();
    assert_eq!(forms, vec![Expr::Str("\u{2764}".into())]);
}

#[test]
fn parse_char_literals() {
    let forms = parse_forms("?a ?\\n ?\\t").unwrap();
    assert_eq!(
        forms,
        vec![Expr::Char('a'), Expr::Char('\n'), Expr::Char('\t')]
    );
}

#[test]
fn parse_char_literal_single_space_syntax_matches_gnu_emacs() {
    let forms = parse_forms("? x ?\ty").unwrap();
    assert_eq!(forms, vec![Expr::Char(' '), Expr::Char('\t')]);
}

#[test]
fn parse_char_literal_requires_gnu_emacs_delimiter() {
    let err = parse_forms("?child").expect_err("?child should be invalid reader syntax");
    assert_eq!(err.message, "?");
}

#[test]
fn parse_control_char_literals() {
    // GNU Emacs lread.c rules for \C-:
    //   \C-a = 1 (letter → & 0x1F, no ctrl bit)
    //   \C-z = 26 (letter → & 0x1F, no ctrl bit)
    //   \C-@ = 0 (@ has bit 6 → & 0x1F, no ctrl bit)
    //   \C-\0 = 0x4000000 (NUL not in [A-Za-z@-_] → add ctrl bit)
    //   \C-? = 127 (DEL special case)
    //   \C-\C-c = 0x4000003 (inner maps c→3, outer adds ctrl bit)
    let forms = parse_forms("?\\C-a ?\\C-z ?\\C-@ ?\\C-\\0 ?\\C-? ?\\C-\\C-c").unwrap();
    assert_eq!(forms[0], Expr::Char('\x01')); // \C-a = 1
    assert_eq!(forms[1], Expr::Char('\x1A')); // \C-z = 26
    assert_eq!(forms[2], Expr::Char('\x00')); // \C-@ = 0
    assert_eq!(forms[3], Expr::Int(0x4000000)); // \C-\0 = CHAR_CTL
    assert_eq!(forms[4], Expr::Char('\x7F')); // \C-? = DEL
    assert_eq!(forms[5], Expr::Int(0x4000003)); // \C-\C-c
}

#[test]
fn parse_char_literal_unicode_name_escape_u_plus() {
    let forms = parse_forms(r"?\N{U+2764}").unwrap();
    assert_eq!(forms, vec![Expr::Char('\u{2764}')]);
}

#[test]
fn parse_keywords() {
    let forms = parse_forms(":test :size").unwrap();
    assert_eq!(
        forms,
        vec![
            Expr::Keyword(intern(":test")),
            Expr::Keyword(intern(":size")),
        ]
    );
}

#[test]
fn parse_uninterned_symbols_create_fresh_ids_and_preserve_labels() {
    let forms = parse_forms("#:foo #:foo #1=#:bar #1#").unwrap();
    assert_eq!(forms.len(), 4);

    let Expr::Symbol(first) = forms[0] else {
        panic!("expected uninterned symbol");
    };
    let Expr::Symbol(second) = forms[1] else {
        panic!("expected uninterned symbol");
    };
    let Expr::Symbol(third) = forms[2] else {
        panic!("expected labeled uninterned symbol");
    };
    let Expr::Symbol(fourth) = forms[3] else {
        panic!("expected label reference");
    };

    assert_eq!(resolve_sym(first), "foo");
    assert_eq!(resolve_sym(second), "foo");
    assert_ne!(first, second, "separate #: reads must be fresh symbols");
    assert_eq!(resolve_sym(third), "bar");
    assert_eq!(third, fourth, "#1= / #1# must preserve symbol identity");
}

#[test]
fn parse_symbols_honor_backslash_escapes() {
    let forms = parse_forms("\\.foo a\\ b a\\,b a\\\\b ## \\#\\#").unwrap();
    assert_eq!(
        forms,
        vec![
            Expr::Symbol(intern(".foo")),
            Expr::Symbol(intern("a b")),
            Expr::Symbol(intern("a,b")),
            Expr::Symbol(intern("a\\b")),
            Expr::Symbol(intern("")),
            Expr::Symbol(intern("##")),
        ]
    );
}

#[test]
fn parse_lists() {
    let forms = parse_forms("(+ 1 2) ()").unwrap();
    assert_eq!(
        forms,
        vec![
            Expr::List(vec![Expr::Symbol(intern("+")), Expr::Int(1), Expr::Int(2),]),
            Expr::List(vec![]),
        ]
    );
}

#[test]
fn parse_dotted_pair() {
    let forms = parse_forms("(a . b)").unwrap();
    assert_eq!(
        forms,
        vec![Expr::DottedList(
            vec![Expr::Symbol(intern("a"))],
            Box::new(Expr::Symbol(intern("b"))),
        )]
    );
}

#[test]
fn parse_vectors() {
    let forms = parse_forms("[1 2 3]").unwrap();
    assert_eq!(
        forms,
        vec![Expr::Vector(vec![Expr::Int(1), Expr::Int(2), Expr::Int(3)])]
    );
}

#[test]
fn parse_quote_shorthand() {
    let forms = parse_forms("'foo '(1 2)").unwrap();
    assert_eq!(
        forms,
        vec![
            Expr::List(vec![
                Expr::Symbol(intern("quote")),
                Expr::Symbol(intern("foo"))
            ]),
            Expr::List(vec![
                Expr::Symbol(intern("quote")),
                Expr::List(vec![Expr::Int(1), Expr::Int(2)]),
            ]),
        ]
    );
}

#[test]
fn parse_function_shorthand() {
    let forms = parse_forms("#'car").unwrap();
    assert_eq!(
        forms,
        vec![Expr::List(vec![
            Expr::Symbol(intern("function")),
            Expr::Symbol(intern("car")),
        ])]
    );
}

#[test]
fn parse_backquote() {
    let forms = parse_forms("`(a ,b ,@c)").unwrap();
    assert_eq!(forms.len(), 1);
}

#[test]
fn parse_hex_literal() {
    let forms = parse_forms("#xff #b1010 #o17").unwrap();
    assert_eq!(forms, vec![Expr::Int(255), Expr::Int(10), Expr::Int(15)]);
}

#[test]
fn parse_line_comment() {
    let forms = parse_forms("42 ; this is a comment\n7").unwrap();
    assert_eq!(forms, vec![Expr::Int(42), Expr::Int(7)]);
}

#[test]
fn parse_block_comment() {
    let forms = parse_forms("42 #| block comment |# 7").unwrap();
    assert_eq!(forms, vec![Expr::Int(42), Expr::Int(7)]);
}

#[test]
fn parse_nested_block_comment() {
    let forms = parse_forms("42 #| outer #| inner |# still outer |# 7").unwrap();
    assert_eq!(forms, vec![Expr::Int(42), Expr::Int(7)]);
}

#[test]
fn parse_bytecode_literal_vector_uses_byte_code_literal_form() {
    let forms = parse_forms("#[(x) \"\\bT\\207\" [x] 1 (#$ . 83)]").unwrap();
    assert_eq!(forms.len(), 1);
    let Expr::List(items) = &forms[0] else {
        panic!("expected byte-code-literal form");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[0], Expr::Symbol(intern("byte-code-literal")));

    let Expr::Vector(values) = &items[1] else {
        panic!("expected vector body");
    };
    let Expr::DottedList(cons_items, cdr) = &values[4] else {
        panic!("expected source-loc dotted pair");
    };
    assert_eq!(cons_items, &vec![Expr::ReaderLoadFileName]);
    assert_eq!(**cdr, Expr::Int(83));
}

#[test]
fn parse_paren_bytecode_literal_is_rejected() {
    let err = parse_forms("#((x) \"\\bT\\207\" [x] 1 (#$ . 83))").expect_err("should fail");
    assert!(err.message.contains('#'));
}

#[test]
fn parse_trailing_hash_reports_hash_payload() {
    let err = parse_forms("#").expect_err("should fail");
    assert_eq!(err.message, "#");
}

#[test]
fn parse_hash_unknown_dispatch_preserves_payload() {
    let err = parse_forms("#a").expect_err("should fail");
    assert_eq!(err.message, "#a");

    let err = parse_forms("#0").expect_err("should fail");
    assert_eq!(err.message, "#0");

    let err = parse_forms("# ").expect_err("should fail");
    assert_eq!(err.message, "# ");
}

#[test]
fn parse_hash_radix_missing_digits_reports_oracle_payload() {
    let err = parse_forms("#x").expect_err("should fail");
    assert_eq!(err.message, "integer, radix 16");
}

#[test]
fn parse_hash_open_paren_without_close_reports_eof_shape() {
    let err = parse_forms("#(").expect_err("should fail");
    assert!(err.message.contains("unterminated"));
}

#[test]
fn parse_hash_skip_bytes_reads_next_form() {
    let forms = parse_forms("#@4data42").unwrap();
    assert_eq!(forms, vec![Expr::Int(42)]);
}

#[test]
fn parse_hash_s_without_list_reports_hash_s_payload() {
    let err = parse_forms("#s").expect_err("should fail");
    assert_eq!(err.message, "#s");
}

#[test]
fn parse_hash_skip_without_length_reports_end_of_input() {
    let err = parse_forms("#@").expect_err("should fail");
    assert!(err.message.contains("end of input"));

    let err = parse_forms("#@x").expect_err("should fail");
    assert!(err.message.contains("end of input"));
}

#[test]
fn parse_hash_dollar_maps_to_load_file_name_symbol() {
    let forms = parse_forms("#$").unwrap();
    assert_eq!(forms, vec![Expr::ReaderLoadFileName]);
}
