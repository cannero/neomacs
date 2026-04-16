//! Bytecode virtual machine — stack-based interpreter.

use std::collections::{HashMap, HashSet};

use super::chunk::ByteCodeFunction;
use super::opcode::Op;
use crate::buffer::{BufferManager, InsertionType};
use crate::emacs_core::advice::VariableWatcherList;
use crate::emacs_core::builtins;
use crate::emacs_core::coding::CodingSystemManager;
use crate::emacs_core::custom::CustomManager;
use crate::emacs_core::error::*;
use crate::emacs_core::eval::{ConditionFrame, Context, ResumeTarget};
use crate::emacs_core::intern::{SymId, intern, intern_uninterned, resolve_sym};
use crate::emacs_core::regex::MatchData;
// storage_char_len and storage_substring no longer needed here — using emacs_char + LispString
use crate::emacs_core::value::*;
use crate::window::{FrameId, FrameManager, Window};

/// Local marker for catch/condition-case frames mirrored into the shared
/// condition runtime.
#[derive(Clone, Debug)]
enum Handler {
    /// Local marker corresponding to a catch/condition-case frame already
    /// stored in `Context.condition_stack`.
    Condition,
}

use crate::emacs_core::eval::SpecBinding;

/// The bytecode VM execution engine.
///
/// Operates on an Context's obarray and dynamic binding stack.
pub struct Vm<'a> {
    ctx: &'a mut crate::emacs_core::eval::Context,
}

impl<'a> crate::emacs_core::hook_runtime::HookRuntime for Vm<'a> {
    fn hook_context(&self) -> &crate::emacs_core::eval::Context {
        &self.ctx
    }

    fn call_hook_callable(&mut self, function: Value, args: &[Value]) -> EvalResult {
        self.call_function_with_roots(function, args)
    }

    fn with_hook_root_scope<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, Flow>,
    ) -> Result<T, Flow> {
        self.with_dynamic_vm_roots(|vm| f(vm))
    }

    fn push_hook_root(&mut self, value: Value) {
        self.push_dynamic_vm_root(value);
    }
}

impl<'a> Vm<'a> {
    pub(crate) fn from_context(ctx: &'a mut crate::emacs_core::eval::Context) -> Self {
        Self { ctx }
    }

    /// Set the current depth and max_depth (inherited from the Context).
    pub fn set_depth(&mut self, depth: usize, max_depth: usize) {
        self.ctx.depth = depth;
        self.ctx.max_depth = max_depth;
    }

    /// Get the current depth (to sync back to the Context).
    pub fn get_depth(&self) -> usize {
        self.ctx.depth
    }

    fn with_dynamic_vm_roots<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        self.ctx.push_vm_root_frame();
        let result = f(self);
        self.ctx.pop_vm_root_frame();
        result
    }

    fn with_vm_root_scope<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let scope = self.ctx.save_vm_roots();
        let result = f(self);
        self.ctx.restore_vm_roots(scope);
        result
    }

    fn push_dynamic_vm_root(&mut self, value: Value) {
        self.ctx.push_vm_frame_root(value);
    }

    fn with_frame_roots<T>(
        &mut self,
        func: &ByteCodeFunction,
        extra: &[Value],
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.with_dynamic_vm_roots(|vm| {
            for value in func.constants.iter().copied() {
                vm.ctx.push_vm_frame_root(value);
            }
            // bc_buf is scanned by collect_roots — no need to snapshot stack.
            // specpdl entries are on ctx.specpdl which is already GC-traced.
            for value in extra.iter().copied() {
                vm.ctx.push_vm_frame_root(value);
            }
            f(vm)
        })
    }

    fn with_frame_arg_roots<T>(
        &mut self,
        func: &ByteCodeFunction,
        args: Vec<Value>,
        f: impl FnOnce(&mut Self, Vec<Value>) -> T,
    ) -> T {
        self.with_frame_roots(func, &[], |vm| {
            for value in args.iter().copied() {
                vm.ctx.push_vm_frame_root(value);
            }
            f(vm, args)
        })
    }

    fn with_frame_call_roots<T>(
        &mut self,
        func: &ByteCodeFunction,
        function: Value,
        args: Vec<Value>,
        f: impl FnOnce(&mut Self, Vec<Value>) -> T,
    ) -> T {
        self.with_frame_roots(func, &[], |vm| {
            vm.ctx.push_vm_frame_root(function);
            for value in args.iter().copied() {
                vm.ctx.push_vm_frame_root(value);
            }
            f(vm, args)
        })
    }

    fn with_macro_expansion_scope<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, Flow>,
    ) -> Result<T, Flow> {
        let state = self.ctx.begin_macro_expansion_scope();
        let result = f(self);
        self.ctx.finish_macro_expansion_scope(state);
        result
    }


    fn collect_flow_roots(flow: &Flow, out: &mut Vec<Value>) {
        match flow {
            Flow::Signal(sig) => {
                out.push(Value::from_sym_id(sig.symbol));
                out.extend(sig.data.iter().copied());
                if let Some(raw) = sig.raw_data {
                    out.push(raw);
                }
            }
            Flow::Throw { tag, value } => {
                out.push(*tag);
                out.push(*value);
            }
        }
    }

    fn result_roots(result: &EvalResult) -> Vec<Value> {
        let mut roots = Vec::new();
        match result {
            Ok(value) => roots.push(*value),
            Err(flow) => Self::collect_flow_roots(flow, &mut roots),
        }
        roots
    }

    /// Execute a bytecode function with given arguments.
    pub(crate) fn execute(&mut self, func: &ByteCodeFunction, args: Vec<Value>) -> EvalResult {
        self.execute_with_func_value(func, args, Value::NIL)
    }

    /// Execute a bytecode function, passing through the original function
    /// value for use in `wrong-number-of-arguments` error reporting.
    pub(crate) fn execute_with_func_value(
        &mut self,
        func: &ByteCodeFunction,
        args: Vec<Value>,
        func_value: Value,
    ) -> EvalResult {
        self.ctx.depth += 1;
        if self.ctx.depth > self.ctx.max_depth {
            let overflow_depth = self.ctx.depth as i64;
            self.ctx.depth -= 1;
            return Err(signal(
                "excessive-lisp-nesting",
                vec![Value::fixnum(overflow_depth)],
            ));
        }

        // Root the bytecode function's constants so they survive GC during
        // nested calls. Keep them in the active VM root frame so they remain
        // reachable even if the ByteCodeObj tracing has a gap (e.g. cloned
        // ByteCodeFunction whose constants diverge from the heap object, or
        // NIL func_value from sf_byte_code_value).
        let result = self.with_dynamic_vm_roots(|vm| {
            if func_value.is_heap_object() {
                vm.push_dynamic_vm_root(func_value);
            }
            for value in func.constants.iter().copied() {
                vm.push_dynamic_vm_root(value);
            }
            vm.run_frame(func, args, func_value)
        });
        self.ctx.depth -= 1;
        result
    }

    fn run_frame(
        &mut self,
        func: &ByteCodeFunction,
        args: Vec<Value>,
        func_value: Value,
    ) -> EvalResult {
        let condition_stack_base = self.ctx.condition_stack_len();
        let frame_base = self.ctx.bc_buf.len();
        self.ctx.bc_frames.push(crate::emacs_core::eval::BcFrame {
            base: frame_base,
            fun: func_value,
        });
        let mut pc: usize = 0;
        let mut handlers: Vec<Handler> = Vec::new();
        let specpdl_base = self.ctx.specpdl.len();
        let mut bind_stack: Vec<usize> = Vec::new();

        // Unified calling convention: push args onto the stack.
        // Both NeoVM-compiled and GNU-compiled bytecode use StackRef(n)
        // for parameter access.
        let nargs = args.len();
        let n_required = func.params.required.len();
        let n_optional = func.params.optional.len();
        let has_rest = func.params.rest.is_some();
        let nonrest = n_required + n_optional;

        // GNU Emacs validates bytecode arity before pushing the frame.
        // See src/bytecode.c: the VM checks the arg descriptor and signals
        // wrong-number-of-arguments immediately instead of nil-padding missing
        // required args.
        if !(n_required <= nargs && (has_rest || nargs <= nonrest)) {
            let max_val = if has_rest {
                Value::symbol("many")
            } else {
                Value::fixnum(nonrest as i64)
            };
            let arity = Value::cons(Value::fixnum(n_required as i64), max_val);
            self.ctx.bc_buf.truncate(frame_base);
            self.ctx.bc_frames.pop();
            return Err(signal(
                "wrong-number-of-arguments",
                vec![arity, Value::fixnum(nargs as i64)],
            ));
        }

        // Push required + optional args (pad with nil for missing optionals)
        for i in 0..nonrest {
            if i < nargs {
                let v = args[i];
                if v.is_string() {
                    let ptr = v.as_string_ptr().unwrap();
                    let hdr =
                        unsafe { &(*(ptr as *const crate::tagged::header::StringObj)).header };
                    if !matches!(hdr.kind, crate::tagged::header::HeapObjectKind::String) {
                        panic!(
                            "RUN_FRAME ARG BUG: arg[{}] = {:#x} (ptr {:?}, kind={:?}) is corrupt string. \
                             nargs={}, func has {} required, {} optional, rest={}",
                            i,
                            v.0,
                            ptr,
                            hdr.kind,
                            nargs,
                            func.params.required.len(),
                            func.params.optional.len(),
                            func.params.rest.is_some(),
                        );
                    }
                }
                self.ctx.bc_buf.push(v);
            } else {
                self.ctx.bc_buf.push(Value::NIL);
            }
        }

        // If &rest, collect remaining args into a list
        if has_rest {
            let rest_list = if nargs > nonrest {
                Value::list(args[nonrest..].to_vec())
            } else {
                Value::NIL
            };
            self.ctx.bc_buf.push(rest_list);
        }

        // GNU's bytecode stores lexical params at known stack positions; the
        // byte-compiler emits `byte-stack-ref` for every lexical reference,
        // so the param names are NOT looked up at runtime and don't need any
        // environment entry.  Dynamic params, on the other hand, are
        // referenced via `byte-varref` and must be specbound on the
        // function's specpdl span.  This split mirrors `byte-compile-bind`
        // in bytecomp.el and matches GNU's `funcall_lambda` (eval.c) ->
        // `exec_byte_code` (bytecode.c).  Building an intermediate
        // OrderedRuntimeBindingMap of params per call (which the previous
        // code did even for the lexical case) is dead work that dominated
        // debug-build batch-byte-compile runtime.
        let has_named_params = nonrest > 0 || has_rest;
        if has_named_params {
            if func.lexical || func.env.is_some() {
                // Lexical bytecode functions: params live on bc_buf at the
                // bottom of the frame.  Just install the captured closure
                // env (if any) and run; the body's stack-ref opcodes find
                // the params via frame_base.
                let saved_lexenv = if let Some(env) = func.env {
                    std::mem::replace(&mut self.ctx.lexenv, env)
                } else {
                    self.ctx.lexenv
                };
                let result = self.run_loop(func, frame_base, &mut pc, &mut handlers, &mut bind_stack);
                self.ctx.truncate_condition_stack(condition_stack_base);
                self.ctx.lexenv = saved_lexenv;
                self.ctx.unbind_to(specpdl_base);
                self.ctx.bc_buf.truncate(frame_base);
                self.ctx.bc_frames.pop();
                return result;
            }

            // Dynamic bytecode functions: each param needs a specbind so
            // that varref opcodes inside the body can find it via the
            // obarray.  Bind params directly from `args`/the rest list with
            // no intermediate map.
            let mut arg_idx = 0;
            for param in &func.params.required {
                let val = if arg_idx < nargs {
                    args[arg_idx]
                } else {
                    Value::NIL
                };
                crate::emacs_core::eval::specbind_in_state(
                    &mut self.ctx.obarray,
                    &mut self.ctx.specpdl,
                    *param,
                    val,
                );
                arg_idx += 1;
            }
            for param in &func.params.optional {
                let val = if arg_idx < nargs {
                    args[arg_idx]
                } else {
                    Value::NIL
                };
                crate::emacs_core::eval::specbind_in_state(
                    &mut self.ctx.obarray,
                    &mut self.ctx.specpdl,
                    *param,
                    val,
                );
                arg_idx += 1;
            }
            if let Some(rest_name) = func.params.rest {
                let rest_list = if arg_idx < nargs {
                    Value::list(args[arg_idx..].to_vec())
                } else {
                    Value::NIL
                };
                crate::emacs_core::eval::specbind_in_state(
                    &mut self.ctx.obarray,
                    &mut self.ctx.specpdl,
                    rest_name,
                    rest_list,
                );
            }
            let result = self.run_loop(func, frame_base, &mut pc, &mut handlers, &mut bind_stack);
            self.ctx.truncate_condition_stack(condition_stack_base);
            self.ctx.unbind_to(specpdl_base);
            self.ctx.bc_buf.truncate(frame_base);
            self.ctx.bc_frames.pop();
            return result;
        }

        // No params: set up lexenv for lexical closures/functions, then run.
        let saved_lexenv = if let Some(env) = func.env {
            Some(std::mem::replace(&mut self.ctx.lexenv, env))
        } else if func.lexical {
            Some(self.ctx.lexenv)
        } else {
            None
        };

        let result = self.run_loop(func, frame_base, &mut pc, &mut handlers, &mut bind_stack);
        self.ctx.truncate_condition_stack(condition_stack_base);

        if let Some(old) = saved_lexenv {
            self.ctx.lexenv = old;
        }
        self.ctx.unbind_to(specpdl_base);
        self.ctx.bc_buf.truncate(frame_base);
        self.ctx.bc_frames.pop();
        result
    }

    fn run_loop(
        &mut self,
        func: &ByteCodeFunction,
        frame_base: usize,
        pc: &mut usize,
        handlers: &mut Vec<Handler>,
        bind_stack: &mut Vec<usize>,
    ) -> EvalResult {
        let ops = &func.ops;
        let constants = &func.constants;

        macro_rules! stk {
            () => {
                self.ctx.bc_buf
            };
        }

        // Debug: validate string values before pushing to bc_buf
        macro_rules! stk_push {
            ($val:expr) => {{
                let v = $val;
                #[cfg(debug_assertions)]
                if v.is_string() {
                    let ptr = v.as_string_ptr().unwrap();
                    let hdr =
                        unsafe { &(*(ptr as *const crate::tagged::header::StringObj)).header };
                    if !matches!(hdr.kind, crate::tagged::header::HeapObjectKind::String) {
                        panic!(
                            "BC_BUF PUSH BUG: pushing corrupt string {:#x} (ptr {:?}, kind={:?}) \
                             at pc={}, op={:?}, bc_buf.len()={}, frame_base={}",
                            v.0,
                            ptr,
                            hdr.kind,
                            *pc - 1,
                            ops.get(*pc - 1),
                            stk!().len(),
                            frame_base,
                        );
                    }
                }
                self.ctx.bc_buf.push(v);
            }};
        }

        macro_rules! vm_try {
            ($expr:expr) => {{
                match $expr {
                    Ok(value) => value,
                    Err(flow) => {
                        self.resume_nonlocal(func, pc, handlers, bind_stack, flow)?;
                        continue;
                    }
                }
            }};
        }

        while *pc < ops.len() {
            let op = &ops[*pc];
            *pc += 1;

            match op {
                // -- Constants and stack --
                Op::Constant(idx) => {
                    stk_push!(constants[*idx as usize]);
                }
                Op::Nil => stk_push!(Value::NIL),
                Op::True => stk_push!(Value::T),
                Op::Pop => {
                    stk!().pop();
                }
                Op::Dup => {
                    if let Some(&top) = stk!().last() {
                        stk_push!(top);
                    }
                }
                Op::StackRef(n) => {
                    let idx = stk!().len().saturating_sub(1 + *n as usize);
                    let val = stk!()[idx];
                    stk_push!(val);
                }
                Op::StackSet(n) => {
                    if stk!().is_empty() {
                        continue;
                    }
                    let n = *n as usize;
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    if n == 0 {
                        continue;
                    }
                    if n <= stk!().len() {
                        let idx = stk!().len() - n;
                        stk!()[idx] = val;
                    }
                }
                Op::DiscardN(raw) => {
                    let preserve_tos = (raw & 0x80) != 0;
                    let mut n = (raw & 0x7F) as usize;
                    if n == 0 {
                        continue;
                    }
                    n = n.min(stk!().len());
                    if preserve_tos && n < stk!().len() {
                        if let Some(&top) = stk!().last() {
                            let target = stk!().len() - 1 - n;
                            stk!()[target] = top;
                        }
                    }
                    let new_len = stk!().len().saturating_sub(n);
                    stk!().truncate(new_len);
                }

                // -- Variable access --
                Op::VarRef(idx) => {
                    let name_id = sym_id_at(constants, *idx);
                    let val = vm_try!(self.lookup_var_id(name_id));
                    stk_push!(val);
                }
                Op::VarSet(idx) => {
                    let name_id = sym_id_at(constants, *idx);
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let extra = [val];
                    vm_try!(self.with_frame_roots(func, &extra, |vm| {
                        vm.assign_var_id(name_id, val)
                    },));
                }
                Op::VarBind(idx) => {
                    // GNU bytecode.c Bvarbind: `specbind (vectorp[arg], POP);`
                    // — always a dynamic binding, no lexical fallback. The
                    // byte-compiler (bytecomp.el byte-compile-bind) emits
                    // `byte-varbind` ONLY for variables that
                    // `cconv--not-lexical-var-p` reports as dynamic — i.e.
                    // members of `byte-compile-bound-variables`, populated
                    // from the file's top-level `(defvar VAR)` declarations
                    // among other sources. Lexical `let` bindings never get
                    // a varbind opcode at all; they live on the value stack
                    // and are tracked via `byte-compile--lexical-environment`.
                    //
                    // Therefore the VM must NOT second-guess the byte-compiler
                    // by inspecting `is_special_id` / `lexenv_declares_special`
                    // at runtime. Doing so misroutes file-local-only dynamic
                    // declarations (e.g. `(defvar cconv-freevars-alist)` in
                    // cconv.el — declared special locally but not globally) to
                    // the lexenv, where they are invisible to other functions
                    // called from the let body and surface as `void-variable`.
                    let name_id = sym_id_at(constants, *idx);
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    bind_stack.push(self.ctx.specpdl.len());
                    self.ctx.specbind(name_id, val);
                }
                Op::Unbind(n) => {
                    let n = *n as usize;
                    let target = if n <= bind_stack.len() {
                        let depth = bind_stack[bind_stack.len() - n];
                        bind_stack.truncate(bind_stack.len() - n);
                        depth
                    } else {
                        bind_stack.clear();
                        0
                    };
                    self.ctx.unbind_to(target);
                }

                // -- Function calls --
                Op::Call(n) => {
                    let n = *n as usize;
                    let args_start = stk!().len().saturating_sub(n);
                    let args: Vec<Value> = stk!().drain(args_start..).collect();
                    let func_val = stk!().pop().unwrap_or(Value::NIL);
                    let writeback_names = self.writeback_callable_names(&func_val);
                    let writeback_args = args.clone();
                    let result = vm_try!(self.with_frame_call_roots(
                        func,
                        func_val,
                        args,
                        |vm, args| vm.call_function(func_val, args),
                    ));
                    if let Some((called_name, alias_target)) = writeback_names.as_ref() {
                        self.maybe_writeback_mutating_first_arg(
                            called_name,
                            alias_target.as_deref(),
                            &writeback_args,
                            &result,
                        );
                    }
                    stk_push!(result);
                }
                Op::Apply(n) => {
                    let n = *n as usize;
                    if n == 0 {
                        let func_val = stk!().pop().unwrap_or(Value::NIL);
                        let result = vm_try!(self.with_frame_call_roots(
                        func,
                            func_val,
                            vec![],
                            |vm, args| vm.call_function(func_val, args),
                        ));
                        stk_push!(result);
                    } else {
                        let args_start = stk!().len().saturating_sub(n);
                        let mut args: Vec<Value> = stk!().drain(args_start..).collect();
                        let func_val = stk!().pop().unwrap_or(Value::NIL);
                        // Spread last argument
                        if let Some(last) = args.pop() {
                            let spread = list_to_vec(&last).unwrap_or_default();
                            args.extend(spread);
                        }
                        let writeback_names = self.writeback_callable_names(&func_val);
                        let writeback_args = args.clone();
                        let result = vm_try!(self.with_frame_call_roots(
                        func,
                            func_val,
                            args,
                            |vm, args| vm.call_function(func_val, args),
                        ));
                        if let Some((called_name, alias_target)) = writeback_names.as_ref() {
                            self.maybe_writeback_mutating_first_arg(
                                called_name,
                                alias_target.as_deref(),
                                &writeback_args,
                                &result,
                            );
                        }
                        stk_push!(result);
                    }
                }

                // -- Control flow --
                Op::Goto(addr) => {
                    *pc = *addr as usize;
                }
                Op::GotoIfNil(addr) => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    if val.is_nil() {
                        *pc = *addr as usize;
                    }
                }
                Op::GotoIfNotNil(addr) => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    if val.is_truthy() {
                        *pc = *addr as usize;
                    }
                }
                Op::GotoIfNilElsePop(addr) => {
                    if stk!().last().is_none_or(|v| v.is_nil()) {
                        *pc = *addr as usize;
                    } else {
                        stk!().pop();
                    }
                }
                Op::GotoIfNotNilElsePop(addr) => {
                    if stk!().last().is_some_and(|v| v.is_truthy()) {
                        *pc = *addr as usize;
                    } else {
                        stk!().pop();
                    }
                }
                Op::Switch => {
                    let jump_table = stk!().pop().unwrap_or(Value::NIL);
                    let dispatch = stk!().pop().unwrap_or(Value::NIL);

                    if !matches!(
                        jump_table.kind(),
                        ValueKind::Veclike(VecLikeType::HashTable)
                    ) {
                        self.resume_nonlocal(
                            func,
                            pc,
                            handlers,
                            bind_stack,
                            signal(
                                "wrong-type-argument",
                                vec![Value::symbol("hash-table-p"), jump_table],
                            ),
                        )?;
                        continue;
                    }

                    let ht = jump_table.as_hash_table().unwrap();
                    let key = dispatch.to_hash_key(&ht.test);
                    let target = ht.data.get(&key).copied();

                    match target {
                        Some(target_val) => match target_val.kind() {
                            ValueKind::Fixnum(addr) => {
                                *pc = vm_try!(resolve_switch_target(func, addr));
                            }
                            _ => {
                                vm_try!(Err(signal(
                                    "wrong-type-argument",
                                    vec![Value::symbol("integerp"), target_val],
                                )));
                            }
                        },
                        None => {}
                    }
                }
                Op::Return => {
                    return Ok(stk!().pop().unwrap_or(Value::NIL));
                }
                Op::SaveCurrentBuffer => {
                    if let Some(buffer_id) =
                        self.ctx.buffers.current_buffer().map(|buffer| buffer.id)
                    {
                        bind_stack.push(self.ctx.specpdl.len());
                        self.ctx.specpdl.push(SpecBinding::SaveCurrentBuffer { buffer_id });
                    }
                }
                Op::SaveExcursion => {
                    if let Some((buffer_id, point)) = self
                        .ctx
                        .buffers
                        .current_buffer()
                        .map(|buffer| (buffer.id, buffer.pt_byte))
                    {
                        let marker_id =
                            self.ctx
                                .buffers
                                .create_marker(buffer_id, point, InsertionType::Before);
                        bind_stack.push(self.ctx.specpdl.len());
                        self.ctx.specpdl.push(SpecBinding::SaveExcursion {
                            buffer_id,
                            marker_id,
                        });
                    }
                }
                Op::SaveRestriction => {
                    if let Some(saved) = self.ctx.buffers.save_current_restriction_state() {
                        bind_stack.push(self.ctx.specpdl.len());
                        self.ctx.specpdl.push(SpecBinding::SaveRestriction { state: saved });
                    }
                }

                // -- Arithmetic --
                // Inline fixnum fast paths match GNU Emacs bytecode.c design:
                // the bytecode opcode IS the contract — no override check needed.
                Op::Add => {
                    let len = stk!().len();
                    let b = stk!()[len - 1];
                    let a = stk!()[len - 2];
                    if let (Some(av), Some(bv)) = (a.as_fixnum(), b.as_fixnum()) {
                        let res = av.wrapping_add(bv);
                        if res >= Value::MOST_NEGATIVE_FIXNUM && res <= Value::MOST_POSITIVE_FIXNUM {
                            stk!()[len - 2] = Value::fixnum(res);
                            stk!().pop();
                        } else {
                            stk!().truncate(len - 2);
                            let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, "+", vec![a, b]));
                            stk_push!(result);
                        }
                    } else {
                        stk!().truncate(len - 2);
                        let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, "+", vec![a, b]));
                        stk_push!(result);
                    }
                }
                Op::Sub => {
                    let len = stk!().len();
                    let b = stk!()[len - 1];
                    let a = stk!()[len - 2];
                    if let (Some(av), Some(bv)) = (a.as_fixnum(), b.as_fixnum()) {
                        let res = av.wrapping_sub(bv);
                        if res >= Value::MOST_NEGATIVE_FIXNUM && res <= Value::MOST_POSITIVE_FIXNUM {
                            stk!()[len - 2] = Value::fixnum(res);
                            stk!().pop();
                        } else {
                            stk!().truncate(len - 2);
                            let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, "-", vec![a, b]));
                            stk_push!(result);
                        }
                    } else {
                        stk!().truncate(len - 2);
                        let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, "-", vec![a, b]));
                        stk_push!(result);
                    }
                }
                Op::Mul => {
                    let len = stk!().len();
                    let b = stk!()[len - 1];
                    let a = stk!()[len - 2];
                    if let (Some(av), Some(bv)) = (a.as_fixnum(), b.as_fixnum()) {
                        if let Some(res) = av.checked_mul(bv) {
                            if res >= Value::MOST_NEGATIVE_FIXNUM && res <= Value::MOST_POSITIVE_FIXNUM {
                                stk!()[len - 2] = Value::fixnum(res);
                                stk!().pop();
                            } else {
                                stk!().truncate(len - 2);
                                let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, "*", vec![a, b]));
                                stk_push!(result);
                            }
                        } else {
                            stk!().truncate(len - 2);
                            let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, "*", vec![a, b]));
                            stk_push!(result);
                        }
                    } else {
                        stk!().truncate(len - 2);
                        let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, "*", vec![a, b]));
                        stk_push!(result);
                    }
                }
                Op::Div => {
                    let len = stk!().len();
                    let b = stk!()[len - 1];
                    let a = stk!()[len - 2];
                    if let (Some(av), Some(bv)) = (a.as_fixnum(), b.as_fixnum()) {
                        if bv != 0 {
                            // Emacs truncation division (towards zero), matching C semantics
                            let res = if (av < 0) != (bv < 0) && av % bv != 0 {
                                av / bv
                            } else {
                                av / bv
                            };
                            stk!()[len - 2] = Value::fixnum(res);
                            stk!().pop();
                        } else {
                            stk!().truncate(len - 2);
                            let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, "/", vec![a, b]));
                            stk_push!(result);
                        }
                    } else {
                        stk!().truncate(len - 2);
                        let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, "/", vec![a, b]));
                        stk_push!(result);
                    }
                }
                Op::Rem => {
                    let len = stk!().len();
                    let b = stk!()[len - 1];
                    let a = stk!()[len - 2];
                    if let (Some(av), Some(bv)) = (a.as_fixnum(), b.as_fixnum()) {
                        if bv != 0 {
                            stk!()[len - 2] = Value::fixnum(av % bv);
                            stk!().pop();
                        } else {
                            stk!().truncate(len - 2);
                            let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, "%", vec![a, b]));
                            stk_push!(result);
                        }
                    } else {
                        stk!().truncate(len - 2);
                        let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, "%", vec![a, b]));
                        stk_push!(result);
                    }
                }
                Op::Add1 => {
                    let top = *stk!().last().unwrap();
                    if let Some(n) = top.as_fixnum() {
                        if n != Value::MOST_POSITIVE_FIXNUM {
                            *stk!().last_mut().unwrap() = Value::fixnum(n + 1);
                        } else {
                            stk!().pop();
                            let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, "1+", vec![top]));
                            stk_push!(result);
                        }
                    } else {
                        stk!().pop();
                        let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, "1+", vec![top]));
                        stk_push!(result);
                    }
                }
                Op::Sub1 => {
                    let top = *stk!().last().unwrap();
                    if let Some(n) = top.as_fixnum() {
                        if n != Value::MOST_NEGATIVE_FIXNUM {
                            *stk!().last_mut().unwrap() = Value::fixnum(n - 1);
                        } else {
                            stk!().pop();
                            let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, "1-", vec![top]));
                            stk_push!(result);
                        }
                    } else {
                        stk!().pop();
                        let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, "1-", vec![top]));
                        stk_push!(result);
                    }
                }
                Op::Negate => {
                    let top = *stk!().last().unwrap();
                    if let Some(n) = top.as_fixnum() {
                        if n != Value::MOST_NEGATIVE_FIXNUM {
                            *stk!().last_mut().unwrap() = Value::fixnum(-n);
                        } else {
                            stk!().pop();
                            let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, "-", vec![top]));
                            stk_push!(result);
                        }
                    } else {
                        stk!().pop();
                        let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, "-", vec![top]));
                        stk_push!(result);
                    }
                }

                // -- Comparison --
                // Inline fixnum fast paths match GNU Emacs bytecode.c.
                Op::Eqlsign => {
                    let len = stk!().len();
                    let b = stk!()[len - 1];
                    let a = stk!()[len - 2];
                    if a.is_fixnum() && b.is_fixnum() {
                        stk!()[len - 2] = if a.0 == b.0 { Value::T } else { Value::NIL };
                        stk!().pop();
                    } else {
                        stk!().truncate(len - 2);
                        let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, "=", vec![a, b]));
                        stk_push!(result);
                    }
                }
                Op::Gtr => {
                    let len = stk!().len();
                    let b = stk!()[len - 1];
                    let a = stk!()[len - 2];
                    if let (Some(av), Some(bv)) = (a.as_fixnum(), b.as_fixnum()) {
                        stk!()[len - 2] = if av > bv { Value::T } else { Value::NIL };
                        stk!().pop();
                    } else {
                        stk!().truncate(len - 2);
                        let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, ">", vec![a, b]));
                        stk_push!(result);
                    }
                }
                Op::Lss => {
                    let len = stk!().len();
                    let b = stk!()[len - 1];
                    let a = stk!()[len - 2];
                    if let (Some(av), Some(bv)) = (a.as_fixnum(), b.as_fixnum()) {
                        stk!()[len - 2] = if av < bv { Value::T } else { Value::NIL };
                        stk!().pop();
                    } else {
                        stk!().truncate(len - 2);
                        let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, "<", vec![a, b]));
                        stk_push!(result);
                    }
                }
                Op::Leq => {
                    let len = stk!().len();
                    let b = stk!()[len - 1];
                    let a = stk!()[len - 2];
                    if let (Some(av), Some(bv)) = (a.as_fixnum(), b.as_fixnum()) {
                        stk!()[len - 2] = if av <= bv { Value::T } else { Value::NIL };
                        stk!().pop();
                    } else {
                        stk!().truncate(len - 2);
                        let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, "<=", vec![a, b]));
                        stk_push!(result);
                    }
                }
                Op::Geq => {
                    let len = stk!().len();
                    let b = stk!()[len - 1];
                    let a = stk!()[len - 2];
                    if let (Some(av), Some(bv)) = (a.as_fixnum(), b.as_fixnum()) {
                        stk!()[len - 2] = if av >= bv { Value::T } else { Value::NIL };
                        stk!().pop();
                    } else {
                        stk!().truncate(len - 2);
                        let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, ">=", vec![a, b]));
                        stk_push!(result);
                    }
                }
                Op::Max => {
                    let len = stk!().len();
                    let b = stk!()[len - 1];
                    let a = stk!()[len - 2];
                    if let (Some(av), Some(bv)) = (a.as_fixnum(), b.as_fixnum()) {
                        stk!()[len - 2] = if av >= bv { a } else { b };
                        stk!().pop();
                    } else {
                        stk!().truncate(len - 2);
                        let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, "max", vec![a, b]));
                        stk_push!(result);
                    }
                }
                Op::Min => {
                    let len = stk!().len();
                    let b = stk!()[len - 1];
                    let a = stk!()[len - 2];
                    if let (Some(av), Some(bv)) = (a.as_fixnum(), b.as_fixnum()) {
                        stk!()[len - 2] = if av <= bv { a } else { b };
                        stk!().pop();
                    } else {
                        stk!().truncate(len - 2);
                        let result = vm_try!(self.dispatch_vm_builtin_with_frame(func, "min", vec![a, b]));
                        stk_push!(result);
                    }
                }

                // -- List operations --
                // Inline car/cdr/car-safe/cdr-safe match GNU Emacs exactly:
                // direct cons field access, nil passthrough, error on wrong type.
                Op::Car => {
                    let top = stk!().last_mut().unwrap();
                    if top.is_cons() {
                        *top = top.cons_car();
                    } else if !top.is_nil() {
                        let val = *top;
                        stk!().pop();
                        vm_try!(Err(signal("wrong-type-argument", vec![Value::symbol("listp"), val])));
                    }
                    // nil → nil: no change needed
                }
                Op::Cdr => {
                    let top = stk!().last_mut().unwrap();
                    if top.is_cons() {
                        *top = top.cons_cdr();
                    } else if !top.is_nil() {
                        let val = *top;
                        stk!().pop();
                        vm_try!(Err(signal("wrong-type-argument", vec![Value::symbol("listp"), val])));
                    }
                }
                Op::CarSafe => {
                    let top = stk!().last_mut().unwrap();
                    *top = if top.is_cons() { top.cons_car() } else { Value::NIL };
                }
                Op::CdrSafe => {
                    let top = stk!().last_mut().unwrap();
                    *top = if top.is_cons() { top.cons_cdr() } else { Value::NIL };
                }
                Op::Cons => {
                    let len = stk!().len();
                    let cdr_val = stk!()[len - 1];
                    let car_val = stk!()[len - 2];
                    stk!()[len - 2] = Value::cons(car_val, cdr_val);
                    stk!().pop();
                }
                Op::List(n) => {
                    let n = *n as usize;
                    let start = stk!().len().saturating_sub(n);
                    let items: Vec<Value> = stk!().drain(start..).collect();
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "list",
                        items.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(func, "list", items,))
                    };
                    stk_push!(result);
                }
                Op::Length => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![val];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "length",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                                func, "length", call_args,
                        ))
                    };
                    stk_push!(result);
                }
                Op::Nth => {
                    let list = stk!().pop().unwrap_or(Value::NIL);
                    let n = stk!().pop().unwrap_or(Value::fixnum(0));
                    let call_args = vec![n, list];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "nth",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(
                            self.dispatch_vm_builtin_with_frame(func, "nth", call_args,)
                        )
                    };
                    stk_push!(result);
                }
                Op::Nthcdr => {
                    let list = stk!().pop().unwrap_or(Value::NIL);
                    let n = stk!().pop().unwrap_or(Value::fixnum(0));
                    let call_args = vec![n, list];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "nthcdr",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                                func, "nthcdr", call_args,
                        ))
                    };
                    stk_push!(result);
                }
                Op::Elt => {
                    let idx = stk!().pop().unwrap_or(Value::NIL);
                    let seq = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![seq, idx];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "elt",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(
                            self.dispatch_vm_builtin_with_frame(func, "elt", call_args,)
                        )
                    };
                    stk_push!(result);
                }
                Op::Setcar => {
                    let newcar = stk!().pop().unwrap_or(Value::NIL);
                    let cell = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![cell, newcar];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "setcar",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                                func, "setcar", call_args,
                        ))
                    };
                    stk_push!(result);
                }
                Op::Setcdr => {
                    let newcdr = stk!().pop().unwrap_or(Value::NIL);
                    let cell = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![cell, newcdr];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "setcdr",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                                func, "setcdr", call_args,
                        ))
                    };
                    stk_push!(result);
                }
                Op::Nconc => {
                    let b = stk!().pop().unwrap_or(Value::NIL);
                    let a = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![a, b];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "nconc",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(
                            self.dispatch_vm_builtin_with_frame(func, "nconc", call_args,)
                        )
                    };
                    stk_push!(result);
                }
                Op::Nreverse => {
                    let list = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![list];
                    let result =
                        if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                            "nreverse",
                            call_args.clone(),
                        )) {
                            result
                        } else {
                            vm_try!(self.dispatch_vm_builtin_with_frame(
                                func, "nreverse", call_args,
                            ))
                        };
                    stk_push!(result);
                }
                Op::Member => {
                    let list = stk!().pop().unwrap_or(Value::NIL);
                    let elt = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![elt, list];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "member",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                                func, "member", call_args,
                        ))
                    };
                    stk_push!(result);
                }
                Op::Memq => {
                    let list = stk!().pop().unwrap_or(Value::NIL);
                    let elt = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![elt, list];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "memq",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(
                            self.dispatch_vm_builtin_with_frame(func, "memq", call_args,)
                        )
                    };
                    stk_push!(result);
                }
                Op::Assq => {
                    let alist = stk!().pop().unwrap_or(Value::NIL);
                    let key = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![key, alist];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "assq",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(
                            self.dispatch_vm_builtin_with_frame(func, "assq", call_args,)
                        )
                    };
                    stk_push!(result);
                }

                // -- Type predicates --
                // -- Type predicates --
                // Pure inline tag checks, zero function calls. Matches GNU exactly.
                Op::Symbolp => {
                    let top = stk!().last_mut().unwrap();
                    *top = if top.is_symbol() { Value::T } else { Value::NIL };
                }
                Op::Consp => {
                    let top = stk!().last_mut().unwrap();
                    *top = if top.is_cons() { Value::T } else { Value::NIL };
                }
                Op::Stringp => {
                    let top = stk!().last_mut().unwrap();
                    *top = if top.is_string() { Value::T } else { Value::NIL };
                }
                Op::Listp => {
                    let top = stk!().last_mut().unwrap();
                    *top = if top.is_cons() || top.is_nil() { Value::T } else { Value::NIL };
                }
                Op::Integerp => {
                    let top = stk!().last_mut().unwrap();
                    *top = if top.is_fixnum() { Value::T } else { Value::NIL };
                }
                Op::Numberp => {
                    let top = stk!().last_mut().unwrap();
                    *top = if top.is_fixnum() || top.is_float() { Value::T } else { Value::NIL };
                }
                Op::Null | Op::Not => {
                    let top = stk!().last_mut().unwrap();
                    *top = if top.is_nil() { Value::T } else { Value::NIL };
                }
                Op::Eq => {
                    let len = stk!().len();
                    let b = stk!()[len - 1];
                    let a = stk!()[len - 2];
                    stk!()[len - 2] = if a.0 == b.0 { Value::T } else { Value::NIL };
                    stk!().pop();
                }
                Op::Equal => {
                    let b = stk!().pop().unwrap_or(Value::NIL);
                    let a = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![a, b];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "equal",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(
                            self.dispatch_vm_builtin_with_frame(func, "equal", call_args,)
                        )
                    };
                    stk_push!(result);
                }

                // -- String operations --
                Op::Concat(n) => {
                    let n = *n as usize;
                    let start = stk!().len().saturating_sub(n);
                    let parts: Vec<Value> = stk!().drain(start..).collect();
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "concat",
                        parts.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(
                            self.dispatch_vm_builtin_with_frame(func, "concat", parts,)
                        )
                    };
                    stk_push!(result);
                }
                Op::Substring => {
                    let to = stk!().pop().unwrap_or(Value::NIL);
                    let from = stk!().pop().unwrap_or(Value::fixnum(0));
                    let array = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![array, from, to];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "substring",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func,
                            "substring",
                            call_args,
                        ))
                    };
                    stk_push!(result);
                }
                Op::StringEqual => {
                    let b = stk!().pop().unwrap_or(Value::NIL);
                    let a = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![a, b];
                    let result =
                        if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                            "string=",
                            call_args.clone(),
                        )) {
                            result
                        } else {
                            vm_try!(self.dispatch_vm_builtin_with_frame(
                                func, "string=", call_args,
                            ))
                        };
                    stk_push!(result);
                }
                Op::StringLessp => {
                    let b = stk!().pop().unwrap_or(Value::NIL);
                    let a = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![a, b];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "string-lessp",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func,
                            "string-lessp",
                            call_args,
                        ))
                    };
                    stk_push!(result);
                }

                // -- Vector operations --
                Op::Aref => {
                    let idx_val = stk!().pop().unwrap_or(Value::fixnum(0));
                    let vec_val = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![vec_val, idx_val];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "aref",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(builtins::builtin_aref(call_args))
                    };
                    stk_push!(result);
                }
                Op::Aset => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let idx_val = stk!().pop().unwrap_or(Value::fixnum(0));
                    let vec_val = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![vec_val, idx_val, val];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "aset",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(builtins::builtin_aset(call_args.clone()))
                    };
                    self.maybe_writeback_mutating_first_arg("aset", None, &call_args, &result);
                    stk_push!(result);
                }

                // -- Symbol operations --
                Op::SymbolValue => {
                    let sym = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![sym];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "symbol-value",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func,
                            "symbol-value",
                            call_args,
                        ))
                    };
                    stk_push!(result);
                }
                Op::SymbolFunction => {
                    let sym = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![sym];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "symbol-function",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func,
                            "symbol-function",
                            call_args,
                        ))
                    };
                    stk_push!(result);
                }
                Op::Set => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let sym = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![sym, val];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "set",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(
                            self.dispatch_vm_builtin_with_frame(func, "set", call_args,)
                        )
                    };
                    stk_push!(result);
                }
                Op::Fset => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let sym = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![sym, val];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "fset",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(
                            self.dispatch_vm_builtin_with_frame(func, "fset", call_args,)
                        )
                    };
                    stk_push!(result);
                }
                Op::Get => {
                    let prop = stk!().pop().unwrap_or(Value::NIL);
                    let sym = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![sym, prop];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "get",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(
                            self.dispatch_vm_builtin_with_frame(func, "get", call_args,)
                        )
                    };
                    stk_push!(result);
                }
                Op::Put => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let prop = stk!().pop().unwrap_or(Value::NIL);
                    let sym = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![sym, prop, val];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        "put",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(
                            self.dispatch_vm_builtin_with_frame(func, "put", call_args,)
                        )
                    };
                    stk_push!(result);
                }

                // -- Error handling --
                Op::PushConditionCase(target) => {
                    let stack_len = stk!().len();
                    let spec_depth = self.ctx.specpdl.len();
                    let bsl = bind_stack.len();
                    let resume_id = self.ctx.allocate_resume_id();
                    handlers.push(Handler::Condition);
                    self.ctx
                        .push_condition_frame(ConditionFrame::ConditionCase {
                            conditions: Value::symbol("error"),
                            resume: ResumeTarget::VmConditionCase {
                                resume_id,
                                target: *target,
                                stack_len,
                                spec_depth,
                                bind_stack_len: bsl,
                            },
                        });
                }
                Op::PushConditionCaseRaw(target) => {
                    // GNU bytecode consumes the handler pattern operand from TOS.
                    let conditions = stk!().pop().unwrap_or(Value::NIL);
                    let stack_len = stk!().len();
                    let spec_depth = self.ctx.specpdl.len();
                    let bsl = bind_stack.len();
                    let resume_id = self.ctx.allocate_resume_id();
                    handlers.push(Handler::Condition);
                    self.ctx
                        .push_condition_frame(ConditionFrame::ConditionCase {
                            conditions,
                            resume: ResumeTarget::VmConditionCase {
                                resume_id,
                                target: *target,
                                stack_len,
                                spec_depth,
                                bind_stack_len: bsl,
                            },
                        });
                }
                Op::PushCatch(target) => {
                    let tag = stk!().pop().unwrap_or(Value::NIL);
                    let stack_len = stk!().len();
                    let spec_depth = self.ctx.specpdl.len();
                    let bsl = bind_stack.len();
                    let resume_id = self.ctx.allocate_resume_id();
                    handlers.push(Handler::Condition);
                    self.ctx.push_condition_frame(ConditionFrame::Catch {
                        tag,
                        resume: ResumeTarget::VmCatch {
                            resume_id,
                            target: *target,
                            stack_len,
                            spec_depth,
                            bind_stack_len: bsl,
                        },
                    });
                }
                Op::PopHandler => {
                    if handlers.pop().is_some() {
                        self.ctx.pop_condition_frame();
                    }
                }
                Op::UnwindProtectPop => {
                    let cleanup = stk!().pop().unwrap_or(Value::NIL);
                    bind_stack.push(self.ctx.specpdl.len());
                    self.ctx.specpdl.push(SpecBinding::UnwindProtect { forms: cleanup, lexenv: self.ctx.lexenv });
                }
                Op::Throw => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let tag = stk!().pop().unwrap_or(Value::NIL);
                    self.resume_nonlocal(
                        func,
                        pc,
                        handlers,
                        bind_stack,
                        Flow::Throw { tag, value: val },
                    )?;
                    continue;
                }

                // -- Closure --
                Op::MakeClosure(idx) => {
                    let val = constants[*idx as usize];
                    if let Some(bc_data) = val.get_bytecode_data() {
                        let mut closure = bc_data.clone();
                        closure.env = Some(self.ctx.lexenv);
                        stk_push!(Value::make_bytecode(closure));
                    } else {
                        stk_push!(val);
                    }
                }

                // -- Builtin escape hatch --
                Op::CallBuiltin(name_idx, n) => {
                    let name = sym_name(constants, *name_idx);
                    let n = *n as usize;
                    let args_start = stk!().len().saturating_sub(n);
                    let args: Vec<Value> = stk!().drain(args_start..).collect();
                    let writeback_args = args.clone();
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        &name,
                        args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(func, &name, args,))
                    };
                    self.maybe_writeback_mutating_first_arg(&name, None, &writeback_args, &result);
                    stk_push!(result);
                }
            }
        }

        // Fell off the end — return TOS or nil
        Ok(stk!().pop().unwrap_or(Value::NIL))
    }

    // -- Helper methods --

    fn writeback_callable_names(&self, func_val: &Value) -> Option<(String, Option<String>)> {
        match func_val.kind() {
            ValueKind::Veclike(VecLikeType::Subr) => {
                let id = func_val.as_subr_id().unwrap();
                Some((resolve_sym(id).to_owned(), None))
            }
            ValueKind::Symbol(id) => {
                let name = resolve_sym(id);
                let alias_target =
                    self.ctx
                        .obarray
                        .symbol_function(name)
                        .and_then(|bound| match bound.kind() {
                            ValueKind::Symbol(tid) => Some(resolve_sym(tid).to_owned()),
                            ValueKind::Veclike(VecLikeType::Subr) => {
                                let tid = bound.as_subr_id().unwrap();
                                Some(resolve_sym(tid).to_owned())
                            }
                            _ => None,
                        });
                Some((name.to_owned(), alias_target))
            }
            _ => None,
        }
    }

    fn named_builtin_fast_path_allowed(&self, name: &str) -> bool {
        if crate::emacs_core::eval::compiler_function_overrides_active_in_obarray(&self.ctx.obarray)
        {
            return false;
        }
        match self.ctx.obarray.symbol_function(name) {
            Some(val) => match val.kind() {
                ValueKind::Veclike(VecLikeType::Subr) => {
                    let id = val.as_subr_id().unwrap();
                    resolve_sym(id) == name
                }
                ValueKind::Nil => true,
                _ => false,
            },
            None => true,
        }
    }

    fn maybe_call_named_function_cell(
        &mut self,
        func: &ByteCodeFunction,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Option<Value>, Flow> {
        if self.named_builtin_fast_path_allowed(name) {
            return Ok(None);
        }

        let func_val = Value::symbol(name);
        self.with_frame_call_roots(func, func_val, args, |vm, args| {
            vm.call_function(func_val, args)
        })
        .map(Some)
    }

    fn maybe_writeback_mutating_first_arg(
        &mut self,
        called_name: &str,
        alias_target: Option<&str>,
        call_args: &[Value],
        result: &Value,
    ) {
        let mutates_fillarray =
            called_name == "fillarray" || alias_target.is_some_and(|name| name == "fillarray");
        let mutates_aset = called_name == "aset" || alias_target.is_some_and(|name| name == "aset");
        if !mutates_fillarray && !mutates_aset {
            return;
        }

        let Some(first_arg) = call_args.first() else {
            return;
        };
        if !first_arg.is_string() {
            return;
        }

        let replacement = if mutates_fillarray {
            if !result.is_string() || eq_value(first_arg, result) {
                return;
            }
            *result
        } else {
            if call_args.len() < 3 {
                return;
            }
            let Ok(updated) =
                builtins::aset_string_replacement(first_arg, &call_args[1], &call_args[2])
            else {
                return;
            };
            if eq_value(first_arg, &updated) {
                return;
            }
            updated
        };

        if first_arg.as_str() == replacement.as_str() {
            return;
        }

        let mut visited = HashSet::new();
        for value in self.ctx.bc_buf.iter_mut() {
            Self::replace_alias_refs_in_value(value, first_arg, &replacement, &mut visited);
        }
        // Walk the lexenv cons alist and replace alias refs in binding values
        {
            let mut lexenv_val = self.ctx.lexenv;
            Self::replace_alias_refs_in_value(
                &mut lexenv_val,
                first_arg,
                &replacement,
                &mut visited,
            );
            self.ctx.lexenv = lexenv_val;
        }
        // dynamic stack removed — specbind writes directly to obarray
        if let Some(current_id) = self.ctx.buffers.current_buffer_id()
            && let Some(buf) = self.ctx.buffers.get_mut(current_id)
        {
            for value in buf.bound_buffer_local_values_mut() {
                Self::replace_alias_refs_in_value(value, first_arg, &replacement, &mut visited);
            }
        }

        self.ctx.obarray.for_each_value_cell_mut(|value| {
            Self::replace_alias_refs_in_value(value, first_arg, &replacement, &mut visited);
        });
    }

    fn replace_alias_refs_in_value(
        value: &mut Value,
        from: &Value,
        to: &Value,
        visited: &mut HashSet<usize>,
    ) {
        if eq_value(value, from) {
            *value = *to;
            return;
        }

        match value.kind() {
            ValueKind::Cons => {
                let key = value.bits() ^ 0x1;
                if !visited.insert(key) {
                    return;
                }
                let mut new_car = value.cons_car();
                let mut new_cdr = value.cons_cdr();
                Self::replace_alias_refs_in_value(&mut new_car, from, to, visited);
                Self::replace_alias_refs_in_value(&mut new_cdr, from, to, visited);
                value.set_car(new_car);
                value.set_cdr(new_cdr);
            }
            ValueKind::Veclike(VecLikeType::Vector) => {
                let key = value.bits() ^ 0x2;
                if !visited.insert(key) {
                    return;
                }
                let mut data = value.as_vector_data().unwrap().clone();
                for item in data.iter_mut() {
                    Self::replace_alias_refs_in_value(item, from, to, visited);
                }
                let _ = value.replace_vector_data(data);
            }
            ValueKind::Veclike(VecLikeType::HashTable) => {
                let key = value.bits() ^ 0x4;
                if !visited.insert(key) {
                    return;
                }
                let old_ptr = match from.kind() {
                    ValueKind::String => Some(from.bits()),
                    _ => None,
                };
                let new_ptr = match to.kind() {
                    ValueKind::String => Some(to.bits()),
                    _ => None,
                };
                let _ = value.with_hash_table_mut(|ht| {
                    if matches!(ht.test, HashTableTest::Eq | HashTableTest::Eql) {
                        if let (Some(old_ptr), Some(new_ptr)) = (old_ptr, new_ptr) {
                            if let Some(existing) = ht.data.remove(&HashKey::Ptr(old_ptr)) {
                                ht.data.insert(HashKey::Ptr(new_ptr), existing);
                            }
                            if ht.key_snapshots.remove(&HashKey::Ptr(old_ptr)).is_some() {
                                ht.key_snapshots.insert(HashKey::Ptr(new_ptr), *to);
                            }
                            for k in &mut ht.insertion_order {
                                if *k == HashKey::Ptr(old_ptr) {
                                    *k = HashKey::Ptr(new_ptr);
                                }
                            }
                        }
                    }
                    for item in ht.data.values_mut() {
                        Self::replace_alias_refs_in_value(item, from, to, visited);
                    }
                });
            }
            _ => {}
        }
    }

    /// Variable reference by SymId — used by `Op::VarRef` to skip the
    /// `intern(name)` and `as_symbol_name() -> resolve_sym` round-trips
    /// that show up as the dominant cost in debug-build profiles.
    /// The bytecode constant is already a `Value::Symbol(SymId)`, so
    /// the SymId is available for free at the call site.
    fn lookup_var_id(&mut self, name_id: SymId) -> EvalResult {
        // Match GNU eval_sub: lexical environment lookup happens before
        // alias resolution fallback and does not rescan declared-special
        // flags.
        if let Some(val) = self.ctx.lexenv_lookup_cached_in(self.ctx.lexenv, name_id) {
            return Ok(val);
        }

        let resolved = crate::emacs_core::builtins::symbols::resolve_variable_alias_id_in_obarray(
            &self.ctx.obarray,
            name_id,
        )?;
        if resolved != name_id
            && let Some(val) = self.ctx.lexenv_lookup_cached_in(self.ctx.lexenv, resolved)
        {
            return Ok(val);
        }

        // Phase 9 of the symbol-redirect refactor: if the symbol's
        // redirect tag is LOCALIZED or FORWARDED, the new redirect
        // machinery is the source of truth. Route the read through
        // `find_symbol_value_in_buffer` which will swap the BLV
        // cache for LOCALIZED and read the slot for FORWARDED.
        //
        // For PLAINVAL / VARALIAS (and the transitional case where a
        // LOCALIZED read misses because the legacy `make-local-variable`
        // wrote only to BufferLocals), we fall through to the legacy
        // buffer-local detour and the PLAINVAL fast path.
        use crate::emacs_core::symbol::SymbolRedirect;
        let redirect = self.ctx.obarray.get_by_id(resolved).map(|s| s.redirect());
        if matches!(
            redirect,
            Some(SymbolRedirect::Localized | SymbolRedirect::Forwarded)
        ) {
            let (cur_val, alist, slots_ptr, buf_id, local_flags) =
                match self.ctx.buffers.current_buffer() {
                    Some(buf) => (
                        Value::make_buffer(buf.id),
                        buf.local_var_alist,
                        Some(&buf.slots[..] as *const [Value]),
                        Some(buf.id),
                        buf.local_flags,
                    ),
                    None => (Value::NIL, Value::NIL, None, None, 0u64),
                };
            let defaults_ptr: *const [Value] =
                &self.ctx.buffers.buffer_defaults[..] as *const [Value];
            // Safety: the slots and defaults pointers are valid for
            // the duration of this call because we hold `&mut self.ctx`,
            // the buffer and BufferManager live inside `self.ctx`, and
            // `find_symbol_value_in_buffer` does not mutate the
            // buffer manager. The raw pointer dance is only needed
            // because `find_symbol_value_in_buffer` also needs
            // `&mut self.ctx.obarray` for the BLV swap-in, and the
            // borrow checker can't express "hold slices of two
            // fields while mutating a third" across the method call.
            let slots_opt: Option<&[Value]> = slots_ptr.map(|p| unsafe { &*p });
            let defaults_opt: Option<&[Value]> = Some(unsafe { &*defaults_ptr });
            if let Some(val) = self.ctx.obarray.find_symbol_value_in_buffer(
                resolved,
                buf_id,
                cur_val,
                alist,
                slots_opt,
                local_flags,
                defaults_opt,
            ) {
                // `Qunbound` from the BLV cache / alist walk marks a
                // void LOCALIZED binding for this buffer — signal
                // `void-variable` instead of returning the sentinel
                // to the caller. Mirrors GNU `Fsymbol_value` which
                // signals when `find_symbol_value` returns
                // `Qunbound`.
                if val.is_unbound() {
                    return Err(signal("void-variable", vec![Value::from_sym_id(name_id)]));
                }
                return Ok(val);
            }
        }

        // Phase 2 fall-back: legacy buffer-local detour for symbols
        // still on the legacy storage path. Gated on
        // `is_buffer_local_id` so the String allocation + HashMap
        // lookup only fires for marked buffer-local variables.
        let is_local = self.ctx.obarray.is_buffer_local_id(resolved)
            || self.ctx.custom.is_auto_buffer_local_symbol(resolved);
        if is_local
            && crate::emacs_core::builtins::is_canonical_symbol_id(resolved)
            && let Some(buf) = self.ctx.buffers.current_buffer()
        {
            let resolved_name = resolve_sym(resolved);
            if let Some(binding) = buf.get_buffer_local_binding(resolved_name) {
                return binding
                    .as_value()
                    .or_else(|| {
                        (resolved_name == "buffer-undo-list")
                            .then(|| buf.buffer_local_value(resolved_name))
                            .flatten()
                    })
                    .ok_or_else(|| signal("void-variable", vec![Value::from_sym_id(name_id)]));
            }
        }

        // Obarray top-level value via the new redirect dispatch.
        if let Some(val) = self.ctx.obarray.find_symbol_value(resolved) {
            return Ok(val);
        }

        Err(signal("void-variable", vec![Value::from_sym_id(name_id)]))
    }

    /// Variable assignment by SymId — counterpart to `lookup_var_id`.
    fn assign_var_id(&mut self, name_id: SymId, value: Value) -> Result<(), Flow> {
        let resolved = crate::emacs_core::builtins::symbols::resolve_variable_alias_id_in_obarray(
            &self.ctx.obarray,
            name_id,
        )?;
        if let Some(cell_id) = self.ctx.lexenv_assq_cached_in(self.ctx.lexenv, name_id) {
            lexenv_set(cell_id, value);
            return Ok(());
        }
        if resolved != name_id
            && let Some(cell_id) = self.ctx.lexenv_assq_cached_in(self.ctx.lexenv, resolved)
        {
            lexenv_set(cell_id, value);
            return Ok(());
        }

        if self.ctx.obarray.is_constant_id(resolved) {
            return Err(signal(
                "setting-constant",
                vec![Value::from_sym_id(name_id)],
            ));
        }

        // Phase 9b of the symbol-redirect refactor: for LOCALIZED
        // symbols, route the write through
        // Obarray::set_internal_localized which updates the BLV
        // cache and (for auto-create `Set` writes with
        // `local_if_set`) extends the current buffer's
        // local_var_alist. The legacy set_runtime_binding_in_state
        // path below stays populated as a fallback until Phase 10
        // deletes it.
        use crate::emacs_core::symbol::{SetInternalBind, SymbolRedirect};
        let redirect = self.ctx.obarray.get_by_id(resolved).map(|s| s.redirect());
        // Phase 10B: FORWARDED writes go to the buffer slot the
        // descriptor points at. Mirrors GNU
        // `store_symval_forwarding` for the BUFFER_OBJFWD arm
        // (`data.c:1374-1471`).
        //
        // Phase 10D: for conditional slots (`local_flags_idx >= 0`),
        // also set the per-buffer local-flags bit so subsequent reads
        // route to `slots[off]` rather than `buffer_defaults`. This
        // mirrors GNU `set_internal` SYMBOL_FORWARDED arm at
        // `data.c:1774-1786` which calls `SET_PER_BUFFER_VALUE_P`.
        if matches!(redirect, Some(SymbolRedirect::Forwarded)) {
            if let Some(buf_id) = self.ctx.buffers.current_buffer_id() {
                use crate::emacs_core::forward::{LispBufferObjFwd, LispFwdType};
                let fwd_ptr = self
                    .ctx
                    .obarray
                    .get_by_id(resolved)
                    .map(|s| unsafe { s.val.fwd });
                if let Some(fwd) = fwd_ptr {
                    // Safety: install_buffer_objfwd leaks a 'static
                    // descriptor and the symbol's redirect tag is
                    // immutable once installed.
                    let header = unsafe { &*fwd };
                    if matches!(header.ty, LispFwdType::BufferObj) {
                        let buf_fwd = unsafe { &*(fwd as *const LispBufferObjFwd) };
                        let offset = buf_fwd.offset as usize;
                        let flags_idx = buf_fwd.local_flags_idx;
                        if let Some(buf) = self.ctx.buffers.get_mut(buf_id)
                            && offset < buf.slots.len()
                        {
                            buf.slots[offset] = value;
                            if flags_idx >= 0 {
                                buf.set_slot_local_flag(offset, true);
                            }
                            return self.run_variable_watchers_by_id(
                                resolved,
                                &value,
                                &Value::NIL,
                                "set",
                            );
                        }
                    }
                }
            }
        }

        if matches!(redirect, Some(SymbolRedirect::Localized)) {
            if let Some(buf_id) = self.ctx.buffers.current_buffer_id() {
                // Extract buffer state before obarray borrow.
                let (cur_val, alist) = match self.ctx.buffers.get(buf_id) {
                    Some(buf) => (Value::make_buffer(buf.id), buf.local_var_alist),
                    None => (Value::NIL, Value::NIL),
                };
                // GNU `eval.c:3559-3577 (let_shadows_buffer_binding_p)`
                // walks the specpdl looking for SPECPDL_LET_LOCAL or
                // SPECPDL_LET_DEFAULT records bound to this symbol.
                // The Context-side version on `eval::Context` already
                // handles this; route through it instead of the
                // free-function stub. Buffer-local audit Medium 4 in
                // `drafts/buffer-local-variables-audit.md`.
                let let_shadows = self.ctx.let_shadows_buffer_binding_p(resolved);
                let new_alist = self.ctx.obarray.set_internal_localized(
                    resolved,
                    value,
                    cur_val,
                    alist,
                    SetInternalBind::Set,
                    let_shadows,
                );
                // Store back the (possibly extended) alist.
                if let Some(buf) = self.ctx.buffers.get_mut(buf_id) {
                    buf.local_var_alist = new_alist;
                }
            }
        }

        // Legacy path: set_runtime_binding_in_state routes to
        // either BufferLocals or the obarray value cell. Phase 10
        // deletes this call once every LOCALIZED symbol is
        // exclusively served by the new BLV path above.
        crate::emacs_core::eval::set_runtime_binding_in_state(&mut *self.ctx, resolved, value);
        self.run_variable_watchers_by_id(resolved, &value, &Value::NIL, "set")
    }

    fn lookup_var(&mut self, name: &str) -> EvalResult {
        if name.starts_with(':') {
            return Ok(Value::keyword(name));
        }

        let name_id = intern(name);
        // Match GNU eval_sub: lexical environment lookup happens before
        // alias resolution fallback.
        if let Some(val) = self.ctx.lexenv_lookup_cached_in(self.ctx.lexenv, name_id) {
            return Ok(val);
        }
        let resolved = crate::emacs_core::builtins::symbols::resolve_variable_alias_id_in_obarray(
            &self.ctx.obarray,
            name_id,
        )?;
        let resolved_name = resolve_sym(resolved);
        if resolved != name_id
            && let Some(val) = self.ctx.lexenv_lookup_cached_in(self.ctx.lexenv, resolved)
        {
            return Ok(val);
        }

        // specbind writes directly to obarray, so dynamic stack lookup is
        // no longer needed — fall through to buffer-local and obarray lookups.

        // Phase 2: only consult buffer-local storage if the symbol is
        // actually marked as buffer-local.
        let is_local = self.ctx.obarray.is_buffer_local_id(resolved)
            || self.ctx.custom.is_auto_buffer_local_symbol(resolved);
        if is_local
            && crate::emacs_core::builtins::is_canonical_symbol_id(resolved)
            && let Some(buf) = self.ctx.buffers.current_buffer()
        {
            if let Some(binding) = buf.get_buffer_local_binding(resolved_name) {
                return binding
                    .as_value()
                    .or_else(|| {
                        (resolved_name == "buffer-undo-list")
                            .then(|| buf.buffer_local_value(resolved_name))
                            .flatten()
                    })
                    .ok_or_else(|| signal("void-variable", vec![Value::symbol(name)]));
            }
        }

        // Obarray top-level via the new redirect dispatch.
        if let Some(val) = self.ctx.obarray.find_symbol_value(resolved) {
            return Ok(val);
        }

        if name == "nil" {
            return Ok(Value::NIL);
        }
        if name == "t" {
            return Ok(Value::T);
        }
        if resolved_name == "nil" {
            return Ok(Value::NIL);
        }
        if resolved_name == "t" {
            return Ok(Value::T);
        }
        if resolved_name.starts_with(':') {
            return Ok(Value::keyword(resolved_name));
        }

        Err(signal("void-variable", vec![Value::symbol(name)]))
    }

    fn assign_var(&mut self, name: &str, value: Value) -> Result<(), Flow> {
        let name_id = intern(name);
        let resolved = crate::emacs_core::builtins::symbols::resolve_variable_alias_id_in_obarray(
            &self.ctx.obarray,
            name_id,
        )?;
        if let Some(cell_id) = self.ctx.lexenv_assq_cached_in(self.ctx.lexenv, name_id) {
            lexenv_set(cell_id, value);
            return Ok(());
        }
        if resolved != name_id
            && let Some(cell_id) = self.ctx.lexenv_assq_cached_in(self.ctx.lexenv, resolved)
        {
            lexenv_set(cell_id, value);
            return Ok(());
        }

        // specbind writes directly to obarray, so dynamic stack mutation
        // is no longer needed — fall through to obarray write.

        if self.ctx.obarray.is_constant_id(resolved) {
            return Err(signal("setting-constant", vec![Value::symbol(name)]));
        }

        crate::emacs_core::eval::set_runtime_binding_in_state(&mut *self.ctx, resolved, value);
        self.run_variable_watchers_by_id(resolved, &value, &Value::NIL, "set")
    }

    fn run_variable_watchers_by_id(
        &mut self,
        sym_id: SymId,
        new_value: &Value,
        old_value: &Value,
        operation: &str,
    ) -> Result<(), Flow> {
        self.run_variable_watchers_by_id_with_where(
            sym_id,
            new_value,
            old_value,
            operation,
            &Value::NIL,
        )
    }

    fn run_variable_watchers_by_id_with_where(
        &mut self,
        sym_id: SymId,
        new_value: &Value,
        old_value: &Value,
        operation: &str,
        where_value: &Value,
    ) -> Result<(), Flow> {
        if !self.ctx.watchers.has_watchers(sym_id) {
            return Ok(());
        }
        let calls =
            self.ctx
                .watchers
                .notify_watchers(sym_id, new_value, old_value, operation, where_value);
        for (callback, args) in calls {
            let _ = self.call_function_with_roots(callback, &args)?;
        }
        Ok(())
    }

    fn run_variable_watchers(
        &mut self,
        name: &str,
        new_value: &Value,
        old_value: &Value,
        operation: &str,
    ) -> Result<(), Flow> {
        self.run_variable_watchers_by_id(intern(name), new_value, old_value, operation)
    }

    fn run_variable_watchers_with_where(
        &mut self,
        name: &str,
        new_value: &Value,
        old_value: &Value,
        operation: &str,
        where_value: &Value,
    ) -> Result<(), Flow> {
        self.run_variable_watchers_by_id_with_where(
            intern(name),
            new_value,
            old_value,
            operation,
            where_value,
        )
    }

    fn call_function_with_roots(&mut self, function: Value, args: &[Value]) -> EvalResult {
        self.call_function(function, args.to_vec())
    }

    fn builtin_run_hooks_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::hook_runtime::run_named_hooks(self, args)
    }

    fn builtin_run_hook_with_args_shared(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_min_args("run-hook-with-args", args, 1)?;
        crate::emacs_core::hook_runtime::run_named_hook_with_args(self, args)
    }

    fn builtin_run_hook_with_args_until_success_shared(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_min_args("run-hook-with-args-until-success", args, 1)?;
        crate::emacs_core::hook_runtime::run_named_hook_with_args_until_success(self, args)
    }

    fn builtin_run_hook_with_args_until_failure_shared(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_min_args("run-hook-with-args-until-failure", args, 1)?;
        crate::emacs_core::hook_runtime::run_named_hook_with_args_until_failure(self, args)
    }

    fn builtin_run_hook_wrapped_shared(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_min_args("run-hook-wrapped", args, 2)?;
        crate::emacs_core::hook_runtime::run_named_hook_wrapped(self, args)
    }

    fn builtin_run_hook_query_error_with_timeout_shared(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("run-hook-query-error-with-timeout", args, 1)?;
        let hook_sym = crate::emacs_core::hook_runtime::resolve_hook_symbol(&self.ctx, args[0])?;
        let hook_value = crate::emacs_core::hook_runtime::hook_value_by_id(&self.ctx, hook_sym)
            .unwrap_or(Value::NIL);
        crate::emacs_core::hook_runtime::run_hook_query_error_with_timeout(
            self, hook_sym, hook_value,
        )
    }

    fn builtin_set_shared(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("set", args, 2)?;
        let symbol = crate::emacs_core::builtins::symbols::expect_symbol_id(&args[0])?;
        let resolved = crate::emacs_core::builtins::symbols::resolve_variable_alias_id_in_obarray(
            &self.ctx.obarray,
            symbol,
        )?;
        let value = args[1];
        if let Some(result) = crate::emacs_core::builtins::symbols::constant_set_outcome_in_obarray(
            &self.ctx.obarray,
            resolved,
            args[0],
            value,
        ) {
            return result;
        }
        let where_value =
            crate::emacs_core::eval::set_runtime_binding_in_state(&mut *self.ctx, resolved, value)
                .map(Value::make_buffer)
                .unwrap_or(Value::NIL);
        self.run_variable_watchers_by_id_with_where(
            resolved,
            &value,
            &Value::NIL,
            "set",
            &where_value,
        )?;
        Ok(value)
    }

    fn builtin_set_default_shared(&mut self, args: &[Value]) -> EvalResult {
        use crate::emacs_core::builtins::symbols::resolve_variable_alias_id_in_obarray;

        if args.len() != 2 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![
                    Value::symbol("set-default"),
                    Value::fixnum(args.len() as i64),
                ],
            ));
        }
        let symbol = match args[0].kind() {
            ValueKind::Nil => intern("nil"),
            ValueKind::T => intern("t"),
            ValueKind::Symbol(id) => id,
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("symbolp"), args[0]],
                ));
            }
        };
        let resolved = resolve_variable_alias_id_in_obarray(&self.ctx.obarray, symbol)?;
        let resolved_name = resolve_sym(resolved);
        if self.ctx.obarray.is_constant_id(resolved) {
            return Err(signal("setting-constant", vec![args[0]]));
        }
        let value = args[1];

        // GNU PLAINVAL path: for non-buffer-local variables, `set-default`
        // behaves like `set` -- writes to dynamic frame if let-bound.
        let is_buffer_local = self.ctx.obarray.is_buffer_local(resolved_name)
            || self.ctx.custom.is_auto_buffer_local_symbol(resolved);
        if !is_buffer_local {
            crate::emacs_core::eval::set_runtime_binding_in_state(&mut *self.ctx, resolved, value);
        } else {
            self.ctx.obarray.set_symbol_value_id(resolved, value);
        }

        // Fire watchers AFTER the write.
        self.run_variable_watchers_by_id(resolved, &value, &Value::NIL, "set")?;
        Ok(value)
    }

    fn builtin_set_default_toplevel_value_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::symbols::set_default_toplevel_value_impl(
            &mut *self.ctx,
            args.to_vec(),
        )?;
        let symbol = crate::emacs_core::builtins::symbols::expect_symbol_id(&args[0])?;
        let resolved = crate::emacs_core::builtins::symbols::resolve_variable_alias_id_in_obarray(
            &self.ctx.obarray,
            symbol,
        )?;
        let value = args[1];
        self.run_variable_watchers_by_id(resolved, &value, &Value::NIL, "set")?;
        if resolved != symbol {
            self.run_variable_watchers_by_id(resolved, &value, &Value::NIL, "set")?;
        }
        Ok(Value::NIL)
    }

    fn builtin_defalias_shared(&mut self, args: &[Value]) -> EvalResult {
        let plan = crate::emacs_core::builtins::plan_defalias_in_obarray(&self.ctx.obarray, args)?;
        let crate::emacs_core::builtins::DefaliasPlan {
            action,
            docstring,
            result,
        } = plan;
        match action {
            crate::emacs_core::builtins::DefaliasAction::SetFunction { symbol, definition } => {
                self.ctx.obarray.set_symbol_function_id(symbol, definition);
            }
            crate::emacs_core::builtins::DefaliasAction::CallHook {
                hook,
                symbol_value,
                definition,
            } => {
                let _ = self.call_function_with_roots(hook, &[symbol_value, definition])?;
            }
        }
        if let Some(docstring) = docstring {
            crate::emacs_core::builtins::symbols::builtin_put(
                &mut *self.ctx,
                vec![result, Value::symbol("function-documentation"), docstring],
            )?;
        }
        Ok(result)
    }

    fn builtin_defvaralias_shared(&mut self, args: &[Value]) -> EvalResult {
        let state_change =
            crate::emacs_core::builtins::symbols::defvaralias_impl(&mut *self.ctx, args.to_vec())?;
        self.run_variable_watchers_by_id(
            state_change.previous_target_id,
            &state_change.base_variable,
            &Value::NIL,
            "defvaralias",
        )?;
        self.ctx.watchers.clear_watchers(state_change.alias_id);
        crate::emacs_core::builtins::symbols::builtin_put(
            &mut *self.ctx,
            vec![
                args[0],
                Value::symbol("variable-documentation"),
                state_change.docstring,
            ],
        )?;
        Ok(state_change.result)
    }

    fn builtin_makunbound_shared(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("makunbound", args, 1)?;
        let symbol = crate::emacs_core::builtins::symbols::expect_symbol_id(&args[0])?;
        let resolved = crate::emacs_core::builtins::symbols::resolve_variable_alias_id_in_obarray(
            &self.ctx.obarray,
            symbol,
        )?;
        if self.ctx.obarray.is_constant_id(resolved) {
            return Err(signal("setting-constant", vec![args[0]]));
        }
        crate::emacs_core::eval::makunbound_runtime_binding_in_state(
            &mut self.ctx.obarray,
            &mut self.ctx.buffers,
            &self.ctx.custom,
            &[],
            resolved,
        );
        self.run_variable_watchers_by_id(resolved, &Value::NIL, &Value::NIL, "makunbound")?;
        Ok(args[0])
    }

    fn builtin_make_local_variable_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::custom::builtin_make_local_variable(&mut *self.ctx, args.to_vec())
    }

    fn builtin_local_variable_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::custom::builtin_local_variable_p(&mut *self.ctx, args.to_vec())
    }

    fn builtin_buffer_local_variables_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::custom::builtin_buffer_local_variables(&mut *self.ctx, args.to_vec())
    }

    fn builtin_kill_local_variable_shared(&mut self, args: &[Value]) -> EvalResult {
        let outcome =
            crate::emacs_core::custom::builtin_kill_local_variable_impl(&mut *self.ctx, args)?;
        if outcome.removed
            && let Some(buffer_id) = outcome.buffer_id
        {
            self.run_variable_watchers_by_id_with_where(
                outcome.resolved_id,
                &Value::NIL,
                &Value::NIL,
                "makunbound",
                &Value::make_buffer(buffer_id),
            )?;
        }
        Ok(outcome.result)
    }

    fn ensure_selected_frame_id(&mut self) -> FrameId {
        crate::emacs_core::window_cmds::ensure_selected_frame_id_in_state(
            &mut self.ctx.frames,
            &mut self.ctx.buffers,
        )
    }

    fn resolve_frame_id(&mut self, arg: Option<&Value>, predicate: &str) -> Result<FrameId, Flow> {
        let Some(val) = arg else {
            return Ok(self.ensure_selected_frame_id());
        };
        match val.kind() {
            ValueKind::Nil => Ok(self.ensure_selected_frame_id()),
            ValueKind::Fixnum(n) => {
                let fid = FrameId(n as u64);
                if self.ctx.frames.get(fid).is_some() {
                    Ok(fid)
                } else {
                    Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol(predicate), Value::fixnum(n)],
                    ))
                }
            }
            ValueKind::Veclike(VecLikeType::Frame) => {
                let id = val.as_frame_id().unwrap();
                let fid = FrameId(id);
                if self.ctx.frames.get(fid).is_some() {
                    Ok(fid)
                } else {
                    Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol(predicate), *val],
                    ))
                }
            }
            _ => Err(signal(
                "wrong-type-argument",
                vec![Value::symbol(predicate), *val],
            )),
        }
    }

    fn ensure_global_keymap(&mut self) -> Value {
        if let Some(value) = self.ctx.obarray.symbol_value("global-map").copied() {
            if crate::emacs_core::keymap::is_list_keymap(&value) {
                return value;
            }
        }
        let keymap = crate::emacs_core::keymap::make_list_keymap();
        self.ctx.obarray.set_symbol_value("global-map", keymap);
        keymap
    }

    fn builtin_mapcar_fast(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("mapcar", args, 2)?;
        let func = args[0];
        let sequence = args[1];
        self.with_vm_root_scope(|vm| {
            vm.push_dynamic_vm_root(func);
            vm.push_dynamic_vm_root(sequence);
            let mut results = Vec::new();
            let map_result = crate::emacs_core::builtins::higher_order::for_each_sequence_element(
                &sequence,
                |item| {
                    let value = vm.with_vm_root_scope(|vm| {
                        vm.push_dynamic_vm_root(item);
                        vm.call_function(func, vec![item])
                    })?;
                    vm.push_dynamic_vm_root(value);
                    results.push(value);
                    Ok(())
                },
            );

            match map_result {
                Ok(()) => Ok(Value::list(results)),
                Err(flow) => Err(flow),
            }
        })
    }

    fn builtin_mapc_fast(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("mapc", args, 2)?;
        let func = args[0];
        let sequence = args[1];
        self.with_vm_root_scope(|vm| {
            vm.push_dynamic_vm_root(func);
            vm.push_dynamic_vm_root(sequence);
            crate::emacs_core::builtins::higher_order::for_each_sequence_element(
                &sequence,
                |item| {
                    let result = vm.with_vm_root_scope(|vm| {
                        vm.push_dynamic_vm_root(item);
                        vm.call_function(func, vec![item])
                    });
                    result?;
                    Ok(())
                },
            )?;
            Ok(sequence)
        })
    }

    fn builtin_mapcan_fast(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("mapcan", args, 2)?;
        let func = args[0];
        let sequence = args[1];
        self.with_vm_root_scope(|vm| {
            vm.push_dynamic_vm_root(func);
            vm.push_dynamic_vm_root(sequence);
            let mut mapped = Vec::new();
            let map_result = crate::emacs_core::builtins::higher_order::for_each_sequence_element(
                &sequence,
                |item| {
                    let value = vm.with_vm_root_scope(|vm| {
                        vm.push_dynamic_vm_root(item);
                        vm.call_function(func, vec![item])
                    })?;
                    vm.push_dynamic_vm_root(value);
                    mapped.push(value);
                    Ok(())
                },
            );

            match map_result {
                Ok(()) => crate::emacs_core::builtins::builtin_nconc(mapped),
                Err(flow) => Err(flow),
            }
        })
    }

    fn builtin_mapconcat_fast(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_range_args("mapconcat", args, 2, 3)?;
        let func = args[0];
        let sequence = args[1];
        let separator = args.get(2).copied().unwrap_or_else(|| Value::string(""));
        self.with_vm_root_scope(|vm| {
            vm.push_dynamic_vm_root(func);
            vm.push_dynamic_vm_root(sequence);
            vm.push_dynamic_vm_root(separator);
            let mut parts = Vec::new();
            let map_result = crate::emacs_core::builtins::higher_order::for_each_sequence_element(
                &sequence,
                |item| {
                    let value = vm.with_vm_root_scope(|vm| {
                        vm.push_dynamic_vm_root(item);
                        vm.call_function(func, vec![item])
                    })?;
                    vm.push_dynamic_vm_root(value);
                    parts.push(value);
                    Ok(())
                },
            );

            match map_result {
                Ok(()) if parts.is_empty() => Ok(Value::string("")),
                Ok(()) => {
                    let mut concat_args = Vec::with_capacity(parts.len() * 2 - 1);
                    for (index, part) in parts.iter().copied().enumerate() {
                        if index > 0 {
                            concat_args.push(separator);
                        }
                        concat_args.push(part);
                    }
                    crate::emacs_core::builtins::builtin_concat(concat_args)
                }
                Err(flow) => Err(flow),
            }
        })
    }

    fn builtin_sort_fast(&mut self, args: &[Value]) -> EvalResult {
        let crate::emacs_core::builtins::higher_order::SortOptions {
            key_fn,
            lessp_fn,
            reverse,
            in_place,
        } = crate::emacs_core::builtins::higher_order::parse_sort_options(args)?;
        let sequence = args[0];
        self.with_vm_root_scope(|vm| {
            vm.push_dynamic_vm_root(sequence);
            vm.push_dynamic_vm_root(key_fn);
            vm.push_dynamic_vm_root(lessp_fn);
            match sequence.kind() {
                ValueKind::Nil => Ok(Value::NIL),
                ValueKind::Cons => {
                    let mut cons_cells = Vec::new();
                    let mut values = Vec::new();
                    let mut cursor = sequence;
                    loop {
                        match cursor.kind() {
                            ValueKind::Nil => break,
                            ValueKind::Cons => {
                                let value = cursor.cons_car();
                                vm.push_dynamic_vm_root(value);
                                values.push(value);
                                cons_cells.push(cursor);
                                cursor = cursor.cons_cdr();
                            }
                            _tail => {
                                return Err(signal(
                                    "wrong-type-argument",
                                    vec![Value::symbol("listp"), cursor],
                                ));
                            }
                        }
                    }
                    let mut sorted_values =
                        crate::emacs_core::builtins::higher_order::stable_sort_values_with(
                            vm, &values, key_fn, lessp_fn, reverse,
                        )?;
                    if in_place {
                        for (cell, value) in cons_cells.iter().zip(sorted_values.into_iter()) {
                            cell.set_car(value);
                        }
                        Ok(sequence)
                    } else {
                        Ok(Value::list(std::mem::take(&mut sorted_values)))
                    }
                }
                ValueKind::Veclike(VecLikeType::Vector)
                | ValueKind::Veclike(VecLikeType::Record) => {
                    let is_record =
                        matches!(sequence.kind(), ValueKind::Veclike(VecLikeType::Record));
                    let values = if is_record {
                        sequence.as_record_data().unwrap().clone()
                    } else {
                        sequence.as_vector_data().unwrap().clone()
                    };
                    for value in values.iter().copied() {
                        vm.push_dynamic_vm_root(value);
                    }
                    let sorted_values =
                        crate::emacs_core::builtins::higher_order::stable_sort_values_with(
                            vm, &values, key_fn, lessp_fn, reverse,
                        )?;

                    if in_place {
                        if is_record {
                            let _ = sequence.replace_record_data(sorted_values);
                        } else {
                            let _ = sequence.replace_vector_data(sorted_values);
                        }
                        Ok(sequence)
                    } else if is_record {
                        Ok(Value::make_record(sorted_values))
                    } else {
                        Ok(Value::vector(sorted_values))
                    }
                }
                _other => Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("list-or-vector-p"), sequence],
                )),
            }
        })
    }

    fn builtin_frame_list_fast(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("frame-list", args, 0)?;
        let _ = self.ensure_selected_frame_id();
        let frames = self
            .ctx
            .frames
            .frame_list()
            .into_iter()
            .map(|frame_id| Value::make_frame(frame_id.0))
            .collect();
        Ok(Value::list(frames))
    }

    fn builtin_framep_fast(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("framep", args, 1)?;
        let id = match args[0].kind() {
            ValueKind::Veclike(VecLikeType::Frame) => args[0].as_frame_id().unwrap(),
            ValueKind::Fixnum(n) => n as u64,
            _ => return Ok(Value::NIL),
        };
        let Some(frame) = self.ctx.frames.get(FrameId(id)) else {
            return Ok(Value::NIL);
        };
        Ok(frame.parameter("window-system").unwrap_or(Value::T))
    }

    fn builtin_frame_parameter_fast(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("frame-parameter", args, 2)?;
        let fid = self.resolve_frame_id(args.first(), "framep")?;
        let param_name = match args[1].kind() {
            ValueKind::Symbol(id) => resolve_sym(id).to_owned(),
            _ => return Ok(Value::NIL),
        };
        let frame = self
            .ctx
            .frames
            .get(fid)
            .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
        match param_name.as_str() {
            "name" => Ok(frame.name_value()),
            "icon-name" => Ok(frame.icon_name_value()),
            "title" => Ok(frame.title_value()),
            "explicit-name" => Ok(frame.explicit_name_value()),
            "width" => Ok(frame
                .parameter("width")
                .unwrap_or(Value::fixnum(frame.columns() as i64))),
            "height" => Ok(frame
                .parameter("height")
                .unwrap_or(Value::fixnum(frame.lines() as i64))),
            "visibility" => Ok(if frame.visible { Value::T } else { Value::NIL }),
            _ => Ok(frame.parameter(&param_name).unwrap_or(Value::NIL)),
        }
    }

    fn builtin_fboundp_fast(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::symbols::builtin_fboundp(&mut *self.ctx, args.to_vec())
    }

    fn builtin_current_indentation_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::indent::builtin_current_indentation(&mut *self.ctx, args.to_vec())
    }

    fn builtin_indent_to_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::indent::builtin_indent_to(&mut *self.ctx, args.to_vec())
    }

    fn builtin_current_column_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::indent::builtin_current_column(&mut *self.ctx, args.to_vec())
    }

    fn builtin_buffer_string_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_string(&mut *self.ctx, args.to_vec())
    }

    fn builtin_buffer_substring_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_substring(&mut *self.ctx, args.to_vec())
    }

    fn builtin_field_beginning_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_field_beginning(&mut *self.ctx, args.to_vec())
    }

    fn builtin_field_end_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_field_end(&mut *self.ctx, args.to_vec())
    }

    fn builtin_field_string_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_field_string(&mut *self.ctx, args.to_vec())
    }

    fn builtin_field_string_no_properties_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_field_string_no_properties(
            &mut *self.ctx,
            args.to_vec(),
        )
    }

    fn builtin_constrain_to_field_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_constrain_to_field(&mut *self.ctx, args.to_vec())
    }

    fn builtin_point_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_point(&mut *self.ctx, args.to_vec())
    }

    fn builtin_accept_process_output_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::process::builtin_accept_process_output(&mut *self.ctx, args.to_vec())
    }

    fn builtin_buffer_list_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_list(&mut *self.ctx, args.to_vec())
    }

    fn builtin_other_buffer_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_other_buffer(&mut *self.ctx, args.to_vec())
    }

    fn builtin_generate_new_buffer_name_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_generate_new_buffer_name(&mut *self.ctx, args.to_vec())
    }

    fn builtin_get_file_buffer_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_get_file_buffer(&mut *self.ctx, args.to_vec())
    }

    fn builtin_make_indirect_buffer_shared(&mut self, args: &[Value]) -> EvalResult {
        let plan = crate::emacs_core::builtins::prepare_make_indirect_buffer_in_manager(
            &mut self.ctx.buffers,
            args.to_vec(),
        )?;
        if plan.run_clone_hook {
            self.ctx.switch_current_buffer(plan.id)?;
            let hook_sym = crate::emacs_core::hook_runtime::hook_symbol_by_name(
                &self.ctx,
                "clone-indirect-buffer-hook",
            );
            let clone_result = crate::emacs_core::hook_runtime::run_named_hook(self, hook_sym, &[]);
            if let Some(saved_id) = plan.saved_current
                && self.ctx.buffers.get(saved_id).is_some()
            {
                self.ctx.restore_current_buffer_if_live(saved_id);
            }
            clone_result?;
        }
        if !self.ctx.buffers.buffer_hooks_inhibited(plan.id) {
            let hook_sym = crate::emacs_core::hook_runtime::hook_symbol_by_name(
                &self.ctx,
                "buffer-list-update-hook",
            );
            let _ = crate::emacs_core::hook_runtime::run_named_hook(self, hook_sym, &[])?;
        }
        Ok(Value::make_buffer(plan.id))
    }

    fn builtin_kill_buffer_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_kill_buffer(&mut *self.ctx, args.to_vec())
    }

    fn builtin_current_active_maps_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::keymaps::builtin_current_active_maps_impl(&mut *self.ctx, args)
    }

    fn builtin_current_minor_mode_maps_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::keymaps::builtin_current_minor_mode_maps_impl(&*self.ctx, args)
    }

    fn builtin_map_keymap_shared(&mut self, args: &[Value], include_parents: bool) -> EvalResult {
        let (function, mut keymap) = if include_parents {
            builtins::expect_min_args("map-keymap", args, 2)?;
            builtins::expect_max_args("map-keymap", args, 3)?;
            (
                args[0],
                crate::emacs_core::keymap::get_keymap_in_runtime(
                    &mut *self.ctx,
                    &args[1],
                    true,
                    true,
                )?,
            )
        } else {
            builtins::expect_args("map-keymap-internal", args, 2)?;
            (
                args[0],
                crate::emacs_core::keymap::get_keymap_in_runtime(
                    &mut *self.ctx,
                    &args[1],
                    true,
                    true,
                )?,
            )
        };

        loop {
            let plan = crate::emacs_core::builtins::keymaps::plan_keymap_iteration(keymap);
            let parent = plan.parent;
            let bindings = plan.bindings;
            for (event, binding) in &bindings {
                let call_args = [*event, *binding];
                let _ = self.call_function_with_roots(function, &call_args)?;
            }

            if !include_parents {
                return Ok(parent);
            }
            if parent.is_nil() || !crate::emacs_core::keymap::is_list_keymap(&parent) {
                return Ok(Value::NIL);
            }
            keymap = parent;
        }
    }

    fn builtin_map_char_table_shared(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("map-char-table", args, 2)?;
        let function = args[0];
        crate::emacs_core::chartable::for_each_char_table_mapping(&args[1], |key, value| {
            let call_args = [key, value];
            let _ = self.call_function_with_roots(function, &call_args)?;
            Ok(())
        })?;
        Ok(Value::NIL)
    }

    fn builtin_call_last_kbd_macro_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::kmacro::builtin_call_last_kbd_macro(&mut *self.ctx, args.to_vec())
    }

    fn builtin_execute_kbd_macro_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::kmacro::builtin_execute_kbd_macro(&mut *self.ctx, args.to_vec())
    }

    fn builtin_command_remapping_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::interactive::builtin_command_remapping_impl(&*self.ctx, args.to_vec())
    }

    fn builtin_key_binding_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::interactive::builtin_key_binding_impl(&mut *self.ctx, args.to_vec())
    }

    fn builtin_local_key_binding_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::interactive::builtin_local_key_binding_impl(&*self.ctx, args.to_vec())
    }

    fn builtin_minor_mode_key_binding_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::interactive::builtin_minor_mode_key_binding_impl(
            &*self.ctx,
            args.to_vec(),
        )
    }

    fn builtin_set_buffer_multibyte_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_set_buffer_multibyte(&mut *self.ctx, args.to_vec())
    }

    fn builtin_insert_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_insert(&mut *self.ctx, args.to_vec())
    }

    fn builtin_barf_if_buffer_read_only_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_barf_if_buffer_read_only_impl(
            &*self.ctx,
            args.to_vec(),
        )
    }

    fn builtin_insert_and_inherit_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_insert_and_inherit(&mut *self.ctx, args.to_vec())
    }

    fn builtin_insert_before_markers_and_inherit_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_insert_before_markers_and_inherit(
            &mut *self.ctx,
            args.to_vec(),
        )
    }

    fn builtin_point_min_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_point_min(&mut *self.ctx, args.to_vec())
    }

    fn builtin_point_max_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_point_max(&mut *self.ctx, args.to_vec())
    }

    fn builtin_goto_char_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_goto_char(&mut *self.ctx, args.to_vec())
    }

    fn builtin_char_after_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_char_after(&mut *self.ctx, args.to_vec())
    }

    fn builtin_char_before_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_char_before(&mut *self.ctx, args.to_vec())
    }

    fn builtin_buffer_size_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_size(&mut *self.ctx, args.to_vec())
    }

    fn builtin_byte_to_position_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_byte_to_position(&mut *self.ctx, args.to_vec())
    }

    fn builtin_position_bytes_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_position_bytes(&mut *self.ctx, args.to_vec())
    }

    fn builtin_get_byte_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_get_byte(&mut *self.ctx, args.to_vec())
    }

    fn builtin_narrow_to_region_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_narrow_to_region(&mut *self.ctx, args.to_vec())
    }

    fn builtin_widen_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_widen(&mut *self.ctx, args.to_vec())
    }

    fn builtin_buffer_modified_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_modified_p(&mut *self.ctx, args.to_vec())
    }

    fn builtin_set_buffer_modified_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_set_buffer_modified_p(&mut *self.ctx, args.to_vec())
    }

    fn builtin_buffer_modified_tick_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_modified_tick(&mut *self.ctx, args.to_vec())
    }

    fn builtin_buffer_chars_modified_tick_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_chars_modified_tick(
            &mut *self.ctx,
            args.to_vec(),
        )
    }

    fn builtin_insert_char_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_insert_char(&mut *self.ctx, args.to_vec())
    }

    fn builtin_insert_byte_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_insert_byte(&mut *self.ctx, args.to_vec())
    }

    fn builtin_subst_char_in_region_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_subst_char_in_region(&mut *self.ctx, args.to_vec())
    }

    fn builtin_bobp_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_bobp(&mut *self.ctx, args.to_vec())
    }

    fn builtin_eobp_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_eobp(&mut *self.ctx, args.to_vec())
    }

    fn builtin_bolp_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_bolp(&mut *self.ctx, args.to_vec())
    }

    fn builtin_eolp_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_eolp(&mut *self.ctx, args.to_vec())
    }

    fn builtin_line_beginning_position_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_line_beginning_position(
            &mut *self.ctx,
            args.to_vec(),
        )
    }

    fn builtin_line_end_position_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_line_end_position(&mut *self.ctx, args.to_vec())
    }

    fn builtin_insert_before_markers_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_insert_before_markers(&mut *self.ctx, args.to_vec())
    }

    fn builtin_insert_buffer_substring_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_insert_buffer_substring(&mut *self.ctx, args.to_vec())
    }

    fn builtin_replace_region_contents_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_replace_region_contents(&mut *self.ctx, args.to_vec())
    }

    fn builtin_delete_char_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_delete_char(&mut *self.ctx, args.to_vec())
    }

    fn builtin_buffer_substring_no_properties_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_buffer_substring_no_properties(
            &*self.ctx,
            args.to_vec(),
        )
    }

    fn builtin_following_char_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_following_char(&*self.ctx, args.to_vec())
    }

    fn builtin_preceding_char_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_preceding_char(&*self.ctx, args.to_vec())
    }

    fn builtin_delete_region_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_delete_region(&mut *self.ctx, args.to_vec())
    }

    fn builtin_compare_buffer_substrings_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_compare_buffer_substrings_with_case_fold(
            self.case_fold_search_enabled(),
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_delete_field_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_delete_field(&mut *self.ctx, args.to_vec())
    }

    fn builtin_delete_and_extract_region_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_delete_and_extract_region(&mut *self.ctx, args.to_vec())
    }

    fn builtin_erase_buffer_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_erase_buffer(&mut *self.ctx, args.to_vec())
    }

    fn builtin_undo_boundary_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::undo::builtin_undo_boundary(&mut *self.ctx, args.to_vec())
    }

    fn builtin_buffer_enable_undo_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_enable_undo(&mut *self.ctx, args.to_vec())
    }

    fn builtin_buffer_disable_undo_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_disable_undo(&mut *self.ctx, args.to_vec())
    }

    fn builtin_kill_all_local_variables_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_kill_all_local_variables(&mut *self.ctx, args.to_vec())
    }

    fn builtin_buffer_local_value_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_local_value(&mut *self.ctx, args.to_vec())
    }

    fn builtin_local_variable_if_set_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::symbols::builtin_local_variable_if_set_p(
            &mut *self.ctx,
            args.to_vec(),
        )
    }

    fn builtin_variable_binding_locus_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::symbols::builtin_variable_binding_locus(
            &mut *self.ctx,
            args.to_vec(),
        )
    }

    fn builtin_move_to_column_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::indent::builtin_move_to_column(&mut *self.ctx, args.to_vec())
    }

    fn case_fold_search_enabled(&mut self) -> bool {
        self.lookup_var("case-fold-search")
            .map(|value| !value.is_nil())
            .unwrap_or(true)
    }

    fn builtin_search_forward_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_search_forward_with_state(
            self.case_fold_search_enabled(),
            &mut self.ctx.buffers,
            &mut self.ctx.match_data,
            args,
        )
    }

    fn builtin_search_backward_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_search_backward_with_state(
            self.case_fold_search_enabled(),
            &mut self.ctx.buffers,
            &mut self.ctx.match_data,
            args,
        )
    }

    fn builtin_re_search_forward_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_re_search_forward_with_state(
            self.case_fold_search_enabled(),
            &mut self.ctx.buffers,
            &mut self.ctx.match_data,
            args,
        )
    }

    fn builtin_re_search_backward_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_re_search_backward_with_state(
            self.case_fold_search_enabled(),
            &mut self.ctx.buffers,
            &mut self.ctx.match_data,
            args,
        )
    }

    fn builtin_search_forward_regexp_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_search_forward_regexp_with_state(
            self.case_fold_search_enabled(),
            &mut self.ctx.buffers,
            &mut self.ctx.match_data,
            args,
        )
    }

    fn builtin_search_backward_regexp_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_search_backward_regexp_with_state(
            self.case_fold_search_enabled(),
            &mut self.ctx.buffers,
            &mut self.ctx.match_data,
            args,
        )
    }

    fn builtin_looking_at_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_looking_at_with_state(
            self.case_fold_search_enabled(),
            &self.ctx.buffers,
            &mut self.ctx.match_data,
            args,
        )
    }

    fn builtin_looking_at_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_looking_at_p_with_state(
            self.case_fold_search_enabled(),
            &self.ctx.buffers,
            args,
        )
    }

    fn builtin_posix_looking_at_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_posix_looking_at_with_state(
            self.case_fold_search_enabled(),
            &self.ctx.buffers,
            &mut self.ctx.match_data,
            args,
        )
    }

    fn builtin_posix_string_match_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_posix_string_match_with_state(
            self.case_fold_search_enabled(),
            &mut self.ctx.match_data,
            args,
        )
    }

    fn builtin_match_data_translate_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_match_data_translate_with_state(
            &mut self.ctx.match_data,
            args,
        )
    }

    fn builtin_replace_match_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_replace_match_with_state(
            &mut self.ctx.buffers,
            &mut self.ctx.match_data,
            args,
        )
    }

    fn builtin_find_charset_region_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::charset::builtin_find_charset_region(&mut *self.ctx, args.to_vec())
    }

    fn builtin_charset_after_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::charset::builtin_charset_after(&mut *self.ctx, args.to_vec())
    }

    fn builtin_compose_region_internal_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::composite::builtin_compose_region_internal(&mut *self.ctx, args.to_vec())
    }

    fn builtin_interactive_form_shared(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("interactive-form", args, 1)?;
        let mut target = args[0];
        loop {
            match crate::emacs_core::builtins::symbols::plan_interactive_form_in_state(
                &self.ctx.obarray,
                &self.ctx.interactive,
                target,
            )? {
                crate::emacs_core::builtins::symbols::InteractiveFormPlan::Return(value) => {
                    return Ok(value);
                }
                crate::emacs_core::builtins::symbols::InteractiveFormPlan::Autoload {
                    fundef,
                    funname,
                } => {
                    let mut load_args = vec![fundef];
                    if !funname.is_nil() {
                        load_args.push(funname);
                    }
                    target = self.with_vm_root_scope(|vm| {
                        vm.push_dynamic_vm_root(target);
                        for value in args.iter().copied() {
                            vm.push_dynamic_vm_root(value);
                        }
                        for value in load_args.iter().copied() {
                            vm.push_dynamic_vm_root(value);
                        }
                        crate::emacs_core::autoload::builtin_autoload_do_load_in_vm_runtime(
                            &mut vm.ctx,
                            &load_args,
                        )
                    })?;
                }
            }
        }
    }

    fn builtin_skip_chars_forward_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_skip_chars_forward(&mut *self.ctx, args.to_vec())
    }

    fn builtin_skip_chars_backward_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_skip_chars_backward(&mut *self.ctx, args.to_vec())
    }

    fn builtin_scan_lists_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::syntax::builtin_scan_lists(&mut *self.ctx, args.to_vec())
    }

    fn builtin_scan_sexps_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::syntax::builtin_scan_sexps(&mut *self.ctx, args.to_vec())
    }

    fn visible_variable_value_or_nil(&self, name: &str) -> Value {
        let name_id = intern(name);
        if let Some(value) = self.ctx.lexenv_lookup_cached_in(self.ctx.lexenv, name_id) {
            return value;
        }
        // specbind writes directly to obarray, so no dynamic stack lookup needed.
        if let Some(buffer) = self.ctx.buffers.current_buffer()
            && let Some(binding) = buffer.get_buffer_local_binding(name)
        {
            return binding.as_value().unwrap_or(Value::NIL);
        }
        if let Some(value) = self.ctx.obarray.symbol_value(name).copied() {
            return value;
        }
        if name == "nil" {
            return Value::NIL;
        }
        if name == "t" {
            return Value::T;
        }
        Value::NIL
    }

    fn call_function(&mut self, func_val: Value, args: Vec<Value>) -> EvalResult {
        let bt_count = self.ctx.specpdl.len();
        self.ctx.push_backtrace_frame(func_val, &args);
        let result = match func_val.kind() {
            // Fast path: stay in VM for bytecoded calls.
            // Matches GNU Emacs's CLOSUREP → goto setup_frame in bytecode.c.
            ValueKind::Veclike(VecLikeType::ByteCode) => {
                let bc_data = func_val.get_bytecode_data().unwrap().clone();
                self.execute_with_func_value(&bc_data, args, func_val)
            }
            // Everything else: shared dispatch via funcall_general on Context.
            // Matches GNU Emacs where exec_byte_code delegates to funcall_general.
            _ => self.ctx.funcall_general_untraced(func_val, args),
        };
        self.ctx.unbind_to(bt_count);
        result
    }

    /// Execute a compiled function without param binding (for inline compilation).
    fn execute_inline(&mut self, func: &ByteCodeFunction) -> EvalResult {
        let condition_stack_base = self.ctx.condition_stack_len();
        let frame_base = self.ctx.bc_buf.len();
        self.ctx.bc_frames.push(crate::emacs_core::eval::BcFrame {
            base: frame_base,
            fun: Value::NIL,
        });
        let mut pc: usize = 0;
        let mut handlers: Vec<Handler> = Vec::new();
        let specpdl_base = self.ctx.specpdl.len();
        let mut bind_stack: Vec<usize> = Vec::new();
        let result = self.run_loop(func, frame_base, &mut pc, &mut handlers, &mut bind_stack);
        self.ctx.truncate_condition_stack(condition_stack_base);
        self.ctx.unbind_to(specpdl_base);
        self.ctx.bc_buf.truncate(frame_base);
        self.ctx.bc_frames.pop();
        result
    }

    fn resume_nonlocal(
        &mut self,
        _func: &ByteCodeFunction,
        pc: &mut usize,
        handlers: &mut Vec<Handler>,
        bind_stack: &mut Vec<usize>,
        flow: Flow,
    ) -> Result<(), Flow> {
        match flow {
            Flow::Throw { tag, value } => {
                let selected_resume = self.ctx.matching_catch_resume(&tag);
                if let Some(ResumeTarget::VmCatch {
                    target,
                    stack_len,
                    spec_depth,
                    bind_stack_len,
                    ..
                }) = unwind_handlers_to_selected_resume(
                    handlers,
                    &mut self.ctx.condition_stack,
                    selected_resume.as_ref(),
                ) {
                    self.ctx.unbind_to(spec_depth);
                    bind_stack.truncate(bind_stack_len);
                    self.ctx.bc_buf.truncate(stack_len);
                    self.ctx.bc_buf.push(value);
                    *pc = target as usize;
                    return Ok(());
                }

                if selected_resume.is_some() {
                    return Err(Flow::Throw { tag, value });
                }
                Err(signal("no-catch", vec![tag, value]))
            }
            Flow::Signal(sig) => {
                if sig.symbol == intern("kill-emacs") {
                    return Err(Flow::Signal(sig));
                }
                // dispatch_signal_if_needed may call signal hooks and
                // handler-bind handlers via eval.apply(), which can trigger
                // GC.  We must root the current frame so values survive
                // collection.
                let mut sig_extra = Vec::new();
                Self::collect_flow_roots(&Flow::Signal(sig.clone()), &mut sig_extra);
                let sig = match self.with_frame_roots(_func, &sig_extra, |vm| {
                    vm.ctx.dispatch_signal_if_needed(sig)
                }) {
                    Ok(sig) => sig,
                    Err(flow) => {
                        return self.resume_nonlocal(_func, pc, handlers, bind_stack, flow);
                    }
                };
                if let Some(ResumeTarget::VmConditionCase {
                    target,
                    stack_len,
                    spec_depth,
                    bind_stack_len,
                    ..
                }) = unwind_handlers_to_selected_resume(
                    handlers,
                    &mut self.ctx.condition_stack,
                    sig.selected_resume.as_ref(),
                ) {
                    self.ctx.unbind_to(spec_depth);
                    bind_stack.truncate(bind_stack_len);
                    self.ctx.bc_buf.truncate(stack_len);
                    self.ctx.bc_buf.push(make_signal_binding_value(&sig));
                    *pc = target as usize;
                    return Ok(());
                }
                Err(Flow::Signal(sig))
            }
        }
    }

    fn dispatch_vm_builtin_with_frame(
        &mut self,
        func: &ByteCodeFunction,
        name: &str,
        args: Vec<Value>,
    ) -> EvalResult {
        self.with_frame_arg_roots(func, args, |vm, args| {
            vm.dispatch_vm_builtin_unrooted(name, args)
        })
    }

    fn dispatch_vm_builtin(&mut self, name: &str, args: Vec<Value>) -> EvalResult {
        self.dispatch_vm_builtin_unrooted(name, args)
    }

    /// Dispatch to builtin functions from the VM.
    fn dispatch_vm_builtin_unrooted(&mut self, name: &str, args: Vec<Value>) -> EvalResult {
        // VM-internal bytecode operations that are not real Elisp builtins.
        match name {
            "call-interactively" => return self.builtin_call_interactively_shared(&args),
            "start-kbd-macro" => {
                return crate::emacs_core::kmacro::builtin_start_kbd_macro(&mut *self.ctx, args);
            }
            "end-kbd-macro" => {
                return crate::emacs_core::kmacro::builtin_end_kbd_macro(&mut *self.ctx, args);
            }
            "call-last-kbd-macro" => return self.builtin_call_last_kbd_macro_shared(&args),
            "execute-kbd-macro" => return self.builtin_execute_kbd_macro_shared(&args),
            "garbage-collect" => return self.builtin_garbage_collect_shared(&args),
            "mapatoms" => return self.builtin_mapatoms_shared(&args),
            "maphash" => return self.builtin_maphash_shared(&args),
            "store-kbd-macro-event" => {
                return crate::emacs_core::kmacro::builtin_store_kbd_macro_event(
                    &mut *self.ctx,
                    args,
                );
            }
            "cancel-kbd-macro-events" => {
                return crate::emacs_core::builtins::builtin_cancel_kbd_macro_events(
                    &mut *self.ctx,
                    args,
                );
            }
            "%%defvar" => {
                if args.len() >= 2 {
                    let sym_name = args[1].as_symbol_name().unwrap_or("nil").to_string();
                    if !self.ctx.obarray.boundp(&sym_name) {
                        self.ctx.obarray.set_symbol_value(&sym_name, args[0]);
                    }
                    self.ctx.obarray.make_special(&sym_name);
                    return Ok(Value::symbol(sym_name));
                }
                return Ok(Value::NIL);
            }
            "%%defconst" => {
                if args.len() >= 2 {
                    let sym_name = args[1].as_symbol_name().unwrap_or("nil").to_string();
                    self.ctx.obarray.set_symbol_value(&sym_name, args[0]);
                    self.ctx.obarray.set_constant(&sym_name);
                    self.ctx.obarray.make_special(&sym_name);
                    return Ok(Value::symbol(sym_name));
                }
                return Ok(Value::NIL);
            }
            "%%unimplemented-elc-bytecode" => {
                return Err(signal(
                    "error",
                    vec![Value::string(
                        "Compiled .elc bytecode execution is not implemented yet",
                    )],
                ));
            }
            _ => {}
        }

        // All real builtins go through funcall_general → dispatch_subr.
        // This matches GNU Emacs where the bytecode VM delegates to
        // funcall_general for everything except bytecoded closures.
        self.ctx.funcall_general(Value::subr(intern(name)), args)
    }

    fn with_default_directory_binding<T>(
        &mut self,
        directory: &crate::heap_types::LispString,
        f: impl FnOnce(&mut Self) -> Result<T, Flow>,
    ) -> Result<T, Flow> {
        let specpdl_count = self.ctx.specpdl.len();
        crate::emacs_core::eval::specbind_in_state(
            &mut self.ctx.obarray,
            &mut self.ctx.specpdl,
            intern("default-directory"),
            Value::heap_string(directory.clone()),
        );
        let result = f(self);
        crate::emacs_core::eval::unbind_to_in_state(
            &mut self.ctx.obarray,
            &mut self.ctx.specpdl,
            specpdl_count,
        );
        result
    }

    fn builtin_documentation_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::doc::builtin_documentation_in_vm_runtime(&mut self.ctx, args.to_vec())
    }

    fn builtin_documentation_property_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::doc::builtin_documentation_property_in_vm_runtime(
            &mut self.ctx,
            args.to_vec(),
        )
    }

    fn builtin_format_mode_line_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::xdisp::builtin_format_mode_line_in_vm_runtime(&mut self.ctx, args)
    }

    fn builtin_read_from_minibuffer_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::reader::finish_read_from_minibuffer_in_vm_runtime(&mut self.ctx, args)
    }

    fn builtin_call_interactively_shared(&mut self, args: &[Value]) -> EvalResult {
        let mut plan = crate::emacs_core::interactive::plan_call_interactively_in_state(
            &self.ctx.obarray,
            &self.ctx.interactive,
            self.ctx.read_command_keys(),
            args,
        )?;
        if crate::emacs_core::interactive::callable_form_needs_instantiation(&plan.func) {
            plan.func = self.ctx.instantiate_callable_cons_form(plan.func)?;
        }
        self.with_vm_root_scope(|vm| {
            for value in args.iter().copied() {
                vm.push_dynamic_vm_root(value);
            }
            vm.push_dynamic_vm_root(plan.func);
            let (function, call_args) =
                crate::emacs_core::interactive::resolve_call_interactively_target_and_args_with_vm_fallback(
                    &mut vm.ctx,
                    &mut plan,
                )?;
            let mut funcall_args = Vec::with_capacity(call_args.len() + 1);
            funcall_args.push(function);
            funcall_args.extend(call_args);
            vm.call_function_with_roots(Value::symbol("funcall-interactively"), &funcall_args)
        })
    }

    fn builtin_assoc_shared(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_range_args("assoc", args, 2, 3)?;
        if args.get(2).is_some_and(|value| !value.is_nil()) {
            let key = args[0];
            let list = args[1];
            let test_fn = args[2];
            return self.with_vm_root_scope(|vm| {
                vm.push_dynamic_vm_root(key);
                vm.push_dynamic_vm_root(list);
                vm.push_dynamic_vm_root(test_fn);
                let mut cursor = list;
                loop {
                    match cursor.kind() {
                        ValueKind::Nil => return Ok(Value::NIL),
                        ValueKind::Cons => {
                            let pair_car = cursor.cons_car();
                            let pair_cdr = cursor.cons_cdr();
                            if let ValueKind::Cons = pair_car.kind() {
                                let entry_key = pair_car.cons_car();
                                let matches = vm.with_vm_root_scope(|vm| {
                                    vm.push_dynamic_vm_root(cursor);
                                    vm.push_dynamic_vm_root(pair_car);
                                    vm.push_dynamic_vm_root(pair_cdr);
                                    vm.push_dynamic_vm_root(entry_key);
                                    vm.call_function(test_fn, vec![entry_key, key])
                                        .map(|value| value.is_truthy())
                                });
                                let matches = matches?;
                                if matches {
                                    return Ok(pair_car);
                                }
                            }
                            cursor = pair_cdr;
                        }
                        _ => {
                            return Err(signal(
                                "wrong-type-argument",
                                vec![Value::symbol("listp"), list],
                            ));
                        }
                    }
                }
            });
        }
        crate::emacs_core::builtins::builtin_assoc(&mut *self.ctx, vec![args[0], args[1]])
    }

    fn builtin_plist_member_shared(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_range_args("plist-member", args, 2, 3)?;
        if args.get(2).is_some_and(|value| !value.is_nil()) {
            let plist = args[0];
            let prop = args[1];
            let predicate = args[2];
            return self.with_vm_root_scope(|vm| {
                vm.push_dynamic_vm_root(plist);
                vm.push_dynamic_vm_root(prop);
                vm.push_dynamic_vm_root(predicate);
                let mut cursor = plist;
                loop {
                    match cursor.kind() {
                        ValueKind::Cons => {
                            let pair_car = cursor.cons_car();
                            let pair_cdr = cursor.cons_cdr();
                            let entry_key = pair_car;
                            let matches = vm.with_vm_root_scope(|vm| {
                                vm.push_dynamic_vm_root(cursor);
                                vm.push_dynamic_vm_root(entry_key);
                                vm.push_dynamic_vm_root(pair_cdr);
                                vm.call_function(predicate, vec![entry_key, prop])
                                    .map(|value| value.is_truthy())
                            });
                            let matches = matches?;
                            if matches {
                                return Ok(cursor);
                            }

                            // Match GNU's `plist_member` nil-
                            // terminator rule: an unpaired last key is
                            // a valid end (return nil, not-found);
                            // only dotted tails signal plistp.
                            match pair_cdr.kind() {
                                ValueKind::Cons => {
                                    cursor = pair_cdr.cons_cdr();
                                }
                                ValueKind::Nil => {
                                    return Ok(Value::NIL);
                                }
                                _ => {
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
            });
        }
        crate::emacs_core::builtins::plist_member_eq(args.to_vec())
    }

    fn builtin_garbage_collect_shared(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("garbage-collect", args, 0)?;
        self.ctx.gc_collect_exact();
        crate::emacs_core::builtins_extra::builtin_garbage_collect_stats()
    }

    fn builtin_kill_emacs_shared(&mut self, args: &[Value]) -> EvalResult {
        let request = crate::emacs_core::builtins::symbols::plan_kill_emacs_request(args)?;
        self.builtin_run_hooks_shared(&[Value::symbol("kill-emacs-hook")])?;
        self.ctx
            .request_shutdown(request.exit_code, request.restart);
        Err(signal_suppressed("kill-emacs", vec![]))
    }

    fn builtin_macroexpand_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::symbols::builtin_macroexpand_with_runtime(self, args.to_vec())
    }

    fn builtin_mapatoms_shared(&mut self, args: &[Value]) -> EvalResult {
        let (func, symbols) =
            crate::emacs_core::hashtab::collect_mapatoms_symbols(&self.ctx.obarray, args.to_vec())?;
        self.with_dynamic_vm_roots(|vm| {
            vm.push_dynamic_vm_root(func);
            for sym in symbols.iter().copied() {
                vm.push_dynamic_vm_root(sym);
            }
            for sym in symbols {
                vm.call_function(func, vec![sym])?;
            }
            Ok(Value::NIL)
        })
    }

    fn builtin_maphash_shared(&mut self, args: &[Value]) -> EvalResult {
        let (func, entries) = crate::emacs_core::hashtab::collect_maphash_entries(args.to_vec())?;
        self.with_dynamic_vm_roots(|vm| {
            vm.push_dynamic_vm_root(func);
            for (key, value) in &entries {
                vm.push_dynamic_vm_root(*key);
                vm.push_dynamic_vm_root(*value);
            }
            for (key, value) in entries {
                vm.call_function(func, vec![key, value])?;
            }
            Ok(Value::NIL)
        })
    }

    fn builtin_read_string_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::reader::finish_read_string_in_vm_runtime(&mut self.ctx, args)
    }

    fn builtin_completing_read_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::reader::finish_completing_read_in_vm_runtime(&mut self.ctx, args)
    }

    fn builtin_read_buffer_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::minibuffer::builtin_read_buffer_in_runtime(self.ctx, args)?;
        let completing_args =
            crate::emacs_core::minibuffer::read_buffer_completing_args(&self.ctx.buffers, args);
        self.builtin_completing_read_shared(&completing_args)
    }

    fn builtin_try_completion_shared(&mut self, args: &[Value]) -> EvalResult {
        let candidates =
            crate::emacs_core::minibuffer::completion_candidates_from_collection_in_state(
                &*self.ctx, &args[1],
            )?;
        let ignore_case = self
            .ctx
            .obarray
            .symbol_value("completion-ignore-case")
            .is_some_and(|v| v.is_truthy());
        let regexps = crate::emacs_core::minibuffer::completion_regexp_lisp_list_from_obarray(
            &self.ctx.obarray,
        );
        crate::emacs_core::minibuffer::builtin_try_completion_with_candidates(
            args,
            candidates,
            ignore_case,
            &regexps,
            |function, call_args| self.call_function_with_roots(function, &call_args),
        )
    }

    fn builtin_all_completions_shared(&mut self, args: &[Value]) -> EvalResult {
        let candidates =
            crate::emacs_core::minibuffer::completion_candidates_from_collection_in_state(
                &*self.ctx, &args[1],
            )?;
        let ignore_case = self
            .ctx
            .obarray
            .symbol_value("completion-ignore-case")
            .is_some_and(|v| v.is_truthy());
        let regexps = crate::emacs_core::minibuffer::completion_regexp_lisp_list_from_obarray(
            &self.ctx.obarray,
        );
        crate::emacs_core::minibuffer::builtin_all_completions_with_candidates(
            args,
            candidates,
            ignore_case,
            &regexps,
            |function, call_args| self.call_function_with_roots(function, &call_args),
        )
    }

    fn builtin_file_name_completion_shared(&mut self, args: &[Value]) -> EvalResult {
        let needs_eval_predicate = matches!(
            args.get(2),
            Some(predicate)
                if !predicate.is_nil()
                    && !(predicate.is_symbol() || predicate.as_subr_id().is_some())
        );
        if needs_eval_predicate {
            let plan = crate::emacs_core::dired::prepare_file_name_completion_in_state(
                &self.ctx.obarray,
                &[],
                &self.ctx.buffers,
                args,
            )?;
            let predicate = args[2];
            let use_absolute_path = crate::emacs_core::dired::predicate_uses_absolute_file_argument(
                &self.ctx.obarray,
                &predicate,
            );
            let bound_directory = plan.directory.clone();
            return crate::emacs_core::dired::finish_file_name_completion_with_callable_predicate(
                use_absolute_path,
                plan.directory,
                plan.file,
                plan.completions,
                plan.ignore_case,
                |predicate_arg| {
                    self.with_default_directory_binding(&bound_directory, |vm| {
                        vm.call_function_with_roots(predicate, &[predicate_arg])
                    })
                },
            );
        }
        crate::emacs_core::dired::builtin_file_name_completion(&mut *self.ctx, args.to_vec())
    }

    fn builtin_read_command_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::minibuffer::finish_read_command_in_vm_runtime(&mut self.ctx, args)
    }

    fn builtin_read_variable_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::minibuffer::finish_read_variable_in_vm_runtime(&mut self.ctx, args)
    }

    fn builtin_test_completion_shared(&mut self, args: &[Value]) -> EvalResult {
        let candidates =
            crate::emacs_core::minibuffer::completion_candidates_from_collection_in_state(
                &*self.ctx, &args[1],
            )?;
        let ignore_case = self
            .ctx
            .obarray
            .symbol_value("completion-ignore-case")
            .is_some_and(|v| v.is_truthy());
        let regexps = crate::emacs_core::minibuffer::completion_regexp_lisp_list_from_obarray(
            &self.ctx.obarray,
        );
        crate::emacs_core::minibuffer::builtin_test_completion_with_candidates(
            args,
            candidates,
            ignore_case,
            &regexps,
            |function, call_args| self.call_function_with_roots(function, &call_args),
        )
    }

    fn builtin_input_pending_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::reader::builtin_input_pending_p(&mut *self.ctx, args.to_vec())
    }

    fn builtin_discard_input_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::reader::builtin_discard_input(&mut *self.ctx, args.to_vec())
    }

    fn builtin_current_input_mode_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::reader::builtin_current_input_mode(&mut *self.ctx, args.to_vec())
    }

    fn builtin_set_input_mode_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::reader::builtin_set_input_mode(&mut *self.ctx, args.to_vec())
    }

    fn builtin_set_input_interrupt_mode_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::reader::builtin_set_input_interrupt_mode(&mut *self.ctx, args.to_vec())
    }

    fn builtin_read_char_shared(&mut self, args: &[Value]) -> EvalResult {
        if let Some(value) =
            crate::emacs_core::reader::builtin_read_char_in_runtime(self.ctx, args)?
        {
            return Ok(value);
        }
        crate::emacs_core::reader::finish_read_char_interactive_in_runtime(self.ctx, args)
    }

    fn builtin_read_from_string_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::reader::builtin_read_from_string(&mut *self.ctx, args.to_vec())
    }

    fn builtin_read_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::reader::builtin_read(&mut *self.ctx, args.to_vec())
    }

    fn builtin_read_event_shared(&mut self, args: &[Value]) -> EvalResult {
        if let Some(value) =
            crate::emacs_core::lread::builtin_read_event_in_runtime(self.ctx, args)?
        {
            return Ok(value);
        }
        crate::emacs_core::lread::finish_read_event_interactive_in_runtime(self.ctx, args)
    }

    fn builtin_read_char_exclusive_shared(&mut self, args: &[Value]) -> EvalResult {
        if let Some(value) =
            crate::emacs_core::lread::builtin_read_char_exclusive_in_runtime(self.ctx, args)?
        {
            return Ok(value);
        }
        crate::emacs_core::lread::finish_read_char_exclusive_interactive_in_runtime(self.ctx, args)
    }

    fn builtin_read_key_sequence_shared(&mut self, args: &[Value]) -> EvalResult {
        if let Some(value) =
            crate::emacs_core::reader::builtin_read_key_sequence_in_runtime(self.ctx, args)?
        {
            return Ok(value);
        }
        crate::emacs_core::reader::finish_read_key_sequence_interactive_in_runtime(
            self.ctx,
            crate::emacs_core::reader::read_key_sequence_options_from_args(args),
        )
    }

    fn builtin_read_key_sequence_vector_shared(&mut self, args: &[Value]) -> EvalResult {
        if let Some(value) =
            crate::emacs_core::reader::builtin_read_key_sequence_vector_in_runtime(self.ctx, args)?
        {
            return Ok(value);
        }
        crate::emacs_core::reader::finish_read_key_sequence_vector_interactive_in_runtime(
            self.ctx,
            crate::emacs_core::reader::read_key_sequence_options_from_args(args),
        )
    }

    fn builtin_recent_keys_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::keymaps::builtin_recent_keys_impl(&*self.ctx, args.to_vec())
    }

    fn builtin_current_message_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_current_message(&mut *self.ctx, args.to_vec())
    }

    fn builtin_current_case_table_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::casetab::builtin_current_case_table(&mut *self.ctx, args.to_vec())
    }

    fn builtin_standard_case_table_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::casetab::builtin_standard_case_table(&mut *self.ctx, args.to_vec())
    }

    fn builtin_set_case_table_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::casetab::builtin_set_case_table(&mut *self.ctx, args.to_vec())
    }

    fn builtin_set_standard_case_table_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::casetab::builtin_set_standard_case_table(&mut *self.ctx, args.to_vec())
    }

    fn builtin_format_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_format_wrapper_strict(&mut *self.ctx, args.to_vec())
    }

    fn builtin_format_message_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_format_message(&mut *self.ctx, args.to_vec())
    }

    fn builtin_message_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_message(&mut *self.ctx, args.to_vec())
    }

    fn builtin_message_box_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_message_box(&mut *self.ctx, args.to_vec())
    }

    fn builtin_message_or_box_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_message_or_box(&mut *self.ctx, args.to_vec())
    }

    fn builtin_make_thread_shared(&mut self, args: &[Value]) -> EvalResult {
        let (thread_id, function) =
            crate::emacs_core::threads::prepare_make_thread(&mut self.ctx.threads, args)?;
        self.ctx
            .threads
            .set_thread_current_buffer(thread_id, self.ctx.buffers.current_buffer_id());
        let runtime_state =
            crate::emacs_core::threads::enter_thread_runtime(&mut *self.ctx, thread_id)?;
        let result = self.call_function_with_roots(function, &[]);
        crate::emacs_core::threads::exit_thread_runtime(&mut *self.ctx, thread_id, runtime_state);
        crate::emacs_core::threads::finish_make_thread_result(
            &mut self.ctx.threads,
            thread_id,
            result,
        )
    }

    fn builtin_thread_join_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_thread_join(&mut *self.ctx, args.to_vec())
    }

    fn builtin_thread_yield_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_thread_yield(&mut *self.ctx, args.to_vec())
    }

    fn builtin_thread_name_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_thread_name(&mut *self.ctx, args.to_vec())
    }

    fn builtin_thread_live_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_thread_live_p(&mut *self.ctx, args.to_vec())
    }

    fn builtin_threadp_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_threadp(&mut *self.ctx, args.to_vec())
    }

    fn builtin_thread_signal_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_thread_signal(&mut *self.ctx, args.to_vec())
    }

    fn builtin_current_thread_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_current_thread(&mut *self.ctx, args.to_vec())
    }

    fn builtin_all_threads_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_all_threads(&mut *self.ctx, args.to_vec())
    }

    fn builtin_thread_last_error_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_thread_last_error(&mut *self.ctx, args.to_vec())
    }

    fn builtin_make_mutex_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_make_mutex(&mut *self.ctx, args.to_vec())
    }

    fn builtin_mutex_name_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_mutex_name(&mut *self.ctx, args.to_vec())
    }

    fn builtin_mutex_lock_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_mutex_lock(&mut *self.ctx, args.to_vec())
    }

    fn builtin_mutex_unlock_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_mutex_unlock(&mut *self.ctx, args.to_vec())
    }

    fn builtin_mutexp_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_mutexp(&mut *self.ctx, args.to_vec())
    }

    fn builtin_make_condition_variable_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_make_condition_variable(&mut *self.ctx, args.to_vec())
    }

    fn builtin_condition_variable_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_condition_variable_p(&mut *self.ctx, args.to_vec())
    }

    fn builtin_condition_name_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_condition_name(&mut *self.ctx, args.to_vec())
    }

    fn builtin_condition_mutex_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_condition_mutex(&mut *self.ctx, args.to_vec())
    }

    fn builtin_condition_wait_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_condition_wait(&mut *self.ctx, args.to_vec())
    }

    fn builtin_condition_notify_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_condition_notify(&mut *self.ctx, args.to_vec())
    }

    fn builtin_princ_shared(&mut self, args: &[Value]) -> EvalResult {
        let target =
            crate::emacs_core::builtins::resolve_print_target_in_state(&*self.ctx, args.get(1));
        if crate::emacs_core::builtins::print_target_is_direct(target) {
            return crate::emacs_core::builtins::builtin_princ_impl(&mut *self.ctx, args.to_vec());
        }
        let text = crate::emacs_core::builtins::print_value_princ_in_state(&*self.ctx, &args[0]);
        crate::emacs_core::builtins::dispatch_print_callback_chars(&text, |ch| {
            self.call_function_with_roots(target, &[ch]).map(|_| ())
        })?;
        Ok(args[0])
    }

    fn builtin_prin1_shared(&mut self, args: &[Value]) -> EvalResult {
        let target =
            crate::emacs_core::builtins::resolve_print_target_in_state(&*self.ctx, args.get(1));
        if crate::emacs_core::builtins::print_target_is_direct(target) {
            return crate::emacs_core::builtins::builtin_prin1_impl(&mut *self.ctx, args.to_vec());
        }
        let text = crate::emacs_core::error::print_value_in_state(&*self.ctx, &args[0]);
        crate::emacs_core::builtins::dispatch_print_callback_chars(&text, |ch| {
            self.call_function_with_roots(target, &[ch]).map(|_| ())
        })?;
        Ok(args[0])
    }

    fn builtin_prin1_to_string_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_prin1_to_string_impl(&*self.ctx, args.to_vec())
    }

    fn builtin_print_shared(&mut self, args: &[Value]) -> EvalResult {
        let target =
            crate::emacs_core::builtins::resolve_print_target_in_state(&*self.ctx, args.get(1));
        if crate::emacs_core::builtins::print_target_is_direct(target) {
            return crate::emacs_core::builtins::builtin_print_impl(&mut *self.ctx, args.to_vec());
        }
        let text = {
            let mut text = String::new();
            text.push('\n');
            text.push_str(&crate::emacs_core::error::print_value_in_state(
                &*self.ctx, &args[0],
            ));
            text.push('\n');
            text
        };
        crate::emacs_core::builtins::dispatch_print_callback_chars(&text, |ch| {
            self.call_function_with_roots(target, &[ch]).map(|_| ())
        })?;
        Ok(args[0])
    }

    fn builtin_terpri_shared(&mut self, args: &[Value]) -> EvalResult {
        if let Some(result) =
            crate::emacs_core::builtins::builtin_terpri_impl(&mut *self.ctx, args.to_vec())?
        {
            return Ok(result);
        }
        let target =
            crate::emacs_core::builtins::resolve_print_target_in_state(&*self.ctx, args.first());
        self.call_function_with_roots(target, &[Value::fixnum('\n' as i64)])?;
        Ok(Value::T)
    }

    fn builtin_write_char_shared(&mut self, args: &[Value]) -> EvalResult {
        if let Some(result) =
            crate::emacs_core::builtins::builtin_write_char_impl(&mut *self.ctx, args.to_vec())?
        {
            return Ok(result);
        }
        let target =
            crate::emacs_core::builtins::resolve_print_target_in_state(&*self.ctx, args.get(1));
        builtins::expect_range_args("write-char", args, 1, 2)?;
        let char_code = builtins::expect_fixnum(&args[0])?;
        self.call_function_with_roots(target, &[Value::fixnum(char_code)])?;
        Ok(Value::fixnum(char_code))
    }

    fn builtin_redraw_frame_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::dispnew::pure::builtin_redraw_frame(&mut *self.ctx, args.to_vec())
    }

    fn builtin_x_get_resource_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::display::builtin_x_get_resource(&mut *self.ctx, args.to_vec())
    }

    fn builtin_x_list_fonts_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::display::builtin_x_list_fonts(&mut *self.ctx, args.to_vec())
    }

    fn builtin_x_server_vendor_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::display::builtin_x_server_vendor(&mut *self.ctx, args.to_vec())
    }

    fn builtin_xw_display_color_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::symbols::builtin_xw_display_color_p_ctx(
            &*self.ctx,
            args.to_vec(),
        )
    }

    fn builtin_display_color_cells_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::display::builtin_display_color_cells(&mut *self.ctx, args.to_vec())
    }

    fn builtin_tty_type_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::terminal::pure::builtin_tty_type(&mut *self.ctx, args.to_vec())
    }

    fn builtin_suspend_tty_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::terminal::pure::builtin_suspend_tty(&mut *self.ctx, args.to_vec())
    }

    fn builtin_resume_tty_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::terminal::pure::builtin_resume_tty(&mut *self.ctx, args.to_vec())
    }

    fn builtin_x_create_frame_shared(&mut self, args: &[Value]) -> EvalResult {
        tracing::debug!("builtin_x_create_frame_shared: delegating to Context");
        crate::emacs_core::window_cmds::builtin_x_create_frame(&mut *self.ctx, args.to_vec())
    }

    fn builtin_make_frame_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::window_cmds::builtin_make_frame(&mut *self.ctx, args.to_vec())
    }

    fn builtin_set_frame_height_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::window_cmds::builtin_set_frame_height(&mut *self.ctx, args.to_vec())
    }

    fn builtin_set_frame_width_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::window_cmds::builtin_set_frame_width(&mut *self.ctx, args.to_vec())
    }

    fn builtin_set_frame_size_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::window_cmds::builtin_set_frame_size(&mut *self.ctx, args.to_vec())
    }

    fn builtin_yes_or_no_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::reader::finish_yes_or_no_p_in_vm_runtime(&mut self.ctx, args)
    }
}

impl<'a> crate::emacs_core::builtins::symbols::MacroexpandRuntime for Vm<'a> {
    fn symbol_function_by_id(&self, symbol: SymId) -> Option<Value> {
        crate::emacs_core::builtins::symbols::symbol_function_cell_in_obarray(
            &self.ctx.obarray,
            symbol,
        )
    }

    fn autoload_do_load_macro(&mut self, autoload: Value, head: Value) -> Result<(), Flow> {
        let args = vec![autoload, head, Value::symbol("macro")];
        let _ = self.with_vm_root_scope(|vm| {
            for value in args.iter().copied() {
                vm.push_dynamic_vm_root(value);
            }
            crate::emacs_core::autoload::builtin_autoload_do_load_in_vm_runtime(&mut vm.ctx, &args)
        })?;
        Ok(())
    }

    fn apply_macro_function(
        &mut self,
        form: Value,
        function: Value,
        args: Vec<Value>,
        environment: Option<Value>,
    ) -> Result<Value, Flow> {
        if let Some(cached) = self
            .ctx
            .lookup_runtime_macro_expansion(function, &args, environment)
        {
            return Ok(cached);
        }
        let args_for_cache = args.clone();
        let expand_start = std::time::Instant::now();
        self.with_dynamic_vm_roots(move |vm| {
            vm.push_dynamic_vm_root(form);
            vm.push_dynamic_vm_root(function);
            if let Some(environment) = environment {
                vm.push_dynamic_vm_root(environment);
            }
            for value in args.iter().copied() {
                vm.push_dynamic_vm_root(value);
            }
            let expanded = vm.with_macro_expansion_scope(|vm| vm.call_function(function, args))?;
            let expand_elapsed = expand_start.elapsed();
            vm.ctx.store_runtime_macro_expansion(
                form,
                function,
                &args_for_cache,
                &expanded,
                expand_elapsed,
                environment,
            );
            Ok(expanded)
        })
    }
}

impl crate::emacs_core::builtins::higher_order::SortRuntime for Vm<'_> {
    fn call_sort_function(&mut self, function: Value, args: Vec<Value>) -> Result<Value, Flow> {
        self.with_vm_root_scope(|vm| {
            for arg in args.iter().copied() {
                vm.push_dynamic_vm_root(arg);
            }
            vm.call_function(function, args)
        })
    }

    fn root_sort_value(&mut self, value: Value) {
        self.push_dynamic_vm_root(value);
    }

    fn compare_sort_keys(
        &mut self,
        left: &Value,
        right: &Value,
    ) -> Result<std::cmp::Ordering, Flow> {
        crate::emacs_core::builtins::symbols::compare_value_lt(self.ctx, left, right)
    }
}

// -- Arithmetic helpers --

fn condition_frame_resume(frame: ConditionFrame) -> ResumeTarget {
    match frame {
        ConditionFrame::Catch { resume, .. } | ConditionFrame::ConditionCase { resume, .. } => {
            resume
        }
        ConditionFrame::HandlerBind { .. } | ConditionFrame::SkipConditions { .. } => {
            unreachable!("VM handler stack only mirrors catch/condition-case frames")
        }
    }
}

fn unwind_handlers_to_selected_resume(
    handlers: &mut Vec<Handler>,
    condition_stack: &mut Vec<ConditionFrame>,
    selected_resume: Option<&ResumeTarget>,
) -> Option<ResumeTarget> {
    while let Some(handler) = handlers.pop() {
        match handler {
            Handler::Condition => {
                let resume = condition_frame_resume(
                    condition_stack
                        .pop()
                        .expect("handler stack and condition stack diverged"),
                );
                if selected_resume.is_some_and(|selected| &resume == selected) {
                    return Some(resume);
                }
            }
        }
    }
    None
}

fn normalize_vm_builtin_error(name: &str, flow: Flow) -> Flow {
    match flow {
        Flow::Signal(mut sig) if sig.symbol_name() == "wrong-number-of-arguments" => {
            if let Some(first) = sig.data.first_mut() {
                if matches!(first.kind(), ValueKind::Symbol(id) if resolve_sym(id) == name) {
                    *first = Value::subr(intern(name));
                }
            }
            Flow::Signal(sig)
        }
        other => other,
    }
}

fn resolve_switch_target(func: &ByteCodeFunction, raw_addr: i64) -> Result<usize, Flow> {
    let raw_addr = usize::try_from(raw_addr).map_err(|_| {
        signal(
            "error",
            vec![Value::string(format!(
                "invalid GNU switch target byte offset {}",
                raw_addr
            ))],
        )
    })?;

    if let Some(offset_map) = &func.gnu_byte_offset_map {
        offset_map.get(&raw_addr).copied().ok_or_else(|| {
            signal(
                "error",
                vec![Value::string(format!(
                    "invalid GNU switch target byte offset {}",
                    raw_addr
                ))],
            )
        })
    } else {
        Ok(raw_addr)
    }
}

fn sym_name(constants: &[Value], idx: u16) -> String {
    constants
        .get(idx as usize)
        .and_then(|v| v.as_symbol_name())
        .unwrap_or("nil")
        .to_string()
}

/// Extract a `SymId` from a bytecode constants vector entry without
/// going through the global string interner.
///
/// `Op::VarRef` / `Op::VarSet` / `Op::VarBind` all reference variables
/// by index into the function's constants table.  Each constant is
/// already a `Value::Symbol(SymId)`, so we can extract the SymId via a
/// pure tag inspection.  Going through `as_symbol_name() -> &str ->
/// intern() -> SymId` instead would acquire the global interner
/// `RwLock` twice per opcode, which dominated debug-build runtime when
/// the byte-compiler iterated over hot loops.
fn sym_id_at(constants: &[Value], idx: u16) -> SymId {
    constants
        .get(idx as usize)
        .and_then(|v| v.as_symbol_id())
        .unwrap_or_else(|| intern("nil"))
}
#[cfg(test)]
#[path = "vm_test.rs"]
mod tests;
