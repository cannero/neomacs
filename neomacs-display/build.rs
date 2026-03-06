//! Build script for neomacs-display C bridge.
//!
//! Generates C headers for the FFI entrypoints with cbindgen.

use std::env;
use std::path::PathBuf;

fn main() {
    let crate_dir = env::var("CARGO_MANIFEST_DIR").unwrap();

    // On macOS, unresolved symbols are provided by the embedding Emacs binary.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "macos" {
        println!("cargo:rustc-cdylib-link-arg=-Wl,-undefined,dynamic_lookup");
    }

    // Rust's pre-built std for x86_64-pc-windows-gnu expects -l:libpthread.a.
    // No actual pthread symbols are referenced, so an empty stub suffices.
    if target_os == "windows" {
        let stubs = PathBuf::from(&crate_dir).join(".mingw-stubs");
        if stubs.exists() {
            println!("cargo:rustc-link-search=native={}", stubs.display());
        }
    }

    generate_c_headers(&crate_dir);
}

fn generate_c_headers(crate_dir: &str) {
    if which::which("cbindgen").is_ok() {
        let output_file = PathBuf::from(crate_dir)
            .join("include")
            .join("neomacs_display.h");

        std::fs::create_dir_all(PathBuf::from(crate_dir).join("include")).ok();

        let config = cbindgen::Config::from_file("cbindgen.toml").unwrap_or_default();

        cbindgen::Builder::new()
            .with_crate(crate_dir)
            .with_config(config)
            .generate()
            .map(|bindings| bindings.write_to_file(&output_file))
            .ok();

        println!("cargo:rerun-if-changed=src/ffi");
        println!("cargo:rerun-if-changed=cbindgen.toml");
    }
}
