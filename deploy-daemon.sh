#!/usr/bin/env bash
# Build nterm-daemon and npick and install them for this user, so that an
# nterm window on another machine can run its shells here: `nterm --remote`
# runs `ssh HOST nterm-daemon --relay`. The counterpart of deploy.sh for a
# machine that runs shells and shows none.
#
# Usage: deploy-daemon.sh [--skip-build] [PREFIX]
#   PREFIX defaults to ~/.local, and gets
#     bin/nterm-daemon, bin/npick
#     share/nterm/lua      the modules nterm-daemon.lua can require
#     share/nterm/bash     nterm.bash, for ~/.bashrc (see there)
#     share/terminfo       xterm-ghostty, which the daemon's shells are given
#
# The build is build.sh's: libghostty from the ghostty submodule (zig), then
# the Rust. Building libghostty takes more memory than a small machine may
# have (4 GB is not enough without swap); --skip-build installs what out/ and
# target/release already hold.
#
# An ssh that runs a command reads no ~/.profile, so its PATH may not have
# PREFIX/bin: a remote's entry in the window's configuration then names the
# daemon, e.g.  daemon = "/home/me/.local/bin/nterm-daemon".
set -euo pipefail

usage() {
    echo "Usage: $0 [--skip-build] [PREFIX]" >&2
    exit 2
}

skip_build=false
prefix=
for arg in "$@"; do
    case "$arg" in
        --skip-build) skip_build=true ;;
        -h | --help) usage ;;
        -*) echo "Unknown option: $arg" >&2; usage ;;
        *)
            [[ -z $prefix ]] || usage
            prefix=$arg
            ;;
    esac
done
prefix=$(realpath -m "${prefix:-$HOME/.local}")

root=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
output="$root/out"
release="${CARGO_TARGET_DIR:-$root/target}/release"

if ! $skip_build; then
    (cd "$root/ghostty" && zig build host-lib -Dapp-runtime=none -Doptimize=ReleaseFast --prefix "$output")
    NFM_GHOSTTY_VT_LIB_NAME=ghostty-vt cargo build --release --manifest-path "$root/Cargo.toml" \
        -p nterm-daemon -p nterm --bin nterm-daemon --bin npick
fi

for required in "$release/nterm-daemon" "$release/npick" "$output/share/terminfo/x/xterm-ghostty"; do
    if [[ ! -e $required ]]; then
        echo "Missing build output: $required" >&2
        exit 1
    fi
done

# A daemon that is running keeps the program it started from: install puts
# the new one in place of the name, and the next daemon started is the new.
mkdir -p "$prefix/bin" "$prefix/share/nterm/bash" "$prefix/share/terminfo"
install -m 755 "$release/nterm-daemon" "$release/npick" "$prefix/bin/"
rm -rf "$prefix/share/nterm/lua"
cp -R "$root/lua" "$prefix/share/nterm/lua"
install -m 644 "$root/bash/nterm.bash" "$root/npick-completion.bash" "$prefix/share/nterm/bash/"
cp -R "$output/share/terminfo/." "$prefix/share/terminfo/"

echo "Installed $prefix/bin/nterm-daemon and $prefix/bin/npick"
echo "For bash, add to ~/.bashrc:"
echo "  [[ -n \$NTERM_SHARE && -r \$NTERM_SHARE/bash/nterm.bash ]] && . \"\$NTERM_SHARE/bash/nterm.bash\""
