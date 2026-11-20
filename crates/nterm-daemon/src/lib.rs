//! nterm's session daemon. It owns every shell and a headless terminal for
//! each, and serves UIs that attach, detach and come back.

pub mod vt;

/// A shell under a pseudoconsole on Windows, under a pseudoterminal
/// elsewhere: the two have the same face.
#[cfg(windows)]
mod conpty;
#[cfg(unix)]
mod pty;
#[cfg(unix)]
use pty as conpty;

pub mod clipboard;
mod configured;
mod files;
pub mod log;
mod picker;
mod previewing;
pub mod relay;
pub mod server;
