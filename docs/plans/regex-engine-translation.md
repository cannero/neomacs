# Regex Engine Translation Plan: GNU regex-emacs.c → Rust

## Goal

Replace NeoVM's 4-engine regex system with a single Rust translation of GNU Emacs's
`regex-emacs.c`, achieving 100% semantic compatibility with GNU's regex/search behavior.

## Why

NeoVM currently uses 4 separate engines (Literal, Segmented, Backref NFA, Rust regex crate)
with a translation layer that converts Emacs regex syntax to Rust regex syntax. This approach
has fundamental limitations:

- No syntax-table integration (`\w`, `\b`, `\s` ignore buffer syntax table)
- Symbol boundaries `\_<` `\_>` incorrectly map to word boundaries `\b`
- Character categories `\cc` are stubs
- POSIX vs non-POSIX backtracking is not differentiated
- Cache key doesn't include syntax table (stale matches after mode change)
- Rust regex crate has different Unicode semantics than GNU

GNU's design uses ONE engine that queries the syntax table during matching. Translating
it to Rust gives us identical behavior with no translation layer bugs.

## Source Files

### GNU Emacs (C source to translate)
- `src/regex-emacs.c` — 5355 lines
  - `regex_compile()` (line 1710-3400): Compiler — pattern → bytecode (~1690 lines)
  - `re_search_2()` (line 3408-4070): Searcher — find match in text (~663 lines)
  - `re_match_2_internal()` (line 4072-5340): Matcher — execute bytecode (~1269 lines)
  - Opcodes enum (line 200-340): Bytecode instruction set
  - Helpers (line 340-1710): Char classes, fastmap, macros
- `src/regex-emacs.h` — Data structures (re_pattern_buffer, re_registers)
- `src/search.c` — 3514 lines
  - `compile_pattern()` (line 199): Cache lookup + compile
  - `search_buffer()` (line 1518): Buffer search coordination
  - `Fstring_match()`, `Flooking_at()`, etc.: Elisp builtins
  - `Fmatch_data()`, `Fset_match_data()`: Match data management
  - `Freplace_match()`: Replacement with group references

### NeoVM (Rust files to replace/modify)
- `neovm-core/src/emacs_core/regex.rs` — 3321 lines (REPLACE most of this)
- `neovm-core/src/emacs_core/search.rs` — 506 lines (MODIFY to use new engine)
- `neovm-core/src/emacs_core/builtins/search.rs` — (KEEP, rewire to new engine)

## Bytecode Instruction Set (from regex-emacs.c)

```
exactn N C1..CN          — Match N exact characters
anychar                  — Match any character (except newline)
charset BITMAP           — Match character in bitmap
charset_not BITMAP       — Match character NOT in bitmap
start_memory N           — Start of group N
stop_memory N            — End of group N
duplicate N              — Match same text as group N (backreference)
begline / endline        — ^ / $
begbuf / endbuf          — \` / \'
wordbeg / wordend        — \< / \>  (syntax-table aware)
wordbound / notwordbound — \b / \B  (syntax-table aware)
syntaxspec C             — \sC      (syntax-table aware)
notsyntaxspec C          — \SC      (syntax-table aware)
categoryspec C           — \cC      (category-table aware)
notcategoryspec C        — \CC      (category-table aware)
on_failure_jump OFF      — Push failure point, continue
jump OFF                 — Unconditional jump
on_failure_jump_loop     — Loop variant (prevents infinite empty matches)
on_failure_jump_smart    — Greedy * and + optimization
succeed_n N OFF          — Counted repetition \{n,m\}
jump_n N OFF             — Counted jump
set_number_at OFF N      — Set counter for repetition
```

## Translation Phases

### Phase 1: Data Structures & Opcodes

**New file**: `neovm-core/src/emacs_core/regex_emacs.rs`

Translate:
- `re_opcode_t` enum → Rust `enum RegexOp`
- `re_pattern_buffer` → Rust `struct CompiledPattern`
- `re_registers` → Rust `struct MatchRegisters`
- Fastmap array → `[bool; 256]`
- Character class helpers

Estimated: ~200 lines of Rust

### Phase 2: Compiler (regex_compile)

Translate `regex_compile()` (lines 1710-3400 of regex-emacs.c):
- Parse Emacs regex syntax character by character
- Emit bytecode ops into a Vec<u8> buffer
- Handle: groups, alternation, repetition, character classes
- Handle: syntax specs (\sw, \s-), category specs (\cc)
- Handle: backreferences (\1-\9)
- Build fastmap for fast rejection

Key C patterns to translate:
- `PATFETCH(c)` macro → iterator over pattern chars
- `BUF_PUSH(op)` macro → `bytecode.push(op)`
- `STORE_JUMP(op, loc, to)` → write jump offset at location
- `INSERT_JUMP(op, loc, to)` → insert jump, shift subsequent bytes
- `GET_BUFFER_SPACE(n)` → Vec auto-grows (not needed in Rust)

Estimated: ~1200 lines of Rust (C has more boilerplate)

### Phase 3: Matcher (re_match_2_internal)

Translate `re_match_2_internal()` (lines 4072-5340):
- Execute bytecode against text (string or buffer)
- Backtracking via failure stack
- Query syntax table for wordbeg/wordend/syntaxspec
- Query category table for categoryspec
- Track group start/end positions in registers

Key C patterns to translate:
- `PREFETCH()` macro → bounds-checked char access
- `PUSH_FAILURE_POINT()` → push to Vec-based failure stack
- `POP_FAILURE_POINT()` → pop from failure stack
- `SYNTAX(c)` → query buffer's syntax table
- `CATEGORY_MEMBER(c, cat)` → query category table

Estimated: ~1000 lines of Rust

### Phase 4: Searcher (re_search_2)

Translate `re_search_2()` (lines 3408-4070):
- Use fastmap to skip non-matching positions
- Call re_match_2_internal at each candidate position
- Handle forward and backward search
- Handle anchored patterns (^, \`)

Estimated: ~500 lines of Rust

### Phase 5: Integration

Modify existing files:
- Replace `compile_search_pattern()` to use new compiler
- Replace match execution to use new matcher
- Keep existing MatchData struct but populate from MatchRegisters
- Keep cache but add syntax_table to cache key
- Keep all existing builtins (search.rs, builtins/search.rs) — just rewire internals

### Phase 6: Cleanup

- Delete old code: `translate_emacs_regex()`, `BackrefParser`, `SegmentedPattern`
- Remove `regex` crate dependency (for regex matching — may keep for other uses)
- Delete ~2000 lines of old regex.rs code
- Run full test suite

## Key Translation Decisions

### 1. Text Access
GNU uses `re_char *` pointers with `PREFETCH()` macro.
Rust: Use `&[u8]` slices with bounds checking. For buffer text, use the gap buffer's
contiguous view or copy to a temporary buffer.

### 2. Syntax Table Queries
GNU: `SYNTAX(c)` macro reads from `gl_state.current_syntax_table`.
Rust: Pass `&SyntaxTable` reference to matcher. Query `syntax_table.char_syntax(c)`.

### 3. Failure Stack
GNU: Uses alloca() with overflow to heap.
Rust: Use `Vec<FailurePoint>` — simpler, no manual memory management.

### 4. Multibyte Characters
GNU: Complex RE_STRING_CHAR / RE_STRING_CHAR_AND_LENGTH macros.
Rust: UTF-8 iteration with `char_indices()` or manual byte-level for performance.

### 5. Cache
GNU: Linked list of 20 `regexp_cache` entries, keyed by (pattern, translate, syntax_table, posix).
Rust: Keep existing LRU Vec cache, add syntax_table identity to key.

## Testing Strategy

1. Unit tests: Compare compiled bytecode for known patterns
2. Match tests: Compare match results for pattern+text pairs against GNU
3. Search tests: Compare re-search-forward/backward results
4. Batch test: Run Doom Emacs startup, check font-lock correctness
5. Oracle tests: Randomized pattern+text fuzzing against GNU Emacs

## Immediate Fixes (Before Translation)

While the translation is in progress, fix these quick issues:
1. `case-fold-search` default: change from `true` to `false` (1 line)
2. Fix `\_<` `\_>` comment to document known limitation
3. Add syntax-table cache key to existing cache (partial fix)

## Files Created/Modified

```
CREATE: neovm-core/src/emacs_core/regex_emacs.rs  (~3000 lines)
MODIFY: neovm-core/src/emacs_core/regex.rs         (delete ~2000, keep cache/helpers)
MODIFY: neovm-core/src/emacs_core/search.rs         (rewire to new engine)
KEEP:   neovm-core/src/emacs_core/builtins/search.rs (unchanged API)
KEEP:   neovm-core/src/emacs_core/isearch.rs         (unchanged, uses search.rs)
```

## Estimated Total

- New Rust code: ~3000 lines
- Deleted old code: ~2000 lines
- Net: +1000 lines
- Time: 3-5 focused days
