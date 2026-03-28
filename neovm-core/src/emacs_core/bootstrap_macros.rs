use super::eval::Context;
use super::intern::intern;
use super::parser::parse_forms;
use super::value::{LambdaData, LambdaParams, Value};

struct BootstrapMacroSpec {
    name: &'static str,
    params: fn() -> LambdaParams,
    body_src: &'static str,
}

fn rest_only_params(arg: &'static str) -> LambdaParams {
    LambdaParams {
        required: vec![],
        optional: vec![],
        rest: Some(intern(arg)),
    }
}

fn bootstrap_macro_specs() -> Vec<BootstrapMacroSpec> {
    vec![
        // GNU `byte-run.el` executes `eval-and-compile` in top-level forms
        // before its own `defmacro` later in the same file.  That one name
        // needs a temporary source-bootstrap macro cell when loading GNU Lisp
        // from source instead of precompiled early Lisp.
        BootstrapMacroSpec {
            name: "eval-and-compile",
            params: || rest_only_params("body"),
            body_src: "(list 'quote
                           (eval (cons 'progn body)
                                 (if lexical-binding
                                     (or macroexp--dynvars t)
                                   nil)))",
        },
    ]
}

fn build_bootstrap_macro(spec: &BootstrapMacroSpec) -> Value {
    let body = parse_forms(spec.body_src)
        .unwrap_or_else(|err| panic!("bootstrap macro {} parse failed: {err}", spec.name));
    Value::make_macro(LambdaData {
        params: (spec.params)(),
        body: body.into(),
        env: None,
        docstring: None,
        doc_form: None,
        interactive: None,
    })
}

pub(crate) fn install_bootstrap_macro_function_cells(ctx: &mut Context) {
    for spec in bootstrap_macro_specs() {
        let sym_id = intern(spec.name);
        ctx.obarray.intern(spec.name);
        if ctx.obarray.symbol_function_id(sym_id).is_none() {
            ctx.obarray
                .set_symbol_function_id(sym_id, build_bootstrap_macro(&spec));
        }
    }
}
