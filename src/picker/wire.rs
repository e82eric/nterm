use serde::Deserialize;

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
