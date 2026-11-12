//! What the nterm UI and its session daemon share: the messages they exchange
//! and the pipe they exchange them over.
//!
//! The daemon owns every shell and a headless terminal for each. The UI is a
//! client that can go away and come back.

pub mod protocol;

#[cfg(windows)]
mod identity;
#[cfg(windows)]
pub mod pipe;

/// The ghostty revision this build was made from.
pub const GHOSTTY_REVISION: &str = env!("NTERM_GHOSTTY_REVISION");

/// The session a UI uses when it names none.
pub const DEFAULT_SESSION: &str = "default";
