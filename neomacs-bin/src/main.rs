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

mod input_bridge;
mod tty_frontend;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use neomacs_display_runtime::FrameGlyphBuffer;
use neomacs_display_runtime::render_thread::{
    RenderThread, SharedImageDimensions, SharedMonitorInfo,
};
use neomacs_display_runtime::thread_comm::{
    InputEvent as DisplayInputEvent, RenderCommand, ThreadComms,
};
use neomacs_layout_engine::font_metrics::FontMetricsService;
use neomacs_layout_engine::fontconfig::face_height_to_pixels;

use neovm_core::buffer::BufferId;
use neovm_core::emacs_core::Value;
use neovm_core::emacs_core::builtins::set_neomacs_monitor_info;
use neovm_core::emacs_core::display::gui_window_system_symbol;
use neovm_core::emacs_core::eval::{
    FontResolveRequest, FontSpecResolveRequest, GuiFrameHostSize, ResolvedFontMatch,
    ResolvedFontSpecMatch,
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

fn parse_startup_options(args: impl IntoIterator<Item = String>) -> Result<StartupOptions, String> {
    let mut iter = args.into_iter();
    let program = iter.next().unwrap_or_else(|| "neomacs".to_string());
    let args = iter.collect::<Vec<_>>();
    let mut forwarded_args = vec![program];
    let mut frontend = FrontendKind::Gui;
    let mut terminal_device = None;
    let mut noninteractive = false;
    let mut temacs_mode = None;
    let mut dump_file_override = None;
    let mut index = 0usize;

    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            forwarded_args.extend(args[index..].iter().cloned());
            break;
        }

        if matches!(arg.as_str(), "-nw" | "--no-window-system" | "--no-windows") {
            frontend = FrontendKind::Tty;
            index += 1;
            continue;
        }

        if matches!(arg.as_str(), "--batch" | "-batch") {
            noninteractive = true;
            frontend = FrontendKind::Tty;
            index += 1;
            continue;
        }

        if arg == "-temacs" || arg == "--temacs" {
            let Some(value) = args.get(index + 1) else {
                return Err(format!("neomacs: option `{arg}` requires an argument"));
            };
            temacs_mode = Some(parse_temacs_mode(value)?);
            forwarded_args.push(arg.clone());
            forwarded_args.push(value.clone());
            index += 2;
            continue;
        }

        if let Some(value) = arg.strip_prefix("--temacs=") {
            temacs_mode = Some(parse_temacs_mode(value)?);
            forwarded_args.push(arg.clone());
            index += 1;
            continue;
        }

        if arg == "-dump-file" || arg == "--dump-file" {
            let Some(value) = args.get(index + 1) else {
                return Err(format!("neomacs: option `{arg}` requires an argument"));
            };
            dump_file_override = Some(PathBuf::from(value));
            forwarded_args.push(arg.clone());
            forwarded_args.push(value.clone());
            index += 2;
            continue;
        }

        if let Some(value) = arg.strip_prefix("--dump-file=") {
            dump_file_override = Some(PathBuf::from(value));
            forwarded_args.push(arg.clone());
            index += 1;
            continue;
        }

        if arg == "-t" || arg == "--terminal" {
            let Some(device) = args.get(index + 1) else {
                return Err(format!("neomacs: option `{arg}` requires an argument"));
            };
            frontend = FrontendKind::Tty;
            terminal_device = Some(device.clone());
            index += 2;
            continue;
        }

        if let Some(device) = arg.strip_prefix("--terminal=") {
            frontend = FrontendKind::Tty;
            terminal_device = Some(device.to_string());
            index += 1;
            continue;
        }

        if arg == "-d" || arg == "--display" {
            let Some(value) = args.get(index + 1) else {
                return Err(format!("neomacs: option `{arg}` requires an argument"));
            };
            forwarded_args.push(arg.clone());
            forwarded_args.push(value.clone());
            index += 2;
            continue;
        }

        if arg.starts_with("--display=") {
            forwarded_args.push(arg.clone());
            index += 1;
            continue;
        }

        forwarded_args.push(arg.clone());
        index += 1;
    }

    Ok(StartupOptions {
        frontend,
        forwarded_args,
        terminal_device,
        noninteractive,
        temacs_mode,
        dump_file_override,
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
            // Compute pixel size from font metrics (100 cols x 30 lines).
            let cols = 100u32;
            let lines = 30u32;
            let width = (cols as f32 * frame_metrics.char_width).round() as u32;
            let height = (lines as f32 * frame_metrics.char_height).round() as u32;
            (width.max(200), height.max(100))
        }
        FrontendKind::Tty => {
            let (cols, rows) = query_terminal_size_cells().unwrap_or((80, 25));
            let width = (cols as f32 * frame_metrics.char_width)
                .round()
                .max(frame_metrics.char_width) as u32;
            let height = (rows as f32 * frame_metrics.char_height)
                .round()
                .max(frame_metrics.char_height * 2.0) as u32;
            (width, height)
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
}

impl FrontendHandle {
    fn join(self) {
        match self {
            Self::Gui(handle) => handle.join(),
            Self::TtyRifInput(handle) => handle.join(),
        }
    }
}

struct PrimaryWindowDisplayHost {
    cmd_tx: crossbeam_channel::Sender<RenderCommand>,
    primary_window_adopted: bool,
    primary_frame_id: Option<neovm_core::window::FrameId>,
    font_metrics: Option<FontMetricsService>,
    primary_window_size: SharedPrimaryWindowSize,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PrimaryWindowSize {
    width: u32,
    height: u32,
}

type SharedPrimaryWindowSize = Arc<Mutex<PrimaryWindowSize>>;

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
                    title: request.title,
                })
                .map_err(|err| format!("failed to update primary window title: {err}"))?;
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
                    title: request.title,
                })
                .map_err(|err| format!("failed to create additional GUI window: {err}"))?;
        }
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
            })
            .map_err(|err| format!("failed to resize GUI frame: {err}"))?;
        Ok(())
    }

    fn current_primary_window_size(&self) -> Option<GuiFrameHostSize> {
        if self.primary_window_adopted {
            return None;
        }
        let state = match self.primary_window_size.lock() {
            Ok(state) => *state,
            Err(poisoned) => *poisoned.into_inner(),
        };
        Some(GuiFrameHostSize {
            width: state.width,
            height: state.height,
        })
    }

    fn resolve_font_for_char(
        &mut self,
        request: FontResolveRequest,
    ) -> Result<Option<ResolvedFontMatch>, String> {
        let requested_family = request.face.family.as_deref().unwrap_or("Monospace");
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
            .unwrap_or_else(|_| panic!("bootstrap image should load; see log for details"))
        }
        RuntimeMode::FinalRun => neovm_core::emacs_core::load::load_runtime_image_with_features(
            RuntimeImageRole::Final,
            BOOTSTRAP_CORE_FEATURES,
            startup.dump_file_override.as_deref(),
        )
        .unwrap_or_else(|_| panic!("final image should load; see log for details")),
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

    let has_internal_loadup_marker =
        matches!(args.get(1).map(String::as_str), Some("-l" | "--load"))
            && args.get(2).map(String::as_str) == Some("loadup");
    if !has_internal_loadup_marker {
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

    if let Some(action) = classify_early_cli_action(std::env::args()) {
        match action {
            EarlyCliAction::PrintHelp { program } => {
                print!("{}", render_help_text(&program));
            }
            EarlyCliAction::PrintVersion => {
                print!("{}", render_version_text());
            }
        }
        return;
    }

    let startup = parse_startup_options(std::env::args()).unwrap_or_else(|message| {
        eprintln!("{message}");
        std::process::exit(1);
    });

    if mode == RuntimeMode::Raw
        && let Some(temacs_mode) = startup.temacs_mode
    {
        run_temacs_dump_mode(temacs_mode, &startup);
        return;
    }

    // 1. Initialize logging
    neomacs_display_runtime::init_logging();

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
    let (width, height) = startup_dimensions(startup.frontend, bootstrap_frame_metrics());
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
            reset_terminal_host();
            configure_terminal_runtime(detect_tty_runtime());
        }
    }
    // Disable GC during startup — the bytecode VM's specpdl is not yet
    // scanned by the GC, so collections during VM execution can free
    // values that are still referenced by unwind-protect cleanup forms.
    evaluator.set_gc_threshold(usize::MAX);
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
    if startup.frontend == FrontendKind::Tty {
        set_terminal_host(Box::new(TtyTerminalHost {
            cmd_tx: emacs_comms.cmd_tx.clone(),
        }));
    }
    if startup.frontend == FrontendKind::Gui {
        evaluator.set_display_host(Box::new(PrimaryWindowDisplayHost {
            cmd_tx: emacs_comms.cmd_tx.clone(),
            primary_window_adopted: false,
            primary_frame_id: None,
            font_metrics: None,
            primary_window_size: Arc::clone(&primary_window_size),
        }));
    }

    // 5. Spawn the frontend loop matching the requested startup mode.
    let frontend = match startup.frontend {
        FrontendKind::Gui => {
            let image_dimensions: SharedImageDimensions = Arc::new(Mutex::new(HashMap::new()));
            let shared_monitors: SharedMonitorInfo =
                Arc::new((Mutex::new(Vec::new()), std::sync::Condvar::new()));
            let render_thread = RenderThread::spawn(
                render_comms,
                width,
                height,
                "Neomacs".to_string(),
                Arc::clone(&image_dimensions),
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
            // Single-thread TTY path: terminal init here, rendering via TtyRif
            // on the evaluator thread, input reader on a background thread.
            tty_init_terminal();
            let input_reader = tty_frontend::TtyInputReader::spawn(render_comms);
            tracing::info!("TTY frontend spawned (TtyRif single-thread redisplay)");
            FrontendHandle::TtyRifInput(input_reader)
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
    let mut frame_glyphs = FrameGlyphBuffer::with_size(width as f32, height as f32);

    // 7. Create input bridge: convert display runtime events → keyboard events
    let (input_tx, input_rx) = crossbeam_channel::unbounded();
    let display_input_rx = emacs_comms.input_rx;
    let primary_window_size_for_input = Arc::clone(&primary_window_size);
    std::thread::Builder::new()
        .name("input-bridge".to_string())
        .spawn(move || {
            while let Ok(event) = display_input_rx.recv() {
                record_primary_window_resize(&primary_window_size_for_input, &event);
                if let Some(kb_event) = input_bridge::convert_display_event(event) {
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

    // 9. Set up redisplay callback (layout engine + send frame)
    match startup.frontend {
        FrontendKind::Gui => {
            let frame_tx = emacs_comms.frame_tx;
            evaluator.redisplay_fn = Some(Box::new(move |eval: &mut Context| {
                eval.setup_thread_locals();
                run_layout(eval, &mut frame_glyphs);
                let _ = frame_tx.try_send(frame_glyphs.clone());
            }));
        }
        FrontendKind::Tty => {
            // Single-thread TTY redisplay: run layout, then rasterize via TtyRif
            // directly on the evaluator thread (no channel, no render thread).
            let (cols, rows) = query_terminal_size_cells().unwrap_or((80, 25));
            let mut tty_rif =
                neomacs_display_protocol::tty_rif::TtyRif::new(cols as usize, rows as usize);
            evaluator.redisplay_fn = Some(Box::new(move |eval: &mut Context| {
                eval.setup_thread_locals();
                run_layout(eval, &mut frame_glyphs);
                // Extract FrameDisplayState from the layout engine's thread-local
                run_tty_rif_redisplay(&mut tty_rif);
            }));
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
    // Diagnostic: print startup state
    eprintln!("PRE-RECURSIVE-EDIT: about to enter command loop");
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
    if startup.frontend == FrontendKind::Tty {
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
    neomacs_display_runtime::init_logging();
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

fn bootstrap_buffers(
    eval: &mut Context,
    width: u32,
    height: u32,
    display: BootstrapDisplayConfig,
) -> BootstrapResult {
    let frame_metrics = bootstrap_frame_metrics();
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
        let content = ";; This buffer is for text that is not saved, and for Lisp evaluation.\n\
                       ;; To create a file, visit it with C-x C-f and enter text in its buffer.\n\n";
        if buf.text.len() == 0 {
            buf.goto_byte(0);
            buf.insert(content);
            buf.set_modified(false);
        }
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
        let default_font = bootstrap_default_font_parameter(frame_metrics.font_pixel_size);
        let default_font_name = bootstrap_default_font_name(frame_metrics.font_pixel_size);
        frame.width = width;
        frame.height = height;
        frame.visible = true;
        if let Some(window_system) = display.window_system_symbol() {
            frame.set_window_system(Some(Value::symbol(window_system)));
        } else {
            frame.set_window_system(None);
        }
        frame.parameters.insert(
            "display-type".to_string(),
            Value::symbol(display.display_type_symbol()),
        );
        frame.parameters.insert(
            "background-mode".to_string(),
            Value::symbol(display.background_mode),
        );
        frame
            .parameters
            .insert("font".to_string(), default_font_name);
        frame
            .parameters
            .insert("font-parameter".to_string(), default_font);
        frame.title = "Neomacs".to_string();
        frame.font_pixel_size = frame_metrics.font_pixel_size;
        frame.char_width = frame_metrics.char_width;
        frame.char_height = frame_metrics.char_height;
        frame.sync_tab_bar_height_from_parameters();
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
                frame.parameters.get("environment").copied(),
            )
        })
        .unwrap_or((80, 25, None));
    let terminal_frame_id =
        eval.frame_manager_mut()
            .create_frame("Fstartup-tty", width, height, seed_buffer_id);
    if let Some(frame) = eval.frame_manager_mut().get_mut(terminal_frame_id) {
        frame.visible = false;
        frame.set_window_system(None);
        frame.parameters.remove("display-type");
        frame.parameters.remove("background-mode");
        if let Some(environment) = environment {
            frame
                .parameters
                .insert("environment".to_string(), environment);
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
    let exit_helper = neovm_core::emacs_core::parse_forms(
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
    .expect("startup exit helper should parse");
    eval.eval_expr(&exit_helper[0])
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
    let forms =
        neovm_core::emacs_core::parse_forms(source).expect("startup trace helper should parse");
    for form in &forms {
        eval.eval_expr(form)
            .expect("startup trace helper should install");
    }
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
    static LAYOUT_ENGINE: std::cell::RefCell<neomacs_display_runtime::layout::LayoutEngine> =
        std::cell::RefCell::new(neomacs_display_runtime::layout::LayoutEngine::new());
}

/// Run the layout engine on the selected live frame.
fn run_layout(evaluator: &mut Context, frame_glyphs: &mut FrameGlyphBuffer) {
    let Some(frame_id) = current_layout_frame_id(evaluator) else {
        tracing::warn!("run_layout: no selected live frame");
        return;
    };

    LAYOUT_ENGINE.with(|engine| {
        engine
            .borrow_mut()
            .layout_frame_rust(evaluator, frame_id, frame_glyphs);
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
            if !output.is_empty() {
                use std::io::Write;
                let _ = std::io::stdout().write_all(&output);
                let _ = std::io::stdout().flush();
            }
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
