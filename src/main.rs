mod app;
mod config;
mod copy_mode;
mod ffi;
mod frame_trace;
mod input;
mod painter;
mod picker;
mod picker_action;
mod picker_config;
mod picker_files;
mod picker_font;
mod presentation;
mod quick_select;
mod search;
mod terminal;

use std::ffi::CString;

fn main() {
    let (args, pickers) = match config::prepare(std::env::args().collect()) {
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
    app::run(args, pickers);
}
