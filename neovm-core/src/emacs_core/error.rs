//! Error and signal types for the evaluator.

use std::error::Error;
use std::fmt::{self, Display, Formatter};

use super::intern::{SymId, intern, resolve_sym};
use super::print::PrintOptions;
use super::value::{Value, read_cons, with_heap};
use crate::window::WindowId;

/// Public-facing evaluation error.
#[derive(Clone, Debug)]
pub enum EvalError {
    Signal { symbol: SymId, data: Vec<Value> },
    UncaughtThrow { tag: Value, value: Value },
}

impl Display for EvalError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Signal { symbol, data } => {
                write!(
                    f,
                    "signal {} {}",
                    resolve_sym(*symbol),
                    super::print::print_value(&Value::list(data.clone()))
                )
            }
            Self::UncaughtThrow { tag, value } => write!(
                f,
                "uncaught throw tag={} value={}",
                super::print::print_value(tag),
                super::print::print_value(value),
            ),
        }
    }
}

impl Error for EvalError {}

/// Internal non-local control flow.
#[derive(Clone, Debug)]
pub(crate) enum Flow {
    Signal(SignalData),
    Throw { tag: Value, value: Value },
}

#[derive(Clone, Debug)]
pub(crate) struct SignalData {
    pub symbol: SymId,
    pub data: Vec<Value>,
    /// Original cdr payload when a signal uses non-list data.
    pub raw_data: Option<Value>,
}

impl SignalData {
    /// Resolve the signal symbol name via the interner.
    pub fn symbol_name(&self) -> &str {
        resolve_sym(self.symbol)
    }
}

pub(crate) type EvalResult = Result<Value, Flow>;

/// Create a signal flow.
pub(crate) fn signal(symbol: &str, data: Vec<Value>) -> Flow {
    Flow::Signal(SignalData {
        symbol: intern(symbol),
        data,
        raw_data: None,
    })
}

/// Create a signal where DATA is used as the raw cdr payload.
///
/// This preserves dotted signal data shapes such as `(foo . 1)`.
pub(crate) fn signal_with_data(symbol: &str, data: Value) -> Flow {
    let normalized = super::value::list_to_vec(&data).unwrap_or_else(|| vec![data]);
    Flow::Signal(SignalData {
        symbol: intern(symbol),
        data: normalized,
        raw_data: Some(data),
    })
}

/// Convert internal flow to public EvalError.
pub(crate) fn map_flow(flow: Flow) -> EvalError {
    match flow {
        Flow::Signal(sig) => EvalError::Signal {
            symbol: sig.symbol,
            data: sig.data,
        },
        Flow::Throw { tag, value } => EvalError::UncaughtThrow { tag, value },
    }
}

/// Check if a condition-case pattern matches a signal symbol.
pub(crate) fn signal_matches(pattern: &super::expr::Expr, symbol: &str) -> bool {
    use super::expr::Expr;
    match pattern {
        Expr::Symbol(id) => {
            let name = resolve_sym(*id);
            name == symbol || name == "error" || name == "t"
        }
        Expr::List(items) => items.iter().any(|item| signal_matches(item, symbol)),
        _ => false,
    }
}

/// Build the binding value for condition-case variable: (symbol . data)
pub(crate) fn make_signal_binding_value(sig: &SignalData) -> Value {
    if let Some(raw) = &sig.raw_data {
        return Value::cons(Value::Symbol(sig.symbol), *raw);
    }
    let mut values = Vec::with_capacity(sig.data.len() + 1);
    values.push(Value::Symbol(sig.symbol));
    values.extend(sig.data.clone());
    Value::list(values)
}

/// Format an eval result for the compat test harness (TSV output).
pub fn format_eval_result(result: &Result<Value, EvalError>) -> String {
    match result {
        Ok(value) => format!("OK {}", super::print::print_value(value)),
        Err(EvalError::Signal { symbol, data }) => {
            let payload = if data.is_empty() {
                "nil".to_string()
            } else {
                super::print::print_value(&Value::list(data.clone()))
            };
            format!("ERR ({} {})", resolve_sym(*symbol), payload)
        }
        Err(EvalError::UncaughtThrow { tag, value }) => {
            format!(
                "ERR (no-catch ({} {}))",
                super::print::print_value(tag),
                super::print::print_value(value),
            )
        }
    }
}

fn format_opaque_handle_in_state(
    buffers: &crate::buffer::BufferManager,
    frames: &crate::window::FrameManager,
    threads: &super::threads::ThreadManager,
    value: &Value,
) -> Option<String> {
    if let Some(handle) = super::terminal::pure::print_terminal_handle(value) {
        return Some(handle);
    }
    if let Value::Window(id) = value {
        return Some(format_window_handle_in_state(buffers, frames, *id));
    }
    if let Some(id) = threads.thread_id_from_handle(value) {
        return Some(format!("#<thread {id}>"));
    }
    if let Some(id) = threads.mutex_id_from_handle(value) {
        return Some(format!("#<mutex {id}>"));
    }
    if let Some(id) = threads.condition_variable_id_from_handle(value) {
        return Some(format!("#<condvar {id}>"));
    }
    if let Value::Buffer(id) = value {
        if let Some(buf) = buffers.get(*id) {
            return Some(format!("#<buffer {}>", buf.name));
        }
        if buffers.dead_buffer_last_name(*id).is_some() {
            return Some("#<killed buffer>".to_string());
        }
    }
    None
}

fn format_window_handle_in_state(
    buffers: &crate::buffer::BufferManager,
    frames: &crate::window::FrameManager,
    id: u64,
) -> String {
    let window_id = WindowId(id);
    if let Some(frame_id) = frames.find_window_frame_id(window_id) {
        if let Some(frame) = frames.get(frame_id) {
            if let Some(window) = frame.find_window(window_id) {
                if let Some(buffer_id) = window.buffer_id() {
                    if let Some(buffer) = buffers.get(buffer_id) {
                        return format!("#<window {id} on {}>", buffer.name);
                    }
                }
                return format!("#<window {id} on {}>", frame.name);
            }
        }
    }
    format!("#<window {id}>")
}

fn print_options_from_state(obarray: &super::symbol::Obarray) -> PrintOptions {
    let print_gensym = obarray
        .symbol_value("print-gensym")
        .is_some_and(Value::is_truthy);
    let print_circle = obarray
        .symbol_value("print-circle")
        .is_some_and(Value::is_truthy);
    let print_level = match obarray.symbol_value("print-level") {
        Some(Value::Int(n)) if *n >= 0 => Some(*n),
        _ => None,
    };
    let print_length = match obarray.symbol_value("print-length") {
        Some(Value::Int(n)) if *n >= 0 => Some(*n),
        _ => None,
    };
    PrintOptions::new(print_gensym, print_circle, print_level, print_length)
}

pub(crate) fn print_value_in_state(
    obarray: &super::symbol::Obarray,
    buffers: &crate::buffer::BufferManager,
    frames: &crate::window::FrameManager,
    threads: &super::threads::ThreadManager,
    value: &Value,
) -> String {
    format_value_in_state(
        obarray,
        buffers,
        frames,
        threads,
        value,
        print_options_from_state(obarray),
    )
}

fn format_value_in_state(
    obarray: &super::symbol::Obarray,
    buffers: &crate::buffer::BufferManager,
    frames: &crate::window::FrameManager,
    threads: &super::threads::ThreadManager,
    value: &Value,
    options: PrintOptions,
) -> String {
    if let Some(handle) = format_opaque_handle_in_state(buffers, frames, threads, value) {
        return handle;
    }
    // Use the stateful printer when print-circle, print-level, or print-length
    // are active. This ensures correct handling of shared structure, depth
    // limiting, and length limiting throughout the entire value tree.
    if options.print_circle || options.print_level.is_some() || options.print_length.is_some() {
        return super::print::print_value_stateful(value, options);
    }
    match value {
        super::value::Value::Cons(_) | super::value::Value::Vector(_) => {
            format_value_in_state_slow(obarray, buffers, frames, threads, value, options)
        }
        _ => super::print::print_value_with_options(value, options),
    }
}

fn format_value_in_state_slow(
    obarray: &super::symbol::Obarray,
    buffers: &crate::buffer::BufferManager,
    frames: &crate::window::FrameManager,
    threads: &super::threads::ThreadManager,
    value: &Value,
    options: PrintOptions,
) -> String {
    match value {
        Value::Cons(_) => {
            if let Some(shorthand) =
                format_list_shorthand_in_state(obarray, buffers, frames, threads, value, options)
            {
                return shorthand;
            }
            let mut out = String::from("(");
            format_cons_in_state(obarray, buffers, frames, threads, value, &mut out, options);
            out.push(')');
            out
        }
        Value::Vector(vec) => {
            let mut out = String::from("[");
            let items = with_heap(|h| h.get_vector(*vec).clone());
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push(' ');
                }
                out.push_str(&format_value_in_state(
                    obarray, buffers, frames, threads, item, options,
                ));
            }
            out.push(']');
            out
        }
        _ => super::print::print_value_with_options(value, options),
    }
}

fn format_list_shorthand_in_state(
    obarray: &super::symbol::Obarray,
    buffers: &crate::buffer::BufferManager,
    frames: &crate::window::FrameManager,
    threads: &super::threads::ThreadManager,
    value: &Value,
    options: PrintOptions,
) -> Option<String> {
    let items = super::value::list_to_vec(value)?;
    if items.len() != 2 {
        return None;
    }

    let head = match &items[0] {
        Value::Symbol(id) => resolve_sym(*id),
        _ => return None,
    };

    if head == "make-hash-table-from-literal" {
        let payload = quote_payload(&items[1])?;
        return Some(format!(
            "#s{}",
            format_value_in_state(obarray, buffers, frames, threads, &payload, options)
        ));
    }

    let (prefix, quoted, nested_options) = match head {
        "quote" => Some(("'", &items[1], options)),
        "function" => Some(("#'", &items[1], options)),
        "`" => Some(("`", &items[1], options.enter_backquote())),
        "," => {
            options
                .allow_unquote_shorthand()
                .then_some((",", &items[1], options.exit_backquote()))
        }
        ",@" => {
            options
                .allow_unquote_shorthand()
                .then_some((",@", &items[1], options.exit_backquote()))
        }
        _ => None,
    }?;

    Some(format!(
        "{prefix}{}",
        format_value_in_state(obarray, buffers, frames, threads, quoted, nested_options)
    ))
}

fn format_cons_in_state(
    obarray: &super::symbol::Obarray,
    buffers: &crate::buffer::BufferManager,
    frames: &crate::window::FrameManager,
    threads: &super::threads::ThreadManager,
    value: &Value,
    out: &mut String,
    options: PrintOptions,
) {
    let mut cursor = *value;
    let mut first = true;
    loop {
        match cursor {
            Value::Cons(cell) => {
                if !first {
                    out.push(' ');
                }
                let pair = read_cons(cell);
                out.push_str(&format_value_in_state(
                    obarray, buffers, frames, threads, &pair.car, options,
                ));
                cursor = pair.cdr;
                first = false;
            }
            Value::Nil => return,
            other => {
                if !first {
                    out.push_str(" . ");
                }
                out.push_str(&format_value_in_state(
                    obarray, buffers, frames, threads, &other, options,
                ));
                return;
            }
        }
    }
}

pub(crate) fn print_value_bytes_in_state(
    obarray: &super::symbol::Obarray,
    buffers: &crate::buffer::BufferManager,
    frames: &crate::window::FrameManager,
    threads: &super::threads::ThreadManager,
    value: &Value,
) -> Vec<u8> {
    if let Some(handle) = format_opaque_handle_in_state(buffers, frames, threads, value) {
        return handle.into_bytes();
    }
    format_value_bytes_in_state_with_options(
        obarray,
        buffers,
        frames,
        threads,
        value,
        print_options_from_state(obarray),
    )
}

fn format_value_bytes_in_state_with_options(
    obarray: &super::symbol::Obarray,
    buffers: &crate::buffer::BufferManager,
    frames: &crate::window::FrameManager,
    threads: &super::threads::ThreadManager,
    value: &Value,
    options: PrintOptions,
) -> Vec<u8> {
    if let Some(handle) = format_opaque_handle_in_state(buffers, frames, threads, value) {
        return handle.into_bytes();
    }
    // Use the stateful printer when print-circle, print-level, or print-length
    // are active, then convert the result to bytes.
    if options.print_circle || options.print_level.is_some() || options.print_length.is_some() {
        return super::print::print_value_stateful(value, options).into_bytes();
    }
    match value {
        Value::Cons(_) => {
            format_cons_bytes_in_state(obarray, buffers, frames, threads, value, options)
        }
        Value::Vector(_) => {
            format_vector_bytes_in_state(obarray, buffers, frames, threads, value, options)
        }
        _ => super::print::print_value_bytes_with_options(value, options),
    }
}

fn format_cons_bytes_in_state(
    obarray: &super::symbol::Obarray,
    buffers: &crate::buffer::BufferManager,
    frames: &crate::window::FrameManager,
    threads: &super::threads::ThreadManager,
    value: &Value,
    options: PrintOptions,
) -> Vec<u8> {
    if let Some(shorthand) =
        format_list_shorthand_bytes_in_state(obarray, buffers, frames, threads, value, options)
    {
        return shorthand;
    }
    let mut out = Vec::new();
    out.push(b'(');
    append_cons_bytes_in_state(obarray, buffers, frames, threads, value, &mut out, options);
    out.push(b')');
    out
}

fn format_vector_bytes_in_state(
    obarray: &super::symbol::Obarray,
    buffers: &crate::buffer::BufferManager,
    frames: &crate::window::FrameManager,
    threads: &super::threads::ThreadManager,
    value: &Value,
    options: PrintOptions,
) -> Vec<u8> {
    if super::chartable::bool_vector_length(value).is_some()
        || super::chartable::char_table_external_slots(value).is_some()
    {
        return super::print::print_value_bytes_with_options(value, options);
    }
    let mut out = Vec::new();
    out.push(b'[');
    let Value::Vector(items) = value else {
        return out;
    };
    let values = with_heap(|h| h.get_vector(*items).clone());
    for (idx, item) in values.iter().enumerate() {
        if idx > 0 {
            out.push(b' ');
        }
        out.extend(format_value_bytes_in_state_with_options(
            obarray, buffers, frames, threads, item, options,
        ));
    }
    out.push(b']');
    out
}

fn format_list_shorthand_bytes_in_state(
    obarray: &super::symbol::Obarray,
    buffers: &crate::buffer::BufferManager,
    frames: &crate::window::FrameManager,
    threads: &super::threads::ThreadManager,
    value: &Value,
    options: PrintOptions,
) -> Option<Vec<u8>> {
    let items = super::value::list_to_vec(value)?;
    if items.len() != 2 {
        return None;
    }

    let head = match &items[0] {
        Value::Symbol(id) => resolve_sym(*id),
        _ => return None,
    };

    if head == "make-hash-table-from-literal" {
        let payload = quote_payload(&items[1])?;
        let mut out = Vec::new();
        out.extend_from_slice(b"#s");
        out.extend(format_value_bytes_in_state_with_options(
            obarray, buffers, frames, threads, &payload, options,
        ));
        return Some(out);
    }

    let (prefix, quoted, nested_options) = match head {
        "quote" => Some((b"'" as &[u8], &items[1], options)),
        "function" => Some((b"#'" as &[u8], &items[1], options)),
        "`" => Some((b"`" as &[u8], &items[1], options.enter_backquote())),
        "," => options.allow_unquote_shorthand().then_some((
            b"," as &[u8],
            &items[1],
            options.exit_backquote(),
        )),
        ",@" => options.allow_unquote_shorthand().then_some((
            b",@" as &[u8],
            &items[1],
            options.exit_backquote(),
        )),
        _ => None,
    }?;

    let mut out = Vec::new();
    out.extend_from_slice(prefix);
    out.extend(format_value_bytes_in_state_with_options(
        obarray,
        buffers,
        frames,
        threads,
        quoted,
        nested_options,
    ));
    Some(out)
}

fn quote_payload(value: &Value) -> Option<Value> {
    let items = super::value::list_to_vec(value)?;
    if items.len() != 2 {
        return None;
    }
    match &items[0] {
        Value::Symbol(id) if resolve_sym(*id) == "quote" => Some(items[1]),
        _ => None,
    }
}

fn append_cons_bytes_in_state(
    obarray: &super::symbol::Obarray,
    buffers: &crate::buffer::BufferManager,
    frames: &crate::window::FrameManager,
    threads: &super::threads::ThreadManager,
    value: &Value,
    out: &mut Vec<u8>,
    options: PrintOptions,
) {
    let mut cursor = *value;
    let mut first = true;
    loop {
        match cursor {
            Value::Cons(cell) => {
                if !first {
                    out.push(b' ');
                }
                let pair = read_cons(cell);
                out.extend(format_value_bytes_in_state_with_options(
                    obarray, buffers, frames, threads, &pair.car, options,
                ));
                cursor = pair.cdr;
                first = false;
            }
            Value::Nil => return,
            other => {
                if !first {
                    out.extend_from_slice(b" . ");
                }
                out.extend(format_value_bytes_in_state_with_options(
                    obarray, buffers, frames, threads, &other, options,
                ));
                return;
            }
        }
    }
}

/// Render a value with evaluator-context-aware opaque handle formatting.
pub fn print_value_with_eval(eval: &super::eval::Evaluator, value: &Value) -> String {
    print_value_in_state(
        &eval.obarray,
        &eval.buffers,
        &eval.frames,
        &eval.threads,
        value,
    )
}

/// Render a value as bytes with evaluator-context-aware opaque handle formatting.
pub fn print_value_bytes_with_eval(eval: &super::eval::Evaluator, value: &Value) -> Vec<u8> {
    print_value_bytes_in_state(
        &eval.obarray,
        &eval.buffers,
        &eval.frames,
        &eval.threads,
        value,
    )
}

fn print_data_payload_with_eval(eval: &super::eval::Evaluator, data: &[Value]) -> String {
    if data.is_empty() {
        "nil".to_string()
    } else {
        let parts = data
            .iter()
            .map(|v| print_value_with_eval(eval, v))
            .collect::<Vec<_>>();
        format!("({})", parts.join(" "))
    }
}

fn append_print_value_bytes_with_eval(
    eval: &super::eval::Evaluator,
    value: &Value,
    out: &mut Vec<u8>,
) {
    out.extend_from_slice(&print_value_bytes_with_eval(eval, value));
}

/// Format an eval result for harnesses that have evaluator context and need
/// opaque handle rendering for thread/mutex/condvar/terminal values.
pub fn format_eval_result_with_eval(
    eval: &super::eval::Evaluator,
    result: &Result<Value, EvalError>,
) -> String {
    match result {
        Ok(value) => format!("OK {}", print_value_with_eval(eval, value)),
        Err(EvalError::Signal { symbol, data }) => {
            let payload = print_data_payload_with_eval(eval, data);
            format!("ERR ({} {})", resolve_sym(*symbol), payload)
        }
        Err(EvalError::UncaughtThrow { tag, value }) => {
            format!(
                "ERR (no-catch ({} {}))",
                print_value_with_eval(eval, tag),
                print_value_with_eval(eval, value),
            )
        }
    }
}

/// Byte-preserving variant of `format_eval_result_with_eval`.
///
/// This preserves non-UTF-8 byte payloads in printed string literals used by
/// vm-compat corpus checks while still applying evaluator-aware opaque-handle
/// rendering for thread/mutex/condvar/terminal values.
pub fn format_eval_result_bytes_with_eval(
    eval: &super::eval::Evaluator,
    result: &Result<Value, EvalError>,
) -> Vec<u8> {
    let mut out = Vec::new();
    match result {
        Ok(value) => {
            out.extend_from_slice(b"OK ");
            append_print_value_bytes_with_eval(eval, value, &mut out);
        }
        Err(EvalError::Signal { symbol, data }) => {
            out.extend_from_slice(b"ERR (");
            out.extend_from_slice(resolve_sym(*symbol).as_bytes());
            out.push(b' ');
            if data.is_empty() {
                out.extend_from_slice(b"nil");
            } else {
                out.push(b'(');
                for (idx, item) in data.iter().enumerate() {
                    if idx > 0 {
                        out.push(b' ');
                    }
                    append_print_value_bytes_with_eval(eval, item, &mut out);
                }
                out.push(b')');
            }
            out.push(b')');
        }
        Err(EvalError::UncaughtThrow { tag, value }) => {
            out.extend_from_slice(b"ERR (no-catch (");
            append_print_value_bytes_with_eval(eval, tag, &mut out);
            out.push(b' ');
            append_print_value_bytes_with_eval(eval, value, &mut out);
            out.extend_from_slice(b"))");
        }
    }
    out
}
#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
