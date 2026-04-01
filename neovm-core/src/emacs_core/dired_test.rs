use super::*;
use crate::emacs_core::value::ValueKind;
use std::io::Write;

/// Test helper: create a fresh eval context for dired tests.
fn test_eval_ctx() -> super::super::eval::Context {
    super::super::eval::Context::new()
}

fn call_directory_files_and_attributes(args: Vec<Value>) -> EvalResult {
    let mut eval = test_eval_ctx();
    builtin_directory_files_and_attributes(&mut eval, args)
}

fn call_file_name_all_completions(args: Vec<Value>) -> EvalResult {
    let mut eval = test_eval_ctx();
    builtin_file_name_all_completions(&mut eval, args)
}

fn call_file_attributes(args: Vec<Value>) -> EvalResult {
    let mut eval = test_eval_ctx();
    builtin_file_attributes(&mut eval, args)
}

fn make_test_dir(name: &str) -> (std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!("neovm_dired_test_{}", name));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_string_lossy().to_string();
    (dir, dir_str)
}

fn create_file(dir: &std::path::Path, name: &str, content: &str) {
    let path = dir.join(name);
    let mut f = fs::File::create(&path).unwrap();
    f.write_all(content.as_bytes()).unwrap();
}

// -----------------------------------------------------------------------
// directory-files-and-attributes
// -----------------------------------------------------------------------

#[test]
fn test_directory_files_and_attributes_basic() {
    crate::test_utils::init_test_tracing();
    let (dir, dir_str) = make_test_dir("dfa_basic");
    create_file(&dir, "test.txt", "hello");

    let result = call_directory_files_and_attributes(vec![Value::string(&dir_str)]).unwrap();
    let items = list_to_vec(&result).unwrap();

    // Find our file.
    let mut found = false;
    for item in &items {
        if item.is_cons() {
            let pair_car = item.cons_car();
            let pair_cdr = item.cons_cdr();
            if pair_car.as_str() == Some("test.txt") {
                found = true;
                // cdr should be a list (the attributes).
                assert!(pair_cdr.is_cons() || pair_cdr.is_nil());
            }
        }
    }
    assert!(found, "test.txt not found in results");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_directory_files_and_attributes_order_and_count() {
    crate::test_utils::init_test_tracing();
    let (dir, dir_str) = make_test_dir("dfa_count");
    create_file(&dir, "alpha.txt", "");
    create_file(&dir, "beta.txt", "");
    create_file(&dir, "z.txt", "");

    let unsorted = call_directory_files_and_attributes(vec![
        Value::string(&dir_str),
        Value::NIL,
        Value::NIL,
        Value::T,
    ])
    .unwrap();
    let unsorted_items = list_to_vec(&unsorted).unwrap();
    let unsorted_names: Vec<String> = unsorted_items
        .iter()
        .map(|pair| {
            if pair.is_cons() {
                pair.cons_car().as_str().unwrap().to_string()
            } else {
                panic!("expected cons pair");
            }
        })
        .collect();
    assert!(unsorted_names.contains(&".".to_string()));
    assert!(unsorted_names.contains(&"..".to_string()));

    let unsorted_limited = call_directory_files_and_attributes(vec![
        Value::string(&dir_str),
        Value::NIL,
        Value::NIL,
        Value::T,
        Value::NIL,
        Value::fixnum(2),
    ])
    .unwrap();
    let unsorted_limited_names: Vec<String> = list_to_vec(&unsorted_limited)
        .unwrap()
        .iter()
        .map(|pair| {
            if pair.is_cons() {
                pair.cons_car().as_str().unwrap().to_string()
            } else {
                panic!("expected cons pair");
            }
        })
        .collect();
    let tail = &unsorted_names[unsorted_names.len() - 2..];
    assert_eq!(unsorted_limited_names.as_slice(), tail);

    let sorted_limited = call_directory_files_and_attributes(vec![
        Value::string(&dir_str),
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::fixnum(2),
    ])
    .unwrap();
    let sorted_limited_names: Vec<String> = list_to_vec(&sorted_limited)
        .unwrap()
        .iter()
        .map(|pair| {
            if pair.is_cons() {
                pair.cons_car().as_str().unwrap().to_string()
            } else {
                panic!("expected cons pair");
            }
        })
        .collect();
    let mut sorted_from_unsorted = unsorted_limited_names.clone();
    sorted_from_unsorted.sort();
    assert_eq!(sorted_limited_names, sorted_from_unsorted);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_directory_files_and_attributes_count_and_id_format() {
    crate::test_utils::init_test_tracing();
    let (dir, dir_str) = make_test_dir("dfa_types");
    create_file(&dir, "alpha.txt", "");

    let result = call_directory_files_and_attributes(vec![
        Value::string(&dir_str),
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::fixnum(-1),
    ]);
    assert!(result.is_err());

    let result = call_directory_files_and_attributes(vec![
        Value::string(&dir_str),
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::string("x"),
    ]);
    assert!(result.is_err());

    let result = call_directory_files_and_attributes(vec![
        Value::string(&dir_str),
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::T,
        Value::fixnum(1),
    ])
    .unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 1);
    let attrs = if items[0].is_cons() {
        items[0].cons_cdr()
    } else {
        panic!("expected cons pair");
    };
    let attrs_items = list_to_vec(&attrs).unwrap();
    assert!(attrs_items[2].is_string());
    assert!(attrs_items[3].is_string());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_directory_files_and_attributes_eval_respects_default_directory() {
    crate::test_utils::init_test_tracing();
    let base = std::env::temp_dir().join("neovm_dfa_eval_builtin");
    let fixture = base.join("fixtures");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&fixture).unwrap();
    fs::write(fixture.join("alpha.txt"), "").unwrap();
    fs::write(fixture.join("beta.el"), "").unwrap();

    let mut eval = Context::new();
    let base_str = format!("{}/", base.to_string_lossy());
    eval.obarray
        .set_symbol_value("default-directory", Value::string(&base_str));

    let result = builtin_directory_files_and_attributes(
        &mut eval,
        vec![
            Value::string("fixtures"),
            Value::NIL,
            Value::string("\\.el$"),
        ],
    )
    .unwrap();

    let names: Vec<String> = list_to_vec(&result)
        .unwrap()
        .iter()
        .map(|pair| {
            if pair.is_cons() {
                pair.cons_car().as_str().unwrap().to_string()
            } else {
                panic!("expected cons pair");
            }
        })
        .collect();
    assert_eq!(names, vec!["beta.el"]);

    let _ = fs::remove_dir_all(&base);
}

// -----------------------------------------------------------------------
// file-name-completion
// -----------------------------------------------------------------------

#[test]
fn test_file_name_completion_basic() {
    crate::test_utils::init_test_tracing();
    let mut ctx = test_eval_ctx();
    let (dir, dir_str) = make_test_dir("fnc_basic");
    create_file(&dir, "foobar.txt", "");
    create_file(&dir, "foobaz.txt", "");

    // "foo" should complete to "fooba" (longest common prefix).
    let result = builtin_file_name_completion(
        &mut ctx,
        vec![Value::string("foo"), Value::string(&dir_str)],
    )
    .unwrap();
    assert_eq!(result.as_str(), Some("fooba"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_file_name_completion_exact() {
    crate::test_utils::init_test_tracing();
    let mut ctx = test_eval_ctx();
    let (dir, dir_str) = make_test_dir("fnc_exact");
    create_file(&dir, "unique.txt", "");

    let result = builtin_file_name_completion(
        &mut ctx,
        vec![Value::string("unique.txt"), Value::string(&dir_str)],
    )
    .unwrap();
    // Exact and unique match returns t.
    assert!(result.is_truthy());
    // In Emacs, exact unique match returns t.
    match result.kind() {
        ValueKind::T => {} // correct
        _ => panic!("Expected t for exact match, got {:?}", result),
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_file_name_completion_no_match() {
    crate::test_utils::init_test_tracing();
    let mut ctx = test_eval_ctx();
    let (dir, dir_str) = make_test_dir("fnc_none");
    create_file(&dir, "hello.txt", "");

    let result = builtin_file_name_completion(
        &mut ctx,
        vec![Value::string("xyz"), Value::string(&dir_str)],
    )
    .unwrap();
    assert!(result.is_nil());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_file_name_completion_dot_and_slash_behavior() {
    crate::test_utils::init_test_tracing();
    let mut ctx = test_eval_ctx();
    let (dir, dir_str) = make_test_dir("fnc_dot_slash");
    create_file(&dir, ".hidden", "");
    fs::create_dir(dir.join("subdir")).unwrap();

    let dot =
        builtin_file_name_completion(&mut ctx, vec![Value::string("."), Value::string(&dir_str)])
            .unwrap();
    assert_eq!(dot.as_str(), Some(".hidden"));

    let dotdot =
        builtin_file_name_completion(&mut ctx, vec![Value::string(".."), Value::string(&dir_str)])
            .unwrap();
    assert_eq!(dotdot.as_str(), Some("../"));

    let slash =
        builtin_file_name_completion(&mut ctx, vec![Value::string("./"), Value::string(&dir_str)])
            .unwrap();
    assert!(slash.is_nil());

    let subdir_prefix = builtin_file_name_completion(
        &mut ctx,
        vec![Value::string("sub"), Value::string(&dir_str)],
    )
    .unwrap();
    assert_eq!(subdir_prefix.as_str(), Some("subdir/"));

    let subdir_with_slash = builtin_file_name_completion(
        &mut ctx,
        vec![Value::string("subdir/"), Value::string(&dir_str)],
    )
    .unwrap();
    assert!(subdir_with_slash.is_nil());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_file_name_completion_predicate_with_eval() {
    crate::test_utils::init_test_tracing();
    let (dir, dir_str) = make_test_dir("fnc_predicate");
    create_file(&dir, "alpha.txt", "");
    fs::create_dir(dir.join("subdir")).unwrap();

    let mut eval = Context::new();
    eval.obarray
        .set_symbol_value("default-directory", Value::string("/tmp/"));

    let dirs_only_none = builtin_file_name_completion(
        &mut eval,
        vec![
            Value::string("a"),
            Value::string(&dir_str),
            Value::symbol("file-directory-p"),
        ],
    )
    .unwrap();
    assert!(dirs_only_none.is_nil());

    let dirs_only_match = builtin_file_name_completion(
        &mut eval,
        vec![
            Value::string("s"),
            Value::string(&dir_str),
            Value::symbol("file-directory-p"),
        ],
    )
    .unwrap();
    assert_eq!(dirs_only_match.as_str(), Some("subdir/"));

    let bad_pred = builtin_file_name_completion(
        &mut eval,
        vec![
            Value::string("a"),
            Value::string(&dir_str),
            Value::fixnum(123),
        ],
    );
    assert!(bad_pred.is_err());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_file_name_completion_eval_relative_directory() {
    crate::test_utils::init_test_tracing();
    let (base, base_str) = make_test_dir("fnc_eval_relative");
    let fixture_dir = base.join("fixtures");
    fs::create_dir(&fixture_dir).unwrap();
    create_file(&fixture_dir, "alpha.txt", "");
    fs::create_dir(fixture_dir.join("subdir")).unwrap();

    let mut eval = Context::new();
    eval.obarray.set_symbol_value(
        "default-directory",
        Value::string(ensure_trailing_slash(&base_str)),
    );

    let result = builtin_file_name_completion(
        &mut eval,
        vec![Value::string("sub"), Value::string("fixtures/")],
    )
    .unwrap();
    assert_eq!(result.as_str(), Some("subdir/"));

    let _ = fs::remove_dir_all(&base);
}

// -----------------------------------------------------------------------
// file-name-all-completions
// -----------------------------------------------------------------------

#[test]
fn test_file_name_all_completions() {
    crate::test_utils::init_test_tracing();
    let (dir, dir_str) = make_test_dir("fnac");
    create_file(&dir, "abc.txt", "");
    create_file(&dir, "abd.txt", "");
    create_file(&dir, "xyz.txt", "");

    let result =
        call_file_name_all_completions(vec![Value::string("ab"), Value::string(&dir_str)]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 2);
    let names: Vec<&str> = items.iter().map(|v| v.as_str().unwrap()).collect();
    assert!(names.contains(&"abc.txt"));
    assert!(names.contains(&"abd.txt"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_file_name_all_completions_empty() {
    crate::test_utils::init_test_tracing();
    let (dir, dir_str) = make_test_dir("fnac_empty");
    create_file(&dir, "hello.txt", "");

    let result =
        call_file_name_all_completions(vec![Value::string("zzz"), Value::string(&dir_str)])
            .unwrap();
    assert!(result.is_nil());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_file_name_all_completions_special_entries() {
    crate::test_utils::init_test_tracing();
    let (dir, dir_str) = make_test_dir("fnac_special");
    create_file(&dir, ".hidden", "");
    fs::create_dir(dir.join("subdir")).unwrap();

    let dot =
        call_file_name_all_completions(vec![Value::string("."), Value::string(&dir_str)]).unwrap();
    let dot_items = list_to_vec(&dot).unwrap();
    let dot_names: Vec<&str> = dot_items.iter().map(|v| v.as_str().unwrap()).collect();
    assert!(dot_names.contains(&"./"));
    assert!(dot_names.contains(&"../"));
    assert!(dot_names.contains(&".hidden"));

    let dotdot =
        call_file_name_all_completions(vec![Value::string(".."), Value::string(&dir_str)]).unwrap();
    let dotdot_items = list_to_vec(&dotdot).unwrap();
    assert_eq!(dotdot_items.len(), 1);
    assert_eq!(dotdot_items[0].as_str(), Some("../"));

    let slash =
        call_file_name_all_completions(vec![Value::string("./"), Value::string(&dir_str)]).unwrap();
    assert!(slash.is_nil());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_file_name_all_completions_eval_relative_directory() {
    crate::test_utils::init_test_tracing();
    let (base, base_str) = make_test_dir("fnac_eval_relative");
    let fixture_dir = base.join("fixtures");
    fs::create_dir(&fixture_dir).unwrap();
    create_file(&fixture_dir, "alpha.txt", "");
    fs::create_dir(fixture_dir.join("subdir")).unwrap();

    let mut eval = Context::new();
    eval.obarray.set_symbol_value(
        "default-directory",
        Value::string(ensure_trailing_slash(&base_str)),
    );

    let result = builtin_file_name_all_completions(
        &mut eval,
        vec![Value::string("sub"), Value::string("fixtures/")],
    )
    .unwrap();
    let items = list_to_vec(&result).unwrap();
    let names: Vec<&str> = items.iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(names, vec!["subdir/"]);

    let _ = fs::remove_dir_all(&base);
}

// -----------------------------------------------------------------------
// file-attributes
// -----------------------------------------------------------------------

#[test]
fn test_file_attributes_regular_file() {
    crate::test_utils::init_test_tracing();
    let (dir, _dir_str) = make_test_dir("fa_reg");
    let path = dir.join("test.txt");
    let path_str = path.to_string_lossy().to_string();
    create_file(&dir, "test.txt", "hello");

    let result = call_file_attributes(vec![Value::string(&path_str)]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 12);

    // TYPE should be nil for regular file.
    assert!(items[0].is_nil());
    // SIZE should be 5.
    assert_eq!(items[7].as_int(), Some(5));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_file_attributes_directory() {
    crate::test_utils::init_test_tracing();
    let (dir, _dir_str) = make_test_dir("fa_dir");
    let sub = dir.join("subdir");
    fs::create_dir_all(&sub).unwrap();
    let sub_str = sub.to_string_lossy().to_string();

    let result = call_file_attributes(vec![Value::string(&sub_str)]).unwrap();
    let items = list_to_vec(&result).unwrap();

    // TYPE should be t for directory.
    assert!(items[0].is_t());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_file_attributes_nonexistent() {
    crate::test_utils::init_test_tracing();
    let result = call_file_attributes(vec![Value::string("/nonexistent_file_xyz_99999")]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn test_file_attributes_time_tuple_shape_and_gid_changep() {
    crate::test_utils::init_test_tracing();
    let (dir, _) = make_test_dir("fa_time");
    let path = dir.join("time.txt");
    let path_str = path.to_string_lossy().to_string();
    create_file(&dir, "time.txt", "hello");

    let result = call_file_attributes(vec![Value::string(&path_str)]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 12);

    for idx in [4usize, 5usize, 6usize] {
        let tm = list_to_vec(&items[idx]).expect("time tuple must be a list");
        assert_eq!(tm.len(), 4);
        assert!(tm.iter().all(|v| v.is_fixnum()));
    }

    // Emacs commonly reports non-nil here on Unix.
    #[cfg(unix)]
    assert!(items[9].is_truthy());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_file_attributes_id_format_string() {
    crate::test_utils::init_test_tracing();
    let (dir, _) = make_test_dir("fa_idfmt");
    let path = dir.join("idtest.txt");
    let path_str = path.to_string_lossy().to_string();
    create_file(&dir, "idtest.txt", "x");

    let result =
        call_file_attributes(vec![Value::string(&path_str), Value::symbol("string")]).unwrap();
    let items = list_to_vec(&result).unwrap();
    // UID (index 2) should be a string.
    assert!(items[2].is_string());
    // GID (index 3) should be a string.
    assert!(items[3].is_string());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_file_attributes_eval_respects_default_directory() {
    crate::test_utils::init_test_tracing();
    let (dir, dir_str) = make_test_dir("fa_eval");
    create_file(&dir, "alpha.txt", "x");

    let mut eval = Context::new();
    eval.obarray.set_symbol_value(
        "default-directory",
        Value::string(ensure_trailing_slash(&dir_str)),
    );

    let result = builtin_file_attributes(&mut eval, vec![Value::string("alpha.txt")]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 12);
    assert!(items[0].is_nil());
    assert_eq!(items[7].as_int(), Some(1));

    let _ = fs::remove_dir_all(&dir);
}

// -----------------------------------------------------------------------
// file-attributes-lessp
// -----------------------------------------------------------------------

#[test]
fn test_file_attributes_lessp() {
    crate::test_utils::init_test_tracing();
    let f1 = Value::cons(Value::string("alpha.txt"), Value::NIL);
    let f2 = Value::cons(Value::string("beta.txt"), Value::NIL);

    let result = builtin_file_attributes_lessp(vec![f1, f2]).unwrap();
    assert!(result.is_truthy());

    let result = builtin_file_attributes_lessp(vec![f2, f1]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn test_file_attributes_lessp_equal() {
    crate::test_utils::init_test_tracing();
    let f1 = Value::cons(Value::string("same.txt"), Value::NIL);
    let f2 = Value::cons(Value::string("same.txt"), Value::NIL);

    let result = builtin_file_attributes_lessp(vec![f1, f2]).unwrap();
    assert!(result.is_nil()); // not less than
}

// -----------------------------------------------------------------------
// system-users / system-groups
// -----------------------------------------------------------------------

#[test]
fn test_system_users() {
    crate::test_utils::init_test_tracing();
    let result = builtin_system_users(vec![]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert!(!items.is_empty());
    assert!(items[0].is_string());
}

#[test]
fn test_system_groups_ignores_override_path() {
    crate::test_utils::init_test_tracing();
    let baseline = builtin_system_groups(vec![]).unwrap();
    let baseline_names = list_to_vec(&baseline).unwrap();

    let dir = std::env::temp_dir().join("neovm_group_override");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("group");
    fs::write(&file, "tmpgroup:x:0:\n").unwrap();
    unsafe { std::env::set_var("NEOVM_GROUP_PATH", &file) };
    let result = builtin_system_groups(vec![]).unwrap();
    let names = list_to_vec(&result).unwrap();
    assert_eq!(names, baseline_names);
    unsafe { std::env::remove_var("NEOVM_GROUP_PATH") };
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_system_groups() {
    crate::test_utils::init_test_tracing();
    let result = builtin_system_groups(vec![]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert!(!items.is_empty());
    assert!(items[0].is_string());
}

#[test]
fn test_system_users_ignores_override_path() {
    crate::test_utils::init_test_tracing();
    let baseline = builtin_system_users(vec![]).unwrap();
    let baseline_names = list_to_vec(&baseline).unwrap();

    let dir = std::env::temp_dir().join("neovm_passwd_override");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("passwd");
    fs::write(&file, "tmpuser:x:0:0::/tmp:/bin/sh\n").unwrap();
    unsafe { std::env::set_var("NEOVM_PASSWD_PATH", &file) };
    let result = builtin_system_users(vec![]).unwrap();
    let names = list_to_vec(&result).unwrap();
    assert_eq!(names, baseline_names);
    unsafe { std::env::remove_var("NEOVM_PASSWD_PATH") };
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_parse_colon_file_names_reverses_order() {
    crate::test_utils::init_test_tracing();
    let parsed = parse_colon_file_names(
        "root:x:0:0:root:/root:/bin/sh\nexec:x:1000:1000::/home/exec:/bin/sh\n",
    );
    assert_eq!(parsed, vec!["exec".to_string(), "root".to_string()]);
}

#[test]
fn test_parse_colon_file_names_skips_comments_and_blanks() {
    crate::test_utils::init_test_tracing();
    let parsed = parse_colon_file_names("\n# comment\nuser1:x:1000\n\nuser2:x:1001\n");
    assert_eq!(parsed, vec!["user2".to_string(), "user1".to_string()]);
}

#[test]
fn test_parse_colon_file_names_repels_malformed_entries() {
    crate::test_utils::init_test_tracing();
    let parsed = parse_colon_file_names("nocolon\n:empty\nvalid:x:1000\n");
    assert_eq!(parsed, vec!["valid".to_string()]);
}

#[test]
fn test_parse_colon_file_names_trims_spaces() {
    crate::test_utils::init_test_tracing();
    let parsed = parse_colon_file_names("  spaced :x:0:0\nnormal:x:0:0\n");
    assert_eq!(parsed, vec!["normal".to_string(), "spaced".to_string()]);
}

#[test]
fn test_parse_colon_file_names_handles_crlf_lines() {
    crate::test_utils::init_test_tracing();
    let parsed = parse_colon_file_names("first:x:0:0\r\nsecond:x:0:0\r\n");
    assert_eq!(parsed, vec!["second".to_string(), "first".to_string()]);
}

#[test]
fn test_read_colon_file_names_reads_file() {
    crate::test_utils::init_test_tracing();
    let dir = std::env::temp_dir();
    let path = dir.join("neovm_dired_users.txt");
    let _ = fs::remove_file(&path);
    fs::write(
        &path,
        "alpha:x:1000:1000::/home/alpha:/bin/sh\nbeta:x:1001:1001::/home/beta:/bin/sh\n",
    )
    .unwrap();

    let names = read_colon_file_names(&path.to_string_lossy());
    assert_eq!(names, vec!["beta".to_string(), "alpha".to_string()]);

    let _ = fs::remove_file(&path);
}

#[test]
fn test_read_colon_file_names_missing_file_returns_empty() {
    crate::test_utils::init_test_tracing();
    let dir = std::env::temp_dir();
    let path = dir.join("neovm_dired_missing.txt");
    let _ = fs::remove_file(&path);
    let names = read_colon_file_names(&path.to_string_lossy());
    assert!(names.is_empty());
}

// -----------------------------------------------------------------------
// Argument validation
// -----------------------------------------------------------------------

#[test]
fn test_directory_files_and_attributes_wrong_args() {
    crate::test_utils::init_test_tracing();
    // No args.
    assert!(call_directory_files_and_attributes(vec![]).is_err());
    // Too many args.
    assert!(
        call_directory_files_and_attributes(vec![
            Value::string("/tmp"),
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::NIL, // 7th arg
        ])
        .is_err()
    );
}

#[test]
fn test_file_attributes_wrong_args() {
    crate::test_utils::init_test_tracing();
    assert!(call_file_attributes(vec![]).is_err());
    assert!(call_file_attributes(vec![Value::string("/tmp"), Value::NIL, Value::NIL,]).is_err());
}

#[test]
fn test_system_users_wrong_args() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_system_users(vec![Value::NIL]).is_err());
}

#[test]
fn test_system_groups_wrong_args() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_system_groups(vec![Value::NIL]).is_err());
}

// -----------------------------------------------------------------------
// Time helper
// -----------------------------------------------------------------------

#[test]
fn test_time_to_emacs_tuple() {
    crate::test_utils::init_test_tracing();
    let val = time_to_emacs_tuple(1_234_567_890, 123_456_789);
    let items = list_to_vec(&val).unwrap();
    assert_eq!(items.len(), 4);
    let high = items[0].as_int().unwrap();
    let low = items[1].as_int().unwrap();
    assert_eq!((high << 16) | low, 1_234_567_890);
    assert_eq!(items[2].as_int(), Some(123_456));
    assert_eq!(items[3].as_int(), Some(789_000));
}

#[cfg(unix)]
#[test]
fn test_format_mode_string() {
    crate::test_utils::init_test_tracing();
    // Regular file with 0o644.
    let dir = std::env::temp_dir().join("neovm_mode_test");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("modefile.txt");
    {
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(b"test").unwrap();
    }
    let meta = fs::symlink_metadata(&path).unwrap();
    let mode_str = format_mode_string(0o100644, &meta);
    assert_eq!(&mode_str[0..1], "-");
    assert_eq!(&mode_str[1..4], "rw-");
    assert_eq!(&mode_str[4..7], "r--");
    assert_eq!(&mode_str[7..10], "r--");

    let _ = fs::remove_dir_all(&dir);
}
