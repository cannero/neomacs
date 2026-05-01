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
    assert_eq!(parse_key_bytes(b"\r"), Some((XK_RETURN, 0)));
    assert_eq!(
        parse_key_bytes(b"\n"),
        Some((b'j' as u32, RENDER_CTRL_MASK))
    );
    assert_eq!(
        parse_key_bytes(&[0x08]),
        Some((b'h' as u32, RENDER_CTRL_MASK))
    );
    assert_eq!(parse_key_bytes(&[0x7F]), Some((XK_BACKSPACE, 0)));
}

#[test]
fn parses_meta_keypress() {
    assert_eq!(
        parse_key_bytes(&[0x1B, b'x']),
        Some((b'x' as u32, RENDER_META_MASK))
    );
    assert_eq!(
        parse_key_bytes(&[0x1B, 0x7F]),
        Some((0x7F, RENDER_META_MASK))
    );
}

#[test]
fn parses_utf8_keypress() {
    assert_eq!(parse_key_bytes("中".as_bytes()), Some(('中' as u32, 0)));
}

#[test]
fn parses_escape_and_arrow_sequences() {
    assert_eq!(parse_key_bytes(&[0x1B]), Some((0x1B, 0)));
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

#[test]
fn tty_resize_event_tracks_signal_and_dimension_changes() {
    let mut last_size = Some((160, 50));

    assert!(tty_resize_event_for_size(&mut last_size, Some((160, 50)), false).is_none());

    let signal_event = tty_resize_event_for_size(&mut last_size, Some((160, 50)), true)
        .expect("SIGWINCH should forward the current size");
    assert!(matches!(
        signal_event,
        InputEvent::WindowResize {
            width: 160,
            height: 50,
            emacs_frame_id: 0,
        }
    ));

    let changed_event = tty_resize_event_for_size(&mut last_size, Some((100, 30)), false)
        .expect("dimension change should forward a resize");
    assert!(matches!(
        changed_event,
        InputEvent::WindowResize {
            width: 100,
            height: 30,
            emacs_frame_id: 0,
        }
    ));
    assert_eq!(last_size, Some((100, 30)));

    assert!(tty_resize_event_for_size(&mut last_size, None, true).is_none());
    assert!(tty_resize_event_for_size(&mut last_size, Some((0, 30)), true).is_none());
}
