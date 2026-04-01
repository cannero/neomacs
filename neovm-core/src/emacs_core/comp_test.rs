use super::*;

#[test]
fn comp_init_and_release_return_true() {
    assert_val_eq!(builtin_comp_init_ctxt(vec![]).unwrap(), Value::T);
    assert_val_eq!(builtin_comp_release_ctxt(vec![]).unwrap(), Value::T);
}

#[test]
fn comp_subr_signature_requires_subr() {
    let err = builtin_comp_subr_signature(vec![Value::symbol("+")]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn comp_el_to_eln_reports_missing_file() {
    let err = builtin_comp_el_to_eln_filename(vec![Value::string("no-such-file.el")]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "file-missing"),
        other => panic!("expected signal, got {other:?}"),
    }
}
