@echo off
rem Stop nterm and its daemon, and install the test config if there is none.
taskkill /f /im nterm.exe >nul 2>&1
taskkill /f /im nterm-daemon.exe >nul 2>&1
if not exist "%USERPROFILE%\nterm.lua" copy Z:\nterm-plexer2\spike\vm-run\nterm-test.lua "%USERPROFILE%\nterm.lua"
dir /b "%USERPROFILE%\nterm.lua"
