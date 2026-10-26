# Build libghostty from the ghostty submodule and the Rust host, and install
# both into out/ (bin, lib, share) ready to run or deploy.
param(
    [ValidateSet('Debug', 'ReleaseSafe', 'ReleaseFast', 'ReleaseSmall')]
    [string] $Optimize = 'ReleaseFast'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$ghostty = Join-Path $PSScriptRoot 'ghostty'
$prefix = Join-Path $PSScriptRoot 'out'
if (-not (Test-Path -LiteralPath (Join-Path $ghostty 'build.zig') -PathType Leaf)) {
    throw "Missing ghostty submodule: $ghostty. Run git submodule update --init."
}

# Libraries, resources, and the pinned ConPTY runtime.
Push-Location -LiteralPath $ghostty
try {
    & zig build host-lib "-Doptimize=$Optimize" --prefix $prefix
    if ($LASTEXITCODE -ne 0) {
        throw "ghostty host-lib build failed with exit code $LASTEXITCODE"
    }
}
finally {
    Pop-Location
}

# .cargo/config.toml points the build scripts at ghostty/ and out/lib.
& cargo build --release --manifest-path (Join-Path $PSScriptRoot 'Cargo.toml')
if ($LASTEXITCODE -ne 0) {
    throw "cargo build failed with exit code $LASTEXITCODE"
}

$target = Join-Path $PSScriptRoot 'target\release'
Copy-Item -LiteralPath (Join-Path $target 'nterm.exe') -Destination (Join-Path $prefix 'bin') -Force
# Lua modules a config can require, e.g. require("nterm.git").
$share = Join-Path $prefix 'share\nterm'
$lua = Join-Path $share 'lua'
if (Test-Path -LiteralPath $lua) {
    Remove-Item -LiteralPath $lua -Recurse -Force
}
New-Item -ItemType Directory -Path $share -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'lua') -Destination $share -Recurse

Write-Host "Built $(Join-Path $prefix 'bin\nterm.exe')"
