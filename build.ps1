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

# An nterm or a session daemon running from out\bin holds its programs and
# libraries, which cannot then be written over. They can be renamed: what is
# running keeps the old files under their new names, and the build puts new
# ones in their places. The old files are deleted by the first build after
# whatever held them has ended.
$bin = Join-Path $prefix 'bin'
$aside = @()
if (Test-Path -LiteralPath $bin -PathType Container) {
    Get-ChildItem -LiteralPath $bin -File -Filter '*.old-*' |
        Remove-Item -Force -ErrorAction SilentlyContinue
    $stamp = Get-Date -Format 'yyyyMMddHHmmss'
    foreach ($file in Get-ChildItem -LiteralPath $bin -File | Where-Object { $_.Extension -in '.exe', '.dll' }) {
        try {
            # Opening it alone fails while anything has it loaded.
            [System.IO.File]::Open($file.FullName, 'Open', 'ReadWrite', 'None').Dispose()
        }
        catch [System.IO.IOException] {
            Rename-Item -LiteralPath $file.FullName -NewName "$($file.Name).old-$stamp"
            $aside += $file
        }
    }
}

try {
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
    # The session daemon is a default member of the workspace.
    foreach ($program in 'nterm.exe', 'nterm-daemon.exe') {
        Copy-Item -LiteralPath (Join-Path $target $program) -Destination $bin -Force
    }
}
finally {
    # A file the build did not make again goes back, also when the build
    # failed: something that was running may be all that had it.
    foreach ($file in $aside) {
        if (-not (Test-Path -LiteralPath $file.FullName)) {
            Copy-Item -LiteralPath "$($file.FullName).old-$stamp" -Destination $file.FullName
        }
    }
}
if ($aside) {
    Write-Warning "In use, so kept under another name until the next build: $($aside.Name -join ', '). What is running keeps the old build."
}
# Lua modules a config can require, e.g. require("nterm.git").
$share = Join-Path $prefix 'share\nterm'
$lua = Join-Path $share 'lua'
if (Test-Path -LiteralPath $lua) {
    Remove-Item -LiteralPath $lua -Recurse -Force
}
New-Item -ItemType Directory -Path $share -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'lua') -Destination $share -Recurse

Write-Host "Built $(Join-Path $prefix 'bin\nterm.exe')"
