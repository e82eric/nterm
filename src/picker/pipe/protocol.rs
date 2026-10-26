//! Picker IPC framing limits and handshake proofs, shared by the terminal's
//! servers and the `npick` client. Keep this free of crate dependencies other
//! than hmac and sha2: `npick` includes it with `#[path]`.
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Frames are a little-endian u32 length followed by that many JSON bytes.
pub(crate) const MAX_FRAME: usize = 1024 * 1024;

pub(crate) fn proof(
    token: &str,
    endpoint: &str,
    server: &str,
    client: &str,
    role: &str,
) -> Vec<u8> {
    transcript_mac(token, endpoint, server, client, role)
        .finalize()
        .into_bytes()
        .to_vec()
}
fn transcript_mac(
    token: &str,
    endpoint: &str,
    server: &str,
    client: &str,
    role: &str,
) -> Hmac<Sha256> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(token.as_bytes()).expect("HMAC accepts any key size");
    // Length-prefixed transcript avoids ambiguous concatenation.
    for part in ["nterm-picker/1", endpoint, server, client, role] {
        mac.update(&(part.len() as u32).to_le_bytes());
        mac.update(part.as_bytes());
    }
    mac
}
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub(crate) fn verify_proof(
    token: &str,
    endpoint: &str,
    server: &str,
    client: &str,
    supplied: &str,
) -> bool {
    if supplied.len() != 64 || !supplied.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    let decoded: Vec<_> = (0..64)
        .step_by(2)
        .map(|i| u8::from_str_radix(&supplied[i..i + 2], 16).unwrap())
        .collect();
    transcript_mac(token, endpoint, server, client, "client")
        .verify_slice(&decoded)
        .is_ok()
}
