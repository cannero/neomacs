//! GNU Emacs bytecode decoder.
//!
//! Translates GNU Emacs `.elc` bytecodes into NeoVM's `Op` instruction set.
//! GNU bytecodes are documented in `lisp/emacs-lisp/bytecomp.el` (lines 749-937).
//!
//! The decoder performs two passes:
//! 1. Decode all instructions sequentially, building a byte-offset → instruction-index map.
//! 2. Patch all jump targets from absolute byte offsets to instruction indices.

use std::collections::HashMap;
use std::fmt;

use super::opcode::Op;
use crate::emacs_core::value::{Value, ValueKind};

/// Errors that can occur during GNU bytecode decoding.
#[derive(Debug)]
pub enum DecodeError {
    /// Unknown or unimplemented opcode byte.
    UnknownOpcode(u8, usize),
    /// Premature end of bytecode stream while reading operand.
    UnexpectedEnd(usize),
    /// Jump target byte offset not found in the offset map.
    InvalidJumpTarget(usize, usize),
    /// Obsolete opcode that should not appear in modern .elc files.
    ObsoleteOpcode(u8, usize),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::UnknownOpcode(byte, off) => {
                write!(
                    f,
                    "unknown GNU opcode 0x{:02X} at byte offset {}",
                    byte, off
                )
            }
            DecodeError::UnexpectedEnd(off) => {
                write!(f, "unexpected end of bytecode at offset {}", off)
            }
            DecodeError::InvalidJumpTarget(target, off) => {
                write!(
                    f,
                    "jump target byte offset {} not found (from instruction at byte {})",
                    target, off
                )
            }
            DecodeError::ObsoleteOpcode(byte, off) => {
                write!(
                    f,
                    "obsolete GNU opcode 0x{:02X} at byte offset {}",
                    byte, off
                )
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// Convert a GNU bytecode string value to raw bytes.
///
/// GNU bytecode strings are unibyte — each char maps to one byte (0–255).
/// After NeoVM's parser processes octal escapes, each char in the Rust string
/// can be directly cast to `u8`.
pub fn string_value_to_bytes(s: &str) -> Vec<u8> {
    s.chars().map(|c| c as u8).collect()
}

/// Decode GNU Emacs bytecodes into NeoVM `Op` instructions.
///
/// `bytecodes` is the raw byte stream from a GNU bytecode string.
/// `constants` is mutably borrowed because some opcodes (buffer ops)
/// may inject new symbol entries into the constant pool.
///
/// Returns the decoded instruction sequence with jump targets resolved
/// to instruction indices.
pub fn decode_gnu_bytecode(
    bytecodes: &[u8],
    constants: &mut Vec<Value>,
) -> Result<Vec<Op>, DecodeError> {
    let (ops, _) = decode_gnu_bytecode_with_offset_map(bytecodes, constants)?;
    Ok(ops)
}

/// Decode GNU Emacs bytecodes and retain the original byte-offset map.
///
/// GNU `.elc` switch tables store target byte offsets inside hash-table
/// constants. NeoVM executes decoded bytecode by instruction index, so
/// GNU-decoded functions must preserve the original byte-offset ->
/// instruction-index map for runtime translation of `Bswitch`.
pub fn decode_gnu_bytecode_with_offset_map(
    bytecodes: &[u8],
    constants: &mut Vec<Value>,
) -> Result<(Vec<Op>, HashMap<usize, usize>), DecodeError> {
    let (raw_ops, offset_map, jump_patches) = decode_pass1(bytecodes, constants)?;
    let ops = patch_jumps(raw_ops, &offset_map, &jump_patches, bytecodes.len())?;
    Ok((ops, offset_map))
}

/// Intermediate instruction that may contain raw byte-offset jump targets.
#[derive(Clone, Debug)]
enum RawOp {
    /// A fully resolved Op (no jump target to patch).
    Resolved(Op),
    /// An Op with a jump target that needs patching from byte offset to instruction index.
    Jump(JumpKind, usize),
}

#[derive(Clone, Debug)]
enum JumpKind {
    Goto,
    GotoIfNil,
    GotoIfNotNil,
    GotoIfNilElsePop,
    GotoIfNotNilElsePop,
    PushConditionCaseRaw,
    PushCatch,
}

/// Jump patch entry: instruction index and source byte offset (for error messages).
struct JumpPatch {
    instr_idx: usize,
    source_byte: usize,
}

fn decode_pass1(
    bytecodes: &[u8],
    constants: &mut Vec<Value>,
) -> Result<(Vec<RawOp>, HashMap<usize, usize>, Vec<JumpPatch>), DecodeError> {
    let mut ops: Vec<RawOp> = Vec::new();
    let mut offset_map: HashMap<usize, usize> = HashMap::new();
    let mut jump_patches: Vec<JumpPatch> = Vec::new();
    let mut pos: usize = 0;
    let len = bytecodes.len();

    while pos < len {
        let byte_offset = pos;
        let instr_idx = ops.len();
        offset_map.insert(byte_offset, instr_idx);

        let byte = bytecodes[pos];
        pos += 1;

        match byte {
            // -- Immediate-arg groups (8 bytes each) --

            // 0-7: stack-ref
            0..=5 => ops.push(RawOp::Resolved(Op::StackRef(byte as u16))),
            6 => {
                let arg = fetch1(bytecodes, &mut pos, byte_offset)?;
                ops.push(RawOp::Resolved(Op::StackRef(arg as u16)));
            }
            7 => {
                let arg = fetch2(bytecodes, &mut pos, byte_offset)?;
                ops.push(RawOp::Resolved(Op::StackRef(arg)));
            }

            // 8-15: varref
            8..=13 => ops.push(RawOp::Resolved(Op::VarRef((byte - 8) as u16))),
            14 => {
                let arg = fetch1(bytecodes, &mut pos, byte_offset)?;
                ops.push(RawOp::Resolved(Op::VarRef(arg as u16)));
            }
            15 => {
                let arg = fetch2(bytecodes, &mut pos, byte_offset)?;
                ops.push(RawOp::Resolved(Op::VarRef(arg)));
            }

            // 16-23: varset
            16..=21 => ops.push(RawOp::Resolved(Op::VarSet((byte - 16) as u16))),
            22 => {
                let arg = fetch1(bytecodes, &mut pos, byte_offset)?;
                ops.push(RawOp::Resolved(Op::VarSet(arg as u16)));
            }
            23 => {
                let arg = fetch2(bytecodes, &mut pos, byte_offset)?;
                ops.push(RawOp::Resolved(Op::VarSet(arg)));
            }

            // 24-31: varbind
            24..=29 => ops.push(RawOp::Resolved(Op::VarBind((byte - 24) as u16))),
            30 => {
                let arg = fetch1(bytecodes, &mut pos, byte_offset)?;
                ops.push(RawOp::Resolved(Op::VarBind(arg as u16)));
            }
            31 => {
                let arg = fetch2(bytecodes, &mut pos, byte_offset)?;
                ops.push(RawOp::Resolved(Op::VarBind(arg)));
            }

            // 32-39: call
            32..=37 => ops.push(RawOp::Resolved(Op::Call((byte - 32) as u16))),
            38 => {
                let arg = fetch1(bytecodes, &mut pos, byte_offset)?;
                ops.push(RawOp::Resolved(Op::Call(arg as u16)));
            }
            39 => {
                let arg = fetch2(bytecodes, &mut pos, byte_offset)?;
                ops.push(RawOp::Resolved(Op::Call(arg)));
            }

            // 40-47: unbind
            40..=45 => ops.push(RawOp::Resolved(Op::Unbind((byte - 40) as u16))),
            46 => {
                let arg = fetch1(bytecodes, &mut pos, byte_offset)?;
                ops.push(RawOp::Resolved(Op::Unbind(arg as u16)));
            }
            47 => {
                let arg = fetch2(bytecodes, &mut pos, byte_offset)?;
                ops.push(RawOp::Resolved(Op::Unbind(arg)));
            }

            // -- Fixed opcodes --
            48 => ops.push(RawOp::Resolved(Op::PopHandler)),
            49 => {
                // pushconditioncase: FETCH2 jump target
                let target = fetch2(bytecodes, &mut pos, byte_offset)? as usize;
                jump_patches.push(JumpPatch {
                    instr_idx: ops.len(),
                    source_byte: byte_offset,
                });
                ops.push(RawOp::Jump(JumpKind::PushConditionCaseRaw, target));
            }
            50 => {
                // pushcatch: FETCH2 jump target
                let target = fetch2(bytecodes, &mut pos, byte_offset)? as usize;
                jump_patches.push(JumpPatch {
                    instr_idx: ops.len(),
                    source_byte: byte_offset,
                });
                ops.push(RawOp::Jump(JumpKind::PushCatch, target));
            }

            // 51-55: reserved/unused
            51..=55 => {
                // Treat as unknown but skip gracefully
                return Err(DecodeError::UnknownOpcode(byte, byte_offset));
            }

            56 => ops.push(RawOp::Resolved(Op::Nth)),
            57 => ops.push(RawOp::Resolved(Op::Symbolp)),
            58 => ops.push(RawOp::Resolved(Op::Consp)),
            59 => ops.push(RawOp::Resolved(Op::Stringp)),
            60 => ops.push(RawOp::Resolved(Op::Listp)),
            61 => ops.push(RawOp::Resolved(Op::Eq)),
            62 => ops.push(RawOp::Resolved(Op::Memq)),
            63 => ops.push(RawOp::Resolved(Op::Not)),
            64 => ops.push(RawOp::Resolved(Op::Car)),
            65 => ops.push(RawOp::Resolved(Op::Cdr)),
            66 => ops.push(RawOp::Resolved(Op::Cons)),
            67 => ops.push(RawOp::Resolved(Op::List(1))),
            68 => ops.push(RawOp::Resolved(Op::List(2))),
            69 => ops.push(RawOp::Resolved(Op::List(3))),
            70 => ops.push(RawOp::Resolved(Op::List(4))),
            71 => ops.push(RawOp::Resolved(Op::Length)),
            72 => ops.push(RawOp::Resolved(Op::Aref)),
            73 => ops.push(RawOp::Resolved(Op::Aset)),
            74 => ops.push(RawOp::Resolved(Op::SymbolValue)),
            75 => ops.push(RawOp::Resolved(Op::SymbolFunction)),
            76 => ops.push(RawOp::Resolved(Op::Set)),
            77 => ops.push(RawOp::Resolved(Op::Fset)),
            78 => ops.push(RawOp::Resolved(Op::Get)),
            79 => ops.push(RawOp::Resolved(Op::Substring)),
            80 => ops.push(RawOp::Resolved(Op::Concat(2))),
            81 => ops.push(RawOp::Resolved(Op::Concat(3))),
            82 => ops.push(RawOp::Resolved(Op::Concat(4))),
            83 => ops.push(RawOp::Resolved(Op::Sub1)),
            84 => ops.push(RawOp::Resolved(Op::Add1)),
            85 => ops.push(RawOp::Resolved(Op::Eqlsign)),
            86 => ops.push(RawOp::Resolved(Op::Gtr)),
            87 => ops.push(RawOp::Resolved(Op::Lss)),
            88 => ops.push(RawOp::Resolved(Op::Leq)),
            89 => ops.push(RawOp::Resolved(Op::Geq)),
            90 => ops.push(RawOp::Resolved(Op::Sub)),
            91 => ops.push(RawOp::Resolved(Op::Negate)),
            92 => ops.push(RawOp::Resolved(Op::Add)),
            93 => ops.push(RawOp::Resolved(Op::Max)),
            94 => ops.push(RawOp::Resolved(Op::Min)),
            95 => ops.push(RawOp::Resolved(Op::Mul)),

            // 96-127: buffer/point ops
            96..=127 => {
                if byte == 114 {
                    ops.push(RawOp::Resolved(Op::SaveCurrentBuffer));
                } else {
                    let (name, arg_count) = buffer_op_info(byte);
                    let name_idx = add_or_find_symbol(constants, name);
                    ops.push(RawOp::Resolved(Op::CallBuiltin(name_idx, arg_count)));
                }
            }

            // 128: constant with 1-byte index (but GNU defines this range oddly)
            128 => {
                let arg = fetch1(bytecodes, &mut pos, byte_offset)?;
                ops.push(RawOp::Resolved(Op::Constant(arg as u16)));
            }

            // 129: constant2 with 2-byte index
            129 => {
                let arg = fetch2(bytecodes, &mut pos, byte_offset)?;
                ops.push(RawOp::Resolved(Op::Constant(arg)));
            }

            // 130: goto
            130 => {
                let target = fetch2(bytecodes, &mut pos, byte_offset)? as usize;
                jump_patches.push(JumpPatch {
                    instr_idx: ops.len(),
                    source_byte: byte_offset,
                });
                ops.push(RawOp::Jump(JumpKind::Goto, target));
            }
            // 131: goto-if-nil
            131 => {
                let target = fetch2(bytecodes, &mut pos, byte_offset)? as usize;
                jump_patches.push(JumpPatch {
                    instr_idx: ops.len(),
                    source_byte: byte_offset,
                });
                ops.push(RawOp::Jump(JumpKind::GotoIfNil, target));
            }
            // 132: goto-if-not-nil
            132 => {
                let target = fetch2(bytecodes, &mut pos, byte_offset)? as usize;
                jump_patches.push(JumpPatch {
                    instr_idx: ops.len(),
                    source_byte: byte_offset,
                });
                ops.push(RawOp::Jump(JumpKind::GotoIfNotNil, target));
            }
            // 133: goto-if-nil-else-pop
            133 => {
                let target = fetch2(bytecodes, &mut pos, byte_offset)? as usize;
                jump_patches.push(JumpPatch {
                    instr_idx: ops.len(),
                    source_byte: byte_offset,
                });
                ops.push(RawOp::Jump(JumpKind::GotoIfNilElsePop, target));
            }
            // 134: goto-if-not-nil-else-pop
            134 => {
                let target = fetch2(bytecodes, &mut pos, byte_offset)? as usize;
                jump_patches.push(JumpPatch {
                    instr_idx: ops.len(),
                    source_byte: byte_offset,
                });
                ops.push(RawOp::Jump(JumpKind::GotoIfNotNilElsePop, target));
            }

            135 => ops.push(RawOp::Resolved(Op::Return)),
            136 => ops.push(RawOp::Resolved(Op::Pop)),
            137 => ops.push(RawOp::Resolved(Op::Dup)),

            138 => {
                ops.push(RawOp::Resolved(Op::SaveExcursion));
            }

            // 139: obsolete (was save-window-excursion before Emacs 24)
            139 => return Err(DecodeError::ObsoleteOpcode(byte, byte_offset)),

            140 => {
                ops.push(RawOp::Resolved(Op::SaveRestriction));
            }

            // 141: obsolete (was catch before Emacs 25)
            141 => return Err(DecodeError::ObsoleteOpcode(byte, byte_offset)),

            142 => {
                // unwind-protect: GNU pops cleanup fn from TOS (no operand)
                ops.push(RawOp::Resolved(Op::UnwindProtectPop));
            }

            // 143, 144, 145: obsolete
            143..=145 => return Err(DecodeError::ObsoleteOpcode(byte, byte_offset)),

            // 146: unused
            146 => return Err(DecodeError::UnknownOpcode(byte, byte_offset)),

            147 => {
                // set-marker
                let name_idx = add_or_find_symbol(constants, "set-marker");
                ops.push(RawOp::Resolved(Op::CallBuiltin(name_idx, 3)));
            }
            148 => {
                // match-beginning
                let name_idx = add_or_find_symbol(constants, "match-beginning");
                ops.push(RawOp::Resolved(Op::CallBuiltin(name_idx, 1)));
            }
            149 => {
                // match-end
                let name_idx = add_or_find_symbol(constants, "match-end");
                ops.push(RawOp::Resolved(Op::CallBuiltin(name_idx, 1)));
            }
            150 => {
                // upcase
                let name_idx = add_or_find_symbol(constants, "upcase");
                ops.push(RawOp::Resolved(Op::CallBuiltin(name_idx, 1)));
            }
            151 => {
                // downcase
                let name_idx = add_or_find_symbol(constants, "downcase");
                ops.push(RawOp::Resolved(Op::CallBuiltin(name_idx, 1)));
            }

            152 => ops.push(RawOp::Resolved(Op::StringEqual)),
            153 => ops.push(RawOp::Resolved(Op::StringLessp)),
            154 => ops.push(RawOp::Resolved(Op::Equal)),
            155 => ops.push(RawOp::Resolved(Op::Nthcdr)),
            156 => ops.push(RawOp::Resolved(Op::Elt)),
            157 => ops.push(RawOp::Resolved(Op::Member)),
            158 => ops.push(RawOp::Resolved(Op::Assq)),
            159 => ops.push(RawOp::Resolved(Op::Nreverse)),
            160 => ops.push(RawOp::Resolved(Op::Setcar)),
            161 => ops.push(RawOp::Resolved(Op::Setcdr)),
            162 => ops.push(RawOp::Resolved(Op::CarSafe)),
            163 => ops.push(RawOp::Resolved(Op::CdrSafe)),
            164 => ops.push(RawOp::Resolved(Op::Nconc)),
            165 => ops.push(RawOp::Resolved(Op::Div)),
            166 => ops.push(RawOp::Resolved(Op::Rem)),
            167 => ops.push(RawOp::Resolved(Op::Numberp)),
            168 => ops.push(RawOp::Resolved(Op::Integerp)),

            // 169-174: unused/reserved in modern Emacs
            169..=174 => return Err(DecodeError::UnknownOpcode(byte, byte_offset)),

            175 => {
                // listN: 1-byte count
                let count = fetch1(bytecodes, &mut pos, byte_offset)?;
                ops.push(RawOp::Resolved(Op::List(count as u16)));
            }
            176 => {
                // concatN: 1-byte count
                let count = fetch1(bytecodes, &mut pos, byte_offset)?;
                ops.push(RawOp::Resolved(Op::Concat(count as u16)));
            }
            177 => {
                // insertN: 1-byte count
                let count = fetch1(bytecodes, &mut pos, byte_offset)?;
                let name_idx = add_or_find_symbol(constants, "insert");
                ops.push(RawOp::Resolved(Op::CallBuiltin(name_idx, count)));
            }
            178 => {
                // stack-set: 1-byte
                let n = fetch1(bytecodes, &mut pos, byte_offset)?;
                ops.push(RawOp::Resolved(Op::StackSet(n as u16)));
            }
            179 => {
                // stack-set2: 2-byte
                let n = fetch2(bytecodes, &mut pos, byte_offset)?;
                ops.push(RawOp::Resolved(Op::StackSet(n)));
            }

            // 180-181: unused/reserved
            180..=181 => return Err(DecodeError::UnknownOpcode(byte, byte_offset)),

            182 => {
                // discardN: 1-byte (high bit = preserve TOS)
                let n = fetch1(bytecodes, &mut pos, byte_offset)?;
                ops.push(RawOp::Resolved(Op::DiscardN(n)));
            }

            183 => ops.push(RawOp::Resolved(Op::Switch)),

            // 184-191: unused/reserved
            184..=191 => return Err(DecodeError::UnknownOpcode(byte, byte_offset)),

            // 192-255: constant with 6-bit immediate
            192..=255 => {
                let idx = (byte - 192) as u16;
                ops.push(RawOp::Resolved(Op::Constant(idx)));
            }
        }
    }

    Ok((ops, offset_map, jump_patches))
}

fn patch_jumps(
    raw_ops: Vec<RawOp>,
    offset_map: &HashMap<usize, usize>,
    jump_patches: &[JumpPatch],
    bytecode_len: usize,
) -> Result<Vec<Op>, DecodeError> {
    // Build the ops vector, extracting byte targets for jump instructions.
    let mut ops: Vec<Op> = Vec::with_capacity(raw_ops.len());
    let mut byte_targets: HashMap<usize, usize> = HashMap::new();

    for (i, raw) in raw_ops.into_iter().enumerate() {
        match raw {
            RawOp::Resolved(op) => ops.push(op),
            RawOp::Jump(kind, byte_target) => {
                byte_targets.insert(i, byte_target);
                ops.push(match kind {
                    JumpKind::Goto => Op::Goto(0),
                    JumpKind::GotoIfNil => Op::GotoIfNil(0),
                    JumpKind::GotoIfNotNil => Op::GotoIfNotNil(0),
                    JumpKind::GotoIfNilElsePop => Op::GotoIfNilElsePop(0),
                    JumpKind::GotoIfNotNilElsePop => Op::GotoIfNotNilElsePop(0),
                    JumpKind::PushConditionCaseRaw => Op::PushConditionCaseRaw(0),
                    JumpKind::PushCatch => Op::PushCatch(0),
                });
            }
        }
    }

    // Patch jump targets from byte offsets to instruction indices.
    for patch in jump_patches {
        let byte_target = byte_targets[&patch.instr_idx];
        // If byte_target equals the end of the bytecode stream, it points past
        // the last instruction (used for fall-through after the function body).
        let instr_target = if let Some(&idx) = offset_map.get(&byte_target) {
            idx
        } else {
            if byte_target == bytecode_len {
                ops.len()
            } else {
                return Err(DecodeError::InvalidJumpTarget(
                    byte_target,
                    patch.source_byte,
                ));
            }
        };

        let target = instr_target as u32;
        match &mut ops[patch.instr_idx] {
            Op::Goto(addr)
            | Op::GotoIfNil(addr)
            | Op::GotoIfNotNil(addr)
            | Op::GotoIfNilElsePop(addr)
            | Op::GotoIfNotNilElsePop(addr)
            | Op::PushConditionCaseRaw(addr)
            | Op::PushCatch(addr) => {
                *addr = target;
            }
            _ => unreachable!("jump patch on non-jump instruction"),
        }
    }

    Ok(ops)
}

// --- Helper functions ---

/// Fetch a 1-byte operand.
fn fetch1(bytecodes: &[u8], pos: &mut usize, byte_offset: usize) -> Result<u8, DecodeError> {
    if *pos >= bytecodes.len() {
        return Err(DecodeError::UnexpectedEnd(byte_offset));
    }
    let val = bytecodes[*pos];
    *pos += 1;
    Ok(val)
}

/// Fetch a 2-byte (little-endian) operand.
fn fetch2(bytecodes: &[u8], pos: &mut usize, byte_offset: usize) -> Result<u16, DecodeError> {
    if *pos + 1 >= bytecodes.len() {
        return Err(DecodeError::UnexpectedEnd(byte_offset));
    }
    let lo = bytecodes[*pos] as u16;
    let hi = bytecodes[*pos + 1] as u16;
    *pos += 2;
    Ok(lo | (hi << 8))
}

/// Add a symbol to the constants vector if not already present, return its index.
fn add_or_find_symbol(constants: &mut Vec<Value>, name: &str) -> u16 {
    let sym = Value::symbol(name);
    for (i, c) in constants.iter().enumerate() {
        if let (Value::symbol(a), Value::symbol(b)) = (c, &sym) {
            if a == b {
                return i as u16;
            }
        }
    }
    let idx = constants.len() as u16;
    constants.push(sym);
    idx
}

/// Map buffer/point opcode byte (96-127) to (builtin name, arg count).
fn buffer_op_info(byte: u8) -> (&'static str, u8) {
    match byte {
        96 => ("point", 0),
        97 => return ("%%obsolete-mark", 0), // obsolete
        98 => ("goto-char", 1),
        99 => ("insert", 1),
        100 => ("point-max", 0),
        101 => ("point-min", 0),
        102 => ("char-after", 1),
        103 => ("following-char", 0),
        104 => ("preceding-char", 0),
        105 => ("current-column", 0),
        106 => ("indent-to", 1),
        107 => return ("%%obsolete-scan-buffer", 0), // obsolete
        108 => ("eolp", 0),
        109 => ("eobp", 0),
        110 => ("bolp", 0),
        111 => ("bobp", 0),
        112 => ("current-buffer", 0),
        113 => ("set-buffer", 1),
        114 => unreachable!("byte 114 handled as SaveCurrentBuffer"),
        115 => return ("%%obsolete-interactive-p", 0), // obsolete
        116 => return ("%%obsolete-forward-char", 0),  // obsolete
        117 => ("forward-char", 1),
        118 => ("forward-word", 1),
        119 => ("skip-chars-forward", 2),
        120 => ("skip-chars-backward", 2),
        121 => ("forward-line", 1),
        122 => ("char-syntax", 1),
        123 => ("buffer-substring", 2),
        124 => ("delete-region", 2),
        125 => ("narrow-to-region", 2),
        126 => ("widen", 0),
        127 => ("end-of-line", 1),
        _ => unreachable!("buffer_op_info called with byte outside 96-127"),
    }
}

// ---------------------------------------------------------------------------
// Arglist descriptor parsing (Phase 2)
// ---------------------------------------------------------------------------

use crate::emacs_core::intern::{intern, resolve_sym};
use crate::emacs_core::value::LambdaParams;

/// Parse a GNU integer arglist descriptor into `LambdaParams`.
///
/// GNU encoding:
/// - bits 0..6: mandatory argument count
/// - bit 7: `&rest` slot present
/// - bits 8..14: total non-`&rest` argument count (mandatory + optional)
///
/// For lexical bytecode compiled by GNU Emacs, the rest bit describes stack
/// layout, not just source-level `&rest`.  CL-generated constructors can use a
/// hidden extra slot even when the original source arglist only shows
/// `&optional`, so the runtime frame must follow the descriptor exactly.
pub fn parse_arglist_descriptor(descriptor: i64) -> LambdaParams {
    let mandatory = (descriptor & 127) as usize;
    let has_rest = (descriptor & 128) != 0;
    let nonrest = (descriptor >> 8) as usize;
    let optional_count = nonrest.saturating_sub(mandatory);

    let mut required = Vec::with_capacity(mandatory);
    for i in 0..mandatory {
        required.push(intern(&format!("arg{}", i)));
    }
    let mut optional = Vec::with_capacity(optional_count);
    for i in 0..optional_count {
        optional.push(intern(&format!("opt{}", i)));
    }

    LambdaParams {
        required,
        optional,
        rest: has_rest.then(|| intern("rest")),
    }
}

/// Parse an arglist value which can be either an integer descriptor
/// or a list of symbols `(x &optional y &rest z)`.
pub fn parse_arglist_value(arglist: &Value) -> LambdaParams {
    match arglist.kind() {
        ValueKind::Fixnum(n) => parse_arglist_descriptor(n),
        ValueKind::Nil => LambdaParams {
            required: Vec::new(),
            optional: Vec::new(),
            rest: None,
        },
        ValueKind::Cons => {
            // Parse list of symbols
            let items = crate::emacs_core::value::list_to_vec(arglist).unwrap_or_default();
            let mut required = Vec::new();
            let mut optional = Vec::new();
            let mut rest = None;
            let mut mode = 0u8; // 0 = required, 1 = optional, 2 = rest

            for item in &items {
                if let Some(name) = item.as_symbol_name() {
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
                }
                let sym_id = match item.kind() {
                    ValueKind::Symbol(id) => id,
                    _ => intern("_"),
                };
                match mode {
                    0 => required.push(sym_id),
                    1 => optional.push(sym_id),
                    2 => {
                        rest = Some(sym_id);
                        break; // Only one rest param
                    }
                    _ => unreachable!(),
                }
            }
            LambdaParams {
                required,
                optional,
                rest,
            }
        }
        _ => {
            // Fallback: treat as zero-arg
            LambdaParams {
                required: Vec::new(),
                optional: Vec::new(),
                rest: None,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "decode_test.rs"]
mod tests;
