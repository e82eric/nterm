mod app;
mod copy_mode;
mod ffi;
mod frame_trace;
mod input;
mod painter;
mod presentation;
mod search;
mod terminal;

use std::ffi::CString;

fn main() {
    let args = std::env::args()
        .map(|arg| CString::new(arg).expect("command-line arguments cannot contain NUL bytes"))
        .collect();
    app::run(args);
}
