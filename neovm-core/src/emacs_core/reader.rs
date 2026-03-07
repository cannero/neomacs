//! Reader/printer builtins: read-from-string, read, prin1-to-string (enhanced),
//! format-spec, and various interactive-input stubs.

use super::error::{EvalResult, Flow, signal};
use super::expr::Expr;
use super::intern::{SymId, intern, resolve_sym};
use super::value::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_max_args(name: &str, args: &[Value], max: usize) -> Result<(), Flow> {
    if args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_string(value: &Value) -> Result<String, Flow> {
    match value {
        Value::Str(id) => Ok(with_heap(|h| h.get_string(*id).clone())),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
    }
}

fn expect_number(value: &Value) -> Result<(), Flow> {
    match value {
        Value::Int(_) | Value::Float(_, _) | Value::Char(_) => Ok(()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("numberp"), *other],
        )),
    }
}

fn expect_initial_input_stringish(value: &Value) -> Result<(), Flow> {
    match value {
        Value::Nil | Value::Str(_) => Ok(()),
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            if !matches!(pair.car, Value::Str(_)) {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), pair.car],
                ));
            }
            Ok(())
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
    }
}

fn expect_completing_read_initial_input(value: &Value) -> Result<(), Flow> {
    match value {
        Value::Nil | Value::Str(_) => Ok(()),
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            if !matches!(pair.car, Value::Str(_)) {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), pair.car],
                ));
            }
            if !matches!(pair.cdr, Value::Int(_) | Value::Char(_)) {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("number-or-marker-p"), pair.cdr],
                ));
            }
            Ok(())
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
    }
}

// ---------------------------------------------------------------------------
// 1. read-from-string
// ---------------------------------------------------------------------------

/// `(read-from-string STRING &optional START END)`
///
/// Parse a single Lisp object from STRING starting at position START (default 0).
/// Returns `(OBJECT . END-POSITION)` where END-POSITION is the character index
/// after the parsed object.
pub(crate) fn builtin_read_from_string(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("read-from-string", &args, 1)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("read-from-string"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let full_string = expect_string(&args[0])?;

    let start_arg = args.get(1).cloned().unwrap_or(Value::Nil);
    let end_arg = args.get(2).cloned().unwrap_or(Value::Nil);
    let to_index = |value: &Value| -> Result<usize, Flow> {
        match value {
            Value::Nil => Ok(0),
            Value::Int(n) => {
                let idx = if *n < 0 {
                    (full_string.len() as i64) + *n
                } else {
                    *n
                };
                if idx < 0 || idx > full_string.len() as i64 {
                    return Err(signal(
                        "args-out-of-range",
                        vec![args[0], start_arg, end_arg],
                    ));
                }
                Ok(idx as usize)
            }
            other => Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("integerp"), *other],
            )),
        }
    };
    let start = if args.len() > 1 {
        to_index(&start_arg)?
    } else {
        0
    };
    let end = if args.len() > 2 {
        to_index(&end_arg)?
    } else {
        full_string.len()
    };

    if start > end {
        return Err(signal(
            "args-out-of-range",
            vec![args[0], start_arg, end_arg],
        ));
    }

    let substring = &full_string[start..end];
    if starts_with_hash_skip_dispatch(substring) {
        return Err(signal(
            "end-of-file",
            vec![Value::string("End of file during parsing")],
        ));
    }
    let end_pos = compute_read_end_position(substring);
    if end_pos == 0 {
        return Err(signal(
            "end-of-file",
            vec![Value::string("End of file during parsing")],
        ));
    }

    let consumed = &substring[..end_pos.min(substring.len())];
    let forms = super::parser::parse_forms(consumed).map_err(|e| {
        if e.message.contains("unterminated") || e.message.contains("end of input") {
            signal(
                "end-of-file",
                vec![Value::string("End of file during parsing")],
            )
        } else {
            signal(
                "invalid-read-syntax",
                vec![Value::string(e.message.clone())],
            )
        }
    })?;

    if forms.is_empty() {
        return Err(signal(
            "end-of-file",
            vec![Value::string("End of file during parsing")],
        ));
    }

    let value = if let Some(bytecode) = first_form_byte_code_literal_value(eval, &forms[0]) {
        bytecode
    } else if let Some(hash_table) = first_form_hash_table_literal_value(eval, &forms[0]) {
        hash_table
    } else {
        eval.quote_to_runtime_value(&forms[0])
    };
    let absolute_end = start + end_pos;

    Ok(Value::cons(value, Value::Int(absolute_end as i64)))
}

fn first_form_byte_code_literal_value(
    eval: &mut super::eval::Evaluator,
    expr: &Expr,
) -> Option<Value> {
    let Expr::List(items) = expr else {
        return None;
    };
    if items.len() != 2 {
        return None;
    }
    let Expr::Symbol(id) = &items[0] else {
        return None;
    };
    if resolve_sym(*id) != "byte-code-literal" {
        return None;
    }
    let Expr::Vector(values) = &items[1] else {
        return None;
    };
    let values = values
        .iter()
        .map(|value| eval.quote_to_runtime_value(value))
        .collect();
    Some(Value::vector(values))
}

fn first_form_hash_table_literal_value(
    eval: &mut super::eval::Evaluator,
    expr: &Expr,
) -> Option<Value> {
    let Expr::List(items) = expr else {
        return None;
    };
    if items.len() != 2 {
        return None;
    }
    let Expr::Symbol(id) = &items[0] else {
        return None;
    };
    if resolve_sym(*id) != "make-hash-table-from-literal" {
        return None;
    }
    let Expr::List(quoted) = &items[1] else {
        return None;
    };
    if quoted.len() != 2 {
        return None;
    }
    if !matches!(&quoted[0], Expr::Symbol(id) if resolve_sym(*id) == "quote") {
        return None;
    }
    let Expr::List(spec) = &quoted[1] else {
        return None;
    };
    if !matches!(spec.first(), Some(Expr::Symbol(id)) if resolve_sym(*id) == "hash-table") {
        return None;
    }

    let mut test = HashTableTest::Eql;
    let mut test_name: Option<SymId> = None;
    let mut size = 0_i64;
    let mut weakness: Option<HashTableWeakness> = None;
    let mut rehash_size = 1.5_f64;
    let mut rehash_threshold = 0.8125_f64;
    let mut data_expr: Option<&Expr> = None;

    let mut i = 1_usize;
    while i + 1 < spec.len() {
        let Expr::Symbol(key_id) = &spec[i] else {
            i += 1;
            continue;
        };
        let value = eval.quote_to_runtime_value(&spec[i + 1]);
        match resolve_sym(*key_id) {
            "size" => {
                size = value.as_int()?;
            }
            "test" => {
                let name = value.as_symbol_name()?;
                test = match name {
                    "eq" => HashTableTest::Eq,
                    "eql" => HashTableTest::Eql,
                    "equal" => HashTableTest::Equal,
                    _ => return None,
                };
                test_name = Some(intern(name));
            }
            "weakness" => {
                weakness = match value.as_symbol_name() {
                    Some("key") => Some(HashTableWeakness::Key),
                    Some("value") => Some(HashTableWeakness::Value),
                    Some("key-or-value") => Some(HashTableWeakness::KeyOrValue),
                    Some("key-and-value") => Some(HashTableWeakness::KeyAndValue),
                    Some("nil") | None => None,
                    _ => return None,
                };
            }
            "rehash-size" => {
                rehash_size = value.as_float().unwrap_or(value.as_int()? as f64);
            }
            "rehash-threshold" => {
                rehash_threshold = value.as_float().unwrap_or(value.as_int()? as f64);
            }
            "data" => {
                data_expr = Some(&spec[i + 1]);
            }
            _ => {}
        }
        i += 2;
    }

    let table_value =
        Value::hash_table_with_options(test, size, weakness, rehash_size, rehash_threshold);
    if let Value::HashTable(table_ref) = &table_value {
        with_heap_mut(|h| {
            let table = h.get_hash_table_mut(*table_ref);
            table.test_name = test_name;
            if let Some(Expr::List(data_items)) = data_expr {
                let mut idx = 0_usize;
                while idx + 1 < data_items.len() {
                    let key_value = eval.quote_to_runtime_value(&data_items[idx]);
                    let val_value = eval.quote_to_runtime_value(&data_items[idx + 1]);
                    let key = key_value.to_hash_key(&table.test);
                    let inserting_new_key = !table.data.contains_key(&key);
                    table.data.insert(key.clone(), val_value);
                    if inserting_new_key {
                        table.key_snapshots.insert(key.clone(), key_value);
                        table.insertion_order.push(key);
                    }
                    idx += 2;
                }
            }
        });
    }
    Some(table_value)
}

fn starts_with_hash_skip_dispatch(input: &str) -> bool {
    let bytes = input.as_bytes();
    let pos = skip_ws_comments(input, 0);
    pos + 1 < bytes.len() && bytes[pos] == b'#' && bytes[pos + 1] == b'@'
}

/// Estimate the end position of the first parsed form in the input string.
/// We re-parse character by character to find where the parser would stop
/// after reading one expression.
fn compute_read_end_position(input: &str) -> usize {
    // Use a simple approach: parse just one form and see how far we get.
    // We create a mini-parser that tracks position.
    let mut pos = 0;
    let bytes = input.as_bytes();

    // Skip leading whitespace and comments
    pos = skip_ws_comments(input, pos);

    if pos >= input.len() {
        return input.len();
    }

    // Now skip one sexp
    pos = skip_one_sexp(input, pos);

    // Skip any trailing whitespace up to the end of the consumed region
    // (Emacs `read-from-string` stops right after the sexp, no trailing ws skip)
    let _ = bytes;
    pos
}

fn skip_ws_comments(input: &str, mut pos: usize) -> usize {
    let bytes = input.as_bytes();
    loop {
        if pos >= bytes.len() {
            return pos;
        }
        let ch = bytes[pos];
        if ch.is_ascii_whitespace() {
            pos += 1;
            continue;
        }
        if ch == b';' {
            // line comment
            while pos < bytes.len() && bytes[pos] != b'\n' {
                pos += 1;
            }
            if pos < bytes.len() {
                pos += 1; // skip newline
            }
            continue;
        }
        if ch == b'#' && pos + 1 < bytes.len() && bytes[pos + 1] == b'|' {
            // block comment
            pos += 2;
            let mut depth = 1;
            while depth > 0 && pos < bytes.len() {
                if bytes[pos] == b'#' && pos + 1 < bytes.len() && bytes[pos + 1] == b'|' {
                    depth += 1;
                    pos += 2;
                } else if bytes[pos] == b'|' && pos + 1 < bytes.len() && bytes[pos + 1] == b'#' {
                    depth -= 1;
                    pos += 2;
                } else {
                    pos += 1;
                }
            }
            continue;
        }
        return pos;
    }
}

fn skip_one_sexp(input: &str, mut pos: usize) -> usize {
    let bytes = input.as_bytes();
    if pos >= bytes.len() {
        return pos;
    }

    let ch = bytes[pos];

    match ch {
        b'(' => {
            pos += 1;
            let mut depth = 1;
            while depth > 0 && pos < bytes.len() {
                match bytes[pos] {
                    b'(' => {
                        depth += 1;
                        pos += 1;
                    }
                    b')' => {
                        depth -= 1;
                        pos += 1;
                    }
                    b'"' => {
                        pos = skip_string(input, pos);
                    }
                    b';' => {
                        while pos < bytes.len() && bytes[pos] != b'\n' {
                            pos += 1;
                        }
                    }
                    b'\\' => {
                        pos += 1; // skip backslash
                        if pos < bytes.len() {
                            pos += 1; // skip escaped char
                        }
                    }
                    _ => {
                        pos += 1;
                    }
                }
            }
            pos
        }
        b'[' => {
            pos += 1;
            let mut depth = 1;
            while depth > 0 && pos < bytes.len() {
                match bytes[pos] {
                    b'[' => {
                        depth += 1;
                        pos += 1;
                    }
                    b']' => {
                        depth -= 1;
                        pos += 1;
                    }
                    b'"' => {
                        pos = skip_string(input, pos);
                    }
                    b'\\' => {
                        pos += 1;
                        if pos < bytes.len() {
                            pos += 1;
                        }
                    }
                    _ => {
                        pos += 1;
                    }
                }
            }
            pos
        }
        b'"' => skip_string(input, pos),
        b'\'' | b'`' => {
            // quote / backquote — skip prefix then one sexp
            pos += 1;
            pos = skip_ws_comments(input, pos);
            skip_one_sexp(input, pos)
        }
        b',' => {
            pos += 1;
            if pos < bytes.len() && bytes[pos] == b'@' {
                pos += 1;
            }
            pos = skip_ws_comments(input, pos);
            skip_one_sexp(input, pos)
        }
        b'#' => {
            pos += 1;
            if pos >= bytes.len() {
                return pos;
            }
            match bytes[pos] {
                b'\'' => {
                    // #'symbol
                    pos += 1;
                    pos = skip_ws_comments(input, pos);
                    skip_one_sexp(input, pos)
                }
                b'(' => {
                    // #(vector)
                    skip_one_sexp(input, pos)
                }
                b'[' => {
                    // #[vector] compiled-function literal
                    skip_one_sexp(input, pos)
                }
                b'@' => {
                    // #@N<bytes> ... next-object
                    pos += 1;
                    let digits_start = pos;
                    while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                        pos += 1;
                    }
                    if pos == digits_start {
                        return pos;
                    }
                    let len = std::str::from_utf8(&bytes[digits_start..pos])
                        .ok()
                        .and_then(|s| s.parse::<usize>().ok());
                    let Some(len) = len else {
                        return pos;
                    };
                    let Some(after_data) = pos.checked_add(len) else {
                        return bytes.len();
                    };
                    if after_data > bytes.len() {
                        return bytes.len();
                    }
                    pos = skip_ws_comments(input, after_data);
                    if pos >= bytes.len() {
                        pos
                    } else {
                        skip_one_sexp(input, pos)
                    }
                }
                b'$' => {
                    // #$ pseudo object
                    pos + 1
                }
                b':' => {
                    // #:symbol — uninterned symbol reader syntax.
                    pos += 1;
                    skip_symbol_token(input, pos)
                }
                b'#' => {
                    // ## empty-symbol reader spelling
                    pos + 1
                }
                b'&' => {
                    // #&SIZE"DATA" bool-vector literal.
                    pos += 1;
                    while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                        pos += 1;
                    }
                    if pos < bytes.len() && bytes[pos] == b'"' {
                        skip_string(input, pos)
                    } else {
                        pos
                    }
                }
                b's' => {
                    // #s(hash-table ...)
                    pos += 1;
                    if pos < bytes.len() && bytes[pos] == b'(' {
                        skip_one_sexp(input, pos)
                    } else {
                        pos
                    }
                }
                b'x' | b'X' | b'o' | b'O' | b'b' | b'B' => {
                    // radix number
                    pos += 1;
                    while pos < bytes.len()
                        && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_')
                    {
                        pos += 1;
                    }
                    pos
                }
                b'0'..=b'9' => {
                    // #N=EXPR / #N#
                    while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                        pos += 1;
                    }
                    if pos >= bytes.len() {
                        return pos;
                    }
                    match bytes[pos] {
                        b'=' => {
                            pos += 1;
                            pos = skip_ws_comments(input, pos);
                            skip_one_sexp(input, pos)
                        }
                        b'#' => pos + 1,
                        _ => pos,
                    }
                }
                _ => pos + 1,
            }
        }
        b'?' => {
            // char literal
            pos += 1;
            if pos < bytes.len() && bytes[pos] == b'\\' {
                pos += 1;
                if pos < bytes.len() {
                    let esc = bytes[pos];
                    pos += 1;
                    match esc {
                        b'x' => {
                            while pos < bytes.len() && bytes[pos].is_ascii_hexdigit() {
                                pos += 1;
                            }
                            // Optional terminating ';'
                            if pos < bytes.len() && bytes[pos] == b';' {
                                pos += 1;
                            }
                        }
                        b'u' => {
                            for _ in 0..4 {
                                if pos < bytes.len() && bytes[pos].is_ascii_hexdigit() {
                                    pos += 1;
                                }
                            }
                        }
                        b'U' => {
                            for _ in 0..8 {
                                if pos < bytes.len() && bytes[pos].is_ascii_hexdigit() {
                                    pos += 1;
                                }
                            }
                        }
                        b'0'..=b'7' => {
                            for _ in 0..2 {
                                if pos < bytes.len() && bytes[pos] >= b'0' && bytes[pos] <= b'7' {
                                    pos += 1;
                                }
                            }
                        }
                        b'C' | b'M' | b'S' => {
                            if pos < bytes.len() && bytes[pos] == b'-' {
                                pos += 1;
                                if pos < bytes.len() {
                                    pos += 1;
                                }
                            }
                        }
                        _ => {} // single escaped char already consumed
                    }
                }
            } else if pos < bytes.len() {
                // Regular character — consume one UTF-8 char
                let ch = input[pos..].chars().next();
                if let Some(c) = ch {
                    pos += c.len_utf8();
                }
            }
            pos
        }
        _ => {
            // Atom: symbol or number
            skip_symbol_token(input, pos)
        }
    }
}

fn skip_symbol_token(input: &str, mut pos: usize) -> usize {
    let bytes = input.as_bytes();
    while pos < bytes.len() {
        let b = bytes[pos];
        if b.is_ascii_whitespace()
            || b == b'('
            || b == b')'
            || b == b'['
            || b == b']'
            || b == b'\''
            || b == b'`'
            || b == b','
            || b == b'"'
            || b == b';'
        {
            break;
        }
        if b == b'\\' {
            pos += 1;
            if pos < bytes.len() {
                pos += 1;
            }
        } else {
            pos += 1;
        }
    }
    pos
}

fn skip_string(input: &str, mut pos: usize) -> usize {
    let bytes = input.as_bytes();
    if pos >= bytes.len() || bytes[pos] != b'"' {
        return pos;
    }
    pos += 1; // opening quote
    while pos < bytes.len() {
        match bytes[pos] {
            b'"' => {
                pos += 1;
                return pos;
            }
            b'\\' => {
                pos += 1;
                if pos < bytes.len() {
                    pos += 1;
                }
            }
            _ => {
                pos += 1;
            }
        }
    }
    pos
}

// ---------------------------------------------------------------------------
// 2. read
// ---------------------------------------------------------------------------

/// `(read &optional STREAM)`
///
/// Read one Lisp expression from STREAM.
/// - If STREAM is a string, read from that string (equivalent to car of read-from-string).
/// - If STREAM is nil, would read from stdin (returns nil in non-interactive mode).
/// - If STREAM is a buffer, read from buffer at point.
pub(crate) fn builtin_read(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_max_args("read", &args, 1)?;

    if args.is_empty() || args[0].is_nil() {
        // In batch/non-interactive runs, stdin-backed read signals EOF.
        return Err(signal(
            "end-of-file",
            vec![Value::string("End of file during parsing")],
        ));
    }

    match &args[0] {
        Value::Str(_) => {
            // Read from string
            let result = builtin_read_from_string(eval, args)?;
            // Return just the car (the parsed object)
            match &result {
                Value::Cons(cell) => {
                    let pair = read_cons(*cell);
                    Ok(pair.car)
                }
                _ => Ok(result),
            }
        }
        Value::Buffer(id) => {
            // Read from buffer at point
            let buf_id = *id;
            let (text, pt) = {
                let buf = eval
                    .buffers
                    .get(buf_id)
                    .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;
                (buf.buffer_string(), buf.pt)
            };
            // pt is 1-based, substring from (pt-1)
            let start = if pt > 0 { pt - 1 } else { 0 };
            if start >= text.len() {
                return Err(signal(
                    "end-of-file",
                    vec![Value::string("End of file during parsing")],
                ));
            }
            let substring = &text[start..];
            let forms = super::parser::parse_forms(substring).map_err(|e| {
                signal(
                    "invalid-read-syntax",
                    vec![Value::string(e.message.clone())],
                )
            })?;
            if forms.is_empty() {
                return Err(signal(
                    "end-of-file",
                    vec![Value::string("End of file during parsing")],
                ));
            }
            let value = if let Some(bytecode) = first_form_byte_code_literal_value(eval, &forms[0])
            {
                bytecode
            } else if let Some(hash_table) = first_form_hash_table_literal_value(eval, &forms[0]) {
                hash_table
            } else {
                eval.quote_to_runtime_value(&forms[0])
            };
            // Advance point past the read form
            let end_offset = compute_read_end_position(substring);
            let new_pt = pt + end_offset;
            if let Some(buf) = eval.buffers.get_mut(buf_id) {
                buf.pt = new_pt;
            }
            Ok(value)
        }
        Value::Symbol(id) => Err(signal(
            "void-function",
            vec![Value::symbol(resolve_sym(*id))],
        )),
        Value::True => Err(signal(
            "end-of-file",
            vec![Value::string("End of file during parsing")],
        )),
        Value::Keyword(id) => Err(signal(
            "void-function",
            vec![Value::symbol(resolve_sym(*id))],
        )),
        _ => {
            // Unsupported stream source type for read-char function protocol.
            Err(signal("invalid-function", vec![args[0]]))
        }
    }
}

// ---------------------------------------------------------------------------
// 5. read-from-minibuffer
// ---------------------------------------------------------------------------

/// `(read-from-minibuffer PROMPT ...)`
///
/// In batch/non-interactive mode, if `unread-command-events` is non-empty,
/// signal `end-of-file` and keep the event queue unchanged (Oracle-compatible
/// behavior).
pub(crate) fn builtin_read_from_minibuffer(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("read-from-minibuffer", &args, 1)?;
    expect_max_args("read-from-minibuffer", &args, 7)?;
    let _prompt = expect_string(&args[0])?;
    if let Some(initial) = args.get(1) {
        expect_initial_input_stringish(initial)?;
    }
    if eval.peek_unread_command_event().is_some() {
        return Err(signal(
            "end-of-file",
            vec![Value::string("Error reading from stdin")],
        ));
    }
    Err(signal(
        "end-of-file",
        vec![Value::string("Error reading from stdin")],
    ))
}

// ---------------------------------------------------------------------------
// 6. read-string
// ---------------------------------------------------------------------------

/// `(read-string PROMPT ...)`
///
/// In batch/non-interactive mode, if `unread-command-events` is non-empty,
/// signal `end-of-file` and keep the event queue unchanged (Oracle-compatible
/// behavior).
pub(crate) fn builtin_read_string(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("read-string", &args, 1)?;
    expect_max_args("read-string", &args, 5)?;
    let _prompt = expect_string(&args[0])?;
    if let Some(initial) = args.get(1) {
        expect_initial_input_stringish(initial)?;
    }
    if eval.peek_unread_command_event().is_some() {
        return Err(signal(
            "end-of-file",
            vec![Value::string("Error reading from stdin")],
        ));
    }
    Err(signal(
        "end-of-file",
        vec![Value::string("Error reading from stdin")],
    ))
}

// ---------------------------------------------------------------------------
// 7. read-number
// ---------------------------------------------------------------------------

/// `(read-number PROMPT &optional DEFAULT)`
///
/// In batch mode, if `unread-command-events` is non-empty, signal
/// `end-of-file` and keep the event queue unchanged (Oracle-compatible
/// behavior).
pub(crate) fn builtin_read_number(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("read-number", &args, 1)?;
    expect_max_args("read-number", &args, 3)?;
    let _prompt = expect_string(&args[0])?;
    if let Some(default) = args.get(1) {
        if !default.is_nil() {
            expect_number(default)?;
        }
    }
    if eval.peek_unread_command_event().is_some() {
        return Err(signal(
            "end-of-file",
            vec![Value::string("Error reading from stdin")],
        ));
    }
    Err(signal(
        "end-of-file",
        vec![Value::string("Error reading from stdin")],
    ))
}

// ---------------------------------------------------------------------------
// 8. read-passwd
// ---------------------------------------------------------------------------

/// `(read-passwd PROMPT &optional CONFIRM DEFAULT)`
///
/// In batch/non-interactive mode, if `unread-command-events` is non-empty,
/// signal `end-of-file` and keep the event queue unchanged (Oracle-compatible
/// behavior).
pub(crate) fn builtin_read_passwd(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("read-passwd", &args, 1)?;
    expect_max_args("read-passwd", &args, 3)?;
    let _prompt = expect_string(&args[0])?;
    if eval.peek_unread_command_event().is_some() {
        return Err(signal(
            "end-of-file",
            vec![Value::string("Error reading from stdin")],
        ));
    }
    Err(signal(
        "end-of-file",
        vec![Value::string("Error reading from stdin")],
    ))
}

// ---------------------------------------------------------------------------
// 9. completing-read
// ---------------------------------------------------------------------------

/// `(completing-read PROMPT COLLECTION ...)`
///
/// In batch/non-interactive mode, if `unread-command-events` is non-empty,
/// signal `end-of-file` and keep the event queue unchanged (Oracle-compatible
/// behavior).
pub(crate) fn builtin_completing_read(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("completing-read", &args, 2)?;
    expect_max_args("completing-read", &args, 8)?;
    let _prompt = expect_string(&args[0])?;
    if let Some(initial) = args.get(4) {
        expect_completing_read_initial_input(initial)?;
    }
    if eval.peek_unread_command_event().is_some() {
        return Err(signal(
            "end-of-file",
            vec![Value::string("Error reading from stdin")],
        ));
    }
    Err(signal(
        "end-of-file",
        vec![Value::string("Error reading from stdin")],
    ))
}

fn event_to_int(event: &Value) -> Option<i64> {
    match event {
        Value::Int(n) => Some(*n),
        Value::Char(c) => Some(*c as i64),
        _ => None,
    }
}

fn event_to_char(event: &Value) -> Option<char> {
    match event {
        Value::Char(c) => Some(*c),
        Value::Int(n) if *n >= 0 => char::from_u32(*n as u32),
        _ => None,
    }
}

fn expect_optional_prompt_string(args: &[Value]) -> Result<(), Flow> {
    if args.is_empty() || args[0].is_nil() || matches!(args[0], Value::Str(_)) {
        return Ok(());
    }
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("stringp"), args[0]],
    ))
}

fn non_character_input_event_error() -> Flow {
    signal("error", vec![Value::string("Non-character input-event")])
}

// ---------------------------------------------------------------------------
// 10. input-pending-p
// ---------------------------------------------------------------------------

/// `(input-pending-p &optional CHECK-TIMERS)`
///
/// Return non-nil when `unread-command-events` has at least one pending event.
/// `CHECK-TIMERS` is currently accepted for arity compatibility and ignored.
pub(crate) fn builtin_input_pending_p(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("input-pending-p", &args, 1)?;
    Ok(Value::bool(eval.peek_unread_command_event().is_some()))
}

// ---------------------------------------------------------------------------
// 11. discard-input
// ---------------------------------------------------------------------------

/// `(discard-input)`
///
/// Discard pending unread command events for the current scope.
pub(crate) fn builtin_discard_input(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("discard-input", &args, 0)?;
    eval.assign("unread-command-events", Value::Nil);
    Ok(Value::Nil)
}

// ---------------------------------------------------------------------------
// 12. current-input-mode / set-input-mode
// ---------------------------------------------------------------------------

/// `(current-input-mode)` -> `(INTERRUPT FLOW META QUIT)`
pub(crate) fn builtin_current_input_mode(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("current-input-mode", &args, 0)?;
    let (interrupt, flow, meta, quit) = eval.current_input_mode_tuple();
    Ok(Value::list(vec![
        Value::bool(interrupt),
        Value::bool(flow),
        Value::bool(meta),
        Value::Int(quit),
    ]))
}

/// `(set-input-mode INTERRUPT FLOW META QUIT)`
///
/// Batch-compatible behavior currently tracks only INTERRUPT and ignores
/// FLOW/META/QUIT while preserving arity/return-value semantics.
pub(crate) fn builtin_set_input_mode(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("set-input-mode", &args, 3)?;
    expect_max_args("set-input-mode", &args, 4)?;
    eval.set_input_mode_interrupt(args[0].is_truthy());
    Ok(Value::Nil)
}

// ---------------------------------------------------------------------------
// 13. input mode helper setters
// ---------------------------------------------------------------------------

/// `(set-input-interrupt-mode INTERRUPT)`
pub(crate) fn builtin_set_input_interrupt_mode(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-input-interrupt-mode", &args, 1)?;
    eval.set_input_mode_interrupt(args[0].is_truthy());
    Ok(Value::Nil)
}

/// `(set-input-meta-mode META)`
///
/// Batch-compatible behavior: accepts GNU-compatible optional TERMINAL and returns nil.
pub(crate) fn builtin_set_input_meta_mode(args: Vec<Value>) -> EvalResult {
    expect_min_args("set-input-meta-mode", &args, 1)?;
    expect_max_args("set-input-meta-mode", &args, 2)?;
    Ok(Value::Nil)
}

/// `(set-output-flow-control FLOW)`
///
/// Batch-compatible behavior: accepts one argument and returns nil.
pub(crate) fn builtin_set_output_flow_control(args: Vec<Value>) -> EvalResult {
    expect_min_args("set-output-flow-control", &args, 1)?;
    expect_max_args("set-output-flow-control", &args, 2)?;
    Ok(Value::Nil)
}

/// `(set-quit-char CHAR)`
///
/// Batch-compatible behavior: accepts one argument and returns nil.
pub(crate) fn builtin_set_quit_char(args: Vec<Value>) -> EvalResult {
    expect_args("set-quit-char", &args, 1)?;
    Ok(Value::Nil)
}

// ---------------------------------------------------------------------------
// 14. waiting-for-user-input-p
// ---------------------------------------------------------------------------

/// `(waiting-for-user-input-p)`
///
/// Batch-mode compatibility: always returns nil.
pub(crate) fn builtin_waiting_for_user_input_p(args: Vec<Value>) -> EvalResult {
    expect_args("waiting-for-user-input-p", &args, 0)?;
    Ok(Value::Nil)
}

// ---------------------------------------------------------------------------
// 15. y-or-n-p
// ---------------------------------------------------------------------------

/// `(y-or-n-p PROMPT)` currently returns EOF in batch mode.
pub(crate) fn builtin_y_or_n_p(_eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("y-or-n-p", &args, 1)?;
    match &args[0] {
        Value::Str(_) | Value::Vector(_) | Value::Nil => {}
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("sequencep"), *other],
            ));
        }
    }
    Err(signal(
        "end-of-file",
        vec![Value::string("Error reading from stdin")],
    ))
}

// ---------------------------------------------------------------------------
// 16. yes-or-no-p
// ---------------------------------------------------------------------------

/// `(yes-or-no-p PROMPT)` currently returns EOF in batch mode.
pub(crate) fn builtin_yes_or_no_p(
    _eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("yes-or-no-p", &args, 1)?;
    if !matches!(args[0], Value::Str(_)) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[0]],
        ));
    }
    Err(signal(
        "end-of-file",
        vec![Value::string("Error reading from stdin")],
    ))
}

// ---------------------------------------------------------------------------
// 17. read-char
// ---------------------------------------------------------------------------

/// `(read-char &optional PROMPT ...)`
///
/// Batch-mode: returns next unread character codepoint when available, otherwise nil.
pub(crate) fn builtin_read_char(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("read-char"), Value::Int(args.len() as i64)],
        ));
    }
    expect_optional_prompt_string(&args)?;
    let seconds_is_nil_or_omitted = args.get(2).is_none_or(Value::is_nil);
    if let Some(event) = eval.peek_unread_command_event() {
        if let Some(n) = event_to_int(&event) {
            let _ = eval.pop_unread_command_event();
            if eval.read_command_keys().is_empty() && seconds_is_nil_or_omitted {
                eval.set_read_command_keys(vec![event]);
            }
            return Ok(Value::Int(n));
        }
        eval.assign("unread-command-events", Value::list(vec![event]));
        eval.record_input_event(event);
        return Err(non_character_input_event_error());
    }
    Ok(Value::Nil)
}

/// `(read-key &optional PROMPT)`
///
/// Batch-mode: return next `unread-command-events` event when present, else nil.
pub(crate) fn builtin_read_key(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("read-key"), Value::Int(args.len() as i64)],
        ));
    }
    expect_optional_prompt_string(&args)?;
    if let Some(event) = eval.pop_unread_command_event() {
        eval.record_nonmenu_input_event(event);
        eval.set_read_command_keys(vec![event]);
        if let Some(n) = event_to_int(&event) {
            return Ok(Value::Int(n));
        }
        return Ok(event);
    }
    eval.clear_read_command_keys();
    Ok(Value::Nil)
}

// ---------------------------------------------------------------------------
// 18. read-key-sequence
// ---------------------------------------------------------------------------

/// `(read-key-sequence PROMPT)`
///
/// Batch-mode: consume one queued event and return it, or return empty string.
pub(crate) fn builtin_read_key_sequence(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("read-key-sequence", &args, 1)?;
    expect_max_args("read-key-sequence", &args, 6)?;
    expect_optional_prompt_string(&args)?;
    if let Some(event) = eval.pop_unread_command_event() {
        eval.record_nonmenu_input_event(event);
        eval.set_read_command_keys(vec![event]);
        if let Some(c) = event_to_char(&event) {
            return Ok(Value::string(c.to_string()));
        }
        return Ok(Value::vector(vec![event]));
    }
    eval.clear_read_command_keys();
    Ok(Value::string(""))
}

/// `(read-key-sequence-vector PROMPT)`
///
/// Batch mode: returns next `unread-command-events` event as a single-element
/// vector when present, otherwise an empty vector.
pub(crate) fn builtin_read_key_sequence_vector(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("read-key-sequence-vector", &args, 1)?;
    expect_max_args("read-key-sequence-vector", &args, 6)?;
    expect_optional_prompt_string(&args)?;
    if let Some(event) = eval.pop_unread_command_event() {
        eval.record_nonmenu_input_event(event);
        eval.set_read_command_keys(vec![event]);
        if let Some(n) = event_to_int(&event) {
            return Ok(Value::vector(vec![Value::Int(n)]));
        }
        return Ok(Value::vector(vec![event]));
    }
    eval.clear_read_command_keys();
    Ok(Value::vector(vec![]))
}

// ---------------------------------------------------------------------------
// 14. with-output-to-string (special form)
// ---------------------------------------------------------------------------

/// Special form: `(with-output-to-string BODY...)`
///
/// Evaluate BODY, capturing output from print functions into a temporary
/// buffer bound through `standard-output`.
pub(crate) fn sf_with_output_to_string(
    eval: &mut super::eval::Evaluator,
    tail: &[Expr],
) -> EvalResult {
    let temp_name = eval
        .buffers
        .generate_new_buffer_name(" *with-output-to-string*");
    let temp_id = eval.buffers.create_buffer(&temp_name);

    let mut frame = OrderedSymMap::new();
    frame.insert(intern("standard-output"), Value::Buffer(temp_id));
    eval.dynamic.push(frame);

    let body_result = eval.sf_progn(tail);
    let captured = eval
        .buffers
        .get(temp_id)
        .map(|buf| buf.buffer_string())
        .unwrap_or_default();

    let _ = eval.dynamic.pop();
    eval.buffers.kill_buffer(temp_id);

    body_result.map(|_| Value::string(captured))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "reader_test.rs"]
mod tests;
