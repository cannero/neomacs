#[test]
fn migrated_string_subsystems_do_not_call_generic_runtime_string_adapter_directly() {
    let forbidden = concat!("lisp_string", "_to_runtime_string(");
    for (path, source) in [
        ("display.rs", include_str!("display.rs")),
        ("font.rs", include_str!("font.rs")),
        ("fontset.rs", include_str!("fontset.rs")),
        ("load.rs", include_str!("load.rs")),
        ("lread.rs", include_str!("lread.rs")),
        ("minibuffer.rs", include_str!("minibuffer.rs")),
        ("reader.rs", include_str!("reader.rs")),
        ("value_reader.rs", include_str!("value_reader.rs")),
    ] {
        assert!(
            !source.contains(forbidden),
            "{path} should use subsystem-local string helpers instead of the generic runtime-string adapter"
        );
    }
}
