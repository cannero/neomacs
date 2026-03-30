use super::*;

// ===========================================================================
// Type predicates
// ===========================================================================

pub(crate) fn builtin_null(args: Vec<Value>) -> EvalResult {
    expect_args("null", &args, 1)?;
    Ok(Value::bool(args[0].is_nil()))
}

pub(crate) fn builtin_atom(args: Vec<Value>) -> EvalResult {
    expect_args("atom", &args, 1)?;
    Ok(Value::bool(!args[0].is_cons()))
}

pub(crate) fn builtin_consp(args: Vec<Value>) -> EvalResult {
    expect_args("consp", &args, 1)?;
    Ok(Value::bool(args[0].is_cons()))
}

pub(crate) fn builtin_listp(args: Vec<Value>) -> EvalResult {
    expect_args("listp", &args, 1)?;
    Ok(Value::bool(args[0].is_list()))
}

pub(crate) fn builtin_list_of_strings_p(args: Vec<Value>) -> EvalResult {
    expect_args("list-of-strings-p", &args, 1)?;
    let mut seen = HashSet::new();
    let mut cursor = args[0];
    loop {
        match cursor {
            Value::Nil => return Ok(Value::True),
            Value::Cons(cell) => {
                let ptr = cell.index as usize;
                if !seen.insert(ptr) {
                    return Ok(Value::Nil);
                }
                let pair = read_cons(cell);
                if !pair.car.is_string() {
                    return Ok(Value::Nil);
                }
                cursor = pair.cdr;
            }
            _ => return Ok(Value::Nil),
        }
    }
}

pub(crate) fn builtin_nlistp(args: Vec<Value>) -> EvalResult {
    expect_args("nlistp", &args, 1)?;
    Ok(Value::bool(!args[0].is_list()))
}

pub(crate) fn builtin_symbolp(args: Vec<Value>) -> EvalResult {
    expect_args("symbolp", &args, 1)?;
    Ok(Value::bool(args[0].is_symbol()))
}

pub(crate) fn builtin_booleanp(args: Vec<Value>) -> EvalResult {
    expect_args("booleanp", &args, 1)?;
    Ok(Value::bool(matches!(args[0], Value::Nil | Value::True)))
}

pub(crate) fn builtin_numberp(args: Vec<Value>) -> EvalResult {
    expect_args("numberp", &args, 1)?;
    Ok(Value::bool(args[0].is_number()))
}

pub(crate) fn builtin_integerp(args: Vec<Value>) -> EvalResult {
    expect_args("integerp", &args, 1)?;
    Ok(Value::bool(args[0].is_integer()))
}

pub(crate) fn builtin_integer_or_null_p(args: Vec<Value>) -> EvalResult {
    expect_args("integer-or-null-p", &args, 1)?;
    Ok(Value::bool(args[0].is_nil() || args[0].is_integer()))
}

pub(crate) fn builtin_string_or_null_p(args: Vec<Value>) -> EvalResult {
    expect_args("string-or-null-p", &args, 1)?;
    Ok(Value::bool(args[0].is_nil() || args[0].is_string()))
}

pub(crate) fn builtin_integer_or_marker_p(args: Vec<Value>) -> EvalResult {
    expect_args("integer-or-marker-p", &args, 1)?;
    let is_integer_or_marker =
        matches!(args[0], Value::Int(_) | Value::Char(_)) || super::marker::is_marker(&args[0]);
    Ok(Value::bool(is_integer_or_marker))
}

pub(crate) fn builtin_number_or_marker_p(args: Vec<Value>) -> EvalResult {
    expect_args("number-or-marker-p", &args, 1)?;
    let is_number_or_marker =
        matches!(args[0], Value::Int(_) | Value::Float(_, _) | Value::Char(_))
            || super::marker::is_marker(&args[0]);
    Ok(Value::bool(is_number_or_marker))
}

pub(crate) fn builtin_floatp(args: Vec<Value>) -> EvalResult {
    expect_args("floatp", &args, 1)?;
    Ok(Value::bool(args[0].is_float()))
}

pub(crate) fn builtin_stringp(args: Vec<Value>) -> EvalResult {
    expect_args("stringp", &args, 1)?;
    Ok(Value::bool(args[0].is_string()))
}

pub(crate) fn builtin_vectorp(args: Vec<Value>) -> EvalResult {
    expect_args("vectorp", &args, 1)?;
    // GNU: vectorp returns nil for char-tables and bool-vectors
    let is_vec = args[0].is_vector()
        && !super::chartable::is_char_table(&args[0])
        && !super::chartable::is_bool_vector(&args[0]);
    Ok(Value::bool(is_vec))
}

pub(crate) fn builtin_vector_or_char_table_p(args: Vec<Value>) -> EvalResult {
    expect_args("vector-or-char-table-p", &args, 1)?;
    Ok(Value::bool(
        args[0].is_vector() || super::chartable::is_char_table(&args[0]),
    ))
}

pub(crate) fn builtin_characterp(args: Vec<Value>) -> EvalResult {
    expect_args("characterp", &args, 1)?;
    // Official Emacs: characterp accepts both Char values and integers
    // in the valid Unicode range (0..MAX_CHAR).
    let is_char = match &args[0] {
        Value::Char(_) => true,
        Value::Int(n) => *n >= 0 && *n <= 0x3F_FFFF, // MAX_CHAR in Emacs
        _ => false,
    };
    Ok(Value::bool(is_char))
}

pub(crate) fn builtin_char_uppercase_p(args: Vec<Value>) -> EvalResult {
    expect_args("char-uppercase-p", &args, 1)?;
    let code = expect_character_code(&args[0])?;
    Ok(Value::bool(downcase_char_code_emacs_compat(code) != code))
}

pub(super) fn is_lambda_form_list(value: &Value) -> bool {
    match value {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            let name = pair.car.as_symbol_name();
            name == Some("lambda") || name == Some("closure")
        }
        _ => false,
    }
}

fn is_macro_marker_list(value: &Value) -> bool {
    match value {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            pair.car.as_symbol_name() == Some("macro")
        }
        _ => false,
    }
}

fn is_runtime_function_object(value: &Value) -> bool {
    match value {
        Value::Lambda(_) | Value::ByteCode(_) => true,
        Value::Subr(id) => !super::subr_info::is_special_form(resolve_sym(*id)),
        _ => false,
    }
}

fn autoload_type_of(value: &Value) -> Option<super::autoload::AutoloadType> {
    if !super::autoload::is_autoload_value(value) {
        return None;
    }
    let items = list_to_vec(value)?;
    let type_value = items.get(4).cloned().unwrap_or(Value::Nil);
    Some(super::autoload::AutoloadType::from_value(&type_value))
}

pub(crate) fn builtin_functionp(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("functionp", &args, 1)?;
    let is_function = if let Some(symbol) = match &args[0] {
        Value::Nil => Some(intern("nil")),
        Value::True => Some(intern("t")),
        Value::Symbol(id) | Value::Keyword(id) => Some(*id),
        _ => None,
    } {
        if let Some(function) =
            resolve_indirect_symbol_by_id_in_obarray(&eval.obarray, symbol).map(|(_, value)| value)
        {
            if let Some(autoload_type) = autoload_type_of(&function) {
                matches!(autoload_type, super::autoload::AutoloadType::Function)
            } else {
                is_runtime_function_object(&function) || is_lambda_form_list(&function)
            }
        } else {
            false
        }
    } else {
        match &args[0] {
            Value::Lambda(_) | Value::Subr(_) | Value::ByteCode(_) => {
                is_runtime_function_object(&args[0])
            }
            Value::Cons(_) => !is_macro_marker_list(&args[0]) && is_lambda_form_list(&args[0]),
            _ => false,
        }
    };
    Ok(Value::bool(is_function))
}

pub(crate) fn builtin_keywordp(args: Vec<Value>) -> EvalResult {
    expect_args("keywordp", &args, 1)?;
    Ok(Value::bool(args[0].is_keyword()))
}

pub(crate) fn builtin_hash_table_p(args: Vec<Value>) -> EvalResult {
    expect_args("hash-table-p", &args, 1)?;
    Ok(Value::bool(args[0].is_hash_table()))
}

pub(crate) fn builtin_type_of(args: Vec<Value>) -> EvalResult {
    expect_args("type-of", &args, 1)?;
    // GNU Emacs `type-of` handles symbol, integer, subr directly,
    // then delegates to `cl-type-of` for everything else.
    match &args[0] {
        Value::Nil | Value::True | Value::Symbol(_) | Value::Keyword(_) => {
            Ok(Value::symbol("symbol"))
        }
        Value::Int(_) | Value::Char(_) => Ok(Value::symbol("integer")),
        Value::Subr(_) => Ok(Value::symbol("subr")),
        _ => builtin_cl_type_of(args),
    }
}

/// Context-aware type-of that dumps Lisp backtrace on stale ObjId.
pub(crate) fn builtin_type_of_with_ctx(
    ctx: &mut super::super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    // Check for stale BEFORE dispatching
    let stale_id = match &args[0] {
        Value::Vector(id)
        | Value::Record(id)
        | Value::Cons(id)
        | Value::HashTable(id)
        | Value::Str(id)
        | Value::Lambda(id)
        | Value::Macro(id)
        | Value::ByteCode(id)
        | Value::Overlay(id)
        | Value::Marker(id) => {
            let is_stale = crate::emacs_core::value::with_heap(|h| {
                let i = id.index as usize;
                i < h.generations().len() && h.generations()[i] != id.generation
            });
            if is_stale { Some(*id) } else { None }
        }
        _ => None,
    };
    if let Some(id) = stale_id {
        let variant = match &args[0] {
            Value::Record(_) => "record",
            Value::Vector(_) => "vector",
            Value::Cons(_) => "cons",
            _ => "heap-obj",
        };
        eprintln!("STALE-DETECT type-of: {} {:?}", variant, id);
        eprintln!("  Lisp backtrace ({} frames):", ctx.runtime_backtrace.len());
        for (i, frame) in ctx.runtime_backtrace.iter().rev().take(10).enumerate() {
            eprintln!("    #{}: {}", i, frame.function);
        }
    }
    builtin_type_of(args)
}

pub(crate) fn builtin_cl_type_of(args: Vec<Value>) -> EvalResult {
    expect_args("cl-type-of", &args, 1)?;
    // Debug: detect stale ObjId BEFORE accessing heap — return safe value
    let stale = match &args[0] {
        Value::Vector(id)
        | Value::Record(id)
        | Value::Cons(id)
        | Value::HashTable(id)
        | Value::Str(id)
        | Value::Lambda(id)
        | Value::Macro(id)
        | Value::ByteCode(id)
        | Value::Overlay(id)
        | Value::Marker(id) => crate::emacs_core::value::with_heap(|h| {
            let i = id.index as usize;
            i < h.generations().len() && h.generations()[i] != id.generation
        }),
        _ => false,
    };
    if stale {
        let variant = match &args[0] {
            Value::Vector(_) => "vector",
            Value::Record(_) => "record",
            Value::Cons(_) => "cons",
            Value::HashTable(_) => "hash-table",
            Value::Str(_) => "string",
            Value::Lambda(_) => "interpreted-function",
            Value::Macro(_) => "macro",
            Value::ByteCode(_) => "byte-code-function",
            _ => "unknown",
        };
        eprintln!(
            "STALE-DETECT: cl-type-of got stale {} — returning symbol instead of crashing",
            variant
        );
        return Ok(Value::symbol(variant));
    }
    // Records: return the type tag (slot 0).
    // GNU data.c:269-277: if slot 0 is itself a record with len > 1,
    // return slot 1 of that inner record (the class name symbol).
    // This is how EIEIO objects work: slot 0 is the eieio--class
    // record, and slot 1 of that record is the class name.
    if let Value::Record(id) = &args[0] {
        let tag = with_heap(|h| h.get_vector(*id).first().copied());
        if let Some(Value::Record(tag_id)) = tag {
            let tag_vec = with_heap(|h| h.get_vector(tag_id).clone());
            if tag_vec.len() > 1 {
                return Ok(tag_vec[1]);
            }
        }
        return Ok(tag.unwrap_or_else(|| Value::symbol("record")));
    }
    // Char-tables and bool-vectors are tagged vectors
    if chartable::is_char_table(&args[0]) {
        return Ok(Value::symbol("char-table"));
    }
    if chartable::is_bool_vector(&args[0]) {
        return Ok(Value::symbol("bool-vector"));
    }
    let name = match &args[0] {
        Value::Nil => "null",
        Value::True => "boolean",
        Value::Int(_) | Value::Char(_) => "fixnum",
        Value::Float(_, _) => "float",
        Value::Str(_) => "string",
        Value::Symbol(_) | Value::Keyword(_) => "symbol",
        Value::Cons(_) => "cons",
        Value::Vector(_) => "vector",
        Value::Record(_) => unreachable!(),
        Value::HashTable(_) => "hash-table",
        Value::Subr(_) => "primitive-function",
        Value::Lambda(_) | Value::Macro(_) => "interpreted-function",
        Value::ByteCode(_) => "byte-code-function",
        Value::Marker(_) => "marker",
        Value::Buffer(_) => "buffer",
        Value::Overlay(_) => "overlay",
        Value::Window(_) => "window",
        Value::Frame(_) => "frame",
        Value::Timer(_) => "timer",
    };
    Ok(Value::symbol(name))
}

pub(crate) fn builtin_sequencep(args: Vec<Value>) -> EvalResult {
    expect_args("sequencep", &args, 1)?;
    // GNU: sequences are lists, vectors, strings, bool-vectors, char-tables.
    // Lambdas and records are NOT sequences.
    let is_seq = args[0].is_list() || args[0].is_vector() || args[0].is_string();
    Ok(Value::bool(is_seq))
}

pub(crate) fn builtin_arrayp(args: Vec<Value>) -> EvalResult {
    expect_args("arrayp", &args, 1)?;
    // GNU: arrays are vectors, strings, char-tables, bool-vectors.
    // Records are NOT arrays.
    let is_arr = args[0].is_vector() || args[0].is_string();
    Ok(Value::bool(is_arr))
}

// ===========================================================================
// Equality
// ===========================================================================

pub(crate) fn builtin_eq(args: Vec<Value>) -> EvalResult {
    expect_args("eq", &args, 2)?;
    Ok(Value::bool(eq_value(&args[0], &args[1])))
}

pub(crate) fn builtin_eql(args: Vec<Value>) -> EvalResult {
    expect_args("eql", &args, 2)?;
    Ok(Value::bool(eql_value(&args[0], &args[1])))
}

pub(crate) fn builtin_equal(args: Vec<Value>) -> EvalResult {
    expect_args("equal", &args, 2)?;
    Ok(Value::bool(equal_value(&args[0], &args[1], 0)))
}

pub(crate) fn builtin_function_equal(args: Vec<Value>) -> EvalResult {
    expect_args("function-equal", &args, 2)?;
    Ok(Value::bool(eq_value(&args[0], &args[1])))
}

pub(crate) fn builtin_module_function_p(args: Vec<Value>) -> EvalResult {
    expect_args("module-function-p", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_user_ptrp(args: Vec<Value>) -> EvalResult {
    expect_args("user-ptrp", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_symbol_with_pos_p(args: Vec<Value>) -> EvalResult {
    expect_args("symbol-with-pos-p", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_symbol_with_pos_pos(args: Vec<Value>) -> EvalResult {
    expect_args("symbol-with-pos-pos", &args, 1)?;
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("symbol-with-pos-p"), args[0]],
    ))
}

pub(crate) fn builtin_char_equal(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("char-equal", &args, 2)?;
    let left = expect_char_equal_code(&args[0])?;
    let right = expect_char_equal_code(&args[1])?;
    let case_fold = super::misc_eval::dynamic_or_global_symbol_value_in_state(
        &eval.obarray,
        &[],
        "case-fold-search",
    )
    .map(|v| !v.is_nil())
    .unwrap_or(true);
    if !case_fold {
        return Ok(Value::bool(left == right));
    }
    match (char_equal_folded(left), char_equal_folded(right)) {
        (Some(a), Some(b)) => Ok(Value::bool(a == b)),
        _ => Ok(Value::bool(left == right)),
    }
}

pub(crate) fn builtin_not(args: Vec<Value>) -> EvalResult {
    expect_args("not", &args, 1)?;
    Ok(Value::bool(args[0].is_nil()))
}
