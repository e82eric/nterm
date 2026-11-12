use std::{env, path::PathBuf};

fn main() {
    // The static libghostty-vt that `zig build host-lib` installs. The daemon
    // links nothing else of ghostty.
    let lib_dir = env::var_os("GHOSTTY_LIB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../out/lib"));
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rerun-if-env-changed=GHOSTTY_LIB_DIR");
    if cfg!(windows) {
        println!("cargo:rustc-link-lib=static=ghostty-vt-static");
        println!("cargo:rustc-link-lib=ntdll");
        println!(
            "cargo:rerun-if-changed={}",
            lib_dir.join("ghostty-vt-static.lib").display()
        );
    } else {
        println!("cargo:rustc-link-lib=static=ghostty-vt");
    }
}
