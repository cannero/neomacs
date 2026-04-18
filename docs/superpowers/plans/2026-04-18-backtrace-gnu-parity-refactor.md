# Backtrace / eval_sub GNU-parity architectural refactor

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make neomacs's backtrace model match GNU eval.c's invariant that **every `eval_sub` call pushes exactly one mutable backtrace frame**, phase-transitioned from UNEVALLED to EVALD via in-place mutation. Close the three specific drifts from GNU after commit `34036a7e8`.

**Tech stack:** Rust (neovm-core crate).

**Reference implementation:** `/home/exec/Projects/github.com/emacs-mirror/emacs/src/eval.c` — `eval_sub` (eval.c:2567-2760), `record_in_backtrace` + `set_backtrace_args` (eval.c:121-156, 2638, 2660, 3299), `Ffuncall` (eval.c:3152-3185), `backtrace_frame_apply` (eval.c:3984-4000), `apply_lambda` (eval.c:3281-3300).

---

## Correcting my earlier audit

An earlier draft of this plan claimed "8 scattered push sites, each deciding independently to push." That was inaccurate. Actual neomacs architecture:

| GNU site | Neomacs counterpart | Status |
|---|---|---|
| `Ffuncall` pushes one EVALD frame (eval.c:3171) | `funcall_general` pushes via `push_backtrace_frame` (eval.rs:9048-9054) | Architecturally equivalent |
| Bytecode `call` opcode → Ffuncall → one frame | `bytecode::Vm` pushes at vm.rs:3443 | Architecturally equivalent |
| `apply_lambda` mutates outer eval_sub frame (eval.c:3299) | `apply_internal` pushes its own frame (eval.rs:8987) | **Divergent — extra push** |
| `eval_sub` pushes UNEVALLED at entry (eval.c:2585) | `eval_sub_cons` has no outer push for non-special-form paths | **Divergent — missing push** |
| Macro path: outer eval_sub + inner apply1 → two frames | Macro path: inner push only (eval.rs:6548, 6560) | **Divergent — missing outer** |

The `apply`/`apply_with_frame_function`/`funcall_general`/named-callable pair of variants at eval.rs:8987, 9028, 9048, 9441, 9463 are not independent designs — they all funnel to `funcall_general_untraced`. Consolidating them is housekeeping, not an architectural fix.

---

## What actually diverges

1. **Missing outer eval_sub frame for every non-special-form call.** GNU pushes an UNEVALLED frame at eval_sub entry (eval.c:2583-2585) for every form, including `(car x)` and `(foo (bar) (baz))`. Neomacs only pushes for special forms (post-`34036a7e8`). During arg evaluation, GNU's stack shows `foo` while `bar` runs; neomacs's shows nothing.
2. **No in-place phase transition.** GNU's `set_backtrace_args` (eval.c:144-156) mutates the specpdl slot from UNEVALLED→EVALD at three sites: SUBRP MANY (eval.c:2638), SUBRP fixed-arity (eval.c:2660), apply_lambda (eval.c:3299). Neomacs has no mutation helper; its macro and apply paths push a *new* EVALD frame, so macro calls get two frames (outer missing + inner pushed) while GNU's macro path gets two frames (outer UNEVALLED + inner Ffuncall-EVALD).
3. **Macro path labels the outer-equivalent frame EVALD.** Neomacs at eval.rs:6548 pushes `original_fun` with `arg_values` — carrying the macro symbol name but the EVALD shape. GNU would have an outer UNEVALLED frame with the original-args cons PLUS the inner apply1/Ffuncall frame for the macro function.

The user-visible consequence: any elisp that walks the backtrace during arg evaluation (edebug, `trace-function`, a debugger hook inside `(bar)` in the example above) sees a different stack than GNU.

## Observable behaviors that must be preserved

- `backtrace-frame--internal` output for every existing assertion in `misc_test.rs` and elisp callers in `subr.el`.
- `condition-case` handler invocation ordering (specpdl unwinds in LIFO order, unchanged).
- `unwind-protect` CLEANUP clause runs even when BODY signals — `quit_regression_test.rs::unbind_to_suppresses_quit_during_unwind_protect_cleanup`.
- `mapbacktrace` frame ordering (newest first, eval.c:4031-4041).
- Bytecode `condition-case-debug` marker semantics — `vm_test.rs::vm_condition_case_debug_marker_calls_debugger_before_handler`.
- The three new UNEVALLED regression tests from `34036a7e8`.

---

## Phased approach

### Phase 1: Add `set_backtrace_args_evalled` mutation helper (additive)

- [ ] Implement `Context::set_backtrace_args_evalled(count: usize, args: &[Value])` that locates `specpdl[count - 1]`, asserts it is a `SpecBinding::Backtrace { unevalled: true, .. }`, and mutates it in place: clears `unevalled`, replaces `args` with a fresh `LispArgVec` populated from the evaluated slice. Model on GNU `set_backtrace_args` (eval.c:144-156). Panic on shape mismatch — this is an internal invariant, not user input.
- [ ] Unit test: push an UNEVALLED frame, call the mutator, walk the specpdl, confirm the slot now reads as EVALD with the expected args.

### Phase 2: Introduce outer UNEVALLED push in `eval_sub_cons`

- [ ] At the top of `eval_sub_cons` (eval.rs:6442), before the literal-form fast path, capture `bt_count = self.specpdl.len()` and push an UNEVALLED frame with `(original_fun, original_args)`.
- [ ] At every exit path from `eval_sub_cons`, emit `self.unbind_to(bt_count)` to pop the outer frame.
  - Early returns in the literal-form fast path (line 6468), the resolved-subr special-form branch (added in `34036a7e8`, line 6527), and the macro/apply paths all need this.
- [ ] Remove the `push_unevalled_backtrace_frame` + `unbind_to` inserted in `34036a7e8` at eval.rs:6522-6533 — the new outer push subsumes it.

### Phase 3: Promote the outer frame in place instead of pushing a new inner one

- [ ] Macro path (eval.rs:6548): replace `push_backtrace_frame(original_fun, &arg_values) + unbind_to` with `set_backtrace_args_evalled(bt_count, &arg_values)`. The outer frame transitions UNEVALLED→EVALD with the evaluated macro args. This matches GNU's behavior closest to eval.c:2752-2754 — except GNU's `apply1` subsequently calls Ffuncall which pushes its OWN inner frame on top. To fully match that, call `funcall_general` (which pushes) rather than `apply_lambda` directly, so a second EVALD frame for the macro function appears. Verify against GNU by diffing live backtraces.
- [ ] Cons-cell macro path (eval.rs:6560): same treatment.
- [ ] SUBRP / lambda / bytecode dispatch (the paths that currently go through `apply` from eval_sub_cons): replace the `apply`-call-which-pushes with `apply_untraced`-call-wrapped-by-`set_backtrace_args_evalled`. The outer frame becomes the single frame for this call, matching GNU's eval.c:2638/2660/3299 pattern.

### Phase 4: Audit `Ffuncall`-entry paths

These are already close to GNU — they just need to not push a second frame when called from inside `eval_sub_cons` (which now owns the outer frame).

- [ ] `funcall_general` (eval.rs:9048-9054): keep the push. This is the Ffuncall entry used by bytecode, `apply`/`funcall` builtins, and any external caller. It runs on its own; no outer frame exists.
- [ ] `apply_internal` (eval.rs:8987): audit whether its two callers (`apply` and `apply_untraced`) are ever called from inside `eval_sub_cons`. If yes, the outer + inner stacking gives two frames per call — acceptable because GNU does the same when eval_sub invokes Ffuncall-using primitives (`apply`, `funcall`).
- [ ] `apply_with_frame_function` (eval.rs:9028): consolidate into `apply` with an optional frame-label parameter, or keep separate — housekeeping, not parity-affecting.
- [ ] Bytecode `call` opcode (vm.rs:3443): keep the push. Matches GNU bytecode → Ffuncall → one frame.

### Phase 5: Validation

- [ ] Re-run the three UNEVALLED regression tests from `34036a7e8` (`backtrace_frame_internal_surfaces_unevalled_frame`, `backtrace_frame_internal_surfaces_live_frame`, `eval_sub_cons_pushes_unevalled_frame_for_special_forms`). They should still pass, since the UNEVALLED frame now comes from the outer push.
- [ ] Add a new probe-based test: `eval_sub_cons_pushes_unevalled_frame_for_every_form` — register a Rust `Subr` probe, evaluate `(foo (probe) (probe))` where `foo` is user-defined, assert probe sees an UNEVALLED `foo` frame during each arg eval.
- [ ] Run the broader sweep: `cargo nextest run -p neovm-core` (redirect to file per `feedback_cargo_nextest_workflow`).
- [ ] Manual GNU parity diff: load a file in both `src/emacs -nw -Q` and `neomacs`; trigger `(debug)` inside a macro call; compare frame-by-frame.
- [ ] Run `cargo xtask fresh-build` end-to-end.

## Non-goals

- **No consolidation of the `apply`/`apply_with_frame_function`/`funcall_general` variant quartet.** They already funnel to `funcall_general_untraced`; tidying is out of scope for a parity refactor.
- **No performance work.** Adding one push + unbind_to per `eval_sub_cons` call will measurably slow interpreted code. Accept unless `fresh-build` wall-clock regresses >5%. Bytecode is unchanged by this refactor.
- **No changes to `SpecBinding` variants other than `Backtrace`.**
- **No changes to `debug_on_next_call` / `Vdebug_on_signal` plumbing.** Those are orthogonal.
- **No attempt to unify `eval_sub_cons` with `eval_sub` atoms path.** GNU's eval_sub pushes the frame for the cons-form case only (lines 2564-2565 return early for non-cons); neomacs can keep its split.

## Risks

- **Interpreter slowdown.** Outer push + unbind_to on every non-literal form evaluation. GNU accepts this cost; expect the same but verify on bytecomp.el workloads.
- **Frame ordering assertions.** Any test that counts specpdl entries during a special-form dispatch will see exactly one UNEVALLED frame (same as post-`34036a7e8`). Tests that counted during non-special-form calls will see one new frame where they used to see zero. Fix at the test layer — don't adjust assertions to paper over incorrect-before shapes.
- **`set_backtrace_args_evalled` shape mismatch panic.** If a refactor leaves a code path that tries to promote a non-UNEVALLED frame, the assertion will fire loudly. That's a feature — better than silent shape corruption.
- **Macro two-frame behavior.** Phase 3 calls for calling `funcall_general` inside the macro path so a second EVALD frame appears (matching GNU eval.c:2752 apply1). Must verify that the macro expansion still terminates correctly with two frames on the stack.

## References

- GNU Emacs source: `/home/exec/Projects/github.com/emacs-mirror/emacs/src/eval.c`
- GNU Emacs binary (31.0.50): `src/emacs`
- Surgical fix this plan extends: commit `34036a7e8` (eval/backtrace: record UNEVALLED frames for special-form dispatch)
- Related: `bf201f48a`, `81a116c3a` (quit/advice architecture with similar "dispatch-site opportunistic" shape)
