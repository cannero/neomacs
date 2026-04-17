# Syntax Table GNU-Parity Refactor Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete the eagerly-compiled `SyntaxTable { entries: HashMap<char, SyntaxEntry>, parent }` representation. Read syntax entries on demand from the chartable `Value` stored in `buf.slots[BUFFER_SLOT_SYNTAX_TABLE]`, matching GNU Emacs's design where `SYNTAX_ENTRY(c) = CHAR_TABLE_REF(current_buffer->syntax_table, c)` is the only runtime form.

**Why:** The compiled HashMap gave O(4K) rebuilds per `set-buffer` / `set-syntax-table`, which killed redisplay fontification (see commit `d18db589d` memoization stopgap). GNU has no such dual representation — the chartable *is* the runtime form, and motion/parse code calls `SYNTAX_ENTRY` per character. Parity is non-negotiable; memoization around a non-GNU structure is not the right fix.

**Architecture:** Five tasks.
- T0: lock in behavior with elisp-level regression tests that exercise `forward-word`, `backward-word`, `skip-syntax-forward/backward`, `parse-partial-sexp`, `char-syntax`, `modify-syntax-entry`, `with-syntax-table`. They pass today on the memoized HashMap form and must keep passing through every subsequent task.
- T1: add GNU-parity direct-read helpers — `syntax_class_at_char(table: Value, c: char)` and `syntax_entry_at_char(table: Value, c: char)` — mirroring `SYNTAX_ENTRY` / `syntax.h:syntax.c`. No callers yet.
- T2: route every motion/parse/query path (`forward_word`, `backward_word`, `scan_sexps`, `skip_syntax_forward/backward`, `parse_partial_sexp`, `char_syntax`, `find_defun_start` and friends) through the new helpers. Compiled `SyntaxTable` stays compiled-but-unused as a no-op alongside.
- T3: delete `Buffer.syntax_table`, the `SyntaxTable` struct and all impls, `syntax_table_from_chartable`, `apply_compiled_syntax_entry`, `sync_current_buffer_syntax_table_state`, and the memoization from `d18db589d`. `SyntaxClass` / `SyntaxEntry` / `SyntaxFlags` stay — they're the shape of one decoded chartable entry.
- T4: pdump v23→v24, drop `DumpSyntaxTable` / `DumpSyntaxEntry` wrapping. The chartable `Value` is already serialized through the normal Value path; nothing else is needed.

**Tech stack:** Rust (stable 1.93.1), `cargo nextest`, `neovm-core` crate.

**Testing conventions:** `cargo nextest run -p neovm-core <filter>` — **never** `cargo test`. `cargo check -p neovm-core` for compile checks; never `--release`. Redirect nextest output to a file and grep; the log is large.

**Spec reference (GNU):** `src/syntax.h` — `SYNTAX_ENTRY`, `SYNTAX(c)`, `SYNTAX_MATCH(c)`, `SYNTAX_WITH_FLAGS(c)`. `src/syntax.c` — `forward-word`, `scan-sexps`, `skip-syntax-forward`, `Fset_syntax_table`. Every motion function receives only the buffer; the syntax table is `BVAR (current_buffer, syntax_table)`, looked up as a char-table on each character.

**Baseline:** `main` at `d18db589d` (current HEAD). Branch each task if desired; keep commits small and sequential.

**Pre-existing failing test:** `vm_syntax_table_accessors_use_shared_current_buffer_state` fails with SIGABRT on plain `main` independent of this refactor. Not a blocker; investigate separately.

---

## Task 0: Regression test harness

**File:** `neovm-core/src/emacs_core/syntax_gnu_parity_regression_test.rs` (new)

- [ ] **Step 0.1: Write elisp-level regression tests.** Cover: `(forward-word 1)` crossing whitespace/punctuation; `(backward-word 1)`; `(skip-syntax-forward "w")` / `(skip-syntax-backward "w")`; `(char-syntax ?a)`, `(char-syntax ?\\ )`, `(char-syntax ?\n)`, `(char-syntax ?\u4e2d)` (CJK word); `(modify-syntax-entry ?_ "w")` then verify forward-word crosses underscore; `(with-syntax-table (copy-syntax-table) (modify-syntax-entry ...) (forward-word 1))` returns to outer table afterward; `(parse-partial-sexp BEG END)` — depth, last-sexp-start, in-string, in-comment. Each test should run in a fresh Context, drive via `eval_value` on read-from-string forms.

- [ ] **Step 0.2: Register the module.** Add `#[cfg(test)] mod syntax_gnu_parity_regression_test;` in `neovm-core/src/emacs_core/mod.rs`.

- [ ] **Step 0.3: Confirm baseline passes.** `cargo nextest run -p neovm-core syntax_gnu_parity_regression` — all green on current HEAD before proceeding.

**Commit:** `syntax: add GNU-parity regression tests`

---

## Task 1: Direct-read helpers

**File:** `neovm-core/src/emacs_core/syntax.rs`

- [ ] **Step 1.1: Add `syntax_entry_at_char(table: Value, c: char) -> Option<SyntaxEntry>`.** Call `chartable::builtin_char_table_range(vec![table, Value::fixnum(c as i64)])`. Feed result through the existing `syntax_entry_from_chartable_entry` decoder. Return `None` on nil entry — caller falls back to GNU default (word for >=0x80, whitespace otherwise), matching `char_syntax()` today.

- [ ] **Step 1.2: Add `syntax_class_at_char(table: Value, c: char) -> SyntaxClass`.** Thin wrapper — `syntax_entry_at_char(table, c).map(|e| e.class).unwrap_or_else(default_for_char)`.

- [ ] **Step 1.3: Unit-test the helpers.** Inline `#[cfg(test)]` tests in `syntax.rs` that construct a chartable via `make-char-table` / `set-char-table-range`, call the new helpers, and verify parity with `SyntaxTable::char_syntax()` across ASCII, CJK, word-boundary chars.

**Commit:** `syntax: add direct chartable readers mirroring GNU SYNTAX_ENTRY`

---

## Task 2: Route callers through direct readers

**File:** `neovm-core/src/emacs_core/syntax.rs` (bulk of the change)

Each motion/parse function currently takes `&SyntaxTable`. Change the signature to take the chartable `Value` instead. The buffer already holds this — no wider API change needed.

- [ ] **Step 2.1: `forward_word` and `forward_word_with_options`.** Signature from `(&Buffer, &SyntaxTable, i64)` → `(&Buffer, Value, i64)`. Replace `table.char_syntax(ch)` with `syntax_class_at_char(table, ch)` at every call site.

- [ ] **Step 2.2: `skip_syntax_forward`, `skip_syntax_backward`.** Same signature swap, same call-site rewrite.

- [ ] **Step 2.3: `scan_sexps`, `scan_lists`, `parse_partial_sexp` and internal helpers (`syntax_class_and_flags`, `effective_syntax_entry_for_char_at_byte`).** These are the largest consumers. Keep the per-character lookup — GNU does the same. If a tight loop re-reads the chartable Value, that's fine — it's a simple tree walk.

- [ ] **Step 2.4: `builtin_char_syntax` / `builtin_syntax_class`.** Already operate on a chartable Value externally; internally switch from the compiled form to the new helpers.

- [ ] **Step 2.5: All call sites in `syntax_test.rs` and other test files.** Update test fixtures that construct `SyntaxTable` directly to construct a chartable via `make-char-table` and pass the Value.

- [ ] **Step 2.6: `cargo nextest run -p neovm-core syntax_gnu_parity_regression` — must still pass.** Then run the full syntax test suite (minus the pre-existing `vm_syntax_table_accessors_use_shared_current_buffer_state` SIGABRT) and confirm green.

**Commit:** `syntax: route motion/parse through chartable readers`

---

## Task 3: Delete the compiled SyntaxTable

**File:** `neovm-core/src/emacs_core/syntax.rs`, `neovm-core/src/buffer/buffer.rs`, `neovm-core/src/emacs_core/eval.rs`

- [ ] **Step 3.1: Delete `pub struct SyntaxTable`** and all its impls (`new_standard`, `make_syntax_table`, `copy_syntax_table`, `get_entry`, `char_syntax`, `modify_syntax_entry`, `from_dump`, `dump_entries`, `dump_parent`, `Clone`, `Default`, `source_bits`). Keep `SyntaxClass`, `SyntaxEntry`, `SyntaxFlags`, `syntax_entry_from_chartable_entry`.

- [ ] **Step 3.2: Delete `syntax_table_from_chartable`, `apply_compiled_syntax_entry`, `sync_current_buffer_syntax_table_state`.** Remove the `sync_current_buffer_syntax_table_state` call from `sync_current_buffer_runtime_state` in `eval.rs`. Case-table sync stays.

- [ ] **Step 3.3: Delete `Buffer.syntax_table` field.** Remove initialization in `Buffer::default`, `Buffer::new_with_text`, and any `set_buffer_syntax_table` helpers. The chartable in `buf.slots[BUFFER_SLOT_SYNTAX_TABLE]` is now the only storage — matches GNU `bset_syntax_table`.

- [ ] **Step 3.4: Delete `Context::standard_syntax_table` compiled form if present.** The standard syntax table lives as a chartable Value only, mirroring GNU `Vstandard_syntax_table`.

- [ ] **Step 3.5: `cargo check -p neovm-core`** — fix any straggler call sites.

- [ ] **Step 3.6: `cargo nextest run -p neovm-core` (minus pre-existing SIGABRT).** All green.

**Commit:** `syntax: delete compiled SyntaxTable, chartable is the only runtime form`

---

## Task 4: pdump format bump v23→v24

**File:** `neovm-core/src/emacs_core/pdump/types.rs`, `neovm-core/src/emacs_core/pdump/convert.rs`, `neovm-core/src/emacs_core/pdump/mod.rs`

- [ ] **Step 4.1: Delete `DumpSyntaxTable` and `DumpSyntaxEntry` structs** from `types.rs`.

- [ ] **Step 4.2: Delete `dump_syntax_table` / `load_syntax_table`** from `convert.rs`. Remove the `syntax_table: dump_syntax_table(...)` line from `DumpBuffer` serialization. The buffer's `slots[BUFFER_SLOT_SYNTAX_TABLE]` entry already carries the chartable Value through the standard Value path.

- [ ] **Step 4.3: Bump `FORMAT_VERSION` v23→v24** in `pdump/mod.rs`. Update any version-gated load logic.

- [ ] **Step 4.4: Regenerate dump** — `cargo xtask fresh-build`, verify `xxd target/debug/neomacs.pdump | head -1` shows `18 00 00 00` (v24).

- [ ] **Step 4.5: Full pdump test suite.** `cargo nextest run -p neovm-core pdump` — all green.

**Commit:** `pdump: bump v23→v24, drop DumpSyntaxTable`

---

## Task 5: Revert memoization

**File:** `neovm-core/src/emacs_core/syntax.rs`

- [ ] **Step 5.1: Revert commit `d18db589d` (the `source_bits` memoization).** After T3 deletes `SyntaxTable` entirely, the memoization is gone anyway; this task is a no-op checkpoint confirming nothing from the memoization commit survived.

- [ ] **Step 5.2: Grep for `source_bits`** — should return zero hits.

- [ ] **Step 5.3: Run the TUI repro** that triggered the original hang: `timeout 10 ./target/debug/neomacs -nw --init-directory=/tmp/nearly`. With GNU-style on-demand reads, the 4th-redisplay fontification must progress (not necessarily fast — there may be separate perf issues — but not spin in `syntax_table_from_chartable`).

---

## Done criteria

1. `rg 'struct SyntaxTable' neovm-core/src/` returns zero hits.
2. `rg 'syntax_table_from_chartable\|apply_compiled_syntax_entry\|sync_current_buffer_syntax_table_state' neovm-core/src/` returns zero hits.
3. `rg 'buf\.syntax_table\|Buffer.*syntax_table:' neovm-core/src/` returns zero hits outside tests that reference `slots[BUFFER_SLOT_SYNTAX_TABLE]`.
4. `cargo nextest run -p neovm-core` — same pass rate as before (pre-existing SIGABRT unchanged).
5. `cargo xtask fresh-build` — produces v24 pdump; `target/debug/neomacs -nw` reaches the command loop.
6. T0 regression tests still green.
