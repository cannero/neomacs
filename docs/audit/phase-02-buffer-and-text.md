# Phase 2 Audit: Buffer & Text

**Date**: 2026-03-28

## GNU source ownership

Primary GNU source files:

- `src/buffer.c`
- `src/insdel.c`
- `src/marker.c`
- `src/intervals.c`
- `src/textprop.c`
- `src/itree.c`
- `src/region-cache.c`
- `src/undo.c`

GNU design here is centralized:

- `buffer.c`, `insdel.c`, and `marker.c` define the authoritative text and
  marker model.
- `intervals.c` and `textprop.c` define text property semantics on the same
  text core.
- `itree.c` defines overlays on that same text core.
- `undo.c` depends on the same mutation model.

GNU does **not** duplicate these semantics in its display backend.

## Neomacs source ownership

Primary Neomacs semantic files:

- `neovm-core/src/buffer/buffer.rs`
- `neovm-core/src/buffer/buffer_text.rs`
- `neovm-core/src/buffer/gap_buffer.rs`
- `neovm-core/src/buffer/overlay.rs`
- `neovm-core/src/buffer/text_props.rs`
- `neovm-core/src/buffer/undo.rs`
- `neovm-core/src/emacs_core/buffer_vars.rs`
- `neovm-core/src/emacs_core/marker.rs`
- `neovm-core/src/emacs_core/builtins/buffers.rs`

Recent cleanup removed the old duplicate text/runtime core from
`neomacs-display-runtime/src/core/`.  That crate now consumes protocol/layout
types instead of exporting a second editor-text subsystem.

## Audit result

Status is **functionally improving, still not GNU-shaped yet**.

Good:

- `neovm-core/src/buffer/` is already a real semantic buffer subsystem.
- Recent compatibility work improved modified/autosave state, buffer-local
  behavior, and indirect-buffer oracle coverage.
- GNU oracle coverage now includes indirect-buffer text-property behavior after
  shared-text mutation.
- The deleted duplicate display-runtime text core was the right refactor:
  GNU does not split buffer semantics across a VM crate and a display crate,
  and Neomacs should not either.

Remaining design gaps:

- GNU splits buffer-local storage into:
  - dedicated per-buffer slots described by `buffer_local_flags`
  - `local_var_alist` for ordinary Lisp locals
  Neomacs still collapses both into `properties + local_binding_names`.
- GNU text properties are interval state on the shared text object.
  Neomacs still stores `TextPropertyTable` per buffer and keeps indirect-buffer
  siblings in sync manually.
- GNU narrowing markers (`pt_marker`, `begv_marker`, `zv_marker`) are explicit
  C-owned state for indirect buffers. Neomacs models narrowing and markers
  correctly at the surface in many cases, but the source ownership is still
  less direct than GNU's `buffer.c` + `marker.c`.
- GNU region-cache / interval ownership is inside the core buffer/text engine.
  Neomacs still has a simpler model and has not yet matched the full C design.

## Long-term ideal design

The ideal design is:

- `neovm-core/src/buffer/` is the **only** semantic owner of buffer text,
  markers, overlays, text properties, narrowing, and undo state.
- `neovm-core/src/emacs_core/` exposes those semantics to Lisp.
- `neomacs-display-runtime` consumes snapshots or read-only projections from
  `neovm-core`; it does not re-implement text semantics.
- `BufferText` should grow toward GNU's shared text-object role:
  it should own shared interval/text-property state for indirect buffers, not
  leave that state duplicated on each `Buffer`.
- Buffer-local storage should distinguish slot-backed builtin locals from
  ordinary Lisp local bindings, instead of flattening everything into one map.

The display runtime may have caches, but those caches must not become a second
buffer model.

## Required work

- Keep `neomacs-display-runtime` as a consumer of VM-owned text state.
- Refactor buffer-local storage toward GNU's two-tier model:
  slot-backed per-buffer variables plus ordinary `local_var_alist`-style
  bindings.
- Move text-property ownership closer to the shared text object used by
  indirect buffers.
- Extend GNU differential coverage for:
  marker relocation, insertion-type, text property mutation, overlay ordering,
  narrowing, and undo boundary behavior.
- Add explicit GNU oracle cases for narrowing-marker behavior and interval/root
  ownership transitions on indirect-buffer teardown.

## Exit criteria

- One authoritative text model in `neovm-core`.
- Overlay, marker, text property, and undo behavior proven against GNU Emacs.
- No display/runtime text core that can drift from VM semantics.
- Buffer-local storage modeled close enough to GNU that `buffer.c` remains the
  obvious source-level spec.
