//! TUI comparisons for ordinary scratch-buffer editing.
//!
//! GNU's default path for printable characters is `self-insert-command`
//! (`src/cmds.c`), while RET in ordinary editing invokes `newline`
//! (`lisp/simple.el`) and may run electric indentation from
//! `lisp/electric.el`.

mod support;

use neomacs_tui_tests::TuiSession;
use std::fs;
use std::time::Duration;
use support::*;

const FIRST: &str = "alpha scratch line";
const SECOND: &str = "beta scratch line";
const THIRD: &str = "gamma scratch line";
const EXPECTED_BUFFER: &str = "alpha scratch line\nbeta scratch line\ngamma scratch line";

fn visible_rows_for_typed_lines(label: &str, session: &TuiSession) -> [usize; 3] {
    let grid = session.text_grid();
    let find_row = |needle: &str| {
        grid.iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| {
                panic!(
                    "{label} should visibly contain {needle:?}\n{}",
                    grid.join("\n")
                )
            })
    };

    let rows = [find_row(FIRST), find_row(SECOND), find_row(THIRD)];
    assert_eq!(
        rows[1],
        rows[0] + 1,
        "{label}: first and second typed lines should be adjacent"
    );
    assert_eq!(
        rows[2],
        rows[1] + 1,
        "{label}: second and third typed lines should be adjacent"
    );
    rows
}

#[test]
fn scratch_self_insert_ret_creates_three_visible_lines() {
    let (mut gnu, mut neo) = boot_pair("");

    send_both(&mut gnu, &mut neo, "C-x h C-w");
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    let typed = format!("{FIRST}\r{SECOND}\r{THIRD}");
    gnu.send(typed.as_bytes());
    neo.send(typed.as_bytes());

    let typed_lines_visible = |grid: &[String]| {
        [FIRST, SECOND, THIRD]
            .iter()
            .all(|line| grid.iter().any(|row| row.contains(line)))
    };
    gnu.read_until(Duration::from_secs(6), typed_lines_visible);
    neo.read_until(Duration::from_secs(8), typed_lines_visible);
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    let gnu_rows = visible_rows_for_typed_lines("GNU", &gnu);
    let neo_rows = visible_rows_for_typed_lines("Neomacs", &neo);
    assert_eq!(
        neo_rows, gnu_rows,
        "Neomacs should render the typed scratch lines on the same rows as GNU"
    );

    eval_expression(
        &mut gnu,
        &mut neo,
        r#"(with-current-buffer "*scratch*" (write-region (point-min) (point-max) "~/scratch-three-lines.txt" nil 'silent))"#,
    );
    read_both(&mut gnu, &mut neo, Duration::from_secs(1));

    let gnu_buffer =
        fs::read_to_string(gnu.home_dir().join("scratch-three-lines.txt")).expect("read GNU dump");
    let neo_buffer = fs::read_to_string(neo.home_dir().join("scratch-three-lines.txt"))
        .expect("read Neomacs dump");
    assert_eq!(gnu_buffer, EXPECTED_BUFFER, "GNU scratch buffer contents");
    assert_eq!(
        neo_buffer, EXPECTED_BUFFER,
        "Neomacs scratch buffer contents"
    );
}
