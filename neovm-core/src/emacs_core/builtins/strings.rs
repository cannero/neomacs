use super::*;

// ===========================================================================
// String operations
// ===========================================================================

pub(crate) fn builtin_string_equal(args: Vec<Value>) -> EvalResult {
    expect_args("string-equal", &args, 2)?;
    let a = expect_string_comparison_operand(&args[0])?;
    let b = expect_string_comparison_operand(&args[1])?;
    Ok(Value::bool(a == b))
}

pub(crate) fn builtin_string_lessp(args: Vec<Value>) -> EvalResult {
    expect_args("string-lessp", &args, 2)?;
    let a = expect_string_comparison_operand(&args[0])?;
    let b = expect_string_comparison_operand(&args[1])?;
    Ok(Value::bool(a < b))
}

pub(crate) fn builtin_string_greaterp(args: Vec<Value>) -> EvalResult {
    expect_args("string-greaterp", &args, 2)?;
    let a = expect_string_comparison_operand(&args[0])?;
    let b = expect_string_comparison_operand(&args[1])?;
    Ok(Value::bool(a > b))
}

fn substring_impl(name: &str, args: &[Value], preserve_props: bool) -> EvalResult {
    expect_min_args(name, args, 1)?;
    expect_max_args(name, args, 3)?;
    match &args[0] {
        Value::Str(src_id) => {
            let src_props = if preserve_props {
                get_string_text_properties_table(*src_id).filter(|table| !table.is_empty())
            } else {
                None
            };
            let (result, sliced_props) = with_heap(|h| {
                let src = h.get_lisp_string(*src_id);
                let s = src.as_str();
                let normalize_index =
                    |value: &Value, default: i64, len: i64| -> Result<i64, Flow> {
                        let raw = if value.is_nil() {
                            default
                        } else {
                            expect_int(value)?
                        };
                        let idx = if raw < 0 { len + raw } else { raw };
                        if idx < 0 || idx > len {
                            return Err(signal(
                                "args-out-of-range",
                                vec![args[0], args[1], args.get(2).cloned().unwrap_or(Value::Nil)],
                            ));
                        }
                        Ok(idx)
                    };

                if src_props.is_none() && s.is_ascii() {
                    let len = s.len() as i64;
                    let from = if args.len() > 1 {
                        normalize_index(&args[1], 0, len)?
                    } else {
                        0
                    } as usize;
                    let to = if args.len() > 2 {
                        normalize_index(&args[2], len, len)?
                    } else {
                        len
                    } as usize;
                    if from > to {
                        return Err(signal(
                            "args-out-of-range",
                            vec![
                                args[0],
                                args.get(1).cloned().unwrap_or(Value::Int(0)),
                                args.get(2).cloned().unwrap_or(Value::Nil),
                            ],
                        ));
                    }
                    return Ok::<_, Flow>((
                        src.slice(from, to).expect("validated ascii slice"),
                        None,
                    ));
                }

                let len = storage_char_len(s) as i64;
                let from = if args.len() > 1 {
                    normalize_index(&args[1], 0, len)?
                } else {
                    0
                } as usize;

                let to = if args.len() > 2 {
                    normalize_index(&args[2], len, len)?
                } else {
                    len
                } as usize;

                if from > to {
                    return Err(signal(
                        "args-out-of-range",
                        vec![
                            args[0],
                            args.get(1).cloned().unwrap_or(Value::Int(0)),
                            args.get(2).cloned().unwrap_or(Value::Nil),
                        ],
                    ));
                }
                let (byte_from, byte_to) = super::super::string_escape::storage_substring_bounds(
                    s, from, to,
                )
                .ok_or_else(|| {
                    signal(
                        "args-out-of-range",
                        vec![
                            args[0],
                            args.get(1).cloned().unwrap_or(Value::Int(0)),
                            args.get(2).cloned().unwrap_or(Value::Nil),
                        ],
                    )
                })?;
                let result = src
                    .slice(byte_from, byte_to)
                    .expect("validated storage substring bounds");
                let sliced_props = if let Some(src_table) = src_props.as_ref() {
                    let sliced = src_table.slice(byte_from, byte_to);
                    (!sliced.is_empty()).then_some(sliced)
                } else {
                    None
                };
                Ok::<_, Flow>((result, sliced_props))
            })?;
            let new_val = Value::heap_string(result);

            // Preserve text properties from source string
            if let (true, Value::Str(new_id), Some(sliced)) =
                (preserve_props, &new_val, sliced_props)
            {
                set_string_text_properties_table(*new_id, sliced);
            }

            Ok(new_val)
        }
        Value::Vector(v) | Value::Record(v) if name == "substring" => {
            let items = with_heap(|h| h.get_vector(*v).clone());
            let len = items.len() as i64;
            let normalize_index = |value: &Value, default: i64| -> Result<i64, Flow> {
                let raw = if value.is_nil() {
                    default
                } else {
                    expect_int(value)?
                };
                let idx = if raw < 0 { len + raw } else { raw };
                if idx < 0 || idx > len {
                    return Err(signal(
                        "args-out-of-range",
                        vec![args[0], args[1], args.get(2).cloned().unwrap_or(Value::Nil)],
                    ));
                }
                Ok(idx)
            };
            let from = if args.len() > 1 {
                normalize_index(&args[1], 0)?
            } else {
                0
            } as usize;
            let to = if args.len() > 2 {
                normalize_index(&args[2], len)?
            } else {
                len
            } as usize;
            if from > to {
                return Err(signal(
                    "args-out-of-range",
                    vec![
                        args[0],
                        args.get(1).cloned().unwrap_or(Value::Int(0)),
                        args.get(2).cloned().unwrap_or(Value::Nil),
                    ],
                ));
            }
            Ok(Value::vector(items[from..to].to_vec()))
        }
        _ => {
            let s = expect_string(&args[0])?;
            let _ = s;
            unreachable!("expect_string either returns a string or signals")
        }
    }
}

pub(crate) fn builtin_substring(args: Vec<Value>) -> EvalResult {
    crate::emacs_core::perf_trace::time_op(
        crate::emacs_core::perf_trace::HotpathOp::Substring,
        || substring_impl("substring", &args, true),
    )
}

pub(crate) fn builtin_substring_no_properties(args: Vec<Value>) -> EvalResult {
    crate::emacs_core::perf_trace::time_op(
        crate::emacs_core::perf_trace::HotpathOp::Substring,
        || substring_impl("substring-no-properties", &args, false),
    )
}

pub(crate) fn builtin_concat(args: Vec<Value>) -> EvalResult {
    crate::emacs_core::perf_trace::time_op(crate::emacs_core::perf_trace::HotpathOp::Concat, || {
        fn push_concat_int(result: &mut String, n: i64) -> Result<(), Flow> {
            if !(0..=0x3FFFFF).contains(&n) {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("characterp"), Value::Int(n)],
                ));
            }

            let cp = n as u32;
            if let Some(c) = char::from_u32(cp) {
                result.push(c);
                return Ok(());
            }

            // Emacs concat path for raw-byte non-Unicode chars uses byte->multibyte encoding.
            if (0x3FFF00..=0x3FFFFF).contains(&cp) {
                let b = (cp - 0x3FFF00) as u8;
                let bytes = if b < 0x80 {
                    vec![b]
                } else {
                    vec![0xC0 | ((b >> 6) & 0x01), 0x80 | (b & 0x3F)]
                };
                result.push_str(&bytes_to_storage_string(&bytes));
                return Ok(());
            }

            if let Some(encoded) = encode_nonunicode_char_for_storage(cp) {
                result.push_str(&encoded);
                return Ok(());
            }

            Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), Value::Int(n)],
            ))
        }

        fn push_concat_element(result: &mut String, value: &Value) -> Result<(), Flow> {
            match value {
                Value::Char(c) => {
                    result.push(*c);
                    Ok(())
                }
                Value::Int(n) => push_concat_int(result, *n),
                other => Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("characterp"), *other],
                )),
            }
        }

        if args.iter().all(|arg| matches!(arg, Value::Str(_))) {
            let has_text_props = args.iter().any(|arg| {
                matches!(
                    arg,
                    Value::Str(id)
                        if get_string_text_properties_table(*id)
                            .is_some_and(|table| !table.is_empty())
                )
            });
            if !has_text_props {
                let result = with_heap(|h| {
                    let mut parts = Vec::new();
                    let mut multibyte = false;
                    for arg in &args {
                        if let Value::Str(id) = arg {
                            let string = h.get_lisp_string(*id);
                            string.append_parts_to(&mut parts);
                            multibyte |= string.multibyte;
                        }
                    }
                    crate::gc::types::LispString::from_parts(parts, multibyte)
                });
                return Ok(Value::heap_string(result));
            }
        }

        let preallocated_len = args.iter().fold(0usize, |acc, arg| match arg {
            Value::Str(id) => acc + with_heap(|h| h.get_string(*id).len()),
            _ => acc,
        });
        let mut result = String::with_capacity(preallocated_len);
        // Track string sources with their byte offsets for property preservation
        let mut string_sources: Vec<(crate::gc::types::ObjId, usize)> = Vec::new();

        for arg in &args {
            match arg {
                Value::Str(id) => {
                    let offset = result.len();
                    with_heap(|h| result.push_str(h.get_string(*id)));
                    string_sources.push((*id, offset));
                }
                Value::Nil => {}
                Value::Cons(_) => {
                    let mut cursor = *arg;
                    loop {
                        match cursor {
                            Value::Nil => break,
                            Value::Cons(cell) => {
                                let pair = read_cons(cell);
                                push_concat_element(&mut result, &pair.car)?;
                                cursor = pair.cdr;
                            }
                            tail => {
                                return Err(signal(
                                    "wrong-type-argument",
                                    vec![Value::symbol("listp"), tail],
                                ));
                            }
                        }
                    }
                }
                Value::Vector(v) => {
                    let items = with_heap(|h| h.get_vector(*v).clone());
                    for item in items.iter() {
                        push_concat_element(&mut result, item)?;
                    }
                }
                _ => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("sequencep"), *arg],
                    ));
                }
            }
        }

        let new_val = Value::string(&result);

        // Preserve text properties from string sources
        if let Value::Str(new_id) = &new_val {
            let mut combined_table = crate::buffer::text_props::TextPropertyTable::new();
            let mut has_props = false;
            for (src_id, offset) in &string_sources {
                if let Some(src_table) = get_string_text_properties_table(*src_id) {
                    if !src_table.is_empty() {
                        combined_table.append_shifted(&src_table, *offset);
                        has_props = true;
                    }
                }
            }
            if has_props {
                set_string_text_properties_table(*new_id, combined_table);
            }
        }

        Ok(new_val)
    })
}

pub(crate) fn builtin_string_to_number(args: Vec<Value>) -> EvalResult {
    expect_min_args("string-to-number", &args, 1)?;
    expect_max_args("string-to-number", &args, 2)?;
    let s = expect_string(&args[0])?;
    let base = if args.len() > 1 {
        expect_int(&args[1])?
    } else {
        10
    };

    if !(2..=16).contains(&base) {
        return Err(signal("args-out-of-range", vec![Value::Int(base)]));
    }

    let s = s.trim_start_matches(|c: char| c == ' ' || c == '\t');
    if base == 10 {
        // Match GNU Emacs's string_to_number float detection rules:
        // A number is float if it has digits after the decimal point (TRAIL_INT)
        // OR if it has leading digits and an exponent (LEAD_INT & E_EXP).
        // "100." is integer (no trailing digits), "100.0" is float, "1e10" is float.
        let number_prefix =
            Regex::new(r"^[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?")
                .expect("number prefix regexp should compile");
        if let Some(m) = number_prefix.find(s) {
            let token = m.as_str();
            // Emacs float_syntax: trail_int || (lead_int && e_exp)
            // trail_int = has digits after '.'
            // e_exp = has 'e'/'E' exponent
            let has_trail_int = if let Some(dot_pos) = token.find('.') {
                token[dot_pos + 1..]
                    .chars()
                    .next()
                    .map_or(false, |c| c.is_ascii_digit())
            } else {
                false
            };
            let has_e_exp = token.contains('e') || token.contains('E');
            let has_lead_int = token
                .trim_start_matches(['+', '-'])
                .starts_with(|c: char| c.is_ascii_digit());
            let is_float = has_trail_int || (has_lead_int && has_e_exp);
            if is_float {
                if let Ok(f) = token.parse::<f64>() {
                    return Ok(Value::Float(f, next_float_id()));
                }
            } else {
                // Parse integer part only (stop at dot if present)
                let int_token = if let Some(dot_pos) = token.find('.') {
                    &token[..dot_pos]
                } else {
                    token
                };
                if let Ok(n) = int_token.parse::<i64>() {
                    return Ok(Value::Int(n));
                }
            }
        }
    } else {
        let bytes = s.as_bytes();
        let mut pos = 0usize;
        let mut negative = false;
        if pos < bytes.len() {
            if bytes[pos] == b'+' {
                pos += 1;
            } else if bytes[pos] == b'-' {
                negative = true;
                pos += 1;
            }
        }
        let digit_start = pos;
        while pos < bytes.len() {
            let ch = bytes[pos] as char;
            let Some(d) = ch.to_digit(36) else { break };
            if (d as i64) < base {
                pos += 1;
            } else {
                break;
            }
        }
        if pos > digit_start {
            let token = &s[digit_start..pos];
            if let Ok(parsed) = i64::from_str_radix(token, base as u32) {
                return Ok(Value::Int(if negative { -parsed } else { parsed }));
            }
        }
    }
    Ok(Value::Int(0))
}

pub(crate) fn builtin_number_to_string(args: Vec<Value>) -> EvalResult {
    expect_args("number-to-string", &args, 1)?;
    match &args[0] {
        Value::Int(n) => Ok(Value::string(n.to_string())),
        Value::Float(f, _) => Ok(Value::string(super::print::format_float(*f))),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("numberp"), *other],
        )),
    }
}

pub(crate) fn builtin_upcase(args: Vec<Value>) -> EvalResult {
    expect_args("upcase", &args, 1)?;
    match &args[0] {
        Value::Str(id) => Ok(Value::string(upcase_string_emacs_compat(&with_heap(|h| {
            h.get_string(*id).to_owned()
        })))),
        Value::Char(c) => {
            let mapped = upcase_char_code_emacs_compat(*c as i64);
            if let Some(ch) = u32::try_from(mapped).ok().and_then(char::from_u32) {
                Ok(Value::Char(ch))
            } else {
                Ok(Value::Char(*c))
            }
        }
        Value::Int(n) => {
            if *n < 0 {
                Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("char-or-string-p"), Value::Int(*n)],
                ))
            } else {
                Ok(Value::Int(upcase_char_code_emacs_compat(*n)))
            }
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("char-or-string-p"), *other],
        )),
    }
}

fn preserve_emacs_upcase_payload(code: i64) -> bool {
    matches!(
        code,
        305
            | 329
            | 383
            | 411
            | 496
            | 612
            | 912
            | 944
            | 1415
            | 7306
            | 7830..=7834
            | 8016
            | 8018
            | 8020
            | 8022
            | 8072..=8079
            | 8088..=8095
            | 8104..=8111
            | 8114
            | 8116
            | 8118..=8119
            | 8124
            | 8130
            | 8132
            | 8134..=8135
            | 8140
            | 8146..=8147
            | 8150..=8151
            | 8162..=8164
            | 8166..=8167
            | 8178
            | 8180
            | 8182..=8183
            | 8188
            | 42957
            | 42959
            | 42963
            | 42965
            | 42971
            | 64256..=64262
            | 64275..=64279
            | 68976..=68997
            | 93883..=93907
    )
}

fn upcase_string_emacs_compat(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        let code = ch as i64;
        if ch == '\u{0131}' || preserve_emacs_upcase_string_payload(code) {
            out.push(ch);
            continue;
        }
        for up in ch.to_uppercase() {
            out.push(up);
        }
    }
    out
}

fn upcase_char_code_emacs_compat(code: i64) -> i64 {
    if preserve_emacs_upcase_payload(code) {
        return code;
    }
    match code {
        223 => 7838,
        8064..=8071 | 8080..=8087 | 8096..=8103 => code + 8,
        8115 | 8131 | 8179 => code + 9,
        _ => {
            if let Some(c) = u32::try_from(code).ok().and_then(char::from_u32) {
                c.to_uppercase().next().unwrap_or(c) as i64
            } else {
                code
            }
        }
    }
}

fn preserve_emacs_upcase_string_payload(code: i64) -> bool {
    matches!(
        code,
        411
            | 612
            | 7306
            | 42957
            | 42959
            | 42963
            | 42965
            | 42971
            | 68976..=68997
            | 93883..=93907
    )
}

fn preserve_emacs_downcase_payload(code: i64) -> bool {
    matches!(
        code,
        304
            | 7305
            | 8490
            | 42955
            | 42956
            | 42958
            | 42962
            | 42964
            | 42970
            | 42972
            | 68944..=68965
            | 93856..=93880
    )
}

pub(super) fn downcase_char_code_emacs_compat(code: i64) -> i64 {
    if preserve_emacs_downcase_payload(code) {
        return code;
    }
    if let Some(c) = u32::try_from(code).ok().and_then(char::from_u32) {
        c.to_lowercase().next().unwrap_or(c) as i64
    } else {
        code
    }
}

pub(crate) fn builtin_downcase(args: Vec<Value>) -> EvalResult {
    expect_args("downcase", &args, 1)?;
    match &args[0] {
        Value::Str(id) => Ok(Value::string(downcase_string_emacs_compat(&with_heap(
            |h| h.get_string(*id).to_owned(),
        )))),
        Value::Char(c) => {
            let mapped = downcase_char_code_emacs_compat(*c as i64);
            if let Some(ch) = u32::try_from(mapped).ok().and_then(char::from_u32) {
                Ok(Value::Char(ch))
            } else {
                Ok(Value::Char(*c))
            }
        }
        Value::Int(n) => {
            if *n < 0 {
                Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("char-or-string-p"), Value::Int(*n)],
                ))
            } else {
                Ok(Value::Int(downcase_char_code_emacs_compat(*n)))
            }
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("char-or-string-p"), *other],
        )),
    }
}

fn downcase_string_emacs_compat(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        let code = ch as i64;
        if ch == '\u{212A}' || preserve_emacs_downcase_string_payload(code) {
            out.push(ch);
            continue;
        }
        for low in ch.to_lowercase() {
            out.push(low);
        }
    }
    out
}

fn preserve_emacs_downcase_string_payload(code: i64) -> bool {
    matches!(
        code,
        7305
            | 42955
            | 42956
            | 42958
            | 42962
            | 42964
            | 42970
            | 42972
            | 68944..=68965
            | 93856..=93880
    )
}

pub(crate) fn builtin_ngettext(args: Vec<Value>) -> EvalResult {
    expect_args("ngettext", &args, 3)?;
    let singular = expect_strict_string(&args[0])?;
    let plural = expect_strict_string(&args[1])?;
    let count = expect_int(&args[2])?;
    if count == 1 {
        Ok(Value::string(singular))
    } else {
        Ok(Value::string(plural))
    }
}

pub(crate) fn builtin_format(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    // With specbind, dynamic let-bindings are written directly to the obarray,
    // so print_options_from_state correctly resolves print-* variables.
    builtin_format_wrapper_strict(eval, args)
}

fn format_percent_s_in_state(ctx: &crate::emacs_core::eval::Context, value: &Value) -> String {
    super::misc_eval::print_value_princ_in_state(ctx, value)
}

fn format_not_enough_args_error() -> Flow {
    signal(
        "error",
        vec![Value::string("Not enough arguments for format string")],
    )
}

fn format_spec_type_mismatch_error() -> Flow {
    signal(
        "error",
        vec![Value::string(
            "Format specifier doesn’t match argument type",
        )],
    )
}

fn format_char_argument(n: i64) -> Result<String, Flow> {
    if !(0..=KEY_CHAR_CODE_MASK).contains(&n) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), Value::Int(n)],
        ));
    }

    write_char_rendered_text(n).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), Value::Int(n)],
        )
    })
}

/// Parsed format specification: %[flags][width][.precision]conversion
struct FormatSpec {
    minus: bool,
    plus: bool,
    space: bool,
    zero: bool,
    sharp: bool,
    width: Option<usize>,
    precision: Option<usize>,
    conversion: char,
}

/// Parse a format spec from a char iterator positioned just after '%'.
/// Returns None only if the format string ends prematurely.
fn parse_format_spec(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<FormatSpec> {
    let mut spec = FormatSpec {
        minus: false,
        plus: false,
        space: false,
        zero: false,
        sharp: false,
        width: None,
        precision: None,
        conversion: '\0',
    };

    // Parse flags
    loop {
        match chars.peek() {
            Some('-') => {
                spec.minus = true;
                chars.next();
            }
            Some('+') => {
                spec.plus = true;
                chars.next();
            }
            Some(' ') => {
                spec.space = true;
                chars.next();
            }
            Some('0') => {
                spec.zero = true;
                chars.next();
            }
            Some('#') => {
                spec.sharp = true;
                chars.next();
            }
            _ => break,
        }
    }

    // Ignore flags when sprintf ignores them
    if spec.plus {
        spec.space = false;
    }
    if spec.minus {
        spec.zero = false;
    }

    // Parse width
    let mut width_str = String::new();
    while let Some(&ch) = chars.peek() {
        if ch.is_ascii_digit() {
            width_str.push(ch);
            chars.next();
        } else {
            break;
        }
    }
    if !width_str.is_empty() {
        spec.width = width_str.parse().ok();
    }

    // Parse precision
    if chars.peek() == Some(&'.') {
        chars.next();
        let mut prec_str = String::new();
        while let Some(&ch) = chars.peek() {
            if ch.is_ascii_digit() {
                prec_str.push(ch);
                chars.next();
            } else {
                break;
            }
        }
        spec.precision = Some(if prec_str.is_empty() {
            0
        } else {
            prec_str.parse().unwrap_or(0)
        });
    }

    // Parse conversion character
    spec.conversion = chars.next()?;
    Some(spec)
}

/// Apply width/alignment padding to a formatted string.
fn apply_width(s: &str, spec: &FormatSpec) -> String {
    let w = match spec.width {
        Some(w) if w > s.chars().count() => w,
        _ => return s.to_string(),
    };
    let pad_char = if spec.zero && !spec.minus { '0' } else { ' ' };
    if spec.minus {
        format!("{:<width$}", s, width = w)
    } else if spec.zero && !spec.minus {
        // For zero-padding, handle negative numbers specially
        if s.starts_with('-') {
            format!("-{:0>width$}", &s[1..], width = w - 1)
        } else if s.starts_with('+') {
            format!("+{:0>width$}", &s[1..], width = w - 1)
        } else {
            format!("{:0>width$}", s, width = w)
        }
    } else {
        format!("{:>width$}", s, width = w)
    }
}

/// Format an integer with the given spec.
fn format_int_spec(n: i64, spec: &FormatSpec) -> String {
    let s = match spec.conversion {
        'd' => {
            if spec.plus && n >= 0 {
                format!("+{}", n)
            } else if spec.space && n >= 0 {
                format!(" {}", n)
            } else {
                n.to_string()
            }
        }
        'o' => {
            let negative = n < 0;
            let abs_val = (n as i128).unsigned_abs() as u64;
            let sign = if negative { "-" } else { "" };
            let prefix = if spec.sharp && abs_val != 0 { "0" } else { "" };
            format!("{}{}{:o}", sign, prefix, abs_val)
        }
        'x' => {
            let negative = n < 0;
            let abs_val = (n as i128).unsigned_abs() as u64;
            let sign = if negative { "-" } else { "" };
            let prefix = if spec.sharp && abs_val != 0 { "0x" } else { "" };
            format!("{}{}{:x}", sign, prefix, abs_val)
        }
        'X' => {
            let negative = n < 0;
            let abs_val = (n as i128).unsigned_abs() as u64;
            let sign = if negative { "-" } else { "" };
            let prefix = if spec.sharp && abs_val != 0 { "0X" } else { "" };
            format!("{}{}{:X}", sign, prefix, abs_val)
        }
        _ => n.to_string(),
    };
    apply_width(&s, spec)
}

/// Normalize Rust scientific notation to match C printf: sign always
/// present, at least two exponent digits (e.g. `e0` -> `e+00`).
fn normalize_exp_notation(s: &str) -> String {
    if let Some(e_pos) = s.rfind('e').or_else(|| s.rfind('E')) {
        let (mantissa, exp_part) = s.split_at(e_pos);
        let e_char = &exp_part[..1];
        let rest = &exp_part[1..];
        let (sign, digits) = if rest.starts_with('+') || rest.starts_with('-') {
            (&rest[..1], &rest[1..])
        } else {
            ("+", rest)
        };
        let padded = if digits.len() < 2 {
            format!("{:0>2}", digits)
        } else {
            digits.to_string()
        };
        format!("{}{}{}{}", mantissa, e_char, sign, padded)
    } else {
        s.to_string()
    }
}

/// Format a float with the given spec.
fn format_float_spec(f: f64, spec: &FormatSpec) -> String {
    let prec = spec.precision.unwrap_or(6);
    let s = match spec.conversion {
        'f' => format!("{:.prec$}", f, prec = prec),
        'e' => normalize_exp_notation(&format!("{:.prec$e}", f, prec = prec)),
        'E' => normalize_exp_notation(&format!("{:.prec$E}", f, prec = prec)),
        'g' | 'G' => {
            let p = if prec == 0 { 1 } else { prec };
            // %g uses %e if exponent < -4 or >= precision, else %f
            let exp_fmt = format!("{:.prec$e}", f, prec = p.saturating_sub(1));
            // Parse the exponent
            let exp_val = exp_fmt
                .rfind('e')
                .and_then(|i| exp_fmt[i + 1..].parse::<i32>().ok())
                .unwrap_or(0);
            if exp_val < -4 || exp_val >= p as i32 {
                // Use %e style, strip trailing zeros
                let mut s = format!("{:.prec$e}", f, prec = p.saturating_sub(1));
                // Strip trailing zeros before 'e'
                if let Some(e_pos) = s.rfind('e') {
                    let mantissa = &s[..e_pos];
                    let exp_part = &s[e_pos..];
                    let trimmed = mantissa.trim_end_matches('0');
                    let trimmed = trimmed.trim_end_matches('.');
                    s = format!("{}{}", trimmed, exp_part);
                }
                s = normalize_exp_notation(&s);
                if spec.conversion == 'G' {
                    s = s.replace('e', "E");
                }
                s
            } else {
                // Use %f style with appropriate decimals
                let decimal_places = if exp_val >= 0 {
                    p.saturating_sub(exp_val as usize + 1)
                } else {
                    p
                };
                let mut s = format!("{:.prec$}", f, prec = decimal_places);
                // Strip trailing zeros after decimal point
                if s.contains('.') {
                    s = s.trim_end_matches('0').to_string();
                    s = s.trim_end_matches('.').to_string();
                }
                s
            }
        }
        _ => format!("{:.prec$}", f, prec = prec),
    };
    let s = if spec.plus && f >= 0.0 && !f.is_nan() {
        format!("+{}", s)
    } else if spec.space && f >= 0.0 && !f.is_nan() {
        format!(" {}", s)
    } else {
        s
    };
    apply_width(&s, spec)
}

/// Format a string (%s) with width and precision.
fn format_string_spec(s: &str, spec: &FormatSpec) -> String {
    let truncated = if let Some(prec) = spec.precision {
        if prec < s.chars().count() {
            &s[..s.char_indices().nth(prec).map_or(s.len(), |(i, _)| i)]
        } else {
            s
        }
    } else {
        s
    };
    apply_width(truncated, spec)
}

/// Get the princ representation of a value (for %s).
fn format_value_princ(val: &Value) -> String {
    super::misc_eval::print_value_princ(val)
}

/// Core format implementation shared by both pure and eval variants.
fn do_format(
    args: &[Value],
    princ_fn: &dyn Fn(&Value) -> String,
    prin1_fn: &dyn Fn(&Value) -> String,
) -> Result<String, Flow> {
    let fmt_str = expect_strict_string(&args[0])?;
    let mut result = String::new();
    let mut arg_idx = 1;
    let mut chars = fmt_str.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '%' {
            result.push(ch);
            continue;
        }

        let Some(spec) = parse_format_spec(&mut chars) else {
            result.push('%');
            continue;
        };

        if spec.conversion == '%' {
            result.push('%');
            continue;
        }

        if arg_idx >= args.len() {
            return Err(format_not_enough_args_error());
        }

        let formatted = match spec.conversion {
            's' => {
                let s = princ_fn(&args[arg_idx]);
                arg_idx += 1;
                format_string_spec(&s, &spec)
            }
            'S' => {
                let s = prin1_fn(&args[arg_idx]);
                arg_idx += 1;
                format_string_spec(&s, &spec)
            }
            'd' | 'o' | 'x' | 'X' => {
                let n = match &args[arg_idx] {
                    Value::Int(i) => *i,
                    Value::Char(c) => *c as i64,
                    Value::Float(f, _) => *f as i64,
                    _ => {
                        return Err(format_spec_type_mismatch_error());
                    }
                };
                arg_idx += 1;
                format_int_spec(n, &spec)
            }
            'f' | 'e' | 'E' | 'g' | 'G' => {
                let f =
                    expect_number(&args[arg_idx]).map_err(|_| format_spec_type_mismatch_error())?;
                arg_idx += 1;
                format_float_spec(f, &spec)
            }
            'c' => {
                let n =
                    expect_int(&args[arg_idx]).map_err(|_| format_spec_type_mismatch_error())?;
                arg_idx += 1;
                let s = format_char_argument(n)?;
                format_string_spec(&s, &spec)
            }
            _ => {
                // Unknown specifier: pass through literally
                arg_idx += 1;
                format!("%{}", spec.conversion)
            }
        };
        result.push_str(&formatted);
    }

    Ok(result)
}

pub(crate) fn builtin_format_wrapper_strict(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    crate::emacs_core::perf_trace::time_op(crate::emacs_core::perf_trace::HotpathOp::Format, || {
        expect_min_args("format", &args, 1)?;
        let s = do_format(&args, &|v| format_percent_s_in_state(ctx, v), &|v| {
            super::error::print_value_in_state(ctx, v)
        })?;
        Ok(Value::string(s))
    })
}

/// Apply `text-quoting-style` translation to a string.
///
/// When the style is `curve` (the modern default), grave accent (U+0060)
/// is replaced with LEFT SINGLE QUOTATION MARK (U+2018) and apostrophe
/// (U+0027) is replaced with RIGHT SINGLE QUOTATION MARK (U+2019).
/// This mirrors GNU Emacs's `styled_format` with `message = true`.
fn apply_text_quoting(s: &str) -> String {
    // text-quoting-style is always `curve` in NeoVM (see coding.rs).
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '`' => out.push('\u{2018}'),
            '\'' => out.push('\u{2019}'),
            _ => out.push(ch),
        }
    }
    out
}

pub(crate) fn builtin_format_message(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("format-message", &args, 1)?;
    let formatted = builtin_format_wrapper_strict(ctx, args)?;
    match formatted {
        Value::Str(id) => {
            let s = super::super::value::with_heap(|h| h.get_string(id).to_owned());
            Ok(Value::string(apply_text_quoting(&s)))
        }
        other => Ok(other),
    }
}

pub(crate) fn builtin_make_string(args: Vec<Value>) -> EvalResult {
    expect_min_args("make-string", &args, 2)?;
    expect_max_args("make-string", &args, 3)?;
    let count_raw = expect_int(&args[0])?;
    if count_raw < 0 {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("wholenump"), args[0]],
        ));
    }
    let count = count_raw as usize;

    let ch = match &args[1] {
        Value::Int(c) => {
            if *c < 0 {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("characterp"), args[1]],
                ));
            }
            *c as u32
        }
        Value::Char(c) => *c as u32,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), *other],
            ));
        }
    };

    // GNU Emacs alloc.c: `make-string` returns a unibyte string only when the
    // initializer is ASCII and the optional MULTIBYTE arg is nil/omitted.
    let multibyte = args.get(2).is_some_and(Value::is_truthy) || ch > 0x7f;

    let unit = encode_char_code_for_string_storage(ch, multibyte).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), args[1]],
        )
    })?;
    let result = unit.repeat(count);
    Ok(if multibyte {
        Value::multibyte_string(result)
    } else {
        Value::unibyte_string(result)
    })
}

pub(crate) fn builtin_string(args: Vec<Value>) -> EvalResult {
    let mut result = String::new();
    for arg in args {
        match arg {
            Value::Char(c) => result.push(c),
            Value::Int(code) => {
                if code < 0 {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("characterp"), Value::Int(code)],
                    ));
                }
                if let Some(ch) = char::from_u32(code as u32) {
                    result.push(ch);
                } else if let Some(encoded) = encode_nonunicode_char_for_storage(code as u32) {
                    result.push_str(&encoded);
                } else {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("characterp"), Value::Int(code)],
                    ));
                }
            }
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("characterp"), other],
                ));
            }
        }
    }
    Ok(Value::string(result))
}

/// `(unibyte-string &rest BYTES)` -> unibyte storage string.
pub(crate) fn builtin_unibyte_string(args: Vec<Value>) -> EvalResult {
    let mut bytes = Vec::with_capacity(args.len());
    for arg in args {
        let n = match arg {
            Value::Int(v) => v,
            Value::Char(c) => c as i64,
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), other],
                ));
            }
        };
        if !(0..=255).contains(&n) {
            return Err(signal(
                "args-out-of-range",
                vec![Value::Int(n), Value::Int(0), Value::Int(255)],
            ));
        }
        bytes.push(n as u8);
    }
    Ok(Value::unibyte_string(bytes_to_unibyte_storage_string(
        &bytes,
    )))
}

pub(crate) fn builtin_byte_to_string(args: Vec<Value>) -> EvalResult {
    expect_args("byte-to-string", &args, 1)?;
    let byte = expect_fixnum(&args[0])?;
    if !(0..=255).contains(&byte) {
        return Err(signal("error", vec![Value::string("Invalid byte")]));
    }
    Ok(Value::unibyte_string(bytes_to_unibyte_storage_string(&[
        byte as u8,
    ])))
}

pub(crate) fn builtin_bitmap_spec_p(args: Vec<Value>) -> EvalResult {
    expect_args("bitmap-spec-p", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_clear_face_cache(args: Vec<Value>) -> EvalResult {
    expect_max_args("clear-face-cache", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_clear_buffer_auto_save_failure(args: Vec<Value>) -> EvalResult {
    expect_args("clear-buffer-auto-save-failure", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_string_width(args: Vec<Value>) -> EvalResult {
    expect_min_args("string-width", &args, 1)?;
    expect_max_args("string-width", &args, 3)?;
    let s = expect_string(&args[0])?;
    if args.len() <= 1
        || (args.len() == 2 && args[1] == Value::Nil)
        || (args.len() <= 3
            && (args.len() < 2 || args[1] == Value::Nil || args[1] == Value::Int(0))
            && (args.len() < 3 || args[2] == Value::Nil))
    {
        // Fast path: full string width
        return Ok(Value::Int(storage_string_display_width(&s) as i64));
    }
    // Substring range specified — decode units and sum width for [from, to)
    let units = super::super::string_escape::decode_storage_units(&s);
    let from = if args.len() > 1 && args[1] != Value::Nil {
        expect_int(&args[1])? as usize
    } else {
        0
    };
    let to = if args.len() > 2 && args[2] != Value::Nil {
        expect_int(&args[2])? as usize
    } else {
        units.len()
    };
    let width: usize = units
        .iter()
        .skip(from)
        .take(to.saturating_sub(from))
        .map(|(_, w)| w)
        .sum();
    Ok(Value::Int(width as i64))
}
