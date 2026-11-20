@echo off
rem Start nterm in the test VM.
set WGPU_BACKEND=dx12
set NTERM_DAEMON_TRACE=%NTERM_TRACE%
start "" Z:\nterm-plexer2\out\bin\nterm.exe
