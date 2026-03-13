//! Value printing (Lisp representation).

use std::cell::RefCell;

use super::chartable::{bool_vector_length, char_table_external_slots};
use super::expr::{self, Expr};
use super::intern::{lookup_interned, resolve_sym};
use super::string_escape::{format_lisp_string, format_lisp_string_bytes};
use super::value::{
    HashTableTest, StringTextPropertyRun, Value, get_string_text_properties, list_to_vec,
    read_cons, with_heap,
};
use crate::gc::types::ObjId;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PrintOptions {
    pub print_gensym: bool,
    backquote_output_level: usize,
}

impl PrintOptions {
    pub const fn with_print_gensym(print_gensym: bool) -> Self {
        Self {
            print_gensym,
            backquote_output_level: 0,
        }
    }

    pub(crate) fn enter_backquote(self) -> Self {
        Self {
            backquote_output_level: self.backquote_output_level + 1,
            ..self
        }
    }

    pub(crate) fn exit_backquote(self) -> Self {
        Self {
            backquote_output_level: self.backquote_output_level.saturating_sub(1),
            ..self
        }
    }

    pub(crate) fn allow_unquote_shorthand(self) -> bool {
        self.backquote_output_level > 0
    }
}

thread_local! {
    static PRINT_OBJECT_STACK: RefCell<Vec<PrintObjectRef>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrintObjectRef {
    Lambda(ObjId),
}

fn with_print_object_guard<R>(
    object: PrintObjectRef,
    on_cycle: impl FnOnce(usize) -> R,
    render: impl FnOnce() -> R,
) -> R {
    PRINT_OBJECT_STACK.with(|stack| {
        if let Some(index) = stack.borrow().iter().position(|entry| *entry == object) {
            return on_cycle(index);
        }

        stack.borrow_mut().push(object);
        let rendered = render();
        stack.borrow_mut().pop();
        rendered
    })
}

fn format_marker_handle(
    value: &Value,
    buffers: Option<&crate::buffer::BufferManager>,
) -> Option<String> {
    if !super::marker::is_marker(value) {
        return None;
    }

    let Value::Vector(vector) = value else {
        return None;
    };
    let items = with_heap(|h| h.get_vector(*vector).clone());
    let buffer_name = match items.get(1) {
        Some(Value::Str(id)) => Some(with_heap(|h| h.get_string(*id).to_owned())),
        _ => None,
    };
    let position = match items.get(2) {
        Some(Value::Int(pos)) => Some(*pos),
        _ => None,
    };
    let insertion_type = items.get(3).is_some_and(Value::is_truthy);

    let live_buffer = buffer_name
        .as_deref()
        .is_some_and(|name| buffers.is_none_or(|bufs| bufs.find_buffer_by_name(name).is_some()));

    let mut out = String::from("#<marker ");
    if insertion_type {
        out.push_str("(moves after insertion) ");
    }
    if live_buffer {
        if let (Some(pos), Some(name)) = (position, buffer_name.as_deref()) {
            out.push_str(&format!("at {pos} in {name}"));
        } else {
            out.push_str("in no buffer");
        }
    } else {
        out.push_str("in no buffer");
    }
    out.push('>');
    Some(out)
}

fn print_special_handle(
    value: &Value,
    buffers: Option<&crate::buffer::BufferManager>,
) -> Option<String> {
    super::terminal::pure::print_terminal_handle(value)
        .or_else(|| format_marker_handle(value, buffers))
}

fn format_frame_handle(id: u64) -> String {
    if id >= crate::window::FRAME_ID_BASE {
        let ordinal = id - crate::window::FRAME_ID_BASE + 1;
        format!("#<frame F{} 0x{:x}>", ordinal, id)
    } else {
        format!("#<frame {}>", id)
    }
}

fn format_lisp_propertized_string(
    s: &str,
    runs: &[StringTextPropertyRun],
    options: PrintOptions,
) -> String {
    let mut out = String::from("#(");
    out.push_str(&format_lisp_string(s));
    for run in runs {
        out.push(' ');
        out.push_str(&run.start.to_string());
        out.push(' ');
        out.push_str(&run.end.to_string());
        out.push(' ');
        out.push_str(&print_value_with_options(&run.plist, options));
    }
    out.push(')');
    out
}

/// Print a `Value` as a Lisp string, with buffer-manager awareness for
/// proper buffer name / killed-buffer rendering.
pub fn print_value_with_buffers(value: &Value, buffers: &crate::buffer::BufferManager) -> String {
    print_value_with_buffers_and_options(value, buffers, PrintOptions::default())
}

pub fn print_value_with_buffers_and_options(
    value: &Value,
    buffers: &crate::buffer::BufferManager,
    options: PrintOptions,
) -> String {
    if let Some(handle) = print_special_handle(value, Some(buffers)) {
        return handle;
    }
    match value {
        Value::Buffer(id) => {
            if let Some(buf) = buffers.get(*id) {
                return format!("#<buffer {}>", buf.name);
            }
            if buffers.dead_buffer_last_name(*id).is_some() {
                return "#<killed buffer>".to_string();
            }
            format!("#<buffer {}>", id.0)
        }
        Value::Cons(_) => {
            // Recurse with buffer awareness
            if let Some(shorthand) = print_list_shorthand_with_buffers(value, buffers, options) {
                return shorthand;
            }
            let mut out = String::from("(");
            print_cons_with_buffers(value, &mut out, buffers, options);
            out.push(')');
            out
        }
        Value::Vector(v) => {
            if let Some(nbits) = super::chartable::bool_vector_length(value) {
                return format_bool_vector(value, nbits as usize);
            }
            if let Some(slots) = char_table_external_slots(value) {
                let parts: Vec<String> = slots
                    .iter()
                    .map(|v| print_value_with_buffers_and_options(v, buffers, options))
                    .collect();
                return format!("#^[{}]", parts.join(" "));
            }
            let items = with_heap(|h| h.get_vector(*v).clone());
            let parts: Vec<String> = items
                .iter()
                .map(|v| print_value_with_buffers_and_options(v, buffers, options))
                .collect();
            format!("[{}]", parts.join(" "))
        }
        _ => print_value_with_options(value, options),
    }
}

fn print_list_shorthand_with_buffers(
    value: &Value,
    buffers: &crate::buffer::BufferManager,
    options: PrintOptions,
) -> Option<String> {
    let items = list_to_vec(value)?;
    if items.len() != 2 {
        return None;
    }
    let head = match &items[0] {
        Value::Symbol(id) => resolve_sym(*id),
        _ => return None,
    };
    if head == "make-hash-table-from-literal" {
        if let Some(payload) = quote_payload(&items[1]) {
            return Some(format!(
                "#s{}",
                print_value_with_buffers_and_options(&payload, buffers, options)
            ));
        }
        return None;
    }
    let (prefix, nested_options) = match head {
        "quote" => ("'", options),
        "function" => ("#'", options),
        "`" => ("`", options.enter_backquote()),
        "," => {
            if !options.allow_unquote_shorthand() {
                return None;
            }
            (",", options.exit_backquote())
        }
        ",@" => {
            if !options.allow_unquote_shorthand() {
                return None;
            }
            (",@", options.exit_backquote())
        }
        _ => return None,
    };
    Some(format!(
        "{prefix}{}",
        print_value_with_buffers_and_options(&items[1], buffers, nested_options)
    ))
}

fn print_cons_with_buffers(
    value: &Value,
    out: &mut String,
    buffers: &crate::buffer::BufferManager,
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
                out.push_str(&print_value_with_buffers_and_options(
                    &pair.car, buffers, options,
                ));
                cursor = pair.cdr;
                first = false;
            }
            Value::Nil => return,
            other => {
                if !first {
                    out.push_str(" . ");
                }
                out.push_str(&print_value_with_buffers_and_options(
                    &other, buffers, options,
                ));
                return;
            }
        }
    }
}

/// Print a `Value` as a Lisp string.
pub fn print_value(value: &Value) -> String {
    print_value_with_options(value, PrintOptions::default())
}

pub fn print_value_with_options(value: &Value, options: PrintOptions) -> String {
    if let Some(handle) = print_special_handle(value, None) {
        return handle;
    }
    match value {
        Value::Nil => "nil".to_string(),
        Value::True => "t".to_string(),
        Value::Int(v) => v.to_string(),
        Value::Float(f, _) => format_float(*f),
        Value::Symbol(id) => format_symbol(*id, options),
        Value::Keyword(id) => resolve_sym(*id).to_owned(),
        Value::Str(id) => {
            let s = with_heap(|h| h.get_string(*id).to_owned());
            match get_string_text_properties(*id) {
                Some(runs) => format_lisp_propertized_string(&s, &runs, options),
                None => format_lisp_string(&s),
            }
        }
        // Emacs chars are integer values, so print as codepoint.
        Value::Char(c) => (*c as u32).to_string(),
        Value::Cons(_) => {
            if let Some(shorthand) = print_list_shorthand(value, options) {
                return shorthand;
            }
            let mut out = String::from("(");
            print_cons(value, &mut out, options);
            out.push(')');
            out
        }
        Value::Vector(v) => {
            if let Some(nbits) = bool_vector_length(value) {
                return format_bool_vector(value, nbits as usize);
            }
            if let Some(slots) = char_table_external_slots(value) {
                let parts: Vec<String> = slots
                    .iter()
                    .map(|item| print_value_with_options(item, options))
                    .collect();
                return format!("#^[{}]", parts.join(" "));
            }
            let items = with_heap(|h| h.get_vector(*v).clone());
            let parts: Vec<String> = items
                .iter()
                .map(|item| print_value_with_options(item, options))
                .collect();
            format!("[{}]", parts.join(" "))
        }
        Value::Record(v) => {
            let items = with_heap(|h| h.get_vector(*v).clone());
            let parts: Vec<String> = items
                .iter()
                .map(|item| print_value_with_options(item, options))
                .collect();
            format!("#s({})", parts.join(" "))
        }
        Value::HashTable(id) => format_hash_table(*id, options),
        Value::Lambda(id) => with_print_object_guard(
            PrintObjectRef::Lambda(*id),
            |index| format!("#{index}"),
            || {
                let lambda = value.get_lambda_data().unwrap();
                if lambda.env.is_some() {
                    return format_interpreted_closure(lambda, options);
                }
                let params = format_params(&lambda.params);
                let body = lambda
                    .body
                    .iter()
                    .map(expr::print_expr)
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("(lambda {} {})", params, body)
            },
        ),
        Value::Macro(_id) => {
            let m = value.get_lambda_data().unwrap();
            let params = format_params(&m.params);
            let body = m
                .body
                .iter()
                .map(expr::print_expr)
                .collect::<Vec<_>>()
                .join(" ");
            format!("(macro {} {})", params, body)
        }
        Value::Subr(id) => format!("#<subr {}>", resolve_sym(*id)),
        Value::ByteCode(_id) => {
            let bc = value.get_bytecode_data().unwrap();
            let params = format_params(&bc.params);
            format!("#<bytecode {} ({} ops)>", params, bc.ops.len())
        }
        Value::Buffer(id) => format!("#<buffer {}>", id.0),
        Value::Window(id) => format!("#<window {}>", id),
        Value::Frame(id) => format_frame_handle(*id),
        Value::Timer(id) => format!("#<timer {}>", id),
    }
}

/// Print a `Value` as a Lisp byte sequence.
///
/// This preserves non-UTF-8 byte payloads encoded via NeoVM string sentinels.
pub fn print_value_bytes(value: &Value) -> Vec<u8> {
    print_value_bytes_with_options(value, PrintOptions::default())
}

pub fn print_value_bytes_with_options(value: &Value, options: PrintOptions) -> Vec<u8> {
    let mut out = Vec::new();
    append_print_value_bytes(value, &mut out, options);
    out
}

fn append_print_value_bytes(value: &Value, out: &mut Vec<u8>, options: PrintOptions) {
    if let Some(handle) = print_special_handle(value, None) {
        out.extend_from_slice(handle.as_bytes());
        return;
    }
    match value {
        Value::Nil => out.extend_from_slice(b"nil"),
        Value::True => out.extend_from_slice(b"t"),
        Value::Int(v) => out.extend_from_slice(v.to_string().as_bytes()),
        Value::Float(f, _) => out.extend_from_slice(format_float(*f).as_bytes()),
        Value::Symbol(id) => append_symbol_bytes(*id, out, options),
        Value::Keyword(id) => out.extend_from_slice(resolve_sym(*id).as_bytes()),
        Value::Str(id) => {
            let s = with_heap(|h| h.get_string(*id).to_owned());
            if let Some(runs) = get_string_text_properties(*id) {
                out.extend_from_slice(b"#(");
                out.extend_from_slice(&format_lisp_string_bytes(&s));
                for run in runs {
                    out.push(b' ');
                    out.extend_from_slice(run.start.to_string().as_bytes());
                    out.push(b' ');
                    out.extend_from_slice(run.end.to_string().as_bytes());
                    out.push(b' ');
                    append_print_value_bytes(&run.plist, out, options);
                }
                out.push(b')');
            } else {
                out.extend_from_slice(&format_lisp_string_bytes(&s));
            }
        }
        Value::Char(c) => out.extend_from_slice((*c as u32).to_string().as_bytes()),
        Value::Cons(_) => {
            if let Some(shorthand) = print_list_shorthand_bytes(value, options) {
                out.extend_from_slice(&shorthand);
                return;
            }
            out.push(b'(');
            print_cons_bytes(value, out, options);
            out.push(b')');
        }
        Value::Vector(v) => {
            if let Some(nbits) = bool_vector_length(value) {
                append_bool_vector_bytes(value, nbits as usize, out);
                return;
            }
            if let Some(slots) = char_table_external_slots(value) {
                out.extend_from_slice(b"#^[");
                for (idx, item) in slots.iter().enumerate() {
                    if idx > 0 {
                        out.push(b' ');
                    }
                    append_print_value_bytes(item, out, options);
                }
                out.push(b']');
                return;
            }
            out.push(b'[');
            let items = with_heap(|h| h.get_vector(*v).clone());
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push(b' ');
                }
                append_print_value_bytes(item, out, options);
            }
            out.push(b']');
        }
        Value::Record(v) => {
            out.extend_from_slice(b"#s(");
            let items = with_heap(|h| h.get_vector(*v).clone());
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push(b' ');
                }
                append_print_value_bytes(item, out, options);
            }
            out.push(b')');
        }
        Value::HashTable(id) => {
            out.extend_from_slice(format_hash_table(*id, options).as_bytes());
        }
        Value::Lambda(id) => {
            let text = with_print_object_guard(
                PrintObjectRef::Lambda(*id),
                |index| format!("#{index}"),
                || {
                    let lambda = value.get_lambda_data().unwrap();
                    if lambda.env.is_some() {
                        format_interpreted_closure(lambda, options)
                    } else {
                        let params = format_params(&lambda.params);
                        let body = lambda
                            .body
                            .iter()
                            .map(expr::print_expr)
                            .collect::<Vec<_>>()
                            .join(" ");
                        format!("(lambda {} {})", params, body)
                    }
                },
            );
            out.extend_from_slice(text.as_bytes());
        }
        Value::Macro(_id) => {
            let m = value.get_lambda_data().unwrap();
            let params = format_params(&m.params);
            let body = m
                .body
                .iter()
                .map(expr::print_expr)
                .collect::<Vec<_>>()
                .join(" ");
            out.extend_from_slice(format!("(macro {} {})", params, body).as_bytes());
        }
        Value::Subr(id) => {
            out.extend_from_slice(format!("#<subr {}>", resolve_sym(*id)).as_bytes())
        }
        Value::ByteCode(_id) => {
            let bc = value.get_bytecode_data().unwrap();
            let params = format_params(&bc.params);
            out.extend_from_slice(
                format!("#<bytecode {} ({} ops)>", params, bc.ops.len()).as_bytes(),
            );
        }
        Value::Buffer(id) => out.extend_from_slice(format!("#<buffer {}>", id.0).as_bytes()),
        Value::Window(id) => out.extend_from_slice(format!("#<window {}>", id).as_bytes()),
        Value::Frame(id) => out.extend_from_slice(format_frame_handle(*id).as_bytes()),
        Value::Timer(id) => out.extend_from_slice(format!("#<timer {}>", id).as_bytes()),
    }
}

/// Re-export for compatibility.
pub fn print_expr(expr: &Expr) -> String {
    expr::print_expr(expr)
}

fn format_symbol(id: super::intern::SymId, options: PrintOptions) -> String {
    let name = resolve_sym(id);
    let canonical = lookup_interned(name);
    if canonical == Some(id) {
        format_symbol_name(name)
    } else if options.print_gensym {
        if name.is_empty() {
            "#:".to_string()
        } else {
            format!("#:{}", format_symbol_name(name))
        }
    } else {
        format_symbol_name(name)
    }
}

fn append_symbol_bytes(id: super::intern::SymId, out: &mut Vec<u8>, options: PrintOptions) {
    out.extend_from_slice(format_symbol(id, options).as_bytes());
}

fn format_symbol_name(name: &str) -> String {
    if name.is_empty() {
        return "##".to_string();
    }
    let mut out = String::with_capacity(name.len());
    for (idx, ch) in name.chars().enumerate() {
        let needs_escape = matches!(
            ch,
            ' ' | '\t'
                | '\n'
                | '\r'
                | '\u{0c}'
                | '('
                | ')'
                | '['
                | ']'
                | '"'
                | '\\'
                | ';'
                | '#'
                | '\''
                | '`'
                | ','
        ) || (idx == 0 && matches!(ch, '.' | '?'));
        if needs_escape {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

pub(crate) fn format_float(f: f64) -> String {
    const NAN_QUIET_BIT: u64 = 1u64 << 51;
    const NAN_PAYLOAD_MASK: u64 = (1u64 << 51) - 1;

    if f.is_nan() {
        let bits = f.to_bits();
        let frac = bits & ((1u64 << 52) - 1);
        if (frac & NAN_QUIET_BIT) != 0 {
            let payload = frac & NAN_PAYLOAD_MASK;
            return if f.is_sign_negative() {
                format!("-{}.0e+NaN", payload)
            } else {
                format!("{}.0e+NaN", payload)
            };
        }
        return if f.is_sign_negative() {
            "-0.0e+NaN".to_string()
        } else {
            "0.0e+NaN".to_string()
        };
    }
    if f.is_infinite() {
        return if f > 0.0 {
            "1.0e+INF".to_string()
        } else {
            "-1.0e+INF".to_string()
        };
    }
    format_float_dtoastr(f)
}

/// Format a finite float matching GNU Emacs's `dtoastr` / `float_to_string`:
/// use `%g`-style formatting with the minimum precision (starting from DBL_DIG=15)
/// that round-trips through strtod, then ensure a decimal point or exponent is present.
fn format_float_dtoastr(f: f64) -> String {
    let abs_f = f.abs();
    let start_prec = if abs_f != 0.0 && abs_f < f64::MIN_POSITIVE {
        1
    } else {
        15 // DBL_DIG
    };
    for prec in start_prec..=20 {
        // %g: uses %e if exponent < -4 or >= precision, otherwise %f.
        // %g also trims trailing zeros.
        let s = format!("{:.prec$e}", f, prec = prec - 1);
        // Parse back and check round-trip
        if let Ok(parsed) = s.parse::<f64>() {
            if parsed.to_bits() == f.to_bits() {
                // Convert from Rust's scientific notation to %g-style output
                return rust_sci_to_emacs_g(f, &s, prec);
            }
        }
    }
    // Fallback: maximum precision
    let s = format!("{:.20e}", f);
    rust_sci_to_emacs_g(f, &s, 21)
}

/// Convert Rust scientific notation string to GNU Emacs %g-style output.
/// %g rules: use fixed notation unless exponent >= precision or exponent < -4.
/// %g trims trailing zeros (but keeps at least one digit after decimal point
/// for Emacs's post-processing).
fn rust_sci_to_emacs_g(f: f64, sci: &str, prec: usize) -> String {
    // Parse the exponent from Rust's scientific notation (e.g., "3.14e2")
    let (mantissa_str, exp_str) = sci.split_once('e').unwrap_or((sci, "0"));
    let exp: i32 = exp_str.parse().unwrap_or(0);

    // %g uses fixed notation when -4 <= exp < prec
    let result = if exp >= -4 && exp < prec as i32 {
        // Fixed notation
        format_g_fixed(f, mantissa_str, exp, prec)
    } else {
        // Scientific notation with Emacs-style exponent formatting
        format_g_scientific(mantissa_str, exp, prec)
    };

    // Emacs post-processing: ensure decimal point or exponent is present
    ensure_decimal_point(result)
}

/// Format as fixed-point notation for %g, trimming trailing zeros.
fn format_g_fixed(f: f64, _mantissa: &str, exp: i32, prec: usize) -> String {
    // %g precision = total significant digits.
    // digits_after_dot = prec - exp - 1 (works for both positive and negative exp)
    let digits_after_dot = (prec as i32 - exp - 1).max(0) as usize;
    let s = format!("{:.digits$}", f, digits = digits_after_dot);
    trim_trailing_zeros_g(&s)
}

/// Format as scientific notation for %g, trimming trailing zeros.
fn format_g_scientific(mantissa: &str, exp: i32, _prec: usize) -> String {
    // Trim trailing zeros from mantissa
    let trimmed = trim_trailing_zeros_g(mantissa);
    // Emacs uses e+XX / e-XX with at least 2-digit exponent for |exp| < 100,
    // but %g in glibc actually uses minimal digits. Let's match C's %g.
    if exp >= 0 {
        format!("{}e+{:02}", trimmed, exp)
    } else {
        format!("{}e-{:02}", trimmed, -exp)
    }
}

/// Trim trailing zeros after decimal point (%g style).
/// "3.1400" -> "3.14", "3.0000" -> "3", "100" -> "100"
fn trim_trailing_zeros_g(s: &str) -> String {
    if !s.contains('.') {
        return s.to_string();
    }
    let trimmed = s.trim_end_matches('0');
    let trimmed = trimmed.trim_end_matches('.');
    trimmed.to_string()
}

/// Ensure the output has a decimal point with trailing digit (Emacs requirement).
/// If no decimal point or exponent, append ".0".
fn ensure_decimal_point(mut s: String) -> String {
    // Check if there's already a decimal point or exponent
    let has_dot_or_exp = s.bytes().any(|b| b == b'.' || b == b'e' || b == b'E');
    if !has_dot_or_exp {
        s.push_str(".0");
    } else if s.ends_with('.') {
        s.push('0');
    }
    s
}

fn format_params(params: &super::value::LambdaParams) -> String {
    let mut parts = Vec::new();
    for p in &params.required {
        parts.push(resolve_sym(*p).to_string());
    }
    if !params.optional.is_empty() {
        parts.push("&optional".to_string());
        for p in &params.optional {
            parts.push(resolve_sym(*p).to_string());
        }
    }
    if let Some(rest) = params.rest {
        parts.push("&rest".to_string());
        parts.push(resolve_sym(rest).to_string());
    }
    if parts.is_empty() {
        "nil".to_string()
    } else {
        format!("({})", parts.join(" "))
    }
}

fn format_lambda_body_forms(body: &[Expr]) -> String {
    if body.is_empty() {
        "nil".to_string()
    } else {
        format!(
            "({})",
            body.iter()
                .map(expr::print_expr)
                .collect::<Vec<_>>()
                .join(" ")
        )
    }
}

fn format_interpreted_closure(lambda: &super::value::LambdaData, options: PrintOptions) -> String {
    let mut slots = Vec::with_capacity(5);
    slots.push(format_params(&lambda.params));
    slots.push(format_lambda_body_forms(lambda.body.as_ref()));
    let env = lambda.env.expect("closure env");
    slots.push(if env == Value::Nil {
        "(t)".to_string()
    } else {
        print_value_with_options(&env, options)
    });
    if lambda.docstring.is_some() || lambda.doc_form.is_some() {
        slots.push("nil".to_string());
        slots.push(if let Some(doc_form) = lambda.doc_form {
            print_value_with_options(&doc_form, options)
        } else if let Some(docstring) = &lambda.docstring {
            format_lisp_string(docstring)
        } else {
            "nil".to_string()
        });
    }
    format!("#[{}]", slots.join(" "))
}

fn print_list_shorthand(value: &Value, options: PrintOptions) -> Option<String> {
    let items = list_to_vec(value)?;
    if items.len() != 2 {
        return None;
    }

    let head = match &items[0] {
        Value::Symbol(id) => resolve_sym(*id),
        _ => return None,
    };

    if head == "make-hash-table-from-literal" {
        if let Some(payload) = quote_payload(&items[1]) {
            return Some(format!("#s{}", print_value_with_options(&payload, options)));
        }
        return None;
    }

    let (prefix, nested_options) = match head {
        "quote" => ("'", options),
        "function" => ("#'", options),
        "`" => ("`", options.enter_backquote()),
        "," => {
            if !options.allow_unquote_shorthand() {
                return None;
            }
            (",", options.exit_backquote())
        }
        ",@" => {
            if !options.allow_unquote_shorthand() {
                return None;
            }
            (",@", options.exit_backquote())
        }
        _ => return None,
    };

    Some(format!(
        "{prefix}{}",
        print_value_with_options(&items[1], nested_options)
    ))
}

fn print_list_shorthand_bytes(value: &Value, options: PrintOptions) -> Option<Vec<u8>> {
    let items = list_to_vec(value)?;
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
        append_print_value_bytes(&payload, &mut out, options);
        return Some(out);
    }

    let (prefix, nested_options): (&[u8], PrintOptions) = match head {
        "quote" => (b"'", options),
        "function" => (b"#'", options),
        "`" => (b"`", options.enter_backquote()),
        "," => {
            if !options.allow_unquote_shorthand() {
                return None;
            }
            (b",", options.exit_backquote())
        }
        ",@" => {
            if !options.allow_unquote_shorthand() {
                return None;
            }
            (b",@", options.exit_backquote())
        }
        _ => return None,
    };

    let mut out = Vec::new();
    out.extend_from_slice(prefix);
    append_print_value_bytes(&items[1], &mut out, nested_options);
    Some(out)
}

fn quote_payload(value: &Value) -> Option<Value> {
    let items = list_to_vec(value)?;
    if items.len() != 2 {
        return None;
    }
    match &items[0] {
        Value::Symbol(id) if resolve_sym(*id) == "quote" => Some(items[1]),
        _ => None,
    }
}

fn print_cons(value: &Value, out: &mut String, options: PrintOptions) {
    let mut cursor = *value;
    let mut first = true;
    loop {
        match cursor {
            Value::Cons(cell) => {
                if !first {
                    out.push(' ');
                }
                let pair = read_cons(cell);
                out.push_str(&print_value_with_options(&pair.car, options));
                cursor = pair.cdr;
                first = false;
            }
            Value::Nil => return,
            other => {
                if !first {
                    out.push_str(" . ");
                }
                out.push_str(&print_value_with_options(&other, options));
                return;
            }
        }
    }
}

fn print_cons_bytes(value: &Value, out: &mut Vec<u8>, options: PrintOptions) {
    let mut cursor = *value;
    let mut first = true;
    loop {
        match cursor {
            Value::Cons(cell) => {
                if !first {
                    out.push(b' ');
                }
                let pair = read_cons(cell);
                append_print_value_bytes(&pair.car, out, options);
                cursor = pair.cdr;
                first = false;
            }
            Value::Nil => return,
            other => {
                if !first {
                    out.extend_from_slice(b" . ");
                }
                append_print_value_bytes(&other, out, options);
                return;
            }
        }
    }
}
// -- Bool-vector printing ---------------------------------------------------

/// Format a bool-vector as `#&N"..."`.
fn format_bool_vector(value: &Value, nbits: usize) -> String {
    let mut out = Vec::new();
    append_bool_vector_bytes(value, nbits, &mut out);
    String::from_utf8_lossy(&out).into_owned()
}

/// Append bool-vector bytes as `#&N"..."`.
fn append_bool_vector_bytes(value: &Value, nbits: usize, out: &mut Vec<u8>) {
    let items = match value {
        Value::Vector(v) => with_heap(|h| h.get_vector(*v).clone()),
        _ => return,
    };
    // items[0] = tag, items[1] = size, items[2..] = individual bit values
    let nbytes = (nbits + 7) / 8;

    out.extend_from_slice(b"#&");
    out.extend_from_slice(nbits.to_string().as_bytes());
    out.push(b'"');

    for byte_idx in 0..nbytes {
        let mut byte_val: u8 = 0;
        for bit_idx in 0..8 {
            let overall_bit = byte_idx * 8 + bit_idx;
            if overall_bit >= nbits {
                break;
            }
            let is_set = match items.get(2 + overall_bit) {
                Some(Value::Int(n)) => *n != 0,
                Some(v) => v.is_truthy(),
                None => false,
            };
            if is_set {
                byte_val |= 1 << bit_idx; // LSB first
            }
        }
        match byte_val {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b if b > 0x7F => {
                // Octal escape for high bytes, matching GNU Emacs
                out.extend_from_slice(format!("\\{:03o}", b).as_bytes());
            }
            _ => out.push(byte_val),
        }
    }

    out.push(b'"');
}

// -- Hash-table printing ----------------------------------------------------

fn format_hash_table(id: crate::gc::types::ObjId, options: PrintOptions) -> String {
    let table = with_heap(|h| h.get_hash_table(id).clone());
    let mut out = String::from("#s(hash-table");

    // GNU Emacs omits test when it's eql (the default).
    match table.test {
        HashTableTest::Eq => out.push_str(" test eq"),
        HashTableTest::Equal => out.push_str(" test equal"),
        HashTableTest::Eql => {} // default, omitted
    }

    // GNU Emacs omits weakness when there is none.
    if let Some(ref weakness) = table.weakness {
        let name = match weakness {
            super::value::HashTableWeakness::Key => "key",
            super::value::HashTableWeakness::Value => "value",
            super::value::HashTableWeakness::KeyOrValue => "key-or-value",
            super::value::HashTableWeakness::KeyAndValue => "key-and-value",
        };
        out.push_str(" weakness ");
        out.push_str(name);
    }

    // GNU Emacs omits data when the table is empty.
    if !table.data.is_empty() {
        out.push_str(" data (");
        let mut first = true;
        for key in &table.insertion_order {
            if let Some(val) = table.data.get(key) {
                if !first {
                    out.push(' ');
                }
                let key_val = super::hashtab::hash_key_to_visible_value(&table, key);
                out.push_str(&print_value_with_options(&key_val, options));
                out.push(' ');
                out.push_str(&print_value_with_options(val, options));
                first = false;
            }
        }
        out.push(')');
    }

    out.push(')');
    out
}

#[cfg(test)]
#[path = "print_test.rs"]
mod tests;
