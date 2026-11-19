# PowerShell 7.4+. Dot-source this file from your $PROFILE, or copy it there
# and change the keys and pickers: what they do is in the NTerm module
# (powershell\NTerm), and only which key opens which picker is here.
# nterm Lua: allow the ProfileFiles, ProfileDirectories, ProfileRipgrep,
# ProfileHistory and ProfileCompletions named pickers. See docs/picker-pipe.md or
# nterm.example.lua. The bindings with Shift ask the session daemon for its
# Files, Directories and History pickers: see nterm-daemon.example.lua.
# Outside an nterm pane this file does nothing, so your other bindings stay.
param([string[]]$PickerRoots = @((Get-Location).Path))

if (-not $env:NTERM_DAEMON_PIPE) { return }
# The session daemon names the folder installed with it, on its own machine.
# A daemon from before it did, or a checkout not yet built: the module beside
# this file.
$ntermModule = if ($env:NTERM_SHARE) {
    Join-Path $env:NTERM_SHARE 'powershell\NTerm'
} else {
    Join-Path $PSScriptRoot 'powershell\NTerm'
}
Import-Module $ntermModule -ErrorAction SilentlyContinue
if (-not (Get-Module NTerm)) { return }

$script:NtermProfilePickerRoots = $PickerRoots

if (-not (Get-Module PSReadLine)) { Import-Module PSReadLine }

# Folders, and files: Ctrl under the shell's folder, Alt under the roots
# passed to this file.
Set-PSReadLineKeyHandler -Chord Ctrl+o -ScriptBlock { Set-NtermLocationFromPicker -Picker ProfileDirectories -Roots $PWD.Path }
Set-PSReadLineKeyHandler -Chord Alt+o -ScriptBlock { Set-NtermLocationFromPicker -Picker ProfileDirectories -Roots $script:NtermProfilePickerRoots }
Set-PSReadLineKeyHandler -Chord Ctrl+t -ScriptBlock { Add-NtermPickedPath -Picker ProfileFiles -Roots $PWD.Path }
Set-PSReadLineKeyHandler -Chord Alt+t -ScriptBlock { Add-NtermPickedPath -Picker ProfileFiles -Roots $script:NtermProfilePickerRoots }
# The same four with Shift, with the session daemon running the picker: it
# lists and searches on the machine this shell is on, so these work in a
# window on another machine. A capital letter is the key with Shift:
# PSReadLine reads Alt+Shift+o as Alt+O.
Set-PSReadLineKeyHandler -Chord Ctrl+Shift+o -ScriptBlock { Set-NtermLocationFromPicker -DaemonPicker Directories -Roots $PWD.Path }
Set-PSReadLineKeyHandler -Chord Alt+O -ScriptBlock { Set-NtermLocationFromPicker -DaemonPicker Directories -Roots $script:NtermProfilePickerRoots }
Set-PSReadLineKeyHandler -Chord Ctrl+Shift+t -ScriptBlock { Add-NtermPickedPath -DaemonPicker Files -Roots $PWD.Path }
Set-PSReadLineKeyHandler -Chord Alt+T -ScriptBlock { Add-NtermPickedPath -DaemonPicker Files -Roots $script:NtermProfilePickerRoots }

# What was typed before, newest first, searched for what is on the line.
Set-PSReadLineKeyHandler -Chord Alt+r -ScriptBlock { Set-NtermLineFromHistory -Picker ProfileHistory }
Set-PSReadLineKeyHandler -Chord Alt+R -ScriptBlock { Set-NtermLineFromHistory -DaemonPicker History }

# rg for what is on the line; the chosen match becomes `nvim +LINE 'PATH'`,
# for review. Give -Editor for another editor.
Set-PSReadLineKeyHandler -Chord Ctrl+Shift+g -ScriptBlock { Set-NtermLineFromRipgrep -Picker ProfileRipgrep }

# PowerShell's tab completions: in the window's ProfileCompletions, whose
# preview shows Get-Help, or with Shift in the session daemon's own items
# picker, which has no preview.
Set-PSReadLineKeyHandler -Chord 'Ctrl+@', 'Ctrl+Spacebar' -ScriptBlock { Complete-NtermLine -Picker ProfileCompletions }
Set-PSReadLineKeyHandler -Chord Ctrl+Shift+Spacebar -ScriptBlock { Complete-NtermLine }

# The prompt tells nterm the shell's folder (OSC 7), so pickers and new panes
# start there, and where each command starts and ends (OSC 133). It keeps the
# prompt there is when it runs: define your own before this line.
# Enable-NtermShellIntegration
