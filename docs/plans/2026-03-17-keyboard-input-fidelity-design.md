# Design: GNU-Compatible Keyboard Input Fidelity and Winit Testing

**Date**: 2026-03-17
**Status**: Proposed

## Problem

Neomacs now reaches much farther into GNU-compatible startup and command
execution, but the live GUI input path is still weaker than the core Lisp
semantics.

The recurring symptom pattern is:

- focused Lisp regressions pass
- direct evaluator tests pass
- command-loop tests pass
- live GUI input through X11 or `xdotool` still behaves inconsistently for
  `M-x`, `ESC` prefixes, `Backspace`, and modifier-heavy chords

That means the remaining drift is not primarily in GNU Lisp command semantics.
It is in the boundary between:

- native window-system input
- Rust input normalization
- Emacs input events consumed by `read_char` / `read_key_sequence`

## GNU Emacs Ownership Boundary

The intended Neomacs ownership split remains:

- **GNU C input/runtime machinery -> Rust**
- **GNU Lisp command/keymap semantics -> upstream Lisp**

For keyboard behavior, GNU Emacs does not let the frontend invent command
semantics. Native backends produce low-level input events, and GNU command
reading/keymaps own the rest.

Relevant GNU source files:

- [`src/keyboard.c`](/home/exec/Projects/github.com/emacs-mirror/emacs/src/keyboard.c)
- [`src/xterm.c`](/home/exec/Projects/github.com/emacs-mirror/emacs/src/xterm.c)
- [`lisp/simple.el`](/home/exec/Projects/github.com/emacs-mirror/emacs/lisp/simple.el)
- [`lisp/subr.el`](/home/exec/Projects/github.com/emacs-mirror/emacs/lisp/subr.el)

The architectural rule for Neomacs is:

- Rust should replace GNU C input event generation and queueing
- Rust should not replace GNU Lisp keymaps, translation maps, minibuffer input
  semantics, or command dispatch

## Current Neomacs Risk

The input path is currently split across multiple layers:

- [`neomacs-display-runtime/src/render_thread/window_events.rs`](/home/exec/Projects/github.com/eval-exec/neomacs-windows/neomacs-display-runtime/src/render_thread/window_events.rs)
- [`neomacs-display-runtime/src/render_thread/input.rs`](/home/exec/Projects/github.com/eval-exec/neomacs-windows/neomacs-display-runtime/src/render_thread/input.rs)
- [`neomacs-bin/src/input_bridge.rs`](/home/exec/Projects/github.com/eval-exec/neomacs-windows/neomacs-bin/src/input_bridge.rs)
- [`neovm-core/src/keyboard.rs`](/home/exec/Projects/github.com/eval-exec/neomacs-windows/neovm-core/src/keyboard.rs)
- [`neovm-core/src/emacs_core/reader.rs`](/home/exec/Projects/github.com/eval-exec/neomacs-windows/neovm-core/src/emacs_core/reader.rs)
- [`neovm-core/src/emacs_core/eval.rs`](/home/exec/Projects/github.com/eval-exec/neomacs-windows/neovm-core/src/emacs_core/eval.rs)

The risk is early flattening:

- native input becomes an ad hoc keysym/modifier shape too early
- printable text and command keys are not clearly separated
- there are multiple event representations before command reading
- `xdotool` X11 quirks can be mistaken for core semantic bugs

## Design Goals

### Goal 1: One canonical Emacs-facing input normalization layer

Neomacs should have one Rust layer whose job is equivalent to GNU
`keyboard.c`/`xterm.c` input shaping:

- receive native frontend events
- normalize them into GNU-compatible Emacs input events
- feed those events into the command loop

The command loop should continue to own:

- `read_char`
- `read_key_sequence`
- prefix handling
- translation maps
- keymap lookup
- command dispatch

### Goal 2: Treat printable text differently from command keys

For normal text entry, committed text should be authoritative whenever command
modifiers are not active.

That means:

- `Shift+a` should become `A`, not `S-a`
- `Shift+1` should become `!`, not `S-1`
- IME commit should become text insertion, not synthetic command keys

When command modifiers are active, the key event should stay in command form:

- `M-x`
- `C-x C-c`
- `C-x 2`
- `C-M-f`

### Goal 3: Keep backend quirks separate from semantic correctness

X11 automation through `xdotool` is useful, but it is not the semantic oracle.
It is only one backend-specific injection path.

The semantic oracle remains:

- GNU source behavior
- GNU Lisp command-loop behavior
- focused Neomacs core regressions that model GNU-compatible events

## Target Architecture

### Layer 1: Native frontend event collection

Frontend code should capture rich native event data from `winit`, including:

- logical key
- physical key
- committed text
- modifiers
- pressed / released / repeat
- IME preedit / commit state

This layer should not decide Emacs semantics.

### Layer 2: Canonical Rust input normalization

Add one canonical normalization boundary in `neovm-core`.

Conceptually:

```text
winit native event
  -> canonical Neomacs native input event
  -> GNU-compatible Emacs input event
  -> command loop
```

This layer should own:

- Meta / Control / Super / Hyper bit shaping
- printable text vs command-key separation
- `Backspace` to GNU `DEL` behavior
- `ESC` prefix behavior
- repeat handling
- IME commit handling

This layer should not own:

- command bindings
- minibuffer command semantics
- keymap policy

### Layer 3: GNU-style command reading

The existing command-loop path should remain the semantic owner for:

- key sequence reading
- minibuffer reads
- translation maps
- prefix commands
- `this-command-keys` / `recent-keys`

That keeps GNU Lisp and GNU-style runtime semantics in the right layer.

## Testing Strategy

Other `winit`-based Rust projects do not rely only on OS-level GUI automation.
The practical pattern is layered testing.

### 1. Pure normalization tests

These should be the main semantic tests.

Feed handcrafted native input events into the canonical normalization function
and assert the resulting Emacs event sequence.

Required cases:

- `a`
- `A`
- `!`
- `Backspace`
- `ESC x`
- `M-x`
- `C-x C-c`
- `C-x 2`
- `C-x 3`
- IME commit text

These tests should live close to:

- [`neovm-core/src/keyboard.rs`](/home/exec/Projects/github.com/eval-exec/neomacs-windows/neovm-core/src/keyboard.rs)
- [`neovm-core/src/emacs_core/keyboard/`](/home/exec/Projects/github.com/eval-exec/neomacs-windows/neovm-core/src/emacs_core/keyboard)

### 2. Event-loop harness tests

Use a pumpable/testable event loop or a small internal harness so Neomacs can
run a real window/input loop in tests without depending on external X11 tools.

The goal is to test:

- focus changes
- modifier transitions
- text delivery
- basic command-loop event ingestion

without the flakiness of shell-driven GUI automation.

### 3. Black-box backend smoke tests

Keep a small number of X11 tests using:

- `xdotool --clearmodifiers`
- screenshot capture
- focused debug logs

These should verify backend-specific behavior only:

- X11 focus
- Alt/Meta interaction on X11
- modifier leakage
- real window visibility

They should not be the primary semantic test surface.

## Recommended External Tooling Direction

### Winit harness direction

Prefer a harness based on pumpable event-loop control rather than a forever-run
GUI main loop. Relevant references:

- `winit` `EventLoopExtPumpEvents`
- `winit-test`

These point toward the right shape for deterministic event-loop tests.

### Accessibility / higher-level UI testing

Longer term, a higher-level UI harness would be stronger than raw key
injection. The `egui` ecosystem shows one viable direction through
accessibility-tree testing and snapshot verification.

Neomacs does not need this immediately, but it is the best long-term direction
for robust GUI behavioral tests.

## Logging Guidance

Input debugging should stay available and intentional.

We should keep useful debug logging around:

- native `winit` key events
- modifier transitions
- committed text enqueueing
- canonical input normalization
- command-loop reads and minibuffer transitions

But high-volume rendering logs should stay at `trace` unless they are directly
needed for an active display bug.

Recommended policy:

- keyboard/input path logs: `debug`
- command-loop semantic transitions: `debug`
- per-glyph renderer spam: `trace`
- per-frame renderer summaries: `trace` unless actively debugging display

## Concrete Implementation Plan

### Phase 1: Canonicalize input normalization

1. Define one internal native-input structure with the fields Neomacs actually
   needs from `winit`.
2. Add one canonical conversion path from that native input to Emacs events.
3. Remove duplicate or ad hoc normalization logic where possible.

Success metric:

- all targeted input normalization tests pass without a GUI

### Phase 2: Add harnessed event-loop tests

1. Build a small test harness around the render/input thread.
2. Inject synthetic frontend events without using X11 shell automation.
3. Assert resulting Emacs-visible behavior.

Success metric:

- `M-x`, `ESC x`, `Backspace`, `C-x C-c`, `C-x 2`, and `C-x 3` have stable
  harness tests

### Phase 3: Keep a minimal backend smoke suite

1. Keep only a few X11 smoke tests.
2. Use them for backend quirks, not semantic truth.
3. Pair them with screenshot capture and focused log filters.

Success metric:

- backend smoke tests catch real X11 regressions without driving design

## What We Should Not Do

- We should not let `xdotool` become the semantic oracle for keyboard
  correctness.
- We should not keep inventing more ad hoc keysym conversion layers.
- We should not reimplement GNU Lisp command behavior in Rust just to make GUI
  input tests pass.
- We should not collapse printable text and command-key events into one vague
  representation.

## Immediate Next Slice

1. Add a dedicated canonical input normalization API in `neovm-core`.
2. Add focused tests for `M-x`, `ESC x`, `Backspace`, `C-x C-c`, `C-x 2`, and
   `C-x 3`.
3. Build a small `winit`-style input harness for Neomacs.
4. Reduce reliance on raw `xdotool` runs to a small X11 smoke suite.

## References

- `winit` event-loop pumping docs:
  https://docs.rs/winit/latest/winit/platform/pump_events/trait.EventLoopExtPumpEvents.html
- `winit-test`:
  https://docs.rs/winit-test/latest/winit_test/
- `winit` keyboard model docs:
  https://docs.rs/winit/latest/winit/keyboard/enum.Key.html
- `winit` keyboard model discussion:
  https://github.com/rust-windowing/winit/issues/753
- `egui_kittest`:
  https://docs.rs/egui_kittest/latest/egui_kittest/
- `kittest`:
  https://docs.rs/kittest/latest/kittest/
