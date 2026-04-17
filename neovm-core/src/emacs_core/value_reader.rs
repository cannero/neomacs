//! Value-native Lisp reader.
//!
//! A mechanical translation of `parser.rs` that produces `Value` (tagged heap
//! pointers) directly instead of intermediate `Expr` AST nodes.
//!
//! Supports: integers, floats, strings (with escapes), symbols, keywords,
//! uninterned symbols (`#:foo`), character literals (?a), lists, dotted pairs,
//! vectors, quote ('), function (#'), backquote (`), unquote (,), splice (,@),
//! line comments (;), block comments (#|..|#), hash-table literals, records,
//! bool-vector literals, byte-code literals, read labels (#N= / #N#),
//! radix integers (#x, #o, #b), propertized strings, reader skip (#@N).

use super::eval::{push_scratch_gc_root, restore_scratch_gc_roots, save_scratch_gc_roots};
use super::intern::{intern, intern_uninterned, resolve_sym};
// bytes_to_unibyte_storage_string and encode_nonunicode_char_for_storage
// imports removed — using emacs_char + Vec<u8> directly
use super::emacs_char;
use std::cell::RefCell;

thread_local! {
    /// Current load-file-name for `#$` reader macro.
    /// Set by `with_load_context` in load.rs before reading a file.
    static READER_LOAD_FILE_NAME: RefCell<Option<Value>> = const { RefCell::new(None) };
}

pub(crate) fn collect_value_reader_gc_roots(roots: &mut Vec<Value>) {
    READER_LOAD_FILE_NAME.with(|slot| {
        if let Some(value) = *slot.borrow() {
            roots.push(value);
        }
    });
}

/// Set the current load-file-name for the `#$` reader macro.
pub fn set_reader_load_file_name(value: Option<Value>) {
    READER_LOAD_FILE_NAME.with(|slot| *slot.borrow_mut() = value);
}

/// Get the current load-file-name for the `#$` reader macro.
fn get_reader_load_file_name() -> Value {
    READER_LOAD_FILE_NAME.with(|slot| slot.borrow().unwrap_or(Value::NIL))
}

/// Public getter for save/restore in with_load_context.
pub fn get_reader_load_file_name_public() -> Option<Value> {
    READER_LOAD_FILE_NAME.with(|slot| *slot.borrow())
}
use super::value::{HashTableTest, Value, build_hash_table_literal_value};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Read all top-level forms from `input`, returning them as `Value`.
pub fn read_all(input: &str) -> Result<Vec<Value>, ReadError> {
    read_all_with_source_multibyte(input, true)
}

/// Read all top-level forms from `input`, preserving the source string's
/// multibyte/unibyte distinction where it affects reader results.
pub fn read_all_with_source_multibyte(
    input: &str,
    source_multibyte: bool,
) -> Result<Vec<Value>, ReadError> {
    let mut reader = Reader::new(input, source_multibyte);
    let mut forms = Vec::new();
    while reader.skip_ws_and_comments() {
        forms.push(reader.read_form()?);
    }
    Ok(forms)
}

/// Read a single form from `input` starting at byte offset `start`.
/// Returns `None` if there is nothing to read (only whitespace/comments remain).
/// On success returns `(value, end_position)`.
pub fn read_one(input: &str, start: usize) -> Result<Option<(Value, usize)>, ReadError> {
    read_one_with_source_multibyte(input, true, start)
}

/// Read a single form from `input`, preserving whether the original source was
/// multibyte or unibyte.
pub fn read_one_with_source_multibyte(
    input: &str,
    source_multibyte: bool,
    start: usize,
) -> Result<Option<(Value, usize)>, ReadError> {
    let mut reader = Reader::new(input, source_multibyte);
    reader.pos = start;
    if !reader.skip_ws_and_comments() {
        return Ok(None);
    }
    let value = reader.read_form()?;
    Ok(Some((value, reader.pos)))
}

/// Read a single form from `input`, optionally wrapping interned symbols
/// in `symbol-with-pos` objects that record the byte offset where the
/// symbol was found.  Used by `read-positioning-symbols`.
pub fn read_one_with_locate_syms(
    input: &str,
    source_multibyte: bool,
    start: usize,
    locate_syms: bool,
) -> Result<Option<(Value, usize)>, ReadError> {
    let mut reader = Reader::new(input, source_multibyte);
    reader.pos = start;
    reader.locate_syms = locate_syms;
    if !reader.skip_ws_and_comments() {
        return Ok(None);
    }
    let value = reader.read_form()?;
    Ok(Some((value, reader.pos)))
}

/// Reader source wrapper for Lisp strings.
///
/// This keeps the runtime-storage adapter inside the reader boundary so callers
/// can work in logical Emacs-byte offsets instead of storage-string byte math.
pub struct LispReadSource<'a> {
    input: &'a crate::heap_types::LispString,
    storage: String,
}

impl<'a> LispReadSource<'a> {
    pub fn new(input: &'a crate::heap_types::LispString) -> Self {
        Self {
            input,
            storage: crate::emacs_core::builtins::runtime_string_from_lisp_string(input),
        }
    }

    pub fn is_multibyte(&self) -> bool {
        self.input.is_multibyte()
    }

    pub fn logical_len(&self) -> usize {
        self.input.sbytes()
    }

    pub fn storage_slice_range(&self, start: usize, end: usize) -> &str {
        assert!(start <= end, "invalid LispReadSource range: {start}..{end}");
        assert!(
            end <= self.logical_len(),
            "LispReadSource end {end} exceeds logical length {}",
            self.logical_len()
        );
        let start_storage = crate::emacs_core::string_escape::storage_logical_byte_to_storage_byte(
            &self.storage,
            start,
        );
        let end_storage = crate::emacs_core::string_escape::storage_logical_byte_to_storage_byte(
            &self.storage,
            end,
        );
        &self.storage[start_storage..end_storage]
    }

    pub fn read_one(&self, start: usize) -> Result<Option<(Value, usize)>, ReadError> {
        self.read_one_range(start, self.logical_len())
    }

    pub fn read_one_range(
        &self,
        start: usize,
        end: usize,
    ) -> Result<Option<(Value, usize)>, ReadError> {
        let substring = self.storage_slice_range(start, end);
        match read_one_with_source_multibyte(substring, self.is_multibyte(), 0) {
            Ok(Some((value, end_pos))) => Ok(Some((
                value,
                start
                    + crate::emacs_core::string_escape::storage_byte_to_logical_byte(
                        substring, end_pos,
                    ),
            ))),
            Ok(None) => Ok(None),
            Err(err) => Err(ReadError {
                message: err.message,
                position: start
                    + crate::emacs_core::string_escape::storage_byte_to_logical_byte(
                        substring,
                        err.position,
                    ),
            }),
        }
    }

    pub fn read_one_with_locate_syms(
        &self,
        start: usize,
        locate_syms: bool,
    ) -> Result<Option<(Value, usize)>, ReadError> {
        self.read_one_range_with_locate_syms(start, self.logical_len(), locate_syms)
    }

    pub fn read_one_range_with_locate_syms(
        &self,
        start: usize,
        end: usize,
        locate_syms: bool,
    ) -> Result<Option<(Value, usize)>, ReadError> {
        let substring = self.storage_slice_range(start, end);
        match read_one_with_locate_syms(substring, self.is_multibyte(), 0, locate_syms) {
            Ok(Some((value, end_pos))) => Ok(Some((
                value,
                start
                    + crate::emacs_core::string_escape::storage_byte_to_logical_byte(
                        substring, end_pos,
                    ),
            ))),
            Ok(None) => Ok(None),
            Err(err) => Err(ReadError {
                message: err.message,
                position: start
                    + crate::emacs_core::string_escape::storage_byte_to_logical_byte(
                        substring,
                        err.position,
                    ),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ReadError {
    pub message: String,
    pub position: usize,
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "read error at {}: {}", self.position, self.message)
    }
}

impl std::error::Error for ReadError {}

// ---------------------------------------------------------------------------
// Reader struct
// ---------------------------------------------------------------------------

struct Reader<'a> {
    input: &'a str,
    source_multibyte: bool,
    pos: usize,
    /// `#N=EXPR` / `#N#` read labels for shared structure in `.elc` files.
    read_labels: std::collections::HashMap<usize, Value>,
    /// When true, wrap interned symbols in symbol-with-pos objects.
    locate_syms: bool,
}

fn translate_runtime_source_char(ch: char) -> u32 {
    let cp = ch as u32;
    if (0xE080..=0xE0FF).contains(&cp) {
        crate::emacs_core::emacs_char::byte8_to_char((cp - 0xE000) as u8)
    } else if (0xE300..=0xE3FF).contains(&cp) {
        (cp - 0xE300) as u32
    } else {
        cp
    }
}

impl<'a> Reader<'a> {
    fn new(input: &'a str, source_multibyte: bool) -> Self {
        Self {
            input,
            source_multibyte,
            pos: 0,
            read_labels: std::collections::HashMap::new(),
            locate_syms: false,
        }
    }

    // -- Whitespace & comments -----------------------------------------------

    fn skip_ws_and_comments(&mut self) -> bool {
        loop {
            let Some(ch) = self.current() else {
                return false;
            };
            if ch.is_ascii_whitespace() {
                self.bump();
                continue;
            }
            if ch == ';' {
                // Line comment
                while let Some(c) = self.current() {
                    self.bump();
                    if c == '\n' {
                        break;
                    }
                }
                continue;
            }
            if ch == '#' && self.peek_at(1) == Some('|') {
                // Block comment #| ... |#
                self.bump(); // #
                self.bump(); // |
                let mut depth = 1;
                while depth > 0 {
                    match self.current() {
                        None => return false,
                        Some('#') if self.peek_at(1) == Some('|') => {
                            self.bump();
                            self.bump();
                            depth += 1;
                        }
                        Some('|') if self.peek_at(1) == Some('#') => {
                            self.bump();
                            self.bump();
                            depth -= 1;
                        }
                        _ => self.bump(),
                    }
                }
                continue;
            }
            return true;
        }
    }

    // -- Main read dispatch --------------------------------------------------

    fn read_form(&mut self) -> Result<Value, ReadError> {
        self.skip_ws_and_comments();
        // Record the byte position before reading — used by locate_syms
        // to tag symbols with their source offset (mirrors GNU read0).
        let form_start = self.pos;
        let Some(ch) = self.current() else {
            return Err(self.error("unexpected end of input"));
        };

        let value = match ch {
            '(' => self.read_list_or_dotted(),
            ')' => {
                self.bump();
                Err(self.error(")"))
            }
            '[' => self.read_vector(),
            '\'' => {
                self.bump();
                let saved = save_scratch_gc_roots();
                let quoted = self.read_form()?;
                push_scratch_gc_root(quoted);
                let result = Value::list(vec![Value::symbol("quote"), quoted]);
                restore_scratch_gc_roots(saved);
                Ok(result)
            }
            '`' => {
                self.bump();
                let saved = save_scratch_gc_roots();
                let quoted = self.read_form()?;
                push_scratch_gc_root(quoted);
                let result = Value::list(vec![Value::symbol(intern("`")), quoted]);
                restore_scratch_gc_roots(saved);
                Ok(result)
            }
            ',' => {
                self.bump();
                if self.current() == Some('@') {
                    self.bump();
                    let saved = save_scratch_gc_roots();
                    let expr = self.read_form()?;
                    push_scratch_gc_root(expr);
                    let result = Value::list(vec![Value::symbol(intern(",@")), expr]);
                    restore_scratch_gc_roots(saved);
                    Ok(result)
                } else {
                    let saved = save_scratch_gc_roots();
                    let expr = self.read_form()?;
                    push_scratch_gc_root(expr);
                    let result = Value::list(vec![Value::symbol(intern(",")), expr]);
                    restore_scratch_gc_roots(saved);
                    Ok(result)
                }
            }
            '"' => self.read_string(),
            '?' => self.read_char_literal(),
            '#' => self.read_hash_syntax(),
            _ => self.read_atom(),
        }?;

        // Wrap symbols with their source position when locate_syms is active.
        // Matches GNU read0: SYMBOLP(val) && !NILP(val).
        if self.locate_syms && value.is_symbol() && !value.is_nil() {
            let pos_val = Value::fixnum(form_start as i64);
            Ok(crate::tagged::gc::with_tagged_heap(|heap| {
                heap.alloc_symbol_with_pos(value, pos_val)
            }))
        } else {
            Ok(value)
        }
    }

    // -- Lists and dotted pairs ----------------------------------------------

    fn read_list_or_dotted(&mut self) -> Result<Value, ReadError> {
        self.expect('(')?;
        let saved = save_scratch_gc_roots();
        let mut items = Vec::new();
        loop {
            self.skip_ws_and_comments();
            match self.current() {
                Some(')') => {
                    self.bump();
                    let result = Value::list(items);
                    restore_scratch_gc_roots(saved);
                    return Ok(result);
                }
                Some('.') if self.is_dot_separator() => {
                    // Dotted pair
                    self.bump(); // consume '.'
                    let cdr = self.read_form()?;
                    push_scratch_gc_root(cdr);
                    self.skip_ws_and_comments();
                    match self.current() {
                        Some(')') => {
                            self.bump();
                            // Build cons chain: (a b c . d)
                            // items = [a, b, c], cdr = d
                            let mut acc = cdr;
                            for item in items.into_iter().rev() {
                                acc = Value::cons(item, acc);
                                push_scratch_gc_root(acc);
                            }
                            restore_scratch_gc_roots(saved);
                            return Ok(acc);
                        }
                        _ => {
                            restore_scratch_gc_roots(saved);
                            return Err(self.error("expected ')' after dotted pair"));
                        }
                    }
                }
                Some(_) => {
                    let item = self.read_form()?;
                    push_scratch_gc_root(item);
                    items.push(item);
                }
                None => {
                    restore_scratch_gc_roots(saved);
                    return Err(self.error("unterminated list"));
                }
            }
        }
    }

    /// Check if current '.' is a dot separator (not part of a number like 1.5).
    fn is_dot_separator(&self) -> bool {
        match self.peek_at(1) {
            None => true,
            Some(c) => c.is_ascii_whitespace() || c == ')' || c == '(' || c == ';',
        }
    }

    // -- Vectors [1 2 3] ----------------------------------------------------

    /// Read `[...]` and return items as a Vec<Value>.
    fn read_vector_items(&mut self) -> Result<Vec<Value>, ReadError> {
        self.expect('[')?;
        let saved = save_scratch_gc_roots();
        let mut items = Vec::new();
        loop {
            self.skip_ws_and_comments();
            match self.current() {
                Some(']') => {
                    self.bump();
                    restore_scratch_gc_roots(saved);
                    return Ok(items);
                }
                Some(_) => {
                    let item = self.read_form()?;
                    push_scratch_gc_root(item);
                    items.push(item);
                }
                None => {
                    restore_scratch_gc_roots(saved);
                    return Err(self.error("unterminated vector"));
                }
            }
        }
    }

    fn read_vector(&mut self) -> Result<Value, ReadError> {
        let saved = save_scratch_gc_roots();
        let items = self.read_vector_items()?;
        for item in &items {
            push_scratch_gc_root(*item);
        }
        let result = Value::make_vector(items);
        restore_scratch_gc_roots(saved);
        Ok(result)
    }

    // -- Strings "..." -------------------------------------------------------

    fn read_string(&mut self) -> Result<Value, ReadError> {
        self.expect('"')?;
        let mut buf = Vec::new();
        let mut unibyte_buf = (!self.source_multibyte).then(Vec::new);
        loop {
            let Some(ch) = self.current() else {
                return Err(self.error("unterminated string"));
            };
            self.bump();
            match ch {
                '"' => {
                    let string = if let Some(bytes) = unibyte_buf {
                        crate::heap_types::LispString::from_unibyte(bytes)
                    } else {
                        maybe_recombine_latin1_emacs(buf)
                    };
                    return Ok(Value::heap_string(string));
                }
                '\\' => {
                    let Some(esc) = self.current() else {
                        return Err(self.error("unterminated escape in string"));
                    };
                    self.bump();
                    match esc {
                        'n' => {
                            buf.push(b'\n');
                            if let Some(bytes) = unibyte_buf.as_mut() {
                                bytes.push(b'\n');
                            }
                        }
                        'r' => {
                            buf.push(b'\r');
                            if let Some(bytes) = unibyte_buf.as_mut() {
                                bytes.push(b'\r');
                            }
                        }
                        't' => {
                            buf.push(b'\t');
                            if let Some(bytes) = unibyte_buf.as_mut() {
                                bytes.push(b'\t');
                            }
                        }
                        '\\' => {
                            buf.push(b'\\');
                            if let Some(bytes) = unibyte_buf.as_mut() {
                                bytes.push(b'\\');
                            }
                        }
                        '"' => {
                            buf.push(b'"');
                            if let Some(bytes) = unibyte_buf.as_mut() {
                                bytes.push(b'"');
                            }
                        }
                        'a' => {
                            buf.push(0x07);
                            if let Some(bytes) = unibyte_buf.as_mut() {
                                bytes.push(0x07);
                            }
                        }
                        'b' => {
                            buf.push(0x08);
                            if let Some(bytes) = unibyte_buf.as_mut() {
                                bytes.push(0x08);
                            }
                        }
                        'f' => {
                            buf.push(0x0C);
                            if let Some(bytes) = unibyte_buf.as_mut() {
                                bytes.push(0x0C);
                            }
                        }
                        'e' => {
                            buf.push(0x1B);
                            if let Some(bytes) = unibyte_buf.as_mut() {
                                bytes.push(0x1B);
                            }
                        }
                        'v' => {
                            buf.push(0x0B);
                            if let Some(bytes) = unibyte_buf.as_mut() {
                                bytes.push(0x0B);
                            }
                        }
                        // Modifier escapes in strings
                        's' if self.current() == Some('-') => {
                            self.bump(); // consume '-'
                            let val = self.parse_string_char_value(1 << 23)?;
                            Self::push_modified_char_bytes(&mut buf, val);
                            unibyte_buf = None;
                        }
                        's' => {
                            buf.push(b' ');
                            if let Some(bytes) = unibyte_buf.as_mut() {
                                bytes.push(b' ');
                            }
                        }
                        'C' if self.current() == Some('-') => {
                            self.bump(); // consume '-'
                            let base = self.parse_string_char_value(0)?;
                            let base_char = base & 0x3FFFFF;
                            let mods = base & !0x3FFFFFu32;
                            let result = if base_char == 0x3F {
                                0x7F | mods // '?' -> DEL
                            } else if (0x40..=0x5F).contains(&base_char)
                                || (0x61..=0x7A).contains(&base_char)
                            {
                                (base_char & 0x1F) | mods
                            } else {
                                base_char | mods | (1u32 << 26)
                            };
                            Self::push_modified_char_bytes(&mut buf, result);
                            unibyte_buf = None;
                        }
                        'M' if self.current() == Some('-') => {
                            self.bump(); // consume '-'
                            let val = self.parse_string_char_value(1 << 27)?;
                            Self::push_modified_char_bytes(&mut buf, val);
                            unibyte_buf = None;
                        }
                        'S' if self.current() == Some('-') => {
                            self.bump(); // consume '-'
                            let val = self.parse_string_char_value(1 << 25)?;
                            Self::push_modified_char_bytes(&mut buf, val);
                            unibyte_buf = None;
                        }
                        'A' if self.current() == Some('-') => {
                            self.bump(); // consume '-'
                            let val = self.parse_string_char_value(1 << 22)?;
                            Self::push_modified_char_bytes(&mut buf, val);
                            unibyte_buf = None;
                        }
                        'H' if self.current() == Some('-') => {
                            self.bump(); // consume '-'
                            let val = self.parse_string_char_value(1 << 24)?;
                            Self::push_modified_char_bytes(&mut buf, val);
                            unibyte_buf = None;
                        }
                        'd' => {
                            buf.push(0x7F);
                            if let Some(bytes) = unibyte_buf.as_mut() {
                                bytes.push(0x7F);
                            }
                        }
                        'x' => {
                            let (hex, _digit_count) = self.read_hex_digits()?;
                            if hex <= emacs_char::MAX_CHAR {
                                let mut tmp = [0u8; emacs_char::MAX_MULTIBYTE_LENGTH];
                                let len = emacs_char::char_string(hex, &mut tmp);
                                buf.extend_from_slice(&tmp[..len]);
                                if let Some(bytes) = unibyte_buf.as_mut() {
                                    if hex <= 0xFF {
                                        bytes.push(hex as u8);
                                    } else {
                                        unibyte_buf = None;
                                    }
                                }
                            } else {
                                return Err(self.error(
                                    "invalid codepoint in \\x escape (exceeds Emacs 22-bit limit)",
                                ));
                            }
                        }
                        'u' => {
                            let hex = self.read_fixed_hex(4)?;
                            let mut tmp = [0u8; emacs_char::MAX_MULTIBYTE_LENGTH];
                            let len = emacs_char::char_string(hex, &mut tmp);
                            buf.extend_from_slice(&tmp[..len]);
                            if let Some(bytes) = unibyte_buf.as_mut() {
                                if hex < 0x80 {
                                    bytes.push(hex as u8);
                                } else {
                                    unibyte_buf = None;
                                }
                            }
                        }
                        'U' => {
                            let hex = self.read_fixed_hex(8)?;
                            if hex <= emacs_char::MAX_CHAR {
                                let mut tmp = [0u8; emacs_char::MAX_MULTIBYTE_LENGTH];
                                let len = emacs_char::char_string(hex, &mut tmp);
                                buf.extend_from_slice(&tmp[..len]);
                                if let Some(bytes) = unibyte_buf.as_mut() {
                                    if hex < 0x80 {
                                        bytes.push(hex as u8);
                                    } else {
                                        unibyte_buf = None;
                                    }
                                }
                            } else {
                                return Err(self.error("invalid unicode codepoint in \\U escape"));
                            }
                        }
                        'N' if self.current() == Some('{') => {
                            let value = self.read_unicode_name_escape()?;
                            if let Some(c) = char::from_u32(value) {
                                let mut tmp = [0u8; 4];
                                buf.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
                                if let Some(bytes) = unibyte_buf.as_mut() {
                                    if value < 0x80 {
                                        bytes.push(value as u8);
                                    } else {
                                        unibyte_buf = None;
                                    }
                                }
                            } else {
                                return Err(self.error("invalid unicode codepoint in \\N{...}"));
                            }
                        }
                        '0'..='7' => {
                            // Octal escape
                            let mut val = (esc as u32) - ('0' as u32);
                            for _ in 0..2 {
                                match self.current() {
                                    Some(c @ '0'..='7') => {
                                        self.bump();
                                        val = val * 8 + (c as u32 - '0' as u32);
                                    }
                                    _ => break,
                                }
                            }
                            if val <= emacs_char::MAX_CHAR {
                                let mut tmp = [0u8; emacs_char::MAX_MULTIBYTE_LENGTH];
                                let len = emacs_char::char_string(val, &mut tmp);
                                buf.extend_from_slice(&tmp[..len]);
                                if let Some(bytes) = unibyte_buf.as_mut() {
                                    if val <= 0xFF {
                                        bytes.push(val as u8);
                                    } else {
                                        unibyte_buf = None;
                                    }
                                }
                            }
                        }
                        '\n' => {
                            // Line continuation — skip newline
                        }
                        other => {
                            // Unknown escape — keep the character as UTF-8
                            let mut tmp = [0u8; 4];
                            buf.extend_from_slice(other.encode_utf8(&mut tmp).as_bytes());
                            if let Some(bytes) = unibyte_buf.as_mut() {
                                if other.is_ascii() {
                                    bytes.push(other as u8);
                                } else {
                                    unibyte_buf = None;
                                }
                            }
                        }
                    }
                }
                other => {
                    let cp = other as u32;
                    if (0xE300..=0xE3FF).contains(&cp) {
                        let byte = (cp - 0xE300) as u8;
                        buf.push(byte);
                        if let Some(bytes) = unibyte_buf.as_mut() {
                            bytes.push(byte);
                        }
                    } else if (0xE080..=0xE0FF).contains(&cp) {
                        let mut tmp = [0u8; emacs_char::MAX_MULTIBYTE_LENGTH];
                        let len = emacs_char::char_string(
                            emacs_char::byte8_to_char((cp - 0xE000) as u8),
                            &mut tmp,
                        );
                        buf.extend_from_slice(&tmp[..len]);
                        unibyte_buf = None;
                    } else if cp >= 0x80 && cp <= 0xFF {
                        // Non-ASCII byte from .elc loading (Latin-1 mapped).
                        //
                        // When source_multibyte=false, .elc content uses Latin-1
                        // encoding (each byte as char with same code point). A
                        // UTF-8 multi-byte sequence like U+2018 (bytes E2 80 98)
                        // arrives as three separate chars: U+00E2, U+0080, U+0098.
                        //
                        // GNU Emacs lread.c reads raw bytes and decodes UTF-8:
                        // any non-ASCII char sets force_multibyte=true (line 3131).
                        //
                        // Match GNU: if this byte is a UTF-8 lead byte (>= 0xC0),
                        // reassemble the full sequence from following continuation
                        // bytes (0x80..0xBF). On success, emit the decoded Unicode
                        // char and force multibyte. On failure, emit the raw byte.
                        let byte0 = cp as u8;
                        let decoded = if !self.source_multibyte && byte0 >= 0xC0 {
                            let expected_len = if byte0 < 0xE0 { 2 }
                                else if byte0 < 0xF0 { 3 }
                                else if byte0 < 0xF8 { 4 }
                                else { 0 };
                            if expected_len >= 2 {
                                let save_pos = self.pos;
                                let mut utf8_bytes = vec![byte0];
                                let mut ok = true;
                                for _ in 1..expected_len {
                                    match self.current() {
                                        Some(c) if (c as u32) >= 0x80 && (c as u32) <= 0xBF => {
                                            utf8_bytes.push(c as u8);
                                            self.bump();
                                        }
                                        _ => { ok = false; break; }
                                    }
                                }
                                if ok {
                                    if let Ok(s) = std::str::from_utf8(&utf8_bytes) {
                                        s.chars().next().map(|ch| ch as u32)
                                    } else {
                                        self.pos = save_pos;
                                        None
                                    }
                                } else {
                                    self.pos = save_pos;
                                    None
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        };

                        if let Some(code) = decoded {
                            // Successfully decoded a UTF-8 multi-byte char
                            let mut tmp = [0u8; emacs_char::MAX_MULTIBYTE_LENGTH];
                            let len = emacs_char::char_string(code, &mut tmp);
                            buf.extend_from_slice(&tmp[..len]);
                            unibyte_buf = None; // force multibyte
                        } else {
                            // Raw byte — keep as unibyte
                            buf.push(byte0);
                            if let Some(bytes) = unibyte_buf.as_mut() {
                                bytes.push(byte0);
                            }
                        }
                    } else {
                        // Normal Unicode — encode as UTF-8 (== Emacs encoding)
                        let mut tmp = [0u8; 4];
                        buf.extend_from_slice(other.encode_utf8(&mut tmp).as_bytes());
                        if let Some(bytes) = unibyte_buf.as_mut() {
                            if other.is_ascii() {
                                bytes.push(other as u8);
                            } else {
                                unibyte_buf = None;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Parse the next character in a string, applying accumulated modifiers.
    /// Handles recursive modifiers (e.g. `\M-\C-x`) and escape sequences.
    fn parse_string_char_value(&mut self, modifiers: u32) -> Result<u32, ReadError> {
        let Some(ch) = self.current() else {
            return Err(self.error("expected character after modifier escape in string"));
        };
        self.bump();
        if ch == '\\' {
            let Some(esc) = self.current() else {
                return Err(self.error("unterminated escape in string modifier"));
            };
            self.bump();
            match esc {
                'C' if self.current() == Some('-') => {
                    self.bump();
                    let base = self.parse_string_char_value(modifiers)?;
                    let base_char = base & 0x3FFFFF;
                    let mods = base & !0x3FFFFFu32;
                    Ok(if base_char == 0x3F {
                        0x7F | mods
                    } else if (0x40..=0x5F).contains(&base_char)
                        || (0x61..=0x7A).contains(&base_char)
                    {
                        (base_char & 0x1F) | mods
                    } else {
                        base_char | mods | (1u32 << 26)
                    })
                }
                'M' if self.current() == Some('-') => {
                    self.bump();
                    self.parse_string_char_value(modifiers | (1 << 27))
                }
                'S' if self.current() == Some('-') => {
                    self.bump();
                    self.parse_string_char_value(modifiers | (1 << 25))
                }
                's' if self.current() == Some('-') => {
                    self.bump();
                    self.parse_string_char_value(modifiers | (1 << 23))
                }
                'A' if self.current() == Some('-') => {
                    self.bump();
                    self.parse_string_char_value(modifiers | (1 << 22))
                }
                'H' if self.current() == Some('-') => {
                    self.bump();
                    self.parse_string_char_value(modifiers | (1 << 24))
                }
                'n' => Ok('\n' as u32 | modifiers),
                'r' => Ok('\r' as u32 | modifiers),
                't' => Ok('\t' as u32 | modifiers),
                'a' => Ok('\x07' as u32 | modifiers),
                'b' => Ok('\x08' as u32 | modifiers),
                'f' => Ok('\x0C' as u32 | modifiers),
                'v' => Ok('\x0B' as u32 | modifiers),
                'e' => Ok('\x1B' as u32 | modifiers),
                's' => Ok(' ' as u32 | modifiers),
                'd' => Ok('\x7F' as u32 | modifiers),
                '\\' => Ok('\\' as u32 | modifiers),
                '"' => Ok('"' as u32 | modifiers),
                'N' if self.current() == Some('{') => {
                    Ok(self.read_unicode_name_escape()? | modifiers)
                }
                '^' => {
                    let Some(base) = self.current() else {
                        return Err(self.error("expected char after \\^ in string"));
                    };
                    self.bump();
                    Ok((translate_runtime_source_char(base) & 0x1F) | modifiers)
                }
                other => Ok(translate_runtime_source_char(other) | modifiers),
            }
        } else {
            Ok(translate_runtime_source_char(ch) | modifiers)
        }
    }

    /// Push a character value (possibly with modifier bits) into a byte buffer.
    fn push_modified_char_bytes(buf: &mut Vec<u8>, val: u32) {
        let meta = val & (1 << 27) != 0;
        let base = val & !(1u32 << 27); // strip meta bit
        if meta && base < 128 {
            // Meta + ASCII: encode as raw byte (base | 0x80)
            let mut tmp = [0u8; emacs_char::MAX_MULTIBYTE_LENGTH];
            let c = emacs_char::byte8_to_char((base | 0x80) as u8);
            let len = emacs_char::char_string(c, &mut tmp);
            buf.extend_from_slice(&tmp[..len]);
        } else {
            let code = val & 0x3FFFFF;
            let mut tmp = [0u8; emacs_char::MAX_MULTIBYTE_LENGTH];
            let len = emacs_char::char_string(code, &mut tmp);
            buf.extend_from_slice(&tmp[..len]);
        }
    }

    fn read_hex_digits(&mut self) -> Result<(u32, usize), ReadError> {
        let start = self.pos;
        while let Some(c) = self.current() {
            if c.is_ascii_hexdigit() {
                self.bump();
            } else {
                if c == ';' {
                    self.bump(); // consume terminating semicolon
                }
                break;
            }
        }
        let hex_str = &self.input[start..self.pos].trim_end_matches(';');
        if hex_str.is_empty() {
            return Err(self.error("expected hex digits after \\x"));
        }
        let digits = hex_str.len();
        let value =
            u32::from_str_radix(hex_str, 16).map_err(|_| self.error("invalid hex escape"))?;
        Ok((value, digits))
    }

    fn read_fixed_hex(&mut self, count: usize) -> Result<u32, ReadError> {
        let start = self.pos;
        for _ in 0..count {
            match self.current() {
                Some(c) if c.is_ascii_hexdigit() => self.bump(),
                _ => return Err(self.error(&format!("expected {} hex digits", count))),
            }
        }
        u32::from_str_radix(&self.input[start..self.pos], 16)
            .map_err(|_| self.error("invalid hex escape"))
    }

    fn read_unicode_name_escape(&mut self) -> Result<u32, ReadError> {
        self.expect('{')?;
        let start = self.pos;
        while let Some(ch) = self.current() {
            if ch == '}' {
                break;
            }
            self.bump();
        }
        if self.current() != Some('}') {
            return Err(self.error("unterminated \\N{...} escape"));
        }
        let name = &self.input[start..self.pos];
        self.bump();

        let hex = name
            .strip_prefix("U+")
            .or_else(|| name.strip_prefix("u+"))
            .ok_or_else(|| self.error("unsupported \\N{...} escape"))?;
        let value =
            u32::from_str_radix(hex, 16).map_err(|_| self.error("invalid \\N{...} escape"))?;
        if value > 0x3F_FFFF {
            return Err(self.error("\\N{...} escape out of range"));
        }
        Ok(value)
    }

    // -- Character literals ?a -----------------------------------------------

    fn read_char_literal(&mut self) -> Result<Value, ReadError> {
        self.expect('?')?;
        if matches!(self.current(), Some(' ' | '\t')) {
            let ch = self.current().expect("matched whitespace char literal");
            self.bump();
            return Ok(Value::char(ch));
        }

        let val = self.parse_char_value(0)?;
        if matches!(self.current(), Some(ch) if !is_char_literal_delimiter(ch)) {
            return Err(self.error("?"));
        }
        // Character literals with modifier bits produce values beyond Unicode range.
        // Emit them as fixnums, matching GNU Emacs where characters ARE integers.
        Ok(Value::fixnum(val as i64))
    }

    /// Parse the value part of a character literal, accumulating modifier bits.
    fn parse_char_value(&mut self, modifiers: u32) -> Result<u32, ReadError> {
        let Some(ch) = self.current() else {
            return Err(self.error("expected character in char literal"));
        };
        self.bump();

        if ch == '\\' {
            let Some(esc) = self.current() else {
                return Err(self.error("unterminated character escape"));
            };
            self.bump();
            let val = match esc {
                'n' => '\n' as u32,
                'r' => '\r' as u32,
                't' => '\t' as u32,
                '\\' => '\\' as u32,
                '\'' => '\'' as u32,
                '"' => '"' as u32,
                'a' => 0x07, // BEL
                'b' => 0x08, // BS
                'f' => 0x0C, // FF
                'v' => 0x0B, // VT
                'e' => 0x1B, // ESC
                'd' => 0x7F, // DEL
                's' if self.current() == Some('-') => {
                    self.bump();
                    return self.parse_char_value(modifiers | (1 << 23)); // super bit
                }
                's' => ' ' as u32,
                'x' => self.read_hex_digits()?.0,
                'u' => self.read_fixed_hex(4)?,
                'U' => self.read_fixed_hex(8)?,
                'N' if self.current() == Some('{') => self.read_unicode_name_escape()?,
                '0'..='7' => {
                    let mut val = (esc as u32) - ('0' as u32);
                    for _ in 0..2 {
                        match self.current() {
                            Some(c @ '0'..='7') => {
                                self.bump();
                                val = val * 8 + (c as u32 - '0' as u32);
                            }
                            _ => break,
                        }
                    }
                    val
                }
                'C' if self.current() == Some('-') => {
                    self.bump(); // consume '-'
                    let base = self.parse_char_value(modifiers)?;
                    let base_char = base & 0x3FFFFF;
                    let existing_mods = base & 0xFC00000;
                    if base_char == 0x3F {
                        return Ok(0x7F | existing_mods);
                    } else if (base_char >= 0x40 && base_char <= 0x5F)
                        || (base_char >= 0x61 && base_char <= 0x7A)
                    {
                        return Ok((base_char & 0x1F) | existing_mods);
                    } else {
                        return Ok(base_char | existing_mods | (1u32 << 26));
                    }
                }
                'M' if self.current() == Some('-') => {
                    self.bump();
                    return self.parse_char_value(modifiers | (1 << 27)); // meta bit
                }
                'S' if self.current() == Some('-') => {
                    self.bump();
                    return self.parse_char_value(modifiers | (1 << 25)); // shift bit
                }
                'A' if self.current() == Some('-') => {
                    self.bump();
                    return self.parse_char_value(modifiers | (1 << 22)); // alt bit
                }
                'H' if self.current() == Some('-') => {
                    self.bump();
                    return self.parse_char_value(modifiers | (1 << 24)); // hyper bit
                }
                '^' => {
                    let Some(base) = self.current() else {
                        return Err(self.error("expected char after \\^"));
                    };
                    self.bump();
                    let base_val = translate_runtime_source_char(base);
                    if base_val == 0x3F {
                        0x7F // '?' -> DEL
                    } else {
                        base_val & 0x1F
                    }
                }
                other => translate_runtime_source_char(other),
            };
            Ok(val | modifiers)
        } else {
            Ok(translate_runtime_source_char(ch) | modifiers)
        }
    }

    // -- Hash syntax #' #( etc -----------------------------------------------

    fn read_hash_syntax(&mut self) -> Result<Value, ReadError> {
        self.expect('#')?;
        let Some(ch) = self.current() else {
            return Err(self.error("#"));
        };

        match ch {
            '\'' => {
                // #'function
                self.bump();
                let saved = save_scratch_gc_roots();
                let expr = self.read_form()?;
                push_scratch_gc_root(expr);
                let result = Value::list(vec![Value::symbol("function"), expr]);
                restore_scratch_gc_roots(saved);
                Ok(result)
            }
            '(' => {
                // #("string" START END (PROPS...) ...) — propertized string.
                // Parse all elements, extract the string (first element), and
                // discard text properties for now.
                let saved = save_scratch_gc_roots();
                let list = self.read_list_or_dotted()?;
                push_scratch_gc_root(list);
                // Extract the first element (should be a string)
                if list.is_cons() {
                    let car = list.cons_car();
                    if car.is_string() {
                        restore_scratch_gc_roots(saved);
                        return Ok(car);
                    } else {
                        restore_scratch_gc_roots(saved);
                        return Err(self.error("#(: first element must be a string"));
                    }
                } else if list.is_nil() {
                    restore_scratch_gc_roots(saved);
                    return Err(self.error("#(: expected propertized string"));
                }
                restore_scratch_gc_roots(saved);
                Err(self.error("#(: expected propertized string"))
            }
            '[' => {
                // #[...] — compiled-function literal in .elc.
                // Produce a ByteCode Value directly, matching GNU Emacs's reader.
                // The vector items are [arglist bytecode-string constants-vector
                // stack-depth docstring interactive-spec].
                // Since read_form is called recursively for each element, any
                // nested #[...] literals in the constants vector are already
                // converted to ByteCode values by the time we get here.
                let saved = save_scratch_gc_roots();
                let items = self.read_vector_items()?;
                for item in &items {
                    push_scratch_gc_root(*item);
                }
                let result = if items.len() >= 4 {
                    crate::emacs_core::builtins::make_byte_code_from_parts(
                        &items[0],
                        &items[1],
                        &items[2],
                        &items[3],
                        items.get(4),
                        items.get(5),
                    )
                    .map_err(|e| {
                        let msg = match &e {
                            crate::emacs_core::error::Flow::Signal(sig) => sig
                                .data
                                .first()
                                .and_then(|v| v.is_string().then(|| v.as_runtime_string_owned()))
                                .flatten()
                                .unwrap_or_else(|| format!("{:?}", sig.data)),
                            other => format!("{:?}", other),
                        };
                        self.error(&format!("byte-code literal: {}", msg))
                    })
                } else {
                    // Too few elements — fall back to plain vector
                    Ok(Value::make_vector(items))
                };
                restore_scratch_gc_roots(saved);
                result
            }
            '@' => {
                // #@N<bytes> — reader skip used by .elc for inline data blocks.
                self.read_hash_skip_bytes()
            }
            '!' => {
                // `#!shebang line` — GNU `lread.c` treats `#!` as a
                // comment to end-of-line, so a script-style shebang
                // (`#!/usr/bin/env emacs --script`) loads cleanly.
                // Skip to the next newline (or EOF) and read the next
                // form.
                self.bump();
                while let Some(c) = self.current() {
                    self.bump();
                    if c == '\n' {
                        break;
                    }
                }
                self.read_form()
            }
            ':' => {
                // #:X — uninterned symbol.
                self.bump();
                let (token, _) = self.read_symbol_token();
                Ok(Value::from_sym_id(intern_uninterned(&token)))
            }
            '$' => {
                // #$ — expands to the current load file name during read.
                // Matches GNU lread.c: returns Vload_file_name (the actual
                // file path string), not the symbol `load-file-name`.
                self.bump();
                Ok(get_reader_load_file_name())
            }
            '#' => {
                // ## — symbol with empty name.
                self.bump();
                Ok(Value::from_sym_id(intern("")))
            }
            'b' | 'B' => {
                // #b... binary integer
                self.bump();
                self.read_radix_number(2)
            }
            'o' | 'O' => {
                // #o... octal integer
                self.bump();
                self.read_radix_number(8)
            }
            'x' | 'X' => {
                // #x... hex integer
                self.bump();
                self.read_radix_number(16)
            }
            's' => {
                // #s(hash-table ...) or #s(record-type ...)
                self.bump();
                if self.current() == Some('(') {
                    self.read_hash_table_or_record_literal()
                } else {
                    Err(self.error("#s"))
                }
            }
            '&' => {
                // #&SIZE"DATA" — bool-vector literal.
                self.bump();
                self.read_bool_vector_literal()
            }
            '0'..='9' => {
                // #N=EXPR defines read label N, #N# references it.
                let mut n: usize = (ch as u8 - b'0') as usize;
                self.bump();
                while let Some(d) = self.current() {
                    if d.is_ascii_digit() {
                        n = n * 10 + (d as u8 - b'0') as usize;
                        self.bump();
                    } else {
                        break;
                    }
                }
                match self.current() {
                    Some('=') => {
                        // #N=EXPR — define label N and return EXPR
                        self.bump();
                        let expr = self.read_form()?;
                        self.read_labels.insert(n, expr);
                        Ok(expr)
                    }
                    Some('#') => {
                        // #N# — reference previously defined label N
                        self.bump();
                        self.read_labels
                            .get(&n)
                            .copied()
                            .ok_or_else(|| self.error(&format!("#{n}#: undefined read label")))
                    }
                    _ => Err(self.error(&format!("#{n}"))),
                }
            }
            _ => Err(self.error_after_current(&format!("#{}", ch))),
        }
    }

    fn read_hash_skip_bytes(&mut self) -> Result<Value, ReadError> {
        self.expect('@')?;
        if !matches!(self.current(), Some(c) if c.is_ascii_digit()) {
            return Err(self.error("end of input"));
        }
        let len = self.parse_decimal_usize()?;
        self.skip_exact_source_bytes(len)?;
        self.read_form()
    }

    fn read_bool_vector_literal(&mut self) -> Result<Value, ReadError> {
        if !matches!(self.current(), Some(c) if c.is_ascii_digit()) {
            return Err(self.error("#& expected decimal size"));
        }
        let size = self.parse_decimal_usize()?;
        let data = self.read_string()?;
        let data_str = data
            .as_utf8_str()
            .ok_or_else(|| self.error("#& expected string after size"))?;

        // Expand packed bytes to individual bits and emit as
        // (bool-vector t nil t ...) — the builtin uses truthiness.
        let saved = save_scratch_gc_roots();
        let mut call = Vec::with_capacity(1 + size);
        call.push(Value::symbol("bool-vector"));
        let mut bit_count = 0;
        for byte_val in data_str.bytes() {
            for bit_idx in 0..8 {
                if bit_count >= size {
                    break;
                }
                if (byte_val >> bit_idx) & 1 != 0 {
                    call.push(Value::T);
                } else {
                    call.push(Value::NIL);
                }
                bit_count += 1;
            }
        }
        // Pad with nil if data is shorter than SIZE
        while bit_count < size {
            call.push(Value::NIL);
            bit_count += 1;
        }
        let result = Value::list(call);
        restore_scratch_gc_roots(saved);
        Ok(result)
    }

    fn read_radix_number(&mut self, radix: u32) -> Result<Value, ReadError> {
        let start = self.pos;
        let negative = if self.current() == Some('-') {
            self.bump();
            true
        } else if self.current() == Some('+') {
            self.bump();
            false
        } else {
            false
        };

        while let Some(c) = self.current() {
            if c.is_digit(radix) || c == '_' {
                self.bump();
            } else {
                break;
            }
        }

        let digits: String = self.input[start..self.pos]
            .chars()
            .filter(|c| *c != '_' && *c != '-' && *c != '+')
            .collect();
        if digits.is_empty() {
            return Err(self.error(&format!("integer, radix {}", radix)));
        }

        // Try i64 first; on overflow promote to a rug::Integer with the
        // requested radix. Mirrors GNU `string_to_number` (`src/lread.c`)
        // which falls through to the bignum path on overflow.
        let value = match i64::from_str_radix(&digits, radix) {
            Ok(val) => Value::make_integer(rug::Integer::from(if negative { -val } else { val })),
            Err(_) => {
                let mut signed = String::with_capacity(digits.len() + 1);
                if negative {
                    signed.push('-');
                }
                signed.push_str(&digits);
                let parsed = rug::Integer::parse_radix(&signed, radix as i32)
                    .map_err(|_| self.error("invalid radix number"))?;
                Value::make_integer(rug::Integer::from(parsed))
            }
        };
        Ok(value)
    }

    fn read_hash_table_or_record_literal(&mut self) -> Result<Value, ReadError> {
        // #s(hash-table size N test T data (k1 v1 k2 v2 ...))
        // or #s(record-type field1 field2 ...)
        let saved = save_scratch_gc_roots();
        let list = self.read_list_or_dotted()?;
        push_scratch_gc_root(list);

        // Check if this is a proper list (cons chain)
        if !list.is_cons() && !list.is_nil() {
            // Not a proper list — fallback
            let result = Value::list(vec![
                Value::symbol("make-hash-table-from-literal"),
                Value::list(vec![Value::symbol("quote"), list]),
            ]);
            restore_scratch_gc_roots(saved);
            return Ok(result);
        }

        // Collect items into a Vec for easier processing
        let mut items: Vec<Value> = Vec::new();
        let mut cursor = list;
        while cursor.is_cons() {
            items.push(cursor.cons_car());
            cursor = cursor.cons_cdr();
        }

        // Check if first element is the symbol `hash-table`
        let is_hash_table = items
            .first()
            .is_some_and(|v| v.is_symbol_named("hash-table"));

        if is_hash_table {
            // Parse keyword args from the list
            let mut test = HashTableTest::Eql;
            let mut data_pairs: Vec<(Value, Value)> = Vec::new();
            let mut size: i64 = 0;
            let mut i = 1;
            while i < items.len() {
                let kw_name = if let Some(id) = items[i].as_keyword_id() {
                    Some(resolve_sym(id).to_string())
                } else if let Some(name) = items[i].as_symbol_name() {
                    Some(name.to_string())
                } else {
                    None
                };
                if let Some(kw_name) = kw_name {
                    if i + 1 < items.len() {
                        match kw_name.trim_start_matches(':') {
                            "test" => {
                                if let Some(sym_name) = items[i + 1].as_symbol_name() {
                                    test = match sym_name {
                                        "eq" => HashTableTest::Eq,
                                        "eql" => HashTableTest::Eql,
                                        "equal" => HashTableTest::Equal,
                                        _ => HashTableTest::Eql,
                                    };
                                }
                                i += 2;
                            }
                            "size" => {
                                if let Some(n) = items[i + 1].as_fixnum() {
                                    size = n;
                                }
                                i += 2;
                            }
                            "data" => {
                                // data value is a list of alternating key-value pairs
                                let data_list = items[i + 1];
                                let mut data_cursor = data_list;
                                while data_cursor.is_cons() {
                                    let key = data_cursor.cons_car();
                                    data_cursor = data_cursor.cons_cdr();
                                    if data_cursor.is_cons() {
                                        let val = data_cursor.cons_car();
                                        data_cursor = data_cursor.cons_cdr();
                                        data_pairs.push((key, val));
                                    }
                                }
                                i += 2;
                            }
                            _ => {
                                i += 2; // skip unknown keywords
                            }
                        }
                    } else {
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }

            let ht_value =
                build_hash_table_literal_value(test, None, size, None, 1.5, 0.8125, data_pairs);
            restore_scratch_gc_roots(saved);
            return Ok(ht_value);
        }

        // Not a hash-table — it's a record #s(type field1 field2 ...)
        if !items.is_empty() {
            let record_value = Value::make_record(items);
            restore_scratch_gc_roots(saved);
            return Ok(record_value);
        }

        // Fallback for empty
        let result = Value::list(vec![
            Value::symbol("make-hash-table-from-literal"),
            Value::list(vec![Value::symbol("quote"), list]),
        ]);
        restore_scratch_gc_roots(saved);
        Ok(result)
    }

    // -- Atoms (numbers, symbols) --------------------------------------------

    fn read_atom(&mut self) -> Result<Value, ReadError> {
        let (token, had_escape) = self.read_symbol_token();

        if token.is_empty() {
            return Err(self.error("expected atom"));
        }

        // Keywords (:foo) — including bare `:` which is a keyword in Emacs
        if token.starts_with(':') {
            return Ok(Value::keyword(&token));
        }

        // Try integer. Funnel through Value::make_integer so a value
        // that fits in i64 but not in fixnum (62-bit) is promoted to
        // a bignum, matching GNU `string_to_number` behavior. On i64
        // overflow, fall through to a rug::Integer parse so true
        // bignum literals work.
        if looks_like_integer(&token) {
            if let Ok(n) = token.parse::<i64>() {
                return Ok(Value::make_integer(rug::Integer::from(n)));
            }
            if let Ok(parsed) = rug::Integer::parse(&token) {
                return Ok(Value::make_integer(rug::Integer::from(parsed)));
            }
        }

        // Try float — handles 1.5, 1e10, .5, 1.5e-3, etc.
        if looks_like_float(&token) {
            if let Ok(f) = token.parse::<f64>() {
                return Ok(Value::make_float(f));
            }
            if let Some(f) = parse_emacs_special_float(&token) {
                return Ok(Value::make_float(f));
            }
        }

        // Hex integer: 0xFF
        if token.starts_with("0x") || token.starts_with("0X") {
            if let Ok(n) = i64::from_str_radix(&token[2..], 16) {
                return Ok(Value::make_integer(rug::Integer::from(n)));
            }
        }

        // t and nil
        if token == "t" {
            return Ok(Value::T);
        }
        if token == "nil" {
            return Ok(Value::NIL);
        }

        // Emacs reader shorthand: bare ## reads as the symbol with empty name.
        if token == "##" && !had_escape {
            return Ok(Value::from_sym_id(intern("")));
        }

        Ok(Value::from_sym_id(intern(&token)))
    }

    fn read_symbol_token(&mut self) -> (String, bool) {
        let mut token = String::new();
        let mut had_escape = false;
        while let Some(ch) = self.current() {
            if ch.is_ascii_whitespace()
                || matches!(ch, '(' | ')' | '[' | ']' | '\'' | '`' | ',' | '"' | ';')
            {
                break;
            }
            if ch == '\\' {
                had_escape = true;
                self.bump();
                match self.current() {
                    Some(escaped) => {
                        token.push(escaped);
                        self.bump();
                    }
                    None => token.push('\\'),
                }
                continue;
            }
            token.push(ch);
            self.bump();
        }
        (token, had_escape)
    }

    // -- Helpers -------------------------------------------------------------

    fn expect(&mut self, expected: char) -> Result<(), ReadError> {
        match self.current() {
            Some(ch) if ch == expected => {
                self.bump();
                Ok(())
            }
            _ => Err(self.error(&format!("expected '{}'", expected))),
        }
    }

    fn current(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.input[self.pos..].chars().nth(offset)
    }

    fn bump(&mut self) {
        if let Some(ch) = self.current() {
            self.pos += ch.len_utf8();
        }
    }

    fn error(&self, message: &str) -> ReadError {
        ReadError {
            position: self.pos,
            message: message.to_string(),
        }
    }

    fn error_after_current(&mut self, message: &str) -> ReadError {
        if self.current().is_some() {
            self.bump();
        }
        self.error(message)
    }

    fn parse_decimal_usize(&mut self) -> Result<usize, ReadError> {
        let start = self.pos;
        while matches!(self.current(), Some(c) if c.is_ascii_digit()) {
            self.bump();
        }
        if self.pos == start {
            return Err(self.error("expected decimal length"));
        }
        self.input[start..self.pos]
            .parse::<usize>()
            .map_err(|_| self.error("invalid decimal length"))
    }

    /// Advance `pos` past `len` source bytes from a `.elc` file.
    ///
    /// `.elc` bytes are Latin-1-decoded into a Rust `String` so that every
    /// source byte (including raw 0x80..=0xFF) becomes exactly one `char`.
    /// `#@LEN` skips count source bytes, not UTF-8 bytes, so we advance by
    /// `len` chars and let each char contribute its actual UTF-8 width to
    /// `pos`. A naive byte-wise advance would under-skip by 1 for every
    /// 0x80..=0xFF source byte (which becomes a 2-byte UTF-8 sequence in
    /// our `String`) and land mid-docstring on files like `window.elc`,
    /// where docstrings contain U+2019 (`'`) stored as `0xe2 0x80 0x99`.
    fn skip_exact_source_bytes(&mut self, len: usize) -> Result<(), ReadError> {
        let mut chars = self.input[self.pos..].chars();
        let mut bytes_advanced = 0usize;
        for _ in 0..len {
            match chars.next() {
                Some(c) => bytes_advanced += c.len_utf8(),
                None => return Err(self.error("byte skip past end of input")),
            }
        }
        self.pos += bytes_advanced;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Free functions (copied from parser.rs)
// ---------------------------------------------------------------------------

fn is_char_literal_delimiter(ch: char) -> bool {
    (ch as u32) <= 32
        || matches!(
            ch,
            '"' | '\'' | ';' | '(' | ')' | '[' | ']' | '#' | '?' | '`' | ',' | '.'
        )
}

fn looks_like_float(s: &str) -> bool {
    let s = if s.starts_with('+') || s.starts_with('-') {
        &s[1..]
    } else {
        s
    };
    if s.is_empty() {
        return false;
    }
    let first = s.as_bytes()[0];
    if !first.is_ascii_digit() && first != b'.' {
        return false;
    }
    s.contains('.') || s.contains('e') || s.contains('E')
}

/// True if `s` is a plain decimal integer literal (with optional sign)
/// — i.e. would parse as either an i64 or a bignum but never as a
/// float. We use this to gate the bignum-fallback path so we don't
/// trip on tokens that contain `e`/`E`/`.` (those are floats) or that
/// aren't numeric at all (those are symbols).
fn looks_like_integer(s: &str) -> bool {
    let s = if s.starts_with('+') || s.starts_with('-') {
        &s[1..]
    } else {
        s
    };
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn parse_emacs_special_float(token: &str) -> Option<f64> {
    const NAN_QUIET_BIT: u64 = 1u64 << 51;
    const NAN_PAYLOAD_MASK: u64 = (1u64 << 51) - 1;
    const NAN_LEADING_DOT_PAYLOAD: u64 = 2_251_799_813_685_246;

    let exp_idx = token.find(['e', 'E'])?;
    let (mantissa, exponent_suffix) = token.split_at(exp_idx);
    let suffix = &exponent_suffix[1..];
    match suffix {
        "+INF" => {
            let mantissa = mantissa.parse::<f64>().ok()?;
            if !mantissa.is_finite() {
                return None;
            }
            Some(if mantissa.is_sign_negative() {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            })
        }
        "+NaN" => {
            let mantissa_value = mantissa.parse::<f64>().ok()?;
            if !mantissa_value.is_finite() {
                return None;
            }

            let body = mantissa
                .strip_prefix('+')
                .or_else(|| mantissa.strip_prefix('-'))
                .unwrap_or(mantissa);

            let mut payload = 0u64;
            if body.starts_with('.') {
                payload = NAN_LEADING_DOT_PAYLOAD;
            } else {
                let integer_part = body
                    .split_once('.')
                    .map(|(int_part, _)| int_part)
                    .unwrap_or(body);
                let mut any_nonzero = false;
                for digit in integer_part.bytes() {
                    if !digit.is_ascii_digit() {
                        return None;
                    }
                    let value = (digit - b'0') as u64;
                    any_nonzero |= value != 0;
                    payload = ((payload * 10) + value) & NAN_PAYLOAD_MASK;
                }
                if !any_nonzero {
                    payload = 0;
                }
            }

            if payload == 0 {
                return Some(if mantissa_value.is_sign_negative() {
                    -f64::NAN
                } else {
                    f64::NAN
                });
            }

            let sign = if mantissa_value.is_sign_negative() {
                1u64 << 63
            } else {
                0
            };
            let bits = sign | (0x7ffu64 << 52) | NAN_QUIET_BIT | (payload & NAN_PAYLOAD_MASK);
            Some(f64::from_bits(bits))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Latin-1 → UTF-8 recombination for .elc string constants
// ---------------------------------------------------------------------------

/// Re-decode a string that may contain Latin-1 codepoints (0x80–0xFF)
/// which are actually UTF-8 byte sequences decomposed by the `.elc`
/// loader's `b as char` mapping.
///
/// `.elc` files are loaded as Latin-1 (`load.rs:1418`) because bytecode
/// instruction strings contain raw bytes 0x00–0xFF that aren't valid
/// UTF-8. However, this also decomposes multibyte string *constants*
/// — e.g., the 3-byte UTF-8 for U+2018 (LEFT SINGLE QUOTATION MARK)
/// becomes three Latin-1 codepoints U+00E2 U+0080 U+0098.
///
/// This function detects strings whose chars ≤ U+00FF form valid UTF-8
/// when treated as raw bytes, and recombines them into proper Unicode
/// codepoints. Strings that are pure ASCII or contain chars > U+00FF
/// are returned unchanged. Strings whose bytes don't form valid UTF-8
/// (genuine unibyte/bytecode data) are also returned unchanged.
///
/// This mirrors GNU Emacs `lread.c` which reads `.elc` strings in
/// unibyte mode and then re-encodes multibyte strings via
/// `string_to_multibyte`.
/// Build a LispString from reader-produced bytes (Emacs internal encoding).
///
/// GNU reads ordinary source string literals as unibyte when the contents are
/// pure ASCII, even though the same bytes could be represented as multibyte
/// UTF-8. Keep that canonicalization here so `(intern "foo")` and
/// `(intern (string-to-multibyte "foo"))` name the same symbol.
///
/// Non-ASCII reader bytes stay multibyte and go through `from_emacs_bytes`
/// so Emacs internal encoding still counts characters correctly.
fn maybe_recombine_latin1_emacs(data: Vec<u8>) -> crate::heap_types::LispString {
    if data.is_empty() || data.iter().all(|&b| b < 0x80) {
        return crate::heap_types::LispString::from_unibyte(data);
    }
    // The bytes are in Emacs internal encoding (may contain C0/C1 overlong
    // for raw bytes, or standard multi-byte UTF-8 for Unicode).
    crate::heap_types::LispString::from_emacs_bytes(data)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "value_reader_test.rs"]
mod tests;
