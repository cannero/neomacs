use crate::emacs_core::load::{apply_runtime_startup_state, create_bootstrap_evaluator_cached};
use crate::emacs_core::{
    DisplayHost, Evaluator, GuiFrameHostRequest, Value, format_eval_result, parse_forms,
};
use std::cell::RefCell;
use std::rc::Rc;

/// Evaluate all forms with a fresh evaluator that has a frame+window set up.
fn eval_with_frame(src: &str) -> Vec<String> {
    let mut ev = Evaluator::new();
    let forms = parse_forms(src).expect("parse");
    // Create a buffer for the initial window.
    let buf = ev.buffers.create_buffer("*scratch*");
    // Create a frame so window/frame builtins have something to work with.
    ev.frames.create_frame("F1", 800, 600, buf);
    ev.eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect()
}

fn eval_one_with_frame(src: &str) -> String {
    eval_with_frame(src).into_iter().next().unwrap()
}

fn bootstrap_eval_with_frame(src: &str) -> Vec<String> {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    let forms = parse_forms(src).expect("parse");
    ev.eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect()
}

fn bootstrap_eval_one_with_frame(src: &str) -> String {
    bootstrap_eval_with_frame(src)
        .into_iter()
        .next()
        .expect("result")
}

#[test]
fn active_minibuffer_window_tracks_live_minibuffer_state() {
    let mut ev = Evaluator::new();
    let buf = ev.buffers.create_buffer("*scratch*");
    ev.frames.create_frame("F1", 800, 600, buf);

    let fid = super::ensure_selected_frame_id_in_state(&mut ev.frames, &mut ev.buffers);
    let minibuffer_buffer_id = {
        let frame = ev.frames.get(fid).expect("selected frame");
        let minibuffer_wid = frame.minibuffer_window.expect("minibuffer window");
        frame
            .find_window(minibuffer_wid)
            .and_then(|window| window.buffer_id())
            .expect("minibuffer buffer")
    };
    ev.minibuffers
        .read_from_minibuffer(minibuffer_buffer_id, "M-x ", None, None)
        .expect("active minibuffer state");

    let minibuffer_window = super::builtin_minibuffer_window(&mut ev, vec![]).unwrap();
    let active_minibuffer_window =
        super::builtin_active_minibuffer_window_eval(&mut ev, vec![]).unwrap();
    assert_eq!(active_minibuffer_window, minibuffer_window);
    assert!(!active_minibuffer_window.is_nil());
}

#[derive(Clone, Default)]
struct RecordingDisplayHost {
    requests: Rc<RefCell<Vec<GuiFrameHostRequest>>>,
}

impl RecordingDisplayHost {
    fn new() -> Self {
        Self::default()
    }
}

impl DisplayHost for RecordingDisplayHost {
    fn realize_gui_frame(&mut self, request: GuiFrameHostRequest) -> Result<(), String> {
        self.requests.borrow_mut().push(request);
        Ok(())
    }
}

// -- Window queries --

#[test]
fn bootstrap_window_command_boundary_matches_gnu_emacs() {
    let result = bootstrap_eval_one_with_frame(
        r#"(list (subrp (symbol-function 'select-window))
                 (subrp (symbol-function 'split-window-internal))
                 (subrp (symbol-function 'delete-window-internal))
                 (subrp (symbol-function 'delete-other-windows-internal))
                 (subrp (symbol-function 'other-window-for-scrolling))
                 (subrp (symbol-function 'display-buffer))
                 (subrp (symbol-function 'switch-to-buffer))
                 (subrp (symbol-function 'pop-to-buffer))
                 (subrp (symbol-function 'other-window))
                 (subrp (symbol-function 'delete-window))
                 (subrp (symbol-function 'delete-other-windows))
                 (subrp (symbol-function 'split-window))
                 (subrp (symbol-function 'split-window-below))
                 (subrp (symbol-function 'split-window-right)))"#,
    );
    assert_eq!(result, "OK (t t t t t nil nil nil nil nil nil nil nil nil)");
}

#[test]
fn selected_window_returns_window_handle() {
    let r = eval_one_with_frame("(selected-window)");
    assert!(
        r.starts_with("OK #<window "),
        "expected window handle, got: {r}"
    );
}

#[test]
fn selected_window_bootstraps_initial_frame() {
    let mut ev = Evaluator::new();
    let forms = parse_forms("(window-live-p (selected-window))").expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK t");
}

#[test]
fn frame_selected_window_arity_and_designators() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(windowp (frame-selected-window))
         (windowp (frame-selected-window nil))
         (windowp (frame-selected-window (selected-frame)))
         (condition-case err (frame-selected-window \"x\") (error err))
         (condition-case err (frame-selected-window 999999) (error err))
         (condition-case err (frame-selected-window nil nil) (error (car err)))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK t");
    assert_eq!(out[1], "OK t");
    assert_eq!(out[2], "OK t");
    assert_eq!(out[3], "OK (wrong-type-argument frame-live-p \"x\")");
    assert_eq!(out[4], "OK (wrong-type-argument frame-live-p 999999)");
    assert_eq!(out[5], "OK wrong-number-of-arguments");
}

#[test]
fn minibuffer_window_frame_first_window_and_window_minibuffer_p_semantics() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(window-minibuffer-p)
         (windowp (minibuffer-window))
         (windowp (minibuffer-window (selected-frame)))
         (window-minibuffer-p (minibuffer-window))
         (eq (frame-first-window) (selected-window))
         (eq (frame-first-window (selected-window)) (selected-window))
         (eq (frame-first-window (minibuffer-window)) (selected-window))
         (eq (minibuffer-window) (car (nthcdr (1- (length (window-list nil t))) (window-list nil t))))
         (condition-case err (minibuffer-window 999999) (error err))
         (condition-case err (window-minibuffer-p 999999) (error err))
         (condition-case err (frame-first-window 999999) (error err))
         (condition-case err (minibuffer-window (selected-window)) (error (car err)))
         (condition-case err (window-minibuffer-p nil nil) (error (car err)))
         (condition-case err (frame-first-window nil nil) (error (car err)))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK nil");
    assert_eq!(out[1], "OK t");
    assert_eq!(out[2], "OK t");
    assert_eq!(out[3], "OK t");
    assert_eq!(out[4], "OK t");
    assert_eq!(out[5], "OK t");
    assert_eq!(out[6], "OK t");
    assert_eq!(out[7], "OK t");
    assert_eq!(out[8], "OK (wrong-type-argument frame-live-p 999999)");
    assert_eq!(out[9], "OK (wrong-type-argument window-valid-p 999999)");
    assert_eq!(out[10], "OK (wrong-type-argument frame-live-p 999999)");
    assert_eq!(out[11], "OK wrong-type-argument");
    assert_eq!(out[12], "OK wrong-number-of-arguments");
    assert_eq!(out[13], "OK wrong-number-of-arguments");
}

#[test]
fn frame_root_window_window_valid_and_minibuffer_activity_semantics() {
    let out = bootstrap_eval_with_frame(
        "(window-valid-p (selected-window))
         (window-valid-p (minibuffer-window))
         (window-valid-p nil)
         (window-valid-p 999999)
         (window-valid-p 'foo)
         (eq (frame-root-window) (selected-window))
         (eq (frame-root-window (selected-frame)) (selected-window))
         (eq (frame-root-window (selected-window)) (selected-window))
         (eq (frame-root-window (minibuffer-window)) (selected-window))
         (minibuffer-selected-window)
         (active-minibuffer-window)
         (minibuffer-window-active-p (minibuffer-window))
         (minibuffer-window-active-p (selected-window))
         (minibuffer-window-active-p nil)
         (minibuffer-window-active-p 999999)
         (minibuffer-window-active-p 'foo)
         (condition-case err (window-valid-p) (error err))
         (condition-case err (window-valid-p nil nil) (error err))
         (condition-case err (frame-root-window 999999) (error err))
         (condition-case err (frame-root-window 'foo) (error err))
         (condition-case err (frame-root-window nil nil) (error err))
         (condition-case err (minibuffer-selected-window nil) (error err))
         (condition-case err (active-minibuffer-window nil) (error err))
         (condition-case err (minibuffer-window-active-p) (error err))
         (condition-case err (minibuffer-window-active-p nil nil) (error err))",
    );
    assert_eq!(out[0], "OK t");
    assert_eq!(out[1], "OK t");
    assert_eq!(out[2], "OK nil");
    assert_eq!(out[3], "OK nil");
    assert_eq!(out[4], "OK nil");
    assert_eq!(out[5], "OK t");
    assert_eq!(out[6], "OK t");
    assert_eq!(out[7], "OK t");
    assert_eq!(out[8], "OK t");
    assert_eq!(out[9], "OK nil");
    assert_eq!(out[10], "OK nil");
    assert_eq!(out[11], "OK nil");
    assert_eq!(out[12], "OK nil");
    assert_eq!(out[13], "OK nil");
    assert_eq!(out[14], "OK nil");
    assert_eq!(out[15], "OK nil");
    assert_eq!(out[16], "OK (wrong-number-of-arguments window-valid-p 0)");
    assert_eq!(out[17], "OK (wrong-number-of-arguments window-valid-p 2)");
    assert_eq!(out[18], "OK (wrong-type-argument frame-live-p 999999)");
    assert_eq!(out[19], "OK (wrong-type-argument frame-live-p foo)");
    assert_eq!(
        out[20],
        "OK (wrong-number-of-arguments frame-root-window 2)"
    );
    assert_eq!(
        out[21],
        "OK (wrong-number-of-arguments minibuffer-selected-window 1)"
    );
    assert_eq!(
        out[22],
        "OK (wrong-number-of-arguments active-minibuffer-window 1)"
    );
    assert_eq!(
        out[23],
        "OK (wrong-number-of-arguments minibuffer-window-active-p 0)"
    );
    assert_eq!(
        out[24],
        "OK (wrong-number-of-arguments minibuffer-window-active-p 2)"
    );
}

#[test]
fn frame_root_window_p_semantics_and_errors() {
    let out = bootstrap_eval_with_frame(
        "(frame-root-window-p (selected-window))
         (frame-root-window-p (minibuffer-window))
         (condition-case err (frame-root-window-p 999999) (error err))
         (condition-case err (frame-root-window-p 'foo) (error err))
         (condition-case err (frame-root-window-p) (error (car err)))
         (condition-case err (frame-root-window-p nil nil) (error (car err)))",
    );
    assert_eq!(out[0], "OK t");
    assert_eq!(out[1], "OK nil");
    assert_eq!(out[2], "OK (wrong-type-argument frame-live-p 999999)");
    assert_eq!(out[3], "OK (wrong-type-argument frame-live-p foo)");
    assert_eq!(out[4], "OK wrong-number-of-arguments");
    assert_eq!(out[5], "OK wrong-number-of-arguments");
}

#[test]
fn window_at_matches_batch_coordinate_and_error_semantics() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(windowp (window-at 0 0))
         (windowp (window-at 79 0))
         (null (window-at 80 0))
         (windowp (window-at 0 23))
         (let ((w (window-at 0 24))) (and w (window-minibuffer-p w)))
         (null (window-at 0 25))
         (null (window-at -1 0))
         (null (window-at 0 -1))
         (windowp (window-at 79.9 0))
         (null (window-at 80.0 0))
         (windowp (window-at 0 24.1))
         (condition-case err (window-at 'foo 0) (error err))
         (condition-case err (window-at 0 'foo) (error err))
         (condition-case err (window-at 0 0 999999) (error err))
         (condition-case err (window-at 0) (error (car err)))
         (condition-case err (window-at 0 0 nil nil) (error (car err)))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK t");
    assert_eq!(out[1], "OK t");
    assert_eq!(out[2], "OK t");
    assert_eq!(out[3], "OK t");
    assert_eq!(out[4], "OK t");
    assert_eq!(out[5], "OK t");
    assert_eq!(out[6], "OK t");
    assert_eq!(out[7], "OK t");
    assert_eq!(out[8], "OK t");
    assert_eq!(out[9], "OK t");
    assert_eq!(out[10], "OK t");
    assert_eq!(out[11], "OK (wrong-type-argument numberp foo)");
    assert_eq!(out[12], "OK (wrong-type-argument numberp foo)");
    assert_eq!(out[13], "OK (wrong-type-argument frame-live-p 999999)");
    assert_eq!(out[14], "OK wrong-number-of-arguments");
    assert_eq!(out[15], "OK wrong-number-of-arguments");
}

#[test]
fn window_frame_arity_and_designators() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(framep (window-frame))
         (framep (window-frame nil))
         (framep (window-frame (selected-window)))
         (condition-case err (window-frame \"x\") (error err))
         (condition-case err (window-frame 999999) (error err))
         (condition-case err (window-frame nil nil) (error (car err)))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK t");
    assert_eq!(out[1], "OK t");
    assert_eq!(out[2], "OK t");
    assert_eq!(out[3], "OK (wrong-type-argument window-valid-p \"x\")");
    assert_eq!(out[4], "OK (wrong-type-argument window-valid-p 999999)");
    assert_eq!(out[5], "OK wrong-number-of-arguments");
}

#[test]
fn window_designators_bootstrap_nil_and_validate_invalid_window_handles() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(window-start nil)
         (window-point nil)
         (window-buffer nil)
         (condition-case err (window-start 999999) (error err))
         (condition-case err (window-buffer 999999) (error err))
         (condition-case err (set-window-start nil 1) (error err))
         (condition-case err (set-window-point nil 1) (error err))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK 1");
    assert_eq!(out[1], "OK 1");
    assert!(
        out[2].starts_with("OK #<buffer "),
        "unexpected value: {}",
        out[2]
    );
    assert_eq!(out[3], "OK (wrong-type-argument window-live-p 999999)");
    assert_eq!(out[4], "OK (wrong-type-argument windowp 999999)");
    assert_eq!(out[5], "OK 1");
    assert_eq!(out[6], "OK 1");
}

#[test]
fn windowp_true() {
    let r = eval_with_frame("(windowp (selected-window))");
    assert_eq!(r[0], "OK t");
}

#[test]
fn windowp_true_for_stale_deleted_window() {
    let r = eval_one_with_frame(
        "(let ((w (split-window-internal (selected-window) nil nil nil)))
           (delete-window w)
           (windowp w))",
    );
    assert_eq!(r, "OK t");
}

#[test]
fn windowp_false() {
    let r = eval_one_with_frame("(windowp 999999)");
    assert_eq!(r, "OK nil");
}

#[test]
fn window_live_p_true() {
    let r = eval_with_frame("(window-live-p (selected-window))");
    assert_eq!(r[0], "OK t");
}

#[test]
fn window_live_p_false_for_non_window() {
    let r = eval_one_with_frame("(window-live-p 999999)");
    assert_eq!(r, "OK nil");
}

#[test]
fn window_buffer_returns_buffer() {
    let r = eval_one_with_frame("(bufferp (window-buffer))");
    assert_eq!(r, "OK t");
}

#[test]
fn window_buffer_returns_nil_for_stale_deleted_window() {
    let r = eval_one_with_frame(
        "(let ((w (split-window-internal (selected-window) nil nil nil)))
           (delete-window w)
           (window-buffer w))",
    );
    assert_eq!(r, "OK nil");
}

#[test]
fn window_start_default() {
    let r = eval_one_with_frame("(window-start)");
    assert_eq!(r, "OK 0");
}

#[test]
fn set_window_start_and_read() {
    let results = eval_with_frame(
        "(let ((w (selected-window)))
            (with-current-buffer (window-buffer w)
              (erase-buffer)
              (insert (make-string 200 ?x)))
            (set-window-start w 42))
         (window-start)",
    );
    assert_eq!(results[0], "OK 42");
    assert_eq!(results[1], "OK 42");
}

#[test]
fn window_point_default() {
    let r = eval_one_with_frame("(window-point)");
    assert_eq!(r, "OK 0");
}

#[test]
fn set_window_point_and_read() {
    let results = eval_with_frame(
        "(let ((w (selected-window)))
            (with-current-buffer (window-buffer w)
              (erase-buffer)
              (insert (make-string 200 ?x)))
            (set-window-point w 10))
         (window-point)",
    );
    assert_eq!(results[0], "OK 10");
    assert_eq!(results[1], "OK 10");
}

#[test]
fn set_window_start_point_and_group_start_accept_marker_positions() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(let* ((w (selected-window))
                (m (with-current-buffer (window-buffer w)
                     (erase-buffer)
                     (insert \"abcdef\")
                     (goto-char 3)
                     (point-marker))))
           (list (markerp (set-window-start w m))
                 (window-start w)
                 (set-window-point w m)
                 (window-point w)
                 (markerp (set-window-group-start w m))
                 (window-start w)
                 (window-point w)))
         (let* ((w (selected-window))
                (_ (progn
                     (set-window-start w 7)
                     (set-window-point w 7)))
                (m (with-current-buffer (get-buffer-create \" *neovm-marker-other*\")
                     (erase-buffer)
                     (insert \"xyz\")
                     (goto-char 2)
                     (point-marker))))
           (list (markerp (set-window-start w m))
                 (window-start w)
                 (set-window-point w m)
                 (window-point w)
                 (markerp (set-window-group-start w m))
                 (window-start w)
                 (window-point w)))
         (let* ((w (selected-window))
                (_ (set-window-start w 1))
                (_ (set-window-point w 1)))
           (list (= (set-window-start w 0) 0)
                 (= (window-start w) 1)
                 (= (set-window-point w 0) 0)
                 (= (window-point w) 1)
                 (= (set-window-group-start w 0) 0)
                 (= (window-group-start w) 1)
                 (= (window-point w) 1)
                 (= (set-window-start w -10) -10)
                 (= (window-start w) 1)
                 (= (set-window-point w -10) -10)
                 (= (window-point w) 1)
                 (= (set-window-group-start w -10) -10)
                 (= (window-group-start w) 1)
                 (= (window-point w) 1)))
         (let* ((w (selected-window))
                (_ (set-window-start w 1))
                (_ (set-window-point w 1))
                (m0 (make-marker))
                (_ (set-marker m0 0 (window-buffer w)))
                (mneg (make-marker))
                (_ (set-marker mneg -5 (window-buffer w))))
           (list (markerp (set-window-start w m0))
                 (= (window-start w) 1)
                 (= (set-window-point w m0) 1)
                 (= (window-point w) 1)
                 (markerp (set-window-group-start w m0))
                 (= (window-group-start w) 1)
                 (= (window-point w) 1)
                 (markerp (set-window-start w mneg))
                 (= (window-start w) 1)
                 (= (set-window-point w mneg) 1)
                 (= (window-point w) 1)
                 (markerp (set-window-group-start w mneg))
                 (= (window-group-start w) 1)
                 (= (window-point w) 1)))
         (let* ((w (selected-window))
                (_ (with-current-buffer (window-buffer w)
                     (erase-buffer)
                     (insert \"abcdef\")
                     (goto-char 1)))
                (_ (set-window-start w 1))
                (_ (set-window-point w 1)))
           (list (= (set-window-start w 9999) 9999)
                 (= (window-start w) 7)
                 (= (set-window-point w 9999) 9999)
                 (= (window-point w) 7)
                 (= (set-window-group-start w 9999) 9999)
                 (= (window-group-start w) 7)
                 (= (window-point w) 7)))
         (let* ((w (selected-window))
                (_ (with-current-buffer (window-buffer w)
                     (erase-buffer)
                     (insert \"abcdef\")
                     (goto-char 1)))
                (m (make-marker))
                (_ (set-marker m 9999 (window-buffer w))))
           (list (markerp (set-window-start w m))
                 (= (window-start w) 7)
                 (= (set-window-point w m) 7)
                 (= (window-point w) 7)
                 (markerp (set-window-group-start w m))
                 (= (window-group-start w) 7)
                 (= (window-point w) 7)))
         (let ((m (make-marker)))
           (list (condition-case err (set-window-start (selected-window) m) (error err))
                 (condition-case err (set-window-point (selected-window) m) (error err))
                 (condition-case err (set-window-group-start (selected-window) m) (error err))))
         (list (condition-case err (set-window-start nil 1.5) (error err))
               (condition-case err (set-window-point nil 1.5) (error err))
               (condition-case err (set-window-group-start nil 1.5) (error err))
               (condition-case err (set-window-start nil 'foo) (error err))
               (condition-case err (set-window-point nil 'foo) (error err))
               (condition-case err (set-window-group-start nil 'foo) (error err)))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK (t 3 3 3 t 3 3)");
    assert_eq!(out[1], "OK (t 2 2 2 t 2 2)");
    assert_eq!(out[2], "OK (t t t t t t t t t t t t t t)");
    assert_eq!(out[3], "OK (t t t t t t t t t t t t t t)");
    assert_eq!(out[4], "OK (t t t t t t t)");
    assert_eq!(out[5], "OK (t t t t t t t)");
    assert_eq!(
        out[6],
        "OK (#<marker in no buffer> (error \"Marker does not point anywhere\") #<marker in no buffer>)"
    );
    assert_eq!(
        out[7],
        "OK ((wrong-type-argument integer-or-marker-p 1.5) (wrong-type-argument integer-or-marker-p 1.5) (wrong-type-argument integer-or-marker-p 1.5) (wrong-type-argument integer-or-marker-p foo) (wrong-type-argument integer-or-marker-p foo) (wrong-type-argument integer-or-marker-p foo))"
    );
}

#[test]
fn window_height_positive() {
    let r = eval_one_with_frame("(window-height)");
    assert!(r.starts_with("OK "));
    let val: i64 = r.strip_prefix("OK ").unwrap().trim().parse().unwrap();
    assert!(val > 0, "window-height should be positive, got {val}");
}

#[test]
fn window_width_positive() {
    let r = eval_one_with_frame("(window-width)");
    assert!(r.starts_with("OK "));
    let val: i64 = r.strip_prefix("OK ").unwrap().trim().parse().unwrap();
    assert!(val > 0, "window-width should be positive, got {val}");
}

#[test]
fn window_body_height_pixelwise() {
    let r = eval_one_with_frame("(window-body-height nil t)");
    assert!(r.starts_with("OK "));
    let val: i64 = r.strip_prefix("OK ").unwrap().trim().parse().unwrap();
    // Batch mode returns character rows.
    assert_eq!(val, 36);
}

#[test]
fn window_body_width_pixelwise() {
    let r = eval_one_with_frame("(window-body-width nil t)");
    assert!(r.starts_with("OK "));
    let val: i64 = r.strip_prefix("OK ").unwrap().trim().parse().unwrap();
    // Batch mode returns character columns.
    assert_eq!(val, 100);
}

#[test]
fn window_total_size_queries_work() {
    let results = eval_with_frame(
        "(list (integerp (window-total-height))
               (integerp (window-total-width))
               (integerp (window-total-height nil t))
               (integerp (window-total-width nil t)))",
    );
    assert_eq!(results[0], "OK (t t t t)");
}

#[test]
fn get_buffer_window_finds_selected_window_for_current_buffer() {
    let result = eval_one_with_frame(
        "(let ((w (selected-window)))
           (eq w (get-buffer-window (window-buffer w))))",
    );
    assert_eq!(result, "OK t");
}

#[test]
fn get_buffer_window_list_returns_matching_windows() {
    let result = bootstrap_eval_with_frame("(length (get-buffer-window-list (window-buffer)))");
    assert_eq!(result[0], "OK 1");
}

#[test]
fn get_buffer_window_and_list_match_optional_and_missing_buffer_semantics() {
    let results = bootstrap_eval_with_frame(
        "(let ((vm-gbwl-live (generate-new-buffer \"gbwl-live\"))
               (vm-gbwl-dead (generate-new-buffer \"gbwl-dead\")))
           (list
            (condition-case err (get-buffer-window) (error err))
            (condition-case err (get-buffer-window nil) (error err))
            (condition-case err (get-buffer-window \"missing\") (error err))
            (windowp (get-buffer-window \"*scratch*\"))
            (length (get-buffer-window-list))
            (length (get-buffer-window-list nil))
            (length (get-buffer-window-list \"*scratch*\"))
            (condition-case err (get-buffer-window-list \"missing\") (error err))
            (condition-case err (get-buffer-window-list 1) (error err))
            (prog1 (condition-case err (get-buffer-window-list vm-gbwl-live) (error err))
              (kill-buffer vm-gbwl-live))
            (progn
              (kill-buffer vm-gbwl-dead)
              (condition-case err (get-buffer-window-list vm-gbwl-dead) (error err)))))",
    );
    assert_eq!(
        results[0],
        "OK (nil nil nil t 1 1 1 (error \"No such live buffer missing\") (error \"No such buffer 1\") nil (error \"No such live buffer #<killed buffer>\"))"
    );
}

#[test]
fn fit_window_to_buffer_returns_nil_in_batch_mode() {
    let result = bootstrap_eval_with_frame("(fit-window-to-buffer)");
    assert_eq!(result[0], "OK nil");
}

#[test]
fn fit_window_to_buffer_invalid_window_designators_signal_error() {
    let results = bootstrap_eval_with_frame(
        "(condition-case err (fit-window-to-buffer 999999) (error (car err)))
         (condition-case err (fit-window-to-buffer 'foo) (error (car err)))",
    );
    assert_eq!(results[0], "OK error");
    assert_eq!(results[1], "OK error");
}

#[test]
fn window_list_1_callable_paths_return_live_windows() {
    let r = eval_one_with_frame(
        "(let* ((fn (indirect-function 'window-list-1))
                (a (funcall #'window-list-1 nil nil))
                (b (apply #'window-list-1 '(nil nil)))
                (c (funcall fn nil nil)))
           (list (listp a)
                 (consp a)
                 (equal a b)
                 (equal a c)
                 (null (memq nil (mapcar #'windowp a)))))",
    );
    assert_eq!(r, "OK (t t t t t)");
}

#[test]
fn window_list_1_stale_window_signals_wrong_type_argument() {
    let r = eval_one_with_frame(
        "(let ((w (split-window-internal (selected-window) nil nil nil)))
           (delete-window w)
           (list (condition-case err (window-list-1 w nil) (error (car err)))
                 (condition-case err (funcall #'window-list-1 w nil) (error (car err)))
                 (condition-case err (apply #'window-list-1 (list w nil)) (error (car err)))))",
    );
    assert_eq!(
        r,
        "OK (wrong-type-argument wrong-type-argument wrong-type-argument)"
    );
}

#[test]
fn window_list_1_all_frames_includes_other_frame_windows() {
    let r = eval_one_with_frame(
        "(let ((f1 (selected-frame))
              (f2 (make-frame)))
           (let ((w1 (progn (select-frame f1) (selected-window)))
                 (w2 (progn (select-frame f2) (selected-window))))
             (prog1
                 (list (null (memq w2 (window-list-1 w1 nil nil)))
                       (not (null (memq w2 (window-list-1 w1 nil t))))
                       (not (null (memq w2 (window-list-1 w1 nil 'visible))))
                       (not (null (memq w2 (window-list-1 w1 nil 0))))
                       (not (null (memq w2 (window-list-1 w1 nil f2))))
                       (null (memq w2 (window-list-1 w1 nil :bad))))
               (select-frame f1)
               (delete-frame f2))))",
    );
    assert_eq!(r, "OK (t t t t t t)");
}

#[test]
fn window_list_returns_list() {
    let r = eval_one_with_frame("(listp (window-list))");
    assert_eq!(r, "OK t");
}

#[test]
fn window_list_has_one_entry() {
    let r = eval_one_with_frame("(length (window-list))");
    assert_eq!(r, "OK 1");
}

#[test]
fn window_list_matches_frame_minibuffer_and_all_frames_batch_semantics() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(condition-case err (length (window-list)) (error err))
         (condition-case err (length (window-list (selected-frame))) (error err))
         (condition-case err (window-list 999999) (error err))
         (condition-case err (window-list 'foo) (error err))
         (condition-case err (window-list (selected-window)) (error err))
         (condition-case err (window-list 999999 nil t) (error err))
         (condition-case err (window-list nil nil t) (error err))
         (condition-case err (window-list nil nil 0) (error err))
         (length (window-list nil t))
         (length (window-list (selected-frame) t))
         (length (window-list nil nil (selected-window)))
         (length (window-list nil t (selected-window)))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK 1");
    assert_eq!(out[1], "OK 1");
    assert_eq!(out[2], "OK (error \"Window is on a different frame\")");
    assert_eq!(out[3], "OK (error \"Window is on a different frame\")");
    assert_eq!(out[4], "OK (error \"Window is on a different frame\")");
    assert_eq!(out[5], "OK (wrong-type-argument windowp t)");
    assert_eq!(out[6], "OK (wrong-type-argument windowp t)");
    assert_eq!(out[7], "OK (wrong-type-argument windowp 0)");
    assert_eq!(out[8], "OK 2");
    assert_eq!(out[9], "OK 2");
    assert_eq!(out[10], "OK 1");
    assert_eq!(out[11], "OK 2");
}

#[test]
fn minibuffer_window_from_window_list_supports_basic_accessors() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(let ((m (car (nthcdr (1- (length (window-list nil t))) (window-list nil t)))))
           (list (window-live-p m)
                 (windowp m)
                 (buffer-name (window-buffer m))
                 (window-start m)
                 (window-point m)
                 (window-body-height m)
                 (window-body-height m t)))
         (let ((m (car (nthcdr (1- (length (window-list nil t))) (window-list nil t)))))
           (set-window-start m 7)
           (window-start m))
         (let ((m (car (nthcdr (1- (length (window-list nil t))) (window-list nil t)))))
           (set-window-point m 8)
           (window-point m))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK (t t \" *Minibuf-0*\" 1 1 1 1)");
    assert_eq!(out[1], "OK 1");
    assert_eq!(out[2], "OK 1");
}

#[test]
fn window_dedicated_p_default() {
    let r = eval_one_with_frame("(window-dedicated-p)");
    assert_eq!(r, "OK nil");
}

#[test]
fn window_accessors_enforce_max_arity() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(condition-case err (window-buffer nil nil) (error (car err)))
         (condition-case err (window-start nil nil) (error (car err)))
         (condition-case err (window-end nil nil nil) (error (car err)))
         (condition-case err (window-point nil nil) (error (car err)))
         (condition-case err (window-dedicated-p nil nil) (error (car err)))
         (condition-case err (set-window-start nil 1 nil nil) (error (car err)))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK wrong-number-of-arguments");
    assert_eq!(out[1], "OK wrong-number-of-arguments");
    assert_eq!(out[2], "OK wrong-number-of-arguments");
    assert_eq!(out[3], "OK wrong-number-of-arguments");
    assert_eq!(out[4], "OK wrong-number-of-arguments");
    assert_eq!(out[5], "OK wrong-number-of-arguments");
}

#[test]
fn set_window_dedicated_p() {
    let results = eval_with_frame(
        "(set-window-dedicated-p (selected-window) t)
         (window-dedicated-p)",
    );
    assert_eq!(results[0], "OK t");
    assert_eq!(results[1], "OK t");
}

#[test]
fn set_window_dedicated_p_bootstraps_nil_and_validates_designators() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(condition-case err (set-window-dedicated-p nil t) (error err))
         (window-dedicated-p nil)
         (condition-case err (set-window-dedicated-p 'foo t) (error err))
         (condition-case err (set-window-dedicated-p 999999 t) (error err))
         (condition-case err (set-window-dedicated-p nil nil) (error err))
         (window-dedicated-p nil)",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK t");
    assert_eq!(out[1], "OK t");
    assert_eq!(out[2], "OK (wrong-type-argument window-live-p foo)");
    assert_eq!(out[3], "OK (wrong-type-argument window-live-p 999999)");
    assert_eq!(out[4], "OK nil");
    assert_eq!(out[5], "OK nil");
}

// -- Window manipulation --

#[test]
fn split_window_internal_creates_new() {
    let results = eval_with_frame(
        "(split-window-internal (selected-window) nil nil nil)
         (length (window-list))",
    );
    assert!(results[0].starts_with("OK "));
    assert_eq!(results[1], "OK 2");
}

#[test]
fn split_window_internal_enforces_arity() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(condition-case err
             (split-window-internal (selected-window) nil nil nil nil nil)
           (error (car err)))
         (let ((w (split-window-internal (selected-window) nil nil nil)))
           (window-live-p w))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK wrong-number-of-arguments");
    assert_eq!(out[1], "OK t");
}

#[test]
fn split_delete_window_invalid_designators_signal_error() {
    let results = eval_with_frame(
        "(condition-case err
             (split-window-internal 999999 nil nil nil)
           (error (car err)))
         (condition-case err
             (split-window-internal 'foo nil nil nil)
           (error (car err)))
         (condition-case err (delete-window 999999) (error (car err)))
         (condition-case err (delete-window 'foo) (error (car err)))
         (condition-case err (delete-other-windows 999999) (error (car err)))
         (condition-case err (delete-other-windows 'foo) (error (car err)))",
    );
    assert_eq!(results[0], "OK error");
    assert_eq!(results[1], "OK error");
    assert_eq!(results[2], "OK error");
    assert_eq!(results[3], "OK error");
    assert_eq!(results[4], "OK error");
    assert_eq!(results[5], "OK error");
}

#[test]
fn delete_window_after_split() {
    let results = bootstrap_eval_with_frame(
        "(let ((new-win (split-window-internal (selected-window) nil nil nil)))
           (delete-window new-win)
           (length (window-list)))",
    );
    assert_eq!(results[0], "OK 1");
}

#[test]
fn delete_window_updates_current_buffer_to_selected_window_buffer() {
    let result = bootstrap_eval_one_with_frame(
        "(save-current-buffer
           (let* ((b1 (get-buffer-create \"dw-curbuf-a\"))
                  (b2 (get-buffer-create \"dw-curbuf-b\")))
             (set-window-buffer nil b1)
             (let ((w2 (split-window-internal (selected-window) nil nil nil)))
               (set-window-buffer w2 b2)
               (select-window w2)
               (delete-window w2)
               (buffer-name (current-buffer)))))",
    );
    assert_eq!(result, "OK \"dw-curbuf-a\"");
}

#[test]
fn delete_sole_window_errors() {
    let r = bootstrap_eval_one_with_frame("(delete-window)");
    assert!(r.contains("ERR"), "deleting sole window should error: {r}");
}

#[test]
fn delete_window_and_delete_other_windows_enforce_max_arity() {
    let out = bootstrap_eval_with_frame(
        "(condition-case err (delete-window nil nil) (error (car err)))
         (condition-case err (delete-other-windows nil nil nil) (error (car err)))
         (condition-case err
             (let ((w2 (split-window-internal (selected-window) nil nil nil)))
               (delete-other-windows w2 nil))
           (error err))",
    );
    assert_eq!(out[0], "OK wrong-number-of-arguments");
    assert_eq!(out[1], "OK wrong-number-of-arguments");
    assert_eq!(out[2], "OK nil");
}

#[test]
fn delete_other_windows_keeps_one() {
    let results = bootstrap_eval_with_frame(
        "(split-window-internal (selected-window) nil nil nil)
         (split-window-internal (selected-window) nil nil nil)
         (delete-other-windows)
         (length (window-list))",
    );
    assert_eq!(results[3], "OK 1");
}

#[test]
fn delete_other_windows_updates_current_buffer_when_kept_window_differs() {
    let result = bootstrap_eval_one_with_frame(
        "(save-current-buffer
           (let* ((b1 (get-buffer-create \"dow-curbuf-a\"))
                  (b2 (get-buffer-create \"dow-curbuf-b\")))
             (set-window-buffer nil b1)
             (let ((w2 (split-window-internal (selected-window) nil nil nil))
                   (w1 (selected-window)))
               (set-window-buffer w2 b2)
               (select-window w2)
               (delete-other-windows w1)
               (buffer-name (current-buffer)))))",
    );
    assert_eq!(result, "OK \"dow-curbuf-a\"");
}

#[test]
fn select_window_works() {
    let results = eval_with_frame(
        "(let ((new-win (split-window-internal (selected-window) nil nil nil)))
           (select-window new-win)
           (eq (selected-window) new-win))",
    );
    assert_eq!(results[0], "OK t");
}

#[test]
fn select_window_validates_designators_and_arity() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(condition-case err (select-window nil) (error err))
         (condition-case err (select-window 'foo) (error err))
         (condition-case err (select-window 999999) (error err))
         (windowp (select-window (selected-window)))
         (condition-case err (select-window (selected-window) nil nil) (error (car err)))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK (wrong-type-argument window-live-p nil)");
    assert_eq!(out[1], "OK (wrong-type-argument window-live-p foo)");
    assert_eq!(out[2], "OK (wrong-type-argument window-live-p 999999)");
    assert_eq!(out[3], "OK t");
    assert_eq!(out[4], "OK wrong-number-of-arguments");
}

#[test]
fn select_window_updates_current_buffer_to_selected_window_buffer() {
    let result = eval_one_with_frame(
        "(save-current-buffer
           (let* ((b1 (get-buffer-create \"sw-curbuf-a\"))
                  (b2 (get-buffer-create \"sw-curbuf-b\")))
             (set-window-buffer nil b1)
             (let ((w2 (split-window-internal (selected-window) nil nil nil)))
               (set-window-buffer w2 b2)
               (select-window w2)
               (buffer-name (current-buffer)))))",
    );
    assert_eq!(result, "OK \"sw-curbuf-b\"");
}

#[test]
fn other_window_cycles() {
    let results = bootstrap_eval_with_frame(
        "(let ((w1 (selected-window)))
           (split-window-internal (selected-window) nil nil nil)
           (other-window 1)
           (not (eq (selected-window) w1)))",
    );
    assert_eq!(results[0], "OK t");
}

#[test]
fn other_window_updates_current_buffer_to_selected_window_buffer() {
    let result = bootstrap_eval_one_with_frame(
        "(save-current-buffer
           (let* ((b1 (get-buffer-create \"ow-curbuf-a\"))
                  (b2 (get-buffer-create \"ow-curbuf-b\")))
             (set-window-buffer nil b1)
             (let ((w2 (split-window-internal (selected-window) nil nil nil)))
               (set-window-buffer w2 b2)
               (other-window 1)
               (buffer-name (current-buffer)))))",
    );
    assert_eq!(result, "OK \"ow-curbuf-b\"");
}

#[test]
fn other_window_requires_count_and_enforces_number_or_marker_p() {
    let out = bootstrap_eval_with_frame(
        "(condition-case err (other-window) (error (car err)))
         (condition-case err (other-window nil) (error err))
         (condition-case err (other-window \"x\") (error err))",
    );
    assert_eq!(out[0], "OK wrong-number-of-arguments");
    assert_eq!(out[1], "OK (wrong-type-argument number-or-marker-p nil)");
    assert_eq!(out[2], "OK (wrong-type-argument number-or-marker-p \"x\")");
}

#[test]
fn other_window_accepts_float_counts_with_floor_semantics() {
    let results = bootstrap_eval_with_frame(
        "(let* ((w1 (progn (delete-other-windows) (selected-window)))
                (w2 (split-window-internal (selected-window) nil nil nil)))
           (list
             (progn (other-window 1.5) (eq (selected-window) w2))
             (progn (select-window w1) (other-window 0.4) (eq (selected-window) w1))
             (progn (select-window w1) (other-window -0.4) (eq (selected-window) w2))
             (progn (select-window w1) (other-window -1.2) (eq (selected-window) w1))))",
    );
    assert_eq!(results[0], "OK (t t t t)");
}

#[test]
fn other_window_enforces_max_arity() {
    let out = bootstrap_eval_with_frame(
        "(condition-case err (other-window 1 nil nil nil) (error (car err)))
         (condition-case err
             (let ((w1 (selected-window)))
               (split-window-internal (selected-window) nil nil nil)
               (other-window 1 nil nil)
               (not (eq (selected-window) w1)))
           (error err))",
    );
    assert_eq!(out[0], "OK wrong-number-of-arguments");
    assert_eq!(out[1], "OK t");
}

#[test]
fn other_window_without_selected_frame_returns_nil() {
    let mut ev = Evaluator::new();
    let forms = parse_forms("(other-window 1)").expect("parse");
    let results = ev.eval_forms(&forms);
    assert_eq!(format_eval_result(&results[0]), "OK nil");
}

#[test]
fn selected_frame_bootstraps_initial_frame() {
    let mut ev = Evaluator::new();
    let forms =
        parse_forms("(list (framep (selected-frame)) (length (frame-list)))").expect("parse");
    let results = ev.eval_forms(&forms);
    assert_eq!(format_eval_result(&results[0]), "OK (t 1)");
}

#[test]
fn window_size_queries_bootstrap_initial_frame() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(list (integerp (window-height))
               (integerp (window-width))
               (integerp (window-body-height))
               (integerp (window-body-width)))",
    )
    .expect("parse");
    let results = ev.eval_forms(&forms);
    assert_eq!(format_eval_result(&results[0]), "OK (t t t t)");
}

#[test]
fn window_size_queries_match_batch_defaults_and_invalid_window_predicates() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(window-height nil)
         (window-width nil)
         (window-body-height nil)
         (window-body-width nil)
         (window-total-height nil)
         (window-total-width nil)
         (condition-case err (window-height 999999) (error err))
         (condition-case err (window-width 999999) (error err))
         (condition-case err (window-body-height 999999) (error err))
         (condition-case err (window-body-width 999999) (error err))
         (condition-case err (window-total-height 999999) (error err))
         (condition-case err (window-total-width 999999) (error err))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK 24");
    assert_eq!(out[1], "OK 80");
    assert_eq!(out[2], "OK 23");
    assert_eq!(out[3], "OK 80");
    assert_eq!(out[4], "OK 24");
    assert_eq!(out[5], "OK 80");
    assert_eq!(out[6], "OK (wrong-type-argument window-valid-p 999999)");
    assert_eq!(out[7], "OK (wrong-type-argument window-live-p 999999)");
    assert_eq!(out[8], "OK (wrong-type-argument window-live-p 999999)");
    assert_eq!(out[9], "OK (wrong-type-argument window-live-p 999999)");
    assert_eq!(out[10], "OK (wrong-type-argument window-valid-p 999999)");
    assert_eq!(out[11], "OK (wrong-type-argument window-valid-p 999999)");
}

#[test]
fn window_geometry_helper_queries_match_batch_defaults_and_error_predicates() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(let* ((w (selected-window))
                (m (minibuffer-window)))
           (list (window-left-column w)
                 (window-left-column m)
                 (window-top-line w)
                 (window-top-line m)
                 (window-hscroll w)
                 (window-hscroll m)
                 (window-margins w)
                 (window-margins m)
                 (window-fringes w)
                 (window-fringes m)
                 (window-scroll-bars w)
                 (window-scroll-bars m)))
         (list (condition-case err (window-left-column 999999) (error err))
               (condition-case err (window-top-line 999999) (error err))
               (condition-case err (window-hscroll 999999) (error err))
               (condition-case err (window-margins 999999) (error err))
               (condition-case err (window-fringes 999999) (error err))
               (condition-case err (window-scroll-bars 999999) (error err))
               (condition-case err (window-left-column nil nil) (error err))
               (condition-case err (window-top-line nil nil) (error err))
               (condition-case err (window-hscroll nil nil) (error err))
               (condition-case err (window-margins nil nil) (error err))
               (condition-case err (window-fringes nil nil) (error err))
               (condition-case err (window-scroll-bars nil nil) (error err)))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(
        out[0],
        "OK (0 0 0 24 0 0 (nil) (nil) (0 0 nil nil) (0 0 nil nil) (nil 0 t nil 0 t nil) (nil 0 t nil 0 t nil))"
    );
    assert_eq!(
        out[1],
        "OK ((wrong-type-argument window-valid-p 999999) (wrong-type-argument window-valid-p 999999) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p 999999) (wrong-number-of-arguments window-left-column 2) (wrong-number-of-arguments window-top-line 2) (wrong-number-of-arguments window-hscroll 2) (wrong-number-of-arguments window-margins 2) (wrong-number-of-arguments window-fringes 2) (wrong-number-of-arguments window-scroll-bars 2))"
    );
}

#[test]
fn window_use_time_and_old_state_queries_match_batch_defaults_and_error_predicates() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(let* ((w (selected-window))
                (m (minibuffer-window)))
           (list (window-use-time w)
                 (window-use-time m)
                 (window-old-point w)
                 (window-old-point m)
                 (window-old-buffer w)
                 (window-old-buffer m)
                 (window-prev-buffers w)
                 (window-prev-buffers m)
                 (window-next-buffers w)
                 (window-next-buffers m)))
         (let* ((w1 (selected-window))
                (w2 (split-window-internal (selected-window) nil nil nil))
                (m (minibuffer-window)))
           (list (window-use-time w1)
                 (window-use-time w2)
                 (window-use-time m)
                 (window-old-point w1)
                 (window-old-point w2)
                 (window-old-point m)
                 (window-old-buffer w1)
                 (window-old-buffer w2)
                 (window-prev-buffers w1)
                 (window-prev-buffers w2)
                 (window-next-buffers w1)
                 (window-next-buffers w2)))
         (list (condition-case err (window-use-time 999999) (error err))
               (condition-case err (window-old-point 999999) (error err))
               (condition-case err (window-old-buffer 999999) (error err))
               (condition-case err (window-prev-buffers 999999) (error err))
               (condition-case err (window-next-buffers 999999) (error err))
               (condition-case err (window-use-time nil nil) (error err))
               (condition-case err (window-old-point nil nil) (error err))
               (condition-case err (window-old-buffer nil nil) (error err))
               (condition-case err (window-prev-buffers nil nil) (error err))
               (condition-case err (window-next-buffers nil nil) (error err)))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK (1 0 1 1 nil nil nil nil nil nil)");
    assert_eq!(out[1], "OK (1 0 0 1 1 1 nil nil nil nil nil nil)");
    assert_eq!(
        out[2],
        "OK ((wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p 999999) (wrong-number-of-arguments window-use-time 2) (wrong-number-of-arguments window-old-point 2) (wrong-number-of-arguments window-old-buffer 2) (wrong-number-of-arguments window-prev-buffers 2) (wrong-number-of-arguments window-next-buffers 2))"
    );
}

#[test]
fn window_bump_use_time_tracks_second_most_recent_window() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(let* ((w1 (selected-window))
                (w2 (split-window-internal (selected-window) nil nil nil)))
           (list (window-use-time w1)
                 (window-use-time w2)
                 (window-bump-use-time w2)
                 (window-use-time w1)
                 (window-use-time w2)
                 (window-bump-use-time w1)))
         (list (condition-case err (window-bump-use-time 1) (error err))
               (condition-case err (window-bump-use-time nil nil) (error err))
               (let ((w (split-window-internal (selected-window) nil nil nil)))
                 (delete-window w)
                 (condition-case err (window-bump-use-time w) (error (car err)))))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK (1 0 1 2 1 nil)");
    assert_eq!(
        out[1],
        "OK ((wrong-type-argument window-live-p 1) (wrong-number-of-arguments window-bump-use-time 2) wrong-type-argument)"
    );
}

#[test]
fn window_bump_use_time_shared_state_smoke() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(let* ((w1 (selected-window))
                (w2 (split-window-internal (selected-window) nil nil nil)))
           (list (window-use-time w1)
                 (window-use-time w2)
                 (window-bump-use-time w2)
                 (window-use-time w1)
                 (window-use-time w2)
                 (window-bump-use-time w1)))
         (list (condition-case err (window-bump-use-time 1) (error err))
               (condition-case err (window-bump-use-time nil nil) (error err)))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK (1 0 1 2 1 nil)");
    assert_eq!(
        out[1],
        "OK ((wrong-type-argument window-live-p 1) (wrong-number-of-arguments window-bump-use-time 2))"
    );
}

#[test]
fn window_vscroll_helpers_match_batch_defaults_and_error_predicates() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(let* ((w (selected-window))
                (m (minibuffer-window)))
           (list (window-vscroll w)
                 (window-vscroll m)
                 (window-vscroll w t)
                 (window-vscroll m t)
                 (set-window-vscroll w 1)
                 (set-window-vscroll w 2 t)
                 (set-window-vscroll w 3 t t)
                 (set-window-vscroll nil 1.5)
                 (window-vscroll w)
                 (window-vscroll w t)))
         (list (condition-case err (window-vscroll 999999) (error err))
               (condition-case err (window-vscroll 'foo) (error err))
               (condition-case err (set-window-vscroll 999999 1) (error err))
               (condition-case err (set-window-vscroll 'foo 1) (error err))
               (condition-case err (set-window-vscroll nil 'foo) (error err))
               (condition-case err (window-vscroll nil nil nil) (error err))
               (condition-case err (set-window-vscroll nil 1 nil nil nil) (error err)))
         (let ((w (split-window-internal (selected-window) nil nil nil)))
           (delete-window w)
           (list (condition-case err (window-vscroll w) (error (car err)))
                 (condition-case err (set-window-vscroll w 1) (error (car err)))))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK (0 0 0 0 0 0 0 0 0 0)");
    assert_eq!(
        out[1],
        "OK ((wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p foo) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p foo) (wrong-type-argument numberp foo) (wrong-number-of-arguments window-vscroll 3) (wrong-number-of-arguments set-window-vscroll 5))"
    );
    assert_eq!(out[2], "OK (wrong-type-argument wrong-type-argument)");
}

#[test]
fn window_scroll_state_shared_state_smoke() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(let* ((w (selected-window))
                (m (minibuffer-window)))
           (list (window-vscroll w)
                 (window-vscroll m)
                 (window-vscroll w t)
                 (window-vscroll m t)
                 (set-window-vscroll w 1)
                 (set-window-vscroll w 2 t)
                 (set-window-vscroll w 3 t t)
                 (set-window-vscroll nil 1.5)
                 (window-vscroll w)
                 (window-vscroll w t)
                 (window-hscroll w)
                 (set-window-hscroll w 3)
                 (window-hscroll w)
                 (set-window-hscroll w -1)
                 (window-hscroll w)
                 (set-window-hscroll w ?a)
                 (window-hscroll w)
                 (window-margins w)
                 (set-window-margins w 1 2)
                 (window-margins w)
                 (set-window-margins w 1 2)
                 (set-window-margins w nil nil)
                 (window-margins w)
                 (set-window-margins w 3)
                 (window-margins w)
                 (set-window-margins w 3)
                 (window-fringes w)
                 (window-fringes m)
                 (set-window-fringes w 0 0)
                 (set-window-fringes w 1 2)
                 (set-window-fringes w nil nil)
                 (window-fringes w)
                 (window-scroll-bars w)
                 (window-scroll-bars m)
                 (set-window-scroll-bars w nil nil nil nil)
                 (set-window-scroll-bars w 'left)
                 (window-scroll-bars w)))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(
        out[0],
        "OK (0 0 0 0 0 0 0 0 0 0 0 3 3 0 0 97 97 (nil) t (1 . 2) nil t (nil) t (3) nil (0 0 nil nil) (0 0 nil nil) nil nil nil (0 0 nil nil) (nil 0 t nil 0 t nil) (nil 0 t nil 0 t nil) nil nil (nil 0 t nil 0 t nil))"
    );
}

#[test]
fn window_hscroll_and_margin_setters_match_batch_defaults_and_error_predicates() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(let* ((w (selected-window))
                (m (minibuffer-window)))
           (list (window-hscroll w)
                 (set-window-hscroll w 3)
                 (window-hscroll w)
                 (set-window-hscroll w -1)
                 (window-hscroll w)
                 (set-window-hscroll w ?a)
                 (window-hscroll w)
                 (window-margins w)
                 (set-window-margins w 1 2)
                 (window-margins w)
                 (set-window-margins w 1 2)
                 (set-window-margins w nil nil)
                 (window-margins w)
                 (set-window-margins w 3)
                 (window-margins w)
                 (set-window-margins w 3)
                 (window-hscroll m)
                 (set-window-hscroll m 4)
                 (window-hscroll m)
                 (window-margins m)
                 (set-window-margins m 4 5)
                 (window-margins m)))
         (list (condition-case err (set-window-hscroll nil 1.5) (error err))
               (condition-case err (set-window-hscroll nil 'foo) (error err))
               (condition-case err (set-window-hscroll 999999 1) (error err))
               (condition-case err (set-window-hscroll 'foo 1) (error err))
               (condition-case err (set-window-hscroll nil) (error err))
               (condition-case err (set-window-hscroll nil 1 nil) (error err))
               (condition-case err (set-window-margins nil -1 0) (error err))
               (condition-case err (set-window-margins nil 1 -2) (error err))
               (condition-case err (set-window-margins nil 1.5 0) (error err))
               (condition-case err (set-window-margins nil 'foo 0) (error err))
               (condition-case err (set-window-margins nil 1 'foo) (error err))
               (condition-case err (set-window-margins 999999 1 2) (error err))
               (condition-case err (set-window-margins 'foo 1 2) (error err))
               (condition-case err (set-window-margins nil) (error err))
               (condition-case err (set-window-margins nil 1 2 3) (error err)))
         (let ((w (split-window-internal (selected-window) nil nil nil)))
           (delete-window w)
           (list (condition-case err (set-window-hscroll w 1) (error (car err)))
                 (condition-case err (set-window-margins w 1 2) (error (car err)))))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(
        out[0],
        "OK (0 3 3 0 0 97 97 (nil) t (1 . 2) nil t (nil) t (3) nil 0 4 4 (nil) t (4 . 5))"
    );
    assert_eq!(
        out[1],
        "OK ((wrong-type-argument fixnump 1.5) (wrong-type-argument fixnump foo) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p foo) (wrong-number-of-arguments set-window-hscroll 1) (wrong-number-of-arguments set-window-hscroll 3) (args-out-of-range -1 0 2147483647) (args-out-of-range -2 0 2147483647) (wrong-type-argument integerp 1.5) (wrong-type-argument integerp foo) (wrong-type-argument integerp foo) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p foo) (wrong-number-of-arguments set-window-margins 1) (wrong-number-of-arguments set-window-margins 4))"
    );
    assert_eq!(out[2], "OK (wrong-type-argument wrong-type-argument)");
}

#[test]
fn window_fringes_and_scroll_bar_setters_match_batch_defaults_and_error_predicates() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(let* ((w (selected-window))
                (m (minibuffer-window)))
           (list (window-fringes w)
                 (window-fringes m)
                 (set-window-fringes w 0 0)
                 (set-window-fringes w 1 2)
                 (set-window-fringes w nil nil)
                 (window-fringes w)
                 (window-fringes m)
                 (window-scroll-bars w)
                 (window-scroll-bars m)
                 (set-window-scroll-bars w nil nil nil nil)
                 (set-window-scroll-bars w 'left)
                 (window-scroll-bars w)
                 (window-scroll-bars m)
                 (set-window-fringes m 0 0)
                 (set-window-scroll-bars m nil)
                 (window-fringes m)
                 (window-scroll-bars m)))
         (list (condition-case err (set-window-fringes nil 1 2 nil nil nil) (error err))
               (condition-case err (set-window-scroll-bars nil nil nil nil nil nil nil) (error err))
               (condition-case err (set-window-fringes 999999 0 0) (error err))
               (condition-case err (set-window-fringes 'foo 0 0) (error err))
               (condition-case err (set-window-scroll-bars 999999 nil) (error err))
               (condition-case err (set-window-scroll-bars 'foo nil) (error err))
               (condition-case err (set-window-fringes nil) (error err))
               (condition-case err (set-window-scroll-bars) (error err)))
         (let ((w (split-window-internal (selected-window) nil nil nil)))
           (delete-window w)
           (list (condition-case err (set-window-fringes w 0 0) (error (car err)))
                 (condition-case err (set-window-scroll-bars w nil) (error (car err)))))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(
        out[0],
        "OK ((0 0 nil nil) (0 0 nil nil) nil nil nil (0 0 nil nil) (0 0 nil nil) (nil 0 t nil 0 t nil) (nil 0 t nil 0 t nil) nil nil (nil 0 t nil 0 t nil) (nil 0 t nil 0 t nil) nil nil (0 0 nil nil) (nil 0 t nil 0 t nil))"
    );
    assert_eq!(
        out[1],
        "OK ((wrong-number-of-arguments set-window-fringes 6) (wrong-number-of-arguments set-window-scroll-bars 7) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p foo) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p foo) (wrong-number-of-arguments set-window-fringes 1) (wrong-number-of-arguments set-window-scroll-bars 0))"
    );
    assert_eq!(out[2], "OK (wrong-type-argument wrong-type-argument)");
}

#[test]
fn window_parameter_helpers_match_batch_defaults_and_key_semantics() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(let* ((w (selected-window))
                (m (minibuffer-window)))
           (list (window-parameters w)
                 (window-parameters m)
                 (window-parameter w 'foo)
                 (window-parameter m 'foo)
                 (set-window-parameter w 'foo 'bar)
                 (window-parameter w 'foo)
                 (window-parameters w)
                 (set-window-parameter m 'foo 42)
                 (window-parameter m 'foo)
                 (window-parameters m)
                 (set-window-parameter w 'foo nil)
                 (window-parameter w 'foo)
                 (window-parameters w)
                 (set-window-parameter w 1 2)
                 (window-parameter w 1)
                 (window-parameters w)))
         (list (condition-case err (window-parameter 999999 'foo) (error err))
               (condition-case err (set-window-parameter 999999 'foo 'bar) (error err))
               (condition-case err (window-parameters 999999) (error err))
               (condition-case err (window-parameter nil) (error err))
               (condition-case err (window-parameter nil nil nil) (error err))
               (condition-case err (set-window-parameter nil nil) (error err))
               (condition-case err (set-window-parameter nil nil nil nil) (error err))
               (condition-case err (window-parameters nil nil) (error err))
               (condition-case err (window-parameter 'foo 'bar) (error err))
               (condition-case err (set-window-parameter 'foo 'bar 'baz) (error err)))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(
        out[0],
        "OK (nil nil nil nil bar bar ((foo . bar)) 42 42 ((foo . 42)) nil nil ((foo)) 2 2 ((1 . 2) (foo)))"
    );
    assert_eq!(
        out[1],
        "OK ((wrong-type-argument windowp 999999) (wrong-type-argument windowp 999999) (wrong-type-argument window-valid-p 999999) (wrong-number-of-arguments window-parameter 1) (wrong-number-of-arguments window-parameter 3) (wrong-number-of-arguments set-window-parameter 2) (wrong-number-of-arguments set-window-parameter 4) (wrong-number-of-arguments window-parameters 2) (wrong-type-argument windowp foo) (wrong-type-argument windowp foo))"
    );
}

#[test]
fn window_display_table_helpers_match_batch_defaults_and_set_get_semantics() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(let* ((w (selected-window))
                (m (minibuffer-window))
                (dt '(1 2 3)))
           (list (null (window-display-table w))
                 (null (window-display-table m))
                 (let ((rv (set-window-display-table w dt))) (equal rv dt))
                 (equal (window-display-table w) dt)
                 (null (set-window-display-table w nil))
                 (null (window-display-table w))
                 (let ((rv (set-window-display-table m dt))) (equal rv dt))
                 (equal (window-display-table m) dt)
                 (eq (set-window-display-table m 'foo) 'foo)
                 (eq (window-display-table m) 'foo)
                 (null (set-window-display-table m nil))
                 (null (window-display-table m))))
         (list (condition-case err (window-display-table nil nil) (error err))
               (condition-case err (set-window-display-table nil nil nil) (error err))
               (condition-case err (window-display-table 999999) (error err))
               (condition-case err (set-window-display-table 999999 nil) (error err))
               (condition-case err (window-display-table 'foo) (error err))
               (condition-case err (set-window-display-table 'foo nil) (error err)))
         (let ((w (split-window-internal (selected-window) nil nil nil)))
           (delete-window w)
           (list (condition-case err (window-display-table w) (error (car err)))
                 (condition-case err (set-window-display-table w nil) (error (car err)))))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK (t t t t t t t t t t t t)");
    assert_eq!(
        out[1],
        "OK ((wrong-number-of-arguments window-display-table 2) (wrong-number-of-arguments set-window-display-table 3) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p foo) (wrong-type-argument window-live-p foo))"
    );
    assert_eq!(out[2], "OK (wrong-type-argument wrong-type-argument)");
}

#[test]
fn window_cursor_type_helpers_match_batch_defaults_and_set_get_semantics() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(let* ((w (selected-window))
                (m (minibuffer-window)))
           (list (window-cursor-type w)
                 (window-cursor-type m)
                 (set-window-cursor-type w nil)
                 (window-cursor-type w)
                 (set-window-cursor-type w 'bar)
                 (window-cursor-type w)
                 (set-window-cursor-type w t)
                 (window-cursor-type w)
                 (set-window-cursor-type m 'hbar)
                 (window-cursor-type m)
                 (set-window-cursor-type m nil)
                 (window-cursor-type m)))
         (list (condition-case err (window-cursor-type nil nil) (error err))
               (condition-case err (set-window-cursor-type nil) (error err))
               (condition-case err (set-window-cursor-type nil nil nil) (error err))
               (condition-case err (window-cursor-type 999999) (error err))
               (condition-case err (set-window-cursor-type 999999 nil) (error err))
               (condition-case err (window-cursor-type 'foo) (error err))
               (condition-case err (set-window-cursor-type 'foo nil) (error err)))
         (let ((w (split-window-internal (selected-window) nil nil nil)))
           (delete-window w)
           (list (condition-case err (window-cursor-type w) (error (car err)))
                 (condition-case err (set-window-cursor-type w nil) (error (car err)))))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK (t t nil nil bar bar t t hbar hbar nil nil)");
    assert_eq!(
        out[1],
        "OK ((wrong-number-of-arguments window-cursor-type 2) (wrong-number-of-arguments set-window-cursor-type 1) (wrong-number-of-arguments set-window-cursor-type 3) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p foo) (wrong-type-argument window-live-p foo))"
    );
    assert_eq!(out[2], "OK (wrong-type-argument wrong-type-argument)");
}

#[test]
fn window_metadata_shared_state_smoke() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(let* ((w (selected-window))
                (m (minibuffer-window))
                (dt '(1 2 3)))
           (list (window-dedicated-p w)
                 (set-window-dedicated-p w t)
                 (window-dedicated-p w)
                 (set-window-dedicated-p w nil)
                 (window-dedicated-p w)
                 (null (window-parameters w))
                 (set-window-parameter w 'foo 'bar)
                 (window-parameter w 'foo)
                 (equal (window-parameters w) '((foo . bar)))
                 (set-window-parameter w 'foo nil)
                 (equal (window-parameters w) '((foo)))
                 (null (window-display-table w))
                 (let ((rv (set-window-display-table w dt))) (equal rv dt))
                 (equal (window-display-table w) dt)
                 (null (set-window-display-table w nil))
                 (null (window-display-table w))
                 (window-cursor-type w)
                 (set-window-cursor-type w 'bar)
                 (window-cursor-type w)
                 (set-window-cursor-type w t)
                 (window-cursor-type w)
                 (set-window-cursor-type m nil)
                 (window-cursor-type m)))
         (list (condition-case err (window-parameter 999999 'foo) (error err))
               (condition-case err (set-window-parameter 999999 'foo 'bar) (error err))
               (condition-case err (window-display-table 999999) (error err))
               (condition-case err (set-window-display-table 999999 nil) (error err))
               (condition-case err (window-cursor-type 999999) (error err))
               (condition-case err (set-window-cursor-type 999999 nil) (error err))
               (condition-case err (set-window-dedicated-p 999999 t) (error err)))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(
        out[0],
        "OK (nil t t nil nil t bar bar t nil t t t t t t t bar bar t t nil nil)"
    );
    assert_eq!(
        out[1],
        "OK ((wrong-type-argument windowp 999999) (wrong-type-argument windowp 999999) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p 999999))"
    );
}

#[test]
fn window_preserve_size_fixed_and_resizable_helpers_match_batch_semantics() {
    let out = bootstrap_eval_with_frame(
        "(let ((w (selected-window)))
           (list (window-size-fixed-p w)
                 (window-size-fixed-p w t)
                 (let ((r (window-preserve-size w nil t)))
                   (list (bufferp (car r))
                         (nth 1 r)
                         (integerp (nth 2 r))))
                 (window-size-fixed-p w)
                 (window-size-fixed-p w t)
                 (let ((r (window-preserve-size w t t)))
                   (list (bufferp (car r))
                         (integerp (nth 1 r))
                         (integerp (nth 2 r))))
                 (window-size-fixed-p w)
                 (window-size-fixed-p w t)
                 (window-size-fixed-p w nil t)
                 (window-size-fixed-p w t t)
                 (progn
                   (window-preserve-size w nil nil)
                   (window-preserve-size w t nil)
                   (list (window-size-fixed-p w)
                         (window-size-fixed-p w t)))))
         (let ((w (split-window-internal (selected-window) nil 'right nil)))
           (split-window-internal w nil 'below nil)
           (window-preserve-size w t t)
           (let ((before (list (window-resizable w 100 t)
                               (window-resizable w -100 t)
                               (window-resizable w 100 nil)
                               (window-resizable w -100 nil)
                               (window-size-fixed-p w)
                               (window-size-fixed-p w t)
                               (window-resizable w 1 t)
                               (window-resizable w 1 t 'preserved)
                               (window-resizable w 1.5 t)
                               (window-resizable w -1.5 t))))
             (window-preserve-size w t nil)
             (list before
                   (window-size-fixed-p w t)
                   (window-resizable w 1 t)
                   (window-resizable w 1.5 t)
                   (window-resizable w -1.5 t))))
         (list (condition-case err (window-size-fixed-p 999999) (error (car err)))
               (condition-case err (window-preserve-size 999999 nil t) (error (car err)))
               (condition-case err (window-resizable 999999 1) (error (car err)))
               (condition-case err (window-resizable nil 'foo) (error (car err)))
               (condition-case err (window-size-fixed-p nil nil nil nil) (error err))
               (condition-case err (window-preserve-size nil nil nil nil) (error err))
               (condition-case err (window-resizable nil 1 nil nil nil nil) (error err)))",
    );
    assert_eq!(
        out[0],
        "OK (nil nil (t nil t) t nil (t t t) t t nil nil (nil nil))"
    );
    assert_eq!(out[1], "OK ((0 0 8 -8 nil t 0 1 0 0) nil 1 1.5 -1.5)");
    assert_eq!(
        out[2],
        "OK (error error error wrong-type-argument (wrong-number-of-arguments window-size-fixed-p 4) (wrong-number-of-arguments window-preserve-size 4) (wrong-number-of-arguments window-resizable 6))"
    );
}

#[test]
fn window_tree_navigation_and_normal_size_match_gnu_runtime() {
    let out = bootstrap_eval_with_frame(
        "(let* ((left (selected-window))
                (right (split-window nil nil 'right))
                (bottom (split-window right nil 'below))
                (root (frame-root-window))
                (vparent (window-parent right)))
           (list (window-valid-p root)
                 (window-live-p root)
                 (eq (window-parent left) root)
                 (eq (window-next-sibling left) vparent)
                 (eq (window-left-child root) left)
                 (window-top-child root)
                 (eq (window-parent right) vparent)
                 (eq (window-parent bottom) vparent)
                 (eq (window-top-child vparent) right)
                 (window-left-child vparent)
                 (eq (window-next-sibling right) bottom)
                 (eq (window-prev-sibling bottom) right)
                 (window-normal-size left)
                 (window-normal-size left t)
                 (window-normal-size right)
                 (window-normal-size right t)
                 (window-normal-size vparent)
                 (window-normal-size vparent t)))",
    );
    assert_eq!(
        out[0],
        "OK (t nil t t t nil t t t nil t t 1.0 0.5 0.5 1.0 1.0 0.5)"
    );
}

#[test]
fn window_geometry_queries_match_batch_alias_and_edge_shapes() {
    let out = bootstrap_eval_with_frame(
        "(list (symbol-function 'window-inside-pixel-edges)
               (symbol-function 'window-inside-edges))
         (let* ((w (selected-window))
                (m (minibuffer-window)))
           (list (window-mode-line-height w)
                 (window-mode-line-height m)
                 (window-header-line-height w)
                 (window-header-line-height m)
                 (window-pixel-height w)
                 (window-pixel-height m)
                 (window-pixel-width w)
                 (window-pixel-width m)
                 (window-text-height w)
                 (window-text-height m)
                 (window-text-height w t)
                 (window-text-height m t)
                 (window-text-width w)
                 (window-text-width m)
                 (window-text-width w t)
                 (window-text-width m t)
                 (window-body-pixel-edges w)
                 (window-body-pixel-edges m)
                 (window-pixel-edges w)
                 (window-pixel-edges m)
                 (window-body-edges w)
                 (window-body-edges m)
                 (window-edges w)
                 (window-edges m)
                 (window-edges w t)
                 (window-edges m t)))
         (list (condition-case err (window-mode-line-height 999999) (error err))
               (condition-case err (window-header-line-height 999999) (error err))
               (condition-case err (window-pixel-height 999999) (error err))
               (condition-case err (window-pixel-width 999999) (error err))
               (condition-case err (window-text-height 999999) (error err))
               (condition-case err (window-text-width 999999) (error err))
               (condition-case err (window-body-pixel-edges 999999) (error err))
               (condition-case err (window-pixel-edges 999999) (error err))
               (condition-case err (window-body-edges 999999) (error err))
               (condition-case err (window-edges 999999) (error err))
               (condition-case err (window-text-height nil nil nil) (error err))
               (condition-case err (window-mode-line-height nil nil) (error err))
               (condition-case err (window-inside-pixel-edges nil nil) (error (car err)))
               (condition-case err (window-edges nil nil nil nil) (error err))
               (condition-case err (window-edges nil nil nil nil nil) (error err)))",
    );
    assert_eq!(out[0], "OK (window-body-pixel-edges window-body-edges)");
    assert_eq!(
        out[1],
        "OK (1 0 0 0 24 1 80 80 23 1 23 1 80 80 80 80 (0 0 80 23) (0 24 80 25) (0 0 80 24) (0 24 80 25) (0 0 80 23) (0 24 80 25) (0 0 80 24) (0 24 80 25) (0 0 80 23) (0 24 80 25))"
    );
    assert_eq!(
        out[2],
        "OK ((wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-valid-p 999999) (wrong-type-argument window-valid-p 999999) (wrong-type-argument window-live-p 999999) (wrong-type-argument window-live-p 999999) (error \"999999 is not a live window\") (error \"999999 is not a valid window\") (error \"999999 is not a live window\") (error \"999999 is not a valid window\") (wrong-number-of-arguments window-text-height 3) (wrong-number-of-arguments window-mode-line-height 2) wrong-number-of-arguments (0 0 80 24) (wrong-number-of-arguments window-edges 5))"
    );
}

#[test]
fn next_window_cycles() {
    let results = eval_with_frame(
        "(let ((w1 (selected-window)))
           (split-window-internal (selected-window) nil nil nil)
           (let ((w2 (next-window)))
             (not (eq w1 w2))))",
    );
    assert_eq!(results[0], "OK t");
}

#[test]
fn one_window_p_tracks_current_window_count() {
    let results = bootstrap_eval_with_frame(
        "(list (one-window-p)
               (progn
                 (split-window-internal (selected-window) nil nil nil)
                 (one-window-p)))",
    );
    assert_eq!(results[0], "OK (t nil)");
}

#[test]
fn one_window_p_enforces_max_arity() {
    let results = bootstrap_eval_with_frame(
        "(condition-case err (one-window-p nil nil nil) (error (car err)))",
    );
    assert_eq!(results[0], "OK wrong-number-of-arguments");
}

#[test]
fn next_previous_window_enforce_max_arity() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(condition-case err (next-window nil nil nil nil) (error (car err)))
         (condition-case err (previous-window nil nil nil nil) (error (car err)))
         (let ((w1 (selected-window)))
           (split-window-internal (selected-window) nil nil nil)
           (windowp (next-window w1 nil nil)))
         (let ((w1 (selected-window)))
           (split-window-internal (selected-window) nil nil nil)
           (windowp (previous-window w1 nil nil)))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK wrong-number-of-arguments");
    assert_eq!(out[1], "OK wrong-number-of-arguments");
    assert_eq!(out[2], "OK t");
    assert_eq!(out[3], "OK t");
}

#[test]
fn previous_window_wraps() {
    let results = eval_with_frame(
        "(split-window-internal (selected-window) nil nil nil)
         (let ((w (previous-window)))
           (windowp w))",
    );
    assert_eq!(results[1], "OK t");
}

// -- Frame operations --

#[test]
fn frame_ops_enforce_max_arity() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(condition-case err (make-frame nil nil) (error (car err)))
         (condition-case err (delete-frame nil nil nil) (error (car err)))
         (condition-case err (frame-parameter nil 'name nil) (error (car err)))
         (condition-case err (frame-parameters nil nil) (error (car err)))
         (condition-case err (modify-frame-parameters nil nil nil) (error (car err)))
         (condition-case err (frame-visible-p nil nil) (error (car err)))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK wrong-number-of-arguments");
    assert_eq!(out[1], "OK wrong-number-of-arguments");
    assert_eq!(out[2], "OK wrong-number-of-arguments");
    assert_eq!(out[3], "OK wrong-number-of-arguments");
    assert_eq!(out[4], "OK wrong-number-of-arguments");
    assert_eq!(out[5], "OK wrong-number-of-arguments");
}

#[test]
fn frame_visible_p_enforces_arity_and_designators() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(condition-case err (frame-visible-p) (error (car err)))
         (condition-case err (frame-visible-p nil) (error err))
         (condition-case err (frame-visible-p 999999) (error err))
         (frame-visible-p (selected-frame))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK wrong-number-of-arguments");
    assert_eq!(out[1], "OK (wrong-type-argument frame-live-p nil)");
    assert_eq!(out[2], "OK (wrong-type-argument frame-live-p 999999)");
    assert_eq!(out[3], "OK t");
}

#[test]
fn frame_designator_errors_use_emacs_predicates() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(condition-case err (frame-parameter \"x\" 'name) (error err))
         (condition-case err (frame-parameter 999999 'name) (error err))
         (condition-case err (frame-parameters \"x\") (error err))
         (condition-case err (frame-parameters 999999) (error err))
         (condition-case err (modify-frame-parameters \"x\" nil) (error err))
         (condition-case err (modify-frame-parameters 999999 nil) (error err))
         (condition-case err (delete-frame \"x\") (error err))
         (condition-case err (delete-frame 999999) (error err))
         (frame-parameter nil 'name)
         (condition-case err (modify-frame-parameters nil nil) (error err))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK (wrong-type-argument framep \"x\")");
    assert_eq!(out[1], "OK (wrong-type-argument framep 999999)");
    assert_eq!(out[2], "OK (wrong-type-argument framep \"x\")");
    assert_eq!(out[3], "OK (wrong-type-argument framep 999999)");
    assert_eq!(out[4], "OK (wrong-type-argument frame-live-p \"x\")");
    assert_eq!(out[5], "OK (wrong-type-argument frame-live-p 999999)");
    assert_eq!(out[6], "OK (wrong-type-argument framep \"x\")");
    assert_eq!(out[7], "OK (wrong-type-argument framep 999999)");
    assert_eq!(out[8], "OK \"F1\"");
    assert_eq!(out[9], "OK nil");
}

#[test]
fn frame_query_builtins_match_gnu_batch_startup_geometry() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        r#"(list (frame-char-height)
                 (frame-char-width)
                 (frame-native-height)
                 (frame-native-width)
                 (frame-text-cols)
                 (frame-text-lines)
                 (frame-text-width)
                 (frame-text-height)
                 (frame-total-cols)
                 (frame-total-lines)
                 (frame-position))"#,
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK (1 1 25 80 80 25 80 25 80 25 (0 . 0))");
}

#[test]
fn frame_identity_builtins_match_gnu_batch_startup_defaults() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        r#"(let ((mouse (mouse-position))
                 (pixel (mouse-pixel-position)))
             (list (frame-id)
                   (eq (frame-root-frame) (selected-frame))
                   (eq (next-frame) (selected-frame))
                   (eq (previous-frame) (selected-frame))
                   (eq (old-selected-frame) (selected-frame))
                   (eq (car mouse) (selected-frame))
                   (cdr mouse)
                   (eq (car pixel) (selected-frame))
                   (cdr pixel)))"#,
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK (1 t t t t t (nil) t (nil))");
}

#[test]
fn frame_query_builtins_report_pixel_sizes_for_gui_frames() {
    let mut ev = Evaluator::new();
    let buf = ev.buffers.create_buffer("*scratch*");
    let fid = ev.frames.create_frame("gui", 800, 600, buf);
    {
        let frame = ev.frames.get_mut(fid).expect("gui frame");
        frame
            .parameters
            .insert("window-system".to_string(), Value::symbol("neomacs"));
    }

    assert_eq!(
        super::builtin_frame_native_width(&mut ev, vec![Value::Frame(fid.0)]).unwrap(),
        Value::Int(800)
    );
    assert_eq!(
        super::builtin_frame_native_height(&mut ev, vec![Value::Frame(fid.0)]).unwrap(),
        Value::Int(600)
    );
    assert_eq!(
        super::builtin_frame_text_width(&mut ev, vec![Value::Frame(fid.0)]).unwrap(),
        Value::Int(800)
    );
    assert_eq!(
        super::builtin_frame_text_height(&mut ev, vec![Value::Frame(fid.0)]).unwrap(),
        Value::Int(600)
    );
}

#[test]
fn select_frame_arity_designators_and_selection() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(condition-case err (select-frame) (error (car err)))
         (condition-case err (select-frame nil) (error err))
         (condition-case err (select-frame \"x\") (error err))
         (condition-case err (select-frame 999999) (error err))
         (let ((f1 (selected-frame))
               (f2 (make-frame)))
           (prog1
               (list (framep (select-frame f2))
                     (eq (selected-frame) f2))
             (select-frame f1)
             (delete-frame f2)))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK wrong-number-of-arguments");
    assert_eq!(out[1], "OK (wrong-type-argument frame-live-p nil)");
    assert_eq!(out[2], "OK (wrong-type-argument frame-live-p \"x\")");
    assert_eq!(out[3], "OK (wrong-type-argument frame-live-p 999999)");
    assert_eq!(out[4], "OK (t t)");
}

#[test]
fn select_frame_set_input_focus_arity_designators_and_result() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(condition-case err (select-frame-set-input-focus) (error (car err)))
         (condition-case err (select-frame-set-input-focus nil) (error err))
         (condition-case err (select-frame-set-input-focus \"x\") (error err))
         (condition-case err (select-frame-set-input-focus 999999) (error err))
         (let ((f (selected-frame)))
           (list (select-frame-set-input-focus f)
                 (eq (selected-frame) f)))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK wrong-number-of-arguments");
    assert_eq!(out[1], "OK (wrong-type-argument frame-live-p nil)");
    assert_eq!(out[2], "OK (wrong-type-argument frame-live-p \"x\")");
    assert_eq!(out[3], "OK (wrong-type-argument frame-live-p 999999)");
    assert_eq!(out[4], "OK (nil t)");
}

#[test]
fn set_frame_selected_window_matches_selection_and_error_semantics() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(condition-case err (set-frame-selected-window) (error (car err)))
         (condition-case err (set-frame-selected-window nil nil) (error err))
         (condition-case err (set-frame-selected-window nil 999999) (error err))
         (let* ((w1 (selected-window))
                (w2 (split-window-internal (selected-window) nil nil nil)))
           (prog1
               (list (eq (set-frame-selected-window nil w2) w2)
                     (eq (selected-window) w2))
             (select-window w1)
             (delete-window w2)))
         (let* ((w1 (selected-window))
                (w2 (split-window-internal (selected-window) nil nil nil))
                (t1 (window-use-time w1))
                (t2 (window-use-time w2)))
           (prog1
               (list (eq (set-frame-selected-window nil w2 t) w2)
                     (= (window-use-time w1) t1)
                     (= (window-use-time w2) t2)
                     (eq (selected-window) w2))
             (select-window w1)
             (delete-window w2)))
         (let* ((f1 (selected-frame))
                (f2 (make-frame))
                (w2 (progn
                      (select-frame f2)
                      (split-window-internal (selected-window) nil nil nil))))
           (select-frame f1)
           (prog1
               (list (eq (set-frame-selected-window f2 w2) w2)
                     (eq (selected-frame) f1)
                     (eq (frame-selected-window f2) w2))
             (select-frame f2)
             (delete-window w2)
             (select-frame f1)
             (delete-frame f2)))
         (let* ((f2 (make-frame))
                (w1 (selected-window)))
           (prog1
               (condition-case err (set-frame-selected-window f2 w1) (error err))
             (delete-frame f2)))
         (let* ((w1 (selected-window))
                (w2 (split-window-internal (selected-window) nil nil nil)))
           (prog1
               (list (eq (funcall #'set-frame-selected-window nil w2) w2)
                     (eq (apply #'set-frame-selected-window (list nil w1)) w1))
             (select-window w1)
             (delete-window w2)))
         (list (condition-case err (funcall #'set-frame-selected-window nil (selected-window) nil nil) (error err))
               (condition-case err (apply #'set-frame-selected-window '(nil)) (error err)))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK wrong-number-of-arguments");
    assert_eq!(out[1], "OK (wrong-type-argument window-live-p nil)");
    assert_eq!(out[2], "OK (wrong-type-argument window-live-p 999999)");
    assert_eq!(out[3], "OK (t t)");
    assert_eq!(out[4], "OK (t t t t)");
    assert_eq!(out[5], "OK (t t t)");
    assert_eq!(
        out[6],
        "OK (error \"In `set-frame-selected-window', WINDOW is not on FRAME\")"
    );
    assert_eq!(out[7], "OK (t t)");
    assert_eq!(
        out[8],
        "OK ((wrong-number-of-arguments #<subr set-frame-selected-window> 4) (wrong-number-of-arguments #<subr set-frame-selected-window> 1))"
    );
}

#[test]
fn old_selected_window_matches_stable_and_stale_window_semantics() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(windowp (old-selected-window))
         (let* ((w1 (selected-window))
                (w2 (split-window-internal (selected-window) nil nil nil)))
           (prog1
               (list (eq (old-selected-window) w1)
                     (progn (select-window w2) (eq (old-selected-window) w1))
                     (progn (select-window w1) (eq (old-selected-window) w1))
                     (progn (other-window 1) (eq (old-selected-window) w1))
                     (progn (other-window 1) (eq (old-selected-window) w1))
                     (progn (select-window w2 t) (eq (old-selected-window) w1))
                     (progn (select-window w1 t) (eq (old-selected-window) w1)))
             (select-window w1)
             (delete-window w2)))
         (let* ((w1 (selected-window))
                (w2 (split-window-internal (selected-window) nil nil nil)))
           (prog1
               (list (progn (select-window w2) (eq (old-selected-window) w1))
                     (progn (delete-window w1) (windowp (old-selected-window)))
                     (window-live-p (old-selected-window))
                     (eq (old-selected-window) w2))
             (delete-other-windows w2)))
         (list (condition-case err (old-selected-window nil) (error (car err)))
               (eq (funcall #'old-selected-window) (old-selected-window))
               (eq (apply #'old-selected-window nil) (old-selected-window))
               (condition-case err (funcall #'old-selected-window nil) (error (car err)))
               (condition-case err (apply #'old-selected-window '(nil)) (error (car err))))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK t");
    assert_eq!(out[1], "OK (t t t t t t t)");
    assert_eq!(out[2], "OK (t t nil nil)");
    assert_eq!(
        out[3],
        "OK (wrong-number-of-arguments t t wrong-number-of-arguments wrong-number-of-arguments)"
    );
}

#[test]
fn frame_old_selected_window_matches_batch_and_arity_semantics() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(condition-case err (frame-old-selected-window 999999) (error err))
         (condition-case err (frame-old-selected-window 'foo) (error err))
         (condition-case err (frame-old-selected-window nil nil) (error (car err)))
         (let ((f (selected-frame)))
           (list (frame-old-selected-window)
                 (frame-old-selected-window nil)
                 (frame-old-selected-window f)
                 (frame-old-selected-window (window-frame (selected-window)))))
         (let* ((w1 (selected-window))
                (w2 (split-window-internal (selected-window) nil nil nil)))
           (prog1
               (list (eq (frame-old-selected-window) nil)
                     (progn (select-window w2) (eq (frame-old-selected-window) nil))
                     (progn (other-window 1) (eq (frame-old-selected-window) nil))
                     (progn (set-frame-selected-window nil w2) (eq (frame-old-selected-window) nil))
                     (progn (set-frame-selected-window nil w1) (eq (frame-old-selected-window) nil))
                     (progn (set-frame-selected-window nil w2 t) (eq (frame-old-selected-window) nil))
                     (progn (set-frame-selected-window nil w1 t) (eq (frame-old-selected-window) nil)))
             (select-window w1)
             (delete-window w2)))
         (list (condition-case err (funcall #'frame-old-selected-window nil nil) (error err))
               (condition-case err (apply #'frame-old-selected-window '(nil nil)) (error err))
               (eq (funcall #'frame-old-selected-window) (frame-old-selected-window))
               (eq (apply #'frame-old-selected-window nil) (frame-old-selected-window)))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK (wrong-type-argument frame-live-p 999999)");
    assert_eq!(out[1], "OK (wrong-type-argument frame-live-p foo)");
    assert_eq!(out[2], "OK wrong-number-of-arguments");
    assert_eq!(out[3], "OK (nil nil nil nil)");
    assert_eq!(out[4], "OK (t t t t t t t)");
    assert_eq!(
        out[5],
        "OK ((wrong-number-of-arguments #<subr frame-old-selected-window> 2) (wrong-number-of-arguments #<subr frame-old-selected-window> 2) t t)"
    );
}

#[test]
fn frame_old_selected_window_direct_wrapper_matches_batch_nil_semantics() {
    let mut ev = Evaluator::new();
    let fid = super::ensure_selected_frame_id(&mut ev);

    assert_eq!(
        super::builtin_frame_old_selected_window(&mut ev, vec![]).unwrap(),
        Value::Nil
    );
    assert_eq!(
        super::builtin_frame_old_selected_window(&mut ev, vec![Value::Nil]).unwrap(),
        Value::Nil
    );
    assert_eq!(
        super::builtin_frame_old_selected_window(&mut ev, vec![Value::Frame(fid.0)]).unwrap(),
        Value::Nil
    );

    let err = super::builtin_frame_old_selected_window(&mut ev, vec![Value::Int(999999)])
        .expect_err("invalid frame should signal");
    match err {
        crate::emacs_core::error::Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("frame-live-p"), Value::Int(999999)]
            );
        }
        other => panic!("expected wrong-type-argument, got {other:?}"),
    }
}

#[test]
fn selected_frame_returns_frame_handle() {
    let r = eval_one_with_frame(
        "(let ((f (selected-frame)))
           (list (framep f)
                 (frame-live-p f)
                 (integerp f)
                 (eq f (window-frame))))",
    );
    assert_eq!(r, "OK (t t nil t)");
}

#[test]
fn frame_list_has_one() {
    let r = eval_one_with_frame("(length (frame-list))");
    assert_eq!(r, "OK 1");
}

#[test]
fn make_frame_creates_new() {
    let results = eval_with_frame(
        "(make-frame)
         (length (frame-list))",
    );
    assert!(results[0].starts_with("OK "));
    assert_eq!(results[1], "OK 2");
}

#[test]
fn x_create_frame_creates_live_frame_and_preserves_char_geometry_params() {
    let mut ev = Evaluator::new();
    let scratch = ev.buffers.create_buffer("*scratch*");
    let fid = ev.frames.create_frame("bootstrap", 800, 600, scratch);
    ev.frames
        .get_mut(fid)
        .expect("bootstrap frame")
        .parameters
        .insert("window-system".to_string(), Value::symbol("neomacs"));

    let params = Value::list(vec![
        Value::cons(Value::symbol("name"), Value::string("GUI")),
        Value::cons(Value::symbol("width"), Value::Int(80)),
        Value::cons(Value::symbol("height"), Value::Int(25)),
        Value::cons(Value::symbol("visibility"), Value::Nil),
    ]);
    let created = super::builtin_x_create_frame(&mut ev, vec![params]).expect("x-create-frame");

    let created_id = match created {
        Value::Frame(id) => crate::window::FrameId(id),
        other => panic!("expected frame object, got {other:?}"),
    };
    assert_ne!(created_id, fid);
    let frame = ev.frames.get(created_id).expect("created frame");
    assert_eq!(ev.frames.frame_list().len(), 2);
    assert_eq!(frame.name, "GUI");
    assert_eq!(frame.parameters.get("width"), Some(&Value::Int(80)));
    assert_eq!(frame.parameters.get("height"), Some(&Value::Int(25)));
    assert!(!frame.visible);
    assert_eq!(frame.char_width, 8.0);
    assert_eq!(frame.char_height, 16.0);
}

#[test]
fn x_create_frame_creates_opening_frame_and_notifies_host() {
    let mut ev = Evaluator::new();
    let scratch = ev.buffers.create_buffer("*scratch*");
    let fid = ev.frames.create_frame("bootstrap", 960, 640, scratch);
    {
        let frame = ev.frames.get_mut(fid).expect("bootstrap frame");
        frame
            .parameters
            .insert("window-system".to_string(), Value::symbol("neomacs"));
        if let Some(mini_leaf) = frame.minibuffer_leaf.as_mut() {
            mini_leaf.set_bounds(crate::window::Rect::new(0.0, 608.0, 960.0, 32.0));
        }
    }
    ev.set_variable("terminal-frame", Value::Frame(fid.0));
    let host = RecordingDisplayHost::new();
    let requests = host.requests.clone();
    ev.set_display_host(Box::new(host));

    let params = Value::list(vec![
        Value::cons(Value::symbol("name"), Value::string("Neomacs")),
        Value::cons(Value::symbol("title"), Value::string("Neomacs")),
        Value::cons(Value::symbol("width"), Value::Int(80)),
        Value::cons(Value::symbol("height"), Value::Int(25)),
    ]);
    let created = super::builtin_x_create_frame(&mut ev, vec![params]).expect("x-create-frame");

    let created_id = match created {
        Value::Frame(id) => crate::window::FrameId(id),
        other => panic!("expected frame object, got {other:?}"),
    };
    assert_ne!(created_id, fid);
    assert_eq!(ev.frames.frame_list().len(), 2);
    let frame = ev.frames.get(created_id).expect("created opening frame");
    assert_eq!(frame.parameters.get("width"), Some(&Value::Int(80)));
    assert_eq!(frame.parameters.get("height"), Some(&Value::Int(25)));
    let requests = requests.borrow();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].frame_id, created_id);
    assert_eq!(requests[0].title, "Neomacs");
    assert_eq!(requests[0].width, frame.width);
    assert_eq!(requests[0].height, frame.height);
}

#[test]
fn delete_frame_works() {
    let results = eval_with_frame(
        "(let ((f2 (make-frame)))
           (delete-frame f2)
           (length (frame-list)))",
    );
    assert_eq!(results[0], "OK 1");
}

#[test]
fn framep_true() {
    let r = eval_one_with_frame("(framep (selected-frame))");
    assert_eq!(r, "OK t");
}

#[test]
fn framep_returns_window_system_symbol_for_gui_frames() {
    let mut ev = Evaluator::new();
    let frame_id = super::ensure_selected_frame_id(&mut ev);
    ev.frames
        .get_mut(frame_id)
        .expect("selected frame")
        .parameters
        .insert("window-system".to_string(), Value::symbol("neomacs"));

    let result = super::builtin_framep(&mut ev, vec![Value::Frame(frame_id.0)]).unwrap();
    assert_eq!(result, Value::symbol("neomacs"));
}

#[test]
fn framep_false() {
    let r = eval_one_with_frame("(framep 999999)");
    assert_eq!(r, "OK nil");
}

#[test]
fn frame_live_p_true() {
    let r = eval_one_with_frame("(frame-live-p (selected-frame))");
    assert_eq!(r, "OK t");
}

#[test]
fn frame_live_p_false() {
    let r = eval_one_with_frame("(frame-live-p 999999)");
    assert_eq!(r, "OK nil");
}

#[test]
fn frame_builtins_accept_frame_handle_values() {
    let mut ev = Evaluator::new();
    let fid = super::ensure_selected_frame_id(&mut ev);
    let frame = Value::Frame(fid.0);

    assert_eq!(
        super::builtin_framep(&mut ev, vec![frame]).unwrap(),
        Value::True
    );
    assert_eq!(
        super::builtin_frame_live_p(&mut ev, vec![frame]).unwrap(),
        Value::True
    );
    assert_eq!(
        super::builtin_frame_visible_p(&mut ev, vec![frame]).unwrap(),
        Value::True
    );
    assert_eq!(
        super::builtin_select_frame(&mut ev, vec![frame]).unwrap(),
        Value::Frame(fid.0)
    );
    assert_eq!(
        super::builtin_select_frame_set_input_focus(&mut ev, vec![frame]).unwrap(),
        Value::Nil
    );
}

#[test]
fn frame_visible_p_requires_one_arg() {
    let r = eval_one_with_frame("(condition-case err (frame-visible-p) (error (car err)))");
    assert_eq!(r, "OK wrong-number-of-arguments");
}

#[test]
fn frame_parameter_name() {
    let r = eval_one_with_frame("(frame-parameter (selected-frame) 'name)");
    assert_eq!(r, r#"OK "F1""#);
}

#[test]
fn frame_parameter_width() {
    let r = eval_one_with_frame("(frame-parameter (selected-frame) 'width)");
    assert_eq!(r, "OK 100");
}

#[test]
fn frame_parameter_height() {
    let r = eval_one_with_frame("(frame-parameter (selected-frame) 'height)");
    assert_eq!(r, "OK 37");
}

#[test]
fn frame_parameters_returns_alist() {
    let r = eval_one_with_frame("(listp (frame-parameters))");
    assert_eq!(r, "OK t");
}

#[test]
fn modify_frame_parameters_name() {
    let results = eval_with_frame(
        "(modify-frame-parameters (selected-frame) '((name . \"NewName\")))
         (frame-parameter (selected-frame) 'name)",
    );
    assert_eq!(results[0], "OK nil");
    assert_eq!(results[1], r#"OK "NewName""#);
}

#[test]
fn modify_frame_parameters_width_height_preserve_pixel_dimensions() {
    let mut ev = Evaluator::new();
    let buf = ev.buffers.create_buffer("*scratch*");
    let fid = ev.frames.create_frame("F1", 800, 600, buf);
    let forms =
        parse_forms("(modify-frame-parameters (selected-frame) '((width . 80) (height . 25)))")
            .expect("parse");
    let out = ev.eval_forms(&forms);
    assert!(
        out[0].is_ok(),
        "modify-frame-parameters failed: {:?}",
        out[0]
    );

    let frame = ev.frames.get(fid).expect("frame should exist");
    assert_eq!(frame.width, 800);
    assert_eq!(frame.height, 600);
    assert_eq!(frame.parameters.get("width"), Some(&Value::Int(80)));
    assert_eq!(frame.parameters.get("height"), Some(&Value::Int(25)));
}

#[test]
fn set_frame_size_builtins_preserve_pixel_dimensions() {
    let mut ev = Evaluator::new();
    let buf = ev.buffers.create_buffer("*scratch*");
    let fid = ev.frames.create_frame("F1", 800, 600, buf);
    let forms = parse_forms(
        "(progn
           (set-frame-width (selected-frame) 90)
           (set-frame-height (selected-frame) 30)
           (set-frame-size (selected-frame) 100 35))",
    )
    .expect("parse");
    let out = ev.eval_forms(&forms);
    assert!(
        out[0].is_ok(),
        "set-frame-size builtins failed: {:?}",
        out[0]
    );

    let frame = ev.frames.get(fid).expect("frame should exist");
    assert_eq!(frame.width, 800);
    assert_eq!(frame.height, 600);
    assert_eq!(frame.parameters.get("width"), Some(&Value::Int(100)));
    assert_eq!(frame.parameters.get("height"), Some(&Value::Int(36)));
    assert_eq!(
        frame.parameters.get("neovm--frame-text-lines"),
        Some(&Value::Int(35))
    );
}

#[test]
fn switch_to_buffer_changes_window() {
    let results = eval_with_frame(
        "(get-buffer-create \"other-buf\")
         (switch-to-buffer \"other-buf\")
         (bufferp (window-buffer))",
    );
    assert_eq!(results[2], "OK t");
}

#[test]
fn set_window_buffer_works() {
    let results = eval_with_frame(
        "(get-buffer-create \"buf2\")
         (set-window-buffer (selected-window) \"buf2\")
         (bufferp (window-buffer))",
    );
    assert_eq!(results[1], "OK nil"); // set-window-buffer returns nil
    assert_eq!(results[2], "OK t");
}

#[test]
fn set_window_buffer_restores_saved_window_point_and_keep_margins() {
    let results = eval_with_frame(
        "(setq swb-test-w (selected-window))
         (setq swb-test-b1 (get-buffer-create \"swb-state-a\"))
         (setq swb-test-b2 (get-buffer-create \"swb-state-b\"))
         (with-current-buffer swb-test-b1
           (erase-buffer)
           (insert (make-string 300 ?a))
           (goto-char 120))
         (with-current-buffer swb-test-b2
           (erase-buffer)
           (insert (make-string 300 ?b))
           (goto-char 150))
         (set-window-buffer swb-test-w swb-test-b1)
         (set-window-start swb-test-w 110)
         (set-window-point swb-test-w 120)
         (set-window-margins swb-test-w 3 4)
         (list (window-start swb-test-w)
               (window-point swb-test-w)
               (window-margins swb-test-w))
         (progn
           (set-window-buffer swb-test-w swb-test-b2)
           (list (window-start swb-test-w)
                 (window-point swb-test-w)
                 (window-margins swb-test-w)))
         (progn
           (set-window-margins swb-test-w 7 8)
           (set-window-buffer swb-test-w swb-test-b1 t)
           (list (window-start swb-test-w)
                 (window-point swb-test-w)
                 (window-margins swb-test-w)))
         (progn
           (set-window-margins swb-test-w 9 10)
           (set-window-buffer swb-test-w swb-test-b2 t)
           (list (window-start swb-test-w)
                 (window-point swb-test-w)
                 (window-margins swb-test-w)))
         (progn
           (set-window-margins swb-test-w 11 12)
           (set-window-buffer swb-test-w swb-test-b1 nil)
           (list (window-start swb-test-w)
                 (window-point swb-test-w)
                 (window-margins swb-test-w)))",
    );
    assert_eq!(results[9], "OK (110 120 (3 . 4))");
    assert_eq!(results[10], "OK (1 150 (nil))");
    assert_eq!(results[11], "OK (110 120 (7 . 8))");
    assert_eq!(results[12], "OK (1 150 (9 . 10))");
    assert_eq!(results[13], "OK (110 120 (nil))");
}

#[test]
fn set_window_buffer_updates_history_lists_on_real_buffer_switches() {
    let results = eval_with_frame(
        "(let* ((w (selected-window))
                (b1 (get-buffer-create \"swb-hist-a\"))
                (b2 (get-buffer-create \"swb-hist-b\"))
                (n '((foo 1 2))))
           (with-current-buffer b1
             (erase-buffer)
             (insert (make-string 300 ?a)))
           (with-current-buffer b2
             (erase-buffer)
             (insert (make-string 300 ?b)))
           (set-window-prev-buffers w nil)
           (set-window-next-buffers w nil)
           (set-window-buffer w b1)
           (set-window-start w 7)
           (set-window-point w 11)
           (set-window-next-buffers w n)
           (set-window-buffer w b2)
           (list (null (window-next-buffers w))
                 (mapcar (lambda (e) (buffer-name (car e))) (window-prev-buffers w))
                 (mapcar (lambda (e)
                           (list (markerp (nth 1 e))
                                 (marker-position (nth 1 e))
                                 (markerp (nth 2 e))
                                 (marker-position (nth 2 e))))
                         (window-prev-buffers w))))
         (let* ((w (selected-window))
                (same (window-buffer w))
                (n '((foo 1 2)))
                (before (window-prev-buffers w)))
           (set-window-next-buffers w n)
           (set-window-buffer w same)
           (list (equal (window-prev-buffers w) before)
                 (equal (window-next-buffers w) n)))
         (let* ((w (selected-window))
                (b1 (get-buffer-create \"swb-hist-d1\"))
                (b2 (get-buffer-create \"swb-hist-d2\")))
           (set-window-prev-buffers w nil)
           (set-window-buffer w b1)
           (set-window-buffer w b2)
           (set-window-buffer w b1)
           (set-window-buffer w b2)
           (mapcar (lambda (e) (buffer-name (car e))) (window-prev-buffers w)))",
    );
    assert_eq!(
        results[0],
        "OK (t (\"swb-hist-a\" \"*scratch*\") ((t 7 t 11) (t 1 t 1)))"
    );
    assert_eq!(results[1], "OK (t t)");
    assert_eq!(
        results[2],
        "OK (\"swb-hist-d1\" \"swb-hist-d2\" \"swb-hist-b\")"
    );
}

#[test]
fn window_end_greater_than_start() {
    let r = eval_one_with_frame("(> (window-end) (window-start))");
    assert_eq!(r, "OK t");
}

#[test]
fn display_buffer_returns_window() {
    let results = bootstrap_eval_with_frame(
        "(get-buffer-create \"disp-buf\")
         (windowp (display-buffer \"disp-buf\"))",
    );
    assert_eq!(results[1], "OK t");
}

#[test]
fn pop_to_buffer_returns_buffer() {
    let results = bootstrap_eval_with_frame(
        "(get-buffer-create \"pop-buf\")
         (bufferp (pop-to-buffer \"pop-buf\"))",
    );
    assert_eq!(results[1], "OK t");
}

#[test]
fn switch_display_pop_bootstrap_initial_frame() {
    let out = bootstrap_eval_with_frame(
        "(save-current-buffer (bufferp (switch-to-buffer \"*scratch*\")))
         (save-current-buffer (windowp (display-buffer \"*scratch*\")))
         (save-current-buffer (bufferp (pop-to-buffer \"*scratch*\")))",
    );
    assert_eq!(out[0], "OK t");
    assert_eq!(out[1], "OK t");
    assert_eq!(out[2], "OK t");
}

#[test]
fn switch_display_pop_enforce_max_arity() {
    let results = bootstrap_eval_with_frame(
        "(condition-case err (switch-to-buffer \"*scratch*\" nil nil nil) (error (car err)))
         (condition-case err (display-buffer \"*scratch*\" nil nil nil) (error (car err)))
         (condition-case err (pop-to-buffer \"*scratch*\" nil nil nil) (error (car err)))
         (condition-case err (set-window-buffer (selected-window) \"*scratch*\" nil nil) (error (car err)))",
    );
    assert_eq!(results[0], "OK wrong-number-of-arguments");
    assert_eq!(results[1], "OK wrong-number-of-arguments");
    assert_eq!(results[2], "OK wrong-number-of-arguments");
    assert_eq!(results[3], "OK wrong-number-of-arguments");
}

#[test]
fn switch_display_pop_reject_non_buffer_designators() {
    let results = bootstrap_eval_with_frame(
        "(condition-case err (switch-to-buffer 1) (error (list (car err) (nth 1 err) (nth 2 err))))
         (condition-case err (display-buffer 1) (error (list (car err) (nth 1 err) (nth 2 err))))
         (condition-case err (pop-to-buffer 1) (error (list (car err) (nth 1 err) (nth 2 err))))
         (condition-case err (set-window-buffer (selected-window) 1) (error (list (car err) (nth 1 err) (nth 2 err))))",
    );
    assert_eq!(results[0], "OK (wrong-type-argument stringp 1)");
    assert_eq!(results[1], "OK (wrong-type-argument stringp 1)");
    assert_eq!(results[2], "OK (wrong-type-argument stringp 1)");
    assert_eq!(results[3], "OK (wrong-type-argument stringp 1)");
}

#[test]
fn switch_and_pop_create_missing_named_buffers() {
    let results = bootstrap_eval_with_frame(
        "(save-current-buffer (bufferp (switch-to-buffer \"sw-auto-create\")))
         (buffer-live-p (get-buffer \"sw-auto-create\"))
         (kill-buffer \"sw-auto-create\")
         (save-current-buffer (bufferp (pop-to-buffer \"pop-auto-create\")))
         (buffer-live-p (get-buffer \"pop-auto-create\"))
         (kill-buffer \"pop-auto-create\")",
    );
    assert_eq!(results[0], "OK t");
    assert_eq!(results[1], "OK t");
    assert_eq!(results[2], "OK t");
    assert_eq!(results[3], "OK t");
    assert_eq!(results[4], "OK t");
    assert_eq!(results[5], "OK t");
}

#[test]
fn display_buffer_missing_or_dead_signals_invalid_buffer() {
    let results = bootstrap_eval_with_frame(
        "(condition-case err (display-buffer \"db-missing\") (error err))
         (let ((b (get-buffer-create \"db-dead\")))
           (kill-buffer b)
           (condition-case err (display-buffer b) (error err)))",
    );
    assert_eq!(results[0], "OK (error \"Invalid buffer\")");
    assert_eq!(results[1], "OK (error \"Invalid buffer\")");
}

#[test]
fn set_window_buffer_matches_window_and_buffer_designator_errors() {
    let mut ev = Evaluator::new();
    let buf = ev.buffers.create_buffer("*scratch*");
    ev.frames.create_frame("F1", 800, 600, buf);
    let dead = Value::Buffer(ev.buffers.create_buffer("swb-dead"));
    ev.set_variable("vm-swb-dead", dead);
    let forms = parse_forms(
        "(condition-case err (set-window-buffer nil \"*scratch*\") (error err))
         (condition-case err (set-window-buffer nil \"swb-missing\") (error err))
         (progn
           (kill-buffer vm-swb-dead)
           (condition-case err (set-window-buffer nil vm-swb-dead) (error err)))
         (condition-case err (set-window-buffer 999999 \"*scratch*\") (error err))
         (condition-case err (set-window-buffer 'foo \"*scratch*\") (error err))",
    )
    .expect("parse");
    let results = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(results[0], "OK nil");
    assert_eq!(results[1], "OK (wrong-type-argument bufferp nil)");
    assert_eq!(
        results[2],
        "OK (error \"Attempt to display deleted buffer\")"
    );
    assert_eq!(results[3], "OK (wrong-type-argument window-live-p 999999)");
    assert_eq!(results[4], "OK (wrong-type-argument window-live-p foo)");
}

#[test]
fn set_window_buffer_bootstraps_initial_frame_for_nil_window_designator() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        "(condition-case err
             (let ((b (get-buffer-create \"swb-bootstrap\")))
               (set-buffer b)
               (erase-buffer)
               (insert \"abcdef\")
               (goto-char 1)
               (set-window-buffer nil b)
               (list (buffer-name (window-buffer nil))
                     (window-start nil)
                     (window-end nil)))
           (error err))",
    )
    .expect("parse");
    let out = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>();
    assert_eq!(out[0], "OK (\"swb-bootstrap\" 1 7)");
}

#[test]
fn scroll_and_recenter_use_selected_window_state() {
    let results = eval_with_frame(
        "(let ((w (selected-window)))
           (with-current-buffer (window-buffer w)
             (erase-buffer)
             (insert \"a\nb\nc\nd\ne\nf\ng\nh\n\"))
           (set-window-point w 1)
           (list (progn (scroll-up 2) (window-point w))
                 (progn (scroll-down 1) (window-point w))
                 (progn (scroll-left 3) (window-hscroll w))
                 (progn (scroll-right 1) (window-hscroll w))
                 (progn (set-window-point w 9) (recenter 1) (window-start w))))",
    );
    assert_eq!(results[0], "OK (5 3 3 2 7)");
}
