param(
    [string] $Destination,
    [ValidateSet('x64', 'x86', 'arm64')]
    [string] $Architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant()
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repository = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).Path
# Pin both ConPTY components to the preview with resize cursor synchronization.
# Cache the package independently of build outputs, including with -SkipBuild.
$conptyVersion = '1.25.260710002-preview'
$conptyFileVersion = '1.25.2607.10002'
$packageSha256 = '86EDB1054C6F56D7377117FF75489197A5F095FE9835DF3AD04C2BA9D826B114'
# Microsoft Corporation author certificate for this pinned package.
$authorCertificateSha256 = '566A31882BE208BE4422F7CFD66ED09F5D4524A5994F50CCC8B05EC0528C1353'
$componentSha256 = @{
    x64 = @{
        'conpty.dll' = 'E2FE87E2258C4E46FFC5157F727218CC25F34A174902F72EB8A5B49EDD9A6458'
        'OpenConsole.exe' = '2525C351AA136D555E5DF9A3C9D6CE9BE43F785E37E3C993B8F23B3F0A53C7FA'
    }
    x86 = @{
        'conpty.dll' = '399DF767224480502CEFABA207DF422A01EF18E404300F9E922D3BA01429A77F'
        'OpenConsole.exe' = '1A76D1EF14F34CEBEB2AB33088FF48B5456A839F369CFBCCB6352497769956DE'
    }
    arm64 = @{
        'conpty.dll' = '36A5A3977E83B888F353CE96BAE2B5283708630FC43D0F518EAAF2235DA8902C'
        'OpenConsole.exe' = '197A765E0A0B67A03B142CE9B93B1F428D92F7EEE310B110BDF08383ED8D0D73'
    }
}

function Assert-Sha256([string] $Path, [string] $Expected) {
    # Use .NET directly so Windows PowerShell works even when launched from
    # PowerShell 7 with an inherited PSModulePath that omits Windows modules.
    $stream = [System.IO.File]::OpenRead($Path)
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        $actual = [System.BitConverter]::ToString($sha256.ComputeHash($stream)).Replace('-', '')
    } finally {
        $sha256.Dispose()
        $stream.Dispose()
    }
    if ($actual -ne $Expected) {
        throw "ConPTY SHA-256 mismatch: $Path (expected $Expected, got $actual). No components will be installed."
    }
}
$architecture = $Architecture
if ($architecture -notin @('x64', 'x86', 'arm64')) {
    throw "Unsupported ConPTY architecture: $architecture"
}
$conptyCache = Join-Path $repository ".zig-cache\conpty\$conptyVersion"
$conptyPackage = Join-Path $conptyCache 'package'
$conptyDll = Join-Path $conptyPackage "runtimes\win-$architecture\native\conpty.dll"
$conptyHost = Join-Path $conptyPackage "build\native\runtimes\$architecture\OpenConsole.exe"
$archive = Join-Path $conptyCache 'package.zip'
if (-not (Test-Path -LiteralPath $archive -PathType Leaf)) {
    New-Item -ItemType Directory -Path $conptyCache -Force | Out-Null
    $packageUrl = "https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/$conptyVersion/microsoft.windows.console.conpty.$conptyVersion.nupkg"
    Invoke-WebRequest -Uri $packageUrl -OutFile $archive
}
# Check downloads and cached archives before extracting anything.
Assert-Sha256 $archive $packageSha256
# Verify signatures on cached packages too, before extraction or installation.
# Capture native output so callers requesting component paths receive only the
# result object, not dotnet's verification messages.
$dotnet = Get-Command dotnet -CommandType Application -ErrorAction SilentlyContinue
if (-not $dotnet) {
    throw 'ConPTY signature verification requires the .NET SDK (dotnet nuget verify).'
}
$verification = & $dotnet.Source nuget verify $archive --all --certificate-fingerprint $authorCertificateSha256 2>&1
if ($LASTEXITCODE -ne 0) {
    throw "ConPTY package signature verification failed. No components will be installed.`n$($verification -join "`n")"
}
if (-not ((Test-Path -LiteralPath $conptyDll -PathType Leaf) -and
          (Test-Path -LiteralPath $conptyHost -PathType Leaf))) {
    Expand-Archive -LiteralPath $archive -DestinationPath $conptyPackage -Force
}
foreach ($path in @($conptyDll, $conptyHost)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Missing ConPTY package component: $path"
    }
    # Cached extracted files can change independently of the verified archive.
    Assert-Sha256 $path $componentSha256[$architecture][(Split-Path -Leaf $path)]
    if ((Get-Item -LiteralPath $path).VersionInfo.FileVersion -ne $conptyFileVersion) {
        throw "Unexpected ConPTY component version: $path (expected $conptyFileVersion)"
    }
}

if ($Destination) {
    New-Item -ItemType Directory -Path $Destination -Force | Out-Null
    foreach ($path in @($conptyDll, $conptyHost)) {
        Copy-Item -LiteralPath $path -Destination (Join-Path $Destination (Split-Path -Leaf $path)) -Force
    }
    Write-Host "Installed ConPTY $conptyVersion ($architecture) to $Destination"
} else {
    [pscustomobject]@{
        Version = $conptyVersion
        Architecture = $architecture
        Dll = $conptyDll
        Host = $conptyHost
    }
}
