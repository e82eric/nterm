# PowerShell 7.3+. Dot-source this file from your $PROFILE.
# nterm Lua: allow streaming and the ProfileFiles/ProfileDirectories named
# FileSystem pickers. See PICKER-PIPE.md or nterm.example.lua.
# Outside nterm, install fzf on PATH for the same bindings.
param([string[]]$PickerRoots = @((Get-Location).Path))

$script:NtermProfilePickerRoots = $PickerRoots
. (Join-Path $PSScriptRoot 'Invoke-NtermPicker.ps1') -BackgroundInitialize -FreshConnection

function Invoke-ProfilePicker {
    param(
        [object[]]$Items,
        [string]$Query = '',
        [string]$DisplayProperty,
        [string]$ValueProperty
    )
    if (-not $Items -or $Items.Count -eq 0) { return }
    if ($env:NTERM_PICKER_ENDPOINT -and $env:NTERM_PICKER_TOKEN) {
        $Items | Invoke-NtermPicker -DisplayProperty $DisplayProperty -ValueProperty $ValueProperty `
            -Options @{ initial_query = $Query }
        return
    }

    $fzf = Get-Command fzf -CommandType Application -ErrorAction SilentlyContinue
    if (-not $fzf) { throw 'Run inside nterm with the picker pipe enabled, or install fzf on PATH.' }
    # IDs preserve multiline values and completion quoting independently of display.
    $rows = for ($i = 0; $i -lt $Items.Count; $i++) {
        $text = if ($DisplayProperty) { [string]$Items[$i].$DisplayProperty } else { [string]$Items[$i] }
        "$i`t" + ($text -replace '[\r\n\t]+', ' ')
    }
    # Launch directly: PSReadLine key-handler pipelines interfere with fzf's UI.
    $start = [System.Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $fzf.Source
    $start.UseShellExecute = $false
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.StandardInputEncoding = [System.Text.UTF8Encoding]::new($false)
    $start.StandardOutputEncoding = [System.Text.UTF8Encoding]::new($false)
    if ($PWD.Provider.Name -eq 'FileSystem') { $start.WorkingDirectory = $PWD.ProviderPath }
    foreach ($argument in @('--no-preview', '--no-multi', '--no-sort', '--layout=reverse', '--delimiter=\t', '--with-nth=2..', "--query=$Query")) {
        $start.ArgumentList.Add($argument)
    }
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $start
    try {
        [void]$process.Start()
        $output = $process.StandardOutput.ReadToEndAsync()
        try {
            foreach ($row in $rows) { $process.StandardInput.WriteLine($row) }
        } catch [System.IO.IOException] {
            # fzf can close input early after selection or cancellation.
        } finally {
            try { $process.StandardInput.Close() } catch [System.IO.IOException] { }
        }
        $process.WaitForExit()
        $selected = $output.GetAwaiter().GetResult().TrimEnd([char[]]"`r`n")
        if ($process.ExitCode -ne 0 -or [string]::IsNullOrEmpty($selected)) { return }
    } finally { $process.Dispose() }
    $index = [int](([string]$selected -split "`t", 2)[0])
    if ($ValueProperty) { $Items[$index].$ValueProperty } else { $Items[$index] }
}

function Get-ProfilePickerPaths {
    param([string[]]$Roots, [switch]$Directories)
    foreach ($root in $Roots) {
        if ($Directories) {
            Get-ChildItem -LiteralPath $root -Directory -Recurse -ErrorAction SilentlyContinue
        } else {
            Get-ChildItem -LiteralPath $root -File -Recurse -ErrorAction SilentlyContinue
        }
    }
}

function Invoke-ProfilePathPicker {
    param([string[]]$Roots = $script:NtermProfilePickerRoots, [switch]$Directories)
    if ($env:NTERM_PICKER_ENDPOINT -and $env:NTERM_PICKER_TOKEN) {
        $name = if ($Directories) { 'ProfileDirectories' } else { 'ProfileFiles' }
        return Invoke-NtermPicker -Name $name -Parameters @{
            roots = (ConvertTo-Json -InputObject @($Roots) -Compress)
            cwd = $PWD.Path
        }
    }
    $items = @(Get-ProfilePickerPaths -Roots $Roots -Directories:$Directories)
    Invoke-ProfilePicker -Items $items -DisplayProperty FullName -ValueProperty FullName
}

function Insert-FilePickerResult {
    param([string[]]$Roots = $script:NtermProfilePickerRoots)
    $result = Invoke-ProfilePathPicker -Roots $Roots
    if ($null -ne $result) {
        # Quote paths containing spaces or apostrophes for PowerShell.
        $quoted = "'" + ([string]$result).Replace("'", "''") + "'"
        [Microsoft.PowerShell.PSConsoleReadLine]::Insert($quoted)
    }
}

function Set-LocationFromPicker {
    param([string[]]$Roots = $script:NtermProfilePickerRoots)
    $result = Invoke-ProfilePathPicker -Roots $Roots -Directories
    if ($null -ne $result) {
        $quoted = "'" + ([string]$result).Replace("'", "''") + "'"
        [Microsoft.PowerShell.PSConsoleReadLine]::RevertLine()
        [Microsoft.PowerShell.PSConsoleReadLine]::Insert("Set-Location -LiteralPath $quoted")
        [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
    }
}

function ConvertTo-HistoryPickerItem {
    begin { $lines = [System.Collections.Generic.List[string]]::new() }
    process {
        $text = [string]$_
        $continued = $text.EndsWith([string][char]96)
        if ($continued) { $text = $text.Substring(0, $text.Length - 1) }
        $lines.Add($text)
        if (-not $continued) {
            $command = $lines -join "`n"
            [pscustomobject]@{ Text = $command -replace '\r?\n\s*', '` '; Command = $command }
            $lines.Clear()
        }
    }
    end {
        if ($lines.Count) {
            $command = $lines -join "`n"
            [pscustomobject]@{ Text = $command -replace '\r?\n\s*', '` '; Command = $command }
        }
    }
}

if (-not (Get-Module PSReadLine)) { Import-Module PSReadLine }

Set-PSReadLineKeyHandler -Chord Ctrl+o -ScriptBlock { Set-LocationFromPicker -Roots @($PWD.Path) }
Set-PSReadLineKeyHandler -Chord Alt+o -ScriptBlock { Set-LocationFromPicker }
Set-PSReadLineKeyHandler -Chord Ctrl+t -ScriptBlock { Insert-FilePickerResult -Roots @($PWD.Path) }
Set-PSReadLineKeyHandler -Chord Alt+t -ScriptBlock { Insert-FilePickerResult }

Set-PSReadLineKeyHandler -Chord Alt+r -ScriptBlock {
    $line = ''; $cursor = 0
    [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
    $historyPath = (Get-PSReadLineOption).HistorySavePath
    if (-not (Test-Path -LiteralPath $historyPath)) { return }
    $items = @(Get-Content -LiteralPath $historyPath -Encoding utf8 | ConvertTo-HistoryPickerItem)
    [array]::Reverse($items)
    $result = Invoke-ProfilePicker -Items $items -DisplayProperty Text -ValueProperty Command -Query $line
    if ($null -ne $result) {
        [Microsoft.PowerShell.PSConsoleReadLine]::RevertLine()
        [Microsoft.PowerShell.PSConsoleReadLine]::Insert([string]$result)
    }
}

Set-PSReadLineKeyHandler -Chord Ctrl+Shift+g -ScriptBlock {
    $line = ''; $cursor = 0
    [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
    if ([string]::IsNullOrWhiteSpace($line)) { return }
    if (-not (Get-Command rg -CommandType Application -ErrorAction SilentlyContinue)) {
        throw 'Install ripgrep (rg) on PATH to use the text-search binding.'
    }
    $items = @(& rg --line-number --column --color never -- $line .)
    $result = Invoke-ProfilePicker -Items $items
    if ($null -eq $result) { return }
    $match = [regex]::Match([string]$result, '^(.+?):(\d+):\d+:')
    if (-not $match.Success) { return }
    $path = $match.Groups[1].Value.Replace("'", "''")
    $row = $match.Groups[2].Value
    # Insert for review; change nvim to your preferred editor.
    [Microsoft.PowerShell.PSConsoleReadLine]::RevertLine()
    [Microsoft.PowerShell.PSConsoleReadLine]::Insert("nvim +$row '$path'")
}

Set-PSReadLineKeyHandler -Chord 'Ctrl+@', 'Ctrl+Spacebar' -ScriptBlock {
    $line = ''; $cursor = 0
    [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
    $completions = [System.Management.Automation.CommandCompletion]::CompleteInput($line, $cursor, @{})
    if ($completions.CompletionMatches.Count -eq 0) { return }
    $items = @($completions.CompletionMatches | ForEach-Object {
        [pscustomobject]@{ Text = [string]$_.ListItemText; Value = [string]$_.CompletionText }
    })
    $result = Invoke-ProfilePicker -Items $items -DisplayProperty Text -ValueProperty Value
    if ($null -ne $result) {
        [Microsoft.PowerShell.PSConsoleReadLine]::Replace(
            $completions.ReplacementIndex, $completions.ReplacementLength, [string]$result
        )
    }
}
