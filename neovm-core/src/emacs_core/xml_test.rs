use super::*;
use crate::emacs_core::value::ValueKind;

#[test]
fn zlib_decompress_region_arity_and_type_validation() {
    crate::test_utils::init_test_tracing();
    let arity = builtin_zlib_decompress_region(vec![]);
    assert!(arity.is_err());

    let too_many = builtin_zlib_decompress_region(vec![
        Value::fixnum(1),
        Value::fixnum(1),
        Value::NIL,
        Value::NIL,
    ]);
    assert!(too_many.is_err());

    let bad_type = builtin_zlib_decompress_region(vec![Value::string("x"), Value::fixnum(1)]);
    assert!(bad_type.is_err());
}

#[test]
fn zlib_decompress_region_signals_unibyte_requirement() {
    crate::test_utils::init_test_tracing();
    let result = builtin_zlib_decompress_region(vec![Value::fixnum(1), Value::fixnum(1)])
        .expect_err("must signal error in multibyte buffers");
    match result {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string(
                    "This function can be called only in unibyte buffers"
                )]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn libxml_parse_xml_region_arity_and_type_subset() {
    crate::test_utils::init_test_tracing();
    assert_eq!(builtin_libxml_parse_xml_region(vec![]).unwrap(), Value::NIL);
    assert_eq!(
        builtin_libxml_parse_xml_region(vec![Value::NIL]).unwrap(),
        Value::NIL
    );
    assert_eq!(
        builtin_libxml_parse_xml_region(vec![Value::fixnum(1), Value::fixnum(1)]).unwrap(),
        Value::NIL
    );
    assert_eq!(
        builtin_libxml_parse_xml_region(vec![Value::NIL, Value::fixnum(1)]).unwrap(),
        Value::NIL
    );

    let wrong_type =
        builtin_libxml_parse_xml_region(vec![Value::string("x"), Value::fixnum(1)]).unwrap_err();
    match wrong_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("integer-or-marker-p"), Value::string("x")]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
    let wrong_base =
        builtin_libxml_parse_xml_region(vec![Value::fixnum(1), Value::fixnum(2), Value::fixnum(1)])
            .unwrap_err();
    match wrong_base {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::fixnum(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let wrong_arity = builtin_libxml_parse_xml_region(vec![
        Value::fixnum(1),
        Value::fixnum(1),
        Value::NIL,
        Value::NIL,
        Value::NIL,
    ])
    .unwrap_err();
    match wrong_arity {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("libxml-parse-xml-region"), Value::fixnum(5)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn libxml_parse_html_region_arity_and_type_subset() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        builtin_libxml_parse_html_region(vec![]).unwrap(),
        html_parse_fallback("libxml-parse-html-region", &[])
    );
    assert_eq!(
        builtin_libxml_parse_html_region(vec![Value::NIL]).unwrap(),
        html_parse_fallback("libxml-parse-html-region", &[Value::NIL])
    );
    assert_eq!(
        builtin_libxml_parse_html_region(vec![Value::fixnum(1)]).unwrap(),
        html_parse_fallback("libxml-parse-html-region", &[Value::fixnum(1)])
    );
    assert_eq!(
        builtin_libxml_parse_html_region(vec![Value::fixnum(1), Value::NIL]).unwrap(),
        html_parse_fallback("libxml-parse-html-region", &[Value::fixnum(1), Value::NIL])
    );
    assert_eq!(
        builtin_libxml_parse_html_region(vec![Value::NIL, Value::fixnum(1)]).unwrap(),
        Value::NIL
    );
    assert_eq!(
        builtin_libxml_parse_html_region(vec![Value::fixnum(1), Value::fixnum(1)]).unwrap(),
        Value::NIL
    );
    assert_eq!(
        builtin_libxml_parse_html_region(vec![Value::fixnum(1), Value::fixnum(2)]).unwrap(),
        html_parse_fallback(
            "libxml-parse-html-region",
            &[Value::fixnum(1), Value::fixnum(2)]
        )
    );

    let wrong_type =
        builtin_libxml_parse_html_region(vec![Value::string("x"), Value::fixnum(1)]).unwrap_err();
    match wrong_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("integer-or-marker-p"), Value::string("x")]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
    let wrong_base = builtin_libxml_parse_html_region(vec![
        Value::fixnum(1),
        Value::fixnum(2),
        Value::fixnum(1),
    ])
    .unwrap_err();
    match wrong_base {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::fixnum(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let wrong_arity = builtin_libxml_parse_html_region(vec![
        Value::fixnum(1),
        Value::fixnum(1),
        Value::NIL,
        Value::NIL,
        Value::NIL,
    ])
    .unwrap_err();
    match wrong_arity {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("libxml-parse-html-region"), Value::fixnum(5)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn availability_probes_return_true_and_validate_arity() {
    crate::test_utils::init_test_tracing();
    assert_eq!(builtin_libxml_available_p(vec![]).unwrap(), Value::T);
    assert_eq!(builtin_zlib_available_p(vec![]).unwrap(), Value::T);

    let libxml_arity = builtin_libxml_available_p(vec![Value::fixnum(1)]).unwrap_err();
    match libxml_arity {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("libxml-available-p"), Value::fixnum(1)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let zlib_arity = builtin_zlib_available_p(vec![Value::fixnum(1)]).unwrap_err();
    match zlib_arity {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("zlib-available-p"), Value::fixnum(1)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}
