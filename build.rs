use std::{env, path::PathBuf};

fn main() {
    let root = env::var_os("GHOSTTY_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));

    let mut shim = cc::Build::new();
    if env::var_os("CARGO_FEATURE_VULKAN").is_some() {
        shim.define("GHOSTTY_EGUI_VULKAN", None);
    }
    shim.file("src/ghostty_shim.c")
        .include(root.join("include"))
        .warnings(true)
        .compile("ghostty_egui_shim");

    println!(
        "cargo:rustc-link-search=native={}",
        root.join("zig-out/lib").display()
    );
    println!("cargo:rustc-link-lib=dylib=ghostty");
    if cfg!(target_family = "unix") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib");
    }
    println!("cargo:rerun-if-changed=src/ghostty_shim.c");
    println!(
        "cargo:rerun-if-changed={}",
        root.join("include/ghostty.h").display()
    );
}
