//! Process-wide tracing/logging initialization.
//!
//! Two entry points:
//!
//! - [`init`] — for binary entry points. Installs a stderr fmt
//!   subscriber, bridges the `log` facade into tracing, and (if
//!   `NEOMACS_LOG_TO_FILE=1`) also writes to `neomacs-{pid}.log` in the
//!   current working directory via a non-blocking background appender.
//!   Returns a [`LoggingGuard`] that must be kept alive until process
//!   exit so the file appender can flush its queue.
//!
//! - [`init_for_tests`] — for unit/integration tests. Uses
//!   `with_test_writer` so output is captured per-test by the test
//!   harness, defaults to `info` filter, idempotent across many `#[test]`
//!   calls.
//!
//! Both honor `RUST_LOG` and bridge `log` → `tracing` so events from
//! crates using the `log` facade (e.g. `cosmic-text`, `wgpu`) flow into
//! the tracing subscriber.

use std::sync::OnceLock;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

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

/// Initialize tracing for a binary entry point.
///
/// Behavior:
///
/// - Always installs a stderr fmt subscriber filtered by `RUST_LOG`
///   (default `info`).
/// - Bridges crates using the `log` facade (e.g. `cosmic-text`, `wgpu`)
///   into the tracing subscriber via `tracing_log::LogTracer`.
/// - When `NEOMACS_LOG_TO_FILE=1`, also writes to `neomacs-{pid}.log`
///   in the current working directory in append mode, via a non-blocking
///   background writer. The same `RUST_LOG` filter applies.
/// - Idempotent — safe to call multiple times. Only the first call sets
///   up the global subscriber; subsequent calls return an empty guard.
///
/// If `NEOMACS_LOG_TO_FILE=1` is set but the file cannot be opened, a
/// warning is printed to stderr and the function continues with stderr
/// logging only.
pub fn init() -> LoggingGuard {
    static INIT: OnceLock<()> = OnceLock::new();
    let mut guard: Option<tracing_appender::non_blocking::WorkerGuard> = None;
    INIT.get_or_init(|| {
        guard = init_inner();
    });
    LoggingGuard { _file: guard }
}

fn init_inner() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    // Bridge `log` → `tracing` for crates using the log facade.
    let _ = tracing_log::LogTracer::init();

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let stderr_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);

    let want_file = std::env::var("NEOMACS_LOG_TO_FILE")
        .map(|v| v == "1")
        .unwrap_or(false);

    if want_file {
        let pid = std::process::id();
        let path = std::path::PathBuf::from(format!("neomacs-{pid}.log"));
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(file) => {
                let (writer, worker_guard) = tracing_appender::non_blocking(file);
                let file_layer = tracing_subscriber::fmt::layer()
                    .with_writer(writer)
                    .with_ansi(false);
                let result = tracing_subscriber::registry()
                    .with(env_filter)
                    .with(stderr_layer)
                    .with(file_layer)
                    .try_init();
                if let Err(e) = result {
                    eprintln!("warning: tracing subscriber init failed: {e}");
                }
                return Some(worker_guard);
            }
            Err(e) => {
                eprintln!(
                    "warning: NEOMACS_LOG_TO_FILE=1 but failed to open {}: {e}; continuing with stderr only",
                    path.display(),
                );
            }
        }
    }

    let result = tracing_subscriber::registry()
        .with(env_filter)
        .with(stderr_layer)
        .try_init();
    if let Err(e) = result {
        eprintln!("warning: tracing subscriber init failed: {e}");
    }
    None
}

/// Initialize tracing for unit/integration tests.
///
/// Uses `with_test_writer()` so output is captured per-test by the test
/// harness (visible only on test failure). Default filter is `info`,
/// overridable via `RUST_LOG`. Bridges the `log` facade into tracing.
///
/// Idempotent — safe to call from every `#[test]` function. Returns
/// silently if a global subscriber is already installed.
pub fn init_for_tests() {
    let _ = tracing_log::LogTracer::init();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();
}
