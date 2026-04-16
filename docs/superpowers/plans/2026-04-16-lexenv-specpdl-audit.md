# Lexenv Specpdl Audit: Neomacs vs GNU Emacs

## Principle
GNU Emacs uses `specbind(Qinternal_interpreter_environment, ...)` for ALL lexenv
save/restore. `unbind_to(count)` automatically restores. Neomacs must match exactly.

## Audit of ALL self.lexenv modification sites

### 1. Line 5973/5976/5990 — set_lexical_binding / clear_top_level_eval_state
**Purpose:** Toggle lexical-binding mode at top level
**GNU equivalent:** Setting `Vinternal_interpreter_environment` to `(t)` or nil
**Status:** OK — these are initialization, not inside eval_sub

### 2. Line 6993 — defvaralias replace_alias_refs_in_value
**Purpose:** Update lexenv entries when a variable is aliased
**GNU equivalent:** defvaralias modifies Vinternal_interpreter_environment directly
**Status:** OK — structural modification, not a binding operation

### 3. Line 7392 — sf_let_value_named (let)
**Purpose:** Install new lexenv for let body
**GNU equivalent:** eval.c:1188 `specbind(Qinternal_interpreter_environment, lexenv)`
**Current neomacs:** `self.lexenv = new_lexenv` + LexicalEnv on specpdl
**Status:** PARTIALLY FIXED — builds locally, swaps atomically, specpdl save at entry.
But uses `lexenv_at_entry` captured before init loop; should use the specpdl entry
pushed before init forms run.

### 4. Line 7476 — sf_let_star_value_named (let*) calls bind_lexical_value_rooted
**Purpose:** Add each lexical binding to lexenv sequentially
**GNU equivalent:** eval.c:1113-1120 — ONE specbind for first binding, then direct assign
**Current neomacs:** Calls bind_lexical_value_rooted_in_specpdl which modifies self.lexenv
directly AND pushes/pops a temporary GcRoot
**Status:** BUG — should match GNU: one specbind for first binding (already done at line 7431),
then direct `self.lexenv = cons(binding, self.lexenv)` for each. The GcRoot
push/pop in bind_lexical_value_rooted_in_specpdl is unnecessary if we root
the cons cells differently.

### 5. Line 7759 — sf_defvar_value (defvar without init value)
**Purpose:** Add dynamic-binding marker to lexenv
**GNU equivalent:** eval.c:1018 — direct `Vinternal_interpreter_environment = Fcons(sym, ...)`
**Status:** OK — matches GNU exactly (direct assignment, no specbind needed)

### 6. Line 8046 — sf_condition_case_value_named error handler
**Purpose:** Bind error variable in handler body
**GNU equivalent:** eval.c condition-case handler creates a new lexenv binding
**Current neomacs:** Pushes LexicalEnv + calls bind_lexical_value_rooted
**Status:** NEEDS FIX — should build locally and assign, like let. The LexicalEnv push
is correct for save; the bind_lexical_value_rooted call should be replaced with
direct cons + assign.

### 7. Lines 10319/10331/10339 — unbind_to LexicalEnv/UnwindProtect arms
**Purpose:** Restore lexenv during specpdl unwinding
**Status:** OK — these ARE the restore mechanism

### 8. Line 1810 (begin_lambda_call_in_state) — lambda call
**Purpose:** Set lexenv to closure's captured env for body execution
**GNU equivalent:** eval.c:3416 `specbind(Qinternal_interpreter_environment, lexenv)`
**Current neomacs:** `std::mem::replace(lexenv, env)` + LexicalEnv on specpdl
**Status:** OK — functionally equivalent to GNU's specbind

### 9. load.rs lines 1089/1094/1096 — with_load_context
**Purpose:** Set lexenv to (t) for file loading
**GNU equivalent:** lread.c:2220 `specbind(Qinternal_interpreter_environment, (t))`
**Current neomacs:** specpdl-based (just fixed)
**Status:** FIXED — now matches GNU

## Root cause of remaining leak

`bind_lexical_value_rooted_in_specpdl` at line 1721 does:
```rust
*lexenv = Value::make_cons(binding, *lexenv);
```

This directly mutates `self.lexenv`. In `let*` (line 7476), multiple calls accumulate
bindings. The LexicalEnv at line 7431 saves the pre-let* lexenv. After body execution,
unbind_to restores to the pre-let* value.

BUT: if the body's execution (or init form evaluation) triggers `cconv-make-interpreted-closure`
→ `macroexpand-all` → which evaluates Elisp that creates more `let*` forms → those nested
let* forms add to self.lexenv via bind_lexical_value_rooted → the outer let*'s LexicalEnv
restore puts back the pre-outer-let* value, but the INNER let*'s modifications persist
until the inner let*'s unbind_to runs.

The issue: between the inner let*'s binding phase and its unbind_to, the self.lexenv has
extra entries. If during this window, another closure is created, it captures these extras.

## Fix plan

Replace `bind_lexical_value_rooted_in_specpdl` with direct cons + assign in all callers:

1. **let** (sf_let_value_named): DONE — already builds locally
2. **let*** (sf_let_star_value_named): Change to match GNU eval.c:1113-1120:
   - One specbind for first lexical binding (already done)
   - Direct `self.lexenv = cons(binding, self.lexenv)` for each binding
   - No GcRoot push/pop needed
3. **condition-case** (sf_condition_case_value_named): Build locally, assign once
4. **Remove** `bind_lexical_value_rooted_in_specpdl` and `bind_lexical_value_rooted`

After this, the only function that adds pairs to lexenv is direct `self.lexenv = cons(...)`,
always protected by a LexicalEnv entry on the specpdl.
