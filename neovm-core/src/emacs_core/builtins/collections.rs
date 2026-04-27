use super::*;
use crate::emacs_core::value::{ValueKind, VecLikeType};

// ===========================================================================
// Vector operations
// ===========================================================================

pub(crate) fn builtin_make_vector(args: Vec<Value>) -> EvalResult {
    expect_args("make-vector", &args, 2)?;
    let len = expect_wholenump(&args[0])? as usize;
    Ok(Value::vector(vec![args[1]; len]))
}

pub(crate) fn builtin_vector(args: Vec<Value>) -> EvalResult {
    Ok(Value::vector(args))
}

pub(crate) fn builtin_aref(args: Vec<Value>) -> EvalResult {
    expect_args("aref", &args, 2)?;
    let idx_fixnum = expect_fixnum(&args[1])?;
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::Vector) if super::chartable::is_char_table(&args[0]) => {
            let ch = expect_char_table_index(&args[1])?;
            super::chartable::builtin_char_table_range(vec![args[0], Value::fixnum(ch)])
        }
        ValueKind::Veclike(VecLikeType::Vector) | ValueKind::Veclike(VecLikeType::Record) => {
            let idx = idx_fixnum as usize;
            let items = args[0]
                .as_vector_data()
                .or_else(|| args[0].as_record_data())
                .unwrap();
            let is_bool_vector =
                items.len() >= 2 && items[0].as_symbol_name() == Some("--bool-vector--");
            if is_bool_vector {
                let len = match items.get(1).map(|v| v.kind()) {
                    Some(ValueKind::Fixnum(n)) if n >= 0 => n as usize,
                    _ => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("bool-vector-p"), args[0]],
                        ));
                    }
                };
                if idx >= len {
                    return Err(signal("args-out-of-range", vec![args[0], args[1]]));
                }
                let bit = items
                    .get(idx + 2)
                    .copied()
                    .ok_or_else(|| signal("args-out-of-range", vec![args[0], args[1]]))?;
                let truthy = match bit.kind() {
                    ValueKind::Fixnum(n) => n != 0,
                    ValueKind::Nil => false,
                    _ => bit.is_truthy(),
                };
                return Ok(Value::bool_val(truthy));
            }
            items
                .get(idx)
                .copied()
                .ok_or_else(|| signal("args-out-of-range", vec![args[0], args[1]]))
        }
        ValueKind::String => {
            let idx = idx_fixnum as usize;
            let string = args[0].as_lisp_string().expect("string");
            super::lisp_string_char_at(string, idx)
                .map(|cp| Value::fixnum(cp as i64))
                .ok_or_else(|| signal("args-out-of-range", vec![args[0], args[1]]))
        }
        // In official Emacs, closures support aref for oclosure slot access.
        // The closure vector layout is:
        //   [0]=ARGS  [1]=BODY  [2]=ENV  [3]=nil  [4]=DOCSTRING  [5]=IFORM
        ValueKind::Veclike(VecLikeType::Lambda) => {
            let idx = idx_fixnum as usize;
            let vec = lambda_to_closure_vector(&args[0]);
            vec.get(idx)
                .cloned()
                .ok_or_else(|| signal("args-out-of-range", vec![args[0], args[1]]))
        }
        // ByteCode closures: [0]=ARGLIST [1]=CODE [2]=ENV/CONSTANTS [3]=DEPTH [4]=DOC
        ValueKind::Veclike(VecLikeType::ByteCode) => {
            let idx = idx_fixnum as usize;
            let vec = bytecode_to_closure_vector(&args[0]);
            vec.get(idx)
                .cloned()
                .ok_or_else(|| signal("args-out-of-range", vec![args[0], args[1]]))
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("arrayp"), args[0]],
        )),
    }
}

pub(crate) fn aset_string_replacement(
    array: &Value,
    index: &Value,
    new_element: &Value,
) -> Result<Value, Flow> {
    if !array.is_string() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *array],
        ));
    };

    let idx = expect_fixnum(index)? as usize;
    let multibyte = array.string_is_multibyte();
    let mut codes = super::lisp_string_char_codes(array.as_lisp_string().expect("string"));
    if idx >= codes.len() {
        return Err(signal("args-out-of-range", vec![*array, *index]));
    }

    let replacement_code = insert_char_code_from_value(new_element)? as u32;
    if !multibyte && replacement_code > 0xff {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *new_element],
        ));
    }
    codes[idx] = replacement_code;

    use crate::emacs_core::emacs_char;
    let mut rebuilt = Vec::new();
    for code in codes {
        if multibyte {
            let mut buf = [0u8; emacs_char::MAX_MULTIBYTE_LENGTH];
            let len = emacs_char::char_string(code, &mut buf);
            rebuilt.extend_from_slice(&buf[..len]);
        } else {
            if code > 0xff {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("characterp"), *new_element],
                ));
            }
            rebuilt.push(code as u8);
        }
    }
    // Modify the string in-place on the heap so identity (eq) is preserved.
    let _ = array.with_lisp_string_mut(|s| {
        let data = s.data_mut();
        data.clear();
        data.extend_from_slice(&rebuilt);
        s.recompute_size();
    });
    Ok(*array)
}

pub(crate) fn builtin_aset(args: Vec<Value>) -> EvalResult {
    expect_args("aset", &args, 3)?;
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::Vector) if super::chartable::is_char_table(&args[0]) => {
            let ch = expect_char_table_index(&args[1])?;
            super::chartable::builtin_set_char_table_range(vec![
                args[0],
                Value::fixnum(ch),
                args[2],
            ])
        }
        ValueKind::Veclike(VecLikeType::Vector) | ValueKind::Veclike(VecLikeType::Record) => {
            let idx = expect_fixnum(&args[1])? as usize;
            let items = args[0]
                .as_vector_data()
                .or_else(|| args[0].as_record_data())
                .unwrap();
            let is_bool_vector =
                items.len() >= 2 && items[0].as_symbol_name() == Some("--bool-vector--");
            let bool_len = if is_bool_vector {
                match items.get(1).map(|v| v.kind()) {
                    Some(ValueKind::Fixnum(n)) if n >= 0 => Some(n as usize),
                    _ => None,
                }
            } else {
                None
            };
            let vec_len = items.len();
            if is_bool_vector {
                let len = match bool_len {
                    Some(n) => n,
                    None => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("bool-vector-p"), args[0]],
                        ));
                    }
                };
                if idx >= len {
                    return Err(signal("args-out-of-range", vec![args[0], args[1]]));
                }
                let store_idx = idx + 2;
                if store_idx >= vec_len {
                    return Err(signal("args-out-of-range", vec![args[0], args[1]]));
                }
                let val = Value::fixnum(if args[2].is_truthy() { 1 } else { 0 });
                match args[0].veclike_type() {
                    Some(VecLikeType::Vector) => {
                        args[0].set_vector_slot(store_idx, val);
                    }
                    Some(VecLikeType::Record) => {
                        args[0].set_record_slot(store_idx, val);
                    }
                    _ => unreachable!("vector/record path should only reach vectorlike arrays"),
                }
                return Ok(args[2]);
            }
            if idx >= vec_len {
                return Err(signal("args-out-of-range", vec![args[0], args[1]]));
            }
            match args[0].veclike_type() {
                Some(VecLikeType::Vector) => {
                    args[0].set_vector_slot(idx, args[2]);
                }
                Some(VecLikeType::Record) => {
                    args[0].set_record_slot(idx, args[2]);
                }
                _ => unreachable!("vector/record path should only reach vectorlike arrays"),
            }
            Ok(args[2])
        }
        ValueKind::String => {
            let _updated = aset_string_replacement(&args[0], &args[1], &args[2])?;
            Ok(args[2])
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("arrayp"), args[0]],
        )),
    }
}

pub(crate) fn builtin_vconcat(args: Vec<Value>) -> EvalResult {
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
                _tail => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("listp"), cursor],
                    ));
                }
            }
        }
    }

    let mut result = Vec::new();
    for arg in &args {
        match arg.kind() {
            ValueKind::Veclike(VecLikeType::Vector) | ValueKind::Veclike(VecLikeType::Record) => {
                result.extend(arg.as_vector_data().unwrap().clone().into_iter())
            }
            ValueKind::String => {
                let string = arg.as_lisp_string().expect("string");
                super::for_each_lisp_string_char(string, |cp| {
                    result.push(Value::fixnum(cp as i64));
                });
            }
            ValueKind::Nil => {}
            ValueKind::Cons => extend_from_proper_list(&mut result, arg)?,
            ValueKind::Veclike(VecLikeType::Lambda) => {
                result.extend(lambda_to_closure_vector(arg).into_iter())
            }
            ValueKind::Veclike(VecLikeType::ByteCode) => {
                result.extend(bytecode_to_closure_vector(arg).into_iter())
            }
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("sequencep"), *arg],
                ));
            }
        }
    }
    Ok(Value::vector(result))
}

// ===========================================================================
// Hash table operations
// ===========================================================================

thread_local! {
    static HASH_TABLE_TEST_ALIASES: RefCell<HashMap<String, HashTableTest>> =
        RefCell::new(HashMap::new());
}

pub(super) fn reset_collections_thread_locals() {
    HASH_TABLE_TEST_ALIASES.with(|slot| slot.borrow_mut().clear());
}

fn invalid_hash_table_argument_list(arg: Value) -> Flow {
    signal("error", vec![Value::string("Invalid argument list"), arg])
}

fn hash_test_from_designator(value: &Value) -> Option<HashTableTest> {
    let name = value.as_symbol_name()?;
    match name {
        "eq" => Some(HashTableTest::Eq),
        "eql" => Some(HashTableTest::Eql),
        "equal" => Some(HashTableTest::Equal),
        _ => None,
    }
}

fn hash_test_from_user_test_pair(test: &Value, hash: &Value) -> Option<HashTableTest> {
    let test_name = test.as_symbol_name()?;
    let hash_name = hash.as_symbol_name()?;
    match (test_name, hash_name) {
        ("eq", "sxhash-eq") => Some(HashTableTest::Eq),
        ("eql", "sxhash-eql") => Some(HashTableTest::Eql),
        ("equal", "sxhash-equal")
        | ("equal", "sxhash-equal-including-properties")
        | ("equal-including-properties", "sxhash-equal")
        | ("equal-including-properties", "sxhash-equal-including-properties") => {
            Some(HashTableTest::Equal)
        }
        _ => None,
    }
}

fn register_hash_table_test_alias(name: &str, test: HashTableTest) {
    HASH_TABLE_TEST_ALIASES.with(|slot| slot.borrow_mut().insert(name.to_string(), test));
}

pub(super) fn lookup_hash_table_test_alias(name: &str) -> Option<HashTableTest> {
    HASH_TABLE_TEST_ALIASES.with(|slot| slot.borrow().get(name).cloned())
}

fn maybe_resize_hash_table_for_insert(table: &mut LispHashTable, inserting_new_key: bool) {
    if !inserting_new_key {
        return;
    }
    let current_size = usize::try_from(table.size.max(0)).unwrap_or(usize::MAX);
    if table.data.len() < current_size {
        return;
    }

    // Match Emacs growth policy: zero-sized tables grow to 6 slots on first
    // insertion; small tables then grow by 4x (up to size 64), larger tables
    // grow by 2x.
    let min_size = 6_i64;
    let base = table.size.max(min_size).min(i64::MAX / 2);
    table.size = if table.size == 0 {
        min_size
    } else if base <= 64 {
        base.saturating_mul(4)
    } else {
        base.saturating_mul(2)
    };
}

pub(crate) fn builtin_define_hash_table_test(args: Vec<Value>) -> EvalResult {
    expect_args("define-hash-table-test", &args, 3)?;
    let Some(alias_name) = args[0].as_symbol_name() else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        ));
    };
    if let Some(test) = hash_test_from_user_test_pair(&args[1], &args[2])
        .or_else(|| hash_test_from_designator(&args[1]))
    {
        register_hash_table_test_alias(alias_name, test);
    }
    Ok(Value::list(vec![args[1], args[2]]))
}

pub(crate) fn builtin_make_hash_table(args: Vec<Value>) -> EvalResult {
    let mut test = HashTableTest::Eql;
    let mut test_name: Option<SymId> = None;
    let mut size: i64 = 0;
    let mut weakness: Option<HashTableWeakness> = None;
    let mut seen_test = false;
    let mut seen_size = false;
    let mut seen_weakness = false;
    let mut seen_rehash_size = false;
    let mut seen_rehash_threshold = false;

    let mut i = 0;
    while i < args.len() {
        let Some(option) = args[i].as_keyword_id() else {
            return Err(invalid_hash_table_argument_list(args[i]));
        };

        match resolve_sym(option) {
            ":test" => {
                if seen_test {
                    return Err(invalid_hash_table_argument_list(args[i]));
                }
                let Some(value) = args.get(i + 1) else {
                    return Err(invalid_hash_table_argument_list(args[i]));
                };
                seen_test = true;
                match value.kind() {
                    ValueKind::Nil => {
                        return Err(signal(
                            "error",
                            vec![Value::string("Invalid hash table test")],
                        ));
                    }
                    _ => {
                        let Some(name) = value.as_symbol_name() else {
                            return Err(signal(
                                "wrong-type-argument",
                                vec![Value::symbol("symbolp"), *value],
                            ));
                        };
                        test_name = Some(intern(name));
                        test = match name {
                            "eq" => HashTableTest::Eq,
                            "eql" => HashTableTest::Eql,
                            "equal" => HashTableTest::Equal,
                            _ => {
                                if let Some(alias_test) = lookup_hash_table_test_alias(name) {
                                    alias_test
                                } else {
                                    return Err(signal(
                                        "error",
                                        vec![Value::string("Invalid hash table test"), *value],
                                    ));
                                }
                            }
                        };
                    }
                }
                i += 2;
            }
            ":size" => {
                if seen_size {
                    return Err(invalid_hash_table_argument_list(args[i]));
                }
                let Some(value) = args.get(i + 1) else {
                    return Err(invalid_hash_table_argument_list(args[i]));
                };
                seen_size = true;
                size = match value.kind() {
                    ValueKind::Nil => 0,
                    ValueKind::Fixnum(n) if n >= 0 => n,
                    _ => {
                        return Err(signal(
                            "error",
                            vec![Value::string("Invalid hash table size"), *value],
                        ));
                    }
                };
                i += 2;
            }
            ":weakness" => {
                if seen_weakness {
                    return Err(invalid_hash_table_argument_list(args[i]));
                }
                let Some(value) = args.get(i + 1) else {
                    return Err(invalid_hash_table_argument_list(args[i]));
                };
                seen_weakness = true;
                weakness = match value.kind() {
                    ValueKind::Nil => None,
                    ValueKind::T => Some(HashTableWeakness::KeyAndValue),
                    _ => {
                        let Some(name) = value.as_symbol_name() else {
                            return Err(signal(
                                "error",
                                vec![Value::string("Invalid hash table weakness"), *value],
                            ));
                        };
                        Some(match name {
                            "key" => HashTableWeakness::Key,
                            "value" => HashTableWeakness::Value,
                            "key-or-value" => HashTableWeakness::KeyOrValue,
                            "key-and-value" => HashTableWeakness::KeyAndValue,
                            _ => {
                                return Err(signal(
                                    "error",
                                    vec![Value::string("Invalid hash table weakness"), *value],
                                ));
                            }
                        })
                    }
                };
                i += 2;
            }
            ":rehash-size" => {
                if seen_rehash_size {
                    return Err(invalid_hash_table_argument_list(args[i]));
                }
                seen_rehash_size = true;
                if i + 1 >= args.len() {
                    i += 1;
                } else if args[i + 1].as_keyword_id().is_some_and(|kw| {
                    matches!(
                        resolve_sym(kw),
                        ":test" | ":size" | ":weakness" | ":rehash-size" | ":rehash-threshold"
                    )
                }) {
                    i += 1;
                } else {
                    i += 2;
                }
                continue;
            }
            ":rehash-threshold" => {
                if seen_rehash_threshold {
                    return Err(invalid_hash_table_argument_list(args[i]));
                }
                seen_rehash_threshold = true;
                if i + 1 >= args.len() {
                    i += 1;
                } else if args[i + 1].as_keyword_id().is_some_and(|kw| {
                    matches!(
                        resolve_sym(kw),
                        ":test" | ":size" | ":weakness" | ":rehash-size" | ":rehash-threshold"
                    )
                }) {
                    i += 1;
                } else {
                    i += 2;
                }
                continue;
            }
            _ => return Err(invalid_hash_table_argument_list(args[i])),
        }
    }
    let table = Value::hash_table_with_options(test, size, weakness, 1.5, 0.8125);
    if table.is_hash_table() {
        let _ = table.with_hash_table_mut(|ht| {
            ht.test_name = test_name;
        });
    }
    Ok(table)
}

pub(crate) fn builtin_gethash(args: Vec<Value>) -> EvalResult {
    builtin_gethash_with_symbols(args, false)
}

pub(crate) fn builtin_gethash_with_symbols(
    args: Vec<Value>,
    symbols_with_pos_enabled: bool,
) -> EvalResult {
    expect_min_args("gethash", &args, 2)?;
    let default = if args.len() > 2 { args[2] } else { Value::NIL };
    match args[1].kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => {
            let ht = args[1].as_hash_table().unwrap();
            let key = args[0].to_hash_key_swp(&ht.test, symbols_with_pos_enabled);
            Ok(ht.data.get(&key).cloned().unwrap_or(default))
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), args[1]],
        )),
    }
}

pub(crate) fn builtin_puthash(args: Vec<Value>) -> EvalResult {
    builtin_puthash_with_symbols(args, false)
}

pub(crate) fn builtin_puthash_with_symbols(
    args: Vec<Value>,
    symbols_with_pos_enabled: bool,
) -> EvalResult {
    expect_args("puthash", &args, 3)?;
    match args[2].kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => {
            let test = args[2].as_hash_table().unwrap().test.clone();
            let key = args[0].to_hash_key_swp(&test, symbols_with_pos_enabled);
            let _ = args[2].with_hash_table_mut(|ht| {
                let inserting_new_key = !ht.data.contains_key(&key);
                maybe_resize_hash_table_for_insert(ht, inserting_new_key);
                ht.data.insert(key.clone(), args[1]);
                if inserting_new_key {
                    ht.key_snapshots.insert(key.clone(), args[0]);
                    ht.insertion_order.push(key);
                }
            });
            Ok(args[1])
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), args[2]],
        )),
    }
}

pub(crate) fn builtin_remhash(args: Vec<Value>) -> EvalResult {
    builtin_remhash_with_symbols(args, false)
}

pub(crate) fn builtin_remhash_with_symbols(
    args: Vec<Value>,
    symbols_with_pos_enabled: bool,
) -> EvalResult {
    expect_args("remhash", &args, 2)?;
    match args[1].kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => {
            let test = args[1].as_hash_table().unwrap().test.clone();
            let key = args[0].to_hash_key_swp(&test, symbols_with_pos_enabled);
            let _ = args[1].with_hash_table_mut(|ht| {
                ht.data.remove(&key);
                ht.key_snapshots.remove(&key);
                ht.insertion_order.retain(|k| k != &key);
            });
            Ok(Value::NIL)
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), args[1]],
        )),
    }
}

pub(crate) fn builtin_clrhash(args: Vec<Value>) -> EvalResult {
    expect_args("clrhash", &args, 1)?;
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => {
            let _ = args[0].with_hash_table_mut(|ht| {
                ht.data.clear();
                ht.key_snapshots.clear();
                ht.insertion_order.clear();
            });
            Ok(Value::NIL)
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), args[0]],
        )),
    }
}

pub(crate) fn builtin_hash_table_count(args: Vec<Value>) -> EvalResult {
    expect_args("hash-table-count", &args, 1)?;
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => Ok(Value::fixnum(
            args[0].as_hash_table().unwrap().data.len() as i64,
        )),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), args[0]],
        )),
    }
}

pub(crate) fn builtin_char_to_string(args: Vec<Value>) -> EvalResult {
    expect_args("char-to-string", &args, 1)?;
    let code = expect_character_code(&args[0])? as u32;
    if crate::emacs_core::emacs_char::char_byte8_p(code) {
        // Raw byte → unibyte string with the actual byte value
        let byte = crate::emacs_core::emacs_char::char_to_byte8(code);
        Ok(Value::heap_string(
            crate::heap_types::LispString::from_unibyte(vec![byte]),
        ))
    } else if code <= 0x7f {
        // ASCII → unibyte
        Ok(Value::heap_string(
            crate::heap_types::LispString::from_unibyte(vec![code as u8]),
        ))
    } else {
        // Non-ASCII Unicode → multibyte
        let mut buf = [0u8; crate::emacs_core::emacs_char::MAX_MULTIBYTE_LENGTH];
        let len = crate::emacs_core::emacs_char::char_string(code, &mut buf);
        Ok(Value::heap_string(
            crate::heap_types::LispString::from_emacs_bytes(buf[..len].to_vec()),
        ))
    }
}

pub(crate) fn builtin_string_to_char(args: Vec<Value>) -> EvalResult {
    expect_args("string-to-char", &args, 1)?;
    let string = args[0].as_lisp_string().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[0]],
        )
    })?;
    let codes = super::lisp_string_char_codes(string);
    let first = codes.into_iter().next().unwrap_or(0);
    Ok(Value::fixnum(first as i64))
}

// ===========================================================================
// Property lists
// ===========================================================================

pub(crate) fn builtin_plist_get(args: Vec<Value>) -> EvalResult {
    builtin_plist_get_eq_swp(args, false)
}

pub(crate) fn builtin_plist_get_with_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("plist-get", &args, 2)?;
    expect_max_args("plist-get", &args, 3)?;
    if args.get(2).is_none_or(|value| value.is_nil()) {
        return builtin_plist_get_eq_swp(args, eval.symbols_with_pos_enabled);
    }

    let plist = args[0];
    let prop = args[1];
    let predicate = args[2];
    let roots = eval.save_specpdl_roots();
    eval.push_specpdl_root(plist);
    eval.push_specpdl_root(prop);
    eval.push_specpdl_root(predicate);

    let mut cursor = plist;
    let plist_result = loop {
        match cursor.kind() {
            ValueKind::Cons => {
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                if !pair_cdr.is_cons() {
                    break Ok(Value::NIL);
                }
                match eval.apply(predicate, vec![pair_car, prop]) {
                    Ok(value) if value.is_truthy() => break Ok(pair_cdr.cons_car()),
                    Ok(_) => {
                        cursor = pair_cdr.cons_cdr();
                    }
                    Err(err) => break Err(err),
                }
            }
            _ => break Ok(Value::NIL),
        }
    };

    eval.restore_specpdl_roots(roots);
    plist_result
}

fn builtin_plist_get_eq_swp(args: Vec<Value>, symbols_with_pos_enabled: bool) -> EvalResult {
    expect_min_args("plist-get", &args, 2)?;
    expect_max_args("plist-get", &args, 3)?;
    if args.get(2).is_some_and(|value| !value.is_nil()) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[2]],
        ));
    }
    let mut cursor = args[0];
    loop {
        match cursor.kind() {
            ValueKind::Cons => {
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                if eq_value_swp(&pair_car, &args[1], symbols_with_pos_enabled) {
                    // Next element is the value
                    match pair_cdr.kind() {
                        ValueKind::Cons => {
                            return Ok(pair_cdr.cons_car());
                        }
                        _ => return Ok(Value::NIL),
                    }
                }
                // Skip the value entry
                match pair_cdr.kind() {
                    ValueKind::Cons => {
                        cursor = pair_cdr.cons_cdr();
                    }
                    _ => return Ok(Value::NIL),
                }
            }
            _ => return Ok(Value::NIL),
        }
    }
}

pub(crate) fn builtin_plist_put(args: Vec<Value>) -> EvalResult {
    builtin_plist_put_eq_swp(args, false)
}

pub(crate) fn builtin_plist_put_with_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("plist-put", &args, 3)?;
    expect_max_args("plist-put", &args, 4)?;
    if args.get(3).is_none_or(|value| value.is_nil()) {
        return builtin_plist_put_eq_swp(args, eval.symbols_with_pos_enabled);
    }

    let plist = args[0];
    let key = args[1];
    let new_val = args[2];
    let predicate = args[3];
    let roots = eval.save_specpdl_roots();
    eval.push_specpdl_root(plist);
    eval.push_specpdl_root(key);
    eval.push_specpdl_root(new_val);
    eval.push_specpdl_root(predicate);

    let mut cursor = plist;
    let mut prev = Value::NIL;
    let plist_result = loop {
        match cursor.kind() {
            ValueKind::Cons => {
                let entry_key = cursor.cons_car();
                let entry_rest = cursor.cons_cdr();
                if !entry_rest.is_cons() {
                    break Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("plistp"), plist],
                    ));
                }

                match eval.apply(predicate, vec![entry_key, key]) {
                    Ok(value) if value.is_truthy() => {
                        entry_rest.set_car(new_val);
                        break Ok(plist);
                    }
                    Ok(_) => {
                        prev = cursor;
                        cursor = entry_rest.cons_cdr();
                    }
                    Err(err) => break Err(err),
                }
            }
            ValueKind::Nil => {
                let new_cell = Value::cons(key, Value::cons(new_val, Value::NIL));
                if prev.is_nil() {
                    break Ok(new_cell);
                }
                prev.cons_cdr().set_cdr(new_cell);
                break Ok(plist);
            }
            _ => {
                break Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("plistp"), plist],
                ));
            }
        }
    };

    eval.restore_specpdl_roots(roots);
    plist_result
}

fn builtin_plist_put_eq_swp(args: Vec<Value>, symbols_with_pos_enabled: bool) -> EvalResult {
    expect_min_args("plist-put", &args, 3)?;
    expect_max_args("plist-put", &args, 4)?;
    if args.get(3).is_some_and(|value| !value.is_nil()) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[3]],
        ));
    }
    let plist = args[0];
    let key = args[1];
    let new_val = args[2];

    if plist.is_nil() {
        return Ok(Value::list(vec![key, new_val]));
    }

    let mut cursor = plist;
    let mut last_value_cell: Option<Value> = None;

    loop {
        match cursor.kind() {
            ValueKind::Cons => {
                let entry_key = cursor.cons_car();
                let entry_rest = cursor.cons_cdr();

                match entry_rest.kind() {
                    ValueKind::Cons => {
                        if eq_value_swp(&entry_key, &key, symbols_with_pos_enabled) {
                            entry_rest.set_car(new_val);
                            return Ok(plist);
                        }
                        let value_cell = entry_rest;
                        cursor = entry_rest.cons_cdr();
                        last_value_cell = Some(value_cell);
                    }
                    _ => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("plistp"), plist],
                        ));
                    }
                }
            }
            ValueKind::Nil => {
                if let Some(value_cell) = last_value_cell {
                    let new_tail = Value::cons(key, Value::cons(new_val, Value::NIL));
                    value_cell.set_cdr(new_tail);
                    return Ok(plist);
                }
                return Ok(Value::list(vec![key, new_val]));
            }
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("plistp"), plist],
                ));
            }
        }
    }
}

pub(crate) fn builtin_plist_member(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let predicate = args
        .get(2)
        .and_then(|value| if value.is_nil() { None } else { Some(*value) });
    if predicate.is_none() {
        return plist_member_eq_swp(args, eval.symbols_with_pos_enabled);
    }

    expect_range_args("plist-member", &args, 2, 3)?;
    let plist = args[0];
    let prop = args[1];
    let predicate = predicate;

    // Root Values that survive across eval.apply() in the loop.
    let roots = eval.save_specpdl_roots();
    eval.push_specpdl_root(plist);
    eval.push_specpdl_root(prop);
    if let Some(p) = predicate {
        eval.push_specpdl_root(p);
    }

    let mut cursor = plist;
    let plist_result = loop {
        match cursor.kind() {
            ValueKind::Cons => {
                let entry_key = cursor.cons_car();
                let entry_rest = cursor.cons_cdr();

                let matches = if let Some(predicate) = &predicate {
                    match eval.apply(*predicate, vec![entry_key, prop]) {
                        Ok(v) => v.is_truthy(),
                        Err(e) => {
                            break Err(e);
                        }
                    }
                } else {
                    eq_value(&entry_key, &prop)
                };
                if matches {
                    break Ok(cursor);
                }

                // See `plist_member_eq` for the nil-terminator
                // rule: an unpaired last key is a valid end per
                // GNU, only dotted tails signal plistp.
                match entry_rest.kind() {
                    ValueKind::Cons => {
                        cursor = entry_rest.cons_cdr();
                    }
                    ValueKind::Nil => {
                        break Ok(Value::NIL);
                    }
                    _ => {
                        break Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("plistp"), plist],
                        ));
                    }
                }
            }
            ValueKind::Nil => break Ok(Value::NIL),
            _ => {
                break Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("plistp"), plist],
                ));
            }
        }
    };
    eval.restore_specpdl_roots(roots);
    plist_result
}

pub(crate) fn plist_member_eq(args: Vec<Value>) -> EvalResult {
    plist_member_eq_swp(args, false)
}

pub(crate) fn plist_member_eq_swp(args: Vec<Value>, symbols_with_pos_enabled: bool) -> EvalResult {
    expect_range_args("plist-member", &args, 2, 3)?;
    let plist = args[0];
    let prop = args[1];
    if args.get(2).is_some_and(|value| !value.is_nil()) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[2]],
        ));
    }

    // Mirrors GNU's `Fplist_member` / `plist_member_eq` (fns.c). Walks
    // the plist two elements at a time looking for PROP. A nil tail at
    // any step ends the walk cleanly and returns nil (not-found),
    // matching GNU `FOR_EACH_TAIL`'s implicit break on non-cons. Only a
    // non-nil improper tail (dotted list) signals `plistp`.
    let mut cursor = plist;
    loop {
        match cursor.kind() {
            ValueKind::Cons => {
                let entry_key = cursor.cons_car();
                let entry_rest = cursor.cons_cdr();

                if eq_value_swp(&entry_key, &prop, symbols_with_pos_enabled) {
                    return Ok(cursor);
                }

                match entry_rest.kind() {
                    ValueKind::Cons => {
                        cursor = entry_rest.cons_cdr();
                    }
                    ValueKind::Nil => {
                        // Unpaired last key: valid end of plist per
                        // GNU; return not-found.
                        return Ok(Value::NIL);
                    }
                    _ => {
                        // Dotted tail after a key: malformed plist.
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("plistp"), plist],
                        ));
                    }
                }
            }
            ValueKind::Nil => return Ok(Value::NIL),
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("plistp"), plist],
                ));
            }
        }
    }
}
