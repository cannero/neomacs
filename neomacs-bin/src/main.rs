//! Neomacs — standalone Rust binary
//!
//! Uses the neovm-core Elisp evaluator with a GNU Emacs-compatible command
//! loop.  The evaluator's `recursive_edit()` drives the main event loop:
//!
//!   read_char() → key-binding → command-execute → redisplay
//!
//! All editing commands, keybindings, and user customizations come from Elisp
//! (loaded .el files), just like GNU Emacs.  Only the core command loop and
//! low-level primitives are implemented in Rust.

mod args;
mod input_bridge;
mod tty_frontend;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use neomacs_display_runtime::render_thread::{
    RenderThread, SharedImageDimensions, SharedMonitorInfo,
};
use neomacs_display_runtime::thread_comm::{
    InputEvent as DisplayInputEvent, RenderCommand, ThreadComms,
};
use neomacs_layout_engine::font_metrics::FontMetricsService;
use neomacs_layout_engine::fontconfig::face_height_to_pixels;
use neomacs_layout_engine::gui_chrome::{collect_gui_menu_bar_items, collect_gui_tool_bar_items};

use neovm_core::buffer::BufferId;
use neovm_core::emacs_core::Value;
use neovm_core::emacs_core::builtins::set_neomacs_monitor_info;
use neovm_core::emacs_core::display::gui_window_system_symbol;
use neovm_core::emacs_core::eval::{
    FontResolveRequest, FontSpecResolveRequest, GuiFrameHostSize, ImageResolveRequest,
    ImageResolveSource, ResolvedFontMatch, ResolvedFontSpecMatch, ResolvedFrameFont, ResolvedImage,
};
use neovm_core::emacs_core::load::LoadupDumpMode;
use neovm_core::emacs_core::load::LoadupStartupSurface;
use neovm_core::emacs_core::load::RuntimeImageRole;
#[cfg(test)]
use neovm_core::emacs_core::print_value_with_eval;
use neovm_core::emacs_core::terminal::pure::{
    TerminalHost, TerminalRuntimeConfig, configure_terminal_runtime, reset_terminal_host,
    reset_terminal_runtime, set_terminal_host,
};
use neovm_core::emacs_core::{Context, DisplayHost, GuiFrameHostRequest};
use neovm_core::face::{FaceHeight, FontSlant, FontWeight, FontWidth};
use neovm_core::window::{FrameId, Window};

#[derive(Debug, Clone, PartialEq, Eq)]
enum EarlyCliAction {
    PrintHelp { program: String },
    PrintVersion,
    PrintFingerprint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrontendKind {
    Gui,
    Tty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMode {
    Raw,
    BootstrapUse,
    FinalRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DumpImageKind {
    Bootstrap,
    Final,
}

impl RuntimeMode {
    pub const fn binary_name(self) -> &'static str {
        match self {
            Self::Raw => "neomacs-temacs",
            Self::BootstrapUse => "bootstrap-neomacs",
            Self::FinalRun => "neomacs",
        }
    }

    pub const fn dump_image_kind(self) -> Option<DumpImageKind> {
        match self {
            Self::Raw => None,
            Self::BootstrapUse => Some(DumpImageKind::Bootstrap),
            Self::FinalRun => Some(DumpImageKind::Final),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StartupOptions {
    frontend: FrontendKind,
    forwarded_args: Vec<String>,
    terminal_device: Option<String>,
    noninteractive: bool,
    temacs_mode: Option<LoadupDumpMode>,
    dump_file_override: Option<PathBuf>,
    /// Set by `-Q` (peek) and `-x` (consumed). Mirrors GNU
    /// `no_site_lisp` at emacs.c:2126/2135.
    no_site_lisp: bool,
    /// Set by `-nl` / `--no-loadup`. Mirrors GNU `no_loadup` at
    /// emacs.c:2031. Only meaningful in `RuntimeMode::Raw`, where it
    /// suppresses the `-l loadup` splice that would otherwise force
    /// `loadup.el` to run.
    no_loadup: bool,
    /// Set by `-no-build-details` / `--no-build-details`. Mirrors GNU
    /// `build_details` at emacs.c:2037 (where the negation is taken).
    /// When true, build-time strings (e.g. `emacs-build-time`) should
    /// be cleared rather than populated.
    no_build_details: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BootstrapDisplayConfig {
    frontend: FrontendKind,
    color_cells: i64,
    background_mode: &'static str,
}

const EARLY_HELP_BODY: &str = concat!(
    "Run Neomacs, the extensible, customizable, self-documenting real-time\n",
    "display editor.  The recommended way to start Neomacs for normal editing\n",
    "is with no options at all.\n",
    "\n",
    "Run M-x info RET m emacs RET m emacs invocation RET inside Emacs to\n",
    "read the main documentation for these command-line arguments.\n",
    "\n",
    "Initialization options:\n",
    "\n",
    "--batch                     do not do interactive display; implies -q\n",
    "--chdir DIR                 change to directory DIR\n",
    "--daemon, --bg-daemon[=NAME] start a (named) server in the background\n",
    "--fg-daemon[=NAME]          start a (named) server in the foreground\n",
    "--debug-init                enable Emacs Lisp debugger for init file\n",
    "--display, -d DISPLAY       use X server DISPLAY\n",
    "--no-build-details          do not add build details such as time stamps\n",
    "--no-desktop                do not load a saved desktop\n",
    "--no-init-file, -q          load neither ~/.emacs nor default.el\n",
    "--no-loadup, -nl            do not load loadup.el into bare Emacs\n",
    "--no-site-file              do not load site-start.el\n",
    "--no-x-resources            do not load X resources\n",
    "--no-site-lisp, -nsl        do not add site-lisp directories to load-path\n",
    "--no-splash                 do not display a splash screen on startup\n",
    "--no-window-system, -nw     do not communicate with X, ignoring $DISPLAY\n",
    "--init-directory=DIR        use DIR when looking for the Emacs init files.\n",
    "--quick, -Q                 equivalent to:\n",
    "                              -q --no-site-file --no-site-lisp --no-splash\n",
    "                              --no-x-resources\n",
    "--script FILE               run FILE as an Emacs Lisp script\n",
    "-x                          to be used in #!/usr/bin/emacs -x\n",
    "                              and has approximately the same meaning\n",
    "                              as -Q --script\n",
    "--terminal, -t DEVICE       use DEVICE for terminal I/O\n",
    "--user, -u USER             load ~USER/.emacs instead of your own\n",
    "\n",
    "Action options:\n",
    "\n",
    "FILE                    visit FILE\n",
    "+LINE                   go to line LINE in next FILE\n",
    "+LINE:COLUMN            go to line LINE, column COLUMN, in next FILE\n",
    "--directory, -L DIR     prepend DIR to load-path (with :DIR, append DIR)\n",
    "--eval EXPR             evaluate Emacs Lisp expression EXPR\n",
    "--execute EXPR          evaluate Emacs Lisp expression EXPR\n",
    "--file FILE             visit FILE\n",
    "--find-file FILE        visit FILE\n",
    "--funcall, -f FUNC      call Emacs Lisp function FUNC with no arguments\n",
    "--insert FILE           insert contents of FILE into current buffer\n",
    "--kill                  exit without asking for confirmation\n",
    "--load, -l FILE         load Emacs Lisp FILE using the load function\n",
    "--visit FILE            visit FILE\n",
    "\n",
    "Display options:\n",
    "\n",
    "--background-color, -bg COLOR   window background color\n",
    "--basic-display, -D             disable many display features;\n",
    "                                  used for debugging Emacs\n",
    "--border-color, -bd COLOR       main border color\n",
    "--border-width, -bw WIDTH       width of main border\n",
    "--cursor-color, -cr COLOR       color of the Emacs cursor indicating point\n",
    "--font, -fn FONT                default font; must be fixed-width\n",
    "--foreground-color, -fg COLOR   window foreground color\n",
    "--fullheight, -fh               make the first frame high as the screen\n",
    "--fullscreen, -fs               make the first frame fullscreen\n",
    "--fullwidth, -fw                make the first frame wide as the screen\n",
    "--maximized, -mm                make the first frame maximized\n",
    "--geometry, -g GEOMETRY         window geometry\n",
    "--iconic                        start Neomacs in iconified state\n",
    "--internal-border, -ib WIDTH    width between text and main border\n",
    "--line-spacing, -lsp PIXELS     additional space to put between lines\n",
    "--mouse-color, -ms COLOR        mouse cursor color in Neomacs window\n",
    "--name NAME                     title for initial Neomacs frame\n",
    "--no-blinking-cursor, -nbc      disable blinking cursor\n",
    "--reverse-video, -r, -rv        switch foreground and background\n",
    "--title, -T TITLE               title for initial Neomacs frame\n",
    "--vertical-scroll-bars, -vb     enable vertical scroll bars\n",
    "--xrm XRESOURCES                set additional X resources\n",
    "--parent-id XID                 set parent window\n",
    "--help                          display this help and exit\n",
    "--fingerprint                   output fingerprint and exit\n",
    "--version                       output version information and exit\n",
    "\n",
    "You can generally also specify long option names with a single -; for\n",
    "example, -batch as well as --batch.  You can use any unambiguous\n",
    "abbreviation for a --option.\n",
    "\n",
    "Various environment variables and window system resources also affect\n",
    "the operation of Neomacs.  See the main documentation.\n",
    "\n",
    "Report bugs to https://github.com/eval-exec/neomacs-windows/issues.\n",
);

const BOOTSTRAP_CORE_FEATURES: &[&str] = &["neomacs"];

fn classify_early_cli_action(args: impl IntoIterator<Item = String>) -> Option<EarlyCliAction> {
    let mut args = args.into_iter();
    let program = args.next().unwrap_or_else(|| "neomacs".to_string());
    for arg in args {
        if arg == "--" {
            break;
        }
        match arg.as_str() {
            "--help" | "-help" => {
                return Some(EarlyCliAction::PrintHelp { program });
            }
            "--version" | "-version" => {
                return Some(EarlyCliAction::PrintVersion);
            }
            "--fingerprint" | "-fingerprint" => {
                return Some(EarlyCliAction::PrintFingerprint);
            }
            _ => {}
        }
    }
    None
}

fn render_help_text(program: &str) -> String {
    let mut out = String::new();
    let _ = write!(&mut out, "Usage: {program} [OPTION-OR-FILENAME]...\n\n");
    out.push_str(EARLY_HELP_BODY);
    out
}

fn render_version_text() -> String {
    format!(
        "Neomacs {}\nStandalone Rust binary for Neomacs (no C dependency)\n",
        neomacs_display_runtime::VERSION
    )
}

fn render_fingerprint_text() -> String {
    format!("{}\n", neovm_core::emacs_core::pdump::fingerprint_hex())
}

fn render_startup_image_error(err: &neovm_core::emacs_core::error::EvalError) -> String {
    match err {
        neovm_core::emacs_core::error::EvalError::Signal {
            raw_data: Some(payload),
            ..
        } => payload
            .as_symbol_name()
            .map(str::to_owned)
            .or_else(|| payload.as_str().map(str::to_owned))
            .unwrap_or_else(|| format!("{err:?}")),
        _ => format!("{err:?}"),
    }
}

fn parse_startup_options(args: impl IntoIterator<Item = String>) -> Result<StartupOptions, String> {
    use args::{ArgMatch, argmatch, sort_args};

    // GNU `argmatch` works on a `(argc, argv)` pair plus a `*skipptr`
    // index that mirrors the consumed cursor in argv. We model the same
    // shape: `parsed[0]` is the program name (matching argv[0]) and
    // `parsed[1..]` are the user-supplied tokens. The `idx` cursor below
    // is `*skipptr` — `argmatch` looks at `parsed[idx + 1]` so an idx of
    // 0 means "look at the first user token".
    let mut parsed: Vec<String> = args.into_iter().collect();

    // GNU emacs.c:1502 — sort_args runs once before the main matching
    // pass so the parser walks argv in canonical priority order. This
    // also has the effect of moving option/value pairs in front of
    // file-name args, matching how lisp/startup.el's `command-line` and
    // `command-line-1` expect to see them regardless of how the user
    // typed them on the command line.
    sort_args(&mut parsed)?;

    let program = parsed
        .first()
        .cloned()
        .unwrap_or_else(|| "neomacs".to_string());
    let mut forwarded_args = vec![program];
    let mut frontend = FrontendKind::Gui;
    let mut terminal_device = None;
    let mut noninteractive = false;
    let mut temacs_mode = None;
    let mut dump_file_override = None;
    let mut no_site_lisp = false;
    let mut no_loadup = false;
    let mut no_build_details = false;
    let mut idx = 0usize;

    while idx + 1 < parsed.len() {
        // GNU walks argv left-to-right inside `main()` after `sort_args`
        // has reordered things. We don't reorder yet (that's Phase 2), so
        // we walk the original token order. Each `argmatch` call looks at
        // `parsed[idx + 1]`; on a match it advances `idx` past the
        // consumed entry/entries. On no-match we drop to the catch-all
        // forwarding branch and bump `idx` ourselves.
        let next = parsed[idx + 1].as_str();

        // `--` is the terminator: every following token is forwarded
        // verbatim and parsing stops here.
        if next == "--" {
            forwarded_args.extend(parsed[idx + 1..].iter().cloned());
            break;
        }

        // -chdir / --chdir DIR (GNU emacs.c:1538-1561). Must run before
        // any later parsing or file resolution: GNU calls chdir() at
        // line 1549, so subsequent file-name args see the new cwd.
        match argmatch(&parsed, &mut idx, "-chdir", Some("--chdir"), 4, true) {
            ArgMatch::Value(dir) => {
                if let Err(e) = std::env::set_current_dir(&dir) {
                    return Err(format!("neomacs: Can't chdir to {dir}: {e}"));
                }
                continue;
            }
            ArgMatch::MissingValue => {
                return Err("neomacs: option `-chdir' requires an argument".to_string());
            }
            ArgMatch::NoMatch => {}
            ArgMatch::Bare => unreachable!(),
        }

        // -nw / --no-window-system / --no-windows
        // (GNU emacs.c:1696-1697; the -nw row in standard_args[] declares
        // both long aliases with minlen 6.)
        match argmatch(
            &parsed,
            &mut idx,
            "-nw",
            Some("--no-window-system"),
            6,
            false,
        ) {
            ArgMatch::Bare => {
                frontend = FrontendKind::Tty;
                continue;
            }
            ArgMatch::NoMatch => {}
            ArgMatch::Value(_) | ArgMatch::MissingValue => unreachable!(),
        }
        match argmatch(&parsed, &mut idx, "-nw", Some("--no-windows"), 6, false) {
            ArgMatch::Bare => {
                frontend = FrontendKind::Tty;
                continue;
            }
            ArgMatch::NoMatch => {}
            ArgMatch::Value(_) | ArgMatch::MissingValue => unreachable!(),
        }

        // -batch / --batch (GNU emacs.c:1702)
        match argmatch(&parsed, &mut idx, "-batch", Some("--batch"), 5, false) {
            ArgMatch::Bare => {
                noninteractive = true;
                frontend = FrontendKind::Tty;
                continue;
            }
            ArgMatch::NoMatch => {}
            ArgMatch::Value(_) | ArgMatch::MissingValue => unreachable!(),
        }

        // -script FILE / --script FILE (GNU emacs.c:1708-1717). GNU
        // sets noninteractive, then rewrites the matched argv slot to
        // -scriptload (an internal flag picked up later by
        // lisp/startup.el's command-line-1) before re-sorting. We do
        // the same: noninteractive + push -scriptload FILE into the
        // forwarded args. Lisp's command-line-1 in startup.el:2841 will
        // pick it up and load FILE.
        match argmatch(&parsed, &mut idx, "-script", Some("--script"), 3, true) {
            ArgMatch::Value(script_file) => {
                noninteractive = true;
                frontend = FrontendKind::Tty;
                forwarded_args.push("-scriptload".to_string());
                forwarded_args.push(script_file);
                continue;
            }
            ArgMatch::MissingValue => {
                return Err("neomacs: option `-script' requires an argument".to_string());
            }
            ArgMatch::NoMatch => {}
            ArgMatch::Bare => unreachable!(),
        }

        // -x (GNU emacs.c:2132-2140). The `-x` form of shebang scripts:
        //   #!/usr/bin/neomacs -x
        // GNU sets noninteractive AND no_site_lisp, then rewrites argv
        // by replacing `-x` with the internal `-scripteval` flag.
        // lisp/startup.el:2841 picks up `-scripteval` and runs the
        // following file as evaluated text rather than loaded code.
        match argmatch(&parsed, &mut idx, "-x", None, 1, false) {
            ArgMatch::Bare => {
                noninteractive = true;
                frontend = FrontendKind::Tty;
                no_site_lisp = true;
                forwarded_args.push("-scripteval".to_string());
                continue;
            }
            ArgMatch::NoMatch => {}
            ArgMatch::Value(_) | ArgMatch::MissingValue => unreachable!(),
        }

        // -nl / --no-loadup (GNU emacs.c:2031-2032). Skip loading
        // loadup.el under RuntimeMode::Raw. Consumed entirely; no
        // forwarding.
        match argmatch(&parsed, &mut idx, "-nl", Some("--no-loadup"), 6, false) {
            ArgMatch::Bare => {
                no_loadup = true;
                continue;
            }
            ArgMatch::NoMatch => {}
            ArgMatch::Value(_) | ArgMatch::MissingValue => unreachable!(),
        }

        // -nsl / --no-site-lisp (GNU emacs.c:2034-2035). Drops site-lisp
        // directories from load-path before lread.c builds it.
        // Consumed entirely; no forwarding.
        match argmatch(&parsed, &mut idx, "-nsl", Some("--no-site-lisp"), 11, false) {
            ArgMatch::Bare => {
                no_site_lisp = true;
                continue;
            }
            ArgMatch::NoMatch => {}
            ArgMatch::Value(_) | ArgMatch::MissingValue => unreachable!(),
        }

        // -no-build-details / --no-build-details (GNU emacs.c:2037-2038).
        // Inverts the GNU `build_details` global; when set, build-time
        // strings (e.g. `emacs-build-time`) should be cleared.
        // Consumed entirely; no forwarding.
        match argmatch(
            &parsed,
            &mut idx,
            "-no-build-details",
            Some("--no-build-details"),
            7,
            false,
        ) {
            ArgMatch::Bare => {
                no_build_details = true;
                continue;
            }
            ArgMatch::NoMatch => {}
            ArgMatch::Value(_) | ArgMatch::MissingValue => unreachable!(),
        }

        // -temacs / --temacs (GNU emacs.c:1364). Forward the original
        // token(s) verbatim so any later consumer (Lisp or another
        // raw_loadup pass) sees the same shape GNU does — emacs.c
        // does NOT rewrite this slot, only the display slot.
        let pre_idx = idx;
        match argmatch(&parsed, &mut idx, "-temacs", Some("--temacs"), 8, true) {
            ArgMatch::Value(value) => {
                temacs_mode = Some(parse_temacs_mode(&value)?);
                for slot in &parsed[pre_idx + 1..=idx] {
                    forwarded_args.push(slot.clone());
                }
                continue;
            }
            ArgMatch::MissingValue => {
                return Err("neomacs: option `-temacs' requires an argument".to_string());
            }
            ArgMatch::NoMatch => {}
            ArgMatch::Bare => unreachable!(),
        }

        // -dump-file / --dump-file (GNU emacs.c:942, 991). Same forward-
        // verbatim treatment as --temacs.
        let pre_idx = idx;
        match argmatch(
            &parsed,
            &mut idx,
            "-dump-file",
            Some("--dump-file"),
            6,
            true,
        ) {
            ArgMatch::Value(value) => {
                dump_file_override = Some(PathBuf::from(&value));
                for slot in &parsed[pre_idx + 1..=idx] {
                    forwarded_args.push(slot.clone());
                }
                continue;
            }
            ArgMatch::MissingValue => {
                return Err("neomacs: option `-dump-file' requires an argument".to_string());
            }
            ArgMatch::NoMatch => {}
            ArgMatch::Bare => unreachable!(),
        }

        // -t / --terminal (GNU emacs.c:1665)
        match argmatch(&parsed, &mut idx, "-t", Some("--terminal"), 4, true) {
            ArgMatch::Value(device) => {
                frontend = FrontendKind::Tty;
                terminal_device = Some(device);
                continue;
            }
            ArgMatch::MissingValue => {
                return Err("neomacs: option `-t' requires an argument".to_string());
            }
            ArgMatch::NoMatch => {}
            ArgMatch::Bare => unreachable!(),
        }

        // -d / --display / -display (GNU emacs.c:2097-2099 — peek + roll
        // back). Our window backend uses winit which reads `DISPLAY` from
        // the environment, so we don't need to act on the value, but we
        // still consume it from argv structurally and re-forward it so
        // Lisp's `command-line-1` sees it where GNU does.
        match argmatch(&parsed, &mut idx, "-d", Some("--display"), 3, true) {
            ArgMatch::Value(value) => {
                forwarded_args.push("-d".to_string());
                forwarded_args.push(value);
                continue;
            }
            ArgMatch::MissingValue => {
                return Err("neomacs: option `-d' requires an argument".to_string());
            }
            ArgMatch::NoMatch => {}
            ArgMatch::Bare => unreachable!(),
        }
        // -display alone (no long form) — GNU emacs.c:2099 has lstr = 0
        // for this row. Use a None lstr to match.
        match argmatch(&parsed, &mut idx, "-display", None, 0, true) {
            ArgMatch::Value(value) => {
                forwarded_args.push("-display".to_string());
                forwarded_args.push(value);
                continue;
            }
            ArgMatch::MissingValue => {
                return Err("neomacs: option `-display' requires an argument".to_string());
            }
            ArgMatch::NoMatch => {}
            ArgMatch::Bare => unreachable!(),
        }

        // No flag matched at this position: forward verbatim.
        forwarded_args.push(parsed[idx + 1].clone());
        idx += 1;
    }

    // -Q / --quick / -quick PEEK (GNU emacs.c:2123-2130). GNU walks
    // argv one more time looking for any of these three spellings; if
    // found, it sets `no_site_lisp = 1` and leaves the flag in argv so
    // lisp/startup.el (`command-line` at lisp/startup.el:1404) can also
    // act on it. Critically the flag is NOT consumed — only `no_site_lisp`
    // is updated as a side effect. This is the only "peek but do not
    // consume" idiom in GNU's parser.
    //
    // We replicate the same scan over `forwarded_args` (the survivors
    // of the consume pass) since that's what the rest of startup will
    // see. Skip if `no_site_lisp` is already set (e.g. by an earlier
    // -nsl or -x), matching GNU's `if (! no_site_lisp)` guard.
    if !no_site_lisp
        && forwarded_args
            .iter()
            .skip(1)
            .any(|a| a == "-Q" || a == "--quick" || a == "-quick")
    {
        no_site_lisp = true;
    }

    Ok(StartupOptions {
        frontend,
        forwarded_args,
        terminal_device,
        noninteractive,
        temacs_mode,
        dump_file_override,
        no_site_lisp,
        no_loadup,
        no_build_details,
    })
}

fn parse_temacs_mode(value: &str) -> Result<LoadupDumpMode, String> {
    match value {
        "pbootstrap" => Ok(LoadupDumpMode::Pbootstrap),
        "pdump" => Ok(LoadupDumpMode::Pdump),
        other => Err(format!("neomacs: invalid --temacs mode `{other}`")),
    }
}

fn bootstrap_display_config(frontend: FrontendKind) -> BootstrapDisplayConfig {
    match frontend {
        FrontendKind::Gui => BootstrapDisplayConfig {
            frontend,
            color_cells: 16777216,
            // GNU `frame--current-background-mode` defaults GUI frames to
            // `light` unless a real background color or terminal default says
            // otherwise.  Live frame-parameter updates recompute this later.
            background_mode: "light",
        },
        FrontendKind::Tty => BootstrapDisplayConfig {
            frontend,
            color_cells: detect_tty_color_cells(),
            background_mode: detect_tty_background_mode(),
        },
    }
}

impl BootstrapDisplayConfig {
    fn window_system_symbol(self) -> Option<&'static str> {
        match self.frontend {
            FrontendKind::Gui => Some(gui_window_system_symbol()),
            FrontendKind::Tty => None,
        }
    }

    fn display_type_symbol(self) -> &'static str {
        if self.color_cells > 0 {
            "color"
        } else {
            "mono"
        }
    }
}

fn detect_tty_runtime() -> TerminalRuntimeConfig {
    let tty_type = std::env::var("TERM").ok().filter(|value| !value.is_empty());
    TerminalRuntimeConfig::interactive(tty_type, detect_tty_color_cells())
}

fn detect_tty_color_cells() -> i64 {
    let colorterm = std::env::var("COLORTERM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if colorterm.contains("truecolor") || colorterm.contains("24bit") {
        return 16777216;
    }

    let term = std::env::var("TERM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if term.is_empty() || term == "dumb" {
        return 0;
    }
    if term.contains("256color") {
        return 256;
    }
    8
}

fn detect_tty_background_mode() -> &'static str {
    let Some(colorfgbg) = std::env::var("COLORFGBG").ok() else {
        return "dark";
    };
    let Some(background) = colorfgbg
        .split(';')
        .next_back()
        .and_then(|value| value.parse::<i32>().ok())
    else {
        return "dark";
    };

    if (7..=15).contains(&background) {
        "light"
    } else {
        "dark"
    }
}

fn startup_dimensions(frontend: FrontendKind, frame_metrics: BootstrapFrameMetrics) -> (u32, u32) {
    match frontend {
        FrontendKind::Gui => {
            // GNU gui_figure_window_size seeds the first GUI frame from an
            // 80x36 text grid using the default frame font metrics.
            let cols = 80u32;
            let lines = 36u32;
            let width = (cols as f32 * frame_metrics.char_width).round() as u32;
            let height = (lines as f32 * frame_metrics.char_height).round() as u32;
            (width.max(200), height.max(100))
        }
        FrontendKind::Tty => {
            // TTY frames use 1x1 character cells (GNU Emacs frame.c:1184-1185),
            // so frame dimensions are in character cells, not pixels.
            let (cols, rows) = query_terminal_size_cells().unwrap_or((80, 25));
            (cols as u32, rows as u32)
        }
    }
}

#[cfg(unix)]
fn query_terminal_size_cells() -> Option<(u16, u16)> {
    use std::mem::MaybeUninit;

    unsafe {
        let mut winsize = MaybeUninit::<libc::winsize>::uninit();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, winsize.as_mut_ptr()) == 0 {
            let winsize = winsize.assume_init();
            if winsize.ws_col > 0 && winsize.ws_row > 0 {
                return Some((winsize.ws_col, winsize.ws_row));
            }
        }
    }
    None
}

#[cfg(not(unix))]
fn query_terminal_size_cells() -> Option<(u16, u16)> {
    None
}

enum FrontendHandle {
    Gui(RenderThread),
    /// Single-thread TTY path: input reader only, rendering via TtyRif on eval thread.
    TtyRifInput(tty_frontend::TtyInputReader),
    Batch,
}

impl FrontendHandle {
    fn join(self) {
        match self {
            Self::Gui(handle) => handle.join(),
            Self::TtyRifInput(handle) => handle.join(),
            Self::Batch => {}
        }
    }
}

struct PrimaryWindowDisplayHost {
    cmd_tx: crossbeam_channel::Sender<RenderCommand>,
    primary_window_adopted: bool,
    primary_frame_id: Option<neovm_core::window::FrameId>,
    last_window_titles: Mutex<HashMap<neovm_core::window::FrameId, String>>,
    font_metrics: Option<FontMetricsService>,
    primary_window_size: SharedPrimaryWindowSize,
    image_dimensions: SharedImageDimensions,
    resolved_images: Mutex<HashMap<ImageResolveRequest, ResolvedImage>>,
}

struct TtyTerminalHost {
    cmd_tx: crossbeam_channel::Sender<RenderCommand>,
}

impl TerminalHost for TtyTerminalHost {
    fn suspend_tty(&mut self) -> Result<(), String> {
        self.cmd_tx
            .send(RenderCommand::SuspendTty)
            .map_err(|err| format!("failed to suspend tty frontend: {err}"))
    }

    fn resume_tty(&mut self) -> Result<(), String> {
        self.cmd_tx
            .send(RenderCommand::ResumeTty)
            .map_err(|err| format!("failed to resume tty frontend: {err}"))
    }

    fn delete_terminal(&mut self) -> Result<(), String> {
        self.cmd_tx
            .send(RenderCommand::Shutdown)
            .map_err(|err| format!("failed to delete tty terminal frontend: {err}"))
    }
}

fn should_enable_live_tty_io(startup: &StartupOptions) -> bool {
    startup.frontend == FrontendKind::Tty && !startup.noninteractive
}

fn maybe_install_tty_redisplay_callback(evaluator: &mut Context, startup: &StartupOptions) {
    if !should_enable_live_tty_io(startup) {
        return;
    }

    let (cols, rows) = query_terminal_size_cells().unwrap_or((80, 25));
    let mut tty_rif = neomacs_display_protocol::tty_rif::TtyRif::new(cols as usize, rows as usize);
    // TTY frames use 1x1 character cell metrics (GNU Emacs
    // frame.c:1184-1185). Drop the layout engine's cosmic-text
    // FontMetricsService so char_advance,
    // status_line_font_metrics, etc. fall back to the
    // char-cell grid.
    LAYOUT_ENGINE.with(|engine| {
        engine.borrow_mut().disable_cosmic_metrics();
    });
    evaluator.redisplay_fn = Some(Box::new(move |eval: &mut Context| {
        eval.setup_thread_locals();
        run_layout(eval);
        // Extract FrameDisplayState from the layout engine's thread-local
        run_tty_rif_redisplay(&mut tty_rif);
    }));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PrimaryWindowSize {
    width: u32,
    height: u32,
}

type SharedPrimaryWindowSize = Arc<Mutex<PrimaryWindowSize>>;

const HOST_IMAGE_ID_START: u32 = 0x4000_0000;
static HOST_IMAGE_ID_ALLOCATOR: AtomicU32 = AtomicU32::new(HOST_IMAGE_ID_START);

fn next_host_image_id() -> u32 {
    HOST_IMAGE_ID_ALLOCATOR.fetch_add(1, Ordering::Relaxed)
}

fn wait_for_image_dimensions(
    shared: &SharedImageDimensions,
    id: u32,
    timeout: Duration,
) -> Option<(u32, u32)> {
    let (lock, cvar) = &**shared;
    let deadline = Instant::now() + timeout;
    let mut dims = match lock.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    loop {
        if let Some(size) = dims.get(&id).copied() {
            return Some(size);
        }
        let remaining = deadline.checked_duration_since(Instant::now())?;
        match cvar.wait_timeout(dims, remaining) {
            Ok((guard, result)) => {
                dims = guard;
                if result.timed_out() {
                    return dims.get(&id).copied();
                }
            }
            Err(poisoned) => {
                let (guard, _) = poisoned.into_inner();
                dims = guard;
            }
        }
    }
}

fn read_primary_window_size(shared: &SharedPrimaryWindowSize) -> PrimaryWindowSize {
    match shared.lock() {
        Ok(state) => *state,
        Err(poisoned) => *poisoned.into_inner(),
    }
}

fn prime_initial_monitor_snapshot(shared: &SharedMonitorInfo) {
    let (lock, cvar) = &**shared;
    let monitors = match lock.lock() {
        Ok(guard) => {
            if guard.is_empty() {
                match cvar.wait_timeout(guard, Duration::from_secs(2)) {
                    Ok((guard, _)) => guard.clone(),
                    Err(poisoned) => {
                        let (guard, _) = poisoned.into_inner();
                        guard.clone()
                    }
                }
            } else {
                guard.clone()
            }
        }
        Err(poisoned) => poisoned.into_inner().clone(),
    };

    if !monitors.is_empty() {
        set_neomacs_monitor_info(input_bridge::convert_monitor_infos(&monitors));
    }
}

fn record_primary_window_resize(shared: &SharedPrimaryWindowSize, event: &DisplayInputEvent) {
    let DisplayInputEvent::WindowResize {
        width,
        height,
        emacs_frame_id,
    } = event
    else {
        return;
    };

    if *emacs_frame_id != 0 || *width == 0 || *height == 0 {
        return;
    }

    match shared.lock() {
        Ok(mut state) => {
            state.width = *width;
            state.height = *height;
        }
        Err(poisoned) => {
            let mut state = poisoned.into_inner();
            state.width = *width;
            state.height = *height;
        }
    }
}

impl DisplayHost for PrimaryWindowDisplayHost {
    fn realize_gui_frame(&mut self, request: GuiFrameHostRequest) -> Result<(), String> {
        tracing::debug!(
            "PrimaryWindowDisplayHost::realize_gui_frame fid=0x{:x} adopted={} size={}x{} title={}",
            request.frame_id.0,
            self.primary_window_adopted,
            request.width,
            request.height,
            request.title
        );
        if !self.primary_window_adopted {
            self.cmd_tx
                .send(RenderCommand::SetWindowTitle {
                    title: request.title.clone(),
                })
                .map_err(|err| format!("failed to update primary window title: {err}"))?;
            self.cmd_tx
                .send(RenderCommand::SetFrameGeometryHints {
                    emacs_frame_id: 0,
                    geometry_hints: request.geometry_hints,
                })
                .map_err(|err| format!("failed to update primary window geometry hints: {err}"))?;
            // The opening GUI frame adopts the already-existing primary host
            // window. Do not push stale Lisp bootstrap dimensions back into
            // that window during adoption; host resize events remain the
            // source of truth until the window is fully realized.
            self.primary_window_adopted = true;
            self.primary_frame_id = Some(request.frame_id);
        } else {
            self.cmd_tx
                .send(RenderCommand::CreateWindow {
                    emacs_frame_id: request.frame_id.0,
                    width: request.width,
                    height: request.height,
                    title: request.title.clone(),
                    geometry_hints: request.geometry_hints,
                })
                .map_err(|err| format!("failed to create additional GUI window: {err}"))?;
        }
        self.last_window_titles
            .lock()
            .map_err(|err| format!("failed to cache GUI frame title: {err}"))?
            .insert(request.frame_id, request.title);
        Ok(())
    }

    fn opening_gui_frame_pending(&self) -> bool {
        !self.primary_window_adopted
    }

    fn resize_gui_frame(&mut self, request: GuiFrameHostRequest) -> Result<(), String> {
        let emacs_frame_id = if self.primary_frame_id == Some(request.frame_id) {
            0
        } else {
            request.frame_id.0
        };
        tracing::debug!(
            "PrimaryWindowDisplayHost::resize_gui_frame fid=0x{:x} route=0x{:x} size={}x{}",
            request.frame_id.0,
            emacs_frame_id,
            request.width,
            request.height
        );
        self.cmd_tx
            .send(RenderCommand::ResizeWindow {
                emacs_frame_id,
                width: request.width,
                height: request.height,
                geometry_hints: request.geometry_hints,
            })
            .map_err(|err| format!("failed to resize GUI frame: {err}"))?;
        Ok(())
    }

    fn set_gui_frame_geometry_hints(
        &mut self,
        frame_id: neovm_core::window::FrameId,
        geometry_hints: neovm_core::window::GuiFrameGeometryHints,
    ) -> Result<(), String> {
        let emacs_frame_id =
            if !self.primary_window_adopted || self.primary_frame_id == Some(frame_id) {
                0
            } else {
                frame_id.0
            };
        self.cmd_tx
            .send(RenderCommand::SetFrameGeometryHints {
                emacs_frame_id,
                geometry_hints,
            })
            .map_err(|err| format!("failed to update GUI frame geometry hints: {err}"))?;
        Ok(())
    }

    fn set_gui_frame_title(
        &mut self,
        frame_id: neovm_core::window::FrameId,
        title: String,
    ) -> Result<(), String> {
        let mut cached_titles = self
            .last_window_titles
            .lock()
            .map_err(|err| format!("failed to cache GUI frame title: {err}"))?;
        if cached_titles
            .get(&frame_id)
            .is_some_and(|cached| cached == &title)
        {
            return Ok(());
        }
        cached_titles.insert(frame_id, title.clone());
        drop(cached_titles);

        let emacs_frame_id = if self.primary_frame_id == Some(frame_id) {
            0
        } else {
            frame_id.0
        };
        self.cmd_tx
            .send(RenderCommand::SetFrameWindowTitle {
                emacs_frame_id,
                title,
            })
            .map_err(|err| format!("failed to update GUI frame title: {err}"))?;
        Ok(())
    }

    fn current_primary_window_size(&self) -> Option<GuiFrameHostSize> {
        if self.primary_window_adopted {
            return None;
        }
        let state = read_primary_window_size(&self.primary_window_size);
        Some(GuiFrameHostSize {
            width: state.width,
            height: state.height,
        })
    }

    fn resolve_font_for_char(
        &mut self,
        request: FontResolveRequest,
    ) -> Result<Option<ResolvedFontMatch>, String> {
        let requested_family_storage = request.face.family_runtime_string_owned();
        let requested_family = requested_family_storage.as_deref().unwrap_or("Monospace");
        let requested_weight = request.face.weight.unwrap_or(FontWeight::NORMAL).0;
        let requested_italic = request
            .face
            .slant
            .map(|slant| slant.is_italic())
            .unwrap_or(false);
        let font_size = font_size_px_for_face(&request.face);
        let selected = self
            .font_metrics
            .get_or_insert_with(FontMetricsService::new)
            .select_font_for_char(
                request.character,
                requested_family,
                requested_weight,
                requested_italic,
                font_size,
            );
        tracing::debug!(
            target: "neomacs::font_at",
            character = %request.character,
            requested_family,
            requested_weight,
            requested_italic,
            font_size,
            request_face = ?request.face,
            selected = ?selected,
            "display host resolved font-at request"
        );
        Ok(selected.map(|font| ResolvedFontMatch {
            family: font.family,
            foundry: None,
            weight: font.weight,
            slant: font.slant,
            width: font.width,
            postscript_name: font.postscript_name,
        }))
    }

    fn resolve_frame_font(
        &mut self,
        _frame_id: FrameId,
        face: neovm_core::face::Face,
    ) -> Result<Option<ResolvedFrameFont>, String> {
        let requested_family_storage = face.family_runtime_string_owned();
        let requested_family = requested_family_storage.as_deref().unwrap_or("Monospace");
        let requested_weight = face.weight.unwrap_or(FontWeight::NORMAL).0;
        let requested_italic = face.slant.map(|slant| slant.is_italic()).unwrap_or(false);
        let font_size = font_size_px_for_face(&face);
        let selected = self
            .font_metrics
            .get_or_insert_with(FontMetricsService::new)
            .select_font_for_char(
                'M',
                requested_family,
                requested_weight,
                requested_italic,
                font_size,
            );
        let Some(font) = selected else {
            return Ok(None);
        };
        let metrics = self
            .font_metrics
            .get_or_insert_with(FontMetricsService::new)
            .font_metrics(
                &font.family,
                font.weight.0,
                font.slant.is_italic(),
                font_size,
            );
        Ok(Some(ResolvedFrameFont {
            family: font.family,
            foundry: None,
            weight: font.weight,
            slant: font.slant,
            width: font.width,
            postscript_name: font.postscript_name,
            font_size_px: font_size,
            char_width: metrics.char_width.max(1.0),
            line_height: metrics.line_height.max(1.0),
        }))
    }

    fn resolve_font_for_spec(
        &mut self,
        request: FontSpecResolveRequest,
    ) -> Result<Option<ResolvedFontSpecMatch>, String> {
        let matched = neomacs_layout_engine::fontconfig::find_font_for_spec(
            request.family.as_deref(),
            request.registry.as_deref(),
            request.lang.as_deref(),
            request.weight.map(|weight| weight.0),
            request.slant,
        );
        Ok(matched.map(|font| ResolvedFontSpecMatch {
            family: font.family,
            registry: Some("iso10646-1".to_string()),
            weight: font.weight.map(FontWeight),
            slant: Some(font.slant),
            width: font.width,
            spacing: font.spacing,
            postscript_name: font.postscript_name,
        }))
    }

    fn resolve_image(&self, request: ImageResolveRequest) -> Result<Option<ResolvedImage>, String> {
        let cache = match self.resolved_images.lock() {
            Ok(cache) => cache,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(image) = cache.get(&request) {
            return Ok(Some(image.clone()));
        }
        drop(cache);

        let image_id = next_host_image_id();
        match &request.source {
            ImageResolveSource::File(path) => {
                self.cmd_tx
                    .send(RenderCommand::ImageLoadFile {
                        id: image_id,
                        path: path.clone(),
                        max_width: request.max_width,
                        max_height: request.max_height,
                        fg_color: request.fg_color,
                        bg_color: request.bg_color,
                    })
                    .map_err(|err| format!("failed to queue image load: {err}"))?;
            }
            ImageResolveSource::Data(data) => {
                self.cmd_tx
                    .send(RenderCommand::ImageLoadData {
                        id: image_id,
                        data: data.clone(),
                        max_width: request.max_width,
                        max_height: request.max_height,
                        fg_color: request.fg_color,
                        bg_color: request.bg_color,
                    })
                    .map_err(|err| format!("failed to queue image data load: {err}"))?;
            }
        }

        let Some((width, height)) =
            wait_for_image_dimensions(&self.image_dimensions, image_id, Duration::from_secs(1))
        else {
            return Ok(None);
        };

        let resolved = ResolvedImage {
            image_id,
            width,
            height,
        };
        match self.resolved_images.lock() {
            Ok(mut cache) => {
                cache.insert(request, resolved.clone());
            }
            Err(poisoned) => {
                let mut cache = poisoned.into_inner();
                cache.insert(request, resolved.clone());
            }
        }
        Ok(Some(resolved))
    }
}

fn frame_host_title(eval: &mut Context, frame_id: FrameId) -> String {
    let Some((selected_window_id, buffer_id, fallback_title, target_cols)) =
        eval.frame_manager().get(frame_id).map(|frame| {
            let fallback_title = frame.host_title_runtime_string_owned();
            let buffer_id = match frame.selected_window() {
                Some(Window::Leaf { buffer_id, .. }) => Some(*buffer_id),
                _ => None,
            };
            let target_cols = if frame.char_width > 0.0 {
                ((frame.width as f32) / frame.char_width.max(1.0))
                    .floor()
                    .max(1.0) as usize
            } else {
                frame.width.max(1) as usize
            };
            (
                frame.selected_window,
                buffer_id,
                fallback_title,
                target_cols.max(1),
            )
        })
    else {
        return "Neomacs".to_string();
    };

    let format = eval
        .obarray()
        .symbol_value("frame-title-format")
        .copied()
        .unwrap_or(Value::NIL);
    if format.is_nil() {
        return fallback_title;
    }

    let rendered = neovm_core::emacs_core::xdisp::format_mode_line_for_display(
        eval,
        format,
        Value::make_window(selected_window_id.0),
        buffer_id
            .map(|buffer_id| Value::make_buffer(buffer_id))
            .unwrap_or(Value::NIL),
        target_cols,
    );
    rendered.as_str().unwrap_or(&fallback_title).to_owned()
}

fn adopt_existing_primary_gui_frame(eval: &mut Context) -> Result<(), String> {
    if eval
        .display_host
        .as_ref()
        .is_none_or(|host| !host.opening_gui_frame_pending())
    {
        return Ok(());
    }
    let Some((frame_id, width, height)) = eval
        .frame_manager()
        .selected_frame()
        .map(|frame| (frame.id, frame.width, frame.height))
    else {
        return Ok(());
    };
    let title = frame_host_title(eval, frame_id);
    let geometry_hints = eval
        .frame_manager()
        .get(frame_id)
        .map(|frame| frame.gui_geometry_hints())
        .ok_or_else(|| "selected GUI frame disappeared before adoption".to_string())?;
    let Some(host) = eval.display_host.as_mut() else {
        return Ok(());
    };
    host.realize_gui_frame(GuiFrameHostRequest {
        frame_id,
        width,
        height,
        title,
        geometry_hints,
    })
}

fn sync_live_gui_frame_titles(eval: &mut Context) {
    let frame_ids = eval.frame_manager().frame_list();
    for frame_id in frame_ids {
        let is_gui_frame = eval
            .frame_manager()
            .get(frame_id)
            .is_some_and(|frame| frame.effective_window_system().is_some());
        if !is_gui_frame {
            continue;
        }
        let title = frame_host_title(eval, frame_id);
        if let Some(host) = eval.display_host.as_mut() {
            let _ = host.set_gui_frame_title(frame_id, title);
        }
    }
}

fn seed_gnu_default_gui_chrome_modes(eval: &mut Context) {
    eval.set_variable("menu-bar-mode", Value::T);
    eval.set_variable("tool-bar-mode", Value::T);
}

fn ensure_gnu_tool_bar_setup(eval: &mut Context) {
    let needs_setup = match eval.eval_str(
        "(and (fboundp 'tool-bar-setup) tool-bar-mode (= 1 (length (default-value 'tool-bar-map))))",
    ) {
        Ok(value) => value.is_truthy(),
        Err(err) => {
            tracing::warn!("failed probing tool-bar setup state: {err}");
            false
        }
    };
    if !needs_setup {
        return;
    }
    if let Err(err) = eval.eval_str("(tool-bar-setup)") {
        tracing::warn!("failed running GNU tool-bar setup: {err}");
    }
}

fn sync_selected_gui_chrome_state(eval: &mut Context) {
    let menu_enabled = !eval
        .obarray()
        .symbol_value("menu-bar-mode")
        .copied()
        .unwrap_or(Value::NIL)
        .is_nil();
    let tool_enabled = !eval
        .obarray()
        .symbol_value("tool-bar-mode")
        .copied()
        .unwrap_or(Value::NIL)
        .is_nil();
    if tool_enabled {
        ensure_gnu_tool_bar_setup(eval);
    }

    let menu_items = if menu_enabled {
        collect_gui_menu_bar_items(eval)
    } else {
        Vec::new()
    };
    let tool_items = if tool_enabled {
        collect_gui_tool_bar_items(eval)
    } else {
        Vec::new()
    };

    let mut geometry_hints = None;
    if let Some(frame) = eval.frame_manager_mut().selected_frame_mut() {
        if frame.effective_window_system().is_none() {
            return;
        }
        frame.set_parameter(
            "menu-bar-lines",
            Value::fixnum(if menu_items.is_empty() { 0 } else { 1 }),
        );
        frame.set_parameter(
            "tool-bar-lines",
            Value::fixnum(if tool_items.is_empty() { 0 } else { 1 }),
        );
        frame.sync_menu_bar_height_from_parameters();
        frame.sync_tool_bar_height_from_parameters();
        geometry_hints = Some((frame.id, frame.gui_geometry_hints()));
    }

    if let Some((frame_id, hints)) = geometry_hints
        && let Some(host) = eval.display_host.as_mut()
    {
        let _ = host.set_gui_frame_geometry_hints(frame_id, hints);
    }
}

fn font_size_px_for_face(face: &neovm_core::face::Face) -> f32 {
    let default_font_size = face_height_to_pixels(100);
    match &face.height {
        Some(FaceHeight::Absolute(tenths)) => face_height_to_pixels(*tenths),
        Some(FaceHeight::Relative(scale)) => default_font_size * (*scale as f32),
        None => default_font_size,
    }
}

fn create_startup_evaluator_for_mode(mode: RuntimeMode, startup: &StartupOptions) -> Context {
    match mode {
        RuntimeMode::Raw => {
            let startup_surface = raw_loadup_startup_surface(startup, None);
            neovm_core::emacs_core::load::create_bootstrap_evaluator_with_startup_surface(
                BOOTSTRAP_CORE_FEATURES,
                None,
                Some(&startup_surface),
            )
            .expect("raw bootstrap should succeed")
        }
        RuntimeMode::BootstrapUse => {
            neovm_core::emacs_core::load::load_runtime_image_with_features(
                RuntimeImageRole::Bootstrap,
                BOOTSTRAP_CORE_FEATURES,
                startup.dump_file_override.as_deref(),
            )
            .unwrap_or_else(|err| {
                panic!(
                    "bootstrap image should load: {}",
                    render_startup_image_error(&err)
                )
            })
        }
        RuntimeMode::FinalRun => neovm_core::emacs_core::load::load_runtime_image_with_features(
            RuntimeImageRole::Final,
            BOOTSTRAP_CORE_FEATURES,
            startup.dump_file_override.as_deref(),
        )
        .unwrap_or_else(|err| {
            panic!(
                "final image should load: {}",
                render_startup_image_error(&err)
            )
        }),
    }
}

fn raw_loadup_command_line(
    startup: &StartupOptions,
    dump_mode: Option<LoadupDumpMode>,
) -> Vec<String> {
    let mut args = startup.forwarded_args.clone();
    if args.is_empty() {
        args.push(RuntimeMode::Raw.binary_name().to_string());
    }

    // GNU emacs.c:2578 — `if (!no_loadup) ... loadup.el`. We achieve the
    // same effect at the argv level by skipping the `-l loadup` splice
    // when --no-loadup is set. The `--temacs=...` mode below still
    // appends so that the rest of dump bookkeeping continues to run.
    let has_internal_loadup_marker =
        matches!(args.get(1).map(String::as_str), Some("-l" | "--load"))
            && args.get(2).map(String::as_str) == Some("loadup");
    if !startup.no_loadup && !has_internal_loadup_marker {
        args.splice(1..1, ["-l".to_string(), "loadup".to_string()]);
    }

    if let Some(dump_mode) = dump_mode {
        let has_temacs_mode = args
            .iter()
            .any(|arg| arg == "-temacs" || arg == "--temacs" || arg.starts_with("--temacs="));
        if !has_temacs_mode {
            args.push(format!("--temacs={}", dump_mode.as_gnu_string()));
        }
    }

    args
}

fn raw_loadup_startup_surface(
    startup: &StartupOptions,
    dump_mode: Option<LoadupDumpMode>,
) -> LoadupStartupSurface {
    LoadupStartupSurface {
        command_line_args: raw_loadup_command_line(startup, dump_mode),
        noninteractive: startup.noninteractive || dump_mode.is_some(),
    }
}

pub fn run(mode: RuntimeMode) {
    // Always enable full backtraces for debugging low-level runtime crashes.
    if std::env::var("RUST_BACKTRACE").is_err() {
        unsafe {
            std::env::set_var("RUST_BACKTRACE", "1");
        }
    }

    // Increase the stack size to 64 MB, matching GNU Emacs which adjusts
    // RLIMIT_STACK in main(). Deep Elisp evaluation chains (startup.el →
    // normal-top-level → command-line → init → Doom hooks) can exhaust
    // the default 8 MB stack.
    increase_stack_limit();

    // Handle --help / --version with no logging side effects (so e.g.
    // `NEOMACS_LOG_TO_FILE=1 neomacs --help` does not create a stray
    // neomacs-{pid}.log file).
    if let Some(action) = classify_early_cli_action(std::env::args()) {
        match action {
            EarlyCliAction::PrintHelp { program } => {
                print!("{}", render_help_text(&program));
            }
            EarlyCliAction::PrintVersion => {
                print!("{}", render_version_text());
            }
            EarlyCliAction::PrintFingerprint => {
                print!("{}", render_fingerprint_text());
            }
        }
        return;
    }

    // Parse argv before initializing tracing so we know whether this is a
    // GUI or TTY run — logging policy differs between the two (under TTY
    // any tracing output would smash the alt-screen redisplay engine).
    // `parse_startup_options` emits no tracing events, so delaying init
    // past it costs no diagnostics.
    let startup = parse_startup_options(std::env::args()).unwrap_or_else(|message| {
        eprintln!("{message}");
        std::process::exit(1);
    });

    // Initialize tracing with a writer target appropriate to the
    // binary:
    //
    // - `neomacs-temacs` (RuntimeMode::Raw) and `bootstrap-neomacs`
    //   (RuntimeMode::BootstrapUse) are build-time utilities whose
    //   stdout is captured by the xtask driver — they MUST log to
    //   stdout so the build log shows what they are doing. Frontend
    //   is always `Tty` for them (they run with --batch), but they
    //   have no TUI redisplay engine fighting for the pty, so
    //   stdout logging is safe and useful.
    //
    // - `neomacs` (RuntimeMode::FinalRun) is the user-facing binary.
    //   Under a GUI frontend, stdout is captured to a file by the
    //   calling shell (e.g. `> /tmp/neomacs-gui.log 2>&1`), so
    //   LogTarget::Stdout is fine. Under a TTY frontend (`-nw`,
    //   `--batch`), stdout is the alt-screen pty the redisplay
    //   engine is drawing into, so LogTarget::File routes tracing
    //   to a file instead.
    //
    // In all cases `NEOMACS_LOG_FILE=<path>` overrides the file path
    // (and, for LogTarget::Stdout, also adds a file layer alongside
    // stdout).
    let log_target = match mode {
        RuntimeMode::Raw | RuntimeMode::BootstrapUse => neovm_core::logging::LogTarget::Stdout,
        RuntimeMode::FinalRun => match startup.frontend {
            FrontendKind::Gui => neovm_core::logging::LogTarget::Stdout,
            FrontendKind::Tty => neovm_core::logging::LogTarget::File,
        },
    };
    let _logging_guard = neovm_core::logging::init(log_target);

    if mode == RuntimeMode::Raw
        && let Some(temacs_mode) = startup.temacs_mode
    {
        run_temacs_dump_mode(temacs_mode, &startup);
        return;
    }

    tracing::info!(
        "{} {} starting (pure Rust, backend={}, pid={}, mode={:?}, image={:?})",
        mode.binary_name(),
        neomacs_display_runtime::VERSION,
        neomacs_display_runtime::CORE_BACKEND,
        std::process::id(),
        mode,
        mode.dump_image_kind()
    );
    tracing::info!("Startup frontend: {:?}", startup.frontend);
    if let Some(device) = startup.terminal_device.as_deref() {
        tracing::warn!(
            "terminal device {:?} requested; using current tty until explicit device handoff lands",
            device
        );
    }

    let bootstrap_display = bootstrap_display_config(startup.frontend);
    // For TTY, frame dimensions are in character cells (1x1), so we
    // don't need to scan the system font database for font metrics.
    // This avoids ~500ms of FontMetricsService initialization at
    // startup. GUI mode computes real pixel dimensions from font
    // metrics via bootstrap_frame_metrics().
    let frame_metrics = bootstrap_frame_metrics_for_frontend(startup.frontend);
    let (width, height) = startup_dimensions(startup.frontend, frame_metrics);
    // 2. Initialize the evaluator from the canonical bootstrap surface.
    //    GNU loads the dumped bootstrap image here, then lets the outer
    //    command loop evaluate `top-level`/`normal-top-level`.
    let mut evaluator = create_startup_evaluator_for_mode(mode, &startup);
    evaluator.setup_thread_locals();
    evaluator.set_max_depth(1600);
    match startup.frontend {
        FrontendKind::Gui => {
            reset_terminal_host();
            reset_terminal_runtime();
        }
        FrontendKind::Tty => {
            if should_enable_live_tty_io(&startup) {
                reset_terminal_host();
                configure_terminal_runtime(detect_tty_runtime());
            } else {
                reset_terminal_host();
                reset_terminal_runtime();
            }
        }
    }
    // GNU Emacs does NOT disable GC during startup — GC runs normally.
    // The bc_buf refactor and conservative stack scanning ensure all
    // bytecode VM values are reachable during collection.
    evaluator.set_variable("dump-mode", Value::NIL);
    tracing::info!("Context initialized");

    // 3. Bootstrap the host-side initial frame/buffers.
    let _bootstrap = bootstrap_buffers(&mut evaluator, width, height, bootstrap_display);
    let frame_id = evaluator
        .frame_manager()
        .selected_frame()
        .expect("No selected frame after bootstrap")
        .id;
    configure_gnu_startup_state(&mut evaluator, frame_id, &startup);

    maybe_install_startup_phase_trace(&mut evaluator);

    // 4. Create communication channels before entering GNU's outer
    //    recursive-edit command loop. GNU evaluates `top-level` from that
    //    outer loop, not directly from `main`.
    let comms = ThreadComms::new().expect("Failed to create thread comms");
    let (emacs_comms, render_comms) = comms.split();
    let primary_window_size: SharedPrimaryWindowSize =
        Arc::new(Mutex::new(PrimaryWindowSize { width, height }));
    if should_enable_live_tty_io(&startup) {
        set_terminal_host(Box::new(TtyTerminalHost {
            cmd_tx: emacs_comms.cmd_tx.clone(),
        }));
    }
    let gui_image_dimensions: SharedImageDimensions =
        Arc::new((Mutex::new(HashMap::new()), Condvar::new()));

    if startup.frontend == FrontendKind::Gui {
        evaluator.set_display_host(Box::new(PrimaryWindowDisplayHost {
            cmd_tx: emacs_comms.cmd_tx.clone(),
            primary_window_adopted: false,
            primary_frame_id: None,
            last_window_titles: Mutex::new(HashMap::new()),
            font_metrics: None,
            primary_window_size: Arc::clone(&primary_window_size),
            image_dimensions: Arc::clone(&gui_image_dimensions),
            resolved_images: Mutex::new(HashMap::new()),
        }));
        adopt_existing_primary_gui_frame(&mut evaluator)
            .expect("bootstrap GUI frame adoption should succeed");
    }

    // 5. Spawn the frontend loop matching the requested startup mode.
    let frontend = match startup.frontend {
        FrontendKind::Gui => {
            let shared_monitors: SharedMonitorInfo =
                Arc::new((Mutex::new(Vec::new()), Condvar::new()));
            let render_thread = RenderThread::spawn(
                render_comms,
                width,
                height,
                "Neomacs".to_string(),
                Arc::clone(&gui_image_dimensions),
                Arc::clone(&shared_monitors),
            )
            .unwrap_or_else(|err| {
                eprintln!("neomacs: failed to start GUI frontend: {err}");
                std::process::exit(1);
            });
            prime_initial_monitor_snapshot(&shared_monitors);
            tracing::info!("GUI render thread spawned ({}x{})", width, height);
            FrontendHandle::Gui(render_thread)
        }
        FrontendKind::Tty => {
            if startup.noninteractive {
                // Batch mode: no terminal I/O, matching GNU which
                // skips init_display() for --batch (emacs.c:1835).
                tracing::info!("TTY batch mode — skipping terminal init");
                FrontendHandle::Batch
            } else {
                // Single-thread TTY path: terminal init here, rendering via TtyRif
                // on the evaluator thread, input reader on a background thread.
                tty_init_terminal();
                let input_reader = tty_frontend::TtyInputReader::spawn(render_comms);
                tracing::info!("TTY frontend spawned (TtyRif single-thread redisplay)");
                FrontendHandle::TtyRifInput(input_reader)
            }
        }
    };

    // 6. Allocate the glyph buffer, but do not publish a pre-startup frame.
    //
    // GNU's outer command loop evaluates `top-level` before blocking for the
    // first input, and `read_char` performs the first redisplay only after
    // that startup work finishes.  Publishing a frame here paints stale
    // pre-startup face state (notably chrome faces like mode-line) and leaves
    // it visible until some later input or timer happens to trigger a
    // redisplay.  Keep the frontend window alive, but let the first real frame
    // come from the command loop's redisplay path.
    // 7. Create input bridge: convert display runtime events → keyboard events.
    //
    // GNU Emacs does NOT initialize terminal I/O in --batch mode.
    // The evaluator runs without any input receiver, so
    // `input_rx.is_none()` correctly signals batch mode throughout
    // the keyboard/command-loop code. This prevents blocking on
    // `rx.recv()` in read_char_with_timeout and avoids spawning
    // unnecessary threads.
    if !startup.noninteractive {
        let (input_tx, input_rx) = crossbeam_channel::unbounded();
        let display_input_rx = emacs_comms.input_rx;
        let primary_window_size_for_input = Arc::clone(&primary_window_size);
        std::thread::Builder::new()
            .name("input-bridge".to_string())
            .spawn(move || {
                while let Ok(event) = display_input_rx.recv() {
                    tracing::info!("input-bridge: received event");
                    record_primary_window_resize(&primary_window_size_for_input, &event);
                    if let Some(kb_event) = input_bridge::convert_display_event(event) {
                        tracing::info!("input-bridge: converted to kb event");
                        if input_tx.send(kb_event).is_err() {
                            break; // Context dropped
                        }
                    }
                }
            })
            .expect("Failed to spawn input bridge thread");

        // 8. Connect evaluator to input system
        let wakeup_fd = emacs_comms.wakeup_read_fd;
        evaluator.init_input_system(input_rx, wakeup_fd);
    }

    // 9. Set up redisplay callback (layout engine + send frame)
    match startup.frontend {
        FrontendKind::Gui => {
            // GUI mode: enable cosmic-text font metrics on the layout
            // engine. The thread-local starts without fonts to keep
            // TTY startup fast; GUI enables them here on first use.
            LAYOUT_ENGINE.with(|engine| {
                engine.borrow_mut().enable_cosmic_metrics();
            });
            let frame_tx = emacs_comms.frame_tx;
            evaluator.redisplay_fn = Some(Box::new(move |eval: &mut Context| {
                eval.setup_thread_locals();
                sync_selected_gui_chrome_state(eval);
                run_layout(eval);
                sync_live_gui_frame_titles(eval);
                // Take the complete FrameDisplayState produced by the layout
                // engine's GlyphMatrixBuilder.
                let display_state = LAYOUT_ENGINE
                    .with(|engine| engine.borrow_mut().last_frame_display_state.take());
                let Some(display_state) = display_state else {
                    return;
                };
                let _ = frame_tx.try_send(display_state);
            }));
        }
        FrontendKind::Tty => {
            maybe_install_tty_redisplay_callback(&mut evaluator, &startup);
        }
    }

    // Add undo boundary after startup so initial content isn't undoable
    if let Some(buf) = evaluator.buffer_manager_mut().current_buffer_mut() {
        let mut ul = buf.get_undo_list();
        neovm_core::buffer::undo_list_boundary(&mut ul);
        buf.set_undo_list(ul);
    }

    // 10. Enter GNU's outer command loop. This mirrors src/emacs.c, which
    //     enters recursive-edit and lets the outer command loop evaluate the
    //     `top-level` startup form before reading interactive input.
    neovm_core::emacs_core::load::maybe_run_after_pdump_load_hook(&mut evaluator);
    tracing::info!("Entering GNU command loop (recursive-edit)...");
    let exit_status = evaluator.recursive_edit();
    if exit_status.is_ok() {
        tracing::info!("Command loop exited normally");
    } else {
        tracing::warn!("Command loop exited with error");
    }

    // 11. Shutdown
    tracing::info!("Shutting down...");
    let _ = emacs_comms
        .cmd_tx
        .try_send(neomacs_display_runtime::thread_comm::RenderCommand::Shutdown);
    frontend.join();
    if should_enable_live_tty_io(&startup) {
        tty_shutdown_terminal();
    }
    tracing::info!("Neomacs exited cleanly");

    if let Some(request) = evaluator.shutdown_request() {
        if request.restart {
            tracing::warn!("restart requested via kill-emacs, but restart is not implemented yet");
        }
        if request.exit_code != 0 {
            std::process::exit(request.exit_code);
        }
    }
}

// ---------------------------------------------------------------------------
// TTY terminal setup/teardown for TtyRif single-thread path
// ---------------------------------------------------------------------------

/// Saved original termios for the TtyRif path. Stored globally so
/// `tty_shutdown_terminal` can restore it even from a panic handler.
#[cfg(unix)]
static TTY_SAVED_TERMIOS: std::sync::Mutex<Option<libc::termios>> = std::sync::Mutex::new(None);

/// Set up the terminal for the TtyRif direct-rendering path:
/// raw mode, alternate screen buffer, hidden cursor.
#[cfg(unix)]
fn tty_init_terminal() {
    use std::io::Write;
    use std::mem::MaybeUninit;

    unsafe {
        let mut original = MaybeUninit::<libc::termios>::uninit();
        if libc::tcgetattr(libc::STDIN_FILENO, original.as_mut_ptr()) != 0 {
            tracing::error!("tty_init_terminal: tcgetattr failed");
            return;
        }
        let original = original.assume_init();

        // Save for later restore
        if let Ok(mut guard) = TTY_SAVED_TERMIOS.lock() {
            *guard = Some(original);
        }

        let mut raw = original;
        // Input: no break, no CR->NL, no parity, no strip, no start/stop
        raw.c_iflag &= !(libc::BRKINT | libc::ICRNL | libc::INPCK | libc::ISTRIP | libc::IXON);
        // Output: disable post-processing
        raw.c_oflag &= !libc::OPOST;
        // Control: 8-bit chars
        raw.c_cflag |= libc::CS8;
        // Local: no echo, no canonical, no signals, no extended
        raw.c_lflag &= !(libc::ECHO | libc::ICANON | libc::ISIG | libc::IEXTEN);
        // Non-blocking reads
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 0;

        if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &raw) != 0 {
            tracing::error!("tty_init_terminal: tcsetattr failed");
            return;
        }
    }

    // Enter alternate screen, hide cursor, clear
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(b"\x1b[?1049h\x1b[?25l\x1b[2J");
    let _ = stdout.flush();
    tracing::info!("TTY terminal initialized (raw mode + alt screen)");
}

#[cfg(not(unix))]
fn tty_init_terminal() {
    tracing::warn!("tty_init_terminal: not implemented on this platform");
}

/// Restore the terminal to its original state: show cursor, leave alt screen,
/// reset SGR, restore saved termios.
#[cfg(unix)]
fn tty_shutdown_terminal() {
    use std::io::Write;

    // Show cursor, reset SGR, leave alternate screen
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(b"\x1b[0m\x1b[?25h\x1b[?1049l");
    let _ = stdout.flush();

    // Restore termios
    if let Ok(guard) = TTY_SAVED_TERMIOS.lock() {
        if let Some(ref original) = *guard {
            unsafe {
                let _ = libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, original);
            }
        }
    }
    tracing::info!("TTY terminal restored");
}

#[cfg(not(unix))]
fn tty_shutdown_terminal() {
    tracing::warn!("tty_shutdown_terminal: not implemented on this platform");
}

fn run_temacs_dump_mode(dump_mode: LoadupDumpMode, startup: &StartupOptions) {
    // Logging is already initialized by `run()` before this function is
    // called; calling `init()` again here is redundant (it would be a
    // no-op anyway because the global subscriber is set once).
    tracing::info!(
        "{} {} starting raw loadup dump (dump-mode={}, pid={})",
        RuntimeMode::Raw.binary_name(),
        neomacs_display_runtime::VERSION,
        dump_mode.as_gnu_string(),
        std::process::id()
    );

    let startup_surface = raw_loadup_startup_surface(startup, Some(dump_mode));
    let eval = neovm_core::emacs_core::load::create_bootstrap_evaluator_with_startup_surface(
        BOOTSTRAP_CORE_FEATURES,
        Some(dump_mode),
        Some(&startup_surface),
    )
    .expect("temacs bootstrap dump should succeed");

    if let Some(request) = eval.shutdown_request()
        && request.exit_code != 0
    {
        std::process::exit(request.exit_code);
    }
}

#[allow(dead_code)]
fn main() {
    run(RuntimeMode::FinalRun);
}

// ---------------------------------------------------------------------------
// Bootstrap helpers
// ---------------------------------------------------------------------------

struct BootstrapResult {
    #[allow(dead_code)]
    scratch_id: BufferId,
    #[allow(dead_code)]
    minibuf_id: BufferId,
}

#[derive(Clone, Copy, Debug)]
struct BootstrapFrameMetrics {
    char_width: f32,
    char_height: f32,
    font_pixel_size: f32,
}

fn font_weight_symbol(weight: FontWeight) -> &'static str {
    match weight.0 {
        0..=150 => "thin",
        151..=250 => "extra-light",
        251..=350 => "light",
        351..=450 => "normal",
        451..=550 => "medium",
        551..=650 => "semi-bold",
        651..=750 => "bold",
        751..=850 => "extra-bold",
        _ => "black",
    }
}

fn startup_font_weight_symbol(weight: FontWeight) -> &'static str {
    match weight.0 {
        351..=450 => "regular",
        _ => font_weight_symbol(weight),
    }
}

fn font_slant_symbol(slant: FontSlant) -> &'static str {
    match slant {
        FontSlant::Normal => "normal",
        FontSlant::Italic => "italic",
        FontSlant::Oblique => "oblique",
        FontSlant::ReverseItalic => "reverse-italic",
        FontSlant::ReverseOblique => "reverse-oblique",
    }
}

fn font_width_symbol(width: FontWidth) -> &'static str {
    match width {
        FontWidth::UltraCondensed => "ultra-condensed",
        FontWidth::ExtraCondensed => "extra-condensed",
        FontWidth::Condensed => "condensed",
        FontWidth::SemiCondensed => "semi-condensed",
        FontWidth::Normal => "normal",
        FontWidth::SemiExpanded => "semi-expanded",
        FontWidth::Expanded => "expanded",
        FontWidth::ExtraExpanded => "extra-expanded",
        FontWidth::UltraExpanded => "ultra-expanded",
    }
}

fn bootstrap_default_font_parameter(font_pixel_size: f32) -> Value {
    let mut metrics_svc = FontMetricsService::new();
    let selected = metrics_svc.select_font_for_char('M', "Monospace", 400, false, font_pixel_size);
    let rounded_pixel_size = font_pixel_size.max(1.0).round() as i64;

    let family = selected
        .as_ref()
        .map(|font| font.family.as_str())
        .unwrap_or("Monospace");
    let weight = selected
        .as_ref()
        .map(|font| startup_font_weight_symbol(font.weight))
        .unwrap_or("regular");
    let slant = selected
        .as_ref()
        .map(|font| font_slant_symbol(font.slant))
        .unwrap_or("normal");
    let width = selected
        .as_ref()
        .map(|font| font_width_symbol(font.width))
        .unwrap_or("normal");

    Value::vector(vec![
        Value::keyword("font-object"),
        Value::keyword("family"),
        Value::string(family),
        Value::keyword("weight"),
        Value::symbol(weight),
        Value::keyword("slant"),
        Value::symbol(slant),
        Value::keyword("width"),
        Value::symbol(width),
        // In GNU font objects, :size is pixel size.  Keep :height in
        // face-attribute units (1/10pt) so face derivation still sees the
        // default-face height rather than a raw pixel count.
        Value::keyword("size"),
        Value::fixnum(rounded_pixel_size),
        Value::keyword("height"),
        Value::fixnum(100),
    ])
}

fn bootstrap_default_font_name(font_pixel_size: f32) -> Value {
    let mut metrics_svc = FontMetricsService::new();
    let selected = metrics_svc.select_font_for_char('M', "Monospace", 400, false, font_pixel_size);
    let rounded_pixel_size = font_pixel_size.max(1.0).round() as i64;

    let family = selected
        .as_ref()
        .map(|font| font.family.as_str())
        .unwrap_or("Monospace");
    let weight = selected
        .as_ref()
        .map(|font| startup_font_weight_symbol(font.weight))
        .unwrap_or("regular");
    let slant = selected
        .as_ref()
        .map(|font| font_slant_symbol(font.slant))
        .unwrap_or("normal");

    Value::string(format!(
        "-*-{family}-{weight}-{slant}-*-*-{rounded_pixel_size}-*-*-*-*-*-*-*"
    ))
}

fn bootstrap_frame_metrics() -> BootstrapFrameMetrics {
    // GNU X backends seed the first GUI frame from a 10pt default font and
    // convert that through the active Xft DPI.
    let font_pixel_size = face_height_to_pixels(100);
    let mut metrics_svc = FontMetricsService::new();
    let metrics = metrics_svc.font_metrics("Monospace", 400, false, font_pixel_size);
    BootstrapFrameMetrics {
        char_width: metrics.char_width.max(1.0),
        char_height: metrics.line_height.max(1.0),
        font_pixel_size,
    }
}

fn bootstrap_frame_metrics_for_frontend(frontend: FrontendKind) -> BootstrapFrameMetrics {
    if frontend == FrontendKind::Tty {
        BootstrapFrameMetrics {
            char_width: 1.0,
            char_height: 1.0,
            font_pixel_size: 16.0,
        }
    } else {
        bootstrap_frame_metrics()
    }
}

fn bootstrap_buffers(
    eval: &mut Context,
    width: u32,
    height: u32,
    display: BootstrapDisplayConfig,
) -> BootstrapResult {
    let frame_metrics = bootstrap_frame_metrics_for_frontend(display.frontend);
    let find_or_create_buffer = |eval: &mut Context, name: &str| {
        eval.buffer_manager()
            .find_buffer_by_name(name)
            .unwrap_or_else(|| eval.buffer_manager_mut().create_buffer(name))
    };

    // Reuse GNU startup buffers instead of creating duplicate names on top of
    // cached bootstrap state.
    let scratch_id = find_or_create_buffer(eval, "*scratch*");
    let _ = eval
        .buffer_manager_mut()
        .clear_buffer_labeled_restrictions(scratch_id);
    if let Some(buf) = eval.buffer_manager_mut().get_mut(scratch_id) {
        buf.widen();
        // Don't insert scratch content here. GNU Emacs populates
        // *scratch* from startup.el:2948 via
        //   (insert (substitute-command-keys initial-scratch-message))
        // which handles \\[...] key-binding expansion and backtick →
        // curly-quote conversion via text-quoting-style. Hardcoding
        // the content in Rust bypassed both of those, producing bare
        // "C-x C-f" instead of quoted "'C-x C-f'".
        buf.goto_byte(buf.point_max());
    }

    // Set *scratch* as the current buffer
    eval.buffer_manager_mut().set_current(scratch_id);

    let msg_id = find_or_create_buffer(eval, "*Messages*");
    let _ = eval
        .buffer_manager_mut()
        .clear_buffer_labeled_restrictions(msg_id);
    if let Some(buf) = eval.buffer_manager_mut().get_mut(msg_id) {
        buf.widen();
        buf.goto_byte(0);
    }

    let mini_id = find_or_create_buffer(eval, " *Minibuf-0*");
    let _ = eval
        .buffer_manager_mut()
        .clear_buffer_labeled_restrictions(mini_id);
    if let Some(buf) = eval.buffer_manager_mut().get_mut(mini_id) {
        buf.widen();
        buf.goto_byte(0);
    }

    let frame_id = {
        let frame_manager = eval.frame_manager();
        let selected = frame_manager.selected_frame().map(|frame| frame.id);
        let should_reuse_existing = selected.is_some() && frame_manager.frame_list().len() == 1;
        (selected, should_reuse_existing)
    };
    let frame_id = if frame_id.1 {
        let frame_id = frame_id.0.expect("selected startup frame");
        tracing::info!(
            "Reusing existing startup frame {:?} as bootstrap frame ({}x{})",
            frame_id,
            width,
            height
        );
        frame_id
    } else {
        let frame_id = eval
            .frame_manager_mut()
            .create_frame("F1", width, height, scratch_id);
        tracing::info!(
            "Created frame {:?} ({}x{}) with *scratch*={:?}",
            frame_id,
            width,
            height,
            scratch_id
        );
        frame_id
    };
    let _ = eval.frame_manager_mut().select_frame(frame_id);

    // Seed frame parameters so GNU Lisp startup sees the correct host surface.
    if let Some(frame) = eval.frame_manager_mut().get_mut(frame_id) {
        // Font parameter resolution creates a FontMetricsService which
        // scans the system font database (~500ms). Skip for TTY where
        // font parameters are unused — TTY uses 1x1 character cells.
        let (default_font, default_font_name) = if display.frontend == FrontendKind::Tty {
            (Value::NIL, Value::string("fixed"))
        } else {
            (
                bootstrap_default_font_parameter(frame_metrics.font_pixel_size),
                bootstrap_default_font_name(frame_metrics.font_pixel_size),
            )
        };
        frame.width = width;
        frame.height = height;
        frame.visible = true;
        if let Some(window_system) = display.window_system_symbol() {
            frame.set_window_system(Some(Value::symbol(window_system)));
        } else {
            frame.set_window_system(None);
        }
        frame.set_parameter("display-type", Value::symbol(display.display_type_symbol()));
        frame.set_parameter("background-mode", Value::symbol(display.background_mode));
        frame.set_parameter("font", default_font_name);
        frame.set_parameter("font-parameter", default_font);
        // GNU frame.c: initial frame title is NULL (unset). The %F
        // mode-line construct falls through to frame->name ("F1") when
        // title is unset. Don't set a title here — let %F show the
        // frame name, matching GNU behaviour.

        frame.font_pixel_size = frame_metrics.font_pixel_size;
        if display.frontend == FrontendKind::Tty {
            // TTY frames use 1x1 character cell metrics
            // (GNU Emacs frame.c:1184-1185: column_width=1, line_height=1).
            frame.char_width = 1.0;
            frame.char_height = 1.0;
            // The minibuffer was created with a pixel height (16.0) in Frame::new.
            // For TTY, resize it to 1 row (char_height=1.0) before sync.
            if let Some(mini) = frame.minibuffer_leaf.as_mut() {
                let b = *mini.bounds();
                mini.set_bounds(neovm_core::window::Rect::new(b.x, b.y, b.width, 1.0));
            }
        } else {
            frame.char_width = frame_metrics.char_width;
            frame.char_height = frame_metrics.char_height;
        }
        frame.sync_tab_bar_height_from_parameters();
        // Match GNU `frame.c:1307-1309` (TTY frame init):
        //   FRAME_MENU_BAR_LINES (f) = NILP (Vmenu_bar_mode) ? 0 : 1;
        // On TTY frames neomacs has no per-frame default-frame-alist
        // bridge yet, so seed the parameter directly here when the
        // frontend is TTY before calling `sync_menu_bar_height_from_parameters`.
        // The GUI path has its own menu bar pipeline (see
        // `neomacs-display-runtime`) and never goes through this code,
        // so we only need to set the parameter for `FrontendKind::Tty`.
        if display.frontend == FrontendKind::Tty {
            frame.set_parameter("menu-bar-lines", neovm_core::emacs_core::Value::fixnum(1));
        }
        frame.sync_menu_bar_height_from_parameters();
        frame.sync_tool_bar_height_from_parameters();
        if let Window::Leaf {
            buffer_id,
            window_start,
            point,
            ..
        } = &mut frame.root_window
        {
            *buffer_id = scratch_id;
            *window_start = 0;
            *point = 0;
        }
    }
    if display.frontend == FrontendKind::Gui {
        seed_gnu_default_gui_chrome_modes(eval);
        sync_selected_gui_chrome_state(eval);
    }

    if display.window_system_symbol().is_some() {
        neovm_core::emacs_core::font::seed_live_frame_default_face_from_font_parameter(
            eval, frame_id,
        );
    }

    // Fix window geometry: root window takes frame height minus minibuffer.
    if let Some(frame) = eval.frame_manager_mut().get_mut(frame_id) {
        let mini_h = frame.char_height.max(1.0);
        let mini_y = height as f32 - mini_h;
        if let Window::Leaf { bounds, .. } = &mut frame.root_window {
            bounds.height = mini_y;
        }
        if let Some(mini_leaf) = &mut frame.minibuffer_leaf {
            if let Window::Leaf {
                buffer_id,
                window_start,
                point,
                bounds,
                ..
            } = mini_leaf
            {
                *buffer_id = mini_id;
                *window_start = 0;
                *point = 0;
                bounds.y = mini_y;
                bounds.height = mini_h;
                bounds.width = width as f32;
            }
        }
    }

    BootstrapResult {
        scratch_id,
        minibuf_id: mini_id,
    }
}

fn configure_gnu_startup_state(eval: &mut Context, frame_id: FrameId, startup: &StartupOptions) {
    let argv_strings = startup.forwarded_args.iter().cloned().collect::<Vec<_>>();
    let argv = argv_strings
        .iter()
        .cloned()
        .map(Value::string)
        .collect::<Vec<_>>();
    let argv_left = argv_strings
        .iter()
        .skip(1)
        .cloned()
        .map(Value::string)
        .collect::<Vec<_>>();
    let invocation_directory = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("/"));
    let invocation_name = std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| "neomacs".to_string());
    let invocation_directory = ensure_dir_string(&invocation_directory);

    eval.set_variable("command-line-args", Value::list(argv));
    eval.set_variable("command-line-args-left", Value::list(argv_left));
    eval.set_variable("command-line-processed", Value::NIL);
    eval.set_variable(
        "noninteractive",
        if startup.noninteractive {
            Value::T
        } else {
            Value::NIL
        },
    );
    // Mirror GNU's C-side `no_site_lisp` / `build_details` globals as
    // Lisp variables. GNU itself does not expose them as Lisp vars (the
    // load-path / version code reads the C globals directly), but
    // surfacing them here lets oracle tests verify the parsed value
    // and lets future load-path or version code observe the choice
    // without re-walking argv. Defaults match GNU: no_site_lisp=false
    // means site-lisp is included; build-details=t means build-time
    // strings are populated.
    eval.set_variable(
        "no-site-lisp",
        if startup.no_site_lisp {
            Value::T
        } else {
            Value::NIL
        },
    );
    eval.set_variable(
        "build-details",
        if startup.no_build_details {
            Value::NIL
        } else {
            Value::T
        },
    );
    let (terminal_frame, frame_initial_frame, default_minibuffer_frame) = match startup.frontend {
        FrontendKind::Gui => {
            let terminal_frame_id = ensure_gnu_startup_terminal_frame(eval, frame_id);
            let window_system = Value::symbol(gui_window_system_symbol());
            eval.set_variable("window-system", window_system);
            eval.set_variable("initial-window-system", window_system);
            eval.set_variable(
                "frame-initial-frame-alist",
                opening_frame_initial_alist(eval, window_system),
            );
            (
                Value::make_frame(terminal_frame_id.0),
                Value::make_frame(frame_id.0),
                Value::make_frame(frame_id.0),
            )
        }
        FrontendKind::Tty => {
            eval.set_variable("window-system", Value::NIL);
            eval.set_variable("initial-window-system", Value::NIL);
            (Value::make_frame(frame_id.0), Value::NIL, Value::NIL)
        }
    };
    eval.set_variable("invocation-name", Value::string(invocation_name));
    eval.set_variable("invocation-directory", Value::string(invocation_directory));
    let cwd = std::env::current_dir()
        .map(|p| ensure_dir_string(&p))
        .unwrap_or_else(|_| "/".to_string());
    eval.set_variable("default-directory", Value::string(&cwd));
    eval.set_variable("terminal-frame", terminal_frame);
    eval.set_variable("frame-initial-frame", frame_initial_frame);
    eval.set_variable("default-minibuffer-frame", default_minibuffer_frame);
    // Skip the splash screen — its fill-region is extremely slow through
    // with_mirrored_evaluator.  Users who want it can set this to nil in
    // their init file.
    eval.set_variable("inhibit-startup-screen", Value::T);
}

fn ensure_gnu_startup_terminal_frame(eval: &mut Context, opening_frame_id: FrameId) -> FrameId {
    if let Some(existing) = eval
        .frame_manager()
        .frame_list()
        .into_iter()
        .find(|candidate| {
            *candidate != opening_frame_id
                && eval.frame_manager().get(*candidate).is_some_and(|frame| {
                    !frame.visible && frame.effective_window_system().is_none()
                })
        })
    {
        return existing;
    }

    let seed_buffer_id = if let Some(id) = eval.buffer_manager().current_buffer_id() {
        id
    } else if let Some(id) = eval.buffer_manager().find_buffer_by_name("*scratch*") {
        id
    } else {
        eval.buffer_manager_mut().create_buffer("*scratch*")
    };
    let (width, height, environment) = eval
        .frame_manager()
        .get(opening_frame_id)
        .map(|frame| {
            (
                frame.width.max(1),
                frame.height.max(1),
                frame.parameter("environment"),
            )
        })
        .unwrap_or((80, 25, None));
    let terminal_frame_id =
        eval.frame_manager_mut()
            .create_frame("Fstartup-tty", width, height, seed_buffer_id);
    if let Some(frame) = eval.frame_manager_mut().get_mut(terminal_frame_id) {
        frame.visible = false;
        frame.set_window_system(None);
        frame.remove_parameter("display-type");
        frame.remove_parameter("background-mode");
        if let Some(environment) = environment {
            frame.set_parameter("environment", environment);
        }
    }
    terminal_frame_id
}

fn opening_frame_initial_alist(eval: &Context, window_system: Value) -> Value {
    let mut params = vec![Value::cons(Value::symbol("window-system"), window_system)];
    for symbol_name in ["initial-frame-alist", "default-frame-alist"] {
        if let Some(value) = eval.obarray().symbol_value(symbol_name)
            && let Some(items) = neovm_core::emacs_core::value::list_to_vec(value)
        {
            params.extend(items);
        }
    }
    Value::list(params)
}

#[cfg(test)]
fn run_gnu_startup(eval: &mut Context) {
    eval.setup_thread_locals();
    let _ = std::fs::write("/tmp/neomacs-startup-phases.trace", "");
    maybe_install_startup_phase_trace(eval);
    eval.eval_str(
        r#"
        (progn
          (defun neomacs--test-exit-startup-recursive-edit ()
            (remove-hook 'window-setup-hook
                         #'neomacs--test-exit-startup-recursive-edit)
            (exit-recursive-edit))
          (add-hook 'window-setup-hook
                    #'neomacs--test-exit-startup-recursive-edit))
        "#,
    )
    .expect("startup exit helper should install");
    let top_level = eval.obarray().symbol_value("top-level").cloned();
    tracing::info!("top-level variable before startup: {:?}", top_level);

    let (_tx, rx) = crossbeam_channel::unbounded();

    let mut wake_pipe = [0; 2];
    let pipe_result = unsafe { libc::pipe(wake_pipe.as_mut_ptr()) };
    assert_eq!(pipe_result, 0, "pipe should initialize");
    eval.init_input_system(rx, wake_pipe[0]);

    let result = eval.recursive_edit();
    unsafe {
        libc::close(wake_pipe[0]);
        libc::close(wake_pipe[1]);
    }

    if let Err(other) = result {
        let last_phase = eval
            .obarray()
            .symbol_value("neomacs--startup-last-phase")
            .cloned()
            .map(|value| print_value_with_eval(eval, &value));
        let last_call = eval
            .obarray()
            .symbol_value("neomacs--startup-last-call")
            .cloned()
            .map(|value| print_value_with_eval(eval, &value));
        panic!(
            "GNU startup via recursive_edit failed: {other} last-phase={last_phase:?} last-call={last_call:?}"
        );
    }
}

fn maybe_install_startup_phase_trace(eval: &mut Context) {
    if !cfg!(test) && std::env::var("NEOMACS_TRACE_STARTUP_PHASES").unwrap_or_default() != "1" {
        return;
    }
    let source = r#"
        (progn
          (defvar neomacs--startup-last-phase nil)
          (defvar neomacs--startup-last-call nil)
          (with-temp-buffer
            (write-region (point-min) (point-max)
                          "/tmp/neomacs-startup-phases.trace" nil 'silent))
          (defun neomacs--startup-trace-around (name orig &rest args)
            (setq neomacs--startup-last-phase name)
            (setq neomacs--startup-last-call (cons name args))
            (with-temp-buffer
              (insert (format "enter %S %S\n" name args))
              (append-to-file (point-min) (point-max)
                              "/tmp/neomacs-startup-phases.trace"))
            (prog1
                (apply orig args)
              (with-temp-buffer
                (insert (format "leave %S\n" name))
                (append-to-file (point-min) (point-max)
                                "/tmp/neomacs-startup-phases.trace"))))
          (dolist (fn '(set-locale-environment
                        handle-args-function
                        window-system-initialization
                        command-line
                        frame-initialize
                        display-graphic-p
                        face-spec-recalc
                        face-spec-choose
                        face-background
                        face-attribute
                        internal-get-lisp-face-attribute
                        internal-merge-in-global-face
                        tab-bar-height
                        tool-bar-height
                        tab-bar-mode
                        tool-bar-mode
                        minibuffer-frame-list
                        delete-frame
                        frame-parameters
                        frame-parameter
                        modify-frame-parameters
                        make-frame
                        frame-set-background-mode
                        coding-system-type
                        coding-system-get
                        coding-system-change-eol-conversion
                        set-keyboard-coding-system
                        set-keyboard-coding-system-internal
                        set-terminal-coding-system
                        set-terminal-coding-system-internal
                        startup--setup-quote-display
                        frame-notice-user-settings
                        frame-focus-state
                        blink-cursor--should-blink
                        blink-cursor-check
                        blink-cursor-suspend
                        sit-for
                        read-event
                        frame-list
                        frame-selected-window
                        frame-visible-p
                        window-minibuffer-p
                        tty-run-terminal-initialization
                        face-set-after-frame-default))
            (when (fboundp fn)
              (advice-add fn :around
                          (apply-partially #'neomacs--startup-trace-around fn)))))
    "#;
    eval.eval_str(source)
        .expect("startup trace helper should install");
}

fn ensure_dir_string(path: &Path) -> String {
    let mut dir = path.to_string_lossy().to_string();
    if !dir.ends_with('/') {
        dir.push('/');
    }
    dir
}

fn current_layout_frame_id(evaluator: &Context) -> Option<FrameId> {
    evaluator
        .frame_manager()
        .selected_frame()
        .map(|frame| frame.id)
}

thread_local! {
    // Start without font metrics to avoid the ~500ms cosmic-text
    // font database scan on first access. The GUI path enables
    // cosmic metrics explicitly; the TTY path leaves it as None.
    static LAYOUT_ENGINE: std::cell::RefCell<neomacs_display_runtime::layout::LayoutEngine> =
        std::cell::RefCell::new(neomacs_display_runtime::layout::LayoutEngine::new_without_font_metrics());
}

/// Run the layout engine on the selected live frame.
fn run_layout(evaluator: &mut Context) {
    let Some(frame_id) = current_layout_frame_id(evaluator) else {
        tracing::warn!("run_layout: no selected live frame");
        return;
    };

    LAYOUT_ENGINE.with(|engine| {
        engine.borrow_mut().layout_frame_rust(evaluator, frame_id);
    });
}

/// After `run_layout` has populated the layout engine's `last_frame_display_state`,
/// rasterize the display state into a `TtyRif` and write the ANSI output to stdout.
fn run_tty_rif_redisplay(tty_rif: &mut neomacs_display_protocol::tty_rif::TtyRif) {
    LAYOUT_ENGINE.with(|engine| {
        let engine = engine.borrow();
        if let Some(ref state) = engine.last_frame_display_state {
            tty_rif.rasterize(state);
            tty_rif.diff_and_render();
            let output = tty_rif.take_output();
            tracing::debug!("tty_rif: output {} bytes", output.len());
            if !output.is_empty() {
                use std::io::Write;
                let _ = std::io::stdout().write_all(&output);
                let _ = std::io::stdout().flush();
            }
        } else {
            tracing::debug!("tty_rif: no last_frame_display_state");
        }
    });
}

/// Increase the process stack size limit, matching GNU Emacs's behavior
/// in emacs.c main() which adjusts RLIMIT_STACK.
#[cfg(unix)]
fn increase_stack_limit() {
    const TARGET_STACK_MB: u64 = 128;
    let target = TARGET_STACK_MB * 1024 * 1024;
    unsafe {
        let mut rlim = std::mem::MaybeUninit::<libc::rlimit>::uninit();
        if libc::getrlimit(libc::RLIMIT_STACK, rlim.as_mut_ptr()) == 0 {
            let mut rlim = rlim.assume_init();
            if rlim.rlim_cur < target as libc::rlim_t {
                rlim.rlim_cur = std::cmp::min(target as libc::rlim_t, rlim.rlim_max);
                let _ = libc::setrlimit(libc::RLIMIT_STACK, &rlim);
            }
        }
    }
}

#[cfg(not(unix))]
fn increase_stack_limit() {}

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;
