#[test]
fn migrated_string_subsystems_do_not_call_generic_runtime_string_adapter_directly() {
    let forbidden = concat!("lisp_string", "_to_runtime_string(");
    for (path, source) in [
        ("abbrev.rs", include_str!("abbrev.rs")),
        ("autoload.rs", include_str!("autoload.rs")),
        ("callproc/mod.rs", include_str!("callproc/mod.rs")),
        ("charset.rs", include_str!("charset.rs")),
        ("coding.rs", include_str!("coding.rs")),
        ("dired.rs", include_str!("dired.rs")),
        ("display.rs", include_str!("display.rs")),
        ("editfns.rs", include_str!("editfns.rs")),
        ("errors.rs", include_str!("errors.rs")),
        ("fileio.rs", include_str!("fileio.rs")),
        ("filelock.rs", include_str!("filelock.rs")),
        ("fns.rs", include_str!("fns.rs")),
        ("font.rs", include_str!("font.rs")),
        ("fontset.rs", include_str!("fontset.rs")),
        ("format.rs", include_str!("format.rs")),
        ("interactive.rs", include_str!("interactive.rs")),
        ("isearch.rs", include_str!("isearch.rs")),
        ("load.rs", include_str!("load.rs")),
        ("lread.rs", include_str!("lread.rs")),
        ("marker.rs", include_str!("marker.rs")),
        ("minibuffer.rs", include_str!("minibuffer.rs")),
        ("misc.rs", include_str!("misc.rs")),
        ("process.rs", include_str!("process.rs")),
        ("reader.rs", include_str!("reader.rs")),
        ("syntax.rs", include_str!("syntax.rs")),
        ("textprop.rs", include_str!("textprop.rs")),
        ("timer.rs", include_str!("timer.rs")),
        ("value_reader.rs", include_str!("value_reader.rs")),
        ("window_cmds/mod.rs", include_str!("window_cmds/mod.rs")),
        ("xdisp.rs", include_str!("xdisp.rs")),
    ] {
        assert!(
            !source.contains(forbidden),
            "{path} should use subsystem-local string helpers instead of the generic runtime-string adapter"
        );
    }
}

#[test]
fn semantic_string_subsystems_do_not_reintroduce_utf8_unwraps() {
    let forbidden = concat!("as_str", "().unwrap(");
    for (path, source) in [
        ("builtins/symbols.rs", include_str!("builtins/symbols.rs")),
        ("cl_lib.rs", include_str!("cl_lib.rs")),
        ("search.rs", include_str!("search.rs")),
    ] {
        assert!(
            !source.contains(forbidden),
            "{path} should use LispString/runtime helpers instead of UTF-8 unwraps"
        );
    }
}

#[test]
fn live_treesit_paths_do_not_use_buffer_string_adapter() {
    let forbidden = concat!("buffer_", "string(");
    for (path, source) in [
        ("builtins/treesit.rs", include_str!("builtins/treesit.rs")),
        ("editfns.rs", include_str!("editfns.rs")),
    ] {
        assert!(
            !source.contains(forbidden),
            "{path} should use explicit buffer source helpers instead of buffer_string()"
        );
    }
}
