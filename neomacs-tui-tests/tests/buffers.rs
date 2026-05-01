//! TUI comparison tests: buffers.

mod support;
use neomacs_tui_tests::*;
use std::fs;
use std::time::Duration;
use support::*;

// ── Tests ──────────────────────────────────────────────────
#[test]
fn switch_buffer_via_cx_b_visits_existing_file_buffer() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "switch-alpha.txt",
        "alpha buffer body\n",
        "C-x C-f",
    );
    open_home_file(
        &mut gnu,
        &mut neo,
        "switch-beta.txt",
        "beta buffer body\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x b");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Switch to buffer:"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "switch_buffer_via_cx_b_visits_existing_file_buffer/prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"switch-alpha.txt");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let alpha_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("switch-alpha.txt"))
            && grid.iter().any(|row| row.contains("alpha buffer body"))
            && !grid.iter().any(|row| row.contains("beta buffer body"))
    };
    gnu.read_until(Duration::from_secs(6), alpha_ready);
    neo.read_until(Duration::from_secs(8), alpha_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "switch_buffer_via_cx_b_visits_existing_file_buffer",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn rename_buffer_via_cx_x_r_updates_current_buffer_name() {
    let (mut gnu, mut neo) = boot_pair("");

    send_both(&mut gnu, &mut neo, "C-x x r");
    let prompt_ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Rename buffer") && row.contains("to new name"))
    };
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "rename_buffer_via_cx_x_r_updates_current_buffer_name/prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"renamed-scratch");
    }
    send_both(&mut gnu, &mut neo, "RET");
    let renamed_ready = |grid: &[String]| grid.iter().any(|row| row.contains("renamed-scratch"));
    gnu.read_until(Duration::from_secs(6), renamed_ready);
    neo.read_until(Duration::from_secs(8), renamed_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "rename_buffer_via_cx_x_r_updates_current_buffer_name/renamed",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "C-x C-b");
    let list_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*Buffer List*"))
            && grid.iter().any(|row| row.contains("renamed-scratch"))
    };
    gnu.read_until(Duration::from_secs(6), list_ready);
    neo.read_until(Duration::from_secs(8), list_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "rename_buffer_via_cx_x_r_updates_current_buffer_name/list-buffers",
        &gnu,
        &neo,
        2,
    );
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
fn buffer_menu_search_and_select_file_buffer_via_ret() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "buffer-menu-select.txt",
        "selected buffer body\n",
        "C-x C-f",
    );

    invoke_mx_command(&mut gnu, &mut neo, "buffer-menu");
    let menu_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*Buffer List*"))
            && grid.iter().any(|row| row.contains("Buffer Menu"))
            && grid
                .iter()
                .any(|row| row.contains("buffer-menu-select.txt"))
    };
    gnu.read_until(Duration::from_secs(6), menu_ready);
    neo.read_until(Duration::from_secs(8), menu_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-s");
    for session in [&mut gnu, &mut neo] {
        session.send(b"buffer-menu-select.txt");
    }
    let search_ready = |grid: &[String]| {
        grid.last()
            .is_some_and(|row| row.contains("I-search") && row.contains("buffer-menu-select.txt"))
    };
    gnu.read_until(Duration::from_secs(6), search_ready);
    neo.read_until(Duration::from_secs(8), search_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));

    send_both(&mut gnu, &mut neo, "C-g RET");
    let selected = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("buffer-menu-select.txt"))
            && grid.iter().any(|row| row.contains("selected buffer body"))
            && !grid.iter().any(|row| row.contains("*Buffer List*"))
    };
    gnu.read_until(Duration::from_secs(6), selected);
    neo.read_until(Duration::from_secs(8), selected);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "buffer_menu_search_and_select_file_buffer_via_ret",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn clone_indirect_buffer_other_window_via_cx4_c() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "clone-indirect.txt",
        "alpha line\nbeta line\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x 4 c");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("clone-indirect.txt<2>"))
            && grid.iter().filter(|row| row.contains("alpha line")).count() >= 2
    };
    gnu.read_until(Duration::from_secs(8), ready);
    neo.read_until(Duration::from_secs(12), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "clone_indirect_buffer_other_window_via_cx4_c",
        &gnu,
        &neo,
        3,
    );
}

#[test]
fn ibuffer_via_mx_lists_file_buffer_and_q_quits() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "ibuffer-usage.txt",
        "alpha line\nbeta line\n",
        "C-x C-f",
    );

    invoke_mx_command(&mut gnu, &mut neo, "ibuffer");
    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*Ibuffer*"))
            && grid.iter().any(|row| row.contains("ibuffer-usage.txt"))
            && grid.iter().any(|row| row.contains("Commands:"))
    };
    gnu.read_until(Duration::from_secs(8), ready);
    neo.read_until(Duration::from_secs(10), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    if !ready(&gnu.text_grid()) || !ready(&neo.text_grid()) {
        dump_pair_grids("ibuffer_via_mx_lists_file_buffer_and_q_quits", &gnu, &neo);
    }
    assert_pair_nearly_matches(
        "ibuffer_via_mx_lists_file_buffer_and_q_quits/list",
        &gnu,
        &neo,
        5,
    );

    send_both(&mut gnu, &mut neo, "q");
    let quit_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("ibuffer-usage.txt"))
            && grid.iter().any(|row| row.contains("alpha line"))
            && !grid.iter().any(|row| row.contains("*Ibuffer*"))
    };
    gnu.read_until(Duration::from_secs(6), quit_ready);
    neo.read_until(Duration::from_secs(8), quit_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "ibuffer_via_mx_lists_file_buffer_and_q_quits/quit",
        &gnu,
        &neo,
        2,
    );
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
fn view_echo_area_messages_via_ch_e_shows_messages_buffer_tail() {
    let (mut gnu, mut neo) = boot_pair("");

    send_both(&mut gnu, &mut neo, "M-:");
    read_both(&mut gnu, &mut neo, Duration::from_secs(2));
    for session in [&mut gnu, &mut neo] {
        session.send(br#"(message "common usage echo log")"#);
    }
    send_both(&mut gnu, &mut neo, "RET");
    read_both(&mut gnu, &mut neo, Duration::from_secs(2));

    send_help_sequence(&mut gnu, &mut neo, "e");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*Messages*"))
            && grid.iter().any(|row| row.contains("common usage echo log"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    if !ready(&gnu.text_grid()) || !ready(&neo.text_grid()) {
        dump_pair_grids(
            "view_echo_area_messages_via_ch_e_shows_messages_buffer_tail/not-ready",
            &gnu,
            &neo,
        );
    }

    assert_pair_nearly_matches(
        "view_echo_area_messages_via_ch_e_shows_messages_buffer_tail",
        &gnu,
        &neo,
        3,
    );
}

#[test]
fn switch_to_file_buffer_via_cx_b_restores_existing_buffer() {
    let (mut gnu, mut neo) = boot_pair("");

    open_home_file(
        &mut gnu,
        &mut neo,
        "switch-alpha.txt",
        "alpha first\n",
        "C-x C-f",
    );
    open_home_file(
        &mut gnu,
        &mut neo,
        "switch-beta.txt",
        "beta second\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x b");
    let prompt_ready = |grid: &[String]| {
        grid.last()
            .is_some_and(|row| row.contains("Switch to buffer:"))
    };
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    for session in [&mut gnu, &mut neo] {
        session.send(b"switch-alpha.txt");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("switch-alpha.txt"))
            && grid.iter().any(|row| row.contains("alpha first"))
            && !grid.iter().any(|row| row.contains("beta second"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "switch_to_file_buffer_via_cx_b_restores_existing_buffer",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn switch_to_buffer_tab_completion_via_cx_b_completes_existing_buffer() {
    let (mut gnu, mut neo) = boot_pair("");

    open_home_file(
        &mut gnu,
        &mut neo,
        "buffer-completion-target.txt",
        "buffer completion body\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x b");
    let switch_prompt = |grid: &[String]| grid.iter().any(|row| row.contains("Switch to buffer:"));
    gnu.read_until(Duration::from_secs(6), switch_prompt);
    neo.read_until(Duration::from_secs(8), switch_prompt);
    for session in [&mut gnu, &mut neo] {
        session.send(b"*scratch*");
    }
    send_both(&mut gnu, &mut neo, "RET");
    gnu.read_until(Duration::from_secs(6), scratch_ready);
    neo.read_until(Duration::from_secs(8), scratch_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x b");
    gnu.read_until(Duration::from_secs(6), switch_prompt);
    neo.read_until(Duration::from_secs(8), switch_prompt);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "switch_to_buffer_tab_completion_via_cx_b_completes_existing_buffer/prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"buffer-completion-tar");
    }
    send_both(&mut gnu, &mut neo, "TAB");
    let completed = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("buffer-completion-target.txt"))
    };
    gnu.read_until(Duration::from_secs(6), completed);
    neo.read_until(Duration::from_secs(8), completed);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "switch_to_buffer_tab_completion_via_cx_b_completes_existing_buffer/completed",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "RET");
    let ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("buffer-completion-target.txt"))
            && grid
                .iter()
                .any(|row| row.contains("buffer completion body"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "switch_to_buffer_tab_completion_via_cx_b_completes_existing_buffer",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn previous_and_next_buffer_via_mx_cycle_recent_file_buffers() {
    let (mut gnu, mut neo) = boot_pair("");

    open_home_file(
        &mut gnu,
        &mut neo,
        "cycle-alpha.txt",
        "alpha cycle\n",
        "C-x C-f",
    );
    open_home_file(
        &mut gnu,
        &mut neo,
        "cycle-beta.txt",
        "beta cycle\n",
        "C-x C-f",
    );

    invoke_mx_command(&mut gnu, &mut neo, "previous-buffer");
    let alpha_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("cycle-alpha.txt"))
            && grid.iter().any(|row| row.contains("alpha cycle"))
            && !grid.iter().any(|row| row.contains("beta cycle"))
    };
    gnu.read_until(Duration::from_secs(6), alpha_ready);
    neo.read_until(Duration::from_secs(8), alpha_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "previous_and_next_buffer_via_mx_cycle_recent_file_buffers/previous",
        &gnu,
        &neo,
        3,
    );

    invoke_mx_command(&mut gnu, &mut neo, "next-buffer");
    let beta_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("cycle-beta.txt"))
            && grid.iter().any(|row| row.contains("beta cycle"))
            && !grid.iter().any(|row| row.contains("alpha cycle"))
    };
    gnu.read_until(Duration::from_secs(6), beta_ready);
    neo.read_until(Duration::from_secs(8), beta_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "previous_and_next_buffer_via_mx_cycle_recent_file_buffers/next",
        &gnu,
        &neo,
        3,
    );
}

#[test]
fn bury_and_unbury_buffer_via_mx_moves_current_buffer_to_end() {
    let (mut gnu, mut neo) = boot_pair("");

    open_home_file(
        &mut gnu,
        &mut neo,
        "bury-alpha.txt",
        "alpha bury\n",
        "C-x C-f",
    );
    open_home_file(
        &mut gnu,
        &mut neo,
        "bury-beta.txt",
        "beta bury\n",
        "C-x C-f",
    );

    invoke_mx_command(&mut gnu, &mut neo, "bury-buffer");
    let alpha_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("bury-alpha.txt"))
            && grid.iter().any(|row| row.contains("alpha bury"))
            && grid
                .get(usize::from(ROWS - 2))
                .is_some_and(|row| row.contains("F1  bury-alpha.txt"))
            && !grid.iter().any(|row| row.contains("beta bury"))
    };
    gnu.read_until(Duration::from_secs(6), alpha_ready);
    neo.read_until(Duration::from_secs(8), alpha_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            alpha_ready(&grid),
            "{label} should settle on the previous buffer after bury-buffer:\n{}",
            grid.join("\n")
        );
    }
    assert_pair_nearly_matches(
        "bury_and_unbury_buffer_via_mx_moves_current_buffer_to_end/buried",
        &gnu,
        &neo,
        3,
    );

    invoke_mx_command(&mut gnu, &mut neo, "unbury-buffer");
    let beta_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("bury-beta.txt"))
            && grid.iter().any(|row| row.contains("beta bury"))
            && grid
                .get(usize::from(ROWS - 2))
                .is_some_and(|row| row.contains("F1  bury-beta.txt"))
            && !grid.iter().any(|row| row.contains("alpha bury"))
    };
    gnu.read_until(Duration::from_secs(6), beta_ready);
    neo.read_until(Duration::from_secs(8), beta_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            beta_ready(&grid),
            "{label} should settle on the buried buffer after unbury-buffer:\n{}",
            grid.join("\n")
        );
    }
    assert_pair_nearly_matches(
        "bury_and_unbury_buffer_via_mx_moves_current_buffer_to_end",
        &gnu,
        &neo,
        3,
    );
}

#[test]
fn clone_buffer_via_mx_creates_independent_scratch_copy() {
    let (mut gnu, mut neo) = boot_pair("");

    send_both(&mut gnu, &mut neo, "M-<");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"original clone body");
    }
    let original_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("original clone body"))
            && grid
                .get(usize::from(ROWS - 2))
                .is_some_and(|row| row.contains("*scratch*"))
    };
    gnu.read_until(Duration::from_secs(6), original_ready);
    neo.read_until(Duration::from_secs(8), original_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    invoke_mx_command(&mut gnu, &mut neo, "clone-buffer");
    let clone_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("original clone body"))
            && grid
                .get(usize::from(ROWS - 2))
                .is_some_and(|row| row.contains("*scratch*<2>"))
    };
    gnu.read_until(Duration::from_secs(8), clone_ready);
    neo.read_until(Duration::from_secs(12), clone_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    if !clone_ready(&gnu.text_grid()) || !clone_ready(&neo.text_grid()) {
        dump_pair_grids(
            "clone_buffer_via_mx_creates_independent_scratch_copy/clone-ready",
            &gnu,
            &neo,
        );
    }

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            clone_ready(&grid),
            "{label} should show an independent cloned scratch buffer:\n{}",
            grid.join("\n")
        );
    }

    send_both(&mut gnu, &mut neo, "M-<");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"clone-only edit");
    }
    let clone_edited = |grid: &[String]| {
        grid.iter().any(|row| row.contains("clone-only edit"))
            && grid
                .get(usize::from(ROWS - 2))
                .is_some_and(|row| row.contains("*scratch*<2>"))
    };
    gnu.read_until(Duration::from_secs(6), clone_edited);
    neo.read_until(Duration::from_secs(8), clone_edited);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "M-:");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(br#"(format "clone-check %S/%S" (buffer-name) (with-current-buffer "*scratch*" (save-excursion (goto-char (point-min)) (search-forward "clone-only edit" nil t))))"#);
    }
    send_both(&mut gnu, &mut neo, "RET");

    let clone_check = |grid: &[String]| {
        grid.iter().any(|row| {
            row.contains("clone-check") && row.contains("*scratch*<2>") && row.contains("/nil")
        })
    };
    gnu.read_until(Duration::from_secs(8), clone_check);
    neo.read_until(Duration::from_secs(12), clone_check);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            clone_check(&grid),
            "{label} should keep clone edits out of the original scratch buffer:\n{}",
            grid.join("\n")
        );
    }
    assert_pair_nearly_matches(
        "clone_buffer_via_mx_creates_independent_scratch_copy",
        &gnu,
        &neo,
        3,
    );
}

#[test]
fn switch_to_buffer_other_window_via_cx4_b_displays_messages() {
    let (mut gnu, mut neo) = boot_pair("");

    send_both(&mut gnu, &mut neo, "M-:");
    read_both(&mut gnu, &mut neo, Duration::from_secs(2));
    for session in [&mut gnu, &mut neo] {
        session.send(br#"(message "other window buffer switch")"#);
    }
    send_both(&mut gnu, &mut neo, "RET");
    read_both(&mut gnu, &mut neo, Duration::from_secs(2));

    send_both(&mut gnu, &mut neo, "C-x 4 b");
    let prompt_ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Switch to buffer in other window"))
    };
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    let expected_prompt = "Switch to buffer in other window (default *Messages*): ";
    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains(expected_prompt)),
            "{label} should show read-buffer's default in the prompt\n{}",
            grid.join("\n")
        );
    }
    assert_pair_nearly_matches(
        "switch_to_buffer_other_window_via_cx4_b_displays_messages/prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"*Messages*");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*scratch*"))
            && grid.iter().any(|row| row.contains("*Messages*"))
            && grid
                .iter()
                .any(|row| row.contains("other window buffer switch"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "switch_to_buffer_other_window_via_cx4_b_displays_messages",
        &gnu,
        &neo,
        2,
    );
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
