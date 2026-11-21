# PowerShell 7.4+. Dot-source this file from your $PROFILE, or copy it there
# and change the keys and pickers: what they do is in the NTerm module
# (powershell\NTerm), and only which key opens which picker is here.

# nterm's PowerShell module: the pickers for the keys below, and the prompt
# marks. The session daemon names the folder installed with it, on its own
# machine; outside nterm, or with a daemon from before it did, the module
# beside this file, as in a checkout.
$ntermModule = if ($env:NTERM_SHARE) {
    Join-Path $env:NTERM_SHARE 'powershell\NTerm'
} else {
    Join-Path $PSScriptRoot 'powershell\NTerm'
}
Import-Module $ntermModule -ErrorAction SilentlyContinue

# The pickers, in an nterm pane: each is one of ~/nterm-daemon.lua on the
# machine this shell is on (see nterm-daemon.example.lua), so they work the
# same in a pane on another machine, and the window's nterm.lua needs no
# named pickers for them. Outside a pane no key is bound, so your other
# bindings stay. Ctrl+Shift+G, for rg, is nterm.example.lua's own key, which
# nterm keeps: it never reaches the shell.
if ($env:NTERM_DAEMON_PIPE -and (Get-Module NTerm)) {
    # A file under the shell's folder, its path put at the cursor.
    Set-PSReadLineKeyHandler -Chord Ctrl+t -ScriptBlock { Add-NtermPickedPath -DaemonPicker Files -Roots $PWD.Path }
    # A folder under the shell's folder, gone to.
    Set-PSReadLineKeyHandler -Chord Ctrl+o -ScriptBlock { Set-NtermLocationFromPicker -DaemonPicker Directories }
    # What was typed before, newest first, searched for what is on the line.
    Set-PSReadLineKeyHandler -Chord Alt+r -ScriptBlock { Set-NtermLineFromHistory -DaemonPicker History }
    # PowerShell's tab completions, with Get-Help as the preview.
    Set-PSReadLineKeyHandler -Chord 'Ctrl+@', 'Ctrl+Spacebar' -ScriptBlock { Complete-NtermLine -DaemonPicker CompletionsHelp }
}

# The prompt tells the terminal the shell's folder (OSC 7), so pickers and new
# panes start there, and where each command starts and ends (OSC 133), and
# binds Enter to mark where output starts. It wraps the prompt there is now:
# define your own before this line.
if (Get-Module NTerm) { Enable-NtermShellIntegration }
