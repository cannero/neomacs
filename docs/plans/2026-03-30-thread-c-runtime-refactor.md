# GNU `thread.c` Runtime Refactor

**Date**: 2026-03-30
**Goal**: make neomacs thread design follow GNU Emacs `src/thread.c` as
closely as practical, instead of keeping the current synchronous thread shim.

## Target Shape

GNU thread ownership is:

- one global Lisp lock
- real host threads
- per-thread eval state
- runtime-owned blocker / signal / join state
- buffer-kill integration through per-thread `buffer_disposition`

Neomacs should move to the same shape in phases.

## Phase 1

Move obvious runtime ownership onto thread state without claiming full GNU
concurrency yet.

- add per-thread execution snapshot fields:
  - current buffer
  - blocker object
  - pending/terminal signal fields
- route `thread--blocker` through thread runtime state
- make `make-thread` inherit a per-thread current buffer
- restore the caller thread's current buffer after synchronous worker execution

This phase is scaffolding. It makes the runtime more GNU-shaped even while
`make-thread` is still synchronous.

## Phase 2

Split `Context` state into shared runtime state versus per-thread eval state.

The per-thread side should eventually own:

- `specpdl`
- current buffer
- match data
- pending signal
- blocker state
- eval depth / VM activation record ownership that is truly thread-local

The shared runtime should keep:

- heap
- obarray
- buffers
- windows / frames
- processes
- timers

## Phase 3

Replace synchronous `make-thread` with real host thread creation plus a global
Lisp lock.

This is the first phase where neomacs can start matching GNU semantics for:

- `thread-live-p`
- `all-threads`
- `thread-yield`
- `thread-join`

## Phase 4

Implement real blocked-thread state and wakeup paths.

This must cover:

- `thread-join`
- `mutex-lock`
- `condition-wait`
- `thread--blocker`
- `thread-signal` wakeup behavior

## Phase 5

Integrate thread state with buffer kill behavior.

This is where GNU semantics for:

- `thread-buffer-disposition`
- `thread-buffer-killed`
- `'silently`

must become real runtime behavior, not just accessors.

## Acceptance Criteria

This refactor should not be considered done until neomacs can pass the GNU
thread tests with the same runtime model, not with accessor lies.

The key oracle file is:

- `/home/exec/Projects/github.com/emacs-mirror/emacs/test/src/thread-tests.el`
