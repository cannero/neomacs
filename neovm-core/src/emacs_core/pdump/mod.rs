//! Portable dumper (pdump) for NeoVM.
//!
//! Serializes the post-bootstrap `Evaluator` state to a binary file using
//! serde + bincode, then deserializes on startup to skip the 3-5s bootstrap.
//!
//! File format:
//! ```text
//! [8 bytes: magic "NEOPDUMP"]
//! [4 bytes: format version u32 LE]
//! [32 bytes: SHA-256 of bincode payload]
//! [4 bytes: payload length u32 LE]
//! [N bytes: bincode-serialized DumpEvaluatorState]
//! ```

pub mod convert;
pub mod types;

use std::io::{Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

use self::convert::*;
use self::types::DumpEvaluatorState;
use crate::emacs_core::eval::Evaluator;
use crate::emacs_core::value::{self, set_current_heap};
use crate::emacs_core::intern::{self, set_current_interner};

const MAGIC: &[u8; 8] = b"NEOPDUMP";
const FORMAT_VERSION: u32 = 1;

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
pub fn dump_to_file(eval: &Evaluator, path: &Path) -> Result<(), DumpError> {
    let state = dump_evaluator(eval);

    let payload = bincode::serialize(&state)
        .map_err(|e| DumpError::SerializationError(e.to_string()))?;

    let mut hasher = Sha256::new();
    hasher.update(&payload);
    let checksum = hasher.finalize();

    let mut file = std::fs::File::create(path)?;
    file.write_all(MAGIC)?;
    file.write_all(&FORMAT_VERSION.to_le_bytes())?;
    file.write_all(&checksum)?;
    file.write_all(&(payload.len() as u32).to_le_bytes())?;
    file.write_all(&payload)?;
    file.flush()?;

    Ok(())
}

/// Load evaluator state from a pdump file.
///
/// This reconstructs a full `Evaluator` from the serialized state,
/// setting up thread-local pointers and resetting caches.
pub fn load_from_dump(path: &Path) -> Result<Evaluator, DumpError> {
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
    let state: types::DumpEvaluatorState = bincode::deserialize(&payload)
        .map_err(|e| DumpError::DeserializationError(e.to_string()))?;

    // Reconstruct evaluator
    reconstruct_evaluator(state)
}

/// Reconstruct an `Evaluator` from deserialized dump state.
fn reconstruct_evaluator(state: DumpEvaluatorState) -> Result<Evaluator, DumpError> {
    // 1. Reconstruct interner and set thread-local
    let mut interner = Box::new(load_interner(&state.interner));
    set_current_interner(&mut interner);

    // 2. Reconstruct heap and set thread-local
    let mut heap = Box::new(load_heap(&state.heap));
    set_current_heap(&mut heap);

    // 3. Reset thread-local caches (same as Evaluator::new())
    super::syntax::reset_syntax_thread_locals();
    super::casetab::reset_casetab_thread_locals();
    super::category::reset_category_thread_locals();
    value::reset_string_text_properties();
    super::ccl::reset_ccl_registry();
    super::dispnew::pure::reset_dispnew_thread_locals();
    super::font::clear_font_cache_state();
    super::builtins::reset_builtins_thread_locals();
    super::charset::reset_charset_registry();
    super::timefns::reset_timefns_thread_locals();

    // 4. Restore string text properties
    let stp: Vec<(u64, Vec<value::StringTextPropertyRun>)> = state
        .string_text_props
        .iter()
        .map(|(key, runs)| (*key, runs.iter().map(load_string_text_prop_run).collect::<Vec<_>>()))
        .collect();
    value::restore_string_text_props(stp);

    // 5. Reconstruct all subsystems
    let obarray = load_obarray(&state.obarray);
    let dynamic: Vec<_> = state
        .dynamic
        .iter()
        .map(|m| {
            crate::emacs_core::value::OrderedSymMap::from_entries(
                m.entries.iter().map(|(k, v)| (load_sym_id(k), load_value(v))).collect(),
            )
        })
        .collect();
    let lexenv = load_value(&state.lexenv);
    let features: Vec<_> = state.features.iter().map(|id| intern::SymId(*id)).collect();
    let require_stack: Vec<_> = state.require_stack.iter().map(|id| intern::SymId(*id)).collect();

    let eval = Evaluator::from_dump(
        interner,
        heap,
        obarray,
        dynamic,
        lexenv,
        features,
        require_stack,
        load_buffer_manager(&state.buffers),
        load_autoload_manager(&state.autoloads),
        load_custom_manager(&state.custom),
        load_mode_registry(&state.modes),
        load_coding_system_manager(&state.coding_systems),
        load_face_table(&state.face_table),
        load_category_manager(&state.category_manager),
        load_abbrev_manager(&state.abbrevs),
        load_interactive_registry(&state.interactive),
        load_kill_ring(&state.kill_ring),
        load_rectangle(&state.rectangle),
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
    use crate::emacs_core::value::Value;

    #[test]
    fn test_pdump_round_trip_basic() {
        // Create a minimal evaluator
        let mut eval = Evaluator::new();

        // Set a symbol value to verify round-trip
        eval.obarray.set_symbol_value("test-pdump-var", Value::Int(42));

        // Dump to temp file
        let dir = tempfile::tempdir().unwrap();
        let dump_path = dir.path().join("test.pdump");
        dump_to_file(&eval, &dump_path).expect("dump should succeed");

        // Load from dump
        let loaded = load_from_dump(&dump_path).expect("load should succeed");

        // Verify the symbol value survived
        assert_eq!(
            loaded.obarray.symbol_value("test-pdump-var"),
            Some(&Value::Int(42))
        );
    }

    #[test]
    fn test_pdump_bad_magic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.pdump");
        std::fs::write(&path, b"BADMAGIC").unwrap();
        assert!(matches!(load_from_dump(&path), Err(DumpError::BadMagic)));
    }

    #[test]
    fn test_pdump_checksum_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let dump_path = dir.path().join("test.pdump");

        let eval = Evaluator::new();
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
}
