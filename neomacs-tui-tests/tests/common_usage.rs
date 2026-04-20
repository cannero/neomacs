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

fn send_both(gnu: &mut TuiSession, neo: &mut TuiSession, keys: &str) {
    gnu.send_keys(keys);
    neo.send_keys(keys);
}

fn read_both(gnu: &mut TuiSession, neo: &mut TuiSession, timeout: Duration) {
    gnu.read(timeout);
    neo.read(timeout);
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
