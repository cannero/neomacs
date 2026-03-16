//! Elisp interpreter module.
//!
//! Provides a full Elisp evaluator with:
//! - Value types: nil, t, int, float, string, symbol, keyword, char, cons, vector, hash-table
//! - Complete parser: strings, floats, chars, vectors, dotted pairs, quasiquote, reader macros
//! - Special forms: quote, function, let, let*, setq, if, and, or, cond, while, progn, prog1,
//!   lambda, defun, defvar, defconst, defmacro, funcall, catch, throw, unwind-protect,
//!   condition-case, when, unless
//! - 100+ built-in functions: arithmetic, comparisons, type predicates, list ops, string ops,
//!   vector ops, hash tables, higher-order functions, conversion, property lists

pub mod abbrev;
pub mod advice;
pub mod autoload;
pub mod bookmark;
pub mod buffer;
pub mod buffer_vars;
pub(crate) mod builtin_registry;
pub mod builtins;
pub mod builtins_extra;
pub mod bytecode;
pub mod callproc;
pub mod casefiddle;
pub mod casetab;
pub mod category;
pub mod ccl;
pub mod character;
pub mod charset;
pub mod chartable;
pub mod cl_lib;
pub mod coding;
pub mod comp;
#[cfg(test)]
pub mod compat_regressions;
pub mod composite;
pub mod custom;
pub mod data;
pub mod dbus;
pub mod debug;
pub mod dired;
pub mod display;
pub mod dispnew;
pub mod doc;
pub mod editfns;
pub mod error;
pub mod errors;
pub mod eval;
pub mod expr;
pub(crate) mod file_compile;
pub(crate) mod file_compile_format;
pub mod fileio;
pub mod floatfns;
pub mod fns;
pub mod font;
pub mod format;
pub mod frame_vars;
pub mod hashtab;
pub mod image;
pub mod indent;
pub mod interactive;
pub mod intern;
pub mod isearch;
pub mod json;
pub mod kbd;
pub mod keyboard;
pub mod keymap;
#[cfg(test)]
mod kill_ring_test;
pub mod kmacro;
pub mod load;
pub mod lread;
pub mod marker;
pub mod minibuffer;
pub mod misc;
pub mod mode;
pub mod navigation;
pub mod network;
#[cfg(test)]
mod oracle_test;
pub mod parser;
pub mod pdump;
pub(crate) mod perf_trace;
pub mod print;
pub mod process;
pub mod reader;
pub mod rect;
pub mod regex;
pub mod register;
pub mod search;
pub(crate) mod string_escape;
pub mod subr_info;
pub mod symbol;
pub mod syntax;
pub mod terminal;
pub mod textprop;
pub mod threads;
pub mod timefns;
pub mod timer;
pub mod undo;
pub mod value;
pub mod window_cmds;
pub mod xdisp;
pub mod xfaces;
pub mod xml;

// Re-export the main public API
pub use bytecode::{ByteCodeFunction, Compiler as ByteCompiler, Vm as ByteVm};
pub use error::{
    EvalError, format_eval_result, format_eval_result_bytes_with_eval,
    format_eval_result_with_eval, print_value_bytes_with_eval, print_value_with_eval,
};
pub use eval::{DisplayHost, Evaluator, GuiFrameHostRequest};
pub use expr::{Expr, ParseError, print_expr};
pub use intern::SymId;
pub use parser::parse_forms;
pub use print::{print_value, print_value_bytes, print_value_with_buffers};
pub use symbol::Obarray;
pub use value::{LambdaData, LambdaParams, Value};

/// Convenience: parse and evaluate source code.
pub fn eval_source(input: &str) -> Result<Vec<Result<Value, EvalError>>, ParseError> {
    let forms = parse_forms(input)?;
    let mut evaluator = Evaluator::new();
    Ok(evaluator.eval_forms(&forms))
}
