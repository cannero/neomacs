//! JSON serialization and parsing builtins.
//!
//! Implements the Emacs JSON interface:
//! - `json-serialize` — convert Lisp value to JSON string
//! - `json-parse-string` — parse JSON string to Lisp value
//!
//! Key mapping (Emacs convention):
//! - Lisp nil / :null → JSON null
//! - Lisp t → JSON true
//! - Lisp :false / :json-false → JSON false
//! - Lisp integer/float → JSON number
//! - Lisp string → JSON string
//! - Lisp hash-table → JSON object
//! - Lisp vector → JSON array
//! - Lisp alist/plist → JSON object (when :object-type specifies)
//!
//! No external crate (serde_json etc.) is used — the parser and serializer
//! are implemented from scratch with simple recursive descent.

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::value::*;
use crate::buffer::BufferManager;

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Keyword argument parsing
// ---------------------------------------------------------------------------

/// Options that control how Lisp values are serialized to JSON.
#[derive(Clone, Debug)]
struct SerializeOpts {
    /// How nil is serialized.  In Emacs, `json-serialize` always maps nil to
    /// JSON `null`, but the keyword arg can override (:null-object).
    null_object: Value,
    /// The Lisp value that maps to JSON false.
    false_object: Value,
}

impl Default for SerializeOpts {
    fn default() -> Self {
        Self {
            null_object: Value::NIL,
            false_object: Value::keyword(":false"),
        }
    }
}

/// Options that control how a JSON string is parsed into Lisp values.
#[derive(Clone, Debug)]
struct ParseOpts {
    /// How JSON objects are represented.
    object_type: ObjectType,
    /// How JSON arrays are represented.
    array_type: ArrayType,
    /// Lisp value for JSON null.
    null_object: Value,
    /// Lisp value for JSON false.
    false_object: Value,
}

impl Default for ParseOpts {
    fn default() -> Self {
        Self {
            object_type: ObjectType::HashTable,
            array_type: ArrayType::Vector,
            null_object: Value::keyword(":null"),
            false_object: Value::keyword(":false"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum ObjectType {
    HashTable,
    Alist,
    Plist,
}

#[derive(Clone, Debug, PartialEq)]
enum ArrayType {
    Vector,
    List,
}

/// Parse keyword arguments from the &rest tail (starting at `start_index`).
/// Returns `ParseOpts`.  Unknown keywords signal `json-error`.
fn parse_parse_kwargs(args: &[Value], start_index: usize) -> Result<ParseOpts, Flow> {
    let mut opts = ParseOpts::default();
    let rest = &args[start_index..];
    if !rest.len().is_multiple_of(2) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("plistp"), Value::list(rest.to_vec())],
        ));
    }

    let mut i = 0;
    while i + 1 < rest.len() {
        let key = &rest[i];
        let value = &rest[i + 1];
        match key.kind() {
            ValueKind::Symbol(k) if resolve_sym(k) == ":object-type" => match value.kind() {
                ValueKind::Symbol(id) if resolve_sym(id) == "hash-table" => {
                    opts.object_type = ObjectType::HashTable
                }
                ValueKind::Symbol(id) if resolve_sym(id) == "alist" => {
                    opts.object_type = ObjectType::Alist
                }
                ValueKind::Symbol(id) if resolve_sym(id) == "plist" => {
                    opts.object_type = ObjectType::Plist
                }
                _ => {
                    return Err(signal(
                        "error",
                        vec![
                            Value::string("One of hash-table, alist or plist should be specified"),
                            *value,
                        ],
                    ));
                }
            },
            ValueKind::Symbol(k) if resolve_sym(k) == ":array-type" => match value.kind() {
                ValueKind::Symbol(id) if resolve_sym(id) == "array" => {
                    opts.array_type = ArrayType::Vector
                }
                ValueKind::Symbol(id) if resolve_sym(id) == "list" => {
                    opts.array_type = ArrayType::List
                }
                _ => {
                    return Err(signal(
                        "error",
                        vec![
                            Value::string("One of array or list should be specified"),
                            *value,
                        ],
                    ));
                }
            },
            ValueKind::Symbol(k) if resolve_sym(k) == ":null-object" => {
                opts.null_object = *value;
            }
            ValueKind::Symbol(k) if resolve_sym(k) == ":false-object" => {
                opts.false_object = *value;
            }
            _ => {
                return Err(signal(
                    "error",
                    vec![
                        Value::string(
                            "One of :object-type, :array-type, :null-object or :false-object should be specified",
                        ),
                        *value,
                    ],
                ));
            }
        }
        i += 2;
    }
    Ok(opts)
}

/// Parse keyword arguments relevant to `json-serialize` / `json-insert`.
fn parse_serialize_kwargs(args: &[Value], start_index: usize) -> Result<SerializeOpts, Flow> {
    let mut opts = SerializeOpts::default();
    let rest = &args[start_index..];
    if !rest.len().is_multiple_of(2) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("plistp"), Value::list(rest.to_vec())],
        ));
    }

    let mut i = 0;
    while i + 1 < rest.len() {
        let key = &rest[i];
        let value = &rest[i + 1];
        match key.kind() {
            ValueKind::Symbol(k) if resolve_sym(k) == ":null-object" => {
                opts.null_object = *value;
            }
            ValueKind::Symbol(k) if resolve_sym(k) == ":false-object" => {
                opts.false_object = *value;
            }
            _ => {
                return Err(signal(
                    "error",
                    vec![
                        Value::string("One of :null-object or :false-object should be specified"),
                        *value,
                    ],
                ));
            }
        }
        i += 2;
    }
    Ok(opts)
}

// ===========================================================================
// JSON Serializer (Lisp → JSON string)
// ===========================================================================

/// Check if two Values are equivalent for the purpose of matching the
/// null/false sentinel objects.
fn value_matches(a: &Value, b: &Value) -> bool {
    super::value::equal_value(a, b, 0)
}

/// Serialize a Lisp value to a JSON string.
fn serialize_to_json(value: &Value, opts: &SerializeOpts, depth: usize) -> Result<String, Flow> {
    if depth > 512 {
        return Err(signal(
            "json-serialize-error",
            vec![Value::string("Nesting too deep")],
        ));
    }

    // Check for null sentinel.
    if value_matches(value, &opts.null_object) {
        return Ok("null".to_string());
    }

    // Check for false sentinel.
    if value_matches(value, &opts.false_object) {
        return Ok("false".to_string());
    }

    match value.kind() {
        // t → true (checked after false sentinel, which is usually :false not t)
        ValueKind::T => Ok("true".to_string()),

        ValueKind::Fixnum(n) => Ok(n.to_string()),

        ValueKind::Float => {
            let f = value.xfloat();
            if f.is_nan() || f.is_infinite() {
                return Err(signal(
                    "json-serialize-error",
                    vec![Value::string("Not a finite number")],
                ));
            }
            // JSON numbers must not have trailing dot, use full representation.
            if f.fract() == 0.0 && f.abs() < (i64::MAX as f64) {
                // Emit as integer-looking float (e.g. 1.0 → 1.0, not 1)
                // Actually JSON allows both; Emacs json-serialize emits "1.0"
                // for float 1.0.  We follow that convention.
                Ok(format!("{:.1}", f))
            } else {
                Ok(format!("{}", f))
            }
        }

        ValueKind::String => {
            let string = value
                .as_lisp_string()
                .expect("ValueKind::String must carry LispString payload");
            let rendered = if string.is_multibyte() {
                string.as_utf8_str().ok_or_else(|| {
                    signal(
                        "wrong-type-argument",
                        vec![Value::symbol("json-value-p"), *value],
                    )
                })?
            } else {
                if !string.as_bytes().iter().all(u8::is_ascii) {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("json-value-p"), *value],
                    ));
                }
                std::str::from_utf8(string.as_bytes())
                    .expect("ASCII unibyte strings must be valid UTF-8")
            };
            Ok(json_encode_string(rendered))
        }

        ValueKind::Veclike(VecLikeType::Vector) => {
            let items = value.as_vector_data().unwrap().clone();
            let mut parts = Vec::with_capacity(items.len());
            for item in items.iter() {
                parts.push(serialize_to_json(item, opts, depth + 1)?);
            }
            Ok(format!("[{}]", parts.join(",")))
        }

        ValueKind::Veclike(VecLikeType::HashTable) => {
            let table = value.as_hash_table().unwrap().clone();
            let mut parts = Vec::with_capacity(table.data.len());
            for (key, val) in &table.data {
                let key_str = hash_key_to_string(key)?;
                let val_json = serialize_to_json(val, opts, depth + 1)?;
                parts.push(format!("{}:{}", json_encode_string(&key_str), val_json));
            }
            Ok(format!("{{{}}}", parts.join(",")))
        }

        // Alist: list of (KEY . VALUE) cons cells → JSON object.
        ValueKind::Cons => {
            let items = list_to_vec(value).ok_or_else(|| {
                signal("wrong-type-argument", vec![Value::symbol("listp"), *value])
            })?;

            let mut parts = Vec::with_capacity(items.len());
            for item in &items {
                match item.kind() {
                    ValueKind::Cons => {
                        let pair_car = item.cons_car();
                        let pair_cdr = item.cons_cdr();
                        let key_str = symbol_object_key(&pair_car)?;
                        let val_json = serialize_to_json(&pair_cdr, opts, depth + 1)?;
                        parts.push(format!("{}:{}", json_encode_string(&key_str), val_json));
                    }
                    _ => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("consp"), *item],
                        ));
                    }
                }
            }
            Ok(format!("{{{}}}", parts.join(",")))
        }

        // Nil was already checked as null_object above. If we reach here,
        // it means nil is NOT the null sentinel (user provided a custom
        // :null-object).  Emacs treats nil as JSON null regardless, but we
        // follow the sentinel logic.  Since nil is also an empty list, treat
        // it as an empty JSON object when it wasn't matched as null.
        ValueKind::Nil => Ok("null".to_string()),

        // Keywords that were not matched as false/null sentinels.
        ValueKind::Symbol(k) if resolve_sym(k) == ":json-false" => Ok("false".to_string()),
        ValueKind::Symbol(k)
            if {
                let n = resolve_sym(k);
                n == ":null" || n == ":json-null"
            } =>
        {
            Ok("null".to_string())
        }

        _ => Err(signal(
            "json-serialize-error",
            vec![Value::string(format!(
                "Value cannot be serialized to JSON: {}",
                super::print::print_value(value)
            ))],
        )),
    }
}

/// Convert a HashKey to a string suitable as a JSON object key.
fn hash_key_to_string(key: &HashKey) -> Result<String, Flow> {
    match key {
        HashKey::Text(s) => Ok(s.clone()),
        HashKey::Symbol(id) => Ok(resolve_sym(*id).to_owned()),
        HashKey::Keyword(id) => {
            let s = resolve_sym(*id);
            // Strip leading colon if present.
            if let Some(stripped) = s.strip_prefix(':') {
                Ok(stripped.to_string())
            } else {
                Ok(s.to_owned())
            }
        }
        HashKey::Int(n) => Ok(n.to_string()),
        HashKey::Nil => Ok("nil".to_string()),
        HashKey::True => Ok("t".to_string()),
        _ => Err(signal(
            "json-serialize-error",
            vec![Value::string(
                "Hash table key cannot be converted to JSON object key",
            )],
        )),
    }
}

/// Convert a Lisp value to a string key for a JSON object (used when
/// serializing alists).
///
/// Emacs `json-serialize` expects symbol keys in alists.
fn symbol_object_key(value: &Value) -> Result<String, Flow> {
    match value.kind() {
        ValueKind::Symbol(id) => Ok(resolve_sym(id).to_owned()),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), *value],
        )),
    }
}

/// Encode a Rust string as a JSON string with proper escaping.
fn json_encode_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0C' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                // Control characters: emit \u00XX.
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ===========================================================================
// JSON Parser (JSON string → Lisp value)
// ===========================================================================

fn json_utf8_decode_error(start: usize, end: usize) -> Flow {
    signal(
        "json-utf8-decode-error",
        vec![
            Value::fixnum(start as i64),
            Value::NIL,
            Value::fixnum(end as i64),
        ],
    )
}

fn json_source_char_pos(input: &crate::heap_types::LispString, byte_pos: usize) -> usize {
    if input.is_multibyte() {
        crate::emacs_core::emacs_char::byte_to_char_pos(input.as_bytes(), byte_pos)
    } else {
        byte_pos
    }
}

/// Parser state: a cursor over the input bytes.
struct JsonParser<'a> {
    input: &'a [u8],
    input_multibyte: bool,
    pos: usize,
    opts: ParseOpts,
}

impl<'a> JsonParser<'a> {
    fn new(input: &'a [u8], input_multibyte: bool, opts: ParseOpts) -> Self {
        Self {
            input,
            input_multibyte,
            pos: 0,
            opts,
        }
    }

    /// Current byte (or None if at end).
    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    /// Advance by one byte.
    fn advance(&mut self) {
        self.pos += 1;
    }

    fn source_char_pos(&self) -> usize {
        if self.input_multibyte {
            crate::emacs_core::emacs_char::byte_to_char_pos(self.input, self.pos)
        } else {
            self.pos
        }
    }

    /// Skip whitespace.
    fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
                self.advance();
            } else {
                break;
            }
        }
    }

    /// Consume a specific byte, or signal error.
    fn expect_byte(&mut self, expected: u8) -> Result<(), Flow> {
        self.skip_ws();
        match self.peek() {
            Some(b) if b == expected => {
                self.advance();
                Ok(())
            }
            Some(b) => Err(signal(
                "json-parse-error",
                vec![Value::string(format!(
                    "Expected '{}', got '{}' at position {}",
                    expected as char, b as char, self.pos
                ))],
            )),
            None => Err(signal(
                "json-parse-error",
                vec![Value::string(format!(
                    "Unexpected end of input, expected '{}'",
                    expected as char
                ))],
            )),
        }
    }

    /// Parse one JSON value.
    fn parse_value(&mut self) -> Result<Value, Flow> {
        self.skip_ws();
        match self.peek() {
            None => Err(signal(
                "json-parse-error",
                vec![Value::string("Unexpected end of input")],
            )),
            Some(b'"') => self.parse_string(),
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b't') => self.parse_true(),
            Some(b'f') => self.parse_false(),
            Some(b'n') => self.parse_null(),
            Some(b) if b == b'-' || b.is_ascii_digit() => self.parse_number(),
            Some(b) => Err(signal(
                "json-parse-error",
                vec![Value::string(format!(
                    "Unexpected character '{}' at position {}",
                    b as char, self.pos
                ))],
            )),
        }
    }

    /// Parse `true`.
    fn parse_true(&mut self) -> Result<Value, Flow> {
        self.expect_literal(b"true")?;
        Ok(Value::T)
    }

    /// Parse `false`.
    fn parse_false(&mut self) -> Result<Value, Flow> {
        self.expect_literal(b"false")?;
        Ok(self.opts.false_object)
    }

    /// Parse `null`.
    fn parse_null(&mut self) -> Result<Value, Flow> {
        self.expect_literal(b"null")?;
        Ok(self.opts.null_object)
    }

    /// Expect an exact byte sequence.
    fn expect_literal(&mut self, literal: &[u8]) -> Result<(), Flow> {
        for &expected in literal {
            match self.peek() {
                Some(b) if b == expected => self.advance(),
                _ => {
                    return Err(signal(
                        "json-parse-error",
                        vec![Value::string(format!(
                            "Expected '{}' at position {}",
                            std::str::from_utf8(literal).unwrap_or("?"),
                            self.pos
                        ))],
                    ));
                }
            }
        }
        Ok(())
    }

    /// Parse a JSON string (opening `"` has not been consumed yet).
    fn parse_string(&mut self) -> Result<Value, Flow> {
        let s = self.parse_string_raw()?;
        Ok(Value::string(s))
    }

    /// Parse a JSON string and return the raw Rust String.
    fn parse_string_raw(&mut self) -> Result<String, Flow> {
        self.expect_byte(b'"')?;
        let mut result = String::new();
        loop {
            match self.peek() {
                None => {
                    return Err(signal(
                        "json-end-of-file",
                        vec![
                            Value::fixnum(1),
                            Value::NIL,
                            Value::fixnum(self.source_char_pos() as i64),
                        ],
                    ));
                }
                Some(b'"') => {
                    self.advance();
                    return Ok(result);
                }
                Some(b'\\') => {
                    self.advance();
                    match self.peek() {
                        Some(b'"') => {
                            self.advance();
                            result.push('"');
                        }
                        Some(b'\\') => {
                            self.advance();
                            result.push('\\');
                        }
                        Some(b'/') => {
                            self.advance();
                            result.push('/');
                        }
                        Some(b'n') => {
                            self.advance();
                            result.push('\n');
                        }
                        Some(b'r') => {
                            self.advance();
                            result.push('\r');
                        }
                        Some(b't') => {
                            self.advance();
                            result.push('\t');
                        }
                        Some(b'b') => {
                            self.advance();
                            result.push('\x08');
                        }
                        Some(b'f') => {
                            self.advance();
                            result.push('\x0C');
                        }
                        Some(b'u') => {
                            self.advance();
                            let cp = self.parse_unicode_escape()?;
                            // Handle UTF-16 surrogate pairs.
                            if (0xD800..=0xDBFF).contains(&cp) {
                                // High surrogate — expect \uXXXX low surrogate.
                                if self.peek() == Some(b'\\') {
                                    self.advance();
                                    if self.peek() == Some(b'u') {
                                        self.advance();
                                        let low = self.parse_unicode_escape()?;
                                        if (0xDC00..=0xDFFF).contains(&low) {
                                            let combined = 0x10000
                                                + ((cp as u32 - 0xD800) << 10)
                                                + (low as u32 - 0xDC00);
                                            if let Some(c) = char::from_u32(combined) {
                                                result.push(c);
                                            } else {
                                                result.push(char::REPLACEMENT_CHARACTER);
                                            }
                                        } else {
                                            result.push(char::REPLACEMENT_CHARACTER);
                                            result.push(char::REPLACEMENT_CHARACTER);
                                        }
                                    } else {
                                        result.push(char::REPLACEMENT_CHARACTER);
                                    }
                                } else {
                                    result.push(char::REPLACEMENT_CHARACTER);
                                }
                            } else if let Some(c) = char::from_u32(cp as u32) {
                                result.push(c);
                            } else {
                                result.push(char::REPLACEMENT_CHARACTER);
                            }
                        }
                        Some(b) => {
                            return Err(signal(
                                "json-parse-error",
                                vec![Value::string(format!(
                                    "Invalid escape '\\{}' at position {}",
                                    b as char, self.pos
                                ))],
                            ));
                        }
                        None => {
                            return Err(signal(
                                "json-end-of-file",
                                vec![
                                    Value::fixnum(1),
                                    Value::NIL,
                                    Value::fixnum(self.source_char_pos() as i64),
                                ],
                            ));
                        }
                    }
                }
                Some(b) => {
                    if b < 0x80 {
                        self.advance();
                        result.push(b as char);
                    } else {
                        let start = self.pos;
                        let seq_len = match b {
                            0xC2..=0xDF => 2,
                            0xE0..=0xEF => 3,
                            0xF0..=0xF4 => 4,
                            _ => {
                                return Err(json_utf8_decode_error(
                                    start,
                                    (start + 2).min(self.input.len()),
                                ));
                            }
                        };
                        let end = (start + seq_len).min(self.input.len());
                        let seq = self.input.get(start..end).ok_or_else(|| {
                            json_utf8_decode_error(start, (start + seq_len).min(self.input.len()))
                        })?;
                        let decoded = std::str::from_utf8(seq)
                            .ok()
                            .and_then(|s| {
                                let mut chars = s.chars();
                                let ch = chars.next()?;
                                chars.next().is_none().then_some(ch)
                            })
                            .ok_or_else(|| json_utf8_decode_error(start, end))?;
                        self.pos = end;
                        result.push(decoded);
                    }
                }
            }
        }
    }

    /// Parse 4 hex digits for a \uXXXX escape.
    fn parse_unicode_escape(&mut self) -> Result<u16, Flow> {
        let mut value: u16 = 0;
        for _ in 0..4 {
            match self.peek() {
                Some(b) if b.is_ascii_hexdigit() => {
                    self.advance();
                    let digit = match b {
                        b'0'..=b'9' => (b - b'0') as u16,
                        b'a'..=b'f' => (b - b'a' + 10) as u16,
                        b'A'..=b'F' => (b - b'A' + 10) as u16,
                        _ => unreachable!(),
                    };
                    value = value * 16 + digit;
                }
                _ => {
                    return Err(signal(
                        "json-parse-error",
                        vec![Value::string(format!(
                            "Invalid unicode escape at position {}",
                            self.pos
                        ))],
                    ));
                }
            }
        }
        Ok(value)
    }

    /// Parse a JSON number.
    fn parse_number(&mut self) -> Result<Value, Flow> {
        let start = self.pos;
        let mut is_float = false;

        // Optional leading minus.
        if self.peek() == Some(b'-') {
            self.advance();
        }

        // Integer part.
        match self.peek() {
            Some(b'0') => {
                self.advance();
            }
            Some(b) if (b'1'..=b'9').contains(&b) => {
                self.advance();
                while let Some(b) = self.peek() {
                    if b.is_ascii_digit() {
                        self.advance();
                    } else {
                        break;
                    }
                }
            }
            _ => {
                return Err(signal(
                    "json-parse-error",
                    vec![Value::string(format!(
                        "Invalid number at position {}",
                        self.pos
                    ))],
                ));
            }
        }

        // Fractional part.
        if self.peek() == Some(b'.') {
            is_float = true;
            self.advance();
            let frac_start = self.pos;
            while let Some(b) = self.peek() {
                if b.is_ascii_digit() {
                    self.advance();
                } else {
                    break;
                }
            }
            if self.pos == frac_start {
                return Err(signal(
                    "json-parse-error",
                    vec![Value::string(format!(
                        "Expected digit after decimal point at position {}",
                        self.pos
                    ))],
                ));
            }
        }

        // Exponent part.
        if let Some(b'e') | Some(b'E') = self.peek() {
            is_float = true;
            self.advance();
            if let Some(b'+') | Some(b'-') = self.peek() {
                self.advance();
            }
            let exp_start = self.pos;
            while let Some(b) = self.peek() {
                if b.is_ascii_digit() {
                    self.advance();
                } else {
                    break;
                }
            }
            if self.pos == exp_start {
                return Err(signal(
                    "json-parse-error",
                    vec![Value::string(format!(
                        "Expected digit in exponent at position {}",
                        self.pos
                    ))],
                ));
            }
        }

        let num_str = std::str::from_utf8(&self.input[start..self.pos]).unwrap_or("0");

        if is_float {
            let f: f64 = num_str.parse().map_err(|_| {
                signal(
                    "json-parse-error",
                    vec![Value::string(format!("Invalid float: {}", num_str))],
                )
            })?;
            Ok(Value::make_float(f))
        } else {
            // Try parsing as i64 first; fall back to f64 for very large numbers.
            match num_str.parse::<i64>() {
                Ok(n) => Ok(Value::fixnum(n)),
                Err(_) => {
                    let f: f64 = num_str.parse().map_err(|_| {
                        signal(
                            "json-parse-error",
                            vec![Value::string(format!("Invalid number: {}", num_str))],
                        )
                    })?;
                    Ok(Value::make_float(f)) // TODO(tagged): remove next_float_id()
                }
            }
        }
    }

    /// Parse a JSON array: `[` value { `,` value } `]`.
    fn parse_array(&mut self) -> Result<Value, Flow> {
        self.expect_byte(b'[')?;
        self.skip_ws();
        let mut items: Vec<Value> = Vec::new();

        if self.peek() == Some(b']') {
            self.advance();
        } else {
            loop {
                let val = self.parse_value()?;
                items.push(val);
                self.skip_ws();
                match self.peek() {
                    Some(b',') => {
                        self.advance();
                    }
                    Some(b']') => {
                        self.advance();
                        break;
                    }
                    _ => {
                        return Err(signal(
                            "json-parse-error",
                            vec![Value::string(format!(
                                "Expected ',' or ']' at position {}",
                                self.pos
                            ))],
                        ));
                    }
                }
            }
        }

        match self.opts.array_type {
            ArrayType::Vector => Ok(Value::vector(items)),
            ArrayType::List => Ok(Value::list(items)),
        }
    }

    /// Parse a JSON object: `{` string `:` value { `,` string `:` value } `}`.
    fn parse_object(&mut self) -> Result<Value, Flow> {
        self.expect_byte(b'{')?;
        self.skip_ws();

        match self.opts.object_type {
            ObjectType::HashTable => self.parse_object_hash_table(),
            ObjectType::Alist => self.parse_object_alist(),
            ObjectType::Plist => self.parse_object_plist(),
        }
    }

    fn parse_object_hash_table(&mut self) -> Result<Value, Flow> {
        let ht = Value::hash_table(HashTableTest::Equal);
        if self.peek() == Some(b'}') {
            self.advance();
            return Ok(ht);
        }

        loop {
            self.skip_ws();
            let key = self.parse_string_raw()?;
            self.expect_byte(b':')?;
            let val = self.parse_value()?;

            {
                let key_val = Value::string(&key);
                let hash_key = HashKey::Text(key);
                let _ = ht.with_hash_table_mut(|table| {
                    let inserting_new_key = !table.data.contains_key(&hash_key);
                    table.data.insert(hash_key.clone(), val);
                    if inserting_new_key {
                        table.key_snapshots.insert(hash_key.clone(), key_val);
                        table.insertion_order.push(hash_key);
                    }
                });
            }

            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.advance();
                }
                Some(b'}') => {
                    self.advance();
                    break;
                }
                _ => {
                    return Err(signal(
                        "json-parse-error",
                        vec![Value::string(format!(
                            "Expected ',' or '}}' at position {}",
                            self.pos
                        ))],
                    ));
                }
            }
        }

        Ok(ht)
    }

    fn parse_object_alist(&mut self) -> Result<Value, Flow> {
        let mut pairs: Vec<Value> = Vec::new();

        if self.peek() == Some(b'}') {
            self.advance();
            return Ok(Value::NIL);
        }

        loop {
            self.skip_ws();
            let key = self.parse_string_raw()?;
            self.expect_byte(b':')?;
            let val = self.parse_value()?;

            pairs.push(Value::cons(Value::symbol(key), val));

            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.advance();
                }
                Some(b'}') => {
                    self.advance();
                    break;
                }
                _ => {
                    return Err(signal(
                        "json-parse-error",
                        vec![Value::string(format!(
                            "Expected ',' or '}}' at position {}",
                            self.pos
                        ))],
                    ));
                }
            }
        }

        Ok(Value::list(pairs))
    }

    fn parse_object_plist(&mut self) -> Result<Value, Flow> {
        let mut items: Vec<Value> = Vec::new();

        if self.peek() == Some(b'}') {
            self.advance();
            return Ok(Value::NIL);
        }

        loop {
            self.skip_ws();
            let key = self.parse_string_raw()?;
            self.expect_byte(b':')?;
            let val = self.parse_value()?;

            // Plist keys are keywords (symbols with leading colon).
            items.push(Value::keyword(format!(":{}", key)));
            items.push(val);

            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.advance();
                }
                Some(b'}') => {
                    self.advance();
                    break;
                }
                _ => {
                    return Err(signal(
                        "json-parse-error",
                        vec![Value::string(format!(
                            "Expected ',' or '}}' at position {}",
                            self.pos
                        ))],
                    ));
                }
            }
        }

        Ok(Value::list(items))
    }
}

// ===========================================================================
// Public builtin functions
// ===========================================================================

/// `(json-serialize VALUE &rest ARGS)` — serialize a Lisp value to a JSON string.
///
/// ARGS are keyword arguments:
/// - `:null-object VALUE` — Lisp value to serialize as JSON null (default: nil)
/// - `:false-object VALUE` — Lisp value to serialize as JSON false (default: :false)
pub(crate) fn builtin_json_serialize(args: Vec<Value>) -> EvalResult {
    expect_min_args("json-serialize", &args, 1)?;
    let opts = parse_serialize_kwargs(&args, 1)?;
    let json = serialize_to_json(&args[0], &opts, 0)?;
    Ok(Value::string(json))
}

/// `(json-parse-string STRING &rest ARGS)` — parse a JSON string into a Lisp value.
///
/// ARGS are keyword arguments:
/// - `:object-type SYMBOL` — `hash-table` (default), `alist`, or `plist`
/// - `:array-type SYMBOL` — `array` (default, yields vector) or `list`
/// - `:null-object VALUE` — Lisp value for JSON null (default: :null)
/// - `:false-object VALUE` — Lisp value for JSON false (default: :false)
pub(crate) fn builtin_json_parse_string(args: Vec<Value>) -> EvalResult {
    expect_min_args("json-parse-string", &args, 1)?;
    let input = match args[0].kind() {
        ValueKind::String => args[0]
            .as_lisp_string()
            .expect("string object must carry LispString payload"),
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), args[0]],
            ));
        }
    };
    let opts = parse_parse_kwargs(&args, 1)?;
    let mut parser = JsonParser::new(input.as_bytes(), input.is_multibyte(), opts);
    parser.skip_ws();
    if parser.pos >= parser.input.len() {
        let p = json_source_char_pos(&input, parser.pos) as i64;
        return Err(signal(
            "json-end-of-file",
            vec![Value::fixnum(1), Value::NIL, Value::fixnum(p)],
        ));
    }
    let result = parser.parse_value()?;

    // Ensure there is no trailing non-whitespace.
    parser.skip_ws();
    if parser.pos < parser.input.len() {
        let p = json_source_char_pos(&input, parser.pos) as i64 + 1;
        return Err(signal(
            "json-trailing-content",
            vec![Value::fixnum(1), Value::NIL, Value::fixnum(p)],
        ));
    }

    Ok(result)
}

/// `(json-parse-buffer &rest ARGS)` — parse one JSON value from point.
///
/// Unlike `json-parse-string`, this parses a single JSON value starting at the
/// current point (after leading whitespace), leaves trailing buffer content
/// untouched, and advances point to just after the parsed value.
pub(crate) fn builtin_json_parse_buffer(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let opts = parse_parse_kwargs(&args, 0)?;
    let (input, point_base) = {
        let buf = eval
            .buffers
            .current_buffer()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        let input = buf.buffer_substring_lisp_string(buf.point(), buf.point_max());
        (input, buf.point())
    };

    let mut parser = JsonParser::new(input.as_bytes(), input.is_multibyte(), opts);
    parser.skip_ws();
    if parser.pos >= parser.input.len() {
        let p = json_source_char_pos(&input, parser.pos) as i64;
        return Err(signal(
            "json-end-of-file",
            vec![Value::fixnum(1), Value::NIL, Value::fixnum(p)],
        ));
    }

    let result = parser.parse_value()?;
    let new_point = point_base + parser.pos;
    if let Some(current_id) = eval.buffers.current_buffer_id() {
        let _ = eval.buffers.goto_buffer_byte(current_id, new_point);
    }
    Ok(result)
}

/// `(json-insert VALUE &rest ARGS)` — insert JSON text at point.
///
/// Keyword arguments mirror `json-serialize` (`:null-object`, `:false-object`).
pub(crate) fn builtin_json_insert(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("json-insert", &args, 1)?;
    let opts = parse_serialize_kwargs(&args, 1)?;
    let json = serialize_to_json(&args[0], &opts, 0)?;
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let insert_pos = eval.buffers.get(current_id).map(|b| b.pt_byte).unwrap_or(0);
    let json_len = json.len();
    super::editfns::signal_before_change(eval, insert_pos, insert_pos)?;
    let _ = eval.buffers.insert_into_buffer(current_id, &json);
    super::editfns::signal_after_change(eval, insert_pos, insert_pos + json_len, 0)?;
    Ok(Value::NIL)
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
#[path = "json_test.rs"]
mod tests;
