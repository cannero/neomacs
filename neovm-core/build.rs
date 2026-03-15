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

fn should_hash_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(OsStr::to_str),
        Some("rs" | "el" | "elc" | "toml")
    )
}
