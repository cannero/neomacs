//! Register system -- quick storage and retrieval of text, positions, etc.
//!
//! Provides Emacs-compatible register functionality:
//! - `copy-to-register` -- store text in a register
//! - `insert-register` -- insert text from a register
//! - `point-to-register` -- store current position in a register
//! - `jump-to-register` -- jump to a stored position
//! - `number-to-register` -- store a number in a register
//! - `increment-register` -- increment a number in a register
//! - `view-register` -- describe a register's contents
//! - `list-registers` -- list all non-empty registers

use std::collections::HashMap;

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::value::{Value, ValueKind, next_float_id};
use crate::gc::GcTrace;

// ---------------------------------------------------------------------------
// Register content types
// ---------------------------------------------------------------------------

/// The different kinds of data that can be stored in a register.
#[derive(Clone, Debug)]
pub enum RegisterContent {
    /// Plain text string.
    Text(String),
    /// An integer value.
    Number(i64),
    /// A saved buffer position: (buffer name, point offset).
    Position { buffer: String, point: usize },
    /// A rectangle (list of strings, one per line).
    Rectangle(Vec<String>),
    /// A saved window/frame configuration (opaque Lisp value).
    FrameConfig(Value),
    /// A file name (for `set-register` with file references).
    File(String),
    /// A keyboard macro (sequence of key events).
    KbdMacro(Vec<Value>),
}

impl RegisterContent {
    /// Return a short human-readable description of the content kind.
    fn description(&self) -> &str {
        match self {
            RegisterContent::Text(_) => "text",
            RegisterContent::Number(_) => "number",
            RegisterContent::Position { .. } => "position",
            RegisterContent::Rectangle(_) => "rectangle",
            RegisterContent::FrameConfig(_) => "frame-config",
            RegisterContent::File(_) => "file",
            RegisterContent::KbdMacro(_) => "kbd-macro",
        }
    }
}

// ---------------------------------------------------------------------------
// RegisterManager
// ---------------------------------------------------------------------------

/// Central registry for all registers.
#[derive(Clone, Debug)]
pub struct RegisterManager {
    registers: HashMap<char, RegisterContent>,
}

impl Default for RegisterManager {
    fn default() -> Self {
        Self::new()
    }
}

impl RegisterManager {
    /// Create a new empty register manager.
    pub fn new() -> Self {
        Self {
            registers: HashMap::new(),
        }
    }

    /// Store content in a register, replacing any previous content.
    pub fn set(&mut self, register: char, content: RegisterContent) {
        self.registers.insert(register, content);
    }

    /// Retrieve the content of a register, if any.
    pub fn get(&self, register: char) -> Option<&RegisterContent> {
        self.registers.get(&register)
    }

    /// Clear a single register.
    pub fn clear(&mut self, register: char) {
        self.registers.remove(&register);
    }

    /// Clear all registers.
    pub fn clear_all(&mut self) {
        self.registers.clear();
    }

    /// Return a sorted list of (register-char, description) pairs for all
    /// non-empty registers.
    pub fn list(&self) -> Vec<(char, &str)> {
        let mut entries: Vec<(char, &str)> = self
            .registers
            .iter()
            .map(|(&ch, content)| (ch, content.description()))
            .collect();
        entries.sort_by_key(|(ch, _)| *ch);
        entries
    }

    /// Convenience: get the text stored in a register, if it holds text.
    pub fn get_text(&self, register: char) -> Option<&str> {
        match self.registers.get(&register) {
            Some(RegisterContent::Text(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Append (or prepend) text to a register that already holds text.
    /// If the register is empty or not text, it becomes a Text register
    /// containing just the new text.
    pub fn append_text(&mut self, register: char, text: &str, prepend: bool) {
        match self.registers.get_mut(&register) {
            Some(RegisterContent::Text(existing)) => {
                if prepend {
                    let mut new = String::with_capacity(text.len() + existing.len());
                    new.push_str(text);
                    new.push_str(existing);
                    *existing = new;
                } else {
                    existing.push_str(text);
                }
            }
            _ => {
                self.registers
                    .insert(register, RegisterContent::Text(text.to_string()));
            }
        }
    }

    // pdump accessors
    pub(crate) fn dump_registers(&self) -> &HashMap<char, RegisterContent> {
        &self.registers
    }
    pub(crate) fn from_dump(registers: HashMap<char, RegisterContent>) -> Self {
        Self { registers }
    }
}

impl GcTrace for RegisterManager {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        for content in self.registers.values() {
            match content {
                RegisterContent::FrameConfig(v) => {
                    roots.push(*v);
                }
                RegisterContent::KbdMacro(keys) => {
                    for v in keys {
                        roots.push(*v);
                    }
                }
                _ => {}
            }
        }
    }
}

// ===========================================================================
// Builtin helpers
// ===========================================================================

fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_max_args(name: &str, args: &[Value], max: usize) -> Result<(), Flow> {
    if args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_string(value: &Value) -> Result<String, Flow> {
    match value.kind() {
        ValueKind::String => Ok(value.as_str().unwrap().to_string()),
        ValueKind::Symbol(id) => Ok(resolve_sym(id).to_owned()),
        ValueKind::Nil => Ok("nil".to_string()),
        ValueKind::T => Ok("t".to_string()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *value],
        )),
    }
}

fn expect_int(value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        ValueKind::Char(c) => Ok(c as i64),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *value],
        )),
    }
}

/// Extract a register character from a first argument.
/// Accepts a Char directly, or an Int (treated as ASCII code), or a
/// single-character string.
fn expect_register(value: &Value) -> Result<char, Flow> {
    match value.kind() {
        ValueKind::Char(c) => Ok(c),
        ValueKind::Fixnum(n) => {
            if n >= 0 && n <= 0x10FFFF {
                if let Some(c) = char::from_u32(n as u32) {
                    return Ok(c);
                }
            }
            Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), *value],
            ))
        }
        ValueKind::String => {
            let st = value.as_str().unwrap();
            let mut chars = st.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Ok(c),
                _ => Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("characterp"), *value],
                )),
            }
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *value],
        )),
    }
}

// ===========================================================================
// Builtins (evaluator-dependent)
// ===========================================================================

/// (copy-to-register REGISTER START END &optional DELETE-FLAG) -> nil
///
/// In the VM we don't have buffer positions, so START and END are
/// interpreted as a text string to store (the caller passes the
/// extracted region text as a string in arg index 1).
/// Simplified: (copy-to-register REGISTER TEXT) -> nil
pub(crate) fn builtin_copy_to_register(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("copy-to-register", &args, 2)?;
    expect_max_args("copy-to-register", &args, 5)?;
    let reg = expect_register(&args[0])?;
    let text = expect_string(&args[1])?;
    eval.registers.set(reg, RegisterContent::Text(text));
    Ok(Value::NIL)
}

/// (insert-register REGISTER &optional NOT-KILL) -> nil
///
/// Returns the text stored in the register as a string (for the caller
/// to insert into the buffer).  Signals an error if the register is empty
/// or does not hold text.
pub(crate) fn builtin_insert_register(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("insert-register", &args, 1)?;
    expect_max_args("insert-register", &args, 2)?;
    let reg = expect_register(&args[0])?;
    match eval.registers.get(reg) {
        Some(RegisterContent::Text(s)) => Ok(Value::string(s.clone())),
        Some(RegisterContent::Number(n)) => Ok(Value::string(n.to_string())),
        Some(RegisterContent::Rectangle(lines)) => Ok(Value::string(lines.join("\n"))),
        Some(_) => Err(signal(
            "error",
            vec![Value::string(format!(
                "Register does not contain text: {}",
                reg
            ))],
        )),
        None => Err(signal(
            "error",
            vec![Value::string(format!("Register '{}' is empty", reg))],
        )),
    }
}

/// (point-to-register REGISTER) -> nil
///
/// Store the current buffer name and point in the register.
pub(crate) fn builtin_point_to_register(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("point-to-register", &args, 1)?;
    let reg = expect_register(&args[0])?;
    let buffer_name = eval
        .buffers
        .current_buffer()
        .map(|b| b.name.clone())
        .unwrap_or_else(|| "*scratch*".to_string());
    let point = eval
        .buffers
        .current_buffer()
        .map(|b| b.point())
        .unwrap_or(1);
    eval.registers.set(
        reg,
        RegisterContent::Position {
            buffer: buffer_name,
            point,
        },
    );
    Ok(Value::NIL)
}

/// (number-to-register NUMBER REGISTER) -> nil
pub(crate) fn builtin_number_to_register(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("number-to-register", &args, 2)?;
    let num = expect_int(&args[0])?;
    let reg = expect_register(&args[1])?;
    eval.registers.set(reg, RegisterContent::Number(num));
    Ok(Value::NIL)
}

/// (increment-register NUMBER REGISTER) -> nil
///
/// If the register holds a number, add NUMBER to it.
/// If it holds text, append the printed number.
/// Otherwise signal an error.
pub(crate) fn builtin_increment_register(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("increment-register", &args, 2)?;
    let inc = expect_int(&args[0])?;
    let reg = expect_register(&args[1])?;
    match eval.registers.get(reg).cloned() {
        Some(RegisterContent::Number(n)) => {
            eval.registers.set(reg, RegisterContent::Number(n + inc));
            Ok(Value::NIL)
        }
        Some(RegisterContent::Text(mut s)) => {
            s.push_str(&inc.to_string());
            eval.registers.set(reg, RegisterContent::Text(s));
            Ok(Value::NIL)
        }
        Some(_) => Err(signal(
            "error",
            vec![Value::string(format!(
                "Register does not contain a number or text: {}",
                reg
            ))],
        )),
        None => {
            // Empty register: treat as number starting from 0
            eval.registers.set(reg, RegisterContent::Number(inc));
            Ok(Value::NIL)
        }
    }
}

/// (view-register REGISTER) -> string
///
/// Return a human-readable description of the register's contents.
pub(crate) fn builtin_view_register(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("view-register", &args, 1)?;
    let reg = expect_register(&args[0])?;
    match eval.registers.get(reg) {
        Some(RegisterContent::Text(s)) => {
            let desc = if s.len() > 60 {
                format!("Register {} contains text: {}...", reg, &s[..60])
            } else {
                format!("Register {} contains text: {}", reg, s)
            };
            Ok(Value::string(desc))
        }
        Some(RegisterContent::Number(n)) => Ok(Value::string(format!(
            "Register {} contains the number {}",
            reg, n
        ))),
        Some(RegisterContent::Position { buffer, point }) => Ok(Value::string(format!(
            "Register {} contains a position: buffer={} point={}",
            reg, buffer, point
        ))),
        Some(RegisterContent::Rectangle(lines)) => Ok(Value::string(format!(
            "Register {} contains a rectangle ({} lines)",
            reg,
            lines.len()
        ))),
        Some(RegisterContent::FrameConfig(_)) => Ok(Value::string(format!(
            "Register {} contains a frame configuration",
            reg
        ))),
        Some(RegisterContent::File(f)) => Ok(Value::string(format!(
            "Register {} contains file: {}",
            reg, f
        ))),
        Some(RegisterContent::KbdMacro(keys)) => Ok(Value::string(format!(
            "Register {} contains a keyboard macro ({} keys)",
            reg,
            keys.len()
        ))),
        None => Ok(Value::string(format!("Register {} is empty", reg))),
    }
}

/// (get-register REGISTER) -> value or nil
///
/// Return the content of a register as a Lisp value.
/// Text -> string, Number -> integer, Position -> list, otherwise nil.
pub(crate) fn builtin_get_register(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("get-register", &args, 1)?;
    let reg = expect_register(&args[0])?;
    match eval.registers.get(reg) {
        Some(RegisterContent::Text(s)) => Ok(Value::string(s.clone())),
        Some(RegisterContent::Number(n)) => Ok(Value::fixnum(*n)),
        Some(RegisterContent::Position { buffer, point }) => Ok(Value::cons(
            Value::string(buffer.clone()),
            Value::fixnum(*point as i64),
        )),
        Some(RegisterContent::Rectangle(lines)) => {
            let vals: Vec<Value> = lines.iter().map(|l| Value::string(l.clone())).collect();
            Ok(Value::list(vals))
        }
        Some(RegisterContent::File(f)) => Ok(Value::string(f.clone())),
        Some(RegisterContent::FrameConfig(v)) => Ok(*v),
        Some(RegisterContent::KbdMacro(keys)) => Ok(Value::list(keys.clone())),
        None => Ok(Value::NIL),
    }
}

/// (register-to-string REGISTER) -> string or nil
///
/// Return textual content from REGISTER when available.
#[cfg(test)]
pub(crate) fn builtin_register_to_string(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("register-to-string", &args, 1)?;
    let reg = expect_register(&args[0])?;
    match eval.registers.get(reg) {
        Some(RegisterContent::Text(s)) => Ok(Value::string(s.clone())),
        Some(RegisterContent::Rectangle(lines)) => Ok(Value::string(lines.join("\n"))),
        _ => Ok(Value::NIL),
    }
}

/// (set-register REGISTER VALUE) -> nil
///
/// Low-level: store an arbitrary Lisp value.  Strings become Text,
/// integers become Number, otherwise stored as FrameConfig (opaque).
pub(crate) fn builtin_set_register(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-register", &args, 2)?;
    let reg = expect_register(&args[0])?;
    let content = match args[1].kind() {
        ValueKind::String => RegisterContent::Text(args[1].as_str().unwrap().to_string()),
        ValueKind::Fixnum(n) => RegisterContent::Number(n),
        ValueKind::Nil => {
            eval.registers.clear(reg);
            return Ok(Value::NIL);
        }
        other => RegisterContent::FrameConfig(args[1]),
    };
    eval.registers.set(reg, content);
    Ok(Value::NIL)
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
#[path = "register_test.rs"]
mod tests;
