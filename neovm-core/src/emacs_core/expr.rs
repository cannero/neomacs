//! AST (expression) types produced by the parser.

use std::error::Error;
use std::fmt::{self, Display, Formatter};

#[cfg(test)]
use super::intern::intern;
use super::intern::{SymId, lookup_interned, resolve_sym};
use super::string_escape::format_lisp_string;

/// Parsed Lisp expression (AST node).
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Symbol(SymId),
    /// Reader pseudo-object `#$` — current load file name captured at read time.
    ReaderLoadFileName,
    Keyword(SymId),
    Str(String),
    Char(char),
    List(Vec<Expr>),
    Vector(Vec<Expr>),
    /// Dotted pair `(a b . c)` — last cdr is not nil.
    DottedList(Vec<Expr>, Box<Expr>),
    /// Boolean literal — Emacs uses nil/t symbols, but we also accept #t/#f.
    Bool(bool),
    /// Opaque runtime value (Lambda, ByteCode, Subr, etc.) embedded in code.
    /// Produced by `value_to_expr` when converting data structures containing
    /// callable values (e.g., closures inside backquoted defcustom expansions).
    /// Evaluates to the contained value directly.
    OpaqueValue(super::value::Value),
}

impl Expr {
    /// Check if this Expr tree depends on reader-time runtime state.
    ///
    /// `#$` is the current example: it must capture `load-file-name` at read
    /// time, so caching a quoted Value across different loads would be wrong.
    pub fn depends_on_reader_runtime_state(&self) -> bool {
        match self {
            Expr::ReaderLoadFileName => true,
            Expr::List(items) | Expr::Vector(items) => {
                items.iter().any(|e| e.depends_on_reader_runtime_state())
            }
            Expr::DottedList(items, last) => {
                items.iter().any(|e| e.depends_on_reader_runtime_state())
                    || last.depends_on_reader_runtime_state()
            }
            _ => false,
        }
    }

    /// Check if this Expr tree contains any `OpaqueValue` nodes.
    /// Forms containing opaque values cannot be serialized to cache.
    pub fn contains_opaque_value(&self) -> bool {
        match self {
            Expr::OpaqueValue(_) => true,
            Expr::ReaderLoadFileName => false,
            Expr::List(items) | Expr::Vector(items) => {
                items.iter().any(|e| e.contains_opaque_value())
            }
            Expr::DottedList(items, last) => {
                items.iter().any(|e| e.contains_opaque_value()) || last.contains_opaque_value()
            }
            _ => false,
        }
    }

    /// Collect all Values embedded in `OpaqueValue` nodes in this Expr tree.
    /// Used by GC to trace Values inside Lambda/Macro body expressions.
    pub fn collect_opaque_values(&self, out: &mut Vec<super::value::Value>) {
        match self {
            Expr::OpaqueValue(v) => out.push(*v),
            Expr::ReaderLoadFileName => {}
            Expr::List(items) | Expr::Vector(items) => {
                for e in items {
                    e.collect_opaque_values(out);
                }
            }
            Expr::DottedList(items, last) => {
                for e in items {
                    e.collect_opaque_values(out);
                }
                last.collect_opaque_values(out);
            }
            _ => {} // Leaf nodes: Int, Float, Symbol, Keyword, Str, Char, Bool
        }
    }
}

#[derive(Clone, Debug)]
pub struct ParseError {
    pub position: usize,
    pub message: String,
}

impl Display for ParseError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "parse error at {}: {}", self.position, self.message)
    }
}

impl Error for ParseError {}

/// Print an expression as Lisp source.
pub fn print_expr(expr: &Expr) -> String {
    match expr {
        Expr::Int(v) => v.to_string(),
        Expr::Float(v) => format_float(*v),
        Expr::Symbol(id) => format_symbol_id(*id),
        Expr::ReaderLoadFileName => "#$".to_string(),
        Expr::Keyword(id) => resolve_sym(*id).to_owned(),
        Expr::Str(s) => format_lisp_string(s),
        Expr::Char(c) => format_char_literal(*c),
        Expr::Bool(true) => "t".to_string(),
        Expr::Bool(false) => "nil".to_string(),
        Expr::OpaqueValue(v) => format!("{}", v),
        Expr::List(items) => {
            if items.is_empty() {
                return "nil".to_string();
            }
            if items.len() == 2 {
                if let Expr::Symbol(id) = &items[0] {
                    let s = resolve_sym(*id);
                    if s == "quote" {
                        return format!("'{}", print_expr(&items[1]));
                    }
                    if s == "function" {
                        return format!("#'{}", print_expr(&items[1]));
                    }
                    if s == "`" {
                        return format!("`{}", print_expr(&items[1]));
                    }
                    if s == "," {
                        return format!(",{}", print_expr(&items[1]));
                    }
                    if s == ",@" {
                        return format!(",@{}", print_expr(&items[1]));
                    }
                }
            }
            let parts: Vec<String> = items.iter().map(print_expr).collect();
            format!("({})", parts.join(" "))
        }
        Expr::DottedList(items, last) => {
            let mut parts: Vec<String> = items.iter().map(print_expr).collect();
            parts.push(format!(". {}", print_expr(last)));
            format!("({})", parts.join(" "))
        }
        Expr::Vector(items) => {
            let parts: Vec<String> = items.iter().map(print_expr).collect();
            format!("[{}]", parts.join(" "))
        }
    }
}

fn format_symbol_name(name: &str) -> String {
    if name.is_empty() {
        return "##".to_string();
    }
    let mut out = String::with_capacity(name.len());
    for (idx, ch) in name.chars().enumerate() {
        let needs_escape = matches!(
            ch,
            ' ' | '\t'
                | '\n'
                | '\r'
                | '\u{0c}'
                | '('
                | ')'
                | '['
                | ']'
                | '"'
                | '\\'
                | ';'
                | '#'
                | '\''
                | '`'
                | ','
        ) || (idx == 0 && matches!(ch, '.' | '?'));
        if needs_escape {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn format_symbol_id(id: SymId) -> String {
    let name = resolve_sym(id);
    if lookup_interned(name) == Some(id) {
        format_symbol_name(name)
    } else if name.is_empty() {
        "#:".to_string()
    } else {
        format!("#:{}", format_symbol_name(name))
    }
}

fn format_float(f: f64) -> String {
    super::print::format_float(f)
}

fn format_char_literal(c: char) -> String {
    match c {
        ' ' => "?\\ ".to_string(),
        '\\' => "?\\\\".to_string(),
        '\n' => "?\\n".to_string(),
        '\t' => "?\\t".to_string(),
        '\r' => "?\\r".to_string(),
        '\x07' => "?\\a".to_string(),
        '\x08' => "?\\b".to_string(),
        '\x0C' => "?\\f".to_string(),
        '\x1B' => "?\\e".to_string(),
        '\x7F' => "?\\d".to_string(),
        '(' | ')' | '[' | ']' | '"' | ';' | '#' | '\'' | '`' | ',' => format!("?\\{c}"),
        c if c < ' ' => format!("?\\{:03o}", c as u32),
        c if (c as u32) > 0x7F => format!("?\\x{:x}", c as u32),
        _ => format!("?{c}"),
    }
}

#[cfg(test)]
#[path = "expr_test.rs"]
mod tests;
