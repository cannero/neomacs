//! Process-wide tracing/logging initialization.
//!
//! Two entry points:
//!
//! - [`init`] — for binary entry points. Takes a [`LogTarget`] describing
//!   the runtime environment (GUI binary, TUI binary, or test runner) and
//!   installs a fmt subscriber whose default writer matches the policy for
//!   that target. See [`LogTarget`] for the per-target default and the
//!   `NEOMACS_LOG_FILE` override. Returns a [`LoggingGuard`] that must be
//!   kept alive until process exit so the file appender can flush its
//!   queue.
//!
//! - [`init_for_tests`] — thin wrapper around [`init`] with
//!   [`LogTarget::Test`] for unit/integration tests. Uses
//!   `with_test_writer` so output is captured per-test by the test harness
//!   and only appears on failure.
//!
//! Both honor `RUST_LOG` and bridge the `log` facade into `tracing`, so
//! events from crates using the `log` facade (e.g. `cosmic-text`, `wgpu`)
//! flow into the tracing subscriber.

use std::sync::OnceLock;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Which runtime environment the binary is running in.
///
/// This determines the default writer when `NEOMACS_LOG_FILE` is not set.
/// When `NEOMACS_LOG_FILE` is set, a file layer is added (or replaces the
/// default writer for [`LogTarget::Tty`] which would otherwise be silent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogTarget {
    /// GUI binary (winit/wgpu window). Default writer: stdout.
    ///
    /// With `NEOMACS_LOG_FILE=<path>`: stdout **and** file.
    Gui,
    /// TTY/TUI binary (`-nw`, `--batch`, `-t`). Default writer: none.
    ///
    /// Logging to stdout or stderr under TTY would smash the alt-screen
    /// the redisplay engine is drawing into, so the default is silent.
    ///
    /// With `NEOMACS_LOG_FILE=<path>`: file only.
    Tty,
    /// Unit or integration test. Default writer: captured test writer
    /// (`tracing_subscriber::fmt::TestWriter`, visible only on test
    /// failure).
    ///
    /// With `NEOMACS_LOG_FILE=<path>`: test writer **and** file.
    Test,
}

/// Held by the binary's `main` function for the duration of the process so
/// the non-blocking file appender's background worker can flush on
/// shutdown.
///
/// Drop this only when the process is about to exit; dropping it earlier
/// will cause subsequent log lines to be lost from the file output.
#[must_use = "drop the LoggingGuard only at process exit; dropping early loses file log lines"]
pub struct LoggingGuard {
    _file: Option<tracing_appender::non_blocking::WorkerGuard>,
}

type BoxedLayer = Box<dyn Layer<Registry> + Send + Sync + 'static>;

/// Initialize tracing for a binary entry point.
///
/// The `target` argument selects the default writer:
///
/// | target | default writer | with `NEOMACS_LOG_FILE=<path>` |
/// |---|---|---|
/// | [`LogTarget::Gui`] | stdout | stdout + file |
/// | [`LogTarget::Tty`] | (silent) | file only |
/// | [`LogTarget::Test`] | captured test writer | test writer + file |
///
/// Behavior shared across all targets:
///
/// - Filter comes from `RUST_LOG`; defaults to `info`.
/// - Bridges crates using the `log` facade into tracing via
///   `tracing_log::LogTracer`.
/// - Idempotent — safe to call multiple times. Only the first call sets
///   up the global subscriber; subsequent calls return an empty guard.
/// - If `NEOMACS_LOG_FILE=<path>` fails to open, a warning is printed to
///   stderr and the function continues with the default writer only.
///
/// Legacy: `NEOMACS_LOG_TO_FILE=1` is still accepted and is equivalent to
/// setting `NEOMACS_LOG_FILE=neomacs-{pid}.log` in the current directory.
/// New call sites should prefer `NEOMACS_LOG_FILE`.
pub fn init(target: LogTarget) -> LoggingGuard {
    static INIT: OnceLock<()> = OnceLock::new();
    let mut guard: Option<tracing_appender::non_blocking::WorkerGuard> = None;
    INIT.get_or_init(|| {
        guard = init_inner(target);
    });
    LoggingGuard { _file: guard }
}

/// Initialize tracing for unit/integration tests.
///
/// Thin wrapper around [`init`] with [`LogTarget::Test`]. Idempotent — safe
/// to call from every `#[test]` function. The returned guard is discarded
/// because tests do not have a well-defined process shutdown boundary.
pub fn init_for_tests() {
    let _ = init(LogTarget::Test);
}

fn resolve_log_file_path() -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("NEOMACS_LOG_FILE") {
        let path = std::path::PathBuf::from(path);
        if path.as_os_str().is_empty() {
            return None;
        }
        return Some(path);
    }
    let legacy = std::env::var("NEOMACS_LOG_TO_FILE")
        .map(|v| v == "1")
        .unwrap_or(false);
    if legacy {
        let pid = std::process::id();
        return Some(std::path::PathBuf::from(format!("neomacs-{pid}.log")));
    }
    None
}

fn make_env_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
}

fn default_layer(target: LogTarget) -> Option<BoxedLayer> {
    match target {
        LogTarget::Gui => Some(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stdout)
                .with_filter(make_env_filter())
                .boxed(),
        ),
        LogTarget::Tty => None,
        LogTarget::Test => Some(
            tracing_subscriber::fmt::layer()
                .with_test_writer()
                .with_filter(make_env_filter())
                .boxed(),
        ),
    }
}

fn file_layer() -> (Option<BoxedLayer>, Option<tracing_appender::non_blocking::WorkerGuard>) {
    let Some(path) = resolve_log_file_path() else {
        return (None, None);
    };
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(file) => {
            let (writer, worker_guard) = tracing_appender::non_blocking(file);
            let layer = tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(false)
                .with_filter(make_env_filter())
                .boxed();
            (Some(layer), Some(worker_guard))
        }
        Err(e) => {
            eprintln!(
                "warning: NEOMACS_LOG_FILE={} failed to open: {e}; continuing without file output",
                path.display(),
            );
            (None, None)
        }
    }
}

fn init_inner(target: LogTarget) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    // Bridge `log` → `tracing` for crates using the log facade.
    let _ = tracing_log::LogTracer::init();

    let default = default_layer(target);
    let (file, file_guard) = file_layer();

    // Each layer carries its own `EnvFilter` (per-layer filter), so the
    // registry is not wrapped in a global filter. We combine the
    // (optional) default and file layers into a single `Vec<BoxedLayer>`:
    // `Vec<L>` implements `Layer<S>` when each `L: Layer<S>`, so a single
    // `.with(layers)` call stacks both atop the plain `Registry`. This
    // avoids the trait-bound explosion that occurs when chaining
    // `.with(Option<BoxedLayer>).with(Option<BoxedLayer>)`.
    let mut layers: Vec<BoxedLayer> = Vec::new();
    if let Some(layer) = default {
        layers.push(layer);
    }
    if let Some(layer) = file {
        layers.push(layer);
    }
    let result = tracing_subscriber::registry().with(layers).try_init();
    if let Err(e) = result {
        eprintln!("warning: tracing subscriber init failed: {e}");
    }
    file_guard
}
