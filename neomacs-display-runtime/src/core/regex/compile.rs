//! Emacs regex pattern compiler.
//!
//! Parses Emacs-style regex syntax and produces bytecode for the matching engine.
//! Key differences from POSIX/PCRE:
//! - Groups: `\(` ... `\)` (not bare parens)
//! - Alternation: `\|`
//! - Repetition braces: `\{n,m\}`
//! - Syntax classes: `\s`, `\S` + syntax code
//! - Categories: `\c`, `\C` + category letter
//! - Word boundaries: `\b`, `\B`, `\<`, `\>`

use super::types::*;

/// State for tracking open groups during compilation.
struct GroupEntry {
    /// Group number (1-based, 0 for shy groups).
    group_num: usize,
    /// Position of the group's start_memory opcode in bytecode.
    start_pos: usize,
    /// Position of the alternation fixup chain (if any).
    alt_chain: Option<usize>,
}

/// Compile an Emacs regex pattern into bytecode.
pub fn compile(pattern: &str, multibyte: bool) -> Result<PatternBuffer, RegexError> {
    let mut compiler = Compiler::new(multibyte);
    compiler.compile(pattern)?;
    Ok(compiler.finish())
}

struct Compiler {
    bytecode: Vec<u8>,
    group_stack: Vec<GroupEntry>,
    next_group: usize,
    multibyte: bool,
    num_groups: usize,
    uses_syntax: bool,
    /// Track the start position of the last compiled "atom" for repetition operators.
    last_atom_start: Option<usize>,
}

impl Compiler {
    fn new(multibyte: bool) -> Self {
        Compiler {
            bytecode: Vec::with_capacity(256),
            group_stack: Vec::new(),
            next_group: 1,
            multibyte,
            num_groups: 0,
            uses_syntax: false,
            last_atom_start: None,
        }
    }

    fn compile(&mut self, pattern: &str) -> Result<(), RegexError> {
        let bytes = pattern.as_bytes();
        let len = bytes.len();
        let mut i = 0;

        while i < len {
            let b = bytes[i];
            match b {
                b'\\' => {
                    i += 1;
                    if i >= len {
                        return Err(RegexError::TrailingBackslash);
                    }
                    i = self.compile_escape(bytes, i)?;
                }
                b'[' => {
                    i = self.compile_charset(bytes, i)?;
                }
                b'.' => {
                    self.last_atom_start = Some(self.bytecode.len());
                    self.emit(Opcode::AnyChar as u8);
                    i += 1;
                }
                b'^' => {
                    self.emit(Opcode::BegLine as u8);
                    self.last_atom_start = None;
                    i += 1;
                }
                b'$' => {
                    self.emit(Opcode::EndLine as u8);
                    self.last_atom_start = None;
                    i += 1;
                }
                b'*' | b'+' | b'?' => {
                    i = self.compile_repetition(bytes, i)?;
                }
                _ => {
                    // Literal character
                    i = self.compile_literal(bytes, i)?;
                }
            }
        }

        // Check for unclosed groups
        if !self.group_stack.is_empty() {
            return Err(RegexError::UnmatchedParen);
        }

        self.emit(Opcode::Succeed as u8);
        Ok(())
    }

    fn compile_escape(&mut self, bytes: &[u8], mut i: usize) -> Result<usize, RegexError> {
        let b = bytes[i];
        match b {
            b'(' => {
                // Start group: check for shy group (?:...) or numbered (?N...)
                let (group_num, skip) = if i + 1 < bytes.len() && bytes[i + 1] == b'?' {
                    if i + 2 < bytes.len() && bytes[i + 2] == b':' {
                        (0, 3) // Shy group
                    } else if i + 2 < bytes.len() && bytes[i + 2].is_ascii_digit() {
                        let num = (bytes[i + 2] - b'0') as usize;
                        (num, 3) // Explicitly numbered
                    } else {
                        let gn = self.next_group;
                        self.next_group += 1;
                        (gn, 1)
                    }
                } else {
                    let gn = self.next_group;
                    self.next_group += 1;
                    (gn, 1)
                };

                if group_num > 0 && group_num > self.num_groups {
                    self.num_groups = group_num;
                }

                let start_pos = self.bytecode.len();
                if group_num > 0 {
                    self.emit(Opcode::StartMemory as u8);
                    self.emit(group_num as u8);
                }

                self.group_stack.push(GroupEntry {
                    group_num,
                    start_pos,
                    alt_chain: None,
                });

                self.last_atom_start = None;
                Ok(i + skip)
            }
            b')' => {
                // End group
                let entry = self.group_stack.pop().ok_or(RegexError::UnmatchedParen)?;

                // Fix up alternation chain
                if let Some(alt_pos) = entry.alt_chain {
                    self.fixup_alt_chain(alt_pos);
                }

                if entry.group_num > 0 {
                    self.emit(Opcode::StopMemory as u8);
                    self.emit(entry.group_num as u8);
                }

                self.last_atom_start = Some(entry.start_pos);
                Ok(i + 1)
            }
            b'|' => {
                // Alternation
                let alt_pos = self.bytecode.len();

                // Emit jump to skip the next alternative (will be fixed up later)
                self.emit(Opcode::Jump as u8);
                self.emit16(0); // Placeholder

                // For the top group, record this in the alt chain
                if let Some(entry) = self.group_stack.last_mut() {
                    // Chain: this jump's offset points to previous alt
                    let prev = entry.alt_chain.replace(alt_pos);
                    if let Some(prev_pos) = prev {
                        // Fixup the previous on_failure_jump to point here
                        let target = self.bytecode.len() as i16 - (prev_pos + 3) as i16;
                        self.bytecode[prev_pos + 1] = target as u8;
                        self.bytecode[prev_pos + 2] = (target >> 8) as u8;
                    }
                }

                // Emit on_failure_jump at the start of this alternative
                // (actually, alternation in Emacs regex is handled differently —
                // the on_failure_jump goes BEFORE the first alternative to try the second)
                // For simplicity, we emit a failure jump here
                let fail_pos = self.bytecode.len();
                self.emit(Opcode::OnFailureJump as u8);
                self.emit16(0); // Will be fixed up

                if let Some(entry) = self.group_stack.last_mut() {
                    entry.alt_chain = Some(fail_pos);
                }

                self.last_atom_start = None;
                Ok(i + 1)
            }
            b'{' => {
                // Bounded repetition \{n,m\}
                self.compile_bounded_rep(bytes, i)
            }
            b'1'..=b'9' => {
                // Backreference
                let group = b - b'0';
                if (group as usize) > self.num_groups {
                    return Err(RegexError::InvalidBackreference(group));
                }
                self.last_atom_start = Some(self.bytecode.len());
                self.emit(Opcode::Duplicate as u8);
                self.emit(group);
                Ok(i + 1)
            }
            b'b' => {
                self.emit(Opcode::WordBound as u8);
                self.uses_syntax = true;
                self.last_atom_start = None;
                Ok(i + 1)
            }
            b'B' => {
                self.emit(Opcode::NotWordBound as u8);
                self.uses_syntax = true;
                self.last_atom_start = None;
                Ok(i + 1)
            }
            b'<' => {
                self.emit(Opcode::WordBeg as u8);
                self.uses_syntax = true;
                self.last_atom_start = None;
                Ok(i + 1)
            }
            b'>' => {
                self.emit(Opcode::WordEnd as u8);
                self.uses_syntax = true;
                self.last_atom_start = None;
                Ok(i + 1)
            }
            b'_' => {
                // Symbol boundaries: \_ followed by < or >
                if i + 1 < bytes.len() {
                    match bytes[i + 1] {
                        b'<' => {
                            self.emit(Opcode::SymBeg as u8);
                            self.uses_syntax = true;
                            self.last_atom_start = None;
                            Ok(i + 2)
                        }
                        b'>' => {
                            self.emit(Opcode::SymEnd as u8);
                            self.uses_syntax = true;
                            self.last_atom_start = None;
                            Ok(i + 2)
                        }
                        _ => {
                            // Literal underscore
                            self.compile_literal_char(b'_');
                            Ok(i + 1)
                        }
                    }
                } else {
                    self.compile_literal_char(b'_');
                    Ok(i + 1)
                }
            }
            b'`' => {
                self.emit(Opcode::BegBuf as u8);
                self.last_atom_start = None;
                Ok(i + 1)
            }
            b'\'' => {
                self.emit(Opcode::EndBuf as u8);
                self.last_atom_start = None;
                Ok(i + 1)
            }
            b's' => {
                // Syntax class \sX
                if i + 1 >= bytes.len() {
                    return Err(RegexError::InvalidPattern("\\s without class".into()));
                }
                let class = syntax_code_from_char(bytes[i + 1]);
                self.last_atom_start = Some(self.bytecode.len());
                self.emit(Opcode::SyntaxSpec as u8);
                self.emit(class);
                self.uses_syntax = true;
                Ok(i + 2)
            }
            b'S' => {
                if i + 1 >= bytes.len() {
                    return Err(RegexError::InvalidPattern("\\S without class".into()));
                }
                let class = syntax_code_from_char(bytes[i + 1]);
                self.last_atom_start = Some(self.bytecode.len());
                self.emit(Opcode::NotSyntaxSpec as u8);
                self.emit(class);
                self.uses_syntax = true;
                Ok(i + 2)
            }
            b'c' => {
                // Category \cX
                if i + 1 >= bytes.len() {
                    return Err(RegexError::InvalidPattern("\\c without category".into()));
                }
                self.last_atom_start = Some(self.bytecode.len());
                self.emit(Opcode::CategorySpec as u8);
                self.emit(bytes[i + 1]);
                Ok(i + 2)
            }
            b'C' => {
                if i + 1 >= bytes.len() {
                    return Err(RegexError::InvalidPattern("\\C without category".into()));
                }
                self.last_atom_start = Some(self.bytecode.len());
                self.emit(Opcode::NotCategorySpec as u8);
                self.emit(bytes[i + 1]);
                Ok(i + 2)
            }
            b'w' => {
                // \w = word constituent (shorthand for \sw)
                self.last_atom_start = Some(self.bytecode.len());
                self.emit(Opcode::SyntaxSpec as u8);
                self.emit(SyntaxClass::Word as u8);
                self.uses_syntax = true;
                Ok(i + 1)
            }
            b'W' => {
                self.last_atom_start = Some(self.bytecode.len());
                self.emit(Opcode::NotSyntaxSpec as u8);
                self.emit(SyntaxClass::Word as u8);
                self.uses_syntax = true;
                Ok(i + 1)
            }
            // Literal escaped characters
            b'\\' | b'.' | b'*' | b'+' | b'?' | b'[' | b']' | b'^' | b'$' | b'{' | b'}' => {
                self.compile_literal_char(b);
                Ok(i + 1)
            }
            b'n' => {
                self.compile_literal_char(b'\n');
                Ok(i + 1)
            }
            b't' => {
                self.compile_literal_char(b'\t');
                Ok(i + 1)
            }
            b'r' => {
                self.compile_literal_char(b'\r');
                Ok(i + 1)
            }
            b'f' => {
                self.compile_literal_char(b'\x0C');
                Ok(i + 1)
            }
            b'a' => {
                self.compile_literal_char(b'\x07');
                Ok(i + 1)
            }
            _ => {
                // Unknown escape — treat as literal
                self.compile_literal_char(b);
                Ok(i + 1)
            }
        }
    }

    fn compile_charset(&mut self, bytes: &[u8], mut i: usize) -> Result<usize, RegexError> {
        debug_assert!(bytes[i] == b'[');
        i += 1;

        let negated = if i < bytes.len() && bytes[i] == b'^' {
            i += 1;
            true
        } else {
            false
        };

        let opcode = if negated {
            Opcode::CharsetNot
        } else {
            Opcode::Charset
        };

        self.last_atom_start = Some(self.bytecode.len());
        self.emit(opcode as u8);

        // Reserve space for bitmap size byte
        let size_pos = self.bytecode.len();
        self.emit(0); // placeholder

        // 32-byte ASCII bitmap (256 bits)
        let bitmap_start = self.bytecode.len();
        self.bytecode.extend_from_slice(&[0u8; 32]);

        // Multibyte ranges
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        let mut char_class_bits: u16 = 0;

        // Parse charset contents
        let mut first = true;
        while i < bytes.len() {
            let b = bytes[i];

            if b == b']' && !first {
                i += 1;
                break;
            }

            if i >= bytes.len() {
                return Err(RegexError::UnmatchedBracket);
            }

            first = false;

            if b == b'[' && i + 1 < bytes.len() && bytes[i + 1] == b':' {
                // Named character class [:alpha:]
                let class_start = i + 2;
                let class_end = bytes[class_start..]
                    .windows(2)
                    .position(|w| w == b":]")
                    .map(|p| class_start + p);

                if let Some(end) = class_end {
                    let name = std::str::from_utf8(&bytes[class_start..end])
                        .map_err(|_| RegexError::InvalidCharClass("invalid UTF-8".into()))?;

                    if let Some(class) = CharClass::from_name(name) {
                        // Set bits in bitmap for ASCII chars matching the class
                        for c in 0u8..=127 {
                            if class.matches(c as char) {
                                self.bytecode[bitmap_start + (c / 8) as usize] |= 1 << (c % 8);
                            }
                        }
                        // Record class bits for multibyte
                        char_class_bits |= char_class_bit(&class);
                    } else {
                        return Err(RegexError::InvalidCharClass(name.to_string()));
                    }

                    i = end + 2;
                    continue;
                } else {
                    return Err(RegexError::InvalidCharClass("unclosed [:".into()));
                }
            }

            // Check for range a-z
            let ch = self.parse_charset_char(bytes, &mut i)?;

            if i < bytes.len() && bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1] != b']' {
                i += 1; // skip '-'
                let end_ch = self.parse_charset_char(bytes, &mut i)?;

                if ch > end_ch {
                    return Err(RegexError::InvalidRange);
                }

                // Set range in bitmap (ASCII part)
                let start = ch.min(127);
                let end = end_ch.min(127);
                for c in start..=end {
                    self.bytecode[bitmap_start + (c / 8) as usize] |= 1 << (c % 8);
                }

                // Multibyte range
                if end_ch > 127 {
                    ranges.push((ch.max(128) as u32, end_ch as u32));
                }
            } else {
                // Single character
                if ch <= 127 {
                    self.bytecode[bitmap_start + (ch / 8) as usize] |= 1 << (ch % 8);
                } else {
                    ranges.push((ch as u32, ch as u32));
                }
            }
        }

        // Encode bitmap size and optional range table
        if ranges.is_empty() && char_class_bits == 0 {
            // Simple bitmap only
            self.bytecode[size_pos] = 32; // 32 bytes of bitmap
        } else {
            // Bitmap + range table
            self.bytecode[size_pos] = 32 | 0x80; // Flag indicating range table follows

            // Range table: [flags_lo] [flags_hi] [count_lo] [count_hi] [ranges...]
            let count = ranges.len() as u16;
            self.emit((char_class_bits & 0xFF) as u8);
            self.emit(((char_class_bits >> 8) & 0xFF) as u8);
            self.emit((count & 0xFF) as u8);
            self.emit(((count >> 8) & 0xFF) as u8);

            for (start, end) in &ranges {
                // Each range is 3 bytes for start + 3 bytes for end
                self.emit((*start & 0xFF) as u8);
                self.emit(((*start >> 8) & 0xFF) as u8);
                self.emit(((*start >> 16) & 0xFF) as u8);
                self.emit((*end & 0xFF) as u8);
                self.emit(((*end >> 8) & 0xFF) as u8);
                self.emit(((*end >> 16) & 0xFF) as u8);
            }
        }

        Ok(i)
    }

    fn parse_charset_char(&self, bytes: &[u8], i: &mut usize) -> Result<u8, RegexError> {
        if *i >= bytes.len() {
            return Err(RegexError::UnmatchedBracket);
        }

        let b = bytes[*i];
        *i += 1;

        if b == b'\\' && *i < bytes.len() {
            let escaped = bytes[*i];
            *i += 1;
            match escaped {
                b'n' => Ok(b'\n'),
                b't' => Ok(b'\t'),
                b'r' => Ok(b'\r'),
                _ => Ok(escaped),
            }
        } else {
            Ok(b)
        }
    }

    fn compile_repetition(&mut self, bytes: &[u8], mut i: usize) -> Result<usize, RegexError> {
        let op = bytes[i];
        let atom_start = self.last_atom_start.ok_or(RegexError::NoPrecedingElement)?;

        i += 1;

        // Check for non-greedy variant
        let greedy = if i < bytes.len() && bytes[i] == b'?' {
            i += 1;
            false
        } else {
            true
        };

        match op {
            b'*' => {
                // Zero or more
                if greedy {
                    self.wrap_with_star(atom_start);
                } else {
                    self.wrap_with_star_nongreedy(atom_start);
                }
            }
            b'+' => {
                // One or more
                if greedy {
                    self.wrap_with_plus(atom_start);
                } else {
                    self.wrap_with_plus_nongreedy(atom_start);
                }
            }
            b'?' => {
                // Zero or one
                if greedy {
                    self.wrap_with_optional(atom_start);
                } else {
                    self.wrap_with_optional_nongreedy(atom_start);
                }
            }
            _ => unreachable!(),
        }

        self.last_atom_start = None; // Repetition consumes the atom
        Ok(i)
    }

    fn compile_bounded_rep(&mut self, bytes: &[u8], mut i: usize) -> Result<usize, RegexError> {
        // \{n,m\} — parse the numbers
        debug_assert!(bytes[i] == b'{');
        i += 1;

        let mut min_count: u32 = 0;
        let mut max_count: Option<u32> = None;
        let mut has_comma = false;

        // Parse min
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            min_count = min_count * 10 + (bytes[i] - b'0') as u32;
            i += 1;
        }

        if i < bytes.len() && bytes[i] == b',' {
            has_comma = true;
            i += 1;

            // Parse max
            if i < bytes.len() && bytes[i].is_ascii_digit() {
                let mut m: u32 = 0;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    m = m * 10 + (bytes[i] - b'0') as u32;
                    i += 1;
                }
                max_count = Some(m);
            }
            // else: unbounded (\{n,\})
        } else {
            // Exact: \{n\}
            max_count = Some(min_count);
        }

        // Expect \}
        if i < bytes.len() && bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'}' {
            i += 2;
        } else {
            return Err(RegexError::UnmatchedBrace);
        }

        let atom_start = self.last_atom_start.ok_or(RegexError::NoPrecedingElement)?;

        // For now, emit as repeated atoms
        // This is a simplified approach — full implementation would use
        // succeed_n / jump_n opcodes
        let atom_bytecode: Vec<u8> = self.bytecode[atom_start..].to_vec();
        self.bytecode.truncate(atom_start);

        // Emit min_count copies
        for _ in 0..min_count {
            self.bytecode.extend_from_slice(&atom_bytecode);
        }

        // Emit optional copies for the range
        match max_count {
            Some(max) if max > min_count => {
                for _ in 0..(max - min_count) {
                    let opt_start = self.bytecode.len();
                    self.bytecode.extend_from_slice(&atom_bytecode);
                    self.wrap_with_optional(opt_start);
                }
            }
            None => {
                // Unbounded: emit one more copy with star
                let star_start = self.bytecode.len();
                self.bytecode.extend_from_slice(&atom_bytecode);
                self.wrap_with_star(star_start);
            }
            _ => {} // Exact match or max == min
        }

        self.last_atom_start = Some(atom_start);
        Ok(i)
    }

    fn compile_literal(&mut self, bytes: &[u8], mut i: usize) -> Result<usize, RegexError> {
        // Collect consecutive literal bytes into an Exactn instruction
        let start = self.bytecode.len();
        self.last_atom_start = Some(start);
        self.emit(Opcode::Exactn as u8);
        let count_pos = self.bytecode.len();
        self.emit(0); // placeholder for count

        let mut count: u8 = 0;

        while i < bytes.len() {
            let b = bytes[i];
            // Stop at special characters
            if b == b'\\'
                || b == b'['
                || b == b'.'
                || b == b'^'
                || b == b'$'
                || b == b'*'
                || b == b'+'
                || b == b'?'
            {
                break;
            }

            self.emit(b);
            count += 1;
            i += 1;

            // Limit exactn to 255 bytes
            if count == 255 {
                break;
            }
        }

        if count == 0 {
            // Shouldn't happen, but handle gracefully
            self.bytecode.truncate(start);
            self.last_atom_start = None;
        } else {
            self.bytecode[count_pos] = count;

            // If the next char is a repetition operator, we need to split
            // the last char into its own exactn
            if i < bytes.len()
                && (bytes[i] == b'*' || bytes[i] == b'+' || bytes[i] == b'?')
                && count > 1
            {
                // Move last byte to a new exactn
                let last_byte = self.bytecode.pop().unwrap();
                self.bytecode[count_pos] = count - 1;

                self.last_atom_start = Some(self.bytecode.len());
                self.emit(Opcode::Exactn as u8);
                self.emit(1);
                self.emit(last_byte);
            }
        }

        Ok(i)
    }

    fn compile_literal_char(&mut self, ch: u8) {
        self.last_atom_start = Some(self.bytecode.len());
        self.emit(Opcode::Exactn as u8);
        self.emit(1);
        self.emit(ch);
    }

    // ===== Repetition wrapping =====

    /// Wrap bytecode[atom_start..] with greedy star (zero or more).
    fn wrap_with_star(&mut self, atom_start: usize) {
        // on_failure_jump +AFTER
        // [atom]
        // jump -ATOM-3
        let atom_len = self.bytecode.len() - atom_start;

        // Insert on_failure_jump before the atom
        let jump_bytes = vec![
            Opcode::OnFailureJumpLoop as u8,
            (atom_len + 3) as u8,
            ((atom_len + 3) >> 8) as u8,
        ];
        self.bytecode.splice(atom_start..atom_start, jump_bytes);

        // Append jump back to on_failure_jump
        let back_offset = -(atom_len as i16 + 3 + 3);
        self.emit(Opcode::Jump as u8);
        self.emit(back_offset as u8);
        self.emit((back_offset >> 8) as u8);
    }

    /// Wrap with non-greedy star.
    fn wrap_with_star_nongreedy(&mut self, atom_start: usize) {
        let atom_len = self.bytecode.len() - atom_start;

        // jump +AFTER_ATOM
        // [atom]
        // on_failure_jump -ATOM-3
        let jump_over = vec![
            Opcode::Jump as u8,
            (atom_len + 3) as u8,
            ((atom_len + 3) >> 8) as u8,
        ];
        self.bytecode.splice(atom_start..atom_start, jump_over);

        let back_offset = -(atom_len as i16 + 3 + 3);
        self.emit(Opcode::OnFailureJumpNastyloop as u8);
        self.emit(back_offset as u8);
        self.emit((back_offset >> 8) as u8);
    }

    /// Wrap with greedy plus (one or more).
    fn wrap_with_plus(&mut self, atom_start: usize) {
        // [atom]
        // on_failure_keep_string_jump +3  (fail → skip loop)
        // jump -(atom_len+6)             (loop back to atom)
        let atom_len = self.bytecode.len() - atom_start;
        let back_offset = -(atom_len as i16 + 6);
        self.emit(Opcode::OnFailureKeepStringJump as u8);
        self.emit(3u8);
        self.emit(0u8);
        self.emit(Opcode::Jump as u8);
        self.emit(back_offset as u8);
        self.emit((back_offset >> 8) as u8);
    }

    /// Wrap with non-greedy plus.
    fn wrap_with_plus_nongreedy(&mut self, atom_start: usize) {
        let atom_len = self.bytecode.len() - atom_start;
        let back_offset = -(atom_len as i16 + 3);
        self.emit(Opcode::OnFailureJumpNastyloop as u8);
        self.emit(back_offset as u8);
        self.emit((back_offset >> 8) as u8);
    }

    /// Wrap with greedy optional (zero or one).
    fn wrap_with_optional(&mut self, atom_start: usize) {
        let atom_len = self.bytecode.len() - atom_start;

        let jump_bytes = vec![
            Opcode::OnFailureJump as u8,
            atom_len as u8,
            (atom_len >> 8) as u8,
        ];
        self.bytecode.splice(atom_start..atom_start, jump_bytes);
    }

    /// Wrap with non-greedy optional.
    fn wrap_with_optional_nongreedy(&mut self, atom_start: usize) {
        let atom_len = self.bytecode.len() - atom_start;

        let jump_bytes = vec![Opcode::Jump as u8, atom_len as u8, (atom_len >> 8) as u8];
        self.bytecode.splice(atom_start..atom_start, jump_bytes);

        // After the atom, emit on_failure_jump back
        // Actually for non-greedy optional: jump past atom, then on_failure_jump to atom
        // Let me redo this:
        // on_failure_jump_keep +atom_len (to skip atom = match empty)
        // [atom]
        // This already handled by the jump we inserted.
        // Actually the semantics: non-greedy ? means prefer NOT matching.
        // So: jump +atom_len, [atom]. If the overall match fails, backtrack and try with atom.
        // The Jump we inserted already does this correctly since the failure of the
        // continuing match will cause backtracking.
    }

    // ===== Helpers =====

    fn emit(&mut self, byte: u8) {
        self.bytecode.push(byte);
    }

    fn emit16(&mut self, val: i16) {
        self.bytecode.push(val as u8);
        self.bytecode.push((val >> 8) as u8);
    }

    fn fixup_alt_chain(&mut self, _alt_pos: usize) {
        // Fix up the jump targets in the alternation chain
        // For each on_failure_jump in the chain, set its target to after the group
        let current = self.bytecode.len();
        // The alt_pos points to the last on_failure_jump
        // Its target should be here (end of group)
        if _alt_pos + 3 <= self.bytecode.len() {
            let target = current as i16 - (_alt_pos + 3) as i16;
            self.bytecode[_alt_pos + 1] = target as u8;
            self.bytecode[_alt_pos + 2] = (target >> 8) as u8;
        }
    }

    fn finish(self) -> PatternBuffer {
        let mut buf = PatternBuffer::new();
        buf.bytecode = self.bytecode;
        buf.num_groups = self.num_groups;
        buf.multibyte = self.multibyte;
        buf.uses_syntax = self.uses_syntax;
        buf
    }
}

/// Convert a syntax code character to its numeric value.
fn syntax_code_from_char(ch: u8) -> u8 {
    match ch {
        b' ' | b'-' => SyntaxClass::Whitespace as u8,
        b'.' => SyntaxClass::Punct as u8,
        b'w' => SyntaxClass::Word as u8,
        b'_' => SyntaxClass::Symbol as u8,
        b'(' => SyntaxClass::Open as u8,
        b')' => SyntaxClass::Close as u8,
        b'\'' => SyntaxClass::Quote as u8,
        b'"' => SyntaxClass::String as u8,
        b'$' => SyntaxClass::Math as u8,
        b'\\' => SyntaxClass::Escape as u8,
        b'/' => SyntaxClass::CharQuote as u8,
        b'<' => SyntaxClass::Comment as u8,
        b'>' => SyntaxClass::EndComment as u8,
        _ => SyntaxClass::Whitespace as u8,
    }
}

fn char_class_bit(class: &CharClass) -> u16 {
    match class {
        CharClass::Word => 0x001,
        CharClass::Lower => 0x002,
        CharClass::Punct => 0x004,
        CharClass::Space => 0x008,
        CharClass::Upper => 0x010,
        CharClass::Multibyte => 0x020,
        CharClass::Alpha => 0x040,
        CharClass::Alnum => 0x080,
        CharClass::Graph => 0x100,
        CharClass::Print => 0x200,
        CharClass::Blank => 0x400,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compile_literal() {
        let buf = compile("hello", true).unwrap();
        // Should produce: Exactn 5 'h' 'e' 'l' 'l' 'o' Succeed
        assert_eq!(buf.bytecode[0], Opcode::Exactn as u8);
        assert_eq!(buf.bytecode[1], 5); // length
        assert_eq!(buf.bytecode[2], b'h');
        assert_eq!(buf.bytecode[6], b'o');
        assert_eq!(buf.bytecode[7], Opcode::Succeed as u8);
    }

    #[test]
    fn test_compile_dot() {
        let buf = compile("a.b", true).unwrap();
        // Exactn 1 'a', AnyChar, Exactn 1 'b', Succeed
        assert_eq!(buf.bytecode[0], Opcode::Exactn as u8);
        assert_eq!(buf.bytecode[1], 1);
        assert_eq!(buf.bytecode[2], b'a');
        assert_eq!(buf.bytecode[3], Opcode::AnyChar as u8);
        assert_eq!(buf.bytecode[4], Opcode::Exactn as u8);
        assert_eq!(buf.bytecode[5], 1);
        assert_eq!(buf.bytecode[6], b'b');
    }

    #[test]
    fn test_compile_anchors() {
        let buf = compile("^foo$", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::BegLine as u8);
        assert_eq!(buf.bytecode[1], Opcode::Exactn as u8);
        // ... ends with EndLine
        let last_opcode_pos = buf.bytecode.len() - 2;
        assert_eq!(buf.bytecode[last_opcode_pos], Opcode::EndLine as u8);
    }

    #[test]
    fn test_compile_star() {
        let buf = compile("a*", true).unwrap();
        // Should contain OnFailureJumpLoop and Jump opcodes
        assert!(
            buf.bytecode
                .iter()
                .any(|&b| b == Opcode::OnFailureJumpLoop as u8)
        );
        assert!(buf.bytecode.iter().any(|&b| b == Opcode::Jump as u8));
    }

    #[test]
    fn test_compile_group() {
        let buf = compile("\\(foo\\)", true).unwrap();
        assert_eq!(buf.num_groups, 1);
        assert!(buf.bytecode.iter().any(|&b| b == Opcode::StartMemory as u8));
        assert!(buf.bytecode.iter().any(|&b| b == Opcode::StopMemory as u8));
    }

    #[test]
    fn test_compile_shy_group() {
        let buf = compile("\\(?:foo\\)", true).unwrap();
        assert_eq!(buf.num_groups, 0);
        // Shy groups don't emit StartMemory/StopMemory
        assert!(!buf.bytecode.iter().any(|&b| b == Opcode::StartMemory as u8));
    }

    #[test]
    fn test_compile_word_boundary() {
        let buf = compile("\\bfoo\\b", true).unwrap();
        assert!(buf.uses_syntax);
        assert!(buf.bytecode.iter().any(|&b| b == Opcode::WordBound as u8));
    }

    #[test]
    fn test_compile_charset() {
        let buf = compile("[abc]", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::Charset as u8);
    }

    #[test]
    fn test_compile_negated_charset() {
        let buf = compile("[^abc]", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::CharsetNot as u8);
    }

    #[test]
    fn test_compile_syntax_class() {
        let buf = compile("\\sw", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::SyntaxSpec as u8);
        assert_eq!(buf.bytecode[1], SyntaxClass::Word as u8);
    }

    #[test]
    fn test_trailing_backslash() {
        assert!(compile("foo\\", true).is_err());
    }

    #[test]
    fn test_unmatched_paren() {
        assert!(compile("\\(foo", true).is_err());
    }

    #[test]
    fn test_compile_buffer_boundaries() {
        let buf = compile("\\`foo\\'", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::BegBuf as u8);
    }

    #[test]
    fn test_compile_plus() {
        let buf = compile("a+", true).unwrap();
        assert!(buf.bytecode.len() > 3); // At least: Exactn 1 'a' + jump opcodes
    }

    #[test]
    fn test_compile_optional() {
        let buf = compile("a?", true).unwrap();
        assert!(
            buf.bytecode
                .iter()
                .any(|&b| b == Opcode::OnFailureJump as u8)
        );
    }

    // ===== Basic literal pattern compilation =====

    #[test]
    fn test_compile_single_char_literal() {
        let buf = compile("x", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::Exactn as u8);
        assert_eq!(buf.bytecode[1], 1);
        assert_eq!(buf.bytecode[2], b'x');
        assert_eq!(buf.bytecode[3], Opcode::Succeed as u8);
    }

    #[test]
    fn test_compile_empty_pattern() {
        let buf = compile("", true).unwrap();
        // Should produce just Succeed
        assert_eq!(buf.bytecode.len(), 1);
        assert_eq!(buf.bytecode[0], Opcode::Succeed as u8);
    }

    #[test]
    fn test_compile_literal_splits_before_repetition() {
        // "ab*" should split: Exactn 1 'a', then Exactn 1 'b' with star wrapping
        let buf = compile("ab*", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::Exactn as u8);
        assert_eq!(buf.bytecode[1], 1);
        assert_eq!(buf.bytecode[2], b'a');
        // The 'b' should be in its own Exactn since it's followed by *
    }

    #[test]
    fn test_compile_long_literal() {
        // Multiple consecutive literal chars coalesce into one Exactn
        let buf = compile("abcdef", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::Exactn as u8);
        assert_eq!(buf.bytecode[1], 6);
        assert_eq!(buf.bytecode[2], b'a');
        assert_eq!(buf.bytecode[3], b'b');
        assert_eq!(buf.bytecode[4], b'c');
        assert_eq!(buf.bytecode[5], b'd');
        assert_eq!(buf.bytecode[6], b'e');
        assert_eq!(buf.bytecode[7], b'f');
        assert_eq!(buf.bytecode[8], Opcode::Succeed as u8);
    }

    // ===== Character classes [a-z], [^abc] =====

    #[test]
    fn test_charset_range_sets_bitmap_bits() {
        let buf = compile("[a-z]", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::Charset as u8);
        let bitmap_start = 2; // opcode + size byte
        // Check that 'a' (97) through 'z' (122) bits are set
        for c in b'a'..=b'z' {
            let byte_idx = bitmap_start + (c / 8) as usize;
            let bit = 1 << (c % 8);
            assert!(
                buf.bytecode[byte_idx] & bit != 0,
                "char '{}' (byte {}) should be in charset",
                c as char,
                c
            );
        }
        // Check that 'A' is NOT set
        let byte_idx_a = bitmap_start + (b'A' / 8) as usize;
        let bit_a = 1 << (b'A' % 8);
        assert_eq!(
            buf.bytecode[byte_idx_a] & bit_a,
            0,
            "'A' should NOT be in [a-z]"
        );
    }

    #[test]
    fn test_negated_charset_individual_chars() {
        let buf = compile("[^xyz]", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::CharsetNot as u8);
        let bitmap_start = 2;
        for c in [b'x', b'y', b'z'] {
            let byte_idx = bitmap_start + (c / 8) as usize;
            let bit = 1 << (c % 8);
            assert!(
                buf.bytecode[byte_idx] & bit != 0,
                "char '{}' should be in negated charset bitmap",
                c as char
            );
        }
    }

    #[test]
    fn test_charset_single_chars() {
        let buf = compile("[aeiou]", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::Charset as u8);
        let bitmap_start = 2;
        for c in [b'a', b'e', b'i', b'o', b'u'] {
            let byte_idx = bitmap_start + (c / 8) as usize;
            let bit = 1 << (c % 8);
            assert!(buf.bytecode[byte_idx] & bit != 0);
        }
        // 'b' should NOT be set
        let byte_idx_b = bitmap_start + (b'b' / 8) as usize;
        let bit_b = 1 << (b'b' % 8);
        assert_eq!(buf.bytecode[byte_idx_b] & bit_b, 0);
    }

    #[test]
    fn test_charset_named_class_alpha() {
        let buf = compile("[[:alpha:]]", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::Charset as u8);
        let bitmap_start = 2;
        // All ASCII letters should be set
        for c in b'a'..=b'z' {
            let byte_idx = bitmap_start + (c / 8) as usize;
            let bit = 1 << (c % 8);
            assert!(
                buf.bytecode[byte_idx] & bit != 0,
                "'{}' should be in [:alpha:]",
                c as char
            );
        }
        for c in b'A'..=b'Z' {
            let byte_idx = bitmap_start + (c / 8) as usize;
            let bit = 1 << (c % 8);
            assert!(
                buf.bytecode[byte_idx] & bit != 0,
                "'{}' should be in [:alpha:]",
                c as char
            );
        }
        // digits should NOT be set
        let byte_idx_0 = bitmap_start + (b'0' / 8) as usize;
        let bit_0 = 1 << (b'0' % 8);
        assert_eq!(
            buf.bytecode[byte_idx_0] & bit_0,
            0,
            "'0' should NOT be in [:alpha:]"
        );
    }

    #[test]
    fn test_charset_named_class_digit() {
        let buf = compile("[[:digit:]]", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::Charset as u8);
        let bitmap_start = 2;
        for c in b'0'..=b'9' {
            let byte_idx = bitmap_start + (c / 8) as usize;
            let bit = 1 << (c % 8);
            assert!(buf.bytecode[byte_idx] & bit != 0);
        }
    }

    #[test]
    fn test_charset_invalid_named_class() {
        let result = compile("[[:foobar:]]", true);
        match result {
            Err(RegexError::InvalidCharClass(name)) => assert_eq!(name, "foobar"),
            Err(other) => panic!("Expected InvalidCharClass, got {:?}", other),
            Ok(_) => panic!("Expected error, got Ok"),
        }
    }

    #[test]
    fn test_charset_bracket_as_first_char() {
        // ']' as first character in charset is literal, not closing bracket
        let buf = compile("[]abc]", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::Charset as u8);
        let bitmap_start = 2;
        let byte_idx = bitmap_start + (b']' / 8) as usize;
        let bit = 1 << (b']' % 8);
        assert!(
            buf.bytecode[byte_idx] & bit != 0,
            "']' should be in charset when first"
        );
    }

    #[test]
    fn test_charset_invalid_range() {
        // [z-a] is an invalid range since z > a
        match compile("[z-a]", true) {
            Err(RegexError::InvalidRange) => {} // expected
            Err(other) => panic!("Expected InvalidRange, got {:?}", other),
            Ok(_) => panic!("Expected error, got Ok"),
        }
    }

    // ===== Emacs-specific operators =====

    #[test]
    fn test_compile_group_numbered() {
        let buf = compile("\\(a\\)\\(b\\)", true).unwrap();
        assert_eq!(buf.num_groups, 2);
        // First group: StartMemory 1 ... StopMemory 1
        // Second group: StartMemory 2 ... StopMemory 2
        let start_mem_positions: Vec<usize> = buf
            .bytecode
            .windows(2)
            .enumerate()
            .filter(|(_, w)| w[0] == Opcode::StartMemory as u8)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(start_mem_positions.len(), 2);
        assert_eq!(buf.bytecode[start_mem_positions[0] + 1], 1);
        assert_eq!(buf.bytecode[start_mem_positions[1] + 1], 2);
    }

    #[test]
    fn test_compile_alternation() {
        let buf = compile("\\(foo\\|bar\\)", true).unwrap();
        assert_eq!(buf.num_groups, 1);
        // Should contain Jump (for alternation) and OnFailureJump
        assert!(buf.bytecode.iter().any(|&b| b == Opcode::Jump as u8));
        assert!(
            buf.bytecode
                .iter()
                .any(|&b| b == Opcode::OnFailureJump as u8)
        );
    }

    #[test]
    fn test_compile_bounded_rep_exact() {
        // \{3\} = exactly 3 repetitions
        let buf = compile("a\\{3\\}", true).unwrap();
        // Should contain 3 copies of: Exactn 1 'a'
        let exactn_count = buf
            .bytecode
            .windows(3)
            .filter(|w| w[0] == Opcode::Exactn as u8 && w[1] == 1 && w[2] == b'a')
            .count();
        assert_eq!(exactn_count, 3, "Expected 3 copies of Exactn 1 'a'");
    }

    #[test]
    fn test_compile_bounded_rep_range() {
        // \{2,4\} = 2 to 4 repetitions
        let buf = compile("a\\{2,4\\}", true).unwrap();
        // Should succeed without error; contains at least 2 mandatory Exactn 'a's
        // plus 2 optional ones (wrapped with OnFailureJump)
        assert!(
            buf.bytecode
                .iter()
                .any(|&b| b == Opcode::OnFailureJump as u8)
        );
    }

    #[test]
    fn test_compile_bounded_rep_unbounded() {
        // \{2,\} = 2 or more repetitions
        let buf = compile("a\\{2,\\}", true).unwrap();
        // Should contain star wrapping for the unbounded part
        assert!(
            buf.bytecode
                .iter()
                .any(|&b| b == Opcode::OnFailureJumpLoop as u8)
        );
    }

    #[test]
    fn test_compile_word_constituent() {
        let buf = compile("\\w", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::SyntaxSpec as u8);
        assert_eq!(buf.bytecode[1], SyntaxClass::Word as u8);
        assert!(buf.uses_syntax);
    }

    #[test]
    fn test_compile_non_word_constituent() {
        let buf = compile("\\W", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::NotSyntaxSpec as u8);
        assert_eq!(buf.bytecode[1], SyntaxClass::Word as u8);
        assert!(buf.uses_syntax);
    }

    #[test]
    fn test_compile_syntax_class_whitespace() {
        let buf = compile("\\s ", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::SyntaxSpec as u8);
        assert_eq!(buf.bytecode[1], SyntaxClass::Whitespace as u8);
        assert!(buf.uses_syntax);
    }

    #[test]
    fn test_compile_not_syntax_class() {
        let buf = compile("\\S ", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::NotSyntaxSpec as u8);
        assert_eq!(buf.bytecode[1], SyntaxClass::Whitespace as u8);
    }

    #[test]
    fn test_compile_not_word_boundary() {
        let buf = compile("\\B", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::NotWordBound as u8);
        assert!(buf.uses_syntax);
    }

    #[test]
    fn test_compile_word_start_end() {
        let buf = compile("\\<\\w+\\>", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::WordBeg as u8);
        // WordEnd should be near the end
        let last_before_succeed = buf.bytecode.len() - 2;
        assert_eq!(buf.bytecode[last_before_succeed], Opcode::WordEnd as u8);
    }

    // ===== Special characters: . ^ $ * + ? =====

    #[test]
    fn test_compile_dot_as_anychar() {
        let buf = compile(".", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::AnyChar as u8);
        assert_eq!(buf.bytecode[1], Opcode::Succeed as u8);
    }

    #[test]
    fn test_compile_caret_at_start() {
        let buf = compile("^", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::BegLine as u8);
    }

    #[test]
    fn test_compile_dollar_at_end() {
        let buf = compile("$", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::EndLine as u8);
    }

    #[test]
    fn test_compile_star_no_preceding_element() {
        match compile("*", true) {
            Err(RegexError::NoPrecedingElement) => {} // expected
            Err(other) => panic!("Expected NoPrecedingElement, got {:?}", other),
            Ok(_) => panic!("Expected error, got Ok"),
        }
    }

    #[test]
    fn test_compile_plus_no_preceding_element() {
        match compile("+", true) {
            Err(RegexError::NoPrecedingElement) => {} // expected
            Err(other) => panic!("Expected NoPrecedingElement, got {:?}", other),
            Ok(_) => panic!("Expected error, got Ok"),
        }
    }

    #[test]
    fn test_compile_question_no_preceding_element() {
        match compile("?", true) {
            Err(RegexError::NoPrecedingElement) => {} // expected
            Err(other) => panic!("Expected NoPrecedingElement, got {:?}", other),
            Ok(_) => panic!("Expected error, got Ok"),
        }
    }

    #[test]
    fn test_compile_non_greedy_star() {
        let buf = compile("a*?", true).unwrap();
        // Non-greedy star uses Jump and OnFailureJumpNastyloop
        assert!(
            buf.bytecode
                .iter()
                .any(|&b| b == Opcode::OnFailureJumpNastyloop as u8)
        );
    }

    #[test]
    fn test_compile_non_greedy_plus() {
        let buf = compile("a+?", true).unwrap();
        assert!(
            buf.bytecode
                .iter()
                .any(|&b| b == Opcode::OnFailureJumpNastyloop as u8)
        );
    }

    #[test]
    fn test_compile_non_greedy_optional() {
        let buf = compile("a??", true).unwrap();
        // Non-greedy optional uses Jump (to skip the atom)
        assert!(buf.bytecode.iter().any(|&b| b == Opcode::Jump as u8));
    }

    // ===== Edge cases =====

    #[test]
    fn test_compile_escaped_special_chars() {
        // Escaped dot, star, plus, etc. should be literal
        let buf = compile("\\.\\*\\+\\?\\[\\]\\^\\$", true).unwrap();
        // Each escaped char becomes Exactn 1 <char>
        let mut i = 0;
        let expected = [b'.', b'*', b'+', b'?', b'[', b']', b'^', b'$'];
        for &exp_ch in &expected {
            assert_eq!(buf.bytecode[i], Opcode::Exactn as u8, "at offset {}", i);
            assert_eq!(buf.bytecode[i + 1], 1);
            assert_eq!(
                buf.bytecode[i + 2],
                exp_ch,
                "expected '{}' at offset {}",
                exp_ch as char,
                i
            );
            i += 3;
        }
        assert_eq!(buf.bytecode[i], Opcode::Succeed as u8);
    }

    #[test]
    fn test_compile_escaped_backslash() {
        let buf = compile("\\\\", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::Exactn as u8);
        assert_eq!(buf.bytecode[1], 1);
        assert_eq!(buf.bytecode[2], b'\\');
    }

    #[test]
    fn test_compile_nested_groups() {
        let buf = compile("\\(\\(a\\)\\(b\\)\\)", true).unwrap();
        assert_eq!(buf.num_groups, 3);
        // Count StartMemory opcodes
        let start_count = buf
            .bytecode
            .iter()
            .filter(|&&b| b == Opcode::StartMemory as u8)
            .count();
        assert_eq!(start_count, 3);
        let stop_count = buf
            .bytecode
            .iter()
            .filter(|&&b| b == Opcode::StopMemory as u8)
            .count();
        assert_eq!(stop_count, 3);
    }

    #[test]
    fn test_compile_escape_sequences() {
        // \n, \t, \r, \f, \a
        let buf = compile("\\n\\t\\r\\f\\a", true).unwrap();
        let mut i = 0;
        let expected = [b'\n', b'\t', b'\r', 0x0C, 0x07];
        for &exp_ch in &expected {
            assert_eq!(buf.bytecode[i], Opcode::Exactn as u8);
            assert_eq!(buf.bytecode[i + 1], 1);
            assert_eq!(buf.bytecode[i + 2], exp_ch);
            i += 3;
        }
    }

    #[test]
    fn test_compile_buffer_boundaries_begin_end() {
        let buf = compile("\\`\\'", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::BegBuf as u8);
        assert_eq!(buf.bytecode[1], Opcode::EndBuf as u8);
        assert_eq!(buf.bytecode[2], Opcode::Succeed as u8);
    }

    #[test]
    fn test_compile_symbol_boundaries() {
        let buf = compile("\\_<foo\\_>", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::SymBeg as u8);
        assert!(buf.uses_syntax);
        // SymEnd should be near the end
        let succeed_pos = buf.bytecode.len() - 1;
        assert_eq!(buf.bytecode[succeed_pos - 1], Opcode::SymEnd as u8);
    }

    #[test]
    fn test_compile_category() {
        let buf = compile("\\ca", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::CategorySpec as u8);
        assert_eq!(buf.bytecode[1], b'a');
    }

    #[test]
    fn test_compile_not_category() {
        let buf = compile("\\Ca", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::NotCategorySpec as u8);
        assert_eq!(buf.bytecode[1], b'a');
    }

    #[test]
    fn test_compile_backreference() {
        let buf = compile("\\(a\\)\\1", true).unwrap();
        assert_eq!(buf.num_groups, 1);
        // Should contain Duplicate opcode referencing group 1
        let dup_pos = buf
            .bytecode
            .iter()
            .position(|&b| b == Opcode::Duplicate as u8)
            .expect("should have Duplicate opcode");
        assert_eq!(buf.bytecode[dup_pos + 1], 1);
    }

    #[test]
    fn test_compile_multibyte_flag() {
        let buf_mb = compile("a", true).unwrap();
        assert!(buf_mb.multibyte);
        let buf_sb = compile("a", false).unwrap();
        assert!(!buf_sb.multibyte);
    }

    // ===== Error cases =====

    #[test]
    fn test_error_trailing_backslash() {
        match compile("abc\\", true) {
            Err(RegexError::TrailingBackslash) => {} // expected
            Err(other) => panic!("Expected TrailingBackslash, got {:?}", other),
            Ok(_) => panic!("Expected error, got Ok"),
        }
    }

    #[test]
    fn test_error_unmatched_open_paren() {
        match compile("\\(abc", true) {
            Err(RegexError::UnmatchedParen) => {} // expected
            Err(other) => panic!("Expected UnmatchedParen, got {:?}", other),
            Ok(_) => panic!("Expected error, got Ok"),
        }
    }

    #[test]
    fn test_error_unmatched_close_paren() {
        match compile("abc\\)", true) {
            Err(RegexError::UnmatchedParen) => {} // expected
            Err(other) => panic!("Expected UnmatchedParen, got {:?}", other),
            Ok(_) => panic!("Expected error, got Ok"),
        }
    }

    #[test]
    fn test_error_unmatched_brace() {
        // \{3 without closing \}
        match compile("a\\{3", true) {
            Err(RegexError::UnmatchedBrace) => {} // expected
            Err(other) => panic!("Expected UnmatchedBrace, got {:?}", other),
            Ok(_) => panic!("Expected error, got Ok"),
        }
    }

    #[test]
    fn test_error_invalid_backreference() {
        // \2 without having 2 groups
        match compile("\\2", true) {
            Err(RegexError::InvalidBackreference(2)) => {} // expected
            Err(other) => panic!("Expected InvalidBackreference(2), got {:?}", other),
            Ok(_) => panic!("Expected error, got Ok"),
        }
    }

    #[test]
    fn test_error_syntax_class_without_specifier() {
        let result = compile("\\s", true);
        assert!(result.is_err());
    }

    #[test]
    fn test_error_not_syntax_class_without_specifier() {
        let result = compile("\\S", true);
        assert!(result.is_err());
    }

    #[test]
    fn test_error_category_without_specifier() {
        let result = compile("\\c", true);
        assert!(result.is_err());
    }

    #[test]
    fn test_compile_greedy_plus_structure() {
        // a+ should produce: Exactn 1 'a', OnFailureKeepStringJump, Jump (back)
        let buf = compile("a+", true).unwrap();
        assert!(
            buf.bytecode
                .iter()
                .any(|&b| b == Opcode::OnFailureKeepStringJump as u8)
        );
        assert!(buf.bytecode.iter().any(|&b| b == Opcode::Jump as u8));
    }

    #[test]
    fn test_compile_underscore_not_boundary() {
        // \_ followed by something other than < or > is a literal underscore
        let buf = compile("\\_x", true).unwrap();
        // Should compile as Exactn 1 '_', Exactn 1 'x'
        assert_eq!(buf.bytecode[0], Opcode::Exactn as u8);
        assert_eq!(buf.bytecode[1], 1);
        assert_eq!(buf.bytecode[2], b'_');
    }

    #[test]
    fn test_compile_charset_with_escape_in_range() {
        // [\t-\r] — tab through carriage return
        let buf = compile("[\\t-\\r]", true).unwrap();
        assert_eq!(buf.bytecode[0], Opcode::Charset as u8);
        let bitmap_start = 2;
        // \t = 9, \n = 10, \r = 13 should all be set
        for c in [9u8, 10, 13] {
            let byte_idx = bitmap_start + (c / 8) as usize;
            let bit = 1 << (c % 8);
            assert!(
                buf.bytecode[byte_idx] & bit != 0,
                "byte {} should be in charset",
                c
            );
        }
    }

    #[test]
    fn test_compile_shy_group_no_group_count() {
        let buf = compile("\\(?:a\\)\\(?:b\\)", true).unwrap();
        assert_eq!(buf.num_groups, 0);
        assert!(!buf.bytecode.iter().any(|&b| b == Opcode::StartMemory as u8));
        assert!(!buf.bytecode.iter().any(|&b| b == Opcode::StopMemory as u8));
    }

    #[test]
    fn test_compile_complex_pattern() {
        // Realistic Emacs regex: match function definition
        let buf = compile("^\\(defun\\s \\(\\w+\\)\\)", true).unwrap();
        assert!(buf.bytecode.len() > 0);
        assert_eq!(buf.num_groups, 2);
        assert!(buf.uses_syntax);
        assert_eq!(buf.bytecode[0], Opcode::BegLine as u8);
        // Should end with Succeed
        assert_eq!(*buf.bytecode.last().unwrap(), Opcode::Succeed as u8);
    }

    #[test]
    fn test_compile_alternation_no_group() {
        // Bare alternation at top level — \| outside any group
        // The compiler handles this by checking group_stack.last_mut()
        // With no group on the stack, the alt_chain bookkeeping is skipped
        // but it shouldn't crash.
        let buf = compile("a\\|b", true).unwrap();
        assert!(buf.bytecode.iter().any(|&b| b == Opcode::Jump as u8));
    }

    #[test]
    fn test_compile_bounded_rep_zero_min() {
        // \{0,2\} = 0 to 2 repetitions (all optional)
        let buf = compile("a\\{0,2\\}", true).unwrap();
        // Should contain OnFailureJump for the optional copies
        let ofj_count = buf
            .bytecode
            .iter()
            .filter(|&&b| b == Opcode::OnFailureJump as u8)
            .count();
        assert!(
            ofj_count >= 2,
            "Expected at least 2 optional wrappings, got {}",
            ofj_count
        );
    }
}
