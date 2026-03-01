//! Bytecode virtual machine — stack-based interpreter.

use std::collections::{HashMap, HashSet};

use super::chunk::ByteCodeFunction;
use super::opcode::Op;
use crate::buffer::BufferManager;
use crate::emacs_core::advice::VariableWatcherList;
use crate::emacs_core::builtins;
use crate::emacs_core::error::*;
use crate::emacs_core::regex::MatchData;
use crate::emacs_core::string_escape::{storage_char_len, storage_substring};
use crate::emacs_core::symbol::Obarray;
use crate::emacs_core::intern::{intern, resolve_sym, SymId};
use crate::emacs_core::value::*;

/// Handler frame for catch/condition-case/unwind-protect.
#[derive(Clone, Debug)]
#[allow(dead_code)]
enum Handler {
    /// catch: tag value, jump target.
    Catch { tag: Value, target: u32 },
    /// condition-case: handler patterns, jump target.
    ConditionCase { target: u32 },
    /// unwind-protect: cleanup target.
    UnwindProtect { target: u32 },
    /// GNU-style unwind-protect: cleanup function popped from TOS.
    UnwindProtectFn { cleanup: Value },
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
    buffers: &'a mut BufferManager,
    match_data: &'a mut Option<MatchData>,
    watchers: &'a mut VariableWatcherList,
    /// Active catch tags from the evaluator — shared with interpreter
    /// so throws can check for matching catches across eval/VM boundaries.
    catch_tags: &'a mut Vec<Value>,
    depth: usize,
    max_depth: usize,
}

impl<'a> Vm<'a> {
    pub fn new(
        obarray: &'a mut Obarray,
        dynamic: &'a mut Vec<OrderedSymMap>,
        lexenv: &'a mut Value,
        features: &'a mut Vec<SymId>,
        buffers: &'a mut BufferManager,
        match_data: &'a mut Option<MatchData>,
        watchers: &'a mut VariableWatcherList,
        catch_tags: &'a mut Vec<Value>,
    ) -> Self {
        Self {
            obarray,
            dynamic,
            lexenv,
            features,
            buffers,
            match_data,
            watchers,
            catch_tags,
            depth: 0,
            max_depth: 200,
        }
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

    fn run_frame(&mut self, func: &ByteCodeFunction, args: Vec<Value>, func_value: Value) -> EvalResult {
        let mut stack: Vec<Value> = Vec::with_capacity(func.max_stack as usize);
        let mut pc: usize = 0;
        let mut handlers: Vec<Handler> = Vec::new();
        let mut bind_count: usize = 0;
        let mut unbind_watch: Vec<(String, Value)> = Vec::new();

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
                frame.insert(*param, if arg_idx < nargs { args[arg_idx] } else { Value::Nil });
                arg_idx += 1;
            }
            for param in &func.params.optional {
                frame.insert(*param, if arg_idx < nargs { args[arg_idx] } else { Value::Nil });
                arg_idx += 1;
            }
            if let Some(rest_name) = func.params.rest {
                let rest_args: Vec<Value> = if arg_idx < nargs { args[arg_idx..].to_vec() } else { vec![] };
                frame.insert(rest_name, Value::list(rest_args));
            }

            if let Some(env) = func.env {
                // Closure: prepend param bindings onto the captured lexenv
                let saved_lexenv = std::mem::replace(self.lexenv, env);
                for (sym_id, val) in frame.iter() {
                    *self.lexenv = lexenv_prepend(*self.lexenv, *sym_id, *val);
                }
                let result = self.run_loop(
                    func,
                    &mut stack,
                    &mut pc,
                    &mut handlers,
                    &mut bind_count,
                    &mut unbind_watch,
                );
                *self.lexenv = saved_lexenv;
                let cleanup = self.cleanup_varbind_unwind(&mut bind_count, &mut unbind_watch);
                return merge_result_with_cleanup(result, cleanup);
            }

            self.dynamic.push(frame);
            let result = self.run_loop(
                func,
                &mut stack,
                &mut pc,
                &mut handlers,
                &mut bind_count,
                &mut unbind_watch,
            );
            self.dynamic.pop();
            let cleanup = self.cleanup_varbind_unwind(&mut bind_count, &mut unbind_watch);
            return merge_result_with_cleanup(result, cleanup);
        }

        // No params: set up lexenv if closure, then run
        let saved_lexenv = func.env.map(|env| std::mem::replace(self.lexenv, env));

        let result = self.run_loop(
            func,
            &mut stack,
            &mut pc,
            &mut handlers,
            &mut bind_count,
            &mut unbind_watch,
        );

        if let Some(old) = saved_lexenv {
            *self.lexenv = old;
        }
        let cleanup = self.cleanup_varbind_unwind(&mut bind_count, &mut unbind_watch);
        merge_result_with_cleanup(result, cleanup)
    }

    fn run_loop(
        &mut self,
        func: &ByteCodeFunction,
        stack: &mut Vec<Value>,
        pc: &mut usize,
        handlers: &mut Vec<Handler>,
        bind_count: &mut usize,
        unbind_watch: &mut Vec<(String, Value)>,
    ) -> EvalResult {
        let ops = &func.ops;
        let constants = &func.constants;

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
                    let val = self.lookup_var(&name)?;
                    stack.push(val);
                }
                Op::VarSet(idx) => {
                    let name = sym_name(constants, *idx);
                    let val = stack.pop().unwrap_or(Value::Nil);
                    self.assign_var(&name, val)?;
                }
                Op::VarBind(idx) => {
                    let name = sym_name(constants, *idx);
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let old_value = self.lookup_var(&name).unwrap_or(Value::Nil);
                    let mut frame = OrderedSymMap::new();
                    frame.insert(intern(&name), val);
                    self.dynamic.push(frame);
                    unbind_watch.push((name.clone(), old_value));
                    self.run_variable_watchers(&name, &val, &Value::Nil, "let")?;
                    *bind_count += 1;
                }
                Op::Unbind(n) => {
                    self.cleanup_varbind_unwind_n(*n as usize, bind_count, unbind_watch)?;
                }

                // -- Function calls --
                Op::Call(n) => {
                    let n = *n as usize;
                    let args_start = stack.len().saturating_sub(n);
                    let args: Vec<Value> = stack.drain(args_start..).collect();
                    let func_val = stack.pop().unwrap_or(Value::Nil);
                    let writeback_names = self.writeback_callable_names(&func_val);
                    let writeback_args = args.clone();
                    match self.call_function(func_val, args) {
                        Ok(result) => {
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
                        Err(Flow::Throw { tag, value }) => {
                            if let Some(res) = resolve_throw_target(handlers, &mut self.catch_tags, &tag) {
                                self.run_throw_cleanups(&res.cleanups);
                                stack.push(value);
                                *pc = res.target as usize;
                                continue;
                            }
                            return Err(Flow::Throw { tag, value });
                        }
                        Err(flow) => return Err(flow),
                    }
                }
                Op::Apply(n) => {
                    let n = *n as usize;
                    if n == 0 {
                        let func_val = stack.pop().unwrap_or(Value::Nil);
                        match self.call_function(func_val, vec![]) {
                            Ok(result) => stack.push(result),
                            Err(Flow::Throw { tag, value }) => {
                                if let Some(res) = resolve_throw_target(handlers, &mut self.catch_tags, &tag) {
                                    self.run_throw_cleanups(&res.cleanups);
                                    stack.push(value);
                                    *pc = res.target as usize;
                                    continue;
                                }
                                return Err(Flow::Throw { tag, value });
                            }
                            Err(flow) => return Err(flow),
                        }
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
                        match self.call_function(func_val, args) {
                            Ok(result) => {
                                if let Some((called_name, alias_target)) = writeback_names.as_ref()
                                {
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
                            Err(Flow::Throw { tag, value }) => {
                                if let Some(res) = resolve_throw_target(handlers, &mut self.catch_tags, &tag) {
                                    self.run_throw_cleanups(&res.cleanups);
                                    stack.push(value);
                                    *pc = res.target as usize;
                                    continue;
                                }
                                return Err(Flow::Throw { tag, value });
                            }
                            Err(flow) => return Err(flow),
                        }
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
                Op::Return => {
                    return Ok(stack.pop().unwrap_or(Value::Nil));
                }

                // -- Arithmetic --
                Op::Add => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(arith_add(&a, &b)?);
                }
                Op::Sub => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(arith_sub(&a, &b)?);
                }
                Op::Mul => {
                    let b = stack.pop().unwrap_or(Value::Int(1));
                    let a = stack.pop().unwrap_or(Value::Int(1));
                    stack.push(arith_mul(&a, &b)?);
                }
                Op::Div => {
                    let b = stack.pop().unwrap_or(Value::Int(1));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(arith_div(&a, &b)?);
                }
                Op::Rem => {
                    let b = stack.pop().unwrap_or(Value::Int(1));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(arith_rem(&a, &b)?);
                }
                Op::Add1 => {
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(arith_add1(&a)?);
                }
                Op::Sub1 => {
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(arith_sub1(&a)?);
                }
                Op::Negate => {
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(arith_negate(&a)?);
                }

                // -- Comparison --
                Op::Eqlsign => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(Value::bool(num_eq(&a, &b)?));
                }
                Op::Gtr => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(Value::bool(num_cmp(&a, &b)? > 0));
                }
                Op::Lss => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(Value::bool(num_cmp(&a, &b)? < 0));
                }
                Op::Leq => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(Value::bool(num_cmp(&a, &b)? <= 0));
                }
                Op::Geq => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(Value::bool(num_cmp(&a, &b)? >= 0));
                }
                Op::Max => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(if num_cmp(&a, &b)? >= 0 { a } else { b });
                }
                Op::Min => {
                    let b = stack.pop().unwrap_or(Value::Int(0));
                    let a = stack.pop().unwrap_or(Value::Int(0));
                    stack.push(if num_cmp(&a, &b)? <= 0 { a } else { b });
                }

                // -- List operations --
                Op::Car => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let result = self.dispatch_vm_builtin("car", vec![val])?;
                    stack.push(result);
                }
                Op::Cdr => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let result = self.dispatch_vm_builtin("cdr", vec![val])?;
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
                    stack.push(length_value(&val)?);
                }
                Op::Nth => {
                    let list = stack.pop().unwrap_or(Value::Nil);
                    let n = stack.pop().unwrap_or(Value::Int(0));
                    let result = self.dispatch_vm_builtin("nth", vec![n, list])?;
                    stack.push(result);
                }
                Op::Nthcdr => {
                    let list = stack.pop().unwrap_or(Value::Nil);
                    let n = stack.pop().unwrap_or(Value::Int(0));
                    let result = self.dispatch_vm_builtin("nthcdr", vec![n, list])?;
                    stack.push(result);
                }
                Op::Elt => {
                    let idx = stack.pop().unwrap_or(Value::Nil);
                    let seq = stack.pop().unwrap_or(Value::Nil);
                    let result = self.dispatch_vm_builtin("elt", vec![seq, idx])?;
                    stack.push(result);
                }
                Op::Setcar => {
                    let newcar = stack.pop().unwrap_or(Value::Nil);
                    let cell = stack.pop().unwrap_or(Value::Nil);
                    if let Value::Cons(c) = &cell {
                        with_heap_mut(|h| h.set_car(*c, newcar));
                        stack.push(newcar);
                    } else {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("consp"), cell],
                        ));
                    }
                }
                Op::Setcdr => {
                    let newcdr = stack.pop().unwrap_or(Value::Nil);
                    let cell = stack.pop().unwrap_or(Value::Nil);
                    if let Value::Cons(c) = &cell {
                        with_heap_mut(|h| h.set_cdr(*c, newcdr));
                        stack.push(newcdr);
                    } else {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("consp"), cell],
                        ));
                    }
                }
                Op::Nconc => {
                    let b = stack.pop().unwrap_or(Value::Nil);
                    let a = stack.pop().unwrap_or(Value::Nil);
                    let result = self.dispatch_vm_builtin("nconc", vec![a, b])?;
                    stack.push(result);
                }
                Op::Nreverse => {
                    let list = stack.pop().unwrap_or(Value::Nil);
                    let result = self.dispatch_vm_builtin("nreverse", vec![list])?;
                    stack.push(result);
                }
                Op::Member => {
                    let list = stack.pop().unwrap_or(Value::Nil);
                    let elt = stack.pop().unwrap_or(Value::Nil);
                    let result = self.dispatch_vm_builtin("member", vec![elt, list])?;
                    stack.push(result);
                }
                Op::Memq => {
                    let list = stack.pop().unwrap_or(Value::Nil);
                    let elt = stack.pop().unwrap_or(Value::Nil);
                    let result = self.dispatch_vm_builtin("memq", vec![elt, list])?;
                    stack.push(result);
                }
                Op::Assq => {
                    let alist = stack.pop().unwrap_or(Value::Nil);
                    let key = stack.pop().unwrap_or(Value::Nil);
                    let result = self.dispatch_vm_builtin("assq", vec![key, alist])?;
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
                    let result = self.dispatch_vm_builtin("concat", parts)?;
                    stack.push(result);
                }
                Op::Substring => {
                    let to = stack.pop().unwrap_or(Value::Nil);
                    let from = stack.pop().unwrap_or(Value::Int(0));
                    let array = stack.pop().unwrap_or(Value::Nil);
                    let result = substring_value(&array, &from, &to)?;
                    stack.push(result);
                }
                Op::StringEqual => {
                    let b = stack.pop().unwrap_or(Value::Nil);
                    let a = stack.pop().unwrap_or(Value::Nil);
                    let result = self.dispatch_vm_builtin("string=", vec![a, b])?;
                    stack.push(result);
                }
                Op::StringLessp => {
                    let b = stack.pop().unwrap_or(Value::Nil);
                    let a = stack.pop().unwrap_or(Value::Nil);
                    let result = self.dispatch_vm_builtin("string-lessp", vec![a, b])?;
                    stack.push(result);
                }

                // -- Vector operations --
                Op::Aref => {
                    let idx_val = stack.pop().unwrap_or(Value::Int(0));
                    let vec_val = stack.pop().unwrap_or(Value::Nil);
                    let result = builtins::builtin_aref(vec![vec_val, idx_val])?;
                    stack.push(result);
                }
                Op::Aset => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let idx_val = stack.pop().unwrap_or(Value::Int(0));
                    let vec_val = stack.pop().unwrap_or(Value::Nil);
                    let call_args = vec![vec_val, idx_val, val];
                    let result = builtins::builtin_aset(call_args.clone())?;
                    self.maybe_writeback_mutating_first_arg(
                        "aset", None, &call_args, &result, stack,
                    );
                    stack.push(result);
                }

                // -- Symbol operations --
                Op::SymbolValue => {
                    let sym = stack.pop().unwrap_or(Value::Nil);
                    let result = self.dispatch_vm_builtin("symbol-value", vec![sym])?;
                    stack.push(result);
                }
                Op::SymbolFunction => {
                    let sym = stack.pop().unwrap_or(Value::Nil);
                    let result = self.dispatch_vm_builtin("symbol-function", vec![sym])?;
                    stack.push(result);
                }
                Op::Set => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let sym = stack.pop().unwrap_or(Value::Nil);
                    let result = self.dispatch_vm_builtin("set", vec![sym, val])?;
                    stack.push(result);
                }
                Op::Fset => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let sym = stack.pop().unwrap_or(Value::Nil);
                    let result = self.dispatch_vm_builtin("fset", vec![sym, val])?;
                    stack.push(result);
                }
                Op::Get => {
                    let prop = stack.pop().unwrap_or(Value::Nil);
                    let sym = stack.pop().unwrap_or(Value::Nil);
                    let result = self.dispatch_vm_builtin("get", vec![sym, prop])?;
                    stack.push(result);
                }
                Op::Put => {
                    let val = stack.pop().unwrap_or(Value::Nil);
                    let prop = stack.pop().unwrap_or(Value::Nil);
                    let sym = stack.pop().unwrap_or(Value::Nil);
                    let result = self.dispatch_vm_builtin("put", vec![sym, prop, val])?;
                    stack.push(result);
                }

                // -- Error handling --
                Op::PushConditionCase(target) => {
                    handlers.push(Handler::ConditionCase { target: *target });
                }
                Op::PushConditionCaseRaw(target) => {
                    // GNU bytecode consumes the handler pattern operand from TOS.
                    stack.pop();
                    handlers.push(Handler::ConditionCase { target: *target });
                }
                Op::PushCatch(target) => {
                    let tag = stack.pop().unwrap_or(Value::Nil);
                    handlers.push(Handler::Catch {
                        tag,
                        target: *target,
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
                                match self.call_function(cleanup, vec![]) {
                                    Ok(_) => {}
                                    Err(Flow::Throw { tag, value }) => {
                                        if let Some(res) =
                                            resolve_throw_target(handlers, &mut self.catch_tags, &tag)
                                        {
                                            self.run_throw_cleanups(&res.cleanups);
                                            stack.push(value);
                                            *pc = res.target as usize;
                                            continue;
                                        }
                                        return Err(Flow::Throw { tag, value });
                                    }
                                    Err(flow) => return Err(flow),
                                }
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
                    if let Some(res) = resolve_throw_target(handlers, &mut self.catch_tags, &tag) {
                        self.run_throw_cleanups(&res.cleanups);
                        stack.push(val);
                        *pc = res.target as usize;
                        continue;
                    }
                    // No matching catch in VM handler stack.  Check evaluator
                    // catch_tags (catches established by the interpreter above us).
                    // If found → Flow::Throw (will be caught by sf_catch).
                    // If not → signal no-catch immediately (GNU Emacs semantics).
                    if self.catch_tags.iter().rev().any(|t| eq_value(t, &tag)) {
                        return Err(Flow::Throw { tag, value: val });
                    }
                    return Err(signal("no-catch", vec![tag, val]));
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
                    let result = self.dispatch_vm_builtin(&name, args)?;
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
            Self::replace_alias_refs_in_value(&mut lexenv_val, first_arg, &replacement, &mut visited);
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
        let calls = self
            .watchers
            .notify_watchers(name, new_value, old_value, operation, &Value::Nil);
        for (callback, args) in calls {
            let _ = self.call_function(callback, args)?;
        }
        Ok(())
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
                args.len(), params.min_arity(), params
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
                    args.len(), max, params
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
        let mut bind_count: usize = 0;
        let mut unbind_watch: Vec<(String, Value)> = Vec::new();
        let result = self.run_loop(
            func,
            &mut stack,
            &mut pc,
            &mut handlers,
            &mut bind_count,
            &mut unbind_watch,
        );
        let cleanup = self.cleanup_varbind_unwind(&mut bind_count, &mut unbind_watch);
        merge_result_with_cleanup(result, cleanup)
    }

    /// Run cleanup functions collected during throw resolution.
    fn run_throw_cleanups(&mut self, cleanups: &[Value]) {
        for cleanup in cleanups {
            let _ = self.call_function(*cleanup, vec![]);
        }
    }

    fn cleanup_varbind_unwind(
        &mut self,
        bind_count: &mut usize,
        unbind_watch: &mut Vec<(String, Value)>,
    ) -> Result<(), Flow> {
        self.cleanup_varbind_unwind_n(*bind_count, bind_count, unbind_watch)
    }

    fn cleanup_varbind_unwind_n(
        &mut self,
        count: usize,
        bind_count: &mut usize,
        unbind_watch: &mut Vec<(String, Value)>,
    ) -> Result<(), Flow> {
        for _ in 0..count {
            if *bind_count == 0 {
                break;
            }
            self.dynamic.pop();
            *bind_count -= 1;
            if let Some((name, restored_value)) = unbind_watch.pop() {
                self.run_variable_watchers(&name, &restored_value, &Value::Nil, "unlet")?;
            }
        }
        Ok(())
    }

    /// Dispatch to builtin functions from the VM.
    fn dispatch_vm_builtin(&mut self, name: &str, args: Vec<Value>) -> EvalResult {
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
                        signal(
                            "wrong-type-argument",
                            vec![Value::symbol("listp"), *last],
                        )
                    })?,
                    _ => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("listp"), *last],
                        ))
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
                        vec![
                            Value::Subr(intern("throw")),
                            Value::Int(args.len() as i64),
                        ],
                    ));
                }
                let tag = args[0];
                let value = args[1];
                // Check evaluator catch_tags for a matching catch.
                if self.catch_tags.iter().rev().any(|t| eq_value(t, &tag)) {
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

    /// Dispatch builtins that require evaluator context by running them
    /// on a temporary evaluator mirrored from the VM's current obarray/env.
    fn dispatch_vm_builtin_eval(&mut self, name: &str, args: Vec<Value>) -> Option<EvalResult> {
        use crate::emacs_core::intern::with_saved_interner;
        use crate::emacs_core::value::{with_saved_heap, current_heap_ptr, set_current_heap};
        // Evaluator::new() overwrites the thread-local heap/interner pointers.
        // Save and restore them so ObjIds/SymIds from the caller remain valid.
        let mut eval =
            with_saved_interner(|| with_saved_heap(crate::emacs_core::eval::Evaluator::new));

        // The temp evaluator owns a fresh empty heap, but all ObjIds in
        // args/obarray/dynamic/etc. belong to the ORIGINAL heap (the one
        // set as CURRENT_HEAP by the parent Evaluator).  Evaluator methods
        // like apply() and gc_collect() use self.heap, not the thread-local,
        // so we must swap the real heap data into the temp evaluator.
        let original_heap_ptr = current_heap_ptr();
        assert!(!original_heap_ptr.is_null(), "dispatch_vm_builtin_eval: no current heap");
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
        eval.buffers = self.buffers.clone();
        eval.match_data = self.match_data.clone();
        std::mem::swap(self.watchers, &mut eval.watchers);

        let result = builtins::dispatch_builtin(&mut eval, name, args);

        std::mem::swap(self.obarray, &mut eval.obarray);
        std::mem::swap(self.dynamic, &mut eval.dynamic);
        std::mem::swap(self.lexenv, &mut eval.lexenv);
        std::mem::swap(self.features, &mut eval.features);
        std::mem::swap(self.buffers, &mut eval.buffers);
        std::mem::swap(self.match_data, &mut eval.match_data);
        std::mem::swap(self.watchers, &mut eval.watchers);

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
            } => {
                // Remove from evaluator catch_tags registry (this catch is being unwound).
                catch_tags.pop();
                if eq_value(&catch_tag, tag) {
                    return Some(ThrowResolution { target, cleanups });
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
        Value::Str(id) => Ok(Value::Int(with_heap(|h| h.get_string(*id).chars().count()) as i64)),
        Value::Vector(v) => Ok(Value::Int(with_heap(|h| h.vector_len(*v)) as i64)),
        // In official Emacs, closures are vectors with layout:
        // [ARGS, BODY, ENV, nil, DOCSTRING] → always 5 slots
        Value::Lambda(_) => Ok(Value::Int(5)),
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
                        ))
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
            ))
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
                    ))
                }
            }
        };
        let idx = if raw < 0 { len + raw } else { raw };
        if idx < 0 || idx > len {
            return Err(signal(
                "args-out-of-range",
                vec![*array, *from, *to],
            ));
        }
        Ok(idx)
    };

    let start = normalize_index(from, 0)? as usize;
    let end = normalize_index(to, len)? as usize;
    if start > end {
        return Err(signal(
            "args-out-of-range",
            vec![*array, *from, *to],
        ));
    }

    match array {
        Value::Str(id) => {
            let s = with_heap(|h| h.get_string(*id).clone());
            let result = storage_substring(&s, start, end).ok_or_else(|| {
                signal(
                    "args-out-of-range",
                    vec![*array, *from, *to],
                )
            })?;
            Ok(Value::string(result))
        }
        Value::Vector(v) => {
            let data = with_heap(|h| h.get_vector(*v).clone());
            if end > data.len() {
                return Err(signal(
                    "args-out-of-range",
                    vec![*array, *from, *to],
                ));
            }
            Ok(Value::vector(data[start..end].to_vec()))
        }
        _ => unreachable!(),
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
mod tests {
    use super::*;
    use crate::emacs_core::bytecode::compiler::Compiler;
    use crate::emacs_core::parse_forms;

    fn vm_eval(src: &str) -> Result<Value, EvalError> {
        let forms = parse_forms(src).expect("parse");
        let mut compiler = Compiler::new(false);
        let mut obarray = Obarray::new();
        // Set up standard variables
        obarray.set_symbol_value("most-positive-fixnum", Value::Int(i64::MAX));
        obarray.set_symbol_value("most-negative-fixnum", Value::Int(i64::MIN));

        let mut dynamic: Vec<OrderedSymMap> = Vec::new();
        let mut lexenv: Value = Value::Nil;
        let mut features: Vec<SymId> = Vec::new();
        let mut buffers = crate::buffer::BufferManager::new();
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
                &mut buffers,
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

        let nthcdr_int_err =
            vm_eval("(nthcdr 'a '(1 2 3))").expect_err("nthcdr must type-check index");
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
}
