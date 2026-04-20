//! TUI comparison tests for TTY child frame functionality.
//!
//! Each test spawns GNU Emacs and Neomacs side-by-side in isolated PTYs,
//! evaluates Elisp to create/manipulate child frames, and compares the
//! rendered output. These tests exercise GNU Emacs's `(featurep
//! 'tty-child-frames)` feature and serve as a spec for NeoMacs.

use neomacs_tui_tests::*;
use std::time::Duration;

/// Elisp helper loaded via `-l` at boot. Defines `cf--make-child` for
/// concise child-frame creation in tests.
const CHILD_FRAME_INIT: &str = r#";;; -*- lexical-binding: t; -*-
(eval-when-compile (require 'cl-lib))
(defun cf--make-child (&rest params)
  "Create a child frame with PARAMS merged into defaults."
  (let* ((parent (selected-frame))
         (defaults '((parent-frame . nil)
                     (width . 30)
                     (height . 5)
                     (left . 5)
                     (top . 3)
                     (minibuffer . nil)
                     (cursor-type . nil)))
         (full (cons (cons 'parent-frame parent)
                     (cl-remove-duplicates
                      (append params (cdr defaults))
                      :key #'car :from-end t))))
    (make-frame full)))
"#;

fn boot_child_frame_pair() -> (TuiSession, TuiSession) {
    let init = std::env::temp_dir().join("neomacs-child-frame-init.el");
    std::fs::write(&init, CHILD_FRAME_INIT).expect("write child-frame init file");
    let extra_args = format!("-l {}", init.display());
    let mut gnu = TuiSession::gnu_emacs(&extra_args);
    let mut neo = TuiSession::neomacs(&extra_args);
    let startup_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*scratch*"))
            && grid
                .iter()
                .any(|row| row.contains("This buffer is for text that is not saved"))
    };
    gnu.read_until(Duration::from_secs(10), startup_ready);
    neo.read_until(Duration::from_secs(16), startup_ready);
    settle_session(&mut gnu, Duration::from_secs(1), 2);
    settle_session(&mut neo, Duration::from_secs(1), 5);
    std::thread::sleep(Duration::from_secs(3));
    gnu.read(Duration::from_secs(1));
    neo.read(Duration::from_secs(1));
    (gnu, neo)
}

fn settle_session(session: &mut TuiSession, timeout: Duration, max_rounds: usize) {
    let mut previous = session.text_grid();
    for _ in 0..max_rounds {
        session.read(timeout);
        let current = session.text_grid();
        if current == previous {
            return;
        }
        previous = current;
    }
}

fn send_both(gnu: &mut TuiSession, neo: &mut TuiSession, keys: &str) {
    gnu.send_keys(keys);
    neo.send_keys(keys);
}

fn read_both(gnu: &mut TuiSession, neo: &mut TuiSession, timeout: Duration) {
    gnu.read(timeout);
    neo.read(timeout);
}

/// Evaluate an Elisp expression in both sessions via `M-:` and wait for
/// the echo area to update.
fn eval_both(gnu: &mut TuiSession, neo: &mut TuiSession, expr: &str) {
    send_both(gnu, neo, "M-:");
    read_both(gnu, neo, Duration::from_secs(2));
    for s in [&mut *gnu, &mut *neo] {
        s.send(expr.as_bytes());
    }
    send_both(gnu, neo, "RET");
    read_both(gnu, neo, Duration::from_secs(3));
}

fn meaningful_diffs(diffs: Vec<RowDiff>) -> Vec<RowDiff> {
    diffs
        .into_iter()
        .filter(|d| !is_boot_info_row(&d.gnu, &d.neo))
        .collect()
}

fn assert_pair_nearly_matches(
    label: &str,
    gnu: &TuiSession,
    neo: &TuiSession,
    allowed_rows: usize,
) {
    let gl = gnu.text_grid();
    let nl = neo.text_grid();
    let diffs = meaningful_diffs(diff_text_grids(&gl, &nl));
    if !diffs.is_empty() {
        eprintln!("{label}: {} rows differ", diffs.len());
        print_row_diffs(&diffs);
    }
    assert!(
        diffs.len() <= allowed_rows,
        "{label} differs in {} rows",
        diffs.len()
    );
}

/// Check that `text` appears somewhere in the rendered rows `[row_start, row_end)`.
fn assert_region_contains(session: &TuiSession, row_start: usize, row_end: usize, text: &str) {
    let grid = session.text_grid();
    let region: Vec<&String> = grid[row_start..row_end].iter().collect();
    let found = region.iter().any(|row| row.contains(text));
    if !found {
        eprintln!(
            "{}: '{}' not found in rows {row_start}..{row_end}",
            session.name, text
        );
        for (i, row) in region.iter().enumerate() {
            eprintln!("  {:2}: |{}|", row_start + i, row.trim_end());
        }
    }
    assert!(
        found,
        "{}: expected '{}' in rows {row_start}..{row_end}",
        session.name, text
    );
}

/// Delete all child frames (all frames except the selected one).
fn delete_all_child_frames(gnu: &mut TuiSession, neo: &mut TuiSession) {
    let expr = r#"(dolist (f (delq (selected-frame) (frame-list))) (delete-frame f))"#;
    eval_both(gnu, neo, expr);
    read_both(gnu, neo, Duration::from_secs(2));
}
