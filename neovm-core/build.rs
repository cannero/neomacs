use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

#[path = "build_support/unicode_gen.rs"]
mod unicode_gen;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let project_root = manifest_dir.parent().expect("workspace root");

    unicode_gen::ensure_generated_unicode_lisp(&manifest_dir, project_root);
    generate_x11_color_table(project_root, &manifest_dir);

    let tracked_roots = [
        manifest_dir.join("src"),
        manifest_dir.join("unicode-data"),
        manifest_dir.join("Cargo.toml"),
        manifest_dir.join("build.rs"),
        project_root.join("lisp"),
    ];

    for root in &tracked_roots {
        println!("cargo:rerun-if-changed={}", root.display());
    }

    let mut files = Vec::new();
    collect_files(&tracked_roots, &mut files);
    files.sort();

    let mut hasher = Sha256::new();
    for path in &files {
        println!("cargo:rerun-if-changed={}", path.display());
        let rel = path.strip_prefix(project_root).unwrap_or(path);
        hasher.update(rel.as_os_str().as_encoded_bytes());
        hasher.update([0]);
        hasher.update(fs::read(path).unwrap_or_default());
        hasher.update([0xff]);
    }

    let digest = hasher.finalize();
    let seed = format!("{:x}", digest);
    println!("cargo:rustc-env=NEOVM_BOOTSTRAP_CACHE_SEED={}", &seed[..16]);
}

fn collect_files(roots: &[PathBuf], out: &mut Vec<PathBuf>) {
    for root in roots {
        collect_path(root, out);
    }
}

fn collect_path(path: &Path, out: &mut Vec<PathBuf>) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };

    if metadata.is_file() {
        if should_hash_file(path) {
            out.push(path.to_path_buf());
        }
        return;
    }

    let Ok(entries) = fs::read_dir(path) else {
        return;
    };

    let mut children = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .collect::<Vec<_>>();
    children.sort();
    for child in children {
        collect_path(&child, out);
    }
}

/// Parse etc/rgb.txt and generate a Rust source file with a static
/// color lookup function. This gives us the full X11 color database
/// (788 colors including grey0-grey100, DarkGoldenrod, etc.) with
/// zero runtime file I/O — the table is compiled into the binary.
fn generate_x11_color_table(project_root: &Path, manifest_dir: &Path) {
    let rgb_path = project_root.join("etc/rgb.txt");
    println!("cargo:rerun-if-changed={}", rgb_path.display());

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let out_path = out_dir.join("x11_colors.rs");

    let content = fs::read_to_string(&rgb_path).unwrap_or_else(|e| {
        eprintln!("cargo:warning=Cannot read {}: {}", rgb_path.display(), e);
        String::new()
    });

    // Parse rgb.txt: "R G B\t\tColorName"
    // Collect unique (lowercase_name -> (r, g, b)), also add no-space variants.
    let mut colors: std::collections::BTreeMap<String, (u8, u8, u8)> =
        std::collections::BTreeMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let r = parts.next().and_then(|s| s.parse::<u8>().ok());
        let g = parts.next().and_then(|s| s.parse::<u8>().ok());
        let b = parts.next().and_then(|s| s.parse::<u8>().ok());
        // Remaining is the color name (may contain spaces)
        let name: String = parts.collect::<Vec<_>>().join(" ");

        if let (Some(r), Some(g), Some(b)) = (r, g, b) {
            if !name.is_empty() {
                let lower = name.to_lowercase();
                let no_spaces = lower.replace(' ', "");
                colors.entry(lower.clone()).or_insert((r, g, b));
                if no_spaces != lower {
                    colors.entry(no_spaces).or_insert((r, g, b));
                }
            }
        }
    }

    // Generate Rust source: a function with a match statement.
    let mut code = String::new();
    code.push_str("/// Auto-generated from etc/rgb.txt — do not edit.\n");
    code.push_str("/// X11 color name lookup (case-insensitive).\n");
    code.push_str("pub fn x11_color_lookup(name: &str) -> Option<(u8, u8, u8)> {\n");
    code.push_str("    match name.to_lowercase().as_str() {\n");
    for (name, (r, g, b)) in &colors {
        code.push_str(&format!(
            "        {:?} => Some(({}, {}, {})),\n",
            name, r, g, b
        ));
    }
    code.push_str("        _ => None,\n");
    code.push_str("    }\n");
    code.push_str("}\n");

    fs::write(&out_path, &code).expect("Failed to write x11_colors.rs");
    eprintln!(
        "cargo:warning=Generated X11 color table: {} entries from {}",
        colors.len(),
        rgb_path.display()
    );
}

fn should_hash_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(OsStr::to_str),
        Some("rs" | "el" | "elc" | "toml")
    )
}
