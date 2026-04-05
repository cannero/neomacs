use super::*;
use crate::emacs_core::bytecode::Compiler;
use crate::emacs_core::eval::Context;
use crate::emacs_core::file_compile::compile_file_forms;
use crate::emacs_core::intern::intern;
use crate::emacs_core::parser::parse_forms;
use crate::emacs_core::value::{LambdaData, LambdaParams};
use std::rc::Rc;

fn sample_lambda_data() -> LambdaData {
    LambdaData {
        params: LambdaParams::simple(vec![intern("x")]),
        body: Rc::new(parse_forms("(+ x 1)").unwrap()),
        env: Some(Value::list(vec![Value::cons(
            Value::symbol("x"),
            Value::fixnum(41),
        )])),
        docstring: Some("sample closure".to_owned()),
        doc_form: None,
        interactive: Some(Value::string("p".to_owned())),
    }
}

fn loaded_form_runtime_value(form: &LoadedForm) -> Option<Value> {
    match form {
        LoadedForm::Eval(value) | LoadedForm::EagerEval(value) => Some(*value),
        LoadedForm::Constant(_) => None,
    }
}

#[test]
fn test_source_sha256() {
    crate::test_utils::init_test_tracing();
    let hash = source_sha256("hello world");
    // Known SHA-256 of "hello world".
    assert_eq!(
        hash,
        "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
    );
}

#[test]
fn test_neobc_magic_matches_format_version() {
    crate::test_utils::init_test_tracing();
    let magic = std::str::from_utf8(NEOBC_MAGIC).expect("valid utf8 magic");
    assert!(
        magic.starts_with("NEOVM-BC-V"),
        "unexpected neobc magic prefix: {magic:?}"
    );
    assert!(
        magic.ends_with('\n'),
        "unexpected neobc magic terminator: {magic:?}"
    );
    let version = magic
        .trim_end()
        .strip_prefix("NEOVM-BC-V")
        .expect("version prefix");
    assert_eq!(version, NEOBC_FORMAT_VERSION.to_string());
}

#[test]
fn test_roundtrip_simple_eval_form() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let forms = parse_forms("(+ 1 2)").unwrap();
    let compiled = compile_file_forms(&mut eval, &forms).unwrap();

    let hash = source_sha256("(+ 1 2)");
    let bytes = serialize_neobc(&hash, false, &compiled).expect("serialize");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.neobc");
    std::fs::write(&path, &bytes).unwrap();

    let loaded = read_neobc(&path, &hash).unwrap();
    assert!(!loaded.lexical_binding);
    assert_eq!(loaded.forms.len(), 1);
    assert!(matches!(
        &loaded.forms[0],
        LoadedForm::Eval(_) | LoadedForm::EagerEval(_)
    ));

    // Re-evaluate the loaded form and check result.
    if let Some(value) = loaded_form_runtime_value(&loaded.forms[0]) {
        let mut eval2 = Context::new();
        let result = eval2.eval_sub(value).unwrap();
        assert_eq!(result, Value::fixnum(3));
    }
}

#[test]
fn test_roundtrip_eval_when_compile() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let src = "(eval-when-compile (+ 10 20))";
    let forms = parse_forms(src).unwrap();
    let compiled = compile_file_forms(&mut eval, &forms).unwrap();
    assert_eq!(compiled.len(), 1);
    assert!(matches!(&compiled[0], CompiledForm::Constant(_)));

    let hash = source_sha256(src);
    let bytes = serialize_neobc(&hash, true, &compiled).expect("serialize");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.neobc");
    std::fs::write(&path, &bytes).unwrap();

    let loaded = read_neobc(&path, &hash).unwrap();
    assert!(loaded.lexical_binding);
    assert_eq!(loaded.forms.len(), 1);
    match &loaded.forms[0] {
        LoadedForm::Constant(v) => assert_eq!(*v, Value::fixnum(30)),
        other => panic!("expected Constant, got Eval"),
    }
}

#[test]
fn test_roundtrip_mixed_forms() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let src = "(defvar fc-fmt-a 1)\n(eval-when-compile (+ 2 3))\n(defvar fc-fmt-b 10)";
    let forms = parse_forms(src).unwrap();
    let compiled = compile_file_forms(&mut eval, &forms).unwrap();
    assert_eq!(compiled.len(), 3);

    let hash = source_sha256(src);
    let bytes = serialize_neobc(&hash, false, &compiled).expect("serialize");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.neobc");
    std::fs::write(&path, &bytes).unwrap();

    let loaded = read_neobc(&path, &hash).unwrap();
    assert_eq!(loaded.forms.len(), 3);
    assert!(matches!(
        &loaded.forms[0],
        LoadedForm::Eval(_) | LoadedForm::EagerEval(_)
    ));
    assert!(matches!(&loaded.forms[1], LoadedForm::Constant(_)));
    assert!(matches!(
        &loaded.forms[2],
        LoadedForm::Eval(_) | LoadedForm::EagerEval(_)
    ));
}

#[test]
fn test_hash_mismatch_rejected() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let forms = parse_forms("(+ 1 2)").unwrap();
    let compiled = compile_file_forms(&mut eval, &forms).unwrap();

    let hash = source_sha256("(+ 1 2)");
    let bytes = serialize_neobc(&hash, false, &compiled).expect("serialize");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.neobc");
    std::fs::write(&path, &bytes).unwrap();

    let err = read_neobc(&path, "wrong-hash").unwrap_err();
    assert!(err.to_string().contains("hash mismatch"));
}

#[test]
fn test_hash_skip_with_empty_string() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let forms = parse_forms("(+ 1 2)").unwrap();
    let compiled = compile_file_forms(&mut eval, &forms).unwrap();

    let hash = source_sha256("(+ 1 2)");
    let bytes = serialize_neobc(&hash, false, &compiled).expect("serialize");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.neobc");
    std::fs::write(&path, &bytes).unwrap();

    // Empty string skips hash check.
    let loaded = read_neobc(&path, "").unwrap();
    assert_eq!(loaded.forms.len(), 1);
}

#[test]
fn test_bad_magic_rejected() {
    crate::test_utils::init_test_tracing();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.neobc");
    std::fs::write(&path, b"NOT-A-NEOBC-FILE").unwrap();

    let err = read_neobc(&path, "").unwrap_err();
    assert!(err.to_string().contains("magic"));
}

#[test]
fn test_truncated_file_rejected() {
    crate::test_utils::init_test_tracing();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.neobc");
    // Write magic + a payload length that exceeds the actual data.
    let mut data = Vec::new();
    data.extend_from_slice(NEOBC_MAGIC);
    data.extend_from_slice(&1000u32.to_le_bytes());
    data.extend_from_slice(b"short");
    std::fs::write(&path, &data).unwrap();

    let err = read_neobc(&path, "").unwrap_err();
    assert!(err.to_string().contains("truncated"));
}

#[test]
fn test_write_neobc_convenience() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let forms = parse_forms("(+ 1 2)").unwrap();
    let compiled = compile_file_forms(&mut eval, &forms).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.neobc");
    let hash = source_sha256("(+ 1 2)");

    write_neobc(&path, &hash, false, &compiled).unwrap();

    let loaded = read_neobc(&path, &hash).unwrap();
    assert_eq!(loaded.forms.len(), 1);
}

#[test]
fn test_write_neobc_exprs_round_trip() {
    crate::test_utils::init_test_tracing();
    let forms = parse_forms("(progn (setq x 1) x)").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("exprs.neobc");
    let hash = source_sha256("(progn (setq x 1) x)");

    write_neobc_exprs(&path, &hash, false, &forms).unwrap();

    let loaded = read_neobc(&path, &hash).unwrap();
    assert!(!loaded.lexical_binding);
    assert_eq!(loaded.forms.len(), 1);
    match &loaded.forms[0] {
        LoadedForm::Eval(value) | LoadedForm::EagerEval(value) => {
            assert!(value.is_cons())
        }
        LoadedForm::Constant(_) => panic!("expected Eval form"),
    }
}

#[test]
fn test_roundtrip_string_constant() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let src = r#"(eval-when-compile "hello")"#;
    let forms = parse_forms(src).unwrap();
    let compiled = compile_file_forms(&mut eval, &forms).unwrap();
    assert_eq!(compiled.len(), 1);

    let hash = source_sha256(src);
    let bytes = serialize_neobc(&hash, false, &compiled).expect("serialize");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.neobc");
    std::fs::write(&path, &bytes).unwrap();

    let loaded = read_neobc(&path, &hash).unwrap();
    assert_eq!(loaded.forms.len(), 1);
    match &loaded.forms[0] {
        LoadedForm::Constant(v) => {
            assert_eq!(v.as_str(), Some("hello"));
        }
        _ => panic!("expected Constant"),
    }
}

#[test]
fn test_roundtrip_nil_constant() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let src = "(eval-when-compile nil)";
    let forms = parse_forms(src).unwrap();
    let compiled = compile_file_forms(&mut eval, &forms).unwrap();

    let hash = source_sha256(src);
    let bytes = serialize_neobc(&hash, false, &compiled).expect("serialize");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.neobc");
    std::fs::write(&path, &bytes).unwrap();

    let loaded = read_neobc(&path, &hash).unwrap();
    assert_eq!(loaded.forms.len(), 1);
    match &loaded.forms[0] {
        LoadedForm::Constant(v) => assert_eq!(*v, Value::NIL),
        _ => panic!("expected Constant"),
    }
}

#[test]
fn test_roundtrip_lexical_binding_flag() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let forms = parse_forms("t").unwrap();
    let compiled = compile_file_forms(&mut eval, &forms).unwrap();

    let hash = source_sha256("t");

    // lexical_binding = true
    let bytes = serialize_neobc(&hash, true, &compiled).expect("serialize");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.neobc");
    std::fs::write(&path, &bytes).unwrap();
    let loaded = read_neobc(&path, &hash).unwrap();
    assert!(loaded.lexical_binding);

    // lexical_binding = false
    let bytes = serialize_neobc(&hash, false, &compiled).expect("serialize");
    std::fs::write(&path, &bytes).unwrap();
    let loaded = read_neobc(&path, &hash).unwrap();
    assert!(!loaded.lexical_binding);
}

#[test]
fn test_neobc_rejects_propertized_string_runtime_values() {
    crate::test_utils::init_test_tracing();
    let value = Value::string_with_text_properties(
        "x",
        vec![super::super::value::StringTextPropertyRun {
            start: 0,
            end: 1,
            plist: Value::list(vec![
                Value::keyword("face"),
                Value::make_symbol("bold".to_owned()),
            ]),
        }],
    );
    let mut builder = NeobcBuilder::new("hash", false);
    let err = builder.push_eval_value_detailed(&value).unwrap_err();
    assert_eq!(err.path(), "root");
    assert_eq!(err.detail(), "string with text properties");
}

#[test]
fn test_neobc_reports_nested_unsupported_runtime_value_path() {
    crate::test_utils::init_test_tracing();
    let value = Value::vector(vec![Value::fixnum(1), Value::subr(intern("car"))]);
    let mut builder = NeobcBuilder::new("hash", false);
    let err = builder.push_eval_value_detailed(&value).unwrap_err();
    assert_eq!(err.path(), "root[1]");
    assert!(err.detail().contains("Subr"));
}

#[test]
fn test_transplant_value_pair_preserves_shared_uninterned_symbol_identity() {
    crate::test_utils::init_test_tracing();
    let sym = intern_uninterned("shared-transplant-symbol");
    let first = Value::list(vec![Value::symbol(sym), Value::fixnum(1)]);
    let second = Value::list(vec![Value::symbol(sym), Value::fixnum(2)]);

    let (first_local, second_local) =
        transplant_value_pair(&first, &second).expect("transplant pair");

    let first_items = crate::emacs_core::value::list_to_vec(&first_local).expect("first list");
    let second_items = crate::emacs_core::value::list_to_vec(&second_local).expect("second list");

    let first_sym = first_items[0].as_symbol_id().expect("first symbol");
    let second_sym = second_items[0].as_symbol_id().expect("second symbol");
    assert_eq!(resolve_sym(first_sym), "shared-transplant-symbol");
    assert_eq!(first_sym, second_sym);
}

#[test]
fn test_roundtrip_record_literal_expr() {
    crate::test_utils::init_test_tracing();
    let src = "#s(cl-slot-descriptor foo 1)";
    let forms = parse_forms(src).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("record.neobc");
    let hash = source_sha256(src);

    write_neobc_exprs(&path, &hash, false, &forms).unwrap();

    let loaded = read_neobc(&path, &hash).unwrap();
    match &loaded.forms[0] {
        LoadedForm::Eval(value) | LoadedForm::EagerEval(value) => {
            let mut eval = Context::new();
            let result = eval.eval_sub(*value).unwrap();
            assert!(result.is_record());
            let items = result.as_record_data().unwrap();
            assert_eq!(items.len(), 3);
            assert_eq!(items[0].as_symbol_name(), Some("cl-slot-descriptor"));
            assert_eq!(items[1].as_symbol_name(), Some("foo"));
            assert_eq!(items[2], Value::fixnum(1));
        }
        LoadedForm::Constant(_) => panic!("expected Eval form"),
    }
}

#[test]
fn test_roundtrip_hash_table_literal_expr() {
    crate::test_utils::init_test_tracing();
    let src = "#s(hash-table size 3 test equal data (\"a\" 1 \"b\" 2))";
    let forms = parse_forms(src).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hash-table.neobc");
    let hash = source_sha256(src);

    write_neobc_exprs(&path, &hash, false, &forms).unwrap();

    let loaded = read_neobc(&path, &hash).unwrap();
    match &loaded.forms[0] {
        LoadedForm::Eval(value) | LoadedForm::EagerEval(value) => {
            let mut eval = Context::new();
            let result = eval.eval_sub(*value).unwrap();
            let table = result.as_hash_table().unwrap();
            assert_eq!(table.test, crate::emacs_core::value::HashTableTest::Equal);
            assert_eq!(table.size, 3);
            assert_eq!(table.data.len(), 2);
        }
        LoadedForm::Constant(_) => panic!("expected Eval form"),
    }
}

#[test]
fn test_roundtrip_record_with_nested_hash_table_runtime_value() {
    crate::test_utils::init_test_tracing();
    let table = Value::hash_table_with_options(
        crate::emacs_core::value::HashTableTest::Eq,
        2,
        None,
        1.5,
        0.8125,
    );
    let _ = table.with_hash_table_mut(|ht| {
        ht.test_name = Some(intern("eq"));
        let alpha = Value::symbol("alpha");
        let beta = Value::symbol("beta");
        let alpha_key = alpha.to_hash_key(&ht.test);
        let beta_key = beta.to_hash_key(&ht.test);
        ht.data.insert(alpha_key.clone(), Value::fixnum(1));
        ht.key_snapshots.insert(alpha_key.clone(), alpha);
        ht.insertion_order.push(alpha_key);
        ht.data.insert(beta_key.clone(), Value::fixnum(2));
        ht.key_snapshots.insert(beta_key.clone(), beta);
        ht.insertion_order.push(beta_key);
    });
    let record = Value::make_record(vec![Value::symbol("class"), Value::fixnum(7), table]);

    let mut builder = NeobcBuilder::new("hash", false);
    builder.push_eval_value_detailed(&record).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested-record.neobc");
    builder.write(&path).unwrap();

    let loaded = read_neobc(&path, "hash").unwrap();
    match &loaded.forms[0] {
        LoadedForm::Eval(value) | LoadedForm::EagerEval(value) => {
            let mut eval = Context::new();
            let result = eval.eval_sub(*value).unwrap();
            let items = result.as_record_data().unwrap();
            let table = items[2].as_hash_table().unwrap();
            assert_eq!(table.test, crate::emacs_core::value::HashTableTest::Eq);
            assert_eq!(table.data.len(), 2);
            assert_eq!(table.test_name.map(resolve_sym), Some("eq"));
        }
        LoadedForm::Constant(_) => panic!("expected Eval form"),
    }
}

#[test]
fn test_roundtrip_lambda_runtime_value() {
    crate::test_utils::init_test_tracing();
    let lambda = Value::make_lambda(sample_lambda_data());
    let mut builder = NeobcBuilder::new("hash", false);
    builder.push_eval_value_detailed(&lambda).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lambda.neobc");
    builder.write(&path).unwrap();

    let loaded = read_neobc(&path, "hash").unwrap();
    match &loaded.forms[0] {
        LoadedForm::Eval(value) | LoadedForm::EagerEval(value) => {
            let mut eval = Context::new();
            let result = eval.eval_sub(*value).unwrap();
            assert!(result.is_lambda());
            assert_eq!(result.closure_docstring(), Some(Some("sample closure")));
            assert_eq!(result.closure_interactive(), Some(Some(Value::string("p"))));
            assert_eq!(
                result
                    .closure_params()
                    .map(|params| params.required.clone())
                    .unwrap_or_default(),
                vec![intern("x")]
            );
        }
        LoadedForm::Constant(_) => panic!("expected Eval form"),
    }
}

#[test]
fn test_roundtrip_lambda_runtime_value_with_nested_lambda_env() {
    crate::test_utils::init_test_tracing();
    let nested = Value::make_lambda(sample_lambda_data());
    let outer = Value::make_lambda(LambdaData {
        params: LambdaParams::simple(vec![intern("y")]),
        body: Rc::new(parse_forms("(list y)").unwrap()),
        env: Some(Value::list(vec![nested])),
        docstring: Some("outer closure".to_owned()),
        doc_form: None,
        interactive: None,
    });
    let mut builder = NeobcBuilder::new("hash", false);
    builder.push_eval_value_detailed(&outer).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested-lambda-env.neobc");
    builder.write(&path).unwrap();

    let loaded = read_neobc(&path, "hash").unwrap();
    match &loaded.forms[0] {
        LoadedForm::Eval(value) | LoadedForm::EagerEval(value) => {
            let mut eval = Context::new();
            let result = eval.eval_sub(*value).unwrap();
            assert!(result.is_lambda());
            assert_eq!(result.closure_docstring(), Some(Some("outer closure")));
            let env = result.closure_env().unwrap().unwrap();
            let items = crate::emacs_core::value::list_to_vec(&env).unwrap();
            assert_eq!(items.len(), 1);
            assert!(items[0].is_lambda());
            assert_eq!(items[0].closure_docstring(), Some(Some("sample closure")));
        }
        LoadedForm::Constant(_) => panic!("expected Eval form"),
    }
}

#[test]
fn test_roundtrip_macro_runtime_value() {
    crate::test_utils::init_test_tracing();
    let macro_value = Value::make_macro(sample_lambda_data());
    let mut builder = NeobcBuilder::new("hash", false);
    builder.push_eval_value_detailed(&macro_value).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("macro.neobc");
    builder.write(&path).unwrap();

    let loaded = read_neobc(&path, "hash").unwrap();
    match &loaded.forms[0] {
        LoadedForm::Eval(value) | LoadedForm::EagerEval(value) => {
            let mut eval = Context::new();
            let result = eval.eval_sub(*value).unwrap();
            assert!(result.is_macro());
            assert_eq!(result.closure_docstring(), Some(Some("sample closure")));
        }
        LoadedForm::Constant(_) => panic!("expected Eval form"),
    }
}

#[test]
fn test_write_neobc_exprs_round_trips_opaque_lambda_refs() {
    crate::test_utils::init_test_tracing();
    let lambda = Value::make_lambda(sample_lambda_data());
    let exprs = vec![Expr::OpaqueValueRef(
        OPAQUE_POOL.with(|pool| pool.borrow_mut().insert(lambda)),
    )];
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("opaque-lambda.neobc");
    let hash = source_sha256("opaque-lambda");

    write_neobc_exprs(&path, &hash, false, &exprs).unwrap();

    let loaded = read_neobc(&path, &hash).unwrap();
    match &loaded.forms[0] {
        LoadedForm::Eval(value) | LoadedForm::EagerEval(value) => {
            let mut eval = Context::new();
            let result = eval.eval_sub(*value).unwrap();
            assert!(result.is_lambda());
            assert_eq!(result.closure_docstring(), Some(Some("sample closure")));
        }
        LoadedForm::Constant(_) => panic!("expected Eval form"),
    }
}

#[test]
fn test_roundtrip_bytecode_runtime_value() {
    crate::test_utils::init_test_tracing();
    let body = parse_forms("(+ x 1)").unwrap();
    let bytecode = Value::make_bytecode(
        Compiler::new(false).compile_lambda(&LambdaParams::simple(vec![intern("x")]), &body),
    );
    let mut builder = NeobcBuilder::new("hash", false);
    builder.push_eval_value_detailed(&bytecode).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bytecode.neobc");
    builder.write(&path).unwrap();

    let loaded = read_neobc(&path, "hash").unwrap();
    match &loaded.forms[0] {
        LoadedForm::Eval(value) | LoadedForm::EagerEval(value) => {
            let mut eval = Context::new();
            let result = eval.eval_sub(*value).unwrap();
            assert!(result.is_bytecode());
            let bytecode = result.get_bytecode_data().expect("bytecode payload");
            assert_eq!(bytecode.params.required, vec![intern("x")]);
            assert!(!bytecode.ops.is_empty());
        }
        LoadedForm::Constant(_) => panic!("expected Eval form"),
    }
}

#[test]
fn test_write_neobc_exprs_round_trips_opaque_bytecode_refs() {
    crate::test_utils::init_test_tracing();
    let body = parse_forms("(+ x 1)").unwrap();
    let bytecode = Value::make_bytecode(
        Compiler::new(false).compile_lambda(&LambdaParams::simple(vec![intern("x")]), &body),
    );
    let exprs = vec![Expr::OpaqueValueRef(
        OPAQUE_POOL.with(|pool| pool.borrow_mut().insert(bytecode)),
    )];
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("opaque-bytecode.neobc");
    let hash = source_sha256("opaque-bytecode");

    write_neobc_exprs(&path, &hash, false, &exprs).unwrap();

    let loaded = read_neobc(&path, &hash).unwrap();
    match &loaded.forms[0] {
        LoadedForm::Eval(value) | LoadedForm::EagerEval(value) => {
            let mut eval = Context::new();
            let result = eval.eval_sub(*value).unwrap();
            assert!(result.is_bytecode());
            assert_eq!(
                result
                    .get_bytecode_data()
                    .expect("bytecode payload")
                    .params
                    .required,
                vec![intern("x")]
            );
        }
        LoadedForm::Constant(_) => panic!("expected Eval form"),
    }
}
