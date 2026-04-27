use super::*;

fn parse_options(args: &[&str]) -> FreshBuildOptions {
    FreshBuildOptions::parse(
        PathBuf::from("/repo"),
        args.iter().map(|arg| OsString::from(arg)),
    )
    .unwrap()
}

#[test]
fn parse_defaults_to_debug_bin_dir() {
    let options = parse_options(&[]);
    assert!(!options.release);
    assert_eq!(options.bin_dir, PathBuf::from("/repo/target/debug"));
}

#[test]
fn parse_release_uses_release_bin_dir() {
    let options = parse_options(&["--release"]);
    assert!(options.release);
    assert_eq!(options.bin_dir, PathBuf::from("/repo/target/release"));
}

#[test]
fn explicit_bin_dir_overrides_release_default() {
    let options = parse_options(&["--release", "--bin-dir", "out/neomacs-bin"]);
    assert!(options.release);
    assert_eq!(options.bin_dir, PathBuf::from("/repo/out/neomacs-bin"));
}

#[test]
fn explicit_bin_dir_before_release_stays_in_effect() {
    let options = parse_options(&["--bin-dir", "out/neomacs-bin", "--release"]);
    assert!(options.release);
    assert_eq!(options.bin_dir, PathBuf::from("/repo/out/neomacs-bin"));
}

#[test]
fn parse_compile_first_skips_native_entries_by_default() {
    let tempdir = tempdir();
    let lisp_root = tempdir.join("lisp");
    fs::create_dir_all(lisp_root.join("emacs-lisp")).unwrap();
    fs::write(lisp_root.join("emacs-lisp/early.el"), "").unwrap();
    fs::write(lisp_root.join("emacs-lisp/native-only.el"), "").unwrap();

    let contents = "\
COMPILE_FIRST = $(lisp)/emacs-lisp/early.elc \\
                $(lisp)/missing.elc
ifeq ($(HAVE_NATIVE_COMP),yes)
COMPILE_FIRST += $(lisp)/emacs-lisp/native-only.elc
endif
";

    let parsed = parse_compile_first_sources_from_str(contents, &lisp_root, false);
    assert_eq!(parsed, vec![lisp_root.join("emacs-lisp/early.el")]);
}

#[test]
fn parse_compile_first_includes_native_entries_when_enabled() {
    let tempdir = tempdir();
    let lisp_root = tempdir.join("lisp");
    fs::create_dir_all(lisp_root.join("emacs-lisp")).unwrap();
    fs::write(lisp_root.join("emacs-lisp/early.el"), "").unwrap();
    fs::write(lisp_root.join("emacs-lisp/native-only.el"), "").unwrap();

    let contents = "\
ifeq ($(HAVE_NATIVE_COMP),yes)
COMPILE_FIRST += $(lisp)/emacs-lisp/native-only.elc
endif
COMPILE_FIRST += $(lisp)/emacs-lisp/early.elc
";

    let parsed = parse_compile_first_sources_from_str(contents, &lisp_root, true);
    assert_eq!(
        parsed,
        vec![
            lisp_root.join("emacs-lisp/native-only.el"),
            lisp_root.join("emacs-lisp/early.el"),
        ]
    );
}

#[test]
fn parse_main_first_sources_handles_gnu_multiline_list() {
    let lisp_root = PathBuf::from("/repo/lisp");
    let contents = "\
MAIN_FIRST = ./emacs-lisp/eieio.el ./emacs-lisp/eieio-base.el \\
  ./org/ox.el ./already-elc.elc
";

    let parsed = parse_main_first_sources_from_str(contents, &lisp_root);

    assert_eq!(
        parsed,
        vec![
            lisp_root.join("emacs-lisp/eieio.el"),
            lisp_root.join("emacs-lisp/eieio-base.el"),
            lisp_root.join("org/ox.el"),
            lisp_root.join("already-elc.el"),
        ]
    );
}

#[test]
fn generated_lisp_bytecode_files_collects_nested_elc_files() {
    let tempdir = tempdir();
    let lisp_root = tempdir.join("lisp");
    fs::create_dir_all(lisp_root.join("emacs-lisp")).unwrap();
    fs::create_dir_all(lisp_root.join("org")).unwrap();
    fs::write(lisp_root.join("emacs-lisp/macroexp.elc"), "").unwrap();
    fs::write(lisp_root.join("org/org.elc"), "").unwrap();
    fs::write(lisp_root.join("org/org.el"), "").unwrap();

    let files = generated_lisp_bytecode_files(&lisp_root).unwrap();

    assert_eq!(
        files,
        vec![
            lisp_root.join("emacs-lisp/macroexp.elc"),
            lisp_root.join("org/org.elc"),
        ]
    );
}

#[test]
fn compile_main_sources_follow_gnu_no_byte_compile_filter() {
    let tempdir = tempdir();
    let lisp_root = tempdir.join("lisp");
    fs::create_dir_all(lisp_root.join("sub")).unwrap();
    fs::write(lisp_root.join("a.el"), "").unwrap();
    fs::write(lisp_root.join(".hidden.el"), "").unwrap();
    fs::write(
        lisp_root.join("skip.el"),
        ";;; skip.el -*- no-byte-compile: t -*-\n",
    )
    .unwrap();
    fs::write(
        lisp_root.join("skip-existing.el"),
        ";;; skip-existing.el -*- no-byte-compile: t -*-\n",
    )
    .unwrap();
    fs::write(lisp_root.join("skip-existing.elc"), "").unwrap();
    fs::write(lisp_root.join("sub/b.el"), "").unwrap();

    let sources = compile_main_sources(&lisp_root).unwrap();

    assert_eq!(
        sources,
        vec![
            lisp_root.join("a.el"),
            lisp_root.join("skip-existing.el"),
            lisp_root.join("sub/b.el"),
        ]
    );
}

#[test]
fn gnu_no_byte_compile_marker_matches_makefile_grep_shape() {
    assert!(gnu_no_byte_compile_marker_line(
        ";;; file.el -*- no-byte-compile: t -*-"
    ));
    assert!(gnu_no_byte_compile_marker_line(
        ";; Local Variables: no-byte-compile: t"
    ));
    assert!(gnu_no_byte_compile_marker_line(
        ";; local-no-byte-compile: t"
    ));
    assert!(!gnu_no_byte_compile_marker_line(";; ano-byte-compile: t"));
    assert!(gnu_no_byte_compile_marker_line(
        ";; ano-byte-compile: t; no-byte-compile: t"
    ));
    assert!(!gnu_no_byte_compile_marker_line(
        ";;; file.el -*- no-byte-compile: nil -*-"
    ));
    assert!(!gnu_no_byte_compile_marker_line("(setq no-byte-compile t)"));
}

#[test]
fn inject_no_byte_compile_matches_loaddefs_boot_intent() {
    let input = "\
;;; loaddefs.el --- generated -*- lexical-binding:t -*-
;; Local Variables:
;; version-control: never
;; End:
";
    let output = inject_no_byte_compile(input);
    assert!(output.contains(";; Local Variables:\n;; no-byte-compile: t\n"));
}

#[test]
fn validate_primary_loaddefs_accepts_gnu_docstring_layout() {
    let contents = format!(
        "\
;;; loaddefs.el --- generated

{}

\x0c
;;; End of scraped data
;; Local Variables:
;; End:
",
        GNU_EBROWSE_DECLARATION_AUTOLOAD
    );

    validate_primary_loaddefs_contents(&contents).unwrap();
}

#[test]
fn validate_primary_loaddefs_rejects_moved_docstring_layout() {
    let contents = "\
;;; loaddefs.el --- generated

(autoload 'ebrowse-tags-find-declaration \"ebrowse\" \"\\
 t)

Find declaration of member at point.\"\x0c
;;; End of scraped data
;; Local Variables:
;; End:
";

    let err = validate_primary_loaddefs_contents(contents).unwrap_err();
    assert!(
        err.to_string().contains("moved an ebrowse docstring"),
        "unexpected error: {err}"
    );
}

#[test]
fn compile_first_args_match_gnu_non_native_shape() {
    let args = compile_first_args_for_source(false, Path::new("/tmp/macroexp.el"));
    assert_eq!(
        args,
        vec![
            OsString::from("--batch"),
            OsString::from("-f"),
            OsString::from("batch-byte-compile"),
            OsString::from("/tmp/macroexp.el"),
        ]
    );
}

#[test]
fn compile_first_args_match_gnu_native_shape() {
    let args = compile_first_args_for_source(true, Path::new("/tmp/macroexp.el"));
    assert_eq!(
        args,
        vec![
            OsString::from("--batch"),
            OsString::from("-l"),
            OsString::from("comp"),
            OsString::from("-f"),
            OsString::from("batch-byte-compile"),
            OsString::from("/tmp/macroexp.el"),
        ]
    );
}

#[test]
fn compile_main_args_match_gnu_non_native_shape() {
    let args = compile_main_args_for_source(false, Path::new("/tmp/simple.el"));
    assert_eq!(
        args,
        vec![
            OsString::from("--batch"),
            OsString::from("--no-site-file"),
            OsString::from("--no-site-lisp"),
            OsString::from("--eval"),
            OsString::from("(setq load-prefer-newer t byte-compile-warnings 'all)"),
            OsString::from("--eval"),
            OsString::from("(setq org--inhibit-version-check t)"),
            OsString::from("-f"),
            OsString::from("batch-byte-compile"),
            OsString::from("/tmp/simple.el"),
        ]
    );
}

#[test]
fn compile_main_args_match_gnu_native_shape() {
    let args = compile_main_args_for_source(true, Path::new("/tmp/simple.el"));
    assert_eq!(
        args,
        vec![
            OsString::from("--batch"),
            OsString::from("--no-site-file"),
            OsString::from("--no-site-lisp"),
            OsString::from("--eval"),
            OsString::from("(setq load-prefer-newer t byte-compile-warnings 'all)"),
            OsString::from("--eval"),
            OsString::from("(setq org--inhibit-version-check t)"),
            OsString::from("-l"),
            OsString::from("comp"),
            OsString::from("-f"),
            OsString::from("batch-byte+native-compile"),
            OsString::from("/tmp/simple.el"),
        ]
    );
}

#[test]
fn loaddefs_generation_args_force_full_generation() {
    let loaddefs_gen = Path::new("/repo/lisp/emacs-lisp/loaddefs-gen.el");
    let loaddefs_dirs = vec![
        PathBuf::from("/repo/lisp"),
        PathBuf::from("/repo/lisp/calendar"),
    ];
    let args = loaddefs_generation_args(loaddefs_gen, &loaddefs_dirs);
    let rendered = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    assert!(rendered.contains(&"--eval".to_string()));
    assert!(rendered.contains(&"neomacs-loaddefs-generate--force".to_string()));
    assert!(rendered.iter().any(|arg| arg.contains("(loaddefs-generate")
        && arg.contains("nil t t")
        && arg.contains("theme-loaddefs.el")));
    assert_eq!(
        &rendered[rendered.len() - 2..],
        ["/repo/lisp", "/repo/lisp/calendar"]
    );
}

fn tempdir() -> PathBuf {
    let dir = env::temp_dir().join(format!(
        "xtask-tests-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}
