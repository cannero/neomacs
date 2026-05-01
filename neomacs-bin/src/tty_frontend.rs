use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{select, unbounded};
use neomacs_display_runtime::thread_comm::{InputEvent, RenderCommand, RenderComms};

const RESIZE_POLL_INTERVAL_MS: u64 = 100;

#[cfg(unix)]
static TTY_RESIZE_PENDING: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
static INSTALL_SIGWINCH_HANDLER: std::sync::Once = std::sync::Once::new();

#[cfg(unix)]
extern "C" fn handle_sigwinch(_: libc::c_int) {
    TTY_RESIZE_PENDING.store(true, Ordering::Relaxed);
}

#[cfg(unix)]
fn install_tty_resize_handler() {
    INSTALL_SIGWINCH_HANDLER.call_once(|| unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = handle_sigwinch as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        action.sa_flags = 0;
        if libc::sigaction(libc::SIGWINCH, &action, std::ptr::null_mut()) != 0 {
            tracing::warn!(
                "tty_resize: failed to install SIGWINCH handler: {}",
                io::Error::last_os_error()
            );
        }
    });
}

#[cfg(unix)]
fn query_terminal_size_cells() -> Option<(u32, u32)> {
    use std::mem::MaybeUninit;

    unsafe {
        let mut winsize = MaybeUninit::<libc::winsize>::uninit();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, winsize.as_mut_ptr()) == 0 {
            let winsize = winsize.assume_init();
            if winsize.ws_col > 0 && winsize.ws_row > 0 {
                return Some((u32::from(winsize.ws_col), u32::from(winsize.ws_row)));
            }
        }
    }
    None
}

fn tty_resize_event_for_size(
    last_size: &mut Option<(u32, u32)>,
    current_size: Option<(u32, u32)>,
    signal_pending: bool,
) -> Option<InputEvent> {
    let (width, height) = current_size?;
    if width == 0 || height == 0 {
        return None;
    }

    let changed = *last_size != Some((width, height));
    if !signal_pending && !changed {
        return None;
    }

    *last_size = Some((width, height));
    Some(InputEvent::WindowResize {
        width,
        height,
        emacs_frame_id: 0,
    })
}

#[cfg(unix)]
fn spawn_tty_resize_watcher(
    tx: crossbeam_channel::Sender<InputEvent>,
    stop: Arc<AtomicBool>,
) -> Option<JoinHandle<()>> {
    install_tty_resize_handler();
    let handle = thread::Builder::new()
        .name("tty-resize-watch".to_string())
        .spawn(move || {
            let mut last_size = query_terminal_size_cells();
            while !stop.load(Ordering::Relaxed) {
                let signal_pending = TTY_RESIZE_PENDING.swap(false, Ordering::Relaxed);
                if let Some(event) = tty_resize_event_for_size(
                    &mut last_size,
                    query_terminal_size_cells(),
                    signal_pending,
                ) {
                    tracing::debug!("tty_resize: forwarding resize event {:?}", event);
                    if tx.send(event).is_err() {
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(RESIZE_POLL_INTERVAL_MS));
            }
        })
        .ok()?;
    Some(handle)
}

#[cfg(not(unix))]
fn spawn_tty_resize_watcher(
    _tx: crossbeam_channel::Sender<InputEvent>,
    _stop: Arc<AtomicBool>,
) -> Option<JoinHandle<()>> {
    None
}

// ── TTY byte input (GNU-compatible raw byte stream) ───────────────────
//
// Matches GNU Emacs's `tty_read_avail_input` (keyboard.c:8134-8307):
// every byte read from the terminal becomes an ASCII_KEYSTROKE_EVENT.
// Escape sequences are NOT parsed at the Rust level — translation of
// \e[A → [up] etc. happens at the Lisp level via input-decode-map.

/// Convert a single raw TTY byte into a (keysym, modifiers) pair.
///
/// `meta_key` controls 8-bit interpretation, matching GNU's per-terminal
/// `tty->meta_key` mode:
///   - 0: clear the 8th bit, no Meta modifier (default for UTF-8 terminals)
///   - 1: if the 8th bit is set, clear it and add Meta modifier
///   - 2: pass every byte through unchanged (raw 8-bit / coding-system)
fn parse_tty_byte(byte: u8, meta_key: u8) -> (u32, u32) {
    match meta_key {
        0 => (u32::from(byte & 0x7F), 0),
        1 => {
            if byte & 0x80 != 0 {
                (u32::from(byte & 0x7F), RENDER_META_MASK)
            } else {
                (u32::from(byte), 0)
            }
        }
        _ => (u32::from(byte), 0),
    }
}

/// Bitmask constants for the frontend modifier word.
/// These match the values expected by `keyboard::render_modifiers_to_modifiers`.
const RENDER_META_MASK: u32 = 1 << 27;

/// Try to decode a UTF-8 multi-byte character starting at `bytes[*pos]`.
/// On success, advances `*pos` past the consumed bytes and returns the char.
/// Returns `None` if `bytes[*pos]` is not a UTF-8 lead byte or the sequence
/// is incomplete.
fn decode_utf8_from_slice(bytes: &[u8], pos: &mut usize) -> Option<char> {
    let first = bytes[*pos];
    let len = match first {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return None,
    };

    let end = *pos + len;
    if end > bytes.len() {
        return None;
    }

    let s = std::str::from_utf8(&bytes[*pos..end]).ok()?;
    let ch = s.chars().next()?;
    *pos = end;
    Some(ch)
}

/// Convert a batch of raw TTY bytes into `InputEvent::Key` events.
///
/// Each byte (or decoded UTF-8 character) becomes its own event, matching
/// GNU's `tty_read_avail_input` per-byte `ASCII_KEYSTROKE_EVENT` loop.
///
/// UTF-8 lead bytes (0xC2..=0xF4) trigger multi-byte decoding; all other
/// bytes are emitted individually via `parse_tty_byte`.
fn emit_events_from_bytes(bytes: &[u8], meta_key: u8) -> Vec<InputEvent> {
    let mut events = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if (0xC2..=0xF4).contains(&byte) {
            if let Some(ch) = decode_utf8_from_slice(bytes, &mut i) {
                events.push(InputEvent::Key {
                    keysym: ch as u32,
                    modifiers: 0,
                    pressed: true,
                    emacs_frame_id: 0,
                });
            } else {
                // Incomplete UTF-8 sequence at end of buffer: emit the
                // lead byte as-is.  The continuation bytes will arrive
                // in the next read and be emitted individually.
                let (keysym, modifiers) = parse_tty_byte(byte, meta_key);
                events.push(InputEvent::Key {
                    keysym,
                    modifiers,
                    pressed: true,
                    emacs_frame_id: 0,
                });
                i += 1;
            }
        } else {
            let (keysym, modifiers) = parse_tty_byte(byte, meta_key);
            events.push(InputEvent::Key {
                keysym,
                modifiers,
                pressed: true,
                emacs_frame_id: 0,
            });
            i += 1;
        }
    }
    events
}

/// Block until stdin has data available or `stop` is set.
///
/// Returns `Ok(true)` when data is ready, `Ok(false)` when stopped.
fn poll_stdin_blocking(stop: &AtomicBool) -> io::Result<bool> {
    let mut pollfd = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };

    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(false);
        }

        let rc = unsafe { libc::poll(&mut pollfd, 1, 50) };
        if rc > 0 {
            return Ok((pollfd.revents & (libc::POLLIN | libc::POLLHUP)) != 0);
        }
        if rc == 0 {
            continue;
        }

        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Err(err);
    }
}

/// Read available bytes from stdin into `buf`.
///
/// Returns the number of bytes read, 0 on EOF, or an error.
fn read_stdin_bytes(buf: &mut [u8]) -> io::Result<usize> {
    loop {
        let n = unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr() as *mut _, buf.len()) };
        if n > 0 {
            return Ok(n as usize);
        }
        if n == 0 {
            return Ok(0);
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EAGAIN) => return Ok(0),
            _ => return Err(err),
        }
    }
}

/// Read one batch of input events from stdin.
///
/// Blocks until data arrives or `stop` is set, then reads all available
/// bytes and converts them to `InputEvent::Key` events.
fn read_batch_input_events(stop: &AtomicBool) -> io::Result<Vec<InputEvent>> {
    if !poll_stdin_blocking(stop)? {
        return Ok(Vec::new());
    }

    let mut buf = [0u8; 64];
    match read_stdin_bytes(&mut buf)? {
        0 => Ok(Vec::new()),
        n => Ok(emit_events_from_bytes(&buf[..n], /* meta_key= */ 0)),
    }
}

fn read_tty_input(
    tx: crossbeam_channel::Sender<InputEvent>,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Relaxed) {
        if paused.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(25));
            continue;
        }
        match read_batch_input_events(&stop) {
            Ok(events) => {
                for event in events {
                    tracing::info!("tty_input: got event {:?}", event);
                    if tx.send(event).is_err() {
                        tracing::warn!("tty_input: channel closed");
                        return;
                    }
                }
            }
            Err(err) => {
                tracing::warn!("tty input reader stopped: {err}");
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Standalone TTY input reader (for TtyRif single-thread redisplay path)
// ---------------------------------------------------------------------------

/// A standalone TTY input reader that forwards terminal key events to
/// `RenderComms` without running a full `TtyFrontend` render loop.
/// Used by the `-nw` path when rendering goes through `TtyRif` on the
/// evaluator thread.
pub struct TtyInputReader {
    handle: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

impl TtyInputReader {
    /// Spawn a background thread that reads terminal input and sends events
    /// through `comms.send_input()`.
    pub fn spawn(comms: RenderComms) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let input_stop = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name("tty-input-reader".to_string())
            .spawn(move || {
                let pause = Arc::new(AtomicBool::new(false));
                let (tx, rx) = unbounded();
                let reader_stop = Arc::clone(&input_stop);
                let reader_pause = Arc::clone(&pause);
                let resize_handle = spawn_tty_resize_watcher(tx.clone(), Arc::clone(&input_stop));
                let reader_handle = thread::Builder::new()
                    .name("tty-input-raw".to_string())
                    .spawn(move || read_tty_input(tx, reader_stop, reader_pause))
                    .ok();

                // Forward raw input events to the RenderComms channel, and
                // listen for shutdown commands.
                loop {
                    select! {
                        recv(comms.cmd_rx) -> msg => {
                            match msg {
                                Ok(RenderCommand::Shutdown) | Err(_) => break,
                                Ok(_) => {}
                            }
                        }
                        recv(rx) -> msg => {
                            match msg {
                                Ok(event) => comms.send_input(event),
                                Err(_) => break,
                            }
                        }
                        default(Duration::from_millis(50)) => {}
                    }
                }

                input_stop.store(true, Ordering::Relaxed);
                if let Some(h) = reader_handle {
                    let _ = h.join();
                }
                if let Some(h) = resize_handle {
                    let _ = h.join();
                }
            })
            .expect("Failed to spawn tty-input-reader thread");

        Self {
            handle: Some(handle),
            stop,
        }
    }

    /// Signal the input reader to stop and wait for it to finish.
    pub fn join(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
#[path = "tty_frontend_test.rs"]
mod tests;
