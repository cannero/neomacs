//! Syntax table system for the Elisp VM.
//!
//! Implements Emacs-compatible syntax tables with character classification,
//! motion functions (forward/backward word, sexp scanning), and the
//! `string-to-syntax` descriptor parser.

use std::cell::RefCell;
use std::collections::HashMap;

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::value::{Value, read_cons, with_heap};
use crate::buffer::{Buffer, BufferManager};

thread_local! {
    static STANDARD_SYNTAX_TABLE_OBJECT: RefCell<Option<Value>> = const { RefCell::new(None) };
}

/// Clear cached thread-local syntax table (must be called when heap changes).
pub fn reset_syntax_thread_locals() {
    STANDARD_SYNTAX_TABLE_OBJECT.with(|slot| *slot.borrow_mut() = None);
}

/// Restore the canonical standard syntax-table object for the current thread.
///
/// GNU Emacs keeps the standard syntax table as a single canonical Lisp object.
/// NeoVM exposes it through a thread-local cache because `standard-syntax-table`
/// is currently a no-evaluator builtin; callers that reconstruct or move an
/// `Evaluator` between threads must restore that identity explicitly.
pub(crate) fn restore_standard_syntax_table_object(table: Value) {
    STANDARD_SYNTAX_TABLE_OBJECT.with(|slot| *slot.borrow_mut() = Some(table));
}

/// Snapshot the current thread's canonical standard syntax-table object.
pub(crate) fn snapshot_standard_syntax_table_object() -> Option<Value> {
    STANDARD_SYNTAX_TABLE_OBJECT.with(|slot| *slot.borrow())
}

/// Collect GC roots from the cached syntax table.
pub fn collect_syntax_gc_roots(roots: &mut Vec<Value>) {
    STANDARD_SYNTAX_TABLE_OBJECT.with(|slot| {
        if let Some(v) = *slot.borrow() {
            roots.push(v);
        }
    });
}

const SYNTAX_TABLE_OBJECT_PROPERTY: &str = "syntax-table-object";

// ===========================================================================
// Syntax classes
// ===========================================================================

/// Emacs syntax classes, matching the designator characters used in
/// `string-to-syntax` and `modify-syntax-entry`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SyntaxClass {
    /// ' ' — Whitespace
    Whitespace,
    /// 'w' — Word constituent
    Word,
    /// '_' — Symbol constituent
    Symbol,
    /// '.' — Punctuation
    Punctuation,
    /// '(' — Open parenthesis/bracket
    Open,
    /// ')' — Close parenthesis/bracket
    Close,
    /// '\'' — Expression prefix
    Prefix,
    /// '"' — String delimiter
    StringDelim,
    /// '$' — Math delimiter (paired)
    MathDelim,
    /// '\\' — Escape character
    Escape,
    /// '/' — Character quote (only quotes the next character)
    CharQuote,
    /// '<' — Comment starter
    Comment,
    /// '>' — Comment ender
    EndComment,
    /// '@' — Inherit from standard syntax table
    InheritStandard,
    /// '!' — Generic comment delimiter
    Generic,
    /// '|' — Generic string fence (pairs with itself, like `"` but independent)
    StringFence,
}

impl SyntaxClass {
    /// Parse a syntax class from its single-character designator.
    pub fn from_char(ch: char) -> Option<SyntaxClass> {
        match ch {
            ' ' | '-' => Some(SyntaxClass::Whitespace),
            'w' => Some(SyntaxClass::Word),
            '_' => Some(SyntaxClass::Symbol),
            '.' => Some(SyntaxClass::Punctuation),
            '(' => Some(SyntaxClass::Open),
            ')' => Some(SyntaxClass::Close),
            '\'' => Some(SyntaxClass::Prefix),
            '"' => Some(SyntaxClass::StringDelim),
            '$' => Some(SyntaxClass::MathDelim),
            '\\' => Some(SyntaxClass::Escape),
            '/' => Some(SyntaxClass::CharQuote),
            '<' => Some(SyntaxClass::Comment),
            '>' => Some(SyntaxClass::EndComment),
            '@' => Some(SyntaxClass::InheritStandard),
            '!' => Some(SyntaxClass::Generic),
            '|' => Some(SyntaxClass::StringFence),
            _ => None,
        }
    }

    /// Return the canonical single-character designator for this class.
    pub fn to_char(self) -> char {
        match self {
            SyntaxClass::Whitespace => ' ',
            SyntaxClass::Word => 'w',
            SyntaxClass::Symbol => '_',
            SyntaxClass::Punctuation => '.',
            SyntaxClass::Open => '(',
            SyntaxClass::Close => ')',
            SyntaxClass::Prefix => '\'',
            SyntaxClass::StringDelim => '"',
            SyntaxClass::MathDelim => '$',
            SyntaxClass::Escape => '\\',
            SyntaxClass::CharQuote => '/',
            SyntaxClass::Comment => '<',
            SyntaxClass::EndComment => '>',
            SyntaxClass::InheritStandard => '@',
            SyntaxClass::Generic => '!',
            SyntaxClass::StringFence => '|',
        }
    }

    /// Return the integer code Emacs uses for this syntax class
    /// (used in the cons cell returned by `string-to-syntax`).
    pub fn code(self) -> i64 {
        match self {
            SyntaxClass::Whitespace => 0,
            SyntaxClass::Punctuation => 1,
            SyntaxClass::Word => 2,
            SyntaxClass::Symbol => 3,
            SyntaxClass::Open => 4,
            SyntaxClass::Close => 5,
            SyntaxClass::Prefix => 6,
            SyntaxClass::StringDelim => 7,
            SyntaxClass::MathDelim => 8,
            SyntaxClass::Escape => 9,
            SyntaxClass::CharQuote => 10,
            SyntaxClass::Comment => 11,
            SyntaxClass::EndComment => 12,
            SyntaxClass::InheritStandard => 13,
            SyntaxClass::Generic => 14,
            SyntaxClass::StringFence => 15,
        }
    }

    /// Parse a syntax class from its integer code (inverse of `code()`).
    pub fn from_code(n: i64) -> Option<SyntaxClass> {
        match n & 0xFF {
            0 => Some(SyntaxClass::Whitespace),
            1 => Some(SyntaxClass::Punctuation),
            2 => Some(SyntaxClass::Word),
            3 => Some(SyntaxClass::Symbol),
            4 => Some(SyntaxClass::Open),
            5 => Some(SyntaxClass::Close),
            6 => Some(SyntaxClass::Prefix),
            7 => Some(SyntaxClass::StringDelim),
            8 => Some(SyntaxClass::MathDelim),
            9 => Some(SyntaxClass::Escape),
            10 => Some(SyntaxClass::CharQuote),
            11 => Some(SyntaxClass::Comment),
            12 => Some(SyntaxClass::EndComment),
            13 => Some(SyntaxClass::InheritStandard),
            14 => Some(SyntaxClass::Generic),
            15 => Some(SyntaxClass::StringFence),
            _ => None,
        }
    }
}

// ===========================================================================
// Syntax flags
// ===========================================================================

/// Flags for comment style and prefix behavior, mirroring Emacs syntax flags.
///
/// Uses a raw `u8` bitmask to avoid external dependencies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SyntaxFlags(u8);

impl SyntaxFlags {
    /// '1' — first char of a two-char comment start sequence
    pub const COMMENT_START_FIRST: SyntaxFlags = SyntaxFlags(0b0000_0001);
    /// '2' — second char of a two-char comment start sequence
    pub const COMMENT_START_SECOND: SyntaxFlags = SyntaxFlags(0b0000_0010);
    /// '3' — first char of a two-char comment end sequence
    pub const COMMENT_END_FIRST: SyntaxFlags = SyntaxFlags(0b0000_0100);
    /// '4' — second char of a two-char comment end sequence
    pub const COMMENT_END_SECOND: SyntaxFlags = SyntaxFlags(0b0000_1000);
    /// 'p' — prefix character (e.g., quote, backquote)
    pub const PREFIX: SyntaxFlags = SyntaxFlags(0b0001_0000);
    /// 'b' — belongs to alternative "b" comment style
    pub const COMMENT_STYLE_B: SyntaxFlags = SyntaxFlags(0b0010_0000);
    /// 'n' — nestable comment
    pub const COMMENT_NESTABLE: SyntaxFlags = SyntaxFlags(0b0100_0000);
    /// 'c' — belongs to alternative "c" comment style
    pub const COMMENT_STYLE_C: SyntaxFlags = SyntaxFlags(0b1000_0000);

    /// Construct from raw bits.
    pub const fn new(bits: u8) -> Self {
        SyntaxFlags(bits)
    }

    /// Empty flags (no bits set).
    pub const fn empty() -> Self {
        SyntaxFlags(0)
    }

    /// Whether no flags are set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether `self` contains all the bits of `other`.
    pub const fn contains(self, other: SyntaxFlags) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Return the raw bits.
    pub const fn bits(self) -> u8 {
        self.0
    }
}

impl std::ops::BitOr for SyntaxFlags {
    type Output = SyntaxFlags;
    fn bitor(self, rhs: SyntaxFlags) -> SyntaxFlags {
        SyntaxFlags(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for SyntaxFlags {
    fn bitor_assign(&mut self, rhs: SyntaxFlags) {
        self.0 |= rhs.0;
    }
}

// ===========================================================================
// SyntaxEntry
// ===========================================================================

/// A single entry in a syntax table: the class, an optional matching
/// character (for parens/string delimiters), and comment/prefix flags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxEntry {
    pub class: SyntaxClass,
    pub matching_char: Option<char>,
    pub flags: SyntaxFlags,
}

impl SyntaxEntry {
    /// Create a simple entry with no matching char or flags.
    pub fn simple(class: SyntaxClass) -> Self {
        Self {
            class,
            matching_char: None,
            flags: SyntaxFlags::empty(),
        }
    }

    /// Create an entry with a matching character (for open/close parens).
    pub fn with_match(class: SyntaxClass, matching: char) -> Self {
        Self {
            class,
            matching_char: Some(matching),
            flags: SyntaxFlags::empty(),
        }
    }
}

// ===========================================================================
// string-to-syntax parser
// ===========================================================================

/// Parse an Emacs syntax descriptor string (e.g., `" "`, `"w"`, `"()"`,
/// `". 12"`) into a `SyntaxEntry`.
pub fn string_to_syntax(s: &str) -> Result<SyntaxEntry, String> {
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return Err("Empty syntax descriptor".to_string());
    }

    let class = SyntaxClass::from_char(chars[0])
        .ok_or_else(|| format!("Invalid syntax class character: '{}'", chars[0]))?;

    let matching_char = if chars.len() > 1 && chars[1] != ' ' {
        Some(chars[1])
    } else {
        None
    };

    let mut flags = SyntaxFlags::empty();
    // Flags start at position 2 (after class + matching char).
    let flag_start = if chars.len() > 1 { 2 } else { 1 };
    for &ch in chars.get(flag_start..).unwrap_or(&[]) {
        match ch {
            '1' => flags |= SyntaxFlags::COMMENT_START_FIRST,
            '2' => flags |= SyntaxFlags::COMMENT_START_SECOND,
            '3' => flags |= SyntaxFlags::COMMENT_END_FIRST,
            '4' => flags |= SyntaxFlags::COMMENT_END_SECOND,
            'p' => flags |= SyntaxFlags::PREFIX,
            'b' => flags |= SyntaxFlags::COMMENT_STYLE_B,
            'n' => flags |= SyntaxFlags::COMMENT_NESTABLE,
            'c' => flags |= SyntaxFlags::COMMENT_STYLE_C,
            ' ' => {} // whitespace in flag area is ignored
            _ => {}   // Emacs silently ignores unknown flags
        }
    }

    Ok(SyntaxEntry {
        class,
        matching_char,
        flags,
    })
}

/// Convert a `SyntaxEntry` into the Emacs cons-cell representation
/// returned by `string-to-syntax`: `(CODE . MATCHING-CHAR-OR-NIL)`.
///
/// The CODE is computed as: `(class_code) | (flags << 16)`.
pub fn syntax_entry_to_value(entry: &SyntaxEntry) -> Value {
    let code = entry.class.code() | ((entry.flags.bits() as i64) << 16);
    let matching = match entry.matching_char {
        Some(ch) => Value::Int(ch as i64),
        None => Value::Nil,
    };
    Value::cons(Value::Int(code), matching)
}

// ===========================================================================
// SyntaxTable
// ===========================================================================

/// An Emacs-style syntax table mapping characters to syntax entries.
///
/// Characters not explicitly set fall back to a parent table (if present)
/// or to the built-in standard defaults.
#[derive(Clone, Debug)]
pub struct SyntaxTable {
    /// Per-character overrides.
    entries: HashMap<char, SyntaxEntry>,
    /// Optional parent table for inheritance.
    parent: Option<Box<SyntaxTable>>,
}

impl SyntaxTable {
    // -- Construction --------------------------------------------------------

    /// Create the standard Emacs syntax table with ASCII defaults.
    pub fn new_standard() -> Self {
        let mut entries = HashMap::new();

        // Control characters are punctuation by default, except a few
        // whitespace characters explicitly reset below.
        for cp in 0u32..=(' ' as u32 - 1) {
            if let Some(ch) = char::from_u32(cp) {
                entries.insert(ch, SyntaxEntry::simple(SyntaxClass::Punctuation));
            }
        }
        entries.insert('\u{007f}', SyntaxEntry::simple(SyntaxClass::Punctuation));

        // Whitespace.
        for ch in [' ', '\t', '\n', '\r', '\u{000c}'] {
            entries.insert(ch, SyntaxEntry::simple(SyntaxClass::Whitespace));
        }

        // Word constituents: a-z, A-Z, 0-9, '$', '%'.
        for ch in 'a'..='z' {
            entries.insert(ch, SyntaxEntry::simple(SyntaxClass::Word));
        }
        for ch in 'A'..='Z' {
            entries.insert(ch, SyntaxEntry::simple(SyntaxClass::Word));
        }
        for ch in '0'..='9' {
            entries.insert(ch, SyntaxEntry::simple(SyntaxClass::Word));
        }
        entries.insert('$', SyntaxEntry::simple(SyntaxClass::Word));
        entries.insert('%', SyntaxEntry::simple(SyntaxClass::Word));

        // Parentheses (with matching chars).
        entries.insert('(', SyntaxEntry::with_match(SyntaxClass::Open, ')'));
        entries.insert(')', SyntaxEntry::with_match(SyntaxClass::Close, '('));
        entries.insert('[', SyntaxEntry::with_match(SyntaxClass::Open, ']'));
        entries.insert(']', SyntaxEntry::with_match(SyntaxClass::Close, '['));
        entries.insert('{', SyntaxEntry::with_match(SyntaxClass::Open, '}'));
        entries.insert('}', SyntaxEntry::with_match(SyntaxClass::Close, '{'));

        // String delimiter
        entries.insert('"', SyntaxEntry::simple(SyntaxClass::StringDelim));

        // Escape
        entries.insert('\\', SyntaxEntry::simple(SyntaxClass::Escape));

        // Symbol constituents.
        for ch in ['_', '-', '+', '*', '/', '&', '|', '<', '>', '='] {
            entries.insert(ch, SyntaxEntry::simple(SyntaxClass::Symbol));
        }

        // Punctuation.
        for ch in ['.', ',', ';', ':', '?', '!', '#', '@', '~', '^', '\'', '`'] {
            entries.insert(ch, SyntaxEntry::simple(SyntaxClass::Punctuation));
        }

        Self {
            entries,
            parent: None,
        }
    }

    /// Create a new syntax table that inherits from the standard table.
    pub fn make_syntax_table() -> Self {
        Self {
            entries: HashMap::new(),
            parent: Some(Box::new(Self::new_standard())),
        }
    }

    /// Create a copy of this syntax table (deep clone).
    pub fn copy_syntax_table(&self) -> Self {
        self.clone()
    }

    // -- Queries -------------------------------------------------------------

    /// Look up the syntax entry for `ch`.
    pub fn get_entry(&self, ch: char) -> Option<&SyntaxEntry> {
        self.entries
            .get(&ch)
            .or_else(|| self.parent.as_ref().and_then(|p| p.get_entry(ch)))
    }

    /// Return the syntax class for `ch`.
    pub fn char_syntax(&self, ch: char) -> SyntaxClass {
        self.get_entry(ch).map(|e| e.class).unwrap_or_else(|| {
            if u32::from(ch) >= 0x80 {
                SyntaxClass::Word
            } else {
                SyntaxClass::Whitespace
            }
        })
    }

    // -- Mutation -------------------------------------------------------------

    /// Set the syntax entry for `ch`.
    pub fn modify_syntax_entry(&mut self, ch: char, entry: SyntaxEntry) {
        self.entries.insert(ch, entry);
    }

    // pdump accessors
    pub(crate) fn dump_entries(&self) -> &HashMap<char, SyntaxEntry> {
        &self.entries
    }
    pub(crate) fn dump_parent(&self) -> &Option<Box<SyntaxTable>> {
        &self.parent
    }
    pub(crate) fn from_dump(
        entries: HashMap<char, SyntaxEntry>,
        parent: Option<Box<SyntaxTable>>,
    ) -> Self {
        Self { entries, parent }
    }
}

impl Default for SyntaxTable {
    fn default() -> Self {
        Self::new_standard()
    }
}

// ===========================================================================
// Motion functions (operate on a Buffer + SyntaxTable)
// ===========================================================================

/// Move forward over `count` words.  Returns the resulting byte position.
///
/// A "word" is a maximal run of characters with syntax class `Word`.
/// Between words, non-word characters are skipped.
pub fn forward_word(buf: &Buffer, table: &SyntaxTable, count: i64) -> usize {
    if count < 0 {
        return backward_word(buf, table, -count);
    }

    let text = buf.buffer_string();
    let chars: Vec<char> = text.chars().collect();
    // Convert byte pos to char index within accessible region.
    let base = buf.point_min();
    let rel_byte = buf.point().saturating_sub(base);
    let mut idx = buf.text.byte_to_char(base + rel_byte) - buf.text.byte_to_char(base);

    let accessible_char_start = buf.text.byte_to_char(base);
    let accessible_char_end = buf.text.byte_to_char(buf.point_max());
    let accessible_len = accessible_char_end - accessible_char_start;

    for _ in 0..count {
        // Skip non-word characters
        while idx < accessible_len && !matches!(table.char_syntax(chars[idx]), SyntaxClass::Word) {
            idx += 1;
        }
        // Skip word characters
        while idx < accessible_len && matches!(table.char_syntax(chars[idx]), SyntaxClass::Word) {
            idx += 1;
        }
    }

    // Convert char index back to byte position (absolute).
    let abs_char = accessible_char_start + idx;
    buf.text.char_to_byte(abs_char)
}

/// Move backward over `count` words.  Returns the resulting byte position.
pub fn backward_word(buf: &Buffer, table: &SyntaxTable, count: i64) -> usize {
    if count < 0 {
        return forward_word(buf, table, -count);
    }

    let text = buf.buffer_string();
    let chars: Vec<char> = text.chars().collect();
    let base = buf.point_min();
    let rel_byte = buf.point().saturating_sub(base);
    let mut idx = buf.text.byte_to_char(base + rel_byte) - buf.text.byte_to_char(base);

    for _ in 0..count {
        // Skip non-word characters backward
        while idx > 0 && !matches!(table.char_syntax(chars[idx - 1]), SyntaxClass::Word) {
            idx -= 1;
        }
        // Skip word characters backward
        while idx > 0 && matches!(table.char_syntax(chars[idx - 1]), SyntaxClass::Word) {
            idx -= 1;
        }
    }

    let accessible_char_start = buf.text.byte_to_char(base);
    let abs_char = accessible_char_start + idx;
    buf.text.char_to_byte(abs_char)
}

/// Skip forward over characters whose syntax class matches any character in
/// `syntax_chars` (each character in the string names a syntax class,
/// e.g., `"w_"` matches Word and Symbol).  Returns the resulting byte position.
pub fn skip_syntax_forward(
    buf: &Buffer,
    table: &SyntaxTable,
    syntax_chars: &str,
    limit: Option<usize>,
) -> usize {
    let classes: Vec<SyntaxClass> = syntax_chars
        .chars()
        .filter_map(SyntaxClass::from_char)
        .collect();

    let text = buf.buffer_string();
    let chars: Vec<char> = text.chars().collect();
    let base = buf.point_min();
    let rel_byte = buf.point().saturating_sub(base);
    let mut idx = buf.text.byte_to_char(base + rel_byte) - buf.text.byte_to_char(base);

    let accessible_char_start = buf.text.byte_to_char(base);
    let accessible_char_end = buf.text.byte_to_char(buf.point_max());
    let accessible_len = accessible_char_end - accessible_char_start;

    let char_limit = limit
        .map(|lim| {
            let lim_clamped = lim.min(buf.point_max());
            buf.text.byte_to_char(lim_clamped) - accessible_char_start
        })
        .unwrap_or(accessible_len);

    while idx < char_limit {
        let syn = table.char_syntax(chars[idx]);
        if !classes.contains(&syn) {
            break;
        }
        idx += 1;
    }

    let abs_char = accessible_char_start + idx;
    buf.text.char_to_byte(abs_char)
}

/// Skip backward over characters whose syntax class matches any character in
/// `syntax_chars`.  Returns the resulting byte position.
pub fn skip_syntax_backward(
    buf: &Buffer,
    table: &SyntaxTable,
    syntax_chars: &str,
    limit: Option<usize>,
) -> usize {
    let classes: Vec<SyntaxClass> = syntax_chars
        .chars()
        .filter_map(SyntaxClass::from_char)
        .collect();

    let text = buf.buffer_string();
    let chars: Vec<char> = text.chars().collect();
    let base = buf.point_min();
    let rel_byte = buf.point().saturating_sub(base);
    let mut idx = buf.text.byte_to_char(base + rel_byte) - buf.text.byte_to_char(base);

    let accessible_char_start = buf.text.byte_to_char(base);

    let char_limit = limit
        .map(|lim| {
            let lim_clamped = lim.max(base);
            buf.text.byte_to_char(lim_clamped) - accessible_char_start
        })
        .unwrap_or(0);

    while idx > char_limit {
        let syn = table.char_syntax(chars[idx - 1]);
        if !classes.contains(&syn) {
            break;
        }
        idx -= 1;
    }

    let abs_char = accessible_char_start + idx;
    buf.text.char_to_byte(abs_char)
}

/// Scan for balanced expressions (sexps).
///
/// Starting from byte position `from`, scan `count` sexps forward (positive
/// count) or backward (negative count).  Returns the byte position after the
/// last sexp, or an error if unbalanced.
pub fn scan_sexps(
    buf: &Buffer,
    table: &SyntaxTable,
    from: usize,
    count: i64,
) -> Result<usize, String> {
    if count == 0 {
        return Ok(from);
    }

    let text = buf.text.to_string();
    let chars: Vec<char> = text.chars().collect();
    let total_chars = chars.len();

    // Convert byte position to char index.
    let mut idx = buf.text.byte_to_char(from);

    if count > 0 {
        for _ in 0..count {
            idx = scan_sexp_forward(&chars, total_chars, idx, table)?;
        }
    } else {
        for _ in 0..(-count) {
            idx = scan_sexp_backward(&chars, idx, table)?;
        }
    }

    Ok(buf.text.char_to_byte(idx))
}

/// Scan one sexp forward from char index `start`.
fn scan_sexp_forward(
    chars: &[char],
    len: usize,
    start: usize,
    table: &SyntaxTable,
) -> Result<usize, String> {
    let mut idx = start;

    // Skip whitespace and comments
    while idx < len
        && matches!(
            table.char_syntax(chars[idx]),
            SyntaxClass::Whitespace | SyntaxClass::Comment | SyntaxClass::EndComment
        )
    {
        idx += 1;
    }

    if idx >= len {
        return Err("Scan error: unbalanced parentheses".to_string());
    }

    let ch = chars[idx];
    let syn = table.char_syntax(ch);

    match syn {
        SyntaxClass::Open => {
            // Find matching close, respecting nesting.
            let open_char = ch;
            let close_char = table
                .get_entry(open_char)
                .and_then(|e| e.matching_char)
                .unwrap_or(')');
            let mut depth = 1i32;
            idx += 1;
            while idx < len && depth > 0 {
                let c = chars[idx];
                let s = table.char_syntax(c);
                match s {
                    SyntaxClass::Open => {
                        depth += 1;
                    }
                    SyntaxClass::Close => {
                        depth -= 1;
                    }
                    SyntaxClass::StringDelim | SyntaxClass::StringFence => {
                        // Skip over string contents
                        let delim_class = s;
                        idx += 1;
                        while idx < len {
                            let sc = table.char_syntax(chars[idx]);
                            if sc == delim_class
                                && (s == SyntaxClass::StringFence || chars[idx] == c)
                            {
                                break;
                            }
                            if matches!(sc, SyntaxClass::Escape) {
                                idx += 1; // skip escaped char
                            }
                            idx += 1;
                        }
                        // idx now points at closing delim (or past end)
                    }
                    SyntaxClass::Escape => {
                        idx += 1; // skip next char
                    }
                    _ => {}
                }
                idx += 1;
            }
            if depth != 0 {
                return Err(format!(
                    "Scan error: unbalanced parentheses (looking for '{}')",
                    close_char
                ));
            }
            Ok(idx)
        }
        SyntaxClass::Close => {
            Err("Scan error: unbalanced parentheses (unexpected close)".to_string())
        }
        SyntaxClass::StringDelim | SyntaxClass::StringFence => {
            // Scan to matching string delimiter.
            // StringFence always pairs with itself (like `"` but independent).
            let delim_class = syn;
            idx += 1;
            while idx < len {
                let c = chars[idx];
                let s = table.char_syntax(c);
                if s == delim_class && (syn == SyntaxClass::StringFence || c == ch) {
                    break;
                }
                if matches!(s, SyntaxClass::Escape) {
                    idx += 1; // skip escaped char
                }
                idx += 1;
            }
            if idx >= len {
                return Err("Scan error: unterminated string".to_string());
            }
            Ok(idx + 1) // past closing delim
        }
        SyntaxClass::Word | SyntaxClass::Symbol => {
            // Scan over a symbol/word sexp.
            while idx < len
                && matches!(
                    table.char_syntax(chars[idx]),
                    SyntaxClass::Word | SyntaxClass::Symbol
                )
            {
                idx += 1;
            }
            Ok(idx)
        }
        SyntaxClass::Escape | SyntaxClass::CharQuote => {
            // Escape + next char form one sexp.
            idx += 1;
            if idx < len {
                idx += 1;
            }
            Ok(idx)
        }
        SyntaxClass::MathDelim => {
            // Scan to matching math delimiter.
            let delim = ch;
            idx += 1;
            while idx < len && chars[idx] != delim {
                idx += 1;
            }
            if idx >= len {
                return Err("Scan error: unterminated math delimiter".to_string());
            }
            Ok(idx + 1)
        }
        _ => {
            // Single punctuation or other character is its own sexp.
            Ok(idx + 1)
        }
    }
}

/// Scan one sexp backward from char index `start`.
fn scan_sexp_backward(chars: &[char], start: usize, table: &SyntaxTable) -> Result<usize, String> {
    let mut idx = start;

    // Skip whitespace and comments backward
    while idx > 0
        && matches!(
            table.char_syntax(chars[idx - 1]),
            SyntaxClass::Whitespace | SyntaxClass::Comment | SyntaxClass::EndComment
        )
    {
        idx -= 1;
    }

    if idx == 0 {
        return Err("Scan error: beginning of buffer".to_string());
    }

    idx -= 1; // move to the character we're examining
    let ch = chars[idx];
    let syn = table.char_syntax(ch);

    match syn {
        SyntaxClass::Close => {
            // Find matching open, respecting nesting.
            let close_char = ch;
            let open_char = table
                .get_entry(close_char)
                .and_then(|e| e.matching_char)
                .unwrap_or('(');
            let mut depth = 1i32;
            while idx > 0 && depth > 0 {
                idx -= 1;
                let c = chars[idx];
                let s = table.char_syntax(c);
                match s {
                    SyntaxClass::Close => {
                        depth += 1;
                    }
                    SyntaxClass::Open => {
                        depth -= 1;
                    }
                    SyntaxClass::StringDelim | SyntaxClass::StringFence => {
                        // Skip over string contents backward
                        let delim_class = s;
                        if idx > 0 {
                            idx -= 1;
                            while idx > 0 {
                                let sc = table.char_syntax(chars[idx]);
                                if sc == delim_class
                                    && (s == SyntaxClass::StringFence || chars[idx] == c)
                                {
                                    break;
                                }
                                idx -= 1;
                            }
                            // idx now points at the opening delim
                        }
                    }
                    _ => {}
                }
            }
            if depth != 0 {
                return Err(format!(
                    "Scan error: unbalanced parentheses (looking for '{}')",
                    open_char
                ));
            }
            Ok(idx)
        }
        SyntaxClass::Open => {
            Err("Scan error: unbalanced parentheses (unexpected open)".to_string())
        }
        SyntaxClass::StringDelim | SyntaxClass::StringFence => {
            // Scan backward to matching string delimiter.
            let delim_class = syn;
            if idx == 0 {
                return Err("Scan error: unterminated string".to_string());
            }
            idx -= 1;
            while idx > 0 {
                let c = chars[idx];
                let s = table.char_syntax(c);
                if s == delim_class && (syn == SyntaxClass::StringFence || c == ch) {
                    break;
                }
                idx -= 1;
            }
            let c = chars[idx];
            let s = table.char_syntax(c);
            if !(s == delim_class && (syn == SyntaxClass::StringFence || c == ch)) {
                return Err("Scan error: unterminated string".to_string());
            }
            Ok(idx)
        }
        SyntaxClass::Word | SyntaxClass::Symbol => {
            // Scan backward over word/symbol chars.
            while idx > 0
                && matches!(
                    table.char_syntax(chars[idx - 1]),
                    SyntaxClass::Word | SyntaxClass::Symbol
                )
            {
                idx -= 1;
            }
            Ok(idx)
        }
        SyntaxClass::Escape | SyntaxClass::CharQuote => {
            // The escape char itself is a sexp.
            Ok(idx)
        }
        SyntaxClass::MathDelim => {
            let delim = ch;
            if idx == 0 {
                return Err("Scan error: unterminated math delimiter".to_string());
            }
            idx -= 1;
            while idx > 0 && chars[idx] != delim {
                idx -= 1;
            }
            if chars[idx] != delim {
                return Err("Scan error: unterminated math delimiter".to_string());
            }
            Ok(idx)
        }
        _ => {
            // Single char sexp.
            Ok(idx)
        }
    }
}

// ===========================================================================
// Builtin functions (pure — no evaluator needed)
// ===========================================================================

/// `(string-to-syntax S)` — parse a syntax descriptor string.
pub(crate) fn builtin_string_to_syntax(args: Vec<Value>) -> EvalResult {
    if args.len() != 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("string-to-syntax"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let s = match &args[0] {
        Value::Str(_) => args[0].as_str().unwrap().to_string(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };
    let entry = string_to_syntax(&s).map_err(|msg| signal("error", vec![Value::string(&msg)]))?;
    if matches!(entry.class, SyntaxClass::InheritStandard) {
        return Ok(Value::Nil);
    }
    Ok(syntax_entry_to_value(&entry))
}

/// `(make-syntax-table &optional PARENT)` — create a new syntax table.
pub(crate) fn builtin_make_syntax_table(args: Vec<Value>) -> EvalResult {
    if args.len() > 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("make-syntax-table"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let table = super::chartable::make_char_table_value(Value::symbol("syntax-table"), Value::Nil);
    let parent = if args.is_empty() || args[0].is_nil() {
        ensure_standard_syntax_table_object()?
    } else {
        args[0]
    };
    if !parent.is_nil() {
        super::chartable::builtin_set_char_table_parent(vec![table, parent])?;
    }
    Ok(table)
}

/// `(copy-syntax-table &optional TABLE)` — return a fresh copy of TABLE.
pub(crate) fn builtin_copy_syntax_table(args: Vec<Value>) -> EvalResult {
    if args.len() > 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("copy-syntax-table"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let source = if args.is_empty() || args[0].is_nil() {
        builtin_standard_syntax_table(vec![])?
    } else {
        let table = args[0];
        if builtin_syntax_table_p(vec![table])?.is_nil() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("syntax-table-p"), table],
            ));
        }
        table
    };

    match source {
        Value::Vector(v) => {
            let copy = Value::vector(with_heap(|h| h.get_vector(v).clone()));
            super::chartable::builtin_set_char_table_range(vec![copy, Value::Nil, Value::Nil])?;
            if super::chartable::builtin_char_table_parent(vec![copy])?.is_nil() {
                super::chartable::builtin_set_char_table_parent(vec![
                    copy,
                    ensure_standard_syntax_table_object()?,
                ])?;
            }
            Ok(copy)
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("syntax-table-p"), other],
        )),
    }
}

fn ensure_standard_syntax_table_object() -> EvalResult {
    STANDARD_SYNTAX_TABLE_OBJECT.with(|slot| {
        if let Some(table) = slot.borrow().as_ref() {
            return Ok(*table);
        }
        let whitespace = syntax_entry_to_value(&SyntaxEntry::simple(SyntaxClass::Whitespace));
        let punctuation = syntax_entry_to_value(&SyntaxEntry::simple(SyntaxClass::Punctuation));
        let word = syntax_entry_to_value(&SyntaxEntry::simple(SyntaxClass::Word));
        let table =
            super::chartable::make_char_table_value(Value::symbol("syntax-table"), whitespace);

        for cp in 0..=(' ' as i64 - 1) {
            super::chartable::builtin_set_char_table_range(vec![
                table,
                Value::Int(cp),
                punctuation,
            ])?;
        }
        super::chartable::builtin_set_char_table_range(vec![table, Value::Int(0x7f), punctuation])?;

        let standard = SyntaxTable::new_standard();
        for (ch, entry) in &standard.entries {
            super::chartable::builtin_set_char_table_range(vec![
                table,
                Value::Int(*ch as i64),
                syntax_entry_to_value(entry),
            ])?;
        }
        super::chartable::builtin_set_char_table_range(vec![
            table,
            Value::cons(Value::Int(0x80), Value::Int(0x3F_FFFF)),
            word,
        ])?;
        *slot.borrow_mut() = Some(table);
        Ok(table)
    })
}

fn current_buffer_syntax_table_object_in_buffers(
    buffers: &mut BufferManager,
) -> Result<Value, Flow> {
    let fallback = ensure_standard_syntax_table_object()?;
    let buf = buffers
        .current_buffer_mut()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    if let Some(value) = buf.properties.get(SYNTAX_TABLE_OBJECT_PROPERTY) {
        if builtin_syntax_table_p(vec![*value])?.is_truthy() {
            return Ok(*value);
        }
    }

    buf.properties
        .insert(SYNTAX_TABLE_OBJECT_PROPERTY.to_string(), fallback);
    Ok(fallback)
}

fn current_buffer_syntax_table_object(eval: &mut super::eval::Evaluator) -> Result<Value, Flow> {
    current_buffer_syntax_table_object_in_buffers(&mut eval.buffers)
}

fn set_current_buffer_syntax_table_object_in_buffers(
    buffers: &mut BufferManager,
    table: Value,
) -> Result<(), Flow> {
    let buf = buffers
        .current_buffer_mut()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    buf.properties
        .insert(SYNTAX_TABLE_OBJECT_PROPERTY.to_string(), table);
    Ok(())
}

fn set_current_buffer_syntax_table_object(
    eval: &mut super::eval::Evaluator,
    table: Value,
) -> Result<(), Flow> {
    set_current_buffer_syntax_table_object_in_buffers(&mut eval.buffers, table)
}

fn syntax_entry_from_chartable_entry(entry: &Value) -> Option<SyntaxEntry> {
    match entry {
        Value::Nil => None,
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            let code = match pair.car {
                Value::Int(code) => code,
                _ => return None,
            };
            let class = SyntaxClass::from_code(code)?;
            let matching_char = match pair.cdr {
                Value::Int(n) => char::from_u32(n as u32),
                Value::Char(c) => Some(c),
                Value::Nil => None,
                _ => None,
            };
            Some(SyntaxEntry {
                class,
                matching_char,
                flags: SyntaxFlags::new(((code >> 16) & 0xFF) as u8),
            })
        }
        Value::Int(code) => Some(SyntaxEntry {
            class: SyntaxClass::from_code(*code)?,
            matching_char: None,
            flags: SyntaxFlags::new(((*code >> 16) & 0xFF) as u8),
        }),
        _ => None,
    }
}

fn apply_compiled_syntax_entry(
    syntax_table: &mut SyntaxTable,
    key: Value,
    entry: Option<&SyntaxEntry>,
) -> Result<(), Flow> {
    match key {
        Value::Int(n) => {
            if let Some(ch) = char::from_u32(n as u32) {
                if let Some(entry) = entry {
                    syntax_table.modify_syntax_entry(ch, entry.clone());
                } else {
                    syntax_table.entries.remove(&ch);
                }
            }
        }
        Value::Char(ch) => {
            if let Some(entry) = entry {
                syntax_table.modify_syntax_entry(ch, entry.clone());
            } else {
                syntax_table.entries.remove(&ch);
            }
        }
        Value::Cons(cell) => {
            let pair = read_cons(cell);
            let (start, end) = match (pair.car, pair.cdr) {
                (Value::Int(start), Value::Int(end)) => (start, end),
                _ => return Ok(()),
            };
            if start > end {
                return Ok(());
            }
            if start >= 0x80
                && end == 0x3F_FFFF
                && matches!(entry, Some(e) if e.class == SyntaxClass::Word && e.matching_char.is_none())
            {
                return Ok(());
            }
            for cp in start..=end {
                if let Some(ch) = char::from_u32(cp as u32) {
                    if let Some(entry) = entry {
                        syntax_table.modify_syntax_entry(ch, entry.clone());
                    } else {
                        syntax_table.entries.remove(&ch);
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn syntax_table_from_chartable(table: Value) -> Result<SyntaxTable, Flow> {
    if builtin_syntax_table_p(vec![table])?.is_nil() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("syntax-table-p"), table],
        ));
    }

    let parent = super::chartable::builtin_char_table_parent(vec![table])?;
    let mut compiled = SyntaxTable {
        entries: HashMap::new(),
        parent: if parent.is_nil() {
            None
        } else {
            Some(Box::new(syntax_table_from_chartable(parent)?))
        },
    };

    for (key, value) in super::chartable::char_table_local_entries(&table)? {
        let entry = syntax_entry_from_chartable_entry(&value);
        apply_compiled_syntax_entry(&mut compiled, key, entry.as_ref())?;
    }

    Ok(compiled)
}

/// `(syntax-class-to-char CLASS)` — map syntax class code to descriptor char.
pub(crate) fn builtin_syntax_class_to_char(args: Vec<Value>) -> EvalResult {
    if args.len() != 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("syntax-class-to-char"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let class = match &args[0] {
        Value::Int(n) => *n,
        Value::Char(c) => *c as i64,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("fixnump"), *other],
            ));
        }
    };

    let ch = match class {
        0 => ' ',
        1 => '.',
        2 => 'w',
        3 => '_',
        4 => '(',
        5 => ')',
        6 => '\'',
        7 => '"',
        8 => '$',
        9 => '\\',
        10 => '/',
        11 => '<',
        12 => '>',
        13 => '@',
        14 => '!',
        15 => '|',
        n => {
            return Err(signal(
                "args-out-of-range",
                vec![Value::Int(15), Value::Int(n)],
            ));
        }
    };

    Ok(Value::Char(ch))
}

/// `(matching-paren CHAR)` — return matching paren for bracket chars.
///
/// This is an evaluator-dependent version that uses the current buffer's
/// syntax table. For backwards compatibility, also works as a pure function
/// with standard bracket pairs.
pub(crate) fn builtin_matching_paren_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() != 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("matching-paren"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let ch = match &args[0] {
        Value::Int(n) => char::from_u32(*n as u32).ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), args[0]],
            )
        })?,
        Value::Char(c) => *c,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), *other],
            ));
        }
    };

    // Look up in the current buffer's syntax table
    if let Some(buf) = eval.buffers.current_buffer() {
        let entry = buf.syntax_table.get_entry(ch);
        if let Some(e) = entry {
            if matches!(e.class, SyntaxClass::Open | SyntaxClass::Close) {
                if let Some(m) = e.matching_char {
                    return Ok(Value::Char(m));
                }
            }
        }
    }

    // Fallback to standard hardcoded pairs
    let out = match ch {
        '(' => Some(')'),
        ')' => Some('('),
        '[' => Some(']'),
        ']' => Some('['),
        '{' => Some('}'),
        '}' => Some('{'),
        _ => None,
    };
    Ok(out.map_or(Value::Nil, Value::Char))
}

/// Pure (no-eval) version of `matching-paren` using standard hardcoded pairs.
/// Kept for unit tests; dispatch uses `builtin_matching_paren_eval` instead.
#[allow(dead_code)]
pub(crate) fn builtin_matching_paren(args: Vec<Value>) -> EvalResult {
    if args.len() != 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("matching-paren"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let ch = match &args[0] {
        Value::Int(n) => char::from_u32(*n as u32).ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), args[0]],
            )
        })?,
        Value::Char(c) => *c,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), *other],
            ));
        }
    };

    let out = match ch {
        '(' => Some(')'),
        ')' => Some('('),
        '[' => Some(']'),
        ']' => Some('['),
        '{' => Some('}'),
        '}' => Some('{'),
        _ => None,
    };
    Ok(out.map_or(Value::Nil, Value::Char))
}

/// `(standard-syntax-table)` — return the standard syntax table.
pub(crate) fn builtin_standard_syntax_table(args: Vec<Value>) -> EvalResult {
    if !args.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("standard-syntax-table"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    ensure_standard_syntax_table_object()
}

/// `(syntax-table-p OBJECT)` — return t if OBJECT is a syntax table.
pub(crate) fn builtin_syntax_table_p(args: Vec<Value>) -> EvalResult {
    if args.len() != 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("syntax-table-p"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let is_char_table = super::chartable::builtin_char_table_p(vec![args[0]])?;
    if !is_char_table.is_truthy() {
        return Ok(Value::Nil);
    }

    let subtype = super::chartable::builtin_char_table_subtype(vec![args[0]])?;
    match subtype {
        Value::Symbol(id) if resolve_sym(id) == "syntax-table" => Ok(Value::True),
        _ => Ok(Value::Nil),
    }
}

/// `(syntax-table)` — return the current buffer syntax table.
///
/// Returns the buffer-local syntax-table object, defaulting to the standard
/// syntax-table object.
pub(crate) fn builtin_syntax_table(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if !args.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("syntax-table"), Value::Int(args.len() as i64)],
        ));
    }
    current_buffer_syntax_table_object(eval)
}

/// `(set-syntax-table TABLE)` — install TABLE for current buffer and return it.
///
/// NeoVM currently stores syntax behavior on `Buffer.syntax_table` internals;
/// this installs the exposed syntax-table object for compatibility and returns it.
pub(crate) fn builtin_set_syntax_table(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() != 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("set-syntax-table"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    if builtin_syntax_table_p(vec![args[0]])?.is_nil() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("syntax-table-p"), args[0]],
        ));
    }
    let table = args[0];
    set_current_buffer_syntax_table_object(eval, table)?;
    let compiled = syntax_table_from_chartable(table)?;
    let buf = eval
        .buffers
        .current_buffer_mut()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    buf.syntax_table = compiled;
    Ok(table)
}

// ===========================================================================
// Builtin functions (evaluator-dependent — operate on current buffer)
// ===========================================================================

/// `(modify-syntax-entry CHAR NEWENTRY &optional SYNTAX-TABLE)`
pub(crate) fn builtin_modify_syntax_entry(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    modify_syntax_entry_in_buffers(&mut eval.buffers, &args)
}

pub(crate) fn modify_syntax_entry_in_buffers(
    buffers: &mut BufferManager,
    args: &[Value],
) -> EvalResult {
    if args.len() < 2 || args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("modify-syntax-entry"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let descriptor = match &args[1] {
        Value::Str(_) => args[1].as_str().unwrap().to_string(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };
    let entry =
        string_to_syntax(&descriptor).map_err(|msg| signal("error", vec![Value::string(&msg)]))?;
    let target_table = if let Some(table) = args.get(2) {
        if builtin_syntax_table_p(vec![*table])?.is_nil() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("syntax-table-p"), *table],
            ));
        }
        *table
    } else {
        current_buffer_syntax_table_object_in_buffers(buffers)?
    };
    let current_table = current_buffer_syntax_table_object_in_buffers(buffers)?;
    let update_current_buffer_table = target_table == current_table;

    // Update the exposed syntax-table object.
    let chartable_entry = if matches!(entry.class, SyntaxClass::InheritStandard) {
        Value::Nil
    } else {
        syntax_entry_to_value(&entry)
    };
    super::chartable::builtin_set_char_table_range(vec![target_table, args[0], chartable_entry])?;

    if !update_current_buffer_table {
        return Ok(Value::Nil);
    }
    let compiled = syntax_table_from_chartable(target_table)?;
    let buf = buffers
        .current_buffer_mut()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    buf.syntax_table = compiled;
    Ok(Value::Nil)
}

/// `(char-syntax CHAR)` — return the syntax class designator char.
pub(crate) fn builtin_char_syntax(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() != 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("char-syntax"), Value::Int(args.len() as i64)],
        ));
    }
    let ch = match &args[0] {
        Value::Char(c) => *c,
        Value::Int(n) => char::from_u32(*n as u32).ok_or_else(|| {
            signal(
                "error",
                vec![Value::string(format!("Invalid character code: {}", n))],
            )
        })?,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), *other],
            ));
        }
    };

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let class = buf.syntax_table.char_syntax(ch);
    Ok(Value::Char(class.to_char()))
}

/// `(syntax-after POS)` — return syntax descriptor for char at POS.
pub(crate) fn builtin_syntax_after(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() != 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("syntax-after"), Value::Int(args.len() as i64)],
        ));
    }

    let pos = match &args[0] {
        Value::Int(n) => *n,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("number-or-marker-p"), *other],
            ));
        }
    };
    if pos <= 0 {
        return Ok(Value::Nil);
    }

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    let char_index = pos as usize - 1;
    let byte_index = buf.text.char_to_byte(char_index.min(buf.text.char_count()));
    let Some(ch) = buf.char_after(byte_index) else {
        return Ok(Value::Nil);
    };

    let entry = buf
        .syntax_table
        .get_entry(ch)
        .cloned()
        .unwrap_or_else(|| SyntaxEntry::simple(buf.syntax_table.char_syntax(ch)));
    Ok(syntax_entry_to_value(&entry))
}

/// `(forward-comment COUNT)` — move point over COUNT comment/whitespace
/// constructs. Returns `t` if all COUNT were successfully skipped, `nil`
/// if scanning stopped early (hit non-comment/non-whitespace or buffer
/// boundary).
pub(crate) fn builtin_forward_comment(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() != 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("forward-comment"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let count = match &args[0] {
        Value::Int(n) => *n,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("integerp"), *other],
            ));
        }
    };

    let buf = eval
        .buffers
        .current_buffer_mut()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    if count == 0 {
        return Ok(Value::True);
    }

    if count > 0 {
        let ok = forward_comment_forward(buf, count as u64);
        return Ok(if ok { Value::True } else { Value::Nil });
    } else {
        let ok = forward_comment_backward(buf, (-count) as u64);
        return Ok(if ok { Value::True } else { Value::Nil });
    }
}

/// Skip whitespace and comments forward. Returns true if all `count`
/// comments were skipped successfully.
fn forward_comment_forward(buf: &mut Buffer, count: u64) -> bool {
    let mut remaining = count;

    while remaining > 0 {
        // Phase 1: skip whitespace (and stray EndComment newlines).
        loop {
            let pt = buf.point();
            let max = buf.point_max();
            if pt >= max {
                return false;
            }
            let Some(ch) = buf.char_after(pt) else {
                return false;
            };
            let entry = buf.syntax_table.get_entry(ch);
            let class = entry.map(|e| e.class).unwrap_or(SyntaxClass::Symbol);
            let flags = entry.map(|e| e.flags).unwrap_or(SyntaxFlags::empty());

            if class == SyntaxClass::Whitespace {
                buf.goto_char(pt + ch.len_utf8());
                continue;
            }
            // In GNU Emacs, EndComment newline is treated as whitespace
            // for forward scanning.
            if class == SyntaxClass::EndComment && ch == '\n' {
                buf.goto_char(pt + ch.len_utf8());
                continue;
            }
            break;
        }

        // Phase 2: detect comment start.
        let pt = buf.point();
        let max = buf.point_max();
        if pt >= max {
            return false;
        }
        let Some(ch) = buf.char_after(pt) else {
            return false;
        };
        let entry = buf.syntax_table.get_entry(ch);
        let class = entry.map(|e| e.class).unwrap_or(SyntaxClass::Symbol);
        let flags = entry.map(|e| e.flags).unwrap_or(SyntaxFlags::empty());

        // Single-char comment start (class `<`).
        if class == SyntaxClass::Comment {
            let style_b = flags.contains(SyntaxFlags::COMMENT_STYLE_B);
            let nested = flags.contains(SyntaxFlags::COMMENT_NESTABLE);
            buf.goto_char(pt + ch.len_utf8());
            if !scan_forward_comment_body(buf, style_b, nested) {
                return false;
            }
            remaining -= 1;
            continue;
        }

        // Comment fence (class `!` = Generic).
        if class == SyntaxClass::Generic {
            buf.goto_char(pt + ch.len_utf8());
            // Scan forward for matching comment fence.
            if !scan_forward_comment_fence(buf) {
                return false;
            }
            remaining -= 1;
            continue;
        }

        // Two-char comment start: check COMMENT_START_FIRST on current
        // char and COMMENT_START_SECOND on next char.
        if flags.contains(SyntaxFlags::COMMENT_START_FIRST) {
            let next_pos = pt + ch.len_utf8();
            if next_pos < max {
                if let Some(ch2) = buf.char_after(next_pos) {
                    let entry2 = buf.syntax_table.get_entry(ch2);
                    let flags2 = entry2.map(|e| e.flags).unwrap_or(SyntaxFlags::empty());
                    if flags2.contains(SyntaxFlags::COMMENT_START_SECOND) {
                        let style_b = flags2.contains(SyntaxFlags::COMMENT_STYLE_B);
                        let nested = flags2.contains(SyntaxFlags::COMMENT_NESTABLE)
                            || flags.contains(SyntaxFlags::COMMENT_NESTABLE);
                        buf.goto_char(next_pos + ch2.len_utf8());
                        if !scan_forward_comment_body(buf, style_b, nested) {
                            return false;
                        }
                        remaining -= 1;
                        continue;
                    }
                }
            }
        }

        // Not whitespace or comment — stop.
        return false;
    }

    true
}

/// Scan forward through comment body until matching comment end.
/// Point should be positioned right after the comment start.
/// Returns true if comment end was found.
fn scan_forward_comment_body(buf: &mut Buffer, style_b: bool, nested: bool) -> bool {
    let mut nesting = 1i32;

    loop {
        let pt = buf.point();
        let max = buf.point_max();
        if pt >= max {
            return false;
        }
        let Some(ch) = buf.char_after(pt) else {
            return false;
        };
        let entry = buf.syntax_table.get_entry(ch);
        let class = entry.map(|e| e.class).unwrap_or(SyntaxClass::Symbol);
        let flags = entry.map(|e| e.flags).unwrap_or(SyntaxFlags::empty());

        // Handle escape / charquote.
        if class == SyntaxClass::Escape || class == SyntaxClass::CharQuote {
            buf.goto_char(pt + ch.len_utf8());
            // Skip the next char too.
            let pt2 = buf.point();
            if pt2 >= buf.point_max() {
                return false;
            }
            if let Some(ch2) = buf.char_after(pt2) {
                buf.goto_char(pt2 + ch2.len_utf8());
            }
            continue;
        }

        // Nested comment start (only if nested flag is set).
        if nested {
            if class == SyntaxClass::Comment {
                let sf_b = flags.contains(SyntaxFlags::COMMENT_STYLE_B);
                if sf_b == style_b {
                    nesting += 1;
                    buf.goto_char(pt + ch.len_utf8());
                    continue;
                }
            }

            if flags.contains(SyntaxFlags::COMMENT_START_FIRST) {
                let next_pos = pt + ch.len_utf8();
                if next_pos < buf.point_max() {
                    if let Some(ch2) = buf.char_after(next_pos) {
                        let entry2 = buf.syntax_table.get_entry(ch2);
                        let flags2 = entry2.map(|e| e.flags).unwrap_or(SyntaxFlags::empty());
                        if flags2.contains(SyntaxFlags::COMMENT_START_SECOND) {
                            let sf_b = flags2.contains(SyntaxFlags::COMMENT_STYLE_B);
                            if sf_b == style_b {
                                nesting += 1;
                                buf.goto_char(next_pos + ch2.len_utf8());
                                continue;
                            }
                        }
                    }
                }
            }
        }

        // Single-char comment end (class `>`).
        if class == SyntaxClass::EndComment {
            let se_b = flags.contains(SyntaxFlags::COMMENT_STYLE_B);
            if se_b == style_b {
                buf.goto_char(pt + ch.len_utf8());
                nesting -= 1;
                if nesting <= 0 {
                    return true;
                }
                continue;
            }
        }

        // Comment fence end.
        if class == SyntaxClass::Generic {
            buf.goto_char(pt + ch.len_utf8());
            nesting -= 1;
            if nesting <= 0 {
                return true;
            }
            continue;
        }

        // Two-char comment end.
        if flags.contains(SyntaxFlags::COMMENT_END_FIRST) {
            let next_pos = pt + ch.len_utf8();
            if next_pos < buf.point_max() {
                if let Some(ch2) = buf.char_after(next_pos) {
                    let entry2 = buf.syntax_table.get_entry(ch2);
                    let flags2 = entry2.map(|e| e.flags).unwrap_or(SyntaxFlags::empty());
                    if flags2.contains(SyntaxFlags::COMMENT_END_SECOND) {
                        let se_b = flags2.contains(SyntaxFlags::COMMENT_STYLE_B);
                        if se_b == style_b {
                            buf.goto_char(next_pos + ch2.len_utf8());
                            nesting -= 1;
                            if nesting <= 0 {
                                return true;
                            }
                            continue;
                        }
                    }
                }
            }
        }

        buf.goto_char(pt + ch.len_utf8());
    }
}

/// Scan forward for matching comment fence character.
fn scan_forward_comment_fence(buf: &mut Buffer) -> bool {
    loop {
        let pt = buf.point();
        if pt >= buf.point_max() {
            return false;
        }
        let Some(ch) = buf.char_after(pt) else {
            return false;
        };
        let entry = buf.syntax_table.get_entry(ch);
        let class = entry.map(|e| e.class).unwrap_or(SyntaxClass::Symbol);

        if class == SyntaxClass::Escape || class == SyntaxClass::CharQuote {
            buf.goto_char(pt + ch.len_utf8());
            let pt2 = buf.point();
            if pt2 >= buf.point_max() {
                return false;
            }
            if let Some(ch2) = buf.char_after(pt2) {
                buf.goto_char(pt2 + ch2.len_utf8());
            }
            continue;
        }

        buf.goto_char(pt + ch.len_utf8());

        if class == SyntaxClass::Generic {
            return true;
        }
    }
}

/// Skip whitespace and comments backward. Returns true if all `count`
/// comments were skipped successfully.
fn forward_comment_backward(buf: &mut Buffer, count: u64) -> bool {
    let mut remaining = count;

    while remaining > 0 {
        // Phase 1: skip whitespace backward.
        loop {
            let pt = buf.point();
            let min = buf.point_min();
            if pt <= min {
                return false;
            }
            let Some(ch) = buf.char_before(pt) else {
                return false;
            };
            let entry = buf.syntax_table.get_entry(ch);
            let class = entry.map(|e| e.class).unwrap_or(SyntaxClass::Symbol);

            if class == SyntaxClass::Whitespace {
                buf.goto_char(pt - ch.len_utf8());
                continue;
            }
            // EndComment newline treated as whitespace backward too.
            if class == SyntaxClass::EndComment && ch == '\n' {
                buf.goto_char(pt - ch.len_utf8());
                continue;
            }
            break;
        }

        // Phase 2: detect comment end backward.
        let pt = buf.point();
        let min = buf.point_min();
        if pt <= min {
            return false;
        }
        let Some(ch) = buf.char_before(pt) else {
            return false;
        };
        let entry = buf.syntax_table.get_entry(ch);
        let class = entry.map(|e| e.class).unwrap_or(SyntaxClass::Symbol);
        let flags = entry.map(|e| e.flags).unwrap_or(SyntaxFlags::empty());

        // Single-char comment end (class `>`).
        if class == SyntaxClass::EndComment {
            let style_b = flags.contains(SyntaxFlags::COMMENT_STYLE_B);
            let nested = flags.contains(SyntaxFlags::COMMENT_NESTABLE);
            buf.goto_char(pt - ch.len_utf8());
            if !scan_backward_comment_body(buf, style_b, nested) {
                return false;
            }
            remaining -= 1;
            continue;
        }

        // Comment fence backward.
        if class == SyntaxClass::Generic {
            buf.goto_char(pt - ch.len_utf8());
            if !scan_backward_comment_fence(buf) {
                return false;
            }
            remaining -= 1;
            continue;
        }

        // Two-char comment end: current char has COMMENT_END_SECOND, prev
        // has COMMENT_END_FIRST.
        if flags.contains(SyntaxFlags::COMMENT_END_SECOND) {
            let prev_pos = pt - ch.len_utf8();
            if prev_pos > min {
                if let Some(ch2) = buf.char_before(prev_pos) {
                    let entry2 = buf.syntax_table.get_entry(ch2);
                    let flags2 = entry2.map(|e| e.flags).unwrap_or(SyntaxFlags::empty());
                    if flags2.contains(SyntaxFlags::COMMENT_END_FIRST) {
                        let style_b = flags.contains(SyntaxFlags::COMMENT_STYLE_B);
                        let nested = flags.contains(SyntaxFlags::COMMENT_NESTABLE)
                            || flags2.contains(SyntaxFlags::COMMENT_NESTABLE);
                        buf.goto_char(prev_pos - ch2.len_utf8());
                        if !scan_backward_comment_body(buf, style_b, nested) {
                            return false;
                        }
                        remaining -= 1;
                        continue;
                    }
                }
            }
        }

        // Not whitespace or comment end — stop.
        return false;
    }

    true
}

/// Scan backward through comment body to find matching comment start.
/// Point should be positioned right before the comment end.
fn scan_backward_comment_body(buf: &mut Buffer, style_b: bool, nested: bool) -> bool {
    let mut nesting = 1i32;

    loop {
        let pt = buf.point();
        let min = buf.point_min();
        if pt <= min {
            return false;
        }
        let Some(ch) = buf.char_before(pt) else {
            return false;
        };
        let entry = buf.syntax_table.get_entry(ch);
        let class = entry.map(|e| e.class).unwrap_or(SyntaxClass::Symbol);
        let flags = entry.map(|e| e.flags).unwrap_or(SyntaxFlags::empty());

        // Nested comment end (only if nested flag is set).
        if nested {
            if class == SyntaxClass::EndComment {
                let se_b = flags.contains(SyntaxFlags::COMMENT_STYLE_B);
                if se_b == style_b {
                    nesting += 1;
                    buf.goto_char(pt - ch.len_utf8());
                    continue;
                }
            }

            // Two-char comment end backward.
            if flags.contains(SyntaxFlags::COMMENT_END_SECOND) {
                let prev_pos = pt - ch.len_utf8();
                if prev_pos > min {
                    if let Some(ch2) = buf.char_before(prev_pos) {
                        let entry2 = buf.syntax_table.get_entry(ch2);
                        let flags2 = entry2.map(|e| e.flags).unwrap_or(SyntaxFlags::empty());
                        if flags2.contains(SyntaxFlags::COMMENT_END_FIRST) {
                            let se_b = flags.contains(SyntaxFlags::COMMENT_STYLE_B);
                            if se_b == style_b {
                                nesting += 1;
                                buf.goto_char(prev_pos - ch2.len_utf8());
                                continue;
                            }
                        }
                    }
                }
            }
        }

        // Single-char comment start (class `<`).
        if class == SyntaxClass::Comment {
            let sc_b = flags.contains(SyntaxFlags::COMMENT_STYLE_B);
            if sc_b == style_b {
                buf.goto_char(pt - ch.len_utf8());
                nesting -= 1;
                if nesting <= 0 {
                    return true;
                }
                continue;
            }
        }

        // Comment fence.
        if class == SyntaxClass::Generic {
            buf.goto_char(pt - ch.len_utf8());
            nesting -= 1;
            if nesting <= 0 {
                return true;
            }
            continue;
        }

        // Two-char comment start backward: COMMENT_START_SECOND on current,
        // COMMENT_START_FIRST on prev.
        if flags.contains(SyntaxFlags::COMMENT_START_SECOND) {
            let prev_pos = pt - ch.len_utf8();
            if prev_pos > min {
                if let Some(ch2) = buf.char_before(prev_pos) {
                    let entry2 = buf.syntax_table.get_entry(ch2);
                    let flags2 = entry2.map(|e| e.flags).unwrap_or(SyntaxFlags::empty());
                    if flags2.contains(SyntaxFlags::COMMENT_START_FIRST) {
                        let sc_b = flags.contains(SyntaxFlags::COMMENT_STYLE_B);
                        if sc_b == style_b {
                            buf.goto_char(prev_pos - ch2.len_utf8());
                            nesting -= 1;
                            if nesting <= 0 {
                                return true;
                            }
                            continue;
                        }
                    }
                }
            }
        }

        buf.goto_char(pt - ch.len_utf8());
    }
}

/// Scan backward for matching comment fence character.
fn scan_backward_comment_fence(buf: &mut Buffer) -> bool {
    loop {
        let pt = buf.point();
        if pt <= buf.point_min() {
            return false;
        }
        let Some(ch) = buf.char_before(pt) else {
            return false;
        };
        let entry = buf.syntax_table.get_entry(ch);
        let class = entry.map(|e| e.class).unwrap_or(SyntaxClass::Symbol);

        buf.goto_char(pt - ch.len_utf8());

        if class == SyntaxClass::Generic {
            return true;
        }
    }
}

/// `(backward-prefix-chars)` — move point backward over prefix-syntax chars.
pub(crate) fn builtin_backward_prefix_chars(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if !args.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("backward-prefix-chars"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let buf = eval
        .buffers
        .current_buffer_mut()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    loop {
        let pt = buf.point();
        if pt <= buf.point_min() {
            break;
        }
        let Some(ch) = buf.char_before(pt) else {
            break;
        };
        let is_prefix = buf
            .syntax_table
            .get_entry(ch)
            .map(|entry| entry.flags.contains(SyntaxFlags::PREFIX))
            .unwrap_or(false);
        if !is_prefix {
            break;
        }
        buf.goto_char(pt.saturating_sub(ch.len_utf8()));
    }

    Ok(Value::Nil)
}

/// `(forward-word &optional COUNT)` — move point forward COUNT words.
pub(crate) fn builtin_forward_word(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    let count = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        match &args[0] {
            Value::Int(n) => *n,
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), *other],
                ));
            }
        }
    };

    // We need to read the syntax table first, then call forward_word, then write point.
    // To satisfy the borrow checker, clone the syntax table.
    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = buf.syntax_table.clone();
    let new_pos = forward_word(buf, &table, count);

    let buf = eval
        .buffers
        .current_buffer_mut()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    buf.goto_char(new_pos);
    Ok(Value::Nil)
}

/// `(backward-word &optional COUNT)` — move point backward COUNT words.
pub(crate) fn builtin_backward_word(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    let count = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        match &args[0] {
            Value::Int(n) => *n,
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), *other],
                ));
            }
        }
    };

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = buf.syntax_table.clone();
    let new_pos = backward_word(buf, &table, count);

    let buf = eval
        .buffers
        .current_buffer_mut()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    buf.goto_char(new_pos);
    Ok(Value::Nil)
}

/// `(forward-sexp &optional COUNT)` — move point forward over COUNT balanced
/// expressions.
pub(crate) fn builtin_forward_sexp(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    let count = if args.is_empty() || args[0].is_nil() {
        1i64
    } else {
        match &args[0] {
            Value::Int(n) => *n,
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), *other],
                ));
            }
        }
    };

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = buf.syntax_table.clone();
    let from = buf.point();
    let new_pos = scan_sexps(buf, &table, from, count)
        .map_err(|msg| signal("scan-error", vec![Value::string(&msg)]))?;

    let buf = eval
        .buffers
        .current_buffer_mut()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    buf.goto_char(new_pos);
    Ok(Value::Nil)
}

/// `(backward-sexp &optional COUNT)` — move point backward over COUNT balanced
/// expressions.
pub(crate) fn builtin_backward_sexp(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    let count = if args.is_empty() || args[0].is_nil() {
        1i64
    } else {
        match &args[0] {
            Value::Int(n) => *n,
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), *other],
                ));
            }
        }
    };

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = buf.syntax_table.clone();
    let from = buf.point();
    // backward-sexp with positive count => scan_sexps with negative count
    let new_pos = scan_sexps(buf, &table, from, -count)
        .map_err(|msg| signal("scan-error", vec![Value::string(&msg)]))?;

    let buf = eval
        .buffers
        .current_buffer_mut()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    buf.goto_char(new_pos);
    Ok(Value::Nil)
}

/// `(scan-lists FROM COUNT DEPTH)` — scan across balanced expressions.
///
/// This uses the same core scanner as `forward-sexp`/`backward-sexp`.
pub(crate) fn builtin_scan_lists(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() != 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("scan-lists"), Value::Int(args.len() as i64)],
        ));
    }

    let from = match &args[0] {
        Value::Int(n) => *n,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("integer-or-marker-p"), *other],
            ));
        }
    };
    let count = match &args[1] {
        Value::Int(n) => *n,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("integerp"), *other],
            ));
        }
    };
    let _depth = match &args[2] {
        Value::Int(n) => *n,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("integerp"), *other],
            ));
        }
    };

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = buf.syntax_table.clone();

    let from_char = if from > 0 { from as usize - 1 } else { 0 };
    let from_byte = buf.text.char_to_byte(from_char.min(buf.text.char_count()));

    match scan_sexps(buf, &table, from_byte, count) {
        Ok(new_byte) => Ok(Value::Int(buf.text.byte_to_char(new_byte) as i64 + 1)),
        Err(_) if count < 0 => Ok(Value::Nil),
        Err(msg) => Err(signal("scan-error", vec![Value::string(&msg)])),
    }
}

/// `(scan-sexps FROM COUNT)` — scan over COUNT sexps from FROM.
pub(crate) fn builtin_scan_sexps(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() != 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("scan-sexps"), Value::Int(args.len() as i64)],
        ));
    }

    let from = match &args[0] {
        Value::Int(n) => *n,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("number-or-marker-p"), *other],
            ));
        }
    };
    let count = match &args[1] {
        Value::Int(n) => *n,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("integerp"), *other],
            ));
        }
    };

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = buf.syntax_table.clone();

    let from_char = if from > 0 { from as usize - 1 } else { 0 };
    let from_byte = buf.text.char_to_byte(from_char.min(buf.text.char_count()));

    match scan_sexps(buf, &table, from_byte, count) {
        Ok(new_byte) => Ok(Value::Int(buf.text.byte_to_char(new_byte) as i64 + 1)),
        Err(_) if count < 0 => Ok(Value::Nil),
        Err(msg) => Err(signal("scan-error", vec![Value::string(&msg)])),
    }
}

fn parse_state_from_range(buf: &Buffer, table: &SyntaxTable, from: i64, to: i64) -> Value {
    let chars: Vec<char> = buf.buffer_string().chars().collect();
    let from_idx = if from > 0 { from as usize - 1 } else { 0 };
    let to_idx = if to > 0 { to as usize - 1 } else { 0 }.min(chars.len());

    let mut depth = 0i64;
    let mut stack: Vec<i64> = Vec::new();
    let mut last_sexp_start: Option<i64> = None;
    let mut completed_toplevel_list_start: Option<i64> = None;

    if to_idx > from_idx {
        for (idx, ch) in chars[from_idx..to_idx].iter().enumerate() {
            let pos1 = (from_idx + idx + 1) as i64;
            match table.char_syntax(*ch) {
                SyntaxClass::Open => {
                    depth += 1;
                    stack.push(pos1);
                }
                SyntaxClass::Close => {
                    if depth > 0 {
                        depth -= 1;
                    }
                    if let Some(open_pos) = stack.pop() {
                        if depth == 0 {
                            completed_toplevel_list_start = Some(open_pos);
                        }
                    }
                }
                SyntaxClass::Whitespace | SyntaxClass::Comment | SyntaxClass::EndComment => {}
                _ => {
                    if last_sexp_start.is_none() {
                        last_sexp_start = Some(pos1);
                    }
                }
            }
        }
    }

    if let Some(open_pos) = completed_toplevel_list_start {
        last_sexp_start = Some(open_pos);
    }

    let stack_value = if depth > 0 {
        Value::list(stack.iter().map(|p| Value::Int(*p)).collect())
    } else {
        Value::Nil
    };

    Value::list(vec![
        Value::Int(depth),
        stack.last().map_or(Value::Nil, |p| Value::Int(*p)),
        last_sexp_start.map_or(Value::Nil, Value::Int),
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Int(0),
        Value::Nil,
        Value::Nil,
        stack_value,
        Value::Nil,
    ])
}

/// `(parse-partial-sexp FROM TO &optional TARGETDEPTH STOPBEFORE STATE COMMENTSTOP)`
/// Baseline parser-state implementation for structural Lisp motion/state queries.
pub(crate) fn builtin_parse_partial_sexp(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() < 2 || args.len() > 6 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("parse-partial-sexp"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let from = match &args[0] {
        Value::Int(n) => *n,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("number-or-marker-p"), *other],
            ));
        }
    };
    let to = match &args[1] {
        Value::Int(n) => *n,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("number-or-marker-p"), *other],
            ));
        }
    };

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = buf.syntax_table.clone();
    Ok(parse_state_from_range(buf, &table, from, to))
}

/// `(syntax-ppss &optional POS)` — parser state at POS.
pub(crate) fn builtin_syntax_ppss(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("syntax-ppss"), Value::Int(args.len() as i64)],
        ));
    }

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = buf.syntax_table.clone();

    let pos = if args.is_empty() || args[0].is_nil() {
        buf.point_char() as i64 + 1
    } else {
        match &args[0] {
            Value::Int(n) => *n,
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("number-or-marker-p"), *other],
                ));
            }
        }
    };

    Ok(parse_state_from_range(buf, &table, 1, pos))
}

/// `(syntax-ppss-flush-cache POS &rest _IGNORED)` — flush parser-state cache.
///
/// NeoVM currently computes parser state directly, so this is a no-op that
/// enforces Emacs-compatible arity/type behavior.
pub(crate) fn builtin_syntax_ppss_flush_cache(
    _eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("syntax-ppss-flush-cache"), Value::Int(0)],
        ));
    }

    match &args[0] {
        Value::Int(_) | Value::Char(_) => Ok(Value::Nil),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), *other],
        )),
    }
}

/// `(skip-syntax-forward SYNTAX &optional LIMIT)` — skip forward over chars
/// matching the given syntax classes.
pub(crate) fn builtin_skip_syntax_forward(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("skip-syntax-forward"), Value::Int(0)],
        ));
    }
    let syntax_chars = match &args[0] {
        Value::Str(_) => args[0].as_str().unwrap().to_string(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };
    let limit = if args.len() > 1 && !args[1].is_nil() {
        match &args[1] {
            Value::Int(n) => Some(*n as usize),
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), *other],
                ));
            }
        }
    } else {
        None
    };

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = buf.syntax_table.clone();
    let new_pos = skip_syntax_forward(buf, &table, &syntax_chars, limit);

    let old_pt = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?
        .point();

    let buf = eval
        .buffers
        .current_buffer_mut()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    buf.goto_char(new_pos);

    // Return number of characters skipped (Emacs convention).
    let chars_moved = if new_pos >= old_pt {
        buf.text.byte_to_char(new_pos) as i64 - buf.text.byte_to_char(old_pt) as i64
    } else {
        buf.text.byte_to_char(old_pt) as i64 - buf.text.byte_to_char(new_pos) as i64
    };
    Ok(Value::Int(chars_moved))
}

/// `(skip-syntax-backward SYNTAX &optional LIMIT)` — skip backward over chars
/// matching the given syntax classes.
pub(crate) fn builtin_skip_syntax_backward(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("skip-syntax-backward"), Value::Int(0)],
        ));
    }
    let syntax_chars = match &args[0] {
        Value::Str(_) => args[0].as_str().unwrap().to_string(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };
    let limit = if args.len() > 1 && !args[1].is_nil() {
        match &args[1] {
            Value::Int(n) => Some(*n as usize),
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), *other],
                ));
            }
        }
    } else {
        None
    };

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = buf.syntax_table.clone();
    let new_pos = skip_syntax_backward(buf, &table, &syntax_chars, limit);

    let old_pt = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?
        .point();

    let buf = eval
        .buffers
        .current_buffer_mut()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    buf.goto_char(new_pos);

    // Return negative number of characters skipped.
    let chars_moved = if old_pt >= new_pos {
        -(buf.text.byte_to_char(old_pt) as i64 - buf.text.byte_to_char(new_pos) as i64)
    } else {
        buf.text.byte_to_char(new_pos) as i64 - buf.text.byte_to_char(old_pt) as i64
    };
    Ok(Value::Int(chars_moved))
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
#[path = "syntax_test.rs"]
mod tests;
