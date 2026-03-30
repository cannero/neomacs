use super::*;
pub(crate) fn builtin_apply(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    if args.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("apply"), Value::Int(args.len() as i64)],
        ));
    }
    if args.len() == 1 {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), args[0]],
        ));
    }
    let func = args[0];
    let last = &args[args.len() - 1];
    let mut call_args: Vec<Value> = args[1..args.len() - 1].to_vec();

    // Last argument must be a list, which gets spread
    match last {
        Value::Nil => {}
        Value::Cons(_) => {
            let mut cursor = *last;
            loop {
                match cursor {
                    Value::Nil => break,
                    Value::Cons(cell) => {
                        let pair = read_cons(cell);
                        call_args.push(pair.car);
                        cursor = pair.cdr;
                    }
                    other => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("listp"), other],
                        ));
                    }
                }
            }
        }
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("listp"), *last],
            ));
        }
    }

    eval.apply(func, call_args)
}

pub(crate) fn builtin_funcall(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("funcall", &args, 1)?;
    let func = args[0];
    let call_args = args[1..].to_vec();
    eval.apply(func, call_args)
}

pub(crate) fn builtin_funcall_interactively(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("funcall-interactively", &args, 1)?;
    let func = args[0];
    let call_args = args[1..].to_vec();
    eval.interactive.push_interactive_call(true);
    let result = eval.apply(func, call_args);
    eval.interactive.pop_interactive_call();
    result
}

pub(crate) fn builtin_funcall_with_delayed_message(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("funcall-with-delayed-message", &args, 3)?;
    let _delay = expect_number(&args[0])?;
    let _message = expect_string(&args[1])?;
    eval.apply(args[2], vec![])
}

// ===========================================================================
// Higher-order
// ===========================================================================

pub(crate) fn for_each_sequence_element<F>(seq: &Value, mut f: F) -> Result<(), Flow>
where
    F: FnMut(Value) -> Result<(), Flow>,
{
    match seq {
        Value::Nil => Ok(()),
        Value::Cons(_) => {
            let mut cursor = *seq;
            loop {
                match cursor {
                    Value::Nil => break,
                    Value::Cons(cell) => {
                        let pair = read_cons(cell);
                        let item = pair.car;
                        cursor = pair.cdr;
                        drop(pair);
                        f(item)?;
                    }
                    tail => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("listp"), tail],
                        ));
                    }
                }
            }
            Ok(())
        }
        Value::Vector(v) | Value::Record(v) => {
            for item in with_heap(|h| h.get_vector(*v).clone()).into_iter() {
                f(item)?;
            }
            Ok(())
        }
        Value::Lambda(_) => {
            for item in super::cons_list::lambda_to_closure_vector(seq).into_iter() {
                f(item)?;
            }
            Ok(())
        }
        Value::ByteCode(_) => {
            for item in super::cons_list::bytecode_to_closure_vector(seq).into_iter() {
                f(item)?;
            }
            Ok(())
        }
        Value::Str(id) => {
            let s = with_heap(|h| h.get_string(*id).to_owned());
            for cp in decode_storage_char_codes(&s) {
                f(Value::Int(cp as i64))?;
            }
            Ok(())
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), *seq],
        )),
    }
}

pub(crate) fn builtin_mapcar(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    if args.len() != 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("mapcar"), Value::Int(args.len() as i64)],
        ));
    }
    let func = args[0];
    let seq = args[1];
    let saved = eval.save_temp_roots();
    eval.push_temp_root(func);
    eval.push_temp_root(seq);
    let mut results = Vec::new();
    // Root cursor at each step for precise GC safety (see builtin_mapc).
    let map_result: Result<(), Flow> = if seq.is_cons() || seq.is_nil() {
        let mut cursor = seq;
        loop {
            match cursor {
                Value::Nil => break Ok(()),
                Value::Cons(cell) => {
                    let pair = read_cons(cell);
                    let item = pair.car;
                    cursor = pair.cdr;
                    drop(pair);
                    eval.push_temp_root(cursor);
                    let val = eval.apply(func, vec![item])?;
                    eval.push_temp_root(val);
                    results.push(val);
                }
                tail => {
                    break Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("listp"), tail],
                    ));
                }
            }
        }
    } else {
        for_each_sequence_element(&seq, |item| {
            let val = eval.apply(func, vec![item])?;
            eval.push_temp_root(val);
            results.push(val);
            Ok(())
        })
    };
    eval.restore_temp_roots(saved);
    map_result?;
    Ok(Value::list(results))
}

pub(crate) fn builtin_mapc(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    if args.len() != 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("mapc"), Value::Int(args.len() as i64)],
        ));
    }
    let func = args[0];
    let seq = args[1];
    let saved = eval.save_temp_roots();
    eval.push_temp_root(func);
    eval.push_temp_root(seq);
    // For cons lists, root cursor at each step so our precise GC
    // (which doesn't scan the Rust stack) can find the remaining
    // chain even if a hook callback modifies the list.
    let result: Result<(), Flow> = if seq.is_cons() || seq.is_nil() {
        let mut cursor = seq;
        loop {
            match cursor {
                Value::Nil => break Ok(()),
                Value::Cons(cell) => {
                    let pair = read_cons(cell);
                    let item = pair.car;
                    cursor = pair.cdr;
                    drop(pair);
                    // Root the remaining tail before calling the function.
                    eval.push_temp_root(cursor);
                    eval.apply(func, vec![item])?;
                }
                tail => {
                    break Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("listp"), tail],
                    ));
                }
            }
        }
    } else {
        for_each_sequence_element(&seq, |item| {
            eval.apply(func, vec![item])?;
            Ok(())
        })
    };
    eval.restore_temp_roots(saved);
    result?;
    Ok(seq)
}

pub(crate) fn builtin_mapconcat(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_range_args("mapconcat", &args, 2, 3)?;
    let func = args[0];
    let sequence = args[1];
    // Emacs 30: separator is optional, defaults to ""
    let separator = args.get(2).copied().unwrap_or_else(|| Value::string(""));

    let saved = eval.save_temp_roots();
    eval.push_temp_root(func);
    eval.push_temp_root(sequence);
    eval.push_temp_root(separator);
    let mut parts = Vec::new();
    let map_result = for_each_sequence_element(&sequence, |item| {
        let val = eval.apply(func, vec![item])?;
        eval.push_temp_root(val);
        parts.push(val);
        Ok(())
    });
    eval.restore_temp_roots(saved);
    map_result?;

    if parts.is_empty() {
        return Ok(Value::string(""));
    }

    let mut concat_args = Vec::with_capacity(parts.len() * 2 - 1);
    for (index, part) in parts.into_iter().enumerate() {
        if index > 0 {
            concat_args.push(separator);
        }
        concat_args.push(part);
    }
    builtin_concat(concat_args)
}

pub(crate) fn builtin_mapcan(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    if args.len() != 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("mapcan"), Value::Int(args.len() as i64)],
        ));
    }
    let func = args[0];
    let sequence = args[1];
    let saved = eval.save_temp_roots();
    eval.push_temp_root(func);
    eval.push_temp_root(sequence);
    let mut mapped = Vec::new();
    let map_result = for_each_sequence_element(&sequence, |item| {
        let val = eval.apply(func, vec![item])?;
        eval.push_temp_root(val);
        mapped.push(val);
        Ok(())
    });
    eval.restore_temp_roots(saved);
    map_result?;
    builtin_nconc(mapped)
}

pub(crate) struct SortOptions {
    pub(crate) key_fn: Value,
    pub(crate) lessp_fn: Value,
    pub(crate) reverse: bool,
    pub(crate) in_place: bool,
}

pub(crate) trait SortRuntime {
    fn call_sort_function(&mut self, function: Value, args: Vec<Value>) -> Result<Value, Flow>;
    fn root_sort_value(&mut self, value: Value);
}

impl SortRuntime for super::eval::Context {
    fn call_sort_function(&mut self, function: Value, args: Vec<Value>) -> Result<Value, Flow> {
        self.apply(function, args)
    }

    fn root_sort_value(&mut self, value: Value) {
        self.push_temp_root(value);
    }
}

pub(crate) fn parse_sort_options(args: &[Value]) -> Result<SortOptions, Flow> {
    if args.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("sort"), Value::Int(0)],
        ));
    }

    // Emacs 30 sort: (sort SEQ &key :key :lessp :reverse :in-place)
    // Old form: (sort SEQ PRED) — still supported, always in-place.
    let mut key_fn = Value::Nil;
    let mut lessp_fn = Value::Nil;
    let mut reverse = false;
    let mut in_place = false;

    if args.len() == 2 && !args[1].is_keyword() {
        lessp_fn = args[1];
        in_place = true;
    } else if args.len() > 2 && !args[1].is_keyword() {
        return Err(signal(
            "error",
            vec![Value::string("Invalid argument list")],
        ));
    } else if args.len() > 1 {
        let mut i = 1;
        while i < args.len() {
            if let Some(kw) = args[i].as_symbol_name() {
                match kw {
                    ":key" => {
                        i += 1;
                        if i < args.len() {
                            key_fn = args[i];
                        }
                    }
                    ":lessp" => {
                        i += 1;
                        if i < args.len() {
                            lessp_fn = args[i];
                        }
                    }
                    ":reverse" => {
                        i += 1;
                        if i < args.len() {
                            reverse = args[i].is_truthy();
                        }
                    }
                    ":in-place" => {
                        i += 1;
                        if i < args.len() {
                            in_place = args[i].is_truthy();
                        }
                    }
                    _ => {}
                }
            }
            i += 1;
        }
    }

    if matches!(key_fn.as_symbol_name(), Some("identity")) {
        key_fn = Value::Nil;
    }
    if matches!(lessp_fn.as_symbol_name(), Some("value<")) {
        lessp_fn = Value::Nil;
    }

    Ok(SortOptions {
        key_fn,
        lessp_fn,
        reverse,
        in_place,
    })
}

pub(crate) fn builtin_sort(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let SortOptions {
        key_fn,
        lessp_fn,
        reverse,
        in_place,
    } = parse_sort_options(&args)?;

    match &args[0] {
        Value::Nil => Ok(Value::Nil),
        Value::Cons(_) => {
            let mut cons_cells = Vec::new();
            let mut values = Vec::new();
            let mut cursor = args[0];
            loop {
                match cursor {
                    Value::Nil => break,
                    Value::Cons(cell) => {
                        values.push(with_heap(|h| h.cons_car(cell)));
                        cons_cells.push(cell);
                        cursor = with_heap(|h| h.cons_cdr(cell));
                    }
                    tail => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("listp"), tail],
                        ));
                    }
                }
            }

            let saved = eval.save_temp_roots();
            eval.push_temp_root(args[0]);
            eval.push_temp_root(lessp_fn);
            eval.push_temp_root(key_fn);
            for value in &values {
                eval.push_temp_root(*value);
            }
            let sorted_values = stable_sort_values_with(eval, &values, key_fn, lessp_fn, reverse);
            eval.restore_temp_roots(saved);
            let mut sorted_values = sorted_values?;
            if in_place {
                for (cell, value) in cons_cells.iter().zip(sorted_values.into_iter()) {
                    with_heap_mut(|h| h.set_car(*cell, value));
                }
                Ok(args[0])
            } else {
                Ok(Value::list(std::mem::take(&mut sorted_values)))
            }
        }
        Value::Vector(v) | Value::Record(v) => {
            let values = with_heap(|h| h.get_vector(*v).clone());
            let saved = eval.save_temp_roots();
            eval.push_temp_root(args[0]);
            eval.push_temp_root(lessp_fn);
            eval.push_temp_root(key_fn);
            for value in &values {
                eval.push_temp_root(*value);
            }
            let sorted_values = stable_sort_values_with(eval, &values, key_fn, lessp_fn, reverse);
            eval.restore_temp_roots(saved);
            let sorted_values = sorted_values?;

            if in_place {
                with_heap_mut(|h| *h.get_vector_mut(*v) = sorted_values);
                Ok(args[0])
            } else {
                match args[0] {
                    Value::Vector(_) => Ok(Value::vector(sorted_values)),
                    Value::Record(_) => {
                        let id = with_heap_mut(|h| h.alloc_vector(sorted_values));
                        Ok(Value::Record(id))
                    }
                    _ => unreachable!(),
                }
            }
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("list-or-vector-p"), *other],
        )),
    }
}

#[derive(Clone, Copy)]
struct SortItem {
    value: Value,
    key: Value,
}

pub(crate) fn stable_sort_values_with(
    runtime: &mut impl SortRuntime,
    values: &[Value],
    key_fn: Value,
    lessp_fn: Value,
    reverse: bool,
) -> Result<Vec<Value>, Flow> {
    use std::cmp::Ordering;

    if values.len() < 2 {
        return Ok(values.to_vec());
    }

    let mut items: Vec<SortItem> = values
        .iter()
        .copied()
        .map(|value| SortItem {
            value,
            key: Value::Nil,
        })
        .collect();

    if !key_fn.is_nil() {
        for item in &mut items {
            let key = runtime.call_sort_function(key_fn, vec![item.value])?;
            runtime.root_sort_value(key);
            item.key = key;
        }
    } else {
        for item in &mut items {
            item.key = item.value;
        }
    }

    if reverse {
        items.reverse();
    }

    let mut sort_error: Option<Flow> = None;
    items.sort_by(|left, right| {
        if sort_error.is_some() {
            return Ordering::Equal;
        }
        match compare_sort_items(runtime, left, right, lessp_fn) {
            Ok(ordering) => ordering,
            Err(err) => {
                sort_error = Some(err);
                Ordering::Equal
            }
        }
    });

    if reverse {
        items.reverse();
    }

    if let Some(err) = sort_error {
        return Err(err);
    }
    Ok(items.into_iter().map(|item| item.value).collect())
}

fn compare_sort_items(
    runtime: &mut impl SortRuntime,
    left: &SortItem,
    right: &SortItem,
    lessp_fn: Value,
) -> Result<std::cmp::Ordering, Flow> {
    if lessp_fn.is_nil() {
        return super::symbols::compare_value_lt(&left.key, &right.key)
            .map_err(|(lhs, rhs)| signal("type-mismatch", vec![lhs, rhs]));
    }

    if runtime
        .call_sort_function(lessp_fn, vec![left.key, right.key])?
        .is_truthy()
    {
        return Ok(std::cmp::Ordering::Less);
    }
    if runtime
        .call_sort_function(lessp_fn, vec![right.key, left.key])?
        .is_truthy()
    {
        return Ok(std::cmp::Ordering::Greater);
    }
    Ok(std::cmp::Ordering::Equal)
}
