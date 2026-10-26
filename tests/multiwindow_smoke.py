"""Windows GUI smoke test. Run build.ps1 first; briefly takes input focus."""
import ctypes as c
from ctypes import wintypes as w
import os
from pathlib import Path
import subprocess
import tempfile
import time
import argparse
import json
import statistics
options = argparse.ArgumentParser(description=__doc__)
options.add_argument('--memory-report', type=Path)
options.add_argument('--shell', default='cmd.exe')
options.add_argument('--executable', default='out/bin/ghostty_egui.exe')
options.add_argument('--terminal-arg', action='append', default=[])
options = options.parse_args()
memory_samples = {}

def process_counts():
    kernel = c.WinDLL('kernel32', use_last_error=True)
    kernel.CreateToolhelp32Snapshot.argtypes = [w.DWORD, w.DWORD]
    kernel.CreateToolhelp32Snapshot.restype = w.HANDLE
    kernel.CloseHandle.argtypes = [w.HANDLE]

    class ProcessEntry(c.Structure):
        _fields_ = [('dwSize', w.DWORD), ('cntUsage', w.DWORD), ('pid', w.DWORD), ('heap', c.c_size_t), ('module', w.DWORD), ('threads', w.DWORD), ('parent', w.DWORD), ('priority', w.LONG), ('flags', w.DWORD), ('exe', w.WCHAR * 260)]
    kernel.Process32FirstW.argtypes = [w.HANDLE, c.POINTER(ProcessEntry)]
    kernel.Process32NextW.argtypes = [w.HANDLE, c.POINTER(ProcessEntry)]
    snapshot = kernel.CreateToolhelp32Snapshot(2, 0)
    if snapshot == c.c_void_p(-1).value:
        raise c.WinError(c.get_last_error())
    try:
        entry = ProcessEntry(dwSize=c.sizeof(ProcessEntry))
        found = []
        valid = kernel.Process32FirstW(snapshot, c.byref(entry))
        while valid:
            found.append((entry.pid, entry.parent, entry.exe, entry.threads))
            valid = kernel.Process32NextW(snapshot, c.byref(entry))
        descendants = {process.pid}
        while True:
            expanded = descendants | {pid for pid, parent, _, _ in found if parent in descendants}
            if expanded == descendants:
                break
            descendants = expanded
        shell = Path(options.shell).name.lower()
        shells = sum((pid in descendants and name.lower() == shell for pid, _, name, _ in found))
        threads = next((count for pid, _, _, count in found if pid == process.pid))
        return (shells, threads)
    finally:
        kernel.CloseHandle(snapshot)

def sample_memory(label, expected_shells):
    if options.memory_report is None:
        return
    kernel = c.WinDLL('kernel32', use_last_error=True)
    psapi = c.WinDLL('psapi', use_last_error=True)
    kernel.OpenProcess.argtypes = [w.DWORD, w.BOOL, w.DWORD]
    kernel.OpenProcess.restype = w.HANDLE
    kernel.CloseHandle.argtypes = [w.HANDLE]
    psapi.QueryWorkingSet.argtypes = [w.HANDLE, c.c_void_p, w.DWORD]
    psapi.GetProcessMemoryInfo.argtypes = [w.HANDLE, c.c_void_p, w.DWORD]

    class MemoryInfo(c.Structure):
        _fields_ = [('cb', w.DWORD), ('PageFaultCount', w.DWORD)] + [(name, c.c_size_t) for name in ('PeakWorkingSetSize', 'WorkingSetSize', 'QuotaPeakPagedPoolUsage', 'QuotaPagedPoolUsage', 'QuotaPeakNonPagedPoolUsage', 'QuotaNonPagedPoolUsage', 'PagefileUsage', 'PeakPagefileUsage', 'PrivateUsage')]
    handle = kernel.OpenProcess(1024 | 16, False, process.pid)
    if not handle:
        raise c.WinError(c.get_last_error())
    try:
        time.sleep(2)
        deadline = time.monotonic() + 10
        while True:
            assert process.poll() is None, f'App exited: {process.returncode}'
            shells, threads = process_counts()
            if shells == expected_shells or time.monotonic() >= deadline:
                break
            time.sleep(0.2)
        assert shells == expected_shells, f'{label}: expected {expected_shells} shells, got {shells}'
        samples = []
        for _ in range(5):
            info = MemoryInfo(cb=c.sizeof(MemoryInfo))
            if not psapi.GetProcessMemoryInfo(handle, c.byref(info), info.cb):
                raise c.WinError(c.get_last_error())
            entries = (c.c_size_t * (1024 * 1024))()
            if not psapi.QueryWorkingSet(handle, entries, c.sizeof(entries)):
                raise c.WinError(c.get_last_error())
            # PSAPI_WORKING_SET_BLOCK Shared flag is bit 8; entries are 4 KiB pages.
            private_ws = sum((not entries[i] & 1 << 8 for i in range(1, entries[0] + 1))) * 4096
            samples.append({'private_working_set': private_ws, 'working_set': info.WorkingSetSize, 'private_bytes': info.PrivateUsage})
            time.sleep(0.1)
        memory_samples[label] = {name: statistics.median((sample[name] for sample in samples)) for name in samples[0]}
        memory_samples[label].update(shells=shells, threads=threads)
        sizes = []
        for hwnd in windows():
            rect = w.RECT()
            u.GetClientRect(hwnd, c.byref(rect))
            sizes.append([rect.right, rect.bottom])
        memory_samples[label]['window_sizes'] = sorted(sizes)
        print('MEMORY:', label, memory_samples[label], flush=True)
    finally:
        kernel.CloseHandle(handle)
u = c.WinDLL('user32', use_last_error=True)
u.SetProcessDPIAware()
u.GetForegroundWindow.restype = w.HWND
u.SetForegroundWindow.argtypes = [w.HWND]
u.BringWindowToTop.argtypes = [w.HWND]
u.SetFocus.argtypes = [w.HWND]
u.PostMessageW.argtypes = [w.HWND, w.UINT, w.WPARAM, w.LPARAM]
u.GetWindowThreadProcessId.argtypes = [w.HWND, c.POINTER(w.DWORD)]
u.GetWindowTextW.argtypes = [w.HWND, w.LPWSTR, c.c_int]
u.IsWindowVisible.argtypes = [w.HWND]
u.ShowWindow.argtypes = [w.HWND, c.c_int]
u.SetWindowPos.argtypes = [w.HWND, w.HWND, c.c_int, c.c_int, c.c_int, c.c_int, w.UINT]
u.GetClientRect.argtypes = [w.HWND, c.POINTER(w.RECT)]
u.keybd_event.argtypes = [w.BYTE, w.BYTE, w.DWORD, c.c_size_t]
callback_type = c.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)
u.EnumWindows.argtypes = [callback_type, w.LPARAM]
previous = u.GetForegroundWindow()
env = dict(os.environ, GHOSTTY_LOG='stderr')
os.chdir(Path(__file__).resolve().parents[1])
log = tempfile.TemporaryFile()
startup = subprocess.STARTUPINFO()
startup.dwFlags = subprocess.STARTF_USESHOWWINDOW
startup.wShowWindow = 0
process = subprocess.Popen([options.executable, '--command=' + options.shell, *options.terminal_arg], env=env, stderr=log, startupinfo=startup)

def windows():
    result = []

    @callback_type
    def visit(hwnd, _):
        pid = w.DWORD()
        u.GetWindowThreadProcessId(hwnd, c.byref(pid))
        title = c.create_unicode_buffer(256)
        u.GetWindowTextW(hwnd, title, 256)
        if pid.value == process.pid and u.IsWindowVisible(hwnd) and ('Ghostty' in title.value):
            result.append(hwnd)
        return True
    u.EnumWindows(visit, 0)
    return result

def wait_count(count, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        assert process.poll() is None, f'App exited: {process.returncode}'
        found = windows()
        if len(found) == count:
            time.sleep(0.4)
            return found
        time.sleep(0.1)
    raise AssertionError(f'Expected {count} windows, got {len(windows())}')

def focus(hwnd):
    kernel = c.WinDLL('kernel32')
    current_thread = kernel.GetCurrentThreadId()
    target_thread = u.GetWindowThreadProcessId(hwnd, None)
    u.ShowWindow(hwnd, 9)
    for attempt in range(10):
        foreground_thread = u.GetWindowThreadProcessId(u.GetForegroundWindow(), None)
        attached_threads = []
        for thread in set((target_thread, foreground_thread)) - {0, current_thread}:
            if u.AttachThreadInput(current_thread, thread, True):
                attached_threads.append(thread)
        try:
            u.BringWindowToTop(hwnd)
            u.SetForegroundWindow(hwnd)
            u.SetFocus(hwnd)
            time.sleep(0.1)
        finally:
            for thread in attached_threads:
                u.AttachThreadInput(current_thread, thread, False)
        if u.GetForegroundWindow() == hwnd:
            break
        time.sleep(0.1)
    assert u.GetForegroundWindow() == hwnd, 'Could not focus test window'
    time.sleep(0.25)

def shortcut(hwnd, key):
    focus(hwnd)
    for vk in (17, 16, key):
        u.keybd_event(vk, 0, 0, 0)
    time.sleep(0.08)
    for vk in (key, 16, 17):
        u.keybd_event(vk, 0, 2, 0)
    time.sleep(0.5)

def open_window(hwnd, count):
    # A tiling manager can move focus again after SetForegroundWindow. Retry
    # only while no window has opened, so a delayed shortcut cannot duplicate it.
    for attempt in range(3):
        shortcut(hwnd, ord('N'))
        try:
            return wait_count(count, 5)
        except AssertionError:
            assert process.poll() is None, f'App exited: {process.returncode}'
            assert len(windows()) == count - 1, 'Unexpected window count during shortcut retry'
            if attempt == 2:
                raise
try:
    first = wait_count(1)[0]
    sample_memory('one_window', 1)
    current = first
    for count in (2, 3, 4):
        prior = set(windows())
        current = next(iter(set(open_window(current, count)) - prior))
        print(f'PASS: {count} windows in PID {process.pid}', flush=True)
    sample_memory('four_windows', 4)
    if options.memory_report:
        for hwnd in windows():
            shortcut(hwnd, ord('E'))
        sample_memory('four_windows_with_floating', 8)
        for hwnd in windows():
            shortcut(hwnd, ord('W'))
        sample_memory('four_windows_after_floating_closed', 4)
    shortcut(current, ord('E'))
    shortcut(current, ord('W'))
    u.PostMessageW(first, 16, 0, 0)
    remaining = wait_count(3)
    print('PASS: original window closed; secondary windows survive', flush=True)
    prior = set(remaining)
    current = next(iter(set(open_window(remaining[0], 4)) - prior))
    print('PASS: surviving window creates another window', flush=True)
    for index, hwnd in enumerate(windows()):
        u.SetWindowPos(hwnd, 0, 50 + index * 40, 50 + index * 40, 750 + index * 50, 500 + index * 40, 4)
        time.sleep(0.25)
    focus(current)
    for key in (ord('E'), ord('X'), ord('I'), ord('T'), 13):
        u.keybd_event(key, 0, 0, 0)
        u.keybd_event(key, 0, 2, 0)
        time.sleep(0.05)
    wait_count(3)
    print('PASS: exiting a shell closes only its window', flush=True)
    for hwnd in windows():
        u.PostMessageW(hwnd, 16, 0, 0)
    assert process.wait(timeout=15) == 0
    log.seek(0)
    diagnostics = log.read().decode('utf-8', errors='replace')
    for failure in ('Graphics device error', 'Terminal render operation failed'):
        assert failure not in diagnostics, diagnostics[-16000:]
    print('PASS: last window closes process cleanly', flush=True)
    if options.memory_report:
        options.memory_report.parent.mkdir(parents=True, exist_ok=True)
        options.memory_report.write_text(json.dumps(memory_samples, indent=2))
finally:
    if options.memory_report and memory_samples:
        options.memory_report.parent.mkdir(parents=True, exist_ok=True)
        options.memory_report.write_text(json.dumps(memory_samples, indent=2))
    for vk in (17, 16):
        u.keybd_event(vk, 0, 2, 0)
    if process.poll() is None:
        for hwnd in windows():
            u.PostMessageW(hwnd, 16, 0, 0)
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.terminate()
    if previous:
        u.SetForegroundWindow(previous)
    log.seek(0)
    diagnostics = log.read().decode('utf-8', errors='replace')
    if process.returncode != 0 or 'err' in diagnostics.lower() or 'panic' in diagnostics.lower():
        print('APP DIAGNOSTICS:\n' + diagnostics[-16000:], flush=True)
    log.close()
