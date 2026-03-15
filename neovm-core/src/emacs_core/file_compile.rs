//! File-level byte compilation.
//!
//! Processes top-level forms from a parsed `.el` file, evaluating
//! `eval-when-compile` bodies at compile time and emitting the results as
//! constants.  All other forms are evaluated for side effects (so that
//! `defun`, `defvar`, `require`, etc. take effect in the compile-time
//! environment) and also emitted as `Eval` forms to replay at load time.

use super::error::Flow;
use super::eval::{Evaluator, quote_to_value};
use super::expr::Expr;
use super::intern::resolve_sym;
use super::value::Value;

/// A single compiled top-level form.
#[derive(Clone, Debug)]
pub enum CompiledForm {
    /// A form to evaluate at load time (already macro-expanded).
    Eval(Value),
    /// A constant produced by `eval-when-compile` — body was evaluated at
    /// compile time and only the result value is retained.
    Constant(Value),
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
pub fn compile_file_forms(eval: &mut Evaluator, forms: &[Expr]) -> Result<Vec<CompiledForm>, Flow> {
    let mut compiled = Vec::new();
    for form in forms {
        compile_toplevel_file_form(eval, form, &mut compiled)?;
    }
    Ok(compiled)
}

/// Process a single top-level form, appending results to `out`.
fn compile_toplevel_file_form(
    eval: &mut Evaluator,
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
                        // Evaluate body NOW (compile-time side effects) and
                        // ALSO emit it so it runs again at load time.
                        eval.sf_progn(&items[1..])?;
                        out.push(CompiledForm::Eval(quote_to_value(form)));
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    // Default: evaluate for side effects, emit as Eval form.
    eval.eval(form)?;
    out.push(CompiledForm::Eval(quote_to_value(form)));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emacs_core::parser::parse_forms;

    #[test]
    fn test_compile_simple_form() {
        let mut eval = Evaluator::new();
        let forms = parse_forms("(+ 1 2)").unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();
        assert_eq!(compiled.len(), 1);
        assert!(matches!(&compiled[0], CompiledForm::Eval(_)));
    }

    #[test]
    fn test_compile_eval_when_compile() {
        let mut eval = Evaluator::new();
        let forms = parse_forms("(eval-when-compile (+ 10 20))").unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();
        assert_eq!(compiled.len(), 1);
        match &compiled[0] {
            CompiledForm::Constant(v) => assert_eq!(*v, Value::Int(30)),
            other => panic!("expected Constant, got {:?}", other),
        }
    }

    #[test]
    fn test_compile_eval_and_compile() {
        let mut eval = Evaluator::new();
        let forms = parse_forms("(eval-and-compile (defvar test-fc-var 42))").unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();
        assert_eq!(compiled.len(), 1);
        assert!(matches!(&compiled[0], CompiledForm::Eval(_)));
        // The defvar should have taken effect at compile time.
        let val = eval.obarray().symbol_value("test-fc-var");
        assert_eq!(val, Some(&Value::Int(42)));
    }

    #[test]
    fn test_compile_progn_flattens() {
        let mut eval = Evaluator::new();
        let forms = parse_forms("(progn (+ 1 2) (+ 3 4))").unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();
        // progn with 2 sub-forms should produce 2 CompiledForm entries.
        assert_eq!(compiled.len(), 2);
        assert!(matches!(&compiled[0], CompiledForm::Eval(_)));
        assert!(matches!(&compiled[1], CompiledForm::Eval(_)));
    }

    #[test]
    fn test_compile_progn_with_eval_when_compile() {
        let mut eval = Evaluator::new();
        let forms = parse_forms("(progn (eval-when-compile (+ 1 2)) (+ 3 4))").unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();
        assert_eq!(compiled.len(), 2);
        match &compiled[0] {
            CompiledForm::Constant(v) => assert_eq!(*v, Value::Int(3)),
            other => panic!("expected Constant, got {:?}", other),
        }
        assert!(matches!(&compiled[1], CompiledForm::Eval(_)));
    }

    #[test]
    fn test_compile_defun_side_effect() {
        let mut eval = Evaluator::new();
        let forms = parse_forms("(defun test-fc-fn () 99)").unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();
        assert_eq!(compiled.len(), 1);
        assert!(matches!(&compiled[0], CompiledForm::Eval(_)));
        // defun should have registered the function at compile time.
        assert!(eval.obarray().symbol_function("test-fc-fn").is_some());
    }

    #[test]
    fn test_compile_multiple_forms() {
        let mut eval = Evaluator::new();
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
            CompiledForm::Constant(v) => assert_eq!(*v, Value::Int(5)),
            other => panic!("expected Constant, got {:?}", other),
        }
        assert!(matches!(&compiled[2], CompiledForm::Eval(_)));
    }

    #[test]
    fn test_compile_empty_forms() {
        let mut eval = Evaluator::new();
        let compiled = compile_file_forms(&mut eval, &[]).unwrap();
        assert!(compiled.is_empty());
    }
}
