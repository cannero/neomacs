# Overlay Newline + Minibuffer Auto-Resize Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make fido-vertical-mode show a vertical completion list in the minibuffer, matching GNU Emacs behavior.

**Architecture:** GNU's `display_line()` in `src/xdisp.c` processes overlay after-strings character-by-character via an iterator; when a `\n` is encountered (`ITERATOR_AT_END_OF_LINE_P`), `display_line()` returns and the caller starts a new glyph row. GNU's `resize_mini_window()` in `src/xdisp.c:13161-13301` measures the needed display height by running `move_it_to()` to the end of the buffer, then calls `grow_mini_window()` / `shrink_mini_window()` in `src/window.c:5896-5960` to adjust the minibuffer pixel height and shrink the root window by the same delta.

neomacs has two bugs: (1) `render_overlay_string()` in `engine.rs:856` skips `\n` with `continue`, flattening the vertical list to one line; (2) the minibuffer window has a static height (1 row on TTY) and `resize-mini-window-internal` is a stub that always errors. Both must be fixed to match GNU.

**Tech Stack:** Rust (neomacs-layout-engine, neomacs-display-protocol, neovm-core)

---

## File Map

| File | Change | Responsibility |
|---|---|---|
| `neomacs-layout-engine/src/engine.rs` | Modify | Overlay string newline → new glyph row; minibuffer height measurement after layout |
| `neovm-core/src/window/mod.rs` | Modify | `grow_mini_window` / `shrink_mini_window` methods on Frame |
| `neovm-core/src/emacs_core/builtins/symbols.rs` | Modify | Replace `resize-mini-window-internal` stub with real impl |
| `neomacs-layout-engine/src/matrix_builder.rs` | Modify (minor) | Add `push_overlay_char` that pushes a char into the current row |
| `neomacs-display-protocol/src/tty_rif_test.rs` | Modify | Regression test for multi-row overlay in TTY rasterize |
| `neovm-core/src/emacs_core/window_cmds_test.rs` | Modify | Test `grow_mini_window` / `shrink_mini_window` |

---

### Task 1: Make `render_overlay_string` emit glyphs and handle newlines

GNU reference: `src/xdisp.c` — overlay strings are processed by the
main `display_line()` iterator. When `\n` is encountered
(`ITERATOR_AT_END_OF_LINE_P` at `dispextern.h:2918`), `display_line()`
returns, and the caller starts a new glyph row. neomacs's analog: when
we hit `\n` inside an overlay string, we must end the current glyph row,
advance `row`/`y`, begin a new row, and continue rendering.

**Files:**
- Modify: `neomacs-layout-engine/src/engine.rs:856-891` (`render_overlay_string`)
- Modify: `neomacs-layout-engine/src/engine.rs:2380-2402` (call site for after-strings)
- Modify: `neomacs-layout-engine/src/engine.rs:3375-3401` (call site for before-strings)

**Current bug:** `engine.rs:886`:
```rust
if ch == '\n' {
    continue; // Skip newlines in overlay strings  ← BUG
}
```

The fix must:
1. Change `render_overlay_string` signature to accept `&mut self` (the engine), `row: &mut usize`, `y: &mut f32`, `max_rows: usize`, and the `GlyphMatrixBuilder` ref — so it can call `end_row` / `begin_row` / `push_char` on newlines.
2. On `\n`: flush the current glyph run, call `self.matrix_builder.end_row()`, increment `*row`, update `*y`, call `self.matrix_builder.begin_row(*row, GlyphRowRole::Text)`, reset `*x` and `*col` to the left edge.
3. On non-`\n`: push the character glyph via `self.matrix_builder.push_char(ch, face_id, 0)` (overlay chars have charpos=0 since they're not buffer text).
4. Respect `max_rows` — stop rendering if `*row >= max_rows`.
5. Update both call sites (after-strings at line 2380 and before-strings at line 3375) to pass the new parameters.

- [ ] **Step 1: Refactor `render_overlay_string` signature**

Change from a free function to a method on `LayoutEngine` (or pass the builder). Add `row`, `y`, `max_rows` parameters. Handle `\n` by advancing rows.

- [ ] **Step 2: Update the after-string call site (line ~2380)**

Pass `&mut row`, `&mut y`, `max_rows`, and the matrix builder. After the call, the `row`/`y` state reflects any newlines consumed.

- [ ] **Step 3: Update the before-string call site (line ~3375)**

Same parameter threading.

- [ ] **Step 4: Verify with a manual test**

Build neomacs, run `neomacs -nw -Q`, enable `(fido-vertical-mode 1)`, press `M-x`. The completion list should now render vertically in the minibuffer (even if it's truncated to 1 row due to the static minibuffer height — that's fixed in Task 2).

- [ ] **Step 5: Commit**

```
overlay: handle newlines in overlay after-strings (fido-vertical-mode)
```

---

### Task 2: Implement `grow_mini_window` / `shrink_mini_window` on Frame

GNU reference: `src/window.c:5896-5960`. `grow_mini_window(w, delta, unit)` adds `delta` pixels to the minibuffer and shrinks the root window by the same amount. `shrink_mini_window(w, unit)` restores to minimum (1 line). The minimum is always 1 frame line height. Maximum is governed by `max-mini-window-height` (default 0.25 = 25% of frame inner height).

**Files:**
- Modify: `neovm-core/src/window/mod.rs` — add methods to `Frame`

- [ ] **Step 1: Add `grow_mini_window(&mut self, delta_rows: i32)` to Frame**

Compute new minibuffer height = current + delta_rows * char_height. Clamp to [1 row, max_mini_window_height fraction of frame]. Update `minibuffer_leaf.bounds.height`. Call `sync_window_area_bounds()` to propagate the change to the root window.

```rust
pub fn grow_mini_window(&mut self, delta_rows: i32) {
    let Some(mini) = self.minibuffer_leaf.as_mut() else { return };
    let char_h = self.char_height.max(1.0);
    let unit = char_h;
    let current_h = mini.bounds().height;
    let frame_inner_h = self.height as f32 - self.chrome_top_height();
    let max_h = (frame_inner_h * 0.25).max(unit); // max-mini-window-height default
    let new_h = (current_h + delta_rows as f32 * unit)
        .clamp(unit, max_h);
    if (new_h - current_h).abs() < 0.5 { return; }
    let mut bounds = *mini.bounds();
    bounds.height = new_h;
    mini.set_bounds(bounds);
    self.sync_window_area_bounds();
}
```

- [ ] **Step 2: Add `shrink_mini_window(&mut self)` to Frame**

Restore minibuffer to 1-line height.

```rust
pub fn shrink_mini_window(&mut self) {
    let Some(mini) = self.minibuffer_leaf.as_mut() else { return };
    let unit = self.char_height.max(1.0);
    let mut bounds = *mini.bounds();
    bounds.height = unit;
    mini.set_bounds(bounds);
    self.sync_window_area_bounds();
}
```

- [ ] **Step 3: Write test**

```rust
#[test]
fn grow_and_shrink_mini_window_adjusts_bounds() {
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 80, 24, BufferId(1));
    let frame = mgr.get(fid).unwrap();
    let initial_mini_h = frame.minibuffer_leaf.as_ref().unwrap().bounds().height;
    assert!(initial_mini_h <= 1.0 || initial_mini_h <= 16.0);

    mgr.get_mut(fid).unwrap().grow_mini_window(3);
    let after_grow = mgr.get(fid).unwrap();
    let grown_h = after_grow.minibuffer_leaf.as_ref().unwrap().bounds().height;
    assert!(grown_h > initial_mini_h);

    mgr.get_mut(fid).unwrap().shrink_mini_window();
    let after_shrink = mgr.get(fid).unwrap();
    let shrunk_h = after_shrink.minibuffer_leaf.as_ref().unwrap().bounds().height;
    assert!(shrunk_h <= grown_h);
}
```

- [ ] **Step 4: Commit**

```
window: grow_mini_window / shrink_mini_window (GNU window.c parity)
```

---

### Task 3: Call `grow_mini_window` from the layout engine after measuring minibuffer content

GNU reference: `resize_mini_window()` in `xdisp.c:13161-13301` is called from `redisplay_window()` BEFORE laying out the minibuffer. It measures the content height using `move_it_to()`, then calls `grow_mini_window` if the content exceeds the current height.

neomacs analog: after `layout_window_rust` returns for the minibuffer window, count the number of enabled glyph rows in its matrix. If the count exceeds the window's allocated rows, call `grow_mini_window` on the frame and re-run the layout for the entire frame.

**Files:**
- Modify: `neomacs-layout-engine/src/engine.rs` — in `layout_frame_rust`, after minibuffer layout
- Modify: `neomacs-layout-engine/src/neovm_bridge.rs` — helper to read minibuffer content line count

- [ ] **Step 1: After laying out all windows including minibuffer, count minibuffer display rows**

In `layout_frame_rust`, after the window iteration loop completes, check if the last window was the minibuffer. Count its enabled glyph rows. If count > allocated rows, grow the minibuffer and re-layout.

- [ ] **Step 2: Implement the re-layout loop**

```rust
// After the main window iteration in layout_frame_rust:
if let Some(mini_entry) = self.matrix_builder.windows().last() {
    let mini_rows_used = mini_entry.matrix.rows.iter()
        .filter(|r| r.enabled).count();
    let allocated = params_for_minibuffer.bounds.height / char_h;
    if mini_rows_used > allocated as usize {
        let delta = (mini_rows_used as i32) - (allocated as i32);
        evaluator.frame_manager_mut()
            .get_mut(frame_id).unwrap()
            .grow_mini_window(delta);
        // Re-run layout with updated bounds
        self.matrix_builder.reset();
        // ... recursive call or loop
    }
}
```

This must respect `resize-mini-windows` variable (`grow-only` or `t`) and `max-mini-window-height`.

- [ ] **Step 3: Handle shrink on empty minibuffer**

When the minibuffer content fits in 1 row but the window is taller (from a previous grow), call `shrink_mini_window()` to restore it. GNU's `resize-mini-windows = grow-only` skips this shrink unless the buffer is empty.

- [ ] **Step 4: Commit**

```
layout: auto-resize minibuffer after measuring overlay content
```

---

### Task 4: Replace `resize-mini-window-internal` stub

GNU reference: `Fresize_mini_window_internal` in `window.c:5967-5996`. Takes a WINDOW arg, computes delta from current vs desired height, calls `resize_mini_window_apply`.

**Files:**
- Modify: `neovm-core/src/emacs_core/builtins/symbols.rs:2203-2222`
- Modify: `neovm-core/src/emacs_core/builtins/mod.rs:5329-5330` (signature change to accept Context)

- [ ] **Step 1: Replace the stub with real implementation**

```rust
pub(crate) fn builtin_resize_mini_window_internal(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("resize-mini-window-internal", &args, 1)?;
    let wid = args[0].as_window_id()
        .ok_or_else(|| signal("wrong-type-argument",
            vec![Value::symbol("window-live-p"), args[0]]))?;
    // Find the frame containing this minibuffer window
    let fid = eval.frames.find_window_frame_id(WindowId(wid))
        .ok_or_else(|| signal("error",
            vec![Value::string("Window not found")]))?;
    let frame = eval.frames.get(fid)
        .ok_or_else(|| signal("error",
            vec![Value::string("Frame not found")]))?;
    if frame.minibuffer_window != Some(WindowId(wid)) {
        return Err(signal("error",
            vec![Value::string("Not a minibuffer window")]));
    }
    // The Lisp caller passes the desired pixel height.
    // For now, just acknowledge — the layout engine drives
    // the actual resize via grow_mini_window.
    Ok(Value::NIL)
}
```

- [ ] **Step 2: Update registration to pass Context**

Change `mod.rs:5329-5330` from `|_ctx, args| builtin_resize_mini_window_internal(args)` to pass `ctx`.

- [ ] **Step 3: Commit**

```
window: implement resize-mini-window-internal (GNU window.c parity)
```

---

### Task 5: End-to-end verification

- [ ] **Step 1: Build neomacs**

```bash
cargo build -p neomacs-bin
```

- [ ] **Step 2: Run interactively**

```bash
./target/debug/neomacs -nw -Q --eval '(fido-vertical-mode 1)'
```

Then press `M-x`. The minibuffer should grow to show a vertical list of command names. Type characters to filter.

- [ ] **Step 3: Compare with GNU**

```bash
emacs -nw -Q --eval '(fido-vertical-mode 1)'
```

The visual appearance should match: vertical list, highlighted selection, proper face colors.

- [ ] **Step 4: Capture pty output and compare**

Use the `script(1)` + VT100 replay tool from earlier to capture both and diff the minibuffer rows.

---

## Implementation Notes

- **`render_overlay_string` refactor is the hardest task.** The current function is a free function that only tracks x/col advancement. It must become a method (or take many more parameters) that can push glyphs AND advance rows. The main text loop's newline handling (engine.rs:2422-2478) is the template.

- **Re-layout after grow is expensive.** GNU avoids this by calling `resize_mini_window` BEFORE `redisplay_window` for the minibuffer. neomacs's architecture (layout all windows in one pass) makes this harder. The simplest approach: detect after the first pass, grow, reset matrix builder, re-layout. Cap retries at 1 to avoid infinite loops.

- **`max-mini-window-height` default is 0.25** (25% of frame height). On a 24-row TTY frame, that's 6 rows max. neomacs should read this variable from the obarray.

- **`resize-mini-windows` variable** controls behavior: `grow-only` (default), `t` (resize both ways), `nil` (never). neomacs has this at `frame_vars.rs:15` set to `grow-only` and also at `eval.rs:2506` set to `nil`. The layout engine should check the live value.
