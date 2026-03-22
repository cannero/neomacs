use super::*;
use crate::emacs_core::symbol::Obarray;

pub(crate) fn builtin_get_pos_property(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_get_pos_property_in_state(&eval.obarray, &eval.dynamic, &eval.buffers, args)
}

pub(crate) fn builtin_get_pos_property_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
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
            if let Some(value) = table.get_property(byte_pos, &prop) {
                return Ok(*value);
            }
        }
        return Ok(Value::Nil);
    }

    let buf_id = match args.get(2) {
        None | Some(Value::Nil) => buffers
            .current_buffer()
            .map(|b| b.id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")])),
        Some(Value::Buffer(id)) => Ok(*id),
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

    match text_property_stickiness_in_state(obarray, dynamic, buf, pos, &prop) {
        1 => Ok(text_property_value_at_char_pos(buf, pos, &prop)),
        -1 if pos > buf.point_min_char() as i64 + 1 => {
            Ok(text_property_value_at_char_pos(buf, pos - 1, &prop))
        }
        _ => Ok(Value::Nil),
    }
}

pub(crate) fn builtin_next_char_property_change(
    eval: &mut super::eval::Evaluator,
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
    let result = match args.len() {
        1 => super::textprop::builtin_next_property_change_in_buffers(buffers, args)?,
        2 => super::textprop::builtin_next_property_change_in_buffers(
            buffers,
            vec![args[0], Value::Nil, args[1]],
        )?,
        _ => unreachable!(),
    };
    if !result.is_nil() {
        return Ok(result);
    }

    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    Ok(Value::Int(buf.point_max_char() as i64 + 1))
}

pub(crate) fn builtin_pos_bol(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    builtin_pos_bol_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_pos_eol(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    builtin_pos_eol_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_pos_bol_in_buffers(
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("pos-bol", &args, 1)?;
    super::navigation::builtin_line_beginning_position_in_manager(buffers, args)
}

pub(crate) fn builtin_pos_eol_in_buffers(
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("pos-eol", &args, 1)?;
    super::navigation::builtin_line_end_position_in_manager(buffers, args)
}

pub(crate) fn builtin_previous_property_change(
    eval: &mut super::eval::Evaluator,
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
    if let Some(Value::Str(str_id)) = args.get(1) {
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
                                Some(lv) => Value::Int(lv),
                                None => Value::Nil,
                            });
                        }
                    }
                    let check = if prev > 0 { prev - 1 } else { 0 };
                    let new_props = table.get_properties(check);
                    if new_props != current_props {
                        return Ok(Value::Int(textprop::string_byte_to_elisp_pos(&s, prev)));
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
            Some(lv) => Value::Int(lv),
            None => Value::Nil,
        });
    }

    // --- Buffer OBJECT ---
    let buf_id = match args.get(1) {
        None | Some(Value::Nil) => buffers
            .current_buffer()
            .map(|b| b.id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")])),
        Some(Value::Buffer(id)) => Ok(*id),
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
    let current_props = buf.text_props.get_properties(ref_byte);
    let mut cursor = byte_pos;

    loop {
        match buf.text_props.previous_property_change(cursor) {
            Some(prev) => {
                if let (Some(lim_byte), Some(lv)) = (byte_limit, limit_val) {
                    if prev <= lim_byte {
                        return Ok(Value::Int(lv));
                    }
                }

                let check = if prev > 0 { prev - 1 } else { 0 };
                let new_props = buf.text_props.get_properties(check);
                if new_props != current_props {
                    return Ok(Value::Int(buf.text.byte_to_char(prev) as i64 + 1));
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
        Some(lv) => Ok(Value::Int(lv)),
        None => Ok(Value::Nil),
    }
}

pub(crate) fn builtin_previous_char_property_change(
    eval: &mut super::eval::Evaluator,
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

    let mut forwarded = vec![args[0], Value::Nil];
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
    Ok(Value::Int(buf.point_min_char() as i64 + 1))
}

pub(crate) fn builtin_next_single_char_property_change(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_next_single_char_property_change_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_next_single_char_property_change_in_buffers(
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("next-single-char-property-change", &args, 2)?;
    expect_max_args("next-single-char-property-change", &args, 4)?;

    if let Some(Value::Str(id)) = args.get(2) {
        if let Some(limit) = args.get(3) {
            if !limit.is_nil() {
                return Ok(Value::Int(expect_integer_or_marker(limit)?));
            }
        }
        return Ok(Value::Int(
            with_heap(|h| h.get_string(*id).chars().count()) as i64
        ));
    }

    let result =
        super::textprop::builtin_next_single_property_change_in_buffers(buffers, args.clone())?;
    if !result.is_nil() {
        return Ok(result);
    }

    let upper = match args.get(2) {
        Some(Value::Buffer(id)) => {
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
    Ok(Value::Int(upper))
}

pub(crate) fn builtin_previous_single_char_property_change(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_previous_single_char_property_change_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_previous_single_char_property_change_in_buffers(
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("previous-single-char-property-change", &args, 2)?;
    expect_max_args("previous-single-char-property-change", &args, 4)?;

    if let Some(Value::Str(_)) = args.get(2) {
        if let Some(limit) = args.get(3) {
            if !limit.is_nil() {
                return Ok(Value::Int(expect_integer_or_marker(limit)?));
            }
        }
        return Ok(Value::Int(0));
    }

    let result =
        super::textprop::builtin_previous_single_property_change_in_buffers(buffers, args.clone())?;
    if !result.is_nil() {
        return Ok(result);
    }

    let lower = match args.get(2) {
        Some(Value::Buffer(id)) => {
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
    Ok(Value::Int(lower))
}

pub(crate) fn builtin_defalias(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
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
        super::symbols::builtin_put_in_obarray(
            eval.obarray_mut(),
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
    let symbol = match args[0] {
        Value::Nil => intern("nil"),
        Value::True => intern("t"),
        Value::Symbol(id) | Value::Keyword(id) => id,
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
    let result = match args[0] {
        Value::Nil => Value::Nil,
        Value::True => Value::True,
        Value::Keyword(_) => args[0],
        _ => Value::Symbol(symbol),
    };
    let hook = obarray
        .get_property_id(symbol, intern("defalias-fset-function"))
        .cloned()
        .unwrap_or(Value::Nil);
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

pub(crate) fn builtin_provide(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_range_args("provide", &args, 1, 2)?;
    eval.provide_value(args[0], args.get(1).cloned())
}

pub(crate) fn builtin_provide_in_state(
    obarray: &mut Obarray,
    features: &mut Vec<SymId>,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("provide", &args, 1, 2)?;
    crate::emacs_core::eval::provide_value_in_state(
        obarray,
        features,
        args[0],
        args.get(1).cloned(),
    )
}

pub(crate) fn builtin_require(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_range_args("require", &args, 1, 3)?;
    eval.require_value(args[0], args.get(1).cloned(), args.get(2).cloned())
}

// ===========================================================================
// Loading / eval
// ===========================================================================

/// Convert an EvalError back to a Flow for builtins that call load_file.
fn eval_error_to_flow(e: super::error::EvalError) -> Flow {
    match e {
        super::error::EvalError::Signal { symbol, data } => {
            Flow::Signal(super::error::SignalData {
                symbol,
                data,
                raw_data: None,
            })
        }
        super::error::EvalError::UncaughtThrow { tag, value } => {
            // The throw was uncaught in the sub-evaluation — surface as no-catch signal.
            super::error::signal("no-catch", vec![tag, value])
        }
    }
}

/// `(garbage-collect)` — run a full GC cycle and return memory statistics.
pub(super) fn builtin_garbage_collect_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("garbage-collect", &args, 0)?;
    eval.gc_collect();
    // Return the same stats format as the old stub for compatibility.
    super::builtins_extra::builtin_garbage_collect(vec![])
}

pub(crate) fn builtin_load(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("load", &args, 1)?;
    match super::load::plan_load_in_state(
        &eval.obarray,
        args[0],
        args.get(1).copied(),
        args.get(3).copied(),
        args.get(4).copied(),
    )? {
        super::load::LoadPlan::Return(value) => Ok(value),
        super::load::LoadPlan::Load { path } => {
            super::load::load_file(eval, &path).map_err(eval_error_to_flow)
        }
    }
}

pub(crate) fn builtin_load_file(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("load-file", &args, 1)?;
    let file = expect_string(&args[0])?;
    let path = std::path::Path::new(&file);
    super::load::load_file(eval, path).map_err(eval_error_to_flow)
}

/// `(neovm-precompile-file FILE)` -> cache path string
///
/// NeoVM extension: parse source `.el` and emit internal `.neoc` cache sidecar.
pub(crate) fn builtin_neovm_precompile_file(
    _eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_neovm_precompile_file_in_state(args)
}

pub(crate) fn builtin_neovm_precompile_file_in_state(args: Vec<Value>) -> EvalResult {
    expect_args("neovm-precompile-file", &args, 1)?;
    let file = expect_string(&args[0])?;
    let path = std::path::Path::new(&file);
    let cache = super::load::precompile_source_file(path).map_err(eval_error_to_flow)?;
    Ok(Value::string(cache.to_string_lossy()))
}

pub(crate) fn builtin_eval(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("eval", &args, 1)?;
    expect_max_args("eval", &args, 2)?;
    eval.eval_value_with_lexical_arg(args[0], args.get(1).copied())
}

// Misc builtins
// ===========================================================================

pub(super) fn dynamic_or_global_symbol_value(
    eval: &super::eval::Evaluator,
    name: &str,
) -> Option<Value> {
    dynamic_or_global_symbol_value_in_state(&eval.obarray, &eval.dynamic, name)
}

pub(super) fn dynamic_or_global_symbol_value_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    name: &str,
) -> Option<Value> {
    let name_id = intern(name);
    for frame in dynamic.iter().rev() {
        if let Some(value) = frame.get(&name_id) {
            return Some(*value);
        }
    }
    obarray.symbol_value(name).cloned()
}

fn text_property_stickiness_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buf: &crate::buffer::Buffer,
    pos: i64,
    prop: &str,
) -> i8 {
    let ignore_previous_character = pos <= buf.point_min_char() as i64 + 1;

    let default_nonsticky = dynamic_or_global_symbol_value_in_state(
        obarray,
        dynamic,
        "text-property-default-nonsticky",
    );

    let mut rear_sticky = !(ignore_previous_character
        || default_nonsticky
            .and_then(|value| assq_cdr(&value, prop))
            .is_some_and(|value| value.is_truthy()));

    if rear_sticky && !ignore_previous_character {
        let previous_props = text_property_value_at_char_pos(buf, pos - 1, "rear-nonsticky");
        if matches_rear_nonsticky(previous_props, prop) {
            rear_sticky = false;
        }
    }

    let front_sticky = matches_front_sticky(
        text_property_value_at_char_pos(buf, pos, "front-sticky"),
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

    if ignore_previous_character || text_property_value_at_char_pos(buf, pos - 1, prop).is_nil() {
        1
    } else {
        -1
    }
}

fn text_property_value_at_char_pos(buf: &crate::buffer::Buffer, pos: i64, prop: &str) -> Value {
    let byte_pos = buf.lisp_pos_to_byte(pos);
    buf.text_props
        .get_property(byte_pos, prop)
        .copied()
        .unwrap_or(Value::Nil)
}

fn matches_front_sticky(value: Value, prop: &str) -> bool {
    value == Value::True || value_list_contains_symbol(&value, prop)
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
    while let Value::Cons(_) = cursor {
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
    while let Value::Cons(_) = cursor {
        let item = cursor.cons_car();
        if item.as_symbol_name() == Some(prop) {
            return true;
        }
        cursor = cursor.cons_cdr();
    }
    false
}

pub(super) fn buffer_read_only_active(
    eval: &super::eval::Evaluator,
    buf: &crate::buffer::Buffer,
) -> bool {
    let inhibit_name_id = intern("inhibit-read-only");
    for frame in eval.dynamic.iter().rev() {
        if let Some(value) = frame.get(&inhibit_name_id)
            && value.is_truthy()
        {
            return false;
        }
    }

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

    let name_id = intern("buffer-read-only");
    for frame in eval.dynamic.iter().rev() {
        if let Some(value) = frame.get(&name_id) {
            return value.is_truthy();
        }
    }

    if let Some(value) = buf.get_buffer_local("buffer-read-only") {
        return value.is_truthy();
    }

    eval.obarray
        .symbol_value("buffer-read-only")
        .is_some_and(|value| value.is_truthy())
}

pub(crate) fn builtin_barf_if_buffer_read_only(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_barf_if_buffer_read_only_in_state(&eval.obarray, &eval.dynamic, &eval.buffers, args)
}

pub(crate) fn builtin_barf_if_buffer_read_only_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("barf-if-buffer-read-only", &args, 1)?;
    let position = match args.first() {
        None | Some(Value::Nil) => None,
        Some(value) => Some(expect_fixnum(value)?),
    };

    let Some(buf) = buffers.current_buffer() else {
        return Ok(Value::Nil);
    };
    let point_min = buf.point_min_char() as i64 + 1;
    let read_only =
        crate::emacs_core::editfns::buffer_read_only_active_in_state(obarray, dynamic, buf);
    if !read_only {
        return Ok(Value::Nil);
    }
    let pos = position.unwrap_or_else(|| buf.point_char() as i64 + 1);
    if pos < point_min {
        return Err(signal(
            "args-out-of-range",
            vec![Value::Int(pos), Value::Int(pos)],
        ));
    }
    let prop_byte = buf.lisp_pos_to_accessible_byte(pos);
    if buf
        .text_props
        .get_property(prop_byte, "inhibit-read-only")
        .is_some_and(Value::is_truthy)
    {
        return Ok(Value::Nil);
    }
    Err(signal("buffer-read-only", vec![Value::Buffer(buf.id)]))
}

pub(crate) fn builtin_bury_buffer_internal(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_bury_buffer_internal_in_state(&eval.buffers, args)
}

pub(crate) fn builtin_bury_buffer_internal_in_state(
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("bury-buffer-internal", &args, 1)?;
    let id = expect_buffer_id(&args[0])?;
    let _ = buffers.get(id);
    Ok(Value::Nil)
}

pub(crate) fn builtin_cancel_kbd_macro_events(args: Vec<Value>) -> EvalResult {
    expect_args("cancel-kbd-macro-events", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_combine_after_change_execute(args: Vec<Value>) -> EvalResult {
    expect_args("combine-after-change-execute", &args, 0)?;
    Ok(Value::Nil)
}

fn resolve_print_target(eval: &super::eval::Evaluator, printcharfun: Option<&Value>) -> Value {
    match printcharfun {
        Some(dest) if !dest.is_nil() => *dest,
        _ => dynamic_or_global_symbol_value(eval, "standard-output").unwrap_or(Value::True),
    }
}

pub(crate) fn resolve_print_target_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    printcharfun: Option<&Value>,
) -> Value {
    match printcharfun {
        Some(dest) if !dest.is_nil() => *dest,
        _ => dynamic_or_global_symbol_value_in_state(obarray, dynamic, "standard-output")
            .unwrap_or(Value::True),
    }
}

fn write_print_output_to_target(
    buffers: &mut crate::buffer::BufferManager,
    target: Value,
    text: &str,
) -> Result<(), Flow> {
    match target {
        Value::True | Value::Nil => Ok(()),
        Value::Buffer(id) => {
            if buffers.get(id).is_none() {
                return Err(signal(
                    "error",
                    vec![Value::string("Output buffer no longer exists")],
                ));
            }
            let _ = buffers.insert_into_buffer(id, text);
            Ok(())
        }
        Value::Str(name_id) => {
            let name = with_heap(|h| h.get_string(name_id).to_owned());
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
            let Some((Some(buffer_name), _, _)) = super::marker::marker_logical_fields(&other)
            else {
                return Err(signal(
                    "error",
                    vec![Value::string("Marker does not point anywhere")],
                ));
            };
            let Some(buffer_id) = buffers.find_buffer_by_name(&buffer_name) else {
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

            buffers.set_current(buffer_id);
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
                vec![other, Value::Int(new_marker_pos), Value::Buffer(buffer_id)],
            )?;

            if let Some(saved_id) = saved_current {
                buffers.set_current(saved_id);
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
        Value::True | Value::Nil | Value::Buffer(_) | Value::Str(_)
    ) || super::marker::is_marker(&target)
}

pub(crate) fn dispatch_print_callback_chars(
    text: &str,
    mut emit_char: impl FnMut(Value) -> Result<(), Flow>,
) -> Result<(), Flow> {
    for ch in text.chars() {
        emit_char(Value::Int(ch as i64))?;
    }
    Ok(())
}

fn write_print_output(
    eval: &mut super::eval::Evaluator,
    printcharfun: Option<&Value>,
    text: &str,
) -> Result<(), Flow> {
    let target = resolve_print_target(eval, printcharfun);
    write_print_output_to_target(&mut eval.buffers, target, text)
}

pub(crate) fn write_print_output_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &mut crate::buffer::BufferManager,
    printcharfun: Option<&Value>,
    text: &str,
) -> Result<(), Flow> {
    let target = resolve_print_target_in_state(obarray, dynamic, printcharfun);
    write_print_output_to_target(buffers, target, text)
}

fn write_terpri_output(eval: &mut super::eval::Evaluator, target: Value) -> Result<(), Flow> {
    match target {
        Value::True | Value::Nil => Ok(()),
        Value::Buffer(id) => {
            if eval.buffers.get(id).is_none() {
                return Err(signal(
                    "error",
                    vec![Value::string("Output buffer no longer exists")],
                ));
            }
            let _ = eval.buffers.insert_into_buffer(id, "\n");
            Ok(())
        }
        Value::Str(name_id) => {
            let name = with_heap(|h| h.get_string(name_id).to_owned());
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
            let saved_roots = eval.save_temp_roots();
            eval.push_temp_root(other);
            let result = eval.apply(other, vec![Value::Int('\n' as i64)]);
            eval.restore_temp_roots(saved_roots);
            result?;
            Ok(())
        }
    }
}

pub(super) fn print_value_eval(eval: &super::eval::Evaluator, value: &Value) -> String {
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

    let head = match &items[0] {
        Value::Symbol(id) => resolve_sym(*id),
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
    match value {
        Value::Str(id) => with_heap(|h| h.get_string(*id).to_owned()),
        Value::Symbol(id) => resolve_sym(*id).to_owned(),
        Value::Keyword(id) => resolve_sym(*id).to_owned(),
        Value::Cons(_) => {
            if let Some(shorthand) = print_value_princ_list_shorthand(value, &print_value_princ) {
                return shorthand;
            }
            let mut out = String::from("(");
            let mut cursor = *value;
            let mut first = true;
            loop {
                match cursor {
                    Value::Cons(cell) => {
                        if !first {
                            out.push(' ');
                        }
                        let pair = read_cons(cell);
                        out.push_str(&print_value_princ(&pair.car));
                        cursor = pair.cdr;
                        first = false;
                    }
                    Value::Nil => break,
                    other => {
                        if !first {
                            out.push_str(" . ");
                        }
                        out.push_str(&print_value_princ(&other));
                        break;
                    }
                }
            }
            out.push(')');
            out
        }
        Value::Vector(id) => {
            let items = with_heap(|h| h.get_vector(*id).clone());
            let parts: Vec<String> = items.iter().map(print_value_princ).collect();
            format!("[{}]", parts.join(" "))
        }
        Value::Record(id) => {
            let items = with_heap(|h| h.get_vector(*id).clone());
            let parts: Vec<String> = items.iter().map(print_value_princ).collect();
            format!("#s({})", parts.join(" "))
        }
        other => super::print::print_value(other),
    }
}

pub(crate) fn print_value_princ_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    buffers: &crate::buffer::BufferManager,
    frames: &crate::window::FrameManager,
    threads: &crate::emacs_core::threads::ThreadManager,
    value: &Value,
) -> String {
    if super::terminal::pure::print_terminal_handle(value).is_some()
        || threads.thread_id_from_handle(value).is_some()
        || threads.mutex_id_from_handle(value).is_some()
        || threads.condition_variable_id_from_handle(value).is_some()
    {
        return super::error::print_value_in_state(obarray, buffers, frames, threads, value);
    }
    match value {
        Value::Str(id) => with_heap(|h| h.get_string(*id).to_owned()),
        Value::Symbol(id) => resolve_sym(*id).to_owned(),
        Value::Keyword(id) => resolve_sym(*id).to_owned(),
        Value::Buffer(id) => {
            if let Some(buf) = buffers.get(*id) {
                return buf.name.clone();
            }
            if buffers.dead_buffer_last_name(*id).is_some() {
                return "#<killed buffer>".to_string();
            }
            super::error::print_value_in_state(obarray, buffers, frames, threads, value)
        }
        Value::Cons(_) => {
            if let Some(shorthand) = print_value_princ_list_shorthand(value, &|item| {
                print_value_princ_in_state(obarray, buffers, frames, threads, item)
            }) {
                return shorthand;
            }
            let mut out = String::from("(");
            let mut cursor = *value;
            let mut first = true;
            loop {
                match cursor {
                    Value::Cons(cell) => {
                        if !first {
                            out.push(' ');
                        }
                        let pair = read_cons(cell);
                        out.push_str(&print_value_princ_in_state(
                            obarray, buffers, frames, threads, &pair.car,
                        ));
                        cursor = pair.cdr;
                        first = false;
                    }
                    Value::Nil => break,
                    other => {
                        if !first {
                            out.push_str(" . ");
                        }
                        out.push_str(&print_value_princ_in_state(
                            obarray, buffers, frames, threads, &other,
                        ));
                        break;
                    }
                }
            }
            out.push(')');
            out
        }
        Value::Vector(id) => {
            let items = with_heap(|h| h.get_vector(*id).clone());
            let parts: Vec<String> = items
                .iter()
                .map(|item| print_value_princ_in_state(obarray, buffers, frames, threads, item))
                .collect();
            format!("[{}]", parts.join(" "))
        }
        Value::Record(id) => {
            let items = with_heap(|h| h.get_vector(*id).clone());
            let parts: Vec<String> = items
                .iter()
                .map(|item| print_value_princ_in_state(obarray, buffers, frames, threads, item))
                .collect();
            format!("#s({})", parts.join(" "))
        }
        other => super::error::print_value_in_state(obarray, buffers, frames, threads, other),
    }
}

pub(super) fn print_value_princ_eval(eval: &super::eval::Evaluator, value: &Value) -> String {
    print_value_princ_in_state(
        &eval.obarray,
        &eval.buffers,
        &eval.frames,
        &eval.threads,
        value,
    )
}

fn prin1_to_string_value(value: &Value, noescape: bool) -> String {
    if noescape {
        match value {
            Value::Str(id) => with_heap(|h| h.get_string(*id).to_owned()),
            other => super::print::print_value(other),
        }
    } else {
        bytes_to_storage_string(&super::print::print_value_bytes(value))
    }
}

fn prin1_to_string_value_eval(
    eval: &super::eval::Evaluator,
    value: &Value,
    noescape: bool,
) -> String {
    prin1_to_string_value_in_state(
        &eval.obarray,
        &eval.buffers,
        &eval.frames,
        &eval.threads,
        value,
        noescape,
    )
}

pub(crate) fn prin1_to_string_value_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    buffers: &crate::buffer::BufferManager,
    frames: &crate::window::FrameManager,
    threads: &crate::emacs_core::threads::ThreadManager,
    value: &Value,
    noescape: bool,
) -> String {
    if noescape {
        match value {
            Value::Str(id) => with_heap(|h| h.get_string(*id).to_owned()),
            other => super::error::print_value_in_state(obarray, buffers, frames, threads, other),
        }
    } else {
        bytes_to_storage_string(&super::error::print_value_bytes_in_state(
            obarray, buffers, frames, threads, value,
        ))
    }
}

pub(crate) fn builtin_princ(args: Vec<Value>) -> EvalResult {
    expect_min_args("princ", &args, 1)?;
    // In real Emacs this prints to standard output; here just return the value
    Ok(args[0])
}

pub(crate) fn builtin_prin1(args: Vec<Value>) -> EvalResult {
    expect_min_args("prin1", &args, 1)?;
    Ok(args[0])
}

pub(crate) fn builtin_princ_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("princ", &args, 1)?;
    let target = resolve_print_target(eval, args.get(1));
    if print_target_is_direct(target) {
        return builtin_princ_in_state(
            &eval.obarray,
            &eval.dynamic,
            &mut eval.buffers,
            &eval.frames,
            &eval.threads,
            args,
        );
    }

    let text = print_value_princ_in_state(
        &eval.obarray,
        &eval.buffers,
        &eval.frames,
        &eval.threads,
        &args[0],
    );
    let saved_roots = eval.save_temp_roots();
    eval.push_temp_root(target);
    let callback_result =
        dispatch_print_callback_chars(&text, |ch| eval.apply(target, vec![ch]).map(|_| ()));
    eval.restore_temp_roots(saved_roots);
    callback_result?;
    Ok(args[0])
}

pub(crate) fn builtin_princ_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &mut crate::buffer::BufferManager,
    frames: &crate::window::FrameManager,
    threads: &crate::emacs_core::threads::ThreadManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("princ", &args, 1)?;
    let text = print_value_princ_in_state(obarray, buffers, frames, threads, &args[0]);
    write_print_output_in_state(obarray, dynamic, buffers, args.get(1), &text)?;
    Ok(args[0])
}

pub(crate) fn builtin_prin1_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("prin1", &args, 1)?;
    let target = resolve_print_target(eval, args.get(1));
    if print_target_is_direct(target) {
        return builtin_prin1_in_state(
            &eval.obarray,
            &eval.dynamic,
            &mut eval.buffers,
            &eval.frames,
            &eval.threads,
            args,
        );
    }

    let text = super::error::print_value_in_state(
        &eval.obarray,
        &eval.buffers,
        &eval.frames,
        &eval.threads,
        &args[0],
    );
    let saved_roots = eval.save_temp_roots();
    eval.push_temp_root(target);
    let callback_result =
        dispatch_print_callback_chars(&text, |ch| eval.apply(target, vec![ch]).map(|_| ()));
    eval.restore_temp_roots(saved_roots);
    callback_result?;
    Ok(args[0])
}

pub(crate) fn builtin_prin1_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &mut crate::buffer::BufferManager,
    frames: &crate::window::FrameManager,
    threads: &crate::emacs_core::threads::ThreadManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("prin1", &args, 1)?;
    let text = super::error::print_value_in_state(obarray, buffers, frames, threads, &args[0]);
    write_print_output_in_state(obarray, dynamic, buffers, args.get(1), &text)?;
    Ok(args[0])
}

pub(crate) fn builtin_prin1_to_string(args: Vec<Value>) -> EvalResult {
    expect_min_args("prin1-to-string", &args, 1)?;
    let noescape = args.get(1).is_some_and(|v| v.is_truthy());
    Ok(Value::string(prin1_to_string_value(&args[0], noescape)))
}

pub(crate) fn builtin_prin1_to_string_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_prin1_to_string_in_state(
        &eval.obarray,
        &eval.buffers,
        &eval.frames,
        &eval.threads,
        args,
    )
}

pub(crate) fn builtin_prin1_to_string_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    buffers: &crate::buffer::BufferManager,
    frames: &crate::window::FrameManager,
    threads: &crate::emacs_core::threads::ThreadManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("prin1-to-string", &args, 1)?;
    let noescape = args.get(1).is_some_and(|v| v.is_truthy());
    Ok(Value::string(prin1_to_string_value_in_state(
        obarray, buffers, frames, threads, &args[0], noescape,
    )))
}

pub(crate) fn builtin_print(args: Vec<Value>) -> EvalResult {
    expect_min_args("print", &args, 1)?;
    Ok(args[0])
}

pub(crate) fn builtin_terpri(args: Vec<Value>) -> EvalResult {
    expect_max_args("terpri", &args, 2)?;
    Ok(Value::True)
}

pub(crate) fn builtin_print_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("print", &args, 1)?;
    let target = resolve_print_target(eval, args.get(1));
    if print_target_is_direct(target) {
        return builtin_print_in_state(
            &eval.obarray,
            &eval.dynamic,
            &mut eval.buffers,
            &eval.frames,
            &eval.threads,
            args,
        );
    }

    let mut text = String::new();
    text.push('\n');
    text.push_str(&super::error::print_value_in_state(
        &eval.obarray,
        &eval.buffers,
        &eval.frames,
        &eval.threads,
        &args[0],
    ));
    text.push('\n');
    let saved_roots = eval.save_temp_roots();
    eval.push_temp_root(target);
    let callback_result =
        dispatch_print_callback_chars(&text, |ch| eval.apply(target, vec![ch]).map(|_| ()));
    eval.restore_temp_roots(saved_roots);
    callback_result?;
    Ok(args[0])
}

pub(crate) fn builtin_print_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &mut crate::buffer::BufferManager,
    frames: &crate::window::FrameManager,
    threads: &crate::emacs_core::threads::ThreadManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("print", &args, 1)?;
    let mut text = String::new();
    text.push('\n');
    text.push_str(&super::error::print_value_in_state(
        obarray, buffers, frames, threads, &args[0],
    ));
    text.push('\n');
    write_print_output_in_state(obarray, dynamic, buffers, args.get(1), &text)?;
    Ok(args[0])
}

pub(crate) fn builtin_terpri_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if let Some(result) = builtin_terpri_in_state(
        &eval.obarray,
        &eval.dynamic,
        &mut eval.buffers,
        args.clone(),
    )? {
        return Ok(result);
    }
    finish_terpri_in_eval(eval, &args)
}

pub(crate) fn builtin_terpri_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> Result<Option<Value>, Flow> {
    expect_max_args("terpri", &args, 2)?;
    let target = resolve_print_target_in_state(obarray, dynamic, args.first());
    if print_target_is_direct(target) {
        write_print_output_to_target(buffers, target, "\n")?;
        return Ok(Some(Value::True));
    }
    Ok(None)
}

pub(crate) fn finish_terpri_in_eval(
    eval: &mut super::eval::Evaluator,
    args: &[Value],
) -> EvalResult {
    expect_max_args("terpri", args, 2)?;
    let target = resolve_print_target(eval, args.first());
    write_terpri_output(eval, target)?;
    Ok(Value::True)
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

pub(crate) fn builtin_write_char(args: Vec<Value>) -> EvalResult {
    expect_range_args("write-char", &args, 1, 2)?;
    let char_code = expect_fixnum(&args[0])?;
    Ok(Value::Int(char_code))
}

pub(crate) fn builtin_write_char_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if let Some(result) = builtin_write_char_in_state(
        &eval.obarray,
        &eval.dynamic,
        &mut eval.buffers,
        args.clone(),
    )? {
        return Ok(result);
    }
    finish_write_char_in_eval(eval, &args)
}

pub(crate) fn finish_write_char_in_eval(
    eval: &mut super::eval::Evaluator,
    args: &[Value],
) -> EvalResult {
    expect_range_args("write-char", args, 1, 2)?;
    let char_code = expect_fixnum(&args[0])?;
    let target = resolve_print_target(eval, args.get(1));

    match target {
        Value::True | Value::Nil => {}
        Value::Buffer(id) => {
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
        Value::Str(name_id) => {
            if let Some(text) = write_char_rendered_text(char_code) {
                let name = with_heap(|h| h.get_string(name_id).to_owned());
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
            let saved_roots = eval.save_temp_roots();
            eval.push_temp_root(other);
            let result = eval.apply(other, vec![Value::Int(char_code)]);
            eval.restore_temp_roots(saved_roots);
            result?;
        }
    }

    Ok(Value::Int(char_code))
}

pub(crate) fn builtin_write_char_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> Result<Option<Value>, Flow> {
    expect_range_args("write-char", &args, 1, 2)?;
    let char_code = expect_fixnum(&args[0])?;
    let target = resolve_print_target_in_state(obarray, dynamic, args.get(1));

    if print_target_is_direct(target) {
        if let Some(text) = write_char_rendered_text(char_code) {
            write_print_output_to_target(buffers, target, &text)?;
        }
        return Ok(Some(Value::Int(char_code)));
    }

    Ok(None)
}

pub(crate) fn builtin_propertize(args: Vec<Value>) -> EvalResult {
    expect_min_args("propertize", &args, 1)?;

    let s = match &args[0] {
        Value::Str(id) => with_heap(|h| h.get_string(*id).to_owned()),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };

    // `propertize` requires an odd argument count: 1 string + plist pairs.
    if args.len().is_multiple_of(2) {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("propertize"), Value::Int(args.len() as i64)],
        ));
    }

    // Create a copy of the string
    let new_str = Value::string(&s);
    let new_id = match &new_str {
        Value::Str(id) => *id,
        _ => unreachable!(),
    };

    // Copy existing text properties from source string
    if let Value::Str(src_id) = &args[0] {
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
                    Value::Keyword(id) => Some(resolve_sym(*id).to_owned()),
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

pub(crate) fn builtin_string_to_syntax(args: Vec<Value>) -> EvalResult {
    super::syntax::builtin_string_to_syntax(args)
}

pub(crate) fn builtin_current_cpu_time(args: Vec<Value>) -> EvalResult {
    expect_args("current-cpu-time", &args, 0)?;
    use std::sync::OnceLock;
    use std::time::Instant;
    static CPU_TIME_START: OnceLock<Instant> = OnceLock::new();
    let start = CPU_TIME_START.get_or_init(Instant::now);
    let ticks = start.elapsed().as_micros() as i64;
    Ok(Value::cons(Value::Int(ticks), Value::Int(1_000_000)))
}

pub(crate) fn builtin_current_idle_time(args: Vec<Value>) -> EvalResult {
    expect_args("current-idle-time", &args, 0)?;
    // Batch mode does not track UI idle duration; Oracle returns nil here.
    Ok(Value::Nil)
}
