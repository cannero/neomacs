//! TUI comparison tests: shell compile.

mod support;
use neomacs_tui_tests::*;
use std::fs;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use support::*;

// ── Tests ──────────────────────────────────────────────────
#[test]
fn shell_command_via_mbang_displays_short_output() {
    let (mut gnu, mut neo) = boot_pair("");

    send_both(&mut gnu, &mut neo, "M-!");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Shell command:"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "shell_command_via_mbang_displays_short_output/prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"printf tui-shell-ok");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| grid.iter().any(|row| row.contains("tui-shell-ok"));
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "shell_command_via_mbang_displays_short_output",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn shell_command_with_prefix_inserts_output_at_point_via_cu_mbang() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "shell-command-insert.txt",
        "start\nend\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-< C-e C-u M-!");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Shell command:"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "shell_command_with_prefix_inserts_output_at_point_via_cu_mbang/prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"printf shell-out");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("startshell-out"))
            && grid.iter().any(|row| row.contains("end"))
    };
    gnu.read_until(Duration::from_secs(8), ready);
    neo.read_until(Duration::from_secs(12), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "shell_command_with_prefix_inserts_output_at_point_via_cu_mbang",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn async_shell_command_via_mampersand_displays_output_buffer() {
    let (mut gnu, mut neo) = boot_pair("");

    send_both(&mut gnu, &mut neo, "M-&");
    let prompt_ready =
        |grid: &[String]| grid.iter().any(|row| row.contains("Async shell command:"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "async_shell_command_via_mampersand_displays_output_buffer/prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"printf tui-async-ok");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*Async Shell Command*"))
            && grid.iter().any(|row| row.contains("tui-async-ok"))
    };
    gnu.read_until(Duration::from_secs(8), ready);
    neo.read_until(Duration::from_secs(12), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            ready(&grid),
            "{label} should display async shell command output:\n{}",
            grid.join("\n")
        );
    }
    assert_pair_nearly_matches(
        "async_shell_command_via_mampersand_displays_output_buffer",
        &gnu,
        &neo,
        3,
    );
}

#[test]
fn shell_command_on_region_with_prefix_replaces_region_via_mbar() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "shell-command-region.txt",
        "alpha\nbeta\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x h C-u M-|");
    let prompt_ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Shell command on region:"))
    };
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "shell_command_on_region_with_prefix_replaces_region_via_mbar/prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"tr a-z A-Z");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("ALPHA"))
            && grid.iter().any(|row| row.contains("BETA"))
            && !grid.iter().any(|row| row.contains("alpha"))
            && !grid.iter().any(|row| row.contains("beta"))
    };
    gnu.read_until(Duration::from_secs(8), ready);
    neo.read_until(Duration::from_secs(12), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "shell_command_on_region_with_prefix_replaces_region_via_mbar",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn shell_command_on_region_without_prefix_displays_output_buffer_via_mbar() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "neomacs-shell-command-region-output-{}-{unique}.txt",
        std::process::id()
    ));
    fs::write(&path, "alpha\nbeta\n").expect("write shell command region fixture");
    let file_arg = path.display().to_string();
    let mut gnu = TuiSession::gnu_emacs(&file_arg);
    let mut neo = TuiSession::neomacs(&file_arg);
    let file_name = path
        .file_name()
        .expect("fixture file name")
        .to_string_lossy()
        .to_string();
    let file_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains(&file_name))
            && grid.iter().any(|row| row.contains("alpha"))
    };
    gnu.read_until(Duration::from_secs(10), file_ready);
    neo.read_until(Duration::from_secs(16), file_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));

    send_both(&mut gnu, &mut neo, "C-x h M-|");
    let prompt_ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Shell command on region:"))
    };
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "shell_command_on_region_without_prefix_displays_output_buffer_via_mbar/prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"tr a-z A-Z");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.trim_end() == "ALPHA")
            && grid.iter().any(|row| row.trim_end() == "BETA")
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));
    if !ready(&gnu.text_grid()) || !ready(&neo.text_grid()) {
        dump_pair_grids(
            "shell_command_on_region_without_prefix_displays_output_buffer_via_mbar/not-ready",
            &gnu,
            &neo,
        );
    }

    assert_pair_nearly_matches(
        "shell_command_on_region_without_prefix_displays_output_buffer_via_mbar",
        &gnu,
        &neo,
        3,
    );
    fs::remove_file(path).expect("remove shell command region fixture");
}

#[test]
fn shell_via_mx_runs_interactive_command_in_comint_buffer() {
    let init = std::env::temp_dir().join("neomacs-common-usage-shell.el");
    fs::write(
        &init,
        ";;; -*- lexical-binding: t; -*-\n\
         (setq explicit-shell-file-name \"/bin/sh\"\n\
               explicit-sh-args '(\"-i\"))\n\
         (setenv \"ENV\" nil)\n\
         (setenv \"BASH_ENV\" nil)\n\
         (setenv \"PS1\" \"tui-sh> \")\n",
    )
    .expect("write shell init file");
    let extra_args = format!("-l {}", init.display());
    let (mut gnu, mut neo) = boot_pair(&extra_args);

    invoke_mx_command(&mut gnu, &mut neo, "shell");
    let shell_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*shell*"))
            && grid.iter().any(|row| row.contains("tui-sh>"))
    };
    gnu.read_until(Duration::from_secs(10), shell_ready);
    neo.read_until(Duration::from_secs(14), shell_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    if !shell_ready(&gnu.text_grid()) || !shell_ready(&neo.text_grid()) {
        dump_pair_grids(
            "shell_via_mx_runs_interactive_command_in_comint_buffer/prompt",
            &gnu,
            &neo,
        );
    }
    assert_pair_nearly_matches(
        "shell_via_mx_runs_interactive_command_in_comint_buffer/prompt",
        &gnu,
        &neo,
        4,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"printf 'tui-interactive-shell-ok\\n'");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let command_ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("tui-interactive-shell-ok"))
            && grid.iter().filter(|row| row.contains("tui-sh>")).count() >= 2
    };
    gnu.read_until(Duration::from_secs(10), command_ready);
    neo.read_until(Duration::from_secs(14), command_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    if !command_ready(&gnu.text_grid()) || !command_ready(&neo.text_grid()) {
        dump_pair_grids(
            "shell_via_mx_runs_interactive_command_in_comint_buffer",
            &gnu,
            &neo,
        );
    }
    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter()
                .any(|row| row.contains("tui-interactive-shell-ok")),
            "{label} should show interactive shell command output"
        );
        assert!(
            grid.iter().any(|row| row.contains("*shell*")),
            "{label} should stay in the shell buffer"
        );
    }
    assert_pair_nearly_matches(
        "shell_via_mx_runs_interactive_command_in_comint_buffer",
        &gnu,
        &neo,
        6,
    );
}

#[test]
fn compile_via_mx_runs_command_in_compilation_buffer() {
    let (mut gnu, mut neo) = boot_pair("");

    invoke_mx_command(&mut gnu, &mut neo, "compile");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Compile command:"));
    gnu.read_until(Duration::from_secs(8), prompt_ready);
    neo.read_until(Duration::from_secs(10), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "compile_via_mx_runs_command_in_compilation_buffer/prompt",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "C-a C-k");
    for session in [&mut gnu, &mut neo] {
        session.send(b"printf 'tui-compile-ok\\n'");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*compilation*"))
            && grid.iter().any(|row| row.contains("tui-compile-ok"))
            && grid
                .iter()
                .any(|row| row.contains("finished") || row.contains("exited"))
    };
    gnu.read_until(Duration::from_secs(12), ready);
    neo.read_until(Duration::from_secs(14), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    if !ready(&gnu.text_grid()) || !ready(&neo.text_grid()) {
        dump_pair_grids(
            "compile_via_mx_runs_command_in_compilation_buffer",
            &gnu,
            &neo,
        );
    }
    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains("*compilation*")),
            "{label} should display the compilation buffer"
        );
        assert!(
            grid.iter().any(|row| row.contains("tui-compile-ok")),
            "{label} should show compilation command output"
        );
    }
    assert_pair_nearly_matches(
        "compile_via_mx_runs_command_in_compilation_buffer",
        &gnu,
        &neo,
        5,
    );
}

#[test]
fn grep_via_mx_lists_matching_file_lines() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "grep-usage.txt",
        "alpha needle one\nbeta plain\ngamma needle two\n",
        "C-x C-f",
    );

    invoke_mx_command(&mut gnu, &mut neo, "grep");
    let prompt_ready =
        |grid: &[String]| grid.iter().any(|row| row.contains("Run grep (like this):"));
    gnu.read_until(Duration::from_secs(8), prompt_ready);
    neo.read_until(Duration::from_secs(10), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "grep_via_mx_lists_matching_file_lines/prompt",
        &gnu,
        &neo,
        3,
    );

    send_both(&mut gnu, &mut neo, "C-a C-k");
    for session in [&mut gnu, &mut neo] {
        session.send(b"grep -n needle grep-usage.txt");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*grep*"))
            && grid
                .iter()
                .any(|row| row.contains("grep-usage.txt:1:alpha needle one"))
            && grid
                .iter()
                .any(|row| row.contains("grep-usage.txt:3:gamma needle two"))
    };
    gnu.read_until(Duration::from_secs(12), ready);
    neo.read_until(Duration::from_secs(14), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    if !ready(&gnu.text_grid()) || !ready(&neo.text_grid()) {
        dump_pair_grids("grep_via_mx_lists_matching_file_lines", &gnu, &neo);
    }
    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains("*grep*")),
            "{label} should display the grep buffer"
        );
        assert!(
            grid.iter().any(|row| row.contains("alpha needle one"))
                && grid.iter().any(|row| row.contains("gamma needle two")),
            "{label} should show matching grep lines"
        );
    }
    assert_pair_nearly_matches("grep_via_mx_lists_matching_file_lines", &gnu, &neo, 5);
}

#[test]
fn diff_buffer_with_file_via_mx_shows_unsaved_changes() {
    let (mut gnu, mut neo) = boot_pair("");
    let shared_path = write_shared_temp_file("diff-buffer-file.txt", "alpha\nbeta\n");
    open_shared_file(&mut gnu, &mut neo, &shared_path, "C-x C-f");

    send_both_raw(&mut gnu, &mut neo, b"changed\n");
    let edited = |grid: &[String]| grid.iter().any(|row| row.contains("changed"));
    gnu.read_until(Duration::from_secs(6), edited);
    neo.read_until(Duration::from_secs(8), edited);

    invoke_mx_command(&mut gnu, &mut neo, "diff-buffer-with-file");
    let buffer_prompt = |grid: &[String]| grid.iter().any(|row| row.contains("Buffer:"));
    gnu.read_until(Duration::from_secs(6), buffer_prompt);
    neo.read_until(Duration::from_secs(8), buffer_prompt);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    send_both(&mut gnu, &mut neo, "RET");

    let diff_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*Diff*"))
            && grid.iter().any(|row| row.contains("+changed"))
            && grid.iter().any(|row| row.contains(" alpha"))
    };
    gnu.read_until(Duration::from_secs(10), diff_ready);
    neo.read_until(Duration::from_secs(14), diff_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    if !diff_ready(&gnu.text_grid()) || !diff_ready(&neo.text_grid()) {
        dump_pair_grids(
            "diff_buffer_with_file_via_mx_shows_unsaved_changes",
            &gnu,
            &neo,
        );
    }
    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains("*Diff*")),
            "{label} should display the diff buffer"
        );
        assert!(
            grid.iter().any(|row| row.contains("+changed"))
                && grid.iter().any(|row| row.contains(" alpha")),
            "{label} should show added and context diff lines"
        );
    }
    assert_pair_nearly_matches(
        "diff_buffer_with_file_via_mx_shows_unsaved_changes",
        &gnu,
        &neo,
        10,
    );
}
