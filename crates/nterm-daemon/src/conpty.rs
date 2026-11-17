//! A shell under a pseudoconsole.
//!
//! The pseudoconsole is the `conpty.dll` shipped beside the daemon when there
//! is one, as in the UI, and the system's otherwise. The shell is created the
//! way ghostty creates it: a command line, an explicit environment, and no
//! inherited handles.

use std::{
    io,
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
    sync::OnceLock,
};
use windows_sys::Win32::{
    Foundation::*,
    Storage::FileSystem::{ReadFile, WriteFile},
    System::{
        Console::{COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole},
        LibraryLoader::{GetProcAddress, LoadLibraryExW},
        Pipes::CreatePipe,
        Threading::*,
    },
};

const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: usize = 0x0002_0016;
const LOAD_LIBRARY_SEARCH_APPLICATION_DIR: u32 = 0x200;
const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x8;

type Create = unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> i32;
type Resize = unsafe extern "system" fn(HPCON, COORD) -> i32;
type Close = unsafe extern "system" fn(HPCON);

struct Api {
    create: Create,
    resize: Resize,
    close: Close,
    source: String,
}

fn api() -> &'static Api {
    static API: OnceLock<Api> = OnceLock::new();
    API.get_or_init(|| {
        // `NTERM_CONPTY_DLL` names a conpty.dll for runs from a build
        // directory that has none beside the daemon.
        let (path, flags) = match std::env::var("NTERM_CONPTY_DLL") {
            Ok(path) => (path, LOAD_WITH_ALTERED_SEARCH_PATH),
            Err(_) => ("conpty.dll".to_owned(), LOAD_LIBRARY_SEARCH_APPLICATION_DIR),
        };
        let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
        unsafe {
            let module = LoadLibraryExW(wide.as_ptr(), null_mut(), flags);
            if !module.is_null() {
                let find = |name: &std::ffi::CStr| GetProcAddress(module, name.as_ptr().cast());
                if let (Some(create), Some(resize), Some(close)) = (
                    find(c"CreatePseudoConsole"),
                    find(c"ResizePseudoConsole"),
                    find(c"ClosePseudoConsole"),
                ) {
                    // The exports have the signatures of the system functions
                    // they replace.
                    type Symbol = unsafe extern "system" fn() -> isize;
                    return Api {
                        create: std::mem::transmute::<Symbol, Create>(create),
                        resize: std::mem::transmute::<Symbol, Resize>(resize),
                        close: std::mem::transmute::<Symbol, Close>(close),
                        source: path,
                    };
                }
            }
        }
        Api {
            create: CreatePseudoConsole,
            resize: ResizePseudoConsole,
            close: ClosePseudoConsole,
            source: "system".into(),
        }
    })
}

/// Which pseudoconsole is in use, for the log.
pub fn source() -> &'static str {
    &api().source
}

struct Handle(HANDLE);
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// The pseudoconsole itself. Closing it ends whatever is still attached and
/// flushes the last output.
pub struct Console(HPCON);
unsafe impl Send for Console {}

impl Console {
    pub fn resize(&self, cols: u16, rows: u16) {
        unsafe {
            (api().resize)(self.0, COORD { X: clamp(cols), Y: clamp(rows) });
        }
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        unsafe { (api().close)(self.0) }
    }
}

/// Where the shell's output is read.
pub struct Output(Handle);

impl Output {
    /// Blocks. An error is the end, and is the system's code for why: the
    /// pseudoconsole was closed, or whatever ran it is gone.
    pub fn read(&self, buffer: &mut [u8]) -> Result<usize, u32> {
        let mut count = 0u32;
        let ok = unsafe {
            ReadFile(self.0.0, buffer.as_mut_ptr(), buffer.len() as u32, &mut count, null_mut())
        };
        if ok == 0 { Err(unsafe { GetLastError() }) } else { Ok(count as usize) }
    }
}

/// Where the shell's input is written.
pub struct Input(Handle);

impl Input {
    /// Blocks while the pseudoconsole is not reading. An error once it is
    /// gone: the system's code for why.
    pub fn write(&self, mut bytes: &[u8]) -> Result<(), u32> {
        while !bytes.is_empty() {
            let mut count = 0u32;
            let len = bytes.len().min(1 << 20) as u32;
            if unsafe { WriteFile(self.0.0, bytes.as_ptr(), len, &mut count, null_mut()) } == 0 {
                return Err(unsafe { GetLastError() });
            }
            bytes = &bytes[count as usize..];
        }
        Ok(())
    }
}

/// The shell process.
pub struct Process {
    handle: Handle,
    pub pid: u32,
    /// Creation FILETIME; with the pid it identifies the process.
    pub creation_time: u64,
}

impl Process {
    /// Blocks until the shell ends and returns its exit code.
    pub fn wait(&self) -> u32 {
        let mut code = 0u32;
        unsafe {
            WaitForSingleObject(self.handle.0, INFINITE);
            GetExitCodeProcess(self.handle.0, &mut code);
        }
        code
    }

    pub fn terminate(&self) {
        unsafe {
            TerminateProcess(self.handle.0, 1);
        }
    }
}

pub struct Spawned {
    pub console: Console,
    pub output: Output,
    pub input: Input,
    pub process: Process,
}

fn clamp(cells: u16) -> i16 {
    cells.clamp(1, i16::MAX as u16) as i16
}

fn pipe() -> io::Result<(Handle, Handle)> {
    let (mut read, mut write): (HANDLE, HANDLE) = (null_mut(), null_mut());
    if unsafe { CreatePipe(&mut read, &mut write, null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((Handle(read), Handle(write)))
}

/// A Windows environment block. Empty input means "inherit the daemon's".
fn environment(env: &[(String, String)]) -> Option<Vec<u16>> {
    if env.is_empty() {
        return None;
    }
    let mut pairs: Vec<&(String, String)> =
        env.iter().filter(|(key, _)| !key.is_empty() && !key.contains('\0')).collect();
    // The block is expected sorted by name without regard to case.
    pairs.sort_by_cached_key(|(key, _)| key.to_uppercase());
    let mut block = Vec::new();
    for (key, value) in pairs {
        block.extend(key.encode_utf16());
        block.push(b'=' as u16);
        block.extend(value.encode_utf16().take_while(|unit| *unit != 0));
        block.push(0);
    }
    block.push(0);
    Some(block)
}

/// Start `command` under a new pseudoconsole of the given size.
pub fn spawn(
    command: &str,
    cwd: Option<&str>,
    env: &[(String, String)],
    cols: u16,
    rows: u16,
) -> io::Result<Spawned> {
    if command.trim().is_empty() || command.contains('\0') {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no command"));
    }
    let api = api();
    let (input_read, input_write) = pipe()?;
    let (output_read, output_write) = pipe()?;
    let mut console: HPCON = 0;
    let result = unsafe {
        (api.create)(
            COORD { X: clamp(cols), Y: clamp(rows) },
            input_read.0,
            output_write.0,
            0,
            &mut console,
        )
    };
    if result < 0 {
        return Err(io::Error::other(format!("CreatePseudoConsole failed ({result:#x})")));
    }
    let console = Console(console);
    // The pseudoconsole holds its own copies of these ends.
    drop((input_read, output_write));

    let mut bytes = 0usize;
    unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut bytes) };
    // Aligned for the pointers the list holds.
    let mut list = vec![0usize; bytes.div_ceil(size_of::<usize>())];
    let attributes: LPPROC_THREAD_ATTRIBUTE_LIST = list.as_mut_ptr().cast();
    if unsafe { InitializeProcThreadAttributeList(attributes, 1, 0, &mut bytes) } == 0 {
        return Err(io::Error::last_os_error());
    }
    struct Attributes(LPPROC_THREAD_ATTRIBUTE_LIST);
    impl Drop for Attributes {
        fn drop(&mut self) {
            unsafe { DeleteProcThreadAttributeList(self.0) }
        }
    }
    let _attributes = Attributes(attributes);
    if unsafe {
        UpdateProcThreadAttribute(
            attributes,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
            console.0 as *const _,
            size_of::<HPCON>(),
            null_mut(),
            null(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }

    let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    // Without this the shell would take the daemon's own standard handles,
    // when it has any, instead of the pseudoconsole.
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.lpAttributeList = attributes;
    let mut line: Vec<u16> = command.encode_utf16().chain([0]).collect();
    let directory: Option<Vec<u16>> = cwd
        .filter(|cwd| !cwd.is_empty() && !cwd.contains('\0'))
        .map(|cwd| cwd.encode_utf16().chain([0]).collect());
    let block = environment(env);
    let mut process: PROCESS_INFORMATION = unsafe { zeroed() };
    let mut create = |directory: Option<&Vec<u16>>| unsafe {
        CreateProcessW(
            null(),
            line.as_mut_ptr(),
            null(),
            null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            block.as_ref().map_or(null(), |block| block.as_ptr().cast()),
            directory.map_or(null(), |directory| directory.as_ptr()),
            &startup.StartupInfo,
            &mut process,
        )
    };
    if create(directory.as_ref()) == 0 {
        let error = io::Error::last_os_error();
        // A directory that has gone away should not cost the user the shell.
        let missing = matches!(
            error.raw_os_error().map(|code| code as u32),
            Some(ERROR_DIRECTORY | ERROR_PATH_NOT_FOUND | ERROR_FILE_NOT_FOUND)
        );
        if directory.is_none() || !missing || create(None) == 0 {
            return Err(io::Error::new(error.kind(), format!("cannot start {command:?}: {error}")));
        }
    }
    unsafe {
        CloseHandle(process.hThread);
    }
    let handle = Handle(process.hProcess);
    let (mut start, mut end, mut kernel, mut user): (FILETIME, FILETIME, FILETIME, FILETIME) =
        unsafe { (zeroed(), zeroed(), zeroed(), zeroed()) };
    unsafe { GetProcessTimes(handle.0, &mut start, &mut end, &mut kernel, &mut user) };
    Ok(Spawned {
        console,
        output: Output(output_read),
        input: Input(input_write),
        process: Process {
            handle,
            pid: process.dwProcessId,
            creation_time: (start.dwHighDateTime as u64) << 32 | start.dwLowDateTime as u64,
        },
    })
}
