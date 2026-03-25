//! Serialization format for `.neobc` compiled Elisp files.
//!
//! Converts `CompiledForm` (which contains runtime `Value` types with heap
//! references) into a self-contained binary format that can be written to disk
//! and loaded without the original source.
//!
//! The format uses the same `CachedExpr` encoding strategy as the expanded
//! cache in `load.rs`: symbols are stored by name (not `SymId`) so files are
//! portable across evaluator sessions.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;

use super::eval::{quote_to_value, value_to_expr};
use super::expr::Expr;
use super::file_compile::CompiledForm;
use super::intern::{SymId, intern, intern_uninterned, lookup_interned, resolve_sym};
use super::value::Value;

/// Magic bytes identifying a `.neobc` file.
const NEOBC_MAGIC: &[u8] = b"NEOVM-BC-V1\n";

// ---------------------------------------------------------------------------
// Serializable expression type (mirrors load.rs CachedExpr, which is private)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
enum CachedExpr {
    Int(i64),
    Float(f64),
    Symbol(String),
    UninternedSymbol { slot: u32, name: String },
    ReaderLoadFileName,
    Keyword(String),
    Str(String),
    Char(char),
    List(Vec<CachedExpr>),
    Vector(Vec<CachedExpr>),
    DottedList(Vec<CachedExpr>, Box<CachedExpr>),
    Bool(bool),
}

/// A single serializable compiled form.
#[derive(Serialize, Deserialize, Debug, Clone)]
enum SerializedForm {
    /// An expression to evaluate at load time.
    Eval(CachedExpr),
    /// A constant result from `eval-when-compile`.
    Constant(CachedExpr),
}

/// Top-level `.neobc` file contents.
#[derive(Serialize, Deserialize, Debug)]
struct NeobcFile {
    /// SHA-256 hex digest of the source `.el` file contents.
    source_hash: String,
    /// Whether the source was compiled with lexical binding.
    lexical_binding: bool,
    /// The compiled forms.
    forms: Vec<SerializedForm>,
}

// ---------------------------------------------------------------------------
// Expr <-> CachedExpr conversion (encoder/decoder)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ExprEncoder {
    uninterned_slots: HashMap<SymId, u32>,
    next_slot: u32,
}

#[derive(Default)]
struct ExprDecoder {
    uninterned_slots: HashMap<u32, SymId>,
}

fn is_canonical_symbol_id(id: SymId) -> bool {
    lookup_interned(resolve_sym(id)).is_some_and(|canonical| canonical == id)
}

impl ExprEncoder {
    fn encode(&mut self, expr: &Expr) -> Option<CachedExpr> {
        Some(match expr {
            Expr::Int(n) => CachedExpr::Int(*n),
            Expr::Float(f) => CachedExpr::Float(*f),
            Expr::Symbol(id) => {
                let name = resolve_sym(*id).to_owned();
                if is_canonical_symbol_id(*id) {
                    CachedExpr::Symbol(name)
                } else {
                    let slot = *self.uninterned_slots.entry(*id).or_insert_with(|| {
                        let slot = self.next_slot;
                        self.next_slot += 1;
                        slot
                    });
                    CachedExpr::UninternedSymbol { slot, name }
                }
            }
            Expr::ReaderLoadFileName => CachedExpr::ReaderLoadFileName,
            Expr::Keyword(id) => CachedExpr::Keyword(resolve_sym(*id).to_owned()),
            Expr::Str(s) => CachedExpr::Str(s.clone()),
            Expr::Char(c) => CachedExpr::Char(*c),
            Expr::List(items) => CachedExpr::List(
                items
                    .iter()
                    .map(|item| self.encode(item))
                    .collect::<Option<Vec<_>>>()?,
            ),
            Expr::Vector(items) => CachedExpr::Vector(
                items
                    .iter()
                    .map(|item| self.encode(item))
                    .collect::<Option<Vec<_>>>()?,
            ),
            Expr::DottedList(items, tail) => CachedExpr::DottedList(
                items
                    .iter()
                    .map(|item| self.encode(item))
                    .collect::<Option<Vec<_>>>()?,
                Box::new(self.encode(tail)?),
            ),
            Expr::Bool(b) => CachedExpr::Bool(*b),
            // OpaqueValue (lambdas, subrs, etc.) cannot be serialized.
            Expr::OpaqueValue(_) => return None,
        })
    }
}

impl ExprDecoder {
    fn decode(&mut self, expr: &CachedExpr) -> Expr {
        match expr {
            CachedExpr::Int(n) => Expr::Int(*n),
            CachedExpr::Float(f) => Expr::Float(*f),
            CachedExpr::Symbol(name) => Expr::Symbol(intern(name)),
            CachedExpr::UninternedSymbol { slot, name } => {
                let sym = *self
                    .uninterned_slots
                    .entry(*slot)
                    .or_insert_with(|| intern_uninterned(name));
                Expr::Symbol(sym)
            }
            CachedExpr::ReaderLoadFileName => Expr::ReaderLoadFileName,
            CachedExpr::Keyword(name) => Expr::Keyword(intern(name)),
            CachedExpr::Str(s) => Expr::Str(s.clone()),
            CachedExpr::Char(c) => Expr::Char(*c),
            CachedExpr::List(items) => {
                Expr::List(items.iter().map(|item| self.decode(item)).collect())
            }
            CachedExpr::Vector(items) => {
                Expr::Vector(items.iter().map(|item| self.decode(item)).collect())
            }
            CachedExpr::DottedList(items, tail) => Expr::DottedList(
                items.iter().map(|item| self.decode(item)).collect(),
                Box::new(self.decode(tail)),
            ),
            CachedExpr::Bool(b) => Expr::Bool(*b),
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compute the SHA-256 hex digest of source file contents.
pub fn source_sha256(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Serialize compiled forms to `.neobc` binary format.
///
/// Returns `None` if any form contains an `OpaqueValue` that cannot be
/// serialized (e.g., a lambda or subr embedded by eval-when-compile).
pub fn serialize_neobc(
    source_hash: &str,
    lexical_binding: bool,
    compiled_forms: &[CompiledForm],
) -> Option<Vec<u8>> {
    let mut encoder = ExprEncoder::default();
    let mut forms = Vec::with_capacity(compiled_forms.len());

    for form in compiled_forms {
        match form {
            CompiledForm::Eval(value) => {
                let expr = value_to_expr(value);
                let cached = encoder.encode(&expr)?;
                forms.push(SerializedForm::Eval(cached));
            }
            CompiledForm::Constant(value) => {
                let expr = value_to_expr(value);
                let cached = encoder.encode(&expr)?;
                forms.push(SerializedForm::Constant(cached));
            }
        }
    }

    let file = NeobcFile {
        source_hash: source_hash.to_owned(),
        lexical_binding,
        forms,
    };

    let payload = bincode::serialize(&file).ok()?;

    let mut out = Vec::with_capacity(NEOBC_MAGIC.len() + 4 + payload.len());
    out.extend_from_slice(NEOBC_MAGIC);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    Some(out)
}

/// Write compiled forms to a `.neobc` file on disk.
///
/// Returns `Err` if the forms cannot be serialized (e.g., contains opaque
/// values) or if the file cannot be written.
pub fn write_neobc(
    path: &Path,
    source_hash: &str,
    lexical_binding: bool,
    compiled_forms: &[CompiledForm],
) -> std::io::Result<()> {
    let bytes = serialize_neobc(source_hash, lexical_binding, compiled_forms).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "compiled forms contain non-serializable values",
        )
    })?;
    std::fs::write(path, bytes)
}

/// Result of reading a `.neobc` file.
#[derive(Debug)]
pub struct LoadedNeobc {
    /// Whether the source was compiled with lexical binding.
    pub lexical_binding: bool,
    /// Decoded forms ready for load-time evaluation.
    ///
    /// `Eval` forms become `Expr` to re-evaluate; `Constant` forms become
    /// `Value` (via `quote_to_value`) to use directly.
    pub forms: Vec<LoadedForm>,
}

/// A single form loaded from a `.neobc` file.
#[derive(Debug)]
pub enum LoadedForm {
    /// Re-evaluate this expression at load time.
    Eval(Expr),
    /// A pre-computed constant (result of `eval-when-compile`).
    Constant(Value),
}

/// Read and validate a `.neobc` file.
///
/// `expected_hash` is the SHA-256 hex digest of the current source; if the
/// file's stored hash does not match, `Err` is returned (stale cache).
/// Pass an empty string to skip the hash check.
pub fn read_neobc(path: &Path, expected_hash: &str) -> std::io::Result<LoadedNeobc> {
    let data = std::fs::read(path)?;

    // Validate magic header.
    if data.len() < NEOBC_MAGIC.len() + 4 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file too short for neobc header",
        ));
    }
    if &data[..NEOBC_MAGIC.len()] != NEOBC_MAGIC {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "bad neobc magic",
        ));
    }

    // Read payload length and deserialize.
    let offset = NEOBC_MAGIC.len();
    let payload_len = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
    let payload_start = offset + 4;
    if data.len() < payload_start + payload_len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "truncated neobc payload",
        ));
    }
    let payload = &data[payload_start..payload_start + payload_len];

    let file: NeobcFile = bincode::deserialize(payload)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    // Validate source hash (unless caller passes empty string to skip).
    if !expected_hash.is_empty() && file.source_hash != expected_hash {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "neobc hash mismatch: expected {}, got {}",
                expected_hash, file.source_hash
            ),
        ));
    }

    // Decode forms.
    let mut decoder = ExprDecoder::default();
    let forms = file
        .forms
        .iter()
        .map(|sf| match sf {
            SerializedForm::Eval(cached) => LoadedForm::Eval(decoder.decode(cached)),
            SerializedForm::Constant(cached) => {
                let expr = decoder.decode(cached);
                LoadedForm::Constant(quote_to_value(&expr))
            }
        })
        .collect();

    Ok(LoadedNeobc {
        lexical_binding: file.lexical_binding,
        forms,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emacs_core::eval::Context;
    use crate::emacs_core::file_compile::compile_file_forms;
    use crate::emacs_core::parser::parse_forms;

    #[test]
    fn test_source_sha256() {
        let hash = source_sha256("hello world");
        // Known SHA-256 of "hello world".
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn test_roundtrip_simple_eval_form() {
        let mut eval = Context::new();
        let forms = parse_forms("(+ 1 2)").unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();

        let hash = source_sha256("(+ 1 2)");
        let bytes = serialize_neobc(&hash, false, &compiled).expect("serialize");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.neobc");
        std::fs::write(&path, &bytes).unwrap();

        let loaded = read_neobc(&path, &hash).unwrap();
        assert!(!loaded.lexical_binding);
        assert_eq!(loaded.forms.len(), 1);
        assert!(matches!(&loaded.forms[0], LoadedForm::Eval(_)));

        // Re-evaluate the loaded form and check result.
        if let LoadedForm::Eval(expr) = &loaded.forms[0] {
            let mut eval2 = Context::new();
            let result = eval2.eval(expr).unwrap();
            assert_eq!(result, Value::Int(3));
        }
    }

    #[test]
    fn test_roundtrip_eval_when_compile() {
        let mut eval = Context::new();
        let src = "(eval-when-compile (+ 10 20))";
        let forms = parse_forms(src).unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();
        assert_eq!(compiled.len(), 1);
        assert!(matches!(&compiled[0], CompiledForm::Constant(_)));

        let hash = source_sha256(src);
        let bytes = serialize_neobc(&hash, true, &compiled).expect("serialize");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.neobc");
        std::fs::write(&path, &bytes).unwrap();

        let loaded = read_neobc(&path, &hash).unwrap();
        assert!(loaded.lexical_binding);
        assert_eq!(loaded.forms.len(), 1);
        match &loaded.forms[0] {
            LoadedForm::Constant(v) => assert_eq!(*v, Value::Int(30)),
            other => panic!("expected Constant, got Eval"),
        }
    }

    #[test]
    fn test_roundtrip_mixed_forms() {
        let mut eval = Context::new();
        let src = "(defvar fc-fmt-a 1)\n(eval-when-compile (+ 2 3))\n(defvar fc-fmt-b 10)";
        let forms = parse_forms(src).unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();
        assert_eq!(compiled.len(), 3);

        let hash = source_sha256(src);
        let bytes = serialize_neobc(&hash, false, &compiled).expect("serialize");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.neobc");
        std::fs::write(&path, &bytes).unwrap();

        let loaded = read_neobc(&path, &hash).unwrap();
        assert_eq!(loaded.forms.len(), 3);
        assert!(matches!(&loaded.forms[0], LoadedForm::Eval(_)));
        assert!(matches!(&loaded.forms[1], LoadedForm::Constant(_)));
        assert!(matches!(&loaded.forms[2], LoadedForm::Eval(_)));
    }

    #[test]
    fn test_hash_mismatch_rejected() {
        let mut eval = Context::new();
        let forms = parse_forms("(+ 1 2)").unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();

        let hash = source_sha256("(+ 1 2)");
        let bytes = serialize_neobc(&hash, false, &compiled).expect("serialize");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.neobc");
        std::fs::write(&path, &bytes).unwrap();

        let err = read_neobc(&path, "wrong-hash").unwrap_err();
        assert!(err.to_string().contains("hash mismatch"));
    }

    #[test]
    fn test_hash_skip_with_empty_string() {
        let mut eval = Context::new();
        let forms = parse_forms("(+ 1 2)").unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();

        let hash = source_sha256("(+ 1 2)");
        let bytes = serialize_neobc(&hash, false, &compiled).expect("serialize");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.neobc");
        std::fs::write(&path, &bytes).unwrap();

        // Empty string skips hash check.
        let loaded = read_neobc(&path, "").unwrap();
        assert_eq!(loaded.forms.len(), 1);
    }

    #[test]
    fn test_bad_magic_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.neobc");
        std::fs::write(&path, b"NOT-A-NEOBC-FILE").unwrap();

        let err = read_neobc(&path, "").unwrap_err();
        assert!(err.to_string().contains("magic"));
    }

    #[test]
    fn test_truncated_file_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.neobc");
        // Write magic + a payload length that exceeds the actual data.
        let mut data = Vec::new();
        data.extend_from_slice(NEOBC_MAGIC);
        data.extend_from_slice(&1000u32.to_le_bytes());
        data.extend_from_slice(b"short");
        std::fs::write(&path, &data).unwrap();

        let err = read_neobc(&path, "").unwrap_err();
        assert!(err.to_string().contains("truncated"));
    }

    #[test]
    fn test_write_neobc_convenience() {
        let mut eval = Context::new();
        let forms = parse_forms("(+ 1 2)").unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.neobc");
        let hash = source_sha256("(+ 1 2)");

        write_neobc(&path, &hash, false, &compiled).unwrap();

        let loaded = read_neobc(&path, &hash).unwrap();
        assert_eq!(loaded.forms.len(), 1);
    }

    #[test]
    fn test_roundtrip_string_constant() {
        let mut eval = Context::new();
        let src = r#"(eval-when-compile "hello")"#;
        let forms = parse_forms(src).unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();
        assert_eq!(compiled.len(), 1);

        let hash = source_sha256(src);
        let bytes = serialize_neobc(&hash, false, &compiled).expect("serialize");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.neobc");
        std::fs::write(&path, &bytes).unwrap();

        let loaded = read_neobc(&path, &hash).unwrap();
        assert_eq!(loaded.forms.len(), 1);
        match &loaded.forms[0] {
            LoadedForm::Constant(v) => {
                assert_eq!(v.as_str(), Some("hello"));
            }
            _ => panic!("expected Constant"),
        }
    }

    #[test]
    fn test_roundtrip_nil_constant() {
        let mut eval = Context::new();
        let src = "(eval-when-compile nil)";
        let forms = parse_forms(src).unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();

        let hash = source_sha256(src);
        let bytes = serialize_neobc(&hash, false, &compiled).expect("serialize");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.neobc");
        std::fs::write(&path, &bytes).unwrap();

        let loaded = read_neobc(&path, &hash).unwrap();
        assert_eq!(loaded.forms.len(), 1);
        match &loaded.forms[0] {
            LoadedForm::Constant(v) => assert_eq!(*v, Value::Nil),
            _ => panic!("expected Constant"),
        }
    }

    #[test]
    fn test_roundtrip_lexical_binding_flag() {
        let mut eval = Context::new();
        let forms = parse_forms("t").unwrap();
        let compiled = compile_file_forms(&mut eval, &forms).unwrap();

        let hash = source_sha256("t");

        // lexical_binding = true
        let bytes = serialize_neobc(&hash, true, &compiled).expect("serialize");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.neobc");
        std::fs::write(&path, &bytes).unwrap();
        let loaded = read_neobc(&path, &hash).unwrap();
        assert!(loaded.lexical_binding);

        // lexical_binding = false
        let bytes = serialize_neobc(&hash, false, &compiled).expect("serialize");
        std::fs::write(&path, &bytes).unwrap();
        let loaded = read_neobc(&path, &hash).unwrap();
        assert!(!loaded.lexical_binding);
    }
}
