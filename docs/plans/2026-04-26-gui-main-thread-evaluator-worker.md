# GUI Main Thread Evaluator Worker Implementation Plan

**Goal:** Refactor GUI/Desktop startup so `winit` runs on the OS main thread
while the Elisp evaluator runs on a dedicated worker thread, without changing
TTY or batch startup.

**Rules for this plan:**

- Study GNU Emacs source before touching Neomacs code.
- Preserve GNU command-loop and event semantics at the Lisp boundary.
- Use `cargo nextest` exclusively.
- Commit locally after each coherent task. Do not push unless explicitly asked.
- Do not hide startup, shutdown, or rendering bugs behind compatibility
  branches or timeouts.

**Architecture:** Keep the existing channel boundary (`ThreadComms`) and
rendering pipeline, but invert host-thread ownership. The OS main thread becomes
the GUI runtime owner. The evaluator worker becomes the Neomacs VM runtime owner
and executes `recursive_edit()`.

The critical constraint is startup ordering: the GUI event loop must be running
before the evaluator publishes the first GUI frame or performs any display-host
operation that can require GUI progress.

---

## Task 0: Reconfirm GNU Design Anchors

**Files:**

- Read only: `/home/exec/Projects/github.com/emacs-mirror/emacs/src/emacs.c`
- Read only: `/home/exec/Projects/github.com/emacs-mirror/emacs/src/keyboard.c`
- Read only: `/home/exec/Projects/github.com/emacs-mirror/emacs/src/thread.c`
- Read only: `/home/exec/Projects/github.com/emacs-mirror/emacs/src/xterm.c`
- Read only: `/home/exec/Projects/github.com/emacs-mirror/emacs/src/nsterm.m`
- Read only: `/home/exec/Projects/github.com/emacs-mirror/emacs/src/xdisp.c`

Before implementation, verify these facts against GNU source:

- `emacs.c` enters the editor through `Frecursive_edit()`.
- `keyboard.c` owns `command_loop`, `command_loop_1`, key sequence reading,
  input wait redisplay, and special-event handling.
- `thread.c` defines Lisp `main-thread` as the main runtime thread object.
- X/GTK turns window-system close into a delete-window input event.
- NS/AppKit pumps Cocoa on the process main thread, but still routes GUI events
  into Emacs input handling.
- Redisplay remains evaluator/editor-owned. GUI-side cursor blink can render
  without waking Lisp, but editor state changes cannot move to the GUI thread.

Record any contradictory finding in the design doc before editing code.

No commit for this task unless the docs need correction.

## Task 1: Expose Current-Thread GUI Runtime API

**Files:**

- Modify: `neomacs-display-runtime/src/render_thread/bootstrap.rs`
- Modify: `neomacs-display-runtime/src/render_thread/mod.rs`
- Modify: `neomacs-display-runtime/src/lib.rs`
- Test: `neomacs-display-runtime/src/render_thread/thread_handle_test.rs`

### Step 1: Write the failing test

Add a render-runtime test that proves the product API can surface current-thread
startup errors without spawning a render thread. Target a small helper API, not
a real `winit` loop:

```rust
#[test]
fn run_on_current_thread_surfaces_startup_error() {
    let err = super::run_render_loop_current_thread_for_test(|| Err("boom".to_string()))
        .expect_err("startup error should propagate");
    assert!(err.contains("boom"));
}
```

### Step 2: Run the failing test

```bash
cargo nextest run -p neomacs-display-runtime run_on_current_thread_surfaces_startup_error
```

Expected: FAIL because the current-thread helper does not exist yet.

### Step 3: Implement the current-thread entrypoint

Expose a product entrypoint like:

```rust
pub fn run_render_loop_current_thread(
    comms: RenderComms,
    width: u32,
    height: u32,
    title: String,
    image_dimensions: SharedImageDimensions,
    shared_monitors: SharedMonitorInfo,
    frontend_ready: FrontendReadyNotifier,
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
        frontend_ready,
        #[cfg(feature = "neo-term")]
        shared_terminals,
    )
}
```

`FrontendReadyNotifier` should be a narrow display-runtime type, a callback, or
an existing `RenderComms` event. `neomacs-display-runtime` must not depend on
`neomacs-bin`. The important property is that the GUI runtime can signal
"primary window and event loop are live" from inside the actual `winit`
lifecycle.

Do not remove `RenderThread` yet if tests or non-product helpers still use it.
The product GUI path will stop using it in a later task.

### Step 4: Verify

```bash
cargo nextest run -p neomacs-display-runtime run_on_current_thread_surfaces_startup_error
```

Expected: PASS.

### Step 5: Commit

```bash
git add neomacs-display-runtime/src/render_thread/bootstrap.rs neomacs-display-runtime/src/render_thread/mod.rs neomacs-display-runtime/src/lib.rs neomacs-display-runtime/src/render_thread/thread_handle_test.rs
git commit -m "refactor: expose current-thread gui runtime entrypoint"
```

## Task 2: Add Explicit GUI Lifecycle Protocol

**Files:**

- Modify: `neomacs-bin/src/main.rs`
- Test: `neomacs-bin/src/main_test.rs`

### Step 1: Write failing protocol tests

Add tests for the two lifecycle invariants that prevent the audited deadlock:

```rust
#[test]
fn gui_worker_preflight_does_not_publish_frame_or_send_render_commands() {
    let probe = gui_worker_preflight_probe();
    assert!(probe.preflight_reported);
    assert_eq!(probe.frames_published, 0);
    assert_eq!(probe.render_commands_sent, 0);
}

#[test]
fn gui_startup_does_not_wait_for_initial_frame_before_entering_gui_runtime() {
    let probe = gui_startup_order_probe();
    assert!(probe.worker_spawned);
    assert!(probe.gui_runtime_entered);
    assert!(!probe.waited_for_initial_frame);
}
```

### Step 2: Run failing tests

```bash
cargo nextest run -p neomacs-bin gui_worker_preflight_does_not_publish_frame_or_send_render_commands
cargo nextest run -p neomacs-bin gui_startup_does_not_wait_for_initial_frame_before_entering_gui_runtime
```

Expected: FAIL because the protocol helpers do not exist and current startup is
still render-thread based.

### Step 3: Implement protocol types

Introduce narrow internal types in `neomacs-bin/src/main.rs`, for example:

```rust
struct GuiFrontendReady {
    primary_width: u32,
    primary_height: u32,
}

enum GuiEvaluatorStartup {
    PreflightReady,
    Fatal(String),
}

struct EvaluatorExit {
    shutdown_request: Option<ShutdownRequest>,
    command_loop_ok: bool,
}
```

Protocol rules:

- `PreflightReady` means the evaluator worker created `Context` and completed
  only non-GUI-dependent setup.
- `PreflightReady` must be sent before `adopt_existing_primary_gui_frame`,
  `publish_gui_frame`, image resolution, or any display-host operation that may
  need the GUI loop to drain commands.
- Main may wait for `PreflightReady` only if the worker cannot send GUI
  commands while producing it.
- Main must never wait for "initial frame published" before entering the GUI
  runtime.

### Step 4: Verify

```bash
cargo nextest run -p neomacs-bin gui_worker_preflight_does_not_publish_frame_or_send_render_commands
cargo nextest run -p neomacs-bin gui_startup_does_not_wait_for_initial_frame_before_entering_gui_runtime
```

Expected: PASS.

### Step 5: Commit

```bash
git add neomacs-bin/src/main.rs neomacs-bin/src/main_test.rs
git commit -m "refactor: add gui evaluator lifecycle protocol"
```

## Task 3: Extract Evaluator Worker in Two Phases

**Files:**

- Modify: `neomacs-bin/src/main.rs`
- Test: `neomacs-bin/src/main_test.rs`

### Step 1: Write failing tests

Add tests that encode the two-phase worker:

```rust
#[test]
fn gui_evaluator_waits_for_frontend_ready_before_initial_publish() {
    let probe = gui_evaluator_worker_startup_probe();
    assert!(probe.preflight_reported);
    assert!(!probe.initial_frame_published_before_frontend_ready);
    assert!(probe.initial_frame_published_after_frontend_ready);
}
```

### Step 2: Run failing test

```bash
cargo nextest run -p neomacs-bin gui_evaluator_waits_for_frontend_ready_before_initial_publish
```

Expected: FAIL because the worker entrypoint does not exist.

### Step 3: Extract the worker entrypoint

Extract the GUI evaluator path from `run(...)` into a worker-owned entrypoint:

```rust
fn run_gui_evaluator_worker(
    mode: RuntimeMode,
    startup: StartupOptions,
    emacs_comms: EmacsComms,
    initial_width: u32,
    initial_height: u32,
    primary_window_size: SharedPrimaryWindowSize,
    gui_image_dimensions: SharedImageDimensions,
    startup_tx: std::sync::mpsc::SyncSender<GuiEvaluatorStartup>,
    frontend_ready_rx: std::sync::mpsc::Receiver<GuiFrontendReady>,
) -> EvaluatorExit
```

Worker phase A, before frontend readiness:

- `create_startup_evaluator_for_mode`
- `setup_thread_locals`
- `set_max_depth`
- terminal runtime reset/config matching GUI mode
- bootstrap buffers/frame only if this does not call the GUI display host
- input bridge setup and `init_input_system`
- redisplay callback construction, but no first publish
- send `PreflightReady`

Worker phase B, after frontend readiness:

- record actual primary window size
- install or activate the GUI display host if it was deferred
- `adopt_existing_primary_gui_frame`
- `publish_gui_frame`
- add startup undo boundary
- `maybe_run_after_pdump_load_hook`
- enter `recursive_edit()`
- capture `shutdown_request()`

Do not construct `Context` on the OS main thread and move it later.

### Step 4: Verify

```bash
cargo nextest run -p neomacs-bin gui_evaluator_waits_for_frontend_ready_before_initial_publish
```

Expected: PASS.

### Step 5: Commit

```bash
git add neomacs-bin/src/main.rs neomacs-bin/src/main_test.rs
git commit -m "refactor: extract phased gui evaluator worker"
```

## Task 4: Run GUI Runtime on the OS Main Thread

**Files:**

- Modify: `neomacs-bin/src/main.rs`
- Modify: `neomacs-display-runtime/src/render_thread/bootstrap.rs`
- Modify: `neomacs-display-runtime/src/render_thread/thread_handle.rs`
- Modify: `neomacs-bin/src/bin/mock-display.rs`
- Test: `neomacs-bin/src/main_test.rs`

### Step 1: Write failing tests

```rust
#[test]
fn gui_startup_uses_current_thread_render_runtime() {
    let probe = gui_runtime_probe();
    assert!(probe.used_current_thread_runner);
    assert!(!probe.used_render_thread_spawn);
}

#[test]
fn gui_frontend_ready_is_sent_from_winit_lifecycle() {
    let probe = gui_frontend_ready_probe();
    assert!(probe.ready_sent_after_resumed);
    assert!(probe.primary_window_size_nonzero);
}
```

### Step 2: Run failing tests

```bash
cargo nextest run -p neomacs-bin gui_startup_uses_current_thread_render_runtime
cargo nextest run -p neomacs-bin gui_frontend_ready_is_sent_from_winit_lifecycle
```

Expected: FAIL while GUI startup still uses `RenderThread::spawn`.

### Step 3: Implement main-thread GUI startup

In the GUI/Desktop branch of `run(...)`:

- create `ThreadComms`
- split to `(emacs_comms, render_comms)`
- create evaluator startup, frontend-ready, and evaluator-exit channels
- spawn evaluator worker
- wait only for `GuiEvaluatorStartup::PreflightReady`
- immediately call `run_render_loop_current_thread(...)` on the OS main thread
- send frontend readiness from the real `winit` lifecycle after the primary
  window and GPU state are live
- after the GUI runtime exits, join/receive the evaluator result

Do not wait on first frame publication or any worker state that can require the
GUI loop to drain `RenderCommand`.

Update `mock-display` to match the new current-thread runtime shape or keep its
old model explicitly test-only with a comment.

### Step 4: Verify

```bash
cargo nextest run -p neomacs-bin gui_startup_uses_current_thread_render_runtime
cargo nextest run -p neomacs-bin gui_frontend_ready_is_sent_from_winit_lifecycle
```

Expected: PASS.

### Step 5: Commit

```bash
git add neomacs-bin/src/main.rs neomacs-bin/src/bin/mock-display.rs neomacs-display-runtime/src/render_thread/bootstrap.rs neomacs-display-runtime/src/render_thread/thread_handle.rs neomacs-bin/src/main_test.rs
git commit -m "refactor: run gui runtime on process main thread"
```

## Task 5: Harden Display-Host Protocol Against Deadlocks

**Files:**

- Modify: `neomacs-bin/src/main.rs`
- Modify: `neomacs-display-runtime/src/thread_comm.rs`
- Test: `neomacs-bin/src/main_test.rs`

### Step 1: Write failing tests

```rust
#[test]
fn gui_startup_main_thread_never_waits_while_worker_can_send_render_commands() {
    let probe = gui_deadlock_probe();
    assert!(!probe.main_waited_during_gui_command_phase);
}

#[test]
fn gui_reply_requests_are_rejected_before_frontend_ready() {
    let probe = gui_reply_request_before_ready_probe();
    assert!(probe.rejected_without_blocking);
}
```

### Step 2: Run failing tests

```bash
cargo nextest run -p neomacs-bin gui_startup_main_thread_never_waits_while_worker_can_send_render_commands
cargo nextest run -p neomacs-bin gui_reply_requests_are_rejected_before_frontend_ready
```

Expected: FAIL until protocol state is explicit.

### Step 3: Implement protocol checks

Classify display-host operations:

- async commands: frame title, resize request, geometry hints, cursor blink,
  create/destroy window
- reply-bearing requests: primary window size, image dimensions, future sync
  font or platform queries

Requirements:

- Reply-bearing requests must verify frontend readiness before blocking.
- Any bounded-channel send path must be impossible to call while the GUI thread
  is waiting for the evaluator.
- Send failure is a real error. Do not silently drop commands that affect editor
  semantics.
- Lossy events must be explicitly documented; close, resize, focus, keyboard,
  menu, toolbar, and file-drop events are not lossy.

### Step 4: Verify

```bash
cargo nextest run -p neomacs-bin gui_startup_main_thread_never_waits_while_worker_can_send_render_commands
cargo nextest run -p neomacs-bin gui_reply_requests_are_rejected_before_frontend_ready
```

Expected: PASS.

### Step 5: Commit

```bash
git add neomacs-bin/src/main.rs neomacs-display-runtime/src/thread_comm.rs neomacs-bin/src/main_test.rs
git commit -m "refactor: make gui evaluator protocol readiness explicit"
```

## Task 6: Preserve GNU Window-Close Semantics

**Files:**

- Modify: `neomacs-display-runtime/src/render_thread/window_events.rs`
- Modify: `neomacs-display-runtime/src/render_thread/bootstrap.rs`
- Modify: `neomacs-display-runtime/src/render_thread/command_processing.rs`
- Modify: `neomacs-bin/src/input_bridge.rs`
- Test: `neomacs-bin/src/main_test.rs`
- Test: `neovm-core/src/emacs_core/eval_test.rs`

### Step 1: Write failing tests

```rust
#[test]
fn gui_window_close_is_delivered_to_evaluator_before_frontend_exit() {
    let probe = gui_window_close_probe();
    assert!(probe.window_close_forwarded);
    assert!(!probe.event_loop_exited_before_evaluator_policy);
}

#[test]
fn primary_window_close_waits_for_evaluator_shutdown_or_delete_frame() {
    let probe = gui_primary_close_policy_probe();
    assert!(probe.evaluator_saw_close);
    assert!(probe.gui_exited_only_after_policy);
}
```

### Step 2: Run failing tests

```bash
cargo nextest run -p neomacs-bin gui_window_close_is_delivered_to_evaluator_before_frontend_exit
cargo nextest run -p neomacs-bin primary_window_close_waits_for_evaluator_shutdown_or_delete_frame
```

Expected: FAIL because current primary close exits the event loop immediately.

### Step 3: Implement evaluator-mediated close

Change GUI close handling:

- On `WindowEvent::CloseRequested`, send `InputEvent::WindowClose`.
- Do not call `event_loop.exit()` for a normal user close request.
- Do not destroy secondary windows immediately either; wait for evaluator
  delete-frame policy to send `RenderCommand::DestroyWindow` or shutdown.
- Only fatal backend loss may exit the GUI runtime first, and that path must
  notify the evaluator and produce an explicit error/shutdown result.

This mirrors GNU's direction: window-system close becomes an input event, then
the command loop/Lisp frame policy decides what happens.

### Step 4: Verify

```bash
cargo nextest run -p neomacs-bin gui_window_close_is_delivered_to_evaluator_before_frontend_exit
cargo nextest run -p neomacs-bin primary_window_close_waits_for_evaluator_shutdown_or_delete_frame
```

Expected: PASS.

### Step 5: Commit

```bash
git add neomacs-display-runtime/src/render_thread/window_events.rs neomacs-display-runtime/src/render_thread/bootstrap.rs neomacs-display-runtime/src/render_thread/command_processing.rs neomacs-bin/src/input_bridge.rs neomacs-bin/src/main_test.rs neovm-core/src/emacs_core/eval_test.rs
git commit -m "fix: route gui window close through evaluator policy"
```

## Task 7: Preserve Input Bridge, Quit, and Throw-On-Input Semantics

**Files:**

- Modify: `neomacs-bin/src/main.rs`
- Test: `neomacs-bin/src/main_test.rs`
- Test: `neovm-core/src/emacs_core/quit_regression_test.rs`
- Test: `neovm-core/src/emacs_core/eval_test.rs`

### Step 1: Write failing tests

```rust
#[test]
fn gui_input_bridge_still_sets_quit_requested_for_worker_context() {
    let probe = gui_quit_probe();
    assert!(probe.quit_requested_observed);
}

#[test]
fn gui_input_still_interrupts_while_no_input_on_worker() {
    let probe = gui_throw_on_input_probe();
    assert!(probe.throw_observed);
    assert!(probe.input_preserved_for_later_read);
}
```

### Step 2: Run failing tests

```bash
cargo nextest run -p neomacs-bin gui_input_bridge_still_sets_quit_requested_for_worker_context
cargo nextest run -p neomacs-bin gui_input_still_interrupts_while_no_input_on_worker
```

Expected: FAIL until input bridge ownership points at the worker context.

### Step 3: Implement

Keep the existing input bridge thread for this refactor. Move only the ownership
needed so it reads display events from `EmacsComms::input_rx`, writes evaluator
keyboard events to the worker's input receiver, and flips the worker
`quit_requested` atomic for default `C-g`.

Do not fold the bridge into the GUI thread in this refactor.

### Step 4: Verify

```bash
cargo nextest run -p neomacs-bin gui_input_bridge_still_sets_quit_requested_for_worker_context
cargo nextest run -p neomacs-bin gui_input_still_interrupts_while_no_input_on_worker
cargo nextest run -p neovm-core quit_requested_atomic_is_drained_into_flag
```

Expected: PASS.

### Step 5: Commit

```bash
git add neomacs-bin/src/main.rs neomacs-bin/src/main_test.rs neovm-core/src/emacs_core/quit_regression_test.rs neovm-core/src/emacs_core/eval_test.rs
git commit -m "test: preserve gui worker input quit semantics"
```

## Task 8: Update Documentation, Logs, and Naming

**Files:**

- Modify: `neomacs-bin/src/main.rs`
- Modify: `neomacs-display-runtime/src/render_thread/mod.rs`
- Modify: `neomacs-display-runtime/src/render_thread/bootstrap.rs`
- Modify: `docs/plans/2026-02-04-two-thread-architecture-design.md` if still referenced

### Step 1: Verify stale wording

```bash
rg -n "render thread spawned|Render thread building winit event loop|recursive_edit\\(\\) drives the main event loop|GUI exits[[:space:]]+first|Lisp main thread == OS main thread" neomacs-bin neomacs-display-runtime
```

Expected: stale ownership wording exists before this task.

### Step 2: Implement

Update comments and logs so they distinguish:

- OS main thread: `winit` and platform GUI owner
- Lisp `main-thread`: evaluator worker and `Context` owner
- TTY/batch: unchanged startup topology

Do not leave product-facing logs saying "render thread spawned" for the GUI
product path. Internal `RenderThread` names may remain only where the old helper
still literally spawns a thread.

### Step 3: Verify

```bash
rg -n "render thread spawned|recursive_edit\\(\\) drives the main event loop|GUI exits[[:space:]]+first|Lisp main thread == OS main thread" neomacs-bin neomacs-display-runtime
```

Expected: no stale product-shape wording remains.

### Step 4: Commit

```bash
git add neomacs-bin/src/main.rs neomacs-display-runtime/src/render_thread/mod.rs neomacs-display-runtime/src/render_thread/bootstrap.rs docs/plans/2026-02-04-two-thread-architecture-design.md
git commit -m "docs: clarify gui and evaluator thread ownership"
```

## Task 9: Full Verification

**Files:**

- Verify only

### Step 1: Run focused Rust tests

```bash
cargo nextest run -p neomacs-display-runtime
cargo nextest run -p neomacs-bin
cargo nextest run -p neovm-core quit_requested_atomic_is_drained_into_flag
```

Expected: PASS.

### Step 2: Run GUI desktop smoke check

```bash
nix develop . -c bash -lc 'timeout 10s ./target/release/neomacs -Q'
```

Expected:

- GUI starts.
- No `winit` main-thread creation error.
- No deadlock before first frame.
- Process exits with `124` only because of `timeout`.

### Step 3: Run static ownership greps

```bash
rg -n "RenderThread::spawn\\(|FrontendHandle::Gui\\(|with_any_thread\\(" neomacs-bin neomacs-display-runtime
rg -n "cargo[[:space:]]+test" docs/plans/2026-04-26-gui-main-thread-evaluator-worker.md docs/plans/2026-04-26-gui-main-thread-evaluator-worker-design.md
```

Expected:

- No GUI/Desktop product startup path uses `RenderThread::spawn`.
- `FrontendHandle::Gui` is gone from product startup.
- Linux `with_any_thread` is removed from product startup or left only in
  clearly documented test/non-product code.
- No Cargo built-in test-runner command remains in these plan docs.

### Step 4: Final integration commit

Only if Task 9 caused follow-up edits:

```bash
git add neomacs-bin/src/main.rs neomacs-display-runtime/src/render_thread/bootstrap.rs neomacs-display-runtime/src/render_thread/window_events.rs neomacs-display-runtime/src/thread_comm.rs docs/plans/2026-02-04-two-thread-architecture-design.md
git commit -m "refactor: move gui runtime to process main thread"
```

## Notes for Execution

- Implement in order. Do not start with the worker move before the lifecycle
  protocol tests exist.
- Do not preserve the old GUI startup path as a hidden product fallback.
- TTY and batch are intentionally out of scope except where shared helpers force
  mechanical changes.
- `Context` must be constructed on the evaluator worker, not constructed on the
  OS main thread and transferred later.
- If a task reveals that the design is wrong, stop and revise the design doc
  before patching around it.
