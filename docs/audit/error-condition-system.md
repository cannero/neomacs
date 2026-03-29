# Error / Condition System Audit

Date: 2026-03-29

Scope:
- `signal`, `condition-case`, `handler-bind`, throw/signal formatting, and signal payload fidelity.
- GNU Emacs reference tree: `/home/exec/Projects/github.com/emacs-mirror/emacs/`

## Fixed in this patch

### Raw signal payload shape now matches GNU Emacs

GNU Emacs preserves the original cdr passed to `signal`. In `src/eval.c`, the error object is constructed as `(ERROR-SYMBOL . DATA)` instead of normalizing DATA into a proper list. That matters for cases like:

```elisp
(condition-case err
    (signal 'error 1)
  (error err))
```

GNU returns:

```elisp
(error . 1)
```

Before this patch, neomacs normalized non-list signal payloads into a `Vec<Value>` and lost the original cdr shape. That broke:
- `condition-case` bindings for raw/non-list payloads
- display/formatting of signaled errors
- rethrowing/conversion paths that round-tripped `EvalError` back into `Flow`

The fix introduces `raw_data` alongside the normalized vector payload and preserves it through:
- `neovm-core/src/emacs_core/error.rs`
- `neovm-core/src/emacs_core/errors.rs`
- `neovm-core/src/emacs_core/eval.rs`
- `neovm-core/src/emacs_core/load.rs`
- `neovm-core/src/emacs_core/lread.rs`
- `neovm-core/src/emacs_core/builtins/misc_eval.rs`
- `neovm-worker/src/lib.rs`

Focused regression tests were added for:
- raw payload preservation in `builtin_signal`
- `condition-case` binding shape for `(signal 'error 1)`
- formatter output for raw payload signals

## Remaining incompatibilities

### `handler-bind` runs too late in neomacs

GNU Emacs installs handler-bind frames into the active handler stack before calling the body function. The handler is therefore invoked inside the dynamic extent of the signaling code. See:
- GNU `src/eval.c`: `push_handler_bind` / `Fhandler_bind_1`
- GNU `test/src/eval-tests.el`: `eval-tests--handler-bind`

Current neomacs behavior is different. `builtin_handler_bind_1` calls the body first, waits for `Err(Flow::Signal(sig))`, and only then walks handlers in Rust:
- `neovm-core/src/emacs_core/builtins/symbols.rs`

That means inner dynamic context has already unwound before the handler runs. GNU specifically guarantees the opposite for `handler-bind`.

Impact:
- inner `catch`
- inner `condition-case`
- dynamic bindings visible at signal time

These can behave differently under neomacs even when the same handler eventually runs.

This is not a small local fix. To match GNU, handler-bind needs to participate directly in the evaluator's nonlocal-exit search, not as an after-the-fact retry layer.

### `condition-case` does not implement GNU's special `debug` condition semantics

GNU Lisp uses a special `debug` condition to let handlers coexist with debugger entry. `condition-case-unless-debug` in `lisp/subr.el` rewrites handlers to include `debug`, relying on runtime debugger-aware signal handling.

Current neomacs `sf_condition_case` only performs structural/hierarchical matching against the signaled condition:
- `neovm-core/src/emacs_core/eval.rs`

It does not implement the debugger-aware `debug` path, and it does not consult `debug-on-error` / `debugger` while choosing handlers. That means macros depending on GNU's `debug` condition semantics are not yet truly compatible.

## Verification notes

GNU reference check used:

```sh
/home/exec/Projects/github.com/emacs-mirror/emacs/src/emacs --batch -Q -l /tmp/neomacs-oracle-read-file.el /tmp/neomacs-signal-raw.forms
```

Observed GNU result:

```elisp
(error . 1)
```

`cargo nextest` on the broader oracle slice remains too expensive for this area under the current parallel setup and produced many timeouts, so this patch uses focused regression coverage instead of treating those timeouts as semantic failures.

## Recommended next steps

1. Rework `handler-bind` so handler frames live in the evaluator's active handler stack during body execution.
2. Add debugger-aware signal dispatch for the `debug` condition and validate `condition-case-unless-debug`.
3. Split large oracle/property suites for error handling into smaller nextest-friendly groups so compatibility regressions stop hiding behind timeouts.
