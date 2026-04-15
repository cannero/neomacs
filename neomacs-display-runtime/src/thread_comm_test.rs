use super::*;
use crate::core::frame_glyphs::FrameGlyphBuffer;

// ===================================================================
// Constants
// ===================================================================

#[test]
fn channel_capacity_constants() {
    assert_eq!(INPUT_CHANNEL_CAPACITY, 256);
    assert_eq!(COMMAND_CHANNEL_CAPACITY, 64);
}

// ===================================================================
// WakeupPipe
// ===================================================================

#[test]
fn wakeup_pipe_new_succeeds() {
    let pipe = WakeupPipe::new();
    assert!(pipe.is_ok());
}

#[cfg(unix)]
#[test]
fn wakeup_pipe_reader_fd_is_valid() {
    let pipe = WakeupPipe::new().unwrap();
    let fd = pipe.read_fd();
    // A valid fd is non-negative
    assert!(fd >= 0, "read_fd should be a non-negative fd, got {}", fd);
}

#[cfg(unix)]
#[test]
fn wakeup_pipe_wake_and_clear() {
    let pipe = WakeupPipe::new().unwrap();

    // Wake writes one byte to the pipe
    pipe.wake();

    // After wake, reading the pipe should yield data.
    // We can verify by doing a non-blocking read before clear to confirm
    // there is something, then call clear and confirm the pipe is drained.
    let mut buf = [0u8; 1];
    let n = unsafe {
        let flags = libc::fcntl(pipe.read_fd(), libc::F_GETFL);
        libc::fcntl(pipe.read_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        let n = libc::read(pipe.read_fd(), buf.as_mut_ptr() as *mut _, 1);
        libc::fcntl(pipe.read_fd(), libc::F_SETFL, flags);
        n
    };
    assert_eq!(
        n, 1,
        "wake() should have written 1 byte, read returned {}",
        n
    );
    assert_eq!(buf[0], 1, "wake() writes the byte 0x01");
}

#[cfg(unix)]
#[test]
fn wakeup_pipe_clear_drains_pipe() {
    let pipe = WakeupPipe::new().unwrap();

    // Wake multiple times
    pipe.wake();
    pipe.wake();
    pipe.wake();

    // Clear should drain all bytes
    pipe.clear();

    // After clear, a non-blocking read should return EAGAIN (nothing to read)
    let mut buf = [0u8; 1];
    let n = unsafe {
        let flags = libc::fcntl(pipe.read_fd(), libc::F_GETFL);
        libc::fcntl(pipe.read_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        let n = libc::read(pipe.read_fd(), buf.as_mut_ptr() as *mut _, 1);
        libc::fcntl(pipe.read_fd(), libc::F_SETFL, flags);
        n
    };
    assert!(
        n <= 0,
        "pipe should be empty after clear(), but read returned {}",
        n
    );
}

#[cfg(unix)]
#[test]
fn wakeup_pipe_multiple_wakes() {
    let pipe = WakeupPipe::new().unwrap();

    // Write 5 bytes
    for _ in 0..5 {
        pipe.wake();
    }

    // Read them all out to verify we got 5
    let mut total_read = 0isize;
    let mut buf = [0u8; 64];
    unsafe {
        let flags = libc::fcntl(pipe.read_fd(), libc::F_GETFL);
        libc::fcntl(pipe.read_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        loop {
            let n = libc::read(pipe.read_fd(), buf.as_mut_ptr() as *mut _, buf.len());
            if n <= 0 {
                break;
            }
            total_read += n;
        }
        libc::fcntl(pipe.read_fd(), libc::F_SETFL, flags);
    }
    assert_eq!(
        total_read, 5,
        "expected 5 bytes from 5 wake() calls, got {}",
        total_read
    );
}

#[test]
fn wakeup_pipe_clear_on_empty_pipe_is_noop() {
    let pipe = WakeupPipe::new().unwrap();
    // Clearing an empty pipe should not block or panic
    pipe.clear();
}

#[cfg(unix)]
#[test]
fn wakeup_pipe_wake_clear_wake_clear_cycle() {
    let pipe = WakeupPipe::new().unwrap();

    pipe.wake();
    pipe.clear();

    // Pipe should be empty now
    pipe.wake();
    pipe.wake();
    pipe.clear();

    // Verify drained
    let mut buf = [0u8; 1];
    let n = unsafe {
        let flags = libc::fcntl(pipe.read_fd(), libc::F_GETFL);
        libc::fcntl(pipe.read_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        let n = libc::read(pipe.read_fd(), buf.as_mut_ptr() as *mut _, 1);
        libc::fcntl(pipe.read_fd(), libc::F_SETFL, flags);
        n
    };
    assert!(n <= 0, "pipe should be empty after second clear()");
}

// ===================================================================
// ThreadComms
// ===================================================================

#[test]
fn thread_comms_new_succeeds() {
    let comms = ThreadComms::new();
    assert!(comms.is_ok());
}

#[test]
fn thread_comms_input_channel_roundtrip() {
    let comms = ThreadComms::new().unwrap();

    let event = InputEvent::Key {
        keysym: 65, // 'A'
        modifiers: 0,
        pressed: true,
        emacs_frame_id: 0,
    };

    comms.input_tx.send(event.clone()).unwrap();

    let received = comms.input_rx.try_recv().unwrap();
    match received {
        InputEvent::Key {
            keysym,
            modifiers,
            pressed,
            emacs_frame_id,
        } => {
            assert_eq!(keysym, 65);
            assert_eq!(modifiers, 0);
            assert!(pressed);
            assert_eq!(emacs_frame_id, 0);
        }
        other => panic!("Expected Key event, got {:?}", other),
    }
}

#[test]
fn thread_comms_cmd_channel_roundtrip() {
    let comms = ThreadComms::new().unwrap();

    comms.cmd_tx.send(RenderCommand::Shutdown).unwrap();

    let received = comms.cmd_rx.try_recv().unwrap();
    match received {
        RenderCommand::Shutdown => {} // ok
        other => panic!("Expected Shutdown, got {:?}", other),
    }
}

#[test]
fn thread_comms_frame_channel_roundtrip() {
    let comms = ThreadComms::new().unwrap();

    let buf = FrameGlyphBuffer::new();
    let state = FrameDisplayState::from_frame_glyph_buffer(&buf);
    comms.frame_tx.send(state).unwrap();

    let received = comms.frame_rx.try_recv().unwrap();
    assert_eq!(received.frame_pixel_width, 0.0);
    assert_eq!(received.frame_pixel_height, 0.0);
}

#[test]
fn thread_comms_frame_channel_is_unbounded() {
    let comms = ThreadComms::new().unwrap();

    // Send many frames without blocking -- unbounded channel
    for i in 0..100 {
        let buf = FrameGlyphBuffer::with_size(i as f32, i as f32);
        let state = FrameDisplayState::from_frame_glyph_buffer(&buf);
        comms.frame_tx.send(state).unwrap();
    }

    // Drain and verify
    for i in 0..100 {
        let received = comms.frame_rx.try_recv().unwrap();
        assert_eq!(received.frame_pixel_width, i as f32);
    }
}

#[test]
fn thread_comms_cmd_channel_bounded_capacity() {
    let comms = ThreadComms::new().unwrap();

    // Fill up the command channel to capacity
    for _ in 0..COMMAND_CHANNEL_CAPACITY {
        comms.cmd_tx.try_send(RenderCommand::Shutdown).unwrap();
    }

    // Next try_send should fail (channel full)
    let result = comms.cmd_tx.try_send(RenderCommand::Shutdown);
    assert!(
        result.is_err(),
        "cmd channel should be full after {} sends",
        COMMAND_CHANNEL_CAPACITY
    );
}

#[test]
fn thread_comms_input_channel_bounded_capacity() {
    let comms = ThreadComms::new().unwrap();

    // Fill up the input channel to capacity
    for _ in 0..INPUT_CHANNEL_CAPACITY {
        let event = InputEvent::Key {
            keysym: 0,
            modifiers: 0,
            pressed: false,
            emacs_frame_id: 0,
        };
        comms.input_tx.try_send(event).unwrap();
    }

    // Next try_send should fail (channel full)
    let result = comms.input_tx.try_send(InputEvent::Key {
        keysym: 0,
        modifiers: 0,
        pressed: false,
        emacs_frame_id: 0,
    });
    assert!(
        result.is_err(),
        "input channel should be full after {} sends",
        INPUT_CHANNEL_CAPACITY
    );
}

// ===================================================================
// ThreadComms::split()
// ===================================================================

#[test]
fn thread_comms_split_channels_work() {
    let comms = ThreadComms::new().unwrap();
    let (emacs, render) = comms.split();

    // Emacs sends command, render receives
    emacs.cmd_tx.send(RenderCommand::VisualBell).unwrap();
    let cmd = render.cmd_rx.try_recv().unwrap();
    match cmd {
        RenderCommand::VisualBell => {}
        other => panic!("Expected VisualBell, got {:?}", other),
    }

    // Render sends input, Emacs receives
    render
        .input_tx
        .send(InputEvent::WindowClose { emacs_frame_id: 42 })
        .unwrap();
    let evt = emacs.input_rx.try_recv().unwrap();
    match evt {
        InputEvent::WindowClose { emacs_frame_id } => assert_eq!(emacs_frame_id, 42),
        other => panic!("Expected WindowClose, got {:?}", other),
    }

    // Emacs sends frame, render receives
    let buf = FrameGlyphBuffer::with_size(800.0, 600.0);
    let state = FrameDisplayState::from_frame_glyph_buffer(&buf);
    emacs.frame_tx.send(state).unwrap();
    let frame = render.frame_rx.try_recv().unwrap();
    assert_eq!(frame.frame_pixel_width, 800.0);
    assert_eq!(frame.frame_pixel_height, 600.0);
}

#[test]
fn thread_comms_split_wakeup_fd_matches() {
    let comms = ThreadComms::new().unwrap();
    let wakeup_fd = comms.wakeup.read_fd();
    let (emacs, _render) = comms.split();
    assert_eq!(emacs.wakeup_read_fd, wakeup_fd);
}

// ===================================================================
// RenderComms::send_input()
// ===================================================================

#[test]
fn render_comms_send_input_delivers_event_and_wakes() {
    let comms = ThreadComms::new().unwrap();
    let (emacs, render) = comms.split();

    render.send_input(InputEvent::MouseMove {
        x: 100.0,
        y: 200.0,
        modifiers: 0,
        target_frame_id: 0,
    });

    // Event should be receivable
    let evt = emacs.input_rx.try_recv().unwrap();
    match evt {
        InputEvent::MouseMove { x, y, .. } => {
            assert_eq!(x, 100.0);
            assert_eq!(y, 200.0);
        }
        other => panic!("Expected MouseMove, got {:?}", other),
    }

    // Wakeup pipe should have been written to, clear it
    emacs.wakeup_clear.clear();
}

// ===================================================================
// WakeupClear
// ===================================================================

#[cfg(unix)]
#[test]
fn wakeup_clear_drains_pipe() {
    let comms = ThreadComms::new().unwrap();
    let (emacs, render) = comms.split();

    // Wake via RenderComms
    render.wakeup.wake();
    render.wakeup.wake();

    // Clear via EmacsComms
    emacs.wakeup_clear.clear();

    // Pipe should be empty
    let mut buf = [0u8; 1];
    let n = unsafe {
        let flags = libc::fcntl(emacs.wakeup_read_fd, libc::F_GETFL);
        libc::fcntl(
            emacs.wakeup_read_fd,
            libc::F_SETFL,
            flags | libc::O_NONBLOCK,
        );
        let n = libc::read(emacs.wakeup_read_fd, buf.as_mut_ptr() as *mut _, 1);
        libc::fcntl(emacs.wakeup_read_fd, libc::F_SETFL, flags);
        n
    };
    assert!(n <= 0, "pipe should be drained after WakeupClear::clear()");
}

// ===================================================================
// InputEvent enum variant construction
// ===================================================================

#[test]
fn input_event_key_construction() {
    let event = InputEvent::Key {
        keysym: 0xFF0D, // Return
        modifiers: 4,   // Ctrl
        pressed: true,
        emacs_frame_id: 0,
    };
    match event {
        InputEvent::Key {
            keysym,
            modifiers,
            pressed,
            emacs_frame_id,
        } => {
            assert_eq!(keysym, 0xFF0D);
            assert_eq!(modifiers, 4);
            assert!(pressed);
            assert_eq!(emacs_frame_id, 0);
        }
        _ => panic!("Wrong variant"),
    }
}

#[test]
fn input_event_mouse_button_construction() {
    let event = InputEvent::MouseButton {
        button: 1,
        x: 50.5,
        y: 100.3,
        pressed: true,
        modifiers: 0,
        target_frame_id: 0,
        webkit_id: 0,
        webkit_rel_x: 0,
        webkit_rel_y: 0,
    };
    match event {
        InputEvent::MouseButton {
            button,
            x,
            y,
            pressed,
            modifiers,
            target_frame_id,
            ..
        } => {
            assert_eq!(button, 1);
            assert_eq!(x, 50.5);
            assert_eq!(y, 100.3);
            assert!(pressed);
            assert_eq!(modifiers, 0);
            assert_eq!(target_frame_id, 0);
        }
        _ => panic!("Wrong variant"),
    }
}

#[test]
fn input_event_mouse_move_construction() {
    let event = InputEvent::MouseMove {
        x: 200.0,
        y: 300.0,
        modifiers: 1,
        target_frame_id: 42,
    };
    match event {
        InputEvent::MouseMove {
            x,
            y,
            modifiers,
            target_frame_id,
        } => {
            assert_eq!(x, 200.0);
            assert_eq!(y, 300.0);
            assert_eq!(modifiers, 1);
            assert_eq!(target_frame_id, 42);
        }
        _ => panic!("Wrong variant"),
    }
}

#[test]
fn input_event_mouse_scroll_construction() {
    let event = InputEvent::MouseScroll {
        delta_x: 0.0,
        delta_y: -3.0,
        x: 400.0,
        y: 500.0,
        modifiers: 0,
        pixel_precise: false,
        target_frame_id: 0,
        webkit_id: 0,
        webkit_rel_x: 0,
        webkit_rel_y: 0,
    };
    match event {
        InputEvent::MouseScroll {
            delta_x,
            delta_y,
            pixel_precise,
            ..
        } => {
            assert_eq!(delta_x, 0.0);
            assert_eq!(delta_y, -3.0);
            assert!(!pixel_precise);
        }
        _ => panic!("Wrong variant"),
    }
}

#[test]
fn input_event_mouse_scroll_pixel_precise() {
    let event = InputEvent::MouseScroll {
        delta_x: 10.5,
        delta_y: -25.3,
        x: 0.0,
        y: 0.0,
        modifiers: 0,
        pixel_precise: true,
        target_frame_id: 0,
        webkit_id: 0,
        webkit_rel_x: 0,
        webkit_rel_y: 0,
    };
    match event {
        InputEvent::MouseScroll { pixel_precise, .. } => assert!(pixel_precise),
        _ => panic!("Wrong variant"),
    }
}

#[test]
fn input_event_window_resize_construction() {
    let event = InputEvent::WindowResize {
        width: 1920,
        height: 1080,
        emacs_frame_id: 0,
    };
    match event {
        InputEvent::WindowResize {
            width,
            height,
            emacs_frame_id,
        } => {
            assert_eq!(width, 1920);
            assert_eq!(height, 1080);
            assert_eq!(emacs_frame_id, 0);
        }
        _ => panic!("Wrong variant"),
    }
}

#[test]
fn input_event_window_close_construction() {
    let event = InputEvent::WindowClose {
        emacs_frame_id: 123,
    };
    match event {
        InputEvent::WindowClose { emacs_frame_id } => assert_eq!(emacs_frame_id, 123),
        _ => panic!("Wrong variant"),
    }
}

#[test]
fn input_event_window_focus_construction() {
    let focused = InputEvent::WindowFocus {
        focused: true,
        emacs_frame_id: 0,
    };
    match focused {
        InputEvent::WindowFocus {
            focused,
            emacs_frame_id,
        } => {
            assert!(focused);
            assert_eq!(emacs_frame_id, 0);
        }
        _ => panic!("Wrong variant"),
    }

    let unfocused = InputEvent::WindowFocus {
        focused: false,
        emacs_frame_id: 5,
    };
    match unfocused {
        InputEvent::WindowFocus {
            focused,
            emacs_frame_id,
        } => {
            assert!(!focused);
            assert_eq!(emacs_frame_id, 5);
        }
        _ => panic!("Wrong variant"),
    }
}

#[test]
fn input_event_image_dimensions_ready_construction() {
    let event = InputEvent::ImageDimensionsReady {
        id: 7,
        width: 640,
        height: 480,
    };
    match event {
        InputEvent::ImageDimensionsReady { id, width, height } => {
            assert_eq!(id, 7);
            assert_eq!(width, 640);
            assert_eq!(height, 480);
        }
        _ => panic!("Wrong variant"),
    }
}

#[test]
fn input_event_menu_selection_construction() {
    let selected = InputEvent::MenuSelection { index: 3 };
    match selected {
        InputEvent::MenuSelection { index } => assert_eq!(index, 3),
        _ => panic!("Wrong variant"),
    }

    let cancelled = InputEvent::MenuSelection { index: -1 };
    match cancelled {
        InputEvent::MenuSelection { index } => assert_eq!(index, -1),
        _ => panic!("Wrong variant"),
    }
}

#[test]
fn input_event_file_drop_construction() {
    let event = InputEvent::FileDrop {
        paths: vec![
            "/home/user/file.txt".to_string(),
            "/tmp/image.png".to_string(),
        ],
        x: 100.0,
        y: 200.0,
    };
    match event {
        InputEvent::FileDrop { paths, x, y } => {
            assert_eq!(paths.len(), 2);
            assert_eq!(paths[0], "/home/user/file.txt");
            assert_eq!(paths[1], "/tmp/image.png");
            assert_eq!(x, 100.0);
            assert_eq!(y, 200.0);
        }
        _ => panic!("Wrong variant"),
    }
}

#[test]
fn input_event_clone() {
    let original = InputEvent::Key {
        keysym: 42,
        modifiers: 8,
        pressed: false,
        emacs_frame_id: 0,
    };
    let cloned = original.clone();
    match cloned {
        InputEvent::Key {
            keysym,
            modifiers,
            pressed,
            emacs_frame_id,
        } => {
            assert_eq!(keysym, 42);
            assert_eq!(modifiers, 8);
            assert!(!pressed);
            assert_eq!(emacs_frame_id, 0);
        }
        _ => panic!("Clone changed variant"),
    }
}

#[test]
fn input_event_debug() {
    let event = InputEvent::Key {
        keysym: 65,
        modifiers: 0,
        pressed: true,
        emacs_frame_id: 0,
    };
    let debug = format!("{:?}", event);
    assert!(
        debug.contains("Key"),
        "Debug output should contain variant name: {}",
        debug
    );
}

// ===================================================================
// RenderCommand enum variant construction
// ===================================================================

#[test]
fn render_command_shutdown() {
    let cmd = RenderCommand::Shutdown;
    match cmd {
        RenderCommand::Shutdown => {}
        other => panic!("Expected Shutdown, got {:?}", other),
    }
}

#[test]
fn render_command_scroll_blit() {
    let cmd = RenderCommand::ScrollBlit {
        x: 0,
        y: 100,
        width: 800,
        height: 500,
        from_y: 100,
        to_y: 116,
        bg_r: 0.1,
        bg_g: 0.1,
        bg_b: 0.1,
    };
    match cmd {
        RenderCommand::ScrollBlit {
            x,
            y,
            width,
            height,
            from_y,
            to_y,
            bg_r,
            bg_g,
            bg_b,
        } => {
            assert_eq!(x, 0);
            assert_eq!(y, 100);
            assert_eq!(width, 800);
            assert_eq!(height, 500);
            assert_eq!(from_y, 100);
            assert_eq!(to_y, 116);
            assert_eq!(bg_r, 0.1);
            assert_eq!(bg_g, 0.1);
            assert_eq!(bg_b, 0.1);
        }
        other => panic!("Expected ScrollBlit, got {:?}", other),
    }
}

#[test]
fn render_command_image_load_file() {
    let cmd = RenderCommand::ImageLoadFile {
        id: 1,
        path: "/home/user/photo.png".to_string(),
        max_width: 1024,
        max_height: 768,
        fg_color: 0,
        bg_color: 0,
    };
    match cmd {
        RenderCommand::ImageLoadFile {
            id,
            path,
            max_width,
            max_height,
            fg_color,
            bg_color,
        } => {
            assert_eq!(id, 1);
            assert_eq!(path, "/home/user/photo.png");
            assert_eq!(max_width, 1024);
            assert_eq!(max_height, 768);
            assert_eq!(fg_color, 0);
            assert_eq!(bg_color, 0);
        }
        other => panic!("Expected ImageLoadFile, got {:?}", other),
    }
}

#[test]
fn render_command_image_free() {
    let cmd = RenderCommand::ImageFree { id: 42 };
    match cmd {
        RenderCommand::ImageFree { id } => assert_eq!(id, 42),
        other => panic!("Expected ImageFree, got {:?}", other),
    }
}

#[test]
fn render_command_webkit_create() {
    let cmd = RenderCommand::WebKitCreate {
        id: 1,
        width: 800,
        height: 600,
    };
    match cmd {
        RenderCommand::WebKitCreate { id, width, height } => {
            assert_eq!(id, 1);
            assert_eq!(width, 800);
            assert_eq!(height, 600);
        }
        other => panic!("Expected WebKitCreate, got {:?}", other),
    }
}

#[test]
fn render_command_webkit_load_uri() {
    let cmd = RenderCommand::WebKitLoadUri {
        id: 1,
        url: "https://example.com".to_string(),
    };
    match cmd {
        RenderCommand::WebKitLoadUri { id, url } => {
            assert_eq!(id, 1);
            assert_eq!(url, "https://example.com");
        }
        other => panic!("Expected WebKitLoadUri, got {:?}", other),
    }
}

#[test]
fn render_command_set_mouse_cursor() {
    let cmd = RenderCommand::SetMouseCursor { cursor_type: 2 };
    match cmd {
        RenderCommand::SetMouseCursor { cursor_type } => assert_eq!(cursor_type, 2),
        other => panic!("Expected SetMouseCursor, got {:?}", other),
    }
}

#[test]
fn render_command_warp_mouse() {
    let cmd = RenderCommand::WarpMouse { x: 500, y: 300 };
    match cmd {
        RenderCommand::WarpMouse { x, y } => {
            assert_eq!(x, 500);
            assert_eq!(y, 300);
        }
        other => panic!("Expected WarpMouse, got {:?}", other),
    }
}

#[test]
fn render_command_set_window_title() {
    let cmd = RenderCommand::SetWindowTitle {
        title: "Neomacs - main.rs".to_string(),
    };
    match cmd {
        RenderCommand::SetWindowTitle { title } => {
            assert_eq!(title, "Neomacs - main.rs");
        }
        other => panic!("Expected SetWindowTitle, got {:?}", other),
    }
}

#[test]
fn render_command_set_window_fullscreen() {
    // Test all modes
    for mode in [0u32, 1, 4] {
        let cmd = RenderCommand::SetWindowFullscreen { mode };
        match cmd {
            RenderCommand::SetWindowFullscreen { mode: m } => assert_eq!(m, mode),
            other => panic!("Expected SetWindowFullscreen, got {:?}", other),
        }
    }
}

#[test]
fn render_command_set_window_minimized() {
    let cmd = RenderCommand::SetWindowMinimized { minimized: true };
    match cmd {
        RenderCommand::SetWindowMinimized { minimized } => assert!(minimized),
        other => panic!("Expected SetWindowMinimized, got {:?}", other),
    }
}

#[test]
fn render_command_set_window_position() {
    let cmd = RenderCommand::SetWindowPosition { x: 100, y: 200 };
    match cmd {
        RenderCommand::SetWindowPosition { x, y } => {
            assert_eq!(x, 100);
            assert_eq!(y, 200);
        }
        other => panic!("Expected SetWindowPosition, got {:?}", other),
    }
}

#[test]
fn render_command_set_window_size() {
    let cmd = RenderCommand::SetWindowSize {
        width: 1280,
        height: 720,
    };
    match cmd {
        RenderCommand::SetWindowSize { width, height } => {
            assert_eq!(width, 1280);
            assert_eq!(height, 720);
        }
        other => panic!("Expected SetWindowSize, got {:?}", other),
    }
}

#[test]
fn render_command_resize_window() {
    let geometry_hints = GuiFrameGeometryHints {
        base_width: 42,
        base_height: 58,
        min_width: 42,
        min_height: 58,
        width_inc: 26,
        height_inc: 58,
    };
    let cmd = RenderCommand::ResizeWindow {
        emacs_frame_id: 99,
        width: 1024,
        height: 768,
        geometry_hints,
    };
    match cmd {
        RenderCommand::ResizeWindow {
            emacs_frame_id,
            width,
            height,
            geometry_hints: actual_hints,
        } => {
            assert_eq!(emacs_frame_id, 99);
            assert_eq!(width, 1024);
            assert_eq!(height, 768);
            assert_eq!(actual_hints, geometry_hints);
        }
        other => panic!("Expected ResizeWindow, got {:?}", other),
    }
}

#[test]
fn render_command_set_frame_geometry_hints() {
    let geometry_hints = GuiFrameGeometryHints {
        base_width: 42,
        base_height: 58,
        min_width: 42,
        min_height: 58,
        width_inc: 26,
        height_inc: 58,
    };
    let cmd = RenderCommand::SetFrameGeometryHints {
        emacs_frame_id: 0,
        geometry_hints,
    };
    match cmd {
        RenderCommand::SetFrameGeometryHints {
            emacs_frame_id,
            geometry_hints: actual_hints,
        } => {
            assert_eq!(emacs_frame_id, 0);
            assert_eq!(actual_hints, geometry_hints);
        }
        other => panic!("Expected SetFrameGeometryHints, got {:?}", other),
    }
}

#[test]
fn render_command_set_window_decorated() {
    let cmd = RenderCommand::SetWindowDecorated { decorated: false };
    match cmd {
        RenderCommand::SetWindowDecorated { decorated } => assert!(!decorated),
        other => panic!("Expected SetWindowDecorated, got {:?}", other),
    }
}

#[test]
fn render_command_set_cursor_blink() {
    let cmd = RenderCommand::SetCursorBlink {
        enabled: true,
        interval_ms: 500,
    };
    match cmd {
        RenderCommand::SetCursorBlink {
            enabled,
            interval_ms,
        } => {
            assert!(enabled);
            assert_eq!(interval_ms, 500);
        }
        other => panic!("Expected SetCursorBlink, got {:?}", other),
    }
}

#[test]
fn render_command_set_cursor_animation() {
    let cmd = RenderCommand::SetCursorAnimation {
        enabled: true,
        speed: 0.85,
    };
    match cmd {
        RenderCommand::SetCursorAnimation { enabled, speed } => {
            assert!(enabled);
            assert_eq!(speed, 0.85);
        }
        other => panic!("Expected SetCursorAnimation, got {:?}", other),
    }
}

#[test]
fn render_command_set_animation_config() {
    let cmd = RenderCommand::SetAnimationConfig {
        cursor_enabled: true,
        cursor_speed: 0.9,
        cursor_style: crate::core::types::CursorAnimStyle::EaseOutCubic,
        cursor_duration_ms: 150,
        transition_policy: TransitionPolicy::from_indices(true, 200, 0, 0, true, 150, 1, 2),
        trail_size: 0.5,
    };
    match cmd {
        RenderCommand::SetAnimationConfig {
            cursor_enabled,
            cursor_speed,
            cursor_style,
            cursor_duration_ms,
            transition_policy,
            trail_size,
        } => {
            assert!(cursor_enabled);
            assert_eq!(cursor_speed, 0.9);
            assert_eq!(
                cursor_style,
                crate::core::types::CursorAnimStyle::EaseOutCubic
            );
            assert_eq!(cursor_duration_ms, 150);
            assert!(transition_policy.crossfade_enabled);
            assert_eq!(transition_policy.crossfade_duration_ms, 200);
            assert!(transition_policy.scroll_enabled);
            assert_eq!(transition_policy.scroll_duration_ms, 150);
            assert_eq!(
                transition_policy.scroll_effect,
                neomacs_display_protocol::ScrollEffect::Crossfade
            );
            assert_eq!(
                transition_policy.scroll_easing,
                neomacs_display_protocol::ScrollEasing::Spring
            );
            assert_eq!(trail_size, 0.5);
        }
        other => panic!("Expected SetAnimationConfig, got {:?}", other),
    }
}

#[test]
fn render_command_show_popup_menu() {
    let items = vec![
        PopupMenuItem {
            label: "Open".to_string(),
            shortcut: "C-x C-f".to_string(),
            enabled: true,
            separator: false,
            submenu: false,
            depth: 0,
        },
        PopupMenuItem {
            label: String::new(),
            shortcut: String::new(),
            enabled: false,
            separator: true,
            submenu: false,
            depth: 0,
        },
        PopupMenuItem {
            label: "Quit".to_string(),
            shortcut: "C-x C-c".to_string(),
            enabled: true,
            separator: false,
            submenu: false,
            depth: 0,
        },
    ];

    let cmd = RenderCommand::ShowPopupMenu {
        x: 100.0,
        y: 200.0,
        items: items.clone(),
        title: Some("File".to_string()),
        fg: Some((1.0, 1.0, 1.0)),
        bg: Some((0.1, 0.1, 0.1)),
    };
    match cmd {
        RenderCommand::ShowPopupMenu {
            x,
            y,
            items: menu_items,
            title,
            fg,
            bg,
        } => {
            assert_eq!(x, 100.0);
            assert_eq!(y, 200.0);
            assert_eq!(menu_items.len(), 3);
            assert_eq!(menu_items[0].label, "Open");
            assert_eq!(menu_items[0].shortcut, "C-x C-f");
            assert!(menu_items[0].enabled);
            assert!(menu_items[1].separator);
            assert!(!menu_items[1].enabled);
            assert_eq!(title, Some("File".to_string()));
            assert_eq!(fg, Some((1.0, 1.0, 1.0)));
            assert_eq!(bg, Some((0.1, 0.1, 0.1)));
        }
        other => panic!("Expected ShowPopupMenu, got {:?}", other),
    }
}

#[test]
fn render_command_hide_popup_menu() {
    let cmd = RenderCommand::HidePopupMenu;
    match cmd {
        RenderCommand::HidePopupMenu => {}
        other => panic!("Expected HidePopupMenu, got {:?}", other),
    }
}

#[test]
fn render_command_show_tooltip() {
    let cmd = RenderCommand::ShowTooltip {
        x: 300.0,
        y: 400.0,
        text: "This is a tooltip".to_string(),
        fg_r: 1.0,
        fg_g: 1.0,
        fg_b: 1.0,
        bg_r: 0.0,
        bg_g: 0.0,
        bg_b: 0.0,
    };
    match cmd {
        RenderCommand::ShowTooltip {
            x,
            y,
            text,
            fg_r,
            fg_g,
            fg_b,
            bg_r,
            bg_g,
            bg_b,
        } => {
            assert_eq!(x, 300.0);
            assert_eq!(y, 400.0);
            assert_eq!(text, "This is a tooltip");
            assert_eq!(fg_r, 1.0);
            assert_eq!(bg_r, 0.0);
        }
        other => panic!("Expected ShowTooltip, got {:?}", other),
    }
}

#[test]
fn render_command_hide_tooltip() {
    let cmd = RenderCommand::HideTooltip;
    match cmd {
        RenderCommand::HideTooltip => {}
        other => panic!("Expected HideTooltip, got {:?}", other),
    }
}

#[test]
fn render_command_visual_bell() {
    let cmd = RenderCommand::VisualBell;
    match cmd {
        RenderCommand::VisualBell => {}
        other => panic!("Expected VisualBell, got {:?}", other),
    }
}

#[test]
fn render_command_request_attention() {
    let cmd = RenderCommand::RequestAttention { urgent: true };
    match cmd {
        RenderCommand::RequestAttention { urgent } => assert!(urgent),
        other => panic!("Expected RequestAttention, got {:?}", other),
    }
}

#[test]
fn render_command_update_effect() {
    let cmd = RenderCommand::UpdateEffect(EffectUpdater(Box::new(|_config| {
        // no-op for testing
    })));
    match cmd {
        RenderCommand::UpdateEffect(_) => {}
        other => panic!("Expected UpdateEffect, got {:?}", other),
    }
}

#[test]
fn render_command_set_scroll_indicators() {
    let cmd = RenderCommand::SetScrollIndicators { enabled: true };
    match cmd {
        RenderCommand::SetScrollIndicators { enabled } => assert!(enabled),
        other => panic!("Expected SetScrollIndicators, got {:?}", other),
    }
}

#[test]
fn render_command_set_titlebar_height() {
    let cmd = RenderCommand::SetTitlebarHeight { height: 32.0 };
    match cmd {
        RenderCommand::SetTitlebarHeight { height } => assert_eq!(height, 32.0),
        other => panic!("Expected SetTitlebarHeight, got {:?}", other),
    }
}

#[test]
fn render_command_set_show_fps() {
    let cmd = RenderCommand::SetShowFps { enabled: true };
    match cmd {
        RenderCommand::SetShowFps { enabled } => assert!(enabled),
        other => panic!("Expected SetShowFps, got {:?}", other),
    }
}

#[test]
fn render_command_set_corner_radius() {
    let cmd = RenderCommand::SetCornerRadius { radius: 8.0 };
    match cmd {
        RenderCommand::SetCornerRadius { radius } => assert_eq!(radius, 8.0),
        other => panic!("Expected SetCornerRadius, got {:?}", other),
    }
}

#[test]
fn render_command_set_extra_spacing() {
    let cmd = RenderCommand::SetExtraSpacing {
        line_spacing: 2.0,
        letter_spacing: 0.5,
    };
    match cmd {
        RenderCommand::SetExtraSpacing {
            line_spacing,
            letter_spacing,
        } => {
            assert_eq!(line_spacing, 2.0);
            assert_eq!(letter_spacing, 0.5);
        }
        other => panic!("Expected SetExtraSpacing, got {:?}", other),
    }
}

#[test]
fn render_command_set_indent_guide_rainbow() {
    let colors = vec![
        (1.0, 0.0, 0.0, 0.3),
        (0.0, 1.0, 0.0, 0.3),
        (0.0, 0.0, 1.0, 0.3),
    ];
    let cmd = RenderCommand::SetIndentGuideRainbow {
        enabled: true,
        colors: colors.clone(),
    };
    match cmd {
        RenderCommand::SetIndentGuideRainbow { enabled, colors: c } => {
            assert!(enabled);
            assert_eq!(c.len(), 3);
            assert_eq!(c[0], (1.0, 0.0, 0.0, 0.3));
        }
        other => panic!("Expected SetIndentGuideRainbow, got {:?}", other),
    }
}

#[test]
fn render_command_set_cursor_size_transition() {
    let cmd = RenderCommand::SetCursorSizeTransition {
        enabled: true,
        duration_ms: 200,
    };
    match cmd {
        RenderCommand::SetCursorSizeTransition {
            enabled,
            duration_ms,
        } => {
            assert!(enabled);
            assert_eq!(duration_ms, 200);
        }
        other => panic!("Expected SetCursorSizeTransition, got {:?}", other),
    }
}

#[test]
fn render_command_set_ligatures_enabled() {
    let cmd = RenderCommand::SetLigaturesEnabled { enabled: true };
    match cmd {
        RenderCommand::SetLigaturesEnabled { enabled } => assert!(enabled),
        other => panic!("Expected SetLigaturesEnabled, got {:?}", other),
    }
}

#[test]
fn render_command_remove_child_frame() {
    let cmd = RenderCommand::RemoveChildFrame { frame_id: 0xDEAD };
    match cmd {
        RenderCommand::RemoveChildFrame { frame_id } => assert_eq!(frame_id, 0xDEAD),
        other => panic!("Expected RemoveChildFrame, got {:?}", other),
    }
}

#[test]
fn render_command_create_window() {
    let geometry_hints = GuiFrameGeometryHints {
        base_width: 42,
        base_height: 58,
        min_width: 42,
        min_height: 58,
        width_inc: 26,
        height_inc: 58,
    };
    let cmd = RenderCommand::CreateWindow {
        emacs_frame_id: 99,
        width: 1024,
        height: 768,
        title: "New Frame".to_string(),
        geometry_hints,
    };
    match cmd {
        RenderCommand::CreateWindow {
            emacs_frame_id,
            width,
            height,
            title,
            geometry_hints: actual_hints,
        } => {
            assert_eq!(emacs_frame_id, 99);
            assert_eq!(width, 1024);
            assert_eq!(height, 768);
            assert_eq!(title, "New Frame");
            assert_eq!(actual_hints, geometry_hints);
        }
        other => panic!("Expected CreateWindow, got {:?}", other),
    }
}

#[test]
fn render_command_destroy_window() {
    let cmd = RenderCommand::DestroyWindow { emacs_frame_id: 99 };
    match cmd {
        RenderCommand::DestroyWindow { emacs_frame_id } => assert_eq!(emacs_frame_id, 99),
        other => panic!("Expected DestroyWindow, got {:?}", other),
    }
}

#[test]
fn render_command_set_child_frame_style() {
    let cmd = RenderCommand::SetChildFrameStyle {
        corner_radius: 12.0,
        shadow_enabled: true,
        shadow_layers: 3,
        shadow_offset: 4.0,
        shadow_opacity: 0.5,
    };
    match cmd {
        RenderCommand::SetChildFrameStyle {
            corner_radius,
            shadow_enabled,
            shadow_layers,
            shadow_offset,
            shadow_opacity,
        } => {
            assert_eq!(corner_radius, 12.0);
            assert!(shadow_enabled);
            assert_eq!(shadow_layers, 3);
            assert_eq!(shadow_offset, 4.0);
            assert_eq!(shadow_opacity, 0.5);
        }
        other => panic!("Expected SetChildFrameStyle, got {:?}", other),
    }
}

#[test]
fn render_command_webkit_resize() {
    let cmd = RenderCommand::WebKitResize {
        id: 5,
        width: 1024,
        height: 768,
    };
    match cmd {
        RenderCommand::WebKitResize { id, width, height } => {
            assert_eq!(id, 5);
            assert_eq!(width, 1024);
            assert_eq!(height, 768);
        }
        other => panic!("Expected WebKitResize, got {:?}", other),
    }
}

#[test]
fn render_command_webkit_destroy() {
    let cmd = RenderCommand::WebKitDestroy { id: 3 };
    match cmd {
        RenderCommand::WebKitDestroy { id } => assert_eq!(id, 3),
        other => panic!("Expected WebKitDestroy, got {:?}", other),
    }
}

#[test]
fn render_command_webkit_click() {
    let cmd = RenderCommand::WebKitClick {
        id: 1,
        x: 50,
        y: 75,
        button: 1,
    };
    match cmd {
        RenderCommand::WebKitClick { id, x, y, button } => {
            assert_eq!(id, 1);
            assert_eq!(x, 50);
            assert_eq!(y, 75);
            assert_eq!(button, 1);
        }
        other => panic!("Expected WebKitClick, got {:?}", other),
    }
}

#[test]
fn render_command_webkit_scroll() {
    let cmd = RenderCommand::WebKitScroll {
        id: 1,
        x: 0,
        y: 0,
        delta_x: 0,
        delta_y: -3,
    };
    match cmd {
        RenderCommand::WebKitScroll { id, delta_y, .. } => {
            assert_eq!(id, 1);
            assert_eq!(delta_y, -3);
        }
        other => panic!("Expected WebKitScroll, got {:?}", other),
    }
}

#[test]
fn render_command_webkit_key_event() {
    let cmd = RenderCommand::WebKitKeyEvent {
        id: 1,
        keyval: 0xFF0D,
        keycode: 36,
        pressed: true,
        modifiers: 0,
    };
    match cmd {
        RenderCommand::WebKitKeyEvent {
            id,
            keyval,
            keycode,
            pressed,
            modifiers,
        } => {
            assert_eq!(id, 1);
            assert_eq!(keyval, 0xFF0D);
            assert_eq!(keycode, 36);
            assert!(pressed);
            assert_eq!(modifiers, 0);
        }
        other => panic!("Expected WebKitKeyEvent, got {:?}", other),
    }
}

#[test]
fn render_command_webkit_navigation() {
    let back = RenderCommand::WebKitGoBack { id: 1 };
    match back {
        RenderCommand::WebKitGoBack { id } => assert_eq!(id, 1),
        other => panic!("Expected WebKitGoBack, got {:?}", other),
    }

    let fwd = RenderCommand::WebKitGoForward { id: 2 };
    match fwd {
        RenderCommand::WebKitGoForward { id } => assert_eq!(id, 2),
        other => panic!("Expected WebKitGoForward, got {:?}", other),
    }

    let reload = RenderCommand::WebKitReload { id: 3 };
    match reload {
        RenderCommand::WebKitReload { id } => assert_eq!(id, 3),
        other => panic!("Expected WebKitReload, got {:?}", other),
    }
}

#[test]
fn render_command_webkit_execute_javascript() {
    let cmd = RenderCommand::WebKitExecuteJavaScript {
        id: 1,
        script: "document.title".to_string(),
    };
    match cmd {
        RenderCommand::WebKitExecuteJavaScript { id, script } => {
            assert_eq!(id, 1);
            assert_eq!(script, "document.title");
        }
        other => panic!("Expected WebKitExecuteJavaScript, got {:?}", other),
    }
}

#[test]
fn render_command_webkit_set_floating() {
    let cmd = RenderCommand::WebKitSetFloating {
        id: 1,
        x: 10.0,
        y: 20.0,
        width: 400.0,
        height: 300.0,
    };
    match cmd {
        RenderCommand::WebKitSetFloating {
            id,
            x,
            y,
            width,
            height,
        } => {
            assert_eq!(id, 1);
            assert_eq!(x, 10.0);
            assert_eq!(y, 20.0);
            assert_eq!(width, 400.0);
            assert_eq!(height, 300.0);
        }
        other => panic!("Expected WebKitSetFloating, got {:?}", other),
    }
}

#[test]
fn render_command_webkit_remove_floating() {
    let cmd = RenderCommand::WebKitRemoveFloating { id: 7 };
    match cmd {
        RenderCommand::WebKitRemoveFloating { id } => assert_eq!(id, 7),
        other => panic!("Expected WebKitRemoveFloating, got {:?}", other),
    }
}

#[test]
fn render_command_webkit_pointer_event() {
    let cmd = RenderCommand::WebKitPointerEvent {
        id: 1,
        event_type: 2,
        x: 100,
        y: 200,
        button: 1,
        state: 0,
        modifiers: 4,
    };
    match cmd {
        RenderCommand::WebKitPointerEvent {
            id,
            event_type,
            x,
            y,
            button,
            state,
            modifiers,
        } => {
            assert_eq!(id, 1);
            assert_eq!(event_type, 2);
            assert_eq!(x, 100);
            assert_eq!(y, 200);
            assert_eq!(button, 1);
            assert_eq!(state, 0);
            assert_eq!(modifiers, 4);
        }
        other => panic!("Expected WebKitPointerEvent, got {:?}", other),
    }
}

#[test]
fn render_command_video_lifecycle() {
    let create = RenderCommand::VideoCreate {
        id: 1,
        path: "/home/user/video.mp4".to_string(),
    };
    match create {
        RenderCommand::VideoCreate { id, path } => {
            assert_eq!(id, 1);
            assert_eq!(path, "/home/user/video.mp4");
        }
        other => panic!("Expected VideoCreate, got {:?}", other),
    }

    let play = RenderCommand::VideoPlay { id: 1 };
    match play {
        RenderCommand::VideoPlay { id } => assert_eq!(id, 1),
        other => panic!("Expected VideoPlay, got {:?}", other),
    }

    let pause = RenderCommand::VideoPause { id: 1 };
    match pause {
        RenderCommand::VideoPause { id } => assert_eq!(id, 1),
        other => panic!("Expected VideoPause, got {:?}", other),
    }

    let destroy = RenderCommand::VideoDestroy { id: 1 };
    match destroy {
        RenderCommand::VideoDestroy { id } => assert_eq!(id, 1),
        other => panic!("Expected VideoDestroy, got {:?}", other),
    }
}

#[test]
fn render_command_debug() {
    let cmd = RenderCommand::Shutdown;
    let debug = format!("{:?}", cmd);
    assert!(debug.contains("Shutdown"), "Debug output: {}", debug);
}

// ===================================================================
// PopupMenuItem
// ===================================================================

#[test]
fn popup_menu_item_construction() {
    let item = PopupMenuItem {
        label: "Save".to_string(),
        shortcut: "C-x C-s".to_string(),
        enabled: true,
        separator: false,
        submenu: false,
        depth: 0,
    };
    assert_eq!(item.label, "Save");
    assert_eq!(item.shortcut, "C-x C-s");
    assert!(item.enabled);
    assert!(!item.separator);
    assert!(!item.submenu);
    assert_eq!(item.depth, 0);
}

#[test]
fn popup_menu_item_separator() {
    let sep = PopupMenuItem {
        label: String::new(),
        shortcut: String::new(),
        enabled: false,
        separator: true,
        submenu: false,
        depth: 0,
    };
    assert!(sep.separator);
    assert!(!sep.enabled);
}

#[test]
fn popup_menu_item_submenu() {
    let sub = PopupMenuItem {
        label: "Recent Files".to_string(),
        shortcut: String::new(),
        enabled: true,
        separator: false,
        submenu: true,
        depth: 1,
    };
    assert!(sub.submenu);
    assert_eq!(sub.depth, 1);
}

#[test]
fn popup_menu_item_clone() {
    let item = PopupMenuItem {
        label: "Test".to_string(),
        shortcut: "M-x".to_string(),
        enabled: true,
        separator: false,
        submenu: false,
        depth: 2,
    };
    let cloned = item.clone();
    assert_eq!(cloned.label, "Test");
    assert_eq!(cloned.depth, 2);
}

#[test]
fn popup_menu_item_debug() {
    let item = PopupMenuItem {
        label: "Debug".to_string(),
        shortcut: String::new(),
        enabled: true,
        separator: false,
        submenu: false,
        depth: 0,
    };
    let debug = format!("{:?}", item);
    assert!(debug.contains("PopupMenuItem"), "Debug output: {}", debug);
}

// ===================================================================
// EffectUpdater
// ===================================================================

#[test]
fn effect_updater_debug_format() {
    let updater = EffectUpdater(Box::new(|_| {}));
    let debug = format!("{:?}", updater);
    assert_eq!(debug, "EffectUpdater(...)");
}

#[test]
fn effect_updater_closure_executes() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let called = Arc::new(AtomicBool::new(false));
    let called_clone = called.clone();

    let updater = EffectUpdater(Box::new(move |_config| {
        called_clone.store(true, Ordering::SeqCst);
    }));

    let mut config = EffectsConfig::default();
    (updater.0)(&mut config);

    assert!(
        called.load(Ordering::SeqCst),
        "EffectUpdater closure should have been called"
    );
}

// ===================================================================
// Channel operations: send through crossbeam, receive correctly
// ===================================================================

#[test]
fn channel_sends_multiple_input_events_in_order() {
    let comms = ThreadComms::new().unwrap();

    let events = vec![
        InputEvent::Key {
            keysym: 1,
            modifiers: 0,
            pressed: true,
            emacs_frame_id: 0,
        },
        InputEvent::Key {
            keysym: 2,
            modifiers: 0,
            pressed: true,
            emacs_frame_id: 0,
        },
        InputEvent::Key {
            keysym: 3,
            modifiers: 0,
            pressed: true,
            emacs_frame_id: 0,
        },
        InputEvent::MouseMove {
            x: 10.0,
            y: 20.0,
            modifiers: 0,
            target_frame_id: 0,
        },
        InputEvent::WindowResize {
            width: 800,
            height: 600,
            emacs_frame_id: 0,
        },
    ];

    for e in &events {
        comms.input_tx.send(e.clone()).unwrap();
    }

    // Receive and verify order
    for (i, expected) in events.iter().enumerate() {
        let received = comms.input_rx.try_recv().unwrap();
        let expected_debug = format!("{:?}", expected);
        let received_debug = format!("{:?}", received);
        assert_eq!(
            expected_debug, received_debug,
            "Event {} mismatch: expected {:?}, got {:?}",
            i, expected_debug, received_debug
        );
    }

    // No more events
    assert!(comms.input_rx.try_recv().is_err());
}

#[test]
fn channel_sends_multiple_commands_in_order() {
    let comms = ThreadComms::new().unwrap();

    comms.cmd_tx.send(RenderCommand::Shutdown).unwrap();
    comms.cmd_tx.send(RenderCommand::VisualBell).unwrap();
    comms.cmd_tx.send(RenderCommand::HideTooltip).unwrap();

    match comms.cmd_rx.try_recv().unwrap() {
        RenderCommand::Shutdown => {}
        other => panic!("Expected Shutdown, got {:?}", other),
    }
    match comms.cmd_rx.try_recv().unwrap() {
        RenderCommand::VisualBell => {}
        other => panic!("Expected VisualBell, got {:?}", other),
    }
    match comms.cmd_rx.try_recv().unwrap() {
        RenderCommand::HideTooltip => {}
        other => panic!("Expected HideTooltip, got {:?}", other),
    }

    assert!(comms.cmd_rx.try_recv().is_err());
}

#[test]
fn channel_empty_recv_returns_error() {
    let comms = ThreadComms::new().unwrap();
    assert!(comms.input_rx.try_recv().is_err());
    assert!(comms.cmd_rx.try_recv().is_err());
    assert!(comms.frame_rx.try_recv().is_err());
}

// ===================================================================
// Cross-thread usage simulation
// ===================================================================

#[test]
fn cross_thread_input_event_delivery() {
    let comms = ThreadComms::new().unwrap();
    let (emacs, render) = comms.split();

    let handle = std::thread::spawn(move || {
        render.send_input(InputEvent::Key {
            keysym: 0x61, // 'a'
            modifiers: 0,
            pressed: true,
            emacs_frame_id: 0,
        });
        render.send_input(InputEvent::WindowResize {
            width: 1920,
            height: 1080,
            emacs_frame_id: 0,
        });
    });

    handle.join().unwrap();

    // Both events should be receivable on the Emacs side
    let evt1 = emacs.input_rx.try_recv().unwrap();
    match evt1 {
        InputEvent::Key { keysym, .. } => assert_eq!(keysym, 0x61),
        other => panic!("Expected Key, got {:?}", other),
    }

    let evt2 = emacs.input_rx.try_recv().unwrap();
    match evt2 {
        InputEvent::WindowResize { width, height, .. } => {
            assert_eq!(width, 1920);
            assert_eq!(height, 1080);
        }
        other => panic!("Expected WindowResize, got {:?}", other),
    }

    emacs.wakeup_clear.clear();
}

#[test]
fn cross_thread_command_delivery() {
    let comms = ThreadComms::new().unwrap();
    let (emacs, render) = comms.split();

    let handle = std::thread::spawn(move || {
        let cmd = render.cmd_rx.recv().unwrap();
        match cmd {
            RenderCommand::SetWindowTitle { title } => {
                assert_eq!(title, "test-title");
            }
            other => panic!("Expected SetWindowTitle, got {:?}", other),
        }
    });

    emacs
        .cmd_tx
        .send(RenderCommand::SetWindowTitle {
            title: "test-title".to_string(),
        })
        .unwrap();

    handle.join().unwrap();
}

#[test]
fn cross_thread_frame_delivery() {
    let comms = ThreadComms::new().unwrap();
    let (emacs, render) = comms.split();

    let handle = std::thread::spawn(move || {
        let frame = render.frame_rx.recv().unwrap();
        assert_eq!(frame.frame_pixel_width, 1920.0);
        assert_eq!(frame.frame_pixel_height, 1080.0);
    });

    let buf = FrameGlyphBuffer::with_size(1920.0, 1080.0);
    let state = FrameDisplayState::from_frame_glyph_buffer(&buf);
    emacs.frame_tx.send(state).unwrap();

    handle.join().unwrap();
}
