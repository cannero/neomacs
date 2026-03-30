# Process / Timer / Event Loop Audit

**Date**: 2026-03-30
**Status**: Neomacs now has a shared wait/service path and a real `callproc`
owner, but Phase 9 still has GNU-compatibility risk in exact ordering and
callback semantics across timers, process output, and input waits.

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

### `accept-process-output` and `sleep-for` now share the wait path, but ordering still needs audit

GNU runs timers from the shared wait path unless `just_wait_proc < 0`, i.e.
unless the caller used integer `JUST-THIS-ONE`. See
[process.c](/home/exec/Projects/github.com/emacs-mirror/emacs/src/process.c#L5475),
[keyboard.c](/home/exec/Projects/github.com/emacs-mirror/emacs/src/keyboard.c#L4718),
and [atimer.c](/home/exec/Projects/github.com/emacs-mirror/emacs/src/atimer.c#L453).

Neomacs no longer has the earlier split where `accept-process-output` and
`sleep-for` each owned a separate polling loop. Both now route through the
shared wait/service path in
[process.rs](/home/exec/Projects/github.com/eval-exec/neomacs/neovm-core/src/emacs_core/process.rs#L1120)
and
[timer.rs](/home/exec/Projects/github.com/eval-exec/neomacs/neovm-core/src/emacs_core/timer.rs#L611).
That closed the earlier starvation bug and the older `PROCESS` /
`JUST-THIS-ONE` mismatch.

The remaining GNU risk is narrower now: exact ordering when GNU Lisp timers,
Rust timers, process filters, sentinels, and input wakeups are all due in the
same wait cycle is not yet locked down by differential coverage.

### Process callbacks now share one async callback envelope, but it is still a translated design

GNU process filters and sentinels preserve important dynamic state when they
run:

- current buffer is restored afterwards
- match data is preserved/restored
- `waiting_for_user_input_p` is restored
- quit is inhibited while the callback runs
- `last_nonmenu_event` is rebound
- `deactivate-mark` is restored afterwards

See
[process.c](/home/exec/Projects/github.com/emacs-mirror/emacs/src/process.c#L6518)
and [process.c](/home/exec/Projects/github.com/emacs-mirror/emacs/src/process.c#L7789).

Neomacs now routes process filters, sentinels, and client network `"open\n"`
sentinel delivery through one shared helper in
[process.rs](/home/exec/Projects/github.com/eval-exec/neomacs/neovm-core/src/emacs_core/process.rs#L950).
That helper now preserves current buffer, match data, `waiting-for-user-input-p`,
`inhibit-quit`, `last-nonmenu-event`, and `deactivate-mark` across both
`accept-process-output` and other process callback entry points.

So the earlier “plain `eval.apply` on one path, protected callback on another”
mismatch is closed. The remaining difference from GNU is not split behavior;
it is that Neomacs still expresses this as a Rust-side translated helper rather
than GNU’s exact `read_process_output_call` / `internal_condition_case_1` /
`record_unwind_current_buffer` control flow.

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

### Synchronous subprocess ownership is now mostly `callproc.c`-shaped

GNU keeps synchronous subprocess invocation in `src/callproc.c`, separate from
`process.c` but still part of the same process/timer/input boundary. It also
honors `DISPLAY` by redisplaying while output is inserted. See
[callproc.c](/home/exec/Projects/github.com/emacs-mirror/emacs/src/callproc.c#L272)
and
[callproc.c](/home/exec/Projects/github.com/emacs-mirror/emacs/src/callproc.c#L510).

Neomacs now has a real synchronous subprocess owner in:

- [callproc/mod.rs](/home/exec/Projects/github.com/eval-exec/neomacs/neovm-core/src/emacs_core/callproc/mod.rs#L1)

`call-process`, `process-file`, `process-lines`, `call-process-region`, and the
shell-command variants now execute through that module, while
[process.rs](/home/exec/Projects/github.com/eval-exec/neomacs/neovm-core/src/emacs_core/process.rs#L4002)
has been reduced to builtin delegation plus asynchronous-process ownership.
GNU-style `DISPLAY` redisplay is now also requested after synchronous output is
routed into a buffer destination.

The remaining gap is narrower than before: `callproc` still reuses some generic
string/region/process-I/O helpers from `process.rs`, so the file split is not
yet as self-contained as GNU `callproc.c` versus `process.c`.

## Coverage Gaps

Current neomacs coverage in `process_test.rs` is mostly argument contract and
rooting coverage. It does not yet lock down the high-risk shared-wait-path
semantics, especially:

- timer ordering when GNU Lisp timers and Rust timers are simultaneously due
- exact ordering between process filters, sentinels, and timers in one wait cycle
- `sleep-for` / `sit-for` parity once input and redisplay are involved
- chunked synchronous subprocess insertion and redisplay fidelity versus GNU
  `callproc.c`

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

1. timer ordering when both GNU Lisp timers and Rust timers are due
2. filter/sentinel ordering relative to timer firing in one wait cycle
3. `sleep-for` / `sit-for` parity once input and redisplay are involved
4. chunked synchronous subprocess insertion/redisplay fidelity versus GNU
   `callproc.c`
5. focused differential coverage for the shared wait-path cases listed above

## Conclusion

The highest-priority Phase 9 problem is no longer ownership. Neomacs now has a
shared wait path and a real `callproc` boundary. The remaining work is exact
GNU ordering and callback behavior within that shared runtime.
