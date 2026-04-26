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
pub(crate) mod mmap_image;
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
// Phase 21 bump: phase 16 introduced an explicit dump-local symbol table,
// phase 17 fixed the on-disk `DumpSymbolData` layout, phase 18 stores subr
// names as dump-local name atoms instead of dump-local symbol slots,
// phase 19 stores `loads_in_progress` as Lisp strings instead of UTF-8 paths,
// phase 20 was the Phase H SymbolValue deletion (in-memory only, no wire-format
// change needed at that time), and phase 21 redesigns `DumpSymbolData` to
// drop the legacy `name`/`value`/`symbol_value`/`special`/`constant` fields
// and encode redirect + flags + value cell directly.  Old dumps are discarded
// and regenerated (no backward compatibility required per project memory S105).
// v22: LispSymbol::plist flipped to Value cons list (DumpSymbolData::plist is now DumpValue).
// v23: LispSymbol::function flipped to Value with NIL sentinel (DumpSymbolData::function is now DumpValue).
// v25: see commit history (slot/local_var_alist round-trip).
// v26: marker GNU-parity refactor (T10). DumpMarker now carries `bytepos`/`charpos`
//   directly (the legacy `position: Option<i64>` field is gone) and `DumpBuffer.markers`
//   is `Vec<DumpMarker>` walked in head→tail chain order. The retired
//   `DumpMarkerEntry` shape (per-buffer flat tuple) is no longer accepted; old v25
//   dumps fail with `UnsupportedVersion` and are regenerated from scratch
//   per the project's "no backward compat for pdump" policy.
// v27: GNU low-tag parity: fixnums use tags 010/110, cons is 011,
//   vectorlike is 101, float is 111. `Qunbound` is a noncanonical symbol
//   value, and subrs are PVEC_SUBR-like vectorlike objects instead of
//   Neomacs-only immediate tag 111 values.
const FORMAT_VERSION: u32 = 27;

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
    ImageFormatError(String),
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
            DumpError::ImageFormatError(s) => write!(f, "pdump image format error: {s}"),
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
    let mut decoder = LoadDecoder::new(&state.tagged_heap);
    decoder.preload_tagged_heap()?;

    // 3. Reset thread-local runtime caches before replaying semantic state.
    reset_runtime_for_new_heap(HeapResetMode::PdumpRestore);

    // 4b. Restore thread-local registries whose contents are semantic runtime
    // state, not disposable caches.
    load_charset_registry(&mut decoder, &state.charset_registry);
    load_fontset_registry(&state.fontset_registry);

    // 5. Reconstruct all subsystems
    let obarray = load_obarray(&mut decoder, &state.obarray)?;
    let lexenv = decoder.load_value(&state.lexenv);
    let features: Vec<_> = state.features.iter().map(load_sym_id).collect();
    let require_stack: Vec<_> = state.require_stack.iter().map(load_sym_id).collect();
    let loads_in_progress: Vec<_> = state
        .loads_in_progress
        .iter()
        .map(load_lisp_string)
        .collect();

    let mut eval = Context::from_dump(
        tagged_heap,
        obarray,
        lexenv,
        features,
        require_stack,
        loads_in_progress,
        load_buffer_manager(&mut decoder, &state.buffers),
        load_autoload_manager(&mut decoder, &state.autoloads),
        load_custom_manager(&state.custom),
        load_mode_registry(&mut decoder, &state.modes),
        load_coding_system_manager(&mut decoder, &state.coding_systems),
        load_face_table(&mut decoder, &state.face_table),
        load_abbrev_manager(&state.abbrevs),
        load_interactive_registry(&mut decoder, &state.interactive),
        load_rectangle(&state.rectangle),
        decoder.load_value(&state.standard_syntax_table),
        decoder.load_value(&state.standard_category_table),
        decoder.load_value(&state.current_local_map),
        load_kmacro(&mut decoder, &state.kmacro),
        load_register_manager(&mut decoder, &state.registers),
        load_bookmark_manager(&state.bookmarks),
        load_watcher_list(&mut decoder, &state.watchers),
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
#[path = "pdump_test.rs"]
mod tests;
