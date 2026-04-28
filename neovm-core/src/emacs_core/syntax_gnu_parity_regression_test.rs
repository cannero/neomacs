//! Regression tests pinning behavior through the syntax-table GNU-parity
//! refactor (`docs/superpowers/plans/2026-04-17-syntax-table-gnu-parity.md`).
//!
//! These tests drive word motion, skip-syntax, parse-partial-sexp,
//! char-syntax, and modify-syntax-entry through the builtin layer and
//! direct buffer manipulation. They pass today (against the HashMap
//! compiled form) and must continue passing after the compiled form is
//! deleted and runtime reads go directly through the chartable Value.
//!
//! The existing `syntax_test.rs` suite covers the lower-level Rust API
//! (`forward_word(&buf, &table, ...)`) whose signature *will* change in
//! T2. These regression tests target stable public entry points
//! (builtins + buffer-manager helpers) that survive the refactor.

use crate::buffer::{BufferId, BufferText};
use crate::emacs_core::eval::Context;
use crate::emacs_core::value::Value;
use crate::tagged::value::ValueKind;

fn ctx_with_buffer(text: &str) -> (Context, BufferId) {
    let mut ctx = Context::new();
    let id = ctx.buffer_manager_mut().create_buffer("t");
    {
        let buf = ctx.buffer_manager_mut().get_mut(id).expect("buffer");
        buf.text = BufferText::from_str(text);
        buf.widen();
        buf.goto_byte(0);
    }
    let _ = ctx.switch_current_buffer(id);
    (ctx, id)
}

fn call(ctx: &mut Context, name: &str, args: Vec<Value>) -> Value {
    ctx.funcall_general(Value::symbol(name), args)
        .expect("funcall")
}

fn fixnum(n: i64) -> Value {
    Value::fixnum(n)
}

fn as_int(v: Value) -> i64 {
    match v.kind() {
        ValueKind::Fixnum(n) => n,
        other => panic!("expected fixnum, got {:?}", other),
    }
}

fn point(ctx: &mut Context) -> i64 {
    as_int(call(ctx, "point", vec![]))
}

fn goto(ctx: &mut Context, pos: i64) {
    call(ctx, "goto-char", vec![fixnum(pos)]);
}

// --- char-syntax --------------------------------------------------------

#[test]
fn char_syntax_ascii_word() {
    crate::test_utils::init_test_tracing();
    let (mut ctx, _) = ctx_with_buffer("");
    assert_eq!(
        as_int(call(&mut ctx, "char-syntax", vec![fixnum('a' as i64)])),
        'w' as i64
    );
    assert_eq!(
        as_int(call(&mut ctx, "char-syntax", vec![fixnum('Z' as i64)])),
        'w' as i64
    );
    assert_eq!(
        as_int(call(&mut ctx, "char-syntax", vec![fixnum('5' as i64)])),
        'w' as i64
    );
}

#[test]
fn char_syntax_ascii_whitespace() {
    crate::test_utils::init_test_tracing();
    let (mut ctx, _) = ctx_with_buffer("");
    // GNU's canonical whitespace syntax-class designator is SPACE (32).
    // Both SPACE and '-' (45) parse to SyntaxClass::Whitespace via
    // `string-to-syntax`, but `char-syntax` returns the first form.
    let space = ' ' as i64;
    assert_eq!(
        as_int(call(&mut ctx, "char-syntax", vec![fixnum(' ' as i64)])),
        space
    );
    assert_eq!(
        as_int(call(&mut ctx, "char-syntax", vec![fixnum('\t' as i64)])),
        space
    );
    assert_eq!(
        as_int(call(&mut ctx, "char-syntax", vec![fixnum('\n' as i64)])),
        space
    );
}

#[test]
fn char_syntax_cjk_is_word() {
    crate::test_utils::init_test_tracing();
    let (mut ctx, _) = ctx_with_buffer("");
    // U+4E2D 中 — GNU's standard-syntax-table ranges 0x80..=0x3FFFFF as Word.
    assert_eq!(
        as_int(call(&mut ctx, "char-syntax", vec![fixnum(0x4e2d)])),
        'w' as i64
    );
}

// --- forward-word / backward-word ---------------------------------------

#[test]
fn forward_word_crosses_whitespace() {
    crate::test_utils::init_test_tracing();
    let (mut ctx, _) = ctx_with_buffer("hello world foo");
    goto(&mut ctx, 1);
    call(&mut ctx, "forward-word", vec![fixnum(1)]);
    assert_eq!(point(&mut ctx), 6, "after 'hello', point is 6");
}

#[test]
fn forward_word_count_two_skips_second_word() {
    crate::test_utils::init_test_tracing();
    let (mut ctx, _) = ctx_with_buffer("aa bb cc");
    goto(&mut ctx, 1);
    call(&mut ctx, "forward-word", vec![fixnum(2)]);
    assert_eq!(point(&mut ctx), 6, "after 'aa bb', point is 6");
}

#[test]
fn forward_word_negative_goes_backward() {
    crate::test_utils::init_test_tracing();
    // `backward-word` is defined in lisp/simple.el; the underlying primitive
    // is `forward-word` with a negative count. Exercise that directly.
    let (mut ctx, _) = ctx_with_buffer("hello world");
    goto(&mut ctx, 12); // point-max
    call(&mut ctx, "forward-word", vec![fixnum(-1)]);
    assert_eq!(
        point(&mut ctx),
        7,
        "forward-word -1 from end lands at 'w' of 'world' = 7"
    );
}

// --- skip-syntax-forward / skip-syntax-backward ------------------------

#[test]
fn skip_syntax_forward_word_returns_count() {
    crate::test_utils::init_test_tracing();
    let (mut ctx, _) = ctx_with_buffer("abc def");
    goto(&mut ctx, 1);
    let n = as_int(call(
        &mut ctx,
        "skip-syntax-forward",
        vec![Value::string("w")],
    ));
    assert_eq!(n, 3, "skip \"w\" over 'abc' returns 3");
}

#[test]
fn skip_syntax_forward_whitespace() {
    crate::test_utils::init_test_tracing();
    let (mut ctx, _) = ctx_with_buffer("   abc");
    goto(&mut ctx, 1);
    let n = as_int(call(
        &mut ctx,
        "skip-syntax-forward",
        vec![Value::string(" ")],
    ));
    assert_eq!(n, 3);
}

// --- modify-syntax-entry changes motion --------------------------------

#[test]
fn modify_syntax_entry_adds_underscore_to_word_class() {
    crate::test_utils::init_test_tracing();
    let (mut ctx, _) = ctx_with_buffer("foo_bar baz");
    // modify-syntax-entry ?_ "w" — upgrades underscore to word class.
    call(
        &mut ctx,
        "modify-syntax-entry",
        vec![fixnum('_' as i64), Value::string("w")],
    );
    goto(&mut ctx, 1);
    call(&mut ctx, "forward-word", vec![fixnum(1)]);
    assert_eq!(
        point(&mut ctx),
        8,
        "with _ as word, forward-word consumes 'foo_bar'"
    );
}

// --- parse-partial-sexp ------------------------------------------------

#[test]
fn parse_partial_sexp_tracks_depth() {
    crate::test_utils::init_test_tracing();
    let (mut ctx, _) = ctx_with_buffer("((a b) (c");
    let state = call(&mut ctx, "parse-partial-sexp", vec![fixnum(1), fixnum(10)]);
    // "((a b) (c" — three opens ( ( (, one close ) → depth 2.
    let depth = as_int(call(&mut ctx, "car", vec![state]));
    assert_eq!(depth, 2, "three opens, one close => depth 2");
}

#[test]
fn parse_partial_sexp_string_state() {
    crate::test_utils::init_test_tracing();
    let (mut ctx, _) = ctx_with_buffer("foo \"bar");
    let state = call(&mut ctx, "parse-partial-sexp", vec![fixnum(1), fixnum(9)]);
    // state's 4th elt (index 3) is non-nil if inside a string.
    let in_string = call(&mut ctx, "nth", vec![fixnum(3), state]);
    assert!(!in_string.is_nil(), "unterminated string => elt 3 non-nil");
}

#[test]
fn parse_partial_sexp_tracks_last_complete_sexp_start() {
    crate::test_utils::init_test_tracing();
    let (mut ctx, _) =
        ctx_with_buffer("(defun sample ()\n(message \"alpha\")\n(when t\n(message \"beta\")))\n");

    let after_defun_signature = call(&mut ctx, "parse-partial-sexp", vec![fixnum(1), fixnum(18)]);
    assert_eq!(
        as_int(call(
            &mut ctx,
            "nth",
            vec![fixnum(2), after_defun_signature]
        )),
        15,
        "last complete sexp before function body should be the empty arg list"
    );

    let after_message_form = call(&mut ctx, "parse-partial-sexp", vec![fixnum(1), fixnum(36)]);
    assert_eq!(
        as_int(call(&mut ctx, "nth", vec![fixnum(2), after_message_form])),
        18,
        "last complete sexp at function body level should be the message form"
    );
}

#[test]
fn parse_partial_sexp_tracks_later_top_level_atom_after_list() {
    crate::test_utils::init_test_tracing();
    let (mut ctx, _) = ctx_with_buffer("(a) b");

    let state = call(&mut ctx, "parse-partial-sexp", vec![fixnum(1), fixnum(6)]);
    assert_eq!(
        as_int(call(&mut ctx, "nth", vec![fixnum(2), state])),
        5,
        "last complete sexp at top level should advance from the list to the later atom"
    );
}

#[test]
fn parse_partial_sexp_stopbefore_stops_at_sexp_start() {
    crate::test_utils::init_test_tracing();
    let (mut ctx, _) = ctx_with_buffer("(a) x");

    let state = call(
        &mut ctx,
        "parse-partial-sexp",
        vec![fixnum(1), fixnum(6), Value::NIL, Value::T],
    );

    assert_eq!(point(&mut ctx), 1, "STOPBEFORE leaves point at sexp start");
    assert!(
        call(&mut ctx, "nth", vec![fixnum(2), state]).is_nil(),
        "no sexp has been consumed when stopping before the opening paren"
    );
}

#[test]
fn parse_partial_sexp_targetdepth_stops_after_depth_reached() {
    crate::test_utils::init_test_tracing();
    let (mut ctx, _) = ctx_with_buffer("(a) x");

    let state = call(
        &mut ctx,
        "parse-partial-sexp",
        vec![fixnum(1), fixnum(6), fixnum(0), Value::NIL],
    );

    assert_eq!(point(&mut ctx), 4, "TARGETDEPTH stops after closing paren");
    assert_eq!(as_int(call(&mut ctx, "car", vec![state])), 0);
}

#[test]
fn parse_partial_sexp_oldstate_matches_full_scan_from_nonzero_position() {
    crate::test_utils::init_test_tracing();
    let (mut ctx, _) = ctx_with_buffer("λ (alpha \"β\") (gamma)");

    let point_max = call(&mut ctx, "point-max", vec![]);
    let full = call(&mut ctx, "parse-partial-sexp", vec![fixnum(1), point_max]);
    let first = call(&mut ctx, "parse-partial-sexp", vec![fixnum(1), fixnum(14)]);
    let segmented = call(
        &mut ctx,
        "parse-partial-sexp",
        vec![fixnum(14), point_max, Value::NIL, Value::NIL, first],
    );

    assert!(
        call(&mut ctx, "equal", vec![full, segmented]).is_truthy(),
        "OLDSTATE scan from a nonzero position should match a full scan"
    );
}

#[test]
fn parse_partial_sexp_uses_absolute_positions_under_narrowing() {
    crate::test_utils::init_test_tracing();
    let (mut ctx, _) = ctx_with_buffer("xx (α) yy");
    call(&mut ctx, "narrow-to-region", vec![fixnum(4), fixnum(7)]);

    let state = call(&mut ctx, "parse-partial-sexp", vec![fixnum(4), fixnum(7)]);

    assert_eq!(point(&mut ctx), 7, "scan should stop at absolute TO");
    assert_eq!(as_int(call(&mut ctx, "car", vec![state])), 0);
    assert_eq!(
        as_int(call(&mut ctx, "nth", vec![fixnum(2), state])),
        4,
        "last complete sexp should keep its absolute buffer position"
    );
}
