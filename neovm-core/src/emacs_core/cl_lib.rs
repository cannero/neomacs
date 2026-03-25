//! CL-lib, seq.el, and JSON built-in functions.
//!
//! Provides Common Lisp compatibility functions, sequence operations,
//! and JSON parsing/serialization for the Elisp interpreter.

use super::error::{EvalResult, Flow, signal};
use super::intern::{intern, resolve_sym};
use super::value::*;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// Argument helpers (local copies for this module)
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
#[cfg(test)]

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

#[cfg(test)]
static CL_GENSYM_COUNTER: AtomicU64 = AtomicU64::new(0);

fn expect_int(val: &Value) -> Result<i64, Flow> {
    match val {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *other],
        )),
    }
}

fn expect_number_or_marker(val: &Value) -> Result<f64, Flow> {
    match val {
        Value::Int(n) => Ok(*n as f64),
        Value::Float(f, _) => Ok(*f),
        Value::Char(c) => Ok(*c as i64 as f64),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), *other],
        )),
    }
}

/// Collect elements from any sequence type into a Vec.
fn collect_sequence(val: &Value) -> Vec<Value> {
    match val {
        Value::Nil => Vec::new(),
        Value::Cons(_) => list_to_vec(val).unwrap_or_default(),
        Value::Vector(v) => with_heap(|h| h.get_vector(*v).clone()),
        Value::Str(s) => with_heap(|h| h.get_string(*s).chars().map(Value::Char).collect()),
        _ => vec![*val],
    }
}
#[cfg(test)]

fn cl_list_nth(list: &Value, index: usize) -> EvalResult {
    let mut cursor = *list;
    for _ in 0..index {
        match cursor {
            Value::Nil => return Ok(Value::Nil),
            Value::Cons(cell) => {
                let pair = read_cons(cell);
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

    match cursor {
        Value::Nil => Ok(Value::Nil),
        Value::Cons(cell) => Ok(with_heap(|h| h.cons_car(cell))),
        tail => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), tail],
        )),
    }
}

/// `(cl-first LIST)` -- return the first element of LIST.
#[cfg(test)]
pub(crate) fn builtin_cl_first(args: Vec<Value>) -> EvalResult {
    expect_args("cl-first", &args, 1)?;
    cl_list_nth(&args[0], 0)
}

/// `(cl-second LIST)` -- return the second element of LIST.
#[cfg(test)]
pub(crate) fn builtin_cl_second(args: Vec<Value>) -> EvalResult {
    expect_args("cl-second", &args, 1)?;
    cl_list_nth(&args[0], 1)
}

/// `(cl-third LIST)` -- return the third element of LIST.
#[cfg(test)]
pub(crate) fn builtin_cl_third(args: Vec<Value>) -> EvalResult {
    expect_args("cl-third", &args, 1)?;
    cl_list_nth(&args[0], 2)
}

/// `(cl-fourth LIST)` -- return the fourth element of LIST.
#[cfg(test)]
pub(crate) fn builtin_cl_fourth(args: Vec<Value>) -> EvalResult {
    expect_args("cl-fourth", &args, 1)?;
    cl_list_nth(&args[0], 3)
}

/// `(cl-fifth LIST)` -- return the fifth element of LIST.
#[cfg(test)]
pub(crate) fn builtin_cl_fifth(args: Vec<Value>) -> EvalResult {
    expect_args("cl-fifth", &args, 1)?;
    cl_list_nth(&args[0], 4)
}

/// `(cl-sixth LIST)` -- return the sixth element of LIST.
#[cfg(test)]
pub(crate) fn builtin_cl_sixth(args: Vec<Value>) -> EvalResult {
    expect_args("cl-sixth", &args, 1)?;
    cl_list_nth(&args[0], 5)
}

/// `(cl-seventh LIST)` -- return the seventh element of LIST.
#[cfg(test)]
pub(crate) fn builtin_cl_seventh(args: Vec<Value>) -> EvalResult {
    expect_args("cl-seventh", &args, 1)?;
    cl_list_nth(&args[0], 6)
}

/// `(cl-eighth LIST)` -- return the eighth element of LIST.
#[cfg(test)]
pub(crate) fn builtin_cl_eighth(args: Vec<Value>) -> EvalResult {
    expect_args("cl-eighth", &args, 1)?;
    cl_list_nth(&args[0], 7)
}

/// `(cl-ninth LIST)` -- return the ninth element of LIST.
#[cfg(test)]
pub(crate) fn builtin_cl_ninth(args: Vec<Value>) -> EvalResult {
    expect_args("cl-ninth", &args, 1)?;
    cl_list_nth(&args[0], 8)
}

/// `(cl-tenth LIST)` -- return the tenth element of LIST.
#[cfg(test)]
pub(crate) fn builtin_cl_tenth(args: Vec<Value>) -> EvalResult {
    expect_args("cl-tenth", &args, 1)?;
    cl_list_nth(&args[0], 9)
}

/// `(cl-rest LIST)` -- return the tail (cdr) of LIST.
#[cfg(test)]
pub(crate) fn builtin_cl_rest(args: Vec<Value>) -> EvalResult {
    expect_args("cl-rest", &args, 1)?;
    match &args[0] {
        Value::Nil => Ok(Value::Nil),
        Value::Cons(cell) => Ok(with_heap(|h| h.cons_cdr(*cell))),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), *other],
        )),
    }
}

/// `(cl-evenp N)` -- return t if N is even.
#[cfg(test)]
pub(crate) fn builtin_cl_evenp(args: Vec<Value>) -> EvalResult {
    expect_args("cl-evenp", &args, 1)?;
    let n = expect_int(&args[0])?;
    Ok(Value::bool(n % 2 == 0))
}

/// `(cl-oddp N)` -- return t if N is odd.
#[cfg(test)]
pub(crate) fn builtin_cl_oddp(args: Vec<Value>) -> EvalResult {
    expect_args("cl-oddp", &args, 1)?;
    let n = expect_int(&args[0])?;
    Ok(Value::bool(n % 2 != 0))
}

/// `(cl-plusp N)` -- return t if N is strictly positive.
#[cfg(test)]
pub(crate) fn builtin_cl_plusp(args: Vec<Value>) -> EvalResult {
    expect_args("cl-plusp", &args, 1)?;
    let n = expect_number_or_marker(&args[0])?;
    Ok(Value::bool(n > 0.0))
}

/// `(cl-minusp N)` -- return t if N is strictly negative.
#[cfg(test)]
pub(crate) fn builtin_cl_minusp(args: Vec<Value>) -> EvalResult {
    expect_args("cl-minusp", &args, 1)?;
    let n = expect_number_or_marker(&args[0])?;
    Ok(Value::bool(n < 0.0))
}

/// `(cl-member ITEM LIST)` -- CL alias for `member`.
#[cfg(test)]
pub(crate) fn builtin_cl_member(args: Vec<Value>) -> EvalResult {
    super::builtins::builtin_member(args)
}

/// `(cl-coerce OBJECT TYPE)` -- coerce a sequence to TYPE.
#[cfg(test)]
pub(crate) fn builtin_cl_coerce(args: Vec<Value>) -> EvalResult {
    expect_args("cl-coerce", &args, 2)?;
    builtin_seq_concatenate(vec![args[1], args[0]])
}

/// `(cl-adjoin ITEM LIST)` -- add ITEM to LIST if not already present.
#[cfg(test)]
pub(crate) fn builtin_cl_adjoin(args: Vec<Value>) -> EvalResult {
    expect_args("cl-adjoin", &args, 2)?;
    let item = args[0];
    let list = args[1];
    let found = super::builtins::builtin_member(vec![item, list])?;
    if found.is_truthy() {
        Ok(list)
    } else {
        Ok(Value::cons(item, list))
    }
}

/// `(cl-remove ITEM LIST)` -- CL alias for `remove`.
#[cfg(test)]
pub(crate) fn builtin_cl_remove(args: Vec<Value>) -> EvalResult {
    super::builtins_extra::remove_list_equal(args)
}

fn seq_position_list_elements(seq: &Value) -> Result<Vec<Value>, Flow> {
    let mut elements = Vec::new();
    let mut cursor = *seq;
    loop {
        match cursor {
            Value::Nil => return Ok(elements),
            Value::Cons(cell) => {
                let pair = read_cons(cell);
                elements.push(pair.car);
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

fn seq_position_elements(seq: &Value) -> Result<Vec<Value>, Flow> {
    match seq {
        Value::Nil => Ok(Vec::new()),
        Value::Cons(_) => seq_position_list_elements(seq),
        Value::Vector(v) => Ok(with_heap(|h| h.get_vector(*v).clone())),
        Value::Str(s) => Ok(with_heap(|h| {
            h.get_string(*s)
                .chars()
                .map(|ch| Value::Int(ch as i64))
                .collect()
        })),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), *other],
        )),
    }
}

fn seq_default_match(left: &Value, right: &Value) -> bool {
    if equal_value(left, right, 0) {
        return true;
    }
    match (left, right) {
        (Value::Char(a), Value::Int(b)) => (*a as i64) == *b,
        (Value::Int(a), Value::Char(b)) => *a == (*b as i64),
        _ => false,
    }
}

fn seq_collect_concat_arg(arg: &Value) -> Result<Vec<Value>, Flow> {
    match arg {
        Value::Nil => Ok(Vec::new()),
        Value::Cons(_) => {
            let mut out = Vec::new();
            let mut cursor = *arg;
            loop {
                match cursor {
                    Value::Nil => return Ok(out),
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
        Value::Vector(v) => Ok(with_heap(|h| h.get_vector(*v).clone())),
        Value::Str(s) => Ok(with_heap(|h| {
            h.get_string(*s)
                .chars()
                .map(|ch| Value::Int(ch as i64))
                .collect()
        })),
        other => Err(signal(
            "error",
            vec![Value::string(format!(
                "Cannot convert {} into a sequence",
                super::print::print_value(other)
            ))],
        )),
    }
}

// ===========================================================================
// Seq.el pure operations
// ===========================================================================

/// `(seq-reverse SEQ)` — reverse a sequence.
pub(crate) fn builtin_seq_reverse(args: Vec<Value>) -> EvalResult {
    expect_args("seq-reverse", &args, 1)?;
    let mut elems = seq_position_elements(&args[0])?;
    elems.reverse();
    match &args[0] {
        Value::Vector(_) => Ok(Value::vector(elems)),
        Value::Str(_) => {
            let mut s = String::new();
            for value in &elems {
                let ch = match value {
                    Value::Char(c) => *c,
                    Value::Int(n) => char::from_u32(*n as u32).ok_or_else(|| {
                        signal(
                            "wrong-type-argument",
                            vec![Value::symbol("characterp"), *value],
                        )
                    })?,
                    other => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("characterp"), *other],
                        ));
                    }
                };
                s.push(ch);
            }
            Ok(Value::string(s))
        }
        _ => Ok(Value::list(elems)),
    }
}

/// `(seq-drop SEQ N)` — drop first n elements.
pub(crate) fn builtin_seq_drop(args: Vec<Value>) -> EvalResult {
    expect_args("seq-drop", &args, 2)?;
    let n = expect_int(&args[1])?;

    match &args[0] {
        Value::Nil => Ok(Value::Nil),
        Value::Vector(v) => {
            let elems = with_heap(|h| h.get_vector(*v).clone());
            if n <= 0 {
                return Ok(Value::vector(elems.clone()));
            }
            let n = (n as usize).min(elems.len());
            Ok(Value::vector(elems[n..].to_vec()))
        }
        Value::Str(s) => {
            let string = with_heap(|h| h.get_string(*s).to_owned());
            let chars: Vec<char> = string.chars().collect();
            if n <= 0 {
                return Ok(Value::string(string));
            }
            let n = (n as usize).min(chars.len());
            let out: String = chars[n..].iter().collect();
            Ok(Value::string(out))
        }
        Value::Cons(_) => {
            if n <= 0 {
                return Ok(args[0]);
            }
            let mut cursor = args[0];
            let mut remaining = n as usize;
            while remaining > 0 {
                match cursor {
                    Value::Nil => return Ok(Value::Nil),
                    Value::Cons(cell) => {
                        let pair = read_cons(cell);
                        cursor = pair.cdr;
                        remaining -= 1;
                    }
                    _ => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("listp"), args[0]],
                        ));
                    }
                }
            }
            Ok(cursor)
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), *other],
        )),
    }
}

/// `(seq-take SEQ N)` — take first n elements.
pub(crate) fn builtin_seq_take(args: Vec<Value>) -> EvalResult {
    expect_args("seq-take", &args, 2)?;
    let n = expect_int(&args[1])?;

    match &args[0] {
        Value::Nil => Ok(Value::Nil),
        Value::Vector(v) => {
            let elems = with_heap(|h| h.get_vector(*v).clone());
            if n <= 0 {
                return Ok(Value::vector(Vec::new()));
            }
            let n = (n as usize).min(elems.len());
            Ok(Value::vector(elems[..n].to_vec()))
        }
        Value::Str(s) => {
            let string = with_heap(|h| h.get_string(*s).to_owned());
            let chars: Vec<char> = string.chars().collect();
            if n <= 0 {
                return Ok(Value::string(""));
            }
            let n = (n as usize).min(chars.len());
            let out: String = chars[..n].iter().collect();
            Ok(Value::string(out))
        }
        Value::Cons(_) => {
            if n <= 0 {
                return Ok(Value::Nil);
            }
            let mut out = Vec::new();
            let mut cursor = args[0];
            let mut remaining = n as usize;
            while remaining > 0 {
                match cursor {
                    Value::Nil => break,
                    Value::Cons(cell) => {
                        let pair = read_cons(cell);
                        out.push(pair.car);
                        cursor = pair.cdr;
                        remaining -= 1;
                    }
                    tail => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("listp"), tail],
                        ));
                    }
                }
            }
            Ok(Value::list(out))
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), *other],
        )),
    }
}

fn builtin_seq_subseq_legacy(args: &[Value]) -> EvalResult {
    let elems = collect_sequence(&args[0]);
    let start = expect_int(&args[1])? as usize;
    let end = if args.len() > 2 && !args[2].is_nil() {
        expect_int(&args[2])? as usize
    } else {
        elems.len()
    };
    let start = start.min(elems.len());
    let end = end.min(elems.len());
    if start > end {
        return Ok(Value::Nil);
    }
    let result: Vec<Value> = elems[start..end].to_vec();
    match &args[0] {
        Value::Vector(_) => Ok(Value::vector(result)),
        _ => Ok(Value::list(result)),
    }
}

/// `(seq-subseq SEQ START &optional END)` — subsequence.
pub(crate) fn builtin_seq_subseq(args: Vec<Value>) -> EvalResult {
    expect_min_args("seq-subseq", &args, 2)?;
    let start = expect_int(&args[1])?;
    let end = if args.len() > 2 && !args[2].is_nil() {
        Some(expect_int(&args[2])?)
    } else {
        None
    };

    // Preserve existing behavior for negative indices until full seq.el
    // index normalization support lands.
    if start < 0 || end.is_some_and(|v| v < 0) {
        return builtin_seq_subseq_legacy(&args);
    }

    match &args[0] {
        Value::Nil | Value::Cons(_) | Value::Vector(_) | Value::Str(_) => {
            let dropped = builtin_seq_drop(vec![args[0], Value::Int(start)])?;
            if let Some(end_idx) = end {
                let span = end_idx - start;
                builtin_seq_take(vec![dropped, Value::Int(span)])
            } else {
                Ok(dropped)
            }
        }
        other => Err(signal(
            "error",
            vec![Value::string(format!(
                "Unsupported sequence: {}",
                super::print::print_value(other)
            ))],
        )),
    }
}

/// `(seq-concatenate TYPE &rest SEQS)` — concatenate sequences into target type.
pub(crate) fn builtin_seq_concatenate(args: Vec<Value>) -> EvalResult {
    expect_min_args("seq-concatenate", &args, 1)?;
    let target = match &args[0] {
        Value::Symbol(id) => resolve_sym(*id),
        other => {
            return Err(signal(
                "error",
                vec![Value::string(format!(
                    "Not a sequence type name: {}",
                    super::print::print_value(other)
                ))],
            ));
        }
    };
    if target != "list" && target != "vector" && target != "string" {
        return Err(signal(
            "error",
            vec![Value::string(format!(
                "Not a sequence type name: {}",
                target
            ))],
        ));
    }

    let mut combined = Vec::new();
    for arg in &args[1..] {
        combined.extend(seq_collect_concat_arg(arg)?);
    }
    match target {
        "list" => Ok(Value::list(combined)),
        "vector" => Ok(Value::vector(combined)),
        "string" => {
            let mut s = String::new();
            for value in &combined {
                let ch = match value {
                    Value::Char(c) => *c,
                    Value::Int(n) => char::from_u32(*n as u32).ok_or_else(|| {
                        signal(
                            "wrong-type-argument",
                            vec![Value::symbol("characterp"), *value],
                        )
                    })?,
                    other => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("characterp"), *other],
                        ));
                    }
                };
                s.push(ch);
            }
            Ok(Value::string(s))
        }
        _ => unreachable!(),
    }
}

/// `(seq-empty-p SEQ)` — is sequence empty?
pub(crate) fn builtin_seq_empty_p(args: Vec<Value>) -> EvalResult {
    expect_args("seq-empty-p", &args, 1)?;
    match &args[0] {
        Value::Nil => Ok(Value::True),
        Value::Cons(_) => Ok(Value::Nil),
        Value::Lambda(_) | Value::ByteCode(_) => Ok(Value::Nil),
        Value::Str(s) => Ok(Value::bool(with_heap(|h| h.get_string(*s).is_empty()))),
        Value::Vector(v) => Ok(Value::bool(with_heap(|h| h.vector_len(*v)) == 0)),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), *other],
        )),
    }
}

/// `(seq-min SEQ)` — minimum element (numeric).
pub(crate) fn builtin_seq_min(args: Vec<Value>) -> EvalResult {
    expect_args("seq-min", &args, 1)?;
    let elems = seq_position_elements(&args[0])?;
    if elems.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::Subr(intern("min")), Value::Int(0)],
        ));
    }
    let mut min_val = &elems[0];
    let mut min_num = expect_number_or_marker(min_val)?;
    for e in &elems[1..] {
        let b = expect_number_or_marker(e)?;
        if b < min_num {
            min_num = b;
            min_val = e;
        }
    }
    Ok(*min_val)
}

/// `(seq-max SEQ)` — maximum element (numeric).
pub(crate) fn builtin_seq_max(args: Vec<Value>) -> EvalResult {
    expect_args("seq-max", &args, 1)?;
    let elems = seq_position_elements(&args[0])?;
    if elems.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::Subr(intern("max")), Value::Int(0)],
        ));
    }
    let mut max_val = &elems[0];
    let mut max_num = expect_number_or_marker(max_val)?;
    for e in &elems[1..] {
        let b = expect_number_or_marker(e)?;
        if b > max_num {
            max_num = b;
            max_val = e;
        }
    }
    Ok(*max_val)
}

// ===========================================================================
// Seq.el eval-dependent operations
// ===========================================================================

/// `(seq-position SEQ ELT &optional TESTFN)` — return first matching index.
pub(crate) fn builtin_seq_position(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("seq-position", &args, 2)?;
    let seq = &args[0];
    if matches!(seq, Value::Lambda(_) | Value::ByteCode(_)) {
        return Ok(Value::Nil);
    }
    let target = args[1];
    let test_fn = if args.len() > 2 && !args[2].is_nil() {
        Some(args[2])
    } else {
        None
    };
    let elements = seq_position_elements(seq)?;

    let saved = eval.save_temp_roots();
    eval.push_temp_root(target);
    if let Some(tf) = &test_fn {
        eval.push_temp_root(*tf);
    }
    for e in &elements {
        eval.push_temp_root(*e);
    }

    for (idx, element) in elements.into_iter().enumerate() {
        let matches = if let Some(test) = &test_fn {
            match eval.apply(*test, vec![element, target]) {
                Ok(v) => v.is_truthy(),
                Err(e) => {
                    eval.restore_temp_roots(saved);
                    return Err(e);
                }
            }
        } else {
            seq_default_match(&element, &target)
        };
        if matches {
            eval.restore_temp_roots(saved);
            return Ok(Value::Int(idx as i64));
        }
    }
    eval.restore_temp_roots(saved);
    Ok(Value::Nil)
}

/// `(cl-position ITEM SEQ &optional TESTFN)` -- CL argument order wrapper.
#[cfg(test)]
pub(crate) fn builtin_cl_position(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("cl-position", &args, 2)?;
    expect_max_args("cl-position", &args, 3)?;

    let mut forwarded = Vec::with_capacity(args.len());
    forwarded.push(args[1]);
    forwarded.push(args[0]);
    if args.len() == 3 {
        forwarded.push(args[2]);
    }

    builtin_seq_position(eval, forwarded)
}

/// `(cl-notany PREDICATE SEQ)` -- true when no element satisfies PREDICATE.
#[cfg(test)]
pub(crate) fn builtin_cl_notany(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let found = builtin_seq_some(eval, args)?;
    Ok(Value::bool(found.is_nil()))
}

/// `(cl-notevery PREDICATE SEQ)` -- true when not all elements satisfy PREDICATE.
#[cfg(test)]
pub(crate) fn builtin_cl_notevery(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let every = builtin_seq_every_p(eval, args)?;
    Ok(Value::bool(!every.is_truthy()))
}

/// `(cl-gensym &optional PREFIX)` -- generate an uninterned-style symbol name.
#[cfg(test)]
pub(crate) fn builtin_cl_gensym(args: Vec<Value>) -> EvalResult {
    expect_max_args("cl-gensym", &args, 1)?;
    let prefix = match args.first() {
        None => "G".to_string(),
        Some(Value::Nil) => "G".to_string(),
        Some(Value::Str(s)) => with_heap(|h| h.get_string(*s).to_owned()),
        Some(other) => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };
    let n = CL_GENSYM_COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(Value::symbol(format!("{prefix}{n}")))
}

/// `(cl-find ITEM SEQ)` -- return first element in SEQ equal to ITEM.
#[cfg(test)]
pub(crate) fn builtin_cl_find(args: Vec<Value>) -> EvalResult {
    expect_args("cl-find", &args, 2)?;
    let target = &args[0];
    let elements = seq_position_elements(&args[1])?;
    for element in elements {
        if equal_value(&element, target, 0) {
            return Ok(element);
        }
    }
    Ok(Value::Nil)
}

/// `(cl-find-if PREDICATE SEQ)` -- return first element satisfying PREDICATE.
#[cfg(test)]
pub(crate) fn builtin_cl_find_if(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("cl-find-if", &args, 2)?;
    let pred = args[0];
    let elements = seq_position_elements(&args[1])?;
    let saved = eval.save_temp_roots();
    eval.push_temp_root(pred);
    for e in &elements {
        eval.push_temp_root(*e);
    }
    for element in elements {
        match eval.apply(pred, vec![element]) {
            Ok(matched) => {
                if matched.is_truthy() {
                    eval.restore_temp_roots(saved);
                    return Ok(element);
                }
            }
            Err(err) => {
                eval.restore_temp_roots(saved);
                return Err(err);
            }
        }
    }
    eval.restore_temp_roots(saved);
    Ok(Value::Nil)
}

/// `(cl-subsetp LIST1 LIST2)` -- return t if every element of LIST1 appears in LIST2.
#[cfg(test)]
pub(crate) fn builtin_cl_subsetp(args: Vec<Value>) -> EvalResult {
    expect_args("cl-subsetp", &args, 2)?;
    let left = seq_position_elements(&args[0])?;
    let right = seq_position_elements(&args[1])?;

    for item in left {
        if !right
            .iter()
            .any(|candidate| equal_value(&item, candidate, 0))
        {
            return Ok(Value::Nil);
        }
    }
    Ok(Value::True)
}

/// `(cl-intersection LIST1 LIST2)` -- set-style intersection preserving LIST1 order.
#[cfg(test)]
pub(crate) fn builtin_cl_intersection(args: Vec<Value>) -> EvalResult {
    expect_args("cl-intersection", &args, 2)?;
    let left = seq_position_elements(&args[0])?;
    let right = seq_position_elements(&args[1])?;

    let mut out = Vec::new();
    for item in left {
        let in_right = right
            .iter()
            .any(|candidate| equal_value(&item, candidate, 0));
        let already_in_out = out.iter().any(|seen| equal_value(&item, seen, 0));
        if in_right && !already_in_out {
            out.push(item);
        }
    }
    Ok(Value::list(out))
}

/// `(cl-set-difference LIST1 LIST2)` -- set-style difference preserving LIST1 order.
#[cfg(test)]
pub(crate) fn builtin_cl_set_difference(args: Vec<Value>) -> EvalResult {
    expect_args("cl-set-difference", &args, 2)?;
    let left = seq_position_elements(&args[0])?;
    let right = seq_position_elements(&args[1])?;

    let mut out = Vec::new();
    for item in left {
        let in_right = right
            .iter()
            .any(|candidate| equal_value(&item, candidate, 0));
        let already_in_out = out.iter().any(|seen| equal_value(&item, seen, 0));
        if !in_right && !already_in_out {
            out.push(item);
        }
    }
    Ok(Value::list(out))
}

/// `(cl-union LIST1 LIST2)` -- set-style union preserving left-to-right discovery order.
#[cfg(test)]
pub(crate) fn builtin_cl_union(args: Vec<Value>) -> EvalResult {
    expect_args("cl-union", &args, 2)?;
    let left = seq_position_elements(&args[0])?;
    let right = seq_position_elements(&args[1])?;

    let mut out = Vec::new();
    for item in left.into_iter().chain(right.into_iter()) {
        let already_in_out = out.iter().any(|seen| equal_value(&item, seen, 0));
        if !already_in_out {
            out.push(item);
        }
    }
    Ok(Value::list(out))
}

/// `(cl-substitute NEW OLD SEQ)` -- replace OLD with NEW across SEQ.
#[cfg(test)]
pub(crate) fn builtin_cl_substitute(args: Vec<Value>) -> EvalResult {
    expect_args("cl-substitute", &args, 3)?;
    let new_value = args[0];
    let old_value = &args[1];
    let elements = seq_position_elements(&args[2])?;

    let replaced = elements
        .into_iter()
        .map(|item| {
            if equal_value(&item, old_value, 0) {
                new_value
            } else {
                item
            }
        })
        .collect::<Vec<_>>();
    Ok(Value::list(replaced))
}

/// `(cl-sort SEQ PREDICATE)` -- CL alias for `sort`.
#[cfg(test)]
pub(crate) fn builtin_cl_sort(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    super::builtins::builtin_sort(eval, args)
}

/// `(cl-stable-sort SEQ PREDICATE)` -- CL alias for stable `sort`.
#[cfg(test)]
pub(crate) fn builtin_cl_stable_sort(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    super::builtins::builtin_sort(eval, args)
}

/// `(cl-remove-if PREDICATE SEQ)` -- remove elements satisfying PREDICATE.
#[cfg(test)]
pub(crate) fn builtin_cl_remove_if(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("cl-remove-if", &args, 2)?;
    let pred = args[0];
    let elements = seq_position_elements(&args[1])?;
    let saved = eval.save_temp_roots();
    eval.push_temp_root(pred);
    for e in &elements {
        eval.push_temp_root(*e);
    }
    let mut out = Vec::new();

    for element in elements {
        match eval.apply(pred, vec![element]) {
            Ok(matched) => {
                if !matched.is_truthy() {
                    out.push(element);
                }
            }
            Err(err) => {
                eval.restore_temp_roots(saved);
                return Err(err);
            }
        }
    }
    eval.restore_temp_roots(saved);
    Ok(Value::list(out))
}

/// `(cl-remove-if-not PREDICATE SEQ)` -- keep elements satisfying PREDICATE.
#[cfg(test)]
pub(crate) fn builtin_cl_remove_if_not(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("cl-remove-if-not", &args, 2)?;
    let pred = args[0];
    let elements = seq_position_elements(&args[1])?;
    let saved = eval.save_temp_roots();
    eval.push_temp_root(pred);
    for e in &elements {
        eval.push_temp_root(*e);
    }
    let mut out = Vec::new();

    for element in elements {
        match eval.apply(pred, vec![element]) {
            Ok(matched) => {
                if matched.is_truthy() {
                    out.push(element);
                }
            }
            Err(err) => {
                eval.restore_temp_roots(saved);
                return Err(err);
            }
        }
    }
    eval.restore_temp_roots(saved);
    Ok(Value::list(out))
}

/// `(cl-map RESULT-TYPE FUNCTION SEQ...)` -- CL map with explicit result type.
#[cfg(test)]
pub(crate) fn builtin_cl_map(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("cl-map", &args, 3)?;
    let result_type = args[0];
    let func = args[1];
    let seqs = args[2..].to_vec();

    let mut forwarded = Vec::with_capacity(1 + seqs.len());
    forwarded.push(func);
    forwarded.extend(seqs);
    let mapped = builtin_seq_mapn(eval, forwarded)?;

    match result_type {
        Value::Symbol(id) if resolve_sym(id) == "list" => Ok(mapped),
        Value::Symbol(id) if resolve_sym(id) == "vector" => {
            let items = list_to_vec(&mapped).ok_or_else(|| {
                signal("wrong-type-argument", vec![Value::symbol("listp"), mapped])
            })?;
            Ok(Value::vector(items))
        }
        Value::Symbol(id) if resolve_sym(id) == "string" => {
            let items = list_to_vec(&mapped).ok_or_else(|| {
                signal("wrong-type-argument", vec![Value::symbol("listp"), mapped])
            })?;
            let mut out = String::new();
            for item in items {
                let ch =
                    match item {
                        Value::Char(c) => c,
                        Value::Int(n) => u32::try_from(n)
                            .ok()
                            .and_then(char::from_u32)
                            .ok_or_else(|| {
                                signal(
                                    "wrong-type-argument",
                                    vec![Value::symbol("characterp"), Value::Int(n)],
                                )
                            })?,
                        other => {
                            return Err(signal(
                                "wrong-type-argument",
                                vec![Value::symbol("characterp"), other],
                            ));
                        }
                    };
                out.push(ch);
            }
            Ok(Value::string(out))
        }
        other => Err(signal(
            "error",
            vec![Value::string(format!(
                "Unsupported cl-map result type: {}",
                super::print::print_value(&other)
            ))],
        )),
    }
}

/// `(seq-contains-p SEQ ELT &optional TESTFN)` — membership test for sequence.
pub(crate) fn builtin_seq_contains_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    if !(2..=3).contains(&args.len()) {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("seq-contains-p"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let seq = &args[0];
    let target = args[1];
    let test_fn = if args.len() == 3 && !args[2].is_nil() {
        Some(args[2])
    } else {
        None
    };
    let elements = seq_position_elements(seq)?;

    let saved = eval.save_temp_roots();
    eval.push_temp_root(target);
    if let Some(tf) = &test_fn {
        eval.push_temp_root(*tf);
    }
    for e in &elements {
        eval.push_temp_root(*e);
    }

    for element in elements {
        let matches = if let Some(test) = &test_fn {
            match eval.apply(*test, vec![element, target]) {
                Ok(v) => v.is_truthy(),
                Err(e) => {
                    eval.restore_temp_roots(saved);
                    return Err(e);
                }
            }
        } else {
            seq_default_match(&element, &target)
        };
        if matches {
            eval.restore_temp_roots(saved);
            return Ok(Value::True);
        }
    }
    eval.restore_temp_roots(saved);
    Ok(Value::Nil)
}

/// `(seq-mapn FN &rest SEQS)` — map over multiple sequences.
pub(crate) fn builtin_seq_mapn(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("seq-mapn", &args, 2)?;
    let func = args[0];
    let seqs: Vec<Vec<Value>> = args[1..].iter().map(collect_sequence).collect();
    if seqs.is_empty() {
        return Ok(Value::Nil);
    }
    let min_len = seqs.iter().map(|s| s.len()).min().unwrap_or(0);
    let saved = eval.save_temp_roots();
    eval.push_temp_root(func);
    for seq in &seqs {
        for e in seq {
            eval.push_temp_root(*e);
        }
    }
    let mut results = Vec::new();
    for i in 0..min_len {
        let call_args: Vec<Value> = seqs.iter().map(|s| s[i]).collect();
        match eval.apply(func, call_args) {
            Ok(val) => {
                eval.push_temp_root(val);
                results.push(val);
            }
            Err(e) => {
                eval.restore_temp_roots(saved);
                return Err(e);
            }
        }
    }
    eval.restore_temp_roots(saved);
    Ok(Value::list(results))
}

/// `(seq-do FN SEQ)` — apply fn for side effects, return nil.
pub(crate) fn builtin_seq_do(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("seq-do", &args, 2)?;
    let func = args[0];
    let elems = collect_sequence(&args[1]);
    let saved = eval.save_temp_roots();
    eval.push_temp_root(func);
    for e in &elems {
        eval.push_temp_root(*e);
    }
    for e in elems {
        if let Err(err) = eval.apply(func, vec![e]) {
            eval.restore_temp_roots(saved);
            return Err(err);
        }
    }
    eval.restore_temp_roots(saved);
    Ok(Value::Nil)
}

/// `(seq-count PRED SEQ)` — count elements matching predicate.
pub(crate) fn builtin_seq_count(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("seq-count", &args, 2)?;
    let pred = args[0];
    let elems = collect_sequence(&args[1]);
    let saved = eval.save_temp_roots();
    eval.push_temp_root(pred);
    for e in &elems {
        eval.push_temp_root(*e);
    }
    let mut count = 0i64;
    for e in elems {
        match eval.apply(pred, vec![e]) {
            Ok(r) => {
                if r.is_truthy() {
                    count += 1;
                }
            }
            Err(err) => {
                eval.restore_temp_roots(saved);
                return Err(err);
            }
        }
    }
    eval.restore_temp_roots(saved);
    Ok(Value::Int(count))
}

/// `(seq-reduce FN SEQ INITIAL)` — reduce with initial value.
pub(crate) fn builtin_seq_reduce(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("seq-reduce", &args, 3)?;
    let func = args[0];
    let elems = collect_sequence(&args[1]);
    let mut acc = args[2];
    let saved = eval.save_temp_roots();
    eval.push_temp_root(func);
    eval.push_temp_root(acc);
    for e in &elems {
        eval.push_temp_root(*e);
    }
    for e in elems {
        match eval.apply(func, vec![acc, e]) {
            Ok(val) => {
                acc = val;
                eval.push_temp_root(acc);
            }
            Err(err) => {
                eval.restore_temp_roots(saved);
                return Err(err);
            }
        }
    }
    eval.restore_temp_roots(saved);
    Ok(acc)
}

/// `(seq-some PRED SEQ)` — some element matches predicate.
pub(crate) fn builtin_seq_some(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("seq-some", &args, 2)?;
    let pred = args[0];
    let elems = collect_sequence(&args[1]);
    let saved = eval.save_temp_roots();
    eval.push_temp_root(pred);
    for e in &elems {
        eval.push_temp_root(*e);
    }
    for e in elems {
        match eval.apply(pred, vec![e]) {
            Ok(r) => {
                if r.is_truthy() {
                    eval.restore_temp_roots(saved);
                    return Ok(r);
                }
            }
            Err(err) => {
                eval.restore_temp_roots(saved);
                return Err(err);
            }
        }
    }
    eval.restore_temp_roots(saved);
    Ok(Value::Nil)
}

/// `(seq-every-p PRED SEQ)` — all elements match predicate.
pub(crate) fn builtin_seq_every_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("seq-every-p", &args, 2)?;
    let pred = args[0];
    let elems = collect_sequence(&args[1]);
    let saved = eval.save_temp_roots();
    eval.push_temp_root(pred);
    for e in &elems {
        eval.push_temp_root(*e);
    }
    for e in elems {
        match eval.apply(pred, vec![e]) {
            Ok(r) => {
                if r.is_nil() {
                    eval.restore_temp_roots(saved);
                    return Ok(Value::Nil);
                }
            }
            Err(err) => {
                eval.restore_temp_roots(saved);
                return Err(err);
            }
        }
    }
    eval.restore_temp_roots(saved);
    Ok(Value::True)
}

/// `(seq-sort PRED SEQ)` — sort with predicate.
pub(crate) fn builtin_seq_sort(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("seq-sort", &args, 2)?;
    let pred = args[0];
    let mut items = collect_sequence(&args[1]);

    let saved = eval.save_temp_roots();
    eval.push_temp_root(pred);
    for e in &items {
        eval.push_temp_root(*e);
    }

    // Insertion sort (stable, supports fallible predicates)
    for i in 1..items.len() {
        let mut j = i;
        while j > 0 {
            match eval.apply(pred, vec![items[j], items[j - 1]]) {
                Ok(result) => {
                    if result.is_truthy() {
                        items.swap(j, j - 1);
                        j -= 1;
                    } else {
                        break;
                    }
                }
                Err(err) => {
                    eval.restore_temp_roots(saved);
                    return Err(err);
                }
            }
        }
    }
    eval.restore_temp_roots(saved);
    Ok(Value::list(items))
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
#[path = "cl_lib_test.rs"]
mod tests;
