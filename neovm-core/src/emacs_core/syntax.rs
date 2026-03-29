//! Syntax table system for the Elisp VM.
//!
//! Implements Emacs-compatible syntax tables with character classification,
//! motion functions (forward/backward word, sexp scanning), and the
//! `string-to-syntax` descriptor parser.

use std::cell::RefCell;
use std::collections::HashMap;

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::value::{RuntimeBindingValue, Value, list_to_vec, read_cons, with_heap};
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
/// `Context` between threads must restore that identity explicitly.
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

/// Pre-populate GNU Emacs syntax variables that are defined from C.
pub fn init_syntax_vars(
    obarray: &mut super::symbol::Obarray,
    custom: &mut super::custom::CustomManager,
) {
    obarray.set_symbol_value("parse-sexp-ignore-comments", Value::Nil);
    obarray.set_symbol_value("parse-sexp-lookup-properties", Value::Nil);
    obarray.set_symbol_value("syntax-propertize--done", Value::Int(-1));
    obarray.set_symbol_value("words-include-escapes", Value::Nil);
    obarray.set_symbol_value("multibyte-syntax-as-symbol", Value::Nil);
    obarray.set_symbol_value("open-paren-in-column-0-is-defun-start", Value::True);
    obarray.set_symbol_value(
        "find-word-boundary-function-table",
        super::chartable::make_char_table_value(Value::Nil, Value::Nil),
    );
    obarray.set_symbol_value("comment-end-can-be-escaped", Value::Nil);
    obarray.set_symbol_value("forward-comment-function", Value::Nil);

    for name in &[
        "parse-sexp-ignore-comments",
        "parse-sexp-lookup-properties",
        "syntax-propertize--done",
        "words-include-escapes",
        "multibyte-syntax-as-symbol",
        "open-paren-in-column-0-is-defun-start",
        "find-word-boundary-function-table",
        "comment-end-can-be-escaped",
        "forward-comment-function",
    ] {
        obarray.make_special(name);
    }

    custom.make_variable_buffer_local("syntax-propertize--done");
    obarray.make_buffer_local("syntax-propertize--done", true);
    custom.make_variable_buffer_local("comment-end-can-be-escaped");
    obarray.make_buffer_local("comment-end-can-be-escaped", true);
}

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
    forward_word_with_options(buf, table, count, false)
}

fn forward_word_with_options(
    buf: &Buffer,
    table: &SyntaxTable,
    count: i64,
    honor_properties: bool,
) -> usize {
    if count < 0 {
        return backward_word_with_options(buf, table, -count, honor_properties);
    }

    let text = buf.buffer_string();
    let chars: Vec<char> = text.chars().collect();
    // Convert byte pos to char index within accessible region.
    let base = buf.point_min();
    let rel_byte = buf.point().saturating_sub(base);
    let mut idx = buf.text.byte_to_char(base + rel_byte) - buf.text.byte_to_char(base);

    let accessible_char_start = buf.text.byte_to_char(base);
    let accessible_char_end = buf.point_max_char();
    let accessible_len = accessible_char_end - accessible_char_start;

    for _ in 0..count {
        // Skip non-word characters
        while idx < accessible_len
            && !matches!(
                effective_syntax_entry_for_abs_char(
                    buf,
                    table,
                    chars[idx],
                    accessible_char_start + idx,
                    honor_properties,
                )
                .class,
                SyntaxClass::Word
            )
        {
            idx += 1;
        }
        // Skip word characters
        while idx < accessible_len
            && matches!(
                effective_syntax_entry_for_abs_char(
                    buf,
                    table,
                    chars[idx],
                    accessible_char_start + idx,
                    honor_properties,
                )
                .class,
                SyntaxClass::Word
            )
        {
            idx += 1;
        }
    }

    // Convert char index back to byte position (absolute).
    let abs_char = accessible_char_start + idx;
    buf.text.char_to_byte(abs_char)
}

/// Move backward over `count` words.  Returns the resulting byte position.
pub fn backward_word(buf: &Buffer, table: &SyntaxTable, count: i64) -> usize {
    backward_word_with_options(buf, table, count, false)
}

fn backward_word_with_options(
    buf: &Buffer,
    table: &SyntaxTable,
    count: i64,
    honor_properties: bool,
) -> usize {
    if count < 0 {
        return forward_word_with_options(buf, table, -count, honor_properties);
    }

    let text = buf.buffer_string();
    let chars: Vec<char> = text.chars().collect();
    let base = buf.point_min();
    let rel_byte = buf.point().saturating_sub(base);
    let mut idx = buf.text.byte_to_char(base + rel_byte) - buf.text.byte_to_char(base);
    let accessible_char_start = buf.text.byte_to_char(base);

    for _ in 0..count {
        // Skip non-word characters backward
        while idx > 0
            && !matches!(
                effective_syntax_entry_for_abs_char(
                    buf,
                    table,
                    chars[idx - 1],
                    accessible_char_start + idx - 1,
                    honor_properties,
                )
                .class,
                SyntaxClass::Word
            )
        {
            idx -= 1;
        }
        // Skip word characters backward
        while idx > 0
            && matches!(
                effective_syntax_entry_for_abs_char(
                    buf,
                    table,
                    chars[idx - 1],
                    accessible_char_start + idx - 1,
                    honor_properties,
                )
                .class,
                SyntaxClass::Word
            )
        {
            idx -= 1;
        }
    }

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
    skip_syntax_forward_with_options(buf, table, syntax_chars, limit, false)
}

fn skip_syntax_forward_with_options(
    buf: &Buffer,
    table: &SyntaxTable,
    syntax_chars: &str,
    limit: Option<usize>,
    honor_properties: bool,
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
    let accessible_char_end = buf.point_max_char();
    let accessible_len = accessible_char_end - accessible_char_start;

    let char_limit = limit
        .map(|lim| {
            let lim_clamped = lim.min(buf.point_max());
            buf.text.byte_to_char(lim_clamped) - accessible_char_start
        })
        .unwrap_or(accessible_len);

    while idx < char_limit {
        let syn = effective_syntax_entry_for_abs_char(
            buf,
            table,
            chars[idx],
            accessible_char_start + idx,
            honor_properties,
        )
        .class;
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
    skip_syntax_backward_with_options(buf, table, syntax_chars, limit, false)
}

fn skip_syntax_backward_with_options(
    buf: &Buffer,
    table: &SyntaxTable,
    syntax_chars: &str,
    limit: Option<usize>,
    honor_properties: bool,
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
        let syn = effective_syntax_entry_for_abs_char(
            buf,
            table,
            chars[idx - 1],
            accessible_char_start + idx - 1,
            honor_properties,
        )
        .class;
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
    scan_sexps_with_options(buf, table, from, count, false)
}

fn scan_sexps_with_options(
    buf: &Buffer,
    table: &SyntaxTable,
    from: usize,
    count: i64,
    honor_properties: bool,
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
            idx = scan_sexp_forward(buf, &chars, total_chars, idx, table, honor_properties)?;
        }
    } else {
        for _ in 0..(-count) {
            idx = scan_sexp_backward(buf, &chars, idx, table, honor_properties)?;
        }
    }

    Ok(buf.text.char_to_byte(idx))
}

/// Scan one sexp forward from char index `start`.
fn scan_sexp_forward(
    buf: &Buffer,
    chars: &[char],
    len: usize,
    start: usize,
    table: &SyntaxTable,
    honor_properties: bool,
) -> Result<usize, String> {
    let mut idx = start;

    // Skip whitespace and comments
    while idx < len
        && matches!(
            effective_syntax_entry_for_abs_char(buf, table, chars[idx], idx, honor_properties)
                .class,
            SyntaxClass::Whitespace
                | SyntaxClass::Comment
                | SyntaxClass::EndComment
                | SyntaxClass::Punctuation
                | SyntaxClass::Prefix
        )
    {
        idx += 1;
    }

    if idx >= len {
        return Err("Scan error: unbalanced parentheses".to_string());
    }

    let ch = chars[idx];
    let syn_entry = effective_syntax_entry_for_abs_char(buf, table, ch, idx, honor_properties);
    let syn = syn_entry.class;

    match syn {
        SyntaxClass::Open => {
            // Find matching close, respecting nesting.
            let close_char = syn_entry.matching_char.unwrap_or(')');
            let mut depth = 1i32;
            idx += 1;
            while idx < len && depth > 0 {
                let c = chars[idx];
                let s =
                    effective_syntax_entry_for_abs_char(buf, table, c, idx, honor_properties).class;
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
                            let sc = effective_syntax_entry_for_abs_char(
                                buf,
                                table,
                                chars[idx],
                                idx,
                                honor_properties,
                            )
                            .class;
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
                let s =
                    effective_syntax_entry_for_abs_char(buf, table, c, idx, honor_properties).class;
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
                    effective_syntax_entry_for_abs_char(
                        buf,
                        table,
                        chars[idx],
                        idx,
                        honor_properties,
                    )
                    .class,
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
fn scan_sexp_backward(
    buf: &Buffer,
    chars: &[char],
    start: usize,
    table: &SyntaxTable,
    honor_properties: bool,
) -> Result<usize, String> {
    let mut idx = start;

    // Skip whitespace and comments backward
    while idx > 0
        && matches!(
            effective_syntax_entry_for_abs_char(
                buf,
                table,
                chars[idx - 1],
                idx - 1,
                honor_properties
            )
            .class,
            SyntaxClass::Whitespace
                | SyntaxClass::Comment
                | SyntaxClass::EndComment
                | SyntaxClass::Punctuation
                | SyntaxClass::Prefix
        )
    {
        idx -= 1;
    }

    if idx == 0 {
        return Err("Scan error: beginning of buffer".to_string());
    }

    idx -= 1; // move to the character we're examining
    let ch = chars[idx];
    let syn_entry = effective_syntax_entry_for_abs_char(buf, table, ch, idx, honor_properties);
    let syn = syn_entry.class;

    match syn {
        SyntaxClass::Close => {
            // Find matching open, respecting nesting.
            let open_char = syn_entry.matching_char.unwrap_or('(');
            let mut depth = 1i32;
            while idx > 0 && depth > 0 {
                idx -= 1;
                let c = chars[idx];
                let s =
                    effective_syntax_entry_for_abs_char(buf, table, c, idx, honor_properties).class;
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
                                let sc = effective_syntax_entry_for_abs_char(
                                    buf,
                                    table,
                                    chars[idx],
                                    idx,
                                    honor_properties,
                                )
                                .class;
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
                let s =
                    effective_syntax_entry_for_abs_char(buf, table, c, idx, honor_properties).class;
                if s == delim_class && (syn == SyntaxClass::StringFence || c == ch) {
                    break;
                }
                idx -= 1;
            }
            let c = chars[idx];
            let s = effective_syntax_entry_for_abs_char(buf, table, c, idx, honor_properties).class;
            if !(s == delim_class && (syn == SyntaxClass::StringFence || c == ch)) {
                return Err("Scan error: unterminated string".to_string());
            }
            Ok(idx)
        }
        SyntaxClass::Word | SyntaxClass::Symbol => {
            // Scan backward over word/symbol chars.
            while idx > 0
                && matches!(
                    effective_syntax_entry_for_abs_char(
                        buf,
                        table,
                        chars[idx - 1],
                        idx - 1,
                        honor_properties,
                    )
                    .class,
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
    let current_id = buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = buffers
        .get_mut(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    if let Some(RuntimeBindingValue::Bound(value)) =
        buf.get_buffer_local_binding(SYNTAX_TABLE_OBJECT_PROPERTY)
    {
        if builtin_syntax_table_p(vec![value])?.is_truthy() {
            return Ok(value);
        }
    }

    buf.set_buffer_local(SYNTAX_TABLE_OBJECT_PROPERTY, fallback);
    Ok(fallback)
}

fn current_buffer_syntax_table_object(eval: &mut super::eval::Context) -> Result<Value, Flow> {
    current_buffer_syntax_table_object_in_buffers(&mut eval.buffers)
}

pub(crate) fn sync_current_buffer_syntax_table_state(
    ctx: &mut super::eval::Context,
) -> Result<(), Flow> {
    let table = current_buffer_syntax_table_object_in_buffers(&mut ctx.buffers)?;
    let compiled = syntax_table_from_chartable(table)?;
    let current_id = ctx
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = ctx
        .buffers
        .get_mut(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    buf.syntax_table = compiled;
    Ok(())
}

fn set_current_buffer_syntax_table_object_in_buffers(
    buffers: &mut BufferManager,
    table: Value,
) -> Result<(), Flow> {
    let current_id = buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = buffers
        .get_mut(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    buf.set_buffer_local(SYNTAX_TABLE_OBJECT_PROPERTY, table);
    Ok(())
}

fn set_current_buffer_syntax_table_object(
    eval: &mut super::eval::Context,
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

fn syntax_entry_from_syntax_property(prop: Value, ch: char) -> Option<SyntaxEntry> {
    if builtin_syntax_table_p(vec![prop]).ok()?.is_truthy() {
        let raw =
            super::chartable::builtin_char_table_range(vec![prop, Value::Int(ch as i64)]).ok()?;
        syntax_entry_from_chartable_entry(&raw)
    } else {
        syntax_entry_from_chartable_entry(&prop)
    }
}

fn effective_syntax_entry_for_char_at_byte(
    buf: &Buffer,
    table: &SyntaxTable,
    ch: char,
    byte_pos: usize,
    honor_properties: bool,
) -> SyntaxEntry {
    if honor_properties
        && let Some(prop) = buf.text.text_props_get_property(byte_pos, "syntax-table")
        && let Some(entry) = syntax_entry_from_syntax_property(prop, ch)
    {
        return entry;
    }

    table
        .get_entry(ch)
        .cloned()
        .unwrap_or_else(|| SyntaxEntry::simple(table.char_syntax(ch)))
}

fn effective_syntax_entry_for_abs_char(
    buf: &Buffer,
    table: &SyntaxTable,
    ch: char,
    abs_char: usize,
    honor_properties: bool,
) -> SyntaxEntry {
    let byte_pos = buf.text.char_to_byte(abs_char);
    effective_syntax_entry_for_char_at_byte(buf, table, ch, byte_pos, honor_properties)
}

fn parse_sexp_lookup_properties_enabled(ctx: &super::eval::Context) -> bool {
    ctx.obarray
        .symbol_value("parse-sexp-lookup-properties")
        .copied()
        .unwrap_or(Value::Nil)
        .is_truthy()
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
pub(crate) fn builtin_matching_paren(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_matching_paren_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_matching_paren_in_buffers(
    buffers: &BufferManager,
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
    if let Some(buf) = buffers.current_buffer() {
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
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_syntax_table_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_syntax_table_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    if !args.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("syntax-table"), Value::Int(args.len() as i64)],
        ));
    }
    current_buffer_syntax_table_object_in_buffers(buffers)
}

/// `(set-syntax-table TABLE)` — install TABLE for current buffer and return it.
///
/// NeoVM currently stores syntax behavior on `Buffer.syntax_table` internals;
/// this installs the exposed syntax-table object for compatibility and returns it.
pub(crate) fn builtin_set_syntax_table(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_set_syntax_table_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_set_syntax_table_in_buffers(
    buffers: &mut BufferManager,
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
    set_current_buffer_syntax_table_object_in_buffers(buffers, table)?;
    let compiled = syntax_table_from_chartable(table)?;
    let current_id = buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = buffers
        .get_mut(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    buf.syntax_table = compiled;
    Ok(table)
}

// ===========================================================================
// Builtin functions (evaluator-dependent — operate on current buffer)
// ===========================================================================

/// `(modify-syntax-entry CHAR NEWENTRY &optional SYNTAX-TABLE)`
pub(crate) fn builtin_modify_syntax_entry(
    eval: &mut super::eval::Context,
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
    let current_id = buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = buffers
        .get_mut(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    buf.syntax_table = compiled;
    Ok(Value::Nil)
}

/// `(char-syntax CHAR)` — return the syntax class designator char.
pub(crate) fn builtin_char_syntax(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_char_syntax_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_char_syntax_in_buffers(
    buffers: &BufferManager,
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

    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let class = buf.syntax_table.char_syntax(ch);
    Ok(Value::Char(class.to_char()))
}

/// `(syntax-after POS)` — return syntax descriptor for char at POS.
pub(crate) fn builtin_syntax_after(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_syntax_after_in_buffers(&eval.buffers, args)
}

pub(crate) fn builtin_syntax_after_in_buffers(
    buffers: &BufferManager,
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

    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    let char_index = pos as usize - 1;
    let byte_index = buf.text.char_to_byte(char_index.min(buf.text.char_count()));
    let Some(ch) = buf.char_after(byte_index) else {
        return Ok(Value::Nil);
    };

    let entry =
        effective_syntax_entry_for_char_at_byte(buf, &buf.syntax_table, ch, byte_index, true);
    Ok(syntax_entry_to_value(&entry))
}

/// `(forward-comment COUNT)` — move point over COUNT comment/whitespace
/// constructs. Returns `t` if all COUNT were successfully skipped, `nil`
/// if scanning stopped early (hit non-comment/non-whitespace or buffer
/// boundary).
pub(crate) fn builtin_forward_comment(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let honor_properties = parse_sexp_lookup_properties_enabled(eval);
    builtin_forward_comment_in_buffers(&mut eval.buffers, args, honor_properties)
}

pub(crate) fn builtin_forward_comment_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
    honor_properties: bool,
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

    let current_id = buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = buffers
        .get_mut(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    if count == 0 {
        return Ok(Value::True);
    }

    if count > 0 {
        let ok = forward_comment_forward(buf, count as u64, honor_properties);
        return Ok(if ok { Value::True } else { Value::Nil });
    } else {
        let ok = forward_comment_backward(buf, (-count) as u64, honor_properties);
        return Ok(if ok { Value::True } else { Value::Nil });
    }
}

/// Skip whitespace and comments forward. Returns true if all `count`
/// comments were skipped successfully.
fn forward_comment_forward(buf: &mut Buffer, count: u64, honor_properties: bool) -> bool {
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
            let entry = effective_syntax_entry_for_char_at_byte(
                buf,
                &buf.syntax_table,
                ch,
                pt,
                honor_properties,
            );
            let class = entry.class;

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
        let entry = effective_syntax_entry_for_char_at_byte(
            buf,
            &buf.syntax_table,
            ch,
            pt,
            honor_properties,
        );
        let class = entry.class;
        let flags = entry.flags;

        // Single-char comment start (class `<`).
        if class == SyntaxClass::Comment {
            let style_b = flags.contains(SyntaxFlags::COMMENT_STYLE_B);
            let nested = flags.contains(SyntaxFlags::COMMENT_NESTABLE);
            buf.goto_char(pt + ch.len_utf8());
            if !scan_forward_comment_body(buf, style_b, nested, honor_properties) {
                return false;
            }
            remaining -= 1;
            continue;
        }

        // Comment fence (class `!` = Generic).
        if class == SyntaxClass::Generic {
            buf.goto_char(pt + ch.len_utf8());
            // Scan forward for matching comment fence.
            if !scan_forward_comment_fence(buf, honor_properties) {
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
                    let entry2 = effective_syntax_entry_for_char_at_byte(
                        buf,
                        &buf.syntax_table,
                        ch2,
                        next_pos,
                        honor_properties,
                    );
                    let flags2 = entry2.flags;
                    if flags2.contains(SyntaxFlags::COMMENT_START_SECOND) {
                        let style_b = flags2.contains(SyntaxFlags::COMMENT_STYLE_B);
                        let nested = flags2.contains(SyntaxFlags::COMMENT_NESTABLE)
                            || flags.contains(SyntaxFlags::COMMENT_NESTABLE);
                        buf.goto_char(next_pos + ch2.len_utf8());
                        if !scan_forward_comment_body(buf, style_b, nested, honor_properties) {
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
fn scan_forward_comment_body(
    buf: &mut Buffer,
    style_b: bool,
    nested: bool,
    honor_properties: bool,
) -> bool {
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
        let entry = effective_syntax_entry_for_char_at_byte(
            buf,
            &buf.syntax_table,
            ch,
            pt,
            honor_properties,
        );
        let class = entry.class;
        let flags = entry.flags;

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
                        let entry2 = effective_syntax_entry_for_char_at_byte(
                            buf,
                            &buf.syntax_table,
                            ch2,
                            next_pos,
                            honor_properties,
                        );
                        let flags2 = entry2.flags;
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
                    let entry2 = effective_syntax_entry_for_char_at_byte(
                        buf,
                        &buf.syntax_table,
                        ch2,
                        next_pos,
                        honor_properties,
                    );
                    let flags2 = entry2.flags;
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
fn scan_forward_comment_fence(buf: &mut Buffer, honor_properties: bool) -> bool {
    loop {
        let pt = buf.point();
        if pt >= buf.point_max() {
            return false;
        }
        let Some(ch) = buf.char_after(pt) else {
            return false;
        };
        let entry = effective_syntax_entry_for_char_at_byte(
            buf,
            &buf.syntax_table,
            ch,
            pt,
            honor_properties,
        );
        let class = entry.class;

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
fn forward_comment_backward(buf: &mut Buffer, count: u64, honor_properties: bool) -> bool {
    let mut remaining = count;

    // Outer loop: skip `remaining` comments backward.
    while remaining > 0 {
        // Inner loop: scan backward character-by-character to find one
        // comment to skip.  This mirrors GNU Emacs's Fforward_comment
        // backward logic: each iteration decrements point, inspects
        // the character, and either (a) skips whitespace, (b) enters
        // backward comment scanning, or (c) gives up on
        // non-comment/non-whitespace.
        loop {
            let pt = buf.point();
            let min = buf.point_min();
            if pt <= min {
                return false;
            }
            let Some(ch) = buf.char_before(pt) else {
                return false;
            };
            let ch_pos = pt - ch.len_utf8();
            let entry = effective_syntax_entry_for_char_at_byte(
                buf,
                &buf.syntax_table,
                ch,
                ch_pos,
                honor_properties,
            );
            let class = entry.class;
            let flags = entry.flags;

            let mut code = class;
            let mut comstyle_b = false;
            let mut nested = flags.contains(SyntaxFlags::COMMENT_NESTABLE);

            if class == SyntaxClass::EndComment {
                comstyle_b = flags.contains(SyntaxFlags::COMMENT_STYLE_B);
            }

            // Check for two-char comment end: current char has
            // COMMENT_END_SECOND, prev char has COMMENT_END_FIRST.
            if flags.contains(SyntaxFlags::COMMENT_END_SECOND) {
                let prev_pos = pt - ch.len_utf8();
                if prev_pos > min {
                    if let Some(ch2) = buf.char_before(prev_pos) {
                        let ch2_pos = prev_pos - ch2.len_utf8();
                        let entry2 = effective_syntax_entry_for_char_at_byte(
                            buf,
                            &buf.syntax_table,
                            ch2,
                            ch2_pos,
                            honor_properties,
                        );
                        let flags2 = entry2.flags;
                        if flags2.contains(SyntaxFlags::COMMENT_END_FIRST) {
                            code = SyntaxClass::EndComment;
                            comstyle_b = flags.contains(SyntaxFlags::COMMENT_STYLE_B);
                            nested = nested || flags2.contains(SyntaxFlags::COMMENT_NESTABLE);
                            // Move past both chars of the two-char end.
                            buf.goto_char(prev_pos - ch2.len_utf8());
                        }
                    }
                }
            }

            // Comment fence backward.
            if code == SyntaxClass::Generic {
                buf.goto_char(pt - ch.len_utf8());
                if !scan_backward_comment_fence(buf, honor_properties) {
                    buf.goto_char(pt);
                    return false;
                }
                // Successfully skipped one comment via fence.
                break;
            }

            if code == SyntaxClass::EndComment {
                // If we didn't already move point for a two-char end,
                // move past the single-char end now.
                if buf.point() == pt {
                    buf.goto_char(pt - ch.len_utf8());
                }
                let saved = buf.point();
                if scan_backward_comment_body(buf, comstyle_b, nested, honor_properties) {
                    // Successfully scanned back through the comment body.
                    break;
                }
                // scan_backward_comment_body failed.
                if ch == '\n' {
                    // GNU: "This end-of-line is not an end-of-comment.
                    // Treat it like a whitespace."
                    // Restore to just before the newline and continue
                    // the inner loop.
                    buf.goto_char(pt - ch.len_utf8());
                    continue;
                }
                // Non-newline EndComment that failed to find a matching
                // comment start — failure.
                if class != SyntaxClass::EndComment {
                    // Was a two-char sequence: restore one char forward.
                    buf.goto_char(saved + ch.len_utf8());
                } else {
                    buf.goto_char(pt);
                }
                return false;
            }

            if class == SyntaxClass::Whitespace {
                buf.goto_char(pt - ch.len_utf8());
                continue;
            }

            // Not whitespace, not comment end — stop.
            return false;
        }
        remaining -= 1;
    }

    true
}

/// Scan backward through comment body to find matching comment start.
///
/// This is a simplified version of GNU Emacs's `back_comment()`.  Point
/// should be positioned right after the comment-end delimiter has been
/// consumed (i.e. just before the comment body).
///
/// For **nested** comments the function returns as soon as the nesting
/// count drops to zero.
///
/// For **non-nested** comments the function scans all the way backward,
/// recording the *earliest* comment-starter of the matching style it
/// finds.  A same-style comment-ender encountered during the scan means
/// "anything before this belongs to a different comment" and stops the
/// search.  At the end, point is set to the recorded position.
fn scan_backward_comment_body(
    buf: &mut Buffer,
    style_b: bool,
    nested: bool,
    honor_properties: bool,
) -> bool {
    let mut nesting = 1i32;

    // For non-nested comments: record the earliest matching comment-start
    // seen so far.
    let mut comstart_pos: Option<usize> = None;

    loop {
        let pt = buf.point();
        let min = buf.point_min();
        if pt <= min {
            // Reached beginning of accessible region.
            break;
        }
        let Some(ch) = buf.char_before(pt) else {
            break;
        };
        let ch_pos = pt - ch.len_utf8();
        let entry = effective_syntax_entry_for_char_at_byte(
            buf,
            &buf.syntax_table,
            ch,
            ch_pos,
            honor_properties,
        );
        let class = entry.class;
        let flags = entry.flags;

        // ── Comment-end (same style) ──────────────────────────────
        // For nested: increases nesting.
        // For non-nested: means our comment can't extend past this,
        //   so stop scanning.
        if class == SyntaxClass::EndComment {
            let se_b = flags.contains(SyntaxFlags::COMMENT_STYLE_B);
            if se_b == style_b {
                if nested {
                    nesting += 1;
                    buf.goto_char(pt - ch.len_utf8());
                    continue;
                } else {
                    // Non-nested: this is a same-style comment ender.
                    // Anything before this can't be our comment start
                    // because it would match this ender instead.
                    break;
                }
            }
        }

        // Two-char comment end backward.
        if flags.contains(SyntaxFlags::COMMENT_END_SECOND) {
            let prev_pos = pt - ch.len_utf8();
            if prev_pos > min {
                if let Some(ch2) = buf.char_before(prev_pos) {
                    let ch2_pos = prev_pos - ch2.len_utf8();
                    let entry2 = effective_syntax_entry_for_char_at_byte(
                        buf,
                        &buf.syntax_table,
                        ch2,
                        ch2_pos,
                        honor_properties,
                    );
                    let flags2 = entry2.flags;
                    if flags2.contains(SyntaxFlags::COMMENT_END_FIRST) {
                        let se_b = flags.contains(SyntaxFlags::COMMENT_STYLE_B);
                        if se_b == style_b {
                            if nested {
                                nesting += 1;
                                buf.goto_char(prev_pos - ch2.len_utf8());
                                continue;
                            } else {
                                break;
                            }
                        }
                    }
                }
            }
        }

        // ── Single-char comment start (class `<`) ────────────────
        if class == SyntaxClass::Comment {
            let sc_b = flags.contains(SyntaxFlags::COMMENT_STYLE_B);
            if sc_b == style_b {
                let new_pos = pt - ch.len_utf8();
                if nested {
                    buf.goto_char(new_pos);
                    nesting -= 1;
                    if nesting <= 0 {
                        return true;
                    }
                    continue;
                } else {
                    // Non-nested: record this as the best (earliest)
                    // comment-start candidate and keep scanning.
                    comstart_pos = Some(new_pos);
                    buf.goto_char(new_pos);
                    continue;
                }
            }
        }

        // ── Comment fence ────────────────────────────────────────
        if class == SyntaxClass::Generic {
            let new_pos = pt - ch.len_utf8();
            buf.goto_char(new_pos);
            if nested {
                nesting -= 1;
                if nesting <= 0 {
                    return true;
                }
            } else {
                comstart_pos = Some(new_pos);
            }
            continue;
        }

        // ── Two-char comment start backward ──────────────────────
        // COMMENT_START_SECOND on current char, COMMENT_START_FIRST
        // on the preceding char.
        if flags.contains(SyntaxFlags::COMMENT_START_SECOND) {
            let prev_pos = pt - ch.len_utf8();
            if prev_pos > min {
                if let Some(ch2) = buf.char_before(prev_pos) {
                    let ch2_pos = prev_pos - ch2.len_utf8();
                    let entry2 = effective_syntax_entry_for_char_at_byte(
                        buf,
                        &buf.syntax_table,
                        ch2,
                        ch2_pos,
                        honor_properties,
                    );
                    let flags2 = entry2.flags;
                    if flags2.contains(SyntaxFlags::COMMENT_START_FIRST) {
                        let sc_b = flags.contains(SyntaxFlags::COMMENT_STYLE_B);
                        if sc_b == style_b {
                            let new_pos = prev_pos - ch2.len_utf8();
                            if nested {
                                buf.goto_char(new_pos);
                                nesting -= 1;
                                if nesting <= 0 {
                                    return true;
                                }
                                continue;
                            } else {
                                comstart_pos = Some(new_pos);
                                buf.goto_char(new_pos);
                                continue;
                            }
                        }
                    }
                }
            }
        }

        // Default: skip this character and continue scanning.
        buf.goto_char(pt - ch.len_utf8());
    }

    // For non-nested comments, check if we recorded any comment-start.
    if !nested {
        if let Some(pos) = comstart_pos {
            buf.goto_char(pos);
            return true;
        }
    }

    false
}

/// Scan backward for matching comment fence character.
fn scan_backward_comment_fence(buf: &mut Buffer, honor_properties: bool) -> bool {
    loop {
        let pt = buf.point();
        if pt <= buf.point_min() {
            return false;
        }
        let Some(ch) = buf.char_before(pt) else {
            return false;
        };
        let ch_pos = pt - ch.len_utf8();
        let entry = effective_syntax_entry_for_char_at_byte(
            buf,
            &buf.syntax_table,
            ch,
            ch_pos,
            honor_properties,
        );
        let class = entry.class;

        buf.goto_char(pt - ch.len_utf8());

        if class == SyntaxClass::Generic {
            return true;
        }
    }
}

/// `(backward-prefix-chars)` — move point backward over prefix-syntax chars.
pub(crate) fn builtin_backward_prefix_chars(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let honor_properties = parse_sexp_lookup_properties_enabled(eval);
    builtin_backward_prefix_chars_in_buffers(&mut eval.buffers, args, honor_properties)
}

pub(crate) fn builtin_backward_prefix_chars_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
    honor_properties: bool,
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

    let current_id = buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = buffers
        .get_mut(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    loop {
        let pt = buf.point();
        if pt <= buf.point_min() {
            break;
        }
        let Some(ch) = buf.char_before(pt) else {
            break;
        };
        let ch_pos = pt - ch.len_utf8();
        let entry = effective_syntax_entry_for_char_at_byte(
            buf,
            &buf.syntax_table,
            ch,
            ch_pos,
            honor_properties,
        );
        let is_prefix =
            entry.class == SyntaxClass::Prefix || entry.flags.contains(SyntaxFlags::PREFIX);
        if !is_prefix {
            break;
        }
        buf.goto_char(pt.saturating_sub(ch.len_utf8()));
    }

    Ok(Value::Nil)
}

/// `(forward-word &optional COUNT)` — move point forward COUNT words.
pub(crate) fn builtin_forward_word(
    eval: &mut super::eval::Context,
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
    let honor_properties = parse_sexp_lookup_properties_enabled(eval);
    let new_pos = forward_word_with_options(buf, &table, count, honor_properties);

    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = eval.buffers.goto_buffer_byte(current_id, new_pos);
    Ok(Value::Nil)
}

pub(crate) fn builtin_forward_word_in_buffers(
    buffers: &mut BufferManager,
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
    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = buf.syntax_table.clone();
    let new_pos = forward_word(buf, &table, count);

    let current_id = buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = buffers.goto_buffer_byte(current_id, new_pos);
    Ok(Value::Nil)
}

/// `(backward-word &optional COUNT)` — move point backward COUNT words.
pub(crate) fn builtin_backward_word(
    eval: &mut super::eval::Context,
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
    let honor_properties = parse_sexp_lookup_properties_enabled(eval);
    let new_pos = backward_word_with_options(buf, &table, count, honor_properties);

    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = eval.buffers.goto_buffer_byte(current_id, new_pos);
    Ok(Value::Nil)
}

/// `(forward-sexp &optional COUNT)` — move point forward over COUNT balanced
/// expressions.
pub(crate) fn builtin_forward_sexp(
    eval: &mut super::eval::Context,
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
    let honor_properties = parse_sexp_lookup_properties_enabled(eval);
    let new_pos = scan_sexps_with_options(buf, &table, from, count, honor_properties)
        .map_err(|msg| signal("scan-error", vec![Value::string(&msg)]))?;

    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = eval.buffers.goto_buffer_byte(current_id, new_pos);
    Ok(Value::Nil)
}

/// `(backward-sexp &optional COUNT)` — move point backward over COUNT balanced
/// expressions.
pub(crate) fn builtin_backward_sexp(
    eval: &mut super::eval::Context,
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
    let honor_properties = parse_sexp_lookup_properties_enabled(eval);
    // backward-sexp with positive count => scan_sexps with negative count
    let new_pos = scan_sexps_with_options(buf, &table, from, -count, honor_properties)
        .map_err(|msg| signal("scan-error", vec![Value::string(&msg)]))?;

    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = eval.buffers.goto_buffer_byte(current_id, new_pos);
    Ok(Value::Nil)
}

/// `(scan-lists FROM COUNT DEPTH)` — scan across balanced expressions.
///
/// This uses the same core scanner as `forward-sexp`/`backward-sexp`.
pub(crate) fn builtin_scan_lists(ctx: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
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

    let buf = ctx
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = buf.syntax_table.clone();

    let from_char = if from > 0 { from as usize - 1 } else { 0 };
    let from_byte = buf.text.char_to_byte(from_char.min(buf.text.char_count()));

    let honor_properties = parse_sexp_lookup_properties_enabled(ctx);
    match scan_sexps_with_options(buf, &table, from_byte, count, honor_properties) {
        Ok(new_byte) => Ok(Value::Int(buf.text.byte_to_char(new_byte) as i64 + 1)),
        Err(_) if count < 0 => Ok(Value::Nil),
        Err(msg) => Err(signal("scan-error", vec![Value::string(&msg)])),
    }
}

/// `(scan-sexps FROM COUNT)` — scan over COUNT sexps from FROM.
pub(crate) fn builtin_scan_sexps(ctx: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
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

    let buf = ctx
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = buf.syntax_table.clone();

    let from_char = if from > 0 { from as usize - 1 } else { 0 };
    let from_byte = buf.text.char_to_byte(from_char.min(buf.text.char_count()));

    let honor_properties = parse_sexp_lookup_properties_enabled(ctx);
    match scan_sexps_with_options(buf, &table, from_byte, count, honor_properties) {
        Ok(new_byte) => Ok(Value::Int(buf.text.byte_to_char(new_byte) as i64 + 1)),
        Err(_) if count < 0 => Ok(Value::Nil),
        Err(msg) => Err(signal("scan-error", vec![Value::string(&msg)])),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParseStringState {
    Delim(char),
    Fence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParseCommentState {
    Syntax {
        depth: i64,
        style_b: bool,
        nestable: bool,
    },
    Fence {
        depth: i64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommentStopMode {
    None,
    Comment,
    SyntaxTable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PartialParseState {
    depth: i64,
    mindepth: i64,
    stack: Vec<i64>,
    last_sexp_start: Option<i64>,
    completed_toplevel_list_start: Option<i64>,
    in_string: Option<ParseStringState>,
    in_comment: Option<ParseCommentState>,
    comment_or_string_start: Option<i64>,
    quoted: bool,
}

impl PartialParseState {
    fn new() -> Self {
        Self {
            depth: 0,
            mindepth: 0,
            stack: Vec::new(),
            last_sexp_start: None,
            completed_toplevel_list_start: None,
            in_string: None,
            in_comment: None,
            comment_or_string_start: None,
            quoted: false,
        }
    }

    fn from_oldstate(oldstate: Option<&Value>) -> Self {
        let mut state = Self::new();
        let Some(oldstate) = oldstate else {
            return state;
        };
        let Some(items) = list_to_vec(oldstate) else {
            return state;
        };

        state.depth = items
            .first()
            .and_then(|v| match v {
                Value::Int(n) => Some(*n),
                _ => None,
            })
            .unwrap_or(0);

        if let Some(Value::Int(start)) = items.get(8) {
            state.comment_or_string_start = Some(*start);
        }

        if let Some(Value::True) = items.get(5) {
            state.quoted = true;
        }

        if let Some(item) = items.get(3) {
            state.in_string = match item {
                Value::Nil => None,
                Value::True => Some(ParseStringState::Fence),
                Value::Int(n) => u32::try_from(*n)
                    .ok()
                    .and_then(char::from_u32)
                    .map(ParseStringState::Delim),
                _ => None,
            };
        }

        if let Some(item) = items.get(4) {
            state.in_comment = match item {
                Value::Nil => None,
                Value::True => Some(ParseCommentState::Syntax {
                    depth: 1,
                    style_b: false,
                    nestable: false,
                }),
                Value::Int(n) => Some(ParseCommentState::Syntax {
                    depth: *n,
                    style_b: false,
                    nestable: true,
                }),
                _ => None,
            };
        }

        if let Some(item) = items.get(9)
            && let Some(stack_items) = list_to_vec(item)
        {
            state.stack = stack_items
                .into_iter()
                .filter_map(|v| match v {
                    Value::Int(n) => Some(n),
                    _ => None,
                })
                .collect();
        }

        state
    }

    fn into_value(mut self) -> Value {
        if let Some(open_pos) = self.completed_toplevel_list_start {
            self.last_sexp_start = Some(open_pos);
        }

        let stack_value = if self.depth > 0 {
            Value::list(self.stack.iter().map(|p| Value::Int(*p)).collect())
        } else {
            Value::Nil
        };

        let string_value = match self.in_string {
            Some(ParseStringState::Delim(term)) => Value::Int(term as i64),
            Some(ParseStringState::Fence) => Value::True,
            None => Value::Nil,
        };

        let comment_value = match self.in_comment {
            Some(ParseCommentState::Syntax {
                depth: comment_depth,
                nestable: false,
                ..
            }) => {
                debug_assert_eq!(comment_depth, 1);
                Value::True
            }
            Some(ParseCommentState::Syntax {
                depth: comment_depth,
                nestable: true,
                ..
            }) => Value::Int(comment_depth),
            Some(ParseCommentState::Fence {
                depth: comment_depth,
            }) => Value::Int(comment_depth),
            None => Value::Nil,
        };

        Value::list(vec![
            Value::Int(self.depth),
            self.stack.last().map_or(Value::Nil, |p| Value::Int(*p)),
            self.last_sexp_start.map_or(Value::Nil, Value::Int),
            string_value,
            comment_value,
            if self.quoted { Value::True } else { Value::Nil },
            Value::Int(self.mindepth),
            Value::Nil,
            self.comment_or_string_start.map_or(Value::Nil, Value::Int),
            stack_value,
            Value::Nil,
        ])
    }
}

fn syntax_class_and_flags(
    buf: &Buffer,
    table: &SyntaxTable,
    ch: char,
    abs_char: usize,
    honor_properties: bool,
) -> (SyntaxClass, SyntaxFlags) {
    let entry = effective_syntax_entry_for_abs_char(buf, table, ch, abs_char, honor_properties);
    (entry.class, entry.flags)
}

fn parse_commentstop_mode(arg: Option<&Value>) -> CommentStopMode {
    match arg {
        None | Some(Value::Nil) => CommentStopMode::None,
        Some(Value::Symbol(sym)) if resolve_sym(*sym) == "syntax-table" => {
            CommentStopMode::SyntaxTable
        }
        Some(_) => CommentStopMode::Comment,
    }
}

fn parse_state_from_range_with_options(
    buf: &Buffer,
    table: &SyntaxTable,
    from: i64,
    to: i64,
    oldstate: Option<&Value>,
    commentstop: CommentStopMode,
    honor_properties: bool,
) -> (Value, i64) {
    let chars: Vec<char> = buf.buffer_string().chars().collect();
    let accessible_char_start = buf.text.byte_to_char(buf.point_min());
    let from_idx = if from > 0 { from as usize - 1 } else { 0 }.min(chars.len());
    let to_idx = if to > 0 { to as usize - 1 } else { 0 }.min(chars.len());

    let mut state = PartialParseState::from_oldstate(oldstate);
    let mut idx = from_idx;

    while idx < to_idx {
        let pos1 = (idx + 1) as i64;
        let ch = chars[idx];
        let (class, flags) = syntax_class_and_flags(
            buf,
            table,
            ch,
            accessible_char_start + idx,
            honor_properties,
        );

        if state.quoted {
            state.quoted = false;
            idx += 1;
            continue;
        }

        if let Some(string_state) = state.in_string {
            match class {
                SyntaxClass::Escape | SyntaxClass::CharQuote => {
                    idx += 1;
                    if idx < to_idx {
                        idx += 1;
                    } else {
                        state.quoted = true;
                    }
                    continue;
                }
                SyntaxClass::StringFence if string_state == ParseStringState::Fence => {
                    state.in_string = None;
                    state.comment_or_string_start = None;
                    idx += 1;
                    if commentstop == CommentStopMode::SyntaxTable {
                        break;
                    }
                    continue;
                }
                SyntaxClass::StringDelim if matches!(string_state, ParseStringState::Delim(term) if ch == term) =>
                {
                    state.in_string = None;
                    state.comment_or_string_start = None;
                    idx += 1;
                    if commentstop == CommentStopMode::SyntaxTable {
                        break;
                    }
                    continue;
                }
                _ => {
                    idx += 1;
                    continue;
                }
            }
        }

        if let Some(comment_state) = state.in_comment {
            match comment_state {
                ParseCommentState::Fence {
                    depth: comment_depth,
                } => {
                    if class == SyntaxClass::Generic {
                        let next_depth = comment_depth - 1;
                        idx += 1;
                        if next_depth <= 0 {
                            state.in_comment = None;
                            state.comment_or_string_start = None;
                        } else {
                            state.in_comment = Some(ParseCommentState::Fence { depth: next_depth });
                        }
                        if commentstop == CommentStopMode::SyntaxTable {
                            break;
                        }
                        continue;
                    }
                    if matches!(class, SyntaxClass::Escape | SyntaxClass::CharQuote) {
                        idx += 1;
                        if idx < to_idx {
                            idx += 1;
                        } else {
                            state.quoted = true;
                        }
                        continue;
                    }
                    idx += 1;
                    continue;
                }
                ParseCommentState::Syntax {
                    depth: comment_depth,
                    style_b,
                    nestable,
                } => {
                    if matches!(class, SyntaxClass::Escape | SyntaxClass::CharQuote) {
                        idx += 1;
                        if idx < to_idx {
                            idx += 1;
                        } else {
                            state.quoted = true;
                        }
                        continue;
                    }

                    if nestable {
                        if class == SyntaxClass::Comment
                            && flags.contains(SyntaxFlags::COMMENT_STYLE_B) == style_b
                        {
                            state.in_comment = Some(ParseCommentState::Syntax {
                                depth: comment_depth + 1,
                                style_b,
                                nestable,
                            });
                            idx += 1;
                            continue;
                        }

                        if flags.contains(SyntaxFlags::COMMENT_START_FIRST) && idx + 1 < to_idx {
                            let (_, next_flags) = syntax_class_and_flags(
                                buf,
                                table,
                                chars[idx + 1],
                                accessible_char_start + idx + 1,
                                honor_properties,
                            );
                            if next_flags.contains(SyntaxFlags::COMMENT_START_SECOND)
                                && next_flags.contains(SyntaxFlags::COMMENT_STYLE_B) == style_b
                            {
                                state.in_comment = Some(ParseCommentState::Syntax {
                                    depth: comment_depth + 1,
                                    style_b,
                                    nestable,
                                });
                                idx += 2;
                                continue;
                            }
                        }
                    }

                    if class == SyntaxClass::EndComment
                        && flags.contains(SyntaxFlags::COMMENT_STYLE_B) == style_b
                    {
                        let next_depth = comment_depth - 1;
                        idx += 1;
                        if next_depth <= 0 {
                            state.in_comment = None;
                            state.comment_or_string_start = None;
                        } else {
                            state.in_comment = Some(ParseCommentState::Syntax {
                                depth: next_depth,
                                style_b,
                                nestable,
                            });
                        }
                        if commentstop == CommentStopMode::SyntaxTable {
                            break;
                        }
                        continue;
                    }

                    if flags.contains(SyntaxFlags::COMMENT_END_FIRST) && idx + 1 < to_idx {
                        let (_, next_flags) = syntax_class_and_flags(
                            buf,
                            table,
                            chars[idx + 1],
                            accessible_char_start + idx + 1,
                            honor_properties,
                        );
                        if next_flags.contains(SyntaxFlags::COMMENT_END_SECOND)
                            && next_flags.contains(SyntaxFlags::COMMENT_STYLE_B) == style_b
                        {
                            let next_depth = comment_depth - 1;
                            idx += 2;
                            if next_depth <= 0 {
                                state.in_comment = None;
                                state.comment_or_string_start = None;
                            } else {
                                state.in_comment = Some(ParseCommentState::Syntax {
                                    depth: next_depth,
                                    style_b,
                                    nestable,
                                });
                            }
                            if commentstop == CommentStopMode::SyntaxTable {
                                break;
                            }
                            continue;
                        }
                    }

                    idx += 1;
                    continue;
                }
            }
        }

        if flags.contains(SyntaxFlags::COMMENT_START_FIRST) && idx + 1 < to_idx {
            let (_, next_flags) = syntax_class_and_flags(
                buf,
                table,
                chars[idx + 1],
                accessible_char_start + idx + 1,
                honor_properties,
            );
            if next_flags.contains(SyntaxFlags::COMMENT_START_SECOND) {
                state.in_comment = Some(ParseCommentState::Syntax {
                    depth: 1,
                    style_b: next_flags.contains(SyntaxFlags::COMMENT_STYLE_B),
                    nestable: flags.contains(SyntaxFlags::COMMENT_NESTABLE)
                        || next_flags.contains(SyntaxFlags::COMMENT_NESTABLE),
                });
                state.comment_or_string_start = Some(pos1);
                idx += 2;
                if commentstop != CommentStopMode::None {
                    break;
                }
                continue;
            }
        }

        match class {
            SyntaxClass::Open => {
                state.depth += 1;
                state.stack.push(pos1);
            }
            SyntaxClass::Close => {
                if state.depth > 0 {
                    state.depth -= 1;
                    state.mindepth = state.mindepth.min(state.depth);
                }
                if let Some(open_pos) = state.stack.pop() {
                    if state.depth == 0 {
                        state.completed_toplevel_list_start = Some(open_pos);
                    }
                }
            }
            SyntaxClass::StringDelim => {
                state.in_string = Some(ParseStringState::Delim(ch));
                state.comment_or_string_start = Some(pos1);
                idx += 1;
                if commentstop == CommentStopMode::SyntaxTable {
                    break;
                }
                continue;
            }
            SyntaxClass::StringFence => {
                state.in_string = Some(ParseStringState::Fence);
                state.comment_or_string_start = Some(pos1);
                idx += 1;
                if commentstop == CommentStopMode::SyntaxTable {
                    break;
                }
                continue;
            }
            SyntaxClass::Comment => {
                state.in_comment = Some(ParseCommentState::Syntax {
                    depth: 1,
                    style_b: flags.contains(SyntaxFlags::COMMENT_STYLE_B),
                    nestable: flags.contains(SyntaxFlags::COMMENT_NESTABLE),
                });
                state.comment_or_string_start = Some(pos1);
                idx += 1;
                if commentstop != CommentStopMode::None {
                    break;
                }
                continue;
            }
            SyntaxClass::Generic => {
                state.in_comment = Some(ParseCommentState::Fence { depth: 1 });
                state.comment_or_string_start = Some(pos1);
                idx += 1;
                if commentstop != CommentStopMode::None {
                    break;
                }
                continue;
            }
            SyntaxClass::Escape | SyntaxClass::CharQuote => {
                if idx + 1 < to_idx {
                    idx += 2;
                    continue;
                }
                state.quoted = true;
                idx += 1;
                continue;
            }
            SyntaxClass::Whitespace | SyntaxClass::EndComment => {}
            _ => {
                if state.last_sexp_start.is_none() {
                    state.last_sexp_start = Some(pos1);
                }
            }
        }

        idx += 1;
    }

    (state.into_value(), idx as i64 + 1)
}

fn parse_state_from_range(buf: &Buffer, table: &SyntaxTable, from: i64, to: i64) -> Value {
    parse_state_from_range_with_options(buf, table, from, to, None, CommentStopMode::None, false).0
}

/// `(parse-partial-sexp FROM TO &optional TARGETDEPTH STOPBEFORE STATE COMMENTSTOP)`
/// Baseline parser-state implementation for structural Lisp motion/state queries.
pub(crate) fn builtin_parse_partial_sexp(
    eval: &mut super::eval::Context,
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

    if to < from {
        return Err(signal(
            "error",
            vec![Value::string("End position is smaller than start position")],
        ));
    }

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = buf.syntax_table.clone();
    let oldstate = args.get(4).filter(|v| !v.is_nil());
    let commentstop = parse_commentstop_mode(args.get(5));
    let honor_properties = parse_sexp_lookup_properties_enabled(eval);
    let (state, stop_pos) = parse_state_from_range_with_options(
        buf,
        &table,
        from,
        to,
        oldstate,
        commentstop,
        honor_properties,
    );
    let stop_byte = lisp_pos_to_byte(buf, stop_pos);
    if let Some(buf_mut) = eval.buffers.current_buffer_mut() {
        buf_mut.goto_char(stop_byte);
    }
    Ok(state)
}

/// `(syntax-ppss &optional POS)` — parser state at POS.
pub(crate) fn builtin_syntax_ppss(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
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

    let honor_properties = parse_sexp_lookup_properties_enabled(eval);
    Ok(parse_state_from_range_with_options(
        buf,
        &table,
        1,
        pos,
        None,
        CommentStopMode::None,
        honor_properties,
    )
    .0)
}

/// `(syntax-ppss-flush-cache POS &rest _IGNORED)` — flush parser-state cache.
///
/// NeoVM currently computes parser state directly, so this is a no-op that
/// enforces Emacs-compatible arity/type behavior.
pub(crate) fn builtin_syntax_ppss_flush_cache(
    _eval: &mut super::eval::Context,
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

fn lisp_pos_to_byte(buf: &Buffer, raw: i64) -> usize {
    buf.lisp_pos_to_accessible_byte(raw)
}

/// `(skip-syntax-forward SYNTAX &optional LIMIT)` — skip forward over chars
/// matching the given syntax classes.
pub(crate) fn builtin_skip_syntax_forward(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let honor_properties = parse_sexp_lookup_properties_enabled(eval);
    builtin_skip_syntax_forward_in_buffers(&mut eval.buffers, args, honor_properties)
}

pub(crate) fn builtin_skip_syntax_forward_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
    honor_properties: bool,
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
            Value::Int(n) => Some(*n),
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

    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = buf.syntax_table.clone();
    let limit = limit.map(|raw| lisp_pos_to_byte(buf, raw));
    let new_pos =
        skip_syntax_forward_with_options(buf, &table, &syntax_chars, limit, honor_properties);

    let old_pt = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?
        .point();

    let current_id = buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = buffers.goto_buffer_byte(current_id, new_pos);

    // Return number of characters skipped (Emacs convention).
    let buf = buffers
        .get(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
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
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let honor_properties = parse_sexp_lookup_properties_enabled(eval);
    builtin_skip_syntax_backward_in_buffers(&mut eval.buffers, args, honor_properties)
}

pub(crate) fn builtin_skip_syntax_backward_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
    honor_properties: bool,
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
            Value::Int(n) => Some(*n),
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

    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = buf.syntax_table.clone();
    let limit = limit.map(|raw| lisp_pos_to_byte(buf, raw));
    let new_pos =
        skip_syntax_backward_with_options(buf, &table, &syntax_chars, limit, honor_properties);

    let old_pt = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?
        .point();

    let current_id = buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = buffers.goto_buffer_byte(current_id, new_pos);

    // Return negative number of characters skipped.
    let buf = buffers
        .get(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
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
