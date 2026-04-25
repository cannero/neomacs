use super::*;
use crate::emacs_core::value::{ValueKind, VecLikeType};

// ===========================================================================
// Cons / List operations
// ===========================================================================

pub(crate) fn builtin_cons(args: Vec<Value>) -> EvalResult {
    expect_args("cons", &args, 2)?;
    Ok(Value::cons(args[0], args[1]))
}

// ---------------------------------------------------------------------------
// Lambda → cons-list transparency helpers
//
// Official Emacs represents closures as cons lists:
//   (closure ENV PARAMS [DOCSTRING] BODY...)   — lexical closure
//   (lambda PARAMS [DOCSTRING] BODY...)        — dynamic lambda
//
// NeoVM uses Value::Lambda(tagged pointer) internally.  The helpers below let all
// list / sequence operations treat Lambda values as if they were cons lists,
// which is required for cl-generic, oclosure, nadvice and many other packages.
// ---------------------------------------------------------------------------

/// Convert a Lambda (or Macro) value to a cons-list representation matching
/// the official Emacs format.
pub(crate) fn lambda_to_cons_list(value: &Value) -> Option<Value> {
    let saved_roots = crate::emacs_core::eval::save_scratch_gc_roots();
    let mut elements = Vec::new();

    if let Some(env_val) = value.closure_env().flatten() {
        elements.push(Value::symbol("closure"));
        if env_val.is_nil() {
            elements.push(Value::list(vec![Value::T]));
        } else {
            elements.push(env_val);
        }
    } else {
        elements.push(Value::symbol("lambda"));
    }

    let params_value = lambda_params_to_value(value.closure_params()?);
    crate::emacs_core::eval::push_scratch_gc_root(params_value);
    elements.push(params_value);

    if let Some(doc) = value.closure_docstring().flatten() {
        let doc_value = Value::heap_string(doc.clone());
        crate::emacs_core::eval::push_scratch_gc_root(doc_value);
        elements.push(doc_value);
    }

    // Include (:documentation TYPE) form for oclosures
    if let Some(doc_form) = value.closure_doc_form().flatten() {
        let doc_entry = Value::list(vec![Value::keyword(":documentation"), doc_form]);
        crate::emacs_core::eval::push_scratch_gc_root(doc_entry);
        elements.push(doc_entry);
    }

    if let Some(interactive) = value.closure_interactive().flatten() {
        let interactive_entry = if interactive.is_cons()
            && interactive.cons_car().as_symbol_name() == Some("interactive")
        {
            interactive
        } else if interactive.is_vector() {
            let items = interactive.as_vector_data().cloned().unwrap_or_default();
            let mut list_items = Vec::with_capacity(items.len() + 1);
            list_items.push(Value::symbol("interactive"));
            list_items.extend(items);
            Value::list(list_items)
        } else {
            Value::list(vec![Value::symbol("interactive"), interactive])
        };
        crate::emacs_core::eval::push_scratch_gc_root(interactive_entry);
        elements.push(interactive_entry);
    }

    let body = value.closure_body_value()?;
    for form in list_to_vec(&body)? {
        crate::emacs_core::eval::push_scratch_gc_root(form);
        elements.push(form);
    }

    let result = Value::list(elements);
    crate::emacs_core::eval::restore_scratch_gc_roots(saved_roots);
    Some(result)
}

pub(crate) fn lambda_closure_length(value: &Value) -> Option<i64> {
    let slots = value.closure_slots()?;
    let mut logical_len = slots.len();
    while logical_len > 3
        && slots
            .get(logical_len - 1)
            .is_some_and(|value| value.is_nil())
    {
        logical_len -= 1;
    }
    Some(logical_len as i64)
}

/// Convert a Lambda value to the GNU Emacs closure vector layout:
///   [0]=ARGS  [1]=BODY  [2]=ENV  [(3)=nil, (4)=DOCSTRING/TYPE]
/// NeoVM does not currently store the optional interactive slot.
/// This is used by `aref` on closures for oclosure slot access.
pub fn lambda_to_closure_vector(value: &Value) -> Vec<Value> {
    value.closure_slots().cloned().unwrap_or_default()
}

pub(crate) fn bytecode_closure_length(value: &Value) -> Option<i64> {
    let bc = value.get_bytecode_data()?;
    let mut logical_len = 4;
    if bc.docstring.is_some() || bc.doc_form.is_some() {
        logical_len = 5;
    }
    if bc.interactive.is_some() {
        logical_len = 6;
    }
    Some(logical_len)
}

pub(crate) fn closure_vector_length(value: &Value) -> Option<i64> {
    match value.kind() {
        ValueKind::Veclike(VecLikeType::Lambda) => lambda_closure_length(value),
        ValueKind::Veclike(VecLikeType::ByteCode) => bytecode_closure_length(value),
        _ => None,
    }
}

/// Convert a ByteCode value to the GNU Emacs closure vector layout:
///   [0]=ARGLIST  [1]=CODE  [2]=ENV/CONSTANTS  [3]=DEPTH  [(4)=DOC/TYPE]
/// NeoVM does not currently store the optional interactive slot.
/// This is used by `aref` on bytecode closures for oclosure slot access.
pub(crate) fn bytecode_to_closure_vector(value: &Value) -> Vec<Value> {
    let bc = match value.get_bytecode_data() {
        Some(d) => d.clone(),
        None => return Vec::new(),
    };
    let saved_roots = crate::emacs_core::eval::save_scratch_gc_roots();

    let args = bc.arglist;
    crate::emacs_core::eval::push_scratch_gc_root(args);

    // Slot 1: bytecode string.  GNU Emacs stores this as a unibyte string of
    // raw opcode bytes.  NeoVM normally executes from `ops` (decoded IR), but
    // elisp code like `byte-compile-make-closure` reads `(aref fn 1)` and
    // passes it to `make-byte-code`, so we need to round-trip the bytes.
    let code = if let Some(bytes) = &bc.gnu_bytecode_bytes {
        // Store raw bytes directly as a unibyte string.
        // GNU Emacs bytecode strings are unibyte — each byte is one character.
        Value::heap_string(crate::heap_types::LispString::from_unibyte(bytes.clone()))
    } else {
        Value::NIL
    };
    crate::emacs_core::eval::push_scratch_gc_root(code);

    // Slot 2: env if NeoVM-compiled (cons alist), else constants vector
    let env = if let Some(env_val) = bc.env {
        env_val
    } else {
        Value::vector(bc.constants.clone())
    };
    crate::emacs_core::eval::push_scratch_gc_root(env);

    // Slot 3: max stack depth
    let depth = Value::fixnum(bc.max_stack as i64);

    let slot4 = bc
        .doc_form
        .or_else(|| bc.docstring.as_ref().map(|d| Value::heap_string(d.clone())))
        .unwrap_or(Value::NIL);
    let slot5 = bc.interactive.unwrap_or(Value::NIL);

    // GNU Emacs bytecode objects always have at least 5 slots (indices 0-4).
    // Some code (e.g. advice--p) does (aref func 4) unconditionally.
    let mut result = vec![args, code, env, depth, slot4];
    if !slot5.is_nil() {
        result.push(slot5);
    }
    crate::emacs_core::eval::restore_scratch_gc_roots(saved_roots);
    result
}

/// Convert LambdaParams to a Lisp list (a b &optional c &rest d).
/// Parse a Lisp arglist Value into LambdaParams.
pub fn parse_lambda_params_from_value(
    arglist: &Value,
) -> Result<LambdaParams, super::super::error::Flow> {
    use crate::emacs_core::intern::{intern, resolve_sym};
    let items = list_to_vec(arglist).unwrap_or_default();
    let mut required = Vec::new();
    let mut optional = Vec::new();
    let mut rest = None;
    let mut mode = 0; // 0=required, 1=optional, 2=rest
    for item in &items {
        if let Some(name) = item.as_symbol_name() {
            match name {
                "&optional" => {
                    mode = 1;
                    continue;
                }
                "&rest" => {
                    mode = 2;
                    continue;
                }
                _ => {}
            }
        }
        let sym_id = item.as_symbol_id().unwrap_or_else(|| intern("_"));
        match mode {
            0 => required.push(sym_id),
            1 => optional.push(sym_id),
            2 => {
                rest = Some(sym_id);
                break;
            }
            _ => {}
        }
    }
    Ok(LambdaParams {
        required,
        optional,
        rest,
    })
}

pub fn lambda_params_to_value(params: &LambdaParams) -> Value {
    let mut elements = Vec::new();
    for p in &params.required {
        elements.push(Value::from_sym_id(*p));
    }
    if !params.optional.is_empty() {
        elements.push(Value::symbol("&optional"));
        for p in &params.optional {
            elements.push(Value::from_sym_id(*p));
        }
    }
    if let Some(ref rest) = params.rest {
        elements.push(Value::symbol("&rest"));
        elements.push(Value::from_sym_id(*rest));
    }
    Value::list(elements)
}

fn car_value(value: &Value) -> Result<Value, Flow> {
    match value.kind() {
        ValueKind::Nil => Ok(Value::NIL),
        ValueKind::Cons => Ok(value.cons_car()),
        // In official Emacs, closures are cons lists with car = closure/lambda.
        ValueKind::Veclike(VecLikeType::Lambda) => {
            if value.closure_env().flatten().is_some() {
                Ok(Value::symbol("closure"))
            } else {
                Ok(Value::symbol("lambda"))
            }
        }
        _ => {
            if value.is_t() {
                tracing::error!("car called on t — likely closure env or alist issue");
            }
            Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("listp"), *value],
            ))
        }
    }
}

fn cdr_value(value: &Value) -> Result<Value, Flow> {
    match value.kind() {
        ValueKind::Nil => Ok(Value::NIL),
        ValueKind::Cons => Ok(value.cons_cdr()),
        // Convert Lambda to cons list, return cdr.
        ValueKind::Veclike(VecLikeType::Lambda) => {
            let list = lambda_to_cons_list(value).unwrap_or(Value::NIL);
            match list.kind() {
                ValueKind::Cons => Ok(list.cons_cdr()),
                _ => Ok(Value::NIL),
            }
        }
        _ => {
            if value.is_t() {
                tracing::error!("cdr called on t — stack trace:");
                let bt = std::backtrace::Backtrace::force_capture();
                tracing::error!("{bt}");
            }
            Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("listp"), *value],
            ))
        }
    }
}

pub(crate) fn builtin_car(args: Vec<Value>) -> EvalResult {
    expect_args("car", &args, 1)?;
    car_value(&args[0])
}

pub(crate) fn builtin_car_1(_eval: &mut super::eval::Context, arg: Value) -> EvalResult {
    car_value(&arg)
}

pub(crate) fn builtin_cdr(args: Vec<Value>) -> EvalResult {
    expect_args("cdr", &args, 1)?;
    cdr_value(&args[0])
}

pub(crate) fn builtin_cdr_1(_eval: &mut super::eval::Context, arg: Value) -> EvalResult {
    cdr_value(&arg)
}

pub(crate) fn builtin_car_safe(args: Vec<Value>) -> EvalResult {
    expect_args("car-safe", &args, 1)?;
    Ok(car_safe_value(&args[0]))
}

pub(crate) fn builtin_car_safe_1(_eval: &mut super::eval::Context, arg: Value) -> EvalResult {
    Ok(car_safe_value(&arg))
}

pub(crate) fn builtin_cdr_safe(args: Vec<Value>) -> EvalResult {
    expect_args("cdr-safe", &args, 1)?;
    Ok(cdr_safe_value(&args[0]))
}

pub(crate) fn builtin_cdr_safe_1(_eval: &mut super::eval::Context, arg: Value) -> EvalResult {
    Ok(cdr_safe_value(&arg))
}

// Treats neomacs Lambda values as their cons-list equivalents to match
// GNU semantics where lambdas are actual cons lists.
fn car_safe_value(val: &Value) -> Value {
    match val.kind() {
        ValueKind::Cons => val.cons_car(),
        ValueKind::Veclike(VecLikeType::Lambda) => {
            if val.closure_env().flatten().is_some() {
                Value::symbol("closure")
            } else {
                Value::symbol("lambda")
            }
        }
        _ => Value::NIL,
    }
}

fn cdr_safe_value(val: &Value) -> Value {
    match val.kind() {
        ValueKind::Cons => val.cons_cdr(),
        ValueKind::Veclike(VecLikeType::Lambda) => {
            let list = super::lambda_to_cons_list(val).unwrap_or(Value::NIL);
            match list.kind() {
                ValueKind::Cons => list.cons_cdr(),
                _ => Value::NIL,
            }
        }
        _ => Value::NIL,
    }
}

pub(crate) fn builtin_setcar(args: Vec<Value>) -> EvalResult {
    expect_args("setcar", &args, 2)?;
    match args[0].kind() {
        ValueKind::Cons => {
            args[0].set_car(args[1]);
            Ok(args[1])
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("consp"), args[0]],
        )),
    }
}

pub(crate) fn builtin_setcdr(args: Vec<Value>) -> EvalResult {
    expect_args("setcdr", &args, 2)?;
    match args[0].kind() {
        ValueKind::Cons => {
            args[0].set_cdr(args[1]);
            Ok(args[1])
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("consp"), args[0]],
        )),
    }
}

pub(crate) fn builtin_list(args: Vec<Value>) -> EvalResult {
    Ok(Value::list(args))
}

pub(crate) fn builtin_length(args: Vec<Value>) -> EvalResult {
    expect_args("length", &args, 1)?;
    match args[0].kind() {
        ValueKind::Nil => Ok(Value::fixnum(0)),
        ValueKind::Veclike(VecLikeType::Lambda) | ValueKind::Veclike(VecLikeType::ByteCode) => {
            Ok(Value::fixnum(closure_vector_length(&args[0]).unwrap()))
        }
        ValueKind::Cons => match list_length(&args[0]) {
            Some(n) => Ok(Value::fixnum(n as i64)),
            None => {
                // Mirrors GNU `Flength` which walks the cons list
                // until a non-cons cdr is found, and signals
                // `(wrong-type-argument listp TAIL)` where TAIL is
                // the offending non-nil non-cons cell (not the
                // whole list). Verified via the GNU emacs binary:
                // `(length '(1 . 2))` → `(wrong-type-argument listp 2)`.
                let mut cursor = args[0];
                while cursor.is_cons() {
                    cursor = cursor.cons_cdr();
                }
                Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), cursor],
                ))
            }
        },
        ValueKind::String => {
            let s = args[0].as_lisp_string().expect("string");
            Ok(Value::fixnum(s.schars() as i64))
        }
        ValueKind::Veclike(VecLikeType::Vector) | ValueKind::Veclike(VecLikeType::Record) => {
            Ok(Value::fixnum(vector_sequence_length(&args[0])))
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), args[0]],
        )),
    }
}

fn vector_sequence_length(sequence: &Value) -> i64 {
    super::chartable::bool_vector_length(sequence)
        .or_else(|| super::chartable::char_table_length(sequence))
        .unwrap_or_else(|| sequence.as_vector_data().unwrap().len() as i64)
}

fn sequence_length_less_than(sequence: &Value, target: i64) -> Result<bool, Flow> {
    match sequence.kind() {
        ValueKind::Nil => Ok(0 < target),
        ValueKind::Veclike(VecLikeType::Lambda) | ValueKind::Veclike(VecLikeType::ByteCode) => {
            Ok(closure_vector_length(sequence).unwrap() < target)
        }
        ValueKind::String => {
            Ok((sequence.as_lisp_string().expect("string").schars() as i64) < target)
        }
        ValueKind::Veclike(VecLikeType::Vector) | ValueKind::Veclike(VecLikeType::Record) => {
            Ok(vector_sequence_length(sequence) < target)
        }
        ValueKind::Cons => {
            if target <= 0 {
                return Ok(false);
            }
            let mut remaining = target;
            let mut cursor = *sequence;
            while remaining > 0 {
                match cursor.kind() {
                    ValueKind::Cons => {
                        cursor = cursor.cons_cdr();
                        remaining -= 1;
                    }
                    _ => return Ok(true),
                }
            }
            Ok(false)
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), *sequence],
        )),
    }
}

fn sequence_length_equal(sequence: &Value, target: i64) -> Result<bool, Flow> {
    match sequence.kind() {
        ValueKind::Nil => Ok(target == 0),
        ValueKind::Veclike(VecLikeType::Lambda) | ValueKind::Veclike(VecLikeType::ByteCode) => {
            Ok(closure_vector_length(sequence).unwrap() == target)
        }
        ValueKind::String => {
            Ok((sequence.as_lisp_string().expect("string").schars() as i64) == target)
        }
        ValueKind::Veclike(VecLikeType::Vector) | ValueKind::Veclike(VecLikeType::Record) => {
            Ok(vector_sequence_length(sequence) == target)
        }
        ValueKind::Cons => {
            if target < 0 {
                return Ok(false);
            }
            let mut remaining = target;
            let mut cursor = *sequence;
            while remaining > 0 {
                match cursor.kind() {
                    ValueKind::Cons => {
                        cursor = cursor.cons_cdr();
                        remaining -= 1;
                    }
                    _ => return Ok(false),
                }
            }
            Ok(!cursor.is_cons())
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), *sequence],
        )),
    }
}

fn sequence_length_greater_than(sequence: &Value, target: i64) -> Result<bool, Flow> {
    match sequence.kind() {
        ValueKind::Nil => Ok(0 > target),
        ValueKind::Veclike(VecLikeType::Lambda) | ValueKind::Veclike(VecLikeType::ByteCode) => {
            Ok(closure_vector_length(sequence).unwrap() > target)
        }
        ValueKind::String => {
            Ok((sequence.as_lisp_string().expect("string").schars() as i64) > target)
        }
        ValueKind::Veclike(VecLikeType::Vector) | ValueKind::Veclike(VecLikeType::Record) => {
            Ok(vector_sequence_length(sequence) > target)
        }
        ValueKind::Cons => {
            if target < 0 {
                return Ok(true);
            }
            if target == i64::MAX {
                return Ok(false);
            }
            let mut remaining = target + 1;
            let mut cursor = *sequence;
            while remaining > 0 {
                match cursor.kind() {
                    ValueKind::Cons => {
                        cursor = cursor.cons_cdr();
                        remaining -= 1;
                    }
                    _ => return Ok(false),
                }
            }
            Ok(true)
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), *sequence],
        )),
    }
}

pub(crate) fn builtin_length_lt(args: Vec<Value>) -> EvalResult {
    expect_args("length<", &args, 2)?;
    let target = expect_fixnum(&args[1])?;
    Ok(Value::bool_val(sequence_length_less_than(
        &args[0], target,
    )?))
}

pub(crate) fn builtin_length_eq(args: Vec<Value>) -> EvalResult {
    expect_args("length=", &args, 2)?;
    let target = expect_fixnum(&args[1])?;
    Ok(Value::bool_val(sequence_length_equal(&args[0], target)?))
}

pub(crate) fn builtin_length_gt(args: Vec<Value>) -> EvalResult {
    expect_args("length>", &args, 2)?;
    let target = expect_fixnum(&args[1])?;
    Ok(Value::bool_val(sequence_length_greater_than(
        &args[0], target,
    )?))
}

pub(crate) fn builtin_nth(args: Vec<Value>) -> EvalResult {
    expect_args("nth", &args, 2)?;
    let n = expect_int(&args[0])?;
    let tail = nthcdr_impl(n, args[1])?;
    match tail.kind() {
        ValueKind::Cons => Ok(tail.cons_car()),
        ValueKind::Nil => Ok(Value::NIL),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), tail],
        )),
    }
}

fn nthcdr_impl(n: i64, list: Value) -> EvalResult {
    if n <= 0 {
        return Ok(list);
    }

    // Convert Lambda to cons list for traversal.
    let mut cursor = match list.kind() {
        ValueKind::Veclike(VecLikeType::Lambda) => lambda_to_cons_list(&list).unwrap_or(Value::NIL),
        _ => list,
    };
    for _ in 0..(n as usize) {
        match cursor.kind() {
            ValueKind::Cons => {
                cursor = cursor.cons_cdr();
            }
            ValueKind::Nil => return Ok(Value::NIL),
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), list],
                ));
            }
        }
    }
    Ok(cursor)
}

pub(crate) fn builtin_nthcdr(args: Vec<Value>) -> EvalResult {
    expect_args("nthcdr", &args, 2)?;
    let n = expect_int(&args[0])?;
    nthcdr_impl(n, args[1])
}

pub(crate) fn builtin_append(args: Vec<Value>) -> EvalResult {
    fn extend_from_proper_list(out: &mut Vec<Value>, list: &Value) -> Result<(), Flow> {
        let mut cursor = *list;
        loop {
            match cursor.kind() {
                ValueKind::Nil => return Ok(()),
                ValueKind::Cons => {
                    let pair_car = cursor.cons_car();
                    let pair_cdr = cursor.cons_cdr();
                    out.push(pair_car);
                    cursor = pair_cdr;
                }
                _ => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("listp"), cursor],
                    ));
                }
            }
        }
    }

    if args.is_empty() {
        return Ok(Value::NIL);
    }
    if args.len() == 1 {
        return Ok(args[0]);
    }

    // Collect all elements from all lists except the last, then use last as tail
    let mut elements: Vec<Value> = Vec::new();
    for arg in &args[..args.len() - 1] {
        match arg.kind() {
            ValueKind::Nil => {}
            ValueKind::Cons => extend_from_proper_list(&mut elements, arg)?,
            ValueKind::Veclike(VecLikeType::Lambda) => {
                elements.extend(lambda_to_closure_vector(arg).into_iter())
            }
            ValueKind::Veclike(VecLikeType::ByteCode) => {
                elements.extend(bytecode_to_closure_vector(arg).into_iter())
            }
            ValueKind::Veclike(VecLikeType::Vector) => {
                elements.extend(arg.as_vector_data().unwrap().clone().into_iter())
            }
            ValueKind::String => {
                let string = arg.as_lisp_string().expect("string");
                super::for_each_lisp_string_char(string, |cp| {
                    elements.push(Value::fixnum(cp as i64));
                });
            }
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("sequencep"), *arg],
                ));
            }
        }
    }

    let last = &args[args.len() - 1];
    // Debug: trace when append's last arg is t (causes dotted pair ending with t)
    if last.is_t() && !elements.is_empty() {
        tracing::error!(
            "append: last arg is t, will create dotted pair! nargs={} elements={}",
            args.len(),
            elements.len()
        );
    }
    if elements.is_empty() {
        return Ok(*last);
    }

    // Build list with last arg as tail (supports improper lists)
    let tail = *last;
    Ok(elements
        .into_iter()
        .rev()
        .fold(tail, |acc, item| Value::cons(item, acc)))
}

pub(crate) fn builtin_reverse(args: Vec<Value>) -> EvalResult {
    fn reverse_string(value: Value) -> EvalResult {
        let string = value
            .as_lisp_string()
            .ok_or_else(|| signal("wrong-type-argument", vec![Value::symbol("stringp"), value]))?;

        if !string.is_multibyte() {
            let mut bytes = string.as_bytes().to_vec();
            bytes.reverse();
            return Ok(Value::heap_string(
                crate::heap_types::LispString::from_unibyte(bytes),
            ));
        }

        let mut codes = super::lisp_string_char_codes(string);
        codes.reverse();

        let mut data = Vec::with_capacity(string.sbytes());
        let mut buf = [0u8; crate::emacs_core::emacs_char::MAX_MULTIBYTE_LENGTH];
        for code in codes {
            let len = crate::emacs_core::emacs_char::char_string(code, &mut buf);
            data.extend_from_slice(&buf[..len]);
        }
        Ok(Value::heap_string(
            crate::heap_types::LispString::from_emacs_bytes(data),
        ))
    }

    fn reverse_bool_vector(value: Value) -> EvalResult {
        let Some(mut data) = value.as_vector_data().cloned() else {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("sequencep"), value],
            ));
        };
        let logical_len = super::chartable::bool_vector_length(&value).unwrap_or_default() as usize;
        let bits_end = 2 + logical_len;
        if data.len() >= bits_end {
            data[2..bits_end].reverse();
        }
        Ok(Value::vector(data))
    }

    expect_args("reverse", &args, 1)?;
    match args[0].kind() {
        ValueKind::Nil => Ok(Value::NIL),
        ValueKind::Cons => {
            let items = list_to_vec(&args[0]).ok_or_else(|| {
                signal("wrong-type-argument", vec![Value::symbol("listp"), args[0]])
            })?;
            let mut reversed = items;
            reversed.reverse();
            Ok(Value::list(reversed))
        }
        ValueKind::Veclike(VecLikeType::Vector) => {
            if super::chartable::is_char_table(&args[0]) {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("sequencep"), args[0]],
                ));
            }
            if super::chartable::is_bool_vector(&args[0]) {
                return reverse_bool_vector(args[0]);
            }
            let mut items = args[0].as_vector_data().unwrap().clone();
            items.reverse();
            Ok(Value::vector(items))
        }
        ValueKind::String => reverse_string(args[0]),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), args[0]],
        )),
    }
}

pub(crate) fn builtin_nreverse(args: Vec<Value>) -> EvalResult {
    fn dotted_list_prefix(list: &Value) -> Option<Value> {
        let mut cursor = *list;
        let mut prefix = Vec::new();
        loop {
            match cursor.kind() {
                ValueKind::Cons => {
                    let pair_car = cursor.cons_car();
                    let pair_cdr = cursor.cons_cdr();
                    prefix.push(pair_car);
                    cursor = pair_cdr;
                }
                ValueKind::Nil => return None,
                _ => return Some(Value::list(prefix)),
            }
        }
    }

    expect_args("nreverse", &args, 1)?;
    match args[0].kind() {
        ValueKind::Nil => Ok(Value::NIL),
        ValueKind::Cons => {
            // Match Emacs list semantics: reject dotted lists with proper-prefix payload.
            if let Some(prefix) = dotted_list_prefix(&args[0]) {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), prefix],
                ));
            }

            let mut prev = Value::NIL;
            let mut current = args[0];
            loop {
                match current.kind() {
                    ValueKind::Nil => return Ok(prev),
                    ValueKind::Cons => {
                        let next = current.cons_cdr();
                        current.set_cdr(prev);
                        prev = current;
                        current = next;
                    }
                    _ => unreachable!("proper-list check should reject dotted tails"),
                }
            }
        }
        ValueKind::Veclike(VecLikeType::Vector) => {
            if super::chartable::is_char_table(&args[0]) {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("arrayp"), args[0]],
                ));
            }
            if super::chartable::is_bool_vector(&args[0]) {
                let logical_len =
                    super::chartable::bool_vector_length(&args[0]).unwrap_or_default() as usize;
                let bits_end = 2 + logical_len;
                let mut data = args[0].as_vector_data().cloned().unwrap_or_default();
                if data.len() >= bits_end {
                    data[2..bits_end].reverse();
                }
                let _ = args[0].replace_vector_data(data);
                return Ok(args[0]);
            }
            let mut data = args[0].as_vector_data().cloned().unwrap_or_default();
            data.reverse();
            let _ = args[0].replace_vector_data(data);
            Ok(args[0])
        }
        ValueKind::String => builtin_reverse(args),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("arrayp"), args[0]],
        )),
    }
}

pub(crate) fn builtin_member(args: Vec<Value>) -> EvalResult {
    builtin_member_with_symbols(args, false)
}

pub(crate) fn builtin_member_with_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_member_with_symbols(args, eval.symbols_with_pos_enabled)
}

fn builtin_member_with_symbols(args: Vec<Value>, symbols_with_pos_enabled: bool) -> EvalResult {
    expect_args("member", &args, 2)?;
    let target = &args[0];
    let list = args[1];
    if list.is_t() {
        tracing::error!(
            "(member {} t) — list is bare t! target={:?}",
            crate::emacs_core::print::print_value(target),
            target.kind()
        );
    }
    let mut cursor = list;
    loop {
        match cursor.kind() {
            ValueKind::Nil => return Ok(Value::NIL),
            ValueKind::Cons => {
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                if equal_value_swp(target, &pair_car, 0, symbols_with_pos_enabled) {
                    return Ok(cursor);
                }
                cursor = pair_cdr;
            }
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), list],
                ));
            }
        }
    }
}

pub(crate) fn builtin_memq(args: Vec<Value>) -> EvalResult {
    builtin_memq_with_symbols(args, false)
}

pub(crate) fn builtin_memq_with_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_memq_with_symbols(args, eval.symbols_with_pos_enabled)
}

fn builtin_memq_with_symbols(args: Vec<Value>, symbols_with_pos_enabled: bool) -> EvalResult {
    expect_args("memq", &args, 2)?;
    let target = &args[0];
    let list = args[1];
    let mut cursor = list;
    loop {
        match cursor.kind() {
            ValueKind::Nil => return Ok(Value::NIL),
            ValueKind::Cons => {
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                if eq_value_swp(target, &pair_car, symbols_with_pos_enabled) {
                    return Ok(cursor);
                }
                cursor = pair_cdr;
            }
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), list],
                ));
            }
        }
    }
}

pub(crate) fn builtin_memql(args: Vec<Value>) -> EvalResult {
    builtin_memql_with_symbols(args, false)
}

pub(crate) fn builtin_memql_with_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_memql_with_symbols(args, eval.symbols_with_pos_enabled)
}

fn builtin_memql_with_symbols(args: Vec<Value>, symbols_with_pos_enabled: bool) -> EvalResult {
    expect_args("memql", &args, 2)?;
    let target = &args[0];
    let list = args[1];
    let mut cursor = list;
    loop {
        match cursor.kind() {
            ValueKind::Nil => return Ok(Value::NIL),
            ValueKind::Cons => {
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                if eql_value_swp(target, &pair_car, symbols_with_pos_enabled) {
                    return Ok(cursor);
                }
                cursor = pair_cdr;
            }
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), list],
                ));
            }
        }
    }
}

pub(crate) fn builtin_assoc(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    crate::emacs_core::perf_trace::time_op(crate::emacs_core::perf_trace::HotpathOp::Assoc, || {
        expect_range_args("assoc", &args, 2, 3)?;
        let key = &args[0];
        let list = args[1];
        let test_fn = args
            .get(2)
            .and_then(|value| if value.is_nil() { None } else { Some(*value) });
        if test_fn.is_some() {
            let roots = eval.save_specpdl_roots();
            eval.push_specpdl_root(*key);
            eval.push_specpdl_root(list);
            eval.push_specpdl_root(test_fn.unwrap());
            let mut cursor = list;
            let assoc_result = loop {
                match cursor.kind() {
                    ValueKind::Nil => break Ok(Value::NIL),
                    ValueKind::Cons => {
                        let pair_car = cursor.cons_car();
                        let pair_cdr = cursor.cons_cdr();
                        if let ValueKind::Cons = pair_car.kind() {
                            let entry_key = pair_car.cons_car();
                            let matches = if let Some(test_fn) = &test_fn {
                                match eval.apply(*test_fn, vec![entry_key, *key]) {
                                    Ok(v) => v.is_truthy(),
                                    Err(e) => {
                                        break Err(e);
                                    }
                                }
                            } else {
                                equal_value_swp(key, &entry_key, 0, eval.symbols_with_pos_enabled)
                            };
                            if matches {
                                break Ok(pair_car);
                            }
                        }
                        cursor = pair_cdr;
                    }
                    _ => {
                        break Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("listp"), list],
                        ));
                    }
                }
            };
            eval.restore_specpdl_roots(roots);
            return assoc_result;
        }
        // No test_fn: simple equal-based traversal (no rooting needed)
        let roots = eval.save_specpdl_roots();
        eval.push_specpdl_root(list);
        let mut cursor = list;
        let assoc_result = loop {
            match cursor.kind() {
                ValueKind::Nil => break Ok(Value::NIL),
                ValueKind::Cons => {
                    let pair_car = cursor.cons_car();
                    let pair_cdr = cursor.cons_cdr();
                    if let ValueKind::Cons = pair_car.kind() {
                        let entry_key = pair_car.cons_car();
                        if equal_value_swp(key, &entry_key, 0, eval.symbols_with_pos_enabled) {
                            break Ok(pair_car);
                        }
                    }
                    cursor = pair_cdr;
                }
                _ => {
                    break Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("listp"), list],
                    ));
                }
            }
        };
        eval.restore_specpdl_roots(roots);
        assoc_result
    })
}

pub(crate) fn builtin_assq(args: Vec<Value>) -> EvalResult {
    builtin_assq_with_symbols(args, false)
}

pub(crate) fn builtin_assq_with_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_assq_with_symbols(args, eval.symbols_with_pos_enabled)
}

fn builtin_assq_with_symbols(args: Vec<Value>, symbols_with_pos_enabled: bool) -> EvalResult {
    expect_args("assq", &args, 2)?;
    let key = &args[0];
    let list = args[1];
    let mut cursor = list;
    loop {
        match cursor.kind() {
            ValueKind::Nil => return Ok(Value::NIL),
            ValueKind::Cons => {
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                if let ValueKind::Cons = pair_car.kind() {
                    let entry_key = pair_car.cons_car();
                    if eq_value_swp(key, &entry_key, symbols_with_pos_enabled) {
                        return Ok(pair_car);
                    }
                }
                cursor = pair_cdr;
            }
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), list],
                ));
            }
        }
    }
}

pub(crate) fn builtin_copy_sequence(args: Vec<Value>) -> EvalResult {
    expect_args("copy-sequence", &args, 1)?;
    match args[0].kind() {
        ValueKind::Nil => Ok(Value::NIL),
        ValueKind::Cons => {
            let mut items = Vec::new();
            let mut cursor = args[0];
            loop {
                match cursor.kind() {
                    ValueKind::Nil => break,
                    ValueKind::Cons => {
                        let pair_car = cursor.cons_car();
                        let pair_cdr = cursor.cons_cdr();
                        items.push(pair_car);
                        cursor = pair_cdr;
                    }
                    _ => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("listp"), cursor],
                        ));
                    }
                }
            }
            Ok(Value::list(items))
        }
        ValueKind::String => {
            let string = args[0]
                .as_lisp_string()
                .expect("ValueKind::String must carry LispString payload");
            // GNU Emacs: (copy-sequence "") returns "" itself (eq).
            if string.is_empty() {
                return Ok(args[0]);
            }
            let new_val = Value::heap_string(string.clone());
            // Copy text properties
            if new_val.is_string() {
                if let Some(table) = get_string_text_properties_table_for_value(args[0]) {
                    set_string_text_properties_table_for_value(new_val, table);
                }
            }
            Ok(new_val)
        }
        ValueKind::Veclike(VecLikeType::Vector) => {
            let elems = args[0].as_vector_data().unwrap().clone();
            // GNU Emacs: (copy-sequence (vector)) returns the same empty vector (eq).
            if elems.is_empty() {
                return Ok(args[0]);
            }
            Ok(Value::vector(elems))
        }
        ValueKind::Veclike(VecLikeType::Record) => {
            let items = args[0].as_record_data().unwrap().clone();
            Ok(Value::make_record(items))
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), args[0]],
        )),
    }
}

// ===========================================================================
// Extended list operations
// ===========================================================================

fn delete_from_list_in_place_result<F>(seq: &Value, mut should_delete: F) -> Result<Value, Flow>
where
    F: FnMut(&Value) -> Result<bool, Flow>,
{
    let mut probe = *seq;
    loop {
        match probe.kind() {
            ValueKind::Nil => break,
            ValueKind::Cons => {
                probe = probe.cons_cdr();
            }
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), probe],
                ));
            }
        }
    }

    let mut head = *seq;
    loop {
        match head.kind() {
            ValueKind::Nil => return Ok(Value::NIL),
            ValueKind::Cons => {
                let remove = {
                    let pair_car = head.cons_car();
                    should_delete(&pair_car)?
                };
                if remove {
                    head = head.cons_cdr();
                } else {
                    break;
                }
            }
            _ => unreachable!("list shape checked above"),
        }
    }

    if head.is_nil() {
        return Ok(Value::NIL);
    }

    let mut prev = head;

    loop {
        let next = prev.cons_cdr();
        match next.kind() {
            ValueKind::Nil => break,
            ValueKind::Cons => {
                let remove = {
                    let pair_car = next.cons_car();
                    should_delete(&pair_car)?
                };
                if remove {
                    let after = next.cons_cdr();
                    prev.set_cdr(after);
                } else {
                    prev = next;
                }
            }
            _ => unreachable!("list shape checked above"),
        }
    }

    Ok(head)
}

fn delete_from_list_in_place<F>(seq: &Value, should_delete: F) -> Result<Value, Flow>
where
    F: Fn(&Value) -> bool,
{
    delete_from_list_in_place_result(seq, |value| Ok(should_delete(value)))
}

pub(crate) fn builtin_delete(args: Vec<Value>) -> EvalResult {
    builtin_delete_with_symbols(args, false)
}

pub(crate) fn builtin_delete_with_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_delete_with_symbols(args, eval.symbols_with_pos_enabled)
}

fn builtin_delete_with_symbols(args: Vec<Value>, symbols_with_pos_enabled: bool) -> EvalResult {
    expect_args("delete", &args, 2)?;
    let elt = &args[0];
    match args[1].kind() {
        ValueKind::Nil => Ok(Value::NIL),
        ValueKind::Cons => delete_from_list_in_place(&args[1], |item| {
            equal_value_swp(elt, item, 0, symbols_with_pos_enabled)
        }),
        ValueKind::Veclike(VecLikeType::Vector) => {
            let items = args[1].as_vector_data().unwrap().clone();
            let mut changed = false;
            let mut kept = Vec::with_capacity(items.len());
            for item in items.iter() {
                if equal_value_swp(elt, item, 0, symbols_with_pos_enabled) {
                    changed = true;
                } else {
                    kept.push(*item);
                }
            }
            if changed {
                Ok(Value::vector(kept))
            } else {
                Ok(args[1])
            }
        }
        ValueKind::String => {
            let mut changed = false;
            let mut kept = Vec::new();
            let string = args[1].as_lisp_string().expect("string");
            for cp in super::lisp_string_char_codes(string) {
                let ch = Value::fixnum(cp as i64);
                if equal_value_swp(elt, &ch, 0, symbols_with_pos_enabled) {
                    changed = true;
                } else {
                    kept.push(ch);
                }
            }
            if !changed {
                return Ok(args[1]);
            }
            builtin_concat(vec![Value::list(kept)])
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), args[1]],
        )),
    }
}

pub(crate) fn builtin_delq(args: Vec<Value>) -> EvalResult {
    builtin_delq_with_symbols(args, false)
}

pub(crate) fn builtin_delq_with_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_delq_with_symbols(args, eval.symbols_with_pos_enabled)
}

fn builtin_delq_with_symbols(args: Vec<Value>, symbols_with_pos_enabled: bool) -> EvalResult {
    expect_args("delq", &args, 2)?;
    let elt = &args[0];
    match args[1].kind() {
        ValueKind::Nil => Ok(Value::NIL),
        ValueKind::Cons => delete_from_list_in_place(&args[1], |item| {
            eq_value_swp(elt, item, symbols_with_pos_enabled)
        }),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), args[1]],
        )),
    }
}

pub(crate) fn builtin_elt(args: Vec<Value>) -> EvalResult {
    expect_args("elt", &args, 2)?;
    match args[0].kind() {
        ValueKind::Cons | ValueKind::Nil | ValueKind::Veclike(VecLikeType::Lambda) => {
            builtin_nth(vec![args[1], args[0]])
        }
        ValueKind::Veclike(VecLikeType::Vector)
        | ValueKind::Veclike(VecLikeType::Record)
        | ValueKind::String => builtin_aref(vec![args[0], args[1]]),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), args[0]],
        )),
    }
}

pub(crate) fn builtin_nconc(args: Vec<Value>) -> EvalResult {
    if args.is_empty() {
        return Ok(Value::NIL);
    }

    let mut result_head: Option<Value> = None;
    let mut last_tail: Option<Value> = None;

    for (index, arg) in args.iter().enumerate() {
        let is_last = index + 1 == args.len();

        if is_last {
            if let Some(prev) = last_tail {
                prev.set_cdr(*arg);
                return Ok(result_head.unwrap_or(*arg));
            }
            return Ok(*arg);
        }

        match arg.kind() {
            ValueKind::Nil => continue,
            ValueKind::Cons => {
                if result_head.is_none() {
                    result_head = Some(*arg);
                }
                if let Some(prev) = last_tail {
                    prev.set_cdr(*arg);
                }

                let mut tail = *arg;
                loop {
                    let next = tail.cons_cdr();
                    match next.kind() {
                        ValueKind::Cons => tail = next,
                        _ => {
                            last_tail = Some(tail);
                            break;
                        }
                    }
                }
            }
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("consp"), *arg],
                ));
            }
        }
    }

    Ok(result_head.unwrap_or(Value::NIL))
}

// ===========================================================================
