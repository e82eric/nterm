# PowerShell 7.4+. Dot-source this file from your $PROFILE.
# nterm Lua: allow the ProfileFiles, ProfileDirectories, ProfileRipgrep,
# ProfileHistory and ProfileCompletions named pickers. See docs/picker-pipe.md or
# nterm.example.lua.
# Outside an nterm pane this file does nothing, so your other bindings stay.
param([string[]]$PickerRoots = @((Get-Location).Path))

if (-not $env:NTERM_DAEMON_PIPE) { return }
if (-not (Get-Command Invoke-NtermPicker -CommandType Function -ErrorAction SilentlyContinue)) {
    . (Join-Path $PSScriptRoot 'Invoke-NtermPicker.ps1')
}
if (-not (Get-Command Invoke-NtermPicker -CommandType Function -ErrorAction SilentlyContinue)) { return }

$script:NtermProfilePickerRoots = $PickerRoots

function Invoke-ProfilePathPicker {
    # With -Daemon the session daemon lists and searches, on the machine this
    # shell is on, and the window only draws: the one that works when the
    # window is on another machine.
    param([string[]]$Roots = $script:NtermProfilePickerRoots, [switch]$Directories, [switch]$Daemon)
    $parameters = @{
        roots = (ConvertTo-Json -InputObject @($Roots) -Compress)
        cwd = $PWD.Path
    }
    if ($Daemon) {
        $name = if ($Directories) { 'directories' } else { 'files' }
        return Invoke-NtermPicker -DaemonPicker $name -Parameters $parameters
    }
    $name = if ($Directories) { 'ProfileDirectories' } else { 'ProfileFiles' }
    Invoke-NtermPicker -Name $name -Parameters $parameters
}

function Insert-FilePickerResult {
    param([string[]]$Roots = $script:NtermProfilePickerRoots, [switch]$Daemon)
    $result = Invoke-ProfilePathPicker -Roots $Roots -Daemon:$Daemon
    if ($null -ne $result) {
        # Quote paths containing spaces or apostrophes for PowerShell.
        $quoted = "'" + ([string]$result).Replace("'", "''") + "'"
        [Microsoft.PowerShell.PSConsoleReadLine]::Insert($quoted)
    }
}

function Set-LocationFromPicker {
    param([string[]]$Roots = $script:NtermProfilePickerRoots, [switch]$Daemon)
    $result = Invoke-ProfilePathPicker -Roots $Roots -Directories -Daemon:$Daemon
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
# The same four with Shift, with the session daemon running the picker. A
# capital letter is the key with Shift: PSReadLine reads Alt+Shift+o as Alt+o.
Set-PSReadLineKeyHandler -Chord Ctrl+Shift+o -ScriptBlock { Set-LocationFromPicker -Roots @($PWD.Path) -Daemon }
Set-PSReadLineKeyHandler -Chord Alt+O -ScriptBlock { Set-LocationFromPicker -Daemon }
Set-PSReadLineKeyHandler -Chord Ctrl+Shift+t -ScriptBlock { Insert-FilePickerResult -Roots @($PWD.Path) -Daemon }
Set-PSReadLineKeyHandler -Chord Alt+T -ScriptBlock { Insert-FilePickerResult -Daemon }

function Invoke-ProfileHistoryPicker {
    param([switch]$Daemon)
    $line = ''; $cursor = 0
    [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
    $historyPath = (Get-PSReadLineOption).HistorySavePath
    if (-not (Test-Path -LiteralPath $historyPath)) { return }
    # The history file is read newest first, joining backtick continuations:
    # by the session daemon with -Daemon, on the machine this shell is on,
    # and by nterm without.
    $result = if ($Daemon) {
        Invoke-NtermPicker -DaemonPicker history -Parameters @{
            path = $historyPath
            query = $line
            position = 'cursor'
        }
    } else {
        Invoke-NtermPicker -Name ProfileHistory -Parameters @{
            history = $historyPath
            query = $line
            cwd = $PWD.Path
        }
    }
    if ($null -ne $result) {
        [Microsoft.PowerShell.PSConsoleReadLine]::RevertLine()
        [Microsoft.PowerShell.PSConsoleReadLine]::Insert([string]$result)
    }
}
Set-PSReadLineKeyHandler -Chord Alt+r -ScriptBlock { Invoke-ProfileHistoryPicker }
Set-PSReadLineKeyHandler -Chord Alt+R -ScriptBlock { Invoke-ProfileHistoryPicker -Daemon }

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

function Invoke-ProfileCompletionPicker {
    param([switch]$Daemon)
    $line = ''; $cursor = 0
    [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
    $completions = [System.Management.Automation.CommandCompletion]::CompleteInput($line, $cursor, @{})
    if ($completions.CompletionMatches.Count -eq 0) { return }
    $result = if ($Daemon) {
        # The matches are given to the session daemon's items picker, which
        # searches them. It has no preview.
        $completions.CompletionMatches |
            Invoke-NtermPicker -DaemonPicker items -Parameters @{ label = 'Completions'; position = 'cursor' } `
                -DisplayProperty ListItemText -ValueProperty CompletionText
    } else {
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
        $completions.CompletionMatches |
            Invoke-NtermPicker -Name ProfileCompletions -Parameters @{ commandName = $commandName; cwd = $PWD.Path } `
                -DisplayProperty ListItemText -ValueProperty CompletionText
    }
    if ($null -ne $result) {
        [Microsoft.PowerShell.PSConsoleReadLine]::Replace(
            $completions.ReplacementIndex, $completions.ReplacementLength, [string]$result
        )
    }
}
Set-PSReadLineKeyHandler -Chord 'Ctrl+@', 'Ctrl+Spacebar' -ScriptBlock { Invoke-ProfileCompletionPicker }
Set-PSReadLineKeyHandler -Chord Ctrl+Shift+Spacebar -ScriptBlock { Invoke-ProfileCompletionPicker -Daemon }

# # Report the working directory to the terminal (OSC 7) so pickers run in it.
# # The guard keeps a re-sourced profile from wrapping the wrapper.
# if (-not $global:__ntermOriginalPrompt) {
#     $global:__ntermOriginalPrompt = $function:prompt
# }
# $global:__ntermLastHistoryId = -1
# function prompt {
#     # Read the status before anything else overwrites $?.
#     $succeeded = $?
#     # OSC 133 semantic prompt marks: D = previous command finished,
#     # A = prompt start, B = input start (C is written by the Enter handler).
#     $last = Get-History -Count 1
#     if ($global:__ntermLastHistoryId -ne -1) {
#         if ($last.Id -eq $global:__ntermLastHistoryId) {
#             # Blank line or Ctrl+C: no command ran.
#             [Console]::Write("`e]133;D`a")
#         } else {
#             $code = if ($succeeded) { 0 } elseif ($LASTEXITCODE) { $LASTEXITCODE } else { 1 }
#             [Console]::Write("`e]133;D;$code`a")
#         }
#     }
#     $global:__ntermLastHistoryId = if ($last) { $last.Id } else { 0 }
#     if ($PWD.Provider.Name -eq 'FileSystem') {
#         $path = $PWD.ProviderPath -replace '\\', '/'
#         [Console]::Write("`e]7;kitty-shell-cwd://localhost/$path`a")
#     }
#     # Programs such as nvim can exit without restoring the cursor shape.
#     # DECSCUSR 5 (blinking bar) is a no-op when the cursor already is one.
#     [Console]::Write("`e[5 q")
#     # redraw=0: PSReadLine doesn't redraw the prompt on resize, so the
#     # terminal must not clear it.
#     [Console]::Write("`e]133;A;redraw=0`a")
#     "$(& $global:__ntermOriginalPrompt)`e]133;B`a"
# }
#
# # Mark where command output begins when Enter accepts the line.
# Set-PSReadLineKeyHandler -Key Enter -ScriptBlock {
#     [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
#     [Console]::Write("`e]133;C`a")
# }
