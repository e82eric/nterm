# PowerShell 7.4+. Dot-source this file from your $PROFILE.
# nterm Lua: allow the ProfileFiles, ProfileDirectories, ProfileRipgrep,
# ProfileHistory and ProfileCompletions named pickers. See docs/picker-pipe.md or
# nterm.example.lua.
# Outside an nterm pane this file does nothing, so your other bindings stay.
param([string[]]$PickerRoots = @((Get-Location).Path))

if (-not ($env:NTERM_PICKER_ENDPOINT -and $env:NTERM_PICKER_TOKEN)) { return }
if (-not (Get-Command Invoke-NtermPicker -CommandType Function -ErrorAction SilentlyContinue)) {
    . (Join-Path $PSScriptRoot 'Invoke-NtermPicker.ps1') -BackgroundInitialize -FreshConnection
}
if (-not (Get-Command Invoke-NtermPicker -CommandType Function -ErrorAction SilentlyContinue)) { return }

$script:NtermProfilePickerRoots = $PickerRoots

function Invoke-ProfilePathPicker {
    param([string[]]$Roots = $script:NtermProfilePickerRoots, [switch]$Directories)
    $name = if ($Directories) { 'ProfileDirectories' } else { 'ProfileFiles' }
    Invoke-NtermPicker -Name $name -Parameters @{
        roots = (ConvertTo-Json -InputObject @($Roots) -Compress)
        cwd = $PWD.Path
    }
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
    # nterm reads the history file newest first, joining backtick continuations.
    $result = Invoke-NtermPicker -Name ProfileHistory -Parameters @{
        history = $historyPath
        query = $line
        cwd = $PWD.Path
    }
    if ($null -ne $result) {
        [Microsoft.PowerShell.PSConsoleReadLine]::RevertLine()
        [Microsoft.PowerShell.PSConsoleReadLine]::Insert([string]$result)
    }
}

Set-PSReadLineKeyHandler -Chord Ctrl+Shift+g -ScriptBlock {
    $line = ''; $cursor = 0
    [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
    if ([string]::IsNullOrWhiteSpace($line)) { return }
    # nterm runs rg from the ProfileRipgrep picker and streams its matches.
    $result = Invoke-NtermPicker -Name ProfileRipgrep -Parameters @{ pattern = $line; cwd = $PWD.Path }
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
    # The innermost command around the cursor, so the preview can show
    # parameter help for completions such as -Path.
    $tokens = $null; $parseErrors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseInput($line, [ref]$tokens, [ref]$parseErrors)
    $commandAst = $ast.FindAll({
        param($node)
        $node -is [System.Management.Automation.Language.CommandAst] -and
            $node.Extent.StartOffset -le $cursor -and $node.Extent.EndOffset -ge $cursor
    }, $true) | Sort-Object { $_.Extent.StartOffset } -Descending | Select-Object -First 1
    $commandName = if ($commandAst) { [string]$commandAst.GetCommandName() } else { '' }
    # Streams the matches into the ProfileCompletions Items picker, whose
    # configured preview shows Get-Help for the highlighted completion.
    $result = $completions.CompletionMatches |
        Invoke-NtermPicker -Name ProfileCompletions -Parameters @{ commandName = $commandName; cwd = $PWD.Path } `
            -DisplayProperty ListItemText -ValueProperty CompletionText
    if ($null -ne $result) {
        [Microsoft.PowerShell.PSConsoleReadLine]::Replace(
            $completions.ReplacementIndex, $completions.ReplacementLength, [string]$result
        )
    }
}
