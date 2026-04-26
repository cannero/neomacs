# GUI Main Thread, Evaluator Worker Design

**Date:** 2026-04-26
**Status:** Revised after audit. Do not implement the older startup/shutdown
ordering.
**Scope:** GUI/Desktop only (`winit` frontend). TTY and batch startup must stay
on their current paths.

## Problem

Neomacs currently runs the Elisp evaluator on the process main thread and
spawns the GUI runtime as a render worker. In GUI mode, `neomacs-bin/src/main.rs`
creates the evaluator, bootstraps buffers, installs the display host, starts
`RenderThread::spawn(...)`, publishes the first GUI frame, and then enters
`recursive_edit()`. The render worker builds and owns the `winit` event loop in
`neomacs-display-runtime/src/render_thread/thread_handle.rs` and
`bootstrap.rs`.

That works on Linux only because the current code uses `with_any_thread(true)`
for X11/Wayland. It is the wrong long-term desktop shape because macOS/AppKit
and `winit` require the GUI event loop to be created and run on the process main
thread.

The target is therefore:

- OS main thread owns `winit`, windows, surfaces, IME, monitor snapshots, and
  platform callbacks.
- Evaluator worker owns `Context`, editor state mutation, Lisp-visible thread
  state, and `recursive_edit()`.
- The two communicate only through explicit GUI/evaluator protocol channels.

## GNU Design Anchors

This is not permission to diverge from GNU editor semantics.

GNU Emacs enters the editor from `src/emacs.c` by calling `Frecursive_edit()`.
`src/keyboard.c` owns the command loop, key sequence reading, redisplay before
input waits, and special-event handling. GUI backends feed events into that same
runtime path:

- X/GTK converts window-system events into keyboard-buffer events; for example,
  `WM_DELETE_WINDOW` becomes `DELETE_WINDOW_EVENT` before command-loop handling.
- NS/AppKit must pump Cocoa on the process main thread, but `nsterm.m` still
  routes events back into Emacs input handling instead of letting the GUI layer
  directly mutate editor state.
- `src/thread.c` defines `main-thread` as GNU's Lisp-visible main runtime
  thread.

Neomacs may use a different Rust host-thread topology to satisfy `winit`, but
the Lisp-visible contract must remain GNU-like:

1. User input, resize, focus, close, menu, toolbar, and monitor changes are
   input/events for the evaluator, not frontend-owned editor mutations.
2. Window close is a request. The GUI must not unilaterally terminate the
   editor before the evaluator has run the Lisp/frame policy.
3. Redisplay publication is evaluator-owned. GUI-side animation such as cursor
   blink may render without waking Lisp, but editor state changes still come
   from the evaluator.
4. The evaluator worker is Neomacs's Lisp `main-thread`; the OS process main
   thread is the GUI owner. Code and docs must keep those concepts distinct.

## Decision

For GUI/Desktop mode, run the GUI runtime on the OS main thread and move the
Elisp evaluator to a dedicated worker thread that owns `Context` for its entire
lifetime. TTY and batch mode remain unchanged.

This is a host architecture change, not a change in editor semantics. The
worker-thread evaluator must preserve GNU-style command-loop ownership and event
ordering.

## Ownership Invariants

After the refactor:

1. Only the OS main thread builds and runs the `winit` event loop.
2. Only the evaluator worker mutates `Context`, buffers, frames, windows,
   keymaps, Lisp-visible thread state, timers, and process state.
3. The evaluator worker is Lisp `main-thread`; do not conflate it with the OS
   main thread.
4. The GUI thread never calls into `Context`.
5. The evaluator never directly touches `winit`, `wgpu` surfaces, windows, or
   platform GUI objects.
6. Any blocking evaluator request that needs GUI progress is illegal before the
   GUI event loop is running.
7. The main GUI event loop must not block waiting for an evaluator state that
   can itself depend on the GUI loop draining commands.

## Startup Lifecycle

The critical fix from the audit is ordering. The GUI runtime must be live before
the evaluator performs any GUI-dependent publication or display-host request.

Correct startup:

1. `main()` parses startup options and determines GUI mode.
2. `main()` creates `ThreadComms`, shared image dimensions, shared monitor
   state, primary-window-size tracking, and evaluator lifecycle channels.
3. `main()` spawns the evaluator worker.
4. The evaluator worker creates `Context` on the worker thread, calls
   `setup_thread_locals()`, performs non-GUI-dependent bootstrap, installs the
   channel-backed display host, wires the input bridge/input receiver, and then
   waits for frontend readiness before any initial GUI frame publication.
5. The OS main thread immediately builds the `winit` event loop and enters the
   current-thread GUI runtime. It must not wait for "initial frame published"
   or for any worker state that may require GUI command draining.
6. During `resumed`/initial window creation, the GUI runtime creates the primary
   window, initializes GPU state, records monitors/window size, and signals
   frontend readiness to the evaluator.
7. After frontend readiness, the evaluator may adopt the primary GUI frame,
   publish the initial frame, run startup Lisp through `recursive_edit()`, and
   service normal input.

Allowed startup waits:

- Main may wait for a narrow worker preflight result that is guaranteed not to
  send GUI commands or wait for GUI input.
- Main must not wait for frame publication, image loading, window resize
  acknowledgement, monitor discovery, or any display-host call.

## Protocol Requirements

The current `ThreadComms` split can stay, but the protocol must be made
explicit enough to avoid hidden deadlocks.

Evaluator -> GUI:

- Frame publication is asynchronous. The GUI drains frames and renders the most
  recent state.
- Most `RenderCommand` values are asynchronous. The evaluator must treat send
  failure as a real frontend error.
- Requests that require a reply, such as image dimensions or primary window
  size, must either use an explicit reply channel or a documented shared-state
  handshake. They may only be issued after frontend readiness.
- Blocking `send` on a bounded command channel is not allowed while the GUI
  thread is waiting for the evaluator. Either ensure the GUI loop is running or
  use a non-blocking/error-returning path with tests.

GUI -> Evaluator:

- Window events, keyboard/mouse input, resize, focus, monitor changes, menu bar,
  toolbar, and file-drop events enter the evaluator through input/event
  channels.
- The wakeup pipe must wake the evaluator when input arrives.
- Events dropped because the bounded input channel is full are bugs unless the
  event type is explicitly documented as lossy.

## Shutdown Lifecycle

Window close must mirror GNU's semantic direction: frontend event first, Lisp
policy second.

Correct shutdown:

- If the evaluator exits first, it sends `RenderCommand::Shutdown`; the GUI
  event loop exits; main returns the evaluator's exit status.
- If the user requests window close, the GUI sends `WindowClose`/delete-frame
  style input to the evaluator and keeps the window/runtime alive. The
  evaluator decides whether to delete a frame, run `kill-emacs`, ignore/cancel,
  or surface an error.
- The GUI event loop may exit first only for fatal frontend/backend loss. That
  path must notify the evaluator and produce a clear error/shutdown result.

## Non-Goals

This refactor does not attempt to:

- change TTY or batch startup
- redesign Lisp thread semantics
- move layout out of the evaluator
- eliminate the input bridge in the first pass
- hide existing GUI rendering bugs

## Expected Code Shape

- `neomacs-display-runtime` exposes a current-thread GUI runtime entrypoint for
  product GUI startup.
- Product GUI startup no longer uses `RenderThread::spawn(...)`.
- `neomacs-bin/src/main.rs` has an explicit GUI lifecycle model: main-thread
  frontend owner, evaluator-worker owner, frontend-ready signal, evaluator-exit
  result.
- Display-host operations are classified as async commands or reply-bearing
  requests.
- Primary window close no longer calls `event_loop.exit()` until evaluator
  policy has produced shutdown/delete-frame intent.
- Comments and logs distinguish "OS main thread" from "Lisp main-thread".

## Risks

- Startup deadlock if main waits for an evaluator milestone that requires the
  GUI loop to drain `RenderCommand`.
- Shutdown semantic regression if close destroys the GUI before Lisp policy can
  run.
- Hidden `Context` thread-affinity assumptions in GC, thread-local runtime
  state, or `DisplayHost`.
- Existing sync operations such as image dimension resolution timing out if they
  run before frontend readiness.
- Confusing docs/logs that call both OS main thread and Lisp main-thread "main".

## Validation

Success criteria:

1. GUI startup no longer relies on Linux `with_any_thread(true)` in product
   paths.
2. GUI event loop is created and run on the OS main thread.
3. Evaluator `Context` is constructed and run on the evaluator worker.
4. First GUI frame publication happens only after frontend readiness.
5. Primary window close is delivered to evaluator policy before event-loop exit.
6. Input, resize, focus, monitor, menu, toolbar, image, and shutdown events keep
   GNU-style ordering.
7. TTY and batch behavior remain unchanged.
8. Verification uses `cargo nextest` exclusively.

Primary smoke check:

```bash
nix develop . -c bash -lc 'timeout 10s ./target/release/neomacs -Q'
```

Expected result:

- GUI window starts.
- No `winit` main-thread creation error.
- No startup deadlock before the first frame.
- Process exits with `124` only because of `timeout`.
