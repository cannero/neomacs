# Plan: Unify .el and .elc Loading into One readevalloop

## Current State (NeoVM)

Two completely separate functions:

```
load_file_body()           →  .el path  (lines 1650-2080, ~430 lines)
load_elc_file_body()       →  .elc path (lines 2090-2200, ~110 lines)
```

They share nothing. Each has its own file reading, lexical-binding detection,
load-file-name binding, form iteration loop, error handling, load-history
recording.

## Target State (matching GNU Emacs)

One function: `readevalloop`. The ONLY differences between .el and .elc are:

1. **Header**: .elc has `;ELC` magic header to skip. .el does not.
2. **Reify**: .elc forms may contain `byte-code-literal` AST nodes that need
   conversion to `Value::ByteCode`. .el forms never have these.
3. **Macroexpand flag**: .el gets eager macroexpansion. .elc does not (macros
   already compiled away).

Everything else is identical: file reading, encoding detection,
lexical-binding cookie, load-file-name binding, `loads-in-progress` tracking,
form-by-form eval, GC safe points, load-history recording, `eval-after-load`
hooks.

## GNU Emacs Reference

GNU Emacs `lread.c`:
```c
Fload()
  -> open file, detect .elc
  -> readevalloop()     // ONE function, both paths
       -> read form     // SAME reader for .el and .elc
       -> if (macroexpand) eager_expand_eval(val)
          else eval_sub(val)
```

The ONLY difference is one flag (line 2187):
```c
if (suffix_p(sourcename, ".elc"))
    macroexpand = Qnil;   // .elc: skip macroexpansion
```

Then in the loop (line 2338):
```c
if (!NILP(macroexpand))
    val = readevalloop_eager_expand_eval(val, macroexpand);  // .el
else
    val = eval_sub(val);  // .elc
```

Everything else is shared: reader, eval, GC, load-history.

## Implementation Steps

### Step 1: Create readevalloop function

```rust
fn readevalloop(
    ctx: &mut Context,
    path: &Path,
    content: &str,      // already read and header-skipped
    is_elc: bool,
) -> Result<Value, EvalError> {
    // 1. Detect lexical-binding from file content
    //    (same logic for both -- cookie is in first line/comment)

    // 2. Save/restore load context
    //    load-file-name, lexical-binding, lexenv
    //    (identical for both)

    // 3. Parse forms
    let forms = parse_forms(content)?;
    //    Parser handles #[...], #@N, #$ transparently for both
    //    (.el files just never contain these reader syntaxes)

    // 4. Get macroexpand function (ONE flag difference)
    let macroexpand_fn = if !is_elc {
        get_eager_macroexpand_fn(ctx)
    } else {
        None
    };

    // 5. Form-by-form eval loop (SHARED)
    let mut last = Value::Nil;
    for form in &forms {
        // 5a. Reify byte-code literals
        //     No-op for .el (no byte-code-literal nodes exist)
        let form = ctx.reify_byte_code_literals(form)?;

        // 5b. Eval with optional eager macroexpansion
        last = if let Some(ref expand_fn) = macroexpand_fn {
            eager_expand_eval(ctx, form_to_value(&form), *expand_fn)?
        } else {
            ctx.eval(&form).map_err(map_flow)?
        };

        // 5c. GC safe point (shared)
        ctx.gc_safe_point();
    }

    // 6. Record load-history (shared)
    // 7. Run eval-after-load hooks (shared)
    // 8. Restore load context (shared)

    Ok(last)
}
```

### Step 2: Simplify load_file_body

```rust
fn load_file_body(ctx: &mut Context, path: &Path) -> Result<Value, EvalError> {
    let is_elc = path.extension() == Some("elc");

    // NeoVM-specific: try .neobc cache for .el files
    if !is_elc {
        if let Some(result) = try_load_neobc_cache(ctx, path)? {
            return Ok(result);
        }
    }

    // Read file
    let raw_bytes = std::fs::read(path)?;

    // Skip .elc header if needed
    let content = if is_elc {
        skip_elc_header(&raw_bytes)
    } else {
        decode_emacs_utf8(&raw_bytes)
    };

    // Shared eval loop
    readevalloop(ctx, path, &content, is_elc)
}
```

### Step 3: Delete load_elc_file_body entirely

Replaced by `readevalloop` with `is_elc=true`.

### Step 4: Move shared load context management into readevalloop

Both paths currently do the same save/restore dance:
- Save `load-file-name`, `lexical-binding`, `lexenv`
- Set `load-file-name` to current path
- Detect lexical-binding cookie
- Push to `loads-in-progress`
- On return: restore everything, pop `loads-in-progress`

This all moves into `readevalloop`.

## Why reify_byte_code_literals exists

NeoVM-specific step not in GNU Emacs. Reason:

- GNU Emacs reader produces `Lisp_Object` values directly. When it reads
  `#[arglist bytecodes constants maxdepth]`, it creates a bytecode object
  during the read phase. No post-processing needed.

- NeoVM reader produces `Expr` AST nodes. When it reads `#[...]`, it creates
  `Expr::List(["byte-code-literal", vector])`. This must be converted to
  `Value::ByteCode` before evaluation. The `reify_byte_code_literals` pass
  does this conversion.

For .el files, this pass is a no-op because .el files never contain
`byte-code-literal` AST nodes. So it can safely be called for both paths.

## Why eval is already shared

NeoVM's tree-walking evaluator (`ctx.eval()`) already handles macros inline:
when it encounters `(some-macro args...)`, it checks the obarray, finds the
macro definition, and expands it. The `eager_expand_eval` in the .el path
is an ADDITIONAL optimization -- expanding macros at the top level so they
are expanded once instead of at every call site.

For .elc files, macros are compiled away, so `ctx.eval()` finds no macros
and the inline expansion is a no-op. The same `eval` works for both paths.

## Lines affected

| Current | Action |
|---|---|
| `load_file_body` (~430 lines) | Slim to ~20 lines (prepare + call readevalloop) |
| `load_elc_file_body` (~110 lines) | Delete entirely |
| New `readevalloop` (~80 lines) | Shared loop extracted from both |
| Shared context save/restore (~60 lines) | Deduplicated (was in both functions) |

Estimated net: -400 lines

## What about the .neobc cache?

NeoVM has a pre-compiled `.neobc` cache for .el files. This is NeoVM-specific
(not in GNU Emacs). It stays as a pre-check BEFORE `readevalloop` in
`load_file_body`. If the cache hits, `readevalloop` is never called.

## Risk

Low. The two paths already do the same things in the same order. Unifying
them removes duplication without changing behavior. The only risk is subtle
ordering differences between the two current implementations that might
surface as regressions -- test with the full bootstrap after merging.
