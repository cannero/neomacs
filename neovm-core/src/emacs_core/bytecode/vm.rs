//! Bytecode virtual machine — stack-based interpreter.

use std::collections::{HashMap, HashSet};

use super::chunk::ByteCodeFunction;
use super::opcode::Op;
use crate::buffer::{BufferId, BufferManager, InsertionType, SavedRestrictionState};
use crate::emacs_core::advice::VariableWatcherList;
use crate::emacs_core::builtins;
use crate::emacs_core::coding::CodingSystemManager;
use crate::emacs_core::custom::CustomManager;
use crate::emacs_core::error::*;
use crate::emacs_core::eval::{ConditionFrame, Context, ResumeTarget};
use crate::emacs_core::intern::{SymId, intern, intern_uninterned, resolve_sym};
use crate::emacs_core::regex::MatchData;
use crate::emacs_core::string_escape::{storage_char_len, storage_substring};
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

#[derive(Clone, Debug)]
enum VmUnwindEntry {
    DynamicBinding {
        specpdl_count: usize,
    },
    LexicalBinding {
        sym_id: SymId,
        restored_value: Value,
        old_lexenv: Value,
    },
    Cleanup {
        cleanup: Value,
    },
    CurrentBuffer {
        buffer_id: BufferId,
    },
    Excursion {
        buffer_id: BufferId,
        marker_id: u64,
    },
    Restriction(SavedRestrictionState),
}

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

    fn with_hook_roots<T>(
        &mut self,
        roots: &[Value],
        f: impl FnOnce(&mut Self) -> Result<T, Flow>,
    ) -> Result<T, Flow> {
        self.with_extra_roots(roots, f)
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
        let saved_len = self.ctx.vm_gc_roots.len();
        let result = f(self);
        self.ctx.vm_gc_roots.truncate(saved_len);
        result
    }

    fn push_dynamic_vm_root(&mut self, value: Value) {
        self.ctx.vm_gc_roots.push(value);
    }

    fn with_frame_roots<T>(
        &mut self,
        func: &ByteCodeFunction,
        handlers: &[Handler],
        specpdl: &[VmUnwindEntry],
        extra: &[Value],
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.with_dynamic_vm_roots(|vm| {
            vm.ctx.vm_gc_roots.extend(func.constants.iter().copied());
            // bc_buf is scanned by collect_roots — no need to snapshot stack
            Self::collect_handler_roots(handlers, &mut vm.ctx.vm_gc_roots);
            Self::collect_specpdl_roots(specpdl, &mut vm.ctx.vm_gc_roots);
            vm.ctx.vm_gc_roots.extend(extra.iter().copied());
            f(vm)
        })
    }

    fn with_extra_roots<T>(&mut self, extra: &[Value], f: impl FnOnce(&mut Self) -> T) -> T {
        self.with_dynamic_vm_roots(|vm| {
            vm.ctx.vm_gc_roots.extend(extra.iter().copied());
            f(vm)
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

    fn collect_handler_roots(_handlers: &[Handler], _out: &mut Vec<Value>) {}

    fn collect_specpdl_roots(specpdl: &[VmUnwindEntry], out: &mut Vec<Value>) {
        for entry in specpdl {
            match entry {
                VmUnwindEntry::DynamicBinding { .. } => {}
                VmUnwindEntry::LexicalBinding {
                    restored_value,
                    old_lexenv,
                    ..
                } => {
                    out.push(*restored_value);
                    out.push(*old_lexenv);
                }
                VmUnwindEntry::Cleanup { cleanup } => out.push(*cleanup),
                VmUnwindEntry::CurrentBuffer { .. } | VmUnwindEntry::Excursion { .. } => {}
                VmUnwindEntry::Restriction(saved) => saved.trace_roots(out),
            }
        }
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
        // nested calls.  Always push constants directly into vm_gc_roots
        // so they remain reachable even if the ByteCodeObj tracing has a
        // gap (e.g. cloned ByteCodeFunction whose constants diverge from
        // the heap object, or NIL func_value from sf_byte_code_value).
        let result = self.with_dynamic_vm_roots(|vm| {
            if func_value.is_heap_object() {
                vm.push_dynamic_vm_root(func_value);
            }
            vm.ctx.vm_gc_roots.extend(func.constants.iter().copied());
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
        let mut specpdl: Vec<VmUnwindEntry> = Vec::new();

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
                self.ctx.bc_buf.push(args[i]);
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

        // Push a dynamic frame mapping param names -> values so that inner
        // closures and VarRef lookups can find parameters by name.
        let has_named_params = nonrest > 0 || has_rest;
        if has_named_params {
            let mut frame = OrderedRuntimeBindingMap::new();
            let mut arg_idx = 0;
            for param in &func.params.required {
                frame.insert(
                    *param,
                    if arg_idx < nargs {
                        args[arg_idx]
                    } else {
                        Value::NIL
                    },
                );
                arg_idx += 1;
            }
            for param in &func.params.optional {
                frame.insert(
                    *param,
                    if arg_idx < nargs {
                        args[arg_idx]
                    } else {
                        Value::NIL
                    },
                );
                arg_idx += 1;
            }
            if let Some(rest_name) = func.params.rest {
                let rest_args: Vec<Value> = if arg_idx < nargs {
                    args[arg_idx..].to_vec()
                } else {
                    vec![]
                };
                frame.insert(rest_name, Value::list(rest_args));
            }

            if func.lexical || func.env.is_some() {
                // Lexical bytecode functions prepend parameter bindings onto
                // the current lexical environment, starting from the captured
                // closure env when one exists.
                let saved_lexenv = if let Some(env) = func.env {
                    std::mem::replace(&mut self.ctx.lexenv, env)
                } else {
                    self.ctx.lexenv
                };
                for (sym_id, val) in frame.iter() {
                    if let Some(val) = val.as_value() {
                        self.ctx.lexenv = lexenv_prepend(self.ctx.lexenv, *sym_id, val);
                    }
                }
                let result = self.run_loop(func, frame_base, &mut pc, &mut handlers, &mut specpdl);
                self.ctx.truncate_condition_stack(condition_stack_base);
                self.ctx.lexenv = saved_lexenv;
                let cleanup = self.unwind_specpdl_all(&mut specpdl);
                self.ctx.bc_buf.truncate(frame_base);
                self.ctx.bc_frames.pop();
                return merge_result_with_cleanup(result, cleanup);
            }

            let specpdl_count = self.ctx.specpdl.len();
            for (sym_id, val) in frame.iter() {
                if let Some(val) = val.as_value() {
                    crate::emacs_core::eval::specbind_in_state(
                        &mut self.ctx.obarray,
                        &mut self.ctx.specpdl,
                        *sym_id,
                        val,
                    );
                }
            }
            let result = self.run_loop(func, frame_base, &mut pc, &mut handlers, &mut specpdl);
            self.ctx.truncate_condition_stack(condition_stack_base);
            crate::emacs_core::eval::unbind_to_in_state(
                &mut self.ctx.obarray,
                &mut self.ctx.specpdl,
                specpdl_count,
            );
            let cleanup = self.unwind_specpdl_all(&mut specpdl);
            self.ctx.bc_buf.truncate(frame_base);
            self.ctx.bc_frames.pop();
            return merge_result_with_cleanup(result, cleanup);
        }

        // No params: set up lexenv for lexical closures/functions, then run.
        let saved_lexenv = if let Some(env) = func.env {
            Some(std::mem::replace(&mut self.ctx.lexenv, env))
        } else if func.lexical {
            Some(self.ctx.lexenv)
        } else {
            None
        };

        let result = self.run_loop(func, frame_base, &mut pc, &mut handlers, &mut specpdl);
        self.ctx.truncate_condition_stack(condition_stack_base);

        if let Some(old) = saved_lexenv {
            self.ctx.lexenv = old;
        }
        let cleanup_roots = Self::result_roots(&result);
        let mut cleanup_extra_roots = cleanup_roots.clone();
        Self::collect_specpdl_roots(&specpdl, &mut cleanup_extra_roots);
        let cleanup =
            self.with_frame_roots(func, &handlers, &[], &cleanup_extra_roots, |vm| {
                vm.unwind_specpdl_all(&mut specpdl)
            });
        self.ctx.bc_buf.truncate(frame_base);
        self.ctx.bc_frames.pop();
        merge_result_with_cleanup(result, cleanup)
    }

    fn run_loop(
        &mut self,
        func: &ByteCodeFunction,
        frame_base: usize,
        pc: &mut usize,
        handlers: &mut Vec<Handler>,
        specpdl: &mut Vec<VmUnwindEntry>,
    ) -> EvalResult {
        let ops = &func.ops;
        let constants = &func.constants;

        macro_rules! stk {
            () => { self.ctx.bc_buf };
        }

        macro_rules! vm_try {
            ($expr:expr) => {{
                match $expr {
                    Ok(value) => value,
                    Err(flow) => {
                        self.resume_nonlocal(func, frame_base, pc, handlers, specpdl, flow)?;
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
                    stk!().push(constants[*idx as usize]);
                }
                Op::Nil => stk!().push(Value::NIL),
                Op::True => stk!().push(Value::T),
                Op::Pop => {
                    stk!().pop();
                }
                Op::Dup => {
                    if let Some(&top) = stk!().last() {
                        stk!().push(top);
                    }
                }
                Op::StackRef(n) => {
                    let idx = stk!().len().saturating_sub(1 + *n as usize);
                    let val = stk!()[idx];
                    stk!().push(val);
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
                    let name = sym_name(constants, *idx);
                    let val = vm_try!(self.lookup_var(&name));
                    stk!().push(val);
                }
                Op::VarSet(idx) => {
                    let name = sym_name(constants, *idx);
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let extra = [val];
                    vm_try!(
                        self.with_frame_roots(func, handlers, specpdl, &extra, |vm| vm
                            .assign_var(&name, val),)
                    );
                }
                Op::VarBind(idx) => {
                    let name = sym_name(constants, *idx);
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let old_value = self.lookup_var(&name).unwrap_or(Value::NIL);
                    let name_id = intern(&name);
                    let lexical_bind = func.lexical
                        && !self.ctx.obarray.is_constant_id(name_id)
                        && !self.ctx.obarray.is_special_id(name_id)
                        && !self
                            .ctx
                            .lexenv_declares_special_cached_in(self.ctx.lexenv, name_id);
                    if lexical_bind {
                        let old_lexenv = self.ctx.lexenv;
                        self.ctx.lexenv = lexenv_prepend(self.ctx.lexenv, name_id, val);
                        specpdl.push(VmUnwindEntry::LexicalBinding {
                            sym_id: name_id,
                            restored_value: old_value,
                            old_lexenv,
                        });
                    } else {
                        let specpdl_count = self.ctx.specpdl.len();
                        // Use full specbind which handles buffer-local variables
                        // (LetLocal/LetDefault). The simplified specbind_in_state
                        // only handles plain Let bindings, which causes bugs when
                        // let-binding buffer-local variables like `mode-name`.
                        self.ctx.specbind(name_id, val);
                        specpdl.push(VmUnwindEntry::DynamicBinding { specpdl_count });
                    }
                }
                Op::Unbind(n) => {
                    let mut unwind_roots = Vec::new();
                    Self::collect_specpdl_roots(specpdl, &mut unwind_roots);
                    vm_try!(self.with_frame_roots(
                        func,
                        handlers,
                        &[],
                        &unwind_roots,
                        |vm| vm.unwind_specpdl_n(*n as usize, specpdl),
                    ));
                }

                // -- Function calls --
                Op::Call(n) => {
                    let n = *n as usize;
                    let args_start = stk!().len().saturating_sub(n);
                    let args: Vec<Value> = stk!().drain(args_start..).collect();
                    let func_val = stk!().pop().unwrap_or(Value::NIL);
                    let writeback_names = self.writeback_callable_names(&func_val);
                    let writeback_args = args.clone();
                    let mut call_roots = Vec::with_capacity(args.len() + 1);
                    call_roots.push(func_val);
                    call_roots.extend(args.iter().copied());
                    let result = vm_try!(self.with_frame_roots(
                        func,
                        handlers,
                        specpdl,
                        &call_roots,
                        |vm| vm.call_function(func_val, args),
                    ));
                    if let Some((called_name, alias_target)) = writeback_names.as_ref() {
                        self.maybe_writeback_mutating_first_arg(
                            called_name,
                            alias_target.as_deref(),
                            &writeback_args,
                            &result,
                        );
                    }
                    stk!().push(result);
                }
                Op::Apply(n) => {
                    let n = *n as usize;
                    if n == 0 {
                        let func_val = stk!().pop().unwrap_or(Value::NIL);
                        let call_roots = [func_val];
                        let result = vm_try!(self.with_frame_roots(
                            func,
                            handlers,
                            specpdl,
                            &call_roots,
                            |vm| vm.call_function(func_val, vec![]),
                        ));
                        stk!().push(result);
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
                        let mut call_roots = Vec::with_capacity(args.len() + 1);
                        call_roots.push(func_val);
                        call_roots.extend(args.iter().copied());
                        let result = vm_try!(self.with_frame_roots(
                            func,
                            handlers,
                            specpdl,
                            &call_roots,
                            |vm| vm.call_function(func_val, args),
                        ));
                        if let Some((called_name, alias_target)) = writeback_names.as_ref() {
                            self.maybe_writeback_mutating_first_arg(
                                called_name,
                                alias_target.as_deref(),
                                &writeback_args,
                                &result,
                            );
                        }
                        stk!().push(result);
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
                            frame_base,
                            pc,
                            handlers,
                            specpdl,
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
                        specpdl.push(VmUnwindEntry::CurrentBuffer { buffer_id });
                    }
                }
                Op::SaveExcursion => {
                    if let Some((buffer_id, point)) = self
                        .ctx
                        .buffers
                        .current_buffer()
                        .map(|buffer| (buffer.id, buffer.pt))
                    {
                        let marker_id =
                            self.ctx
                                .buffers
                                .create_marker(buffer_id, point, InsertionType::Before);
                        specpdl.push(VmUnwindEntry::Excursion {
                            buffer_id,
                            marker_id,
                        });
                    }
                }
                Op::SaveRestriction => {
                    if let Some(saved) = self.ctx.buffers.save_current_restriction_state() {
                        specpdl.push(VmUnwindEntry::Restriction(saved));
                    }
                }

                // -- Arithmetic --
                Op::Add => {
                    let b = stk!().pop().unwrap_or(Value::fixnum(0));
                    let a = stk!().pop().unwrap_or(Value::fixnum(0));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "+",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(vm_try!(arith_add(self, &call_args[0], &call_args[1])));
                    }
                }
                Op::Sub => {
                    let b = stk!().pop().unwrap_or(Value::fixnum(0));
                    let a = stk!().pop().unwrap_or(Value::fixnum(0));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "-",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(vm_try!(arith_sub(self, &call_args[0], &call_args[1])));
                    }
                }
                Op::Mul => {
                    let b = stk!().pop().unwrap_or(Value::fixnum(1));
                    let a = stk!().pop().unwrap_or(Value::fixnum(1));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "*",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(vm_try!(arith_mul(self, &call_args[0], &call_args[1])));
                    }
                }
                Op::Div => {
                    let b = stk!().pop().unwrap_or(Value::fixnum(1));
                    let a = stk!().pop().unwrap_or(Value::fixnum(0));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "/",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(vm_try!(arith_div(self, &call_args[0], &call_args[1])));
                    }
                }
                Op::Rem => {
                    let b = stk!().pop().unwrap_or(Value::fixnum(1));
                    let a = stk!().pop().unwrap_or(Value::fixnum(0));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "%",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(vm_try!(arith_rem(&call_args[0], &call_args[1])));
                    }
                }
                Op::Add1 => {
                    let a = stk!().pop().unwrap_or(Value::fixnum(0));
                    let call_args = vec![a];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "1+",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(vm_try!(arith_add1(self, &call_args[0])));
                    }
                }
                Op::Sub1 => {
                    let a = stk!().pop().unwrap_or(Value::fixnum(0));
                    let call_args = vec![a];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "1-",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(vm_try!(arith_sub1(self, &call_args[0])));
                    }
                }
                Op::Negate => {
                    let a = stk!().pop().unwrap_or(Value::fixnum(0));
                    let call_args = vec![a];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "-",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(vm_try!(arith_negate(self, &call_args[0])));
                    }
                }

                // -- Comparison --
                Op::Eqlsign => {
                    let b = stk!().pop().unwrap_or(Value::fixnum(0));
                    let a = stk!().pop().unwrap_or(Value::fixnum(0));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "=",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(Value::bool_val(vm_try!(num_eq(
                            self,
                            &call_args[0],
                            &call_args[1],
                        ))));
                    }
                }
                Op::Gtr => {
                    let b = stk!().pop().unwrap_or(Value::fixnum(0));
                    let a = stk!().pop().unwrap_or(Value::fixnum(0));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        ">",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(Value::bool_val(
                            vm_try!(num_cmp(self, &call_args[0], &call_args[1],)) > 0,
                        ));
                    }
                }
                Op::Lss => {
                    let b = stk!().pop().unwrap_or(Value::fixnum(0));
                    let a = stk!().pop().unwrap_or(Value::fixnum(0));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "<",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(Value::bool_val(
                            vm_try!(num_cmp(self, &call_args[0], &call_args[1],)) < 0,
                        ));
                    }
                }
                Op::Leq => {
                    let b = stk!().pop().unwrap_or(Value::fixnum(0));
                    let a = stk!().pop().unwrap_or(Value::fixnum(0));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "<=",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(Value::bool_val(
                            vm_try!(num_cmp(self, &call_args[0], &call_args[1],)) <= 0,
                        ));
                    }
                }
                Op::Geq => {
                    let b = stk!().pop().unwrap_or(Value::fixnum(0));
                    let a = stk!().pop().unwrap_or(Value::fixnum(0));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        ">=",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(Value::bool_val(
                            vm_try!(num_cmp(self, &call_args[0], &call_args[1],)) >= 0,
                        ));
                    }
                }
                Op::Max => {
                    let b = stk!().pop().unwrap_or(Value::fixnum(0));
                    let a = stk!().pop().unwrap_or(Value::fixnum(0));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "max",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(
                            if vm_try!(num_cmp(self, &call_args[0], &call_args[1])) >= 0 {
                                call_args[0]
                            } else {
                                call_args[1]
                            },
                        );
                    }
                }
                Op::Min => {
                    let b = stk!().pop().unwrap_or(Value::fixnum(0));
                    let a = stk!().pop().unwrap_or(Value::fixnum(0));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "min",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(
                            if vm_try!(num_cmp(self, &call_args[0], &call_args[1])) <= 0 {
                                call_args[0]
                            } else {
                                call_args[1]
                            },
                        );
                    }
                }

                // -- List operations --
                Op::Car => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![val];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "car",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func, handlers, specpdl, "car", call_args,
                        ))
                    };
                    stk!().push(result);
                }
                Op::Cdr => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![val];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "cdr",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func, handlers, specpdl, "cdr", call_args,
                        ))
                    };
                    stk!().push(result);
                }
                Op::CarSafe => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![val];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "car-safe",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        match call_args[0].kind() {
                            ValueKind::Cons => {
                                stk!().push(call_args[0].cons_car());
                            }
                            // Closures are cons lists in official Emacs.
                            ValueKind::Veclike(VecLikeType::Lambda) => {
                                stk!().push(if call_args[0].closure_env().flatten().is_some() {
                                    Value::symbol("closure")
                                } else {
                                    Value::symbol("lambda")
                                });
                            }
                            _ => stk!().push(Value::NIL),
                        }
                    }
                }
                Op::CdrSafe => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![val];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "cdr-safe",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        match call_args[0].kind() {
                            ValueKind::Cons => {
                                stk!().push(call_args[0].cons_cdr());
                            }
                            // Closures are cons lists in official Emacs.
                            ValueKind::Veclike(VecLikeType::Lambda) => {
                                use crate::emacs_core::builtins::lambda_to_cons_list;
                                let list = lambda_to_cons_list(&call_args[0]).unwrap_or(Value::NIL);
                                match list.kind() {
                                    ValueKind::Cons => {
                                        stk!().push(list.cons_cdr());
                                    }
                                    _ => stk!().push(Value::NIL),
                                }
                            }
                            _ => stk!().push(Value::NIL),
                        }
                    }
                }
                Op::Cons => {
                    let cdr_val = stk!().pop().unwrap_or(Value::NIL);
                    let car_val = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![car_val, cdr_val];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "cons",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(Value::cons(call_args[0], call_args[1]));
                    }
                }
                Op::List(n) => {
                    let n = *n as usize;
                    let start = stk!().len().saturating_sub(n);
                    let items: Vec<Value> = stk!().drain(start..).collect();
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "list",
                        items.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(Value::list(items));
                    }
                }
                Op::Length => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![val];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "length",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(vm_try!(length_value(&call_args[0])));
                    }
                }
                Op::Nth => {
                    let list = stk!().pop().unwrap_or(Value::NIL);
                    let n = stk!().pop().unwrap_or(Value::fixnum(0));
                    let call_args = vec![n, list];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "nth",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func, handlers, specpdl, "nth", call_args,
                        ))
                    };
                    stk!().push(result);
                }
                Op::Nthcdr => {
                    let list = stk!().pop().unwrap_or(Value::NIL);
                    let n = stk!().pop().unwrap_or(Value::fixnum(0));
                    let call_args = vec![n, list];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "nthcdr",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func, handlers, specpdl, "nthcdr", call_args,
                        ))
                    };
                    stk!().push(result);
                }
                Op::Elt => {
                    let idx = stk!().pop().unwrap_or(Value::NIL);
                    let seq = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![seq, idx];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "elt",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func, handlers, specpdl, "elt", call_args,
                        ))
                    };
                    stk!().push(result);
                }
                Op::Setcar => {
                    let newcar = stk!().pop().unwrap_or(Value::NIL);
                    let cell = stk!().pop().unwrap_or(Value::NIL);
                    if cell.is_cons() {
                        cell.set_car(newcar);
                        stk!().push(newcar);
                    } else {
                        vm_try!(Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("consp"), cell],
                        )));
                    }
                }
                Op::Setcdr => {
                    let newcdr = stk!().pop().unwrap_or(Value::NIL);
                    let cell = stk!().pop().unwrap_or(Value::NIL);
                    if cell.is_cons() {
                        cell.set_cdr(newcdr);
                        stk!().push(newcdr);
                    } else {
                        vm_try!(Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("consp"), cell],
                        )));
                    }
                }
                Op::Nconc => {
                    let b = stk!().pop().unwrap_or(Value::NIL);
                    let a = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![a, b];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "nconc",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func, handlers, specpdl, "nconc", call_args,
                        ))
                    };
                    stk!().push(result);
                }
                Op::Nreverse => {
                    let list = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![list];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "nreverse",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func, handlers, specpdl, "nreverse", call_args,
                        ))
                    };
                    stk!().push(result);
                }
                Op::Member => {
                    let list = stk!().pop().unwrap_or(Value::NIL);
                    let elt = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![elt, list];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "member",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func, handlers, specpdl, "member", call_args,
                        ))
                    };
                    stk!().push(result);
                }
                Op::Memq => {
                    let list = stk!().pop().unwrap_or(Value::NIL);
                    let elt = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![elt, list];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "memq",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func, handlers, specpdl, "memq", call_args,
                        ))
                    };
                    stk!().push(result);
                }
                Op::Assq => {
                    let alist = stk!().pop().unwrap_or(Value::NIL);
                    let key = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![key, alist];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "assq",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func, handlers, specpdl, "assq", call_args,
                        ))
                    };
                    stk!().push(result);
                }

                // -- Type predicates --
                Op::Symbolp => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![val];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "symbolp",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(Value::bool_val(call_args[0].is_symbol()));
                    }
                }
                Op::Consp => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![val];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "consp",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(Value::bool_val(call_args[0].is_cons()));
                    }
                }
                Op::Stringp => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![val];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "stringp",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(Value::bool_val(call_args[0].is_string()));
                    }
                }
                Op::Listp => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![val];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "listp",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(Value::bool_val(call_args[0].is_list()));
                    }
                }
                Op::Integerp => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![val];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "integerp",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(Value::bool_val(call_args[0].is_integer()));
                    }
                }
                Op::Numberp => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![val];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "numberp",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(Value::bool_val(call_args[0].is_number()));
                    }
                }
                Op::Null | Op::Not => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let opname = if matches!(op, Op::Null) {
                        "null"
                    } else {
                        "not"
                    };
                    let call_args = vec![val];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        opname,
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(Value::bool_val(call_args[0].is_nil()));
                    }
                }
                Op::Eq => {
                    let b = stk!().pop().unwrap_or(Value::NIL);
                    let a = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "eq",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(Value::bool_val(eq_value(&call_args[0], &call_args[1])));
                    }
                }
                Op::Equal => {
                    let b = stk!().pop().unwrap_or(Value::NIL);
                    let a = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "equal",
                        call_args.clone(),
                    )) {
                        stk!().push(result);
                    } else {
                        stk!().push(Value::bool_val(equal_value(
                            &call_args[0],
                            &call_args[1],
                            0,
                        )));
                    }
                }

                // -- String operations --
                Op::Concat(n) => {
                    let n = *n as usize;
                    let start = stk!().len().saturating_sub(n);
                    let parts: Vec<Value> = stk!().drain(start..).collect();
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "concat",
                        parts.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func, handlers, specpdl, "concat", parts,
                        ))
                    };
                    stk!().push(result);
                }
                Op::Substring => {
                    let to = stk!().pop().unwrap_or(Value::NIL);
                    let from = stk!().pop().unwrap_or(Value::fixnum(0));
                    let array = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![array, from, to];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "substring",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(substring_value(&call_args[0], &call_args[1], &call_args[2]))
                    };
                    stk!().push(result);
                }
                Op::StringEqual => {
                    let b = stk!().pop().unwrap_or(Value::NIL);
                    let a = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![a, b];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "string=",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func, handlers, specpdl, "string=", call_args,
                        ))
                    };
                    stk!().push(result);
                }
                Op::StringLessp => {
                    let b = stk!().pop().unwrap_or(Value::NIL);
                    let a = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![a, b];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "string-lessp",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func,
                            handlers,
                            specpdl,
                            "string-lessp",
                            call_args,
                        ))
                    };
                    stk!().push(result);
                }

                // -- Vector operations --
                Op::Aref => {
                    let idx_val = stk!().pop().unwrap_or(Value::fixnum(0));
                    let vec_val = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![vec_val, idx_val];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "aref",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(builtins::builtin_aref(call_args))
                    };
                    stk!().push(result);
                }
                Op::Aset => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let idx_val = stk!().pop().unwrap_or(Value::fixnum(0));
                    let vec_val = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![vec_val, idx_val, val];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "aset",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(builtins::builtin_aset(call_args.clone()))
                    };
                    self.maybe_writeback_mutating_first_arg(
                        "aset", None, &call_args, &result,
                    );
                    stk!().push(result);
                }

                // -- Symbol operations --
                Op::SymbolValue => {
                    let sym = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![sym];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "symbol-value",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func,
                            handlers,
                            specpdl,
                            "symbol-value",
                            call_args,
                        ))
                    };
                    stk!().push(result);
                }
                Op::SymbolFunction => {
                    let sym = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![sym];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "symbol-function",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func,
                            handlers,
                            specpdl,
                            "symbol-function",
                            call_args,
                        ))
                    };
                    stk!().push(result);
                }
                Op::Set => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let sym = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![sym, val];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "set",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func, handlers, specpdl, "set", call_args,
                        ))
                    };
                    stk!().push(result);
                }
                Op::Fset => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let sym = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![sym, val];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "fset",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func, handlers, specpdl, "fset", call_args,
                        ))
                    };
                    stk!().push(result);
                }
                Op::Get => {
                    let prop = stk!().pop().unwrap_or(Value::NIL);
                    let sym = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![sym, prop];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "get",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func, handlers, specpdl, "get", call_args,
                        ))
                    };
                    stk!().push(result);
                }
                Op::Put => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let prop = stk!().pop().unwrap_or(Value::NIL);
                    let sym = stk!().pop().unwrap_or(Value::NIL);
                    let call_args = vec![sym, prop, val];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        handlers,
                        specpdl,
                        "put",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func, handlers, specpdl, "put", call_args,
                        ))
                    };
                    stk!().push(result);
                }

                // -- Error handling --
                Op::PushConditionCase(target) => {
                    let stack_len = stk!().len();
                    let spec_depth = specpdl.len();
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
                            },
                        });
                }
                Op::PushConditionCaseRaw(target) => {
                    // GNU bytecode consumes the handler pattern operand from TOS.
                    let conditions = stk!().pop().unwrap_or(Value::NIL);
                    let stack_len = stk!().len();
                    let spec_depth = specpdl.len();
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
                            },
                        });
                }
                Op::PushCatch(target) => {
                    let tag = stk!().pop().unwrap_or(Value::NIL);
                    let stack_len = stk!().len();
                    let spec_depth = specpdl.len();
                    let resume_id = self.ctx.allocate_resume_id();
                    handlers.push(Handler::Condition);
                    self.ctx.push_condition_frame(ConditionFrame::Catch {
                        tag,
                        resume: ResumeTarget::VmCatch {
                            resume_id,
                            target: *target,
                            stack_len,
                            spec_depth,
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
                    specpdl.push(VmUnwindEntry::Cleanup { cleanup });
                }
                Op::Throw => {
                    let val = stk!().pop().unwrap_or(Value::NIL);
                    let tag = stk!().pop().unwrap_or(Value::NIL);
                    self.resume_nonlocal(
                        func,
                        frame_base,
                        pc,
                        handlers,
                        specpdl,
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
                        stk!().push(Value::make_bytecode(closure));
                    } else {
                        stk!().push(val);
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
                        handlers,
                        specpdl,
                        &name,
                        args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin_with_frame(
                            func, handlers, specpdl, &name, args,
                        ))
                    };
                    self.maybe_writeback_mutating_first_arg(
                        &name,
                        None,
                        &writeback_args,
                        &result,
                    );
                    stk!().push(result);
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
        handlers: &[Handler],
        specpdl: &[VmUnwindEntry],
        name: &str,
        args: Vec<Value>,
    ) -> Result<Option<Value>, Flow> {
        if self.named_builtin_fast_path_allowed(name) {
            return Ok(None);
        }

        let func_val = Value::symbol(name);
        let mut call_roots = Vec::with_capacity(args.len() + 1);
        call_roots.push(func_val);
        call_roots.extend(args.iter().copied());
        self.with_frame_roots(func, handlers, specpdl, &call_roots, |vm| {
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

    fn lookup_var(&mut self, name: &str) -> EvalResult {
        if name.starts_with(':') {
            return Ok(Value::keyword(name));
        }

        let name_id = intern(name);
        let resolved = crate::emacs_core::builtins::symbols::resolve_variable_alias_id_in_obarray(
            &self.ctx.obarray,
            name_id,
        )?;
        let resolved_name = resolve_sym(resolved);
        // Match GNU eval_sub: lexical environment lookup happens before alias
        // resolution fallback and does not rescan declared-special flags.
        if let Some(val) = self.ctx.lexenv_lookup_cached_in(self.ctx.lexenv, name_id) {
            return Ok(val);
        }
        if resolved != name_id
            && let Some(val) = self.ctx.lexenv_lookup_cached_in(self.ctx.lexenv, resolved)
        {
            return Ok(val);
        }

        // specbind writes directly to obarray, so dynamic stack lookup is
        // no longer needed — fall through to buffer-local and obarray lookups.

        // Current buffer-local binding.
        if crate::emacs_core::builtins::is_canonical_symbol_id(resolved)
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

        // Obarray
        if let Some(val) = self.ctx.obarray.symbol_value_id(resolved) {
            return Ok(*val);
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
        let mut roots = Vec::with_capacity(args.len() + 1);
        roots.push(function);
        roots.extend(args.iter().copied());
        self.with_extra_roots(&roots, |vm| vm.call_function(function, args.to_vec()))
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
            || self.ctx.custom.is_auto_buffer_local(resolved_name);
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
        self.with_extra_roots(&[func, sequence], |vm| {
            let mut results = Vec::new();
            let map_result = crate::emacs_core::builtins::higher_order::for_each_sequence_element(
                &sequence,
                |item| {
                    let value = vm.with_extra_roots(&results, |vm| {
                        vm.with_extra_roots(&[item], |vm| vm.call_function(func, vec![item]))
                    })?;
                    results.push(value);
                    Ok(())
                },
            );

            match map_result {
                Ok(()) => vm.with_extra_roots(&results, |_| Ok(Value::list(results.clone()))),
                Err(flow) => Err(flow),
            }
        })
    }

    fn builtin_mapc_fast(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("mapc", args, 2)?;
        let func = args[0];
        let sequence = args[1];
        self.with_extra_roots(&[func, sequence], |vm| {
            crate::emacs_core::builtins::higher_order::for_each_sequence_element(
                &sequence,
                |item| {
                    vm.with_extra_roots(&[item], |vm| vm.call_function(func, vec![item]))?;
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
        self.with_extra_roots(&[func, sequence], |vm| {
            let mut mapped = Vec::new();
            let map_result = crate::emacs_core::builtins::higher_order::for_each_sequence_element(
                &sequence,
                |item| {
                    let value = vm.with_extra_roots(&mapped, |vm| {
                        vm.with_extra_roots(&[item], |vm| vm.call_function(func, vec![item]))
                    })?;
                    mapped.push(value);
                    Ok(())
                },
            );

            match map_result {
                Ok(()) => vm.with_extra_roots(&mapped, |_| {
                    crate::emacs_core::builtins::builtin_nconc(mapped.clone())
                }),
                Err(flow) => Err(flow),
            }
        })
    }

    fn builtin_mapconcat_fast(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_range_args("mapconcat", args, 2, 3)?;
        let func = args[0];
        let sequence = args[1];
        let separator = args.get(2).copied().unwrap_or_else(|| Value::string(""));
        self.with_extra_roots(&[func, sequence, separator], |vm| {
            let mut parts = Vec::new();
            let map_result = crate::emacs_core::builtins::higher_order::for_each_sequence_element(
                &sequence,
                |item| {
                    let value = vm.with_extra_roots(&parts, |vm| {
                        vm.with_extra_roots(&[item], |vm| vm.call_function(func, vec![item]))
                    })?;
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
                    vm.with_extra_roots(&parts, |vm| {
                        vm.with_extra_roots(&concat_args, |_| {
                            crate::emacs_core::builtins::builtin_concat(concat_args.clone())
                        })
                    })
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
        self.with_extra_roots(&[sequence, key_fn, lessp_fn], |vm| {
            vm.with_dynamic_vm_roots(|vm| match sequence.kind() {
                ValueKind::Nil => Ok(Value::NIL),
                ValueKind::Cons => {
                    let mut cons_cells = Vec::new();
                    let mut values = Vec::new();
                    let mut cursor = sequence;
                    loop {
                        match cursor.kind() {
                            ValueKind::Nil => break,
                            ValueKind::Cons => {
                                values.push(cursor.cons_car());
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
                    vm.with_extra_roots(&values, |vm| {
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
                    })
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
                    vm.with_extra_roots(&values, |vm| {
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
                    })
                }
                _other => Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("list-or-vector-p"), sequence],
                )),
            })
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
        Ok(frame
            .parameters
            .get("window-system")
            .copied()
            .unwrap_or(Value::T))
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
            "name" => Ok(Value::string(frame.name.clone())),
            "title" => Ok(Value::string(frame.title.clone())),
            "width" => Ok(frame
                .parameters
                .get("width")
                .cloned()
                .unwrap_or(Value::fixnum(frame.columns() as i64))),
            "height" => Ok(frame
                .parameters
                .get("height")
                .cloned()
                .unwrap_or(Value::fixnum(frame.lines() as i64))),
            "visibility" => Ok(if frame.visible { Value::T } else { Value::NIL }),
            _ => Ok(frame
                .parameters
                .get(&param_name)
                .cloned()
                .unwrap_or(Value::NIL)),
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
                    let mut extra_roots = Vec::with_capacity(args.len() + load_args.len() + 1);
                    extra_roots.push(target);
                    extra_roots.extend(args.iter().copied());
                    extra_roots.extend(load_args.iter().copied());
                    target = crate::emacs_core::autoload::builtin_autoload_do_load_in_vm_runtime(
                        &mut self.ctx,
                        &[],
                        &load_args,
                        &extra_roots,
                    )?;
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
        self.ctx.push_runtime_backtrace_frame(func_val, &args);
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
        self.ctx.pop_runtime_backtrace_frame();
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
        let mut specpdl: Vec<VmUnwindEntry> = Vec::new();
        let result = self.run_loop(func, frame_base, &mut pc, &mut handlers, &mut specpdl);
        self.ctx.truncate_condition_stack(condition_stack_base);
        let cleanup_roots = Self::result_roots(&result);
        let mut cleanup_extra_roots = cleanup_roots.clone();
        Self::collect_specpdl_roots(&specpdl, &mut cleanup_extra_roots);
        let cleanup =
            self.with_frame_roots(func, &handlers, &[], &cleanup_extra_roots, |vm| {
                vm.unwind_specpdl_all(&mut specpdl)
            });
        self.ctx.bc_buf.truncate(frame_base);
        self.ctx.bc_frames.pop();
        merge_result_with_cleanup(result, cleanup)
    }

    fn resume_nonlocal(
        &mut self,
        _func: &ByteCodeFunction,
        _frame_base: usize,
        pc: &mut usize,
        handlers: &mut Vec<Handler>,
        specpdl: &mut Vec<VmUnwindEntry>,
        flow: Flow,
    ) -> Result<(), Flow> {
        match flow {
            Flow::Throw { tag, value } => {
                let selected_resume = self.ctx.matching_catch_resume(&tag);
                if let Some(ResumeTarget::VmCatch {
                    target,
                    stack_len,
                    spec_depth,
                    ..
                }) = unwind_handlers_to_selected_resume(
                    handlers,
                    &mut self.ctx.condition_stack,
                    selected_resume.as_ref(),
                ) {
                    let extra = [tag, value];
                    let mut unwind_roots = extra.to_vec();
                    Self::collect_specpdl_roots(specpdl, &mut unwind_roots);
                    if let Err(cleanup_flow) =
                        self.with_frame_roots(_func, handlers, &[], &unwind_roots, |vm| {
                            vm.unwind_specpdl_to(spec_depth, specpdl)
                        })
                    {
                        return self.resume_nonlocal(
                            _func,
                            _frame_base,
                            pc,
                            handlers,
                            specpdl,
                            cleanup_flow,
                        );
                    }
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
                // dispatch_signal_if_needed may call signal hooks and
                // handler-bind handlers via eval.apply(), which can trigger
                // GC.  We must root the current frame so values survive
                // collection.
                let mut sig_extra = Vec::new();
                Self::collect_flow_roots(&Flow::Signal(sig.clone()), &mut sig_extra);
                let sig = match self.with_frame_roots(
                    _func, handlers, specpdl, &sig_extra,
                    |vm| vm.ctx.dispatch_signal_if_needed(sig),
                ) {
                    Ok(sig) => sig,
                    Err(flow) => {
                        return self.resume_nonlocal(_func, _frame_base, pc, handlers, specpdl, flow);
                    }
                };
                if let Some(ResumeTarget::VmConditionCase {
                    target,
                    stack_len,
                    spec_depth,
                    ..
                }) = unwind_handlers_to_selected_resume(
                    handlers,
                    &mut self.ctx.condition_stack,
                    sig.selected_resume.as_ref(),
                ) {
                    let mut signal_roots = Vec::new();
                    Self::collect_flow_roots(&Flow::Signal(sig.clone()), &mut signal_roots);
                    let mut unwind_roots = signal_roots.clone();
                    Self::collect_specpdl_roots(specpdl, &mut unwind_roots);
                    if let Err(cleanup_flow) =
                        self.with_frame_roots(_func, handlers, &[], &unwind_roots, |vm| {
                            vm.unwind_specpdl_to(spec_depth, specpdl)
                        })
                    {
                        return self.resume_nonlocal(
                            _func,
                            _frame_base,
                            pc,
                            handlers,
                            specpdl,
                            cleanup_flow,
                        );
                    }
                    self.ctx.bc_buf.truncate(stack_len);
                    self.ctx.bc_buf.push(make_signal_binding_value(&sig));
                    *pc = target as usize;
                    return Ok(());
                }
                Err(Flow::Signal(sig))
            }
        }
    }

    fn unwind_specpdl_all(&mut self, specpdl: &mut Vec<VmUnwindEntry>) -> Result<(), Flow> {
        self.unwind_specpdl_to(0, specpdl)
    }

    fn unwind_specpdl_n(
        &mut self,
        count: usize,
        specpdl: &mut Vec<VmUnwindEntry>,
    ) -> Result<(), Flow> {
        let target_depth = specpdl.len().saturating_sub(count);
        self.unwind_specpdl_to(target_depth, specpdl)
    }

    fn unwind_specpdl_to(
        &mut self,
        target_depth: usize,
        specpdl: &mut Vec<VmUnwindEntry>,
    ) -> Result<(), Flow> {
        while specpdl.len() > target_depth {
            let entry = specpdl.pop().expect("specpdl entry");
            self.restore_unwind_entry(entry)?;
        }
        Ok(())
    }

    fn restore_unwind_entry(&mut self, entry: VmUnwindEntry) -> Result<(), Flow> {
        match entry {
            VmUnwindEntry::DynamicBinding { specpdl_count } => {
                // Use full unbind_to which handles LetLocal (buffer-local)
                // and LetDefault bindings, not just plain Let bindings.
                self.ctx.unbind_to(specpdl_count);
            }
            VmUnwindEntry::LexicalBinding {
                sym_id: _sym_id,
                restored_value: _restored_value,
                old_lexenv,
            } => {
                self.ctx.lexenv = old_lexenv;
            }
            VmUnwindEntry::Cleanup { cleanup } => {
                let cleanup_root = [cleanup];
                self.with_extra_roots(&cleanup_root, |vm| vm.call_function(cleanup, vec![]))?;
            }
            VmUnwindEntry::CurrentBuffer { buffer_id } => {
                self.ctx.restore_current_buffer_if_live(buffer_id);
            }
            VmUnwindEntry::Excursion {
                buffer_id,
                marker_id,
            } => {
                if self.ctx.buffers.get(buffer_id).is_some() {
                    self.ctx.restore_current_buffer_if_live(buffer_id);
                    if let Some(saved_pt) = self.ctx.buffers.marker_position(buffer_id, marker_id) {
                        let _ = self.ctx.buffers.goto_buffer_byte(buffer_id, saved_pt);
                    }
                }
                self.ctx.buffers.remove_marker(marker_id);
            }
            VmUnwindEntry::Restriction(saved) => self.restore_saved_restriction(saved),
        }
        Ok(())
    }

    fn restore_saved_restriction(&mut self, saved: SavedRestrictionState) {
        self.ctx.buffers.restore_saved_restriction_state(saved);
    }

    fn dispatch_vm_builtin_with_frame(
        &mut self,
        func: &ByteCodeFunction,
        handlers: &[Handler],
        specpdl: &[VmUnwindEntry],
        name: &str,
        args: Vec<Value>,
    ) -> EvalResult {
        let extra_roots = args.clone();
        self.with_frame_roots(func, handlers, specpdl, &extra_roots, |vm| {
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
        directory: &str,
        f: impl FnOnce(&mut Self) -> Result<T, Flow>,
    ) -> Result<T, Flow> {
        let specpdl_count = self.ctx.specpdl.len();
        crate::emacs_core::eval::specbind_in_state(
            &mut self.ctx.obarray,
            &mut self.ctx.specpdl,
            intern("default-directory"),
            Value::string(directory),
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
        crate::emacs_core::doc::builtin_documentation_in_vm_runtime(
            &mut self.ctx,
            &[],
            args.to_vec(),
        )
    }

    fn builtin_documentation_property_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::doc::builtin_documentation_property_in_vm_runtime(
            &mut self.ctx,
            &[],
            args.to_vec(),
        )
    }

    fn builtin_format_mode_line_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::xdisp::builtin_format_mode_line_in_vm_runtime(&mut self.ctx, &[], args)
    }

    fn builtin_read_from_minibuffer_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::reader::finish_read_from_minibuffer_in_vm_runtime(
            &mut self.ctx,
            &[],
            args,
        )
    }

    fn builtin_call_interactively_shared(&mut self, args: &[Value]) -> EvalResult {
        let mut plan = crate::emacs_core::interactive::plan_call_interactively_in_state(
            &self.ctx.obarray,
            &self.ctx.interactive,
            self.ctx.read_command_keys(),
            args,
        )?;
        let extra_roots = args.to_vec();
        if crate::emacs_core::interactive::callable_form_needs_instantiation(&plan.func) {
            plan.func = self.ctx.instantiate_callable_cons_form(plan.func)?;
        }
        let (function, call_args) =
            crate::emacs_core::interactive::resolve_call_interactively_target_and_args_with_vm_fallback(
                &mut self.ctx,
                &mut plan,
                &[],
                &extra_roots,
            )?;
        let mut funcall_args = Vec::with_capacity(call_args.len() + 1);
        funcall_args.push(function);
        funcall_args.extend(call_args);
        self.call_function_with_roots(Value::symbol("funcall-interactively"), &funcall_args)
    }

    fn builtin_assoc_shared(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_range_args("assoc", args, 2, 3)?;
        if args.get(2).is_some_and(|value| !value.is_nil()) {
            let key = args[0];
            let list = args[1];
            let test_fn = args[2];
            return self.with_extra_roots(&[key, list, test_fn], |vm| {
                let mut cursor = list;
                loop {
                    match cursor.kind() {
                        ValueKind::Nil => return Ok(Value::NIL),
                        ValueKind::Cons => {
                            let pair_car = cursor.cons_car();
                            let pair_cdr = cursor.cons_cdr();
                            if let ValueKind::Cons = pair_car.kind() {
                                let entry_key = pair_car.cons_car();
                                let matches = vm.with_extra_roots(
                                    &[cursor, pair_car, pair_cdr, entry_key],
                                    |vm| {
                                        vm.call_function(test_fn, vec![entry_key, key])
                                            .map(|value| value.is_truthy())
                                    },
                                )?;
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
            return self.with_extra_roots(&[plist, prop, predicate], |vm| {
                let mut cursor = plist;
                loop {
                    match cursor.kind() {
                        ValueKind::Cons => {
                            let pair_car = cursor.cons_car();
                            let pair_cdr = cursor.cons_cdr();
                            let entry_key = pair_car;
                            let matches =
                                vm.with_extra_roots(&[cursor, entry_key, pair_cdr], |vm| {
                                    vm.call_function(predicate, vec![entry_key, prop])
                                        .map(|value| value.is_truthy())
                                })?;
                            if matches {
                                return Ok(cursor);
                            }

                            match pair_cdr.kind() {
                                ValueKind::Cons => {
                                    cursor = pair_cdr.cons_cdr();
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
        Ok(Value::NIL)
    }

    fn builtin_macroexpand_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::symbols::builtin_macroexpand_with_runtime(self, args.to_vec())
    }

    fn builtin_mapatoms_shared(&mut self, args: &[Value]) -> EvalResult {
        let (func, symbols) =
            crate::emacs_core::hashtab::collect_mapatoms_symbols(&self.ctx.obarray, args.to_vec())?;
        let mut roots = Vec::with_capacity(symbols.len() + 1);
        roots.push(func);
        roots.extend(symbols.iter().copied());
        self.with_extra_roots(&roots, |vm| {
            for sym in symbols {
                vm.call_function(func, vec![sym])?;
            }
            Ok(Value::NIL)
        })
    }

    fn builtin_maphash_shared(&mut self, args: &[Value]) -> EvalResult {
        let (func, entries) = crate::emacs_core::hashtab::collect_maphash_entries(args.to_vec())?;
        let mut roots = Vec::with_capacity(entries.len() * 2 + 1);
        roots.push(func);
        for (key, value) in &entries {
            roots.push(*key);
            roots.push(*value);
        }
        self.with_extra_roots(&roots, |vm| {
            for (key, value) in entries {
                vm.call_function(func, vec![key, value])?;
            }
            Ok(Value::NIL)
        })
    }

    fn builtin_read_string_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::reader::finish_read_string_in_vm_runtime(&mut self.ctx, &[], args)
    }

    fn builtin_completing_read_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::reader::finish_completing_read_in_vm_runtime(&mut self.ctx, &[], args)
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
        let regexps =
            crate::emacs_core::minibuffer::completion_regexp_list_from_obarray(&self.ctx.obarray);
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
        let regexps =
            crate::emacs_core::minibuffer::completion_regexp_list_from_obarray(&self.ctx.obarray);
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
                    self.with_default_directory_binding(bound_directory.as_str(), |vm| {
                        vm.call_function_with_roots(predicate, &[predicate_arg])
                    })
                },
            );
        }
        crate::emacs_core::dired::builtin_file_name_completion(&mut *self.ctx, args.to_vec())
    }

    fn builtin_read_command_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::minibuffer::finish_read_command_in_vm_runtime(&mut self.ctx, &[], args)
    }

    fn builtin_read_variable_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::minibuffer::finish_read_variable_in_vm_runtime(&mut self.ctx, &[], args)
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
        let regexps =
            crate::emacs_core::minibuffer::completion_regexp_list_from_obarray(&self.ctx.obarray);
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
        crate::emacs_core::reader::finish_yes_or_no_p_in_vm_runtime(&mut self.ctx, &[], args)
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
        let extra_roots = args.clone();
        let _ = crate::emacs_core::autoload::builtin_autoload_do_load_in_vm_runtime(
            &mut self.ctx,
            &[],
            &args,
            &extra_roots,
        )?;
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
        let mut extra_roots = Vec::with_capacity(args.len() + 3);
        extra_roots.push(form);
        extra_roots.push(function);
        if let Some(environment) = environment {
            extra_roots.push(environment);
        }
        extra_roots.extend(args.iter().copied());
        self.with_extra_roots(&extra_roots, move |vm| {
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

fn merge_result_with_cleanup(result: EvalResult, cleanup: Result<(), Flow>) -> EvalResult {
    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

impl crate::emacs_core::builtins::higher_order::SortRuntime for Vm<'_> {
    fn call_sort_function(&mut self, function: Value, args: Vec<Value>) -> Result<Value, Flow> {
        let roots = args.clone();
        self.with_extra_roots(&roots, |vm| vm.call_function(function, args))
    }

    fn root_sort_value(&mut self, value: Value) {
        self.push_dynamic_vm_root(value);
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

fn arith_add(vm: &Vm<'_>, a: &Value, b: &Value) -> EvalResult {
    match (a.kind(), b.kind()) {
        (ValueKind::Fixnum(a), ValueKind::Fixnum(b)) => Ok(Value::fixnum(a.wrapping_add(b))),
        _ => {
            let a = number_or_marker_as_f64(vm, a)?;
            let b = number_or_marker_as_f64(vm, b)?;
            Ok(Value::make_float(a + b)) // TODO(tagged): remove next_float_id()
        }
    }
}

fn arith_sub(vm: &Vm<'_>, a: &Value, b: &Value) -> EvalResult {
    match (a.kind(), b.kind()) {
        (ValueKind::Fixnum(a), ValueKind::Fixnum(b)) => Ok(Value::fixnum(a.wrapping_sub(b))),
        _ => {
            let a = number_or_marker_as_f64(vm, a)?;
            let b = number_or_marker_as_f64(vm, b)?;
            Ok(Value::make_float(a - b)) // TODO(tagged): remove next_float_id()
        }
    }
}

fn arith_mul(vm: &Vm<'_>, a: &Value, b: &Value) -> EvalResult {
    match (a.kind(), b.kind()) {
        (ValueKind::Fixnum(a), ValueKind::Fixnum(b)) => Ok(Value::fixnum(a.wrapping_mul(b))),
        _ => {
            let a = number_or_marker_as_f64(vm, a)?;
            let b = number_or_marker_as_f64(vm, b)?;
            Ok(Value::make_float(a * b)) // TODO(tagged): remove next_float_id()
        }
    }
}

fn arith_div(vm: &Vm<'_>, a: &Value, b: &Value) -> EvalResult {
    match (a.kind(), b.kind()) {
        (ValueKind::Fixnum(_), ValueKind::Fixnum(0)) => Err(signal(
            "arith-error",
            vec![Value::string("Division by zero")],
        )),
        (ValueKind::Fixnum(a), ValueKind::Fixnum(b)) => {
            // Integer division — matches GNU Emacs and the interpreter's builtin_div
            Ok(Value::fixnum(a.checked_div(b).unwrap_or(0)))
        }
        _ => {
            let a = number_or_marker_as_f64(vm, a)?;
            let b = number_or_marker_as_f64(vm, b)?;
            if b == 0.0 {
                return Err(signal(
                    "arith-error",
                    vec![Value::string("Division by zero")],
                ));
            }
            Ok(Value::make_float(a / b))
        }
    }
}

fn arith_rem(a: &Value, b: &Value) -> EvalResult {
    match (a.kind(), b.kind()) {
        (ValueKind::Fixnum(_), ValueKind::Fixnum(0)) => Err(signal(
            "arith-error",
            vec![Value::string("Division by zero")],
        )),
        (ValueKind::Fixnum(a), ValueKind::Fixnum(b)) => {
            Ok(Value::fixnum(a.checked_rem(b).unwrap_or(0)))
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *a],
        )),
    }
}

fn arith_add1(vm: &Vm<'_>, a: &Value) -> EvalResult {
    match a.kind() {
        ValueKind::Fixnum(n) => Ok(Value::fixnum(n.wrapping_add(1))),
        ValueKind::Float => Ok(Value::make_float(a.xfloat() + 1.0)),
        _ if a.is_marker() => Ok(Value::fixnum(
            crate::emacs_core::marker::marker_position_as_int_with_buffers(&vm.ctx.buffers, a)?
                .wrapping_add(1),
        )),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), *a],
        )),
    }
}

fn arith_sub1(vm: &Vm<'_>, a: &Value) -> EvalResult {
    match a.kind() {
        ValueKind::Fixnum(n) => Ok(Value::fixnum(n.wrapping_sub(1))),
        ValueKind::Float => Ok(Value::make_float(a.xfloat() - 1.0)),
        _ if a.is_marker() => Ok(Value::fixnum(
            crate::emacs_core::marker::marker_position_as_int_with_buffers(&vm.ctx.buffers, a)?
                .wrapping_sub(1),
        )),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), *a],
        )),
    }
}

fn arith_negate(vm: &Vm<'_>, a: &Value) -> EvalResult {
    match a.kind() {
        ValueKind::Fixnum(n) => Ok(Value::fixnum(-n)),
        ValueKind::Float => Ok(Value::make_float(-a.xfloat())),
        _ if a.is_marker() => Ok(Value::fixnum(
            -crate::emacs_core::marker::marker_position_as_int_with_buffers(&vm.ctx.buffers, a)?,
        )),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), *a],
        )),
    }
}

fn num_eq(vm: &Vm<'_>, a: &Value, b: &Value) -> Result<bool, Flow> {
    match (a.kind(), b.kind()) {
        (ValueKind::Fixnum(a), ValueKind::Fixnum(b)) => Ok(a == b),
        _ => {
            let a = number_or_marker_as_f64(vm, a)?;
            let b = number_or_marker_as_f64(vm, b)?;
            Ok(a == b)
        }
    }
}

fn num_cmp(vm: &Vm<'_>, a: &Value, b: &Value) -> Result<i32, Flow> {
    match (a.kind(), b.kind()) {
        (ValueKind::Fixnum(a), ValueKind::Fixnum(b)) => Ok(a.cmp(&b) as i32),
        _ => {
            let a = number_or_marker_as_f64(vm, a)?;
            let b = number_or_marker_as_f64(vm, b)?;
            Ok(if a < b {
                -1
            } else if a > b {
                1
            } else {
                0
            })
        }
    }
}

fn number_or_marker_as_f64(vm: &Vm<'_>, value: &Value) -> Result<f64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n as f64),
        ValueKind::Float => Ok(value.xfloat()),
        _ if value.is_marker() => Ok(
            crate::emacs_core::marker::marker_position_as_int_with_buffers(&vm.ctx.buffers, value)?
                as f64,
        ),
        _other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), *value],
        )),
    }
}

fn length_value(val: &Value) -> EvalResult {
    match val.kind() {
        ValueKind::Nil => Ok(Value::fixnum(0)),
        ValueKind::String => Ok(Value::fixnum(val.as_str().unwrap().chars().count() as i64)),
        ValueKind::Veclike(VecLikeType::Vector) => {
            Ok(Value::fixnum(val.as_vector_data().unwrap().len() as i64))
        }
        ValueKind::Veclike(VecLikeType::Lambda) | ValueKind::Veclike(VecLikeType::ByteCode) => {
            Ok(Value::fixnum(builtins::closure_vector_length(val).unwrap()))
        }
        ValueKind::Cons => {
            let mut len: i64 = 0;
            let mut cursor = *val;
            loop {
                match cursor.kind() {
                    ValueKind::Cons => {
                        len += 1;
                        cursor = cursor.cons_cdr();
                    }
                    ValueKind::Nil => return Ok(Value::fixnum(len)),
                    _tail => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("listp"), cursor],
                        ));
                    }
                }
            }
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), *val],
        )),
    }
}

fn substring_value(array: &Value, from: &Value, to: &Value) -> EvalResult {
    let len = match array.kind() {
        ValueKind::String => storage_char_len(array.as_str().unwrap()) as i64,
        ValueKind::Veclike(VecLikeType::Vector) => array.as_vector_data().unwrap().len() as i64,
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("arrayp"), *array],
            ));
        }
    };

    let normalize_index = |value: &Value, default: i64| -> Result<i64, Flow> {
        let raw = if value.is_nil() {
            default
        } else {
            match value.kind() {
                ValueKind::Fixnum(i) => i,
                _ => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("integerp"), *value],
                    ));
                }
            }
        };
        let idx = if raw < 0 { len + raw } else { raw };
        if idx < 0 || idx > len {
            return Err(signal("args-out-of-range", vec![*array, *from, *to]));
        }
        Ok(idx)
    };

    let start = normalize_index(from, 0)? as usize;
    let end = normalize_index(to, len)? as usize;
    if start > end {
        return Err(signal("args-out-of-range", vec![*array, *from, *to]));
    }

    match array.kind() {
        ValueKind::String => {
            let s = array.as_str().unwrap().to_owned();
            let result = storage_substring(&s, start, end)
                .ok_or_else(|| signal("args-out-of-range", vec![*array, *from, *to]))?;
            Ok(Value::string(result))
        }
        ValueKind::Veclike(VecLikeType::Vector) => {
            let data = array.as_vector_data().unwrap().clone();
            if end > data.len() {
                return Err(signal("args-out-of-range", vec![*array, *from, *to]));
            }
            Ok(Value::vector(data[start..end].to_vec()))
        }
        _ => unreachable!(),
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
#[cfg(test)]
#[path = "vm_test.rs"]
mod tests;
