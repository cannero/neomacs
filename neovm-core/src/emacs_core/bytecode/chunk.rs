//! ByteCode chunk — compiled function representation.

use super::opcode::Op;
use crate::emacs_core::value::{LambdaParams, Value};

/// A compiled bytecode function.
#[derive(Clone, Debug)]
pub struct ByteCodeFunction {
    /// The bytecode instructions.
    pub ops: Vec<Op>,
    /// Constant pool: values referenced by Constant/VarRef/VarSet/etc.
    pub constants: Vec<Value>,
    /// Maximum stack depth needed (for pre-allocation).
    pub max_stack: u16,
    /// Parameter specification.
    pub params: LambdaParams,
    /// For closures: captured lexical environment as a cons alist.
    pub env: Option<Value>,
    /// Optional docstring.
    pub docstring: Option<String>,
    /// Optional documentation form (e.g., oclosure type symbol in slot 4).
    pub doc_form: Option<Value>,
}

impl ByteCodeFunction {
    pub fn new(params: LambdaParams) -> Self {
        Self {
            ops: Vec::new(),
            constants: Vec::new(),
            max_stack: 0,
            params,
            env: None,
            docstring: None,
            doc_form: None,
        }
    }

    /// Add a constant to the pool and return its index.
    /// Deduplicates by value equality for symbols and integers.
    pub fn add_constant(&mut self, value: Value) -> u16 {
        // Check for existing constant (simple dedup for common types)
        for (i, existing) in self.constants.iter().enumerate() {
            match (&value, existing) {
                (Value::Int(a), Value::Int(b)) if a == b => return i as u16,
                (Value::Symbol(a), Value::Symbol(b)) if a == b => return i as u16,
                (Value::Symbol(a), Value::Keyword(b)) if a == b => return i as u16,
                (Value::Keyword(a), Value::Symbol(b)) if a == b => return i as u16,
                (Value::Nil, Value::Nil) => return i as u16,
                (Value::True, Value::True) => return i as u16,
                (Value::Keyword(a), Value::Keyword(b)) if a == b => return i as u16,
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
            | Op::UnwindProtect(addr) => {
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
