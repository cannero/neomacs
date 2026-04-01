use super::*;

fn install_test_runtime() {
    // Tagged heap is auto-created in test mode; no manual setup needed.
}

#[test]
fn file_user_uid_matches_user_uid() {
    let user_uid = builtin_user_uid(vec![]).expect("user-uid should succeed");
    let file_user_uid = builtin_file_user_uid(vec![]).expect("file-user-uid should succeed");
    assert_eq!(file_user_uid, user_uid);
    assert!(file_user_uid.is_fixnum());
}

#[test]
fn file_user_uid_arity_errors() {
    assert!(builtin_file_user_uid(vec![Value::NIL]).is_err());
}

#[test]
fn file_group_gid_matches_group_gid() {
    let group_gid = builtin_group_gid(vec![]).expect("group-gid should succeed");
    let file_group_gid = builtin_file_group_gid(vec![]).expect("file-group-gid should succeed");
    assert_eq!(file_group_gid, group_gid);
    assert!(file_group_gid.is_fixnum());
}

#[test]
fn file_group_gid_arity_errors() {
    assert!(builtin_file_group_gid(vec![Value::NIL]).is_err());
}

// -- expect_args / expect_min_args / expect_max_args ----------------------

#[test]
fn expect_args_exact_match() {
    assert!(expect_args("test", &[Value::NIL, Value::NIL], 2).is_ok());
}

#[test]
fn expect_args_wrong_count() {
    let err = expect_args("test", &[Value::NIL], 2);
    assert!(err.is_err());
}

#[test]
fn expect_min_args_at_min() {
    assert!(expect_min_args("test", &[Value::NIL], 1).is_ok());
}

#[test]
fn expect_min_args_below_min() {
    assert!(expect_min_args("test", &[], 1).is_err());
}

#[test]
fn expect_max_args_at_max() {
    assert!(expect_max_args("test", &[Value::NIL, Value::NIL], 2).is_ok());
}

#[test]
fn expect_max_args_above_max() {
    assert!(expect_max_args("test", &[Value::NIL, Value::NIL, Value::NIL], 2).is_err());
}

// -- expect_integer -------------------------------------------------------

#[test]
fn expect_integer_from_int() {
    assert_eq!(expect_integer("test", &Value::fixnum(42)).unwrap(), 42);
}

#[test]
fn expect_integer_from_non_int() {
    assert!(expect_integer("test", &Value::NIL).is_err());
}

// -- collect_insert_text --------------------------------------------------

#[test]
fn collect_insert_text_strings_and_chars() {
    install_test_runtime();

    let s = Value::string("hello");
    let c = Value::char('!');
    let result = collect_insert_text("insert", &[s, c]).unwrap();
    assert_eq!(result, "hello!");
}

#[test]
fn collect_insert_text_int_as_char() {
    install_test_runtime();

    // ASCII 65 = 'A'
    let result = collect_insert_text("insert", &[Value::fixnum(65)]).unwrap();
    assert_eq!(result, "A");
}

#[test]
fn collect_insert_text_wrong_type() {
    install_test_runtime();

    assert!(collect_insert_text("insert", &[Value::NIL]).is_err());
}

// -- builtin_logcount -----------------------------------------------------

#[test]
fn logcount_positive() {
    install_test_runtime();

    // 7 = 0b111 → 3 bits
    let result = builtin_logcount(vec![Value::fixnum(7)]).unwrap();
    assert_val_eq!(result, Value::fixnum(3));
}

#[test]
fn logcount_zero() {
    install_test_runtime();

    let result = builtin_logcount(vec![Value::fixnum(0)]).unwrap();
    assert_val_eq!(result, Value::fixnum(0));
}

#[test]
fn logcount_negative() {
    install_test_runtime();

    // -1 = all 1s → !(-1) = 0 → count_ones = 0
    let result = builtin_logcount(vec![Value::fixnum(-1)]).unwrap();
    assert_val_eq!(result, Value::fixnum(0));

    // -2 = ...1110 → !(-2) = 1 → count_ones = 1
    let result = builtin_logcount(vec![Value::fixnum(-2)]).unwrap();
    assert_val_eq!(result, Value::fixnum(1));
}

#[test]
fn logcount_wrong_type() {
    install_test_runtime();

    assert!(builtin_logcount(vec![Value::NIL]).is_err());
}

#[test]
fn erase_buffer_widens_before_deleting_current_contents() {
    install_test_runtime();

    let obarray = Obarray::new();
    let dynamic: Vec<OrderedRuntimeBindingMap> = Vec::new();
    let mut buffers = crate::buffer::BufferManager::new();
    let current = buffers.current_buffer_id().expect("current buffer");
    let _ = buffers.insert_into_buffer(current, "abcdef");
    {
        let buf = buffers.get_mut(current).expect("buffer");
        buf.narrow_to_region(2, 4);
        buf.goto_char(4);
    }

    let result = erase_buffer_impl(&obarray, &dynamic, &mut buffers, vec![]);
    assert!(result.as_ref().map_or(false, |v| v.is_nil()));

    let buf = buffers.get(current).expect("buffer after erase");
    assert_eq!(buf.buffer_string(), "");
    assert_eq!(buf.buffer_size(), 0);
    assert_eq!(buf.point(), 0);
    assert_eq!(buf.point_min(), 0);
    assert_eq!(buf.point_max(), 0);
}
