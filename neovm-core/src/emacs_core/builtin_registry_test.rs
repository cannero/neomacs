use super::is_dispatch_builtin_name;

#[test]
fn registry_contains_common_builtins() {
    assert!(is_dispatch_builtin_name("message"));
    assert!(is_dispatch_builtin_name("load"));
    assert!(is_dispatch_builtin_name("symbol-value"));
    assert!(is_dispatch_builtin_name("+"));
    assert!(is_dispatch_builtin_name("if"));
    assert!(is_dispatch_builtin_name("let"));
    assert!(is_dispatch_builtin_name("setq"));
    assert!(is_dispatch_builtin_name("unwind-protect"));
    // read-key is now Elisp (from subr.el)
    assert!(is_dispatch_builtin_name("read-char-exclusive"));
    assert!(is_dispatch_builtin_name("input-pending-p"));
    assert!(is_dispatch_builtin_name("discard-input"));
    assert!(is_dispatch_builtin_name("current-input-mode"));
    assert!(is_dispatch_builtin_name("set-input-mode"));
    assert!(is_dispatch_builtin_name("set-input-interrupt-mode"));
    assert!(is_dispatch_builtin_name("set-input-meta-mode"));
    assert!(is_dispatch_builtin_name("set-output-flow-control"));
    assert!(is_dispatch_builtin_name("set-quit-char"));
    assert!(is_dispatch_builtin_name("waiting-for-user-input-p"));
    // read-passwd is now Elisp (from auth-source.el/subr.el)
    assert!(is_dispatch_builtin_name("minibuffer-prompt"));
    assert!(is_dispatch_builtin_name("minibuffer-contents"));
    assert!(is_dispatch_builtin_name(
        "minibuffer-contents-no-properties"
    ));
    assert!(is_dispatch_builtin_name("sleep-for"));
    assert!(is_dispatch_builtin_name("redraw-frame"));
    assert!(is_dispatch_builtin_name("last-nonminibuffer-frame"));
    // exit-minibuffer is now Elisp (from minibuffer.el)
    assert!(is_dispatch_builtin_name("recursive-edit"));
    assert!(is_dispatch_builtin_name("exit-recursive-edit"));
    assert!(is_dispatch_builtin_name("top-level"));
}

#[test]
fn registry_excludes_unknown_names() {
    assert!(!is_dispatch_builtin_name("definitely-not-a-builtin"));
}

#[test]
fn registry_contains_arithmetic_ops() {
    for name in ["+", "-", "*", "/", "%", "1+", "1-"] {
        assert!(
            is_dispatch_builtin_name(name),
            "missing arithmetic op: {name}"
        );
    }
}

#[test]
fn registry_contains_predicates() {
    for name in [
        "numberp",
        "stringp",
        "symbolp",
        "consp",
        "listp",
        "null",
        "integerp",
        "floatp",
        "vectorp",
        "keywordp",
        "characterp",
    ] {
        assert!(is_dispatch_builtin_name(name), "missing predicate: {name}");
    }
}

#[test]
fn registry_contains_list_ops() {
    for name in [
        "cons", "car", "cdr", "nth", "length", "append", "mapcar", "reverse", "nreverse", "member",
        "memq", "assoc", "assq",
    ] {
        assert!(is_dispatch_builtin_name(name), "missing list op: {name}");
    }
}
