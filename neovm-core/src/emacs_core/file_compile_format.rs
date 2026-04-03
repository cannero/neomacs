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
use std::rc::Rc;

use super::builtins::parse_lambda_params_from_value;
use super::bytecode::chunk::ByteCodeFunction;
use super::bytecode::opcode::Op;
use super::eval::{OPAQUE_POOL, quote_to_value, value_to_expr};
use super::expr::Expr;
use super::file_compile::CompiledForm;
use super::intern::{SymId, intern, intern_uninterned, is_canonical_id, resolve_sym};
use super::value::{
    HashKey, HashTableTest, HashTableWeakness, LambdaData, LambdaParams, Value,
    build_hash_table_literal_value, list_to_vec,
};
use crate::tagged::header::{
    CLOSURE_ARGLIST, CLOSURE_CODE, CLOSURE_CONSTANTS, CLOSURE_DOC_STRING, CLOSURE_INTERACTIVE,
    VecLikeType,
};

/// Magic bytes identifying a `.neobc` file.
const NEOBC_MAGIC: &[u8] = b"NEOVM-BC-V3\n";

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
    fn encode_sym_ref(&mut self, id: SymId) -> CachedSymRef {
        let name = resolve_sym(id).to_owned();
        if is_canonical_symbol_id(id) {
            CachedSymRef::Symbol(name)
        } else {
            let slot = *self.uninterned_slots.entry(id).or_insert_with(|| {
                let slot = self.next_slot;
                self.next_slot += 1;
                slot
            });
            CachedSymRef::UninternedSymbol { slot, name }
        }
    }

    fn encode_lambda_params(&mut self, params: &LambdaParams) -> CachedLambdaParams {
        CachedLambdaParams {
            required: params
                .required
                .iter()
                .map(|id| self.encode_sym_ref(*id))
                .collect(),
            optional: params
                .optional
                .iter()
                .map(|id| self.encode_sym_ref(*id))
                .collect(),
            rest: params.rest.map(|id| self.encode_sym_ref(id)),
        }
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
            params: self.encode_lambda_params(&bc.params),
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
            test_name: table.test_name.map(|id| resolve_sym(id).to_owned()),
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
                let name = resolve_sym(id).to_owned();
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
        super::value::ValueKind::Symbol(id) => format!("symbol {}", resolve_sym(id)),
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
        | super::value::ValueKind::Symbol(_) => {
            UnsupportedValue::new(path, "value was expected to be serializable")
        }
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
    lexical_binding: bool,
    encoder: ExprEncoder,
    forms: Vec<SerializedForm>,
}

impl NeobcBuilder {
    pub(crate) fn new(source_hash: &str, lexical_binding: bool) -> Self {
        Self {
            source_hash: source_hash.to_owned(),
            lexical_binding,
            encoder: ExprEncoder::default(),
            forms: Vec::new(),
        }
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

    pub(crate) fn len(&self) -> usize {
        self.forms.len()
    }

    fn finish_bytes(self) -> Option<Vec<u8>> {
        let file = NeobcFile {
            source_hash: self.source_hash,
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
            .map(|item| quote_to_value(&self.decode(item)))
            .collect();
        Value::make_bytecode(ByteCodeFunction {
            ops: bytecode.ops.clone(),
            constants,
            max_stack: bytecode.max_stack,
            params: self.decode_lambda_params(&bytecode.params),
            lexical: bytecode.lexical,
            env: bytecode
                .env
                .as_ref()
                .map(|value| quote_to_value(&self.decode(value))),
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
                .map(|value| quote_to_value(&self.decode(value))),
            interactive: bytecode
                .interactive
                .as_ref()
                .map(|value| quote_to_value(&self.decode(value))),
        })
    }

    fn decode_closure_value(&mut self, kind: VecLikeType, slots: &[CachedExpr]) -> Value {
        let slot_values: Vec<Value> = slots
            .iter()
            .map(|item| quote_to_value(&self.decode(item)))
            .collect();
        let arglist = slot_values
            .get(CLOSURE_ARGLIST)
            .copied()
            .unwrap_or(Value::NIL);
        let body_value = slot_values.get(CLOSURE_CODE).copied().unwrap_or(Value::NIL);
        let env_value = slot_values
            .get(CLOSURE_CONSTANTS)
            .copied()
            .unwrap_or(Value::NIL);
        let doc_value = slot_values
            .get(CLOSURE_DOC_STRING)
            .copied()
            .unwrap_or(Value::NIL);
        let interactive = slot_values
            .get(CLOSURE_INTERACTIVE)
            .copied()
            .filter(|value| !value.is_nil());
        let params = parse_lambda_params_from_value(&arglist)
            .unwrap_or_else(|_| crate::emacs_core::value::LambdaParams::simple(Vec::new()));
        let body = list_to_vec(&body_value)
            .unwrap_or_default()
            .into_iter()
            .map(|item| value_to_expr(&item))
            .collect();
        let env = (!env_value.is_nil()).then_some(env_value);
        let (docstring, doc_form) = if doc_value.is_nil() {
            (None, None)
        } else if let Some(doc) = doc_value.as_str() {
            (Some(doc.to_owned()), None)
        } else {
            (None, Some(doc_value))
        };
        let data = LambdaData {
            params,
            body: Rc::new(body),
            env,
            docstring,
            doc_form,
            interactive,
        };
        match kind {
            VecLikeType::Lambda => Value::make_lambda(data),
            VecLikeType::Macro => Value::make_macro(data),
            _ => Value::NIL,
        }
    }

    fn decode_hash_table(&mut self, table: &CachedHashTable) -> Value {
        let entries = table
            .entries
            .iter()
            .map(|(key, value)| {
                (
                    quote_to_value(&self.decode(key)),
                    quote_to_value(&self.decode(value)),
                )
            })
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
                let expr_items: Vec<Expr> = items.iter().map(|item| self.decode(item)).collect();
                let values: Vec<Value> = expr_items.iter().map(quote_to_value).collect();
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
    let mut builder = NeobcBuilder::new(source_hash, lexical_binding);
    for form in compiled_forms {
        match form {
            CompiledForm::Eval(value) => builder.push_eval_value(value)?,
            CompiledForm::Constant(value) => builder.push_constant_value(value)?,
        }
    }
    builder.finish_bytes()
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
    let bytes = serialize_neobc(source_hash, lexical_binding, compiled_forms).ok_or_else(|| {
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
    Eval(Expr),
    /// Re-run eager macroexpansion for this source form at load time.
    EagerEval(Expr),
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
            SerializedForm::EagerEval(cached) => LoadedForm::EagerEval(decoder.decode(cached)),
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
    use crate::emacs_core::bytecode::Compiler;
    use crate::emacs_core::eval::Context;
    use crate::emacs_core::file_compile::compile_file_forms;
    use crate::emacs_core::intern::intern;
    use crate::emacs_core::parser::parse_forms;
    use crate::emacs_core::value::{LambdaData, LambdaParams};
    use std::rc::Rc;

    fn sample_lambda_data() -> LambdaData {
        LambdaData {
            params: LambdaParams::simple(vec![intern("x")]),
            body: Rc::new(parse_forms("(+ x 1)").unwrap()),
            env: Some(Value::list(vec![Value::cons(
                Value::symbol("x"),
                Value::fixnum(41),
            )])),
            docstring: Some("sample closure".to_owned()),
            doc_form: None,
            interactive: Some(Value::string("p".to_owned())),
        }
    }

    #[test]
    fn test_source_sha256() {
        crate::test_utils::init_test_tracing();
        let hash = source_sha256("hello world");
        // Known SHA-256 of "hello world".
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn test_roundtrip_simple_eval_form() {
        crate::test_utils::init_test_tracing();
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
        assert!(matches!(
            &loaded.forms[0],
            LoadedForm::Eval(_) | LoadedForm::EagerEval(_)
        ));

        // Re-evaluate the loaded form and check result.
        if let LoadedForm::Eval(expr) | LoadedForm::EagerEval(expr) = &loaded.forms[0] {
            let mut eval2 = Context::new();
            let result = eval2.eval(expr).unwrap();
            assert_eq!(result, Value::fixnum(3));
        }
    }

    #[test]
    fn test_roundtrip_eval_when_compile() {
        crate::test_utils::init_test_tracing();
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
            LoadedForm::Constant(v) => assert_eq!(*v, Value::fixnum(30)),
            other => panic!("expected Constant, got Eval"),
        }
    }

    #[test]
    fn test_roundtrip_mixed_forms() {
        crate::test_utils::init_test_tracing();
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
        assert!(matches!(
            &loaded.forms[0],
            LoadedForm::Eval(_) | LoadedForm::EagerEval(_)
        ));
        assert!(matches!(&loaded.forms[1], LoadedForm::Constant(_)));
        assert!(matches!(
            &loaded.forms[2],
            LoadedForm::Eval(_) | LoadedForm::EagerEval(_)
        ));
    }

    #[test]
    fn test_hash_mismatch_rejected() {
        crate::test_utils::init_test_tracing();
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
        crate::test_utils::init_test_tracing();
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
        crate::test_utils::init_test_tracing();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.neobc");
        std::fs::write(&path, b"NOT-A-NEOBC-FILE").unwrap();

        let err = read_neobc(&path, "").unwrap_err();
        assert!(err.to_string().contains("magic"));
    }

    #[test]
    fn test_truncated_file_rejected() {
        crate::test_utils::init_test_tracing();
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
        crate::test_utils::init_test_tracing();
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
    fn test_write_neobc_exprs_round_trip() {
        crate::test_utils::init_test_tracing();
        let forms = parse_forms("(progn (setq x 1) x)").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exprs.neobc");
        let hash = source_sha256("(progn (setq x 1) x)");

        write_neobc_exprs(&path, &hash, false, &forms).unwrap();

        let loaded = read_neobc(&path, &hash).unwrap();
        assert!(!loaded.lexical_binding);
        assert_eq!(loaded.forms.len(), 1);
        match &loaded.forms[0] {
            LoadedForm::Eval(expr) | LoadedForm::EagerEval(expr) => {
                assert!(matches!(expr, Expr::List(_)))
            }
            LoadedForm::Constant(_) => panic!("expected Eval form"),
        }
    }

    #[test]
    fn test_roundtrip_string_constant() {
        crate::test_utils::init_test_tracing();
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
        crate::test_utils::init_test_tracing();
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
            LoadedForm::Constant(v) => assert_eq!(*v, Value::NIL),
            _ => panic!("expected Constant"),
        }
    }

    #[test]
    fn test_roundtrip_lexical_binding_flag() {
        crate::test_utils::init_test_tracing();
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

    #[test]
    fn test_neobc_rejects_propertized_string_runtime_values() {
        crate::test_utils::init_test_tracing();
        let value = Value::string_with_text_properties(
            "x",
            vec![super::super::value::StringTextPropertyRun {
                start: 0,
                end: 1,
                plist: Value::list(vec![
                    Value::keyword("face"),
                    Value::make_symbol("bold".to_owned()),
                ]),
            }],
        );
        let mut builder = NeobcBuilder::new("hash", false);
        let err = builder.push_eval_value_detailed(&value).unwrap_err();
        assert_eq!(err.path(), "root");
        assert_eq!(err.detail(), "string with text properties");
    }

    #[test]
    fn test_neobc_reports_nested_unsupported_runtime_value_path() {
        crate::test_utils::init_test_tracing();
        let value = Value::vector(vec![Value::fixnum(1), Value::subr(intern("car"))]);
        let mut builder = NeobcBuilder::new("hash", false);
        let err = builder.push_eval_value_detailed(&value).unwrap_err();
        assert_eq!(err.path(), "root[1]");
        assert!(err.detail().contains("Subr"));
    }

    #[test]
    fn test_roundtrip_record_literal_expr() {
        crate::test_utils::init_test_tracing();
        let src = "#s(cl-slot-descriptor foo 1)";
        let forms = parse_forms(src).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("record.neobc");
        let hash = source_sha256(src);

        write_neobc_exprs(&path, &hash, false, &forms).unwrap();

        let loaded = read_neobc(&path, &hash).unwrap();
        match &loaded.forms[0] {
            LoadedForm::Eval(expr) | LoadedForm::EagerEval(expr) => {
                let mut eval = Context::new();
                let value = eval.eval(expr).unwrap();
                assert!(value.is_record());
                let items = value.as_record_data().unwrap();
                assert_eq!(items.len(), 3);
                assert_eq!(items[0].as_symbol_name(), Some("cl-slot-descriptor"));
                assert_eq!(items[1].as_symbol_name(), Some("foo"));
                assert_eq!(items[2], Value::fixnum(1));
            }
            LoadedForm::Constant(_) => panic!("expected Eval form"),
        }
    }

    #[test]
    fn test_roundtrip_hash_table_literal_expr() {
        crate::test_utils::init_test_tracing();
        let src = "#s(hash-table size 3 test equal data (\"a\" 1 \"b\" 2))";
        let forms = parse_forms(src).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hash-table.neobc");
        let hash = source_sha256(src);

        write_neobc_exprs(&path, &hash, false, &forms).unwrap();

        let loaded = read_neobc(&path, &hash).unwrap();
        match &loaded.forms[0] {
            LoadedForm::Eval(expr) | LoadedForm::EagerEval(expr) => {
                let mut eval = Context::new();
                let value = eval.eval(expr).unwrap();
                let table = value.as_hash_table().unwrap();
                assert_eq!(table.test, crate::emacs_core::value::HashTableTest::Equal);
                assert_eq!(table.size, 3);
                assert_eq!(table.data.len(), 2);
            }
            LoadedForm::Constant(_) => panic!("expected Eval form"),
        }
    }

    #[test]
    fn test_roundtrip_record_with_nested_hash_table_runtime_value() {
        crate::test_utils::init_test_tracing();
        let table = Value::hash_table_with_options(
            crate::emacs_core::value::HashTableTest::Eq,
            2,
            None,
            1.5,
            0.8125,
        );
        let _ = table.with_hash_table_mut(|ht| {
            ht.test_name = Some(intern("eq"));
            let alpha = Value::symbol("alpha");
            let beta = Value::symbol("beta");
            let alpha_key = alpha.to_hash_key(&ht.test);
            let beta_key = beta.to_hash_key(&ht.test);
            ht.data.insert(alpha_key.clone(), Value::fixnum(1));
            ht.key_snapshots.insert(alpha_key.clone(), alpha);
            ht.insertion_order.push(alpha_key);
            ht.data.insert(beta_key.clone(), Value::fixnum(2));
            ht.key_snapshots.insert(beta_key.clone(), beta);
            ht.insertion_order.push(beta_key);
        });
        let record = Value::make_record(vec![Value::symbol("class"), Value::fixnum(7), table]);

        let mut builder = NeobcBuilder::new("hash", false);
        builder.push_eval_value_detailed(&record).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested-record.neobc");
        builder.write(&path).unwrap();

        let loaded = read_neobc(&path, "hash").unwrap();
        match &loaded.forms[0] {
            LoadedForm::Eval(expr) | LoadedForm::EagerEval(expr) => {
                let mut eval = Context::new();
                let value = eval.eval(expr).unwrap();
                let items = value.as_record_data().unwrap();
                let table = items[2].as_hash_table().unwrap();
                assert_eq!(table.test, crate::emacs_core::value::HashTableTest::Eq);
                assert_eq!(table.data.len(), 2);
                assert_eq!(table.test_name.map(resolve_sym), Some("eq"));
            }
            LoadedForm::Constant(_) => panic!("expected Eval form"),
        }
    }

    #[test]
    fn test_roundtrip_lambda_runtime_value() {
        crate::test_utils::init_test_tracing();
        let lambda = Value::make_lambda(sample_lambda_data());
        let mut builder = NeobcBuilder::new("hash", false);
        builder.push_eval_value_detailed(&lambda).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lambda.neobc");
        builder.write(&path).unwrap();

        let loaded = read_neobc(&path, "hash").unwrap();
        match &loaded.forms[0] {
            LoadedForm::Eval(expr) | LoadedForm::EagerEval(expr) => {
                let mut eval = Context::new();
                let value = eval.eval(expr).unwrap();
                assert!(value.is_lambda());
                assert_eq!(value.closure_docstring(), Some(Some("sample closure")));
                assert_eq!(value.closure_interactive(), Some(Some(Value::string("p"))));
                assert_eq!(
                    value
                        .closure_params()
                        .map(|params| params.required.clone())
                        .unwrap_or_default(),
                    vec![intern("x")]
                );
            }
            LoadedForm::Constant(_) => panic!("expected Eval form"),
        }
    }

    #[test]
    fn test_roundtrip_lambda_runtime_value_with_nested_lambda_env() {
        crate::test_utils::init_test_tracing();
        let nested = Value::make_lambda(sample_lambda_data());
        let outer = Value::make_lambda(LambdaData {
            params: LambdaParams::simple(vec![intern("y")]),
            body: Rc::new(parse_forms("(list y)").unwrap()),
            env: Some(Value::list(vec![nested])),
            docstring: Some("outer closure".to_owned()),
            doc_form: None,
            interactive: None,
        });
        let mut builder = NeobcBuilder::new("hash", false);
        builder.push_eval_value_detailed(&outer).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested-lambda-env.neobc");
        builder.write(&path).unwrap();

        let loaded = read_neobc(&path, "hash").unwrap();
        match &loaded.forms[0] {
            LoadedForm::Eval(expr) | LoadedForm::EagerEval(expr) => {
                let mut eval = Context::new();
                let value = eval.eval(expr).unwrap();
                assert!(value.is_lambda());
                assert_eq!(value.closure_docstring(), Some(Some("outer closure")));
                let env = value.closure_env().unwrap().unwrap();
                let items = crate::emacs_core::value::list_to_vec(&env).unwrap();
                assert_eq!(items.len(), 1);
                assert!(items[0].is_lambda());
                assert_eq!(items[0].closure_docstring(), Some(Some("sample closure")));
            }
            LoadedForm::Constant(_) => panic!("expected Eval form"),
        }
    }

    #[test]
    fn test_roundtrip_macro_runtime_value() {
        crate::test_utils::init_test_tracing();
        let macro_value = Value::make_macro(sample_lambda_data());
        let mut builder = NeobcBuilder::new("hash", false);
        builder.push_eval_value_detailed(&macro_value).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("macro.neobc");
        builder.write(&path).unwrap();

        let loaded = read_neobc(&path, "hash").unwrap();
        match &loaded.forms[0] {
            LoadedForm::Eval(expr) | LoadedForm::EagerEval(expr) => {
                let mut eval = Context::new();
                let value = eval.eval(expr).unwrap();
                assert!(value.is_macro());
                assert_eq!(value.closure_docstring(), Some(Some("sample closure")));
            }
            LoadedForm::Constant(_) => panic!("expected Eval form"),
        }
    }

    #[test]
    fn test_write_neobc_exprs_round_trips_opaque_lambda_refs() {
        crate::test_utils::init_test_tracing();
        let lambda = Value::make_lambda(sample_lambda_data());
        let exprs = vec![Expr::OpaqueValueRef(
            OPAQUE_POOL.with(|pool| pool.borrow_mut().insert(lambda)),
        )];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opaque-lambda.neobc");
        let hash = source_sha256("opaque-lambda");

        write_neobc_exprs(&path, &hash, false, &exprs).unwrap();

        let loaded = read_neobc(&path, &hash).unwrap();
        match &loaded.forms[0] {
            LoadedForm::Eval(expr) | LoadedForm::EagerEval(expr) => {
                let mut eval = Context::new();
                let value = eval.eval(expr).unwrap();
                assert!(value.is_lambda());
                assert_eq!(value.closure_docstring(), Some(Some("sample closure")));
            }
            LoadedForm::Constant(_) => panic!("expected Eval form"),
        }
    }

    #[test]
    fn test_roundtrip_bytecode_runtime_value() {
        crate::test_utils::init_test_tracing();
        let body = parse_forms("(+ x 1)").unwrap();
        let bytecode = Value::make_bytecode(
            Compiler::new(false).compile_lambda(&LambdaParams::simple(vec![intern("x")]), &body),
        );
        let mut builder = NeobcBuilder::new("hash", false);
        builder.push_eval_value_detailed(&bytecode).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bytecode.neobc");
        builder.write(&path).unwrap();

        let loaded = read_neobc(&path, "hash").unwrap();
        match &loaded.forms[0] {
            LoadedForm::Eval(expr) | LoadedForm::EagerEval(expr) => {
                let mut eval = Context::new();
                let value = eval.eval(expr).unwrap();
                assert!(value.is_bytecode());
                let bytecode = value.get_bytecode_data().expect("bytecode payload");
                assert_eq!(bytecode.params.required, vec![intern("x")]);
                assert!(!bytecode.ops.is_empty());
            }
            LoadedForm::Constant(_) => panic!("expected Eval form"),
        }
    }

    #[test]
    fn test_write_neobc_exprs_round_trips_opaque_bytecode_refs() {
        crate::test_utils::init_test_tracing();
        let body = parse_forms("(+ x 1)").unwrap();
        let bytecode = Value::make_bytecode(
            Compiler::new(false).compile_lambda(&LambdaParams::simple(vec![intern("x")]), &body),
        );
        let exprs = vec![Expr::OpaqueValueRef(
            OPAQUE_POOL.with(|pool| pool.borrow_mut().insert(bytecode)),
        )];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opaque-bytecode.neobc");
        let hash = source_sha256("opaque-bytecode");

        write_neobc_exprs(&path, &hash, false, &exprs).unwrap();

        let loaded = read_neobc(&path, &hash).unwrap();
        match &loaded.forms[0] {
            LoadedForm::Eval(expr) | LoadedForm::EagerEval(expr) => {
                let mut eval = Context::new();
                let value = eval.eval(expr).unwrap();
                assert!(value.is_bytecode());
                assert_eq!(
                    value
                        .get_bytecode_data()
                        .expect("bytecode payload")
                        .params
                        .required,
                    vec![intern("x")]
                );
            }
            LoadedForm::Constant(_) => panic!("expected Eval form"),
        }
    }
}
