# NeoVM Subsystem Porting Playbook

This playbook describes how to bring the remaining Rust-written Elisp subsystems online inside the NeoVM core so they can participate in the compatibility gates and the builtin registry.

## Candidate modules

The following directories currently exist under `rust/neovm-core/src/elisp/` as placeholders or work-in-progress ports from the Emacs C core. Each directory should eventually be wired into `elisp/mod.rs`, registered with the builtin dispatch table, and guarded by compatibility cases when appropriate.

| Module | Purpose | Current status |
| --- | --- | --- |
| `buffer` | Buffer management builtins, buffer-local helpers, overlays | placeholder directory (no files yet) |
| `callproc` | Process/subprocess and `$PATH`-style environment helpers | placeholder directory |
| `character` | Character width/direction conversions and predicates | placeholder directory |
| `data` | Symbol, variable, and obarray helpers (including buffer-local helpers) | placeholder directory |
| `dispnew` | Redisplay/time helpers such as `sit-for`, `sleep-for`, redraw, and stubs | **extracted** — `dispnew/pure.rs` holds 13 builtins + cursor state + window designator helpers |
| `keyboard` | Input/command/bindings helpers (event loops, key parsing) | placeholder directory |
| `terminal` | Terminal/display capability query builtins | **extracted** — `terminal/pure.rs` holds 32 builtins + state |
| `xfaces` | Face/color/font helpers required for display configuration | placeholder directory |

> These modules are tracked in `docs/neovm-untracked-elisp-port-inventory.md`. They exist to keep porting efforts visible and aligned, even when there currently are no source files stored inside them.

## Integration checklist

For each module you bring online:

1. **Expose the module through `elisp/mod.rs`.** Add `pub mod <module>;` to the module list so the compiler builds it when NeoVM is enabled. Keep the list alphabetically grouped with adjacent domains.
2. **Register builtins.** Ensure the module declares builtin definitions (via `builtin_registry::declare_builtin!` or similar) and that `builtin_registry` picks up the functions. This typically means creating a `dispatch` function that adds entries to the VM’s registry when the module is initialized.
3. **Sync compatibility cases.** Expand `test/neovm/vm-compat` with relevant `.forms` files (or reuse existing ones) that exercise the new builtins so the NeoVM output can be compared to the GNU Emacs oracle. Update additional gating scripts if the subsystem affects `math`, `display`, or builtin registry counts.
4. **Run the compatibility gate.** Execute the targeted harness (e.g., `make -C test/neovm/vm-compat check-neovm` or the specific builtin registry gate) and verify there are no regressions. Record the completed slice in `docs/ongoing-tasks.md` so the active backlog stays current.
5. **Document your slice.** Capture the change in the relevant plan or design note under `docs/plans/` when the work needs durable context beyond the running task log.

## Suggested first slices

- Implement textual verification builtins from `terminal/*` because they are isolated and affect display capability queries only. Start by creating the src file inside `rust/neovm-core/src/elisp/terminal/`, wiring the builtins into the registry, and adding a `.forms` test of `display-attributes` or `window-system`.
- Port `keyboard` helpers next by implementing `current-input-method`, `key-description`, or similar functions, registering them, and checking them against the oracle.
- Add buffer/operator helpers in `buffer/` after those modules have a stable API; run the general `check-neovm` sweep to keep the gating green.

If you need further guidance on a specific subsystem, refer to `docs/elisp-vm-design.md` for the overarching compatibility contract and `docs/ongoing-tasks.md` for how to break work into gated slices.
