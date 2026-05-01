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

fn drain_unread_buffer() -> Vec<u8> {
    let mut out = Vec::new();
    STDIN_UNREAD.with(|buf| {
        let mut buf = buf.borrow_mut();
        while let Some(b) = buf.pop() {
            out.push(b);
        }
    });
    out
}

#[test]
fn unrecognized_csi_sequence_emits_esc_and_unreads_remainder() {
    // Ensure clean state
    drain_unread_buffer();

    // Terminal DA response \e[?1;2c — final byte 'c' is not in the
    // recognized set (A-H, Z, ~), so map_csi_sequence returns None.
    let result = parse_key_bytes(&[0x1B, b'[', b'?', b'1', b';', b'2', b'c']);
    assert_eq!(result, Some((0x1B, 0)));

    let unread = drain_unread_buffer();
    assert_eq!(unread, vec![b'[', b'?', b'1', b';', b'2', b'c']);
}

#[test]
fn unrecognized_csi_with_modifier_params_emits_esc_and_unreads_remainder() {
    drain_unread_buffer();

    // DA2 response with version: \e[>1;2c
    let result = parse_key_bytes(&[0x1B, b'[', b'>', b'1', b';', b'2', b'c']);
    assert_eq!(result, Some((0x1B, 0)));

    let unread = drain_unread_buffer();
    assert_eq!(unread, vec![b'[', b'>', b'1', b';', b'2', b'c']);
}

#[test]
fn osc_sequence_emits_esc_and_unreads_payload() {
    drain_unread_buffer();

    // OSC color query: \e]11;?\a
    let result = parse_key_bytes(&[0x1B, b']', b'1', b'1', b';', b'?', 0x07]);
    assert_eq!(result, Some((0x1B, 0)));

    let unread = drain_unread_buffer();
    assert_eq!(unread, vec![b']', b'1', b'1', b';', b'?', 0x07]);
}

#[test]
fn osc_sequence_st_terminator_emits_esc_and_unreads_payload() {
    drain_unread_buffer();

    // OSC with ST terminator: \e]0;test\e\\
    let result = parse_key_bytes(&[0x1B, b']', b'0', b';', b't', b'e', b's', b't', 0x1B, 0x5C]);
    assert_eq!(result, Some((0x1B, 0)));

    let unread = drain_unread_buffer();
    assert_eq!(
        unread,
        vec![b']', b'0', b';', b't', b'e', b's', b't', 0x1B, 0x5C]
    );
}

#[test]
fn unrecognized_ss3_sequence_emits_esc_and_unreads_remainder() {
    drain_unread_buffer();

    // \eO followed by unrecognized final byte
    let result = parse_key_bytes(&[0x1B, b'O', b'X']);
    assert_eq!(result, Some((0x1B, 0)));

    let unread = drain_unread_buffer();
    assert_eq!(unread, vec![b'O', b'X']);
}

#[test]
fn unrecognized_csi_empty_body_emits_esc_and_unreads() {
    drain_unread_buffer();

    // CSI with final byte that's not in the recognized set (e.g. \e[c with no params)
    let result = parse_key_bytes(&[0x1B, b'[', b'c']);
    assert_eq!(result, Some((0x1B, 0)));

    let unread = drain_unread_buffer();
    assert_eq!(unread, vec![b'[', b'c']);
}
