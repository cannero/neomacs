# Error / Condition Unification Plan

Date: 2026-03-29

## Recommendation

Yes, neomacs should unify this subsystem.

But the correct target is not "one giant stack for everything".

GNU Emacs uses multiple stacks:

- C stack
- `specpdl` for dynamic bindings, backtraces, and unwind-protect cleanup
- handler stack for `catch`, `condition-case`, and `handler-bind`

So the right neomacs goal is:

- unify condition dispatch
- keep unwind/binding state separate

That is the GNU design we should copy.

## Why unification is the right move

The remaining incompatibilities are not independent bugs.

They all come from the same architectural split:

- interpreter `condition-case` is handled in `eval.rs`
- `handler-bind-1` is a wrapper builtin in `builtins/symbols.rs`
- VM condition handling is local to `bytecode/vm.rs`
- debugger policy is isolated in `debug.rs` and not part of signal dispatch

GNU does not split those semantics this way.

In GNU:

- `signal_or_quit` searches active handlers
- `handler-bind` participates in that search directly
- `condition-case` participates in that search directly
- debugger entry is decided from the result of that search
- bytecode and interpreter use the same condition runtime

So if neomacs keeps patching local symptoms, more parity bugs will keep
reappearing around the same boundary.

## Current neomacs pieces

Pieces that are useful and should stay:

- `Context.specpdl`
- `Flow::Signal` with `raw_data`
- lexical-environment save/restore machinery
- GC rooting machinery (`temp_roots`, `vm_gc_roots`)

Pieces that should stop being authoritative:

- interpreter-only `sf_condition_case` matching loop
- wrapper-style `builtin_handler_bind_1`
- VM-local `Handler` stack as the source of truth

Pieces that are currently detached:

- `DebugState`

`DebugState` is not wired into `Context` or the actual signal path.  That means
it should not be treated as the kernel of the future design.  At most it can
survive as debugger UI/session state after signal dispatch is unified.

## Target architecture

Add a Context-owned condition handler stack.

Example shape:

```rust
enum ConditionFrame {
    Catch {
        tag: Value,
        resume: ResumeTarget,
    },
    ConditionCase {
        conditions: Value,
        resume: ResumeTarget,
    },
    HandlerBind {
        conditions: Value,
        handler: Value,
        mute_span: usize,
    },
    SkipConditions {
        remaining: usize,
    },
}
```

`ResumeTarget` should encode where control returns:

- interpreter catch
- interpreter condition-case clause
- VM catch target
- VM condition-case target

The important point is not the exact enum layout.  The important point is that
all condition search happens against one stack owned by `Context`.

## What should remain separate

Do not merge these into the condition stack:

- `specpdl`
- unwind-protect cleanup records
- plain dynamic let bindings
- backtrace records

GNU keeps those on `specpdl`, and neomacs already has a reasonable analogue.

This also implies one concrete cleanup:

- VM local handler markers should only mirror condition frames; unwind cleanup
  belongs entirely to unwind records

`unwind-protect` belongs with unwind records, not with condition dispatch.

## Required runtime entry points

Neomacs needs a single internal runtime path for nonlocal condition dispatch.

Conceptually:

```rust
fn dispatch_signal(sig: SignalData) -> Result<Value, Flow>
fn dispatch_throw(tag: Value, value: Value) -> Result<Value, Flow>
```

`dispatch_signal` should own:

1. building the GNU-style error object
2. `signal-hook-function`
3. error hierarchy lookup
4. handler-stack search
5. `handler-bind` immediate invocation
6. temporary muting of lower condition handlers
7. debugger decision
8. transfer to the chosen resume target

`dispatch_throw` should own:

1. searching active catch frames
2. deciding throw vs `no-catch`
3. resuming interpreter or VM target

This is the GNU boundary.  Today neomacs spreads these responsibilities across
special forms, builtins, and the VM.

## Debugger policy

Debugger policy should move into signal dispatch.

The authoritative inputs should be Lisp-visible variables already seeded in the
obarray:

- `debug-on-error`
- `debug-on-signal`
- `debug-on-quit`
- `debug-ignored-errors`
- `inhibit-debugger`
- `debugger`

That matches GNU much more closely than the current standalone `DebugState`
booleans.

Recommendation:

- keep `debug.rs` only for debugger session/backtrace tooling
- stop using it as the semantic owner of "should this signal enter debugger?"

## Bytecode integration

The VM should stop owning its own separate condition semantics.

Today it has:

- local `handlers: Vec<Handler>`
- `resolve_signal_target`
- `resolve_throw_target`

That duplicates interpreter behavior and guarantees drift.

Target:

- VM pushes/pops `ConditionFrame`s on `Context`
- VM-specific resume metadata stays in `ResumeTarget`
- common dispatch logic resolves the frame
- VM only performs the actual low-level resume once a target is selected

This preserves VM efficiency while removing semantic duplication.

## Migration order

### Phase 1: Introduce the shared stack

- Add `Context.condition_stack`
- Keep existing code paths, but mirror `catch` / `condition-case` / VM pushes
  into it
- Use assertions/tests to verify stack balance

Phase 1 scaffold status:

- `Context.condition_stack` mirrors interpreter `catch`,
  `condition-case`, `handler-bind-1`, and VM catch/condition-case frames
- top-level evaluator cleanup and VM frame exit both truncate the shared stack
- GC root collection now traces mirrored condition frames
- runtime dispatch semantics are still unchanged in this phase:
  `catch_tags`, interpreter-local `condition-case`, wrapper-style
  `handler-bind-1`, and VM-local resolution still decide behavior

### Phase 2: Unify `catch` and `throw`

- Make throw resolution consult the shared stack
- Reduce `catch_tags` to a compatibility mirror
- Remove it once no runtime code depends on it

Phase 2 progress:

- interpreter `throw` and `validate_throw` now consult `Context.condition_stack`
- VM outer-catch fallback now consults the shared stack after local VM unwind
- VM no longer mirrors local catch frames into a separate catch-tag list
- the old `catch_tags` mirror has now been removed entirely

### Phase 3: Unify `condition-case`

- Move interpreter `condition-case` selection to shared dispatch
- VM condition targets use the same selection logic

Phase 3 progress:

- interpreter `condition-case` frames now carry stable resume identity in the
  shared stack
- shared signal dispatch annotates the selected interpreter or VM resume target
- nested interpreter and VM `condition-case` selection now follows the shared
  stack instead of local ad hoc matching
- the bootstrap regression
  `bootstrap_condition_case_lexical_handler_binding_restores_outer_let`
  passes with the unified selection path

### Phase 4: Rebuild `handler-bind`

- Remove wrapper retry semantics
- Invoke handlers during dispatch
- Implement GNU-style lower-handler muting

Phase 4 progress:

- `handler-bind-1` no longer retries handlers after body unwind
- shared signal dispatch invokes `handler-bind` handlers immediately
- temporary `SkipConditions` frames now mute lower
  `condition-case` / `handler-bind` frames during handler execution
- GNU-style dynamic-extent and masking regressions now pass in both the
  interpreter and the VM

### Phase 5: Move debugger policy into dispatch

- Implement `debug` marker semantics
- consult `debug-on-error`, `debug-on-signal`, `debug-ignored-errors`,
  `inhibit-debugger`
- make `condition-case-unless-debug` and `with-demoted-errors` true GNU-style

Phase 5 progress:

- shared signal dispatch now decides debugger entry from the selected clause,
  matching GNU's "search first, debugger second" design
- dispatch consults `debug-on-error`, `debug-on-quit`, `debug-on-signal`,
  `debug-ignored-errors`, `inhibit-debugger`, `debugger`, and
  `debugger-may-continue`
- `(debug ...)` handler markers now permit debugger entry without bypassing the
  handler
- interpreter, lexical-binding interpreter, loaded `subr.el` macro paths, and
  VM clause dispatch now agree on debugger suppression vs entry
- `condition-case-unless-debug` and `with-demoted-errors` regressions now pass
  with the shared runtime
- detached debugger-policy state has been deleted from `DebugState`
- active catches now live only in the shared condition stack; the old
  `catch_tags` mirror is gone
- VM no longer stores duplicate catch/condition-case target metadata in its
  local handler stack
- shared VM resume targets now carry stable identities, so nested frames with
  identical numeric `(pc, stack_len, spec_depth)` tuples cannot be conflated
- VM local state still owns low-level unwind structure, especially
  `unwind-protect` cleanup sequencing
- neomacs-compiled `unwind-protect` now lowers through GNU-style cleanup
  closures plus `UnwindProtectPop`
- the VM local handler list now mirrors only condition frames; the legacy
  jump-target opcode has been removed from the live runtime `Op` surface and
  survives only as an explicit pdump compatibility rejection boundary

### Phase 6: Delete redundant logic

- remove VM-local resolution logic
- remove detached debugger-policy logic from `DebugState`
- simplify special forms that only wrapped local searches

## Acceptance criteria

This refactor is only done when all of these are true:

- GNU `handler-bind` semantics match, especially dynamic extent and muted lower
  handlers
- GNU `condition-case-unless-debug` semantics match
- bytecode and interpreter produce the same error-dispatch behavior
- raw signal payloads stay preserved
- no remaining runtime dependency on `catch_tags` or wrapper-style
  `handler-bind-1`

## Bottom line

Yes, unify.

But unify the same thing GNU unifies:

- the condition runtime

Do not unify the wrong thing:

- `specpdl`, unwind cleanup, and all evaluator state into one generic stack

If we follow GNU's split precisely, the design gets simpler, not more complex.
