use super::*;
use crate::emacs_core::symbol::Obarray;

pub(crate) fn builtin_get_pos_property(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_get_pos_property_impl(&eval.obarray, &[], &eval.buffers, args)
}

pub(crate) fn builtin_get_pos_property_impl(
    obarray: &crate::emacs_core::symbol::Obarray,
    _dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("get-pos-property", &args, 2)?;
    expect_max_args("get-pos-property", &args, 3)?;
    let pos = super::buffers::expect_integer_or_marker_in_buffers(buffers, &args[0])?;
    let prop = super::textprop::expect_symbol_name(&args[1])?;

    if let Some(str_id) = super::textprop::is_string_object(args.get(2)) {
        let s = with_heap(|h| h.get_string(str_id).to_owned());
        if let Some(table) = get_string_text_properties_table(str_id) {
            let byte_pos = super::textprop::string_elisp_pos_to_byte(&s, pos);
            return Ok(super::textprop::builtin_get_text_property_in_state(
                obarray,
                buffers,
                vec![
                    Value::fixnum(pos),
                    Value::symbol(prop.clone()),
                    ValueKind::String,
                ],
            )?);
        }
        return Ok(Value::NIL);
    }

    let buf_id = match args.get(2) {
        None | Some(ValueKind::Nil) => buffers
            .current_buffer()
            .map(|b| b.id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")])),
        Some(ValueKind::Veclike(VecLikeType::Buffer)) => Ok(*id),
        Some(other) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("buffer-or-string-p"), *other],
        )),
    }?;

    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;
    let byte_pos = buf.lisp_pos_to_byte(pos);

    if let Some((value, _overlay_id)) =
        super::textprop::buffer_overlay_property_for_inserted_char_at_byte_pos(buf, byte_pos, &prop)
    {
        return Ok(value);
    }

    match text_property_stickiness_in_state(obarray, buffers, buf, pos, &prop) {
        1 => Ok(text_property_value_at_char_pos(
            obarray, buffers, buf, pos, &prop,
        )),
        -1 if pos > buf.point_min_char() as i64 + 1 => Ok(text_property_value_at_char_pos(
            obarray,
            buffers,
            buf,
            pos - 1,
            &prop,
        )),
        _ => Ok(Value::NIL),
    }
}

pub(crate) fn builtin_next_char_property_change(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_next_char_property_change_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_next_char_property_change_in_buffers(
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("next-char-property-change", &args, 1)?;
    expect_max_args("next-char-property-change", &args, 2)?;
    let result = match args.len().kind() {
        1 => super::textprop::builtin_next_property_change_in_buffers(buffers, args)?,
        2 => super::textprop::builtin_next_property_change_in_buffers(
            buffers,
            vec![args[0], Value::NIL, args[1]],
        )?,
        _ => unreachable!(),
    };
    if !result.is_nil() {
        return Ok(result);
    }

    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    Ok(Value::fixnum(buf.point_max_char() as i64 + 1))
}

pub(crate) fn builtin_pos_bol(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_max_args("pos-bol", &args, 1)?;
    super::navigation::builtin_line_beginning_position(eval, args)
}

pub(crate) fn builtin_pos_eol(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_max_args("pos-eol", &args, 1)?;
    super::navigation::builtin_line_end_position(eval, args)
}

pub(crate) fn builtin_previous_property_change(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_previous_property_change_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_previous_property_change_in_buffers(
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("previous-property-change", &args, 1)?;
    expect_max_args("previous-property-change", &args, 3)?;

    let pos = super::buffers::expect_integer_or_marker_in_buffers(buffers, &args[0])?;

    // --- String OBJECT ---
    if let Some(ValueKind::String) = args.get(1) {
        let str_id = *str_id;
        let s = with_heap(|h| h.get_string(str_id).to_owned());
        let table = get_string_text_properties_table(str_id).unwrap_or_default();
        let byte_pos = textprop::string_elisp_pos_to_byte(&s, pos);
        let (byte_limit, limit_val) = match args.get(2) {
            Some(v) if !v.is_nil() => {
                let lim_int = expect_integer_or_marker(v)?;
                (
                    Some(textprop::string_elisp_pos_to_byte(&s, lim_int)),
                    Some(lim_int),
                )
            }
            _ => (None, None),
        };

        let ref_byte = if byte_pos > 0 { byte_pos - 1 } else { 0 };
        let current_props = table.get_properties(ref_byte);
        let mut cursor = byte_pos;

        loop {
            match table.previous_property_change(cursor) {
                Some(prev) => {
                    if let Some(lim) = byte_limit {
                        if prev <= lim {
                            return Ok(match limit_val {
                                Some(lv) => Value::fixnum(lv),
                                None => Value::NIL,
                            });
                        }
                    }
                    let check = if prev > 0 { prev - 1 } else { 0 };
                    let new_props = table.get_properties(check);
                    if new_props != current_props {
                        return Ok(Value::fixnum(textprop::string_byte_to_elisp_pos(&s, prev)));
                    }
                    if prev == 0 {
                        break;
                    }
                    cursor = if prev < cursor { prev } else { prev - 1 };
                }
                None => break,
            }
        }

        return Ok(match limit_val {
            Some(lv) => Value::fixnum(lv),
            None => Value::NIL,
        });
    }

    // --- Buffer OBJECT ---
    let buf_id = match args.get(1) {
        None | Some(ValueKind::Nil) => buffers
            .current_buffer()
            .map(|b| b.id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")])),
        Some(ValueKind::Veclike(VecLikeType::Buffer)) => Ok(*id),
        Some(other) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("buffer-or-string-p"), *other],
        )),
    }?;

    let buf = buffers
        .get(buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;

    let byte_pos = buf.lisp_pos_to_byte(pos);

    let (byte_limit, limit_val) = match args.get(2) {
        Some(v) if !v.is_nil() => {
            let limit = super::buffers::expect_integer_or_marker_in_buffers(buffers, v)?;
            let limit_byte = buf.lisp_pos_to_byte(limit);
            (Some(limit_byte), Some(limit))
        }
        _ => (None, None),
    };

    let ref_byte = if byte_pos > 0 { byte_pos - 1 } else { 0 };
    let current_props = buf.text.text_props_get_properties(ref_byte);
    let mut cursor = byte_pos;

    loop {
        match buf.text.text_props_previous_change(cursor) {
            Some(prev) => {
                if let (Some(lim_byte), Some(lv)) = (byte_limit, limit_val) {
                    if prev <= lim_byte {
                        return Ok(Value::fixnum(lv));
                    }
                }

                let check = if prev > 0 { prev - 1 } else { 0 };
                let new_props = buf.text.text_props_get_properties(check);
                if new_props != current_props {
                    return Ok(Value::fixnum(buf.text.byte_to_char(prev) as i64 + 1));
                }

                if prev == 0 {
                    break;
                }
                cursor = if prev < cursor { prev } else { prev - 1 };
            }
            None => break,
        }
    }

    match limit_val {
        Some(lv) => Ok(Value::fixnum(lv)),
        None => Ok(Value::NIL),
    }
}

pub(crate) fn builtin_previous_char_property_change(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_previous_char_property_change_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_previous_char_property_change_in_buffers(
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("previous-char-property-change", &args, 1)?;
    expect_max_args("previous-char-property-change", &args, 2)?;

    let mut forwarded = vec![args[0], Value::NIL];
    if let Some(limit) = args.get(1) {
        forwarded.push(*limit);
    }
    let result = builtin_previous_property_change_in_buffers(buffers, forwarded)?;
    if !result.is_nil() {
        return Ok(result);
    }

    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    Ok(Value::fixnum(buf.point_min_char() as i64 + 1))
}

pub(crate) fn builtin_next_single_char_property_change(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_next_single_char_property_change_in_buffers(&eval.obarray, &eval.buffers, args)
}

pub(crate) fn builtin_next_single_char_property_change_in_buffers(
    obarray: &crate::emacs_core::symbol::Obarray,
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("next-single-char-property-change", &args, 2)?;
    expect_max_args("next-single-char-property-change", &args, 4)?;

    if let Some(ValueKind::String) = args.get(2) {
        if let Some(limit) = args.get(3) {
            if !limit.is_nil() {
                return Ok(Value::fixnum(expect_integer_or_marker(limit)?));
            }
        }
        return Ok(Value::fixnum(
            with_heap(|h| h.get_string(*id).chars().count()) as i64
        ));
    }

    let result = super::textprop::builtin_next_single_property_change_in_state(
        obarray,
        buffers,
        args.clone(),
    )?;
    if !result.is_nil() {
        return Ok(result);
    }

    let upper = match args.get(2) {
        Some(ValueKind::Veclike(VecLikeType::Buffer)) => {
            let buf = buffers
                .get(*id)
                .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;
            buf.point_max_char() as i64 + 1
        }
        _ => {
            let buf = buffers
                .current_buffer()
                .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
            buf.point_max_char() as i64 + 1
        }
    };
    Ok(Value::fixnum(upper))
}

pub(crate) fn builtin_previous_single_char_property_change(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_previous_single_char_property_change_in_buffers(&eval.obarray, &eval.buffers, args)
}

pub(crate) fn builtin_previous_single_char_property_change_in_buffers(
    obarray: &crate::emacs_core::symbol::Obarray,
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("previous-single-char-property-change", &args, 2)?;
    expect_max_args("previous-single-char-property-change", &args, 4)?;

    if let Some(ValueKind::String) = args.get(2) {
        if let Some(limit) = args.get(3) {
            if !limit.is_nil() {
                return Ok(Value::fixnum(expect_integer_or_marker(limit)?));
            }
        }
        return Ok(Value::fixnum(0));
    }

    let result = super::textprop::builtin_previous_single_property_change_in_state(
        obarray,
        buffers,
        args.clone(),
    )?;
    if !result.is_nil() {
        return Ok(result);
    }

    let lower = match args.get(2) {
        Some(ValueKind::Veclike(VecLikeType::Buffer)) => {
            let buf = buffers
                .get(*id)
                .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;
            buf.point_min_char() as i64 + 1
        }
        _ => {
            let buf = buffers
                .current_buffer()
                .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
            buf.point_min_char() as i64 + 1
        }
    };
    Ok(Value::fixnum(lower))
}

pub(crate) fn builtin_defalias(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let plan = plan_defalias_in_obarray(eval.obarray(), &args)?;
    let DefaliasPlan {
        action,
        docstring,
        result,
    } = plan;
    match action {
        DefaliasAction::SetFunction { symbol, definition } => {
            eval.obarray_mut()
                .set_symbol_function_id(symbol, definition);
        }
        DefaliasAction::CallHook {
            hook,
            symbol_value,
            definition,
        } => {
            eval.apply(hook, vec![symbol_value, definition])?;
        }
    }
    if let Some(docstring) = docstring {
        super::symbols::builtin_put(
            eval,
            vec![result, Value::symbol("function-documentation"), docstring],
        )?;
    }
    Ok(result)
}

pub(crate) enum DefaliasAction {
    SetFunction {
        symbol: SymId,
        definition: Value,
    },
    CallHook {
        hook: Value,
        symbol_value: Value,
        definition: Value,
    },
}

pub(crate) struct DefaliasPlan {
    pub(crate) action: DefaliasAction,
    pub(crate) docstring: Option<Value>,
    pub(crate) result: Value,
}

pub(crate) fn plan_defalias_in_obarray(
    obarray: &Obarray,
    args: &[Value],
) -> Result<DefaliasPlan, Flow> {
    expect_range_args("defalias", args, 2, 3)?;
    let symbol = match args[0].kind() {
        ValueKind::Nil => intern("nil"),
        ValueKind::T => intern("t"),
        ValueKind::Symbol(id) | ValueKind::Keyword(id) => id,
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[0]],
            ));
        }
    };
    if symbol == intern("nil") {
        return Err(signal("setting-constant", vec![Value::symbol("nil")]));
    }
    let definition = args[1];
    if super::symbols::would_create_function_alias_cycle_in_obarray(obarray, symbol, &definition) {
        return Err(signal("cyclic-function-indirection", vec![args[0]]));
    }
    let result = match args[0].kind() {
        ValueKind::Nil => Value::NIL,
        ValueKind::T => Value::T,
        ValueKind::Keyword(_) => args[0],
        _ => Value::symbol(symbol),
    };
    let hook = obarray
        .get_property_id(symbol, intern("defalias-fset-function"))
        .cloned()
        .unwrap_or(Value::NIL);
    let action = if hook.is_nil() {
        DefaliasAction::SetFunction { symbol, definition }
    } else {
        DefaliasAction::CallHook {
            hook,
            symbol_value: result,
            definition,
        }
    };
    let docstring = args.get(2).copied().filter(|value| !value.is_nil());
    Ok(DefaliasPlan {
        action,
        docstring,
        result,
    })
}

pub(crate) fn builtin_provide(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_range_args("provide", &args, 1, 2)?;
    eval.provide_value(args[0], args.get(1).cloned())
}

pub(crate) fn builtin_require(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_range_args("require", &args, 1, 3)?;
    eval.require_value(args[0], args.get(1).cloned(), args.get(2).cloned())
}

// ===========================================================================
// Loading / eval
// ===========================================================================

/// Convert an EvalError back to a Flow for builtins that call load_file.
fn eval_error_to_flow(e: super::error::EvalError) -> Flow {
    match e {
        super::error::EvalError::Signal {
            symbol,
            data,
            raw_data,
        } => Flow::Signal(super::error::SignalData {
            symbol,
            data,
            raw_data,
            suppress_signal_hook: false,
            selected_resume: None,
            search_complete: false,
        }),
        super::error::EvalError::UncaughtThrow { tag, value } => {
            // The throw was uncaught in the sub-evaluation — surface as no-catch signal.
            super::error::signal("no-catch", vec![tag, value])
        }
    }
}

/// `(garbage-collect)` — run a full GC cycle and return memory statistics.
pub(super) fn builtin_garbage_collect(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("garbage-collect", &args, 0)?;
    eval.gc_collect();
    // Return GC stats.
    super::builtins_extra::builtin_garbage_collect_stats()
}

pub(crate) fn builtin_load(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("load", &args, 1)?;
    match super::load::plan_load_in_state(
        &eval.obarray,
        args[0],
        args.get(1).copied(),
        args.get(3).copied(),
        args.get(4).copied(),
    )? {
        super::load::LoadPlan::Return(value) => Ok(value),
        super::load::LoadPlan::Load { path } => super::load::load_file_with_flags(
            eval,
            &path,
            args.get(1).is_some_and(|v| v.is_truthy()),
            args.get(2).is_some_and(|v| v.is_truthy()),
        )
        .map_err(eval_error_to_flow),
    }
}

pub(crate) fn builtin_load_file(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("load-file", &args, 1)?;
    let file = expect_string(&args[0])?;
    let path = std::path::Path::new(&file);
    super::load::load_file(eval, path).map_err(eval_error_to_flow)
}

pub(crate) fn builtin_eval(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("eval", &args, 1)?;
    expect_max_args("eval", &args, 2)?;
    eval.eval_value_with_lexical_arg(args[0], args.get(1).copied())
}

// Misc builtins
// ===========================================================================

pub(super) fn dynamic_or_global_symbol_value(
    eval: &super::eval::Context,
    name: &str,
) -> Option<Value> {
    eval.obarray.symbol_value(name).cloned()
}

pub(super) fn dynamic_or_global_symbol_value_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    _dynamic: &[OrderedRuntimeBindingMap],
    name: &str,
) -> Option<Value> {
    obarray.symbol_value(name).cloned()
}

fn buffer_local_or_global_symbol_value(
    obarray: &crate::emacs_core::symbol::Obarray,
    buf: &crate::buffer::Buffer,
    name: &str,
) -> Option<Value> {
    if let Some(binding) = buf.get_buffer_local_binding(name) {
        return binding.as_value();
    }
    obarray.symbol_value(name).cloned()
}

fn text_property_stickiness_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    buffers: &crate::buffer::BufferManager,
    buf: &crate::buffer::Buffer,
    pos: i64,
    prop: &str,
) -> i8 {
    let ignore_previous_character = pos <= buf.point_min_char() as i64 + 1;

    let default_nonsticky =
        buffer_local_or_global_symbol_value(obarray, buf, "text-property-default-nonsticky");

    let mut rear_sticky = !(ignore_previous_character
        || default_nonsticky
            .and_then(|value| assq_cdr(&value, prop))
            .is_some_and(|value| value.is_truthy()));

    if rear_sticky && !ignore_previous_character {
        let previous_props =
            text_property_value_at_char_pos(obarray, buffers, buf, pos - 1, "rear-nonsticky");
        if matches_rear_nonsticky(previous_props, prop) {
            rear_sticky = false;
        }
    }

    let front_sticky = matches_front_sticky(
        text_property_value_at_char_pos(obarray, buffers, buf, pos, "front-sticky"),
        prop,
    );

    if rear_sticky && !front_sticky {
        return -1;
    }
    if !rear_sticky && front_sticky {
        return 1;
    }
    if !rear_sticky && !front_sticky {
        return 0;
    }

    if ignore_previous_character
        || text_property_value_at_char_pos(obarray, buffers, buf, pos - 1, prop).is_nil()
    {
        1
    } else {
        -1
    }
}

pub(crate) fn inherited_text_properties_for_inserted_range_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buf: &crate::buffer::Buffer,
    insert_start: usize,
    insert_len: usize,
) -> Vec<(String, Value)> {
    let left_props = if insert_start > buf.point_min_byte() {
        buf.text
            .text_props_get_properties_ordered(insert_start.saturating_sub(1))
    } else {
        Vec::new()
    };
    let right_pos = insert_start.saturating_add(insert_len);
    let right_props = if right_pos < buf.point_max_byte() {
        buf.text.text_props_get_properties_ordered(right_pos)
    } else {
        Vec::new()
    };

    let left_map: HashMap<String, Value> = left_props.iter().cloned().collect();
    let right_map: HashMap<String, Value> = right_props.iter().cloned().collect();
    let left_front = left_map.get("front-sticky").copied().unwrap_or(Value::NIL);
    let left_rear = left_map
        .get("rear-nonsticky")
        .copied()
        .unwrap_or(Value::NIL);
    let right_front = right_map.get("front-sticky").copied().unwrap_or(Value::NIL);
    let right_rear = right_map
        .get("rear-nonsticky")
        .copied()
        .unwrap_or(Value::NIL);
    let default_nonsticky =
        buffer_local_or_global_symbol_value(obarray, buf, "text-property-default-nonsticky");

    let mut merged_props = Vec::new();
    let mut front_sticky = Vec::new();
    let mut rear_nonsticky = Vec::new();
    let mut seen = HashSet::new();

    for (name, right_value) in &right_props {
        if name == "front-sticky" || name == "rear-nonsticky" {
            continue;
        }
        seen.insert(name.clone());

        let left_present = left_map.contains_key(name);
        let left_value = left_map.get(name).copied().unwrap_or(Value::NIL);
        let default_entry = default_nonsticky
            .as_ref()
            .and_then(|value| assq_cdr(value, name));
        let default_rear_nonsticky = default_entry.as_ref().is_some_and(|v| v.is_truthy());
        let default_front_sticky = matches!(default_entry, Some(Value::NIL));

        let mut use_left =
            left_present && !(matches_rear_nonsticky(left_rear, name) || default_rear_nonsticky);
        let mut use_right = matches_front_sticky(right_front, name) || default_front_sticky;
        if use_left && use_right {
            if left_value.is_nil() {
                use_left = false;
            } else if right_value.is_nil() {
                use_right = false;
            }
        }

        if use_left {
            merged_props.push((name.clone(), left_value));
            if matches_front_sticky(left_front, name) {
                front_sticky.push(name.clone());
            }
            if matches_rear_nonsticky(left_rear, name) {
                rear_nonsticky.push(name.clone());
            }
        } else if use_right {
            merged_props.push((name.clone(), *right_value));
            if matches_front_sticky(right_front, name) {
                front_sticky.push(name.clone());
            }
            if matches_rear_nonsticky(right_rear, name) {
                rear_nonsticky.push(name.clone());
            }
        }
    }

    for (name, left_value) in &left_props {
        if name == "front-sticky" || name == "rear-nonsticky" || seen.contains(name) {
            continue;
        }

        let default_entry = default_nonsticky
            .as_ref()
            .and_then(|value| assq_cdr(value, name));
        let default_rear_nonsticky = default_entry.as_ref().is_some_and(|v| v.is_truthy());
        let default_front_sticky = matches!(default_entry, Some(Value::NIL));
        let left_nonsticky = matches_rear_nonsticky(left_rear, name);
        let right_sticky = matches_front_sticky(right_front, name) || default_front_sticky;

        if !(left_nonsticky || default_rear_nonsticky) {
            merged_props.push((name.clone(), *left_value));
            if matches_front_sticky(left_front, name) {
                front_sticky.push(name.clone());
            }
        } else if right_sticky {
            front_sticky.push(name.clone());
            if matches_rear_nonsticky(right_rear, name) {
                rear_nonsticky.push(name.clone());
            }
        }
    }

    if !rear_nonsticky.is_empty() {
        merged_props.insert(
            0,
            (
                "rear-nonsticky".to_string(),
                Value::list(
                    rear_nonsticky
                        .into_iter()
                        .map(Value::symbol)
                        .collect::<Vec<_>>(),
                ),
            ),
        );
    }

    if !front_sticky.is_empty() {
        merged_props.insert(
            0,
            (
                "front-sticky".to_string(),
                Value::list(
                    front_sticky
                        .into_iter()
                        .map(Value::symbol)
                        .collect::<Vec<_>>(),
                ),
            ),
        );
    }

    merged_props
}

fn text_property_value_at_char_pos(
    obarray: &crate::emacs_core::symbol::Obarray,
    buffers: &crate::buffer::BufferManager,
    buf: &crate::buffer::Buffer,
    pos: i64,
    prop: &str,
) -> Value {
    let byte_pos = buf.lisp_pos_to_byte(pos);
    super::textprop::lookup_buffer_text_property(obarray, buffers, buf, byte_pos, prop)
}

fn matches_front_sticky(value: Value, prop: &str) -> bool {
    value == Value::T || value_list_contains_symbol(&value, prop)
}

fn matches_rear_nonsticky(value: Value, prop: &str) -> bool {
    if value.is_nil() {
        return false;
    }
    if value.is_cons() {
        return value_list_contains_symbol(&value, prop);
    }
    true
}

fn assq_cdr(list: &Value, prop: &str) -> Option<Value> {
    let mut cursor = *list;
    while cursor.is_cons() {
        let entry = cursor.cons_car();
        if let Value::Cons(_) = entry
            && entry.cons_car().as_symbol_name() == Some(prop)
        {
            return Some(entry.cons_cdr());
        }
        cursor = cursor.cons_cdr();
    }
    None
}

fn value_list_contains_symbol(list: &Value, prop: &str) -> bool {
    let mut cursor = *list;
    while cursor.is_cons() {
        let item = cursor.cons_car();
        if item.as_symbol_name() == Some(prop) {
            return true;
        }
        cursor = cursor.cons_cdr();
    }
    false
}

pub(super) fn buffer_read_only_active(
    eval: &super::eval::Context,
    buf: &crate::buffer::Buffer,
) -> bool {
    if let Some(value) = buf.get_buffer_local("inhibit-read-only")
        && value.is_truthy()
    {
        return false;
    }

    if eval
        .obarray
        .symbol_value("inhibit-read-only")
        .is_some_and(|value| value.is_truthy())
    {
        return false;
    }

    if buf.read_only {
        return true;
    }

    if let Some(value) = buf.get_buffer_local("buffer-read-only") {
        return value.is_truthy();
    }

    eval.obarray
        .symbol_value("buffer-read-only")
        .is_some_and(|value| value.is_truthy())
}

pub(crate) fn builtin_barf_if_buffer_read_only(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_barf_if_buffer_read_only_impl(eval, args)
}

pub(crate) fn builtin_barf_if_buffer_read_only_impl(
    ctx: &crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("barf-if-buffer-read-only", &args, 1)?;
    let position = match args.first() {
        None | Some(ValueKind::Nil) => None,
        Some(value) => Some(expect_fixnum(value)?),
    };

    let Some(buf) = ctx.buffers.current_buffer() else {
        return Ok(Value::NIL);
    };
    let point_min = buf.point_min_char() as i64 + 1;
    let read_only =
        crate::emacs_core::editfns::buffer_read_only_active_in_state(&ctx.obarray, &[], buf);
    if !read_only {
        return Ok(Value::NIL);
    }
    let pos = position.unwrap_or_else(|| buf.point_char() as i64 + 1);
    if pos < point_min {
        return Err(signal(
            "args-out-of-range",
            vec![Value::fixnum(pos), Value::fixnum(pos)],
        ));
    }
    let prop_byte = buf.lisp_pos_to_accessible_byte(pos);
    if buf
        .text
        .text_props_get_property(prop_byte, "inhibit-read-only")
        .is_some_and(|value| value.is_truthy())
    {
        return Ok(Value::NIL);
    }
    Err(signal("buffer-read-only", vec![Value::make_buffer(buf.id)]))
}

pub(crate) fn builtin_bury_buffer_internal(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_bury_buffer_internal_impl(&eval.buffers, args)
}

pub(crate) fn builtin_bury_buffer_internal_impl(
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("bury-buffer-internal", &args, 1)?;
    let id = expect_buffer_id(&args[0])?;
    let _ = buffers.get(id);
    Ok(Value::NIL)
}

pub(crate) fn builtin_cancel_kbd_macro_events(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("cancel-kbd-macro-events", &args, 0)?;
    eval.cancel_kbd_macro_runtime_events();
    Ok(Value::NIL)
}

pub(crate) fn builtin_combine_after_change_execute(args: Vec<Value>) -> EvalResult {
    expect_args("combine-after-change-execute", &args, 0)?;
    Ok(Value::NIL)
}

fn resolve_print_target(eval: &super::eval::Context, printcharfun: Option<&Value>) -> Value {
    match printcharfun {
        Some(dest) if !dest.is_nil() => *dest,
        _ => dynamic_or_global_symbol_value(eval, "standard-output").unwrap_or(Value::T),
    }
}

pub(crate) fn resolve_print_target_in_state(
    ctx: &crate::emacs_core::eval::Context,
    printcharfun: Option<&Value>,
) -> Value {
    match printcharfun {
        Some(dest) if !dest.is_nil() => *dest,
        _ => ctx
            .obarray
            .symbol_value("standard-output")
            .cloned()
            .unwrap_or(Value::T),
    }
}

fn write_print_output_to_target(
    buffers: &mut crate::buffer::BufferManager,
    target: Value,
    text: &str,
) -> Result<(), Flow> {
    match target.kind() {
        ValueKind::T | ValueKind::Nil => Ok(()),
        ValueKind::Veclike(VecLikeType::Buffer) => {
            if buffers.get(id).is_none() {
                return Err(signal(
                    "error",
                    vec![Value::string("Output buffer no longer exists")],
                ));
            }
            let _ = buffers.insert_into_buffer(id, text);
            Ok(())
        }
        ValueKind::String => {
            let name = target.as_str().unwrap().to_owned();
            let Some(id) = buffers.find_buffer_by_name(&name) else {
                return Err(signal(
                    "error",
                    vec![Value::string(format!("No buffer named {name}"))],
                ));
            };
            if buffers.get(id).is_none() {
                return Err(signal(
                    "error",
                    vec![Value::string("Output buffer no longer exists")],
                ));
            }
            let _ = buffers.insert_into_buffer(id, text);
            Ok(())
        }
        other if super::marker::is_marker(&other) => {
            let Some((Some(buffer_id), _, _)) = super::marker::marker_logical_fields(&other) else {
                return Err(signal(
                    "error",
                    vec![Value::string("Marker does not point anywhere")],
                ));
            };
            let marker_pos = super::marker::marker_position_as_int_with_buffers(buffers, &other)?;
            let Some(buffer) = buffers.get(buffer_id) else {
                return Err(signal(
                    "error",
                    vec![Value::string("Output buffer no longer exists")],
                ));
            };
            let min_pos = buffer.point_min_char() as i64 + 1;
            let max_pos = buffer.point_max_char() as i64 + 1;
            if marker_pos < min_pos || marker_pos > max_pos {
                return Err(signal(
                    "error",
                    vec![Value::string(
                        "Marker is outside the accessible part of the buffer",
                    )],
                ));
            }
            let marker_byte = buffer.lisp_pos_to_byte(marker_pos);
            let saved_current = buffers.current_buffer_id();
            let saved_point = saved_current.and_then(|id| buffers.get(id).map(|buf| buf.point()));

            buffers.switch_current(buffer_id);
            let _ = buffers.goto_buffer_byte(buffer_id, marker_byte);
            let _ = buffers.insert_into_buffer(buffer_id, text);

            let new_marker_pos = buffers
                .get(buffer_id)
                .map(|buf| buf.point_char() as i64 + 1)
                .ok_or_else(|| {
                    signal(
                        "error",
                        vec![Value::string("Output buffer no longer exists")],
                    )
                })?;
            let _ = super::marker::builtin_set_marker_in_buffers(
                buffers,
                vec![other, Value::fixnum(new_marker_pos), ValueKind::Veclike(VecLikeType::Buffer)],
            )?;

            if let Some(saved_id) = saved_current {
                buffers.switch_current(saved_id);
                if let Some(old_point) = saved_point {
                    let restore_point = if saved_id == buffer_id && old_point >= marker_byte {
                        old_point + text.len()
                    } else {
                        old_point
                    };
                    let _ = buffers.goto_buffer_byte(saved_id, restore_point);
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

pub(crate) fn print_target_is_direct(target: Value) -> bool {
    matches!(
        target,
        Value::T | Value::NIL | Value::make_buffer(_) | Value::Str(_)
    ) || super::marker::is_marker(&target)
}

pub(crate) fn dispatch_print_callback_chars(
    text: &str,
    mut emit_char: impl FnMut(Value) -> Result<(), Flow>,
) -> Result<(), Flow> {
    for ch in text.chars() {
        emit_char(Value::fixnum(ch as i64))?;
    }
    Ok(())
}

fn write_print_output(
    eval: &mut super::eval::Context,
    printcharfun: Option<&Value>,
    text: &str,
) -> Result<(), Flow> {
    let target = resolve_print_target(eval, printcharfun);
    write_print_output_to_target(&mut eval.buffers, target, text)
}

fn write_print_output_from_ctx(
    ctx: &mut crate::emacs_core::eval::Context,
    printcharfun: Option<&Value>,
    text: &str,
) -> Result<(), Flow> {
    let target = resolve_print_target_in_state(ctx, printcharfun);
    write_print_output_to_target(&mut ctx.buffers, target, text)
}

fn write_terpri_output(eval: &mut super::eval::Context, target: Value) -> Result<(), Flow> {
    match target.kind() {
        ValueKind::T | ValueKind::Nil => Ok(()),
        ValueKind::Veclike(VecLikeType::Buffer) => {
            if eval.buffers.get(id).is_none() {
                return Err(signal(
                    "error",
                    vec![Value::string("Output buffer no longer exists")],
                ));
            }
            let _ = eval.buffers.insert_into_buffer(id, "\n");
            Ok(())
        }
        ValueKind::String => {
            let name = target.as_str().unwrap().to_owned();
            let Some(id) = eval.buffers.find_buffer_by_name(&name) else {
                return Err(signal(
                    "error",
                    vec![Value::string(format!("No buffer named {name}"))],
                ));
            };
            if eval.buffers.get(id).is_none() {
                return Err(signal(
                    "error",
                    vec![Value::string("Output buffer no longer exists")],
                ));
            }
            let _ = eval.buffers.insert_into_buffer(id, "\n");
            Ok(())
        }
        other => {
            // Root the callable target across eval.apply().
            eval.with_gc_scope_result(|ctx| {
                ctx.root(target);
                ctx.apply(target, vec![Value::fixnum('\n' as i64)])?;
                Ok(())
            })?;
            Ok(())
        }
    }
}

pub(super) fn print_value_eval(eval: &super::eval::Context, value: &Value) -> String {
    super::error::print_value_with_eval(eval, value)
}

fn print_value_princ_list_shorthand(
    value: &Value,
    render: &dyn Fn(&Value) -> String,
) -> Option<String> {
    let items = super::value::list_to_vec(value)?;
    if items.len() != 2 {
        return None;
    }

    let head = match items[0].kind() {
        ValueKind::Symbol(id) => resolve_sym(id),
        _ => return None,
    };
    let prefix = match head {
        "quote" => "'",
        "function" => "#'",
        "`" => "`",
        "," => ",",
        ",@" => ",@",
        _ => return None,
    };
    Some(format!("{prefix}{}", render(&items[1])))
}

pub(super) fn print_value_princ(value: &Value) -> String {
    match value.kind() {
        ValueKind::String => value.as_str().unwrap().to_owned(),
        ValueKind::Symbol(id) => resolve_sym(id).to_owned(),
        ValueKind::Keyword(id) => resolve_sym(id).to_owned(),
        ValueKind::Cons => {
            if let Some(shorthand) = print_value_princ_list_shorthand(value, &print_value_princ) {
                return shorthand;
            }
            let mut out = String::from("(");
            let mut cursor = *value;
            let mut first = true;
            loop {
                match cursor.kind() {
                    ValueKind::Cons => {
                        if !first {
                            out.push(' ');
                        }
                        let pair_car = cursor.cons_car();
                        let pair_cdr = cursor.cons_cdr();
                        out.push_str(&print_value_princ(&pair_car));
                        cursor = pair_cdr;
                        first = false;
                    }
                    ValueKind::Nil => break,
                    other => {
                        if !first {
                            out.push_str(" . ");
                        }
                        out.push_str(&print_value_princ(&cursor));
                        break;
                    }
                }
            }
            out.push(')');
            out
        }
        ValueKind::Veclike(VecLikeType::Vector) => {
            let items = value.as_vector_data().unwrap().clone();
            let parts: Vec<String> = items.iter().map(print_value_princ).collect();
            format!("[{}]", parts.join(" "))
        }
        ValueKind::Veclike(VecLikeType::Record) => {
            let items = value.as_record_data().unwrap().clone();
            let parts: Vec<String> = items.iter().map(print_value_princ).collect();
            format!("#s({})", parts.join(" "))
        }
        other => super::print::print_value(value),
    }
}

pub(crate) fn print_value_princ_in_state(
    ctx: &crate::emacs_core::eval::Context,
    value: &Value,
) -> String {
    if super::terminal::pure::print_terminal_handle(value).is_some()
        || ctx.threads.thread_id_from_handle(value).is_some()
        || ctx.threads.mutex_id_from_handle(value).is_some()
        || ctx
            .threads
            .condition_variable_id_from_handle(value)
            .is_some()
    {
        return super::error::print_value_in_state(ctx, value);
    }
    match value.kind() {
        ValueKind::String => value.as_str().unwrap().to_owned(),
        ValueKind::Symbol(id) => resolve_sym(id).to_owned(),
        ValueKind::Keyword(id) => resolve_sym(id).to_owned(),
        ValueKind::Veclike(VecLikeType::Buffer) => {
            if let Some(buf) = ctx.buffers.get(*id) {
                return buf.name.clone();
            }
            if ctx.buffers.dead_buffer_last_name(*id).is_some() {
                return "#<killed buffer>".to_string();
            }
            super::error::print_value_in_state(ctx, value)
        }
        ValueKind::Cons => {
            if let Some(shorthand) = print_value_princ_list_shorthand(value, &|item| {
                print_value_princ_in_state(ctx, item)
            }) {
                return shorthand;
            }
            let mut out = String::from("(");
            let mut cursor = *value;
            let mut first = true;
            loop {
                match cursor.kind() {
                    ValueKind::Cons => {
                        if !first {
                            out.push(' ');
                        }
                        let pair_car = cursor.cons_car();
                        let pair_cdr = cursor.cons_cdr();
                        out.push_str(&print_value_princ_in_state(ctx, &pair_car));
                        cursor = pair_cdr;
                        first = false;
                    }
                    ValueKind::Nil => break,
                    other => {
                        if !first {
                            out.push_str(" . ");
                        }
                        out.push_str(&print_value_princ_in_state(ctx, &cursor));
                        break;
                    }
                }
            }
            out.push(')');
            out
        }
        ValueKind::Veclike(VecLikeType::Vector) => {
            let items = value.as_vector_data().unwrap().clone();
            let parts: Vec<String> = items
                .iter()
                .map(|item| print_value_princ_in_state(ctx, item))
                .collect();
            format!("[{}]", parts.join(" "))
        }
        ValueKind::Veclike(VecLikeType::Record) => {
            let items = value.as_record_data().unwrap().clone();
            let parts: Vec<String> = items
                .iter()
                .map(|item| print_value_princ_in_state(ctx, item))
                .collect();
            format!("#s({})", parts.join(" "))
        }
        other => super::error::print_value_in_state(ctx, value),
    }
}

pub(super) fn print_value_princ_eval(eval: &super::eval::Context, value: &Value) -> String {
    print_value_princ_in_state(eval, value)
}

fn prin1_to_string_value(value: &Value, noescape: bool) -> String {
    if noescape {
        match value.kind() {
            ValueKind::String => value.as_str().unwrap().to_owned(),
            other => super::print::print_value(value),
        }
    } else {
        bytes_to_storage_string(&super::print::print_value_bytes(value))
    }
}

fn prin1_to_string_value_eval(
    eval: &super::eval::Context,
    value: &Value,
    noescape: bool,
) -> String {
    prin1_to_string_value_in_state(eval, value, noescape)
}

pub(crate) fn prin1_to_string_value_in_state(
    ctx: &crate::emacs_core::eval::Context,
    value: &Value,
    noescape: bool,
) -> String {
    if noescape {
        match value.kind() {
            ValueKind::String => value.as_str().unwrap().to_owned(),
            other => super::error::print_value_in_state(ctx, value),
        }
    } else {
        bytes_to_storage_string(&super::error::print_value_bytes_in_state(
            &ctx.obarray,
            &ctx.buffers,
            &ctx.frames,
            &ctx.threads,
            value,
        ))
    }
}

pub(crate) fn builtin_princ(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("princ", &args, 1)?;
    let target = resolve_print_target(eval, args.get(1));
    if print_target_is_direct(target) {
        return builtin_princ_impl(eval, args);
    }

    let text = print_value_princ_in_state(eval, &args[0]);
    eval.with_gc_scope_result(|ctx| {
        ctx.root(target);
        dispatch_print_callback_chars(&text, |ch| ctx.apply(target, vec![ch]).map(|_| ()))
    })?;
    Ok(args[0])
}

pub(crate) fn builtin_princ_impl(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("princ", &args, 1)?;
    let text = print_value_princ_in_state(ctx, &args[0]);
    write_print_output_from_ctx(ctx, args.get(1), &text)?;
    Ok(args[0])
}

pub(crate) fn builtin_prin1(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("prin1", &args, 1)?;
    let target = resolve_print_target(eval, args.get(1));
    if print_target_is_direct(target) {
        return builtin_prin1_impl(eval, args);
    }

    let text = super::error::print_value_in_state(eval, &args[0]);
    eval.with_gc_scope_result(|ctx| {
        ctx.root(target);
        dispatch_print_callback_chars(&text, |ch| ctx.apply(target, vec![ch]).map(|_| ()))
    })?;
    Ok(args[0])
}

pub(crate) fn builtin_prin1_impl(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("prin1", &args, 1)?;
    let text = super::error::print_value_in_state(ctx, &args[0]);
    write_print_output_from_ctx(ctx, args.get(1), &text)?;
    Ok(args[0])
}

pub(crate) fn builtin_prin1_to_string(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_prin1_to_string_impl(eval, args)
}

pub(crate) fn builtin_prin1_to_string_impl(
    ctx: &crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("prin1-to-string", &args, 1)?;
    let noescape = args.get(1).is_some_and(|v| v.is_truthy());
    Ok(Value::string(prin1_to_string_value_in_state(
        ctx, &args[0], noescape,
    )))
}

pub(crate) fn builtin_print(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("print", &args, 1)?;
    let target = resolve_print_target(eval, args.get(1));
    if print_target_is_direct(target) {
        return builtin_print_impl(eval, args);
    }

    let mut text = String::new();
    text.push('\n');
    text.push_str(&super::error::print_value_in_state(eval, &args[0]));
    text.push('\n');
    eval.with_gc_scope_result(|ctx| {
        ctx.root(target);
        dispatch_print_callback_chars(&text, |ch| ctx.apply(target, vec![ch]).map(|_| ()))
    })?;
    Ok(args[0])
}

pub(crate) fn builtin_print_impl(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("print", &args, 1)?;
    let mut text = String::new();
    text.push('\n');
    text.push_str(&super::error::print_value_in_state(ctx, &args[0]));
    text.push('\n');
    write_print_output_from_ctx(ctx, args.get(1), &text)?;
    Ok(args[0])
}

pub(crate) fn builtin_terpri(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = builtin_terpri_impl(eval, args.clone())? {
        return Ok(result);
    }
    finish_terpri_in_eval(eval, &args)
}

pub(crate) fn builtin_terpri_impl(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> Result<Option<Value>, Flow> {
    expect_max_args("terpri", &args, 2)?;
    let target = resolve_print_target_in_state(ctx, args.first());
    if print_target_is_direct(target) {
        write_print_output_to_target(&mut ctx.buffers, target, "\n")?;
        return Ok(Some(Value::T));
    }
    Ok(None)
}

pub(crate) fn finish_terpri_in_eval(eval: &mut super::eval::Context, args: &[Value]) -> EvalResult {
    expect_max_args("terpri", args, 2)?;
    let target = resolve_print_target(eval, args.first());
    write_terpri_output(eval, target)?;
    Ok(Value::T)
}

pub(super) fn write_char_rendered_text(char_code: i64) -> Option<String> {
    if !(0..=u32::MAX as i64).contains(&char_code) {
        return None;
    }
    let code = char_code as u32;
    char::from_u32(code)
        .map(|ch| ch.to_string())
        .or_else(|| encode_nonunicode_char_for_storage(code))
}

pub(crate) fn builtin_write_char(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    if let Some(result) = builtin_write_char_impl(eval, args.clone())? {
        return Ok(result);
    }
    finish_write_char_in_eval(eval, &args)
}

pub(crate) fn finish_write_char_in_eval(
    eval: &mut super::eval::Context,
    args: &[Value],
) -> EvalResult {
    expect_range_args("write-char", args, 1, 2)?;
    let char_code = expect_fixnum(&args[0])?;
    let target = resolve_print_target(eval, args.get(1));

    match target.kind() {
        ValueKind::T | ValueKind::Nil => {}
        ValueKind::Veclike(VecLikeType::Buffer) => {
            if let Some(text) = write_char_rendered_text(char_code) {
                if eval.buffers.get(id).is_none() {
                    return Err(signal(
                        "error",
                        vec![Value::string("Output buffer no longer exists")],
                    ));
                }
                let _ = eval.buffers.insert_into_buffer(id, &text);
            }
        }
        ValueKind::String => {
            if let Some(text) = write_char_rendered_text(char_code) {
                let name = target.as_str().unwrap().to_owned();
                let Some(id) = eval.buffers.find_buffer_by_name(&name) else {
                    return Err(signal(
                        "error",
                        vec![Value::string(format!("No buffer named {name}"))],
                    ));
                };
                if eval.buffers.get(id).is_none() {
                    return Err(signal(
                        "error",
                        vec![Value::string("Output buffer no longer exists")],
                    ));
                }
                let _ = eval.buffers.insert_into_buffer(id, &text);
            }
        }
        other => {
            eval.with_gc_scope_result(|ctx| {
                ctx.root(target);
                ctx.apply(target, vec![Value::fixnum(char_code)])?;
                Ok(())
            })?;
        }
    }

    Ok(Value::fixnum(char_code))
}

pub(crate) fn builtin_write_char_impl(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> Result<Option<Value>, Flow> {
    expect_range_args("write-char", &args, 1, 2)?;
    let char_code = expect_fixnum(&args[0])?;
    let target = resolve_print_target_in_state(ctx, args.get(1));

    if print_target_is_direct(target) {
        if let Some(text) = write_char_rendered_text(char_code) {
            write_print_output_to_target(&mut ctx.buffers, target, &text)?;
        }
        return Ok(Some(Value::fixnum(char_code)));
    }

    Ok(None)
}

pub(crate) fn builtin_propertize(args: Vec<Value>) -> EvalResult {
    expect_min_args("propertize", &args, 1)?;

    let s = match args[0].kind() {
        ValueKind::String => args[0].as_str().unwrap().to_owned(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), args[0]],
            ));
        }
    };

    // `propertize` requires an odd argument count: 1 string + plist pairs.
    if args.len().is_multiple_of(2) {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("propertize"), Value::fixnum(args.len() as i64)],
        ));
    }

    // Create a copy of the string
    let new_str = Value::string(&s);
    let new_id = match new_str.kind() {
        ValueKind::String => *id,
        _ => unreachable!(),
    };

    // Copy existing text properties from source string
    if args[0].is_string() {
        if let Some(src_table) = get_string_text_properties_table(*src_id) {
            set_string_text_properties_table(new_id, src_table);
        }
    }

    // Parse and apply plist properties.
    // GNU Emacs's `propertize` reverses the property list before calling
    // `add_text_properties`, which then prepends each property. The net
    // effect is that properties appear in the original order in the plist.
    // We match this by iterating the pairs in reverse order, since our
    // `put_property` prepends new properties.
    if args.len() > 1 {
        let byte_len = s.len();
        let mut table = get_string_text_properties_table(new_id)
            .unwrap_or_else(|| crate::buffer::text_props::TextPropertyTable::new());
        let pairs = &args[1..];
        // Collect chunks and iterate in reverse to match GNU's reversal behavior
        let chunks: Vec<&[Value]> = pairs.chunks(2).collect();
        for chunk in chunks.iter().rev() {
            if chunk.len() == 2 {
                if let Some(name) = chunk[0].as_symbol_name() {
                    table.put_property(0, byte_len, name, chunk[1]);
                } else if let Some(name) = match &chunk[0] {
                    Value::keyword(id) => Some(resolve_sym(*id).to_owned()),
                    _ => None,
                } {
                    table.put_property(0, byte_len, &name, chunk[1]);
                }
            }
        }
        set_string_text_properties_table(new_id, table);
    }

    Ok(new_str)
}

pub(crate) fn builtin_current_cpu_time(args: Vec<Value>) -> EvalResult {
    expect_args("current-cpu-time", &args, 0)?;
    use std::sync::OnceLock;
    use std::time::Instant;
use crate::emacs_core::value::{ValueKind, VecLikeType};
    static CPU_TIME_START: OnceLock<Instant> = OnceLock::new();
    let start = CPU_TIME_START.get_or_init(Instant::now);
    let ticks = start.elapsed().as_micros() as i64;
    Ok(Value::cons(Value::fixnum(ticks), Value::fixnum(1_000_000)))
}

pub(crate) fn builtin_current_idle_time(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("current-idle-time", &args, 0)?;
    Ok(eval.current_idle_time_value())
}
