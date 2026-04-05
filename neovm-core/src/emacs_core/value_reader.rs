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
use super::string_escape::{bytes_to_unibyte_storage_string, encode_nonunicode_char_for_storage};
use std::cell::RefCell;

thread_local! {
    /// Current load-file-name for `#$` reader macro.
    /// Set by `with_load_context` in load.rs before reading a file.
    static READER_LOAD_FILE_NAME: RefCell<Option<Value>> = const { RefCell::new(None) };
}

/// Set the current load-file-name for the `#$` reader macro.
pub fn set_reader_load_file_name(value: Option<Value>) {
    READER_LOAD_FILE_NAME.with(|slot| *slot.borrow_mut() = value);
}

/// Get the current load-file-name for the `#$` reader macro.
fn get_reader_load_file_name() -> Value {
    READER_LOAD_FILE_NAME.with(|slot| {
        slot.borrow().unwrap_or(Value::NIL)
    })
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
    let mut reader = Reader::new(input);
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
    let mut reader = Reader::new(input);
    reader.pos = start;
    if !reader.skip_ws_and_comments() {
        return Ok(None);
    }
    let value = reader.read_form()?;
    Ok(Some((value, reader.pos)))
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
    pos: usize,
    /// `#N=EXPR` / `#N#` read labels for shared structure in `.elc` files.
    read_labels: std::collections::HashMap<usize, Value>,
}

impl<'a> Reader<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            pos: 0,
            read_labels: std::collections::HashMap::new(),
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
        let Some(ch) = self.current() else {
            return Err(self.error("unexpected end of input"));
        };

        match ch {
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
        let mut s = String::new();
        loop {
            let Some(ch) = self.current() else {
                return Err(self.error("unterminated string"));
            };
            self.bump();
            match ch {
                '"' => return Ok(Value::string(s)),
                '\\' => {
                    let Some(esc) = self.current() else {
                        return Err(self.error("unterminated escape in string"));
                    };
                    self.bump();
                    match esc {
                        'n' => s.push('\n'),
                        'r' => s.push('\r'),
                        't' => s.push('\t'),
                        '\\' => s.push('\\'),
                        '"' => s.push('"'),
                        'a' => s.push('\x07'), // bell
                        'b' => s.push('\x08'), // backspace
                        'f' => s.push('\x0C'), // form feed
                        'e' => s.push('\x1B'), // escape
                        'v' => s.push('\x0B'), // vertical tab
                        // Modifier escapes in strings
                        's' if self.current() == Some('-') => {
                            self.bump(); // consume '-'
                            let val = self.parse_string_char_value(1 << 23)?;
                            Self::push_modified_char(&mut s, val);
                        }
                        's' => s.push(' '), // space
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
                            Self::push_modified_char(&mut s, result);
                        }
                        'M' if self.current() == Some('-') => {
                            self.bump(); // consume '-'
                            let val = self.parse_string_char_value(1 << 27)?;
                            Self::push_modified_char(&mut s, val);
                        }
                        'S' if self.current() == Some('-') => {
                            self.bump(); // consume '-'
                            let val = self.parse_string_char_value(1 << 25)?;
                            Self::push_modified_char(&mut s, val);
                        }
                        'A' if self.current() == Some('-') => {
                            self.bump(); // consume '-'
                            let val = self.parse_string_char_value(1 << 22)?;
                            Self::push_modified_char(&mut s, val);
                        }
                        'H' if self.current() == Some('-') => {
                            self.bump(); // consume '-'
                            let val = self.parse_string_char_value(1 << 24)?;
                            Self::push_modified_char(&mut s, val);
                        }
                        'd' => s.push('\x7F'), // delete
                        'x' => {
                            let (hex, digit_count) = self.read_hex_digits()?;
                            if digit_count < 3 && (0x80..0x100).contains(&hex) {
                                s.push_str(&bytes_to_unibyte_storage_string(&[hex as u8]));
                            } else if let Some(c) = char::from_u32(hex) {
                                s.push(c);
                            } else if hex <= 0x3FFFFF {
                                Self::push_emacs_extended_char(&mut s, hex);
                            } else {
                                return Err(self.error(
                                    "invalid codepoint in \\x escape (exceeds Emacs 22-bit limit)",
                                ));
                            }
                        }
                        'u' => {
                            let hex = self.read_fixed_hex(4)?;
                            if let Some(c) = char::from_u32(hex) {
                                s.push(c);
                            } else {
                                return Err(self.error("invalid unicode codepoint in \\u escape"));
                            }
                        }
                        'U' => {
                            let hex = self.read_fixed_hex(8)?;
                            if let Some(c) = char::from_u32(hex) {
                                s.push(c);
                            } else {
                                return Err(self.error("invalid unicode codepoint in \\U escape"));
                            }
                        }
                        'N' if self.current() == Some('{') => {
                            let value = self.read_unicode_name_escape()?;
                            if let Some(c) = char::from_u32(value) {
                                s.push(c);
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
                            if (0x80..0x100).contains(&val) {
                                s.push_str(&bytes_to_unibyte_storage_string(&[val as u8]));
                            } else if let Some(c) = char::from_u32(val) {
                                s.push(c);
                            } else if val <= 0x3FFFFF {
                                Self::push_emacs_extended_char(&mut s, val);
                            }
                        }
                        '\n' => {
                            // Line continuation — skip newline
                        }
                        other => {
                            // Unknown escape — keep the character
                            s.push(other);
                        }
                    }
                }
                other => s.push(other),
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
                    Ok((base as u32 & 0x1F) | modifiers)
                }
                other => Ok(other as u32 | modifiers),
            }
        } else {
            Ok(ch as u32 | modifiers)
        }
    }

    /// Push an Emacs extended character (above Unicode U+10FFFF) into a string.
    fn push_emacs_extended_char(s: &mut String, val: u32) {
        if let Some(encoded) = encode_nonunicode_char_for_storage(val) {
            s.push_str(&encoded);
        } else if let Some(c) = char::from_u32(val) {
            s.push(c);
        } else {
            s.push('\u{FFFD}');
        }
    }

    /// Push a character value (possibly with modifier bits) into a string.
    fn push_modified_char(s: &mut String, val: u32) {
        let meta = val & (1 << 27) != 0;
        let base = val & !(1u32 << 27); // strip meta bit
        if meta && base < 128 {
            s.push_str(&bytes_to_unibyte_storage_string(&[(base | 0x80) as u8]));
        } else if let Some(c) = char::from_u32(val & 0x3FFFFF) {
            s.push(c);
        } else {
            Self::push_emacs_extended_char(s, val & 0x3FFFFF);
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
                    let base_val = base as u32;
                    if base_val == 0x3F {
                        0x7F // '?' -> DEL
                    } else {
                        base_val & 0x1F
                    }
                }
                other => other as u32,
            };
            Ok(val | modifiers)
        } else {
            Ok(ch as u32 | modifiers)
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
                            crate::emacs_core::error::Flow::Signal(sig) => {
                                sig.data.first()
                                    .and_then(|v| v.as_str().map(str::to_owned))
                                    .unwrap_or_else(|| format!("{:?}", sig.data))
                            }
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
        self.skip_exact_bytes(len)?;
        self.read_form()
    }

    fn read_bool_vector_literal(&mut self) -> Result<Value, ReadError> {
        if !matches!(self.current(), Some(c) if c.is_ascii_digit()) {
            return Err(self.error("#& expected decimal size"));
        }
        let size = self.parse_decimal_usize()?;
        let data = self.read_string()?;
        let data_str = data
            .as_str()
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

        let val =
            i64::from_str_radix(&digits, radix).map_err(|_| self.error("invalid radix number"))?;
        Ok(Value::fixnum(if negative { -val } else { val }))
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

        // Try integer
        if let Ok(n) = token.parse::<i64>() {
            return Ok(Value::fixnum(n));
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
                return Ok(Value::fixnum(n));
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

    fn skip_exact_bytes(&mut self, len: usize) -> Result<(), ReadError> {
        let Some(new_pos) = self.pos.checked_add(len) else {
            return Err(self.error("byte skip overflow"));
        };
        if new_pos > self.input.len() {
            return Err(self.error("byte skip past end of input"));
        }
        if !self.input.is_char_boundary(new_pos) {
            return Err(self.error("byte skip ended mid-character"));
        }
        self.pos = new_pos;
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
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "value_reader_test.rs"]
mod tests;
