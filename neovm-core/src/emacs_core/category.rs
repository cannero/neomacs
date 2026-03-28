//! Character category tables.
//!
//! GNU Emacs stores category semantics on category-table char-tables:
//! - the char-table contents are category-set bool-vectors
//! - extra slot 0 stores the category docstring vector
//! - the current buffer's `category-table` slot selects the active table
//!
//! NeoVM now mirrors that ownership model instead of routing semantics
//! through a parallel Rust-side manager.

use std::cell::RefCell;

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::value::{RuntimeBindingValue, Value, read_cons, with_heap, with_heap_mut};

thread_local! {
    static STANDARD_CATEGORY_TABLE_OBJECT: RefCell<Option<Value>> = const { RefCell::new(None) };
}

pub fn reset_category_thread_locals() {
    STANDARD_CATEGORY_TABLE_OBJECT.with(|slot| *slot.borrow_mut() = None);
}

pub(crate) fn restore_standard_category_table_object(table: Value) {
    STANDARD_CATEGORY_TABLE_OBJECT.with(|slot| *slot.borrow_mut() = Some(table));
}

pub fn collect_category_gc_roots(roots: &mut Vec<Value>) {
    STANDARD_CATEGORY_TABLE_OBJECT.with(|slot| {
        if let Some(v) = *slot.borrow() {
            roots.push(v);
        }
    });
}

const CATEGORY_TABLE_PROPERTY: &str = "category-table";
const CATEGORY_DOCSTRING_SLOT: i64 = 0;
const CATEGORY_VERSION_SLOT: i64 = 1;
const CATEGORY_DOCSTRING_COUNT: usize = 95;
const CATEGORY_MIN: i64 = 0x20;
const CATEGORY_MAX: i64 = 0x7e;

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

fn is_category_letter(ch: char) -> bool {
    (CATEGORY_MIN as u8 as char..=CATEGORY_MAX as u8 as char).contains(&ch)
}

fn extract_char_opt(value: &Value, fn_name: &str) -> Result<Option<char>, Flow> {
    match value {
        Value::Char(c) => Ok(Some(*c)),
        Value::Int(n) => {
            if let Some(c) = char::from_u32(*n as u32) {
                Ok(Some(c))
            } else if (0..=0x3F_FFFF).contains(n) {
                Ok(None)
            } else {
                Err(signal(
                    "error",
                    vec![Value::string(format!(
                        "{}: Invalid character code: {}",
                        fn_name, n
                    ))],
                ))
            }
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *other],
        )),
    }
}

fn extract_char(value: &Value, fn_name: &str) -> Result<char, Flow> {
    extract_char_opt(value, fn_name)?.ok_or_else(|| {
        signal(
            "error",
            vec![Value::string(format!(
                "{}: Invalid character code",
                fn_name
            ))],
        )
    })
}

fn extract_char_code(value: &Value, fn_name: &str) -> Result<i64, Flow> {
    match value {
        Value::Char(c) => Ok(*c as i64),
        Value::Int(n) if (0..=0x3F_FFFF).contains(n) => Ok(*n),
        Value::Int(n) => Err(signal(
            "error",
            vec![Value::string(format!(
                "{}: Invalid character code: {}",
                fn_name, n
            ))],
        )),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *other],
        )),
    }
}

fn make_empty_category_set() -> EvalResult {
    super::chartable::builtin_make_bool_vector(vec![Value::Int(128), Value::Nil])
}

fn clone_vector_value(value: &Value) -> EvalResult {
    match value {
        Value::Vector(v) => Ok(Value::vector(with_heap(|h| h.get_vector(*v).clone()))),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("vectorp"), *other],
        )),
    }
}

fn is_category_table_value(value: &Value) -> Result<bool, Flow> {
    let is_char_table = super::chartable::builtin_char_table_p(vec![*value])?;
    if !is_char_table.is_truthy() {
        return Ok(false);
    }
    let subtype = super::chartable::builtin_char_table_subtype(vec![*value])?;
    Ok(matches!(subtype, Value::Symbol(id) if resolve_sym(id) == "category-table"))
}

fn make_category_table_object() -> EvalResult {
    let default = make_empty_category_set()?;
    let table = super::chartable::make_char_table_with_extra_slots(
        Value::symbol("category-table"),
        default,
        2,
    );
    super::chartable::builtin_set_char_table_extra_slot(vec![
        table,
        Value::Int(CATEGORY_DOCSTRING_SLOT),
        Value::vector(vec![Value::Nil; CATEGORY_DOCSTRING_COUNT]),
    ])?;
    super::chartable::builtin_set_char_table_extra_slot(vec![
        table,
        Value::Int(CATEGORY_VERSION_SLOT),
        Value::Nil,
    ])?;
    Ok(table)
}

pub(crate) fn ensure_standard_category_table_object() -> EvalResult {
    STANDARD_CATEGORY_TABLE_OBJECT.with(|slot| {
        if let Some(table) = slot.borrow().as_ref() {
            return Ok(*table);
        }

        let table = make_category_table_object()?;
        *slot.borrow_mut() = Some(table);
        Ok(table)
    })
}

fn clone_char_table_object(value: &Value) -> EvalResult {
    match value {
        Value::Vector(v) => Ok(Value::vector(with_heap(|h| h.get_vector(*v).clone()))),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("category-table-p"), *other],
        )),
    }
}

fn deep_copy_category_table(source: &Value) -> EvalResult {
    if !is_category_table_value(source)? {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("category-table-p"), *source],
        ));
    }

    let copy = clone_char_table_object(source)?;
    let default = super::chartable::builtin_char_table_range(vec![*source, Value::Nil])?;
    if matches!(default, Value::Vector(_)) {
        super::chartable::builtin_set_char_table_range(vec![
            copy,
            Value::Nil,
            clone_vector_value(&default)?,
        ])?;
    }

    let docstrings = super::chartable::builtin_char_table_extra_slot(vec![
        *source,
        Value::Int(CATEGORY_DOCSTRING_SLOT),
    ])?;
    super::chartable::builtin_set_char_table_extra_slot(vec![
        copy,
        Value::Int(CATEGORY_DOCSTRING_SLOT),
        clone_vector_value(&docstrings)?,
    ])?;

    for (key, value) in super::chartable::char_table_local_entries(source)? {
        let copied = if matches!(value, Value::Vector(_)) {
            clone_vector_value(&value)?
        } else {
            value
        };
        super::chartable::builtin_set_char_table_range(vec![copy, key, copied])?;
    }

    Ok(copy)
}

fn category_doc_index(category: char) -> usize {
    (category as usize) - (CATEGORY_MIN as usize)
}

fn category_docstrings(table: Value) -> Result<Value, Flow> {
    super::chartable::builtin_char_table_extra_slot(vec![
        table,
        Value::Int(CATEGORY_DOCSTRING_SLOT),
    ])
}

fn category_docstring_in_table(table: Value, category: char) -> Result<Value, Flow> {
    let docs = category_docstrings(table)?;
    let Value::Vector(arc) = docs else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("vectorp"), docs],
        ));
    };
    let docs = with_heap(|h| h.get_vector(arc).clone());
    Ok(docs
        .get(category_doc_index(category))
        .copied()
        .unwrap_or(Value::Nil))
}

fn set_category_docstring_in_table(
    table: Value,
    category: char,
    docstring: Value,
) -> Result<(), Flow> {
    let docs = category_docstrings(table)?;
    let Value::Vector(arc) = docs else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("vectorp"), docs],
        ));
    };
    with_heap_mut(|h| {
        let vec = h.get_vector_mut(arc);
        let idx = category_doc_index(category);
        if idx < vec.len() {
            vec[idx] = docstring;
        }
    });
    Ok(())
}

fn current_buffer_category_table_in_buffers(
    buffers: &mut crate::buffer::BufferManager,
) -> Result<Value, Flow> {
    let current_id = buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = buffers
        .get(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    if let Some(RuntimeBindingValue::Bound(table)) =
        buf.get_buffer_local_binding(CATEGORY_TABLE_PROPERTY)
    {
        return Ok(table);
    }

    let fallback = ensure_standard_category_table_object()?;
    let _ = buffers.set_buffer_local_property(current_id, CATEGORY_TABLE_PROPERTY, fallback);
    Ok(fallback)
}

fn check_category_table_in_buffers(
    buffers: &mut crate::buffer::BufferManager,
    table: Option<Value>,
) -> Result<Value, Flow> {
    match table {
        None | Some(Value::Nil) => current_buffer_category_table_in_buffers(buffers),
        Some(table) => {
            if !is_category_table_value(&table)? {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("category-table-p"), table],
                ));
            }
            Ok(table)
        }
    }
}

fn check_category_table(
    eval: &mut super::eval::Context,
    table: Option<Value>,
) -> Result<Value, Flow> {
    check_category_table_in_buffers(&mut eval.buffers, table)
}

fn set_current_buffer_category_table_in_buffers(
    buffers: &mut crate::buffer::BufferManager,
    table: Value,
) -> Result<(), Flow> {
    let current_id = buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = buffers.set_buffer_local_property(current_id, CATEGORY_TABLE_PROPERTY, table);
    Ok(())
}

fn category_set_contains(category_set: &Value, category: char) -> Result<bool, Flow> {
    let Value::Vector(arc) = category_set else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("categorysetp"), *category_set],
        ));
    };
    let vec = with_heap(|h| h.get_vector(*arc).clone());
    let bit_idx = 2 + (category as usize);
    Ok(matches!(vec.get(bit_idx), Some(Value::Int(n)) if *n != 0))
}

fn set_category_set_member(
    category_set: &Value,
    category: char,
    present: bool,
) -> Result<(), Flow> {
    let Value::Vector(arc) = category_set else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("categorysetp"), *category_set],
        ));
    };
    with_heap_mut(|h| {
        let vec = h.get_vector_mut(*arc);
        let bit_idx = 2 + (category as usize);
        if bit_idx < vec.len() {
            vec[bit_idx] = Value::Int(if present { 1 } else { 0 });
        }
    });
    Ok(())
}

pub(crate) fn builtin_category_table_p(args: Vec<Value>) -> EvalResult {
    expect_args("category-table-p", &args, 1)?;
    Ok(Value::bool(is_category_table_value(&args[0])?))
}

pub(crate) fn builtin_make_category_table(args: Vec<Value>) -> EvalResult {
    expect_max_args("make-category-table", &args, 0)?;
    make_category_table_object()
}

pub(crate) fn builtin_copy_category_table(args: Vec<Value>) -> EvalResult {
    expect_max_args("copy-category-table", &args, 1)?;

    let source = match args.first() {
        None | Some(Value::Nil) => ensure_standard_category_table_object()?,
        Some(table) => {
            if !is_category_table_value(table)? {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("category-table-p"), *table],
                ));
            }
            *table
        }
    };

    deep_copy_category_table(&source)
}

pub(crate) fn builtin_make_category_set(args: Vec<Value>) -> EvalResult {
    expect_args("make-category-set", &args, 1)?;

    let categories = match &args[0] {
        Value::Str(id) => with_heap(|h| h.get_string(*id).to_owned()),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };

    let mut bits = vec![Value::Int(0); 128];
    for ch in categories.chars() {
        if is_category_letter(ch) {
            bits[ch as usize] = Value::Int(1);
        }
    }

    let mut vec = Vec::with_capacity(130);
    vec.push(Value::symbol("--bool-vector--"));
    vec.push(Value::Int(128));
    vec.extend(bits);
    Ok(Value::vector(vec))
}

pub(crate) fn builtin_category_set_mnemonics(args: Vec<Value>) -> EvalResult {
    expect_args("category-set-mnemonics", &args, 1)?;

    let Value::Vector(bits_arc) = &args[0] else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("categorysetp"), args[0]],
        ));
    };

    let bits = with_heap(|h| h.get_vector(*bits_arc).clone());
    let valid_shape = bits.len() >= 130
        && matches!(&bits[0], Value::Symbol(id) if resolve_sym(*id) == "--bool-vector--")
        && matches!(&bits[1], Value::Int(128));
    if !valid_shape {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("categorysetp"), args[0]],
        ));
    }

    let mut out = String::new();
    for idx in CATEGORY_MIN as usize..=CATEGORY_MAX as usize {
        let is_set = match bits.get(2 + idx) {
            Some(Value::Nil) => false,
            Some(Value::Int(0)) | None => false,
            _ => true,
        };
        if is_set {
            out.push(idx as u8 as char);
        }
    }

    Ok(Value::string(&out))
}

pub(crate) fn builtin_modify_category_entry(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("modify-category-entry", &args, 2)?;
    expect_max_args("modify-category-entry", &args, 4)?;

    let category = extract_char(&args[1], "modify-category-entry")?;
    if !is_category_letter(category) {
        return Err(signal(
            "error",
            vec![Value::string(format!(
                "Invalid category character '{}': must be 0x20..0x7E",
                category
            ))],
        ));
    }

    let table = check_category_table(eval, args.get(2).copied())?;
    if category_docstring_in_table(table, category)?.is_nil() {
        return Err(signal(
            "error",
            vec![Value::string(format!("Undefined category: {}", category))],
        ));
    }
    let reset = args.get(3).is_some_and(Value::is_truthy);

    let (start, end) = match &args[0] {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            (
                extract_char_code(&pair.car, "modify-category-entry")?,
                extract_char_code(&pair.cdr, "modify-category-entry")?,
            )
        }
        value => {
            let ch = extract_char_code(value, "modify-category-entry")?;
            (ch, ch)
        }
    };

    if start > end {
        return Ok(Value::Nil);
    }

    let mut cursor = start;
    while cursor <= end {
        let (existing, _from, to) = super::chartable::char_table_ref_and_range(&table, cursor)?;
        let has_category = category_set_contains(&existing, category)?;
        if has_category == reset {
            let updated = clone_vector_value(&existing)?;
            set_category_set_member(&updated, category, !reset)?;
            let key = if cursor == to {
                Value::Int(cursor)
            } else {
                Value::cons(Value::Int(cursor), Value::Int(to))
            };
            super::chartable::builtin_set_char_table_range(vec![table, key, updated])?;
        }
        cursor = to.saturating_add(1);
    }

    Ok(Value::Nil)
}

pub(crate) fn builtin_define_category(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("define-category", &args, 2)?;
    expect_max_args("define-category", &args, 3)?;

    let category = extract_char(&args[0], "define-category")?;
    if !is_category_letter(category) {
        return Err(signal(
            "error",
            vec![Value::string(format!(
                "Invalid category character '{}': must be ASCII graphic",
                category
            ))],
        ));
    }
    let docstring = match &args[1] {
        Value::Str(id) => Value::string(with_heap(|h| h.get_string(*id).to_owned())),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };
    let table = check_category_table(eval, args.get(2).copied())?;
    if !category_docstring_in_table(table, category)?.is_nil() {
        return Err(signal(
            "error",
            vec![Value::string(format!(
                "Category ‘{}’ is already defined",
                category
            ))],
        ));
    }

    set_category_docstring_in_table(table, category, docstring)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_category_docstring(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("category-docstring", &args, 1)?;
    expect_max_args("category-docstring", &args, 2)?;

    let category = extract_char(&args[0], "category-docstring")?;
    let table = check_category_table(eval, args.get(1).copied())?;
    category_docstring_in_table(table, category)
}

pub(crate) fn builtin_get_unused_category(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("get-unused-category", &args, 1)?;

    let table = check_category_table(eval, args.first().copied())?;
    for code in CATEGORY_MIN..=CATEGORY_MAX {
        let category = char::from_u32(code as u32).expect("ASCII category code");
        if category_docstring_in_table(table, category)?.is_nil() {
            return Ok(Value::Char(category));
        }
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_char_category_set(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("char-category-set", &args, 1)?;
    let _ = extract_char_code(&args[0], "char-category-set")?;
    let table = current_buffer_category_table_in_buffers(&mut eval.buffers)?;
    super::chartable::builtin_char_table_range(vec![table, args[0]])
}

pub(crate) fn builtin_category_table(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_category_table_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_category_table_in_buffers(
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("category-table", &args, 0)?;
    current_buffer_category_table_in_buffers(buffers)
}

pub(crate) fn builtin_standard_category_table(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("standard-category-table", &args, 0)?;
    ensure_standard_category_table_object()
}

pub(crate) fn builtin_set_category_table(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_set_category_table_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_set_category_table_in_buffers(
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-category-table", &args, 1)?;

    let installed = check_category_table_in_buffers(buffers, args.first().copied())?;
    set_current_buffer_category_table_in_buffers(buffers, installed)?;
    Ok(installed)
}

#[cfg(test)]
#[path = "category_test.rs"]
mod tests;
