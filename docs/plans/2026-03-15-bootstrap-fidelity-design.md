# Design: Bootstrap Fidelity as the Primary Architecture Goal

**Date**: 2026-03-15
**Status**: Proposed

## Problem

NeoVM currently reaches GNU Emacs compatibility through a mix of:

- upstream GNU Lisp files
- Rust evaluator/bootstrap shims
- startup-only autoload seeding
- custom bootstrap sentinels in the load sequence
- post-bootstrap runtime normalization
- selective `.neobc` compilation workarounds

This works incrementally, but it is not the ideal long-term architecture.
It creates split ownership of behavior and pushes GNU Lisp library semantics
into Rust modules that should not be the semantic source of truth.

The visible symptoms include:

- `cl_lib.rs` acting as a compatibility bucket instead of upstream `cl-lib.el`
  being the sole owner of `cl-*`/`seq-*` library behavior
- bootstrap stubs for `gv` / `cl-preloaded` support
- startup seeding of autoload/function-cell shapes that should eventually fall
  out of faithful loadup + dump state
- runtime cleanup/normalization code to repair bootstrap surfaces after the fact

## Principle

- **C / runtime machinery in GNU Emacs -> Rust in NeoVM**
- **Elisp library semantics in GNU Emacs -> load the upstream `.el` file**

This means:

- Rust should own the VM, primitives, memory model, bytecode/JIT, host ABI,
  scheduler, buffers/windows/frames/processes/timers, and redisplay plumbing.
- GNU Lisp should remain the source of truth for `cl-lib`, `gv`, `seq`,
  `nadvice`, `simple`, `subr`, `pcase`, and other Lisp-layer libraries.
- Rust-side compatibility buckets for GNU Lisp libraries should be treated as
  deletion-target transitional debt.

## Root Cause

The current bootstrap path is not yet faithful enough to GNU Emacs.

The main reasons are:

1. NeoVM is source-first and does not consume GNU `.elc` artifacts.
2. The bootstrap sequence is hand-emulated in Rust rather than inherited from a
   mature dumped runtime end-to-end.
3. Macroexpansion/bootstrap semantics are still incomplete enough that files
   such as `gv.el` cannot always load in the same phase and shape as GNU Emacs.
4. Source bootstrap exposes dependency cycles and `eval-when-compile` effects
   that GNU often avoids through dumped / precompiled startup state.

Those constraints forced pragmatic local shims. They were reasonable for
incremental progress, but they should not become the architecture.

## Target Architecture

### 1. Strict ownership split

```text
GNU Lisp libraries
  -> semantic owner of Lisp-layer behavior

neovm-core
  -> semantic owner of VM/runtime/primitive behavior

neovm-host-abi
  -> only typed host boundary

neomacs host/editor
  -> semantic owner of editor host state and rendering integration
```

Rules:

- `neovm-core` may implement primitives needed by Lisp, but not reimplement GNU
  Lisp library semantics unless a primitive truly belongs below the Lisp layer.
- `neovm-core` must not become a second copy of `cl-lib` / `seq` / `gv`.
- GNU Lisp files should load with the same phase/order assumptions as much as
  possible.

### 2. Faithful bootstrap state, not post-hoc repair

The ideal runtime should come from a faithful bootstrap image, not from:

- hand-seeded startup autoloads
- manual function-cell shape patching
- cleanup passes that unbind and then restore pieces of the runtime surface
- selective source recompilation as a semantic workaround

Short-term repair code may remain while migrating, but the target is:

- build the right bootstrap image
- dump the right bootstrap image
- load the right bootstrap image
- avoid repairing it afterward

### 3. Upstream Lisp-first library loading

The target for GNU Lisp libraries is:

- `require 'cl-lib` loads upstream `cl-lib.el`
- `require 'gv` loads upstream `gv.el`
- `require 'seq` loads upstream `seq.el`
- `eval-when-compile` behavior is correct enough that these files do not need
  Rust-side stand-ins

Any Rust helper that exists only to compensate for incorrect bootstrap order or
incomplete macroexpansion should be considered temporary.

## Explicit Long-Term Goals

### Goal A: Shrink and delete compatibility buckets

Files like `cl_lib.rs` should converge toward one of two outcomes:

- only true primitive/runtime helpers remain, or
- the file disappears entirely

This also applies to any future Rust module that starts mirroring GNU Lisp
library behavior instead of implementing a genuine runtime primitive.

### Goal B: Remove bootstrap sentinels

The custom bootstrap sentinels should be treated as debt:

- `!bootstrap-cl-preloaded-stubs`
- `!require-gv`
- `!reload-subr-after-gv`
- any similar bootstrap-only repair stage

Their removal is a concrete architecture metric.

### Goal C: Remove runtime normalization hacks

The runtime should not need manual cleanup to look GNU-like after bootstrap.

The more state we normalize after the fact, the less faithful the bootstrap is.
Normalization code should trend down over time, not up.

### Goal D: Keep optional performance layers orthogonal

Quickening, JIT, parse caches, `.neoc`/`.neobc`, isolate scheduling, and render
thread improvements should all sit on top of faithful semantics. They must not
become excuses to fork Lisp-layer behavior.

## Migration Strategy

### Phase 1: Freeze new semantic drift

Policy:

- no new Rust compatibility buckets for GNU Lisp libraries
- no new startup seeding unless required to unblock bootstrap, and every new
  seed must carry a deletion condition
- every bootstrap shim must point to the exact upstream file/load phase it is
  compensating for

### Phase 2: Make bootstrap measurable

Add dedicated bootstrap fidelity checks for:

- `require` behavior of `cl-lib`, `gv`, `seq`, `cl-generic`, `nadvice`
- `autoloadp` / `symbol-function` startup shapes
- `featurep` surface before and after runtime startup
- `eval-when-compile` behavior during source bootstrap
- `load-history`, `after-load`, and `loaddefs` side effects

Success metric:

- bootstrap compatibility failures are visible as first-class regressions, not
  discovered indirectly through random package failures

### Phase 3: Fix macro/bootstrap ordering

Prioritize removal of the specific blockers that force shims:

- `pcase` compatibility strong enough for `gv.el`
- `macroexpand-for-load` parity
- `eval-when-compile` parity under source loading
- `cl-preloaded`/`cl-macs`/`cl-generic` load-order fidelity

Success metric:

- `gv` / `cl-lib` / `seq` / `cl-generic` load through upstream Lisp paths
  without bootstrap stand-ins

### Phase 4: Delete repair layers

After the above is green:

- remove dead Rust-side `cl-*` / `seq-*` compatibility helpers
- remove bootstrap sentinels one by one
- remove startup autoload seeds that are only compensating for bootstrap drift
- reduce runtime normalization passes to the minimum true runtime contract

Success metric:

- GNU Lisp owns GNU Lisp behavior again

## What We Should Not Do

- We should not replace GNU Lisp library semantics with Rust just because Rust
  is easier to control.
- We should not optimize bootstrap by hardcoding more library behavior in Rust.
- We should not accept split ownership as the stable architecture.
- We should not interpret temporary bootstrap hacks as proof that the hacks are
  the right design.

## Recommended Immediate Next Slice

The next architecture-driven slice should be a bootstrap fidelity slice, not a
new feature slice:

1. Add a focused bootstrap oracle suite for `require 'cl-lib`, `require 'gv`,
   `require 'seq`, and `require 'cl-generic`.
2. Annotate every remaining shim with its exact blocker and deletion target.
3. Remove one shim by fixing the real bootstrap blocker rather than adding
   another compatibility layer.

## Architecture Standard

The clean long-term standard is:

- **Observable GNU behavior must match**
- **GNU Lisp libraries remain the semantic source of truth**
- **Rust owns the VM and host internals, not the Lisp library layer**
- **Bootstrap fidelity is the path to performance, maintainability, and real
  compatibility**
