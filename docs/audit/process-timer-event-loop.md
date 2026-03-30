# Process / Timer / Event Loop Audit

**Date**: 2026-03-30
**Status**: GNU ownership is unified around one wait path. Neomacs still
splits process/timer/event-loop semantics across `process.rs`, `keyboard.rs`,
and `timer.rs`, and the split currently causes confirmed GNU mismatches.

## GNU Emacs Design

Primary GNU ownership:

- `src/process.c`
- `src/keyboard.c`
- `src/atimer.c`
- `src/callproc.c`
- `src/timefns.c`

The important design point is that GNU does not treat subprocess I/O, timers,
and input waiting as separate utility layers.

- `accept-process-output` in `process.c` delegates to one runtime owner:
  `wait_reading_process_output`.
- `wait_reading_process_output` runs timer checks, waits for process/network
  state, notices input, and services filters/sentinels.
- Lisp timers are owned by the keyboard/input side through `timer_check`,
  `detect_input_pending_run_timers`, and `do_pending_atimers`.
- Process filters and sentinels run under a shared async-callback envelope that
  preserves current buffer, match data, and `waiting_for_user_input_p`, and
  inhibits quit while the callback runs.

Relevant GNU references:

- [process.c](/home/exec/Projects/github.com/emacs-mirror/emacs/src/process.c#L4865)
- [process.c](/home/exec/Projects/github.com/emacs-mirror/emacs/src/process.c#L5332)
- [process.c](/home/exec/Projects/github.com/emacs-mirror/emacs/src/process.c#L6518)
- [process.c](/home/exec/Projects/github.com/emacs-mirror/emacs/src/process.c#L7789)
- [keyboard.c](/home/exec/Projects/github.com/emacs-mirror/emacs/src/keyboard.c#L4718)
- [keyboard.c](/home/exec/Projects/github.com/emacs-mirror/emacs/src/keyboard.c#L12009)
- [atimer.c](/home/exec/Projects/github.com/emacs-mirror/emacs/src/atimer.c#L451)

## Neomacs Current Design

The equivalent Neomacs ownership is currently split:

- `accept-process-output` has a local polling loop in
  [process.rs](/home/exec/Projects/github.com/eval-exec/neomacs/neovm-core/src/emacs_core/process.rs#L4893)
- keyboard/input waiting runs its own process and timer servicing path in
  [keyboard.rs](/home/exec/Projects/github.com/eval-exec/neomacs/neovm-core/src/keyboard.rs#L3123)
  and
  [keyboard.rs](/home/exec/Projects/github.com/eval-exec/neomacs/neovm-core/src/keyboard.rs#L3195)
- Rust timers are managed separately in
  [timer.rs](/home/exec/Projects/github.com/eval-exec/neomacs/neovm-core/src/emacs_core/timer.rs#L20)
  and manually serviced in
  [timer.rs](/home/exec/Projects/github.com/eval-exec/neomacs/neovm-core/src/emacs_core/timer.rs#L613)

That split is already more than an architectural smell. It causes concrete
semantic mismatches.

## Confirmed Findings

### `accept-process-output` is not a shared event-loop entry point

GNU `accept-process-output` does not own a private subprocess wait loop. It
delegates to `wait_reading_process_output`, which also runs timers and sees
pending input.

Neomacs instead implements a separate local polling loop in
[process.rs](/home/exec/Projects/github.com/eval-exec/neomacs/neovm-core/src/emacs_core/process.rs#L4893).
That means:

- timer servicing is bypassed entirely from this path
- callback timing differs from keyboard/input waits
- process/timer/input semantics are not owned by one runtime path

This is the main architectural mismatch for the whole subsystem.

### `accept-process-output PROCESS` wrongly suspends all other processes

GNU only suspends other processes when `JUST-THIS-ONE` is non-`nil`, and only
suppresses timers when `JUST-THIS-ONE` is an integer, per
[process.c](/home/exec/Projects/github.com/emacs-mirror/emacs/src/process.c#L4878).

Neomacs computes `proc_ids` from `PROCESS` alone:

- if a target process is provided, it polls only that process
- the fourth `JUST-THIS-ONE` argument is ignored semantically

See
[process.rs](/home/exec/Projects/github.com/eval-exec/neomacs/neovm-core/src/emacs_core/process.rs#L4970)
and
[process.rs](/home/exec/Projects/github.com/eval-exec/neomacs/neovm-core/src/emacs_core/process.rs#L4980).

That is a real GNU incompatibility: `accept-process-output` with a target
process should still allow unrelated process filters/sentinels to run unless
the caller explicitly asked to suspend them.

### `accept-process-output` never services timers

GNU runs timers from the shared wait path unless `just_wait_proc < 0`, i.e.
unless the caller used integer `JUST-THIS-ONE`. See
[process.c](/home/exec/Projects/github.com/emacs-mirror/emacs/src/process.c#L5475),
[keyboard.c](/home/exec/Projects/github.com/emacs-mirror/emacs/src/keyboard.c#L4718),
and [atimer.c](/home/exec/Projects/github.com/emacs-mirror/emacs/src/atimer.c#L453).

Neomacs `accept-process-output` does not call any timer service path at all.
The only timer servicing around waits is currently in the keyboard path and the
ad hoc `sleep-for` loop:

- [keyboard.rs](/home/exec/Projects/github.com/eval-exec/neomacs/neovm-core/src/keyboard.rs#L3143)
- [timer.rs](/home/exec/Projects/github.com/eval-exec/neomacs/neovm-core/src/emacs_core/timer.rs#L613)

This means timer callbacks can starve during `accept-process-output`, which is
not GNU behavior.

### `accept-process-output` callbacks bypass the GNU async callback envelope

GNU process filters and sentinels preserve important dynamic state when they
run:

- current buffer is restored afterwards
- match data is preserved/restored
- `waiting_for_user_input_p` is restored
- quit is inhibited while the callback runs

See
[process.c](/home/exec/Projects/github.com/emacs-mirror/emacs/src/process.c#L6518)
and [process.c](/home/exec/Projects/github.com/emacs-mirror/emacs/src/process.c#L7789).

Neomacs keyboard-driven process callbacks already partially mirror this by
saving/restoring match data and current buffer in
[keyboard.rs](/home/exec/Projects/github.com/eval-exec/neomacs/neovm-core/src/keyboard.rs#L3219).
But the `accept-process-output` path directly accumulates callbacks and later
invokes them through plain `eval.apply` in
[process.rs](/home/exec/Projects/github.com/eval-exec/neomacs/neovm-core/src/emacs_core/process.rs#L5867),
with only temporary rooting support.

So the same callback can observe different dynamic behavior depending on
whether it was reached through keyboard waiting or through
`accept-process-output`. GNU has one behavior here, not two.

### Timer ownership is still split between GNU-shaped Lisp timers and a Rust timer manager

GNU’s ordinary and idle timers are part of the keyboard/event-loop contract.
Neomacs currently has two timer worlds:

- GNU Lisp timer vectors handled by `timer-event-handler` in
  [keyboard.rs](/home/exec/Projects/github.com/eval-exec/neomacs/neovm-core/src/keyboard.rs#L3143)
- Rust `TimerManager` entries in
  [timer.rs](/home/exec/Projects/github.com/eval-exec/neomacs/neovm-core/src/emacs_core/timer.rs#L20)

This split may be acceptable as a migration step, but it is not yet GNU-like
ownership. Event-loop semantics are only GNU-compatible if both timer surfaces
are serviced from the same wait path with the same ordering rules.

## Refactor Direction

The GNU-shaped direction is:

1. Introduce one `Context`-owned wait/service entry point for:
   - process output
   - process status transitions
   - sentinels
   - filters
   - GNU timers
   - Rust timers during migration
   - input wakeups / timeout calculation
2. Make `accept-process-output`, `sleep-for`, and keyboard wait paths call that
   same entry point.
3. Treat the `PROCESS` and `JUST-THIS-ONE` arguments as wait policy only, not
   as permission to create a separate semantic runtime.
4. Move callback execution behind one async envelope that restores:
   - current buffer
   - match data
   - waiting/input state
   - quit/debug policy as needed
5. Keep `neovm-core` as the semantic owner. Pollers, worker/runtime tasks, and
   frontend wakeups should remain transport mechanisms only.

## What To Audit Next Inside Phase 9

The next concrete source-level audit order should be:

1. `accept-process-output` policy and callback timing
2. filter/sentinel dynamic-state preservation
3. `sleep-for` / `sit-for` / input wait integration
4. `call-process` / `process-file` interaction with the same wait path
5. timer ordering when both GNU Lisp timers and Rust timers are due

## Conclusion

The highest-priority Phase 9 problem is no longer “threads”. It is that
Neomacs still lacks GNU’s single semantic wait path for processes, timers, and
input. Until that is unified, individual APIs can look plausible while still
firing callbacks in the wrong order or under the wrong dynamic state.
