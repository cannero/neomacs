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

use crate::emacs_core::{emacs_char, syntax::SyntaxClass};

// ---------------------------------------------------------------------------
// Phase 1: Opcodes and Data Structures
// ---------------------------------------------------------------------------

/// Bytecode opcodes for the compiled regex pattern.
///
/// Translated from `re_opcode_t` enum in regex-emacs.c (lines 202-337).
/// Each opcode may be followed by argument bytes in the bytecode buffer.
/// Bytecode opcodes for the compiled regex pattern.
///
/// **Strict GNU parity**: the numeric values here mirror
/// `enum re_opcode_t` in GNU `src/regex-emacs.c:202-337` exactly.
/// A compiled pattern emitted by our compiler is byte-compatible with
/// the same pattern emitted by GNU's compiler — every opcode occupies
/// the same numeric slot, so bytecode dumps can be compared directly
/// during debugging and future external tools can read either
/// without a translation layer.
///
/// The one-byte form we emit via `<op> as u8` is the same as GNU's
/// `BUF_COMPILED[pc++]` byte. **Do not reorder without updating the
/// GNU reference at the top of this file.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum RegexOp {
    /// No operation (padding/alignment). GNU `no_op` = 0.
    NoOp = 0,

    /// Succeed immediately — no more backtracking. GNU `succeed` = 1.
    Succeed = 1,

    /// Match N exact bytes.  Followed by one byte N, then N literal
    /// bytes. GNU `exactn` = 2.
    Exactn = 2,

    /// Match any character (except newline in some modes).
    /// GNU `anychar` = 3.
    AnyChar = 3,

    /// Match character in bitmap set. Same byte layout as GNU
    /// `charset` = 4:
    /// - 1 byte: bitmap length (low 7 bits), high bit = has range table
    /// - N bytes: bitmap (bit per character, low-bit-first)
    /// - Optional range table for multibyte characters
    Charset = 4,

    /// Match character NOT in bitmap set.  Same format as `Charset`.
    /// GNU `charset_not` = 5.
    CharsetNot = 5,

    /// Start remembering text for group N.  Followed by 1 byte: group
    /// number. GNU `start_memory` = 6.
    StartMemory = 6,

    /// Stop remembering text for group N.  Followed by 1 byte: group
    /// number. GNU `stop_memory` = 7.
    StopMemory = 7,

    /// Match duplicate of group N (backreference \N).  Followed by
    /// 1 byte: group number. GNU `duplicate` = 8.
    Duplicate = 8,

    /// Fail unless at beginning of line (^). GNU `begline` = 9.
    BegLine = 9,

    /// Fail unless at end of line ($). GNU `endline` = 10.
    EndLine = 10,

    /// Succeed at beginning of buffer/string. `` \` ``.
    /// GNU `begbuf` = 11.
    BegBuf = 11,

    /// Succeed at end of buffer/string. `\'`.
    /// GNU `endbuf` = 12.
    EndBuf = 12,

    /// Unconditional jump.  Followed by 2-byte signed offset.
    /// GNU `jump` = 13.
    Jump = 13,

    /// Push failure point, then continue.  Followed by 2-byte signed
    /// offset. GNU `on_failure_jump` = 14.
    OnFailureJump = 14,

    /// Like `OnFailureJump` but doesn't restore string position on
    /// failure. GNU `on_failure_keep_string_jump` = 15.
    OnFailureKeepStringJump = 15,

    /// Like `OnFailureJump` but detects infinite empty-match loops.
    /// GNU `on_failure_jump_loop` = 16.
    OnFailureJumpLoop = 16,

    /// Like `OnFailureJumpLoop` but for non-greedy operators.
    /// GNU `on_failure_jump_nastyloop` = 17.
    OnFailureJumpNastyloop = 17,

    /// Smart jump for greedy `*` and `+`.  Analyzes loop to optimize.
    /// GNU `on_failure_jump_smart` = 18.
    OnFailureJumpSmart = 18,

    /// Match N times then jump on failure.  Followed by 2-byte offset
    /// + 2-byte count. GNU `succeed_n` = 19.
    SucceedN = 19,

    /// Jump N times then fail.  Followed by 2-byte offset + 2-byte
    /// count. GNU `jump_n` = 20.
    JumpN = 20,

    /// Set counter at offset.  Followed by 2-byte offset + 2-byte
    /// value. GNU `set_number_at` = 21.
    SetNumberAt = 21,

    /// Succeed at word beginning (syntax-table aware).  `\<`.
    /// GNU `wordbeg` = 22.
    WordBeg = 22,

    /// Succeed at word end (syntax-table aware).  `\>`.
    /// GNU `wordend` = 23.
    WordEnd = 23,

    /// Succeed at word boundary (syntax-table aware).  `\b`.
    /// GNU `wordbound` = 24.
    WordBound = 24,

    /// Succeed at non-word boundary (syntax-table aware).  `\B`.
    /// GNU `notwordbound` = 25.
    NotWordBound = 25,

    /// Succeed at symbol beginning (syntax-table aware).  `\_<`.
    /// GNU `symbeg` = 26.
    SymBeg = 26,

    /// Succeed at symbol end (syntax-table aware).  `\_>`.
    /// GNU `symend` = 27.
    SymEnd = 27,

    /// Match character with syntax class C.  Followed by 1 byte:
    /// syntax code.  `\sC`. GNU `syntaxspec` = 28.
    SyntaxSpec = 28,

    /// Match character without syntax class C.  Followed by 1 byte.
    /// `\SC`. GNU `notsyntaxspec` = 29.
    NotSyntaxSpec = 29,

    /// Succeed if at point.  `\=`. GNU `at_dot` = 30.
    AtDot = 30,

    /// Match character with category C.  Followed by 1 byte: category
    /// code.  `\cC`. GNU `categoryspec` = 31.
    CategorySpec = 31,

    /// Match character without category C.  Followed by 1 byte.
    /// `\CC`. GNU `notcategoryspec` = 32.
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

    /// True if the source regexp string was multibyte.
    pub multibyte: bool,

    /// True if the current search target is multibyte.
    pub target_multibyte: bool,

    /// True if the pattern can match the empty string.
    pub can_be_null: bool,

    /// Character translation table for case-folding.
    /// Maps each character to its canonical form (e.g., 'A' → 'a').
    pub translate: Option<Vec<char>>,

    /// Multibyte (non-ASCII) character ranges for Charset/CharsetNot opcodes.
    /// Key = bytecode position of the Charset/CharsetNot opcode.
    /// Value = list of inclusive (start_char, end_char) character ranges.
    pub multibyte_charsets: HashMap<usize, Vec<(char, char)>>,

    /// Per-charset class flags for `[[:word:]]` and `[[:space:]]`,
    /// which GNU resolves at match time via `BUFFER_SYNTAX(c)` and
    /// not at compile time. Bit layout:
    ///
    ///   - bit 0 (`CHARSET_CLASS_BIT_WORD`)  → `[[:word:]]`
    ///   - bit 1 (`CHARSET_CLASS_BIT_SPACE`) → `[[:space:]]`
    ///
    /// At match time the charset matcher takes the union of the
    /// raw bitmap and the syntax-driven bits, so a per-buffer
    /// override of `_` to `Sword` extends `[[:word:]]` correctly.
    /// Mirrors GNU `regex-emacs.c:re_wctype_to_bit` (line 1635) +
    /// `execute_charset` (regex-emacs.c:3795-3802). See audit
    /// finding #8 in `drafts/regex-search-audit.md`.
    pub charset_class_bits: HashMap<usize, u8>,
}

/// Bit set on a `Charset`/`CharsetNot` opcode for `[[:word:]]`. The
/// matcher resolves it via the buffer syntax table at match time.
pub const CHARSET_CLASS_BIT_WORD: u8 = 1 << 0;
/// Bit set on a `Charset`/`CharsetNot` opcode for `[[:space:]]`.
pub const CHARSET_CLASS_BIT_SPACE: u8 = 1 << 1;

impl CompiledPattern {
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(256),
            re_nsub: 0,
            fastmap: [false; 256],
            fastmap_accurate: false,
            posix: false,
            multibyte: true,
            target_multibyte: true,
            can_be_null: false,
            translate: None,
            multibyte_charsets: HashMap::new(),
            charset_class_bits: HashMap::new(),
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

// SyntaxClass is imported from crate::emacs_core::syntax.

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
    let pattern = crate::heap_types::LispString::from_utf8(pattern);
    regex_compile_lisp(&pattern, posix, case_fold)
}

pub(crate) fn regex_compile_lisp(
    pattern: &crate::heap_types::LispString,
    posix: bool,
    case_fold: bool,
) -> Result<CompiledPattern, RegexCompileError> {
    let mut buf = CompiledPattern::new();
    buf.posix = posix;
    buf.multibyte = pattern.is_multibyte();
    buf.target_multibyte = pattern.is_multibyte();

    // Build case-fold translation table if needed.
    //
    // GNU `compile_pattern` (search.c:287-289) passes the buffer's
    // `case_canon_table` here, which is a full Unicode char-table
    // configurable per buffer via `(set-case-table ...)`. neomacs's
    // translate is a 256-entry `Vec<char>` populated from Rust's
    // `to_lowercase()` on each ASCII byte, which:
    //
    //   - covers ASCII A-Z → a-z exactly (the only common case),
    //   - is identity for the rest of the byte range (no
    //     buffer-specific case folding for Latin-1 or higher),
    //   - is unreachable for code points > 0xFF (the matcher's
    //     `tr()` short-circuits when `c >= 256`).
    //
    // Audit finding #5 in `drafts/regex-search-audit.md` flags this
    // as "translate table byte-only, no buffer case table". A
    // GNU-parity refactor would require:
    //
    //   1. A char-keyed translate table (HashMap<char, char> or a
    //      sparse char-table backed by the buffer's own case_canon).
    //   2. Threading the buffer's case_canon table down through
    //      `compile_search_pattern_with_posix`.
    //   3. Decoding the input as full UTF-8 chars in `RegexOp::Exactn`
    //      (instead of byte-by-byte) so non-ASCII chars can be
    //      case-folded against the pattern.
    //
    // That is the audit's Phase B Task 2.1 (1-2 days). Until it
    // lands, the byte-only table here is documented intentional; it
    // matches GNU exactly for ASCII letters and silently no-ops for
    // multibyte case folds.
    if case_fold {
        let mut table = Vec::with_capacity(256);
        for i in 0..256u32 {
            let c = char::from_u32(i).expect("0..=255 must be valid Unicode scalars");
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
                let pattern_multibyte = buf.multibyte;
                compile_charset(
                    pattern_bytes,
                    &mut p,
                    &mut buf,
                    case_fold,
                    pattern_multibyte,
                )?;
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
                            set_bitmap_bit(&mut buf.buffer, bitmap_start, ch, None);
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
                            set_bitmap_bit(&mut buf.buffer, bitmap_start, ch, None);
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

    // Emit final succeed — but only for non-POSIX patterns.
    //
    // GNU regex-emacs.c:2683-2686:
    //
    //     /* If we don't want backtracking, force success
    //        the first time we reach the end of the compiled pattern.  */
    //     if (!posix_backtracking)
    //       BUF_PUSH (succeed);
    //
    // When `posix_backtracking` is true the matcher must see the
    // natural "fell off the end of the bytecode" path so the POSIX
    // longest-match logic at regex-emacs.c:4272-4344 can run. Emitting
    // `succeed` unconditionally (as an earlier version of this file
    // did) made every pattern jump to `succeed_label`, bypassing the
    // longest-match code entirely.
    if !posix {
        emit_op!(RegexOp::Succeed);
    }

    // Populate the fastmap for search-time position skipping.
    compile_fastmap(&mut buf);

    Ok(buf)
}

// ---------------------------------------------------------------------------
// Compiler Helpers
// ---------------------------------------------------------------------------

/// Emit a literal character as part of an `exactn` sequence.
///
/// GNU `regex-emacs.c` applies `RE_TRANSLATE (translate, c)` before
/// buffering the char, so a pattern like `"C"` compiled with
/// case-fold on is stored in the bytecode as `'c'`. At match time the
/// buffer char is also `tr()`-translated, so both sides are
/// case-folded to the canonical (lowercase) form. Without the
/// translate-on-compile step here, the pattern byte stays as `'C'`
/// while the matched text byte becomes `'c'` and they fail to compare
/// equal.
fn goto_normal_char(
    c: u8,
    buf: &mut CompiledPattern,
    pending_exact: &mut Option<usize>,
    laststart: &mut Option<usize>,
) {
    let c = if let Some(table) = buf.translate.as_ref() {
        table[c as usize] as u32 as u8
    } else {
        c
    };

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

/// Decode one Emacs character from a pattern byte slice starting at `pos`.
///
/// Multibyte patterns use Emacs internal encoding; unibyte patterns map each
/// byte to a single character code directly.
fn decode_pattern_char(bytes: &[u8], pos: usize, multibyte: bool) -> Option<(u32, usize)> {
    if pos >= bytes.len() {
        return None;
    }
    if multibyte {
        Some(emacs_char::string_char(&bytes[pos..]))
    } else {
        Some((bytes[pos] as u32, 1))
    }
}

fn emacs_char_to_rust_char(code: u32) -> char {
    if emacs_char::char_byte8_p(code) {
        char::from(emacs_char::char_to_byte8(code))
    } else {
        char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER)
    }
}

/// Compile a character class `[...]` into charset bytecode.
fn compile_charset(
    pattern: &[u8],
    p: &mut usize,
    buf: &mut CompiledPattern,
    case_fold: bool,
    pattern_multibyte: bool,
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

    // Record the bytecode position of this charset opcode for the
    // multibyte_charsets map.
    let charset_opcode_pos = buf.buffer.len();
    buf.buffer.push(op as u8);
    let _bitmap_len_pos = buf.buffer.len();
    buf.buffer.push(32); // 256 bits = 32 bytes bitmap

    // Initialize 32-byte bitmap (256 bits, one per ASCII char)
    let bitmap_start = buf.buffer.len();
    buf.buffer.extend_from_slice(&[0u8; 32]);

    // Collect multibyte (non-ASCII) ranges for this charset.
    let mut mb_ranges: Vec<(char, char)> = Vec::new();

    // Bitmask of class flags for `[[:word:]]` / `[[:space:]]`. The
    // matcher checks these against the buffer syntax table at run
    // time so per-mode word/space definitions take effect.
    let mut class_bits: u8 = 0;

    // Special case: ] at start is literal
    let mut first = true;
    let mut last_char: Option<char> = None; // Track last single char for ranges

    while *p < plen {
        let b = pattern[*p];

        // Decode a full Emacs character from the pattern.
        let (c, clen) =
            decode_pattern_char(pattern, *p, pattern_multibyte).unwrap_or((b as u32, 1));
        *p += clen;

        if b == b']' && !first {
            break;
        }
        first = false;

        if b == b'-' && *p < plen && pattern[*p] != b']' {
            if let Some(range_start) = last_char {
                // Range: range_start - next_char
                let (range_end, rlen) = decode_pattern_char(pattern, *p, pattern_multibyte)
                    .unwrap_or((pattern[*p] as u32, 1));
                let range_end = emacs_char_to_rust_char(range_end);
                *p += rlen;
                let translate = buf.translate.as_deref();
                if range_start.is_ascii() && range_end.is_ascii() {
                    // Both ASCII — use the bitmap
                    for ch in (range_start as u8)..=(range_end as u8) {
                        set_bitmap_bit(&mut buf.buffer, bitmap_start, ch, translate);
                    }
                } else {
                    // At least one endpoint is non-ASCII.
                    // Put the ASCII portion in the bitmap and the rest in
                    // multibyte ranges.
                    let start_u32 = range_start as u32;
                    let end_u32 = range_end as u32;
                    // ASCII portion: codepoints <= 127
                    if start_u32 <= 127 {
                        let ascii_end = end_u32.min(127) as u8;
                        for ch in (start_u32 as u8)..=ascii_end {
                            set_bitmap_bit(&mut buf.buffer, bitmap_start, ch, translate);
                        }
                    }
                    // Multibyte portion: codepoints >= 128
                    let mb_start = if start_u32 >= 128 {
                        range_start
                    } else {
                        '\u{80}'
                    };
                    if end_u32 >= 128 {
                        add_multibyte_range(&mut mb_ranges, mb_start, range_end, case_fold);
                    }
                }
                last_char = None; // Range consumed
                continue;
            }
            // '-' at start or after a range → literal '-'
            set_bitmap_bit(
                &mut buf.buffer,
                bitmap_start,
                b'-',
                buf.translate.as_deref(),
            );
            last_char = Some('-');
            continue;
        }

        // GNU `regex-emacs.c` treats backslash as a literal character
        // inside a bracket expression: the parser at lines 2055-2140
        // has no escape handling in the `[...]` loop, so `[\w]` is
        // the character class containing `\` and `w`, and `\n` is
        // the class containing `\` and `n`. Users who want a word
        // character class inside a bracket expression must use the
        // POSIX class `[[:word:]]`.
        //
        // Earlier versions of this file carried a workaround that
        // expanded `\w`, `\W`, `\s-`, `\d`, `\D` to their out-of-
        // bracket meanings for the convenience of Rust callers, at
        // the cost of diverging from GNU. That divergence is audit
        // finding #10 in `drafts/regex-search-audit.md`; it has
        // been removed.
        if b == b'\\' {
            set_bitmap_bit(
                &mut buf.buffer,
                bitmap_start,
                b'\\',
                buf.translate.as_deref(),
            );
            last_char = Some('\\');
            continue;
        }

        // Handle POSIX classes [[:alpha:]], etc.
        if b == b'[' && *p < plen && pattern[*p] == b':' {
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
                apply_posix_class(
                    class_name,
                    &mut buf.buffer,
                    bitmap_start,
                    &mut mb_ranges,
                    &mut class_bits,
                    buf.translate.as_deref(),
                )?;
                last_char = None;
                continue;
            }
        }

        // Regular character
        let c = emacs_char_to_rust_char(c);
        if c.is_ascii() {
            set_bitmap_bit(
                &mut buf.buffer,
                bitmap_start,
                c as u8,
                buf.translate.as_deref(),
            );
        } else {
            // Non-ASCII character — add as a single-char range to multibyte list
            add_multibyte_range(&mut mb_ranges, c, c, case_fold);
        }
        last_char = Some(c);
    }

    // Store multibyte ranges if any were collected.
    if !mb_ranges.is_empty() {
        buf.multibyte_charsets.insert(charset_opcode_pos, mb_ranges);
    }

    // Record class flags so the matcher can consult the buffer
    // syntax table at run time for `[[:word:]]` and `[[:space:]]`.
    if class_bits != 0 {
        buf.charset_class_bits
            .insert(charset_opcode_pos, class_bits);
    }

    Ok(())
}

/// Add a multibyte character range, optionally expanding for case-folding.
fn add_multibyte_range(ranges: &mut Vec<(char, char)>, start: char, end: char, case_fold: bool) {
    ranges.push((start, end));
    if case_fold {
        // For case-folding, also add the upper/lower-case variants.
        // For single-char ranges, just add the case-folded char.
        // For multi-char ranges, this is a conservative approximation:
        // we add the lowercased and uppercased versions of the endpoints.
        if start == end {
            for variant in start.to_lowercase() {
                if variant != start {
                    ranges.push((variant, variant));
                }
            }
            for variant in start.to_uppercase() {
                if variant != start {
                    ranges.push((variant, variant));
                }
            }
        }
        // For multi-char ranges (start != end), the range itself should
        // cover the needed codepoints in most cases. We don't expand
        // further to avoid combinatorial explosion.
    }
}

/// Set a bit in the charset bitmap, translating through TRANSLATE if
/// supplied.
///
/// GNU `regex-emacs.c:SETUP_ASCII_RANGE` (lines 1397-1412) runs
/// `C1 = TRANSLATE(C0)` and then `SET_LIST_BIT(C1)` — it translates
/// each individual character as the range is walked and only stores
/// the translated bit. The matcher at regex-emacs.c:4553 does the
/// same TRANSLATE on the input character before the bitmap lookup,
/// so matches work out for any case-equivalent input.
///
/// Earlier versions of this function instead set the bit for both
/// the raw character and its Rust-derived upper/lower partners,
/// regardless of what translate table the pattern was compiled with.
/// That was audit finding #9 in `drafts/regex-search-audit.md`:
/// "charset case-fold range translation is eager (not lazy)". The
/// practical difference only shows up when Rust's Unicode case
/// mapping disagrees with Emacs's case canon table, but the GNU-
/// parity fix is to consult the same translate table both sides.
fn set_bitmap_bit(buffer: &mut Vec<u8>, bitmap_start: usize, c: u8, translate: Option<&[char]>) {
    let target = match translate {
        Some(table) => table
            .get(c as usize)
            .map(|ch| *ch as u32 as u8)
            .unwrap_or(c),
        None => c,
    };
    let byte_idx = bitmap_start + (target as usize / 8);
    let bit_idx = target as usize % 8;
    if byte_idx < buffer.len() {
        buffer[byte_idx] |= 1 << bit_idx;
    }
}

/// Apply a POSIX character class to the bitmap and multibyte range list.
///
/// Mirrors GNU `regex-emacs.c:re_wctype_parse` (lines 1525-1601) and
/// `re_iswctype` (lines 1603-1630). The full set of 17 classes is:
/// `alnum`, `alpha`, `blank`, `cntrl`, `digit`, `graph`, `lower`,
/// `print`, `punct`, `space`, `upper`, `xdigit`, `ascii`, `word`,
/// `nonascii`, `unibyte`, `multibyte`.
///
/// Semantics are taken from GNU's header macros at `regex-emacs.c:98-153`:
///
/// - `IS_REAL_ASCII(c)` is `c < 0x80`.
/// - `ISBLANK(c)` for ASCII is `c == ' ' || c == '\t'` only
///   (space and tab; NOT newline, formfeed, carriage return).
/// - `ISSPACE(c)` is `BUFFER_SYNTAX(c) == Swhitespace`; GNU's default
///   standard syntax table treats space, tab, newline, formfeed, and
///   carriage return as whitespace.
/// - `ISGRAPH(c)` for single-byte is `c > ' '` AND NOT in
///   `[0x7F..=0xA0]`.
/// - `ISPRINT(c)` for single-byte is `c >= ' '` AND NOT in
///   `[0x7F..=0x9F]`.
/// - `ISWORD(c)` is `BUFFER_SYNTAX(c) == Sword`; GNU's default treats
///   ASCII letters and digits as word constituents.
/// - `IS_REAL_ASCII(c)` covers 0x00..=0x7F for `ascii`.
/// - `nonascii` = `!IS_REAL_ASCII(c)` (>= 0x80).
/// - `unibyte` matches any single-byte character (bytes 0x00..=0xFF
///   in the bitmap, plus 8-bit raw byte chars).
/// - `multibyte` = `!ISUNIBYTE(c)`; matches multibyte characters
///   only (non-ASCII range via the multibyte range list).
///
/// Unknown class names mirror GNU's `RECC_ERROR` (regex-emacs.c:1600,
/// consumed as `REG_ECTYPE` at line 2071). We signal the same error
/// rather than silently ignoring the class as before.
///
/// Note: `word` and `space` semantically depend on the buffer's
/// syntax table (see audit finding #8 in
/// `drafts/regex-search-audit.md`). For now we bake in the standard
/// default; threading the per-buffer syntax table through charset
/// compilation is tracked as audit #8.
fn apply_posix_class(
    name: &str,
    buffer: &mut Vec<u8>,
    bitmap_start: usize,
    mb_ranges: &mut Vec<(char, char)>,
    class_bits: &mut u8,
    translate: Option<&[char]>,
) -> Result<(), RegexCompileError> {
    // GNU `regex-emacs.c:2100-2101` records `BIT_SPACE` and
    // `BIT_WORD` on the charset so the matcher can later consult
    // the buffer syntax table at run time. We do the same via
    // `class_bits` and the `[[:word:]]`/`[[:space:]]` arms below.
    // Audit finding #8 in `drafts/regex-search-audit.md`.
    match name {
        "word" => *class_bits |= CHARSET_CLASS_BIT_WORD,
        "space" => *class_bits |= CHARSET_CLASS_BIT_SPACE,
        _ => {}
    }
    // --- ASCII bitmap bits ------------------------------------------------
    //
    // Each class enumerates which bytes in 0x00..=0xFF should be
    // marked in the ASCII-extended bitmap. For multibyte classes
    // (`nonascii`, `multibyte`) the non-ASCII portion is added to
    // `mb_ranges` so the matcher's multibyte dispatch path catches it.
    let ascii_bytes: Vec<u8> = match name {
        "alpha" => (b'A'..=b'Z').chain(b'a'..=b'z').collect(),
        "digit" => (b'0'..=b'9').collect(),
        "alnum" => (b'A'..=b'Z')
            .chain(b'a'..=b'z')
            .chain(b'0'..=b'9')
            .collect(),
        // GNU ISSPACE uses BUFFER_SYNTAX; default standard-syntax-table
        // whitespace is space, tab, LF, CR, and FF. Vtab (0x0B) is NOT
        // whitespace in GNU's default. See syntax.c standard init.
        "space" => vec![b' ', b'\t', b'\n', b'\r', 0x0C],
        // GNU ISBLANK is strictly ASCII space and tab.
        "blank" => vec![b' ', b'\t'],
        "upper" => (b'A'..=b'Z').collect(),
        "lower" => (b'a'..=b'z').collect(),
        "punct" => b"!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~".to_vec(),
        // GNU ISPRINT single-byte: c >= ' ' and not in [0x7F..=0x9F].
        // That's 0x20..=0x7E and 0xA0..=0xFF.
        "print" => (0x20u8..=0x7E).chain(0xA0u8..=0xFF).collect(),
        // GNU ISGRAPH single-byte: c > ' ' and not in [0x7F..=0xA0].
        // That's 0x21..=0x7E and 0xA1..=0xFF.
        "graph" => (0x21u8..=0x7E).chain(0xA1u8..=0xFF).collect(),
        "cntrl" => (0x00u8..=0x1F).chain(std::iter::once(0x7F)).collect(),
        "xdigit" => (b'0'..=b'9')
            .chain(b'A'..=b'F')
            .chain(b'a'..=b'f')
            .collect(),
        "ascii" => (0x00u8..=0x7F).collect(),
        // GNU ISWORD(c) = BUFFER_SYNTAX(c) == Sword. Default standard
        // syntax table has ASCII letters and digits as word
        // constituents. Per-buffer syntax tables are audit #8.
        "word" => (b'A'..=b'Z')
            .chain(b'a'..=b'z')
            .chain(b'0'..=b'9')
            .collect(),
        // IS_REAL_ASCII(c) is c < 0x80, so nonascii matches 0x80..=0xFF
        // in the bitmap plus all non-ASCII multibyte characters.
        "nonascii" => (0x80u8..=0xFF).collect(),
        // ISUNIBYTE(c) = SINGLE_BYTE_CHAR_P(c); every byte 0..=0xFF
        // qualifies. No multibyte range (multibyte chars are NOT
        // unibyte by definition).
        "unibyte" => (0x00u8..=0xFF).collect(),
        // !ISUNIBYTE(c): only multibyte (non-ASCII) characters.
        // Nothing in the bitmap; everything in the multibyte range.
        "multibyte" => Vec::new(),
        // GNU `re_wctype_parse` returns RECC_ERROR (regex-emacs.c:1600)
        // for unknown names; the caller at regex-emacs.c:2071 then
        // signals REG_ECTYPE. We raise the equivalent compile error
        // here rather than silently continuing.
        _ => {
            return Err(RegexCompileError {
                message: format!("Invalid character class name: {}", name),
            });
        }
    };

    // GNU regex-emacs.c:2081-2092 sets the bit for the raw class
    // member AND also the bit for its TRANSLATE-mapped partner when
    // a translate table is in effect. Our set_bitmap_bit always
    // applies the translation (so it already sets the translated
    // bit); we additionally set the raw bit here to cover inputs
    // that match the raw form without going through the translate.
    for c in ascii_bytes {
        // Raw bit (no translation).
        set_bitmap_bit(buffer, bitmap_start, c, None);
        // Translated bit, when a translate table is active. This is
        // a no-op when `translate` is `None` or `translate[c] == c`.
        if translate.is_some() {
            set_bitmap_bit(buffer, bitmap_start, c, translate);
        }
    }

    // --- Multibyte coverage ----------------------------------------------
    //
    // Classes that include non-ASCII code points need to reach the
    // matcher's multibyte dispatch. GNU uses a range-table bit
    // (`re_wctype_to_bit`) that triggers `re_iswctype` at match time;
    // neomacs's equivalent is the `multibyte_charsets` map of
    // (start, end) ranges built in `compile_charset`. We append the
    // appropriate ranges here.
    match name {
        // Non-ASCII entirely: 0x80..=max Unicode scalar.
        "nonascii" | "multibyte" => {
            mb_ranges.push(('\u{80}', '\u{10FFFF}'));
        }
        _ => {}
    }

    Ok(())
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
/// This is used when no buffer-specific syntax table is available
/// (e.g. in unit tests or string-only matching).
pub(crate) struct DefaultSyntaxLookup;

/// Syntax lookup backed by a buffer's actual syntax table.
/// Used when regex searching within a buffer context.
pub(crate) struct BufferSyntaxLookup {
    pub syntax_table: crate::emacs_core::syntax::SyntaxTable,
}

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
        default_char_has_category(c, cat)
    }
}

impl SyntaxLookup for BufferSyntaxLookup {
    fn char_syntax(&self, c: char) -> SyntaxClass {
        self.syntax_table.char_syntax(c)
    }

    fn char_has_category(&self, c: char, cat: u8) -> bool {
        // Neomacs has no per-buffer category table yet, so we
        // share the same Unicode-block defaults as the default
        // syntax lookup. Audit finding #6 in
        // `drafts/regex-search-audit.md`.
        default_char_has_category(c, cat)
    }
}

/// Return whether character `c` belongs to the GNU regex category
/// `cat` (`\cX`).
///
/// GNU's category mechanism (`src/category.c`) gives each character
/// a 128-bit set of category memberships, populated at startup
/// time from `lisp/international/characters.el`. We don't ship the
/// full table; instead we hardcode the most common categories using
/// Unicode block ranges. The category mnemonics here are taken
/// directly from `lisp/international/characters.el` (the GNU
/// `(define-category ?X "...")` lines starting at line 37).
///
/// Audit finding #6 in `drafts/regex-search-audit.md` flagged that
/// only `\c|` worked. This implementation covers the categories the
/// CJK font-lock and bidi paths actually use.
fn default_char_has_category(c: char, cat: u8) -> bool {
    let cp = c as u32;
    match cat {
        // |  -- "line breakable". GNU's `characters.el` adds this
        // for most CJK and fullwidth ranges; we use the practical
        // shortcut of "any non-ASCII char" which is what neomacs
        // historically returned.
        b'|' => !c.is_ascii(),

        // a  -- ASCII (chars 32..126 in GNU; we accept the full
        // ASCII range to avoid false negatives on control chars).
        b'a' => c.is_ascii(),

        // A  -- 2-byte alnum. GNU populates this from CJK Latin /
        // fullwidth ASCII ranges. The practical shortcut is the
        // fullwidth ASCII alphanumeric block.
        b'A' => matches!(cp, 0xFF10..=0xFF19 | 0xFF21..=0xFF3A | 0xFF41..=0xFF5A),

        // l  -- Latin (a-z, A-Z and Latin-1/Extended letters).
        // r  -- Roman (Japanese context, same effective range).
        b'l' | b'r' => {
            c.is_ascii_alphabetic()
                || matches!(cp, 0x00C0..=0x00FF | 0x0100..=0x024F | 0x1E00..=0x1EFF)
        }

        // g  -- Greek (Greek and Coptic block).
        b'g' => matches!(cp, 0x0370..=0x03FF | 0x1F00..=0x1FFF),

        // G  -- 2-byte Greek (fullwidth Greek). Rare; use the
        // same practical bounds as `g` for now.
        b'G' => matches!(cp, 0x0370..=0x03FF | 0x1F00..=0x1FFF),

        // y  -- Cyrillic.
        b'y' | b'Y' => matches!(cp, 0x0400..=0x04FF | 0x0500..=0x052F),

        // b  -- Arabic.
        b'b' => matches!(cp, 0x0600..=0x06FF | 0x0750..=0x077F | 0xFB50..=0xFDFF),

        // w  -- Hebrew.
        b'w' => matches!(cp, 0x0590..=0x05FF | 0xFB1D..=0xFB4F),

        // t  -- Thai.
        b't' => matches!(cp, 0x0E00..=0x0E7F),

        // o  -- Lao.
        b'o' => matches!(cp, 0x0E80..=0x0EFF),

        // q  -- Tibetan.
        b'q' => matches!(cp, 0x0F00..=0x0FFF),

        // i  -- Indian (Devanagari + related). GNU's actual table
        // covers more scripts; this is the most common one.
        b'i' => matches!(cp, 0x0900..=0x097F),

        // I  -- Indian glyphs (broader Indic blocks).
        b'I' => matches!(cp, 0x0900..=0x0DFF),

        // e  -- Ethiopic (Ge'ez).
        b'e' => matches!(cp, 0x1200..=0x137F),

        // v  -- Vietnamese (Latin Extended Additional).
        b'v' => matches!(cp, 0x1E00..=0x1EFF),

        // h  -- Korean (Hangul Syllables + Jamo).
        // N  -- 2-byte Korean (same range here).
        b'h' | b'N' => {
            matches!(cp, 0x1100..=0x11FF | 0xAC00..=0xD7A3 | 0xA960..=0xA97F | 0xD7B0..=0xD7FF)
        }

        // c  -- Chinese / Han ideographs (broad).
        // C  -- 2-byte han (slightly narrower set).
        b'c' | b'C' => matches!(
            cp,
            0x3400..=0x4DBF
                | 0x4E00..=0x9FFF
                | 0xF900..=0xFAFF
                | 0x20000..=0x2FFFF
                | 0x30000..=0x323AF
        ),

        // H  -- Hiragana (Japanese).
        b'H' => matches!(cp, 0x3040..=0x309F | 0x1B000..=0x1B16F),

        // K  -- Katakana (Japanese).
        b'K' => matches!(
            cp,
            0x3099..=0x309C | 0x30A0..=0x30FF | 0x31F0..=0x31FF | 0x1AFF0..=0x1B16F
        ),

        // k  -- Katakana (lowercase mnemonic, same coverage).
        b'k' => matches!(cp, 0x30A0..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F),

        // j  -- Japanese (Hiragana + Katakana + half-width Katakana
        // + CJK punctuation + fullwidth ASCII).
        b'j' => matches!(
            cp,
            0x3000..=0x303F
                | 0x3040..=0x309F
                | 0x30A0..=0x30FF
                | 0xFF00..=0xFFEF
        ),

        // .  -- Base (Unicode L,N,P,S,Zs).
        b'.' => match c.is_ascii() {
            true => c.is_ascii_graphic() || c == ' ',
            false => {
                !matches!(cp, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
            }
        },

        // ^  -- Combining diacritic / mark (Unicode M).
        b'^' => {
            matches!(cp, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
        }

        // R  -- Strong R2L (right-to-left). Practical heuristic:
        // Hebrew and Arabic ranges.
        b'R' => matches!(cp, 0x0590..=0x05FF | 0x0600..=0x06FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF),

        // L  -- Strong L2R (everything else).
        b'L' => {
            !matches!(cp, 0x0590..=0x05FF | 0x0600..=0x06FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF)
        }

        // 6  -- digit (numeric).
        b'6' => c.is_numeric(),

        // Categories we don't recognize fall through as "no
        // membership" — same as GNU's behavior for an unset bit.
        _ => false,
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

    // Best match tracking for POSIX longest-match (audit #2).
    //
    // Mirrors GNU regex-emacs.c:4143-4154 and the main loop handling
    // at lines 4268-4345. When the pattern reaches its end with the
    // matcher positioned before the end of the searchable region
    // (`d < stop`), we save the current register state as the "best
    // so far" and force a backtrack to explore alternative paths.
    // After all backtracks have been exhausted, the best saved match
    // is restored. See GNU regex-emacs.c:5278-5279 for the
    // equivalent "restore after total failure" path.
    let posix_longest = pattern.posix;
    let mut best_regs_set = false;
    let mut best_match_end: usize = pos;
    let mut best_regstart: Vec<Option<usize>> = vec![None; num_regs];
    let mut best_regend: Vec<Option<usize>> = vec![None; num_regs];

    let mut pc = 0usize; // Bytecode program counter
    let mut d = pos; // Data position in text

    let translate = &pattern.translate;
    let pattern_multibyte = pattern.multibyte;
    let target_multibyte = pattern.target_multibyte;

    // Helper: translate a character for case-folding
    let tr = |c: u32| -> u32 {
        if let Some(table) = translate {
            if c < 256 { table[c as usize] as u32 } else { c }
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

    let unibyte_to_emacs_char = |byte: u8| -> u32 {
        if byte < 0x80 {
            byte as u32
        } else {
            emacs_char::byte8_to_char(byte)
        }
    };
    let syntax_char = |code: u32| -> char {
        if emacs_char::char_byte8_p(code) {
            char::from(emacs_char::char_to_byte8(code))
        } else {
            char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER)
        }
    };
    let emacs_char_to_unibyte = |code: u32| -> Option<u8> {
        if code < 0x80 || emacs_char::char_byte8_p(code) {
            Some(emacs_char::char_to_byte8(code))
        } else {
            None
        }
    };

    // Helper: decode an Emacs character at position.
    let text_char = |pos: usize| -> Option<(u32, usize)> {
        if pos >= text.len() {
            return None;
        }
        if target_multibyte {
            Some(emacs_char::string_char(&text[pos..]))
        } else {
            Some((unibyte_to_emacs_char(text[pos]), 1))
        }
    };

    // Helper: find the start of the character before `pos`.
    let prev_char_start = |pos: usize| -> Option<usize> {
        if pos == 0 {
            return None;
        }
        if !target_multibyte {
            return Some(pos - 1);
        }
        let mut p = pos - 1;
        while p > 0 && (text[p] & 0xC0) == 0x80 {
            p -= 1;
        }
        Some(p)
    };

    // Helper: is position at a word boundary?
    let at_word_boundary = |pos: usize| -> bool {
        let prev_word = if let Some(prev) = prev_char_start(pos) {
            text_char(prev)
                .map(|(c, _)| syntax.char_syntax(syntax_char(c)) == SyntaxClass::Word)
                .unwrap_or(false)
        } else {
            false
        };
        let curr_word = text_char(pos)
            .map(|(c, _)| syntax.char_syntax(syntax_char(c)) == SyntaxClass::Word)
            .unwrap_or(false);
        prev_word != curr_word
    };

    // Helper: is position at a symbol boundary?
    let at_symbol_boundary = |pos: usize| -> bool {
        let is_symbol_char = |c: u32| {
            let syn = syntax.char_syntax(syntax_char(c));
            syn == SyntaxClass::Word || syn == SyntaxClass::Symbol
        };
        let prev_sym = if let Some(prev) = prev_char_start(pos) {
            text_char(prev)
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

    // `try_fail!()` is the in-function replacement for GNU's
    // `goto fail`: pop the failure stack and resume there. If the
    // stack is empty, every backtracking avenue has been exhausted.
    // GNU's re_match_2_internal checks `best_regs_set` at that point
    // (regex-emacs.c:5278) and restores the saved best match instead
    // of returning -1. We do the same by setting `total_failure` and
    // breaking to the outer finalization block, which consults
    // `best_regs_set` to decide between returning None and restoring
    // the best registers.
    let mut total_failure = false;
    // `macro_rules!` labels are hygienic, so the label has to be
    // passed in explicitly as a `lifetime` metavariable.
    macro_rules! try_fail {
        ($label:lifetime) => {
            if goto_fail(
                &mut pc,
                &mut d,
                &mut fail_stack,
                &mut regstart,
                &mut regend,
                &mut counters,
            )
            .is_none()
            {
                total_failure = true;
                break $label;
            }
        };
    }

    'main_loop: loop {
        // End of pattern = potential match.
        //
        // GNU regex-emacs.c:4272-4345: if we haven't consumed the
        // full match region and POSIX longest-match is requested,
        // save the current registers as the best seen so far and
        // force another backtrack. When no more backtracks remain,
        // restore whichever saved best is better than the final
        // candidate (regex-emacs.c:4323-4344).
        if pc >= bytecode.len() {
            if posix_longest && d < stop {
                let better_than_best = !best_regs_set || d > best_match_end;
                if !fail_stack.is_empty() {
                    if better_than_best {
                        best_regs_set = true;
                        best_match_end = d;
                        best_regstart.clone_from_slice(&regstart);
                        best_regend.clone_from_slice(&regend);
                    }
                    // Force a backtrack to explore alternative paths.
                    // The stack is non-empty so goto_fail cannot fail.
                    try_fail!('main_loop);
                    continue 'main_loop;
                } else if best_regs_set && !better_than_best {
                    // No more backtracks; the previously saved best
                    // beats the current finishing position.  Restore
                    // it before finalizing.
                    d = best_match_end;
                    for i in 1..num_regs {
                        regstart[i] = best_regstart[i];
                        regend[i] = best_regend[i];
                    }
                }
            }
            break 'main_loop;
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
                // GNU regex-emacs.c:4429-4431 jumps directly to
                // `succeed_label`, bypassing the POSIX longest-match
                // check. For non-POSIX patterns, neomacs's compiler
                // emits a trailing `Succeed` so the matcher exits as
                // soon as the pattern completes (mirroring GNU's
                // `if (!posix_backtracking) BUF_PUSH(succeed)` at
                // regex-emacs.c:2685). In POSIX mode the trailing
                // `Succeed` is NOT emitted, so the matcher instead
                // falls through to the end-of-bytecode check above.
                break 'main_loop;
            }

            RegexOp::Exactn => {
                let count = bytecode[pc] as usize;
                pc += 1;
                let mut matched = true;
                let literal_start = pc;
                let literal_end = literal_start + count;
                let mut pat_pos = literal_start;
                while pat_pos < literal_end {
                    if d >= stop {
                        matched = false;
                        break;
                    }

                    let Some((buf_ch, buf_len)) = text_char(d) else {
                        matched = false;
                        break;
                    };

                    if target_multibyte {
                        let (pat_ch, pat_len) = if pattern_multibyte {
                            emacs_char::string_char(&bytecode[pat_pos..literal_end])
                        } else {
                            (unibyte_to_emacs_char(bytecode[pat_pos]), 1)
                        };
                        if tr(buf_ch) != pat_ch {
                            matched = false;
                            break;
                        }
                        pat_pos += pat_len;
                        d += buf_len;
                    } else {
                        let pat_byte = if pattern_multibyte {
                            let (pat_ch, pat_len) =
                                emacs_char::string_char(&bytecode[pat_pos..literal_end]);
                            let Some(byte) = emacs_char_to_unibyte(pat_ch) else {
                                matched = false;
                                break;
                            };
                            pat_pos += pat_len;
                            byte
                        } else {
                            let byte = bytecode[pat_pos];
                            pat_pos += 1;
                            byte
                        };
                        let buf_byte = text[d];
                        let mut translated = unibyte_to_emacs_char(buf_byte);
                        if !emacs_char::char_byte8_p(translated) {
                            translated = tr(translated);
                            if let Some(byte) = emacs_char_to_unibyte(translated) {
                                translated = byte as u32;
                            } else {
                                translated = buf_byte as u32;
                            }
                        } else {
                            translated = buf_byte as u32;
                        }
                        if translated as u8 != pat_byte {
                            matched = false;
                            break;
                        }
                        d += 1;
                    }
                }
                pc = literal_end;
                if !matched {
                    try_fail!('main_loop);
                }
            }

            RegexOp::AnyChar => {
                if d >= stop {
                    try_fail!('main_loop);
                    continue;
                }
                // Match any character except newline
                let Some((buf_ch, buf_len)) = text_char(d) else {
                    try_fail!('main_loop);
                    continue;
                };
                if tr(buf_ch) == '\n' as u32 {
                    try_fail!('main_loop);
                    continue;
                }
                d += buf_len;
            }

            RegexOp::Charset | RegexOp::CharsetNot => {
                let negate = op == RegexOp::CharsetNot;
                let charset_op_pos = pc - 1; // bytecode position of the opcode
                let bitmap_len = bytecode[pc] as usize & 0x7F;
                pc += 1;

                if d >= stop {
                    pc += bitmap_len;
                    try_fail!('main_loop);
                    continue;
                }

                let Some((orig_ch, ch_len)) = text_char(d) else {
                    pc += bitmap_len;
                    try_fail!('main_loop);
                    continue;
                };
                let mut ch = orig_ch;
                let mut unibyte_char = false;

                if target_multibyte {
                    ch = tr(ch);
                    if let Some(byte) = emacs_char_to_unibyte(ch) {
                        unibyte_char = true;
                        ch = byte as u32;
                    }
                } else {
                    let mut converted = unibyte_to_emacs_char(text[d]);
                    if !emacs_char::char_byte8_p(converted) {
                        converted = tr(converted);
                        if let Some(byte) = emacs_char_to_unibyte(converted) {
                            unibyte_char = true;
                            ch = byte as u32;
                        }
                    } else {
                        unibyte_char = true;
                        ch = text[d] as u32;
                    }
                }

                let in_set = if unibyte_char {
                    let c = ch as usize;
                    if (c / 8) < bitmap_len {
                        let byte = bytecode[pc + c / 8];
                        (byte >> (c % 8)) & 1 != 0
                    } else {
                        false
                    }
                } else if let Some(ranges) = pattern.multibyte_charsets.get(&charset_op_pos) {
                    let ch = syntax_char(ch);
                    ranges.iter().any(|&(lo, hi)| ch >= lo && ch <= hi)
                } else {
                    false
                };

                // GNU `regex-emacs.c:execute_charset` at lines
                // 3795-3802 takes the union of the bitmap with the
                // class flags consulted via the buffer syntax table.
                // For `[[:word:]]`/`[[:space:]]` this lets per-mode
                // overrides (e.g. `_` is `Sword` in `python-mode`)
                // extend the charset at match time. Audit finding
                // #8 in `drafts/regex-search-audit.md`.
                let in_set = in_set
                    || pattern
                        .charset_class_bits
                        .get(&charset_op_pos)
                        .copied()
                        .map(|bits| {
                            (bits & CHARSET_CLASS_BIT_WORD != 0
                                && syntax.char_syntax(syntax_char(orig_ch)) == SyntaxClass::Word)
                                || (bits & CHARSET_CLASS_BIT_SPACE != 0
                                    && syntax.char_syntax(syntax_char(orig_ch))
                                        == SyntaxClass::Whitespace)
                        })
                        .unwrap_or(false);

                let matched = if negate { !in_set } else { in_set };
                pc += bitmap_len;

                if !matched {
                    try_fail!('main_loop);
                    continue;
                }
                d += ch_len;
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
                    try_fail!('main_loop);
                    continue;
                };
                let Some(end) = regend.get(group).copied().flatten() else {
                    try_fail!('main_loop);
                    continue;
                };

                let ref_len = end - start;
                if d + ref_len > stop {
                    try_fail!('main_loop);
                    continue;
                }

                // Compare the backreference text
                let mut matched = true;
                for i in 0..ref_len {
                    if tr(text[d + i].into()) != tr(text[start + i].into()) {
                        matched = false;
                        break;
                    }
                }
                if !matched {
                    try_fail!('main_loop);
                    continue;
                }
                d += ref_len;
            }

            RegexOp::BegLine => {
                if d == 0 || (d > 0 && text[d - 1] == b'\n') {
                    // At beginning of line — succeed
                } else {
                    try_fail!('main_loop);
                }
            }

            RegexOp::EndLine => {
                if d >= stop || text[d] == b'\n' {
                    // At end of line — succeed
                } else {
                    try_fail!('main_loop);
                }
            }

            RegexOp::BegBuf => {
                if d != 0 {
                    try_fail!('main_loop);
                }
            }

            RegexOp::EndBuf => {
                if d != stop {
                    try_fail!('main_loop);
                }
            }

            RegexOp::AtDot => {
                if d != point {
                    try_fail!('main_loop);
                }
            }

            RegexOp::WordBound => {
                if !at_word_boundary(d) {
                    try_fail!('main_loop);
                }
            }

            RegexOp::NotWordBound => {
                if at_word_boundary(d) {
                    try_fail!('main_loop);
                }
            }

            RegexOp::WordBeg => {
                // Word beginning: not in word before, in word after
                let prev_word = prev_char_start(d)
                    .and_then(|p| text_char(p))
                    .map(|(c, _)| syntax.char_syntax(syntax_char(c)) == SyntaxClass::Word)
                    .unwrap_or(false);
                let curr_word = text_char(d)
                    .map(|(c, _)| syntax.char_syntax(syntax_char(c)) == SyntaxClass::Word)
                    .unwrap_or(false);
                if prev_word || !curr_word {
                    try_fail!('main_loop);
                }
            }

            RegexOp::WordEnd => {
                let prev_word = prev_char_start(d)
                    .and_then(|p| text_char(p))
                    .map(|(c, _)| syntax.char_syntax(syntax_char(c)) == SyntaxClass::Word)
                    .unwrap_or(false);
                let curr_word = text_char(d)
                    .map(|(c, _)| syntax.char_syntax(syntax_char(c)) == SyntaxClass::Word)
                    .unwrap_or(false);
                if !prev_word || curr_word {
                    try_fail!('main_loop);
                }
            }

            RegexOp::SymBeg => {
                let is_sym = |c: u32| {
                    let s = syntax.char_syntax(syntax_char(c));
                    s == SyntaxClass::Word || s == SyntaxClass::Symbol
                };
                let prev_sym = prev_char_start(d)
                    .and_then(|p| text_char(p))
                    .map(|(c, _)| is_sym(c))
                    .unwrap_or(false);
                let curr_sym = text_char(d).map(|(c, _)| is_sym(c)).unwrap_or(false);
                if prev_sym || !curr_sym {
                    try_fail!('main_loop);
                }
            }

            RegexOp::SymEnd => {
                let is_sym = |c: u32| {
                    let s = syntax.char_syntax(syntax_char(c));
                    s == SyntaxClass::Word || s == SyntaxClass::Symbol
                };
                let prev_sym = prev_char_start(d)
                    .and_then(|p| text_char(p))
                    .map(|(c, _)| is_sym(c))
                    .unwrap_or(false);
                let curr_sym = text_char(d).map(|(c, _)| is_sym(c)).unwrap_or(false);
                if !prev_sym || curr_sym {
                    try_fail!('main_loop);
                }
            }

            RegexOp::SyntaxSpec => {
                let class_byte = bytecode[pc];
                pc += 1;
                if d >= stop {
                    try_fail!('main_loop);
                    continue;
                }
                let Some((c, len)) = text_char(d) else {
                    try_fail!('main_loop);
                    continue;
                };
                if syntax.char_syntax(syntax_char(c)) as u8 != class_byte {
                    try_fail!('main_loop);
                    continue;
                }
                d += len;
            }

            RegexOp::NotSyntaxSpec => {
                let class_byte = bytecode[pc];
                pc += 1;
                if d >= stop {
                    try_fail!('main_loop);
                    continue;
                }
                let Some((c, len)) = text_char(d) else {
                    try_fail!('main_loop);
                    continue;
                };
                if syntax.char_syntax(syntax_char(c)) as u8 == class_byte {
                    try_fail!('main_loop);
                    continue;
                }
                d += len;
            }

            RegexOp::CategorySpec => {
                let cat = bytecode[pc];
                pc += 1;
                if d >= stop {
                    try_fail!('main_loop);
                    continue;
                }
                let Some((c, len)) = text_char(d) else {
                    try_fail!('main_loop);
                    continue;
                };
                if !syntax.char_has_category(syntax_char(c), cat) {
                    try_fail!('main_loop);
                    continue;
                }
                d += len;
            }

            RegexOp::NotCategorySpec => {
                let cat = bytecode[pc];
                pc += 1;
                if d >= stop {
                    try_fail!('main_loop);
                    continue;
                }
                let Some((c, len)) = text_char(d) else {
                    try_fail!('main_loop);
                    continue;
                };
                if syntax.char_has_category(syntax_char(c), cat) {
                    try_fail!('main_loop);
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

    // GNU regex-emacs.c:5278-5279: when the matcher breaks out of
    // the main loop due to total backtracking exhaustion, if a best
    // match was previously saved for POSIX longest-match, restore it
    // and fall through to the success path; otherwise there is no
    // match at all.
    if total_failure {
        if best_regs_set {
            d = best_match_end;
            for i in 1..num_regs {
                regstart[i] = best_regstart[i];
                regend[i] = best_regend[i];
            }
        } else {
            return None;
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
/// Populate `fastmap` for `\sX` (or `\SX` when `negate` is true) by
/// querying the standard syntax table for every ASCII byte. Mirrors
/// GNU regex-emacs.c:3170-3186 which iterates the same range and
/// consults the buffer's actual syntax table. We don't have a per-
/// buffer syntax table at compile time so we fall back to the
/// standard one — that matches GNU's behavior for all the standard
/// classes (Whitespace, Punctuation, Word, Symbol, Open, Close, ...)
/// for ASCII bytes. Audit finding #16 in
/// `drafts/regex-search-audit.md`.
fn fastmap_for_syntax_class(fastmap: &mut [bool; 256], class_byte: u8, negate: bool) {
    let target = match crate::emacs_core::syntax::SyntaxClass::from_code(class_byte as i64) {
        Some(cls) => cls,
        None => {
            // Unknown class — conservatively allow every byte
            // (matches GNU's "fall through to set all" behavior).
            *fastmap = [true; 256];
            return;
        }
    };
    let table = crate::emacs_core::syntax::SyntaxTable::new_standard();
    for c in 0u8..=127 {
        let in_class = table.char_syntax(c as char) == target;
        if in_class != negate {
            fastmap[c as usize] = true;
        }
    }
    // Conservatively allow every non-ASCII byte. The matcher will do
    // the real per-character syntax lookup at match time.
    for c in 128..256usize {
        fastmap[c] = true;
    }
}

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
                    let charset_op_pos = pc - 1;
                    let bitmap_len = bytecode[pc] as usize & 0x7F;
                    pc += 1;
                    for c in 0..256usize {
                        if c / 8 < bitmap_len && pc + c / 8 < bytecode.len() {
                            if (bytecode[pc + c / 8] >> (c % 8)) & 1 != 0 {
                                pattern.fastmap[c] = true;
                            }
                        }
                    }
                    // If this charset has multibyte ranges, conservatively
                    // allow all non-ASCII leading bytes in the fastmap.
                    if pattern.multibyte_charsets.contains_key(&charset_op_pos) {
                        for c in 128..256usize {
                            pattern.fastmap[c] = true;
                        }
                    }
                    break;
                }

                RegexOp::CharsetNot => {
                    if pc >= bytecode.len() {
                        break;
                    }
                    let charset_op_pos = pc - 1;
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
                    // GNU `re_compile_fastmap` consults the buffer's
                    // syntax table for `\sX` (regex-emacs.c:3170-3186).
                    // We don't have a per-buffer table at compile
                    // time so we use the standard one. The previous
                    // body hardcoded Rust's Unicode `is_whitespace` /
                    // `is_alphanumeric` and silently dropped classes
                    // 4-15, so any pattern using `\s(`, `\s)`, `\s\"`
                    // etc. went down a wrong fastmap path. See audit
                    // finding #16 in `drafts/regex-search-audit.md`.
                    fastmap_for_syntax_class(&mut pattern.fastmap, bytecode[pc], false);
                    break;
                }

                RegexOp::NotSyntaxSpec => {
                    if pc >= bytecode.len() {
                        break;
                    }
                    fastmap_for_syntax_class(&mut pattern.fastmap, bytecode[pc], true);
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
/// Equivalent to GNU's `re_search_2()` operating on a single
/// contiguous string. GNU also exposes the two-string variant
/// `re_match_2(pattern, string1, size1, string2, size2, ...)` which
/// walks the buffer text across the gap boundary
/// (`BEG..GPT` and `GPT..ZV`) without copying — for a 100MB buffer
/// that saves a 100MB allocation per search. Audit finding #17 in
/// `drafts/regex-search-audit.md` flags this as missing in neomacs.
///
/// We currently allocate the full buffer text via
/// `buf.text.text_range(0, buf.text.len())` at the call site in
/// `regex.rs::re_search_forward_with_posix` and friends, which is
/// correctness-equivalent to GNU's `re_match_2_internal` running
/// over a single string but is O(buffer-size) per search instead of
/// O(match-length). Porting the gap-aware path is a separate
/// optimization (audit Phase D Task 4.1, ~1 day).
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

    // Translate a byte through the compiled pattern's translate
    // table, mirroring GNU `regex-emacs.c:TRANSLATE` used at
    // regex-emacs.c:3568 inside the fastmap loop.
    let fastmap_tr = |b: u8| -> u8 {
        match &pattern.translate {
            Some(table) => table
                .get(b as usize)
                .copied()
                .map(|ch| ch as u32 as u8)
                .unwrap_or(b),
            None => b,
        }
    };

    if range >= 0 {
        // Forward search
        let end = (start + range as usize).min(text_len);
        let mut pos = start;
        while pos <= end {
            if pos > text_len {
                break;
            }
            // Skip UTF-8 continuation bytes — only try match at character
            // boundaries to avoid matching in the middle of a multibyte char.
            if pattern.target_multibyte && pos < text_len && (text[pos] & 0xC0) == 0x80 {
                pos += 1;
                continue;
            }
            // GNU disables fastmap skipping for nullable patterns so zero-width
            // matches like `\\(?:...\\)\\=` are still considered at every point.
            //
            // GNU regex-emacs.c:3568 applies TRANSLATE to the input
            // byte before indexing the fastmap. Under case-fold that
            // is what lets a fastmap built for a bitmap of lowercase
            // characters still catch uppercase input (audit #9).
            if pattern.fastmap_accurate && !pattern.can_be_null && pos < text_len {
                if !pattern.fastmap[fastmap_tr(text[pos]) as usize] {
                    pos += 1;
                    continue;
                }
            }
            if let Some(result) = re_match(pattern, text, pos, text_len, syntax, point) {
                return Some((pos, result.1));
            }
            pos += 1;
        }
    } else {
        // Backward search
        let end = start.saturating_sub((-range) as usize);
        for pos in (end..=start).rev() {
            // Skip UTF-8 continuation bytes — only try at character boundaries.
            if pattern.target_multibyte && pos < text_len && (text[pos] & 0xC0) == 0x80 {
                continue;
            }
            // GNU disables fastmap skipping for nullable patterns so zero-width
            // matches like `\\(?:...\\)\\=` are still considered at every point.
            if pattern.fastmap_accurate && !pattern.can_be_null && pos < text_len {
                if !pattern.fastmap[fastmap_tr(text[pos]) as usize] {
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
#[path = "regex_emacs_test.rs"]
mod tests;
