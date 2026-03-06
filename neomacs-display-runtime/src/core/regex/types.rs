//! Core types for the Emacs regex engine.

/// Regex opcodes — the bytecode instruction set.
///
/// This matches the semantics of Emacs's `re_opcode_t` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Opcode {
    /// No operation (padding).
    NoOp = 0,
    /// Match succeeded.
    Succeed = 1,
    /// Match exactly N bytes (followed by count byte + literal bytes).
    Exactn = 2,
    /// Match any character except newline.
    AnyChar = 3,
    /// Character set (bitmap for ASCII + optional range table for multibyte).
    Charset = 4,
    /// Negated character set.
    CharsetNot = 5,
    /// Start of capture group N (followed by group number byte).
    StartMemory = 6,
    /// End of capture group N (followed by group number byte).
    StopMemory = 7,
    /// Backreference to group N (followed by group number byte).
    Duplicate = 8,
    /// Match at line start (beginning of string or after \n).
    BegLine = 9,
    /// Match at line end (end of string or before \n).
    EndLine = 10,
    /// Match at buffer/string start.
    BegBuf = 11,
    /// Match at buffer/string end.
    EndBuf = 12,
    /// Unconditional jump (2-byte signed relative offset).
    Jump = 13,
    /// Push failure point, continue (2-byte offset to failure target).
    OnFailureJump = 14,
    /// Like OnFailureJump but don't restore string position on failure.
    OnFailureKeepStringJump = 15,
    /// Loop jump with cycle detection.
    OnFailureJumpLoop = 16,
    /// Non-greedy loop with cycle detection.
    OnFailureJumpNastyloop = 17,
    /// Self-modifying greedy loop optimization.
    OnFailureJumpSmart = 18,
    /// Succeed N times (4 bytes: 2-byte offset + 2-byte counter).
    SucceedN = 19,
    /// Jump N times (4 bytes: 2-byte offset + 2-byte counter).
    JumpN = 20,
    /// Modify counter at offset (4 bytes: 2-byte offset + 2-byte value).
    SetNumberAt = 21,
    /// Word start boundary.
    WordBeg = 22,
    /// Word end boundary.
    WordEnd = 23,
    /// Word boundary (\b).
    WordBound = 24,
    /// Non-word boundary (\B).
    NotWordBound = 25,
    /// Symbol start boundary.
    SymBeg = 26,
    /// Symbol end boundary.
    SymEnd = 27,
    /// Match character with given syntax class (followed by syntax code byte).
    SyntaxSpec = 28,
    /// Negated syntax class.
    NotSyntaxSpec = 29,
    /// Match character in category (followed by category code byte).
    CategorySpec = 30,
    /// Negated category.
    NotCategorySpec = 31,
    /// Match at point (Emacs buffer-specific).
    AtDot = 32,
}

impl Opcode {
    pub fn from_u8(val: u8) -> Option<Self> {
        if val <= 32 {
            Some(unsafe { std::mem::transmute(val) })
        } else {
            None
        }
    }
}

/// Emacs syntax classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SyntaxClass {
    Whitespace = 0,
    Punct = 1,
    Word = 2,
    Symbol = 3,
    Open = 4,
    Close = 5,
    Quote = 6,
    String = 7,
    Math = 8,
    Escape = 9,
    CharQuote = 10,
    Comment = 11,
    EndComment = 12,
    InheritStandard = 13,
    CommentFence = 14,
    StringFence = 15,
    Max = 16,
}

/// Named character classes for [:alpha:], [:digit:], etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharClass {
    Alnum,
    Alpha,
    Blank,
    Cntrl,
    Digit,
    Graph,
    Lower,
    Print,
    Punct,
    Space,
    Upper,
    XDigit,
    Ascii,
    NonAscii,
    Word,
    Multibyte,
    Unibyte,
}

impl CharClass {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "alnum" => Some(CharClass::Alnum),
            "alpha" => Some(CharClass::Alpha),
            "blank" => Some(CharClass::Blank),
            "cntrl" => Some(CharClass::Cntrl),
            "digit" => Some(CharClass::Digit),
            "graph" => Some(CharClass::Graph),
            "lower" => Some(CharClass::Lower),
            "print" => Some(CharClass::Print),
            "punct" => Some(CharClass::Punct),
            "space" => Some(CharClass::Space),
            "upper" => Some(CharClass::Upper),
            "xdigit" => Some(CharClass::XDigit),
            "ascii" => Some(CharClass::Ascii),
            "nonascii" => Some(CharClass::NonAscii),
            "word" => Some(CharClass::Word),
            "multibyte" => Some(CharClass::Multibyte),
            "unibyte" => Some(CharClass::Unibyte),
            _ => None,
        }
    }

    /// Test if a character matches this class.
    pub fn matches(&self, c: char) -> bool {
        let cu = c as u32;
        match self {
            CharClass::Alnum => c.is_alphanumeric(),
            CharClass::Alpha => c.is_alphabetic(),
            CharClass::Blank => c == ' ' || c == '\t',
            CharClass::Cntrl => c.is_control(),
            CharClass::Digit => c.is_ascii_digit(),
            CharClass::Graph => !c.is_control() && !c.is_whitespace() && cu > 0x20,
            CharClass::Lower => c.is_lowercase(),
            CharClass::Print => !c.is_control() || c == ' ',
            CharClass::Punct => {
                c.is_ascii_punctuation() || (cu > 127 && !c.is_alphanumeric() && !c.is_whitespace())
            }
            CharClass::Space => c.is_whitespace(),
            CharClass::Upper => c.is_uppercase(),
            CharClass::XDigit => c.is_ascii_hexdigit(),
            CharClass::Ascii => cu < 128,
            CharClass::NonAscii => cu >= 128,
            CharClass::Word => c.is_alphanumeric() || c == '_',
            CharClass::Multibyte => cu >= 128,
            CharClass::Unibyte => cu < 256,
        }
    }
}

/// Compiled regex pattern buffer.
#[derive(Clone)]
pub struct PatternBuffer {
    /// Compiled bytecode.
    pub bytecode: Vec<u8>,
    /// Number of capture groups (subexpressions).
    pub num_groups: usize,
    /// Whether the pattern can match an empty string.
    pub can_be_null: bool,
    /// 256-byte fastmap for first-character optimization.
    /// Entry `i` is true if byte `i` can be the first byte of a match.
    pub fastmap: [bool; 256],
    /// Whether the fastmap is valid.
    pub fastmap_accurate: bool,
    /// Whether the pattern was compiled with multibyte support.
    pub multibyte: bool,
    /// Whether the pattern uses syntax table features.
    pub uses_syntax: bool,
}

impl PatternBuffer {
    pub fn new() -> Self {
        PatternBuffer {
            bytecode: Vec::new(),
            num_groups: 0,
            can_be_null: false,
            fastmap: [false; 256],
            fastmap_accurate: false,
            multibyte: true,
            uses_syntax: false,
        }
    }
}

impl Default for PatternBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Capture group register results.
#[derive(Debug, Clone)]
pub struct Registers {
    /// Number of registers (groups + 1 for whole match).
    pub num_regs: usize,
    /// Start byte positions for each register (-1 if unmatched).
    pub starts: Vec<i64>,
    /// End byte positions for each register (-1 if unmatched).
    pub ends: Vec<i64>,
}

impl Registers {
    pub fn new(num_regs: usize) -> Self {
        Registers {
            num_regs,
            starts: vec![-1; num_regs],
            ends: vec![-1; num_regs],
        }
    }

    pub fn reset(&mut self) {
        for s in &mut self.starts {
            *s = -1;
        }
        for e in &mut self.ends {
            *e = -1;
        }
    }
}

/// Compilation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegexError {
    InvalidPattern(String),
    TrailingBackslash,
    InvalidBackreference(u8),
    UnmatchedBracket,
    UnmatchedParen,
    UnmatchedBrace,
    InvalidRepetition(String),
    InvalidRange,
    InvalidCharClass(String),
    PatternTooLarge,
    OutOfMemory,
    NoPrecedingElement,
}

impl std::fmt::Display for RegexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegexError::InvalidPattern(s) => write!(f, "Invalid pattern: {}", s),
            RegexError::TrailingBackslash => write!(f, "Trailing backslash"),
            RegexError::InvalidBackreference(n) => write!(f, "Invalid backreference \\{}", n),
            RegexError::UnmatchedBracket => write!(f, "Unmatched ["),
            RegexError::UnmatchedParen => write!(f, "Unmatched \\("),
            RegexError::UnmatchedBrace => write!(f, "Unmatched \\{{"),
            RegexError::InvalidRepetition(s) => write!(f, "Invalid repetition: {}", s),
            RegexError::InvalidRange => write!(f, "Invalid range"),
            RegexError::InvalidCharClass(s) => write!(f, "Invalid character class: {}", s),
            RegexError::PatternTooLarge => write!(f, "Pattern too large"),
            RegexError::OutOfMemory => write!(f, "Out of memory"),
            RegexError::NoPrecedingElement => write!(f, "No preceding element for repetition"),
        }
    }
}

impl std::error::Error for RegexError {}

#[cfg(test)]
mod tests {
    use super::*;

    // ──────────────────────────────────────────────────────────────
    // CharClass::from_name — all valid class names
    // ──────────────────────────────────────────────────────────────

    #[test]
    fn charclass_from_name_all_valid() {
        let cases = vec![
            ("alnum", CharClass::Alnum),
            ("alpha", CharClass::Alpha),
            ("blank", CharClass::Blank),
            ("cntrl", CharClass::Cntrl),
            ("digit", CharClass::Digit),
            ("graph", CharClass::Graph),
            ("lower", CharClass::Lower),
            ("print", CharClass::Print),
            ("punct", CharClass::Punct),
            ("space", CharClass::Space),
            ("upper", CharClass::Upper),
            ("xdigit", CharClass::XDigit),
            ("ascii", CharClass::Ascii),
            ("nonascii", CharClass::NonAscii),
            ("word", CharClass::Word),
            ("multibyte", CharClass::Multibyte),
            ("unibyte", CharClass::Unibyte),
        ];
        for (name, expected) in cases {
            assert_eq!(
                CharClass::from_name(name),
                Some(expected),
                "from_name({:?}) should return Some({:?})",
                name,
                expected
            );
        }
    }

    #[test]
    fn charclass_from_name_invalid_names() {
        assert_eq!(CharClass::from_name(""), None);
        assert_eq!(CharClass::from_name("ALNUM"), None);
        assert_eq!(CharClass::from_name("Alpha"), None);
        assert_eq!(CharClass::from_name("digits"), None);
        assert_eq!(CharClass::from_name("hexdigit"), None);
        assert_eq!(CharClass::from_name(" space"), None);
        assert_eq!(CharClass::from_name("space "), None);
        assert_eq!(CharClass::from_name("UPPER"), None);
        assert_eq!(CharClass::from_name("foo"), None);
        assert_eq!(CharClass::from_name("123"), None);
    }

    // ──────────────────────────────────────────────────────────────
    // CharClass::matches — character membership per class
    // ──────────────────────────────────────────────────────────────

    #[test]
    fn charclass_alnum_matches() {
        assert!(CharClass::Alnum.matches('a'));
        assert!(CharClass::Alnum.matches('Z'));
        assert!(CharClass::Alnum.matches('0'));
        assert!(CharClass::Alnum.matches('9'));
        // Unicode alphanumeric
        assert!(CharClass::Alnum.matches('\u{00E9}')); // e-acute
        assert!(!CharClass::Alnum.matches(' '));
        assert!(!CharClass::Alnum.matches('!'));
        assert!(!CharClass::Alnum.matches('\n'));
    }

    #[test]
    fn charclass_alpha_matches() {
        assert!(CharClass::Alpha.matches('a'));
        assert!(CharClass::Alpha.matches('Z'));
        assert!(CharClass::Alpha.matches('\u{03B1}')); // Greek alpha
        assert!(!CharClass::Alpha.matches('0'));
        assert!(!CharClass::Alpha.matches(' '));
        assert!(!CharClass::Alpha.matches('_'));
    }

    #[test]
    fn charclass_blank_matches() {
        assert!(CharClass::Blank.matches(' '));
        assert!(CharClass::Blank.matches('\t'));
        assert!(!CharClass::Blank.matches('\n'));
        assert!(!CharClass::Blank.matches('\r'));
        assert!(!CharClass::Blank.matches('a'));
        // Only space and tab, not other whitespace
        assert!(!CharClass::Blank.matches('\u{00A0}')); // non-breaking space
    }

    #[test]
    fn charclass_cntrl_matches() {
        assert!(CharClass::Cntrl.matches('\x00')); // NUL
        assert!(CharClass::Cntrl.matches('\x01')); // SOH
        assert!(CharClass::Cntrl.matches('\x1F')); // US
        assert!(CharClass::Cntrl.matches('\x7F')); // DEL
        assert!(CharClass::Cntrl.matches('\n'));
        assert!(CharClass::Cntrl.matches('\r'));
        assert!(CharClass::Cntrl.matches('\t'));
        assert!(!CharClass::Cntrl.matches(' '));
        assert!(!CharClass::Cntrl.matches('a'));
        assert!(!CharClass::Cntrl.matches('0'));
    }

    #[test]
    fn charclass_digit_matches() {
        for d in '0'..='9' {
            assert!(CharClass::Digit.matches(d), "digit should match '{}'", d);
        }
        assert!(!CharClass::Digit.matches('a'));
        assert!(!CharClass::Digit.matches(' '));
        // Non-ASCII digits should NOT match (ascii_digit only)
        assert!(!CharClass::Digit.matches('\u{0661}')); // Arabic-Indic digit one
    }

    #[test]
    fn charclass_graph_matches() {
        assert!(CharClass::Graph.matches('a'));
        assert!(CharClass::Graph.matches('Z'));
        assert!(CharClass::Graph.matches('0'));
        assert!(CharClass::Graph.matches('!'));
        assert!(CharClass::Graph.matches('~'));
        // Space is not graphical (cu == 0x20, fails > 0x20)
        assert!(!CharClass::Graph.matches(' '));
        // Control chars not graphical
        assert!(!CharClass::Graph.matches('\n'));
        assert!(!CharClass::Graph.matches('\x00'));
        assert!(!CharClass::Graph.matches('\t'));
    }

    #[test]
    fn charclass_lower_matches() {
        assert!(CharClass::Lower.matches('a'));
        assert!(CharClass::Lower.matches('z'));
        assert!(CharClass::Lower.matches('\u{00E9}')); // e-acute lowercase
        assert!(!CharClass::Lower.matches('A'));
        assert!(!CharClass::Lower.matches('Z'));
        assert!(!CharClass::Lower.matches('0'));
        assert!(!CharClass::Lower.matches(' '));
    }

    #[test]
    fn charclass_upper_matches() {
        assert!(CharClass::Upper.matches('A'));
        assert!(CharClass::Upper.matches('Z'));
        assert!(CharClass::Upper.matches('\u{00C9}')); // E-acute uppercase
        assert!(!CharClass::Upper.matches('a'));
        assert!(!CharClass::Upper.matches('z'));
        assert!(!CharClass::Upper.matches('0'));
    }

    #[test]
    fn charclass_print_matches() {
        assert!(CharClass::Print.matches(' ')); // space is printable
        assert!(CharClass::Print.matches('a'));
        assert!(CharClass::Print.matches('!'));
        assert!(CharClass::Print.matches('~'));
        // Control chars are not printable (except space)
        assert!(!CharClass::Print.matches('\x00'));
        assert!(!CharClass::Print.matches('\x1F'));
        assert!(!CharClass::Print.matches('\x7F'));
        assert!(!CharClass::Print.matches('\n'));
    }

    #[test]
    fn charclass_punct_matches() {
        // ASCII punctuation
        assert!(CharClass::Punct.matches('!'));
        assert!(CharClass::Punct.matches('.'));
        assert!(CharClass::Punct.matches(','));
        assert!(CharClass::Punct.matches('('));
        assert!(CharClass::Punct.matches(')'));
        assert!(CharClass::Punct.matches('@'));
        assert!(CharClass::Punct.matches('#'));
        assert!(!CharClass::Punct.matches('a'));
        assert!(!CharClass::Punct.matches('0'));
        assert!(!CharClass::Punct.matches(' '));
    }

    #[test]
    fn charclass_space_matches() {
        assert!(CharClass::Space.matches(' '));
        assert!(CharClass::Space.matches('\t'));
        assert!(CharClass::Space.matches('\n'));
        assert!(CharClass::Space.matches('\r'));
        assert!(CharClass::Space.matches('\u{000C}')); // form feed
        assert!(!CharClass::Space.matches('a'));
        assert!(!CharClass::Space.matches('0'));
    }

    #[test]
    fn charclass_xdigit_matches() {
        for d in '0'..='9' {
            assert!(CharClass::XDigit.matches(d));
        }
        for d in 'a'..='f' {
            assert!(CharClass::XDigit.matches(d));
        }
        for d in 'A'..='F' {
            assert!(CharClass::XDigit.matches(d));
        }
        assert!(!CharClass::XDigit.matches('g'));
        assert!(!CharClass::XDigit.matches('G'));
        assert!(!CharClass::XDigit.matches(' '));
    }

    #[test]
    fn charclass_ascii_matches() {
        assert!(CharClass::Ascii.matches('\x00'));
        assert!(CharClass::Ascii.matches('a'));
        assert!(CharClass::Ascii.matches('\x7F'));
        assert!(!CharClass::Ascii.matches('\u{0080}')); // first non-ASCII
        assert!(!CharClass::Ascii.matches('\u{00FF}'));
        assert!(!CharClass::Ascii.matches('\u{1F600}')); // emoji
    }

    #[test]
    fn charclass_nonascii_matches() {
        assert!(!CharClass::NonAscii.matches('\x00'));
        assert!(!CharClass::NonAscii.matches('a'));
        assert!(!CharClass::NonAscii.matches('\x7F'));
        assert!(CharClass::NonAscii.matches('\u{0080}'));
        assert!(CharClass::NonAscii.matches('\u{00FF}'));
        assert!(CharClass::NonAscii.matches('\u{1F600}')); // emoji
    }

    #[test]
    fn charclass_word_matches() {
        assert!(CharClass::Word.matches('a'));
        assert!(CharClass::Word.matches('Z'));
        assert!(CharClass::Word.matches('0'));
        assert!(CharClass::Word.matches('9'));
        assert!(CharClass::Word.matches('_'));
        // Unicode alphanumeric counts
        assert!(CharClass::Word.matches('\u{00E9}')); // e-acute
        assert!(!CharClass::Word.matches(' '));
        assert!(!CharClass::Word.matches('-'));
        assert!(!CharClass::Word.matches('!'));
    }

    #[test]
    fn charclass_multibyte_matches() {
        assert!(!CharClass::Multibyte.matches('\x00'));
        assert!(!CharClass::Multibyte.matches('\x7F'));
        assert!(CharClass::Multibyte.matches('\u{0080}'));
        assert!(CharClass::Multibyte.matches('\u{1F600}'));
    }

    #[test]
    fn charclass_unibyte_matches() {
        // Unibyte: codepoint < 256
        assert!(CharClass::Unibyte.matches('\x00'));
        assert!(CharClass::Unibyte.matches('\x7F'));
        assert!(CharClass::Unibyte.matches('\u{00FF}'));
        assert!(!CharClass::Unibyte.matches('\u{0100}')); // first beyond 255
        assert!(!CharClass::Unibyte.matches('\u{1F600}'));
    }

    #[test]
    fn charclass_ascii_nonascii_partition() {
        // ASCII and NonAscii must be exact complements
        for cp in 0u32..=255 {
            if let Some(c) = char::from_u32(cp) {
                assert_ne!(
                    CharClass::Ascii.matches(c),
                    CharClass::NonAscii.matches(c),
                    "Ascii and NonAscii must be complements for U+{:04X}",
                    cp
                );
            }
        }
    }

    // ──────────────────────────────────────────────────────────────
    // Opcode::from_u8 — valid opcodes and out-of-range values
    // ──────────────────────────────────────────────────────────────

    #[test]
    fn opcode_from_u8_all_valid() {
        let expected = vec![
            (0, Opcode::NoOp),
            (1, Opcode::Succeed),
            (2, Opcode::Exactn),
            (3, Opcode::AnyChar),
            (4, Opcode::Charset),
            (5, Opcode::CharsetNot),
            (6, Opcode::StartMemory),
            (7, Opcode::StopMemory),
            (8, Opcode::Duplicate),
            (9, Opcode::BegLine),
            (10, Opcode::EndLine),
            (11, Opcode::BegBuf),
            (12, Opcode::EndBuf),
            (13, Opcode::Jump),
            (14, Opcode::OnFailureJump),
            (15, Opcode::OnFailureKeepStringJump),
            (16, Opcode::OnFailureJumpLoop),
            (17, Opcode::OnFailureJumpNastyloop),
            (18, Opcode::OnFailureJumpSmart),
            (19, Opcode::SucceedN),
            (20, Opcode::JumpN),
            (21, Opcode::SetNumberAt),
            (22, Opcode::WordBeg),
            (23, Opcode::WordEnd),
            (24, Opcode::WordBound),
            (25, Opcode::NotWordBound),
            (26, Opcode::SymBeg),
            (27, Opcode::SymEnd),
            (28, Opcode::SyntaxSpec),
            (29, Opcode::NotSyntaxSpec),
            (30, Opcode::CategorySpec),
            (31, Opcode::NotCategorySpec),
            (32, Opcode::AtDot),
        ];
        for (val, expected_op) in expected {
            let result = Opcode::from_u8(val);
            assert_eq!(
                result,
                Some(expected_op),
                "Opcode::from_u8({}) should return {:?}",
                val,
                expected_op
            );
        }
    }

    #[test]
    fn opcode_from_u8_out_of_range() {
        assert_eq!(Opcode::from_u8(33), None);
        assert_eq!(Opcode::from_u8(34), None);
        assert_eq!(Opcode::from_u8(100), None);
        assert_eq!(Opcode::from_u8(255), None);
    }

    #[test]
    fn opcode_from_u8_boundary() {
        // 32 is the last valid value (AtDot)
        assert!(Opcode::from_u8(32).is_some());
        assert_eq!(Opcode::from_u8(32), Some(Opcode::AtDot));
        // 33 is the first invalid value
        assert!(Opcode::from_u8(33).is_none());
    }

    // ──────────────────────────────────────────────────────────────
    // PatternBuffer construction and fields
    // ──────────────────────────────────────────────────────────────

    #[test]
    fn pattern_buffer_new_defaults() {
        let pb = PatternBuffer::new();
        assert!(pb.bytecode.is_empty());
        assert_eq!(pb.num_groups, 0);
        assert!(!pb.can_be_null);
        assert!(!pb.fastmap_accurate);
        assert!(pb.multibyte); // default is true
        assert!(!pb.uses_syntax);
        // All fastmap entries should be false
        assert!(pb.fastmap.iter().all(|&b| !b));
    }

    #[test]
    fn pattern_buffer_default_trait() {
        let pb = PatternBuffer::default();
        assert!(pb.bytecode.is_empty());
        assert_eq!(pb.num_groups, 0);
        assert!(!pb.can_be_null);
        assert!(pb.multibyte);
    }

    #[test]
    fn pattern_buffer_new_equals_default() {
        let a = PatternBuffer::new();
        let b = PatternBuffer::default();
        assert_eq!(a.bytecode, b.bytecode);
        assert_eq!(a.num_groups, b.num_groups);
        assert_eq!(a.can_be_null, b.can_be_null);
        assert_eq!(a.fastmap, b.fastmap);
        assert_eq!(a.fastmap_accurate, b.fastmap_accurate);
        assert_eq!(a.multibyte, b.multibyte);
        assert_eq!(a.uses_syntax, b.uses_syntax);
    }

    #[test]
    fn pattern_buffer_fields_mutable() {
        let mut pb = PatternBuffer::new();
        pb.bytecode = vec![2, 3, b'a', b'b', b'c'];
        pb.num_groups = 2;
        pb.can_be_null = true;
        pb.fastmap[b'a' as usize] = true;
        pb.fastmap_accurate = true;
        pb.multibyte = false;
        pb.uses_syntax = true;

        assert_eq!(pb.bytecode, vec![2, 3, b'a', b'b', b'c']);
        assert_eq!(pb.num_groups, 2);
        assert!(pb.can_be_null);
        assert!(pb.fastmap[b'a' as usize]);
        assert!(!pb.fastmap[b'b' as usize]); // only 'a' was set
        assert!(pb.fastmap_accurate);
        assert!(!pb.multibyte);
        assert!(pb.uses_syntax);
    }

    #[test]
    fn pattern_buffer_clone() {
        let mut pb = PatternBuffer::new();
        pb.bytecode = vec![1, 2, 3];
        pb.num_groups = 5;
        pb.can_be_null = true;
        let cloned = pb.clone();
        assert_eq!(cloned.bytecode, vec![1, 2, 3]);
        assert_eq!(cloned.num_groups, 5);
        assert!(cloned.can_be_null);
    }

    // ──────────────────────────────────────────────────────────────
    // Registers — default state and operations
    // ──────────────────────────────────────────────────────────────

    #[test]
    fn registers_new_default_state() {
        let regs = Registers::new(5);
        assert_eq!(regs.num_regs, 5);
        assert_eq!(regs.starts.len(), 5);
        assert_eq!(regs.ends.len(), 5);
        // All positions initialized to -1
        for i in 0..5 {
            assert_eq!(regs.starts[i], -1);
            assert_eq!(regs.ends[i], -1);
        }
    }

    #[test]
    fn registers_new_zero_regs() {
        let regs = Registers::new(0);
        assert_eq!(regs.num_regs, 0);
        assert!(regs.starts.is_empty());
        assert!(regs.ends.is_empty());
    }

    #[test]
    fn registers_new_single_reg() {
        let regs = Registers::new(1);
        assert_eq!(regs.num_regs, 1);
        assert_eq!(regs.starts, vec![-1]);
        assert_eq!(regs.ends, vec![-1]);
    }

    #[test]
    fn registers_reset() {
        let mut regs = Registers::new(3);
        // Simulate matched positions
        regs.starts[0] = 0;
        regs.ends[0] = 5;
        regs.starts[1] = 1;
        regs.ends[1] = 3;
        regs.starts[2] = 10;
        regs.ends[2] = 20;

        regs.reset();

        for i in 0..3 {
            assert_eq!(regs.starts[i], -1, "starts[{}] should be -1 after reset", i);
            assert_eq!(regs.ends[i], -1, "ends[{}] should be -1 after reset", i);
        }
    }

    #[test]
    fn registers_reset_preserves_length() {
        let mut regs = Registers::new(10);
        regs.starts[5] = 42;
        regs.ends[5] = 99;
        regs.reset();
        assert_eq!(regs.num_regs, 10);
        assert_eq!(regs.starts.len(), 10);
        assert_eq!(regs.ends.len(), 10);
    }

    #[test]
    fn registers_clone() {
        let mut regs = Registers::new(3);
        regs.starts[0] = 10;
        regs.ends[0] = 20;
        let cloned = regs.clone();
        assert_eq!(cloned.num_regs, 3);
        assert_eq!(cloned.starts[0], 10);
        assert_eq!(cloned.ends[0], 20);
        assert_eq!(cloned.starts[1], -1);
    }

    // ──────────────────────────────────────────────────────────────
    // RegexError — variants and Display impl
    // ──────────────────────────────────────────────────────────────

    #[test]
    fn regex_error_display_invalid_pattern() {
        let err = RegexError::InvalidPattern("bad stuff".into());
        assert_eq!(err.to_string(), "Invalid pattern: bad stuff");
    }

    #[test]
    fn regex_error_display_trailing_backslash() {
        let err = RegexError::TrailingBackslash;
        assert_eq!(err.to_string(), "Trailing backslash");
    }

    #[test]
    fn regex_error_display_invalid_backreference() {
        let err = RegexError::InvalidBackreference(5);
        assert_eq!(err.to_string(), "Invalid backreference \\5");
    }

    #[test]
    fn regex_error_display_unmatched_bracket() {
        let err = RegexError::UnmatchedBracket;
        assert_eq!(err.to_string(), "Unmatched [");
    }

    #[test]
    fn regex_error_display_unmatched_paren() {
        let err = RegexError::UnmatchedParen;
        assert_eq!(err.to_string(), "Unmatched \\(");
    }

    #[test]
    fn regex_error_display_unmatched_brace() {
        let err = RegexError::UnmatchedBrace;
        assert_eq!(err.to_string(), "Unmatched \\{");
    }

    #[test]
    fn regex_error_display_invalid_repetition() {
        let err = RegexError::InvalidRepetition("overflow".into());
        assert_eq!(err.to_string(), "Invalid repetition: overflow");
    }

    #[test]
    fn regex_error_display_invalid_range() {
        let err = RegexError::InvalidRange;
        assert_eq!(err.to_string(), "Invalid range");
    }

    #[test]
    fn regex_error_display_invalid_char_class() {
        let err = RegexError::InvalidCharClass("bogus".into());
        assert_eq!(err.to_string(), "Invalid character class: bogus");
    }

    #[test]
    fn regex_error_display_pattern_too_large() {
        let err = RegexError::PatternTooLarge;
        assert_eq!(err.to_string(), "Pattern too large");
    }

    #[test]
    fn regex_error_display_out_of_memory() {
        let err = RegexError::OutOfMemory;
        assert_eq!(err.to_string(), "Out of memory");
    }

    #[test]
    fn regex_error_display_no_preceding_element() {
        let err = RegexError::NoPrecedingElement;
        assert_eq!(err.to_string(), "No preceding element for repetition");
    }

    #[test]
    fn regex_error_is_std_error() {
        let err: Box<dyn std::error::Error> = Box::new(RegexError::TrailingBackslash);
        // Verify it implements std::error::Error by using it as a trait object
        assert_eq!(err.to_string(), "Trailing backslash");
    }

    #[test]
    fn regex_error_equality() {
        assert_eq!(RegexError::TrailingBackslash, RegexError::TrailingBackslash);
        assert_eq!(
            RegexError::InvalidPattern("x".into()),
            RegexError::InvalidPattern("x".into())
        );
        assert_ne!(
            RegexError::InvalidPattern("x".into()),
            RegexError::InvalidPattern("y".into())
        );
        assert_ne!(RegexError::TrailingBackslash, RegexError::UnmatchedBracket);
        assert_eq!(
            RegexError::InvalidBackreference(3),
            RegexError::InvalidBackreference(3)
        );
        assert_ne!(
            RegexError::InvalidBackreference(3),
            RegexError::InvalidBackreference(4)
        );
    }

    #[test]
    fn regex_error_clone() {
        let err = RegexError::InvalidPattern("test".into());
        let cloned = err.clone();
        assert_eq!(err, cloned);
    }

    // ──────────────────────────────────────────────────────────────
    // MatchResult variants
    // ──────────────────────────────────────────────────────────────

    #[test]
    fn match_result_variants() {
        let m = MatchResult::Match(42);
        if let MatchResult::Match(pos) = m {
            assert_eq!(pos, 42);
        } else {
            panic!("Expected MatchResult::Match");
        }

        let nm = MatchResult::NoMatch;
        assert!(matches!(nm, MatchResult::NoMatch));

        let e = MatchResult::Error;
        assert!(matches!(e, MatchResult::Error));
    }

    #[test]
    fn match_result_clone() {
        let m = MatchResult::Match(100);
        let cloned = m.clone();
        if let MatchResult::Match(pos) = cloned {
            assert_eq!(pos, 100);
        } else {
            panic!("Expected MatchResult::Match after clone");
        }
    }

    // ──────────────────────────────────────────────────────────────
    // DefaultCharProperties
    // ──────────────────────────────────────────────────────────────

    #[test]
    fn default_char_properties_syntax_class() {
        let props = DefaultCharProperties;
        assert_eq!(props.syntax_class(' ', 0), SyntaxClass::Whitespace);
        assert_eq!(props.syntax_class('\t', 0), SyntaxClass::Whitespace);
        assert_eq!(props.syntax_class('\n', 0), SyntaxClass::Whitespace);
        assert_eq!(props.syntax_class('a', 0), SyntaxClass::Word);
        assert_eq!(props.syntax_class('Z', 0), SyntaxClass::Word);
        assert_eq!(props.syntax_class('0', 0), SyntaxClass::Word);
        assert_eq!(props.syntax_class('_', 0), SyntaxClass::Word);
        assert_eq!(props.syntax_class('(', 0), SyntaxClass::Open);
        assert_eq!(props.syntax_class('[', 0), SyntaxClass::Open);
        assert_eq!(props.syntax_class('{', 0), SyntaxClass::Open);
        assert_eq!(props.syntax_class(')', 0), SyntaxClass::Close);
        assert_eq!(props.syntax_class(']', 0), SyntaxClass::Close);
        assert_eq!(props.syntax_class('}', 0), SyntaxClass::Close);
        assert_eq!(props.syntax_class('"', 0), SyntaxClass::String);
        assert_eq!(props.syntax_class('\\', 0), SyntaxClass::Escape);
        assert_eq!(props.syntax_class('!', 0), SyntaxClass::Punct);
        assert_eq!(props.syntax_class('@', 0), SyntaxClass::Punct);
    }

    #[test]
    fn default_char_properties_in_category_always_false() {
        let props = DefaultCharProperties;
        assert!(!props.in_category('a', 0));
        assert!(!props.in_category('Z', 255));
    }

    #[test]
    fn default_char_properties_translate_identity() {
        let props = DefaultCharProperties;
        assert_eq!(props.translate('a'), 'a');
        assert_eq!(props.translate('A'), 'A');
        assert_eq!(props.translate('0'), '0');
        assert_eq!(props.translate('\u{1F600}'), '\u{1F600}');
    }

    // ──────────────────────────────────────────────────────────────
    // SyntaxClass repr values
    // ──────────────────────────────────────────────────────────────

    #[test]
    fn syntax_class_repr_values() {
        assert_eq!(SyntaxClass::Whitespace as u8, 0);
        assert_eq!(SyntaxClass::Punct as u8, 1);
        assert_eq!(SyntaxClass::Word as u8, 2);
        assert_eq!(SyntaxClass::Symbol as u8, 3);
        assert_eq!(SyntaxClass::Open as u8, 4);
        assert_eq!(SyntaxClass::Close as u8, 5);
        assert_eq!(SyntaxClass::Quote as u8, 6);
        assert_eq!(SyntaxClass::String as u8, 7);
        assert_eq!(SyntaxClass::Math as u8, 8);
        assert_eq!(SyntaxClass::Escape as u8, 9);
        assert_eq!(SyntaxClass::CharQuote as u8, 10);
        assert_eq!(SyntaxClass::Comment as u8, 11);
        assert_eq!(SyntaxClass::EndComment as u8, 12);
        assert_eq!(SyntaxClass::InheritStandard as u8, 13);
        assert_eq!(SyntaxClass::CommentFence as u8, 14);
        assert_eq!(SyntaxClass::StringFence as u8, 15);
        assert_eq!(SyntaxClass::Max as u8, 16);
    }
}

/// Match result.
#[derive(Debug, Clone)]
pub enum MatchResult {
    /// Match found at the given byte position.
    Match(i64),
    /// No match.
    NoMatch,
    /// Internal error.
    Error,
}

/// Callback trait for syntax/category table integration.
/// This allows the regex engine to query Emacs-specific character properties
/// without depending on Emacs data structures.
pub trait CharProperties {
    /// Return the syntax class of character `c` at position `pos`.
    fn syntax_class(&self, c: char, pos: usize) -> SyntaxClass;

    /// Return true if character `c` belongs to category `cat`.
    fn in_category(&self, c: char, cat: u8) -> bool;

    /// Translate a character for case-folding. Returns the canonical form.
    fn translate(&self, c: char) -> char;
}

/// Default implementation that uses Unicode properties.
pub struct DefaultCharProperties;

impl CharProperties for DefaultCharProperties {
    fn syntax_class(&self, c: char, _pos: usize) -> SyntaxClass {
        if c.is_whitespace() {
            SyntaxClass::Whitespace
        } else if c.is_alphanumeric() || c == '_' {
            SyntaxClass::Word
        } else if c == '(' || c == '[' || c == '{' {
            SyntaxClass::Open
        } else if c == ')' || c == ']' || c == '}' {
            SyntaxClass::Close
        } else if c == '"' {
            SyntaxClass::String
        } else if c == '\\' {
            SyntaxClass::Escape
        } else {
            SyntaxClass::Punct
        }
    }

    fn in_category(&self, _c: char, _cat: u8) -> bool {
        false
    }

    fn translate(&self, c: char) -> char {
        c
    }
}
