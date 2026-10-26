param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string] $Destination,

    [switch] $SkipBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$source = Join-Path $PSScriptRoot 'out\bin'
$resourceSource = Join-Path $PSScriptRoot 'out\share'
$destinationPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Destination)

if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot 'build.ps1')
}

# Use the same pinned ConPTY package as zig build, including with -SkipBuild.
$conpty = & (Join-Path $PSScriptRoot 'ghostty\dist\windows\conpty.ps1')
$conptyVersion = $conpty.Version
$architecture = $conpty.Architecture
$conptyDll = $conpty.Dll
$conptyHost = $conpty.Host

# Ghostty loads conpty.dll and its matching OpenConsole.exe beside the app.
$files = @('ghostty.dll', 'nterm.exe', 'conpty.dll', 'OpenConsole.exe')
$fileSources = @{
    'ghostty.dll' = Join-Path $source 'ghostty.dll'
    'nterm.exe' = Join-Path $source 'nterm.exe'
    'conpty.dll' = $conptyDll
    'OpenConsole.exe' = $conptyHost
}
foreach ($file in $files) {
    $path = $fileSources[$file]
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
    Copy-Item -LiteralPath $fileSources[$file] -Destination (Join-Path $destinationPath $file) -Force
}

# Keep the share layout beside the executable so Ghostty's resource discovery
# finds both its terminfo sentinel and the bundled themes.
$resourceDestination = Join-Path $destinationPath 'share'
New-Item -ItemType Directory -Path $resourceDestination -Force | Out-Null
# nterm holds the Lua modules a config can require.
foreach ($directory in @('ghostty', 'terminfo', 'nterm')) {
    Copy-Item -LiteralPath (Join-Path $resourceSource $directory) -Destination $resourceDestination -Recurse -Force
}

Write-Host "Deployed $($files -join ', ') to $destinationPath"
Write-Host "ConPTY version: $conptyVersion ($architecture)"
