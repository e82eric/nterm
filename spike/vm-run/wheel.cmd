@echo off
rem wheel.cmd X Y NOTCHES: turn the wheel at a screen position; positive is up.
rem Named, because PowerShell reads a bare negative number as a parameter.
powershell -NoProfile -ExecutionPolicy Bypass -File Z:\nterm-plexer2\spike\vm-run\wheel.ps1 -X %1 -Y %2 -Notches %3
