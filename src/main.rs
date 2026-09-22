mod app;
mod ffi;
mod input;
mod painter;
mod search;
mod terminal;

use std::ffi::CString;

fn main() {
    let args = std::env::args()
        .map(|arg| CString::new(arg).expect("command-line arguments cannot contain NUL bytes"))
        .collect();
    app::run(args);
}
