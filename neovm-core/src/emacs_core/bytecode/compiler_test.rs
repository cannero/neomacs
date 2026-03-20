use super::*;
use crate::emacs_core::parse_forms;

fn compile(src: &str) -> ByteCodeFunction {
    let forms = parse_forms(src).expect("parse");
    let mut compiler = Compiler::new(false);
    compiler.compile_toplevel(&forms[0])
}

#[test]
fn compile_literal_int() {
    let func = compile("42");
    assert_eq!(func.ops.len(), 2); // Constant + Return
    assert!(matches!(func.ops[0], Op::Constant(0)));
    assert!(matches!(func.ops[1], Op::Return));
    assert_eq!(func.constants[0].as_int(), Some(42));
}

#[test]
fn compile_nil_t() {
    let func = compile("nil");
    assert!(matches!(func.ops[0], Op::Nil));

    let func = compile("t");
    assert!(matches!(func.ops[0], Op::True));
}

#[test]
fn compile_addition() {
    let func = compile("(+ 1 2)");
    // Constant(1), Constant(2), Add, Return
    assert_eq!(func.ops.len(), 4);
    assert!(matches!(func.ops[2], Op::Add));
}

#[test]
fn compile_if() {
    let func = compile("(if t 1 2)");
    // Has GotoIfNil and Goto for branching
    let has_goto_nil = func.ops.iter().any(|op| matches!(op, Op::GotoIfNil(_)));
    assert!(has_goto_nil);
}

#[test]
fn compile_let() {
    let func = compile("(let ((x 1)) x)");
    let has_varbind = func.ops.iter().any(|op| matches!(op, Op::VarBind(_)));
    let has_unbind = func.ops.iter().any(|op| matches!(op, Op::Unbind(_)));
    assert!(has_varbind);
    assert!(has_unbind);
}

#[test]
fn compile_setq() {
    let func = compile("(setq x 42)");
    let has_varset = func.ops.iter().any(|op| matches!(op, Op::VarSet(_)));
    assert!(has_varset);
}

#[test]
fn compile_while() {
    let func = compile("(while nil 1)");
    let has_goto = func.ops.iter().any(|op| matches!(op, Op::Goto(_)));
    let has_goto_nil = func.ops.iter().any(|op| matches!(op, Op::GotoIfNil(_)));
    assert!(has_goto);
    assert!(has_goto_nil);
}

#[test]
fn compile_lambda() {
    let func = compile("(lambda (x) (+ x 1))");
    // Should have MakeClosure or Constant (depends on lexical mode)
    let has_constant = func.ops.iter().any(|op| matches!(op, Op::Constant(_)));
    assert!(has_constant);
}

#[test]
fn compile_quote() {
    let func = compile("'(1 2 3)");
    assert_eq!(func.ops.len(), 2); // Constant + Return
}

#[test]
fn compile_and_or() {
    let func = compile("(and 1 2 3)");
    let has_short_circuit = func
        .ops
        .iter()
        .any(|op| matches!(op, Op::GotoIfNilElsePop(_)));
    assert!(has_short_circuit);

    let func = compile("(or 1 2 3)");
    let has_short_circuit = func
        .ops
        .iter()
        .any(|op| matches!(op, Op::GotoIfNotNilElsePop(_)));
    assert!(has_short_circuit);
}

#[test]
fn compile_type_predicates() {
    let func = compile("(null x)");
    let has_not = func.ops.iter().any(|op| matches!(op, Op::Not));
    assert!(has_not);

    let func = compile("(consp x)");
    let has_consp = func.ops.iter().any(|op| matches!(op, Op::Consp));
    assert!(has_consp);
}

#[test]
fn compile_list_ops() {
    let func = compile("(car x)");
    assert!(func.ops.iter().any(|op| matches!(op, Op::Car)));

    let func = compile("(cdr x)");
    assert!(func.ops.iter().any(|op| matches!(op, Op::Cdr)));

    let func = compile("(cons 1 2)");
    assert!(func.ops.iter().any(|op| matches!(op, Op::Cons)));
}

#[test]
fn compile_progn() {
    let func = compile("(progn 1 2 3)");
    // Should only keep last value
    assert!(matches!(func.ops.last(), Some(Op::Return)));
}

#[test]
fn disassemble_output() {
    let func = compile("(+ 1 2)");
    let dis = func.disassemble();
    assert!(dis.contains("add"));
    assert!(dis.contains("constant"));
}

#[test]
fn compile_cond() {
    let func = compile("(cond (nil 1) (t 2))");
    // cond compiles to a series of conditional branches
    let has_goto_nil = func.ops.iter().any(|op| matches!(op, Op::GotoIfNil(_)));
    assert!(has_goto_nil);
}

#[test]
fn compile_when() {
    let func = compile("(when t 1 2)");
    let has_goto_nil = func.ops.iter().any(|op| matches!(op, Op::GotoIfNil(_)));
    assert!(has_goto_nil);
}

#[test]
fn compile_unless() {
    let func = compile("(unless nil 1)");
    // unless branches when condition is NOT nil
    let has_goto_not_nil = func.ops.iter().any(|op| matches!(op, Op::GotoIfNotNil(_)));
    let has_goto_nil = func.ops.iter().any(|op| matches!(op, Op::GotoIfNil(_)));
    // Either branching strategy is valid
    assert!(has_goto_not_nil || has_goto_nil);
}

#[test]
fn compile_catch() {
    let func = compile("(catch 'tag (+ 1 2))");
    // catch compiles via compile_catch which uses PushCatch + PopHandler
    let has_handler = func.ops.iter().any(|op| matches!(op, Op::PushCatch(_)));
    assert!(has_handler);
    let has_pop = func.ops.iter().any(|op| matches!(op, Op::PopHandler));
    assert!(has_pop);
}

#[test]
fn compile_unwind_protect() {
    let func = compile("(unwind-protect 1 2)");
    let has_unwind = func.ops.iter().any(|op| matches!(op, Op::UnwindProtect(_)));
    assert!(has_unwind);
}

#[test]
fn compile_condition_case() {
    let func = compile("(condition-case err (error \"boom\") (error err))");
    let has_push_cc = func
        .ops
        .iter()
        .any(|op| matches!(op, Op::PushConditionCase(_) | Op::PushConditionCaseRaw(_)));
    assert!(has_push_cc);
}

#[test]
fn compile_prog1() {
    let func = compile("(prog1 1 2 3)");
    // prog1 keeps the first value — needs discard operations
    assert!(matches!(func.ops.last(), Some(Op::Return)));
}

#[test]
fn compile_defun() {
    let func = compile("(defun my-fn (x) (+ x 1))");
    // defun should produce a constant (the function) and a call to defalias/fset
    let has_constant = func.ops.iter().any(|op| matches!(op, Op::Constant(_)));
    assert!(has_constant);
}

#[test]
fn compile_dotimes() {
    let func = compile("(dotimes (i 10) i)");
    // dotimes uses a counter loop with goto
    let has_goto = func.ops.iter().any(|op| matches!(op, Op::Goto(_)));
    assert!(has_goto);
}

#[test]
fn compile_dolist() {
    let func = compile("(dolist (x '(1 2 3)) x)");
    // dolist iterates a list with goto
    let has_goto = func.ops.iter().any(|op| matches!(op, Op::Goto(_)));
    assert!(has_goto);
}

#[test]
fn compile_let_star() {
    let func = compile("(let* ((x 1) (y x)) y)");
    // let* uses sequential binding
    let varbind_count = func
        .ops
        .iter()
        .filter(|op| matches!(op, Op::VarBind(_)))
        .count();
    assert_eq!(varbind_count, 2);
}

#[test]
fn compile_save_excursion() {
    // save-excursion is compiled as a progn (stub), so body should still produce a value
    let func = compile("(save-excursion 1)");
    // Should have the constant 1 in it
    assert!(func.ops.iter().any(|op| matches!(op, Op::Constant(_))));
}

#[test]
fn compile_subtraction_and_multiplication() {
    let func = compile("(- 3 1)");
    assert!(func.ops.iter().any(|op| matches!(op, Op::Sub)));

    let func = compile("(* 2 3)");
    assert!(func.ops.iter().any(|op| matches!(op, Op::Mul)));
}

#[test]
fn compile_comparisons() {
    let func = compile("(< 1 2)");
    assert!(func.ops.iter().any(|op| matches!(op, Op::Lss)));

    let func = compile("(> 1 2)");
    assert!(func.ops.iter().any(|op| matches!(op, Op::Gtr)));

    let func = compile("(= 1 1)");
    assert!(func.ops.iter().any(|op| matches!(op, Op::Eqlsign)));
}
