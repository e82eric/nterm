@echo off
rem Spike: start nterm attached to the stand-in daemon. The VM has no
rem Vulkan, so use DirectX 12, which falls back to software rendering.
set WGPU_BACKEND=dx12
set NTERM_SPIKE_ATTACH=1
start "" Z:\nterm-plexer2\out\bin\nterm.exe
