#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

mod actions;
mod app;
mod config;
mod copy_mode;
mod ghostty_ffi;
mod input;
mod painter;
mod picker;
mod presentation;
mod quick_select;
mod search;
mod terminal;

use std::ffi::CString;

fn main() {
    let (args, pickers, floating_padding) = match config::prepare(std::env::args().collect()) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    let args = args
        .into_iter()
        .map(|arg| CString::new(arg).expect("command-line arguments cannot contain NUL bytes"))
        .collect();
    app::run(args, pickers, floating_padding);
}
