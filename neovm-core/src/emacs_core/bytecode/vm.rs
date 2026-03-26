//! Bytecode virtual machine — stack-based interpreter.

use std::collections::{HashMap, HashSet};

use super::chunk::ByteCodeFunction;
use super::opcode::Op;
use crate::buffer::{BufferId, BufferManager, InsertionType, SavedRestrictionState};
use crate::emacs_core::advice::VariableWatcherList;
use crate::emacs_core::builtins;
use crate::emacs_core::category::CategoryManager;
use crate::emacs_core::coding::CodingSystemManager;
use crate::emacs_core::custom::CustomManager;
use crate::emacs_core::error::*;
use crate::emacs_core::errors::signal_matches_condition_value;
use crate::emacs_core::eval::Context;
use crate::emacs_core::intern::{SymId, intern, intern_uninterned, resolve_sym};
use crate::emacs_core::regex::MatchData;
use crate::emacs_core::string_escape::{storage_char_len, storage_substring};
use crate::emacs_core::symbol::Obarray;
use crate::emacs_core::value::*;
use crate::window::{FrameId, FrameManager, Window};

/// Handler frame for catch/condition-case/unwind-protect.
#[derive(Clone, Debug)]
#[allow(dead_code)]
enum Handler {
    /// catch: tag value, jump target.
    Catch {
        tag: Value,
        target: u32,
        stack_len: usize,
        spec_depth: usize,
    },
    /// condition-case: handler patterns, jump target.
    ConditionCase {
        conditions: Value,
        target: u32,
        stack_len: usize,
        spec_depth: usize,
    },
    /// unwind-protect: cleanup target.
    UnwindProtect { target: u32 },
}

#[derive(Clone, Debug)]
enum VmUnwindEntry {
    DynamicBinding {
        name: String,
        restored_value: Value,
        specpdl_count: usize,
    },
    LexicalBinding {
        name: String,
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

    fn with_frame_roots<T>(
        &mut self,
        func: &ByteCodeFunction,
        stack: &[Value],
        handlers: &[Handler],
        specpdl: &[VmUnwindEntry],
        extra: &[Value],
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let saved_len = self.ctx.vm_gc_roots.len();
        self.ctx.vm_gc_roots.extend(func.constants.iter().copied());
        self.ctx.vm_gc_roots.extend(stack.iter().copied());
        Self::collect_handler_roots(handlers, &mut self.ctx.vm_gc_roots);
        Self::collect_specpdl_roots(specpdl, &mut self.ctx.vm_gc_roots);
        self.ctx.vm_gc_roots.extend(extra.iter().copied());
        let result = f(self);
        self.ctx.vm_gc_roots.truncate(saved_len);
        result
    }

    fn with_extra_roots<T>(&mut self, extra: &[Value], f: impl FnOnce(&mut Self) -> T) -> T {
        let saved_len = self.ctx.vm_gc_roots.len();
        self.ctx.vm_gc_roots.extend(extra.iter().copied());
        let result = f(self);
        self.ctx.vm_gc_roots.truncate(saved_len);
        result
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

    fn collect_handler_roots(handlers: &[Handler], out: &mut Vec<Value>) {
        for handler in handlers {
            match handler {
                Handler::Catch { tag, .. } => out.push(*tag),
                Handler::ConditionCase { conditions, .. } => out.push(*conditions),
                Handler::UnwindProtect { .. } => {}
            }
        }
    }

    fn collect_specpdl_roots(specpdl: &[VmUnwindEntry], out: &mut Vec<Value>) {
        for entry in specpdl {
            match entry {
                VmUnwindEntry::DynamicBinding { restored_value, .. } => out.push(*restored_value),
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
                out.push(Value::Symbol(sig.symbol));
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
        self.execute_with_func_value(func, args, Value::Nil)
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
                vec![Value::Int(overflow_depth)],
            ));
        }

        let result = self.run_frame(func, args, func_value);
        self.ctx.depth -= 1;
        result
    }

    fn run_frame(
        &mut self,
        func: &ByteCodeFunction,
        args: Vec<Value>,
        func_value: Value,
    ) -> EvalResult {
        let mut stack: Vec<Value> = Vec::with_capacity(func.max_stack as usize);
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
                Value::Int(nonrest as i64)
            };
            let arity = Value::cons(Value::Int(n_required as i64), max_val);
            return Err(signal(
                "wrong-number-of-arguments",
                vec![arity, Value::Int(nargs as i64)],
            ));
        }

        // Push required + optional args (pad with nil for missing optionals)
        for i in 0..nonrest {
            if i < nargs {
                stack.push(args[i]);
            } else {
                stack.push(Value::Nil);
            }
        }

        // If &rest, collect remaining args into a list
        if has_rest {
            let rest_list = if nargs > nonrest {
                Value::list(args[nonrest..].to_vec())
            } else {
                Value::Nil
            };
            stack.push(rest_list);
        }

        // Push a dynamic frame mapping param names → values so that inner
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
                        Value::Nil
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
                        Value::Nil
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
                let result = self.run_loop(func, &mut stack, &mut pc, &mut handlers, &mut specpdl);
                self.ctx.lexenv = saved_lexenv;
                let cleanup = self.unwind_specpdl_all(&mut specpdl);
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
            let result = self.run_loop(func, &mut stack, &mut pc, &mut handlers, &mut specpdl);
            crate::emacs_core::eval::unbind_to_in_state(
                &mut self.ctx.obarray,
                &mut self.ctx.specpdl,
                specpdl_count,
            );
            let cleanup = self.unwind_specpdl_all(&mut specpdl);
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

        let result = self.run_loop(func, &mut stack, &mut pc, &mut handlers, &mut specpdl);

        if let Some(old) = saved_lexenv {
            self.ctx.lexenv = old;
        }
        let cleanup_roots = Self::result_roots(&result);
        let mut cleanup_extra_roots = cleanup_roots.clone();
        Self::collect_specpdl_roots(&specpdl, &mut cleanup_extra_roots);
        let cleanup =
            self.with_frame_roots(func, &stack, &handlers, &[], &cleanup_extra_roots, |vm| {
                vm.unwind_specpdl_all(&mut specpdl)
            });
        merge_result_with_cleanup(result, cleanup)
    }

    fn run_loop(
        &mut self,
        func: &ByteCodeFunction,
        stack: &mut Vec<Value>,
        pc: &mut usize,
        handlers: &mut Vec<Handler>,
        specpdl: &mut Vec<VmUnwindEntry>,
    ) -> EvalResult {
        let ops = &func.ops;
        let constants = &func.constants;

        macro_rules! vm_try {
            ($expr:expr) => {{
                match $expr {
                    Ok(value) => value,
                    Err(flow) => {
                        self.resume_nonlocal(func, stack, pc, handlers, specpdl, flow)?;
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
                    stack.push(constants[*idx as usize]);
                }
                Op::Nil => stack.push(Value::Nil),
                Op::True => stack.push(Value::True),
                Op::Pop => {
                    stack.pop();
                }
                Op::Dup => {
                    if let Some(top) = stack.last() {
                        stack.push(*top);
                    }
                }
                Op::StackRef(n) => {
                    let idx = stack.len().saturating_sub(1 + *n as usize);
                    stack.push(stack[idx]);
                }
                Op::StackSet(n) => {
                    if stack.is_empty() {
                        continue;
                    }
                    let n = *n as usize;
                    let val = stack.pop().unwrap_or(Value::Nil);
                    if n == 0 {
                        continue;
                    }
                    if n <= stack.len() {
                        let idx = stack.len() - n;
                        stack[idx] = val;
                    }
                }
                Op::DiscardN(raw) => {
                    let preserve_tos = (raw & 0x80) != 0;
                    let mut n = (raw & 0x7F) as usize;
                    if n == 0 {
                        continue;
                    }
                    n = n.min(stack.len());
                    if preserve_tos && n < stack.len() {
                        if let Some(top) = stack.last().cloned() {
                            let target = stack.len() - 1 - n;
                            stack[target] = top;
                        }
                    }
                    let new_len = stack.len().saturating_sub(n);
                    stack.truncate(new_len);
                }

                // -- Variable access --
                Op::VarRef(idx) => {
                    let name = sym_name(constants, *idx);
                    let val = vm_try!(self.lookup_var(&name));
                    stack.push(val);
                }
                Op::VarSet(idx) => {
                    let name = sym_name(constants, *idx);
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let extra = [val];
                    vm_try!(
                        self.with_frame_roots(func, stack, handlers, specpdl, &extra, |vm| vm
                            .assign_var(&name, val),)
                    );
                }
                Op::VarBind(idx) => {
                    let name = sym_name(constants, *idx);
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let old_value = self.lookup_var(&name).unwrap_or(Value::Nil);
                    let name_id = intern(&name);
                    let lexical_bind = func.lexical
                        && !self.ctx.obarray.is_constant_id(name_id)
                        && !self.ctx.obarray.is_special_id(name_id)
                        && !crate::emacs_core::value::lexenv_declares_special(
                            self.ctx.lexenv,
                            name_id,
                        );
                    if lexical_bind {
                        let old_lexenv = self.ctx.lexenv;
                        self.ctx.lexenv = lexenv_prepend(self.ctx.lexenv, name_id, val);
                        specpdl.push(VmUnwindEntry::LexicalBinding {
                            name: name.clone(),
                            restored_value: old_value,
                            old_lexenv,
                        });
                    } else {
                        let specpdl_count = self.ctx.specpdl.len();
                        crate::emacs_core::eval::specbind_in_state(
                            &mut self.ctx.obarray,
                            &mut self.ctx.specpdl,
                            name_id,
                            val,
                        );
                        specpdl.push(VmUnwindEntry::DynamicBinding {
                            name: name.clone(),
                            restored_value: old_value,
                            specpdl_count,
                        });
                    }
                    let extra = [val];
                    vm_try!(
                        self.with_frame_roots(func, stack, handlers, specpdl, &extra, |vm| vm
                            .run_variable_watchers(&name, &val, &Value::Nil, "let"),)
                    );
                }
                Op::Unbind(n) => {
                    let mut unwind_roots = Vec::new();
                    Self::collect_specpdl_roots(specpdl, &mut unwind_roots);
                    vm_try!(self.with_frame_roots(
                        func,
                        stack,
                        handlers,
                        &[],
                        &unwind_roots,
                        |vm| vm.unwind_specpdl_n(*n as usize, specpdl),
                    ));
                }

                // -- Function calls --
                Op::Call(n) => {
                    let n = *n as usize;
                    let args_start = stack.len().saturating_sub(n);
                    let args: Vec<Value> = stack.drain(args_start..).collect();
                    let func_val = stack.pop().unwrap_or(Value::Nil);
                    let writeback_names = self.writeback_callable_names(&func_val);
                    let writeback_args = args.clone();
                    let mut call_roots = Vec::with_capacity(args.len() + 1);
                    call_roots.push(func_val);
                    call_roots.extend(args.iter().copied());
                    let result = vm_try!(self.with_frame_roots(
                        func,
                        stack,
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
                            stack,
                        );
                    }
                    stack.push(result);
                }
                Op::Apply(n) => {
                    let n = *n as usize;
                    if n == 0 {
                        let func_val = stack.pop().unwrap_or(Value::Nil);
                        let call_roots = [func_val];
                        let result = vm_try!(self.with_frame_roots(
                            func,
                            stack,
                            handlers,
                            specpdl,
                            &call_roots,
                            |vm| vm.call_function(func_val, vec![]),
                        ));
                        stack.push(result);
                    } else {
                        let args_start = stack.len().saturating_sub(n);
                        let mut args: Vec<Value> = stack.drain(args_start..).collect();
                        let func_val = stack.pop().unwrap_or(Value::Nil);
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
                            stack,
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
                                stack,
                            );
                        }
                        stack.push(result);
                    }
                }

                // -- Control flow --
                Op::Goto(addr) => {
                    *pc = *addr as usize;
                }
                Op::GotoIfNil(addr) => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    if val.is_nil() {
                        *pc = *addr as usize;
                    }
                }
                Op::GotoIfNotNil(addr) => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    if val.is_truthy() {
                        *pc = *addr as usize;
                    }
                }
                Op::GotoIfNilElsePop(addr) => {
                    if stack.last().is_none_or(|v| v.is_nil()) {
                        *pc = *addr as usize;
                    } else {
                        stack.pop();
                    }
                }
                Op::GotoIfNotNilElsePop(addr) => {
                    if stack.last().is_some_and(|v| v.is_truthy()) {
                        *pc = *addr as usize;
                    } else {
                        stack.pop();
                    }
                }
                Op::Switch => {
                    let jump_table = stack.pop().unwrap_or(Value::Nil);
                    let dispatch = stack.pop().unwrap_or(Value::Nil);

                    let table_id = match jump_table {
                        Value::HashTable(table_id) => table_id,
                        other => {
                            self.resume_nonlocal(
                                func,
                                stack,
                                pc,
                                handlers,
                                specpdl,
                                signal(
                                    "wrong-type-argument",
                                    vec![Value::symbol("hash-table-p"), other],
                                ),
                            )?;
                            continue;
                        }
                    };

                    let target = with_heap(|heap| {
                        let table = heap.get_hash_table(table_id);
                        let key = dispatch.to_hash_key(&table.test);
                        table.data.get(&key).copied()
                    });

                    match target {
                        Some(Value::Int(addr)) => {
                            *pc = vm_try!(resolve_switch_target(func, addr));
                        }
                        Some(other) => {
                            vm_try!(Err(signal(
                                "wrong-type-argument",
                                vec![Value::symbol("integerp"), other],
                            )));
                        }
                        None => {}
                    }
                }
                Op::Return => {
                    return Ok(stack.pop().unwrap_or(Value::Nil));
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
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "+",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(vm_try!(arith_add(self, &call_args[0], &call_args[1])));
                    }
                }
                Op::Sub => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "-",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(vm_try!(arith_sub(self, &call_args[0], &call_args[1])));
                    }
                }
                Op::Mul => {
                    let b = stack.pop().unwrap_or(Value::Int(1));
                    let a = stack.pop().unwrap_or(Value::Int(1));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "*",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(vm_try!(arith_mul(self, &call_args[0], &call_args[1])));
                    }
                }
                Op::Div => {
                    let b = stack.pop().unwrap_or(Value::Int(1));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "/",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(vm_try!(arith_div(self, &call_args[0], &call_args[1])));
                    }
                }
                Op::Rem => {
                    let b = stack.pop().unwrap_or(Value::Int(1));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "%",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(vm_try!(arith_rem(&call_args[0], &call_args[1])));
                    }
                }
                Op::Add1 => {
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    let call_args = vec![a];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "1+",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(vm_try!(arith_add1(self, &call_args[0])));
                    }
                }
                Op::Sub1 => {
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    let call_args = vec![a];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "1-",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(vm_try!(arith_sub1(self, &call_args[0])));
                    }
                }
                Op::Negate => {
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    let call_args = vec![a];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "-",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(vm_try!(arith_negate(self, &call_args[0])));
                    }
                }

                // -- Comparison --
                Op::Eqlsign => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "=",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(Value::bool(vm_try!(num_eq(
                            self,
                            &call_args[0],
                            &call_args[1],
                        ))));
                    }
                }
                Op::Gtr => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        ">",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(Value::bool(
                            vm_try!(num_cmp(self, &call_args[0], &call_args[1],)) > 0,
                        ));
                    }
                }
                Op::Lss => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "<",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(Value::bool(
                            vm_try!(num_cmp(self, &call_args[0], &call_args[1],)) < 0,
                        ));
                    }
                }
                Op::Leq => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "<=",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(Value::bool(
                            vm_try!(num_cmp(self, &call_args[0], &call_args[1],)) <= 0,
                        ));
                    }
                }
                Op::Geq => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        ">=",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(Value::bool(
                            vm_try!(num_cmp(self, &call_args[0], &call_args[1],)) >= 0,
                        ));
                    }
                }
                Op::Max => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "max",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(
                            if vm_try!(num_cmp(self, &call_args[0], &call_args[1])) >= 0 {
                                call_args[0]
                            } else {
                                call_args[1]
                            },
                        );
                    }
                }
                Op::Min => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "min",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(
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
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![val];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "car",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin("car", call_args))
                    };
                    stack.push(result);
                }
                Op::Cdr => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![val];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "cdr",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin("cdr", call_args))
                    };
                    stack.push(result);
                }
                Op::CarSafe => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![val];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "car-safe",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        match call_args[0] {
                            Value::Cons(cell) => {
                                let pair = read_cons(cell);
                                stack.push(pair.car);
                            }
                            // Closures are cons lists in official Emacs.
                            Value::Lambda(_) => {
                                let data = call_args[0].get_lambda_data().unwrap();
                                stack.push(if data.env.is_some() {
                                    Value::symbol("closure")
                                } else {
                                    Value::symbol("lambda")
                                });
                            }
                            _ => stack.push(Value::Nil),
                        }
                    }
                }
                Op::CdrSafe => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![val];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "cdr-safe",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        match call_args[0] {
                            Value::Cons(cell) => {
                                let pair = read_cons(cell);
                                stack.push(pair.cdr);
                            }
                            // Closures are cons lists in official Emacs.
                            Value::Lambda(_) => {
                                use crate::emacs_core::builtins::lambda_to_cons_list;
                                let list = lambda_to_cons_list(&call_args[0]).unwrap_or(Value::Nil);
                                match list {
                                    Value::Cons(cell) => {
                                        stack.push(with_heap(|h| h.cons_cdr(cell)));
                                    }
                                    _ => stack.push(Value::Nil),
                                }
                            }
                            _ => stack.push(Value::Nil),
                        }
                    }
                }
                Op::Cons => {
                    let cdr_val = stack.pop().unwrap_or(Value::Nil);
                    let car_val = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![car_val, cdr_val];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "cons",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(Value::cons(call_args[0], call_args[1]));
                    }
                }
                Op::List(n) => {
                    let n = *n as usize;
                    let start = stack.len().saturating_sub(n);
                    let items: Vec<Value> = stack.drain(start..).collect();
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "list",
                        items.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(Value::list(items));
                    }
                }
                Op::Length => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![val];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "length",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(vm_try!(length_value(&call_args[0])));
                    }
                }
                Op::Nth => {
                    let list = stack.pop().unwrap_or(Value::Nil);
                    let n = stack.pop().unwrap_or(Value::Int(0));
                    let call_args = vec![n, list];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "nth",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin("nth", call_args))
                    };
                    stack.push(result);
                }
                Op::Nthcdr => {
                    let list = stack.pop().unwrap_or(Value::Nil);
                    let n = stack.pop().unwrap_or(Value::Int(0));
                    let call_args = vec![n, list];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "nthcdr",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin("nthcdr", call_args))
                    };
                    stack.push(result);
                }
                Op::Elt => {
                    let idx = stack.pop().unwrap_or(Value::Nil);
                    let seq = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![seq, idx];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "elt",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin("elt", call_args))
                    };
                    stack.push(result);
                }
                Op::Setcar => {
                    let newcar = stack.pop().unwrap_or(Value::Nil);
                    let cell = stack.pop().unwrap_or(Value::Nil);
                    if let Value::Cons(c) = &cell {
                        with_heap_mut(|h| h.set_car(*c, newcar));
                        stack.push(newcar);
                    } else {
                        vm_try!(Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("consp"), cell],
                        )));
                    }
                }
                Op::Setcdr => {
                    let newcdr = stack.pop().unwrap_or(Value::Nil);
                    let cell = stack.pop().unwrap_or(Value::Nil);
                    if let Value::Cons(c) = &cell {
                        with_heap_mut(|h| h.set_cdr(*c, newcdr));
                        stack.push(newcdr);
                    } else {
                        vm_try!(Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("consp"), cell],
                        )));
                    }
                }
                Op::Nconc => {
                    let b = stack.pop().unwrap_or(Value::Nil);
                    let a = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![a, b];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "nconc",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin("nconc", call_args))
                    };
                    stack.push(result);
                }
                Op::Nreverse => {
                    let list = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![list];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "nreverse",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin("nreverse", call_args))
                    };
                    stack.push(result);
                }
                Op::Member => {
                    let list = stack.pop().unwrap_or(Value::Nil);
                    let elt = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![elt, list];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "member",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin("member", call_args))
                    };
                    stack.push(result);
                }
                Op::Memq => {
                    let list = stack.pop().unwrap_or(Value::Nil);
                    let elt = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![elt, list];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "memq",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin("memq", call_args))
                    };
                    stack.push(result);
                }
                Op::Assq => {
                    let alist = stack.pop().unwrap_or(Value::Nil);
                    let key = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![key, alist];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "assq",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin("assq", call_args))
                    };
                    stack.push(result);
                }

                // -- Type predicates --
                Op::Symbolp => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![val];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "symbolp",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(Value::bool(call_args[0].is_symbol()));
                    }
                }
                Op::Consp => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![val];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "consp",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(Value::bool(call_args[0].is_cons()));
                    }
                }
                Op::Stringp => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![val];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "stringp",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(Value::bool(call_args[0].is_string()));
                    }
                }
                Op::Listp => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![val];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "listp",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(Value::bool(call_args[0].is_list()));
                    }
                }
                Op::Integerp => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![val];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "integerp",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(Value::bool(call_args[0].is_integer()));
                    }
                }
                Op::Numberp => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![val];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "numberp",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(Value::bool(call_args[0].is_number()));
                    }
                }
                Op::Null | Op::Not => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let opname = if matches!(op, Op::Null) {
                        "null"
                    } else {
                        "not"
                    };
                    let call_args = vec![val];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        opname,
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(Value::bool(call_args[0].is_nil()));
                    }
                }
                Op::Eq => {
                    let b = stack.pop().unwrap_or(Value::Nil);
                    let a = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "eq",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(Value::bool(eq_value(&call_args[0], &call_args[1])));
                    }
                }
                Op::Equal => {
                    let b = stack.pop().unwrap_or(Value::Nil);
                    let a = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![a, b];
                    if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "equal",
                        call_args.clone(),
                    )) {
                        stack.push(result);
                    } else {
                        stack.push(Value::bool(equal_value(&call_args[0], &call_args[1], 0)));
                    }
                }

                // -- String operations --
                Op::Concat(n) => {
                    let n = *n as usize;
                    let start = stack.len().saturating_sub(n);
                    let parts: Vec<Value> = stack.drain(start..).collect();
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "concat",
                        parts.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin("concat", parts))
                    };
                    stack.push(result);
                }
                Op::Substring => {
                    let to = stack.pop().unwrap_or(Value::Nil);
                    let from = stack.pop().unwrap_or(Value::Int(0));
                    let array = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![array, from, to];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "substring",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(substring_value(&call_args[0], &call_args[1], &call_args[2]))
                    };
                    stack.push(result);
                }
                Op::StringEqual => {
                    let b = stack.pop().unwrap_or(Value::Nil);
                    let a = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![a, b];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "string=",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin("string=", call_args))
                    };
                    stack.push(result);
                }
                Op::StringLessp => {
                    let b = stack.pop().unwrap_or(Value::Nil);
                    let a = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![a, b];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "string-lessp",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin("string-lessp", call_args))
                    };
                    stack.push(result);
                }

                // -- Vector operations --
                Op::Aref => {
                    let idx_val = stack.pop().unwrap_or(Value::Int(0));
                    let vec_val = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![vec_val, idx_val];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "aref",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(builtins::builtin_aref(call_args))
                    };
                    stack.push(result);
                }
                Op::Aset => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let idx_val = stack.pop().unwrap_or(Value::Int(0));
                    let vec_val = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![vec_val, idx_val, val];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
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
                        "aset", None, &call_args, &result, stack,
                    );
                    stack.push(result);
                }

                // -- Symbol operations --
                Op::SymbolValue => {
                    let sym = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![sym];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "symbol-value",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin("symbol-value", call_args))
                    };
                    stack.push(result);
                }
                Op::SymbolFunction => {
                    let sym = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![sym];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "symbol-function",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin("symbol-function", call_args))
                    };
                    stack.push(result);
                }
                Op::Set => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let sym = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![sym, val];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "set",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin("set", call_args))
                    };
                    stack.push(result);
                }
                Op::Fset => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let sym = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![sym, val];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "fset",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin("fset", call_args))
                    };
                    stack.push(result);
                }
                Op::Get => {
                    let prop = stack.pop().unwrap_or(Value::Nil);
                    let sym = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![sym, prop];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "get",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin("get", call_args))
                    };
                    stack.push(result);
                }
                Op::Put => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let prop = stack.pop().unwrap_or(Value::Nil);
                    let sym = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![sym, prop, val];
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        "put",
                        call_args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin("put", call_args))
                    };
                    stack.push(result);
                }

                // -- Error handling --
                Op::PushConditionCase(target) => {
                    handlers.push(Handler::ConditionCase {
                        conditions: Value::symbol("error"),
                        target: *target,
                        stack_len: stack.len(),
                        spec_depth: specpdl.len(),
                    });
                }
                Op::PushConditionCaseRaw(target) => {
                    // GNU bytecode consumes the handler pattern operand from TOS.
                    let conditions = stack.pop().unwrap_or(Value::Nil);
                    handlers.push(Handler::ConditionCase {
                        conditions,
                        target: *target,
                        stack_len: stack.len(),
                        spec_depth: specpdl.len(),
                    });
                }
                Op::PushCatch(target) => {
                    let tag = stack.pop().unwrap_or(Value::Nil);
                    handlers.push(Handler::Catch {
                        tag,
                        target: *target,
                        stack_len: stack.len(),
                        spec_depth: specpdl.len(),
                    });
                    // Register in evaluator so sf_throw / nested VM throws can
                    // see this catch tag when deciding throw vs no-catch.
                    self.ctx.catch_tags.push(tag);
                }
                Op::PopHandler => {
                    if let Some(handler) = handlers.pop() {
                        match handler {
                            Handler::Catch { .. } => {
                                // Remove from evaluator's catch_tags registry.
                                self.ctx.catch_tags.pop();
                            }
                            _ => {}
                        }
                    }
                }
                Op::UnwindProtect(target) => {
                    handlers.push(Handler::UnwindProtect { target: *target });
                }
                Op::UnwindProtectPop => {
                    let cleanup = stack.pop().unwrap_or(Value::Nil);
                    specpdl.push(VmUnwindEntry::Cleanup { cleanup });
                }
                Op::Throw => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let tag = stack.pop().unwrap_or(Value::Nil);
                    self.resume_nonlocal(
                        func,
                        stack,
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
                        stack.push(Value::make_bytecode(closure));
                    } else {
                        stack.push(val);
                    }
                }

                // -- Builtin escape hatch --
                Op::CallBuiltin(name_idx, n) => {
                    let name = sym_name(constants, *name_idx);
                    let n = *n as usize;
                    let args_start = stack.len().saturating_sub(n);
                    let args: Vec<Value> = stack.drain(args_start..).collect();
                    let writeback_args = args.clone();
                    let result = if let Some(result) = vm_try!(self.maybe_call_named_function_cell(
                        func,
                        stack,
                        handlers,
                        specpdl,
                        &name,
                        args.clone(),
                    )) {
                        result
                    } else {
                        vm_try!(self.dispatch_vm_builtin(&name, args))
                    };
                    self.maybe_writeback_mutating_first_arg(
                        &name,
                        None,
                        &writeback_args,
                        &result,
                        stack,
                    );
                    stack.push(result);
                }
            }
        }

        // Fell off the end — return TOS or nil
        Ok(stack.pop().unwrap_or(Value::Nil))
    }

    // -- Helper methods --

    fn writeback_callable_names(&self, func_val: &Value) -> Option<(String, Option<String>)> {
        match func_val {
            Value::Subr(id) => Some((resolve_sym(*id).to_owned(), None)),
            Value::Symbol(id) => {
                let name = resolve_sym(*id);
                let alias_target =
                    self.ctx
                        .obarray
                        .symbol_function(name)
                        .and_then(|bound| match bound {
                            Value::Symbol(tid) => Some(resolve_sym(*tid).to_owned()),
                            Value::Subr(tid) => Some(resolve_sym(*tid).to_owned()),
                            _ => None,
                        });
                Some((name.to_owned(), alias_target))
            }
            _ => None,
        }
    }

    fn named_builtin_fast_path_allowed(&self, name: &str) -> bool {
        match self.ctx.obarray.symbol_function(name) {
            Some(Value::Subr(id)) => resolve_sym(*id) == name,
            Some(Value::Nil) | None => true,
            _ => false,
        }
    }

    fn maybe_call_named_function_cell(
        &mut self,
        func: &ByteCodeFunction,
        stack: &[Value],
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
        self.with_frame_roots(func, stack, handlers, specpdl, &call_roots, |vm| {
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
        stack: &mut Vec<Value>,
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
        for value in stack.iter_mut() {
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
            for value in buf.properties.values_mut() {
                if let RuntimeBindingValue::Bound(value) = value {
                    Self::replace_alias_refs_in_value(value, first_arg, &replacement, &mut visited);
                }
            }
        }

        let symbols: Vec<String> = self
            .ctx
            .obarray
            .all_symbols()
            .into_iter()
            .map(str::to_string)
            .collect();
        for name in symbols {
            if let Some(symbol) = self.ctx.obarray.get_mut(&name) {
                match &mut symbol.value {
                    crate::emacs_core::symbol::SymbolValue::Plain(Some(value)) => {
                        Self::replace_alias_refs_in_value(
                            value,
                            first_arg,
                            &replacement,
                            &mut visited,
                        );
                    }
                    crate::emacs_core::symbol::SymbolValue::BufferLocal {
                        default: Some(value),
                        ..
                    } => {
                        Self::replace_alias_refs_in_value(
                            value,
                            first_arg,
                            &replacement,
                            &mut visited,
                        );
                    }
                    _ => {}
                }
            }
        }
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

        match value {
            Value::Cons(cell) => {
                let key = (cell.index as usize) ^ 0x1;
                if !visited.insert(key) {
                    return;
                }
                let pair = read_cons(*cell);
                let mut new_car = pair.car;
                let mut new_cdr = pair.cdr;
                Self::replace_alias_refs_in_value(&mut new_car, from, to, visited);
                Self::replace_alias_refs_in_value(&mut new_cdr, from, to, visited);
                with_heap_mut(|h| {
                    h.set_car(*cell, new_car);
                    h.set_cdr(*cell, new_cdr);
                });
            }
            Value::Vector(items) => {
                let key = (items.index as usize) ^ 0x2;
                if !visited.insert(key) {
                    return;
                }
                let mut values = with_heap(|h| h.get_vector(*items).clone());
                for item in values.iter_mut() {
                    Self::replace_alias_refs_in_value(item, from, to, visited);
                }
                with_heap_mut(|h| *h.get_vector_mut(*items) = values);
            }
            Value::HashTable(table) => {
                let key = (table.index as usize) ^ 0x4;
                if !visited.insert(key) {
                    return;
                }
                let mut ht = with_heap(|h| h.get_hash_table(*table).clone());
                let old_ptr = match from {
                    Value::Str(value) => Some(value.index as usize),
                    _ => None,
                };
                let new_ptr = match to {
                    Value::Str(value) => Some(value.index as usize),
                    _ => None,
                };
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
                with_heap_mut(|h| *h.get_hash_table_mut(*table) = ht);
            }
            _ => {}
        }
    }

    fn lookup_var(&self, name: &str) -> EvalResult {
        if name.starts_with(':') {
            return Ok(Value::Keyword(intern(name)));
        }

        let name_id = intern(name);
        let resolved = crate::emacs_core::builtins::symbols::resolve_variable_alias_id_in_obarray(
            &self.ctx.obarray,
            name_id,
        )?;
        let resolved_name = resolve_sym(resolved);
        let is_special =
            self.ctx.obarray.is_special_id(name_id) && !self.ctx.obarray.is_constant_id(name_id);
        let resolved_is_special =
            self.ctx.obarray.is_special_id(resolved) && !self.ctx.obarray.is_constant_id(resolved);
        let locally_special =
            crate::emacs_core::value::lexenv_declares_special(self.ctx.lexenv, name_id)
                || (resolved != name_id
                    && crate::emacs_core::value::lexenv_declares_special(
                        self.ctx.lexenv,
                        resolved,
                    ));

        // GNU Emacs resolves declared-special vars dynamically even when
        // lexical binding is active; the interpreter path already does this.
        if !is_special && !resolved_is_special && !locally_special {
            if let Some(val) = lexenv_lookup(self.ctx.lexenv, name_id) {
                return Ok(val);
            }
            if resolved != name_id
                && let Some(val) = lexenv_lookup(self.ctx.lexenv, resolved)
            {
                return Ok(val);
            }
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
            return Ok(Value::Nil);
        }
        if name == "t" {
            return Ok(Value::True);
        }
        if resolved_name == "nil" {
            return Ok(Value::Nil);
        }
        if resolved_name == "t" {
            return Ok(Value::True);
        }
        if resolved_name.starts_with(':') {
            return Ok(Value::Keyword(resolved));
        }

        Err(signal("void-variable", vec![Value::symbol(name)]))
    }

    fn assign_var(&mut self, name: &str, value: Value) -> Result<(), Flow> {
        let name_id = intern(name);
        let resolved = crate::emacs_core::builtins::symbols::resolve_variable_alias_id_in_obarray(
            &self.ctx.obarray,
            name_id,
        )?;
        let is_special =
            self.ctx.obarray.is_special_id(name_id) && !self.ctx.obarray.is_constant_id(name_id);
        let resolved_is_special =
            self.ctx.obarray.is_special_id(resolved) && !self.ctx.obarray.is_constant_id(resolved);
        let locally_special =
            crate::emacs_core::value::lexenv_declares_special(self.ctx.lexenv, name_id)
                || (resolved != name_id
                    && crate::emacs_core::value::lexenv_declares_special(
                        self.ctx.lexenv,
                        resolved,
                    ));

        if !is_special && !resolved_is_special && !locally_special {
            if let Some(cell_id) = lexenv_assq(self.ctx.lexenv, name_id) {
                lexenv_set(cell_id, value);
                return Ok(());
            }
            if resolved != name_id
                && let Some(cell_id) = lexenv_assq(self.ctx.lexenv, resolved)
            {
                lexenv_set(cell_id, value);
                return Ok(());
            }
        }

        // specbind writes directly to obarray, so dynamic stack mutation
        // is no longer needed — fall through to obarray write.

        if self.ctx.obarray.is_constant_id(resolved) {
            return Err(signal("setting-constant", vec![Value::symbol(name)]));
        }

        crate::emacs_core::eval::set_runtime_binding_in_state(
            &mut self.ctx.obarray,
            &mut self.ctx.buffers,
            &self.ctx.custom,
            self.ctx.specpdl.as_slice(),
            resolved,
            value,
        );
        self.run_variable_watchers(resolve_sym(resolved), &value, &Value::Nil, "set")
    }

    fn run_variable_watchers(
        &mut self,
        name: &str,
        new_value: &Value,
        old_value: &Value,
        operation: &str,
    ) -> Result<(), Flow> {
        self.run_variable_watchers_with_where(name, new_value, old_value, operation, &Value::Nil)
    }

    fn run_variable_watchers_with_where(
        &mut self,
        name: &str,
        new_value: &Value,
        old_value: &Value,
        operation: &str,
        where_value: &Value,
    ) -> Result<(), Flow> {
        if !self.ctx.watchers.has_watchers(name) {
            return Ok(());
        }
        let calls =
            self.ctx
                .watchers
                .notify_watchers(name, new_value, old_value, operation, where_value);
        for (callback, args) in calls {
            let _ = self.call_function_with_roots(callback, &args)?;
        }
        Ok(())
    }

    fn call_function_with_roots(&mut self, function: Value, args: &[Value]) -> EvalResult {
        let mut roots = Vec::with_capacity(args.len() + 1);
        roots.push(function);
        roots.extend(args.iter().copied());
        self.with_extra_roots(&roots, |vm| vm.call_function(function, args.to_vec()))
    }

    fn run_hook_functions(&mut self, functions: &[Value], args: &[Value]) -> Result<(), Flow> {
        for function in functions {
            let _ = self.call_function_with_roots(*function, args)?;
        }
        Ok(())
    }

    fn builtin_run_hooks_shared(&mut self, args: &[Value]) -> EvalResult {
        for hook_sym in args {
            let hook_name = hook_sym.as_symbol_name().ok_or_else(|| {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("symbolp"), *hook_sym],
                )
            })?;
            let hook_value =
                crate::emacs_core::builtins::symbol_dynamic_buffer_or_global_value_in_state(
                    &self.ctx.obarray,
                    &[],
                    &self.ctx.buffers,
                    hook_name,
                )
                .unwrap_or(Value::Nil);
            let functions = crate::emacs_core::builtins::collect_hook_functions_in_state(
                &self.ctx.obarray,
                hook_name,
                hook_value,
                true,
            );
            self.run_hook_functions(&functions, &[])?;
        }
        Ok(Value::Nil)
    }

    fn builtin_run_hook_with_args_shared(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_min_args("run-hook-with-args", args, 1)?;
        let hook_name = args[0].as_symbol_name().ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[0]],
            )
        })?;
        let hook_value =
            crate::emacs_core::builtins::symbol_dynamic_buffer_or_global_value_in_state(
                &self.ctx.obarray,
                &[],
                &self.ctx.buffers,
                hook_name,
            )
            .unwrap_or(Value::Nil);
        let functions = crate::emacs_core::builtins::collect_hook_functions_in_state(
            &self.ctx.obarray,
            hook_name,
            hook_value,
            true,
        );
        self.run_hook_functions(&functions, &args[1..])?;
        Ok(Value::Nil)
    }

    fn builtin_run_hook_with_args_until_success_shared(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_min_args("run-hook-with-args-until-success", args, 1)?;
        let hook_name = args[0].as_symbol_name().ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[0]],
            )
        })?;
        let hook_value =
            crate::emacs_core::builtins::symbol_dynamic_buffer_or_global_value_in_state(
                &self.ctx.obarray,
                &[],
                &self.ctx.buffers,
                hook_name,
            )
            .unwrap_or(Value::Nil);
        let functions = crate::emacs_core::builtins::collect_hook_functions_in_state(
            &self.ctx.obarray,
            hook_name,
            hook_value,
            true,
        );
        for function in functions {
            let value = self.call_function_with_roots(function, &args[1..])?;
            if value.is_truthy() {
                return Ok(value);
            }
        }
        Ok(Value::Nil)
    }

    fn builtin_run_hook_with_args_until_failure_shared(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_min_args("run-hook-with-args-until-failure", args, 1)?;
        let hook_name = args[0].as_symbol_name().ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[0]],
            )
        })?;
        let hook_value =
            crate::emacs_core::builtins::symbol_dynamic_buffer_or_global_value_in_state(
                &self.ctx.obarray,
                &[],
                &self.ctx.buffers,
                hook_name,
            )
            .unwrap_or(Value::Nil);
        let functions = crate::emacs_core::builtins::collect_hook_functions_in_state(
            &self.ctx.obarray,
            hook_name,
            hook_value,
            true,
        );
        for function in functions {
            let value = self.call_function_with_roots(function, &args[1..])?;
            if value.is_nil() {
                return Ok(Value::Nil);
            }
        }
        Ok(Value::True)
    }

    fn builtin_run_hook_wrapped_shared(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_min_args("run-hook-wrapped", args, 2)?;
        let hook_name = args[0].as_symbol_name().ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[0]],
            )
        })?;
        let wrapper = args[1];
        let hook_value =
            crate::emacs_core::builtins::symbol_dynamic_buffer_or_global_value_in_state(
                &self.ctx.obarray,
                &[],
                &self.ctx.buffers,
                hook_name,
            )
            .unwrap_or(Value::Nil);
        let functions = crate::emacs_core::builtins::collect_hook_functions_in_state(
            &self.ctx.obarray,
            hook_name,
            hook_value,
            true,
        );
        for function in functions {
            let mut call_args = Vec::with_capacity(args.len() - 1);
            call_args.push(function);
            call_args.extend(args[2..].iter().copied());
            let _ = self.call_function_with_roots(wrapper, &call_args)?;
        }
        Ok(Value::Nil)
    }

    fn builtin_run_hook_query_error_with_timeout_shared(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("run-hook-query-error-with-timeout", args, 1)?;
        let hook_name = args[0].as_symbol_name().ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[0]],
            )
        })?;
        let hook_value =
            crate::emacs_core::builtins::symbol_dynamic_buffer_or_global_value_in_state(
                &self.ctx.obarray,
                &[],
                &self.ctx.buffers,
                hook_name,
            )
            .unwrap_or(Value::Nil);
        let functions = crate::emacs_core::builtins::collect_hook_functions_in_state(
            &self.ctx.obarray,
            hook_name,
            hook_value,
            true,
        );
        match self.run_hook_functions(&functions, &[]) {
            Ok(()) => Ok(Value::Nil),
            Err(Flow::Signal(_)) => Err(signal(
                "end-of-file",
                vec![Value::string("Error reading from stdin")],
            )),
            Err(flow) => Err(flow),
        }
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
        let where_value = crate::emacs_core::eval::set_runtime_binding_in_state(
            &mut self.ctx.obarray,
            &mut self.ctx.buffers,
            &self.ctx.custom,
            self.ctx.specpdl.as_slice(),
            resolved,
            value,
        )
        .map(Value::Buffer)
        .unwrap_or(Value::Nil);
        self.run_variable_watchers_with_where(
            resolve_sym(resolved),
            &value,
            &Value::Nil,
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
                vec![Value::symbol("set-default"), Value::Int(args.len() as i64)],
            ));
        }
        let symbol = match args[0] {
            Value::Nil => intern("nil"),
            Value::True => intern("t"),
            Value::Symbol(id) | Value::Keyword(id) => id,
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
            crate::emacs_core::eval::set_runtime_binding_in_state(
                &mut self.ctx.obarray,
                &mut self.ctx.buffers,
                &self.ctx.custom,
                self.ctx.specpdl.as_slice(),
                resolved,
                value,
            );
        } else {
            self.ctx.obarray.set_symbol_value_id(resolved, value);
        }

        // Fire watchers AFTER the write.
        self.run_variable_watchers(resolved_name, &value, &Value::Nil, "set")?;
        Ok(value)
    }

    fn builtin_set_default_toplevel_value_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::symbols::builtin_set_default_toplevel_value_in_obarray(
            &mut self.ctx.obarray,
            args.to_vec(),
        )?;
        let symbol = crate::emacs_core::builtins::symbols::expect_symbol_id(&args[0])?;
        let resolved = crate::emacs_core::builtins::symbols::resolve_variable_alias_id_in_obarray(
            &self.ctx.obarray,
            symbol,
        )?;
        let resolved_name = resolve_sym(resolved);
        let value = args[1];
        self.run_variable_watchers(resolved_name, &value, &Value::Nil, "set")?;
        if resolved != symbol {
            self.run_variable_watchers(resolved_name, &value, &Value::Nil, "set")?;
        }
        Ok(Value::Nil)
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
            crate::emacs_core::builtins::symbols::builtin_put_in_obarray(
                &mut self.ctx.obarray,
                vec![result, Value::symbol("function-documentation"), docstring],
            )?;
        }
        Ok(result)
    }

    fn builtin_defvaralias_shared(&mut self, args: &[Value]) -> EvalResult {
        let state_change = crate::emacs_core::builtins::symbols::builtin_defvaralias_in_state(
            &mut self.ctx.obarray,
            args.to_vec(),
        )?;
        self.run_variable_watchers(
            &state_change.previous_target,
            &state_change.base_variable,
            &Value::Nil,
            "defvaralias",
        )?;
        self.ctx.watchers.clear_watchers(&state_change.alias_name);
        crate::emacs_core::builtins::symbols::builtin_put_in_obarray(
            &mut self.ctx.obarray,
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
        self.run_variable_watchers(
            resolve_sym(resolved),
            &Value::Nil,
            &Value::Nil,
            "makunbound",
        )?;
        Ok(args[0])
    }

    fn builtin_make_local_variable_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::custom::builtin_make_local_variable_in_state(
            &self.ctx.obarray,
            &[],
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_local_variable_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::custom::builtin_local_variable_p_in_state(
            &self.ctx.obarray,
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_buffer_local_variables_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::custom::builtin_buffer_local_variables_in_state(
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_kill_local_variable_shared(&mut self, args: &[Value]) -> EvalResult {
        let outcome = crate::emacs_core::custom::builtin_kill_local_variable_in_state(
            &self.ctx.obarray,
            &mut self.ctx.buffers,
            args.to_vec(),
        )?;
        if outcome.removed
            && let Some(buffer_id) = outcome.buffer_id
        {
            self.run_variable_watchers_with_where(
                &outcome.resolved_name,
                &Value::Nil,
                &Value::Nil,
                "makunbound",
                &Value::Buffer(buffer_id),
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
        match arg {
            None | Some(Value::Nil) => Ok(self.ensure_selected_frame_id()),
            Some(Value::Int(n)) => {
                let fid = FrameId(*n as u64);
                if self.ctx.frames.get(fid).is_some() {
                    Ok(fid)
                } else {
                    Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol(predicate), Value::Int(*n)],
                    ))
                }
            }
            Some(Value::Frame(id)) => {
                let fid = FrameId(*id);
                if self.ctx.frames.get(fid).is_some() {
                    Ok(fid)
                } else {
                    Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol(predicate), Value::Frame(*id)],
                    ))
                }
            }
            Some(other) => Err(signal(
                "wrong-type-argument",
                vec![Value::symbol(predicate), *other],
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
        let saved_roots = self.ctx.vm_gc_roots.len();
        self.ctx.vm_gc_roots.push(func);
        self.ctx.vm_gc_roots.push(sequence);

        let mut results = Vec::new();
        let map_result = crate::emacs_core::builtins::higher_order::for_each_sequence_element(
            &sequence,
            |item| {
                let value =
                    self.with_extra_roots(&[item], |vm| vm.call_function(func, vec![item]))?;
                results.push(value);
                self.ctx.vm_gc_roots.push(value);
                Ok(())
            },
        );

        let out = match map_result {
            Ok(()) => self.with_extra_roots(&results, |_| Ok(Value::list(results.clone()))),
            Err(flow) => Err(flow),
        };
        self.ctx.vm_gc_roots.truncate(saved_roots);
        out
    }

    fn builtin_mapc_fast(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("mapc", args, 2)?;
        let func = args[0];
        let sequence = args[1];
        let saved_roots = self.ctx.vm_gc_roots.len();
        self.ctx.vm_gc_roots.push(func);
        self.ctx.vm_gc_roots.push(sequence);

        let map_result = crate::emacs_core::builtins::higher_order::for_each_sequence_element(
            &sequence,
            |item| {
                self.with_extra_roots(&[item], |vm| vm.call_function(func, vec![item]))?;
                Ok(())
            },
        );

        self.ctx.vm_gc_roots.truncate(saved_roots);
        map_result?;
        Ok(sequence)
    }

    fn builtin_mapcan_fast(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("mapcan", args, 2)?;
        let func = args[0];
        let sequence = args[1];
        let saved_roots = self.ctx.vm_gc_roots.len();
        self.ctx.vm_gc_roots.push(func);
        self.ctx.vm_gc_roots.push(sequence);

        let mut mapped = Vec::new();
        let map_result = crate::emacs_core::builtins::higher_order::for_each_sequence_element(
            &sequence,
            |item| {
                let value =
                    self.with_extra_roots(&[item], |vm| vm.call_function(func, vec![item]))?;
                mapped.push(value);
                self.ctx.vm_gc_roots.push(value);
                Ok(())
            },
        );

        let out = match map_result {
            Ok(()) => self.with_extra_roots(&mapped, |_| {
                crate::emacs_core::builtins::builtin_nconc(mapped.clone())
            }),
            Err(flow) => Err(flow),
        };
        self.ctx.vm_gc_roots.truncate(saved_roots);
        out
    }

    fn builtin_mapconcat_fast(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_range_args("mapconcat", args, 2, 3)?;
        let func = args[0];
        let sequence = args[1];
        let separator = args.get(2).copied().unwrap_or_else(|| Value::string(""));
        let saved_roots = self.ctx.vm_gc_roots.len();
        self.ctx.vm_gc_roots.push(func);
        self.ctx.vm_gc_roots.push(sequence);
        self.ctx.vm_gc_roots.push(separator);

        let mut parts = Vec::new();
        let map_result = crate::emacs_core::builtins::higher_order::for_each_sequence_element(
            &sequence,
            |item| {
                let value =
                    self.with_extra_roots(&[item], |vm| vm.call_function(func, vec![item]))?;
                parts.push(value);
                self.ctx.vm_gc_roots.push(value);
                Ok(())
            },
        );

        let out = match map_result {
            Ok(()) if parts.is_empty() => Ok(Value::string("")),
            Ok(()) => {
                let mut concat_args = Vec::with_capacity(parts.len() * 2 - 1);
                for (index, part) in parts.iter().copied().enumerate() {
                    if index > 0 {
                        concat_args.push(separator);
                    }
                    concat_args.push(part);
                }
                self.with_extra_roots(&concat_args, |_| {
                    crate::emacs_core::builtins::builtin_concat(concat_args.clone())
                })
            }
            Err(flow) => Err(flow),
        };
        self.ctx.vm_gc_roots.truncate(saved_roots);
        out
    }

    fn builtin_sort_fast(&mut self, args: &[Value]) -> EvalResult {
        let options = crate::emacs_core::builtins::higher_order::parse_sort_options(args)?;
        let sequence = args[0];
        let saved_roots = self.ctx.vm_gc_roots.len();
        self.ctx.vm_gc_roots.push(sequence);
        self.ctx.vm_gc_roots.push(options.key_fn);
        self.ctx.vm_gc_roots.push(options.lessp_fn);

        let out = match sequence {
            Value::Nil => Ok(Value::Nil),
            Value::Cons(_) => {
                let mut cons_cells = Vec::new();
                let mut values = Vec::new();
                let mut cursor = sequence;
                loop {
                    match cursor {
                        Value::Nil => break,
                        Value::Cons(cell) => {
                            values.push(with_heap(|h| h.cons_car(cell)));
                            cons_cells.push(cell);
                            cursor = with_heap(|h| h.cons_cdr(cell));
                        }
                        tail => {
                            return Err(signal(
                                "wrong-type-argument",
                                vec![Value::symbol("listp"), tail],
                            ));
                        }
                    }
                }
                for value in &values {
                    self.ctx.vm_gc_roots.push(*value);
                }
                let mut sorted_values =
                    crate::emacs_core::builtins::higher_order::stable_sort_values_with(
                        self,
                        &values,
                        options.key_fn,
                        options.lessp_fn,
                        options.reverse,
                    )?;
                if options.in_place {
                    for (cell, value) in cons_cells.iter().zip(sorted_values.into_iter()) {
                        with_heap_mut(|h| h.set_car(*cell, value));
                    }
                    Ok(sequence)
                } else {
                    Ok(Value::list(std::mem::take(&mut sorted_values)))
                }
            }
            Value::Vector(v) | Value::Record(v) => {
                let values = with_heap(|h| h.get_vector(v).clone());
                for value in &values {
                    self.ctx.vm_gc_roots.push(*value);
                }
                let sorted_values =
                    crate::emacs_core::builtins::higher_order::stable_sort_values_with(
                        self,
                        &values,
                        options.key_fn,
                        options.lessp_fn,
                        options.reverse,
                    )?;

                if options.in_place {
                    with_heap_mut(|h| *h.get_vector_mut(v) = sorted_values);
                    Ok(sequence)
                } else {
                    match sequence {
                        Value::Vector(_) => Ok(Value::vector(sorted_values)),
                        Value::Record(_) => {
                            let id = with_heap_mut(|h| h.alloc_vector(sorted_values));
                            Ok(Value::Record(id))
                        }
                        _ => unreachable!(),
                    }
                }
            }
            other => Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("list-or-vector-p"), other],
            )),
        };

        self.ctx.vm_gc_roots.truncate(saved_roots);
        out
    }

    fn builtin_frame_list_fast(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("frame-list", args, 0)?;
        let _ = self.ensure_selected_frame_id();
        let frames = self
            .ctx
            .frames
            .frame_list()
            .into_iter()
            .map(|frame_id| Value::Frame(frame_id.0))
            .collect();
        Ok(Value::list(frames))
    }

    fn builtin_framep_fast(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("framep", args, 1)?;
        let id = match args[0] {
            Value::Frame(id) => id,
            Value::Int(n) => n as u64,
            _ => return Ok(Value::Nil),
        };
        let Some(frame) = self.ctx.frames.get(FrameId(id)) else {
            return Ok(Value::Nil);
        };
        Ok(frame
            .parameters
            .get("window-system")
            .copied()
            .unwrap_or(Value::True))
    }

    fn builtin_frame_parameter_fast(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("frame-parameter", args, 2)?;
        let fid = self.resolve_frame_id(args.first(), "framep")?;
        let param_name = match args[1] {
            Value::Symbol(id) => resolve_sym(id).to_owned(),
            _ => return Ok(Value::Nil),
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
                .unwrap_or(Value::Int(frame.columns() as i64))),
            "height" => Ok(frame
                .parameters
                .get("height")
                .cloned()
                .unwrap_or(Value::Int(frame.lines() as i64))),
            "visibility" => Ok(if frame.visible {
                Value::True
            } else {
                Value::Nil
            }),
            _ => Ok(frame
                .parameters
                .get(&param_name)
                .cloned()
                .unwrap_or(Value::Nil)),
        }
    }

    fn builtin_fboundp_fast(&self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::symbols::builtin_fboundp_in_obarray(&self.ctx.obarray, args)
    }

    fn builtin_current_indentation_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::indent::builtin_current_indentation_in_state(
            &self.ctx.obarray,
            &[],
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_indent_to_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::indent::builtin_indent_to_in_state(
            &self.ctx.obarray,
            &[],
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_current_column_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::indent::builtin_current_column_in_state(
            &self.ctx.obarray,
            &[],
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_buffer_string_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_string_in_manager(
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_buffer_substring_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_substring_in_manager(
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_field_beginning_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_field_beginning_in_state(
            &self.ctx.obarray,
            &[],
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_field_end_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_field_end_in_state(
            &self.ctx.obarray,
            &[],
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_field_string_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_field_string_in_state(
            &self.ctx.obarray,
            &[],
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_field_string_no_properties_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_field_string_no_properties_in_state(
            &self.ctx.obarray,
            &[],
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_constrain_to_field_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_constrain_to_field_in_state(
            &self.ctx.obarray,
            &[],
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_point_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_point_in_manager(&self.ctx.buffers, args.to_vec())
    }

    fn builtin_accept_process_output_shared(&mut self, args: &[Value]) -> EvalResult {
        let (result, callbacks) =
            crate::emacs_core::process::builtin_accept_process_output_collect(
                &mut self.ctx.processes,
                args.to_vec(),
            )?;
        for (callback, callback_args) in callbacks {
            let _ = self.call_function_with_roots(callback, &callback_args)?;
        }
        Ok(result)
    }

    fn builtin_buffer_list_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_list_in_manager(
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_other_buffer_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_other_buffer_in_manager(
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_generate_new_buffer_name_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_generate_new_buffer_name_in_manager(
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_get_file_buffer_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_get_file_buffer_in_state(
            &self.ctx.obarray,
            &[],
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_make_indirect_buffer_shared(&mut self, args: &[Value]) -> EvalResult {
        let plan = crate::emacs_core::builtins::prepare_make_indirect_buffer_in_manager(
            &mut self.ctx.buffers,
            args.to_vec(),
        )?;
        if plan.run_clone_hook {
            self.ctx.buffers.set_current(plan.id);
            let hook_value =
                crate::emacs_core::builtins::symbol_dynamic_buffer_or_global_value_in_state(
                    &self.ctx.obarray,
                    &[],
                    &self.ctx.buffers,
                    "clone-indirect-buffer-hook",
                )
                .unwrap_or(Value::Nil);
            let functions = crate::emacs_core::builtins::collect_hook_functions_in_state(
                &self.ctx.obarray,
                "clone-indirect-buffer-hook",
                hook_value,
                true,
            );
            let clone_result = self.run_hook_functions(&functions, &[]);
            if let Some(saved_id) = plan.saved_current
                && self.ctx.buffers.get(saved_id).is_some()
            {
                self.ctx.buffers.set_current(saved_id);
            }
            clone_result?;
        }
        if plan.run_buffer_list_update_hook {
            let hook_value =
                crate::emacs_core::builtins::symbol_dynamic_buffer_or_global_value_in_state(
                    &self.ctx.obarray,
                    &[],
                    &self.ctx.buffers,
                    "buffer-list-update-hook",
                )
                .unwrap_or(Value::Nil);
            let functions = crate::emacs_core::builtins::collect_hook_functions_in_state(
                &self.ctx.obarray,
                "buffer-list-update-hook",
                hook_value,
                true,
            );
            self.run_hook_functions(&functions, &[])?;
        }
        Ok(Value::Buffer(plan.id))
    }

    fn builtin_kill_buffer_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_kill_buffer_in_state(
            &mut self.ctx.buffers,
            &mut self.ctx.frames,
            args.to_vec(),
        )
    }

    fn builtin_current_active_maps_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::keymaps::builtin_current_active_maps_in_state(
            &mut self.ctx.obarray,
            &[],
            self.ctx.buffers.current_local_map(),
            args,
        )
    }

    fn builtin_current_minor_mode_maps_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::keymaps::builtin_current_minor_mode_maps_in_state(
            &self.ctx.obarray,
            &[],
            args,
        )
    }

    fn builtin_map_keymap_shared(&mut self, args: &[Value], include_parents: bool) -> EvalResult {
        let (function, mut keymap) = if include_parents {
            builtins::expect_min_args("map-keymap", args, 2)?;
            builtins::expect_max_args("map-keymap", args, 3)?;
            (
                args[0],
                crate::emacs_core::builtins::keymaps::expect_keymap_in_obarray(
                    &self.ctx.obarray,
                    &args[1],
                )?,
            )
        } else {
            builtins::expect_args("map-keymap-internal", args, 2)?;
            (
                args[0],
                crate::emacs_core::builtins::keymaps::expect_keymap_in_obarray(
                    &self.ctx.obarray,
                    &args[1],
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
                return Ok(Value::Nil);
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
        Ok(Value::Nil)
    }

    fn builtin_call_last_kbd_macro_shared(&mut self, args: &[Value]) -> EvalResult {
        let (macro_events, count) = {
            let kmacro = self.ctx.kmacro_mut();
            crate::emacs_core::kmacro::plan_call_last_kbd_macro(kmacro, args)?
        };
        let self_insert = self
            .ctx
            .obarray
            .symbol_function("self-insert-command")
            .cloned();
        self.ctx.kmacro_mut().executing = true;
        let result = crate::emacs_core::kmacro::execute_kbd_macro_events(
            self_insert,
            &macro_events,
            count,
            |func, call_args| self.call_function_with_roots(func, &call_args),
        );
        self.ctx.kmacro_mut().executing = false;
        result
    }

    fn builtin_execute_kbd_macro_shared(&mut self, args: &[Value]) -> EvalResult {
        let (macro_events, count) = crate::emacs_core::kmacro::plan_execute_kbd_macro(args)?;
        let self_insert = self
            .ctx
            .obarray
            .symbol_function("self-insert-command")
            .cloned();
        self.ctx.kmacro_mut().executing = true;
        let result = crate::emacs_core::kmacro::execute_kbd_macro_events(
            self_insert,
            &macro_events,
            count,
            |func, call_args| self.call_function_with_roots(func, &call_args),
        );
        self.ctx.kmacro_mut().executing = false;
        result
    }

    fn builtin_command_remapping_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::interactive::builtin_command_remapping_in_state(
            &self.ctx.obarray,
            &[],
            &self.ctx.buffers,
            self.ctx.buffers.current_local_map(),
            args.to_vec(),
        )
    }

    fn builtin_key_binding_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::interactive::builtin_key_binding_in_state(
            &mut self.ctx.obarray,
            &[],
            &self.ctx.buffers,
            self.ctx.buffers.current_local_map(),
            args.to_vec(),
        )
    }

    fn builtin_local_key_binding_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::interactive::builtin_local_key_binding_in_state(
            self.ctx.buffers.current_local_map(),
            args.to_vec(),
        )
    }

    fn builtin_minor_mode_key_binding_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::interactive::builtin_minor_mode_key_binding_in_state(
            &self.ctx.obarray,
            &[],
            args.to_vec(),
        )
    }

    fn builtin_set_buffer_multibyte_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_set_buffer_multibyte_in_manager(
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_insert_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_insert_in_state(
            &self.ctx.obarray,
            &[],
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_barf_if_buffer_read_only_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_barf_if_buffer_read_only_in_state(
            &self.ctx.obarray,
            &[],
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_insert_and_inherit_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_insert_and_inherit_in_state(
            &self.ctx.obarray,
            &[],
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_insert_before_markers_and_inherit_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_insert_before_markers_and_inherit_in_state(
            &self.ctx.obarray,
            &[],
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_point_min_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_point_min_in_manager(&self.ctx.buffers, args.to_vec())
    }

    fn builtin_point_max_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_point_max_in_manager(&self.ctx.buffers, args.to_vec())
    }

    fn builtin_goto_char_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_goto_char_in_manager(
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_char_after_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_char_after_in_manager(&self.ctx.buffers, args.to_vec())
    }

    fn builtin_char_before_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_char_before_in_manager(
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_buffer_size_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_size_in_manager(
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_byte_to_position_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_byte_to_position_in_manager(
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_position_bytes_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_position_bytes_in_manager(
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_get_byte_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_get_byte_in_manager(&self.ctx.buffers, args.to_vec())
    }

    fn builtin_narrow_to_region_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_narrow_to_region_in_manager(
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_widen_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_widen_in_manager(&mut self.ctx.buffers, args.to_vec())
    }

    fn builtin_buffer_modified_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_modified_p_in_manager(
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_set_buffer_modified_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_set_buffer_modified_p_in_manager(
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_buffer_modified_tick_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_modified_tick_in_manager(
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_buffer_chars_modified_tick_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_chars_modified_tick_in_manager(
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_insert_char_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_insert_char_in_state(
            &self.ctx.obarray,
            &[],
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_insert_byte_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_insert_byte_in_state(
            &self.ctx.obarray,
            &[],
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_subst_char_in_region_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_subst_char_in_region_in_state(
            &self.ctx.obarray,
            &[],
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_bobp_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_bobp_in_manager(&self.ctx.buffers, args.to_vec())
    }

    fn builtin_eobp_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_eobp_in_manager(&self.ctx.buffers, args.to_vec())
    }

    fn builtin_bolp_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_bolp_in_manager(&self.ctx.buffers, args.to_vec())
    }

    fn builtin_eolp_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_eolp_in_manager(&self.ctx.buffers, args.to_vec())
    }

    fn builtin_line_beginning_position_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_line_beginning_position_in_manager(
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_line_end_position_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_line_end_position_in_manager(
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_insert_before_markers_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_insert_before_markers_in_state(
            &self.ctx.obarray,
            &[],
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_insert_buffer_substring_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_insert_buffer_substring_in_state(
            &self.ctx.obarray,
            &[],
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_replace_region_contents_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_replace_region_contents_in_state(
            &self.ctx.obarray,
            &[],
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_delete_char_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_delete_char_in_state(
            &self.ctx.obarray,
            &[],
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_buffer_substring_no_properties_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_buffer_substring_no_properties_in_state(
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_following_char_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_following_char_in_state(
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_preceding_char_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_preceding_char_in_state(
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_delete_region_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_delete_region_in_state(
            &self.ctx.obarray,
            &[],
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_compare_buffer_substrings_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_compare_buffer_substrings_in_state(
            self.case_fold_search_enabled(),
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_delete_field_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_delete_field_in_state(
            &self.ctx.obarray,
            &[],
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_delete_and_extract_region_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_delete_and_extract_region_in_state(
            &self.ctx.obarray,
            &[],
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_erase_buffer_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_erase_buffer_in_state(
            &self.ctx.obarray,
            &[],
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_undo_boundary_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::undo::builtin_undo_boundary_in_state(
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_buffer_enable_undo_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_enable_undo_in_manager(
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_buffer_disable_undo_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_disable_undo_in_manager(
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_kill_all_local_variables_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_kill_all_local_variables_in_state(
            &self.ctx.obarray,
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_buffer_local_value_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_local_value_in_state(
            &self.ctx.obarray,
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_local_variable_if_set_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::symbols::builtin_local_variable_if_set_p_in_state(
            &self.ctx.obarray,
            &self.ctx.custom,
            args.to_vec(),
        )
    }

    fn builtin_variable_binding_locus_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::symbols::builtin_variable_binding_locus_in_state(
            &self.ctx.obarray,
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_move_to_column_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::indent::builtin_move_to_column_in_state(
            &self.ctx.obarray,
            &[],
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn case_fold_search_enabled(&self) -> bool {
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
        crate::emacs_core::charset::builtin_find_charset_region_in_manager(
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_charset_after_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::charset::builtin_charset_after_in_manager(
            &self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_compose_region_internal_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::composite::builtin_compose_region_internal_in_manager(
            &self.ctx.buffers,
            args.to_vec(),
        )
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
        crate::emacs_core::navigation::builtin_skip_chars_forward_in_manager(
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_skip_chars_backward_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_skip_chars_backward_in_manager(
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_scan_lists_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::syntax::builtin_scan_lists_in_manager(&self.ctx.buffers, args.to_vec())
    }

    fn builtin_scan_sexps_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::syntax::builtin_scan_sexps_in_manager(&self.ctx.buffers, args.to_vec())
    }

    fn visible_variable_value_or_nil(&self, name: &str) -> Value {
        let name_id = intern(name);
        let is_dynamically_special =
            self.ctx.obarray.is_special_id(name_id) && !self.ctx.obarray.is_constant_id(name_id);
        if !is_dynamically_special
            && !lexenv_declares_special(self.ctx.lexenv, name_id)
            && let Some(value) = lexenv_lookup(self.ctx.lexenv, name_id)
        {
            return value;
        }
        // specbind writes directly to obarray, so no dynamic stack lookup needed.
        if let Some(buffer) = self.ctx.buffers.current_buffer()
            && let Some(binding) = buffer.get_buffer_local_binding(name)
        {
            return binding.as_value().unwrap_or(Value::Nil);
        }
        if let Some(value) = self.ctx.obarray.symbol_value(name).copied() {
            return value;
        }
        if name == "nil" {
            return Value::Nil;
        }
        if name == "t" {
            return Value::True;
        }
        Value::Nil
    }

    fn instantiate_callable_cons_form(&mut self, function: Value) -> EvalResult {
        let items =
            list_to_vec(&function).ok_or_else(|| signal("invalid-function", vec![function]))?;
        let Some(head_name) = items.first().and_then(Value::as_symbol_name) else {
            return Err(signal("invalid-function", vec![function]));
        };

        let (env_value, params_value, mut body_start) = match head_name {
            "lambda" => {
                let Some(params_value) = items.get(1).copied() else {
                    return Err(signal("invalid-function", vec![function]));
                };
                let env_value = if self
                    .ctx
                    .obarray
                    .symbol_value("lexical-binding")
                    .is_some_and(|value| value.is_truthy())
                    || !self.ctx.lexenv.is_nil()
                {
                    if self.ctx.lexenv.is_nil() {
                        Value::list(vec![Value::True])
                    } else {
                        self.ctx.lexenv
                    }
                } else {
                    Value::Nil
                };
                (env_value, params_value, 2)
            }
            "closure" => {
                let (Some(env_value), Some(params_value)) =
                    (items.get(1).copied(), items.get(2).copied())
                else {
                    return Err(signal("invalid-function", vec![function]));
                };
                (env_value, params_value, 3)
            }
            _ => return Err(signal("invalid-function", vec![function])),
        };

        let docstring_value = if matches!(items.get(body_start), Some(Value::Str(_)))
            && items.get(body_start + 1).is_some()
        {
            let value = items[body_start];
            body_start += 1;
            value
        } else {
            Value::Nil
        };

        let mut doc_form_value = Value::Nil;
        if let Some(item) = items.get(body_start)
            && let Some(entry) = list_to_vec(item)
            && entry.len() == 2
            && entry[0].as_symbol_name() == Some(":documentation")
        {
            doc_form_value = entry[1];
            body_start += 1;
        }

        while let Some(item) = items.get(body_start) {
            let Some(declare) = list_to_vec(item) else {
                break;
            };
            if declare
                .first()
                .and_then(Value::as_symbol_name)
                .is_some_and(|name| name == "declare")
            {
                body_start += 1;
            } else {
                break;
            }
        }

        let body_value = if body_start >= items.len() {
            Value::Nil
        } else {
            Value::list(items[body_start..].to_vec())
        };
        let closure_doc_value = if !doc_form_value.is_nil() {
            doc_form_value
        } else {
            docstring_value
        };
        let iform_value = Value::Nil;

        let mut extra_roots = vec![
            function,
            params_value,
            body_value,
            env_value,
            closure_doc_value,
            iform_value,
        ];

        if head_name == "lambda" && !env_value.is_nil() {
            let closure_hook =
                self.visible_variable_value_or_nil("internal-make-interpreted-closure-function");
            if !closure_hook.is_nil() {
                extra_roots.push(closure_hook);
                return self.with_extra_roots(&extra_roots, |vm| {
                    vm.call_function_with_roots(
                        closure_hook,
                        &[
                            params_value,
                            body_value,
                            env_value,
                            closure_doc_value,
                            iform_value,
                        ],
                    )
                });
            }
        }

        self.with_extra_roots(&extra_roots, |_vm| {
            crate::emacs_core::builtins::symbols::make_interpreted_closure_from_parts(
                &params_value,
                &body_value,
                &env_value,
                Some(&closure_doc_value),
                Some(&iform_value),
            )
        })
    }

    fn call_function(&mut self, func_val: Value, args: Vec<Value>) -> EvalResult {
        match func_val {
            Value::ByteCode(_) => {
                let bc_data = func_val.get_bytecode_data().unwrap().clone();
                self.execute_with_func_value(&bc_data, args, func_val)
            }
            Value::Lambda(_) => {
                let lambda_data = func_val.get_lambda_data().unwrap().clone();
                let mut extra_roots = Vec::with_capacity(args.len() + 1);
                extra_roots.push(func_val);
                extra_roots.extend(args.iter().copied());
                let call_state = self.ctx.begin_lambda_call(&lambda_data, &args, func_val)?;
                let body = lambda_data.body.clone();
                let result = crate::emacs_core::eval::eval_lambda_body_in_vm_runtime(
                    &mut self.ctx,
                    &[],
                    &extra_roots,
                    body,
                );
                self.ctx.finish_lambda_call(call_state);
                result
            }
            Value::Subr(id) => self.dispatch_vm_builtin(resolve_sym(id), args),
            Value::Symbol(id) => {
                let name = resolve_sym(id);
                // Try obarray function cell
                if let Some(func) = self.ctx.obarray.symbol_function(name).cloned() {
                    if func.is_nil() {
                        if self.ctx.subr_registry.contains_key(&intern(name)) {
                            return self.dispatch_vm_builtin(name, args);
                        }
                        return Err(signal("void-function", vec![Value::symbol(name)]));
                    }
                    if crate::emacs_core::autoload::is_autoload_value(&func) {
                        let mut autoload_roots = Vec::with_capacity(args.len() + 2);
                        autoload_roots.push(Value::Symbol(id));
                        autoload_roots.push(func);
                        autoload_roots.extend(args.iter().copied());
                        let loaded =
                            crate::emacs_core::autoload::builtin_autoload_do_load_in_vm_runtime(
                                &mut self.ctx,
                                &[],
                                &[func, Value::Symbol(id)],
                                &autoload_roots,
                            )?;
                        return self.call_function(loaded, args);
                    }
                    return self.call_function(func, args);
                }
                // Try builtin
                self.dispatch_vm_builtin(name, args)
            }
            function @ Value::Cons(_) => {
                if crate::emacs_core::autoload::is_autoload_value(&function) {
                    Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("symbolp"), function],
                    ))
                } else if function.cons_car().is_symbol_named("lambda")
                    || function.cons_car().is_symbol_named("closure")
                {
                    let callable = self.instantiate_callable_cons_form(function)?;
                    self.call_function(callable, args)
                } else {
                    Err(signal("invalid-function", vec![function]))
                }
            }
            _ => Err(signal("invalid-function", vec![func_val])),
        }
    }

    /// Execute a compiled function without param binding (for inline compilation).
    fn execute_inline(&mut self, func: &ByteCodeFunction) -> EvalResult {
        let mut stack: Vec<Value> = Vec::with_capacity(func.max_stack as usize);
        let mut pc: usize = 0;
        let mut handlers: Vec<Handler> = Vec::new();
        let mut specpdl: Vec<VmUnwindEntry> = Vec::new();
        let result = self.run_loop(func, &mut stack, &mut pc, &mut handlers, &mut specpdl);
        let cleanup_roots = Self::result_roots(&result);
        let mut cleanup_extra_roots = cleanup_roots.clone();
        Self::collect_specpdl_roots(&specpdl, &mut cleanup_extra_roots);
        let cleanup =
            self.with_frame_roots(func, &stack, &handlers, &[], &cleanup_extra_roots, |vm| {
                vm.unwind_specpdl_all(&mut specpdl)
            });
        merge_result_with_cleanup(result, cleanup)
    }

    /// Run cleanup functions collected during non-local resolution.
    fn run_unwind_cleanups(&mut self, cleanups: &[Value]) -> Result<(), Flow> {
        for cleanup in cleanups {
            let cleanup_root = [*cleanup];
            self.with_extra_roots(&cleanup_root, |vm| vm.call_function(*cleanup, vec![]))?;
        }
        Ok(())
    }

    fn resume_nonlocal(
        &mut self,
        _func: &ByteCodeFunction,
        stack: &mut Vec<Value>,
        pc: &mut usize,
        handlers: &mut Vec<Handler>,
        specpdl: &mut Vec<VmUnwindEntry>,
        flow: Flow,
    ) -> Result<(), Flow> {
        match flow {
            Flow::Throw { tag, value } => {
                if let Some(res) = resolve_throw_target(handlers, &mut self.ctx.catch_tags, &tag) {
                    let extra = [tag, value];
                    if let Err(cleanup_flow) =
                        self.with_frame_roots(_func, stack, handlers, specpdl, &extra, |vm| {
                            vm.run_unwind_cleanups(&res.cleanups)
                        })
                    {
                        return self.resume_nonlocal(
                            _func,
                            stack,
                            pc,
                            handlers,
                            specpdl,
                            cleanup_flow,
                        );
                    }
                    let mut unwind_roots = extra.to_vec();
                    Self::collect_specpdl_roots(specpdl, &mut unwind_roots);
                    if let Err(cleanup_flow) =
                        self.with_frame_roots(_func, stack, handlers, &[], &unwind_roots, |vm| {
                            vm.unwind_specpdl_to(res.spec_depth, specpdl)
                        })
                    {
                        return self.resume_nonlocal(
                            _func,
                            stack,
                            pc,
                            handlers,
                            specpdl,
                            cleanup_flow,
                        );
                    }
                    stack.truncate(res.stack_len);
                    stack.push(value);
                    *pc = res.target as usize;
                    return Ok(());
                }

                // No matching catch in VM handler stack. Check evaluator
                // catch_tags (catches established by the interpreter above us).
                // If found -> Flow::Throw (will be caught by sf_catch).
                // If not -> signal no-catch immediately (GNU Emacs semantics).
                if !tag.is_nil() && self.ctx.catch_tags.iter().rev().any(|t| eq_value(t, &tag)) {
                    return Err(Flow::Throw { tag, value });
                }
                Err(signal("no-catch", vec![tag, value]))
            }
            Flow::Signal(sig) => {
                if let Some(res) = resolve_signal_target(
                    handlers,
                    &mut self.ctx.catch_tags,
                    &self.ctx.obarray,
                    &sig,
                ) {
                    let mut signal_roots = Vec::new();
                    Self::collect_flow_roots(&Flow::Signal(sig.clone()), &mut signal_roots);
                    if let Err(cleanup_flow) = self.with_frame_roots(
                        _func,
                        stack,
                        handlers,
                        specpdl,
                        &signal_roots,
                        |vm| vm.run_unwind_cleanups(&res.cleanups),
                    ) {
                        return self.resume_nonlocal(
                            _func,
                            stack,
                            pc,
                            handlers,
                            specpdl,
                            cleanup_flow,
                        );
                    }
                    let mut unwind_roots = signal_roots.clone();
                    Self::collect_specpdl_roots(specpdl, &mut unwind_roots);
                    if let Err(cleanup_flow) =
                        self.with_frame_roots(_func, stack, handlers, &[], &unwind_roots, |vm| {
                            vm.unwind_specpdl_to(res.spec_depth, specpdl)
                        })
                    {
                        return self.resume_nonlocal(
                            _func,
                            stack,
                            pc,
                            handlers,
                            specpdl,
                            cleanup_flow,
                        );
                    }
                    stack.truncate(res.stack_len);
                    stack.push(make_signal_binding_value(&sig));
                    *pc = res.target as usize;
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
            VmUnwindEntry::DynamicBinding {
                name,
                restored_value,
                specpdl_count,
            } => {
                crate::emacs_core::eval::unbind_to_in_state(
                    &mut self.ctx.obarray,
                    &mut self.ctx.specpdl,
                    specpdl_count,
                );
                self.run_variable_watchers(&name, &restored_value, &Value::Nil, "unlet")?;
            }
            VmUnwindEntry::LexicalBinding {
                name,
                restored_value,
                old_lexenv,
            } => {
                self.ctx.lexenv = old_lexenv;
                self.run_variable_watchers(&name, &restored_value, &Value::Nil, "unlet")?;
            }
            VmUnwindEntry::Cleanup { cleanup } => {
                let cleanup_root = [cleanup];
                self.with_extra_roots(&cleanup_root, |vm| vm.call_function(cleanup, vec![]))?;
            }
            VmUnwindEntry::CurrentBuffer { buffer_id } => {
                self.ctx.buffers.set_current(buffer_id);
            }
            VmUnwindEntry::Excursion {
                buffer_id,
                marker_id,
            } => {
                if self.ctx.buffers.get(buffer_id).is_some() {
                    self.ctx.buffers.set_current(buffer_id);
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

    /// Dispatch to builtin functions from the VM.
    fn dispatch_vm_builtin(&mut self, name: &str, args: Vec<Value>) -> EvalResult {
        // Handle VM-specific builtins that need call_function (the VM's own
        // dispatch loop). Everything else goes through the unified
        // dispatch_builtin on Context.
        match name {
            "apply" => {
                if args.is_empty() {
                    return Err(signal(
                        "wrong-number-of-arguments",
                        vec![Value::symbol("apply"), Value::Int(args.len() as i64)],
                    ));
                }
                if args.len() == 1 {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("listp"), args[0]],
                    ));
                }
                let func = args[0];
                let last = &args[args.len() - 1];
                let mut call_args: Vec<Value> = args[1..args.len() - 1].to_vec();
                let spread = match last {
                    Value::Nil => Vec::new(),
                    Value::Cons(_) => list_to_vec(last).ok_or_else(|| {
                        signal("wrong-type-argument", vec![Value::symbol("listp"), *last])
                    })?,
                    _ => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("listp"), *last],
                        ));
                    }
                };
                call_args.extend(spread);
                return self.call_function(func, call_args);
            }
            "funcall" => {
                if args.is_empty() {
                    return Err(signal(
                        "wrong-number-of-arguments",
                        vec![Value::symbol("funcall"), Value::Int(args.len() as i64)],
                    ));
                }
                return self.call_function(args[0], args[1..].to_vec());
            }
            "funcall-interactively" => {
                if args.is_empty() {
                    return Err(signal(
                        "wrong-number-of-arguments",
                        vec![
                            Value::symbol("funcall-interactively"),
                            Value::Int(args.len() as i64),
                        ],
                    ));
                }
                return self.call_function(args[0], args[1..].to_vec());
            }
            "call-interactively" => return self.builtin_call_interactively_shared(&args),
            "interactive" => return Ok(Value::Nil),
            "%%defvar" => {
                // args: [init_value, symbol_name]
                if args.len() >= 2 {
                    let sym_name = args[1].as_symbol_name().unwrap_or("nil").to_string();
                    if !self.ctx.obarray.boundp(&sym_name) {
                        self.ctx.obarray.set_symbol_value(&sym_name, args[0]);
                    }
                    self.ctx.obarray.make_special(&sym_name);
                    return Ok(Value::symbol(sym_name));
                }
                return Ok(Value::Nil);
            }
            "%%defconst" => {
                if args.len() >= 2 {
                    let sym_name = args[1].as_symbol_name().unwrap_or("nil").to_string();
                    self.ctx.obarray.set_symbol_value(&sym_name, args[0]);
                    let sym = self.ctx.obarray.get_or_intern(&sym_name);
                    sym.constant = true;
                    sym.special = true;
                    return Ok(Value::symbol(sym_name));
                }
                return Ok(Value::Nil);
            }
            "%%unimplemented-elc-bytecode" => {
                return Err(signal(
                    "error",
                    vec![Value::string(
                        "Compiled .elc bytecode execution is not implemented yet",
                    )],
                ));
            }
            "throw" => {
                if args.len() != 2 {
                    return Err(signal(
                        "wrong-number-of-arguments",
                        vec![Value::Subr(intern("throw")), Value::Int(args.len() as i64)],
                    ));
                }
                let tag = args[0];
                let value = args[1];
                // Check evaluator catch_tags for a matching catch.
                if !tag.is_nil() && self.ctx.catch_tags.iter().rev().any(|t| eq_value(t, &tag)) {
                    return Err(Flow::Throw { tag, value });
                }
                return Err(signal("no-catch", vec![tag, value]));
            }
            "signal" => return crate::emacs_core::errors::builtin_signal(args),
            _ => {}
        }

        // Unified dispatch: all builtins go through the single dispatch_builtin
        // on Context. No more duplicated _in_obarray/_in_state variants needed.
        if let Some(result) = builtins::dispatch_builtin(&mut *self.ctx, name, args) {
            return result.map_err(|flow| normalize_vm_builtin_error(name, flow));
        }
        Err(signal("void-function", vec![Value::symbol(name)]))
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
            plan.func = self.instantiate_callable_cons_form(plan.func)?;
        }
        let (function, call_args) =
            crate::emacs_core::interactive::resolve_call_interactively_target_and_args_with_vm_fallback(
                &mut self.ctx,
                &mut plan,
                &[],
                &extra_roots,
            )?;
        self.ctx.interactive.push_interactive_call(true);
        let result = self.call_function_with_roots(function, &call_args);
        self.ctx.interactive.pop_interactive_call();
        result
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
                    match cursor {
                        Value::Nil => return Ok(Value::Nil),
                        Value::Cons(cell) => {
                            let pair = read_cons(cell);
                            if let Value::Cons(ref entry) = pair.car {
                                let entry_pair = read_cons(*entry);
                                let entry_key = entry_pair.car;
                                let matches = vm.with_extra_roots(
                                    &[cursor, pair.car, pair.cdr, entry_key],
                                    |vm| {
                                        vm.call_function(test_fn, vec![entry_key, key])
                                            .map(|value| value.is_truthy())
                                    },
                                )?;
                                if matches {
                                    return Ok(pair.car);
                                }
                            }
                            cursor = pair.cdr;
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
        crate::emacs_core::builtins::builtin_assoc(vec![args[0], args[1]])
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
                    match cursor {
                        Value::Cons(key_cell) => {
                            let pair = read_cons(key_cell);
                            let entry_key = pair.car;
                            let matches =
                                vm.with_extra_roots(&[cursor, entry_key, pair.cdr], |vm| {
                                    vm.call_function(predicate, vec![entry_key, prop])
                                        .map(|value| value.is_truthy())
                                })?;
                            if matches {
                                return Ok(Value::Cons(key_cell));
                            }

                            match pair.cdr {
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
            });
        }
        crate::emacs_core::builtins::builtin_plist_member_in_state(args.to_vec())
    }

    fn builtin_garbage_collect_shared(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("garbage-collect", args, 0)?;
        self.ctx.gc_collect();
        crate::emacs_core::builtins_extra::builtin_garbage_collect(vec![])
    }

    fn builtin_kill_emacs_shared(&mut self, args: &[Value]) -> EvalResult {
        let request = crate::emacs_core::builtins::symbols::plan_kill_emacs_request(args)?;
        self.builtin_run_hooks_shared(&[Value::symbol("kill-emacs-hook")])?;
        self.ctx
            .request_shutdown(request.exit_code, request.restart);
        Ok(Value::Nil)
    }

    fn builtin_macroexpand_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::symbols::builtin_macroexpand_with_runtime(self, args.to_vec())
    }

    fn builtin_mapatoms_shared(&mut self, args: &[Value]) -> EvalResult {
        let (func, symbols) =
            crate::emacs_core::hashtab::collect_mapatoms_symbols(&self.ctx.obarray, args.to_vec())?;
        for sym in symbols {
            self.call_function_with_roots(func, &[sym])?;
        }
        Ok(Value::Nil)
    }

    fn builtin_maphash_shared(&mut self, args: &[Value]) -> EvalResult {
        let (func, entries) = crate::emacs_core::hashtab::collect_maphash_entries(args.to_vec())?;
        for (key, value) in entries {
            self.call_function_with_roots(func, &[key, value])?;
        }
        Ok(Value::Nil)
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
                &self.ctx.obarray,
                &args[1],
            )?;
        crate::emacs_core::minibuffer::builtin_try_completion_with_candidates(
            args,
            candidates,
            |function, call_args| self.call_function_with_roots(function, &call_args),
        )
    }

    fn builtin_all_completions_shared(&mut self, args: &[Value]) -> EvalResult {
        let candidates =
            crate::emacs_core::minibuffer::completion_candidates_from_collection_in_state(
                &self.ctx.obarray,
                &args[1],
            )?;
        crate::emacs_core::minibuffer::builtin_all_completions_with_candidates(
            args,
            candidates,
            |function, call_args| self.call_function_with_roots(function, &call_args),
        )
    }

    fn builtin_file_name_completion_shared(&mut self, args: &[Value]) -> EvalResult {
        let needs_eval_predicate = matches!(
            args.get(2),
            Some(predicate)
                if !predicate.is_nil()
                    && !matches!(predicate, Value::Symbol(_) | Value::Subr(_))
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
                |predicate_arg| {
                    self.with_default_directory_binding(bound_directory.as_str(), |vm| {
                        vm.call_function_with_roots(predicate, &[predicate_arg])
                    })
                },
            );
        }
        crate::emacs_core::dired::builtin_file_name_completion_in_state(
            &mut self.ctx,
            args.to_vec(),
        )
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
                &self.ctx.obarray,
                &args[1],
            )?;
        crate::emacs_core::minibuffer::builtin_test_completion_with_candidates(
            args,
            candidates,
            |function, call_args| self.call_function_with_roots(function, &call_args),
        )
    }

    fn builtin_input_pending_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::reader::builtin_input_pending_p_in_state(
            &self.ctx.obarray,
            &[],
            args.to_vec(),
        )
    }

    fn builtin_discard_input_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::reader::builtin_discard_input_in_state(
            &mut self.ctx.obarray,
            &mut [],
            &mut self.ctx.buffers,
            &self.ctx.custom,
            self.ctx.specpdl.as_slice(),
            args.to_vec(),
        )
    }

    fn builtin_current_input_mode_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::reader::builtin_current_input_mode_in_state(
            self.ctx.input_mode_interrupt,
            args.to_vec(),
        )
    }

    fn builtin_set_input_mode_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::reader::builtin_set_input_mode_in_state(
            &mut self.ctx.input_mode_interrupt,
            args.to_vec(),
        )
    }

    fn builtin_set_input_interrupt_mode_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::reader::builtin_set_input_interrupt_mode_in_state(
            &mut self.ctx.input_mode_interrupt,
            args.to_vec(),
        )
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
        crate::emacs_core::reader::builtin_read_from_string_in_state(
            &self.ctx.obarray,
            args.to_vec(),
        )
    }

    fn builtin_read_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::reader::builtin_read_in_state(
            &self.ctx.obarray,
            &mut self.ctx.buffers,
            args.to_vec(),
        )
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
        crate::emacs_core::reader::finish_read_key_sequence_interactive_in_runtime(self.ctx)
    }

    fn builtin_read_key_sequence_vector_shared(&mut self, args: &[Value]) -> EvalResult {
        if let Some(value) =
            crate::emacs_core::reader::builtin_read_key_sequence_vector_in_runtime(self.ctx, args)?
        {
            return Ok(value);
        }
        crate::emacs_core::reader::finish_read_key_sequence_vector_interactive_in_runtime(self.ctx)
    }

    fn builtin_recent_keys_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::keymaps::builtin_recent_keys_in_state(
            self.ctx.recent_input_events.as_slice(),
            args.to_vec(),
        )
    }

    fn builtin_current_message_shared(&mut self, args: &[Value]) -> EvalResult {
        let (_, _, _, _, current_message) = self.ctx.printer_runtime_state();
        crate::emacs_core::builtins::builtin_current_message_in_state(
            &*current_message,
            args.to_vec(),
        )
    }

    fn builtin_current_case_table_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::casetab::builtin_current_case_table_in_state(
            &mut self.ctx.obarray,
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_standard_case_table_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::casetab::builtin_standard_case_table_in_state(
            &mut self.ctx.obarray,
            args.to_vec(),
        )
    }

    fn builtin_set_case_table_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::casetab::builtin_set_case_table_in_state(
            &mut self.ctx.obarray,
            &mut self.ctx.buffers,
            args.to_vec(),
        )
    }

    fn builtin_set_standard_case_table_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::casetab::builtin_set_standard_case_table_in_state(
            &mut self.ctx.obarray,
            args.to_vec(),
        )
    }

    fn builtin_format_shared(&mut self, args: &[Value]) -> EvalResult {
        let (obarray, buffers, frames, threads, _) = self.ctx.printer_runtime_state();
        crate::emacs_core::builtins::builtin_format_in_state(
            &*obarray,
            &*buffers,
            &*frames,
            &*threads,
            args.to_vec(),
        )
    }

    fn builtin_format_message_shared(&mut self, args: &[Value]) -> EvalResult {
        let (obarray, buffers, frames, threads, _) = self.ctx.printer_runtime_state();
        crate::emacs_core::builtins::builtin_format_message_in_state(
            &*obarray,
            &*buffers,
            &*frames,
            &*threads,
            args.to_vec(),
        )
    }

    fn builtin_message_shared(&mut self, args: &[Value]) -> EvalResult {
        let (obarray, buffers, frames, threads, current_message) = self.ctx.printer_runtime_state();
        crate::emacs_core::builtins::builtin_message_in_state(
            &*obarray,
            &[],
            &*buffers,
            &*frames,
            &*threads,
            current_message,
            args.to_vec(),
        )
    }

    fn builtin_message_box_shared(&mut self, args: &[Value]) -> EvalResult {
        let (obarray, buffers, frames, threads, _) = self.ctx.printer_runtime_state();
        crate::emacs_core::builtins::builtin_message_box_in_state(
            &*obarray,
            &*buffers,
            &*frames,
            &*threads,
            args.to_vec(),
        )
    }

    fn builtin_message_or_box_shared(&mut self, args: &[Value]) -> EvalResult {
        let (obarray, buffers, frames, threads, _) = self.ctx.printer_runtime_state();
        crate::emacs_core::builtins::builtin_message_or_box_in_state(
            &*obarray,
            &*buffers,
            &*frames,
            &*threads,
            args.to_vec(),
        )
    }

    fn builtin_make_thread_shared(&mut self, args: &[Value]) -> EvalResult {
        let (thread_id, function) =
            crate::emacs_core::threads::prepare_make_thread_in_state(&mut self.ctx.threads, args)?;
        let saved_current = self.ctx.threads.enter_thread(thread_id);
        let result = self.call_function_with_roots(function, &[]);
        self.ctx.threads.restore_thread(saved_current);
        crate::emacs_core::threads::finish_make_thread_result_in_state(
            &mut self.ctx.threads,
            thread_id,
            result,
        )
    }

    fn builtin_thread_join_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_thread_join_in_state(
            &mut self.ctx.threads,
            args.to_vec(),
        )
    }

    fn builtin_thread_yield_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_thread_yield_in_state(args.to_vec())
    }

    fn builtin_thread_name_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_thread_name_in_state(&self.ctx.threads, args.to_vec())
    }

    fn builtin_thread_live_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_thread_live_p_in_state(&self.ctx.threads, args.to_vec())
    }

    fn builtin_threadp_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_threadp_in_state(&self.ctx.threads, args.to_vec())
    }

    fn builtin_thread_signal_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_thread_signal_in_state(&self.ctx.threads, args.to_vec())
    }

    fn builtin_current_thread_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_current_thread_in_state(
            &self.ctx.threads,
            args.to_vec(),
        )
    }

    fn builtin_all_threads_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_all_threads_in_state(&self.ctx.threads, args.to_vec())
    }

    fn builtin_thread_last_error_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_thread_last_error_in_state(
            &mut self.ctx.threads,
            args.to_vec(),
        )
    }

    fn builtin_make_mutex_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_make_mutex_in_state(
            &mut self.ctx.threads,
            args.to_vec(),
        )
    }

    fn builtin_mutex_name_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_mutex_name_in_state(&self.ctx.threads, args.to_vec())
    }

    fn builtin_mutex_lock_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_mutex_lock_in_state(
            &mut self.ctx.threads,
            args.to_vec(),
        )
    }

    fn builtin_mutex_unlock_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_mutex_unlock_in_state(
            &mut self.ctx.threads,
            args.to_vec(),
        )
    }

    fn builtin_mutexp_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_mutexp_in_state(&self.ctx.threads, args.to_vec())
    }

    fn builtin_make_condition_variable_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_make_condition_variable_in_state(
            &mut self.ctx.threads,
            args.to_vec(),
        )
    }

    fn builtin_condition_variable_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_condition_variable_p_in_state(
            &self.ctx.threads,
            args.to_vec(),
        )
    }

    fn builtin_condition_name_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_condition_name_in_state(
            &self.ctx.threads,
            args.to_vec(),
        )
    }

    fn builtin_condition_mutex_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_condition_mutex_in_state(
            &self.ctx.threads,
            args.to_vec(),
        )
    }

    fn builtin_condition_wait_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_condition_wait_in_state(
            &self.ctx.threads,
            args.to_vec(),
        )
    }

    fn builtin_condition_notify_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::threads::builtin_condition_notify_in_state(
            &self.ctx.threads,
            args.to_vec(),
        )
    }

    fn builtin_princ_shared(&mut self, args: &[Value]) -> EvalResult {
        let (target, text) = {
            let (obarray, buffers, frames, threads, _) = self.ctx.printer_runtime_state();
            let target = crate::emacs_core::builtins::resolve_print_target_in_state(
                &*obarray,
                &[],
                args.get(1),
            );
            if crate::emacs_core::builtins::print_target_is_direct(target) {
                return crate::emacs_core::builtins::builtin_princ_in_state(
                    &*obarray,
                    &[],
                    buffers,
                    &*frames,
                    &*threads,
                    args.to_vec(),
                );
            }
            let text = crate::emacs_core::builtins::print_value_princ_in_state(
                &*obarray, &*buffers, &*frames, &*threads, &args[0],
            );
            (target, text)
        };
        crate::emacs_core::builtins::dispatch_print_callback_chars(&text, |ch| {
            self.call_function_with_roots(target, &[ch]).map(|_| ())
        })?;
        Ok(args[0])
    }

    fn builtin_prin1_shared(&mut self, args: &[Value]) -> EvalResult {
        let (target, text) = {
            let (obarray, buffers, frames, threads, _) = self.ctx.printer_runtime_state();
            let target = crate::emacs_core::builtins::resolve_print_target_in_state(
                &*obarray,
                &[],
                args.get(1),
            );
            if crate::emacs_core::builtins::print_target_is_direct(target) {
                return crate::emacs_core::builtins::builtin_prin1_in_state(
                    &*obarray,
                    &[],
                    buffers,
                    &*frames,
                    &*threads,
                    args.to_vec(),
                );
            }
            let text = crate::emacs_core::error::print_value_in_state(
                &*obarray, &*buffers, &*frames, &*threads, &args[0],
            );
            (target, text)
        };
        crate::emacs_core::builtins::dispatch_print_callback_chars(&text, |ch| {
            self.call_function_with_roots(target, &[ch]).map(|_| ())
        })?;
        Ok(args[0])
    }

    fn builtin_prin1_to_string_shared(&mut self, args: &[Value]) -> EvalResult {
        let (obarray, buffers, frames, threads, _) = self.ctx.printer_runtime_state();
        crate::emacs_core::builtins::builtin_prin1_to_string_in_state(
            &*obarray,
            &*buffers,
            &*frames,
            &*threads,
            args.to_vec(),
        )
    }

    fn builtin_print_shared(&mut self, args: &[Value]) -> EvalResult {
        let (target, text) = {
            let (obarray, buffers, frames, threads, _) = self.ctx.printer_runtime_state();
            let target = crate::emacs_core::builtins::resolve_print_target_in_state(
                &*obarray,
                &[],
                args.get(1),
            );
            if crate::emacs_core::builtins::print_target_is_direct(target) {
                return crate::emacs_core::builtins::builtin_print_in_state(
                    &*obarray,
                    &[],
                    buffers,
                    &*frames,
                    &*threads,
                    args.to_vec(),
                );
            }
            let mut text = String::new();
            text.push('\n');
            text.push_str(&crate::emacs_core::error::print_value_in_state(
                &*obarray, &*buffers, &*frames, &*threads, &args[0],
            ));
            text.push('\n');
            (target, text)
        };
        crate::emacs_core::builtins::dispatch_print_callback_chars(&text, |ch| {
            self.call_function_with_roots(target, &[ch]).map(|_| ())
        })?;
        Ok(args[0])
    }

    fn builtin_terpri_shared(&mut self, args: &[Value]) -> EvalResult {
        let target = {
            let (obarray, buffers, _, _, _) = self.ctx.printer_runtime_state();
            if let Some(result) = crate::emacs_core::builtins::builtin_terpri_in_state(
                &*obarray,
                &[],
                buffers,
                args.to_vec(),
            )? {
                return Ok(result);
            }
            crate::emacs_core::builtins::resolve_print_target_in_state(&*obarray, &[], args.first())
        };
        self.call_function_with_roots(target, &[Value::Int('\n' as i64)])?;
        Ok(Value::True)
    }

    fn builtin_write_char_shared(&mut self, args: &[Value]) -> EvalResult {
        let target = {
            let (obarray, buffers, _, _, _) = self.ctx.printer_runtime_state();
            if let Some(result) = crate::emacs_core::builtins::builtin_write_char_in_state(
                &*obarray,
                &[],
                buffers,
                args.to_vec(),
            )? {
                return Ok(result);
            }
            crate::emacs_core::builtins::resolve_print_target_in_state(&*obarray, &[], args.get(1))
        };
        let char_code = match crate::emacs_core::builtins::builtin_write_char(args.to_vec())? {
            Value::Int(n) => n,
            _ => unreachable!("write-char returns character"),
        };
        self.call_function_with_roots(target, &[Value::Int(char_code)])?;
        Ok(Value::Int(char_code))
    }

    fn builtin_redraw_frame_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::dispnew::pure::builtin_redraw_frame(args.to_vec())
    }

    fn builtin_x_get_resource_shared(&mut self, args: &[Value]) -> EvalResult {
        self.ctx.with_parent_evaluator(|eval| {
            crate::emacs_core::display::builtin_x_get_resource_eval(eval, args.to_vec())
        })
    }

    fn builtin_x_list_fonts_shared(&mut self, args: &[Value]) -> EvalResult {
        self.ctx.with_parent_evaluator(|eval| {
            crate::emacs_core::display::builtin_x_list_fonts_eval(eval, args.to_vec())
        })
    }

    fn builtin_x_server_vendor_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::display::builtin_x_server_vendor_in_state(
            &self.ctx.frames,
            args.to_vec(),
        )
    }

    fn builtin_xw_display_color_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::symbols::builtin_xw_display_color_p_in_state(
            &self.ctx.frames,
            &self.ctx.obarray,
            &[],
            args.to_vec(),
        )
    }

    fn builtin_display_color_cells_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::display::builtin_display_color_cells_in_state(
            &self.ctx.frames,
            &self.ctx.obarray,
            &[],
            args.to_vec(),
        )
    }

    fn builtin_tty_type_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::terminal::pure::builtin_tty_type_in_state(
            &self.ctx.frames,
            args.to_vec(),
        )
    }

    fn builtin_suspend_tty_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::terminal::pure::builtin_suspend_tty_in_state(
            &self.ctx.frames,
            args.to_vec(),
        )
    }

    fn builtin_resume_tty_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::terminal::pure::builtin_resume_tty_in_state(
            &self.ctx.frames,
            args.to_vec(),
        )
    }

    fn builtin_x_create_frame_shared(&mut self, args: &[Value]) -> EvalResult {
        let args = args.to_vec();
        tracing::debug!("builtin_x_create_frame_shared: delegating via parent evaluator");
        self.ctx.with_parent_evaluator(|eval| {
            crate::emacs_core::window_cmds::builtin_x_create_frame(eval, args)
        })
    }

    fn builtin_make_frame_shared(&mut self, args: &[Value]) -> EvalResult {
        let args = args.to_vec();
        self.ctx.with_parent_evaluator(|eval| {
            crate::emacs_core::window_cmds::builtin_make_frame(eval, args)
        })
    }

    fn builtin_set_frame_height_shared(&mut self, args: &[Value]) -> EvalResult {
        let (frames, buffers, display_host) = self.ctx.gui_frame_creation_state();
        crate::emacs_core::window_cmds::builtin_set_frame_height_in_state(
            frames,
            buffers,
            display_host,
            args.to_vec(),
        )
    }

    fn builtin_set_frame_width_shared(&mut self, args: &[Value]) -> EvalResult {
        let (frames, buffers, display_host) = self.ctx.gui_frame_creation_state();
        crate::emacs_core::window_cmds::builtin_set_frame_width_in_state(
            frames,
            buffers,
            display_host,
            args.to_vec(),
        )
    }

    fn builtin_set_frame_size_shared(&mut self, args: &[Value]) -> EvalResult {
        let (frames, buffers, display_host) = self.ctx.gui_frame_creation_state();
        crate::emacs_core::window_cmds::builtin_set_frame_size_in_state(
            frames,
            buffers,
            display_host,
            args.to_vec(),
        )
    }

    fn builtin_yes_or_no_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::reader::finish_yes_or_no_p_in_vm_runtime(&mut self.ctx, &[], args)
    }
}

impl<'a> crate::emacs_core::builtins::symbols::MacroexpandRuntime for Vm<'a> {
    fn next_pcase_macroexpand_temp_symbol(&mut self) -> Value {
        self.ctx.next_pcase_macroexpand_temp_symbol()
    }

    fn resolve_indirect_symbol_by_id(&self, symbol: SymId) -> Option<(SymId, Value)> {
        crate::emacs_core::builtins::symbols::resolve_indirect_symbol_by_id_in_obarray(
            &self.ctx.obarray,
            symbol,
        )
    }

    fn is_global_function_placeholder(&self, symbol: SymId) -> bool {
        self.ctx.obarray.symbol_function_id(symbol).is_none()
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
    ) -> Result<Value, Flow> {
        let mut extra_roots = Vec::with_capacity(args.len() + 2);
        extra_roots.push(form);
        extra_roots.push(function);
        extra_roots.extend(args.iter().copied());
        self.with_extra_roots(&extra_roots, move |vm| {
            vm.with_macro_expansion_scope(|vm| vm.call_function(function, args))
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
        self.ctx.vm_gc_roots.push(value);
    }
}

// -- Arithmetic helpers --

/// Result of resolving a throw target, including any cleanup functions
/// from handler frames that were unwound through before reaching the target.
struct ThrowResolution {
    target: u32,
    stack_len: usize,
    spec_depth: usize,
    cleanups: Vec<Value>,
}

struct SignalResolution {
    target: u32,
    stack_len: usize,
    spec_depth: usize,
    cleanups: Vec<Value>,
}

fn resolve_throw_target(
    handlers: &mut Vec<Handler>,
    catch_tags: &mut Vec<Value>,
    tag: &Value,
) -> Option<ThrowResolution> {
    let cleanups = Vec::new();
    while let Some(handler) = handlers.pop() {
        match handler {
            Handler::Catch {
                tag: catch_tag,
                target,
                stack_len,
                spec_depth,
            } => {
                // Remove from evaluator catch_tags registry (this catch is being unwound).
                catch_tags.pop();
                if !tag.is_nil() && eq_value(&catch_tag, tag) {
                    return Some(ThrowResolution {
                        target,
                        stack_len,
                        spec_depth,
                        cleanups,
                    });
                }
            }
            _ => {}
        }
    }
    None
}

fn resolve_signal_target(
    handlers: &mut Vec<Handler>,
    catch_tags: &mut Vec<Value>,
    obarray: &Obarray,
    sig: &SignalData,
) -> Option<SignalResolution> {
    let cleanups = Vec::new();
    while let Some(handler) = handlers.pop() {
        match handler {
            Handler::Catch { .. } => {
                catch_tags.pop();
            }
            Handler::ConditionCase {
                conditions,
                target,
                stack_len,
                spec_depth,
            } => {
                if signal_matches_condition_value(obarray, sig.symbol_name(), &conditions) {
                    return Some(SignalResolution {
                        target,
                        stack_len,
                        spec_depth,
                        cleanups,
                    });
                }
            }
            Handler::UnwindProtect { .. } => {}
        }
    }
    None
}

fn normalize_vm_builtin_error(name: &str, flow: Flow) -> Flow {
    match flow {
        Flow::Signal(mut sig) if sig.symbol_name() == "wrong-number-of-arguments" => {
            if let Some(first) = sig.data.first_mut() {
                if matches!(first, Value::Symbol(id) if resolve_sym(*id) == name) {
                    *first = Value::Subr(intern(name));
                }
            }
            Flow::Signal(sig)
        }
        other => other,
    }
}

fn arith_add(vm: &Vm<'_>, a: &Value, b: &Value) -> EvalResult {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_add(*b))),
        _ => {
            let a = number_or_marker_as_f64(vm, a)?;
            let b = number_or_marker_as_f64(vm, b)?;
            Ok(Value::Float(a + b, next_float_id()))
        }
    }
}

fn arith_sub(vm: &Vm<'_>, a: &Value, b: &Value) -> EvalResult {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_sub(*b))),
        _ => {
            let a = number_or_marker_as_f64(vm, a)?;
            let b = number_or_marker_as_f64(vm, b)?;
            Ok(Value::Float(a - b, next_float_id()))
        }
    }
}

fn arith_mul(vm: &Vm<'_>, a: &Value, b: &Value) -> EvalResult {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_mul(*b))),
        _ => {
            let a = number_or_marker_as_f64(vm, a)?;
            let b = number_or_marker_as_f64(vm, b)?;
            Ok(Value::Float(a * b, next_float_id()))
        }
    }
}

fn arith_div(vm: &Vm<'_>, a: &Value, b: &Value) -> EvalResult {
    match (a, b) {
        (Value::Int(_), Value::Int(0)) => Err(signal(
            "arith-error",
            vec![Value::string("Division by zero")],
        )),
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a / b)),
        _ => {
            let a = number_or_marker_as_f64(vm, a)?;
            let b = number_or_marker_as_f64(vm, b)?;
            if b == 0.0 {
                return Err(signal(
                    "arith-error",
                    vec![Value::string("Division by zero")],
                ));
            }
            Ok(Value::Float(a / b, next_float_id()))
        }
    }
}

fn arith_rem(a: &Value, b: &Value) -> EvalResult {
    match (a, b) {
        (Value::Int(_), Value::Int(0)) => Err(signal(
            "arith-error",
            vec![Value::string("Division by zero")],
        )),
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a % b)),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *a],
        )),
    }
}

fn arith_add1(vm: &Vm<'_>, a: &Value) -> EvalResult {
    match a {
        Value::Int(n) => Ok(Value::Int(n.wrapping_add(1))),
        Value::Float(f, _) => Ok(Value::Float(f + 1.0, next_float_id())),
        marker if crate::emacs_core::marker::is_marker(marker) => Ok(Value::Int(
            crate::emacs_core::marker::marker_position_as_int_with_buffers(
                &vm.ctx.buffers,
                marker,
            )?
            .wrapping_add(1),
        )),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), *a],
        )),
    }
}

fn arith_sub1(vm: &Vm<'_>, a: &Value) -> EvalResult {
    match a {
        Value::Int(n) => Ok(Value::Int(n.wrapping_sub(1))),
        Value::Float(f, _) => Ok(Value::Float(f - 1.0, next_float_id())),
        marker if crate::emacs_core::marker::is_marker(marker) => Ok(Value::Int(
            crate::emacs_core::marker::marker_position_as_int_with_buffers(
                &vm.ctx.buffers,
                marker,
            )?
            .wrapping_sub(1),
        )),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), *a],
        )),
    }
}

fn arith_negate(vm: &Vm<'_>, a: &Value) -> EvalResult {
    match a {
        Value::Int(n) => Ok(Value::Int(-n)),
        Value::Float(f, _) => Ok(Value::Float(-f, next_float_id())),
        marker if crate::emacs_core::marker::is_marker(marker) => Ok(Value::Int(
            -crate::emacs_core::marker::marker_position_as_int_with_buffers(
                &vm.ctx.buffers,
                marker,
            )?,
        )),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), *a],
        )),
    }
}

fn num_eq(vm: &Vm<'_>, a: &Value, b: &Value) -> Result<bool, Flow> {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => Ok(a == b),
        _ => {
            let a = number_or_marker_as_f64(vm, a)?;
            let b = number_or_marker_as_f64(vm, b)?;
            Ok(a == b)
        }
    }
}

fn num_cmp(vm: &Vm<'_>, a: &Value, b: &Value) -> Result<i32, Flow> {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => Ok(a.cmp(b) as i32),
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
    match value {
        Value::Int(n) => Ok(*n as f64),
        Value::Float(f, _) => Ok(*f),
        Value::Char(c) => Ok(*c as u32 as f64),
        marker if crate::emacs_core::marker::is_marker(marker) => Ok(
            crate::emacs_core::marker::marker_position_as_int_with_buffers(&vm.ctx.buffers, marker)?
                as f64,
        ),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), *other],
        )),
    }
}

fn length_value(val: &Value) -> EvalResult {
    match val {
        Value::Nil => Ok(Value::Int(0)),
        Value::Str(id) => Ok(Value::Int(
            with_heap(|h| h.get_string(*id).chars().count()) as i64
        )),
        Value::Vector(v) => Ok(Value::Int(with_heap(|h| h.vector_len(*v)) as i64)),
        Value::Lambda(_) | Value::ByteCode(_) => {
            Ok(Value::Int(builtins::closure_vector_length(val).unwrap()))
        }
        Value::Cons(_) => {
            let mut len: i64 = 0;
            let mut cursor = *val;
            loop {
                match cursor {
                    Value::Cons(cell) => {
                        len += 1;
                        cursor = with_heap(|h| h.cons_cdr(cell));
                    }
                    Value::Nil => return Ok(Value::Int(len)),
                    tail => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("listp"), tail],
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
    let len = match array {
        Value::Str(id) => with_heap(|h| storage_char_len(h.get_string(*id))) as i64,
        Value::Vector(v) => with_heap(|h| h.vector_len(*v)) as i64,
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
            match value {
                Value::Int(i) => *i,
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

    match array {
        Value::Str(id) => {
            let s = with_heap(|h| h.get_string(*id).to_owned());
            let result = storage_substring(&s, start, end)
                .ok_or_else(|| signal("args-out-of-range", vec![*array, *from, *to]))?;
            Ok(Value::string(result))
        }
        Value::Vector(v) => {
            let data = with_heap(|h| h.get_vector(*v).clone());
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
