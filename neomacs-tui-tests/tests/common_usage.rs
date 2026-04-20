//! TUI comparisons for common Emacs workflows from GNU's tutorial.
//!
//! The command set here is chosen from `lisp/tutorial.el`, which
//! documents the default key sequences GNU Emacs treats as the common
//! day-to-day editing path.

use neomacs_tui_tests::*;
use std::fs;
use std::time::Duration;

fn boot_pair(extra_args: &str) -> (TuiSession, TuiSession) {
    let mut gnu = TuiSession::gnu_emacs(extra_args);
    let mut neo = TuiSession::neomacs(extra_args);
    let startup_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*scratch*"))
            && grid
                .iter()
                .any(|row| row.contains("This buffer is for text that is not saved"))
            && grid
                .iter()
                .any(|row| row.contains("For information about GNU Emacs and the GNU system"))
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

fn boot_fido_vertical_pair() -> (TuiSession, TuiSession) {
    let init = std::env::temp_dir().join("neomacs-common-usage-fido-vertical.el");
    fs::write(
        &init,
        ";;; -*- lexical-binding: t; -*-\n\
         (setq max-mini-window-height 8\n\
               resize-mini-windows t\n\
               icomplete-prospects-height 8)\n\
         (fido-vertical-mode 1)\n",
    )
    .expect("write fido vertical init file");
    let extra_args = format!("-l {}", init.display());
    boot_pair(&extra_args)
}

fn send_both(gnu: &mut TuiSession, neo: &mut TuiSession, keys: &str) {
    gnu.send_keys(keys);
    neo.send_keys(keys);
}

fn send_both_raw(gnu: &mut TuiSession, neo: &mut TuiSession, bytes: &[u8]) {
    gnu.send(bytes);
    neo.send(bytes);
}

fn read_both(gnu: &mut TuiSession, neo: &mut TuiSession, timeout: Duration) {
    gnu.read(timeout);
    neo.read(timeout);
}

fn scratch_ready(grid: &[String]) -> bool {
    grid.iter().any(|row| row.contains("*scratch*"))
        && grid
            .iter()
            .any(|row| row.contains("This buffer is for text that is not saved"))
}

fn send_help_sequence(gnu: &mut TuiSession, neo: &mut TuiSession, key: &str) {
    send_both(gnu, neo, "C-h");
    let prefix_ready = |grid: &[String]| grid.iter().any(|row| row.contains("C-h-"));
    gnu.read_until(Duration::from_secs(6), prefix_ready);
    neo.read_until(Duration::from_secs(8), prefix_ready);
    read_both(gnu, neo, Duration::from_millis(300));
    send_both(gnu, neo, key);
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

fn assert_bottom_rows_nearly_match(
    label: &str,
    gnu: &TuiSession,
    neo: &TuiSession,
    first_row: usize,
    allowed_rows: usize,
) {
    let gl = gnu.text_grid();
    let nl = neo.text_grid();
    let diffs = diff_text_grids(&gl[first_row..], &nl[first_row..])
        .into_iter()
        .map(|mut diff| {
            diff.row += first_row;
            diff
        })
        .collect::<Vec<_>>();
    if !diffs.is_empty() {
        eprintln!("{label}: {} bottom rows differ", diffs.len());
        print_row_diffs(&diffs);
    }
    assert!(
        diffs.len() <= allowed_rows,
        "{label} bottom rows differ in {} rows",
        diffs.len()
    );
}

fn bottom_nonempty_rows(session: &TuiSession, first_row: usize) -> Vec<String> {
    session
        .text_grid()
        .into_iter()
        .skip(first_row)
        .map(|row| row.trim().to_string())
        .filter(|row| !row.is_empty())
        .collect()
}

fn assert_fido_prompt_matches_stable_behavior(label: &str, gnu: &TuiSession, neo: &TuiSession) {
    let gnu_rows = bottom_nonempty_rows(gnu, 16);
    let neo_rows = bottom_nonempty_rows(neo, 16);

    assert!(
        !gnu_rows.is_empty() && !neo_rows.is_empty(),
        "{label} should show non-empty minibuffer rows"
    );
    assert_eq!(
        gnu_rows[0], neo_rows[0],
        "{label} should show the same prompt header"
    );
    assert_eq!(
        gnu_rows.len(),
        neo_rows.len(),
        "{label} should use the same number of visible minibuffer rows"
    );

    let gnu_find_file = gnu_rows
        .iter()
        .find(|row| row.contains("find-file"))
        .cloned()
        .expect("GNU should show find-file");
    let neo_find_file = neo_rows
        .iter()
        .find(|row| row.contains("find-file"))
        .cloned()
        .expect("NEO should show find-file");
    assert_eq!(
        gnu_find_file, neo_find_file,
        "{label} should agree on the top find-file candidate"
    );

    for stable in [
        "find-file",
        "ido-find-file",
        "find-function",
        "hexl-find-file",
        "woman-find-file",
    ] {
        assert!(
            gnu_rows.iter().any(|row| row.contains(stable)),
            "{label} GNU should show {stable}"
        );
        assert!(
            neo_rows.iter().any(|row| row.contains(stable)),
            "{label} NEO should show {stable}"
        );
    }
}

fn dump_pair_grids(label: &str, gnu: &TuiSession, neo: &TuiSession) {
    eprintln!("{label}: GNU grid");
    for (row, text) in gnu.text_grid().iter().enumerate() {
        eprintln!("  {row:02}: |{}|", text.trim_end());
    }
    eprintln!("{label}: NEO grid");
    for (row, text) in neo.text_grid().iter().enumerate() {
        eprintln!("  {row:02}: |{}|", text.trim_end());
    }
    let diffs = meaningful_diffs(diff_text_grids(&gnu.text_grid(), &neo.text_grid()));
    if !diffs.is_empty() {
        eprintln!("{label}: {} differing rows", diffs.len());
        print_row_diffs(&diffs);
    }
}

fn wait_for_fido_mx_candidates(gnu: &mut TuiSession, neo: &mut TuiSession, query: &str) {
    send_both(gnu, neo, "M-x");
    let prompt_ready = |grid: &[String]| grid.last().is_some_and(|row| row.contains("M-x"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(gnu, neo, Duration::from_millis(300));

    gnu.send(query.as_bytes());
    neo.send(query.as_bytes());
    let candidates_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("find-file"))
            && grid[16..24]
                .iter()
                .filter(|row| !row.trim().is_empty())
                .count()
                >= 3
    };
    gnu.read_until(Duration::from_secs(6), candidates_ready);
    neo.read_until(Duration::from_secs(8), candidates_ready);
    read_both(gnu, neo, Duration::from_secs(1));
}

fn abort_minibuffer_and_wait_for_scratch(gnu: &mut TuiSession, neo: &mut TuiSession) {
    send_both(gnu, neo, "C-g");
    gnu.read_until(Duration::from_secs(6), scratch_ready);
    neo.read_until(Duration::from_secs(8), scratch_ready);
    read_both(gnu, neo, Duration::from_secs(1));
}

fn write_home_file(session: &TuiSession, name: &str, contents: &str) {
    let path = session.home_dir().join(name);
    fs::write(path, contents).expect("write test file in isolated HOME");
}

fn open_home_file(
    gnu: &mut TuiSession,
    neo: &mut TuiSession,
    name: &str,
    contents: &str,
    keys: &str,
) {
    write_home_file(gnu, name, contents);
    write_home_file(neo, name, contents);

    send_both(gnu, neo, keys);
    let minibuffer_path = format!("~/{name}");
    gnu.send(minibuffer_path.as_bytes());
    neo.send(minibuffer_path.as_bytes());
    send_both(gnu, neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains(name))
            && grid.iter().any(|row| {
                contents
                    .lines()
                    .next()
                    .is_some_and(|line| row.contains(line))
            })
    };
    gnu.read_until(Duration::from_secs(10), ready);
    neo.read_until(Duration::from_secs(20), ready);
    read_both(gnu, neo, Duration::from_secs(1));
}

#[test]
fn find_file_via_cx_cf() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "common-usage.txt",
        "alpha line\nbeta line\n",
        "C-x C-f",
    );
    assert_pair_nearly_matches("find_file_via_cx_cf", &gnu, &neo, 2);
}

#[test]
fn list_buffers_after_find_file() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "common-usage.txt",
        "alpha line\nbeta line\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x C-b");
    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*Buffer List*"))
            && grid.iter().any(|row| row.contains("common-usage.txt"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("list_buffers_after_find_file", &gnu, &neo, 2);
}

#[test]
fn switch_to_messages_buffer_via_cx_b() {
    let (mut gnu, mut neo) = boot_pair("");

    send_both(&mut gnu, &mut neo, "M-:");
    read_both(&mut gnu, &mut neo, Duration::from_secs(2));
    for session in [&mut gnu, &mut neo] {
        session.send(br#"(message "common usage smoke")"#);
    }
    send_both(&mut gnu, &mut neo, "RET");
    read_both(&mut gnu, &mut neo, Duration::from_secs(2));

    send_both(&mut gnu, &mut neo, "C-x b");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"*Messages*");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*Messages*"))
            && grid.iter().any(|row| row.contains("common usage smoke"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("switch_to_messages_buffer_via_cx_b", &gnu, &neo, 2);
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
fn fido_vertical_mode_mx_find_f_matches_gnu_then_cg() {
    let (mut gnu, mut neo) = boot_fido_vertical_pair();

    wait_for_fido_mx_candidates(&mut gnu, &mut neo, "find-f");
    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains("find-file")),
            "{label} should show find-file in fido candidates"
        );
        assert!(
            grid[16..24]
                .iter()
                .filter(|row| !row.trim().is_empty())
                .count()
                >= 3,
            "{label} should expand the minibuffer into a vertical candidate list"
        );
    }
    assert_bottom_rows_nearly_match(
        "fido_vertical_mode_mx_find_f_matches_gnu_then_cg/prompt-layout",
        &gnu,
        &neo,
        16,
        3,
    );
    assert_fido_prompt_matches_stable_behavior(
        "fido_vertical_mode_mx_find_f_matches_gnu_then_cg/prompt",
        &gnu,
        &neo,
    );

    abort_minibuffer_and_wait_for_scratch(&mut gnu, &mut neo);
    assert_pair_nearly_matches(
        "fido_vertical_mode_mx_find_f_matches_gnu_then_cg/abort",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn fido_vertical_mode_mx_find_f_abort_then_repeat_matches_gnu() {
    let (mut gnu, mut neo) = boot_fido_vertical_pair();

    wait_for_fido_mx_candidates(&mut gnu, &mut neo, "find-f");
    assert_bottom_rows_nearly_match(
        "fido_vertical_mode_mx_find_f_abort_then_repeat_matches_gnu/first-prompt-layout",
        &gnu,
        &neo,
        16,
        3,
    );
    assert_fido_prompt_matches_stable_behavior(
        "fido_vertical_mode_mx_find_f_abort_then_repeat_matches_gnu/first-prompt",
        &gnu,
        &neo,
    );
    abort_minibuffer_and_wait_for_scratch(&mut gnu, &mut neo);
    assert_pair_nearly_matches(
        "fido_vertical_mode_mx_find_f_abort_then_repeat_matches_gnu/first-abort",
        &gnu,
        &neo,
        2,
    );

    wait_for_fido_mx_candidates(&mut gnu, &mut neo, "find-f");
    assert_bottom_rows_nearly_match(
        "fido_vertical_mode_mx_find_f_abort_then_repeat_matches_gnu/second-prompt-layout",
        &gnu,
        &neo,
        16,
        3,
    );
    assert_fido_prompt_matches_stable_behavior(
        "fido_vertical_mode_mx_find_f_abort_then_repeat_matches_gnu/second-prompt",
        &gnu,
        &neo,
    );
    abort_minibuffer_and_wait_for_scratch(&mut gnu, &mut neo);
    assert_pair_nearly_matches(
        "fido_vertical_mode_mx_find_f_abort_then_repeat_matches_gnu/second-abort",
        &gnu,
        &neo,
        2,
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
fn describe_mode_on_scratch_via_ch_m() {
    let (mut gnu, mut neo) = boot_pair("");
    send_help_sequence(&mut gnu, &mut neo, "m");
    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*Help*"))
            && grid.iter().any(|row| row.contains("Fundamental mode"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("describe_mode_on_scratch_via_ch_m", &gnu, &neo, 2);
}

#[test]
fn describe_mode_outline_heading_via_ch_m() {
    let (mut gnu, mut neo) = boot_pair("");
    send_help_sequence(&mut gnu, &mut neo, "m");
    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*Help*"))
            && grid
                .iter()
                .any(|row| row.contains("Major mode fundamental-mode"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("describe_mode_outline_heading_via_ch_m", &gnu, &neo, 2);
}

#[test]
fn quit_help_buffer_via_q() {
    let (mut gnu, mut neo) = boot_pair("");
    send_help_sequence(&mut gnu, &mut neo, "m");
    let help_ready = |grid: &[String]| grid.iter().any(|row| row.contains("*Help*"));
    gnu.read_until(Duration::from_secs(10), help_ready);
    neo.read_until(Duration::from_secs(20), help_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "q");
    let scratch_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*scratch*"))
            && grid
                .iter()
                .any(|row| row.contains("This buffer is for text that is not saved"))
    };
    gnu.read_until(Duration::from_secs(6), scratch_ready);
    neo.read_until(Duration::from_secs(8), scratch_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("quit_help_buffer_via_q", &gnu, &neo, 2);
}

#[test]
fn describe_key_find_file_via_chk() {
    let (mut gnu, mut neo) = boot_pair("");
    send_help_sequence(&mut gnu, &mut neo, "k");
    send_both(&mut gnu, &mut neo, "C-x C-f");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*Help*"))
            && grid.iter().any(|row| row.contains("find-file"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains("*Help*")),
            "{label} should show *Help* after C-h k"
        );
        assert!(
            grid.iter().any(|row| row.contains("find-file")),
            "{label} help buffer should mention find-file"
        );
        assert!(
            grid.iter().any(|row| row.contains("C-x C-f")),
            "{label} help buffer should mention C-x C-f"
        );
    }
}

#[test]
fn find_file_other_window_via_cx4_cf() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "other-window.txt",
        "window line 1\nwindow line 2\n",
        "C-x 4 C-f",
    );

    let ready = |grid: &[String]| {
        grid.iter().filter(|row| row.contains("*scratch*")).count() >= 1
            && grid.iter().any(|row| row.contains("other-window.txt"))
            && grid.iter().any(|row| row.contains("window line 1"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("find_file_other_window_via_cx4_cf", &gnu, &neo, 2);
}

#[test]
fn split_window_then_open_file_in_other_window_via_cx2_cxo_cx_cf() {
    let (mut gnu, mut neo) = boot_pair("");
    write_home_file(&gnu, "split-window.txt", "split line 1\nsplit line 2\n");
    write_home_file(&neo, "split-window.txt", "split line 1\nsplit line 2\n");

    send_both(&mut gnu, &mut neo, "C-x 2");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-x o");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x C-f");
    let minibuffer_path = "~/split-window.txt";
    gnu.send(minibuffer_path.as_bytes());
    neo.send(minibuffer_path.as_bytes());
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("split line 1"))
            && grid.iter().any(|row| row.contains("split-window.txt"))
            && grid.iter().any(|row| row.contains("*scratch*"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "split_window_then_open_file_in_other_window_via_cx2_cxo_cx_cf",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn other_window_via_cxo() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "other-window-hop.txt",
        "window body\n",
        "C-x 2 C-x o C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-a");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"BOTTOM ");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x o");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "M-<");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"TOP ");
    }

    let ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("TOP ;; This buffer is for text that is not saved"))
            && grid.iter().any(|row| row.contains("BOTTOM window body"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("other_window_via_cxo", &gnu, &neo, 2);
}

#[test]
fn delete_other_windows_after_find_file_other_window_via_cx1() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "single-window.txt",
        "window collapse\n",
        "C-x 4 C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x 1");
    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("single-window.txt"))
            && grid.iter().any(|row| row.contains("window collapse"))
            && grid.iter().filter(|row| row.contains("*scratch*")).count() == 0
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "delete_other_windows_after_find_file_other_window_via_cx1",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn delete_selected_other_window_via_cx0() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "delete-window.txt",
        "delete me window\n",
        "C-x 2 C-x o C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x 0");
    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*scratch*"))
            && grid
                .iter()
                .any(|row| row.contains("This buffer is for text that is not saved"))
            && !grid.iter().any(|row| row.contains("delete-window.txt"))
            && !grid.iter().any(|row| row.contains("delete me window"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("delete_selected_other_window_via_cx0", &gnu, &neo, 2);
}

#[test]
fn write_file_after_edit_via_cx_cw() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "write-file-source.txt",
        "alpha line\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M->");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"omega line");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x C-w");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Write file:"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for session in [&mut gnu, &mut neo] {
        session.send(b"~/write-file-dest.txt");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("write-file-dest.txt"))
            && grid.iter().any(|row| row.contains("omega line"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    let expected_dest = "alpha line\nomega line\n";
    let gnu_dest = gnu.home_dir().join("write-file-dest.txt");
    let neo_dest = neo.home_dir().join("write-file-dest.txt");
    for _ in 0..10 {
        read_both(&mut gnu, &mut neo, Duration::from_millis(300));
        let gnu_saved = fs::read_to_string(&gnu_dest).ok().as_deref() == Some(expected_dest);
        let neo_saved = fs::read_to_string(&neo_dest).ok().as_deref() == Some(expected_dest);
        if gnu_saved && neo_saved {
            break;
        }
    }

    assert_eq!(
        fs::read_to_string(gnu.home_dir().join("write-file-source.txt"))
            .expect("read GNU source file"),
        "alpha line\n"
    );
    assert_eq!(
        fs::read_to_string(neo.home_dir().join("write-file-source.txt"))
            .expect("read Neo source file"),
        "alpha line\n"
    );
    assert_eq!(
        fs::read_to_string(&gnu_dest).expect("read GNU write-file dest"),
        expected_dest
    );
    assert_eq!(
        fs::read_to_string(&neo_dest).expect("read Neo write-file dest"),
        expected_dest
    );

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains("Wrote ")),
            "{label} screen missing save completion message:\n{}",
            grid.join("\n")
        );
        assert!(
            grid.iter().any(|row| row.contains("write-file-dest.txt")),
            "{label} screen missing destination file name after write-file:\n{}",
            grid.join("\n")
        );
    }

    send_both(&mut gnu, &mut neo, "C-l");
    let recentered = |grid: &[String]| {
        grid.iter().any(|row| row.contains("write-file-dest.txt"))
            && grid.iter().any(|row| row.contains("omega line"))
    };
    gnu.read_until(Duration::from_secs(6), recentered);
    neo.read_until(Duration::from_secs(8), recentered);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches("write_file_after_edit_via_cx_cw", &gnu, &neo, 2);
}

#[test]
fn save_buffer_after_edit_via_cx_cs() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "save-usage.txt",
        "alpha line\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M->");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"omega line");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x C-s");
    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("save-usage.txt"))
            && grid.iter().any(|row| row.contains("omega line"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_eq!(
        fs::read_to_string(gnu.home_dir().join("save-usage.txt")).expect("read GNU saved file"),
        "alpha line\nomega line\n"
    );
    assert_eq!(
        fs::read_to_string(neo.home_dir().join("save-usage.txt")).expect("read Neo saved file"),
        "alpha line\nomega line\n"
    );
    assert_pair_nearly_matches("save_buffer_after_edit_via_cx_cs", &gnu, &neo, 2);
}

#[test]
fn save_some_buffers_after_edit_via_cx_s() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "save-some-usage.txt",
        "alpha line\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M->");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"omega line");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x s");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Save file"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "SPC");
    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("save-some-usage.txt"))
            && grid.iter().any(|row| row.contains("omega line"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    let expected = "alpha line\nomega line\n";
    let gnu_path = gnu.home_dir().join("save-some-usage.txt");
    let neo_path = neo.home_dir().join("save-some-usage.txt");
    for _ in 0..10 {
        read_both(&mut gnu, &mut neo, Duration::from_millis(300));
        let gnu_saved = fs::read_to_string(&gnu_path).ok().as_deref() == Some(expected);
        let neo_saved = fs::read_to_string(&neo_path).ok().as_deref() == Some(expected);
        if gnu_saved && neo_saved {
            break;
        }
    }

    let gnu_saved = fs::read_to_string(&gnu_path).expect("read GNU save-some file");
    let neo_saved = fs::read_to_string(&neo_path).expect("read Neo save-some file");
    if gnu_saved != expected || neo_saved != expected {
        eprintln!(
            "save_some_buffers_after_edit_via_cx_s: GNU file = {:?}",
            gnu_saved
        );
        eprintln!(
            "save_some_buffers_after_edit_via_cx_s: NEO file = {:?}",
            neo_saved
        );
        dump_pair_grids("save_some_buffers_after_edit_via_cx_s", &gnu, &neo);
    }
    assert_eq!(gnu_saved, expected);
    assert_eq!(neo_saved, expected);
    assert_pair_nearly_matches("save_some_buffers_after_edit_via_cx_s", &gnu, &neo, 2);
}

#[test]
fn kill_buffer_after_find_file_via_cx_k() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "kill-buffer.txt",
        "temporary buffer\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x k");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*scratch*"))
            && !grid.iter().any(|row| row.contains("kill-buffer.txt"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("kill_buffer_after_find_file_via_cx_k", &gnu, &neo, 2);
}

#[test]
fn isearch_forward_via_cs() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "search-usage.txt",
        "alpha line\nbeta target\nomega line\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-s");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"target");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("search-usage.txt"))
            && grid.iter().any(|row| row.contains("beta target"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("isearch_forward_via_cs", &gnu, &neo, 2);
}

#[test]
fn isearch_backward_via_cr() {
    let (mut gnu, mut neo) = boot_pair("");
    let mut contents = String::new();
    for line in 1..=40 {
        if line == 5 {
            contents.push_str("needle target\n");
        } else {
            contents.push_str(&format!("filler line {line:02}\n"));
        }
    }
    open_home_file(
        &mut gnu,
        &mut neo,
        "reverse-search.txt",
        &contents,
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M->");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-r");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"needle");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| grid.iter().any(|row| row.contains("needle target"));
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("isearch_backward_via_cr", &gnu, &neo, 2);
}

#[test]
fn kill_region_and_yank_via_cw_cy() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "cut-yank.txt",
        "alpha line\nbeta line\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-@");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-e");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-w");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-y");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alpha line"))
            && grid.iter().any(|row| row.contains("beta line"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("kill_region_and_yank_via_cw_cy", &gnu, &neo, 2);
}

#[test]
fn undo_edit_via_cx_u() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "undo-usage.txt",
        "alpha line\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M->");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"omega line");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-x u");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("undo-usage.txt"))
            && grid.iter().any(|row| row.contains("alpha line"))
            && !grid.iter().any(|row| row.contains("omega line"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("undo_edit_via_cx_u", &gnu, &neo, 2);
}

#[test]
fn scroll_page_down_and_up_via_cv_mv() {
    let (mut gnu, mut neo) = boot_pair("");
    let mut contents = String::new();
    for line in 1..=80 {
        contents.push_str(&format!("scroll line {line:02}\n"));
    }
    open_home_file(&mut gnu, &mut neo, "scroll-usage.txt", &contents, "C-x C-f");

    send_both(&mut gnu, &mut neo, "C-v");
    let paged = |grid: &[String]| grid.iter().any(|row| row.contains("scroll line 20"));
    gnu.read_until(Duration::from_secs(6), paged);
    neo.read_until(Duration::from_secs(8), paged);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "M-v");
    let returned = |grid: &[String]| grid.iter().any(|row| row.contains("scroll line 01"));
    gnu.read_until(Duration::from_secs(6), returned);
    neo.read_until(Duration::from_secs(8), returned);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("scroll_page_down_and_up_via_cv_mv", &gnu, &neo, 2);
}

#[test]
fn goto_buffer_end_and_beginning_via_mgt_mlt() {
    let (mut gnu, mut neo) = boot_pair("");
    let mut contents = String::new();
    for line in 1..=60 {
        contents.push_str(&format!("edge line {line:02}\n"));
    }
    open_home_file(&mut gnu, &mut neo, "edge-usage.txt", &contents, "C-x C-f");

    send_both(&mut gnu, &mut neo, "M->");
    let at_end = |grid: &[String]| grid.iter().any(|row| row.contains("edge line 60"));
    gnu.read_until(Duration::from_secs(6), at_end);
    neo.read_until(Duration::from_secs(8), at_end);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "M-<");
    let at_start = |grid: &[String]| grid.iter().any(|row| row.contains("edge line 01"));
    gnu.read_until(Duration::from_secs(6), at_start);
    neo.read_until(Duration::from_secs(8), at_start);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("goto_buffer_end_and_beginning_via_mgt_mlt", &gnu, &neo, 2);
}

#[test]
fn move_beginning_and_end_of_line_via_ca_ce() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "line-motion.txt",
        "alpha beta gamma\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-e");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b" END");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-a");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"BEGIN ");
    }

    let ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("BEGIN alpha beta gamma END"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("move_beginning_and_end_of_line_via_ca_ce", &gnu, &neo, 2);
}

#[test]
fn delete_char_and_delete_backward_char_via_cd_del() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(&mut gnu, &mut neo, "delete-char.txt", "alpha\n", "C-x C-f");

    send_both(&mut gnu, &mut neo, "C-f C-f");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-d");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "DEL");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("aha")) && !grid.iter().any(|row| row.contains("alpha"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "delete_char_and_delete_backward_char_via_cd_del",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn next_and_previous_line_via_cn_cp() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "line-step.txt",
        "line one\nline two\nline three\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-n C-n C-a");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"THREE ");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-p C-a");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"TWO ");
    }

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("line one"))
            && grid.iter().any(|row| row.contains("TWO line two"))
            && grid.iter().any(|row| row.contains("THREE line three"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("next_and_previous_line_via_cn_cp", &gnu, &neo, 2);
}

#[test]
fn forward_and_backward_char_via_cf_cb() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(&mut gnu, &mut neo, "char-motion.txt", "alpha\n", "C-x C-f");

    send_both(&mut gnu, &mut neo, "C-f C-f");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"X");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-b C-b");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"Y");
    }

    let ready = |grid: &[String]| grid.iter().any(|row| row.contains("aYlXpha"));
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("forward_and_backward_char_via_cf_cb", &gnu, &neo, 2);
}

#[test]
fn transpose_chars_via_ct() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "transpose-chars.txt",
        "acb\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-f C-f");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-t");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("abc")) && !grid.iter().any(|row| row.contains("acb"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("transpose_chars_via_ct", &gnu, &neo, 2);
}

#[test]
fn forward_and_backward_word_via_mf_mb() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "word-motion.txt",
        "alpha beta gamma\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-f M-f");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b" END");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "M-b M-b");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"MID ");
    }

    let ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("alpha MID beta END gamma"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("forward_and_backward_word_via_mf_mb", &gnu, &neo, 2);
}

#[test]
fn backward_kill_word_via_esc_del() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "backward-kill-word.txt",
        "alpha beta gamma\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-f M-f C-f");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, &[0x1b, 0x7f]);

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alpha gamma"))
            && !grid.iter().any(|row| row.contains("alpha beta gamma"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("backward_kill_word_via_esc_del", &gnu, &neo, 2);
}

#[test]
fn forward_and_backward_sentence_via_me_ma() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "sentence-motion.txt",
        "Alpha one. Beta two. Gamma three.\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-e");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"[[E]]");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "M-a");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"[[A]]");
    }

    let ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("[[A]]") && row.contains("[[E]]"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("forward_and_backward_sentence_via_me_ma", &gnu, &neo, 2);
}

#[test]
fn open_line_via_co() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "open-line.txt",
        "beta gamma\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-o");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"alpha");
    }

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alpha"))
            && grid.iter().any(|row| row.contains("beta gamma"))
            && !grid.iter().any(|row| row.contains("alphabeta"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("open_line_via_co", &gnu, &neo, 2);
}

#[test]
fn newline_via_cm() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "newline-usage.txt",
        "alpha gamma\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-f");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-m");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"beta");
    }

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alpha"))
            && grid.iter().any(|row| row.contains("beta gamma"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("newline_via_cm", &gnu, &neo, 2);
}

#[test]
fn set_fill_column_then_fill_paragraph_via_cx_f_mq() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "fill-paragraph.txt",
        "alpha beta gamma delta epsilon zeta eta theta iota kappa\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x f");
    let prompt_ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Change fill-column from"))
    };
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "set_fill_column_then_fill_paragraph_via_cx_f_mq/prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"20");
    }
    send_both(&mut gnu, &mut neo, "RET");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "M-q");
    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alpha beta gamma"))
            && grid.iter().any(|row| row.contains("delta epsilon zeta"))
            && grid.iter().any(|row| row.contains("eta theta iota kappa"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "set_fill_column_then_fill_paragraph_via_cx_f_mq",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn recenter_top_bottom_cycle_via_cl() {
    let (mut gnu, mut neo) = boot_pair("");
    let mut contents = String::new();
    for line in 1..=80 {
        contents.push_str(&format!("recenter line {line:02}\n"));
    }
    open_home_file(
        &mut gnu,
        &mut neo,
        "recenter-usage.txt",
        &contents,
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-s");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"recenter line 40");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "RET");

    let found = |grid: &[String]| grid.iter().any(|row| row.contains("recenter line 40"));
    gnu.read_until(Duration::from_secs(6), found);
    neo.read_until(Duration::from_secs(8), found);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-l");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches("recenter_top_bottom_cycle_via_cl/middle", &gnu, &neo, 2);

    send_both(&mut gnu, &mut neo, "C-l");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches("recenter_top_bottom_cycle_via_cl/top", &gnu, &neo, 2);

    send_both(&mut gnu, &mut neo, "C-l");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches("recenter_top_bottom_cycle_via_cl/bottom", &gnu, &neo, 2);
}

#[test]
fn scroll_other_window_via_cmv() {
    let (mut gnu, mut neo) = boot_pair("");
    let mut contents = String::new();
    for line in 1..=80 {
        contents.push_str(&format!("other scroll {line:02}\n"));
    }
    write_home_file(&gnu, "other-scroll.txt", &contents);
    write_home_file(&neo, "other-scroll.txt", &contents);

    send_both(&mut gnu, &mut neo, "C-x 2");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-x o");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-x C-f");
    let minibuffer_path = "~/other-scroll.txt";
    gnu.send(minibuffer_path.as_bytes());
    neo.send(minibuffer_path.as_bytes());
    send_both(&mut gnu, &mut neo, "RET");

    let opened = |grid: &[String]| {
        grid.iter().any(|row| row.contains("other scroll 01"))
            && grid.iter().any(|row| row.contains("*scratch*"))
    };
    gnu.read_until(Duration::from_secs(6), opened);
    neo.read_until(Duration::from_secs(8), opened);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x o");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-M-v");

    let scrolled = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*scratch*"))
            && grid.iter().any(|row| row.contains("other scroll 20"))
    };
    gnu.read_until(Duration::from_secs(6), scrolled);
    neo.read_until(Duration::from_secs(8), scrolled);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("scroll_other_window_via_cmv", &gnu, &neo, 2);
}

#[test]
fn kill_sentence_via_mk() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "kill-sentence.txt",
        "Alpha one. Beta two.\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-k");
    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("Beta two."))
            && !grid.iter().any(|row| row.contains("Alpha one."))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("kill_sentence_via_mk", &gnu, &neo, 2);
}

#[test]
fn kill_ring_save_region_then_yank_via_mw_cy() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "kill-ring-save.txt",
        "alpha beta\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-@");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "M-f");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "M-w");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-e");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "SPC C-y");

    let ready = |grid: &[String]| grid.iter().any(|row| row.contains("alpha beta alpha"));
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("kill_ring_save_region_then_yank_via_mw_cy", &gnu, &neo, 2);
}

#[test]
fn kill_word_via_md() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "kill-word.txt",
        "alpha beta gamma\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-f");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "SPC");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "M-d");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alpha gamma"))
            && !grid.iter().any(|row| row.contains("alpha beta gamma"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("kill_word_via_md", &gnu, &neo, 2);
}

#[test]
fn kill_line_twice_then_yank_via_ck_ck_cy() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "kill-line.txt",
        "alpha line\nbeta line\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-k");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-k");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-y");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alpha line"))
            && grid.iter().any(|row| row.contains("beta line"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("kill_line_twice_then_yank_via_ck_ck_cy", &gnu, &neo, 2);
}

#[test]
fn yank_pop_after_yank_via_my() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "yank-pop.txt",
        "alpha line\nbeta line\ngamma line\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-k");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-n");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-k");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-y");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "M-y");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("beta line"))
            && grid.iter().any(|row| row.contains("alpha line"))
            && !grid.iter().any(|row| row.contains("gamma line"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("yank_pop_after_yank_via_my", &gnu, &neo, 2);
}

#[test]
fn describe_bindings_via_ch_b() {
    let (mut gnu, mut neo) = boot_pair("");
    send_help_sequence(&mut gnu, &mut neo, "b");
    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*Help*"))
            && grid.iter().any(|row| {
                row.contains("Key translations")
                    || row.contains("Major Mode Bindings")
                    || row.contains("lisp-interaction-mode")
            })
    };
    gnu.read_until(Duration::from_secs(15), ready);
    neo.read_until(Duration::from_secs(30), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains("*Help*")),
            "{label} should show *Help* after C-h b\n{}",
            grid.join("\n")
        );
        assert!(
            grid.iter().any(|row| row.contains("Key translations")
                || row.contains("Major Mode Bindings")
                || row.contains("lisp-interaction-mode")),
            "{label} describe-bindings should show a GNU-visible heading\n{}",
            grid.join("\n")
        );
    }
}

#[test]
fn quit_describe_bindings_via_q() {
    let (mut gnu, mut neo) = boot_pair("");
    send_help_sequence(&mut gnu, &mut neo, "b");
    let help_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*Help*"))
            && grid.iter().any(|row| {
                row.contains("Key translations")
                    || row.contains("Major Mode Bindings")
                    || row.contains("lisp-interaction-mode")
            })
    };
    gnu.read_until(Duration::from_secs(15), help_ready);
    neo.read_until(Duration::from_secs(30), help_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "q");
    let scratch_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*scratch*"))
            && grid
                .iter()
                .any(|row| row.contains("This buffer is for text that is not saved"))
    };
    gnu.read_until(Duration::from_secs(6), scratch_ready);
    neo.read_until(Duration::from_secs(8), scratch_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains("*scratch*")),
            "{label} should return to *scratch* after q"
        );
        assert!(
            grid.iter()
                .any(|row| row.contains("This buffer is for text that is not saved")),
            "{label} should show the scratch buffer contents after q"
        );
    }
}

#[test]
fn describe_function_find_file_via_ch_f() {
    let (mut gnu, mut neo) = boot_pair("");
    send_help_sequence(&mut gnu, &mut neo, "f");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Describe function"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"find-file");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*Help*"))
            && grid.iter().any(|row| row.contains("find-file is"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains("*Help*")),
            "{label} should show *Help* after C-h f"
        );
        assert!(
            grid.iter().any(|row| row.contains("find-file is")),
            "{label} describe-function should mention find-file"
        );
        assert!(
            grid.iter().any(|row| row.contains("C-x C-f")),
            "{label} describe-function should mention C-x C-f"
        );
    }
}

#[test]
fn describe_key_briefly_find_file_via_ch_c() {
    let (mut gnu, mut neo) = boot_pair("");
    send_help_sequence(&mut gnu, &mut neo, "c");
    send_both(&mut gnu, &mut neo, "C-x C-f");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("C-x C-f"))
            && grid.iter().any(|row| row.contains("find-file"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains("C-x C-f")),
            "{label} should show the described key after C-h c"
        );
        assert!(
            grid.iter().any(|row| row.contains("find-file")),
            "{label} describe-key-briefly should mention find-file"
        );
    }
}

#[test]
fn query_replace_via_mpercent_bang() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "query-replace.txt",
        "alpha one\nalpha two\nalpha three\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-%");
    let from_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Query replace"));
    gnu.read_until(Duration::from_secs(6), from_ready);
    neo.read_until(Duration::from_secs(8), from_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for session in [&mut gnu, &mut neo] {
        session.send(b"alpha");
    }
    send_both(&mut gnu, &mut neo, "RET");
    let to_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("with:")) || grid.iter().any(|row| row.contains("with "))
    };
    gnu.read_until(Duration::from_secs(6), to_ready);
    neo.read_until(Duration::from_secs(8), to_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for session in [&mut gnu, &mut neo] {
        session.send(b"omega");
    }
    send_both(&mut gnu, &mut neo, "RET");
    let query_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("Query replacing"))
            && grid.iter().any(|row| row.contains("(y/n/!/q/?)"))
    };
    gnu.read_until(Duration::from_secs(6), query_ready);
    neo.read_until(Duration::from_secs(8), query_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "!");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("omega one"))
            && grid.iter().any(|row| row.contains("omega two"))
            && grid.iter().any(|row| row.contains("omega three"))
            && !grid.iter().any(|row| row.contains("alpha one"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("query_replace_via_mpercent_bang", &gnu, &neo, 2);
}

#[test]
fn upcase_word_via_mu() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "upcase-word.txt",
        "alpha beta gamma\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-u");
    let ready = |grid: &[String]| grid.iter().any(|row| row.contains("ALPHA beta gamma"));
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("upcase_word_via_mu", &gnu, &neo, 2);
}

#[test]
fn downcase_word_via_ml() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "downcase-word.txt",
        "ALPHA beta gamma\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-l");
    let ready = |grid: &[String]| grid.iter().any(|row| row.contains("alpha beta gamma"));
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("downcase_word_via_ml", &gnu, &neo, 2);
}

#[test]
fn capitalize_word_via_mc() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "capitalize-word.txt",
        "alpha beta gamma\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-c");
    let ready = |grid: &[String]| grid.iter().any(|row| row.contains("Alpha beta gamma"));
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("capitalize_word_via_mc", &gnu, &neo, 2);
}

#[test]
fn exchange_point_and_mark_via_cx_cx() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "exchange-point-and-mark.txt",
        "alpha beta gamma\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-f");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-@");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "M-f M-f");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-x C-x");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"X");

    let ready = |grid: &[String]| grid.iter().any(|row| row.contains("alphaX beta gamma"));
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("exchange_point_and_mark_via_cx_cx", &gnu, &neo, 2);
}

#[test]
fn mark_paragraph_then_kill_and_yank_via_mh_cw_cy() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "mark-paragraph.txt",
        "alpha one\nalpha two\n\nbeta one\nbeta two\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-h");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-w");

    let killed = |grid: &[String]| {
        grid.iter().any(|row| row.contains("beta one"))
            && grid.iter().any(|row| row.contains("beta two"))
            && !grid.iter().any(|row| row.contains("alpha one"))
            && !grid.iter().any(|row| row.contains("alpha two"))
    };
    gnu.read_until(Duration::from_secs(6), killed);
    neo.read_until(Duration::from_secs(8), killed);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-y");
    let restored = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alpha one"))
            && grid.iter().any(|row| row.contains("alpha two"))
            && grid.iter().any(|row| row.contains("beta one"))
            && grid.iter().any(|row| row.contains("beta two"))
    };
    gnu.read_until(Duration::from_secs(6), restored);
    neo.read_until(Duration::from_secs(8), restored);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "mark_paragraph_then_kill_and_yank_via_mh_cw_cy",
        &gnu,
        &neo,
        2,
    );
}
