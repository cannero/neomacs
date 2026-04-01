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
    let data = value.get_lambda_data()?.clone();
    let saved_roots = crate::emacs_core::eval::save_scratch_gc_roots();
    let mut elements = Vec::new();

    if let Some(env_val) = data.env {
        elements.push(Value::symbol("closure"));
        if env_val.is_nil() {
            elements.push(Value::list(vec![Value::T]));
        } else {
            elements.push(env_val);
        }
    } else {
        elements.push(Value::symbol("lambda"));
    }

    let params_value = lambda_params_to_value(&data.params);
    crate::emacs_core::eval::push_scratch_gc_root(params_value);
    elements.push(params_value);

    if let Some(ref doc) = data.docstring {
        let doc_value = Value::string(doc.clone());
        crate::emacs_core::eval::push_scratch_gc_root(doc_value);
        elements.push(doc_value);
    }

    // Include (:documentation TYPE) form for oclosures
    if let Some(doc_form) = data.doc_form {
        let doc_entry = Value::list(vec![Value::keyword(":documentation"), doc_form]);
        crate::emacs_core::eval::push_scratch_gc_root(doc_entry);
        elements.push(doc_entry);
    }

    for expr in data.body.iter() {
        let quoted = crate::emacs_core::eval::quote_to_value(expr);
        crate::emacs_core::eval::push_scratch_gc_root(quoted);
        elements.push(quoted);
    }

    let result = Value::list(elements);
    crate::emacs_core::eval::restore_scratch_gc_roots(saved_roots);
    Some(result)
}

pub(crate) fn lambda_closure_length(value: &Value) -> Option<i64> {
    let data = value.get_lambda_data()?;
    let has_doc_slot = data.doc_form.is_some() || data.docstring.is_some();
    Some(if has_doc_slot { 5 } else { 3 })
}

/// Convert a Lambda value to the GNU Emacs closure vector layout:
///   [0]=ARGS  [1]=BODY  [2]=ENV  [(3)=nil, (4)=DOCSTRING/TYPE]
/// NeoVM does not currently store the optional interactive slot.
/// This is used by `aref` on closures for oclosure slot access.
pub fn lambda_to_closure_vector(value: &Value) -> Vec<Value> {
    let data = match value.get_lambda_data() {
        Some(d) => d.clone(),
        None => return Vec::new(),
    };
    let saved_roots = crate::emacs_core::eval::save_scratch_gc_roots();

    let args = lambda_params_to_value(&data.params);
    crate::emacs_core::eval::push_scratch_gc_root(args);

    // Body: list of body forms
    let body_forms: Vec<Value> = data
        .body
        .iter()
        .map(|expr| {
            let quoted = crate::emacs_core::eval::quote_to_value(expr);
            crate::emacs_core::eval::push_scratch_gc_root(quoted);
            quoted
        })
        .collect();
    let body = Value::list(body_forms);
    crate::emacs_core::eval::push_scratch_gc_root(body);

    // Env — already stored as a flat cons alist matching GNU Emacs's
    // Vinternal_interpreter_environment.
    let env = match data.env {
        Some(env_val) if env_val.is_nil() => Value::list(vec![Value::T]),
        Some(env_val) => env_val,
        None => Value::NIL,
    };

    let mut result = vec![args, body, env];

    let slot4 = data
        .doc_form
        .or_else(|| data.docstring.as_ref().map(|d| Value::string(d.clone())));
    if let Some(slot4) = slot4 {
        result.push(Value::NIL);
        result.push(slot4);
    }
    crate::emacs_core::eval::restore_scratch_gc_roots(saved_roots);
    result
}

pub(crate) fn bytecode_closure_length(value: &Value) -> Option<i64> {
    let bc = value.get_bytecode_data()?;
    let has_doc_slot = bc.doc_form.is_some() || bc.docstring.is_some();
    Some(if has_doc_slot { 5 } else { 4 })
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

    let args = lambda_params_to_value(&bc.params);
    crate::emacs_core::eval::push_scratch_gc_root(args);

    // Slot 1: bytecode string — NeoVM uses decoded IR, not raw bytes
    let code = Value::NIL;

    // Slot 2: env if NeoVM-compiled (cons alist), else constants vector
    let env = if let Some(env_val) = bc.env {
        env_val
    } else {
        Value::vector(bc.constants.clone())
    };
    crate::emacs_core::eval::push_scratch_gc_root(env);

    // Slot 3: max stack depth
    let depth = Value::fixnum(bc.max_stack as i64);

    let mut result = vec![args, code, env, depth];

    let slot4 = bc
        .doc_form
        .or_else(|| bc.docstring.as_ref().map(|d| Value::string(d.clone())));
    if let Some(slot4) = slot4 {
        result.push(slot4);
    }
    crate::emacs_core::eval::restore_scratch_gc_roots(saved_roots);
    result
}

/// Convert LambdaParams to a Lisp list (a b &optional c &rest d).
fn lambda_params_to_value(params: &LambdaParams) -> Value {
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
            let data = value.get_lambda_data().unwrap();
            if data.env.is_some() {
                Ok(Value::symbol("closure"))
            } else {
                Ok(Value::symbol("lambda"))
            }
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), *value],
        )),
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
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), *value],
        )),
    }
}

pub(crate) fn builtin_car(args: Vec<Value>) -> EvalResult {
    expect_args("car", &args, 1)?;
    car_value(&args[0])
}

pub(crate) fn builtin_cdr(args: Vec<Value>) -> EvalResult {
    expect_args("cdr", &args, 1)?;
    cdr_value(&args[0])
}

pub(crate) fn builtin_car_safe(args: Vec<Value>) -> EvalResult {
    expect_args("car-safe", &args, 1)?;
    match args[0].kind() {
        ValueKind::Cons => Ok(args[0].cons_car()),
        _ => Ok(Value::NIL),
    }
}

pub(crate) fn builtin_cdr_safe(args: Vec<Value>) -> EvalResult {
    expect_args("cdr-safe", &args, 1)?;
    match args[0].kind() {
        ValueKind::Cons => Ok(args[0].cons_cdr()),
        _ => Ok(Value::NIL),
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
            None => Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("listp"), args[0]],
            )),
        },
        ValueKind::String => Ok(Value::fixnum(
            storage_char_len(args[0].as_str().unwrap()) as i64
        )),
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
        ValueKind::String => Ok((storage_char_len(sequence.as_str().unwrap()) as i64) < target),
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
        ValueKind::String => Ok((storage_char_len(sequence.as_str().unwrap()) as i64) == target),
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
        ValueKind::String => Ok((storage_char_len(sequence.as_str().unwrap()) as i64) > target),
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
                let s = arg.as_str().unwrap().to_owned();
                elements.extend(
                    decode_storage_char_codes(&s)
                        .into_iter()
                        .map(|cp| Value::fixnum(cp as i64)),
                );
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
            let mut items = args[0].as_vector_data().unwrap().clone();
            items.reverse();
            Ok(Value::vector(items))
        }
        ValueKind::String => {
            let s = args[0].as_str().unwrap().to_owned();
            let reversed: String = s.chars().rev().collect();
            Ok(Value::string(reversed))
        }
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
            args[0].as_vector_data_mut().unwrap().reverse();
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
    expect_args("member", &args, 2)?;
    let target = &args[0];
    let list = args[1];
    let mut cursor = list;
    loop {
        match cursor.kind() {
            ValueKind::Nil => return Ok(Value::NIL),
            ValueKind::Cons => {
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                if equal_value(target, &pair_car, 0) {
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
                if eq_value(target, &pair_car) {
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
                if eql_value(target, &pair_car) {
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
            return eval.with_gc_scope_result(|ctx| {
                ctx.root(*key);
                ctx.root(list);
                ctx.root(test_fn.unwrap());
                let mut cursor = list;
                loop {
                    match cursor.kind() {
                        ValueKind::Nil => return Ok(Value::NIL),
                        ValueKind::Cons => {
                            let pair_car = cursor.cons_car();
                            let pair_cdr = cursor.cons_cdr();
                            if let ValueKind::Cons = pair_car.kind() {
                                let entry_key = pair_car.cons_car();
                                let matches = if let Some(test_fn) = &test_fn {
                                    ctx.apply(*test_fn, vec![entry_key, *key])?.is_truthy()
                                } else {
                                    equal_value(key, &entry_key, 0)
                                };
                                if matches {
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
            });
        }
        // No test_fn: simple equal-based traversal (no rooting needed)
        eval.with_gc_scope_result(|ctx| {
            ctx.root(list);
            let mut cursor = list;
            loop {
                match cursor.kind() {
                    ValueKind::Nil => return Ok(Value::NIL),
                    ValueKind::Cons => {
                        let pair_car = cursor.cons_car();
                        let pair_cdr = cursor.cons_cdr();
                        if let ValueKind::Cons = pair_car.kind() {
                            let entry_key = pair_car.cons_car();
                            if equal_value(key, &entry_key, 0) {
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
        })
    })
}

pub(crate) fn builtin_assq(args: Vec<Value>) -> EvalResult {
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
                    if eq_value(key, &entry_key) {
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
            let s = args[0].as_str().unwrap().to_owned();
            // GNU Emacs: (copy-sequence "") returns "" itself (eq).
            if s.is_empty() {
                return Ok(args[0]);
            }
            let new_val = Value::string(&s);
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
    expect_args("delete", &args, 2)?;
    let elt = &args[0];
    match args[1].kind() {
        ValueKind::Nil => Ok(Value::NIL),
        ValueKind::Cons => delete_from_list_in_place(&args[1], |item| equal_value(elt, item, 0)),
        ValueKind::Veclike(VecLikeType::Vector) => {
            let items = args[1].as_vector_data().unwrap().clone();
            let mut changed = false;
            let mut kept = Vec::with_capacity(items.len());
            for item in items.iter() {
                if equal_value(elt, item, 0) {
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
            let s = args[1].as_str().unwrap().to_owned();
            for cp in decode_storage_char_codes(&s) {
                let ch = Value::fixnum(cp as i64);
                if equal_value(elt, &ch, 0) {
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
    expect_args("delq", &args, 2)?;
    let elt = &args[0];
    match args[1].kind() {
        ValueKind::Nil => Ok(Value::NIL),
        ValueKind::Cons => delete_from_list_in_place(&args[1], |item| eq_value(elt, item)),
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
