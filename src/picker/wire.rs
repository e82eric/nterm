use serde::Deserialize;

pub(crate) fn new_token() -> Result<String, crate::native_error::FatalError> {
    token_with_random(getrandom::fill)
}

fn token_with_random(
    fill: impl FnOnce(&mut [u8]) -> Result<(), getrandom::Error>,
) -> Result<String, crate::native_error::FatalError> {
    use crate::native_error::FatalResult;
    let mut bytes = [0u8; 32];
    fill(&mut bytes).fatal("generate a secure picker pipe token")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
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

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(message.contains("generate a secure picker pipe token"));
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
