//! Basic TUI comparison tests: boot screen, window splits, echo area.
//!
//! Each test spawns GNU Emacs and Neomacs side-by-side in isolated PTYs,
//! sends identical keystrokes, and asserts the rendered screens match.

use neomacs_tui_tests::*;
use std::time::Duration;

/// Helper: boot both editors, wait for them to settle.
fn boot_pair(extra_args: &str) -> (TuiSession, TuiSession) {
    let mut gnu = TuiSession::gnu_emacs(extra_args);
    let mut neo = TuiSession::neomacs(extra_args);
    gnu.read(Duration::from_secs(8));
    neo.read(Duration::from_secs(12));
    (gnu, neo)
}

/// Helper: send the same keys to both sessions.
fn send_both(gnu: &mut TuiSession, neo: &mut TuiSession, keys: &str) {
    gnu.send_keys(keys);
    neo.send_keys(keys);
}

/// Helper: read output from both sessions.
fn read_both(gnu: &mut TuiSession, neo: &mut TuiSession, timeout: Duration) {
    gnu.read(timeout);
    neo.read(timeout);
}

/// Filter out boot-info rows (welcome text, copyright) from diffs.
fn meaningful_diffs(diffs: Vec<RowDiff>) -> Vec<RowDiff> {
    diffs
        .into_iter()
        .filter(|d| !is_boot_info_row(&d.gnu, &d.neo))
        .collect()
}

// ── Tests ────────────────────────────────────────────────────────────

#[test]
fn boot_screen_layout() {
    let (gnu, neo) = boot_pair("");
    let gl = gnu.text_grid();
    let nl = neo.text_grid();
    let diffs = meaningful_diffs(diff_text_grids(&gl, &nl));
    if !diffs.is_empty() {
        eprintln!("boot_screen_layout: {} rows differ", diffs.len());
        print_row_diffs(&diffs);
    }
    assert!(
        diffs.len() <= 2,
        "Boot screens differ in {} rows (expected <= 2 for menu bar / echo area)",
        diffs.len()
    );
}

#[test]
fn split_window_below() {
    let (mut gnu, mut neo) = boot_pair("");
    send_both(&mut gnu, &mut neo, "C-x 2");
    read_both(&mut gnu, &mut neo, Duration::from_secs(2));

    let gl = gnu.text_grid();
    let nl = neo.text_grid();
    let diffs = meaningful_diffs(diff_text_grids(&gl, &nl));
    if !diffs.is_empty() {
        eprintln!("split_window_below: {} rows differ", diffs.len());
        print_row_diffs(&diffs);
    }
    assert!(
        diffs.len() <= 2,
        "C-x 2 screens differ in {} rows",
        diffs.len()
    );
}

#[test]
fn split_window_right() {
    let (mut gnu, mut neo) = boot_pair("");
    send_both(&mut gnu, &mut neo, "C-x 3");
    read_both(&mut gnu, &mut neo, Duration::from_secs(2));

    let gl = gnu.text_grid();
    let nl = neo.text_grid();
    let diffs = meaningful_diffs(diff_text_grids(&gl, &nl));
    if !diffs.is_empty() {
        eprintln!("split_window_right: {} rows differ", diffs.len());
        print_row_diffs(&diffs);
    }
    assert!(
        diffs.len() <= 2,
        "C-x 3 screens differ in {} rows",
        diffs.len()
    );
}

#[test]
fn mx_prompt() {
    let (mut gnu, mut neo) = boot_pair("");
    send_both(&mut gnu, &mut neo, "M-x");
    read_both(&mut gnu, &mut neo, Duration::from_secs(3));

    let gl = gnu.text_grid();
    let nl = neo.text_grid();

    // The last row should contain "M-x " in both
    let gnu_last = gl.last().unwrap();
    let neo_last = nl.last().unwrap();
    assert!(
        gnu_last.contains("M-x"),
        "GNU last row should contain 'M-x': {gnu_last:?}"
    );
    assert!(
        neo_last.contains("M-x"),
        "NEO last row should contain 'M-x': {neo_last:?}"
    );

    // Cancel
    send_both(&mut gnu, &mut neo, "C-g");
}

#[test]
fn eval_expression() {
    let (mut gnu, mut neo) = boot_pair("");
    send_both(&mut gnu, &mut neo, "M-:");
    read_both(&mut gnu, &mut neo, Duration::from_secs(2));

    // Type (+ 1 2) RET
    for s in [&mut gnu, &mut neo] {
        s.send(b"(+ 1 2)");
    }
    std::thread::sleep(Duration::from_millis(500));
    send_both(&mut gnu, &mut neo, "RET");
    read_both(&mut gnu, &mut neo, Duration::from_secs(2));

    // Echo area (last row) should show "3"
    let gl = gnu.text_grid();
    let nl = neo.text_grid();
    let gnu_echo = gl.last().unwrap();
    let neo_echo = nl.last().unwrap();
    assert!(
        gnu_echo.contains('3'),
        "GNU echo should show 3: {gnu_echo:?}"
    );
    assert!(
        neo_echo.contains('3'),
        "NEO echo should show 3: {neo_echo:?}"
    );
}

#[test]
fn universal_argument() {
    let (mut gnu, mut neo) = boot_pair("");
    send_both(&mut gnu, &mut neo, "C-u");
    std::thread::sleep(Duration::from_millis(200));
    for s in [&mut gnu, &mut neo] {
        s.send(b"8a");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(2));

    let gl = gnu.text_grid();
    let nl = neo.text_grid();

    // First row should contain "aaaaaaaa" (8 a's)
    // The menu bar is row 0, buffer starts at row 1
    let gnu_buf = &gl[1];
    let neo_buf = &nl[1];
    assert!(
        gnu_buf.contains("aaaaaaaa"),
        "GNU buffer should have 8 a's: {gnu_buf:?}"
    );
    assert!(
        neo_buf.contains("aaaaaaaa"),
        "NEO buffer should have 8 a's: {neo_buf:?}"
    );
}

#[test]
fn echo_area_message() {
    let (mut gnu, mut neo) = boot_pair("");
    // C-x = (what-cursor-position) shows char info in echo area
    send_both(&mut gnu, &mut neo, "C-x =");
    read_both(&mut gnu, &mut neo, Duration::from_secs(2));

    let gl = gnu.text_grid();
    let nl = neo.text_grid();
    let gnu_echo = gl.last().unwrap();
    let neo_echo = nl.last().unwrap();

    // Both should show cursor position info (contains "Char:" or "point=")
    let gnu_has_info = gnu_echo.contains("Char") || gnu_echo.contains("point");
    let neo_has_info = neo_echo.contains("Char") || neo_echo.contains("point");

    if !gnu_has_info {
        eprintln!("GNU echo area: {gnu_echo:?}");
    }
    if !neo_has_info {
        eprintln!("NEO echo area: {neo_echo:?}");
    }

    // At minimum, check neomacs shows something in the echo area
    assert!(
        neo_has_info || !neo_echo.trim().is_empty(),
        "NEO echo area should show cursor info after C-x ="
    );
}

#[test]
fn isearch_forward() {
    let (mut gnu, mut neo) = boot_pair("");
    send_both(&mut gnu, &mut neo, "C-s");
    std::thread::sleep(Duration::from_millis(500));
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));
    for s in [&mut gnu, &mut neo] {
        s.send(b"buffer");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(2));

    let gl = gnu.text_grid();
    let nl = neo.text_grid();

    // Echo area should show "I-search: buffer" or similar
    let gnu_echo = gl.last().unwrap();
    let neo_echo = nl.last().unwrap();
    assert!(
        gnu_echo.contains("search") || gnu_echo.contains("buffer"),
        "GNU should show isearch: {gnu_echo:?}"
    );
    assert!(
        neo_echo.contains("search") || neo_echo.contains("buffer"),
        "NEO should show isearch: {neo_echo:?}"
    );

    send_both(&mut gnu, &mut neo, "C-g");
}

#[test]
fn other_window_after_split() {
    let (mut gnu, mut neo) = boot_pair("");
    send_both(&mut gnu, &mut neo, "C-x 2");
    read_both(&mut gnu, &mut neo, Duration::from_secs(2));
    send_both(&mut gnu, &mut neo, "C-x o");
    read_both(&mut gnu, &mut neo, Duration::from_secs(2));

    let gl = gnu.text_grid();
    let nl = neo.text_grid();
    let diffs = meaningful_diffs(diff_text_grids(&gl, &nl));
    if !diffs.is_empty() {
        eprintln!("other_window_after_split: {} rows differ", diffs.len());
        print_row_diffs(&diffs);
    }
    // Allow some tolerance for cursor position display
    assert!(
        diffs.len() <= 3,
        "C-x 2, C-x o screens differ in {} rows",
        diffs.len()
    );
}

#[test]
fn fido_vertical_mode_completions() {
    // Create init file
    let init = "/tmp/tui-cmp-fido-test.el";
    std::fs::write(init, ";;; -*- lexical-binding: t; -*-\n(fido-vertical-mode 1)\n")
        .expect("write init file");

    let init_arg = format!("-l {init}");
    let (mut gnu, mut neo) = boot_pair(&init_arg);

    // M-x then type to trigger completions
    send_both(&mut gnu, &mut neo, "M-x");
    read_both(&mut gnu, &mut neo, Duration::from_secs(3));
    for ch in b"forw" {
        for s in [&mut gnu, &mut neo] {
            s.send(&[*ch]);
        }
        std::thread::sleep(Duration::from_millis(500));
        read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(3));

    let gl = gnu.text_grid();
    let nl = neo.text_grid();

    // Check that completions are visible (multiple non-empty rows near bottom)
    let gnu_nonempty_bottom = gl[18..24]
        .iter()
        .filter(|r| !r.trim().is_empty())
        .count();
    let neo_nonempty_bottom = nl[18..24]
        .iter()
        .filter(|r| !r.trim().is_empty())
        .count();

    eprintln!("GNU bottom 6 rows with content: {gnu_nonempty_bottom}");
    eprintln!("NEO bottom 6 rows with content: {neo_nonempty_bottom}");
    for (i, (g, n)) in gl.iter().zip(nl.iter()).enumerate() {
        let gt = g.trim();
        let nt = n.trim();
        if !gt.is_empty() || !nt.is_empty() {
            eprintln!("  {i:2}: GNU=|{gt}| NEO=|{nt}|");
        }
    }

    assert!(
        neo_nonempty_bottom >= 2,
        "Neomacs should show fido completion candidates (got {neo_nonempty_bottom} non-empty rows)"
    );

    // C-g — minibuffer should shrink back
    send_both(&mut gnu, &mut neo, "C-g");
    read_both(&mut gnu, &mut neo, Duration::from_secs(3));

    let _gl2 = gnu.text_grid();
    let nl2 = neo.text_grid();

    // After C-g, the bottom area should be mostly empty again
    let neo_nonempty_after = nl2[20..24]
        .iter()
        .filter(|r| !r.trim().is_empty())
        .count();
    eprintln!("NEO bottom 4 rows after C-g: {neo_nonempty_after}");
    assert!(
        neo_nonempty_after <= 2,
        "Neomacs minibuffer should shrink after C-g (got {neo_nonempty_after} non-empty rows)"
    );
}
