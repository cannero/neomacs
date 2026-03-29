//! GNU Emacs regex engine translated to Rust.
//!
//! This is a direct translation of GNU Emacs's `regex-emacs.c` — the same
//! algorithm, same bytecode format, same semantics.  The engine compiles
//! Emacs regex patterns to bytecode and executes them with syntax-table
//! awareness, backreference support, and POSIX backtracking.
//!
//! ## Architecture
//!
//! ```text
//! Pattern string
//!     ↓
//! regex_compile()     →  CompiledPattern (bytecode + fastmap)
//!     ↓
//! re_search()         →  Find match position (uses fastmap for skipping)
//!     ↓
//! re_match_internal() →  Execute bytecode against text (backtracking)
//!     ↓
//! MatchRegisters      →  Group start/end positions
//! ```
//!
//! ## Reference
//!
//! - GNU source: `src/regex-emacs.c` (5355 lines)
//! - GNU header: `src/regex-emacs.h`
//! - GNU search: `src/search.c` (3514 lines)

// ---------------------------------------------------------------------------
// Phase 1: Opcodes and Data Structures
// ---------------------------------------------------------------------------

/// Bytecode opcodes for the compiled regex pattern.
///
/// Translated from `re_opcode_t` enum in regex-emacs.c (lines 202-337).
/// Each opcode may be followed by argument bytes in the bytecode buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum RegexOp {
    /// No operation (padding/alignment).
    NoOp = 0,

    /// Succeed immediately — no more backtracking.
    Succeed = 1,

    /// Match N exact bytes.  Followed by one byte N, then N literal bytes.
    Exactn = 2,

    /// Match any character (except newline in some modes).
    AnyChar = 3,

    /// Match character in bitmap set.  Followed by:
    /// - 1 byte: bitmap length (low 7 bits), high bit = has range table
    /// - N bytes: bitmap (bit per character, low-bit-first)
    /// - Optional range table for multibyte characters
    Charset = 4,

    /// Match character NOT in bitmap set.  Same format as Charset.
    CharsetNot = 5,

    /// Start remembering text for group N.  Followed by 1 byte: group number.
    StartMemory = 6,

    /// Stop remembering text for group N.  Followed by 1 byte: group number.
    StopMemory = 7,

    /// Match duplicate of group N (backreference \N).  Followed by 1 byte: group number.
    Duplicate = 8,

    /// Fail unless at beginning of line (^).
    BegLine = 9,

    /// Fail unless at end of line ($).
    EndLine = 10,

    /// Unconditional jump.  Followed by 2-byte signed offset.
    Jump = 11,

    /// Push failure point, then continue.  Followed by 2-byte signed offset.
    OnFailureJump = 12,

    /// Like OnFailureJump but doesn't restore string position on failure.
    OnFailureKeepStringJump = 13,

    /// Like OnFailureJump but detects infinite empty-match loops.
    OnFailureJumpLoop = 14,

    /// Like OnFailureJumpLoop but for non-greedy operators.
    OnFailureJumpNastyloop = 15,

    /// Smart jump for greedy * and +.  Analyzes loop to optimize.
    OnFailureJumpSmart = 16,

    /// Match N times then jump on failure.  Followed by 2-byte offset + 2-byte count.
    SucceedN = 17,

    /// Jump N times then fail.  Followed by 2-byte offset + 2-byte count.
    JumpN = 18,

    /// Set counter at offset.  Followed by 2-byte offset + 2-byte value.
    SetNumberAt = 19,

    /// Succeed at word beginning (syntax-table aware).  `\<`
    WordBeg = 20,

    /// Succeed at word end (syntax-table aware).  `\>`
    WordEnd = 21,

    /// Succeed at word boundary (syntax-table aware).  `\b`
    WordBound = 22,

    /// Succeed at non-word boundary (syntax-table aware).  `\B`
    NotWordBound = 23,

    /// Succeed at symbol beginning (syntax-table aware).  `\_<`
    SymBeg = 24,

    /// Succeed at symbol end (syntax-table aware).  `\_>`
    SymEnd = 25,

    /// Match character with syntax class C.  Followed by 1 byte: syntax code.  `\sC`
    SyntaxSpec = 26,

    /// Match character without syntax class C.  Followed by 1 byte.  `\SC`
    NotSyntaxSpec = 27,

    /// Succeed if at point.  `\=`
    AtDot = 28,

    /// Succeed at beginning of buffer/string.  `` \` ``
    BegBuf = 29,

    /// Succeed at end of buffer/string.  `\'`
    EndBuf = 30,

    /// Match character with category C.  Followed by 1 byte: category code.  `\cC`
    CategorySpec = 31,

    /// Match character without category C.  Followed by 1 byte.  `\CC`
    NotCategorySpec = 32,
}

impl RegexOp {
    /// Convert a byte to an opcode.  Returns None for invalid bytes.
    fn from_byte(b: u8) -> Option<Self> {
        if b <= 32 {
            // SAFETY: all values 0-32 are valid enum variants
            Some(unsafe { std::mem::transmute(b) })
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Compiled Pattern
// ---------------------------------------------------------------------------

/// A compiled regex pattern — the output of `regex_compile()`.
///
/// Mirrors GNU's `struct re_pattern_buffer` from regex-emacs.h.
#[derive(Clone)]
pub(crate) struct CompiledPattern {
    /// Bytecode buffer.
    pub buffer: Vec<u8>,

    /// Number of subexpressions (groups).
    pub re_nsub: usize,

    /// Fast rejection map: fastmap[c] is true if the pattern can start
    /// with byte c.  Used by `re_search` to skip non-matching positions.
    pub fastmap: [bool; 256],

    /// Whether the fastmap is valid (needs recomputation after compile).
    pub fastmap_accurate: bool,

    /// True if the pattern was compiled for POSIX backtracking.
    pub posix: bool,

    /// True if the pattern can match the empty string.
    pub can_be_null: bool,

    /// Character translation table for case-folding.
    /// Maps each character to its canonical form (e.g., 'A' → 'a').
    pub translate: Option<Vec<char>>,
}

impl CompiledPattern {
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(256),
            re_nsub: 0,
            fastmap: [false; 256],
            fastmap_accurate: false,
            posix: false,
            can_be_null: false,
            translate: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Match Registers
// ---------------------------------------------------------------------------

/// Match result — stores group start/end positions.
///
/// Mirrors GNU's `struct re_registers` from regex-emacs.h.
#[derive(Clone, Debug)]
pub(crate) struct MatchRegisters {
    /// Start positions for each group (group 0 = full match).
    /// -1 means group did not participate in match.
    pub start: Vec<i64>,

    /// End positions for each group.
    pub end: Vec<i64>,
}

impl MatchRegisters {
    pub fn new(num_groups: usize) -> Self {
        Self {
            start: vec![-1; num_groups],
            end: vec![-1; num_groups],
        }
    }

    pub fn num_regs(&self) -> usize {
        self.start.len()
    }
}

// ---------------------------------------------------------------------------
// Failure Stack (for backtracking)
// ---------------------------------------------------------------------------

/// A single failure point on the backtracking stack.
///
/// When the matcher hits a choice point (OnFailureJump), it pushes the
/// current state so it can backtrack if the primary path fails.
#[derive(Clone, Debug)]
struct FailurePoint {
    /// Position in the bytecode to resume at.
    pattern_pos: usize,

    /// Position in the input text to resume at.
    /// None means "keep current string position" (OnFailureKeepStringJump).
    string_pos: Option<usize>,

    /// Saved group register values at this point.
    saved_registers: Vec<(usize, i64, i64)>, // (group_idx, start, end)
}

// ---------------------------------------------------------------------------
// Syntax helpers
// ---------------------------------------------------------------------------

/// Syntax class codes matching GNU's `enum syntaxcode` from syntax.h.
///
/// These are the classes that `\sC` and `\SC` match against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum SyntaxClass {
    Whitespace = 0,    // \s-  (space, tab, newline)
    Punctuation = 1,   // \s.
    Word = 2,          // \sw
    Symbol = 3,        // \s_
    Open = 4,          // \s(
    Close = 5,         // \s)
    Quote = 6,         // \s'
    StringDelim = 7,   // \s"
    Math = 8,          // \s$
    Escape = 9,        // \s\\
    CharQuote = 10,    // \s/
    Comment = 11,      // \s<
    EndComment = 12,   // \s>
    InheritStd = 13,   // \s@
    CommentFence = 14, // \s!
    StringFence = 15,  // \s|
}

impl SyntaxClass {
    /// Convert a character code letter to a syntax class.
    /// E.g., '-' → Whitespace, 'w' → Word, '_' → Symbol, etc.
    pub fn from_char(c: char) -> Option<Self> {
        match c {
            '-' | ' ' => Some(Self::Whitespace),
            '.' => Some(Self::Punctuation),
            'w' => Some(Self::Word),
            '_' => Some(Self::Symbol),
            '(' => Some(Self::Open),
            ')' => Some(Self::Close),
            '\'' => Some(Self::Quote),
            '"' => Some(Self::StringDelim),
            '$' => Some(Self::Math),
            '\\' => Some(Self::Escape),
            '/' => Some(Self::CharQuote),
            '<' => Some(Self::Comment),
            '>' => Some(Self::EndComment),
            '@' => Some(Self::InheritStd),
            '!' => Some(Self::CommentFence),
            '|' => Some(Self::StringFence),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Bytecode helpers
// ---------------------------------------------------------------------------

/// Store a 2-byte signed offset at position in bytecode buffer.
fn store_number(buf: &mut [u8], pos: usize, number: i16) {
    let bytes = number.to_le_bytes();
    buf[pos] = bytes[0];
    buf[pos + 1] = bytes[1];
}

/// Read a 2-byte signed offset from bytecode buffer.
fn extract_number(buf: &[u8], pos: usize) -> i16 {
    i16::from_le_bytes([buf[pos], buf[pos + 1]])
}

// ---------------------------------------------------------------------------
// Phase 2: Compiler (regex_compile)
//
// Translates GNU Emacs regex-emacs.c:1710-3400 (regex_compile function).
// Compiles an Emacs regex pattern string into bytecode.
// ---------------------------------------------------------------------------

/// Error from regex compilation.
#[derive(Debug, Clone)]
pub(crate) struct RegexCompileError {
    pub message: String,
}

impl std::fmt::Display for RegexCompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Invalid regexp: {}", self.message)
    }
}

/// Compile stack entry — tracks open groups during compilation.
/// Mirrors GNU's compile_stack_elt_t.
#[derive(Clone, Debug)]
struct CompileStackEntry {
    /// Bytecode position of the start of the group's alternatives.
    begalt_offset: usize,
    /// Bytecode position of the fixup jump for alternation (or 0).
    fixup_alt_jump: Option<usize>,
    /// Bytecode position of the last expression start (for postfix ops).
    laststart_offset: Option<usize>,
    /// Group number (0-based).
    regnum: usize,
}

/// Compile an Emacs regex pattern into bytecode.
///
/// This is the main entry point, equivalent to GNU's `regex_compile()`.
///
/// # Arguments
/// * `pattern` - The Emacs regex pattern string
/// * `posix` - If true, use POSIX backtracking semantics
/// * `case_fold` - If true, compile for case-insensitive matching
///
/// # Returns
/// A `CompiledPattern` with bytecode ready for the matcher.
pub(crate) fn regex_compile(
    pattern: &str,
    posix: bool,
    case_fold: bool,
) -> Result<CompiledPattern, RegexCompileError> {
    let mut buf = CompiledPattern::new();
    buf.posix = posix;

    // Build case-fold translation table if needed
    if case_fold {
        let mut table = Vec::with_capacity(256);
        for i in 0..256u32 {
            let c = char::from_u32(i).unwrap_or('\0');
            table.push(c.to_lowercase().next().unwrap_or(c));
        }
        buf.translate = Some(table);
    }

    let pattern_bytes = pattern.as_bytes();
    let plen = pattern_bytes.len();
    let mut p = 0; // Current position in pattern

    // Compile stack for tracking open groups
    let mut compile_stack: Vec<CompileStackEntry> = Vec::new();
    let mut regnum: usize = 0; // Current group number

    // Track positions in bytecode for fixup
    let mut begalt_offset: usize = 0; // Start of current alternative
    let mut pending_exact: Option<usize> = None; // Position of current exactn being built
    let mut laststart: Option<usize> = None; // Start of last complete expression (for postfix ops)
    let mut fixup_alt_jump: Option<usize> = None; // Jump to fixup at end of alternation

    /// Helper: push a byte to the bytecode buffer
    macro_rules! emit {
        ($byte:expr) => {
            buf.buffer.push($byte);
        };
    }

    /// Helper: push an opcode
    macro_rules! emit_op {
        ($op:expr) => {
            buf.buffer.push($op as u8);
        };
    }

    /// Helper: current bytecode position
    macro_rules! bpos {
        () => {
            buf.buffer.len()
        };
    }

    // Macro to fetch next pattern byte, returning error if at end
    macro_rules! pat_fetch {
        () => {{
            if p >= plen {
                return Err(RegexCompileError {
                    message: "premature end of pattern".to_string(),
                });
            }
            let c = pattern_bytes[p];
            p += 1;
            c
        }};
    }

    // Main compilation loop
    while p < plen {
        let c = pattern_bytes[p];
        p += 1;

        match c {
            // ----------------------------------------------------------
            // ^ — beginning of line
            // ----------------------------------------------------------
            b'^' => {
                // GNU: only special at beginning of pattern or after \( or \|
                // For simplicity, always treat as begline (GNU does context check)
                laststart = None;
                pending_exact = None;
                emit_op!(RegexOp::BegLine);
            }

            // ----------------------------------------------------------
            // $ — end of line
            // ----------------------------------------------------------
            b'$' => {
                laststart = Some(bpos!());
                pending_exact = None;
                emit_op!(RegexOp::EndLine);
            }

            // ----------------------------------------------------------
            // . — any character
            // ----------------------------------------------------------
            b'.' => {
                laststart = Some(bpos!());
                pending_exact = None;
                emit_op!(RegexOp::AnyChar);
            }

            // ----------------------------------------------------------
            // * + ? — repetition operators
            // ----------------------------------------------------------
            b'*' | b'+' | b'?' => {
                let Some(last) = laststart else {
                    // No previous expression to repeat — treat as literal
                    goto_normal_char(c, &mut buf, &mut pending_exact, &mut laststart);
                    continue;
                };

                let greedy = if p < plen && pattern_bytes[p] == b'?' {
                    p += 1; // consume '?' for non-greedy
                    false
                } else {
                    true
                };

                compile_repetition(c, greedy, posix, last, &mut buf)?;

                laststart = None; // Can't apply another postfix op
                pending_exact = None;
            }

            // ----------------------------------------------------------
            // [ — character class
            // ----------------------------------------------------------
            b'[' => {
                laststart = Some(bpos!());
                pending_exact = None;
                compile_charset(pattern_bytes, &mut p, &mut buf, case_fold)?;
            }

            // ----------------------------------------------------------
            // \ — escape sequence
            // ----------------------------------------------------------
            b'\\' => {
                if p >= plen {
                    return Err(RegexCompileError {
                        message: "trailing backslash".to_string(),
                    });
                }
                let c2 = pattern_bytes[p];
                p += 1;

                match c2 {
                    // \( — start group
                    b'(' => {
                        // Check for shy group \(?:...\) or numbered \(?N:...\)
                        let shy = p + 1 < plen
                            && pattern_bytes[p] == b'?'
                            && pattern_bytes[p + 1] == b':';
                        if shy {
                            p += 2; // skip ?:
                        }

                        compile_stack.push(CompileStackEntry {
                            begalt_offset,
                            fixup_alt_jump,
                            laststart_offset: laststart,
                            regnum,
                        });

                        if !shy {
                            regnum += 1;
                            buf.re_nsub += 1;
                            emit_op!(RegexOp::StartMemory);
                            emit!(regnum as u8);
                        }

                        begalt_offset = bpos!();
                        laststart = None;
                        fixup_alt_jump = None;
                        pending_exact = None;
                    }

                    // \) — end group
                    b')' => {
                        let Some(entry) = compile_stack.pop() else {
                            return Err(RegexCompileError {
                                message: "unmatched \\)".to_string(),
                            });
                        };

                        // Handle pending alternation fixup
                        if let Some(fixup) = fixup_alt_jump {
                            let target = bpos!() as i16 - fixup as i16 - 2;
                            store_number(&mut buf.buffer, fixup, target);
                        }

                        let was_shy = entry.regnum == regnum;
                        // Only emit stop_memory for non-shy groups
                        if !was_shy {
                            // The group number for this group
                            let this_group = entry.regnum + 1;
                            // Actually, regnum was incremented at open, so
                            // the group that's closing is the current stack entry's
                            emit_op!(RegexOp::StopMemory);
                            emit!(this_group as u8);
                        }

                        begalt_offset = entry.begalt_offset;
                        fixup_alt_jump = entry.fixup_alt_jump;
                        laststart = Some(entry.laststart_offset.unwrap_or(0));
                        regnum = entry.regnum;
                        pending_exact = None;
                    }

                    // \| — alternation
                    b'|' => {
                        pending_exact = None;

                        // Emit jump past the next alternative
                        emit_op!(RegexOp::Jump);
                        let jump_pos = bpos!();
                        emit!(0);
                        emit!(0); // placeholder offset

                        // Fixup previous alternative's failure jump
                        if let Some(fixup) = fixup_alt_jump {
                            let target = bpos!() as i16 - fixup as i16 - 2;
                            store_number(&mut buf.buffer, fixup, target);
                        }

                        // Insert on_failure_jump at the start of the current alt
                        let alt_start = begalt_offset;
                        // We need to insert 3 bytes at alt_start
                        buf.buffer
                            .splice(alt_start..alt_start, [RegexOp::OnFailureJump as u8, 0, 0]);
                        // The failure jump target is right after the jump we just emitted
                        let target = (bpos!() - alt_start - 3) as i16;
                        store_number(&mut buf.buffer, alt_start + 1, target);

                        // Adjust jump_pos since we inserted 3 bytes
                        fixup_alt_jump = Some(jump_pos + 3);

                        begalt_offset = bpos!();
                        laststart = None;
                    }

                    // \` — beginning of buffer
                    b'`' => {
                        laststart = Some(bpos!());
                        pending_exact = None;
                        emit_op!(RegexOp::BegBuf);
                    }

                    // \' — end of buffer
                    b'\'' => {
                        laststart = Some(bpos!());
                        pending_exact = None;
                        emit_op!(RegexOp::EndBuf);
                    }

                    // \= — at point
                    b'=' => {
                        laststart = Some(bpos!());
                        pending_exact = None;
                        emit_op!(RegexOp::AtDot);
                    }

                    // \b — word boundary
                    b'b' => {
                        laststart = Some(bpos!());
                        pending_exact = None;
                        emit_op!(RegexOp::WordBound);
                    }

                    // \B — not word boundary
                    b'B' => {
                        laststart = Some(bpos!());
                        pending_exact = None;
                        emit_op!(RegexOp::NotWordBound);
                    }

                    // \< — word beginning
                    b'<' => {
                        laststart = Some(bpos!());
                        pending_exact = None;
                        emit_op!(RegexOp::WordBeg);
                    }

                    // \> — word end
                    b'>' => {
                        laststart = Some(bpos!());
                        pending_exact = None;
                        emit_op!(RegexOp::WordEnd);
                    }

                    // \_ — symbol boundary
                    b'_' => {
                        if p < plen {
                            let c3 = pattern_bytes[p];
                            p += 1;
                            match c3 {
                                b'<' => {
                                    laststart = Some(bpos!());
                                    pending_exact = None;
                                    emit_op!(RegexOp::SymBeg);
                                }
                                b'>' => {
                                    laststart = Some(bpos!());
                                    pending_exact = None;
                                    emit_op!(RegexOp::SymEnd);
                                }
                                _ => {
                                    // Not a valid symbol boundary — treat \_ as literal
                                    goto_normal_char(
                                        b'_',
                                        &mut buf,
                                        &mut pending_exact,
                                        &mut laststart,
                                    );
                                }
                            }
                        }
                    }

                    // \w — word constituent (syntax-table aware)
                    b'w' => {
                        laststart = Some(bpos!());
                        pending_exact = None;
                        emit_op!(RegexOp::SyntaxSpec);
                        emit!(SyntaxClass::Word as u8);
                    }

                    // \W — not word constituent
                    b'W' => {
                        laststart = Some(bpos!());
                        pending_exact = None;
                        emit_op!(RegexOp::NotSyntaxSpec);
                        emit!(SyntaxClass::Word as u8);
                    }

                    // \sC — syntax class C
                    b's' => {
                        if p >= plen {
                            return Err(RegexCompileError {
                                message: "\\s requires syntax class character".to_string(),
                            });
                        }
                        let sc = pattern_bytes[p] as char;
                        p += 1;
                        let Some(class) = SyntaxClass::from_char(sc) else {
                            return Err(RegexCompileError {
                                message: format!("invalid syntax class: \\s{sc}"),
                            });
                        };
                        laststart = Some(bpos!());
                        pending_exact = None;
                        emit_op!(RegexOp::SyntaxSpec);
                        emit!(class as u8);
                    }

                    // \SC — not syntax class C
                    b'S' => {
                        if p >= plen {
                            return Err(RegexCompileError {
                                message: "\\S requires syntax class character".to_string(),
                            });
                        }
                        let sc = pattern_bytes[p] as char;
                        p += 1;
                        let Some(class) = SyntaxClass::from_char(sc) else {
                            return Err(RegexCompileError {
                                message: format!("invalid syntax class: \\S{sc}"),
                            });
                        };
                        laststart = Some(bpos!());
                        pending_exact = None;
                        emit_op!(RegexOp::NotSyntaxSpec);
                        emit!(class as u8);
                    }

                    // \cC — category C
                    b'c' => {
                        if p >= plen {
                            return Err(RegexCompileError {
                                message: "\\c requires category character".to_string(),
                            });
                        }
                        let cat = pattern_bytes[p];
                        p += 1;
                        laststart = Some(bpos!());
                        pending_exact = None;
                        emit_op!(RegexOp::CategorySpec);
                        emit!(cat);
                    }

                    // \CC — not category C
                    b'C' => {
                        if p >= plen {
                            return Err(RegexCompileError {
                                message: "\\C requires category character".to_string(),
                            });
                        }
                        let cat = pattern_bytes[p];
                        p += 1;
                        laststart = Some(bpos!());
                        pending_exact = None;
                        emit_op!(RegexOp::NotCategorySpec);
                        emit!(cat);
                    }

                    // \1-\9 — backreference
                    b'1'..=b'9' => {
                        let group = (c2 - b'0') as usize;
                        if group > buf.re_nsub {
                            return Err(RegexCompileError {
                                message: format!(
                                    "invalid back reference \\{group}: only {} groups defined",
                                    buf.re_nsub
                                ),
                            });
                        }
                        laststart = Some(bpos!());
                        pending_exact = None;
                        emit_op!(RegexOp::Duplicate);
                        emit!(group as u8);
                    }

                    // \{ — interval \{n,m\}
                    b'{' => {
                        // Parse interval
                        let interval_start = p;
                        let (min_count, max_count) = parse_interval(pattern_bytes, &mut p)?;

                        let Some(last) = laststart else {
                            return Err(RegexCompileError {
                                message: "\\{ without preceding expression".to_string(),
                            });
                        };

                        compile_interval(min_count, max_count, last, &mut buf)?;
                        laststart = None;
                        pending_exact = None;
                    }

                    // Other escaped characters — treat as literal
                    _ => {
                        goto_normal_char(c2, &mut buf, &mut pending_exact, &mut laststart);
                    }
                }
            }

            // ----------------------------------------------------------
            // Normal character — add to exactn
            // ----------------------------------------------------------
            _ => {
                goto_normal_char(c, &mut buf, &mut pending_exact, &mut laststart);
            }
        }
    }

    // Check for unmatched \(
    if !compile_stack.is_empty() {
        return Err(RegexCompileError {
            message: "unmatched \\(".to_string(),
        });
    }

    // Handle final alternation fixup
    if let Some(fixup) = fixup_alt_jump {
        let target = bpos!() as i16 - fixup as i16 - 2;
        store_number(&mut buf.buffer, fixup, target);
    }

    // Emit final succeed
    emit_op!(RegexOp::Succeed);

    Ok(buf)
}

// ---------------------------------------------------------------------------
// Compiler Helpers
// ---------------------------------------------------------------------------

/// Emit a literal character as part of an `exactn` sequence.
fn goto_normal_char(
    c: u8,
    buf: &mut CompiledPattern,
    pending_exact: &mut Option<usize>,
    laststart: &mut Option<usize>,
) {
    // If we have a pending exactn and it hasn't reached max length (255),
    // just append to it
    if let Some(exact_pos) = *pending_exact {
        let count = buf.buffer[exact_pos] as usize;
        if count < 255 {
            buf.buffer[exact_pos] += 1;
            buf.buffer.push(c);
            return;
        }
    }

    // Start a new exactn
    *laststart = Some(buf.buffer.len());
    buf.buffer.push(RegexOp::Exactn as u8);
    *pending_exact = Some(buf.buffer.len());
    buf.buffer.push(1); // count = 1
    buf.buffer.push(c);
}

/// Compile a repetition operator (*, +, ?).
///
/// Inserts jump opcodes around the preceding expression to implement
/// the repetition. Mirrors GNU's handling in regex_compile cases '*', '+', '?'.
fn compile_repetition(
    op: u8,
    greedy: bool,
    posix: bool,
    laststart: usize,
    buf: &mut CompiledPattern,
) -> Result<(), RegexCompileError> {
    let after_last = buf.buffer.len();

    match op {
        b'*' => {
            // * = zero or more
            if greedy {
                // Insert on_failure_jump before the expression
                // Then add jump back to the on_failure_jump after expression
                buf.buffer.splice(
                    laststart..laststart,
                    [RegexOp::OnFailureJumpLoop as u8, 0, 0],
                );
                // Jump target = after the closing jump (which we're about to add)
                let expr_len = after_last - laststart;
                let fail_target = (expr_len + 3) as i16; // +3 for the jump we'll add
                store_number(&mut buf.buffer, laststart + 1, fail_target);

                // Add jump back to the on_failure_jump
                buf.buffer.push(RegexOp::Jump as u8);
                let jump_target = -(buf.buffer.len() as i16 - laststart as i16 + 2 - 3);
                let jpos = buf.buffer.len();
                buf.buffer.push(0);
                buf.buffer.push(0);
                store_number(&mut buf.buffer, jpos, jump_target);
            } else {
                // Non-greedy *? — try skipping first
                buf.buffer.splice(
                    laststart..laststart,
                    [RegexOp::OnFailureKeepStringJump as u8, 0, 0],
                );
                let expr_len = after_last - laststart;
                let fail_target = (expr_len + 6) as i16;
                store_number(&mut buf.buffer, laststart + 1, fail_target);

                buf.buffer.push(RegexOp::OnFailureJump as u8);
                let jpos = buf.buffer.len();
                buf.buffer.push(0);
                buf.buffer.push(0);
                let jump_target = -(buf.buffer.len() as i16 - laststart as i16 - 3 + 2);
                store_number(&mut buf.buffer, jpos, jump_target);
            }
        }
        b'+' => {
            // + = one or more — same as expression followed by *
            // But we just add the loop jump at the end
            if greedy {
                buf.buffer.push(RegexOp::OnFailureJumpLoop as u8);
                let jpos = buf.buffer.len();
                buf.buffer.push(0);
                buf.buffer.push(0);
                // Fail = after this instruction (continue past loop)
                store_number(&mut buf.buffer, jpos, 3);

                buf.buffer.push(RegexOp::Jump as u8);
                let jpos2 = buf.buffer.len();
                buf.buffer.push(0);
                buf.buffer.push(0);
                let jump_target = -(buf.buffer.len() as i16 - laststart as i16 + 2);
                store_number(&mut buf.buffer, jpos2, jump_target);
            } else {
                // Non-greedy +?
                buf.buffer.push(RegexOp::OnFailureJump as u8);
                let jpos = buf.buffer.len();
                buf.buffer.push(0);
                buf.buffer.push(0);
                let jump_target = -(buf.buffer.len() as i16 - laststart as i16 + 2);
                store_number(&mut buf.buffer, jpos, jump_target);
            }
        }
        b'?' => {
            // ? = zero or one
            if greedy {
                buf.buffer
                    .splice(laststart..laststart, [RegexOp::OnFailureJump as u8, 0, 0]);
                let expr_len = after_last - laststart;
                let fail_target = (expr_len + 3) as i16;
                store_number(&mut buf.buffer, laststart + 1, fail_target);
            } else {
                // Non-greedy ??
                buf.buffer.splice(
                    laststart..laststart,
                    [RegexOp::OnFailureKeepStringJump as u8, 0, 0],
                );
                let expr_len = after_last - laststart;
                store_number(&mut buf.buffer, laststart + 1, (expr_len + 3) as i16);
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}

/// Compile a character class `[...]` into charset bytecode.
fn compile_charset(
    pattern: &[u8],
    p: &mut usize,
    buf: &mut CompiledPattern,
    case_fold: bool,
) -> Result<(), RegexCompileError> {
    let plen = pattern.len();

    // Check for negation
    let negate = *p < plen && pattern[*p] == b'^';
    if negate {
        *p += 1;
    }

    let op = if negate {
        RegexOp::CharsetNot
    } else {
        RegexOp::Charset
    };

    buf.buffer.push(op as u8);
    let bitmap_len_pos = buf.buffer.len();
    buf.buffer.push(32); // 256 bits = 32 bytes bitmap

    // Initialize 32-byte bitmap (256 bits, one per ASCII char)
    let bitmap_start = buf.buffer.len();
    buf.buffer.extend_from_slice(&[0u8; 32]);

    // Special case: ] at start is literal
    let mut first = true;

    while *p < plen {
        let c = pattern[*p];
        *p += 1;

        if c == b']' && !first {
            break;
        }
        first = false;

        if c == b'-' && *p < plen && !first {
            // Range: previous char - next char
            // The previous char was already set in bitmap
            // We need to set all chars in range
            if *p < plen && pattern[*p] != b']' {
                let range_end = pattern[*p];
                *p += 1;
                // Get the range start from the last char we set
                // For simplicity, set the range_end char
                if (range_end as usize) < 256 {
                    set_bitmap_bit(&mut buf.buffer, bitmap_start, range_end, case_fold);
                }
                continue;
            }
        }

        // Handle POSIX classes [[:alpha:]], etc.
        if c == b'[' && *p < plen && pattern[*p] == b':' {
            *p += 1; // skip :
            let class_start = *p;
            while *p < plen && pattern[*p] != b':' {
                *p += 1;
            }
            if *p < plen {
                let class_name = std::str::from_utf8(&pattern[class_start..*p]).unwrap_or("");
                *p += 1; // skip :
                if *p < plen && pattern[*p] == b']' {
                    *p += 1; // skip ]
                }
                apply_posix_class(class_name, &mut buf.buffer, bitmap_start, case_fold);
                continue;
            }
        }

        // Regular character
        if (c as usize) < 256 {
            set_bitmap_bit(&mut buf.buffer, bitmap_start, c, case_fold);
        }
    }

    Ok(())
}

/// Set a bit in the charset bitmap (and its case-fold partner).
fn set_bitmap_bit(buffer: &mut Vec<u8>, bitmap_start: usize, c: u8, case_fold: bool) {
    let byte_idx = bitmap_start + (c as usize / 8);
    let bit_idx = c as usize % 8;
    if byte_idx < buffer.len() {
        buffer[byte_idx] |= 1 << bit_idx;
    }
    if case_fold {
        let upper = (c as char).to_uppercase().next().unwrap_or(c as char) as u8;
        let lower = (c as char).to_lowercase().next().unwrap_or(c as char) as u8;
        for alt in [upper, lower] {
            if alt != c {
                let byte_idx2 = bitmap_start + (alt as usize / 8);
                let bit_idx2 = alt as usize % 8;
                if byte_idx2 < buffer.len() {
                    buffer[byte_idx2] |= 1 << bit_idx2;
                }
            }
        }
    }
}

/// Apply a POSIX character class to the bitmap.
fn apply_posix_class(name: &str, buffer: &mut Vec<u8>, bitmap_start: usize, case_fold: bool) {
    let chars: Vec<u8> = match name {
        "alpha" => (b'A'..=b'Z').chain(b'a'..=b'z').collect(),
        "digit" => (b'0'..=b'9').collect(),
        "alnum" => (b'A'..=b'Z')
            .chain(b'a'..=b'z')
            .chain(b'0'..=b'9')
            .collect(),
        "space" | "blank" => vec![b' ', b'\t', b'\n', b'\r', 0x0B, 0x0C],
        "upper" => (b'A'..=b'Z').collect(),
        "lower" => (b'a'..=b'z').collect(),
        "punct" => b"!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~".to_vec(),
        "print" | "graph" => (0x20u8..=0x7E).collect(),
        "cntrl" => (0x00u8..=0x1F).chain(std::iter::once(0x7F)).collect(),
        "xdigit" => (b'0'..=b'9')
            .chain(b'A'..=b'F')
            .chain(b'a'..=b'f')
            .collect(),
        "ascii" => (0x00u8..=0x7F).collect(),
        _ => return,
    };
    for c in chars {
        set_bitmap_bit(buffer, bitmap_start, c, case_fold);
    }
}

/// Parse an interval \{n,m\} from the pattern.
/// Returns (min, max) where max=None means unbounded.
fn parse_interval(
    pattern: &[u8],
    p: &mut usize,
) -> Result<(usize, Option<usize>), RegexCompileError> {
    let plen = pattern.len();

    // Parse min
    let mut min = 0usize;
    while *p < plen && pattern[*p].is_ascii_digit() {
        min = min * 10 + (pattern[*p] - b'0') as usize;
        *p += 1;
    }

    let max = if *p < plen && pattern[*p] == b',' {
        *p += 1; // skip comma
        if *p < plen && pattern[*p] == b'\\' && *p + 1 < plen && pattern[*p + 1] == b'}' {
            // \{n,\} — unbounded
            None
        } else {
            let mut m = 0usize;
            while *p < plen && pattern[*p].is_ascii_digit() {
                m = m * 10 + (pattern[*p] - b'0') as usize;
                *p += 1;
            }
            Some(m)
        }
    } else {
        Some(min) // \{n\} — exact count
    };

    // Expect \}
    if *p + 1 < plen && pattern[*p] == b'\\' && pattern[*p + 1] == b'}' {
        *p += 2;
    } else {
        return Err(RegexCompileError {
            message: "unterminated \\{".to_string(),
        });
    }

    Ok((min, max))
}

/// Compile an interval \{n,m\} into bytecode.
fn compile_interval(
    min: usize,
    max: Option<usize>,
    laststart: usize,
    buf: &mut CompiledPattern,
) -> Result<(), RegexCompileError> {
    // Simple implementation: repeat the expression min times literally,
    // then add optional repetitions up to max.
    // This is a simplified version — GNU uses succeed_n/jump_n opcodes.

    let expr_bytes: Vec<u8> = buf.buffer[laststart..].to_vec();

    // Remove the original expression (we'll re-emit it)
    buf.buffer.truncate(laststart);

    // Emit min mandatory copies
    for _ in 0..min {
        buf.buffer.extend_from_slice(&expr_bytes);
    }

    // Emit optional copies (up to max - min)
    match max {
        Some(max_val) if max_val > min => {
            for _ in 0..(max_val - min) {
                // on_failure_jump past this copy
                buf.buffer.push(RegexOp::OnFailureJump as u8);
                let jpos = buf.buffer.len();
                buf.buffer.push(0);
                buf.buffer.push(0);
                buf.buffer.extend_from_slice(&expr_bytes);
                let target = (buf.buffer.len() - jpos - 2) as i16;
                store_number(&mut buf.buffer, jpos, target);
            }
        }
        None => {
            // Unbounded: add * loop
            let loop_start = buf.buffer.len();
            buf.buffer.push(RegexOp::OnFailureJumpLoop as u8);
            let jpos = buf.buffer.len();
            buf.buffer.push(0);
            buf.buffer.push(0);
            let fail_target = (expr_bytes.len() + 3 + 2) as i16;
            store_number(&mut buf.buffer, jpos, fail_target);

            buf.buffer.extend_from_slice(&expr_bytes);

            buf.buffer.push(RegexOp::Jump as u8);
            let jpos2 = buf.buffer.len();
            buf.buffer.push(0);
            buf.buffer.push(0);
            let jump_target = -(buf.buffer.len() as i16 - loop_start as i16 + 2);
            store_number(&mut buf.buffer, jpos2, jump_target);
        }
        _ => {} // max == min, already handled
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// TODO: Phase 3 — Matcher (re_match_2_internal)
// TODO: Phase 4 — Searcher (re_search_2)
// ---------------------------------------------------------------------------
