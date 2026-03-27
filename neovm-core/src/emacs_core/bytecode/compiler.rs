//! Bytecode compiler: transforms Expr AST into ByteCodeFunction.

use std::collections::HashSet;

use super::chunk::ByteCodeFunction;
use super::opcode::Op;
use crate::emacs_core::expr::Expr;
use crate::emacs_core::intern::{intern, resolve_sym};
use crate::emacs_core::value::{LambdaParams, Value, next_float_id};

/// A stack-local variable (function parameter accessed via StackRef).
struct StackLocal {
    name: String,
    slot: usize,
}

/// Lexical scope for function parameters on the VM stack.
struct LexScope {
    locals: Vec<StackLocal>,
    /// Names shadowed by VarBind (let/let*) — these should use VarRef, not StackRef.
    shadowed: HashSet<String>,
}

impl LexScope {
    /// Find the stack slot for a name, returning None if shadowed.
    fn find(&self, name: &str) -> Option<usize> {
        if self.shadowed.contains(name) {
            return None;
        }
        self.locals.iter().find(|l| l.name == name).map(|l| l.slot)
    }
}

/// Compiler state.
pub struct Compiler {
    /// Whether lexical-binding is active.
    lexical: bool,
    /// Set of known special (dynamically-scoped) variable names.
    specials: Vec<String>,
    /// Lexical scope for the current function's parameters.
    lex_scope: Option<LexScope>,
    /// Current stack depth (tracked to compute StackRef offsets).
    stack_depth: i32,
}

impl Compiler {
    pub fn new(lexical: bool) -> Self {
        Self {
            lexical,
            specials: Vec::new(),
            lex_scope: None,
            stack_depth: 0,
        }
    }

    /// Mark a variable as special (always dynamically bound).
    pub fn add_special(&mut self, name: &str) {
        if !self.specials.contains(&name.to_string()) {
            self.specials.push(name.to_string());
        }
    }

    #[allow(dead_code)]
    fn is_special(&self, name: &str) -> bool {
        self.specials.contains(&name.to_string())
    }

    /// Emit an opcode and update the tracked stack depth.
    fn emit_tracked(&mut self, func: &mut ByteCodeFunction, op: Op) {
        self.stack_depth += stack_delta(&op);
        func.emit(op);
    }

    /// Compile a top-level expression (not a function body).
    pub fn compile_toplevel(&mut self, expr: &Expr) -> ByteCodeFunction {
        let mut func = ByteCodeFunction::new(LambdaParams::simple(vec![]));
        func.lexical = self.lexical;
        self.stack_depth = 0;
        self.compile_expr(&mut func, expr, true);
        self.emit_tracked(&mut func, Op::Return);
        self.compute_max_stack(&mut func, 0);
        func
    }

    /// Compile a lambda expression into a ByteCodeFunction.
    pub fn compile_lambda(&mut self, params: &LambdaParams, body: &[Expr]) -> ByteCodeFunction {
        let mut func = ByteCodeFunction::new(params.clone());
        func.lexical = self.lexical;

        // Save outer scope state (handles nested lambdas)
        let saved_lex_scope = self.lex_scope.take();
        let saved_stack_depth = self.stack_depth;

        // Build LexScope mapping each param to its stack slot
        let mut locals = Vec::new();
        let mut slot = 0usize;
        for param in &params.required {
            locals.push(StackLocal {
                name: resolve_sym(*param).to_string(),
                slot,
            });
            slot += 1;
        }
        for param in &params.optional {
            locals.push(StackLocal {
                name: resolve_sym(*param).to_string(),
                slot,
            });
            slot += 1;
        }
        if let Some(rest) = params.rest {
            locals.push(StackLocal {
                name: resolve_sym(rest).to_string(),
                slot,
            });
            slot += 1;
        }

        let num_param_slots = slot;
        self.lex_scope = if num_param_slots > 0 {
            Some(LexScope {
                locals,
                shadowed: HashSet::new(),
            })
        } else {
            None
        };
        self.stack_depth = num_param_slots as i32;

        if body.is_empty() {
            self.emit_tracked(&mut func, Op::Nil);
        } else {
            for (i, form) in body.iter().enumerate() {
                let is_last = i == body.len() - 1;
                let need_value = is_last;
                self.compile_expr(&mut func, form, need_value);
            }
        }
        self.emit_tracked(&mut func, Op::Return);
        self.compute_max_stack(&mut func, num_param_slots);

        // Restore outer scope state
        self.lex_scope = saved_lex_scope;
        self.stack_depth = saved_stack_depth;

        func
    }

    /// Compile a single expression.
    /// `for_value`: whether the result is needed on the stack.
    fn compile_expr(&mut self, func: &mut ByteCodeFunction, expr: &Expr, for_value: bool) {
        match expr {
            Expr::Int(n) => {
                if for_value {
                    let idx = func.add_constant(Value::Int(*n));
                    self.emit_tracked(func, Op::Constant(idx));
                }
            }
            Expr::Float(f) => {
                if for_value {
                    let idx = func.add_constant(Value::Float(*f, next_float_id()));
                    self.emit_tracked(func, Op::Constant(idx));
                }
            }
            Expr::Str(s) => {
                if for_value {
                    let idx = func.add_constant(Value::string(s.clone()));
                    self.emit_tracked(func, Op::Constant(idx));
                }
            }
            Expr::Char(c) => {
                if for_value {
                    let idx = func.add_constant(Value::Char(*c));
                    self.emit_tracked(func, Op::Constant(idx));
                }
            }
            Expr::Keyword(id) => {
                if for_value {
                    let idx = func.add_constant(Value::Keyword(*id));
                    self.emit_tracked(func, Op::Constant(idx));
                }
            }
            Expr::Bool(true) => {
                if for_value {
                    self.emit_tracked(func, Op::True);
                }
            }
            Expr::Bool(false) => {
                if for_value {
                    self.emit_tracked(func, Op::Nil);
                }
            }
            Expr::Symbol(id) => {
                if for_value {
                    self.compile_symbol_ref(func, resolve_sym(*id));
                }
            }
            Expr::ReaderLoadFileName => {
                if for_value {
                    self.compile_symbol_ref(func, "load-file-name");
                }
            }
            Expr::Vector(items) => {
                if for_value {
                    // GNU Emacs treats vectors as self-evaluating objects, so
                    // `[remap ignore]` is data, not `(vector remap ignore)`.
                    let vals: Vec<Value> = items.iter().map(literal_to_value).collect();
                    let idx = func.add_constant(Value::vector(vals));
                    self.emit_tracked(func, Op::Constant(idx));
                }
            }
            Expr::List(items) => {
                self.compile_list(func, items, for_value);
            }
            Expr::DottedList(items, _last) => {
                // Treat as regular list call (dotted lists in source are rare)
                self.compile_list(func, items, for_value);
            }
            Expr::OpaqueValue(v) => {
                if for_value {
                    let idx = func.add_constant(*v);
                    self.emit_tracked(func, Op::Constant(idx));
                }
            }
        }
    }

    fn compile_symbol_ref(&mut self, func: &mut ByteCodeFunction, name: &str) {
        // Like GNU Emacs, a lambda parameter named `t` or `nil` shadows the
        // self-evaluating constant while the function body runs. Check the
        // current stack-local scope before lowering those names to literal
        // opcodes.
        if let Some(slot) = self.lex_scope.as_ref().and_then(|s| s.find(name)) {
            let offset = self.stack_depth as usize - slot - 1;
            self.emit_tracked(func, Op::StackRef(offset as u16));
            return;
        }

        match name {
            "nil" => self.emit_tracked(func, Op::Nil),
            "t" => self.emit_tracked(func, Op::True),
            _ if name.starts_with(':') => {
                let idx = func.add_constant(Value::Keyword(intern(name)));
                self.emit_tracked(func, Op::Constant(idx));
            }
            _ => {
                let idx = func.add_symbol(name);
                self.emit_tracked(func, Op::VarRef(idx));
            }
        }
    }

    fn compile_list(&mut self, func: &mut ByteCodeFunction, items: &[Expr], for_value: bool) {
        if items.is_empty() {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        }

        let (head, tail) = items.split_first().unwrap();

        if let Expr::Symbol(id) = head {
            let name = resolve_sym(*id);
            // Try special forms first
            if self.try_compile_special_form(func, name, tail, for_value) {
                return;
            }

            // Try dedicated opcodes for known builtins
            if for_value {
                if let Some(()) = self.try_compile_builtin_op(func, name, tail) {
                    return;
                }
            }

            // General function call
            // Push function reference, then args
            let name_idx = func.add_symbol(name);
            self.emit_tracked(func, Op::Constant(name_idx));
            for arg in tail {
                self.compile_expr(func, arg, true);
            }
            self.emit_tracked(func, Op::Call(tail.len() as u16));

            if !for_value {
                self.emit_tracked(func, Op::Pop);
            }
            return;
        }

        // Head is not a symbol — could be a lambda form
        if let Expr::List(lambda_form) = head {
            if let Some(Expr::Symbol(id)) = lambda_form.first() {
                if resolve_sym(*id) == "lambda" {
                    self.compile_expr(func, head, true);
                    for arg in tail {
                        self.compile_expr(func, arg, true);
                    }
                    self.emit_tracked(func, Op::Call(tail.len() as u16));
                    if !for_value {
                        self.emit_tracked(func, Op::Pop);
                    }
                    return;
                }
            }
        }

        // Fallback: evaluate head as function
        self.compile_expr(func, head, true);
        for arg in tail {
            self.compile_expr(func, arg, true);
        }
        self.emit_tracked(func, Op::Call(tail.len() as u16));
        if !for_value {
            self.emit_tracked(func, Op::Pop);
        }
    }

    /// Returns true if the special form was handled.
    fn try_compile_special_form(
        &mut self,
        func: &mut ByteCodeFunction,
        name: &str,
        tail: &[Expr],
        for_value: bool,
    ) -> bool {
        match name {
            "quote" => {
                if for_value {
                    if let Some(expr) = tail.first() {
                        let val = literal_to_value(expr);
                        let idx = func.add_constant(val);
                        self.emit_tracked(func, Op::Constant(idx));
                    } else {
                        self.emit_tracked(func, Op::Nil);
                    }
                }
                true
            }
            "progn" => {
                self.compile_progn(func, tail, for_value);
                true
            }
            "prog1" => {
                if tail.is_empty() {
                    if for_value {
                        self.emit_tracked(func, Op::Nil);
                    }
                } else {
                    self.compile_expr(func, &tail[0], for_value);
                    for form in &tail[1..] {
                        self.compile_expr(func, form, false);
                    }
                }
                true
            }
            "if" => {
                self.compile_if(func, tail, for_value);
                true
            }
            "and" => {
                self.compile_and(func, tail, for_value);
                true
            }
            "or" => {
                self.compile_or(func, tail, for_value);
                true
            }
            "cond" => {
                self.compile_cond(func, tail, for_value);
                true
            }
            "while" => {
                self.compile_while(func, tail);
                if for_value {
                    self.emit_tracked(func, Op::Nil);
                }
                true
            }
            "let" => {
                self.compile_let(func, tail, for_value);
                true
            }
            "let*" => {
                self.compile_let_star(func, tail, for_value);
                true
            }
            "setq" => {
                self.compile_setq(func, tail, for_value);
                true
            }
            "defun" => {
                self.compile_defun(func, tail, for_value);
                true
            }
            "defvar" => {
                self.compile_defvar(func, tail, for_value);
                true
            }
            "defconst" => {
                self.compile_defconst(func, tail, for_value);
                true
            }
            "lambda" | "function" => {
                if for_value {
                    self.compile_lambda_or_function(func, name, tail);
                }
                true
            }
            "funcall" => {
                if tail.is_empty() {
                    if for_value {
                        self.emit_tracked(func, Op::Nil);
                    }
                } else {
                    self.compile_expr(func, &tail[0], true);
                    for arg in &tail[1..] {
                        self.compile_expr(func, arg, true);
                    }
                    self.emit_tracked(func, Op::Call(tail.len().saturating_sub(1) as u16));
                    if !for_value {
                        self.emit_tracked(func, Op::Pop);
                    }
                }
                true
            }
            "when" => {
                if tail.is_empty() {
                    if for_value {
                        self.emit_tracked(func, Op::Nil);
                    }
                } else {
                    // (when COND BODY...) => (if COND (progn BODY...))
                    self.compile_expr(func, &tail[0], true);
                    let jump_false = func.current_offset();
                    self.emit_tracked(func, Op::GotoIfNil(0)); // placeholder
                    self.compile_progn(func, &tail[1..], for_value);
                    let jump_end = func.current_offset();
                    self.emit_tracked(func, Op::Goto(0)); // placeholder
                    let else_target = func.current_offset();
                    func.patch_jump(jump_false, else_target);
                    if for_value {
                        self.emit_tracked(func, Op::Nil);
                    }
                    let end_target = func.current_offset();
                    func.patch_jump(jump_end, end_target);
                }
                true
            }
            "unless" => {
                if tail.is_empty() {
                    if for_value {
                        self.emit_tracked(func, Op::Nil);
                    }
                } else {
                    self.compile_expr(func, &tail[0], true);
                    let jump_true = func.current_offset();
                    self.emit_tracked(func, Op::GotoIfNotNil(0)); // placeholder
                    self.compile_progn(func, &tail[1..], for_value);
                    let jump_end = func.current_offset();
                    self.emit_tracked(func, Op::Goto(0)); // placeholder
                    let else_target = func.current_offset();
                    func.patch_jump(jump_true, else_target);
                    if for_value {
                        self.emit_tracked(func, Op::Nil);
                    }
                    let end_target = func.current_offset();
                    func.patch_jump(jump_end, end_target);
                }
                true
            }
            "catch" => {
                self.compile_catch(func, tail, for_value);
                true
            }
            "unwind-protect" => {
                self.compile_unwind_protect(func, tail, for_value);
                true
            }
            "condition-case" => {
                self.compile_condition_case(func, tail, for_value);
                true
            }
            "interactive" | "declare" => {
                // Ignored
                if for_value {
                    self.emit_tracked(func, Op::Nil);
                }
                true
            }
            "dotimes" => {
                self.compile_dotimes(func, tail, for_value);
                true
            }
            "dolist" => {
                self.compile_dolist(func, tail, for_value);
                true
            }
            "save-excursion" => {
                self.compile_simple_unwind_form(func, Op::SaveExcursion, tail, for_value);
                true
            }
            "save-restriction" => {
                self.compile_simple_unwind_form(func, Op::SaveRestriction, tail, for_value);
                true
            }
            "save-current-buffer" => {
                self.compile_simple_unwind_form(func, Op::SaveCurrentBuffer, tail, for_value);
                true
            }
            "with-current-buffer" => {
                self.compile_with_current_buffer(func, tail, for_value);
                true
            }
            "ignore-errors" => {
                self.compile_ignore_errors(func, tail, for_value);
                true
            }
            _ => false,
        }
    }

    /// Try to compile a known builtin using a dedicated opcode.
    fn try_compile_builtin_op(
        &mut self,
        func: &mut ByteCodeFunction,
        name: &str,
        args: &[Expr],
    ) -> Option<()> {
        match (name, args.len()) {
            // Arithmetic (2 args)
            ("+", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Add);
                Some(())
            }
            ("-", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Sub);
                Some(())
            }
            ("*", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Mul);
                Some(())
            }
            ("/", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Div);
                Some(())
            }
            ("%", 2) | ("mod", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Rem);
                Some(())
            }
            ("1+", 1) => {
                self.compile_expr(func, &args[0], true);
                self.emit_tracked(func, Op::Add1);
                Some(())
            }
            ("1-", 1) => {
                self.compile_expr(func, &args[0], true);
                self.emit_tracked(func, Op::Sub1);
                Some(())
            }
            // Variadic + and *
            ("+", n) if n != 2 => {
                if n == 0 {
                    let idx = func.add_constant(Value::Int(0));
                    self.emit_tracked(func, Op::Constant(idx));
                } else {
                    self.compile_expr(func, &args[0], true);
                    for arg in &args[1..] {
                        self.compile_expr(func, arg, true);
                        self.emit_tracked(func, Op::Add);
                    }
                }
                Some(())
            }
            ("*", n) if n != 2 => {
                if n == 0 {
                    let idx = func.add_constant(Value::Int(1));
                    self.emit_tracked(func, Op::Constant(idx));
                } else {
                    self.compile_expr(func, &args[0], true);
                    for arg in &args[1..] {
                        self.compile_expr(func, arg, true);
                        self.emit_tracked(func, Op::Mul);
                    }
                }
                Some(())
            }
            ("-", 1) => {
                self.compile_expr(func, &args[0], true);
                self.emit_tracked(func, Op::Negate);
                Some(())
            }
            // Comparisons
            ("=", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Eqlsign);
                Some(())
            }
            (">", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Gtr);
                Some(())
            }
            ("<", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Lss);
                Some(())
            }
            ("<=", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Leq);
                Some(())
            }
            (">=", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Geq);
                Some(())
            }
            ("/=", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Eqlsign);
                self.emit_tracked(func, Op::Not);
                Some(())
            }
            ("max", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Max);
                Some(())
            }
            ("min", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Min);
                Some(())
            }
            // List ops
            ("car", 1) => {
                self.compile_expr(func, &args[0], true);
                self.emit_tracked(func, Op::Car);
                Some(())
            }
            ("cdr", 1) => {
                self.compile_expr(func, &args[0], true);
                self.emit_tracked(func, Op::Cdr);
                Some(())
            }
            ("cons", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Cons);
                Some(())
            }
            ("list", _) => {
                for arg in args {
                    self.compile_expr(func, arg, true);
                }
                self.emit_tracked(func, Op::List(args.len() as u16));
                Some(())
            }
            ("length", 1) => {
                self.compile_expr(func, &args[0], true);
                self.emit_tracked(func, Op::Length);
                Some(())
            }
            ("nth", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Nth);
                Some(())
            }
            ("nthcdr", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Nthcdr);
                Some(())
            }
            ("setcar", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Setcar);
                Some(())
            }
            ("setcdr", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Setcdr);
                Some(())
            }
            ("memq", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Memq);
                Some(())
            }
            ("assq", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Assq);
                Some(())
            }
            // Type predicates
            ("null", 1) | ("not", 1) => {
                self.compile_expr(func, &args[0], true);
                self.emit_tracked(func, Op::Not);
                Some(())
            }
            ("symbolp", 1) => {
                self.compile_expr(func, &args[0], true);
                self.emit_tracked(func, Op::Symbolp);
                Some(())
            }
            ("consp", 1) => {
                self.compile_expr(func, &args[0], true);
                self.emit_tracked(func, Op::Consp);
                Some(())
            }
            ("stringp", 1) => {
                self.compile_expr(func, &args[0], true);
                self.emit_tracked(func, Op::Stringp);
                Some(())
            }
            ("listp", 1) => {
                self.compile_expr(func, &args[0], true);
                self.emit_tracked(func, Op::Listp);
                Some(())
            }
            ("integerp", 1) => {
                self.compile_expr(func, &args[0], true);
                self.emit_tracked(func, Op::Integerp);
                Some(())
            }
            ("numberp", 1) => {
                self.compile_expr(func, &args[0], true);
                self.emit_tracked(func, Op::Numberp);
                Some(())
            }
            ("eq", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Eq);
                Some(())
            }
            ("equal", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Equal);
                Some(())
            }
            // String ops
            ("concat", _) => {
                for arg in args {
                    self.compile_expr(func, arg, true);
                }
                self.emit_tracked(func, Op::Concat(args.len() as u16));
                Some(())
            }
            ("substring", 2) | ("substring", 3) => {
                for arg in args {
                    self.compile_expr(func, arg, true);
                }
                // Use CallBuiltin for substring since it has variable args
                let name_idx = func.add_symbol("substring");
                self.emit_tracked(func, Op::CallBuiltin(name_idx, args.len() as u8));
                Some(())
            }
            ("string-equal", 2) | ("string=", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::StringEqual);
                Some(())
            }
            ("string-lessp", 2) | ("string<", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::StringLessp);
                Some(())
            }
            // Vector ops
            ("aref", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Aref);
                Some(())
            }
            ("aset", 3) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.compile_expr(func, &args[2], true);
                self.emit_tracked(func, Op::Aset);
                Some(())
            }
            // Symbol ops
            ("symbol-value", 1) => {
                self.compile_expr(func, &args[0], true);
                self.emit_tracked(func, Op::SymbolValue);
                Some(())
            }
            ("symbol-function", 1) => {
                self.compile_expr(func, &args[0], true);
                self.emit_tracked(func, Op::SymbolFunction);
                Some(())
            }
            ("set", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Set);
                Some(())
            }
            ("fset", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Fset);
                Some(())
            }
            ("get", 2) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.emit_tracked(func, Op::Get);
                Some(())
            }
            ("put", 3) => {
                self.compile_expr(func, &args[0], true);
                self.compile_expr(func, &args[1], true);
                self.compile_expr(func, &args[2], true);
                self.emit_tracked(func, Op::Put);
                Some(())
            }
            _ => None,
        }
    }

    // -- Special form compilation helpers ------------------------------------

    fn compile_progn(&mut self, func: &mut ByteCodeFunction, forms: &[Expr], for_value: bool) {
        if forms.is_empty() {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        }
        for (i, form) in forms.iter().enumerate() {
            let is_last = i == forms.len() - 1;
            let need_value = if is_last { for_value } else { false };
            self.compile_expr(func, form, need_value);
            if !is_last && !need_value {
                // Value was not pushed, nothing to pop
            }
        }
    }

    fn compile_if(&mut self, func: &mut ByteCodeFunction, tail: &[Expr], for_value: bool) {
        if tail.len() < 2 {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        }
        // Compile condition
        self.compile_expr(func, &tail[0], true);
        let jump_false = func.current_offset();
        self.emit_tracked(func, Op::GotoIfNil(0)); // placeholder

        // Then branch
        self.compile_expr(func, &tail[1], for_value);
        let jump_end = func.current_offset();
        self.emit_tracked(func, Op::Goto(0)); // placeholder

        // Else branch
        let else_target = func.current_offset();
        func.patch_jump(jump_false, else_target);
        if tail.len() > 2 {
            self.compile_progn(func, &tail[2..], for_value);
        } else if for_value {
            self.emit_tracked(func, Op::Nil);
        }

        let end_target = func.current_offset();
        func.patch_jump(jump_end, end_target);
    }

    fn compile_and(&mut self, func: &mut ByteCodeFunction, forms: &[Expr], for_value: bool) {
        if forms.is_empty() {
            if for_value {
                self.emit_tracked(func, Op::True);
            }
            return;
        }

        let mut jump_patches = Vec::new();

        for (i, form) in forms.iter().enumerate() {
            let is_last = i == forms.len() - 1;
            self.compile_expr(func, form, true);

            if !is_last {
                if for_value {
                    let jump = func.current_offset();
                    self.emit_tracked(func, Op::GotoIfNilElsePop(0));
                    jump_patches.push(jump);
                } else {
                    let jump = func.current_offset();
                    self.emit_tracked(func, Op::GotoIfNil(0));
                    jump_patches.push(jump);
                }
            }
        }

        if !for_value {
            self.emit_tracked(func, Op::Pop);
        }

        let end = func.current_offset();
        for patch in jump_patches {
            func.patch_jump(patch, end);
        }
        if !for_value {
            // The nil-short-circuit jumps also need to land here
            // but they don't push a value in the !for_value case
        }
    }

    fn compile_or(&mut self, func: &mut ByteCodeFunction, forms: &[Expr], for_value: bool) {
        if forms.is_empty() {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        }

        let mut jump_patches = Vec::new();

        for (i, form) in forms.iter().enumerate() {
            let is_last = i == forms.len() - 1;
            self.compile_expr(func, form, true);

            if !is_last {
                if for_value {
                    let jump = func.current_offset();
                    self.emit_tracked(func, Op::GotoIfNotNilElsePop(0));
                    jump_patches.push(jump);
                } else {
                    let jump = func.current_offset();
                    self.emit_tracked(func, Op::GotoIfNotNil(0));
                    jump_patches.push(jump);
                }
            }
        }

        if !for_value {
            self.emit_tracked(func, Op::Pop);
        }

        let end = func.current_offset();
        for patch in jump_patches {
            func.patch_jump(patch, end);
        }
    }

    fn compile_cond(&mut self, func: &mut ByteCodeFunction, clauses: &[Expr], for_value: bool) {
        if clauses.is_empty() {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        }

        let mut end_patches = Vec::new();

        for (i, clause) in clauses.iter().enumerate() {
            let is_last = i == clauses.len() - 1;
            let Expr::List(items) = clause else {
                continue;
            };
            if items.is_empty() {
                continue;
            }

            // Compile test
            self.compile_expr(func, &items[0], true);

            if items.len() == 1 {
                // (cond (TEST)) - return test value if true
                if is_last {
                    if !for_value {
                        self.emit_tracked(func, Op::Pop);
                    }
                } else if for_value {
                    let jump = func.current_offset();
                    self.emit_tracked(func, Op::GotoIfNotNilElsePop(0));
                    end_patches.push(jump);
                } else {
                    let jump = func.current_offset();
                    self.emit_tracked(func, Op::GotoIfNotNil(0));
                    end_patches.push(jump);
                }
            } else {
                // (cond (TEST BODY...))
                if is_last {
                    // Last clause: run body if test passes, nil if not
                    let jump_skip = func.current_offset();
                    self.emit_tracked(func, Op::GotoIfNil(0));
                    self.compile_progn(func, &items[1..], for_value);
                    let jump_end = func.current_offset();
                    self.emit_tracked(func, Op::Goto(0)); // jump past trailing nil
                    end_patches.push(jump_end);
                    let skip_target = func.current_offset();
                    func.patch_jump(jump_skip, skip_target);
                } else {
                    let jump_skip = func.current_offset();
                    self.emit_tracked(func, Op::GotoIfNil(0));
                    self.compile_progn(func, &items[1..], for_value);
                    let jump_end = func.current_offset();
                    self.emit_tracked(func, Op::Goto(0));
                    end_patches.push(jump_end);
                    let skip_target = func.current_offset();
                    func.patch_jump(jump_skip, skip_target);
                }
            }
        }

        // All remaining clauses fell through — push nil if needed
        if for_value {
            self.emit_tracked(func, Op::Nil);
        }

        let end = func.current_offset();
        for patch in end_patches {
            func.patch_jump(patch, end);
        }
    }

    fn compile_while(&mut self, func: &mut ByteCodeFunction, tail: &[Expr]) {
        if tail.is_empty() {
            return;
        }
        let loop_start = func.current_offset();
        self.compile_expr(func, &tail[0], true);
        let exit_jump = func.current_offset();
        self.emit_tracked(func, Op::GotoIfNil(0));

        for form in &tail[1..] {
            self.compile_expr(func, form, false);
        }
        self.emit_tracked(func, Op::Goto(loop_start));

        let exit_target = func.current_offset();
        func.patch_jump(exit_jump, exit_target);
    }

    fn compile_let(&mut self, func: &mut ByteCodeFunction, tail: &[Expr], for_value: bool) {
        if tail.is_empty() {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        }

        let mut bind_count = 0u16;
        let mut shadowed_names: Vec<String> = Vec::new();

        match &tail[0] {
            Expr::List(entries) => {
                // Evaluate all init values first (parallel let)
                let mut names: Vec<&str> = Vec::new();
                for binding in entries {
                    match binding {
                        Expr::Symbol(id) => {
                            self.emit_tracked(func, Op::Nil);
                            names.push(resolve_sym(*id));
                        }
                        Expr::List(pair) if !pair.is_empty() => {
                            let Expr::Symbol(id) = &pair[0] else {
                                continue;
                            };
                            if pair.len() > 1 {
                                self.compile_expr(func, &pair[1], true);
                            } else {
                                self.emit_tracked(func, Op::Nil);
                            }
                            names.push(resolve_sym(*id));
                        }
                        _ => {}
                    }
                }
                // Now bind them all
                for name in names.iter().rev() {
                    let idx = func.add_symbol(name);
                    self.emit_tracked(func, Op::VarBind(idx));
                    bind_count += 1;
                    // Shadow stack-local params so body uses VarRef
                    if self
                        .lex_scope
                        .as_ref()
                        .is_some_and(|s| s.find(name).is_some())
                    {
                        if let Some(ref mut scope) = self.lex_scope {
                            scope.shadowed.insert(name.to_string());
                        }
                        shadowed_names.push(name.to_string());
                    }
                }
            }
            Expr::Symbol(id) if resolve_sym(*id) == "nil" => {} // (let nil ...)
            _ => {}
        }

        self.compile_progn(func, &tail[1..], for_value);

        if bind_count > 0 {
            self.emit_tracked(func, Op::Unbind(bind_count));
        }

        // Unshadow names
        for name in &shadowed_names {
            if let Some(ref mut scope) = self.lex_scope {
                scope.shadowed.remove(name);
            }
        }
    }

    fn compile_let_star(&mut self, func: &mut ByteCodeFunction, tail: &[Expr], for_value: bool) {
        if tail.is_empty() {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        }

        let mut bind_count = 0u16;
        let mut shadowed_names: Vec<String> = Vec::new();

        match &tail[0] {
            Expr::List(entries) => {
                for binding in entries {
                    match binding {
                        Expr::Symbol(id) => {
                            let name = resolve_sym(*id);
                            self.emit_tracked(func, Op::Nil);
                            let idx = func.add_symbol(name);
                            self.emit_tracked(func, Op::VarBind(idx));
                            bind_count += 1;
                            // Shadow stack-local params
                            if self
                                .lex_scope
                                .as_ref()
                                .is_some_and(|s| s.find(name).is_some())
                            {
                                if let Some(ref mut scope) = self.lex_scope {
                                    scope.shadowed.insert(name.to_string());
                                }
                                shadowed_names.push(name.to_string());
                            }
                        }
                        Expr::List(pair) if !pair.is_empty() => {
                            let Expr::Symbol(id) = &pair[0] else {
                                continue;
                            };
                            let name = resolve_sym(*id);
                            if pair.len() > 1 {
                                self.compile_expr(func, &pair[1], true);
                            } else {
                                self.emit_tracked(func, Op::Nil);
                            }
                            let idx = func.add_symbol(name);
                            self.emit_tracked(func, Op::VarBind(idx));
                            bind_count += 1;
                            // Shadow stack-local params
                            if self
                                .lex_scope
                                .as_ref()
                                .is_some_and(|s| s.find(name).is_some())
                            {
                                if let Some(ref mut scope) = self.lex_scope {
                                    scope.shadowed.insert(name.to_string());
                                }
                                shadowed_names.push(name.to_string());
                            }
                        }
                        _ => {}
                    }
                }
            }
            Expr::Symbol(id) if resolve_sym(*id) == "nil" => {}
            _ => {}
        }

        self.compile_progn(func, &tail[1..], for_value);

        if bind_count > 0 {
            self.emit_tracked(func, Op::Unbind(bind_count));
        }

        // Unshadow names
        for name in &shadowed_names {
            if let Some(ref mut scope) = self.lex_scope {
                scope.shadowed.remove(name);
            }
        }
    }

    fn compile_setq(&mut self, func: &mut ByteCodeFunction, tail: &[Expr], for_value: bool) {
        if tail.is_empty() {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        }

        let mut i = 0;
        while i + 1 < tail.len() {
            let id = match &tail[i] {
                Expr::Symbol(id) | Expr::Keyword(id) => *id,
                _ => {
                    i += 2;
                    continue;
                }
            };
            let name = resolve_sym(id);
            self.compile_expr(func, &tail[i + 1], true);
            let is_last_pair = i + 2 >= tail.len();
            if for_value && is_last_pair {
                self.emit_tracked(func, Op::Dup);
            }
            // Check if target is a stack-local parameter
            if let Some(slot) = self.lex_scope.as_ref().and_then(|s| s.find(name)) {
                // StackSet(n): pops TOS, stores at stack[len-n] (after pop)
                let n = self.stack_depth as usize - 1 - slot;
                self.emit_tracked(func, Op::StackSet(n as u16));
            } else {
                let idx = func.add_symbol(name);
                self.emit_tracked(func, Op::VarSet(idx));
            }
            i += 2;
        }
    }

    fn compile_defun(&mut self, func: &mut ByteCodeFunction, tail: &[Expr], for_value: bool) {
        if tail.len() < 3 {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        }
        let Expr::Symbol(id) = &tail[0] else {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        };
        let name = resolve_sym(*id);

        // Compile the lambda body (skip docstring)
        let body_start = if tail.len() > 3 {
            if let Expr::Str(_) = &tail[2] { 3 } else { 2 }
        } else {
            2
        };

        // Parse params
        let params = parse_params(&tail[1]);

        // Compile nested lambda as bytecode
        let inner = self.compile_lambda(&params, &tail[body_start..]);

        // Store the compiled function as a constant
        let bytecode_val = Value::make_bytecode(inner);
        let func_idx = func.add_constant(bytecode_val);
        let name_idx = func.add_symbol(name);

        // (fset 'name <compiled-function>)
        self.emit_tracked(func, Op::Constant(name_idx));
        self.emit_tracked(func, Op::Constant(func_idx));
        self.emit_tracked(func, Op::Fset);

        if for_value {
            self.emit_tracked(func, Op::Constant(name_idx));
        } else {
            self.emit_tracked(func, Op::Pop); // fset returns the value, discard it
        }
    }

    fn compile_defvar(&mut self, func: &mut ByteCodeFunction, tail: &[Expr], for_value: bool) {
        if tail.is_empty() {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        }
        let Expr::Symbol(id) = &tail[0] else {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        };
        let name = resolve_sym(*id);

        self.add_special(name);

        // defvar only sets if not already bound — use CallBuiltin
        // We compile this as a runtime call to preserve the "only set if unbound" semantics
        let name_idx = func.add_symbol(name);
        if tail.len() > 1 {
            self.compile_expr(func, &tail[1], true);
        } else {
            self.emit_tracked(func, Op::Nil);
        }
        let defvar_name = func.add_symbol("%%defvar");
        self.emit_tracked(func, Op::Constant(name_idx)); // symbol name
        // Stack: [init-value, symbol-name]
        // Swap order for defvar builtin: needs (name value)
        self.emit_tracked(func, Op::CallBuiltin(defvar_name, 2));
        if !for_value {
            self.emit_tracked(func, Op::Pop);
        }
    }

    fn compile_defconst(&mut self, func: &mut ByteCodeFunction, tail: &[Expr], for_value: bool) {
        if tail.len() < 2 {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        }
        let Expr::Symbol(id) = &tail[0] else {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        };
        let name = resolve_sym(*id);

        self.add_special(name);

        let name_idx = func.add_symbol(name);
        self.compile_expr(func, &tail[1], true);
        let defconst_name = func.add_symbol("%%defconst");
        self.emit_tracked(func, Op::Constant(name_idx));
        self.emit_tracked(func, Op::CallBuiltin(defconst_name, 2));
        if !for_value {
            self.emit_tracked(func, Op::Pop);
        }
    }

    fn compile_lambda_or_function(
        &mut self,
        func: &mut ByteCodeFunction,
        name: &str,
        tail: &[Expr],
    ) {
        if name == "function" {
            // #'symbol or #'(lambda ...)
            if let Some(Expr::Symbol(id)) = tail.first() {
                // #'symbol — push function reference
                let idx = func.add_symbol(resolve_sym(*id));
                self.emit_tracked(func, Op::Constant(idx));
                return;
            }
            if let Some(Expr::List(items)) = tail.first() {
                if let Some(Expr::Symbol(id)) = items.first() {
                    if resolve_sym(*id) == "lambda" {
                        // #'(lambda ...)
                        self.compile_raw_lambda(func, &items[1..]);
                        return;
                    }
                }
            }
            self.emit_tracked(func, Op::Nil);
        } else {
            // bare `lambda`
            self.compile_raw_lambda(func, tail);
        }
    }

    fn compile_raw_lambda(&mut self, func: &mut ByteCodeFunction, tail: &[Expr]) {
        if tail.is_empty() {
            self.emit_tracked(func, Op::Nil);
            return;
        }

        let params = parse_params(&tail[0]);
        let body_start = if tail.len() > 2 {
            if let Expr::Str(_) = &tail[1] { 2 } else { 1 }
        } else {
            1
        };

        let inner = self.compile_lambda(&params, &tail[body_start..]);
        let bytecode_val = Value::make_bytecode(inner);

        if self.lexical {
            let idx = func.add_constant(bytecode_val);
            self.emit_tracked(func, Op::MakeClosure(idx));
        } else {
            let idx = func.add_constant(bytecode_val);
            self.emit_tracked(func, Op::Constant(idx));
        }
    }

    fn compile_catch(&mut self, func: &mut ByteCodeFunction, tail: &[Expr], for_value: bool) {
        if tail.is_empty() {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        }
        // Compile tag
        self.compile_expr(func, &tail[0], true);

        // Push handler
        let handler_jump = func.current_offset();
        self.emit_tracked(func, Op::PushCatch(0)); // placeholder

        // Compile body
        self.compile_progn(func, &tail[1..], true);
        self.emit_tracked(func, Op::PopHandler);
        let end_jump = func.current_offset();
        self.emit_tracked(func, Op::Goto(0)); // placeholder

        // Handler target: error value is on stack
        let handler_target = func.current_offset();
        func.patch_jump(handler_jump, handler_target);

        // The catch handler will have the caught value on stack
        let end_target = func.current_offset();
        func.patch_jump(end_jump, end_target);

        if !for_value {
            self.emit_tracked(func, Op::Pop);
        }
    }

    fn compile_unwind_protect(
        &mut self,
        func: &mut ByteCodeFunction,
        tail: &[Expr],
        for_value: bool,
    ) {
        if tail.is_empty() {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        }

        let cleanup_jump = func.current_offset();
        self.emit_tracked(func, Op::UnwindProtect(0)); // placeholder

        // Protected form
        self.compile_expr(func, &tail[0], for_value);

        // Pop the unwind-protect handler
        self.emit_tracked(func, Op::PopHandler);

        // Run cleanup forms (result discarded)
        for form in &tail[1..] {
            self.compile_expr(func, form, false);
        }

        let skip_cleanup = func.current_offset();
        self.emit_tracked(func, Op::Goto(0)); // skip cleanup re-execution on normal path

        // Cleanup target (entered on non-local exit)
        let cleanup_target = func.current_offset();
        func.patch_jump(cleanup_jump, cleanup_target);
        for form in &tail[1..] {
            self.compile_expr(func, form, false);
        }
        // Re-throw or continue

        let end = func.current_offset();
        func.patch_jump(skip_cleanup, end);
    }

    fn compile_simple_unwind_form(
        &mut self,
        func: &mut ByteCodeFunction,
        setup: Op,
        tail: &[Expr],
        for_value: bool,
    ) {
        self.emit_tracked(func, setup);
        self.compile_progn(func, tail, for_value);
        self.emit_tracked(func, Op::Unbind(1));
    }

    fn compile_with_current_buffer(
        &mut self,
        func: &mut ByteCodeFunction,
        tail: &[Expr],
        for_value: bool,
    ) {
        if tail.is_empty() {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        }

        self.emit_tracked(func, Op::SaveCurrentBuffer);
        self.compile_expr(func, &tail[0], true);
        let set_buffer_name = func.add_symbol("set-buffer");
        self.emit_tracked(func, Op::CallBuiltin(set_buffer_name, 1));

        if tail.len() == 1 {
            if !for_value {
                self.emit_tracked(func, Op::Pop);
            }
        } else {
            self.emit_tracked(func, Op::Pop);
            self.compile_progn(func, &tail[1..], for_value);
        }

        self.emit_tracked(func, Op::Unbind(1));
    }

    fn compile_condition_case(
        &mut self,
        func: &mut ByteCodeFunction,
        tail: &[Expr],
        for_value: bool,
    ) {
        if tail.len() < 3 {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        }

        let mut handler_jump = func.current_offset();
        let mut pushed_raw_handler = false;
        if let Some(Expr::List(handler_items)) = tail.get(2) {
            if let Some(pattern) = handler_items.first() {
                let pattern_idx = func.add_constant(literal_to_value(pattern));
                self.emit_tracked(func, Op::Constant(pattern_idx));
                handler_jump = func.current_offset();
                self.emit_tracked(func, Op::PushConditionCaseRaw(0)); // placeholder
                pushed_raw_handler = true;
            }
        }
        if !pushed_raw_handler {
            handler_jump = func.current_offset();
            self.emit_tracked(func, Op::PushConditionCase(0)); // placeholder
        }

        // Body
        self.compile_expr(func, &tail[1], true);
        self.emit_tracked(func, Op::PopHandler);
        let end_jump = func.current_offset();
        self.emit_tracked(func, Op::Goto(0)); // jump past handlers

        // Handlers
        let handler_target = func.current_offset();
        func.patch_jump(handler_jump, handler_target);

        // Error value is on stack. Bind to variable if needed.
        let _var = match &tail[0] {
            Expr::Symbol(id) if resolve_sym(*id) != "nil" => Some(resolve_sym(*id).to_owned()),
            _ => None,
        };

        // For simplicity, bind error value and compile handler bodies
        // In practice, condition-case selects handler by error type, but
        // for the VM we do this at runtime via the VM's handler mechanism.
        // Just compile the first handler's body.
        if let Some(Expr::List(handler_items)) = tail.get(2) {
            if handler_items.len() > 1 {
                if let Some(ref var_name) = _var {
                    let var_idx = func.add_symbol(var_name);
                    self.emit_tracked(func, Op::VarBind(var_idx));
                    // Shadow stack-local param if applicable
                    let need_unshadow = self
                        .lex_scope
                        .as_ref()
                        .is_some_and(|s| s.find(var_name).is_some());
                    if need_unshadow {
                        if let Some(ref mut scope) = self.lex_scope {
                            scope.shadowed.insert(var_name.clone());
                        }
                    }
                    self.compile_progn(func, &handler_items[1..], for_value);
                    self.emit_tracked(func, Op::Unbind(1));
                    if need_unshadow {
                        if let Some(ref mut scope) = self.lex_scope {
                            scope.shadowed.remove(var_name);
                        }
                    }
                } else {
                    self.emit_tracked(func, Op::Pop); // discard error value
                    self.compile_progn(func, &handler_items[1..], for_value);
                }
            } else {
                self.emit_tracked(func, Op::Pop);
                if for_value {
                    self.emit_tracked(func, Op::Nil);
                }
            }
        } else {
            self.emit_tracked(func, Op::Pop);
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
        }

        let end_target = func.current_offset();
        func.patch_jump(end_jump, end_target);
    }

    fn compile_ignore_errors(
        &mut self,
        func: &mut ByteCodeFunction,
        tail: &[Expr],
        for_value: bool,
    ) {
        // Push handler
        let handler_jump = func.current_offset();
        self.emit_tracked(func, Op::PushConditionCase(0));

        // Body
        self.compile_progn(func, tail, for_value);
        self.emit_tracked(func, Op::PopHandler);
        let end_jump = func.current_offset();
        self.emit_tracked(func, Op::Goto(0));

        // Error handler: push nil
        let handler_target = func.current_offset();
        func.patch_jump(handler_jump, handler_target);
        self.emit_tracked(func, Op::Pop); // discard error
        if for_value {
            self.emit_tracked(func, Op::Nil);
        }

        let end = func.current_offset();
        func.patch_jump(end_jump, end);
    }

    fn compile_dotimes(&mut self, func: &mut ByteCodeFunction, tail: &[Expr], for_value: bool) {
        if tail.is_empty() {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        }
        let Expr::List(spec) = &tail[0] else {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        };
        if spec.len() < 2 {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        }
        let Expr::Symbol(var_id) = &spec[0] else {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        };
        let var_id = *var_id;

        // Lower to: (let ((VAR 0)) (while (< VAR COUNT) BODY (setq VAR (1+ VAR))))
        let while_cond = Expr::List(vec![
            Expr::Symbol(intern("<")),
            Expr::Symbol(var_id),
            spec[1].clone(),
        ]);
        let incr = Expr::List(vec![
            Expr::Symbol(intern("setq")),
            Expr::Symbol(var_id),
            Expr::List(vec![Expr::Symbol(intern("1+")), Expr::Symbol(var_id)]),
        ]);

        // Bind var = 0
        let init_val = Expr::Int(0);
        let binding = Expr::List(vec![Expr::Symbol(var_id), init_val]);

        let mut body_forms: Vec<Expr> = tail[1..].to_vec();
        body_forms.push(incr);

        let mut while_forms = vec![while_cond];
        while_forms.extend(body_forms);

        let let_form = Expr::List({
            let mut items = vec![Expr::Symbol(intern("let"))];
            items.push(Expr::List(vec![binding]));
            items.push(Expr::List({
                let mut w = vec![Expr::Symbol(intern("while"))];
                w.extend(while_forms);
                w
            }));
            // Result
            if spec.len() > 2 {
                items.push(spec[2].clone());
            }
            items
        });

        self.compile_expr(func, &let_form, for_value);
    }

    fn compile_dolist(&mut self, func: &mut ByteCodeFunction, tail: &[Expr], for_value: bool) {
        if tail.is_empty() {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        }
        let Expr::List(spec) = &tail[0] else {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        };
        if spec.len() < 2 {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        }
        let Expr::Symbol(var_id) = &spec[0] else {
            if for_value {
                self.emit_tracked(func, Op::Nil);
            }
            return;
        };
        let var_id = *var_id;

        // Synthesize as:
        // (let ((__dolist_tail__ LIST) (VAR nil))
        //   (while __dolist_tail__
        //     (setq VAR (car __dolist_tail__))
        //     BODY...
        //     (setq __dolist_tail__ (cdr __dolist_tail__)))
        //   RESULT)
        let tail_var_id = intern("__dolist_tail__");

        let binding_tail = Expr::List(vec![Expr::Symbol(tail_var_id), spec[1].clone()]);
        let binding_var = Expr::List(vec![Expr::Symbol(var_id), Expr::Symbol(intern("nil"))]);

        let setq_var = Expr::List(vec![
            Expr::Symbol(intern("setq")),
            Expr::Symbol(var_id),
            Expr::List(vec![Expr::Symbol(intern("car")), Expr::Symbol(tail_var_id)]),
        ]);

        let advance_tail = Expr::List(vec![
            Expr::Symbol(intern("setq")),
            Expr::Symbol(tail_var_id),
            Expr::List(vec![Expr::Symbol(intern("cdr")), Expr::Symbol(tail_var_id)]),
        ]);

        let mut while_body = vec![setq_var];
        while_body.extend_from_slice(&tail[1..]);
        while_body.push(advance_tail);

        let while_form = Expr::List({
            let mut w = vec![Expr::Symbol(intern("while"))];
            w.push(Expr::Symbol(tail_var_id));
            w.extend(while_body);
            w
        });

        let let_form = Expr::List({
            let mut items = vec![Expr::Symbol(intern("let*"))];
            items.push(Expr::List(vec![binding_tail, binding_var]));
            items.push(while_form);
            if spec.len() > 2 {
                items.push(spec[2].clone());
            }
            items
        });

        self.compile_expr(func, &let_form, for_value);
    }

    /// Compute max stack depth by walking the instruction stream.
    fn compute_max_stack(&self, func: &mut ByteCodeFunction, num_param_slots: usize) {
        let mut depth: i32 = num_param_slots as i32;
        let mut max: i32 = depth;

        for op in &func.ops {
            let delta = stack_delta(op);
            depth += delta;
            if depth > max {
                max = depth;
            }
            // Don't let depth go negative (indicates a bug, but be safe)
            if depth < 0 {
                depth = 0;
            }
        }

        func.max_stack = max.max(1) as u16;
    }
}

/// Parse an Expr into LambdaParams.
fn parse_params(expr: &Expr) -> LambdaParams {
    match expr {
        Expr::Symbol(id) if resolve_sym(*id) == "nil" => LambdaParams::simple(vec![]),
        Expr::List(items) => {
            let mut required = Vec::new();
            let mut optional = Vec::new();
            let mut rest = None;
            let mut mode = 0;

            for item in items {
                let Expr::Symbol(id) = item else { continue };
                let name = resolve_sym(*id);
                match name {
                    "&optional" => {
                        mode = 1;
                        continue;
                    }
                    "&rest" => {
                        mode = 2;
                        continue;
                    }
                    _ => {}
                }
                match mode {
                    0 => required.push(*id),
                    1 => optional.push(*id),
                    2 => {
                        rest = Some(*id);
                        break;
                    }
                    _ => {}
                }
            }
            LambdaParams {
                required,
                optional,
                rest,
            }
        }
        _ => LambdaParams::simple(vec![]),
    }
}

/// Check if an expression is a literal (constant) that doesn't need evaluation.
fn is_literal(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Int(_)
            | Expr::Float(_)
            | Expr::Str(_)
            | Expr::Char(_)
            | Expr::Keyword(_)
            | Expr::OpaqueValue(_)
            | Expr::Bool(_)
    ) || matches!(expr, Expr::Symbol(id) if resolve_sym(*id) == "nil" || resolve_sym(*id) == "t")
}

/// Convert a literal expression to a Value.
fn literal_to_value(expr: &Expr) -> Value {
    match expr {
        Expr::Int(n) => Value::Int(*n),
        Expr::Float(f) => Value::Float(*f, next_float_id()),
        Expr::ReaderLoadFileName => Value::symbol("load-file-name"),
        Expr::Str(s) => Value::string(s.clone()),
        Expr::Char(c) => Value::Char(*c),
        Expr::Keyword(id) => Value::Keyword(*id),
        Expr::Bool(true) => Value::True,
        Expr::Bool(false) => Value::Nil,
        Expr::Symbol(id) if resolve_sym(*id) == "nil" => Value::Nil,
        Expr::Symbol(id) if resolve_sym(*id) == "t" => Value::True,
        Expr::Symbol(id) => Value::Symbol(*id),
        Expr::List(items) if items.is_empty() => Value::Nil,
        Expr::List(items) => {
            // For quoted list, recursively convert
            if items.len() == 2 {
                if let Expr::Symbol(id) = &items[0] {
                    if resolve_sym(*id) == "quote" {
                        return literal_to_value(&items[1]);
                    }
                }
            }
            let vals: Vec<Value> = items.iter().map(literal_to_value).collect();
            Value::list(vals)
        }
        Expr::Vector(items) => {
            let vals: Vec<Value> = items.iter().map(literal_to_value).collect();
            Value::vector(vals)
        }
        Expr::DottedList(items, last) => {
            let head_vals: Vec<Value> = items.iter().map(literal_to_value).collect();
            let tail_val = literal_to_value(last);
            head_vals
                .into_iter()
                .rev()
                .fold(tail_val, |acc, item| Value::cons(item, acc))
        }
        Expr::OpaqueValue(v) => *v,
    }
}

/// Stack depth change for an operation.
fn stack_delta(op: &Op) -> i32 {
    match op {
        Op::Constant(_) | Op::Nil | Op::True | Op::Dup | Op::StackRef(_) => 1,
        Op::StackSet(_) => -1,
        Op::DiscardN(n) => -((*n & 0x7F) as i32),
        Op::Pop => -1,
        Op::VarRef(_) => 1,
        Op::VarSet(_) => -1,
        Op::VarBind(_) => -1,
        Op::Unbind(_) => 0,
        Op::Call(n) => -(*n as i32), // pops func + n args, pushes result
        Op::Apply(n) => -(*n as i32),
        Op::Goto(_) => 0,
        Op::GotoIfNil(_) | Op::GotoIfNotNil(_) => -1,
        Op::GotoIfNilElsePop(_) | Op::GotoIfNotNilElsePop(_) => 0, // conditional pop
        Op::Switch => -2,
        Op::Return => -1,
        // Binary ops: pop 2, push 1
        Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Rem => -1,
        Op::Add1 | Op::Sub1 | Op::Negate => 0, // pop 1, push 1
        Op::Eqlsign | Op::Gtr | Op::Lss | Op::Leq | Op::Geq | Op::Max | Op::Min => -1,
        Op::Car | Op::Cdr | Op::CarSafe | Op::CdrSafe => 0,
        Op::Cons => -1,
        Op::List(n) => -(*n as i32) + 1,
        Op::Length => 0,
        Op::Nth | Op::Nthcdr | Op::Elt | Op::Member | Op::Memq | Op::Assq | Op::Nconc => -1,
        Op::Nreverse => 0,
        Op::Setcar | Op::Setcdr => -1,
        Op::Symbolp
        | Op::Consp
        | Op::Stringp
        | Op::Listp
        | Op::Integerp
        | Op::Numberp
        | Op::Null
        | Op::Not => 0,
        Op::Eq | Op::Equal => -1,
        Op::Concat(n) => -(*n as i32) + 1,
        Op::Substring | Op::StringEqual | Op::StringLessp => -1,
        Op::Aref => -1,
        Op::Aset => -2,
        Op::SymbolValue | Op::SymbolFunction => 0,
        Op::Set | Op::Fset | Op::Get => -1,
        Op::Put => -2,
        Op::PushConditionCase(_) => 0,
        Op::PushConditionCaseRaw(_) => -1,
        Op::PushCatch(_) => -1,
        Op::PopHandler => 0,
        Op::UnwindProtect(_) => 0,
        Op::UnwindProtectPop => -1, // pops cleanup fn from TOS
        Op::Throw => -1,
        Op::SaveCurrentBuffer | Op::SaveExcursion | Op::SaveRestriction => 0,
        Op::MakeClosure(_) => 1,
        Op::CallBuiltin(_, n) => -(*n as i32) + 1,
    }
}
#[cfg(test)]
#[path = "compiler_test.rs"]
mod tests;
