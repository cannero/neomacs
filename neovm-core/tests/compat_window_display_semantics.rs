use neovm_core::buffer::BufferId;
use neovm_core::emacs_core::value::set_current_heap;
use neovm_core::emacs_core::{Context, Value, format_eval_result, parse_forms};
use neovm_core::gc::LispHeap;
use neovm_core::window::{FrameManager, SplitDirection, Window};

fn run_neovm_gui_eval(body: &str) -> String {
    let mut eval = Context::new();
    let scratch = eval.buffer_manager_mut().create_buffer("*scratch*");
    eval.buffer_manager_mut().set_current(scratch);
    let frame_id = eval
        .frame_manager_mut()
        .create_frame("F1", 800, 600, scratch);
    eval.frame_manager_mut()
        .get_mut(frame_id)
        .expect("frame")
        .set_window_system(Some(Value::symbol("neo")));
    let forms = parse_forms(&format!(r#"(progn {body})"#)).expect("parse");
    eval.eval_forms(&forms)
        .iter()
        .last()
        .map(format_eval_result)
        .expect("result")
}

#[test]
fn compat_gui_window_scroll_bars_round_trip() {
    let actual = run_neovm_gui_eval(
        "(let ((w (selected-window)))
           (list (window-scroll-bars w)
                 (set-window-scroll-bars w 13 'left 9 'bottom t)
                 (window-scroll-bars w)
                 (window-scroll-bar-width w)
                 (window-scroll-bar-height w)))",
    );
    assert_eq!(
        actual,
        "OK ((nil 1 t nil 0 t nil) t (13 2 left 9 1 bottom t) 13 9)"
    );
}

#[test]
fn compat_gui_window_vscroll_round_trip() {
    let actual = run_neovm_gui_eval(
        "(let ((w (selected-window)))
           (list (window-vscroll w)
                 (set-window-vscroll w 3)
                 (window-vscroll w)
                 (set-window-vscroll w 25 t)
                 (window-vscroll w t)
                 (set-window-vscroll w 1.5)
                 (window-vscroll w)
                 (set-window-vscroll w -4)
                 (window-vscroll w)))",
    );
    assert_eq!(actual, "OK (0 3 3 25 25 1.5 1.5 0 0)");
}

#[test]
fn compat_gui_window_body_geometry_excludes_scroll_bar_area() {
    let actual = run_neovm_gui_eval(
        "(let ((w (selected-window)))
           (set-window-scroll-bars w 13 'left)
           (list (window-body-width w t)
                 (window-text-width w t)))",
    );
    assert_eq!(actual, "OK (771 771)");
}

#[test]
fn compat_gui_set_window_buffer_resets_vscroll_except_same_buffer_keep_margins() {
    let actual = run_neovm_gui_eval(
        "(let* ((w (selected-window))
                (b1 (get-buffer-create \" *gui-vscroll-a*\"))
                (b2 (get-buffer-create \" *gui-vscroll-b*\")))
           (unwind-protect
               (progn
                 (set-window-buffer w b1)
                 (set-window-vscroll w 25 t t)
                 (list
                  (window-vscroll w t)
                  (progn
                    (set-window-buffer w b1 t)
                    (window-vscroll w t))
                  (progn
                    (set-window-buffer w b2)
                    (window-vscroll w t))
                  (progn
                    (set-window-vscroll w 33 t t)
                    (set-window-buffer w b2 t)
                    (window-vscroll w t))))
             (kill-buffer b1)
             (kill-buffer b2)))",
    );
    assert_eq!(actual, "OK (25 25 0 33)");
}

#[test]
fn compat_gui_set_window_buffer_applies_display_defaults() {
    let mut eval = Context::new();
    let scratch = eval.buffer_manager_mut().create_buffer("*scratch*");
    eval.buffer_manager_mut().set_current(scratch);
    let frame_id = eval
        .frame_manager_mut()
        .create_frame("F1", 800, 600, scratch);
    eval.frame_manager_mut()
        .get_mut(frame_id)
        .expect("frame")
        .set_window_system(Some(Value::symbol("neo")));
    let buffer_name = " *gui-swb-display*";
    let buffer_id = eval.buffer_manager_mut().create_buffer(buffer_name);
    eval.buffer_manager_mut()
        .set_buffer_local_property(buffer_id, "left-fringe-width", Value::Int(3))
        .expect("left fringe");
    eval.buffer_manager_mut()
        .set_buffer_local_property(buffer_id, "right-fringe-width", Value::Int(5))
        .expect("right fringe");
    eval.buffer_manager_mut()
        .set_buffer_local_property(buffer_id, "fringes-outside-margins", Value::True)
        .expect("outside margins");
    eval.buffer_manager_mut()
        .set_buffer_local_property(buffer_id, "scroll-bar-width", Value::Int(11))
        .expect("scroll bar width");
    eval.buffer_manager_mut()
        .set_buffer_local_property(buffer_id, "vertical-scroll-bar", Value::symbol("left"))
        .expect("vertical scroll bar");
    eval.buffer_manager_mut()
        .set_buffer_local_property(buffer_id, "scroll-bar-height", Value::Int(7))
        .expect("scroll bar height");
    eval.buffer_manager_mut()
        .set_buffer_local_property(buffer_id, "horizontal-scroll-bar", Value::symbol("bottom"))
        .expect("horizontal scroll bar");

    let forms = parse_forms(
        "(let ((w (selected-window)))
           (set-window-buffer w \" *gui-swb-display*\")
           (list (window-fringes w)
                 (window-scroll-bars w)
                 (window-scroll-bar-width w)
                 (window-scroll-bar-height w)))",
    )
    .expect("parse");
    let actual = eval
        .eval_forms(&forms)
        .iter()
        .last()
        .map(format_eval_result)
        .expect("result");
    assert_eq!(actual, "OK ((3 5 t nil) (11 2 left 7 1 bottom nil) 11 7)");
}

#[test]
fn compat_split_window_copies_window_display_state() {
    let mut heap = LispHeap::new();
    set_current_heap(&mut heap);
    let mut frames = FrameManager::new();
    let frame_id = frames.create_frame("F1", 800, 600, BufferId(1));
    let original_window_id = frames.get(frame_id).expect("frame").window_list()[0];
    {
        let frame = frames.get_mut(frame_id).expect("frame");
        let display = frame
            .find_window_mut(original_window_id)
            .and_then(Window::display_mut)
            .expect("leaf display");
        display.display_table = Value::Int(17);
        display.cursor_type = Value::Nil;
        display.left_fringe_width = 3;
        display.right_fringe_width = 5;
        display.fringes_outside_margins = true;
        display.fringes_persistent = true;
        display.scroll_bar_width = 11;
        display.vertical_scroll_bar_type = Value::True;
        display.scroll_bar_height = 7;
        display.horizontal_scroll_bar_type = Value::Nil;
        display.scroll_bars_persistent = true;
    }

    let new_window_id = frames
        .split_window(
            frame_id,
            original_window_id,
            SplitDirection::Horizontal,
            BufferId(2),
        )
        .expect("split");
    let frame = frames.get(frame_id).expect("frame");
    let original_display = frame
        .find_window(original_window_id)
        .and_then(Window::display)
        .expect("original display");
    let new_display = frame
        .find_window(new_window_id)
        .and_then(Window::display)
        .expect("new display");

    assert_eq!(original_display.display_table, Value::Int(17));
    assert_eq!(new_display.display_table, Value::Int(17));
    assert_eq!(original_display.cursor_type, Value::Nil);
    assert_eq!(new_display.cursor_type, Value::Nil);
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
    assert_eq!(original_display.vertical_scroll_bar_type, Value::True);
    assert_eq!(new_display.vertical_scroll_bar_type, Value::True);
    assert_eq!(original_display.scroll_bar_height, 7);
    assert_eq!(new_display.scroll_bar_height, 7);
    assert_eq!(original_display.horizontal_scroll_bar_type, Value::Nil);
    assert_eq!(new_display.horizontal_scroll_bar_type, Value::Nil);
    assert!(original_display.scroll_bars_persistent);
    assert!(new_display.scroll_bars_persistent);
}
