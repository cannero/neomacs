# Phase 9 Audit: Process / Thread / Timer

**Date**: 2026-03-28

## GNU source ownership

Primary GNU source files:

- `src/process.c`
- `src/callproc.c`
- `src/thread.c`
- `src/emacs-module.c`
- `src/atimer.c`
- `src/timefns.c`

GNU integrates these with the command loop and event system rather than
treating them as isolated utility modules.

## Neomacs source ownership

VM/core side:

- `neovm-core/src/emacs_core/process.rs`
- `neovm-core/src/emacs_core/callproc/mod.rs`
- `neovm-core/src/emacs_core/network.rs`
- `neovm-core/src/emacs_core/threads.rs`
- `neovm-core/src/emacs_core/timer.rs`
- `neovm-core/src/emacs_core/timefns.rs`

Host/runtime side:

- `neovm-host-abi/`
- `neovm-worker/`
- runtime communications in `neomacs-display-runtime/src/thread_comm.rs`

## Audit result

Status is **under-audited and architecturally high-risk**.

Good:

- There is a real Rust process manager and timer layer.
- The code is concentrated enough to audit.

Bad:

- GNU couples process/timer behavior tightly to its event loop.
- Neomacs uses a more distributed runtime/worker/host architecture.
- `neovm-core/src/emacs_core/threads.rs` explicitly implements a simulated
  thread model where `make-thread` is an API shim rather than GNU-equal thread
  semantics.
- `neovm-core/src/emacs_core/timer.rs` currently owns a standalone
  `Instant`-based vector scheduler rather than a GNU-shaped timer/event-loop
  integration.
- `neovm-core/src/emacs_core/process.rs` uses a Rust `polling::Poller` and
  direct OS child/network management, while `neovm-host-abi` and
  `neovm-worker` add a separate task/affinity/runtime layer.
- That makes Lisp-visible ordering and state transitions a real source-level
  risk, even if individual APIs look plausible.

## Long-term ideal design

The ideal design is:

- `neovm-core` owns Lisp-visible process, timer, and thread semantics.
- Worker/runtime/host abstractions remain transport/execution mechanisms, not
  semantic owners.
- The event loop that Lisp sees must still behave like GNU even if the host
  implementation is more concurrent internally.
- If Neomacs later uses real multithreading internally, that concurrency should
  stay below the Lisp boundary unless and until a GNU-compatible Lisp contract
  is defined for it.

## Required work

- Audit process filters, sentinels, timer firing, and
  `accept-process-output` ordering against GNU.
- Re-study GNU `thread.c` before changing `threads.rs`; the current simulated
  implementation should be treated as a compatibility placeholder, not as the
  final design.
- Make host/runtime scheduling invisible at the Lisp boundary.
- Treat process/timer/thread behavior as one event-loop subsystem in the audit,
  not three separate utilities.

## Exit criteria

- Lisp-visible process/timer/thread behavior is VM-owned.
- Host/runtime scheduling does not change semantic ordering relative to GNU.
- Differential coverage exists for filters, sentinels, shell commands, timers,
  and thread-visible behavior.
