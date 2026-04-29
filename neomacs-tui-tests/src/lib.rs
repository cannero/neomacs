//! TUI comparison test harness for Neomacs vs GNU Emacs.
//!
//! Spawns both editors in isolated pseudo-terminals, feeds identical
//! keystrokes, and compares the rendered screen cell by cell using the
//! `vt100` virtual terminal emulator.
//!
//! # Architecture
//!
//! - [`TuiSession`] wraps a child process in a PTY with a `vt100::Parser`.
//!   Call [`TuiSession::send`] to type keys and [`TuiSession::read`] to
//!   advance the parser. [`TuiSession::screen`] returns the current
//!   virtual screen.
//!
//! - [`emacs_key`] translates Emacs key descriptions (`"C-x"`, `"M-x"`,
//!   `"RET"`) into the raw bytes a terminal would send.
//!
//! - [`diff_screens`] compares two `vt100::Screen` snapshots and returns
//!   a list of [`CellDiff`] entries for every mismatched cell.
//!
//! - [`diff_screens_text`] is a simpler text-only comparison that ignores
//!   face attributes and normalises product names.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

// ── Session ──────────────────────────────────────────────────────────

/// Default terminal size for tests.
pub const COLS: u16 = 160;
pub const ROWS: u16 = 50;

/// A TUI editor session running inside an isolated PTY.
pub struct TuiSession {
    pty: pty_process::blocking::Pty,
    _child: std::process::Child,
    parser: vt100::Parser,
    recent_output: Vec<u8>,
    home: PathBuf,
    pub name: String,
}

impl TuiSession {
    fn unique_home_dir(name: &str) -> PathBuf {
        static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

        let session_id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
        let home = std::env::temp_dir().join(format!(
            "neomacs-tui-test-home-{}-{}-{}",
            std::process::id(),
            name.to_ascii_lowercase(),
            session_id
        ));
        let emacs_d = home.join(".emacs.d");
        std::fs::create_dir_all(&emacs_d).expect("create isolated tui test HOME");
        home
    }

    /// Spawn `cmd` (e.g. `"emacs -nw -Q"`) in a new PTY.
    pub fn spawn(cmd: &str, name: &str) -> Self {
        let (pty, pts) = pty_process::blocking::open().expect("open pty");
        pty.resize(pty_process::Size::new(ROWS, COLS))
            .expect("resize pty");

        let parts: Vec<&str> = cmd.split_whitespace().collect();
        let mut command = pty_process::blocking::Command::new(parts[0]);
        for arg in &parts[1..] {
            command = command.arg(arg);
        }
        let home = Self::unique_home_dir(name);
        command = command
            .env("TERM", "xterm-256color")
            .env("COLUMNS", COLS.to_string())
            .env("LINES", ROWS.to_string())
            // Prevent user config from interfering while also isolating
            // concurrent TUI tests from one another.
            .env("HOME", &home);
        for var in ["RUST_LOG", "NEOMACS_LOG_FILE", "NEOMACS_LOG_TO_FILE"] {
            if let Some(value) = std::env::var_os(var) {
                command = command.env(var, value);
            }
        }

        let child = command.spawn(pts).expect("spawn");

        let parser = vt100::Parser::new(ROWS, COLS, 0);

        TuiSession {
            pty,
            _child: child,
            parser,
            recent_output: Vec::new(),
            home,
            name: name.to_string(),
        }
    }

    /// Spawn GNU Emacs in TUI mode.
    pub fn gnu_emacs(extra_args: &str) -> Self {
        // Keep the GNU oracle focused on TUI behavior.  On NixOS the async
        // native compiler can fail after startup and pop *Warnings*, which
        // pollutes the rendered screen unrelated to the command under test.
        let quiet_native_comp = "--eval=(progn(set'native-comp-jit-compilation())(set'native-comp-async-report-warnings-errors'silent)(push'(native-compiler)warning-suppress-types)(mapc'kill-process(process-list)))";
        let cmd = if extra_args.is_empty() {
            format!("emacs -nw -Q -no-comp-spawn {quiet_native_comp}")
        } else {
            format!("emacs -nw -Q -no-comp-spawn {quiet_native_comp} {extra_args}")
        };
        Self::spawn(&cmd, "GNU")
    }

    /// Spawn Neomacs in TUI mode.
    ///
    /// `NEOMACS_TUI_NEOMACS_BIN` can override the binary path.  Otherwise,
    /// the harness follows Cargo's active test profile: debug nextest runs
    /// use `target/debug/neomacs`, and `cargo nextest --release` runs use
    /// `target/release/neomacs`.
    pub fn neomacs(extra_args: &str) -> Self {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace = manifest.parent().expect("workspace root");
        let bin = neomacs_binary_path(workspace);
        assert!(
            bin.exists(),
            "neomacs binary not found at {}\nRun `cargo build -p neomacs-bin` for debug, \
             `cargo build --release -p neomacs-bin` for release, or set NEOMACS_TUI_NEOMACS_BIN.",
            bin.display()
        );
        let cmd = if extra_args.is_empty() {
            format!("{} -nw -Q", bin.display())
        } else {
            format!("{} -nw -Q {extra_args}", bin.display())
        };
        Self::spawn(&cmd, "NEO")
    }

    /// Read PTY output until the editor has been quiet for
    /// [`IDLE_CUTOFF`] *after at least one byte has arrived*, or
    /// `max_timeout` elapses — whichever comes first. Feeds whatever
    /// it reads into the vt100 parser.
    ///
    /// The `max_timeout` argument is a safety cap, not the expected
    /// runtime: a TUI editor that starts emitting within 100 ms and
    /// finishes within another 200 ms will return after ~300 ms, not
    /// after the full timeout. The "saw at least one byte" gate
    /// guards against returning immediately after a `send_keys()`
    /// that the editor hasn't yet begun to process.
    pub fn read(&mut self, max_timeout: Duration) {
        /// How long a PTY must be quiet *after* the first byte to
        /// count as settled. Tune up if editors start pausing
        /// mid-render longer than this.
        const IDLE_CUTOFF: Duration = Duration::from_millis(300);
        /// Each `poll()` call waits at most this long before we
        /// re-check idle / max-deadline conditions.
        const POLL_SLICE_MS: i32 = 50;
        let max_deadline = Instant::now() + max_timeout;
        let mut last_activity: Option<Instant> = None;
        let mut buf = [0u8; 65536];
        loop {
            let now = Instant::now();
            if now >= max_deadline {
                break;
            }
            if let Some(last) = last_activity
                && now.duration_since(last) >= IDLE_CUTOFF
            {
                break;
            }
            let fd = std::os::fd::AsRawFd::as_raw_fd(&self.pty);
            let ready = unsafe {
                let mut pfd = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                libc::poll(&mut pfd, 1, POLL_SLICE_MS) > 0 && (pfd.revents & libc::POLLIN) != 0
            };
            if ready {
                match self.pty.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        self.recent_output.extend_from_slice(&buf[..n]);
                        if self.recent_output.len() > 262_144 {
                            let drain = self.recent_output.len() - 262_144;
                            self.recent_output.drain(..drain);
                        }
                        self.parser.process(&buf[..n]);
                        last_activity = Some(Instant::now());
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => break,
                }
            }
        }
    }

    /// Send raw bytes to the PTY.
    pub fn send(&mut self, data: &[u8]) {
        let _ = self.pty.write_all(data);
    }

    /// Like [`TuiSession::read`] but keep reading past idle gaps until
    /// `predicate` returns true on some row of the rendered grid, or
    /// `max_timeout` elapses. Useful when a command's legitimate
    /// render pipeline has mid-burst pauses longer than
    /// `IDLE_CUTOFF` (e.g. `view-hello-file` running format-decode →
    /// enriched-decode → view-mode setup) so plain idle-detection
    /// returns too eagerly.
    pub fn read_until<F>(&mut self, max_timeout: Duration, predicate: F)
    where
        F: Fn(&[String]) -> bool,
    {
        let deadline = Instant::now() + max_timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            self.read(remaining);
            if predicate(&self.text_grid()) {
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
        }
    }

    /// Resize the underlying PTY and the virtual terminal parser.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.pty
            .resize(pty_process::Size::new(rows, cols))
            .expect("resize pty");
        self.parser.set_size(rows, cols);
    }

    /// Send an Emacs key description (e.g. `"C-x"`, `"M-x"`, `"RET"`).
    pub fn send_key(&mut self, key: &str) {
        self.send(&emacs_key(key));
    }

    /// Send a sequence of keys separated by spaces (e.g. `"C-x 2"`).
    pub fn send_keys(&mut self, keys: &str) {
        for part in keys.split_whitespace() {
            self.send_key(part);
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Get the current virtual terminal screen.
    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Get the current virtual terminal dimensions as `(rows, cols)`.
    pub fn screen_size(&self) -> (u16, u16) {
        self.screen().size()
    }

    /// Get the text content of a single row (0-indexed).
    pub fn row_text(&self, row: u16) -> String {
        let (_, cols) = self.screen_size();
        self.screen().contents_between(row, 0, row, cols)
    }

    /// Get all rows as a Vec of strings.
    pub fn text_grid(&self) -> Vec<String> {
        let (rows, _) = self.screen_size();
        (0..rows).map(|r| self.row_text(r)).collect()
    }

    /// Clear the accumulated raw PTY output captured by [`read`].
    pub fn clear_recent_output(&mut self) {
        self.recent_output.clear();
    }

    /// Borrow the recent raw PTY output captured by [`read`].
    pub fn recent_output(&self) -> &[u8] {
        &self.recent_output
    }

    /// Return the isolated HOME directory used for this session.
    pub fn home_dir(&self) -> &std::path::Path {
        &self.home
    }
}

const NEOMACS_TUI_NEOMACS_BIN: &str = "NEOMACS_TUI_NEOMACS_BIN";

fn neomacs_binary_path(workspace: &Path) -> PathBuf {
    neomacs_binary_path_from_override(workspace, std::env::var_os(NEOMACS_TUI_NEOMACS_BIN))
}

fn neomacs_binary_path_from_override(
    workspace: &Path,
    override_path: Option<std::ffi::OsString>,
) -> PathBuf {
    if let Some(path) = override_path
        && !path.as_os_str().is_empty()
    {
        return PathBuf::from(path);
    }

    workspace
        .join("target")
        .join(active_cargo_profile_dir())
        .join("neomacs")
}

fn active_cargo_profile_dir() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

impl Drop for TuiSession {
    fn drop(&mut self) {
        // Best-effort kill
        let _ = self._child.kill();
        let _ = self._child.wait();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

// ── Key translation ──────────────────────────────────────────────────

/// Translate an Emacs-style key name to the bytes a terminal sends.
///
/// Supports: `C-x`, `M-x`, `C-M-x`, `RET`, `TAB`, `ESC`, `SPC`,
/// `DEL`, and plain characters.
pub fn emacs_key(key: &str) -> Vec<u8> {
    match key {
        "RET" | "Enter" => return vec![b'\r'],
        "TAB" => return vec![b'\t'],
        "ESC" => return vec![0x1b],
        "SPC" => return vec![b' '],
        "C-SPC" | "C-@" => return vec![0x00],
        "C-M-SPC" | "C-M-@" => return vec![0x1b, 0x00],
        "C-/" | "C-_" => return vec![0x1f],
        "C-M-/" | "C-M-_" => return vec![0x1b, 0x1f],
        "DEL" => return vec![0x7f],
        "BS" => return vec![0x08],
        _ => {}
    }

    // C-M-x  →  ESC + Ctrl(x)
    if let Some(ch) = key.strip_prefix("C-M-").and_then(|s| s.chars().next()) {
        let ctrl = (ch.to_ascii_lowercase() as u8)
            .wrapping_sub(b'a')
            .wrapping_add(1);
        return vec![0x1b, ctrl];
    }
    // C-x  →  Ctrl(x)
    if let Some(ch) = key.strip_prefix("C-").and_then(|s| s.chars().next()) {
        if ch == '@' {
            return vec![0x00];
        }
        let ctrl = (ch.to_ascii_lowercase() as u8)
            .wrapping_sub(b'a')
            .wrapping_add(1);
        return vec![ctrl];
    }
    // M-x  →  ESC x
    if let Some(ch) = key.strip_prefix("M-").and_then(|s| s.chars().next()) {
        return vec![0x1b, ch as u8];
    }

    // Plain character or multi-byte
    key.as_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::{active_cargo_profile_dir, emacs_key, neomacs_binary_path_from_override};
    use std::ffi::OsString;
    use std::fmt::Write as _;
    use std::path::{Path, PathBuf};

    #[test]
    fn neomacs_binary_path_prefers_explicit_override() {
        let workspace = Path::new("/repo");
        let path = neomacs_binary_path_from_override(
            workspace,
            Some(OsString::from("/tmp/custom-neomacs")),
        );

        assert_eq!(path, PathBuf::from("/tmp/custom-neomacs"));
    }

    #[test]
    fn neomacs_binary_path_uses_active_cargo_profile() {
        let workspace = Path::new("/repo");
        let path = neomacs_binary_path_from_override(workspace, None);

        assert_eq!(
            path,
            PathBuf::from("/repo")
                .join("target")
                .join(active_cargo_profile_dir())
                .join("neomacs")
        );
    }

    #[test]
    fn emacs_key_maps_control_space_to_terminal_nul() {
        assert_eq!(emacs_key("C-SPC"), vec![0x00]);
        assert_eq!(emacs_key("C-@"), vec![0x00]);
        assert_eq!(emacs_key("C-M-SPC"), vec![0x1b, 0x00]);
        assert_eq!(emacs_key("C-M-@"), vec![0x1b, 0x00]);
        assert_eq!(emacs_key("C-/"), vec![0x1f]);
        assert_eq!(emacs_key("C-_"), vec![0x1f]);
        assert_eq!(emacs_key("C-M-/"), vec![0x1b, 0x1f]);
        assert_eq!(emacs_key("C-M-_"), vec![0x1b, 0x1f]);
    }

    #[test]
    fn vt100_parser_does_not_render_decscusr_cursor_shape_as_text() {
        let mut parser = vt100::Parser::new(2, 40, 0);
        parser.process(b"\x1b[1;1HList lines matching regexp: \x1b[6 q\x1b[?25h");

        let row = parser.screen().contents_between(0, 0, 0, 40);
        let trimmed = row.trim_end();
        if trimmed != "List lines matching regexp:" {
            let mut bytes = String::new();
            for byte in b"\x1b[1;1HList lines matching regexp: \x1b[6 q\x1b[?25h" {
                let _ = write!(&mut bytes, "{byte:02x} ");
            }
            panic!("unexpected row {trimmed:?} for bytes [{bytes}]");
        }
    }
}

// ── Screen diffing ───────────────────────────────────────────────────

/// A single cell difference between two screens.
#[derive(Debug)]
pub struct CellDiff {
    pub row: u16,
    pub col: u16,
    pub gnu_char: String,
    pub neo_char: String,
    pub gnu_fg: vt100::Color,
    pub neo_fg: vt100::Color,
    pub gnu_bg: vt100::Color,
    pub neo_bg: vt100::Color,
    pub kind: DiffKind,
}

#[derive(Debug, PartialEq)]
pub enum DiffKind {
    Char,
    Color,
    Both,
}

/// Compare two screens cell by cell, returning all differences.
pub fn diff_screens(gnu: &vt100::Screen, neo: &vt100::Screen) -> Vec<CellDiff> {
    let mut diffs = Vec::new();
    for row in 0..ROWS {
        for col in 0..COLS {
            let gc = gnu.cell(row, col);
            let nc = neo.cell(row, col);
            let (gc, nc) = match (gc, nc) {
                (Some(g), Some(n)) => (g, n),
                _ => continue,
            };

            let char_diff = gc.contents() != nc.contents();
            let color_diff = gc.fgcolor() != nc.fgcolor() || gc.bgcolor() != nc.bgcolor();

            if char_diff || color_diff {
                diffs.push(CellDiff {
                    row,
                    col,
                    gnu_char: gc.contents().to_string(),
                    neo_char: nc.contents().to_string(),
                    gnu_fg: gc.fgcolor(),
                    neo_fg: nc.fgcolor(),
                    gnu_bg: gc.bgcolor(),
                    neo_bg: nc.bgcolor(),
                    kind: match (char_diff, color_diff) {
                        (true, true) => DiffKind::Both,
                        (true, false) => DiffKind::Char,
                        (false, true) => DiffKind::Color,
                        _ => unreachable!(),
                    },
                });
            }
        }
    }
    diffs
}

/// A row-level text difference.
#[derive(Debug)]
pub struct RowDiff {
    pub row: usize,
    pub gnu: String,
    pub neo: String,
}

/// Compare two text grids, normalising known product-name differences.
///
/// Returns only rows where meaningful differences remain after
/// replacing "GNU Emacs" ↔ "Neomacs" and stripping trailing whitespace.
pub fn diff_text_grids(gnu: &[String], neo: &[String]) -> Vec<RowDiff> {
    let mut diffs = Vec::new();
    let norm = |s: &str| -> String {
        s.replace("GNU Emacs", "EDITOR__")
            .replace("*GNU Emacs*", "*EDITOR__*")
            .replace("Neomacs", "EDITOR__")
            .replace("*Neomacs*", "*EDITOR__*")
            .trim_end()
            .to_string()
    };
    for (i, (g, n)) in gnu.iter().zip(neo.iter()).enumerate() {
        if norm(g) != norm(n) {
            diffs.push(RowDiff {
                row: i,
                gnu: g.trim_end().to_string(),
                neo: n.trim_end().to_string(),
            });
        }
    }
    diffs
}

/// Check whether a row difference is just boot-screen informational text
/// that we expect to differ (welcome message, copyright, etc.).
pub fn is_boot_info_row(gnu_text: &str, neo_text: &str) -> bool {
    let patterns = [
        "information about GNU",
        "Welcome to GNU",
        "tutorial",
        "Copyright",
        "Free Software",
        "warranty",
        "C-h C-a",
        "Appl",
    ];
    for p in &patterns {
        if gnu_text.contains(p) || neo_text.contains(p) {
            return true;
        }
    }
    false
}

/// Pretty-print row diffs to stderr (useful in test assertions).
pub fn print_row_diffs(diffs: &[RowDiff]) {
    for d in diffs {
        eprintln!("  row {:2}:", d.row);
        eprintln!("    GNU: |{}|", d.gnu);
        eprintln!("    NEO: |{}|", d.neo);
    }
}
