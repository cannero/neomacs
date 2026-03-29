# Phase 8 Plan: Keymap and Key Input Refactor

**Date**: 2026-03-29

## Goal

Make Neomacs keymap and key-input semantics source-level equivalent to GNU
Emacs.

This does **not** require copying GNU's platform event plumbing.  It **does**
require that from `read_char` upward, Neomacs has the same semantic ownership
shape GNU Emacs has:

- `src/keyboard.c` owns key/event normalization, input queues, recent-key
  history, macro playback, translation maps, `read_char`, and
  `read_key_sequence`.
- `src/keymap.c` owns keymap normalization, active-map construction, key lookup,
  command remapping, prefix handling, and keymap parent/autoload semantics.
- GNU Lisp keeps Lisp-owned helpers in `lisp/keymap.el`, `lisp/subr.el`, and
  nearby files.

Neomacs can keep a different GUI/runtime frontend, but the semantic boundary
must converge on GNU.

## GNU Source Ownership

Primary GNU source files:

- `src/keyboard.c`
- `src/keymap.c`
- `src/callint.c`
- `src/macros.c`
- `lisp/keymap.el`
- `lisp/subr.el`

Important GNU ownership facts:

- `active_maps` and `current-active-maps` decide map precedence, not ad hoc
  callers.
- `key-binding` is a thin wrapper over `current-active-maps` plus
  `lookup-key`/`command-remapping`.
- `read_key_sequence` owns replay, translation-map application, raw vs cooked
  key tracking, mouse-prefix handling, and command remapping.
- `event-modifiers`, `event-basic-type`, and other higher-level helpers stay
  Lisp-owned.

## Current Neomacs Ownership

Current semantic ownership is split across:

- `neovm-core/src/emacs_core/keymap.rs`
- `neovm-core/src/emacs_core/builtins/keymaps.rs`
- `neovm-core/src/emacs_core/interactive.rs`
- `neovm-core/src/emacs_core/kbd.rs`
- `neovm-core/src/keyboard.rs`
- `neomacs-bin/src/input_bridge.rs`
- `neomacs-display-runtime/src/render_thread/input.rs`

This is not GNU-shaped enough yet.

## Current Audit Result

### Good

- Neomacs already uses GNU-style Lisp keymap objects.
- Minor-mode precedence is intentionally close to GNU.
- Translation-map order is intentionally close to GNU.
- Some higher-level event helpers are still Lisp-owned, which is the right
  direction.

### Bad

- `current-active-maps` is still too thin and ignores important GNU inputs:
  `overriding-local-map`, `overriding-terminal-local-map`, and position-based
  `local-map` / `keymap` properties.
- `key-binding` duplicates active-map logic instead of delegating to one
  authoritative active-map owner.
- Command remapping still follows a thinner local/global/minor lookup path than
  GNU.
- Event representation and key parsing are duplicated across multiple Rust
  modules.
- `read_key_sequence` is evaluator-owned glue instead of keyboard-subsystem
  ownership.
- Keymap autoload and parent-cycle behavior are not yet GNU-like.

## Ideal Long-Term Design

### Semantic boundary

- `neomacs-display-runtime` and `neomacs-bin` deliver raw platform facts:
  keycode, text, modifiers, pointer/window payloads, IME payloads.
- `neovm-core` owns the Emacs event model and all Lisp-visible keyboard
  semantics.

### `keyboard` ownership

The keyboard subsystem in `neovm-core` should own:

- raw input queue and unread-command-events integration
- keyboard macro playback and recording state
- recent-key history / lossage / command-key buffers
- translation-map application and replay bookkeeping
- `read_char`
- `read_key_sequence`
- current command key surfaces:
  `this-command-keys`, `this-command-keys-vector`,
  `this-single-command-keys`, `recent-keys`

### `keymap` ownership

The keymap subsystem in `neovm-core` should own:

- `get_keymap`
- `access_keymap`
- `lookup-key`
- `current_minor_maps`
- `current-active-maps`
- `key-binding`
- `minor-mode-key-binding`
- `where-is-internal`
- command remapping over active maps
- keymap parent mutation and cycle checks
- keymap autoload behavior where GNU does it in C

### Lisp ownership

GNU Lisp-owned helpers should stay loaded from GNU Lisp:

- `lisp/keymap.el`
- `lisp/subr.el`
- help/describe-key layers
- keymap convenience APIs/macros

If GNU owns it in Lisp, Neomacs should not permanently re-own it in Rust.

## Refactor Plan

## Progress

Completed on 2026-03-29:

- Slice 1 owner is now substantially better: `current-active-maps`,
  `key-binding`, and command remapping share one active-map constructor, honor
  overriding maps, and honor point-sensitive `local-map` / `keymap` text
  properties.
- Key description parsing/formatting no longer keeps an independent parser in
  `src/keyboard.rs`; it now routes through the GNU-facing `kbd` / Emacs-event
  path.
- Command-loop unread-event and keyboard-macro playback state now store
  Lisp-visible Emacs event values instead of frontend `KeyEvent` structs, which
  is closer to GNU `keyboard.c` ownership and removes repeated re-normalization
  in `read_char`.
- Frontend transport bitmasks and X keysym normalization now have a single
  owner in `neovm-core/src/keyboard.rs`; `neomacs-bin` and the TTY frontend
  forward transport facts instead of keeping their own duplicate key transport
  vocabulary.
- `read_char` and `read_key_sequence` implementation ownership has moved out of
  `emacs_core/eval.rs` and into `src/keyboard.rs`, so keyboard sequencing now
  lives with the command-loop owner instead of evaluator glue.
- Pending host input and GNU idle-epoch state now live on `CommandLoop`, and
  the mouse-event builders moved into `src/keyboard.rs`, so the keyboard owner
  no longer depends on evaluator-owned storage for those parts of command-loop
  state.
- Resize synchronization for `read_char` / `redisplay` now lives in
  `src/keyboard.rs` too, including pending resize draining and opening GUI
  frame host-size reconciliation.
- Timer timeout selection, GNU timer firing, and process-output polling for the
  command loop now live in `src/keyboard.rs` too, so the `read_char` wait path
  is no longer evaluator-owned.
- Command-key history is now keyboard-owned too: `recent-keys`,
  translated/raw command-key buffers, `set--this-command-keys`, and GNU-style
  event-array rendering for `this-command-keys` now go through the keyboard
  subsystem instead of ad hoc interactive-layer formatting.

Still open:

- Frontend event transport still passes through a separate Rust `KeyEvent`
  layer before entering the command loop, even though raw keysym/modifier
  normalization is now centralized.
- The moved `read_char` / `read_key_sequence` code still reaches through
  `Context` helper/state surfaces that are evaluator-shaped; the next cleanup is
  the remaining display-host reach-through and the keyboard-macro ownership
  split.
- Keymap autoload and parent-cycle semantics are still thinner than GNU.

### Slice 1: Active maps and `key-binding`

Make one shared active-map constructor and use it for:

- `current-active-maps`
- `key-binding`
- command remapping over active maps

Minimum GNU parity for this slice:

- honor `overriding-local-map`
- honor `overriding-terminal-local-map`
- honor point-sensitive `local-map` / `keymap` text properties
- honor explicit numeric and marker `POSITION`
- keep active-map order GNU-compatible

This is the highest-value first slice because multiple user-visible key lookup
surfaces depend on it.

### Slice 2: Event model unification

Collapse duplicated key/event parsing layers into one authoritative Emacs event
representation inside `neovm-core`.

That should absorb semantic duplication currently spread across:

- `src/keyboard.rs`
- `src/emacs_core/keymap.rs`
- `src/emacs_core/kbd.rs`

### Slice 3: Move key-sequence reading to keyboard ownership

Refactor `read_char` and `read_key_sequence` out of evaluator glue into the
keyboard subsystem.

This slice must move toward GNU handling of:

- translation maps and replay
- raw vs translated key history
- mouse-prefix and click-buffer behavior
- prefix-help / shift fallback behavior

### Slice 4: Keymap autoload, parent, and cache discipline

Make keymap ownership closer to GNU `get_keymap` and `set-keymap-parent`:

- keymap autoload where GNU autoloads
- parent-cycle protection
- fewer ad hoc symbol-to-keymap resolution paths

### Slice 5: Command-key history and macros

Unify:

- `recent-keys`
- `this-command-keys*`
- keyboard macro state
- unread/replayed key sequence bookkeeping

These should stop being split across evaluator, interactive, and keyboard
helpers.

## Testing Plan

Each slice should add GNU differential tests before expanding the scope.

### Slice 1 tests

- `current-active-maps` precedence:
  overriding maps, text-property maps, minor maps, local map, global map
- `key-binding` with integer and marker `POSITION`
- `key-binding` and `current-active-maps` at point with `local-map` / `keymap`
  text properties
- command remapping through the same active-map order

### Later slices

- translation-map replay and key-sequence reading
- recent-key history and `this-command-keys*`
- prefix handling and keymap parent/autoload behavior

## Exit Criteria

Phase 8 keymap/key-input work is not done until:

- `keyboard` and `keymap` ownership in `neovm-core` are GNU-shaped
- frontend/runtime only transports raw platform event data
- `current-active-maps`, `key-binding`, and command remapping all share one
  active-map owner
- `read_key_sequence` semantics are keyboard-owned, not evaluator-owned glue
- GNU differential tests cover active-map precedence, translation/replay,
  recent-key history, remapping, and prefix behavior
