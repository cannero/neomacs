//! File-level byte compilation.
//!
//! Processes top-level forms from a parsed `.el` file, evaluating
//! `eval-when-compile` bodies at compile time and emitting the results as
//! constants.  All other forms are evaluated for side effects (so that
//! `defun`, `defvar`, `require`, etc. take effect in the compile-time
//! environment) and also emitted as `Eval` forms to replay at load time.

use std::path::Path;

use super::builtins::parse_lambda_params_from_value;
use super::bytecode::Compiler;
use super::error::{EvalError, Flow, map_flow};
use super::eval::{Context, quote_to_value};
use super::expr::Expr;
use super::intern::resolve_sym;
use super::value::{Value, list_to_vec};

/// A single compiled top-level form.
#[derive(Clone, Debug)]
pub enum CompiledForm {
    /// A form to evaluate at load time (already macro-expanded).
    Eval(Value),
    /// A source form that must go back through eager macroexpansion at load
    /// time to preserve GNU `eval-and-compile` / macro-expansion side effects.
    EagerEval(Value),
    /// A constant produced by `eval-when-compile` — body was evaluated at
    /// compile time and only the result value is retained.
    Constant(Value),
}

impl CompiledForm {
    fn root_value(&self) -> Value {
        match self {
            CompiledForm::Eval(value)
            | CompiledForm::EagerEval(value)
            | CompiledForm::Constant(value) => *value,
        }
    }
}

fn eval_error_to_flow(err: EvalError) -> Flow {
    match err {
        EvalError::Signal {
            symbol,
            data,
            raw_data,
        } => Flow::Signal(super::error::SignalData {
            symbol,
            data,
            raw_data,
            suppress_signal_hook: false,
            selected_resume: None,
            search_complete: false,
        }),
        EvalError::UncaughtThrow { tag, value } => Flow::Throw { tag, value },
    }
}

fn compile_replayable_toplevel_form(
    eval: &mut Context,
    form: &Expr,
    out: &mut Vec<CompiledForm>,
) -> Result<(), Flow> {
    let form_value = quote_to_value(form);
    let Some(macroexpand_fn) = super::load::get_eager_macroexpand_fn(eval) else {
        eval.eval(form)?;
        out.push(CompiledForm::Eval(form_value));
        return Ok(());
    };

    super::load::eager_expand_toplevel_forms(
        eval,
        form_value,
        macroexpand_fn,
        &mut |ctx, original, expanded, requires_eager_replay| {
            ctx.with_gc_scope_result(|ctx| {
                ctx.root(expanded);
                ctx.eval_value(&expanded)?;
                out.push(if requires_eager_replay {
                    CompiledForm::EagerEval(original)
                } else {
                    CompiledForm::Eval(expanded)
                });
                Ok(Value::NIL)
            })
            .map_err(map_flow)
        },
    )
    .map(|_| ())
    .map_err(eval_error_to_flow)
}

/// Compile a sequence of top-level forms from a `.el` file.
///
/// Each form is classified and processed:
/// - `(eval-when-compile BODY...)` — body is evaluated now; a `Constant` with
///   the result value is emitted.
/// - `(eval-and-compile BODY...)` — body is evaluated now AND emitted as an
///   `Eval` form so it also runs at load time.
/// - `(progn BODY...)` — flattened; each sub-form is compiled recursively.
/// - Everything else — evaluated at compile time (for side effects such as
///   `defun`, `defvar`, `require`), then emitted as `Eval(quoted_form)` to
///   replay at load time.
pub fn compile_file_forms(eval: &mut Context, forms: &[Expr]) -> Result<Vec<CompiledForm>, Flow> {
    let mut compiled = Vec::new();
    for form in forms {
        let compiled_roots: Vec<Value> = compiled.iter().map(CompiledForm::root_value).collect();
        eval.with_gc_scope_result(|ctx| {
            for root in &compiled_roots {
                ctx.root(*root);
            }
            compile_toplevel_file_form(ctx, form, &mut compiled)
        })?;
    }
    Ok(compiled)
}

fn compile_toplevel_defun(eval: &Context, items: &[Expr]) -> Option<Value> {
    if items.len() < 4 {
        return None;
    }

    let Expr::Symbol(name_id) = items.get(1)? else {
        return None;
    };

    let arglist = items.get(2)?;
    let docstring = match items.get(3) {
        Some(Expr::Str(text)) => Some(text.clone()),
        _ => None,
    };
    let body_start = if docstring.is_some() { 4 } else { 3 };
    let body = items.get(body_start..)?;
    if body.is_empty() {
        return None;
    }

    let params = parse_lambda_params_from_value(&quote_to_value(arglist)).ok()?;
    let mut compiler = Compiler::new(eval.lexical_binding());
    let mut bytecode = compiler.compile_lambda(&params, body);
    bytecode.docstring = docstring.clone();

    let mut form = vec![
        Value::symbol("defalias"),
        Value::list(vec![Value::symbol("quote"), Value::from_sym_id(*name_id)]),
        Value::make_bytecode(bytecode),
    ];
    if let Some(doc) = docstring {
        form.push(Value::string(doc));
    }
    Some(Value::list(form))
}

/// Process a single top-level form, appending results to `out`.
fn compile_toplevel_file_form(
    eval: &mut Context,
    form: &Expr,
    out: &mut Vec<CompiledForm>,
) -> Result<(), Flow> {
    match form {
        Expr::List(items) if !items.is_empty() => {
            if let Expr::Symbol(id) = &items[0] {
                let name = resolve_sym(*id);
                match name {
                    "progn" => {
                        // Flatten: recurse into each sub-form.
                        for sub in &items[1..] {
                            compile_toplevel_file_form(eval, sub, out)?;
                        }
                        return Ok(());
                    }
                    "eval-when-compile" => {
                        // Evaluate body at compile time, emit only the result
                        // constant.  This matches GNU Emacs .elc semantics
                        // where eval-when-compile is folded to (quote RESULT).
                        let result = eval.sf_progn(&items[1..])?;
                        out.push(CompiledForm::Constant(result));
                        return Ok(());
                    }
                    "eval-and-compile" => {
                        // Evaluate body NOW and preserve the GNU eager-load
                        // replay policy for the load-time execution.
                        compile_replayable_toplevel_form(eval, form, out)?;
                        return Ok(());
                    }
                    "defun" => {
                        if let Some(compiled_form) = compile_toplevel_defun(eval, items) {
                            eval.eval_value(&compiled_form)?;
                            out.push(CompiledForm::Eval(compiled_form));
                            return Ok(());
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    compile_replayable_toplevel_form(eval, form, out)
}

/// Errors that can occur during file compilation.
#[derive(Debug)]
pub enum CompileFileError {
    /// An I/O error reading the source or writing the output.
    Io(std::io::Error),
    /// A parse error in the source file.
    Parse(String),
    /// An evaluation error during compile-time evaluation.
    Eval(String),
    /// A serialization error (e.g., forms contain non-serializable opaque values).
    Serialize(String),
}

impl std::fmt::Display for CompileFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileFileError::Io(e) => write!(f, "I/O error: {}", e),
            CompileFileError::Parse(e) => write!(f, "parse error: {}", e),
            CompileFileError::Eval(e) => write!(f, "eval error: {}", e),
            CompileFileError::Serialize(e) => write!(f, "serialize error: {}", e),
        }
    }
}

/// Compile a `.el` file to `.neobc` bytecode.
///
/// This is NeoVM's equivalent of GNU Emacs's `byte-compile-file`.
/// Reads the `.el` source, parses forms, evaluates `eval-when-compile`
/// bodies at compile time (folding results to constants), and writes
/// the compiled output to a `.neobc` file alongside the source.
pub fn compile_el_to_neobc(eval: &mut Context, el_path: &Path) -> Result<(), CompileFileError> {
    // 1. Read the .el source.
    let raw_bytes = std::fs::read(el_path).map_err(CompileFileError::Io)?;
    let content = super::load::decode_emacs_utf8(&raw_bytes);

    // 2. Detect lexical-binding from the file-local cookie.
    let lexical = super::load::source_lexical_binding_for_load(
        eval,
        &content,
        Some(Value::string(el_path.to_string_lossy().to_string())),
    )
    .map_err(|e| CompileFileError::Eval(e.to_string()))?;

    // 3. Compute source hash for cache invalidation.
    let source_hash = super::file_compile_format::source_sha256(&content);

    // 4. Parse forms.
    let forms = super::parser::parse_forms(&content)
        .map_err(|e| CompileFileError::Parse(format!("{}", e)))?;

    // 5. Set up evaluator for compilation (honour the source's lexical-binding).
    let old_lexical = eval.lexical_binding();
    if lexical {
        eval.set_lexical_binding(true);
    }

    // 6. Compile forms (evaluating eval-when-compile at compile time).
    let compiled = compile_file_forms(eval, &forms).map_err(|e| {
        // Restore evaluator state before propagating the error.
        eval.set_lexical_binding(old_lexical);
        CompileFileError::Eval(format!("{:?}", e))
    })?;

    // 7. Restore evaluator state.
    eval.set_lexical_binding(old_lexical);

    // 8. Write .neobc alongside the source.
    let neobc_path = el_path.with_extension("neobc");
    let bytes =
        super::file_compile_format::serialize_neobc_detailed(&source_hash, lexical, &compiled)
            .map_err(|err| {
                CompileFileError::Serialize(format!("{}: {}", err.path(), err.detail()))
            })?;
    std::fs::write(&neobc_path, bytes).map_err(CompileFileError::Io)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emacs_core::file_compile_format::{LoadedForm, read_neobc};
    use crate::emacs_core::parser::parse_forms;

    #[test]
    fn test_compile_simple_form() {
        crate::test_utils::init_test_tracing();
        let mut eval = Context::new();
        let forms = parse_forms("(+ 1 2)").unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();
        assert_eq!(compiled.len(), 1);
        assert!(matches!(&compiled[0], CompiledForm::Eval(_)));
    }

    #[test]
    fn test_compile_eval_when_compile() {
        crate::test_utils::init_test_tracing();
        let mut eval = Context::new();
        let forms = parse_forms("(eval-when-compile (+ 10 20))").unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();
        assert_eq!(compiled.len(), 1);
        match &compiled[0] {
            CompiledForm::Constant(v) => assert_eq!(*v, Value::fixnum(30)),
            other => panic!("expected Constant, got {:?}", other),
        }
    }

    #[test]
    fn test_compile_eval_and_compile() {
        crate::test_utils::init_test_tracing();
        let mut eval = Context::new();
        let forms = parse_forms("(eval-and-compile (defvar test-fc-var 42))").unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();
        assert_eq!(compiled.len(), 1);
        assert!(matches!(&compiled[0], CompiledForm::Eval(_)));
        // The defvar should have taken effect at compile time.
        let val = eval.obarray().symbol_value("test-fc-var");
        assert_eq!(val, Some(&Value::fixnum(42)));
    }

    #[test]
    fn test_compile_progn_flattens() {
        crate::test_utils::init_test_tracing();
        let mut eval = Context::new();
        let forms = parse_forms("(progn (+ 1 2) (+ 3 4))").unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();
        // progn with 2 sub-forms should produce 2 CompiledForm entries.
        assert_eq!(compiled.len(), 2);
        assert!(matches!(&compiled[0], CompiledForm::Eval(_)));
        assert!(matches!(&compiled[1], CompiledForm::Eval(_)));
    }

    #[test]
    fn test_compile_progn_with_eval_when_compile() {
        crate::test_utils::init_test_tracing();
        let mut eval = Context::new();
        let forms = parse_forms("(progn (eval-when-compile (+ 1 2)) (+ 3 4))").unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();
        assert_eq!(compiled.len(), 2);
        match &compiled[0] {
            CompiledForm::Constant(v) => assert_eq!(*v, Value::fixnum(3)),
            other => panic!("expected Constant, got {:?}", other),
        }
        assert!(matches!(&compiled[1], CompiledForm::Eval(_)));
    }

    #[test]
    fn test_compile_defun_side_effect() {
        crate::test_utils::init_test_tracing();
        let mut eval = Context::new();
        // defun is no longer a special form; use defalias instead
        let forms = parse_forms("(defalias 'test-fc-fn #'(lambda () 99))").unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();
        assert_eq!(compiled.len(), 1);
        assert!(matches!(&compiled[0], CompiledForm::Eval(_)));
        // defalias should have registered the function at compile time.
        assert!(eval.obarray().symbol_function("test-fc-fn").is_some());
    }

    #[test]
    fn test_compile_defun_emits_compiled_defalias_form() {
        crate::test_utils::init_test_tracing();
        let mut eval = Context::new();
        let forms = parse_forms("(defun test-fc-byte (x) \"doc\" (+ x 1))").unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();
        assert_eq!(compiled.len(), 1);
        let CompiledForm::Eval(value) = &compiled[0] else {
            panic!("expected Eval form");
        };
        let items = list_to_vec(value).expect("compiled top-level form should be a list");
        assert_eq!(items[0].as_symbol_name(), Some("defalias"));
        assert!(items[2].get_bytecode_data().is_some());
        assert_eq!(items[3].as_str(), Some("doc"));
    }

    #[test]
    fn test_compile_multiple_forms() {
        crate::test_utils::init_test_tracing();
        let mut eval = Context::new();
        let forms = parse_forms(
            "(defvar test-fc-a 1)\n\
             (eval-when-compile (+ 2 3))\n\
             (defvar test-fc-b 10)",
        )
        .unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();
        assert_eq!(compiled.len(), 3);
        assert!(matches!(&compiled[0], CompiledForm::Eval(_)));
        match &compiled[1] {
            CompiledForm::Constant(v) => assert_eq!(*v, Value::fixnum(5)),
            other => panic!("expected Constant, got {:?}", other),
        }
        assert!(matches!(&compiled[2], CompiledForm::Eval(_)));
    }

    #[test]
    fn test_compile_empty_forms() {
        crate::test_utils::init_test_tracing();
        let mut eval = Context::new();
        let compiled = compile_file_forms(&mut eval, &[]).unwrap();
        assert!(compiled.is_empty());
    }

    #[test]
    fn test_compile_el_to_neobc_creates_file() {
        crate::test_utils::init_test_tracing();
        use crate::emacs_core::file_compile_format::read_neobc;

        let dir = tempfile::tempdir().unwrap();
        let el_path = dir.path().join("test-compile.el");
        let source = ";; -*- lexical-binding: nil -*-\n\
                      (eval-when-compile (setq test-compile-var 42))\n\
                      (defvar my-var 1)\n";
        std::fs::write(&el_path, source).unwrap();

        let mut eval = Context::new();
        compile_el_to_neobc(&mut eval, &el_path).unwrap();

        // Verify .neobc was created alongside the .el file.
        let neobc_path = el_path.with_extension("neobc");
        assert!(neobc_path.exists(), ".neobc file should be created");

        // Read back and verify contents.
        let loaded = read_neobc(&neobc_path, "").unwrap();
        assert!(!loaded.lexical_binding);
        assert_eq!(loaded.forms.len(), 2);
    }

    #[test]
    fn test_compile_el_to_neobc_round_trips_compiled_defun() {
        crate::test_utils::init_test_tracing();

        let dir = tempfile::tempdir().unwrap();
        let el_path = dir.path().join("compiled-defun.el");
        let source = ";; -*- lexical-binding: nil -*-\n\
                      (defun test-compiled-neobc (x) \"doc\" (+ x 1))\n";
        std::fs::write(&el_path, source).unwrap();

        let mut compiler_eval = Context::new();
        compile_el_to_neobc(&mut compiler_eval, &el_path).unwrap();

        let neobc_path = el_path.with_extension("neobc");
        let loaded = read_neobc(&neobc_path, "").unwrap();
        assert_eq!(loaded.forms.len(), 1);

        let mut runtime_eval = Context::new();
        for form in &loaded.forms {
            match form {
                LoadedForm::Eval(expr) => {
                    let value = runtime_eval.quote_to_runtime_value(expr);
                    runtime_eval.eval_sub(value).unwrap();
                }
                LoadedForm::Constant(_) | LoadedForm::EagerEval(_) => {}
            }
        }

        let func = runtime_eval
            .obarray()
            .symbol_function("test-compiled-neobc")
            .copied()
            .expect("compiled function should be installed");
        assert!(func.get_bytecode_data().is_some());
    }

    #[test]
    fn test_compile_el_to_neobc_lexical_binding() {
        crate::test_utils::init_test_tracing();
        use crate::emacs_core::file_compile_format::read_neobc;

        let dir = tempfile::tempdir().unwrap();
        let el_path = dir.path().join("lexical.el");
        let source = ";; -*- lexical-binding: t -*-\n(+ 1 2)\n";
        std::fs::write(&el_path, source).unwrap();

        let mut eval = Context::new();
        compile_el_to_neobc(&mut eval, &el_path).unwrap();

        let neobc_path = el_path.with_extension("neobc");
        let loaded = read_neobc(&neobc_path, "").unwrap();
        assert!(loaded.lexical_binding);
    }

    #[test]
    fn test_compile_el_to_neobc_restores_lexical_binding() {
        crate::test_utils::init_test_tracing();
        let dir = tempfile::tempdir().unwrap();
        let el_path = dir.path().join("restore.el");
        let source = ";; -*- lexical-binding: t -*-\n(+ 1 2)\n";
        std::fs::write(&el_path, source).unwrap();

        let mut eval = Context::new();
        assert!(!eval.lexical_binding(), "starts as dynamic");
        compile_el_to_neobc(&mut eval, &el_path).unwrap();
        assert!(!eval.lexical_binding(), "should be restored to dynamic");
    }

    #[test]
    fn test_compile_el_to_neobc_nonexistent_file() {
        crate::test_utils::init_test_tracing();
        let mut eval = Context::new();
        let result = compile_el_to_neobc(&mut eval, Path::new("/nonexistent/foo.el"));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), CompileFileError::Io(_)));
    }
}
