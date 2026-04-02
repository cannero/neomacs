//! Portable dumper (pdump) for NeoVM.
//!
//! Serializes the post-bootstrap `Context` state to a binary file using
//! serde + bincode, then deserializes on startup to skip the 3-5s bootstrap.
//!
//! File format:
//! ```text
//! [8 bytes: magic "NEOPDUMP"]
//! [4 bytes: format version u32 LE]
//! [32 bytes: SHA-256 of bincode payload]
//! [4 bytes: payload length u32 LE]
//! [N bytes: bincode-serialized DumpContextState]
//! ```

pub mod convert;
pub mod types;

use std::io::{Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

use self::convert::*;
use self::types::DumpContextState;
use crate::emacs_core::eval::Context;
use crate::emacs_core::intern;
use crate::emacs_core::value;

const MAGIC: &[u8; 8] = b"NEOPDUMP";
const FORMAT_VERSION: u32 = 4;

/// Errors from dump/load operations.
#[derive(Debug)]
pub enum DumpError {
    Io(std::io::Error),
    BadMagic,
    UnsupportedVersion(u32),
    ChecksumMismatch,
    SerializationError(String),
    DeserializationError(String),
}

impl std::fmt::Display for DumpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DumpError::Io(e) => write!(f, "I/O error: {e}"),
            DumpError::BadMagic => write!(f, "not a valid pdump file (bad magic)"),
            DumpError::UnsupportedVersion(v) => write!(f, "unsupported pdump version {v}"),
            DumpError::ChecksumMismatch => write!(f, "pdump checksum mismatch (corrupted file)"),
            DumpError::SerializationError(s) => write!(f, "serialization error: {s}"),
            DumpError::DeserializationError(s) => write!(f, "deserialization error: {s}"),
        }
    }
}

impl std::error::Error for DumpError {}

impl From<std::io::Error> for DumpError {
    fn from(e: std::io::Error) -> Self {
        DumpError::Io(e)
    }
}

/// Serialize the evaluator state to a pdump file.
pub fn dump_to_file(eval: &Context, path: &Path) -> Result<(), DumpError> {
    let state = dump_evaluator(eval);

    let payload =
        bincode::serialize(&state).map_err(|e| DumpError::SerializationError(e.to_string()))?;

    let mut hasher = Sha256::new();
    hasher.update(&payload);
    let checksum = hasher.finalize();

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(MAGIC)?;
    file.write_all(&FORMAT_VERSION.to_le_bytes())?;
    file.write_all(&checksum)?;
    file.write_all(&(payload.len() as u32).to_le_bytes())?;
    file.write_all(&payload)?;
    file.flush()?;
    file.as_file().sync_all()?;

    match file.persist(path) {
        Ok(_) => Ok(()),
        Err(err) => {
            if err.error.kind() == std::io::ErrorKind::AlreadyExists && path.exists() {
                Ok(())
            } else {
                Err(DumpError::Io(err.error))
            }
        }
    }
}

/// Load evaluator state from a pdump file.
///
/// This reconstructs a full `Context` from the serialized state,
/// setting up thread-local pointers and resetting caches.
pub fn load_from_dump(path: &Path) -> Result<Context, DumpError> {
    let mut file = std::fs::File::open(path)?;

    // Read and validate header
    let mut magic = [0u8; 8];
    file.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(DumpError::BadMagic);
    }

    let mut version_bytes = [0u8; 4];
    file.read_exact(&mut version_bytes)?;
    let version = u32::from_le_bytes(version_bytes);
    if version != FORMAT_VERSION {
        return Err(DumpError::UnsupportedVersion(version));
    }

    let mut expected_checksum = [0u8; 32];
    file.read_exact(&mut expected_checksum)?;

    let mut len_bytes = [0u8; 4];
    file.read_exact(&mut len_bytes)?;
    let payload_len = u32::from_le_bytes(len_bytes) as usize;

    let mut payload = vec![0u8; payload_len];
    file.read_exact(&mut payload)?;

    // Validate checksum
    let mut hasher = Sha256::new();
    hasher.update(&payload);
    let actual_checksum = hasher.finalize();
    if actual_checksum.as_slice() != &expected_checksum {
        return Err(DumpError::ChecksumMismatch);
    }

    // Deserialize
    let state: types::DumpContextState = bincode::deserialize(&payload)
        .map_err(|e| DumpError::DeserializationError(e.to_string()))?;

    // Reconstruct evaluator
    reconstruct_evaluator(&state)
}

/// Clone a live evaluator through the pdump conversion pipeline.
///
/// This gives bootstrap/load code an isolated working evaluator with the same
/// logical runtime state, without sharing heap objects that can be mutated
/// during eager macroexpansion.
pub fn snapshot_evaluator(eval: &Context) -> DumpContextState {
    dump_evaluator(eval)
}

/// Reconstruct an evaluator from a previously captured in-memory pdump snapshot.
pub fn restore_snapshot(state: &DumpContextState) -> Result<Context, DumpError> {
    reconstruct_evaluator(state)
}

/// Clone a live evaluator through the pdump conversion pipeline.
///
/// Prefer `snapshot_evaluator` + `restore_snapshot` when cloning the same
/// template repeatedly; that avoids rebuilding the intermediate dump state.
pub fn clone_evaluator(eval: &Context) -> Result<Context, DumpError> {
    restore_snapshot(&snapshot_evaluator(eval))
}

/// Reconstruct an `Context` from deserialized dump state.
fn reconstruct_evaluator(state: &DumpContextState) -> Result<Context, DumpError> {
    // 1. Reconstruct the global append-only symbol table before any values that
    // refer to SymIds are loaded.
    load_interner(&state.interner);

    // 2. Reconstruct heap (phase 1: all objects except hash table entries)
    let mut heap = Box::new(load_heap(&state.heap)?);

    // 2b. Phase 2: populate hash table entries (requires CURRENT_HEAP for HashKey::Str hashing)
    load_heap_hash_tables(&mut heap, &state.heap);

    // 3. Reset thread-local caches (same as Context::new())
    super::syntax::reset_syntax_thread_locals();
    super::casetab::reset_casetab_thread_locals();
    super::category::reset_category_thread_locals();
    crate::tagged::value::reset_current_subrs();
    value::reset_string_text_properties();
    super::ccl::reset_ccl_registry();
    super::dispnew::pure::reset_dispnew_thread_locals();
    super::font::clear_font_cache_state();
    super::builtins::reset_builtins_thread_locals();
    super::timefns::reset_timefns_thread_locals();

    // 4. Restore string text properties
    let stp: Vec<(u64, crate::buffer::text_props::TextPropertyTable)> = state
        .string_text_props
        .iter()
        .map(|(key, runs)| (*key, load_text_property_table(runs)))
        .collect();
    value::restore_string_text_props(stp);

    // 4b. Restore thread-local registries whose contents are semantic runtime
    // state, not disposable caches.
    load_charset_registry(&state.charset_registry);
    load_fontset_registry(&state.fontset_registry);

    // 5. Reconstruct all subsystems
    let obarray = load_obarray(&state.obarray);
    let lexenv = load_value(&state.lexenv);
    let features: Vec<_> = state.features.iter().map(|id| intern::SymId(*id)).collect();
    let require_stack: Vec<_> = state
        .require_stack
        .iter()
        .map(|id| intern::SymId(*id))
        .collect();

    let eval = Context::from_dump(
        obarray,
        lexenv,
        features,
        require_stack,
        load_buffer_manager(&state.buffers),
        load_autoload_manager(&state.autoloads),
        load_custom_manager(&state.custom),
        load_mode_registry(&state.modes),
        load_coding_system_manager(&state.coding_systems),
        load_face_table(&state.face_table),
        load_abbrev_manager(&state.abbrevs),
        load_interactive_registry(&state.interactive),
        load_rectangle(&state.rectangle),
        load_value(&state.standard_syntax_table),
        load_value(&state.standard_category_table),
        load_value(&state.current_local_map),
        load_kmacro(&state.kmacro),
        load_register_manager(&state.registers),
        load_bookmark_manager(&state.bookmarks),
        load_watcher_list(&state.watchers),
    );

    Ok(eval)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emacs_core::pdump::types::{
        DumpByteCodeFunction, DumpHeapObject, DumpLambdaParams, DumpOp,
    };
    use crate::emacs_core::value::Value;

    #[test]
    fn test_pdump_round_trip_basic() {
        crate::test_utils::init_test_tracing();
        // Create a minimal evaluator
        let mut eval = Context::new();

        // Set a symbol value to verify round-trip
        eval.obarray
            .set_symbol_value("test-pdump-var", Value::fixnum(42));

        // Dump to temp file
        let dir = tempfile::tempdir().unwrap();
        let dump_path = dir.path().join("test.pdump");
        dump_to_file(&eval, &dump_path).expect("dump should succeed");

        // Load from dump
        let loaded = load_from_dump(&dump_path).expect("load should succeed");

        // Verify the symbol value survived
        assert_eq!(
            loaded.obarray.symbol_value("test-pdump-var"),
            Some(&Value::fixnum(42))
        );
    }

    #[test]
    fn test_pdump_bad_magic() {
        crate::test_utils::init_test_tracing();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.pdump");
        std::fs::write(&path, b"BADMAGIC").unwrap();
        assert!(matches!(load_from_dump(&path), Err(DumpError::BadMagic)));
    }

    #[test]
    fn test_pdump_round_trip_bootstrap() {
        crate::test_utils::init_test_tracing();
        // Bootstrap, dump, load, and verify eval works on loaded state
        let eval = crate::emacs_core::load::create_bootstrap_evaluator()
            .expect("bootstrap should succeed");

        let dir = tempfile::tempdir().unwrap();
        let dump_path = dir.path().join("bootstrap.pdump");

        let dump_start = std::time::Instant::now();
        dump_to_file(&eval, &dump_path).expect("dump should succeed");
        let dump_time = dump_start.elapsed();
        let file_size = std::fs::metadata(&dump_path).unwrap().len();
        eprintln!(
            "pdump: dump took {dump_time:.2?}, file size: {file_size} bytes ({:.1} MB)",
            file_size as f64 / 1048576.0
        );

        // Drop original evaluator before loading to test standalone load
        drop(eval);

        let load_start = std::time::Instant::now();
        let mut loaded = load_from_dump(&dump_path).expect("load should succeed");
        let load_time = load_start.elapsed();
        eprintln!("pdump: load took {load_time:.2?}");

        // Verify the loaded evaluator can evaluate Elisp
        let forms = crate::emacs_core::parser::parse_forms("(+ 1 2)").unwrap();
        let result = loaded.eval_expr(&forms[0]).expect("eval should succeed");
        assert_eq!(result, Value::fixnum(3));

        // Verify features survived (bootstrap sets many features)
        // Note: subr.el does NOT call (provide 'subr); use 'backquote instead
        let forms = crate::emacs_core::parser::parse_forms("(featurep 'backquote)").unwrap();
        let result = loaded
            .eval_expr(&forms[0])
            .expect("featurep should succeed");
        assert_eq!(result, Value::T, "featurep 'backquote should be t");

        // Verify a bootstrapped function works
        let forms = crate::emacs_core::parser::parse_forms("(length '(a b c))").unwrap();
        let result = loaded.eval_expr(&forms[0]).expect("eval should succeed");
        assert_eq!(result, Value::fixnum(3));

        // Verify string operations (tests heap String objects)
        let forms =
            crate::emacs_core::parser::parse_forms("(concat \"hello\" \" \" \"world\")").unwrap();
        let result = loaded.eval_expr(&forms[0]).expect("eval should succeed");
        assert_eq!(crate::emacs_core::print_value(&result), "\"hello world\"");

        // Verify hash table access (tests hash table round-trip)
        let forms = crate::emacs_core::parser::parse_forms(
            "(let ((h (make-hash-table :test 'equal))) (puthash \"key\" 42 h) (gethash \"key\" h))",
        )
        .unwrap();
        let result = loaded.eval_expr(&forms[0]).expect("eval should succeed");
        assert_eq!(result, Value::fixnum(42));

        // Verify defun works (tests lambda/macro round-trip)
        let forms = crate::emacs_core::parser::parse_forms(
            "(progn (defun pdump-test-fn (x) (* x x)) (pdump-test-fn 7))",
        )
        .unwrap();
        let result = loaded.eval_expr(&forms[0]).expect("eval should succeed");
        assert_eq!(result, Value::fixnum(49));
    }

    #[test]
    fn test_pdump_round_trip_preserves_runtime_derived_mode_syntax() {
        crate::test_utils::init_test_tracing();
        let mut eval = crate::emacs_core::load::create_bootstrap_evaluator()
            .expect("bootstrap should succeed");
        crate::emacs_core::load::apply_runtime_startup_state(&mut eval)
            .expect("runtime startup should succeed");

        let probe = crate::emacs_core::parser::parse_forms(
            r#"(list
                 (boundp 'lisp-data-mode-syntax-table)
                 (boundp 'emacs-lisp-mode-syntax-table)
                 (boundp 'lisp-interaction-mode-syntax-table)
                 (functionp (symbol-function 'lisp-interaction-mode))
                 (eq (char-table-parent emacs-lisp-mode-syntax-table)
                     lisp-data-mode-syntax-table)
                 (eq (char-table-parent lisp-interaction-mode-syntax-table)
                     emacs-lisp-mode-syntax-table)
                 (char-syntax ?\n)
                 (char-syntax ?\;)
                 (char-syntax ?{)
                 (char-syntax ?'))"#,
        )
        .unwrap();
        let full_result = eval
            .eval_expr(&probe[0])
            .expect("full bootstrap probe should run");
        assert_eq!(
            crate::emacs_core::print_value_with_buffers(&full_result, &eval.buffers),
            "(t t t t t t 62 60 95 39)"
        );

        let dir = tempfile::tempdir().unwrap();
        let dump_path = dir.path().join("derived-mode-syntax.pdump");
        dump_to_file(&eval, &dump_path).expect("dump should succeed");
        drop(eval);

        let mut loaded = load_from_dump(&dump_path).expect("load should succeed");
        crate::emacs_core::load::apply_runtime_startup_state(&mut loaded)
            .expect("runtime startup after load should succeed");

        let loaded_result = loaded
            .eval_expr(&probe[0])
            .expect("loaded bootstrap probe should run");
        assert_eq!(
            crate::emacs_core::print_value_with_buffers(&loaded_result, &loaded.buffers),
            "(t t t t t t 62 60 95 39)"
        );
    }

    #[test]
    fn test_pdump_round_trip_preserves_pre_runtime_standard_syntax_identity() {
        crate::test_utils::init_test_tracing();
        let eval = crate::emacs_core::load::create_bootstrap_evaluator()
            .expect("bootstrap should succeed");

        let dir = tempfile::tempdir().unwrap();
        let dump_path = dir.path().join("bootstrap-pre-runtime-syntax.pdump");
        dump_to_file(&eval, &dump_path).expect("dump should succeed");
        drop(eval);

        let mut loaded = load_from_dump(&dump_path).expect("load should succeed");
        crate::emacs_core::load::apply_runtime_startup_state(&mut loaded)
            .expect("runtime startup after load should succeed");

        let probe = crate::emacs_core::parser::parse_forms(
            r#"(list
                 (eq (char-table-parent emacs-lisp-mode-syntax-table)
                     lisp-data-mode-syntax-table)
                 (eq (char-table-parent lisp-interaction-mode-syntax-table)
                     emacs-lisp-mode-syntax-table)
                 (char-syntax ?\n)
                 (char-syntax ?\;)
                 (char-syntax ?{)
                 (char-syntax ?'))"#,
        )
        .unwrap();
        let result = loaded
            .eval_expr(&probe[0])
            .expect("loaded pre-runtime probe should run");
        assert_eq!(
            crate::emacs_core::print_value_with_buffers(&result, &loaded.buffers),
            "(t t 62 60 95 39)"
        );
    }

    #[test]
    fn test_pdump_round_trip_preserves_default_fontset_han_order() {
        crate::test_utils::init_test_tracing();
        let mut eval =
            crate::emacs_core::load::create_bootstrap_evaluator_with_features(&["neomacs"])
                .expect("bootstrap should succeed");
        let setup = crate::emacs_core::parser::parse_forms(
            r#"(new-fontset
                "fontset-default"
                '((han
                   (nil . "GB2312.1980-0")
                   (nil . "JISX0208*")
                   (nil . "gb18030"))))"#,
        )
        .unwrap();
        eval.eval_expr(&setup[0])
            .expect("han-only fontset should install before dump");

        let dir = tempfile::tempdir().unwrap();
        let dump_path = dir.path().join("bootstrap-charsets.pdump");
        dump_to_file(&eval, &dump_path).expect("dump should succeed");
        drop(eval);

        let mut loaded = load_from_dump(&dump_path).expect("load should succeed");
        let probe = crate::emacs_core::parser::parse_forms(
            r#"(list
                (fontset-font t ?好 t)
                (fontset-font t (string-to-char "好") t))"#,
        )
        .unwrap();
        let result = loaded
            .eval_expr(&probe[0])
            .expect("pdump fontset probe should run");
        let rendered = crate::emacs_core::print_value_with_buffers(&result, &loaded.buffers);

        assert!(
            rendered.starts_with(
                "(((nil . \"gb2312.1980-0\") \
                  (nil . \"jisx0208*\") \
                  (nil . \"gb18030\")) \
                 ((nil . \"gb2312.1980-0\") \
                  (nil . \"jisx0208*\") \
                  (nil . \"gb18030\")))"
            ),
            "unexpected pdump fontset order: {rendered}"
        );
    }

    #[test]
    fn test_restore_snapshot_isolated_between_clones() {
        crate::test_utils::init_test_tracing();
        let template = crate::emacs_core::load::create_bootstrap_evaluator_cached()
            .expect("bootstrap template should succeed");
        let snapshot = snapshot_evaluator(&template);

        let setup = crate::emacs_core::parser::parse_forms(
            "(progn
               (setq compat-pdump-clone-smoke 'first)
               compat-pdump-clone-smoke)",
        )
        .unwrap();
        let probe =
            crate::emacs_core::parser::parse_forms("(boundp 'compat-pdump-clone-smoke)").unwrap();

        let mut first = restore_snapshot(&snapshot).expect("first clone should succeed");
        let first_result = first
            .eval_expr(&setup[0])
            .expect("first clone evaluation should succeed");
        assert_eq!(
            crate::emacs_core::print_value_with_buffers(&first_result, &first.buffers),
            "first"
        );

        let mut second = restore_snapshot(&snapshot).expect("second clone should succeed");
        let second_result = second
            .eval_expr(&probe[0])
            .expect("second clone evaluation should succeed");
        assert_eq!(
            crate::emacs_core::print_value_with_buffers(&second_result, &second.buffers),
            "nil"
        );
    }

    #[test]
    fn test_restore_snapshot_preserves_core_subr_callable_surface() {
        crate::test_utils::init_test_tracing();
        let template = Context::new();
        let snapshot = snapshot_evaluator(&template);

        let mut restored = restore_snapshot(&snapshot).expect("restored snapshot should succeed");
        let forms = crate::emacs_core::parser::parse_forms(
            r#"(list (funcall 'cons 1 2)
                     (funcall 'list 1 2 3)
                     (funcall 'intern "compat-pdump-subr-probe")
                     (funcall 'format "%s-%s" "pdump" "ok"))"#,
        )
        .expect("parse");
        let result = restored
            .eval_expr(&forms[0])
            .expect("restored runtime subrs should be callable");
        assert_eq!(
            crate::emacs_core::print_value_with_buffers(&result, &restored.buffers),
            "((1 . 2) (1 2 3) compat-pdump-subr-probe \"pdump-ok\")"
        );
    }

    #[test]
    fn test_pdump_checksum_mismatch() {
        crate::test_utils::init_test_tracing();
        let dir = tempfile::tempdir().unwrap();
        let dump_path = dir.path().join("test.pdump");

        let eval = Context::new();
        dump_to_file(&eval, &dump_path).expect("dump should succeed");

        // Corrupt a byte in the payload
        let mut data = std::fs::read(&dump_path).unwrap();
        if let Some(last) = data.last_mut() {
            *last ^= 0xFF;
        }
        std::fs::write(&dump_path, &data).unwrap();

        let result = load_from_dump(&dump_path);
        // Should fail with checksum mismatch or deserialization error
        assert!(result.is_err());
    }

    #[test]
    fn test_restore_snapshot_rejects_legacy_unwind_protect_dump_opcode() {
        crate::test_utils::init_test_tracing();
        let mut snapshot = snapshot_evaluator(&Context::new());
        snapshot
            .heap
            .objects
            .push(DumpHeapObject::ByteCode(DumpByteCodeFunction {
                ops: vec![DumpOp::UnwindProtect(7), DumpOp::Nil, DumpOp::Return],
                constants: vec![],
                max_stack: 1,
                params: DumpLambdaParams {
                    required: vec![],
                    optional: vec![],
                    rest: None,
                },
                lexical: false,
                env: None,
                gnu_byte_offset_map: None,
                docstring: None,
                doc_form: None,
                interactive: None,
            }));
        snapshot.heap.generations.push(0);

        let result = restore_snapshot(&snapshot);
        match result {
            Err(DumpError::DeserializationError(message)) => {
                assert!(
                    message.contains(
                        "legacy neomacs unwind-protect opcode is unsupported in pdump snapshots"
                    ),
                    "unexpected error: {message}"
                );
            }
            Ok(_) => panic!("expected deserialization error, got successful restore"),
            Err(err) => panic!("expected deserialization error, got {err}"),
        }
    }
}
