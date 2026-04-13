use std::collections::BTreeSet;
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
    emit_pdump_fingerprint(project_root, &manifest_dir);
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("unicode-data").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        project_root.join("etc/rgb.txt").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("build_support/unicode_gen.rs").display()
    );
}

fn emit_pdump_fingerprint(project_root: &Path, manifest_dir: &Path) {
    let mut inputs = BTreeSet::new();
    for file in [
        project_root.join("Cargo.toml"),
        project_root.join("Cargo.lock"),
        manifest_dir.join("Cargo.toml"),
        manifest_dir.join("build.rs"),
        project_root.join("neomacs-bin/Cargo.toml"),
    ] {
        inputs.insert(file);
    }

    collect_fingerprint_inputs(&manifest_dir.join("src"), "rs", &mut inputs);
    collect_fingerprint_inputs(&project_root.join("neomacs-bin/src"), "rs", &mut inputs);
    collect_fingerprint_inputs(&project_root.join("lisp"), "el", &mut inputs);

    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("src").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        project_root.join("neomacs-bin/src").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        project_root.join("lisp").display()
    );
    println!("cargo:rerun-if-env-changed=PROFILE");
    println!("cargo:rerun-if-env-changed=TARGET");

    let mut hasher = Sha256::new();
    hasher.update(b"neomacs-pdump-fingerprint-v1\0");
    hasher.update(std::env::var("TARGET").unwrap_or_default().as_bytes());
    hasher.update([0]);
    hasher.update(std::env::var("PROFILE").unwrap_or_default().as_bytes());
    hasher.update([0]);

    for input in &inputs {
        println!("cargo:rerun-if-changed={}", input.display());
        let rel = input.strip_prefix(project_root).unwrap_or(input);
        hasher.update(rel.as_os_str().as_encoded_bytes());
        hasher.update([0]);
        let bytes = fs::read(input).unwrap_or_else(|err| {
            panic!(
                "failed to read pdump fingerprint input {}: {err}",
                input.display()
            )
        });
        hasher.update(bytes);
        hasher.update([0xff]);
    }

    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02X}");
    }
    println!("cargo:rustc-env=NEOVM_PDUMP_FINGERPRINT={hex}");
}

fn collect_fingerprint_inputs(dir: &Path, extension: &str, out: &mut BTreeSet<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    let mut children = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect::<Vec<_>>();
    children.sort();

    for child in children {
        if child.is_dir() {
            collect_fingerprint_inputs(&child, extension, out);
            continue;
        }
        if child.extension().and_then(|ext| ext.to_str()) == Some(extension) {
            out.insert(child);
        }
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
