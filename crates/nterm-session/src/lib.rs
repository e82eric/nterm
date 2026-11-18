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
#[cfg(windows)]
pub mod relay;
#[cfg(windows)]
pub mod shell;

/// The ghostty revision this build was made from.
pub const GHOSTTY_REVISION: &str = env!("NTERM_GHOSTTY_REVISION");

/// The session a UI uses when it names none.
pub const DEFAULT_SESSION: &str = "default";

/// What is wrong with a session name that is not one.
pub const SESSION_NAME_RULE: &str = "a session name is letters, digits, '-' and '_'";

/// Whether `name` can name a session. It becomes part of a pipe's name and
/// of a log file's.
pub fn is_session_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_session_name_is_letters_digits_hyphens_and_underscores() {
        for name in ["default", "work-2", "a_b", "X"] {
            assert!(super::is_session_name(name), "{name}");
        }
        for name in ["", "two words", "a/b", "a\\b", "a.b", "caf\u{e9}"] {
            assert!(!super::is_session_name(name), "{name}");
        }
    }
}
