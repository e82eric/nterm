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

# Pin both ConPTY components to the preview with resize cursor synchronization.
# Cache the package independently of build outputs, including with -SkipBuild.
$conptyVersion = '1.25.260710002-preview'
$conptyFileVersion = '1.25.2607.10002'
$architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant()
if ($architecture -notin @('x64', 'x86', 'arm64')) {
    throw "Unsupported ConPTY architecture: $architecture"
}
$conptyCache = Join-Path $repository ".zig-cache\conpty\$conptyVersion"
$conptyPackage = Join-Path $conptyCache 'package'
$conptyDll = Join-Path $conptyPackage "runtimes\win-$architecture\native\conpty.dll"
$conptyHost = Join-Path $conptyPackage "build\native\runtimes\$architecture\OpenConsole.exe"
if (-not ((Test-Path -LiteralPath $conptyDll -PathType Leaf) -and
          (Test-Path -LiteralPath $conptyHost -PathType Leaf))) {
    New-Item -ItemType Directory -Path $conptyCache -Force | Out-Null
    $archive = Join-Path $conptyCache 'package.zip'
    $packageUrl = "https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/$conptyVersion/microsoft.windows.console.conpty.$conptyVersion.nupkg"
    Invoke-WebRequest -Uri $packageUrl -OutFile $archive
    Expand-Archive -LiteralPath $archive -DestinationPath $conptyPackage -Force
}
foreach ($path in @($conptyDll, $conptyHost)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Missing ConPTY package component: $path"
    }
    if ((Get-Item -LiteralPath $path).VersionInfo.FileVersion -ne $conptyFileVersion) {
        throw "Unexpected ConPTY component version: $path (expected $conptyFileVersion)"
    }
}

# Ghostty loads conpty.dll and its matching OpenConsole.exe beside the app.
$files = @('ghostty.dll', 'ghostty_egui.exe', 'conpty.dll', 'OpenConsole.exe')
$fileSources = @{
    'ghostty.dll' = Join-Path $source 'ghostty.dll'
    'ghostty_egui.exe' = Join-Path $source 'ghostty_egui.exe'
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
foreach ($directory in @('ghostty', 'terminfo')) {
    Copy-Item -LiteralPath (Join-Path $resourceSource $directory) -Destination $resourceDestination -Recurse -Force
}

Write-Host "Deployed $($files -join ', ') to $destinationPath"
Write-Host "ConPTY version: $conptyVersion ($architecture)"
