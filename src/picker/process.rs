use std::io::Read;
use std::process::{Child, ExitStatus};

pub(super) const STDERR_LIMIT: usize = 16 * 1024;

pub(super) struct ChildGuard(pub Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

pub(super) fn read_bounded(mut reader: impl Read, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(output);
        }
        let retain = count.min(limit.saturating_sub(output.len()));
        output.extend_from_slice(&buffer[..retain]);
    }
}

pub(super) fn reader<T: Send + 'static>(
    name: &str,
    read: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<std::thread::JoinHandle<Result<T, String>>, String> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(read)
        .map_err(|error| format!("Unable to start {name}: {error}"))
}

pub(super) fn join<T>(
    reader: std::thread::JoinHandle<Result<T, String>>,
    name: &str,
) -> Result<T, String> {
    reader.join().map_err(|panic| {
        format!(
            "{name} failed: {}",
            crate::native_error::panic_detail(panic.as_ref())
        )
    })?
}

pub(super) fn no_match_exit(program: &str) -> bool {
    matches!(
        program
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(program)
            .to_ascii_lowercase()
            .as_str(),
        "rg" | "rg.exe" | "ripgrep" | "ripgrep.exe"
    )
}

pub(super) fn describe(args: &[String], cwd: Option<&str>) -> String {
    format!(
        "Command: {:?}\nWorking directory: {}",
        args,
        cwd.unwrap_or("inherited")
    )
}

pub(super) fn status_error(status: ExitStatus, stderr: &[u8]) -> String {
    format!(
        "Process exited with {status}\nStderr:\n{}",
        String::from_utf8_lossy(stderr)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_reader_drains_output_and_reports_read_failure() {
        let mut source = std::io::Cursor::new(vec![b'x'; 10000]);
        assert_eq!(read_bounded(&mut source, 100).unwrap().len(), 100);
        assert_eq!(source.position(), 10000);
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("read failed"))
            }
        }
        assert!(
            read_bounded(Broken, 100)
                .unwrap_err()
                .to_string()
                .contains("read failed")
        );
    }
    #[test]
    fn only_ripgrep_gets_default_no_match_exit() {
        assert!(no_match_exit("C:\\tools\\rg.exe"));
        assert!(!no_match_exit("cmd.exe"));
    }
}
