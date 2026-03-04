use super::*;

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
    match &args[0] {
        Value::Vector(_) if super::chartable::is_char_table(&args[0]) => {
            let ch = expect_char_table_index(&args[1])?;
            super::chartable::builtin_char_table_range(vec![args[0], Value::Int(ch)])
        }
        Value::Vector(v) | Value::Record(v) => {
            let idx = idx_fixnum as usize;
            let items = with_heap(|h| h.get_vector(*v).clone());
            let is_bool_vector = items.len() >= 2
                && matches!(&items[0], Value::Symbol(id) if resolve_sym(*id) == "--bool-vector--");
            if is_bool_vector {
                let len = match items.get(1) {
                    Some(Value::Int(n)) if *n >= 0 => *n as usize,
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
                    .cloned()
                    .ok_or_else(|| signal("args-out-of-range", vec![args[0], args[1]]))?;
                let truthy = match bit {
                    Value::Int(n) => n != 0,
                    Value::Nil => false,
                    other => other.is_truthy(),
                };
                return Ok(Value::bool(truthy));
            }
            items
                .get(idx)
                .cloned()
                .ok_or_else(|| signal("args-out-of-range", vec![args[0], args[1]]))
        }
        Value::Str(id) => {
            let idx = idx_fixnum as usize;
            let s = with_heap(|h| h.get_string(*id).clone());
            let codes = decode_storage_char_codes(&s);
            codes
                .get(idx)
                .map(|cp| Value::Int(*cp as i64))
                .ok_or_else(|| signal("args-out-of-range", vec![args[0], args[1]]))
        }
        // In official Emacs, closures support aref for oclosure slot access.
        // The closure vector layout is:
        //   [0]=ARGS  [1]=BODY  [2]=ENV  [3]=nil  [4]=DOCSTRING  [5]=IFORM
        Value::Lambda(_) => {
            let idx = idx_fixnum as usize;
            let vec = lambda_to_closure_vector(&args[0]);
            vec.get(idx)
                .cloned()
                .ok_or_else(|| signal("args-out-of-range", vec![args[0], args[1]]))
        }
        // ByteCode closures: [0]=ARGLIST [1]=CODE [2]=ENV/CONSTANTS [3]=DEPTH [4]=DOC
        Value::ByteCode(_) => {
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
    let Value::Str(original) = array else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *array],
        ));
    };

    let idx = expect_fixnum(index)? as usize;
    let original_str = with_heap(|h| h.get_string(*original).clone());
    let mut codes = decode_storage_char_codes(&original_str);
    if idx >= codes.len() {
        return Err(signal("args-out-of-range", vec![*array, *index]));
    }

    let replacement_code = insert_char_code_from_value(new_element)? as u32;
    codes[idx] = replacement_code;

    let mut rebuilt = String::new();
    for code in codes {
        if let Some(ch) = char::from_u32(code) {
            rebuilt.push(ch);
        } else if let Some(encoded) = encode_nonunicode_char_for_storage(code) {
            rebuilt.push_str(&encoded);
        } else {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), *new_element],
            ));
        }
    }
    // Modify the string in-place on the heap so identity (eq) is preserved.
    with_heap_mut(|h| *h.get_string_mut(*original) = rebuilt);
    Ok(*array)
}

pub(crate) fn builtin_aset(args: Vec<Value>) -> EvalResult {
    expect_args("aset", &args, 3)?;
    match &args[0] {
        Value::Vector(_) if super::chartable::is_char_table(&args[0]) => {
            let ch = expect_char_table_index(&args[1])?;
            super::chartable::builtin_set_char_table_range(vec![args[0], Value::Int(ch), args[2]])
        }
        Value::Vector(v) | Value::Record(v) => {
            let idx = expect_fixnum(&args[1])? as usize;
            let (is_bool_vector, vec_len, bool_len) = with_heap(|h| {
                let items = h.get_vector(*v);
                let bv = items.len() >= 2
                    && matches!(&items[0], Value::Symbol(id) if resolve_sym(*id) == "--bool-vector--");
                let bl = if bv {
                    match items.get(1) {
                        Some(Value::Int(n)) if *n >= 0 => Some(*n as usize),
                        _ => None,
                    }
                } else {
                    None
                };
                (bv, items.len(), bl)
            });
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
                let val = Value::Int(if args[2].is_truthy() { 1 } else { 0 });
                with_heap_mut(|h| h.get_vector_mut(*v)[store_idx] = val);
                return Ok(args[2]);
            }
            if idx >= vec_len {
                return Err(signal("args-out-of-range", vec![args[0], args[1]]));
            }
            with_heap_mut(|h| h.get_vector_mut(*v)[idx] = args[2]);
            Ok(args[2])
        }
        Value::Str(_) => {
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

    let mut result = Vec::new();
    for arg in &args {
        match arg {
            Value::Vector(v) | Value::Record(v) => {
                result.extend(with_heap(|h| h.get_vector(*v).clone()).into_iter())
            }
            Value::Str(id) => {
                let s = with_heap(|h| h.get_string(*id).clone());
                result.extend(
                    decode_storage_char_codes(&s)
                        .into_iter()
                        .map(|cp| Value::Int(cp as i64)),
                );
            }
            Value::Nil => {}
            Value::Cons(_) => extend_from_proper_list(&mut result, arg)?,
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
    if let Some(test) = hash_test_from_designator(&args[1]) {
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
        let Value::Keyword(option) = &args[i] else {
            return Err(invalid_hash_table_argument_list(args[i]));
        };

        match resolve_sym(*option) {
            ":test" => {
                if seen_test {
                    return Err(invalid_hash_table_argument_list(args[i]));
                }
                let Some(value) = args.get(i + 1) else {
                    return Err(invalid_hash_table_argument_list(args[i]));
                };
                seen_test = true;
                match value {
                    Value::Nil => {
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
                size = match value {
                    Value::Nil => 0,
                    Value::Int(n) if *n >= 0 => *n,
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
                weakness = match value {
                    Value::Nil => None,
                    Value::True => Some(HashTableWeakness::KeyAndValue),
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
                } else if matches!(
                    &args[i + 1],
                    Value::Keyword(option) if matches!(
                        resolve_sym(*option),
                        ":test" | ":size" | ":weakness" | ":rehash-size" | ":rehash-threshold"
                    )
                ) {
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
                } else if matches!(
                    &args[i + 1],
                    Value::Keyword(option) if matches!(
                        resolve_sym(*option),
                        ":test" | ":size" | ":weakness" | ":rehash-size" | ":rehash-threshold"
                    )
                ) {
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
    if let Value::HashTable(table_ref) = &table {
        with_heap_mut(|h| h.get_hash_table_mut(*table_ref).test_name = test_name);
    }
    Ok(table)
}

pub(crate) fn builtin_gethash(args: Vec<Value>) -> EvalResult {
    expect_min_args("gethash", &args, 2)?;
    let default = if args.len() > 2 { args[2] } else { Value::Nil };
    match &args[1] {
        Value::HashTable(ht) => {
            let ht = with_heap(|h| h.get_hash_table(*ht).clone());
            let key = args[0].to_hash_key(&ht.test);
            Ok(ht.data.get(&key).cloned().unwrap_or(default))
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), args[1]],
        )),
    }
}

pub(crate) fn builtin_puthash(args: Vec<Value>) -> EvalResult {
    expect_args("puthash", &args, 3)?;
    match &args[2] {
        Value::HashTable(ht_id) => {
            let test = with_heap(|h| h.get_hash_table(*ht_id).test.clone());
            let key = args[0].to_hash_key(&test);
            with_heap_mut(|h| {
                let ht = h.get_hash_table_mut(*ht_id);
                let inserting_new_key = !ht.data.contains_key(&key);
                maybe_resize_hash_table_for_insert(ht, inserting_new_key);
                ht.data.insert(key.clone(), args[1]);
                if inserting_new_key {
                    ht.key_snapshots.insert(key, args[0]);
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
    expect_args("remhash", &args, 2)?;
    match &args[1] {
        Value::HashTable(ht_id) => {
            let test = with_heap(|h| h.get_hash_table(*ht_id).test.clone());
            let key = args[0].to_hash_key(&test);
            with_heap_mut(|h| {
                let ht = h.get_hash_table_mut(*ht_id);
                ht.data.remove(&key);
                ht.key_snapshots.remove(&key);
            });
            Ok(Value::Nil)
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), args[1]],
        )),
    }
}

pub(crate) fn builtin_clrhash(args: Vec<Value>) -> EvalResult {
    expect_args("clrhash", &args, 1)?;
    match &args[0] {
        Value::HashTable(ht_id) => {
            with_heap_mut(|h| {
                let ht = h.get_hash_table_mut(*ht_id);
                ht.data.clear();
                ht.key_snapshots.clear();
            });
            Ok(Value::Nil)
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), args[0]],
        )),
    }
}

pub(crate) fn builtin_hash_table_count(args: Vec<Value>) -> EvalResult {
    expect_args("hash-table-count", &args, 1)?;
    match &args[0] {
        Value::HashTable(ht) => Ok(Value::Int(
            with_heap(|h| h.get_hash_table(*ht).data.len()) as i64
        )),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), args[0]],
        )),
    }
}

pub(crate) fn builtin_char_to_string(args: Vec<Value>) -> EvalResult {
    expect_args("char-to-string", &args, 1)?;
    match &args[0] {
        Value::Char(c) => Ok(Value::string(c.to_string())),
        Value::Int(n) => {
            if *n < 0 {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("characterp"), args[0]],
                ));
            }
            if let Some(c) = char::from_u32(*n as u32) {
                Ok(Value::string(c.to_string()))
            } else if let Some(encoded) = encode_nonunicode_char_for_storage(*n as u32) {
                Ok(Value::string(encoded))
            } else {
                Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("characterp"), args[0]],
                ))
            }
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *other],
        )),
    }
}

pub(crate) fn builtin_string_to_char(args: Vec<Value>) -> EvalResult {
    expect_args("string-to-char", &args, 1)?;
    let s = expect_string(&args[0])?;
    let first = decode_storage_char_codes(&s)
        .into_iter()
        .next()
        .unwrap_or(0);
    Ok(Value::Int(first as i64))
}

// ===========================================================================
// Property lists
// ===========================================================================

pub(crate) fn builtin_plist_get(args: Vec<Value>) -> EvalResult {
    expect_args("plist-get", &args, 2)?;
    let mut cursor = args[0];
    loop {
        match cursor {
            Value::Cons(cell) => {
                let pair = read_cons(cell);
                if eq_value(&pair.car, &args[1]) {
                    // Next element is the value
                    match &pair.cdr {
                        Value::Cons(val_cell) => {
                            return Ok(with_heap(|h| h.cons_car(*val_cell)));
                        }
                        _ => return Ok(Value::Nil),
                    }
                }
                // Skip the value entry
                match &pair.cdr {
                    Value::Cons(val_cell) => {
                        cursor = with_heap(|h| h.cons_cdr(*val_cell));
                    }
                    _ => return Ok(Value::Nil),
                }
            }
            _ => return Ok(Value::Nil),
        }
    }
}

pub(crate) fn builtin_plist_put(args: Vec<Value>) -> EvalResult {
    expect_args("plist-put", &args, 3)?;
    let plist = args[0];
    let key = args[1];
    let new_val = args[2];

    if plist.is_nil() {
        return Ok(Value::list(vec![key, new_val]));
    }

    let mut cursor = plist;
    let mut last_value_cell = None;

    loop {
        match cursor {
            Value::Cons(key_cell) => {
                let (entry_key, entry_rest) = {
                    let pair = read_cons(key_cell);
                    (pair.car, pair.cdr)
                };

                match entry_rest {
                    Value::Cons(value_cell) => {
                        if eq_value(&entry_key, &key) {
                            with_heap_mut(|h| h.set_car(value_cell, new_val));
                            return Ok(plist);
                        }
                        cursor = with_heap(|h| h.cons_cdr(value_cell));
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
            Value::Nil => {
                if let Some(value_cell) = last_value_cell {
                    let new_tail = Value::cons(key, Value::cons(new_val, Value::Nil));
                    with_heap_mut(|h| h.set_cdr(value_cell, new_tail));
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
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("plist-member", &args, 2, 3)?;
    let plist = args[0];
    let prop = args[1];
    let predicate = args
        .get(2)
        .and_then(|value| if value.is_nil() { None } else { Some(*value) });

    // Root Values that survive across eval.apply() in the loop.
    let saved_roots = eval.save_temp_roots();
    eval.push_temp_root(plist);
    eval.push_temp_root(prop);
    if let Some(p) = predicate {
        eval.push_temp_root(p);
    }

    let result = (|| -> EvalResult {
        let mut cursor = plist;
        loop {
            match cursor {
                Value::Cons(key_cell) => {
                    let (entry_key, entry_rest) = {
                        let pair = read_cons(key_cell);
                        (pair.car, pair.cdr)
                    };

                    let matches = if let Some(predicate) = &predicate {
                        eval.apply(*predicate, vec![entry_key, prop])?.is_truthy()
                    } else {
                        eq_value(&entry_key, &prop)
                    };
                    if matches {
                        return Ok(Value::Cons(key_cell));
                    }

                    match entry_rest {
                        Value::Cons(value_cell) => {
                            cursor = with_heap(|h| h.cons_cdr(value_cell));
                        }
                        _ => {
                            return Err(signal(
                                "wrong-type-argument",
                                vec![Value::symbol("plistp"), plist],
                            ));
                        }
                    }
                }
                Value::Nil => return Ok(Value::Nil),
                _ => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("plistp"), plist],
                    ));
                }
            }
        }
    })();

    eval.restore_temp_roots(saved_roots);
    result
}
