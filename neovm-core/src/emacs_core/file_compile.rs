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
use super::eval::{Context, quote_to_value, value_to_expr};
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
    let mut compiled = Vec::new();
    let mut compiler_macro_env = Value::NIL;
    for form in forms {
        let compiled_roots: Vec<Value> = compiled.iter().map(CompiledForm::root_value).collect();
        eval.with_gc_scope_result(|ctx| {
            for root in &compiled_roots {
                ctx.root(*root);
            }
            ctx.root(compiler_macro_env);
            compile_toplevel_file_form(ctx, form, &mut compiled, &mut compiler_macro_env)
        })?;
    }
    Ok(compiled)
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
    let body_exprs = compile_body_exprs(eval, &body_values);

    let mut compiler = Compiler::new(eval.lexical_binding());
    let mut bytecode = compiler.compile_lambda(&params, &body_exprs);
    bytecode.docstring = metadata.docstring;
    bytecode.doc_form = metadata.doc_form.filter(|value| !value.is_nil());
    bytecode.interactive = metadata.interactive.filter(|value| !value.is_nil());
    Some(Value::make_bytecode(bytecode))
}

fn compile_function_expanded_expr(eval: &mut Context, function: &Expr) -> Option<Value> {
    let Expr::List(items) = function else {
        return None;
    };
    match items.first() {
        Some(Expr::Symbol(id)) if resolve_sym(*id) == "function" => {}
        _ => return None,
    }
    compile_lambda_expanded_expr(eval, items.get(1)?)
}

fn compile_defalias_target_expanded_expr(eval: &mut Context, target: &Expr) -> Option<Value> {
    if let Some(compiled) = compile_function_expanded_expr(eval, target) {
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

    let compiled = compile_function_expanded_expr(eval, items.get(2)?)?;
    Some(quote_to_value(&Expr::List(vec![
        Expr::Symbol(intern("cons")),
        quoted_symbol_expr(intern("macro")),
        value_to_expr(&compiled),
    ])))
}

fn compile_macroexpanded_defalias_expr(eval: &mut Context, expanded: &Expr) -> Option<Value> {
    let Expr::List(items) = expanded else {
        return None;
    };
    let head = match items.first() {
        Some(Expr::Symbol(id)) => resolve_sym(*id),
        _ => return None,
    };
    match head {
        "defalias" => {
            let compiled = compile_defalias_target_expanded_expr(eval, items.get(2)?)?;
            let mut rebuilt = items.clone();
            rebuilt[2] = value_to_expr(&compiled);
            Some(quote_to_value(&Expr::List(rebuilt)))
        }
        "prog1" => {
            let compiled = compile_macroexpanded_defalias_expr(eval, items.get(1)?)?;
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
        if let Some(compiled) = compile_macroexpanded_defalias_expr(eval, &expanded) {
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
    // Source `defmacro` is semantically authoritative. Compiling it through a
    // generic macroexpanded `defalias` shape can lose GNU macro body semantics
    // for backquote/splicing-heavy macros like `macroexp--accumulate`.
    Ok(compile_toplevel_defmacro_direct_with_env(
        eval,
        items,
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
    // Match `compile_toplevel_defmacro`: source `defmacro` should compile from
    // its original body, not from a generic expanded `defalias` wrapper.
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
    if let Some(compiled) = compile_macroexpanded_defalias_value(eval, expanded_value) {
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
    let items = list_to_vec(&lambda)?;
    if items.first().and_then(|value| value.as_symbol_name()) != Some("lambda") {
        return None;
    }

    let params = parse_lambda_params_from_value(items.get(1)?).ok()?;
    let metadata = parse_lambda_metadata_from_value_body(&items[2..]);
    let body = items.get(2 + metadata.body_start..)?;
    let body_exprs = compile_body_exprs(eval, body);

    let mut compiler = Compiler::new(eval.lexical_binding());
    let mut bytecode = compiler.compile_lambda(&params, &body_exprs);
    bytecode.docstring = metadata.docstring;
    bytecode.doc_form = metadata.doc_form.filter(|value| !value.is_nil());
    bytecode.interactive = metadata.interactive.filter(|value| !value.is_nil());
    Some(Value::make_bytecode(bytecode))
}

fn compile_function_expanded_value(eval: &mut Context, function: Value) -> Option<Value> {
    let items = list_to_vec(&function)?;
    if items.first().and_then(|value| value.as_symbol_name()) != Some("function") {
        return None;
    }
    compile_lambda_expanded_value(eval, *items.get(1)?)
}

fn compile_defalias_target_expanded_value(eval: &mut Context, target: Value) -> Option<Value> {
    if let Some(compiled) = compile_function_expanded_value(eval, target) {
        return Some(compiled);
    }

    let items = list_to_vec(&target)?;
    if items.len() != 3 || items.first().and_then(|value| value.as_symbol_name()) != Some("cons") {
        return None;
    }
    if !is_quoted_symbol(*items.get(1)?, "macro") {
        return None;
    }

    let compiled = compile_function_expanded_value(eval, *items.get(2)?)?;
    Some(Value::list(vec![
        Value::symbol("cons"),
        quoted_symbol_value(intern("macro")),
        compiled,
    ]))
}

fn compile_macroexpanded_defalias_value(eval: &mut Context, expanded: Value) -> Option<Value> {
    let items = list_to_vec(&expanded)?;
    match items.first()?.as_symbol_name()? {
        "defalias" => {
            let compiled = compile_defalias_target_expanded_value(eval, *items.get(2)?)?;
            let mut rebuilt = items;
            rebuilt[2] = compiled;
            Some(Value::list(rebuilt))
        }
        "prog1" => {
            let compiled = compile_macroexpanded_defalias_value(eval, *items.get(1)?)?;
            let mut rebuilt = items;
            rebuilt[1] = compiled;
            Some(Value::list(rebuilt))
        }
        _ => None,
    }
}

fn compiled_defalias_name_id(compiled_form: Value) -> Option<SymId> {
    let items = list_to_vec(&compiled_form)?;
    match items.first()?.as_symbol_name()? {
        "defalias" => quoted_symbol_id(*items.get(1)?),
        "prog1" => compiled_defalias_name_id(*items.get(1)?),
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
        _ => compile_macroexpanded_defalias_value(eval, expanded),
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

fn extend_compiler_macro_env(macro_env: &mut Value, name_id: SymId, definition: Value) {
    let entry = Value::cons(Value::symbol(resolve_sym(name_id)), definition);
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

/// Process a single top-level form, appending results to `out`.
fn compile_toplevel_file_form(
    eval: &mut Context,
    form: &Expr,
    out: &mut Vec<CompiledForm>,
    compiler_macro_env: &mut Value,
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
                                compile_toplevel_file_form(ctx, sub, out, compiler_macro_env)
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
                            // GNU byte compilation records top-level defuns in
                            // compiler-owned state instead of installing them
                            // into the live macroexpansion runtime before later
                            // forms are preprocessed.
                            out.push(CompiledForm::Eval(compiled_form));
                            return Ok(());
                        }
                    }
                    "defmacro" => {
                        if let Some(compiled_form) =
                            compile_toplevel_defmacro_with_env(eval, form, *compiler_macro_env)?
                        {
                            if let Some((name_id, definition)) =
                                compiled_macro_binding_from_defalias(compiled_form)
                            {
                                extend_compiler_macro_env(compiler_macro_env, name_id, definition);
                            }
                            let prior_roots = compiled_form_roots(out);
                            eval.with_gc_scope_result(|ctx| {
                                root_saved_values(ctx, &prior_roots);
                                ctx.root(*compiler_macro_env);
                                ctx.root(compiled_form);
                                ctx.eval_value(&compiled_form)
                            })?;
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
        let source = std::fs::read_to_string(project_root.join("lisp/cus-face.el"))
            .expect("read cus-face.el");
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
    fn test_compile_defmacro_runtime_executes_gensym_backquote() {
        crate::test_utils::init_test_tracing();
        let macro_src = r#"
(defmacro test-fc-gensym (x)
  (let ((tmp (make-symbol "tmp")))
    `(let ((,tmp ,x)) ,tmp)))
"#;
        let macro_forms = parse_forms(macro_src).unwrap();

        let mut compiled_eval = minimal_compile_surface_eval();
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

        let mut compiled_eval = minimal_compile_surface_eval();
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
        compile_toplevel_file_form(
            &mut direct_surface,
            &forms[0],
            &mut direct_out,
            &mut direct_env,
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
        let source_expansion = crate::emacs_core::print::print_value_with_buffers(
            &source_expansion,
            &source_eval.buffers,
        );

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
        let source_expansion = crate::emacs_core::print::print_value_with_buffers(
            &source_expansion,
            &source_eval.buffers,
        );

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
        let source_expansion = crate::emacs_core::print::print_value_with_buffers(
            &source_expansion,
            &source_eval.buffers,
        );

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
        let source_expansion = crate::emacs_core::print::print_value_with_buffers(
            &source_expansion,
            &source_eval.buffers,
        );

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
        let source_expansion = crate::emacs_core::print::print_value_with_buffers(
            &source_expansion,
            &source_eval.buffers,
        );

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
        let prefixed_render = crate::emacs_core::print::print_value_with_buffers(
            prefixed_value,
            &render_eval.buffers,
        );

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
        let expr_compiled = compile_toplevel_defmacro_direct(&mut eval, expr_items)
            .expect("expr path should compile");
        let expr_compiled = eval.root(expr_compiled);

        let runtime_value = quote_to_value(&forms[0]);
        let runtime_items = list_to_vec(&runtime_value).expect("runtime form should be a list");
        let value_compiled = compile_toplevel_defmacro_direct_value(&mut eval, &runtime_items)
            .expect("runtime-value path should compile");
        let value_compiled = eval.root(value_compiled);

        let expr_items = list_to_vec(&expr_compiled).expect("expr compiled form should be a list");
        let value_items =
            list_to_vec(&value_compiled).expect("value compiled form should be a list");
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
        let expr_compiled = compile_toplevel_defmacro_direct(&mut eval, expr_items)
            .expect("expr path should compile");
        let expr_compiled = eval.root(expr_compiled);

        let runtime_value = quote_to_value(&defface_form);
        let runtime_items = list_to_vec(&runtime_value).expect("runtime form should be a list");
        let value_compiled = compile_toplevel_defmacro_direct_value(&mut eval, &runtime_items)
            .expect("runtime-value path should compile");
        let value_compiled = eval.root(value_compiled);

        let expr_items = list_to_vec(&expr_compiled).expect("expr compiled form should be a list");
        let value_items =
            list_to_vec(&value_compiled).expect("value compiled form should be a list");
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
        let expr_compiled = compile_toplevel_defmacro_direct(&mut eval, expr_items)
            .expect("expr path should compile");
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
        let expr_compiled = compile_toplevel_defmacro_direct(&mut eval, expr_items)
            .expect("expr path should compile");
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
        let value_items =
            list_to_vec(&value_compiled).expect("value compiled form should be a list");
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
