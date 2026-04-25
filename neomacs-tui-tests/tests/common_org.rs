//! TUI comparisons for common Org mode workflows.
//!
//! GNU behavior here is driven by `lisp/org/org.el`,
//! `lisp/org/org-cycle.el`, `lisp/org/org-keys.el`, and
//! `lisp/org/org-list.el`.

mod support;

use std::time::Duration;
use support::*;

fn grid_contains(session: &neomacs_tui_tests::TuiSession, needle: &str) -> bool {
    session.text_grid().iter().any(|row| row.contains(needle))
}

#[test]
fn org_todo_via_cc_ct_cycles_heading_keyword() {
    let (mut gnu, mut neo) = boot_pair("");
    let name = "org-todo-probe.org";
    let initial = "* Task\n";
    let expected = "* DONE Task\n";

    open_home_file(&mut gnu, &mut neo, name, initial, "C-x C-f");
    send_both(&mut gnu, &mut neo, "C-c C-t");
    wait_for_both(&mut gnu, &mut neo, Duration::from_secs(8), |grid| {
        grid.iter().any(|row| row.contains("* TODO Task"))
    });
    send_both(&mut gnu, &mut neo, "C-c C-t");
    wait_for_both(&mut gnu, &mut neo, Duration::from_secs(8), |grid| {
        grid.iter().any(|row| row.contains("* DONE Task"))
    });

    save_current_file_and_assert_contents("org-todo", &mut gnu, &mut neo, name, expected);
}

#[test]
fn org_meta_return_inserts_same_level_heading() {
    let (mut gnu, mut neo) = boot_pair("");
    let name = "org-meta-return-probe.org";
    let initial = "* First\n* Second\n";
    let expected = "* First\n* Inserted\n* Second\n";

    open_home_file(&mut gnu, &mut neo, name, initial, "C-x C-f");
    send_both(&mut gnu, &mut neo, "C-e ESC RET");
    gnu.send(b"Inserted");
    neo.send(b"Inserted");
    wait_for_both(&mut gnu, &mut neo, Duration::from_secs(8), |grid| {
        grid.iter().any(|row| row.contains("* Inserted"))
    });

    save_current_file_and_assert_contents("org-meta-return", &mut gnu, &mut neo, name, expected);
}

#[test]
fn org_tab_local_cycle_folds_and_reveals_subtree() {
    let (mut gnu, mut neo) = boot_pair("");
    let name = "org-cycle-probe.org";
    let initial = "* Parent\nbody line\n** Child\nchild body\n* Next\n";

    open_home_file(&mut gnu, &mut neo, name, initial, "C-x C-f");
    send_both(&mut gnu, &mut neo, "TAB");
    read_both(&mut gnu, &mut neo, Duration::from_secs(2));

    for session in [&gnu, &neo] {
        assert!(
            grid_contains(session, "* Parent"),
            "{} should keep the folded parent heading visible",
            session.name
        );
        assert!(
            !grid_contains(session, "body line") && !grid_contains(session, "** Child"),
            "{} should hide subtree body and children after first TAB",
            session.name
        );
    }

    send_both(&mut gnu, &mut neo, "TAB TAB");
    wait_for_both(&mut gnu, &mut neo, Duration::from_secs(8), |grid| {
        grid.iter().any(|row| row.contains("body line"))
            && grid.iter().any(|row| row.contains("** Child"))
            && grid.iter().any(|row| row.contains("child body"))
    });
}
