@echo off
rem Spike: print the logs from the VM.
echo --- daemon
type "%LOCALAPPDATA%\nterm-spike\daemon.err"
echo --- ui
type "%TEMP%\nterm-spike.log"
