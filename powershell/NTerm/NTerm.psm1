# nterm for PowerShell 7.4+: pickers for PSReadLine keys, and the marks a
# prompt writes for nterm (the folder the shell is in; where each command
# starts and ends). Which keys open which pickers is the profile's to say,
# as which pickers there are is nterm-daemon.lua's. See profile.example.ps1.
#
#   # The session daemon names the folder installed with it, on its machine.
#   Import-Module (Join-Path $env:NTERM_SHARE 'powershell\NTerm')
#   Enable-NtermShellIntegration
#   Set-PSReadLineKeyHandler -Chord Alt+T -ScriptBlock {
#       Add-NtermPickedPath -DaemonPicker Files -Roots C:\src
#   }
#
# Each picker function takes one of:
#   -DaemonPicker NAME  a picker of nterm-daemon.lua on the shell's machine.
#                       It lists and searches there, so it works the same in
#                       a window on another machine.
#   -Picker NAME        a named picker of the window's nterm.lua, allowed in
#                       its picker_pipe.

. (Join-Path $PSScriptRoot 'Invoke-NtermPicker.ps1')

# --- Pickers -----------------------------------------------------------------

function Invoke-NtermChosenPicker {
    param([string] $Picker, [string] $DaemonPicker, [hashtable] $Parameters)
    if ($DaemonPicker) {
        Invoke-NtermPicker -DaemonPicker $DaemonPicker -Parameters $Parameters
    } else {
        Invoke-NtermPicker -Name $Picker -Parameters $Parameters
    }
}

# A path as PowerShell reads it whatever it holds: in single quotes, with
# each quote in it doubled.
function ConvertTo-NtermQuoted([string] $Text) {
    "'" + $Text.Replace("'", "''") + "'"
}

# The shell's folder, and the folders to look under. Without roots a picker
# looks under its own, or else under the shell's folder.
function Get-NtermPathParameters([string[]] $Roots) {
    $parameters = @{ cwd = $PWD.Path }
    if ($Roots) { $parameters.roots = ConvertTo-Json -InputObject @($Roots) -Compress }
    $parameters
}

function Add-NtermPickedPath {
    <#
    .SYNOPSIS
    Puts the chosen path, quoted, at the cursor of the command line.
    #>
    [CmdletBinding(DefaultParameterSetName = 'Daemon')]
    param(
        [Parameter(Mandatory, ParameterSetName = 'Daemon')][string] $DaemonPicker,
        [Parameter(Mandatory, ParameterSetName = 'Window')][string] $Picker,
        [string[]] $Roots
    )
    $result = Invoke-NtermChosenPicker -Picker $Picker -DaemonPicker $DaemonPicker `
        -Parameters (Get-NtermPathParameters $Roots)
    if ($null -ne $result) {
        [Microsoft.PowerShell.PSConsoleReadLine]::Insert((ConvertTo-NtermQuoted ([string]$result)))
    }
}

function Set-NtermLocationFromPicker {
    <#
    .SYNOPSIS
    Goes to the chosen folder: runs Set-Location to it from the command line.
    #>
    [CmdletBinding(DefaultParameterSetName = 'Daemon')]
    param(
        [Parameter(Mandatory, ParameterSetName = 'Daemon')][string] $DaemonPicker,
        [Parameter(Mandatory, ParameterSetName = 'Window')][string] $Picker,
        [string[]] $Roots
    )
    $result = Invoke-NtermChosenPicker -Picker $Picker -DaemonPicker $DaemonPicker `
        -Parameters (Get-NtermPathParameters $Roots)
    if ($null -ne $result) {
        [Microsoft.PowerShell.PSConsoleReadLine]::RevertLine()
        [Microsoft.PowerShell.PSConsoleReadLine]::Insert("Set-Location -LiteralPath $(ConvertTo-NtermQuoted ([string]$result))")
        [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
    }
}

function Set-NtermLineFromHistory {
    <#
    .SYNOPSIS
    Replaces the command line with a command chosen from PSReadLine's history.
    .DESCRIPTION
    The picker is opened with the history file as `history`, and what is
    typed so far as `query`.
    #>
    [CmdletBinding(DefaultParameterSetName = 'Daemon')]
    param(
        [Parameter(Mandatory, ParameterSetName = 'Daemon')][string] $DaemonPicker,
        [Parameter(Mandatory, ParameterSetName = 'Window')][string] $Picker
    )
    $line = ''; $cursor = 0
    [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
    $historyPath = (Get-PSReadLineOption).HistorySavePath
    if (-not (Test-Path -LiteralPath $historyPath)) { return }
    $parameters = @{ history = $historyPath; query = $line; cwd = $PWD.Path }
    if ($DaemonPicker) { $parameters.position = 'cursor' }
    $result = Invoke-NtermChosenPicker -Picker $Picker -DaemonPicker $DaemonPicker -Parameters $parameters
    if ($null -ne $result) {
        [Microsoft.PowerShell.PSConsoleReadLine]::RevertLine()
        [Microsoft.PowerShell.PSConsoleReadLine]::Insert([string]$result)
    }
}

function Set-NtermLineFromRipgrep {
    <#
    .SYNOPSIS
    Searches for what is on the command line, and puts a command opening the
    chosen match at its line on the command line, to be run or changed.
    .DESCRIPTION
    The picker is opened with the line as `pattern`. Its rows are ripgrep's
    --vimgrep ones: PATH:LINE:COLUMN:TEXT.
    #>
    [CmdletBinding(DefaultParameterSetName = 'Daemon')]
    param(
        [Parameter(Mandatory, ParameterSetName = 'Daemon')][string] $DaemonPicker,
        [Parameter(Mandatory, ParameterSetName = 'Window')][string] $Picker,
        # Run as: EDITOR +LINE 'PATH'.
        [string] $Editor = 'nvim'
    )
    $line = ''; $cursor = 0
    [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
    if ([string]::IsNullOrWhiteSpace($line)) { return }
    $result = Invoke-NtermChosenPicker -Picker $Picker -DaemonPicker $DaemonPicker `
        -Parameters @{ pattern = $line; cwd = $PWD.Path }
    if ($null -eq $result) { return }
    $match = [regex]::Match([string]$result, '^(.+?):(\d+):\d+:')
    if (-not $match.Success) { return }
    [Microsoft.PowerShell.PSConsoleReadLine]::RevertLine()
    [Microsoft.PowerShell.PSConsoleReadLine]::Insert(
        "$Editor +$($match.Groups[2].Value) $(ConvertTo-NtermQuoted $match.Groups[1].Value)")
}

function Complete-NtermLine {
    <#
    .SYNOPSIS
    Picks from PowerShell's tab completions for the command line at the cursor.
    .DESCRIPTION
    The completions are piped into the picker. By default that is the session
    daemon's own `items`, which has no preview. Either kind of picker is
    opened with `commandName`, the command around the cursor, and `cwd`, so
    its preview can show that command's help for a completion such as -Path.
    A daemon picker is also given PowerShell's tooltip of each completion,
    which nterm.completions shows as its preview.
    #>
    [CmdletBinding(DefaultParameterSetName = 'Daemon')]
    param(
        [Parameter(ParameterSetName = 'Daemon')][string] $DaemonPicker = 'items',
        [Parameter(Mandatory, ParameterSetName = 'Window')][string] $Picker
    )
    $line = ''; $cursor = 0
    [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
    $completions = [System.Management.Automation.CommandCompletion]::CompleteInput($line, $cursor, @{})
    if ($completions.CompletionMatches.Count -eq 0) { return }
    $tokens = $null; $parseErrors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseInput($line, [ref]$tokens, [ref]$parseErrors)
    $commandAst = $ast.FindAll({
        param($node)
        $node -is [System.Management.Automation.Language.CommandAst] -and
            $node.Extent.StartOffset -le $cursor -and $node.Extent.EndOffset -ge $cursor
    }, $true) | Sort-Object { $_.Extent.StartOffset } -Descending | Select-Object -First 1
    $commandName = if ($commandAst) { [string]$commandAst.GetCommandName() } else { '' }
    $parameters = @{ commandName = $commandName }
    # A daemon's picker is refused a cwd that is not a folder, as in HKLM:\.
    if ($PWD.Provider.Name -eq 'FileSystem') { $parameters.cwd = $PWD.ProviderPath }
    $picked = @{ DisplayProperty = 'ListItemText'; ValueProperty = 'CompletionText' }
    $result = if ($PSCmdlet.ParameterSetName -eq 'Daemon') {
        $parameters.label = 'Completions'
        $parameters.position = 'cursor'
        $completions.CompletionMatches | Invoke-NtermPicker -DaemonPicker $DaemonPicker `
            -Parameters $parameters -DescriptionProperty ToolTip @picked
    } else {
        $completions.CompletionMatches | Invoke-NtermPicker -Name $Picker -Parameters $parameters @picked
    }
    if ($null -ne $result) {
        [Microsoft.PowerShell.PSConsoleReadLine]::Replace(
            $completions.ReplacementIndex, $completions.ReplacementLength, [string]$result)
    }
}

# --- What the prompt tells nterm ---------------------------------------------

# The folder the shell is in (OSC 7): pickers start there, and so does a new
# pane beside this one.
function Send-NtermCwd {
    param([System.Management.Automation.PathInfo] $Location = $PWD)
    if ($Location.Provider.Name -eq 'FileSystem') {
        $path = $Location.ProviderPath -replace '\\', '/'
        [Console]::Write("`e]7;kitty-shell-cwd://localhost/$path`a")
    }
}

function Enable-NtermShellIntegration {
    <#
    .SYNOPSIS
    Makes the prompt tell nterm the shell's folder and where each command
    starts and ends, and how it ended.
    .DESCRIPTION
    The prompt there is when this runs is kept and shown inside the marks:
    define your own prompt function before calling this. Calling it again,
    as a profile run again does, wraps the prompt there is then, or keeps
    the one already wrapped. Also binds Enter, to mark where output starts.
    #>
    [CmdletBinding()]
    param()
    # Also as the location changes, so `Set-Location foo; claude` reports foo
    # before claude starts and not only at the next prompt.
    $ExecutionContext.SessionState.InvokeCommand.LocationChangedAction = {
        param($sender, $e)
        Send-NtermCwd -Location $e.NewPath
    }

    # The wrapper is this module's, which is how it is known again.
    $current = Get-Item -Path function:prompt -ErrorAction SilentlyContinue
    if (-not $current -or $current.ScriptBlock.Module.Name -ne 'NTerm') {
        $global:__ntermOriginalPrompt = if ($current) { $current.ScriptBlock } else {
            { "PS $($executionContext.SessionState.Path.CurrentLocation)$('>' * ($nestedPromptLevel + 1)) " }
        }
    }
    $script:LastHistoryId = -1
    Set-Item -Path function:global:prompt -Value {
        # Read the status before anything else overwrites $?.
        $succeeded = $?
        # OSC 133 semantic prompt marks: D = previous command finished,
        # A = prompt start, B = input start (C is written by the Enter handler).
        $last = Get-History -Count 1
        if ($script:LastHistoryId -ne -1) {
            if ($last.Id -eq $script:LastHistoryId) {
                # Blank line or Ctrl+C: no command ran.
                [Console]::Write("`e]133;D`a")
            } else {
                $code = if ($succeeded) { 0 } elseif ($global:LASTEXITCODE) { $global:LASTEXITCODE } else { 1 }
                [Console]::Write("`e]133;D;$code`a")
            }
        }
        $script:LastHistoryId = if ($last) { $last.Id } else { 0 }
        Send-NtermCwd
        # Programs such as nvim can exit without restoring the cursor shape.
        # DECSCUSR 5 (blinking bar) is a no-op when the cursor already is one.
        [Console]::Write("`e[5 q")
        # redraw=0: PSReadLine doesn't redraw the prompt on resize, so the
        # terminal must not clear it.
        [Console]::Write("`e]133;A;redraw=0`a")
        "$(& $global:__ntermOriginalPrompt)`e]133;B`a"
    }

    # Mark where command output begins when Enter accepts the line.
    if (-not (Get-Module PSReadLine)) { Import-Module PSReadLine -ErrorAction SilentlyContinue }
    if (Get-Module PSReadLine) {
        Set-PSReadLineKeyHandler -Key Enter -ScriptBlock {
            [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
            [Console]::Write("`e]133;C`a")
        }
    }
}
