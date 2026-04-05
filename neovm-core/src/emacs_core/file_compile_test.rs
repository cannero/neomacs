use super::*;
use crate::emacs_core::file_compile_format::{LoadedForm, read_neobc};
use crate::emacs_core::load::{get_load_path, load_file};
use crate::emacs_core::parser::parse_forms;
use crate::emacs_core::print::print_expr;

fn bootstrap_fixture_path(load_path: &[String], logical_name: &str) -> std::path::PathBuf {
    for dir in load_path {
        let candidate = std::path::PathBuf::from(dir).join(format!("{logical_name}.el"));
        if candidate.exists() {
            return candidate;
        }
    }
    panic!("bootstrap file not found: {logical_name}");
}

fn expr_contains_symbol_named(expr: &Expr, target: &str) -> bool {
    match expr {
        Expr::Symbol(id) => resolve_sym(*id) == target,
        Expr::List(items) | Expr::Vector(items) => items
            .iter()
            .any(|item| expr_contains_symbol_named(item, target)),
        Expr::DottedList(items, tail) => {
            items
                .iter()
                .any(|item| expr_contains_symbol_named(item, target))
                || expr_contains_symbol_named(tail, target)
        }
        _ => false,
    }
}

fn minimal_compile_surface_eval() -> Context {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let lisp_dir = project_root.join("lisp");

    let mut eval = Context::new();
    let mut load_path_entries = Vec::new();
    for sub in ["", "emacs-lisp"] {
        let dir = if sub.is_empty() {
            lisp_dir.clone()
        } else {
            lisp_dir.join(sub)
        };
        if dir.is_dir() {
            load_path_entries.push(Value::string(dir.to_string_lossy().to_string()));
        }
    }
    eval.set_variable("load-path", Value::list(load_path_entries));
    eval.set_variable("dump-mode", Value::symbol("pbootstrap"));
    eval.set_variable("purify-flag", Value::NIL);
    eval.set_variable("max-lisp-eval-depth", Value::fixnum(1600));
    eval.set_variable(
        "macroexp--pending-eager-loads",
        Value::list(vec![Value::symbol("skip")]),
    );

    let load_path = get_load_path(&eval.obarray());
    for name in &[
        "emacs-lisp/debug-early",
        "emacs-lisp/byte-run",
        "emacs-lisp/backquote",
        "subr",
        "emacs-lisp/macroexp",
        "emacs-lisp/pcase",
    ] {
        let path = bootstrap_fixture_path(&load_path, name);
        load_file(&mut eval, &path).unwrap_or_else(|err| {
            panic!("failed loading {name} from {}: {:?}", path.display(), err)
        });
    }

    eval.require_value(Value::symbol("gv"), None, None)
        .expect("require gv for macroexpansion");
    eval.set_variable("macroexp--pending-eager-loads", Value::NIL);
    eval
}

fn explicit_macroexpand_env_with_hidden_macro(eval: &mut Context, name: &str) -> Value {
    let macro_form = parse_forms(&format!(
        r#"(defmacro {name} (x)
                 `(let ((tmp ,x)) tmp))"#
    ))
    .expect("parse helper macro");
    eval.eval_expr(&macro_form[0])
        .expect("install helper macro for explicit env");

    let definition = eval
        .obarray()
        .symbol_function(name)
        .copied()
        .expect("helper macro definition");
    let expander = lowered_macro_expander(definition).expect("helper macro expander");
    eval.obarray_mut().set_symbol_function(name, Value::NIL);

    Value::list(vec![Value::cons(Value::symbol(name), expander)])
}

fn eval_source_file_direct(eval: &mut Context, path: &std::path::Path) {
    let source = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    let forms =
        parse_forms(&source).unwrap_or_else(|err| panic!("parse {}: {err}", path.display()));

    let old_lexical = eval.lexical_binding();
    let old_lexenv = eval.lexenv;
    let old_load_file = eval.obarray().symbol_value("load-file-name").cloned();

    eval.with_gc_scope_result(|ctx| {
        ctx.root(old_lexenv);
        if let Some(old) = old_load_file {
            ctx.root(old);
        }

        ctx.set_lexical_binding(true);
        ctx.lexenv = Value::list(vec![Value::T]);
        ctx.set_variable(
            "load-file-name",
            Value::string(path.to_string_lossy().to_string()),
        );

        for form in &forms {
            ctx.eval_expr(form).unwrap_or_else(|err| {
                panic!(
                    "direct source load failed for {}: {:?}",
                    path.display(),
                    err
                )
            });
        }

        ctx.set_lexical_binding(old_lexical);
        ctx.lexenv = old_lexenv;
        if let Some(old) = old_load_file {
            ctx.set_variable("load-file-name", old);
        } else {
            ctx.set_variable("load-file-name", Value::NIL);
        }

        Ok(Value::NIL)
    })
    .expect("direct source load should succeed");
}

fn direct_source_compile_surface_eval(include_pcase: bool) -> Context {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let lisp_dir = project_root.join("lisp");

    let mut eval = Context::new();
    let mut load_path_entries = Vec::new();
    for sub in ["", "emacs-lisp"] {
        let dir = if sub.is_empty() {
            lisp_dir.clone()
        } else {
            lisp_dir.join(sub)
        };
        if dir.is_dir() {
            load_path_entries.push(Value::string(dir.to_string_lossy().to_string()));
        }
    }
    eval.set_variable("load-path", Value::list(load_path_entries));
    eval.set_variable("dump-mode", Value::symbol("pbootstrap"));
    eval.set_variable("purify-flag", Value::NIL);
    eval.set_variable("max-lisp-eval-depth", Value::fixnum(1600));
    eval.set_variable(
        "macroexp--pending-eager-loads",
        Value::list(vec![Value::symbol("skip")]),
    );

    let load_path = get_load_path(&eval.obarray());
    for name in &[
        "emacs-lisp/debug-early",
        "emacs-lisp/byte-run",
        "emacs-lisp/backquote",
        "subr",
        "emacs-lisp/macroexp",
    ] {
        let path = bootstrap_fixture_path(&load_path, name);
        eval_source_file_direct(&mut eval, &path);
    }

    if include_pcase {
        let pcase_path = bootstrap_fixture_path(&load_path, "emacs-lisp/pcase");
        eval_source_file_direct(&mut eval, &pcase_path);

        // GNU loadup reloads macroexp after pcase defines the backquote
        // macroexpander used by macroexpand-all.
        let macroexp_path = bootstrap_fixture_path(&load_path, "emacs-lisp/macroexp");
        eval_source_file_direct(&mut eval, &macroexp_path);
    }

    eval.set_variable("macroexp--pending-eager-loads", Value::NIL);
    eval
}

fn real_custom_defmacro_form(name: &str) -> Expr {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let source =
        std::fs::read_to_string(project_root.join("lisp/custom.el")).expect("read custom.el");
    let forms = parse_forms(&source).expect("parse custom.el");
    forms
        .into_iter()
        .find(|form| match form {
            Expr::List(items) => matches!(
                (items.first(), items.get(1)),
                (Some(Expr::Symbol(id0)), Some(Expr::Symbol(id1)))
                    if resolve_sym(*id0) == "defmacro" && resolve_sym(*id1) == name
            ),
            _ => false,
        })
        .unwrap_or_else(|| panic!("find defmacro {name} in custom.el"))
}

fn real_inline_defmacro_form(name: &str) -> Expr {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let source = std::fs::read_to_string(project_root.join("lisp/emacs-lisp/inline.el"))
        .expect("read inline.el");
    let forms = parse_forms(&source).expect("parse inline.el");
    forms
        .into_iter()
        .find(|form| match form {
            Expr::List(items) => matches!(
                (items.first(), items.get(1)),
                (Some(Expr::Symbol(id0)), Some(Expr::Symbol(id1)))
                    if resolve_sym(*id0) == "defmacro" && resolve_sym(*id1) == name
            ),
            _ => false,
        })
        .unwrap_or_else(|| panic!("find defmacro {name} in inline.el"))
}

fn real_cl_macs_defmacro_form(name: &str) -> Expr {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let source = std::fs::read_to_string(project_root.join("lisp/emacs-lisp/cl-macs.el"))
        .expect("read cl-macs.el");
    let forms = parse_forms(&source).expect("parse cl-macs.el");
    forms
        .into_iter()
        .find(|form| match form {
            Expr::List(items) => matches!(
                (items.first(), items.get(1)),
                (Some(Expr::Symbol(id0)), Some(Expr::Symbol(id1)))
                    if resolve_sym(*id0) == "defmacro" && resolve_sym(*id1) == name
            ),
            _ => false,
        })
        .unwrap_or_else(|| panic!("find defmacro {name} in cl-macs.el"))
}

fn macroexp_accumulate_defmacro_form() -> Expr {
    parse_forms(
        r#"
(defmacro macroexp--accumulate (var+list &rest body)
  (let ((var (car var+list))
        (list (cadr var+list))
        (shared (make-symbol "shared"))
        (unshared (make-symbol "unshared"))
        (tail (make-symbol "tail"))
        (new-el (make-symbol "new-el")))
    `(let* ((,shared ,list)
            (,unshared nil)
            (,tail ,shared)
            ,var ,new-el)
       (while (consp ,tail)
         (setq ,var (car ,tail)
               ,new-el (progn ,@body))
         (unless (eq ,var ,new-el)
           (while (not (eq ,shared ,tail))
             (push (pop ,shared) ,unshared))
           (setq ,shared (cdr ,shared))
           (push ,new-el ,unshared))
         (setq ,tail (cdr ,tail)))
       (nconc (nreverse ,unshared) ,shared))))
"#,
    )
    .expect("parse macroexp--accumulate")
    .into_iter()
    .next()
    .expect("one form")
}

fn real_cus_face_defun_form(name: &str) -> Expr {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let source =
        std::fs::read_to_string(project_root.join("lisp/cus-face.el")).expect("read cus-face.el");
    let forms = parse_forms(&source).expect("parse cus-face.el");
    forms
        .into_iter()
        .find(|form| match form {
            Expr::List(items) => matches!(
                (items.first(), items.get(1)),
                (Some(Expr::Symbol(id0)), Some(Expr::Symbol(id1)))
                    if resolve_sym(*id0) == "defun" && resolve_sym(*id1) == name
            ),
            _ => false,
        })
        .unwrap_or_else(|| panic!("find defun {name} in cus-face.el"))
}

fn normalize_uninterned_expr(
    expr: &Expr,
    ids: &mut std::collections::HashMap<SymId, SymId>,
    next_id: &mut usize,
) -> Expr {
    match expr {
        Expr::Symbol(id)
            if !super::super::intern::is_canonical_id(*id)
                && !matches!(super::super::intern::lookup_interned(resolve_sym(*id)), Some(existing) if existing == *id) =>
        {
            let placeholder = *ids.entry(*id).or_insert_with(|| {
                let current = *next_id;
                *next_id += 1;
                intern(&format!("__uninterned_{}_{current}__", resolve_sym(*id)))
            });
            Expr::Symbol(placeholder)
        }
        Expr::List(items) => Expr::List(
            items
                .iter()
                .map(|item| normalize_uninterned_expr(item, ids, next_id))
                .collect(),
        ),
        Expr::Vector(items) => Expr::Vector(
            items
                .iter()
                .map(|item| normalize_uninterned_expr(item, ids, next_id))
                .collect(),
        ),
        Expr::DottedList(items, tail) => Expr::DottedList(
            items
                .iter()
                .map(|item| normalize_uninterned_expr(item, ids, next_id))
                .collect(),
            Box::new(normalize_uninterned_expr(tail, ids, next_id)),
        ),
        _ => expr.clone(),
    }
}

fn normalized_expr(expr: &Expr) -> Expr {
    let mut ids = std::collections::HashMap::new();
    let mut next_id = 0;
    normalize_uninterned_expr(expr, &mut ids, &mut next_id)
}

fn normalized_value(value: Value) -> Expr {
    normalized_expr(&value_to_expr(&value))
}

fn defalias_target_bytecode(
    value: Value,
) -> Option<&'static super::super::bytecode::ByteCodeFunction> {
    if let Some(bytecode) = value.get_bytecode_data() {
        return Some(bytecode);
    }
    if value.is_cons() && value.cons_car().is_symbol_named("macro") {
        return value.cons_cdr().get_bytecode_data();
    }
    let items = list_to_vec(&value)?;
    match items.as_slice() {
        [head, tag, bytecode]
            if head.as_symbol_name() == Some("cons") && is_quoted_symbol(*tag, "macro") =>
        {
            bytecode.get_bytecode_data()
        }
        _ => None,
    }
}

fn assert_same_compiled_defalias(label: &str, left: Value, right: Value) {
    let left_items = list_to_vec(&left).unwrap_or_else(|| {
        panic!(
            "left compiled form should be a list, got {}",
            crate::emacs_core::print::print_value(&left)
        )
    });
    let right_items = list_to_vec(&right).unwrap_or_else(|| {
        panic!(
            "right compiled form should be a list, got {}",
            crate::emacs_core::print::print_value(&right)
        )
    });
    assert_eq!(left_items[0], right_items[0], "{label} head");
    assert_eq!(
        quoted_symbol_id(left_items[1]),
        quoted_symbol_id(right_items[1]),
        "{label} name",
    );
    assert_eq!(left_items.get(3), right_items.get(3), "{label} docstring");

    let left_bc = defalias_target_bytecode(left_items[2]).expect("left bytecode target");
    let right_bc = defalias_target_bytecode(right_items[2]).expect("right bytecode target");
    assert_eq!(left_bc.ops, right_bc.ops, "{label} ops");
    let left_constants: Vec<Expr> = left_bc
        .constants
        .iter()
        .copied()
        .map(normalized_value)
        .collect();
    let right_constants: Vec<Expr> = right_bc
        .constants
        .iter()
        .copied()
        .map(normalized_value)
        .collect();
    assert_eq!(left_constants, right_constants, "{label} constants");
    assert_eq!(left_bc.params, right_bc.params, "{label} params");
    assert_eq!(left_bc.lexical, right_bc.lexical, "{label} lexical");
    assert_eq!(
        left_bc.docstring, right_bc.docstring,
        "{label} bytecode docstring"
    );
    assert_eq!(
        left_bc.doc_form.map(normalized_value),
        right_bc.doc_form.map(normalized_value),
        "{label} bytecode doc form"
    );
    assert_eq!(
        left_bc.interactive.map(normalized_value),
        right_bc.interactive.map(normalized_value),
        "{label} interactive spec"
    );
}

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
fn test_compile_defmacro_emits_compiled_macro_defalias_form() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let forms = parse_forms("(defmacro test-fc-macro (x) \"doc\" x)").unwrap();
    let compiled = compile_file_forms(&mut eval, &forms).unwrap();
    assert_eq!(compiled.len(), 1);

    let CompiledForm::Eval(value) = &compiled[0] else {
        panic!("expected Eval form");
    };
    let items = list_to_vec(value).expect("compiled top-level form should be a list");
    assert_eq!(items[0].as_symbol_name(), Some("defalias"));
    assert_eq!(items[3].as_str(), Some("doc"));

    let target = list_to_vec(&items[2]).expect("macro target should be a list");
    assert_eq!(target[0].as_symbol_name(), Some("cons"));
    assert!(is_quoted_symbol(target[1], "macro"));
    assert!(target[2].get_bytecode_data().is_some());

    let mut runtime_eval = Context::new();
    runtime_eval.eval_sub(*value).unwrap();
    let func = runtime_eval
        .obarray()
        .symbol_function("test-fc-macro")
        .copied()
        .expect("macro function should be installed");
    assert_eq!(func.cons_car().as_symbol_name(), Some("macro"));
    assert!(func.cons_cdr().get_bytecode_data().is_some());
}

#[test]
fn test_compile_defalias_emits_compiled_defalias_form() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let forms = parse_forms("(defalias 'test-fc-alias #'(lambda (x) \"doc\" (+ x 1)))").unwrap();
    let compiled = compile_file_forms(&mut eval, &forms).unwrap();
    assert_eq!(compiled.len(), 1);

    let CompiledForm::Eval(value) = &compiled[0] else {
        panic!("expected Eval form");
    };
    let items = list_to_vec(value).expect("compiled top-level form should be a list");
    assert_eq!(items[0].as_symbol_name(), Some("defalias"));
    assert_eq!(quoted_symbol_id(items[1]), Some(intern("test-fc-alias")));
    assert!(items[2].get_bytecode_data().is_some());
    assert_eq!(
        items[2]
            .get_bytecode_data()
            .and_then(|bytecode| bytecode.docstring.as_deref()),
        Some("doc")
    );
}

#[test]
fn test_compile_macroexpanded_defalias_expr_threads_explicit_macro_env_into_lambda_body() {
    crate::test_utils::init_test_tracing();

    let mut eval = minimal_compile_surface_eval();
    let macroexpand_env =
        explicit_macroexpand_env_with_hidden_macro(&mut eval, "test-fc-localmacro");
    let form = parse_forms("(defalias 'test-fc-localenv #'(lambda (x) (test-fc-localmacro x)))")
        .expect("parse defalias with explicit-env macro");

    let compiled =
        compile_macroexpanded_defalias_expr_with_env(&mut eval, &form[0], macroexpand_env)
            .expect("expr defalias lowering should honor explicit macro env");
    eval.eval_sub(compiled)
        .expect("install expr lowered explicit-env defalias");

    let call = parse_forms("(test-fc-localenv 42)").expect("parse explicit-env helper call");
    assert_eq!(
        eval.eval_expr(&call[0])
            .expect("expr lowered helper should run"),
        Value::fixnum(42)
    );
}

#[test]
fn test_compile_macroexpanded_defalias_value_threads_explicit_macro_env_into_lambda_body() {
    crate::test_utils::init_test_tracing();

    let mut eval = minimal_compile_surface_eval();
    let macroexpand_env =
        explicit_macroexpand_env_with_hidden_macro(&mut eval, "test-fc-localmacro");
    let source = "(defalias 'test-fc-localenv #'(lambda (x) (test-fc-localmacro x)))";
    let form = parse_forms(source).expect("parse defalias with explicit-env macro");
    let form_value = quote_to_value(&form[0]);
    let form_items = list_to_vec(&form_value).expect("value defalias should be a list");
    let target = *form_items
        .get(2)
        .expect("value defalias should have a target form");
    let target_items = list_to_vec(&target).expect("value defalias target should be a list");
    assert_eq!(
        target_items
            .first()
            .and_then(|value| value.as_symbol_name()),
        Some("function"),
        "value defalias target should be a function form"
    );
    let lambda = *target_items
        .get(1)
        .expect("function form should contain lambda");
    let lambda_items = list_to_vec(&lambda).expect("function target should contain lambda list");
    assert_eq!(
        lambda_items
            .first()
            .and_then(|value| value.as_symbol_name()),
        Some("lambda"),
        "function target should contain lambda form"
    );
    assert!(
        compile_lambda_expanded_value_with_env(&mut eval, lambda, macroexpand_env).is_some(),
        "value lambda body should lower under explicit macro env"
    );

    let form =
        parse_forms(source).expect("reparse defalias with explicit-env macro for function form");
    let form_value = quote_to_value(&form[0]);
    let form_items = list_to_vec(&form_value).expect("value defalias should be a list");
    let target = *form_items
        .get(2)
        .expect("value defalias should have a target form");
    assert!(
        compile_function_expanded_value_with_env(&mut eval, target, macroexpand_env).is_some(),
        "value function form should lower under explicit macro env"
    );

    let form =
        parse_forms(source).expect("reparse defalias with explicit-env macro for value path");
    let form_value = quote_to_value(&form[0]);
    let compiled =
        compile_macroexpanded_defalias_value_with_env(&mut eval, form_value, macroexpand_env)
            .expect("value defalias lowering should honor explicit macro env");
    let form = parse_forms(source).expect("reparse defalias with explicit-env macro for expr path");
    let expr_compiled =
        compile_macroexpanded_defalias_expr_with_env(&mut eval, &form[0], macroexpand_env)
            .expect("expr defalias lowering should honor explicit macro env");
    assert_same_compiled_defalias("explicit-env defalias", expr_compiled, compiled);
    eval.eval_sub(compiled)
        .expect("install value lowered explicit-env defalias");

    let call = parse_forms("(test-fc-localenv 42)").expect("parse explicit-env helper call");
    assert_eq!(
        eval.eval_expr(&call[0])
            .expect("value lowered helper should run"),
        Value::fixnum(42)
    );
}

#[test]
fn test_compile_file_forms_same_file_defalias_helper_call_uses_compiler_function_env() {
    crate::test_utils::init_test_tracing();
    let forms = parse_forms(
        r#"
(defalias 'test-fc-helper-via-defalias
  #'(lambda (x) x))

(test-fc-helper-via-defalias 42)
"#,
    )
    .unwrap();

    let mut eval = Context::new();
    let mut compiled = Vec::new();
    let mut compiler_macro_env = Value::NIL;
    let mut compiler_function_overrides = Value::NIL;
    let mut deferred_defmacros = Vec::new();

    compile_toplevel_file_form(
        &mut eval,
        &forms[0],
        &mut compiled,
        &mut compiler_macro_env,
        &mut compiler_function_overrides,
        &mut deferred_defmacros,
    )
    .expect("helper defalias should compile");
    eval.set_variable(
        INTERNAL_COMPILER_FUNCTION_OVERRIDES,
        compiler_function_overrides,
    );

    compile_toplevel_file_form(
        &mut eval,
        &forms[1],
        &mut compiled,
        &mut compiler_macro_env,
        &mut compiler_function_overrides,
        &mut deferred_defmacros,
    )
    .expect("same-file defalias helper call should compile");

    let CompiledForm::Eval(first) = &compiled[0] else {
        panic!("expected compiled defalias for helper");
    };
    let items = list_to_vec(first).expect("compiled defalias should be a list");
    assert_eq!(
        compiled_defalias_name_id(*first),
        Some(intern("test-fc-helper-via-defalias"))
    );
    assert!(compiled_function_binding_from_defalias(*first).is_some());
    assert!(defalias_target_bytecode(items[2]).is_some());
    assert_eq!(compiled.len(), 2);
}

#[test]
fn test_compile_defmacro_runtime_executes_gensym_backquote() {
    crate::test_utils::init_test_tracing();
    let macro_src = r#"
(defmacro test-fc-gensym (x)
  (let ((tmp (make-symbol "tmp")))
    `(let ((,tmp ,x)) ,tmp)))
"#;
    let macro_forms = parse_forms(macro_src).unwrap();

    let mut compiled_eval = direct_source_compile_surface_eval(true);
    let compiled = compile_file_forms(&mut compiled_eval, &macro_forms).unwrap();
    assert_eq!(compiled.len(), 1);
    let CompiledForm::Eval(compiled_value) = &compiled[0] else {
        panic!("expected compiled defmacro form");
    };
    compiled_eval
        .eval_sub(*compiled_value)
        .expect("compiled defmacro should install");
    let compiled_call = parse_forms("(test-fc-gensym 42)").unwrap();
    let compiled_result = compiled_eval
        .eval_expr(&compiled_call[0])
        .expect("compiled macro call should succeed");
    assert_eq!(compiled_result, Value::fixnum(42));
}

#[test]
fn test_compile_defmacro_runtime_executes_gensym_loop_body() {
    crate::test_utils::init_test_tracing();
    let macro_src = r#"
(defmacro test-fc-gensym-loop (list)
  (let ((shared (make-symbol "shared"))
        (tail (make-symbol "tail"))
        (new-el (make-symbol "new-el")))
    `(let* ((,shared ,list)
            (,tail ,shared)
            ,new-el)
       (while (consp ,tail)
         (setq ,new-el (car ,tail))
         (setq ,tail (cdr ,tail)))
       ,new-el)))
"#;
    let macro_forms = parse_forms(macro_src).unwrap();

    let mut compiled_eval = direct_source_compile_surface_eval(true);
    let compiled = compile_file_forms(&mut compiled_eval, &macro_forms).unwrap();
    assert_eq!(compiled.len(), 1);
    let CompiledForm::Eval(compiled_value) = &compiled[0] else {
        panic!("expected compiled defmacro form");
    };
    compiled_eval
        .eval_sub(*compiled_value)
        .expect("compiled defmacro should install");
    let compiled_call = parse_forms("(test-fc-gensym-loop '(1 2 3))").unwrap();
    let compiled_result = compiled_eval
        .eval_expr(&compiled_call[0])
        .expect("compiled macro call should succeed");
    assert_eq!(compiled_result, Value::fixnum(3));
}

#[test]
fn test_compile_defmacro_runtime_preserves_easy_mmode_quote_shape() {
    crate::test_utils::init_test_tracing();
    let macro_src = r#"
(defmacro test-fc-easy-mmode-shape (mode getter)
  (let ((type nil))
    (unless type (setq type '(:type 'boolean)))
    `(progn
       (defcustom ,mode nil "doc" ,@type)
       ,(let ((modevar (pcase getter (`(default-value ',v) v) (_ getter)))
              (minor-modes-var 'local-minor-modes))
          (if (not (symbolp modevar))
              (error "bad modevar")
            `(with-no-warnings
               (when (boundp ',minor-modes-var)
                 (setq ,minor-modes-var
                       (delq ',modevar ,minor-modes-var)))))))))
"#;
    let macro_forms = parse_forms(macro_src).unwrap();

    let mut source_eval = direct_source_compile_surface_eval(true);
    for form in &macro_forms {
        source_eval
            .eval_expr(form)
            .expect("source easy-mmode shape macro should install");
    }

    let mut compiled_eval = direct_source_compile_surface_eval(true);
    let compiled = compile_file_forms(&mut compiled_eval, &macro_forms).unwrap();
    assert_eq!(compiled.len(), 1);
    let CompiledForm::Eval(compiled_value) = &compiled[0] else {
        panic!("expected compiled defmacro form");
    };
    compiled_eval
        .eval_sub(*compiled_value)
        .expect("compiled easy-mmode shape macro should install");

    let macroexpand =
        parse_forms("(macroexpand '(test-fc-easy-mmode-shape sample-mode sample-mode))").unwrap();
    let source_expanded = source_eval
        .eval_expr(&macroexpand[0])
        .expect("source easy-mmode shape macroexpand should succeed");
    let compiled_expanded = compiled_eval
        .eval_expr(&macroexpand[0])
        .expect("compiled easy-mmode shape macroexpand should succeed");

    assert_eq!(
        normalized_value(compiled_expanded),
        normalized_value(source_expanded),
        "compiled easy-mmode-style macro expansion should match source"
    );
}

#[test]
fn test_compile_defmacro_runtime_preserves_condition_case_handler_symbols() {
    crate::test_utils::init_test_tracing();
    let macro_src = r#"
(define-error 'test-fc-invalid-place "Invalid place")

(defmacro test-fc-condition-case-handler ()
  (condition-case err
      (signal 'test-fc-invalid-place '(bad))
    (test-fc-invalid-place
     `(handled ',(car err) ',(cdr err)))))
"#;
    let forms = parse_forms(macro_src).unwrap();

    let mut source_eval = direct_source_compile_surface_eval(false);
    for form in &forms {
        source_eval
            .eval_expr(form)
            .expect("source condition-case handler forms should install");
    }

    let mut compiled_eval = direct_source_compile_surface_eval(false);
    let compiled = compile_file_forms(&mut compiled_eval, &forms).unwrap();
    for form in &compiled {
        match form {
            CompiledForm::Eval(value) | CompiledForm::EagerEval(value) => {
                compiled_eval
                    .eval_sub(*value)
                    .expect("compiled condition-case handler form should install");
            }
            CompiledForm::Constant(_) => {}
        }
    }

    let macroexpand = parse_forms("(macroexpand '(test-fc-condition-case-handler))").unwrap();
    let source_expanded = source_eval
        .eval_expr(&macroexpand[0])
        .expect("source condition-case handler macroexpand should succeed");
    let compiled_expanded = compiled_eval
        .eval_expr(&macroexpand[0])
        .expect("compiled condition-case handler macroexpand should succeed");

    assert_eq!(
        normalized_value(compiled_expanded),
        normalized_value(source_expanded),
        "compiled condition-case handler macro expansion should match source"
    );
}

#[test]
fn test_compile_defmacro_then_defun_uses_compiled_macro() {
    crate::test_utils::init_test_tracing();
    let forms = parse_forms(
        r#"
(defmacro test-fc-gensym (x)
  (let ((tmp (make-symbol "tmp")))
    `(let ((,tmp ,x)) ,tmp)))

(defun test-fc-gensym-user (x)
  (test-fc-gensym x))
"#,
    )
    .unwrap();

    let mut compile_eval = minimal_compile_surface_eval();
    let compiled = compile_file_forms(&mut compile_eval, &forms).unwrap();
    assert_eq!(compiled.len(), 2);

    let mut runtime_eval = Context::new();
    for form in &compiled {
        let CompiledForm::Eval(value) = form else {
            panic!("expected Eval compiled form");
        };
        runtime_eval
            .eval_sub(*value)
            .expect("compiled top-level form should install");
    }

    let call = parse_forms("(test-fc-gensym-user 42)").unwrap();
    let result = runtime_eval
        .eval_expr(&call[0])
        .expect("compiled defun should use compiled macro correctly");
    assert_eq!(result, Value::fixnum(42));
}

#[test]
fn test_compile_file_forms_defmacro_compiles_after_later_helper_macro_exists() {
    crate::test_utils::init_test_tracing();
    let forms = parse_forms(
        r#"
(defmacro test-fc-outer (x)
  (test-fc-helper x))

(defmacro test-fc-helper (x)
  `(+ ,x 1))

(defun test-fc-outer-user (x)
  (test-fc-outer x))
"#,
    )
    .unwrap();

    let mut compile_eval = minimal_compile_surface_eval();
    let compiled = compile_file_forms(&mut compile_eval, &forms).unwrap();
    assert_eq!(compiled.len(), 3);
    assert!(matches!(&compiled[0], CompiledForm::Eval(_)));
    assert!(matches!(&compiled[1], CompiledForm::Eval(_)));
    assert!(matches!(&compiled[2], CompiledForm::Eval(_)));

    let mut runtime_eval = Context::new();
    for form in &compiled {
        let CompiledForm::Eval(value) = form else {
            panic!("expected Eval compiled form");
        };
        runtime_eval
            .eval_sub(*value)
            .expect("compiled top-level form should install");
    }

    let mut source_eval = minimal_compile_surface_eval();
    for form in &forms {
        source_eval
            .eval_expr(form)
            .expect("source top-level form should install");
    }
    let macroexpand = parse_forms("(macroexpand '(test-fc-outer 41))").unwrap();
    let source_expanded = source_eval
        .eval_expr(&macroexpand[0])
        .expect("source forward helper macro should macroexpand");
    let expanded = runtime_eval
        .eval_expr(&macroexpand[0])
        .unwrap_or_else(|flow| match flow {
            crate::emacs_core::error::EvalError::Signal { symbol, data, .. } => panic!(
                "compiled forward helper macro should macroexpand: {} {:?}",
                resolve_sym(symbol),
                data.iter()
                    .map(|value| normalized_value(*value))
                    .collect::<Vec<_>>()
            ),
            other => panic!("compiled forward helper macro should macroexpand: {other:?}"),
        });
    assert_eq!(
        normalized_value(expanded),
        normalized_value(source_expanded)
    );
}

#[test]
fn test_compile_toplevel_defmacro_with_env_supports_later_helper_macro() {
    crate::test_utils::init_test_tracing();
    let forms = parse_forms(
        r#"
(defmacro test-fc-outer (x)
  (test-fc-helper x))

(defmacro test-fc-helper (x)
  `(+ ,x 1))
"#,
    )
    .unwrap();

    let mut eval = minimal_compile_surface_eval();
    let mut out = Vec::new();
    let mut compiler_macro_env = Value::NIL;
    let mut compiler_function_overrides = Value::NIL;
    let mut deferred = Vec::new();

    compile_toplevel_file_form(
        &mut eval,
        &forms[0],
        &mut out,
        &mut compiler_macro_env,
        &mut compiler_function_overrides,
        &mut deferred,
    )
    .expect("first defmacro should source-install");
    compile_toplevel_file_form(
        &mut eval,
        &forms[1],
        &mut out,
        &mut compiler_macro_env,
        &mut compiler_function_overrides,
        &mut deferred,
    )
    .expect("helper defmacro should source-install");

    let macroexpand_fn = macroexpand_all_fn(&eval).expect("macroexpand-all should exist");
    let form_value = quote_to_value(&forms[0]);
    let expanded = eval.with_gc_scope_result(|ctx| {
        ctx.root(macroexpand_fn);
        ctx.root(compiler_macro_env);
        ctx.root(form_value);
        ctx.apply(macroexpand_fn, vec![form_value, compiler_macro_env])
    });
    let expanded_err = expanded.as_ref().err().map(|flow| match flow {
        Flow::Signal(sig) => (
            sig.symbol_name().to_string(),
            sig.data
                .iter()
                .map(|value| normalized_value(*value))
                .collect::<Vec<_>>(),
        ),
        other => (format!("{other:?}"), Vec::new()),
    });
    assert!(
        expanded.is_ok(),
        "top-level expansion should succeed with helper macro env: err={expanded_err:?} env={:?}",
        normalized_value(compiler_macro_env)
    );

    let compiled = compile_toplevel_defmacro_with_env(&mut eval, &forms[0], compiler_macro_env)
        .expect("defmacro compile should not signal");
    assert!(
        compiled.is_some(),
        "deferred defmacro should compile once helper exists: expanded={:?} env={:?}",
        expanded.ok().map(normalized_value),
        normalized_value(compiler_macro_env)
    );
}

#[test]
fn test_compile_file_forms_same_file_direct_helper_call_uses_compiler_function_env() {
    crate::test_utils::init_test_tracing();
    let forms = parse_forms(
        r#"
(defun test-fc-helper-direct (x)
  x)

(test-fc-helper-direct 42)
"#,
    )
    .unwrap();

    let mut eval = minimal_compile_surface_eval();
    let compiled =
        compile_file_forms(&mut eval, &forms).expect("same-file helper call should compile");

    assert_eq!(compiled.len(), 2);
}

#[test]
fn test_runtime_value_lowering_defmacro_then_defun_uses_compiled_macro() {
    crate::test_utils::init_test_tracing();
    let forms = parse_forms(
        r#"
(defmacro test-fc-gensym (x)
  (let ((tmp (make-symbol "tmp")))
    `(let ((,tmp ,x)) ,tmp)))

(defun test-fc-gensym-user (x)
  (test-fc-gensym x))
"#,
    )
    .unwrap();

    let mut direct_compile_eval = minimal_compile_surface_eval();
    let direct_compiled = compile_file_forms(&mut direct_compile_eval, &forms).unwrap();
    let direct_values: Vec<Value> = direct_compiled
        .iter()
        .map(|form| match form {
            CompiledForm::Eval(value) => *value,
            other => panic!("expected Eval compiled form, got {other:?}"),
        })
        .collect();

    let mut lowering_eval = minimal_compile_surface_eval();
    let mut compiler_macro_env = Value::NIL;
    let mut lowered = Vec::new();
    for form in &forms {
        let compiled = lowering_eval.with_gc_scope(|ctx| {
            compiler_macro_env = ctx.root(compiler_macro_env);
            let original = ctx.root(quote_to_value(form));
            let expanded =
                expand_compiler_toplevel_expr_with_env(ctx, original, compiler_macro_env)
                    .map(|expr| quote_to_value(&expr))
                    .unwrap_or(original);
            let expanded = ctx.root(expanded);
            let compiled = lower_runtime_cached_toplevel_form_with_env(
                ctx,
                original,
                expanded,
                compiler_macro_env,
            )
            .expect("runtime lowering should compile");
            maybe_extend_compiler_macro_env_from_lowered(&mut compiler_macro_env, compiled);
            compiled
        });
        let compiled = lowering_eval.root(compiled);
        compiler_macro_env = lowering_eval.root(compiler_macro_env);
        lowered.push(compiled);
    }

    let mut direct_surface = minimal_compile_surface_eval();
    let mut direct_out = Vec::new();
    let mut direct_env = Value::NIL;
    let mut direct_function_overrides = Value::NIL;
    let mut deferred = Vec::new();
    compile_toplevel_file_form(
        &mut direct_surface,
        &forms[0],
        &mut direct_out,
        &mut direct_env,
        &mut direct_function_overrides,
        &mut deferred,
    )
    .expect("direct compiler surface should process macro form");
    direct_env = direct_surface.root(direct_env);
    let direct_expanded = expand_compiler_toplevel_expr_with_env(
        &mut direct_surface,
        quote_to_value(&forms[1]),
        direct_env,
    )
    .expect("direct compiler surface should expand defun");
    let runtime_expanded = expand_compiler_toplevel_expr_with_env(
        &mut lowering_eval,
        quote_to_value(&forms[1]),
        compiler_macro_env,
    )
    .expect("runtime lowering surface should expand defun");
    assert_eq!(
        normalized_expr(&runtime_expanded),
        normalized_expr(&direct_expanded),
        "expanded defun form"
    );

    assert_eq!(lowered.len(), direct_values.len(), "compiled form count");
    assert_same_compiled_defalias("defmacro", lowered[0], direct_values[0]);
    assert_same_compiled_defalias("defun", lowered[1], direct_values[1]);

    let mut runtime_eval = Context::new();
    for (index, value) in lowered.into_iter().enumerate() {
        runtime_eval.eval_sub(value).unwrap_or_else(|err| {
            panic!(
                "runtime-lowered compiled form {index} should install: {:?}",
                err,
            )
        });
    }

    let call = parse_forms("(test-fc-gensym-user 42)").unwrap();
    let result = runtime_eval
        .eval_expr(&call[0])
        .expect("runtime-lowered compiled defun should use compiled macro correctly");
    assert_eq!(result, Value::fixnum(42));
}

#[test]
fn test_runtime_value_lowering_macroexp_accumulate_shape_stays_callable() {
    crate::test_utils::init_test_tracing();
    let forms = parse_forms(
        r#"
(defun macroexp--expand-all (form)
  (list 'expanded form))

(defmacro macroexp--accumulate (var+list &rest body)
  (let ((var (car var+list))
        (list (cadr var+list))
        (shared (make-symbol "shared"))
        (unshared (make-symbol "unshared"))
        (tail (make-symbol "tail"))
        (new-el (make-symbol "new-el")))
    `(let* ((,shared ,list)
            (,unshared nil)
            (,tail ,shared)
            ,var ,new-el)
       (while (consp ,tail)
         (setq ,var (car ,tail)
               ,new-el (progn ,@body))
         (unless (eq ,var ,new-el)
           (while (not (eq ,shared ,tail))
             (push (pop ,shared) ,unshared))
           (setq ,shared (cdr ,shared))
           (push ,new-el ,unshared))
         (setq ,tail (cdr ,tail)))
       (nconc (nreverse ,unshared) ,shared))))

(defun macroexp--all-forms (forms &optional skip)
  (macroexp--accumulate (form forms)
    (if (or (null skip) (zerop skip))
        (macroexp--expand-all form)
      (setq skip (1- skip))
      form)))
"#,
    )
    .unwrap();
    let macroexpand_probe = parse_forms(
        r#"(macroexpand
                 '(macroexp--accumulate (form forms)
                    (if (or (null skip) (zerop skip))
                        (macroexp--expand-all form)
                      (setq skip (1- skip))
                      form)))"#,
    )
    .unwrap();

    let mut direct_compile_eval = minimal_compile_surface_eval();
    let direct_compiled = compile_file_forms(&mut direct_compile_eval, &forms).unwrap();
    let direct_values: Vec<Value> = direct_compiled
        .iter()
        .map(|form| match form {
            CompiledForm::Eval(value) => *value,
            other => panic!("expected Eval compiled form, got {other:?}"),
        })
        .collect();

    let mut lowering_eval = minimal_compile_surface_eval();
    let mut compiler_macro_env = Value::NIL;
    let mut lowered = Vec::new();
    for form in &forms {
        let compiled = lowering_eval.with_gc_scope(|ctx| {
            compiler_macro_env = ctx.root(compiler_macro_env);
            let original = ctx.root(quote_to_value(form));
            let expanded =
                expand_compiler_toplevel_expr_with_env(ctx, original, compiler_macro_env)
                    .map(|expr| quote_to_value(&expr))
                    .unwrap_or(original);
            let expanded = ctx.root(expanded);
            let compiled = lower_runtime_cached_toplevel_form_with_env(
                ctx,
                original,
                expanded,
                compiler_macro_env,
            )
            .expect("runtime lowering should compile");
            maybe_extend_compiler_macro_env_from_lowered(&mut compiler_macro_env, compiled);
            compiled
        });
        let compiled = lowering_eval.root(compiled);
        compiler_macro_env = lowering_eval.root(compiler_macro_env);
        lowered.push(compiled);
    }
    assert_eq!(lowered.len(), direct_values.len(), "compiled form count");
    for (index, (lowered_value, direct_value)) in lowered
        .iter()
        .copied()
        .zip(direct_values.iter().copied())
        .enumerate()
    {
        assert_same_compiled_defalias(
            &format!("macroexp-accumulate-form-{index}"),
            lowered_value,
            direct_value,
        );
    }

    let mut source_eval = minimal_compile_surface_eval();
    for form in &forms {
        source_eval
            .eval_expr(form)
            .expect("source macroexp forms should install");
    }
    let source_expansion = source_eval
        .eval_expr(&macroexpand_probe[0])
        .expect("source macroexpand should succeed");
    let source_expansion =
        crate::emacs_core::print::print_value_with_buffers(&source_expansion, &source_eval.buffers);

    let mut runtime_eval = minimal_compile_surface_eval();
    for value in lowered {
        runtime_eval
            .eval_sub(value)
            .expect("runtime-lowered macroexp forms should install");
    }
    let runtime_expansion = runtime_eval
        .eval_expr(&macroexpand_probe[0])
        .expect("compiled macroexpand should succeed");
    let runtime_expansion = crate::emacs_core::print::print_value_with_buffers(
        &runtime_expansion,
        &runtime_eval.buffers,
    );
    assert_eq!(
        runtime_expansion, source_expansion,
        "runtime-lowered macroexp expansion should match source-installed macro"
    );

    let call = parse_forms(
        r#"(list
                 (macroexp--all-forms '(a b c))
                 (macroexp--all-forms '(a b c) 1)
                 (macroexp--all-forms '(a b c) 2))"#,
    )
    .unwrap();
    let result = runtime_eval
        .eval_expr(&call[0])
        .expect("runtime-lowered macroexp accumulator should stay callable");
    assert_eq!(
        result,
        Value::list(vec![
            Value::list(vec![
                Value::list(vec![Value::symbol("expanded"), Value::symbol("a")]),
                Value::list(vec![Value::symbol("expanded"), Value::symbol("b")]),
                Value::list(vec![Value::symbol("expanded"), Value::symbol("c")]),
            ]),
            Value::list(vec![
                Value::symbol("a"),
                Value::list(vec![Value::symbol("expanded"), Value::symbol("b")]),
                Value::list(vec![Value::symbol("expanded"), Value::symbol("c")]),
            ]),
            Value::list(vec![
                Value::symbol("a"),
                Value::symbol("b"),
                Value::list(vec![Value::symbol("expanded"), Value::symbol("c")]),
            ]),
        ])
    );
}

#[test]
fn test_compile_file_forms_macroexp_accumulate_shape_stays_callable() {
    crate::test_utils::init_test_tracing();
    let forms = parse_forms(
        r#"
(defun macroexp--expand-all (form)
  (list 'expanded form))

(defmacro macroexp--accumulate (var+list &rest body)
  (let ((var (car var+list))
        (list (cadr var+list))
        (shared (make-symbol "shared"))
        (unshared (make-symbol "unshared"))
        (tail (make-symbol "tail"))
        (new-el (make-symbol "new-el")))
    `(let* ((,shared ,list)
            (,unshared nil)
            (,tail ,shared)
            ,var ,new-el)
       (while (consp ,tail)
         (setq ,var (car ,tail)
               ,new-el (progn ,@body))
         (unless (eq ,var ,new-el)
           (while (not (eq ,shared ,tail))
             (push (pop ,shared) ,unshared))
           (setq ,shared (cdr ,shared))
           (push ,new-el ,unshared))
         (setq ,tail (cdr ,tail)))
       (nconc (nreverse ,unshared) ,shared))))

(defun macroexp--all-forms (forms &optional skip)
  (macroexp--accumulate (form forms)
    (if (or (null skip) (zerop skip))
        (macroexp--expand-all form)
      (setq skip (1- skip))
      form)))
"#,
    )
    .unwrap();
    let macroexpand_probe = parse_forms(
        r#"(macroexpand
                 '(macroexp--accumulate (form forms)
                    (if (or (null skip) (zerop skip))
                        (macroexp--expand-all form)
                      (setq skip (1- skip))
                      form)))"#,
    )
    .unwrap();

    let mut compile_eval = minimal_compile_surface_eval();
    let compiled = compile_file_forms(&mut compile_eval, &forms).unwrap();

    let mut source_eval = minimal_compile_surface_eval();
    for form in &forms {
        source_eval
            .eval_expr(form)
            .expect("source macroexp forms should install");
    }
    let source_expansion = source_eval
        .eval_expr(&macroexpand_probe[0])
        .expect("source macroexpand should succeed");
    let source_expansion =
        crate::emacs_core::print::print_value_with_buffers(&source_expansion, &source_eval.buffers);

    let mut runtime_eval = minimal_compile_surface_eval();
    for form in &compiled {
        let CompiledForm::Eval(value) = form else {
            panic!("expected Eval compiled form");
        };
        runtime_eval
            .eval_sub(*value)
            .expect("compiled macroexp forms should install");
    }
    let runtime_expansion = runtime_eval
        .eval_expr(&macroexpand_probe[0])
        .expect("compiled macroexpand should succeed");
    let runtime_expansion = crate::emacs_core::print::print_value_with_buffers(
        &runtime_expansion,
        &runtime_eval.buffers,
    );
    assert_eq!(
        runtime_expansion, source_expansion,
        "compiled macroexp expansion should match source-installed macro"
    );

    let call = parse_forms(
        r#"(list
                 (macroexp--all-forms '(a b c))
                 (macroexp--all-forms '(a b c) 1)
                 (macroexp--all-forms '(a b c) 2))"#,
    )
    .unwrap();
    let result = runtime_eval
        .eval_expr(&call[0])
        .expect("compiled macroexp accumulator should stay callable");
    assert_eq!(
        result,
        Value::list(vec![
            Value::list(vec![
                Value::list(vec![Value::symbol("expanded"), Value::symbol("a")]),
                Value::list(vec![Value::symbol("expanded"), Value::symbol("b")]),
                Value::list(vec![Value::symbol("expanded"), Value::symbol("c")]),
            ]),
            Value::list(vec![
                Value::symbol("a"),
                Value::list(vec![Value::symbol("expanded"), Value::symbol("b")]),
                Value::list(vec![Value::symbol("expanded"), Value::symbol("c")]),
            ]),
            Value::list(vec![
                Value::symbol("a"),
                Value::symbol("b"),
                Value::list(vec![Value::symbol("expanded"), Value::symbol("c")]),
            ]),
        ])
    );
}

#[test]
fn test_compile_body_exprs_expands_macroexp_accumulate_backquote_shape() {
    crate::test_utils::init_test_tracing();
    let forms = parse_forms(
        r#"
(defmacro macroexp--accumulate (var+list &rest body)
  (let ((var (car var+list))
        (list (cadr var+list))
        (shared (make-symbol "shared"))
        (unshared (make-symbol "unshared"))
        (tail (make-symbol "tail"))
        (new-el (make-symbol "new-el")))
    `(let* ((,shared ,list)
            (,unshared nil)
            (,tail ,shared)
            ,var ,new-el)
       (while (consp ,tail)
         (setq ,var (car ,tail)
               ,new-el (progn ,@body))
         (unless (eq ,var ,new-el)
           (while (not (eq ,shared ,tail))
             (push (pop ,shared) ,unshared))
           (setq ,shared (cdr ,shared))
           (push ,new-el ,unshared))
         (setq ,tail (cdr ,tail)))
       (nconc (nreverse ,unshared) ,shared))))
"#,
    )
    .unwrap();
    let Expr::List(items) = &forms[0] else {
        panic!("expected defmacro list");
    };
    let body_values: Vec<Value> = items[3..].iter().map(quote_to_value).collect();
    let mut eval = minimal_compile_surface_eval();
    let compiled_body = compile_body_exprs(&mut eval, &body_values);
    assert!(
        compiled_body
            .iter()
            .all(|expr| !expr_contains_symbol_named(expr, "`")),
        "compiled body should not contain raw backquote symbol"
    );
    assert!(
        compiled_body
            .iter()
            .all(|expr| !expr_contains_symbol_named(expr, "\\,")),
        "compiled body should not contain raw comma symbol"
    );
    assert!(
        compiled_body
            .iter()
            .all(|expr| !expr_contains_symbol_named(expr, "\\,@")),
        "compiled body should not contain raw comma-at symbol"
    );
}

#[test]
fn test_compile_body_exprs_expands_cl_load_time_value_backquote_shape() {
    crate::test_utils::init_test_tracing();
    let form = real_cl_macs_defmacro_form("cl-load-time-value");
    let Expr::List(items) = &form else {
        panic!("expected cl-load-time-value defmacro list");
    };
    let body_values: Vec<Value> = items[3..].iter().map(quote_to_value).collect();
    let mut eval = minimal_compile_surface_eval();
    let compiled_body = compile_body_exprs(&mut eval, &body_values);
    let rendered = compiled_body
        .iter()
        .map(print_expr)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        compiled_body
            .iter()
            .all(|expr| !expr_contains_symbol_named(expr, "`")),
        "compiled cl-load-time-value body should not contain raw backquote symbol: {rendered}"
    );
    assert!(
        compiled_body
            .iter()
            .all(|expr| !expr_contains_symbol_named(expr, "\\,")),
        "compiled cl-load-time-value body should not contain raw comma symbol: {rendered}"
    );
    assert!(
        compiled_body
            .iter()
            .all(|expr| !expr_contains_symbol_named(expr, "\\,@")),
        "compiled cl-load-time-value body should not contain raw comma-at symbol: {rendered}"
    );
}

#[test]
fn test_compile_defmacro_direct_real_cl_load_time_value_emits_compiled_macro() {
    crate::test_utils::init_test_tracing();
    let form = real_cl_macs_defmacro_form("cl-load-time-value");
    let runtime_value = quote_to_value(&form);
    let items = list_to_vec(&runtime_value).expect("defmacro value should be a list");
    let mut eval = minimal_compile_surface_eval();
    let compiled = compile_toplevel_defmacro_direct_value(&mut eval, &items)
        .expect("real cl-load-time-value should compile");
    let compiled_items = list_to_vec(&compiled).expect("compiled form should be a list");
    assert_eq!(compiled_items[0].as_symbol_name(), Some("defalias"));
    let target = list_to_vec(&compiled_items[2]).expect("macro target should be a list");
    assert_eq!(target[0].as_symbol_name(), Some("cons"));
    assert!(is_quoted_symbol(target[1], "macro"));
    assert!(target[2].get_bytecode_data().is_some());
}

#[test]
fn test_compile_defmacro_direct_real_cl_load_time_value_declines_when_expand_fails() {
    crate::test_utils::init_test_tracing();
    let form = real_cl_macs_defmacro_form("cl-load-time-value");
    let runtime_value = quote_to_value(&form);
    let items = list_to_vec(&runtime_value).expect("defmacro value should be a list");
    let mut eval = minimal_compile_surface_eval();
    let compiled = compile_toplevel_defmacro_direct_value_with_env(
        &mut eval,
        &items,
        Value::symbol("bogus-macroexpand-env"),
    );
    assert!(
        compiled.is_none(),
        "defmacro compilation should refuse raw-body fallback when macroexpand fails"
    );
}

#[test]
fn test_compile_defmacro_direct_macroexp_accumulate_macroexpand_matches_source() {
    crate::test_utils::init_test_tracing();
    let forms = parse_forms(
        r#"
(defmacro macroexp--accumulate (var+list &rest body)
  (let ((var (car var+list))
        (list (cadr var+list))
        (shared (make-symbol "shared"))
        (unshared (make-symbol "unshared"))
        (tail (make-symbol "tail"))
        (new-el (make-symbol "new-el")))
    `(let* ((,shared ,list)
            (,unshared nil)
            (,tail ,shared)
            ,var ,new-el)
       (while (consp ,tail)
         (setq ,var (car ,tail)
               ,new-el (progn ,@body))
         (unless (eq ,var ,new-el)
           (while (not (eq ,shared ,tail))
             (push (pop ,shared) ,unshared))
           (setq ,shared (cdr ,shared))
           (push ,new-el ,unshared))
         (setq ,tail (cdr ,tail)))
       (nconc (nreverse ,unshared) ,shared))))
"#,
    )
    .unwrap();
    let macroexpand_probe = parse_forms(
        r#"(macroexpand
                 '(macroexp--accumulate (form forms)
                    (if (or (null skip) (zerop skip))
                        (macroexp--expand-all form)
                      (setq skip (1- skip))
                      form)))"#,
    )
    .unwrap();

    let mut source_eval = minimal_compile_surface_eval();
    source_eval
        .eval_expr(&forms[0])
        .expect("source macro should install");
    let source_expansion = source_eval
        .eval_expr(&macroexpand_probe[0])
        .expect("source macroexpand should succeed");
    let source_expansion =
        crate::emacs_core::print::print_value_with_buffers(&source_expansion, &source_eval.buffers);

    let Expr::List(items) = &forms[0] else {
        panic!("expected defmacro list");
    };
    let mut compile_eval = minimal_compile_surface_eval();
    let compiled_value = compile_toplevel_defmacro_direct(&mut compile_eval, items)
        .expect("direct compiled defmacro should compile");

    let mut runtime_eval = minimal_compile_surface_eval();
    runtime_eval
        .eval_sub(compiled_value)
        .expect("compiled defmacro should install");
    let runtime_expansion = runtime_eval
        .eval_expr(&macroexpand_probe[0])
        .expect("compiled macroexpand should succeed");
    let runtime_expansion = crate::emacs_core::print::print_value_with_buffers(
        &runtime_expansion,
        &runtime_eval.buffers,
    );
    assert_eq!(
        runtime_expansion, source_expansion,
        "direct compiled macroexp expansion should match source-installed macro"
    );
}

#[test]
fn test_compile_file_forms_defmacro_only_macroexp_accumulate_macroexpand_matches_source() {
    crate::test_utils::init_test_tracing();
    let forms = parse_forms(
        r#"
(defmacro macroexp--accumulate (var+list &rest body)
  (let ((var (car var+list))
        (list (cadr var+list))
        (shared (make-symbol "shared"))
        (unshared (make-symbol "unshared"))
        (tail (make-symbol "tail"))
        (new-el (make-symbol "new-el")))
    `(let* ((,shared ,list)
            (,unshared nil)
            (,tail ,shared)
            ,var ,new-el)
       (while (consp ,tail)
         (setq ,var (car ,tail)
               ,new-el (progn ,@body))
         (unless (eq ,var ,new-el)
           (while (not (eq ,shared ,tail))
             (push (pop ,shared) ,unshared))
           (setq ,shared (cdr ,shared))
           (push ,new-el ,unshared))
         (setq ,tail (cdr ,tail)))
       (nconc (nreverse ,unshared) ,shared))))
"#,
    )
    .unwrap();
    let macroexpand_probe = parse_forms(
        r#"(macroexpand
                 '(macroexp--accumulate (form forms)
                    (if (or (null skip) (zerop skip))
                        (macroexp--expand-all form)
                      (setq skip (1- skip))
                      form)))"#,
    )
    .unwrap();

    let mut source_eval = minimal_compile_surface_eval();
    source_eval
        .eval_expr(&forms[0])
        .expect("source macro should install");
    let source_expansion = source_eval
        .eval_expr(&macroexpand_probe[0])
        .expect("source macroexpand should succeed");
    let source_expansion =
        crate::emacs_core::print::print_value_with_buffers(&source_expansion, &source_eval.buffers);

    let mut compile_eval = minimal_compile_surface_eval();
    let compiled = compile_file_forms(&mut compile_eval, &forms).unwrap();
    assert_eq!(compiled.len(), 1);

    let mut runtime_eval = minimal_compile_surface_eval();
    let CompiledForm::Eval(value) = &compiled[0] else {
        panic!("expected Eval compiled form");
    };
    runtime_eval
        .eval_sub(*value)
        .expect("compiled defmacro should install");
    let runtime_expansion = runtime_eval
        .eval_expr(&macroexpand_probe[0])
        .expect("compiled macroexpand should succeed");
    let runtime_expansion = crate::emacs_core::print::print_value_with_buffers(
        &runtime_expansion,
        &runtime_eval.buffers,
    );
    assert_eq!(
        runtime_expansion, source_expansion,
        "compiled-file defmacro expansion should match source-installed macro"
    );
}

#[test]
fn test_compile_file_forms_expand_all_and_macroexp_accumulate_macroexpand_matches_source() {
    crate::test_utils::init_test_tracing();
    let forms = parse_forms(
        r#"
(defun macroexp--expand-all (form)
  (list 'expanded form))

(defmacro macroexp--accumulate (var+list &rest body)
  (let ((var (car var+list))
        (list (cadr var+list))
        (shared (make-symbol "shared"))
        (unshared (make-symbol "unshared"))
        (tail (make-symbol "tail"))
        (new-el (make-symbol "new-el")))
    `(let* ((,shared ,list)
            (,unshared nil)
            (,tail ,shared)
            ,var ,new-el)
       (while (consp ,tail)
         (setq ,var (car ,tail)
               ,new-el (progn ,@body))
         (unless (eq ,var ,new-el)
           (while (not (eq ,shared ,tail))
             (push (pop ,shared) ,unshared))
           (setq ,shared (cdr ,shared))
           (push ,new-el ,unshared))
         (setq ,tail (cdr ,tail)))
       (nconc (nreverse ,unshared) ,shared))))
"#,
    )
    .unwrap();
    let macroexpand_probe = parse_forms(
        r#"(macroexpand
                 '(macroexp--accumulate (form forms)
                    (if (or (null skip) (zerop skip))
                        (macroexp--expand-all form)
                      (setq skip (1- skip))
                      form)))"#,
    )
    .unwrap();

    let mut source_eval = minimal_compile_surface_eval();
    for form in &forms {
        source_eval
            .eval_expr(form)
            .expect("source forms should install");
    }
    let source_expansion = source_eval
        .eval_expr(&macroexpand_probe[0])
        .expect("source macroexpand should succeed");
    let source_expansion =
        crate::emacs_core::print::print_value_with_buffers(&source_expansion, &source_eval.buffers);

    let mut compile_eval = minimal_compile_surface_eval();
    let compiled = compile_file_forms(&mut compile_eval, &forms).unwrap();
    assert_eq!(compiled.len(), 2);

    let mut runtime_eval = minimal_compile_surface_eval();
    for form in &compiled {
        let CompiledForm::Eval(value) = form else {
            panic!("expected Eval compiled form");
        };
        runtime_eval
            .eval_sub(*value)
            .expect("compiled forms should install");
    }
    let runtime_expansion = runtime_eval
        .eval_expr(&macroexpand_probe[0])
        .expect("compiled macroexpand should succeed");
    let runtime_expansion = crate::emacs_core::print::print_value_with_buffers(
        &runtime_expansion,
        &runtime_eval.buffers,
    );
    assert_eq!(
        runtime_expansion, source_expansion,
        "compiled-file prefix expansion should match source-installed macro"
    );
}

#[test]
fn test_compile_file_forms_prefix_preserves_macroexp_accumulate_compiled_artifact() {
    crate::test_utils::init_test_tracing();
    let direct_forms = parse_forms(
        r#"
(defmacro macroexp--accumulate (var+list &rest body)
  (let ((var (car var+list))
        (list (cadr var+list))
        (shared (make-symbol "shared"))
        (unshared (make-symbol "unshared"))
        (tail (make-symbol "tail"))
        (new-el (make-symbol "new-el")))
    `(let* ((,shared ,list)
            (,unshared nil)
            (,tail ,shared)
            ,var ,new-el)
       (while (consp ,tail)
         (setq ,var (car ,tail)
               ,new-el (progn ,@body))
         (unless (eq ,var ,new-el)
           (while (not (eq ,shared ,tail))
             (push (pop ,shared) ,unshared))
           (setq ,shared (cdr ,shared))
           (push ,new-el ,unshared))
         (setq ,tail (cdr ,tail)))
       (nconc (nreverse ,unshared) ,shared))))
"#,
    )
    .unwrap();
    let prefixed_forms = parse_forms(
        r#"
(defun macroexp--expand-all (form)
  (list 'expanded form))

(defmacro macroexp--accumulate (var+list &rest body)
  (let ((var (car var+list))
        (list (cadr var+list))
        (shared (make-symbol "shared"))
        (unshared (make-symbol "unshared"))
        (tail (make-symbol "tail"))
        (new-el (make-symbol "new-el")))
    `(let* ((,shared ,list)
            (,unshared nil)
            (,tail ,shared)
            ,var ,new-el)
       (while (consp ,tail)
         (setq ,var (car ,tail)
               ,new-el (progn ,@body))
         (unless (eq ,var ,new-el)
           (while (not (eq ,shared ,tail))
             (push (pop ,shared) ,unshared))
           (setq ,shared (cdr ,shared))
           (push ,new-el ,unshared))
         (setq ,tail (cdr ,tail)))
       (nconc (nreverse ,unshared) ,shared))))
"#,
    )
    .unwrap();

    let Expr::List(direct_items) = &direct_forms[0] else {
        panic!("expected direct defmacro list");
    };
    let mut direct_eval = minimal_compile_surface_eval();
    let direct_value = compile_toplevel_defmacro_direct(&mut direct_eval, direct_items)
        .expect("direct compiled defmacro should compile");

    let mut single_eval = minimal_compile_surface_eval();
    let single_compiled = compile_file_forms(&mut single_eval, &direct_forms).unwrap();
    let CompiledForm::Eval(single_value) = &single_compiled[0] else {
        panic!("expected Eval compiled form");
    };

    let mut prefix_eval = minimal_compile_surface_eval();
    let prefixed_compiled = compile_file_forms(&mut prefix_eval, &prefixed_forms).unwrap();
    let CompiledForm::Eval(prefixed_value) = &prefixed_compiled[1] else {
        panic!("expected Eval compiled form for prefixed defmacro");
    };

    let render_eval = minimal_compile_surface_eval();
    let direct_render =
        crate::emacs_core::print::print_value_with_buffers(&direct_value, &render_eval.buffers);
    let single_render =
        crate::emacs_core::print::print_value_with_buffers(single_value, &render_eval.buffers);
    let prefixed_render =
        crate::emacs_core::print::print_value_with_buffers(prefixed_value, &render_eval.buffers);

    assert_eq!(
        single_render, direct_render,
        "single-form compile_file_forms should preserve the direct compiled macro artifact"
    );
    assert_eq!(
        prefixed_render, direct_render,
        "prefixed compile_file_forms should preserve the direct compiled macro artifact"
    );
}

#[test]
fn test_compile_defun_with_macroexp_accumulate_expanded_body_stays_callable() {
    crate::test_utils::init_test_tracing();
    let macro_forms = parse_forms(
        r#"
(defmacro macroexp--accumulate (var+list &rest body)
  (let ((var (car var+list))
        (list (cadr var+list))
        (shared (make-symbol "shared"))
        (unshared (make-symbol "unshared"))
        (tail (make-symbol "tail"))
        (new-el (make-symbol "new-el")))
    (list 'let*
          (list (list shared list)
                (cons unshared '(nil))
                (list tail shared)
                var
                new-el)
          (list 'while
                (list 'consp tail)
                (list 'setq var (list 'car tail)
                      new-el (cons 'progn body))
                (list 'unless
                      (list 'eq var new-el)
                      (list 'while
                            (list 'not (list 'eq shared tail))
                            (list 'push (list 'pop shared) unshared))
                      (list 'setq shared (list 'cdr shared))
                      (list 'push new-el unshared))
                (list 'setq tail (list 'cdr tail)))
          (list 'nconc (list 'nreverse unshared) shared))))"#,
    )
    .unwrap();
    let Expr::List(items) = &macro_forms[0] else {
        panic!("expected defmacro list");
    };
    let mut eval = minimal_compile_surface_eval();
    let body_values: Vec<Value> = items[3..].iter().map(quote_to_value).collect();
    let compiled_body = compile_body_exprs(&mut eval, &body_values);
    let mut defun_items = vec![
        Value::symbol("defun"),
        Value::symbol("test-fc-macroexp-accumulate-expanded"),
        Value::list(vec![
            Value::symbol("var+list"),
            Value::symbol("&rest"),
            Value::symbol("body"),
        ]),
    ];
    defun_items.extend(compiled_body.iter().map(quote_to_value));
    let compiled_value = compile_toplevel_defun_direct_value(&mut eval, &defun_items)
        .expect("expanded-body defun should compile");
    eval.eval_sub(compiled_value)
        .expect("compiled defun should install");

    let call = parse_forms(
        r#"(test-fc-macroexp-accumulate-expanded
                 '(form forms)
                 '(macroexp--expand-all form))"#,
    )
    .unwrap();
    let result = eval
        .eval_expr(&call[0])
        .expect("compiled expanded-body defun should stay callable");
    let rendered = crate::emacs_core::print::print_value_with_buffers(&result, &eval.buffers);
    assert!(
        rendered.contains("let*"),
        "expanded-body defun should return template data, got {rendered}"
    );
}

#[test]
fn test_compile_defmacro_direct_value_matches_expr_path_for_defface_shape() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let forms = parse_forms(
        r#"
(defmacro vm--defface-shape (face spec doc &rest args)
  "Declare FACE as a customizable face that defaults to SPEC."
  (declare (doc-string 3) (indent defun))
  (nconc (list 'custom-declare-face (list 'quote face) spec doc) args))
"#,
    )
    .unwrap();

    let Expr::List(expr_items) = &forms[0] else {
        panic!("expected defmacro list");
    };
    let expr_compiled =
        compile_toplevel_defmacro_direct(&mut eval, expr_items).expect("expr path should compile");
    let expr_compiled = eval.root(expr_compiled);

    let runtime_value = quote_to_value(&forms[0]);
    let runtime_items = list_to_vec(&runtime_value).expect("runtime form should be a list");
    let value_compiled = compile_toplevel_defmacro_direct_value(&mut eval, &runtime_items)
        .expect("runtime-value path should compile");
    let value_compiled = eval.root(value_compiled);

    let expr_items = list_to_vec(&expr_compiled).expect("expr compiled form should be a list");
    let value_items = list_to_vec(&value_compiled).expect("value compiled form should be a list");
    assert_eq!(expr_items[0], value_items[0]);
    assert_eq!(expr_items[1], value_items[1]);
    assert_eq!(expr_items[3], value_items[3]);

    let expr_target = list_to_vec(&expr_items[2]).expect("expr target should be a list");
    let value_target = list_to_vec(&value_items[2]).expect("value target should be a list");
    assert_eq!(expr_target[0], value_target[0]);
    assert_eq!(expr_target[1], value_target[1]);

    let expr_bc = expr_target[2].get_bytecode_data().expect("expr bytecode");
    let value_bc = value_target[2].get_bytecode_data().expect("value bytecode");
    assert_eq!(expr_bc.ops, value_bc.ops);
    assert_eq!(expr_bc.constants, value_bc.constants);
    assert_eq!(expr_bc.params, value_bc.params);
    assert_eq!(expr_bc.lexical, value_bc.lexical);
    assert_eq!(expr_bc.docstring, value_bc.docstring);
    assert_eq!(expr_bc.doc_form, value_bc.doc_form);
    assert_eq!(expr_bc.interactive, value_bc.interactive);
}

#[test]
fn test_compile_defmacro_direct_value_matches_expr_path_for_real_custom_defface() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let defface_form = real_custom_defmacro_form("defface");

    let Expr::List(expr_items) = &defface_form else {
        panic!("expected defface defmacro list");
    };
    let expr_compiled =
        compile_toplevel_defmacro_direct(&mut eval, expr_items).expect("expr path should compile");
    let expr_compiled = eval.root(expr_compiled);

    let runtime_value = quote_to_value(&defface_form);
    let runtime_items = list_to_vec(&runtime_value).expect("runtime form should be a list");
    let value_compiled = compile_toplevel_defmacro_direct_value(&mut eval, &runtime_items)
        .expect("runtime-value path should compile");
    let value_compiled = eval.root(value_compiled);

    let expr_items = list_to_vec(&expr_compiled).expect("expr compiled form should be a list");
    let value_items = list_to_vec(&value_compiled).expect("value compiled form should be a list");
    assert_eq!(expr_items[0], value_items[0]);
    assert_eq!(expr_items[1], value_items[1]);
    assert_eq!(expr_items[3], value_items[3]);

    let expr_target = list_to_vec(&expr_items[2]).expect("expr target should be a list");
    let value_target = list_to_vec(&value_items[2]).expect("value target should be a list");
    assert_eq!(expr_target[0], value_target[0]);
    assert_eq!(expr_target[1], value_target[1]);

    let expr_bc = expr_target[2].get_bytecode_data().expect("expr bytecode");
    let value_bc = value_target[2].get_bytecode_data().expect("value bytecode");
    assert_eq!(expr_bc.ops, value_bc.ops);
    assert_eq!(expr_bc.constants, value_bc.constants);
    assert_eq!(expr_bc.params, value_bc.params);
    assert_eq!(expr_bc.lexical, value_bc.lexical);
    assert_eq!(expr_bc.docstring, value_bc.docstring);
    assert_eq!(expr_bc.doc_form, value_bc.doc_form);
    assert_eq!(expr_bc.interactive, value_bc.interactive);
}

#[test]
fn test_compile_defmacro_direct_value_matches_expr_path_for_macroexp_accumulate() {
    crate::test_utils::init_test_tracing();
    let mut eval = minimal_compile_surface_eval();
    let defmacro_form = macroexp_accumulate_defmacro_form();

    let Expr::List(expr_items) = &defmacro_form else {
        panic!("expected defmacro list");
    };
    let expr_compiled =
        compile_toplevel_defmacro_direct(&mut eval, expr_items).expect("expr path should compile");
    let expr_compiled = eval.root(expr_compiled);

    let runtime_value = quote_to_value(&defmacro_form);
    let runtime_items = list_to_vec(&runtime_value).expect("runtime form should be a list");
    let value_compiled = compile_toplevel_defmacro_direct_value(&mut eval, &runtime_items)
        .expect("runtime-value path should compile");
    let value_compiled = eval.root(value_compiled);

    assert_same_compiled_defalias("macroexp--accumulate", expr_compiled, value_compiled);
}

#[test]
fn test_compile_defmacro_direct_value_matches_expr_path_for_inline_quote() {
    crate::test_utils::init_test_tracing();
    let mut eval = minimal_compile_surface_eval();
    let defmacro_form = real_inline_defmacro_form("inline-quote");

    let Expr::List(expr_items) = &defmacro_form else {
        panic!("expected defmacro list");
    };
    let expr_compiled =
        compile_toplevel_defmacro_direct(&mut eval, expr_items).expect("expr path should compile");
    let expr_compiled = eval.root(expr_compiled);

    let runtime_value = quote_to_value(&defmacro_form);
    let runtime_items = list_to_vec(&runtime_value).expect("runtime form should be a list");
    let value_compiled = compile_toplevel_defmacro_direct_value(&mut eval, &runtime_items)
        .expect("runtime-value path should compile");
    let value_compiled = eval.root(value_compiled);

    assert_same_compiled_defalias("inline-quote", expr_compiled, value_compiled);
}

#[test]
fn test_compile_defun_preserves_doc_named_argument_order() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let forms = parse_forms(
        r#"(defun test-fc-doc-arg-order (face spec doc &rest args)
                 "Function docstring."
                 (list face spec doc args))"#,
    )
    .unwrap();
    let compiled = compile_file_forms(&mut eval, &forms).unwrap();
    assert_eq!(compiled.len(), 1);

    let CompiledForm::Eval(value) = &compiled[0] else {
        panic!("expected Eval form");
    };
    eval.eval_sub(*value)
        .expect("compiled defun defalias should install");

    let call = parse_forms(
        r#"(test-fc-doc-arg-order
                 'default
                 '((t nil))
                 "Basic default face."
                 :group
                 'basic-faces)"#,
    )
    .unwrap();
    let result = eval
        .eval_expr(&call[0])
        .expect("compiled defun should preserve argument order");
    assert_eq!(
        result,
        Value::list(vec![
            Value::symbol("default"),
            Value::list(vec![Value::list(vec![Value::T, Value::NIL])]),
            Value::string("Basic default face."),
            Value::list(vec![Value::symbol(":group"), Value::symbol("basic-faces")]),
        ])
    );
}

#[test]
fn test_compile_defun_preserves_doc_named_argument_order_through_conditional() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let forms = parse_forms(
        r#"(defun test-fc-doc-conditional (face spec doc &rest args)
                 "Function docstring."
                 (if (and doc (stringp doc))
                     (list face spec doc args)
                   'bad))"#,
    )
    .unwrap();
    let compiled = compile_file_forms(&mut eval, &forms).unwrap();
    assert_eq!(compiled.len(), 1);

    let CompiledForm::Eval(value) = &compiled[0] else {
        panic!("expected Eval form");
    };
    eval.eval_sub(*value)
        .expect("compiled defun defalias should install");

    let call = parse_forms(
        r#"(test-fc-doc-conditional
                 'default
                 '((t nil))
                 "Basic default face."
                 :group
                 'basic-faces)"#,
    )
    .unwrap();
    let result = eval
        .eval_expr(&call[0])
        .expect("compiled defun should preserve argument order through conditional");
    assert_eq!(
        result,
        Value::list(vec![
            Value::symbol("default"),
            Value::list(vec![Value::list(vec![Value::T, Value::NIL])]),
            Value::string("Basic default face."),
            Value::list(vec![Value::symbol(":group"), Value::symbol("basic-faces")]),
        ])
    );
}

#[test]
fn test_compile_defun_direct_value_matches_expr_path_for_real_custom_declare_face() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let defun_form = real_cus_face_defun_form("custom-declare-face");

    let Expr::List(expr_items) = &defun_form else {
        panic!("expected custom-declare-face defun list");
    };
    let expr_compiled =
        compile_toplevel_defun_direct(&mut eval, expr_items).expect("expr path should compile");

    let runtime_value = quote_to_value(&defun_form);
    let runtime_items = list_to_vec(&runtime_value).expect("runtime form should be a list");
    let value_compiled = compile_toplevel_defun_direct_value(&mut eval, &runtime_items)
        .expect("runtime-value path should compile");

    let expr_items = list_to_vec(&expr_compiled).expect("expr compiled form should be a list");
    let value_items = list_to_vec(&value_compiled).expect("value compiled form should be a list");
    assert_eq!(expr_items[0], value_items[0]);
    assert_eq!(expr_items[1], value_items[1]);
    assert_eq!(expr_items[3], value_items[3]);

    let expr_bc = expr_items[2].get_bytecode_data().expect("expr bytecode");
    let value_bc = value_items[2].get_bytecode_data().expect("value bytecode");
    assert_eq!(expr_bc.ops, value_bc.ops);
    assert_eq!(expr_bc.constants, value_bc.constants);
    assert_eq!(expr_bc.params, value_bc.params);
    assert_eq!(expr_bc.lexical, value_bc.lexical);
    assert_eq!(expr_bc.docstring, value_bc.docstring);
    assert_eq!(expr_bc.doc_form, value_bc.doc_form);
    assert_eq!(expr_bc.interactive, value_bc.interactive);
}

#[test]
fn test_compile_defun_preserves_required_required_rest_locals() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let forms = parse_forms(
        r#"(defun test-fc-rest-locals (face frame &rest args)
                 "Function docstring."
                 (let ((where (if (null frame) 0 frame))
                       (spec args))
                   (list face frame args where spec)))"#,
    )
    .unwrap();
    let compiled = compile_file_forms(&mut eval, &forms).unwrap();
    assert_eq!(compiled.len(), 1);

    let CompiledForm::Eval(value) = &compiled[0] else {
        panic!("expected Eval form");
    };
    eval.eval_sub(*value)
        .expect("compiled defun defalias should install");

    let call = parse_forms(
        r#"(test-fc-rest-locals
                 'default
                 'frame-1
                 :family
                 "Mono"
                 :weight
                 'bold)"#,
    )
    .unwrap();
    let result = eval
        .eval_expr(&call[0])
        .expect("compiled defun should preserve required/rest locals");
    let rest = Value::list(vec![
        Value::symbol(":family"),
        Value::string("Mono"),
        Value::symbol(":weight"),
        Value::symbol("bold"),
    ]);
    assert_eq!(
        result,
        Value::list(vec![
            Value::symbol("default"),
            Value::symbol("frame-1"),
            rest,
            Value::symbol("frame-1"),
            rest,
        ])
    );
}

#[test]
fn test_compile_defun_preserves_required_required_rest_through_while_and_setq() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let forms = parse_forms(
        r#"(defun test-fc-rest-loop (face frame &rest args)
                 "Function docstring."
                 (let ((seen nil))
                   (while args
                     (setq seen (cons (car args) seen))
                     (setq args (cdr (cdr args))))
                   (list face frame (nreverse seen) args)))"#,
    )
    .unwrap();
    let compiled = compile_file_forms(&mut eval, &forms).unwrap();
    assert_eq!(compiled.len(), 1);

    let CompiledForm::Eval(value) = &compiled[0] else {
        panic!("expected Eval form");
    };
    eval.eval_sub(*value)
        .expect("compiled defun defalias should install");

    let call = parse_forms(
        r#"(test-fc-rest-loop
                 'default
                 'frame-1
                 :family
                 "Mono"
                 :weight
                 'bold)"#,
    )
    .unwrap();
    let result = eval
        .eval_expr(&call[0])
        .expect("compiled defun should preserve rest variable through loop");
    assert_eq!(
        result,
        Value::list(vec![
            Value::symbol("default"),
            Value::symbol("frame-1"),
            Value::list(vec![Value::symbol(":family"), Value::symbol(":weight")]),
            Value::NIL,
        ])
    );
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
    // `read_neobc` decodes bytecode literals through the thread-local
    // opaque pool, so evaluate the loaded forms under the same fresh heap
    // lifetime they were decoded into, matching the real loader path.
    let mut runtime_eval = Context::new();
    let loaded = read_neobc(&neobc_path, "").unwrap();
    assert_eq!(loaded.forms.len(), 1);
    for form in &loaded.forms {
        match form {
            LoadedForm::Eval(value) => {
                runtime_eval.eval_sub(*value).unwrap();
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
fn test_compile_el_to_neobc_preserves_compile_time_exported_defun_for_require() {
    crate::test_utils::init_test_tracing();

    let dir = tempfile::tempdir().unwrap();
    let el_path = dir.path().join("compile-session-export.el");
    let source = ";; -*- lexical-binding: nil -*-\n\
                      (defun compile-session-exported-helper () 7)\n\
                      (provide 'compile-session-export)\n";
    std::fs::write(&el_path, source).unwrap();

    let mut eval = Context::new();
    let load_path = Value::list(vec![Value::string(dir.path().display().to_string())]);
    eval.set_variable("load-path", load_path);

    compile_el_to_neobc(&mut eval, &el_path).unwrap();
    eval.require_value(Value::symbol("compile-session-export"), None, None)
        .expect("same-session require should succeed after compile-only pass");

    let helper = eval
        .obarray()
        .symbol_function("compile-session-exported-helper")
        .copied()
        .expect("compile-only pass should leave exported defun installed");
    assert!(
        helper.get_bytecode_data().is_some(),
        "compile-only pass should install lowered bytecode-backed defun in the compiler session"
    );
    assert_eq!(
        eval.eval_sub(Value::list(vec![Value::symbol(
            "compile-session-exported-helper"
        )]))
        .unwrap(),
        Value::fixnum(7)
    );
}

#[test]
fn test_compile_el_to_neobc_preserves_compile_time_exported_defalias_for_require() {
    crate::test_utils::init_test_tracing();

    let dir = tempfile::tempdir().unwrap();
    let el_path = dir.path().join("compile-session-defalias.el");
    let source = ";; -*- lexical-binding: nil -*-\n\
                      (defalias 'compile-session-defalias-helper (lambda () 9))\n\
                      (provide 'compile-session-defalias)\n";
    std::fs::write(&el_path, source).unwrap();

    let mut eval = Context::new();
    let load_path = Value::list(vec![Value::string(dir.path().display().to_string())]);
    eval.set_variable("load-path", load_path);

    compile_el_to_neobc(&mut eval, &el_path).unwrap();
    eval.require_value(Value::symbol("compile-session-defalias"), None, None)
        .expect("same-session require should succeed after compile-only pass");

    let helper = eval
        .obarray()
        .symbol_function("compile-session-defalias-helper")
        .copied()
        .expect("compile-only pass should leave exported defalias installed");
    assert!(
        helper.get_bytecode_data().is_some(),
        "compile-only pass should install lowered bytecode-backed defalias in the compiler session"
    );
    assert_eq!(
        eval.eval_sub(Value::list(vec![Value::symbol(
            "compile-session-defalias-helper"
        )]))
        .unwrap(),
        Value::fixnum(9)
    );
}

#[test]
fn test_compile_el_to_neobc_round_trips_easy_mmode_shape_macro() {
    crate::test_utils::init_test_tracing();

    let dir = tempfile::tempdir().unwrap();
    let el_path = dir.path().join("compiled-easy-mmode-shape.el");
    let source = r#";; -*- lexical-binding: t -*-
(defmacro test-compiled-easy-mmode-shape (mode getter)
  (let ((type nil))
    (unless type (setq type '(:type 'boolean)))
    `(progn
       (defcustom ,mode nil "doc" ,@type)
       ,(let ((modevar (pcase getter (`(default-value ',v) v) (_ getter)))
              (minor-modes-var 'local-minor-modes))
          (if (not (symbolp modevar))
              (error "bad modevar")
            `(with-no-warnings
               (when (boundp ',minor-modes-var)
                 (setq ,minor-modes-var
                       (delq ',modevar ,minor-modes-var)))))))))
"#;
    std::fs::write(&el_path, source).unwrap();

    let mut source_eval = direct_source_compile_surface_eval(true);
    eval_source_file_direct(&mut source_eval, &el_path);

    let mut compiler_eval = direct_source_compile_surface_eval(true);
    compile_el_to_neobc(&mut compiler_eval, &el_path).unwrap();

    let neobc_path = el_path.with_extension("neobc");
    let loaded = read_neobc(&neobc_path, "").unwrap();
    assert_eq!(loaded.forms.len(), 1);

    let mut runtime_eval = direct_source_compile_surface_eval(true);
    for form in &loaded.forms {
        match form {
            LoadedForm::Eval(value) | LoadedForm::EagerEval(value) => {
                runtime_eval
                    .eval_sub(*value)
                    .expect("compiled easy-mmode-shaped macro should install");
            }
            LoadedForm::Constant(_) => {}
        }
    }

    let compiled_func = runtime_eval
        .obarray()
        .symbol_function("test-compiled-easy-mmode-shape")
        .copied()
        .expect("compiled easy-mmode-shaped macro should be installed");
    assert_eq!(compiled_func.cons_car().as_symbol_name(), Some("macro"));
    assert!(
        compiled_func.cons_cdr().get_bytecode_data().is_some(),
        "compiled easy-mmode-shaped macro should replay as bytecode"
    );

    let macroexpand =
        parse_forms("(macroexpand '(test-compiled-easy-mmode-shape sample-mode sample-mode))")
            .unwrap();
    let source_expanded = source_eval
        .eval_expr(&macroexpand[0])
        .expect("source easy-mmode-shaped macroexpand should succeed");
    let compiled_expanded = runtime_eval
        .eval_expr(&macroexpand[0])
        .expect("compiled easy-mmode-shaped macroexpand should succeed");

    assert_eq!(
        normalized_value(compiled_expanded),
        normalized_value(source_expanded),
        "compiled easy-mmode-shaped neobc replay should match source macro expansion"
    );
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
