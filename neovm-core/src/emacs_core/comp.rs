//! Native compilation compatibility builtins.
//!
//! Emacs exposes a small set of `comp-*` and `comp--*` primitives used by the
//! native compilation pipeline. NeoVM does not perform native compilation, but
//! these implementations provide compatible arity/type/error behavior for
//! startup code.

use std::env;
use std::path::{Path, PathBuf};

use super::error::{EvalResult, Flow, signal};
use super::value::{Value, ValueKind};

fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_range_args(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), Flow> {
    if args.len() < min || args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_string(value: &Value) -> Result<String, Flow> {
    match value.kind() {
        ValueKind::String => Ok(crate::emacs_core::value::with_heap(|h| {
            h.get_string(*id).to_owned()
        })),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *value],
        )),
    }
}

fn expect_subr(value: &Value) -> Result<(), Flow> {
    match value.kind() {
        ValueKind::Subr(_) => Ok(()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("subrp"), *value],
        )),
    }
}

fn absolutize_path(path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(p)
    }
}

fn ensure_existing_file(path: &str) -> Result<PathBuf, Flow> {
    let abs = absolutize_path(path);
    if abs.exists() {
        Ok(abs)
    } else {
        Err(signal(
            "file-missing",
            vec![Value::string(abs.display().to_string())],
        ))
    }
}

/// `(comp--compile-ctxt-to-file0 CTXT)` -- no-op native compilation compatibility entry.
pub(crate) fn builtin_comp_compile_ctxt_to_file0(args: Vec<Value>) -> EvalResult {
    expect_args("comp--compile-ctxt-to-file0", &args, 1)?;
    Ok(Value::T)
}

/// `(comp--init-ctxt)` -- initialize native compilation context.
pub(crate) fn builtin_comp_init_ctxt(args: Vec<Value>) -> EvalResult {
    expect_args("comp--init-ctxt", &args, 0)?;
    Ok(Value::T)
}

/// `(comp--install-trampoline FUN TARGET)` -- no-op in NeoVM.
pub(crate) fn builtin_comp_install_trampoline(args: Vec<Value>) -> EvalResult {
    expect_args("comp--install-trampoline", &args, 2)?;
    Ok(Value::NIL)
}

/// `(comp--late-register-subr ...)` -- no-op in NeoVM.
pub(crate) fn builtin_comp_late_register_subr(args: Vec<Value>) -> EvalResult {
    expect_args("comp--late-register-subr", &args, 7)?;
    Ok(Value::NIL)
}

/// `(comp--register-lambda ...)` -- no-op in NeoVM.
pub(crate) fn builtin_comp_register_lambda(args: Vec<Value>) -> EvalResult {
    expect_args("comp--register-lambda", &args, 7)?;
    Ok(Value::NIL)
}

/// `(comp--register-subr ...)` -- no-op in NeoVM.
pub(crate) fn builtin_comp_register_subr(args: Vec<Value>) -> EvalResult {
    expect_args("comp--register-subr", &args, 7)?;
    Ok(Value::NIL)
}

/// `(comp--release-ctxt)` -- release native compilation context.
pub(crate) fn builtin_comp_release_ctxt(args: Vec<Value>) -> EvalResult {
    expect_args("comp--release-ctxt", &args, 0)?;
    Ok(Value::T)
}

/// `(comp--subr-signature SUBR)` -- return native signature metadata.
///
/// NeoVM does not expose signatures; returns nil after validating SUBR.
pub(crate) fn builtin_comp_subr_signature(args: Vec<Value>) -> EvalResult {
    expect_args("comp--subr-signature", &args, 1)?;
    expect_subr(&args[0])?;
    Ok(Value::NIL)
}

/// `(comp-el-to-eln-filename FILE &optional OUTPUT-DIR)` -- map .el -> .eln.
pub(crate) fn builtin_comp_el_to_eln_filename(args: Vec<Value>) -> EvalResult {
    expect_range_args("comp-el-to-eln-filename", &args, 1, 2)?;
    let file = expect_string(&args[0])?;
    let mut out = ensure_existing_file(&file)?;
    out.set_extension("eln");
    Ok(Value::string(out.display().to_string()))
}

/// `(comp-el-to-eln-rel-filename FILE)` -- relative .el -> .eln mapping.
pub(crate) fn builtin_comp_el_to_eln_rel_filename(args: Vec<Value>) -> EvalResult {
    expect_args("comp-el-to-eln-rel-filename", &args, 1)?;
    let file = expect_string(&args[0])?;
    let _ = ensure_existing_file(&file)?;
    let mut out = PathBuf::from(file);
    out.set_extension("eln");
    Ok(Value::string(out.display().to_string()))
}

/// `(comp-libgccjit-version)` -- report libgccjit version tuple.
pub(crate) fn builtin_comp_libgccjit_version(args: Vec<Value>) -> EvalResult {
    expect_args("comp-libgccjit-version", &args, 0)?;
    Ok(Value::list(vec![
        Value::fixnum(14),
        Value::fixnum(3),
        Value::fixnum(0),
    ]))
}

/// `(comp-native-compiler-options-effective-p)` -- options are effective.
pub(crate) fn builtin_comp_native_compiler_options_effective_p(args: Vec<Value>) -> EvalResult {
    expect_args("comp-native-compiler-options-effective-p", &args, 0)?;
    Ok(Value::T)
}

/// `(comp-native-driver-options-effective-p)` -- options are effective.
pub(crate) fn builtin_comp_native_driver_options_effective_p(args: Vec<Value>) -> EvalResult {
    expect_args("comp-native-driver-options-effective-p", &args, 0)?;
    Ok(Value::T)
}
#[cfg(test)]
#[path = "comp_test.rs"]
mod tests;
