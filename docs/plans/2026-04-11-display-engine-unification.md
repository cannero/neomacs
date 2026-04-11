# Display Engine Unification: Problem & Plan

**Date:** 2026-04-11
**Status:** Proposal — needs review before any code lands
**Author:** working session with @eval-exec on neomacs `main`

## Revision history

- **Rev 3 (2026-04-11, later afternoon)** — second round of teammate review. Four more cleanup corrections; no structural changes to the plan. The broad conclusion and refactor steps are unchanged from Rev 2. Specific corrections:
  1. Rev 2 still referred in two places to "Lisp `format-mode-line`" being called by the layout engine. This is imprecise. `engine.rs::eval_status_line_format_value` at `engine.rs:83` explicitly **bypasses** the Lisp-facing `format-mode-line` builtin and calls `neovm-core/src/emacs_core/xdisp.rs::format_mode_line_for_display` at `xdisp.rs:189` directly. That function is a Rust entry point that mirrors GNU's `display_mode_line` with `MODE_LINE_DISPLAY` target, and the layout engine uses it specifically *because* the Lisp `format-mode-line` subr uses the `MODE_LINE_STRING` target which returns `"--"` for `%-` instead of filling with dashes. Rev 3 removes "Lisp" from these references and clarifies the Rust-level call path.
  2. Rev 2's TUI pipeline diagram and appendix said `render_rust_status_line_plain` at `engine.rs:2129` is "for Minibuffer echo only". Wrong — it is also called from `render_frame_tab_bar_rust` at `engine.rs:4729` for the frame tab-bar (`StatusLineKind::TabBar`). The function serves both paths. Rev 3 corrects the diagram and the appendix.
  3. Rev 2 named a nonexistent function `get_face_metrics` at `engine.rs:4659` in two places. The actual function at that site is `status_line_font_metrics` at `engine.rs:4652`. Rev 3 uses the correct name.
  4. Rev 2's Step 3 scoping note said the bidi iterator fields in GNU's `struct it` (`bidi_it`, `paragraph_embedding`, `bidi_p`) are "X-server-specific" and could be skipped when porting. This is factually wrong. Those fields are core iterator state used by `display_line` regardless of backend (verified against `dispextern.h:2591` and `dispextern.h:2891`). GNU has supported bidi text rendering in TTY mode since Emacs 24. Any port of `struct it` that needs to render bidi text must include them. Rev 3 removes the claim and replaces it with more careful triage guidance.

- **Rev 2 (2026-04-11, afternoon)** — first round of teammate review. Five factual errors and one fair challenge in Rev 1 have been fixed. The broad conclusion (`status_line.rs` is the divergence point, buffer text and mode-line take different paths, `parse_display_space_width` is incomplete) is unchanged. Specific corrections:
  1. Rev 1 said the two-pass `format-mode-line` path "flattens `propertize` properties into an intermediate string and loses them at that boundary". This is **wrong**. `neovm-core/src/emacs_core/xdisp.rs` preserves text properties correctly via `append_string_value_preserving_props` and `text_props.append_shifted` (lines 735/761/827/894). The loss happens later, in `neomacs-layout-engine/src/status_line.rs:772` (`build_rust_status_line_spec`) — specifically the harvester at line 803 only scans for `face` and `font-lock-face` intervals and never looks at `display`.
  2. Rev 1 named `render_rust_status_line_plain` as the mode-line rendering function. Wrong. `_plain` is the **minibuffer echo** path (`engine.rs:2129`). Mode-line, header-line, and tab-line all use `render_rust_status_line_value` (`engine.rs:4458/4498/4539`).
  3. Rev 1 said the GUI render thread "mostly just does glyph atlas lookup + GPU draw call submission". Wrong. `WgpuGlyphAtlas::get_or_create` at `neomacs-renderer-wgpu/src/glyph_atlas.rs:201` calls `rasterize_glyph` on cache miss, which invokes `SwashCache` to do real font rasterization + bearing computation. The GUI work split is **measurement on eval thread / rasterization on render thread**, not "all layout on eval / just submit on render".
  4. Rev 1 treated `StatusLineAdvanceMode::{Fixed, Measured}` as active duplication of the TUI/GUI selection in `status_line.rs`. Wrong. `StatusLineSpec::plain` at `status_line.rs:425` hardcodes `Fixed`; there is no live producer of `Measured`. The only `Measured` reference is a consumer match arm at `engine.rs:4689`. It's dormant scaffolding, not active behavior.
  5. Rev 1 claimed "bad colors and missing right-aligned content are the same bug". Unproven. The `:align-to` failure is proven by capture + code trace. The color-band observation is still a hypothesis — `build_rust_status_line_spec` at `status_line.rs:803` **does** preserve face runs for `face`/`font-lock-face` intervals, so if doom-modeline's dashboard segments happen to share a face, the single band is explained without any additional bug. Needs separate investigation.
  6. Rev 1 presented the `DisplayBackend` trait refactor as if it were the only correct fix. A smaller fix — teaching `build_rust_status_line_spec` to harvest `display` intervals and populate `align_entries`/`display_props` on the existing status-line path — is technically available. Rejecting it in favor of unification is a **design choice** following the "100% same architecture as GNU" directive, not a factual consequence of the audit. Rev 2 separates "what the audit proves" from "what we should do about it".

- **Rev 1 (2026-04-11, morning)** — original draft, contained errors listed above.

## TL;DR

Neomacs has two parallel display pipelines — `engine.rs` for buffer text and `status_line.rs` for mode-line/header-line/tab-bar — and a boolean flag (`use_cosmic_metrics`) to switch between TUI and GUI font measurement inside shared layout code. GNU Emacs has neither. GNU has one display engine (`xdisp.c`) and a vtable-based backend abstraction (`struct redisplay_interface`, "RIF") that lives at the glyph-production stage. This document explains the divergence, documents the specific bug that exposed it (doom-modeline `(space :align-to (- right N))` silently dropped), and **proposes** a four-step refactor to unify the engine while keeping the GUI render-thread split that Rust/wgpu actually needs.

The directive driving the proposal: **TUI must be 100% architecturally faithful to GNU; GUI may be Rust-shaped where wgpu/winit demand it, but the abstraction line must be in the right place.**

What this doc claims:

- **Audit evidence (factual):** there are two display paths, mode-line doesn't process display properties, `parse_display_space_width` is incomplete, `use_cosmic_metrics` is a runtime boolean where GNU uses vtable dispatch. These are all verifiable in the current code.
- **Design choice (not derived from the audit):** fix it by unifying through a `DisplayBackend` trait rather than patching the existing divergent path. This is a judgment call following the stated directive. An alternate patch path exists and is described in the "Alternative considered" section below.

---

## Background: how we found this

Two commits landed on `main` yesterday:

- `f7deeed` — `bytecode/vm: delegate primitive opcodes to builtins, matching GNU` — 31 bytecode opcodes (`Setcar`, `Setcdr`, arithmetic, comparison, type predicates, etc.) now delegate through `dispatch_vm_builtin_with_frame` instead of inlining their own implementations. Net `−287` lines in `vm.rs`.
- `e37e718` — `keymap: store_in_keymap must skip string prompt when prepending` — fixed `store_in_keymap` so the keymap prompt string is preserved when prepending a new binding. This is what made the doom dashboard finally render `SPC n a` instead of `EMA ESC SPC n a`.

After those landed, the dashboard rendered correctly in TUI but the user reported three remaining issues:

1. Doom dashboard not centered in the terminal window
2. Mode-line colors wrong (single color band visible on the dashboard, no segment colors)
3. Mode-line right-aligned content missing

Plus a separate `winner-mode` plistp error during doom load. We confirmed GNU's `plist-put` signals on the same malformed list, so neomacs's `plist-put` is correct; the real cause is somewhere upstream calling `plist-put` on a widget `:type` spec, likely via a `cl-generic` dispatch divergence (the log also shows `git-commit` failing with `(invalid-function (#s(cl--generic-method ...)))`). That's a separate investigation and not in scope for this doc.

Issues #2 and #3 were originally believed to be the same bug. Rev 2 walks that claim back — only #3 is proven. See "What's actually proven" below.

---

## The bug, in concrete code terms

### The doom-modeline call

`doom-modeline` builds the right-aligned space stretch via (see `doom-modeline-core.el:1337`):

```elisp
(propertize " "
            'face (doom-modeline-face)
            'display
            `(space :align-to (- ,mode-line-right-align-edge (,rhs-width))))
```

The `:align-to` value is a **cons expression** — `(- right N)` — not a number. GNU evaluates this via `calc_pixel_width_or_height` (`xdisp.c:30102`), a recursive evaluator that handles symbols (`text`, `left`, `right`, `center`, `right-fringe`, `right-margin`, `scroll-bar`, …), arithmetic (`(+ …)`, `(- …)`), unit-suffixed pairs (`(NUM . in)`, `(NUM . cm)`), the pixel form `(NUM)`, and arbitrary buffer-local variables.

### Where neomacs handles `:align-to` for buffer text

One site, in `neomacs-layout-engine/src/engine.rs:527`:

```rust
fn parse_display_space_width(
    val: &neovm_core::emacs_core::Value,
    char_w: f32,
    current_x: f32,
    content_x: f32,
) -> f32 {
    if let Some(items) = neovm_core::emacs_core::value::list_to_vec(val) {
        let mut i = 1;
        while i + 1 < items.len() {
            if items[i].is_symbol_named(":align-to") {
                let item = items[i + 1];
                if let Some(n) = item.as_fixnum() {             // fixnum only
                    let target_x = content_x + n as f32 * char_w;
                    return (target_x - current_x).max(0.0);
                } else if item.is_float() {                      // float only
                    …
                }
            }
            i += 2;
        }
    }
    char_w
}
```

It accepts only fixnum/float. Cons-form expressions like `(- right N)` and the bare symbol `right` fall through and the function returns the default `char_w` — effectively a no-op.

### Where neomacs handles `:align-to` for mode-line

Nowhere. The mode-line path never calls `parse_display_space_width`. Here's the actual chain:

`engine.rs::layout_window_rust` calls `eval_status_line_format_value` (which invokes `neovm-core/src/emacs_core/xdisp.rs::format_mode_line_for_display`) to produce the mode-line string. That string **does** carry its full text-property table — `xdisp.rs:761/827/894` propagate properties via `text_props.append_shifted`:

```rust
// xdisp.rs — append_string_value_preserving_props
fn append_string_value_preserving_props(&mut self, value: &Value) {
    let Some(text) = value.as_str() else { return; };
    let byte_offset = self.text.len();
    self.text.push_str(text);
    if value.is_string() {
        if let Some(props) = get_string_text_properties_table_for_value(*value) {
            self.text_props.append_shifted(&props, byte_offset);
        }
    }
}
```

Then `engine.rs:4458/4498/4539` calls `render_rust_status_line_value` (for mode-line, header-line, tab-line respectively; `render_rust_status_line_plain` at `engine.rs:2129` is the separate minibuffer echo path). `render_rust_status_line_value` calls `build_rust_status_line_spec` at `status_line.rs:772`. That function at line 803 has this harvester:

```rust
let mut boundaries = vec![0usize];
for interval in props.intervals_snapshot() {
    if interval.properties.contains_key("face")
        || interval.properties.contains_key("font-lock-face")
    {
        boundaries.push(interval.start);
        boundaries.push(interval.end);
    }
}
boundaries.sort_unstable();
boundaries.dedup();
```

It only scans for `face` and `font-lock-face`. **`display` is never consulted.** Any `(propertize " " 'display '(space :align-to …))` in the mode-line string just becomes a literal space character. The `display_props` and `align_entries` fields on `StatusLineSpec` are declared, initialized to empty vectors, and never populated.

So the `:align-to` loss point is `status_line.rs:803`, specifically the scan predicate. The two-pass `format-mode-line` → render approach (which differs from GNU's inline `display_mode_element` walker) does **not** lose the properties — they arrive at the status-line builder intact. The builder just doesn't know to look at them.

### The `:align-to` bug is proven

The chain above — doom emits `(space :align-to (- right N))`, `build_rust_status_line_spec` doesn't harvest `display`, `status_line.rs` never evaluates align-to expressions — is verifiable in the code and explains the captured screen state. I'm confident in this one.

### The color bug is a hypothesis

Rev 1 claimed the "single color band" observation (cols 1–66 of the captured mode-line rendering as one uniform color) was the same bug as the `:align-to` collapse. Rev 2 walks that back. Here's why.

`build_rust_status_line_spec` **does** preserve face runs. The harvester at `status_line.rs:803` explicitly looks for `face` and `font-lock-face` intervals and produces boundary records at `status_line.rs:820+`. So if doom-modeline's dashboard segments have distinct faces, those faces should appear as distinct color bands in the rendered output, independent of whether `:align-to` evaluates correctly.

Possible explanations for the single-band observation:

1. **Same face across segments.** The doom dashboard state may legitimately use the same face (`mode-line` or `doom-modeline-buffer-path` both inheriting from a common base) for both sides, so they render identically. If this is true, the color band is not a bug at all.
2. **Face merge issue.** `doom-modeline` uses `add-face-text-property` to apply the `mode-line` face as a base beneath all segments. If neomacs's text-property face merging flattens this to a single resolved face, segment-specific faces would collapse.
3. **Two-pass `format-mode-line` discarding a face attribute.** Unlikely given the `text_props.append_shifted` evidence, but worth ruling out.

**Resolution:** The `:align-to` bug is provable from the current code. The color bug is not. The right next step for the color issue is: after the `:align-to` fix lands, re-capture the mode-line, and if the band is still monochromatic where it should be multi-faced, open a separate investigation. Do not bundle it into this refactor until it's reproduced independently.

---

## Pipeline audit: GNU vs neomacs TUI vs neomacs GUI

### GNU Emacs

**One display engine.** `redisplay_internal` (`xdisp.c:17196`) walks all visible frames. For each frame it walks each window via `redisplay_window` → `try_window_*` → `display_line`. `display_line` advances a `struct it` iterator (the canonical iterator carrying position, face, glyph row, display-property stack — defined in `dispextern.h:2394`) and calls the `PRODUCE_GLYPHS` macro for each display element. The macro dispatches via the per-frame `redisplay_interface` ("RIF", `dispextern.h:3033`) to a backend-specific glyph producer:

```c
#define PRODUCE_GLYPHS(IT)                              \
   do {                                                 \
     if (FRAME_RIF ((IT)->f) != NULL)                   \
       FRAME_RIF ((IT)->f)->produce_glyphs ((IT));      \
     else                                               \
       produce_glyphs ((IT));                           \
   } while (false)
```

For TTY frames, the fallback `produce_glyphs` in `term.c` measures with character cells. For X/GTK/NS, the per-backend `gui_produce_glyphs` in `xdisp.c:33185` measures with real font metrics. Both end at `produce_stretch_glyph` (`xdisp.c:32510`) which calls `calc_pixel_width_or_height` (`xdisp.c:30102`) for `(space …)` evaluation. **Both call the same function** — there is no buffer-vs-modeline split in the evaluator.

**Mode-line is not special.** `display_mode_line` (`xdisp.c:27879`) sets up a `struct it` with the mode-line face id and `it.glyph_row->mode_line_p = true`, then calls `display_mode_element` → `display_string` → `display_line` → `PRODUCE_GLYPHS`. Same code path as buffer text. Mode-line is an *invocation* of the display engine, not an alternate engine.

**Architectural invariants:**

1. One iterator (`struct it`), one walker (`display_line`), one display-prop handler (`handle_display_prop`), one stretch-glyph producer (`produce_stretch_glyph`), one expression evaluator (`calc_pixel_width_or_height`).
2. Backend abstraction lives at glyph production stage, not above it. Above the abstraction line: identical code for all backends. Below: per-backend implementation.
3. Mode-line, header-line, tab-line, overlay strings, buffer text — all flow through the same engine. The only difference is row flags (`mode_line_p`, `tab_line_p`, `header_line_p`).

### Neomacs TUI

```
Context::redisplay() ← eval.rs:5246
        │
        ▼  redisplay_fn callback (TUI closure in main.rs)
        │
LayoutEngine::layout_frame_rust() ← engine.rs:1265
        │
        ├─→ layout_window_rust() ← engine.rs (buffer text)
        │       parse_display_space_width() ← engine.rs:527 (fixnum/float only)
        │
        ├─→ render_rust_status_line_value() ← engine.rs:4458/4498/4539
        │       for ModeLine, HeaderLine, TabLine
        │       → build_rust_status_line_spec() ← status_line.rs:772
        │              property harvester at status_line.rs:803 scans ONLY
        │              face / font-lock-face — never display
        │
        └─→ render_rust_status_line_plain() ← engine.rs:2129 and 4729
                for Minibuffer echo AND frame tab-bar
                (tab-bar via render_frame_tab_bar_rust at engine.rs:4706)
        │
        ▼  produces FrameDisplayState
        │
TtyRif::rasterize() → diff_and_render() → ANSI bytes → stdout
        ← tty_rif.rs:195/401
```

**Single thread.** Layout, rasterize, and stdout write all happen on the evaluator thread. This part matches GNU exactly.

**Two display loops in `neomacs-layout-engine`.** `engine.rs::layout_window_rust` for buffer text, `status_line.rs::render_rust_status_line_value` / `_plain` for mode-line/header-line/tab-bar/minibuffer. Buffer text has incomplete display-property handling (fixnum/float `:align-to` only). Status-line has a simplified harvester that only sees `face`/`font-lock-face`. Both paths are missing pieces that GNU's unified walker has.

**Mode-line is two-pass.** The layout engine first calls `eval_status_line_format_value` (`engine.rs:55`) which invokes `neovm-core/src/emacs_core/xdisp.rs::format_mode_line_for_display` at `xdisp.rs:189`. This is a **Rust** entry point that mirrors GNU's `display_mode_line` with `MODE_LINE_DISPLAY` target — it explicitly bypasses the Lisp-facing `format-mode-line` subr (which uses `MODE_LINE_STRING` target and returns `"--"` for `%-` instead of filling with dashes). The result is a propertized string. That string is then walked by `build_rust_status_line_spec`. The two-pass pattern **does not** lose propertize properties at the format boundary — properties are preserved via `text_props.append_shifted` at `xdisp.rs:761/827/894`. The property loss is one layer deeper, in the harvester at `status_line.rs:803`. **But** the two-pass approach still differs from GNU's inline `display_mode_element` walker (GNU walks the format spec inline and produces glyphs as it goes — no intermediate string), so it remains a legitimate architectural target for unification even though it is not the proximate cause of this specific bug.

### Neomacs GUI

Same trigger, same `layout_frame_rust`, same `status_line.rs` divergence, same display-prop gap. The differences from TUI all show up *after* layout:

```
Context::redisplay() ← eval.rs:5246
        │
        ▼  redisplay_fn callback (GUI closure in main.rs)
        │
LayoutEngine::layout_frame_rust() ← engine.rs:1265  (use_cosmic_metrics=true)
        │
        ├─→ layout_window_rust()
        │       calls FontMetricsService via char_advance() for char widths
        │       ← char_advance at engine.rs:4752 (cosmic-text based measurement)
        │
        └─→ render_rust_status_line_value() — same divergence as TUI
        │
        ▼  produces FrameDisplayState (grid-native, face IDs, char positions)
        │
frame_tx.try_send(state) ← thread_comm.rs (unbounded channel)
        │
        ▼  ════════ thread boundary ════════
        │
RenderThread::poll_frame() → materialize() → render_pass()
        ← render_thread/frame_ingest.rs, render_pass.rs:154
        │
        ▼
WgpuGlyphAtlas::get_or_create() ← glyph_atlas.rs:201
        cache miss → rasterize_glyph() via SwashCache (real shaping)
        → upload to GPU atlas texture
        ← glyph_atlas.rs:522
        │
        ▼  wgpu draw calls → surface.present() → window pixels
```

**Two threads.** Eval thread does measurement via `FontMetricsService` (cosmic-text — char widths, font advance, line height) and produces a grid-native `FrameDisplayState`. Render thread consumes the `FrameDisplayState`, and on glyph-atlas cache miss performs real rasterization via `WgpuGlyphAtlas::rasterize_glyph` → `SwashCache` (actual font rasterization with bearing computation), then GPU upload and draw call submission.

The work split is **measurement on eval / rasterization on render**, not "layout on eval / submit on render". Both threads do real shaping-adjacent work. This matters for threading design (see Step 4) — the simple argument "move the slow cosmic-text work to the render thread" doesn't apply because both sides are doing non-trivial work already.

**Mode-line bug is the same bug as TUI.** `status_line.rs:803`'s harvester is called regardless of frontend. Both frontends inherit the missing `display` property processing.

### What's COMMON between TUI and GUI

| | Shared? |
|---|---|
| Trigger (`Context::redisplay()`) | yes |
| Layout entry (`layout_frame_rust`) | yes |
| Buffer-text walker (`layout_window_rust`) | yes |
| Mode-line path (`render_rust_status_line_value` + `build_rust_status_line_spec`) | yes |
| Mode-line `display` property harvester | yes — and broken on both |
| Buffer `:align-to` evaluator (`parse_display_space_width`) | yes — and incomplete on both |
| Intermediate format (`FrameDisplayState`) | yes |

### What's DIFFERENT between TUI and GUI

| | TUI | GUI |
|---|---|---|
| Font measurement on eval thread | cell grid (`char_advance` returns `min_grid_advance` when `font_metrics` is `None`) | `FontMetricsService` via cosmic-text |
| Output stage | `TtyRif::rasterize` + `diff_and_render` + stdout | `frame_tx.try_send` → render thread |
| Font shaping/rasterization | none (just cells) | `WgpuGlyphAtlas::rasterize_glyph` → `SwashCache` on render thread |
| Differential update | per-row glyph diff (`tty_rif.rs:401`) | none — full atlas lookup each frame, GPU diffs via overdraw |
| Threads | 1 (evaluator) | 2 (evaluator + render) |

### What's DIFFERENT from GNU

| Concern | GNU | Neomacs |
|---|---|---|
| Number of display loops | 1 (`xdisp.c`) | 2 (`engine.rs::layout_window_rust` + `status_line.rs::build_rust_status_line_spec`) |
| Mode-line dispatch | `display_mode_line` sets a flag, calls SAME `display_line` / `PRODUCE_GLYPHS` | Two-pass: `xdisp::format_mode_line_for_display` (a Rust entry that mirrors GNU's `display_mode_line` with `MODE_LINE_DISPLAY`, bypassing the Lisp subr) produces a propertized string, then a separate walker (`build_rust_status_line_spec`) with a simplified property harvester |
| `(space :align-to …)` evaluator | `calc_pixel_width_or_height` (xdisp.c:30102, ~250 lines), recursive, handles symbols/arithmetic/units, called from both buffer & mode-line | `parse_display_space_width` (engine.rs:527, ~30 lines), fixnum/float only, never called from mode-line |
| Backend abstraction location | At glyph-production stage (`PRODUCE_GLYPHS` macro → RIF dispatch) | After layout, on `FrameDisplayState` |
| TUI/GUI selection mechanism | Per-frame RIF function pointer, set once at frame creation | Runtime boolean (`use_cosmic_metrics`) checked inline at every measurement site in `engine.rs` |
| Iterator | `struct it` shared across buffer/string/overlay/mode-line, with display-prop stack | `WindowParams` + ad-hoc state in `layout_window_rust`; `status_line.rs` has its own walker; nothing shared |
| Mode-line display-prop handling | One `handle_display_prop` (xdisp.c:5858) called from iterator advancement in all contexts | `engine.rs::layout_window_rust` checks display props inline at ~2516+; `status_line.rs` builder at 803 only checks `face`/`font-lock-face` |

---

## The `use_cosmic_metrics` smell

The clearest single artifact of the architecture problem is `LayoutEngine::use_cosmic_metrics` (`engine.rs:985`). It is a `pub bool` that defaults to `true` and is flipped to `false` exactly once, in `main.rs:1288` on the TUI startup path:

```rust
// TTY frames use 1x1 character cell metrics (GNU Emacs frame.c:1184-1185),
// not pixel-based cosmic-text font metrics.
LAYOUT_ENGINE.with(|engine| {
    engine.borrow_mut().use_cosmic_metrics = false;
});
```

Four sites inside `engine.rs` branch on it:

| Site | Effect when `false` |
|---|---|
| `engine.rs:1272` (top of `layout_frame_rust`) | Drops `FontMetricsService` |
| `engine.rs:4652` (`status_line_font_metrics`) | Returns face's stored cell metrics instead of cosmic-text-resolved pixel metrics |
| `engine.rs:4752` (`char_advance`) | Returns `min_grid_advance` (cell-based) instead of cosmic-text shaped advance when `font_metrics` is `None` |

Plus `StatusLineAdvanceMode::{Fixed, Measured}` in `status_line.rs:187`. **Important correction from Rev 1:** this enum is *dormant scaffolding*, not an active second branch. `StatusLineSpec::plain` at `status_line.rs:425` hardcodes `advance_mode: StatusLineAdvanceMode::Fixed` and is the only constructor. The `Measured` match arm at `engine.rs:4690` has no producer — it was added for a planned optimization that never shipped. The enum should be deleted as cleanup during the refactor, but it's not currently causing behavior.

The live part of the smell — the `use_cosmic_metrics` flag and its three read sites in `engine.rs` — is GNU's `PRODUCE_GLYPHS` macro inverted. GNU dispatches to a per-backend function via vtable. Neomacs has a single function that contains an inline `if use_cosmic_metrics` branch at every measurement site. Adding a third backend would mean adding a third branch to every check.

The flag is doing two distinct jobs at once: (a) selecting the *measurement strategy* (cells vs pixels), and (b) selecting the *implementation* (skip cosmic-text or call into it). Both jobs should be expressed by *which backend was constructed at startup*, not by a runtime boolean inside shared layout code.

---

## Threading: TUI and GUI take different positions deliberately

GNU is single-threaded for display: `redisplay_internal` runs on the main thread, calls into `display_line` → `PRODUCE_GLYPHS` → backend output, all synchronously. There is no "render thread" in GNU.

Neomacs's TUI matches this exactly. **Keep it that way.**

Neomacs's GUI uses a render thread because wgpu/winit demand one. The division of labor today is:

- **Eval thread:** walks the iterator, calls `FontMetricsService` (cosmic-text) for char widths, evaluates display properties (for buffer text only), produces a grid-native `FrameDisplayState` with chars, face IDs, cursor positions, and window metadata.
- **Render thread:** pulls `FrameDisplayState` from the channel, materializes it into glyph positions, looks up each glyph in `WgpuGlyphAtlas`, on cache miss calls `rasterize_glyph` (real SwashCache-based shaping + rasterization), uploads to GPU atlas, submits draw calls, presents.

Both threads do real work. The render thread is not idle — it does the actual font rasterization — so the simple argument "move the expensive layout to the render thread" doesn't apply. What it *does* mean is that the measurement/rasterization split is roughly the right shape: cheap-ish metrics on the eval thread keep layout fast, expensive rasterization on the render thread keeps the eval thread from blocking on font bitmap generation.

Where it's wrong relative to GNU is not the presence of threads but the *level* of the abstraction. The eval thread's `LayoutEngine::layout_frame_rust` is not backend-agnostic — it flips behavior based on `use_cosmic_metrics`. GNU's equivalent (the iterator walker) **is** backend-agnostic; the per-backend work happens inside `produce_glyphs`/`gui_produce_glyphs` below the vtable line.

The refactor target for threading is: introduce the `DisplayBackend` trait such that above it everything is backend-agnostic, and below it the TUI backend runs synchronously on the eval thread (matching GNU) while the GUI backend may internally manage a render thread. The display engine never knows whether there's a thread; that's an implementation detail of the GUI backend.

Note this is still an incremental proposal, not a full solution. The question "where does cosmic-text `FontMetricsService` actually live — eval thread, render thread, or both" remains open and is discussed under Step 4.

---

## Proposed end-state architecture

```
                          ┌──────────────────────────┐
                          │    Context::redisplay()  │
                          └────────────┬─────────────┘
                                       ▼
                          ┌──────────────────────────┐
                          │  redisplay_internal_rust │   (new; mirrors GNU)
                          └────────────┬─────────────┘
                                       │ for each visible frame
                                       ▼
                          ┌──────────────────────────┐
                          │   redisplay_window_rust  │
                          └────────────┬─────────────┘
                                       │
                          ┌────────────┴───────────────┐
                          ▼                            ▼
              ┌────────────────────┐        ┌─────────────────────┐
              │  display_line_rust │        │display_mode_line_rust│
              │   (buffer text)    │        │  (sets mode_line_p) │
              └─────────┬──────────┘        └──────────┬──────────┘
                        │                              │
                        │   uses unified Iterator      │
                        │   with display_prop_stack    │
                        ▼                              ▼
                          ┌──────────────────────────┐
                          │  display_line_rust_walk  │   ← SAME function
                          └────────────┬─────────────┘
                                       │
                                       ▼
                          ┌──────────────────────────┐
                          │ DisplayBackend::produce_*│   (trait, neomacs RIF)
                          └────────────┬─────────────┘
                                       │ vtable dispatch
                          ┌────────────┴────────────┐
                          ▼                         ▼
              ┌──────────────────────┐   ┌──────────────────────┐
              │ TtyDisplayBackend    │   │ GuiDisplayBackend    │
              │ - cell-based         │   │ - cosmic-text shaping│
              │ - synchronous        │   │ - render-thread split│
              │   on eval thread     │   │   internal           │
              │ - calls wcwidth      │   │ - calls cosmic-text  │
              │ - emits via TtyRif   │   │ - sends to wgpu loop │
              └──────────────────────┘   └──────────────────────┘
                          │                         │
                          ▼                         ▼
                  ANSI bytes → stdout       wgpu draw calls → window
```

The invariants this enforces:

1. **One display engine.** `redisplay_internal_rust`, `redisplay_window_rust`, `display_line_rust`, `display_mode_line_rust`, the iterator, the display-prop walker, the stretch-glyph evaluator — all single implementations, used identically for buffer and mode-line.
2. **One `:align-to` evaluator.** A faithful Rust port of `calc_pixel_width_or_height` lives in one place and is called from `produce_stretch_glyph` regardless of source or backend.
3. **Backend abstraction at glyph production.** `DisplayBackend` trait with methods like `produce_char_glyph`, `produce_stretch_glyph`, `produce_image_glyph`, `write_glyphs`, `clear_to_eol`, etc. TUI implementation is cell-based and synchronous; GUI implementation is pixel-based and may internally manage a render thread. Above the trait: identical code. Below: backend-specific.
4. **No `use_cosmic_metrics` flag.** TUI selects `TtyDisplayBackend` at startup; GUI selects `GuiDisplayBackend`. The decision is expressed by *which backend was constructed*, not by a runtime branch.
5. **Mode-line is not special.** `display_mode_line_rust` is a thin wrapper that sets `it.glyph_row.mode_line_p = true`, picks a face id, and calls `display_line_rust_walk`. Same as GNU's `display_mode_line`.
6. **TUI threading: 1 thread, matching GNU.** Layout, glyph production, output all on eval thread.
7. **GUI threading: 2 threads, but boundary is inside the backend.** The display engine doesn't know about it. The `GuiDisplayBackend` methods may queue work to a render thread and return immediately. This is an implementation detail of the backend, not an architectural divergence from the shared engine.

---

## Refactor plan (4 steps)

Each step is a separate commit. Each step is testable independently. Earlier steps deliver value before later steps land.

### Step 1 — Port `calc_pixel_width_or_height` faithfully

**Goal:** One GNU-faithful evaluator for `(space :width …)` and `(space :align-to …)` expressions. No behavior change in the rest of the code yet.

**File:** new `neomacs-layout-engine/src/display_pixel_calc.rs`

**Reference:** GNU `xdisp.c:30060–30350`.

**Coverage:**

- `nil` → 0
- Number (fixnum or float) → number × base_unit (`FRAME_COLUMN_WIDTH` for width, `FRAME_LINE_HEIGHT` for height)
- Symbols:
  - Two-character unit symbols: `in`, `mm`, `cm` → DPI conversion
  - `height`, `width` → font height / width
  - `text` → `window_box_width(w, TEXT_AREA) - lnum_pixel_width`
  - `left`, `right`, `center` → window-box offsets (align-to mode)
  - `left-fringe`, `right-fringe`, `left-margin`, `right-margin`, `scroll-bar` → corresponding window-box offsets
  - Fall-through: `buffer_local_value(prop)` → recurse
- Cons:
  - `(image PROPS…)`, `(xwidget PROPS…)` — defer with a placeholder return, document
  - `(+ E…)` → recursive sum
  - `(- E…)` → recursive difference (with single-arg negation handled per GNU)
  - `(NUM)` → absolute pixel count (with optional offset for align-to mode)
  - `(NUM . UNIT)` → NUM × recursive(UNIT)

**Signature sketch** (subject to review):

```rust
pub struct PixelCalcContext<'a> {
    pub window: &'a Window,
    pub frame: &'a Frame,
    pub face_metrics: &'a FontMetrics,
    pub default_font_metrics: &'a FontMetrics,
    pub line_number_pixel_width: f32,
}

pub enum PixelCalcMode { Width, Height }

pub fn calc_pixel_width_or_height(
    ctx: &PixelCalcContext,
    prop: &Value,
    mode: PixelCalcMode,
    align_to: Option<&mut i32>,  // -1 sentinel for "first time"
) -> Option<f64>;
```

**Tests:** Unit tests against the example forms in GNU's docstring:

- `(space :width (+ left-fringe left-margin scroll-bar))`
- `(space :align-to 0)`
- `(space :align-to (0.5 . (- text my-image)))`
- `(space :width (- left-margin 1))`
- `(space :width (- left-margin (2 . width)))`
- `(space :align-to (+ left-margin (0.5 . left-margin) -0.5))`
- `(space :width (- (+ left-fringe left-margin) (1)))`
- `(space :width (+ left-fringe left-margin (- (1))))`
- `(space :width (+ left-fringe left-margin (-1)))`

Plus the doom-modeline form: `(space :align-to (- right (200)))`.

**Scope:** ~250 lines of Rust + tests. Pure helper, no call sites yet. **Half a day.**

### Step 2 — Replace `parse_display_space_width` with `calc_pixel_width_or_height`

**Goal:** Buffer-text `:align-to` handles cons-form expressions correctly.

**Files:** `engine.rs`.

**Change:** At the two call sites of `parse_display_space_width` (`engine.rs:2717` and `engine.rs:4089`), construct a `PixelCalcContext` and call the new function. Delete `parse_display_space_width`.

**Tests:** Integration test laying out a buffer with text `(propertize " " 'display '(space :align-to (- right 5)))` and verifying the resulting glyph row has the space at the right position. Re-run all existing layout-engine tests.

**Scope:** ~2 hours.

**Behavior change:** `(space :align-to <expr>)` in **buffer text** works on both TUI and GUI. Mode-line is still broken because mode-line goes through `status_line.rs`.

### Step 3 — Define `DisplayBackend` trait, port TUI backend faithfully, fold mode-line into unified walker

**Goal:** Eliminate the status-line divergence. Mode-line goes through the same walker as buffer text. TUI architecture becomes 100% faithful to GNU. The mode-line `:align-to` bug is fixed automatically because mode-line now flows through `produce_stretch_glyph` → `calc_pixel_width_or_height`.

**Files:** new `neomacs-layout-engine/src/display_backend.rs` (or similar home), large refactor of `engine.rs`, delete (or shrink to a stub) `status_line.rs`, updated `tty_rif.rs` integration.

**Trait sketch (subject to review):**

```rust
pub trait DisplayBackend {
    fn produce_char_glyph(&mut self, it: &mut It, ch: char);
    fn produce_stretch_glyph(&mut self, it: &mut It, width_px: f32, ascent: f32, descent: f32);
    fn produce_image_glyph(&mut self, it: &mut It, image: &ImageSpec);
    fn produce_glyphless_glyph(&mut self, it: &mut It, ch: char);
    fn produce_composition_glyph(&mut self, it: &mut It, comp: &Composition);

    fn char_width(&self, face: &Face, ch: char) -> f32;
    fn font_height(&self, face: &Face) -> f32;
    fn font_ascent(&self, face: &Face) -> f32;
    fn font_descent(&self, face: &Face) -> f32;

    fn write_row(&mut self, row: &GlyphRow);
    fn flush_frame(&mut self);
}
```

**TtyDisplayBackend:**

- Cell-based measurement (`wcwidth`-equivalent: 1 for ASCII/narrow, 2 for CJK/wide)
- `produce_stretch_glyph` rounds pixel width to cell count
- `write_row` calls into the existing `TtyRif` line-diff-and-emit logic, driven by the unified `GlyphRow` format
- Synchronous, runs on the eval thread

**Iterator:**

A Rust port of GNU's `struct it`, carrying position (charpos, bytepos), face state, glyph row, display-prop stack, `current_x`, ascent/descent. One type, used by both `display_line_rust` and `display_mode_line_rust`. GNU's `struct it` has ~300 lines of fields; not all of them are needed on day 1, but we must triage carefully — do not assume anything is skippable without verifying against GNU. Note specifically that the bidi iterator state (`bidi_it`, `paragraph_embedding`, `bidi_p` — `dispextern.h:2591` and `dispextern.h:2891`) is **not** X-specific; GNU supports bidi text rendering in TTY mode and any correct port must include it from the start or accept that Arabic/Hebrew text rendering will be broken. Fields that can plausibly be deferred include `#ifdef HAVE_WINDOW_SYSTEM`-guarded members, xdisp-internal caches, and iterator-stack fields for less-common features (overlay-arrow, line-prefix) — but each deferral should be justified against a specific GNU definition, not a general "this looks X-specific" hunch.

**Mode-line:**

```rust
pub fn display_mode_line_rust(
    backend: &mut dyn DisplayBackend,
    window: &Window,
    face_id: FaceId,
    format: &Value,
) {
    let mut it = It::new(window, /*charpos=*/-1, /*bytepos=*/-1, face_id);
    it.glyph_row.mode_line_p = true;
    display_mode_element_rust(backend, &mut it, format);
}
```

`display_mode_element_rust` walks the format spec inline (mirroring GNU's `display_mode_element`), calling `display_string_rust` for string segments, which feeds them through the unified `display_line_rust_walk`.

**`status_line.rs`:**

Delete. The 1655 lines of face handling, glyph emission, and glyph layout collapse into the unified walker once display-props are processed in-loop. Or shrink to a thin shim if we discover something irreducible.

**Note on scope risk:** The face-run harvester at `status_line.rs:803` and the face layout logic from `status_line.rs:408+` are real work that has to find a home in the unified walker. We may discover edge cases (tab-bar mouse handling, overlay arrows, margin gutters) that the unified walker doesn't currently handle. Plan to land behind a feature flag if needed, or shrink `status_line.rs` to a stub before full deletion.

**Tests:**

- All existing layout-engine tests must pass.
- New integration test: doom-modeline-style format with `(space :align-to (- right 50))` produces a row with the expected layout (TUI).
- Regression: the keymap-prompt fix from `e37e718` still works.

**Scope:** **2 to 3 days.** The biggest risk is in the face-handling and glyph-layout code currently in `status_line.rs`.

**Behavior change after this step:**

- Mode-line `:align-to` works on both TUI and GUI.
- Mode-line display-property handling (images, stretch, composition) works in mode-line too, not just buffer text.
- TUI architecture is 100% faithful to GNU at the display-engine level.
- **Possibly also:** mode-line color segments render correctly (if the single-band issue was caused by display-prop handling rather than a separate face-merge bug). This is *not* promised — see "color bug is a hypothesis" above.
- **Possibly also:** dashboard centering issue resolves as a side effect if doom-dashboard's resize hook re-runs after the unified walker reports correct `(window-width)`. Also not promised.

### Step 4 — `GuiDisplayBackend` and the render-thread boundary

**Goal:** Move the render-thread boundary inside the GUI backend. The display engine no longer knows there is a thread.

**Files:** new `GuiDisplayBackend` impl in `neomacs-display-runtime` or a new `neomacs-display-backend-gui` crate; refactor of the channel/render-thread setup in `main.rs`.

**Behavior:**

- `GuiDisplayBackend::produce_*` methods are called on the eval thread.
- They build up a backend-specific frame work unit (chars + face refs + glyph kinds + measured pixel positions, or whatever the backend needs).
- `flush_frame` enqueues the work to the render thread via channel.
- The render thread pulls work units, performs cosmic-text shaping if not already done, atlas lookup, GPU upload, draw call submission, present.
- The eval thread is unblocked once `flush_frame` returns.

**Open design question:** where does cosmic-text `FontMetricsService` live?

The current code runs it on the eval thread (`char_advance` at `engine.rs:4752`). The render thread also does cosmic-text / SwashCache work in `WgpuGlyphAtlas::rasterize_glyph`. So both sides already touch cosmic-text, just for different purposes (measurement vs rasterization).

Options:

- **A: Keep as-is** — eval thread does `FontMetricsService` measurement for layout decisions (which glyph cell sizes, which wrap points). Render thread does `SwashCache` rasterization for pixel output. Layout pipeline stays roughly the same shape.
- **B: Move measurement to render thread** — eval thread produces an abstract glyph row (chars + face refs, no pixel positions). Render thread does both measurement and rasterization. This is closer to GNU's model where `gui_produce_glyphs` does both. Requires the eval-thread iterator to report abstract advances that get resolved on the render thread, which is a bigger restructuring.
- **C: Hybrid** — cached metrics on eval thread (fast path), slow path on render thread for uncached faces. Adds complexity.

**Recommendation:** Option A for now. Revisit after Step 3 lands and we have measurements of where eval-thread time actually goes under load.

**Tests:** GUI integration tests would need a wgpu harness. Likely defer to manual verification with the existing render thread setup.

**Scope:** **1 to 2 days** for the trait implementation. The hard part is channel/sync semantics without races.

**Behavior change:** GUI threading model is cleaner. `use_cosmic_metrics` flag is deleted. `LayoutEngine` struct shrinks. Ideally no user-visible behavior change beyond cleaner code.

---

## Alternative considered: patch the existing path instead of unifying

**Proposed by the first reviewer:** a smaller fix that teaches `build_rust_status_line_spec` at `status_line.rs:772` to harvest `display` intervals and populate `align_entries` / `display_props` on `StatusLineSpec`. The existing consumer code at `status_line.rs:651+` already has scaffolding for `align_entries` — it's wired to the rendering loop but there's just no producer. Extending the harvester's scan predicate from `face`/`font-lock-face` to include `display`, then evaluating the `display` spec's `(space :align-to …)` via a new helper (possibly the ported `calc_pixel_width_or_height` from Step 1), would fix the `:align-to` bug without touching the overall architecture.

**Pros:**

- Much smaller scope. ~1 day instead of ~5 days.
- Lower risk. No restructuring, no trait introduction, no deletion of 1655 lines of code that's working for other purposes.
- Immediate fix for the user-visible symptom.
- Step 1 (porting `calc_pixel_width_or_height`) is still valuable as a shared helper even without the full refactor.

**Cons:**

- Extends the divergent path. `status_line.rs` stays alive; any future display-property bug in mode-line (images, compositions, `:width`, nested `propertize`) needs a second fix site.
- Doesn't address `use_cosmic_metrics` or any of the other architectural issues.
- The patched code in `status_line.rs` becomes obsolete if we eventually do unify — temporary work.
- Doesn't address the two-pass `format-mode-line` divergence from GNU (which isn't the cause of *this* bug but is still architecturally different).

**Trade-off:** this is a genuine design choice, not an evidence question. The audit proves both paths are viable. The directive "TUI must be 100% architecturally faithful to GNU" argues for unification. The directive "don't mask bugs, don't do workarounds" also argues for unification (on the grounds that patching a divergent path is a workaround by construction). But a reasonable reviewer can disagree — the patch-path is small, tested, and reversible, and the unification refactor carries real risk.

**Author's recommendation (open to pushback):** Unification (Steps 1–4 as above), on the grounds that:

1. The directive is explicit about 100% TUI architectural fidelity.
2. The unification cost amortizes over future display-property features that would otherwise need two-site fixes.
3. Step 1 (porting `calc_pixel_width_or_height`) is valuable either way and can land first as a pure helper.
4. Step 3 is the risk-concentration point — if Step 3 runs into trouble, we can bail out and fall back to the patch approach with only Step 1 sunk cost.

**If the team prefers the patch approach**, the minimum scope is: Step 1 (port `calc_pixel_width_or_height`), then extend `build_rust_status_line_spec` at `status_line.rs:803` to harvest `display` intervals, then add a call site in `status_line.rs:651`'s rendering loop that evaluates the `display` spec via the ported function and produces `align_entries`. Total ~1 day. This fixes the `:align-to` bug for mode-line without touching the engine architecture.

---

## Risks and unknowns

1. **`status_line.rs` edge cases.** 1655 lines is a lot. We will discover features there that the unified walker doesn't yet handle (tab-bar mouse regions, overlay arrows, margin gutters). Plan to land Step 3 behind a feature flag if needed, or shrink `status_line.rs` to a stub before deleting.

2. **`calc_pixel_width_or_height` corner cases.** GNU's docstring lists specific examples but the function handles arbitrary nested expressions. The Rust port needs to be tested against the same forms GNU is tested against, and we should grep popular modeline packages for unusual `:align-to` expressions to make sure we cover them.

3. **Iterator state.** GNU's `struct it` has ~300 lines of fields. Triage which are needed on day 1 before porting, but **do not assume bidi iterator state is skippable** — `bidi_it`, `paragraph_embedding`, `bidi_p` (`dispextern.h:2591`/`:2891`) are core to `display_line` and are used for TTY bidi rendering, not just X. Fields that can legitimately be deferred need to be justified against specific GNU definitions, not general hunches about what "looks X-specific".

4. **TtyRif rewrite scope.** Current `tty_rif.rs::diff_and_render` does its own glyph compare. After Step 3 it should be driven by the unified `GlyphRow` format. The diff logic itself can stay; only the input format changes. Also consider adding scroll-region support (matching `dispnew.c::write_matrix`) as an optional follow-up.

5. **GUI render thread races.** Step 4's biggest risk is introducing races between eval-thread layout and render-thread present. The current "latest wins" channel semantics work because the eval thread always sends a complete frame. After Step 4 the boundary moves and we need to be careful about what state lives where.

6. **Performance regressions.** GNU is fast because of decades of optimization (matrix sync, partial redraws, caches). The Rust port will be slower at first. Benchmark before/after on a doom dashboard render.

7. **Color-band observation is unverified.** The `:align-to` fix may or may not resolve the single-color-band observation. If it doesn't, a separate investigation is needed.

---

## Test plan

For each step:

- All existing tests in `neomacs-layout-engine`, `neomacs-display-protocol`, and `neovm-core` must pass.
- New unit tests for `calc_pixel_width_or_height` against GNU's docstring examples.
- New integration test: layout a buffer with `(propertize " " 'display '(space :align-to (- right 50)))` and verify the resulting glyph row.
- Manual TUI verification with the user's doom config: dashboard centering, mode-line right alignment.
- The keymap-prompt regression test from `e37e718` must continue to pass.

Manual GUI verification can wait until Step 4.

---

## What I'm asking the team to review

1. **Does the audit evidence hold up?** Specifically: one display engine in GNU, two in neomacs; `:align-to` cons-forms unhandled in `parse_display_space_width`; `display` prop unhandled in `build_rust_status_line_spec`'s harvester; `use_cosmic_metrics` as a runtime boolean where GNU uses vtable dispatch.

2. **Is the unification vs patch trade-off correctly characterized in the "Alternative considered" section?** I believe unification is the right call per the stated directive, but the smaller patch is a defensible alternative and I want the team to see both options clearly before committing.

3. **Is the `DisplayBackend` trait shape right?** GNU's RIF (`struct redisplay_interface`) has more methods than sketched here (cursor_to, clear_to_end_of_line, ring_bell, etc.). Some may be needed for completeness.

4. **Step 3 scope realism.** Is "fold `status_line.rs` into the unified walker" actually achievable in 2–3 days, or is it a week-long project hiding behind an estimate?

5. **Step 4 Option A/B/C.** Where should cosmic-text live — eval thread, render thread, or both?

6. **Trait location.** Should `DisplayBackend` live in `neomacs-layout-engine` or its own crate (`neomacs-display-backend`)?

7. **`status_line.rs` deletion vs stub.** Any features in `status_line.rs` that the unified walker won't easily absorb?

8. **TUI scroll-region optimization.** Part of Step 3, or separate follow-up?

9. **Color bug.** Assuming the `:align-to` fix lands and the single-color-band observation remains, what does the team think the most likely explanation is (same face across doom-modeline segments on the dashboard state, or a face-merging bug)?

---

## Appendix A: file references

| File | Purpose |
|---|---|
| `neovm-core/src/emacs_core/eval.rs:5246` | `Context::redisplay()` — neomacs redisplay trigger |
| `neovm-core/src/emacs_core/xdisp.rs:189` | `format_mode_line_for_display` — Rust mode-line formatter mirroring GNU's `display_mode_line` (`MODE_LINE_DISPLAY` target); explicitly bypasses the Lisp `format-mode-line` subr |
| `neovm-core/src/emacs_core/xdisp.rs:761,827,894` | `append_string_value_preserving_props` and friends — **properties are preserved here**, not lost |
| `neomacs-layout-engine/src/engine.rs:55` | `eval_status_line_format_value` — calls `format_mode_line_for_display` directly, bypassing the Lisp `format-mode-line` subr |
| `neomacs-layout-engine/src/engine.rs:527` | `parse_display_space_width` — incomplete `:align-to` evaluator (to be replaced) |
| `neomacs-layout-engine/src/engine.rs:985` | `use_cosmic_metrics` field |
| `neomacs-bin/src/main.rs:1288` | The single site that flips `use_cosmic_metrics` to false |
| `neomacs-layout-engine/src/engine.rs:1272` | `layout_frame_rust` — lazy-init of `FontMetricsService` based on the flag |
| `neomacs-layout-engine/src/engine.rs:2129` | `render_rust_status_line_plain` call site — minibuffer echo |
| `neomacs-layout-engine/src/engine.rs:4729` | `render_rust_status_line_plain` call site — frame tab-bar (via `render_frame_tab_bar_rust` at `engine.rs:4706`) |
| `neomacs-layout-engine/src/engine.rs:4458/4498/4539` | `render_rust_status_line_value` call sites — mode-line / header-line / tab-line |
| `neomacs-layout-engine/src/engine.rs:4652` | `status_line_font_metrics` — branches on `use_cosmic_metrics` |
| `neomacs-layout-engine/src/engine.rs:4752` | `char_advance` (standalone fn) — branches on presence of `FontMetricsService` |
| `neomacs-layout-engine/src/engine.rs:4690` | Consumer of `StatusLineAdvanceMode::Measured` (dead match arm) |
| `neomacs-layout-engine/src/status_line.rs:187` | `StatusLineAdvanceMode` enum (dormant) |
| `neomacs-layout-engine/src/status_line.rs:408` | `StatusLineSpec` struct — holds `display_props` and `align_entries` (never populated) |
| `neomacs-layout-engine/src/status_line.rs:425` | `StatusLineSpec::plain` — hardcodes `advance_mode: Fixed` |
| `neomacs-layout-engine/src/status_line.rs:651` | Existing rendering loop with `align_entries` consumer — no producer |
| `neomacs-layout-engine/src/status_line.rs:772` | `build_rust_status_line_spec` — the mode-line builder |
| `neomacs-layout-engine/src/status_line.rs:803` | **Property harvester — the actual loss point** (only scans `face`/`font-lock-face`) |
| `neomacs-display-protocol/src/tty_rif.rs:195,401` | `TtyRif::rasterize`, `diff_and_render` — TUI output stage |
| `neomacs-display-runtime/src/render_thread/frame_ingest.rs` | GUI render thread frame polling |
| `neomacs-display-runtime/src/render_thread/render_pass.rs:154` | GUI render pass entry |
| `neomacs-renderer-wgpu/src/glyph_atlas.rs:201` | `WgpuGlyphAtlas::get_or_create` — calls `rasterize_glyph` on cache miss |
| `neomacs-renderer-wgpu/src/glyph_atlas.rs:522` | glyph rasterization entry |
| `/home/exec/Projects/github.com/emacs-mirror/emacs/src/xdisp.c:17196` | GNU `redisplay_internal` |
| `/home/exec/Projects/github.com/emacs-mirror/emacs/src/xdisp.c:25609` | GNU `display_line` |
| `/home/exec/Projects/github.com/emacs-mirror/emacs/src/xdisp.c:27879` | GNU `display_mode_line` |
| `/home/exec/Projects/github.com/emacs-mirror/emacs/src/xdisp.c:28131` | GNU `display_mode_element` |
| `/home/exec/Projects/github.com/emacs-mirror/emacs/src/xdisp.c:30102` | GNU `calc_pixel_width_or_height` |
| `/home/exec/Projects/github.com/emacs-mirror/emacs/src/xdisp.c:32510` | GNU `produce_stretch_glyph` |
| `/home/exec/Projects/github.com/emacs-mirror/emacs/src/dispextern.h:2926` | GNU `PRODUCE_GLYPHS` macro |
| `/home/exec/Projects/github.com/emacs-mirror/emacs/src/dispextern.h:3033` | GNU `struct redisplay_interface` (RIF) |
| `/home/exec/Projects/github.com/emacs-mirror/emacs/src/term.c:972` | GNU TTY `tty_write_glyphs` |
| `/home/exec/Projects/github.com/emacs-mirror/emacs/src/dispnew.c:5758` | GNU `write_matrix` (line diff with scrolling) |

## Appendix B: the doom-modeline call site

From `doom-modeline-core.el:1325–1342`:

```elisp
(let* ((rhs-str (format-mode-line `("" ,@rhs-forms)))
       (rhs-width (progn
                    (add-face-text-property
                     0 (length rhs-str) 'mode-line t rhs-str)
                    (doom-modeline-string-pixel-width rhs-str))))
  (propertize
   " "
   'face (doom-modeline-face)
   'display
   (if (and (display-graphic-p)
            (not (eq mode-line-right-align-edge 'window)))
       `(space :align-to (- ,mode-line-right-align-edge
                            (,rhs-width)))
     `(space :align-to (,(- (window-pixel-width)
                            (window-scroll-bar-width)
                            ...))))))
```

The `display` value uses the cons-form `(- ,sym (,num))` and the bare-symbol form `right-margin`/`right-fringe`. Both are unhandled by neomacs's current `parse_display_space_width`. Additionally, since this expression lives on a `propertize`d string fed to `format-mode-line`, the `display` property is preserved through `xdisp.rs::format_mode_line_for_display` to the status-line builder, but then dropped because `build_rust_status_line_spec` doesn't harvest `display` intervals.

## Appendix C: the captured screen state

Decoded from a 200×60 pty run of neomacs with the user's doom config, row 59 (the mode-line):

```
cols   1– 66: bg=(29,32,38) fg=(187,194,207)
              "   ~/Projects/github.com/eval-exec/neomacs-main/  DOOM v3.0.0-pre "
cols  68–200: bg=(40,44,52) fg=(255,255,255)  (default face, empty)
```

**What's proven:** The content "DOOM v3.0.0-pre" in row 59 is the RHS portion of the doom modeline, which should be right-aligned near col 200 via `(space :align-to (- right …))`. Instead it sits immediately after the LHS content because the align-to evaluates to 0 width. This is the `:align-to` bug, fully explained by the audit.

**What's hypothesized, not proven:** The uniform color of cols 1–66 (single `(29,32,38)` background throughout) could be because (a) `build_rust_status_line_spec` *does* preserve face runs and doom-modeline's dashboard-state LHS and RHS segments simply use the same resolved face, or (b) there's a separate face-merge bug dropping segment-specific face attributes. Without a test that captures doom running under real GNU Emacs at the same terminal size and compares the face-run structure, we can't distinguish. After the `:align-to` fix lands, re-capture and re-compare.
