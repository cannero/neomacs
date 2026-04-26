//! Oracle parity tests for argv handling.
//!
//! Each test asserts that GNU `emacs` and our `neomacs` produce
//! comparable observable startup state when invoked with the same argv.
//! Tests gate on `NEOVM_FORCE_ORACLE_PATH` (the same env var
//! `neovm-oracle-tests` uses); when unset, every test exits early so
//! CI without GNU Emacs available still passes.
//!
//! Background: see `drafts/argv-parity-audit.md` for the ground-truth
//! `standard_args[]` table from `emacs.c:2646-2766` and the per-flag
//! gap analysis these tests gate against.
//!
//! ## Scope today
//!
//! These tests cover both early C-side exits (`--help`, `--version`,
//! `--chdir` failure) and batch startup paths where Lisp observes the
//! sorted/forwarded `command-line-args`. When a parity gap is known, the
//! specific test remains ignored with that gap stated on the test itself.

mod common;

use common::{ProbeResult, run_neomacs, run_oracle_emacs};

fn assert_status_eq(neomacs: &ProbeResult, emacs: &ProbeResult, label: &str) {
    assert_eq!(
        neomacs.status, emacs.status,
        "{label}: exit status differs.\nneomacs: {:?}\nemacs: {:?}",
        neomacs, emacs,
    );
}

fn assert_stdout_parity(neomacs: &ProbeResult, emacs: &ProbeResult, label: &str) {
    assert_eq!(
        neomacs.stdout.trim(),
        emacs.stdout.trim(),
        "{label}: stdout differs.\nneomacs stdout: {:?}\nneomacs stderr: {:?}\nemacs stdout: {:?}\nemacs stderr: {:?}",
        neomacs.stdout,
        neomacs.stderr,
        emacs.stdout,
        emacs.stderr,
    );
}

// ---------- enabled today ----------

#[test]
fn version_flag_exits_zero_and_prints_something() {
    skip_unless_oracle!();
    // GNU emacs.c:1508 / 2222 — `--version` prints version info and
    // exits 0. We also handle this via `classify_early_cli_action`
    // which short-circuits before `parse_startup_options`.
    let n = run_neomacs(&["--version"]);
    let e = run_oracle_emacs(&["--version"]);
    assert_status_eq(&n, &e, "--version exit");
    assert!(
        !n.stdout.is_empty(),
        "neomacs --version should print to stdout: {n:?}"
    );
    assert!(
        !e.stdout.is_empty(),
        "emacs --version should print to stdout: {e:?}"
    );
}

#[test]
fn help_flag_exits_zero_and_prints_something() {
    skip_unless_oracle!();
    // GNU emacs.c:1720 — `--help` prints usage and exits 0. We do the
    // same via `classify_early_cli_action`. The exact text differs (we
    // ship our own usage table) but both must exit 0 with non-empty
    // output.
    let n = run_neomacs(&["--help"]);
    let e = run_oracle_emacs(&["--help"]);
    assert_status_eq(&n, &e, "--help exit");
    assert!(!n.stdout.is_empty(), "neomacs --help should print");
    assert!(!e.stdout.is_empty(), "emacs --help should print");
}

#[test]
fn chdir_to_nonexistent_path_fails_with_nonzero_exit() {
    skip_unless_oracle!();
    // GNU emacs.c:1551 — chdir failure prints "X: Can't chdir to Y: Z"
    // to stderr and exits 1. Phase 3a mirrors this exit path. We don't
    // diff the exact stderr text (GNU prefixes with argv[0], which
    // differs by binary name) — only the non-zero exit and the
    // characteristic "chdir to" prefix.
    let n = run_neomacs(&["--chdir", "/this/path/cannot/possibly/exist", "--batch"]);
    let e = run_oracle_emacs(&["--chdir", "/this/path/cannot/possibly/exist", "--batch"]);
    assert_status_eq(&n, &e, "--chdir failure exit");
    assert_ne!(n.status, 0, "neomacs should exit non-zero on chdir failure");
    assert!(
        n.stderr.contains("chdir to"),
        "neomacs stderr should mention chdir failure: {:?}",
        n.stderr
    );
    assert!(
        e.stderr.contains("chdir to"),
        "emacs stderr should mention chdir failure: {:?}",
        e.stderr
    );
}

#[test]
fn batch_eval_prints_result() {
    skip_unless_oracle!();
    let argv = ["--batch", "--eval", "(princ (+ 1 2))"];
    let n = run_neomacs(&argv);
    let e = run_oracle_emacs(&argv);
    assert_stdout_parity(&n, &e, "batch eval parity");
}

#[test]
fn chdir_changes_default_directory() {
    skip_unless_oracle!();
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    let dir_str = dir.to_string_lossy().into_owned();
    let argv: Vec<&str> = vec![
        "--batch",
        "--chdir",
        &dir_str,
        "--eval",
        "(princ default-directory)",
    ];
    let n = run_neomacs(&argv);
    let e = run_oracle_emacs(&argv);
    assert_stdout_parity(&n, &e, "chdir parity");
}

#[test]
fn quick_passes_through_to_lisp() {
    skip_unless_oracle!();
    // -Q must remain in command-line-args after the C-side peek so the
    // Lisp side can also act on it (Phase 3d).
    let argv = [
        "-Q",
        "--batch",
        "--eval",
        "(princ (member \"-Q\" command-line-args))",
    ];
    let n = run_neomacs(&argv);
    let e = run_oracle_emacs(&argv);
    assert_stdout_parity(&n, &e, "-Q peek parity");
}

#[test]
fn no_site_lisp_drops_site_lisp_from_load_path() {
    skip_unless_oracle!();
    let argv = [
        "--no-site-lisp",
        "--batch",
        "--eval",
        "(princ (catch 'found (dolist (p load-path nil) (when (and (stringp p) (string-match-p \"site-lisp\" p)) (throw 'found t)))))",
    ];
    let n = run_neomacs(&argv);
    let e = run_oracle_emacs(&argv);
    assert_stdout_parity(&n, &e, "--no-site-lisp parity");
}

#[test]
fn batch_implies_noninteractive() {
    skip_unless_oracle!();
    let argv = ["--batch", "--eval", "(princ (if noninteractive 't 'nil))"];
    let n = run_neomacs(&argv);
    let e = run_oracle_emacs(&argv);
    assert_stdout_parity(&n, &e, "--batch noninteractive parity");
}

#[test]
fn sort_args_orders_options_canonically() {
    skip_unless_oracle!();
    // The sort_args (Phase 2) parity check: any permutation of the
    // same flag set must produce the same canonical command-line-args
    // when walked by lisp/startup.el.
    // Skip argv[0]: Cargo invokes Neomacs by full test binary path while
    // the oracle is usually invoked as just `emacs`.
    let probe = "(princ (mapconcat 'identity (cdr command-line-args) \"|\"))";
    let argv_a = ["--batch", "-Q", "--eval", probe];
    let argv_b = ["-Q", "--batch", "--eval", probe];

    let na = run_neomacs(&argv_a);
    let nb = run_neomacs(&argv_b);
    let ea = run_oracle_emacs(&argv_a);
    let eb = run_oracle_emacs(&argv_b);

    assert_stdout_parity(&na, &ea, "sort_args parity (variant a)");
    assert_stdout_parity(&nb, &eb, "sort_args parity (variant b)");
    assert_eq!(
        na.stdout.trim(),
        nb.stdout.trim(),
        "neomacs sort_args should canonicalize ordering across permutations"
    );
}

#[test]
fn double_dash_terminator_passes_through() {
    skip_unless_oracle!();
    let argv = [
        "--batch",
        "--eval",
        "(princ (member \"literal-arg\" command-line-args))",
        "--",
        "literal-arg",
    ];
    let n = run_neomacs(&argv);
    let e = run_oracle_emacs(&argv);
    assert_stdout_parity(&n, &e, "-- terminator parity");
}
