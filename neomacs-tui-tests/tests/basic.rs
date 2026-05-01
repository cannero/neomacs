//! TUI comparison tests: basic.

mod support;
use neomacs_tui_tests::*;
use std::time::Duration;
use support::*;

// ── Local helpers ───────────────────────────────────────────

fn is_blank_cell(cell: &vt100::Cell) -> bool {
    cell.contents().trim().is_empty()
}

// ── Tests ──────────────────────────────────────────────────
#[test]
fn control_x_prefix_echo_has_no_trailing_dash() {
    let (mut gnu, mut neo) = boot_pair("");

    send_both(&mut gnu, &mut neo, "C-x");
    read_both(&mut gnu, &mut neo, Duration::from_secs(2));

    let gnu_echo = gnu.row_text(ROWS - 1).trim_end().to_string();
    let neo_echo = neo.row_text(ROWS - 1).trim_end().to_string();
    assert_ne!(
        neo_echo, "C-x-",
        "Neomacs should not eagerly append a dash to C-x prefix echo"
    );
    assert_eq!(
        neo_echo.ends_with('-'),
        gnu_echo.ends_with('-'),
        "Neomacs prefix echo should match GNU trailing-dash state"
    );
}

#[test]
fn terminal_resize_updates_frame_geometry() {
    const TARGET_ROWS: u16 = 30;
    const TARGET_COLS: u16 = 100;

    let (mut gnu, mut neo) = boot_pair("");
    resize_both(&mut gnu, &mut neo, TARGET_ROWS, TARGET_COLS);

    // Drain the resize event before sending input.
    read_both(&mut gnu, &mut neo, Duration::from_secs(2));

    eval_expression(
        &mut gnu,
        &mut neo,
        r#"(message "resize-test %sx%s" (frame-width) (frame-height))"#,
    );

    let expected_frame_height = TARGET_ROWS - 1;
    let expected = format!("resize-test {TARGET_COLS}x{expected_frame_height}");
    gnu.read_until(Duration::from_secs(8), |grid| {
        grid.iter().any(|row| row.contains(&expected))
    });
    neo.read_until(Duration::from_secs(12), |grid| {
        grid.iter().any(|row| row.contains(&expected))
    });

    assert_eq!(gnu.screen_size(), (TARGET_ROWS, TARGET_COLS));
    assert_eq!(neo.screen_size(), (TARGET_ROWS, TARGET_COLS));
    let gnu_grid = gnu.text_grid();
    let neo_grid = neo.text_grid();
    assert!(
        gnu_grid.iter().any(|row| row.contains(&expected)),
        "GNU should report resized frame geometry {expected}\n{}",
        gnu_grid.join("\n")
    );
    assert!(
        neo_grid.iter().any(|row| row.contains(&expected)),
        "Neomacs should report resized frame geometry {expected}\n{}",
        neo_grid.join("\n")
    );
}

#[test]
fn execute_extended_command_tab_completion_via_mx_completes_unique_command() {
    let (mut gnu, mut neo) = boot_pair("");

    open_home_file(
        &mut gnu,
        &mut neo,
        "mx-command-completion.txt",
        "abcdef\nsecond\n",
        "C-x C-f",
    );
    send_both(&mut gnu, &mut neo, "C-a");

    send_both(&mut gnu, &mut neo, "M-x");
    let mx_prompt = |grid: &[String]| grid.last().is_some_and(|row| row.contains("M-x"));
    gnu.read_until(Duration::from_secs(6), mx_prompt);
    neo.read_until(Duration::from_secs(8), mx_prompt);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "execute_extended_command_tab_completion_via_mx_completes_unique_command/prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"overwr");
    }
    send_both(&mut gnu, &mut neo, "TAB");
    let completed = |grid: &[String]| grid.iter().any(|row| row.contains("overwrite-mode"));
    gnu.read_until(Duration::from_secs(6), completed);
    neo.read_until(Duration::from_secs(8), completed);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "execute_extended_command_tab_completion_via_mx_completes_unique_command/completed",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "RET");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "Z");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("Zbcdef"))
            && !grid.iter().any(|row| row.contains("Zabcdef"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "execute_extended_command_tab_completion_via_mx_completes_unique_command",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn keyboard_quit_from_mx_via_cg() {
    let (mut gnu, mut neo) = boot_pair("");

    send_both(&mut gnu, &mut neo, "M-x");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"find-fil");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-g");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*scratch*"))
            && grid
                .iter()
                .any(|row| row.contains("This buffer is for text that is not saved"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("keyboard_quit_from_mx_via_cg", &gnu, &neo, 2);
}

#[test]
fn execute_extended_command_history_via_mx_mp_recalls_previous_command() {
    let (mut gnu, mut neo) = boot_pair("");

    invoke_mx_command(&mut gnu, &mut neo, "calendar");
    let day_header_count = |grid: &[String]| {
        grid.iter()
            .map(|row| row.matches("Su Mo Tu We Th Fr Sa").count())
            .sum::<usize>()
    };
    let calendar_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("Calendar")) && day_header_count(grid) >= 3
    };
    gnu.read_until(Duration::from_secs(8), calendar_ready);
    neo.read_until(Duration::from_secs(10), calendar_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "execute_extended_command_history_via_mx_mp_recalls_previous_command/first-calendar",
        &gnu,
        &neo,
        4,
    );

    send_both_raw(&mut gnu, &mut neo, b"q");
    gnu.read_until(Duration::from_secs(6), scratch_ready);
    neo.read_until(Duration::from_secs(8), scratch_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "execute_extended_command_history_via_mx_mp_recalls_previous_command/quit",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "M-x");
    let mx_prompt = |grid: &[String]| grid.last().is_some_and(|row| row.contains("M-x"));
    gnu.read_until(Duration::from_secs(6), mx_prompt);
    neo.read_until(Duration::from_secs(8), mx_prompt);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "execute_extended_command_history_via_mx_mp_recalls_previous_command/prompt",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "M-p");
    let recalled = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("M-x calendar") || row.contains("M-X calendar"))
    };
    gnu.read_until(Duration::from_secs(6), recalled);
    neo.read_until(Duration::from_secs(8), recalled);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "execute_extended_command_history_via_mx_mp_recalls_previous_command/recalled",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "RET");
    gnu.read_until(Duration::from_secs(8), calendar_ready);
    neo.read_until(Duration::from_secs(10), calendar_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "execute_extended_command_history_via_mx_mp_recalls_previous_command/second-calendar",
        &gnu,
        &neo,
        4,
    );
}

#[test]
fn keyboard_escape_quit_from_mx_via_esc_esc_esc() {
    let (mut gnu, mut neo) = boot_pair("");

    send_both(&mut gnu, &mut neo, "M-x");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"find-fil");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "ESC ESC ESC");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*scratch*"))
            && grid
                .iter()
                .any(|row| row.contains("This buffer is for text that is not saved"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "keyboard_escape_quit_from_mx_via_esc_esc_esc",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn universal_argument_insert_via_cu_8_a() {
    let (mut gnu, mut neo) = boot_pair("");
    send_both(&mut gnu, &mut neo, "C-u 8 a");

    let ready = |grid: &[String]| grid.iter().any(|row| row.contains("aaaaaaaa"));
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("universal_argument_insert_via_cu_8_a", &gnu, &neo, 2);
}

#[test]
fn negative_argument_reverses_forward_word_via_mminus_mf() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "negative-argument.txt",
        "alpha beta gamma\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-f M-f M-- M-f");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"X");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alpha Xbeta gamma"))
            && !grid.iter().any(|row| row.contains("alpha beta gamma"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "negative_argument_reverses_forward_word_via_mminus_mf",
        &gnu,
        &neo,
        2,
    );
}

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
fn boot_blank_cells_use_terminal_default_background() {
    let (gnu, neo) = boot_pair("");
    assert!(
        gnu.text_grid().iter().any(|row| row.contains("*scratch*")),
        "GNU Emacs did not reach the scratch boot screen"
    );
    assert!(
        neo.text_grid().iter().any(|row| row.contains("*scratch*")),
        "Neomacs did not reach the scratch boot screen"
    );

    let mut checked = 0usize;
    let mut mismatches = Vec::new();

    // GNU Emacs leaves ordinary TTY background cells on the terminal default
    // color. A Neomacs regression here painted blank cells explicit white.
    for row in 1..ROWS.saturating_sub(2) {
        for col in 0..COLS {
            let (Some(gnu_cell), Some(neo_cell)) =
                (gnu.screen().cell(row, col), neo.screen().cell(row, col))
            else {
                continue;
            };

            if is_blank_cell(gnu_cell) && is_blank_cell(neo_cell) {
                checked += 1;
                if gnu_cell.bgcolor() != neo_cell.bgcolor() && mismatches.len() < 12 {
                    mismatches.push(format!(
                        "row {row} col {col}: GNU bg {:?}, Neomacs bg {:?}\nGNU: {:?}\nNEO: {:?}",
                        gnu_cell.bgcolor(),
                        neo_cell.bgcolor(),
                        gnu.text_grid().get(row as usize),
                        neo.text_grid().get(row as usize)
                    ));
                }
            }
        }
    }

    assert!(
        checked > 100,
        "Expected many blank body cells to compare, checked {checked}"
    );
    assert!(
        mismatches.is_empty(),
        "Blank body background differs from GNU Emacs:\n{}",
        mismatches.join("\n")
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
fn universal_argument() {
    let (mut gnu, mut neo) = boot_pair("");
    send_both(&mut gnu, &mut neo, "C-u 8 a");
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
