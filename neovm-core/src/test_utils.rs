//! Common test utilities for neovm-core.
//!
//! Provides shared helpers used across all test modules.

/// Initialize the tracing subscriber for test output.
///
/// Reads `RUST_LOG` env var for filter level (default: `info`).
/// Uses `with_test_writer()` so output is captured by the test runner
/// and shown on failure.
///
/// Safe to call multiple times — `try_init()` silently no-ops if
/// already initialized.
///
/// # Usage
/// Call at the start of any test that needs tracing:
/// ```rust,ignore
/// #[test]
/// fn my_test() {
///     crate::test_utils::init_test_tracing();
///     // ... test code ...
/// }
/// ```
pub fn init_test_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug")),
        )
        .with_test_writer()
        .try_init();
}
