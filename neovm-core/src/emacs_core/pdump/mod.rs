//! Portable dumper (pdump) for NeoVM.
//!
//! Serializes the post-bootstrap `Context` state to a binary file using
//! serde + bincode, then deserializes on startup to skip the 3-5s bootstrap.
//!
//! File format:
//! ```text
//! [8 bytes: magic "NEOPDUMP"]
//! [4 bytes: format version u32 LE]
//! [32 bytes: build fingerprint]
//! [32 bytes: SHA-256 of bincode payload]
//! [4 bytes: payload length u32 LE]
//! [N bytes: bincode-serialized DumpContextState]
//! ```

pub mod convert;
pub mod runtime;
pub mod types;

use std::io::{Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

use self::convert::*;
use self::runtime::*;
use self::types::DumpContextState;
use crate::emacs_core::charset::{
    CharsetRegistrySnapshot, restore_charset_registry, snapshot_charset_registry,
};
use crate::emacs_core::eval::Context;
use crate::emacs_core::fontset::{
    FontsetRegistrySnapshot, restore_fontset_registry, snapshot_fontset_registry,
};
use crate::emacs_core::value;

const MAGIC: &[u8; 8] = b"NEOPDUMP";
const AFTER_PDUMP_LOAD_HOOK_PENDING_SYMBOL: &str = "neovm--after-pdump-load-hook-pending";
// Phase 18 bump (18): phase 16 introduced an explicit dump-local symbol table,
// phase 17 fixed the on-disk `DumpSymbolData` layout, and phase 18 stores subr
// names as dump-local name atoms instead of dump-local symbol slots.
const FORMAT_VERSION: u32 = 19;

pub fn fingerprint_hex() -> &'static str {
    env!("NEOVM_PDUMP_FINGERPRINT")
}

fn fingerprint_bytes() -> [u8; 32] {
    let hex = fingerprint_hex().as_bytes();
    assert_eq!(
        hex.len(),
        64,
        "NEOVM_PDUMP_FINGERPRINT must be 64 hex characters"
    );

    let mut out = [0u8; 32];
    for (idx, chunk) in hex.chunks_exact(2).enumerate() {
        out[idx] = (decode_hex_nibble(chunk[0]) << 4) | decode_hex_nibble(chunk[1]);
    }
    out
}

fn decode_hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => panic!(
            "invalid NEOVM_PDUMP_FINGERPRINT hex digit: {}",
            byte as char
        ),
    }
}

fn hex_string(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02X}");
    }
    out
}

/// Errors from dump/load operations.
#[derive(Debug)]
pub enum DumpError {
    Io(std::io::Error),
    BadMagic,
    UnsupportedVersion(u32),
    FingerprintMismatch { expected: String, found: String },
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
            DumpError::FingerprintMismatch { expected, found } => write!(
                f,
                "pdump fingerprint mismatch (expected {expected}, found {found})"
            ),
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

/// Thread-local semantic runtime state that must be restored when switching
/// back from a cloned evaluator to the live evaluator on the same thread.
#[derive(Clone, Debug)]
pub struct ActiveRuntimeSnapshot {
    charset_registry: CharsetRegistrySnapshot,
    fontset_registry: FontsetRegistrySnapshot,
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
    file.write_all(&fingerprint_bytes())?;
    file.write_all(&checksum)?;
    file.write_all(&(payload.len() as u32).to_le_bytes())?;
    file.write_all(&payload)?;
    file.flush()?;
    file.as_file().sync_all()?;

    file.persist(path).map_err(|err| DumpError::Io(err.error))?;
    Ok(())
}

/// Load evaluator state from a pdump file.
///
/// This reconstructs a full `Context` from the serialized state,
/// setting up thread-local pointers and resetting caches.
pub fn load_from_dump(path: &Path) -> Result<Context, DumpError> {
    let load_start = std::time::Instant::now();
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

    let mut found_fingerprint = [0u8; 32];
    file.read_exact(&mut found_fingerprint)?;
    let expected_fingerprint = fingerprint_bytes();
    if found_fingerprint != expected_fingerprint {
        return Err(DumpError::FingerprintMismatch {
            expected: hex_string(&expected_fingerprint),
            found: hex_string(&found_fingerprint),
        });
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
    let mut eval = reconstruct_evaluator(&state)?;
    record_loaded_dump(path, load_start.elapsed());
    mark_after_pdump_load_hook_pending(&mut eval);
    Ok(eval)
}

/// Clone a live evaluator through the pdump conversion pipeline.
///
/// This gives bootstrap/load code an isolated working evaluator with the same
/// logical runtime state, without sharing heap objects that can be mutated
/// during eager macroexpansion.
pub fn snapshot_evaluator(eval: &Context) -> DumpContextState {
    dump_evaluator(eval)
}

/// Snapshot an evaluator after activating its thread-local runtime bindings.
///
/// Use this entry point when multiple `Context`s may share the current thread.
/// The pdump conversion pipeline relies on thread-local tagged-heap state, so
/// the source evaluator must be active before we walk its heap-backed values.
pub fn snapshot_active_evaluator(eval: &mut Context) -> DumpContextState {
    eval.setup_thread_locals();
    dump_evaluator(eval)
}

/// Snapshot thread-local semantic runtime registries for the active evaluator.
///
/// Cloning an evaluator through pdump reconstructs these registries for the
/// cloned heap. Callers that later switch the current thread back to the live
/// evaluator must restore this snapshot as part of runtime reactivation.
pub fn snapshot_active_runtime(eval: &mut Context) -> ActiveRuntimeSnapshot {
    eval.setup_thread_locals();
    ActiveRuntimeSnapshot {
        charset_registry: snapshot_charset_registry(),
        fontset_registry: snapshot_fontset_registry(),
    }
}

/// Reactivate a live evaluator after using a cloned evaluator on the same
/// thread, restoring thread-local semantic registries alongside heap state.
pub fn restore_active_runtime(eval: &mut Context, snapshot: &ActiveRuntimeSnapshot) {
    eval.setup_thread_locals();
    restore_charset_registry(snapshot.charset_registry.clone());
    restore_fontset_registry(snapshot.fontset_registry.clone());
    eval.sync_thread_runtime_bindings();
    eval.sync_current_thread_buffer_state();
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

/// Clone an evaluator after activating its thread-local runtime bindings.
///
/// Use this when cloning from a live runtime that shares the current thread
/// with other `Context`s.
pub fn clone_active_evaluator(eval: &mut Context) -> Result<Context, DumpError> {
    restore_snapshot(&snapshot_active_evaluator(eval))
}

/// Reconstruct an `Context` from deserialized dump state.
fn reconstruct_evaluator(state: &DumpContextState) -> Result<Context, DumpError> {
    struct RestoreCleanup;

    impl Drop for RestoreCleanup {
        fn drop(&mut self) {
            finish_preload_tagged_heap();
            finish_load_interner();
        }
    }

    // 1. Reconstruct the dump-local symbol table before any values that refer
    // to dump-local `DumpSymId`s are loaded.
    load_symbol_table(&state.symbol_table)?;
    let _cleanup = RestoreCleanup;

    // 2. Reconstruct the tagged heap before any heap-backed value/object loads
    // so tagged dump references can resolve directly to live tagged objects.
    let mut tagged_heap = Box::new(crate::tagged::gc::TaggedHeap::new());
    crate::tagged::gc::set_tagged_heap(&mut tagged_heap);
    preload_tagged_heap(&state.tagged_heap)?;

    // 3. Reset thread-local runtime caches before replaying semantic state.
    reset_runtime_for_new_heap(HeapResetMode::PdumpRestore);

    // 4b. Restore thread-local registries whose contents are semantic runtime
    // state, not disposable caches.
    load_charset_registry(&state.charset_registry);
    load_fontset_registry(&state.fontset_registry);

    // 5. Reconstruct all subsystems
    let obarray = load_obarray(&state.obarray)?;
    let lexenv = load_value(&state.lexenv);
    let features: Vec<_> = state.features.iter().map(load_sym_id).collect();
    let require_stack: Vec<_> = state.require_stack.iter().map(load_sym_id).collect();
    let loads_in_progress: Vec<_> = state
        .loads_in_progress
        .iter()
        .map(std::path::PathBuf::from)
        .collect();

    let mut eval = Context::from_dump(
        tagged_heap,
        obarray,
        lexenv,
        features,
        require_stack,
        loads_in_progress,
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

    // Phase 10E follow-up: re-install BUFFER_OBJFWD forwarders.
    // `pdump::convert::load_symbol_data` leaves SymbolValue::Forwarded
    // entries at redirect=Plainval (the descriptor pointer is a
    // 'static reference and is rebuilt from BUFFER_SLOT_INFO at
    // install time, so the dump never carried it). Without this
    // loop, every per-buffer C-slot variable behaves like a plain
    // global after a snapshot restore, breaking writes routed via
    // `Buffer::set_buffer_local`. Mirrors the matching loop in
    // `Context::new_inner` and `finalize_cached_bootstrap_eval`.
    {
        use crate::buffer::buffer::BUFFER_SLOT_INFO;
        use crate::emacs_core::forward::alloc_buffer_objfwd;
        use crate::emacs_core::intern::intern;
        let obarray = eval.obarray_mut();
        for info in BUFFER_SLOT_INFO {
            if !info.install_as_forwarder {
                continue;
            }
            let id = intern(info.name);
            let predicate = if info.predicate.is_empty() {
                intern("null")
            } else {
                intern(info.predicate)
            };
            let fwd = alloc_buffer_objfwd(
                info.offset as u16,
                info.local_flags_idx,
                predicate,
                info.default.to_value(),
            );
            obarray.install_buffer_objfwd(id, fwd);
        }
    }

    Ok(eval)
}

fn mark_after_pdump_load_hook_pending(eval: &mut Context) {
    eval.obarray_mut()
        .set_symbol_value(AFTER_PDUMP_LOAD_HOOK_PENDING_SYMBOL, value::Value::T);
}

pub(crate) fn take_after_pdump_load_hook_pending(eval: &mut Context) -> bool {
    let pending = eval
        .obarray()
        .symbol_value(AFTER_PDUMP_LOAD_HOOK_PENDING_SYMBOL)
        .is_some_and(|value| value.is_truthy());
    eval.obarray_mut()
        .set_symbol_value(AFTER_PDUMP_LOAD_HOOK_PENDING_SYMBOL, value::Value::NIL);
    pending
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emacs_core::intern::intern;
    use crate::emacs_core::pdump::types::{
        DumpByteCodeFunction, DumpHeapObject, DumpLambdaParams, DumpOp, DumpSymId, DumpSymbolData,
        DumpSymbolValue, DumpValue,
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
    fn test_dump_symbol_data_bincode_round_trip_with_legacy_name_omitted() {
        crate::test_utils::init_test_tracing();

        let original = DumpSymbolData {
            name: None,
            value: None,
            symbol_value: Some(DumpSymbolValue::Alias(DumpSymId(7))),
            function: Some(DumpValue::Int(9)),
            plist: vec![(DumpSymId(3), DumpValue::Int(11))],
            special: true,
            constant: false,
        };

        let encoded = bincode::serialize(&original).expect("symbol data should serialize");
        let decoded: DumpSymbolData =
            bincode::deserialize(&encoded).expect("symbol data should deserialize");

        assert!(decoded.name.is_none(), "legacy name field should stay omitted");
        assert!(matches!(
            decoded.symbol_value,
            Some(DumpSymbolValue::Alias(DumpSymId(7)))
        ));
        assert!(matches!(decoded.function, Some(DumpValue::Int(9))));
        assert_eq!(decoded.plist.len(), 1);
        assert_eq!(decoded.plist[0].0, DumpSymId(3));
        assert!(matches!(decoded.plist[0].1, DumpValue::Int(11)));
        assert!(decoded.special);
        assert!(!decoded.constant);
    }

    #[test]
    fn test_clone_active_evaluator_preserves_in_progress_require_and_load_state() {
        crate::test_utils::init_test_tracing();
        let mut eval = Context::new();
        eval.require_stack.push(intern("cl-macs"));
        eval.loads_in_progress.push(std::path::PathBuf::from(
            "/tmp/neomacs-pdump-clone-in-progress.el",
        ));

        let cloned = clone_active_evaluator(&mut eval).expect("clone should succeed");

        assert_eq!(cloned.require_stack, vec![intern("cl-macs")]);
        assert_eq!(
            cloned.loads_in_progress,
            vec![std::path::PathBuf::from(
                "/tmp/neomacs-pdump-clone-in-progress.el"
            )]
        );
    }

    #[test]
    fn test_restore_active_runtime_after_clone_reinstalls_live_charset_registry() {
        crate::test_utils::init_test_tracing();
        crate::emacs_core::charset::reset_charset_registry();

        let mut eval = Context::new();
        let mut args = vec![value::Value::NIL; 17];
        args[0] = value::Value::symbol("charset-pdump-clone-restore-test");
        args[1] = value::Value::fixnum(1);
        args[2] = value::Value::vector(vec![value::Value::fixnum(0), value::Value::fixnum(127)]);
        args[16] = value::Value::list(vec![
            value::Value::symbol("doc"),
            value::Value::string("live charset registry should survive clone handoff"),
        ]);
        crate::emacs_core::charset::builtin_define_charset_internal(args).unwrap();

        let live_runtime = snapshot_active_runtime(&mut eval);
        let cloned = clone_active_evaluator(&mut eval).expect("first clone should succeed");
        restore_active_runtime(&mut eval, &live_runtime);
        drop(cloned);

        let cloned_again = clone_active_evaluator(&mut eval).expect("second clone should succeed");
        restore_active_runtime(&mut eval, &live_runtime);
        drop(cloned_again);

        let registry = crate::emacs_core::charset::snapshot_charset_registry();
        let entry = registry
            .charsets
            .iter()
            .find(|info| info.name == "charset-pdump-clone-restore-test")
            .expect("restored charset entry");
        assert_eq!(
            entry.plist,
            vec![(
                "doc".to_string(),
                value::Value::string("live charset registry should survive clone handoff"),
            )]
        );
    }

    #[test]
    fn test_file_load_records_pdumper_stats_without_running_after_pdump_load_hook() {
        crate::test_utils::init_test_tracing();
        let mut eval = Context::new();
        let setup = crate::emacs_core::value_reader::read_all(
            "(progn
               (setq compat-pdump-hook-fired nil)
               (setq after-pdump-load-hook
                     (list (lambda () (setq compat-pdump-hook-fired t)))))",
        )
        .unwrap();
        eval.eval_sub(setup[0]).expect("setup hook should evaluate");

        let dir = tempfile::tempdir().unwrap();
        let dump_path = dir.path().join("stats-and-hook.pdump");
        dump_to_file(&eval, &dump_path).expect("dump should succeed");
        drop(eval);

        let mut loaded = load_from_dump(&dump_path).expect("load should succeed");
        assert_eq!(
            loaded.obarray.symbol_value("compat-pdump-hook-fired"),
            Some(&Value::NIL)
        );

        let forms = crate::emacs_core::value_reader::read_all("(pdumper-stats)").unwrap();
        let stats = loaded
            .eval_sub(forms[0])
            .expect("pdumper-stats should evaluate");
        assert!(stats.is_cons(), "pdumper-stats should return an alist");

        let dumped_with = stats.cons_car();
        assert_eq!(dumped_with.cons_car(), Value::symbol("dumped-with-pdumper"));
        assert_eq!(dumped_with.cons_cdr(), Value::T);

        let load_time = stats.cons_cdr().cons_car();
        assert_eq!(load_time.cons_car(), Value::symbol("load-time"));
        assert!(load_time.cons_cdr().is_float());

        let dump_file = stats.cons_cdr().cons_cdr().cons_car();
        assert_eq!(dump_file.cons_car(), Value::symbol("dump-file-name"));
        let expected = dump_path
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            dump_file.cons_cdr().as_str_owned().as_deref(),
            Some(expected.as_str())
        );
    }

    #[test]
    fn test_pdump_rejects_fingerprint_mismatch() {
        crate::test_utils::init_test_tracing();
        let eval = Context::new();
        let dir = tempfile::tempdir().unwrap();
        let dump_path = dir.path().join("fingerprint-mismatch.pdump");
        dump_to_file(&eval, &dump_path).expect("dump should succeed");

        let mut bytes = std::fs::read(&dump_path).expect("read dump bytes");
        bytes[12] ^= 0x01;
        std::fs::write(&dump_path, bytes).expect("rewrite dump bytes");

        match load_from_dump(&dump_path) {
            Err(DumpError::FingerprintMismatch { expected, found }) => {
                assert_eq!(expected, fingerprint_hex());
                assert_ne!(expected, found);
            }
            Ok(_) => panic!("expected fingerprint mismatch, but load succeeded"),
            Err(other) => panic!("expected fingerprint mismatch, got {other}"),
        }
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
        let forms = crate::emacs_core::value_reader::read_all("(+ 1 2)").unwrap();
        let result = loaded.eval_sub(forms[0]).expect("eval should succeed");
        assert_eq!(result, Value::fixnum(3));

        // Verify features survived (bootstrap sets many features)
        // Note: subr.el does NOT call (provide 'subr); use 'backquote instead
        let forms = crate::emacs_core::value_reader::read_all("(featurep 'backquote)").unwrap();
        let result = loaded.eval_sub(forms[0]).expect("featurep should succeed");
        assert_eq!(result, Value::T, "featurep 'backquote should be t");

        // Verify a bootstrapped function works
        let forms = crate::emacs_core::value_reader::read_all("(length '(a b c))").unwrap();
        let result = loaded.eval_sub(forms[0]).expect("eval should succeed");
        assert_eq!(result, Value::fixnum(3));

        // Verify string operations (tests heap String objects)
        let forms = crate::emacs_core::value_reader::read_all("(concat \"hello\" \" \" \"world\")")
            .unwrap();
        let result = loaded.eval_sub(forms[0]).expect("eval should succeed");
        assert_eq!(crate::emacs_core::print_value(&result), "\"hello world\"");

        // Verify hash table access (tests hash table round-trip)
        let forms = crate::emacs_core::value_reader::read_all(
            "(let ((h (make-hash-table :test 'equal))) (puthash \"key\" 42 h) (gethash \"key\" h))",
        )
        .unwrap();
        let result = loaded.eval_sub(forms[0]).expect("eval should succeed");
        assert_eq!(result, Value::fixnum(42));

        // Verify defun works (tests lambda/macro round-trip)
        let forms = crate::emacs_core::value_reader::read_all(
            "(progn (defun pdump-test-fn (x) (* x x)) (pdump-test-fn 7))",
        )
        .unwrap();
        let result = loaded.eval_sub(forms[0]).expect("eval should succeed");
        assert_eq!(result, Value::fixnum(49));
    }

    #[test]
    fn test_pdump_round_trip_preserves_runtime_derived_mode_syntax() {
        crate::test_utils::init_test_tracing();
        let mut eval = crate::emacs_core::load::create_bootstrap_evaluator()
            .expect("bootstrap should succeed");
        crate::emacs_core::load::apply_runtime_startup_state(&mut eval)
            .expect("runtime startup should succeed");

        let probe_src = r#"(list
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
                 (char-syntax ?'))"#;
        let probe = crate::emacs_core::value_reader::read_all(probe_src).unwrap();
        let full_result = eval
            .eval_sub(probe[0])
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

        let probe = crate::emacs_core::value_reader::read_all(probe_src).unwrap();
        let loaded_result = loaded
            .eval_sub(probe[0])
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

        let probe = crate::emacs_core::value_reader::read_all(
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
            .eval_sub(probe[0])
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
        let setup = crate::emacs_core::value_reader::read_all(
            r#"(new-fontset
                "fontset-default"
                '((han
                   (nil . "GB2312.1980-0")
                   (nil . "JISX0208*")
                   (nil . "gb18030"))))"#,
        )
        .unwrap();
        eval.eval_sub(setup[0])
            .expect("han-only fontset should install before dump");

        let dir = tempfile::tempdir().unwrap();
        let dump_path = dir.path().join("bootstrap-charsets.pdump");
        dump_to_file(&eval, &dump_path).expect("dump should succeed");
        drop(eval);

        let mut loaded = load_from_dump(&dump_path).expect("load should succeed");
        let probe = crate::emacs_core::value_reader::read_all(
            r#"(list
                (fontset-font t ?好 t)
                (fontset-font t (string-to-char "好") t))"#,
        )
        .unwrap();
        let result = loaded
            .eval_sub(probe[0])
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

        let mut first = restore_snapshot(&snapshot).expect("first clone should succeed");
        let setup = crate::emacs_core::value_reader::read_all(
            "(progn
               (setq compat-pdump-clone-smoke 'first)
               compat-pdump-clone-smoke)",
        )
        .unwrap();
        let first_result = first
            .eval_sub(setup[0])
            .expect("first clone evaluation should succeed");
        assert_eq!(
            crate::emacs_core::print_value_with_buffers(&first_result, &first.buffers),
            "first"
        );

        let mut second = restore_snapshot(&snapshot).expect("second clone should succeed");
        let probe = crate::emacs_core::value_reader::read_all("(boundp 'compat-pdump-clone-smoke)")
            .unwrap();
        let second_result = second
            .eval_sub(probe[0])
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
        let forms = crate::emacs_core::value_reader::read_all(
            r#"(list (funcall 'cons 1 2)
                     (funcall 'list 1 2 3)
                     (funcall 'intern "compat-pdump-subr-probe")
                     (funcall 'format "%s-%s" "pdump" "ok"))"#,
        )
        .expect("parse");
        let result = restored
            .eval_sub(forms[0])
            .expect("restored runtime subrs should be callable");
        assert_eq!(
            crate::emacs_core::print_value_with_buffers(&result, &restored.buffers),
            "((1 . 2) (1 2 3) compat-pdump-subr-probe \"pdump-ok\")"
        );
    }

    #[test]
    fn test_restore_snapshot_preserves_lone_uninterned_symbol_identity() {
        crate::test_utils::init_test_tracing();
        let mut template = Context::new();
        let solo = crate::emacs_core::intern::intern_uninterned("compat-pdump-solo-uninterned");
        template
            .obarray
            .set_symbol_value("compat-pdump-uninterned-holder", Value::from_sym_id(solo));
        let snapshot = snapshot_evaluator(&template);

        let restored = restore_snapshot(&snapshot).expect("restored snapshot should succeed");
        let held = *restored
            .obarray
            .symbol_value("compat-pdump-uninterned-holder")
            .expect("holder binding should exist");
        let held_id = held.as_symbol_id().expect("holder should contain a symbol");
        assert_eq!(
            crate::emacs_core::intern::resolve_sym(held_id),
            "compat-pdump-solo-uninterned"
        );
        assert!(
            !crate::emacs_core::intern::is_canonical_id(held_id),
            "round-tripped lone uninterned symbol should stay uninterned"
        );
    }

    #[test]
    fn test_restore_snapshot_preserves_raw_unibyte_symbol_name_storage() {
        crate::test_utils::init_test_tracing();
        let mut template = Context::new();
        let raw_name = crate::heap_types::LispString::from_unibyte(vec![0xFF, b'a']);
        let uninterned = crate::emacs_core::intern::intern_uninterned_lisp_string(&raw_name);
        let canonical = crate::emacs_core::intern::intern_lisp_string(&raw_name);
        template
            .obarray
            .set_symbol_value("compat-pdump-raw-uninterned-holder", Value::from_sym_id(uninterned));
        template
            .obarray
            .set_symbol_value("compat-pdump-raw-canonical-holder", Value::from_sym_id(canonical));
        template.obarray.ensure_interned_global_id(canonical);
        let snapshot = snapshot_evaluator(&template);

        let restored = restore_snapshot(&snapshot).expect("restored snapshot should succeed");

        for (holder, should_be_canonical) in [
            ("compat-pdump-raw-uninterned-holder", false),
            ("compat-pdump-raw-canonical-holder", true),
        ] {
            let held = *restored
                .obarray
                .symbol_value(holder)
                .expect("holder binding should exist");
            let held_id = held.as_symbol_id().expect("holder should contain a symbol");
            let restored_name = crate::emacs_core::intern::resolve_sym_lisp_string(held_id);
            assert_eq!(restored_name.as_bytes(), &[0xFF, b'a']);
            assert!(!restored_name.is_multibyte());
            assert_eq!(
                crate::emacs_core::intern::is_canonical_id(held_id),
                should_be_canonical
            );
        }
    }

    #[test]
    fn test_restore_snapshot_preserves_subr_name_identity_via_name_atoms() {
        crate::test_utils::init_test_tracing();
        let mut template = Context::new();
        let subr = Value::subr(intern("car"));
        template
            .obarray
            .set_symbol_value("compat-pdump-subr-holder", subr);
        let snapshot = snapshot_evaluator(&template);

        let restored = restore_snapshot(&snapshot).expect("restored snapshot should succeed");
        let held = *restored
            .obarray
            .symbol_value("compat-pdump-subr-holder")
            .expect("holder binding should exist");

        assert!(held.is_subr(), "holder should round-trip a subr object");
        assert_eq!(held.as_subr_id(), Some(intern("car")));
    }

    #[test]
    fn test_restore_snapshot_does_not_report_file_based_pdump_session() {
        crate::test_utils::init_test_tracing();
        let mut template = Context::new();
        let setup = crate::emacs_core::value_reader::read_all(
            "(progn
               (setq compat-pdump-snapshot-hook-fired nil)
               (setq after-pdump-load-hook
                     (list (lambda () (setq compat-pdump-snapshot-hook-fired t)))))",
        )
        .unwrap();
        template
            .eval_sub(setup[0])
            .expect("setup hook should evaluate");
        let snapshot = snapshot_evaluator(&template);

        let mut restored = restore_snapshot(&snapshot).expect("restored snapshot should succeed");
        assert_eq!(
            restored
                .obarray
                .symbol_value("compat-pdump-snapshot-hook-fired"),
            Some(&Value::NIL)
        );

        let forms = crate::emacs_core::value_reader::read_all("(pdumper-stats)").unwrap();
        let stats = restored
            .eval_sub(forms[0])
            .expect("pdumper-stats should evaluate");
        assert!(stats.is_nil());
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
            .tagged_heap
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

    #[test]
    fn test_restore_snapshot_rejects_duplicate_obarray_symbol_slots() {
        crate::test_utils::init_test_tracing();
        let mut snapshot = snapshot_evaluator(&Context::new());
        let duplicate = snapshot
            .obarray
            .symbols
            .first()
            .cloned()
            .expect("snapshot should contain at least one symbol");
        snapshot.obarray.symbols.push(duplicate);

        let result = restore_snapshot(&snapshot);
        match result {
            Err(DumpError::DeserializationError(message)) => {
                assert!(
                    message.contains("duplicate symbol slot"),
                    "unexpected error: {message}"
                );
            }
            Ok(_) => panic!("expected deserialization error, got successful restore"),
            Err(err) => panic!("expected deserialization error, got {err}"),
        }
    }

    #[test]
    fn test_restore_snapshot_rejects_global_member_without_symbol_entry() {
        crate::test_utils::init_test_tracing();
        let template = Context::new();
        let dangling = crate::emacs_core::intern::intern_uninterned("compat-pdump-missing-global");
        let mut snapshot = snapshot_evaluator(&template);
        snapshot.obarray.global_members.push(DumpSymId(dangling.0));

        let result = restore_snapshot(&snapshot);
        match result {
            Err(DumpError::DeserializationError(message)) => {
                assert!(
                    message.contains("global_members entry references missing symbol slot"),
                    "unexpected error: {message}"
                );
            }
            Ok(_) => panic!("expected deserialization error, got successful restore"),
            Err(err) => panic!("expected deserialization error, got {err}"),
        }
    }

    fn summarize_timings(label: &str, samples: &[std::time::Duration]) {
        let mut millis: Vec<f64> = samples.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
        millis.sort_by(|a, b| a.partial_cmp(b).expect("timing values should compare"));
        let count = millis.len();
        let mean = millis.iter().sum::<f64>() / count as f64;
        let min = millis[0];
        let max = millis[count - 1];
        let median = millis[count / 2];
        eprintln!(
            "pdump bench: {label}: mean={mean:.1}ms median={median:.1}ms min={min:.1}ms max={max:.1}ms n={count}"
        );
    }

    fn measure_timings<T>(
        iterations: usize,
        mut op: impl FnMut() -> T,
    ) -> Vec<std::time::Duration> {
        let mut samples = Vec::with_capacity(iterations);
        for _ in 0..iterations {
            let start = std::time::Instant::now();
            let _ = op();
            samples.push(start.elapsed());
        }
        samples
    }

    fn workspace_pdump_paths() -> (std::path::PathBuf, std::path::PathBuf) {
        let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .to_path_buf();
        let final_path = workspace_root.join("target/debug/neomacs.pdump");
        let bootstrap_path = workspace_root.join("target/debug/bootstrap-neomacs.pdump");
        assert!(
            final_path.exists(),
            "missing final image at {}; run a fresh build first",
            final_path.display()
        );
        assert!(
            bootstrap_path.exists(),
            "missing bootstrap image at {}; run a fresh build first",
            bootstrap_path.display()
        );
        (final_path, bootstrap_path)
    }

    #[test]
    fn test_measure_current_workspace_final_pdump_performance() {
        crate::test_utils::init_test_tracing();
        let (final_path, bootstrap_path) = workspace_pdump_paths();
        let final_size = std::fs::metadata(&final_path)
            .expect("stat final pdump")
            .len();
        let bootstrap_size = std::fs::metadata(&bootstrap_path)
            .expect("stat bootstrap pdump")
            .len();
        eprintln!(
            "pdump bench: final image size={} bytes ({:.1} MiB)",
            final_size,
            final_size as f64 / 1048576.0
        );
        eprintln!(
            "pdump bench: bootstrap image size={} bytes ({:.1} MiB)",
            bootstrap_size,
            bootstrap_size as f64 / 1048576.0
        );

        let iterations = 5;
        let final_raw_load = measure_timings(iterations, || {
            load_from_dump(&final_path).expect("raw final load should succeed")
        });
        summarize_timings("raw final load_from_dump", &final_raw_load);

        let finalized_runtime_load = measure_timings(iterations, || {
            crate::emacs_core::load::load_runtime_image_with_features(
                crate::emacs_core::load::RuntimeImageRole::Final,
                &[],
                Some(&final_path),
            )
            .expect("final runtime image load should succeed")
        });
        summarize_timings("final load+finalize", &finalized_runtime_load);

        let loaded_final = load_from_dump(&final_path).expect("prepare final eval for dump bench");
        let dump_dir = tempfile::tempdir().expect("dump tempdir");
        let mut dump_sizes = Vec::with_capacity(iterations);
        let dump_samples = measure_timings(iterations, || {
            let output = dump_dir
                .path()
                .join(format!("bench-{}.pdump", dump_sizes.len()));
            dump_to_file(&loaded_final, &output).expect("dump should succeed");
            dump_sizes.push(std::fs::metadata(&output).expect("stat dumped image").len());
        });
        summarize_timings("dump_to_file from loaded final image", &dump_samples);
        if let Some(last_size) = dump_sizes.last() {
            eprintln!(
                "pdump bench: dumped bench image size={} bytes ({:.1} MiB)",
                last_size,
                *last_size as f64 / 1048576.0
            );
        }
    }

    #[test]
    fn test_measure_current_workspace_bootstrap_pdump_raw_load() {
        crate::test_utils::init_test_tracing();
        let (_final_path, bootstrap_path) = workspace_pdump_paths();
        let bootstrap_size = std::fs::metadata(&bootstrap_path)
            .expect("stat bootstrap pdump")
            .len();
        eprintln!(
            "pdump bench: bootstrap image size={} bytes ({:.1} MiB)",
            bootstrap_size,
            bootstrap_size as f64 / 1048576.0
        );

        let bootstrap_raw_load = measure_timings(5, || {
            load_from_dump(&bootstrap_path).expect("raw bootstrap load should succeed")
        });
        summarize_timings("raw bootstrap load_from_dump", &bootstrap_raw_load);
    }

    #[test]
    fn test_pdump_sequential_decode_round_trip() {
        crate::test_utils::init_test_tracing();
        let mut eval = Context::new();
        eval.obarray
            .set_symbol_value("pdump-sequential-decode-probe", Value::fixnum(17));

        let dir = tempfile::tempdir().expect("tempdir");
        let dump_path = dir.path().join("sequential-decode.pdump");
        dump_to_file(&eval, &dump_path).expect("dump should succeed");

        let bytes = std::fs::read(&dump_path).expect("read test pdump");
        assert!(bytes.len() > 80, "pdump header should exist");
        let payload_len = u32::from_le_bytes(bytes[76..80].try_into().unwrap()) as usize;
        let payload = &bytes[80..80 + payload_len];

        let mut cursor = std::io::Cursor::new(payload);

        macro_rules! decode_field {
            ($label:literal, $ty:ty) => {{
                let start = cursor.position();
                let value: $ty = bincode::deserialize_from(&mut cursor).unwrap_or_else(|err| {
                    panic!(
                        "failed decoding {} at payload offset {}: {}",
                        $label, start, err
                    )
                });
                eprintln!(
                    "pdump decode: {} ok ({} -> {})",
                    $label,
                    start,
                    cursor.position()
                );
                value
            }};
        }

        let _symbol_table = decode_field!("symbol_table", types::DumpSymbolTable);
        let _tagged_heap = decode_field!("tagged_heap", types::DumpTaggedHeap);
        let _obarray = decode_field!("obarray", types::DumpObarray);
        let _dynamic = decode_field!("dynamic", Vec<types::DumpOrderedSymMap>);
        let _lexenv = decode_field!("lexenv", types::DumpValue);
        let _features = decode_field!("features", Vec<types::DumpSymId>);
        let _require_stack = decode_field!("require_stack", Vec<types::DumpSymId>);
        let _loads_in_progress = decode_field!("loads_in_progress", Vec<String>);
        let _buffers = decode_field!("buffers", types::DumpBufferManager);
        let _autoloads = decode_field!("autoloads", types::DumpAutoloadManager);
        let _custom = decode_field!("custom", types::DumpCustomManager);
        let _modes = decode_field!("modes", types::DumpModeRegistry);
        let _coding_systems = decode_field!("coding_systems", types::DumpCodingSystemManager);
        let _charset_registry = decode_field!("charset_registry", types::DumpCharsetRegistry);
        let _fontset_registry = decode_field!("fontset_registry", types::DumpFontsetRegistry);
        let _face_table = decode_field!("face_table", types::DumpFaceTable);
        let _abbrevs = decode_field!("abbrevs", types::DumpAbbrevManager);
        let _interactive = decode_field!("interactive", types::DumpInteractiveRegistry);
        let _rectangle = decode_field!("rectangle", types::DumpRectangleState);
        let _standard_syntax_table = decode_field!("standard_syntax_table", types::DumpValue);
        let _standard_category_table = decode_field!("standard_category_table", types::DumpValue);
        let _current_local_map = decode_field!("current_local_map", types::DumpValue);
        let _kmacro = decode_field!("kmacro", types::DumpKmacroManager);
        let _registers = decode_field!("registers", types::DumpRegisterManager);
        let _bookmarks = decode_field!("bookmarks", types::DumpBookmarkManager);
        let _watchers = decode_field!("watchers", types::DumpVariableWatcherList);

        assert_eq!(
            cursor.position() as usize,
            payload.len(),
            "sequential decode should consume the whole payload"
        );
    }
}
