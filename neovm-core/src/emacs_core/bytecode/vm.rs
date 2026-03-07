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
    /// GNU-style unwind-protect: cleanup function popped from TOS.
    UnwindProtectFn { cleanup: Value },
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
    DynamicBinding { name: String, restored_value: Value },
    CurrentBuffer { buffer_id: BufferId },
    Excursion { buffer_id: BufferId, marker_id: u64 },
    Restriction(SavedRestriction),
}

/// The bytecode VM execution engine.
///
/// Operates on an Evaluator's obarray and dynamic binding stack.
pub struct Vm<'a> {
    obarray: &'a mut Obarray,
    dynamic: &'a mut Vec<OrderedSymMap>,
    lexenv: &'a mut Value,
    #[allow(dead_code)]
    features: &'a mut Vec<SymId>,
    custom: &'a mut CustomManager,
    buffers: &'a mut BufferManager,
    category_manager: &'a mut CategoryManager,
    frames: &'a mut FrameManager,
    coding_systems: &'a mut CodingSystemManager,
    match_data: &'a mut Option<MatchData>,
    watchers: &'a mut VariableWatcherList,
    /// Active catch tags from the evaluator — shared with interpreter
    /// so throws can check for matching catches across eval/VM boundaries.
    catch_tags: &'a mut Vec<Value>,
    /// Values that must remain GC-visible while the VM crosses into evaluator
    /// code that may trigger collection.
    gc_roots: Vec<Value>,
    depth: usize,
    max_depth: usize,
}

impl<'a> Vm<'a> {
    pub fn new(
        obarray: &'a mut Obarray,
        dynamic: &'a mut Vec<OrderedSymMap>,
        lexenv: &'a mut Value,
        features: &'a mut Vec<SymId>,
        custom: &'a mut CustomManager,
        buffers: &'a mut BufferManager,
        category_manager: &'a mut CategoryManager,
        frames: &'a mut FrameManager,
        coding_systems: &'a mut CodingSystemManager,
        match_data: &'a mut Option<MatchData>,
        watchers: &'a mut VariableWatcherList,
        catch_tags: &'a mut Vec<Value>,
    ) -> Self {
        Self {
            obarray,
            dynamic,
            lexenv,
            features,
            custom,
            buffers,
            category_manager,
            frames,
            coding_systems,
            match_data,
            watchers,
            catch_tags,
            gc_roots: Vec::new(),
            depth: 0,
            max_depth: 1600,
        }
    }

    /// Set the current depth and max_depth (inherited from the Evaluator).
    pub fn set_depth(&mut self, depth: usize, max_depth: usize) {
        self.depth = depth;
        self.max_depth = max_depth;
    }

    /// Get the current depth (to sync back to the Evaluator).
    pub fn get_depth(&self) -> usize {
        self.depth
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
                Handler::UnwindProtectFn { cleanup } => out.push(*cleanup),
            }
        }
    }

    fn collect_specpdl_roots(specpdl: &[VmUnwindEntry], out: &mut Vec<Value>) {
        for entry in specpdl {
            if let VmUnwindEntry::DynamicBinding { restored_value, .. } = entry {
                out.push(*restored_value);
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
        self.depth += 1;
        if self.depth > self.max_depth {
            self.depth -= 1;
            return Err(signal(
                "excessive-lisp-nesting",
                vec![Value::Int(self.max_depth as i64)],
            ));
        }

        let result = self.run_frame(func, args, func_value);
        self.depth -= 1;
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

        // No arity check here — the bytecode itself handles parameter layout
        // via StackRef/StackSet. Missing args become nil-padded slots, extra
        // args are collected into &rest or sit unused. This matches GNU Emacs
        // bytecode calling convention behavior.

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
            let mut frame = OrderedSymMap::new();
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

            if let Some(env) = func.env {
                // Closure: prepend param bindings onto the captured lexenv
                let saved_lexenv = std::mem::replace(self.lexenv, env);
                for (sym_id, val) in frame.iter() {
                    *self.lexenv = lexenv_prepend(*self.lexenv, *sym_id, *val);
                }
                let result = self.run_loop(func, &mut stack, &mut pc, &mut handlers, &mut specpdl);
                *self.lexenv = saved_lexenv;
                let cleanup = self.unwind_specpdl_all(&mut specpdl);
                return merge_result_with_cleanup(result, cleanup);
            }

            self.dynamic.push(frame);
            let result = self.run_loop(func, &mut stack, &mut pc, &mut handlers, &mut specpdl);
            self.dynamic.pop();
            let cleanup = self.unwind_specpdl_all(&mut specpdl);
            return merge_result_with_cleanup(result, cleanup);
        }

        // No params: set up lexenv if closure, then run
        let saved_lexenv = func.env.map(|env| std::mem::replace(self.lexenv, env));

        let result = self.run_loop(func, &mut stack, &mut pc, &mut handlers, &mut specpdl);

        if let Some(old) = saved_lexenv {
            *self.lexenv = old;
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
                    let mut frame = OrderedSymMap::new();
                    frame.insert(intern(&name), val);
                    self.dynamic.push(frame);
                    specpdl.push(VmUnwindEntry::DynamicBinding {
                        name: name.clone(),
                        restored_value: old_value,
                    });
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
                    if let Some(buffer_id) = self.buffers.current_buffer().map(|buffer| buffer.id) {
                        specpdl.push(VmUnwindEntry::CurrentBuffer { buffer_id });
                    }
                }
                Op::SaveExcursion => {
                    if let Some((buffer_id, point)) = self
                        .buffers
                        .current_buffer()
                        .map(|buffer| (buffer.id, buffer.pt))
                    {
                        let marker_id =
                            self.buffers
                                .create_marker(buffer_id, point, InsertionType::Before);
                        specpdl.push(VmUnwindEntry::Excursion {
                            buffer_id,
                            marker_id,
                        });
                    }
                }
                Op::SaveRestriction => {
                    if let Some((buffer_id, begv, zv, len)) = self
                        .buffers
                        .current_buffer()
                        .map(|buffer| (buffer.id, buffer.begv, buffer.zv, buffer.text.len()))
                    {
                        let entry = if begv == 0 && zv == len {
                            VmUnwindEntry::Restriction(SavedRestriction::None { buffer_id })
                        } else {
                            let beg_marker =
                                self.buffers
                                    .create_marker(buffer_id, begv, InsertionType::Before);
                            let end_marker =
                                self.buffers
                                    .create_marker(buffer_id, zv, InsertionType::After);
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
                    stack.push(vm_try!(arith_add(&a, &b)));
                }
                Op::Sub => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(vm_try!(arith_sub(&a, &b)));
                }
                Op::Mul => {
                    let b = stack.pop().unwrap_or(Value::Int(1));
                    let a = stack.pop().unwrap_or(Value::Int(1));
                    stack.push(vm_try!(arith_mul(&a, &b)));
                }
                Op::Div => {
                    let b = stack.pop().unwrap_or(Value::Int(1));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(vm_try!(arith_div(&a, &b)));
                }
                Op::Rem => {
                    let b = stack.pop().unwrap_or(Value::Int(1));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(vm_try!(arith_rem(&a, &b)));
                }
                Op::Add1 => {
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(vm_try!(arith_add1(&a)));
                }
                Op::Sub1 => {
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(vm_try!(arith_sub1(&a)));
                }
                Op::Negate => {
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(vm_try!(arith_negate(&a)));
                }

                // -- Comparison --
                Op::Eqlsign => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(Value::bool(vm_try!(num_eq(&a, &b))));
                }
                Op::Gtr => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(Value::bool(vm_try!(num_cmp(&a, &b)) > 0));
                }
                Op::Lss => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(Value::bool(vm_try!(num_cmp(&a, &b)) < 0));
                }
                Op::Leq => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(Value::bool(vm_try!(num_cmp(&a, &b)) <= 0));
                }
                Op::Geq => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(Value::bool(vm_try!(num_cmp(&a, &b)) >= 0));
                }
                Op::Max => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(if vm_try!(num_cmp(&a, &b)) >= 0 { a } else { b });
                }
                Op::Min => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(if vm_try!(num_cmp(&a, &b)) <= 0 { a } else { b });
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
                    self.catch_tags.push(tag);
                }
                Op::PopHandler => {
                    if let Some(handler) = handlers.pop() {
                        match handler {
                            Handler::Catch { .. } => {
                                // Remove from evaluator's catch_tags registry.
                                self.catch_tags.pop();
                            }
                            Handler::UnwindProtectFn { cleanup } => {
                                // GNU-style: call the cleanup function.
                                let _ = vm_try!(self.call_function(cleanup, vec![]));
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
                    handlers.push(Handler::UnwindProtectFn { cleanup });
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
                        closure.env = Some(*self.lexenv);
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
                    self.obarray
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
            let mut lexenv_val = *self.lexenv;
            Self::replace_alias_refs_in_value(
                &mut lexenv_val,
                first_arg,
                &replacement,
                &mut visited,
            );
            *self.lexenv = lexenv_val;
        }
        for frame in self.dynamic.iter_mut() {
            for value in frame.values_mut() {
                Self::replace_alias_refs_in_value(value, first_arg, &replacement, &mut visited);
            }
        }
        if let Some(buf) = self.buffers.current_buffer_mut() {
            for value in buf.properties.values_mut() {
                Self::replace_alias_refs_in_value(value, first_arg, &replacement, &mut visited);
            }
        }

        let symbols: Vec<String> = self
            .obarray
            .all_symbols()
            .into_iter()
            .map(str::to_string)
            .collect();
        for name in symbols {
            if let Some(symbol) = self.obarray.get_mut(&name) {
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
        if name == "nil" {
            return Ok(Value::Nil);
        }
        if name == "t" {
            return Ok(Value::True);
        }
        if name.starts_with(':') {
            return Ok(Value::Keyword(intern(name)));
        }

        // Check lexenv
        let name_id = intern(name);
        if let Some(val) = lexenv_lookup(*self.lexenv, name_id) {
            return Ok(val);
        }

        // Check dynamic
        for frame in self.dynamic.iter().rev() {
            if let Some(val) = frame.get(&name_id) {
                return Ok(*val);
            }
        }

        // Obarray
        if let Some(val) = self.obarray.symbol_value(name) {
            return Ok(*val);
        }

        Err(signal("void-variable", vec![Value::symbol(name)]))
    }

    fn assign_var(&mut self, name: &str, value: Value) -> Result<(), Flow> {
        let name_id = intern(name);
        // Check lexenv
        if let Some(cell_id) = lexenv_assq(*self.lexenv, name_id) {
            lexenv_set(cell_id, value);
            return Ok(());
        }
        // Check dynamic
        for frame in self.dynamic.iter_mut().rev() {
            if frame.contains_key(&name_id) {
                frame.insert(name_id, value);
                return Ok(());
            }
        }
        // Fall through to obarray
        self.obarray.set_symbol_value(name, value);
        self.run_variable_watchers(name, &value, &Value::Nil, "set")
    }

    fn run_variable_watchers(
        &mut self,
        name: &str,
        new_value: &Value,
        old_value: &Value,
        operation: &str,
    ) -> Result<(), Flow> {
        if !self.watchers.has_watchers(name) {
            return Ok(());
        }
        let calls =
            self.watchers
                .notify_watchers(name, new_value, old_value, operation, &Value::Nil);
        for (callback, args) in calls {
            let mut callback_roots = Vec::with_capacity(args.len() + 1);
            callback_roots.push(callback);
            callback_roots.extend(args.iter().copied());
            let _ =
                self.with_extra_roots(&callback_roots, |vm| vm.call_function(callback, args))?;
        }
        Ok(())
    }

    fn ensure_selected_frame_id(&mut self) -> FrameId {
        if let Some(fid) = self.frames.selected_frame().map(|frame| frame.id) {
            return fid;
        }

        let buf_id = self
            .buffers
            .current_buffer()
            .map(|buffer| buffer.id)
            .unwrap_or_else(|| self.buffers.create_buffer("*scratch*"));
        let fid = self.frames.create_frame("F1", 640, 384, buf_id);
        let minibuffer_buf_id = self
            .buffers
            .find_buffer_by_name(" *Minibuf-0*")
            .unwrap_or_else(|| self.buffers.create_buffer(" *Minibuf-0*"));
        if let Some(frame) = self.frames.get_mut(fid) {
            frame.parameters.insert("width".to_string(), Value::Int(80));
            frame
                .parameters
                .insert("height".to_string(), Value::Int(25));
            if let Some(Window::Leaf {
                window_start,
                point,
                ..
            }) = frame.find_window_mut(frame.selected_window)
            {
                *window_start = 1;
                *point = 1;
            }
            if let Some(minibuffer_leaf) = frame.minibuffer_leaf.as_mut() {
                minibuffer_leaf.set_buffer(minibuffer_buf_id);
            }
        }
        fid
    }

    fn resolve_frame_id(&mut self, arg: Option<&Value>, predicate: &str) -> Result<FrameId, Flow> {
        match arg {
            None | Some(Value::Nil) => Ok(self.ensure_selected_frame_id()),
            Some(Value::Int(n)) => {
                let fid = FrameId(*n as u64);
                if self.frames.get(fid).is_some() {
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
                if self.frames.get(fid).is_some() {
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
        if let Some(value) = self.obarray.symbol_value("global-map").copied() {
            if crate::emacs_core::keymap::is_list_keymap(&value) {
                return value;
            }
        }
        let keymap = crate::emacs_core::keymap::make_list_keymap();
        self.obarray.set_symbol_value("global-map", keymap);
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
        let Some(frame) = self.frames.get(FrameId(id)) else {
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
        crate::emacs_core::builtins::symbols::builtin_fboundp_in_obarray(self.obarray, args)
    }

    fn bind_params(
        &self,
        params: &LambdaParams,
        args: Vec<Value>,
        func_value: Value,
    ) -> Result<OrderedSymMap, Flow> {
        let mut frame = OrderedSymMap::new();
        let mut arg_idx = 0;

        if args.len() < params.min_arity() {
            tracing::warn!(
                "wrong-number-of-arguments (vm too few): got {} args, min={}, params={:?}",
                args.len(),
                params.min_arity(),
                params
            );
            return Err(signal(
                "wrong-number-of-arguments",
                vec![func_value, Value::Int(args.len() as i64)],
            ));
        }
        if let Some(max) = params.max_arity() {
            if args.len() > max {
                tracing::warn!(
                    "wrong-number-of-arguments (vm too many): got {} args, max={}, params={:?}",
                    args.len(),
                    max,
                    params
                );
                return Err(signal(
                    "wrong-number-of-arguments",
                    vec![func_value, Value::Int(args.len() as i64)],
                ));
            }
        }

        for param in &params.required {
            frame.insert(*param, args[arg_idx]);
            arg_idx += 1;
        }
        for param in &params.optional {
            if arg_idx < args.len() {
                frame.insert(*param, args[arg_idx]);
                arg_idx += 1;
            } else {
                frame.insert(*param, Value::Nil);
            }
        }
        if let Some(rest_name) = params.rest {
            let rest_args: Vec<Value> = args[arg_idx..].to_vec();
            frame.insert(rest_name, Value::list(rest_args));
        }
        Ok(frame)
    }

    fn call_function(&mut self, func_val: Value, args: Vec<Value>) -> EvalResult {
        match func_val {
            Value::ByteCode(_) => {
                let bc_data = func_val.get_bytecode_data().unwrap().clone();
                self.execute_with_func_value(&bc_data, args, func_val)
            }
            Value::Lambda(_) => {
                // Fall back to tree-walking for non-compiled lambdas
                // This creates a temporary evaluator context
                // Clone all needed data from heap BEFORE any &mut self calls
                let lambda_data = func_val.get_lambda_data().unwrap().clone();
                let frame = self.bind_params(&lambda_data.params, args, func_val)?;

                let saved_lexenv = if let Some(env) = lambda_data.env {
                    let old = std::mem::replace(self.lexenv, env);
                    // Prepend param bindings onto captured env
                    for (sym_id, val) in frame.iter() {
                        *self.lexenv = lexenv_prepend(*self.lexenv, *sym_id, *val);
                    }
                    Some(old)
                } else {
                    self.dynamic.push(frame);
                    None
                };

                // Execute lambda body forms
                let mut result = Value::Nil;
                let has_lexenv = saved_lexenv.is_some();
                for form in lambda_data.body.iter() {
                    // We need to eval Expr — but we only have a VM.
                    // Compile the body on-the-fly and execute.
                    let mut compiler = super::compiler::Compiler::new(has_lexenv);
                    let compiled = compiler.compile_toplevel(form);
                    result = self.execute_inline(&compiled)?;
                }

                if let Some(old_lexenv) = saved_lexenv {
                    *self.lexenv = old_lexenv;
                } else {
                    self.dynamic.pop();
                }
                Ok(result)
            }
            Value::Subr(id) => self.dispatch_vm_builtin(resolve_sym(id), args),
            Value::Symbol(id) => {
                let name = resolve_sym(id);
                // Try obarray function cell
                if let Some(func) = self.obarray.symbol_function(name).cloned() {
                    return self.call_function(func, args);
                }
                // Try builtin
                self.dispatch_vm_builtin(name, args)
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
                if let Some(res) = resolve_throw_target(handlers, &mut self.catch_tags, &tag) {
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
                if !tag.is_nil() && self.catch_tags.iter().rev().any(|t| eq_value(t, &tag)) {
                    return Err(Flow::Throw { tag, value });
                }
                Err(signal("no-catch", vec![tag, value]))
            }
            Flow::Signal(sig) => {
                if let Some(res) =
                    resolve_signal_target(handlers, &mut self.catch_tags, self.obarray, &sig)
                {
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
                self.dynamic.pop();
                self.run_variable_watchers(&name, &restored_value, &Value::Nil, "unlet")?;
            }
            VmUnwindEntry::CurrentBuffer { buffer_id } => {
                self.buffers.set_current(buffer_id);
            }
            VmUnwindEntry::Excursion {
                buffer_id,
                marker_id,
            } => {
                if self.buffers.get(buffer_id).is_some() {
                    self.buffers.set_current(buffer_id);
                    if let Some(saved_pt) = self.buffers.marker_position(buffer_id, marker_id) {
                        if let Some(buffer) = self.buffers.get_mut(buffer_id) {
                            buffer.goto_char(saved_pt);
                        }
                    }
                }
                self.buffers.remove_marker(marker_id);
            }
            VmUnwindEntry::Restriction(saved) => self.restore_saved_restriction(saved),
        }
        Ok(())
    }

    fn restore_saved_restriction(&mut self, saved: SavedRestriction) {
        match saved {
            SavedRestriction::None { buffer_id } => {
                if let Some(buffer) = self.buffers.get_mut(buffer_id) {
                    buffer.begv = 0;
                    buffer.zv = buffer.text.len();
                    buffer.pt = buffer.pt.clamp(buffer.begv, buffer.zv);
                }
            }
            SavedRestriction::Markers {
                buffer_id,
                beg_marker,
                end_marker,
            } => {
                let beg = self.buffers.marker_position(buffer_id, beg_marker);
                let end = self.buffers.marker_position(buffer_id, end_marker);
                if let (Some(begv), Some(zv)) = (beg, end) {
                    if let Some(buffer) = self.buffers.get_mut(buffer_id) {
                        buffer.begv = begv.min(buffer.text.len());
                        buffer.zv = zv.min(buffer.text.len());
                        if buffer.begv > buffer.zv {
                            std::mem::swap(&mut buffer.begv, &mut buffer.zv);
                        }
                        buffer.pt = buffer.pt.clamp(buffer.begv, buffer.zv);
                    }
                }
                self.buffers.remove_marker(beg_marker);
                self.buffers.remove_marker(end_marker);
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
                    if !self.obarray.boundp(&sym_name) {
                        self.obarray.set_symbol_value(&sym_name, args[0]);
                    }
                    self.obarray.make_special(&sym_name);
                    return Ok(Value::symbol(sym_name));
                }
                return Ok(Value::Nil);
            }
            "%%defconst" => {
                if args.len() >= 2 {
                    let sym_name = args[1].as_symbol_name().unwrap_or("nil").to_string();
                    self.obarray.set_symbol_value(&sym_name, args[0]);
                    let sym = self.obarray.get_or_intern(&sym_name);
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
                if !tag.is_nil() && self.catch_tags.iter().rev().any(|t| eq_value(t, &tag)) {
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
            "modify-category-entry" => Some(
                crate::emacs_core::category::modify_category_entry_in_manager(
                    self.category_manager,
                    args,
                ),
            ),
            "modify-syntax-entry" => Some(
                crate::emacs_core::syntax::modify_syntax_entry_in_buffers(self.buffers, args),
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
            "lookup-key" => Some(
                crate::emacs_core::builtins::keymaps::builtin_lookup_key_in_obarray(
                    &*self.obarray,
                    args,
                ),
            ),
            "keymapp" => Some(
                crate::emacs_core::builtins::keymaps::builtin_keymapp_in_obarray(
                    &*self.obarray,
                    args,
                ),
            ),
            "define-key" => Some((|| -> EvalResult {
                builtins::expect_min_args("define-key", args, 3)?;
                builtins::expect_max_args("define-key", args, 4)?;
                let keymap =
                    crate::emacs_core::builtins::keymaps::expect_keymap_in_obarray(
                        self.obarray,
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
                        self.obarray,
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
                Ok(self
                    .obarray
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
                        self.obarray,
                        sym,
                    )
                {
                    let plist =
                        crate::emacs_core::builtins::collections::builtin_plist_put(vec![
                            raw, args[1], value,
                        ])?;
                    crate::emacs_core::builtins::symbols::set_symbol_raw_plist_in_obarray(
                        self.obarray,
                        sym,
                        plist,
                    );
                    return Ok(value);
                }
                self.obarray.put_property_id(sym, prop, value);
                Ok(value)
                })())
            }
            "default-toplevel-value" => Some(
                crate::emacs_core::builtins::symbols::builtin_default_toplevel_value_in_obarray(
                    self.obarray,
                    args.to_vec(),
                ),
            ),
            "set-default-toplevel-value" => {
                let symbol = args
                    .first()
                    .copied()
                    .and_then(|value| crate::emacs_core::builtins::symbols::expect_symbol_id(&value).ok());
                let resolved_name = symbol.and_then(|symbol| {
                    crate::emacs_core::builtins::symbols::resolve_variable_alias_id_in_obarray(
                        self.obarray,
                        symbol,
                    )
                    .ok()
                    .map(resolve_sym)
                });
                if resolved_name.is_some_and(|name| self.watchers.has_watchers(name)) {
                    return None;
                }
                Some(
                    crate::emacs_core::builtins::symbols::builtin_set_default_toplevel_value_in_obarray(
                        self.obarray,
                        args.to_vec(),
                    ),
                )
            }
            "internal--define-uninitialized-variable" => Some(
                crate::emacs_core::builtins::symbols::builtin_internal_define_uninitialized_variable_in_obarray(
                    self.obarray,
                    args.to_vec(),
                ),
            ),
            "set-default" => {
                let symbol = args.first().copied().and_then(|value| match value {
                    Value::Nil => Some(intern("nil")),
                    Value::True => Some(intern("t")),
                    Value::Symbol(id) | Value::Keyword(id) => Some(id),
                    _ => None,
                });
                let resolved_name = symbol.and_then(|symbol| {
                    crate::emacs_core::builtins::symbols::resolve_variable_alias_id_in_obarray(
                        self.obarray,
                        symbol,
                    )
                    .ok()
                    .map(resolve_sym)
                });
                if resolved_name.is_some_and(|name| self.watchers.has_watchers(name)) {
                    return None;
                }
                Some(crate::emacs_core::custom::builtin_set_default_in_obarray(
                    self.obarray,
                    args.to_vec(),
                ))
            }
            "make-variable-buffer-local" => Some(
                crate::emacs_core::custom::builtin_make_variable_buffer_local_with_state(
                    self.obarray,
                    self.custom,
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
                self.obarray.intern(&name);
                Ok(Value::symbol(name))
            })()),
            "mapcar" => Some(self.builtin_mapcar_fast(args)),
            "fboundp" => Some(self.builtin_fboundp_fast(args)),
            "frame-list" => Some(self.builtin_frame_list_fast(args)),
            "framep" => Some(self.builtin_framep_fast(args)),
            "frame-parameter" => Some(self.builtin_frame_parameter_fast(args)),
            "define-coding-system-internal" => Some(
                crate::emacs_core::coding::builtin_define_coding_system_internal(
                    self.coding_systems,
                    args.to_vec(),
                ),
            ),
            "define-coding-system-alias" => Some(
                crate::emacs_core::coding::builtin_define_coding_system_alias(
                    self.coding_systems,
                    args.to_vec(),
                ),
            ),
            "string-match" => Some({
                let case_fold = self
                    .lookup_var("case-fold-search")
                    .map(|value| !value.is_nil())
                    .unwrap_or(true);
                crate::emacs_core::builtins::search::builtin_string_match_with_state(
                    case_fold,
                    self.match_data,
                    args,
                )
            }),
            "string-match-p" => Some({
                let case_fold = self
                    .lookup_var("case-fold-search")
                    .map(|value| !value.is_nil())
                    .unwrap_or(true);
                crate::emacs_core::builtins::search::builtin_string_match_p_with_case_fold(
                    case_fold, args,
                )
            }),
            "match-beginning" => Some(
                crate::emacs_core::builtins::search::builtin_match_beginning_with_state(
                    self.buffers.current_buffer(),
                    self.match_data,
                    args,
                ),
            ),
            "match-end" => Some(
                crate::emacs_core::builtins::search::builtin_match_end_with_state(
                    self.buffers.current_buffer(),
                    self.match_data,
                    args,
                ),
            ),
            "match-data" => Some(
                crate::emacs_core::builtins::search::builtin_match_data_with_state(
                    self.match_data,
                    args,
                ),
            ),
            "set-match-data" => Some(
                crate::emacs_core::builtins::search::builtin_set_match_data_with_state(
                    self.match_data,
                    args,
                ),
            ),
            _ => None,
        }
    }

    /// Dispatch builtins that require evaluator context by running them
    /// on a temporary evaluator mirrored from the VM's current obarray/env.
    fn dispatch_vm_builtin_eval(&mut self, name: &str, args: Vec<Value>) -> Option<EvalResult> {
        use crate::emacs_core::intern::with_saved_interner;
        use crate::emacs_core::value::{current_heap_ptr, set_current_heap, with_saved_heap};
        let trace_vm_builtins = std::env::var_os("NEOVM_TRACE_VM_BUILTINS").is_some();
        let trace_load_file_name = if trace_vm_builtins {
            self.obarray
                .symbol_value("load-file-name")
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "<unknown>".to_string())
        } else {
            String::new()
        };
        let trace_start = trace_vm_builtins.then(std::time::Instant::now);
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
        // Safety: original_heap_ptr was set by the parent Evaluator's
        // setup_thread_locals() and points to a valid, exclusively-owned
        // LispHeap inside the parent's Box<LispHeap>.  The parent Evaluator
        // is alive on the stack (it created this VM) and no other code
        // accesses it while the VM is running.
        unsafe {
            std::mem::swap(&mut *eval.heap, &mut *original_heap_ptr);
        }
        // Point thread-local at eval.heap which now holds the real data.
        set_current_heap(&mut eval.heap);

        eval.obarray = self.obarray.clone();
        eval.dynamic = self.dynamic.clone();
        eval.lexenv = *self.lexenv;
        eval.features = self.features.clone();
        eval.catch_tags = self.catch_tags.clone();
        eval.custom = self.custom.clone();
        eval.buffers = self.buffers.clone();
        std::mem::swap(self.frames, &mut eval.frames);
        eval.match_data = self.match_data.clone();
        eval.depth = self.depth;
        eval.max_depth = self.max_depth;
        std::mem::swap(self.coding_systems, &mut eval.coding_systems);
        std::mem::swap(self.watchers, &mut eval.watchers);
        let saved_temp_roots = eval.save_temp_roots();
        for root in &self.gc_roots {
            eval.push_temp_root(*root);
        }
        for arg in &args {
            eval.push_temp_root(*arg);
        }

        let result = builtins::dispatch_builtin(&mut eval, name, args);
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

        std::mem::swap(self.obarray, &mut eval.obarray);
        std::mem::swap(self.dynamic, &mut eval.dynamic);
        std::mem::swap(self.lexenv, &mut eval.lexenv);
        std::mem::swap(self.features, &mut eval.features);
        std::mem::swap(self.custom, &mut eval.custom);
        std::mem::swap(self.buffers, &mut eval.buffers);
        std::mem::swap(self.frames, &mut eval.frames);
        std::mem::swap(self.match_data, &mut eval.match_data);
        std::mem::swap(self.coding_systems, &mut eval.coding_systems);
        std::mem::swap(self.watchers, &mut eval.watchers);
        eval.restore_temp_roots(saved_temp_roots);
        self.depth = eval.depth;

        // Swap the heap data back to its original location so the parent
        // Evaluator's Box<LispHeap> is consistent when we return.  Any
        // objects allocated during the builtin are now in the original heap.
        unsafe {
            std::mem::swap(&mut *eval.heap, &mut *original_heap_ptr);
        }
        // Restore thread-local to the original location.
        unsafe {
            set_current_heap(&mut *original_heap_ptr);
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
/// from `UnwindProtectFn` handlers that were unwound through.
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
    let mut cleanups = Vec::new();
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
            Handler::UnwindProtectFn { cleanup } => {
                cleanups.push(cleanup);
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
    let mut cleanups = Vec::new();
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
            Handler::UnwindProtectFn { cleanup } => cleanups.push(cleanup),
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

fn arith_add(a: &Value, b: &Value) -> EvalResult {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_add(*b))),
        _ => {
            let a = a.as_number_f64().ok_or_else(|| {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("number-or-marker-p"), *a],
                )
            })?;
            let b = b.as_number_f64().ok_or_else(|| {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("number-or-marker-p"), *b],
                )
            })?;
            Ok(Value::Float(a + b, next_float_id()))
        }
    }
}

fn arith_sub(a: &Value, b: &Value) -> EvalResult {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_sub(*b))),
        _ => {
            let a = a.as_number_f64().ok_or_else(|| {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("number-or-marker-p"), *a],
                )
            })?;
            let b = b.as_number_f64().ok_or_else(|| {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("number-or-marker-p"), *b],
                )
            })?;
            Ok(Value::Float(a - b, next_float_id()))
        }
    }
}

fn arith_mul(a: &Value, b: &Value) -> EvalResult {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_mul(*b))),
        _ => {
            let a = a.as_number_f64().ok_or_else(|| {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("number-or-marker-p"), *a],
                )
            })?;
            let b = b.as_number_f64().ok_or_else(|| {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("number-or-marker-p"), *b],
                )
            })?;
            Ok(Value::Float(a * b, next_float_id()))
        }
    }
}

fn arith_div(a: &Value, b: &Value) -> EvalResult {
    match (a, b) {
        (Value::Int(_), Value::Int(0)) => Err(signal(
            "arith-error",
            vec![Value::string("Division by zero")],
        )),
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a / b)),
        _ => {
            let a = a.as_number_f64().ok_or_else(|| {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("number-or-marker-p"), *a],
                )
            })?;
            let b = b.as_number_f64().ok_or_else(|| {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("number-or-marker-p"), *b],
                )
            })?;
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

fn arith_add1(a: &Value) -> EvalResult {
    match a {
        Value::Int(n) => Ok(Value::Int(n.wrapping_add(1))),
        Value::Float(f, _) => Ok(Value::Float(f + 1.0, next_float_id())),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), *a],
        )),
    }
}

fn arith_sub1(a: &Value) -> EvalResult {
    match a {
        Value::Int(n) => Ok(Value::Int(n.wrapping_sub(1))),
        Value::Float(f, _) => Ok(Value::Float(f - 1.0, next_float_id())),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), *a],
        )),
    }
}

fn arith_negate(a: &Value) -> EvalResult {
    match a {
        Value::Int(n) => Ok(Value::Int(-n)),
        Value::Float(f, _) => Ok(Value::Float(-f, next_float_id())),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), *a],
        )),
    }
}

fn num_eq(a: &Value, b: &Value) -> Result<bool, Flow> {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => Ok(a == b),
        _ => {
            let a = a.as_number_f64().ok_or_else(|| {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("number-or-marker-p"), *a],
                )
            })?;
            let b = b.as_number_f64().ok_or_else(|| {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("number-or-marker-p"), *b],
                )
            })?;
            Ok(a == b)
        }
    }
}

fn num_cmp(a: &Value, b: &Value) -> Result<i32, Flow> {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => Ok(a.cmp(b) as i32),
        _ => {
            let a = a.as_number_f64().ok_or_else(|| {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("number-or-marker-p"), *a],
                )
            })?;
            let b = b.as_number_f64().ok_or_else(|| {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("number-or-marker-p"), *b],
                )
            })?;
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
            let s = with_heap(|h| h.get_string(*id).clone());
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
