use super::*;

#[test]
fn create_frame_and_window() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let frame = mgr.get(fid).unwrap();

    assert_eq!(frame.window_count(), 1);
    assert!(frame.selected_window().is_some());
    assert!(frame.selected_window().unwrap().is_leaf());
}

#[test]
fn frame_manager_gc_traces_name_icon_name_and_title_values() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let focus_fid = mgr.create_frame("F2", 800, 600, BufferId(2));
    {
        let frame = mgr.get_mut(fid).expect("frame");
        frame.icon_name = Value::string("Frame Icon");
        frame.title = Value::string("Frame Title");
        frame.focus_frame = Value::make_frame(focus_fid.0);
    }

    let frame = mgr.get(fid).expect("frame");
    let name = frame.name_value();
    let icon_name = frame.icon_name_value();
    let title = frame.title_value();
    let focus_frame = frame.focus_frame_value();

    let mut roots = Vec::new();
    mgr.trace_roots(&mut roots);

    assert!(roots.contains(&name));
    assert!(roots.contains(&icon_name));
    assert!(roots.contains(&title));
    assert!(roots.contains(&focus_frame));
}

#[test]
fn split_window_horizontal() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().window_list()[0];

    let new_wid = mgr.split_window(fid, wid, SplitDirection::Horizontal, BufferId(2), None);
    assert!(new_wid.is_some());

    let frame = mgr.get(fid).unwrap();
    assert_eq!(frame.window_count(), 2);
}

#[test]
fn split_window_vertical() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().window_list()[0];

    let new_wid = mgr.split_window(fid, wid, SplitDirection::Vertical, BufferId(2), None);
    assert!(new_wid.is_some());

    let frame = mgr.get(fid).unwrap();
    assert_eq!(frame.window_count(), 2);
}

#[test]
fn split_window_copies_window_display_state() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    {
        let frame = mgr.get_mut(fid).unwrap();
        frame.set_window_system(Some(Value::symbol("neo")));
        let wid = frame.window_list()[0];
        let display = frame
            .find_window_mut(wid)
            .and_then(Window::display_mut)
            .expect("leaf display");
        display.display_table = Value::fixnum(17);
        display.cursor_type = Value::NIL;
        display.left_fringe_width = 3;
        display.right_fringe_width = 5;
        display.fringes_outside_margins = true;
        display.fringes_persistent = true;
        display.scroll_bar_width = 11;
        display.vertical_scroll_bar_type = Value::T;
        display.scroll_bar_height = 7;
        display.horizontal_scroll_bar_type = Value::NIL;
        display.scroll_bars_persistent = true;
    }

    let original_wid = mgr.get(fid).unwrap().window_list()[0];
    let new_wid = mgr
        .split_window(
            fid,
            original_wid,
            SplitDirection::Horizontal,
            BufferId(2),
            None,
        )
        .expect("split");

    let frame = mgr.get(fid).unwrap();
    let original_display = frame
        .find_window(original_wid)
        .and_then(Window::display)
        .expect("original display");
    let new_display = frame
        .find_window(new_wid)
        .and_then(Window::display)
        .expect("new display");

    assert_eq!(original_display.display_table, Value::fixnum(17));
    assert_eq!(new_display.display_table, Value::fixnum(17));
    assert_eq!(original_display.cursor_type, Value::NIL);
    assert_eq!(new_display.cursor_type, Value::NIL);
    assert_eq!(original_display.left_fringe_width, 3);
    assert_eq!(new_display.left_fringe_width, 3);
    assert_eq!(original_display.right_fringe_width, 5);
    assert_eq!(new_display.right_fringe_width, 5);
    assert!(original_display.fringes_outside_margins);
    assert!(new_display.fringes_outside_margins);
    assert!(original_display.fringes_persistent);
    assert!(new_display.fringes_persistent);
    assert_eq!(original_display.scroll_bar_width, 11);
    assert_eq!(new_display.scroll_bar_width, 11);
    assert_eq!(original_display.vertical_scroll_bar_type, Value::T);
    assert_eq!(new_display.vertical_scroll_bar_type, Value::T);
    assert_eq!(original_display.scroll_bar_height, 7);
    assert_eq!(new_display.scroll_bar_height, 7);
    assert_eq!(original_display.horizontal_scroll_bar_type, Value::NIL);
    assert_eq!(new_display.horizontal_scroll_bar_type, Value::NIL);
    assert!(original_display.scroll_bars_persistent);
    assert!(new_display.scroll_bars_persistent);
}

#[test]
fn split_window_resets_new_leaf_vscroll_state() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let original_wid = mgr.get(fid).unwrap().window_list()[0];

    if let Some(Window::Leaf {
        vscroll,
        preserve_vscroll_p,
        ..
    }) = mgr
        .get_mut(fid)
        .and_then(|frame| frame.find_window_mut(original_wid))
    {
        *vscroll = -19;
        *preserve_vscroll_p = true;
    }

    let new_wid = mgr
        .split_window(
            fid,
            original_wid,
            SplitDirection::Horizontal,
            BufferId(2),
            None,
        )
        .expect("split");

    let frame = mgr.get(fid).unwrap();
    let Window::Leaf {
        vscroll: original_vscroll,
        preserve_vscroll_p: original_preserve,
        ..
    } = frame.find_window(original_wid).unwrap()
    else {
        panic!("expected original leaf");
    };
    let Window::Leaf {
        vscroll: new_vscroll,
        preserve_vscroll_p: new_preserve,
        ..
    } = frame.find_window(new_wid).unwrap()
    else {
        panic!("expected new leaf");
    };

    assert_eq!(*original_vscroll, -19);
    assert!(*original_preserve);
    assert_eq!(*new_vscroll, 0);
    assert!(!*new_preserve);
}

#[test]
fn delete_window() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().window_list()[0];

    // Split first.
    let new_wid = mgr
        .split_window(fid, wid, SplitDirection::Horizontal, BufferId(2), None)
        .unwrap();

    // Delete the new window.
    assert!(mgr.delete_window(fid, new_wid));
    assert_eq!(mgr.get(fid).unwrap().window_count(), 1);
}

#[test]
fn cannot_delete_last_window() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().window_list()[0];

    assert!(!mgr.delete_window(fid, wid));
}

#[test]
fn select_window() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().window_list()[0];

    let new_wid = mgr
        .split_window(fid, wid, SplitDirection::Horizontal, BufferId(2), None)
        .unwrap();

    assert!(mgr.get_mut(fid).unwrap().select_window(new_wid));
    assert_eq!(mgr.get(fid).unwrap().selected_window.0, new_wid.0,);
}

#[test]
fn window_at_coordinates() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().window_list()[0];

    mgr.split_window(fid, wid, SplitDirection::Horizontal, BufferId(2), None);

    let frame = mgr.get(fid).unwrap();
    // Left half
    let left = frame.window_at(100.0, 300.0);
    assert!(left.is_some());
    // Right half
    let right = frame.window_at(600.0, 300.0);
    assert!(right.is_some());
    // Should be different windows
    assert_ne!(left, right);
}

#[test]
fn frame_columns_and_lines() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let frame = mgr.get(fid).unwrap();

    assert_eq!(frame.columns(), 100); // 800/8
    assert_eq!(frame.lines(), 37); // 600/16 = 37
}

#[test]
fn delete_frame() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    assert!(mgr.delete_frame(fid));
    assert!(mgr.get(fid).is_none());
}

#[test]
fn multiple_frames() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let f1 = mgr.create_frame("F1", 800, 600, BufferId(1));
    let f2 = mgr.create_frame("F2", 1024, 768, BufferId(2));

    assert_eq!(mgr.frame_list().len(), 2);
    assert!(mgr.select_frame(f2));
    assert_eq!(mgr.selected_frame().unwrap().id, f2);

    mgr.delete_frame(f1);
    assert_eq!(mgr.frame_list().len(), 1);
}

#[test]
fn select_frame_retargets_focus_redirections_from_previous_selection() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let selected = mgr.create_frame("F1", 800, 600, BufferId(1));
    let redirected_to_selected = mgr.create_frame("F2", 800, 600, BufferId(2));
    let untouched = mgr.create_frame("F3", 800, 600, BufferId(3));

    mgr.get_mut(redirected_to_selected).unwrap().focus_frame = Value::make_frame(selected.0);
    mgr.get_mut(untouched).unwrap().focus_frame = Value::NIL;

    assert!(mgr.select_frame(redirected_to_selected));
    assert_eq!(
        mgr.get(redirected_to_selected).unwrap().focus_frame_value(),
        Value::make_frame(redirected_to_selected.0)
    );
    assert_eq!(mgr.get(untouched).unwrap().focus_frame_value(), Value::NIL);
}

#[test]
fn rect_contains() {
    crate::test_utils::init_test_tracing();
    let r = Rect::new(10.0, 20.0, 100.0, 50.0);
    assert!(r.contains(10.0, 20.0));
    assert!(r.contains(50.0, 40.0));
    assert!(!r.contains(9.0, 20.0));
    assert!(!r.contains(110.0, 70.0));
}

#[test]
fn find_window_frame_id() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().window_list()[0];

    assert_eq!(mgr.find_window_frame_id(wid), Some(fid));
    assert_eq!(mgr.find_window_frame_id(WindowId(99999)), None);
}

#[test]
fn is_live_window_id() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().window_list()[0];

    assert!(mgr.is_live_window_id(wid));
    assert!(!mgr.is_live_window_id(WindowId(99999)));
}

#[test]
fn window_parameters() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().window_list()[0];

    let key = Value::symbol("my-param");
    let val = Value::fixnum(42);

    // Initially no parameter
    assert!(mgr.window_parameter(wid, &key).is_none());

    mgr.set_window_parameter(wid, key, val);
    assert_eq!(mgr.window_parameter(wid, &key), Some(Value::fixnum(42)));
}

#[test]
fn split_window_does_not_copy_window_parameters() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().window_list()[0];
    let key = Value::symbol("my-param");

    mgr.set_window_parameter(wid, key, Value::fixnum(42));
    let new_wid = mgr
        .split_window(fid, wid, SplitDirection::Horizontal, BufferId(2), None)
        .expect("split");

    assert_eq!(mgr.window_parameter(wid, &key), Some(Value::fixnum(42)));
    assert_eq!(mgr.window_parameter(new_wid, &key), None);
}

#[test]
fn deleted_window_retains_window_parameters() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().window_list()[0];
    let other = mgr
        .split_window(fid, wid, SplitDirection::Horizontal, BufferId(2), None)
        .expect("split");
    let key = Value::symbol("deleted-param");

    mgr.set_window_parameter(other, key, Value::fixnum(7));
    assert!(mgr.delete_window(fid, other));
    assert_eq!(mgr.window_parameter(other, &key), Some(Value::fixnum(7)));
}

#[test]
fn replace_buffer_in_windows() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().window_list()[0];

    // Window should show buffer 1
    let frame = mgr.get(fid).unwrap();
    assert_eq!(
        frame.find_window(wid).unwrap().buffer_id(),
        Some(BufferId(1))
    );

    // Replace buffer 1 with buffer 2
    mgr.replace_buffer_in_windows(BufferId(1), BufferId(2));

    let frame = mgr.get(fid).unwrap();
    assert_eq!(
        frame.find_window(wid).unwrap().buffer_id(),
        Some(BufferId(2))
    );
}

#[test]
fn deep_split_and_delete() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let w1 = mgr.get(fid).unwrap().window_list()[0];

    // Split w1 horizontally → w2
    let w2 = mgr
        .split_window(fid, w1, SplitDirection::Horizontal, BufferId(2), None)
        .unwrap();

    // Split w2 vertically → w3
    let w3 = mgr
        .split_window(fid, w2, SplitDirection::Vertical, BufferId(3), None)
        .unwrap();

    assert_eq!(mgr.get(fid).unwrap().window_count(), 3);

    // Delete w3
    assert!(mgr.delete_window(fid, w3));
    assert_eq!(mgr.get(fid).unwrap().window_count(), 2);

    // Delete w2
    assert!(mgr.delete_window(fid, w2));
    assert_eq!(mgr.get(fid).unwrap().window_count(), 1);

    // w1 is the last one, can't delete
    assert!(!mgr.delete_window(fid, w1));
}

#[test]
fn note_window_selected_updates_use_time() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let w1 = mgr.get(fid).unwrap().window_list()[0];
    let w2 = mgr
        .split_window(fid, w1, SplitDirection::Horizontal, BufferId(2), None)
        .unwrap();

    let t1 = mgr.note_window_selected(w1);
    let t2 = mgr.note_window_selected(w2);
    // Each selection should get a monotonically increasing use-time
    assert!(t2 > t1);
}

#[test]
fn window_set_buffer_resets_position() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().window_list()[0];

    // Modify point
    let frame = mgr.get_mut(fid).unwrap();
    if let Some(w) = frame.find_window_mut(wid) {
        if let Window::Leaf { point, .. } = w {
            *point = 100;
        }
    }

    // Set buffer resets point to 1
    let frame = mgr.get_mut(fid).unwrap();
    if let Some(w) = frame.find_window_mut(wid) {
        w.set_buffer(BufferId(2));
    }

    let frame = mgr.get(fid).unwrap();
    let w = frame.find_window(wid).unwrap();
    if let Window::Leaf {
        point, buffer_id, ..
    } = w
    {
        assert_eq!(*buffer_id, BufferId(2));
        assert_eq!(*point, 1);
    }
}

#[test]
fn frame_resize_pixelwise_updates_window_tree_and_invalidates_display_state() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let w1 = mgr.get(fid).unwrap().window_list()[0];
    let w2 = mgr
        .split_window(fid, w1, SplitDirection::Horizontal, BufferId(2), None)
        .unwrap();

    let frame = mgr.get_mut(fid).unwrap();
    frame.char_width = 10.0;
    frame.char_height = 20.0;
    frame.replace_display_snapshots(vec![WindowDisplaySnapshot {
        window_id: w1,
        phys_cursor: Some(WindowCursorSnapshot {
            kind: WindowCursorKind::Bar,
            x: 7,
            y: 13,
            width: 9,
            height: 17,
            ascent: 12,
            row: 2,
            col: 5,
        }),
        ..WindowDisplaySnapshot::default()
    }]);

    frame
        .find_window_mut(w1)
        .unwrap()
        .set_window_end_from_positions(200, 200, 50, 50, 3);
    frame
        .find_window_mut(w2)
        .unwrap()
        .set_window_end_from_positions(200, 200, 60, 60, 3);

    frame.resize_pixelwise(400, 260);

    assert_eq!(frame.width, 400);
    assert_eq!(frame.height, 260);
    assert!(frame.display_snapshots.is_empty());
    assert!(
        frame
            .find_window(w1)
            .and_then(|window| window.display())
            .and_then(|display| display.phys_cursor.as_ref())
            .is_none()
    );
    assert_eq!(frame.parameter("width"), Some(Value::fixnum(40)));
    assert_eq!(frame.parameter("height"), Some(Value::fixnum(13)));

    let root_bounds = *frame.root_window.bounds();
    assert_eq!(root_bounds, Rect::new(0.0, 0.0, 400.0, 244.0));

    let mini_bounds = *frame.minibuffer_leaf.as_ref().unwrap().bounds();
    assert_eq!(mini_bounds, Rect::new(0.0, 244.0, 400.0, 16.0));

    assert_eq!(
        frame.find_window(w1).unwrap().bounds(),
        &Rect::new(0.0, 0.0, 200.0, 244.0)
    );
    assert_eq!(
        frame.find_window(w2).unwrap().bounds(),
        &Rect::new(200.0, 0.0, 200.0, 244.0)
    );
    assert_eq!(
        frame.find_window(w1).unwrap().window_end_valid(),
        Some(false)
    );
    assert_eq!(
        frame.find_window(w2).unwrap().window_end_valid(),
        Some(false)
    );
    assert_eq!(
        frame.minibuffer_leaf.as_ref().unwrap().window_end_valid(),
        Some(false)
    );
}

#[test]
fn replace_display_snapshots_syncs_live_window_cursor_state() {
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().selected_window;
    let cursor = WindowCursorSnapshot {
        kind: WindowCursorKind::Bar,
        x: 11,
        y: 29,
        width: 3,
        height: 16,
        ascent: 12,
        row: 1,
        col: 4,
    };
    let output_cursor = WindowCursorPos {
        x: 44,
        y: 29,
        row: 1,
        col: 8,
    };

    let frame = mgr.get_mut(fid).unwrap();
    frame.replace_display_snapshots(vec![WindowDisplaySnapshot {
        window_id: wid,
        phys_cursor: Some(cursor.clone()),
        rows: vec![DisplayRowSnapshot {
            row: 1,
            y: 29,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: output_cursor.x,
            end_col: output_cursor.col,
            start_buffer_pos: Some(1),
            end_buffer_pos: Some(8),
        }],
        ..WindowDisplaySnapshot::default()
    }]);

    let display = frame
        .find_window(wid)
        .and_then(|window| window.display())
        .expect("window display state");
    let cursor_pos = WindowCursorPos::from_snapshot(&cursor);
    assert!(display.phys_cursor_on_p);
    assert_eq!(display.phys_cursor_type, WindowCursorKind::Bar);
    assert!(!display.last_cursor_off_p);
    assert_eq!(display.last_cursor_vpos, cursor.row);
    assert_eq!(display.cursor.as_ref(), Some(&cursor_pos));
    assert_eq!(display.phys_cursor.as_ref(), Some(&cursor));
    assert_eq!(display.output_cursor.as_ref(), Some(&output_cursor));
}

#[test]
fn replace_display_snapshots_replaces_old_output_cursor_progress() {
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().selected_window;
    let frame = mgr.get_mut(fid).unwrap();

    frame.replace_display_snapshots(vec![WindowDisplaySnapshot {
        window_id: wid,
        rows: vec![DisplayRowSnapshot {
            row: 1,
            y: 29,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: 44,
            end_col: 8,
            start_buffer_pos: Some(1),
            end_buffer_pos: Some(8),
        }],
        ..WindowDisplaySnapshot::default()
    }]);

    frame.replace_display_snapshots(vec![WindowDisplaySnapshot {
        window_id: wid,
        rows: vec![DisplayRowSnapshot {
            row: 3,
            y: 61,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: 88,
            end_col: 12,
            start_buffer_pos: Some(20),
            end_buffer_pos: Some(32),
        }],
        ..WindowDisplaySnapshot::default()
    }]);

    let display = frame
        .find_window(wid)
        .and_then(|window| window.display())
        .expect("window display state");
    assert_eq!(
        display.output_cursor,
        Some(WindowCursorPos {
            x: 88,
            y: 61,
            row: 3,
            col: 12,
        })
    );
}

#[test]
fn set_display_snapshots_preserves_live_window_cursor_state() {
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().selected_window;
    let cursor = WindowCursorSnapshot {
        kind: WindowCursorKind::Bar,
        x: 11,
        y: 29,
        width: 3,
        height: 16,
        ascent: 12,
        row: 1,
        col: 4,
    };
    let cursor_pos = WindowCursorPos::from_snapshot(&cursor);
    let output_cursor = WindowCursorPos {
        x: 44,
        y: 29,
        row: 1,
        col: 8,
    };
    let snapshot = WindowDisplaySnapshot {
        window_id: wid,
        phys_cursor: Some(cursor.clone()),
        rows: vec![DisplayRowSnapshot {
            row: 1,
            y: 29,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: output_cursor.x,
            end_col: output_cursor.col,
            start_buffer_pos: Some(1),
            end_buffer_pos: Some(8),
        }],
        ..WindowDisplaySnapshot::default()
    };

    let frame = mgr.get_mut(fid).unwrap();
    frame.begin_display_output_pass();
    frame.replay_window_output_snapshot(&snapshot);
    frame.set_display_snapshots(vec![WindowDisplaySnapshot {
        window_id: wid,
        phys_cursor: None,
        ..WindowDisplaySnapshot::default()
    }]);

    let display = frame
        .find_window(wid)
        .and_then(|window| window.display())
        .expect("window display state");
    assert_eq!(display.cursor.as_ref(), Some(&cursor_pos));
    assert_eq!(display.output_cursor.as_ref(), Some(&output_cursor));
    assert_eq!(display.phys_cursor.as_ref(), Some(&cursor));
    assert_eq!(
        frame
            .window_display_snapshot(wid)
            .and_then(|snapshot| snapshot.phys_cursor.as_ref()),
        None
    );
}

#[test]
fn replace_display_snapshots_preserves_logical_cursor_without_physical_cursor() {
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().selected_window;
    let logical_cursor = WindowCursorPos {
        x: 24,
        y: 16,
        row: 1,
        col: 3,
    };

    let frame = mgr.get_mut(fid).unwrap();
    frame.replace_display_snapshots(vec![WindowDisplaySnapshot {
        window_id: wid,
        logical_cursor: Some(logical_cursor),
        rows: vec![DisplayRowSnapshot {
            row: 1,
            y: 16,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: 64,
            end_col: 8,
            start_buffer_pos: Some(10),
            end_buffer_pos: Some(18),
        }],
        ..WindowDisplaySnapshot::default()
    }]);

    let display = frame
        .find_window(wid)
        .and_then(|window| window.display())
        .expect("window display state");
    assert_eq!(display.cursor, Some(logical_cursor));
    assert_eq!(
        display.output_cursor,
        Some(WindowCursorPos {
            x: 64,
            y: 16,
            row: 1,
            col: 8,
        })
    );
    assert_eq!(display.phys_cursor, None);
    assert_eq!(display.phys_cursor_type, WindowCursorKind::NoCursor);
    assert!(!display.phys_cursor_on_p);
}

#[test]
fn replace_display_snapshots_commits_last_cursor_visibility_state() {
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().selected_window;
    let cursor = WindowCursorSnapshot {
        kind: WindowCursorKind::FilledBox,
        x: 4,
        y: 8,
        width: 8,
        height: 16,
        ascent: 12,
        row: 0,
        col: 0,
    };

    let frame = mgr.get_mut(fid).unwrap();
    let display = frame
        .find_window_mut(wid)
        .and_then(|window| window.display_mut())
        .expect("window display state");
    display.cursor_off_p = true;

    frame.replace_display_snapshots(vec![WindowDisplaySnapshot {
        window_id: wid,
        phys_cursor: Some(cursor),
        ..WindowDisplaySnapshot::default()
    }]);

    let display = frame
        .find_window(wid)
        .and_then(|window| window.display())
        .expect("window display state");
    assert!(display.cursor_off_p);
    assert!(display.last_cursor_off_p);
}

#[test]
fn clear_physical_cursor_state_preserves_committed_cursor_history() {
    let cursor = WindowCursorSnapshot {
        kind: WindowCursorKind::FilledBox,
        x: 9,
        y: 21,
        width: 8,
        height: 16,
        ascent: 12,
        row: 2,
        col: 5,
    };
    let snapshot = WindowDisplaySnapshot {
        window_id: WindowId(1),
        phys_cursor: Some(cursor.clone()),
        rows: vec![DisplayRowSnapshot {
            row: 2,
            y: 21,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: 9,
            end_col: 5,
            start_buffer_pos: Some(11),
            end_buffer_pos: Some(11),
        }],
        ..WindowDisplaySnapshot::default()
    };
    let mut display = WindowDisplayState::default();
    display.begin_output_pass();
    display.install_logical_cursor(Some(WindowCursorPos::from_snapshot(&cursor)));
    {
        let mut update = WindowOutputUpdate::new(&mut display);
        update.replay_output_rows(&snapshot.rows);
    }
    display.apply_physical_cursor_snapshot(Some(cursor.clone()));
    display.commit_completed_redisplay();

    display.clear_physical_cursor_state();

    let cursor_pos = WindowCursorPos::from_snapshot(&cursor);
    assert_eq!(display.cursor, Some(cursor_pos));
    assert_eq!(display.output_cursor, Some(cursor_pos));
    assert_eq!(display.phys_cursor, None);
    assert_eq!(display.phys_cursor_type, WindowCursorKind::NoCursor);
    assert!(!display.phys_cursor_on_p);
    assert!(!display.last_cursor_off_p);
    assert_eq!(display.last_cursor_vpos, cursor.row);
}

#[test]
fn begin_output_pass_preserves_committed_output_cursor_until_next_commit() {
    let cursor = WindowCursorSnapshot {
        kind: WindowCursorKind::FilledBox,
        x: 9,
        y: 21,
        width: 8,
        height: 16,
        ascent: 12,
        row: 2,
        col: 5,
    };
    let cursor_pos = WindowCursorPos::from_snapshot(&cursor);
    let mut display = WindowDisplayState::default();

    display.install_logical_cursor(Some(cursor_pos));
    display.output_cursor_to(cursor_pos);
    display.apply_physical_cursor_snapshot(Some(cursor.clone()));

    display.begin_output_pass();

    assert_eq!(display.cursor, None);
    assert_eq!(display.output_cursor, Some(cursor_pos));
    assert_eq!(display.phys_cursor, None);
    assert_eq!(display.phys_cursor_type, WindowCursorKind::NoCursor);
    assert!(!display.phys_cursor_on_p);
}

#[test]
fn begin_window_output_update_clears_output_cursor_for_active_window() {
    let cursor = WindowCursorSnapshot {
        kind: WindowCursorKind::FilledBox,
        x: 9,
        y: 21,
        width: 8,
        height: 16,
        ascent: 12,
        row: 2,
        col: 5,
    };
    let cursor_pos = WindowCursorPos::from_snapshot(&cursor);
    let mut display = WindowDisplayState::default();

    display.install_logical_cursor(Some(cursor_pos));
    display.output_cursor_to(cursor_pos);
    display.apply_physical_cursor_snapshot(Some(cursor));

    display.begin_window_output_update();

    assert_eq!(display.cursor, None);
    assert_eq!(display.output_cursor, None);
    assert_eq!(display.phys_cursor, None);
    assert_eq!(display.phys_cursor_type, WindowCursorKind::NoCursor);
    assert!(!display.phys_cursor_on_p);
}

#[test]
fn output_cursor_tracks_explicit_output_lifecycle() {
    let logical_cursor = WindowCursorPos {
        x: 12,
        y: 24,
        row: 1,
        col: 3,
    };
    let mut display = WindowDisplayState::default();

    display.install_logical_cursor(Some(logical_cursor));
    assert_eq!(display.cursor, Some(logical_cursor));
    assert_eq!(display.output_cursor, None);

    display.output_cursor_to(logical_cursor);
    assert_eq!(display.output_cursor, Some(logical_cursor));

    display.output_cursor_to(WindowCursorPos {
        x: 36,
        y: 24,
        row: 1,
        col: 6,
    });
    assert_eq!(
        display.output_cursor,
        Some(WindowCursorPos {
            x: 36,
            y: 24,
            row: 1,
            col: 6,
        })
    );
    assert_eq!(display.cursor, Some(logical_cursor));
}

#[test]
fn completed_redisplay_preserves_point_row_history_over_output_progress() {
    let mut display = WindowDisplayState::default();
    display.install_logical_cursor(Some(WindowCursorPos {
        x: 12,
        y: 24,
        row: 1,
        col: 3,
    }));
    display.output_cursor_to(WindowCursorPos {
        x: 80,
        y: 72,
        row: 4,
        col: 9,
    });

    display.commit_completed_redisplay();

    assert_eq!(display.last_cursor_vpos, 2);
}

#[test]
fn output_pass_commits_output_cursor_from_row_geometry() {
    let cursor = WindowCursorSnapshot {
        kind: WindowCursorKind::Bar,
        x: 44,
        y: 32,
        width: 3,
        height: 16,
        ascent: 12,
        row: 2,
        col: 7,
    };
    let snapshot = WindowDisplaySnapshot {
        window_id: WindowId(1),
        phys_cursor: Some(cursor.clone()),
        rows: vec![DisplayRowSnapshot {
            row: 2,
            y: 32,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: 80,
            end_col: 12,
            start_buffer_pos: Some(20),
            end_buffer_pos: Some(32),
        }],
        ..WindowDisplaySnapshot::default()
    };
    let mut display = WindowDisplayState::default();

    display.begin_output_pass();
    display.install_logical_cursor(Some(WindowCursorPos::from_snapshot(&cursor)));
    {
        let mut update = WindowOutputUpdate::new(&mut display);
        update.replay_output_rows(&snapshot.rows);
    }
    display.apply_physical_cursor_snapshot(Some(cursor.clone()));
    display.commit_completed_redisplay();

    assert_eq!(
        display.output_cursor,
        Some(WindowCursorPos {
            x: 80,
            y: 32,
            row: 2,
            col: 12,
        })
    );
    assert_eq!(display.last_cursor_vpos, 2);
    assert_eq!(display.phys_cursor, Some(cursor));
}

#[test]
fn explicit_window_output_finalization_prefers_live_output_progress() {
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().selected_window;
    let cursor = WindowCursorSnapshot {
        kind: WindowCursorKind::Bar,
        x: 44,
        y: 32,
        width: 3,
        height: 16,
        ascent: 12,
        row: 2,
        col: 7,
    };
    let frame = mgr.get_mut(fid).expect("frame");
    frame.begin_display_output_pass();
    {
        let mut update = frame.window_output_update(wid).expect("window update");
        update.begin_update();
        update.output_cursor_to_coords(2, 0, 32, 0);
        update.output_cursor_to_coords(2, 12, 32, 80);
        update.finalize_live_update(
            Some(WindowCursorPos::from_snapshot(&cursor)),
            Some(cursor.clone()),
        );
    }

    let display = frame
        .find_window(wid)
        .and_then(|window| window.display())
        .expect("window display state");
    assert_eq!(
        display.output_cursor,
        Some(WindowCursorPos {
            x: 80,
            y: 32,
            row: 2,
            col: 12,
        })
    );
    assert_eq!(display.phys_cursor, Some(cursor));
}

#[test]
fn frame_output_progress_api_tracks_intra_row_progress() {
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().selected_window;
    let frame = mgr.get_mut(fid).expect("frame");

    frame.begin_display_output_pass();
    {
        let mut update = frame.window_output_update(wid).expect("window update");
        update.begin_update();
        update.output_cursor_to_coords(2, 3, 32, 24);
        update.output_cursor_to_coords(2, 7, 32, 56);
    }

    let display = frame
        .find_window(wid)
        .and_then(|window| window.display())
        .expect("window display state");
    assert_eq!(
        display.output_cursor,
        Some(WindowCursorPos {
            x: 56,
            y: 32,
            row: 2,
            col: 7,
        })
    );
}

#[test]
fn explicit_window_output_finalization_preserves_live_logical_and_physical_cursor_state() {
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().selected_window;
    let live_cursor = WindowCursorPos {
        x: 18,
        y: 16,
        row: 1,
        col: 2,
    };
    let live_phys = WindowCursorSnapshot {
        kind: WindowCursorKind::Bar,
        x: 18,
        y: 16,
        width: 3,
        height: 16,
        ascent: 12,
        row: 1,
        col: 2,
    };
    let snapshot_phys = WindowCursorSnapshot {
        kind: WindowCursorKind::FilledBox,
        x: 80,
        y: 64,
        width: 8,
        height: 16,
        ascent: 12,
        row: 4,
        col: 10,
    };
    let snapshot = WindowDisplaySnapshot {
        window_id: wid,
        logical_cursor: Some(WindowCursorPos::from_snapshot(&snapshot_phys)),
        phys_cursor: Some(snapshot_phys),
        rows: vec![DisplayRowSnapshot {
            row: 4,
            y: 64,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: 144,
            end_col: 18,
            start_buffer_pos: Some(20),
            end_buffer_pos: Some(38),
        }],
        ..WindowDisplaySnapshot::default()
    };
    let frame = mgr.get_mut(fid).expect("frame");

    frame.begin_display_output_pass();
    {
        let mut update = frame.window_output_update(wid).expect("window update");
        update.begin_update();
        update.finalize_with_output_fallback(Some(live_cursor), Some(live_phys.clone()), &snapshot);
    }

    let display = frame
        .find_window(wid)
        .and_then(|window| window.display())
        .expect("window display state");
    assert_eq!(display.cursor, Some(live_cursor));
    assert_eq!(display.phys_cursor, Some(live_phys));
    assert_eq!(
        display.output_cursor,
        Some(WindowCursorPos {
            x: 144,
            y: 64,
            row: 4,
            col: 18,
        })
    );
}

#[test]
fn finish_window_output_update_preserves_live_cursor_state_with_snapshot_output_fallback() {
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().selected_window;
    let live_cursor = WindowCursorPos {
        x: 18,
        y: 16,
        row: 1,
        col: 2,
    };
    let live_phys = WindowCursorSnapshot {
        kind: WindowCursorKind::Bar,
        x: 18,
        y: 16,
        width: 3,
        height: 16,
        ascent: 12,
        row: 1,
        col: 2,
    };
    let snapshot = WindowDisplaySnapshot {
        window_id: wid,
        rows: vec![DisplayRowSnapshot {
            row: 4,
            y: 64,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: 144,
            end_col: 18,
            start_buffer_pos: Some(20),
            end_buffer_pos: Some(38),
        }],
        ..WindowDisplaySnapshot::default()
    };
    let frame = mgr.get_mut(fid).expect("frame");

    frame.begin_display_output_pass();
    {
        let mut update = frame.window_output_update(wid).expect("window update");
        update.begin_update();
        update.finalize_with_output_fallback(Some(live_cursor), Some(live_phys.clone()), &snapshot);
    }

    let display = frame
        .find_window(wid)
        .and_then(|window| window.display())
        .expect("window display state");
    assert_eq!(display.cursor, Some(live_cursor));
    assert_eq!(display.phys_cursor, Some(live_phys));
    assert_eq!(
        display.output_cursor,
        Some(WindowCursorPos {
            x: 144,
            y: 64,
            row: 4,
            col: 18,
        })
    );
    assert_eq!(display.last_cursor_vpos, 1);
}

#[test]
fn output_pass_keeps_cursor_target_and_output_progress_separate() {
    let cursor = WindowCursorSnapshot {
        kind: WindowCursorKind::Bar,
        x: 18,
        y: 16,
        width: 3,
        height: 16,
        ascent: 12,
        row: 1,
        col: 2,
    };
    let snapshot = WindowDisplaySnapshot {
        window_id: WindowId(1),
        phys_cursor: Some(cursor.clone()),
        rows: vec![
            DisplayRowSnapshot {
                row: 0,
                y: 0,
                height: 16,
                start_x: 0,
                start_col: 0,
                end_x: 64,
                end_col: 8,
                start_buffer_pos: Some(1),
                end_buffer_pos: Some(8),
            },
            DisplayRowSnapshot {
                row: 1,
                y: 16,
                height: 16,
                start_x: 0,
                start_col: 0,
                end_x: 72,
                end_col: 9,
                start_buffer_pos: Some(9),
                end_buffer_pos: Some(17),
            },
            DisplayRowSnapshot {
                row: 2,
                y: 32,
                height: 16,
                start_x: 0,
                start_col: 0,
                end_x: 80,
                end_col: 10,
                start_buffer_pos: Some(18),
                end_buffer_pos: Some(27),
            },
        ],
        ..WindowDisplaySnapshot::default()
    };
    let mut display = WindowDisplayState::default();

    display.begin_output_pass();
    display.install_logical_cursor(Some(WindowCursorPos::from_snapshot(&cursor)));
    {
        let mut update = WindowOutputUpdate::new(&mut display);
        update.replay_output_rows(&snapshot.rows);
    }
    display.apply_physical_cursor_snapshot(Some(cursor.clone()));
    display.commit_completed_redisplay();

    assert_eq!(
        display.cursor,
        Some(WindowCursorPos {
            x: 18,
            y: 16,
            row: 1,
            col: 2,
        })
    );
    assert_eq!(
        display.output_cursor,
        Some(WindowCursorPos {
            x: 80,
            y: 32,
            row: 2,
            col: 10,
        })
    );
    assert_eq!(display.last_cursor_vpos, 1);
    assert_eq!(display.phys_cursor, Some(cursor));
}

#[test]
fn replace_display_snapshots_preserves_output_cursor_for_omitted_windows() {
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let wid = mgr.get(fid).unwrap().selected_window;
    let cursor = WindowCursorSnapshot {
        kind: WindowCursorKind::FilledBox,
        x: 18,
        y: 36,
        width: 8,
        height: 16,
        ascent: 12,
        row: 2,
        col: 6,
    };
    let cursor_pos = WindowCursorPos::from_snapshot(&cursor);

    let frame = mgr.get_mut(fid).unwrap();
    let display = frame
        .find_window_mut(wid)
        .and_then(|window| window.display_mut())
        .expect("window display state");
    display.install_logical_cursor(Some(cursor_pos));
    display.output_cursor_to(cursor_pos);
    display.apply_physical_cursor_snapshot(Some(cursor));
    display.commit_completed_redisplay();

    frame.replace_display_snapshots(Vec::new());

    let display = frame
        .find_window(wid)
        .and_then(|window| window.display())
        .expect("window display state");
    assert_eq!(display.cursor, None);
    assert_eq!(display.output_cursor, Some(cursor_pos));
    assert_eq!(display.phys_cursor, None);
    assert_eq!(display.phys_cursor_type, WindowCursorKind::NoCursor);
    assert!(!display.phys_cursor_on_p);
    assert_eq!(display.last_cursor_vpos, cursor_pos.row);
}

#[test]
fn frame_resize_pixelwise_reserves_tab_bar_height_above_root_window_tree() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
    let frame = mgr.get_mut(fid).unwrap();
    frame.char_width = 10.0;
    frame.char_height = 20.0;
    frame.set_parameter(Value::symbol("tab-bar-lines"), Value::fixnum(1));

    frame.sync_tab_bar_height_from_parameters();
    frame.resize_pixelwise(400, 260);

    assert_eq!(frame.tab_bar_height, 20);
    assert_eq!(
        *frame.root_window.bounds(),
        Rect::new(0.0, 20.0, 400.0, 224.0)
    );
    assert_eq!(
        *frame.minibuffer_leaf.as_ref().unwrap().bounds(),
        Rect::new(0.0, 244.0, 400.0, 16.0)
    );
    assert_eq!(frame.parameter("height"), Some(Value::fixnum(12)));
}

#[test]
fn grow_and_shrink_mini_window_adjusts_bounds() {
    crate::test_utils::init_test_tracing();
    let mut mgr = FrameManager::new();
    let fid = mgr.create_frame("F1", 80, 24, BufferId(1));
    // Treat the frame as a TTY-style frame where 1 px == 1 character row.
    // char_height=1.0 means `grow_mini_window` grows by 1 row per delta,
    // and max-mini-window-height (25% of 24 rows = 6 rows) is comfortably
    // above the 1-row minimum.
    // Re-initialize the minibuffer to exactly 1 row so that it starts at
    // the minimum height and has room to grow.
    {
        let frame = mgr.get_mut(fid).unwrap();
        frame.char_height = 1.0;
        frame.char_width = 1.0;
        if let Some(mini) = frame.minibuffer_leaf.as_mut() {
            let mut b = *mini.bounds();
            b.height = 1.0;
            mini.set_bounds(b);
        }
        frame.sync_window_area_bounds();
    }
    let frame = mgr.get(fid).unwrap();
    let initial_mini_h = frame.minibuffer_leaf.as_ref().unwrap().bounds().height;

    mgr.get_mut(fid).unwrap().grow_mini_window(3);
    let grown_h = mgr
        .get(fid)
        .unwrap()
        .minibuffer_leaf
        .as_ref()
        .unwrap()
        .bounds()
        .height;
    assert!(
        grown_h > initial_mini_h,
        "minibuffer should grow: initial={initial_mini_h} grown={grown_h}"
    );

    mgr.get_mut(fid).unwrap().shrink_mini_window();
    let shrunk_h = mgr
        .get(fid)
        .unwrap()
        .minibuffer_leaf
        .as_ref()
        .unwrap()
        .bounds()
        .height;
    assert!(
        shrunk_h < grown_h,
        "minibuffer should shrink: grown={grown_h} shrunk={shrunk_h}"
    );
}
