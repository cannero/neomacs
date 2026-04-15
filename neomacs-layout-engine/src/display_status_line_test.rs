use super::*;

// ---------------------------------------------------------------
// Helper: build a 14-byte face run record (native-endian)
// ---------------------------------------------------------------
fn make_run_bytes(byte_offset: u16, fg: u32, bg: u32) -> [u8; 14] {
    let mut rec = [0u8; 14];
    rec[0..2].copy_from_slice(&byte_offset.to_ne_bytes());
    rec[2..6].copy_from_slice(&fg.to_ne_bytes());
    rec[6..10].copy_from_slice(&bg.to_ne_bytes());
    // face_id defaults to 0
    rec
}

// ---------------------------------------------------------------
// StatusLineKind enum
// ---------------------------------------------------------------

#[test]
fn status_line_kind_variants_exist() {
    // Ensure all variants can be constructed (compile-time check
    // made explicit).
    let _ml = StatusLineKind::ModeLine;
    let _hl = StatusLineKind::HeaderLine;
    let _tl = StatusLineKind::TabLine;
}

#[test]
fn status_line_kind_is_distinct() {
    // Discriminants should differ (match each variant).
    let check = |k: &StatusLineKind| -> u8 {
        match k {
            StatusLineKind::ModeLine => 0,
            StatusLineKind::HeaderLine => 1,
            StatusLineKind::TabLine => 2,
        }
    };
    assert_eq!(check(&StatusLineKind::ModeLine), 0);
    assert_eq!(check(&StatusLineKind::HeaderLine), 1);
    assert_eq!(check(&StatusLineKind::TabLine), 2);
}

// ---------------------------------------------------------------
// OverlayFaceRun struct
// ---------------------------------------------------------------

#[test]
fn overlay_face_run_construction_defaults() {
    let run = OverlayFaceRun {
        byte_offset: 0,
        fg: 0,
        bg: 0,
        extend: false,
        face_id: 0,
    };
    assert_eq!(run.byte_offset, 0);
    assert_eq!(run.fg, 0);
    assert_eq!(run.bg, 0);
    assert_eq!(run.extend, false);
}

#[test]
fn overlay_face_run_construction_max_values() {
    let run = OverlayFaceRun {
        byte_offset: u16::MAX,
        fg: u32::MAX,
        bg: u32::MAX,
        extend: true,
        face_id: 0,
    };
    assert_eq!(run.byte_offset, u16::MAX);
    assert_eq!(run.fg, u32::MAX);
    assert_eq!(run.bg, u32::MAX);
    assert_eq!(run.extend, true);
}

#[test]
fn overlay_face_run_construction_typical() {
    // Typical Emacs color values: 0x00RRGGBB
    let run = OverlayFaceRun {
        byte_offset: 42,
        fg: 0x00FFFFFF,
        bg: 0x00000000,
        extend: false,
        face_id: 0,
    };
    assert_eq!(run.byte_offset, 42);
    assert_eq!(run.fg, 0x00FFFFFF);
    assert_eq!(run.bg, 0x00000000);
    assert_eq!(run.extend, false);
}

// ---------------------------------------------------------------
// parse_overlay_face_runs: empty / zero
// ---------------------------------------------------------------

#[test]
fn parse_empty_buffer_zero_runs() {
    let buf: &[u8] = &[];
    let runs = parse_overlay_face_runs(buf, 0, 0);
    assert!(runs.is_empty());
}

#[test]
fn parse_zero_runs_with_text() {
    // Buffer has text but no face runs requested.
    let buf = b"Hello, world!";
    let runs = parse_overlay_face_runs(buf, buf.len(), 0);
    assert!(runs.is_empty());
}

// ---------------------------------------------------------------
// parse_overlay_face_runs: single run
// ---------------------------------------------------------------

#[test]
fn parse_single_run() {
    let text = b"Hello";
    let text_len = text.len(); // 5
    let rec = make_run_bytes(0, 0x00FF0000, 0x0000FF00);

    let mut buf = Vec::from(&text[..]);
    buf.extend_from_slice(&rec);

    let runs = parse_overlay_face_runs(&buf, text_len, 1);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].byte_offset, 0);
    assert_eq!(runs[0].fg, 0x00FF0000);
    assert_eq!(runs[0].bg, 0x0000FF00);
}

#[test]
fn parse_single_run_nonzero_offset() {
    let text = b"ABCDEF";
    let text_len = text.len(); // 6
    // Use 24-bit bg (realistic sRGB). Bit 31 = 0 → extend = false.
    let rec = make_run_bytes(3, 0xAABBCCDD, 0x00223344);

    let mut buf = Vec::from(&text[..]);
    buf.extend_from_slice(&rec);

    let runs = parse_overlay_face_runs(&buf, text_len, 1);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].byte_offset, 3);
    assert_eq!(runs[0].fg, 0xAABBCCDD);
    assert_eq!(runs[0].bg, 0x00223344);
    assert_eq!(runs[0].extend, false);
}

// ---------------------------------------------------------------
// parse_overlay_face_runs: multiple runs
// ---------------------------------------------------------------

#[test]
fn parse_multiple_runs() {
    let text = b"mode-line text here";
    let text_len = text.len();

    let r0 = make_run_bytes(0, 0x00FFFFFF, 0x00000000);
    let r1 = make_run_bytes(10, 0x0000FF00, 0x00FF0000);
    let r2 = make_run_bytes(15, 0x000000FF, 0x00FFFF00);

    let mut buf = Vec::from(&text[..]);
    buf.extend_from_slice(&r0);
    buf.extend_from_slice(&r1);
    buf.extend_from_slice(&r2);

    let runs = parse_overlay_face_runs(&buf, text_len, 3);
    assert_eq!(runs.len(), 3);

    assert_eq!(runs[0].byte_offset, 0);
    assert_eq!(runs[0].fg, 0x00FFFFFF);
    assert_eq!(runs[0].bg, 0x00000000);

    assert_eq!(runs[1].byte_offset, 10);
    assert_eq!(runs[1].fg, 0x0000FF00);
    assert_eq!(runs[1].bg, 0x00FF0000);

    assert_eq!(runs[2].byte_offset, 15);
    assert_eq!(runs[2].fg, 0x000000FF);
    assert_eq!(runs[2].bg, 0x00FFFF00);
}

// ---------------------------------------------------------------
// parse_overlay_face_runs: truncated data
// ---------------------------------------------------------------

#[test]
fn parse_truncated_single_run() {
    // Buffer has text but only 5 bytes of run data (needs 14).
    let text = b"ABC";
    let text_len = text.len();
    let mut buf = Vec::from(&text[..]);
    buf.extend_from_slice(&[0u8; 5]); // only half a record

    let runs = parse_overlay_face_runs(&buf, text_len, 1);
    assert!(runs.is_empty(), "truncated record should be skipped");
}

#[test]
fn parse_truncated_second_run() {
    // First record is complete, second is truncated.
    let text = b"ABCD";
    let text_len = text.len();
    let rec0 = make_run_bytes(0, 0x11111111, 0x22222222);

    let mut buf = Vec::from(&text[..]);
    buf.extend_from_slice(&rec0);
    buf.extend_from_slice(&[0xFFu8; 7]); // 7 bytes, need 14

    let runs = parse_overlay_face_runs(&buf, text_len, 2);
    assert_eq!(runs.len(), 1, "only the first complete record should parse");
    assert_eq!(runs[0].fg, 0x11111111);
}

#[test]
fn parse_nruns_exceeds_buffer() {
    // nruns claims 5 records but buffer only has space for 2.
    let text = b"XY";
    let text_len = text.len();
    let r0 = make_run_bytes(0, 1, 2);
    let r1 = make_run_bytes(1, 3, 4);

    let mut buf = Vec::from(&text[..]);
    buf.extend_from_slice(&r0);
    buf.extend_from_slice(&r1);

    let runs = parse_overlay_face_runs(&buf, text_len, 5);
    assert_eq!(runs.len(), 2, "should only parse records that fit");
    assert_eq!(runs[0].fg, 1);
    assert_eq!(runs[1].fg, 3);
}

// ---------------------------------------------------------------
// parse_overlay_face_runs: zero text_len (runs start at offset 0)
// ---------------------------------------------------------------

#[test]
fn parse_zero_text_len() {
    // No text at all; runs start at offset 0 in the buffer.
    // 0xCAFEBABE has bit 31 set → extend = true, bg = lower 24 bits.
    let rec = make_run_bytes(0, 0xDEADBEEF, 0xCAFEBABE);
    let buf = Vec::from(&rec[..]);

    let runs = parse_overlay_face_runs(&buf, 0, 1);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].fg, 0xDEADBEEF);
    assert_eq!(runs[0].bg, 0x00FEBABE); // lower 24 bits of 0xCAFEBABE
    assert_eq!(runs[0].extend, true); // bit 31 was set
}

// ---------------------------------------------------------------
// parse_overlay_face_runs: endianness verification
// ---------------------------------------------------------------

#[test]
fn parse_verifies_native_endian_u16() {
    // The u16 byte_offset is stored as native-endian bytes.
    // Build a buffer where byte_offset = 0x0102 and verify it
    // decodes correctly on the current platform.
    let expected: u16 = 0x0102;
    let rec = make_run_bytes(expected, 0, 0);
    let buf = Vec::from(&rec[..]);

    let runs = parse_overlay_face_runs(&buf, 0, 1);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].byte_offset, expected);
}

#[test]
fn parse_verifies_native_endian_u32() {
    // Similarly for u32 fg/bg.
    // Use 24-bit bg to avoid extend bit masking.
    let fg_expected: u32 = 0x01020304;
    let bg_expected: u32 = 0x00060708;
    let rec = make_run_bytes(0, fg_expected, bg_expected);
    let buf = Vec::from(&rec[..]);

    let runs = parse_overlay_face_runs(&buf, 0, 1);
    assert_eq!(runs[0].fg, fg_expected);
    assert_eq!(runs[0].bg, bg_expected);
    assert_eq!(runs[0].extend, false);
}

// ---------------------------------------------------------------
// parse_overlay_face_runs: exact boundary (off + 10 == buf.len())
// ---------------------------------------------------------------

#[test]
fn parse_exact_fit() {
    // Buffer is exactly text_len + 14 bytes — the run should parse.
    let text = b"T";
    let text_len = text.len(); // 1
    let rec = make_run_bytes(0, 42, 99);
    let mut buf = Vec::from(&text[..]);
    buf.extend_from_slice(&rec);
    assert_eq!(buf.len(), text_len + 14);

    let runs = parse_overlay_face_runs(&buf, text_len, 1);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].fg, 42);
    assert_eq!(runs[0].bg, 99);
}

#[test]
fn parse_one_byte_short() {
    // Buffer is text_len + 13 bytes — one byte short, run should NOT parse.
    let text = b"T";
    let text_len = text.len();
    let mut buf = Vec::from(&text[..]);
    buf.extend_from_slice(&[0u8; 13]);
    assert_eq!(buf.len(), text_len + 13);

    let runs = parse_overlay_face_runs(&buf, text_len, 1);
    assert!(runs.is_empty());
}

// ---------------------------------------------------------------
// apply_overlay_face_run: basic advancement
// ---------------------------------------------------------------

#[test]
fn apply_overlay_single_run_before_offset() {
    // byte_idx < run.byte_offset  =>  no face change, cr unchanged.
    let runs = vec![OverlayFaceRun {
        byte_offset: 5,
        fg: 0x00FF0000,
        bg: 0x00000000,
        extend: false,
        face_id: 0,
    }];
    // byte_idx = 0, which is < 5
    let cr = apply_overlay_face_run(&runs, 0, 0);
    // Since byte_idx (0) < runs[0].byte_offset (5), the condition at
    // line 57 (`byte_idx >= runs[cr].byte_offset`) is false,
    // so the function just returns cr unchanged.
    assert_eq!(cr, 0);
}

#[test]
fn apply_overlay_single_run_at_offset() {
    // byte_idx == run.byte_offset  =>  face applied, cr stays 0.
    let runs = vec![OverlayFaceRun {
        byte_offset: 5,
        fg: 0x00FF0000,
        bg: 0x0000FF00,
        extend: false,
        face_id: 0,
    }];
    let cr = apply_overlay_face_run(&runs, 5, 0);
    assert_eq!(cr, 0);
}

#[test]
fn apply_overlay_single_run_past_offset() {
    let runs = vec![OverlayFaceRun {
        byte_offset: 5,
        fg: 0x00FF0000,
        bg: 0x0000FF00,
        extend: false,
        face_id: 0,
    }];
    let cr = apply_overlay_face_run(&runs, 10, 0);
    assert_eq!(cr, 0);
}

#[test]
fn apply_overlay_multiple_runs_advance() {
    let runs = vec![
        OverlayFaceRun {
            byte_offset: 0,
            fg: 0x00FF0000,
            bg: 0x00000000,
            extend: false,
            face_id: 0,
        },
        OverlayFaceRun {
            byte_offset: 5,
            fg: 0x0000FF00,
            bg: 0x00000000,
            extend: false,
            face_id: 0,
        },
        OverlayFaceRun {
            byte_offset: 10,
            fg: 0x000000FF,
            bg: 0x00000000,
            extend: false,
            face_id: 0,
        },
    ];
    // byte_idx=0 => should stay at run 0
    let cr = apply_overlay_face_run(&runs, 0, 0);
    assert_eq!(cr, 0);

    // byte_idx=5 => should advance to run 1
    let cr = apply_overlay_face_run(&runs, 5, 0);
    assert_eq!(cr, 1);

    // byte_idx=10 => should advance to run 2
    let cr = apply_overlay_face_run(&runs, 10, 0);
    assert_eq!(cr, 2);
}

#[test]
fn apply_overlay_pre_advance_to_next_byte() {
    // Test the pre-advance logic: if byte_idx + 1 >= next run's byte_offset,
    // cr is pre-advanced.
    let runs = vec![
        OverlayFaceRun {
            byte_offset: 0,
            fg: 1,
            bg: 0,
            extend: false,
            face_id: 0,
        },
        OverlayFaceRun {
            byte_offset: 5,
            fg: 2,
            bg: 0,
            extend: false,
            face_id: 0,
        },
    ];
    // byte_idx=4, cr=0: byte_idx(4) >= runs[0].byte_offset(0) => face applied.
    // Pre-advance: byte_idx+1=5 >= runs[1].byte_offset(5) => cr becomes 1.
    let cr = apply_overlay_face_run(&runs, 4, 0);
    assert_eq!(cr, 1, "should pre-advance when byte_idx+1 reaches next run");
}

#[test]
fn apply_overlay_zero_fg_bg_no_face_change() {
    // When both fg and bg are 0, no face change occurs.
    let runs = vec![OverlayFaceRun {
        byte_offset: 0,
        fg: 0,
        bg: 0,
        extend: false,
        face_id: 0,
    }];

    let cr = apply_overlay_face_run(&runs, 0, 0);
    assert_eq!(cr, 0);
}

// ---------------------------------------------------------------
// parse_overlay_face_runs: stress / many runs
// ---------------------------------------------------------------

#[test]
fn parse_many_runs() {
    let text_len = 0;
    let n = 100;

    let mut buf = Vec::new();
    for i in 0..n {
        let rec = make_run_bytes(i as u16, i as u32 * 100, i as u32 * 200);
        buf.extend_from_slice(&rec);
    }

    let runs = parse_overlay_face_runs(&buf, text_len, n);
    assert_eq!(runs.len(), n as usize);

    for i in 0..n as usize {
        assert_eq!(runs[i].byte_offset, i as u16);
        assert_eq!(runs[i].fg, i as u32 * 100);
        assert_eq!(runs[i].bg, i as u32 * 200);
    }
}

// ---------------------------------------------------------------
// parse_overlay_face_runs: large text_len offset
// ---------------------------------------------------------------

#[test]
fn parse_large_text_offset() {
    // Simulate a buffer where 500 bytes are text, followed by 1 run.
    let text_len = 500;
    let mut buf = vec![0x41u8; text_len]; // 'A' * 500
    let rec = make_run_bytes(100, 0xDEAD, 0xBEEF);
    buf.extend_from_slice(&rec);

    let runs = parse_overlay_face_runs(&buf, text_len, 1);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].byte_offset, 100);
    assert_eq!(runs[0].fg, 0xDEAD);
    assert_eq!(runs[0].bg, 0xBEEF);
}

// ---------------------------------------------------------------
// apply_overlay_face_run: starting from non-zero current_run
// ---------------------------------------------------------------

#[test]
fn apply_overlay_start_from_middle_run() {
    let runs = vec![
        OverlayFaceRun {
            byte_offset: 0,
            fg: 1,
            bg: 0,
            extend: false,
            face_id: 0,
        },
        OverlayFaceRun {
            byte_offset: 5,
            fg: 2,
            bg: 0,
            extend: false,
            face_id: 0,
        },
        OverlayFaceRun {
            byte_offset: 10,
            fg: 3,
            bg: 0,
            extend: false,
            face_id: 0,
        },
    ];
    // Start at current_run=1, byte_idx=10 => should advance to run 2
    let cr = apply_overlay_face_run(&runs, 10, 1);
    assert_eq!(cr, 2);
}

#[test]
fn apply_overlay_start_at_last_run() {
    let runs = vec![
        OverlayFaceRun {
            byte_offset: 0,
            fg: 1,
            bg: 0,
            extend: false,
            face_id: 0,
        },
        OverlayFaceRun {
            byte_offset: 5,
            fg: 2,
            bg: 0,
            extend: false,
            face_id: 0,
        },
    ];
    // Already at last run, byte_idx well past it
    let cr = apply_overlay_face_run(&runs, 100, 1);
    assert_eq!(cr, 1);
}

#[test]
fn status_line_row_height_for_face_uses_realized_line_height_and_box() {
    let mut engine = LayoutEngine::new();
    let mut face = ResolvedFace::default();
    face.font_family = "monospace".to_string();
    face.font_size = 14.0;
    face.font_ascent = 9.0;
    face.font_line_height = 12.0;
    face.box_type = 1;
    face.box_line_width = 1;

    assert_eq!(
        engine.status_line_row_height_for_face(&face, 8.0, 12.0, 20.0),
        20.0
    );
}
