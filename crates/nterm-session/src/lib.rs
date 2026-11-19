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

/// The path in the URL a shell reports its directory as, such as
/// `file://host/C:/my%20src`, with what the URL escaped put back. Text that
/// is not a URL is taken as the path.
pub fn url_path(pwd: &str) -> String {
    let path = match pwd.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("", |at| &rest[at..]),
        None => return pwd.to_owned(),
    };
    let mut bytes = Vec::with_capacity(path.len());
    let mut rest = path.as_bytes();
    while let [first, after @ ..] = rest {
        let escaped = (*first == b'%')
            .then(|| after.get(..2))
            .flatten()
            .and_then(|hex| u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok());
        match escaped {
            Some(byte) => {
                bytes.push(byte);
                rest = &after[2..];
            }
            None => {
                bytes.push(*first);
                rest = after;
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The folder a shell reported as its directory, as a path a shell can be
/// started in: its `url_path`, where on Windows `/C:/src` is `C:/src`.
pub fn reported_folder(pwd: &str) -> String {
    let path = url_path(pwd);
    let bytes = path.as_bytes();
    let drive =
        bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':';
    if cfg!(windows) && drive { path[1..].to_owned() } else { path }
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

    #[test]
    #[cfg(windows)]
    fn a_reported_folder_is_a_path_a_shell_can_start_in() {
        let folder = super::reported_folder;
        assert_eq!(folder("kitty-shell-cwd://localhost/C:/src/nterm"), "C:/src/nterm");
        assert_eq!(folder("file://host/C:/my%20src/caf%C3%A9"), "C:/my src/café");
        // Text that is no URL is the path already.
        assert_eq!(folder("C:\\src\\100%"), "C:\\src\\100%");
        assert_eq!(folder("file://host"), "");
    }
}
