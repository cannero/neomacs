# Phase 8 Plan: Hook Runtime Refactor

**Date**: 2026-03-30

## Goal

Make Neomacs hook semantics and ownership converge on GNU Emacs.

The target is GNU's split:

- hook storage stays in ordinary Lisp variables
- one generic hook runner owns iteration and stopping semantics
- caller subsystems set the right current buffer/window/frame before running hooks

Neomacs should not keep a second parallel hook framework with different rules.

## GNU Source Ownership

Primary GNU source files:

- `src/eval.c`
- `src/window.c`
- `src/insdel.c`
- `src/intervals.c`
- `lisp/subr.el`

Important GNU ownership facts:

- `add-hook` and `remove-hook` are Lisp-owned in `subr.el`
- generic hook dispatch lives in `eval.c`
- `run-hook-wrapped` stops on the first non-`nil` wrapper result
- buffer/window/edit owners prepare execution context before calling the generic runner
- modification-hook recursion suppression is owned by the edit core, not the generic hook runner

## Current Neomacs Ownership

Current hook behavior is split across too many owners:

- `neovm-core/src/emacs_core/builtins/hooks.rs`
- `neovm-core/src/emacs_core/bytecode/vm.rs`
- `neovm-core/src/emacs_core/editfns.rs`
- `neovm-core/src/emacs_core/navigation.rs`
- `neovm-core/src/hooks.rs`

This is not GNU-shaped enough.

## Current Audit Result

### Good

- Lisp-facing hook variables already use GNU-style value shapes: `nil`, one
  function, or a list of functions.
- Local hook lists already understand the `t` marker meaning "also run the
  global/default hook value".
- The command system already exposes the main GNU hook entry points.

### Bad

- Generic hook dispatch is duplicated between the evaluator builtin path and the
  bytecode VM path.
- `run-hook-wrapped` still has the wrong stopping semantics.
- Hook callers still bypass one another with ad hoc hook-value lookup helpers.
- Change hooks still do not have GNU `insdel.c` recursion-suppression semantics.
- Window hooks still do not set up GNU-like caller context.
- `neovm-core/src/hooks.rs` is still a parallel non-Lisp hook system with
  runtime concepts that do not match GNU's owner model.

## Deep Refactor Architecture

### Design rule

Neomacs should follow GNU's owner model:

- generic hook iteration lives in one runtime owner
- buffer/window/edit modules remain responsible for context setup
- hook storage remains ordinary Lisp variable state
- there is no competing Rust-native hook manager for Lisp hook semantics

### Target runtime structures

#### 1. One generic hook runtime owner

Introduce one hook runtime module that owns:

- hook-variable value resolution
- local/global hook-function collection
- stopping semantics for:
  - `run-hooks`
  - `run-hook-with-args`
  - `run-hook-with-args-until-success`
  - `run-hook-with-args-until-failure`
  - `run-hook-wrapped`
  - `run-hook-query-error-with-timeout`

The evaluator and VM should both delegate to that owner.

#### 2. One shared runtime variable-resolution path

Hook dispatch should stop using bespoke "dynamic buffer or global" helpers.
It should resolve hook-variable values through the same runtime owner that
already defines visible variable state.

That means:

- alias resolution once
- current-buffer local values when applicable
- default/global obarray values otherwise
- no independent hook-only lookup rules

#### 3. Caller-owned context

After the generic owner exists, callers should converge on GNU:

- `window.c`-like callers select the right frame/window/buffer before running hooks
- `insdel.c`-like callers handle `inhibit-modification-hooks`,
  `first-change-hook`, and error cleanup
- interval/text-property motion code handles point-entered/point-left ownership

The generic hook runner should not grow window/edit special cases.

## Migration Slices

### Slice A: Generic hook owner

Move generic hook iteration into a new `hook_runtime.rs` module and make:

- `builtins/hooks.rs`
- `bytecode/vm.rs`
- generic edit-hook call sites

delegate to it.

This slice should also fix `run-hook-wrapped` and remove duplicated evaluator
vs VM hook semantics.

### Slice B: Window hook callers

Make window hook callers GNU-shaped:

- `run-window-configuration-change-hook`
- `run-window-scroll-functions`

This requires caller-side buffer/window/frame setup, not more generic-hook
branching.

### Slice C: Modification hooks

Make modification hooks match `insdel.c`:

- `inhibit-modification-hooks` rebound during hook execution
- `first-change-hook`
- char-based OLD-LEN semantics
- reset-to-nil on unhandled hook errors where GNU does

### Slice D: Point-motion hooks

Make interval/property motion hook callers match GNU interval ownership and
deduplication semantics.

### Slice E: Remove parallel Rust hook framework

Either delete `neovm-core/src/hooks.rs` or quarantine it as non-Lisp
infrastructure only. It should not represent a second Lisp hook architecture.

## This Turn

This turn should land Slice A:

- add the shared hook runtime owner
- route evaluator and VM hook builtins through it
- route generic edit/window hook callers through it where that does not require
  new caller-context work
- add tests for `run-hook-wrapped` stop/return semantics
