//! ByteCode chunk — compiled function representation.

use std::collections::HashMap;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use super::opcode::Op;
use crate::emacs_core::value::{LambdaParams, Value, ValueKind};
use crate::heap_types::LispString;

fn arglist_value_from_params(params: &LambdaParams) -> Value {
    let mut elements = Vec::new();
    for sym in &params.required {
        elements.push(Value::from_sym_id(*sym));
    }
    if !params.optional.is_empty() {
        elements.push(Value::symbol("&optional"));
        for sym in &params.optional {
            elements.push(Value::from_sym_id(*sym));
        }
    }
    if let Some(rest) = params.rest {
        elements.push(Value::symbol("&rest"));
        elements.push(Value::from_sym_id(rest));
    }
    Value::list(elements)
}

/// A compiled bytecode function.
#[derive(Debug)]
pub struct ByteCodeFunction {
    /// The bytecode instructions.
    pub ops: Vec<Op>,
    /// Constant pool: values referenced by Constant/VarRef/VarSet/etc.
    pub constants: Vec<Value>,
    /// Maximum stack depth needed (for pre-allocation).
    pub max_stack: u16,
    /// Parameter specification.
    pub params: LambdaParams,
    /// Original GNU byte-code slot 0 value.
    ///
    /// Lexical byte-code uses an integer arg descriptor here, while old-style
    /// dynamic byte-code uses an arglist.  Bytecomp's inliner distinguishes
    /// those cases through `(aref fn 0)`, so this must round-trip exactly.
    pub arglist: Value,
    /// Whether the function was compiled with lexical binding enabled.
    pub lexical: bool,
    /// For closures: captured lexical environment as a cons alist.
    pub env: Option<Value>,
    /// GNU `.elc` bytecode stores branch targets as byte offsets.
    /// Decoded runtime uses instruction indices, so GNU-decoded functions
    /// retain the byte-offset -> instruction-index map for `switch`.
    pub gnu_byte_offset_map: Option<HashMap<usize, usize>>,
    /// Original GNU-format bytecode bytes from the .elc file or `make-byte-code`
    /// call.  NeoVM normally executes from `ops` (decoded IR), but elisp code
    /// like `byte-compile-make-closure` does `(aref FUN 1)` to read the raw
    /// bytecode string and pass it to `make-byte-code` for closure prototype
    /// generation.  Without preserving the original bytes, those round-trips
    /// produce empty bytecode functions.
    pub gnu_bytecode_bytes: Option<Vec<u8>>,
    /// Optional docstring.
    pub docstring: Option<LispString>,
    /// Optional documentation form (e.g., oclosure type symbol in slot 4).
    pub doc_form: Option<Value>,
    /// Interactive spec from GNU closure slot 5 (CLOSURE_INTERACTIVE).
    /// Can be a string code, a form to evaluate, or a vector [spec, modes].
    pub interactive: Option<Value>,
    /// GNU closure pseudovector size for observable sequence operations.
    ///
    /// GNU `make-byte-code` allocates a vector with exactly the number of
    /// arguments supplied, then marks it as `PVEC_CLOSURE`.  Explicit nil slots
    /// therefore still count for `length`, `aref`, `append`, printing, etc.
    pub closure_slot_count: usize,
    /// GNU accepts `&rest ELEMENTS` after the interactive slot.  They have no
    /// execution significance, but remain observable through closure slots.
    pub extra_slots: Vec<Value>,
}

#[cfg(test)]
static BYTECODE_FUNCTION_CLONE_COUNT: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
pub(crate) fn reset_bytecode_function_clone_count_for_test() {
    BYTECODE_FUNCTION_CLONE_COUNT.store(0, Ordering::Relaxed);
}

#[cfg(test)]
pub(crate) fn bytecode_function_clone_count_for_test() -> usize {
    BYTECODE_FUNCTION_CLONE_COUNT.load(Ordering::Relaxed)
}

impl Clone for ByteCodeFunction {
    fn clone(&self) -> Self {
        #[cfg(test)]
        BYTECODE_FUNCTION_CLONE_COUNT.fetch_add(1, Ordering::Relaxed);

        Self {
            ops: self.ops.clone(),
            constants: self.constants.clone(),
            max_stack: self.max_stack,
            params: self.params.clone(),
            arglist: self.arglist,
            lexical: self.lexical,
            env: self.env,
            gnu_byte_offset_map: self.gnu_byte_offset_map.clone(),
            gnu_bytecode_bytes: self.gnu_bytecode_bytes.clone(),
            docstring: self.docstring.clone(),
            doc_form: self.doc_form,
            interactive: self.interactive,
            closure_slot_count: self.closure_slot_count,
            extra_slots: self.extra_slots.clone(),
        }
    }
}

impl ByteCodeFunction {
    pub fn new(params: LambdaParams) -> Self {
        let arglist = arglist_value_from_params(&params);
        Self {
            ops: Vec::new(),
            constants: Vec::new(),
            max_stack: 0,
            params,
            arglist,
            lexical: false,
            env: None,
            gnu_byte_offset_map: None,
            gnu_bytecode_bytes: None,
            docstring: None,
            doc_form: None,
            interactive: None,
            closure_slot_count: 4,
            extra_slots: Vec::new(),
        }
    }

    pub fn observable_closure_slot_count(&self) -> usize {
        let mut count = self.closure_slot_count.max(4);
        if self.docstring.is_some() || self.doc_form.is_some() {
            count = count.max(5);
        }
        if self.interactive.is_some() {
            count = count.max(6);
        }
        if !self.extra_slots.is_empty() {
            count = count.max(6 + self.extra_slots.len());
        }
        count
    }

    /// Add a constant to the pool and return its index.
    /// Deduplicates by value equality for symbols and integers.
    pub fn add_constant(&mut self, value: Value) -> u16 {
        // Check for existing constant (simple dedup for common types)
        for (i, existing) in self.constants.iter().enumerate() {
            match (value.kind(), existing.kind()) {
                (ValueKind::Fixnum(a), ValueKind::Fixnum(b)) if a == b => return i as u16,
                (ValueKind::Symbol(a), ValueKind::Symbol(b)) if a == b => return i as u16,
                (ValueKind::Symbol(a), ValueKind::Symbol(b)) if a == b => return i as u16,
                (ValueKind::Symbol(a), ValueKind::Symbol(b)) if a == b => return i as u16,
                (ValueKind::Nil, ValueKind::Nil) => return i as u16,
                (ValueKind::T, ValueKind::T) => return i as u16,
                (ValueKind::Symbol(a), ValueKind::Symbol(b)) if a == b => return i as u16,
                _ => {}
            }
        }
        let idx = self.constants.len() as u16;
        self.constants.push(value);
        idx
    }

    /// Add a symbol name to the constant pool and return its index.
    pub fn add_symbol(&mut self, name: &str) -> u16 {
        self.add_constant(Value::symbol(name))
    }

    /// Emit an instruction.
    pub fn emit(&mut self, op: Op) {
        self.ops.push(op);
    }

    /// Current instruction count (used for jump target calculation).
    pub fn current_offset(&self) -> u32 {
        self.ops.len() as u32
    }

    /// Patch a jump target at the given instruction index.
    pub fn patch_jump(&mut self, instr_idx: u32, target: u32) {
        let idx = instr_idx as usize;
        match &mut self.ops[idx] {
            Op::Goto(addr)
            | Op::GotoIfNil(addr)
            | Op::GotoIfNotNil(addr)
            | Op::GotoIfNilElsePop(addr)
            | Op::GotoIfNotNilElsePop(addr)
            | Op::PushConditionCase(addr)
            | Op::PushConditionCaseRaw(addr)
            | Op::PushCatch(addr) => {
                *addr = target;
            }
            _ => panic!("patch_jump on non-jump instruction at {}", idx),
        }
    }

    /// Disassemble to a human-readable string.
    pub fn disassemble(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "bytecode function ({} ops, {} constants, stack {})\n",
            self.ops.len(),
            self.constants.len(),
            self.max_stack
        ));

        out.push_str("constants:\n");
        for (i, c) in self.constants.iter().enumerate() {
            out.push_str(&format!("  {}: {}\n", i, c));
        }

        out.push_str("code:\n");
        for (i, op) in self.ops.iter().enumerate() {
            out.push_str(&format!("  {:4}: {}\n", i, op.disasm(&self.constants)));
        }
        out
    }
}
#[cfg(test)]
#[path = "chunk_test.rs"]
mod tests;
