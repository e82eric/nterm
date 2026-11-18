//! nterm's session daemon. It owns every shell and a headless terminal for
//! each, and serves UIs that attach, detach and come back.

pub mod vt;

#[cfg(windows)]
mod conpty;
#[cfg(windows)]
pub mod log;
#[cfg(windows)]
mod configured;
#[cfg(windows)]
mod picker;
#[cfg(windows)]
mod previewing;
#[cfg(windows)]
pub mod relay;
#[cfg(windows)]
pub mod server;
