use super::{
    BOOTSTRAP_CORE_FEATURES, BootstrapDisplayConfig, DumpImageKind, EarlyCliAction, FrontendKind,
    PrimaryWindowDisplayHost, PrimaryWindowSize, RuntimeMode, StartupOptions, TtyTerminalHost,
    adopt_existing_primary_gui_frame, bootstrap_buffers, bootstrap_default_font_name,
    bootstrap_display_config, bootstrap_frame_metrics, classify_early_cli_action,
    configure_gnu_startup_state, current_layout_frame_id, face_height_to_pixels,
    parse_startup_options, raw_loadup_command_line, raw_loadup_startup_surface, render_help_text,
    render_version_text, run_gnu_startup, sync_live_gui_frame_titles,
};
use neomacs_display_runtime::thread_comm::RenderCommand;
use neovm_core::emacs_core::Context;
use neovm_core::emacs_core::GuiFrameHostRequest;
use neovm_core::emacs_core::Value;
use neovm_core::emacs_core::load::{
    LoadupDumpMode, create_bootstrap_evaluator_cached_with_features,
    create_bootstrap_evaluator_with_features,
};
use neovm_core::emacs_core::print_value_with_eval;
use neovm_core::emacs_core::terminal::pure::TerminalHost;
use neovm_core::emacs_core::value::list_to_vec;
use neovm_core::face::FaceHeight;
use neovm_core::window::FrameId;
use std::path::Path;
use std::sync::{Arc, Mutex};

fn gui_display() -> BootstrapDisplayConfig {
    bootstrap_display_config(FrontendKind::Gui)
}

#[test]
fn runtime_mode_binary_names_match_gnu_shaped_roles() {
    assert_eq!(RuntimeMode::Raw.binary_name(), "neomacs-temacs");
    assert_eq!(RuntimeMode::BootstrapUse.binary_name(), "bootstrap-neomacs");
    assert_eq!(RuntimeMode::FinalRun.binary_name(), "neomacs");
}

#[test]
fn runtime_mode_dump_image_kinds_match_pipeline_roles() {
    assert_eq!(RuntimeMode::Raw.dump_image_kind(), None);
    assert_eq!(
        RuntimeMode::BootstrapUse.dump_image_kind(),
        Some(DumpImageKind::Bootstrap)
    );
    assert_eq!(
        RuntimeMode::FinalRun.dump_image_kind(),
        Some(DumpImageKind::Final)
    );
}

#[test]
fn bootstrap_gui_display_defaults_to_gnu_light_background_mode() {
    assert_eq!(gui_display().background_mode, "light");
}

fn gui_startup() -> StartupOptions {
    StartupOptions {
        frontend: FrontendKind::Gui,
        forwarded_args: vec!["neomacs".to_string()],
        terminal_device: None,
        noninteractive: false,
        temacs_mode: None,
        dump_file_override: None,
        no_site_lisp: false,
        no_loadup: false,
        no_build_details: false,
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
        temacs_mode: None,
        dump_file_override: None,
        no_site_lisp: false,
        no_loadup: false,
        no_build_details: false,
    }
}

fn tty_batch_startup_with_args(args: &[&str]) -> StartupOptions {
    let mut forwarded_args = vec!["neomacs".to_string()];
    forwarded_args.extend(args.iter().map(|arg| (*arg).to_string()));
    StartupOptions {
        frontend: FrontendKind::Tty,
        forwarded_args,
        terminal_device: None,
        noninteractive: true,
        temacs_mode: None,
        dump_file_override: None,
        no_site_lisp: false,
        no_loadup: false,
        no_build_details: false,
    }
}

#[test]
fn parse_startup_options_accepts_gnu_temacs_modes() {
    let startup = parse_startup_options([
        "neomacs-temacs".to_string(),
        "--temacs=pbootstrap".to_string(),
        "--batch".to_string(),
    ])
    .expect("startup options should parse");
    assert_eq!(startup.temacs_mode, Some(LoadupDumpMode::Pbootstrap));

    let startup = parse_startup_options([
        "neomacs-temacs".to_string(),
        "--temacs".to_string(),
        "pdump".to_string(),
    ])
    .expect("startup options should parse");
    assert_eq!(startup.temacs_mode, Some(LoadupDumpMode::Pdump));
}

#[test]
fn parse_startup_options_accepts_dump_file_override() {
    let startup = parse_startup_options([
        "neomacs".to_string(),
        "--dump-file=/tmp/custom.pdump".to_string(),
    ])
    .expect("startup options should parse");
    assert_eq!(
        startup.dump_file_override,
        Some(std::path::PathBuf::from("/tmp/custom.pdump"))
    );
}

#[test]
fn parse_startup_options_consumes_chdir_flag_and_changes_cwd() {
    // GNU emacs.c:1538-1561 — `--chdir DIR` calls chdir(DIR) before
    // any later parsing or file resolution. The flag is consumed (not
    // forwarded) and a chdir failure aborts startup.
    //
    // nextest runs each #[test] in its own process so the cwd mutation
    // does not leak into sibling tests.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let canonical = std::fs::canonicalize(tmp.path()).expect("canonicalize tempdir");

    let startup = parse_startup_options([
        "neomacs".to_string(),
        "--chdir".to_string(),
        canonical.to_string_lossy().into_owned(),
    ])
    .expect("startup options should parse");

    let cwd = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
    assert_eq!(cwd, canonical);
    // The flag must NOT appear in forwarded_args — GNU consumes it.
    assert!(
        !startup
            .forwarded_args
            .iter()
            .any(|a| a == "--chdir" || a == "-chdir"),
        "--chdir should be consumed, not forwarded: {:?}",
        startup.forwarded_args
    );
}

#[test]
fn parse_startup_options_chdir_inline_value_form_works() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let canonical = std::fs::canonicalize(tmp.path()).expect("canonicalize tempdir");

    let startup = parse_startup_options([
        "neomacs".to_string(),
        format!("--chdir={}", canonical.display()),
    ])
    .expect("startup options should parse");

    let cwd = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
    assert_eq!(cwd, canonical);
    assert!(
        !startup
            .forwarded_args
            .iter()
            .any(|a| a.starts_with("--chdir")),
        "--chdir=… should be consumed, not forwarded: {:?}",
        startup.forwarded_args
    );
}

#[test]
fn parse_startup_options_chdir_to_nonexistent_dir_errors() {
    // GNU emacs.c:1551 — `Can't chdir to %s: %s`. We match the prefix
    // but use Rust's std::io::Error message for the suffix.
    let err = parse_startup_options([
        "neomacs".to_string(),
        "--chdir".to_string(),
        "/this/path/cannot/possibly/exist".to_string(),
    ])
    .expect_err("chdir to nonexistent should fail");
    assert!(
        err.starts_with("neomacs: Can't chdir to /this/path/cannot/possibly/exist"),
        "unexpected error message: {err}"
    );
}

#[test]
fn parse_startup_options_chdir_missing_value_errors() {
    let err = parse_startup_options(["neomacs".to_string(), "--chdir".to_string()])
        .expect_err("chdir without value should fail");
    assert!(
        err.contains("requires an argument"),
        "expected requires-argument error, got: {err}"
    );
}

#[test]
fn parse_startup_options_consumes_script_flag_with_rewrite() {
    // GNU emacs.c:1708-1717: --script FILE sets noninteractive and
    // rewrites the matched flag to -scriptload (an internal flag that
    // lisp/startup.el:2841 understands). The user's FILE follows
    // -scriptload in argv.
    let startup = parse_startup_options([
        "neomacs".to_string(),
        "--script".to_string(),
        "/tmp/foo.el".to_string(),
    ])
    .expect("startup options should parse");

    assert!(startup.noninteractive, "--script must imply noninteractive");
    assert_eq!(startup.frontend, FrontendKind::Tty);
    // The original --script flag must NOT appear in forwarded_args.
    assert!(
        !startup
            .forwarded_args
            .iter()
            .any(|a| a == "--script" || a == "-script"),
        "--script should be rewritten away: {:?}",
        startup.forwarded_args
    );
    // -scriptload FILE must be present in the right order.
    let pos = startup
        .forwarded_args
        .iter()
        .position(|a| a == "-scriptload")
        .expect("-scriptload should be in forwarded_args");
    assert_eq!(
        startup.forwarded_args.get(pos + 1).map(String::as_str),
        Some("/tmp/foo.el"),
        "FILE should follow -scriptload"
    );
}

#[test]
fn parse_startup_options_script_missing_value_errors() {
    let err = parse_startup_options(["neomacs".to_string(), "--script".to_string()])
        .expect_err("--script with no value should fail");
    assert!(
        err.contains("requires an argument"),
        "expected requires-argument error, got: {err}"
    );
}

#[test]
fn parse_startup_options_consumes_dash_x_with_scripteval_rewrite() {
    // GNU emacs.c:2132-2140: -x sets noninteractive AND no_site_lisp,
    // and rewrites the matched flag to -scripteval (internal flag for
    // shebang-style #!/usr/bin/neomacs -x scripts).
    let startup = parse_startup_options(["neomacs".to_string(), "-x".to_string()])
        .expect("startup options should parse");

    assert!(startup.noninteractive, "-x must imply noninteractive");
    assert!(startup.no_site_lisp, "-x must imply no-site-lisp");
    assert_eq!(startup.frontend, FrontendKind::Tty);
    assert!(
        !startup.forwarded_args.iter().any(|a| a == "-x"),
        "-x should be rewritten away: {:?}",
        startup.forwarded_args
    );
    assert!(
        startup.forwarded_args.iter().any(|a| a == "-scripteval"),
        "-scripteval should be in forwarded_args: {:?}",
        startup.forwarded_args
    );
}

#[test]
fn parse_startup_options_consumes_no_loadup_flag() {
    // GNU emacs.c:2031-2032: --no-loadup sets no_loadup, which gates the
    // -l loadup splice in main(). Consumed entirely; not forwarded.
    let startup = parse_startup_options(["neomacs".to_string(), "--no-loadup".to_string()])
        .expect("startup options should parse");
    assert!(startup.no_loadup);
    assert!(
        !startup
            .forwarded_args
            .iter()
            .any(|a| a == "--no-loadup" || a == "-nl"),
        "--no-loadup should be consumed"
    );
}

#[test]
fn parse_startup_options_consumes_short_nl_flag() {
    let startup = parse_startup_options(["neomacs".to_string(), "-nl".to_string()])
        .expect("startup options should parse");
    assert!(startup.no_loadup);
}

#[test]
fn raw_loadup_command_line_skips_loadup_splice_when_no_loadup_set() {
    // The user-visible effect of --no-loadup at RuntimeMode::Raw: the
    // synthetic `-l loadup` splice is omitted, mirroring GNU
    // emacs.c:2578 `if (!no_loadup) ... loadup.el`.
    let startup = parse_startup_options([
        "neomacs-temacs".to_string(),
        "--no-loadup".to_string(),
        "--temacs=pdump".to_string(),
    ])
    .expect("startup options should parse");
    let argv = raw_loadup_command_line(&startup, Some(LoadupDumpMode::Pdump));
    assert!(
        !argv.windows(2).any(|w| w[0] == "-l" && w[1] == "loadup"),
        "loadup splice should be skipped: {argv:?}"
    );
}

#[test]
fn parse_startup_options_consumes_no_site_lisp_flag() {
    // GNU emacs.c:2034-2035: --no-site-lisp sets no_site_lisp.
    let startup = parse_startup_options(["neomacs".to_string(), "--no-site-lisp".to_string()])
        .expect("startup options should parse");
    assert!(startup.no_site_lisp);
    assert!(
        !startup
            .forwarded_args
            .iter()
            .any(|a| a == "--no-site-lisp" || a == "-nsl"),
        "--no-site-lisp should be consumed"
    );
}

#[test]
fn parse_startup_options_consumes_short_nsl_flag() {
    let startup = parse_startup_options(["neomacs".to_string(), "-nsl".to_string()])
        .expect("startup options should parse");
    assert!(startup.no_site_lisp);
}

#[test]
fn parse_startup_options_consumes_no_build_details_flag() {
    // GNU emacs.c:2037-2038: --no-build-details inverts build_details.
    let startup = parse_startup_options(["neomacs".to_string(), "--no-build-details".to_string()])
        .expect("startup options should parse");
    assert!(startup.no_build_details);
    assert!(
        !startup
            .forwarded_args
            .iter()
            .any(|a| a == "--no-build-details" || a == "-no-build-details"),
        "--no-build-details should be consumed"
    );
}

#[test]
fn parse_startup_options_peeks_q_to_set_no_site_lisp() {
    // GNU emacs.c:2126-2129 — `-Q` is peeked: it sets no_site_lisp=1
    // AND remains in argv so lisp/startup.el's command-line at
    // lisp/startup.el:1404 can also process it. We mirror both halves.
    let startup = parse_startup_options(["neomacs".to_string(), "-Q".to_string()])
        .expect("startup options should parse");
    assert!(startup.no_site_lisp, "-Q peek should set no_site_lisp");
    assert!(
        startup.forwarded_args.iter().any(|a| a == "-Q"),
        "-Q must remain in forwarded_args after peek: {:?}",
        startup.forwarded_args
    );
}

#[test]
fn parse_startup_options_peeks_long_quick_alias() {
    // GNU emacs.c:2126-2127 — `--quick` and `-quick` are equivalent
    // peek aliases for -Q. The -quick spelling matches the same
    // STANDARD_ARGS row that `-Q` does (priority 55).
    for spelling in &["--quick", "-quick"] {
        let startup = parse_startup_options(["neomacs".to_string(), (*spelling).to_string()])
            .expect("startup options should parse");
        assert!(
            startup.no_site_lisp,
            "{spelling} peek should set no_site_lisp"
        );
        assert!(
            startup.forwarded_args.iter().any(|a| a == spelling),
            "{spelling} must remain in forwarded_args after peek: {:?}",
            startup.forwarded_args
        );
    }
}

#[test]
fn parse_startup_options_q_peek_redundant_when_nsl_already_set() {
    // GNU emacs.c:2123 has an `if (! no_site_lisp)` guard around the
    // peek block. Once -nsl has set the flag, peeking -Q is a no-op
    // for state but the -Q token still remains in forwarded_args.
    let startup = parse_startup_options([
        "neomacs".to_string(),
        "--no-site-lisp".to_string(),
        "-Q".to_string(),
    ])
    .expect("startup options should parse");
    assert!(startup.no_site_lisp);
    assert!(startup.forwarded_args.iter().any(|a| a == "-Q"));
    // --no-site-lisp itself was consumed (Phase 3c).
    assert!(!startup.forwarded_args.iter().any(|a| a == "--no-site-lisp"));
}

#[test]
fn parse_startup_options_normalizes_display_args_to_gnu_form() {
    // GNU emacs.c:2110-2120 rewrites `--display=NAME` into the
    // equivalent `-d NAME` two-token form before passing argv on to
    // `lisp/startup.el`. We mirror that normalization in `parse_startup_options`
    // so the Lisp side observes the same shape under both implementations.
    // Other flags like `-Q` flow through unchanged.
    let startup = parse_startup_options([
        "neomacs".to_string(),
        "--display=:1".to_string(),
        "-Q".to_string(),
    ])
    .expect("startup options should parse");

    assert_eq!(
        startup.forwarded_args,
        vec![
            "neomacs".to_string(),
            "-d".to_string(),
            ":1".to_string(),
            "-Q".to_string()
        ]
    );
}

#[test]
fn raw_loadup_command_line_inserts_internal_loadup_marker() {
    // Phase 2 added sort_args to parse_startup_options, so flags now
    // appear in GNU's standard_args[] priority order regardless of how
    // they were typed. -Q (priority 55) sits ahead of --temacs / --dump-file
    // (priority 1). The -l loadup splice from raw_loadup_command_line
    // is then prepended.
    let startup = parse_startup_options([
        "neomacs-temacs".to_string(),
        "--temacs=pdump".to_string(),
        "--dump-file=/tmp/custom.pdump".to_string(),
        "-Q".to_string(),
    ])
    .expect("startup options should parse");

    assert_eq!(
        raw_loadup_command_line(&startup, Some(LoadupDumpMode::Pdump)),
        vec![
            "neomacs-temacs".to_string(),
            "-l".to_string(),
            "loadup".to_string(),
            "-Q".to_string(),
            "--temacs=pdump".to_string(),
            "--dump-file=/tmp/custom.pdump".to_string(),
        ]
    );
}

#[test]
fn raw_loadup_startup_surface_forces_noninteractive_dump_bootstrap() {
    let startup = parse_startup_options([
        "neomacs-temacs".to_string(),
        "--temacs=pbootstrap".to_string(),
    ])
    .expect("startup options should parse");

    let surface = raw_loadup_startup_surface(&startup, Some(LoadupDumpMode::Pbootstrap));
    assert!(surface.noninteractive);
    assert_eq!(
        surface.command_line_args,
        vec![
            "neomacs-temacs".to_string(),
            "-l".to_string(),
            "loadup".to_string(),
            "--temacs=pbootstrap".to_string(),
        ]
    );
}

fn bootstrap_runtime_gui_startup(eval: &mut Context) -> FrameId {
    let _bootstrap = bootstrap_buffers(eval, 960, 640, gui_display());
    let frame_id = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after bootstrap")
        .id;
    configure_gnu_startup_state(eval, frame_id, &gui_startup());
    frame_id
}

fn eval_after_gnu_gui_startup(source: &str) -> String {
    let mut eval = create_bootstrap_evaluator_cached_with_features(BOOTSTRAP_CORE_FEATURES)
        .expect("bootstrap evaluator");
    let _frame_id = bootstrap_runtime_gui_startup(&mut eval);
    run_gnu_startup(&mut eval);

    let result = eval.eval_str(source).expect("probe should evaluate");
    print_value_with_eval(&mut eval, &result)
}

#[test]
fn bootstrap_buffers_realize_default_face_from_frame_font_parameter() {
    let mut eval = create_bootstrap_evaluator_with_features(BOOTSTRAP_CORE_FEATURES)
        .expect("bootstrap evaluator");
    let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
    let default = eval.face_table().get("default").expect("default face");
    assert_eq!(default.family.as_deref(), Some("Hack"));
    assert_eq!(default.weight.map(|weight| weight.0), Some(400));
    assert_eq!(default.height, Some(FaceHeight::Absolute(100)));
}

#[test]
fn opening_gui_frame_adoption_does_not_push_stale_window_size() {
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let mut host = PrimaryWindowDisplayHost {
        cmd_tx,
        primary_window_adopted: false,
        primary_frame_id: None,
        last_window_titles: Mutex::new(std::collections::HashMap::new()),
        font_metrics: None,
        primary_window_size: Arc::new(Mutex::new(PrimaryWindowSize {
            width: 1600,
            height: 1800,
        })),
        image_dimensions: Arc::new((
            Mutex::new(std::collections::HashMap::new()),
            std::sync::Condvar::new(),
        )),
        resolved_images: Mutex::new(std::collections::HashMap::new()),
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
fn bootstrap_gui_frame_adoption_routes_future_resizes_to_primary_window() {
    let mut eval = Context::new();
    let _bootstrap = bootstrap_buffers(&mut eval, 843, 489, gui_display());
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();

    eval.set_display_host(Box::new(PrimaryWindowDisplayHost {
        cmd_tx,
        primary_window_adopted: false,
        primary_frame_id: None,
        last_window_titles: Mutex::new(std::collections::HashMap::new()),
        font_metrics: None,
        primary_window_size: Arc::new(Mutex::new(PrimaryWindowSize {
            width: 843,
            height: 489,
        })),
        image_dimensions: Arc::new((
            Mutex::new(std::collections::HashMap::new()),
            std::sync::Condvar::new(),
        )),
        resolved_images: Mutex::new(std::collections::HashMap::new()),
    }));

    adopt_existing_primary_gui_frame(&mut eval).expect("bootstrap GUI frame should adopt");
    eval.eval_str("(set-frame-size (selected-frame) 132 42)")
        .expect("set-frame-size should succeed");

    let commands: Vec<_> = cmd_rx.try_iter().collect();
    assert!(
        commands
            .iter()
            .any(|cmd| matches!(cmd, RenderCommand::SetWindowTitle { .. })),
        "expected bootstrap adoption to set the primary window title, got {commands:?}"
    );
    assert!(
        commands.iter().any(|cmd| matches!(
            cmd,
            RenderCommand::ResizeWindow {
                emacs_frame_id: 0,
                ..
            }
        )),
        "expected bootstrap resize to target the adopted primary window, got {commands:?}"
    );
}

#[test]
fn redisplay_title_sync_formats_frame_title_format_for_primary_window() {
    let mut eval = Context::new();
    let _bootstrap = bootstrap_buffers(&mut eval, 843, 489, gui_display());
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();

    eval.set_display_host(Box::new(PrimaryWindowDisplayHost {
        cmd_tx,
        primary_window_adopted: false,
        primary_frame_id: None,
        last_window_titles: Mutex::new(std::collections::HashMap::new()),
        font_metrics: None,
        primary_window_size: Arc::new(Mutex::new(PrimaryWindowSize {
            width: 843,
            height: 489,
        })),
        image_dimensions: Arc::new((
            Mutex::new(std::collections::HashMap::new()),
            std::sync::Condvar::new(),
        )),
        resolved_images: Mutex::new(std::collections::HashMap::new()),
    }));

    adopt_existing_primary_gui_frame(&mut eval).expect("bootstrap GUI frame should adopt");
    let _ = cmd_rx.try_iter().collect::<Vec<_>>();

    eval.eval_str(r#"(setq frame-title-format "oracle-title")"#)
        .expect("frame-title-format should set");
    sync_live_gui_frame_titles(&mut eval);

    let commands: Vec<_> = cmd_rx.try_iter().collect();
    assert!(
        commands.iter().any(|cmd| matches!(
            cmd,
            RenderCommand::SetFrameWindowTitle {
                emacs_frame_id: 0,
                title
            } if title == "oracle-title"
        )),
        "expected redisplay title sync to publish the formatted primary title, got {commands:?}"
    );
}

#[test]
fn tty_terminal_host_delete_terminal_sends_shutdown() {
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let mut host = TtyTerminalHost { cmd_tx };

    host.delete_terminal()
        .expect("delete terminal should succeed");

    match cmd_rx
        .try_recv()
        .expect("shutdown command should be queued")
    {
        RenderCommand::Shutdown => {}
        other => panic!("expected Shutdown, got {other:?}"),
    }
}

#[test]
fn current_layout_frame_follows_selected_frame() {
    let mut eval = Context::new();
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
    let mut eval = Context::new();
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
    let mut eval = Context::new();
    let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
    let frame_id = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after bootstrap")
        .id;
    configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

    let terminal_frame = *eval
        .obarray()
        .symbol_value("terminal-frame")
        .expect("terminal-frame");
    let Some(terminal_frame_id) = terminal_frame.as_frame_id() else {
        panic!("GUI startup should seed a hidden terminal frame, got {terminal_frame:?}");
    };
    let terminal_frame_id = FrameId(terminal_frame_id);
    let terminal_frame = eval
        .frame_manager()
        .get(terminal_frame_id)
        .expect("hidden terminal frame");

    assert_eq!(
        terminal_frame.visible, false,
        "GNU frame-initialize should delete a hidden terminal frame, not the opening GUI frame"
    );
    assert!(
        terminal_frame.effective_window_system().is_none(),
        "hidden startup terminal frame must stay non-GUI"
    );
    assert!(
        !terminal_frame.parameters.contains_key("display-type"),
        "hidden startup terminal frame must not inherit GUI face parameters"
    );
    assert!(
        !terminal_frame.parameters.contains_key("background-mode"),
        "hidden startup terminal frame must not inherit GUI face parameters"
    );
    assert_eq!(
        eval.obarray()
            .symbol_value("frame-initial-frame")
            .and_then(|value| value.as_frame_id()),
        Some(frame_id.0)
    );
    assert_eq!(
        eval.obarray().symbol_value("frame-initial-frame-alist"),
        Some(&Value::list(vec![Value::cons(
            Value::symbol("window-system"),
            Value::symbol("neo"),
        )]))
    );
    assert_eq!(
        eval.obarray()
            .symbol_value("default-minibuffer-frame")
            .and_then(|value| value.as_frame_id()),
        Some(frame_id.0)
    );
}

#[test]
fn configure_gnu_startup_state_reports_neo_window_system_for_gui_boots() {
    let mut eval = Context::new();
    configure_gnu_startup_state(&mut eval, FrameId(42), &gui_startup());

    assert_eq!(
        eval.obarray().symbol_value("window-system"),
        Some(&Value::symbol("neo"))
    );
    assert_eq!(
        eval.obarray().symbol_value("initial-window-system"),
        Some(&Value::symbol("neo"))
    );
}

#[test]
fn cl_generic_context_dispatch_uses_neo_window_system_method() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("cached bootstrap evaluator");
    let rendered = eval
        .eval_str(
            r#"
        (progn
          (cl-defgeneric neomacs--ctx-probe ())
          (cl-defmethod neomacs--ctx-probe (&context (window-system nil)) 'tty)
          (cl-defmethod neomacs--ctx-probe (&context (window-system neo)) 'neo)
          (let ((window-system 'neo))
            (neomacs--ctx-probe)))
        "#,
        )
        .map(|value| print_value_with_eval(&mut eval, &value))
        .unwrap_or_else(|err| format!("{err:?}"));
    assert_eq!(rendered, "neo");
}

#[test]
fn pdump_preserves_neo_term_generic_methods() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("cached bootstrap evaluator");

    let pre = eval
        .eval_str(
            r#"
        (let ((window-system 'neo))
          (window-system-initialization)
          neomacs-initialized)
        "#,
        )
        .map(|value| print_value_with_eval(&mut eval, &value))
        .unwrap_or_else(|err| format!("{err:?}"));

    let post = eval
        .eval_str(
            r#"
        (progn
          (load "term/neo-win" nil t)
          (setq neomacs-initialized nil)
          (let ((window-system 'neo))
            (window-system-initialization)
            neomacs-initialized))
        "#,
        )
        .map(|value| print_value_with_eval(&mut eval, &value))
        .unwrap_or_else(|err| format!("{err:?}"));

    assert_eq!(
        pre, "t",
        "runtime pdump lost neo generic methods before reload"
    );
    assert_eq!(
        post, "t",
        "reloading term/neo-win should keep neo init working"
    );
}

#[test]
fn configure_gnu_startup_state_clears_window_system_for_tty_boots() {
    let mut eval = Context::new();
    let startup = StartupOptions {
        frontend: FrontendKind::Tty,
        forwarded_args: vec!["neomacs".to_string(), "-q".to_string()],
        terminal_device: Some("/dev/tty".to_string()),
        noninteractive: false,
        temacs_mode: None,
        dump_file_override: None,
        no_site_lisp: false,
        no_loadup: false,
        no_build_details: false,
    };
    configure_gnu_startup_state(&mut eval, FrameId(7), &startup);

    assert_eq!(
        eval.obarray().symbol_value("window-system"),
        Some(&Value::NIL)
    );
    assert_eq!(
        eval.obarray().symbol_value("initial-window-system"),
        Some(&Value::NIL)
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
    let mut eval = Context::new();
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
        temacs_mode: None,
        dump_file_override: None,
        no_site_lisp: false,
        no_loadup: false,
        no_build_details: false,
    };
    configure_gnu_startup_state(&mut eval, FrameId(9), &startup);

    assert_eq!(
        eval.obarray().symbol_value("noninteractive"),
        Some(&Value::T)
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
    let mut eval = Context::new();
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
    let mut eval = Context::new();
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
fn bootstrap_frame_metrics_uses_default_face_height_pixels() {
    let metrics = bootstrap_frame_metrics();
    assert_eq!(metrics.font_pixel_size, face_height_to_pixels(100));
}

#[test]
fn bootstrap_default_font_name_uses_pixel_size_field() {
    let mut eval = Context::new();
    let font_pixel_size = face_height_to_pixels(100);
    let font_name = bootstrap_default_font_name(font_pixel_size);
    let rendered = print_value_with_eval(&mut eval, &font_name);
    assert!(rendered.contains(&format!("-*-{}-", font_pixel_size.round() as i64)));
    assert!(rendered.contains("-regular-"));
}

#[test]
fn bootstrap_buffers_reuses_selected_startup_frame_when_one_already_exists() {
    let metrics = bootstrap_frame_metrics();
    let mut eval = Context::new();
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
        Some(Value::symbol("neo"))
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
    let mut eval = Context::new();
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
        Some(Value::symbol("neo"))
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

    eval.eval_str(
        r#"
        (progn
          (setq neomacs--probe-handle-args-called nil)
          (setq neomacs--probe-window-system-init-called nil)
          (setq neomacs--probe-frame-initialize-called nil)
          (setq neomacs--probe-normal-top-level-called nil)
          (setq neomacs--probe-command-line-called nil)
          (setq neomacs--probe-top-level-before top-level)
          (setq neomacs--orig-handle-args-function
                (symbol-function 'handle-args-function))
          (setq neomacs--orig-window-system-initialization
                (symbol-function 'window-system-initialization))
          (setq neomacs--orig-frame-initialize
                (symbol-function 'frame-initialize))
          (setq neomacs--orig-normal-top-level
                (symbol-function 'normal-top-level))
          (setq neomacs--orig-command-line
                (symbol-function 'command-line))
          (fset 'handle-args-function
                (lambda (args)
                  (setq neomacs--probe-handle-args-called t)
                  (funcall neomacs--orig-handle-args-function args)))
          (fset 'window-system-initialization
                (lambda (&optional display)
                  (setq neomacs--probe-window-system-init-called t)
                  (funcall neomacs--orig-window-system-initialization display)))
          (fset 'frame-initialize
                (lambda ()
                  (setq neomacs--probe-frame-initialize-called t)
                  (funcall neomacs--orig-frame-initialize)))
          (fset 'normal-top-level
                (lambda ()
                  (setq neomacs--probe-normal-top-level-called t)
                  (funcall neomacs--orig-normal-top-level)))
          (fset 'command-line
                (lambda (&rest args)
                  (setq neomacs--probe-command-line-called t)
                  (apply neomacs--orig-command-line args))))
        "#,
    )
    .expect("startup hook probe should install");

    run_gnu_startup(&mut eval);

    let startup_probe = eval
        .eval_str(
            r#"
         (list
         (current-message)
         noninteractive
         window-system
         initial-window-system
         (featurep 'neo-win)
         (featurep 'term/neo-win)
         (featurep 'x-win)
         (daemonp)
         command-line-processed
         neomacs--probe-top-level-before
         neomacs--probe-normal-top-level-called
         neomacs--probe-command-line-called
         neomacs--probe-handle-args-called
         neomacs--probe-window-system-init-called
         neomacs--probe-frame-initialize-called
         neomacs-initialized
         (get 'neo 'window-system-initialized)
         frame-initial-frame
         neomacs--startup-last-phase
         neomacs--startup-last-call
         terminal-frame
         (mapcar
          (lambda (frame)
            (list frame
                  (frame-parameter frame 'window-system)
                  (frame-parameter frame 'display-type)
                  (frame-parameter frame 'background-mode)
                  (frame-visible-p frame)
                  (eq frame terminal-frame)
                  (eq frame frame-initial-frame)
                  (eq frame (selected-frame))
                  (eq frame (window-frame (minibuffer-window frame)))))
          (frame-list)))
        "#,
        )
        .expect("startup probe should evaluate");
    let shutdown_request = eval.shutdown_request();
    let frame_ids: Vec<_> = eval.frame_manager().frame_list().into_iter().collect();
    let selected_frame_id = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after startup")
        .id;
    assert_eq!(
        frame_ids,
        vec![frame_id],
        "startup probe={} shutdown_request={shutdown_request:?}",
        print_value_with_eval(&mut eval, &startup_probe),
    );
    assert_eq!(
        selected_frame_id,
        frame_id,
        "startup probe={} shutdown_request={shutdown_request:?}",
        print_value_with_eval(&mut eval, &startup_probe),
    );
}

#[test]
fn bootstrap_gui_state_allows_gnu_frame_initialize_to_delete_terminal_frame() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("cached bootstrap evaluator");
    let frame_id = bootstrap_runtime_gui_startup(&mut eval);

    eval.eval_str("(frame-initialize)")
        .expect("frame-initialize should succeed on bootstrap gui state");

    let frame_ids: Vec<_> = eval.frame_manager().frame_list().into_iter().collect();
    assert_eq!(frame_ids, vec![frame_id]);
    assert_eq!(
        eval.obarray().symbol_value("terminal-frame"),
        Some(&Value::NIL)
    );
}

#[test]
fn gnu_startup_keeps_scratch_text_accessible_under_q_startup() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("cached bootstrap evaluator");
    let _frame_id = bootstrap_runtime_gui_startup(&mut eval);

    run_gnu_startup(&mut eval);

    let result = eval
        .eval_str(
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

    let result = eval
        .eval_str("(query-fontset \"fontset-default\")")
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

    let result = eval
        .eval_str(
            r#"(list (current-message)
                   (substring-no-properties (startup-echo-area-message)))"#,
        )
        .expect("startup echo probe should evaluate");
    assert_eq!(
        print_value_with_eval(&mut eval, &result),
        "(\"For information about GNU Emacs and the GNU system, type C-h C-a.\" \"For information about GNU Emacs and the GNU system, type C-h C-a.\")"
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

    let result = eval
        .eval_str("(window-total-height (minibuffer-window))")
        .expect("minibuffer height probe should evaluate");
    assert_eq!(result, Value::fixnum(1));
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

    let result = eval
        .eval_str("(locate-library \"rfc6068\")")
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

    let result = eval
        .eval_str(
            r#"(list
               (lookup-key help-map [1])
               (lookup-key (symbol-function 'help-command) [1])
               (lookup-key (current-global-map) [8])
               (lookup-key (current-global-map) [8 1]))"#,
        )
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
    eval.redisplay_fn = Some(Box::new(move |eval: &mut Context| {
        redisplay_rows_capture
            .lock()
            .expect("redisplay row buffer")
            .push(eval.current_message_text().unwrap_or_default().to_string());
    }));

    let result = eval
        .eval_str("(display-startup-echo-area-message)")
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

    let result = eval
        .eval_str(
            r#"(list
                 (key-binding (kbd "M-x"))
                 (lookup-key (current-global-map) (kbd "M-x"))
                 (key-binding (kbd "C-x 2"))
                 (lookup-key (current-global-map) (kbd "C-x 2"))
                 (key-binding (kbd "C-x 3"))
                 (lookup-key (current-global-map) (kbd "C-x 3")))"#,
        )
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

    let result = eval
        .eval_str(
            r#"(let* ((w (selected-window))
                      (buf (window-buffer w))
                      (mini (minibuffer-window)))
                 (with-current-buffer (window-buffer mini)
                   (format-mode-line "%b" nil w buf)))"#,
        )
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

    let result = eval
        .eval_str(
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
        .expect("startup split-window probe should evaluate");
    let items = list_to_vec(&result).expect("split-window result list");
    assert_eq!(items[0], Value::fixnum(expected_width));
    assert_eq!(items[1], Value::fixnum(expected_height));
    assert_eq!(items[2], Value::fixnum(10));
    assert_eq!(items[3], Value::fixnum(4));
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

    let result = eval
        .eval_str(
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
        .expect("startup split-window probe should evaluate");
    let items = list_to_vec(&result).expect("split-window result list");
    assert_eq!(items[0], Value::fixnum(expected_width));
    assert_eq!(items[1], Value::fixnum(expected_height));
    assert_eq!(items[2], Value::fixnum(10));
    assert_eq!(items[3], Value::fixnum(4));
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

    let result = eval
        .eval_str(
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
            Value::fixnum(0),
            Value::fixnum(0),
            Value::fixnum(pixel_width),
            Value::fixnum(pixel_height)
        ]
    );
    assert_eq!(
        inner_edges,
        vec![
            Value::fixnum(left_fringe),
            Value::fixnum(0),
            Value::fixnum(pixel_width - right_fringe),
            Value::fixnum(body_height)
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

    let result = eval
        .eval_str(
            r#"(list
                 (fboundp 'neomacs-face-test-write-matrix-report)
                 (buffer-live-p (get-buffer "*Neomacs Face Test*"))
                 (buffer-name (window-buffer (selected-window))))"#,
        )
        .expect("startup load-option probe should evaluate");
    let items = list_to_vec(&result).expect("load-option result list");
    assert_eq!(items[0], Value::T);
    assert_eq!(items[1], Value::T);
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
    tx.send(neovm_core::keyboard::InputEvent::WindowClose {
        emacs_frame_id: u64::MAX,
    })
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

    let result = eval
        .eval_str(
            r#"(list
                 (fboundp 'neomacs-face-test-write-matrix-report)
                 (buffer-live-p (get-buffer "*Neomacs Face Test*"))
                 (buffer-name (window-buffer (selected-window))))"#,
        )
        .expect("recursive-edit load-option probe should evaluate");
    let items = list_to_vec(&result).expect("recursive-edit result list");
    assert_eq!(items[0], Value::T);
    assert_eq!(items[1], Value::T);
    assert_eq!(
        print_value_with_eval(&mut eval, &items[2]),
        "\"*Neomacs Face Test*\""
    );
}

#[test]
fn bootstrap_batch_eval_exits_outer_command_loop_like_gnu() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(BOOTSTRAP_CORE_FEATURES)
        .expect("bootstrap evaluator");
    let _bootstrap = bootstrap_buffers(
        &mut eval,
        80,
        24,
        bootstrap_display_config(FrontendKind::Tty),
    );
    let frame_id = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame after bootstrap")
        .id;
    let startup = tty_batch_startup_with_args(&["-Q", "--eval", "(setq neomacs--batch-probe 42)"]);
    configure_gnu_startup_state(&mut eval, frame_id, &startup);

    let (_tx, rx) = crossbeam_channel::unbounded();
    let mut wake_pipe = [0; 2];
    let pipe_result = unsafe { libc::pipe(wake_pipe.as_mut_ptr()) };
    assert_eq!(pipe_result, 0, "pipe should initialize");
    eval.init_input_system(rx, wake_pipe[0]);

    let result = eval.recursive_edit();
    unsafe {
        libc::close(wake_pipe[0]);
        libc::close(wake_pipe[1]);
    }
    result.expect("batch recursive edit should exit cleanly");

    assert_eq!(
        eval.shutdown_request(),
        Some(neovm_core::emacs_core::eval::ShutdownRequest {
            exit_code: 0,
            restart: false,
        })
    );
    assert_eq!(
        eval.obarray().symbol_value("neomacs--batch-probe"),
        Some(&Value::fixnum(42))
    );
    assert_eq!(
        eval.obarray().symbol_value("command-line-processed"),
        Some(&Value::T)
    );
}

#[test]
fn gui_bootstrap_accepts_iso_8859_15_coding_primitives() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(BOOTSTRAP_CORE_FEATURES)
        .expect("bootstrap evaluator");
    let _frame_id = bootstrap_runtime_gui_startup(&mut eval);

    let probes = [
        ("known", "(coding-system-p 'iso-8859-15)", Some(Value::T)),
        (
            "type",
            "(coding-system-type 'iso-8859-15)",
            Some(Value::symbol("charset")),
        ),
        (
            "eol",
            "(coding-system-change-eol-conversion 'iso-8859-15 0)",
            Some(Value::symbol("iso-latin-9-unix")),
        ),
        (
            "terminal-set",
            "(progn (set-terminal-coding-system 'iso-8859-15 nil t) 'ok)",
            Some(Value::symbol("ok")),
        ),
        (
            "keyboard-set",
            "(progn (set-keyboard-coding-system 'iso-8859-15 nil) 'ok)",
            Some(Value::symbol("ok")),
        ),
        (
            "keyboard-var",
            "keyboard-coding-system",
            Some(Value::symbol("iso-latin-9-unix")),
        ),
        (
            "terminal-var",
            "(terminal-coding-system)",
            Some(Value::symbol("iso-8859-15")),
        ),
    ];

    for (label, source, expected) in probes {
        let result = eval.eval_str(source);
        let value = result.unwrap_or_else(|_| panic!("coding probe {label} should evaluate"));
        if let Some(expected_value) = expected {
            let actual_name = value
                .as_symbol_name()
                .map(|name| name.to_string())
                .unwrap_or_else(|| format!("{value:?}"));
            let expected_name = expected_value
                .as_symbol_name()
                .map(|name| name.to_string())
                .unwrap_or_else(|| format!("{expected_value:?}"));
            assert_eq!(
                value, expected_value,
                "coding probe {label} should match GNU bootstrap semantics (actual={actual_name}, expected={expected_name})"
            );
        }
    }
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

    let result = eval
        .eval_str(
            r#"(progn
             (switch-to-buffer "*scratch*")
             (erase-buffer)
             (insert "abc\ndef\nghi")
             (goto-char 1)
             (command-execute 'next-line)
             (point))"#,
        )
        .expect("startup next-line should evaluate");
    assert_eq!(result, Value::fixnum(5));
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
    eval.set_variable("initial-window-system", Value::NIL);

    let result = eval
        .eval_str(
            r#"(condition-case err
                  (progn
                    (frame-set-background-mode (selected-frame))
                    'ok)
                (error (list 'error (error-message-string err))))"#,
        )
        .expect("frame-set-background-mode probe should evaluate");
    assert_eq!(result, Value::symbol("ok"));
}

#[test]
fn modify_frame_parameters_updates_live_default_face_colors_for_gui_frames() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(BOOTSTRAP_CORE_FEATURES)
        .expect("bootstrap evaluator");
    let _frame_id = bootstrap_runtime_gui_startup(&mut eval);

    let result = eval
        .eval_str(
            r##"(progn
             (modify-frame-parameters
              (selected-frame)
              '((foreground-color . "white")
                (background-color . "#000000")))
             (list
              (frame-parameter nil 'background-mode)
              (frame-parameter nil 'foreground-color)
              (frame-parameter nil 'background-color)
              (face-foreground 'default nil t)
              (face-background 'default nil t)))"##,
        )
        .expect("modify-frame-parameters face probe should evaluate");
    assert_eq!(
        print_value_with_eval(&mut eval, &result),
        "(dark \"white\" \"#000000\" \"white\" \"#000000\")"
    );
}

#[test]
fn modify_frame_parameters_background_color_only_completes_for_gui_frames() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(BOOTSTRAP_CORE_FEATURES)
        .expect("bootstrap evaluator");
    let _frame_id = bootstrap_runtime_gui_startup(&mut eval);

    let result = eval
        .eval_str(
            r##"(progn
             (modify-frame-parameters
              (selected-frame)
              '((background-color . "#000000")))
             (list
              'after-modify
              (frame-parameter nil 'background-mode)
              (frame-parameter nil 'background-color)))"##,
        )
        .expect("background-only modify-frame-parameters should evaluate");
    assert_eq!(
        print_value_with_eval(&mut eval, &result),
        "(after-modify dark \"#000000\")"
    );
}

#[test]
fn frame_set_background_mode_keep_face_specs_completes_after_dark_background_change() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(BOOTSTRAP_CORE_FEATURES)
        .expect("bootstrap evaluator");
    let frame_id = bootstrap_runtime_gui_startup(&mut eval);
    {
        let frame = eval
            .frame_manager_mut()
            .get_mut(frame_id)
            .expect("live frame");
        frame
            .parameters
            .insert("background-color".to_string(), Value::string("#000000"));
    }

    let result = eval
        .eval_str(
            r#"(progn
             (frame-set-background-mode (selected-frame) t)
             (list
              'after-frame-set-background-mode
              (frame-parameter nil 'background-mode)
              (frame-parameter nil 'display-type)))"#,
        )
        .expect("frame-set-background-mode keep-face-specs should evaluate");
    assert_eq!(
        print_value_with_eval(&mut eval, &result),
        "(after-frame-set-background-mode dark color)"
    );
}

fn seed_selected_frame_background_color(eval: &mut Context, color: &str) {
    let frame_id = eval
        .frame_manager()
        .selected_frame()
        .expect("selected frame")
        .id;
    let frame = eval
        .frame_manager_mut()
        .get_mut(frame_id)
        .expect("live frame");
    frame
        .parameters
        .insert("background-color".to_string(), Value::string(color));
}

#[test]
fn dark_gui_background_color_values_match_gnu_shape() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(BOOTSTRAP_CORE_FEATURES)
        .expect("bootstrap evaluator");
    let _frame_id = bootstrap_runtime_gui_startup(&mut eval);
    seed_selected_frame_background_color(&mut eval, "#000000");

    let result = eval
        .eval_str(r##"(color-values "#000000" (selected-frame))"##)
        .expect("color-values probe should evaluate");
    assert_eq!(print_value_with_eval(&mut eval, &result), "(0 0 0)");
}

#[test]
fn dark_gui_background_color_dark_predicate_completes() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(BOOTSTRAP_CORE_FEATURES)
        .expect("bootstrap evaluator");
    let _frame_id = bootstrap_runtime_gui_startup(&mut eval);
    seed_selected_frame_background_color(&mut eval, "#000000");

    let result = eval
        .eval_str(
            r##"(color-dark-p
             (mapcar (lambda (c) (/ c 65535.0))
                     (color-values "#000000" (selected-frame))))"##,
        )
        .expect("color-dark-p probe should evaluate");
    assert_eq!(result, Value::T);
}

#[test]
fn dark_gui_frame_current_background_mode_completes() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(BOOTSTRAP_CORE_FEATURES)
        .expect("bootstrap evaluator");
    let _frame_id = bootstrap_runtime_gui_startup(&mut eval);
    seed_selected_frame_background_color(&mut eval, "#000000");

    let result = eval
        .eval_str(r#"(frame--current-background-mode (selected-frame))"#)
        .expect("current background mode probe should evaluate");
    let debug_result = eval
        .eval_str(
            r##"(list
            (frame-parameter nil 'background-color)
            (frame-parameter nil 'background-mode)
            frame-background-mode
            (terminal-parameter nil 'background-mode)
            (window-system (selected-frame))
            (tty-type (selected-frame))
            (color-values "#000000" (selected-frame))
            (frame--current-background-mode (selected-frame)))"##,
        )
        .expect("current background mode debug probe should evaluate");
    assert_eq!(
        print_value_with_eval(&mut eval, &debug_result),
        "(\"#000000\" light nil nil neo nil (0 0 0) dark)"
    );
    assert_eq!(result, Value::symbol("dark"));
}

#[test]
fn modify_frame_parameters_prefers_first_duplicate_frame_parameter_like_gnu() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(BOOTSTRAP_CORE_FEATURES)
        .expect("bootstrap evaluator");
    let _frame_id = bootstrap_runtime_gui_startup(&mut eval);

    let result = eval
        .eval_str(
            r##"(progn
             (modify-frame-parameters
              (selected-frame)
              '((background-color . "#000000")
                (background-color . "white")))
             (list
              (frame-parameter nil 'background-color)
              (face-background 'default nil t)
              (frame-parameter nil 'background-mode)))"##,
        )
        .expect("duplicate frame parameter probe should evaluate");
    assert_eq!(
        print_value_with_eval(&mut eval, &result),
        "(\"#000000\" \"#000000\" dark)"
    );
}

#[test]
fn gnu_startup_seeds_light_gui_chrome_faces_from_faces_el() {
    let mut eval = create_bootstrap_evaluator_with_features(BOOTSTRAP_CORE_FEATURES)
        .expect("bootstrap evaluator");
    let _frame_id = bootstrap_runtime_gui_startup(&mut eval);
    run_gnu_startup(&mut eval);

    let result = eval
        .eval_str(
            r#"(list
             (window-system)
             (frame-parameter nil 'window-system)
             (display-graphic-p)
             (display-graphic-p (selected-frame))
             (xw-display-color-p (selected-frame))
             (frame-parameter nil 'display-type)
             (frame-parameter nil 'background-mode)
             (display-color-p)
             (display-color-cells)
             (face-default-spec 'mode-line)
             (face-spec-choose (face-default-spec 'mode-line) (selected-frame) 'no-match)
             (face-default-spec 'header-line)
             (face-spec-choose (face-default-spec 'header-line) (selected-frame) 'no-match)
             (condition-case err
                 (progn
                   (face-set-after-frame-default
                    (selected-frame)
                    (frame-parameters (selected-frame)))
                   (list
                    (face-background 'mode-line nil t)
                    (face-background 'mode-line-inactive nil t)
                    (face-background 'header-line nil t)
                    (face-background 'tab-bar nil t)
                    (face-background 'tab-line nil t)))
               (error (list 'error (error-message-string err))))
             (list
              (face-background 'mode-line nil t)
              (face-background 'mode-line-inactive nil t)
              (face-background 'header-line nil t)
              (face-background 'tab-bar nil t)
              (face-background 'tab-line nil t))
             (progn
               (set-face-attribute 'mode-line (selected-frame)
                                   :background "grey75"
                                   :foreground "black")
               (list
                (face-background 'mode-line nil t)
                (face-foreground 'mode-line nil t)))
             (face-background 'mode-line nil t)
             (face-foreground 'mode-line nil t)
             (face-background 'mode-line-inactive nil t)
             (face-background 'header-line nil t)
             (face-background 'tab-bar nil t)
             (face-background 'tab-line nil t))"#,
        )
        .expect("chrome face probe should evaluate");
    let values = list_to_vec(&result).expect("chrome face probe should return a list");
    assert_eq!(values.len(), 22);
    let rendered: Vec<String> = values
        .iter()
        .map(|value| print_value_with_eval(&mut eval, value))
        .collect();
    assert_eq!(
        rendered[0], "neo",
        "chrome probe should still report a GUI backend: {rendered:?}"
    );
    assert_eq!(
        rendered[1], "neo",
        "frame backend should stay on neo during startup: {rendered:?}"
    );
    assert_eq!(
        rendered[2], "t",
        "display-graphic-p should stay true during startup: {rendered:?}"
    );
    assert_eq!(
        rendered[3], "t",
        "display-graphic-p should stay true for the selected frame: {rendered:?}"
    );
    assert_eq!(
        rendered[4], "t",
        "xw-display-color-p should stay true for the selected frame: {rendered:?}"
    );
    assert_eq!(
        rendered[5], "color",
        "display-type should stay color during startup: {rendered:?}"
    );
    assert_eq!(
        rendered[6], "light",
        "background-mode should stay light during startup: {rendered:?}"
    );
    assert_eq!(
        rendered[7], "t",
        "display-color-p should stay true during startup: {rendered:?}"
    );
    assert_eq!(
        rendered[8], "16777216",
        "display-color-cells should stay high-color during startup: {rendered:?}"
    );
    assert_eq!(
        rendered[9],
        "((((class color grayscale) (min-colors 88)) :box (:line-width -1 :style released-button) :background \"grey75\" :foreground \"black\") (t :inverse-video t))",
        "mode-line defface spec should be present: {rendered:?}"
    );
    assert_eq!(
        rendered[10],
        "(:box (:line-width -1 :style released-button) :background \"grey75\" :foreground \"black\")",
        "mode-line defface should match the live neo frame: {rendered:?}"
    );
    assert_eq!(
        rendered[11],
        "((default :inherit mode-line) (((type tty)) :inverse-video nil :underline t) (((class color grayscale) (background light)) :background \"grey90\" :foreground \"grey20\" :box nil) (((class color grayscale) (background dark)) :background \"grey20\" :foreground \"grey90\" :box nil) (((class mono) (background light)) :background \"white\" :foreground \"black\" :inverse-video nil :box nil :underline t) (((class mono) (background dark)) :background \"black\" :foreground \"white\" :inverse-video nil :box nil :underline t))",
        "header-line defface spec should be present: {rendered:?}"
    );
    assert_eq!(
        rendered[12], "(:inherit mode-line :background \"grey90\" :foreground \"grey20\" :box nil)",
        "header-line defface should match the live neo frame: {rendered:?}"
    );
    assert_eq!(
        rendered[13], "(\"grey75\" \"grey90\" \"grey90\" \"grey85\" \"grey85\")",
        "face-set-after-frame-default probe = {rendered:?}"
    );
    assert_eq!(
        rendered[14], "(\"grey75\" \"grey90\" \"grey90\" \"grey85\" \"grey85\")",
        "chrome probe = {rendered:?}"
    );
    assert_eq!(
        rendered[15], "(\"grey75\" \"black\")",
        "manual set-face-attribute should still work on the live GUI frame: {rendered:?}"
    );
    assert_eq!(rendered[16], "\"grey75\"", "chrome probe = {rendered:?}");
    assert_eq!(rendered[17], "\"black\"", "chrome probe = {rendered:?}");
    assert_eq!(rendered[18], "\"grey90\"", "chrome probe = {rendered:?}");
    assert_eq!(rendered[19], "\"grey90\"", "chrome probe = {rendered:?}");
    assert_eq!(rendered[20], "\"grey85\"", "chrome probe = {rendered:?}");
    assert_eq!(rendered[21], "\"grey85\"", "chrome probe = {rendered:?}");
}

#[test]
fn gnu_startup_clears_terminal_frame_without_deselecting_opening_gui_frame() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(BOOTSTRAP_CORE_FEATURES)
        .expect("bootstrap evaluator");
    let frame_id = bootstrap_runtime_gui_startup(&mut eval);

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(neovm_core::keyboard::InputEvent::WindowClose {
        emacs_frame_id: u64::MAX,
    })
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
    result.expect("close request should let recursive edit exit cleanly");

    assert_eq!(
        eval.frame_manager().selected_frame().map(|frame| frame.id),
        Some(frame_id),
        "GUI startup should keep the opening frame selected through the first recursive edit"
    );
    assert_eq!(
        eval.obarray().symbol_value("terminal-frame"),
        Some(&Value::NIL),
        "GUI startup should clear terminal-frame after the first recursive edit enters the command loop"
    );
}

#[test]
fn gnu_startup_set_face_attribute_returns_on_live_gui_frame() {
    assert_eq!(
        eval_after_gnu_gui_startup(
            r#"(condition-case err
                  (progn
                    (set-face-attribute 'mode-line (selected-frame)
                                        :background "grey75"
                                        :foreground "black")
                    (list
                     (face-background 'mode-line nil t)
                     (face-foreground 'mode-line nil t)))
                (error (list 'error (error-message-string err))))"#,
        ),
        "(\"grey75\" \"black\")"
    );
}

#[test]
fn gnu_startup_face_set_after_frame_default_materializes_mode_line() {
    assert_eq!(
        eval_after_gnu_gui_startup(
            r#"(condition-case err
                  (progn
                    (face-set-after-frame-default
                     (selected-frame)
                     (frame-parameters (selected-frame)))
                    (list
                     (face-background 'mode-line nil t)
                     (face-foreground 'mode-line nil t)))
                (error (list 'error (error-message-string err))))"#,
        ),
        "(\"grey75\" \"black\")"
    );
}

#[test]
fn gnu_startup_face_recalc_loop_materializes_gui_chrome_faces_progressively() {
    assert_eq!(
        eval_after_gnu_gui_startup(
            r#"(let ((last :unset)
                     changes
                     errors)
                 (dolist (face (nreverse (face-list)))
                   (condition-case err
                       (progn
                         (face-spec-recalc face (selected-frame))
                         (internal-merge-in-global-face face (selected-frame)))
                     (error
                      (push (list face (error-message-string err)) errors)))
                   (let ((current (list (face-background 'mode-line nil t)
                                        (face-foreground 'mode-line nil t)
                                        (face-background 'mode-line-inactive nil t)
                                        (face-background 'header-line nil t)
                                        (face-background 'tab-bar nil t)
                                        (face-background 'tab-line nil t))))
                     (unless (equal current last)
                       (push (list face current) changes)
                       (setq last current))))
                 (list (nreverse changes) (nreverse errors)))"#,
        ),
        "(((default (\"grey75\" \"black\" \"grey90\" \"grey90\" \"grey85\" \"grey85\"))) nil)"
    );
}

#[test]
fn gnu_startup_face_spec_recalc_materializes_mode_line() {
    assert_eq!(
        eval_after_gnu_gui_startup(
            r#"(condition-case err
                  (progn
                    (face-spec-recalc 'mode-line (selected-frame))
                    (list
                     (face-background 'mode-line nil t)
                     (face-foreground 'mode-line nil t)))
                (error (list 'error (error-message-string err))))"#
        ),
        "(\"grey75\" \"black\")"
    );
}

#[test]
fn gnu_startup_internal_merge_in_global_face_preserves_mode_line_after_recalc() {
    assert_eq!(
        eval_after_gnu_gui_startup(
            r#"(condition-case err
                  (progn
                    (face-spec-recalc 'mode-line (selected-frame))
                    (internal-merge-in-global-face 'mode-line (selected-frame))
                    (list
                     (face-background 'mode-line nil t)
                     (face-foreground 'mode-line nil t)))
                (error (list 'error (error-message-string err))))"#
        ),
        "(\"grey75\" \"black\")"
    );
}

#[test]
fn gnu_startup_face_background_getter_returns_on_live_gui_frame() {
    assert_eq!(
        eval_after_gnu_gui_startup(
            r#"(condition-case err
                  (face-background 'mode-line nil t)
                (error (list 'error (error-message-string err))))"#
        ),
        "\"grey75\""
    );
}

#[test]
fn gnu_startup_internal_set_lisp_face_attribute_returns_on_live_gui_frame() {
    assert_eq!(
        eval_after_gnu_gui_startup(
            r#"(condition-case err
                  (progn
                    (internal-set-lisp-face-attribute
                     'mode-line :background "grey75" (selected-frame))
                    (face-background 'mode-line nil t))
                (error (list 'error (error-message-string err))))"#
        ),
        "\"grey75\""
    );
}

#[test]
fn gnu_startup_internal_set_lisp_face_attribute_without_getter_returns_on_live_gui_frame() {
    assert_eq!(
        eval_after_gnu_gui_startup(
            r#"(condition-case err
                  (progn
                    (internal-set-lisp-face-attribute
                     'mode-line :background "grey75" (selected-frame))
                    'ok)
                (error (list 'error (error-message-string err))))"#
        ),
        "ok"
    );
}
