//! The one ghostty application of the process.
//!
//! It is made before any terminal and freed after the last, so a window
//! needs no terminal to tick it, change its settings or create a terminal.
//! Every window holds the same one.

use crate::ghostty_ffi;
use crate::native_error::FatalError;
use std::ffi::{CString, c_char, c_void};
use std::sync::Mutex;

pub(crate) struct GhosttyApp {
    raw: *mut ghostty_ffi::App,
    /// What the native callbacks are given. Boxed so that it does not move.
    state: Box<State>,
    /// The process arguments, which ghostty reads its configuration from
    /// and keeps pointers into.
    _args: Vec<CString>,
    _argv: Vec<*mut c_char>,
}

struct State {
    /// The UI every window draws with: a theme's colours are set on it.
    context: egui::Context,
    config_issues: Mutex<Vec<String>>,
    /// Failures of app-wide work, as (operation, cause), until a window
    /// shows them.
    issues: Mutex<Vec<(String, String)>>,
}

impl GhosttyApp {
    /// `ctx` is the UI all windows share, which is told the theme's
    /// colours as the configuration loads and whenever they change.
    pub(crate) fn new(ctx: &egui::Context, args: Vec<CString>) -> Result<Self, FatalError> {
        let state = Box::new(State {
            context: ctx.clone(),
            config_issues: Mutex::default(),
            issues: Mutex::default(),
        });
        let mut argv: Vec<*mut c_char> = args.iter().map(|arg| arg.as_ptr().cast_mut()).collect();
        let mut error = [0 as c_char; 2048];
        let raw = unsafe {
            ghostty_ffi::nterm_app_create(
                argv.len(),
                argv.as_mut_ptr(),
                std::ptr::from_ref::<State>(&state).cast_mut().cast(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if raw.is_null() {
            let detail = unsafe { std::ffi::CStr::from_ptr(error.as_ptr()) }.to_string_lossy();
            return Err(FatalError::new("initialize the terminal", detail));
        }
        Ok(Self {
            raw,
            state,
            _args: args,
            _argv: argv,
        })
    }

    pub(crate) fn raw(&self) -> *mut ghostty_ffi::App {
        self.raw
    }

    /// Drains the app's mailbox.
    pub(crate) fn tick(&self) {
        unsafe { ghostty_ffi::nterm_app_tick(self.raw) };
    }

    /// The configuration's issues found while it loaded, once.
    pub(crate) fn take_config_issues(&self) -> Vec<String> {
        std::mem::take(&mut *self.state.config_issues.lock().unwrap())
    }

    /// Failures of app-wide work since the last call.
    pub(crate) fn take_issues(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.state.issues.lock().unwrap())
    }

    pub(crate) fn apply_theme(&self, name: &str) -> Result<(), String> {
        self.apply(Some(name), None, 0.0)
    }

    pub(crate) fn apply_settings(&self, config: &crate::config::ReloadedConfig) -> Result<(), String> {
        self.apply(
            config.theme.as_deref(),
            config.font_family.as_deref(),
            config.font_size.unwrap_or(0.0),
        )
    }

    /// A theme of `None` keeps the theme; a family of `None` and a size of
    /// zero keep the font.
    fn apply(&self, theme: Option<&str>, family: Option<&str>, size: f32) -> Result<(), String> {
        let theme = theme
            .map(CString::new)
            .transpose()
            .map_err(|_| "Theme contains NUL")?;
        let family = family
            .map(CString::new)
            .transpose()
            .map_err(|_| "Font contains NUL")?;
        let mut error = [0u8; 4096];
        if unsafe {
            ghostty_ffi::nterm_app_apply_settings(
                self.raw,
                theme.as_ref().map_or(std::ptr::null(), |value| value.as_ptr()),
                family.as_ref().map_or(std::ptr::null(), |value| value.as_ptr()),
                size,
                error.as_mut_ptr().cast(),
                error.len(),
            )
        } {
            Ok(())
        } else {
            Err(
                String::from_utf8_lossy(error.split(|byte| *byte == 0).next().unwrap_or_default())
                    .into_owned(),
            )
        }
    }
}

impl Drop for GhosttyApp {
    fn drop(&mut self) {
        unsafe { ghostty_ffi::nterm_app_destroy(self.raw) };
    }
}

fn state<'a>(userdata: *mut c_void) -> &'a State {
    unsafe { &*userdata.cast::<State>() }
}

fn text(value: *const c_char) -> String {
    unsafe { std::ffi::CStr::from_ptr(value) }
        .to_string_lossy()
        .into_owned()
}

#[unsafe(no_mangle)]
extern "C" fn nterm_config_issue(userdata: *mut c_void, message: *const c_char) {
    crate::native_error::ffi_boundary("handle native callback nterm_config_issue", || {
        if message.is_null() {
            return;
        }
        let message = text(message);
        state(userdata).config_issues.lock().unwrap().push(format!("Ghostty configuration issue. Invalid settings were ignored.\nFix the configuration and restart the app.\n\n{message}"));
    });
}

#[unsafe(no_mangle)]
extern "C" fn nterm_theme_colors(
    userdata: *mut c_void,
    colors: *const crate::colors::ThemeColors,
    name: *const u8,
    name_len: usize,
) {
    crate::native_error::ffi_boundary("handle native callback nterm_theme_colors", || {
        if colors.is_null() {
            return;
        }
        let name = if name.is_null() {
            None
        } else {
            std::str::from_utf8(unsafe { std::slice::from_raw_parts(name, name_len) }).ok()
        };
        crate::colors::COLORS.apply_theme(&state(userdata).context, unsafe { &*colors }, name);
    });
}

/// App-wide work failed. Kept once per distinct failure, for a window to show.
#[unsafe(no_mangle)]
extern "C" fn nterm_app_issue(userdata: *mut c_void, operation: *const c_char, cause: *const c_char) {
    crate::native_error::ffi_boundary("report a native operational failure", || {
        if operation.is_null() || cause.is_null() {
            return;
        }
        let issue = (text(operation), text(cause));
        crate::error_log::error("libghostty", &issue.0, &issue.1);
        let state = state(userdata);
        let mut issues = state.issues.lock().unwrap_or_else(|error| error.into_inner());
        if issues.len() < 32 && !issues.contains(&issue) {
            issues.push(issue);
        }
        state.context.request_repaint();
    });
}
