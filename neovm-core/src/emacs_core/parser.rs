//! Lisp reader / parser.
//!
//! Supports: integers, floats, strings (with escapes), symbols, keywords,
//! uninterned symbols (`#:foo`), character literals (?a), lists, dotted pairs,
//! vectors, quote ('), function (#'), backquote (`), unquote (,), splice (,@),
//! line comments (;), block comments (#|..|#).

use super::expr::{Expr, ParseError};
use super::intern::{intern, intern_uninterned, resolve_sym};
use super::string_escape::{bytes_to_unibyte_storage_string, encode_nonunicode_char_for_storage};

pub fn parse_forms(input: &str) -> Result<Vec<Expr>, ParseError> {
    let mut parser = Parser::new(input);
    let mut forms = Vec::new();
    while parser.skip_ws_and_comments() {
        forms.push(parser.parse_expr()?);
    }
    Ok(forms)
}

pub fn parse_form(input: &str) -> Result<Option<(Expr, usize)>, ParseError> {
    let mut parser = Parser::new(input);
    if !parser.skip_ws_and_comments() {
        return Ok(None);
    }
    let expr = parser.parse_expr()?;
    Ok(Some((expr, parser.pos)))
}

struct Parser<'a> {
    input: &'a str,
    pos: usize,
    /// `#N=EXPR` / `#N#` read labels for shared structure in `.elc` files.
    read_labels: std::collections::HashMap<usize, Expr>,
}

impl<'a> Parser<'a> {
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

    // -- Main parse dispatch -------------------------------------------------

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.skip_ws_and_comments();
        let Some(ch) = self.current() else {
            return Err(self.error("unexpected end of input"));
        };

        match ch {
            '(' => self.parse_list_or_dotted(),
            ')' => {
                self.bump();
                Err(self.error(")"))
            }
            '[' => self.parse_vector(),
            '\'' => {
                self.bump();
                let quoted = self.parse_expr()?;
                Ok(Expr::List(vec![Expr::Symbol(intern("quote")), quoted]))
            }
            '`' => {
                self.bump();
                let quoted = self.parse_expr()?;
                Ok(Expr::List(vec![Expr::Symbol(intern("`")), quoted]))
            }
            ',' => {
                self.bump();
                if self.current() == Some('@') {
                    self.bump();
                    let expr = self.parse_expr()?;
                    Ok(Expr::List(vec![Expr::Symbol(intern(",@")), expr]))
                } else {
                    let expr = self.parse_expr()?;
                    Ok(Expr::List(vec![Expr::Symbol(intern(",")), expr]))
                }
            }
            '"' => self.parse_string(),
            '?' => self.parse_char_literal(),
            '#' => self.parse_hash_syntax(),
            _ => self.parse_atom(),
        }
    }

    // -- Lists and dotted pairs ----------------------------------------------

    fn parse_list_or_dotted(&mut self) -> Result<Expr, ParseError> {
        self.expect('(')?;
        let mut items = Vec::new();
        loop {
            self.skip_ws_and_comments();
            match self.current() {
                Some(')') => {
                    self.bump();
                    return Ok(Expr::List(items));
                }
                Some('.') if self.is_dot_separator() => {
                    // Dotted pair
                    self.bump(); // consume '.'
                    let cdr = self.parse_expr()?;
                    self.skip_ws_and_comments();
                    match self.current() {
                        Some(')') => {
                            self.bump();
                            return Ok(Expr::DottedList(items, Box::new(cdr)));
                        }
                        _ => return Err(self.error("expected ')' after dotted pair")),
                    }
                }
                Some(_) => items.push(self.parse_expr()?),
                None => return Err(self.error("unterminated list")),
            }
        }
    }

    /// Check if current '.' is a dot separator (not part of a number like 1.5).
    fn is_dot_separator(&self) -> bool {
        // A dot is a separator if the next char is whitespace, ')', or EOF
        match self.peek_at(1) {
            None => true,
            Some(c) => c.is_ascii_whitespace() || c == ')' || c == '(' || c == ';',
        }
    }

    // -- Vectors [1 2 3] ----------------------------------------------------

    fn parse_vector(&mut self) -> Result<Expr, ParseError> {
        self.expect('[')?;
        let mut items = Vec::new();
        loop {
            self.skip_ws_and_comments();
            match self.current() {
                Some(']') => {
                    self.bump();
                    return Ok(Expr::Vector(items));
                }
                Some(_) => items.push(self.parse_expr()?),
                None => return Err(self.error("unterminated vector")),
            }
        }
    }

    // -- Strings "..." -------------------------------------------------------

    fn parse_string(&mut self) -> Result<Expr, ParseError> {
        self.expect('"')?;
        let mut s = String::new();
        loop {
            let Some(ch) = self.current() else {
                return Err(self.error("unterminated string"));
            };
            self.bump();
            match ch {
                '"' => return Ok(Expr::Str(s)),
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
                        // Modifier escapes in strings — produce characters with
                        // modifier bits, matching GNU Emacs lread.c.
                        // \s-X: super modifier (check before plain 's' = space)
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
                            // Emacs accepts \x escapes up to 0x3FFFFF (22-bit char space)
                            // including modifier bits and sentinel values above Unicode.
                            // For valid Unicode, push directly. For extended values,
                            // store the raw value using push_emacs_extended_char.
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
    fn parse_string_char_value(&mut self, modifiers: u32) -> Result<u32, ParseError> {
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
    /// These are Emacs-internal characters used as sentinel values, modifier-bit
    /// carriers, etc. We encode them using a private-use Unicode placeholder
    /// since Rust strings require valid UTF-8.
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
    /// For chars with modifier bits that can't be represented as Unicode,
    /// we encode meta as char|0x80 for 7-bit ASCII (matching Emacs unibyte),
    /// or store as the raw u32 value using Rust's char::from_u32 (which may
    /// produce high Unicode codepoints for modifier combos).
    fn push_modified_char(s: &mut String, val: u32) {
        let meta = val & (1 << 27) != 0;
        let base = val & !(1u32 << 27); // strip meta bit
        if meta && base < 128 {
            s.push_str(&bytes_to_unibyte_storage_string(&[(base | 0x80) as u8]));
        } else if let Some(c) = char::from_u32(val & 0x3FFFFF) {
            // For non-meta modifiers (ctrl, shift, etc.), the base char
            // should already be in valid Unicode range
            s.push(c);
        } else {
            Self::push_emacs_extended_char(s, val & 0x3FFFFF);
        }
    }

    fn read_hex_digits(&mut self) -> Result<(u32, usize), ParseError> {
        let start = self.pos;
        while let Some(c) = self.current() {
            if c.is_ascii_hexdigit() {
                self.bump();
            } else {
                // Emacs \\x terminates at first non-hex or at ';'
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

    fn read_fixed_hex(&mut self, count: usize) -> Result<u32, ParseError> {
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

    fn read_unicode_name_escape(&mut self) -> Result<u32, ParseError> {
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

    fn parse_char_literal(&mut self) -> Result<Expr, ParseError> {
        self.expect('?')?;
        if matches!(self.current(), Some(' ' | '\t')) {
            let ch = self.current().expect("matched whitespace char literal");
            self.bump();
            return Ok(Expr::Char(ch));
        }

        let val = self.parse_char_value(0)?;
        if matches!(self.current(), Some(ch) if !is_char_literal_delimiter(ch)) {
            return Err(self.error("?"));
        }
        // Character literals with modifier bits (control, meta, etc.) produce
        // values beyond the Unicode range.  Emit them as integers, matching
        // GNU Emacs where characters ARE integers.
        if let Some(c) = char::from_u32(val) {
            Ok(Expr::Char(c))
        } else {
            Ok(Expr::Int(val as i64))
        }
    }

    /// Parse the value part of a character literal, accumulating modifier bits.
    /// Handles recursive modifiers like \M-\C-x and escape sequences after modifiers.
    fn parse_char_value(&mut self, modifiers: u32) -> Result<u32, ParseError> {
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
                'v' => 0x0B, // VT (vertical tab)
                'e' => 0x1B, // ESC
                'd' => 0x7F, // DEL
                // \s: space UNLESS followed by '-' (then Super modifier)
                's' if self.current() == Some('-') => {
                    self.bump(); // consume '-'
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
                // Modifier keys — recurse to handle chained modifiers like \M-\C-x
                // Aligned with GNU Emacs lread.c read_char_escape 'C' handler:
                //   if base == '?': result = DEL
                //   elif base & 0x40: result = base & 0x1F (letter control mapping)
                //   else: result = base (e.g. \C-\0 keeps base=0)
                //   Always set control bit (1 << 26).
                'C' if self.current() == Some('-') => {
                    // GNU Emacs lread.c control modifier rules:
                    //   \C-X where X in [A-Z a-z @ [ \ ] ^ _] → X & 0x1F (no ctrl bit)
                    //   \C-? → DEL (0x7F, no ctrl bit)
                    //   \C-X otherwise → X | CHAR_CTL
                    self.bump(); // consume '-'
                    let base = self.parse_char_value(modifiers)?;
                    let base_char = base & 0x3FFFFF; // char code without modifier bits
                    let existing_mods = base & 0xFC00000; // modifier bits from inner calls
                    if base_char == 0x3F {
                        // '?' -> DEL
                        return Ok(0x7F | existing_mods);
                    } else if (base_char >= 0x40 && base_char <= 0x5F)
                        || (base_char >= 0x61 && base_char <= 0x7A)
                    {
                        // [A-Z @ [ \ ] ^ _] or [a-z]: ASCII control mapping
                        return Ok((base_char & 0x1F) | existing_mods);
                    } else {
                        // Everything else: add ctrl_modifier bit
                        return Ok(base_char | existing_mods | (1u32 << 26));
                    }
                }
                'M' if self.current() == Some('-') => {
                    self.bump(); // consume '-'
                    return self.parse_char_value(modifiers | (1 << 27)); // meta bit
                }
                'S' if self.current() == Some('-') => {
                    self.bump(); // consume '-'
                    return self.parse_char_value(modifiers | (1 << 25)); // shift bit
                }
                'A' if self.current() == Some('-') => {
                    self.bump(); // consume '-'
                    return self.parse_char_value(modifiers | (1 << 22)); // alt bit
                }
                'H' if self.current() == Some('-') => {
                    self.bump(); // consume '-'
                    return self.parse_char_value(modifiers | (1 << 24)); // hyper bit
                }
                '^' => {
                    // \^X — traditional caret control, maps to ASCII 0-31 range.
                    // Unlike \C-, does NOT set the CHAR_CTL modifier bit.
                    // Matches GNU Emacs lread.c behavior.
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

    fn parse_hash_syntax(&mut self) -> Result<Expr, ParseError> {
        self.expect('#')?;
        let Some(ch) = self.current() else {
            return Err(self.error("#"));
        };

        match ch {
            '\'' => {
                // #'function
                self.bump();
                let expr = self.parse_expr()?;
                Ok(Expr::List(vec![Expr::Symbol(intern("function")), expr]))
            }
            '(' => {
                // #("string" START END (PROPS...) ...) — propertized string.
                // Parse all elements, extract the string (first element), and
                // discard text properties for now (Expr has no property slot).
                // Official Emacs reader calls Fset_text_properties on the string;
                // we return just the bare string since Expr::Str can't carry props.
                let list = self.parse_list_or_dotted()?;
                match list {
                    Expr::List(ref items) if !items.is_empty() => {
                        if let Expr::Str(_) = &items[0] {
                            Ok(items[0].clone())
                        } else {
                            Err(self.error("#(: first element must be a string"))
                        }
                    }
                    _ => Err(self.error("#(: expected propertized string")),
                }
            }
            '[' => {
                // #[...] — compiled-function literal in .elc.
                let vector = self.parse_vector()?;
                Ok(Expr::List(vec![
                    Expr::Symbol(intern("byte-code-literal")),
                    vector,
                ]))
            }
            '@' => {
                // #@N<bytes> — reader skip used by .elc for inline data blocks.
                self.parse_hash_skip_bytes()
            }
            ':' => {
                // #:X — uninterned symbol.
                self.bump();
                let (token, _) = self.read_symbol_token();
                Ok(Expr::Symbol(intern_uninterned(&token)))
            }
            '$' => {
                // #$ — expands to the current load file name during read.
                // Preserve it as a dedicated reader object so quote/bytecode
                // construction can substitute the runtime file name instead of
                // collapsing it to the plain symbol `load-file-name`.
                self.bump();
                Ok(Expr::ReaderLoadFileName)
            }
            '#' => {
                // ## — symbol with empty name.
                self.bump();
                Ok(Expr::Symbol(intern("")))
            }
            'b' | 'B' => {
                // #b... binary integer
                self.bump();
                self.parse_radix_number(2)
            }
            'o' | 'O' => {
                // #o... octal integer
                self.bump();
                self.parse_radix_number(8)
            }
            'x' | 'X' => {
                // #x... hex integer
                self.bump();
                self.parse_radix_number(16)
            }
            's' => {
                // #s(hash-table ...) — simplified reader
                self.bump();
                if self.current() == Some('(') {
                    self.parse_hash_table_literal()
                } else {
                    Err(self.error("#s"))
                }
            }
            '&' => {
                // #&SIZE"DATA" — bool-vector literal.
                // SIZE is the number of bits; DATA is a string with packed bytes.
                self.bump();
                let size = self.parse_bool_vector_size()?;
                let data = self.parse_string()?;
                let data_str = match data {
                    Expr::Str(s) => s,
                    _ => return Err(self.error("#& expected string after size")),
                };
                // Expand packed bytes to individual bits and emit as
                // (bool-vector t nil t ...) — the builtin uses truthiness.
                let t_sym = intern("t");
                let nil_sym = intern("nil");
                let mut call = Vec::with_capacity(1 + size);
                call.push(Expr::Symbol(intern("bool-vector")));
                let mut bit_count = 0;
                for byte_val in data_str.bytes() {
                    for bit_idx in 0..8 {
                        if bit_count >= size {
                            break;
                        }
                        if (byte_val >> bit_idx) & 1 != 0 {
                            call.push(Expr::Symbol(t_sym));
                        } else {
                            call.push(Expr::Symbol(nil_sym));
                        }
                        bit_count += 1;
                    }
                }
                // Pad with nil if data is shorter than SIZE
                while bit_count < size {
                    call.push(Expr::Symbol(nil_sym));
                    bit_count += 1;
                }
                Ok(Expr::List(call))
            }
            '0'..='9' => {
                // #N=EXPR defines read label N, #N# references it.
                // Used in .elc for shared/circular structures.
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
                        let expr = self.parse_expr()?;
                        self.read_labels.insert(n, expr.clone());
                        Ok(expr)
                    }
                    Some('#') => {
                        // #N# — reference previously defined label N
                        self.bump();
                        self.read_labels
                            .get(&n)
                            .cloned()
                            .ok_or_else(|| self.error(&format!("#{n}#: undefined read label")))
                    }
                    _ => Err(self.error(&format!("#{n}"))),
                }
            }
            _ => Err(self.error_after_current(&format!("#{}", ch))),
        }
    }

    fn parse_hash_skip_bytes(&mut self) -> Result<Expr, ParseError> {
        self.expect('@')?;
        if !matches!(self.current(), Some(c) if c.is_ascii_digit()) {
            return Err(self.error("end of input"));
        }
        let len = self.parse_decimal_usize()?;
        self.skip_exact_bytes(len)?;
        self.parse_expr()
    }

    /// Parse the decimal SIZE in `#&SIZE"DATA"`.
    fn parse_bool_vector_size(&mut self) -> Result<usize, ParseError> {
        if !matches!(self.current(), Some(c) if c.is_ascii_digit()) {
            return Err(self.error("#& expected decimal size"));
        }
        self.parse_decimal_usize()
    }

    fn parse_radix_number(&mut self, radix: u32) -> Result<Expr, ParseError> {
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
        Ok(Expr::Int(if negative { -val } else { val }))
    }

    fn parse_hash_table_literal(&mut self) -> Result<Expr, ParseError> {
        // #s(hash-table size N test T data (k1 v1 k2 v2 ...))
        // GNU Emacs reader creates an actual hash table value during reading.
        // We parse the keyword arguments and construct a Value::HashTable
        // wrapped in Expr::OpaqueValueRef so quoting works correctly.
        let list = self.parse_list_or_dotted()?;

        // Check if this is a hash-table or a record/struct
        let items = match &list {
            Expr::List(items) => items,
            _ => {
                // Fallback: emit as code for eval-time construction
                return Ok(Expr::List(vec![
                    Expr::Symbol(intern("make-hash-table-from-literal")),
                    Expr::List(vec![Expr::Symbol(intern("quote")), list]),
                ]));
            }
        };

        if items
            .first()
            .is_some_and(|e| matches!(e, Expr::Symbol(id) if resolve_sym(*id) == "hash-table"))
        {
            // Parse keyword args from the list
            let mut test = super::value::HashTableTest::Eql;
            let mut data_pairs: Vec<(Expr, Expr)> = Vec::new();
            let mut size: i64 = 0;
            let mut i = 1;
            while i < items.len() {
                // GNU hash table read syntax uses bare symbols (test, data, size)
                // not keywords (:test, :data, :size). Handle both forms.
                let kw_name = match &items[i] {
                    Expr::Keyword(kw) => Some(resolve_sym(*kw).to_string()),
                    Expr::Symbol(sym) => Some(resolve_sym(*sym).to_string()),
                    _ => None,
                };
                if let Some(kw_name) = kw_name {
                    if i + 1 < items.len() {
                        match kw_name.trim_start_matches(':') {
                            "test" => {
                                if let Expr::Symbol(id) = &items[i + 1] {
                                    test = match resolve_sym(*id) {
                                        "eq" => super::value::HashTableTest::Eq,
                                        "eql" => super::value::HashTableTest::Eql,
                                        "equal" => super::value::HashTableTest::Equal,
                                        _ => super::value::HashTableTest::Eql,
                                    };
                                }
                                i += 2;
                            }
                            "size" => {
                                if let Expr::Int(n) = &items[i + 1] {
                                    size = *n;
                                }
                                i += 2;
                            }
                            "data" => {
                                if let Expr::List(pairs) = &items[i + 1] {
                                    let mut j = 0;
                                    while j + 1 < pairs.len() {
                                        data_pairs.push((pairs[j].clone(), pairs[j + 1].clone()));
                                        j += 2;
                                    }
                                }
                                i += 2;
                            }
                            _ => {
                                i += 2;
                            } // skip unknown keywords
                        }
                    } else {
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }

            // Construct the hash table value directly during reading,
            // matching GNU Emacs reader behavior.
            use super::value::Value;
            let ht_value = Value::hash_table(test);
            if size > 0 {
                let _ = ht_value.with_hash_table_mut(|ht| {
                    ht.size = size;
                });
            }
            // Insert key-value pairs
            for (k_expr, v_expr) in &data_pairs {
                let key = super::eval::quote_to_value(k_expr);
                let val = super::eval::quote_to_value(v_expr);
                let _ = ht_value.with_hash_table_mut(|ht| {
                    let ht_test = ht.test.clone();
                    let hash_key = key.to_hash_key(&ht_test);
                    ht.data.insert(hash_key.clone(), val);
                    ht.key_snapshots.insert(hash_key.clone(), key);
                    ht.insertion_order.push(hash_key);
                });
            }
            return Ok(Expr::OpaqueValueRef(
                super::eval::OPAQUE_POOL.with(|pool| pool.borrow_mut().insert(ht_value)),
            ));
        }

        // Not a hash-table — it's a record #s(type field1 field2 ...)
        // Create a Record value directly during reading, matching GNU reader.
        if let Expr::List(items) = &list {
            if !items.is_empty() {
                use super::value::Value;
                let vals: Vec<Value> = items.iter().map(super::eval::quote_to_value).collect();
                let record_value = Value::make_record(vals);
                return Ok(Expr::OpaqueValueRef(
                    super::eval::OPAQUE_POOL.with(|pool| pool.borrow_mut().insert(record_value)),
                ));
            }
        }
        // Fallback for empty or non-list
        Ok(Expr::List(vec![
            Expr::Symbol(intern("make-hash-table-from-literal")),
            Expr::List(vec![Expr::Symbol(intern("quote")), list]),
        ]))
    }

    // -- Atoms (numbers, symbols) --------------------------------------------

    fn parse_atom(&mut self) -> Result<Expr, ParseError> {
        let (token, had_escape) = self.read_symbol_token();

        if token.is_empty() {
            return Err(self.error("expected atom"));
        }

        // Keywords (:foo) — including bare `:` which is a keyword in Emacs
        if token.starts_with(':') {
            return Ok(Expr::Keyword(intern(&token)));
        }

        // Try integer
        if let Ok(n) = token.parse::<i64>() {
            return Ok(Expr::Int(n));
        }

        // Try float — handles 1.5, 1e10, .5, 1.5e-3, etc.
        if looks_like_float(&token) {
            if let Ok(f) = token.parse::<f64>() {
                return Ok(Expr::Float(f));
            }
            if let Some(f) = parse_emacs_special_float(&token) {
                return Ok(Expr::Float(f));
            }
        }

        // Hex integer: 0xFF
        if token.starts_with("0x") || token.starts_with("0X") {
            if let Ok(n) = i64::from_str_radix(&token[2..], 16) {
                return Ok(Expr::Int(n));
            }
        }

        // Boolean
        if token == "nil" || token == "t" {
            return Ok(Expr::Symbol(intern(&token)));
        }

        // Emacs reader shorthand: bare ## reads as the symbol with empty name.
        if token == "##" && !had_escape {
            return Ok(Expr::Symbol(intern("")));
        }

        Ok(Expr::Symbol(intern(&token)))
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

    fn expect(&mut self, expected: char) -> Result<(), ParseError> {
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

    fn error(&self, message: &str) -> ParseError {
        ParseError {
            position: self.pos,
            message: message.to_string(),
        }
    }

    fn error_after_current(&mut self, message: &str) -> ParseError {
        if self.current().is_some() {
            self.bump();
        }
        self.error(message)
    }

    fn parse_decimal_usize(&mut self) -> Result<usize, ParseError> {
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

    fn skip_exact_bytes(&mut self, len: usize) -> Result<(), ParseError> {
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

fn is_char_literal_delimiter(ch: char) -> bool {
    (ch as u32) <= 32
        || matches!(
            ch,
            '"' | '\'' | ';' | '(' | ')' | '[' | ']' | '#' | '?' | '`' | ',' | '.'
        )
}

fn looks_like_float(s: &str) -> bool {
    // Must contain a decimal point or exponent marker, and not be purely a symbol
    let s = if s.starts_with('+') || s.starts_with('-') {
        &s[1..]
    } else {
        s
    };
    if s.is_empty() {
        return false;
    }
    // Must start with a digit or '.'
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
#[path = "parser_test.rs"]
mod tests;
