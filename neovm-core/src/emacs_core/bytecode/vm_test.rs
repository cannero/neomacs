use super::*;
use crate::emacs_core::bytecode::compiler::Compiler;
use crate::emacs_core::coding::CodingSystemManager;
use crate::emacs_core::custom::CustomManager;
use crate::emacs_core::parse_forms;
use crate::emacs_core::value::HashTableTest;
use crate::window::FrameManager;

fn vm_eval(src: &str) -> Result<Value, EvalError> {
    let forms = parse_forms(src).expect("parse");
    let mut compiler = Compiler::new(false);
    let mut obarray = Obarray::new();
    crate::emacs_core::errors::init_standard_errors(&mut obarray);
    // Set up standard variables
    obarray.set_symbol_value("most-positive-fixnum", Value::Int(i64::MAX >> 2));
    obarray.set_symbol_value("most-negative-fixnum", Value::Int(-(i64::MAX >> 2) - 1));

    let mut dynamic: Vec<OrderedSymMap> = Vec::new();
    let mut lexenv: Value = Value::Nil;
    let mut features: Vec<SymId> = Vec::new();
    let mut custom = CustomManager::new();
    let mut buffers = crate::buffer::BufferManager::new();
    let mut frames = FrameManager::new();
    let mut coding_systems = CodingSystemManager::new();
    let mut match_data: Option<MatchData> = None;
    let mut watchers = VariableWatcherList::new();
    let mut catch_tags: Vec<Value> = Vec::new();

    let mut last = Value::Nil;
    for form in &forms {
        let func = compiler.compile_toplevel(form);
        let mut vm = Vm::new(
            &mut obarray,
            &mut dynamic,
            &mut lexenv,
            &mut features,
            &mut custom,
            &mut buffers,
            &mut frames,
            &mut coding_systems,
            &mut match_data,
            &mut watchers,
            &mut catch_tags,
        );
        last = vm.execute(&func, vec![]).map_err(map_flow)?;
    }
    Ok(last)
}

fn vm_eval_str(src: &str) -> String {
    match vm_eval(src) {
        Ok(val) => format!("OK {}", val),
        Err(e) => format!("ERR {:?}", e),
    }
}

#[test]
fn vm_literal_int() {
    assert_eq!(vm_eval_str("42"), "OK 42");
}

#[test]
fn vm_nil_t() {
    assert_eq!(vm_eval_str("nil"), "OK nil");
    assert_eq!(vm_eval_str("t"), "OK t");
}

#[test]
fn vm_eval_preserves_variable_watcher_registry_across_builtin_dispatch() {
    assert_eq!(
        vm_eval_str(
            "(progn (add-variable-watcher 'vm-bytecode-var 'vm-bytecode-watch) (get-variable-watchers 'vm-bytecode-var))"
        ),
        "OK (vm-bytecode-watch)"
    );
}

#[test]
fn vm_varset_triggers_variable_watcher_callbacks() {
    assert_eq!(
        vm_eval_str(
            "(progn
               (fset 'vm-bytecode-watch
                 (lambda (sym new op where)
                   (setq vm-bytecode-watch-op op)
                   (setq vm-bytecode-watch-val new)
                   new))
               (add-variable-watcher 'vm-bytecode-target 'vm-bytecode-watch)
               (setq vm-bytecode-target 19)
               (list vm-bytecode-watch-val vm-bytecode-watch-op))"
        ),
        "OK (19 set)"
    );
}

#[test]
fn vm_varbind_and_unbind_trigger_variable_watcher_callbacks() {
    assert_eq!(
        vm_eval_str(
            "(progn
               (setq vm-watch-events nil)
               (setq vm-watch-target 9)
               (fset 'vm-watch-rec
                 (lambda (sym new op where)
                   (setq vm-watch-events (cons (list op new) vm-watch-events))))
               (add-variable-watcher 'vm-watch-target 'vm-watch-rec)
               (let ((vm-watch-target 1)) 'done)
               vm-watch-events)"
        ),
        "OK ((unlet 9) (let 1))"
    );

    assert_eq!(
        vm_eval_str(
            "(progn
               (setq vm-watch-events nil)
               (setq vm-watch-target 9)
               (fset 'vm-watch-rec
                 (lambda (sym new op where)
                   (setq vm-watch-events (cons (list op new) vm-watch-events))))
               (add-variable-watcher 'vm-watch-target 'vm-watch-rec)
               (let* ((vm-watch-target 2)) 'done)
               vm-watch-events)"
        ),
        "OK ((unlet 9) (let 2))"
    );
}

#[test]
fn vm_addition() {
    assert_eq!(vm_eval_str("(+ 1 2)"), "OK 3");
    assert_eq!(vm_eval_str("(+ 1 2 3)"), "OK 6");
}

#[test]
fn vm_subtraction() {
    assert_eq!(vm_eval_str("(- 10 3)"), "OK 7");
    assert_eq!(vm_eval_str("(- 5)"), "OK -5");
}

#[test]
fn vm_multiplication() {
    assert_eq!(vm_eval_str("(* 4 5)"), "OK 20");
}

#[test]
fn vm_division() {
    assert_eq!(vm_eval_str("(/ 10 3)"), "OK 3");
}

#[test]
fn vm_comparisons() {
    assert_eq!(vm_eval_str("(< 1 2)"), "OK t");
    assert_eq!(vm_eval_str("(> 1 2)"), "OK nil");
    assert_eq!(vm_eval_str("(= 3 3)"), "OK t");
    assert_eq!(vm_eval_str("(<= 3 3)"), "OK t");
    assert_eq!(vm_eval_str("(>= 5 3)"), "OK t");
}

#[test]
fn vm_if() {
    assert_eq!(vm_eval_str("(if t 1 2)"), "OK 1");
    assert_eq!(vm_eval_str("(if nil 1 2)"), "OK 2");
    assert_eq!(vm_eval_str("(if nil 1)"), "OK nil");
}

#[test]
fn vm_and_or() {
    assert_eq!(vm_eval_str("(and 1 2 3)"), "OK 3");
    assert_eq!(vm_eval_str("(and 1 nil 3)"), "OK nil");
    assert_eq!(vm_eval_str("(or nil nil 3)"), "OK 3");
    assert_eq!(vm_eval_str("(or nil nil)"), "OK nil");
}

#[test]
fn vm_let() {
    assert_eq!(vm_eval_str("(let ((x 42)) x)"), "OK 42");
    assert_eq!(vm_eval_str("(let ((x 1) (y 2)) (+ x y))"), "OK 3");
}

#[test]
fn vm_let_star() {
    assert_eq!(vm_eval_str("(let* ((x 1) (y (+ x 1))) y)"), "OK 2");
}

#[test]
fn vm_setq() {
    assert_eq!(vm_eval_str("(progn (setq x 42) x)"), "OK 42");
}

#[test]
fn vm_while_loop() {
    assert_eq!(
        vm_eval_str("(let ((x 0)) (while (< x 5) (setq x (1+ x))) x)"),
        "OK 5"
    );
}

#[test]
fn vm_progn() {
    assert_eq!(vm_eval_str("(progn 1 2 3)"), "OK 3");
}

#[test]
fn vm_prog1() {
    assert_eq!(vm_eval_str("(prog1 1 2 3)"), "OK 1");
}

#[test]
fn vm_quote() {
    assert_eq!(vm_eval_str("'foo"), "OK foo");
    assert_eq!(vm_eval_str("'(1 2 3)"), "OK (1 2 3)");
}

#[test]
fn vm_type_predicates() {
    assert_eq!(vm_eval_str("(null nil)"), "OK t");
    assert_eq!(vm_eval_str("(null 1)"), "OK nil");
    assert_eq!(vm_eval_str("(consp '(1 2))"), "OK t");
    assert_eq!(vm_eval_str("(integerp 42)"), "OK t");
    assert_eq!(vm_eval_str("(stringp \"hello\")"), "OK t");
}

#[test]
fn vm_list_ops() {
    assert_eq!(vm_eval_str("(car '(1 2 3))"), "OK 1");
    assert_eq!(vm_eval_str("(cdr '(1 2 3))"), "OK (2 3)");
    assert_eq!(vm_eval_str("(cons 1 '(2 3))"), "OK (1 2 3)");
    assert_eq!(vm_eval_str("(length '(1 2 3))"), "OK 3");
    assert_eq!(vm_eval_str("(list 1 2 3)"), "OK (1 2 3)");
}

#[test]
fn vm_eq_equal() {
    assert_eq!(vm_eval_str("(eq 'foo 'foo)"), "OK t");
    assert_eq!(vm_eval_str("(equal '(1 2) '(1 2))"), "OK t");
}

#[test]
fn vm_concat() {
    assert_eq!(
        vm_eval_str(r#"(concat "hello" " " "world")"#),
        r#"OK "hello world""#
    );
}

#[test]
fn vm_switch_branches_using_hash_table_jump_table() {
    let table = Value::hash_table(HashTableTest::Eq);
    let Value::HashTable(table_id) = table else {
        panic!("expected hash table constant");
    };
    crate::emacs_core::value::with_heap_mut(|heap| {
        let ht = heap.get_hash_table_mut(table_id);
        let key = Value::symbol("foo").to_hash_key(&ht.test);
        ht.data.insert(key.clone(), Value::Int(8));
        ht.key_snapshots.insert(key.clone(), Value::symbol("foo"));
        ht.insertion_order.push(key);
    });

    let func = ByteCodeFunction {
        ops: vec![
            Op::Constant(1),
            Op::Constant(0),
            Op::Switch,
            Op::Constant(2),
            Op::Return,
            Op::Constant(3),
            Op::Return,
        ],
        constants: vec![table, Value::symbol("foo"), Value::Int(10), Value::Int(20)],
        max_stack: 2,
        params: crate::emacs_core::value::LambdaParams::simple(vec![]),
        env: None,
        gnu_byte_offset_map: Some(std::collections::HashMap::from([(8usize, 5usize)])),
        docstring: None,
        doc_form: None,
    };

    let mut obarray = Obarray::new();
    let mut dynamic: Vec<OrderedSymMap> = Vec::new();
    let mut lexenv: Value = Value::Nil;
    let mut features: Vec<SymId> = Vec::new();
    let mut custom = CustomManager::new();
    let mut buffers = crate::buffer::BufferManager::new();
    let mut frames = FrameManager::new();
    let mut coding_systems = CodingSystemManager::new();
    let mut match_data: Option<MatchData> = None;
    let mut watchers = VariableWatcherList::new();
    let mut catch_tags: Vec<Value> = Vec::new();

    let mut vm = Vm::new(
        &mut obarray,
        &mut dynamic,
        &mut lexenv,
        &mut features,
        &mut custom,
        &mut buffers,
        &mut frames,
        &mut coding_systems,
        &mut match_data,
        &mut watchers,
        &mut catch_tags,
    );
    let result = vm.execute(&func, vec![]).expect("vm switch should execute");
    assert_eq!(result, Value::Int(20));
}

#[test]
fn vm_condition_case_catches_signal_and_binds_error() {
    assert_eq!(
        vm_eval_str("(condition-case err missing-vm-var (error err))"),
        "OK (void-variable missing-vm-var)"
    );
}

#[test]
fn vm_catch_returns_thrown_value() {
    assert_eq!(vm_eval_str("(catch 'done (throw 'done 99))"), "OK 99");
}

#[test]
fn vm_define_charset_alias_survives_eval_builtin_bridge() {
    assert_eq!(
        vm_eval_str(
            "(progn
               (define-charset-internal
                 'vm-gbk
                 2
                 [#x40 #xFE #x81 #xFE 0 0 0 0]
                 nil nil nil nil nil nil nil nil
                 #x160000
                 nil nil nil nil
                 '(:name vm-gbk :docstring \"VM GBK\"))
               (mapcar 'list '(1 2 3))
               (define-charset-alias 'vm-gbk-alias 'vm-gbk)
               (list (charsetp 'vm-gbk) (charsetp 'vm-gbk-alias)))"
        ),
        "OK (t t)"
    );
}

#[test]
fn vm_define_coding_system_alias_survives_eval_builtin_bridge() {
    assert_eq!(
        vm_eval_str(
            "(progn
               (apply #'define-coding-system-internal
                      '(vm-utf8-emacs
                        85
                        utf-8
                        (unicode)
                        t
                        nil
                        nil
                        nil
                        nil
                        nil
                        nil
                        (:name vm-utf8-emacs :docstring \"VM UTF-8 Emacs\")
                        nil))
               (define-coding-system-alias 'vm-emacs-internal 'vm-utf8-emacs-unix)
               (list (coding-system-p 'vm-utf8-emacs-unix)
                     (coding-system-p 'vm-emacs-internal)))"
        ),
        "OK (t t)"
    );
}

#[test]
fn vm_roots_bytecode_constants_across_gc_during_eval_builtin_dispatch() {
    assert_eq!(
        vm_eval_str(
            "(let ((map (make-sparse-keymap)))
               (garbage-collect)
               (define-key map [97] 'ignore)
               (lookup-key map [97]))"
        ),
        "OK ignore"
    );
}

#[test]
fn vm_throw_restores_saved_stack_before_resuming_catch() {
    let func = ByteCodeFunction {
        ops: vec![
            Op::Constant(0),
            Op::Constant(1),
            Op::PushCatch(6),
            Op::Constant(1),
            Op::Constant(2),
            Op::Throw,
            Op::List(2),
            Op::Return,
        ],
        constants: vec![Value::Int(42), Value::symbol("done"), Value::Int(99)],
        max_stack: 3,
        params: crate::emacs_core::value::LambdaParams::simple(vec![]),
        env: None,
        gnu_byte_offset_map: None,
        docstring: None,
        doc_form: None,
    };

    let mut obarray = Obarray::new();
    crate::emacs_core::errors::init_standard_errors(&mut obarray);
    let mut dynamic: Vec<OrderedSymMap> = Vec::new();
    let mut lexenv: Value = Value::Nil;
    let mut features: Vec<SymId> = Vec::new();
    let mut custom = CustomManager::new();
    let mut buffers = crate::buffer::BufferManager::new();
    let mut frames = FrameManager::new();
    let mut coding_systems = CodingSystemManager::new();
    let mut match_data: Option<MatchData> = None;
    let mut watchers = VariableWatcherList::new();
    let mut catch_tags: Vec<Value> = Vec::new();

    let mut vm = Vm::new(
        &mut obarray,
        &mut dynamic,
        &mut lexenv,
        &mut features,
        &mut custom,
        &mut buffers,
        &mut frames,
        &mut coding_systems,
        &mut match_data,
        &mut watchers,
        &mut catch_tags,
    );

    let result = vm.execute(&func, vec![]).expect("vm catch should execute");
    assert_eq!(result, Value::list(vec![Value::Int(42), Value::Int(99)]));
}

#[test]
fn vm_eval_bridge_preserves_frames_across_eval_dependent_builtins() {
    assert_eq!(
        vm_eval_str("(frame-parameter (selected-frame) 'width)"),
        "OK 80"
    );
}

#[test]
fn vm_string_match_updates_match_data_for_followup_builtins() {
    assert_eq!(
        vm_eval_str(
            "(progn
               (string-match \"a\\\\(b\\\\)\" \"zabz\")
               (list (match-beginning 0)
                     (match-beginning 1)
                     (match-end 1)
                     (match-data)))"
        ),
        "OK (1 2 3 (1 3 2 3))"
    );
}

#[test]
fn vm_when_unless() {
    assert_eq!(vm_eval_str("(when t 1 2 3)"), "OK 3");
    assert_eq!(vm_eval_str("(when nil 1 2 3)"), "OK nil");
    assert_eq!(vm_eval_str("(unless nil 1 2 3)"), "OK 3");
    assert_eq!(vm_eval_str("(unless t 1 2 3)"), "OK nil");
}

#[test]
fn vm_cond() {
    assert_eq!(vm_eval_str("(cond (nil 1) (t 2))"), "OK 2");
    assert_eq!(vm_eval_str("(cond (nil 1) (nil 2))"), "OK nil");
}

#[test]
fn vm_nested_let() {
    assert_eq!(vm_eval_str("(let ((x 1)) (let ((y 2)) (+ x y)))"), "OK 3");
}

#[test]
fn vm_vector_ops() {
    assert_eq!(vm_eval_str("(aref [10 20 30] 1)"), "OK 20");
    assert_eq!(vm_eval_str("(length [1 2 3])"), "OK 3");
}

#[test]
fn vm_aset_string_writeback() {
    assert_eq!(
        vm_eval_str("(let ((s (copy-sequence \"abc\"))) (aset s 1 ?x) s)"),
        r#"OK "axc""#
    );
}

#[test]
fn vm_fillarray_string_writeback() {
    assert_eq!(
        vm_eval_str("(let ((s (copy-sequence \"abc\"))) (fillarray s ?y) s)"),
        r#"OK "yyy""#
    );
}

#[test]
fn vm_aref_aset_error_parity() {
    let aref_err = vm_eval("(aref [10 20 30] -1)").expect_err("aref should reject -1");
    match aref_err {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "args-out-of-range");
            assert_eq!(
                data,
                vec![
                    Value::vector(vec![Value::Int(10), Value::Int(20), Value::Int(30)]),
                    Value::Int(-1)
                ]
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let aset_err = vm_eval("(aset [10 20 30] -1 99)").expect_err("aset should reject -1");
    match aset_err {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "args-out-of-range");
            assert_eq!(
                data,
                vec![
                    Value::vector(vec![Value::Int(10), Value::Int(20), Value::Int(30)]),
                    Value::Int(-1)
                ]
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let string_aset_err =
        vm_eval("(aset \"abc\" 1 nil)").expect_err("aset string should validate character");
    match string_aset_err {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("characterp"), Value::Nil]);
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn vm_builtin_wrong_arity_uses_subr_payload() {
    let zero_arity = vm_eval("(car)").expect_err("car with 0 args must signal");
    match zero_arity {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "wrong-number-of-arguments");
            assert_eq!(data, vec![Value::Subr(intern("car")), Value::Int(0)]);
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let two_arity = vm_eval("(car 1 2)").expect_err("car with 2 args must signal");
    match two_arity {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "wrong-number-of-arguments");
            assert_eq!(data, vec![Value::Subr(intern("car")), Value::Int(2)]);
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn vm_string_compare_type_errors_match_oracle() {
    let string_equal_err = vm_eval("(string= \"ab\" 1)").expect_err("string= must type-check");
    match string_equal_err {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("stringp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let string_lessp_err =
        vm_eval("(string-lessp \"ab\" 1)").expect_err("string-lessp must type-check");
    match string_lessp_err {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("stringp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn vm_list_lookup_type_errors_match_oracle() {
    let car_err = vm_eval("(car 1)").expect_err("car must type-check list");
    match car_err {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("listp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let cdr_err = vm_eval("(cdr 1)").expect_err("cdr must type-check list");
    match cdr_err {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("listp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    }

    assert_eq!(
        vm_eval("(car-safe 1)").expect("car-safe should be nil"),
        Value::Nil
    );
    assert_eq!(
        vm_eval("(cdr-safe 1)").expect("cdr-safe should be nil"),
        Value::Nil
    );

    let nth_int_err = vm_eval("(nth 'a '(1 2 3))").expect_err("nth must type-check index");
    match nth_int_err {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("integerp"), Value::symbol("a")]);
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let nth_list_err = vm_eval("(nth 1 1)").expect_err("nth must type-check list");
    match nth_list_err {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("listp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let nthcdr_int_err = vm_eval("(nthcdr 'a '(1 2 3))").expect_err("nthcdr must type-check index");
    match nthcdr_int_err {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("integerp"), Value::symbol("a")]);
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let nthcdr_list_err = vm_eval("(nthcdr 1 1)").expect_err("nthcdr must type-check list");
    match nthcdr_list_err {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("listp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let memq_err = vm_eval("(memq 'a 1)").expect_err("memq must type-check list");
    match memq_err {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("listp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let assq_err = vm_eval("(assq 'a 1)").expect_err("assq must type-check alist");
    match assq_err {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("listp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn vm_length_and_symbol_access_type_errors_match_oracle() {
    let dotted_length_err =
        vm_eval("(length '(1 . 2))").expect_err("length must reject dotted lists");
    match dotted_length_err {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("listp"), Value::Int(2)]);
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let symbol_value_err =
        vm_eval("(symbol-value 1)").expect_err("symbol-value must type-check symbols");
    match symbol_value_err {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("symbolp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let symbol_function_err =
        vm_eval("(symbol-function 1)").expect_err("symbol-function must type-check symbols");
    match symbol_function_err {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("symbolp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn vm_symbol_mutator_type_errors_match_oracle() {
    let set_err = vm_eval("(set 1 2)").expect_err("set must type-check symbols");
    match set_err {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("symbolp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let fset_err = vm_eval("(fset 1 2)").expect_err("fset must type-check symbols");
    match fset_err {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("symbolp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let get_err = vm_eval("(get 1 'p)").expect_err("get must type-check symbols");
    match get_err {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("symbolp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let put_err = vm_eval("(put 1 'p 2)").expect_err("put must type-check first argument");
    match put_err {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("symbolp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn vm_not_negation() {
    assert_eq!(vm_eval_str("(/= 1 2)"), "OK t");
    assert_eq!(vm_eval_str("(/= 1 1)"), "OK nil");
}

#[test]
fn vm_float_arithmetic() {
    assert_eq!(vm_eval_str("(+ 1.0 2.0)"), "OK 3.0");
    assert_eq!(vm_eval_str("(+ 1 2.0)"), "OK 3.0");
}

#[test]
fn vm_dotimes() {
    assert_eq!(
        vm_eval_str("(let ((sum 0)) (dotimes (i 5) (setq sum (+ sum i))) sum)"),
        "OK 10"
    );
}

#[test]
fn vm_dolist() {
    assert_eq!(
        vm_eval_str(
            "(let ((result nil)) (dolist (x '(a b c)) (setq result (cons x result))) result)"
        ),
        "OK (c b a)"
    );
}
