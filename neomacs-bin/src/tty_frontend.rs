use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{Receiver, select, unbounded};
use neomacs_display_runtime::DisplayBackend;
use neomacs_display_runtime::FrameGlyphBuffer;
use neomacs_display_runtime::backend::tty::TtyBackend;
use neomacs_display_runtime::core::Scene;
use neomacs_display_runtime::thread_comm::{InputEvent, RenderCommand, RenderComms};
use neovm_core::keyboard::{
    RENDER_CTRL_MASK, RENDER_META_MASK, RENDER_SHIFT_MASK, XK_BACKSPACE, XK_DELETE, XK_DOWN,
    XK_END, XK_ESCAPE, XK_F1, XK_HOME, XK_INSERT, XK_LEFT, XK_PAGE_DOWN, XK_PAGE_UP, XK_RETURN,
    XK_RIGHT, XK_TAB, XK_UP,
};

const ESC_SEQUENCE_TIMEOUT_MS: i32 = 25;
const INPUT_POLL_INTERVAL_MS: i32 = 100;

pub struct TtyFrontend {
    handle: Option<JoinHandle<()>>,
}

impl TtyFrontend {
    pub fn spawn(comms: RenderComms) -> Self {
        let handle = thread::spawn(move || run_tty_frontend(comms));
        Self {
            handle: Some(handle),
        }
    }

    pub fn join(mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn run_tty_frontend(comms: RenderComms) {
    let mut backend = TtyBackend::new();
    if let Err(err) = backend.init() {
        tracing::error!("failed to initialize tty backend: {err}");
        return;
    }

    let stop_input = Arc::new(AtomicBool::new(false));
    let (input_tx, input_rx) = unbounded();
    let input_stop = Arc::clone(&stop_input);
    let input_handle = thread::Builder::new()
        .name("tty-input".to_string())
        .spawn(move || read_tty_input(input_tx, input_stop))
        .ok();

    loop {
        select! {
            recv(comms.cmd_rx) -> msg => {
                match msg {
                    Ok(RenderCommand::Shutdown) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            recv(comms.frame_rx) -> msg => {
                let Ok(first_frame) = msg else {
                    break;
                };
                let frame = latest_frame(first_frame, &comms.frame_rx);
                if let Err(err) = render_frame(&mut backend, frame) {
                    tracing::error!("tty render failed: {err}");
                    break;
                }
            }
            recv(input_rx) -> msg => {
                match msg {
                    Ok(event) => comms.send_input(event),
                    Err(_) => break,
                }
            }
            default(Duration::from_millis(50)) => {}
        }
    }

    stop_input.store(true, Ordering::Relaxed);
    if let Some(handle) = input_handle {
        let _ = handle.join();
    }
}

fn latest_frame(mut frame: FrameGlyphBuffer, rx: &Receiver<FrameGlyphBuffer>) -> FrameGlyphBuffer {
    while let Ok(next) = rx.try_recv() {
        frame = next;
    }
    frame
}

fn render_frame(backend: &mut TtyBackend, frame: FrameGlyphBuffer) -> Result<(), String> {
    let mut scene = Scene::new(frame.width, frame.height);
    scene.background = frame.background;
    backend.set_frame_glyphs(frame);
    backend.render(&scene).map_err(|err| err.to_string())?;
    backend.present().map_err(|err| err.to_string())?;
    Ok(())
}

fn read_tty_input(tx: crossbeam_channel::Sender<InputEvent>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        match read_one_input_event(&stop) {
            Ok(Some(event)) => {
                if tx.send(event).is_err() {
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
        return Ok(Some((XK_ESCAPE, 0)));
    };

    match second {
        b'[' => parse_csi_sequence(next_byte),
        b'O' => parse_ss3_sequence(next_byte),
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
        b'\r' | b'\n' => Some((XK_RETURN, 0)),
        b'\t' => Some((XK_TAB, 0)),
        0x08 | 0x7F => Some((XK_BACKSPACE, 0)),
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
        return Ok(Some((XK_ESCAPE, 0)));
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
            return Ok(Some((XK_ESCAPE, 0)));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn parse_key_bytes(bytes: &[u8]) -> Option<(u32, u32)> {
        let mut queue: VecDeque<u8> = bytes.iter().copied().collect();
        let first = queue.pop_front()?;
        let mut next_byte = |_timeout_ms| Ok(queue.pop_front());
        parse_tty_key_event(first, &mut next_byte).expect("parser should not error")
    }

    #[test]
    fn parses_ascii_keypress() {
        assert_eq!(parse_key_bytes(b"x"), Some((b'x' as u32, 0)));
    }

    #[test]
    fn parses_ctrl_keypress() {
        assert_eq!(
            parse_key_bytes(&[0x18]),
            Some((b'x' as u32, RENDER_CTRL_MASK))
        );
        assert_eq!(
            parse_key_bytes(&[0x00]),
            Some((b'@' as u32, RENDER_CTRL_MASK))
        );
        assert_eq!(parse_key_bytes(b"\t"), Some((XK_TAB, 0)));
        assert_eq!(parse_key_bytes(b"\n"), Some((XK_RETURN, 0)));
        assert_eq!(parse_key_bytes(&[0x08]), Some((XK_BACKSPACE, 0)));
    }

    #[test]
    fn parses_meta_keypress() {
        assert_eq!(
            parse_key_bytes(&[0x1B, b'x']),
            Some((b'x' as u32, RENDER_META_MASK))
        );
    }

    #[test]
    fn parses_utf8_keypress() {
        assert_eq!(parse_key_bytes("中".as_bytes()), Some(('中' as u32, 0)));
    }

    #[test]
    fn parses_escape_and_arrow_sequences() {
        assert_eq!(parse_key_bytes(&[0x1B]), Some((XK_ESCAPE, 0)));
        assert_eq!(parse_key_bytes(&[0x1B, b'[', b'A']), Some((XK_UP, 0)));
        assert_eq!(
            parse_key_bytes(&[0x1B, b'[', b'1', b';', b'5', b'A']),
            Some((XK_UP, RENDER_CTRL_MASK))
        );
    }

    #[test]
    fn parses_tilde_sequences() {
        assert_eq!(
            parse_key_bytes(&[0x1B, b'[', b'3', b'~']),
            Some((XK_DELETE, 0))
        );
        assert_eq!(
            parse_key_bytes(&[0x1B, b'[', b'5', b'~']),
            Some((XK_PAGE_UP, 0))
        );
    }
}
