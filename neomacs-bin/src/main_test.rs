use super::{
    BOOTSTRAP_CORE_FEATURES, BootstrapDisplayConfig, EarlyCliAction, FrontendKind,
    PrimaryWindowDisplayHost, PrimaryWindowSize, StartupOptions, bootstrap_buffers,
    bootstrap_display_config, bootstrap_frame_metrics, classify_early_cli_action,
    configure_gnu_startup_state, current_layout_frame_id, parse_startup_options, render_help_text,
    render_version_text, run_gnu_startup,
};
use neomacs_display_runtime::thread_comm::RenderCommand;
use neovm_core::emacs_core::Evaluator;
use neovm_core::emacs_core::GuiFrameHostRequest;
use neovm_core::emacs_core::Value;
use neovm_core::emacs_core::load::{
    apply_runtime_startup_state, create_bootstrap_evaluator_cached_with_features,
    create_bootstrap_evaluator_with_features,
};
use neovm_core::emacs_core::parse_forms;
use neovm_core::emacs_core::print_value_with_eval;
use neovm_core::emacs_core::value::list_to_vec;
use neovm_core::window::FrameId;
use std::path::Path;
use std::sync::{Arc, Mutex};

fn gui_display() -> BootstrapDisplayConfig {
    bootstrap_display_config(FrontendKind::Gui)
}

fn gui_startup() -> StartupOptions {
    StartupOptions {
        frontend: FrontendKind::Gui,
        forwarded_args: vec!["neomacs".to_string()],
        terminal_device: None,
        noninteractive: false,
    }
}

fn gui_startup_with_args(args: &[&str]) -> StartupOptions {
    let mut forwarded_args = vec!["neomacs".to_string()];
    forwarded_args.extend(args.iter().map(|arg| (*arg).to_string()));
    StartupOptions {
        frontend: FrontendKind::Gui,
        forwarded_args,
        terminal_device: None,
        noninteractive: false,
    }
}

fn bootstrap_runtime_gui_startup(eval: &mut Evaluator) -> FrameId {
    let _bootstrap = bootstrap_buffers(eval, 960, 640, gui_display());
    apply_runtime_startup_state(eval).expect("runtime startup state should succeed");
    let frame_id = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after bootstrap")
        .id;
    configure_gnu_startup_state(eval, frame_id, &gui_startup());
    frame_id
}

#[test]
fn opening_gui_frame_adoption_does_not_push_stale_window_size() {
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let mut host = PrimaryWindowDisplayHost {
        cmd_tx,
        primary_window_adopted: false,
        primary_frame_id: None,
        font_metrics: None,
        primary_window_size: Arc::new(Mutex::new(PrimaryWindowSize {
            width: 1600,
            height: 1800,
        })),
    };

    neovm_core::emacs_core::DisplayHost::realize_gui_frame(
        &mut host,
        GuiFrameHostRequest {
            frame_id: FrameId(0x100000001),
            width: 960,
            height: 640,
            title: "Neomacs".to_string(),
        },
    )
    .expect("adopt opening gui frame");

    let commands: Vec<_> = cmd_rx.try_iter().collect();
    assert_eq!(commands.len(), 1);
    match &commands[0] {
        RenderCommand::SetWindowTitle { title } => assert_eq!(title, "Neomacs"),
        other => panic!("expected SetWindowTitle, got {other:?}"),
    }
    assert!(host.primary_window_adopted);
    assert_eq!(host.primary_frame_id, Some(FrameId(0x100000001)));
}

#[test]
fn current_layout_frame_follows_selected_frame() {
    let mut eval = Evaluator::new();
    let b1 = eval.buffer_manager_mut().create_buffer("*one*");
    let b2 = eval.buffer_manager_mut().create_buffer("*two*");
    let f1 = eval.frame_manager_mut().create_frame("F1", 80, 24, b1);
    let f2 = eval.frame_manager_mut().create_frame("F2", 80, 24, b2);

    assert_eq!(current_layout_frame_id(&eval), Some(f1));
    assert!(eval.frame_manager_mut().select_frame(f2));
    assert_eq!(current_layout_frame_id(&eval), Some(f2));
}

#[test]
fn current_layout_frame_tracks_surrogate_after_bootstrap_frame_deletion() {
    let mut eval = Evaluator::new();
    let b1 = eval.buffer_manager_mut().create_buffer("*one*");
    let b2 = eval.buffer_manager_mut().create_buffer("*two*");
    let f1 = eval.frame_manager_mut().create_frame("F1", 80, 24, b1);
    let f2 = eval.frame_manager_mut().create_frame("F2", 80, 24, b2);

    assert_eq!(current_layout_frame_id(&eval), Some(f1));
    assert!(eval.frame_manager_mut().delete_frame(f1));
    assert_eq!(current_layout_frame_id(&eval), Some(f2));
}

#[test]
fn early_cli_handles_gnu_c_owned_help_and_version_options() {
    assert_eq!(
        classify_early_cli_action(
            ["./target/release/neomacs", "--help"]
                .into_iter()
                .map(str::to_string)
        ),
        Some(EarlyCliAction::PrintHelp {
            program: "./target/release/neomacs".to_string()
        })
    );
    assert_eq!(
        classify_early_cli_action(
            ["./target/release/neomacs", "-version"]
                .into_iter()
                .map(str::to_string)
        ),
        Some(EarlyCliAction::PrintVersion)
    );
    assert_eq!(
        classify_early_cli_action(
            ["./target/release/neomacs", "--", "--help"]
                .into_iter()
                .map(str::to_string)
        ),
        None
    );
}

#[test]
fn early_cli_help_uses_invoked_program_name_and_gnu_style_usage() {
    let help = render_help_text("/tmp/neomacs");
    assert!(help.starts_with("Usage: /tmp/neomacs [OPTION-OR-FILENAME]...\n\n"));
    assert!(help.contains("--help                          display this help and exit"));
    assert!(help.contains("--quick, -Q                 equivalent to:"));
}

#[test]
fn early_cli_version_reports_neomacs_identity() {
    let version = render_version_text();
    assert!(version.starts_with("Neomacs "));
    assert!(version.contains("Standalone Rust binary for Neomacs"));
}

#[test]
fn startup_option_parser_promotes_nw_and_strips_c_owned_display_flags() {
    let parsed = parse_startup_options(
        [
            "neomacs",
            "-nw",
            "--display",
            ":1",
            "--terminal=/dev/pts/7",
            "README.md",
        ]
        .into_iter()
        .map(str::to_string),
    )
    .expect("startup options should parse");

    assert_eq!(parsed.frontend, FrontendKind::Tty);
    assert!(!parsed.noninteractive);
    assert_eq!(parsed.terminal_device.as_deref(), Some("/dev/pts/7"));
    assert_eq!(
        parsed.forwarded_args,
        vec!["neomacs".to_string(), "README.md".to_string()]
    );
}

#[test]
fn startup_option_parser_promotes_batch_to_noninteractive_and_strips_batch_flag() {
    let parsed = parse_startup_options(
        ["neomacs", "--batch", "-Q", "--eval", "(princ 1)"]
            .into_iter()
            .map(str::to_string),
    )
    .expect("startup options should parse");

    assert_eq!(parsed.frontend, FrontendKind::Tty);
    assert!(parsed.noninteractive);
    assert_eq!(
        parsed.forwarded_args,
        vec![
            "neomacs".to_string(),
            "-Q".to_string(),
            "--eval".to_string(),
            "(princ 1)".to_string()
        ]
    );
}

#[test]
fn configure_gnu_startup_state_marks_bootstrap_gui_frame_as_initial_frame() {
    let mut eval = Evaluator::new();
    configure_gnu_startup_state(&mut eval, FrameId(42), &gui_startup());

    assert_eq!(
        eval.obarray().symbol_value("terminal-frame"),
        Some(&Value::Nil)
    );
    assert_eq!(
        eval.obarray().symbol_value("frame-initial-frame"),
        Some(&Value::Frame(42))
    );
    assert_eq!(
        eval.obarray().symbol_value("default-minibuffer-frame"),
        Some(&Value::Frame(42))
    );
}

#[test]
fn configure_gnu_startup_state_reports_neomacs_window_system_for_gui_boots() {
    let mut eval = Evaluator::new();
    configure_gnu_startup_state(&mut eval, FrameId(42), &gui_startup());

    assert_eq!(
        eval.obarray().symbol_value("window-system"),
        Some(&Value::symbol("neomacs"))
    );
    assert_eq!(
        eval.obarray().symbol_value("initial-window-system"),
        Some(&Value::symbol("neomacs"))
    );
}

#[test]
fn configure_gnu_startup_state_clears_window_system_for_tty_boots() {
    let mut eval = Evaluator::new();
    let startup = StartupOptions {
        frontend: FrontendKind::Tty,
        forwarded_args: vec!["neomacs".to_string(), "-q".to_string()],
        terminal_device: Some("/dev/tty".to_string()),
        noninteractive: false,
    };
    configure_gnu_startup_state(&mut eval, FrameId(7), &startup);

    assert_eq!(
        eval.obarray().symbol_value("window-system"),
        Some(&Value::Nil)
    );
    assert_eq!(
        eval.obarray().symbol_value("initial-window-system"),
        Some(&Value::Nil)
    );
    assert_eq!(
        eval.obarray().symbol_value("command-line-args"),
        Some(&Value::list(vec![
            Value::string("neomacs"),
            Value::string("-q")
        ]))
    );
    assert_eq!(
        eval.obarray().symbol_value("command-line-args-left"),
        Some(&Value::list(vec![Value::string("-q")]))
    );
}

#[test]
fn configure_gnu_startup_state_marks_batch_mode_noninteractive() {
    let mut eval = Evaluator::new();
    let startup = StartupOptions {
        frontend: FrontendKind::Tty,
        forwarded_args: vec![
            "neomacs".to_string(),
            "-Q".to_string(),
            "--eval".to_string(),
            "(princ 1)".to_string(),
        ],
        terminal_device: None,
        noninteractive: true,
    };
    configure_gnu_startup_state(&mut eval, FrameId(9), &startup);

    assert_eq!(
        eval.obarray().symbol_value("noninteractive"),
        Some(&Value::True)
    );
    assert_eq!(
        eval.obarray().symbol_value("command-line-args"),
        Some(&Value::list(vec![
            Value::string("neomacs"),
            Value::string("-Q"),
            Value::string("--eval"),
            Value::string("(princ 1)"),
        ]))
    );
}

#[test]
fn configure_gnu_startup_state_seeds_command_line_args_left_for_gnu_startup() {
    let mut eval = Evaluator::new();
    let startup = gui_startup_with_args(&["-Q", "-l", "/tmp/demo.el"]);
    configure_gnu_startup_state(&mut eval, FrameId(42), &startup);

    assert_eq!(
        eval.obarray().symbol_value("command-line-args-left"),
        Some(&Value::list(vec![
            Value::string("-Q"),
            Value::string("-l"),
            Value::string("/tmp/demo.el")
        ]))
    );
}

#[test]
fn bootstrap_buffers_seed_frame_with_renderer_metrics() {
    let metrics = bootstrap_frame_metrics();
    let mut eval = Evaluator::new();
    let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
    let frame = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after bootstrap");
    assert_eq!(frame.char_width, metrics.char_width);
    assert_eq!(frame.char_height, metrics.char_height);
    assert_eq!(frame.font_pixel_size, metrics.font_pixel_size);
    let font_param = frame
        .parameters
        .get("font")
        .expect("bootstrap GUI frame should seed a font frame parameter");
    assert!(font_param.is_string());
    let minibuffer_height = frame
        .minibuffer_leaf
        .as_ref()
        .expect("minibuffer leaf")
        .bounds()
        .height;
    assert_eq!(minibuffer_height, metrics.char_height);
}

#[test]
fn bootstrap_buffers_reuses_selected_startup_frame_when_one_already_exists() {
    let metrics = bootstrap_frame_metrics();
    let mut eval = Evaluator::new();
    let old_buffer = eval.buffer_manager_mut().create_buffer("*old*");
    let old_frame = eval
        .frame_manager_mut()
        .create_frame("old", 320, 200, old_buffer);
    {
        let frame = eval
            .frame_manager_mut()
            .get_mut(old_frame)
            .expect("old frame should exist");
        frame.title = "old".to_string();
    }

    let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());

    assert_eq!(eval.frame_manager().frame_list().len(), 1);
    let selected = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after bootstrap");
    assert_eq!(selected.id, old_frame);
    assert_eq!(selected.width, 960);
    assert_eq!(selected.height, 640);
    assert_eq!(
        selected.effective_window_system(),
        Some(Value::symbol("neomacs"))
    );
    assert_eq!(selected.title, "Neomacs");
    assert_eq!(selected.char_width, metrics.char_width);
    assert_eq!(selected.char_height, metrics.char_height);
    let minibuffer_height = selected
        .minibuffer_leaf
        .as_ref()
        .expect("minibuffer leaf")
        .bounds()
        .height;
    assert_eq!(minibuffer_height, metrics.char_height);
}

#[test]
fn bootstrap_buffers_reuses_cached_surrogate_frame_when_it_is_the_only_selected_frame() {
    let metrics = bootstrap_frame_metrics();
    let mut eval = Evaluator::new();
    let old_buffer = eval.buffer_manager_mut().create_buffer("*old*");
    let surrogate = eval
        .frame_manager_mut()
        .create_frame("F1", 80, 25, old_buffer);

    let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());

    assert_eq!(eval.frame_manager().frame_list().len(), 1);
    let selected = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after bootstrap");
    assert_eq!(selected.id, surrogate);
    assert_eq!(selected.width, 960);
    assert_eq!(selected.height, 640);
    assert_eq!(
        selected.effective_window_system(),
        Some(Value::symbol("neomacs"))
    );
    assert_eq!(selected.char_width, metrics.char_width);
    assert_eq!(selected.char_height, metrics.char_height);
}

#[test]
fn bootstrap_buffers_reuses_existing_named_buffers_in_cached_bootstrap() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("cached bootstrap evaluator");
    let original_scratch = eval
        .buffer_manager()
        .find_buffer_by_name("*scratch*")
        .expect("bootstrap scratch");

    let bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());

    assert_eq!(bootstrap.scratch_id, original_scratch);
    let scratch_count = eval
        .buffer_manager()
        .buffer_list()
        .into_iter()
        .filter(|id| {
            eval.buffer_manager()
                .get(*id)
                .is_some_and(|buffer| buffer.name == "*scratch*")
        })
        .count();
    assert_eq!(scratch_count, 1);
}

#[test]
fn gnu_startup_keeps_scratch_selected_under_q_startup() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("cached bootstrap evaluator");
    let _frame_id = bootstrap_runtime_gui_startup(&mut eval);

    run_gnu_startup(&mut eval);

    let current = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer after startup");
    assert_eq!(current.name, "*scratch*");
}

#[test]
fn gnu_startup_keeps_bootstrap_gui_frame_instead_of_creating_replacement_frame() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("cached bootstrap evaluator");
    let frame_id = bootstrap_runtime_gui_startup(&mut eval);

    run_gnu_startup(&mut eval);

    let frame_ids: Vec<_> = eval.frame_manager().frame_list().into_iter().collect();
    assert_eq!(frame_ids, vec![frame_id]);
    assert_eq!(
        eval.frame_manager()
            .selected_frame()
            .expect("selected frame after startup")
            .id,
        frame_id
    );
}

#[test]
fn gnu_startup_keeps_scratch_text_accessible_under_q_startup() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("cached bootstrap evaluator");
    let _frame_id = bootstrap_runtime_gui_startup(&mut eval);

    run_gnu_startup(&mut eval);

    let forms = parse_forms(
        r#"(with-current-buffer (current-buffer)
                 (list (buffer-name)
                       major-mode
                       (> (point-max) 1)
                       (> (buffer-size) 0)
                       (> (length
                           (buffer-substring-no-properties
                            (point-min)
                            (min (point-max) (+ (point-min) 16))))
                          0)))"#,
    )
    .expect("parse scratch accessibility probe");
    let result = eval
        .eval_expr(&forms[0])
        .expect("scratch accessibility probe should evaluate");
    assert_eq!(
        print_value_with_eval(&mut eval, &result),
        "(\"*scratch*\" lisp-interaction-mode t t t)"
    );
}

#[test]
fn gnu_startup_preserves_default_fontset_alias() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("cached bootstrap evaluator");
    let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
    let frame_id = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after bootstrap")
        .id;
    configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

    run_gnu_startup(&mut eval);

    let forms = parse_forms("(query-fontset \"fontset-default\")").expect("parse fontset query");
    let result = eval
        .eval_expr(&forms[0])
        .expect("fontset query should evaluate");
    assert_eq!(
        result,
        Value::string("-*-*-*-*-*-*-*-*-*-*-*-*-fontset-default")
    );
}

#[test]
fn gnu_startup_posts_echo_area_message() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("cached bootstrap evaluator");
    let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
    let frame_id = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after bootstrap")
        .id;
    configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

    run_gnu_startup(&mut eval);

    let forms = parse_forms(
        "(list (current-message)
                   (substring-no-properties (startup-echo-area-message)))",
    )
    .expect("parse startup echo probe");
    let result = eval
        .eval_expr(&forms[0])
        .expect("startup echo probe should evaluate");
    assert_eq!(
        print_value_with_eval(&mut eval, &result),
        "(nil \"For information about GNU Emacs and the GNU system, type C-h C-a.\")"
    );
}

#[test]
fn gnu_startup_keeps_single_row_minibuffer() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("cached bootstrap evaluator");
    let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
    let frame_id = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after bootstrap")
        .id;
    configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

    run_gnu_startup(&mut eval);

    let forms = parse_forms("(window-total-height (minibuffer-window))")
        .expect("parse minibuffer height probe");
    let result = eval
        .eval_expr(&forms[0])
        .expect("minibuffer height probe should evaluate");
    assert_eq!(result, Value::Int(1));
}

#[test]
fn gnu_startup_runtime_load_path_finds_mail_rfc6068() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("cached bootstrap evaluator");
    let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
    let frame_id = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after bootstrap")
        .id;
    configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

    run_gnu_startup(&mut eval);

    let forms =
        parse_forms("(locate-library \"rfc6068\")").expect("parse locate-library startup probe");
    let result = eval
        .eval_expr(&forms[0])
        .expect("locate-library startup probe should evaluate");
    let path = result
        .as_str()
        .expect("locate-library should return a resolved path string after startup");
    assert!(
        path.ends_with("/mail/rfc6068.el"),
        "expected GNU mail runtime path, got {path}"
    );
}

#[test]
fn gnu_startup_where_is_internal_finds_about_emacs_on_help_prefix() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("cached bootstrap evaluator");
    let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
    let frame_id = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after bootstrap")
        .id;
    configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

    run_gnu_startup(&mut eval);

    let forms = parse_forms(
        "(list
               (lookup-key help-map [1])
               (lookup-key (symbol-function 'help-command) [1])
               (lookup-key (current-global-map) [8])
               (lookup-key (current-global-map) [8 1]))",
    )
    .expect("parse startup help-prefix probe");
    let result = eval
        .eval_expr(&forms[0])
        .expect("startup help-prefix probe should evaluate");
    assert_eq!(
        print_value_with_eval(&mut eval, &result),
        "(about-emacs about-emacs help-command about-emacs)"
    );
}

#[test]
#[ignore = "startup echo helper blocks in this harness; message redisplay is covered in neovm-core"]
fn gnu_startup_requests_redisplay_for_echo_area_message() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("cached bootstrap evaluator");
    let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
    let frame_id = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after bootstrap")
        .id;
    configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

    let redisplay_rows = Arc::new(Mutex::new(Vec::<String>::new()));
    let redisplay_rows_capture = Arc::clone(&redisplay_rows);
    eval.redisplay_fn = Some(Box::new(move |eval: &mut Evaluator| {
        redisplay_rows_capture
            .lock()
            .expect("redisplay row buffer")
            .push(eval.current_message_text().unwrap_or_default().to_string());
    }));

    let forms = parse_forms("(display-startup-echo-area-message)")
        .expect("parse startup echo-area display form");
    let result = eval
        .eval_expr(&forms[0])
        .expect("display-startup-echo-area-message should evaluate");
    assert_eq!(
        result,
        Value::string("For information about GNU Emacs and the GNU system, type C-h C-a.")
    );

    let rendered_rows = redisplay_rows.lock().expect("captured redisplay rows");

    assert!(
        rendered_rows
            .iter()
            .any(|row| row.contains("For information about GNU Emacs and the GNU system")),
        "expected startup echo message during redisplay, got: {rendered_rows:?}"
    );
}

#[test]
fn gnu_startup_restores_meta_and_ctl_x_bindings() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("cached bootstrap evaluator");
    let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
    let frame_id = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after bootstrap")
        .id;
    configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

    run_gnu_startup(&mut eval);

    let forms = parse_forms(
        r#"(list
                 (key-binding (kbd "M-x"))
                 (lookup-key (current-global-map) (kbd "M-x"))
                 (key-binding (kbd "C-x 2"))
                 (lookup-key (current-global-map) (kbd "C-x 2"))
                 (key-binding (kbd "C-x 3"))
                 (lookup-key (current-global-map) (kbd "C-x 3")))"#,
    )
    .expect("parse startup keybinding probe");
    let result = eval
        .eval_expr(&forms[0])
        .expect("startup keybinding probe should evaluate");
    assert_eq!(
        print_value_with_eval(&mut eval, &result),
        "(execute-extended-command execute-extended-command split-window-below split-window-below split-window-right split-window-right)"
    );
}

#[test]
fn gnu_startup_formats_mode_line_for_target_window_buffer() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("cached bootstrap evaluator");
    let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
    let frame_id = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after bootstrap")
        .id;
    configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

    run_gnu_startup(&mut eval);

    let forms = parse_forms(
        r#"(let* ((w (selected-window))
                      (buf (window-buffer w))
                      (mini (minibuffer-window)))
                 (with-current-buffer (window-buffer mini)
                   (format-mode-line "%b" nil w buf)))"#,
    )
    .expect("parse startup mode-line probe");
    let result = eval
        .eval_expr(&forms[0])
        .expect("startup mode-line probe should evaluate");
    assert_eq!(result, Value::string("*scratch*"));
}

#[test]
fn gnu_startup_split_window_right_succeeds_on_opening_frame() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("cached bootstrap evaluator");
    let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
    let frame_id = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after bootstrap")
        .id;
    configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

    run_gnu_startup(&mut eval);

    let (expected_width, expected_height) = {
        let frame = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after startup");
        let selected = frame
            .selected_window()
            .expect("selected window after startup");
        let bounds = selected.bounds();
        (
            (bounds.width / frame.char_width) as i64,
            (bounds.height / frame.char_height) as i64,
        )
    };

    let forms = parse_forms(
        r#"(list
                 (window-total-width)
                 (window-total-height)
                 (window-min-size nil t)
                 (window-min-size nil nil)
                 (window-size-fixed-p (selected-window))
                 (window-size-fixed-p (selected-window) t)
                 (condition-case err
                     (progn (split-window-right) 'ok)
                   (error (list 'error (error-message-string err)))))"#,
    )
    .expect("parse startup split-window probe");
    let result = eval
        .eval_expr(&forms[0])
        .expect("startup split-window probe should evaluate");
    let items = list_to_vec(&result).expect("split-window result list");
    assert_eq!(items[0], Value::Int(expected_width));
    assert_eq!(items[1], Value::Int(expected_height));
    assert_eq!(items[2], Value::Int(10));
    assert_eq!(items[3], Value::Int(4));
    assert!(items[4].is_nil());
    assert!(items[5].is_nil());
    assert_eq!(items[6], Value::symbol("ok"));
}

#[test]
fn gnu_startup_split_window_below_succeeds_on_opening_frame() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("cached bootstrap evaluator");
    let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
    let frame_id = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after bootstrap")
        .id;
    configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

    run_gnu_startup(&mut eval);

    let (expected_width, expected_height) = {
        let frame = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after startup");
        let selected = frame
            .selected_window()
            .expect("selected window after startup");
        let bounds = selected.bounds();
        (
            (bounds.width / frame.char_width) as i64,
            (bounds.height / frame.char_height) as i64,
        )
    };

    let forms = parse_forms(
        r#"(list
                 (window-total-width)
                 (window-total-height)
                 (window-min-size nil t)
                 (window-min-size nil nil)
                 (window-size-fixed-p (selected-window))
                 (window-size-fixed-p (selected-window) t)
                 (condition-case err
                     (progn (split-window-below) 'ok)
                   (error (list 'error (error-message-string err)))))"#,
    )
    .expect("parse startup split-window probe");
    let result = eval
        .eval_expr(&forms[0])
        .expect("startup split-window probe should evaluate");
    let items = list_to_vec(&result).expect("split-window result list");
    assert_eq!(items[0], Value::Int(expected_width));
    assert_eq!(items[1], Value::Int(expected_height));
    assert_eq!(items[2], Value::Int(10));
    assert_eq!(items[3], Value::Int(4));
    assert!(items[4].is_nil());
    assert!(items[5].is_nil());
    assert_eq!(items[6], Value::symbol("ok"));
}

#[test]
fn gnu_startup_window_pixel_queries_use_live_frame_pixels() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("cached bootstrap evaluator");
    let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
    let frame_id = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after bootstrap")
        .id;
    configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

    run_gnu_startup(&mut eval);

    let forms = parse_forms(
        r#"(list
                 (window-pixel-width)
                 (window-pixel-height)
                 (window-body-width nil t)
                 (window-body-height nil t)
                 (window-text-width nil t)
                 (window-text-height nil t)
                 (window-fringes)
                 (window-edges nil nil nil t)
                 (window-edges nil t nil t))"#,
    )
    .expect("parse startup pixel probe");
    let result = eval
        .eval_expr(&forms[0])
        .expect("startup pixel probe should evaluate");
    let items = list_to_vec(&result).expect("pixel query result list");
    let pixel_width = items[0].as_int().expect("window-pixel-width");
    let pixel_height = items[1].as_int().expect("window-pixel-height");
    let body_width = items[2].as_int().expect("window-body-width");
    let body_height = items[3].as_int().expect("window-body-height");
    let text_width = items[4].as_int().expect("window-text-width");
    let text_height = items[5].as_int().expect("window-text-height");
    let fringes = list_to_vec(&items[6]).expect("window fringes");
    let outer_edges = list_to_vec(&items[7]).expect("outer window edges");
    let inner_edges = list_to_vec(&items[8]).expect("inner window edges");
    let left_fringe = fringes[0].as_int().expect("left fringe");
    let right_fringe = fringes[1].as_int().expect("right fringe");

    assert_eq!(pixel_width, 960);
    assert!(pixel_height > 0);
    assert_eq!(body_width, pixel_width - left_fringe - right_fringe);
    assert_eq!(text_width, body_width);
    assert_eq!(body_height, text_height);
    assert!(pixel_height >= body_height);
    assert_eq!(
        outer_edges,
        vec![
            Value::Int(0),
            Value::Int(0),
            Value::Int(pixel_width),
            Value::Int(pixel_height)
        ]
    );
    assert_eq!(
        inner_edges,
        vec![
            Value::Int(left_fringe),
            Value::Int(0),
            Value::Int(pixel_width - right_fringe),
            Value::Int(body_height)
        ]
    );
}

#[test]
fn gnu_startup_processes_load_option_from_forwarded_args() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("cached bootstrap evaluator");
    let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
    let frame_id = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after bootstrap")
        .id;
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate lives in workspace root");
    let face_test = repo_root.join("test/neomacs/neomacs-face-test.el");
    let startup = gui_startup_with_args(&[
        "-Q",
        "-l",
        face_test
            .to_str()
            .expect("face test path must be valid utf-8"),
    ]);
    configure_gnu_startup_state(&mut eval, frame_id, &startup);

    run_gnu_startup(&mut eval);

    let forms = parse_forms(
        r#"(list
                 (fboundp 'neomacs-face-test-write-matrix-report)
                 (buffer-live-p (get-buffer "*Neomacs Face Test*"))
                 (buffer-name (window-buffer (selected-window))))"#,
    )
    .expect("parse startup load-option probe");
    let result = eval
        .eval_expr(&forms[0])
        .expect("startup load-option probe should evaluate");
    let items = list_to_vec(&result).expect("load-option result list");
    assert_eq!(items[0], Value::True);
    assert_eq!(items[1], Value::True);
    assert_eq!(
        print_value_with_eval(&mut eval, &items[2]),
        "\"*Neomacs Face Test*\""
    );
}

#[test]
fn recursive_edit_processes_load_option_from_forwarded_args_before_first_input() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("cached bootstrap evaluator");
    let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
    let frame_id = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after bootstrap")
        .id;
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate lives in workspace root");
    let face_test = repo_root.join("test/neomacs/neomacs-face-test.el");
    let startup = gui_startup_with_args(&[
        "-Q",
        "-l",
        face_test
            .to_str()
            .expect("face test path must be valid utf-8"),
    ]);
    configure_gnu_startup_state(&mut eval, frame_id, &startup);

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(neovm_core::keyboard::InputEvent::CloseRequested)
        .expect("queue close request");
    drop(tx);
    let mut wake_pipe = [0; 2];
    let pipe_result = unsafe { libc::pipe(wake_pipe.as_mut_ptr()) };
    assert_eq!(pipe_result, 0, "pipe should initialize");
    eval.init_input_system(rx, wake_pipe[0]);

    let result = eval.recursive_edit();
    unsafe {
        libc::close(wake_pipe[0]);
        libc::close(wake_pipe[1]);
    }
    result.expect("close request should let the outer recursive edit exit cleanly");

    let forms = parse_forms(
        r#"(list
                 (fboundp 'neomacs-face-test-write-matrix-report)
                 (buffer-live-p (get-buffer "*Neomacs Face Test*"))
                 (buffer-name (window-buffer (selected-window))))"#,
    )
    .expect("parse recursive-edit load-option probe");
    let result = eval
        .eval_expr(&forms[0])
        .expect("recursive-edit load-option probe should evaluate");
    let items = list_to_vec(&result).expect("recursive-edit result list");
    assert_eq!(items[0], Value::True);
    assert_eq!(items[1], Value::True);
    assert_eq!(
        print_value_with_eval(&mut eval, &items[2]),
        "\"*Neomacs Face Test*\""
    );
}

#[test]
fn gnu_startup_next_line_moves_point_on_live_gui_frame() {
    let mut eval = create_bootstrap_evaluator_with_features(BOOTSTRAP_CORE_FEATURES)
        .expect("bootstrap evaluator");
    let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
    let frame_id = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after bootstrap")
        .id;
    configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

    run_gnu_startup(&mut eval);

    let forms = parse_forms(
        r#"(progn
             (switch-to-buffer "*scratch*")
             (erase-buffer)
             (insert "abc\ndef\nghi")
             (goto-char 1)
             (command-execute 'next-line)
             (point))"#,
    )
    .expect("parse startup next-line command");
    let result = eval
        .eval_expr(&forms[0])
        .expect("startup next-line should evaluate");
    assert_eq!(result, Value::Int(5));
}

#[test]
fn frame_set_background_mode_uses_live_gui_window_system_after_startup_clears_initial_flag() {
    let mut eval = create_bootstrap_evaluator_with_features(BOOTSTRAP_CORE_FEATURES)
        .expect("bootstrap evaluator");
    let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
    let frame_id = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after bootstrap")
        .id;
    configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());
    eval.set_variable("initial-window-system", Value::Nil);

    let forms = parse_forms(
        r#"(condition-case err
                  (progn
                    (frame-set-background-mode (selected-frame))
                    'ok)
                (error (list 'error (error-message-string err))))"#,
    )
    .expect("parse frame-set-background-mode probe");
    let result = eval
        .eval_expr(&forms[0])
        .expect("frame-set-background-mode probe should evaluate");
    assert_eq!(result, Value::symbol("ok"));
}
