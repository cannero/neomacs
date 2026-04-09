//! Unit tests for the `args` module — the GNU `argmatch` port.
//!
//! Each test here cites the GNU `emacs.c` line it reproduces, so the
//! parity intent stays explicit.

use super::*;

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn exact_short_no_value() {
    // emacs.c:1696 — argmatch(argv, argc, "-nw", "--no-window-system", 6, NULL, &skip_args)
    let mut idx = 0;
    let argv = args(&["neomacs", "-nw"]);
    assert_eq!(
        argmatch(&argv, &mut idx, "-nw", Some("--no-window-system"), 6, false),
        ArgMatch::Bare,
    );
    assert_eq!(idx, 1);
}

#[test]
fn exact_long_no_value() {
    let mut idx = 0;
    let argv = args(&["neomacs", "--no-window-system"]);
    assert_eq!(
        argmatch(&argv, &mut idx, "-nw", Some("--no-window-system"), 6, false),
        ArgMatch::Bare,
    );
    assert_eq!(idx, 1);
}

#[test]
fn long_prefix_meets_minlen() {
    // GNU's `argmatch` uses strncmp(arg, lstr, arglen) — i.e. the user's
    // argument must be a prefix of the long form. So `--no-w` (6 chars)
    // matches `--no-window-system` because the first 6 characters of
    // both are identical and 6 >= minlen.
    let mut idx = 0;
    let argv = args(&["neomacs", "--no-w"]);
    assert_eq!(
        argmatch(&argv, &mut idx, "-nw", Some("--no-window-system"), 6, false),
        ArgMatch::Bare,
    );
    assert_eq!(idx, 1);
}

#[test]
fn long_form_distinct_aliases_need_separate_calls() {
    // GNU emacs.c:1697 declares "--no-windows" via a separate argmatch
    // call: the strings "--no-windows" and "--no-window-system" diverge
    // at character 11 ('s' vs '-'), so a single argmatch with lstr =
    // "--no-window-system" cannot match a user typing "--no-windows".
    // GNU works around this with TWO consecutive argmatch calls in
    // emacs.c:1696-1697; we test both halves of that pair.
    let mut idx = 0;
    let argv = args(&["neomacs", "--no-windows"]);
    // Call 1: lstr = "--no-window-system" — must NOT match.
    assert_eq!(
        argmatch(&argv, &mut idx, "-nw", Some("--no-window-system"), 6, false),
        ArgMatch::NoMatch,
    );
    assert_eq!(idx, 0);
    // Call 2: lstr = "--no-windows" — matches.
    assert_eq!(
        argmatch(&argv, &mut idx, "-nw", Some("--no-windows"), 6, false),
        ArgMatch::Bare,
    );
    assert_eq!(idx, 1);
}

#[test]
fn long_prefix_below_minlen_does_not_match() {
    // "--no" is 4 chars, below the 6-char minlen for --no-window-system.
    let mut idx = 0;
    let argv = args(&["neomacs", "--no"]);
    assert_eq!(
        argmatch(&argv, &mut idx, "-nw", Some("--no-window-system"), 6, false),
        ArgMatch::NoMatch,
    );
    assert_eq!(idx, 0);
}

#[test]
fn long_with_value_inline_eq() {
    // --temacs=pdump form
    let mut idx = 0;
    let argv = args(&["neomacs", "--temacs=pdump"]);
    assert_eq!(
        argmatch(&argv, &mut idx, "-temacs", Some("--temacs"), 8, true),
        ArgMatch::Value("pdump".to_string()),
    );
    assert_eq!(idx, 1);
}

#[test]
fn long_with_value_separate_token() {
    let mut idx = 0;
    let argv = args(&["neomacs", "--chdir", "/tmp"]);
    assert_eq!(
        argmatch(&argv, &mut idx, "-chdir", Some("--chdir"), 4, true),
        ArgMatch::Value("/tmp".to_string()),
    );
    assert_eq!(idx, 2);
}

#[test]
fn short_with_value_separate_token() {
    let mut idx = 0;
    let argv = args(&["neomacs", "-t", "/dev/pts/3"]);
    assert_eq!(
        argmatch(&argv, &mut idx, "-t", Some("--terminal"), 4, true),
        ArgMatch::Value("/dev/pts/3".to_string()),
    );
    assert_eq!(idx, 2);
}

#[test]
fn missing_value_at_eof_signals_explicitly() {
    // GNU returns 0 here; we return MissingValue so the caller
    // can produce "neomacs: option `--chdir' requires an argument".
    let mut idx = 0;
    let argv = args(&["neomacs", "--chdir"]);
    assert_eq!(
        argmatch(&argv, &mut idx, "-chdir", Some("--chdir"), 4, true),
        ArgMatch::MissingValue,
    );
    assert_eq!(idx, 0);
}

#[test]
fn no_match_leaves_idx_untouched() {
    let mut idx = 0;
    let argv = args(&["neomacs", "--unrelated"]);
    assert_eq!(
        argmatch(&argv, &mut idx, "-nw", Some("--no-window-system"), 6, false),
        ArgMatch::NoMatch,
    );
    assert_eq!(idx, 0);
}

#[test]
fn empty_argv_returns_no_match() {
    let mut idx = 0;
    let argv = args(&["neomacs"]);
    assert_eq!(
        argmatch(&argv, &mut idx, "-nw", Some("--no-window-system"), 6, false),
        ArgMatch::NoMatch,
    );
    assert_eq!(idx, 0);
}

#[test]
fn long_form_only_short_unset() {
    // Some GNU entries have lstr = NULL (e.g. "-display" alone) and a
    // distinct short string. We model that with `lstr = None`; only the
    // exact short match path is exercised.
    let mut idx = 0;
    let argv = args(&["neomacs", "-display"]);
    assert_eq!(
        argmatch(&argv, &mut idx, "-display", None, 0, false),
        ArgMatch::Bare,
    );
    assert_eq!(idx, 1);
}

#[test]
fn idx_threading_walks_argv_one_match_at_a_time() {
    // Multiple flags on the same argv are handled by repeated calls
    // sharing the same `idx` cursor — the parser pattern in main.rs.
    let argv = args(&["neomacs", "-nw", "--batch"]);
    let mut idx = 0;

    assert_eq!(
        argmatch(&argv, &mut idx, "-nw", Some("--no-window-system"), 6, false),
        ArgMatch::Bare,
    );
    assert_eq!(idx, 1);

    assert_eq!(
        argmatch(&argv, &mut idx, "-batch", Some("--batch"), 5, false),
        ArgMatch::Bare,
    );
    assert_eq!(idx, 2);
}
