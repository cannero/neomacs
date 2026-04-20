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
    let startup_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*scratch*"))
            && grid
                .iter()
                .any(|row| row.contains("This buffer is for text that is not saved"))
    };
    gnu.read_until(Duration::from_secs(8), startup_ready);
    neo.read_until(Duration::from_secs(12), startup_ready);
    // Startup can legitimately produce a later burst after the initial
    // `*scratch*` screen becomes visible. Absorb that tail so the first input
    // keystroke does not race the end of startup under parallel load.
    gnu.read(Duration::from_secs(1));
    neo.read(Duration::from_secs(2));
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

fn grid_has_two_scratch_windows(grid: &[String]) -> bool {
    grid.iter().filter(|row| row.contains("*scratch*")).count() >= 2
}

fn wait_for_split_window_below(gnu: &mut TuiSession, neo: &mut TuiSession) {
    let timeout = Duration::from_secs(5);
    gnu.read_until(timeout, grid_has_two_scratch_windows);
    neo.read_until(timeout, grid_has_two_scratch_windows);
}

fn wait_for_other_window_after_split(gnu: &mut TuiSession, neo: &mut TuiSession) {
    let timeout = Duration::from_secs(5);
    gnu.read_until(timeout, |grid| {
        grid_has_two_scratch_windows(grid)
            && grid
                .last()
                .is_some_and(|row| !row.contains("No other window to select"))
    });
    neo.read_until(timeout, |grid| {
        grid_has_two_scratch_windows(grid)
            && grid
                .last()
                .is_some_and(|row| !row.contains("No other window to select"))
    });
}

/// Filter out boot-info rows (welcome text, copyright) from diffs.
fn meaningful_diffs(diffs: Vec<RowDiff>) -> Vec<RowDiff> {
    diffs
        .into_iter()
        .filter(|d| !is_boot_info_row(&d.gnu, &d.neo))
        .collect()
}

fn normalize_hello_vc_row(row: &str) -> String {
    let Some(start) = row.find("Git-") else {
        return row.to_string();
    };
    let Some(rest) = row.get(start..) else {
        return row.to_string();
    };
    let end = rest
        .find("  (")
        .map(|offset| start + offset)
        .unwrap_or(row.len());
    let target_width = row.chars().count();
    let mut normalized = String::with_capacity(row.len());
    normalized.push_str(&row[..start]);
    normalized.push_str("Git-REV1234");
    normalized.push_str(&row[end..]);
    normalized.chars().take(target_width).collect()
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
    wait_for_split_window_below(&mut gnu, &mut neo);

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

    // The 8 a's are inserted at point (end of buffer, after comments).
    // Check that SOME row contains "aaaaaaaa".
    let gnu_has_8a = gl.iter().any(|r| r.contains("aaaaaaaa"));
    let neo_has_8a = nl.iter().any(|r| r.contains("aaaaaaaa"));
    if !gnu_has_8a {
        eprintln!("GNU screen (no 8 a's found):");
        for (i, r) in gl.iter().enumerate() {
            let t = r.trim();
            if !t.is_empty() {
                eprintln!("  {i:2}: |{t}|");
            }
        }
    }
    if !neo_has_8a {
        eprintln!("NEO screen (no 8 a's found):");
        for (i, r) in nl.iter().enumerate() {
            let t = r.trim();
            if !t.is_empty() {
                eprintln!("  {i:2}: |{t}|");
            }
        }
    }
    assert!(gnu_has_8a, "GNU buffer should have 8 a's somewhere");
    assert!(neo_has_8a, "NEO buffer should have 8 a's somewhere");
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
    wait_for_split_window_below(&mut gnu, &mut neo);
    send_both(&mut gnu, &mut neo, "C-x o");
    wait_for_other_window_after_split(&mut gnu, &mut neo);

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
    std::fs::write(
        init,
        ";;; -*- lexical-binding: t; -*-\n(fido-vertical-mode 1)\n",
    )
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
    let gnu_nonempty_bottom = gl[18..24].iter().filter(|r| !r.trim().is_empty()).count();
    let neo_nonempty_bottom = nl[18..24].iter().filter(|r| !r.trim().is_empty()).count();

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
    let neo_nonempty_after = nl2[20..24].iter().filter(|r| !r.trim().is_empty()).count();
    eprintln!("NEO bottom 4 rows after C-g: {neo_nonempty_after}");
    assert!(
        neo_nonempty_after <= 2,
        "Neomacs minibuffer should shrink after C-g (got {neo_nonempty_after} non-empty rows)"
    );
}

#[test]
fn mx_view_hello_file() {
    // M-x view-hello-file opens the built-in etc/HELLO file (the multilingual
    // "hello" demo). Content includes "Hello, world!" (English row) plus many
    // other-language greetings.
    let (mut gnu, mut neo) = boot_pair("");

    send_both(&mut gnu, &mut neo, "M-x");
    read_both(&mut gnu, &mut neo, Duration::from_secs(3));
    for s in [&mut gnu, &mut neo] {
        s.send(b"view-hello-file");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "RET");
    // `view-hello-file` runs format-decode → enriched-decode → view-mode
    // setup with pauses between stages, which can exceed the idle-detect
    // threshold. Wait explicitly for the buffer switch (mode-line shows
    // "HELLO") instead of relying on a plain timeout.
    let wants_hello = |rows: &[String]| rows.iter().any(|r| r.contains("HELLO"));
    gnu.read_until(Duration::from_secs(5), wants_hello);
    neo.read_until(Duration::from_secs(5), wants_hello);

    let gl = gnu.text_grid();
    let nl = neo.text_grid();

    // HELLO opens in view-mode and its content varies (many languages). Both
    // editors should at least have "hello" (case-insensitive) somewhere in
    // the visible rows — that covers the explanatory "…write a 'hello' …"
    // opening, the English "Hola" / "Hello" rows, etc.
    let lower_has_hello = |rows: &[String]| rows.iter().any(|r| r.to_lowercase().contains("hello"));
    let gnu_has_hello = lower_has_hello(&gl);
    let neo_has_hello = lower_has_hello(&nl);

    let dump = |label: &str, rows: &[String]| {
        eprintln!("{label} screen:");
        for (i, r) in rows.iter().enumerate() {
            let t = r.trim();
            if !t.is_empty() {
                eprintln!("  {i:2}: |{t}|");
            }
        }
    };
    if !gnu_has_hello {
        dump("GNU", &gl);
    }
    if !neo_has_hello {
        dump("NEO", &nl);
    }
    assert!(
        gnu_has_hello,
        "GNU should show some 'hello' text after M-x view-hello-file"
    );
    assert!(
        neo_has_hello,
        "NEO should show some 'hello' text after M-x view-hello-file"
    );

    // Mode line should surface the buffer name HELLO.
    let gnu_has_name = gl.iter().any(|r| r.contains("HELLO"));
    let neo_has_name = nl.iter().any(|r| r.contains("HELLO"));
    assert!(gnu_has_name, "GNU should show HELLO in the mode line");
    assert!(neo_has_name, "NEO should show HELLO in the mode line");
}

/// Strict 100%-match aspiration test: after `M-x view-hello-file`, NeoMacs
/// should render exactly the same rows as GNU Emacs. Currently fails because
/// of known feature gaps (not gap-buffer bugs):
///
///   - `enriched-mode` is not auto-activated from the buffer's
///     `Content-Type: text/enriched` header, so the enriched markup
///     (`<x-color>`, `<x-charset>`, `<param>…</param>`) renders as literal
///     text and shifts every row down by ~3.
///   - VC-mode is not wired in, so the mode line is missing the Git branch
///     marker (GNU shows "Git-<sha>").
///   - view-mode echo-area hint text uses a different fallback phrasing
///     (NEO: "M-x help-command for help", GNU: "C-h for help") because the
///     C-h binding in view-mode isn't set up the same way.
///   - `global-eldoc-mode` is on by default in NEO and adds " ElDoc" to
///     the mode-line minor-mode list, which GNU omits.
///
#[test]
fn mx_view_hello_file_strict_match() {
    let (mut gnu, mut neo) = boot_pair("");

    send_both(&mut gnu, &mut neo, "M-x");
    read_both(&mut gnu, &mut neo, Duration::from_secs(3));
    for s in [&mut gnu, &mut neo] {
        s.send(b"view-hello-file");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "RET");
    // Wait for the HELLO buffer to finish rendering (mode-line shows
    // "HELLO") instead of trusting a fixed timeout, same as the
    // non-strict mx_view_hello_file test.
    let wants_hello = |rows: &[String]| rows.iter().any(|r| r.contains("HELLO"));
    gnu.read_until(Duration::from_secs(5), wants_hello);
    neo.read_until(Duration::from_secs(5), wants_hello);

    let gl = gnu.text_grid();
    let nl = neo.text_grid();
    assert!(
        gl.iter().any(|row| row.contains("Git-")),
        "GNU HELLO mode line should show VC status"
    );
    assert!(
        nl.iter().any(|row| row.contains("Git-")),
        "NEO HELLO mode line should show VC status"
    );
    let gl_normalized: Vec<String> = gl.iter().map(|row| normalize_hello_vc_row(row)).collect();
    let nl_normalized: Vec<String> = nl.iter().map(|row| normalize_hello_vc_row(row)).collect();
    let diffs = diff_text_grids(&gl_normalized, &nl_normalized);

    if !diffs.is_empty() {
        eprintln!(
            "mx_view_hello_file_strict_match: {} of {} rows differ",
            diffs.len(),
            gl_normalized.len().min(nl_normalized.len())
        );
        print_row_diffs(&diffs);
    }
    assert_eq!(
        diffs.len(),
        0,
        "NEO and GNU HELLO buffers should be byte-for-byte identical (differ in {} rows)",
        diffs.len()
    );
}
