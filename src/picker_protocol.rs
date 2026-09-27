use serde::Deserialize;

pub(crate) const MAX_PAYLOAD: usize = 2048;

pub(crate) fn new_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("unable to generate terminal OSC token");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    action: String,
}

pub(crate) fn parse(bytes: &[u8], token: &str) -> Result<String, String> {
    if bytes.len() > MAX_PAYLOAD {
        return Err("OSC picker request is too large".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "OSC picker request must be UTF-8")?;
    let mut parts = text.splitn(3, ';');
    if parts.next() != Some("StreamingPicker") {
        return Err("unsupported OSC 9001 protocol".into());
    }
    let supplied = parts.next().ok_or("missing OSC token")?;
    if supplied.len() != token.len()
        || supplied
            .bytes()
            .zip(token.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            != 0
    {
        return Err("invalid OSC picker token".into());
    }
    let request: Request = serde_json::from_str(parts.next().ok_or("missing picker request")?)
        .map_err(|error| format!("invalid picker request: {error}"))?;
    if request.action.is_empty()
        || request.action.len() > 128
        || request.action.chars().any(char::is_control)
    {
        return Err("invalid picker action name".into());
    }
    Ok(request.action)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_envelope_token_and_schema() {
        assert_eq!(
            parse(br#"StreamingPicker;secret;{"action":"files"}"#, "secret").unwrap(),
            "files"
        );
        assert_eq!(
            parse(br#"StreamingPicker;secret;{"action":"a;b"}"#, "secret").unwrap(),
            "a;b"
        );
        for bytes in [
            b"StreamingPicker;wrong;{}".as_slice(),
            br#"StreamingPicker;secret;{"action":""}"#,
            br#"StreamingPicker;secret;{"source":"items"}"#,
            br#"StreamingPicker;secret;{"action":"files","params":{}}"#,
        ] {
            assert!(parse(bytes, "secret").is_err());
        }
        assert!(parse(&vec![b'x'; MAX_PAYLOAD + 1], "secret").is_err());
        assert!(parse(&[0xff], "secret").is_err());
    }
    #[test]
    fn tokens_are_random_and_printable() {
        let first = new_token();
        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, new_token());
    }
}
