//! TUI comparison tests: windows tabs.

mod support;
use neomacs_tui_tests::*;
use std::fs;
use std::time::Duration;
use support::*;

// ── Local helpers ───────────────────────────────────────────

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

// ── Tests ──────────────────────────────────────────────────
#[test]
fn kill_buffer_and_window_via_cx4_0_restores_single_window() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "kill-buffer-window.txt",
        "temporary other-window file\n",
        "C-x 4 C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x 4 0");
    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*scratch*"))
            && !grid
                .iter()
                .any(|row| row.contains("kill-buffer-window.txt"))
            && !grid
                .iter()
                .any(|row| row.contains("temporary other-window file"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "kill_buffer_and_window_via_cx4_0_restores_single_window",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn tab_bar_new_next_and_close_via_cx_t_prefix() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "tab-one.txt",
        "tab one body\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x t 2");
    let new_tab_ready = |grid: &[String]| grid.iter().any(|row| row.contains("*scratch*"));
    gnu.read_until(Duration::from_secs(6), new_tab_ready);
    neo.read_until(Duration::from_secs(8), new_tab_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    open_home_file(
        &mut gnu,
        &mut neo,
        "tab-two.txt",
        "tab two body\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x t o");
    let first_tab_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("tab-one.txt"))
            && grid.iter().any(|row| row.contains("tab one body"))
    };
    gnu.read_until(Duration::from_secs(6), first_tab_ready);
    neo.read_until(Duration::from_secs(8), first_tab_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x t o");
    let second_tab_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("tab-two.txt"))
            && grid.iter().any(|row| row.contains("tab two body"))
    };
    gnu.read_until(Duration::from_secs(6), second_tab_ready);
    neo.read_until(Duration::from_secs(8), second_tab_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x t 0");
    gnu.read_until(Duration::from_secs(6), first_tab_ready);
    neo.read_until(Duration::from_secs(8), first_tab_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("tab_bar_new_next_and_close_via_cx_t_prefix", &gnu, &neo, 2);
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
fn split_window_right_then_open_file_in_other_window_via_cx3_cxo_cx_cf() {
    let (mut gnu, mut neo) = boot_pair("");
    write_home_file(
        &gnu,
        "split-window-right.txt",
        "right split line 1\nright split line 2\n",
    );
    write_home_file(
        &neo,
        "split-window-right.txt",
        "right split line 1\nright split line 2\n",
    );

    send_both(&mut gnu, &mut neo, "C-x 3");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-x o");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x C-f");
    let minibuffer_path = "~/split-window-right.txt";
    gnu.send(minibuffer_path.as_bytes());
    neo.send(minibuffer_path.as_bytes());
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("right split line 1"))
            && grid
                .iter()
                .any(|row| row.contains("split-window-right.txt"))
            && grid.iter().any(|row| row.contains("*scratch*"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "split_window_right_then_open_file_in_other_window_via_cx3_cxo_cx_cf",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn compare_windows_via_mx_advances_both_points_to_first_difference() {
    let (mut gnu, mut neo) = boot_pair("");
    write_home_file(&gnu, "compare-left.txt", "same prefix\nleft side differs\n");
    write_home_file(&neo, "compare-left.txt", "same prefix\nleft side differs\n");
    write_home_file(
        &gnu,
        "compare-right.txt",
        "same prefix\nright side differs\n",
    );
    write_home_file(
        &neo,
        "compare-right.txt",
        "same prefix\nright side differs\n",
    );

    open_home_file(
        &mut gnu,
        &mut neo,
        "compare-left.txt",
        "same prefix\nleft side differs\n",
        "C-x C-f",
    );
    send_both(&mut gnu, &mut neo, "C-x 3 C-x o C-x C-f");
    for session in [&mut gnu, &mut neo] {
        session.send(b"~/compare-right.txt");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let both_files_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("left side differs"))
            && grid.iter().any(|row| row.contains("right side differs"))
    };
    gnu.read_until(Duration::from_secs(6), both_files_ready);
    neo.read_until(Duration::from_secs(8), both_files_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    invoke_mx_command(&mut gnu, &mut neo, "compare-windows");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "M-:");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(br#"(message "compare-points %S/%S" (point) (window-point (next-window)))"#);
    }
    send_both(&mut gnu, &mut neo, "RET");

    let point_ready = |grid: &[String]| grid.iter().any(|row| row.contains("compare-points 13/13"));
    gnu.read_until(Duration::from_secs(6), point_ready);
    neo.read_until(Duration::from_secs(8), point_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            point_ready(&grid),
            "{label} should leave both windows at first difference:\n{}",
            grid.join("\n")
        );
    }
    assert_pair_nearly_matches(
        "compare_windows_via_mx_advances_both_points_to_first_difference",
        &gnu,
        &neo,
        3,
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
fn enlarge_then_balance_windows_via_cx_caret_and_plus() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "resize-windows.txt",
        "top window\nbottom window\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x 2");
    let split_ready = |grid: &[String]| {
        grid.iter()
            .filter(|row| row.contains("resize-windows.txt"))
            .count()
            >= 2
    };
    gnu.read_until(Duration::from_secs(6), split_ready);
    neo.read_until(Duration::from_secs(8), split_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x ^");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "M-:");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(
            br#"(message "resize-window-taller %S" (> (window-total-height) (window-total-height (next-window))))"#,
        );
    }
    send_both(&mut gnu, &mut neo, "RET");

    let taller_ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("resize-window-taller t"))
    };
    gnu.read_until(Duration::from_secs(6), taller_ready);
    neo.read_until(Duration::from_secs(8), taller_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            taller_ready(&grid),
            "{label} should make selected window taller after C-x ^:\n{}",
            grid.join("\n")
        );
    }

    send_both(&mut gnu, &mut neo, "C-x +");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "M-:");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(
            br#"(message "resize-window-balanced %S" (<= (abs (- (window-total-height) (window-total-height (next-window)))) 1))"#,
        );
    }
    send_both(&mut gnu, &mut neo, "RET");

    let balanced_ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("resize-window-balanced t"))
    };
    gnu.read_until(Duration::from_secs(6), balanced_ready);
    neo.read_until(Duration::from_secs(8), balanced_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            balanced_ready(&grid),
            "{label} should balance split window heights after C-x +:\n{}",
            grid.join("\n")
        );
    }
    assert_pair_nearly_matches(
        "enlarge_then_balance_windows_via_cx_caret_and_plus",
        &gnu,
        &neo,
        3,
    );
}

#[test]
fn window_configuration_to_register_and_jump_via_cx_r_w_j() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "window-register.txt",
        "alpha window register\nbeta window register\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x 2");
    let split_ready = |grid: &[String]| {
        grid.iter()
            .filter(|row| row.contains("window-register.txt"))
            .count()
            >= 2
    };
    gnu.read_until(Duration::from_secs(6), split_ready);
    neo.read_until(Duration::from_secs(8), split_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x r w");
    let window_register_prompt = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Window configuration to register:"))
    };
    gnu.read_until(Duration::from_secs(6), window_register_prompt);
    neo.read_until(Duration::from_secs(8), window_register_prompt);
    for session in [&mut gnu, &mut neo] {
        session.send(b"a");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x 1");
    let single_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("window-register.txt"))
            && grid
                .iter()
                .filter(|row| row.contains("window-register.txt"))
                .count()
                == 1
    };
    gnu.read_until(Duration::from_secs(6), single_ready);
    neo.read_until(Duration::from_secs(8), single_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x r j");
    let jump_register_prompt =
        |grid: &[String]| grid.iter().any(|row| row.contains("Jump to register:"));
    gnu.read_until(Duration::from_secs(6), jump_register_prompt);
    neo.read_until(Duration::from_secs(8), jump_register_prompt);
    for session in [&mut gnu, &mut neo] {
        session.send(b"a");
    }

    gnu.read_until(Duration::from_secs(6), split_ready);
    neo.read_until(Duration::from_secs(8), split_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "window_configuration_to_register_and_jump_via_cx_r_w_j",
        &gnu,
        &neo,
        2,
    );
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
