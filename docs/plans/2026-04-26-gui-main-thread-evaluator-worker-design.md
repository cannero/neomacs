# GUI Main Thread, Evaluator Worker Design

**Date:** 2026-04-26
**Status:** Approved in working session; ready for big-bang implementation
**Scope:** GUI/Desktop only (`winit` frontend). TTY and batch mode stay on their current ownership model.

## Problem

Neomacs currently runs the Elisp evaluator on the process main thread and spawns the GUI runtime as a render worker. In GUI mode, `neomacs-bin/src/main.rs` creates the evaluator, bootstraps buffers, installs the display host, then starts `RenderThread::spawn(...)`, after which the original thread enters `recursive_edit()`. The render worker builds and owns the `winit` event loop in `neomacs-display-runtime/src/render_thread/thread_handle.rs` and `bootstrap.rs`.

That shape is acceptable on Linux only because `winit` is currently configured with `with_any_thread(true)` on X11 and Wayland. It is not a sound long-term desktop model because macOS requires AppKit and `winit` event-loop ownership on the process main thread. Keeping the current topology would force either a permanent platform-specific exception or an unsound startup path on macOS.

The design goal is therefore not “make macOS pass,” but “establish one correct desktop ownership model.” The chosen model is:

- OS main thread owns the GUI runtime.
- Evaluator thread owns `Context` and all editor mutations.
- The channel boundary remains the existing `ThreadComms` split.

This keeps GUI platform rules satisfied while also isolating frontend work from Elisp latency.

## Decision

For GUI/Desktop mode, Neomacs will run `winit`, window creation, `wgpu` surface ownership, monitor discovery, IME, platform callbacks, and GLib/WebKit pumping on the process main thread. The Elisp evaluator will move to a dedicated worker thread that owns the `Context` for its entire lifetime and executes `recursive_edit()`.

TTY and batch mode do not have the `winit` main-thread constraint and should remain on their current simpler path. This refactor is intentionally asymmetric:

- GUI/Desktop: main-thread frontend supervisor + evaluator worker
- TTY/Batch: existing startup path

This is a deliberate design choice, not an incomplete unification. The desktop GUI path has stricter platform requirements and a stronger need for latency isolation. TTY does not.

## Why Not Single-Thread GUI + Evaluator?

It is possible to keep both `winit` and the evaluator on the process main thread. That would be closer to GNU Emacs’s ownership model, where the main thread also services the GUI backend. But that is not the right fit for Neomacs’s stated goals.

If GUI and evaluator share one thread:

- OS event pumping, IME, resize handling, redraw scheduling, and GPU submission directly compete with Elisp execution.
- Long-running Elisp work freezes the GUI.
- Busy GUI work steals time from the evaluator.
- Future WebKit/media integration increases jitter on the same thread.

Neomacs already has a clean inter-thread boundary between evaluator-side frame production and frontend-side rendering/input. Preserving that separation while flipping thread ownership is lower risk than collapsing everything onto one thread and then trying to time-slice the result.

The worker-thread evaluator is therefore not a compromise. It is the preferred desktop model.

## Ownership Invariants

After the refactor, these invariants must hold:

1. The OS main thread is the only thread allowed to build or run the GUI event loop.
2. The evaluator thread is the only thread allowed to mutate `Context`, buffer state, frame state, or Lisp-visible runtime state.
3. The evaluator thread is the Neomacs VM “main thread” for Lisp semantics. It does not need to be the OS process main thread.
4. All GUI input observed by the OS main thread reaches the evaluator through explicit channel handoff.
5. All frame publication and GUI commands observed by the evaluator reach the GUI thread through explicit channel handoff.
6. No subsystem may rely on “GUI thread == evaluator thread” in GUI mode after the refactor.

These invariants matter more than preserving today’s naming. If a type called `RenderThread` no longer represents the product architecture, the code should be renamed rather than keeping misleading ownership semantics.

## Thread Roles

### OS Main Thread

The main thread owns:

- `winit::event_loop::EventLoop`
- window creation and `run_app()`
- `wgpu` surface/device/queue ownership already stored in `RenderApp`
- monitor snapshots
- IME and window callbacks
- GLib/WebKit event pumping
- frontend shutdown mechanics

The main thread does not own:

- `Context`
- `recursive_edit()`
- editor state mutation
- Lisp thread bookkeeping

### Evaluator Worker Thread

The worker thread owns:

- `Context`
- `setup_thread_locals()`
- bootstrap evaluator/image load
- frame/buffer bootstrap
- display host installation
- input system wiring
- `redisplay_fn`
- `recursive_edit()`
- shutdown request production

The evaluator must be constructed on this worker thread, not created on the main thread and moved later. `Context` setup establishes thread-local runtime state and GC stack assumptions in `neovm-core`, so the lifetime owner thread should also be the construction thread.

### Input Bridge Thread

The existing input bridge can remain for the first cut. It already translates frontend `InputEvent` values into evaluator keyboard events and updates `quit_requested` without requiring `&mut Context` access. Folding this work into the GUI thread is optional cleanup, not a prerequisite for the ownership inversion.

## Startup Lifecycle

GUI/Desktop startup becomes:

1. `main()` parses startup options and determines GUI mode.
2. `main()` creates `ThreadComms`, shared image dimensions, shared monitor state, primary-window-size tracking, and an evaluator startup/result channel.
3. `main()` spawns the evaluator worker.
4. The evaluator worker creates the `Context`, calls `setup_thread_locals()`, bootstraps buffers and frame state, installs the GUI display host, connects `init_input_system(...)`, sets the GUI redisplay callback, publishes the initial frame, and enters `recursive_edit()`.
5. The OS main thread builds the event loop and immediately runs the frontend runtime on the current thread.

Steady state:

- evaluator thread publishes `FrameDisplayState` and `RenderCommand`
- GUI thread consumes them and renders/presents
- GUI thread sends `InputEvent`
- input bridge converts and forwards evaluator keyboard events

## Shutdown Lifecycle

Shutdown becomes asymmetric:

- If the evaluator exits first, it sends `RenderCommand::Shutdown`; the GUI event loop exits; the main thread returns the evaluator’s exit status.
- If the GUI exits first, it sends `WindowClose` back toward the evaluator; the evaluator decides whether that means clean shutdown, `kill-emacs`, or surfaced error.
- The main thread owns process shutdown mechanics, but not editor shutdown policy.

This keeps process control with the GUI owner thread while keeping semantic shutdown decisions with the evaluator owner thread.

## Non-Goals

This refactor does not attempt to:

- unify TTY and GUI under one thread topology
- redesign the `ThreadComms` protocol beyond ownership-driven changes
- eliminate the input bridge in the first pass
- change layout/rendering responsibilities
- redesign Elisp thread semantics

The goal is ownership inversion, not frontend feature work.

## Expected Code Shape

The main structural changes are:

- `neomacs-display-runtime` exposes a current-thread GUI runtime entrypoint for desktop startup.
- `RenderThread::spawn(...)` stops being the product-facing GUI startup path.
- `neomacs-bin/src/main.rs` extracts the evaluator-heavy GUI setup into a worker-thread entrypoint.
- GUI startup result propagation becomes explicit rather than piggybacking on render-thread spawn success.
- `FrontendHandle::Gui(RenderThread)` disappears or is replaced by a non-thread-handle GUI result model.

TTY and batch mode continue to use the existing `FrontendHandle` ownership model.

## Risks

The real risks are hidden thread-affinity assumptions:

- evaluator startup currently happens on the same thread that parsed args and created frontend state
- some helpers may assume startup happens before frontend existence
- shutdown code currently assumes “send shutdown, then join frontend”
- documentation and comments frequently describe the evaluator as the process main loop owner

The refactor must update those assumptions consistently rather than leaving partial ownership leakage behind.

## Validation

Success criteria:

1. GUI/Desktop startup no longer requires `with_any_thread(true)` as a product architecture crutch.
2. The GUI event loop is built and run on the OS main thread.
3. The evaluator is constructed and run on its dedicated worker thread.
4. Input, frame publication, and shutdown semantics remain correct.
5. TTY and batch mode remain unchanged.

Primary smoke check:

```bash
nix develop . -c bash -lc 'timeout 10s ./target/release/neomacs -Q'
```

Expected result:

- GUI window starts
- no `winit` “event loop must be created on the main thread” failure
- process exits only because of the timeout

## Conclusion

The correct long-term desktop model for Neomacs is:

- `winit` on the OS main thread
- evaluator on a dedicated worker thread
- TTY/batch unchanged

This preserves platform correctness, isolates frontend work from Elisp latency, and keeps Neomacs’s existing two-role architecture instead of collapsing it into a single-thread runtime that would directly work against the project’s performance goals.
