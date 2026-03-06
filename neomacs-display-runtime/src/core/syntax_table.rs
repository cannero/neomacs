//! Pure Rust implementation of Emacs's syntax table system.
//!
//! This module implements the core of Emacs's syntax table machinery from
//! `syntax.c` / `syntax.h`. It provides:
//!
//! - All 16 Emacs syntax classes with descriptor-character parsing
//! - Compact syntax entries encoding class, flags, and matching character
//! - 8 comment/prefix flags as a bitfield
//! - A `SyntaxTable` mapping characters to entries (with inheritance default)
//! - `from_emacs_string()` for parsing Emacs syntax descriptor strings
//! - Parse state for incremental scanning
//! - Core scanning functions: word, list, comment, and full sexp scanning
//! - A `TextSource` trait for abstracting over text representations
//! - A `standard_table()` function returning the default Emacs syntax table

use std::collections::HashMap;
use std::fmt;

// ---------------------------------------------------------------------------
// Syntax Classes
// ---------------------------------------------------------------------------

/// All 16 Emacs syntax classes.
///
/// The numeric values match the internal encoding used by Emacs (0..15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SyntaxClass {
    /// Whitespace (space, tab, newline, formfeed).
    Whitespace = 0,
    /// Punctuation — ordinary punctuation characters.
    Punct = 1,
    /// Word constituent (letters, digits in standard table).
    Word = 2,
    /// Symbol constituent (e.g. `_`).
    Symbol = 3,
    /// Open parenthesis (e.g. `(`, `[`, `{`).
    Open = 4,
    /// Close parenthesis (e.g. `)`, `]`, `}`).
    Close = 5,
    /// Expression prefix (e.g. `'` in Lisp).
    Quote = 6,
    /// String delimiter (e.g. `"`).
    String = 7,
    /// Paired math delimiter (e.g. `$` in TeX).
    Math = 8,
    /// Escape character (e.g. `\`).
    Escape = 9,
    /// Character quote (like escape but only quotes the immediately following char).
    CharQuote = 10,
    /// Comment starter (single-character, e.g. `;` in Lisp).
    Comment = 11,
    /// Comment ender (single-character).
    EndComment = 12,
    /// Inherit from the standard syntax table.
    Inherit = 13,
    /// Generic comment delimiter (fence).
    CommentFence = 14,
    /// Generic string delimiter (fence).
    StringFence = 15,
}

impl SyntaxClass {
    /// Convert an Emacs syntax descriptor character to a syntax class.
    ///
    /// The descriptor characters are the first character in a syntax table
    /// entry string (e.g. `"w"` for Word, `"("` for Open).
    pub fn from_char(ch: char) -> Option<SyntaxClass> {
        match ch {
            ' ' | '-' => Some(SyntaxClass::Whitespace),
            '.' => Some(SyntaxClass::Punct),
            'w' => Some(SyntaxClass::Word),
            '_' => Some(SyntaxClass::Symbol),
            '(' => Some(SyntaxClass::Open),
            ')' => Some(SyntaxClass::Close),
            '\'' => Some(SyntaxClass::Quote),
            '"' => Some(SyntaxClass::String),
            '$' => Some(SyntaxClass::Math),
            '\\' => Some(SyntaxClass::Escape),
            '/' => Some(SyntaxClass::CharQuote),
            '<' => Some(SyntaxClass::Comment),
            '>' => Some(SyntaxClass::EndComment),
            '@' => Some(SyntaxClass::Inherit),
            '|' => Some(SyntaxClass::CommentFence),
            '!' => Some(SyntaxClass::StringFence),
            _ => None,
        }
    }

    /// Return the Emacs descriptor character for this syntax class.
    pub fn to_char(self) -> char {
        match self {
            SyntaxClass::Whitespace => ' ',
            SyntaxClass::Punct => '.',
            SyntaxClass::Word => 'w',
            SyntaxClass::Symbol => '_',
            SyntaxClass::Open => '(',
            SyntaxClass::Close => ')',
            SyntaxClass::Quote => '\'',
            SyntaxClass::String => '"',
            SyntaxClass::Math => '$',
            SyntaxClass::Escape => '\\',
            SyntaxClass::CharQuote => '/',
            SyntaxClass::Comment => '<',
            SyntaxClass::EndComment => '>',
            SyntaxClass::Inherit => '@',
            SyntaxClass::CommentFence => '|',
            SyntaxClass::StringFence => '!',
        }
    }

    /// Whether this class is a "word-like" constituent (Word or Symbol).
    pub fn is_word_like(self) -> bool {
        matches!(self, SyntaxClass::Word | SyntaxClass::Symbol)
    }
}

impl fmt::Display for SyntaxClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_char())
    }
}

// ---------------------------------------------------------------------------
// Syntax Flags
// ---------------------------------------------------------------------------

bitflags::bitflags! {
    /// Flags stored alongside each syntax table entry.
    ///
    /// These encode two-character comment start/end sequences, prefix status,
    /// comment style, and nesting.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct SyntaxFlags: u8 {
        /// This character is the first of a 2-char comment-start sequence.
        const COMSTART_FIRST  = 1 << 0;  // flag bit 16 in Emacs encoding
        /// This character is the second of a 2-char comment-start sequence.
        const COMSTART_SECOND = 1 << 1;  // flag bit 17
        /// This character is the first of a 2-char comment-end sequence.
        const COMEND_FIRST    = 1 << 2;  // flag bit 18
        /// This character is the second of a 2-char comment-end sequence.
        const COMEND_SECOND   = 1 << 3;  // flag bit 19
        /// This character is a prefix character (e.g. `'` in Lisp).
        const PREFIX          = 1 << 4;  // flag bit 20
        /// Comment style b (the "second" comment style).
        const COMMENT_STYLE_B = 1 << 5;  // flag bit 21
        /// Comment can nest.
        const COMMENT_NESTED  = 1 << 6;  // flag bit 22
        /// Comment style c (a third style, used for `c` in some modes).
        const COMMENT_STYLE_C = 1 << 7;  // flag bit 23
    }
}

impl SyntaxFlags {
    /// Return the comment style encoded by this flag set (0, 1, 2, or 3).
    ///
    /// - 0 = style "a" (default)
    /// - 1 = style "b" (`COMMENT_STYLE_B` set)
    /// - 2 = style "c" (`COMMENT_STYLE_C` set)
    /// - 3 = both b and c (unusual but representable)
    pub fn comment_style(self) -> u8 {
        let b = self.contains(SyntaxFlags::COMMENT_STYLE_B) as u8;
        let c = self.contains(SyntaxFlags::COMMENT_STYLE_C) as u8;
        b | (c << 1)
    }

    /// Parse flag characters from an Emacs syntax descriptor string.
    ///
    /// The flag portion is everything after the class character and optional
    /// matching character. Valid flag characters: `1`, `2`, `3`, `4`, `p`,
    /// `b`, `n`, `c`.
    fn from_flag_chars(chars: &[char]) -> SyntaxFlags {
        let mut flags = SyntaxFlags::empty();
        for &ch in chars {
            match ch {
                '1' => flags |= SyntaxFlags::COMSTART_FIRST,
                '2' => flags |= SyntaxFlags::COMSTART_SECOND,
                '3' => flags |= SyntaxFlags::COMEND_FIRST,
                '4' => flags |= SyntaxFlags::COMEND_SECOND,
                'p' => flags |= SyntaxFlags::PREFIX,
                'b' => flags |= SyntaxFlags::COMMENT_STYLE_B,
                'n' => flags |= SyntaxFlags::COMMENT_NESTED,
                'c' => flags |= SyntaxFlags::COMMENT_STYLE_C,
                ' ' => {} // Spaces are allowed as filler in Emacs descriptors.
                _ => {}   // Unknown flag chars are silently ignored per Emacs.
            }
        }
        flags
    }
}

// ---------------------------------------------------------------------------
// Syntax Entry
// ---------------------------------------------------------------------------

/// A single syntax table entry, encoding the class, optional matching
/// character, and flags for one character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxEntry {
    /// The syntax class.
    pub class: SyntaxClass,
    /// For Open/Close class, the matching paren.  For String class, the
    /// terminating character (if different from the string opener).  `None`
    /// for most entries.
    pub matching_char: Option<char>,
    /// Additional flags (comment start/end, nesting, style, prefix).
    pub flags: SyntaxFlags,
}

impl SyntaxEntry {
    /// Create a new entry with the given class and no flags or matching char.
    pub fn new(class: SyntaxClass) -> Self {
        SyntaxEntry {
            class,
            matching_char: None,
            flags: SyntaxFlags::empty(),
        }
    }

    /// Create an entry with matching char (e.g. Open `(` matching `)`).
    pub fn with_match(class: SyntaxClass, matching: char) -> Self {
        SyntaxEntry {
            class,
            matching_char: Some(matching),
            flags: SyntaxFlags::empty(),
        }
    }

    /// Create an entry with specific flags.
    pub fn with_flags(class: SyntaxClass, flags: SyntaxFlags) -> Self {
        SyntaxEntry {
            class,
            matching_char: None,
            flags,
        }
    }

    /// Create a fully specified entry.
    pub fn full(class: SyntaxClass, matching: Option<char>, flags: SyntaxFlags) -> Self {
        SyntaxEntry {
            class,
            matching_char: matching,
            flags,
        }
    }

    /// Parse an Emacs syntax descriptor string into a `SyntaxEntry`.
    ///
    /// Format: `CLASS [MATCHING] [FLAGS...]`
    ///
    /// Examples:
    /// - `"w"`        → Word, no match, no flags
    /// - `"()"`       → Open, matching `)`, no flags
    /// - `"< 1"`      → Comment, flag 1 (COMSTART_FIRST)
    /// - `"> 4b"`     → EndComment, flag 4 + b
    /// - `"(}"`       → Open, matching `}`
    /// - `"_ 23"`     → Symbol, flags COMSTART_SECOND + COMEND_FIRST
    pub fn from_emacs_string(s: &str) -> Option<SyntaxEntry> {
        let chars: Vec<char> = s.chars().collect();
        if chars.is_empty() {
            return None;
        }

        let class = SyntaxClass::from_char(chars[0])?;

        // Second character: matching char (if it is not a space and not a
        // flag digit/letter).  Emacs treats a space as "no matching char".
        let (matching_char, flag_start) = if chars.len() > 1 {
            let ch = chars[1];
            if ch == ' ' {
                // Space means no matching char; flags start at index 2.
                (None, 2)
            } else if is_flag_char(ch) {
                // It is a flag character, not a matching char.
                (None, 1)
            } else {
                // It is a matching character.
                (Some(ch), 2)
            }
        } else {
            (None, 1)
        };

        let flags = if flag_start < chars.len() {
            SyntaxFlags::from_flag_chars(&chars[flag_start..])
        } else {
            SyntaxFlags::empty()
        };

        Some(SyntaxEntry {
            class,
            matching_char,
            flags,
        })
    }
}

/// Return `true` if `ch` is one of the recognized Emacs syntax flag chars.
fn is_flag_char(ch: char) -> bool {
    matches!(ch, '1' | '2' | '3' | '4' | 'p' | 'b' | 'n' | 'c')
}

impl fmt::Display for SyntaxEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.class.to_char())?;
        if let Some(m) = self.matching_char {
            write!(f, "{}", m)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Syntax Table
// ---------------------------------------------------------------------------

/// A mapping from characters to syntax entries.
///
/// Characters not explicitly mapped fall through to a default entry
/// (Whitespace with no flags, matching Emacs's `Sinherit` fallback
/// behaviour when no parent table is present).
#[derive(Debug, Clone)]
pub struct SyntaxTable {
    /// Explicit per-character entries.
    entries: HashMap<char, SyntaxEntry>,
    /// Default entry returned for unmapped characters.
    default_entry: SyntaxEntry,
}

impl SyntaxTable {
    /// Create an empty syntax table whose default entry is `Whitespace`.
    pub fn empty() -> Self {
        SyntaxTable {
            entries: HashMap::new(),
            default_entry: SyntaxEntry::new(SyntaxClass::Whitespace),
        }
    }

    /// Create a new syntax table pre-populated with the standard defaults
    /// (letters → Word, digits → Word, whitespace → Whitespace, etc.).
    ///
    /// This is equivalent to calling `standard_table()`.
    pub fn new() -> Self {
        standard_table()
    }

    /// Set the syntax entry for a character.
    pub fn set(&mut self, ch: char, entry: SyntaxEntry) {
        self.entries.insert(ch, entry);
    }

    /// Get the syntax entry for a character.  Falls back to the default
    /// entry if the character has no explicit mapping.
    pub fn get(&self, ch: char) -> &SyntaxEntry {
        self.entries.get(&ch).unwrap_or(&self.default_entry)
    }

    /// Set the default entry returned for unmapped characters.
    pub fn set_default(&mut self, entry: SyntaxEntry) {
        self.default_entry = entry;
    }

    /// Return the syntax class for a character (convenience shorthand).
    pub fn class_of(&self, ch: char) -> SyntaxClass {
        self.get(ch).class
    }

    /// Return the number of explicitly-mapped entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return `true` if no characters have explicit mappings.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for SyntaxTable {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Standard Syntax Table
// ---------------------------------------------------------------------------

/// Build and return the standard Emacs syntax table.
///
/// This matches the table constructed by `init_syntax_once()` in Emacs's
/// `syntax.c`.
pub fn standard_table() -> SyntaxTable {
    let mut table = SyntaxTable::empty();

    // --- Letters: Word ---
    for ch in 'a'..='z' {
        table.set(ch, SyntaxEntry::new(SyntaxClass::Word));
    }
    for ch in 'A'..='Z' {
        table.set(ch, SyntaxEntry::new(SyntaxClass::Word));
    }

    // --- Digits: Word (yes, word in the standard table) ---
    for ch in '0'..='9' {
        table.set(ch, SyntaxEntry::new(SyntaxClass::Word));
    }

    // --- Whitespace ---
    table.set(' ', SyntaxEntry::new(SyntaxClass::Whitespace));
    table.set('\t', SyntaxEntry::new(SyntaxClass::Whitespace));
    table.set('\n', SyntaxEntry::new(SyntaxClass::Whitespace));
    table.set('\r', SyntaxEntry::new(SyntaxClass::Whitespace));
    table.set('\x0c', SyntaxEntry::new(SyntaxClass::Whitespace)); // formfeed

    // --- Paired delimiters ---
    table.set('(', SyntaxEntry::with_match(SyntaxClass::Open, ')'));
    table.set(')', SyntaxEntry::with_match(SyntaxClass::Close, '('));
    table.set('[', SyntaxEntry::with_match(SyntaxClass::Open, ']'));
    table.set(']', SyntaxEntry::with_match(SyntaxClass::Close, '['));
    table.set('{', SyntaxEntry::with_match(SyntaxClass::Open, '}'));
    table.set('}', SyntaxEntry::with_match(SyntaxClass::Close, '{'));

    // --- String / Quote / Escape ---
    table.set('"', SyntaxEntry::new(SyntaxClass::String));
    table.set('\'', SyntaxEntry::new(SyntaxClass::Quote));
    table.set('\\', SyntaxEntry::new(SyntaxClass::Escape));

    // --- Symbol ---
    table.set('_', SyntaxEntry::new(SyntaxClass::Symbol));

    // --- Punctuation (everything else that is a printable ASCII
    //     non-alphanumeric, non-whitespace, not already mapped) ---
    let punct_chars = [
        '!', '#', '$', '%', '&', '*', '+', ',', '-', '.', '/', ':', ';', '<', '=', '>', '?', '@',
        '^', '`', '|', '~',
    ];
    for &ch in &punct_chars {
        table.set(ch, SyntaxEntry::new(SyntaxClass::Punct));
    }

    table
}

// ---------------------------------------------------------------------------
// Text Source Trait
// ---------------------------------------------------------------------------

/// Abstraction over text storage for syntax scanning functions.
///
/// This allows the scanning algorithms to work with `&str`, gap buffers,
/// ropes, or any other text representation.
pub trait TextSource {
    /// Return the character at byte-offset `pos`, or `None` if out of range.
    fn char_at(&self, pos: usize) -> Option<char>;

    /// Total length in characters.
    fn len(&self) -> usize;

    /// Whether the text is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Implement `TextSource` for `&str` — character-indexed.
///
/// Note: positions here are *character indices* (not byte offsets), which
/// matches how Emacs buffer positions work.
impl TextSource for str {
    fn char_at(&self, pos: usize) -> Option<char> {
        self.chars().nth(pos)
    }

    fn len(&self) -> usize {
        self.chars().count()
    }
}

/// Wrapper that pre-computes character vector for efficient random access.
pub struct CharVec {
    chars: Vec<char>,
}

impl CharVec {
    pub fn new(s: &str) -> Self {
        CharVec {
            chars: s.chars().collect(),
        }
    }
}

impl TextSource for CharVec {
    fn char_at(&self, pos: usize) -> Option<char> {
        self.chars.get(pos).copied()
    }

    fn len(&self) -> usize {
        self.chars.len()
    }
}

// ---------------------------------------------------------------------------
// Parse State
// ---------------------------------------------------------------------------

/// State tracked during incremental syntax scanning.
///
/// This mirrors the `struct lisp_parse_state` from Emacs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseState {
    /// Parenthesis nesting depth.
    pub depth: i32,
    /// If inside a string, the terminating character.  `None` if not in a string.
    pub in_string: Option<char>,
    /// Comment nesting status: 0 = not in comment, -1 = in unnested comment,
    /// >0 = nesting depth of nested comment.
    pub in_comment: i32,
    /// Comment style (0-3).
    pub comment_style: u8,
    /// `true` if the previous character was an escape / char-quote.
    pub quoted: bool,
    /// Minimum depth encountered during the scan (for detecting unmatched
    /// close-parens).
    pub min_depth: i32,
}

impl ParseState {
    /// Create a fresh parse state at top-level.
    pub fn new() -> Self {
        ParseState {
            depth: 0,
            in_string: None,
            in_comment: 0,
            comment_style: 0,
            quoted: false,
            min_depth: 0,
        }
    }

    /// Return `true` if the scanner is currently inside a comment.
    pub fn in_comment(&self) -> bool {
        self.in_comment != 0
    }

    /// Return `true` if the scanner is inside a string.
    pub fn in_string(&self) -> bool {
        self.in_string.is_some()
    }
}

impl Default for ParseState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Syntax Error
// ---------------------------------------------------------------------------

/// Errors that can arise during syntax scanning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxError {
    /// Unbalanced parentheses: ran out of text before matching a paren.
    UnbalancedParen,
    /// Reached beginning-of-buffer while scanning backward.
    BeginningOfBuffer,
    /// Reached end-of-buffer while scanning forward.
    EndOfBuffer,
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyntaxError::UnbalancedParen => write!(f, "Unbalanced parentheses"),
            SyntaxError::BeginningOfBuffer => write!(f, "Beginning of buffer"),
            SyntaxError::EndOfBuffer => write!(f, "End of buffer"),
        }
    }
}

impl std::error::Error for SyntaxError {}

// ---------------------------------------------------------------------------
// Core Scanning Functions
// ---------------------------------------------------------------------------

/// Scan forward or backward over `count` words starting at position `from`.
///
/// A "word" is a maximal run of characters whose syntax class is `Word`.
/// Returns the position after the last word scanned, or `None` if there
/// are not enough words.
///
/// - If `count > 0`, scan forward.
/// - If `count < 0`, scan backward.
/// - If `count == 0`, return `from` immediately.
pub fn scan_words<T: TextSource + ?Sized>(
    table: &SyntaxTable,
    text: &T,
    from: usize,
    count: i32,
) -> Option<usize> {
    if count == 0 {
        return Some(from);
    }

    let len = text.len();

    if count > 0 {
        // Forward word scanning.
        let mut pos = from;
        for _ in 0..count {
            // Skip non-word characters.
            while pos < len {
                if let Some(ch) = text.char_at(pos) {
                    if table.class_of(ch) == SyntaxClass::Word {
                        break;
                    }
                }
                pos += 1;
            }
            if pos >= len {
                return None;
            }
            // Skip word characters.
            while pos < len {
                if let Some(ch) = text.char_at(pos) {
                    if table.class_of(ch) != SyntaxClass::Word {
                        break;
                    }
                }
                pos += 1;
            }
        }
        Some(pos)
    } else {
        // Backward word scanning.
        let mut pos = from;
        for _ in 0..(-count) {
            // Skip non-word characters backward.
            while pos > 0 {
                pos -= 1;
                if let Some(ch) = text.char_at(pos) {
                    if table.class_of(ch) == SyntaxClass::Word {
                        pos += 1; // undo the decrement; we found a word char
                        break;
                    }
                }
            }
            // Now pos is one past the last word char, or 0.
            // If we are at position 0 and it is not a word char, fail.
            if pos == 0 {
                if let Some(ch) = text.char_at(0) {
                    if table.class_of(ch) != SyntaxClass::Word {
                        return None;
                    }
                } else {
                    return None;
                }
            }
            // Skip word characters backward.
            while pos > 0 {
                pos -= 1;
                if let Some(ch) = text.char_at(pos) {
                    if table.class_of(ch) != SyntaxClass::Word {
                        pos += 1;
                        break;
                    }
                }
            }
        }
        Some(pos)
    }
}

/// Scan forward past balanced parentheses.
///
/// Starting at `from`, scan forward (if `count > 0`) or backward (if
/// `count < 0`) past `count` balanced groups.
///
/// - `depth`: initial paren depth (0 for top-level).
/// - `sexpflag`: if `true`, stop after one complete sexp (like `forward-sexp`);
///   otherwise, stop only when depth changes by `count`.
///
/// Returns the position after scanning, or an error if parens are unbalanced
/// or we reach a buffer boundary.
pub fn scan_lists<T: TextSource + ?Sized>(
    table: &SyntaxTable,
    text: &T,
    from: usize,
    count: i32,
    depth: i32,
    sexpflag: bool,
) -> Result<usize, SyntaxError> {
    if count == 0 {
        return Ok(from);
    }

    let len = text.len();
    let mut current_depth = depth;

    if count > 0 {
        let mut pos = from;
        let mut remaining = count;

        while remaining > 0 {
            if pos >= len {
                return Err(if current_depth != 0 {
                    SyntaxError::UnbalancedParen
                } else {
                    SyntaxError::EndOfBuffer
                });
            }

            let ch = match text.char_at(pos) {
                Some(c) => c,
                None => return Err(SyntaxError::EndOfBuffer),
            };
            let entry = table.get(ch);

            match entry.class {
                SyntaxClass::Open => {
                    current_depth += 1;
                    pos += 1;
                    if sexpflag && current_depth == depth + 1 {
                        // We just entered a group; find its close.
                        pos = find_matching_close(table, text, pos, 1)?;
                        current_depth -= 1; // balanced: depth returns to pre-open
                        remaining -= 1;
                    }
                }
                SyntaxClass::Close => {
                    current_depth -= 1;
                    pos += 1;
                    if current_depth < depth {
                        if sexpflag {
                            return Err(SyntaxError::UnbalancedParen);
                        }
                        remaining -= 1;
                    }
                    if !sexpflag && current_depth == depth - count + remaining {
                        // Done!
                    }
                }
                SyntaxClass::String => {
                    // Skip over string.
                    let string_char = ch;
                    pos += 1;
                    pos = skip_string_forward(table, text, pos, string_char)?;
                    if sexpflag && current_depth == depth {
                        remaining -= 1;
                    }
                }
                SyntaxClass::StringFence => {
                    pos += 1;
                    pos = skip_string_fence_forward(table, text, pos)?;
                    if sexpflag && current_depth == depth {
                        remaining -= 1;
                    }
                }
                SyntaxClass::Escape | SyntaxClass::CharQuote => {
                    pos += 1; // skip escape
                    if pos < len {
                        pos += 1; // skip escaped char
                    }
                    if sexpflag && current_depth == depth {
                        remaining -= 1;
                    }
                }
                SyntaxClass::Word | SyntaxClass::Symbol => {
                    if sexpflag && current_depth == depth {
                        // A word/symbol is a sexp.  Scan to end of word/symbol.
                        let start_class = entry.class;
                        pos += 1;
                        while pos < len {
                            if let Some(c2) = text.char_at(pos) {
                                let cl = table.class_of(c2);
                                if cl != start_class
                                    && cl != SyntaxClass::Word
                                    && cl != SyntaxClass::Symbol
                                {
                                    break;
                                }
                            }
                            pos += 1;
                        }
                        remaining -= 1;
                    } else {
                        pos += 1;
                    }
                }
                SyntaxClass::Math => {
                    // Math delimiter is like a string but uses the same char.
                    pos += 1;
                    if sexpflag && current_depth == depth {
                        remaining -= 1;
                    }
                }
                SyntaxClass::Quote => {
                    // Expression prefix — skip it and the following sexp.
                    pos += 1;
                    // Do not decrement remaining; the prefix attaches to the next sexp.
                }
                _ => {
                    pos += 1;
                }
            }
        }

        if current_depth != depth && !sexpflag {
            return Err(SyntaxError::UnbalancedParen);
        }
        Ok(pos)
    } else {
        // Backward scanning.
        let mut pos = from;
        let mut remaining = -count;

        while remaining > 0 {
            if pos == 0 {
                return Err(if current_depth != 0 {
                    SyntaxError::UnbalancedParen
                } else {
                    SyntaxError::BeginningOfBuffer
                });
            }

            pos -= 1;
            let ch = match text.char_at(pos) {
                Some(c) => c,
                None => return Err(SyntaxError::BeginningOfBuffer),
            };
            let entry = table.get(ch);

            match entry.class {
                SyntaxClass::Close => {
                    current_depth += 1;
                    if sexpflag && current_depth == depth + 1 {
                        pos = find_matching_open(table, text, pos, 1)?;
                        current_depth -= 1; // balanced: depth returns to pre-close
                        remaining -= 1;
                    }
                }
                SyntaxClass::Open => {
                    current_depth -= 1;
                    if current_depth < depth {
                        if sexpflag {
                            return Err(SyntaxError::UnbalancedParen);
                        }
                        remaining -= 1;
                    }
                }
                SyntaxClass::String => {
                    let string_char = ch;
                    pos = skip_string_backward(table, text, pos, string_char)?;
                    if sexpflag && current_depth == depth {
                        remaining -= 1;
                    }
                }
                SyntaxClass::StringFence => {
                    pos = skip_string_fence_backward(table, text, pos)?;
                    if sexpflag && current_depth == depth {
                        remaining -= 1;
                    }
                }
                SyntaxClass::Word | SyntaxClass::Symbol => {
                    if sexpflag && current_depth == depth {
                        let start_class = entry.class;
                        while pos > 0 {
                            let prev = pos - 1;
                            if let Some(c2) = text.char_at(prev) {
                                let cl = table.class_of(c2);
                                if cl != start_class
                                    && cl != SyntaxClass::Word
                                    && cl != SyntaxClass::Symbol
                                {
                                    break;
                                }
                            }
                            pos = prev;
                        }
                        remaining -= 1;
                    }
                }
                SyntaxClass::Math => {
                    if sexpflag && current_depth == depth {
                        remaining -= 1;
                    }
                }
                _ => {}
            }
        }

        if current_depth != depth && !sexpflag {
            return Err(SyntaxError::UnbalancedParen);
        }
        Ok(pos)
    }
}

/// Helper: skip forward past a string terminated by `term_char`, handling
/// escapes.  `pos` should be the position *after* the opening string delimiter.
fn skip_string_forward<T: TextSource + ?Sized>(
    table: &SyntaxTable,
    text: &T,
    mut pos: usize,
    term_char: char,
) -> Result<usize, SyntaxError> {
    let len = text.len();
    while pos < len {
        let ch = text.char_at(pos).unwrap();
        pos += 1;
        if ch == term_char {
            return Ok(pos);
        }
        let entry = table.get(ch);
        if entry.class == SyntaxClass::Escape || entry.class == SyntaxClass::CharQuote {
            if pos < len {
                pos += 1; // skip escaped char
            }
        }
    }
    Err(SyntaxError::EndOfBuffer)
}

/// Helper: skip backward to find the opening string delimiter.
fn skip_string_backward<T: TextSource + ?Sized>(
    table: &SyntaxTable,
    text: &T,
    mut pos: usize,
    term_char: char,
) -> Result<usize, SyntaxError> {
    // `pos` is on the closing string delimiter.  Scan backward for the opener.
    while pos > 0 {
        pos -= 1;
        let ch = text.char_at(pos).unwrap();
        if ch == term_char {
            // Check this is not escaped.
            if !is_char_escaped(table, text, pos) {
                return Ok(pos);
            }
        }
    }
    Err(SyntaxError::BeginningOfBuffer)
}

/// Helper: skip forward past a StringFence-delimited string.
fn skip_string_fence_forward<T: TextSource + ?Sized>(
    table: &SyntaxTable,
    text: &T,
    mut pos: usize,
) -> Result<usize, SyntaxError> {
    let len = text.len();
    while pos < len {
        let ch = text.char_at(pos).unwrap();
        pos += 1;
        let entry = table.get(ch);
        if entry.class == SyntaxClass::StringFence {
            return Ok(pos);
        }
        if entry.class == SyntaxClass::Escape || entry.class == SyntaxClass::CharQuote {
            if pos < len {
                pos += 1;
            }
        }
    }
    Err(SyntaxError::EndOfBuffer)
}

/// Helper: skip backward to find the opening StringFence.
fn skip_string_fence_backward<T: TextSource + ?Sized>(
    table: &SyntaxTable,
    text: &T,
    mut pos: usize,
) -> Result<usize, SyntaxError> {
    while pos > 0 {
        pos -= 1;
        let ch = text.char_at(pos).unwrap();
        let entry = table.get(ch);
        if entry.class == SyntaxClass::StringFence {
            return Ok(pos);
        }
    }
    Err(SyntaxError::BeginningOfBuffer)
}

/// Helper: find the matching close paren, starting after the open paren.
fn find_matching_close<T: TextSource + ?Sized>(
    table: &SyntaxTable,
    text: &T,
    mut pos: usize,
    mut depth: i32,
) -> Result<usize, SyntaxError> {
    let len = text.len();
    while pos < len {
        let ch = text.char_at(pos).unwrap();
        let entry = table.get(ch);
        pos += 1;
        match entry.class {
            SyntaxClass::Open => depth += 1,
            SyntaxClass::Close => {
                depth -= 1;
                if depth == 0 {
                    return Ok(pos);
                }
            }
            SyntaxClass::String => {
                pos = skip_string_forward(table, text, pos, ch)?;
            }
            SyntaxClass::StringFence => {
                pos = skip_string_forward(table, text, pos, ch)?;
            }
            SyntaxClass::Escape | SyntaxClass::CharQuote => {
                if pos < len {
                    pos += 1;
                }
            }
            _ => {}
        }
    }
    Err(SyntaxError::UnbalancedParen)
}

/// Helper: find the matching open paren, scanning backward from *on* the close paren.
fn find_matching_open<T: TextSource + ?Sized>(
    table: &SyntaxTable,
    text: &T,
    mut pos: usize,
    mut depth: i32,
) -> Result<usize, SyntaxError> {
    while pos > 0 {
        pos -= 1;
        let ch = text.char_at(pos).unwrap();
        let entry = table.get(ch);
        match entry.class {
            SyntaxClass::Close => depth += 1,
            SyntaxClass::Open => {
                depth -= 1;
                if depth == 0 {
                    return Ok(pos);
                }
            }
            SyntaxClass::String => {
                pos = skip_string_backward(table, text, pos, ch)?;
            }
            _ => {}
        }
    }
    Err(SyntaxError::UnbalancedParen)
}

/// Check whether the character at `pos` is escaped (preceded by an odd
/// number of Escape / CharQuote characters).
fn is_char_escaped<T: TextSource + ?Sized>(table: &SyntaxTable, text: &T, pos: usize) -> bool {
    let mut esc_count = 0u32;
    let mut p = pos;
    while p > 0 {
        p -= 1;
        if let Some(ch) = text.char_at(p) {
            let cls = table.class_of(ch);
            if cls == SyntaxClass::Escape || cls == SyntaxClass::CharQuote {
                esc_count += 1;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    esc_count % 2 == 1
}

/// Skip forward past one comment, starting at `from` (which should be just
/// after the comment-start delimiter).
///
/// `style` is the comment style (0-3).
/// `nesting` is the current nesting depth (use 1 for non-nested, or initial
/// nesting for nested comments).
///
/// Returns `Some((new_pos, new_nesting))` on success, or `None` if the
/// comment is unterminated.
pub fn forward_comment<T: TextSource + ?Sized>(
    table: &SyntaxTable,
    text: &T,
    from: usize,
    style: u8,
    nesting: i32,
) -> Option<(usize, i32)> {
    let len = text.len();
    let mut pos = from;
    let mut depth = nesting;

    while pos < len {
        let ch = text.char_at(pos)?;
        let entry = table.get(ch);

        match entry.class {
            SyntaxClass::EndComment => {
                // Single-character comment ender.
                if entry.flags.comment_style() == style {
                    depth -= 1;
                    pos += 1;
                    if depth <= 0 {
                        return Some((pos, 0));
                    }
                } else {
                    pos += 1;
                }
            }
            SyntaxClass::CommentFence => {
                depth -= 1;
                pos += 1;
                if depth <= 0 {
                    return Some((pos, 0));
                }
            }
            _ => {
                // Check for 2-char comment end: first + second.
                if entry.flags.contains(SyntaxFlags::COMEND_FIRST) {
                    if pos + 1 < len {
                        if let Some(next) = text.char_at(pos + 1) {
                            let next_entry = table.get(next);
                            if next_entry.flags.contains(SyntaxFlags::COMEND_SECOND)
                                && next_entry.flags.comment_style() == style
                            {
                                depth -= 1;
                                pos += 2;
                                if depth <= 0 {
                                    return Some((pos, 0));
                                }
                                continue;
                            }
                        }
                    }
                }
                // Check for 2-char comment start (for nested comments).
                if entry.flags.contains(SyntaxFlags::COMSTART_FIRST)
                    && entry.flags.contains(SyntaxFlags::COMMENT_NESTED)
                {
                    if pos + 1 < len {
                        if let Some(next) = text.char_at(pos + 1) {
                            let next_entry = table.get(next);
                            if next_entry.flags.contains(SyntaxFlags::COMSTART_SECOND)
                                && next_entry.flags.comment_style() == style
                            {
                                depth += 1;
                                pos += 2;
                                continue;
                            }
                        }
                    }
                }
                // Handle escape inside comment.
                if entry.class == SyntaxClass::Escape || entry.class == SyntaxClass::CharQuote {
                    pos += 1;
                    if pos >= len {
                        return None;
                    }
                }
                pos += 1;
            }
        }
    }

    // Reached end of text without terminating comment.
    None
}

/// Full sexp scanner, equivalent to `scan-sexps` / `parse-partial-sexp` in Emacs.
///
/// Scans from `from` to `target`, updating `state` for every character.
/// Returns the resulting parse state after reaching `target`.
pub fn scan_sexps<T: TextSource + ?Sized>(
    table: &SyntaxTable,
    text: &T,
    from: usize,
    target: usize,
    state: ParseState,
) -> ParseState {
    let len = text.len();
    let target = target.min(len);
    let mut pos = from.min(len);
    let mut st = state;

    while pos < target {
        let ch = match text.char_at(pos) {
            Some(c) => c,
            None => break,
        };

        // --- Handle quoted state (previous char was escape) ---
        if st.quoted {
            st.quoted = false;
            pos += 1;
            continue;
        }

        // --- Inside a string ---
        if let Some(term) = st.in_string {
            if ch == term && !is_char_escaped(table, text, pos) {
                st.in_string = None;
            } else {
                let entry = table.get(ch);
                if entry.class == SyntaxClass::Escape || entry.class == SyntaxClass::CharQuote {
                    st.quoted = true;
                }
            }
            pos += 1;
            continue;
        }

        // --- Inside a comment ---
        if st.in_comment != 0 {
            let entry = table.get(ch);
            match entry.class {
                SyntaxClass::EndComment => {
                    if entry.flags.comment_style() == st.comment_style {
                        if st.in_comment > 0 {
                            st.in_comment -= 1;
                        } else {
                            st.in_comment = 0;
                        }
                    }
                    pos += 1;
                    continue;
                }
                SyntaxClass::CommentFence => {
                    if st.in_comment > 0 {
                        st.in_comment -= 1;
                    } else {
                        st.in_comment = 0;
                    }
                    pos += 1;
                    continue;
                }
                _ => {
                    // Check for 2-char comment end.
                    if entry.flags.contains(SyntaxFlags::COMEND_FIRST) && pos + 1 < target {
                        if let Some(next) = text.char_at(pos + 1) {
                            let next_entry = table.get(next);
                            if next_entry.flags.contains(SyntaxFlags::COMEND_SECOND)
                                && next_entry.flags.comment_style() == st.comment_style
                            {
                                if st.in_comment > 0 {
                                    st.in_comment -= 1;
                                } else {
                                    st.in_comment = 0;
                                }
                                pos += 2;
                                continue;
                            }
                        }
                    }
                    // Check for 2-char comment start (nested).
                    if entry.flags.contains(SyntaxFlags::COMSTART_FIRST) && pos + 1 < target {
                        if let Some(next) = text.char_at(pos + 1) {
                            let next_entry = table.get(next);
                            if next_entry.flags.contains(SyntaxFlags::COMSTART_SECOND)
                                && st.in_comment > 0
                            {
                                st.in_comment += 1;
                                pos += 2;
                                continue;
                            }
                        }
                    }
                }
            }
            pos += 1;
            continue;
        }

        // --- Top-level scanning ---
        let entry = table.get(ch);

        match entry.class {
            SyntaxClass::Escape | SyntaxClass::CharQuote => {
                st.quoted = true;
                pos += 1;
            }
            SyntaxClass::Open => {
                st.depth += 1;
                pos += 1;
            }
            SyntaxClass::Close => {
                st.depth -= 1;
                if st.depth < st.min_depth {
                    st.min_depth = st.depth;
                }
                pos += 1;
            }
            SyntaxClass::String => {
                st.in_string = Some(ch);
                pos += 1;
            }
            SyntaxClass::StringFence => {
                st.in_string = Some(ch);
                pos += 1;
            }
            SyntaxClass::Comment => {
                st.in_comment = -1;
                st.comment_style = entry.flags.comment_style();
                pos += 1;
            }
            SyntaxClass::CommentFence => {
                st.in_comment = -1;
                st.comment_style = entry.flags.comment_style();
                pos += 1;
            }
            _ => {
                // Check for 2-char comment start.
                if entry.flags.contains(SyntaxFlags::COMSTART_FIRST) && pos + 1 < target {
                    if let Some(next) = text.char_at(pos + 1) {
                        let next_entry = table.get(next);
                        if next_entry.flags.contains(SyntaxFlags::COMSTART_SECOND) {
                            let nested = entry.flags.contains(SyntaxFlags::COMMENT_NESTED)
                                || next_entry.flags.contains(SyntaxFlags::COMMENT_NESTED);
                            st.in_comment = if nested { 1 } else { -1 };
                            st.comment_style = next_entry.flags.comment_style();
                            pos += 2;
                            continue;
                        }
                    }
                }
                pos += 1;
            }
        }
    }

    // If we ended in a quoted state but ran out of text, keep it set.
    st
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- SyntaxClass tests --

    #[test]
    fn test_syntax_class_from_char_all() {
        assert_eq!(SyntaxClass::from_char(' '), Some(SyntaxClass::Whitespace));
        assert_eq!(SyntaxClass::from_char('-'), Some(SyntaxClass::Whitespace));
        assert_eq!(SyntaxClass::from_char('.'), Some(SyntaxClass::Punct));
        assert_eq!(SyntaxClass::from_char('w'), Some(SyntaxClass::Word));
        assert_eq!(SyntaxClass::from_char('_'), Some(SyntaxClass::Symbol));
        assert_eq!(SyntaxClass::from_char('('), Some(SyntaxClass::Open));
        assert_eq!(SyntaxClass::from_char(')'), Some(SyntaxClass::Close));
        assert_eq!(SyntaxClass::from_char('\''), Some(SyntaxClass::Quote));
        assert_eq!(SyntaxClass::from_char('"'), Some(SyntaxClass::String));
        assert_eq!(SyntaxClass::from_char('$'), Some(SyntaxClass::Math));
        assert_eq!(SyntaxClass::from_char('\\'), Some(SyntaxClass::Escape));
        assert_eq!(SyntaxClass::from_char('/'), Some(SyntaxClass::CharQuote));
        assert_eq!(SyntaxClass::from_char('<'), Some(SyntaxClass::Comment));
        assert_eq!(SyntaxClass::from_char('>'), Some(SyntaxClass::EndComment));
        assert_eq!(SyntaxClass::from_char('@'), Some(SyntaxClass::Inherit));
        assert_eq!(SyntaxClass::from_char('|'), Some(SyntaxClass::CommentFence));
        assert_eq!(SyntaxClass::from_char('!'), Some(SyntaxClass::StringFence));
    }

    #[test]
    fn test_syntax_class_from_char_unknown() {
        assert_eq!(SyntaxClass::from_char('z'), None);
        assert_eq!(SyntaxClass::from_char('A'), None);
        assert_eq!(SyntaxClass::from_char('5'), None);
    }

    #[test]
    fn test_syntax_class_roundtrip() {
        let classes = [
            SyntaxClass::Whitespace,
            SyntaxClass::Punct,
            SyntaxClass::Word,
            SyntaxClass::Symbol,
            SyntaxClass::Open,
            SyntaxClass::Close,
            SyntaxClass::Quote,
            SyntaxClass::String,
            SyntaxClass::Math,
            SyntaxClass::Escape,
            SyntaxClass::CharQuote,
            SyntaxClass::Comment,
            SyntaxClass::EndComment,
            SyntaxClass::Inherit,
            SyntaxClass::CommentFence,
            SyntaxClass::StringFence,
        ];
        for cls in &classes {
            let ch = cls.to_char();
            let back = SyntaxClass::from_char(ch).unwrap();
            assert_eq!(
                *cls, back,
                "Roundtrip failed for {:?} -> '{}' -> {:?}",
                cls, ch, back
            );
        }
    }

    #[test]
    fn test_syntax_class_is_word_like() {
        assert!(SyntaxClass::Word.is_word_like());
        assert!(SyntaxClass::Symbol.is_word_like());
        assert!(!SyntaxClass::Punct.is_word_like());
        assert!(!SyntaxClass::Whitespace.is_word_like());
    }

    // -- SyntaxFlags tests --

    #[test]
    fn test_flags_comment_style() {
        assert_eq!(SyntaxFlags::empty().comment_style(), 0);
        assert_eq!(SyntaxFlags::COMMENT_STYLE_B.comment_style(), 1);
        assert_eq!(SyntaxFlags::COMMENT_STYLE_C.comment_style(), 2);
        assert_eq!(
            (SyntaxFlags::COMMENT_STYLE_B | SyntaxFlags::COMMENT_STYLE_C).comment_style(),
            3
        );
    }

    #[test]
    fn test_flags_from_flag_chars() {
        let flags = SyntaxFlags::from_flag_chars(&['1', '2', 'b']);
        assert!(flags.contains(SyntaxFlags::COMSTART_FIRST));
        assert!(flags.contains(SyntaxFlags::COMSTART_SECOND));
        assert!(flags.contains(SyntaxFlags::COMMENT_STYLE_B));
        assert!(!flags.contains(SyntaxFlags::COMEND_FIRST));
    }

    #[test]
    fn test_flags_from_flag_chars_all() {
        let flags = SyntaxFlags::from_flag_chars(&['1', '2', '3', '4', 'p', 'b', 'n', 'c']);
        assert!(flags.contains(SyntaxFlags::COMSTART_FIRST));
        assert!(flags.contains(SyntaxFlags::COMSTART_SECOND));
        assert!(flags.contains(SyntaxFlags::COMEND_FIRST));
        assert!(flags.contains(SyntaxFlags::COMEND_SECOND));
        assert!(flags.contains(SyntaxFlags::PREFIX));
        assert!(flags.contains(SyntaxFlags::COMMENT_STYLE_B));
        assert!(flags.contains(SyntaxFlags::COMMENT_NESTED));
        assert!(flags.contains(SyntaxFlags::COMMENT_STYLE_C));
    }

    // -- SyntaxEntry parsing tests --

    #[test]
    fn test_entry_from_emacs_string_word() {
        let entry = SyntaxEntry::from_emacs_string("w").unwrap();
        assert_eq!(entry.class, SyntaxClass::Word);
        assert_eq!(entry.matching_char, None);
        assert_eq!(entry.flags, SyntaxFlags::empty());
    }

    #[test]
    fn test_entry_from_emacs_string_open_with_match() {
        let entry = SyntaxEntry::from_emacs_string("()").unwrap();
        assert_eq!(entry.class, SyntaxClass::Open);
        assert_eq!(entry.matching_char, Some(')'));
        assert_eq!(entry.flags, SyntaxFlags::empty());
    }

    #[test]
    fn test_entry_from_emacs_string_open_curly() {
        let entry = SyntaxEntry::from_emacs_string("(}").unwrap();
        assert_eq!(entry.class, SyntaxClass::Open);
        assert_eq!(entry.matching_char, Some('}'));
    }

    #[test]
    fn test_entry_from_emacs_string_comment_with_flags() {
        let entry = SyntaxEntry::from_emacs_string("< 1").unwrap();
        assert_eq!(entry.class, SyntaxClass::Comment);
        assert_eq!(entry.matching_char, None);
        assert!(entry.flags.contains(SyntaxFlags::COMSTART_FIRST));
    }

    #[test]
    fn test_entry_from_emacs_string_endcomment_flags() {
        let entry = SyntaxEntry::from_emacs_string("> 4b").unwrap();
        assert_eq!(entry.class, SyntaxClass::EndComment);
        assert!(entry.flags.contains(SyntaxFlags::COMEND_SECOND));
        assert!(entry.flags.contains(SyntaxFlags::COMMENT_STYLE_B));
    }

    #[test]
    fn test_entry_from_emacs_string_symbol_flags() {
        let entry = SyntaxEntry::from_emacs_string("_ 23").unwrap();
        assert_eq!(entry.class, SyntaxClass::Symbol);
        assert!(entry.flags.contains(SyntaxFlags::COMSTART_SECOND));
        assert!(entry.flags.contains(SyntaxFlags::COMEND_FIRST));
    }

    #[test]
    fn test_entry_from_emacs_string_empty() {
        assert!(SyntaxEntry::from_emacs_string("").is_none());
    }

    #[test]
    fn test_entry_from_emacs_string_invalid_class() {
        assert!(SyntaxEntry::from_emacs_string("Z").is_none());
    }

    // -- Standard Table tests --

    #[test]
    fn test_standard_table_letters() {
        let table = standard_table();
        for ch in 'a'..='z' {
            assert_eq!(
                table.class_of(ch),
                SyntaxClass::Word,
                "expected Word for '{}'",
                ch
            );
        }
        for ch in 'A'..='Z' {
            assert_eq!(
                table.class_of(ch),
                SyntaxClass::Word,
                "expected Word for '{}'",
                ch
            );
        }
    }

    #[test]
    fn test_standard_table_digits() {
        let table = standard_table();
        for ch in '0'..='9' {
            assert_eq!(
                table.class_of(ch),
                SyntaxClass::Word,
                "expected Word for '{}'",
                ch
            );
        }
    }

    #[test]
    fn test_standard_table_whitespace() {
        let table = standard_table();
        assert_eq!(table.class_of(' '), SyntaxClass::Whitespace);
        assert_eq!(table.class_of('\t'), SyntaxClass::Whitespace);
        assert_eq!(table.class_of('\n'), SyntaxClass::Whitespace);
        assert_eq!(table.class_of('\x0c'), SyntaxClass::Whitespace);
    }

    #[test]
    fn test_standard_table_parens() {
        let table = standard_table();
        let entry = table.get('(');
        assert_eq!(entry.class, SyntaxClass::Open);
        assert_eq!(entry.matching_char, Some(')'));

        let entry = table.get(')');
        assert_eq!(entry.class, SyntaxClass::Close);
        assert_eq!(entry.matching_char, Some('('));

        let entry = table.get('[');
        assert_eq!(entry.class, SyntaxClass::Open);
        assert_eq!(entry.matching_char, Some(']'));

        let entry = table.get('{');
        assert_eq!(entry.class, SyntaxClass::Open);
        assert_eq!(entry.matching_char, Some('}'));
    }

    #[test]
    fn test_standard_table_string_quote_escape() {
        let table = standard_table();
        assert_eq!(table.class_of('"'), SyntaxClass::String);
        assert_eq!(table.class_of('\''), SyntaxClass::Quote);
        assert_eq!(table.class_of('\\'), SyntaxClass::Escape);
    }

    #[test]
    fn test_standard_table_symbol() {
        let table = standard_table();
        assert_eq!(table.class_of('_'), SyntaxClass::Symbol);
    }

    #[test]
    fn test_standard_table_punct() {
        let table = standard_table();
        assert_eq!(table.class_of('<'), SyntaxClass::Punct);
        assert_eq!(table.class_of('>'), SyntaxClass::Punct);
        assert_eq!(table.class_of('+'), SyntaxClass::Punct);
        assert_eq!(table.class_of('-'), SyntaxClass::Punct);
        assert_eq!(table.class_of('='), SyntaxClass::Punct);
    }

    // -- SyntaxTable set/get tests --

    #[test]
    fn test_table_set_get() {
        let mut table = SyntaxTable::empty();
        assert_eq!(table.class_of('x'), SyntaxClass::Whitespace); // default

        table.set('x', SyntaxEntry::new(SyntaxClass::Word));
        assert_eq!(table.class_of('x'), SyntaxClass::Word);
    }

    // -- TextSource tests --

    #[test]
    fn test_text_source_str() {
        let s = "hello";
        assert_eq!(TextSource::char_at(s, 0), Some('h'));
        assert_eq!(TextSource::char_at(s, 4), Some('o'));
        assert_eq!(TextSource::char_at(s, 5), None);
        assert_eq!(TextSource::len(s), 5);
    }

    #[test]
    fn test_text_source_char_vec() {
        let v = CharVec::new("hello");
        assert_eq!(v.char_at(0), Some('h'));
        assert_eq!(v.char_at(4), Some('o'));
        assert_eq!(v.char_at(5), None);
        assert_eq!(v.len(), 5);
    }

    // -- scan_words tests --

    #[test]
    fn test_scan_words_forward_one() {
        let table = standard_table();
        let text = "hello world";
        // From 0, scan 1 word forward → end of "hello" = position 5
        let result = scan_words(&table, text, 0, 1);
        assert_eq!(result, Some(5));
    }

    #[test]
    fn test_scan_words_forward_two() {
        let table = standard_table();
        let text = "hello world";
        let result = scan_words(&table, text, 0, 2);
        assert_eq!(result, Some(11));
    }

    #[test]
    fn test_scan_words_forward_too_many() {
        let table = standard_table();
        let text = "hello";
        let result = scan_words(&table, text, 0, 2);
        assert_eq!(result, None); // only 1 word
    }

    #[test]
    fn test_scan_words_backward_one() {
        let table = standard_table();
        let text = "hello world";
        // From end (11), scan 1 word backward → start of "world" = position 6
        let result = scan_words(&table, text, 11, -1);
        assert_eq!(result, Some(6));
    }

    #[test]
    fn test_scan_words_backward_two() {
        let table = standard_table();
        let text = "hello world";
        // From end, scan 2 words backward → start of "hello" = position 0
        let result = scan_words(&table, text, 11, -2);
        assert_eq!(result, Some(0));
    }

    #[test]
    fn test_scan_words_zero() {
        let table = standard_table();
        let result = scan_words(&table, "hello", 3, 0);
        assert_eq!(result, Some(3));
    }

    // -- scan_lists tests --

    #[test]
    fn test_scan_lists_simple_parens() {
        let table = standard_table();
        let text = "(abc)";
        // sexpflag=true: forward 1 sexp from 0 → should land at 5 (after ')')
        let result = scan_lists(&table, text, 0, 1, 0, true);
        assert_eq!(result, Ok(5));
    }

    #[test]
    fn test_scan_lists_nested_parens() {
        let table = standard_table();
        let text = "(a (b) c)";
        let result = scan_lists(&table, text, 0, 1, 0, true);
        assert_eq!(result, Ok(9));
    }

    #[test]
    fn test_scan_lists_unbalanced() {
        let table = standard_table();
        let text = "(abc";
        let result = scan_lists(&table, text, 0, 1, 0, true);
        assert!(result.is_err());
    }

    #[test]
    fn test_scan_lists_backward() {
        let table = standard_table();
        let text = "(abc)";
        // From position 5 (after ')'), scan backward 1 sexp.
        let result = scan_lists(&table, text, 5, -1, 0, true);
        assert_eq!(result, Ok(0));
    }

    #[test]
    fn test_scan_lists_string_inside_parens() {
        let table = standard_table();
        let text = r#"("hello")"#;
        let result = scan_lists(&table, text, 0, 1, 0, true);
        assert_eq!(result, Ok(9));
    }

    #[test]
    fn test_scan_lists_word_sexp() {
        let table = standard_table();
        let text = "hello world";
        // sexpflag=true: forward 1 sexp from 0 → end of first word/symbol
        let result = scan_lists(&table, text, 0, 1, 0, true);
        assert_eq!(result, Ok(5));
    }

    // -- forward_comment tests --

    #[test]
    fn test_forward_comment_single_char() {
        // Build a table with `;` as single-char comment-start and `\n` as
        // single-char comment-end.
        let mut table = standard_table();
        table.set(';', SyntaxEntry::new(SyntaxClass::Comment));
        table.set('\n', SyntaxEntry::new(SyntaxClass::EndComment));

        let text = "; this is a comment\nnext line";
        // Start scanning from position 1 (after the `;`), style 0, nesting -1.
        let result = forward_comment(&table, text, 1, 0, 1);
        // Should stop after the newline at position 20.
        assert_eq!(result, Some((20, 0)));
    }

    #[test]
    fn test_forward_comment_unterminated() {
        let mut table = standard_table();
        table.set(';', SyntaxEntry::new(SyntaxClass::Comment));
        table.set('\n', SyntaxEntry::new(SyntaxClass::EndComment));

        let text = "; no newline";
        let result = forward_comment(&table, text, 1, 0, 1);
        assert_eq!(result, None); // unterminated
    }

    // -- scan_sexps / ParseState tests --

    #[test]
    fn test_scan_sexps_string_detection() {
        let table = standard_table();
        let text = r#"before "inside" after"#;
        let state = ParseState::new();
        // Scan across the opening quote.
        let st = scan_sexps(&table, text, 0, 8, state);
        // At position 8 we are inside the string.
        assert!(st.in_string());
        assert_eq!(st.in_string, Some('"'));
    }

    #[test]
    fn test_scan_sexps_string_closed() {
        let table = standard_table();
        let text = r#""hello""#;
        let state = ParseState::new();
        let st = scan_sexps(&table, text, 0, 7, state);
        // The string is opened and closed.
        assert!(!st.in_string());
    }

    #[test]
    fn test_scan_sexps_paren_depth() {
        let table = standard_table();
        let text = "((a))";
        let state = ParseState::new();
        let st = scan_sexps(&table, text, 0, 2, state);
        assert_eq!(st.depth, 2);
    }

    #[test]
    fn test_scan_sexps_paren_balanced() {
        let table = standard_table();
        let text = "((a))";
        let state = ParseState::new();
        let st = scan_sexps(&table, text, 0, 5, state);
        assert_eq!(st.depth, 0);
    }

    #[test]
    fn test_scan_sexps_unmatched_close() {
        let table = standard_table();
        let text = "a)b";
        let state = ParseState::new();
        let st = scan_sexps(&table, text, 0, 3, state);
        assert_eq!(st.depth, -1);
        assert_eq!(st.min_depth, -1);
    }

    #[test]
    fn test_scan_sexps_escape() {
        let table = standard_table();
        let text = r#""\""#; // three chars: " \ "
        let state = ParseState::new();
        let st = scan_sexps(&table, text, 0, 3, state);
        // Position 0: opens string
        // Position 1: escape, sets quoted
        // Position 2: escaped quote, stays in string
        assert!(st.in_string());
    }

    #[test]
    fn test_parse_state_default() {
        let st = ParseState::new();
        assert_eq!(st.depth, 0);
        assert_eq!(st.in_string, None);
        assert_eq!(st.in_comment, 0);
        assert_eq!(st.comment_style, 0);
        assert!(!st.quoted);
        assert_eq!(st.min_depth, 0);
    }

    // -- Two-char comment start/end via flags --

    #[test]
    fn test_two_char_comment_start_via_scan_sexps() {
        // Set up C-style: '/' has COMSTART_FIRST, '*' has COMSTART_SECOND
        // and COMEND_FIRST, '/' also has COMEND_SECOND.
        let mut table = standard_table();
        table.set(
            '/',
            SyntaxEntry::full(
                SyntaxClass::Punct,
                None,
                SyntaxFlags::COMSTART_FIRST | SyntaxFlags::COMEND_SECOND,
            ),
        );
        table.set(
            '*',
            SyntaxEntry::full(
                SyntaxClass::Punct,
                None,
                SyntaxFlags::COMSTART_SECOND | SyntaxFlags::COMEND_FIRST,
            ),
        );

        let text = "a /* comment */ b";
        let state = ParseState::new();
        let st = scan_sexps(&table, text, 0, 4, state);
        // After scanning to position 4 (past "a /*"), we should be in a comment.
        assert!(st.in_comment());
    }

    // -- CharVec efficiency --

    #[test]
    fn test_char_vec_unicode() {
        let v = CharVec::new("he\u{1F600}lo");
        assert_eq!(v.len(), 5);
        assert_eq!(v.char_at(0), Some('h'));
        assert_eq!(v.char_at(2), Some('\u{1F600}'));
        assert_eq!(v.char_at(4), Some('o'));
    }

    // -- Edge cases --

    #[test]
    fn test_scan_words_from_middle() {
        let table = standard_table();
        let text = "  hello  world  ";
        // Starting from position 3 (inside "hello"), forward 1 word.
        // Should skip remaining word chars to end of "hello" = position 7.
        let result = scan_words(&table, text, 3, 1);
        assert_eq!(result, Some(7));
    }

    #[test]
    fn test_scan_lists_multiple_sexps() {
        let table = standard_table();
        let text = "(a) (b) (c)";
        // Forward 2 sexps from 0.
        let result = scan_lists(&table, text, 0, 2, 0, true);
        assert_eq!(result, Ok(7));
    }

    #[test]
    fn test_scan_lists_escape_in_parens() {
        let table = standard_table();
        let text = r"(a\)b)";
        // The \) should be an escaped close-paren, not matching.
        let result = scan_lists(&table, text, 0, 1, 0, true);
        assert_eq!(result, Ok(6));
    }

    #[test]
    fn test_is_char_escaped_basic() {
        let table = standard_table();
        let text = r#"a\"b"#;
        // text is: a \ " b  (positions 0,1,2,3)
        assert!(!is_char_escaped(&table, text, 0)); // 'a' not escaped
        assert!(!is_char_escaped(&table, text, 1)); // '\' not escaped
        assert!(is_char_escaped(&table, text, 2)); // '"' is escaped by '\'
        assert!(!is_char_escaped(&table, text, 3)); // 'b' not escaped
    }

    #[test]
    fn test_is_char_escaped_double() {
        let table = standard_table();
        let text = r#"a\\"#;
        // text is: a \ \  (positions 0,1,2)
        // The second '\' at position 2 is preceded by exactly one escape char,
        // so it IS escaped.
        assert!(is_char_escaped(&table, text, 2));
    }

    #[test]
    fn test_is_char_escaped_triple() {
        let table = standard_table();
        let text = r#"a\\\"#;
        // text is: a \ \ \  (positions 0,1,2,3)
        // Position 3 is preceded by two escape chars (positions 1,2) → even count → NOT escaped.
        assert!(!is_char_escaped(&table, text, 3));
    }
}
