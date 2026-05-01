use super::*;

// ── parse_tty_byte ────────────────────────────────────────────────────

#[test]
fn parse_tty_byte_meta_key_0_clears_high_bit() {
    // meta_key 0: 8th bit is cleared, no modifiers
    assert_eq!(parse_tty_byte(b'x', 0), (b'x' as u32, 0));
    assert_eq!(parse_tty_byte(b'A', 0), (b'A' as u32, 0));
    // Control characters pass through with high bit already clear
    assert_eq!(parse_tty_byte(0x01, 0), (0x01, 0));
    assert_eq!(parse_tty_byte(0x00, 0), (0x00, 0));
    assert_eq!(parse_tty_byte(0x1F, 0), (0x1F, 0));
    // 8-bit byte: high bit cleared
    assert_eq!(parse_tty_byte(0x80, 0), (0x00, 0));
    assert_eq!(parse_tty_byte(0xFF, 0), (0x7F, 0));
}

#[test]
fn parse_tty_byte_meta_key_1_adds_meta_for_high_bit() {
    // meta_key 1: if 8th bit set, clear it and add Meta modifier
    assert_eq!(parse_tty_byte(b'x', 1), (b'x' as u32, 0));
    assert_eq!(
        parse_tty_byte(0x80 | b'x', 1),
        (b'x' as u32, RENDER_META_MASK)
    );
    // byte with high bit already clear: no Meta
    assert_eq!(parse_tty_byte(0x01, 1), (0x01, 0));
    assert_eq!(parse_tty_byte(0x7F, 1), (0x7F, 0));
}

#[test]
fn parse_tty_byte_meta_key_2_passes_through_raw() {
    // meta_key 2: no bit manipulation, raw byte pass-through
    assert_eq!(parse_tty_byte(b'x', 2), (b'x' as u32, 0));
    assert_eq!(parse_tty_byte(0x80, 2), (0x80, 0));
    assert_eq!(parse_tty_byte(0xFF, 2), (0xFF, 0));
}

// ── decode_utf8_from_slice ─────────────────────────────────────────────

#[test]
fn decode_utf8_from_slice_ascii_returns_none() {
    let buf = b"hello";
    let mut pos = 0;
    assert_eq!(decode_utf8_from_slice(buf, &mut pos), None);
    assert_eq!(pos, 0); // position unchanged
}

#[test]
fn decode_utf8_from_slice_2_byte_sequence() {
    // U+00E4 (ä) = 0xC3 0xA4
    let buf = &[0xC3, 0xA4, b'x'];
    let mut pos = 0;
    assert_eq!(decode_utf8_from_slice(buf, &mut pos), Some('ä'));
    assert_eq!(pos, 2);
}

#[test]
fn decode_utf8_from_slice_3_byte_sequence() {
    // U+4E2D (中) = 0xE4 0xB8 0xAD
    let buf = &[0xE4, 0xB8, 0xAD, b'y'];
    let mut pos = 0;
    assert_eq!(decode_utf8_from_slice(buf, &mut pos), Some('中'));
    assert_eq!(pos, 3);
}

#[test]
fn decode_utf8_from_slice_4_byte_sequence() {
    // U+1F600 (😀) = 0xF0 0x9F 0x98 0x80
    let buf = &[0xF0, 0x9F, 0x98, 0x80, b'z'];
    let mut pos = 0;
    assert_eq!(decode_utf8_from_slice(buf, &mut pos), Some('😀'));
    assert_eq!(pos, 4);
}

#[test]
fn decode_utf8_from_slice_incomplete_sequence_returns_none() {
    // 3-byte lead but only 2 bytes available
    let buf = &[0xE4, 0xB8];
    let mut pos = 0;
    assert_eq!(decode_utf8_from_slice(buf, &mut pos), None);
    assert_eq!(pos, 0); // position unchanged
}

#[test]
fn decode_utf8_from_slice_invalid_continuation_returns_none() {
    // 0xC2 must be followed by a continuation byte (0x80-0xBF)
    let buf = &[0xC2, b'x'];
    let mut pos = 0;
    assert_eq!(decode_utf8_from_slice(buf, &mut pos), None);
    assert_eq!(pos, 0);
}

// ── emit_events_from_bytes ─────────────────────────────────────────────

fn event_keysyms(events: &[InputEvent]) -> Vec<u32> {
    events
        .iter()
        .map(|e| match e {
            InputEvent::Key { keysym, .. } => *keysym,
            _ => panic!("unexpected event type"),
        })
        .collect()
}

fn event_modifiers(events: &[InputEvent]) -> Vec<u32> {
    events
        .iter()
        .map(|e| match e {
            InputEvent::Key { modifiers, .. } => *modifiers,
            _ => panic!("unexpected event type"),
        })
        .collect()
}

#[test]
fn emit_ascii_bytes_as_individual_events() {
    let events = emit_events_from_bytes(b"abc", 0);
    assert_eq!(
        event_keysyms(&events),
        vec![b'a' as u32, b'b' as u32, b'c' as u32]
    );
    assert_eq!(event_modifiers(&events), vec![0, 0, 0]);
}

#[test]
fn emit_esc_sequence_as_individual_byte_events() {
    // \e[A → three separate byte events: ESC, '[', 'A'
    let events = emit_events_from_bytes(&[0x1B, b'[', b'A'], 0);
    assert_eq!(event_keysyms(&events), vec![0x1B, b'[' as u32, b'A' as u32]);
}

#[test]
fn emit_xterm_da_response_as_individual_bytes() {
    // \e[?1;2c → ESC, '[', '?', '1', ';', '2', 'c'
    let events = emit_events_from_bytes(&[0x1B, b'[', b'?', b'1', b';', b'2', b'c'], 0);
    assert_eq!(
        event_keysyms(&events),
        vec![
            0x1B,
            b'[' as u32,
            b'?' as u32,
            b'1' as u32,
            b';' as u32,
            b'2' as u32,
            b'c' as u32
        ]
    );
}

#[test]
fn emit_control_bytes_as_individual_events() {
    // Ctrl-A, Ctrl-B, Ctrl-X
    let events = emit_events_from_bytes(&[0x01, 0x02, 0x18], 0);
    assert_eq!(event_keysyms(&events), vec![0x01, 0x02, 0x18]);
    assert_eq!(event_modifiers(&events), vec![0, 0, 0]);
}

#[test]
fn emit_utf8_character_as_single_event() {
    let mut buf = vec![];
    buf.extend_from_slice("中".as_bytes());
    assert_eq!(buf, vec![0xE4, 0xB8, 0xAD]);

    let events = emit_events_from_bytes(&buf, 0);
    assert_eq!(event_keysyms(&events), vec!['中' as u32]);
    assert_eq!(event_modifiers(&events), vec![0]);
}

#[test]
fn emit_mixed_ascii_and_utf8() {
    let mut buf = vec![b'a'];
    buf.extend_from_slice("ä".as_bytes()); // 0xC3 0xA4
    buf.push(b'z');

    let events = emit_events_from_bytes(&buf, 0);
    assert_eq!(
        event_keysyms(&events),
        vec![b'a' as u32, 'ä' as u32, b'z' as u32]
    );
}

#[test]
fn emit_empty_buffer_returns_no_events() {
    let events = emit_events_from_bytes(&[], 0);
    assert!(events.is_empty());
}

#[test]
fn emit_incomplete_utf8_at_end_emits_lead_byte() {
    // 0xE4 is a 3-byte lead but no continuation bytes follow
    let events = emit_events_from_bytes(&[0xE4], 0);
    // 0xE4 & 0x7F = 0x64 = 'd'
    assert_eq!(event_keysyms(&events), vec![0x64]);
}

#[test]
fn emit_cr_and_tab_as_individual_bytes() {
    let events = emit_events_from_bytes(b"\r\t", 0);
    assert_eq!(event_keysyms(&events), vec![b'\r' as u32, b'\t' as u32]);
}

#[test]
fn emit_meta_key_0_clears_high_bit_for_non_utf8() {
    // 0xFF is not a valid UTF-8 lead byte, emitted as raw byte
    let events = emit_events_from_bytes(&[0xFF], 0);
    assert_eq!(event_keysyms(&events), vec![0x7F]); // 0xFF & 0x7F = 0x7F
}

// ── Resize logic (unchanged) ───────────────────────────────────────────

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
