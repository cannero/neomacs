# neomacs-bin Rewrite: GNU Emacs-Compatible Command Loop

## Date: 2026-03-13

## Problem

`neomacs-bin/src/main.rs` is 5,800 lines of hardcoded keybinding dispatch that
duplicates what Elisp keymaps already provide. It manually implements C-x prefix
handling, minibuffer state machines, electric pairs, search, bookmarks, registers,
and dozens of editing commands — all in Rust. This approach:

- Will never scale to 100% Emacs compatibility
- Ignores user keybinding customizations
- Duplicates Elisp code that already exists
- Makes every new command require hand-coding in Rust

## Principle

- **C in GNU Emacs → Rust in neovm-core**
- **Elisp in GNU Emacs → just load the .el file**

## GNU Emacs C vs Elisp Boundary

### Implemented in C (→ must be Rust in neovm-core)

| C Function | File | Purpose |
|---|---|---|
| `Frecursive_edit()` | keyboard.c:772 | Enter nested command loop |
| `Ftop_level()` | keyboard.c:1187 | Throw to exit all recursive edits |
| `Fexit_recursive_edit()` | keyboard.c:1211 | Exit one recursive edit level |
| `Fabort_recursive_edit()` | keyboard.c:1222 | Abort one recursive edit level |
| `command_loop()` | keyboard.c:1104 | Top-level catch handler + loop |
| `command_loop_1()` | keyboard.c:1306 | Main loop: read → execute → redisplay |
| `command_loop_2()` | keyboard.c:1146 | Error handler wrapper |
| `read_key_sequence()` | keyboard.c:10098 | Multi-key sequence through keymaps |
| `read_char()` | keyboard.c:2489 | Blocking input read (THE blocking point) |
| `Fread_event()` | keyboard.c:9946 | Read single event |
| `Fread_char()` | keyboard.c:9897 | Read single character |
| `timer_check()` | keyboard.c:4644 | Fire expired timers |
| `sit_for()` | dispnew.c:5186 | Wait with input check |
| `Fsleep_for()` | dispnew.c:5133 | Block for duration |
| `wait_reading_process_output()` | process.c:5271 | fd multiplexer |
| `Fredisplay()` | dispnew.c:5259 | Trigger display update |
| `Fself_insert_command()` | cmds.c:263 | Insert character |
| `Finput_pending_p()` | keyboard.c:11273 | Check input queue |
| `Fdiscard_input()` | keyboard.c:11589 | Flush input queue |
| `Fread_from_minibuffer()` | minibuf.c:1284 | Minibuffer input with recursive edit |

### Implemented in Elisp (→ just load the .el file)

| Function | File | Notes |
|---|---|---|
| `command-execute` | simple.el | Calls `call-interactively` |
| `keyboard-quit` | simple.el | Signals 'quit |
| `normal-top-level` | startup.el | Startup sequence |
| `sit-for` (wrapper) | subr.el | Wraps C `sit_for()` |
| `timer-event-handler` | timer.el | Timer dispatch |
| All keybindings | bindings.el, mode .el files | Keymap definitions |
| All editing commands | simple.el, etc. | forward-word, kill-line, etc. |

## Architecture

```
neomacs-bin main() (~100 lines)
  │
  ├── init logging
  ├── bootstrap evaluator
  ├── create channels (ThreadComms)
  ├── spawn render thread
  ├── eval.init_input_system(input_rx, wakeup_fd, frame_tx, cmd_tx)
  ├── bootstrap buffers/frame
  ├── eval: (normal-top-level)  ← loads startup.el, init.el
  └── eval.recursive_edit()     ← never returns
        │
        └── command_loop()                    [neovm-core, Rust]
              └── command_loop_1()            [neovm-core, Rust]
                    │
                    ├── read_key_sequence()   [neovm-core, Rust]
                    │     └── read_char()     [neovm-core, Rust]
                    │           ├── redisplay()        ← layout + send frame
                    │           └── wait_for_input()   ← pselect(wakeup_fd | process_fds)
                    │
                    ├── key_binding()         [neovm-core, Rust, already implemented]
                    └── eval: (command-execute cmd)  ← Elisp in simple.el
```

## Evaluator Changes

neovm-core already has `CommandLoop`, `InputEvent`, `KeyEvent`, `KeySequence`,
`Modifiers`, and `PrefixArg` types in `keyboard.rs`. The `CommandLoop` struct
manages the event queue, prefix args, kbd macros, and quit flag.

New fields added to `Evaluator`:

```rust
// CommandLoop from keyboard.rs (event queue, prefix args, etc.)
pub(crate) command_loop: CommandLoop,

// Input from render thread — None in batch mode
pub input_rx: Option<crossbeam_channel::Receiver<InputEvent>>,

// Wakeup fd for pselect() multiplexing — None in batch mode
#[cfg(unix)]
pub wakeup_fd: Option<RawFd>,
```

Note: `neomacs-layout-engine` depends on `neovm-core`, so neovm-core cannot
depend on the layout engine (circular dependency). Redisplay is handled via
a callback or trait that neomacs-bin wires up.

## Key Functions

### `recursive_edit()` (replaces keyboard.c:772)

```rust
fn recursive_edit(&mut self) -> EvalResult {
    self.command_loop_level += 1;
    let result = self.internal_catch("exit", |eval| eval.command_loop());
    self.command_loop_level -= 1;
    match result {
        Ok(Value::True) => Err(signal("quit", vec![])),  // abort
        Ok(_) => Ok(Value::Nil),                          // normal exit
        Err(e) => Err(e),
    }
}
```

### `command_loop()` (replaces keyboard.c:1104)

```rust
fn command_loop(&mut self) -> EvalResult {
    loop {
        // Run startup form (top_level) on first entry
        self.internal_catch("top-level", |eval| {
            let top_level = eval.eval_symbol("top-level")?;
            eval.eval_value(&top_level)
        })?;
        // Then enter the actual command loop
        self.internal_catch("top-level", |eval| {
            eval.command_loop_2()
        })?;
    }
}
```

### `command_loop_1()` (replaces keyboard.c:1306)

```rust
fn command_loop_1(&mut self) -> EvalResult {
    loop {
        // Run post-command-hook
        self.run_hook("post-command-hook")?;

        // Read key sequence (blocks in read_char)
        let keys = self.read_key_sequence()?;

        // Look up binding
        let cmd = self.key_binding(&keys)?;

        // Set this-command
        self.this_command = cmd;

        // Run pre-command-hook
        self.run_hook("pre-command-hook")?;

        // Execute: (command-execute cmd)
        self.eval_form(&format!("(command-execute '{cmd})"))?;

        // Update last-command
        self.last_command = self.this_command;
    }
}
```

### `read_char()` (replaces keyboard.c:2489)

```rust
fn read_char(&mut self) -> EvalResult {
    // 1. Check unread-command-events
    if let Some(event) = self.pop_unread_command_event() {
        return Ok(event);
    }

    // 2. Check kbd macro playback
    if let Some(event) = self.kmacro.next_event() {
        return Ok(event);
    }

    // 3. Redisplay before blocking (same as GNU Emacs)
    self.redisplay();

    // 4. Block on input
    loop {
        if let Some(timer_event) = self.timer_check() {
            return Ok(timer_event);
        }
        self.wait_for_input(timeout);
        if let Some(event) = self.drain_input_to_event() {
            self.record_input_event(event);
            return Ok(event);
        }
        self.handle_process_output();
    }
}
```

### `wait_for_input()` (replaces wait_reading_process_output)

```rust
fn wait_for_input(&mut self, timeout: Option<Duration>) {
    let mut fds = FdSet::new();

    // Monitor wakeup fd (render thread input)
    if let Some(fd) = self.wakeup_fd {
        fds.insert(fd);
    }

    // Monitor process fds
    for fd in self.processes.active_fds() {
        fds.insert(fd);
    }

    // Compute timeout from timers
    let timer_timeout = self.timer_check_timeout();
    let effective_timeout = min(timeout, timer_timeout);

    // Block
    pselect(&mut fds, effective_timeout);
}
```

### Key event conversion (keysym → Emacs event)

```rust
fn keysym_to_emacs_event(keysym: u32, modifiers: u32) -> Value {
    let mut code = match keysym {
        0x20..=0x7E => keysym,                    // printable ASCII
        XK_RETURN => '\r' as u32,
        XK_TAB => '\t' as u32,
        XK_BACKSPACE => 0x7F,                     // DEL
        XK_ESCAPE => 0x1B,
        XK_LEFT..=XK_DOWN => return symbol_event(keysym),  // function keys
        _ => return symbol_event(keysym),
    };

    if modifiers & CTRL_MASK != 0 { code |= KEY_CHAR_CTRL; }
    if modifiers & META_MASK != 0 { code |= KEY_CHAR_META; }
    if modifiers & SHIFT_MASK != 0 { code |= KEY_CHAR_SHIFT; }
    if modifiers & SUPER_MASK != 0 { code |= KEY_CHAR_SUPER; }

    Value::Int(code as i64)
}
```

## Dependency Graph

```
neomacs-bin
  ├── neovm-core (evaluator + command loop + layout + redisplay)
  │     ├── neomacs-layout-engine
  │     └── neomacs-display-protocol (FrameGlyphBuffer, InputEvent, RenderCommand)
  └── neomacs-display-runtime (RenderThread, ThreadComms)
        └── neomacs-display-protocol
```

## Performance

No meaningful regression. The added overhead per keypress:
- Keymap lookup: ~1-10 microseconds (char-table O(1))
- Elisp command-execute: ~10-100 microseconds

Both negligible vs layout (~1-5ms) and VSync (~8-16ms).

## Implementation Phases

### Phase 1: Core command loop (minimum viable interactive editing)
- Add input system fields to Evaluator
- Implement `recursive_edit`, `command_loop`, `command_loop_1`, `command_loop_2`
- Implement `read_char` blocking on input channel
- Implement `redisplay` calling layout engine + sending frame
- Key event conversion (keysym → Emacs event integer)
- Basic `read_key_sequence` (single keys through keymaps)
- Rewrite neomacs-bin main.rs to ~100 lines

### Phase 2: Full input system
- Multi-key sequences in `read_key_sequence`
- `input-decode-map`, `function-key-map`, `key-translation-map`
- Mouse event conversion and dispatch
- `read-from-minibuffer` with recursive edit
- Prefix argument handling (C-u)

### Phase 3: Full I/O multiplexing
- `wait_for_input` with process fds
- Process filters/sentinels
- Timer integration (`timer_check`)
- `accept-process-output`
- Proper `sit-for` / `sleep-for`

### Phase 4: Polish
- Keyboard macro recording/playback through the command loop
- Focus switching between frames
- Error recovery in command_loop_2
- Echo area for partial key sequences
