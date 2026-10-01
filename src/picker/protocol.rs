use serde::Deserialize;

pub(crate) const MAX_PAYLOAD: usize = 1024 * 1024;

pub(crate) fn new_token() -> Result<String, crate::native_error::FatalError> {
    token_with_random(getrandom::fill)
}

fn token_with_random(
    fill: impl FnOnce(&mut [u8]) -> Result<(), getrandom::Error>,
) -> Result<String, crate::native_error::FatalError> {
    use crate::native_error::FatalResult;
    let mut bytes = [0u8; 32];
    fill(&mut bytes).fatal("generate a secure terminal protocol token")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    #[serde(alias = "name")]
    action: String,
    #[serde(default)]
    params: std::collections::BTreeMap<String, String>,
}

#[derive(Debug)]
pub(crate) struct PickerRequest {
    pub(crate) name: String,
    pub(crate) params: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Item {
    pub(crate) text: String,
    pub(crate) value: String,
}

pub(crate) fn items(
    params: &std::collections::BTreeMap<String, String>,
) -> Result<Vec<Item>, String> {
    let Some(data) = params.get("items") else {
        return Ok(Vec::new());
    };
    // PowerShell's `items | ConvertTo-Json` emits an object for one item.
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ItemList {
        Many(Vec<Item>),
        One(Item),
    }
    let items = match serde_json::from_str(data)
        .map_err(|error| format!("invalid picker items: {error}"))?
    {
        ItemList::Many(items) => items,
        ItemList::One(item) => vec![item],
    };
    validate_items(&items)?;
    Ok(items)
}

pub(crate) fn validate_items(items: &[Item]) -> Result<(), String> {
    if items.len() > 2_000
        || items
            .iter()
            .any(|item| item.text.len() > 8_192 || item.value.len() > 8_192)
    {
        return Err("picker items exceed size limit".into());
    }
    Ok(())
}

pub(crate) fn parse(bytes: &[u8], token: &str) -> Result<PickerRequest, String> {
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
    if request.params.len() > 32
        || request
            .params
            .keys()
            .any(|key| key.is_empty() || key.len() > 128 || key.chars().any(char::is_control))
    {
        return Err("invalid picker parameter name or count".into());
    }
    Ok(PickerRequest {
        name: request.action,
        params: request.params,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_envelope_token_and_schema() {
        assert_eq!(
            parse(br#"StreamingPicker;secret;{"action":"files"}"#, "secret")
                .unwrap()
                .name,
            "files"
        );
        assert_eq!(
            parse(br#"StreamingPicker;secret;{"name":"a;b"}"#, "secret")
                .unwrap()
                .name,
            "a;b"
        );
        for bytes in [
            b"StreamingPicker;wrong;{}".as_slice(),
            br#"StreamingPicker;secret;{"action":""}"#,
            br#"StreamingPicker;secret;{"source":"items"}"#,
            br#"StreamingPicker;secret;{"action":"files","unknown":1}"#,
        ] {
            assert!(parse(bytes, "secret").is_err());
        }
        assert!(parse(&vec![b'x'; MAX_PAYLOAD + 1], "secret").is_err());
        assert!(parse(&[0xff], "secret").is_err());
        let request = parse(br#"StreamingPicker;secret;{"name":"completions","params":{"replaceBefore":"2","items":"[]"}}"#, "secret").unwrap();
        assert_eq!(request.params["replaceBefore"], "2");
        assert!(items(&request.params).unwrap().is_empty());
    }
    #[test]
    fn tokens_are_random_and_printable() {
        let first = new_token().unwrap();
        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, new_token().unwrap());
    }
    #[test]
    fn randomness_failure_returns_error_without_a_fallback_token() {
        let error = super::token_with_random(|bytes| {
            bytes[0] = 42;
            Err(getrandom::Error::UNSUPPORTED)
        })
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("generate a secure terminal protocol token"));
        assert!(message.contains(&getrandom::Error::UNSUPPORTED.to_string()));
    }
    #[test]
    fn parses_item_list_with_separate_display_and_insert_text() {
        let parsed = items(&std::collections::BTreeMap::from([(
            "items".to_string(),
            r#"[{"text":"display","value":"inserted"}]"#.to_string(),
        )]))
        .unwrap();
        assert_eq!(parsed[0].text, "display");
        assert_eq!(parsed[0].value, "inserted");
        let singleton = items(&std::collections::BTreeMap::from([(
            "items".to_string(),
            r#"{"text":"one","value":"single"}"#.to_string(),
        )]))
        .unwrap();
        assert_eq!(singleton[0].value, "single");
    }
}
