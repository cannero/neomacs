use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{select, unbounded};
use neomacs_display_runtime::thread_comm::{InputEvent, RenderCommand, RenderComms};
use neovm_core::keyboard::{
    RENDER_CTRL_MASK, RENDER_META_MASK, RENDER_SHIFT_MASK, XK_BACKSPACE, XK_DELETE, XK_DOWN,
    XK_END, XK_F1, XK_HOME, XK_INSERT, XK_LEFT, XK_PAGE_DOWN, XK_PAGE_UP, XK_RETURN, XK_RIGHT,
    XK_TAB, XK_UP,
};

const ESC_SEQUENCE_TIMEOUT_MS: i32 = 25;
const INPUT_POLL_INTERVAL_MS: i32 = 100;
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
        match read_one_input_event(&stop) {
            Ok(Some(event)) => {
                tracing::info!("tty_input: got event {:?}", event);
                if tx.send(event).is_err() {
                    tracing::warn!("tty_input: channel closed");
                    break;
                }
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!("tty input reader stopped: {err}");
                break;
            }
        }
    }
}

fn read_one_input_event(stop: &AtomicBool) -> io::Result<Option<InputEvent>> {
    let Some(first_byte) = read_stdin_byte_blocking(stop)? else {
        return Ok(None);
    };

    let mut next_byte = |timeout_ms| read_stdin_byte(timeout_ms);
    let Some((keysym, modifiers)) = parse_tty_key_event(first_byte, &mut next_byte)? else {
        return Ok(None);
    };

    Ok(Some(InputEvent::Key {
        keysym,
        modifiers,
        pressed: true,
        emacs_frame_id: 0,
    }))
}

fn read_stdin_byte_blocking(stop: &AtomicBool) -> io::Result<Option<u8>> {
    while !stop.load(Ordering::Relaxed) {
        match read_stdin_byte(INPUT_POLL_INTERVAL_MS)? {
            Some(byte) => return Ok(Some(byte)),
            None => continue,
        }
    }
    Ok(None)
}

fn read_stdin_byte(timeout_ms: i32) -> io::Result<Option<u8>> {
    if !poll_stdin(timeout_ms)? {
        return Ok(None);
    }

    let mut byte = 0u8;
    loop {
        let n = unsafe { libc::read(libc::STDIN_FILENO, &mut byte as *mut u8 as *mut _, 1) };
        if n == 1 {
            return Ok(Some(byte));
        }
        if n == 0 {
            return Ok(None);
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EAGAIN) => return Ok(None),
            _ => return Err(err),
        }
    }
}

fn poll_stdin(timeout_ms: i32) -> io::Result<bool> {
    let mut pollfd = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };

    loop {
        let rc = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
        if rc > 0 {
            return Ok((pollfd.revents & (libc::POLLIN | libc::POLLHUP)) != 0);
        }
        if rc == 0 {
            return Ok(false);
        }

        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Err(err);
    }
}

fn parse_tty_key_event<F>(first: u8, next_byte: &mut F) -> io::Result<Option<(u32, u32)>>
where
    F: FnMut(i32) -> io::Result<Option<u8>>,
{
    if first != 0x1B {
        return parse_simple_key(first, next_byte);
    }

    let Some(second) = next_byte(ESC_SEQUENCE_TIMEOUT_MS)? else {
        return Ok(Some((0x1B, 0)));
    };

    match second {
        b'[' => parse_csi_sequence(next_byte),
        b'O' => parse_ss3_sequence(next_byte),
        0x7F => Ok(Some((0x7F, RENDER_META_MASK))),
        _ => Ok(parse_simple_key(second, next_byte)?
            .map(|(keysym, modifiers)| (keysym, modifiers | RENDER_META_MASK))),
    }
}

fn parse_simple_key<F>(first: u8, next_byte: &mut F) -> io::Result<Option<(u32, u32)>>
where
    F: FnMut(i32) -> io::Result<Option<u8>>,
{
    let key = match first {
        0 => Some((b'@' as u32, RENDER_CTRL_MASK)),
        // Preserve LF as raw C-j. GNU TTY input distinguishes Enter/RET
        // (typically CR) from literal linefeed, and commands like
        // `quoted-insert` rely on `C-q C-j` reaching Lisp as `?\C-j`
        // instead of a synthetic RET event.
        b'\r' => Some((XK_RETURN, 0)),
        b'\t' => Some((XK_TAB, 0)),
        // Match GNU's TTY split between help-char (`C-h` == 0x08) and
        // the physical Backspace keysym. GNU keeps raw 0x08 as a
        // control character for `help-char` and maps `[backspace]`
        // through `function-key-map` to `C-?` instead.
        0x7F => Some((XK_BACKSPACE, 0)),
        0x01..=0x1A => Some((((first - 1) + b'a') as u32, RENDER_CTRL_MASK)),
        0x1C => Some((b'\\' as u32, RENDER_CTRL_MASK)),
        0x1D => Some((b']' as u32, RENDER_CTRL_MASK)),
        0x1E => Some((b'^' as u32, RENDER_CTRL_MASK)),
        0x1F => Some((b'_' as u32, RENDER_CTRL_MASK)),
        0x20..=0x7E => Some((first as u32, 0)),
        0xC2..=0xF4 => decode_utf8_key(first, next_byte)?.map(|ch| (ch as u32, 0)),
        _ => None,
    };

    Ok(key)
}

fn decode_utf8_key<F>(first: u8, next_byte: &mut F) -> io::Result<Option<char>>
where
    F: FnMut(i32) -> io::Result<Option<u8>>,
{
    let len = match first {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return Ok(None),
    };

    let mut bytes = vec![first];
    for _ in 1..len {
        let Some(next) = next_byte(ESC_SEQUENCE_TIMEOUT_MS)? else {
            return Ok(None);
        };
        bytes.push(next);
    }

    Ok(std::str::from_utf8(&bytes)
        .ok()
        .and_then(|s| s.chars().next()))
}

fn parse_ss3_sequence<F>(next_byte: &mut F) -> io::Result<Option<(u32, u32)>>
where
    F: FnMut(i32) -> io::Result<Option<u8>>,
{
    let Some(final_byte) = next_byte(ESC_SEQUENCE_TIMEOUT_MS)? else {
        return Ok(Some((0x1B, 0)));
    };

    Ok(match final_byte {
        b'A' => Some((XK_UP, 0)),
        b'B' => Some((XK_DOWN, 0)),
        b'C' => Some((XK_RIGHT, 0)),
        b'D' => Some((XK_LEFT, 0)),
        b'F' => Some((XK_END, 0)),
        b'H' => Some((XK_HOME, 0)),
        b'P' => Some((XK_F1, 0)),
        b'Q' => Some((XK_F1 + 1, 0)),
        b'R' => Some((XK_F1 + 2, 0)),
        b'S' => Some((XK_F1 + 3, 0)),
        _ => None,
    })
}

fn parse_csi_sequence<F>(next_byte: &mut F) -> io::Result<Option<(u32, u32)>>
where
    F: FnMut(i32) -> io::Result<Option<u8>>,
{
    let mut bytes = Vec::new();
    loop {
        let Some(byte) = next_byte(ESC_SEQUENCE_TIMEOUT_MS)? else {
            return Ok(Some((0x1B, 0)));
        };
        bytes.push(byte);
        if (0x40..=0x7E).contains(&byte) || bytes.len() >= 16 {
            break;
        }
    }

    Ok(map_csi_sequence(&bytes))
}

fn map_csi_sequence(bytes: &[u8]) -> Option<(u32, u32)> {
    let (&final_byte, body) = bytes.split_last()?;
    if body.is_empty() {
        return Some(match final_byte {
            b'A' => (XK_UP, 0),
            b'B' => (XK_DOWN, 0),
            b'C' => (XK_RIGHT, 0),
            b'D' => (XK_LEFT, 0),
            b'F' => (XK_END, 0),
            b'H' => (XK_HOME, 0),
            b'Z' => (XK_TAB, RENDER_SHIFT_MASK),
            _ => return None,
        });
    }

    let body = std::str::from_utf8(body).ok()?;
    let body = body.strip_prefix('?').unwrap_or(body);
    let params: Vec<u16> = body
        .split(';')
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<u16>().ok())
        .collect::<Option<Vec<_>>>()?;
    let modifiers = params.get(1).copied().map(csi_modifier_bits).unwrap_or(0);

    match final_byte {
        b'A' => Some((XK_UP, modifiers)),
        b'B' => Some((XK_DOWN, modifiers)),
        b'C' => Some((XK_RIGHT, modifiers)),
        b'D' => Some((XK_LEFT, modifiers)),
        b'F' => Some((XK_END, modifiers)),
        b'H' => Some((XK_HOME, modifiers)),
        b'Z' => Some((XK_TAB, modifiers | RENDER_SHIFT_MASK)),
        b'~' => {
            let code = *params.first()?;
            let keysym = match code {
                1 | 7 => XK_HOME,
                2 => XK_INSERT,
                3 => XK_DELETE,
                4 | 8 => XK_END,
                5 => XK_PAGE_UP,
                6 => XK_PAGE_DOWN,
                11..=15 => XK_F1 + u32::from(code - 11),
                17..=21 => XK_F1 + u32::from(code - 12),
                23..=24 => XK_F1 + u32::from(code - 13),
                _ => return None,
            };
            Some((keysym, modifiers))
        }
        _ => None,
    }
}

fn csi_modifier_bits(modifier: u16) -> u32 {
    match modifier {
        2 => RENDER_SHIFT_MASK,
        3 => RENDER_META_MASK,
        4 => RENDER_SHIFT_MASK | RENDER_META_MASK,
        5 => RENDER_CTRL_MASK,
        6 => RENDER_SHIFT_MASK | RENDER_CTRL_MASK,
        7 => RENDER_META_MASK | RENDER_CTRL_MASK,
        8 => RENDER_SHIFT_MASK | RENDER_META_MASK | RENDER_CTRL_MASK,
        _ => 0,
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
