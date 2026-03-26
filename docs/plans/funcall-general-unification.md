# Plan: Unify Function Dispatch with funcall_general

## Problem

NeoVM has two parallel function dispatch implementations:

- `apply_inner` in eval.rs (~55 lines) — tree-walking interpreter path
- `call_function` in vm.rs (~70 lines) — bytecode VM path

Both do the same thing: match on Value type (ByteCode, Lambda, Subr, Symbol,
Cons) and dispatch to the appropriate handler. This duplication caused the
autoload bug where one path handled autoloads and the other didn't.

GNU Emacs has ONE function: `funcall_general` (35 lines in eval.c). Both
`eval_sub` (interpreter) and `exec_byte_code` (bytecode VM) call it.

## GNU Emacs Architecture

```
eval_sub (interpreter)
  └─► funcall_general ──► funcall_subr (C builtins via fn pointer)
                       └─► funcall_lambda (Lisp functions)
                       └─► autoload_do_load + retry

exec_byte_code (bytecode VM)
  ├─► CLOSUREP → goto setup_frame (fast path: stay in VM)
  ├─► SUBRP → funcall_subr (direct fn pointer call)
  └─► everything else → funcall_general (shared with interpreter)
```

Key: the bytecode VM has ONE fast path (bytecoded closures stay in the VM
loop). Everything else delegates to the shared `funcall_general`.

## Current NeoVM Dispatch Comparison

| Value type | apply_inner (eval.rs) | call_function (vm.rs) |
|---|---|---|
| ByteCode | Create Vm, execute | self.execute_with_func_value (stay in VM) |
| Lambda | self.apply_lambda | eval_lambda_body_in_vm_runtime |
| Macro | self.apply_lambda | (not handled) |
| Subr | self.apply_subr_object | self.dispatch_vm_builtin |
| Symbol | self.apply_symbol_callable (named-call cache) | manual obarray lookup + autoload |
| Cons lambda | self.eval_value, re-apply | self.instantiate_callable_cons_form |
| Cons closure | self.convert_closure_cons_to_lambda | same as lambda cons |
| Cons autoload | signal error | signal error |
| Nil | void-function | (falls through to invalid-function) |

Same cases, different code paths, different edge-case handling.

## Plan

### Step 1: Create funcall_general on Context

Add a new method to Context that handles all function dispatch:

```rust
impl Context {
    pub(crate) fn funcall_general(&mut self, func: Value, args: Vec<Value>) -> EvalResult {
        match func {
            Value::Subr(id) => {
                // defsubr registry → function pointer call
                let name = resolve_sym(id);
                if let Some(result) = self.dispatch_subr(name, args) {
                    return result;
                }
                Err(signal("void-function", vec![Value::Subr(id)]))
            }
            Value::ByteCode(bc) => {
                // Create VM and execute bytecoded function
                self.refresh_features_from_variable();
                let bc_data = self.heap.get_bytecode(bc).clone();
                let mut vm = super::bytecode::Vm::from_context(self);
                let result = vm.execute_with_func_value(&bc_data, args, Value::ByteCode(bc));
                self.sync_features_variable();
                result
            }
            Value::Lambda(id) => {
                let lambda_data = self.heap.get_lambda(id).clone();
                self.apply_lambda(&lambda_data, args, Value::Lambda(id))
            }
            Value::Macro(id) => {
                let lambda_data = self.heap.get_macro_data(id).clone();
                self.apply_lambda(&lambda_data, args, Value::Macro(id))
            }
            Value::Symbol(id) => {
                // Resolve function cell, handle autoload, retry
                self.funcall_symbol(id, args)
            }
            Value::True => self.funcall_symbol(intern("t"), args),
            Value::Keyword(id) => self.funcall_symbol(id, args),
            Value::Nil => Err(signal("void-function", vec![Value::symbol("nil")])),
            function @ Value::Cons(_) => self.funcall_cons(function, args),
            other => Err(signal("invalid-function", vec![other])),
        }
    }

    fn funcall_symbol(&mut self, sym_id: SymId, args: Vec<Value>) -> EvalResult {
        let name = resolve_sym(sym_id);
        if let Some(func) = self.obarray.symbol_function(name).cloned() {
            if func.is_nil() {
                return Err(signal("void-function", vec![Value::symbol(name)]));
            }
            // Handle autoload
            if super::autoload::is_autoload_value(&func) {
                super::autoload::builtin_autoload_do_load(
                    self,
                    vec![func, Value::symbol(name)],
                )?;
                // Retry after loading
                if let Some(loaded) = self.obarray.symbol_function(name).cloned() {
                    return self.funcall_general(loaded, args);
                }
                return Err(signal("void-function", vec![Value::symbol(name)]));
            }
            return self.funcall_general(func, args);
        }
        // Check if it's a registered builtin
        if let Some(result) = self.dispatch_subr(name, args) {
            return result;
        }
        Err(signal("void-function", vec![Value::symbol(name)]))
    }

    fn funcall_cons(&mut self, function: Value, args: Vec<Value>) -> EvalResult {
        if super::autoload::is_autoload_value(&function) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), function],
            ));
        }
        if function.cons_car().is_symbol_named("lambda") {
            match self.eval_value(&function) {
                Ok(callable) => self.funcall_general(callable, args),
                Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument" => {
                    Err(signal("invalid-function", vec![function]))
                }
                Err(err) => Err(err),
            }
        } else if function.cons_car().is_symbol_named("closure") {
            match self.convert_closure_cons_to_lambda(function) {
                Ok(callable) => self.funcall_general(callable, args),
                Err(_) => Err(signal("invalid-function", vec![function])),
            }
        } else {
            Err(signal("invalid-function", vec![function]))
        }
    }
}
```

### Step 2: Change apply to use funcall_general

```rust
impl Context {
    pub(crate) fn apply(&mut self, function: Value, args: Vec<Value>) -> EvalResult {
        let saved_roots = self.save_temp_roots();
        self.push_temp_root(function);
        for &arg in &args {
            self.push_temp_root(arg);
        }
        let result = stacker::maybe_grow(EVAL_STACK_RED_ZONE, EVAL_STACK_SEGMENT, || {
            self.funcall_general(function, args)
        });
        self.restore_temp_roots(saved_roots);
        result
    }
}
```

Delete `apply_inner` entirely.

### Step 3: Change VM's call_function to delegate

```rust
impl Vm {
    fn call_function(&mut self, func_val: Value, args: Vec<Value>) -> EvalResult {
        match func_val {
            // Fast path: stay in VM for bytecoded calls
            // (matches GNU Emacs's CLOSUREP → goto setup_frame)
            Value::ByteCode(_) => {
                let bc_data = func_val.get_bytecode_data().unwrap().clone();
                self.execute_with_func_value(&bc_data, args, func_val)
            }
            // Everything else: shared dispatch via funcall_general
            _ => self.ctx.funcall_general(func_val, args),
        }
    }
}
```

### Step 4: Delete dispatch_vm_builtin

The VM's `dispatch_vm_builtin` has special handling for apply, funcall,
funcall-interactively, throw, signal, and a few internal builtins. These
are already registered via defsubr, so `funcall_general` handles them
through `dispatch_subr`. Delete `dispatch_vm_builtin` (~120 lines).

The only reason `dispatch_vm_builtin` existed was because those builtins
needed `self.call_function` (the VM's dispatch loop). Now that
`funcall_general` lives on Context, these builtins can call
`ctx.funcall_general` directly from their defsubr-registered function.

For apply and funcall specifically: they're already registered as
`builtin_apply(eval, args)` and `builtin_funcall(eval, args)` which call
`eval.apply()`. Since `apply()` now delegates to `funcall_general`, the
VM naturally routes through the same path. No special VM handling needed.

### Step 5: Verify edge cases

Key things to test:
- Autoload triggering from both interpreter and VM
- funcall/apply from bytecoded code
- Macro expansion (only from interpreter, not VM — funcall_general handles
  Macro values, bytecode compiler compiles macros away)
- Recursive function calls (funcall_general → ByteCode → VM → call_function
  → funcall_general → ...)
- throw/catch across engine boundaries

## What This Eliminates

| Code | Lines | Status |
|---|---|---|
| apply_inner body | ~55 | Replaced by funcall_general call |
| call_function body | ~70 | Replaced by ByteCode fast path + funcall_general |
| dispatch_vm_builtin | ~120 | Deleted (defsubr handles all) |
| Autoload handling duplication | ~30 | One path in funcall_symbol |
| **Total** | **~275** | |

## What This Does NOT Change

- **498 _in_state functions**: These are called from individual builtin
  implementations, not from the dispatch path. They're internal delegation
  within each builtin. Can be inlined later, independently.

- **199 VM inline opcode calls to _in_state**: These are for hot opcodes
  (Bvarref, Bvarset, etc.) that bypass call_function entirely. They're
  performance optimizations, not dispatch duplication.

- **GC root management**: apply() still wraps funcall_general with
  save_temp_roots/restore_temp_roots.

- **Named-call cache**: Currently in apply_symbol_callable. Can be
  preserved in funcall_symbol or removed (defsubr makes it less
  necessary since function cell lookup is O(1)).

## Relationship to _in_state Cleanup

The `_in_state` cleanup is independent of `funcall_general`. It can be
done before, after, or never without affecting the dispatch unification.

If we do it later, the approach is:
1. Change VM opcode handlers from `_in_state(&self.ctx.obarray, ...)` to
   `_eval(&mut *self.ctx, args)` — 199 call sites in vm.rs
2. Inline _in_state bodies into _eval functions — 498 functions across
   43 files, purely mechanical (change `obarray` to `eval.obarray`, etc.)

## Risk Assessment

**Medium risk.** The dispatch path is the most critical code. But:
- funcall_general is a straightforward merge of two working functions
- The VM's ByteCode fast path is unchanged
- Both engines already share dispatch_subr for builtin calls
- The existing test suite catches behavioral differences
- Can be tested incrementally: create funcall_general first, then
  migrate apply_inner, then migrate call_function
