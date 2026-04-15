use super::WindowOutputEmitter;
use neovm_core::emacs_core::Context;

#[test]
fn emit_text_span_advances_live_output_before_row_finish() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let frame_id =
        eval.frame_manager_mut()
            .create_frame("output-emitter-span", 320, 120, buf_id);
    let window_id = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;

    let mut emitter = WindowOutputEmitter::new(frame_id, window_id, 0, 0.0, 0.0);
    emitter.begin_update(&mut eval);
    emitter.begin_text_row(&mut eval, 0, 0, 0.0, 0.0);
    emitter.emit_text_span(&mut eval, 1, 0, 0.0, 0.0, 0.0, 24.0, 16.0, 0, 3);

    let display = eval
        .frame_manager()
        .get(frame_id)
        .and_then(|frame| frame.find_window(window_id))
        .and_then(|window| window.display())
        .expect("window display state");

    assert_eq!(
        display.output_cursor,
        Some(neovm_core::window::WindowCursorPos {
            x: 24,
            y: 0,
            row: 0,
            col: 3,
        })
    );
}

#[test]
fn emit_synthetic_text_span_advances_live_output_without_display_points() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let frame_id =
        eval.frame_manager_mut()
            .create_frame("output-emitter-synthetic", 320, 120, buf_id);
    let window_id = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;

    let mut emitter = WindowOutputEmitter::new(frame_id, window_id, 0, 0.0, 0.0);
    emitter.begin_update(&mut eval);
    emitter.begin_text_row(&mut eval, 0, 0, 0.0, 0.0);
    emitter.emit_synthetic_text_span(&mut eval, 0, 0.0, 0.0, 16.0, 0, 2);

    let display = eval
        .frame_manager()
        .get(frame_id)
        .and_then(|frame| frame.find_window(window_id))
        .and_then(|window| window.display())
        .expect("window display state");

    assert_eq!(emitter.display_point_len(), 0);
    assert_eq!(
        display.output_cursor,
        Some(neovm_core::window::WindowCursorPos {
            x: 16,
            y: 0,
            row: 0,
            col: 2,
        })
    );
}
