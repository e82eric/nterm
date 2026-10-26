#!/usr/bin/env bash
# Build the egui sample and install it with a desktop entry, so launchers such
# as awesome's menubar can start it, and link npick into ~/.local/bin. The
# Linux counterpart of deploy.ps1.
#
# Usage: deploy.sh [--skip-build] [DESTINATION]
#   DESTINATION defaults to ~/.local/opt/nterm.
set -euo pipefail

usage() {
    echo "Usage: $0 [--skip-build] [DESTINATION]" >&2
    exit 2
}

skip_build=false
destination=
for arg in "$@"; do
    case "$arg" in
        --skip-build) skip_build=true ;;
        -h | --help) usage ;;
        -*) echo "Unknown option: $arg" >&2; usage ;;
        *)
            [[ -z $destination ]] || usage
            destination=$arg
            ;;
    esac
done
destination=$(realpath -m "${destination:-$HOME/.local/opt/nterm}")

root=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
output="$root/out"

if ! $skip_build; then
    "$root/build.sh"
fi

# The binary loads libghostty.so from $ORIGIN/../lib, and Ghostty finds its
# resources (share/ghostty) beside the terminfo sentinel in share/terminfo.
icon=share/icons/hicolor/128x128/apps/com.mitchellh.ghostty.png
for required in bin/nterm bin/npick lib/libghostty.so share/ghostty/themes \
    share/nterm/lua/nterm/git.lua \
    share/ghostty/shell-integration share/terminfo/x/xterm-ghostty "$icon"; do
    if [[ ! -e $output/$required ]]; then
        echo "Missing build output: $output/$required" >&2
        exit 1
    fi
done

# Stage the complete installation, then swap it into place. Running instances
# keep the files they already opened, and a failed copy leaves the previous
# installation untouched.
parent=$(dirname "$destination")
mkdir -p "$parent"
stage=$(mktemp -d "$parent/.nterm.XXXXXX")
trap 'rm -rf "$stage"' EXIT
chmod 755 "$stage"
mkdir -p "$stage/bin" "$stage/lib" "$stage/share" "$stage/$(dirname "$icon")"
cp "$output/bin/nterm" "$output/bin/npick" "$stage/bin/"
cp "$output/lib/libghostty.so" "$stage/lib/"
cp -R "$output/share/ghostty" "$output/share/terminfo" "$stage/share/"
cp "$output/$icon" "$stage/$icon"
# Lua modules for require() in nterm.lua, and the Ctrl+Space completion
# picker for bash (source it from ~/.bashrc).
mkdir -p "$stage/share/nterm"
cp -R "$output/share/nterm/lua" "$stage/share/nterm/"
cp "$root/npick-completion.bash" "$stage/share/nterm/"

if [[ -e $destination ]]; then
    previous="$stage.previous"
    mv "$destination" "$previous"
    mv "$stage" "$destination"
    rm -rf "$previous"
else
    mv "$stage" "$destination"
fi
trap - EXIT

# Desktop entry for application launchers.
applications="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
entry="$applications/nterm.desktop"
mkdir -p "$applications"
cat >"$entry.tmp" <<EOF
[Desktop Entry]
Type=Application
Name=nterm
Comment=Terminal built on libghostty
Exec="$destination/bin/nterm"
Icon=$destination/$icon
Terminal=false
Categories=System;TerminalEmulator;
EOF
mv "$entry.tmp" "$entry"
if command -v desktop-file-validate >/dev/null; then
    desktop-file-validate "$entry"
fi
if command -v update-desktop-database >/dev/null; then
    update-desktop-database "$applications" || true
fi

# npick runs inside the terminal's shell, so put it on the PATH.
bin_dir="$HOME/.local/bin"
mkdir -p "$bin_dir"
ln -sfn "$destination/bin/npick" "$bin_dir/npick"

echo "Installed $destination"
echo "Linked $bin_dir/npick"
echo "Desktop entry $entry"
echo "Restart awesome (or your launcher) once so it picks up the new entry."
