use super::*;

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
// NeoVM uses Value::Lambda(ObjId) internally.  The helpers below let all
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
            elements.push(Value::list(vec![Value::True]));
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
pub(crate) fn lambda_to_closure_vector(value: &Value) -> Vec<Value> {
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
        Some(env_val) if env_val.is_nil() => Value::list(vec![Value::True]),
        Some(env_val) => env_val,
        None => Value::Nil,
    };

    let mut result = vec![args, body, env];

    let slot4 = data
        .doc_form
        .or_else(|| data.docstring.as_ref().map(|d| Value::string(d.clone())));
    if let Some(slot4) = slot4 {
        result.push(Value::Nil);
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
    match value {
        Value::Lambda(_) => lambda_closure_length(value),
        Value::ByteCode(_) => bytecode_closure_length(value),
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
    let code = Value::Nil;

    // Slot 2: env if NeoVM-compiled (cons alist), else constants vector
    let env = if let Some(env_val) = bc.env {
        env_val
    } else {
        Value::vector(bc.constants.clone())
    };
    crate::emacs_core::eval::push_scratch_gc_root(env);

    // Slot 3: max stack depth
    let depth = Value::Int(bc.max_stack as i64);

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
        elements.push(Value::Symbol(*p));
    }
    if !params.optional.is_empty() {
        elements.push(Value::symbol("&optional"));
        for p in &params.optional {
            elements.push(Value::Symbol(*p));
        }
    }
    if let Some(ref rest) = params.rest {
        elements.push(Value::symbol("&rest"));
        elements.push(Value::Symbol(*rest));
    }
    Value::list(elements)
}

fn car_value(value: &Value) -> Result<Value, Flow> {
    match value {
        Value::Nil => Ok(Value::Nil),
        Value::Cons(cell) => Ok(with_heap(|h| h.cons_car(*cell))),
        // In official Emacs, closures are cons lists with car = closure/lambda.
        Value::Lambda(_) => {
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
    match value {
        Value::Nil => Ok(Value::Nil),
        Value::Cons(cell) => Ok(with_heap(|h| h.cons_cdr(*cell))),
        // Convert Lambda to cons list, return cdr.
        Value::Lambda(_) => {
            let list = lambda_to_cons_list(value).unwrap_or(Value::Nil);
            match list {
                Value::Cons(cell) => Ok(with_heap(|h| h.cons_cdr(cell))),
                _ => Ok(Value::Nil),
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
    match &args[0] {
        Value::Cons(cell) => Ok(with_heap(|h| h.cons_car(*cell))),
        _ => Ok(Value::Nil),
    }
}

pub(crate) fn builtin_cdr_safe(args: Vec<Value>) -> EvalResult {
    expect_args("cdr-safe", &args, 1)?;
    match &args[0] {
        Value::Cons(cell) => Ok(with_heap(|h| h.cons_cdr(*cell))),
        _ => Ok(Value::Nil),
    }
}

pub(crate) fn builtin_setcar(args: Vec<Value>) -> EvalResult {
    expect_args("setcar", &args, 2)?;
    match &args[0] {
        Value::Cons(cell) => {
            with_heap_mut(|h| h.set_car(*cell, args[1]));
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
    match &args[0] {
        Value::Cons(cell) => {
            with_heap_mut(|h| h.set_cdr(*cell, args[1]));
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
    match &args[0] {
        Value::Nil => Ok(Value::Int(0)),
        Value::Lambda(_) | Value::ByteCode(_) => {
            Ok(Value::Int(closure_vector_length(&args[0]).unwrap()))
        }
        Value::Cons(_) => match list_length(&args[0]) {
            Some(n) => Ok(Value::Int(n as i64)),
            None => Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("listp"), args[0]],
            )),
        },
        Value::Str(id) => Ok(Value::Int(
            with_heap(|h| storage_char_len(h.get_string(*id))) as i64,
        )),
        Value::Vector(v) | Value::Record(v) => Ok(Value::Int(vector_sequence_length(&args[0], *v))),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), args[0]],
        )),
    }
}

fn vector_sequence_length(sequence: &Value, vector: ObjId) -> i64 {
    super::chartable::bool_vector_length(sequence)
        .or_else(|| super::chartable::char_table_length(sequence))
        .unwrap_or_else(|| with_heap(|h| h.vector_len(vector)) as i64)
}

fn sequence_length_less_than(sequence: &Value, target: i64) -> Result<bool, Flow> {
    match sequence {
        Value::Nil => Ok(0 < target),
        Value::Lambda(_) | Value::ByteCode(_) => {
            Ok(closure_vector_length(sequence).unwrap() < target)
        }
        Value::Str(id) => Ok((with_heap(|h| storage_char_len(h.get_string(*id))) as i64) < target),
        Value::Vector(v) | Value::Record(v) => Ok(vector_sequence_length(sequence, *v) < target),
        Value::Cons(_) => {
            if target <= 0 {
                return Ok(false);
            }
            let mut remaining = target;
            let mut cursor = *sequence;
            while remaining > 0 {
                match cursor {
                    Value::Cons(cell) => {
                        cursor = with_heap(|h| h.cons_cdr(cell));
                        remaining -= 1;
                    }
                    _ => return Ok(true),
                }
            }
            Ok(false)
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), *other],
        )),
    }
}

fn sequence_length_equal(sequence: &Value, target: i64) -> Result<bool, Flow> {
    match sequence {
        Value::Nil => Ok(target == 0),
        Value::Lambda(_) | Value::ByteCode(_) => {
            Ok(closure_vector_length(sequence).unwrap() == target)
        }
        Value::Str(id) => Ok((with_heap(|h| storage_char_len(h.get_string(*id))) as i64) == target),
        Value::Vector(v) | Value::Record(v) => Ok(vector_sequence_length(sequence, *v) == target),
        Value::Cons(_) => {
            if target < 0 {
                return Ok(false);
            }
            let mut remaining = target;
            let mut cursor = *sequence;
            while remaining > 0 {
                match cursor {
                    Value::Cons(cell) => {
                        cursor = with_heap(|h| h.cons_cdr(cell));
                        remaining -= 1;
                    }
                    _ => return Ok(false),
                }
            }
            Ok(!matches!(cursor, Value::Cons(_)))
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), *other],
        )),
    }
}

fn sequence_length_greater_than(sequence: &Value, target: i64) -> Result<bool, Flow> {
    match sequence {
        Value::Nil => Ok(0 > target),
        Value::Lambda(_) | Value::ByteCode(_) => {
            Ok(closure_vector_length(sequence).unwrap() > target)
        }
        Value::Str(id) => Ok((with_heap(|h| storage_char_len(h.get_string(*id))) as i64) > target),
        Value::Vector(v) | Value::Record(v) => Ok(vector_sequence_length(sequence, *v) > target),
        Value::Cons(_) => {
            if target < 0 {
                return Ok(true);
            }
            if target == i64::MAX {
                return Ok(false);
            }
            let mut remaining = target + 1;
            let mut cursor = *sequence;
            while remaining > 0 {
                match cursor {
                    Value::Cons(cell) => {
                        cursor = with_heap(|h| h.cons_cdr(cell));
                        remaining -= 1;
                    }
                    _ => return Ok(false),
                }
            }
            Ok(true)
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), *other],
        )),
    }
}

pub(crate) fn builtin_length_lt(args: Vec<Value>) -> EvalResult {
    expect_args("length<", &args, 2)?;
    let target = expect_fixnum(&args[1])?;
    Ok(Value::bool(sequence_length_less_than(&args[0], target)?))
}

pub(crate) fn builtin_length_eq(args: Vec<Value>) -> EvalResult {
    expect_args("length=", &args, 2)?;
    let target = expect_fixnum(&args[1])?;
    Ok(Value::bool(sequence_length_equal(&args[0], target)?))
}

pub(crate) fn builtin_length_gt(args: Vec<Value>) -> EvalResult {
    expect_args("length>", &args, 2)?;
    let target = expect_fixnum(&args[1])?;
    Ok(Value::bool(sequence_length_greater_than(&args[0], target)?))
}

pub(crate) fn builtin_nth(args: Vec<Value>) -> EvalResult {
    expect_args("nth", &args, 2)?;
    let n = expect_int(&args[0])?;
    let tail = nthcdr_impl(n, args[1])?;
    match tail {
        Value::Cons(cell) => Ok(with_heap(|h| h.cons_car(cell))),
        Value::Nil => Ok(Value::Nil),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), other],
        )),
    }
}

fn nthcdr_impl(n: i64, list: Value) -> EvalResult {
    if n <= 0 {
        return Ok(list);
    }

    // Convert Lambda to cons list for traversal.
    let mut cursor = match list {
        Value::Lambda(_) => lambda_to_cons_list(&list).unwrap_or(Value::Nil),
        other => other,
    };
    for _ in 0..(n as usize) {
        match cursor {
            Value::Cons(cell) => {
                cursor = with_heap(|h| h.cons_cdr(cell));
            }
            Value::Nil => return Ok(Value::Nil),
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
            match cursor {
                Value::Nil => return Ok(()),
                Value::Cons(cell) => {
                    let pair = read_cons(cell);
                    out.push(pair.car);
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

    if args.is_empty() {
        return Ok(Value::Nil);
    }
    if args.len() == 1 {
        return Ok(args[0]);
    }

    // Collect all elements from all lists except the last, then use last as tail
    let mut elements: Vec<Value> = Vec::new();
    for arg in &args[..args.len() - 1] {
        match arg {
            Value::Nil => {}
            Value::Cons(_) => extend_from_proper_list(&mut elements, arg)?,
            Value::Lambda(_) => elements.extend(lambda_to_closure_vector(arg).into_iter()),
            Value::ByteCode(_) => elements.extend(bytecode_to_closure_vector(arg).into_iter()),
            Value::Vector(v) => {
                elements.extend(with_heap(|h| h.get_vector(*v).clone()).into_iter())
            }
            Value::Str(id) => {
                let s = with_heap(|h| h.get_string(*id).to_owned());
                elements.extend(
                    decode_storage_char_codes(&s)
                        .into_iter()
                        .map(|cp| Value::Int(cp as i64)),
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
    match &args[0] {
        Value::Nil => Ok(Value::Nil),
        Value::Cons(_) => {
            let items = list_to_vec(&args[0]).ok_or_else(|| {
                signal("wrong-type-argument", vec![Value::symbol("listp"), args[0]])
            })?;
            let mut reversed = items;
            reversed.reverse();
            Ok(Value::list(reversed))
        }
        Value::Vector(v) => {
            let mut items = with_heap(|h| h.get_vector(*v).clone());
            items.reverse();
            Ok(Value::vector(items))
        }
        Value::Str(id) => {
            let s = with_heap(|h| h.get_string(*id).to_owned());
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
            match cursor {
                Value::Cons(cell) => {
                    let pair = read_cons(cell);
                    prefix.push(pair.car);
                    cursor = pair.cdr;
                }
                Value::Nil => return None,
                _ => return Some(Value::list(prefix)),
            }
        }
    }

    expect_args("nreverse", &args, 1)?;
    match &args[0] {
        Value::Nil => Ok(Value::Nil),
        Value::Cons(_) => {
            // Match Emacs list semantics: reject dotted lists with proper-prefix payload.
            if let Some(prefix) = dotted_list_prefix(&args[0]) {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), prefix],
                ));
            }

            let mut prev = Value::Nil;
            let mut current = args[0];
            loop {
                match current {
                    Value::Nil => return Ok(prev),
                    Value::Cons(cell) => {
                        let next = with_heap(|h| h.cons_cdr(cell));
                        with_heap_mut(|h| h.set_cdr(cell, prev));
                        prev = Value::Cons(cell);
                        current = next;
                    }
                    _ => unreachable!("proper-list check should reject dotted tails"),
                }
            }
        }
        Value::Vector(v) => {
            with_heap_mut(|h| h.get_vector_mut(*v).reverse());
            Ok(args[0])
        }
        Value::Str(_) => builtin_reverse(args),
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
        match cursor {
            Value::Nil => return Ok(Value::Nil),
            Value::Cons(cell) => {
                let pair = read_cons(cell);
                if equal_value(target, &pair.car, 0) {
                    drop(pair);
                    return Ok(Value::Cons(cell));
                }
                cursor = pair.cdr;
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
        match cursor {
            Value::Nil => return Ok(Value::Nil),
            Value::Cons(cell) => {
                let pair = read_cons(cell);
                if eq_value(target, &pair.car) {
                    drop(pair);
                    return Ok(Value::Cons(cell));
                }
                cursor = pair.cdr;
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
        match cursor {
            Value::Nil => return Ok(Value::Nil),
            Value::Cons(cell) => {
                let pair = read_cons(cell);
                if eql_value(target, &pair.car) {
                    drop(pair);
                    return Ok(Value::Cons(cell));
                }
                cursor = pair.cdr;
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

pub(crate) fn builtin_assoc(args: Vec<Value>) -> EvalResult {
    crate::emacs_core::perf_trace::time_op(crate::emacs_core::perf_trace::HotpathOp::Assoc, || {
        expect_args("assoc", &args, 2)?;
        let key = &args[0];
        let list = args[1];
        let mut cursor = list;
        loop {
            match cursor {
                Value::Nil => return Ok(Value::Nil),
                Value::Cons(cell) => {
                    let pair = read_cons(cell);
                    if let Value::Cons(ref entry) = pair.car {
                        let entry_pair = read_cons(*entry);
                        if equal_value(key, &entry_pair.car, 0) {
                            return Ok(pair.car);
                        }
                    }
                    cursor = pair.cdr;
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
}

pub(crate) fn builtin_assoc_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    crate::emacs_core::perf_trace::time_op(crate::emacs_core::perf_trace::HotpathOp::Assoc, || {
        expect_range_args("assoc", &args, 2, 3)?;
        let key = &args[0];
        let list = args[1];
        let test_fn = args
            .get(2)
            .and_then(|value| if value.is_nil() { None } else { Some(*value) });
        if test_fn.is_some() {
            let saved = eval.save_temp_roots();
            eval.push_temp_root(*key);
            eval.push_temp_root(list);
            eval.push_temp_root(test_fn.unwrap());
            let result = builtin_assoc_eval_inner(eval, key, list, &test_fn);
            eval.restore_temp_roots(saved);
            return result;
        }
        builtin_assoc_eval_inner(eval, key, list, &test_fn)
    })
}

fn builtin_assoc_eval_inner(
    eval: &mut super::eval::Evaluator,
    key: &Value,
    list: Value,
    test_fn: &Option<Value>,
) -> EvalResult {
    // key, list, and test_fn are already rooted by the caller
    // (builtin_assoc_eval).  Root cursor too since it traverses
    // the list and may become the only reference to a cons cell
    // if the predicate mutates the list.
    let saved_roots = eval.save_temp_roots();
    eval.push_temp_root(list);

    let result = (|| -> EvalResult {
        let mut cursor = list;
        loop {
            match cursor {
                Value::Nil => return Ok(Value::Nil),
                Value::Cons(cell) => {
                    let pair = read_cons(cell);
                    if let Value::Cons(ref entry) = pair.car {
                        let entry_pair = read_cons(*entry);
                        let matches = if let Some(test_fn) = test_fn {
                            // GNU Emacs calls (TESTFN ALIST-KEY SEARCH-KEY)
                            eval.apply(*test_fn, vec![entry_pair.car, *key])?
                                .is_truthy()
                        } else {
                            equal_value(key, &entry_pair.car, 0)
                        };
                        if matches {
                            return Ok(pair.car);
                        }
                    }
                    cursor = pair.cdr;
                }
                _ => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("listp"), list],
                    ));
                }
            }
        }
    })();

    eval.restore_temp_roots(saved_roots);
    result
}

pub(crate) fn builtin_assq(args: Vec<Value>) -> EvalResult {
    expect_args("assq", &args, 2)?;
    let key = &args[0];
    let list = args[1];
    let mut cursor = list;
    loop {
        match cursor {
            Value::Nil => return Ok(Value::Nil),
            Value::Cons(cell) => {
                let pair = read_cons(cell);
                if let Value::Cons(ref entry) = pair.car {
                    let entry_pair = read_cons(*entry);
                    if eq_value(key, &entry_pair.car) {
                        return Ok(pair.car);
                    }
                }
                cursor = pair.cdr;
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
    match &args[0] {
        Value::Nil => Ok(Value::Nil),
        Value::Cons(_) => {
            let mut items = Vec::new();
            let mut cursor = args[0];
            loop {
                match cursor {
                    Value::Nil => break,
                    Value::Cons(cell) => {
                        let pair = read_cons(cell);
                        items.push(pair.car);
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
            Ok(Value::list(items))
        }
        Value::Str(id) => {
            let s = with_heap(|h| h.get_string(*id).to_owned());
            let new_val = Value::string(&s);
            // Copy text properties
            if let Value::Str(new_id) = &new_val {
                if let Some(table) = get_string_text_properties_table(*id) {
                    set_string_text_properties_table(*new_id, table);
                }
            }
            Ok(new_val)
        }
        Value::Vector(v) => Ok(Value::vector(with_heap(|h| h.get_vector(*v).clone()))),
        Value::Record(v) => {
            let items = with_heap(|h| h.get_vector(*v).clone());
            let id = with_heap_mut(|h| h.alloc_vector(items));
            Ok(Value::Record(id))
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), *other],
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
        match probe {
            Value::Nil => break,
            Value::Cons(cell) => {
                probe = with_heap(|h| h.cons_cdr(cell));
            }
            tail => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), tail],
                ));
            }
        }
    }

    let mut head = *seq;
    loop {
        match head {
            Value::Nil => return Ok(Value::Nil),
            Value::Cons(cell) => {
                let remove = {
                    let pair = read_cons(cell);
                    should_delete(&pair.car)?
                };
                if remove {
                    head = with_heap(|h| h.cons_cdr(cell));
                } else {
                    break;
                }
            }
            _ => unreachable!("list shape checked above"),
        }
    }

    let mut prev = match &head {
        Value::Cons(cell) => *cell,
        Value::Nil => return Ok(Value::Nil),
        _ => unreachable!("head must be list"),
    };

    loop {
        let next = with_heap(|h| h.cons_cdr(prev));
        match next {
            Value::Nil => break,
            Value::Cons(next_cell) => {
                let remove = {
                    let pair = read_cons(next_cell);
                    should_delete(&pair.car)?
                };
                if remove {
                    let after = with_heap(|h| h.cons_cdr(next_cell));
                    with_heap_mut(|h| h.set_cdr(prev, after));
                } else {
                    prev = next_cell;
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
    match &args[1] {
        Value::Nil => Ok(Value::Nil),
        Value::Cons(_) => delete_from_list_in_place(&args[1], |item| equal_value(elt, item, 0)),
        Value::Vector(v) => {
            let items = with_heap(|h| h.get_vector(*v).clone());
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
        Value::Str(id) => {
            let mut changed = false;
            let mut kept = Vec::new();
            let s = with_heap(|h| h.get_string(*id).to_owned());
            for cp in decode_storage_char_codes(&s) {
                let ch = Value::Int(cp as i64);
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
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), *other],
        )),
    }
}

pub(crate) fn builtin_delq(args: Vec<Value>) -> EvalResult {
    expect_args("delq", &args, 2)?;
    let elt = &args[0];
    match &args[1] {
        Value::Nil => Ok(Value::Nil),
        Value::Cons(_) => delete_from_list_in_place(&args[1], |item| eq_value(elt, item)),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), args[1]],
        )),
    }
}

pub(crate) fn builtin_elt(args: Vec<Value>) -> EvalResult {
    expect_args("elt", &args, 2)?;
    match &args[0] {
        Value::Cons(_) | Value::Nil | Value::Lambda(_) => builtin_nth(vec![args[1], args[0]]),
        Value::Vector(_) | Value::Record(_) | Value::Str(_) => builtin_aref(vec![args[0], args[1]]),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), *other],
        )),
    }
}

pub(crate) fn builtin_nconc(args: Vec<Value>) -> EvalResult {
    if args.is_empty() {
        return Ok(Value::Nil);
    }

    let mut result_head: Option<Value> = None;
    let mut last_cons: Option<Value> = None;

    for (index, arg) in args.iter().enumerate() {
        let is_last = index + 1 == args.len();

        if is_last {
            if let Some(Value::Cons(cell)) = &last_cons {
                with_heap_mut(|h| h.set_cdr(*cell, *arg));
                return Ok(result_head.unwrap_or(*arg));
            }
            return Ok(*arg);
        }

        match arg {
            Value::Nil => continue,
            Value::Cons(head) => {
                if result_head.is_none() {
                    result_head = Some(*arg);
                }
                if let Some(Value::Cons(prev)) = &last_cons {
                    with_heap_mut(|h| h.set_cdr(*prev, *arg));
                }

                let mut tail = *head;
                loop {
                    let next = with_heap(|h| h.cons_cdr(tail));
                    match next {
                        Value::Cons(next_cell) => tail = next_cell,
                        _ => {
                            last_cons = Some(Value::Cons(tail));
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

    Ok(result_head.unwrap_or(Value::Nil))
}

// ===========================================================================
