use std::{env, path::PathBuf};

fn main() {
    let root = env::var_os("GHOSTTY_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ghostty"));

    let mut shim = cc::Build::new();
    shim.file("src/host_shim.c")
        .include(root.join("include"))
        .warnings(true)
        .compile("nterm_shim");

    let library_dir = env::var_os("GHOSTTY_LIB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("out/lib"));
    println!("cargo:rustc-link-search=native={}", library_dir.display());
    println!("cargo:rerun-if-env-changed=GHOSTTY_LIB_DIR");
    println!("cargo:rustc-link-lib=dylib=ghostty");
    if cfg!(target_family = "unix") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib");
    }
    // Ghostty creates surfaces on the UI thread, deep inside egui frames, and
    // its surface setup uses large stack frames. MSVC's 1 MiB default
    // overflowed when creating a floating pane.
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/STACK:8388608");
    }
    println!("cargo:rerun-if-changed=src/host_shim.c");
    println!(
        "cargo:rerun-if-changed={}",
        root.join("include/ghostty.h").display()
    );
}
