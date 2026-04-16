use std::collections::BTreeSet;
use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

type DynError = Box<dyn Error>;
type Result<T> = std::result::Result<T, DynError>;

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

    if !options.dry_run {
        ensure_binaries_exist(&paths)?;
    }

    let envs = [(
        OsString::from("NEOMACS_RUNTIME_ROOT"),
        options.runtime_root.as_os_str().to_os_string(),
    )];

    // ---------------------------------------------------------------
    // Pre-compile .el files with GNU Emacs.
    //
    // Neomacs loads .elc (byte-compiled) files ~17x faster than .el
    // source. GNU Emacs's byte compiler is also much faster than
    // neomacs's interpreted compiler. Use GNU Emacs to pre-compile
    // all bootstrap .el files before loadup, so neomacs loads fast
    // bytecoded files instead of interpreting source with expensive
    // macro expansion.
    //
    // This mirrors GNU Emacs's own build: `make` compiles all .el
    // files, then loads the .elc versions. The difference is GNU
    // bootstraps its own compiler; we use an external GNU Emacs.
    // ---------------------------------------------------------------
    gnu_emacs_precompile(options, &paths)?;

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
        .filter(|source| compile_first_needs_rebuild(source))
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
    // ---------------------------------------------------------------
    let loaddefs_gen = paths.lisp_root.join("emacs-lisp/loaddefs-gen.el");
    let loaddefs_dirs = loaddefs_dirs(&paths.lisp_root)?;
    let mut loaddefs_args = vec![
        OsString::from("--batch"),
        OsString::from("-l"),
        loaddefs_gen.as_os_str().to_os_string(),
        OsString::from("-f"),
        OsString::from("loaddefs-generate--emacs-batch"),
    ];
    loaddefs_args.extend(
        loaddefs_dirs
            .iter()
            .map(|path| path.as_os_str().to_os_string()),
    );
    run_command(
        options,
        &options.repo_root,
        &paths.bootstrap,
        &loaddefs_args,
        &envs,
    )?;

    let loaddefs_el = paths.lisp_root.join("loaddefs.el");
    let ldefs_boot = paths.lisp_root.join("ldefs-boot.el");
    print_synthetic_step(&format!(
        "generate {} from {}",
        ldefs_boot.display(),
        loaddefs_el.display()
    ));
    if !options.dry_run {
        write_ldefs_boot(&loaddefs_el, &ldefs_boot)?;
    }

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

/// Pre-compile bootstrap .el files using GNU Emacs.
///
/// Finds all .el files under lisp/ that would benefit from byte-compilation,
/// filters out files that already have a fresh .elc, and batch-compiles the
/// rest with GNU Emacs.
fn gnu_emacs_precompile(options: &FreshBuildOptions, paths: &PipelinePaths) -> Result<()> {
    let gnu_emacs = find_gnu_emacs();
    let Some(gnu_emacs) = gnu_emacs else {
        println!(
            "  SKIP  GNU Emacs pre-compilation (no GNU Emacs found; \
             set GNU_EMACS=/path/to/emacs to enable)"
        );
        return Ok(());
    };

    print_synthetic_step(&format!(
        "pre-compile .el → .elc with GNU Emacs ({})",
        gnu_emacs.display()
    ));

    // Collect all .el files under lisp/ that should be compiled.
    // Skip files with `no-byte-compile: t` in their header, and files
    // that already have a fresh .elc.
    let mut to_compile: Vec<PathBuf> = Vec::new();
    collect_compilable_el_files(&paths.lisp_root, &paths.lisp_root, &mut to_compile)?;
    to_compile.retain(|source| compile_first_needs_rebuild(source));

    if to_compile.is_empty() {
        println!("  SKIP  all .elc files are up to date");
        return Ok(());
    }

    println!("  INFO  compiling {} .el files", to_compile.len());

    if options.dry_run {
        for source in &to_compile {
            println!("  would compile: {}", source.display());
        }
        return Ok(());
    }

    // Build load-path args: -L for each unique directory containing .el files
    let mut load_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    for source in &to_compile {
        if let Some(parent) = source.parent() {
            load_dirs.insert(parent.to_path_buf());
        }
    }

    // Build load-path from the first-level subdirectories under lisp/,
    // matching GNU Emacs's default load-path structure. This avoids
    // shadowing issues from deep subdirectories (e.g., cedet/srecode/
    // compile.el shadowing progmodes/compile.el).
    let mut load_path_dirs: Vec<PathBuf> = vec![paths.lisp_root.clone()];
    if let Ok(entries) = fs::read_dir(&paths.lisp_root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                // Skip obsolete and term (matching GNU), and leim (separate tree)
                if !matches!(name, "obsolete" | "term") {
                    load_path_dirs.push(path);
                }
            }
        }
    }
    load_path_dirs.sort();

    let mut args: Vec<OsString> = vec![OsString::from("--batch")];
    for dir in &load_path_dirs {
        args.push(OsString::from("-L"));
        args.push(dir.as_os_str().to_os_string());
    }
    args.push(OsString::from("--eval"));
    args.push(OsString::from("(setq byte-compile-warnings nil)"));
    args.push(OsString::from("-f"));
    args.push(OsString::from("batch-byte-compile"));
    for source in &to_compile {
        args.push(source.as_os_str().to_os_string());
    }

    run_command(options, &options.repo_root, &gnu_emacs, &args, &[])?;

    let compiled = to_compile
        .iter()
        .filter(|s| s.with_extension("elc").exists())
        .count();
    println!("  INFO  compiled {compiled}/{} .el files", to_compile.len());

    Ok(())
}

/// Find a GNU Emacs binary for pre-compilation.
/// Checks: $GNU_EMACS env var, then `emacs` on PATH.
/// Validates that the found binary is actually GNU Emacs (not neomacs).
fn find_gnu_emacs() -> Option<PathBuf> {
    // Check GNU_EMACS env var first
    if let Some(path) = env::var_os("GNU_EMACS") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
    }

    // Try `emacs` on PATH — verify it's GNU Emacs
    let output = Command::new("emacs")
        .arg("--version")
        .output()
        .ok()?;
    let version = String::from_utf8_lossy(&output.stdout);
    if version.contains("GNU Emacs") {
        return Some(PathBuf::from("emacs"));
    }

    None
}

/// Collect all .el files under a directory tree that are candidates for
/// byte-compilation. Skips files with `no-byte-compile: t`.
fn collect_compilable_el_files(
    root: &Path,
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
            // Skip obsolete, term, leim/quail (large generated files)
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if matches!(name, "obsolete" | "term") {
                continue;
            }
            collect_compilable_el_files(root, &path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "el") {
            // Skip files with no-byte-compile: t
            if has_no_byte_compile_cookie(&path) {
                continue;
            }
            out.push(path);
        }
    }

    Ok(())
}

/// Check if a .el file has `no-byte-compile: t` in its header or tail.
fn has_no_byte_compile_cookie(path: &Path) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    let needle = b"no-byte-compile: t";
    // Check first 1024 bytes (header) and last 500 bytes (Local Variables)
    let header_end = bytes.len().min(1024);
    let tail_start = bytes.len().saturating_sub(500);
    bytes[..header_end]
        .windows(needle.len())
        .any(|w| w == needle)
        || bytes[tail_start..]
            .windows(needle.len())
            .any(|w| w == needle)
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
    let elc = source.with_extension("elc");
    let Ok(source_meta) = fs::metadata(source) else {
        // Can't stat the source — let bootstrap-neomacs surface the
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
    for prefix in [
        "COMPILE_FIRST +=",
        "COMPILE_FIRST +=",
        "COMPILE_FIRST =",
        "COMPILE_FIRST+=",
    ] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return Some(rest.trim_start());
        }
    }
    line.strip_prefix("COMPILE_FIRST+=")
        .map(str::trim_start)
        .or_else(|| line.strip_prefix("COMPILE_FIRST=").map(str::trim_start))
}

fn emit_compile_first_paths(
    fragment: &str,
    lisp_root: &Path,
    seen: &mut BTreeSet<PathBuf>,
    out: &mut Vec<PathBuf>,
) {
    let normalized = fragment.replace('\\', " ");
    for token in normalized.split_whitespace() {
        let Some(stripped) = token.strip_prefix("$(lisp)/") else {
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
  3. bootstrap-neomacs generates loaddefs / ldefs-boot
  4. bootstrap-neomacs byte-compiles the GNU COMPILE_FIRST set into .elc files
  5. neomacs-temacs --temacs=pdump

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
