# Building and installing

## Requirements

- **Git**, to clone the repository and its `ghostty` submodule. On Windows,
  Git for Windows also supplies the `tic` used for the terminfo database.
- **Zig 0.17.0**, the `minimum_zig_version` in `ghostty/build.zig.zon`.
- **Rust** stable, 1.85 or newer for the 2024 edition, with Cargo. On Windows
  use the MSVC toolchain (`x86_64-pc-windows-msvc`).
- **A C compiler** for the `cc` build dependency, which compiles
  `src/host_shim.c` and the vendored Lua 5.4: the MSVC Build Tools on Windows,
  gcc or clang on Linux.
- **The .NET SDK** on Windows only, for `dotnet nuget verify` on the pinned
  ConPTY package (see below).

The development builds use Zig 0.17.0, Rust 1.95.0, and .NET SDK 10.0.

## Building

Ghostty is built from the `ghostty` submodule, a fork that adds the embedding
APIs this app uses. Clone with `--recursive`, or run
`git submodule update --init` in an existing clone.

```powershell
.\build.ps1
.\out\bin\nterm.exe --command=pwsh.exe
```

`build.ps1` runs `zig build host-lib` in the submodule, which installs the
Ghostty libraries, resources, and ConPTY into `out/`. It then builds this crate
with Cargo and copies the executable and Lua modules beside them.

## Windows

Windows builds compile the bundled `xterm-ghostty` terminfo database using
`tic` from Git for Windows or PATH. For another installation, pass
`-Dterminfo-compiler="C:/path/to/tic.exe"`. The app sets `TERM`, `COLORTERM`,
and an MSYS-compatible `TERMINFO` path for its shell, so Git's `less` can
recognize the terminal without a PowerShell override.

To build and deploy the Windows sample to another directory:

```powershell
.\deploy.ps1 C:\Apps\Nterm
```

`build.ps1` also installs the matched ConPTY DLL and console host into
`out/bin`. Build and deployment share the package pin in the submodule's
`dist/windows/conpty.ps1`.
The script verifies pinned SHA-256 hashes for the downloaded or cached package
before extraction, and for both extracted components before copying them.
A mismatch stops installation. The helper also requires the .NET SDK and runs
`dotnet nuget verify` before extraction or installation, requiring the pinned
Microsoft signing certificate and valid package signatures. Updating ConPTY
requires reviewing the version, hashes, and signing certificate together.

The script copies `nterm.exe`, its required `ghostty.dll`, and the matched
ConPTY `1.25.260710002-preview` DLL and console host. It downloads the pinned
Microsoft NuGet package on first use and caches it under `ghostty/.zig-cache/conpty`.
Use `-SkipBuild` to copy an existing build; the pinned ConPTY components are
still deployed. Close running instances before replacing their binaries.
It leaves any `nterm.lua` in the
destination untouched; the app also checks the home directory for that file.

## Linux

On Linux, use `build.sh`:

```sh
./build.sh
./out/bin/nterm --command="$SHELL"
```

To install it with a desktop entry for application launchers (such as
awesome's menubar), and link `npick` into `~/.local/bin`, run
`deploy.sh`. It builds,
installs to `~/.local/opt/nterm` (or the folder given as its argument),
and writes `~/.local/share/applications/nterm.desktop`. Pass
`--skip-build` to install the existing build. Running instances are unaffected.

## Preview VT library

Preview parsing uses NFM's `nfm-preview-vt` crate and the standalone VT library
produced by the Ghostty submodule. Cargo builds use `.cargo/config.toml`, which
points at `ghostty/include` and `out/lib`. Run `build.ps1` or `build.sh` once
first so those libraries exist. Explicit
`NFM_GHOSTTY_VT_INCLUDE_DIR`, `NFM_GHOSTTY_VT_LIB_DIR`, and
`NFM_GHOSTTY_VT_LIB_NAME` environment variables can select another VT build.
NFM dependencies use the Git revision pinned in `Cargo.toml`. No companion
checkout or machine-specific path override is required.

## Testing

On Windows, run `python tests/multiwindow_smoke.py`
after building to check the shortcut, multiple windows in one process, floating
panes, resizing, shell exit, and closing the original window before the others.
The check briefly opens and focuses test windows, then closes them.
Add `--memory-report .zig-cache/shared-ui-memory.json` to record private working
set, total working set, and private bytes for one window, four windows, and four
windows with floating panes. Use `--executable <path>` to compare another build.

