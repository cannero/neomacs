//! Emacs regex matching engine.
//!
//! Uses failure-stack backtracking to match compiled bytecode patterns
//! against input strings. Supports capture groups, backreferences,
//! syntax table queries, and case-folding via the `CharProperties` trait.

use super::types::*;

/// Maximum number of failure points to prevent catastrophic backtracking.
const MAX_FAILURES: usize = 40000;

/// A saved failure point for backtracking.
#[derive(Clone)]
struct FailurePoint {
    /// Position in bytecode to resume from.
    pc: usize,
    /// Position in input string to resume from.
    pos: usize,
    /// Saved register state (start positions).
    reg_starts: Vec<i64>,
    /// Saved register state (end positions).
    reg_ends: Vec<i64>,
}

/// Match a compiled pattern against an input string.
///
/// Returns the end position of the match (in bytes) on success,
/// or None if no match.
pub fn match_pattern(
    pattern: &PatternBuffer,
    input: &str,
    start: usize,
    props: &dyn CharProperties,
    regs: &mut Option<&mut Registers>,
) -> Option<usize> {
    let bytes = input.as_bytes();
    // Clone bytecode to allow in-place mutation for opcodes like SucceedN
    // and SetNumberAt that modify counter values during matching (mirroring
    // how Emacs's C regex engine self-modifies its bytecode buffer).
    let mut bytecode = pattern.bytecode.clone();
    let num_regs = pattern.num_groups + 1;

    // Initialize registers
    let mut reg_starts = vec![-1i64; num_regs];
    let mut reg_ends = vec![-1i64; num_regs];
    reg_starts[0] = start as i64;

    // Failure stack
    let mut fail_stack: Vec<FailurePoint> = Vec::new();

    let mut pc: usize = 0; // Program counter (bytecode position)
    let mut pos: usize = start; // Current position in input

    loop {
        if pc >= bytecode.len() {
            return None; // Ran off end of bytecode
        }

        let opcode = match Opcode::from_u8(bytecode[pc]) {
            Some(op) => op,
            None => return None, // Invalid opcode
        };

        match opcode {
            Opcode::Succeed => {
                // Match succeeded
                reg_ends[0] = pos as i64;
                if let Some(r) = regs {
                    let n = r.num_regs.min(num_regs);
                    for i in 0..n {
                        r.starts[i] = reg_starts[i];
                        r.ends[i] = reg_ends[i];
                    }
                }
                return Some(pos);
            }

            Opcode::NoOp => {
                pc += 1;
            }

            Opcode::Exactn => {
                pc += 1;
                let count = bytecode[pc] as usize;
                pc += 1;

                if pos + count > bytes.len() {
                    // Not enough input — fail
                    if !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends) {
                        return None;
                    }
                    continue;
                }

                let mut matched = true;
                for i in 0..count {
                    let pattern_byte = bytecode[pc + i];
                    let input_byte = bytes[pos + i];

                    // Apply case folding
                    let pc = props.translate(pattern_byte as char);
                    let ic = props.translate(input_byte as char);
                    if pc != ic {
                        matched = false;
                        break;
                    }
                }

                if matched {
                    pc += count;
                    pos += count;
                } else if !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends) {
                    return None;
                }
            }

            Opcode::AnyChar => {
                pc += 1;
                if pos >= bytes.len() || bytes[pos] == b'\n' {
                    if !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends) {
                        return None;
                    }
                    continue;
                }

                // Advance past the character (handle multibyte)
                if pattern.multibyte {
                    let ch_len = utf8_char_len(bytes[pos]);
                    if pos + ch_len > bytes.len() {
                        if !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends) {
                            return None;
                        }
                        continue;
                    }
                    pos += ch_len;
                } else {
                    pos += 1;
                }
            }

            Opcode::Charset | Opcode::CharsetNot => {
                let negated = opcode == Opcode::CharsetNot;
                pc += 1;

                if pos >= bytes.len() {
                    if !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends) {
                        return None;
                    }
                    continue;
                }

                let size_byte = bytecode[pc];
                pc += 1;

                let bitmap_size = (size_byte & 0x7F) as usize;
                let has_range_table = (size_byte & 0x80) != 0;

                // Get the character at current position
                let (ch, ch_len) = if pattern.multibyte {
                    decode_utf8(&bytes[pos..])
                } else {
                    (bytes[pos] as u32, 1)
                };

                let mut in_set = false;

                // Check ASCII bitmap
                if ch < 256 && bitmap_size > 0 {
                    let byte_idx = (ch / 8) as usize;
                    let bit = (ch % 8) as u8;
                    if byte_idx < bitmap_size {
                        in_set = (bytecode[pc + byte_idx] & (1 << bit)) != 0;
                    }
                }

                let after_bitmap = pc + bitmap_size;
                pc = after_bitmap;

                // Always advance past range table if present
                if has_range_table {
                    let _flags_lo = bytecode[pc];
                    let _flags_hi = bytecode[pc + 1];
                    let flags = _flags_lo as u16 | ((_flags_hi as u16) << 8);
                    let count = bytecode[pc + 2] as u16 | ((bytecode[pc + 3] as u16) << 8);
                    pc += 4;

                    if !in_set {
                        // Check character class flags
                        if flags != 0 && ch >= 128 {
                            let ch_char = char::from_u32(ch).unwrap_or('\0');
                            if (flags & 0x001) != 0 && CharClass::Word.matches(ch_char) { in_set = true; }
                            if (flags & 0x002) != 0 && CharClass::Lower.matches(ch_char) { in_set = true; }
                            if (flags & 0x004) != 0 && CharClass::Punct.matches(ch_char) { in_set = true; }
                            if (flags & 0x008) != 0 && CharClass::Space.matches(ch_char) { in_set = true; }
                            if (flags & 0x010) != 0 && CharClass::Upper.matches(ch_char) { in_set = true; }
                            if (flags & 0x020) != 0 && CharClass::Multibyte.matches(ch_char) { in_set = true; }
                            if (flags & 0x040) != 0 && CharClass::Alpha.matches(ch_char) { in_set = true; }
                            if (flags & 0x080) != 0 && CharClass::Alnum.matches(ch_char) { in_set = true; }
                            if (flags & 0x100) != 0 && CharClass::Graph.matches(ch_char) { in_set = true; }
                            if (flags & 0x200) != 0 && CharClass::Print.matches(ch_char) { in_set = true; }
                            if (flags & 0x400) != 0 && CharClass::Blank.matches(ch_char) { in_set = true; }
                        }

                        // Check ranges
                        for _ in 0..count {
                            let start = bytecode[pc] as u32
                                | ((bytecode[pc + 1] as u32) << 8)
                                | ((bytecode[pc + 2] as u32) << 16);
                            let end = bytecode[pc + 3] as u32
                                | ((bytecode[pc + 4] as u32) << 8)
                                | ((bytecode[pc + 5] as u32) << 16);
                            pc += 6;

                            if ch >= start && ch <= end {
                                in_set = true;
                            }
                        }
                    } else {
                        // Skip range data
                        pc += count as usize * 6;
                    }
                }

                let matched = in_set != negated;
                if matched {
                    pos += ch_len;
                } else if !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends) {
                    return None;
                }
            }

            Opcode::StartMemory => {
                pc += 1;
                let group = bytecode[pc] as usize;
                pc += 1;
                if group < num_regs {
                    reg_starts[group] = pos as i64;
                    reg_ends[group] = -1;
                }
            }

            Opcode::StopMemory => {
                pc += 1;
                let group = bytecode[pc] as usize;
                pc += 1;
                if group < num_regs {
                    reg_ends[group] = pos as i64;
                }
            }

            Opcode::Duplicate => {
                pc += 1;
                let group = bytecode[pc] as usize;
                pc += 1;

                if group >= num_regs || reg_starts[group] < 0 || reg_ends[group] < 0 {
                    if !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends) {
                        return None;
                    }
                    continue;
                }

                let ref_start = reg_starts[group] as usize;
                let ref_end = reg_ends[group] as usize;
                let ref_len = ref_end - ref_start;

                if pos + ref_len > bytes.len() {
                    if !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends) {
                        return None;
                    }
                    continue;
                }

                let mut matched = true;
                for j in 0..ref_len {
                    let rc = props.translate(bytes[ref_start + j] as char);
                    let ic = props.translate(bytes[pos + j] as char);
                    if rc != ic {
                        matched = false;
                        break;
                    }
                }

                if matched {
                    pos += ref_len;
                } else if !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends) {
                    return None;
                }
            }

            Opcode::BegLine => {
                pc += 1;
                if pos == 0 || bytes[pos - 1] == b'\n' {
                    // OK
                } else if !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends) {
                    return None;
                }
            }

            Opcode::EndLine => {
                pc += 1;
                if pos == bytes.len() || bytes[pos] == b'\n' {
                    // OK
                } else if !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends) {
                    return None;
                }
            }

            Opcode::BegBuf => {
                pc += 1;
                if pos == 0 {
                    // OK
                } else if !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends) {
                    return None;
                }
            }

            Opcode::EndBuf => {
                pc += 1;
                if pos == bytes.len() {
                    // OK
                } else if !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends) {
                    return None;
                }
            }

            Opcode::Jump => {
                pc += 1;
                let offset = bytecode[pc] as i16 | ((bytecode[pc + 1] as i16) << 8);
                pc += 2;
                pc = (pc as i64 + offset as i64) as usize;
            }

            Opcode::OnFailureJump | Opcode::OnFailureJumpLoop | Opcode::OnFailureJumpSmart => {
                pc += 1;
                let offset = bytecode[pc] as i16 | ((bytecode[pc + 1] as i16) << 8);
                pc += 2;

                if fail_stack.len() >= MAX_FAILURES {
                    return None; // Stack overflow
                }

                let fail_pc = (pc as i64 + offset as i64) as usize;
                fail_stack.push(FailurePoint {
                    pc: fail_pc,
                    pos,
                    reg_starts: reg_starts.clone(),
                    reg_ends: reg_ends.clone(),
                });
            }

            Opcode::OnFailureKeepStringJump => {
                pc += 1;
                let offset = bytecode[pc] as i16 | ((bytecode[pc + 1] as i16) << 8);
                pc += 2;

                if fail_stack.len() >= MAX_FAILURES {
                    return None;
                }

                let fail_pc = (pc as i64 + offset as i64) as usize;
                // Don't save string position — on failure, keep current position
                fail_stack.push(FailurePoint {
                    pc: fail_pc,
                    pos, // This will be updated on failure
                    reg_starts: reg_starts.clone(),
                    reg_ends: reg_ends.clone(),
                });
            }

            Opcode::OnFailureJumpNastyloop => {
                pc += 1;
                let offset = bytecode[pc] as i16 | ((bytecode[pc + 1] as i16) << 8);
                pc += 2;

                if fail_stack.len() >= MAX_FAILURES {
                    return None;
                }

                let fail_pc = (pc as i64 + offset as i64) as usize;
                fail_stack.push(FailurePoint {
                    pc: fail_pc,
                    pos,
                    reg_starts: reg_starts.clone(),
                    reg_ends: reg_ends.clone(),
                });
            }

            Opcode::SucceedN => {
                // succeed_n: [offset:2] [count:2]
                // If count > 0, decrement counter in-place and fall through
                // to execute the loop body. If count == 0, jump past the
                // loop body (the repetition is satisfied).
                pc += 1;
                let offset = bytecode[pc] as i16 | ((bytecode[pc + 1] as i16) << 8);
                let count = bytecode[pc + 2] as u16 | ((bytecode[pc + 3] as u16) << 8);
                if count > 0 {
                    // Decrement counter in-place (self-modifying bytecode)
                    let new_count = count - 1;
                    bytecode[pc + 2] = new_count as u8;
                    bytecode[pc + 3] = (new_count >> 8) as u8;
                    pc += 4;
                } else {
                    // Counter reached zero — jump past the loop body
                    pc += 4;
                    pc = (pc as i64 + offset as i64) as usize;
                }
            }

            Opcode::JumpN => {
                // jump_n: [offset:2] [count:2]
                // If count > 0, decrement counter in-place and jump.
                // If count <= 0, fall through (don't jump).
                pc += 1;
                let offset = bytecode[pc] as i16 | ((bytecode[pc + 1] as i16) << 8);
                let count = bytecode[pc + 2] as u16 | ((bytecode[pc + 3] as u16) << 8);
                if count > 0 {
                    let new_count = count - 1;
                    bytecode[pc + 2] = new_count as u8;
                    bytecode[pc + 3] = (new_count >> 8) as u8;
                    pc += 4;
                    pc = (pc as i64 + offset as i64) as usize;
                } else {
                    pc += 4;
                }
            }

            Opcode::SetNumberAt => {
                // set_number_at: [offset:2] [value:2]
                // Writes the 2-byte value into the bytecode at address
                // (pc + 4 + offset). Used to reset/set the repetition
                // counter for SucceedN/JumpN after each loop iteration.
                pc += 1;
                let offset = bytecode[pc] as i16 | ((bytecode[pc + 1] as i16) << 8);
                let value = bytecode[pc + 2] as u16 | ((bytecode[pc + 3] as u16) << 8);
                pc += 4;
                let target = (pc as i64 + offset as i64) as usize;
                bytecode[target] = value as u8;
                bytecode[target + 1] = (value >> 8) as u8;
            }

            Opcode::WordBeg => {
                pc += 1;
                let at_word_start = is_word_start(input, pos, props);
                if !at_word_start
                    && !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends)
                {
                    return None;
                }
            }

            Opcode::WordEnd => {
                pc += 1;
                let at_word_end = is_word_end(input, pos, props);
                if !at_word_end
                    && !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends)
                {
                    return None;
                }
            }

            Opcode::WordBound => {
                pc += 1;
                let at_boundary = is_word_boundary(input, pos, props);
                if !at_boundary
                    && !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends)
                {
                    return None;
                }
            }

            Opcode::NotWordBound => {
                pc += 1;
                let at_boundary = is_word_boundary(input, pos, props);
                if at_boundary
                    && !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends)
                {
                    return None;
                }
            }

            Opcode::SymBeg => {
                pc += 1;
                let at_sym_start = is_symbol_start(input, pos, props);
                if !at_sym_start
                    && !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends)
                {
                    return None;
                }
            }

            Opcode::SymEnd => {
                pc += 1;
                let at_sym_end = is_symbol_end(input, pos, props);
                if !at_sym_end
                    && !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends)
                {
                    return None;
                }
            }

            Opcode::SyntaxSpec => {
                pc += 1;
                let expected_class = bytecode[pc];
                pc += 1;

                if pos >= bytes.len() {
                    if !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends) {
                        return None;
                    }
                    continue;
                }

                let (ch, ch_len) = if pattern.multibyte {
                    decode_utf8_char(&bytes[pos..])
                } else {
                    (bytes[pos] as char, 1)
                };

                let actual_class = props.syntax_class(ch, pos) as u8;
                if actual_class == expected_class {
                    pos += ch_len;
                } else if !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends) {
                    return None;
                }
            }

            Opcode::NotSyntaxSpec => {
                pc += 1;
                let expected_class = bytecode[pc];
                pc += 1;

                if pos >= bytes.len() {
                    if !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends) {
                        return None;
                    }
                    continue;
                }

                let (ch, ch_len) = if pattern.multibyte {
                    decode_utf8_char(&bytes[pos..])
                } else {
                    (bytes[pos] as char, 1)
                };

                let actual_class = props.syntax_class(ch, pos) as u8;
                if actual_class != expected_class {
                    pos += ch_len;
                } else if !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends) {
                    return None;
                }
            }

            Opcode::CategorySpec => {
                pc += 1;
                let cat = bytecode[pc];
                pc += 1;

                if pos >= bytes.len() {
                    if !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends) {
                        return None;
                    }
                    continue;
                }

                let (ch, ch_len) = if pattern.multibyte {
                    decode_utf8_char(&bytes[pos..])
                } else {
                    (bytes[pos] as char, 1)
                };

                if props.in_category(ch, cat) {
                    pos += ch_len;
                } else if !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends) {
                    return None;
                }
            }

            Opcode::NotCategorySpec => {
                pc += 1;
                let cat = bytecode[pc];
                pc += 1;

                if pos >= bytes.len() {
                    if !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends) {
                        return None;
                    }
                    continue;
                }

                let (ch, ch_len) = if pattern.multibyte {
                    decode_utf8_char(&bytes[pos..])
                } else {
                    (bytes[pos] as char, 1)
                };

                if !props.in_category(ch, cat) {
                    pos += ch_len;
                } else if !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends) {
                    return None;
                }
            }

            Opcode::AtDot => {
                // Match at point — not applicable for string matching
                pc += 1;
                if !backtrack(&mut fail_stack, &mut pc, &mut pos, &mut reg_starts, &mut reg_ends) {
                    return None;
                }
            }
        }
    }
}

/// Search for a pattern in an input string.
///
/// Tries matching at each position from `start` to `start + range`.
/// Returns the byte position where the match starts, or None.
pub fn search(
    pattern: &PatternBuffer,
    input: &str,
    start: usize,
    range: i64,
    props: &dyn CharProperties,
    regs: &mut Option<&mut Registers>,
) -> Option<usize> {
    let bytes = input.as_bytes();
    let len = bytes.len();

    let (search_start, search_end, forward) = if range >= 0 {
        let end = (start as i64 + range).min(len as i64) as usize;
        (start, end, true)
    } else {
        let end = (start as i64 + range).max(0) as i64 as usize;
        (end, start, false)
    };

    if forward {
        let mut pos = search_start;
        while pos <= search_end {
            // Fastmap check
            if pattern.fastmap_accurate && pos < len && !pattern.fastmap[bytes[pos] as usize] {
                pos += 1;
                continue;
            }

            if let Some(end_pos) = match_pattern(pattern, input, pos, props, regs) {
                // Update register 0 with match start
                if let Some(r) = regs {
                    r.starts[0] = pos as i64;
                    r.ends[0] = end_pos as i64;
                }
                return Some(pos);
            }
            pos += 1;
        }
    } else {
        let mut pos = search_end;
        loop {
            if pattern.fastmap_accurate && pos < len && !pattern.fastmap[bytes[pos] as usize] {
                if pos == search_start {
                    break;
                }
                pos -= 1;
                continue;
            }

            if let Some(end_pos) = match_pattern(pattern, input, pos, props, regs) {
                if let Some(r) = regs {
                    r.starts[0] = pos as i64;
                    r.ends[0] = end_pos as i64;
                }
                return Some(pos);
            }
            if pos == search_start {
                break;
            }
            pos -= 1;
        }
    }

    None
}

// ===== Helper functions =====

fn backtrack(
    fail_stack: &mut Vec<FailurePoint>,
    pc: &mut usize,
    pos: &mut usize,
    reg_starts: &mut Vec<i64>,
    reg_ends: &mut Vec<i64>,
) -> bool {
    if let Some(fp) = fail_stack.pop() {
        *pc = fp.pc;
        *pos = fp.pos;
        *reg_starts = fp.reg_starts;
        *reg_ends = fp.reg_ends;
        true
    } else {
        false
    }
}

fn utf8_char_len(first_byte: u8) -> usize {
    if first_byte < 0x80 {
        1
    } else if first_byte < 0xC0 {
        1 // Continuation byte, shouldn't happen at start
    } else if first_byte < 0xE0 {
        2
    } else if first_byte < 0xF0 {
        3
    } else {
        4
    }
}

fn decode_utf8(bytes: &[u8]) -> (u32, usize) {
    if bytes.is_empty() {
        return (0, 0);
    }
    let len = utf8_char_len(bytes[0]);
    if len > bytes.len() {
        return (bytes[0] as u32, 1);
    }
    match len {
        1 => (bytes[0] as u32, 1),
        2 => {
            let cp = ((bytes[0] as u32 & 0x1F) << 6) | (bytes[1] as u32 & 0x3F);
            (cp, 2)
        }
        3 => {
            let cp = ((bytes[0] as u32 & 0x0F) << 12)
                | ((bytes[1] as u32 & 0x3F) << 6)
                | (bytes[2] as u32 & 0x3F);
            (cp, 3)
        }
        4 => {
            let cp = ((bytes[0] as u32 & 0x07) << 18)
                | ((bytes[1] as u32 & 0x3F) << 12)
                | ((bytes[2] as u32 & 0x3F) << 6)
                | (bytes[3] as u32 & 0x3F);
            (cp, 4)
        }
        _ => (bytes[0] as u32, 1),
    }
}

fn decode_utf8_char(bytes: &[u8]) -> (char, usize) {
    let (cp, len) = decode_utf8(bytes);
    (char::from_u32(cp).unwrap_or('\u{FFFD}'), len)
}

fn char_at(input: &str, byte_pos: usize) -> Option<char> {
    if byte_pos >= input.len() {
        return None;
    }
    let bytes = input.as_bytes();
    let (ch, _) = decode_utf8_char(&bytes[byte_pos..]);
    Some(ch)
}

fn char_before(input: &str, byte_pos: usize) -> Option<char> {
    if byte_pos == 0 {
        return None;
    }
    // Walk backward to find the start of the previous char
    let bytes = input.as_bytes();
    let mut start = byte_pos - 1;
    while start > 0 && (bytes[start] & 0xC0) == 0x80 {
        start -= 1;
    }
    let (ch, _) = decode_utf8_char(&bytes[start..]);
    Some(ch)
}

fn is_word_char(ch: char, pos: usize, props: &dyn CharProperties) -> bool {
    props.syntax_class(ch, pos) == SyntaxClass::Word
}

fn is_word_boundary(input: &str, pos: usize, props: &dyn CharProperties) -> bool {
    let before = char_before(input, pos).map(|c| is_word_char(c, pos.saturating_sub(1), props));
    let after = char_at(input, pos).map(|c| is_word_char(c, pos, props));

    match (before, after) {
        (Some(bw), Some(aw)) => bw != aw,
        (None, Some(aw)) => aw,
        (Some(bw), None) => bw,
        (None, None) => false,
    }
}

fn is_word_start(input: &str, pos: usize, props: &dyn CharProperties) -> bool {
    let before = char_before(input, pos).map(|c| is_word_char(c, pos.saturating_sub(1), props));
    let after = char_at(input, pos).map(|c| is_word_char(c, pos, props));

    match (before, after) {
        (Some(bw), Some(aw)) => !bw && aw,
        (None, Some(aw)) => aw,
        _ => false,
    }
}

fn is_word_end(input: &str, pos: usize, props: &dyn CharProperties) -> bool {
    let before = char_before(input, pos).map(|c| is_word_char(c, pos.saturating_sub(1), props));
    let after = char_at(input, pos).map(|c| is_word_char(c, pos, props));

    match (before, after) {
        (Some(bw), Some(aw)) => bw && !aw,
        (Some(bw), None) => bw,
        _ => false,
    }
}

fn is_symbol_char(ch: char, pos: usize, props: &dyn CharProperties) -> bool {
    let sc = props.syntax_class(ch, pos);
    sc == SyntaxClass::Word || sc == SyntaxClass::Symbol
}

fn is_symbol_start(input: &str, pos: usize, props: &dyn CharProperties) -> bool {
    let before = char_before(input, pos).map(|c| is_symbol_char(c, pos.saturating_sub(1), props));
    let after = char_at(input, pos).map(|c| is_symbol_char(c, pos, props));

    match (before, after) {
        (Some(bw), Some(aw)) => !bw && aw,
        (None, Some(aw)) => aw,
        _ => false,
    }
}

fn is_symbol_end(input: &str, pos: usize, props: &dyn CharProperties) -> bool {
    let before = char_before(input, pos).map(|c| is_symbol_char(c, pos.saturating_sub(1), props));
    let after = char_at(input, pos).map(|c| is_symbol_char(c, pos, props));

    match (before, after) {
        (Some(bw), Some(aw)) => bw && !aw,
        (Some(bw), None) => bw,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::compile;

    fn search_str(pattern: &str, input: &str) -> Option<usize> {
        let buf = compile::compile(pattern, true).unwrap();
        let props = DefaultCharProperties;
        search(&buf, input, 0, input.len() as i64, &props, &mut None)
    }

    fn match_str(pattern: &str, input: &str) -> bool {
        search_str(pattern, input).is_some()
    }

    fn captures(pattern: &str, input: &str) -> Option<(Vec<i64>, Vec<i64>)> {
        let buf = compile::compile(pattern, true).unwrap();
        let props = DefaultCharProperties;
        let mut regs = Registers::new(buf.num_groups + 1);
        let mut regs_opt: Option<&mut Registers> = Some(&mut regs);
        let result = search(&buf, input, 0, input.len() as i64, &props, &mut regs_opt);
        result.map(|_| (regs.starts, regs.ends))
    }

    #[test]
    fn test_literal_match() {
        assert!(match_str("hello", "hello world"));
        assert!(!match_str("hello", "goodbye"));
    }

    #[test]
    fn test_dot() {
        assert!(match_str("h.llo", "hello"));
        assert!(match_str("h.llo", "hallo"));
        assert!(!match_str("h.llo", "hllo"));
    }

    #[test]
    fn test_dot_not_newline() {
        assert!(!match_str("a.b", "a\nb"));
    }

    #[test]
    fn test_anchors() {
        assert!(match_str("^hello", "hello world"));
        assert!(!match_str("^hello", " hello"));
        assert!(match_str("world$", "hello world"));
        assert!(!match_str("world$", "hello world!"));
    }

    #[test]
    fn test_star() {
        assert!(match_str("ab*c", "ac"));
        assert!(match_str("ab*c", "abc"));
        assert!(match_str("ab*c", "abbc"));
        assert!(match_str("ab*c", "abbbbbbc"));
    }

    #[test]
    fn test_plus() {
        assert!(!match_str("ab+c", "ac"));
        assert!(match_str("ab+c", "abc"));
        assert!(match_str("ab+c", "abbc"));
    }

    #[test]
    fn test_optional() {
        assert!(match_str("ab?c", "ac"));
        assert!(match_str("ab?c", "abc"));
        assert!(!match_str("ab?c", "abbc"));
    }

    #[test]
    fn test_charset() {
        assert!(match_str("[abc]", "a"));
        assert!(match_str("[abc]", "b"));
        assert!(match_str("[abc]", "c"));
        assert!(!match_str("[abc]", "d"));
    }

    #[test]
    fn test_charset_range() {
        assert!(match_str("[a-z]", "m"));
        assert!(!match_str("[a-z]", "M"));
        assert!(match_str("[0-9]", "5"));
    }

    #[test]
    fn test_negated_charset() {
        assert!(!match_str("[^abc]", "a"));
        assert!(match_str("[^abc]", "d"));
    }

    #[test]
    fn test_groups() {
        let result = captures("\\(foo\\)", "foobar");
        assert!(result.is_some());
        let (starts, ends) = result.unwrap();
        assert_eq!(starts[0], 0);
        assert_eq!(ends[0], 3);
        assert_eq!(starts[1], 0);
        assert_eq!(ends[1], 3);
    }

    #[test]
    fn test_word_boundary() {
        assert!(match_str("\\bfoo\\b", "hello foo bar"));
        assert!(!match_str("\\bfoo\\b", "hellofoobarn"));
    }

    #[test]
    fn test_buf_boundaries() {
        assert!(match_str("\\`hello", "hello world"));
        assert!(!match_str("\\`hello", " hello"));
        assert!(match_str("world\\'", "hello world"));
    }

    #[test]
    fn test_search_position() {
        let pos = search_str("world", "hello world");
        assert_eq!(pos, Some(6));
    }

    #[test]
    fn test_escaped_literal() {
        assert!(match_str("a\\.b", "a.b"));
        assert!(!match_str("a\\.b", "axb"));
    }

    #[test]
    fn test_backreference() {
        assert!(match_str("\\(foo\\)bar\\1", "foobarfoo"));
        assert!(!match_str("\\(foo\\)bar\\1", "foobarbar"));
    }

    #[test]
    fn test_newline_anchor() {
        assert!(match_str("^line2", "line1\nline2"));
    }

    #[test]
    fn test_syntax_word_class() {
        assert!(match_str("\\sw", "hello"));
        assert!(!match_str("\\sw", " "));
    }

    #[test]
    fn test_empty_match() {
        assert!(match_str("", "anything"));
    }

    #[test]
    fn test_charset_named_class() {
        assert!(match_str("[[:digit:]]", "5"));
        assert!(!match_str("[[:digit:]]", "a"));
        assert!(match_str("[[:alpha:]]", "a"));
        assert!(!match_str("[[:alpha:]]", "5"));
    }

    // ===== Bounded repetition tests =====

    #[test]
    fn test_exact_repetition() {
        // a\{3\}b — exactly 3 'a's followed by 'b'
        assert!(match_str("a\\{3\\}b", "aaab"));
        assert!(!match_str("a\\{3\\}b", "aab"));
        assert!(!match_str("a\\{3\\}b", "ab"));
        // 4 a's: the pattern should still match (search finds "aaab" substring)
        assert!(match_str("a\\{3\\}b", "aaaab"));
    }

    #[test]
    fn test_exact_repetition_anchored() {
        // ^a\{3\}b$ — exactly 3 'a's, anchored
        assert!(match_str("^a\\{3\\}b$", "aaab"));
        assert!(!match_str("^a\\{3\\}b$", "aab"));
        assert!(!match_str("^a\\{3\\}b$", "aaaab"));
        assert!(!match_str("^a\\{3\\}b$", "ab"));
    }

    #[test]
    fn test_bounded_range_repetition() {
        // a\{2,4\}b — 2 to 4 'a's followed by 'b'
        assert!(!match_str("^a\\{2,4\\}b$", "ab"));
        assert!(match_str("^a\\{2,4\\}b$", "aab"));
        assert!(match_str("^a\\{2,4\\}b$", "aaab"));
        assert!(match_str("^a\\{2,4\\}b$", "aaaab"));
        assert!(!match_str("^a\\{2,4\\}b$", "aaaaab"));
    }

    #[test]
    fn test_zero_min_repetition() {
        // a\{0,3\}b — 0 to 3 'a's followed by 'b'
        assert!(match_str("^a\\{0,3\\}b$", "b"));
        assert!(match_str("^a\\{0,3\\}b$", "ab"));
        assert!(match_str("^a\\{0,3\\}b$", "aab"));
        assert!(match_str("^a\\{0,3\\}b$", "aaab"));
        assert!(!match_str("^a\\{0,3\\}b$", "aaaab"));
    }

    #[test]
    fn test_unbounded_max_repetition() {
        // a\{1,\}b — 1 or more 'a's followed by 'b' (like a+b)
        assert!(!match_str("^a\\{1,\\}b$", "b"));
        assert!(match_str("^a\\{1,\\}b$", "ab"));
        assert!(match_str("^a\\{1,\\}b$", "aab"));
        assert!(match_str("^a\\{1,\\}b$", "aaab"));
        assert!(match_str("^a\\{1,\\}b$", "aaaaaaaab"));
    }

    #[test]
    fn test_zero_min_unbounded_repetition() {
        // a\{0,\}b — 0 or more 'a's followed by 'b' (like a*b)
        assert!(match_str("^a\\{0,\\}b$", "b"));
        assert!(match_str("^a\\{0,\\}b$", "ab"));
        assert!(match_str("^a\\{0,\\}b$", "aaab"));
    }

    #[test]
    fn test_exact_zero_repetition() {
        // a\{0\}b — exactly 0 'a's, so just matches 'b'
        assert!(match_str("^a\\{0\\}b$", "b"));
        assert!(!match_str("^a\\{0\\}b$", "ab"));
    }

    #[test]
    fn test_exact_one_repetition() {
        // a\{1\}b — exactly 1 'a' followed by 'b'
        assert!(match_str("^a\\{1\\}b$", "ab"));
        assert!(!match_str("^a\\{1\\}b$", "b"));
        assert!(!match_str("^a\\{1\\}b$", "aab"));
    }

    #[test]
    fn test_bounded_repetition_with_charset() {
        // [0-9]\{2,4\} — 2 to 4 digits
        assert!(!match_str("^[0-9]\\{2,4\\}$", "1"));
        assert!(match_str("^[0-9]\\{2,4\\}$", "12"));
        assert!(match_str("^[0-9]\\{2,4\\}$", "123"));
        assert!(match_str("^[0-9]\\{2,4\\}$", "1234"));
        assert!(!match_str("^[0-9]\\{2,4\\}$", "12345"));
    }

    #[test]
    fn test_bounded_repetition_with_dot() {
        // .\{2,3\}x — 2 to 3 of any char followed by 'x'
        assert!(!match_str("^.\\{2,3\\}x$", "ax"));
        assert!(match_str("^.\\{2,3\\}x$", "abx"));
        assert!(match_str("^.\\{2,3\\}x$", "abcx"));
        assert!(!match_str("^.\\{2,3\\}x$", "abcdx"));
    }

    #[test]
    fn test_bounded_repetition_search() {
        // Non-anchored search should find the match within the string
        let pos = search_str("a\\{3\\}", "xxaaaxxx");
        assert_eq!(pos, Some(2));
    }
}
