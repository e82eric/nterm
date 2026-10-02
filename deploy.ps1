param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string] $Destination,

    [switch] $SkipBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repository = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).Path
$source = Join-Path $repository 'zig-out\bin'
$resourceSource = Join-Path $repository 'zig-out\share'
$destinationPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Destination)

if (-not $SkipBuild) {
    Push-Location -LiteralPath $repository
    try {
        & zig build egui-sample -Doptimize=ReleaseFast
        if ($LASTEXITCODE -ne 0) {
            throw "egui sample build failed with exit code $LASTEXITCODE"
        }
    }
    finally {
        Pop-Location
    }
}

# Ghostty loads conpty.dll from the application directory. Its matching
# OpenConsole.exe must travel with it; otherwise Ghostty uses system ConPTY.
$files = @('ghostty.dll', 'ghostty_egui.exe', 'conpty.dll', 'OpenConsole.exe')
foreach ($file in $files) {
    $path = Join-Path $source $file
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Missing build output: $path"
    }
}

New-Item -ItemType Directory -Path $destinationPath -Force | Out-Null
$themeSource = Join-Path $resourceSource 'ghostty\themes'
$terminfoSource = Join-Path $resourceSource 'terminfo\ghostty.terminfo'
if (-not (Test-Path -LiteralPath $themeSource -PathType Container)) {
    throw "Missing bundled themes: $themeSource. Build with -Demit-themes=true."
}
if (-not (Test-Path -LiteralPath $terminfoSource -PathType Leaf)) {
    throw "Missing resource discovery sentinel: $terminfoSource. Build with -Demit-terminfo=true."
}
$compiledTerminfoPaths = @('terminfo\78\xterm-ghostty', 'terminfo\x\xterm-ghostty')
$hasCompiledTerminfo = $false
foreach ($relativePath in $compiledTerminfoPaths) {
    if (Test-Path -LiteralPath (Join-Path $resourceSource $relativePath) -PathType Leaf) {
        $hasCompiledTerminfo = $true
        break
    }
}
if (-not $hasCompiledTerminfo) {
    throw "Missing compiled xterm-ghostty terminfo database. Rebuild with Git for Windows installed or pass -Dterminfo-compiler=<path to tic.exe>."
}
foreach ($file in $files) {
    Copy-Item -LiteralPath (Join-Path $source $file) -Destination (Join-Path $destinationPath $file) -Force
}

# Keep the share layout beside the executable so Ghostty's resource discovery
# finds both its terminfo sentinel and the bundled themes.
$resourceDestination = Join-Path $destinationPath 'share'
New-Item -ItemType Directory -Path $resourceDestination -Force | Out-Null
foreach ($directory in @('ghostty', 'terminfo')) {
    Copy-Item -LiteralPath (Join-Path $resourceSource $directory) -Destination $resourceDestination -Recurse -Force
}

Write-Host "Deployed $($files -join ', ') to $destinationPath"
