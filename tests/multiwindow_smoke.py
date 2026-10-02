"""Windows GUI smoke test. Build egui-sample first; briefly takes input focus."""

import ctypes as c
from ctypes import wintypes as w
import os
from pathlib import Path
import subprocess
import tempfile
import time

u=c.WinDLL('user32', use_last_error=True)
u.GetForegroundWindow.restype=w.HWND
u.SetForegroundWindow.argtypes=[w.HWND]
u.PostMessageW.argtypes=[w.HWND,w.UINT,w.WPARAM,w.LPARAM]
u.GetWindowThreadProcessId.argtypes=[w.HWND,c.POINTER(w.DWORD)]
u.GetWindowTextW.argtypes=[w.HWND,w.LPWSTR,c.c_int]
u.IsWindowVisible.argtypes=[w.HWND]
u.ShowWindow.argtypes=[w.HWND,c.c_int]
u.SetWindowPos.argtypes=[w.HWND,w.HWND,c.c_int,c.c_int,c.c_int,c.c_int,w.UINT]
u.keybd_event.argtypes=[w.BYTE,w.BYTE,w.DWORD,c.c_size_t]
callback_type=c.WINFUNCTYPE(w.BOOL,w.HWND,w.LPARAM)
u.EnumWindows.argtypes=[callback_type,w.LPARAM]
previous=u.GetForegroundWindow()
env=dict(os.environ, GHOSTTY_LOG='stderr')
os.chdir(Path(__file__).resolve().parents[3])
log=tempfile.TemporaryFile()
startup=subprocess.STARTUPINFO()
startup.dwFlags=subprocess.STARTF_USESHOWWINDOW
startup.wShowWindow=0
process=subprocess.Popen(['zig-out/bin/ghostty_egui.exe','--command=cmd.exe'],env=env,stderr=log,startupinfo=startup)

def windows():
    result=[]
    @callback_type
    def visit(hwnd, _):
        pid=w.DWORD()
        u.GetWindowThreadProcessId(hwnd,c.byref(pid))
        title=c.create_unicode_buffer(256)
        u.GetWindowTextW(hwnd,title,256)
        if pid.value==process.pid and u.IsWindowVisible(hwnd) and 'Ghostty' in title.value:
            result.append(hwnd)
        return True
    u.EnumWindows(visit,0)
    return result

def wait_count(count):
    deadline=time.monotonic()+15
    while time.monotonic()<deadline:
        assert process.poll() is None, f'App exited: {process.returncode}'
        found=windows()
        if len(found)==count:
            time.sleep(.4)
            return found
        time.sleep(.1)
    raise AssertionError(f'Expected {count} windows, got {len(windows())}')

def shortcut(hwnd,key):
    u.ShowWindow(hwnd,5)
    assert u.SetForegroundWindow(hwnd), 'Could not focus test window'
    time.sleep(.25)
    for vk in (0x11,0x10,key): u.keybd_event(vk,0,0,0)
    time.sleep(.08)
    for vk in (key,0x10,0x11): u.keybd_event(vk,0,2,0)
    time.sleep(.5)

try:
    first=wait_count(1)[0]
    current=first
    for count in (2,3,4):
        prior=set(windows())
        shortcut(current,ord('N'))
        current=next(iter(set(wait_count(count))-prior))
        print(f'PASS: {count} windows in PID {process.pid}',flush=True)
    shortcut(current,ord('E'))
    shortcut(current,ord('W'))
    u.PostMessageW(first,0x10,0,0)
    remaining=wait_count(3)
    print('PASS: original window closed; secondary windows survive',flush=True)
    prior=set(remaining)
    shortcut(remaining[0],ord('N'))
    current=next(iter(set(wait_count(4))-prior))
    print('PASS: surviving window creates another window',flush=True)
    # Exercise resize and redraw after switching between distinct GL contexts.
    for index,hwnd in enumerate(windows()):
        u.SetWindowPos(hwnd,0,50+index*40,50+index*40,750+index*50,500+index*40,0x0004)
        time.sleep(.25)
    assert u.SetForegroundWindow(current)
    time.sleep(.25)
    for key in (ord('E'),ord('X'),ord('I'),ord('T'),0x0D):
        u.keybd_event(key,0,0,0)
        u.keybd_event(key,0,2,0)
        time.sleep(.05)
    wait_count(3)
    print('PASS: exiting a shell closes only its window',flush=True)
    for hwnd in windows(): u.PostMessageW(hwnd,0x10,0,0)
    assert process.wait(timeout=15)==0
    print('PASS: last window closes process cleanly',flush=True)
finally:
    for vk in (0x11,0x10): u.keybd_event(vk,0,2,0)
    if process.poll() is None:
        for hwnd in windows(): u.PostMessageW(hwnd,0x10,0,0)
        try: process.wait(timeout=5)
        except subprocess.TimeoutExpired: process.terminate()
    if previous: u.SetForegroundWindow(previous)
    log.close()
