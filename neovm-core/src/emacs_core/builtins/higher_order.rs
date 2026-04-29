use super::*;
use crate::emacs_core::eval::LispArgVec;
use crate::emacs_core::value::list_length;
use smallvec::SmallVec;

type MapResultVec = SmallVec<[Value; 8]>;

#[inline]
fn apply0(eval: &mut super::eval::Context, func: Value) -> EvalResult {
    eval.apply(func, crate::emacs_core::eval::LispArgVec::new())
}

#[inline]
fn apply1(eval: &mut super::eval::Context, func: Value, arg: Value) -> EvalResult {
    let mut args = crate::emacs_core::eval::LispArgVec::new();
    args.push(arg);
    eval.apply(func, args)
}
pub(crate) fn builtin_apply(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_apply_slice(eval, &args)
}

pub(crate) fn builtin_apply_slice(eval: &mut super::eval::Context, args: &[Value]) -> EvalResult {
    // GNU eval.c Fapply: with one argument, the argument itself is the spread
    // list.  Its first element is the function and the remaining elements are
    // the arguments.
    if args.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("apply"), Value::fixnum(args.len() as i64)],
        ));
    }

    let last = args[args.len() - 1];
    let mut call_args = LispArgVec::new();

    if args.len() == 1 {
        let mut cursor = last;
        let func = match cursor.kind() {
            ValueKind::Nil => args[0],
            ValueKind::Cons => {
                let func = cursor.cons_car();
                cursor = cursor.cons_cdr();
                func
            }
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), last],
                ));
            }
        };
        while cursor.is_cons() {
            call_args.push(cursor.cons_car());
            cursor = cursor.cons_cdr();
        }
        if !cursor.is_nil() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("listp"), cursor],
            ));
        }
        eval.apply(func, call_args)
    } else {
        call_args.extend_from_slice(&args[1..args.len() - 1]);
        let mut cursor = last;
        loop {
            match cursor.kind() {
                ValueKind::Nil => break,
                ValueKind::Cons => {
                    call_args.push(cursor.cons_car());
                    cursor = cursor.cons_cdr();
                }
                _ => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("listp"), cursor],
                    ));
                }
            }
        }
        eval.apply(args[0], call_args)
    }
}

pub(crate) fn builtin_funcall(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_funcall_slice(eval, &args)
}

pub(crate) fn builtin_funcall_slice(eval: &mut super::eval::Context, args: &[Value]) -> EvalResult {
    expect_min_args("funcall", args, 1)?;
    let func = args[0];
    let mut call_args = LispArgVec::new();
    call_args.extend_from_slice(&args[1..]);
    eval.apply(func, call_args)
}

pub(crate) fn builtin_funcall_interactively(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_funcall_interactively_slice(eval, &args)
}

pub(crate) fn builtin_funcall_interactively_slice(
    eval: &mut super::eval::Context,
    args: &[Value],
) -> EvalResult {
    expect_min_args("funcall-interactively", args, 1)?;
    let func = args[0];
    let mut call_args = LispArgVec::new();
    call_args.extend_from_slice(&args[1..]);
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
    apply0(eval, args[2])
}

// ===========================================================================
// Higher-order
// ===========================================================================

pub(crate) fn for_each_sequence_element<F>(seq: &Value, mut f: F) -> Result<(), Flow>
where
    F: FnMut(Value) -> Result<(), Flow>,
{
    match seq.kind() {
        ValueKind::Nil => Ok(()),
        ValueKind::Cons => {
            let mut cursor = *seq;
            loop {
                match cursor.kind() {
                    ValueKind::Nil => break,
                    ValueKind::Cons => {
                        let pair_car = cursor.cons_car();
                        let pair_cdr = cursor.cons_cdr();
                        let item = pair_car;
                        cursor = pair_cdr;
                        f(item)?;
                    }
                    tail => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("listp"), cursor],
                        ));
                    }
                }
            }
            Ok(())
        }
        ValueKind::Veclike(VecLikeType::Vector) | ValueKind::Veclike(VecLikeType::Record) => {
            for item in seq.as_vector_data().unwrap().clone().into_iter() {
                f(item)?;
            }
            Ok(())
        }
        ValueKind::Veclike(VecLikeType::Lambda) => {
            for item in super::cons_list::lambda_to_closure_vector(seq).into_iter() {
                f(item)?;
            }
            Ok(())
        }
        ValueKind::Veclike(VecLikeType::ByteCode) => {
            for item in super::cons_list::bytecode_to_closure_vector(seq).into_iter() {
                f(item)?;
            }
            Ok(())
        }
        ValueKind::String => {
            let string = seq.as_lisp_string().expect("string");
            for cp in super::lisp_string_char_codes(string) {
                f(Value::fixnum(cp as i64))?;
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
            vec![Value::symbol("mapcar"), Value::fixnum(args.len() as i64)],
        ));
    }
    builtin_mapcar_2(eval, args[0], args[1])
}

pub(crate) fn builtin_mapcar_2(
    eval: &mut super::eval::Context,
    func: Value,
    seq: Value,
) -> EvalResult {
    let roots = eval.save_vm_roots();
    eval.push_vm_frame_root(func);
    eval.push_vm_frame_root(seq);
    let mut results = MapResultVec::new();
    // GNU fns.c Fmapcar computes SEQUENCE length before calling FUNCTION, then
    // reads each cons cdr after the callback.  If FUNCTION shortens the list,
    // mapcar returns the mapped prefix instead of following stale cdrs.
    let map_result: Result<(), Flow> = if seq.is_nil() {
        Ok(())
    } else if seq.is_cons() {
        let len = match list_length(&seq) {
            Some(len) => len,
            None => {
                let mut cursor = seq;
                while cursor.is_cons() {
                    cursor = cursor.cons_cdr();
                }
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), cursor],
                ));
            }
        };
        results = MapResultVec::with_capacity(len);
        let mut cursor = seq;
        let mut result = Ok(());
        for _ in 0..len {
            if !cursor.is_cons() {
                break;
            }
            eval.push_vm_frame_root(cursor);
            let item = cursor.cons_car();
            let val = match apply1(eval, func, item) {
                Ok(v) => v,
                Err(e) => {
                    result = Err(e);
                    break;
                }
            };
            eval.push_vm_frame_root(val);
            results.push(val);
            cursor = cursor.cons_cdr();
        }
        result
    } else {
        for_each_sequence_element(&seq, |item| {
            let val = apply1(eval, func, item)?;
            eval.push_vm_frame_root(val);
            results.push(val);
            Ok(())
        })
    };
    eval.restore_vm_roots(roots);
    map_result?;
    Ok(Value::list_from_slice(&results))
}

pub(crate) fn builtin_mapc(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    if args.len() != 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("mapc"), Value::fixnum(args.len() as i64)],
        ));
    }
    builtin_mapc_2(eval, args[0], args[1])
}

pub(crate) fn builtin_mapc_2(
    eval: &mut super::eval::Context,
    func: Value,
    seq: Value,
) -> EvalResult {
    let roots = eval.save_vm_roots();
    eval.push_vm_frame_root(func);
    eval.push_vm_frame_root(seq);
    // For cons lists, root cursor at each step so our precise GC
    // (which doesn't scan the Rust stack) can find the remaining
    // chain even if a hook callback modifies the list.
    let result: Result<(), Flow> = if seq.is_cons() || seq.is_nil() {
        let mut cursor = seq;
        loop {
            match cursor.kind() {
                ValueKind::Nil => break Ok(()),
                ValueKind::Cons => {
                    let pair_car = cursor.cons_car();
                    let pair_cdr = cursor.cons_cdr();
                    let item = pair_car;
                    cursor = pair_cdr;
                    // Root the remaining tail before calling the function.
                    eval.push_vm_frame_root(cursor);
                    if let Err(e) = apply1(eval, func, item) {
                        break Err(e);
                    }
                }
                tail => {
                    break Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("listp"), cursor],
                    ));
                }
            }
        }
    } else {
        for_each_sequence_element(&seq, |item| {
            apply1(eval, func, item)?;
            Ok(())
        })
    };
    eval.restore_vm_roots(roots);
    result?;
    Ok(seq)
}

pub(crate) fn builtin_mapconcat(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_range_args("mapconcat", &args, 2, 3)?;
    let func = args[0];
    let sequence = args[1];
    // Emacs 30: separator is optional, defaults to ""
    let separator = args.get(2).copied().unwrap_or_else(|| Value::string(""));

    let mut parts = Vec::new();
    let roots = eval.save_vm_roots();
    eval.push_vm_frame_root(func);
    eval.push_vm_frame_root(sequence);
    eval.push_vm_frame_root(separator);
    let mapconcat_result = for_each_sequence_element(&sequence, |item| {
        let val = apply1(eval, func, item)?;
        eval.push_vm_frame_root(val);
        parts.push(val);
        Ok(())
    });
    eval.restore_vm_roots(roots);
    mapconcat_result?;

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
            vec![Value::symbol("mapcan"), Value::fixnum(args.len() as i64)],
        ));
    }
    let func = args[0];
    let sequence = args[1];
    let mut mapped = Vec::new();
    let roots = eval.save_vm_roots();
    eval.push_vm_frame_root(func);
    eval.push_vm_frame_root(sequence);
    let mapcan_result = for_each_sequence_element(&sequence, |item| {
        let val = apply1(eval, func, item)?;
        eval.push_vm_frame_root(val);
        mapped.push(val);
        Ok(())
    });
    eval.restore_vm_roots(roots);
    mapcan_result?;
    builtin_nconc(mapped)
}

pub(crate) struct SortOptions {
    pub(crate) key_fn: Value,
    pub(crate) lessp_fn: Value,
    pub(crate) reverse: bool,
    pub(crate) in_place: bool,
}

pub(crate) trait SortRuntime {
    fn call_sort_function1(&mut self, function: Value, arg: Value) -> Result<Value, Flow>;
    fn call_sort_function2(
        &mut self,
        function: Value,
        arg0: Value,
        arg1: Value,
    ) -> Result<Value, Flow>;
    fn root_sort_value(&mut self, value: Value);
    fn compare_sort_keys(
        &mut self,
        left: &Value,
        right: &Value,
    ) -> Result<std::cmp::Ordering, Flow>;
}

impl SortRuntime for super::eval::Context {
    fn call_sort_function1(&mut self, function: Value, arg: Value) -> Result<Value, Flow> {
        let mut args = LispArgVec::new();
        args.push(arg);
        self.apply(function, args)
    }

    fn call_sort_function2(
        &mut self,
        function: Value,
        arg0: Value,
        arg1: Value,
    ) -> Result<Value, Flow> {
        let mut args = LispArgVec::new();
        args.push(arg0);
        args.push(arg1);
        self.apply(function, args)
    }

    fn root_sort_value(&mut self, value: Value) {
        self.push_specpdl_root(value);
    }

    fn compare_sort_keys(
        &mut self,
        left: &Value,
        right: &Value,
    ) -> Result<std::cmp::Ordering, Flow> {
        super::symbols::compare_value_lt(self, left, right)
    }
}

pub(crate) fn parse_sort_options(args: &[Value]) -> Result<SortOptions, Flow> {
    if args.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("sort"), Value::fixnum(0)],
        ));
    }

    // Emacs 30 sort: (sort SEQ &key :key :lessp :reverse :in-place)
    // Old form: (sort SEQ PRED) — still supported, always in-place.
    let mut key_fn = Value::NIL;
    let mut lessp_fn = Value::NIL;
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
        key_fn = Value::NIL;
    }
    if matches!(lessp_fn.as_symbol_name(), Some("value<")) {
        lessp_fn = Value::NIL;
    }

    Ok(SortOptions {
        key_fn,
        lessp_fn,
        reverse,
        in_place,
    })
}

pub(crate) fn builtin_sort(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_sort_slice(eval, &args)
}

pub(crate) fn builtin_sort_slice(eval: &mut super::eval::Context, args: &[Value]) -> EvalResult {
    let SortOptions {
        key_fn,
        lessp_fn,
        reverse,
        in_place,
    } = parse_sort_options(&args)?;

    match args[0].kind() {
        ValueKind::Nil => Ok(Value::NIL),
        ValueKind::Cons => {
            let mut cons_cells = Vec::new();
            let mut values = Vec::new();
            let mut cursor = args[0];
            loop {
                match cursor.kind() {
                    ValueKind::Nil => break,
                    ValueKind::Cons => {
                        values.push(cursor.cons_car());
                        cons_cells.push(cursor);
                        cursor = cursor.cons_cdr();
                    }
                    tail => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("listp"), cursor],
                        ));
                    }
                }
            }

            let roots = eval.save_specpdl_roots();
            eval.push_specpdl_root(args[0]);
            eval.push_specpdl_root(lessp_fn);
            eval.push_specpdl_root(key_fn);
            for value in &values {
                eval.push_specpdl_root(*value);
            }
            let sorted_result = stable_sort_values_with(eval, &values, key_fn, lessp_fn, reverse);
            eval.restore_specpdl_roots(roots);
            let mut sorted_values = sorted_result?;
            if in_place {
                for (cell, value) in cons_cells.iter().zip(sorted_values.into_iter()) {
                    cell.set_car(value);
                }
                Ok(args[0])
            } else {
                Ok(Value::list(std::mem::take(&mut sorted_values)))
            }
        }
        ValueKind::Veclike(VecLikeType::Vector) | ValueKind::Veclike(VecLikeType::Record) => {
            let values = match args[0].kind() {
                ValueKind::Veclike(VecLikeType::Vector) => {
                    args[0].as_vector_data().unwrap().clone()
                }
                ValueKind::Veclike(VecLikeType::Record) => {
                    args[0].as_record_data().unwrap().clone()
                }
                _ => unreachable!(),
            };
            let roots = eval.save_specpdl_roots();
            eval.push_specpdl_root(args[0]);
            eval.push_specpdl_root(lessp_fn);
            eval.push_specpdl_root(key_fn);
            for value in &values {
                eval.push_specpdl_root(*value);
            }
            let sorted_result = stable_sort_values_with(eval, &values, key_fn, lessp_fn, reverse);
            eval.restore_specpdl_roots(roots);
            let sorted_values = sorted_result?;

            if in_place {
                assert!(args[0].replace_vectorlike_sequence_data(sorted_values));
                Ok(args[0])
            } else {
                match args[0].kind() {
                    ValueKind::Veclike(VecLikeType::Vector) => Ok(Value::vector(sorted_values)),
                    ValueKind::Veclike(VecLikeType::Record) => {
                        Ok(Value::make_record(sorted_values))
                    }
                    _ => unreachable!(),
                }
            }
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("list-or-vector-p"), args[0]],
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
    use crate::emacs_core::value::{ValueKind, VecLikeType};
    use std::cmp::Ordering;

    if values.len() < 2 {
        return Ok(values.to_vec());
    }

    let mut items: Vec<SortItem> = values
        .iter()
        .copied()
        .map(|value| SortItem {
            value,
            key: Value::NIL,
        })
        .collect();

    if !key_fn.is_nil() {
        for item in &mut items {
            let key = runtime.call_sort_function1(key_fn, item.value)?;
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
        return runtime.compare_sort_keys(&left.key, &right.key);
    }

    if runtime
        .call_sort_function2(lessp_fn, left.key, right.key)?
        .is_truthy()
    {
        return Ok(std::cmp::Ordering::Less);
    }
    if runtime
        .call_sort_function2(lessp_fn, right.key, left.key)?
        .is_truthy()
    {
        return Ok(std::cmp::Ordering::Greater);
    }
    Ok(std::cmp::Ordering::Equal)
}
