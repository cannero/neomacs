//! File-level byte compilation.
//!
//! Processes top-level forms from a parsed `.el` file, evaluating
//! `eval-when-compile` bodies at compile time and emitting the results as
//! constants.  All other forms are evaluated for side effects (so that
//! `defun`, `defvar`, `require`, etc. take effect in the compile-time
//! environment) and also emitted as `Eval` forms to replay at load time.

use std::{cell::RefCell, path::Path};

use super::builtins::parse_lambda_params_from_value;
use super::bytecode::Compiler;
use super::error::{EvalError, Flow, map_flow};
use super::eval::{Context, INTERNAL_COMPILER_FUNCTION_OVERRIDES, quote_to_value, value_to_expr};
use super::expr::Expr;
use super::intern::{SymId, intern, resolve_sym};
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

fn compiled_form_roots(forms: &[CompiledForm]) -> Vec<Value> {
    forms.iter().map(CompiledForm::root_value).collect()
}

fn root_saved_values(ctx: &mut Context, roots: &[Value]) {
    for root in roots {
        ctx.root(*root);
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
        if let Expr::List(items) = form
            && matches!(items.first(), Some(Expr::Symbol(id)) if resolve_sym(*id) == "eval-and-compile")
        {
            eval.sf_progn(&items[1..])?;
            out.push(CompiledForm::Eval(form_value));
            return Ok(());
        }
        eval.eval(form)?;
        out.push(CompiledForm::Eval(form_value));
        return Ok(());
    };
    let emitted_roots = RefCell::new(compiled_form_roots(out));

    super::load::eager_expand_toplevel_forms_with_extra_roots(
        eval,
        form_value,
        macroexpand_fn,
        &mut |ctx| root_saved_values(ctx, &emitted_roots.borrow()),
        &mut |ctx, original, expanded, requires_eager_replay| {
            ctx.with_gc_scope_result(|ctx| {
                root_saved_values(ctx, &emitted_roots.borrow());
                ctx.root(original);
                ctx.root(expanded);
                ctx.eval_value(&expanded)?;
                let compiled = if requires_eager_replay {
                    CompiledForm::EagerEval(original)
                } else {
                    CompiledForm::Eval(expanded)
                };
                emitted_roots.borrow_mut().push(compiled.root_value());
                out.push(compiled);
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
    let compile_scope = eval.open_gc_scope();
    let old_function_overrides = eval
        .obarray()
        .symbol_value(INTERNAL_COMPILER_FUNCTION_OVERRIDES)
        .copied()
        .unwrap_or(Value::NIL);
    let result = (|| {
        let mut compiled = Vec::new();
        let mut compiler_macro_env = Value::NIL;
        let mut compiler_function_overrides = old_function_overrides;
        let mut deferred_defmacros = Vec::new();
        let mut rooted_compiled_len = 0usize;
        eval.set_variable(
            INTERNAL_COMPILER_FUNCTION_OVERRIDES,
            compiler_function_overrides,
        );

        for form in forms {
            let compiled_roots: Vec<Value> =
                compiled.iter().map(CompiledForm::root_value).collect();
            eval.with_gc_scope_result(|ctx| {
                for root in &compiled_roots {
                    ctx.root(*root);
                }
                ctx.root(compiler_macro_env);
                ctx.root(compiler_function_overrides);
                compile_toplevel_file_form(
                    ctx,
                    form,
                    &mut compiled,
                    &mut compiler_macro_env,
                    &mut compiler_function_overrides,
                    &mut deferred_defmacros,
                )
            })?;

            while rooted_compiled_len < compiled.len() {
                eval.root(compiled[rooted_compiled_len].root_value());
                rooted_compiled_len += 1;
            }
            if !compiler_macro_env.is_nil() {
                eval.root(compiler_macro_env);
            }
            if !compiler_function_overrides.is_nil() {
                eval.root(compiler_function_overrides);
            }
            eval.set_variable(
                INTERNAL_COMPILER_FUNCTION_OVERRIDES,
                compiler_function_overrides,
            );
        }

        for deferred in deferred_defmacros {
            let compiled_roots: Vec<Value> =
                compiled.iter().map(CompiledForm::root_value).collect();
            let replacement = eval.with_gc_scope_result(|ctx| {
                for root in &compiled_roots {
                    ctx.root(*root);
                }
                ctx.root(compiler_macro_env);
                ctx.root(compiler_function_overrides);
                compile_toplevel_defmacro_with_env(ctx, &deferred.form, compiler_macro_env)
            })?;
            if let Some(compiled_form) = replacement {
                compiled[deferred.index] = CompiledForm::Eval(compiled_form);
                eval.root(compiled[deferred.index].root_value());
            }
            if !compiler_macro_env.is_nil() {
                eval.root(compiler_macro_env);
            }
            if !compiler_function_overrides.is_nil() {
                eval.root(compiler_function_overrides);
            }
            eval.set_variable(
                INTERNAL_COMPILER_FUNCTION_OVERRIDES,
                compiler_function_overrides,
            );
        }
        Ok(compiled)
    })();
    eval.set_variable(INTERNAL_COMPILER_FUNCTION_OVERRIDES, old_function_overrides);
    compile_scope.close(eval);
    result
}

struct DeferredDefmacro {
    index: usize,
    form: Expr,
}

struct LambdaMetadata {
    docstring: Option<String>,
    doc_form: Option<Value>,
    interactive: Option<Value>,
    compiler_macro: Option<Expr>,
    body_start: usize,
}

struct ValueLambdaMetadata {
    docstring: Option<String>,
    doc_form: Option<Value>,
    interactive: Option<Value>,
    compiler_macro: Option<Value>,
    body_start: usize,
}

fn parse_lambda_metadata_from_expr_body(body: &[Expr]) -> LambdaMetadata {
    let (docstring, mut body_start) = match (body.first(), body.get(1)) {
        (Some(Expr::Str(s)), Some(_)) => (Some(s.clone()), 1),
        _ => (None, 0),
    };

    let (doc_form, next_body_start) = if let Some(Expr::List(items)) = body.get(body_start) {
        let is_doc_form = matches!(
            items.first(),
            Some(Expr::Keyword(id) | Expr::Symbol(id)) if resolve_sym(*id) == ":documentation"
        );
        if is_doc_form {
            (
                Some(items.get(1).map(quote_to_value).unwrap_or(Value::NIL)),
                body_start + 1,
            )
        } else {
            (None, body_start)
        }
    } else {
        (None, body_start)
    };
    body_start = next_body_start;

    let mut compiler_macro = None;
    while matches!(
        body.get(body_start),
        Some(Expr::List(items))
            if matches!(items.first(), Some(Expr::Symbol(id)) if resolve_sym(*id) == "declare")
    ) {
        if let Some(Expr::List(items)) = body.get(body_start) {
            for decl in items.iter().skip(1) {
                let Expr::List(decl_items) = decl else {
                    continue;
                };
                if matches!(
                    (decl_items.first(), decl_items.get(1)),
                    (Some(Expr::Symbol(id)), Some(_)) if resolve_sym(*id) == "compiler-macro"
                ) {
                    compiler_macro = decl_items.get(1).cloned();
                }
            }
        }
        body_start += 1;
    }

    let interactive = if let Some(Expr::List(items)) = body.get(body_start) {
        if matches!(items.first(), Some(Expr::Symbol(id)) if resolve_sym(*id) == "interactive") {
            body_start += 1;
            Some(if items.len() <= 2 {
                items.get(1).map(quote_to_value).unwrap_or(Value::NIL)
            } else {
                Value::vector(items[1..].iter().map(quote_to_value).collect())
            })
        } else {
            None
        }
    } else {
        None
    };

    LambdaMetadata {
        docstring,
        doc_form,
        interactive,
        compiler_macro,
        body_start,
    }
}

fn parse_lambda_metadata_from_value_body(body: &[Value]) -> ValueLambdaMetadata {
    let (docstring, mut body_start) = match (body.first(), body.get(1)) {
        (Some(value), Some(_)) if value.as_str().is_some() => (value.as_str_owned(), 1),
        _ => (None, 0),
    };

    let (doc_form, next_body_start) = if let Some(value) = body.get(body_start) {
        if let Some(items) = list_to_vec(value) {
            if items.first().and_then(|value| value.as_symbol_name()) == Some(":documentation") {
                (
                    Some(items.get(1).copied().unwrap_or(Value::NIL)),
                    body_start + 1,
                )
            } else {
                (None, body_start)
            }
        } else {
            (None, body_start)
        }
    } else {
        (None, body_start)
    };
    body_start = next_body_start;

    let mut compiler_macro = None;
    while let Some(value) = body.get(body_start) {
        let Some(items) = list_to_vec(value) else {
            break;
        };
        if items.first().and_then(|value| value.as_symbol_name()) != Some("declare") {
            break;
        }
        for decl in items.iter().skip(1) {
            let Some(decl_items) = list_to_vec(decl) else {
                continue;
            };
            if decl_items.first().and_then(|value| value.as_symbol_name()) == Some("compiler-macro")
            {
                compiler_macro = decl_items.get(1).copied();
            }
        }
        body_start += 1;
    }

    let interactive = if let Some(value) = body.get(body_start) {
        if let Some(items) = list_to_vec(value) {
            if items.first().and_then(|value| value.as_symbol_name()) == Some("interactive") {
                body_start += 1;
                Some(if items.len() <= 2 {
                    items.get(1).copied().unwrap_or(Value::NIL)
                } else {
                    Value::vector(items[1..].to_vec())
                })
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    ValueLambdaMetadata {
        docstring,
        doc_form,
        interactive,
        compiler_macro,
        body_start,
    }
}

fn macroexpand_all_fn(eval: &Context) -> Option<Value> {
    let f = eval.obarray().symbol_function("macroexpand-all").cloned()?;
    if f.is_nil() { None } else { Some(f) }
}

fn has_symbol_function(eval: &Context, name: &str) -> bool {
    eval.obarray()
        .symbol_function(name)
        .is_some_and(|value| !value.is_nil())
}

fn feature_enabled(eval: &Context, feature: &str) -> bool {
    let Some(features) = eval.obarray().symbol_value("features").copied() else {
        return false;
    };
    let Some(items) = list_to_vec(&features) else {
        return false;
    };
    items
        .iter()
        .copied()
        .any(|value| value.as_symbol_name() == Some(feature))
}

fn load_compiler_support_library(
    eval: &mut Context,
    current_path: &Path,
    load_path: &[String],
    logical_name: &str,
) -> Result<(), CompileFileError> {
    let Some(path) = super::load::find_file_in_load_path(logical_name, load_path) else {
        return Err(CompileFileError::Eval(format!(
            "compiler surface missing {logical_name}"
        )));
    };
    if path == current_path {
        return Ok(());
    }
    super::load::load_file(eval, &path)
        .map(|_| ())
        .map_err(|err| CompileFileError::Eval(format!("{logical_name}: {err:?}")))
}

fn ensure_minimal_compiler_bootstrap_loaded(
    eval: &mut Context,
    current_path: &Path,
    load_path: &[String],
) -> Result<(), CompileFileError> {
    if !has_symbol_function(eval, "defun")
        || !has_symbol_function(eval, "defmacro")
        || !has_symbol_function(eval, "eval-and-compile")
    {
        load_compiler_support_library(eval, current_path, load_path, "emacs-lisp/debug-early")?;
        load_compiler_support_library(eval, current_path, load_path, "emacs-lisp/byte-run")?;
    }

    if !feature_enabled(eval, "backquote") {
        load_compiler_support_library(eval, current_path, load_path, "emacs-lisp/backquote")?;
    }

    if macroexpand_all_fn(eval).is_none() {
        load_compiler_support_library(eval, current_path, load_path, "subr")?;
    }

    Ok(())
}

fn ensure_compiler_support_loaded(
    eval: &mut Context,
    current_path: &Path,
) -> Result<(), CompileFileError> {
    let current_name = current_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let mut load_path = super::load::get_load_path(&eval.obarray());
    for dir in super::load::runtime_bootstrap_load_path() {
        if !load_path.iter().any(|existing| existing == &dir) {
            load_path.push(dir);
        }
    }

    ensure_minimal_compiler_bootstrap_loaded(eval, current_path, &load_path)?;

    let had_pcase = feature_enabled(eval, "pcase");
    let support = [
        ("macroexp", "emacs-lisp/macroexp", "macroexp.el"),
        ("pcase", "emacs-lisp/pcase", "pcase.el"),
        ("gv", "emacs-lisp/gv", "gv.el"),
    ];

    for (feature, logical_name, file_name) in support {
        if current_name == file_name || feature_enabled(eval, feature) {
            continue;
        }
        load_compiler_support_library(eval, current_path, &load_path, logical_name)?;
    }

    // GNU's compiler surface expects macroexp to see the pcase/backquote
    // expander stack. If pcase became available during this ensure step,
    // reload macroexp so `macroexpand-all` does not keep an earlier surface.
    if current_name != "macroexp.el" && !had_pcase && feature_enabled(eval, "pcase") {
        load_compiler_support_library(eval, current_path, &load_path, "emacs-lisp/macroexp")?;
    }

    if macroexpand_all_fn(eval).is_none() {
        return Err(CompileFileError::Eval(
            "compiler surface missing macroexpand-all".into(),
        ));
    }

    eval.set_variable("macroexp--pending-eager-loads", Value::NIL);
    Ok(())
}

fn with_runtime_macro_cache_disabled<T>(
    eval: &mut Context,
    f: impl FnOnce(&mut Context) -> T,
) -> T {
    let old = eval.macro_cache_disabled;
    eval.macro_cache_disabled = true;
    let result = f(eval);
    eval.macro_cache_disabled = old;
    result
}

fn expand_compiler_body_values(
    eval: &mut Context,
    body_values: &[Value],
    macroexpand_env: Value,
) -> Option<Vec<Expr>> {
    let macroexpand_fn = macroexpand_all_fn(eval)?;
    let body_form = match body_values {
        [] => Value::NIL,
        [single] => *single,
        _ => {
            let mut progn = Vec::with_capacity(body_values.len() + 1);
            progn.push(Value::symbol("progn"));
            progn.extend(body_values.iter().copied());
            Value::list(progn)
        }
    };
    let expanded = with_runtime_macro_cache_disabled(eval, |eval| {
        eval.with_gc_scope_result(|ctx| {
            ctx.root(macroexpand_fn);
            ctx.root(macroexpand_env);
            ctx.root(body_form);
            ctx.apply(macroexpand_fn, vec![body_form, macroexpand_env])
        })
    })
    .ok()?;
    let expanded_values =
        if expanded.is_cons() && expanded.cons_car().as_symbol_name() == Some("progn") {
            list_to_vec(&expanded.cons_cdr()).unwrap_or_else(|| vec![expanded])
        } else {
            vec![expanded]
        };
    Some(if expanded_values.is_empty() {
        vec![Expr::Bool(false)]
    } else {
        expanded_values
            .into_iter()
            .map(|value| value_to_expr(&value))
            .collect()
    })
}

fn expand_compiler_toplevel_expr(eval: &mut Context, form_value: Value) -> Option<Expr> {
    expand_compiler_toplevel_expr_with_env(eval, form_value, Value::NIL)
}

fn expand_compiler_toplevel_expr_with_env(
    eval: &mut Context,
    form_value: Value,
    macroexpand_env: Value,
) -> Option<Expr> {
    let macroexpand_fn = macroexpand_all_fn(eval)?;
    let expanded = with_runtime_macro_cache_disabled(eval, |eval| {
        eval.with_gc_scope_result(|ctx| {
            ctx.root(macroexpand_fn);
            ctx.root(macroexpand_env);
            ctx.root(form_value);
            ctx.apply(macroexpand_fn, vec![form_value, macroexpand_env])
        })
    })
    .ok()?;
    Some(value_to_expr(&expanded))
}

fn compile_body_exprs(eval: &mut Context, body_values: &[Value]) -> Vec<Expr> {
    compile_body_exprs_with_env(eval, body_values, Value::NIL)
}

fn compile_body_exprs_with_env(
    eval: &mut Context,
    body_values: &[Value],
    macroexpand_env: Value,
) -> Vec<Expr> {
    if let Some(expanded) = expand_compiler_body_values(eval, body_values, macroexpand_env) {
        return expanded;
    }
    if body_values.is_empty() {
        vec![Expr::Bool(false)]
    } else {
        body_values.iter().map(value_to_expr).collect()
    }
}

fn compile_expanded_body_exprs(body: &[Expr]) -> Vec<Expr> {
    if body.is_empty() {
        vec![Expr::Bool(false)]
    } else {
        body.to_vec()
    }
}

fn quoted_symbol_expr(symbol: SymId) -> Expr {
    Expr::List(vec![Expr::Symbol(intern("quote")), Expr::Symbol(symbol)])
}

fn expr_quoted_symbol_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Symbol(id) => Some(resolve_sym(*id).to_owned()),
        Expr::List(items) if items.len() == 2 => match (&items[0], &items[1]) {
            (Expr::Symbol(head), Expr::Symbol(id)) if resolve_sym(*head) == "quote" => {
                Some(resolve_sym(*id).to_owned())
            }
            _ => None,
        },
        _ => None,
    }
}

fn expr_quoted_symbol_id(expr: &Expr) -> Option<SymId> {
    match expr {
        Expr::Symbol(id) => Some(*id),
        Expr::List(items) if items.len() == 2 => match (&items[0], &items[1]) {
            (Expr::Symbol(head), Expr::Symbol(id)) if resolve_sym(*head) == "quote" => Some(*id),
            _ => None,
        },
        _ => None,
    }
}

fn is_quoted_symbol(value: Value, name: &str) -> bool {
    value.is_cons()
        && value.cons_car().as_symbol_name() == Some("quote")
        && value.cons_cdr().is_cons()
        && value.cons_cdr().cons_car().as_symbol_name() == Some(name)
        && value.cons_cdr().cons_cdr().is_nil()
}

fn quoted_symbol_value(symbol: SymId) -> Value {
    Value::list(vec![Value::symbol("quote"), Value::symbol(symbol)])
}

fn build_compiled_defalias_value(name: SymId, target: Value, docstring: Option<String>) -> Value {
    let mut form = vec![Value::symbol("defalias"), quoted_symbol_value(name), target];
    if let Some(doc) = docstring {
        form.push(Value::string(doc));
    }
    Value::list(form)
}

fn build_compiled_macro_defalias_value(
    name: SymId,
    target: Value,
    docstring: Option<String>,
) -> Value {
    let macro_target = Value::list(vec![
        Value::symbol("cons"),
        quoted_symbol_value(intern("macro")),
        target,
    ]);
    let mut form = vec![
        Value::symbol("defalias"),
        quoted_symbol_value(name),
        macro_target,
    ];
    if let Some(doc) = docstring {
        form.push(Value::string(doc));
    }
    Value::list(form)
}

fn compile_lambda_expanded_expr(eval: &mut Context, lambda: &Expr) -> Option<Value> {
    compile_lambda_expanded_expr_with_env(eval, lambda, Value::NIL)
}

fn compile_lambda_expanded_expr_with_env(
    eval: &mut Context,
    lambda: &Expr,
    macroexpand_env: Value,
) -> Option<Value> {
    let Expr::List(items) = lambda else {
        return None;
    };
    match items.first() {
        Some(Expr::Symbol(id)) if resolve_sym(*id) == "lambda" => {}
        _ => return None,
    }

    let params = parse_lambda_params_from_value(&quote_to_value(items.get(1)?)).ok()?;
    let metadata = parse_lambda_metadata_from_expr_body(&items[2..]);
    let body = items.get(2 + metadata.body_start..)?;
    let body_values: Vec<Value> = body.iter().map(quote_to_value).collect();
    let body_exprs = compile_body_exprs_with_env(eval, &body_values, macroexpand_env);

    let mut compiler = Compiler::new(eval.lexical_binding());
    let mut bytecode = compiler.compile_lambda(&params, &body_exprs);
    bytecode.docstring = metadata.docstring;
    bytecode.doc_form = metadata.doc_form.filter(|value| !value.is_nil());
    bytecode.interactive = metadata.interactive.filter(|value| !value.is_nil());
    Some(Value::make_bytecode(bytecode))
}

fn compile_function_expanded_expr(eval: &mut Context, function: &Expr) -> Option<Value> {
    compile_function_expanded_expr_with_env(eval, function, Value::NIL)
}

fn compile_function_expanded_expr_with_env(
    eval: &mut Context,
    function: &Expr,
    macroexpand_env: Value,
) -> Option<Value> {
    let Expr::List(items) = function else {
        return None;
    };
    match items.first() {
        Some(Expr::Symbol(id)) if resolve_sym(*id) == "function" => {}
        _ => return None,
    }
    compile_lambda_expanded_expr_with_env(eval, items.get(1)?, macroexpand_env)
}

fn compile_defalias_target_expanded_expr(eval: &mut Context, target: &Expr) -> Option<Value> {
    compile_defalias_target_expanded_expr_with_env(eval, target, Value::NIL)
}

fn compile_defalias_target_expanded_expr_with_env(
    eval: &mut Context,
    target: &Expr,
    macroexpand_env: Value,
) -> Option<Value> {
    if let Some(compiled) = compile_function_expanded_expr_with_env(eval, target, macroexpand_env) {
        return Some(compiled);
    }

    let Expr::List(items) = target else {
        return None;
    };
    if items.len() != 3 {
        return None;
    }
    match items.first() {
        Some(Expr::Symbol(id)) if resolve_sym(*id) == "cons" => {}
        _ => return None,
    }
    match items.get(1)? {
        Expr::List(quote_items)
            if matches!(
                quote_items.as_slice(),
                [Expr::Symbol(quote_id), Expr::Symbol(macro_id)]
                    if resolve_sym(*quote_id) == "quote" && resolve_sym(*macro_id) == "macro"
            ) => {}
        _ => return None,
    }

    let compiled = compile_function_expanded_expr_with_env(eval, items.get(2)?, macroexpand_env)?;
    Some(quote_to_value(&Expr::List(vec![
        Expr::Symbol(intern("cons")),
        quoted_symbol_expr(intern("macro")),
        value_to_expr(&compiled),
    ])))
}

fn compile_macroexpanded_defalias_expr(eval: &mut Context, expanded: &Expr) -> Option<Value> {
    compile_macroexpanded_defalias_expr_with_env(eval, expanded, Value::NIL)
}

fn compile_macroexpanded_defalias_expr_with_env(
    eval: &mut Context,
    expanded: &Expr,
    macroexpand_env: Value,
) -> Option<Value> {
    let Expr::List(items) = expanded else {
        return None;
    };
    let head = match items.first() {
        Some(Expr::Symbol(id)) => resolve_sym(*id),
        _ => return None,
    };
    match head {
        "defalias" => {
            let compiled = compile_defalias_target_expanded_expr_with_env(
                eval,
                items.get(2)?,
                macroexpand_env,
            )?;
            let mut rebuilt = items.clone();
            rebuilt[2] = value_to_expr(&compiled);
            Some(quote_to_value(&Expr::List(rebuilt)))
        }
        "prog1" => {
            let compiled =
                compile_macroexpanded_defalias_expr_with_env(eval, items.get(1)?, macroexpand_env)?;
            let mut rebuilt = items.clone();
            rebuilt[1] = value_to_expr(&compiled);
            Some(quote_to_value(&Expr::List(rebuilt)))
        }
        _ => None,
    }
}

fn compiler_macro_value_value(handler: Value) -> Value {
    if let Some(id) = handler.as_symbol_id() {
        return quoted_symbol_value(id);
    }
    if handler.is_cons() && handler.cons_car().as_symbol_name() == Some("lambda") {
        return Value::list(vec![Value::symbol("function"), handler]);
    }
    handler
}

fn has_compiler_macro_put_value(value: Value, name: SymId) -> bool {
    let Some(items) = list_to_vec(&value) else {
        return false;
    };
    let Some(head) = items.first().and_then(|value| value.as_symbol_name()) else {
        return false;
    };
    match head {
        "function-put" | "put" => {
            matches!(
                (items.get(1), items.get(2)),
                (Some(name_value), Some(prop_value))
                    if quoted_symbol_id(*name_value) == Some(name)
                        && quoted_symbol_id(*prop_value) == Some(intern("compiler-macro"))
            )
        }
        "prog1" | "progn" => items
            .iter()
            .skip(1)
            .copied()
            .any(|item| has_compiler_macro_put_value(item, name)),
        _ => false,
    }
}

fn compiler_macro_value_expr(handler: &Expr) -> Expr {
    match handler {
        Expr::Symbol(id) => quoted_symbol_expr(*id),
        Expr::List(items) if matches!(items.first(), Some(Expr::Symbol(id)) if resolve_sym(*id) == "lambda") => {
            Expr::List(vec![Expr::Symbol(intern("function")), handler.clone()])
        }
        _ => handler.clone(),
    }
}

fn has_compiler_macro_put(expr: &Expr, name: SymId) -> bool {
    let Expr::List(items) = expr else {
        return false;
    };
    let Some(Expr::Symbol(head)) = items.first() else {
        return false;
    };
    match resolve_sym(*head) {
        "function-put" | "put" => {
            matches!(
                (items.get(1), items.get(2)),
                (Some(name_expr), Some(prop_expr))
                    if expr_quoted_symbol_name(name_expr).as_deref() == Some(resolve_sym(name))
                        && expr_quoted_symbol_name(prop_expr).as_deref() == Some("compiler-macro")
            )
        }
        "prog1" | "progn" => items
            .iter()
            .skip(1)
            .any(|item| has_compiler_macro_put(item, name)),
        _ => false,
    }
}

fn maybe_wrap_compiled_defalias_with_compiler_macro(
    name: SymId,
    compiled_form: Value,
    compiler_macro: Option<&Expr>,
) -> Value {
    let Some(handler) = compiler_macro else {
        return compiled_form;
    };
    let compiled_expr = value_to_expr(&compiled_form);
    if has_compiler_macro_put(&compiled_expr, name) {
        return compiled_form;
    }

    quote_to_value(&Expr::List(vec![
        Expr::Symbol(intern("prog1")),
        compiled_expr,
        Expr::List(vec![
            Expr::Symbol(intern("function-put")),
            quoted_symbol_expr(name),
            quoted_symbol_expr(intern("compiler-macro")),
            compiler_macro_value_expr(handler),
        ]),
    ]))
}

fn maybe_wrap_compiled_defalias_with_compiler_macro_value(
    name: SymId,
    compiled_form: Value,
    compiler_macro: Option<Value>,
) -> Value {
    let Some(handler) = compiler_macro else {
        return compiled_form;
    };
    if has_compiler_macro_put_value(compiled_form, name) {
        return compiled_form;
    }

    Value::list(vec![
        Value::symbol("prog1"),
        compiled_form,
        Value::list(vec![
            Value::symbol("function-put"),
            quoted_symbol_value(name),
            quoted_symbol_value(intern("compiler-macro")),
            compiler_macro_value_value(handler),
        ]),
    ])
}

fn compile_toplevel_defun_direct(eval: &mut Context, items: &[Expr]) -> Option<Value> {
    compile_toplevel_defun_direct_with_env(eval, items, Value::NIL)
}

fn compile_toplevel_defun_direct_with_env(
    eval: &mut Context,
    items: &[Expr],
    macroexpand_env: Value,
) -> Option<Value> {
    if items.len() < 4 {
        return None;
    }

    let Expr::Symbol(name_id) = items.get(1)? else {
        return None;
    };

    let arglist = items.get(2)?;
    let metadata = parse_lambda_metadata_from_expr_body(&items[3..]);
    let body = items.get(3 + metadata.body_start..)?;
    let params = parse_lambda_params_from_value(&quote_to_value(arglist)).ok()?;
    let mut compiler = Compiler::new(eval.lexical_binding());
    let body_values: Vec<Value> = body.iter().map(quote_to_value).collect();
    let body = compile_body_exprs_with_env(eval, &body_values, macroexpand_env);
    let mut bytecode = compiler.compile_lambda(&params, &body);
    bytecode.docstring = metadata.docstring.clone();
    bytecode.doc_form = metadata.doc_form.filter(|value| !value.is_nil());
    bytecode.interactive = metadata.interactive.filter(|value| !value.is_nil());

    let bytecode = Value::make_bytecode(bytecode);
    let compiled = build_compiled_defalias_value(*name_id, bytecode, metadata.docstring);
    Some(maybe_wrap_compiled_defalias_with_compiler_macro(
        *name_id,
        compiled,
        metadata.compiler_macro.as_ref(),
    ))
}

fn compile_toplevel_defun_direct_value(eval: &mut Context, items: &[Value]) -> Option<Value> {
    compile_toplevel_defun_direct_value_with_env(eval, items, Value::NIL)
}

fn compile_toplevel_defun_direct_value_with_env(
    eval: &mut Context,
    items: &[Value],
    macroexpand_env: Value,
) -> Option<Value> {
    if items.len() < 4 {
        return None;
    }

    let name_id = items.get(1)?.as_symbol_id()?;
    let params = parse_lambda_params_from_value(items.get(2)?).ok()?;
    let metadata = parse_lambda_metadata_from_value_body(&items[3..]);
    let body = items.get(3 + metadata.body_start..)?;

    let mut compiler = Compiler::new(eval.lexical_binding());
    let compiled_body = compile_body_exprs_with_env(eval, body, macroexpand_env);
    let mut bytecode = compiler.compile_lambda(&params, &compiled_body);
    bytecode.docstring = metadata.docstring.clone();
    bytecode.doc_form = metadata.doc_form.filter(|value| !value.is_nil());
    bytecode.interactive = metadata.interactive.filter(|value| !value.is_nil());

    let bytecode = Value::make_bytecode(bytecode);
    let compiled = build_compiled_defalias_value(name_id, bytecode, metadata.docstring);
    Some(maybe_wrap_compiled_defalias_with_compiler_macro_value(
        name_id,
        compiled,
        metadata.compiler_macro,
    ))
}

fn compile_toplevel_defmacro_direct(eval: &mut Context, items: &[Expr]) -> Option<Value> {
    compile_toplevel_defmacro_direct_with_env(eval, items, Value::NIL)
}

fn compile_toplevel_defmacro_direct_with_env(
    eval: &mut Context,
    items: &[Expr],
    macroexpand_env: Value,
) -> Option<Value> {
    if items.len() < 4 {
        return None;
    }

    let Expr::Symbol(name_id) = items.get(1)? else {
        return None;
    };

    let arglist = items.get(2)?;
    let metadata = parse_lambda_metadata_from_expr_body(&items[3..]);
    let body = items.get(3 + metadata.body_start..)?;
    let params = parse_lambda_params_from_value(&quote_to_value(arglist)).ok()?;
    let mut compiler = Compiler::new(eval.lexical_binding());
    let body_values: Vec<Value> = body.iter().map(quote_to_value).collect();
    let body = expand_compiler_body_values(eval, &body_values, macroexpand_env)?;
    let mut bytecode = compiler.compile_lambda(&params, &body);
    bytecode.docstring = metadata.docstring.clone();

    let bytecode = Value::make_bytecode(bytecode);
    Some(build_compiled_macro_defalias_value(
        *name_id,
        bytecode,
        metadata.docstring,
    ))
}

fn compile_toplevel_defmacro_direct_value(eval: &mut Context, items: &[Value]) -> Option<Value> {
    compile_toplevel_defmacro_direct_value_with_env(eval, items, Value::NIL)
}

fn compile_toplevel_defmacro_direct_value_with_env(
    eval: &mut Context,
    items: &[Value],
    macroexpand_env: Value,
) -> Option<Value> {
    if items.len() < 4 {
        return None;
    }

    let name_id = items.get(1)?.as_symbol_id()?;
    let params = parse_lambda_params_from_value(items.get(2)?).ok()?;
    let metadata = parse_lambda_metadata_from_value_body(&items[3..]);
    let body = items.get(3 + metadata.body_start..)?;

    let mut compiler = Compiler::new(eval.lexical_binding());
    let compiled_body = expand_compiler_body_values(eval, body, macroexpand_env)?;
    let mut bytecode = compiler.compile_lambda(&params, &compiled_body);
    bytecode.docstring = metadata.docstring.clone();

    let bytecode = Value::make_bytecode(bytecode);
    Some(build_compiled_macro_defalias_value(
        name_id,
        bytecode,
        metadata.docstring,
    ))
}

fn compile_toplevel_defun(eval: &mut Context, form: &Expr) -> Result<Option<Value>, Flow> {
    compile_toplevel_defun_with_env(eval, form, Value::NIL)
}

fn compile_toplevel_defun_with_env(
    eval: &mut Context,
    form: &Expr,
    macroexpand_env: Value,
) -> Result<Option<Value>, Flow> {
    let form_value = quote_to_value(form);
    let metadata = match form {
        Expr::List(items) if items.len() >= 4 => parse_lambda_metadata_from_expr_body(&items[3..]),
        _ => {
            return Ok(None);
        }
    };
    let Expr::List(items) = form else {
        return Ok(None);
    };
    let Some(Expr::Symbol(name_id)) = items.get(1) else {
        return Ok(None);
    };
    if let Some(expanded) =
        expand_compiler_toplevel_expr_with_env(eval, form_value, macroexpand_env)
    {
        if let Some(compiled) =
            compile_macroexpanded_defalias_expr_with_env(eval, &expanded, macroexpand_env)
        {
            if compiled_defalias_name_id(compiled) == Some(*name_id) {
                return Ok(Some(maybe_wrap_compiled_defalias_with_compiler_macro(
                    *name_id,
                    compiled,
                    metadata.compiler_macro.as_ref(),
                )));
            }
        }
    }

    Ok(compile_toplevel_defun_direct_with_env(
        eval,
        items,
        macroexpand_env,
    ))
}

fn compile_toplevel_defmacro(eval: &mut Context, form: &Expr) -> Result<Option<Value>, Flow> {
    compile_toplevel_defmacro_with_env(eval, form, Value::NIL)
}

fn compile_toplevel_defmacro_with_env(
    eval: &mut Context,
    form: &Expr,
    macroexpand_env: Value,
) -> Result<Option<Value>, Flow> {
    let Expr::List(items) = form else {
        return Ok(None);
    };
    // Source `defmacro` is semantically authoritative. Nested quasiquote and
    // local binding structure in macros like `define-minor-mode` must be
    // compiled from the original body, not from a generic macroexpanded
    // top-level `defalias` shape.
    Ok(compile_toplevel_defmacro_direct_with_env(
        eval,
        items,
        macroexpand_env,
    ))
}

fn compile_toplevel_defalias(eval: &mut Context, form: &Expr) -> Result<Option<Value>, Flow> {
    compile_toplevel_defalias_with_env(eval, form, Value::NIL)
}

fn compile_toplevel_defalias_with_env(
    eval: &mut Context,
    form: &Expr,
    macroexpand_env: Value,
) -> Result<Option<Value>, Flow> {
    let form_value = quote_to_value(form);
    let Expr::List(items) = form else {
        return Ok(None);
    };
    let Some(name_id) = items.get(1).and_then(expr_quoted_symbol_id) else {
        return Ok(None);
    };

    if let Some(expanded) =
        expand_compiler_toplevel_expr_with_env(eval, form_value, macroexpand_env)
        && let Some(compiled) =
            compile_macroexpanded_defalias_expr_with_env(eval, &expanded, macroexpand_env)
        && compiled_defalias_name_id(compiled) == Some(name_id)
    {
        return Ok(Some(compiled));
    }

    Ok(compile_macroexpanded_defalias_expr_with_env(
        eval,
        form,
        macroexpand_env,
    ))
}

fn compile_toplevel_defmacro_value(eval: &mut Context, form_value: Value) -> Option<Value> {
    compile_toplevel_defmacro_value_with_env(eval, form_value, Value::NIL)
}

fn compile_toplevel_defmacro_value_with_env(
    eval: &mut Context,
    form_value: Value,
    macroexpand_env: Value,
) -> Option<Value> {
    let items = list_to_vec(&form_value)?;
    compile_toplevel_defmacro_direct_value_with_env(eval, &items, macroexpand_env)
}

fn compile_toplevel_defun_value(eval: &mut Context, form_value: Value) -> Option<Value> {
    compile_toplevel_defun_value_with_env(eval, form_value, form_value, Value::NIL)
}

fn compile_toplevel_defun_value_with_env(
    eval: &mut Context,
    form_value: Value,
    expanded_value: Value,
    macroexpand_env: Value,
) -> Option<Value> {
    let items = list_to_vec(&form_value)?;
    if items.len() < 4 {
        return None;
    }
    let name_id = items.get(1)?.as_symbol_id()?;
    let metadata = parse_lambda_metadata_from_value_body(&items[3..]);
    if let Some(compiled) =
        compile_macroexpanded_defalias_value_with_env(eval, expanded_value, macroexpand_env)
    {
        if compiled_defalias_name_id(compiled) == Some(name_id) {
            return Some(maybe_wrap_compiled_defalias_with_compiler_macro_value(
                name_id,
                compiled,
                metadata.compiler_macro,
            ));
        }
    }

    compile_toplevel_defun_direct_value_with_env(eval, &items, macroexpand_env)
}

fn compile_lambda_expanded_value(eval: &mut Context, lambda: Value) -> Option<Value> {
    compile_lambda_expanded_value_with_env(eval, lambda, Value::NIL)
}

fn compile_lambda_expanded_value_with_env(
    eval: &mut Context,
    lambda: Value,
    macroexpand_env: Value,
) -> Option<Value> {
    let compile_scope = eval.open_gc_scope();
    let result = (|| {
        eval.root(lambda);
        if !macroexpand_env.is_nil() {
            eval.root(macroexpand_env);
        }

        let items = list_to_vec(&lambda)?;
        if items.first().and_then(|value| value.as_symbol_name()) != Some("lambda") {
            return None;
        }

        let params = parse_lambda_params_from_value(items.get(1)?).ok()?;
        let metadata = parse_lambda_metadata_from_value_body(&items[2..]);
        let body = items.get(2 + metadata.body_start..)?.to_vec();
        for value in &body {
            eval.root(*value);
        }
        let body_exprs = compile_body_exprs_with_env(eval, &body, macroexpand_env);

        let mut compiler = Compiler::new(eval.lexical_binding());
        let mut bytecode = compiler.compile_lambda(&params, &body_exprs);
        bytecode.docstring = metadata.docstring;
        bytecode.doc_form = metadata.doc_form.filter(|value| !value.is_nil());
        bytecode.interactive = metadata.interactive.filter(|value| !value.is_nil());
        Some(Value::make_bytecode(bytecode))
    })();
    compile_scope.close(eval);
    result
}

fn compile_function_expanded_value(eval: &mut Context, function: Value) -> Option<Value> {
    compile_function_expanded_value_with_env(eval, function, Value::NIL)
}

fn compile_function_expanded_value_with_env(
    eval: &mut Context,
    function: Value,
    macroexpand_env: Value,
) -> Option<Value> {
    let compile_scope = eval.open_gc_scope();
    let result = (|| {
        eval.root(function);
        if !macroexpand_env.is_nil() {
            eval.root(macroexpand_env);
        }

        let items = list_to_vec(&function)?;
        if items.first().and_then(|value| value.as_symbol_name()) != Some("function") {
            return None;
        }
        compile_lambda_expanded_value_with_env(eval, *items.get(1)?, macroexpand_env)
    })();
    compile_scope.close(eval);
    result
}

fn compile_defalias_target_expanded_value(eval: &mut Context, target: Value) -> Option<Value> {
    compile_defalias_target_expanded_value_with_env(eval, target, Value::NIL)
}

fn compile_defalias_target_expanded_value_with_env(
    eval: &mut Context,
    target: Value,
    macroexpand_env: Value,
) -> Option<Value> {
    let compile_scope = eval.open_gc_scope();
    let result = (|| {
        eval.root(target);
        if !macroexpand_env.is_nil() {
            eval.root(macroexpand_env);
        }

        if let Some(compiled) =
            compile_function_expanded_value_with_env(eval, target, macroexpand_env)
        {
            return Some(compiled);
        }

        let items = list_to_vec(&target)?;
        if items.len() != 3
            || items.first().and_then(|value| value.as_symbol_name()) != Some("cons")
        {
            return None;
        }
        if !is_quoted_symbol(*items.get(1)?, "macro") {
            return None;
        }

        let compiled =
            compile_function_expanded_value_with_env(eval, *items.get(2)?, macroexpand_env)?;
        eval.root(compiled);
        Some(Value::list(vec![
            Value::symbol("cons"),
            quoted_symbol_value(intern("macro")),
            compiled,
        ]))
    })();
    compile_scope.close(eval);
    result
}

fn compile_macroexpanded_defalias_value(eval: &mut Context, expanded: Value) -> Option<Value> {
    compile_macroexpanded_defalias_value_with_env(eval, expanded, Value::NIL)
}

fn compile_macroexpanded_defalias_value_with_env(
    eval: &mut Context,
    expanded: Value,
    macroexpand_env: Value,
) -> Option<Value> {
    let compile_scope = eval.open_gc_scope();
    let result = (|| {
        eval.root(expanded);
        if !macroexpand_env.is_nil() {
            eval.root(macroexpand_env);
        }

        let items = list_to_vec(&expanded)?;
        match items.first()?.as_symbol_name()? {
            "defalias" => {
                let compiled = compile_defalias_target_expanded_value_with_env(
                    eval,
                    *items.get(2)?,
                    macroexpand_env,
                )?;
                eval.root(compiled);
                let mut rebuilt = items;
                rebuilt[2] = compiled;
                Some(Value::list(rebuilt))
            }
            "prog1" => {
                let compiled = compile_macroexpanded_defalias_value_with_env(
                    eval,
                    *items.get(1)?,
                    macroexpand_env,
                )?;
                eval.root(compiled);
                let mut rebuilt = items;
                rebuilt[1] = compiled;
                Some(Value::list(rebuilt))
            }
            _ => None,
        }
    })();
    compile_scope.close(eval);
    result
}

fn compiled_defalias_name_id(compiled_form: Value) -> Option<SymId> {
    let items = list_to_vec(&compiled_form)?;
    match items.first()?.as_symbol_name()? {
        "defalias" => quoted_symbol_id(*items.get(1)?),
        "prog1" => compiled_defalias_name_id(*items.get(1)?),
        _ => None,
    }
}

fn compiled_function_binding_from_defalias(compiled_form: Value) -> Option<(SymId, Value)> {
    let items = list_to_vec(&compiled_form)?;
    match items.first()?.as_symbol_name()? {
        "defalias" => Some((quoted_symbol_id(*items.get(1)?)?, *items.get(2)?)),
        "prog1" => compiled_function_binding_from_defalias(*items.get(1)?),
        _ => None,
    }
}

pub(crate) fn lower_runtime_cached_toplevel_form(
    eval: &mut Context,
    original: Value,
    expanded: Value,
) -> Option<Value> {
    lower_runtime_cached_toplevel_form_with_env(eval, original, expanded, Value::NIL)
}

pub(crate) fn lower_runtime_cached_toplevel_form_with_env(
    eval: &mut Context,
    original: Value,
    expanded: Value,
    macroexpand_env: Value,
) -> Option<Value> {
    let items = list_to_vec(&original)?;
    let head = items.first()?.as_symbol_name()?;
    match head {
        "defun" => compile_toplevel_defun_value_with_env(eval, original, expanded, macroexpand_env),
        "defmacro" => compile_toplevel_defmacro_value_with_env(eval, original, macroexpand_env),
        _ => compile_macroexpanded_defalias_value_with_env(eval, expanded, macroexpand_env),
    }
}

fn quoted_symbol_id(value: Value) -> Option<SymId> {
    if let Some(id) = value.as_symbol_id() {
        return Some(id);
    }
    let items = list_to_vec(&value)?;
    match items.as_slice() {
        [head, quoted] if head.as_symbol_name() == Some("quote") => quoted.as_symbol_id(),
        _ => None,
    }
}

fn lowered_macro_expander(target_form: Value) -> Option<Value> {
    if target_form.is_macro() {
        return Some(target_form);
    }
    if target_form.is_cons() && target_form.cons_car().is_symbol_named("macro") {
        return Some(target_form.cons_cdr());
    }

    let items = list_to_vec(&target_form)?;
    match items.as_slice() {
        [head, tag, callable]
            if head.as_symbol_name() == Some("cons") && is_quoted_symbol(*tag, "macro") =>
        {
            Some(*callable)
        }
        _ => None,
    }
}

pub(crate) fn compiled_macro_binding_from_defalias(compiled_form: Value) -> Option<(SymId, Value)> {
    let items = list_to_vec(&compiled_form)?;
    match items.first()?.as_symbol_name()? {
        "defalias" => {
            let name_id = quoted_symbol_id(*items.get(1)?)?;
            let expander = lowered_macro_expander(*items.get(2)?)?;
            Some((name_id, expander))
        }
        "prog1" => compiled_macro_binding_from_defalias(*items.get(1)?),
        _ => None,
    }
}

fn extend_compiler_macro_env(macro_env: &mut Value, name_id: SymId, expander: Value) {
    let entry = Value::cons(Value::symbol(resolve_sym(name_id)), expander);
    *macro_env = Value::cons(entry, *macro_env);
}

pub(crate) fn maybe_extend_compiler_macro_env_from_lowered(
    macro_env: &mut Value,
    lowered_form: Value,
) {
    if let Some((name_id, definition)) = compiled_macro_binding_from_defalias(lowered_form) {
        extend_compiler_macro_env(macro_env, name_id, definition);
    }
}

fn extend_compiler_function_overrides(function_env: &mut Value, name_id: SymId, definition: Value) {
    let entry = Value::cons(Value::symbol(resolve_sym(name_id)), definition);
    *function_env = Value::cons(entry, *function_env);
}

fn maybe_extend_compiler_function_overrides_from_lowered(
    function_env: &mut Value,
    lowered_form: Value,
) {
    if let Some((name_id, definition)) = compiled_function_binding_from_defalias(lowered_form) {
        extend_compiler_function_overrides(function_env, name_id, definition);
    }
}

fn install_lowered_compile_time_form(
    eval: &mut Context,
    out: &[CompiledForm],
    compiler_macro_env: Value,
    compiler_function_overrides: Value,
    lowered_form: Value,
) -> Result<(), Flow> {
    let prior_roots = compiled_form_roots(out);
    eval.with_gc_scope_result(|ctx| {
        root_saved_values(ctx, &prior_roots);
        ctx.root(compiler_macro_env);
        ctx.root(compiler_function_overrides);
        ctx.root(lowered_form);
        ctx.eval_sub(lowered_form)?;
        Ok(Value::NIL)
    })?;
    Ok(())
}

/// Process a single top-level form, appending results to `out`.
fn compile_toplevel_file_form(
    eval: &mut Context,
    form: &Expr,
    out: &mut Vec<CompiledForm>,
    compiler_macro_env: &mut Value,
    compiler_function_overrides: &mut Value,
    deferred_defmacros: &mut Vec<DeferredDefmacro>,
) -> Result<(), Flow> {
    match form {
        Expr::List(items) if !items.is_empty() => {
            if let Expr::Symbol(id) = &items[0] {
                let name = resolve_sym(*id);
                match name {
                    "progn" => {
                        // Flatten: recurse into each sub-form.
                        for sub in &items[1..] {
                            let prior_roots = compiled_form_roots(out);
                            eval.with_gc_scope_result(|ctx| {
                                root_saved_values(ctx, &prior_roots);
                                ctx.root(*compiler_macro_env);
                                compile_toplevel_file_form(
                                    ctx,
                                    sub,
                                    out,
                                    compiler_macro_env,
                                    compiler_function_overrides,
                                    deferred_defmacros,
                                )
                            })?;
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
                        if let Some(compiled_form) =
                            compile_toplevel_defun_with_env(eval, form, *compiler_macro_env)?
                        {
                            install_lowered_compile_time_form(
                                eval,
                                out,
                                *compiler_macro_env,
                                *compiler_function_overrides,
                                compiled_form,
                            )?;
                            maybe_extend_compiler_function_overrides_from_lowered(
                                compiler_function_overrides,
                                compiled_form,
                            );
                            out.push(CompiledForm::Eval(compiled_form));
                            return Ok(());
                        }
                    }
                    "defmacro" => {
                        let Some(Expr::Symbol(name_id)) = items.get(1) else {
                            return compile_replayable_toplevel_form(eval, form, out);
                        };
                        let form_value = quote_to_value(form);
                        let prior_roots = compiled_form_roots(out);
                        eval.with_gc_scope_result(|ctx| {
                            root_saved_values(ctx, &prior_roots);
                            ctx.root(*compiler_macro_env);
                            ctx.root(form_value);
                            ctx.eval(form)
                        })?;
                        if let Some(definition) = eval
                            .obarray()
                            .symbol_function(resolve_sym(*name_id))
                            .copied()
                            && let Some(expander) = lowered_macro_expander(definition)
                        {
                            extend_compiler_macro_env(compiler_macro_env, *name_id, expander);
                        }
                        let index = out.len();
                        out.push(CompiledForm::EagerEval(form_value));
                        deferred_defmacros.push(DeferredDefmacro {
                            index,
                            form: form.clone(),
                        });
                        return Ok(());
                    }
                    "defalias" => {
                        if let Some(compiled_form) =
                            compile_toplevel_defalias_with_env(eval, form, *compiler_macro_env)?
                        {
                            install_lowered_compile_time_form(
                                eval,
                                out,
                                *compiler_macro_env,
                                *compiler_function_overrides,
                                compiled_form,
                            )?;
                            maybe_extend_compiler_macro_env_from_lowered(
                                compiler_macro_env,
                                compiled_form,
                            );
                            maybe_extend_compiler_function_overrides_from_lowered(
                                compiler_function_overrides,
                                compiled_form,
                            );
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

#[cfg(test)]
pub(crate) fn lower_toplevel_compiled_form_for_test(
    eval: &mut Context,
    form: &Expr,
) -> Result<Option<Value>, Flow> {
    match form {
        Expr::List(items) if !items.is_empty() => {
            if let Expr::Symbol(id) = &items[0] {
                match resolve_sym(*id) {
                    "defun" => return compile_toplevel_defun(eval, form),
                    "defmacro" => return compile_toplevel_defmacro(eval, form),
                    "defalias" => return compile_toplevel_defalias(eval, form),
                    _ => {}
                }
            }
        }
        _ => {}
    }
    Ok(None)
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
    compile_el_to_neobc_with_output_and_surface(
        eval,
        el_path,
        &el_path.with_extension("neobc"),
        None,
    )
}

pub(crate) fn compile_el_to_neobc_with_output_and_surface(
    eval: &mut Context,
    el_path: &Path,
    neobc_path: &Path,
    surface_fingerprint: Option<&str>,
) -> Result<(), CompileFileError> {
    ensure_compiler_support_loaded(eval, el_path)?;

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

    // 8. Write .neobc to the requested output path.
    let bytes = super::file_compile_format::serialize_neobc_with_surface_detailed(
        &source_hash,
        lexical,
        &compiled,
        surface_fingerprint,
    )
    .map_err(|err| CompileFileError::Serialize(format!("{}: {}", err.path(), err.detail())))?;
    std::fs::write(neobc_path, bytes).map_err(CompileFileError::Io)?;

    Ok(())
}

#[cfg(test)]
#[path = "file_compile_test.rs"]
mod tests;
