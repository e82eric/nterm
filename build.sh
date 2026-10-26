#!/usr/bin/env bash
# Build libghostty from the ghostty submodule and the Rust host, and install
# both into out/ (bin, lib, share) ready to run or deploy. The Linux
# counterpart of build.ps1.
#
# Usage: build.sh [OPTIMIZE]
#   OPTIMIZE is a zig -Doptimize mode and defaults to ReleaseFast.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ghostty="$root/ghostty"
prefix="$root/out"
if [[ ! -f $ghostty/build.zig ]]; then
    echo "Missing ghostty submodule: $ghostty. Run git submodule update --init." >&2
    exit 1
fi

# Libraries and resources.
(cd "$ghostty" && zig build host-lib -Dapp-runtime=none -Doptimize="${1:-ReleaseFast}" --prefix "$prefix")

# .cargo/config.toml points the build scripts at ghostty/ and out/lib; the
# VT library name differs from the Windows default.
NFM_GHOSTTY_VT_LIB_NAME=ghostty-vt cargo build --release --manifest-path "$root/Cargo.toml"

install -m 755 "$root/target/release/ghostty_egui" "$root/target/release/npick" "$prefix/bin/"
# Lua modules a config can require, e.g. require("ghostty.git").
mkdir -p "$prefix/share/ghostty-egui"
rm -rf "$prefix/share/ghostty-egui/lua"
cp -R "$root/lua" "$prefix/share/ghostty-egui/"

echo "Built $prefix/bin/ghostty_egui"
