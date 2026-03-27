# Bootstrap Pipeline Audit: GNU Emacs vs Neomacs

**Date**: 2026-03-28

## Executive Summary

Neomacs loads the **exact same `loadup.el`** from the GNU Emacs lisp/ directory,
producing an equivalent dumped state. The post-dump runtime path diverges in how
it enters `recursive-edit` and sets up display, but the **Lisp-level semantics
are preserved**. There are a few areas that could break 100% compatibility ---
detailed below.

---

## Phase 1: Bootstrap/Dump Phase (loadup.el)

### GNU Emacs

1. `temacs` binary starts with minimal C core
2. C `main()` sets `Vtop_level = "(load \"loadup.el\")"` and enters `Frecursive_edit()`
3. `loadup.el` runs with `dump-mode` set (`"pdump"` or `"pbootstrap"`)
4. Loads ~80+ .el files in exact sequence
5. Platform-specific files gated by `(featurep 'x)`, `(featurep 'pgtk)`, etc.
6. `(dump-emacs-portable "emacs.pdmp")` serializes state
7. `kill-emacs` terminates temacs

### Neomacs

1. Rust `main()` calls `create_bootstrap_evaluator_cached_with_features(&["neomacs"])`
   (`load.rs:2673`)
2. On first run: loads **the same `loadup.el`** from `lisp/` directory
   (`load.rs:2596-2598`)
3. Sets `dump-mode` to `nil` (not `"pdump"`) so loadup.el skips the
   dump/kill-emacs section
4. loadup.el runs with `(featurep 'x)` → **false** (neomacs feature, not x)
5. After loadup.el completes, **explicitly loads** `term/common-win` and
   `term/neo-win` (`load.rs:2636-2646`)
6. Saves result to `.pdump` cache file for subsequent runs

### Verdict: COMPATIBLE

The core bootstrap loads the same files in the same order. The key differences:

| Aspect | GNU Emacs | Neomacs | Compatibility Risk |
|--------|-----------|---------|-------------------|
| `dump-mode` | `"pdump"` | `nil` | **LOW** — loadup.el's `(null dump-mode)` branch adds load-path subdirs and sets `max-lisp-eval-depth` higher, which is fine |
| Window system in loadup | `(featurep 'x)` → loads x-win | `(featurep 'x)` → false | **DESIGNED** — neo-win loaded separately after loadup |
| Native compilation | `(featurep 'native-compile)` gates trampoline setup | No native comp | **LOW** — neomacs doesn't support native comp yet |
| `Snarf-documentation` | Runs during dump | Skipped or fails silently | **MEDIUM** — see below |

---

## Phase 2: Post-Dump Runtime Startup

### GNU Emacs (after pdump load)

1. C `main()` restores from pdump
2. Sets `command-line-args`, `invocation-name`, etc.
3. Enters `Frecursive_edit()`
4. `recursive_edit` evaluates `Vtop_level` → `(normal-top-level)`
5. `normal-top-level` → `command-line`
6. `command-line` sequence:
   - Sets locale (`set-locale-environment`)
   - Loads `site-start.el` (site-run-file)
   - Loads `early-init.el`
   - Package activation (`package-activate-all`)
   - `window-system-initialization` dispatches to
     `(cl-defmethod ... (window-system x))` → `term/x-win.el`'s method
   - Runs `before-init-hook`
   - `frame-initialize` — creates GUI frame
   - Loads user init file (`~/.emacs.d/init.el`)
   - Sets `after-init-time`, runs `after-init-hook`
   - `command-line-1` processes remaining args
   - Runs `emacs-startup-hook`, `term-setup-hook`
   - `frame-notice-user-settings`
   - Runs `window-setup-hook`
   - Displays splash screen

### Neomacs (main.rs)

1. Rust `main()` creates evaluator from pdmp cache
2. `bootstrap_buffers()` — creates *scratch*, *Messages*, minibuffer, first
   frame
3. `apply_runtime_startup_state()` — minimal post-dump setup (icons, scratch
   mode, closure filter)
4. `configure_gnu_startup_state()` — sets `command-line-args`, `window-system`,
   `initial-window-system`, `terminal-frame`, `invocation-name`, etc.
5. Spawns render thread (winit/wgpu)
6. Sets up input bridge and redisplay callback
7. Enters `evaluator.recursive_edit()`
8. `recursive_edit` evaluates `top-level` → `(normal-top-level)` → **same
   `command-line` function from startup.el**

### Verdict: MOSTLY COMPATIBLE with specific differences

The critical insight: **Neomacs runs the same `startup.el` `command-line`
function**. The `top-level` variable points to `normal-top-level` just like GNU
Emacs. The divergence is in what happens *before* entering that Lisp code.

---

## Compatibility Risk Analysis

### 1. `window-system-initialization` Timing — LOW RISK

GNU: `command-line` calls `window-system-initialization` which calls
`x-open-connection`.
Neomacs: `window-system-initialization` dispatches to `neo-win.el`'s
`cl-defmethod` which calls `x-open-connection` (a Rust builtin stub). The
render thread is already running.

**Status**: Compatible. neo-win.el properly implements the
`window-system-initialization` protocol.

### 2. Frame Pre-Creation vs Lazy Creation — MEDIUM RISK

GNU: `command-line` creates the first GUI frame via `frame-initialize` after
`window-system-initialization`.
Neomacs: `bootstrap_buffers()` creates the frame **before** `recursive_edit`.
When `command-line` runs `frame-initialize`, it may find a pre-existing frame.

**Risk**: `frame-initialize` in startup.el may behave differently if a frame
already exists. Looking at the code, `frame-initialize` uses
`make-initial-frame` which calls `make-frame` — Neomacs's
`PrimaryWindowDisplayHost` handles this by "adopting" the pre-existing window.
**Should be OK** but could cause edge-case issues with frame parameters.

### 3. `inhibit-startup-screen` Forced to `t` — LOW RISK

Neomacs hardcodes `inhibit-startup-screen` to `t` in
`configure_gnu_startup_state` (main.rs:1177). The comment says "its fill-region
is extremely slow through with_mirrored_evaluator."

**Risk**: Users won't see the splash screen. This is intentional and documented.

### 4. `terminal-frame` / `frame-initial-frame` — MEDIUM RISK

GNU: Creates a non-visible TTY "terminal frame" and a visible GUI frame.
`terminal-frame` is the TTY frame.
Neomacs: `ensure_gnu_startup_terminal_frame` creates a hidden non-GUI frame as
`terminal-frame` (main.rs:1189-1237). The GUI frame is `frame-initial-frame`.

**Risk**: The frame ordering and visibility semantics should match, but the
exact frame lifecycle (delete/recreate during startup) could differ.

### 5. `site-start.el` / `early-init.el` / Init File Loading — COMPATIBLE

These are all handled by `startup.el`'s `command-line` function, which Neomacs
runs identically. The load paths point to the same `lisp/` directory.

### 6. Hook Execution Order — COMPATIBLE

Since Neomacs runs the same `startup.el` code, the hook order is identical:

1. `before-init-hook`
2. `after-init-hook`
3. `emacs-startup-hook` + `term-setup-hook`
4. `window-setup-hook`

### 7. Package Initialization — COMPATIBLE

`package-activate-all` runs during `command-line` if
`package-enable-at-startup` is non-nil. Same code path.

### 8. GC Disabled During Startup — LOW RISK

Neomacs sets `evaluator.set_gc_threshold(usize::MAX)` before entering
recursive_edit (main.rs:659), disabling GC during startup. GNU Emacs runs GC
normally.

**Risk**: Higher memory usage during startup. Should not affect semantics unless
code relies on GC side effects (weak hash tables, etc.). GC is re-enabled later
by the Lisp-level `garbage-collect` calls.

### 9. `function-get` Override — LOW RISK

Neomacs overrides the Elisp `function-get` with a Rust builtin
(main.rs:1185-1186) to avoid excessive eval depth during macroexpand.

**Risk**: Should be functionally identical, just faster. If there's any
behavioral difference in the Rust implementation vs the Elisp one, it would show
up here.

### 10. `load-source-file-function` — COMPATIBLE

GNU sets `load-source-file-function` in loadup.el (line 147). Neomacs runs the
same loadup.el.

### 11. No `.elc` Loading — MEDIUM RISK

Neomacs's loader doesn't support `.elc.gz` (load.rs:1178-1181) and appears to
load `.el` source files. GNU Emacs loads `.elc` compiled files.

**Risk**: Slower execution of Lisp during bootstrap (interpreted vs
byte-compiled). This shouldn't affect semantics since both paths evaluate the
same code, but:
- Macro expansion behavior may differ subtly between interpreted and
  byte-compiled code
- Performance-critical paths like `font-lock`, `simple.el`, etc. will be slower

### 12. `emacs-build-number` / Repository Version — LOW RISK

GNU: loadup.el computes `emacs-build-number` from existing binaries and sets
`emacs-repository-version`.
Neomacs: loadup.el runs with `dump-mode=nil`, so the build-number section is
skipped (guarded by `(if (and (or (equal dump-mode "dump") ...)`).

**Risk**: `emacs-build-number` may be undefined. `emacs-repository-version`
won't be set. Minor — most code doesn't depend on these.

### 13. `Snarf-documentation` Skipped — MEDIUM RISK

GNU: Runs `(Snarf-documentation "DOC")` during loadup.el to map doc strings.
Neomacs: With `dump-mode=nil`, the Snarf path still runs (loadup.el:481-483
`condition-case nil (Snarf-documentation "DOC")`) but may fail silently if the
DOC file doesn't exist or the builtin doesn't work the same way.

**Risk**: Documentation strings for builtins may be missing.
`(documentation 'some-builtin)` could return nil or error.

### 14. `dump-mode` Cleanup — LOW RISK

GNU: loadup.el uninterns `dump-mode` before allowing user code.
Neomacs: `dump-mode` is set to `nil` before loadup, then set to `nil` again in
main.rs (line 660). `loadup.el` line 678-680 strips `-l loadup` from
`command-line-args`.

**Risk**: The variable `dump-mode` remains bound (but nil) in Neomacs. GNU
uninterns it. Code checking `(boundp 'dump-mode)` would see different results.
Extremely unlikely to matter.

---

## Summary Table

| Area | Risk | Details |
|------|------|---------|
| loadup.el file loading sequence | OK | Identical files, identical order |
| Window system initialization | OK | neo-win.el properly implements the protocol |
| Hook ordering | OK | Same startup.el runs |
| Init file / early-init loading | OK | Same startup.el handles it |
| Package activation | OK | Same code path |
| Frame lifecycle during startup | MEDIUM | Pre-created frame may cause edge cases |
| .elc.gz unsupported | MEDIUM | Interpreted-only, no compressed elc |
| Documentation strings (Snarf) | MEDIUM | May be missing for builtins |
| Splash screen disabled | LOW | Intentional, users can override |
| GC disabled during startup | LOW | Performance difference only |
| build-number / repo version | LOW | Variables undefined, rarely used |
| `function-get` override | LOW | Should be semantically identical |
| Native compilation | LOW | Not supported, but not expected |

**Bottom line**: The bootstrap pipeline is **semantically compatible at the Lisp
level**. The same `startup.el` → `normal-top-level` → `command-line` →
`command-line-1` sequence runs in both. The two medium-risk items (frame
pre-creation and .elc loading) are the most likely to cause subtle behavioral
differences in edge cases, but won't break normal usage.
