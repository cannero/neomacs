use super::*;
use crate::emacs_core::value::{ValueKind, VecLikeType};

// ===========================================================================
// Type predicates
// ===========================================================================

pub(crate) fn builtin_null(args: Vec<Value>) -> EvalResult {
    expect_args("null", &args, 1)?;
    Ok(Value::bool_val(args[0].is_nil()))
}

pub(crate) fn builtin_atom(args: Vec<Value>) -> EvalResult {
    expect_args("atom", &args, 1)?;
    Ok(Value::bool_val(!args[0].is_cons()))
}

pub(crate) fn builtin_consp(args: Vec<Value>) -> EvalResult {
    expect_args("consp", &args, 1)?;
    Ok(Value::bool_val(args[0].is_cons()))
}

pub(crate) fn builtin_listp(args: Vec<Value>) -> EvalResult {
    expect_args("listp", &args, 1)?;
    Ok(Value::bool_val(args[0].is_list()))
}

pub(crate) fn builtin_list_of_strings_p(args: Vec<Value>) -> EvalResult {
    expect_args("list-of-strings-p", &args, 1)?;
    let mut seen = HashSet::new();
    let mut cursor = args[0];
    loop {
        match cursor.kind() {
            ValueKind::Nil => return Ok(Value::T),
            ValueKind::Cons => {
                let ptr = cursor.bits();
                if !seen.insert(ptr) {
                    return Ok(Value::NIL);
                }
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                if !pair_car.is_string() {
                    return Ok(Value::NIL);
                }
                cursor = pair_cdr;
            }
            _ => return Ok(Value::NIL),
        }
    }
}

pub(crate) fn builtin_nlistp(args: Vec<Value>) -> EvalResult {
    expect_args("nlistp", &args, 1)?;
    Ok(Value::bool_val(!args[0].is_list()))
}

pub(crate) fn builtin_symbolp(args: Vec<Value>) -> EvalResult {
    expect_args("symbolp", &args, 1)?;
    Ok(Value::bool_val(args[0].is_symbol()))
}

pub(crate) fn builtin_booleanp(args: Vec<Value>) -> EvalResult {
    expect_args("booleanp", &args, 1)?;
    Ok(Value::bool_val(args[0].is_nil() || args[0].is_t()))
}

pub(crate) fn builtin_numberp(args: Vec<Value>) -> EvalResult {
    expect_args("numberp", &args, 1)?;
    Ok(Value::bool_val(args[0].is_number()))
}

pub(crate) fn builtin_integerp(args: Vec<Value>) -> EvalResult {
    expect_args("integerp", &args, 1)?;
    Ok(Value::bool_val(args[0].is_integer()))
}

pub(crate) fn builtin_integer_or_null_p(args: Vec<Value>) -> EvalResult {
    expect_args("integer-or-null-p", &args, 1)?;
    Ok(Value::bool_val(args[0].is_nil() || args[0].is_integer()))
}

pub(crate) fn builtin_string_or_null_p(args: Vec<Value>) -> EvalResult {
    expect_args("string-or-null-p", &args, 1)?;
    Ok(Value::bool_val(args[0].is_nil() || args[0].is_string()))
}

pub(crate) fn builtin_integer_or_marker_p(args: Vec<Value>) -> EvalResult {
    expect_args("integer-or-marker-p", &args, 1)?;
    let is_integer_or_marker =
        args[0].is_fixnum() || args[0].is_char() || super::marker::is_marker(&args[0]);
    Ok(Value::bool_val(is_integer_or_marker))
}

pub(crate) fn builtin_number_or_marker_p(args: Vec<Value>) -> EvalResult {
    expect_args("number-or-marker-p", &args, 1)?;
    let is_number_or_marker =
        (args[0].is_fixnum() || args[0].is_float() || args[0].as_char().is_some())
            || super::marker::is_marker(&args[0]);
    Ok(Value::bool_val(is_number_or_marker))
}

pub(crate) fn builtin_floatp(args: Vec<Value>) -> EvalResult {
    expect_args("floatp", &args, 1)?;
    Ok(Value::bool_val(args[0].is_float()))
}

pub(crate) fn builtin_stringp(args: Vec<Value>) -> EvalResult {
    expect_args("stringp", &args, 1)?;
    Ok(Value::bool_val(args[0].is_string()))
}

pub(crate) fn builtin_vectorp(args: Vec<Value>) -> EvalResult {
    expect_args("vectorp", &args, 1)?;
    // GNU: vectorp returns nil for char-tables and bool-vectors
    let is_vec = args[0].is_vector()
        && !super::chartable::is_char_table(&args[0])
        && !super::chartable::is_bool_vector(&args[0]);
    Ok(Value::bool_val(is_vec))
}

pub(crate) fn builtin_vector_or_char_table_p(args: Vec<Value>) -> EvalResult {
    expect_args("vector-or-char-table-p", &args, 1)?;
    Ok(Value::bool_val(
        args[0].is_vector() || super::chartable::is_char_table(&args[0]),
    ))
}

pub(crate) fn builtin_characterp(args: Vec<Value>) -> EvalResult {
    expect_args("characterp", &args, 1)?;
    // Official Emacs: characterp accepts both Char values and integers
    // in the valid Unicode range (0..MAX_CHAR).
    let is_char = match args[0].kind() {
        ValueKind::Fixnum(n) => n >= 0 && n <= 0x3F_FFFF, // MAX_CHAR in Emacs
        _ => false,
    };
    Ok(Value::bool_val(is_char))
}

pub(crate) fn builtin_char_uppercase_p(args: Vec<Value>) -> EvalResult {
    expect_args("char-uppercase-p", &args, 1)?;
    let code = expect_character_code(&args[0])?;
    Ok(Value::bool_val(
        downcase_char_code_emacs_compat(code) != code,
    ))
}

pub(super) fn is_lambda_form_list(value: &Value) -> bool {
    match value.kind() {
        ValueKind::Cons => {
            let pair_car = value.cons_car();
            let pair_cdr = value.cons_cdr();
            let name = pair_car.as_symbol_name();
            name == Some("lambda") || name == Some("closure")
        }
        _ => false,
    }
}

fn is_macro_marker_list(value: &Value) -> bool {
    match value.kind() {
        ValueKind::Cons => {
            let pair_car = value.cons_car();
            let pair_cdr = value.cons_cdr();
            pair_car.as_symbol_name() == Some("macro")
        }
        _ => false,
    }
}

fn is_runtime_function_object(value: &Value) -> bool {
    match value.kind() {
        ValueKind::Veclike(VecLikeType::Lambda) | ValueKind::Veclike(VecLikeType::ByteCode) => true,
        ValueKind::Veclike(VecLikeType::Subr) => {
            let id = value.as_subr_id().unwrap();
            !super::subr_info::is_special_form(resolve_sym(id))
        }
        _ => false,
    }
}

fn autoload_type_of(value: &Value) -> Option<super::autoload::AutoloadType> {
    if !super::autoload::is_autoload_value(value) {
        return None;
    }
    let items = list_to_vec(value)?;
    let type_value = items.get(4).cloned().unwrap_or(Value::NIL);
    Some(super::autoload::AutoloadType::from_value(&type_value))
}

pub(crate) fn builtin_functionp(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("functionp", &args, 1)?;
    let is_function = if let Some(symbol) = match args[0].kind() {
        ValueKind::Nil => Some(intern("nil")),
        ValueKind::T => Some(intern("t")),
        ValueKind::Symbol(id) => Some(id),
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
        match args[0].kind() {
            ValueKind::Veclike(VecLikeType::Lambda)
            | ValueKind::Veclike(VecLikeType::Subr)
            | ValueKind::Veclike(VecLikeType::ByteCode) => is_runtime_function_object(&args[0]),
            ValueKind::Cons => !is_macro_marker_list(&args[0]) && is_lambda_form_list(&args[0]),
            _ => false,
        }
    };
    Ok(Value::bool_val(is_function))
}

pub(crate) fn builtin_keywordp(args: Vec<Value>) -> EvalResult {
    expect_args("keywordp", &args, 1)?;
    Ok(Value::bool_val(args[0].is_keyword()))
}

pub(crate) fn builtin_hash_table_p(args: Vec<Value>) -> EvalResult {
    expect_args("hash-table-p", &args, 1)?;
    Ok(Value::bool_val(args[0].is_hash_table()))
}

pub(crate) fn builtin_type_of(args: Vec<Value>) -> EvalResult {
    expect_args("type-of", &args, 1)?;
    // GNU Emacs `type-of` handles symbol, integer, subr directly,
    // then delegates to `cl-type-of` for everything else.
    match args[0].kind() {
        ValueKind::Nil | ValueKind::T | ValueKind::Symbol(_) => Ok(Value::symbol("symbol")),
        ValueKind::Fixnum(_) => Ok(Value::symbol("integer")),
        ValueKind::Veclike(VecLikeType::Subr) => Ok(Value::symbol("subr")),
        _ => builtin_cl_type_of(args),
    }
}

/// Context-aware type-of that dumps Lisp backtrace on stale reference.
pub(crate) fn builtin_type_of_with_ctx(
    ctx: &mut super::super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    // Stale tagged pointer detection is not applicable with tagged pointers —
    // the old generation-based check relied on tagged pointer indirection which
    // no longer exists.  Just delegate directly.
    let _ = ctx; // suppress unused warning
    builtin_type_of(args)
}

pub(crate) fn builtin_cl_type_of(args: Vec<Value>) -> EvalResult {
    expect_args("cl-type-of", &args, 1)?;
    // Stale tagged pointer detection is not applicable with tagged pointers.
    // Records: return the type tag (slot 0).
    // GNU data.c:269-277: if slot 0 is itself a record with len > 1,
    // return slot 1 of that inner record (the class name symbol).
    // This is how EIEIO objects work: slot 0 is the eieio--class
    // record, and slot 1 of that record is the class name.
    if args[0].is_record() {
        let tag = args[0].as_record_data().and_then(|v| v.first().copied());
        if let Some(tag_val) = tag {
            if tag_val.is_record() {
                let tag_vec = tag_val.as_record_data();
                if let Some(tv) = tag_vec {
                    if tv.len() > 1 {
                        return Ok(tv[1]);
                    }
                }
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
    let name = match args[0].kind() {
        ValueKind::Nil => "null",
        ValueKind::T => "boolean",
        ValueKind::Fixnum(_) => "fixnum",
        ValueKind::Float => "float",
        ValueKind::String => "string",
        ValueKind::Symbol(_) => "symbol",
        ValueKind::Cons => "cons",
        ValueKind::Veclike(VecLikeType::Vector) => "vector",
        ValueKind::Veclike(VecLikeType::Record) => unreachable!(),
        ValueKind::Veclike(VecLikeType::HashTable) => "hash-table",
        ValueKind::Veclike(VecLikeType::Subr) => "primitive-function",
        ValueKind::Veclike(VecLikeType::Lambda) | ValueKind::Veclike(VecLikeType::Macro) => {
            "interpreted-function"
        }
        ValueKind::Veclike(VecLikeType::ByteCode) => "byte-code-function",
        ValueKind::Veclike(VecLikeType::Marker) => "marker",
        ValueKind::Veclike(VecLikeType::Buffer) => "buffer",
        ValueKind::Veclike(VecLikeType::Overlay) => "overlay",
        ValueKind::Veclike(VecLikeType::Window) => "window",
        ValueKind::Veclike(VecLikeType::Frame) => "frame",
        ValueKind::Veclike(VecLikeType::Timer) => "timer",
        ValueKind::Unknown => "unknown",
    };
    Ok(Value::symbol(name))
}

pub(crate) fn builtin_sequencep(args: Vec<Value>) -> EvalResult {
    expect_args("sequencep", &args, 1)?;
    // GNU: sequences are lists, vectors, strings, bool-vectors, char-tables.
    // Lambdas and records are NOT sequences.
    let is_seq = args[0].is_list() || args[0].is_vector() || args[0].is_string();
    Ok(Value::bool_val(is_seq))
}

pub(crate) fn builtin_arrayp(args: Vec<Value>) -> EvalResult {
    expect_args("arrayp", &args, 1)?;
    // GNU: arrays are vectors, strings, char-tables, bool-vectors.
    // Records are NOT arrays.
    let is_arr = args[0].is_vector() || args[0].is_string();
    Ok(Value::bool_val(is_arr))
}

// ===========================================================================
// Equality
// ===========================================================================

pub(crate) fn builtin_eq(args: Vec<Value>) -> EvalResult {
    expect_args("eq", &args, 2)?;
    Ok(Value::bool_val(eq_value(&args[0], &args[1])))
}

pub(crate) fn builtin_eql(args: Vec<Value>) -> EvalResult {
    expect_args("eql", &args, 2)?;
    Ok(Value::bool_val(eql_value(&args[0], &args[1])))
}

pub(crate) fn builtin_equal(args: Vec<Value>) -> EvalResult {
    expect_args("equal", &args, 2)?;
    Ok(Value::bool_val(equal_value(&args[0], &args[1], 0)))
}

pub(crate) fn builtin_function_equal(args: Vec<Value>) -> EvalResult {
    expect_args("function-equal", &args, 2)?;
    Ok(Value::bool_val(eq_value(&args[0], &args[1])))
}

pub(crate) fn builtin_module_function_p(args: Vec<Value>) -> EvalResult {
    expect_args("module-function-p", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_user_ptrp(args: Vec<Value>) -> EvalResult {
    expect_args("user-ptrp", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_symbol_with_pos_p(args: Vec<Value>) -> EvalResult {
    expect_args("symbol-with-pos-p", &args, 1)?;
    Ok(Value::NIL)
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
        return Ok(Value::bool_val(left == right));
    }
    match (char_equal_folded(left), char_equal_folded(right)) {
        (Some(a), Some(b)) => Ok(Value::bool_val(a == b)),
        _ => Ok(Value::bool_val(left == right)),
    }
}

pub(crate) fn builtin_not(args: Vec<Value>) -> EvalResult {
    expect_args("not", &args, 1)?;
    Ok(Value::bool_val(args[0].is_nil()))
}
