# Backtrace / eval_sub GNU-parity architectural refactor

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Funnel every Lisp call through one `eval_sub` analog that pushes exactly one mutable backtrace frame per call, mirroring GNU eval.c:2585. Delete the ~8 opportunistic dispatch-site pushes (`push_backtrace_frame` / `push_backtrace_arg`) that currently approximate GNU behavior but leave coverage gaps every time a new dispatcher is added.

**Tech stack:** Rust (neovm-core crate).

**Context / reference implementation:** `/home/exec/Projects/github.com/emacs-mirror/emacs/src/eval.c` — specifically `eval_sub` (eval.c:2568-2760), `record_in_backtrace` + `set_backtrace_args` (eval.c:121-156, 2638, 2660, 3299), `Ffuncall` (eval.c:3155-3300), and `backtrace_frame_apply` (eval.c:3984-4000).

---

## Motivation

The surgical fix in `34036a7e8` closed the user-visible special-form gap (UNEVALLED frames now appear) but preserved three structural drifts from GNU:

1. **Coverage is opportunistic, not universal.** 8 separate `push_backtrace_frame` call sites (eval.rs:6532, 6544, 6613, 8959, 8999, 9014, 9406, 9427; bytecode/vm.rs:3443) each decide independently to push. Any new dispatcher added later silently omits the frame.
2. **Frames are immutable post-push.** GNU pushes once with UNEVALLED shape and calls `set_backtrace_args(count, argvals, numargs)` to mutate the SAME slot to EVALD after arg evaluation. Neomacs pushes the EVALD shape at dispatch time, producing the same observable stack but losing the phase-transition invariant.
3. **`Ffuncall` has no analog.** GNU's `Ffuncall` (eval.c:3171) pushes its own frame — that's what shows in bytecode `call` opcode traces and `apply`/`funcall` builtins. Neomacs scatters this across `funcall_general`, `apply_named_callable_by_id`, and the bytecode `call` opcode. Each must remember to push.

A single universal entry point would also simplify: the forthcoming advice integration, `debug_on_next_call`, `running-in-debugger` variable semantics, and any profiler hooks that need to observe call entry/exit.

---

## Phased approach

### Phase 1: Introduce the single entry point (additive, no deletions)

- [ ] Add `Context::eval_sub_with_frame(form: Value) -> EvalResult` that does today's `eval_sub` work **plus** pushes an UNEVALLED backtrace frame at entry and pops with `unbind_to(bt_count)` at exit. Model on GNU eval.c:2583-2586.
- [ ] Add `Context::funcall_with_frame(function, args) -> EvalResult` as the `Ffuncall` analog: pushes an EVALD frame at entry, pops at exit. Model on GNU eval.c:3169-3173.
- [ ] Add `Context::set_backtrace_args_evalled(count: usize, args: &[Value])` that mutates the topmost `SpecBinding::Backtrace` at `specpdl[count - 1]` in place: clears `unevalled`, replaces `args` vec with the evaluated slice. Model on GNU `set_backtrace_args` (eval.c:144-156).
- [ ] **Do not** remove existing pushes yet. New entry points coexist with old ones.

### Phase 2: Migrate `eval_sub_cons` to the universal pattern

- [ ] In `eval_sub_cons`, push the UNEVALLED frame **once** at the very top (before fast-path fastmatches, before function resolution).
- [ ] After arg evaluation (macro path, subr MANY path, lambda path), call `set_backtrace_args_evalled` to promote in place.
- [ ] Remove the now-redundant `push_backtrace_frame` calls at eval.rs:6532 (macro), 6544 (cons-cell macro), 6613 (inner apply). These become property of the outer frame.
- [ ] Remove the new `push_unevalled_backtrace_frame` call added in `34036a7e8` at eval.rs:6522 — the outer push subsumes it.
- [ ] Extend the probe test added in `34036a7e8` to observe both the UNEVALLED phase (during arg evaluation of a subr body) and the EVALD phase (during the body of a non-special-form function) from the SAME frame.

### Phase 3: Migrate `funcall_general` and related

- [ ] Move the `push_backtrace_frame` calls at eval.rs:8959, 8999, 9014 (the three `funcall_*` variants) to a single location inside `funcall_with_frame`. Change the existing callers to route through `funcall_with_frame` instead of calling `push_backtrace_frame` + dispatch.
- [ ] Do the same for eval.rs:9406, 9427 (`apply_named_callable_by_id_*`).
- [ ] After migration, all non-bytecode call paths push exactly one frame via `funcall_with_frame`, matching GNU Ffuncall.
- [ ] Audit `push_backtrace_arg` (eval.rs:8869) — in GNU, incremental arg-by-arg updates go through `set_backtrace_args` at the MANY dispatch (eval.c:2638). Decide: keep as-is (incremental visibility during stepping debuggers) or drop (GNU only updates after full arg eval).

### Phase 4: Migrate bytecode VM

- [ ] In `bytecode/vm.rs`, replace `self.ctx.push_backtrace_frame(func_val, &args)` at vm.rs:3443 with `self.ctx.funcall_with_frame(func_val, args)`. This centralizes the bytecode `call` opcode's backtrace emission.
- [ ] Verify that `condition-case`, `unwind-protect`, and `catch` handlers see the same frames they do today — these are tested in `bytecode/vm_test.rs::vm_condition_case_*` and `vm_compiled_unwind_protect_*`.

### Phase 5: Delete the legacy API

- [ ] Once Phases 2-4 leave `push_backtrace_frame` with zero callers outside `eval_sub_with_frame` and `funcall_with_frame`, make both `push_backtrace_frame` and `push_unevalled_backtrace_frame` private to the eval module (or inline them into the new entry points).
- [ ] Keep `push_unevalled_backtrace_frame` exposed only to the eval.rs internals; remove the `pub(crate)` visibility.
- [ ] Update the probe-based regression test in `misc_test.rs` to use only the public entry points.

### Phase 6: Validation

- [ ] Run the three pre-existing failing tests that predate this plan (`vm_byte_position_and_get_byte_use_shared_runtime_state`, `vm_compiled_load_signals_after_gnu_recursive_load_limit`, `vm_composition_and_compute_motion_builtins_use_direct_dispatch`) — this refactor must not regress them further.
- [ ] Run the full `cargo nextest run -p neovm-core` sweep, routing output to a file per memory `feedback_cargo_nextest_workflow`.
- [ ] Run `cargo xtask fresh-build` end-to-end and confirm no `.elc` regressions or pdump crashes.
- [ ] Manual parity check: load the same `.el` file in both neomacs and `src/emacs`, trigger a backtrace (via `(debug)` or uncaught signal), compare frame shapes frame-by-frame.

---

## Observable behaviors that MUST be preserved

- `backtrace-frame--internal` output for every existing test in `misc_test.rs` and `subr.el` callers.
- `condition-case` handler invocation ordering (specpdl unwinds must still pop in LIFO order).
- `unwind-protect` CLEANUP clause runs even when BODY signals — see `quit_regression_test.rs::unbind_to_suppresses_quit_during_unwind_protect_cleanup`.
- `mapbacktrace` frame ordering (newest first, per GNU eval.c:4031-4041).
- Bytecode `condition-case-debug` marker semantics (see `vm_test.rs::vm_condition_case_debug_marker_calls_debugger_before_handler`).

## Non-goals

- **No new language-visible features.** This is a refactor to match GNU's invariants, not a feature addition.
- **No changes to `SpecBinding` variants other than `Backtrace`.** `UnwindProtect`, `SaveExcursion`, etc. stay as-is.
- **No performance work.** If the refactor causes a measurable regression in hot eval paths, that's acceptable for this plan — optimize only if benchmarks show >5% slowdown in real workloads (`fresh-build`, `tui-tests`).
- **No changes to the `debug_on_next_call` / `Vdebug_on_signal` variables.** These stay wherever they live today.

## Risks

- **Frame ordering drift.** If any test asserts an exact backtrace sequence and the refactor shifts frame boundaries (e.g., one outer UNEVALLED frame instead of three scattered EVALD frames), tests will flag it. Fix at root (correct shape) not by adjusting assertions.
- **Bytecode perf.** `funcall_with_frame` on every bytecode `call` opcode adds one push + one unbind_to per call. GNU measures this cost as negligible; verify on the `vm_bytecode_while_polls_quit_flag` tight-loop case.
- **Advice / `around` wrappers.** GNU's Ffuncall pushes the frame before the advice layer; any advice-bypass optimizations in `81a116c3a` must still see consistent frames. Re-read that commit before touching `funcall_general`.

## References

- GNU Emacs source: `/home/exec/Projects/github.com/emacs-mirror/emacs/src/eval.c`
- GNU Emacs binary (31.0.50): `src/emacs` — use for live parity checks
- Surgical fix this plan extends: commit `34036a7e8` (eval/backtrace: record UNEVALLED frames for special-form dispatch)
- Related quit-architecture refactor: `bf201f48a` (quit: wire bytecode VM + unbind_to + cross-thread C-g signal), `81a116c3a` (quit/advice: regex matcher polling and inline-opcode advice bypass)
