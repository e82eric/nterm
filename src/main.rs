#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

mod actions;
mod app;
mod colors;
mod config;
mod copy_mode;
mod error_log;
mod ghostty_ffi;
mod input;
mod messages;
mod native_error;
mod painter;
mod picker;
mod presentation;
mod quick_select;
mod search;
mod terminal;
mod themes;
mod toast;
mod worker;

use std::ffi::CString;

fn main() -> std::process::ExitCode {
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).unwrap_or_else(|panic| {
            Err(native_error::FatalError::new(
                "continue after an internal application failure",
                native_error::panic_detail(panic.as_ref()),
            ))
        });
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            native_error::show(&error.to_string());
            std::process::ExitCode::FAILURE
        }
    }
}

fn encode_args(args: Vec<String>) -> Result<Vec<CString>, native_error::FatalError> {
    args.into_iter()
        .enumerate()
        .map(|(index, arg)| {
            CString::new(arg).map_err(|error| {
                native_error::FatalError::new(
                    "encode startup arguments",
                    format!("Argument {index}: {error}"),
                )
            })
        })
        .collect()
}

fn run() -> Result<(), native_error::FatalError> {
    use native_error::FatalResult;
    let mut issues = Vec::new();
    let arguments = std::env::args_os()
        .enumerate()
        .map(|(index, argument)| {
            argument.into_string().map_err(|_| {
                native_error::FatalError::new(
                    "read startup arguments",
                    format!("Argument {index} contains invalid Unicode"),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (args, pickers, floating_padding, ui_colors) =
        config::prepare(arguments, &mut issues).fatal("read startup arguments")?;
    let args = encode_args(args)?;
    crate::colors::COLORS.configure(ui_colors);
    app::run(args, pickers, floating_padding, issues)
}

#[cfg(test)]
mod startup_tests {
    #[test]
    fn nul_argument_is_reported_without_panicking_or_echoing_contents() {
        let error = super::encode_args(vec!["ghostty".into(), "secret\0argument".into()])
            .unwrap_err()
            .to_string();
        assert!(error.contains("Argument 1"));
        assert!(!error.contains("secret"));
    }
}
