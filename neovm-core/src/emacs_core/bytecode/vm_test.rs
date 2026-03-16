use super::*;
use crate::emacs_core::bytecode::compiler::Compiler;
use crate::emacs_core::eval::{Evaluator, VmSharedState};
use crate::emacs_core::parse_forms;
use crate::emacs_core::value::HashTableTest;
use crate::window::SplitDirection;
use std::path::PathBuf;

fn new_vm(eval: &mut Evaluator) -> Vm<'_> {
    Vm::new(VmSharedState::from_evaluator(eval))
}

fn with_vm_eval<R>(src: &str, lexical: bool, f: impl FnOnce(Result<Value, EvalError>) -> R) -> R {
    let mut eval = Evaluator::new_vm_harness();
    eval.set_lexical_binding(lexical);
    let forms = parse_forms(src).expect("parse");
    let mut compiler = Compiler::new(lexical);

    let mut last = Value::Nil;
    for form in &forms {
        let func = compiler.compile_toplevel(form);
        let mut vm = new_vm(&mut eval);
        match vm.execute(&func, vec![]) {
            Ok(value) => last = value,
            Err(flow) => return f(Err(map_flow(flow))),
        }
    }
    f(Ok(last))
}

fn vm_eval_str(src: &str) -> String {
    with_vm_eval(src, false, |result| {
        crate::emacs_core::error::format_eval_result(&result)
    })
}

fn vm_eval_lexical_str(src: &str) -> String {
    with_vm_eval(src, true, |result| {
        crate::emacs_core::error::format_eval_result(&result)
    })
}

fn vm_eval_with_init_str(src: &str, init: impl FnOnce(&mut Evaluator)) -> String {
    let mut eval = Evaluator::new_vm_harness();
    init(&mut eval);
    let forms = parse_forms(src).expect("parse");
    let mut compiler = Compiler::new(false);

    let mut last = Value::Nil;
    for form in &forms {
        let func = compiler.compile_toplevel(form);
        let mut vm = new_vm(&mut eval);
        match vm.execute(&func, vec![]) {
            Ok(value) => last = value,
            Err(flow) => {
                return crate::emacs_core::error::format_eval_result(&Err(map_flow(flow)));
            }
        }
    }
    crate::emacs_core::error::format_eval_result(&Ok(last))
}

#[test]
fn vm_lexical_let_closure_captures_bytecode_binding() {
    assert_eq!(
        vm_eval_lexical_str(
            r#"
(funcall
 (let ((x 42))
   (lambda () x)))
"#,
        ),
        "OK 42"
    );
}

#[test]
fn vm_lexical_param_closure_captures_bytecode_binding() {
    assert_eq!(
        vm_eval_lexical_str(
            r#"
(funcall
 ((lambda (x)
    (lambda () x))
  42))
"#,
        ),
        "OK 42"
    );
}

#[test]
fn vm_interpreted_lambda_call_restores_outer_binding_state() {
    assert_eq!(
        vm_eval_str("(let ((x 41)) (list (funcall (lambda (x) x) 7) x))"),
        "OK (7 41)"
    );
    assert_eq!(
        vm_eval_lexical_str("(let ((x 41)) (list (funcall (lambda (x) x) 7) x))"),
        "OK (7 41)"
    );
}

fn execute_manual_vm<T>(
    mut func: ByteCodeFunction,
    init: impl FnOnce(&mut ByteCodeFunction, &mut crate::buffer::BufferManager) -> T,
) -> (Value, crate::buffer::BufferManager, T) {
    let mut eval = Evaluator::new_vm_harness();
    let init_state = init(&mut func, &mut eval.buffers);

    let result = {
        let mut vm = new_vm(&mut eval);
        vm.execute(&func, vec![])
            .expect("manual bytecode should execute")
    };

    let buffers = std::mem::replace(&mut eval.buffers, crate::buffer::BufferManager::new());
    (result, buffers, init_state)
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
fn vm_declared_special_ignores_lexical_lookup() {
    assert_eq!(
        vm_eval_lexical_str(
            "(progn
               (defvar vm-special 10)
               (let ((vm-special 20))
                 (let ((f (lambda () vm-special)))
                   (let ((vm-special 30))
                     (funcall f)))))"
        ),
        "OK 30"
    );
}

#[test]
fn vm_declared_special_setq_updates_dynamic_binding() {
    assert_eq!(
        vm_eval_lexical_str(
            "(progn
               (defvar vm-special 10)
               (let ((vm-special 20))
                 (let ((f (lambda () (setq vm-special (+ vm-special 1)))))
                   (let ((vm-special 30))
                     (funcall f)
                     vm-special))))"
        ),
        "OK 31"
    );
}

#[test]
fn vm_unbind_restores_saved_current_buffer() {
    let mut func = ByteCodeFunction::new(LambdaParams {
        required: vec![],
        optional: vec![],
        rest: None,
    });
    let other_buffer_idx = func.add_constant(Value::Nil);
    let set_buffer_idx = func.add_symbol("set-buffer");
    func.ops = vec![
        Op::SaveCurrentBuffer,
        Op::Constant(other_buffer_idx),
        Op::CallBuiltin(set_buffer_idx, 1),
        Op::Pop,
        Op::Unbind(1),
        Op::Nil,
        Op::Return,
    ];
    func.max_stack = 2;

    let (result, buffers, saved_buffer) = execute_manual_vm(func, |func, buffers| {
        let saved_buffer = buffers.create_buffer("saved");
        let other_buffer = buffers.create_buffer("other");
        func.constants[other_buffer_idx as usize] = Value::Buffer(other_buffer);
        buffers.set_current(saved_buffer);
        saved_buffer
    });

    assert_eq!(result, Value::Nil);
    assert_eq!(
        buffers.current_buffer().map(|buffer| buffer.id),
        Some(saved_buffer)
    );
}

#[test]
fn vm_unbind_counts_unwind_protect_entries_like_gnu() {
    let mut noop_func = ByteCodeFunction::new(LambdaParams {
        required: vec![],
        optional: vec![],
        rest: None,
    });
    noop_func.ops = vec![Op::Nil, Op::Return];
    noop_func.max_stack = 1;
    let noop = Value::make_bytecode(noop_func);

    let mut func = ByteCodeFunction::new(LambdaParams {
        required: vec![],
        optional: vec![],
        rest: None,
    });
    let a_idx = func.add_symbol("vm-up-a");
    let b_idx = func.add_symbol("vm-up-b");
    let a_val_idx = func.add_constant(Value::Int(7));
    let b_val_idx = func.add_constant(Value::Int(9));
    let cleanup_idx = func.add_constant(noop);
    func.ops = vec![
        Op::Constant(a_val_idx),
        Op::VarBind(a_idx),
        Op::Constant(b_val_idx),
        Op::VarBind(b_idx),
        Op::Constant(cleanup_idx),
        Op::UnwindProtectPop,
        Op::Unbind(1),
        Op::VarRef(b_idx),
        Op::Return,
    ];
    func.max_stack = 2;

    let (result, _buffers, _) = execute_manual_vm(func, |_func, _buffers| ());
    assert_eq!(result, Value::Int(9));
}

#[test]
fn vm_unbind_restores_saved_excursion_point() {
    let mut func = ByteCodeFunction::new(LambdaParams {
        required: vec![],
        optional: vec![],
        rest: None,
    });
    let goto_target_idx = func.add_constant(Value::Int(5));
    let goto_char_idx = func.add_symbol("goto-char");
    func.ops = vec![
        Op::SaveExcursion,
        Op::Constant(goto_target_idx),
        Op::CallBuiltin(goto_char_idx, 1),
        Op::Pop,
        Op::Unbind(1),
        Op::Nil,
        Op::Return,
    ];
    func.max_stack = 2;

    let (result, buffers, (buffer_id, saved_point)) = execute_manual_vm(func, |_func, buffers| {
        let buffer_id = buffers.create_buffer("excursion");
        buffers.set_current(buffer_id);
        {
            let buffer = buffers.get_mut(buffer_id).expect("buffer");
            buffer.insert("abcdef");
            buffer.goto_char(2);
        }
        let saved_point = buffers.get(buffer_id).expect("buffer").pt;
        (buffer_id, saved_point)
    });

    assert_eq!(result, Value::Nil);
    assert_eq!(
        buffers.current_buffer().map(|buffer| buffer.id),
        Some(buffer_id)
    );
    assert_eq!(buffers.get(buffer_id).expect("buffer").pt, saved_point);
}

#[test]
fn vm_unbind_restores_saved_restriction() {
    let mut func = ByteCodeFunction::new(LambdaParams {
        required: vec![],
        optional: vec![],
        rest: None,
    });
    let beg_idx = func.add_constant(Value::Int(2));
    let end_idx = func.add_constant(Value::Int(4));
    let narrow_idx = func.add_symbol("narrow-to-region");
    func.ops = vec![
        Op::SaveRestriction,
        Op::Constant(beg_idx),
        Op::Constant(end_idx),
        Op::CallBuiltin(narrow_idx, 2),
        Op::Pop,
        Op::Unbind(1),
        Op::Nil,
        Op::Return,
    ];
    func.max_stack = 3;

    let (result, buffers, (buffer_id, saved_begv, saved_zv)) =
        execute_manual_vm(func, |_func, buffers| {
            let buffer_id = buffers.create_buffer("restriction");
            buffers.set_current(buffer_id);
            {
                let buffer = buffers.get_mut(buffer_id).expect("buffer");
                buffer.insert("abcdef");
                buffer.narrow_to_byte_region(1, 5);
                buffer.goto_byte(3);
            }
            let buffer = buffers.get(buffer_id).expect("buffer");
            (buffer_id, buffer.begv, buffer.zv)
        });

    assert_eq!(result, Value::Nil);
    let buffer = buffers.get(buffer_id).expect("buffer");
    assert_eq!(buffer.begv, saved_begv);
    assert_eq!(buffer.zv, saved_zv);
}

#[test]
fn vm_eval_shared_runtime_path_preserves_active_catch_tags() {
    let mut eval = Evaluator::new_vm_harness();
    eval.catch_tags.push(Value::symbol("vm-bridge-catch"));
    let mut vm = new_vm(&mut eval);

    let throw_form = Value::list(vec![
        Value::symbol("throw"),
        Value::list(vec![
            Value::symbol("quote"),
            Value::symbol("vm-bridge-catch"),
        ]),
        Value::Int(7),
    ]);
    let result = vm.dispatch_vm_builtin("eval", vec![throw_form, Value::Nil]);

    assert!(matches!(
        result,
        Err(Flow::Throw { tag, value })
            if tag == Value::symbol("vm-bridge-catch") && value == Value::Int(7)
    ));
}

#[test]
fn vm_eval_with_explicit_lexenv_restores_outer_vm_lexenv() {
    assert_eq!(
        vm_eval_lexical_str("(let ((x 41)) (list (eval 'x '((x . 7))) x))"),
        "OK (7 41)"
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
    assert_eq!(vm_eval_str("[remap ignore]"), "OK [remap ignore]");
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
        lexical: false,
        env: None,
        gnu_byte_offset_map: Some(std::collections::HashMap::from([(8usize, 5usize)])),
        docstring: None,
        doc_form: None,
        interactive: None,
    };

    let mut eval = Evaluator::new_vm_harness();
    let mut vm = new_vm(&mut eval);
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
fn vm_define_coding_system_alias_uses_shared_runtime_manager() {
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
fn vm_coding_system_priority_and_terminal_state_use_shared_runtime_manager() {
    assert_eq!(
        vm_eval_str(
            "(progn
               (set-coding-system-priority 'raw-text 'utf-8)
               (set-terminal-coding-system 'raw-text)
               (list (car (coding-system-priority-list))
                     (terminal-coding-system)))"
        ),
        "OK (raw-text raw-text)"
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
fn vm_length_accepts_plain_bytecode_closure_shape() {
    let bc = Value::make_bytecode(crate::emacs_core::bytecode::ByteCodeFunction::new(
        crate::emacs_core::value::LambdaParams::simple(vec![intern("x")]),
    ));

    assert_eq!(length_value(&bc).unwrap(), Value::Int(4));
}

#[test]
fn vm_keymap_predicate_and_lookup_resolve_symbol_function_cells() {
    assert_eq!(
        vm_eval_str(
            "(let ((map (make-sparse-keymap)))
               (define-key map [97] 'ignore)
               (fset 'vm-test-keymap map)
               (list (keymapp 'vm-test-keymap)
                     (lookup-key 'vm-test-keymap [97])))"
        ),
        "OK (t ignore)"
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
        lexical: false,
        env: None,
        gnu_byte_offset_map: None,
        docstring: None,
        doc_form: None,
        interactive: None,
    };

    let mut eval = Evaluator::new_vm_harness();
    let mut vm = new_vm(&mut eval);

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
fn vm_window_and_frame_selection_builtins_use_shared_runtime_state() {
    assert_eq!(
        vm_eval_str(
            r#"(let* ((f (selected-frame))
                      (w (selected-window)))
                 (list (framep f)
                       (windowp w)
                       (eq (selected-frame) f)
                       (eq (frame-selected-window f) w)
                       (eq (frame-first-window f) w)
                       (eq (frame-root-window f) w)
                       (eq (window-frame w) f)
                       (bufferp (window-buffer w))
                       (window-live-p w)
                       (window-valid-p w)
                       (frame-live-p f)
                       (frame-visible-p f)))"#
        ),
        "OK (t t t t t t t t t t t t)"
    );
}

#[test]
fn vm_window_state_accessors_use_shared_runtime_state() {
    assert_eq!(
        vm_eval_str(
            r#"(let ((w (selected-window)))
                 (with-current-buffer (window-buffer w)
                   (erase-buffer)
                   (insert (make-string 200 ?x)))
                 (set-window-start w 7)
                 (set-window-point w 9)
                 (list (window-start w)
                       (window-group-start w)
                       (window-point w)
                       (integerp (window-use-time w))
                       (window-old-point w)
                       (window-old-buffer w)
                       (window-prev-buffers w)
                       (window-next-buffers w)))"#
        ),
        "OK (7 7 9 t 9 nil nil nil)"
    );
    assert_eq!(
        vm_eval_str(
            r#"(list (condition-case err (window-start 999999) (error err))
                     (condition-case err (window-group-start 999999) (error err))
                     (condition-case err (window-point 999999) (error err))
                     (condition-case err (window-use-time 999999) (error err))
                     (condition-case err (window-old-point 999999) (error err))
                     (condition-case err (window-old-buffer 999999) (error err))
                     (condition-case err (window-prev-buffers 999999) (error err))
                     (condition-case err (window-next-buffers 999999) (error err)))"#
        ),
        "OK ((wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p 999999))"
    );
}

#[test]
fn vm_window_scroll_and_history_builtins_use_shared_runtime_state() {
    assert_eq!(
        vm_eval_str(
            r#"(let ((w (selected-window)))
                 (list (window-hscroll w)
                       (set-window-hscroll w 3)
                       (window-hscroll w)
                       (set-window-hscroll w -1)
                       (window-hscroll w)
                       (set-window-hscroll w ?a)
                       (window-hscroll w)
                       (window-margins w)
                       (set-window-margins w 1 2)
                       (window-margins w)
                       (set-window-margins w 1 2)
                       (set-window-margins w nil nil)
                       (window-margins w)
                       (set-window-margins w 3)
                       (window-margins w)
                       (set-window-margins w 3)
                       (window-vscroll w)
                       (set-window-vscroll w 1)
                       (window-vscroll w)
                       (window-fringes w)
                       (set-window-fringes w 1 2)
                       (window-scroll-bars w)
                       (set-window-scroll-bars w 'left)
                       (window-scroll-bars w)
                       (set-window-prev-buffers w nil)
                       (window-prev-buffers w)
                       (set-window-next-buffers w nil)
                       (window-next-buffers w)))"#
        ),
        "OK (0 3 3 0 0 97 97 (nil) t (1 . 2) nil t (nil) t (3) nil 0 0 0 (0 0 nil nil) nil (nil 0 t nil 0 t nil) nil (nil 0 t nil 0 t nil) nil nil nil nil)"
    );
    assert_eq!(
        vm_eval_str(
            r#"(let* ((w1 (selected-window))
                      (w2 (split-window-internal (selected-window) nil nil nil)))
                 (list (window-use-time w1)
                       (window-use-time w2)
                       (window-bump-use-time w2)
                       (window-use-time w1)
                       (window-use-time w2)
                       (window-bump-use-time w1)))"#
        ),
        "OK (1 0 1 2 1 nil)"
    );
}

#[test]
fn vm_window_geometry_builtins_use_shared_runtime_state() {
    assert_eq!(
        vm_eval_str(
            r#"(let* ((w (selected-window))
                      (m (minibuffer-window)))
                 (with-current-buffer (window-buffer w)
                   (erase-buffer)
                   (insert (make-string 200 ?x)))
                 (list (window-height w)
                       (window-width w)
                       (window-body-height w)
                       (window-body-width w)
                       (window-total-height w)
                       (window-total-width w)
                       (window-left-column w)
                       (window-top-line m)
                       (window-pixel-left w)
                       (window-pixel-top m)
                       (> (window-end w) (window-start w))
                       (window-mode-line-height w)
                       (window-mode-line-height m)
                       (window-header-line-height w)
                       (window-pixel-height w)
                       (window-pixel-height m)
                       (window-pixel-width w)
                       (window-pixel-width m)
                       (window-text-height w)
                       (window-text-height m)
                       (window-text-width w)
                       (window-text-width m)
                       (window-edges w)
                       (window-edges m)
                       (window-edges w t)
                       (window-edges m t)))"#
        ),
        "OK (24 80 23 80 24 80 0 24 0 24 t 1 0 0 24 1 80 80 23 1 80 80 (0 0 80 24) (0 24 80 25) (0 0 80 23) (0 24 80 25))"
    );
}

#[test]
fn vm_window_metadata_builtins_use_shared_runtime_state() {
    assert_eq!(
        vm_eval_str(
            r#"(let* ((w (selected-window))
                      (m (minibuffer-window))
                      (dt '(1 2 3)))
                 (list (window-dedicated-p w)
                       (set-window-dedicated-p w t)
                       (window-dedicated-p w)
                       (set-window-dedicated-p w nil)
                       (window-dedicated-p w)
                       (null (window-parameters w))
                       (set-window-parameter w 'foo 'bar)
                       (window-parameter w 'foo)
                       (equal (window-parameters w) '((foo . bar)))
                       (set-window-parameter w 'foo nil)
                       (equal (window-parameters w) '((foo)))
                       (null (window-display-table w))
                       (let ((rv (set-window-display-table w dt))) (equal rv dt))
                       (equal (window-display-table w) dt)
                       (null (set-window-display-table w nil))
                       (null (window-display-table w))
                       (window-cursor-type w)
                       (set-window-cursor-type w 'bar)
                       (window-cursor-type w)
                       (set-window-cursor-type w t)
                       (window-cursor-type w)
                       (set-window-cursor-type m nil)
                       (window-cursor-type m)))"#
        ),
        "OK (nil t t nil nil t bar bar t nil t t t t t t t bar bar t t nil nil)"
    );
}

#[test]
fn vm_window_tree_and_list_builtins_use_shared_runtime_state() {
    assert_eq!(
        vm_eval_with_init_str(
            r#"(let* ((left (selected-window))
                      (right (next-window left))
                      (bottom (next-window right))
                      (root (frame-root-window))
                      (vparent (window-parent right)))
                 (list (window-valid-p root)
                       (window-live-p root)
                       (eq (window-parent left) root)
                       (eq (window-next-sibling left) vparent)
                       (eq (window-left-child root) left)
                       (window-valid-p (window-top-child root))
                       (eq (window-parent right) vparent)
                       (eq (window-parent bottom) vparent)
                       (eq (window-top-child vparent) right)
                       (null (window-left-child vparent))
                       (eq (window-next-sibling right) bottom)
                       (eq (window-prev-sibling bottom) right)
                       (length (window-list))
                       (length (window-list nil t))
                       (not (null (memq bottom (window-list-1 left nil nil))))
                       (windowp (window-at 0 0))
                       (windowp (window-at 79 0))
                       (let ((m (window-at 0 24))) (and m (window-minibuffer-p m)))
                       (window-combination-limit root)
                       (set-window-combination-limit root t)
                       (window-combination-limit root)))"#,
            |eval| {
                let fid = crate::emacs_core::window_cmds::ensure_selected_frame_id(eval);
                let left = eval.frames.get(fid).expect("frame").selected_window;
                let buffer_id = eval.buffers.current_buffer().expect("buffer").id;
                let right = eval
                    .frames
                    .split_window(fid, left, SplitDirection::Horizontal, buffer_id)
                    .expect("horizontal split");
                let _bottom = eval
                    .frames
                    .split_window(fid, right, SplitDirection::Vertical, buffer_id)
                    .expect("vertical split");
            }
        ),
        "OK (t nil t t t nil t t t t t t 3 4 t t t t nil t t)"
    );
}

#[test]
fn vm_window_selection_and_buffer_builtins_use_shared_runtime_state() {
    assert_eq!(
        vm_eval_with_init_str(
            r#"(let* ((w1 (selected-window))
                      (w2 (next-window w1))
                      (b1 (get-buffer-create "vm-wsel-1"))
                      (b2 (get-buffer-create "vm-wsel-2")))
                 (set-window-buffer w1 b1)
                 (set-window-buffer w2 b2)
                 (select-window w2)
                 (list (eq (selected-window) w2)
                       (eq (current-buffer) b2)
                       (eq (window-buffer w1) b1)
                       (eq (window-buffer w2) b2)
                       (eq (next-window w1) w2)
                       (eq (previous-window w1) w2)
                       (eq (other-window-for-scrolling) w1)
                       (other-window 1)
                       (eq (selected-window) w1)
                       (eq (current-buffer) b1)
                       (eq (other-window-for-scrolling) w2)
                       (window-valid-p (car (window-list)))
                       (window-live-p (car (window-list-1 nil nil)))
                       (condition-case err (set-window-combination-limit w1 t)
                         (error (car err)))))"#,
            |eval| {
                let fid = crate::emacs_core::window_cmds::ensure_selected_frame_id(eval);
                let w1 = eval.frames.get(fid).expect("frame").selected_window;
                let buffer_id = eval.buffers.current_buffer().expect("buffer").id;
                let _w2 = eval
                    .frames
                    .split_window(fid, w1, SplitDirection::Horizontal, buffer_id)
                    .expect("horizontal split");
            }
        ),
        "OK (t t t t t t t nil t t t t t error)"
    );
}

#[test]
fn vm_window_deletion_and_frame_builtins_use_shared_runtime_state() {
    assert_eq!(
        vm_eval_with_init_str(
            r#"(let* ((w1 (selected-window))
                      (w2 (next-window w1))
                      (b1 (get-buffer-create "vm-del-1"))
                      (b2 (get-buffer-create "vm-del-2"))
                      (f2 (make-frame '((name . "vm-frame")))))
                 (set-window-buffer w1 b1)
                 (set-window-buffer w2 b2)
                 (select-window w2)
                 (list (framep f2)
                       (frame-live-p f2)
                       (delete-window w2)
                       (eq (selected-window) w1)
                       (eq (current-buffer) b1)
                       (length (window-list))
                       (progn (delete-other-windows w1) (length (window-list)))
                       (delete-frame f2)
                       (frame-live-p f2)))"#,
            |eval| {
                let fid = crate::emacs_core::window_cmds::ensure_selected_frame_id(eval);
                let w1 = eval.frames.get(fid).expect("frame").selected_window;
                let buffer_id = eval.buffers.current_buffer().expect("buffer").id;
                let _w2 = eval
                    .frames
                    .split_window(fid, w1, SplitDirection::Horizontal, buffer_id)
                    .expect("horizontal split");
            }
        ),
        "OK (t t nil t t 1 1 nil nil)"
    );
}

#[test]
fn vm_split_window_and_frame_selection_builtins_use_shared_runtime_state() {
    assert_eq!(
        vm_eval_str(
            r#"(let* ((f1 (selected-frame))
                      (w1 (selected-window))
                      (w2 (split-window-internal w1 nil 'right nil))
                      (f2 (make-frame '((name . "vm-frame-sel")))))
                 (list (windowp w2)
                       (length (window-list))
                       (eq (select-frame f2) f2)
                       (eq (selected-frame) f2)
                       (eq (make-frame-visible f2) f2)
                       (length (frame-list))
                       (length (visible-frame-list))
                       (progn (iconify-frame f2) (frame-visible-p f2))
                       (length (visible-frame-list))
                       (progn (select-frame-set-input-focus f1)
                              (eq (selected-frame) f1))))"#
        ),
        "OK (t 2 t t t 2 2 nil 1 t)"
    );
}

#[test]
fn vm_window_configuration_builtins_use_shared_runtime_state() {
    assert_eq!(
        vm_eval_str(
            r#"(let* ((w1 (selected-window))
                      (w2 (split-window-internal w1 nil 'right nil))
                      (b1 (get-buffer-create "vm-wcfg-1"))
                      (b2 (get-buffer-create "vm-wcfg-2")))
                 (set-window-buffer w1 b1)
                 (set-window-buffer w2 b2)
                 (select-window w2)
                 (let ((cfg (current-window-configuration)))
                   (delete-window w2)
                   (set-window-configuration cfg)
                   (list (window-configuration-p cfg)
                         (framep (window-configuration-frame cfg))
                         (window-configuration-equal-p cfg cfg)
                         (length (window-list))
                         (eq (selected-window) w2)
                         (eq (current-buffer) b2)
                         (eq (window-buffer w1) b1)
                         (eq (window-buffer w2) b2))))"#
        ),
        "OK (t t t 2 t t t t)"
    );
}

#[test]
fn vm_eval_bridge_preserves_current_local_map_across_builtin_calls() {
    assert_eq!(
        vm_eval_str("(progn (use-local-map (make-sparse-keymap)) (keymapp (current-local-map)))"),
        "OK t"
    );
}

#[test]
fn vm_use_global_map_updates_shared_runtime_state() {
    assert_eq!(
        vm_eval_str("(progn (use-global-map (make-sparse-keymap)) (keymapp (current-global-map)))"),
        "OK t"
    );
}

#[test]
fn vm_keymap_structure_builtins_use_shared_runtime_state() {
    assert_eq!(
        vm_eval_str(
            r#"(let* ((parent (make-keymap))
                      (child (copy-keymap parent))
                      (minor (make-sparse-keymap))
                      (prefix (make-sparse-keymap)))
                 (define-key parent [1] 'parent-binding)
                 (define-key child [2] 'child-binding)
                 (set-keymap-parent child parent)
                 (define-key child [24] prefix)
                 (define-key prefix [97] 'prefixed)
                 (setq minor-mode-map-alist (list (cons 'vm-minor-mode minor)))
                 (setq vm-minor-mode t)
                 (use-local-map child)
                 (list (keymapp parent)
                       (not (eq child parent))
                       (eq (keymap-parent child) parent)
                       (eq (set-keymap-parent child parent) parent)
                       (let ((maps (current-active-maps)))
                         (list (eq (car maps) minor)
                               (eq (car (cdr maps)) child)
                               (eq (car (cdr (cdr maps))) (current-global-map))))
                       (equal (current-minor-mode-maps) (list minor))
                       (let ((root (make-sparse-keymap))
                             (desc (make-sparse-keymap)))
                         (define-key root [24] desc)
                         (not (null (accessible-keymaps root [24]))))
                       (lookup-key child [1])
                       (lookup-key child [2])
                       (lookup-key child [24 97])))"#
        ),
        "OK (t t t t (t t t) t t parent-binding child-binding prefixed)"
    );
}

#[test]
fn vm_map_keymap_builtins_use_shared_state_and_narrow_callback_bridge() {
    assert_eq!(
        vm_eval_str(
            r#"(let* ((parent (make-sparse-keymap))
                      (child (make-sparse-keymap))
                      (seen nil))
                 (define-key parent [1] 'parent-binding)
                 (define-key child [2] 'child-binding)
                 (set-keymap-parent child parent)
                 (fset 'vm-record-binding
                       (lambda (_event binding)
                         (setq seen (cons binding seen))))
                 (list
                  (progn
                    (setq seen nil)
                    (map-keymap-internal 'vm-record-binding child)
                    (reverse seen))
                  (progn
                    (setq seen nil)
                    (map-keymap 'vm-record-binding child)
                    (reverse seen))))"#
        ),
        "OK ((child-binding parent-binding) (child-binding parent-binding))"
    );
}

#[test]
fn vm_key_lookup_builtins_use_shared_runtime_state() {
    assert_eq!(
        vm_eval_str(
            r#"(let* ((g (make-sparse-keymap))
                      (l (make-sparse-keymap))
                      (m (make-sparse-keymap)))
                 (use-global-map g)
                 (use-local-map l)
                 (define-key g "a" 'ignore)
                 (define-key g [remap self-insert-command] 'delete-char)
                 (define-key l "c" 'self-insert-command)
                 (define-key m "b" 'forward-char)
                 (setq minor-mode-map-alist (list (cons 'vm-demo-mode m)))
                 (setq vm-demo-mode t)
                 (list (key-binding "a")
                       (key-binding "c")
                       (key-binding "c" t t)
                       (local-key-binding "c")
                       (minor-mode-key-binding "b")
                       (condition-case err
                           (key-binding "a" t nil 0)
                         (error (car err)))))"#
        ),
        "OK (ignore delete-char self-insert-command self-insert-command ((vm-demo-mode . forward-char)) args-out-of-range)"
    );
}

#[test]
fn vm_command_remapping_uses_shared_runtime_state() {
    assert_eq!(
        vm_eval_str(
            r#"(let ((g (make-sparse-keymap))
                     (m (make-sparse-keymap)))
                 (use-global-map g)
                 (define-key g [remap ignore] 'forward-char)
                 (define-key m [remap ignore] 'delete-char)
                 (setq minor-mode-map-alist (list (cons 'vm-remap-mode m)))
                 (setq vm-remap-mode t)
                 (list (command-remapping 'ignore)
                       (command-remapping 'ignore nil '(keymap (remap keymap (ignore . self-insert-command))))
                       (condition-case err
                           (command-remapping 'ignore 0)
                         (error (car err)))) )"#
        ),
        "OK (delete-char self-insert-command args-out-of-range)"
    );
}

#[test]
fn vm_set_buffer_and_current_buffer_share_buffer_runtime_state() {
    assert_eq!(
        vm_eval_str(
            "(progn
               (get-buffer-create \"*vm-current-buffer*\")
               (set-buffer \"*vm-current-buffer*\")
               (buffer-name (current-buffer)))"
        ),
        r#"OK "*vm-current-buffer*""#
    );
}

#[test]
fn vm_current_buffer_query_builtins_use_shared_runtime_state() {
    assert_eq!(
        vm_eval_with_init_str(
            r#"(list (point-min)
                     (point-max)
                     (buffer-string)
                     (goto-char 99)
                     (point)
                     (goto-char 2)
                     (point)
                     (char-after)
                     (char-before))"#,
            |eval| {
                let current = eval.buffers.current_buffer_id().expect("scratch buffer");
                let buffer = eval.buffers.get_mut(current).expect("scratch buffer");
                buffer.insert("hello");
                let start = buffer.lisp_pos_to_byte(2);
                let end = buffer.lisp_pos_to_byte(5);
                buffer.narrow_to_region(start, end);
            },
        ),
        r#"OK (2 5 "ell" 99 5 2 2 101 nil)"#
    );
}

#[test]
fn vm_goto_char_and_char_queries_use_live_marker_positions() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (insert "ab")
                 (let ((m (copy-marker 2)))
                   (goto-char 1)
                   (insert "X")
                   (list (point)
                         (marker-position m)
                         (progn (goto-char m) (point))
                         (char-after m)
                         (char-before m))))"#
        ),
        "OK (2 3 3 98 97)"
    );
}

#[test]
fn vm_navigation_predicates_and_line_positions_use_shared_narrowed_buffer_state() {
    assert_eq!(
        vm_eval_with_init_str(
            r#"(list (list (bobp) (eobp) (bolp) (eolp)
                           (line-beginning-position) (line-end-position))
                     (progn
                       (goto-char (point-max))
                       (list (bobp) (eobp) (bolp) (eolp)
                             (line-beginning-position) (line-end-position))))"#,
            |eval| {
                let current = eval.buffers.current_buffer_id().expect("scratch buffer");
                let buffer = eval.buffers.get_mut(current).expect("scratch buffer");
                buffer.insert("wx\nab\ncd");
                let start = buffer.lisp_pos_to_byte(4);
                let end = buffer.lisp_pos_to_byte(6);
                buffer.narrow_to_region(start, end);
                buffer.goto_char(buffer.begv);
            },
        ),
        "OK ((t nil t nil 4 6) (nil t nil t 4 6))"
    );
}

#[test]
fn vm_line_position_optional_argument_matches_gnu_current_rules() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (insert "a\nbb\nccc")
                 (goto-char 2)
                 (list (line-beginning-position 2)
                       (line-end-position 2)
                       (line-beginning-position 3)
                       (line-end-position 3)))"#
        ),
        "OK (3 5 6 9)"
    );
}

#[test]
fn vm_buffer_restriction_and_modified_state_use_shared_runtime_manager() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (insert "abcdef")
                 (list (buffer-size)
                       (buffer-modified-p)
                       (set-buffer-modified-p nil)
                       (buffer-modified-p)
                       (buffer-modified-tick)
                       (buffer-chars-modified-tick)
                       (let ((start (copy-marker 2))
                             (end (copy-marker 5 t)))
                         (goto-char 1)
                         (insert "X")
                         (narrow-to-region start end)
                         (list (point-min) (point-max) (buffer-string)))
                       (progn
                         (widen)
                         (list (point-min) (point-max) (buffer-string)))))"#
        ),
        r#"OK (6 t nil nil 2 2 (3 6 "bcd") (1 8 "Xabcdef"))"#
    );
}

#[test]
fn vm_buffer_mutation_builtins_use_shared_runtime_state() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (insert "abcdef")
                 (let ((start (copy-marker 2))
                       (end (copy-marker 5 t)))
                   (goto-char 1)
                   (insert "X")
                   (list (delete-and-extract-region start end)
                         (buffer-string)
                         (progn
                           (narrow-to-region 2 4)
                           (erase-buffer)
                           (list (point-min) (point-max) (buffer-string) (buffer-size))))))"#
        ),
        r#"OK ("bcd" "Xaef" (1 1 "" 0))"#
    );
}

#[test]
fn vm_buffer_substring_copy_builtins_use_shared_runtime_state() {
    assert_eq!(
        vm_eval_str(
            r#"(let ((src (get-buffer-create "*vm-sub-src*"))
                     (dst (get-buffer-create "*vm-sub-dst*")))
                 (set-buffer src)
                 (erase-buffer)
                 (insert "abcXYZ")
                 (put-text-property 2 5 'face 'bold)
                 (set-buffer dst)
                 (erase-buffer)
                 (insert-buffer-substring src 2 5)
                 (let ((sub (progn
                              (set-buffer src)
                              (buffer-substring 2 5)))
                       (copied (progn
                                 (set-buffer dst)
                                 (buffer-string))))
                   (list sub
                         (get-text-property 1 'face sub)
                         copied
                         (get-text-property 1 'face copied))))"#
        ),
        r#"OK (#("bcX" 0 3 (face bold)) bold #("bcX" 0 3 (face bold)) bold)"#
    );
}

#[test]
fn vm_compare_buffer_substrings_uses_shared_case_fold_state() {
    assert_eq!(
        vm_eval_str(
            r#"(let ((left (get-buffer-create "*vm-cmp-left*"))
                     (right (get-buffer-create "*vm-cmp-right*")))
                 (set-buffer left)
                 (erase-buffer)
                 (insert "Abc")
                 (set-buffer right)
                 (erase-buffer)
                 (insert "aBc")
                 (list
                  (let ((case-fold-search nil))
                    (compare-buffer-substrings left nil nil right nil nil))
                  (let ((case-fold-search t))
                    (compare-buffer-substrings left nil nil right nil nil))
                  (let ((case-fold-search t))
                    (compare-buffer-substrings left 1 2 right 1 3))))"#
        ),
        "OK (-1 0 -2)"
    );
}

#[test]
fn vm_buffer_identity_builtins_use_shared_runtime_state() {
    let path =
        std::env::temp_dir().join(format!("neovm-vm-gfb-{}-{}", std::process::id(), "shared"));
    std::fs::write(&path, b"vm-gfb").expect("write test file");
    let file = path.to_string_lossy().to_string();
    let default_dir = format!("{}/", path.parent().unwrap().to_string_lossy());
    let basename = path.file_name().unwrap().to_string_lossy().to_string();
    let form = format!(
        r#"(let ((default-directory {:?}))
             (list
              (buffer-name (get-file-buffer {:?}))
              (progn
                (rename-buffer "*vm-renamed-buffer*")
                (buffer-name))
              (condition-case err
                  (bury-buffer-internal 'x)
                (error (car err)))
              (bury-buffer-internal (current-buffer))))"#,
        default_dir, basename
    );

    let result = vm_eval_with_init_str(&form, |eval| {
        let current = eval.buffers.current_buffer_id().expect("scratch buffer");
        eval.buffers
            .set_buffer_file_name(current, Some(file.clone()))
            .expect("current buffer should accept file name");
    });
    let _ = std::fs::remove_file(path);

    assert_eq!(
        result,
        r#"OK ("*scratch*" "*vm-renamed-buffer*" wrong-type-argument nil)"#
    );
}

#[test]
fn vm_make_indirect_buffer_uses_shared_manager_state_and_narrow_hook_bridge() {
    assert_eq!(
        vm_eval_str(
            r#"(let ((base (get-buffer-create "vm-mib-base")))
                 (fset 'vm-mib-clone (lambda () (setq vm-mib-last-clone (buffer-name))))
                 (fset 'vm-mib-list (lambda () (setq vm-mib-buffer-list-ran t)))
                 (setq clone-indirect-buffer-hook '(vm-mib-clone))
                 (setq buffer-list-update-hook '(vm-mib-list))
                 (setq vm-mib-last-clone nil)
                 (setq vm-mib-buffer-list-ran nil)
                 (set-buffer base)
                 (let ((indirect (make-indirect-buffer base "vm-mib-ind" t)))
                   (list (buffer-name (current-buffer))
                         (buffer-name indirect)
                         vm-mib-last-clone
                         vm-mib-buffer-list-ran
                         (eq (buffer-base-buffer indirect) base))))"#
        ),
        r#"OK ("vm-mib-base" "vm-mib-ind" "vm-mib-ind" t t)"#
    );
}

#[test]
fn vm_kill_buffer_uses_shared_manager_and_frame_state() {
    assert_eq!(
        vm_eval_str(
            r#"(let ((a (get-buffer-create "vm-kill-a"))
                     (b (get-buffer-create "vm-kill-b")))
                 (set-buffer a)
                 (list
                  (kill-buffer nil)
                  (buffer-live-p a)
                  (let ((current (current-buffer)))
                    (list (not (eq current a))
                          (buffer-live-p current)))
                  (condition-case err
                      (kill-buffer "vm-kill-missing")
                    (error (car err)))))"#
        ),
        r#"OK (t nil (t t) error)"#
    );
}

#[test]
fn vm_set_buffer_multibyte_uses_shared_current_buffer_state() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (set-buffer-multibyte nil)
                 (insert-byte 200 1)
                 (let ((unibyte (append (buffer-string) nil)))
                   (erase-buffer)
                   (list unibyte
                         (set-buffer-multibyte 'foo)
                         (progn
                           (insert-byte 200 1)
                           (append (buffer-string) nil)))))"#
        ),
        r#"OK ((200) foo (4194248))"#
    );
}

#[test]
fn vm_field_builtins_use_shared_property_boundary_state() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (erase-buffer)
                 (list
                  (progn
                    (insert "abcdefg")
                    (put-text-property 2 5 'field 'left)
                    (put-text-property 5 8 'field 'right)
                    (put-text-property 2 5 'face 'bold)
                    (let ((s (field-string 3)))
                      (list
                       (list (field-beginning 3)
                             (field-end 3)
                             (field-string-no-properties 3))
                       (get-text-property 1 'face s)
                       (list (field-beginning 5)
                             (field-beginning 5 t)
                             (field-end 5)
                             (field-end 5 t))
                       (progn
                         (delete-field 3)
                         (list
                          (buffer-substring-no-properties (point-min) (point-max))
                          (get-text-property 2 'field))))))
                  (progn
                    (erase-buffer)
                    (insert "abcdefg")
                    (put-text-property 2 4 'field 'left)
                    (put-text-property 4 5 'field 'boundary)
                    (put-text-property 5 8 'field 'right)
                    (list (field-beginning 4)
                          (field-beginning 4 t)
                          (field-end 4)
                          (field-end 4 t)
                          (field-beginning 5)
                          (field-beginning 5 t)
                          (field-end 5)
                          (field-end 5 t)))))"#
        ),
        r#"OK (((2 5 "bcd") bold (2 2 5 8) ("aefg" right)) (2 2 4 8 4 2 5 8))"#
    );
}

#[test]
fn vm_constrain_to_field_uses_shared_field_state() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (erase-buffer)
                 (list
                  (progn
                    (insert "abcdefg")
                    (put-text-property 2 5 'field 'left)
                    (put-text-property 5 8 'field 'right)
                    (put-text-property 3 4 'capture t)
                    (goto-char 7)
                    (list
                     (constrain-to-field 7 3)
                     (constrain-to-field 7 5)
                     (constrain-to-field 7 5 t)
                     (progn
                       (goto-char 7)
                       (list (constrain-to-field nil 3) (point)))
                     (constrain-to-field 7 3 nil nil 'capture)
                     (constrain-to-field 7 2 nil nil 'capture)))
                  (progn
                    (erase-buffer)
                    (insert "ab\ncd\nef")
                    (put-text-property 1 4 'field 'top)
                    (put-text-property 4 9 'field 'bottom)
                    (list
                     (constrain-to-field 6 2 nil t)
                     (constrain-to-field 6 2 nil nil)
                     (constrain-to-field 6 4 t nil)))))"#
        ),
        r#"OK ((5 5 7 (5 5) 5 2) (4 4 6))"#
    );
}

#[test]
fn vm_replace_region_contents_uses_shared_source_and_property_state() {
    assert_eq!(
        vm_eval_str(
            r#"(let ((dest (current-buffer))
                     (src (get-buffer-create "*rrc-src*"))
                     (s (propertize "CD" 'face 'bold)))
                 (set-buffer src)
                 (erase-buffer)
                 (insert "1234")
                 (put-text-property 2 4 'face 'italic)
                 (set-buffer dest)
                 (list
                  (progn
                    (erase-buffer)
                    (insert "abZZef")
                    (replace-region-contents 3 5 s)
                    (list
                     (buffer-substring-no-properties 1 (point-max))
                     (get-text-property 3 'face)))
                  (progn
                    (erase-buffer)
                    (insert "abZZef")
                    (replace-region-contents 3 5 (vector src 2 4))
                    (list
                     (buffer-substring-no-properties 1 (point-max))
                     (get-text-property 3 'face)
                     (get-text-property 4 'face)))
                  (condition-case err
                      (replace-region-contents 3 5 (current-buffer))
                    (error (list (car err) (car (cdr err)))))))"#
        ),
        r#"OK (("abCDef" bold) ("ab23ef" italic italic) (error "Cannot replace a buffer with itself"))"#
    );
}

#[test]
fn vm_read_only_noop_buffer_mutations_match_gnu() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (setq buffer-read-only t)
                 (list (delete-region 1 1)
                       (delete-and-extract-region 1 1)
                       (progn
                         (narrow-to-region 1 1)
                         (erase-buffer)
                         (list (point-min) (point-max) (buffer-string)))))"#
        ),
        r#"OK (nil "" (1 1 ""))"#
    );
}

#[test]
fn vm_autoload_and_symbol_file_share_autoload_runtime_state() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (autoload 'vm-symbol-file-probe "vm-symbol-file-probe-file")
                 (symbol-file 'vm-symbol-file-probe))"#
        ),
        r#"OK "vm-symbol-file-probe-file""#
    );
}

#[test]
fn vm_compiled_autoload_do_load_uses_shared_runtime_and_load_bridge() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("vm-bytecode-autoload-do-load.el"),
        "(defun vm-bytecode-autoload-do-load () 91)\n",
    )
    .expect("write autoload-do-load fixture");

    let mut eval = Evaluator::new_vm_harness();
    eval.obarray.set_symbol_value(
        "load-path",
        Value::list(vec![Value::string(dir.path().to_string_lossy())]),
    );
    let forms = parse_forms(
        r#"(progn
             (autoload 'vm-bytecode-autoload-do-load "vm-bytecode-autoload-do-load")
             (autoload-do-load (symbol-function 'vm-bytecode-autoload-do-load)
                               'vm-bytecode-autoload-do-load)
             (vm-bytecode-autoload-do-load))"#,
    )
    .expect("parse");
    let mut compiler = Compiler::new(false);
    let func = compiler.compile_toplevel(&forms[0]);

    let result = {
        let mut vm = new_vm(&mut eval);
        vm.execute(&func, vec![])
            .expect("compiled autoload-do-load should execute")
    };

    assert_eq!(result, Value::Int(91));
}

#[test]
fn vm_compiled_named_autoload_call_uses_shared_runtime_and_load_bridge() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("vm-bytecode-autoload-call.el"),
        "(defun vm-bytecode-autoload-call (x) (+ x 7))\n",
    )
    .expect("write autoload call fixture");

    let mut eval = Evaluator::new_vm_harness();
    eval.obarray.set_symbol_value(
        "load-path",
        Value::list(vec![Value::string(dir.path().to_string_lossy())]),
    );
    let forms = parse_forms(
        r#"(progn
             (autoload 'vm-bytecode-autoload-call "vm-bytecode-autoload-call")
             (vm-bytecode-autoload-call 5))"#,
    )
    .expect("parse");
    let mut compiler = Compiler::new(false);
    let func = compiler.compile_toplevel(&forms[0]);

    let result = {
        let mut vm = new_vm(&mut eval);
        vm.execute(&func, vec![])
            .expect("compiled autoloaded call should execute")
    };

    assert_eq!(result, Value::Int(12));
}

#[test]
fn vm_indentation_builtins_use_buffer_local_current_buffer_state() {
    assert_eq!(
        vm_eval_with_init_str(
            r#"(list (current-indentation)
                     (current-column)
                     (progn
                       (goto-char 1)
                       (move-to-column 3))
                     (current-column))"#,
            |eval| {
                let current = eval.buffers.current_buffer_id().expect("scratch buffer");
                let buffer = eval.buffers.get_mut(current).expect("scratch buffer");
                buffer.set_buffer_local("tab-width", Value::Int(4));
                buffer.insert("\tb");
                buffer.goto_char(3);
            },
        ),
        "OK (4 5 4 4)"
    );
}

#[test]
fn vm_indent_to_uses_dynamic_indentation_bindings() {
    assert_eq!(
        vm_eval_str(
            r#"(let ((tab-width 4) (indent-tabs-mode t))
                 (list (indent-to 6 1)
                       (current-column)
                       (append (buffer-string) nil)))"#
        ),
        "OK (6 6 (9 32 32))"
    );
}

#[test]
fn vm_insert_before_markers_updates_markers_at_point() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (insert "ab")
                 (goto-char 1)
                 (let ((m (copy-marker (point))))
                   (insert-before-markers "X")
                   (list (buffer-string) (marker-position m))))"#
        ),
        r#"OK ("Xab" 2)"#
    );
}

#[test]
fn vm_insert_and_insert_char_use_shared_buffer_state() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (insert "ab")
                 (goto-char 1)
                 (let ((m (copy-marker (point))))
                   (list
                    (progn
                      (insert "X")
                      (list (buffer-string) (marker-position m) (point)))
                    (progn
                      (insert-char ?Y 2)
                      (list (buffer-string) (marker-position m) (point))))))"#
        ),
        r#"OK (("Xab" 1 2) ("XYYab" 1 4))"#
    );
}

#[test]
fn vm_insert_read_only_shape_and_noop_cases_match_gnu() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (setq buffer-read-only t)
                 (list
                  (condition-case err
                      (insert "x")
                    (error (list (car err) (bufferp (car (cdr err))))))
                  (condition-case err
                      (insert-char ?x 1)
                    (error (list (car err) (bufferp (car (cdr err))))))
                  (condition-case err
                      (insert-and-inherit "x")
                    (error (list (car err) (bufferp (car (cdr err))))))
                  (condition-case err
                      (insert-before-markers-and-inherit "x")
                    (error (list (car err) (bufferp (car (cdr err))))))
                  (condition-case err
                      (insert-byte 120 1)
                    (error (list (car err) (bufferp (car (cdr err))))))
                  (list (insert)
                        (insert "")
                        (insert-char ?x 0)
                        (insert-byte 120 0)
                        (insert-and-inherit)
                        (insert-and-inherit "")
                        (insert-before-markers-and-inherit)
                        (insert-before-markers-and-inherit "")
                        (buffer-string))))"#
        ),
        r#"OK ((buffer-read-only t) (buffer-read-only t) (buffer-read-only t) (buffer-read-only t) (buffer-read-only t) (nil nil nil nil nil nil nil nil ""))"#
    );
}

#[test]
fn vm_insert_inherit_variants_use_shared_runtime_state() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (erase-buffer)
                 (insert "a")
                 (put-text-property 1 2 'face 'bold)
                 (let ((first
                        (progn
                          (insert-and-inherit
                           (propertize "X" 'face 'italic 'mouse-face 'highlight))
                          (list (buffer-substring-no-properties (point-min) (point-max))
                                (get-text-property 2 'face)
                                (get-text-property 2 'mouse-face)))))
                   (erase-buffer)
                   (insert "ab")
                   (put-text-property 1 2 'face 'bold)
                   (goto-char 2)
                   (let ((m (copy-marker (point))))
                     (list first
                           (progn
                             (insert-before-markers-and-inherit
                              (propertize "X" 'mouse-face 'highlight))
                             (list (buffer-substring-no-properties (point-min) (point-max))
                                   (marker-position m)
                                   (get-text-property 2 'face)
                                   (get-text-property 2 'mouse-face)))))))"#
        ),
        r#"OK (("aX" bold highlight) ("aXb" 3 bold highlight))"#
    );
}

#[test]
fn vm_insert_byte_and_buffer_undo_toggles_use_shared_runtime_state() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (list (progn
                         (insert-byte 65 2)
                         (buffer-string))
                       (progn
                         (erase-buffer)
                         (insert-byte 200 1)
                         (append (buffer-string) nil))
                       (progn
                         (buffer-enable-undo)
                         buffer-undo-list)
                       (progn
                         (buffer-disable-undo)
                         buffer-undo-list)))"#
        ),
        r#"OK ("AA" (4194248) nil t)"#
    );

    assert_eq!(
        vm_eval_with_init_str(
            r#"(progn
                 (insert-byte 200 1)
                 (append (buffer-string) nil))"#,
            |eval| {
                let current = eval.buffers.current_buffer_id().expect("scratch buffer");
                eval.buffers
                    .set_buffer_multibyte_flag(current, false)
                    .expect("set-buffer-multibyte should accept scratch buffer");
            },
        ),
        "OK (200)"
    );
}

#[test]
fn vm_subst_char_in_region_uses_shared_runtime_state_and_gnu_noop_rules() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (insert "a\n")
                 (let ((end (copy-marker (point-max) t)))
                   (goto-char (point-min))
                   (insert " ")
                   (let ((changed
                          (progn
                            (subst-char-in-region (point-min) end ?\n ?\s t)
                            (buffer-substring-no-properties (point-min) (point-max)))))
                     (setq buffer-read-only t)
                     (list changed
                           (condition-case err
                               (subst-char-in-region 1 2 ?\s ?_)
                             (error (list (car err) (bufferp (car (cdr err))))))
                           (subst-char-in-region 1 1 ?\s ?_)
                           (subst-char-in-region 1 (point-max) ?z ?_)
                           (buffer-substring-no-properties (point-min) (point-max))))))"#
        ),
        r#"OK (" a " (buffer-read-only t) nil nil " a ")"#
    );
}

#[test]
fn vm_barf_if_buffer_read_only_uses_shared_state_and_inhibit_text_property() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (insert "abc")
                 (put-text-property 2 3 'inhibit-read-only t)
                 (setq buffer-read-only t)
                 (list (barf-if-buffer-read-only 2)
                       (condition-case err
                           (barf-if-buffer-read-only 1)
                         (error (list (car err) (bufferp (car (cdr err))))))))"#
        ),
        r#"OK (nil (buffer-read-only t))"#
    );
}

#[test]
fn vm_char_primitives_and_buffer_substring_use_narrowed_current_buffer_state() {
    assert_eq!(
        vm_eval_with_init_str(
            r#"(list (following-char)
                     (preceding-char)
                     (buffer-substring-no-properties 3 8)
                     (buffer-substring-no-properties 8 3)
                     (condition-case err
                         (buffer-substring-no-properties 0 1)
                       (error (car err))))"#,
            |eval| {
                let current = eval.buffers.current_buffer_id().expect("scratch buffer");
                let buffer = eval.buffers.get_mut(current).expect("scratch buffer");
                buffer.insert("Hello, 世界");
                let start = buffer.lisp_pos_to_byte(3);
                let end = buffer.lisp_pos_to_byte(8);
                buffer.narrow_to_region(start, end);
                buffer.goto_char(buffer.begv);
            },
        ),
        r#"OK (108 0 "llo, " "llo, " args-out-of-range)"#
    );
}

#[test]
fn vm_byte_position_and_get_byte_use_shared_runtime_state() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (insert "éa")
                 (let ((m (copy-marker 2)))
                   (list (byte-to-position 1)
                         (byte-to-position 2)
                         (byte-to-position 3)
                         (position-bytes 1)
                         (position-bytes m)
                         (position-bytes 3)
                         (get-byte m))))"#
        ),
        "OK (1 1 2 1 3 4 97)"
    );

    assert_eq!(
        vm_eval_with_init_str(
            r#"(progn
                 (insert-byte 200 1)
                 (insert-byte 65 1)
                 (list (get-byte 1)
                     (get-byte 2)
                     (condition-case err
                         (get-byte 3)
                       (error (car err)))))"#,
            |eval| {
                let current = eval.buffers.current_buffer_id().expect("scratch buffer");
                eval.buffers
                    .set_buffer_multibyte_flag(current, false)
                    .expect("set-buffer-multibyte should accept scratch buffer");
            },
        ),
        "OK (200 65 args-out-of-range)"
    );
}

#[test]
fn vm_syntax_navigation_builtins_use_shared_runtime_state() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (insert "abc123")
                 (goto-char 1)
                 (list (skip-chars-forward "a-c")
                       (point)
                       (progn
                         (goto-char (point-max))
                         (skip-chars-backward "1-3"))
                       (point)))"#
        ),
        "OK (3 4 -3 4)"
    );

    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (erase-buffer)
                 (insert "(a (b)) c")
                 (list (scan-sexps 1 1)
                       (scan-lists 1 2 0)
                       (scan-sexps (point-max) -1)))"#
        ),
        "OK (8 10 9)"
    );
}

#[test]
fn vm_delete_char_uses_shared_read_only_and_narrowing_state() {
    assert_eq!(
        vm_eval_with_init_str(
            r#"(list
                 (let ((buffer-read-only t))
                   (condition-case err
                       (delete-char 1)
                     (error (car err))))
                 (let ((buffer-read-only t)
                       (inhibit-read-only t))
                   (delete-char 1)
                   (buffer-string))
                 (progn
                   (narrow-to-region 1 2)
                   (goto-char (point-max))
                   (condition-case err
                       (delete-char 1)
                     (error (car err)))))"#,
            |eval| {
                let current = eval.buffers.current_buffer_id().expect("scratch buffer");
                let buffer = eval.buffers.get_mut(current).expect("scratch buffer");
                buffer.insert("abc");
                buffer.goto_char(0);
            },
        ),
        r#"OK (buffer-read-only "bc" end-of-buffer)"#
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
fn vm_buffer_local_and_binding_builtins_use_shared_state() {
    assert_eq!(
        vm_eval_with_init_str(
            r#"(progn
                 (defvaralias 'vm-vm-alias 'vm-vm-base)
                 (defvaralias 'vm-vm-lvis-alias 'vm-vm-lvis-base)
                 (make-variable-buffer-local 'vm-vm-lvis-base)
                 (list (buffer-local-value 'vm-vm-alias (current-buffer))
                       (buffer-local-value 'vm-vm-base (current-buffer))
                       (bufferp (variable-binding-locus 'vm-vm-alias))
                       (buffer-live-p (variable-binding-locus 'vm-vm-base))
                       (local-variable-if-set-p 'vm-vm-lvis-alias)
                       (local-variable-if-set-p 'vm-vm-lvis-base)))"#,
            |eval| {
                let current = eval.buffers.current_buffer_id().expect("scratch buffer");
                let buffer = eval.buffers.get_mut(current).expect("scratch buffer");
                buffer.set_buffer_local("vm-vm-base", Value::Int(3));
            },
        ),
        "OK (3 3 t t t t)"
    );

    assert_eq!(
        vm_eval_str(
            r#"(list
                 (buffer-local-value nil (current-buffer))
                 (buffer-local-value t (current-buffer))
                 (buffer-local-value :vm-k (current-buffer))
                 (condition-case err
                     (buffer-local-value 'vm-miss (current-buffer))
                   (error (car err)))
                 (condition-case err
                     (variable-binding-locus 1)
                   (error (car err)))
                 (condition-case err
                     (local-variable-if-set-p 1)
                   (error (car err))))"#
        ),
        "OK (nil t :vm-k void-variable wrong-type-argument wrong-type-argument)"
    );
}

#[test]
fn vm_search_builtins_use_shared_runtime_state_and_match_data() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (insert "ab")
                 (let ((end (copy-marker (point-max) t)))
                   (goto-char (point-min))
                   (insert "X")
                   (goto-char (point-min))
                   (list (search-forward "b" end t)
                         (point)
                         (marker-position end)
                         (match-beginning 0)
                         (match-end 0))))"#
        ),
        "OK (4 4 4 3 4)"
    );

    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (erase-buffer)
                 (insert "ab12")
                 (goto-char 1)
                 (list (re-search-forward "[0-9]+" nil t)
                       (match-beginning 0)
                       (match-end 0)
                       (progn
                         (goto-char 1)
                         (search-forward-regexp "[a-z]+" nil t))
                       (progn
                         (goto-char 1)
                         (posix-search-forward "[0-9]+" nil t))))"#
        ),
        "OK (5 3 5 3 5)"
    );
}

#[test]
fn vm_looking_at_builtins_use_shared_match_data_and_case_fold() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (insert "A")
                 (goto-char 1)
                 (list
                  (let ((case-fold-search nil))
                    (looking-at-p "a"))
                  (let ((case-fold-search t))
                    (looking-at-p "a"))
                  (progn
                    (set-match-data '(10 11))
                    (let ((case-fold-search t))
                      (looking-at "a" t))
                    (match-beginning 0))
                  (progn
                    (set-match-data nil)
                    (let ((case-fold-search t))
                      (looking-at "a"))
                    (list (match-beginning 0)
                          (match-end 0)))
                  (let ((case-fold-search t))
                    (posix-looking-at "a"))))"#
        ),
        "OK (nil t 10 (1 2) t)"
    );
}

#[test]
fn vm_replace_match_and_match_translate_use_shared_match_state() {
    assert_eq!(
        vm_eval_str(
            r#"(list
                 (let ((case-fold-search t))
                   (posix-string-match "A" "a"))
                 (progn
                   (string-match "\\([a-z]+\\)-\\([0-9]+\\)" "foo-42")
                   (replace-match "bar" t t "foo-42" 1))
                 (progn
                   (set-match-data '(1 4 2 3))
                   (match-data--translate 5)
                   (match-data))
                 (progn
                   (erase-buffer)
                   (insert "foo-42")
                   (goto-char 1)
                   (re-search-forward "\\([a-z]+\\)-\\([0-9]+\\)")
                   (list
                    (replace-match "\\2-\\1")
                    (buffer-string)
                    (match-beginning 0)
                    (match-end 0)
                    (match-beginning 1)
                    (match-end 1)
                    (match-beginning 2)
                    (match-end 2))))"#
        ),
        r#"OK (0 "bar-42" (6 9 7 8) (nil "42-foo" 1 7 1 1 1 7))"#
    );
}

#[test]
fn vm_buffer_manager_query_builtins_use_shared_runtime_state() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (get-buffer-create "*Messages*")
                 (get-buffer-create "*vm-alt*")
                 (get-buffer-create " hidden")
                 (list
                  (mapcar #'buffer-name (buffer-list))
                  (buffer-name (other-buffer "*vm-alt*"))
                  (buffer-name (other-buffer "*vm-alt*" t))
                  (generate-new-buffer-name "*vm-alt*" "*vm-alt*<2>")))"#
        ),
        r#"OK (("*scratch*" "*Messages*" "*vm-alt*" " hidden") "*Messages*" "*scratch*" "*vm-alt*<2>")"#
    );
}

#[test]
fn vm_charset_region_builtins_use_shared_runtime_state() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (insert "aé😀")
                 (list
                  (find-charset-region 1 4)
                  (find-charset-region 2 3)
                  (find-charset-region 4 4)
                  (charset-after 1)
                  (charset-after 2)
                  (charset-after 3)
                  (charset-after 4)))"#
        ),
        r#"OK ((ascii unicode unicode-bmp) (unicode-bmp) (ascii) ascii unicode-bmp unicode nil)"#
    );
}

#[test]
fn vm_compose_region_internal_uses_shared_buffer_state() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (insert "abc")
                 (list
                  (compose-region-internal 1 3)
                  (condition-case err
                      (compose-region-internal 0 3)
                    (error (list (car err) (cdr err))))))"#
        ),
        r#"OK (nil (args-out-of-range (#<buffer 1> 0 3)))"#
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
    with_vm_eval("(aref [10 20 30] -1)", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
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
    });

    with_vm_eval("(aset [10 20 30] -1 99)", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
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
    });

    with_vm_eval("(aset \"abc\" 1 nil)", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("characterp"), Value::Nil]);
        }
        other => panic!("unexpected error: {other:?}"),
    });
}

#[test]
fn vm_builtin_wrong_arity_uses_subr_payload() {
    with_vm_eval("(car)", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
            assert_eq!(resolve_sym(symbol), "wrong-number-of-arguments");
            assert_eq!(data, vec![Value::Subr(intern("car")), Value::Int(0)]);
        }
        other => panic!("unexpected error: {other:?}"),
    });

    with_vm_eval("(car 1 2)", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
            assert_eq!(resolve_sym(symbol), "wrong-number-of-arguments");
            assert_eq!(data, vec![Value::Subr(intern("car")), Value::Int(2)]);
        }
        other => panic!("unexpected error: {other:?}"),
    });
}

#[test]
fn vm_bytecode_wrong_arity_matches_gnu_entry_check() {
    let mut func = ByteCodeFunction::new(
        crate::emacs_core::bytecode::decode::parse_arglist_descriptor(2 | (3 << 8)),
    );
    func.constants = vec![Value::Nil];
    func.ops = vec![Op::Constant(0), Op::Return];
    func.max_stack = 1;

    let mut eval = Evaluator::new_vm_harness();
    let mut vm = new_vm(&mut eval);

    let err = vm
        .execute(&func, vec![Value::Int(1)])
        .expect_err("bytecode arity must be validated at VM entry");
    match map_flow(err) {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "wrong-number-of-arguments");
            assert_eq!(
                data,
                vec![Value::cons(Value::Int(2), Value::Int(3)), Value::Int(1)]
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn vm_string_compare_type_errors_match_oracle() {
    with_vm_eval("(string= \"ab\" 1)", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("stringp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    });

    with_vm_eval("(string-lessp \"ab\" 1)", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("stringp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    });
}

#[test]
fn vm_list_lookup_type_errors_match_oracle() {
    with_vm_eval("(car 1)", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("listp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    });

    with_vm_eval("(cdr 1)", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("listp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    });

    with_vm_eval("(car-safe 1)", false, |result| match result {
        Ok(value) => assert_eq!(value, Value::Nil),
        other => panic!("unexpected error: {other:?}"),
    });
    with_vm_eval("(cdr-safe 1)", false, |result| match result {
        Ok(value) => assert_eq!(value, Value::Nil),
        other => panic!("unexpected error: {other:?}"),
    });

    with_vm_eval("(nth 'a '(1 2 3))", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("integerp"), Value::symbol("a")]);
        }
        other => panic!("unexpected error: {other:?}"),
    });

    with_vm_eval("(nth 1 1)", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("listp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    });

    with_vm_eval("(nthcdr 'a '(1 2 3))", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("integerp"), Value::symbol("a")]);
        }
        other => panic!("unexpected error: {other:?}"),
    });

    with_vm_eval("(nthcdr 1 1)", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("listp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    });

    with_vm_eval("(memq 'a 1)", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("listp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    });

    with_vm_eval("(assq 'a 1)", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("listp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    });
}

#[test]
fn vm_length_and_symbol_access_type_errors_match_oracle() {
    with_vm_eval("(length '(1 . 2))", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("listp"), Value::Int(2)]);
        }
        other => panic!("unexpected error: {other:?}"),
    });

    with_vm_eval("(symbol-value 1)", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("symbolp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    });

    with_vm_eval("(symbol-plist 1)", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("symbolp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    });

    with_vm_eval("(symbol-function 1)", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("symbolp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    });
}

#[test]
fn vm_symbol_introspection_builtins_use_shared_symbol_state() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (fset 'vm-sym-target '(lambda (x) x))
                 (fset 'vm-sym-a 'vm-sym-b)
                 (fset 'vm-sym-b 'vm-sym-target)
                 (put 'vm-sym-a 'vm-prop 17)
                 (autoload 'vm-sym-auto "vm-sym-file")
                 (autoload 'vm-sym-macro "vm-sym-file" nil nil 'macro)
                 (list
                  (symbol-function 'vm-sym-a)
                  (indirect-function 'vm-sym-a)
                  (functionp 'vm-sym-a)
                  (symbol-plist 'vm-sym-a)
                  (symbol-function 'vm-sym-auto)
                  (indirect-function 'vm-sym-auto)
                  (functionp 'vm-sym-auto)
                  (functionp 'vm-sym-macro)))"#
        ),
        r#"OK (vm-sym-b (lambda (x) x) t (vm-prop 17) (autoload "vm-sym-file" nil nil nil) (autoload "vm-sym-file" nil nil nil) t nil)"#
    );
}

#[test]
fn vm_variable_lookup_builtins_use_shared_dynamic_and_buffer_local_state() {
    assert_eq!(
        vm_eval_with_init_str(
            r#"(progn
                 (defvaralias 'vm-vm-alias 'vm-vm-base)
                 (list
                  (boundp 'vm-vm-alias)
                  (default-boundp 'vm-vm-alias)
                  (special-variable-p 'vm-vm-alias)
                  (indirect-variable 'vm-vm-alias)
                  (symbol-value 'vm-vm-alias)
                  (let ((vm-vm-base 9))
                    (list (boundp 'vm-vm-base)
                          (symbol-value 'vm-vm-base)))))"#,
            |eval| {
                let current = eval.buffers.current_buffer_id().expect("current buffer");
                let buffer = eval.buffers.get_mut(current).expect("current buffer");
                buffer.set_buffer_local("vm-vm-base", Value::Int(3));
            },
        ),
        "OK (t nil t vm-vm-base 3 (t 9))"
    );
}

#[test]
fn vm_func_arity_and_obarray_queries_use_shared_obarray_state() {
    assert_eq!(
        vm_eval_with_init_str(
            r#"(progn
                 (fset 'vm-fa-target 'car)
                 (list
                  (func-arity 'vm-fa-target)
                  (intern-soft "vm-soft-target")
                  (intern-soft "vm-soft-miss")
                  (obarrayp (obarray-make 3))
                  (obarrayp [1 2 3])))"#,
            |eval| {
                eval.obarray_mut().intern("vm-soft-target");
            },
        ),
        "OK ((1 . 1) vm-soft-target nil t nil)"
    );
}

#[test]
fn vm_function_mutator_builtins_use_shared_function_state() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (fset 'vm-fset-target 'car)
                 (list
                  (funcall 'vm-fset-target '(4 . 5))
                  (progn
                    (fmakunbound 'vm-fset-target)
                    (fboundp 'vm-fset-target))
                  (condition-case err
                      (fmakunbound nil)
                    (error (car err)))
                  (progn
                    (fset nil nil)
                    (symbol-function nil))))"#
        ),
        "OK (4 nil setting-constant nil)"
    );
}

#[test]
fn vm_set_builtin_uses_shared_runtime_without_touching_lexicals() {
    assert_eq!(
        vm_eval_lexical_str(
            r#"(progn
                 (makunbound 'vm-lex-set)
                 (let ((vm-lex-set 10))
                   (list (set 'vm-lex-set 20)
                         vm-lex-set
                         (symbol-value 'vm-lex-set))))"#
        ),
        "OK (20 10 20)"
    );
}

#[test]
fn vm_varset_and_set_resolve_aliases_and_reject_constants_like_gnu() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (defvaralias 'vm-set-alias 'vm-set-base)
                 (setq vm-set-alias 3)
                 (list
                  vm-set-base
                  vm-set-alias
                  (set 'vm-set-alias 4)
                  vm-set-base
                  vm-set-alias
                  (progn
                    (setq vm-set-side 0)
                    (condition-case err
                        (setq nil (setq vm-set-side 1))
                      (error (list (car err) (cdr err) vm-set-side))))
                  (progn
                    (setq vm-set-side 0)
                    (condition-case err
                        (setq :vm-set-k (setq vm-set-side 2))
                      (error (list (car err) (cdr err) vm-set-side))))))"#
        ),
        "OK (3 3 4 4 4 (setting-constant (nil) 1) (setting-constant (:vm-set-k) 2))"
    );
}

#[test]
fn vm_makunbound_uses_shared_runtime_void_bindings() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (defvar vm-mku-dyn 'global)
                 (list
                  (let ((vm-mku-dyn 'dyn))
                    (list (makunbound 'vm-mku-dyn)
                          (condition-case err vm-mku-dyn (error (car err)))
                          (condition-case err
                              (default-value 'vm-mku-dyn)
                            (error (car err)))
                          (boundp 'vm-mku-dyn)))
                  vm-mku-dyn
                  (default-value 'vm-mku-dyn)))"#
        ),
        "OK ((vm-mku-dyn void-variable void-variable nil) global global)"
    );
}

#[test]
fn vm_make_local_variable_ignores_lexical_locals_and_uses_runtime_binding() {
    assert_eq!(
        vm_eval_lexical_str(
            r#"(progn
                 (setq vm-mlv-lex-global 'global)
                 (let ((buf (get-buffer-create "vm-mlv-lex-buf")))
                   (set-buffer buf)
                   (let ((vm-mlv-lex-global 'lex))
                     (make-local-variable 'vm-mlv-lex-global)
                     (list vm-mlv-lex-global
                           (symbol-value 'vm-mlv-lex-global)
                           (buffer-local-value 'vm-mlv-lex-global buf)
                           (local-variable-p 'vm-mlv-lex-global buf)
                           (condition-case err
                               (buffer-local-value 'vm-mlv-lex-global buf)
                             (error (car err)))
                           (default-value 'vm-mlv-lex-global)))))"#
        ),
        "OK (lex global global t global global)"
    );
}

#[test]
fn vm_kill_local_variable_uses_shared_runtime_and_buffer_where_watchers() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (setq vm-klv-events nil)
                 (fset 'vm-klv-rec
                       (lambda (symbol newval operation where)
                         (setq vm-klv-events
                               (cons (list symbol newval operation (bufferp where) (buffer-live-p where))
                                     vm-klv-events))))
                 (defvaralias 'vm-klv-alias 'vm-klv-base)
                 (add-variable-watcher 'vm-klv-base 'vm-klv-rec)
                 (let ((buf (get-buffer-create "vm-klv-buf")))
                   (set-buffer buf)
                   (make-local-variable 'vm-klv-alias)
                   (set 'vm-klv-alias 7)
                   (kill-local-variable 'vm-klv-alias))
                 vm-klv-events)"#
        ),
        "OK ((vm-klv-base nil makunbound t t) (vm-klv-base 7 set t t))"
    );
}

#[test]
fn vm_kill_all_local_variables_uses_shared_runtime_defaults_and_clears_local_map() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (setq fill-column 70)
                 (use-local-map (make-sparse-keymap))
                 (make-local-variable 'fill-column)
                 (setq fill-column 80)
                 (setq major-mode 'neo-mode)
                 (setq mode-name "Neo")
                 (setq buffer-undo-list t)
                 (kill-all-local-variables)
                 (list fill-column
                       (current-local-map)
                       major-mode
                       mode-name
                       buffer-undo-list
                       (local-variable-p 'major-mode)
                       (local-variable-p 'mode-name)
                       (local-variable-p 'buffer-undo-list)))"#
        ),
        "OK (70 nil fundamental-mode \"Fundamental\" nil t t t)"
    );
}

#[test]
fn vm_syntax_table_accessors_use_shared_current_buffer_state() {
    assert_eq!(
        vm_eval_str(
            r#"(let ((primary (current-buffer))
                     (other (get-buffer-create "vm-syntax-other")))
                 (set-syntax-table (copy-syntax-table (standard-syntax-table)))
                 (modify-syntax-entry ?\; "<")
                 (erase-buffer)
                 (insert ";")
                 (list (syntax-table-p (syntax-table))
                       (= (char-syntax ?\;) ?<)
                       (consp (syntax-after 1))
                       (= (matching-paren ?\() ?\))
                       (not (eq (syntax-table) (standard-syntax-table)))
                       (progn
                         (set-buffer other)
                         (list (= (char-syntax ?\;) ?.)
                               (eq (syntax-table) (standard-syntax-table))))
                       (progn
                         (set-buffer primary)
                         (= (char-syntax ?\;) ?<))))"#
        ),
        "OK (t t t t t (t t) t)"
    );
}

#[test]
fn vm_syntax_motion_builtins_use_shared_point_and_syntax_state() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (set-syntax-table (copy-syntax-table (standard-syntax-table)))
                 (modify-syntax-entry ?\; "<")
                 (modify-syntax-entry ?\n ">")
                 (modify-syntax-entry ?' ". p")
                 (erase-buffer)
                 (insert "  ;c\n''foo bar")
                 (list
                  (progn (goto-char 1) (list (forward-comment 1) (point)))
                  (progn (goto-char 8) (backward-prefix-chars) (point))
                  (progn (goto-char 8) (forward-word) (point))
                  (progn (goto-char 1) (list (skip-syntax-forward " ") (point)))
                  (progn (goto-char 11) (list (skip-syntax-backward "w") (point)))))"#
        ),
        "OK ((t 6) 6 11 (2 3) (-3 8))"
    );
}

#[test]
fn vm_buffer_metadata_builtins_use_shared_manager_state() {
    assert_eq!(
        vm_eval_str(
            r#"(let* ((base (get-buffer-create "vm-meta-base"))
                     (indirect (make-indirect-buffer base "vm-meta-ind" t)))
                 (set-default 'vm-find-target 10)
                 (set-buffer indirect)
                 (make-local-variable 'vm-find-target)
                 (setq vm-find-target 88)
                 (list (buffer-live-p indirect)
                       (eq (get-buffer indirect) indirect)
                       (eq (find-buffer 'vm-find-target 88) indirect)
                       (equal (buffer-name indirect) "vm-meta-ind")
                       (equal (buffer-last-name indirect) "vm-meta-ind")
                       (eq (buffer-base-buffer indirect) base)
                       (buffer-file-name indirect)))"#
        ),
        "OK (t t t t t t nil)"
    );
}

#[test]
fn vm_parse_partial_sexp_uses_shared_current_buffer_state() {
    assert_eq!(
        vm_eval_str(
            r#"(let ((a (get-buffer-create "vm-pps-a"))
                     (b (get-buffer-create "vm-pps-b")))
                 (set-buffer a)
                 (erase-buffer)
                 (insert "(a)")
                 (setq vm-pps-a (parse-partial-sexp 1 3))
                 (set-buffer b)
                 (erase-buffer)
                 (insert "abc")
                 (list vm-pps-a
                       (parse-partial-sexp 1 4)))"#
        ),
        "OK ((1 1 2 nil nil nil 0 nil nil (1) nil) (0 nil 1 nil nil nil 0 nil nil nil nil))"
    );
}

#[test]
fn vm_overlay_builtins_use_shared_current_buffer_state() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (erase-buffer)
                 (insert "overlay body")
                 (let ((ov1 (make-overlay 2 6))
                       (ov2 (make-overlay 6 10)))
                   (overlay-put ov1 'face 'bold)
                   (list
                    (overlayp ov1)
                    (overlay-get ov1 'face)
                    (length (overlays-at 3))
                    (length (overlays-in 1 13))
                    (next-overlay-change 1)
                    (previous-overlay-change 10)
                    (progn
                      (move-overlay ov1 4 8)
                      (list (overlay-start ov1)
                            (overlay-end ov1)
                            (eq (overlay-buffer ov1) (current-buffer))
                            (> (length (overlay-properties ov1)) 0)))
                    (progn
                      (delete-overlay ov2)
                      (length (overlays-in 1 13)))
                    (progn
                      (delete-all-overlays)
                      (length (overlays-in 1 13))))))"#
        ),
        "OK (t bold 1 2 2 6 (4 8 t t) 1 0)"
    );
}

#[test]
fn vm_overlays_at_sorted_uses_shared_priority_order() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (erase-buffer)
                 (insert "overlay body")
                 (let ((ov-low (make-overlay 2 6))
                       (ov-high (make-overlay 2 6))
                       (ov-nil (make-overlay 2 6)))
                   (overlay-put ov-low 'name 'low)
                   (overlay-put ov-high 'name 'high)
                   (overlay-put ov-nil 'name 'nil-priority)
                   (overlay-put ov-low 'priority 1)
                   (overlay-put ov-high 'priority 10)
                   (mapcar (lambda (ov) (overlay-get ov 'name))
                           (overlays-at 3 t))))"#
        ),
        "OK (high low nil-priority)"
    );
}

#[test]
fn vm_text_property_builtins_use_shared_buffer_state() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (erase-buffer)
                 (insert "abcd")
                 (put-text-property 1 3 'face 'bold)
                 (add-text-properties 2 5 '(mouse-face highlight display "D"))
                 (list
                  (get-text-property 2 'face)
                  (get-char-property 3 'mouse-face)
                  (plist-get (text-properties-at 2) 'face)
                  (car (get-char-property-and-overlay 2 'face))
                  (cdr (get-char-property-and-overlay 2 'face))
                  (get-display-property 2 'display)
                  (progn
                    (remove-text-properties 2 5 '(mouse-face highlight))
                    (get-text-property 3 'mouse-face))
                  (progn
                    (set-text-properties 3 5 '(rear-nonsticky t))
                    (get-text-property 4 'rear-nonsticky))
                  (progn
                    (remove-list-of-text-properties 1 3 '(face))
                    (get-text-property 2 'face))))"#
        ),
        "OK (bold highlight bold bold nil \"D\" nil t nil)"
    );
}

#[test]
fn vm_text_property_change_queries_use_shared_live_marker_state() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (erase-buffer)
                 (insert "abcde")
                 (put-text-property 2 5 'p t)
                 (let ((lim (copy-marker 5))
                       (end (copy-marker 5 t)))
                   (goto-char 1)
                   (insert "Z")
                   (list
                    (next-property-change 1)
                    (next-single-property-change 1 'p)
                    (next-char-property-change 1)
                    (next-single-char-property-change 1 'p)
                    (previous-property-change lim)
                    (previous-single-property-change lim 'p)
                    (previous-char-property-change lim)
                    (previous-single-char-property-change lim 'p)
                    (text-property-any 1 end 'p t)
                    (text-property-not-all 3 end 'p t))))"#
        ),
        "OK (3 3 3 3 3 3 3 3 3 nil)"
    );
}

#[test]
fn vm_property_query_builtins_use_shared_overlay_precedence_and_stickiness() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (erase-buffer)
                 (insert "abcd")
                 (put-text-property 1 2 'carry 'before)
                 (put-text-property 1 2 'rear-nonsticky '(carry))
                 (put-text-property 2 3 'carry 'after)
                 (put-text-property 2 3 'front-sticky '(carry))
                 (put-text-property 2 3 'face 'text)
                 (let ((ov-low (make-overlay 2 4 nil t nil))
                       (ov-high (make-overlay 2 4 nil t nil)))
                   (overlay-put ov-low 'face 'low)
                   (overlay-put ov-low 'priority 1)
                   (overlay-put ov-high 'face 'high)
                   (overlay-put ov-high 'priority '(10 . 0))
                   (let ((pair (get-char-property-and-overlay 2 'face)))
                   (list
                    (get-char-property 2 'face)
                    (car pair)
                    (overlay-get (cdr pair) 'face)
                    (get-pos-property 2 'face)
                    (get-pos-property 2 'carry)
                    (get-pos-property 3 'face)))))"#
        ),
        "OK (high high high nil after high)"
    );
}

#[test]
fn vm_add_face_text_property_uses_shared_face_merge_state() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (erase-buffer)
                 (insert "abcd")
                 (put-text-property 1 3 'face 'italic)
                 (add-face-text-property 1 3 'bold)
                 (add-face-text-property 1 3 'underline t)
                 (list
                  (get-text-property 1 'face)
                  (get-text-property 3 'face)))"#
        ),
        "OK ((bold italic underline) nil)"
    );
}

#[test]
fn vm_marker_builtins_use_shared_live_buffer_state() {
    assert_eq!(
        vm_eval_str(
            r#"(progn
                 (erase-buffer)
                 (insert "abcd")
                 (goto-char 3)
                 (let ((pm (point-marker))
                       (cm (copy-marker 3 t))
                       (minm (point-min-marker))
                       (maxm (point-max-marker)))
                   (goto-char 1)
                   (insert "Q")
                   (goto-char 4)
                   (insert "Z")
                   (list
                    (marker-position pm)
                    (marker-position minm)
                    (marker-position maxm)
                    (marker-position cm)
                    (progn (set-marker pm 2) (marker-position pm))
                    (progn (move-marker pm nil) (marker-position pm)))))"#
        ),
        "OK (4 1 7 5 2 nil)"
    );
}

#[test]
fn vm_mark_marker_uses_shared_buffer_mark_state() {
    assert_eq!(
        vm_eval_with_init_str("(marker-position (mark-marker))", |eval| {
            let current = eval.buffers.current_buffer_id().expect("current buffer");
            let _ = eval.buffers.replace_buffer_contents(current, "abcd");
            let _ = eval.buffers.set_buffer_mark(current, 2);
        }),
        "OK 3"
    );
}

#[test]
fn vm_symbol_mutator_type_errors_match_oracle() {
    with_vm_eval("(set 1 2)", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("symbolp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    });

    with_vm_eval("(fset 1 2)", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("symbolp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    });

    with_vm_eval("(get 1 'p)", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("symbolp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    });

    with_vm_eval("(put 1 'p 2)", false, |result| match result {
        Err(EvalError::Signal { symbol, data }) => {
            assert_eq!(resolve_sym(symbol), "wrong-type-argument");
            assert_eq!(data, vec![Value::symbol("symbolp"), Value::Int(1)]);
        }
        other => panic!("unexpected error: {other:?}"),
    });
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

#[test]
fn vm_lambda_parameters_can_shadow_nil_and_t() {
    assert_eq!(vm_eval_str("(funcall (lambda (t) t) 7)"), "OK 7");
    assert_eq!(vm_eval_str("(funcall (lambda (nil) nil) 9)"), "OK 9");
    assert_eq!(
        vm_eval_str("(mapcar (lambda (t) t) '(1 2 3))"),
        "OK (1 2 3)"
    );
    assert_eq!(
        vm_eval_str("(mapcar (lambda (nil) nil) '(4 5 6))"),
        "OK (4 5 6)"
    );
}

#[test]
fn vm_gnu_arg_descriptor_preserves_optional_and_rest_slots() {
    let func = ByteCodeFunction {
        ops: vec![
            Op::StackRef(4),
            Op::StackRef(4),
            Op::StackRef(4),
            Op::StackRef(4),
            Op::StackRef(4),
            Op::List(5),
            Op::Return,
        ],
        constants: vec![],
        max_stack: 10,
        params: crate::emacs_core::bytecode::decode::parse_arglist_descriptor(3 | (4 << 8) | 128),
        lexical: false,
        env: None,
        gnu_byte_offset_map: None,
        docstring: None,
        doc_form: None,
        interactive: None,
    };

    let mut eval = Evaluator::new_vm_harness();
    let mut vm = new_vm(&mut eval);

    let result = vm
        .execute(
            &func,
            vec![Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(4)],
        )
        .expect("vm should preserve GNU descriptor slot layout");

    assert_eq!(
        result,
        Value::list(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Nil,
        ])
    );
}

#[test]
fn vm_compiled_autoload_registration_updates_shared_autoload_manager() {
    let mut eval = Evaluator::new_vm_harness();
    let forms =
        parse_forms("(autoload 'vm-bytecode-auto \"vm-bytecode-auto-file\")").expect("parse");
    let mut compiler = Compiler::new(false);
    let func = compiler.compile_toplevel(&forms[0]);

    let result = {
        let mut vm = new_vm(&mut eval);
        vm.execute(&func, vec![])
            .expect("compiled autoload should execute")
    };

    assert_eq!(result, Value::symbol("vm-bytecode-auto"));
    let entry = eval
        .autoloads
        .get_entry("vm-bytecode-auto")
        .expect("autoload registration should propagate back out of VM bridge");
    assert_eq!(entry.file, "vm-bytecode-auto-file");
}

#[test]
fn vm_compiled_require_respects_recursive_require_guard() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = dir.path().join("vm-bytecode-rec.el");
    std::fs::write(
        &fixture,
        "(setq vm-bytecode-required-ran t)\n(provide 'vm-bytecode-rec)\n",
    )
    .expect("write require fixture");

    let mut eval = Evaluator::new_vm_harness();
    let forms = parse_forms(
        "(progn
           (setq vm-bytecode-required-ran nil)
           (require 'vm-bytecode-rec)
           vm-bytecode-required-ran)",
    )
    .expect("parse");
    let mut compiler = Compiler::new(false);
    let func = compiler.compile_toplevel(&forms[0]);
    eval.obarray.set_symbol_value(
        "load-path",
        Value::list(vec![Value::string(dir.path().to_string_lossy())]),
    );
    eval.require_stack = vec![intern("vm-bytecode-rec")];

    let result = {
        let mut vm = new_vm(&mut eval);
        vm.execute(&func, vec![])
            .expect("compiled require should observe recursive guard")
    };

    assert_eq!(
        result,
        Value::Nil,
        "compiled require should return immediately without loading the file again"
    );
}

#[test]
fn vm_compiled_require_loads_feature_with_nil_filename_through_shared_runtime() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = dir.path().join("vm-bytecode-load.el");
    std::fs::write(
        &fixture,
        "(setq vm-bytecode-required-ran t)\n(provide 'vm-bytecode-load)\n",
    )
    .expect("write require fixture");

    let mut eval = Evaluator::new_vm_harness();
    let forms = parse_forms(
        "(progn
           (setq vm-bytecode-required-ran nil)
           (list
             (require 'vm-bytecode-load nil nil)
             vm-bytecode-required-ran
             (featurep 'vm-bytecode-load)))",
    )
    .expect("parse");
    let mut compiler = Compiler::new(false);
    let func = compiler.compile_toplevel(&forms[0]);
    eval.obarray.set_symbol_value(
        "load-path",
        Value::list(vec![Value::string(dir.path().to_string_lossy())]),
    );

    let result = {
        let mut vm = new_vm(&mut eval);
        vm.execute(&func, vec![])
            .expect("compiled require should load feature through shared runtime")
    };

    assert_eq!(
        result,
        Value::list(vec![
            Value::symbol("vm-bytecode-load"),
            Value::True,
            Value::True,
        ])
    );
    assert!(
        eval.features.contains(&intern("vm-bytecode-load")),
        "compiled require should update shared features state"
    );
    assert!(
        eval.require_stack.is_empty(),
        "compiled require should unwind shared require stack after load"
    );
}

#[test]
fn vm_compiled_load_uses_shared_runtime_and_restores_load_file_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = dir.path().join("vm-bytecode-shared-load.el");
    std::fs::write(&fixture, "(setq vm-bytecode-load-seen load-file-name)\n")
        .expect("write load fixture");

    let mut eval = Evaluator::new_vm_harness();
    let forms = parse_forms(
        "(list
           (load \"vm-bytecode-shared-load\" nil nil nil nil)
           vm-bytecode-load-seen
           load-file-name)",
    )
    .expect("parse");
    let mut compiler = Compiler::new(false);
    let func = compiler.compile_toplevel(&forms[0]);
    eval.obarray.set_symbol_value(
        "load-path",
        Value::list(vec![Value::string(dir.path().to_string_lossy())]),
    );

    let result = {
        let mut vm = new_vm(&mut eval);
        vm.execute(&func, vec![])
            .expect("compiled load should resolve path and execute through shared runtime")
    };

    assert_eq!(
        result,
        Value::list(vec![
            Value::True,
            Value::string(fixture.to_string_lossy()),
            Value::Nil,
        ])
    );
    assert!(
        eval.loads_in_progress.is_empty(),
        "compiled load should unwind shared loads-in-progress state"
    );
}

#[test]
fn vm_compiled_load_respects_loads_in_progress_guard() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = dir.path().join("vm-bytecode-load.el");
    std::fs::write(&fixture, "(setq vm-bytecode-load-ran t)\n").expect("write load fixture");
    let fixture = fixture.canonicalize().expect("canonical load fixture");

    let mut eval = Evaluator::new_vm_harness();
    let forms = parse_forms(&format!(
        "(progn
           (setq vm-bytecode-load-ran nil)
           (load {:?} nil nil t)
           vm-bytecode-load-ran)",
        fixture.to_string_lossy()
    ))
    .expect("parse");
    let mut compiler = Compiler::new(false);
    let func = compiler.compile_toplevel(&forms[0]);
    eval.loads_in_progress = vec![fixture];

    let result = {
        let mut vm = new_vm(&mut eval);
        vm.execute(&func, vec![])
            .expect("compiled load should observe recursive load guard")
    };

    assert_eq!(
        result,
        Value::Nil,
        "compiled load should skip re-entering a file already being loaded"
    );
}

#[test]
fn vm_interactive_form_uses_shared_symbol_property_and_builtin_state() {
    assert_eq!(
        vm_eval_str(
            "(progn
               (fset 'vm-if-shared-target (lambda () 1))
               (fset 'vm-if-shared-alias 'vm-if-shared-target)
               (put 'vm-if-shared-alias 'interactive-form '(interactive \"P\"))
               (list
                 (interactive-form 'vm-if-shared-alias)
                 (interactive-form 'vm-if-shared-target)
                 (interactive-form 'forward-char)
                 (interactive-form 'goto-char)
                 (interactive-form 'car)))"
        ),
        "OK ((interactive \"P\") nil (interactive \"^p\") (interactive (goto-char--read-natnum-interactive \"Go to char: \")) nil)"
    );
}

#[test]
fn vm_interactive_form_uses_shared_autoload_load_bridge() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = dir.path().join("vm-interactive-form-auto.el");
    std::fs::write(
        &fixture,
        "(fset 'vm-interactive-form-auto
           '(lambda () (interactive \"P\") t))\n",
    )
    .expect("write interactive-form autoload fixture");

    let mut eval = Evaluator::new_vm_harness();
    let forms = parse_forms(
        "(progn
           (autoload 'vm-interactive-form-auto \"vm-interactive-form-auto\")
           (interactive-form 'vm-interactive-form-auto))",
    )
    .expect("parse");
    let mut compiler = Compiler::new(false);
    let func = compiler.compile_toplevel(&forms[0]);
    eval.obarray.set_symbol_value(
        "load-path",
        Value::list(vec![Value::string(dir.path().to_string_lossy())]),
    );

    let result = {
        let mut vm = new_vm(&mut eval);
        vm.execute(&func, vec![])
            .expect("compiled interactive-form should use shared autoload bridge")
    };

    assert_eq!(
        result,
        Value::list(vec![Value::symbol("interactive"), Value::string("P")])
    );
}

#[test]
fn vm_command_modes_uses_shared_symbol_and_bytecode_state() {
    assert_eq!(
        vm_eval_str(
            "(progn
               (fset 'vm-cm-shared-target '(lambda () t))
               (fset 'vm-cm-shared-alias 'vm-cm-shared-target)
               (put 'vm-cm-shared-alias 'command-modes '(foo-mode bar-mode))
               (let ((f (make-byte-code '() \"\" [] 0 nil [nil '(rust-ts-mode c-mode)])))
                 (fset 'vm-cm-shared-bytecode f))
               (list
                 (command-modes 'vm-cm-shared-alias)
                 (command-modes 'vm-cm-shared-target)
                 (command-modes '(lambda () (interactive \"p\" text-mode prog-mode) t))
                 (command-modes 'vm-cm-shared-bytecode)
                 (command-modes 'ignore)
                 (command-modes 'car)))"
        ),
        "OK ((foo-mode bar-mode) nil (text-mode prog-mode) (rust-ts-mode c-mode) nil nil)"
    );
}

#[test]
fn vm_commandp_uses_shared_command_metadata_state() {
    assert_eq!(
        vm_eval_str(
            "(let ((f (make-byte-code '() \"\" [] 0 nil [nil nil])))
               (list
                 (commandp 'forward-char)
                 (commandp 'car)
                 (commandp '(lambda () (interactive) t))
                 (commandp '(lambda () t))
                 (commandp \"abc\")
                 (commandp \"abc\" t)
                 (commandp [1 2 3])
                 (commandp [1 2 3] t)
                 (commandp f)))"
        ),
        "OK (t nil t nil t nil t nil t)"
    );
}
