@echo off
rem Spike: start the stand-in daemon inside the test VM. The share is
rem read-only there, so logs go to a local folder.
set L=%LOCALAPPDATA%\nterm-spike
mkdir "%L%" 2>nul
del "%L%\trace.txt" "%L%\daemon.err" "%TEMP%\nterm-spike.log" 2>nul
set NTERM_SPIKE_CONPTY=Z:\nterm-plexer2\out\bin\conpty.dll
set NTERM_SPIKE_TRACE=%L%\trace.txt
Z:\nterm-plexer2\spike\daemon-spike\target\release\daemon-spike.exe serve "C:\Users\ClaudeCode\AppData\Local\Programs\PowerShell\7\pwsh.exe" -NoLogo -NoProfile -NoExit -File Z:\nterm-plexer2\spike\vm-run\gen.ps1 2> "%L%\daemon.err"
