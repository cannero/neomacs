use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

type DynError = Box<dyn Error>;
type Result<T> = std::result::Result<T, DynError>;

const FINGERPRINT_MAGIC_START: &[u8; 16] = b"NEOMACS-FP-START";
const FINGERPRINT_MAGIC_END: &[u8; 16] = b"NEOMACS-FP-END!!";
const FINGERPRINT_PLACEHOLDER: &[u8; 32] = b"NEOMACS_PDUMP_FINGERPRINT_SLOT!!";
const FINGERPRINT_RECORD_LEN: usize =
    FINGERPRINT_MAGIC_START.len() + FINGERPRINT_PLACEHOLDER.len() + FINGERPRINT_MAGIC_END.len();

#[derive(Debug, Clone)]
struct FreshBuildOptions {
    repo_root: PathBuf,
    runtime_root: PathBuf,
    bin_dir: PathBuf,
    release: bool,
    dry_run: bool,
    native_comp: bool,
    skip_build: bool,
}

#[derive(Debug, Clone)]
struct PipelinePaths {
    temacs: PathBuf,
    bootstrap: PathBuf,
    final_bin: PathBuf,
    lisp_root: PathBuf,
    makefile_in: PathBuf,
}

fn main() {
    if let Err(err) = try_main() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn try_main() -> Result<()> {
    let repo_root = repository_root();
    let options = FreshBuildOptions::parse(repo_root, env::args_os().skip(1))?;
    run_fresh_build(&options)
}

impl FreshBuildOptions {
    fn parse(
        repo_root: PathBuf,
        args: impl IntoIterator<Item = OsString>,
    ) -> Result<FreshBuildOptions> {
        let mut args = args.into_iter().peekable();

        if matches!(args.peek(), Some(arg) if arg == "help" || arg == "--help" || arg == "-h") {
            print_usage();
            std::process::exit(0);
        }

        if matches!(args.peek(), Some(arg) if arg == "fresh-build") {
            args.next();
        }

        let mut runtime_root = repo_root.clone();
        let mut bin_dir = None;
        let mut release = false;
        let mut dry_run = false;
        let mut native_comp =
            env::var("NEOMACS_NATIVE_COMP").is_ok_and(|value| value.eq_ignore_ascii_case("yes"));
        let mut skip_build = false;

        while let Some(arg) = args.next() {
            match arg.to_string_lossy().as_ref() {
                "--bin-dir" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "--bin-dir requires a path".to_string())?;
                    bin_dir = Some(resolve_cli_path(&repo_root, value));
                }
                "--runtime-root" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "--runtime-root requires a path".to_string())?;
                    runtime_root = resolve_cli_path(&repo_root, value);
                }
                "--release" => release = true,
                "--dry-run" => dry_run = true,
                "--native-comp" => native_comp = true,
                "--no-native-comp" => native_comp = false,
                "--skip-build" => skip_build = true,
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => {
                    return Err(format!("unknown option: {other}\n\n{}", usage_text()).into());
                }
            }
        }

        let bin_dir = bin_dir.unwrap_or_else(|| default_bin_dir(&repo_root, release));

        Ok(FreshBuildOptions {
            repo_root,
            runtime_root,
            bin_dir,
            release,
            dry_run,
            native_comp,
            skip_build,
        })
    }
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives directly under repository root")
        .to_path_buf()
}

fn default_bin_dir(repo_root: &Path, release: bool) -> PathBuf {
    env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                repo_root.join(path)
            }
        })
        .unwrap_or_else(|| repo_root.join("target"))
        .join(if release { "release" } else { "debug" })
}

fn resolve_cli_path(repo_root: &Path, raw: OsString) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        repo_root.join(path)
    }
}

fn run_fresh_build(options: &FreshBuildOptions) -> Result<()> {
    let paths = pipeline_paths(options);
    ensure_runtime_inputs(&paths)?;

    if !options.skip_build {
        let mut cargo_args = vec![
            OsString::from("build"),
            OsString::from("-p"),
            OsString::from("neomacs-bin"),
        ];
        if options.release {
            cargo_args.push(OsString::from("--release"));
        }
        run_command(
            options,
            &options.repo_root,
            &cargo_program(),
            &cargo_args,
            &[],
        )?;
    }

    patch_executable_fingerprints(options, &paths)?;

    if !options.dry_run {
        ensure_binaries_exist(&paths)?;
    }

    let envs = [(
        OsString::from("NEOMACS_RUNTIME_ROOT"),
        options.runtime_root.as_os_str().to_os_string(),
    )];
    let loaddefs_el = paths.lisp_root.join("loaddefs.el");
    let theme_loaddefs_el = paths.lisp_root.join("theme-loaddefs.el");
    let ldefs_boot = paths.lisp_root.join("ldefs-boot.el");

    // GNU's bootstrap-clean removes Lisp bytecode before building
    // bootstrap-emacs.  Keep primary loaddefs sources available for
    // pbootstrap/COMPILE_FIRST; GNU removes loaddefs.el later, in
    // autoloads-force, immediately before regenerating it.
    remove_stale_lisp_bytecode(options, &paths)?;

    run_command(
        options,
        &options.repo_root,
        &paths.temacs,
        &[
            OsString::from("--batch"),
            OsString::from("-l"),
            OsString::from("loadup"),
            OsString::from("--temacs=pbootstrap"),
        ],
        &envs,
    )?;

    // ---------------------------------------------------------------
    // COMPILE_FIRST: byte-compile the compiler infrastructure.
    //
    // GNU lisp/Makefile.in compiles COMPILE_FIRST files ONE AT A TIME,
    // each in a SEPARATE emacs process, in the listed order:
    //   macroexp.elc → cconv.elc → byte-opt.elc → bytecomp.elc
    //   → loaddefs-gen.elc → radix-tree.elc
    //
    // This ordering is critical: each file is compiled with a compiler
    // that already has the previously-compiled .elc files loaded,
    // making each successive compilation faster.  The comment in GNU's
    // Makefile explains: "They're ordered by size, so we use the
    // slowest-compiler on the smallest file and move to larger files
    // as the compiler gets faster."
    //
    // This MUST run before loaddefs generation, because
    // loaddefs-generate--emacs-batch loads bytecomp.el which loads
    // byte-opt.el.  Without compiled .elc files, the pcase macro
    // expansion in byte-opt.el runs as interpreted elisp and hangs.
    // ---------------------------------------------------------------
    let compile_first_sources =
        parse_compile_first_sources(&paths.makefile_in, &paths.lisp_root, options.native_comp)?;
    let compile_first_sources: Vec<PathBuf> = compile_first_sources
        .into_iter()
        .filter(|source| options.dry_run || compile_first_needs_rebuild(source))
        .collect();
    // Compile one file at a time, each in its own bootstrap-neomacs
    // process.  This matches GNU's make suffix rule which runs
    // `$(emacs) -f batch-byte-compile $<` per file.  Each process
    // picks up the .elc files from previous compilations.
    for source in &compile_first_sources {
        let compile_args = compile_first_args_for_source(options.native_comp, source);
        run_command(
            options,
            &options.repo_root,
            &paths.bootstrap,
            &compile_args,
            &envs,
        )?;
    }

    // ---------------------------------------------------------------
    // Loaddefs generation: uses the now-compiled .elc files.
    //
    // This mirrors GNU lisp/Makefile.in's `autoloads-force` target:
    // bootstrap-neomacs loads loaddefs-gen.elc and runs loaddefs-generate
    // with GENERATE-FULL non-nil.  The same call writes lisp/loaddefs.el,
    // lisp/theme-loaddefs.el, and secondary loaddefs such as
    // org/org-loaddefs.el and dired-loaddefs.el.
    // ---------------------------------------------------------------
    let loaddefs_gen = paths.lisp_root.join("emacs-lisp/loaddefs-gen.el");
    let loaddefs_dirs = loaddefs_dirs(&paths.lisp_root)?;
    let loaddefs_args = loaddefs_generation_args(&loaddefs_gen, &loaddefs_dirs);
    remove_primary_loaddefs_for_regeneration(options, &paths, &loaddefs_el, &theme_loaddefs_el)?;
    // Remove secondary loaddefs from previous builds at the same phase as the
    // full regeneration, so stale generated files cannot influence the new set.
    remove_stale_secondary_loaddefs(options, &paths)?;
    run_command(
        options,
        &options.repo_root,
        &paths.bootstrap,
        &loaddefs_args,
        &envs,
    )?;

    print_synthetic_step(&format!(
        "generate {} from {}",
        ldefs_boot.display(),
        loaddefs_el.display()
    ));
    if !options.dry_run {
        validate_primary_loaddefs(&loaddefs_el)?;
        write_ldefs_boot(&loaddefs_el, &ldefs_boot)?;
    }

    // GNU lisp/Makefile.in runs compile-main after regenerated autoloads and
    // before the final dump.  Leave the resulting .elc files in place so the
    // final pdump and runtime `load' path see bytecode before source, matching
    // GNU lread.c's default `load-suffixes' order.
    run_compile_main(options, &paths, &envs)?;

    run_command(
        options,
        &options.repo_root,
        &paths.temacs,
        &[
            OsString::from("--batch"),
            OsString::from("-l"),
            OsString::from("loadup"),
            OsString::from("--temacs=pdump"),
        ],
        &envs,
    )?;

    Ok(())
}

fn loaddefs_generation_args(loaddefs_gen: &Path, loaddefs_dirs: &[PathBuf]) -> Vec<OsString> {
    let mut loaddefs_args = vec![
        OsString::from("--batch"),
        OsString::from("-l"),
        loaddefs_gen.as_os_str().to_os_string(),
        OsString::from("--eval"),
        force_loaddefs_generate_eval(),
        OsString::from("-f"),
        OsString::from("neomacs-loaddefs-generate--force"),
    ];
    loaddefs_args.extend(
        loaddefs_dirs
            .iter()
            .map(|path| path.as_os_str().to_os_string()),
    );
    loaddefs_args
}

fn force_loaddefs_generate_eval() -> OsString {
    OsString::from(
        r#"(defun neomacs-loaddefs-generate--force ()
  (let* ((args (mapcar #'file-truename command-line-args-left))
         (default-directory (file-truename lisp-directory))
         (output-file (expand-file-name "loaddefs.el")))
    (setq command-line-args-left nil)
    (loaddefs-generate
     args output-file
     (loaddefs-generate--excluded-files)
     nil t t)
    (let ((lisp-mode-autoload-regexp
           "^;;;###\\(\\(noexist\\)-\\)?\\(theme-autoload\\)"))
      (loaddefs-generate
       (expand-file-name "../etc/themes/")
       (expand-file-name "theme-loaddefs.el")))))"#,
    )
}

fn remove_stale_lisp_bytecode(options: &FreshBuildOptions, paths: &PipelinePaths) -> Result<()> {
    let files = generated_lisp_bytecode_files(&paths.lisp_root)?;
    if files.is_empty() {
        return Ok(());
    }

    print_synthetic_step("remove stale Lisp bytecode");
    if options.dry_run {
        println!(
            "  would remove {} .elc files under {}",
            files.len(),
            paths.lisp_root.display()
        );
        return Ok(());
    }

    let mut removed = 0usize;
    for file in &files {
        if remove_file_if_exists(file)? {
            removed += 1;
        }
    }
    println!("  INFO  removed {removed} stale .elc files");
    Ok(())
}

fn generated_lisp_bytecode_files(lisp_root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_lisp_bytecode_files(lisp_root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_lisp_bytecode_files(current: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = match fs::read_dir(current) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_lisp_bytecode_files(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "elc") {
            out.push(path);
        }
    }

    Ok(())
}

fn remove_stale_secondary_loaddefs(
    options: &FreshBuildOptions,
    paths: &PipelinePaths,
) -> Result<()> {
    let files = generated_secondary_loaddefs_files(&paths.lisp_root)?;
    if files.is_empty() {
        return Ok(());
    }

    print_synthetic_step("remove stale secondary loaddefs");
    if options.dry_run {
        for file in &files {
            println!("  would remove: {}", file.display());
        }
        return Ok(());
    }

    let mut removed = 0usize;
    for file in &files {
        if remove_file_if_exists(file)? {
            removed += 1;
        }
        if remove_file_if_exists(&file.with_extension("elc"))? {
            removed += 1;
        }
    }
    println!("  INFO  removed {removed} stale secondary loaddefs artifacts");
    Ok(())
}

fn remove_lisp_bytecode_without_source(
    options: &FreshBuildOptions,
    paths: &PipelinePaths,
) -> Result<()> {
    let files = generated_lisp_bytecode_files(&paths.lisp_root)?
        .into_iter()
        .filter(|file| !file.with_extension("el").is_file())
        .collect::<Vec<_>>();
    if files.is_empty() {
        return Ok(());
    }

    print_synthetic_step("compile-main clean stale Lisp bytecode");
    if options.dry_run {
        for file in &files {
            println!("  would remove: {}", file.display());
        }
        return Ok(());
    }

    let mut removed = 0usize;
    for file in &files {
        if remove_file_if_exists(file)? {
            removed += 1;
        }
    }
    println!("  INFO  removed {removed} stale compile-main .elc files");
    Ok(())
}

fn remove_primary_loaddefs_for_regeneration(
    options: &FreshBuildOptions,
    paths: &PipelinePaths,
    loaddefs_el: &Path,
    theme_loaddefs_el: &Path,
) -> Result<()> {
    print_synthetic_step("force full primary loaddefs regeneration");
    let files = [
        loaddefs_el.to_path_buf(),
        theme_loaddefs_el.to_path_buf(),
        loaddefs_el.with_extension("elc"),
        theme_loaddefs_el.with_extension("elc"),
        paths.lisp_root.join("ldefs-boot.elc"),
        paths.lisp_root.join("emacs-lisp/cl-loaddefs.elc"),
    ];

    if options.dry_run {
        for file in &files {
            println!("  would remove: {}", file.display());
        }
        return Ok(());
    }

    for file in &files {
        let _ = remove_file_if_exists(file)?;
    }
    Ok(())
}

fn generated_secondary_loaddefs_files(lisp_root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_secondary_loaddefs_files(lisp_root, lisp_root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_secondary_loaddefs_files(
    lisp_root: &Path,
    current: &Path,
    out: &mut Vec<PathBuf>,
) -> Result<()> {
    let entries = match fs::read_dir(current) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_secondary_loaddefs_files(lisp_root, &path, out)?;
        } else if is_generated_secondary_loaddefs_file(lisp_root, &path) {
            out.push(path);
        }
    }

    Ok(())
}

fn is_generated_secondary_loaddefs_file(lisp_root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(lisp_root) else {
        return false;
    };

    if matches!(
        relative,
        rel if rel == Path::new("loaddefs.el")
            || rel == Path::new("ldefs-boot.el")
            || rel == Path::new("theme-loaddefs.el")
            || rel == Path::new("emacs-lisp/cl-loaddefs.el")
    ) {
        return false;
    }

    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    file_name == "loaddefs.el" || file_name.ends_with("-loaddefs.el")
}

fn remove_file_if_exists(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.into()),
    }
}

fn pipeline_paths(options: &FreshBuildOptions) -> PipelinePaths {
    let lisp_root = options.runtime_root.join("lisp");
    PipelinePaths {
        temacs: options.bin_dir.join("neomacs-temacs"),
        bootstrap: options.bin_dir.join("bootstrap-neomacs"),
        final_bin: options.bin_dir.join("neomacs"),
        makefile_in: lisp_root.join("Makefile.in"),
        lisp_root,
    }
}

fn ensure_runtime_inputs(paths: &PipelinePaths) -> Result<()> {
    for required in [
        paths.lisp_root.join("loadup.el"),
        paths.makefile_in.clone(),
        paths.lisp_root.join("emacs-lisp/loaddefs-gen.el"),
    ] {
        if !required.exists() {
            return Err(format!("missing required path: {}", required.display()).into());
        }
    }
    Ok(())
}

fn ensure_binaries_exist(paths: &PipelinePaths) -> Result<()> {
    for binary in [&paths.temacs, &paths.bootstrap, &paths.final_bin] {
        if !binary.exists() {
            return Err(format!("missing required path: {}", binary.display()).into());
        }
    }
    Ok(())
}

fn fresh_build_fingerprint_binaries(paths: &PipelinePaths) -> [&Path; 3] {
    [
        paths.temacs.as_path(),
        paths.bootstrap.as_path(),
        paths.final_bin.as_path(),
    ]
}

fn patch_executable_fingerprints(options: &FreshBuildOptions, paths: &PipelinePaths) -> Result<()> {
    // GNU Emacs hashes and patches the just-linked temacs image, then uses
    // that same executable image as bootstrap-emacs/emacs. Neomacs currently
    // builds three Rust binaries for those roles, so fresh-build gives the
    // executable family one shared patched fingerprint.
    let binaries = fresh_build_fingerprint_binaries(paths);
    print_synthetic_step("patch executable pdump fingerprint");
    if options.dry_run {
        for binary in binaries {
            println!("  would patch: {}", binary.display());
        }
        return Ok(());
    }

    ensure_binaries_exist(paths)?;
    let fingerprint = executable_family_fingerprint(&binaries)?;
    for binary in binaries {
        patch_executable_fingerprint(binary, &fingerprint)?;
    }
    println!(
        "  INFO  patched pdump fingerprint {}",
        uppercase_hex(&fingerprint)
    );
    Ok(())
}

fn executable_family_fingerprint(binaries: &[&Path]) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(b"neomacs-executable-fingerprint-v1\0");
    for binary in binaries {
        let bytes = fs::read(binary)?;
        let normalized = normalize_executable_fingerprint_slots(&bytes)
            .ok_or_else(|| format!("missing pdump fingerprint record in {}", binary.display()))?;
        hasher.update(binary.file_name().unwrap_or_default().as_encoded_bytes());
        hasher.update([0]);
        hasher.update(normalized);
        hasher.update([0xff]);
    }

    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    Ok(out)
}

fn normalize_executable_fingerprint_slots(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut normalized = bytes.to_vec();
    let mut found = false;
    for slot in executable_fingerprint_slots(bytes) {
        normalized[slot..slot + FINGERPRINT_PLACEHOLDER.len()]
            .copy_from_slice(FINGERPRINT_PLACEHOLDER);
        found = true;
    }
    found.then_some(normalized)
}

fn patch_executable_fingerprint(path: &Path, fingerprint: &[u8; 32]) -> Result<()> {
    let mut bytes = fs::read(path)?;
    let mut found = false;
    for slot in executable_fingerprint_slots(&bytes) {
        bytes[slot..slot + fingerprint.len()].copy_from_slice(fingerprint);
        found = true;
    }
    if !found {
        return Err(format!("missing pdump fingerprint record in {}", path.display()).into());
    }
    fs::write(path, bytes)?;
    Ok(())
}

fn executable_fingerprint_slots(bytes: &[u8]) -> Vec<usize> {
    let mut slots = Vec::new();
    let mut start = 0usize;
    while let Some(relative) = find_bytes(&bytes[start..], FINGERPRINT_MAGIC_START) {
        let record_start = start + relative;
        let slot_start = record_start + FINGERPRINT_MAGIC_START.len();
        let record_end = record_start + FINGERPRINT_RECORD_LEN;
        if record_end <= bytes.len()
            && &bytes[slot_start + FINGERPRINT_PLACEHOLDER.len()..record_end]
                == FINGERPRINT_MAGIC_END
        {
            slots.push(slot_start);
            start = record_end;
        } else {
            start = record_start + 1;
        }
    }
    slots
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn uppercase_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut out, "{byte:02X}").expect("write to string");
    }
    out
}

fn cargo_program() -> PathBuf {
    env::var_os("CARGO")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("cargo"))
}

fn run_command(
    options: &FreshBuildOptions,
    cwd: &Path,
    program: &Path,
    args: &[OsString],
    envs: &[(OsString, OsString)],
) -> Result<()> {
    print_command(program.as_os_str(), args);
    if options.dry_run {
        return Ok(());
    }

    let mut command = Command::new(program);
    command.current_dir(cwd);
    command.args(args.iter().map(OsString::as_os_str));
    command.envs(envs.iter().map(|(key, value)| (key, value)));

    let status = command.status()?;
    if !status.success() {
        return Err(command_failure(program, args, status).into());
    }
    Ok(())
}

fn command_failure(program: &Path, args: &[OsString], status: ExitStatus) -> String {
    let mut rendered = String::new();
    write!(
        &mut rendered,
        "command failed with status {status}: {}",
        shell_quote(program.as_os_str())
    )
    .expect("write to string");
    for arg in args {
        rendered.push(' ');
        rendered.push_str(&shell_quote(arg.as_os_str()));
    }
    rendered
}

fn print_command(program: &OsStr, args: &[OsString]) {
    let mut rendered = String::from("+ ");
    rendered.push_str(&shell_quote(program));
    for arg in args {
        rendered.push(' ');
        rendered.push_str(&shell_quote(arg.as_os_str()));
    }
    println!("{rendered}");
}

fn print_synthetic_step(message: &str) {
    println!("+ {message}");
}

fn shell_quote(value: &OsStr) -> String {
    let text = value.to_string_lossy();
    if text.is_empty()
        || text
            .chars()
            .any(|ch| ch.is_whitespace() || "'\"\\$`()[]{}*?&;<>|!".contains(ch))
    {
        format!("'{}'", text.replace('\'', "'\"'\"'"))
    } else {
        text.into_owned()
    }
}

fn loaddefs_dirs(lisp_root: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    collect_loaddefs_dirs(lisp_root, lisp_root, &mut dirs)?;
    dirs.sort();
    Ok(dirs)
}

fn collect_loaddefs_dirs(root: &Path, current: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    out.push(current.to_path_buf());

    let mut children = fs::read_dir(current)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    children.sort();

    for child in children {
        let relative = child
            .strip_prefix(root)
            .expect("child directory should remain under lisp root");
        let first_component = relative
            .components()
            .next()
            .and_then(|component| component.as_os_str().to_str());
        if matches!(first_component, Some("obsolete" | "term")) {
            continue;
        }
        collect_loaddefs_dirs(root, &child, out)?;
    }

    Ok(())
}

fn run_compile_main(
    options: &FreshBuildOptions,
    paths: &PipelinePaths,
    envs: &[(OsString, OsString)],
) -> Result<()> {
    remove_lisp_bytecode_without_source(options, paths)?;

    let main_first_sources = parse_main_first_sources(&paths.makefile_in, &paths.lisp_root)?;
    let mut seen = BTreeSet::new();
    let mut main_first = Vec::new();
    for source in main_first_sources {
        push_compile_main_source(source, &mut seen, &mut main_first)?;
    }
    let mut general = Vec::new();
    for source in compile_main_sources(&paths.lisp_root)? {
        if seen.contains(&source) {
            continue;
        }
        push_compile_main_source(source, &mut seen, &mut general)?;
    }

    let main_first = main_first
        .into_iter()
        .filter(|source| compile_main_needs_rebuild(source))
        .collect::<Vec<_>>();
    let general = general
        .into_iter()
        .filter(|source| compile_main_needs_rebuild(source))
        .collect::<Vec<_>>();

    if main_first.is_empty() && general.is_empty() {
        return Ok(());
    }

    print_synthetic_step("compile Lisp bytecode (GNU compile-main)");
    println!(
        "  INFO  byte-compiling {} .el files",
        main_first.len() + general.len()
    );
    let mut errors = Vec::new();

    if !main_first.is_empty() {
        println!(
            "  INFO  byte-compiling {} MAIN_FIRST .el files sequentially",
            main_first.len()
        );
        for source in &main_first {
            if let Err(e) = run_compile_main_source(options, paths, envs, source) {
                eprintln!("  WARN  byte-compile failed: {} ({})", source.display(), e);
                errors.push(source.display().to_string());
            }
        }
    }

    if !general.is_empty() {
        let dependencies = parse_compile_main_dependencies(&paths.makefile_in, &paths.lisp_root)?;
        let jobs = compile_main_jobs();
        println!(
            "  INFO  byte-compiling {} general .el files with {jobs} parallel jobs",
            general.len()
        );
        errors.extend(run_compile_main_parallel(
            options,
            paths,
            envs,
            general,
            &dependencies,
            jobs,
        )?);
    }

    if !errors.is_empty() {
        eprintln!("  WARN  {} files failed to byte-compile:", errors.len());
        for e in &errors {
            eprintln!("    - {}", e);
        }
    }

    Ok(())
}

fn compile_main_jobs() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
        .max(1)
}

fn run_compile_main_parallel(
    options: &FreshBuildOptions,
    paths: &PipelinePaths,
    envs: &[(OsString, OsString)],
    sources: Vec<PathBuf>,
    dependencies: &BTreeMap<PathBuf, BTreeSet<PathBuf>>,
    jobs: usize,
) -> Result<Vec<String>> {
    let pool = rayon::ThreadPoolBuilder::new().num_threads(jobs).build()?;
    let mut pending = sources.into_iter().collect::<BTreeSet<_>>();
    let mut errors = Vec::new();

    while !pending.is_empty() {
        let ready = pending
            .iter()
            .filter(|source| {
                dependencies
                    .get(*source)
                    .is_none_or(|deps| deps.iter().all(|dep| !pending.contains(dep)))
            })
            .cloned()
            .collect::<Vec<_>>();

        if ready.is_empty() {
            return Err(format!(
                "compile-main dependency cycle or missing wave among {} pending files",
                pending.len()
            )
            .into());
        }

        let wave_errors = if options.dry_run {
            ready
                .iter()
                .filter_map(|source| {
                    run_compile_main_source(options, paths, envs, source)
                        .err()
                        .map(|err| format!("{} ({err})", source.display()))
                })
                .collect::<Vec<_>>()
        } else {
            pool.install(|| {
                ready
                    .par_iter()
                    .filter_map(|source| {
                        run_compile_main_source(options, paths, envs, source)
                            .err()
                            .map(|err| format!("{} ({err})", source.display()))
                    })
                    .collect::<Vec<_>>()
            })
        };

        for source in ready {
            pending.remove(&source);
        }
        errors.extend(wave_errors);
    }

    Ok(errors)
}

fn run_compile_main_source(
    options: &FreshBuildOptions,
    paths: &PipelinePaths,
    envs: &[(OsString, OsString)],
    source: &Path,
) -> Result<()> {
    let args = compile_main_args_for_source(options.native_comp, source);
    run_command(options, &options.repo_root, &paths.bootstrap, &args, envs)
}

fn push_compile_main_source(
    source: PathBuf,
    seen: &mut BTreeSet<PathBuf>,
    out: &mut Vec<PathBuf>,
) -> Result<()> {
    if !source.is_file() {
        return Err(format!("compile-main source does not exist: {}", source.display()).into());
    }

    if seen.insert(source.clone()) {
        out.push(source);
    }
    Ok(())
}

fn parse_main_first_sources(makefile_in: &Path, lisp_root: &Path) -> Result<Vec<PathBuf>> {
    let contents = fs::read_to_string(makefile_in)?;
    Ok(parse_main_first_sources_from_str(&contents, lisp_root))
}

fn parse_main_first_sources_from_str(contents: &str, lisp_root: &Path) -> Vec<PathBuf> {
    let mut capture = false;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();

    for raw_line in contents.lines() {
        let line = raw_line.trim_end();
        if let Some(rest) = strip_makefile_assignment(line, "MAIN_FIRST") {
            capture = line.ends_with('\\');
            emit_lisp_source_paths(rest, lisp_root, &mut seen, &mut out);
            continue;
        }

        if capture {
            emit_lisp_source_paths(line, lisp_root, &mut seen, &mut out);
            capture = line.ends_with('\\');
        }
    }

    out
}

fn compile_main_sources(lisp_root: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    collect_lisp_dirs(lisp_root, &mut dirs)?;
    dirs.sort();

    let mut sources = Vec::new();
    for dir in dirs {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        let mut files = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.is_file()
                    && path.extension() == Some(OsStr::new("el"))
                    && !path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with('.'))
            })
            .collect::<Vec<_>>();
        files.sort();

        for source in files {
            if compile_main_should_consider(&source)? {
                sources.push(source);
            }
        }
    }

    Ok(sources)
}

fn collect_lisp_dirs(current: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    out.push(current.to_path_buf());

    let entries = match fs::read_dir(current) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };

    for entry in entries {
        let path = entry?.path();
        if path.is_dir() {
            collect_lisp_dirs(&path, out)?;
        }
    }

    Ok(())
}

fn compile_main_should_consider(source: &Path) -> Result<bool> {
    if source.with_extension("elc").is_file() {
        return Ok(true);
    }

    Ok(!source_has_no_byte_compile_marker(source)?)
}

fn compile_main_needs_rebuild(source: &Path) -> bool {
    if !compile_main_should_consider(source).unwrap_or(true) {
        return false;
    }
    bytecode_needs_rebuild(source)
}

fn source_has_no_byte_compile_marker(source: &Path) -> Result<bool> {
    let contents = fs::read(source)?;
    let contents = String::from_utf8_lossy(&contents);
    Ok(contents
        .lines()
        .any(|line| gnu_no_byte_compile_marker_line(line)))
}

fn parse_compile_main_dependencies(
    makefile_in: &Path,
    lisp_root: &Path,
) -> Result<BTreeMap<PathBuf, BTreeSet<PathBuf>>> {
    let contents = fs::read_to_string(makefile_in)?;
    Ok(parse_compile_main_dependencies_from_str(
        &contents, lisp_root,
    ))
}

fn parse_compile_main_dependencies_from_str(
    contents: &str,
    lisp_root: &Path,
) -> BTreeMap<PathBuf, BTreeSet<PathBuf>> {
    let mut dependencies = BTreeMap::new();
    let mut logical = String::new();

    for raw_line in contents.lines() {
        let line = raw_line.trim_end();
        let continuation = line.ends_with('\\');
        let fragment = line.strip_suffix('\\').unwrap_or(line);
        logical.push_str(fragment);
        logical.push(' ');

        if continuation {
            continue;
        }

        if let Some((targets, deps)) = logical.split_once(':') {
            let targets = compile_main_dependency_paths(targets, lisp_root);
            let deps = compile_main_dependency_paths(deps, lisp_root)
                .into_iter()
                .collect::<BTreeSet<_>>();
            if !targets.is_empty() && !deps.is_empty() {
                for target in targets {
                    dependencies
                        .entry(target)
                        .or_insert_with(BTreeSet::new)
                        .extend(deps.iter().cloned());
                }
            }
        }

        logical.clear();
    }

    dependencies
}

fn compile_main_dependency_paths(fragment: &str, lisp_root: &Path) -> Vec<PathBuf> {
    let normalized = fragment.replace('\\', " ");
    normalized
        .split_whitespace()
        .filter_map(|token| {
            let stripped = token.strip_prefix("$(lisp)/")?;
            let mut path = lisp_root.join(stripped);
            if path.extension() != Some(OsStr::new("elc")) {
                return None;
            }
            path.set_extension("el");
            Some(path)
        })
        .collect()
}

fn gnu_no_byte_compile_marker_line(line: &str) -> bool {
    if !line.starts_with(';') {
        return false;
    }

    let needle = "no-byte-compile:";
    let mut search_from = 0;
    while let Some(relative_index) = line[search_from..].find(needle) {
        let index = search_from + relative_index;
        let previous = line[..index].chars().next_back();
        if previous.is_some_and(|ch| !ch.is_ascii_alphabetic())
            && line[index + needle.len()..].trim_start().starts_with('t')
        {
            return true;
        }
        search_from = index + needle.len();
    }

    false
}

fn compile_main_args_for_source(native_comp: bool, source: &Path) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("--batch"),
        OsString::from("--no-site-file"),
        OsString::from("--no-site-lisp"),
        OsString::from("--eval"),
        OsString::from("(setq load-prefer-newer t byte-compile-warnings 'all)"),
        OsString::from("--eval"),
        OsString::from("(setq org--inhibit-version-check t)"),
    ];
    if native_comp {
        args.push(OsString::from("-l"));
        args.push(OsString::from("comp"));
        args.push(OsString::from("-f"));
        args.push(OsString::from("batch-byte+native-compile"));
    } else {
        args.push(OsString::from("-f"));
        args.push(OsString::from("batch-byte-compile"));
    }
    args.push(source.as_os_str().to_os_string());
    args
}

fn parse_compile_first_sources(
    makefile_in: &Path,
    lisp_root: &Path,
    native_comp: bool,
) -> Result<Vec<PathBuf>> {
    let contents = fs::read_to_string(makefile_in)?;
    Ok(parse_compile_first_sources_from_str(
        &contents,
        lisp_root,
        native_comp,
    ))
}

fn parse_compile_first_sources_from_str(
    contents: &str,
    lisp_root: &Path,
    native_comp: bool,
) -> Vec<PathBuf> {
    let mut capture = false;
    let mut in_native_block = false;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();

    for raw_line in contents.lines() {
        let line = raw_line.trim_end();
        if line == "ifeq ($(HAVE_NATIVE_COMP),yes)" {
            in_native_block = true;
            continue;
        }
        if line == "endif" {
            in_native_block = false;
            continue;
        }

        if let Some(rest) = strip_compile_first_assignment(line) {
            if in_native_block && !native_comp {
                capture = line.ends_with('\\');
                continue;
            }
            capture = line.ends_with('\\');
            emit_compile_first_paths(rest, lisp_root, &mut seen, &mut out);
            continue;
        }

        if capture {
            emit_compile_first_paths(line, lisp_root, &mut seen, &mut out);
            capture = line.ends_with('\\');
        }
    }

    out.into_iter().filter(|path| path.is_file()).collect()
}

/// Return true if `source` (a .el file) needs to be byte-compiled because
/// its .elc sibling is missing or older.  Mirrors what GNU make would do
/// for a `%.elc: %.el` pattern rule under lisp/Makefile.in.
fn compile_first_needs_rebuild(source: &Path) -> bool {
    bytecode_needs_rebuild(source)
}

fn bytecode_needs_rebuild(source: &Path) -> bool {
    let elc = source.with_extension("elc");
    let Ok(source_meta) = fs::metadata(source) else {
        // Can't stat the source — let the compiler surface the
        // error rather than silently skipping it.
        return true;
    };
    let Ok(elc_meta) = fs::metadata(&elc) else {
        return true; // .elc missing
    };
    let source_mtime = source_meta.modified().ok();
    let elc_mtime = elc_meta.modified().ok();
    match (source_mtime, elc_mtime) {
        (Some(s), Some(e)) => s > e,
        _ => true,
    }
}

fn compile_first_args_for_source(native_comp: bool, source: &Path) -> Vec<OsString> {
    compile_first_args_for_sources(native_comp, std::slice::from_ref(&source.to_path_buf()))
}

fn compile_first_args_for_sources(native_comp: bool, sources: &[PathBuf]) -> Vec<OsString> {
    let mut args = vec![OsString::from("--batch")];
    if native_comp {
        args.push(OsString::from("-l"));
        args.push(OsString::from("comp"));
    }
    args.push(OsString::from("-f"));
    args.push(OsString::from("batch-byte-compile"));
    for source in sources {
        args.push(source.as_os_str().to_os_string());
    }
    args
}

fn strip_compile_first_assignment(line: &str) -> Option<&str> {
    strip_makefile_assignment(line, "COMPILE_FIRST")
}

fn emit_compile_first_paths(
    fragment: &str,
    lisp_root: &Path,
    seen: &mut BTreeSet<PathBuf>,
    out: &mut Vec<PathBuf>,
) {
    emit_lisp_source_paths(fragment, lisp_root, seen, out)
}

fn strip_makefile_assignment<'a>(line: &'a str, variable: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(variable)?;
    let rest = rest.trim_start();
    rest.strip_prefix("+=")
        .or_else(|| rest.strip_prefix('='))
        .map(str::trim_start)
}

fn emit_lisp_source_paths(
    fragment: &str,
    lisp_root: &Path,
    seen: &mut BTreeSet<PathBuf>,
    out: &mut Vec<PathBuf>,
) {
    let normalized = fragment.replace('\\', " ");
    for token in normalized.split_whitespace() {
        let Some(stripped) = token
            .strip_prefix("$(lisp)/")
            .or_else(|| token.strip_prefix("./"))
        else {
            continue;
        };
        let mut path = lisp_root.join(stripped);
        if path.extension() == Some(OsStr::new("elc")) {
            path.set_extension("el");
        }
        if seen.insert(path.clone()) {
            out.push(path);
        }
    }
}

fn write_ldefs_boot(loaddefs_el: &Path, ldefs_boot: &Path) -> Result<()> {
    let input = fs::read_to_string(loaddefs_el)?;
    let output = inject_no_byte_compile(&input);
    fs::write(ldefs_boot, output)?;
    Ok(())
}

const LOADDEFS_END_BOUNDARY: &str = "\n\x0c\n;;; End of scraped data";
const GNU_EBROWSE_DECLARATION_AUTOLOAD: &str = concat!(
    "(autoload 'ebrowse-tags-find-declaration \"ebrowse\" \"\\",
    "\nFind declaration of member at point.\" t)"
);
const MISPLACED_EBROWSE_DECLARATION_DOCSTRING: &str =
    "Find declaration of member at point.\"\x0c\n;;; End of scraped data";

fn validate_primary_loaddefs(loaddefs_el: &Path) -> Result<()> {
    let contents = fs::read_to_string(loaddefs_el)
        .map_err(|err| format!("read generated {}: {err}", loaddefs_el.display()))?;
    validate_primary_loaddefs_contents(&contents).map_err(|err| -> DynError {
        format!("validate generated {}: {err}", loaddefs_el.display()).into()
    })
}

fn validate_primary_loaddefs_contents(contents: &str) -> Result<()> {
    if contents.contains(MISPLACED_EBROWSE_DECLARATION_DOCSTRING) {
        return Err("generated loaddefs.el moved an ebrowse docstring to the final page".into());
    }

    if !contents.contains(LOADDEFS_END_BOUNDARY) {
        return Err(format!(
            "generated loaddefs.el is missing GNU end boundary {:?}",
            LOADDEFS_END_BOUNDARY
        )
        .into());
    }

    if !contents.contains(GNU_EBROWSE_DECLARATION_AUTOLOAD) {
        return Err(
            "generated loaddefs.el is missing GNU ebrowse autoload docstring layout".into(),
        );
    }

    Ok(())
}

fn inject_no_byte_compile(contents: &str) -> String {
    let needle = ";; Local Variables:";
    if let Some(index) = contents.find(needle) {
        let insert_at = index + needle.len();
        let mut output = String::with_capacity(contents.len() + 24);
        output.push_str(&contents[..insert_at]);
        output.push('\n');
        output.push_str(";; no-byte-compile: t");
        output.push_str(&contents[insert_at..]);
        output
    } else {
        let mut output = contents.to_string();
        if !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(";; Local Variables:\n");
        output.push_str(";; no-byte-compile: t\n");
        output.push_str(";; End:\n");
        output
    }
}

fn print_usage() {
    print!("{}", usage_text());
}

fn usage_text() -> &'static str {
    "\
Usage: cargo xtask [fresh-build] [--bin-dir DIR] [--runtime-root DIR] [--release] [--dry-run] [--native-comp|--no-native-comp] [--skip-build]

Build the GNU-shaped Neomacs runtime pipeline:
  1. cargo build -p neomacs-bin [--release]
  2. neomacs-temacs --temacs=pbootstrap
  3. bootstrap-neomacs byte-compiles the GNU COMPILE_FIRST set into .elc files
  4. bootstrap-neomacs generates loaddefs / ldefs-boot
  5. bootstrap-neomacs byte-compiles the GNU compile-main Lisp set into .elc files
  6. neomacs-temacs --temacs=pdump

Options:
  --bin-dir DIR       Directory containing neomacs-temacs/bootstrap-neomacs/neomacs
  --runtime-root DIR  Runtime root containing lisp/ and etc/
  --release           Build neomacs-bin in release mode and use target/release by default
  --dry-run           Print planned commands without running them
  --native-comp       Include native-comp-only COMPILE_FIRST entries
  --no-native-comp    Exclude native-comp-only COMPILE_FIRST entries
  --skip-build        Skip the initial cargo build -p neomacs-bin stage

Environment:
  NEOMACS_NATIVE_COMP=yes
      Include the native-comp-only COMPILE_FIRST entries from lisp/Makefile.in.
"
}

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;
