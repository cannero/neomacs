//! TUI comparisons for common Emacs workflows from GNU's tutorial.
//!
//! The command set here is chosen from `lisp/tutorial.el`, which
//! documents the default key sequences GNU Emacs treats as the common
//! day-to-day editing path.

use neomacs_tui_tests::*;
use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

fn resize_both(gnu: &mut TuiSession, neo: &mut TuiSession, rows: u16, cols: u16) {
    gnu.resize(rows, cols);
    neo.resize(rows, cols);
}

fn scratch_ready(grid: &[String]) -> bool {
    grid.iter().any(|row| row.contains("*scratch*"))
        && grid
            .iter()
            .any(|row| row.contains("This buffer is for text that is not saved"))
}

#[test]
fn terminal_resize_updates_frame_geometry() {
    const TARGET_ROWS: u16 = 30;
    const TARGET_COLS: u16 = 100;

    let (mut gnu, mut neo) = boot_pair("");
    resize_both(&mut gnu, &mut neo, TARGET_ROWS, TARGET_COLS);

    // Let GNU's SIGWINCH path and Neomacs' TTY resize watcher enqueue the
    // resize before the next input command reads pending events.
    std::thread::sleep(Duration::from_millis(500));
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "M-:");
    read_both(&mut gnu, &mut neo, Duration::from_secs(2));
    for session in [&mut gnu, &mut neo] {
        session.send(br#"(message "resize-test %sx%s" (frame-width) (frame-height))"#);
    }
    send_both(&mut gnu, &mut neo, "RET");

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

fn send_help_sequence(gnu: &mut TuiSession, neo: &mut TuiSession, key: &str) {
    send_both(gnu, neo, "C-h");
    let prefix_ready = |grid: &[String]| grid.iter().any(|row| row.contains("C-h-"));
    gnu.read_until(Duration::from_secs(6), prefix_ready);
    neo.read_until(Duration::from_secs(8), prefix_ready);
    read_both(gnu, neo, Duration::from_millis(300));
    send_both(gnu, neo, key);
}

fn invoke_mx_command(gnu: &mut TuiSession, neo: &mut TuiSession, command: &str) {
    send_both(gnu, neo, "M-x");
    let mx_prompt = |grid: &[String]| grid.last().is_some_and(|row| row.contains("M-x"));
    gnu.read_until(Duration::from_secs(6), mx_prompt);
    neo.read_until(Duration::from_secs(8), mx_prompt);
    read_both(gnu, neo, Duration::from_millis(300));

    gnu.send(command.as_bytes());
    neo.send(command.as_bytes());
    send_both(gnu, neo, "RET");
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

fn assert_top_rows_nearly_match(
    label: &str,
    gnu: &TuiSession,
    neo: &TuiSession,
    rows: usize,
    allowed_rows: usize,
) {
    let gl = gnu.text_grid();
    let nl = neo.text_grid();
    let rows = rows.min(gl.len()).min(nl.len());
    let diffs = meaningful_diffs(diff_text_grids(&gl[..rows], &nl[..rows]));
    if !diffs.is_empty() {
        eprintln!("{label}: {} top rows differ", diffs.len());
        print_row_diffs(&diffs);
    }
    assert!(
        diffs.len() <= allowed_rows,
        "{label} top rows differ in {} rows",
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

fn fido_bottom_start() -> usize {
    (ROWS as usize).saturating_sub(8)
}

fn bottom_nonempty_rows(session: &TuiSession, first_row: usize) -> Vec<String> {
    bottom_nonempty_rows_from_grid(&session.text_grid(), first_row)
}

fn bottom_nonempty_rows_from_grid(grid: &[String], first_row: usize) -> Vec<String> {
    grid.iter()
        .skip(first_row)
        .map(|row| row.trim().to_string())
        .filter(|row| !row.is_empty())
        .collect()
}

fn assert_fido_prompt_matches_stable_behavior(label: &str, gnu: &TuiSession, neo: &TuiSession) {
    let first_row = fido_bottom_start();
    let gnu_rows = bottom_nonempty_rows(gnu, first_row);
    let neo_rows = bottom_nonempty_rows(neo, first_row);

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

fn assert_describe_mode_help_content(label: &str, gnu: &TuiSession, neo: &TuiSession) {
    for (editor, session) in [("GNU", gnu), ("NEO", neo)] {
        let grid = session.text_grid();
        for needle in [
            "*Help*",
            "Major mode lisp-interaction-mode",
            "eval-print-last-sexp",
            "lisp-interaction-mode-hook",
        ] {
            assert!(
                grid.iter().any(|row| row.contains(needle)),
                "{label}: {editor} help buffer should contain {needle:?}"
            );
        }
    }
}

fn backtrace_ready(grid: &[String]) -> bool {
    grid.iter().any(|row| row.contains("*Backtrace*"))
        && grid.iter().any(|row| row.contains("Debugger entered"))
        && grid
            .iter()
            .any(|row| row.contains("void-variable") || row.contains("value as variable is void"))
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
        let bottom_start = fido_bottom_start();
        grid.iter().any(|row| row.contains("find-file"))
            && grid[bottom_start..]
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

fn save_current_file_and_assert_contents(
    label: &str,
    gnu: &mut TuiSession,
    neo: &mut TuiSession,
    name: &str,
    expected: &str,
) {
    send_both(gnu, neo, "C-x C-s");

    let gnu_path = gnu.home_dir().join(name);
    let neo_path = neo.home_dir().join(name);
    for _ in 0..10 {
        read_both(gnu, neo, Duration::from_millis(300));
        let gnu_saved = fs::read_to_string(&gnu_path).ok().as_deref() == Some(expected);
        let neo_saved = fs::read_to_string(&neo_path).ok().as_deref() == Some(expected);
        if gnu_saved && neo_saved {
            break;
        }
    }

    assert_eq!(
        fs::read_to_string(&gnu_path).expect("read GNU saved file"),
        expected,
        "{label}: GNU saved file contents should match"
    );
    assert_eq!(
        fs::read_to_string(&neo_path).expect("read Neo saved file"),
        expected,
        "{label}: Neomacs saved file contents should match"
    );
}

fn assert_home_file_contents(gnu: &TuiSession, neo: &TuiSession, name: &str, expected: &str) {
    assert_eq!(
        fs::read_to_string(gnu.home_dir().join(name)).expect("read GNU home file"),
        expected,
        "GNU file contents should match"
    );
    assert_eq!(
        fs::read_to_string(neo.home_dir().join(name)).expect("read Neo home file"),
        expected,
        "Neomacs file contents should match"
    );
}

fn make_shared_dired_fixture(label: &str) -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "neomacs-dired-root-{label}-{}-{unique}",
        std::process::id()
    ));
    let dir = root.join("work");
    fs::create_dir_all(dir.join("nested")).expect("create dired fixture directory");
    fs::write(dir.join("alpha.txt"), "alpha body\n").expect("write alpha fixture");
    fs::write(dir.join("beta.org"), "* beta heading\n").expect("write beta fixture");
    fs::write(dir.join("zeta.log"), "zeta body\n").expect("write zeta fixture");
    dir
}

fn open_shared_dired(gnu: &mut TuiSession, neo: &mut TuiSession, dir: &std::path::Path) {
    send_both(gnu, neo, "C-x d");
    let dired_path = format!("{}/", dir.display());
    gnu.send(dired_path.as_bytes());
    neo.send(dired_path.as_bytes());
    send_both(gnu, neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("Dired by name"))
            && ["alpha.txt", "beta.org", "nested", "zeta.log"]
                .iter()
                .all(|name| grid.iter().any(|row| row.contains(name)))
    };
    gnu.read_until(Duration::from_secs(10), ready);
    neo.read_until(Duration::from_secs(20), ready);
    read_both(gnu, neo, Duration::from_secs(1));
}

fn dired_goto_file(gnu: &mut TuiSession, neo: &mut TuiSession, file: &std::path::Path) {
    send_both(gnu, neo, "j");
    let prompt_ready = |grid: &[String]| grid.last().is_some_and(|row| row.contains("Goto file:"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    let file_name = file
        .file_name()
        .and_then(|name| name.to_str())
        .expect("fixture file name should be utf-8")
        .to_string();
    let file_path = file.to_string_lossy().into_owned();
    gnu.send(file_path.as_bytes());
    neo.send(file_path.as_bytes());
    send_both(gnu, neo, "RET");
    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains(&file_name))
            && grid.last().is_none_or(|row| !row.contains("Goto file:"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
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
fn region_kill_yank_and_undo_round_trip() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "kill-yank-undo.txt",
        "one two three\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-SPC M-f C-w");
    let killed = |grid: &[String]| {
        grid.iter().any(|row| row.contains(" two three"))
            && !grid.iter().any(|row| row.contains("one two three"))
    };
    gnu.read_until(Duration::from_secs(6), killed);
    neo.read_until(Duration::from_secs(8), killed);
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));
    assert_pair_nearly_matches("region_kill_yank_and_undo_round_trip/killed", &gnu, &neo, 2);

    send_both(&mut gnu, &mut neo, "C-y");
    let yanked = |grid: &[String]| grid.iter().any(|row| row.contains("one two three"));
    gnu.read_until(Duration::from_secs(6), yanked);
    neo.read_until(Duration::from_secs(8), yanked);
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));
    assert_pair_nearly_matches("region_kill_yank_and_undo_round_trip/yanked", &gnu, &neo, 2);

    send_both(&mut gnu, &mut neo, "C-/");
    gnu.read_until(Duration::from_secs(6), killed);
    neo.read_until(Duration::from_secs(8), killed);
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));
    assert_pair_nearly_matches(
        "region_kill_yank_and_undo_round_trip/undo-yank",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "C-y");
    gnu.read_until(Duration::from_secs(6), yanked);
    neo.read_until(Duration::from_secs(8), yanked);
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));
    save_current_file_and_assert_contents(
        "region_kill_yank_and_undo_round_trip",
        &mut gnu,
        &mut neo,
        "kill-yank-undo.txt",
        "one two three\n",
    );
}

#[test]
fn query_replace_via_mx_accepts_all_matches_and_saves() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "query-replace.txt",
        "foo one\nfoo two\nbar\n",
        "C-x C-f",
    );

    invoke_mx_command(&mut gnu, &mut neo, "query-replace");
    let from_prompt = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Query replace") && !row.contains("with:"))
    };
    gnu.read_until(Duration::from_secs(6), from_prompt);
    neo.read_until(Duration::from_secs(8), from_prompt);
    for session in [&mut gnu, &mut neo] {
        session.send(b"foo");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let to_prompt = |grid: &[String]| grid.iter().any(|row| row.contains("with:"));
    gnu.read_until(Duration::from_secs(6), to_prompt);
    neo.read_until(Duration::from_secs(8), to_prompt);
    for session in [&mut gnu, &mut neo] {
        session.send(b"baz");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let replacement_prompt = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Query replacing") || row.contains("Replace"))
    };
    gnu.read_until(Duration::from_secs(6), replacement_prompt);
    neo.read_until(Duration::from_secs(8), replacement_prompt);
    send_both_raw(&mut gnu, &mut neo, b"!");

    let replaced = |grid: &[String]| {
        grid.iter().any(|row| row.contains("baz one"))
            && grid.iter().any(|row| row.contains("baz two"))
    };
    gnu.read_until(Duration::from_secs(6), replaced);
    neo.read_until(Duration::from_secs(8), replaced);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "query_replace_via_mx_accepts_all_matches_and_saves",
        &gnu,
        &neo,
        2,
    );

    save_current_file_and_assert_contents(
        "query_replace_via_mx_accepts_all_matches_and_saves",
        &mut gnu,
        &mut neo,
        "query-replace.txt",
        "baz one\nbaz two\nbar\n",
    );
    assert_home_file_contents(&gnu, &neo, "query-replace.txt", "baz one\nbaz two\nbar\n");
}

#[test]
fn find_file_tab_completion_via_cx_cf_completes_unique_home_file() {
    let (mut gnu, mut neo) = boot_pair("");
    write_home_file(
        &gnu,
        "completion-unique-target.txt",
        "completed file body\n",
    );
    write_home_file(
        &neo,
        "completion-unique-target.txt",
        "completed file body\n",
    );

    send_both(&mut gnu, &mut neo, "C-x C-f");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Find file:"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "find_file_tab_completion_via_cx_cf_completes_unique_home_file/prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"~/completion-uniq");
    }
    send_both(&mut gnu, &mut neo, "TAB");
    let completed = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("completion-unique-target.txt"))
    };
    gnu.read_until(Duration::from_secs(6), completed);
    neo.read_until(Duration::from_secs(8), completed);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "find_file_tab_completion_via_cx_cf_completes_unique_home_file/completed",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "RET");
    let ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("completion-unique-target.txt"))
            && grid.iter().any(|row| row.contains("completed file body"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "find_file_tab_completion_via_cx_cf_completes_unique_home_file",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn list_directory_via_cx_cd_lists_entries() {
    let (mut gnu, mut neo) = boot_pair("");
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_micros();
    let dir = std::env::temp_dir().join(format!("nmls-{}-{unique}", std::process::id()));
    fs::create_dir_all(dir.join("nested")).expect("create list-directory fixture");
    fs::write(dir.join("alpha.txt"), "alpha body\n").expect("write alpha fixture");
    fs::write(dir.join("beta.org"), "* beta heading\n").expect("write beta fixture");
    fs::write(dir.join("zeta.log"), "zeta body\n").expect("write zeta fixture");

    send_both(&mut gnu, &mut neo, "C-x C-d");
    let prompt_ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("List directory (brief):"))
    };
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    let list_path = format!("{}/", dir.display());
    gnu.send(list_path.as_bytes());
    neo.send(list_path.as_bytes());
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*Directory*"))
            && grid.iter().any(|row| row.contains("Directory "))
            && ["alpha.txt", "beta.org", "nested", "zeta.log"]
                .iter()
                .all(|name| grid.iter().any(|row| row.contains(name)))
    };
    gnu.read_until(Duration::from_secs(10), ready);
    neo.read_until(Duration::from_secs(20), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("list_directory_via_cx_cd_lists_entries", &gnu, &neo, 2);
}

#[test]
fn dired_open_directory_via_cx_d_lists_entries() {
    let (mut gnu, mut neo) = boot_pair("");
    let dir = make_shared_dired_fixture("open");

    open_shared_dired(&mut gnu, &mut neo, &dir);

    assert_pair_nearly_matches("dired_open_directory_via_cx_d_lists_entries", &gnu, &neo, 0);
}

#[test]
fn dired_mark_flag_and_unmark_current_file() {
    let (mut gnu, mut neo) = boot_pair("");
    let dir = make_shared_dired_fixture("mark");
    let alpha = dir.join("alpha.txt");
    let beta = dir.join("beta.org");

    open_shared_dired(&mut gnu, &mut neo, &dir);
    dired_goto_file(&mut gnu, &mut neo, &alpha);
    send_both(&mut gnu, &mut neo, "m");
    let alpha_marked = |grid: &[String]| {
        grid.iter()
            .any(|row| row.starts_with('*') && row.contains("alpha.txt"))
    };
    gnu.read_until(Duration::from_secs(6), alpha_marked);
    neo.read_until(Duration::from_secs(8), alpha_marked);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "dired_mark_flag_and_unmark_current_file/mark",
        &gnu,
        &neo,
        0,
    );

    send_both(&mut gnu, &mut neo, "DEL");
    let alpha_unmarked = |grid: &[String]| {
        grid.iter()
            .any(|row| !row.starts_with('*') && row.contains("alpha.txt"))
            && !grid
                .iter()
                .any(|row| row.starts_with('*') && row.contains("alpha.txt"))
    };
    gnu.read_until(Duration::from_secs(6), alpha_unmarked);
    neo.read_until(Duration::from_secs(8), alpha_unmarked);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "dired_mark_flag_and_unmark_current_file/unmark",
        &gnu,
        &neo,
        0,
    );

    dired_goto_file(&mut gnu, &mut neo, &beta);
    send_both(&mut gnu, &mut neo, "d");
    let beta_flagged = |grid: &[String]| {
        grid.iter()
            .any(|row| row.starts_with('D') && row.contains("beta.org"))
    };
    gnu.read_until(Duration::from_secs(6), beta_flagged);
    neo.read_until(Duration::from_secs(8), beta_flagged);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "dired_mark_flag_and_unmark_current_file/flag",
        &gnu,
        &neo,
        0,
    );

    send_both(&mut gnu, &mut neo, "DEL");
    let beta_unflagged = |grid: &[String]| {
        grid.iter()
            .any(|row| !row.starts_with('D') && row.contains("beta.org"))
            && !grid
                .iter()
                .any(|row| row.starts_with('D') && row.contains("beta.org"))
    };
    gnu.read_until(Duration::from_secs(6), beta_unflagged);
    neo.read_until(Duration::from_secs(8), beta_unflagged);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "dired_mark_flag_and_unmark_current_file/unflag",
        &gnu,
        &neo,
        0,
    );
}

#[test]
fn dired_find_file_via_ret_visits_current_file() {
    let (mut gnu, mut neo) = boot_pair("");
    let dir = make_shared_dired_fixture("find-file");
    let beta = dir.join("beta.org");

    open_shared_dired(&mut gnu, &mut neo, &dir);
    dired_goto_file(&mut gnu, &mut neo, &beta);
    send_both(&mut gnu, &mut neo, "RET");
    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("* beta heading"))
            && grid.iter().any(|row| row.contains("beta.org"))
            && !grid.iter().any(|row| row.contains("Dired by name"))
    };
    gnu.read_until(Duration::from_secs(60), ready);
    neo.read_until(Duration::from_secs(60), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("dired_find_file_via_ret_visits_current_file", &gnu, &neo, 0);
}

#[test]
fn dired_jump_via_cx_cj_opens_parent_listing_on_current_file() {
    let (mut gnu, mut neo) = boot_pair("");
    let dir = make_shared_dired_fixture("jump");
    let beta = dir.join("beta.org");

    send_both(&mut gnu, &mut neo, "C-x C-f");
    let beta_path = beta.to_string_lossy().into_owned();
    gnu.send(beta_path.as_bytes());
    neo.send(beta_path.as_bytes());
    send_both(&mut gnu, &mut neo, "RET");

    let file_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("beta heading"))
            && grid.iter().any(|row| row.contains("beta.org"))
    };
    gnu.read_until(Duration::from_secs(6), file_ready);
    neo.read_until(Duration::from_secs(8), file_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x C-j");
    let dired_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("Dired by name"))
            && ["alpha.txt", "beta.org", "nested", "zeta.log"]
                .iter()
                .all(|name| grid.iter().any(|row| row.contains(name)))
    };
    gnu.read_until(Duration::from_secs(10), dired_ready);
    neo.read_until(Duration::from_secs(20), dired_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "dired_jump_via_cx_cj_opens_parent_listing_on_current_file/dired",
        &gnu,
        &neo,
        0,
    );

    send_both(&mut gnu, &mut neo, "RET");
    gnu.read_until(Duration::from_secs(6), file_ready);
    neo.read_until(Duration::from_secs(8), file_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "dired_jump_via_cx_cj_opens_parent_listing_on_current_file/revisit",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn dired_jump_other_window_via_cx4_cj_keeps_file_and_listing_visible() {
    let (mut gnu, mut neo) = boot_pair("");
    let dir = make_shared_dired_fixture("jump-other-window");
    let beta = dir.join("beta.org");

    send_both(&mut gnu, &mut neo, "C-x C-f");
    let beta_path = beta.to_string_lossy().into_owned();
    gnu.send(beta_path.as_bytes());
    neo.send(beta_path.as_bytes());
    send_both(&mut gnu, &mut neo, "RET");

    let file_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("beta heading"))
            && grid.iter().any(|row| row.contains("beta.org"))
    };
    gnu.read_until(Duration::from_secs(6), file_ready);
    neo.read_until(Duration::from_secs(8), file_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x 4 C-j");
    let jump_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("Dired by name"))
            && grid.iter().any(|row| row.contains("beta heading"))
            && ["alpha.txt", "beta.org", "nested", "zeta.log"]
                .iter()
                .all(|name| grid.iter().any(|row| row.contains(name)))
    };
    gnu.read_until(Duration::from_secs(10), jump_ready);
    neo.read_until(Duration::from_secs(20), jump_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "dired_jump_other_window_via_cx4_cj_keeps_file_and_listing_visible",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn dired_copy_current_file_via_c_copies_file_and_updates_listing() {
    let (mut gnu, mut neo) = boot_pair("");
    let dir = make_shared_dired_fixture("copy");
    let alpha = dir.join("alpha.txt");
    let alpha_copy = dir.join("alpha-copy.txt");
    let alpha_copy_path = alpha_copy.to_string_lossy().into_owned();

    open_shared_dired(&mut gnu, &mut neo, &dir);
    dired_goto_file(&mut gnu, &mut neo, &alpha);

    gnu.send_keys("C");
    let copy_prompt = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Copy") && row.contains("alpha.txt") && row.contains("to:"))
    };
    gnu.read_until(Duration::from_secs(6), copy_prompt);
    gnu.send(alpha_copy_path.as_bytes());
    gnu.send_keys("RET");
    let copy_ready = |grid: &[String]| grid.iter().any(|row| row.contains("alpha-copy.txt"));
    gnu.read_until(Duration::from_secs(10), copy_ready);
    gnu.read(Duration::from_secs(1));
    assert_eq!(
        fs::read_to_string(&alpha_copy).expect("GNU should copy dired file"),
        "alpha body\n"
    );

    fs::remove_file(&alpha_copy).expect("reset copied file before Neomacs operation");

    neo.send_keys("C");
    neo.read_until(Duration::from_secs(8), copy_prompt);
    neo.send(alpha_copy_path.as_bytes());
    neo.send_keys("RET");
    neo.read_until(Duration::from_secs(12), copy_ready);
    neo.read(Duration::from_secs(1));
    assert_eq!(
        fs::read_to_string(&alpha_copy).expect("Neomacs should copy dired file"),
        "alpha body\n"
    );

    assert_pair_nearly_matches(
        "dired_copy_current_file_via_c_copies_file_and_updates_listing",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn dired_rename_current_file_via_r_moves_file_and_updates_listing() {
    let (mut gnu, mut neo) = boot_pair("");
    let dir = make_shared_dired_fixture("rename");
    let beta = dir.join("beta.org");
    let beta_renamed = dir.join("beta-renamed.org");
    let beta_renamed_path = beta_renamed.to_string_lossy().into_owned();

    open_shared_dired(&mut gnu, &mut neo, &dir);
    dired_goto_file(&mut gnu, &mut neo, &beta);

    gnu.send_keys("R");
    let rename_prompt = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Rename") && row.contains("beta.org") && row.contains("to:"))
    };
    gnu.read_until(Duration::from_secs(6), rename_prompt);
    gnu.send(beta_renamed_path.as_bytes());
    gnu.send_keys("RET");
    let rename_ready = |grid: &[String]| grid.iter().any(|row| row.contains("beta-renamed.org"));
    gnu.read_until(Duration::from_secs(10), rename_ready);
    gnu.read(Duration::from_secs(1));
    assert!(
        beta_renamed.exists() && !beta.exists(),
        "GNU should rename beta.org to beta-renamed.org"
    );

    fs::rename(&beta_renamed, &beta).expect("reset renamed file before Neomacs operation");

    neo.send_keys("R");
    neo.read_until(Duration::from_secs(8), rename_prompt);
    neo.send(beta_renamed_path.as_bytes());
    neo.send_keys("RET");
    neo.read_until(Duration::from_secs(12), rename_ready);
    neo.read(Duration::from_secs(1));
    assert!(
        beta_renamed.exists() && !beta.exists(),
        "Neomacs should rename beta.org to beta-renamed.org"
    );

    assert_pair_nearly_matches(
        "dired_rename_current_file_via_r_moves_file_and_updates_listing",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn dired_delete_current_file_via_d_confirms_and_removes_listing() {
    let (mut gnu, mut neo) = boot_pair("");
    let dir = make_shared_dired_fixture("delete");
    let zeta = dir.join("zeta.log");

    open_shared_dired(&mut gnu, &mut neo, &dir);
    dired_goto_file(&mut gnu, &mut neo, &zeta);

    gnu.send_keys("D");
    let delete_prompt = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Delete") && row.contains("zeta.log"))
    };
    gnu.read_until(Duration::from_secs(6), delete_prompt);
    gnu.send(b"yes");
    gnu.send_keys("RET");
    let delete_ready = |grid: &[String]| !grid.iter().any(|row| row.contains("zeta.log"));
    gnu.read_until(Duration::from_secs(10), delete_ready);
    gnu.read(Duration::from_secs(1));
    assert!(!zeta.exists(), "GNU should delete zeta.log from disk");

    fs::write(&zeta, "zeta body\n").expect("reset deleted file before Neomacs operation");

    neo.send_keys("D");
    neo.read_until(Duration::from_secs(8), delete_prompt);
    neo.send(b"yes");
    neo.send_keys("RET");
    neo.read_until(Duration::from_secs(12), delete_ready);
    neo.read(Duration::from_secs(1));
    assert!(!zeta.exists(), "Neomacs should delete zeta.log from disk");

    assert_pair_nearly_matches(
        "dired_delete_current_file_via_d_confirms_and_removes_listing",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn make_directory_via_mx_creates_directory_on_disk() {
    let (mut gnu, mut neo) = boot_pair("");

    invoke_mx_command(&mut gnu, &mut neo, "make-directory");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Make directory:"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "make_directory_via_mx_creates_directory_on_disk/prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"~/mx-made-dir");
    }
    let typed_ready = |grid: &[String]| grid.iter().any(|row| row.contains("mx-made-dir"));
    gnu.read_until(Duration::from_secs(6), typed_ready);
    neo.read_until(Duration::from_secs(8), typed_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "make_directory_via_mx_creates_directory_on_disk/before-ret",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "RET");
    let gnu_dir = gnu.home_dir().join("mx-made-dir");
    let neo_dir = neo.home_dir().join("mx-made-dir");
    for _ in 0..10 {
        read_both(&mut gnu, &mut neo, Duration::from_millis(300));
        if gnu_dir.is_dir() && neo_dir.is_dir() {
            break;
        }
    }

    assert!(
        gnu_dir.is_dir(),
        "GNU should create directory via M-x make-directory"
    );
    assert!(
        neo_dir.is_dir(),
        "Neomacs should create directory via M-x make-directory"
    );

    send_both(&mut gnu, &mut neo, "C-l");
    gnu.read_until(Duration::from_secs(6), scratch_ready);
    neo.read_until(Duration::from_secs(8), scratch_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "make_directory_via_mx_creates_directory_on_disk",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn rename_file_via_mx_moves_file_on_disk() {
    let (mut gnu, mut neo) = boot_pair("");
    write_home_file(&gnu, "rename-source.txt", "rename me\n");
    write_home_file(&neo, "rename-source.txt", "rename me\n");

    invoke_mx_command(&mut gnu, &mut neo, "rename-file");
    let source_prompt = |grid: &[String]| grid.iter().any(|row| row.contains("Rename file:"));
    gnu.read_until(Duration::from_secs(6), source_prompt);
    neo.read_until(Duration::from_secs(8), source_prompt);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "rename_file_via_mx_moves_file_on_disk/source-prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"~/rename-source.txt");
    }
    let source_typed = |grid: &[String]| grid.iter().any(|row| row.contains("rename-source.txt"));
    gnu.read_until(Duration::from_secs(6), source_typed);
    neo.read_until(Duration::from_secs(8), source_typed);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "rename_file_via_mx_moves_file_on_disk/source-before-ret",
        &gnu,
        &neo,
        2,
    );
    send_both(&mut gnu, &mut neo, "RET");

    let dest_prompt = |grid: &[String]| grid.iter().any(|row| row.contains("to file:"));
    gnu.read_until(Duration::from_secs(6), dest_prompt);
    neo.read_until(Duration::from_secs(8), dest_prompt);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "rename_file_via_mx_moves_file_on_disk/dest-prompt",
        &gnu,
        &neo,
        3,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"~/rename-dest.txt");
    }
    let dest_typed = |grid: &[String]| grid.iter().any(|row| row.contains("rename-dest.txt"));
    gnu.read_until(Duration::from_secs(6), dest_typed);
    neo.read_until(Duration::from_secs(8), dest_typed);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "rename_file_via_mx_moves_file_on_disk/dest-before-ret",
        &gnu,
        &neo,
        3,
    );
    send_both(&mut gnu, &mut neo, "RET");

    let gnu_src = gnu.home_dir().join("rename-source.txt");
    let neo_src = neo.home_dir().join("rename-source.txt");
    let gnu_dest = gnu.home_dir().join("rename-dest.txt");
    let neo_dest = neo.home_dir().join("rename-dest.txt");
    for _ in 0..10 {
        read_both(&mut gnu, &mut neo, Duration::from_millis(300));
        if !gnu_src.exists() && !neo_src.exists() && gnu_dest.exists() && neo_dest.exists() {
            break;
        }
    }

    assert!(
        !gnu_src.exists(),
        "GNU rename-file should remove source path"
    );
    assert!(
        !neo_src.exists(),
        "Neomacs rename-file should remove source path"
    );
    assert_eq!(
        fs::read_to_string(&gnu_dest).expect("read GNU renamed file"),
        "rename me\n"
    );
    assert_eq!(
        fs::read_to_string(&neo_dest).expect("read Neomacs renamed file"),
        "rename me\n"
    );

    send_both(&mut gnu, &mut neo, "C-l");
    gnu.read_until(Duration::from_secs(6), scratch_ready);
    neo.read_until(Duration::from_secs(8), scratch_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches("rename_file_via_mx_moves_file_on_disk", &gnu, &neo, 2);
}

#[test]
fn delete_file_via_mx_removes_file_from_disk() {
    let (mut gnu, mut neo) = boot_pair("");
    write_home_file(&gnu, "delete-direct.txt", "delete me\n");
    write_home_file(&neo, "delete-direct.txt", "delete me\n");

    invoke_mx_command(&mut gnu, &mut neo, "delete-file");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Delete file:"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "delete_file_via_mx_removes_file_from_disk/prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"~/delete-direct.txt");
    }
    let typed_ready = |grid: &[String]| grid.iter().any(|row| row.contains("delete-direct.txt"));
    gnu.read_until(Duration::from_secs(6), typed_ready);
    neo.read_until(Duration::from_secs(8), typed_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "delete_file_via_mx_removes_file_from_disk/before-ret",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "RET");
    let gnu_path = gnu.home_dir().join("delete-direct.txt");
    let neo_path = neo.home_dir().join("delete-direct.txt");
    for _ in 0..10 {
        read_both(&mut gnu, &mut neo, Duration::from_millis(300));
        if !gnu_path.exists() && !neo_path.exists() {
            break;
        }
    }

    assert!(
        !gnu_path.exists(),
        "GNU delete-file should remove target path"
    );
    assert!(
        !neo_path.exists(),
        "Neomacs delete-file should remove target path"
    );

    send_both(&mut gnu, &mut neo, "C-l");
    gnu.read_until(Duration::from_secs(6), scratch_ready);
    neo.read_until(Duration::from_secs(8), scratch_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches("delete_file_via_mx_removes_file_from_disk", &gnu, &neo, 2);
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
fn overwrite_mode_via_mx_replaces_character_at_point() {
    let (mut gnu, mut neo) = boot_pair("");

    open_home_file(
        &mut gnu,
        &mut neo,
        "overwrite-usage.txt",
        "abcdef\nsecond\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-a");
    invoke_mx_command(&mut gnu, &mut neo, "overwrite-mode");
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
        "overwrite_mode_via_mx_replaces_character_at_point",
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
fn fido_vertical_mode_mx_find_f_matches_gnu_then_cg() {
    let (mut gnu, mut neo) = boot_fido_vertical_pair();

    wait_for_fido_mx_candidates(&mut gnu, &mut neo, "find-f");
    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        let bottom_start = fido_bottom_start();
        assert!(
            grid.iter().any(|row| row.contains("find-file")),
            "{label} should show find-file in fido candidates"
        );
        assert!(
            grid[bottom_start..]
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
        fido_bottom_start(),
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
fn fido_vertical_mode_enabled_interactively_mx_shows_initial_candidates() {
    let (mut gnu, mut neo) = boot_pair("");

    invoke_mx_command(&mut gnu, &mut neo, "fido-vertical-mode");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "M-x");
    let candidates_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("M-x"))
            && bottom_nonempty_rows_from_grid(grid, fido_bottom_start()).len() >= 6
            && ["cd", "5x5", "gdb"]
                .iter()
                .all(|candidate| grid.iter().any(|row| row.contains(candidate)))
    };
    gnu.read_until(Duration::from_secs(6), candidates_ready);
    neo.read_until(Duration::from_secs(8), candidates_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    if !candidates_ready(&gnu.text_grid()) || !candidates_ready(&neo.text_grid()) {
        dump_pair_grids(
            "fido_vertical_mode_enabled_interactively_mx_shows_initial_candidates",
            &gnu,
            &neo,
        );
    }

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains("M-x")),
            "{label} should leave the M-x prompt active"
        );
        assert!(
            bottom_nonempty_rows(session, fido_bottom_start()).len() >= 6,
            "{label} should show vertical command candidates for an empty M-x query"
        );
        for candidate in ["cd", "5x5", "gdb"] {
            assert!(
                grid.iter().any(|row| row.contains(candidate)),
                "{label} should include {candidate} in the initial M-x candidates"
            );
        }
    }

    abort_minibuffer_and_wait_for_scratch(&mut gnu, &mut neo);
    assert_pair_nearly_matches(
        "fido_vertical_mode_enabled_interactively_mx_shows_initial_candidates/abort",
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
        fido_bottom_start(),
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
        fido_bottom_start(),
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

    assert_describe_mode_help_content("describe_mode_on_scratch_via_ch_m", &gnu, &neo);
    assert_top_rows_nearly_match("describe_mode_on_scratch_via_ch_m", &gnu, &neo, 16, 2);
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

    assert_describe_mode_help_content("describe_mode_outline_heading_via_ch_m", &gnu, &neo);
    assert_top_rows_nearly_match("describe_mode_outline_heading_via_ch_m", &gnu, &neo, 16, 2);
}

#[test]
fn quit_help_buffer_via_q() {
    let (mut gnu, mut neo) = boot_pair("");
    send_help_sequence(&mut gnu, &mut neo, "m");
    let help_ready = |grid: &[String]| grid.iter().any(|row| row.contains("*Help*"));
    gnu.read_until(Duration::from_secs(10), help_ready);
    neo.read_until(Duration::from_secs(20), help_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x o");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "q");
    let scratch_only_ready =
        |grid: &[String]| scratch_ready(grid) && !grid.iter().any(|row| row.contains("*Help*"));
    gnu.read_until(Duration::from_secs(6), scratch_only_ready);
    neo.read_until(Duration::from_secs(8), scratch_only_ready);
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
fn help_with_tutorial_via_ch_t_opens_tutorial_buffer() {
    let (mut gnu, mut neo) = boot_pair("");
    send_help_sequence(&mut gnu, &mut neo, "t");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("TUTORIAL"))
            && grid.iter().any(|row| row.contains("Emacs tutorial"))
            && grid.iter().any(|row| row.contains("CONTROL key"))
    };
    gnu.read_until(Duration::from_secs(8), ready);
    neo.read_until(Duration::from_secs(12), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains("TUTORIAL")),
            "{label} should show the tutorial buffer name"
        );
        assert!(
            grid.iter().any(|row| row.contains("Emacs tutorial")),
            "{label} should show the tutorial heading"
        );
        assert!(
            grid.iter().any(|row| row.contains("CONTROL key")),
            "{label} should show the tutorial contents"
        );
    }
    assert_top_rows_nearly_match(
        "help_with_tutorial_via_ch_t_opens_tutorial_buffer",
        &gnu,
        &neo,
        18,
        3,
    );
}

#[test]
fn info_directory_via_ch_i_opens_info_buffer() {
    let (mut gnu, mut neo) = boot_pair("");
    send_help_sequence(&mut gnu, &mut neo, "i");

    let ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("*info*") || row.contains("*Info*"))
            && grid
                .iter()
                .any(|row| row.contains("INFO tree") || row.contains("Directory node"))
            && grid.iter().any(|row| row.contains("Emacs"))
    };
    gnu.read_until(Duration::from_secs(12), ready);
    neo.read_until(Duration::from_secs(20), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    if !ready(&gnu.text_grid()) || !ready(&neo.text_grid()) {
        dump_pair_grids("info_directory_via_ch_i_opens_info_buffer", &gnu, &neo);
    }

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter()
                .any(|row| row.contains("*info*") || row.contains("*Info*")),
            "{label} should show the Info buffer name"
        );
        assert!(
            grid.iter()
                .any(|row| row.contains("INFO tree") || row.contains("Directory node")),
            "{label} should show the Info directory"
        );
        assert!(
            grid.iter().any(|row| row.contains("Emacs")),
            "{label} should show Emacs entries in the Info directory"
        );
    }
    assert_top_rows_nearly_match(
        "info_directory_via_ch_i_opens_info_buffer",
        &gnu,
        &neo,
        18,
        3,
    );
}

#[test]
fn calendar_via_mx_opens_calendar_and_q_quits() {
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

    if !calendar_ready(&gnu.text_grid()) || !calendar_ready(&neo.text_grid()) {
        dump_pair_grids(
            "calendar_via_mx_opens_calendar_and_q_quits/open",
            &gnu,
            &neo,
        );
    }
    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains("Calendar")),
            "{label} should display the Calendar mode line"
        );
        assert!(
            day_header_count(&grid) >= 3,
            "{label} should show Gregorian calendar day headers"
        );
    }
    assert_pair_nearly_matches(
        "calendar_via_mx_opens_calendar_and_q_quits/open",
        &gnu,
        &neo,
        4,
    );

    send_both_raw(&mut gnu, &mut neo, b"q");
    gnu.read_until(Duration::from_secs(6), scratch_ready);
    neo.read_until(Duration::from_secs(8), scratch_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "calendar_via_mx_opens_calendar_and_q_quits/quit",
        &gnu,
        &neo,
        2,
    );
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
fn save_unnamed_buffer_via_cx_cs_prompts_for_file() {
    let (mut gnu, mut neo) = boot_pair("");

    send_both(&mut gnu, &mut neo, "C-x b");
    let switch_prompt = |grid: &[String]| grid.iter().any(|row| row.contains("Switch to buffer:"));
    gnu.read_until(Duration::from_secs(6), switch_prompt);
    neo.read_until(Duration::from_secs(8), switch_prompt);
    for session in [&mut gnu, &mut neo] {
        session.send(b"unnamed-save-buffer");
    }
    send_both(&mut gnu, &mut neo, "RET");
    let buffer_ready = |grid: &[String]| grid.iter().any(|row| row.contains("unnamed-save-buffer"));
    gnu.read_until(Duration::from_secs(6), buffer_ready);
    neo.read_until(Duration::from_secs(8), buffer_ready);

    for session in [&mut gnu, &mut neo] {
        session.send(b"unnamed save line\n");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x C-s");
    let save_prompt = |grid: &[String]| grid.iter().any(|row| row.contains("File to save in:"));
    gnu.read_until(Duration::from_secs(6), save_prompt);
    neo.read_until(Duration::from_secs(8), save_prompt);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "save_unnamed_buffer_via_cx_cs_prompts_for_file/after-cx-cs",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"~/unnamed-save-buffer.txt");
    }
    let typed_path = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("unnamed-save-buffer.txt"))
    };
    gnu.read_until(Duration::from_secs(6), typed_path);
    neo.read_until(Duration::from_secs(8), typed_path);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "save_unnamed_buffer_via_cx_cs_prompts_for_file/before-ret",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("unnamed-save-buffer.txt"))
            && grid.iter().any(|row| row.contains("unnamed save line"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "save_unnamed_buffer_via_cx_cs_prompts_for_file/after-ret",
        &gnu,
        &neo,
        2,
    );

    let expected = "unnamed save line\n";
    let gnu_path = gnu.home_dir().join("unnamed-save-buffer.txt");
    let neo_path = neo.home_dir().join("unnamed-save-buffer.txt");
    for _ in 0..10 {
        read_both(&mut gnu, &mut neo, Duration::from_millis(300));
        let gnu_saved = fs::read_to_string(&gnu_path).ok().as_deref() == Some(expected);
        let neo_saved = fs::read_to_string(&neo_path).ok().as_deref() == Some(expected);
        if gnu_saved && neo_saved {
            break;
        }
    }

    assert_eq!(
        fs::read_to_string(&gnu_path).expect("read GNU unnamed saved file"),
        expected
    );
    assert_eq!(
        fs::read_to_string(&neo_path).expect("read Neo unnamed saved file"),
        expected
    );
    assert_pair_nearly_matches(
        "save_unnamed_buffer_via_cx_cs_prompts_for_file",
        &gnu,
        &neo,
        2,
    );
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
fn insert_file_via_cx_i_inserts_contents_at_point() {
    let (mut gnu, mut neo) = boot_pair("");
    write_home_file(&gnu, "insert-source.txt", "inserted alpha\ninserted beta\n");
    write_home_file(&neo, "insert-source.txt", "inserted alpha\ninserted beta\n");
    open_home_file(
        &mut gnu,
        &mut neo,
        "insert-target.txt",
        "target header\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M->");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-x i");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Insert file:"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    for session in [&mut gnu, &mut neo] {
        session.send(b"~/insert-source.txt");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("target header"))
            && grid.iter().any(|row| row.contains("inserted alpha"))
            && grid.iter().any(|row| row.contains("inserted beta"))
            && grid.iter().any(|row| row.contains("insert-target.txt"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "insert_file_via_cx_i_inserts_contents_at_point",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn find_file_read_only_then_toggle_and_save_via_cx_cr_cx_cq() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "read-only-toggle.txt",
        "original read-only body\n",
        "C-x C-r",
    );

    send_both(&mut gnu, &mut neo, "M-:");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(br#"(message "readonly:%S" buffer-read-only)"#);
    }
    send_both(&mut gnu, &mut neo, "RET");
    let readonly_ready = |grid: &[String]| grid.iter().any(|row| row.contains("readonly:t"));
    gnu.read_until(Duration::from_secs(6), readonly_ready);
    neo.read_until(Duration::from_secs(8), readonly_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "find_file_read_only_then_toggle_and_save_via_cx_cr_cx_cq/readonly",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "C-x C-q");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "M-<");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"edited line\n");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    save_current_file_and_assert_contents(
        "find_file_read_only_then_toggle_and_save_via_cx_cr_cx_cq",
        &mut gnu,
        &mut neo,
        "read-only-toggle.txt",
        "edited line\noriginal read-only body\n",
    );
    assert_pair_nearly_matches(
        "find_file_read_only_then_toggle_and_save_via_cx_cr_cx_cq",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn view_file_via_mx_opens_view_mode_and_q_quits() {
    let (mut gnu, mut neo) = boot_pair("");
    write_home_file(
        &gnu,
        "view-file.txt",
        "view mode first line\nview mode second line\n",
    );
    write_home_file(
        &neo,
        "view-file.txt",
        "view mode first line\nview mode second line\n",
    );

    invoke_mx_command(&mut gnu, &mut neo, "view-file");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("View file:"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "view_file_via_mx_opens_view_mode_and_q_quits/prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"~/view-file.txt");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("view-file.txt"))
            && grid.iter().any(|row| row.contains("view mode first line"))
            && grid.iter().any(|row| row.contains("View"))
    };
    gnu.read_until(Duration::from_secs(8), ready);
    neo.read_until(Duration::from_secs(12), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "view_file_via_mx_opens_view_mode_and_q_quits/view",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "q");
    let scratch_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*scratch*"))
            && !grid.iter().any(|row| row.contains("view-file.txt"))
    };
    gnu.read_until(Duration::from_secs(6), scratch_ready);
    neo.read_until(Duration::from_secs(8), scratch_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "view_file_via_mx_opens_view_mode_and_q_quits/quit",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn append_to_file_via_mx_appends_region_to_existing_file() {
    let (mut gnu, mut neo) = boot_pair("");
    write_home_file(&gnu, "append-to-file-dest.txt", "existing header\n");
    write_home_file(&neo, "append-to-file-dest.txt", "existing header\n");
    open_home_file(
        &mut gnu,
        &mut neo,
        "append-to-file-source.txt",
        "region alpha\nregion beta\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x h");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    invoke_mx_command(&mut gnu, &mut neo, "append-to-file");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Append to file:"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for session in [&mut gnu, &mut neo] {
        session.send(b"~/append-to-file-dest.txt");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let expected = "existing header\nregion alpha\nregion beta\n";
    let gnu_dest = gnu.home_dir().join("append-to-file-dest.txt");
    let neo_dest = neo.home_dir().join("append-to-file-dest.txt");
    for _ in 0..10 {
        read_both(&mut gnu, &mut neo, Duration::from_millis(300));
        let gnu_saved = fs::read_to_string(&gnu_dest).ok().as_deref() == Some(expected);
        let neo_saved = fs::read_to_string(&neo_dest).ok().as_deref() == Some(expected);
        if gnu_saved && neo_saved {
            break;
        }
    }

    assert_eq!(
        fs::read_to_string(&gnu_dest).expect("read GNU append destination"),
        expected
    );
    assert_eq!(
        fs::read_to_string(&neo_dest).expect("read Neo append destination"),
        expected
    );
    assert_pair_nearly_matches(
        "append_to_file_via_mx_appends_region_to_existing_file",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn copy_to_buffer_via_mx_replaces_target_buffer_contents() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "copy-to-buffer-source.txt",
        "copy alpha\ncopy beta\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x h");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    invoke_mx_command(&mut gnu, &mut neo, "copy-to-buffer");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Copy to buffer:"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    for session in [&mut gnu, &mut neo] {
        session.send(b"copy-to-buffer-target");
    }
    send_both(&mut gnu, &mut neo, "RET");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x b");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"copy-to-buffer-target");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("copy-to-buffer-target"))
            && grid.iter().any(|row| row.contains("copy alpha"))
            && grid.iter().any(|row| row.contains("copy beta"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "copy_to_buffer_via_mx_replaces_target_buffer_contents",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn append_to_buffer_via_mx_inserts_region_at_target_point() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "append-to-buffer-source.txt",
        "append alpha\nappend beta\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x b");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"append-to-buffer-target");
    }
    send_both(&mut gnu, &mut neo, "RET");
    let target_ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("append-to-buffer-target"))
    };
    gnu.read_until(Duration::from_secs(6), target_ready);
    neo.read_until(Duration::from_secs(8), target_ready);
    for session in [&mut gnu, &mut neo] {
        session.send(b"target header\n");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x b");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"append-to-buffer-source.txt");
    }
    send_both(&mut gnu, &mut neo, "RET");
    gnu.read_until(Duration::from_secs(6), |grid| {
        grid.iter().any(|row| row.contains("append alpha"))
    });
    neo.read_until(Duration::from_secs(8), |grid| {
        grid.iter().any(|row| row.contains("append alpha"))
    });

    send_both(&mut gnu, &mut neo, "C-x h");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    invoke_mx_command(&mut gnu, &mut neo, "append-to-buffer");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Append to buffer:"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    for session in [&mut gnu, &mut neo] {
        session.send(b"append-to-buffer-target");
    }
    send_both(&mut gnu, &mut neo, "RET");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x b");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"append-to-buffer-target");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("target header"))
            && grid.iter().any(|row| row.contains("append alpha"))
            && grid.iter().any(|row| row.contains("append beta"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "append_to_buffer_via_mx_inserts_region_at_target_point",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn prepend_to_buffer_via_mx_inserts_region_before_target_text() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "prepend-to-buffer-source.txt",
        "prepend alpha\nprepend beta\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x b");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"prepend-to-buffer-target");
    }
    send_both(&mut gnu, &mut neo, "RET");
    let target_ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("prepend-to-buffer-target"))
    };
    gnu.read_until(Duration::from_secs(6), target_ready);
    neo.read_until(Duration::from_secs(8), target_ready);
    for session in [&mut gnu, &mut neo] {
        session.send(b"target footer\n");
    }
    send_both(&mut gnu, &mut neo, "M-<");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x b");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"prepend-to-buffer-source.txt");
    }
    send_both(&mut gnu, &mut neo, "RET");
    gnu.read_until(Duration::from_secs(6), |grid| {
        grid.iter().any(|row| row.contains("prepend alpha"))
    });
    neo.read_until(Duration::from_secs(8), |grid| {
        grid.iter().any(|row| row.contains("prepend alpha"))
    });

    send_both(&mut gnu, &mut neo, "C-x h");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    invoke_mx_command(&mut gnu, &mut neo, "prepend-to-buffer");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Prepend to buffer:"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    for session in [&mut gnu, &mut neo] {
        session.send(b"prepend-to-buffer-target");
    }
    send_both(&mut gnu, &mut neo, "RET");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x b");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"prepend-to-buffer-target");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("prepend alpha"))
            && grid.iter().any(|row| row.contains("prepend beta"))
            && grid.iter().any(|row| row.contains("target footer"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "prepend_to_buffer_via_mx_inserts_region_before_target_text",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn insert_buffer_via_cx_x_i_inserts_named_buffer_contents() {
    let (mut gnu, mut neo) = boot_pair("");

    send_both(&mut gnu, &mut neo, "C-x b");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"insert-buffer-source");
    }
    send_both(&mut gnu, &mut neo, "RET");
    let source_ready =
        |grid: &[String]| grid.iter().any(|row| row.contains("insert-buffer-source"));
    gnu.read_until(Duration::from_secs(6), source_ready);
    neo.read_until(Duration::from_secs(8), source_ready);
    for session in [&mut gnu, &mut neo] {
        session.send(b"source alpha\nsource beta\n");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x b");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"insert-buffer-target");
    }
    send_both(&mut gnu, &mut neo, "RET");
    let target_ready =
        |grid: &[String]| grid.iter().any(|row| row.contains("insert-buffer-target"));
    gnu.read_until(Duration::from_secs(6), target_ready);
    neo.read_until(Duration::from_secs(8), target_ready);
    for session in [&mut gnu, &mut neo] {
        session.send(b"target header\n");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x x i");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Insert buffer:"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    for session in [&mut gnu, &mut neo] {
        session.send(b"insert-buffer-source");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("insert-buffer-target"))
            && grid.iter().any(|row| row.contains("target header"))
            && grid.iter().any(|row| row.contains("source alpha"))
            && grid.iter().any(|row| row.contains("source beta"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "insert_buffer_via_cx_x_i_inserts_named_buffer_contents",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn find_alternate_file_via_cx_cv_replaces_buffer() {
    let (mut gnu, mut neo) = boot_pair("");
    write_home_file(&gnu, "alternate-new.txt", "new alternate body\n");
    write_home_file(&neo, "alternate-new.txt", "new alternate body\n");
    open_home_file(
        &mut gnu,
        &mut neo,
        "alternate-old.txt",
        "old alternate body\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x C-v");
    let prompt_ready =
        |grid: &[String]| grid.iter().any(|row| row.contains("Find alternate file:"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "find_alternate_file_via_cx_cv_replaces_buffer/prompt",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "C-a C-k");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"~/alternate-new.txt");
    }
    let typed_path = |grid: &[String]| grid.iter().any(|row| row.contains("alternate-new.txt"));
    gnu.read_until(Duration::from_secs(6), typed_path);
    neo.read_until(Duration::from_secs(8), typed_path);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "find_alternate_file_via_cx_cv_replaces_buffer/before-ret",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alternate-new.txt"))
            && grid.iter().any(|row| row.contains("new alternate body"))
            && !grid.iter().any(|row| row.contains("old alternate body"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "find_alternate_file_via_cx_cv_replaces_buffer",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn copy_to_register_and_insert_register_via_cx_r_s_i() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "register-usage.txt",
        "alpha register\nbeta line\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-SPC C-e C-x r s");
    let copy_prompt = |grid: &[String]| grid.iter().any(|row| row.contains("Copy to register:"));
    gnu.read_until(Duration::from_secs(6), copy_prompt);
    neo.read_until(Duration::from_secs(8), copy_prompt);
    for session in [&mut gnu, &mut neo] {
        session.send(b"a");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "M-> C-x r i");
    let insert_prompt = |grid: &[String]| grid.iter().any(|row| row.contains("Insert register:"));
    gnu.read_until(Duration::from_secs(6), insert_prompt);
    neo.read_until(Duration::from_secs(8), insert_prompt);
    for session in [&mut gnu, &mut neo] {
        session.send(b"a");
    }

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("register-usage.txt"))
            && grid
                .iter()
                .filter(|row| row.contains("alpha register"))
                .count()
                >= 2
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "copy_to_register_and_insert_register_via_cx_r_s_i",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn kill_and_yank_rectangle_via_cx_r_k_y() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "rectangle-usage.txt",
        "aa11xx\nbb22yy\ncc33zz\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-f C-f C-SPC C-n C-n C-f C-f C-x r k");
    let killed = |grid: &[String]| {
        grid.iter().any(|row| row.contains("aaxx"))
            && grid.iter().any(|row| row.contains("bbyy"))
            && grid.iter().any(|row| row.contains("cczz"))
    };
    gnu.read_until(Duration::from_secs(6), killed);
    neo.read_until(Duration::from_secs(8), killed);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches("kill_and_yank_rectangle_via_cx_r_k_y/kill", &gnu, &neo, 2);

    send_both(&mut gnu, &mut neo, "C-x r y");
    let yanked = |grid: &[String]| {
        grid.iter().any(|row| row.contains("aa11xx"))
            && grid.iter().any(|row| row.contains("bb22yy"))
            && grid.iter().any(|row| row.contains("cc33zz"))
    };
    gnu.read_until(Duration::from_secs(6), yanked);
    neo.read_until(Duration::from_secs(8), yanked);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("kill_and_yank_rectangle_via_cx_r_k_y/yank", &gnu, &neo, 2);
}

#[test]
fn open_rectangle_via_cx_r_o_shifts_text_right() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "open-rectangle.txt",
        "ab12\ncd34\nef56\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-f C-f C-SPC C-n C-n C-f C-f C-x r o");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("ab  12"))
            && grid.iter().any(|row| row.contains("cd  34"))
            && grid.iter().any(|row| row.contains("ef  56"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("open_rectangle_via_cx_r_o_shifts_text_right", &gnu, &neo, 2);
    save_current_file_and_assert_contents(
        "open_rectangle_via_cx_r_o_shifts_text_right",
        &mut gnu,
        &mut neo,
        "open-rectangle.txt",
        "ab  12\ncd  34\nef  56\n",
    );
}

#[test]
fn clear_rectangle_via_cx_r_c_blanks_selected_columns() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "clear-rectangle.txt",
        "ab12zz\ncd34zz\nef56zz\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-f C-f C-SPC C-n C-n C-f C-f C-x r c");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("ab  zz"))
            && grid.iter().any(|row| row.contains("cd  zz"))
            && grid.iter().any(|row| row.contains("ef  zz"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "clear_rectangle_via_cx_r_c_blanks_selected_columns",
        &gnu,
        &neo,
        2,
    );
    save_current_file_and_assert_contents(
        "clear_rectangle_via_cx_r_c_blanks_selected_columns",
        &mut gnu,
        &mut neo,
        "clear-rectangle.txt",
        "ab  zz\ncd  zz\nef  zz\n",
    );
}

#[test]
fn delete_rectangle_via_cx_r_d_shifts_suffix_left() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "delete-rectangle.txt",
        "ab12zz\ncd34zz\nef56zz\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-f C-f C-SPC C-n C-n C-f C-f C-x r d");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("abzz"))
            && grid.iter().any(|row| row.contains("cdzz"))
            && grid.iter().any(|row| row.contains("efzz"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "delete_rectangle_via_cx_r_d_shifts_suffix_left",
        &gnu,
        &neo,
        2,
    );
    save_current_file_and_assert_contents(
        "delete_rectangle_via_cx_r_d_shifts_suffix_left",
        &mut gnu,
        &mut neo,
        "delete-rectangle.txt",
        "abzz\ncdzz\nefzz\n",
    );
}

#[test]
fn string_rectangle_via_cx_r_t_replaces_columns() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "string-rectangle.txt",
        "abcd 1\nefgh 2\nkeep 3\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-SPC C-n C-f C-f C-f C-f C-x r t");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("String rectangle"));
    gnu.read_until(Duration::from_secs(8), prompt_ready);
    neo.read_until(Duration::from_secs(10), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));

    for session in [&mut gnu, &mut neo] {
        session.send(b"BOX");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("BOX 1"))
            && grid.iter().any(|row| row.contains("BOX 2"))
            && grid.iter().any(|row| row.contains("keep 3"))
            && !grid.iter().any(|row| row.contains("abcd 1"))
            && !grid.iter().any(|row| row.contains("efgh 2"))
    };
    gnu.read_until(Duration::from_secs(8), ready);
    neo.read_until(Duration::from_secs(10), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "string_rectangle_via_cx_r_t_replaces_columns",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn rectangle_number_lines_via_cx_r_n_numbers_selected_lines() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "rectangle-number-lines.txt",
        "apple\nbanana\ncherry\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-SPC C-n C-n C-x r N");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("1 apple"))
            && grid.iter().any(|row| row.contains("2 banana"))
            && grid.iter().any(|row| row.contains("3 cherry"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "rectangle_number_lines_via_cx_r_n_numbers_selected_lines",
        &gnu,
        &neo,
        2,
    );
    save_current_file_and_assert_contents(
        "rectangle_number_lines_via_cx_r_n_numbers_selected_lines",
        &mut gnu,
        &mut neo,
        "rectangle-number-lines.txt",
        "1 apple\n2 banana\n3 cherry\n",
    );
}

#[test]
fn point_to_register_and_jump_to_register_via_cx_r_spc_j() {
    let (mut gnu, mut neo) = boot_pair("");
    let mut contents = String::new();
    for line in 1..=70 {
        contents.push_str(&format!("register jump line {line:02}\n"));
    }
    open_home_file(
        &mut gnu,
        &mut neo,
        "point-register.txt",
        &contents,
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x r SPC");
    let point_prompt = |grid: &[String]| grid.iter().any(|row| row.contains("Point to register:"));
    gnu.read_until(Duration::from_secs(6), point_prompt);
    neo.read_until(Duration::from_secs(8), point_prompt);
    for session in [&mut gnu, &mut neo] {
        session.send(b"a");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "M->");
    let at_end = |grid: &[String]| grid.iter().any(|row| row.contains("register jump line 70"));
    gnu.read_until(Duration::from_secs(6), at_end);
    neo.read_until(Duration::from_secs(8), at_end);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x r j");
    let jump_prompt = |grid: &[String]| grid.iter().any(|row| row.contains("Jump to register:"));
    gnu.read_until(Duration::from_secs(6), jump_prompt);
    neo.read_until(Duration::from_secs(8), jump_prompt);
    for session in [&mut gnu, &mut neo] {
        session.send(b"a");
    }

    let at_start = |grid: &[String]| grid.iter().any(|row| row.contains("register jump line 01"));
    gnu.read_until(Duration::from_secs(6), at_start);
    neo.read_until(Duration::from_secs(8), at_start);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "point_to_register_and_jump_to_register_via_cx_r_spc_j",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn bookmark_set_and_jump_via_cx_r_m_b() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "bookmark-jump.txt",
        "alpha line\nbeta line\ngamma line\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-n C-x r m");
    let set_prompt = |grid: &[String]| grid.iter().any(|row| row.contains("Set bookmark named"));
    gnu.read_until(Duration::from_secs(8), set_prompt);
    neo.read_until(Duration::from_secs(10), set_prompt);
    let gnu_grid = gnu.text_grid();
    let neo_grid = neo.text_grid();
    assert!(
        set_prompt(&gnu_grid),
        "GNU should prompt to set bookmark\n{}",
        gnu_grid.join("\n")
    );
    assert!(
        set_prompt(&neo_grid),
        "Neomacs should prompt to set bookmark\n{}",
        neo_grid.join("\n")
    );
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));

    for session in [&mut gnu, &mut neo] {
        session.send(b"jump-spot");
    }
    send_both(&mut gnu, &mut neo, "RET");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "M-< C-x r b");
    let jump_prompt = |grid: &[String]| grid.iter().any(|row| row.contains("Jump to bookmark"));
    gnu.read_until(Duration::from_secs(8), jump_prompt);
    neo.read_until(Duration::from_secs(10), jump_prompt);
    let gnu_grid = gnu.text_grid();
    let neo_grid = neo.text_grid();
    assert!(
        jump_prompt(&gnu_grid),
        "GNU should prompt to jump to bookmark\n{}",
        gnu_grid.join("\n")
    );
    assert!(
        jump_prompt(&neo_grid),
        "Neomacs should prompt to jump to bookmark\n{}",
        neo_grid.join("\n")
    );
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));

    for session in [&mut gnu, &mut neo] {
        session.send(b"jump-spot");
    }
    send_both(&mut gnu, &mut neo, "RET");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"X");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alpha line"))
            && grid.iter().any(|row| row.contains("Xbeta line"))
            && grid.iter().any(|row| row.contains("gamma line"))
    };
    gnu.read_until(Duration::from_secs(8), ready);
    neo.read_until(Duration::from_secs(10), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("bookmark_set_and_jump_via_cx_r_m_b", &gnu, &neo, 2);
}

#[test]
fn bookmark_list_via_cx_r_l_shows_saved_bookmark() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "bookmark-list.txt",
        "alpha line\nbeta line\ngamma line\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-n C-x r m");
    let set_prompt = |grid: &[String]| grid.iter().any(|row| row.contains("Set bookmark named"));
    gnu.read_until(Duration::from_secs(8), set_prompt);
    neo.read_until(Duration::from_secs(10), set_prompt);
    for session in [&mut gnu, &mut neo] {
        session.send(b"list-spot");
    }
    send_both(&mut gnu, &mut neo, "RET");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x r l");
    let list_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*Bookmark List*"))
            && grid.iter().any(|row| row.contains("Bookmark Name"))
            && grid.iter().any(|row| row.contains("list-spot"))
            && grid.iter().any(|row| row.contains("bookmark-list.txt"))
    };
    gnu.read_until(Duration::from_secs(8), list_ready);
    neo.read_until(Duration::from_secs(10), list_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    if !list_ready(&gnu.text_grid()) || !list_ready(&neo.text_grid()) {
        dump_pair_grids(
            "bookmark_list_via_cx_r_l_shows_saved_bookmark/open",
            &gnu,
            &neo,
        );
    }
    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains("*Bookmark List*")),
            "{label} should display the bookmark list buffer"
        );
        assert!(
            grid.iter().any(|row| row.contains("list-spot")),
            "{label} should list the newly created bookmark"
        );
    }
    assert_pair_nearly_matches(
        "bookmark_list_via_cx_r_l_shows_saved_bookmark/open",
        &gnu,
        &neo,
        4,
    );

    send_both_raw(&mut gnu, &mut neo, b"q");
    let source_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("bookmark-list.txt"))
            && grid.iter().any(|row| row.contains("beta line"))
    };
    gnu.read_until(Duration::from_secs(6), source_ready);
    neo.read_until(Duration::from_secs(8), source_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "bookmark_list_via_cx_r_l_shows_saved_bookmark/quit",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn file_name_shadow_overlay_does_not_leak_into_occur_prompt() {
    let (mut gnu, mut neo) = boot_pair("");

    send_both(&mut gnu, &mut neo, "C-x C-f");
    let file_prompt = |grid: &[String]| grid.iter().any(|row| row.contains("Find file:"));
    gnu.read_until(Duration::from_secs(6), file_prompt);
    neo.read_until(Duration::from_secs(8), file_prompt);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));

    for session in [&mut gnu, &mut neo] {
        session.send(b"/tmp//shadow-probe");
    }
    let shadow_ready = |grid: &[String]| grid.iter().any(|row| row.contains("}"));
    gnu.read_until(Duration::from_secs(6), shadow_ready);
    neo.read_until(Duration::from_secs(8), shadow_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));

    abort_minibuffer_and_wait_for_scratch(&mut gnu, &mut neo);

    send_both(&mut gnu, &mut neo, "M-s o");
    let occur_prompt = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("List lines matching regexp"))
    };
    gnu.read_until(Duration::from_secs(6), occur_prompt);
    neo.read_until(Duration::from_secs(8), occur_prompt);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));

    for (label, session) in [("GNU", &gnu), ("Neomacs", &neo)] {
        let prompt_row = session
            .text_grid()
            .into_iter()
            .find(|row| row.contains("List lines matching regexp"))
            .expect("occur prompt should be visible");
        assert!(
            !prompt_row.contains('{') && !prompt_row.contains('}'),
            "{label} occur prompt should not leak file-name shadow brackets\n{prompt_row}",
        );
    }

    assert_pair_nearly_matches(
        "file_name_shadow_overlay_does_not_leak_into_occur_prompt",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn occur_via_ms_o_lists_matching_lines() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "occur-usage.txt",
        "alpha needle one\nbeta plain\ngamma needle two\nneedle three\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-s o");
    let occur_prompt = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("List lines matching regexp"))
    };
    gnu.read_until(Duration::from_secs(6), occur_prompt);
    neo.read_until(Duration::from_secs(8), occur_prompt);
    for session in [&mut gnu, &mut neo] {
        session.send(b"needle");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*Occur*"))
            && grid.iter().any(|row| row.contains("3 matches"))
            && grid.iter().any(|row| row.contains("alpha needle one"))
            && grid.iter().any(|row| row.contains("gamma needle two"))
            && grid.iter().any(|row| row.contains("needle three"))
    };
    gnu.read_until(Duration::from_secs(8), ready);
    neo.read_until(Duration::from_secs(12), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("occur_via_ms_o_lists_matching_lines", &gnu, &neo, 2);
}

#[test]
fn revert_buffer_via_mx_rereads_file_from_disk() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "revert-usage.txt",
        "original disk line\n",
        "C-x C-f",
    );
    fs::write(
        gnu.home_dir().join("revert-usage.txt"),
        "updated disk line\n",
    )
    .expect("update GNU revert fixture");
    fs::write(
        neo.home_dir().join("revert-usage.txt"),
        "updated disk line\n",
    )
    .expect("update Neo revert fixture");

    send_both(&mut gnu, &mut neo, "M-x");
    let mx_prompt = |grid: &[String]| grid.last().is_some_and(|row| row.contains("M-x"));
    gnu.read_until(Duration::from_secs(6), mx_prompt);
    neo.read_until(Duration::from_secs(8), mx_prompt);
    for session in [&mut gnu, &mut neo] {
        session.send(b"revert-buffer");
    }
    send_both(&mut gnu, &mut neo, "RET");
    let revert_prompt = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Revert buffer from file"))
    };
    gnu.read_until(Duration::from_secs(6), revert_prompt);
    neo.read_until(Duration::from_secs(8), revert_prompt);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for session in [&mut gnu, &mut neo] {
        session.send(b"yes");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("revert-usage.txt"))
            && grid.iter().any(|row| row.contains("updated disk line"))
            && !grid.iter().any(|row| row.contains("original disk line"))
    };
    gnu.read_until(Duration::from_secs(8), ready);
    neo.read_until(Duration::from_secs(12), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("revert_buffer_via_mx_rereads_file_from_disk", &gnu, &neo, 2);
}

#[test]
fn dabbrev_expand_via_mslash_expands_previous_word() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "dabbrev-usage.txt",
        "dynamic-expansion\n\n dyn",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-> M-/");
    let ready = |grid: &[String]| {
        grid.iter()
            .filter(|row| row.contains("dynamic-expansion"))
            .count()
            >= 2
            && !grid.iter().any(|row| row.trim_end().ends_with(" dyn"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "dabbrev_expand_via_mslash_expands_previous_word",
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
fn isearch_repeat_forward_via_cs_cs_moves_to_next_match() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "isearch-repeat.txt",
        "needle first\nmiddle line\nneedle second\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-s");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("I-search"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));

    for session in [&mut gnu, &mut neo] {
        session.send(b"needle");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-s RET");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"X");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("needle first"))
            && grid.iter().any(|row| row.contains("needleX second"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "isearch_repeat_forward_via_cs_cs_moves_to_next_match",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn isearch_delete_char_recovers_from_failed_search() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "isearch-delete-char.txt",
        "alpha target\nomega line\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-s");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("I-search"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));

    for session in [&mut gnu, &mut neo] {
        session.send(b"targetx");
    }
    let failing_search = |grid: &[String]| grid.iter().any(|row| row.contains("Failing I-search"));
    gnu.read_until(Duration::from_secs(6), failing_search);
    neo.read_until(Duration::from_secs(8), failing_search);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));

    send_both(&mut gnu, &mut neo, "DEL RET");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"!");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alpha target!"))
            && grid.iter().any(|row| row.contains("omega line"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "isearch_delete_char_recovers_from_failed_search",
        &gnu,
        &neo,
        2,
    );
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
fn isearch_forward_word_via_ms_w_matches_words_across_whitespace() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "isearch-word.txt",
        "intro line\nalpha\n   beta target\nalpha-x beta miss\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-s w");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("word I-search"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    for session in [&mut gnu, &mut neo] {
        session.send(b"alpha beta");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "RET");
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));
    send_both_raw(&mut gnu, &mut neo, b"!");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("beta! target"))
            && grid.iter().any(|row| row.contains("alpha-x beta miss"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "isearch_forward_word_via_ms_w_matches_words_across_whitespace",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn isearch_forward_symbol_via_ms_underscore_respects_symbol_boundaries() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "isearch-symbol.el",
        "foo_bar\nfoo done\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-s _");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("symbol I-search"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    for session in [&mut gnu, &mut neo] {
        session.send(b"foo");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "RET");
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));
    send_both_raw(&mut gnu, &mut neo, b"!");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("foo_bar"))
            && grid.iter().any(|row| row.contains("foo! done"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "isearch_forward_symbol_via_ms_underscore_respects_symbol_boundaries",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn isearch_forward_regexp_via_cmeta_s_matches_pattern() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "isearch-regexp-forward.txt",
        "alpha 123 target\nalpha abc target\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-M-s");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Regexp I-search"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    for session in [&mut gnu, &mut neo] {
        session.send(br"alpha [0-9]+ target");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "RET");
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));
    send_both_raw(&mut gnu, &mut neo, b"!");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alpha 123! target"))
            && grid.iter().any(|row| row.contains("alpha abc target"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "isearch_forward_regexp_via_cmeta_s_matches_pattern",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn isearch_backward_regexp_via_cmeta_r_matches_previous_pattern() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "isearch-regexp-backward.txt",
        "item alpha\nplain middle\nitem beta\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-> C-M-r");
    let prompt_ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Regexp I-search backward"))
    };
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    for session in [&mut gnu, &mut neo] {
        session.send(br"item .+");
    }
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("item alpha"))
            && grid.iter().any(|row| row.contains("item beta"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "isearch_backward_regexp_via_cmeta_r_matches_previous_pattern",
        &gnu,
        &neo,
        2,
    );
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
fn forward_and_backward_sexp_via_cmeta_f_b() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "sexp-motion.el",
        "(alpha beta) gamma\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-M-f");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"X");

    let forward_ready =
        |grid: &[String]| grid.iter().any(|row| row.contains("(alpha beta)X gamma"));
    gnu.read_until(Duration::from_secs(6), forward_ready);
    neo.read_until(Duration::from_secs(8), forward_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "DEL C-M-b");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"^");

    let backward_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("^(alpha beta) gamma"))
            && !grid.iter().any(|row| row.contains("(alpha beta)X gamma"))
    };
    gnu.read_until(Duration::from_secs(6), backward_ready);
    neo.read_until(Duration::from_secs(8), backward_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("forward_and_backward_sexp_via_cmeta_f_b", &gnu, &neo, 2);
}

#[test]
fn kill_sexp_via_cmeta_k_kills_following_expression() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "kill-sexp.el",
        "(alpha beta) gamma\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-M-k");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"!");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("! gamma"))
            && !grid.iter().any(|row| row.contains("(alpha beta) gamma"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "kill_sexp_via_cmeta_k_kills_following_expression",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn mark_sexp_via_cmeta_spc_then_kill_region() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "mark-sexp.el",
        "(alpha beta) gamma\n",
        "C-x C-f",
    );

    // C-M-SPC is ESC followed by C-@ (NUL) in a terminal.
    send_both_raw(&mut gnu, &mut neo, b"\x1b\x00");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-w");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"!");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("! gamma"))
            && !grid.iter().any(|row| row.contains("(alpha beta) gamma"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("mark_sexp_via_cmeta_spc_then_kill_region", &gnu, &neo, 2);
}

#[test]
fn transpose_sexps_via_cmeta_t_swaps_adjacent_expressions() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "transpose-sexps.el",
        "(foo) (bar)\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-< C-M-f C-M-t");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("(bar) (foo)"))
            && !grid.iter().any(|row| row.contains("(foo) (bar)"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "transpose_sexps_via_cmeta_t_swaps_adjacent_expressions",
        &gnu,
        &neo,
        2,
    );
    save_current_file_and_assert_contents(
        "transpose_sexps_via_cmeta_t_swaps_adjacent_expressions",
        &mut gnu,
        &mut neo,
        "transpose-sexps.el",
        "(bar) (foo)\n",
    );
}

#[test]
fn down_list_and_backward_up_list_via_cmeta_d_u() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "list-depth.el",
        "(outer (inner item) tail)\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-M-d");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"^");

    let down_ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("(^outer (inner item) tail)"))
    };
    gnu.read_until(Duration::from_secs(6), down_ready);
    neo.read_until(Duration::from_secs(8), down_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "DEL C-M-u");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"!");

    let up_ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("!(outer (inner item) tail)"))
            && !grid
                .iter()
                .any(|row| row.contains("(^outer (inner item) tail)"))
    };
    gnu.read_until(Duration::from_secs(6), up_ready);
    neo.read_until(Duration::from_secs(8), up_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "down_list_and_backward_up_list_via_cmeta_d_u",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn forward_and_backward_list_via_cmeta_n_p() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "list-motion.el",
        "(one (nested)) (two)\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-M-n");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"^");

    let forward_ready =
        |grid: &[String]| grid.iter().any(|row| row.contains("(one (nested))^ (two)"));
    gnu.read_until(Duration::from_secs(6), forward_ready);
    neo.read_until(Duration::from_secs(8), forward_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "DEL C-M-p");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"!");

    let backward_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("!(one (nested)) (two)"))
            && !grid.iter().any(|row| row.contains("(one (nested))^ (two)"))
    };
    gnu.read_until(Duration::from_secs(6), backward_ready);
    neo.read_until(Duration::from_secs(8), backward_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("forward_and_backward_list_via_cmeta_n_p", &gnu, &neo, 2);
}

#[test]
fn insert_parentheses_via_m_open_paren() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "insert-parentheses.el",
        "alpha\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-(");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"beta");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("(beta) alpha"))
            && !grid.iter().any(|row| row.contains("betaalpha"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("insert_parentheses_via_m_open_paren", &gnu, &neo, 2);
}

#[test]
fn move_past_close_and_reindent_via_m_close_paren() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "move-past-close.el",
        "(progn\n  (message \"x\")\n  )tail\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-< C-n C-n M-)");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("(progn"))
            && grid.iter().any(|row| row.contains("  (message \"x\"))"))
            && grid.iter().any(|row| row.contains("tail"))
            && !grid.iter().any(|row| row.contains("  )tail"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "move_past_close_and_reindent_via_m_close_paren",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn beginning_and_end_of_defun_via_cmeta_a_e() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "defun-motion.el",
        "(defun first ()\n  1)\n\n(defun second ()\n  2)\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-> C-M-a");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"^");

    let beginning_ready =
        |grid: &[String]| grid.iter().any(|row| row.contains("^(defun second ()"));
    gnu.read_until(Duration::from_secs(6), beginning_ready);
    neo.read_until(Duration::from_secs(8), beginning_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "DEL C-M-e");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"!");

    let end_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("(defun first ()"))
            && grid.iter().any(|row| row.contains("(defun second ()"))
            && grid.iter().any(|row| row.contains("  2)!"))
    };
    gnu.read_until(Duration::from_secs(6), end_ready);
    neo.read_until(Duration::from_secs(8), end_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("beginning_and_end_of_defun_via_cmeta_a_e", &gnu, &neo, 2);
}

#[test]
fn mark_defun_via_cmeta_h_then_kill_region() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "mark-defun.el",
        "(defun first ()\n  1)\n\n(defun second ()\n  2)\n\nafter\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-> C-M-a C-M-h");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-w");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"!");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("(defun first ()"))
            && grid.iter().any(|row| row.contains("!"))
            && grid.iter().any(|row| row.contains("after"))
            && !grid.iter().any(|row| row.contains("defun second"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("mark_defun_via_cmeta_h_then_kill_region", &gnu, &neo, 2);
}

#[test]
fn narrow_to_defun_once_then_widen_via_cx_n_d_w() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "narrow-defun.el",
        "(defun first ()\n  1)\n\n(defun second ()\n  2)\n\nafter\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-> C-M-a C-n C-x n d");
    let narrowed_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("(defun second ()"))
            && grid.iter().any(|row| row.contains("  2)"))
            && !grid.iter().any(|row| row.contains("(defun first ()"))
            && !grid.iter().any(|row| row.contains("after"))
    };
    gnu.read_until(Duration::from_secs(8), narrowed_ready);
    neo.read_until(Duration::from_secs(12), narrowed_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "narrow_to_defun_once_then_widen_via_cx_n_d_w/narrowed",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "C-x n w M-<");
    let widened_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("(defun first ()"))
            && grid.iter().any(|row| row.contains("(defun second ()"))
            && grid.iter().any(|row| row.contains("after"))
    };
    gnu.read_until(Duration::from_secs(6), widened_ready);
    neo.read_until(Duration::from_secs(8), widened_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "narrow_to_defun_once_then_widen_via_cx_n_d_w",
        &gnu,
        &neo,
        2,
    );
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
fn split_line_via_cmeta_o_moves_tail_down() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "split-line.txt",
        "alpha beta gamma\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-< M-f C-M-o");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alpha"))
            && grid.iter().any(|row| row.contains("      beta gamma"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("split_line_via_cmeta_o_moves_tail_down", &gnu, &neo, 2);
    save_current_file_and_assert_contents(
        "split_line_via_cmeta_o_moves_tail_down",
        &mut gnu,
        &mut neo,
        "split-line.txt",
        "alpha \n      beta gamma\n",
    );
}

#[test]
fn tab_to_tab_stop_via_mi_inserts_to_next_tab_column() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(&mut gnu, &mut neo, "tab-to-tab-stop.txt", "ab\n", "C-x C-f");

    send_both(&mut gnu, &mut neo, "C-f C-f M-i");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for session in [&mut gnu, &mut neo] {
        session.send(b"cd");
    }

    let ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("ab") && row.contains("cd"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "tab_to_tab_stop_via_mi_inserts_to_next_tab_column",
        &gnu,
        &neo,
        2,
    );
    save_current_file_and_assert_contents(
        "tab_to_tab_stop_via_mi_inserts_to_next_tab_column",
        &mut gnu,
        &mut neo,
        "tab-to-tab-stop.txt",
        "ab\tcd\n",
    );
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
fn newline_and_indent_via_cj_trims_trailing_space_and_indents() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "newline-and-indent.el",
        "(let ((alpha 1))   beta)\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-f M-f");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-j");
    for session in [&mut gnu, &mut neo] {
        session.send(b"gamma");
    }

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("(let ((alpha 1))"))
            && grid.iter().any(|row| row.contains("gamma   beta)"))
            && !grid.iter().any(|row| row.contains("1))   "))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "newline_and_indent_via_cj_trims_trailing_space_and_indents",
        &gnu,
        &neo,
        2,
    );
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
fn auto_fill_mode_wraps_inserted_text_after_fill_column() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(&mut gnu, &mut neo, "auto-fill.txt", "seed\n", "C-x C-f");
    send_both(&mut gnu, &mut neo, "C-x h C-w");
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));

    send_both(&mut gnu, &mut neo, "C-x f");
    let fill_prompt = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Change fill-column from"))
    };
    gnu.read_until(Duration::from_secs(6), fill_prompt);
    neo.read_until(Duration::from_secs(8), fill_prompt);
    for session in [&mut gnu, &mut neo] {
        session.send(b"20");
    }
    send_both(&mut gnu, &mut neo, "RET");
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));

    invoke_mx_command(&mut gnu, &mut neo, "auto-fill-mode");
    let enabled = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Auto-Fill mode enabled"))
    };
    gnu.read_until(Duration::from_secs(6), enabled);
    neo.read_until(Duration::from_secs(8), enabled);
    assert!(
        enabled(&gnu.text_grid()),
        "GNU should report Auto Fill mode enabled\n{}",
        gnu.text_grid().join("\n")
    );
    assert!(
        enabled(&neo.text_grid()),
        "Neomacs should report Auto Fill mode enabled\n{}",
        neo.text_grid().join("\n")
    );
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));

    for session in [&mut gnu, &mut neo] {
        session.send(b"alpha beta gamma delta epsilon zeta ");
    }

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alpha beta gamma"))
            && grid.iter().any(|row| row.contains("delta epsilon zeta"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    assert!(
        ready(&gnu.text_grid()),
        "GNU should auto-fill inserted text\n{}",
        gnu.text_grid().join("\n")
    );
    assert!(
        ready(&neo.text_grid()),
        "Neomacs should auto-fill inserted text\n{}",
        neo.text_grid().join("\n")
    );
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "auto_fill_mode_wraps_inserted_text_after_fill_column",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn set_fill_prefix_then_fill_paragraph_via_cx_dot_mq() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "fill-prefix.txt",
        "> alpha beta gamma delta epsilon zeta eta theta iota kappa lambda\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x f");
    let fill_prompt = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Change fill-column from"))
    };
    gnu.read_until(Duration::from_secs(6), fill_prompt);
    neo.read_until(Duration::from_secs(8), fill_prompt);
    for session in [&mut gnu, &mut neo] {
        session.send(b"26");
    }
    send_both(&mut gnu, &mut neo, "RET");
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));

    send_both(&mut gnu, &mut neo, "M-< C-f C-f C-x .");
    let prefix_ready = |grid: &[String]| grid.iter().any(|row| row.contains("fill-prefix:"));
    gnu.read_until(Duration::from_secs(6), prefix_ready);
    neo.read_until(Duration::from_secs(8), prefix_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));

    send_both(&mut gnu, &mut neo, "M-q");
    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("> alpha beta gamma"))
            && grid.iter().any(|row| row.contains("> delta epsilon zeta"))
            && grid
                .iter()
                .any(|row| row.contains("> eta theta iota kappa"))
            && grid.iter().any(|row| row.contains("> lambda"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "set_fill_prefix_then_fill_paragraph_via_cx_dot_mq",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn not_modified_via_mtilde_prevents_next_save_from_writing_edit() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "not-modified.txt",
        "original text\n",
        "C-x C-f",
    );

    send_both_raw(&mut gnu, &mut neo, b"changed ");
    let edited = |grid: &[String]| grid.iter().any(|row| row.contains("changed original text"));
    gnu.read_until(Duration::from_secs(6), edited);
    neo.read_until(Duration::from_secs(8), edited);

    send_both(&mut gnu, &mut neo, "M-~ C-x C-s");
    let saved = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("No changes need to be saved"))
    };
    gnu.read_until(Duration::from_secs(6), saved);
    neo.read_until(Duration::from_secs(8), saved);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "not_modified_via_mtilde_prevents_next_save_from_writing_edit",
        &gnu,
        &neo,
        2,
    );
    assert_home_file_contents(&gnu, &neo, "not-modified.txt", "original text\n");
}

#[test]
fn center_line_via_mx_uses_fill_column() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(&mut gnu, &mut neo, "center-line.txt", "title\n", "C-x C-f");

    send_both(&mut gnu, &mut neo, "C-x f");
    let prompt_ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Change fill-column from"))
    };
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    for session in [&mut gnu, &mut neo] {
        session.send(b"20");
    }
    send_both(&mut gnu, &mut neo, "RET");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "M-<");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    invoke_mx_command(&mut gnu, &mut neo, "center-line");

    let ready = |grid: &[String]| grid.iter().any(|row| row.starts_with("       title"));
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("center_line_via_mx_uses_fill_column", &gnu, &neo, 2);
    save_current_file_and_assert_contents(
        "center_line_via_mx_uses_fill_column",
        &mut gnu,
        &mut neo,
        "center-line.txt",
        "       title\n",
    );
}

#[test]
fn fill_region_via_mx_wraps_active_region() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "fill-region.txt",
        "alpha beta gamma delta epsilon zeta eta theta\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x f");
    let prompt_ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Change fill-column from"))
    };
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    for session in [&mut gnu, &mut neo] {
        session.send(b"20");
    }
    send_both(&mut gnu, &mut neo, "RET");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x h");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    invoke_mx_command(&mut gnu, &mut neo, "fill-region");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.trim() == "alpha beta gamma")
            && grid.iter().any(|row| row.trim() == "delta epsilon zeta")
            && grid.iter().any(|row| row.trim() == "eta theta")
            && !grid
                .iter()
                .any(|row| row.contains("alpha beta gamma delta"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("fill_region_via_mx_wraps_active_region", &gnu, &neo, 2);
    save_current_file_and_assert_contents(
        "fill_region_via_mx_wraps_active_region",
        &mut gnu,
        &mut neo,
        "fill-region.txt",
        "alpha beta gamma\ndelta epsilon zeta\neta theta\n",
    );
}

#[test]
fn set_variable_fill_column_via_mx_updates_buffer_local_value() {
    let (mut gnu, mut neo) = boot_pair("");

    invoke_mx_command(&mut gnu, &mut neo, "set-variable");
    let variable_prompt = |grid: &[String]| grid.iter().any(|row| row.contains("Set variable"));
    gnu.read_until(Duration::from_secs(6), variable_prompt);
    neo.read_until(Duration::from_secs(8), variable_prompt);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "set_variable_fill_column_via_mx_updates_buffer_local_value/variable_prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"fill-column");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let value_prompt = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Set fill-column") && row.contains("to value"))
    };
    gnu.read_until(Duration::from_secs(6), value_prompt);
    neo.read_until(Duration::from_secs(8), value_prompt);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter()
                .any(|row| row.contains("Set fill-column") && row.contains("to value")),
            "{label} should prompt for fill-column's new value\n{}",
            grid.join("\n")
        );
    }

    for session in [&mut gnu, &mut neo] {
        session.send(b"55");
    }
    send_both(&mut gnu, &mut neo, "RET");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "M-:");
    let eval_prompt = |grid: &[String]| grid.iter().any(|row| row.contains("Eval:"));
    gnu.read_until(Duration::from_secs(6), eval_prompt);
    neo.read_until(Duration::from_secs(8), eval_prompt);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    for session in [&mut gnu, &mut neo] {
        session.send(br#"(message "fill-column=%S" fill-column)"#);
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter()
            .rev()
            .take(4)
            .any(|row| row.contains("fill-column=55"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "set_variable_fill_column_via_mx_updates_buffer_local_value",
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
fn keyboard_macro_record_and_replay_via_cx_parens_cx_e() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "keyboard-macro.txt",
        "one\ntwo\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x (");
    let recording_ready =
        |grid: &[String]| grid.iter().any(|row| row.contains("Defining kbd macro"));
    gnu.read_until(Duration::from_secs(6), recording_ready);
    neo.read_until(Duration::from_secs(8), recording_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));

    send_both_raw(&mut gnu, &mut neo, b"\x05!");
    send_both(&mut gnu, &mut neo, "C-n C-a C-x )");
    let macro_defined = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Keyboard macro defined") || row.contains("End defining"))
    };
    gnu.read_until(Duration::from_secs(6), macro_defined);
    neo.read_until(Duration::from_secs(8), macro_defined);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x e");
    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("one!")) && grid.iter().any(|row| row.contains("two!"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "keyboard_macro_record_and_replay_via_cx_parens_cx_e",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn keyboard_macro_repeats_via_trailing_e_after_cx_e() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "keyboard-macro-repeat.txt",
        "one\ntwo\nthree\nfour\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x (");
    let recording_ready =
        |grid: &[String]| grid.iter().any(|row| row.contains("Defining kbd macro"));
    gnu.read_until(Duration::from_secs(6), recording_ready);
    neo.read_until(Duration::from_secs(8), recording_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));

    send_both_raw(&mut gnu, &mut neo, b"\x05!");
    send_both(&mut gnu, &mut neo, "C-n C-a C-x )");
    let macro_defined = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Keyboard macro defined") || row.contains("End defining"))
    };
    gnu.read_until(Duration::from_secs(6), macro_defined);
    neo.read_until(Duration::from_secs(8), macro_defined);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x e e");
    let repeated = |grid: &[String]| {
        grid.iter().any(|row| row.contains("one!"))
            && grid.iter().any(|row| row.contains("two!"))
            && grid.iter().any(|row| row.contains("three!"))
    };
    gnu.read_until(Duration::from_secs(6), repeated);
    neo.read_until(Duration::from_secs(8), repeated);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both_raw(&mut gnu, &mut neo, b"X");
    let inserted = |grid: &[String]| grid.iter().any(|row| row.contains("Xfour"));
    gnu.read_until(Duration::from_secs(6), inserted);
    neo.read_until(Duration::from_secs(8), inserted);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "keyboard_macro_repeats_via_trailing_e_after_cx_e",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn transpose_words_via_mt() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "transpose-words.txt",
        "alpha beta gamma\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-f");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "M-t");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("beta alpha gamma"))
            && !grid.iter().any(|row| row.contains("alpha beta gamma"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("transpose_words_via_mt", &gnu, &neo, 2);
}

#[test]
fn transpose_lines_via_cx_ct() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "transpose-lines.txt",
        "alpha line\nbeta line\ngamma line\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-n");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-x C-t");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("beta line"))
            && grid.iter().any(|row| row.contains("alpha line"))
            && grid.iter().any(|row| row.contains("gamma line"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("transpose_lines_via_cx_ct", &gnu, &neo, 2);
}

#[test]
fn just_one_space_via_mspc() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "just-one-space.txt",
        "alpha   beta gamma\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-f");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "M-SPC");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alpha beta gamma"))
            && !grid.iter().any(|row| row.contains("alpha   beta gamma"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("just_one_space_via_mspc", &gnu, &neo, 2);
}

#[test]
fn cycle_spacing_via_repeated_mspc_collapses_deletes_and_restores() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "cycle-spacing.txt",
        "alpha   beta\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-f");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "M-SPC");
    let collapsed = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alpha beta"))
            && !grid.iter().any(|row| row.contains("alpha   beta"))
    };
    gnu.read_until(Duration::from_secs(6), collapsed);
    neo.read_until(Duration::from_secs(8), collapsed);
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));

    send_both(&mut gnu, &mut neo, "M-SPC");
    let deleted = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alphabeta"))
            && !grid.iter().any(|row| row.contains("alpha beta"))
    };
    gnu.read_until(Duration::from_secs(6), deleted);
    neo.read_until(Duration::from_secs(8), deleted);
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));

    send_both(&mut gnu, &mut neo, "M-SPC");
    let restored = |grid: &[String]| grid.iter().any(|row| row.contains("alpha   beta"));
    gnu.read_until(Duration::from_secs(6), restored);
    neo.read_until(Duration::from_secs(8), restored);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "cycle_spacing_via_repeated_mspc_collapses_deletes_and_restores",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn delete_horizontal_space_via_mbackslash() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "delete-horizontal-space.txt",
        "alpha   beta gamma\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-f");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "M-\\");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alphabeta gamma"))
            && !grid.iter().any(|row| row.contains("alpha   beta gamma"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("delete_horizontal_space_via_mbackslash", &gnu, &neo, 2);
}

#[test]
fn delete_blank_lines_after_current_line_via_cx_co() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "delete-blank-lines.txt",
        "alpha\n\n\nbeta\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x C-o");

    let ready = |grid: &[String]| {
        grid.windows(2)
            .any(|rows| rows[0].contains("alpha") && rows[1].contains("beta"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "delete_blank_lines_after_current_line_via_cx_co",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn delete_trailing_whitespace_via_mx_cleans_buffer_before_save() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "delete-trailing-whitespace.txt",
        "alpha   \nbeta\t \n\n\n",
        "C-x C-f",
    );

    invoke_mx_command(&mut gnu, &mut neo, "delete-trailing-whitespace");
    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alpha"))
            && grid.iter().any(|row| row.contains("beta"))
            && grid
                .iter()
                .any(|row| row.contains("delete-trailing-whitespace.txt"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    save_current_file_and_assert_contents(
        "delete_trailing_whitespace_via_mx_cleans_buffer_before_save",
        &mut gnu,
        &mut neo,
        "delete-trailing-whitespace.txt",
        "alpha\nbeta\n",
    );
    assert_pair_nearly_matches(
        "delete_trailing_whitespace_via_mx_cleans_buffer_before_save",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn untabify_region_via_mx_expands_tabs_before_save() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "untabify-region.txt",
        "a\tb\n\tindent\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x h");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    invoke_mx_command(&mut gnu, &mut neo, "untabify");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("a       b"))
            && grid.iter().any(|row| row.contains("        indent"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    save_current_file_and_assert_contents(
        "untabify_region_via_mx_expands_tabs_before_save",
        &mut gnu,
        &mut neo,
        "untabify-region.txt",
        "a       b\n        indent\n",
    );
    assert_pair_nearly_matches(
        "untabify_region_via_mx_expands_tabs_before_save",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn tabify_region_via_mx_converts_spaces_before_save() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "tabify-region.txt",
        "a       b\n        indent\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x h");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    invoke_mx_command(&mut gnu, &mut neo, "tabify");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("a       b"))
            && grid.iter().any(|row| row.contains("        indent"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    save_current_file_and_assert_contents(
        "tabify_region_via_mx_converts_spaces_before_save",
        &mut gnu,
        &mut neo,
        "tabify-region.txt",
        "a\tb\n\tindent\n",
    );
    assert_pair_nearly_matches(
        "tabify_region_via_mx_converts_spaces_before_save",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn indent_rigidly_with_prefix_via_mx_indents_region() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "indent-rigidly.txt",
        "alpha\nbeta\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x h C-u 4");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    invoke_mx_command(&mut gnu, &mut neo, "indent-rigidly");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("    alpha"))
            && grid.iter().any(|row| row.contains("    beta"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "indent_rigidly_with_prefix_via_mx_indents_region",
        &gnu,
        &neo,
        2,
    );
    save_current_file_and_assert_contents(
        "indent_rigidly_with_prefix_via_mx_indents_region",
        &mut gnu,
        &mut neo,
        "indent-rigidly.txt",
        "    alpha\n    beta\n",
    );
}

#[test]
fn indent_region_elisp_via_cmeta_backslash() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "indent-region.el",
        "(defun sample ()\n(message \"alpha\")\n(when t\n(message \"beta\")))\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x h");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    // C-M-\ is ESC followed by C-\ (0x1c).
    send_both_raw(&mut gnu, &mut neo, b"\x1b\x1c");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("  (message \"alpha\")"))
            && grid.iter().any(|row| row.contains("  (when t"))
            && grid
                .iter()
                .any(|row| row.contains("    (message \"beta\")"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    save_current_file_and_assert_contents(
        "indent_region_elisp_via_cmeta_backslash",
        &mut gnu,
        &mut neo,
        "indent-region.el",
        "(defun sample ()\n  (message \"alpha\")\n  (when t\n    (message \"beta\")))\n",
    );
    assert_pair_nearly_matches("indent_region_elisp_via_cmeta_backslash", &gnu, &neo, 2);
}

#[test]
fn eval_last_sexp_via_cx_ce_prints_echo_area_value() {
    let (mut gnu, mut neo) = boot_pair("");

    send_both_raw(&mut gnu, &mut neo, b"(+ 40 2)");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-x C-e");

    let ready = |grid: &[String]| grid.iter().rev().take(4).any(|row| row.contains("42"));
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains("(+ 40 2)")),
            "{label} should keep the evaluated sexp in the buffer"
        );
        assert!(
            grid.iter().rev().take(4).any(|row| row.contains("42")),
            "{label} should show eval-last-sexp's value in the echo area"
        );
    }
    assert_pair_nearly_matches(
        "eval_last_sexp_via_cx_ce_prints_echo_area_value",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn eval_last_sexp_error_via_cx_ce_opens_backtrace() {
    let (mut gnu, mut neo) = boot_pair("");

    send_both_raw(&mut gnu, &mut neo, b"hello");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-x C-e");

    gnu.read_until(Duration::from_secs(6), backtrace_ready);
    neo.read_until(Duration::from_secs(8), backtrace_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    if !backtrace_ready(&gnu.text_grid()) || !backtrace_ready(&neo.text_grid()) {
        dump_pair_grids("eval_last_sexp_error_via_cx_ce_opens_backtrace", &gnu, &neo);
    }

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains("*Backtrace*")),
            "{label} should display the Backtrace buffer"
        );
        assert!(
            grid.iter().any(|row| row.contains("Debugger entered")),
            "{label} should show debugger entry text"
        );
        assert!(
            grid.iter().any(|row| row.contains("hello")),
            "{label} should show the void variable in the backtrace"
        );
    }
    assert_pair_nearly_matches(
        "eval_last_sexp_error_via_cx_ce_opens_backtrace",
        &gnu,
        &neo,
        4,
    );
}

#[test]
fn eval_expression_via_mcolon_prints_echo_area_value() {
    let (mut gnu, mut neo) = boot_pair("");

    send_both(&mut gnu, &mut neo, "M-:");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Eval:"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "eval_expression_via_mcolon_prints_echo_area_value/prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"(+ 2 3)");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter()
            .rev()
            .take(4)
            .any(|row| row.contains("5 (#o5, #x5"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter()
                .rev()
                .take(4)
                .any(|row| row.contains("5 (#o5, #x5")),
            "{label} should show eval-expression's integer value formats"
        );
    }
    assert_pair_nearly_matches(
        "eval_expression_via_mcolon_prints_echo_area_value",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn eval_expression_history_via_mcolon_mp_recalls_previous_expression() {
    let (mut gnu, mut neo) = boot_pair("");

    send_both(&mut gnu, &mut neo, "M-:");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Eval:"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "eval_expression_history_via_mcolon_mp_recalls_previous_expression/prompt-1",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"(+ 1 2)");
    }
    let first_expr_typed =
        |grid: &[String]| grid.last().is_some_and(|row| row.contains("Eval: (+ 1 2)"));
    gnu.read_until(Duration::from_secs(6), first_expr_typed);
    neo.read_until(Duration::from_secs(8), first_expr_typed);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "eval_expression_history_via_mcolon_mp_recalls_previous_expression/typed-1",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "RET");
    let first_result = |grid: &[String]| grid.iter().rev().take(4).any(|row| row.contains("3"));
    gnu.read_until(Duration::from_secs(6), first_result);
    neo.read_until(Duration::from_secs(8), first_result);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "eval_expression_history_via_mcolon_mp_recalls_previous_expression/result-1",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "M-:");
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "eval_expression_history_via_mcolon_mp_recalls_previous_expression/prompt-2",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "M-p");
    let recalled = |grid: &[String]| grid.last().is_some_and(|row| row.contains("Eval: (+ 1 2)"));
    gnu.read_until(Duration::from_secs(6), recalled);
    neo.read_until(Duration::from_secs(8), recalled);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "eval_expression_history_via_mcolon_mp_recalls_previous_expression/recalled",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "DEL DEL");
    send_both_raw(&mut gnu, &mut neo, b"5)");
    let edited = |grid: &[String]| grid.last().is_some_and(|row| row.contains("Eval: (+ 1 5)"));
    gnu.read_until(Duration::from_secs(6), edited);
    neo.read_until(Duration::from_secs(8), edited);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "eval_expression_history_via_mcolon_mp_recalls_previous_expression/edited",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "RET");
    let second_result = |grid: &[String]| grid.iter().rev().take(4).any(|row| row.contains("6"));
    gnu.read_until(Duration::from_secs(6), second_result);
    neo.read_until(Duration::from_secs(8), second_result);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "eval_expression_history_via_mcolon_mp_recalls_previous_expression/result-2",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn eval_expression_error_via_mcolon_opens_backtrace() {
    let (mut gnu, mut neo) = boot_pair("");

    send_both(&mut gnu, &mut neo, "M-:");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Eval:"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "eval_expression_error_via_mcolon_opens_backtrace/prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"missing-variable");
    }
    send_both(&mut gnu, &mut neo, "RET");

    gnu.read_until(Duration::from_secs(6), backtrace_ready);
    neo.read_until(Duration::from_secs(8), backtrace_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    if !backtrace_ready(&gnu.text_grid()) || !backtrace_ready(&neo.text_grid()) {
        dump_pair_grids(
            "eval_expression_error_via_mcolon_opens_backtrace",
            &gnu,
            &neo,
        );
    }

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains("*Backtrace*")),
            "{label} should display the Backtrace buffer"
        );
        assert!(
            grid.iter().any(|row| row.contains("Debugger entered")),
            "{label} should show debugger entry text"
        );
        assert!(
            grid.iter().any(|row| row.contains("missing-variable")),
            "{label} should show the void variable in the backtrace"
        );
    }
    assert_pair_nearly_matches(
        "eval_expression_error_via_mcolon_opens_backtrace",
        &gnu,
        &neo,
        4,
    );
}

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
fn occur_next_error_via_mg_n_visits_match() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "occur-next-error.txt",
        "alpha needle one\nbeta plain\ngamma needle two\nneedle three\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-s o");
    let occur_prompt = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("List lines matching regexp"))
    };
    gnu.read_until(Duration::from_secs(6), occur_prompt);
    neo.read_until(Duration::from_secs(8), occur_prompt);
    for session in [&mut gnu, &mut neo] {
        session.send(b"needle");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let occur_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*Occur*"))
            && grid.iter().any(|row| row.contains("3 matches"))
            && grid.iter().any(|row| row.contains("alpha needle one"))
            && grid.iter().any(|row| row.contains("gamma needle two"))
            && grid.iter().any(|row| row.contains("needle three"))
    };
    gnu.read_until(Duration::from_secs(8), occur_ready);
    neo.read_until(Duration::from_secs(12), occur_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "M-g n");
    let jumped_to_source = |grid: &[String]| {
        !grid.iter().any(|row| row.contains("*Occur*"))
            && grid.iter().any(|row| row.contains("alpha needle one"))
            && grid.iter().any(|row| row.contains("beta plain"))
    };
    gnu.read_until(Duration::from_secs(6), jumped_to_source);
    neo.read_until(Duration::from_secs(8), jumped_to_source);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both_raw(&mut gnu, &mut neo, b"X");
    let edited_ready = |grid: &[String]| {
        !grid.iter().any(|row| row.contains("*Occur*"))
            && grid.iter().any(|row| row.contains("beta plain"))
            && grid.iter().any(|row| row.contains("Xneedle"))
    };
    gnu.read_until(Duration::from_secs(6), edited_ready);
    neo.read_until(Duration::from_secs(8), edited_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    if !edited_ready(&gnu.text_grid()) || !edited_ready(&neo.text_grid()) {
        dump_pair_grids("occur_next_error_via_mg_n_visits_match", &gnu, &neo);
    }

    assert_pair_nearly_matches("occur_next_error_via_mg_n_visits_match", &gnu, &neo, 3);
}

#[test]
fn occur_next_and_previous_error_via_cx_backtick_mg_p() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "occur-prev-error.txt",
        "needle first\nplain middle\nneedle second\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-s o");
    let occur_prompt = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("List lines matching regexp"))
    };
    gnu.read_until(Duration::from_secs(6), occur_prompt);
    neo.read_until(Duration::from_secs(8), occur_prompt);
    for session in [&mut gnu, &mut neo] {
        session.send(b"needle");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let occur_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*Occur*"))
            && grid.iter().any(|row| row.contains("2 matches"))
            && grid.iter().any(|row| row.contains("needle first"))
            && grid.iter().any(|row| row.contains("needle second"))
    };
    gnu.read_until(Duration::from_secs(8), occur_ready);
    neo.read_until(Duration::from_secs(12), occur_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-x `");
    let first_match = |grid: &[String]| {
        !grid.iter().any(|row| row.contains("*Occur*"))
            && grid.iter().any(|row| row.contains("needle first"))
            && grid.iter().any(|row| row.contains("plain middle"))
    };
    gnu.read_until(Duration::from_secs(6), first_match);
    neo.read_until(Duration::from_secs(8), first_match);
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));

    send_both(&mut gnu, &mut neo, "M-g n");
    let second_match = |grid: &[String]| {
        grid.iter().any(|row| row.contains("plain middle"))
            && grid.iter().any(|row| row.contains("needle second"))
    };
    gnu.read_until(Duration::from_secs(6), second_match);
    neo.read_until(Duration::from_secs(8), second_match);
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));

    send_both(&mut gnu, &mut neo, "M-g p");
    gnu.read_until(Duration::from_secs(6), first_match);
    neo.read_until(Duration::from_secs(8), first_match);
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));
    send_both_raw(&mut gnu, &mut neo, b"!");

    let edited_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("!needle first"))
            && grid.iter().any(|row| row.contains("needle second"))
    };
    gnu.read_until(Duration::from_secs(6), edited_ready);
    neo.read_until(Duration::from_secs(8), edited_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "occur_next_and_previous_error_via_cx_backtick_mg_p",
        &gnu,
        &neo,
        3,
    );
}

#[test]
fn set_mark_command_then_kill_region_via_cspc_mf_cw() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "set-mark-kill-region.txt",
        "alpha beta gamma\n",
        "C-x C-f",
    );

    send_both_raw(&mut gnu, &mut neo, &[0]);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "M-f");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-w");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains(" beta gamma"))
            && !grid.iter().any(|row| row.contains("alpha beta gamma"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "set_mark_command_then_kill_region_via_cspc_mf_cw",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn mark_word_then_kill_region_via_mat_cw() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "mark-word.txt",
        "alpha beta gamma\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-@ C-w");
    let killed = |grid: &[String]| {
        grid.iter().any(|row| row.contains(" beta gamma"))
            && !grid.iter().any(|row| row.contains("alpha beta gamma"))
    };
    gnu.read_until(Duration::from_secs(6), killed);
    neo.read_until(Duration::from_secs(8), killed);
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));

    send_both(&mut gnu, &mut neo, "C-e SPC C-y");
    let yanked = |grid: &[String]| grid.iter().any(|row| row.contains(" beta gamma alpha"));
    gnu.read_until(Duration::from_secs(6), yanked);
    neo.read_until(Duration::from_secs(8), yanked);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("mark_word_then_kill_region_via_mat_cw", &gnu, &neo, 2);
}

#[test]
fn append_next_kill_via_cmeta_w_combines_following_region_kill() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "append-next-kill.txt",
        "alpha beta gamma\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-@ C-w");
    let first_killed = |grid: &[String]| grid.iter().any(|row| row.contains(" beta gamma"));
    gnu.read_until(Duration::from_secs(6), first_killed);
    neo.read_until(Duration::from_secs(8), first_killed);
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));

    send_both(&mut gnu, &mut neo, "M-@ C-M-w C-w");
    let second_killed = |grid: &[String]| {
        grid.iter().any(|row| row.contains(" gamma"))
            && !grid.iter().any(|row| row.contains(" beta gamma"))
    };
    gnu.read_until(Duration::from_secs(6), second_killed);
    neo.read_until(Duration::from_secs(8), second_killed);
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));

    send_both(&mut gnu, &mut neo, "C-e SPC C-y");
    let yanked = |grid: &[String]| grid.iter().any(|row| row.contains(" gamma alpha beta"));
    gnu.read_until(Duration::from_secs(6), yanked);
    neo.read_until(Duration::from_secs(8), yanked);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "append_next_kill_via_cmeta_w_combines_following_region_kill",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn delete_indentation_via_mcaret() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "delete-indentation.txt",
        "alpha line\nbeta line\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-n");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "M-^");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alpha line beta line"))
            && !grid.iter().any(|row| row.contains("alpha line\n"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("delete_indentation_via_mcaret", &gnu, &neo, 2);
}

#[test]
fn back_to_indentation_then_insert_via_mm() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "back-to-indentation.txt",
        "    alpha beta\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-e");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "M-m");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "X");

    let ready = |grid: &[String]| grid.iter().any(|row| row.contains("    Xalpha beta"));
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("back_to_indentation_then_insert_via_mm", &gnu, &neo, 2);
}

#[test]
fn zap_to_char_via_mz_spc() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "zap-to-char.txt",
        "alpha beta gamma\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-z SPC");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("beta gamma"))
            && !grid.iter().any(|row| row.contains("alpha beta gamma"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("zap_to_char_via_mz_spc", &gnu, &neo, 2);
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
fn apropos_command_find_file_via_ch_a_lists_matches() {
    let (mut gnu, mut neo) = boot_pair("");
    send_help_sequence(&mut gnu, &mut neo, "a");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Search for command"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "apropos_command_find_file_via_ch_a_lists_matches/prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"find-file");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*Apropos*"))
            && grid.iter().any(|row| row.contains("find-file"))
            && grid.iter().any(|row| row.contains("C-x C-f"))
    };
    gnu.read_until(Duration::from_secs(10), ready);
    neo.read_until(Duration::from_secs(15), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains("*Apropos*")),
            "{label} should show *Apropos* after C-h a"
        );
        assert!(
            grid.iter().any(|row| row.contains("find-file")),
            "{label} apropos-command should list find-file"
        );
        assert!(
            grid.iter().any(|row| row.contains("C-x C-f")),
            "{label} apropos-command should show find-file's default binding"
        );
    }
    assert_pair_nearly_matches(
        "apropos_command_find_file_via_ch_a_lists_matches",
        &gnu,
        &neo,
        3,
    );
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
fn describe_variable_fill_column_via_ch_v() {
    let (mut gnu, mut neo) = boot_pair("");
    send_help_sequence(&mut gnu, &mut neo, "v");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Describe variable"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "describe_variable_fill_column_via_ch_v/prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"fill-column");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*Help*"))
            && grid
                .iter()
                .any(|row| row.contains("fill-column is a variable"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains("*Help*")),
            "{label} should show *Help* after C-h v"
        );
        assert!(
            grid.iter()
                .any(|row| row.contains("fill-column is a variable")),
            "{label} describe-variable should mention fill-column"
        );
        assert!(
            grid.iter().any(|row| row.contains("70")),
            "{label} describe-variable should show fill-column's default value"
        );
    }
    assert_top_rows_nearly_match("describe_variable_fill_column_via_ch_v", &gnu, &neo, 18, 3);
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
fn where_is_find_file_via_ch_w_reports_key_binding() {
    let (mut gnu, mut neo) = boot_pair("");
    send_help_sequence(&mut gnu, &mut neo, "w");
    let prompt_ready = |grid: &[String]| grid.iter().any(|row| row.contains("Where is command"));
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "where_is_find_file_via_ch_w_reports_key_binding/prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"find-file");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter()
            .rev()
            .take(4)
            .any(|row| row.contains("find-file is on") && row.contains("C-x C-f"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter()
                .rev()
                .take(4)
                .any(|row| { row.contains("find-file is on") && row.contains("C-x C-f") }),
            "{label} where-is should report the default find-file binding"
        );
    }
    assert_pair_nearly_matches(
        "where_is_find_file_via_ch_w_reports_key_binding",
        &gnu,
        &neo,
        2,
    );
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
fn replace_string_via_mx_replaces_from_point_to_end() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "replace-string.txt",
        "alpha one\nbeta one\none tail\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-<");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    invoke_mx_command(&mut gnu, &mut neo, "replace-string");

    let from_prompt = |grid: &[String]| grid.iter().any(|row| row.contains("Replace string"));
    gnu.read_until(Duration::from_secs(6), from_prompt);
    neo.read_until(Duration::from_secs(8), from_prompt);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));

    for session in [&mut gnu, &mut neo] {
        session.send(b"one");
    }
    send_both(&mut gnu, &mut neo, "RET");
    let to_prompt = |grid: &[String]| {
        grid.iter().any(|row| row.contains("with:")) || grid.iter().any(|row| row.contains("with "))
    };
    gnu.read_until(Duration::from_secs(6), to_prompt);
    neo.read_until(Duration::from_secs(8), to_prompt);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));

    for session in [&mut gnu, &mut neo] {
        session.send(b"uno");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alpha uno"))
            && grid.iter().any(|row| row.contains("beta uno"))
            && grid.iter().any(|row| row.contains("uno tail"))
            && !grid.iter().any(|row| row.contains("alpha one"))
            && !grid.iter().any(|row| row.contains("beta one"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "replace_string_via_mx_replaces_from_point_to_end",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn replace_regexp_via_mx_replaces_numbers_from_point_to_end() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "replace-regexp.txt",
        "item-101\nitem-202\nplain\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-<");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    invoke_mx_command(&mut gnu, &mut neo, "replace-regexp");

    let from_prompt = |grid: &[String]| grid.iter().any(|row| row.contains("Replace regexp"));
    gnu.read_until(Duration::from_secs(6), from_prompt);
    neo.read_until(Duration::from_secs(8), from_prompt);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));

    for session in [&mut gnu, &mut neo] {
        session.send(b"[0-9][0-9]*");
    }
    send_both(&mut gnu, &mut neo, "RET");
    let to_prompt = |grid: &[String]| {
        grid.iter().any(|row| row.contains("with:")) || grid.iter().any(|row| row.contains("with "))
    };
    gnu.read_until(Duration::from_secs(6), to_prompt);
    neo.read_until(Duration::from_secs(8), to_prompt);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));

    for session in [&mut gnu, &mut neo] {
        session.send(b"NUM");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("item-NUM"))
            && grid.iter().any(|row| row.contains("plain"))
            && !grid.iter().any(|row| row.contains("item-101"))
            && !grid.iter().any(|row| row.contains("item-202"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "replace_regexp_via_mx_replaces_numbers_from_point_to_end",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn sort_lines_region_via_mx_orders_lines() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "sort-lines.txt",
        "delta\nalpha\ncharlie\nbravo\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x h");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    invoke_mx_command(&mut gnu, &mut neo, "sort-lines");

    let ready = |grid: &[String]| {
        let text = grid.join("\n");
        let Some(alpha) = text.find("alpha") else {
            return false;
        };
        let Some(bravo) = text.find("bravo") else {
            return false;
        };
        let Some(charlie) = text.find("charlie") else {
            return false;
        };
        let Some(delta) = text.find("delta") else {
            return false;
        };
        alpha < bravo && bravo < charlie && charlie < delta
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("sort_lines_region_via_mx_orders_lines", &gnu, &neo, 2);
}

#[test]
fn reverse_region_via_mx_reverses_lines() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "reverse-region.txt",
        "one\ntwo\nthree\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x h");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    invoke_mx_command(&mut gnu, &mut neo, "reverse-region");

    let ready = |grid: &[String]| {
        let text = grid.join("\n");
        let Some(three) = text.find("three") else {
            return false;
        };
        let Some(two) = text.find("two") else {
            return false;
        };
        let Some(one) = text.find("one") else {
            return false;
        };
        three < two && two < one
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("reverse_region_via_mx_reverses_lines", &gnu, &neo, 2);
    save_current_file_and_assert_contents(
        "reverse_region_via_mx_reverses_lines",
        &mut gnu,
        &mut neo,
        "reverse-region.txt",
        "three\ntwo\none\n",
    );
}

#[test]
fn sort_fields_second_field_via_prefix_orders_lines() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "sort-fields.txt",
        "3 banana\n2 apple\n1 cherry\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x h C-u 2");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    invoke_mx_command(&mut gnu, &mut neo, "sort-fields");

    let ready = |grid: &[String]| {
        let text = grid.join("\n");
        let Some(apple) = text.find("2 apple") else {
            return false;
        };
        let Some(banana) = text.find("3 banana") else {
            return false;
        };
        let Some(cherry) = text.find("1 cherry") else {
            return false;
        };
        apple < banana && banana < cherry
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "sort_fields_second_field_via_prefix_orders_lines",
        &gnu,
        &neo,
        2,
    );
    save_current_file_and_assert_contents(
        "sort_fields_second_field_via_prefix_orders_lines",
        &mut gnu,
        &mut neo,
        "sort-fields.txt",
        "2 apple\n3 banana\n1 cherry\n",
    );
}

#[test]
fn sort_numeric_fields_second_field_via_prefix_orders_lines() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "sort-numeric-fields.txt",
        "alpha 10\nbravo 2\ncharlie 7\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x h C-u 2");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    invoke_mx_command(&mut gnu, &mut neo, "sort-numeric-fields");

    let ready = |grid: &[String]| {
        let text = grid.join("\n");
        let Some(two) = text.find("bravo 2") else {
            return false;
        };
        let Some(seven) = text.find("charlie 7") else {
            return false;
        };
        let Some(ten) = text.find("alpha 10") else {
            return false;
        };
        two < seven && seven < ten
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "sort_numeric_fields_second_field_via_prefix_orders_lines",
        &gnu,
        &neo,
        2,
    );
    save_current_file_and_assert_contents(
        "sort_numeric_fields_second_field_via_prefix_orders_lines",
        &mut gnu,
        &mut neo,
        "sort-numeric-fields.txt",
        "bravo 2\ncharlie 7\nalpha 10\n",
    );
}

#[test]
fn sort_paragraphs_via_mx_orders_paragraph_blocks() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "sort-paragraphs.txt",
        "zeta paragraph\ncontinues here\n\nalpha paragraph\ncontinues here\n\ngamma paragraph\ncontinues here\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x h");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    invoke_mx_command(&mut gnu, &mut neo, "sort-paragraphs");

    let ready = |grid: &[String]| {
        let text = grid.join("\n");
        let Some(alpha) = text.find("alpha paragraph") else {
            return false;
        };
        let Some(gamma) = text.find("gamma paragraph") else {
            return false;
        };
        let Some(zeta) = text.find("zeta paragraph") else {
            return false;
        };
        alpha < gamma && gamma < zeta
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "sort_paragraphs_via_mx_orders_paragraph_blocks",
        &gnu,
        &neo,
        2,
    );
    save_current_file_and_assert_contents(
        "sort_paragraphs_via_mx_orders_paragraph_blocks",
        &mut gnu,
        &mut neo,
        "sort-paragraphs.txt",
        "alpha paragraph\ncontinues here\n\ngamma paragraph\ncontinues here\n\nzeta paragraph\ncontinues here\n",
    );
}

#[test]
fn delete_duplicate_lines_via_mx_keeps_first_occurrences() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "delete-duplicate-lines.txt",
        "alpha\nbeta\nalpha\ngamma\nbeta\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x h");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    invoke_mx_command(&mut gnu, &mut neo, "delete-duplicate-lines");

    let ready = |grid: &[String]| {
        let text = grid.join("\n");
        text.contains("alpha")
            && text.contains("beta")
            && text.contains("gamma")
            && grid
                .iter()
                .any(|row| row.contains("Deleted 2 duplicate lines"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "delete_duplicate_lines_via_mx_keeps_first_occurrences",
        &gnu,
        &neo,
        2,
    );
    save_current_file_and_assert_contents(
        "delete_duplicate_lines_via_mx_keeps_first_occurrences",
        &mut gnu,
        &mut neo,
        "delete-duplicate-lines.txt",
        "alpha\nbeta\ngamma\n",
    );
}

#[test]
fn flush_lines_via_mx_deletes_matching_lines() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "flush-lines.txt",
        "keep alpha\ndrop beta\nkeep gamma\ndrop delta\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-<");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    invoke_mx_command(&mut gnu, &mut neo, "flush-lines");

    let regexp_prompt = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Flush lines containing match"))
    };
    gnu.read_until(Duration::from_secs(6), regexp_prompt);
    neo.read_until(Duration::from_secs(8), regexp_prompt);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));

    for session in [&mut gnu, &mut neo] {
        session.send(b"drop");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("keep alpha"))
            && grid.iter().any(|row| row.contains("keep gamma"))
            && !grid.iter().any(|row| row.contains("drop beta"))
            && !grid.iter().any(|row| row.contains("drop delta"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("flush_lines_via_mx_deletes_matching_lines", &gnu, &neo, 2);
}

#[test]
fn keep_lines_via_mx_preserves_matching_lines() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "keep-lines.txt",
        "keep alpha\ndrop beta\nkeep gamma\ndrop delta\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-<");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    invoke_mx_command(&mut gnu, &mut neo, "keep-lines");

    let regexp_prompt = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("Keep lines containing match"))
    };
    gnu.read_until(Duration::from_secs(6), regexp_prompt);
    neo.read_until(Duration::from_secs(8), regexp_prompt);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));

    for session in [&mut gnu, &mut neo] {
        session.send(b"keep");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("keep alpha"))
            && grid.iter().any(|row| row.contains("keep gamma"))
            && !grid.iter().any(|row| row.contains("drop beta"))
            && !grid.iter().any(|row| row.contains("drop delta"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("keep_lines_via_mx_preserves_matching_lines", &gnu, &neo, 2);
}

#[test]
fn how_many_via_mx_reports_regexp_match_count() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "how-many.txt",
        "foo\nbar foo\nfoo\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-<");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    invoke_mx_command(&mut gnu, &mut neo, "how-many");
    let prompt_ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains("How many matches for regexp"))
    };
    gnu.read_until(Duration::from_secs(6), prompt_ready);
    neo.read_until(Duration::from_secs(8), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for session in [&mut gnu, &mut neo] {
        session.send(b"foo");
    }
    send_both(&mut gnu, &mut neo, "RET");

    let ready = |grid: &[String]| {
        grid.iter()
            .rev()
            .take(4)
            .any(|row| row.contains("3 occurrences"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter()
                .rev()
                .take(4)
                .any(|row| row.contains("3 occurrences")),
            "{label} should report three regexp matches in the echo area"
        );
    }
    assert_pair_nearly_matches("how_many_via_mx_reports_regexp_match_count", &gnu, &neo, 2);
}

#[test]
fn mark_whole_buffer_then_kill_and_yank_via_cx_h_cw_cy() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "mark-whole-buffer.txt",
        "alpha line\nbeta line\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x h");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-w");

    let killed = |grid: &[String]| {
        !grid.iter().any(|row| row.contains("alpha line"))
            && !grid.iter().any(|row| row.contains("beta line"))
    };
    gnu.read_until(Duration::from_secs(6), killed);
    neo.read_until(Duration::from_secs(8), killed);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both(&mut gnu, &mut neo, "C-y");
    let restored = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alpha line"))
            && grid.iter().any(|row| row.contains("beta line"))
    };
    gnu.read_until(Duration::from_secs(6), restored);
    neo.read_until(Duration::from_secs(8), restored);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "mark_whole_buffer_then_kill_and_yank_via_cx_h_cw_cy",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn narrow_to_region_once_then_widen_via_cx_n_n_w() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "narrow-widen.txt",
        "top line\nmiddle visible\nbottom line\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-n C-SPC C-n C-x n n");
    let prompt_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("Use this command?"))
            || grid
                .iter()
                .any(|row| row.contains("disabled command narrow-to-region"))
    };
    gnu.read_until(Duration::from_secs(8), prompt_ready);
    neo.read_until(Duration::from_secs(12), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains("narrow-to-region")),
            "{label} should show disabled-command help for narrow-to-region"
        );
        assert!(
            grid.iter().any(|row| row.contains("Use this command?")),
            "{label} should ask before running disabled narrow-to-region"
        );
    }

    send_both_raw(&mut gnu, &mut neo, b" ");
    let narrowed_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("middle visible"))
            && !grid.iter().any(|row| row.contains("top line"))
            && !grid.iter().any(|row| row.contains("bottom line"))
    };
    gnu.read_until(Duration::from_secs(8), narrowed_ready);
    neo.read_until(Duration::from_secs(12), narrowed_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "narrow_to_region_once_then_widen_via_cx_n_n_w/narrowed",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "C-x n w");
    let widened_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("top line"))
            && grid.iter().any(|row| row.contains("middle visible"))
            && grid.iter().any(|row| row.contains("bottom line"))
    };
    gnu.read_until(Duration::from_secs(6), widened_ready);
    neo.read_until(Duration::from_secs(8), widened_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "narrow_to_region_once_then_widen_via_cx_n_n_w",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn forward_paragraph_via_m_close_brace() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "forward-paragraph.txt",
        "alpha one\nalpha two\n\nbeta one\nbeta two\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-}");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"X");

    let ready = |grid: &[String]| grid.iter().any(|row| row.contains("Xbeta one"));
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("forward_paragraph_via_m_close_brace", &gnu, &neo, 2);
}

#[test]
fn backward_paragraph_via_m_open_brace() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "backward-paragraph.txt",
        "alpha one\nalpha two\n\nbeta one\nbeta two\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M->");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "M-{");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"X");

    let ready = |grid: &[String]| grid.iter().any(|row| row.contains("Xbeta one"));
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("backward_paragraph_via_m_open_brace", &gnu, &neo, 2);
}

#[test]
fn downcase_region_once_via_disabled_cx_cl() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "downcase-region.txt",
        "ALPHA BETA\nGAMMA DELTA\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x h");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-x C-l");
    let prompt_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("Use this command?"))
            || grid
                .iter()
                .any(|row| row.contains("disabled command downcase-region"))
    };
    gnu.read_until(Duration::from_secs(8), prompt_ready);
    neo.read_until(Duration::from_secs(12), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains("downcase-region")),
            "{label} should show disabled-command help for downcase-region"
        );
        assert!(
            grid.iter().any(|row| row.contains("Use this command?")),
            "{label} should show disabled-command prompt for downcase-region"
        );
    }

    send_both_raw(&mut gnu, &mut neo, b" ");
    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alpha beta"))
            && grid.iter().any(|row| row.contains("gamma delta"))
    };
    gnu.read_until(Duration::from_secs(8), ready);
    neo.read_until(Duration::from_secs(12), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    settle_session(&mut gnu, Duration::from_secs(1), 2);
    settle_session(&mut neo, Duration::from_secs(1), 6);

    assert_pair_nearly_matches("downcase_region_once_via_disabled_cx_cl", &gnu, &neo, 2);
}

#[test]
fn upcase_region_once_via_disabled_cx_cu() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "upcase-region.txt",
        "alpha beta\ngamma delta\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x h");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both(&mut gnu, &mut neo, "C-x C-u");
    let prompt_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("Use this command?"))
            || grid
                .iter()
                .any(|row| row.contains("disabled command upcase-region"))
    };
    gnu.read_until(Duration::from_secs(8), prompt_ready);
    neo.read_until(Duration::from_secs(12), prompt_ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    for (label, session) in [("GNU", &gnu), ("NEO", &neo)] {
        let grid = session.text_grid();
        assert!(
            grid.iter().any(|row| row.contains("upcase-region")),
            "{label} should show disabled-command help for upcase-region"
        );
        assert!(
            grid.iter().any(|row| row.contains("Use this command?")),
            "{label} should show disabled-command prompt for upcase-region"
        );
    }

    send_both_raw(&mut gnu, &mut neo, b" ");
    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("ALPHA BETA"))
            && grid.iter().any(|row| row.contains("GAMMA DELTA"))
    };
    gnu.read_until(Duration::from_secs(8), ready);
    neo.read_until(Duration::from_secs(12), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    settle_session(&mut gnu, Duration::from_secs(1), 2);
    settle_session(&mut neo, Duration::from_secs(1), 6);

    assert_pair_nearly_matches("upcase_region_once_via_disabled_cx_cu", &gnu, &neo, 2);
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

#[test]
fn goto_line_via_mg_g() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "goto-line.txt",
        "alpha line\nbeta line\ngamma line\ndelta line\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-g g 3 RET");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"X");

    let ready = |grid: &[String]| grid.iter().any(|row| row.contains("Xgamma line"));
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("goto_line_via_mg_g", &gnu, &neo, 2);
}

#[test]
fn repeat_complex_command_via_cx_esc_esc_edits_and_replays_goto_line() {
    let (mut gnu, mut neo) = boot_pair("");
    let contents = (1..=50)
        .map(|line| match line {
            2 => "target-two\n".to_string(),
            35 => "target-thirty-five\n".to_string(),
            _ => format!("plain numbered line {line:02}\n"),
        })
        .collect::<String>();
    open_home_file(
        &mut gnu,
        &mut neo,
        "repeat-complex.txt",
        &contents,
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M-g g");
    let goto_prompt = |grid: &[String]| grid.iter().any(|row| row.contains("Goto line:"));
    gnu.read_until(Duration::from_secs(6), goto_prompt);
    neo.read_until(Duration::from_secs(8), goto_prompt);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "repeat_complex_command_via_cx_esc_esc_edits_and_replays_goto_line/goto-prompt",
        &gnu,
        &neo,
        2,
    );

    for session in [&mut gnu, &mut neo] {
        session.send(b"2");
    }
    let goto_typed = |grid: &[String]| grid.last().is_some_and(|row| row.contains("Goto line: 2"));
    gnu.read_until(Duration::from_secs(6), goto_typed);
    neo.read_until(Duration::from_secs(8), goto_typed);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "repeat_complex_command_via_cx_esc_esc_edits_and_replays_goto_line/goto-typed",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "RET");
    let first_target = |grid: &[String]| grid.iter().any(|row| row.contains("target-two"));
    gnu.read_until(Duration::from_secs(6), first_target);
    neo.read_until(Duration::from_secs(8), first_target);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "repeat_complex_command_via_cx_esc_esc_edits_and_replays_goto_line/first-target",
        &gnu,
        &neo,
        3,
    );

    send_both(&mut gnu, &mut neo, "C-x ESC ESC");
    let redo_prompt = |grid: &[String]| {
        grid.iter().any(|row| row.contains("Redo:"))
            && grid.iter().any(|row| row.contains("goto-line"))
            && grid.iter().any(|row| row.contains("2"))
    };
    gnu.read_until(Duration::from_secs(6), redo_prompt);
    neo.read_until(Duration::from_secs(8), redo_prompt);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "repeat_complex_command_via_cx_esc_esc_edits_and_replays_goto_line/redo-prompt",
        &gnu,
        &neo,
        3,
    );

    send_both(&mut gnu, &mut neo, "C-b DEL 3 5");
    let redo_edited = |grid: &[String]| {
        grid.iter().any(|row| row.contains("Redo:"))
            && grid.iter().any(|row| row.contains("goto-line"))
            && grid.iter().any(|row| row.contains("35"))
    };
    gnu.read_until(Duration::from_secs(6), redo_edited);
    neo.read_until(Duration::from_secs(8), redo_edited);
    read_both(&mut gnu, &mut neo, Duration::from_millis(300));
    assert_pair_nearly_matches(
        "repeat_complex_command_via_cx_esc_esc_edits_and_replays_goto_line/redo-edited",
        &gnu,
        &neo,
        3,
    );

    send_both(&mut gnu, &mut neo, "RET");
    let second_target = |grid: &[String]| grid.iter().any(|row| row.contains("target-thirty-five"));
    gnu.read_until(Duration::from_secs(6), second_target);
    neo.read_until(Duration::from_secs(8), second_target);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "repeat_complex_command_via_cx_esc_esc_edits_and_replays_goto_line/second-target",
        &gnu,
        &neo,
        4,
    );

    send_both_raw(&mut gnu, &mut neo, b"X");
    let inserted = |grid: &[String]| grid.iter().any(|row| row.contains("Xtarget-thirty-five"));
    gnu.read_until(Duration::from_secs(6), inserted);
    neo.read_until(Duration::from_secs(8), inserted);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "repeat_complex_command_via_cx_esc_esc_edits_and_replays_goto_line/final-insert",
        &gnu,
        &neo,
        4,
    );
}

#[test]
fn repeat_last_command_via_cx_z_z_replays_previous_motion() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "repeat-last-command.txt",
        "alpha\nbeta\ngamma\ndelta\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-n C-x z z");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"X");

    let ready = |grid: &[String]| grid.iter().any(|row| row.contains("Xdelta"));
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "repeat_last_command_via_cx_z_z_replays_previous_motion",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn goto_char_via_mg_c() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(&mut gnu, &mut neo, "goto-char.txt", "abcdefg\n", "C-x C-f");

    send_both(&mut gnu, &mut neo, "M-g c 4 RET");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"X");

    let ready = |grid: &[String]| grid.iter().any(|row| row.contains("abcXdefg"));
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("goto_char_via_mg_c", &gnu, &neo, 2);
}

#[test]
fn beginning_and_end_of_buffer_via_mless_mgreater() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "buffer-boundaries.txt",
        "alpha line\nbeta line\ngamma line\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "M->");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"X");

    let at_end = |grid: &[String]| grid.iter().any(|row| row.contains("gamma lineX"));
    gnu.read_until(Duration::from_secs(6), at_end);
    neo.read_until(Duration::from_secs(8), at_end);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    assert_pair_nearly_matches(
        "beginning_and_end_of_buffer_via_mless_mgreater/end",
        &gnu,
        &neo,
        2,
    );

    send_both(&mut gnu, &mut neo, "M-<");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"Y");

    let at_beginning = |grid: &[String]| grid.iter().any(|row| row.contains("Yalpha line"));
    gnu.read_until(Duration::from_secs(6), at_beginning);
    neo.read_until(Duration::from_secs(8), at_beginning);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "beginning_and_end_of_buffer_via_mless_mgreater",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn count_words_region_via_mequals() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "count-words.txt",
        "alpha beta\ngamma delta\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-SPC M-> M-=");

    let ready = |grid: &[String]| grid.iter().any(|row| row.contains("Region has"));
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("count_words_region_via_mequals", &gnu, &neo, 2);
}

#[test]
fn what_cursor_position_via_cx_equals() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "what-cursor-position.txt",
        "alpha beta\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-f C-f C-x =");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("Char:"))
            || grid.iter().any(|row| row.contains("character:"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("what_cursor_position_via_cx_equals", &gnu, &neo, 2);
}

#[test]
fn quoted_insert_newline_via_cq_cj() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "quoted-insert.txt",
        "alpha beta\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-f C-f");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"\x11\x0a");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));
    send_both_raw(&mut gnu, &mut neo, b"X");

    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.trim() == "al")
            && grid.iter().any(|row| row.contains("Xpha beta"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("quoted_insert_newline_via_cq_cj", &gnu, &neo, 2);
}

#[test]
fn undo_edit_via_cslash() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "undo-cslash.txt",
        "alpha beta\n",
        "C-x C-f",
    );

    send_both_raw(&mut gnu, &mut neo, b"X");
    let inserted = |grid: &[String]| grid.iter().any(|row| row.contains("Xalpha beta"));
    gnu.read_until(Duration::from_secs(6), inserted);
    neo.read_until(Duration::from_secs(8), inserted);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    send_both_raw(&mut gnu, &mut neo, b"\x1f");
    let undone = |grid: &[String]| {
        grid.iter().any(|row| row.contains("alpha beta"))
            && !grid.iter().any(|row| row.contains("Xalpha beta"))
    };
    gnu.read_until(Duration::from_secs(6), undone);
    neo.read_until(Duration::from_secs(8), undone);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("undo_edit_via_cslash", &gnu, &neo, 2);
}

#[test]
fn comment_region_via_msemicolon() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "comment-dwim.el",
        "(message \"alpha\")\n(message \"beta\")\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-SPC M-> M-;");

    let ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains(";; (message \"alpha\")"))
            && grid.iter().any(|row| row.contains(";; (message \"beta\")"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches("comment_region_via_msemicolon", &gnu, &neo, 2);
}

#[test]
fn comment_line_via_mx_comments_current_line_and_moves_to_next() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "comment-line.el",
        "(message \"alpha\")\n(message \"beta\")\n",
        "C-x C-f",
    );

    invoke_mx_command(&mut gnu, &mut neo, "comment-line");

    let ready = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains(";; (message \"alpha\")"))
            && grid.iter().any(|row| row.contains("(message \"beta\")"))
    };
    gnu.read_until(Duration::from_secs(6), ready);
    neo.read_until(Duration::from_secs(8), ready);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "comment_line_via_mx_comments_current_line_and_moves_to_next",
        &gnu,
        &neo,
        2,
    );
}

#[test]
fn comment_line_via_cx_csemicolon_toggles_current_line() {
    let (mut gnu, mut neo) = boot_pair("");
    open_home_file(
        &mut gnu,
        &mut neo,
        "comment-line-toggle.el",
        "(message \"alpha\")\n(message \"beta\")\n",
        "C-x C-f",
    );

    send_both(&mut gnu, &mut neo, "C-x C-;");
    let commented = |grid: &[String]| {
        grid.iter()
            .any(|row| row.contains(";; (message \"alpha\")"))
            && grid.iter().any(|row| row.contains("(message \"beta\")"))
    };
    gnu.read_until(Duration::from_secs(6), commented);
    neo.read_until(Duration::from_secs(8), commented);
    read_both(&mut gnu, &mut neo, Duration::from_millis(500));

    send_both(&mut gnu, &mut neo, "C-p C-x C-;");
    let uncommented = |grid: &[String]| {
        grid.iter().any(|row| row.contains("(message \"alpha\")"))
            && !grid
                .iter()
                .any(|row| row.contains(";; (message \"alpha\")"))
            && grid.iter().any(|row| row.contains("(message \"beta\")"))
    };
    gnu.read_until(Duration::from_secs(6), uncommented);
    neo.read_until(Duration::from_secs(8), uncommented);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    assert_pair_nearly_matches(
        "comment_line_via_cx_csemicolon_toggles_current_line",
        &gnu,
        &neo,
        2,
    );
}
