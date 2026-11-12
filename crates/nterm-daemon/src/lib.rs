//! nterm's session daemon. It owns every shell and a headless terminal for
//! each, and serves UIs that attach, detach and come back.

pub mod vt;

#[cfg(windows)]
mod conpty;
#[cfg(windows)]
pub mod log;
#[cfg(windows)]
pub mod server;
