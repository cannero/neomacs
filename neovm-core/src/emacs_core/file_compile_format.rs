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

use super::bytecode::chunk::ByteCodeFunction;
use super::bytecode::opcode::Op;
use super::eval::OPAQUE_POOL;
use super::expr::Expr;
use super::file_compile::CompiledForm;
use super::intern::{
    SymId, intern, intern_uninterned, is_canonical_id, resolve_sym, try_resolve_sym,
};
use super::value::{
    HashKey, HashTableTest, HashTableWeakness, LambdaParams, Value, build_hash_table_literal_value,
};
use crate::tagged::header::VecLikeType;

/// Monotonic on-disk `.neobc` schema version.
///
/// Keep this aligned with `NEOBC_MAGIC` and the runtime cache directory
/// version in `load.rs`. Any change that makes previously emitted cached
/// forms semantically unsafe to replay must bump it, even if the bincode
/// layout itself stays readable.
pub(crate) const NEOBC_FORMAT_VERSION: u32 = 19;

/// Magic bytes identifying a `.neobc` file.
const NEOBC_MAGIC: &[u8] = b"NEOVM-BC-V19\n";

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
    Record(Vec<CachedExpr>),
    Lambda(Vec<CachedExpr>),
    Macro(Vec<CachedExpr>),
    ByteCode(Box<CachedByteCodeFunction>),
    HashTable(CachedHashTable),
    DottedList(Vec<CachedExpr>, Box<CachedExpr>),
    Bool(bool),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
enum CachedSymRef {
    Symbol(String),
    UninternedSymbol { slot: u32, name: String },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct CachedLambdaParams {
    required: Vec<CachedSymRef>,
    optional: Vec<CachedSymRef>,
    rest: Option<CachedSymRef>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct CachedByteCodeFunction {
    ops: Vec<Op>,
    constants: Vec<CachedExpr>,
    max_stack: u16,
    params: CachedLambdaParams,
    lexical: bool,
    env: Option<CachedExpr>,
    gnu_byte_offset_map: Option<Vec<(u32, u32)>>,
    docstring: Option<String>,
    doc_form: Option<CachedExpr>,
    interactive: Option<CachedExpr>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
enum CachedHashTableTest {
    Eq,
    Eql,
    Equal,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
enum CachedHashTableWeakness {
    Key,
    Value,
    KeyOrValue,
    KeyAndValue,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct CachedHashTable {
    test: CachedHashTableTest,
    test_name: Option<String>,
    size: i64,
    weakness: Option<CachedHashTableWeakness>,
    rehash_size: f64,
    rehash_threshold: f64,
    entries: Vec<(CachedExpr, CachedExpr)>,
}

/// A single serializable compiled form.
#[derive(Serialize, Deserialize, Debug, Clone)]
enum SerializedForm {
    /// An expression to evaluate at load time.
    Eval(CachedExpr),
    /// A source form that must go back through eager macroexpansion at load time
    /// to preserve GNU `eval-and-compile` / `eval-when-compile` side effects.
    EagerEval(CachedExpr),
    /// A constant result from `eval-when-compile`.
    Constant(CachedExpr),
}

/// Top-level `.neobc` file contents.
#[derive(Serialize, Deserialize, Debug)]
struct NeobcFile {
    /// SHA-256 hex digest of the source `.el` file contents.
    source_hash: String,
    /// Optional fingerprint of the bootstrap/runtime surface that produced
    /// this cache. Runtime eager caches are only safe to replay when the
    /// loader surface matches.
    surface_fingerprint: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnsupportedValue {
    path: String,
    detail: String,
}

impl UnsupportedValue {
    fn new(path: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            detail: detail.into(),
        }
    }

    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    pub(crate) fn detail(&self) -> &str {
        &self.detail
    }

    pub(crate) fn with_path_prefix(self, prefix: impl AsRef<str>) -> Self {
        let prefix = prefix.as_ref();
        let path = if self.path.is_empty() {
            prefix.to_owned()
        } else if prefix.is_empty() {
            self.path
        } else {
            format!("{prefix}.{}", self.path)
        };
        Self {
            path,
            detail: self.detail,
        }
    }
}

fn is_canonical_symbol_id(id: SymId) -> bool {
    is_canonical_id(id)
}

fn encode_hash_table_test(test: &HashTableTest) -> CachedHashTableTest {
    match test {
        HashTableTest::Eq => CachedHashTableTest::Eq,
        HashTableTest::Eql => CachedHashTableTest::Eql,
        HashTableTest::Equal => CachedHashTableTest::Equal,
    }
}

fn decode_hash_table_test(test: &CachedHashTableTest) -> HashTableTest {
    match test {
        CachedHashTableTest::Eq => HashTableTest::Eq,
        CachedHashTableTest::Eql => HashTableTest::Eql,
        CachedHashTableTest::Equal => HashTableTest::Equal,
    }
}

fn encode_hash_table_weakness(weakness: &HashTableWeakness) -> CachedHashTableWeakness {
    match weakness {
        HashTableWeakness::Key => CachedHashTableWeakness::Key,
        HashTableWeakness::Value => CachedHashTableWeakness::Value,
        HashTableWeakness::KeyOrValue => CachedHashTableWeakness::KeyOrValue,
        HashTableWeakness::KeyAndValue => CachedHashTableWeakness::KeyAndValue,
    }
}

fn decode_hash_table_weakness(weakness: &CachedHashTableWeakness) -> HashTableWeakness {
    match weakness {
        CachedHashTableWeakness::Key => HashTableWeakness::Key,
        CachedHashTableWeakness::Value => HashTableWeakness::Value,
        CachedHashTableWeakness::KeyOrValue => HashTableWeakness::KeyOrValue,
        CachedHashTableWeakness::KeyAndValue => HashTableWeakness::KeyAndValue,
    }
}

fn portable_hash_key_value(key: &HashKey) -> Option<Value> {
    Some(match key {
        HashKey::Nil => Value::NIL,
        HashKey::True => Value::T,
        HashKey::Int(n) => Value::fixnum(*n),
        HashKey::Float(bits) | HashKey::FloatEq(bits, _) => {
            Value::make_float(f64::from_bits(*bits))
        }
        HashKey::Symbol(id) => Value::from_sym_id(*id),
        HashKey::Keyword(id) => Value::keyword_id(*id),
        HashKey::Char(c) => Value::char(*c),
        HashKey::EqualCons(car, cdr) => {
            Value::cons(portable_hash_key_value(car)?, portable_hash_key_value(cdr)?)
        }
        HashKey::EqualVec(items) => Value::vector(
            items
                .iter()
                .map(portable_hash_key_value)
                .collect::<Option<Vec<_>>>()?,
        ),
        HashKey::Text(text) => Value::string(text.clone()),
        HashKey::Window(_) | HashKey::Frame(_) | HashKey::Ptr(_) | HashKey::Cycle(_) => {
            return None;
        }
    })
}

impl ExprEncoder {
    fn encode_sym_ref(&mut self, id: SymId) -> Option<CachedSymRef> {
        let name = try_resolve_sym(id)?.to_owned();
        Some(if is_canonical_symbol_id(id) {
            CachedSymRef::Symbol(name)
        } else {
            let slot = *self.uninterned_slots.entry(id).or_insert_with(|| {
                let slot = self.next_slot;
                self.next_slot += 1;
                slot
            });
            CachedSymRef::UninternedSymbol { slot, name }
        })
    }

    fn encode_lambda_params(&mut self, params: &LambdaParams) -> Option<CachedLambdaParams> {
        Some(CachedLambdaParams {
            required: params
                .required
                .iter()
                .map(|id| self.encode_sym_ref(*id))
                .collect::<Option<Vec<_>>>()?,
            optional: params
                .optional
                .iter()
                .map(|id| self.encode_sym_ref(*id))
                .collect::<Option<Vec<_>>>()?,
            rest: match params.rest {
                Some(id) => Some(self.encode_sym_ref(id)?),
                None => None,
            },
        })
    }

    fn encode_bytecode(&mut self, bc: &ByteCodeFunction) -> Option<CachedByteCodeFunction> {
        Some(CachedByteCodeFunction {
            ops: bc.ops.clone(),
            constants: bc
                .constants
                .iter()
                .map(|value| self.encode_value(value))
                .collect::<Option<Vec<_>>>()?,
            max_stack: bc.max_stack,
            params: self.encode_lambda_params(&bc.params)?,
            lexical: bc.lexical,
            env: match bc.env.as_ref() {
                Some(value) => Some(self.encode_value(value)?),
                None => None,
            },
            gnu_byte_offset_map: bc.gnu_byte_offset_map.as_ref().map(|map| {
                map.iter()
                    .map(|(byte_off, instr_idx)| (*byte_off as u32, *instr_idx as u32))
                    .collect()
            }),
            docstring: bc.docstring.clone(),
            doc_form: match bc.doc_form.as_ref() {
                Some(value) => Some(self.encode_value(value)?),
                None => None,
            },
            interactive: match bc.interactive.as_ref() {
                Some(value) => Some(self.encode_value(value)?),
                None => None,
            },
        })
    }

    fn encode_closure_slots(&mut self, value: Value) -> Option<Vec<CachedExpr>> {
        value
            .closure_slots()?
            .iter()
            .map(|item| self.encode_value(item))
            .collect::<Option<Vec<_>>>()
    }

    fn encode_hash_table(&mut self, value: Value) -> Option<CachedHashTable> {
        let table = value.as_hash_table()?;
        let mut entries = Vec::with_capacity(table.insertion_order.len());
        for key in &table.insertion_order {
            let key_value = table
                .key_snapshots
                .get(key)
                .copied()
                .or_else(|| portable_hash_key_value(key))?;
            let entry_value = table.data.get(key)?;
            entries.push((
                self.encode_value(&key_value)?,
                self.encode_value(entry_value)?,
            ));
        }
        Some(CachedHashTable {
            test: encode_hash_table_test(&table.test),
            test_name: match table.test_name {
                Some(id) => Some(try_resolve_sym(id)?.to_owned()),
                None => None,
            },
            size: table.size,
            weakness: table.weakness.as_ref().map(encode_hash_table_weakness),
            rehash_size: table.rehash_size,
            rehash_threshold: table.rehash_threshold,
            entries,
        })
    }

    fn encode(&mut self, expr: &Expr) -> Option<CachedExpr> {
        Some(match expr {
            Expr::Int(n) => CachedExpr::Int(*n),
            Expr::Float(f) => CachedExpr::Float(*f),
            Expr::Symbol(id) => {
                let name = try_resolve_sym(*id)?.to_owned();
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
            Expr::OpaqueValueRef(idx) => {
                let value = OPAQUE_POOL.with(|pool| pool.borrow().get(*idx));
                self.encode_value(&value)?
            }
        })
    }

    fn encode_value(&mut self, value: &Value) -> Option<CachedExpr> {
        Some(match value.kind() {
            super::value::ValueKind::Nil => CachedExpr::Symbol("nil".to_owned()),
            super::value::ValueKind::T => CachedExpr::Symbol("t".to_owned()),
            super::value::ValueKind::Fixnum(n) => CachedExpr::Int(n),
            super::value::ValueKind::Float => CachedExpr::Float(value.as_float()?),
            super::value::ValueKind::Symbol(id) => {
                let name = try_resolve_sym(id)?.to_owned();
                if is_canonical_symbol_id(id) {
                    CachedExpr::Symbol(name)
                } else {
                    let slot = *self.uninterned_slots.entry(id).or_insert_with(|| {
                        let slot = self.next_slot;
                        self.next_slot += 1;
                        slot
                    });
                    CachedExpr::UninternedSymbol { slot, name }
                }
            }
            super::value::ValueKind::String => {
                if super::value::string_has_text_properties_for_value(*value) {
                    return None;
                }
                CachedExpr::Str(value.as_str()?.to_owned())
            }
            super::value::ValueKind::Cons => {
                if let Some(items) = super::value::list_to_vec(value) {
                    CachedExpr::List(
                        items
                            .iter()
                            .map(|item| self.encode_value(item))
                            .collect::<Option<Vec<_>>>()?,
                    )
                } else {
                    let mut items = Vec::new();
                    let mut cursor = *value;
                    loop {
                        match cursor.kind() {
                            super::value::ValueKind::Cons => {
                                items.push(self.encode_value(&cursor.cons_car())?);
                                cursor = cursor.cons_cdr();
                            }
                            _ => {
                                break CachedExpr::DottedList(
                                    items,
                                    Box::new(self.encode_value(&cursor)?),
                                );
                            }
                        }
                    }
                }
            }
            super::value::ValueKind::Veclike(super::value::VecLikeType::Vector) => {
                let items = value.as_vector_data()?;
                CachedExpr::Vector(
                    items
                        .iter()
                        .map(|item| self.encode_value(item))
                        .collect::<Option<Vec<_>>>()?,
                )
            }
            super::value::ValueKind::Veclike(super::value::VecLikeType::Record) => {
                let items = value.as_record_data()?;
                CachedExpr::Record(
                    items
                        .iter()
                        .map(|item| self.encode_value(item))
                        .collect::<Option<Vec<_>>>()?,
                )
            }
            super::value::ValueKind::Veclike(super::value::VecLikeType::Lambda) => {
                CachedExpr::Lambda(self.encode_closure_slots(*value)?)
            }
            super::value::ValueKind::Veclike(super::value::VecLikeType::Macro) => {
                CachedExpr::Macro(self.encode_closure_slots(*value)?)
            }
            super::value::ValueKind::Veclike(super::value::VecLikeType::ByteCode) => {
                CachedExpr::ByteCode(Box::new(self.encode_bytecode(value.get_bytecode_data()?)?))
            }
            super::value::ValueKind::Veclike(super::value::VecLikeType::HashTable) => {
                CachedExpr::HashTable(self.encode_hash_table(*value)?)
            }
            _ => return None,
        })
    }

    fn unsupported_value(&self, value: &Value) -> UnsupportedValue {
        diagnose_unsupported_value(value, "root")
    }
}

fn push_path(base: &str, segment: &str) -> String {
    if base.is_empty() {
        segment.to_owned()
    } else {
        format!("{base}{segment}")
    }
}

fn summarize_value(value: &Value) -> String {
    match value.kind() {
        super::value::ValueKind::Nil => "nil".to_owned(),
        super::value::ValueKind::T => "t".to_owned(),
        super::value::ValueKind::Fixnum(n) => format!("fixnum {n}"),
        super::value::ValueKind::Float => format!("float {}", value.as_float().unwrap_or(0.0)),
        super::value::ValueKind::Symbol(id) => match try_resolve_sym(id) {
            Some(name) => format!("symbol {name}"),
            None => format!("invalid symbol id {}", id.0),
        },
        super::value::ValueKind::String => {
            if super::value::string_has_text_properties_for_value(*value) {
                "string with text properties".to_owned()
            } else {
                "plain string".to_owned()
            }
        }
        super::value::ValueKind::Cons => "cons".to_owned(),
        super::value::ValueKind::Veclike(ty) => format!("vectorlike {ty:?}"),
        super::value::ValueKind::Unknown => format!("unknown tagged value {value:?}"),
    }
}

fn diagnose_unsupported_value(value: &Value, path: &str) -> UnsupportedValue {
    match value.kind() {
        super::value::ValueKind::Nil
        | super::value::ValueKind::T
        | super::value::ValueKind::Fixnum(_)
        | super::value::ValueKind::Float
        | super::value::ValueKind::Symbol(_) => UnsupportedValue::new(
            path,
            format!(
                "value was expected to be serializable ({})",
                summarize_value(value)
            ),
        ),
        super::value::ValueKind::String => {
            if super::value::string_has_text_properties_for_value(*value) {
                UnsupportedValue::new(path, "string with text properties")
            } else {
                UnsupportedValue::new(path, "string value failed plain-string serialization")
            }
        }
        super::value::ValueKind::Cons => {
            if let Some(items) = super::value::list_to_vec(value) {
                for (idx, item) in items.iter().enumerate() {
                    if !ExprEncoder::default().encode_value(item).is_some() {
                        return diagnose_unsupported_value(
                            item,
                            &push_path(path, &format!("[{idx}]")),
                        );
                    }
                }
                UnsupportedValue::new(path, "list value failed serialization")
            } else {
                let car = value.cons_car();
                if ExprEncoder::default().encode_value(&car).is_none() {
                    return diagnose_unsupported_value(&car, &push_path(path, ".car"));
                }
                let cdr = value.cons_cdr();
                if ExprEncoder::default().encode_value(&cdr).is_none() {
                    return diagnose_unsupported_value(&cdr, &push_path(path, ".cdr"));
                }
                UnsupportedValue::new(path, "dotted list failed serialization")
            }
        }
        super::value::ValueKind::Veclike(super::value::VecLikeType::Vector) => {
            if let Some(items) = value.as_vector_data() {
                for (idx, item) in items.iter().enumerate() {
                    if ExprEncoder::default().encode_value(item).is_none() {
                        return diagnose_unsupported_value(
                            item,
                            &push_path(path, &format!("[{idx}]")),
                        );
                    }
                }
                UnsupportedValue::new(path, "vector value failed serialization")
            } else {
                UnsupportedValue::new(path, "vector payload unavailable")
            }
        }
        super::value::ValueKind::Veclike(super::value::VecLikeType::Record) => {
            if let Some(items) = value.as_record_data() {
                for (idx, item) in items.iter().enumerate() {
                    if ExprEncoder::default().encode_value(item).is_none() {
                        return diagnose_unsupported_value(
                            item,
                            &push_path(path, &format!("[{idx}]")),
                        );
                    }
                }
                UnsupportedValue::new(path, "record value failed serialization")
            } else {
                UnsupportedValue::new(path, "record payload unavailable")
            }
        }
        super::value::ValueKind::Veclike(super::value::VecLikeType::Lambda)
        | super::value::ValueKind::Veclike(super::value::VecLikeType::Macro) => {
            if let Some(slots) = value.closure_slots() {
                for (idx, item) in slots.iter().enumerate() {
                    if ExprEncoder::default().encode_value(item).is_none() {
                        return diagnose_unsupported_value(
                            item,
                            &push_path(path, &format!("[{idx}]")),
                        );
                    }
                }
                UnsupportedValue::new(path, "closure value failed serialization")
            } else {
                UnsupportedValue::new(path, "closure payload unavailable")
            }
        }
        super::value::ValueKind::Veclike(super::value::VecLikeType::ByteCode) => {
            if let Some(bytecode) = value.get_bytecode_data() {
                for (idx, item) in bytecode.constants.iter().enumerate() {
                    if ExprEncoder::default().encode_value(item).is_none() {
                        return diagnose_unsupported_value(
                            item,
                            &push_path(path, &format!(".constants[{idx}]")),
                        );
                    }
                }
                UnsupportedValue::new(path, "bytecode value failed serialization")
            } else {
                UnsupportedValue::new(path, "bytecode payload unavailable")
            }
        }
        super::value::ValueKind::Veclike(super::value::VecLikeType::HashTable) => {
            if let Some(table) = value.as_hash_table() {
                for (idx, key) in table.insertion_order.iter().enumerate() {
                    let Some(key_value) = table
                        .key_snapshots
                        .get(key)
                        .copied()
                        .or_else(|| portable_hash_key_value(key))
                    else {
                        return UnsupportedValue::new(
                            push_path(path, &format!(".data[{idx}].key")),
                            "hash-table key requires runtime-only identity",
                        );
                    };
                    if ExprEncoder::default().encode_value(&key_value).is_none() {
                        return diagnose_unsupported_value(
                            &key_value,
                            &push_path(path, &format!(".data[{idx}].key")),
                        );
                    }
                    if let Some(entry_value) = table.data.get(key) {
                        if ExprEncoder::default().encode_value(entry_value).is_none() {
                            return diagnose_unsupported_value(
                                entry_value,
                                &push_path(path, &format!(".data[{idx}].value")),
                            );
                        }
                    }
                }
                UnsupportedValue::new(path, "hash-table value failed serialization")
            } else {
                UnsupportedValue::new(path, "hash-table payload unavailable")
            }
        }
        _ => UnsupportedValue::new(path, summarize_value(value)),
    }
}

pub(crate) struct NeobcBuilder {
    source_hash: String,
    surface_fingerprint: Option<String>,
    lexical_binding: bool,
    encoder: ExprEncoder,
    forms: Vec<SerializedForm>,
}

impl NeobcBuilder {
    pub(crate) fn new(source_hash: &str, lexical_binding: bool) -> Self {
        Self {
            source_hash: source_hash.to_owned(),
            surface_fingerprint: None,
            lexical_binding,
            encoder: ExprEncoder::default(),
            forms: Vec::new(),
        }
    }

    pub(crate) fn set_surface_fingerprint(&mut self, surface_fingerprint: impl Into<String>) {
        self.surface_fingerprint = Some(surface_fingerprint.into());
    }

    pub(crate) fn push_eval_expr(&mut self, expr: &Expr) -> Option<()> {
        let cached = self.encoder.encode(expr)?;
        self.forms.push(SerializedForm::Eval(cached));
        Some(())
    }

    pub(crate) fn push_eval_value(&mut self, value: &Value) -> Option<()> {
        let cached = self.encoder.encode_value(value)?;
        self.forms.push(SerializedForm::Eval(cached));
        Some(())
    }

    pub(crate) fn push_eval_value_detailed(
        &mut self,
        value: &Value,
    ) -> Result<(), UnsupportedValue> {
        let cached = self
            .encoder
            .encode_value(value)
            .ok_or_else(|| self.encoder.unsupported_value(value))?;
        self.forms.push(SerializedForm::Eval(cached));
        Ok(())
    }

    pub(crate) fn push_eager_eval_value_detailed(
        &mut self,
        value: &Value,
    ) -> Result<(), UnsupportedValue> {
        let cached = self
            .encoder
            .encode_value(value)
            .ok_or_else(|| self.encoder.unsupported_value(value))?;
        self.forms.push(SerializedForm::EagerEval(cached));
        Ok(())
    }

    pub(crate) fn push_constant_value(&mut self, value: &Value) -> Option<()> {
        let cached = self.encoder.encode_value(value)?;
        self.forms.push(SerializedForm::Constant(cached));
        Some(())
    }

    pub(crate) fn push_constant_value_detailed(
        &mut self,
        value: &Value,
    ) -> Result<(), UnsupportedValue> {
        let cached = self
            .encoder
            .encode_value(value)
            .ok_or_else(|| self.encoder.unsupported_value(value))?;
        self.forms.push(SerializedForm::Constant(cached));
        Ok(())
    }

    pub(crate) fn len(&self) -> usize {
        self.forms.len()
    }

    fn finish_bytes(self) -> Option<Vec<u8>> {
        let file = NeobcFile {
            source_hash: self.source_hash,
            surface_fingerprint: self.surface_fingerprint,
            lexical_binding: self.lexical_binding,
            forms: self.forms,
        };

        let payload = bincode::serialize(&file).ok()?;
        let mut out = Vec::with_capacity(NEOBC_MAGIC.len() + 4 + payload.len());
        out.extend_from_slice(NEOBC_MAGIC);
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&payload);
        Some(out)
    }

    pub(crate) fn write(self, path: &Path) -> std::io::Result<()> {
        let bytes = self.finish_bytes().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "forms contain non-serializable runtime values",
            )
        })?;
        std::fs::write(path, bytes)
    }
}

impl ExprDecoder {
    fn decode_sym_ref(&mut self, sym: &CachedSymRef) -> SymId {
        match sym {
            CachedSymRef::Symbol(name) => intern(name),
            CachedSymRef::UninternedSymbol { slot, name } => *self
                .uninterned_slots
                .entry(*slot)
                .or_insert_with(|| intern_uninterned(name)),
        }
    }

    fn decode_lambda_params(&mut self, params: &CachedLambdaParams) -> LambdaParams {
        LambdaParams {
            required: params
                .required
                .iter()
                .map(|sym| self.decode_sym_ref(sym))
                .collect(),
            optional: params
                .optional
                .iter()
                .map(|sym| self.decode_sym_ref(sym))
                .collect(),
            rest: params.rest.as_ref().map(|sym| self.decode_sym_ref(sym)),
        }
    }

    fn decode_bytecode_value(&mut self, bytecode: &CachedByteCodeFunction) -> Value {
        let constants = bytecode
            .constants
            .iter()
            .map(|item| self.decode_value(item))
            .collect();
        Value::make_bytecode(ByteCodeFunction {
            ops: bytecode.ops.clone(),
            constants,
            max_stack: bytecode.max_stack,
            params: self.decode_lambda_params(&bytecode.params),
            lexical: bytecode.lexical,
            env: bytecode.env.as_ref().map(|value| self.decode_value(value)),
            gnu_byte_offset_map: bytecode.gnu_byte_offset_map.as_ref().map(|pairs| {
                pairs
                    .iter()
                    .map(|(byte_off, instr_idx)| (*byte_off as usize, *instr_idx as usize))
                    .collect()
            }),
            docstring: bytecode.docstring.clone(),
            doc_form: bytecode
                .doc_form
                .as_ref()
                .map(|value| self.decode_value(value)),
            interactive: bytecode
                .interactive
                .as_ref()
                .map(|value| self.decode_value(value)),
        })
    }

    fn decode_closure_value(&mut self, kind: VecLikeType, slots: &[CachedExpr]) -> Value {
        let slot_values: Vec<Value> = slots.iter().map(|item| self.decode_value(item)).collect();
        match kind {
            VecLikeType::Lambda => Value::make_lambda_with_slots(slot_values),
            VecLikeType::Macro => Value::make_macro_with_slots(slot_values),
            _ => Value::NIL,
        }
    }

    fn decode_hash_table(&mut self, table: &CachedHashTable) -> Value {
        let entries = table
            .entries
            .iter()
            .map(|(key, value)| (self.decode_value(key), self.decode_value(value)))
            .collect();
        build_hash_table_literal_value(
            decode_hash_table_test(&table.test),
            table.test_name.as_deref().map(intern),
            table.size,
            table.weakness.as_ref().map(decode_hash_table_weakness),
            table.rehash_size,
            table.rehash_threshold,
            entries,
        )
    }

    fn decode_value(&mut self, expr: &CachedExpr) -> Value {
        match expr {
            CachedExpr::Int(n) => Value::fixnum(*n),
            CachedExpr::Float(f) => Value::make_float(*f),
            CachedExpr::Symbol(name) if name == "nil" => Value::NIL,
            CachedExpr::Symbol(name) if name == "t" => Value::T,
            CachedExpr::Symbol(name) => Value::symbol(name),
            CachedExpr::UninternedSymbol { slot, name } => {
                let sym = *self
                    .uninterned_slots
                    .entry(*slot)
                    .or_insert_with(|| intern_uninterned(name));
                Value::from_sym_id(sym)
            }
            CachedExpr::ReaderLoadFileName => Value::symbol("load-file-name"),
            CachedExpr::Keyword(name) => Value::keyword(name),
            CachedExpr::Str(s) => Value::string(s.clone()),
            CachedExpr::Char(c) => Value::char(*c),
            CachedExpr::List(items) => {
                Value::list(items.iter().map(|item| self.decode_value(item)).collect())
            }
            CachedExpr::Vector(items) => {
                Value::vector(items.iter().map(|item| self.decode_value(item)).collect())
            }
            CachedExpr::Record(items) => {
                let values: Vec<Value> = items.iter().map(|item| self.decode_value(item)).collect();
                Value::make_record(values)
            }
            CachedExpr::Lambda(slots) => self.decode_closure_value(VecLikeType::Lambda, slots),
            CachedExpr::Macro(slots) => self.decode_closure_value(VecLikeType::Macro, slots),
            CachedExpr::ByteCode(bytecode) => self.decode_bytecode_value(bytecode),
            CachedExpr::HashTable(table) => self.decode_hash_table(table),
            CachedExpr::DottedList(items, tail) => {
                let tail_value = self.decode_value(tail);
                items.iter().rev().fold(tail_value, |acc, item| {
                    Value::cons(self.decode_value(item), acc)
                })
            }
            CachedExpr::Bool(true) => Value::T,
            CachedExpr::Bool(false) => Value::NIL,
        }
    }

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
            CachedExpr::Record(items) => {
                let values: Vec<Value> = items.iter().map(|item| self.decode_value(item)).collect();
                Expr::OpaqueValueRef(
                    OPAQUE_POOL.with(|pool| pool.borrow_mut().insert(Value::make_record(values))),
                )
            }
            CachedExpr::Lambda(slots) => {
                let value = self.decode_closure_value(VecLikeType::Lambda, slots);
                Expr::OpaqueValueRef(OPAQUE_POOL.with(|pool| pool.borrow_mut().insert(value)))
            }
            CachedExpr::Macro(slots) => {
                let value = self.decode_closure_value(VecLikeType::Macro, slots);
                Expr::OpaqueValueRef(OPAQUE_POOL.with(|pool| pool.borrow_mut().insert(value)))
            }
            CachedExpr::ByteCode(bytecode) => {
                let value = self.decode_bytecode_value(bytecode);
                Expr::OpaqueValueRef(OPAQUE_POOL.with(|pool| pool.borrow_mut().insert(value)))
            }
            CachedExpr::HashTable(table) => {
                let value = self.decode_hash_table(table);
                Expr::OpaqueValueRef(OPAQUE_POOL.with(|pool| pool.borrow_mut().insert(value)))
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
/// Returns `None` if any form contains an `OpaqueValueRef` that cannot be
/// serialized (e.g., a lambda or subr embedded by eval-when-compile).
pub fn serialize_neobc(
    source_hash: &str,
    lexical_binding: bool,
    compiled_forms: &[CompiledForm],
) -> Option<Vec<u8>> {
    serialize_neobc_detailed(source_hash, lexical_binding, compiled_forms).ok()
}

pub fn serialize_neobc_detailed(
    source_hash: &str,
    lexical_binding: bool,
    compiled_forms: &[CompiledForm],
) -> Result<Vec<u8>, UnsupportedValue> {
    serialize_neobc_with_surface_detailed(source_hash, lexical_binding, compiled_forms, None)
}

pub fn serialize_neobc_with_surface_detailed(
    source_hash: &str,
    lexical_binding: bool,
    compiled_forms: &[CompiledForm],
    surface_fingerprint: Option<&str>,
) -> Result<Vec<u8>, UnsupportedValue> {
    let mut builder = NeobcBuilder::new(source_hash, lexical_binding);
    if let Some(surface_fingerprint) = surface_fingerprint {
        builder.set_surface_fingerprint(surface_fingerprint);
    }
    for (index, form) in compiled_forms.iter().enumerate() {
        let prefix = format!("forms[{index}]");
        match form {
            CompiledForm::Eval(value) => builder
                .push_eval_value_detailed(value)
                .map_err(|err| err.with_path_prefix(&prefix))?,
            CompiledForm::EagerEval(value) => builder
                .push_eager_eval_value_detailed(value)
                .map_err(|err| err.with_path_prefix(&prefix))?,
            CompiledForm::Constant(value) => builder
                .push_constant_value_detailed(value)
                .map_err(|err| err.with_path_prefix(&prefix))?,
        }
    }
    builder.finish_bytes().ok_or_else(|| {
        UnsupportedValue::new(
            "root",
            "forms contained non-serializable runtime values after encoding",
        )
    })
}

pub(crate) fn transplant_value_pair(
    first: &Value,
    second: &Value,
) -> Result<(Value, Value), UnsupportedValue> {
    let mut encoder = ExprEncoder::default();
    let first_cached = encoder
        .encode_value(first)
        .ok_or_else(|| encoder.unsupported_value(first).with_path_prefix("pair[0]"))?;
    let second_cached = encoder.encode_value(second).ok_or_else(|| {
        encoder
            .unsupported_value(second)
            .with_path_prefix("pair[1]")
    })?;

    let mut decoder = ExprDecoder::default();
    Ok((
        decoder.decode_value(&first_cached),
        decoder.decode_value(&second_cached),
    ))
}

/// Serialize already-expanded top-level expressions to `.neobc`.
pub fn serialize_neobc_exprs(
    source_hash: &str,
    lexical_binding: bool,
    forms: &[Expr],
) -> Option<Vec<u8>> {
    let mut builder = NeobcBuilder::new(source_hash, lexical_binding);
    for form in forms {
        builder.push_eval_expr(form)?;
    }
    builder.finish_bytes()
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
    write_neobc_with_surface(path, source_hash, lexical_binding, compiled_forms, None)
}

pub fn write_neobc_with_surface(
    path: &Path,
    source_hash: &str,
    lexical_binding: bool,
    compiled_forms: &[CompiledForm],
    surface_fingerprint: Option<&str>,
) -> std::io::Result<()> {
    let bytes = serialize_neobc_with_surface_detailed(
        source_hash,
        lexical_binding,
        compiled_forms,
        surface_fingerprint,
    )
    .map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "compiled forms contain non-serializable values",
        )
    })?;
    std::fs::write(path, bytes)
}

/// Write already-expanded top-level expressions to a `.neobc` file on disk.
pub fn write_neobc_exprs(
    path: &Path,
    source_hash: &str,
    lexical_binding: bool,
    forms: &[Expr],
) -> std::io::Result<()> {
    let bytes = serialize_neobc_exprs(source_hash, lexical_binding, forms).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "expanded forms contain non-serializable values",
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
    Eval(Value),
    /// Re-run eager macroexpansion for this source form at load time.
    EagerEval(Value),
    /// A pre-computed constant (result of `eval-when-compile`).
    Constant(Value),
}

impl LoadedForm {
    pub(crate) fn root_value(&self) -> Value {
        match self {
            Self::Eval(value) | Self::EagerEval(value) | Self::Constant(value) => *value,
        }
    }
}

/// Read and validate a `.neobc` file.
///
/// `expected_hash` is the SHA-256 hex digest of the current source; if the
/// file's stored hash does not match, `Err` is returned (stale cache).
/// Pass an empty string to skip the hash check.
pub fn read_neobc(path: &Path, expected_hash: &str) -> std::io::Result<LoadedNeobc> {
    read_neobc_with_surface(path, expected_hash, None)
}

pub fn read_neobc_with_surface(
    path: &Path,
    expected_hash: &str,
    expected_surface: Option<&str>,
) -> std::io::Result<LoadedNeobc> {
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
    if let Some(expected_surface) = expected_surface
        && let Some(actual_surface) = file.surface_fingerprint.as_deref()
        && actual_surface != expected_surface
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "neobc surface mismatch: expected {}, got {}",
                expected_surface, actual_surface
            ),
        ));
    }

    // Decode forms.
    let mut decoder = ExprDecoder::default();
    let forms = file
        .forms
        .iter()
        .map(|sf| match sf {
            SerializedForm::Eval(cached) => LoadedForm::Eval(decoder.decode_value(cached)),
            SerializedForm::EagerEval(cached) => {
                LoadedForm::EagerEval(decoder.decode_value(cached))
            }
            SerializedForm::Constant(cached) => LoadedForm::Constant(decoder.decode_value(cached)),
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
#[path = "file_compile_format_test.rs"]
mod tests;
