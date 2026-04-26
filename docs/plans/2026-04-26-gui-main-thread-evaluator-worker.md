# GUI Main Thread Evaluator Worker Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Refactor GUI/Desktop startup so `winit` always runs on the OS main thread while the Elisp evaluator runs on a dedicated worker thread, without changing TTY or batch startup.

**Architecture:** Keep the existing channel boundary (`ThreadComms`) and rendering pipeline, but invert thread ownership. The main thread becomes the GUI runtime owner and process supervisor; the evaluator thread becomes the Neomacs VM runtime owner and executes `recursive_edit()`. Preserve the input bridge for the first cut to minimize behavioral churn.

**Tech Stack:** Rust, `winit`, `wgpu`, crossbeam channels, `neovm-core` thread-local runtime state, existing `neomacs-display-runtime` render loop.

---

### Task 1: Freeze the Current-Thread GUI Runtime API

**Files:**
- Modify: `neomacs-display-runtime/src/render_thread/bootstrap.rs`
- Modify: `neomacs-display-runtime/src/render_thread/mod.rs`
- Modify: `neomacs-display-runtime/src/lib.rs`
- Test: `neomacs-display-runtime/src/render_thread/thread_handle_test.rs`

**Step 1: Write the failing test**

Add a render-runtime test that proves the product API no longer requires thread spawning for GUI startup. The test should target a small helper API, not a real `winit` loop:

```rust
#[test]
fn run_on_current_thread_surfaces_startup_error() {
    let err = super::run_render_loop_current_thread_for_test(|| Err("boom".to_string()))
        .expect_err("startup error should propagate");
    assert!(err.contains("boom"));
}
```

**Step 2: Run test to verify it fails**

Run:

```bash
cargo test -p neomacs-display-runtime run_on_current_thread_surfaces_startup_error -- --exact
```

Expected: FAIL because the current-thread helper API does not exist yet.

**Step 3: Write minimal implementation**

Reshape `bootstrap.rs` so it exposes a current-thread entrypoint:

```rust
pub fn run_render_loop_current_thread(
    comms: RenderComms,
    width: u32,
    height: u32,
    title: String,
    image_dimensions: SharedImageDimensions,
    shared_monitors: SharedMonitorInfo,
    #[cfg(feature = "neo-term")] shared_terminals: crate::terminal::SharedTerminals,
) -> Result<(), String> {
    let event_loop = build_render_event_loop()?;
    run_render_loop_with_event_loop(
        event_loop,
        comms,
        width,
        height,
        title,
        image_dimensions,
        shared_monitors,
        #[cfg(feature = "neo-term")]
        shared_terminals,
    )
}
```

If testability requires it, add a narrow test-only helper around startup closure execution instead of mocking `winit`.

**Step 4: Run test to verify it passes**

Run:

```bash
cargo test -p neomacs-display-runtime run_on_current_thread_surfaces_startup_error -- --exact
```

Expected: PASS

**Step 5: Commit**

```bash
git add neomacs-display-runtime/src/render_thread/bootstrap.rs neomacs-display-runtime/src/render_thread/mod.rs neomacs-display-runtime/src/lib.rs neomacs-display-runtime/src/render_thread/thread_handle_test.rs
git commit -m "refactor: expose current-thread gui runtime entrypoint"
```

### Task 2: Replace GUI Thread Handle Semantics in `neomacs-bin`

**Files:**
- Modify: `neomacs-bin/src/main.rs:748-763`
- Test: `neomacs-bin/src/main_test.rs`

**Step 1: Write the failing test**

Add a startup-shape test that encodes the intended ownership model:

```rust
#[test]
fn gui_startup_no_longer_requires_render_thread_handle() {
    assert!(!frontend_handle_needs_gui_thread_join());
}
```

Prefer a real helper over a synthetic boolean; the point is to remove the `FrontendHandle::Gui(RenderThread)` assumption from `main.rs`.

**Step 2: Run test to verify it fails**

Run:

```bash
cargo test -p neomacs-bin gui_startup_no_longer_requires_render_thread_handle -- --exact
```

Expected: FAIL because GUI startup is still modeled as a spawned thread handle.

**Step 3: Write minimal implementation**

Refactor the GUI/Desktop control flow so `FrontendHandle` only models TTY/batch ownership. Replace:

```rust
enum FrontendHandle {
    Gui(RenderThread),
    TtyRifInput(...),
    Batch,
}
```

with a shape closer to:

```rust
enum FrontendHandle {
    TtyRifInput(tty_frontend::TtyInputReader),
    Batch,
}
```

and move GUI/Desktop lifecycle ownership into explicit startup/result structs used by `run(...)`.

**Step 4: Run test to verify it passes**

Run:

```bash
cargo test -p neomacs-bin gui_startup_no_longer_requires_render_thread_handle -- --exact
```

Expected: PASS

**Step 5: Commit**

```bash
git add neomacs-bin/src/main.rs neomacs-bin/src/main_test.rs
git commit -m "refactor: remove gui render-thread handle startup model"
```

### Task 3: Extract Evaluator Worker Entry Point

**Files:**
- Modify: `neomacs-bin/src/main.rs:1624-1835`
- Test: `neomacs-bin/src/main_test.rs`

**Step 1: Write the failing test**

Add a test that the evaluator startup body can run independently and report status:

```rust
#[test]
fn gui_evaluator_worker_reports_startup_success_before_recursive_edit_exit() {
    let probe = run_gui_evaluator_worker_startup_probe();
    assert!(probe.started);
    assert!(probe.initial_frame_published);
}
```

**Step 2: Run test to verify it fails**

Run:

```bash
cargo test -p neomacs-bin gui_evaluator_worker_reports_startup_success_before_recursive_edit_exit -- --exact
```

Expected: FAIL because the worker entrypoint/helper does not exist.

**Step 3: Write minimal implementation**

Extract the current evaluator-heavy GUI path from `run(...)` into a dedicated worker entrypoint, for example:

```rust
fn run_gui_evaluator_worker(
    mode: RuntimeMode,
    startup: StartupOptions,
    emacs_comms: EmacsComms,
    width: u32,
    height: u32,
    primary_window_size: SharedPrimaryWindowSize,
    gui_image_dimensions: SharedImageDimensions,
    startup_tx: std::sync::mpsc::SyncSender<Result<(), String>>,
) -> EvaluatorExit
```

This function must own:

- `create_startup_evaluator_for_mode`
- `setup_thread_locals`
- bootstrap buffers/frame
- GUI display-host install
- input system init
- GUI redisplay callback install
- initial frame publish
- `recursive_edit()`
- shutdown request capture

**Step 4: Run test to verify it passes**

Run:

```bash
cargo test -p neomacs-bin gui_evaluator_worker_reports_startup_success_before_recursive_edit_exit -- --exact
```

Expected: PASS

**Step 5: Commit**

```bash
git add neomacs-bin/src/main.rs neomacs-bin/src/main_test.rs
git commit -m "refactor: extract gui evaluator worker entrypoint"
```

### Task 4: Spawn the Evaluator Worker From GUI/Desktop Startup

**Files:**
- Modify: `neomacs-bin/src/main.rs`
- Test: `neomacs-bin/src/main_test.rs`

**Step 1: Write the failing test**

Add a test that GUI mode now spawns the evaluator worker and uses startup channels:

```rust
#[test]
fn gui_startup_spawns_evaluator_worker_before_entering_frontend_runtime() {
    let startup = gui_startup_probe();
    assert!(startup.worker_spawned);
    assert!(startup.waited_for_worker_ready);
}
```

**Step 2: Run test to verify it fails**

Run:

```bash
cargo test -p neomacs-bin gui_startup_spawns_evaluator_worker_before_entering_frontend_runtime -- --exact
```

Expected: FAIL

**Step 3: Write minimal implementation**

In the GUI/Desktop branch of `run(...)`:

- create `ThreadComms`
- split to `(emacs_comms, render_comms)`
- create evaluator startup/result channels
- spawn the evaluator worker thread
- wait for explicit startup success/failure
- if startup succeeds, proceed to current-thread GUI runtime
- if startup fails, print the startup error and exit nonzero

Do not construct the evaluator on the main thread anymore.

**Step 4: Run test to verify it passes**

Run:

```bash
cargo test -p neomacs-bin gui_startup_spawns_evaluator_worker_before_entering_frontend_runtime -- --exact
```

Expected: PASS

**Step 5: Commit**

```bash
git add neomacs-bin/src/main.rs neomacs-bin/src/main_test.rs
git commit -m "refactor: spawn gui evaluator as dedicated worker"
```

### Task 5: Run the GUI Runtime on the OS Main Thread

**Files:**
- Modify: `neomacs-bin/src/main.rs`
- Modify: `neomacs-display-runtime/src/render_thread/thread_handle.rs`
- Modify: `neomacs-bin/src/bin/mock-display.rs`
- Test: `neomacs-bin/src/main_test.rs`

**Step 1: Write the failing test**

Add a probe test that GUI mode calls the current-thread runner:

```rust
#[test]
fn gui_startup_uses_current_thread_render_runtime() {
    let probe = gui_runtime_probe();
    assert!(probe.used_current_thread_runner);
    assert!(!probe.used_render_thread_spawn);
}
```

**Step 2: Run test to verify it fails**

Run:

```bash
cargo test -p neomacs-bin gui_startup_uses_current_thread_render_runtime -- --exact
```

Expected: FAIL

**Step 3: Write minimal implementation**

Replace the current GUI branch:

```rust
let render_thread = RenderThread::spawn(...)?;
FrontendHandle::Gui(render_thread)
```

with a direct call from `run(...)`:

```rust
neomacs_display_runtime::render_thread::run_render_loop_current_thread(
    render_comms,
    width,
    height,
    "Neomacs".to_string(),
    Arc::clone(&gui_image_dimensions),
    Arc::clone(&shared_monitors),
)
```

Update `mock-display` to use the same current-thread runtime shape instead of spawning `RenderThread`.

**Step 4: Run test to verify it passes**

Run:

```bash
cargo test -p neomacs-bin gui_startup_uses_current_thread_render_runtime -- --exact
```

Expected: PASS

**Step 5: Commit**

```bash
git add neomacs-bin/src/main.rs neomacs-bin/src/bin/mock-display.rs neomacs-display-runtime/src/render_thread/thread_handle.rs neomacs-bin/src/main_test.rs
git commit -m "refactor: run gui runtime on process main thread"
```

### Task 6: Rework GUI Shutdown and Exit Status Propagation

**Files:**
- Modify: `neomacs-bin/src/main.rs`
- Modify: `neomacs-display-runtime/src/render_thread/bootstrap.rs`
- Test: `neomacs-bin/src/main_test.rs`

**Step 1: Write the failing test**

Add a shutdown propagation test:

```rust
#[test]
fn gui_shutdown_returns_evaluator_exit_request() {
    let exit = gui_shutdown_probe();
    assert_eq!(exit.code, 17);
    assert!(!exit.restart);
}
```

Also add a GUI-first shutdown test:

```rust
#[test]
fn gui_window_close_notifies_evaluator_before_process_exit() {
    let probe = gui_window_close_probe();
    assert!(probe.window_close_forwarded);
}
```

**Step 2: Run test to verify it fails**

Run:

```bash
cargo test -p neomacs-bin gui_shutdown_returns_evaluator_exit_request -- --exact
cargo test -p neomacs-bin gui_window_close_notifies_evaluator_before_process_exit -- --exact
```

Expected: FAIL

**Step 3: Write minimal implementation**

Introduce an evaluator result struct carried from the worker back to the main thread, for example:

```rust
struct EvaluatorExit {
    shutdown_request: Option<ShutdownRequest>,
    command_loop_ok: bool,
}
```

Main-thread GUI runtime should:

- run until frontend exit
- join the evaluator worker after GUI shutdown
- return evaluator-driven exit status when present

Worker thread should:

- send `RenderCommand::Shutdown` when it decides to exit
- preserve existing `shutdown_request()` semantics

**Step 4: Run test to verify it passes**

Run:

```bash
cargo test -p neomacs-bin gui_shutdown_returns_evaluator_exit_request -- --exact
cargo test -p neomacs-bin gui_window_close_notifies_evaluator_before_process_exit -- --exact
```

Expected: PASS

**Step 5: Commit**

```bash
git add neomacs-bin/src/main.rs neomacs-display-runtime/src/render_thread/bootstrap.rs neomacs-bin/src/main_test.rs
git commit -m "refactor: propagate gui and evaluator shutdown explicitly"
```

### Task 7: Preserve Input Bridge and Quit Semantics

**Files:**
- Modify: `neomacs-bin/src/main.rs`
- Test: `neomacs-bin/src/main_test.rs`
- Test: `neovm-core/src/emacs_core/quit_regression_test.rs`

**Step 1: Write the failing test**

Add a GUI-worker regression test that proves `C-g` still toggles the evaluator quit flag through the bridge:

```rust
#[test]
fn gui_input_bridge_still_sets_quit_requested_for_worker_context() {
    let probe = gui_quit_probe();
    assert!(probe.quit_requested_observed);
}
```

**Step 2: Run test to verify it fails**

Run:

```bash
cargo test -p neomacs-bin gui_input_bridge_still_sets_quit_requested_for_worker_context -- --exact
```

Expected: FAIL

**Step 3: Write minimal implementation**

Keep the existing input bridge thread and make only the ownership changes needed to point it at the evaluator worker’s `quit_requested` atomic and input receiver wiring. Do not fold the bridge into the GUI thread in this refactor.

**Step 4: Run test to verify it passes**

Run:

```bash
cargo test -p neomacs-bin gui_input_bridge_still_sets_quit_requested_for_worker_context -- --exact
```

Expected: PASS

**Step 5: Commit**

```bash
git add neomacs-bin/src/main.rs neomacs-bin/src/main_test.rs neovm-core/src/emacs_core/quit_regression_test.rs
git commit -m "test: preserve gui input bridge quit semantics"
```

### Task 8: Update Documentation and Top-Level Comments

**Files:**
- Modify: `neomacs-bin/src/main.rs:1-10`
- Modify: `neomacs-display-runtime/src/render_thread/mod.rs`
- Modify: `docs/plans/2026-02-04-two-thread-architecture-design.md` (optional reference note only if still maintained)

**Step 1: Write the failing test**

Write a doc-shape test only if the repo already validates docs/comments mechanically. If not, skip introducing a fake test and use compilation plus targeted runtime checks as the verification gate for this task.

**Step 2: Run verification to show stale terminology still exists**

Run:

```bash
rg -n "render thread spawned|Render thread building winit event loop|recursive_edit\\(\\) drives the main event loop" neomacs-bin neomacs-display-runtime
```

Expected: stale ownership wording is still present.

**Step 3: Write minimal implementation**

Update module comments and log messages so they describe:

- GUI runtime on main thread
- evaluator on worker thread
- TTY remaining separate

Do not leave “Render thread” terminology in product-facing startup comments if it now means “main-thread render runtime.”

**Step 4: Run verification to show stale wording is gone**

Run:

```bash
rg -n "render thread spawned|recursive_edit\\(\\) drives the main event loop" neomacs-bin neomacs-display-runtime
```

Expected: no stale product-shape comments remain, or only intentionally retained internal names remain with clarified context.

**Step 5: Commit**

```bash
git add neomacs-bin/src/main.rs neomacs-display-runtime/src/render_thread/mod.rs docs/plans/2026-02-04-two-thread-architecture-design.md
git commit -m "docs: update gui ownership model comments"
```

### Task 9: Full Verification

**Files:**
- Verify only

**Step 1: Run focused Rust tests**

Run:

```bash
cargo test -p neomacs-display-runtime
cargo test -p neomacs-bin
cargo test -p neovm-core quit_requested_atomic_is_drained_into_flag -- --exact
```

Expected: PASS

**Step 2: Run GUI desktop smoke check**

Run:

```bash
nix develop . -c bash -lc 'timeout 10s ./target/release/neomacs -Q'
```

Expected:

- GUI starts
- no `winit` main-thread creation error
- process exits with `124` only because of `timeout`

**Step 3: Run static ownership grep**

Run:

```bash
rg -n "RenderThread::spawn\\(|FrontendHandle::Gui\\(|with_any_thread\\(" neomacs-bin neomacs-display-runtime
```

Expected:

- no GUI/Desktop startup path uses `RenderThread::spawn`
- `FrontendHandle::Gui` is gone
- Linux `with_any_thread` use is either removed or confined to non-product/test-only paths with explicit rationale

**Step 4: Commit final integration state**

```bash
git add -A
git commit -m "refactor: move gui to main thread and evaluator to worker"
```

## Notes for Execution

- This is a big-bang refactor. Do not preserve the old GUI startup path behind a compatibility branch.
- TTY and batch mode are intentionally out of scope except where shared startup helpers force mechanical changes.
- Prefer creating new helper functions for startup/result flow rather than leaving long inline branches in `run(...)`.
- If `Context` construction currently happens before the worker spawn boundary, move construction itself into the worker thread. Do not construct on the main thread and transfer ownership afterward.

Plan complete and saved to `docs/plans/2026-04-26-gui-main-thread-evaluator-worker.md`. Two execution options:

**1. Subagent-Driven (this session)** - I dispatch fresh subagent per task, review between tasks, fast iteration

**2. Parallel Session (separate)** - Open new session with executing-plans, batch execution with checkpoints

Which approach?
