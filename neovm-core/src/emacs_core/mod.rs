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
pub mod alloc;
pub mod autoload;
pub mod bookmark;
pub mod buffer;
pub mod buffer_vars;
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
pub mod fileio;
pub mod filelock;
pub mod floatfns;
pub mod fns;
pub mod font;
pub mod forward;
pub mod fontset;
pub mod format;
pub mod frame_vars;
pub mod hashtab;
pub(crate) mod hook_runtime;
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
pub mod pdump;
pub mod perf_trace;
pub mod print;
pub mod process;
pub mod reader;
pub mod value_reader;
pub mod rect;
pub mod regex;
pub mod regex_emacs;
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
pub use bytecode::{ByteCodeFunction, Vm as ByteVm};
pub use error::{
    EvalError, format_eval_result, format_eval_result_bytes_with_eval,
    format_eval_result_with_eval, print_value_bytes_with_eval, print_value_with_eval,
};
pub use eval::{Context, DisplayHost, GuiFrameHostRequest};
pub use intern::SymId;
pub use print::{print_value, print_value_bytes, print_value_with_buffers};
pub use symbol::Obarray;
pub use value::{LambdaData, LambdaParams, Value, ValueKind, VecLikeType};

/// Convenience: parse and evaluate source code, returning the last form's value.
pub fn eval_source(input: &str) -> Result<Value, EvalError> {
    let mut evaluator = Context::new();
    evaluator.eval_str(input)
}
