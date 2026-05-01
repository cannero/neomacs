#![allow(dead_code)]

use neomacs_tui_tests::*;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

/// Maximum total time for GNU Emacs to reach the startup predicate.
const GNU_STARTUP_TIMEOUT: Duration = Duration::from_secs(12);
/// Maximum total time for Neomacs to reach the startup predicate.
const NEO_STARTUP_TIMEOUT: Duration = Duration::from_secs(18);
/// Idle-settle timeout used after startup — keep reading until the
/// grid is stable for this long with no round cap.
const SETTLE_IDLE: Duration = Duration::from_millis(500);
/// Granularity for interleaved poll of both PTYs during parallel boot.
const POLL_SLICE: Duration = Duration::from_millis(80);

// ── boot_pair (canonical) ──────────────────────────────────────────────

/// Boot GNU Emacs and Neomacs side-by-side.
///
/// # Phases
///
/// 1. **Concurrent poll** — both processes are spawned and their PTYs are
///    drained in interleaved short slices. As soon as one editor reaches the
///    startup predicate it stops polling that PTY, so the faster editor
///    never waits for the slower one.
///
/// 2. **Uncapped settle** — after the predicate fires, the session is
///    read in rounds until its rendered grid stops changing. No round
///    limit — it keeps going until the grid is truly stable. This
///    absorbs late-startup display bursts without a blind `sleep`.
pub fn boot_pair(extra_args: &str) -> (TuiSession, TuiSession) {
    let mut gnu = TuiSession::gnu_emacs(extra_args);
    let mut neo = TuiSession::neomacs(extra_args);

    let startup_ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains("*scratch*"))
            && grid
                .iter()
                .any(|row| row.contains("This buffer is for text that is not saved"))
    };

    // Phase 1 — interleaved concurrent poll
    let gnu_deadline = Instant::now() + GNU_STARTUP_TIMEOUT;
    let neo_deadline = Instant::now() + NEO_STARTUP_TIMEOUT;
    let mut gnu_ready = false;
    let mut neo_ready = false;

    while !gnu_ready || !neo_ready {
        let now = Instant::now();
        if now >= gnu_deadline && now >= neo_deadline {
            break;
        }
        if !gnu_ready && now < gnu_deadline {
            let cap = gnu_deadline.saturating_duration_since(now).min(POLL_SLICE);
            gnu.read(cap);
            gnu_ready = startup_ready(&gnu.text_grid());
        }
        if !neo_ready && now < neo_deadline {
            let cap = neo_deadline.saturating_duration_since(now).min(POLL_SLICE);
            neo.read(cap);
            neo_ready = startup_ready(&neo.text_grid());
        }
    }

    // Phase 2 — uncapped settle (absorbs late-render bursts)
    settle_session(&mut gnu);
    settle_session(&mut neo);

    (gnu, neo)
}

/// Read `session` until its rendered grid stops changing, with no round cap.
pub fn settle_session(session: &mut TuiSession) {
    let mut previous = session.text_grid();
    loop {
        session.read(SETTLE_IDLE);
        let current = session.text_grid();
        if current == previous {
            return;
        }
        previous = current;
    }
}

// ── Shared helpers ─────────────────────────────────────────────────────

pub fn send_both(gnu: &mut TuiSession, neo: &mut TuiSession, keys: &str) {
    gnu.send_keys(keys);
    neo.send_keys(keys);
}

pub fn send_both_raw(gnu: &mut TuiSession, neo: &mut TuiSession, bytes: &[u8]) {
    gnu.send(bytes);
    neo.send(bytes);
}

pub fn read_both(gnu: &mut TuiSession, neo: &mut TuiSession, timeout: Duration) {
    gnu.read(timeout);
    neo.read(timeout);
}

pub fn resize_both(gnu: &mut TuiSession, neo: &mut TuiSession, rows: u16, cols: u16) {
    gnu.resize(rows, cols);
    neo.resize(rows, cols);
}

/// Wait until `predicate` is satisfied on both sessions or `timeout`
/// elapses. Polls both PTYs concurrently in short interleaved slices,
/// same strategy as `boot_pair` phase 1.
pub fn wait_for_both<F>(
    gnu: &mut TuiSession,
    neo: &mut TuiSession,
    timeout: Duration,
    predicate: F,
) where
    F: Fn(&[String]) -> bool + Copy,
{
    let deadline = Instant::now() + timeout;
    let mut gnu_ok = predicate(&gnu.text_grid());
    let mut neo_ok = predicate(&neo.text_grid());
    while !gnu_ok || !neo_ok {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let cap = deadline.saturating_duration_since(now).min(POLL_SLICE);
        if !gnu_ok {
            gnu.read(cap);
            gnu_ok = predicate(&gnu.text_grid());
        }
        if !neo_ok {
            neo.read(cap);
            neo_ok = predicate(&neo.text_grid());
        }
    }
}

// ── Higher-level workflow helpers ──────────────────────────────────────

pub fn invoke_mx_command(gnu: &mut TuiSession, neo: &mut TuiSession, command: &str) {
    send_both(gnu, neo, "M-x");
    let mx_prompt = |grid: &[String]| grid.last().is_some_and(|row| row.contains("M-x"));
    wait_for_both(gnu, neo, Duration::from_secs(8), mx_prompt);
    read_both(gnu, neo, Duration::from_millis(300));

    gnu.send(command.as_bytes());
    neo.send(command.as_bytes());
    send_both(gnu, neo, "RET");
}

pub fn eval_expression(gnu: &mut TuiSession, neo: &mut TuiSession, expression: &str) {
    send_both(gnu, neo, "M-:");
    let prompt_ready = |grid: &[String]| grid.last().is_some_and(|row| row.contains("Eval:"));
    wait_for_both(gnu, neo, Duration::from_secs(8), prompt_ready);
    read_both(gnu, neo, Duration::from_millis(300));
    gnu.send(expression.as_bytes());
    neo.send(expression.as_bytes());
    send_both(gnu, neo, "RET");
}

pub fn open_home_file(
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
    wait_for_both(gnu, neo, Duration::from_secs(20), ready);
    read_both(gnu, neo, Duration::from_secs(1));
}

pub fn open_file_path(
    gnu: &mut TuiSession,
    neo: &mut TuiSession,
    path: &Path,
    first_line: &str,
    keys: &str,
) {
    send_both(gnu, neo, keys);
    let path_str = path.to_string_lossy();
    gnu.send(path_str.as_bytes());
    neo.send(path_str.as_bytes());
    send_both(gnu, neo, "RET");

    let file_name = Path::new(path_str.as_ref())
        .file_name()
        .and_then(|name| name.to_str())
        .expect("test path should have a utf-8 file name")
        .to_string();
    let ready = |grid: &[String]| {
        grid.iter().any(|row| row.contains(&file_name))
            && grid.iter().any(|row| row.contains(first_line))
    };
    wait_for_both(gnu, neo, Duration::from_secs(20), ready);
    read_both(gnu, neo, Duration::from_secs(1));
}

pub fn save_current_file_and_assert_contents(
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

// ── File helpers ──────────────────────────────────────────────────────

pub fn write_home_file(session: &TuiSession, name: &str, contents: &str) {
    let path = session.home_dir().join(name);
    fs::write(path, contents).expect("write test file in isolated HOME");
}
