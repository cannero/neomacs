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
fn parse_compile_main_dependencies_reads_gnu_makefile_rules() {
    let lisp_root = PathBuf::from("/repo/lisp");
    let contents = "\
$(lisp)/progmodes/cc-align.elc \\
  $(lisp)/progmodes/cc-cmds.elc: \\
  $(lisp)/progmodes/cc-bytecomp.elc $(lisp)/progmodes/cc-defs.elc
$(lisp)/progmodes/js.elc: $(lisp)/progmodes/cc-mode.elc $(srcdir)/ignored.elc
not-lisp.elc: $(lisp)/ignored.elc
";

    let deps = parse_compile_main_dependencies_from_str(contents, &lisp_root);

    let cc_bytecomp = lisp_root.join("progmodes/cc-bytecomp.el");
    let cc_defs = lisp_root.join("progmodes/cc-defs.el");
    assert_eq!(
        deps.get(&lisp_root.join("progmodes/cc-align.el")).unwrap(),
        &BTreeSet::from([cc_bytecomp.clone(), cc_defs.clone()])
    );
    assert_eq!(
        deps.get(&lisp_root.join("progmodes/cc-cmds.el")).unwrap(),
        &BTreeSet::from([cc_bytecomp, cc_defs])
    );
    assert_eq!(
        deps.get(&lisp_root.join("progmodes/js.el")).unwrap(),
        &BTreeSet::from([lisp_root.join("progmodes/cc-mode.el")])
    );
    assert!(!deps.contains_key(&lisp_root.join("ignored.el")));
}

#[test]
fn compile_main_dependency_waves_follow_gnu_cc_mode_rules() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let lisp_root = repo_root.join("lisp");
    let contents = fs::read_to_string(lisp_root.join("Makefile.in")).unwrap();
    let deps = parse_compile_main_dependencies_from_str(&contents, &lisp_root);
    let source = |rel: &str| lisp_root.join(rel);
    let sources = vec![
        source("progmodes/cc-bytecomp.el"),
        source("progmodes/cc-defs.el"),
        source("progmodes/cc-vars.el"),
        source("progmodes/cc-langs.el"),
        source("progmodes/cc-engine.el"),
        source("progmodes/cc-align.el"),
        source("progmodes/cc-cmds.el"),
        source("progmodes/cc-menus.el"),
        source("progmodes/cc-styles.el"),
        source("progmodes/cc-mode.el"),
        source("progmodes/js.el"),
    ];

    let waves = compile_main_dependency_waves(sources, &deps).unwrap();
    let wave_index = |path: PathBuf| {
        waves
            .iter()
            .position(|wave| wave.contains(&path))
            .unwrap_or_else(|| panic!("{} missing from dependency waves", path.display()))
    };

    let cc_bytecomp = wave_index(source("progmodes/cc-bytecomp.el"));
    let cc_defs = wave_index(source("progmodes/cc-defs.el"));
    let cc_vars = wave_index(source("progmodes/cc-vars.el"));
    let cc_langs = wave_index(source("progmodes/cc-langs.el"));
    let cc_engine = wave_index(source("progmodes/cc-engine.el"));
    let cc_align = wave_index(source("progmodes/cc-align.el"));
    let cc_cmds = wave_index(source("progmodes/cc-cmds.el"));
    let cc_menus = wave_index(source("progmodes/cc-menus.el"));
    let cc_styles = wave_index(source("progmodes/cc-styles.el"));
    let cc_mode = wave_index(source("progmodes/cc-mode.el"));
    let js = wave_index(source("progmodes/js.el"));

    assert!(cc_bytecomp < cc_defs);
    assert!(cc_defs < cc_vars);
    assert!(cc_vars < cc_langs);
    assert!(cc_langs < cc_engine);
    assert!(cc_engine < cc_align);
    assert!(cc_engine < cc_cmds);
    assert!(cc_align < cc_styles);
    for prerequisite in [
        cc_vars, cc_langs, cc_engine, cc_align, cc_cmds, cc_menus, cc_styles,
    ] {
        assert!(prerequisite < cc_mode);
    }
    assert!(cc_mode < js);
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
fn generated_leim_source_files_match_gnu_bootstrap_clean_scope() {
    let repo_root = PathBuf::from("/repo");
    let paths = PipelinePaths {
        temacs: repo_root.join("target/debug/neomacs-temacs"),
        bootstrap: repo_root.join("target/debug/bootstrap-neomacs"),
        final_bin: repo_root.join("target/debug/neomacs"),
        lisp_root: repo_root.join("lisp"),
        leim_root: repo_root.join("leim"),
        admin_grammars_root: repo_root.join("admin/grammars"),
        makefile_in: repo_root.join("lisp/Makefile.in"),
    };

    let files = generated_leim_source_files(&paths);
    let relative = files
        .iter()
        .map(|path| {
            path.strip_prefix(repo_root.join("lisp"))
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();

    assert!(relative.contains(&"leim/quail/CTLau-b5.el".to_string()));
    assert!(relative.contains(&"language/pinyin.el".to_string()));
    assert!(relative.contains(&"leim/leim-list.el".to_string()));
    assert_eq!(files.len(), LEIM_GENERATION_RULES.len() + 3);
}

#[test]
fn generated_custom_finder_source_files_match_gnu_autogen_scope() {
    let repo_root = PathBuf::from("/repo");
    let paths = PipelinePaths {
        temacs: repo_root.join("target/debug/neomacs-temacs"),
        bootstrap: repo_root.join("target/debug/bootstrap-neomacs"),
        final_bin: repo_root.join("target/debug/neomacs"),
        lisp_root: repo_root.join("lisp"),
        leim_root: repo_root.join("leim"),
        admin_grammars_root: repo_root.join("admin/grammars"),
        makefile_in: repo_root.join("lisp/Makefile.in"),
    };

    assert_eq!(
        generated_custom_finder_source_files(&paths),
        vec![
            repo_root.join("lisp/cus-load.el"),
            repo_root.join("lisp/finder-inf.el"),
        ]
    );
}

#[test]
fn custom_and_finder_dirs_follow_gnu_subdir_filters() {
    let tempdir = tempdir();
    let lisp_root = tempdir.join("lisp");
    for dir in [
        "",
        "calendar",
        "leim",
        "leim/quail",
        "obsolete",
        "term",
        "term/xterm",
    ] {
        fs::create_dir_all(lisp_root.join(dir)).unwrap();
    }

    let custom = lisp_dirs_for_custom_dependencies(&lisp_root)
        .unwrap()
        .into_iter()
        .map(|path| path.strip_prefix(&lisp_root).unwrap().to_path_buf())
        .collect::<Vec<_>>();
    assert!(custom.contains(&PathBuf::from("calendar")));
    assert!(custom.contains(&PathBuf::from("leim")));
    assert!(custom.contains(&PathBuf::from("leim/quail")));
    assert!(!custom.contains(&PathBuf::from("obsolete")));
    assert!(!custom.contains(&PathBuf::from("term")));
    assert!(custom.contains(&PathBuf::from("term/xterm")));

    let finder = lisp_dirs_for_finder_data(&lisp_root)
        .unwrap()
        .into_iter()
        .map(|path| path.strip_prefix(&lisp_root).unwrap().to_path_buf())
        .collect::<Vec<_>>();
    assert!(finder.contains(&PathBuf::from("calendar")));
    assert!(!finder.contains(&PathBuf::from("leim")));
    assert!(!finder.contains(&PathBuf::from("leim/quail")));
    assert!(!finder.contains(&PathBuf::from("obsolete")));
    assert!(!finder.contains(&PathBuf::from("term")));
    assert!(finder.contains(&PathBuf::from("term/xterm")));
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
fn compile_main_failure_summary_reports_failed_file_count() {
    assert_eq!(
        compile_main_failure_summary(&["/repo/lisp/simple.el".to_string()]),
        "compile-main failed to byte-compile 1 file"
    );
    assert_eq!(
        compile_main_failure_summary(&[
            "/repo/lisp/simple.el".to_string(),
            "/repo/lisp/calendar/calendar.el".to_string(),
        ]),
        "compile-main failed to byte-compile 2 files"
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

#[test]
fn custom_dependencies_generation_args_match_gnu_shape() {
    let dirs = vec![
        PathBuf::from("/repo/lisp"),
        PathBuf::from("/repo/lisp/calendar"),
    ];
    let args = custom_dependencies_generation_args(
        Path::new("/repo/lisp"),
        Path::new("/repo/lisp/cus-load.el"),
        &dirs,
    );

    assert_eq!(
        args,
        vec![
            OsString::from("--batch"),
            OsString::from("--no-site-file"),
            OsString::from("--no-site-lisp"),
            OsString::from("-l"),
            OsString::from("cus-dep"),
            OsString::from("--eval"),
            OsString::from(
                "(setq generated-custom-dependencies-file (unmsys--file-name \"/repo/lisp/cus-load.el\"))"
            ),
            OsString::from("-f"),
            OsString::from("custom-make-dependencies"),
            OsString::from("/repo/lisp"),
            OsString::from("/repo/lisp/calendar"),
        ]
    );
}

#[test]
fn finder_data_generation_args_match_gnu_shape() {
    let dirs = vec![
        PathBuf::from("/repo/lisp"),
        PathBuf::from("/repo/lisp/calendar"),
    ];
    let args = finder_data_generation_args(
        Path::new("/repo/lisp"),
        Path::new("/repo/lisp/finder-inf.el"),
        &dirs,
    );

    assert_eq!(
        args,
        vec![
            OsString::from("--batch"),
            OsString::from("--no-site-file"),
            OsString::from("--no-site-lisp"),
            OsString::from("-l"),
            OsString::from("finder"),
            OsString::from("--eval"),
            OsString::from(
                "(setq generated-finder-keywords-file (unmsys--file-name \"/repo/lisp/finder-inf.el\"))"
            ),
            OsString::from("-f"),
            OsString::from("finder-compile-keywords-make-dist"),
            OsString::from("/repo/lisp"),
            OsString::from("/repo/lisp/calendar"),
        ]
    );
}

#[test]
fn semantic_grammar_targets_follow_gnu_admin_grammars_makefile() {
    let outputs = SEMANTIC_GRAMMAR_TARGETS
        .iter()
        .map(|target| target.output_rel)
        .collect::<Vec<_>>();

    assert_eq!(
        outputs,
        vec![
            "cedet/semantic/bovine/c-by.el",
            "cedet/semantic/bovine/make-by.el",
            "cedet/semantic/bovine/scm-by.el",
            "cedet/semantic/grammar-wy.el",
            "cedet/semantic/wisent/javat-wy.el",
            "cedet/semantic/wisent/js-wy.el",
            "cedet/semantic/wisent/python-wy.el",
            "cedet/srecode/srt-wy.el",
        ]
    );
}

#[test]
fn semantic_grammar_args_match_gnu_wisent_shape() {
    let args = semantic_grammar_args(
        SemanticGrammarKind::Wisent,
        Path::new("/repo/lisp/cedet/srecode/srt-wy.el"),
        Path::new("/repo/admin/grammars/srecode-template.wy"),
    );

    assert_eq!(
        args,
        vec![
            OsString::from("--batch"),
            OsString::from("--no-site-file"),
            OsString::from("--no-site-lisp"),
            OsString::from("--eval"),
            OsString::from("(setq load-prefer-newer t)"),
            OsString::from("-l"),
            OsString::from("semantic/wisent/grammar"),
            OsString::from("-f"),
            OsString::from("wisent-batch-make-parser"),
            OsString::from("-o"),
            OsString::from("/repo/lisp/cedet/srecode/srt-wy.el"),
            OsString::from("/repo/admin/grammars/srecode-template.wy"),
        ]
    );
}

#[test]
fn leim_generation_args_match_gnu_titdic_shape() {
    let args = leim_generation_args(
        LeimGenerationKind::TitDic,
        Path::new("/repo/lisp/leim/quail"),
        Path::new("/repo/leim/CXTERM-DIC/CCDOSPY.tit"),
        Path::new("/repo/lisp/leim/quail/CCDOSPY.el"),
    );

    assert_eq!(
        args,
        vec![
            OsString::from("--batch"),
            OsString::from("--no-site-file"),
            OsString::from("--no-site-lisp"),
            OsString::from("-l"),
            OsString::from("titdic-cnv"),
            OsString::from("-f"),
            OsString::from("batch-tit-dic-convert"),
            OsString::from("-dir"),
            OsString::from("/repo/lisp/leim/quail"),
            OsString::from("/repo/leim/CXTERM-DIC/CCDOSPY.tit"),
        ]
    );
}

#[test]
fn leim_ext_append_contents_matches_gnu_sed_filter() {
    let input = "\
plain-entry
;comment
;inc one-level
;;inc two-level
";

    assert_eq!(
        leim_ext_append_contents(input),
        "plain-entry\n; one-level\n;; two-level\n"
    );
}

#[test]
fn executable_fingerprint_patch_is_idempotent() {
    let tempdir = tempdir();
    let binary = tempdir.join("neomacs");
    let mut contents = b"prefix".to_vec();
    contents.extend_from_slice(FINGERPRINT_MAGIC_START);
    contents.extend_from_slice(FINGERPRINT_PLACEHOLDER);
    contents.extend_from_slice(FINGERPRINT_MAGIC_END);
    contents.extend_from_slice(b"suffix");
    fs::write(&binary, contents).unwrap();

    let first = executable_family_fingerprint(&[binary.as_path()]).unwrap();
    patch_executable_fingerprint(&binary, &first).unwrap();
    let patched_once = fs::read(&binary).unwrap();

    let second = executable_family_fingerprint(&[binary.as_path()]).unwrap();
    assert_eq!(first, second);
    patch_executable_fingerprint(&binary, &second).unwrap();
    assert_eq!(patched_once, fs::read(&binary).unwrap());
}

#[test]
fn executable_fingerprint_patches_all_records() {
    let tempdir = tempdir();
    let binary = tempdir.join("neomacs");
    let mut contents = Vec::new();
    for label in [b"one".as_slice(), b"two".as_slice()] {
        contents.extend_from_slice(label);
        contents.extend_from_slice(FINGERPRINT_MAGIC_START);
        contents.extend_from_slice(FINGERPRINT_PLACEHOLDER);
        contents.extend_from_slice(FINGERPRINT_MAGIC_END);
    }
    fs::write(&binary, contents).unwrap();

    let fingerprint = [0xA5; 32];
    patch_executable_fingerprint(&binary, &fingerprint).unwrap();
    let patched = fs::read(&binary).unwrap();

    for slot in executable_fingerprint_slots(&patched) {
        assert_eq!(&patched[slot..slot + 32], &fingerprint);
    }
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
