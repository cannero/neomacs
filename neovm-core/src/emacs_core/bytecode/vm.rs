//! Bytecode virtual machine — stack-based interpreter.

use std::collections::{HashMap, HashSet};

use super::chunk::ByteCodeFunction;
use super::opcode::Op;
use crate::buffer::{BufferId, BufferManager, InsertionType};
use crate::emacs_core::advice::VariableWatcherList;
use crate::emacs_core::builtins;
use crate::emacs_core::category::CategoryManager;
use crate::emacs_core::coding::CodingSystemManager;
use crate::emacs_core::custom::CustomManager;
use crate::emacs_core::error::*;
use crate::emacs_core::errors::signal_matches_condition_value;
use crate::emacs_core::eval::VmSharedState;
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
enum SavedRestriction {
    None {
        buffer_id: BufferId,
    },
    Markers {
        buffer_id: BufferId,
        beg_marker: u64,
        end_marker: u64,
    },
}

#[derive(Clone, Debug)]
enum VmUnwindEntry {
    DynamicBinding {
        name: String,
        restored_value: Value,
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
    Restriction(SavedRestriction),
}

/// The bytecode VM execution engine.
///
/// Operates on an Evaluator's obarray and dynamic binding stack.
pub struct Vm<'a> {
    shared: VmSharedState<'a>,
    /// Values that must remain GC-visible while the VM crosses into evaluator
    /// code that may trigger collection.
    gc_roots: Vec<Value>,
}

impl<'a> Vm<'a> {
    pub(crate) fn from_evaluator(eval: &'a mut crate::emacs_core::eval::Evaluator) -> Self {
        Self::new(VmSharedState::from_evaluator(eval))
    }

    pub(crate) fn new(shared: VmSharedState<'a>) -> Self {
        Self {
            shared,
            gc_roots: Vec::new(),
        }
    }

    /// Set the current depth and max_depth (inherited from the Evaluator).
    pub fn set_depth(&mut self, depth: usize, max_depth: usize) {
        *self.shared.depth = depth;
        *self.shared.max_depth = max_depth;
    }

    /// Get the current depth (to sync back to the Evaluator).
    pub fn get_depth(&self) -> usize {
        *self.shared.depth
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
        let saved_len = self.gc_roots.len();
        self.gc_roots.extend(func.constants.iter().copied());
        self.gc_roots.extend(stack.iter().copied());
        Self::collect_handler_roots(handlers, &mut self.gc_roots);
        Self::collect_specpdl_roots(specpdl, &mut self.gc_roots);
        self.gc_roots.extend(extra.iter().copied());
        let result = f(self);
        self.gc_roots.truncate(saved_len);
        result
    }

    fn with_extra_roots<T>(&mut self, extra: &[Value], f: impl FnOnce(&mut Self) -> T) -> T {
        let saved_len = self.gc_roots.len();
        self.gc_roots.extend(extra.iter().copied());
        let result = f(self);
        self.gc_roots.truncate(saved_len);
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
                VmUnwindEntry::CurrentBuffer { .. }
                | VmUnwindEntry::Excursion { .. }
                | VmUnwindEntry::Restriction(_) => {}
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
        *self.shared.depth += 1;
        if *self.shared.depth > *self.shared.max_depth {
            let overflow_depth = *self.shared.depth as i64;
            *self.shared.depth -= 1;
            return Err(signal(
                "excessive-lisp-nesting",
                vec![Value::Int(overflow_depth)],
            ));
        }

        let result = self.run_frame(func, args, func_value);
        *self.shared.depth -= 1;
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
            let first = if func_value.is_nil() {
                Value::cons(Value::Int(n_required as i64), Value::Int(nonrest as i64))
            } else {
                func_value
            };
            return Err(signal(
                "wrong-number-of-arguments",
                vec![first, Value::Int(nargs as i64)],
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
                    std::mem::replace(self.shared.lexenv, env)
                } else {
                    *self.shared.lexenv
                };
                for (sym_id, val) in frame.iter() {
                    if let Some(val) = val.as_value() {
                        *self.shared.lexenv = lexenv_prepend(*self.shared.lexenv, *sym_id, val);
                    }
                }
                let result = self.run_loop(func, &mut stack, &mut pc, &mut handlers, &mut specpdl);
                *self.shared.lexenv = saved_lexenv;
                let cleanup = self.unwind_specpdl_all(&mut specpdl);
                return merge_result_with_cleanup(result, cleanup);
            }

            self.shared.dynamic.push(frame);
            let result = self.run_loop(func, &mut stack, &mut pc, &mut handlers, &mut specpdl);
            self.shared.dynamic.pop();
            let cleanup = self.unwind_specpdl_all(&mut specpdl);
            return merge_result_with_cleanup(result, cleanup);
        }

        // No params: set up lexenv for lexical closures/functions, then run.
        let saved_lexenv = if let Some(env) = func.env {
            Some(std::mem::replace(self.shared.lexenv, env))
        } else if func.lexical {
            Some(*self.shared.lexenv)
        } else {
            None
        };

        let result = self.run_loop(func, &mut stack, &mut pc, &mut handlers, &mut specpdl);

        if let Some(old) = saved_lexenv {
            *self.shared.lexenv = old;
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
                        && !self.shared.obarray.is_constant_id(name_id)
                        && !self.shared.obarray.is_special_id(name_id)
                        && !crate::emacs_core::value::lexenv_declares_special(
                            *self.shared.lexenv,
                            name_id,
                        );
                    if lexical_bind {
                        let old_lexenv = *self.shared.lexenv;
                        *self.shared.lexenv = lexenv_prepend(*self.shared.lexenv, name_id, val);
                        specpdl.push(VmUnwindEntry::LexicalBinding {
                            name: name.clone(),
                            restored_value: old_value,
                            old_lexenv,
                        });
                    } else {
                        let mut frame = OrderedRuntimeBindingMap::new();
                        frame.insert(name_id, val);
                        self.shared.dynamic.push(frame);
                        specpdl.push(VmUnwindEntry::DynamicBinding {
                            name: name.clone(),
                            restored_value: old_value,
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
                        self.shared.buffers.current_buffer().map(|buffer| buffer.id)
                    {
                        specpdl.push(VmUnwindEntry::CurrentBuffer { buffer_id });
                    }
                }
                Op::SaveExcursion => {
                    if let Some((buffer_id, point)) = self
                        .shared
                        .buffers
                        .current_buffer()
                        .map(|buffer| (buffer.id, buffer.pt))
                    {
                        let marker_id = self.shared.buffers.create_marker(
                            buffer_id,
                            point,
                            InsertionType::Before,
                        );
                        specpdl.push(VmUnwindEntry::Excursion {
                            buffer_id,
                            marker_id,
                        });
                    }
                }
                Op::SaveRestriction => {
                    if let Some((buffer_id, begv, zv, len)) = self
                        .shared
                        .buffers
                        .current_buffer()
                        .map(|buffer| (buffer.id, buffer.begv, buffer.zv, buffer.text.len()))
                    {
                        let entry = if begv == 0 && zv == len {
                            VmUnwindEntry::Restriction(SavedRestriction::None { buffer_id })
                        } else {
                            let beg_marker = self.shared.buffers.create_marker(
                                buffer_id,
                                begv,
                                InsertionType::Before,
                            );
                            let end_marker = self.shared.buffers.create_marker(
                                buffer_id,
                                zv,
                                InsertionType::After,
                            );
                            VmUnwindEntry::Restriction(SavedRestriction::Markers {
                                buffer_id,
                                beg_marker,
                                end_marker,
                            })
                        };
                        specpdl.push(entry);
                    }
                }

                // -- Arithmetic --
                Op::Add => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(vm_try!(arith_add(self, &a, &b)));
                }
                Op::Sub => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(vm_try!(arith_sub(self, &a, &b)));
                }
                Op::Mul => {
                    let b = stack.pop().unwrap_or(Value::Int(1));
                    let a = stack.pop().unwrap_or(Value::Int(1));
                    stack.push(vm_try!(arith_mul(self, &a, &b)));
                }
                Op::Div => {
                    let b = stack.pop().unwrap_or(Value::Int(1));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(vm_try!(arith_div(self, &a, &b)));
                }
                Op::Rem => {
                    let b = stack.pop().unwrap_or(Value::Int(1));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(vm_try!(arith_rem(&a, &b)));
                }
                Op::Add1 => {
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(vm_try!(arith_add1(self, &a)));
                }
                Op::Sub1 => {
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(vm_try!(arith_sub1(self, &a)));
                }
                Op::Negate => {
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(vm_try!(arith_negate(self, &a)));
                }

                // -- Comparison --
                Op::Eqlsign => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(Value::bool(vm_try!(num_eq(self, &a, &b))));
                }
                Op::Gtr => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(Value::bool(vm_try!(num_cmp(self, &a, &b)) > 0));
                }
                Op::Lss => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(Value::bool(vm_try!(num_cmp(self, &a, &b)) < 0));
                }
                Op::Leq => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(Value::bool(vm_try!(num_cmp(self, &a, &b)) <= 0));
                }
                Op::Geq => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(Value::bool(vm_try!(num_cmp(self, &a, &b)) >= 0));
                }
                Op::Max => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(if vm_try!(num_cmp(self, &a, &b)) >= 0 {
                        a
                    } else {
                        b
                    });
                }
                Op::Min => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(if vm_try!(num_cmp(self, &a, &b)) <= 0 {
                        a
                    } else {
                        b
                    });
                }

                // -- List operations --
                Op::Car => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let result = vm_try!(self.dispatch_vm_builtin("car", vec![val]));
                    stack.push(result);
                }
                Op::Cdr => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let result = vm_try!(self.dispatch_vm_builtin("cdr", vec![val]));
                    stack.push(result);
                }
                Op::CarSafe => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    match val {
                        Value::Cons(cell) => {
                            let pair = read_cons(cell);
                            stack.push(pair.car);
                        }
                        // Closures are cons lists in official Emacs.
                        Value::Lambda(_) => {
                            let data = val.get_lambda_data().unwrap();
                            stack.push(if data.env.is_some() {
                                Value::symbol("closure")
                            } else {
                                Value::symbol("lambda")
                            });
                        }
                        _ => stack.push(Value::Nil),
                    }
                }
                Op::CdrSafe => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    match val {
                        Value::Cons(cell) => {
                            let pair = read_cons(cell);
                            stack.push(pair.cdr);
                        }
                        // Closures are cons lists in official Emacs.
                        Value::Lambda(_) => {
                            use crate::emacs_core::builtins::lambda_to_cons_list;
                            let list = lambda_to_cons_list(&val).unwrap_or(Value::Nil);
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
                Op::Cons => {
                    let cdr_val = stack.pop().unwrap_or(Value::Nil);
                    let car_val = stack.pop().unwrap_or(Value::Nil);
                    stack.push(Value::cons(car_val, cdr_val));
                }
                Op::List(n) => {
                    let n = *n as usize;
                    let start = stack.len().saturating_sub(n);
                    let items: Vec<Value> = stack.drain(start..).collect();
                    stack.push(Value::list(items));
                }
                Op::Length => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    stack.push(vm_try!(length_value(&val)));
                }
                Op::Nth => {
                    let list = stack.pop().unwrap_or(Value::Nil);
                    let n = stack.pop().unwrap_or(Value::Int(0));
                    let result = vm_try!(self.dispatch_vm_builtin("nth", vec![n, list]));
                    stack.push(result);
                }
                Op::Nthcdr => {
                    let list = stack.pop().unwrap_or(Value::Nil);
                    let n = stack.pop().unwrap_or(Value::Int(0));
                    let result = vm_try!(self.dispatch_vm_builtin("nthcdr", vec![n, list]));
                    stack.push(result);
                }
                Op::Elt => {
                    let idx = stack.pop().unwrap_or(Value::Nil);
                    let seq = stack.pop().unwrap_or(Value::Nil);
                    let result = vm_try!(self.dispatch_vm_builtin("elt", vec![seq, idx]));
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
                    let result = vm_try!(self.dispatch_vm_builtin("nconc", vec![a, b]));
                    stack.push(result);
                }
                Op::Nreverse => {
                    let list = stack.pop().unwrap_or(Value::Nil);
                    let result = vm_try!(self.dispatch_vm_builtin("nreverse", vec![list]));
                    stack.push(result);
                }
                Op::Member => {
                    let list = stack.pop().unwrap_or(Value::Nil);
                    let elt = stack.pop().unwrap_or(Value::Nil);
                    let result = vm_try!(self.dispatch_vm_builtin("member", vec![elt, list]));
                    stack.push(result);
                }
                Op::Memq => {
                    let list = stack.pop().unwrap_or(Value::Nil);
                    let elt = stack.pop().unwrap_or(Value::Nil);
                    let result = vm_try!(self.dispatch_vm_builtin("memq", vec![elt, list]));
                    stack.push(result);
                }
                Op::Assq => {
                    let alist = stack.pop().unwrap_or(Value::Nil);
                    let key = stack.pop().unwrap_or(Value::Nil);
                    let result = vm_try!(self.dispatch_vm_builtin("assq", vec![key, alist]));
                    stack.push(result);
                }

                // -- Type predicates --
                Op::Symbolp => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    stack.push(Value::bool(val.is_symbol()));
                }
                Op::Consp => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    stack.push(Value::bool(val.is_cons()));
                }
                Op::Stringp => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    stack.push(Value::bool(val.is_string()));
                }
                Op::Listp => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    stack.push(Value::bool(val.is_list()));
                }
                Op::Integerp => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    stack.push(Value::bool(val.is_integer()));
                }
                Op::Numberp => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    stack.push(Value::bool(val.is_number()));
                }
                Op::Null | Op::Not => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    stack.push(Value::bool(val.is_nil()));
                }
                Op::Eq => {
                    let b = stack.pop().unwrap_or(Value::Nil);
                    let a = stack.pop().unwrap_or(Value::Nil);
                    stack.push(Value::bool(eq_value(&a, &b)));
                }
                Op::Equal => {
                    let b = stack.pop().unwrap_or(Value::Nil);
                    let a = stack.pop().unwrap_or(Value::Nil);
                    stack.push(Value::bool(equal_value(&a, &b, 0)));
                }

                // -- String operations --
                Op::Concat(n) => {
                    let n = *n as usize;
                    let start = stack.len().saturating_sub(n);
                    let parts: Vec<Value> = stack.drain(start..).collect();
                    let result = vm_try!(self.dispatch_vm_builtin("concat", parts));
                    stack.push(result);
                }
                Op::Substring => {
                    let to = stack.pop().unwrap_or(Value::Nil);
                    let from = stack.pop().unwrap_or(Value::Int(0));
                    let array = stack.pop().unwrap_or(Value::Nil);
                    let result = vm_try!(substring_value(&array, &from, &to));
                    stack.push(result);
                }
                Op::StringEqual => {
                    let b = stack.pop().unwrap_or(Value::Nil);
                    let a = stack.pop().unwrap_or(Value::Nil);
                    let result = vm_try!(self.dispatch_vm_builtin("string=", vec![a, b]));
                    stack.push(result);
                }
                Op::StringLessp => {
                    let b = stack.pop().unwrap_or(Value::Nil);
                    let a = stack.pop().unwrap_or(Value::Nil);
                    let result = vm_try!(self.dispatch_vm_builtin("string-lessp", vec![a, b]));
                    stack.push(result);
                }

                // -- Vector operations --
                Op::Aref => {
                    let idx_val = stack.pop().unwrap_or(Value::Int(0));
                    let vec_val = stack.pop().unwrap_or(Value::Nil);
                    let result = vm_try!(builtins::builtin_aref(vec![vec_val, idx_val]));
                    stack.push(result);
                }
                Op::Aset => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let idx_val = stack.pop().unwrap_or(Value::Int(0));
                    let vec_val = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![vec_val, idx_val, val];
                    let result = vm_try!(builtins::builtin_aset(call_args.clone()));
                    self.maybe_writeback_mutating_first_arg(
                        "aset", None, &call_args, &result, stack,
                    );
                    stack.push(result);
                }

                // -- Symbol operations --
                Op::SymbolValue => {
                    let sym = stack.pop().unwrap_or(Value::Nil);
                    let result = vm_try!(self.dispatch_vm_builtin("symbol-value", vec![sym]));
                    stack.push(result);
                }
                Op::SymbolFunction => {
                    let sym = stack.pop().unwrap_or(Value::Nil);
                    let result = vm_try!(self.dispatch_vm_builtin("symbol-function", vec![sym]));
                    stack.push(result);
                }
                Op::Set => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let sym = stack.pop().unwrap_or(Value::Nil);
                    let result = vm_try!(self.dispatch_vm_builtin("set", vec![sym, val]));
                    stack.push(result);
                }
                Op::Fset => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let sym = stack.pop().unwrap_or(Value::Nil);
                    let result = vm_try!(self.dispatch_vm_builtin("fset", vec![sym, val]));
                    stack.push(result);
                }
                Op::Get => {
                    let prop = stack.pop().unwrap_or(Value::Nil);
                    let sym = stack.pop().unwrap_or(Value::Nil);
                    let result = vm_try!(self.dispatch_vm_builtin("get", vec![sym, prop]));
                    stack.push(result);
                }
                Op::Put => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let prop = stack.pop().unwrap_or(Value::Nil);
                    let sym = stack.pop().unwrap_or(Value::Nil);
                    let result = vm_try!(self.dispatch_vm_builtin("put", vec![sym, prop, val]));
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
                    self.shared.catch_tags.push(tag);
                }
                Op::PopHandler => {
                    if let Some(handler) = handlers.pop() {
                        match handler {
                            Handler::Catch { .. } => {
                                // Remove from evaluator's catch_tags registry.
                                self.shared.catch_tags.pop();
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
                        closure.env = Some(*self.shared.lexenv);
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
                    let result = vm_try!(self.dispatch_vm_builtin(&name, args));
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
                    self.shared
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
            let mut lexenv_val = *self.shared.lexenv;
            Self::replace_alias_refs_in_value(
                &mut lexenv_val,
                first_arg,
                &replacement,
                &mut visited,
            );
            *self.shared.lexenv = lexenv_val;
        }
        for frame in self.shared.dynamic.iter_mut() {
            for value in frame.values_mut() {
                Self::replace_alias_refs_in_value(value, first_arg, &replacement, &mut visited);
            }
        }
        if let Some(current_id) = self.shared.buffers.current_buffer_id()
            && let Some(buf) = self.shared.buffers.get_mut(current_id)
        {
            for value in buf.properties.values_mut() {
                if let RuntimeBindingValue::Bound(value) = value {
                    Self::replace_alias_refs_in_value(value, first_arg, &replacement, &mut visited);
                }
            }
        }

        let symbols: Vec<String> = self
            .shared
            .obarray
            .all_symbols()
            .into_iter()
            .map(str::to_string)
            .collect();
        for name in symbols {
            if let Some(symbol) = self.shared.obarray.get_mut(&name) {
                if let Some(value) = symbol.value.as_mut() {
                    Self::replace_alias_refs_in_value(value, first_arg, &replacement, &mut visited);
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
            &*self.shared.obarray,
            name_id,
        )?;
        let resolved_name = resolve_sym(resolved);
        let is_special = self.shared.obarray.is_special_id(name_id)
            && !self.shared.obarray.is_constant_id(name_id);
        let resolved_is_special = self.shared.obarray.is_special_id(resolved)
            && !self.shared.obarray.is_constant_id(resolved);
        let locally_special =
            crate::emacs_core::value::lexenv_declares_special(*self.shared.lexenv, name_id)
                || (resolved != name_id
                    && crate::emacs_core::value::lexenv_declares_special(
                        *self.shared.lexenv,
                        resolved,
                    ));

        // GNU Emacs resolves declared-special vars dynamically even when
        // lexical binding is active; the interpreter path already does this.
        if !is_special && !resolved_is_special && !locally_special {
            if let Some(val) = lexenv_lookup(*self.shared.lexenv, name_id) {
                return Ok(val);
            }
            if resolved != name_id
                && let Some(val) = lexenv_lookup(*self.shared.lexenv, resolved)
            {
                return Ok(val);
            }
        }

        // Check dynamic
        if let Some(binding) = lookup_runtime_binding(&self.shared.dynamic, name_id) {
            return binding
                .as_value()
                .ok_or_else(|| signal("void-variable", vec![Value::symbol(name)]));
        }
        if resolved != name_id
            && let Some(binding) = lookup_runtime_binding(&self.shared.dynamic, resolved)
        {
            return binding
                .as_value()
                .ok_or_else(|| signal("void-variable", vec![Value::symbol(name)]));
        }

        // Current buffer-local binding.
        if crate::emacs_core::builtins::is_canonical_symbol_id(resolved)
            && let Some(buf) = self.shared.buffers.current_buffer()
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
        if let Some(val) = self.shared.obarray.symbol_value_id(resolved) {
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
            &*self.shared.obarray,
            name_id,
        )?;
        let is_special = self.shared.obarray.is_special_id(name_id)
            && !self.shared.obarray.is_constant_id(name_id);
        let resolved_is_special = self.shared.obarray.is_special_id(resolved)
            && !self.shared.obarray.is_constant_id(resolved);
        let locally_special =
            crate::emacs_core::value::lexenv_declares_special(*self.shared.lexenv, name_id)
                || (resolved != name_id
                    && crate::emacs_core::value::lexenv_declares_special(
                        *self.shared.lexenv,
                        resolved,
                    ));

        if !is_special && !resolved_is_special && !locally_special {
            if let Some(cell_id) = lexenv_assq(*self.shared.lexenv, name_id) {
                lexenv_set(cell_id, value);
                return Ok(());
            }
            if resolved != name_id
                && let Some(cell_id) = lexenv_assq(*self.shared.lexenv, resolved)
            {
                lexenv_set(cell_id, value);
                return Ok(());
            }
        }

        // Check dynamic
        for frame in self.shared.dynamic.iter_mut().rev() {
            if frame.contains_key(&name_id) {
                frame.insert(name_id, value);
                return Ok(());
            }
            if resolved != name_id && frame.contains_key(&resolved) {
                frame.insert(resolved, value);
                return Ok(());
            }
        }

        if self.shared.obarray.is_constant_id(resolved) {
            return Err(signal("setting-constant", vec![Value::symbol(name)]));
        }

        crate::emacs_core::eval::set_runtime_binding_in_state(
            self.shared.obarray,
            self.shared.dynamic.as_mut_slice(),
            self.shared.buffers,
            &*self.shared.custom,
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
        if !self.shared.watchers.has_watchers(name) {
            return Ok(());
        }
        let calls = self.shared.watchers.notify_watchers(
            name,
            new_value,
            old_value,
            operation,
            where_value,
        );
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
                    &*self.shared.obarray,
                    self.shared.dynamic.as_slice(),
                    &*self.shared.buffers,
                    hook_name,
                )
                .unwrap_or(Value::Nil);
            let functions = crate::emacs_core::builtins::collect_hook_functions_in_state(
                &*self.shared.obarray,
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
                &*self.shared.obarray,
                self.shared.dynamic.as_slice(),
                &*self.shared.buffers,
                hook_name,
            )
            .unwrap_or(Value::Nil);
        let functions = crate::emacs_core::builtins::collect_hook_functions_in_state(
            &*self.shared.obarray,
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
                &*self.shared.obarray,
                self.shared.dynamic.as_slice(),
                &*self.shared.buffers,
                hook_name,
            )
            .unwrap_or(Value::Nil);
        let functions = crate::emacs_core::builtins::collect_hook_functions_in_state(
            &*self.shared.obarray,
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
                &*self.shared.obarray,
                self.shared.dynamic.as_slice(),
                &*self.shared.buffers,
                hook_name,
            )
            .unwrap_or(Value::Nil);
        let functions = crate::emacs_core::builtins::collect_hook_functions_in_state(
            &*self.shared.obarray,
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
                &*self.shared.obarray,
                self.shared.dynamic.as_slice(),
                &*self.shared.buffers,
                hook_name,
            )
            .unwrap_or(Value::Nil);
        let functions = crate::emacs_core::builtins::collect_hook_functions_in_state(
            &*self.shared.obarray,
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
                &*self.shared.obarray,
                self.shared.dynamic.as_slice(),
                &*self.shared.buffers,
                hook_name,
            )
            .unwrap_or(Value::Nil);
        let functions = crate::emacs_core::builtins::collect_hook_functions_in_state(
            &*self.shared.obarray,
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
            &*self.shared.obarray,
            symbol,
        )?;
        let value = args[1];
        if let Some(result) = crate::emacs_core::builtins::symbols::constant_set_outcome_in_obarray(
            &*self.shared.obarray,
            resolved,
            args[0],
            value,
        ) {
            return result;
        }
        let where_value = crate::emacs_core::eval::set_runtime_binding_in_state(
            self.shared.obarray,
            self.shared.dynamic.as_mut_slice(),
            self.shared.buffers,
            &*self.shared.custom,
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
        let result = crate::emacs_core::custom::builtin_set_default_in_obarray(
            self.shared.obarray,
            args.to_vec(),
        )?;
        let symbol = match args[0] {
            Value::Nil => intern("nil"),
            Value::True => intern("t"),
            Value::Symbol(id) | Value::Keyword(id) => id,
            _ => unreachable!("validated by builtin_set_default_in_obarray"),
        };
        let resolved = crate::emacs_core::builtins::symbols::resolve_variable_alias_id_in_obarray(
            &*self.shared.obarray,
            symbol,
        )?;
        let resolved_name = resolve_sym(resolved);
        let value = args[1];
        self.run_variable_watchers(resolved_name, &value, &Value::Nil, "set")?;
        if resolved != symbol {
            self.run_variable_watchers(resolved_name, &value, &Value::Nil, "set")?;
        }
        Ok(result)
    }

    fn builtin_set_default_toplevel_value_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::symbols::builtin_set_default_toplevel_value_in_obarray(
            self.shared.obarray,
            args.to_vec(),
        )?;
        let symbol = crate::emacs_core::builtins::symbols::expect_symbol_id(&args[0])?;
        let resolved = crate::emacs_core::builtins::symbols::resolve_variable_alias_id_in_obarray(
            &*self.shared.obarray,
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
        let plan =
            crate::emacs_core::builtins::plan_defalias_in_obarray(&*self.shared.obarray, args)?;
        let crate::emacs_core::builtins::DefaliasPlan {
            action,
            docstring,
            result,
        } = plan;
        match action {
            crate::emacs_core::builtins::DefaliasAction::SetFunction { symbol, definition } => {
                self.shared
                    .obarray
                    .set_symbol_function_id(symbol, definition);
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
                self.shared.obarray,
                vec![result, Value::symbol("function-documentation"), docstring],
            )?;
        }
        Ok(result)
    }

    fn builtin_defvaralias_shared(&mut self, args: &[Value]) -> EvalResult {
        let state_change = crate::emacs_core::builtins::symbols::builtin_defvaralias_in_state(
            self.shared.obarray,
            args.to_vec(),
        )?;
        self.run_variable_watchers(
            &state_change.previous_target,
            &state_change.base_variable,
            &Value::Nil,
            "defvaralias",
        )?;
        self.shared
            .watchers
            .clear_watchers(&state_change.alias_name);
        crate::emacs_core::builtins::symbols::builtin_put_in_obarray(
            self.shared.obarray,
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
            &*self.shared.obarray,
            symbol,
        )?;
        if self.shared.obarray.is_constant_id(resolved) {
            return Err(signal("setting-constant", vec![args[0]]));
        }
        crate::emacs_core::eval::makunbound_runtime_binding_in_state(
            self.shared.obarray,
            self.shared.dynamic.as_mut_slice(),
            self.shared.buffers,
            &*self.shared.custom,
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
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_local_variable_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::custom::builtin_local_variable_p_in_state(
            &*self.shared.obarray,
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_buffer_local_variables_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::custom::builtin_buffer_local_variables_in_state(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_kill_local_variable_shared(&mut self, args: &[Value]) -> EvalResult {
        let outcome = crate::emacs_core::custom::builtin_kill_local_variable_in_state(
            &*self.shared.obarray,
            self.shared.buffers,
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
            self.shared.frames,
            self.shared.buffers,
        )
    }

    fn resolve_frame_id(&mut self, arg: Option<&Value>, predicate: &str) -> Result<FrameId, Flow> {
        match arg {
            None | Some(Value::Nil) => Ok(self.ensure_selected_frame_id()),
            Some(Value::Int(n)) => {
                let fid = FrameId(*n as u64);
                if self.shared.frames.get(fid).is_some() {
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
                if self.shared.frames.get(fid).is_some() {
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
        if let Some(value) = self.shared.obarray.symbol_value("global-map").copied() {
            if crate::emacs_core::keymap::is_list_keymap(&value) {
                return value;
            }
        }
        let keymap = crate::emacs_core::keymap::make_list_keymap();
        self.shared.obarray.set_symbol_value("global-map", keymap);
        keymap
    }

    fn builtin_mapcar_fast(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("mapcar", args, 2)?;
        let func = args[0];
        let sequence = args[1];
        let saved_roots = self.gc_roots.len();
        self.gc_roots.push(func);
        self.gc_roots.push(sequence);

        let mut results = Vec::new();
        let map_result = crate::emacs_core::builtins::higher_order::for_each_sequence_element(
            &sequence,
            |item| {
                let value =
                    self.with_extra_roots(&[item], |vm| vm.call_function(func, vec![item]))?;
                results.push(value);
                self.gc_roots.push(value);
                Ok(())
            },
        );

        let out = match map_result {
            Ok(()) => self.with_extra_roots(&results, |_| Ok(Value::list(results.clone()))),
            Err(flow) => Err(flow),
        };
        self.gc_roots.truncate(saved_roots);
        out
    }

    fn builtin_frame_list_fast(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("frame-list", args, 0)?;
        let _ = self.ensure_selected_frame_id();
        let frames = self
            .shared
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
        let Some(frame) = self.shared.frames.get(FrameId(id)) else {
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
            .shared
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
        crate::emacs_core::builtins::symbols::builtin_fboundp_in_obarray(self.shared.obarray, args)
    }

    fn builtin_current_indentation_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::indent::builtin_current_indentation_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_indent_to_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::indent::builtin_indent_to_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_current_column_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::indent::builtin_current_column_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_buffer_string_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_string_in_manager(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_buffer_substring_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_substring_in_manager(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_field_beginning_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_field_beginning_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_field_end_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_field_end_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_field_string_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_field_string_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_field_string_no_properties_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_field_string_no_properties_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_constrain_to_field_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_constrain_to_field_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_point_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_point_in_manager(&*self.shared.buffers, args.to_vec())
    }

    fn builtin_buffer_list_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_list_in_manager(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_other_buffer_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_other_buffer_in_manager(
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_generate_new_buffer_name_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_generate_new_buffer_name_in_manager(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_get_file_buffer_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_get_file_buffer_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_make_indirect_buffer_shared(&mut self, args: &[Value]) -> EvalResult {
        let plan = crate::emacs_core::builtins::prepare_make_indirect_buffer_in_manager(
            &mut *self.shared.buffers,
            args.to_vec(),
        )?;
        if plan.run_clone_hook {
            self.shared.buffers.set_current(plan.id);
            let hook_value =
                crate::emacs_core::builtins::symbol_dynamic_buffer_or_global_value_in_state(
                    &*self.shared.obarray,
                    self.shared.dynamic.as_slice(),
                    &*self.shared.buffers,
                    "clone-indirect-buffer-hook",
                )
                .unwrap_or(Value::Nil);
            let functions = crate::emacs_core::builtins::collect_hook_functions_in_state(
                &*self.shared.obarray,
                "clone-indirect-buffer-hook",
                hook_value,
                true,
            );
            let clone_result = self.run_hook_functions(&functions, &[]);
            if let Some(saved_id) = plan.saved_current
                && self.shared.buffers.get(saved_id).is_some()
            {
                self.shared.buffers.set_current(saved_id);
            }
            clone_result?;
        }
        if plan.run_buffer_list_update_hook {
            let hook_value =
                crate::emacs_core::builtins::symbol_dynamic_buffer_or_global_value_in_state(
                    &*self.shared.obarray,
                    self.shared.dynamic.as_slice(),
                    &*self.shared.buffers,
                    "buffer-list-update-hook",
                )
                .unwrap_or(Value::Nil);
            let functions = crate::emacs_core::builtins::collect_hook_functions_in_state(
                &*self.shared.obarray,
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
            &mut *self.shared.buffers,
            &mut *self.shared.frames,
            args.to_vec(),
        )
    }

    fn builtin_current_active_maps_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::keymaps::builtin_current_active_maps_in_state(
            &mut *self.shared.obarray,
            self.shared.dynamic.as_slice(),
            *self.shared.current_local_map,
            args,
        )
    }

    fn builtin_current_minor_mode_maps_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::keymaps::builtin_current_minor_mode_maps_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
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
                    &*self.shared.obarray,
                    &args[1],
                )?,
            )
        } else {
            builtins::expect_args("map-keymap-internal", args, 2)?;
            (
                args[0],
                crate::emacs_core::builtins::keymaps::expect_keymap_in_obarray(
                    &*self.shared.obarray,
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

    fn builtin_command_remapping_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::interactive::builtin_command_remapping_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &*self.shared.buffers,
            *self.shared.current_local_map,
            args.to_vec(),
        )
    }

    fn builtin_key_binding_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::interactive::builtin_key_binding_in_state(
            &mut *self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &*self.shared.buffers,
            *self.shared.current_local_map,
            args.to_vec(),
        )
    }

    fn builtin_local_key_binding_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::interactive::builtin_local_key_binding_in_state(
            *self.shared.current_local_map,
            args.to_vec(),
        )
    }

    fn builtin_minor_mode_key_binding_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::interactive::builtin_minor_mode_key_binding_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            args.to_vec(),
        )
    }

    fn builtin_set_buffer_multibyte_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_set_buffer_multibyte_in_manager(
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_insert_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_insert_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_barf_if_buffer_read_only_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_barf_if_buffer_read_only_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_insert_and_inherit_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_insert_and_inherit_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_insert_before_markers_and_inherit_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_insert_before_markers_and_inherit_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_point_min_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_point_min_in_manager(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_point_max_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_point_max_in_manager(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_goto_char_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_goto_char_in_manager(
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_char_after_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_char_after_in_manager(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_char_before_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_char_before_in_manager(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_buffer_size_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_size_in_manager(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_byte_to_position_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_byte_to_position_in_manager(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_position_bytes_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_position_bytes_in_manager(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_get_byte_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_get_byte_in_manager(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_narrow_to_region_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_narrow_to_region_in_manager(
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_widen_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_widen_in_manager(
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_buffer_modified_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_modified_p_in_manager(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_set_buffer_modified_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_set_buffer_modified_p_in_manager(
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_buffer_modified_tick_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_modified_tick_in_manager(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_buffer_chars_modified_tick_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_chars_modified_tick_in_manager(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_insert_char_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_insert_char_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_insert_byte_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_insert_byte_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_subst_char_in_region_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_subst_char_in_region_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_bobp_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_bobp_in_manager(&*self.shared.buffers, args.to_vec())
    }

    fn builtin_eobp_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_eobp_in_manager(&*self.shared.buffers, args.to_vec())
    }

    fn builtin_bolp_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_bolp_in_manager(&*self.shared.buffers, args.to_vec())
    }

    fn builtin_eolp_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_eolp_in_manager(&*self.shared.buffers, args.to_vec())
    }

    fn builtin_line_beginning_position_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_line_beginning_position_in_manager(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_line_end_position_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_line_end_position_in_manager(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_insert_before_markers_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_insert_before_markers_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_insert_buffer_substring_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_insert_buffer_substring_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_replace_region_contents_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_replace_region_contents_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_delete_char_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_delete_char_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_buffer_substring_no_properties_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_buffer_substring_no_properties_in_state(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_following_char_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_following_char_in_state(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_preceding_char_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_preceding_char_in_state(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_delete_region_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_delete_region_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_compare_buffer_substrings_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_compare_buffer_substrings_in_state(
            self.case_fold_search_enabled(),
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_delete_field_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_delete_field_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_delete_and_extract_region_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_delete_and_extract_region_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_erase_buffer_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::editfns::builtin_erase_buffer_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_buffer_enable_undo_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_enable_undo_in_manager(
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_buffer_disable_undo_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_disable_undo_in_manager(
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_kill_all_local_variables_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_kill_all_local_variables_in_state(
            &*self.shared.obarray,
            &mut *self.shared.buffers,
            self.shared.current_local_map,
            args.to_vec(),
        )
    }

    fn builtin_buffer_local_value_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::builtin_buffer_local_value_in_state(
            &*self.shared.obarray,
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_local_variable_if_set_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::symbols::builtin_local_variable_if_set_p_in_state(
            &*self.shared.obarray,
            &*self.shared.custom,
            args.to_vec(),
        )
    }

    fn builtin_variable_binding_locus_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::symbols::builtin_variable_binding_locus_in_state(
            &*self.shared.obarray,
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_move_to_column_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::indent::builtin_move_to_column_in_state(
            &*self.shared.obarray,
            self.shared.dynamic.as_slice(),
            &mut *self.shared.buffers,
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
            &mut *self.shared.buffers,
            self.shared.match_data,
            args,
        )
    }

    fn builtin_search_backward_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_search_backward_with_state(
            self.case_fold_search_enabled(),
            &mut *self.shared.buffers,
            self.shared.match_data,
            args,
        )
    }

    fn builtin_re_search_forward_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_re_search_forward_with_state(
            self.case_fold_search_enabled(),
            &mut *self.shared.buffers,
            self.shared.match_data,
            args,
        )
    }

    fn builtin_re_search_backward_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_re_search_backward_with_state(
            self.case_fold_search_enabled(),
            &mut *self.shared.buffers,
            self.shared.match_data,
            args,
        )
    }

    fn builtin_search_forward_regexp_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_search_forward_regexp_with_state(
            self.case_fold_search_enabled(),
            &mut *self.shared.buffers,
            self.shared.match_data,
            args,
        )
    }

    fn builtin_search_backward_regexp_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_search_backward_regexp_with_state(
            self.case_fold_search_enabled(),
            &mut *self.shared.buffers,
            self.shared.match_data,
            args,
        )
    }

    fn builtin_looking_at_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_looking_at_with_state(
            self.case_fold_search_enabled(),
            &*self.shared.buffers,
            self.shared.match_data,
            args,
        )
    }

    fn builtin_looking_at_p_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_looking_at_p_with_state(
            self.case_fold_search_enabled(),
            &*self.shared.buffers,
            args,
        )
    }

    fn builtin_posix_looking_at_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_posix_looking_at_with_state(
            self.case_fold_search_enabled(),
            &*self.shared.buffers,
            self.shared.match_data,
            args,
        )
    }

    fn builtin_posix_string_match_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_posix_string_match_with_state(
            self.case_fold_search_enabled(),
            self.shared.match_data,
            args,
        )
    }

    fn builtin_match_data_translate_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_match_data_translate_with_state(
            self.shared.match_data,
            args,
        )
    }

    fn builtin_replace_match_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::builtins::search::builtin_replace_match_with_state(
            &mut *self.shared.buffers,
            self.shared.match_data,
            args,
        )
    }

    fn builtin_find_charset_region_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::charset::builtin_find_charset_region_in_manager(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_charset_after_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::charset::builtin_charset_after_in_manager(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_compose_region_internal_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::composite::builtin_compose_region_internal_in_manager(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_interactive_form_shared(&mut self, args: &[Value]) -> EvalResult {
        builtins::expect_args("interactive-form", args, 1)?;
        let mut target = args[0];
        loop {
            match crate::emacs_core::builtins::symbols::plan_interactive_form_in_state(
                &*self.shared.obarray,
                &*self.shared.interactive,
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
                    target = self.autoload_do_load_with_vm_bridge(load_args, &extra_roots)?;
                }
            }
        }
    }

    fn builtin_skip_chars_forward_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_skip_chars_forward_in_manager(
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_skip_chars_backward_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::navigation::builtin_skip_chars_backward_in_manager(
            &mut *self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_scan_lists_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::syntax::builtin_scan_lists_in_manager(
            &*self.shared.buffers,
            args.to_vec(),
        )
    }

    fn builtin_scan_sexps_shared(&mut self, args: &[Value]) -> EvalResult {
        crate::emacs_core::syntax::builtin_scan_sexps_in_manager(
            &*self.shared.buffers,
            args.to_vec(),
        )
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
                let call_state = self
                    .shared
                    .begin_lambda_call(&lambda_data, &args, func_val)?;
                let body = lambda_data.body.clone();
                let result = self.with_mirrored_evaluator(&extra_roots, move |eval| {
                    eval.eval_lambda_body(&body)
                });
                self.shared.finish_lambda_call(call_state);
                result
            }
            Value::Subr(id) => self.dispatch_vm_builtin(resolve_sym(id), args),
            Value::Symbol(id) => {
                let name = resolve_sym(id);
                // Try obarray function cell
                if let Some(func) = self.shared.obarray.symbol_function(name).cloned() {
                    if func.is_nil() {
                        if builtins::builtin_registry::is_dispatch_builtin_name(name)
                            || builtins::is_pure_builtin_name(name)
                        {
                            return self.dispatch_vm_builtin(name, args);
                        }
                        return Err(signal("void-function", vec![Value::symbol(name)]));
                    }
                    if crate::emacs_core::autoload::is_autoload_value(&func) {
                        let mut autoload_roots = Vec::with_capacity(args.len() + 2);
                        autoload_roots.push(Value::Symbol(id));
                        autoload_roots.push(func);
                        autoload_roots.extend(args.iter().copied());
                        let loaded = self.autoload_do_load_with_vm_bridge(
                            vec![func, Value::Symbol(id)],
                            &autoload_roots,
                        )?;
                        return self.call_function(loaded, args);
                    }
                    return self.call_function(func, args);
                }
                // Try builtin
                self.dispatch_vm_builtin(name, args)
            }
            _ => Err(signal("invalid-function", vec![func_val])),
        }
    }

    fn autoload_do_load_with_vm_bridge(
        &mut self,
        args: Vec<Value>,
        extra_roots: &[Value],
    ) -> EvalResult {
        match crate::emacs_core::autoload::plan_autoload_do_load_in_state(
            &*self.shared.obarray,
            &args,
        )? {
            crate::emacs_core::autoload::AutoloadDoLoadPlan::Return(value) => Ok(value),
            crate::emacs_core::autoload::AutoloadDoLoadPlan::Load { file, funname } => {
                let path = crate::emacs_core::autoload::resolve_autoload_load_path(
                    &*self.shared.obarray,
                    &file,
                )?;
                self.with_mirrored_evaluator(extra_roots, move |eval| {
                    eval.load_file_internal(&path)
                })?;
                crate::emacs_core::autoload::finish_autoload_do_load_in_state(
                    &*self.shared.obarray,
                    funname.as_deref(),
                )
            }
        }
    }

    fn require_with_vm_bridge(&mut self, args: Vec<Value>, extra_roots: &[Value]) -> EvalResult {
        match crate::emacs_core::eval::plan_require_in_state(
            &*self.shared.obarray,
            &*self.shared.features,
            &*self.shared.require_stack,
            args.first().copied().unwrap_or(Value::Nil),
            args.get(1).copied(),
            args.get(2).copied(),
        )? {
            crate::emacs_core::eval::RequirePlan::Return(value) => Ok(value),
            crate::emacs_core::eval::RequirePlan::Load { sym_id, name, path } => {
                self.shared.require_stack.push(sym_id);
                let result = self.with_mirrored_evaluator(extra_roots, move |eval| {
                    eval.load_file_internal(&path)
                });
                let _ = self.shared.require_stack.pop();
                result?;
                crate::emacs_core::eval::finish_require_in_state(
                    &*self.shared.features,
                    sym_id,
                    &name,
                )
            }
        }
    }

    fn load_with_vm_bridge(&mut self, args: Vec<Value>, extra_roots: &[Value]) -> EvalResult {
        if args.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("load"), Value::Int(0)],
            ));
        }
        match crate::emacs_core::load::plan_load_in_state(
            &*self.shared.obarray,
            args[0],
            args.get(1).copied(),
            args.get(3).copied(),
            args.get(4).copied(),
        )? {
            crate::emacs_core::load::LoadPlan::Return(value) => Ok(value),
            crate::emacs_core::load::LoadPlan::Load { path } => self
                .with_mirrored_evaluator(extra_roots, move |eval| eval.load_file_internal(&path)),
        }
    }

    fn eval_with_vm_bridge(&mut self, args: Vec<Value>, extra_roots: &[Value]) -> EvalResult {
        if !(1..=2).contains(&args.len()) {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("eval"), Value::Int(args.len() as i64)],
            ));
        }
        let form = args[0];
        let lexical_arg = args.get(1).copied();
        self.with_mirrored_evaluator(extra_roots, move |eval| {
            eval.eval_value_with_lexical_arg(form, lexical_arg)
        })
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
                if let Some(res) = resolve_throw_target(handlers, &mut self.shared.catch_tags, &tag)
                {
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
                if !tag.is_nil()
                    && self
                        .shared
                        .catch_tags
                        .iter()
                        .rev()
                        .any(|t| eq_value(t, &tag))
                {
                    return Err(Flow::Throw { tag, value });
                }
                Err(signal("no-catch", vec![tag, value]))
            }
            Flow::Signal(sig) => {
                if let Some(res) = resolve_signal_target(
                    handlers,
                    &mut self.shared.catch_tags,
                    self.shared.obarray,
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
            } => {
                self.shared.dynamic.pop();
                self.run_variable_watchers(&name, &restored_value, &Value::Nil, "unlet")?;
            }
            VmUnwindEntry::LexicalBinding {
                name,
                restored_value,
                old_lexenv,
            } => {
                *self.shared.lexenv = old_lexenv;
                self.run_variable_watchers(&name, &restored_value, &Value::Nil, "unlet")?;
            }
            VmUnwindEntry::Cleanup { cleanup } => {
                let cleanup_root = [cleanup];
                self.with_extra_roots(&cleanup_root, |vm| vm.call_function(cleanup, vec![]))?;
            }
            VmUnwindEntry::CurrentBuffer { buffer_id } => {
                self.shared.buffers.set_current(buffer_id);
            }
            VmUnwindEntry::Excursion {
                buffer_id,
                marker_id,
            } => {
                if self.shared.buffers.get(buffer_id).is_some() {
                    self.shared.buffers.set_current(buffer_id);
                    if let Some(saved_pt) =
                        self.shared.buffers.marker_position(buffer_id, marker_id)
                    {
                        let _ = self.shared.buffers.goto_buffer_byte(buffer_id, saved_pt);
                    }
                }
                self.shared.buffers.remove_marker(marker_id);
            }
            VmUnwindEntry::Restriction(saved) => self.restore_saved_restriction(saved),
        }
        Ok(())
    }

    fn restore_saved_restriction(&mut self, saved: SavedRestriction) {
        match saved {
            SavedRestriction::None { buffer_id } => {
                if let Some(len) = self
                    .shared
                    .buffers
                    .get(buffer_id)
                    .map(|buffer| buffer.text.len())
                {
                    let _ = self
                        .shared
                        .buffers
                        .restore_buffer_restriction(buffer_id, 0, len);
                }
            }
            SavedRestriction::Markers {
                buffer_id,
                beg_marker,
                end_marker,
            } => {
                let beg = self.shared.buffers.marker_position(buffer_id, beg_marker);
                let end = self.shared.buffers.marker_position(buffer_id, end_marker);
                if let (Some(begv), Some(zv), Some(len)) = (
                    beg,
                    end,
                    self.shared
                        .buffers
                        .get(buffer_id)
                        .map(|buffer| buffer.text.len()),
                ) {
                    let mut restored_begv = begv.min(len);
                    let mut restored_zv = zv.min(len);
                    if restored_begv > restored_zv {
                        std::mem::swap(&mut restored_begv, &mut restored_zv);
                    }
                    let _ = self.shared.buffers.restore_buffer_restriction(
                        buffer_id,
                        restored_begv,
                        restored_zv,
                    );
                }
                self.shared.buffers.remove_marker(beg_marker);
                self.shared.buffers.remove_marker(end_marker);
            }
        }
    }

    /// Dispatch to builtin functions from the VM.
    fn dispatch_vm_builtin(&mut self, name: &str, args: Vec<Value>) -> EvalResult {
        if let Some(result) = self.dispatch_vm_builtin_fast(name, &args) {
            return result;
        }

        // Handle special VM builtins
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
            "%%defvar" => {
                // args: [init_value, symbol_name]
                if args.len() >= 2 {
                    let sym_name = args[1].as_symbol_name().unwrap_or("nil").to_string();
                    if !self.shared.obarray.boundp(&sym_name) {
                        self.shared.obarray.set_symbol_value(&sym_name, args[0]);
                    }
                    self.shared.obarray.make_special(&sym_name);
                    return Ok(Value::symbol(sym_name));
                }
                return Ok(Value::Nil);
            }
            "%%defconst" => {
                if args.len() >= 2 {
                    let sym_name = args[1].as_symbol_name().unwrap_or("nil").to_string();
                    self.shared.obarray.set_symbol_value(&sym_name, args[0]);
                    let sym = self.shared.obarray.get_or_intern(&sym_name);
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
                if !tag.is_nil()
                    && self
                        .shared
                        .catch_tags
                        .iter()
                        .rev()
                        .any(|t| eq_value(t, &tag))
                {
                    return Err(Flow::Throw { tag, value });
                }
                return Err(signal("no-catch", vec![tag, value]));
            }
            _ => {}
        }

        // Create a temporary evaluator for builtin dispatch
        // This is a bridge: builtins that don't need the evaluator work fine,
        // those that do will need the evaluator reference.
        if let Some(result) = builtins::dispatch_builtin_pure(name, args.clone()) {
            return result.map_err(|flow| normalize_vm_builtin_error(name, flow));
        }
        if let Some(result) = self.dispatch_vm_builtin_eval(name, args.clone()) {
            return result.map_err(|flow| normalize_vm_builtin_error(name, flow));
        }

        Err(signal("void-function", vec![Value::symbol(name)]))
    }

    fn dispatch_vm_builtin_fast(&mut self, name: &str, args: &[Value]) -> Option<EvalResult> {
        match name {
            "make-sparse-keymap" => Some(
                builtins::expect_max_args("make-sparse-keymap", args, 1)
                    .map(|_| crate::emacs_core::keymap::make_sparse_list_keymap()),
            ),
            "make-keymap" => Some(
                crate::emacs_core::builtins::keymaps::builtin_make_keymap_pure(args),
            ),
            "modify-category-entry" => Some(
                crate::emacs_core::category::modify_category_entry_in_manager(
                    self.shared.category_manager,
                    args,
                ),
            ),
            "modify-syntax-entry" => Some(
                crate::emacs_core::syntax::modify_syntax_entry_in_buffers(self.shared.buffers, args),
            ),
            "syntax-table" => Some(crate::emacs_core::syntax::builtin_syntax_table_in_buffers(
                self.shared.buffers,
                args.to_vec(),
            )),
            "set-syntax-table" => Some(
                crate::emacs_core::syntax::builtin_set_syntax_table_in_buffers(
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "char-syntax" => Some(crate::emacs_core::syntax::builtin_char_syntax_in_buffers(
                &*self.shared.buffers,
                args.to_vec(),
            )),
            "syntax-after" => Some(crate::emacs_core::syntax::builtin_syntax_after_in_buffers(
                &*self.shared.buffers,
                args.to_vec(),
            )),
            "matching-paren" => Some(
                crate::emacs_core::syntax::builtin_matching_paren_in_buffers(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "forward-comment" => Some(
                crate::emacs_core::syntax::builtin_forward_comment_in_buffers(
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "backward-prefix-chars" => Some(
                crate::emacs_core::syntax::builtin_backward_prefix_chars_in_buffers(
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "forward-word" => Some(crate::emacs_core::syntax::builtin_forward_word_in_buffers(
                self.shared.buffers,
                args.to_vec(),
            )),
            "skip-syntax-forward" => Some(
                crate::emacs_core::syntax::builtin_skip_syntax_forward_in_buffers(
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "skip-syntax-backward" => Some(
                crate::emacs_core::syntax::builtin_skip_syntax_backward_in_buffers(
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "decode-char" => Some(crate::emacs_core::charset::builtin_decode_char(args.to_vec())),
            "encode-char" => Some(crate::emacs_core::charset::builtin_encode_char(args.to_vec())),
            "set-char-table-range" => Some(
                crate::emacs_core::chartable::builtin_set_char_table_range(args.to_vec()),
            ),
            "char-table-extra-slot" => Some(
                crate::emacs_core::chartable::builtin_char_table_extra_slot(args.to_vec()),
            ),
            "set-char-table-extra-slot" => Some(
                crate::emacs_core::chartable::builtin_set_char_table_extra_slot(args.to_vec()),
            ),
            "current-indentation" => Some(self.builtin_current_indentation_shared(args)),
            "indent-to" => Some(self.builtin_indent_to_shared(args)),
            "current-column" => Some(self.builtin_current_column_shared(args)),
            "move-to-column" => Some(self.builtin_move_to_column_shared(args)),
            "insert" => Some(self.builtin_insert_shared(args)),
            "barf-if-buffer-read-only" => Some(self.builtin_barf_if_buffer_read_only_shared(args)),
            "insert-and-inherit" => Some(self.builtin_insert_and_inherit_shared(args)),
            "insert-before-markers-and-inherit" => {
                Some(self.builtin_insert_before_markers_and_inherit_shared(args))
            }
            "buffer-string" => Some(self.builtin_buffer_string_shared(args)),
            "buffer-substring" => Some(self.builtin_buffer_substring_shared(args)),
            "point" => Some(self.builtin_point_shared(args)),
            "buffer-list" => Some(self.builtin_buffer_list_shared(args)),
            "other-buffer" => Some(self.builtin_other_buffer_shared(args)),
            "generate-new-buffer-name" => Some(self.builtin_generate_new_buffer_name_shared(args)),
            "get-file-buffer" => Some(self.builtin_get_file_buffer_shared(args)),
            "make-indirect-buffer" => Some(self.builtin_make_indirect_buffer_shared(args)),
            "point-min" => Some(self.builtin_point_min_shared(args)),
            "point-max" => Some(self.builtin_point_max_shared(args)),
            "goto-char" => Some(self.builtin_goto_char_shared(args)),
            "field-beginning" => Some(self.builtin_field_beginning_shared(args)),
            "field-end" => Some(self.builtin_field_end_shared(args)),
            "field-string" => Some(self.builtin_field_string_shared(args)),
            "field-string-no-properties" => {
                Some(self.builtin_field_string_no_properties_shared(args))
            }
            "constrain-to-field" => Some(self.builtin_constrain_to_field_shared(args)),
            "char-after" => Some(self.builtin_char_after_shared(args)),
            "char-before" => Some(self.builtin_char_before_shared(args)),
            "buffer-size" => Some(self.builtin_buffer_size_shared(args)),
            "byte-to-position" => Some(self.builtin_byte_to_position_shared(args)),
            "position-bytes" => Some(self.builtin_position_bytes_shared(args)),
            "get-byte" => Some(self.builtin_get_byte_shared(args)),
            "narrow-to-region" => Some(self.builtin_narrow_to_region_shared(args)),
            "widen" => Some(self.builtin_widen_shared(args)),
            "buffer-modified-p" => Some(self.builtin_buffer_modified_p_shared(args)),
            "set-buffer-modified-p" => Some(self.builtin_set_buffer_modified_p_shared(args)),
            "buffer-modified-tick" => Some(self.builtin_buffer_modified_tick_shared(args)),
            "buffer-chars-modified-tick" => {
                Some(self.builtin_buffer_chars_modified_tick_shared(args))
            }
            "insert-char" => Some(self.builtin_insert_char_shared(args)),
            "insert-byte" => Some(self.builtin_insert_byte_shared(args)),
            "subst-char-in-region" => Some(self.builtin_subst_char_in_region_shared(args)),
            "bobp" => Some(self.builtin_bobp_shared(args)),
            "eobp" => Some(self.builtin_eobp_shared(args)),
            "bolp" => Some(self.builtin_bolp_shared(args)),
            "eolp" => Some(self.builtin_eolp_shared(args)),
            "line-beginning-position" => Some(self.builtin_line_beginning_position_shared(args)),
            "line-end-position" => Some(self.builtin_line_end_position_shared(args)),
            "pos-bol" => Some(crate::emacs_core::builtins::builtin_pos_bol_in_buffers(
                &*self.shared.buffers,
                args.to_vec(),
            )),
            "pos-eol" => Some(crate::emacs_core::builtins::builtin_pos_eol_in_buffers(
                &*self.shared.buffers,
                args.to_vec(),
            )),
            "forward-line" => Some(
                crate::emacs_core::navigation::builtin_forward_line_in_manager(
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "beginning-of-line" => Some(
                crate::emacs_core::navigation::builtin_beginning_of_line_in_manager(
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "end-of-line" => Some(crate::emacs_core::navigation::builtin_end_of_line_in_manager(
                self.shared.buffers,
                args.to_vec(),
            )),
            "forward-char" => Some(crate::emacs_core::navigation::builtin_forward_char_in_manager(
                self.shared.buffers,
                args.to_vec(),
            )),
            "backward-char" => Some(
                crate::emacs_core::navigation::builtin_backward_char_in_manager(
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "insert-before-markers" => Some(self.builtin_insert_before_markers_shared(args)),
            "insert-buffer-substring" => Some(self.builtin_insert_buffer_substring_shared(args)),
            "delete-char" => Some(self.builtin_delete_char_shared(args)),
            "buffer-substring-no-properties" => {
                Some(self.builtin_buffer_substring_no_properties_shared(args))
            }
            "following-char" => Some(self.builtin_following_char_shared(args)),
            "preceding-char" => Some(self.builtin_preceding_char_shared(args)),
            "delete-region" => Some(self.builtin_delete_region_shared(args)),
            "delete-and-extract-region" => {
                Some(self.builtin_delete_and_extract_region_shared(args))
            }
            "compare-buffer-substrings" => {
                Some(self.builtin_compare_buffer_substrings_shared(args))
            }
            "replace-region-contents" => Some(self.builtin_replace_region_contents_shared(args)),
            "delete-field" => Some(self.builtin_delete_field_shared(args)),
            "erase-buffer" => Some(self.builtin_erase_buffer_shared(args)),
            "buffer-enable-undo" => Some(self.builtin_buffer_enable_undo_shared(args)),
            "buffer-disable-undo" => Some(self.builtin_buffer_disable_undo_shared(args)),
            "kill-all-local-variables" => Some(self.builtin_kill_all_local_variables_shared(args)),
            "buffer-local-value" => Some(self.builtin_buffer_local_value_shared(args)),
            "local-variable-if-set-p" => Some(self.builtin_local_variable_if_set_p_shared(args)),
            "variable-binding-locus" => Some(self.builtin_variable_binding_locus_shared(args)),
            "region-beginning" => Some(
                crate::emacs_core::navigation::builtin_region_beginning_in_manager(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "region-end" => Some(crate::emacs_core::navigation::builtin_region_end_in_manager(
                &*self.shared.buffers,
                args.to_vec(),
            )),
            "vertical-motion" => Some(
                crate::emacs_core::builtins::symbols::builtin_vertical_motion_in_buffers(
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "skip-chars-forward" => Some(self.builtin_skip_chars_forward_shared(args)),
            "skip-chars-backward" => Some(self.builtin_skip_chars_backward_shared(args)),
            "scan-lists" => Some(self.builtin_scan_lists_shared(args)),
            "scan-sexps" => Some(self.builtin_scan_sexps_shared(args)),
            "parse-partial-sexp" => Some(
                crate::emacs_core::syntax::builtin_parse_partial_sexp_in_manager(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "next-overlay-change" => Some(
                crate::emacs_core::textprop::builtin_next_overlay_change_in_buffers(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "previous-overlay-change" => Some(
                crate::emacs_core::textprop::builtin_previous_overlay_change_in_buffers(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "make-overlay" => Some(crate::emacs_core::textprop::builtin_make_overlay_in_buffers(
                self.shared.buffers,
                args.to_vec(),
            )),
            "delete-overlay" => Some(
                crate::emacs_core::textprop::builtin_delete_overlay_in_buffers(
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "overlay-put" => Some(crate::emacs_core::textprop::builtin_overlay_put_in_buffers(
                self.shared.buffers,
                args.to_vec(),
            )),
            "overlay-get" => Some(crate::emacs_core::textprop::builtin_overlay_get_in_buffers(
                &*self.shared.buffers,
                args.to_vec(),
            )),
            "overlayp" => Some(crate::emacs_core::textprop::builtin_overlayp_pure(args.to_vec())),
            "overlays-at" => Some(crate::emacs_core::textprop::builtin_overlays_at_in_buffers(
                &*self.shared.buffers,
                args.to_vec(),
            )),
            "overlays-in" => Some(crate::emacs_core::textprop::builtin_overlays_in_in_buffers(
                &*self.shared.buffers,
                args.to_vec(),
            )),
            "move-overlay" => Some(crate::emacs_core::textprop::builtin_move_overlay_in_buffers(
                self.shared.buffers,
                args.to_vec(),
            )),
            "overlay-start" => Some(
                crate::emacs_core::textprop::builtin_overlay_start_in_buffers(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "overlay-end" => Some(crate::emacs_core::textprop::builtin_overlay_end_in_buffers(
                &*self.shared.buffers,
                args.to_vec(),
            )),
            "overlay-buffer" => Some(
                crate::emacs_core::textprop::builtin_overlay_buffer_in_buffers(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "overlay-properties" => Some(
                crate::emacs_core::textprop::builtin_overlay_properties_in_buffers(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "delete-all-overlays" => Some(
                crate::emacs_core::builtins::builtin_delete_all_overlays_in_manager(
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "put-text-property" => Some(
                crate::emacs_core::textprop::builtin_put_text_property_in_buffers(
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "get-text-property" => Some(
                crate::emacs_core::textprop::builtin_get_text_property_in_buffers(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "get-char-property" => Some(
                crate::emacs_core::textprop::builtin_get_char_property_in_buffers(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "get-pos-property" => Some(
                crate::emacs_core::builtins::builtin_get_pos_property_in_state(
                    &*self.shared.obarray,
                    &*self.shared.dynamic,
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "add-text-properties" => Some(
                crate::emacs_core::textprop::builtin_add_text_properties_in_buffers(
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "add-face-text-property" => Some(
                crate::emacs_core::textprop::builtin_add_face_text_property_in_buffers(
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "remove-text-properties" => Some(
                crate::emacs_core::textprop::builtin_remove_text_properties_in_buffers(
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "set-text-properties" => Some(
                crate::emacs_core::textprop::builtin_set_text_properties_in_buffers(
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "remove-list-of-text-properties" => Some(
                crate::emacs_core::textprop::builtin_remove_list_of_text_properties_in_buffers(
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "text-properties-at" => Some(
                crate::emacs_core::textprop::builtin_text_properties_at_in_buffers(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "get-char-property-and-overlay" => Some(
                crate::emacs_core::textprop::builtin_get_char_property_and_overlay_in_buffers(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "get-display-property" => Some(
                crate::emacs_core::textprop::builtin_get_display_property_in_buffers(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "next-single-property-change" => Some(
                crate::emacs_core::textprop::builtin_next_single_property_change_in_buffers(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "previous-single-property-change" => Some(
                crate::emacs_core::textprop::builtin_previous_single_property_change_in_buffers(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "next-property-change" => Some(
                crate::emacs_core::textprop::builtin_next_property_change_in_buffers(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "previous-property-change" => Some(
                crate::emacs_core::builtins::builtin_previous_property_change_in_buffers(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "next-char-property-change" => Some(
                crate::emacs_core::builtins::builtin_next_char_property_change_in_buffers(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "previous-char-property-change" => Some(
                crate::emacs_core::builtins::builtin_previous_char_property_change_in_buffers(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "next-single-char-property-change" => Some(
                crate::emacs_core::builtins::builtin_next_single_char_property_change_in_buffers(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "previous-single-char-property-change" => Some(
                crate::emacs_core::builtins::builtin_previous_single_char_property_change_in_buffers(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "text-property-any" => Some(
                crate::emacs_core::textprop::builtin_text_property_any_in_buffers(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "text-property-not-all" => Some(
                crate::emacs_core::textprop::builtin_text_property_not_all_in_buffers(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "marker-position" => Some(
                crate::emacs_core::marker::builtin_marker_position_in_buffers(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "copy-marker" => Some(crate::emacs_core::marker::builtin_copy_marker_in_buffers(
                self.shared.buffers,
                args.to_vec(),
            )),
            "set-marker" => Some(crate::emacs_core::marker::builtin_set_marker_in_buffers(
                self.shared.buffers,
                args.to_vec(),
            )),
            "move-marker" => Some(crate::emacs_core::marker::builtin_move_marker_in_buffers(
                self.shared.buffers,
                args.to_vec(),
            )),
            "point-marker" => Some(crate::emacs_core::marker::builtin_point_marker_in_buffers(
                self.shared.buffers,
                args.to_vec(),
            )),
            "point-min-marker" => Some(
                crate::emacs_core::marker::builtin_point_min_marker_in_buffers(
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "point-max-marker" => Some(
                crate::emacs_core::marker::builtin_point_max_marker_in_buffers(
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "mark-marker" => Some(crate::emacs_core::marker::builtin_mark_marker_in_buffers(
                &*self.shared.buffers,
                args.to_vec(),
            )),
            "standard-case-table" => Some(
                crate::emacs_core::casetab::builtin_standard_case_table(args.to_vec()),
            ),
            "standard-category-table" => Some(
                crate::emacs_core::category::builtin_standard_category_table(args.to_vec()),
            ),
            "current-global-map" => Some(
                builtins::expect_args("current-global-map", args, 0)
                    .map(|_| self.ensure_global_keymap()),
            ),
            "current-active-maps" => Some(self.builtin_current_active_maps_shared(args)),
            "current-minor-mode-maps" => Some(self.builtin_current_minor_mode_maps_shared(args)),
            "use-global-map" => Some(
                crate::emacs_core::builtins::keymaps::builtin_use_global_map_in_obarray(
                    self.shared.obarray,
                    args,
                ),
            ),
            "use-local-map" => Some(
                crate::emacs_core::builtins::keymaps::builtin_use_local_map_in_state(
                    self.shared.obarray,
                    self.shared.current_local_map,
                    args,
                ),
            ),
            "current-local-map" => Some(
                crate::emacs_core::builtins::keymaps::builtin_current_local_map_in_state(
                    *self.shared.current_local_map,
                    args,
                ),
            ),
            "lookup-key" => Some(
                crate::emacs_core::builtins::keymaps::builtin_lookup_key_in_obarray(
                    &*self.shared.obarray,
                    args,
                ),
            ),
            "accessible-keymaps" => Some(
                crate::emacs_core::builtins::keymaps::builtin_accessible_keymaps_in_obarray(
                    &*self.shared.obarray,
                    args,
                ),
            ),
            "copy-keymap" => Some(
                crate::emacs_core::builtins::keymaps::builtin_copy_keymap_in_obarray(
                    &*self.shared.obarray,
                    args,
                ),
            ),
            "keymap-parent" => Some(
                crate::emacs_core::builtins::keymaps::builtin_keymap_parent_in_obarray(
                    &*self.shared.obarray,
                    args,
                ),
            ),
            "set-keymap-parent" => Some(
                crate::emacs_core::builtins::keymaps::builtin_set_keymap_parent_in_obarray(
                    &*self.shared.obarray,
                    args,
                ),
            ),
            "map-keymap" => Some(self.builtin_map_keymap_shared(args, true)),
            "map-keymap-internal" => Some(self.builtin_map_keymap_shared(args, false)),
            "command-remapping" => Some(self.builtin_command_remapping_shared(args)),
            "key-binding" => Some(self.builtin_key_binding_shared(args)),
            "local-key-binding" => Some(self.builtin_local_key_binding_shared(args)),
            "minor-mode-key-binding" => Some(self.builtin_minor_mode_key_binding_shared(args)),
            "run-hooks" => Some(self.builtin_run_hooks_shared(args)),
            "run-hook-with-args" => Some(self.builtin_run_hook_with_args_shared(args)),
            "run-hook-with-args-until-success" => {
                Some(self.builtin_run_hook_with_args_until_success_shared(args))
            }
            "run-hook-with-args-until-failure" => {
                Some(self.builtin_run_hook_with_args_until_failure_shared(args))
            }
            "run-hook-wrapped" => Some(self.builtin_run_hook_wrapped_shared(args)),
            "run-hook-query-error-with-timeout" => {
                Some(self.builtin_run_hook_query_error_with_timeout_shared(args))
            }
            "autoload" => Some(crate::emacs_core::autoload::register_autoload_in_state(
                self.shared.obarray,
                self.shared.autoloads,
                args,
            )),
            "boundp" => Some(crate::emacs_core::builtins::symbols::builtin_boundp_in_state(
                &*self.shared.obarray,
                self.shared.dynamic.as_slice(),
                &*self.shared.buffers,
                args.to_vec(),
            )),
            "default-value" => Some(crate::emacs_core::custom::builtin_default_value_in_state(
                &*self.shared.obarray,
                self.shared.dynamic.as_slice(),
                args.to_vec(),
            )),
            "set" => Some(self.builtin_set_shared(args)),
            "makunbound" => Some(self.builtin_makunbound_shared(args)),
            "default-boundp" => Some(
                crate::emacs_core::builtins::symbols::builtin_default_boundp_in_obarray(
                    &*self.shared.obarray,
                    args.to_vec(),
                ),
            ),
            "special-variable-p" => Some(
                crate::emacs_core::builtins::symbols::builtin_special_variable_p_in_obarray(
                    &*self.shared.obarray,
                    args.to_vec(),
                ),
            ),
            "indirect-variable" => Some(
                crate::emacs_core::builtins::symbols::builtin_indirect_variable_in_obarray(
                    &*self.shared.obarray,
                    args.to_vec(),
                ),
            ),
            "symbol-value" => Some(
                crate::emacs_core::builtins::symbols::builtin_symbol_value_in_state(
                    &*self.shared.obarray,
                    self.shared.dynamic.as_slice(),
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "commandp" => Some(crate::emacs_core::interactive::builtin_commandp_in_state(
                &*self.shared.obarray,
                &*self.shared.interactive,
                args,
            )),
            "interactive-form" => Some(self.builtin_interactive_form_shared(args)),
            "command-modes" => Some(crate::emacs_core::interactive::builtin_command_modes_in_state(
                &*self.shared.obarray,
                args,
            )),
            "featurep" => Some(crate::emacs_core::builtins::builtin_featurep_in_state(
                &*self.shared.obarray,
                self.shared.features,
                args.to_vec(),
            )),
            "provide" => Some(crate::emacs_core::builtins::builtin_provide_in_state(
                self.shared.obarray,
                self.shared.features,
                args.to_vec(),
            )),
            "eval" => Some(self.eval_with_vm_bridge(args.to_vec(), args)),
            "load" => Some(self.load_with_vm_bridge(args.to_vec(), args)),
            "autoload-do-load" => Some(self.autoload_do_load_with_vm_bridge(
                args.to_vec(),
                args,
            )),
            "require" => Some(self.require_with_vm_bridge(args.to_vec(), args)),
            "symbol-file" => Some(crate::emacs_core::autoload::builtin_symbol_file_in_state(
                &*self.shared.obarray,
                &*self.shared.autoloads,
                args,
            )),
            "set-buffer" => Some(
                crate::emacs_core::builtins::builtin_set_buffer_in_manager(
                    self.shared.buffers,
                    args,
                ),
            ),
            "get-buffer-create" => Some(
                crate::emacs_core::builtins::builtin_get_buffer_create_in_manager(
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "get-buffer" => Some(crate::emacs_core::builtins::builtin_get_buffer_in_manager(
                &*self.shared.buffers,
                args.to_vec(),
            )),
            "find-buffer" => Some(crate::emacs_core::builtins::builtin_find_buffer_in_state(
                &*self.shared.obarray,
                self.shared.dynamic.as_slice(),
                &*self.shared.buffers,
                args.to_vec(),
            )),
            "buffer-live-p" => Some(
                crate::emacs_core::builtins::builtin_buffer_live_p_in_manager(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "buffer-name" => Some(crate::emacs_core::builtins::builtin_buffer_name_in_manager(
                &*self.shared.buffers,
                args.to_vec(),
            )),
            "buffer-file-name" => Some(
                crate::emacs_core::builtins::builtin_buffer_file_name_in_manager(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "buffer-base-buffer" => Some(
                crate::emacs_core::builtins::builtin_buffer_base_buffer_in_manager(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "buffer-last-name" => Some(
                crate::emacs_core::builtins::builtin_buffer_last_name_in_manager(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "rename-buffer" => Some(
                crate::emacs_core::builtins::symbols::builtin_rename_buffer_in_manager(
                    &mut *self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "bury-buffer-internal" => Some(
                crate::emacs_core::builtins::builtin_bury_buffer_internal_in_state(
                    &*self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "kill-buffer" => Some(self.builtin_kill_buffer_shared(args)),
            "set-buffer-multibyte" => Some(self.builtin_set_buffer_multibyte_shared(args)),
            "make-local-variable" => Some(self.builtin_make_local_variable_shared(args)),
            "local-variable-p" => Some(self.builtin_local_variable_p_shared(args)),
            "buffer-local-variables" => Some(self.builtin_buffer_local_variables_shared(args)),
            "kill-local-variable" => Some(self.builtin_kill_local_variable_shared(args)),
            "current-buffer" => Some(
                crate::emacs_core::builtins::builtin_current_buffer_in_manager(
                    &*self.shared.buffers,
                    args,
                ),
            ),
            "keymapp" => Some(
                crate::emacs_core::builtins::keymaps::builtin_keymapp_in_obarray(
                    &*self.shared.obarray,
                    args,
                ),
            ),
            "define-key" => Some((|| -> EvalResult {
                builtins::expect_min_args("define-key", args, 3)?;
                builtins::expect_max_args("define-key", args, 4)?;
                let keymap =
                    crate::emacs_core::builtins::keymaps::expect_keymap_in_obarray(
                        self.shared.obarray,
                        &args[0],
                    )?;
                let events = crate::emacs_core::builtins::keymaps::expect_key_events(&args[1])?;
                let def = args[2];
                crate::emacs_core::keymap::list_keymap_define_seq(keymap, &events, def);
                Ok(def)
            })()),
            "get" => Some((|| -> EvalResult {
                builtins::expect_args("get", args, 2)?;
                let sym = crate::emacs_core::builtins::symbols::expect_symbol_id(&args[0])?;
                if let Some(raw) =
                    crate::emacs_core::builtins::symbols::symbol_raw_plist_value_in_obarray(
                        self.shared.obarray,
                        sym,
                    )
                {
                    return Ok(
                        crate::emacs_core::builtins::symbols::plist_lookup_value(&raw, &args[1])
                            .unwrap_or(Value::Nil),
                    );
                }
                let prop = crate::emacs_core::builtins::symbols::expect_symbol_id(&args[1])?;
                if crate::emacs_core::builtins::symbols::is_internal_symbol_plist_property(
                    resolve_sym(prop),
                ) {
                    return Ok(Value::Nil);
                }
                Ok(self.shared.obarray
                    .get_property_id(sym, prop)
                    .cloned()
                    .unwrap_or(Value::Nil))
            })()),
            "put" => {
                Some((|| -> EvalResult {
                builtins::expect_args("put", args, 3)?;
                let sym = crate::emacs_core::builtins::symbols::expect_symbol_id(&args[0])?;
                let prop = crate::emacs_core::builtins::symbols::expect_symbol_id(&args[1])?;
                let value = args[2];
                if let Some(raw) =
                    crate::emacs_core::builtins::symbols::symbol_raw_plist_value_in_obarray(
                        self.shared.obarray,
                        sym,
                    )
                {
                    let plist =
                        crate::emacs_core::builtins::collections::builtin_plist_put(vec![
                            raw, args[1], value,
                        ])?;
                    crate::emacs_core::builtins::symbols::set_symbol_raw_plist_in_obarray(
                        self.shared.obarray,
                        sym,
                        plist,
                    );
                    return Ok(value);
                }
                self.shared.obarray.put_property_id(sym, prop, value);
                Ok(value)
                })())
            }
            "setplist" => Some(
                crate::emacs_core::builtins::symbols::builtin_setplist_in_obarray(
                    self.shared.obarray,
                    args.to_vec(),
                ),
            ),
            "symbol-function" => Some(
                crate::emacs_core::builtins::symbols::builtin_symbol_function_in_obarray(
                    &*self.shared.obarray,
                    args.to_vec(),
                ),
            ),
            "symbol-plist" => Some(
                crate::emacs_core::builtins::symbols::builtin_symbol_plist_in_obarray(
                    &*self.shared.obarray,
                    args.to_vec(),
                ),
            ),
            "indirect-function" => Some(
                crate::emacs_core::builtins::symbols::builtin_indirect_function_in_obarray(
                    &*self.shared.obarray,
                    args.to_vec(),
                ),
            ),
            "functionp" => Some(crate::emacs_core::builtins::builtin_functionp_in_obarray(
                &*self.shared.obarray,
                args.to_vec(),
            )),
            "defalias" => Some(self.builtin_defalias_shared(args)),
            "fset" => Some(crate::emacs_core::builtins::symbols::builtin_fset_in_obarray(
                self.shared.obarray,
                args.to_vec(),
            )),
            "fmakunbound" => Some(
                crate::emacs_core::builtins::symbols::builtin_fmakunbound_in_obarray(
                    self.shared.obarray,
                    args.to_vec(),
                ),
            ),
            "func-arity" => Some(
                crate::emacs_core::builtins::symbols::builtin_func_arity_in_obarray(
                    &*self.shared.obarray,
                    args.to_vec(),
                ),
            ),
            "intern-soft" => Some(
                crate::emacs_core::builtins::symbols::builtin_intern_soft_in_obarray(
                    &*self.shared.obarray,
                    args.to_vec(),
                ),
            ),
            "obarrayp" => Some(crate::emacs_core::builtins::symbols::builtin_obarrayp(
                args.to_vec(),
            )),
            "default-toplevel-value" => Some(
                crate::emacs_core::builtins::symbols::builtin_default_toplevel_value_in_obarray(
                    self.shared.obarray,
                    args.to_vec(),
                ),
            ),
            "defvaralias" => Some(self.builtin_defvaralias_shared(args)),
            "set-default-toplevel-value" => {
                Some(self.builtin_set_default_toplevel_value_shared(args))
            }
            "internal--define-uninitialized-variable" => Some(
                crate::emacs_core::builtins::symbols::builtin_internal_define_uninitialized_variable_in_obarray(
                    self.shared.obarray,
                    args.to_vec(),
                ),
            ),
            "set-default" => Some(self.builtin_set_default_shared(args)),
            "make-variable-buffer-local" => Some(
                crate::emacs_core::custom::builtin_make_variable_buffer_local_with_state(
                    self.shared.obarray,
                    self.shared.custom,
                    args.to_vec(),
                ),
            ),
            "intern" => Some((|| -> EvalResult {
                builtins::expect_min_args("intern", args, 1)?;
                builtins::expect_max_args("intern", args, 2)?;
                if let Some(obarray) = args.get(1) {
                    if !obarray.is_nil() && !matches!(obarray, Value::Vector(_)) {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("obarrayp"), *obarray],
                        ));
                    }
                }
                let name = builtins::expect_string(&args[0])?;
                if let Some(Value::Vector(vec_id)) = args.get(1).filter(|value| !value.is_nil()) {
                    let vec_id = *vec_id;
                    let vec_len = with_heap(|h| h.get_vector(vec_id).len());
                    if vec_len == 0 {
                        return Err(signal("args-out-of-range", vec![Value::Int(0)]));
                    }
                    let bucket_idx =
                        crate::emacs_core::builtins::symbols::obarray_hash(&name, vec_len);
                    let bucket = with_heap(|h| h.get_vector(vec_id)[bucket_idx]);
                    if let Some(sym) =
                        crate::emacs_core::builtins::symbols::obarray_bucket_find(bucket, &name)
                    {
                        return Ok(sym);
                    }
                    let sym = Value::Symbol(intern_uninterned(&name));
                    let new_bucket = Value::cons(sym, bucket);
                    with_heap_mut(|h| {
                        h.get_vector_mut(vec_id)[bucket_idx] = new_bucket;
                    });
                    return Ok(sym);
                }
                self.shared.obarray.intern(&name);
                Ok(Value::symbol(name))
            })()),
            "unintern" => Some(crate::emacs_core::hashtab::builtin_unintern_in_obarray(
                self.shared.obarray,
                args.to_vec(),
            )),
            "mapcar" => Some(self.builtin_mapcar_fast(args)),
            "fboundp" => Some(self.builtin_fboundp_fast(args)),
            "frame-list" => Some(self.builtin_frame_list_fast(args)),
            "framep" => Some(self.builtin_framep_fast(args)),
            "frame-parameter" => Some(self.builtin_frame_parameter_fast(args)),
            "frame-id" => Some(crate::emacs_core::builtins::builtin_frame_id_in_state(
                self.shared.frames,
                self.shared.buffers,
                args.to_vec(),
            )),
            "frame-root-frame" => Some(
                crate::emacs_core::builtins::builtin_frame_root_frame_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "frame-char-height" => Some(
                crate::emacs_core::window_cmds::builtin_frame_char_height_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "frame-char-width" => Some(
                crate::emacs_core::window_cmds::builtin_frame_char_width_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "frame-native-height" => Some(
                crate::emacs_core::window_cmds::builtin_frame_native_height_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "frame-native-width" => Some(
                crate::emacs_core::window_cmds::builtin_frame_native_width_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "frame-text-cols" => Some(
                crate::emacs_core::window_cmds::builtin_frame_text_cols_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "frame-text-lines" => Some(
                crate::emacs_core::window_cmds::builtin_frame_text_lines_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "frame-text-width" => Some(
                crate::emacs_core::window_cmds::builtin_frame_text_width_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "frame-text-height" => Some(
                crate::emacs_core::window_cmds::builtin_frame_text_height_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "frame-total-cols" => Some(
                crate::emacs_core::window_cmds::builtin_frame_total_cols_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "frame-total-lines" => Some(
                crate::emacs_core::window_cmds::builtin_frame_total_lines_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "frame-position" => Some(
                crate::emacs_core::window_cmds::builtin_frame_position_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "next-frame" => Some(
                crate::emacs_core::builtins::symbols::builtin_next_frame_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "previous-frame" => Some(
                crate::emacs_core::builtins::symbols::builtin_previous_frame_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "old-selected-frame" => Some(
                crate::emacs_core::builtins::symbols::builtin_old_selected_frame_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "mouse-pixel-position" => Some(
                crate::emacs_core::builtins::symbols::builtin_mouse_pixel_position_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "mouse-position" => Some(
                crate::emacs_core::builtins::symbols::builtin_mouse_position_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-system" => Some(crate::emacs_core::display::builtin_window_system_in_state(
                &*self.shared.obarray,
                self.shared.dynamic.as_slice(),
                self.shared.frames,
                self.shared.buffers,
                args.to_vec(),
            )),
            "tool-bar-height" => Some(crate::emacs_core::xdisp::builtin_tool_bar_height_in_state(
                self.shared.frames,
                self.shared.buffers,
                args.to_vec(),
            )),
            "tab-bar-height" => Some(crate::emacs_core::xdisp::builtin_tab_bar_height_in_state(
                self.shared.frames,
                self.shared.buffers,
                args.to_vec(),
            )),
            "selected-frame" => Some(
                crate::emacs_core::window_cmds::builtin_selected_frame_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "selected-window" => Some(
                crate::emacs_core::window_cmds::builtin_selected_window_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "frame-selected-window" => Some(
                crate::emacs_core::window_cmds::builtin_frame_selected_window_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "frame-first-window" => Some(
                crate::emacs_core::window_cmds::builtin_frame_first_window_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "frame-root-window" => Some(
                crate::emacs_core::window_cmds::builtin_frame_root_window_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "windowp" => Some(crate::emacs_core::window_cmds::builtin_windowp_in_state(
                &*self.shared.frames,
                args.to_vec(),
            )),
            "split-window-internal" => Some(
                crate::emacs_core::builtins::builtin_split_window_internal_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-valid-p" => Some(
                crate::emacs_core::window_cmds::builtin_window_valid_p_in_state(
                    &*self.shared.frames,
                    args.to_vec(),
                ),
            ),
            "window-live-p" => Some(
                crate::emacs_core::window_cmds::builtin_window_live_p_in_state(
                    &*self.shared.frames,
                    args.to_vec(),
                ),
            ),
            "window-frame" => Some(
                crate::emacs_core::window_cmds::builtin_window_frame_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-buffer" => Some(
                crate::emacs_core::window_cmds::builtin_window_buffer_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-display-table" => Some(
                crate::emacs_core::window_cmds::builtin_window_display_table_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "set-window-display-table" => Some(
                crate::emacs_core::window_cmds::builtin_set_window_display_table_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-cursor-type" => Some(
                crate::emacs_core::window_cmds::builtin_window_cursor_type_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "set-window-cursor-type" => Some(
                crate::emacs_core::window_cmds::builtin_set_window_cursor_type_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-parameter" => Some(
                crate::emacs_core::window_cmds::builtin_window_parameter_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "set-window-parameter" => Some(
                crate::emacs_core::window_cmds::builtin_set_window_parameter_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-parameters" => Some(
                crate::emacs_core::window_cmds::builtin_window_parameters_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-parent" => Some(
                crate::emacs_core::window_cmds::builtin_window_parent_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-top-child" => Some(
                crate::emacs_core::window_cmds::builtin_window_top_child_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-left-child" => Some(
                crate::emacs_core::window_cmds::builtin_window_left_child_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-next-sibling" => Some(
                crate::emacs_core::window_cmds::builtin_window_next_sibling_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-prev-sibling" => Some(
                crate::emacs_core::window_cmds::builtin_window_prev_sibling_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-dedicated-p" => Some(
                crate::emacs_core::window_cmds::builtin_window_dedicated_p_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "set-window-dedicated-p" => Some(
                crate::emacs_core::window_cmds::builtin_set_window_dedicated_p_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-normal-size" => Some(
                crate::emacs_core::window_cmds::builtin_window_normal_size_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-start" => Some(
                crate::emacs_core::window_cmds::builtin_window_start_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-end" => Some(
                crate::emacs_core::window_cmds::builtin_window_end_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-group-start" => Some(
                crate::emacs_core::window_cmds::builtin_window_group_start_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-point" => Some(
                crate::emacs_core::window_cmds::builtin_window_point_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-use-time" => Some(
                crate::emacs_core::window_cmds::builtin_window_use_time_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-old-point" => Some(
                crate::emacs_core::window_cmds::builtin_window_old_point_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-old-buffer" => Some(
                crate::emacs_core::window_cmds::builtin_window_old_buffer_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-prev-buffers" => Some(
                crate::emacs_core::window_cmds::builtin_window_prev_buffers_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-next-buffers" => Some(
                crate::emacs_core::window_cmds::builtin_window_next_buffers_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-height" => Some(
                crate::emacs_core::window_cmds::builtin_window_height_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-width" => Some(
                crate::emacs_core::window_cmds::builtin_window_width_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-left-column" => Some(
                crate::emacs_core::window_cmds::builtin_window_left_column_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-top-line" => Some(
                crate::emacs_core::window_cmds::builtin_window_top_line_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-pixel-left" => Some(
                crate::emacs_core::window_cmds::builtin_window_pixel_left_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-pixel-top" => Some(
                crate::emacs_core::window_cmds::builtin_window_pixel_top_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-bump-use-time" => Some(
                crate::emacs_core::window_cmds::builtin_window_bump_use_time_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "set-window-start" => Some(
                crate::emacs_core::window_cmds::builtin_set_window_start_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "set-window-group-start" => Some(
                crate::emacs_core::window_cmds::builtin_set_window_group_start_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "set-window-point" => Some(
                crate::emacs_core::window_cmds::builtin_set_window_point_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "set-window-prev-buffers" => Some(
                crate::emacs_core::window_cmds::builtin_set_window_prev_buffers_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "set-window-next-buffers" => Some(
                crate::emacs_core::window_cmds::builtin_set_window_next_buffers_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-hscroll" => Some(
                crate::emacs_core::window_cmds::builtin_window_hscroll_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "scroll-left" => Some(crate::emacs_core::window_cmds::builtin_scroll_left_in_state(
                self.shared.frames,
                self.shared.buffers,
                args.to_vec(),
            )),
            "scroll-right" => Some(
                crate::emacs_core::window_cmds::builtin_scroll_right_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "scroll-up" => Some(crate::emacs_core::window_cmds::builtin_scroll_up_in_state(
                &*self.shared.obarray,
                self.shared.frames,
                self.shared.buffers,
                args.to_vec(),
            )),
            "scroll-down" => Some(
                crate::emacs_core::window_cmds::builtin_scroll_down_in_state(
                    &*self.shared.obarray,
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "set-window-hscroll" => Some(
                crate::emacs_core::window_cmds::builtin_set_window_hscroll_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-vscroll" => Some(
                crate::emacs_core::window_cmds::builtin_window_vscroll_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "set-window-vscroll" => Some(
                crate::emacs_core::window_cmds::builtin_set_window_vscroll_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-margins" => Some(
                crate::emacs_core::window_cmds::builtin_window_margins_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "set-window-margins" => Some(
                crate::emacs_core::window_cmds::builtin_set_window_margins_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-fringes" => Some(
                crate::emacs_core::window_cmds::builtin_window_fringes_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "set-window-fringes" => Some(
                crate::emacs_core::window_cmds::builtin_set_window_fringes_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-scroll-bars" => Some(
                crate::emacs_core::window_cmds::builtin_window_scroll_bars_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "set-window-scroll-bars" => Some(
                crate::emacs_core::window_cmds::builtin_set_window_scroll_bars_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-mode-line-height" => Some(
                crate::emacs_core::window_cmds::builtin_window_mode_line_height_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-header-line-height" => Some(
                crate::emacs_core::window_cmds::builtin_window_header_line_height_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-pixel-height" => Some(
                crate::emacs_core::window_cmds::builtin_window_pixel_height_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-pixel-width" => Some(
                crate::emacs_core::window_cmds::builtin_window_pixel_width_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-body-height" => Some(
                crate::emacs_core::window_cmds::builtin_window_body_height_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "recenter" => Some(crate::emacs_core::window_cmds::builtin_recenter_in_state(
                self.shared.frames,
                self.shared.buffers,
                args.to_vec(),
            )),
            "window-body-width" => Some(
                crate::emacs_core::window_cmds::builtin_window_body_width_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-text-height" => Some(
                crate::emacs_core::window_cmds::builtin_window_text_height_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-text-width" => Some(
                crate::emacs_core::window_cmds::builtin_window_text_width_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-edges" => Some(
                crate::emacs_core::window_cmds::builtin_window_edges_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-total-height" => Some(
                crate::emacs_core::window_cmds::builtin_window_total_height_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-total-width" => Some(
                crate::emacs_core::window_cmds::builtin_window_total_width_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-list" => Some(crate::emacs_core::window_cmds::builtin_window_list_in_state(
                self.shared.frames,
                self.shared.buffers,
                args.to_vec(),
            )),
            "window-list-1" => Some(
                crate::emacs_core::window_cmds::builtin_window_list_1_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-at" => Some(crate::emacs_core::window_cmds::builtin_window_at_in_state(
                self.shared.frames,
                self.shared.buffers,
                args.to_vec(),
            )),
            "select-window" => Some(
                crate::emacs_core::window_cmds::builtin_select_window_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "other-window" => Some(
                crate::emacs_core::window_cmds::builtin_other_window_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "other-window-for-scrolling" => Some(
                crate::emacs_core::window_cmds::builtin_other_window_for_scrolling_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "next-window" => Some(
                crate::emacs_core::window_cmds::builtin_next_window_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "previous-window" => Some(
                crate::emacs_core::window_cmds::builtin_previous_window_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "set-window-buffer" => Some(
                crate::emacs_core::window_cmds::builtin_set_window_buffer_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "delete-window" => Some(
                crate::emacs_core::window_cmds::builtin_delete_window_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "delete-other-windows" => Some(
                crate::emacs_core::window_cmds::builtin_delete_other_windows_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "delete-window-internal" => Some(
                crate::emacs_core::window_cmds::builtin_delete_window_internal_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "delete-other-windows-internal" => Some(
                crate::emacs_core::window_cmds::builtin_delete_other_windows_internal_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "current-window-configuration" => Some(
                crate::emacs_core::builtins::builtin_current_window_configuration_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "set-window-configuration" => Some(
                crate::emacs_core::builtins::builtin_set_window_configuration_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "window-combination-limit" => Some(
                crate::emacs_core::window_cmds::builtin_window_combination_limit_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "set-window-combination-limit" => Some(
                crate::emacs_core::window_cmds::builtin_set_window_combination_limit_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "frame-visible-p" => Some(
                crate::emacs_core::window_cmds::builtin_frame_visible_p_in_state(
                    &*self.shared.frames,
                    args.to_vec(),
                ),
            ),
            "select-frame" => Some(
                crate::emacs_core::window_cmds::builtin_select_frame_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "select-frame-set-input-focus" => Some(
                crate::emacs_core::window_cmds::builtin_select_frame_set_input_focus_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "visible-frame-list" => Some(
                crate::emacs_core::window_cmds::builtin_visible_frame_list_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "make-frame-visible" => Some(
                crate::emacs_core::window_cmds::builtin_make_frame_visible_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "iconify-frame" => Some(
                crate::emacs_core::window_cmds::builtin_iconify_frame_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "make-frame" => Some(crate::emacs_core::window_cmds::builtin_make_frame_in_state(
                self.shared.frames,
                self.shared.buffers,
                args.to_vec(),
            )),
            "frame-live-p" => Some(
                crate::emacs_core::window_cmds::builtin_frame_live_p_in_state(
                    &*self.shared.frames,
                    args.to_vec(),
                ),
            ),
            "delete-frame" => Some(
                crate::emacs_core::window_cmds::builtin_delete_frame_in_state(
                    self.shared.frames,
                    self.shared.buffers,
                    args.to_vec(),
                ),
            ),
            "coding-system-list" => Some(crate::emacs_core::coding::builtin_coding_system_list(
                &*self.shared.coding_systems,
                args.to_vec(),
            )),
            "coding-system-aliases" => Some(
                crate::emacs_core::coding::builtin_coding_system_aliases(
                    &*self.shared.coding_systems,
                    args.to_vec(),
                ),
            ),
            "coding-system-get" => Some(crate::emacs_core::coding::builtin_coding_system_get(
                &*self.shared.coding_systems,
                args.to_vec(),
            )),
            "coding-system-plist" => Some(crate::emacs_core::coding::builtin_coding_system_plist(
                &*self.shared.coding_systems,
                args.to_vec(),
            )),
            "coding-system-put" => Some(crate::emacs_core::coding::builtin_coding_system_put(
                self.shared.coding_systems,
                args.to_vec(),
            )),
            "coding-system-base" => Some(crate::emacs_core::coding::builtin_coding_system_base(
                &*self.shared.coding_systems,
                args.to_vec(),
            )),
            "coding-system-eol-type" => Some(
                crate::emacs_core::coding::builtin_coding_system_eol_type(
                    &*self.shared.coding_systems,
                    args.to_vec(),
                ),
            ),
            "coding-system-type" => Some(crate::emacs_core::coding::builtin_coding_system_type(
                &*self.shared.coding_systems,
                args.to_vec(),
            )),
            "coding-system-change-eol-conversion" => Some(
                crate::emacs_core::coding::builtin_coding_system_change_eol_conversion(
                    &*self.shared.coding_systems,
                    args.to_vec(),
                ),
            ),
            "coding-system-change-text-conversion" => Some(
                crate::emacs_core::coding::builtin_coding_system_change_text_conversion(
                    &*self.shared.coding_systems,
                    args.to_vec(),
                ),
            ),
            "coding-system-p" => Some(crate::emacs_core::coding::builtin_coding_system_p(
                &*self.shared.coding_systems,
                args.to_vec(),
            )),
            "check-coding-system" => Some(
                crate::emacs_core::coding::builtin_check_coding_system(
                    &*self.shared.coding_systems,
                    args.to_vec(),
                ),
            ),
            "check-coding-systems-region" => Some(
                crate::emacs_core::coding::builtin_check_coding_systems_region(
                    &*self.shared.coding_systems,
                    args.to_vec(),
                ),
            ),
            "define-coding-system-internal" => Some(
                crate::emacs_core::coding::builtin_define_coding_system_internal(
                    self.shared.coding_systems,
                    args.to_vec(),
                ),
            ),
            "define-coding-system-alias" => Some(
                crate::emacs_core::coding::builtin_define_coding_system_alias(
                    self.shared.coding_systems,
                    args.to_vec(),
                ),
            ),
            "set-coding-system-priority" => Some(
                crate::emacs_core::coding::builtin_set_coding_system_priority(
                    self.shared.coding_systems,
                    args.to_vec(),
                ),
            ),
            "detect-coding-string" => Some(
                crate::emacs_core::coding::builtin_detect_coding_string(
                    &*self.shared.coding_systems,
                    args.to_vec(),
                ),
            ),
            "detect-coding-region" => Some(
                crate::emacs_core::coding::builtin_detect_coding_region(
                    &*self.shared.coding_systems,
                    args.to_vec(),
                ),
            ),
            "keyboard-coding-system" => Some(
                crate::emacs_core::coding::builtin_keyboard_coding_system(
                    &*self.shared.coding_systems,
                    args.to_vec(),
                ),
            ),
            "terminal-coding-system" => Some(
                crate::emacs_core::coding::builtin_terminal_coding_system(
                    &*self.shared.coding_systems,
                    args.to_vec(),
                ),
            ),
            "set-keyboard-coding-system" => Some(
                crate::emacs_core::coding::builtin_set_keyboard_coding_system(
                    self.shared.coding_systems,
                    args.to_vec(),
                ),
            ),
            "set-terminal-coding-system" => Some(
                crate::emacs_core::coding::builtin_set_terminal_coding_system(
                    self.shared.coding_systems,
                    args.to_vec(),
                ),
            ),
            "set-keyboard-coding-system-internal" => Some(
                crate::emacs_core::coding::builtin_set_keyboard_coding_system_internal(
                    self.shared.coding_systems,
                    args.to_vec(),
                ),
            ),
            "set-terminal-coding-system-internal" => Some(
                crate::emacs_core::coding::builtin_set_terminal_coding_system_internal(
                    self.shared.coding_systems,
                    args.to_vec(),
                ),
            ),
            "set-safe-terminal-coding-system-internal" => Some(
                crate::emacs_core::coding::builtin_set_safe_terminal_coding_system_internal(
                    self.shared.coding_systems,
                    args.to_vec(),
                ),
            ),
            "coding-system-priority-list" => Some(
                crate::emacs_core::coding::builtin_coding_system_priority_list(
                    &*self.shared.coding_systems,
                    args.to_vec(),
                ),
            ),
            "search-forward" => Some(self.builtin_search_forward_shared(args)),
            "search-backward" => Some(self.builtin_search_backward_shared(args)),
            "re-search-forward" => Some(self.builtin_re_search_forward_shared(args)),
            "re-search-backward" => Some(self.builtin_re_search_backward_shared(args)),
            "search-forward-regexp" => Some(self.builtin_search_forward_regexp_shared(args)),
            "search-backward-regexp" => Some(self.builtin_search_backward_regexp_shared(args)),
            "posix-search-forward" => Some(self.builtin_re_search_forward_shared(args)),
            "posix-search-backward" => Some(self.builtin_re_search_backward_shared(args)),
            "looking-at" => Some(self.builtin_looking_at_shared(args)),
            "looking-at-p" => Some(self.builtin_looking_at_p_shared(args)),
            "posix-looking-at" => Some(self.builtin_posix_looking_at_shared(args)),
            "string-match" => Some({
                let case_fold = self
                    .lookup_var("case-fold-search")
                    .map(|value| !value.is_nil())
                    .unwrap_or(true);
                crate::emacs_core::builtins::search::builtin_string_match_with_state(
                    case_fold,
                    self.shared.match_data,
                    args,
                )
            }),
            "posix-string-match" => Some(self.builtin_posix_string_match_shared(args)),
            "match-beginning" => Some(
                crate::emacs_core::builtins::search::builtin_match_beginning_with_state(
                    Some(&*self.shared.buffers),
                    self.shared.match_data,
                    args,
                ),
            ),
            "match-end" => Some(
                crate::emacs_core::builtins::search::builtin_match_end_with_state(
                    Some(&*self.shared.buffers),
                    self.shared.match_data,
                    args,
                ),
            ),
            "match-data" => Some(
                crate::emacs_core::builtins::search::builtin_match_data_with_state(
                    Some(self.shared.buffers),
                    self.shared.match_data,
                    args,
                ),
            ),
            "set-match-data" => Some(
                crate::emacs_core::builtins::search::builtin_set_match_data_with_state(
                    self.shared.match_data,
                    args,
                ),
            ),
            "match-data--translate" => Some(self.builtin_match_data_translate_shared(args)),
            "replace-match" => Some(self.builtin_replace_match_shared(args)),
            "find-charset-region" => Some(self.builtin_find_charset_region_shared(args)),
            "charset-after" => Some(self.builtin_charset_after_shared(args)),
            "compose-region-internal" => Some(self.builtin_compose_region_internal_shared(args)),
            _ => None,
        }
    }

    fn with_mirrored_evaluator<T>(
        &mut self,
        extra_roots: &[Value],
        f: impl FnOnce(&mut crate::emacs_core::eval::Evaluator) -> T,
    ) -> T {
        use crate::emacs_core::intern::{
            current_interner_ptr, set_current_interner, with_saved_interner,
        };
        use crate::emacs_core::value::{current_heap_ptr, set_current_heap, with_saved_heap};
        // Evaluator::new() overwrites the thread-local heap/interner pointers.
        // Save and restore them so ObjIds/SymIds from the caller remain valid.
        let mut eval = with_saved_interner(|| {
            with_saved_heap(crate::emacs_core::eval::Evaluator::new_preserving_thread_locals)
        });

        // The temp evaluator owns a fresh empty heap, but all ObjIds in
        // args/obarray/dynamic/etc. belong to the ORIGINAL heap (the one
        // set as CURRENT_HEAP by the parent Evaluator).  Evaluator methods
        // like apply() and gc_collect() use self.heap, not the thread-local,
        // so we must swap the real heap data into the temp evaluator.
        let original_heap_ptr = current_heap_ptr();
        assert!(
            !original_heap_ptr.is_null(),
            "dispatch_vm_builtin_eval: no current heap"
        );
        let original_interner_ptr = current_interner_ptr();
        assert!(
            !original_interner_ptr.is_null(),
            "dispatch_vm_builtin_eval: no current interner"
        );
        // Safety: original_heap_ptr was set by the parent Evaluator's
        // setup_thread_locals() and points to a valid, exclusively-owned
        // LispHeap inside the parent's Box<LispHeap>.  The parent Evaluator
        // is alive on the stack (it created this VM) and no other code
        // accesses it while the VM is running.
        unsafe {
            std::mem::swap(&mut *eval.heap, &mut *original_heap_ptr);
        }
        // Safety: original_interner_ptr was set by the parent Evaluator's
        // setup_thread_locals() and points to the active append-only interner
        // that all live SymIds in shared VM state refer to.
        unsafe {
            std::mem::swap(&mut *eval.interner, &mut *original_interner_ptr);
        }
        // Point thread-local at eval.heap which now holds the real data.
        set_current_heap(&mut eval.heap);
        set_current_interner(&mut eval.interner);

        self.shared.swap_with_evaluator(&mut eval);
        let saved_temp_roots = eval.save_temp_roots();
        for root in &self.gc_roots {
            eval.push_temp_root(*root);
        }
        for root in extra_roots {
            eval.push_temp_root(*root);
        }

        let result = f(&mut eval);

        eval.restore_temp_roots(saved_temp_roots);
        self.shared.swap_with_evaluator(&mut eval);

        // Swap the heap data back to its original location so the parent
        // Evaluator's Box<LispHeap> is consistent when we return.  Any
        // objects allocated during the builtin are now in the original heap.
        unsafe {
            std::mem::swap(&mut *eval.heap, &mut *original_heap_ptr);
        }
        unsafe {
            std::mem::swap(&mut *eval.interner, &mut *original_interner_ptr);
        }
        // Restore thread-local to the original location.
        unsafe {
            set_current_heap(&mut *original_heap_ptr);
            set_current_interner(&mut *original_interner_ptr);
        }

        result
    }

    /// Dispatch builtins that require evaluator context by running them
    /// on a temporary evaluator mirrored from the VM's current obarray/env.
    fn dispatch_vm_builtin_eval(&mut self, name: &str, args: Vec<Value>) -> Option<EvalResult> {
        let trace_vm_builtins = std::env::var_os("NEOVM_TRACE_VM_BUILTINS").is_some();
        let trace_load_file_name = if trace_vm_builtins {
            self.shared
                .obarray
                .symbol_value("load-file-name")
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "<unknown>".to_string())
        } else {
            String::new()
        };
        let trace_start = trace_vm_builtins.then(std::time::Instant::now);
        let extra_roots = args.clone();
        let result = self.with_mirrored_evaluator(&extra_roots, move |eval| {
            builtins::dispatch_builtin(eval, name, args)
        });
        if let Some(start) = trace_start {
            let elapsed = start.elapsed();
            if elapsed.as_millis() > 0 {
                tracing::info!(
                    "VM-BUILTIN-EVAL file={} name={} elapsed={:.2?}",
                    trace_load_file_name,
                    name,
                    elapsed
                );
            }
        }
        result
    }
}

fn merge_result_with_cleanup(result: EvalResult, cleanup: Result<(), Flow>) -> EvalResult {
    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
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
                vm.shared.buffers,
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
                vm.shared.buffers,
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
                vm.shared.buffers,
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
            crate::emacs_core::marker::marker_position_as_int_with_buffers(
                vm.shared.buffers,
                marker,
            )? as f64,
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
