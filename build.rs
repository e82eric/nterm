use std::{env, path::PathBuf};

fn main() {
    let root = env::var_os("GHOSTTY_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ghostty"));

    let mut shim = cc::Build::new();
    shim.file("src/ghostty_shim.c")
        .include(root.join("include"))
        .warnings(true)
        .compile("ghostty_egui_shim");

    let library_dir = env::var_os("GHOSTTY_LIB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("out/lib"));
    println!("cargo:rustc-link-search=native={}", library_dir.display());
    println!("cargo:rerun-if-env-changed=GHOSTTY_LIB_DIR");
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
