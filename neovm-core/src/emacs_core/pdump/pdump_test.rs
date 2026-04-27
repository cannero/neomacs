use super::*;
use crate::emacs_core::intern::intern;
use crate::emacs_core::mode::{FontLockDefaults, FontLockKeyword, MajorMode};
use crate::emacs_core::pdump::types::{
    DumpByteCodeFunction, DumpHeapObject, DumpLambdaParams, DumpOp, DumpSymId, DumpSymbolData,
    DumpSymbolVal, DumpValue,
};
use crate::emacs_core::value::{
    StringTextPropertyRun, Value, get_string_text_properties_for_value, list_to_vec,
    set_string_text_properties_for_value,
};
use crate::heap_types::{LispString, MarkerData, OverlayData};

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
fn file_pdump_stores_symbol_table_in_raw_mmap_section() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    eval.obarray
        .set_symbol_value("pdump-symbol-section-probe", Value::fixnum(71));

    let dir = tempfile::tempdir().unwrap();
    let dump_path = dir.path().join("symbol-table-section.pdump");
    dump_to_file(&eval, &dump_path).expect("dump should succeed");

    let image = super::mmap_image::load_image(&dump_path).expect("load raw mmap image");
    assert!(
        image
            .section(super::mmap_image::DumpSectionKind::SymbolTable)
            .is_some(),
        "file pdumps must carry the symbol interner in a raw mmap section"
    );
    let runtime_state = image
        .section(super::mmap_image::DumpSectionKind::RuntimeState)
        .expect("runtime-state section");
    let state: types::DumpContextState =
        bincode::deserialize(runtime_state).expect("runtime-state should decode");
    assert!(
        state.symbol_table.names.is_empty() && state.symbol_table.symbols.is_empty(),
        "symbol interner metadata should no longer be serialized in RuntimeState"
    );
    assert!(
        state.tagged_heap.mapped_cons.is_empty()
            && state.tagged_heap.mapped_floats.is_empty()
            && state.tagged_heap.mapped_strings.is_empty()
            && state.tagged_heap.mapped_veclikes.is_empty()
            && state.tagged_heap.mapped_slots.is_empty(),
        "mapped heap span metadata should no longer be serialized in RuntimeState"
    );

    let loaded = load_from_dump(&dump_path).expect("load should succeed");
    assert_eq!(
        loaded.obarray.symbol_value("pdump-symbol-section-probe"),
        Some(&Value::fixnum(71))
    );
}

#[test]
fn file_pdump_loads_heap_string_bytes_from_mmap_image() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    eval.obarray.set_symbol_value(
        "test-pdump-mapped-string",
        Value::string("mapped-pdump-string"),
    );

    let dir = tempfile::tempdir().unwrap();
    let dump_path = dir.path().join("mapped-string.pdump");
    dump_to_file(&eval, &dump_path).expect("dump should succeed");

    let image = super::mmap_image::load_image(&dump_path).expect("load raw mmap image");
    let heap_section = image
        .section(super::mmap_image::DumpSectionKind::HeapImage)
        .expect("heap image section");
    assert!(
        heap_section
            .windows(b"mapped-pdump-string".len())
            .any(|window| window == b"mapped-pdump-string"),
        "heap string bytes should live in the mmap heap section"
    );

    let loaded = load_from_dump(&dump_path).expect("load should succeed");
    let value = *loaded
        .obarray
        .symbol_value("test-pdump-mapped-string")
        .expect("restored string symbol");
    let string = value.as_lisp_string().expect("restored string");

    assert_eq!(string.as_bytes(), b"mapped-pdump-string");
    assert!(
        loaded.pdump_image_contains_ptr(value.as_string_ptr().unwrap().cast::<u8>()),
        "loaded string object must be a tagged pointer into the retained mmap image"
    );
    assert!(
        loaded.pdump_image_contains_ptr(string.as_bytes().as_ptr()),
        "loaded string bytes must be borrowed from the retained mmap image"
    );
}

#[test]
fn file_pdump_loads_string_text_props_from_mmap_object() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let value = Value::string("mapped-props");
    set_string_text_properties_for_value(
        value,
        vec![StringTextPropertyRun {
            start: 0,
            end: 6,
            plist: Value::list(vec![
                Value::symbol("face"),
                Value::string("mapped-string-prop-value"),
            ]),
        }],
    );
    assert!(
        get_string_text_properties_for_value(value).is_some(),
        "test setup must attach string text properties before dumping"
    );
    eval.obarray
        .set_symbol_value("test-pdump-mapped-string-props", value);

    let dir = tempfile::tempdir().unwrap();
    let dump_path = dir.path().join("mapped-string-props.pdump");
    dump_to_file(&eval, &dump_path).expect("dump should succeed");
    let image = super::mmap_image::load_image(&dump_path).expect("load raw mmap image");
    let payload = image
        .section(super::mmap_image::DumpSectionKind::RuntimeState)
        .expect("runtime state section");
    let state: super::types::DumpContextState =
        bincode::deserialize(payload).expect("decode runtime state");
    assert!(
        state.tagged_heap.objects.iter().any(|object| matches!(
            object,
            super::types::DumpHeapObject::Str { text_props, .. } if !text_props.is_empty()
        )),
        "dump should contain string text properties"
    );

    let mut loaded = load_from_dump(&dump_path).expect("load should succeed");
    let value = *loaded
        .obarray
        .symbol_value("test-pdump-mapped-string-props")
        .expect("restored string symbol");

    assert!(
        loaded.pdump_image_contains_ptr(value.as_string_ptr().unwrap().cast::<u8>()),
        "loaded string object must be a tagged pointer into the retained mmap image"
    );
    assert!(
        get_string_text_properties_for_value(value).is_some(),
        "string text properties must be restored before GC"
    );

    loaded.gc_collect_exact();
    let value_after_gc = *loaded
        .obarray
        .symbol_value("test-pdump-mapped-string-props")
        .expect("restored string symbol after GC");
    let runs = get_string_text_properties_for_value(value_after_gc).expect("text props after GC");
    let plist = list_to_vec(&runs[0].plist).expect("plist values");
    assert_eq!(
        plist[1]
            .as_lisp_string()
            .expect("text prop string value")
            .as_bytes(),
        b"mapped-string-prop-value"
    );
}

#[test]
fn file_pdump_loads_vector_slots_from_mmap_image() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    eval.obarray.set_symbol_value(
        "test-pdump-mapped-vector",
        Value::vector(vec![
            Value::fixnum(1),
            Value::fixnum(2),
            Value::string("mapped-vector-child"),
        ]),
    );

    let dir = tempfile::tempdir().unwrap();
    let dump_path = dir.path().join("mapped-vector.pdump");
    dump_to_file(&eval, &dump_path).expect("dump should succeed");

    let mut loaded = load_from_dump(&dump_path).expect("load should succeed");
    let value = *loaded
        .obarray
        .symbol_value("test-pdump-mapped-vector")
        .expect("restored vector symbol");
    let slots = value.as_vector_data().expect("restored vector");

    assert_eq!(slots.as_slice()[0], Value::fixnum(1));
    assert_eq!(slots.as_slice()[1], Value::fixnum(2));
    assert_eq!(
        slots.as_slice()[2]
            .as_lisp_string()
            .expect("vector child string")
            .as_bytes(),
        b"mapped-vector-child"
    );
    assert!(
        loaded.pdump_image_contains_ptr(value.as_veclike_ptr().unwrap().cast::<u8>()),
        "loaded vector object must be a tagged pointer into the retained mmap image"
    );
    assert!(
        loaded.pdump_image_contains_ptr(slots.as_slice().as_ptr().cast::<u8>()),
        "loaded vector slots must be borrowed from the retained mmap image"
    );

    loaded.gc_collect_exact();
    let value_after_gc = *loaded
        .obarray
        .symbol_value("test-pdump-mapped-vector")
        .expect("restored vector symbol after GC");
    assert_eq!(
        value_after_gc.as_vector_data().unwrap().as_slice()[2]
            .as_lisp_string()
            .expect("vector child string after GC")
            .as_bytes(),
        b"mapped-vector-child",
        "mapped vector GC marking must trace children from the mmap object"
    );
}

#[test]
fn file_pdump_loads_record_object_from_mmap_image() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    eval.obarray.set_symbol_value(
        "test-pdump-mapped-record",
        Value::make_record(vec![
            Value::symbol("record-type"),
            Value::string("mapped-record-child"),
        ]),
    );

    let dir = tempfile::tempdir().unwrap();
    let dump_path = dir.path().join("mapped-record.pdump");
    dump_to_file(&eval, &dump_path).expect("dump should succeed");

    let mut loaded = load_from_dump(&dump_path).expect("load should succeed");
    let value = *loaded
        .obarray
        .symbol_value("test-pdump-mapped-record")
        .expect("restored record symbol");
    let slots = value.as_record_data().expect("restored record");

    assert!(
        loaded.pdump_image_contains_ptr(value.as_veclike_ptr().unwrap().cast::<u8>()),
        "loaded record object must be a tagged pointer into the retained mmap image"
    );
    assert!(
        loaded.pdump_image_contains_ptr(slots.as_slice().as_ptr().cast::<u8>()),
        "loaded record slots must be borrowed from the retained mmap image"
    );
    assert_eq!(
        slots.as_slice()[1]
            .as_lisp_string()
            .expect("record child string")
            .as_bytes(),
        b"mapped-record-child"
    );

    loaded.gc_collect_exact();
    let value_after_gc = *loaded
        .obarray
        .symbol_value("test-pdump-mapped-record")
        .expect("restored record symbol after GC");
    assert_eq!(
        value_after_gc.as_record_data().unwrap().as_slice()[1]
            .as_lisp_string()
            .expect("record child string after GC")
            .as_bytes(),
        b"mapped-record-child",
        "mapped record GC marking must trace children from the mmap object"
    );
}

#[test]
fn file_pdump_loads_lambda_object_from_mmap_image() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let mut slots = vec![Value::NIL; crate::tagged::header::CLOSURE_MIN_SLOTS];
    slots[1] = Value::string("mapped-lambda-child");
    eval.obarray.set_symbol_value(
        "test-pdump-mapped-lambda",
        Value::make_lambda_with_slots(slots),
    );

    let dir = tempfile::tempdir().unwrap();
    let dump_path = dir.path().join("mapped-lambda.pdump");
    dump_to_file(&eval, &dump_path).expect("dump should succeed");

    let mut loaded = load_from_dump(&dump_path).expect("load should succeed");
    let value = *loaded
        .obarray
        .symbol_value("test-pdump-mapped-lambda")
        .expect("restored lambda symbol");
    let slots = value.closure_slots().expect("restored lambda slots");

    assert!(
        loaded.pdump_image_contains_ptr(value.as_veclike_ptr().unwrap().cast::<u8>()),
        "loaded lambda object must be a tagged pointer into the retained mmap image"
    );
    assert!(
        loaded.pdump_image_contains_ptr(slots.as_slice().as_ptr().cast::<u8>()),
        "loaded lambda slots must be borrowed from the retained mmap image"
    );
    assert_eq!(
        slots.as_slice()[1]
            .as_lisp_string()
            .expect("lambda child string")
            .as_bytes(),
        b"mapped-lambda-child"
    );

    loaded.gc_collect_exact();
    let value_after_gc = *loaded
        .obarray
        .symbol_value("test-pdump-mapped-lambda")
        .expect("restored lambda symbol after GC");
    assert_eq!(
        value_after_gc.closure_slots().unwrap().as_slice()[1]
            .as_lisp_string()
            .expect("lambda child string after GC")
            .as_bytes(),
        b"mapped-lambda-child",
        "mapped lambda GC marking must trace children from the mmap object"
    );
}

#[test]
fn file_pdump_loads_cons_cells_from_mmap_image() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    eval.obarray.set_symbol_value(
        "test-pdump-mapped-cons",
        Value::cons(
            Value::string("mapped-cons-car"),
            Value::vector(vec![Value::fixnum(9)]),
        ),
    );

    let dir = tempfile::tempdir().unwrap();
    let dump_path = dir.path().join("mapped-cons.pdump");
    dump_to_file(&eval, &dump_path).expect("dump should succeed");

    let mut loaded = load_from_dump(&dump_path).expect("load should succeed");
    let value = *loaded
        .obarray
        .symbol_value("test-pdump-mapped-cons")
        .expect("restored cons symbol");

    assert!(value.is_cons());
    assert!(
        loaded.pdump_image_contains_ptr(value.xcons_ptr().cast::<u8>()),
        "loaded cons cell must be a tagged pointer into the retained mmap image"
    );
    assert_eq!(
        value
            .cons_car()
            .as_lisp_string()
            .expect("cons car string")
            .as_bytes(),
        b"mapped-cons-car"
    );
    assert_eq!(
        value.cons_cdr().as_vector_data().unwrap().as_slice(),
        &[Value::fixnum(9)]
    );

    loaded.gc_collect_exact();
    let value_after_gc = *loaded
        .obarray
        .symbol_value("test-pdump-mapped-cons")
        .expect("restored cons symbol after GC");
    assert_eq!(
        value_after_gc
            .cons_car()
            .as_lisp_string()
            .expect("cons car string after GC")
            .as_bytes(),
        b"mapped-cons-car",
        "mapped cons GC marking must trace children from the mmap cell"
    );
}

#[test]
fn file_pdump_loads_float_objects_from_mmap_image() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    eval.obarray.set_symbol_value(
        "test-pdump-mapped-float",
        Value::make_float(std::f64::consts::PI),
    );

    let dir = tempfile::tempdir().unwrap();
    let dump_path = dir.path().join("mapped-float.pdump");
    dump_to_file(&eval, &dump_path).expect("dump should succeed");

    let mut loaded = load_from_dump(&dump_path).expect("load should succeed");
    let value = *loaded
        .obarray
        .symbol_value("test-pdump-mapped-float")
        .expect("restored float symbol");

    assert!(value.is_float());
    assert_eq!(value.xfloat(), std::f64::consts::PI);
    assert!(
        loaded.pdump_image_contains_ptr(value.as_float_ptr().unwrap().cast::<u8>()),
        "loaded float object must be a tagged pointer into the retained mmap image"
    );

    loaded.gc_collect_exact();
    let value_after_gc = *loaded
        .obarray
        .symbol_value("test-pdump-mapped-float")
        .expect("restored float symbol after GC");
    assert_eq!(value_after_gc.xfloat(), std::f64::consts::PI);
}

#[test]
fn file_pdump_loads_marker_object_from_mmap_image() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    eval.obarray.set_symbol_value(
        "test-pdump-mapped-marker",
        Value::make_marker(MarkerData {
            buffer: None,
            insertion_type: true,
            marker_id: Some(42),
            bytepos: 7,
            charpos: 7,
            next_marker: std::ptr::null_mut(),
        }),
    );

    let dir = tempfile::tempdir().unwrap();
    let dump_path = dir.path().join("mapped-marker.pdump");
    dump_to_file(&eval, &dump_path).expect("dump should succeed");

    let mut loaded = load_from_dump(&dump_path).expect("load should succeed");
    let value = *loaded
        .obarray
        .symbol_value("test-pdump-mapped-marker")
        .expect("restored marker symbol");
    let marker = value.as_marker_data().expect("restored marker");

    assert!(
        loaded.pdump_image_contains_ptr(value.as_veclike_ptr().unwrap().cast::<u8>()),
        "loaded marker object must be a tagged pointer into the retained mmap image"
    );
    assert!(marker.insertion_type);
    assert_eq!(marker.marker_id, Some(42));
    assert_eq!(marker.bytepos, 7);
    assert_eq!(marker.charpos, 7);

    loaded.gc_collect_exact();
    let value_after_gc = *loaded
        .obarray
        .symbol_value("test-pdump-mapped-marker")
        .expect("restored marker symbol after GC");
    assert_eq!(
        value_after_gc
            .as_marker_data()
            .expect("marker after GC")
            .marker_id,
        Some(42)
    );
}

#[test]
fn file_pdump_loads_overlay_object_from_mmap_image() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    eval.obarray.set_symbol_value(
        "test-pdump-mapped-overlay",
        Value::make_overlay(OverlayData {
            plist: Value::list(vec![
                Value::symbol("face"),
                Value::string("mapped-overlay-child"),
            ]),
            buffer: None,
            start: 2,
            end: 9,
            front_advance: true,
            rear_advance: false,
        }),
    );

    let dir = tempfile::tempdir().unwrap();
    let dump_path = dir.path().join("mapped-overlay.pdump");
    dump_to_file(&eval, &dump_path).expect("dump should succeed");

    let mut loaded = load_from_dump(&dump_path).expect("load should succeed");
    let value = *loaded
        .obarray
        .symbol_value("test-pdump-mapped-overlay")
        .expect("restored overlay symbol");
    let overlay = value.as_overlay_data().expect("restored overlay");
    let plist = list_to_vec(&overlay.plist).expect("overlay plist");

    assert!(
        loaded.pdump_image_contains_ptr(value.as_veclike_ptr().unwrap().cast::<u8>()),
        "loaded overlay object must be a tagged pointer into the retained mmap image"
    );
    assert_eq!(overlay.start, 2);
    assert_eq!(overlay.end, 9);
    assert!(overlay.front_advance);
    assert!(!overlay.rear_advance);
    assert_eq!(
        plist[1]
            .as_lisp_string()
            .expect("overlay child string")
            .as_bytes(),
        b"mapped-overlay-child"
    );

    loaded.gc_collect_exact();
    let value_after_gc = *loaded
        .obarray
        .symbol_value("test-pdump-mapped-overlay")
        .expect("restored overlay symbol after GC");
    let overlay_after_gc = value_after_gc.as_overlay_data().expect("overlay after GC");
    let plist_after_gc = list_to_vec(&overlay_after_gc.plist).expect("overlay plist after GC");
    assert_eq!(
        plist_after_gc[1]
            .as_lisp_string()
            .expect("overlay child string after GC")
            .as_bytes(),
        b"mapped-overlay-child",
        "mapped overlay GC marking must trace plist children from the mmap object"
    );
}

#[test]
fn pdump_dumps_default_value_for_active_dynamic_plain_binding() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();

    let sym = intern("pdump-dynamic-plain-var");
    eval.obarray
        .set_symbol_value_id(sym, Value::symbol("default-value"));
    eval.specbind(sym, Value::symbol("dynamic-value"));

    let dir = tempfile::tempdir().unwrap();
    let dump_path = dir.path().join("dynamic-plain.pdump");
    dump_to_file(&eval, &dump_path).expect("dump should succeed");

    let loaded = load_from_dump(&dump_path).expect("load should succeed");
    assert_eq!(
        loaded.obarray.symbol_value_id(sym),
        Some(&Value::symbol("default-value")),
        "pdump must serialize the top-level value, not the active dynamic binding"
    );
}

#[test]
fn test_dump_symbol_data_bincode_round_trip() {
    crate::test_utils::init_test_tracing();

    // Format v21: no legacy name/value/symbol_value/special/constant fields.
    // plist is now a DumpValue (Lisp cons list) rather than Vec<(DumpSymId, DumpValue)>.
    let original = DumpSymbolData {
        redirect: 1, // Varalias
        trapped_write: 0,
        interned: 1,
        declared_special: true,
        val: DumpSymbolVal::Alias(DumpSymId(7)),
        function: DumpValue::Int(9),
        plist: DumpValue::Nil,
    };

    let encoded = bincode::serialize(&original).expect("symbol data should serialize");
    let decoded: DumpSymbolData =
        bincode::deserialize(&encoded).expect("symbol data should deserialize");

    assert_eq!(decoded.redirect, 1, "redirect should round-trip");
    assert_eq!(decoded.trapped_write, 0, "trapped_write should round-trip");
    assert_eq!(decoded.interned, 1, "interned should round-trip");
    assert!(
        decoded.declared_special,
        "declared_special should round-trip"
    );
    assert!(
        matches!(decoded.val, DumpSymbolVal::Alias(DumpSymId(7))),
        "val should round-trip as Alias(7)"
    );
    assert!(matches!(decoded.function, DumpValue::Int(9)));
    assert!(
        matches!(decoded.plist, DumpValue::Nil),
        "empty plist should round-trip as Nil"
    );
}

#[test]
fn test_clone_active_evaluator_preserves_in_progress_require_and_load_state() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    eval.require_stack.push(intern("cl-macs"));
    eval.loads_in_progress
        .push(crate::heap_types::LispString::from_utf8(
            "/tmp/neomacs-pdump-clone-in-progress.el",
        ));

    let cloned = clone_active_evaluator(&mut eval).expect("clone should succeed");

    assert_eq!(cloned.require_stack, vec![intern("cl-macs")]);
    assert_eq!(
        cloned.loads_in_progress,
        vec![crate::heap_types::LispString::from_utf8(
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
    let charset_name = crate::emacs_core::intern::intern("charset-pdump-clone-restore-test");
    let doc_key = crate::emacs_core::intern::intern("doc");
    let entry = registry
        .charsets
        .iter()
        .find(|info| info.name == charset_name)
        .expect("restored charset entry");
    assert_eq!(
        entry.plist,
        vec![(
            doc_key,
            value::Value::string("live charset registry should survive clone handoff"),
        )]
    );
}

#[test]
fn test_dump_buffers_use_symbol_ids_for_buffer_local_bindings() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let current = eval.buffers.current_buffer_id().expect("current buffer");
    eval.buffers
        .get_mut(current)
        .expect("current buffer mut")
        .set_buffer_local("fill-column", Value::fixnum(80));

    let dump = crate::emacs_core::pdump::convert::dump_evaluator(&eval);
    let dumped = dump
        .buffers
        .buffers
        .iter()
        .find(|(id, _)| id.0 == current.0)
        .map(|(_, buffer)| buffer)
        .expect("dumped current buffer");

    assert!(
        dumped
            .properties_syms
            .iter()
            .any(|(sym_id, _)| sym_id.0 == intern("fill-column").0)
    );
    assert!(
        dumped
            .local_binding_syms
            .iter()
            .any(|sym_id| sym_id.0 == intern("fill-column").0)
    );
    assert!(
        dumped.properties.is_empty(),
        "fresh dumps should not flatten buffer-local names to strings"
    );
    assert!(
        dumped.local_binding_names.is_empty(),
        "fresh dumps should not record buffer-local ordering via legacy string names"
    );
}

#[test]
fn test_dump_modes_use_symbol_ids_for_font_lock_faces() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    eval.modes.register_major_mode(
        "compat-font-lock-mode",
        MajorMode {
            pretty_name: LispString::from_utf8("Compat Font Lock"),
            parent: None,
            mode_hook: Value::symbol("compat-font-lock-mode-hook"),
            keymap_name: None,
            syntax_table_name: None,
            abbrev_table_name: None,
            font_lock: Some(FontLockDefaults {
                keywords: vec![FontLockKeyword {
                    pattern: LispString::from_utf8("\\_<compat\\_>"),
                    face: intern("font-lock-keyword-face"),
                    group: 0,
                    override_: false,
                    laxmatch: false,
                }],
                case_fold: false,
                syntax_table: None,
            }),
            body: None,
        },
    );

    let dump = crate::emacs_core::pdump::convert::dump_evaluator(&eval);
    let dumped = dump
        .modes
        .major_modes
        .iter()
        .find(|(sym_id, _)| sym_id.0 == intern("compat-font-lock-mode").0)
        .map(|(_, mode)| mode)
        .expect("dumped compat-font-lock-mode");
    let keyword = dumped
        .font_lock
        .as_ref()
        .and_then(|font_lock| font_lock.keywords.first())
        .expect("dumped font-lock keyword");

    assert_eq!(
        keyword.face_sym,
        Some(DumpSymId(intern("font-lock-keyword-face").0))
    );
    assert!(
        keyword.face.is_none(),
        "fresh dumps should not flatten font-lock faces to strings"
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
    let fingerprint_start = 16 + 4 + 4 + 4 + 4;
    bytes[fingerprint_start] ^= 0x01;
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
    let eval =
        crate::emacs_core::load::create_bootstrap_evaluator().expect("bootstrap should succeed");

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
    let forms =
        crate::emacs_core::value_reader::read_all("(concat \"hello\" \" \" \"world\")").unwrap();
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
    let mut eval =
        crate::emacs_core::load::create_bootstrap_evaluator().expect("bootstrap should succeed");
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
    let eval =
        crate::emacs_core::load::create_bootstrap_evaluator().expect("bootstrap should succeed");

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
    let mut eval = crate::emacs_core::load::create_bootstrap_evaluator_with_features(&["neomacs"])
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
    let probe =
        crate::emacs_core::value_reader::read_all("(boundp 'compat-pdump-clone-smoke)").unwrap();
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
    template.obarray.set_symbol_value(
        "compat-pdump-raw-uninterned-holder",
        Value::from_sym_id(uninterned),
    );
    template.obarray.set_symbol_value(
        "compat-pdump-raw-canonical-holder",
        Value::from_sym_id(canonical),
    );
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
fn test_pdump_rejects_corrupt_runtime_state_section() {
    crate::test_utils::init_test_tracing();
    let dir = tempfile::tempdir().unwrap();
    let dump_path = dir.path().join("test.pdump");

    super::mmap_image::write_image(
        &dump_path,
        &[super::mmap_image::ImageSection {
            kind: super::mmap_image::DumpSectionKind::RuntimeState,
            flags: 0,
            bytes: b"not a bincode DumpContextState",
        }],
    )
    .expect("write corrupt runtime-state image");

    let result = load_from_dump(&dump_path);
    assert!(matches!(result, Err(DumpError::DeserializationError(_))));
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
            arglist: None,
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

fn measure_timings<T>(iterations: usize, mut op: impl FnMut() -> T) -> Vec<std::time::Duration> {
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

fn ensure_workspace_bootstrap_pdump_current(bootstrap_path: &std::path::Path) {
    if load_from_dump(bootstrap_path).is_ok() {
        return;
    }

    crate::emacs_core::load::create_bootstrap_evaluator_cached_at_path(&[], bootstrap_path)
        .unwrap_or_else(|err| {
            panic!(
                "refresh workspace bootstrap pdump {}: {err}",
                bootstrap_path.display()
            )
        });
}

fn ensure_workspace_final_pdump_current(
    final_path: &std::path::Path,
    bootstrap_path: &std::path::Path,
) {
    if load_from_dump(final_path).is_ok() {
        return;
    }

    let eval =
        crate::emacs_core::load::create_runtime_startup_evaluator_at_path(&[], bootstrap_path)
            .unwrap_or_else(|err| {
                panic!(
                    "refresh workspace final pdump {} from bootstrap {}: {err}",
                    final_path.display(),
                    bootstrap_path.display()
                )
            });
    dump_to_file(&eval, final_path).unwrap_or_else(|err| {
        panic!(
            "rewrite workspace final pdump {}: {err}",
            final_path.display()
        )
    });
}

#[test]
fn test_measure_current_workspace_final_pdump_performance() {
    crate::test_utils::init_test_tracing();
    let (final_path, bootstrap_path) = workspace_pdump_paths();
    ensure_workspace_bootstrap_pdump_current(&bootstrap_path);
    ensure_workspace_final_pdump_current(&final_path, &bootstrap_path);
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
    ensure_workspace_bootstrap_pdump_current(&bootstrap_path);
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

    let image = mmap_image::load_image(&dump_path).expect("load mmap pdump image");
    let payload = image
        .section(DumpSectionKind::RuntimeState)
        .expect("runtime-state section should exist");

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
    let _loads_in_progress = decode_field!("loads_in_progress", Vec<types::DumpLispString>);
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
