use serde_json::Value;

use super::{AuthFieldInfo, AuthProviderInfo};

pub(super) fn parse_auth_provider(item: &Value) -> Option<AuthProviderInfo> {
    let id = item.get("id")?.as_str()?.to_string();
    let label = item.get("label")?.as_str()?.to_string();
    let description = optional_string(item, "description");
    let description_zh_cn = optional_string(item, "description_zh_cn");
    let builtin_base_url = optional_string(item, "builtin_base_url");
    let fields = item
        .get("fields")
        .and_then(Value::as_array)
        .map(|fields| fields.iter().filter_map(parse_auth_field).collect())
        .unwrap_or_default();
    Some(AuthProviderInfo {
        id,
        label,
        description,
        description_zh_cn,
        builtin_base_url,
        fields,
    })
}

fn parse_auth_field(field: &Value) -> Option<AuthFieldInfo> {
    Some(AuthFieldInfo {
        name: field.get("name")?.as_str()?.to_string(),
        label: field.get("label")?.as_str()?.to_string(),
        hint: optional_string(field, "hint"),
        secret: field
            .get("secret")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        required: field
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        placeholder: optional_string(field, "placeholder"),
    })
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}
