//! Syntax table system for the Elisp VM.
//!
//! Implements Emacs-compatible syntax tables with character classification,
//! motion functions (forward/backward word, sexp scanning), and the
//! `string-to-syntax` descriptor parser.

use std::cell::RefCell;
use std::collections::HashMap;

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::value::{RuntimeBindingValue, Value, ValueKind, VecLikeType, list_to_vec};
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

// Phase 10D holdout 3: the per-buffer syntax table char-table now lives in
// `Buffer::slots[BUFFER_SLOT_SYNTAX_TABLE]`, mirroring GNU's
// `BVAR(buf, syntax_table)` storage. Reads go through `slots[offset]`,
// writes go through `slots[offset]` plus `set_slot_local_flag` (matching
// `Fset_syntax_table`'s `SET_PER_BUFFER_VALUE_P`). The slot itself is
// non-Lisp-visible (`install_as_forwarder: false`), so the symbol
// `syntax-table` continues to signal void-variable as in GNU.

/// Pre-populate GNU Emacs syntax variables that are defined from C.
pub fn init_syntax_vars(
    obarray: &mut super::symbol::Obarray,
    _custom: &mut super::custom::CustomManager,
) {
    obarray.set_symbol_value("parse-sexp-ignore-comments", Value::NIL);
    obarray.set_symbol_value("parse-sexp-lookup-properties", Value::NIL);
    obarray.set_symbol_value("syntax-propertize--done", Value::fixnum(-1));
    obarray.set_symbol_value("words-include-escapes", Value::NIL);
    obarray.set_symbol_value("multibyte-syntax-as-symbol", Value::NIL);
    obarray.set_symbol_value("open-paren-in-column-0-is-defun-start", Value::T);
    obarray.set_symbol_value(
        "find-word-boundary-function-table",
        super::chartable::make_char_table_value(Value::NIL, Value::NIL),
    );
    obarray.set_symbol_value("comment-end-can-be-escaped", Value::NIL);
    obarray.set_symbol_value("forward-comment-function", Value::NIL);

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

    // Mirrors GNU `Fmake_variable_buffer_local` (`data.c:2142-2207`):
    // flip the redirect tag to LOCALIZED, allocate a BLV, set
    // local_if_set = 1. The legacy `obarray.make_buffer_local`
    // helper used to be called here too but it overwrites the
    // freshly-set LOCALIZED redirect back to PLAINVAL and
    // orphans the BLV.
    for name in ["syntax-propertize--done", "comment-end-can-be-escaped"] {
        let id = crate::emacs_core::intern::intern(name);
        let default = obarray
            .find_symbol_value(id)
            .unwrap_or(crate::emacs_core::value::Value::NIL);
        obarray.make_symbol_localized(id, default);
        obarray.set_blv_local_if_set(id, true);
    }
}

// ===========================================================================
// Syntax classes
// ===========================================================================

/// Emacs syntax classes, matching GNU's `enum syntaxcode` from `syntax.h`.
///
/// Discriminant values match the GNU numbering (0–15) so the enum can be
/// cast to `u8` and used directly in bytecode (e.g. the regex engine's
/// `SyntaxSpec` / `NotSyntaxSpec` opcodes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SyntaxClass {
    /// ' ' — Whitespace (Swhitespace = 0)
    Whitespace = 0,
    /// '.' — Punctuation (Spunct = 1)
    Punctuation = 1,
    /// 'w' — Word constituent (Sword = 2)
    Word = 2,
    /// '_' — Symbol constituent (Ssymbol = 3)
    Symbol = 3,
    /// '(' — Open parenthesis/bracket (Sopen = 4)
    Open = 4,
    /// ')' — Close parenthesis/bracket (Sclose = 5)
    Close = 5,
    /// '\'' — Expression prefix (Squote = 6)
    Quote = 6,
    /// '"' — String delimiter (Sstring = 7)
    StringDelim = 7,
    /// '$' — Math delimiter, paired (Smath = 8)
    Math = 8,
    /// '\\' — Escape character (Sescape = 9)
    Escape = 9,
    /// '/' — Character quote, only quotes the next character (Scharquote = 10)
    CharQuote = 10,
    /// '<' — Comment starter (Scomment = 11)
    Comment = 11,
    /// '>' — Comment ender (Sendcomment = 12)
    EndComment = 12,
    /// '@' — Inherit from standard syntax table (Sinherit = 13)
    InheritStd = 13,
    /// '!' — Generic comment delimiter / comment fence (Scomment_fence = 14)
    CommentFence = 14,
    /// '|' — Generic string fence (Sstring_fence = 15)
    StringFence = 15,
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
            '\'' => Some(SyntaxClass::Quote),
            '"' => Some(SyntaxClass::StringDelim),
            '$' => Some(SyntaxClass::Math),
            '\\' => Some(SyntaxClass::Escape),
            '/' => Some(SyntaxClass::CharQuote),
            '<' => Some(SyntaxClass::Comment),
            '>' => Some(SyntaxClass::EndComment),
            '@' => Some(SyntaxClass::InheritStd),
            '!' => Some(SyntaxClass::CommentFence),
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
            SyntaxClass::Quote => '\'',
            SyntaxClass::StringDelim => '"',
            SyntaxClass::Math => '$',
            SyntaxClass::Escape => '\\',
            SyntaxClass::CharQuote => '/',
            SyntaxClass::Comment => '<',
            SyntaxClass::EndComment => '>',
            SyntaxClass::InheritStd => '@',
            SyntaxClass::CommentFence => '!',
            SyntaxClass::StringFence => '|',
        }
    }

    /// Return the integer code Emacs uses for this syntax class
    /// (used in the cons cell returned by `string-to-syntax`).
    #[inline]
    pub fn code(self) -> i64 {
        self as i64
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
            6 => Some(SyntaxClass::Quote),
            7 => Some(SyntaxClass::StringDelim),
            8 => Some(SyntaxClass::Math),
            9 => Some(SyntaxClass::Escape),
            10 => Some(SyntaxClass::CharQuote),
            11 => Some(SyntaxClass::Comment),
            12 => Some(SyntaxClass::EndComment),
            13 => Some(SyntaxClass::InheritStd),
            14 => Some(SyntaxClass::CommentFence),
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

fn syntax_runtime_string(value: &Value) -> Result<String, Flow> {
    value.as_runtime_string_owned().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *value],
        )
    })
}

/// Convert a `SyntaxEntry` into the Emacs cons-cell representation
/// returned by `string-to-syntax`: `(CODE . MATCHING-CHAR-OR-NIL)`.
///
/// The CODE is computed as: `(class_code) | (flags << 16)`.
pub fn syntax_entry_to_value(entry: &SyntaxEntry) -> Value {
    let code = entry.class.code() | ((entry.flags.bits() as i64) << 16);
    let matching = match entry.matching_char {
        Some(ch) => Value::fixnum(ch as i64),
        None => Value::NIL,
    };
    Value::cons(Value::fixnum(code), matching)
}

// ===========================================================================
// SyntaxTable
// ===========================================================================

/// An Emacs-style syntax table mapping characters to syntax entries.
///
/// Characters not explicitly set fall back to a parent table (if present)
/// or to the built-in standard defaults.
/// A Lisp-level syntax table: a thin wrapper around the chartable `Value`
/// stored in `buffer->syntax_table` / `buf.slots[BUFFER_SLOT_SYNTAX_TABLE]`.
///
/// Mirrors GNU Emacs design: the chartable IS the runtime form. All
/// queries go through `CHAR_TABLE_REF(table, c)` (→ our
/// `syntax_{class,entry}_at_char`) on demand; no eagerly-compiled HashMap
/// shadow form is maintained.
///
/// The inner `Value` is `Value::NIL` in two situations:
/// (1) a freshly-constructed `SyntaxTable::new_standard()` before the
///     standard chartable is materialized by the evaluator, and
/// (2) pdump's placeholder before `sync_current_buffer_syntax_table_state`
///     re-attaches the live chartable from the buffer slot.
/// In both cases `char_syntax()` falls back to GNU's default (Word for
/// >= U+0080, Whitespace for < U+0080), matching `SYNTAX_ENTRY`'s nil
/// handling.
#[derive(Clone, Copy, Debug)]
pub struct SyntaxTable {
    chartable: Value,
}

impl SyntaxTable {
    // -- Construction --------------------------------------------------------

    /// Return a `SyntaxTable` backed by the standard chartable Value.
    /// Materializes the chartable on first call via
    /// `ensure_standard_syntax_table_object()` — the same one installed
    /// on new buffers by `current_buffer_syntax_table_object_in_buffers`.
    pub fn new_standard() -> Self {
        match ensure_standard_syntax_table_object() {
            Ok(table) => Self { chartable: table },
            // If we can't build the chartable (no thread-local state),
            // return a nil-backed placeholder — callers fall back to
            // GNU defaults via `char_syntax` / `get_entry`.
            Err(_) => Self {
                chartable: Value::NIL,
            },
        }
    }

    /// Same as `new_standard` — GNU's `make-syntax-table` with nil parent
    /// creates a fresh, empty chartable whose parent is the standard
    /// table. The distinction is handled at the chartable level by
    /// `builtin_make_syntax_table`.
    pub fn make_syntax_table() -> Self {
        Self::new_standard()
    }

    /// Build a `SyntaxTable` that reads from the given chartable `Value`.
    pub(crate) fn from_chartable(chartable: Value) -> Self {
        Self { chartable }
    }

    /// Build a `SyntaxTable` that reads directly from `buf`'s
    /// syntax-table slot. Mirrors GNU `BVAR (buf, syntax_table)`.
    /// Falls back to a nil-backed placeholder (GNU defaults) if the
    /// slot hasn't been seeded yet.
    pub fn for_buffer(buf: &crate::buffer::buffer::Buffer) -> Self {
        Self {
            chartable: buf.syntax_chartable(),
        }
    }

    /// Install an isolated copy of the standard chartable on `buf` so
    /// subsequent `modify_syntax_entry` calls don't leak into the
    /// shared standard. Returns the new `SyntaxTable`. Mirrors the
    /// GNU idiom `(set-syntax-table (copy-syntax-table))`.
    pub fn isolate_for_buffer(buf: &mut crate::buffer::buffer::Buffer) -> Self {
        use crate::buffer::buffer::BUFFER_SLOT_SYNTAX_TABLE;
        let slot = buf.slots[BUFFER_SLOT_SYNTAX_TABLE];
        let source = if slot.is_nil() {
            ensure_standard_syntax_table_object().unwrap_or(Value::NIL)
        } else {
            slot
        };
        let own = if source.is_nil() {
            Value::NIL
        } else {
            builtin_copy_syntax_table(vec![source]).unwrap_or(source)
        };
        buf.slots[BUFFER_SLOT_SYNTAX_TABLE] = own;
        Self { chartable: own }
    }

    /// Deep-copy the backing chartable, matching GNU `copy-syntax-table`
    /// (`syntax.c:265-282`). The copy is independent: mutations to
    /// either table do not affect the other.
    pub fn copy_syntax_table(&self) -> Self {
        if self.chartable.is_nil() {
            return self.clone();
        }
        match builtin_copy_syntax_table(vec![self.chartable]) {
            Ok(copy) => Self { chartable: copy },
            Err(_) => self.clone(),
        }
    }

    /// Return the chartable Value backing this table (may be `NIL` for
    /// a placeholder table — see type-level docs).
    pub(crate) fn chartable(&self) -> Value {
        self.chartable
    }

    // -- Queries -------------------------------------------------------------

    /// Return the syntax entry for `ch`, matching GNU
    /// `SYNTAX_ENTRY(c)`. Falls back to the standard chartable when
    /// the wrapper is nil-backed (handled by `syntax_entry_at_char`).
    pub fn get_entry(&self, ch: char) -> Option<SyntaxEntry> {
        syntax_entry_at_char(&self.chartable, ch)
    }

    /// Return the syntax class for `ch` — GNU `SYNTAX(c)`.
    pub fn char_syntax(&self, ch: char) -> SyntaxClass {
        syntax_class_at_char(&self.chartable, ch)
    }

    // -- Mutation -------------------------------------------------------------

    /// Install `entry` for `ch` in the backing chartable. No-op when
    /// the table is a `NIL` placeholder — the evaluator's
    /// `modify-syntax-entry` builtin routes through the chartable
    /// directly for that case.
    pub fn modify_syntax_entry(&mut self, ch: char, entry: SyntaxEntry) {
        if self.chartable.is_nil() {
            return;
        }
        let _ = super::chartable::builtin_set_char_table_range(vec![
            self.chartable,
            Value::fixnum(ch as i64),
            syntax_entry_to_value(&entry),
        ]);
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

fn syntax_char_from_code(code: u32) -> char {
    super::builtins::character_code_to_rust_char(code as i64).unwrap_or('\u{FFFD}')
}

fn buffer_chars_in_range(buf: &Buffer, start: usize, end: usize) -> Vec<char> {
    let string = buf.buffer_substring_lisp_string(start, end);
    crate::emacs_core::builtins::lisp_string_char_codes(&string)
        .into_iter()
        .map(syntax_char_from_code)
        .collect()
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

    let chars = buffer_chars_in_range(buf, buf.point_min(), buf.point_max());
    // Convert byte pos to char index within accessible region.
    let base = buf.point_min();
    let rel_byte = buf.point().saturating_sub(base);
    let mut idx = buf.text.emacs_byte_to_char(base + rel_byte) - buf.text.emacs_byte_to_char(base);

    let accessible_char_start = buf.text.emacs_byte_to_char(base);
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
    buf.text.char_to_emacs_byte(abs_char)
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

    let chars = buffer_chars_in_range(buf, buf.point_min(), buf.point_max());
    let base = buf.point_min();
    let rel_byte = buf.point().saturating_sub(base);
    let mut idx = buf.text.emacs_byte_to_char(base + rel_byte) - buf.text.emacs_byte_to_char(base);
    let accessible_char_start = buf.text.emacs_byte_to_char(base);

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
    buf.text.char_to_emacs_byte(abs_char)
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

    let chars = buffer_chars_in_range(buf, buf.point_min(), buf.point_max());
    let base = buf.point_min();
    let rel_byte = buf.point().saturating_sub(base);
    let mut idx = buf.text.emacs_byte_to_char(base + rel_byte) - buf.text.emacs_byte_to_char(base);

    let accessible_char_start = buf.text.emacs_byte_to_char(base);
    let accessible_char_end = buf.point_max_char();
    let accessible_len = accessible_char_end - accessible_char_start;

    let char_limit = limit
        .map(|lim| {
            let lim_clamped = lim.min(buf.point_max());
            buf.text.emacs_byte_to_char(lim_clamped) - accessible_char_start
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
    buf.text.char_to_emacs_byte(abs_char)
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

    let chars = buffer_chars_in_range(buf, buf.point_min(), buf.point_max());
    let base = buf.point_min();
    let rel_byte = buf.point().saturating_sub(base);
    let mut idx = buf.text.emacs_byte_to_char(base + rel_byte) - buf.text.emacs_byte_to_char(base);

    let accessible_char_start = buf.text.emacs_byte_to_char(base);

    let char_limit = limit
        .map(|lim| {
            let lim_clamped = lim.max(base);
            buf.text.emacs_byte_to_char(lim_clamped) - accessible_char_start
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
    buf.text.char_to_emacs_byte(abs_char)
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

    let chars = buffer_chars_in_range(buf, 0, buf.total_bytes());
    let total_chars = chars.len();

    // Convert byte position to char index.
    let mut idx = buf.text.emacs_byte_to_char(from);

    if count > 0 {
        for _ in 0..count {
            idx = scan_sexp_forward(buf, &chars, total_chars, idx, table, honor_properties)?;
        }
    } else {
        for _ in 0..(-count) {
            idx = scan_sexp_backward(buf, &chars, idx, table, honor_properties)?;
        }
    }

    Ok(buf.text.char_to_emacs_byte(idx))
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
                | SyntaxClass::Quote
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
        SyntaxClass::Math => {
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
                | SyntaxClass::Quote
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
        SyntaxClass::Math => {
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
                Value::fixnum(args.len() as i64),
            ],
        ));
    }
    let s = syntax_runtime_string(&args[0])?;
    let entry = string_to_syntax(&s).map_err(|msg| signal("error", vec![Value::string(&msg)]))?;
    if matches!(entry.class, SyntaxClass::InheritStd) {
        return Ok(Value::NIL);
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
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    let table = super::chartable::make_char_table_value(Value::symbol("syntax-table"), Value::NIL);
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
                Value::fixnum(args.len() as i64),
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

    match source.kind() {
        ValueKind::Veclike(VecLikeType::Vector) => {
            let copy = Value::vector(source.as_vector_data().unwrap().clone());
            super::chartable::builtin_set_char_table_range(vec![copy, Value::NIL, Value::NIL])?;
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
            vec![Value::symbol("syntax-table-p"), source],
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
                Value::fixnum(cp),
                punctuation,
            ])?;
        }
        super::chartable::builtin_set_char_table_range(vec![
            table,
            Value::fixnum(0x7f),
            punctuation,
        ])?;

        // Standard ASCII defaults — matches GNU `Fset_standard_syntax_table`
        // in `syntax.c:3476-3557`. Word: letters, digits, $ %;
        // Open/Close: paren/bracket/brace pairs with matching chars;
        // StringDelim: "; Escape: \; Symbol: _ - + * / & | < > =;
        // Punctuation: . , ; : ? ! # @ ~ ^ ' `.
        let set = |ch: char, e: SyntaxEntry| -> Result<(), Flow> {
            super::chartable::builtin_set_char_table_range(vec![
                table,
                Value::fixnum(ch as i64),
                syntax_entry_to_value(&e),
            ])
            .map(|_| ())
        };
        for ch in [' ', '\t', '\n', '\r', '\u{000c}'] {
            set(ch, SyntaxEntry::simple(SyntaxClass::Whitespace))?;
        }
        for ch in 'a'..='z' {
            set(ch, SyntaxEntry::simple(SyntaxClass::Word))?;
        }
        for ch in 'A'..='Z' {
            set(ch, SyntaxEntry::simple(SyntaxClass::Word))?;
        }
        for ch in '0'..='9' {
            set(ch, SyntaxEntry::simple(SyntaxClass::Word))?;
        }
        set('$', SyntaxEntry::simple(SyntaxClass::Word))?;
        set('%', SyntaxEntry::simple(SyntaxClass::Word))?;
        set('(', SyntaxEntry::with_match(SyntaxClass::Open, ')'))?;
        set(')', SyntaxEntry::with_match(SyntaxClass::Close, '('))?;
        set('[', SyntaxEntry::with_match(SyntaxClass::Open, ']'))?;
        set(']', SyntaxEntry::with_match(SyntaxClass::Close, '['))?;
        set('{', SyntaxEntry::with_match(SyntaxClass::Open, '}'))?;
        set('}', SyntaxEntry::with_match(SyntaxClass::Close, '{'))?;
        set('"', SyntaxEntry::simple(SyntaxClass::StringDelim))?;
        set('\\', SyntaxEntry::simple(SyntaxClass::Escape))?;
        for ch in ['_', '-', '+', '*', '/', '&', '|', '<', '>', '='] {
            set(ch, SyntaxEntry::simple(SyntaxClass::Symbol))?;
        }
        for ch in ['.', ',', ';', ':', '?', '!', '#', '@', '~', '^', '\'', '`'] {
            set(ch, SyntaxEntry::simple(SyntaxClass::Punctuation))?;
        }
        super::chartable::builtin_set_char_table_range(vec![
            table,
            Value::cons(Value::fixnum(0x80), Value::fixnum(0x3F_FFFF)),
            word,
        ])?;
        *slot.borrow_mut() = Some(table);
        Ok(table)
    })
}

fn current_buffer_syntax_table_object_in_buffers(
    buffers: &mut BufferManager,
) -> Result<Value, Flow> {
    use crate::buffer::buffer::BUFFER_SLOT_SYNTAX_TABLE;
    let fallback = ensure_standard_syntax_table_object()?;
    let current_id = buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = buffers
        .get_mut(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    // Mirrors GNU `Fsyntax_table` (`syntax.c:987-993`):
    //     return BVAR (current_buffer, syntax_table);
    let value = buf.slots[BUFFER_SLOT_SYNTAX_TABLE];
    if !value.is_nil() && builtin_syntax_table_p(vec![value])?.is_truthy() {
        return Ok(value);
    }

    // Slot is unset (fresh buffer or never assigned). Seed it
    // from the standard syntax table — matches GNU's
    // `reset_buffer` (`buffer.c:1149-1157`) which copies the
    // standard tables into a fresh buffer.
    buf.slots[BUFFER_SLOT_SYNTAX_TABLE] = fallback;
    Ok(fallback)
}

fn current_buffer_syntax_table_object(eval: &mut super::eval::Context) -> Result<Value, Flow> {
    current_buffer_syntax_table_object_in_buffers(&mut eval.buffers)
}

pub(crate) fn sync_current_buffer_syntax_table_state(
    ctx: &mut super::eval::Context,
) -> Result<(), Flow> {
    // Just ensure the slot is seeded with the standard chartable if
    // it was left `Value::NIL`. No compilation, no cache rebuild —
    // motion/parse code reads `buf.slots[BUFFER_SLOT_SYNTAX_TABLE]`
    // directly via `SyntaxTable::for_buffer`. Matches GNU
    // `set_buffer_internal`.
    let _ = current_buffer_syntax_table_object_in_buffers(&mut ctx.buffers)?;
    Ok(())
}

fn set_current_buffer_syntax_table_object_in_buffers(
    buffers: &mut BufferManager,
    table: Value,
) -> Result<(), Flow> {
    use crate::buffer::buffer::BUFFER_SLOT_SYNTAX_TABLE;
    let current_id = buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = buffers
        .get_mut(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    // Mirrors GNU `Fset_syntax_table` (`syntax.c:1030-1042`):
    //     bset_syntax_table (current_buffer, table);
    //     SET_PER_BUFFER_VALUE_P (current_buffer,
    //                             PER_BUFFER_VAR_IDX (syntax_table), 1);
    buf.slots[BUFFER_SLOT_SYNTAX_TABLE] = table;
    buf.set_slot_local_flag(BUFFER_SLOT_SYNTAX_TABLE, true);
    Ok(())
}

fn set_current_buffer_syntax_table_object(
    eval: &mut super::eval::Context,
    table: Value,
) -> Result<(), Flow> {
    set_current_buffer_syntax_table_object_in_buffers(&mut eval.buffers, table)
}

/// Read the `SyntaxEntry` for character `c` from the chartable `table`.
///
/// Mirrors GNU Emacs `SYNTAX_ENTRY(c)` in `src/syntax.h`:
///
/// ```c
/// #define SYNTAX_ENTRY(c) \
///   char_table_ref (BVAR (current_buffer, syntax_table), c)
/// ```
///
/// When `table` is `Value::NIL` (an un-seeded buffer slot or a
/// placeholder wrapper), falls back to the evaluator's
/// standard-syntax-table chartable. This mirrors GNU `reset_buffer`,
/// which copies `Vstandard_syntax_table` into every fresh
/// `buffer->syntax_table` — so from the reader's point of view a
/// "never-set" slot always behaves like the standard.
pub(crate) fn syntax_entry_at_char(table: &Value, c: char) -> Option<SyntaxEntry> {
    let effective = if table.is_nil() {
        ensure_standard_syntax_table_object().unwrap_or(Value::NIL)
    } else {
        *table
    };
    if effective.is_nil() {
        return None;
    }
    let entry = super::chartable::ct_lookup(&effective, c as i64).ok()?;
    syntax_entry_from_chartable_entry(&entry)
}

/// Return the `SyntaxClass` for `c` under `table`, mirroring GNU
/// `SYNTAX(c)` in `src/syntax.h`. Uses the same fallback as
/// `SyntaxTable::char_syntax` on the old compiled form: codepoints
/// >= 0x80 default to Word; below 0x80 default to Whitespace.
pub(crate) fn syntax_class_at_char(table: &Value, c: char) -> SyntaxClass {
    match syntax_entry_at_char(table, c) {
        Some(entry) => entry.class,
        None => {
            if u32::from(c) >= 0x80 {
                SyntaxClass::Word
            } else {
                SyntaxClass::Whitespace
            }
        }
    }
}

fn syntax_entry_from_chartable_entry(entry: &Value) -> Option<SyntaxEntry> {
    match entry.kind() {
        ValueKind::Nil => None,
        ValueKind::Cons => {
            let pair_car = entry.cons_car();
            let pair_cdr = entry.cons_cdr();
            let code = match pair_car.kind() {
                ValueKind::Fixnum(code) => code,
                _ => return None,
            };
            let class = SyntaxClass::from_code(code)?;
            let matching_char = match pair_cdr.kind() {
                ValueKind::Fixnum(n) => char::from_u32(n as u32),
                ValueKind::Nil => None,
                _ => None,
            };
            Some(SyntaxEntry {
                class,
                matching_char,
                flags: SyntaxFlags::new(((code >> 16) & 0xFF) as u8),
            })
        }
        ValueKind::Fixnum(code) => Some(SyntaxEntry {
            class: SyntaxClass::from_code(code)?,
            matching_char: None,
            flags: SyntaxFlags::new(((code >> 16) & 0xFF) as u8),
        }),
        _ => None,
    }
}

fn syntax_table_from_chartable(table: Value) -> Result<SyntaxTable, Flow> {
    if builtin_syntax_table_p(vec![table])?.is_nil() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("syntax-table-p"), table],
        ));
    }
    // GNU parity: the chartable IS the runtime form. Just wrap it.
    Ok(SyntaxTable::from_chartable(table))
}

fn syntax_entry_from_syntax_property(prop: Value, ch: char) -> Option<SyntaxEntry> {
    if builtin_syntax_table_p(vec![prop]).ok()?.is_truthy() {
        let raw = super::chartable::builtin_char_table_range(vec![prop, Value::fixnum(ch as i64)])
            .ok()?;
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
        && let Some(prop) = buf
            .text
            .text_props_get_property(byte_pos, Value::symbol("syntax-table"))
        && let Some(entry) = syntax_entry_from_syntax_property(prop, ch)
    {
        return entry;
    }

    table
        .get_entry(ch)
        .unwrap_or_else(|| SyntaxEntry::simple(table.char_syntax(ch)))
}

fn effective_syntax_entry_for_abs_char(
    buf: &Buffer,
    table: &SyntaxTable,
    ch: char,
    abs_char: usize,
    honor_properties: bool,
) -> SyntaxEntry {
    let byte_pos = buf.text.char_to_emacs_byte(abs_char);
    effective_syntax_entry_for_char_at_byte(buf, table, ch, byte_pos, honor_properties)
}

fn parse_sexp_lookup_properties_enabled(ctx: &super::eval::Context) -> bool {
    ctx.obarray
        .symbol_value("parse-sexp-lookup-properties")
        .copied()
        .unwrap_or(Value::NIL)
        .is_truthy()
}

/// `(syntax-class-to-char CLASS)` — map syntax class code to descriptor char.
pub(crate) fn builtin_syntax_class_to_char(args: Vec<Value>) -> EvalResult {
    if args.len() != 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("syntax-class-to-char"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    let class = match args[0].kind() {
        ValueKind::Fixnum(n) => n,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("fixnump"), args[0]],
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
        _ => {
            return Err(signal(
                "args-out-of-range",
                vec![Value::fixnum(15), Value::fixnum(class)],
            ));
        }
    };

    Ok(Value::char(ch))
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
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    let ch = match args[0].kind() {
        ValueKind::Fixnum(n) => char::from_u32(n as u32).ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), args[0]],
            )
        })?,
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), args[0]],
            ));
        }
    };

    // Look up in the current buffer's syntax table
    if let Some(buf) = buffers.current_buffer() {
        let entry = SyntaxTable::for_buffer(buf).get_entry(ch);
        if let Some(e) = entry {
            if matches!(e.class, SyntaxClass::Open | SyntaxClass::Close) {
                if let Some(m) = e.matching_char {
                    return Ok(Value::char(m));
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
    Ok(out.map_or(Value::NIL, Value::char))
}

/// `(standard-syntax-table)` — return the standard syntax table.
pub(crate) fn builtin_standard_syntax_table(args: Vec<Value>) -> EvalResult {
    if !args.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("standard-syntax-table"),
                Value::fixnum(args.len() as i64),
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
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    let is_char_table = super::chartable::builtin_char_table_p(vec![args[0]])?;
    if !is_char_table.is_truthy() {
        return Ok(Value::NIL);
    }

    let subtype = super::chartable::builtin_char_table_subtype(vec![args[0]])?;
    match subtype.kind() {
        ValueKind::Symbol(id) if resolve_sym(id) == "syntax-table" => Ok(Value::T),
        _ => Ok(Value::NIL),
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
            vec![
                Value::symbol("syntax-table"),
                Value::fixnum(args.len() as i64),
            ],
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
                Value::fixnum(args.len() as i64),
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
    // Matches GNU `Fset_syntax_table` — just bset_syntax_table on the
    // slot. Motion code reads it live via `SyntaxTable::for_buffer`.
    set_current_buffer_syntax_table_object_in_buffers(buffers, table)?;
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
                Value::fixnum(args.len() as i64),
            ],
        ));
    }
    let descriptor = syntax_runtime_string(&args[1])?;
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
    let chartable_entry = if matches!(entry.class, SyntaxClass::InheritStd) {
        Value::NIL
    } else {
        syntax_entry_to_value(&entry)
    };
    super::chartable::builtin_set_char_table_range(vec![target_table, args[0], chartable_entry])?;

    if !update_current_buffer_table {
        return Ok(Value::NIL);
    }
    // Current buffer's slot already points at `target_table` (it's the
    // same chartable we just mutated above via set-char-table-range).
    // No compiled form to refresh — motion reads the chartable live.
    let _ = target_table;
    Ok(Value::NIL)
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
            vec![
                Value::symbol("char-syntax"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }
    let ch = match args[0].kind() {
        ValueKind::Fixnum(c) => {
            super::builtins::character_code_to_rust_char(c).ok_or_else(|| {
                signal(
                    "error",
                    vec![Value::string("Invalid character code"), args[0]],
                )
            })?
        }
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), args[0]],
            ));
        }
    };

    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let class = SyntaxTable::for_buffer(buf).char_syntax(ch);
    Ok(Value::char(class.to_char()))
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
            vec![
                Value::symbol("syntax-after"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    let pos = match args[0].kind() {
        ValueKind::Fixnum(n) => n,
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("number-or-marker-p"), args[0]],
            ));
        }
    };
    if pos <= 0 {
        return Ok(Value::NIL);
    }

    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    let char_index = pos as usize - 1;
    let byte_index = buf
        .text
        .char_to_emacs_byte(char_index.min(buf.text.char_count()));
    let Some(ch) = buf.char_after(byte_index) else {
        return Ok(Value::NIL);
    };

    let entry = effective_syntax_entry_for_char_at_byte(
        buf,
        &SyntaxTable::for_buffer(buf),
        ch,
        byte_index,
        true,
    );
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
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    let count = match args[0].kind() {
        ValueKind::Fixnum(n) => n,
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("integerp"), args[0]],
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
        return Ok(Value::T);
    }

    if count > 0 {
        let ok = forward_comment_forward(buf, count as u64, honor_properties);
        return Ok(if ok { Value::T } else { Value::NIL });
    } else {
        let ok = forward_comment_backward(buf, (-count) as u64, honor_properties);
        return Ok(if ok { Value::T } else { Value::NIL });
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
                &SyntaxTable::for_buffer(buf),
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
            &SyntaxTable::for_buffer(buf),
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
        if class == SyntaxClass::CommentFence {
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
                        &SyntaxTable::for_buffer(buf),
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
            &SyntaxTable::for_buffer(buf),
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
                            &SyntaxTable::for_buffer(buf),
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
        if class == SyntaxClass::CommentFence {
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
                        &SyntaxTable::for_buffer(buf),
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
            &SyntaxTable::for_buffer(buf),
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

        if class == SyntaxClass::CommentFence {
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
                &SyntaxTable::for_buffer(buf),
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
                            &SyntaxTable::for_buffer(buf),
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
            if code == SyntaxClass::CommentFence {
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
            &SyntaxTable::for_buffer(buf),
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
                        &SyntaxTable::for_buffer(buf),
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
        if class == SyntaxClass::CommentFence {
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
                        &SyntaxTable::for_buffer(buf),
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
            &SyntaxTable::for_buffer(buf),
            ch,
            ch_pos,
            honor_properties,
        );
        let class = entry.class;

        buf.goto_char(pt - ch.len_utf8());

        if class == SyntaxClass::CommentFence {
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
                Value::fixnum(args.len() as i64),
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
            &SyntaxTable::for_buffer(buf),
            ch,
            ch_pos,
            honor_properties,
        );
        let is_prefix =
            entry.class == SyntaxClass::Quote || entry.flags.contains(SyntaxFlags::PREFIX);
        if !is_prefix {
            break;
        }
        buf.goto_char(pt.saturating_sub(ch.len_utf8()));
    }

    Ok(Value::NIL)
}

/// `(forward-word &optional COUNT)` — move point forward COUNT words.
pub(crate) fn builtin_forward_word(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let count = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        match args[0].kind() {
            ValueKind::Fixnum(n) => n,
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), args[0]],
                ));
            }
        }
    };

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = SyntaxTable::for_buffer(buf);
    let honor_properties = parse_sexp_lookup_properties_enabled(eval);
    let new_pos = forward_word_with_options(buf, &table, count, honor_properties);

    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = eval.buffers.goto_buffer_byte(current_id, new_pos);
    Ok(Value::NIL)
}

pub(crate) fn builtin_forward_word_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    let count = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        match args[0].kind() {
            ValueKind::Fixnum(n) => n,
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), args[0]],
                ));
            }
        }
    };

    // We need to read the syntax table first, then call forward_word, then write point.
    // To satisfy the borrow checker, clone the syntax table.
    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = SyntaxTable::for_buffer(buf);
    let new_pos = forward_word(buf, &table, count);

    let current_id = buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = buffers.goto_buffer_byte(current_id, new_pos);
    Ok(Value::NIL)
}

/// `(backward-word &optional COUNT)` — move point backward COUNT words.
pub(crate) fn builtin_backward_word(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let count = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        match args[0].kind() {
            ValueKind::Fixnum(n) => n,
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), args[0]],
                ));
            }
        }
    };

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = SyntaxTable::for_buffer(buf);
    let honor_properties = parse_sexp_lookup_properties_enabled(eval);
    let new_pos = backward_word_with_options(buf, &table, count, honor_properties);

    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = eval.buffers.goto_buffer_byte(current_id, new_pos);
    Ok(Value::NIL)
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
        match args[0].kind() {
            ValueKind::Fixnum(n) => n,
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), args[0]],
                ));
            }
        }
    };

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = SyntaxTable::for_buffer(buf);
    let from = buf.point();
    let honor_properties = parse_sexp_lookup_properties_enabled(eval);
    let new_pos = scan_sexps_with_options(buf, &table, from, count, honor_properties)
        .map_err(|msg| signal("scan-error", vec![Value::string(&msg)]))?;

    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = eval.buffers.goto_buffer_byte(current_id, new_pos);
    Ok(Value::NIL)
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
        match args[0].kind() {
            ValueKind::Fixnum(n) => n,
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), args[0]],
                ));
            }
        }
    };

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = SyntaxTable::for_buffer(buf);
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
    Ok(Value::NIL)
}

/// `(scan-lists FROM COUNT DEPTH)` — scan across balanced expressions.
///
/// This uses the same core scanner as `forward-sexp`/`backward-sexp`.
pub(crate) fn builtin_scan_lists(ctx: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    if args.len() != 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("scan-lists"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    let from = match args[0].kind() {
        ValueKind::Fixnum(n) => n,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("integer-or-marker-p"), args[0]],
            ));
        }
    };
    let count = match args[1].kind() {
        ValueKind::Fixnum(n) => n,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("integerp"), args[1]],
            ));
        }
    };
    let _depth = match args[2].kind() {
        ValueKind::Fixnum(n) => n,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("integerp"), args[2]],
            ));
        }
    };

    let buf = ctx
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = SyntaxTable::for_buffer(buf);

    let from_char = if from > 0 { from as usize - 1 } else { 0 };
    let from_byte = buf
        .text
        .char_to_emacs_byte(from_char.min(buf.text.char_count()));

    let honor_properties = parse_sexp_lookup_properties_enabled(ctx);
    match scan_sexps_with_options(buf, &table, from_byte, count, honor_properties) {
        Ok(new_byte) => Ok(Value::fixnum(
            buf.text.emacs_byte_to_char(new_byte) as i64 + 1,
        )),
        Err(_) if count < 0 => Ok(Value::NIL),
        Err(msg) => Err(signal("scan-error", vec![Value::string(&msg)])),
    }
}

/// `(scan-sexps FROM COUNT)` — scan over COUNT sexps from FROM.
pub(crate) fn builtin_scan_sexps(ctx: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    if args.len() != 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("scan-sexps"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    let from = match args[0].kind() {
        ValueKind::Fixnum(n) => n,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("number-or-marker-p"), args[0]],
            ));
        }
    };
    let count = match args[1].kind() {
        ValueKind::Fixnum(n) => n,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("integerp"), args[1]],
            ));
        }
    };

    let buf = ctx
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = SyntaxTable::for_buffer(buf);

    let from_char = if from > 0 { from as usize - 1 } else { 0 };
    let from_byte = buf
        .text
        .char_to_emacs_byte(from_char.min(buf.text.char_count()));

    let honor_properties = parse_sexp_lookup_properties_enabled(ctx);
    match scan_sexps_with_options(buf, &table, from_byte, count, honor_properties) {
        Ok(new_byte) => Ok(Value::fixnum(
            buf.text.emacs_byte_to_char(new_byte) as i64 + 1,
        )),
        Err(_) if count < 0 => Ok(Value::NIL),
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

        state.depth = items.first().and_then(|v| v.as_fixnum()).unwrap_or(0);

        if let Some(start) = items.get(8).and_then(|v| v.as_fixnum()) {
            state.comment_or_string_start = Some(start);
        }

        if let Some(v) = items.get(5) {
            if v.is_t() {
                state.quoted = true;
            }
        }

        if let Some(item) = items.get(3) {
            state.in_string = match item.kind() {
                ValueKind::Nil => None,
                ValueKind::T => Some(ParseStringState::Fence),
                ValueKind::Fixnum(n) => u32::try_from(n)
                    .ok()
                    .and_then(char::from_u32)
                    .map(ParseStringState::Delim),
                _ => None,
            };
        }

        if let Some(item) = items.get(4) {
            state.in_comment = match item.kind() {
                ValueKind::Nil => None,
                ValueKind::T => Some(ParseCommentState::Syntax {
                    depth: 1,
                    style_b: false,
                    nestable: false,
                }),
                ValueKind::Fixnum(n) => Some(ParseCommentState::Syntax {
                    depth: n,
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
                .filter_map(|v| v.as_fixnum())
                .collect();
        }

        state
    }

    fn into_value(self) -> Value {
        let stack_value = if self.depth > 0 {
            Value::list(self.stack.iter().map(|p| Value::fixnum(*p)).collect())
        } else {
            Value::NIL
        };

        let string_value = match self.in_string {
            Some(ParseStringState::Delim(term)) => Value::fixnum(term as i64),
            Some(ParseStringState::Fence) => Value::T,
            None => Value::NIL,
        };

        let comment_value = match self.in_comment {
            Some(ParseCommentState::Syntax {
                depth: comment_depth,
                nestable: false,
                ..
            }) => {
                debug_assert_eq!(comment_depth, 1);
                Value::T
            }
            Some(ParseCommentState::Syntax {
                depth: comment_depth,
                nestable: true,
                ..
            }) => Value::fixnum(comment_depth),
            Some(ParseCommentState::Fence {
                depth: comment_depth,
            }) => Value::fixnum(comment_depth),
            None => Value::NIL,
        };

        Value::list(vec![
            Value::fixnum(self.depth),
            self.stack.last().map_or(Value::NIL, |p| Value::fixnum(*p)),
            self.last_sexp_start.map_or(Value::NIL, Value::fixnum),
            string_value,
            comment_value,
            if self.quoted { Value::T } else { Value::NIL },
            Value::fixnum(self.mindepth),
            Value::NIL,
            self.comment_or_string_start
                .map_or(Value::NIL, Value::fixnum),
            stack_value,
            Value::NIL,
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
        None => CommentStopMode::None,
        Some(v) if v.is_nil() => CommentStopMode::None,
        Some(v) => match v.kind() {
            ValueKind::Symbol(sym) if resolve_sym(sym) == "syntax-table" => {
                CommentStopMode::SyntaxTable
            }
            _ => CommentStopMode::Comment,
        },
    }
}

fn parse_state_from_range_with_options(
    buf: &Buffer,
    table: &SyntaxTable,
    from: i64,
    to: i64,
    target_depth: Option<i64>,
    stop_before: bool,
    oldstate: Option<&Value>,
    commentstop: CommentStopMode,
    honor_properties: bool,
) -> (Value, i64) {
    let chars = buffer_chars_in_range(buf, buf.point_min(), buf.point_max());
    let accessible_char_start = buf.text.emacs_byte_to_char(buf.point_min());
    let from_idx = if from > 0 { from as usize - 1 } else { 0 }.min(chars.len());
    let to_idx = if to > 0 { to as usize - 1 } else { 0 }.min(chars.len());

    let mut state = PartialParseState::from_oldstate(oldstate);
    let mut idx = from_idx;
    let mut atom_start: Option<i64> = None;

    let finish_atom = |state: &mut PartialParseState, atom_start: &mut Option<i64>| {
        if let Some(start) = atom_start.take() {
            state.last_sexp_start = Some(start);
        }
    };

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
                    state.last_sexp_start = state.comment_or_string_start;
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
                    state.last_sexp_start = state.comment_or_string_start;
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
                    if class == SyntaxClass::CommentFence {
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

        if stop_before
            && matches!(
                class,
                SyntaxClass::Escape
                    | SyntaxClass::CharQuote
                    | SyntaxClass::Word
                    | SyntaxClass::Symbol
                    | SyntaxClass::Open
                    | SyntaxClass::StringDelim
                    | SyntaxClass::StringFence
            )
        {
            break;
        }

        if !matches!(
            class,
            SyntaxClass::Word | SyntaxClass::Symbol | SyntaxClass::Quote
        ) {
            finish_atom(&mut state, &mut atom_start);
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
                idx += 1;
                if target_depth == Some(state.depth) {
                    break;
                }
                continue;
            }
            SyntaxClass::Close => {
                if state.depth > 0 {
                    state.depth -= 1;
                    state.mindepth = state.mindepth.min(state.depth);
                }
                if let Some(open_pos) = state.stack.pop() {
                    state.last_sexp_start = Some(open_pos);
                }
                idx += 1;
                if target_depth == Some(state.depth) {
                    break;
                }
                continue;
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
            SyntaxClass::CommentFence => {
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
            SyntaxClass::Word | SyntaxClass::Symbol | SyntaxClass::Quote => {
                atom_start.get_or_insert(pos1);
            }
            SyntaxClass::Whitespace | SyntaxClass::EndComment => {}
            _ => {}
        }

        idx += 1;
    }

    finish_atom(&mut state, &mut atom_start);

    (state.into_value(), idx as i64 + 1)
}

fn parse_state_from_range(buf: &Buffer, table: &SyntaxTable, from: i64, to: i64) -> Value {
    parse_state_from_range_with_options(
        buf,
        table,
        from,
        to,
        None,
        false,
        None,
        CommentStopMode::None,
        false,
    )
    .0
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
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    let from = match args[0].kind() {
        ValueKind::Fixnum(n) => n,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("number-or-marker-p"), args[0]],
            ));
        }
    };
    let to = match args[1].kind() {
        ValueKind::Fixnum(n) => n,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("number-or-marker-p"), args[1]],
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
    let table = SyntaxTable::for_buffer(buf);
    let target_depth = match args.get(2) {
        Some(v) if !v.is_nil() => match v.kind() {
            ValueKind::Fixnum(n) => Some(n),
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), *v],
                ));
            }
        },
        _ => None,
    };
    let stop_before = args.get(3).is_some_and(|v| v.is_truthy());
    let oldstate = args.get(4).filter(|v| !v.is_nil());
    let commentstop = parse_commentstop_mode(args.get(5));
    let honor_properties = parse_sexp_lookup_properties_enabled(eval);
    let (state, stop_pos) = parse_state_from_range_with_options(
        buf,
        &table,
        from,
        to,
        target_depth,
        stop_before,
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
            vec![
                Value::symbol("syntax-ppss"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = SyntaxTable::for_buffer(buf);

    let pos = if args.is_empty() || args[0].is_nil() {
        buf.point_char() as i64 + 1
    } else {
        match args[0].kind() {
            ValueKind::Fixnum(n) => n,
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("number-or-marker-p"), args[0]],
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
        false,
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
            vec![Value::symbol("syntax-ppss-flush-cache"), Value::fixnum(0)],
        ));
    }

    match args[0].kind() {
        ValueKind::Fixnum(_) => Ok(Value::NIL),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), args[0]],
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
            vec![Value::symbol("skip-syntax-forward"), Value::fixnum(0)],
        ));
    }
    let syntax_chars = syntax_runtime_string(&args[0])?;
    let limit = if args.len() > 1 && !args[1].is_nil() {
        match args[1].kind() {
            ValueKind::Fixnum(n) => Some(n),
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), args[1]],
                ));
            }
        }
    } else {
        None
    };

    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = SyntaxTable::for_buffer(buf);
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
        buf.text.emacs_byte_to_char(new_pos) as i64 - buf.text.emacs_byte_to_char(old_pt) as i64
    } else {
        buf.text.emacs_byte_to_char(old_pt) as i64 - buf.text.emacs_byte_to_char(new_pos) as i64
    };
    Ok(Value::fixnum(chars_moved))
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
            vec![Value::symbol("skip-syntax-backward"), Value::fixnum(0)],
        ));
    }
    let syntax_chars = syntax_runtime_string(&args[0])?;
    let limit = if args.len() > 1 && !args[1].is_nil() {
        match args[1].kind() {
            ValueKind::Fixnum(n) => Some(n),
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), args[1]],
                ));
            }
        }
    } else {
        None
    };

    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let table = SyntaxTable::for_buffer(buf);
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
        -(buf.text.emacs_byte_to_char(old_pt) as i64 - buf.text.emacs_byte_to_char(new_pos) as i64)
    } else {
        buf.text.emacs_byte_to_char(new_pos) as i64 - buf.text.emacs_byte_to_char(old_pt) as i64
    };
    Ok(Value::fixnum(chars_moved))
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
#[path = "syntax_test.rs"]
mod tests;
