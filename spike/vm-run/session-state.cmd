@echo off
rem What is running, and the end of the daemon's log.
tasklist /fi "imagename eq nterm.exe" /fo csv /nh
tasklist /fi "imagename eq nterm-daemon.exe" /fo csv /nh
tasklist /fi "imagename eq OpenConsole.exe" /fo csv /nh
powershell -NoProfile -Command "Get-Content $env:LOCALAPPDATA\nterm\daemon-default.log -Tail %1 -ErrorAction SilentlyContinue"
