//! A shell under a pseudoterminal, on Unix: what `conpty` is on Windows, with
//! the same face, so that the server does not tell them apart.
//!
//! The shell leads a session of its own with the terminal as its controlling
//! terminal, as it would in any terminal emulator. Its output ends, as a
//! pseudoconsole's does, only once the `Console` is dropped: a shell that has
//! gone can leave a program behind it still writing to the terminal, and what
//! the shell itself wrote last is read before the end.

use std::{
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::process::CommandExt,
    },
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// How long a shell told to end gets before it is killed.
const HANGUP_GRACE: Duration = Duration::from_secs(3);

/// What the shells run under, for the log.
pub fn source() -> &'static str {
    "pty"
}

fn errno() -> u32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32
}

fn size(cols: u16, rows: u16) -> libc::winsize {
    libc::winsize { ws_row: rows.max(1), ws_col: cols.max(1), ws_xpixel: 0, ws_ypixel: 0 }
}

/// The terminal's own side, for resizing. Dropping it ends the output once
/// what is there has been read.
pub struct Console {
    master: Arc<OwnedFd>,
    /// Closed on drop, which wakes the reader.
    _wake: OwnedFd,
}

impl Console {
    pub fn resize(&self, cols: u16, rows: u16) {
        let size = size(cols, rows);
        unsafe {
            libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size);
        }
    }
}

/// Where the shell's output is read.
pub struct Output {
    master: Arc<OwnedFd>,
    woken: OwnedFd,
    /// Whether every program has closed the terminal, after which only the
    /// console's going is waited for.
    hung_up: AtomicBool,
}

impl Output {
    /// Blocks. 0, or an error with the system's code for it, is the end.
    pub fn read(&self, buffer: &mut [u8]) -> Result<usize, u32> {
        loop {
            let hung_up = self.hung_up.load(Ordering::Relaxed);
            let mut fds = [
                libc::pollfd { fd: self.woken.as_raw_fd(), events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: self.master.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            ];
            // Once hung up the terminal is always ready, with nothing to read.
            let count = if hung_up { 1 } else { 2 };
            if unsafe { libc::poll(fds.as_mut_ptr(), count, -1) } < 0 {
                match errno() as i32 {
                    libc::EINTR => continue,
                    code => return Err(code as u32),
                }
            }
            let terminal = fds[1].revents;
            if !hung_up && terminal & libc::POLLIN != 0 {
                let read = unsafe {
                    libc::read(self.master.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len())
                };
                match read {
                    1.. => return Ok(read as usize),
                    0 => self.hung_up.store(true, Ordering::Relaxed),
                    _ => match errno() as i32 {
                        libc::EINTR | libc::EAGAIN => {}
                        // What Linux says once the last program has closed it.
                        libc::EIO => self.hung_up.store(true, Ordering::Relaxed),
                        code => return Err(code as u32),
                    },
                }
                continue;
            }
            if !hung_up && terminal & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                self.hung_up.store(true, Ordering::Relaxed);
                continue;
            }
            // The console has gone, and there is nothing left to read.
            if fds[0].revents != 0 {
                return Ok(0);
            }
        }
    }
}

/// Where the shell's input is written.
pub struct Input(Arc<OwnedFd>);

impl Input {
    /// Blocks while the terminal is full. An error, with the system's code
    /// for it, once nothing reads it any more.
    pub fn write(&self, mut bytes: &[u8]) -> Result<(), u32> {
        while !bytes.is_empty() {
            let wrote = unsafe { libc::write(self.0.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
            if wrote < 0 {
                match errno() as i32 {
                    libc::EINTR | libc::EAGAIN => continue,
                    code => return Err(code as u32),
                }
            }
            bytes = &bytes[wrote as usize..];
        }
        Ok(())
    }
}

/// The shell process.
pub struct Process {
    pub pid: u32,
    /// When it started, in clock ticks after the machine did; with the pid
    /// it identifies the process.
    pub creation_time: u64,
    /// Whether it has been waited for, after which its pid may be another
    /// process's and is not signalled.
    reaped: Arc<Mutex<bool>>,
}

impl Process {
    /// Blocks until the shell ends and returns its exit code: 128 and the
    /// signal's number for one a signal ended.
    pub fn wait(&self) -> u32 {
        let pid = self.pid as libc::pid_t;
        // Until it is reaped below, a signal cannot reach another process
        // by its number.
        loop {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let waited =
                unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, libc::WEXITED | libc::WNOWAIT) };
            if waited == 0 || errno() as i32 != libc::EINTR {
                break;
            }
        }
        let mut reaped = self.reaped.lock().unwrap();
        let mut status = 0;
        let code = loop {
            if unsafe { libc::waitpid(pid, &mut status, 0) } == pid {
                break if libc::WIFEXITED(status) {
                    libc::WEXITSTATUS(status) as u32
                } else if libc::WIFSIGNALED(status) {
                    128 + libc::WTERMSIG(status) as u32
                } else {
                    1
                };
            }
            if errno() as i32 != libc::EINTR {
                break 1;
            }
        };
        *reaped = true;
        code
    }

    /// Hang up on the shell's process group, as closing a terminal does,
    /// and kill the group if it is still there after `HANGUP_GRACE`.
    pub fn terminate(&self) {
        if !signal(&self.reaped, self.pid, libc::SIGHUP) {
            return;
        }
        let (reaped, pid) = (self.reaped.clone(), self.pid);
        let _ = std::thread::Builder::new().name(format!("kill {pid}")).spawn(move || {
            std::thread::sleep(HANGUP_GRACE);
            signal(&reaped, pid, libc::SIGKILL);
        });
    }
}

/// Send `signal` to the process group `pid` leads, unless it has been
/// reaped. Whether it was sent.
fn signal(reaped: &Mutex<bool>, pid: u32, signal: libc::c_int) -> bool {
    let reaped = reaped.lock().unwrap();
    !*reaped && unsafe { libc::kill(-(pid as libc::pid_t), signal) } == 0
}

pub struct Spawned {
    pub console: Console,
    pub output: Output,
    pub input: Input,
    pub process: Process,
}

fn close_on_exec(fd: &OwnedFd) -> io::Result<()> {
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The account's login shell: `$SHELL` in `env`, or the account's entry, or
/// `/bin/sh`.
fn login_shell(env: &[(String, String)]) -> String {
    if let Some((_, shell)) = env.iter().find(|(key, value)| key == "SHELL" && !value.is_empty()) {
        return shell.clone();
    }
    let entry = unsafe { libc::getpwuid(libc::getuid()) };
    if !entry.is_null() {
        let shell = unsafe { std::ffi::CStr::from_ptr((*entry).pw_shell) };
        if let Ok(shell) = shell.to_str()
            && !shell.is_empty()
        {
            return shell.to_owned();
        }
    }
    "/bin/sh".into()
}

/// How `command` is run: by `/bin/sh -c`, as a command line; with none, the
/// login shell, as a login shell.
fn command_for(command: &str, env: &[(String, String)]) -> Command {
    if command.trim().is_empty() {
        let shell = login_shell(env);
        let name = Path::new(&shell).file_name().map_or_else(|| "sh".into(), |name| name.to_string_lossy());
        let mut login = Command::new(&shell);
        login.arg0(format!("-{name}"));
        login
    } else {
        let mut line = Command::new("/bin/sh");
        line.arg("-c").arg(command);
        line
    }
}

/// When `pid` started, from `/proc`, or 0 where there is none.
fn started(pid: u32) -> u64 {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { return 0 };
    // The name, in parentheses, may hold anything; the fields follow it.
    let Some((_, fields)) = stat.rsplit_once(')') else { return 0 };
    // `starttime` is the 22nd field, the 20th after the name.
    fields.split_whitespace().nth(19).and_then(|field| field.parse().ok()).unwrap_or(0)
}

/// Start `command` under a new pseudoterminal of the given size. An empty
/// command is the account's login shell.
pub fn spawn(
    command: &str,
    cwd: Option<&str>,
    env: &[(String, String)],
    cols: u16,
    rows: u16,
) -> io::Result<Spawned> {
    if command.contains('\0') {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "the command holds a NUL"));
    }
    let (mut master, mut slave) = (-1, -1);
    let size = size(cols, rows);
    if unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), &size) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    // Neither is for the programs the shell starts.
    close_on_exec(&master)?;
    close_on_exec(&slave)?;

    let build = |cwd: Option<&str>| -> io::Result<Command> {
        let mut shell = command_for(command, env);
        shell
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave.try_clone()?));
        if !env.is_empty() {
            shell.env_clear().envs(
                env.iter()
                    .filter(|(key, value)| !key.is_empty() && !key.contains(['=', '\0']) && !value.contains('\0'))
                    .map(|(key, value)| (key, value)),
            );
        }
        if let Some(cwd) = cwd {
            shell.current_dir(cwd);
        }
        unsafe {
            shell.pre_exec(|| {
                // A session of its own, with the terminal as its controlling
                // terminal: the shell's job control depends on it.
                if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                // The daemon ignores hangups; the shell must not.
                libc::signal(libc::SIGHUP, libc::SIG_DFL);
                Ok(())
            });
        }
        Ok(shell)
    };
    let cwd = cwd.filter(|cwd| !cwd.is_empty());
    let child = match build(cwd)?.spawn() {
        Ok(child) => child,
        // A directory that has gone away should not cost the user the shell.
        Err(_) if cwd.is_some_and(|cwd| !Path::new(cwd).is_dir()) => build(None)?
            .spawn()
            .map_err(|error| io::Error::new(error.kind(), format!("cannot start {command:?}: {error}")))?,
        Err(error) => {
            return Err(io::Error::new(error.kind(), format!("cannot start {command:?}: {error}")));
        }
    };
    let pid = child.id();
    // Waited for by its pid in `Process::wait`; dropping this leaves it be.
    drop(child);
    drop(slave);

    let mut pipe = [-1; 2];
    if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        let error = io::Error::last_os_error();
        unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
        return Err(error);
    }
    let (woken, wake) = unsafe { (OwnedFd::from_raw_fd(pipe[0]), OwnedFd::from_raw_fd(pipe[1])) };
    let master = Arc::new(master);
    Ok(Spawned {
        console: Console { master: master.clone(), _wake: wake },
        output: Output { master: master.clone(), woken, hung_up: AtomicBool::new(false) },
        input: Input(master),
        process: Process { pid, creation_time: started(pid), reaped: Arc::new(Mutex::new(false)) },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything the shell writes until the output ends: the console is
    /// dropped once the shell has been waited for, as the server does.
    fn run(command: &str, env: &[(String, String)], cwd: Option<&str>) -> (String, u32) {
        let Spawned { console, output, input, process } = spawn(command, cwd, env, 80, 24).unwrap();
        let reader = std::thread::spawn(move || {
            let mut all = Vec::new();
            let mut buffer = [0u8; 4096];
            while let Ok(count @ 1..) = output.read(&mut buffer) {
                all.extend_from_slice(&buffer[..count]);
            }
            String::from_utf8_lossy(&all).into_owned()
        });
        let code = process.wait();
        drop(console);
        drop(input);
        (reader.join().unwrap(), code)
    }

    #[test]
    fn a_command_line_runs_under_a_terminal_of_the_size_asked_for() {
        let (text, code) = run("stty size; test -t 0 && echo is-a-terminal; exit 3", &[], None);
        assert!(text.contains("24 80"), "{text:?}");
        assert!(text.contains("is-a-terminal"), "{text:?}");
        assert_eq!(code, 3);
    }

    #[test]
    fn what_the_shell_writes_last_is_read_before_the_end() {
        let (text, code) = run("seq 1 20000", &[], None);
        assert!(text.trim_end().ends_with("20000"), "{}", &text[text.len().saturating_sub(40)..]);
        assert_eq!(code, 0);
    }

    #[test]
    fn the_environment_and_folder_are_the_ones_given_and_a_missing_folder_is_left_out() {
        let env = vec![("NTERM_PTY_TEST".to_owned(), "given".to_owned()), ("PATH".to_owned(), "/usr/bin:/bin".to_owned())];
        let (text, _) = run("echo \"[$NTERM_PTY_TEST] [$HOME] $(pwd)\"", &env, Some("/usr"));
        // Only what was given: no HOME.
        assert!(text.contains("[given] [] /usr"), "{text:?}");
        let (text, code) = run("pwd", &env, Some("/no/such/folder/anywhere"));
        assert_eq!(code, 0, "{text:?}");
    }

    #[test]
    fn a_resize_reaches_the_program() {
        let Spawned { console, output, input, process } = spawn("sleep 0.3; stty size", None, &[], 80, 24).unwrap();
        console.resize(100, 30);
        let code = process.wait();
        drop(console);
        drop(input);
        let mut text = Vec::new();
        let mut buffer = [0u8; 256];
        while let Ok(count @ 1..) = output.read(&mut buffer) {
            text.extend_from_slice(&buffer[..count]);
        }
        let text = String::from_utf8_lossy(&text);
        assert!(text.contains("30 100"), "{text:?}");
        assert_eq!(code, 0);
    }

    /// Read until the output has said `text`.
    fn until(output: &Output, text: &str) {
        let mut all = Vec::new();
        let mut buffer = [0u8; 256];
        while !String::from_utf8_lossy(&all).contains(text) {
            let count = output.read(&mut buffer).unwrap();
            assert!(count > 0, "ended before {text:?}: {:?}", String::from_utf8_lossy(&all));
            all.extend_from_slice(&buffer[..count]);
        }
    }

    #[test]
    fn terminating_hangs_up_on_the_shell_and_a_shell_that_ignores_it_is_killed() {
        let spawned = spawn("sleep 30", None, &[], 80, 24).unwrap();
        spawned.process.terminate();
        assert_eq!(spawned.process.wait(), 128 + libc::SIGHUP as u32);

        let stubborn = spawn("trap '' HUP; echo ready; sleep 30", None, &[], 80, 24).unwrap();
        // Not before it ignores the hangup.
        until(&stubborn.output, "ready");
        let began = std::time::Instant::now();
        stubborn.process.terminate();
        assert_eq!(stubborn.process.wait(), 128 + libc::SIGKILL as u32);
        assert!(began.elapsed() >= HANGUP_GRACE);
        // Reaped: terminating again signals nothing.
        stubborn.process.terminate();
    }

    #[test]
    fn output_goes_on_after_the_shell_until_the_console_is_dropped() {
        // The shell leaves a program behind it that still has the terminal,
        // and that ignores the hangup the shell's end sends it.
        let Spawned { console, output, input, process } =
            spawn("trap '' HUP; (sleep 0.5; echo left-behind) & echo shell-done", None, &[], 80, 24).unwrap();
        let reader = std::thread::spawn(move || {
            let mut all = Vec::new();
            let mut buffer = [0u8; 256];
            while let Ok(count @ 1..) = output.read(&mut buffer) {
                all.extend_from_slice(&buffer[..count]);
            }
            String::from_utf8_lossy(&all).into_owned()
        });
        assert_eq!(process.wait(), 0);
        std::thread::sleep(Duration::from_secs(1));
        drop(console);
        drop(input);
        let text = reader.join().unwrap();
        assert!(text.contains("shell-done") && text.contains("left-behind"), "{text:?}");
    }

    #[test]
    fn with_no_command_the_login_shell_starts_as_one() {
        let env = vec![("SHELL".to_owned(), "/bin/sh".to_owned()), ("PATH".to_owned(), "/usr/bin:/bin".to_owned())];
        let Spawned { console, output, input, process } = spawn("", None, &env, 80, 24).unwrap();
        input.write(b"echo \"name=$0\"; exit 7\n").unwrap();
        let code = process.wait();
        drop(console);
        let mut text = Vec::new();
        let mut buffer = [0u8; 256];
        while let Ok(count @ 1..) = output.read(&mut buffer) {
            text.extend_from_slice(&buffer[..count]);
        }
        let text = String::from_utf8_lossy(&text);
        assert!(text.contains("name=-sh"), "{text:?}");
        assert_eq!(code, 7);
    }
}
