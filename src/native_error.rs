#[derive(Debug)]
pub(crate) struct FatalError {
    step: &'static str,
    detail: String,
}

impl FatalError {
    pub(crate) fn new(step: &'static str, error: impl std::fmt::Display) -> Self {
        Self {
            step,
            detail: error.to_string(),
        }
    }
}

impl std::fmt::Display for FatalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Unable to {}.\n\n{}", self.step, self.detail)
    }
}

impl std::error::Error for FatalError {}

pub(crate) fn panic_detail(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).into()
    } else {
        "An internal failure occurred without a text diagnostic".into()
    }
}

/// Never permit Rust unwinding to escape an extern C callback. State after
/// an invariant failure is unknown, so display native feedback and terminate.
pub(crate) fn ffi_boundary<T>(operation: &'static str, callback: impl FnOnce() -> T) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(callback)) {
        Ok(value) => value,
        Err(panic) => {
            show(&FatalError::new(operation, panic_detail(panic.as_ref())).to_string());
            std::process::exit(1);
        }
    }
}

pub(crate) trait FatalResult<T> {
    fn fatal(self, step: &'static str) -> Result<T, FatalError>;
}

impl<T, E: std::fmt::Display> FatalResult<T> for Result<T, E> {
    fn fatal(self, step: &'static str) -> Result<T, FatalError> {
        self.map_err(|error| FatalError::new(step, error))
    }
}

#[cfg(test)]
mod fatal_tests {
    use super::FatalResult;

    #[test]
    fn failures_keep_step_and_underlying_error() {
        let result: Result<(), std::io::Error> = Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "driver access denied",
        ));
        let error = result.fatal("create the graphics device").unwrap_err();
        assert_eq!(
            error.to_string(),
            "Unable to create the graphics device.\n\ndriver access denied"
        );
        assert_eq!(
            Ok::<_, std::io::Error>(42)
                .fatal("create the application event loop")
                .unwrap(),
            42
        );
    }
}

/// Report fatal errors without relying on an event loop or renderer.
pub(crate) fn show(message: &str) {
    eprintln!("Ghostty: {message}");
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn MessageBoxW(
                owner: *mut std::ffi::c_void,
                text: *const u16,
                caption: *const u16,
                flags: u32,
            ) -> i32;
        }

        let text = wide(message);
        let caption = wide("Ghostty — Application error");
        // MB_OK | MB_ICONERROR | MB_TASKMODAL: works before a window exists
        // and after the event loop has returned, without creating another loop.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                caption.as_ptr(),
                0x2010,
            );
        }
    }
}

#[cfg(windows)]
fn wide(text: &str) -> Vec<u16> {
    text.replace('\0', "�")
        .encode_utf16()
        .chain(Some(0))
        .collect()
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn dialog_text_preserves_unicode_and_does_not_truncate_at_nul() {
        let encoded = super::wide("Failed: 测试\0details 🦀");
        assert_eq!(encoded.last(), Some(&0));
        assert!(!encoded[..encoded.len() - 1].contains(&0));
        assert_eq!(
            String::from_utf16(&encoded[..encoded.len() - 1]).unwrap(),
            "Failed: 测试�details 🦀"
        );
    }
}
