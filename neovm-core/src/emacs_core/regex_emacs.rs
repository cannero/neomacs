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
// TODO: Phase 2 — Compiler (regex_compile)
// TODO: Phase 3 — Matcher (re_match_2_internal)
// TODO: Phase 4 — Searcher (re_search_2)
// ---------------------------------------------------------------------------
