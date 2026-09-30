param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string] $Destination,

    [switch] $SkipBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repository = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).Path
$source = Join-Path $repository 'zig-out\bin'
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
foreach ($file in $files) {
    Copy-Item -LiteralPath (Join-Path $source $file) -Destination (Join-Path $destinationPath $file) -Force
}

Write-Host "Deployed $($files -join ', ') to $destinationPath"
