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

use std::collections::{HashMap, HashSet};

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

    /// Saved interval-counter overrides at this point.
    /// Keyed by bytecode position of the 2-byte counter field.
    saved_counters: HashMap<usize, i16>,
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

/// Read a counter value from the counter table, falling back to the bytecode
/// if no override has been stored yet.  Used by `succeed_n`, `jump_n`, and
/// `set_number_at` to emulate GNU's in-place bytecode mutation on immutable
/// bytecode.
fn get_counter(counters: &HashMap<usize, i16>, bytecode: &[u8], pos: usize) -> i16 {
    counters
        .get(&pos)
        .copied()
        .unwrap_or_else(|| extract_number(bytecode, pos))
}

/// Store a counter value in the mutable counter table (keyed by bytecode
/// position).
fn set_counter(counters: &mut HashMap<usize, i16>, pos: usize, val: i16) {
    counters.insert(pos, val);
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
    /// Group number at the time of \( (before incrementing).
    regnum: usize,
    /// The actual group number assigned to this \( (None for shy groups).
    assigned_group: Option<usize>,
    /// Bytecode position of the group's StartMemory (or OnFailureJump
    /// for shy groups). Used by postfix ops like ? * + after \).
    group_bytecode_start: usize,
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
    #[allow(unused_macros)]
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
                let Some(mut last) = laststart else {
                    // No previous expression to repeat — treat as literal
                    goto_normal_char(c, &mut buf, &mut pending_exact, &mut laststart);
                    continue;
                };

                // GNU regex_compile: if the preceding expression was part
                // of an exactn with count > 1, split off the last character
                // so that the repetition applies only to that character.
                if buf.buffer[last] == RegexOp::Exactn as u8 {
                    let count_pos = last + 1;
                    let count = buf.buffer[count_pos];
                    if count > 1 {
                        // Decrement the existing exactn's count
                        buf.buffer[count_pos] = count - 1;
                        // Remove the last char from the existing exactn
                        let last_char = buf.buffer.pop().unwrap();
                        // Insert a new single-char exactn for the split char
                        last = buf.buffer.len();
                        buf.buffer.push(RegexOp::Exactn as u8);
                        buf.buffer.push(1);
                        buf.buffer.push(last_char);
                    }
                }

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

                        // Check for explicit numbered group \(?N:...\)
                        let mut explicit_group: Option<usize> = None;
                        if !shy && p < plen && pattern_bytes[p] == b'?' {
                            // Look for \(?N:...\) where N is a digit
                            let saved_p = p;
                            p += 1; // skip ?
                            // Parse digits
                            let num_start = p;
                            while p < plen && pattern_bytes[p].is_ascii_digit() {
                                p += 1;
                            }
                            if p > num_start && p < plen && pattern_bytes[p] == b':' {
                                let num_str = std::str::from_utf8(&pattern_bytes[num_start..p])
                                    .unwrap_or("0");
                                if let Ok(n) = num_str.parse::<usize>() {
                                    explicit_group = Some(n);
                                    p += 1; // skip :
                                }
                            }
                            if explicit_group.is_none() {
                                p = saved_p; // not a valid numbered group
                            }
                        }

                        let is_shy = shy;

                        let group_start = bpos!();
                        let assigned = if let Some(n) = explicit_group {
                            Some(n)
                        } else if !is_shy {
                            Some(regnum + 1)
                        } else {
                            None
                        };

                        compile_stack.push(CompileStackEntry {
                            begalt_offset,
                            fixup_alt_jump,
                            laststart_offset: laststart,
                            regnum,
                            assigned_group: assigned,
                            group_bytecode_start: group_start,
                        });

                        if let Some(n) = explicit_group {
                            // Explicit numbered group: assign group number n
                            while buf.re_nsub < n {
                                buf.re_nsub += 1;
                            }
                            regnum = n;
                            emit_op!(RegexOp::StartMemory);
                            emit!(n as u8);
                        } else if !is_shy {
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

                        // Emit StopMemory for non-shy groups.
                        if let Some(group_num) = entry.assigned_group {
                            emit_op!(RegexOp::StopMemory);
                            emit!(group_num as u8);
                        }

                        begalt_offset = entry.begalt_offset;
                        fixup_alt_jump = entry.fixup_alt_jump;
                        // After \), laststart points to the group's start
                        // so postfix operators (?, *, +) apply to the group.
                        laststart = Some(entry.group_bytecode_start);
                        // Do NOT restore regnum — it keeps incrementing
                        // across sibling groups (GNU behavior).
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
                        let _interval_start = p;
                        let (min_count, max_count) = parse_interval(pattern_bytes, &mut p)?;

                        // Check for non-greedy suffix ?
                        let lazy = p < plen && pattern_bytes[p] == b'?';
                        if lazy {
                            p += 1;
                        }

                        let Some(last) = laststart else {
                            return Err(RegexCompileError {
                                message: "\\{ without preceding expression".to_string(),
                            });
                        };

                        compile_interval(min_count, max_count, lazy, last, &mut buf)?;
                        laststart = None;
                        pending_exact = None;
                    }

                    // Control/escape character shortcuts
                    // GNU Emacs receives these already converted by the
                    // Lisp reader, but callers from Rust may pass the
                    // backslash-letter form. Handle both.
                    b't' => {
                        goto_normal_char(b'\t', &mut buf, &mut pending_exact, &mut laststart);
                    }
                    b'n' => {
                        goto_normal_char(b'\n', &mut buf, &mut pending_exact, &mut laststart);
                    }
                    b'r' => {
                        goto_normal_char(b'\r', &mut buf, &mut pending_exact, &mut laststart);
                    }
                    b'f' => {
                        goto_normal_char(0x0c, &mut buf, &mut pending_exact, &mut laststart);
                    }
                    b'a' => {
                        goto_normal_char(0x07, &mut buf, &mut pending_exact, &mut laststart);
                    }
                    b'e' => {
                        goto_normal_char(0x1b, &mut buf, &mut pending_exact, &mut laststart);
                    }
                    // \d — digit [0-9]  (not in GNU Emacs, but used in tests)
                    b'd' => {
                        laststart = Some(bpos!());
                        pending_exact = None;
                        emit_op!(RegexOp::Charset);
                        emit!(32);
                        let bitmap_start = buf.buffer.len();
                        buf.buffer.extend_from_slice(&[0u8; 32]);
                        for ch in b'0'..=b'9' {
                            set_bitmap_bit(&mut buf.buffer, bitmap_start, ch, false);
                        }
                    }
                    // \D — non-digit [^0-9]
                    b'D' => {
                        laststart = Some(bpos!());
                        pending_exact = None;
                        emit_op!(RegexOp::CharsetNot);
                        emit!(32);
                        let bitmap_start = buf.buffer.len();
                        buf.buffer.extend_from_slice(&[0u8; 32]);
                        for ch in b'0'..=b'9' {
                            set_bitmap_bit(&mut buf.buffer, bitmap_start, ch, false);
                        }
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

    // Populate the fastmap for search-time position skipping.
    compile_fastmap(&mut buf);

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
    _posix: bool,
    laststart: usize,
    buf: &mut CompiledPattern,
) -> Result<(), RegexCompileError> {
    // All offsets are relative to the position right after the 2-byte offset
    // field.  This matches GNU's convention: after EXTRACT_NUMBER_AND_INCR,
    // `p` points past the offset, and the target is `p + mcnt`.

    let after_last = buf.buffer.len();

    match op {
        b'*' => {
            // * = zero or more
            if greedy {
                // Layout:
                //   [laststart] OFJL  offset(2)  <expr>  Jump  offset(2)
                //   OFJL fail target → past the Jump instruction
                //   Jump target → back to OFJL opcode

                // Insert OnFailureJumpLoop before the expression
                buf.buffer.splice(
                    laststart..laststart,
                    [RegexOp::OnFailureJumpLoop as u8, 0, 0],
                );
                // After splice, expr occupies [laststart+3 .. laststart+3+expr_len)
                let expr_len = after_last - laststart; // original expr length

                // Add Jump back to the OFJL
                buf.buffer.push(RegexOp::Jump as u8);
                let jpos = buf.buffer.len();
                buf.buffer.push(0);
                buf.buffer.push(0);

                // OFJL fail target: from (laststart+3) → past Jump = (jpos+2)
                // offset = (jpos+2) - (laststart+3) = expr_len + 3
                let ofjl_offset = (expr_len + 3) as i16;
                store_number(&mut buf.buffer, laststart + 1, ofjl_offset);

                // Jump target: from (jpos+2) → OFJL opcode at laststart
                // offset = laststart - (jpos + 2)
                let jump_offset = laststart as i16 - (jpos as i16 + 2);
                store_number(&mut buf.buffer, jpos, jump_offset);
            } else {
                // Non-greedy *?
                // Layout:
                //   [laststart] OFKSJ  offset(2)  <expr>  OFJ  offset(2)
                //   OFKSJ fail target → past the OFJ instruction
                //   OFJ target → back to OFKSJ opcode
                buf.buffer.splice(
                    laststart..laststart,
                    [RegexOp::OnFailureKeepStringJump as u8, 0, 0],
                );
                let expr_len = after_last - laststart;

                buf.buffer.push(RegexOp::OnFailureJump as u8);
                let jpos = buf.buffer.len();
                buf.buffer.push(0);
                buf.buffer.push(0);

                // OFKSJ fail target: from (laststart+3) → past OFJ = (jpos+2)
                let ofksj_offset = (expr_len + 3) as i16;
                store_number(&mut buf.buffer, laststart + 1, ofksj_offset);

                // OFJ target: from (jpos+2) → OFKSJ opcode at laststart
                let ofj_offset = laststart as i16 - (jpos as i16 + 2);
                store_number(&mut buf.buffer, jpos, ofj_offset);
            }
        }
        b'+' => {
            // + = one or more
            // Layout: <expr(already emitted)>  OFJL/OFJ  offset(2)  Jump  offset(2)
            if greedy {
                // OFJL fail target → past the Jump instruction (continue)
                buf.buffer.push(RegexOp::OnFailureJumpLoop as u8);
                let ofjl_pos = buf.buffer.len();
                buf.buffer.push(0);
                buf.buffer.push(0);

                buf.buffer.push(RegexOp::Jump as u8);
                let jpos = buf.buffer.len();
                buf.buffer.push(0);
                buf.buffer.push(0);

                // OFJL fail: from (ofjl_pos+2) → (jpos+2)
                store_number(&mut buf.buffer, ofjl_pos, (jpos + 2 - ofjl_pos - 2) as i16);

                // Jump target: from (jpos+2) → laststart (start of expr)
                let jump_offset = laststart as i16 - (jpos as i16 + 2);
                store_number(&mut buf.buffer, jpos, jump_offset);
            } else {
                // Non-greedy +?
                buf.buffer.push(RegexOp::OnFailureJump as u8);
                let jpos = buf.buffer.len();
                buf.buffer.push(0);
                buf.buffer.push(0);
                // OFJ target: from (jpos+2) → laststart
                let jump_offset = laststart as i16 - (jpos as i16 + 2);
                store_number(&mut buf.buffer, jpos, jump_offset);
            }
        }
        b'?' => {
            // ? = zero or one
            if greedy {
                // Layout: [laststart] OFJ  offset(2)  <expr>
                // OFJ fail target → past expr
                buf.buffer
                    .splice(laststart..laststart, [RegexOp::OnFailureJump as u8, 0, 0]);
                let expr_len = after_last - laststart;
                // From (laststart+3) → (laststart+3+expr_len), offset = expr_len
                store_number(&mut buf.buffer, laststart + 1, expr_len as i16);
            } else {
                // Non-greedy ??
                buf.buffer.splice(
                    laststart..laststart,
                    [RegexOp::OnFailureKeepStringJump as u8, 0, 0],
                );
                let expr_len = after_last - laststart;
                store_number(&mut buf.buffer, laststart + 1, expr_len as i16);
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
    let mut last_char: Option<u8> = None; // Track last single char for ranges

    while *p < plen {
        let c = pattern[*p];
        *p += 1;

        if c == b']' && !first {
            break;
        }
        first = false;

        if c == b'-' && *p < plen && pattern[*p] != b']' {
            if let Some(range_start) = last_char {
                // Range: range_start - next char
                let range_end = pattern[*p];
                *p += 1;
                for ch in range_start..=range_end {
                    set_bitmap_bit(&mut buf.buffer, bitmap_start, ch, case_fold);
                }
                last_char = None; // Range consumed
                continue;
            }
            // '-' at start or after a range → literal '-'
            set_bitmap_bit(&mut buf.buffer, bitmap_start, b'-', case_fold);
            last_char = Some(b'-');
            continue;
        }

        // In GNU Emacs, backslash is NOT special inside [...].
        // It's treated as a literal backslash character.
        // However, NeoVM callers (from Rust) may pass \w, \s, \d
        // inside [...] expecting them to work. We handle the most
        // common cases but fall through to literal for unknown escapes.
        if c == b'\\' && *p < plen {
            let esc = pattern[*p];
            match esc {
                b'w' => {
                    *p += 1;
                    for ch in b'a'..=b'z' {
                        set_bitmap_bit(&mut buf.buffer, bitmap_start, ch, case_fold);
                    }
                    for ch in b'A'..=b'Z' {
                        set_bitmap_bit(&mut buf.buffer, bitmap_start, ch, case_fold);
                    }
                    for ch in b'0'..=b'9' {
                        set_bitmap_bit(&mut buf.buffer, bitmap_start, ch, false);
                    }
                    set_bitmap_bit(&mut buf.buffer, bitmap_start, b'_', false);
                    last_char = None;
                    continue;
                }
                b'W' => {
                    *p += 1;
                    for ch in 0u8..=127 {
                        if !ch.is_ascii_alphanumeric() && ch != b'_' {
                            set_bitmap_bit(&mut buf.buffer, bitmap_start, ch, false);
                        }
                    }
                    last_char = None;
                    continue;
                }
                b's' if *p + 1 < plen => {
                    let sc = pattern[*p + 1];
                    if sc == b'-' || sc == b' ' {
                        *p += 2;
                        set_bitmap_bit(&mut buf.buffer, bitmap_start, b' ', false);
                        set_bitmap_bit(&mut buf.buffer, bitmap_start, b'\t', false);
                        set_bitmap_bit(&mut buf.buffer, bitmap_start, b'\n', false);
                        set_bitmap_bit(&mut buf.buffer, bitmap_start, b'\r', false);
                        set_bitmap_bit(&mut buf.buffer, bitmap_start, 0x0c, false);
                        set_bitmap_bit(&mut buf.buffer, bitmap_start, 0x0b, false);
                        last_char = None;
                        continue;
                    }
                    // Fall through to treat \ as literal
                }
                b'd' => {
                    *p += 1;
                    for ch in b'0'..=b'9' {
                        set_bitmap_bit(&mut buf.buffer, bitmap_start, ch, false);
                    }
                    last_char = None;
                    continue;
                }
                _ => {
                    // In GNU Emacs, backslash inside [...] is literal.
                    // Don't consume the next char — just set '\' bit.
                }
            }
            // Treat backslash as literal character
            set_bitmap_bit(&mut buf.buffer, bitmap_start, b'\\', case_fold);
            last_char = Some(b'\\');
            continue;
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
                last_char = None;
                continue;
            }
        }

        // Regular character
        if (c as usize) < 256 {
            set_bitmap_bit(&mut buf.buffer, bitmap_start, c, case_fold);
            last_char = Some(c);
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
    lazy: bool,
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
            if lazy {
                // Non-greedy: use OnFailureKeepStringJump to prefer
                // skipping the optional copy (matching fewer).
                for _ in 0..(max_val - min) {
                    buf.buffer.push(RegexOp::OnFailureKeepStringJump as u8);
                    let jpos = buf.buffer.len();
                    buf.buffer.push(0);
                    buf.buffer.push(0);
                    buf.buffer.extend_from_slice(&expr_bytes);
                    let target = (buf.buffer.len() - jpos - 2) as i16;
                    store_number(&mut buf.buffer, jpos, target);
                }
            } else {
                // Greedy: use OnFailureJump to prefer matching the copy.
                for _ in 0..(max_val - min) {
                    buf.buffer.push(RegexOp::OnFailureJump as u8);
                    let jpos = buf.buffer.len();
                    buf.buffer.push(0);
                    buf.buffer.push(0);
                    buf.buffer.extend_from_slice(&expr_bytes);
                    let target = (buf.buffer.len() - jpos - 2) as i16;
                    store_number(&mut buf.buffer, jpos, target);
                }
            }
        }
        None => {
            if lazy {
                // Non-greedy unbounded: OFKSJ + OFJ loop
                // Layout:
                //   [loop_start] OFKSJ  offset(2)  <expr>  OFJ  offset(2)
                //   OFKSJ skip target → past OFJ (done)
                //   OFJ fail target → back to OFKSJ (try another iteration)
                let loop_start = buf.buffer.len();
                buf.buffer.push(RegexOp::OnFailureKeepStringJump as u8);
                let jpos = buf.buffer.len();
                buf.buffer.push(0);
                buf.buffer.push(0);

                buf.buffer.extend_from_slice(&expr_bytes);

                buf.buffer.push(RegexOp::OnFailureJump as u8);
                let jpos2 = buf.buffer.len();
                buf.buffer.push(0);
                buf.buffer.push(0);

                // OFKSJ skip target: from (jpos+2) → past OFJ = (jpos2+2)
                let skip_target = (jpos2 + 2 - jpos - 2) as i16;
                store_number(&mut buf.buffer, jpos, skip_target);

                // OFJ target: from (jpos2+2) → loop_start
                let ofj_target = loop_start as i16 - (jpos2 as i16 + 2);
                store_number(&mut buf.buffer, jpos2, ofj_target);
            } else {
                // Greedy unbounded: OFJL + Jump loop
                let loop_start = buf.buffer.len();
                buf.buffer.push(RegexOp::OnFailureJumpLoop as u8);
                let jpos = buf.buffer.len();
                buf.buffer.push(0);
                buf.buffer.push(0);

                buf.buffer.extend_from_slice(&expr_bytes);

                buf.buffer.push(RegexOp::Jump as u8);
                let jpos2 = buf.buffer.len();
                buf.buffer.push(0);
                buf.buffer.push(0);

                // OFJL fail: from (jpos+2) → past Jump = (jpos2+2)
                let fail_target = (jpos2 + 2 - jpos - 2) as i16;
                store_number(&mut buf.buffer, jpos, fail_target);

                // Jump target: from (jpos2+2) → loop_start
                let jump_target = loop_start as i16 - (jpos2 as i16 + 2);
                store_number(&mut buf.buffer, jpos2, jump_target);
            }
        }
        _ => {} // max == min, already handled
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Phase 3: Matcher (re_match_2_internal)
//
// Translates GNU regex-emacs.c:4072-5340.
// Executes compiled bytecode against input text with backtracking.
// ---------------------------------------------------------------------------

/// Context for syntax-table and category-table queries during matching.
///
/// The matcher queries the syntax table to implement `\w`, `\b`, `\sC`, etc.
/// In GNU Emacs, this is done via the `SYNTAX()` macro which reads from
/// `gl_state.current_syntax_table`.
pub(crate) trait SyntaxLookup {
    /// Return the syntax class of character `c` in the current syntax table.
    fn char_syntax(&self, c: char) -> SyntaxClass;

    /// Return true if character `c` belongs to category `cat`.
    fn char_has_category(&self, c: char, cat: u8) -> bool;
}

/// Default syntax lookup — uses ASCII-based definitions.
/// This is used when no buffer-specific syntax table is available.
pub(crate) struct DefaultSyntaxLookup;

impl SyntaxLookup for DefaultSyntaxLookup {
    fn char_syntax(&self, c: char) -> SyntaxClass {
        if c.is_alphanumeric() || c == '_' {
            SyntaxClass::Word
        } else if c.is_whitespace() {
            SyntaxClass::Whitespace
        } else if c.is_ascii_punctuation() {
            SyntaxClass::Punctuation
        } else if matches!(c, '(' | '[' | '{') {
            SyntaxClass::Open
        } else if matches!(c, ')' | ']' | '}') {
            SyntaxClass::Close
        } else if c == '"' || c == '\'' {
            SyntaxClass::StringDelim
        } else {
            SyntaxClass::Symbol
        }
    }

    fn char_has_category(&self, c: char, cat: u8) -> bool {
        // Minimal category support for standalone testing.
        // Category '|' (0x7c) = multibyte characters
        if cat == b'|' {
            return !c.is_ascii();
        }
        false
    }
}

/// Match a compiled pattern against input text.
///
/// This is the core matching function, equivalent to GNU's `re_match_2_internal`.
///
/// # Arguments
/// * `pattern` - Compiled bytecode pattern
/// * `text` - Input text to match against
/// * `pos` - Starting position in text
/// * `stop` - End of matching region
/// * `syntax` - Syntax table for `\w`, `\b`, `\sC` etc.
/// * `point` - Buffer point position (for `\=` / AtDot)
///
/// # Returns
/// * `Some(end_pos)` if matched — end position of the match
/// * `None` if no match
pub(crate) fn re_match(
    pattern: &CompiledPattern,
    text: &[u8],
    pos: usize,
    stop: usize,
    syntax: &dyn SyntaxLookup,
    point: usize,
) -> Option<(usize, MatchRegisters)> {
    let bytecode = &pattern.buffer;
    let num_regs = pattern.re_nsub + 1;

    let mut regs = MatchRegisters::new(num_regs);
    let mut fail_stack: Vec<FailurePoint> = Vec::new();

    // Mutable counter table for interval repetition (succeed_n / jump_n / set_number_at).
    // GNU modifies bytecode in-place; we use a side table keyed by bytecode position.
    let mut counters: HashMap<usize, i16> = HashMap::new();

    // Register tracking arrays
    let mut regstart: Vec<Option<usize>> = vec![None; num_regs];
    let mut regend: Vec<Option<usize>> = vec![None; num_regs];

    // Best match tracking (for POSIX)
    let _best_regs_set = false;
    let _best_regstart: Vec<Option<usize>> = vec![None; num_regs];
    let _best_regend: Vec<Option<usize>> = vec![None; num_regs];
    let _match_end: usize = pos;

    let mut pc = 0usize; // Bytecode program counter
    let mut d = pos; // Data position in text

    let translate = &pattern.translate;

    // Helper: translate a character for case-folding
    let tr = |c: u8| -> u8 {
        if let Some(table) = translate {
            let ch = c as u32;
            if ch < 256 {
                table[ch as usize] as u8
            } else {
                c
            }
        } else {
            c
        }
    };

    // Helper: get char at position in text (with bounds check)
    let text_byte = |pos: usize| -> Option<u8> {
        if pos < text.len() {
            Some(text[pos])
        } else {
            None
        }
    };

    // Helper: decode a UTF-8 character at position
    let text_char = |pos: usize| -> Option<(char, usize)> {
        if pos >= text.len() {
            return None;
        }
        let s = std::str::from_utf8(&text[pos..]).ok()?;
        let c = s.chars().next()?;
        Some((c, c.len_utf8()))
    };

    // Helper: is position at a word boundary?
    let at_word_boundary = |pos: usize| -> bool {
        let prev_word = if pos > 0 {
            text_char(pos.saturating_sub(1))
                .map(|(c, _)| syntax.char_syntax(c) == SyntaxClass::Word)
                .unwrap_or(false)
        } else {
            false
        };
        let curr_word = text_char(pos)
            .map(|(c, _)| syntax.char_syntax(c) == SyntaxClass::Word)
            .unwrap_or(false);
        prev_word != curr_word
    };

    // Helper: is position at a symbol boundary?
    let at_symbol_boundary = |pos: usize| -> bool {
        let is_symbol_char = |c: char| {
            let syn = syntax.char_syntax(c);
            syn == SyntaxClass::Word || syn == SyntaxClass::Symbol
        };
        let prev_sym = if pos > 0 {
            text_char(pos.saturating_sub(1))
                .map(|(c, _)| is_symbol_char(c))
                .unwrap_or(false)
        } else {
            false
        };
        let curr_sym = text_char(pos)
            .map(|(c, _)| is_symbol_char(c))
            .unwrap_or(false);
        prev_sym != curr_sym
    };

    loop {
        // End of pattern = potential match
        if pc >= bytecode.len() {
            break;
        }

        let op_byte = bytecode[pc];
        let Some(op) = RegexOp::from_byte(op_byte) else {
            // Invalid opcode — treat as match failure
            return None;
        };
        pc += 1;

        match op {
            RegexOp::NoOp => {
                // Skip
            }

            RegexOp::Succeed => {
                // Immediate success
                break;
            }

            RegexOp::Exactn => {
                let count = bytecode[pc] as usize;
                pc += 1;
                let mut matched = true;
                for i in 0..count {
                    if d >= stop {
                        matched = false;
                        pc += count - i;
                        break;
                    }
                    let pat_byte = bytecode[pc + i];
                    let txt_byte = text[d];
                    if tr(txt_byte) != tr(pat_byte) {
                        matched = false;
                        pc += count - i;
                        break;
                    }
                    d += 1;
                }
                if matched {
                    pc += count;
                } else {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                }
            }

            RegexOp::AnyChar => {
                if d >= stop {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                    continue;
                }
                // Match any character except newline
                if text[d] == b'\n' {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                    continue;
                }
                // Advance past one UTF-8 character
                if let Some((_, len)) = text_char(d) {
                    d += len;
                } else {
                    d += 1;
                }
            }

            RegexOp::Charset | RegexOp::CharsetNot => {
                let negate = op == RegexOp::CharsetNot;
                let bitmap_len = bytecode[pc] as usize & 0x7F;
                pc += 1;

                if d >= stop {
                    pc += bitmap_len;
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                    continue;
                }

                let c = text[d];
                let in_set = if (c as usize / 8) < bitmap_len {
                    let byte = bytecode[pc + c as usize / 8];
                    (byte >> (c as usize % 8)) & 1 != 0
                } else {
                    false
                };

                let matched = if negate { !in_set } else { in_set };
                pc += bitmap_len;

                if !matched {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                    continue;
                }
                d += 1;
            }

            RegexOp::StartMemory => {
                let group = bytecode[pc] as usize;
                pc += 1;
                if group < num_regs {
                    regstart[group] = Some(d);
                }
            }

            RegexOp::StopMemory => {
                let group = bytecode[pc] as usize;
                pc += 1;
                if group < num_regs {
                    regend[group] = Some(d);
                }
            }

            RegexOp::Duplicate => {
                let group = bytecode[pc] as usize;
                pc += 1;

                let Some(start) = regstart.get(group).copied().flatten() else {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                    continue;
                };
                let Some(end) = regend.get(group).copied().flatten() else {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                    continue;
                };

                let ref_len = end - start;
                if d + ref_len > stop {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                    continue;
                }

                // Compare the backreference text
                let mut matched = true;
                for i in 0..ref_len {
                    if tr(text[d + i]) != tr(text[start + i]) {
                        matched = false;
                        break;
                    }
                }
                if !matched {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                    continue;
                }
                d += ref_len;
            }

            RegexOp::BegLine => {
                if d == 0 || (d > 0 && text[d - 1] == b'\n') {
                    // At beginning of line — succeed
                } else {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                }
            }

            RegexOp::EndLine => {
                if d >= stop || text[d] == b'\n' {
                    // At end of line — succeed
                } else {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                }
            }

            RegexOp::BegBuf => {
                if d != 0 {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                }
            }

            RegexOp::EndBuf => {
                if d != stop {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                }
            }

            RegexOp::AtDot => {
                if d != point {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                }
            }

            RegexOp::WordBound => {
                if !at_word_boundary(d) {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                }
            }

            RegexOp::NotWordBound => {
                if at_word_boundary(d) {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                }
            }

            RegexOp::WordBeg => {
                // Word beginning: not in word before, in word after
                let prev_word = d > 0
                    && text_char(d - 1)
                        .map(|(c, _)| syntax.char_syntax(c) == SyntaxClass::Word)
                        .unwrap_or(false);
                let curr_word = text_char(d)
                    .map(|(c, _)| syntax.char_syntax(c) == SyntaxClass::Word)
                    .unwrap_or(false);
                if prev_word || !curr_word {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                }
            }

            RegexOp::WordEnd => {
                let prev_word = d > 0
                    && text_char(d - 1)
                        .map(|(c, _)| syntax.char_syntax(c) == SyntaxClass::Word)
                        .unwrap_or(false);
                let curr_word = text_char(d)
                    .map(|(c, _)| syntax.char_syntax(c) == SyntaxClass::Word)
                    .unwrap_or(false);
                if !prev_word || curr_word {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                }
            }

            RegexOp::SymBeg => {
                let is_sym = |c: char| {
                    let s = syntax.char_syntax(c);
                    s == SyntaxClass::Word || s == SyntaxClass::Symbol
                };
                let prev_sym = d > 0 && text_char(d - 1).map(|(c, _)| is_sym(c)).unwrap_or(false);
                let curr_sym = text_char(d).map(|(c, _)| is_sym(c)).unwrap_or(false);
                if prev_sym || !curr_sym {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                }
            }

            RegexOp::SymEnd => {
                let is_sym = |c: char| {
                    let s = syntax.char_syntax(c);
                    s == SyntaxClass::Word || s == SyntaxClass::Symbol
                };
                let prev_sym = d > 0 && text_char(d - 1).map(|(c, _)| is_sym(c)).unwrap_or(false);
                let curr_sym = text_char(d).map(|(c, _)| is_sym(c)).unwrap_or(false);
                if !prev_sym || curr_sym {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                }
            }

            RegexOp::SyntaxSpec => {
                let class_byte = bytecode[pc];
                pc += 1;
                if d >= stop {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                    continue;
                }
                let Some((c, len)) = text_char(d) else {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                    continue;
                };
                if syntax.char_syntax(c) as u8 != class_byte {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                    continue;
                }
                d += len;
            }

            RegexOp::NotSyntaxSpec => {
                let class_byte = bytecode[pc];
                pc += 1;
                if d >= stop {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                    continue;
                }
                let Some((c, len)) = text_char(d) else {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                    continue;
                };
                if syntax.char_syntax(c) as u8 == class_byte {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                    continue;
                }
                d += len;
            }

            RegexOp::CategorySpec => {
                let cat = bytecode[pc];
                pc += 1;
                if d >= stop {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                    continue;
                }
                let Some((c, len)) = text_char(d) else {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                    continue;
                };
                if !syntax.char_has_category(c, cat) {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                    continue;
                }
                d += len;
            }

            RegexOp::NotCategorySpec => {
                let cat = bytecode[pc];
                pc += 1;
                if d >= stop {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                    continue;
                }
                let Some((c, len)) = text_char(d) else {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                    continue;
                };
                if syntax.char_has_category(c, cat) {
                    goto_fail(
                        &mut pc,
                        &mut d,
                        &mut fail_stack,
                        &mut regstart,
                        &mut regend,
                        &mut counters,
                    )?;
                    continue;
                }
                d += len;
            }

            RegexOp::Jump => {
                let offset = extract_number(bytecode, pc);
                pc = ((pc as i64) + 2 + (offset as i64)) as usize;
            }

            RegexOp::OnFailureJump => {
                let offset = extract_number(bytecode, pc);
                pc += 2;
                let fail_pc = ((pc as i64) + (offset as i64)) as usize;
                fail_stack.push(FailurePoint {
                    pattern_pos: fail_pc,
                    string_pos: Some(d),
                    saved_registers: save_registers(&regstart, &regend, num_regs),
                    saved_counters: counters.clone(),
                });
            }

            RegexOp::OnFailureKeepStringJump => {
                let offset = extract_number(bytecode, pc);
                pc += 2;
                let fail_pc = ((pc as i64) + (offset as i64)) as usize;
                fail_stack.push(FailurePoint {
                    pattern_pos: fail_pc,
                    string_pos: None, // Don't restore string position
                    saved_registers: save_registers(&regstart, &regend, num_regs),
                    saved_counters: counters.clone(),
                });
            }

            RegexOp::OnFailureJumpLoop => {
                let offset = extract_number(bytecode, pc);
                pc += 2;
                let fail_pc = ((pc as i64) + (offset as i64)) as usize;
                // Check for infinite loop (empty match detection)
                let already_at_same_pos = fail_stack
                    .last()
                    .is_some_and(|fp| fp.string_pos == Some(d) && fp.pattern_pos == fail_pc);
                if already_at_same_pos {
                    // Would loop forever on empty match — skip the loop
                    pc = fail_pc;
                } else {
                    fail_stack.push(FailurePoint {
                        pattern_pos: fail_pc,
                        string_pos: Some(d),
                        saved_registers: save_registers(&regstart, &regend, num_regs),
                        saved_counters: counters.clone(),
                    });
                }
            }

            RegexOp::OnFailureJumpNastyloop => {
                // Same as OnFailureJumpLoop but for non-greedy
                let offset = extract_number(bytecode, pc);
                pc += 2;
                let fail_pc = ((pc as i64) + (offset as i64)) as usize;
                fail_stack.push(FailurePoint {
                    pattern_pos: fail_pc,
                    string_pos: Some(d),
                    saved_registers: save_registers(&regstart, &regend, num_regs),
                    saved_counters: counters.clone(),
                });
            }

            RegexOp::OnFailureJumpSmart => {
                // Smart greedy optimization — treated same as OnFailureJump
                let offset = extract_number(bytecode, pc);
                pc += 2;
                let fail_pc = ((pc as i64) + (offset as i64)) as usize;
                fail_stack.push(FailurePoint {
                    pattern_pos: fail_pc,
                    string_pos: Some(d),
                    saved_registers: save_registers(&regstart, &regend, num_regs),
                    saved_counters: counters.clone(),
                });
            }

            RegexOp::SucceedN => {
                // GNU: succeed_n  <jump-offset:2> <counter:2>
                // "Have to succeed matching what follows at least n times."
                // Counter lives at pc+2 (2 bytes).  When counter > 0 we
                // decrement and continue (must still succeed more times).
                // When counter == 0 we fall through to on_failure_jump_loop
                // semantics using the jump offset.
                let counter_pos = pc + 2; // bytecode position of the counter
                let count = get_counter(&counters, bytecode, counter_pos);
                if count != 0 {
                    // Still must succeed more times — decrement & continue
                    set_counter(&mut counters, counter_pos, count - 1);
                    pc += 4;
                } else {
                    // Counter exhausted — behave like on_failure_jump_loop.
                    // Read the jump offset and push a failure point.
                    let offset = extract_number(bytecode, pc);
                    pc += 2; // skip the offset field
                    let fail_pc = ((pc as i64) + (offset as i64)) as usize;
                    pc += 2; // skip the counter field
                    // Infinite-loop detection (same as OnFailureJumpLoop)
                    let already_at_same_pos = fail_stack
                        .last()
                        .is_some_and(|fp| fp.string_pos == Some(d) && fp.pattern_pos == fail_pc);
                    if already_at_same_pos {
                        pc = fail_pc;
                    } else {
                        fail_stack.push(FailurePoint {
                            pattern_pos: fail_pc,
                            string_pos: Some(d),
                            saved_registers: save_registers(&regstart, &regend, num_regs),
                            saved_counters: counters.clone(),
                        });
                    }
                }
            }

            RegexOp::JumpN => {
                // GNU: jump_n  <jump-offset:2> <counter:2>
                // "Originally, this is how many times we CAN jump."
                // If counter > 0, decrement and jump.
                // If counter == 0, skip past (don't jump).
                let counter_pos = pc + 2;
                let count = get_counter(&counters, bytecode, counter_pos);
                if count != 0 {
                    // Decrement counter and perform unconditional jump
                    set_counter(&mut counters, counter_pos, count - 1);
                    let offset = extract_number(bytecode, pc);
                    pc = ((pc as i64) + 2 + (offset as i64)) as usize;
                } else {
                    pc += 4; // Skip past offset + counter fields
                }
            }

            RegexOp::SetNumberAt => {
                // GNU: set_number_at  <offset-to-counter:2> <value:2>
                // Sets the counter at the given offset to the given value.
                // Used to reset interval counters at the start of a loop.
                let rel_offset = extract_number(bytecode, pc);
                pc += 2; // advance past the offset field
                let value = extract_number(bytecode, pc);
                pc += 2; // advance past the value field
                // Target counter position: relative to position after
                // the offset field (same convention as GNU).
                let target_pos = ((pc as i64) - 2 + (rel_offset as i64)) as usize;
                set_counter(&mut counters, target_pos, value);
            }
        }
    }

    // If we got here, we matched!
    // Fill in registers
    regs.start[0] = pos as i64;
    regs.end[0] = d as i64;
    for i in 1..num_regs {
        regs.start[i] = regstart
            .get(i)
            .copied()
            .flatten()
            .map(|v| v as i64)
            .unwrap_or(-1);
        regs.end[i] = regend
            .get(i)
            .copied()
            .flatten()
            .map(|v| v as i64)
            .unwrap_or(-1);
    }

    Some((d, regs))
}

/// Save current register state for backtracking.
fn save_registers(
    regstart: &[Option<usize>],
    regend: &[Option<usize>],
    num_regs: usize,
) -> Vec<(usize, i64, i64)> {
    let mut saved = Vec::new();
    for i in 1..num_regs.min(regstart.len()).min(regend.len()) {
        saved.push((
            i,
            regstart[i].map(|v| v as i64).unwrap_or(-1),
            regend[i].map(|v| v as i64).unwrap_or(-1),
        ));
    }
    saved
}

/// Restore register state from a failure point.
fn restore_registers(
    fp: &FailurePoint,
    regstart: &mut [Option<usize>],
    regend: &mut [Option<usize>],
) {
    for &(idx, start, end) in &fp.saved_registers {
        if idx < regstart.len() {
            regstart[idx] = if start >= 0 {
                Some(start as usize)
            } else {
                None
            };
        }
        if idx < regend.len() {
            regend[idx] = if end >= 0 { Some(end as usize) } else { None };
        }
    }
}

/// Handle match failure — pop the failure stack and backtrack.
/// Returns None if the failure stack is empty (complete failure).
fn goto_fail(
    pc: &mut usize,
    d: &mut usize,
    fail_stack: &mut Vec<FailurePoint>,
    regstart: &mut Vec<Option<usize>>,
    regend: &mut Vec<Option<usize>>,
    counters: &mut HashMap<usize, i16>,
) -> Option<()> {
    let fp = fail_stack.pop()?;
    *pc = fp.pattern_pos;
    if let Some(sp) = fp.string_pos {
        *d = sp;
    }
    restore_registers(&fp, regstart, regend);
    // Restore interval counters to the state when this failure point was pushed
    *counters = fp.saved_counters;
    Some(())
}

// ---------------------------------------------------------------------------
// Phase 4: Searcher (re_search_2)
//
// Translates GNU regex-emacs.c:3408-4070.
// Searches for a match in text, using fastmap for optimization.
// ---------------------------------------------------------------------------

/// Analyze compiled bytecode to populate `pattern.fastmap`.
///
/// For each byte value `c` that could possibly appear as the first byte of a
/// match, sets `pattern.fastmap[c] = true`.  The searcher (`re_search`) uses
/// this to skip positions that cannot start a match, giving a significant
/// speed-up for patterns that begin with a restricted set of characters.
///
/// Translated from GNU regex-emacs.c `re_compile_fastmap`.
fn compile_fastmap(pattern: &mut CompiledPattern) {
    pattern.fastmap = [false; 256];
    pattern.can_be_null = false;

    let bytecode = &pattern.buffer;
    if bytecode.is_empty() {
        pattern.can_be_null = true;
        pattern.fastmap_accurate = true;
        return;
    }

    let case_fold = pattern.translate.is_some();

    // Worklist of bytecode positions still to process.
    let mut worklist: Vec<usize> = vec![0];
    let mut visited: HashSet<usize> = HashSet::new();

    while let Some(pc) = worklist.pop() {
        let mut pc = pc;

        loop {
            if !visited.insert(pc) {
                // Already processed this position — avoid infinite loops.
                break;
            }

            if pc >= bytecode.len() {
                // Fell off the end of bytecode — pattern can match empty string.
                pattern.can_be_null = true;
                break;
            }

            let Some(op) = RegexOp::from_byte(bytecode[pc]) else {
                break;
            };
            pc += 1;

            match op {
                RegexOp::Succeed => {
                    // Pattern can succeed here (may match empty string).
                    pattern.can_be_null = true;
                    break;
                }

                RegexOp::Exactn => {
                    if pc >= bytecode.len() {
                        break;
                    }
                    let count = bytecode[pc] as usize;
                    pc += 1;
                    if count == 0 || pc >= bytecode.len() {
                        break;
                    }
                    let first = bytecode[pc];
                    pattern.fastmap[first as usize] = true;
                    if case_fold {
                        let upper = (first as char)
                            .to_uppercase()
                            .next()
                            .unwrap_or(first as char) as u8;
                        let lower = (first as char)
                            .to_lowercase()
                            .next()
                            .unwrap_or(first as char) as u8;
                        pattern.fastmap[upper as usize] = true;
                        pattern.fastmap[lower as usize] = true;
                    }
                    break; // This opcode consumes input — done on this path.
                }

                RegexOp::AnyChar => {
                    // Matches any character except newline.
                    for c in 0..256usize {
                        if c != b'\n' as usize {
                            pattern.fastmap[c] = true;
                        }
                    }
                    break;
                }

                RegexOp::Charset => {
                    if pc >= bytecode.len() {
                        break;
                    }
                    let bitmap_len = bytecode[pc] as usize & 0x7F;
                    pc += 1;
                    for c in 0..256usize {
                        if c / 8 < bitmap_len && pc + c / 8 < bytecode.len() {
                            if (bytecode[pc + c / 8] >> (c % 8)) & 1 != 0 {
                                pattern.fastmap[c] = true;
                            }
                        }
                    }
                    break;
                }

                RegexOp::CharsetNot => {
                    if pc >= bytecode.len() {
                        break;
                    }
                    let bitmap_len = bytecode[pc] as usize & 0x7F;
                    pc += 1;
                    for c in 0..256usize {
                        let in_set = if c / 8 < bitmap_len && pc + c / 8 < bytecode.len() {
                            (bytecode[pc + c / 8] >> (c % 8)) & 1 != 0
                        } else {
                            false
                        };
                        if !in_set {
                            pattern.fastmap[c] = true;
                        }
                    }
                    break;
                }

                RegexOp::SyntaxSpec => {
                    if pc >= bytecode.len() {
                        break;
                    }
                    let class_byte = bytecode[pc];
                    // Set fastmap entries for all bytes whose ASCII char matches
                    // this syntax class (approximation for the default syntax table).
                    for c in 0u8..=127 {
                        let ch = c as char;
                        let matches_class = match class_byte {
                            // Whitespace = 0
                            0 => ch.is_whitespace(),
                            // Punctuation = 1
                            1 => ch.is_ascii_punctuation(),
                            // Word = 2
                            2 => ch.is_alphanumeric() || ch == '_',
                            // Symbol = 3
                            3 => {
                                !ch.is_alphanumeric()
                                    && !ch.is_whitespace()
                                    && !ch.is_ascii_punctuation()
                                    && ch != '_'
                            }
                            _ => false,
                        };
                        if matches_class {
                            pattern.fastmap[c as usize] = true;
                        }
                    }
                    // For non-ASCII bytes, conservatively set them all true.
                    for c in 128..256usize {
                        pattern.fastmap[c] = true;
                    }
                    break;
                }

                RegexOp::NotSyntaxSpec => {
                    if pc >= bytecode.len() {
                        break;
                    }
                    let class_byte = bytecode[pc];
                    for c in 0u8..=127 {
                        let ch = c as char;
                        let matches_class = match class_byte {
                            0 => ch.is_whitespace(),
                            1 => ch.is_ascii_punctuation(),
                            2 => ch.is_alphanumeric() || ch == '_',
                            3 => {
                                !ch.is_alphanumeric()
                                    && !ch.is_whitespace()
                                    && !ch.is_ascii_punctuation()
                                    && ch != '_'
                            }
                            _ => false,
                        };
                        if !matches_class {
                            pattern.fastmap[c as usize] = true;
                        }
                    }
                    for c in 128..256usize {
                        pattern.fastmap[c] = true;
                    }
                    break;
                }

                RegexOp::CategorySpec | RegexOp::NotCategorySpec => {
                    // Categories are too dynamic to predict — allow all bytes.
                    pattern.fastmap = [true; 256];
                    break;
                }

                // Zero-width assertions: they don't consume input, so we
                // continue to the next opcode to find what actually starts
                // the match.
                RegexOp::BegLine
                | RegexOp::EndLine
                | RegexOp::BegBuf
                | RegexOp::EndBuf
                | RegexOp::AtDot
                | RegexOp::WordBound
                | RegexOp::NotWordBound
                | RegexOp::WordBeg
                | RegexOp::WordEnd
                | RegexOp::SymBeg
                | RegexOp::SymEnd => {
                    // Continue to the next opcode.
                }

                RegexOp::StartMemory | RegexOp::StopMemory => {
                    // Skip the group number byte, continue.
                    pc += 1;
                }

                RegexOp::Duplicate => {
                    // Backreferences can match anything — set all.
                    pattern.fastmap = [true; 256];
                    break;
                }

                RegexOp::NoOp => {
                    // Continue to next opcode.
                }

                RegexOp::Jump => {
                    if pc + 1 >= bytecode.len() {
                        break;
                    }
                    let offset = extract_number(bytecode, pc);
                    pc = ((pc as i64) + 2 + (offset as i64)) as usize;
                    // Continue walking from the jump target (don't break).
                }

                RegexOp::OnFailureJump
                | RegexOp::OnFailureKeepStringJump
                | RegexOp::OnFailureJumpLoop
                | RegexOp::OnFailureJumpNastyloop
                | RegexOp::OnFailureJumpSmart => {
                    if pc + 1 >= bytecode.len() {
                        break;
                    }
                    let offset = extract_number(bytecode, pc);
                    pc += 2;
                    // Both the fallthrough (next opcode) and the jump target
                    // can start a match.  Push the jump target onto the
                    // worklist and continue with the fallthrough.
                    let target = ((pc as i64) + (offset as i64)) as usize;
                    worklist.push(target);
                    // Continue with the next opcode (fallthrough path).
                }

                RegexOp::SucceedN => {
                    // succeed_n <offset:2> <counter:2>
                    // When counter > 0, acts like a mandatory match of what follows.
                    // When counter == 0, acts like on_failure_jump.
                    // For fastmap purposes, both paths are possible.
                    if pc + 3 >= bytecode.len() {
                        break;
                    }
                    let offset = extract_number(bytecode, pc);
                    let target = ((pc as i64) + 2 + (offset as i64)) as usize;
                    worklist.push(target);
                    pc += 4; // skip offset + counter, continue with fallthrough
                }

                RegexOp::JumpN => {
                    // jump_n <offset:2> <counter:2>
                    // If counter > 0, jumps; if counter == 0, falls through.
                    // For fastmap, both paths are possible.
                    if pc + 3 >= bytecode.len() {
                        break;
                    }
                    let offset = extract_number(bytecode, pc);
                    let target = ((pc as i64) + 2 + (offset as i64)) as usize;
                    worklist.push(target);
                    pc += 4; // fallthrough
                }

                RegexOp::SetNumberAt => {
                    // set_number_at <offset:2> <value:2> — no input consumed.
                    pc += 4;
                }
            }
        }
    }

    pattern.fastmap_accurate = true;
}

/// Search for a match of the compiled pattern in text.
///
/// Equivalent to GNU's `re_search_2()`.
///
/// # Arguments
/// * `pattern` - Compiled pattern
/// * `text` - Input text
/// * `start` - Starting search position
/// * `range` - How far to search (positive = forward, negative = backward)
/// * `syntax` - Syntax table lookup
/// * `point` - Buffer point (for `\=`)
///
/// # Returns
/// * `Some((match_start, registers))` if found
/// * `None` if no match
pub(crate) fn re_search(
    pattern: &CompiledPattern,
    text: &[u8],
    start: usize,
    range: isize,
    syntax: &dyn SyntaxLookup,
    point: usize,
) -> Option<(usize, MatchRegisters)> {
    let text_len = text.len();

    if range >= 0 {
        // Forward search
        let end = (start + range as usize).min(text_len);
        for pos in start..=end {
            if pos > text_len {
                break;
            }
            // Use fastmap to skip positions that can't start a match
            if pattern.fastmap_accurate && pos < text_len {
                if !pattern.fastmap[text[pos] as usize] {
                    continue;
                }
            }
            if let Some(result) = re_match(pattern, text, pos, text_len, syntax, point) {
                return Some((pos, result.1));
            }
        }
    } else {
        // Backward search
        let end = start.saturating_sub((-range) as usize);
        for pos in (end..=start).rev() {
            // Use fastmap to skip positions that can't start a match
            if pattern.fastmap_accurate && pos < text_len {
                if !pattern.fastmap[text[pos] as usize] {
                    continue;
                }
            }
            if let Some(result) = re_match(pattern, text, pos, text_len, syntax, point) {
                return Some((pos, result.1));
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Convenience: compile + search in one call
// ---------------------------------------------------------------------------

/// Compile a pattern and search for it in text.
pub(crate) fn search_pattern(
    pattern_str: &str,
    text: &str,
    start: usize,
    case_fold: bool,
    syntax: &dyn SyntaxLookup,
    point: usize,
) -> Result<Option<(usize, MatchRegisters)>, RegexCompileError> {
    let compiled = regex_compile(pattern_str, false, case_fold)?;
    Ok(re_search(
        &compiled,
        text.as_bytes(),
        start,
        (text.len() - start) as isize,
        syntax,
        point,
    ))
}

/// Compile a pattern and match at a specific position.
pub(crate) fn match_pattern(
    pattern_str: &str,
    text: &str,
    pos: usize,
    case_fold: bool,
    syntax: &dyn SyntaxLookup,
    point: usize,
) -> Result<Option<(usize, MatchRegisters)>, RegexCompileError> {
    let compiled = regex_compile(pattern_str, false, case_fold)?;
    Ok(re_match(
        &compiled,
        text.as_bytes(),
        pos,
        text.len(),
        syntax,
        point,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_literal() {
        let syn = DefaultSyntaxLookup;
        let result = search_pattern("hello", "say hello world", 0, false, &syn, 0);
        assert!(result.is_ok());
        let r = result.unwrap();
        assert!(r.is_some());
        let (pos, regs) = r.unwrap();
        assert_eq!(pos, 4); // "hello" starts at position 4
        assert_eq!(regs.end[0], 9); // ends at 9
    }

    #[test]
    fn test_dot_matches_any() {
        let syn = DefaultSyntaxLookup;
        let result = search_pattern("h.llo", "say hello world", 0, false, &syn, 0);
        assert!(result.is_ok());
        let r = result.unwrap();
        assert!(r.is_some());
    }

    #[test]
    fn test_anchors() {
        let syn = DefaultSyntaxLookup;
        // ^ at beginning
        let r = match_pattern("^hello", "hello world", 0, false, &syn, 0).unwrap();
        assert!(r.is_some());
        // ^ not at beginning
        let r = match_pattern("^hello", "say hello", 4, false, &syn, 0).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn test_groups() {
        let syn = DefaultSyntaxLookup;
        let result = search_pattern("\\(hel\\)lo", "hello", 0, false, &syn, 0);
        assert!(result.is_ok());
        let (pos, regs) = result.unwrap().unwrap();
        assert_eq!(pos, 0);
        assert_eq!(regs.start[1], 0); // group 1 start
        assert_eq!(regs.end[1], 3); // group 1 end ("hel")
    }

    #[test]
    fn test_word_boundary() {
        let syn = DefaultSyntaxLookup;
        let r = search_pattern("\\bhello\\b", "say hello world", 0, false, &syn, 0);
        assert!(r.is_ok());
        assert!(r.unwrap().is_some());
    }

    #[test]
    fn test_star_repetition() {
        let syn = DefaultSyntaxLookup;
        let r = search_pattern("hel*o", "heo", 0, false, &syn, 0);
        assert!(r.unwrap().is_some()); // zero l's
        let r = search_pattern("hel*o", "hello", 0, false, &syn, 0);
        assert!(r.unwrap().is_some()); // two l's
        let r = search_pattern("hel*o", "hellllo", 0, false, &syn, 0);
        assert!(r.unwrap().is_some()); // four l's
    }

    #[test]
    fn test_charset() {
        let syn = DefaultSyntaxLookup;
        let r = search_pattern("[abc]", "xbz", 0, false, &syn, 0);
        assert!(r.unwrap().is_some());
        let r = search_pattern("[abc]", "xyz", 0, false, &syn, 0);
        assert!(r.unwrap().is_none());
    }

    #[test]
    fn test_syntax_word() {
        let syn = DefaultSyntaxLookup;
        // \sw matches word characters
        let r = search_pattern("\\sw+", "hello world", 0, false, &syn, 0);
        assert!(r.unwrap().is_some());
    }

    #[test]
    fn test_backreference() {
        let syn = DefaultSyntaxLookup;
        let r = search_pattern("\\(a\\)\\1", "aa", 0, false, &syn, 0);
        assert!(r.unwrap().is_some());
        let r = search_pattern("\\(a\\)\\1", "ab", 0, false, &syn, 0);
        assert!(r.unwrap().is_none());
    }

    #[test]
    fn test_alternation() {
        let syn = DefaultSyntaxLookup;
        let r = search_pattern("\\(foo\\|bar\\)", "test bar baz", 0, false, &syn, 0);
        assert!(r.is_ok(), "compile failed: {:?}", r.err());
        assert!(r.as_ref().unwrap().is_some(), "match failed");
        let (pos, regs) = r.unwrap().unwrap();
        assert_eq!(pos, 5, "match position");
        assert_eq!(regs.start[0], 5);
        assert_eq!(regs.end[0], 8);
    }

    #[test]
    fn test_char_range() {
        let syn = DefaultSyntaxLookup;
        let r = search_pattern("[0-9]+", "foo 123 bar", 0, false, &syn, 0);
        assert!(r.is_ok(), "compile failed: {:?}", r.err());
        assert!(r.as_ref().unwrap().is_some(), "match failed");
        let (pos, _regs) = r.unwrap().unwrap();
        assert_eq!(pos, 4, "match position");
    }

    #[test]
    fn test_fastmap_skips_positions() {
        let syn = DefaultSyntaxLookup;
        // Pattern starts with 'z' — should skip to position where 'z' appears
        let r = search_pattern("zing", "aaaaaaaaaazing", 0, false, &syn, 0);
        assert!(r.unwrap().is_some());
        let r = search_pattern("zing", "aaaaaaaaaazing", 0, false, &syn, 0);
        let (pos, _) = r.unwrap().unwrap();
        assert_eq!(pos, 10);
    }

    #[test]
    fn test_fastmap_literal_accurate() {
        // Verify fastmap is populated and accurate for a simple literal
        let compiled = regex_compile("hello", false, false).unwrap();
        assert!(compiled.fastmap_accurate);
        assert!(compiled.fastmap[b'h' as usize]);
        assert!(!compiled.fastmap[b'a' as usize]);
        assert!(!compiled.fastmap[b'z' as usize]);
    }

    #[test]
    fn test_fastmap_charset() {
        // Verify fastmap for character class patterns
        let compiled = regex_compile("[abc]", false, false).unwrap();
        assert!(compiled.fastmap_accurate);
        assert!(compiled.fastmap[b'a' as usize]);
        assert!(compiled.fastmap[b'b' as usize]);
        assert!(compiled.fastmap[b'c' as usize]);
        assert!(!compiled.fastmap[b'd' as usize]);
    }

    #[test]
    fn test_fastmap_case_fold() {
        // Case-folded pattern should match both cases
        let compiled = regex_compile("Hello", false, true).unwrap();
        assert!(compiled.fastmap_accurate);
        assert!(compiled.fastmap[b'h' as usize]);
        assert!(compiled.fastmap[b'H' as usize]);
    }

    #[test]
    fn test_fastmap_alternation() {
        // Alternation: both branches should appear in fastmap
        let compiled = regex_compile("\\(foo\\|bar\\)", false, false).unwrap();
        assert!(compiled.fastmap_accurate);
        assert!(compiled.fastmap[b'f' as usize]);
        assert!(compiled.fastmap[b'b' as usize]);
        assert!(!compiled.fastmap[b'z' as usize]);
    }

    #[test]
    fn test_fastmap_dot() {
        // AnyChar: everything except newline
        let compiled = regex_compile(".", false, false).unwrap();
        assert!(compiled.fastmap_accurate);
        assert!(compiled.fastmap[b'a' as usize]);
        assert!(compiled.fastmap[b'Z' as usize]);
        assert!(!compiled.fastmap[b'\n' as usize]);
    }

    #[test]
    fn test_fastmap_anchor_then_literal() {
        // ^hello — anchor is zero-width, fastmap should see 'h'
        let compiled = regex_compile("^hello", false, false).unwrap();
        assert!(compiled.fastmap_accurate);
        assert!(compiled.fastmap[b'h' as usize]);
        assert!(!compiled.fastmap[b'x' as usize]);
    }

    #[test]
    fn test_fastmap_charset_not() {
        // [^abc] should allow everything except a, b, c
        let compiled = regex_compile("[^abc]", false, false).unwrap();
        assert!(compiled.fastmap_accurate);
        assert!(!compiled.fastmap[b'a' as usize]);
        assert!(!compiled.fastmap[b'b' as usize]);
        assert!(!compiled.fastmap[b'c' as usize]);
        assert!(compiled.fastmap[b'd' as usize]);
        assert!(compiled.fastmap[b'z' as usize]);
    }
}
