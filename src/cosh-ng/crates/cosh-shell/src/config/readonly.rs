use crate::tools::readonly_rules::{
    PathMode, ReadonlyRuleKey, RuntimeGenericSpec, RuntimeReadonlySpec, RuntimeSubcommandSpec,
    RuntimeValidator,
};

pub(super) fn parse_disabled_rules(
    value: &toml::Value,
    key: &str,
) -> Result<Vec<ReadonlyRuleKey>, String> {
    let Some(items) = value.as_array() else {
        return Err(format!("{key} must be an array"));
    };
    items
        .iter()
        .map(|item| {
            let Some(raw) = item.as_str() else {
                return Err(format!("{key} entries must be strings"));
            };
            let parts = raw.split_whitespace().collect::<Vec<_>>();
            match parts.as_slice() {
                [command] => Ok(ReadonlyRuleKey::command(*command)),
                [command, subcommand] => Ok(ReadonlyRuleKey::subcommand(*command, *subcommand)),
                _ => Err(format!("invalid readonly_disabled entry: {raw}")),
            }
        })
        .collect()
}

pub(super) fn parse_runtime_spec(
    command: &str,
    value: &toml::Value,
    prefix: &str,
) -> Result<Option<RuntimeReadonlySpec>, String> {
    let Some(table) = value.as_table() else {
        return Err(format!("{prefix}.{command} must be a table"));
    };
    if table
        .get("disabled")
        .and_then(toml::Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(None);
    }

    let label = format!("{prefix}.{command}");
    let validator = parse_validator_table(&label, table)?;
    Ok(Some(RuntimeReadonlySpec {
        command: command.to_string(),
        validator,
    }))
}

fn parse_validator_table(
    label: &str,
    table: &toml::map::Map<String, toml::Value>,
) -> Result<RuntimeValidator, String> {
    let kind = table
        .get("type")
        .and_then(toml::Value::as_str)
        .unwrap_or("generic");
    match kind {
        "bare" => Ok(RuntimeValidator::Bare),
        "generic" => Ok(RuntimeValidator::Generic(parse_generic_spec(label, table)?)),
        "version_check" => Ok(RuntimeValidator::VersionCheck(required_string_array(
            label, table, "flags",
        )?)),
        "subcommand" => Ok(RuntimeValidator::Subcommand(parse_subcommand_spec(
            label, table,
        )?)),
        other => Err(format!("{label}: unknown readonly validator type {other}")),
    }
}

fn parse_generic_spec(
    label: &str,
    table: &toml::map::Map<String, toml::Value>,
) -> Result<RuntimeGenericSpec, String> {
    Ok(RuntimeGenericSpec {
        short_flags: table
            .get("short_flags")
            .and_then(toml::Value::as_str)
            .unwrap_or("")
            .to_string(),
        long_flags: optional_string_array(table, "long_flags")?,
        value_flags: optional_value_flags(table, "value_flags")?,
        deny_flags: optional_string_array(table, "deny_flags")?,
        path_mode: parse_path_mode(label, table.get("path_mode"))?,
        bare_number_max: optional_u32(label, table, "bare_number_max")?.unwrap_or(0),
    })
}

fn parse_subcommand_spec(
    label: &str,
    table: &toml::map::Map<String, toml::Value>,
) -> Result<RuntimeSubcommandSpec, String> {
    let Some(subcommands) = table.get("subcommands").and_then(toml::Value::as_table) else {
        return Err(format!(
            "{label}: subcommand validator needs subcommands table"
        ));
    };
    let mut parsed = Vec::new();
    for (name, value) in subcommands {
        let Some(table) = value.as_table() else {
            return Err(format!("{label}.{name}: subcommand must be a table"));
        };
        parsed.push((
            name.clone(),
            parse_validator_table(&format!("{label}.{name}"), table)?,
        ));
    }
    Ok(RuntimeSubcommandSpec {
        deny_args: optional_string_array(table, "deny_args")?,
        subcommands: parsed,
    })
}

fn parse_path_mode(label: &str, value: Option<&toml::Value>) -> Result<PathMode, String> {
    match value.and_then(toml::Value::as_str).unwrap_or("none") {
        "none" => Ok(PathMode::None),
        "optional" => Ok(PathMode::Optional),
        "required" => Ok(PathMode::Required),
        "unchecked" => Ok(PathMode::Unchecked),
        other => Err(format!("{label}: invalid path_mode {other}")),
    }
}

fn required_string_array(
    label: &str,
    table: &toml::map::Map<String, toml::Value>,
    key: &str,
) -> Result<Vec<String>, String> {
    table
        .get(key)
        .ok_or_else(|| format!("{label}: missing {key}"))
        .and_then(|value| string_array(value, key))
}

fn optional_string_array(
    table: &toml::map::Map<String, toml::Value>,
    key: &str,
) -> Result<Vec<String>, String> {
    match table.get(key) {
        Some(value) => string_array(value, key),
        None => Ok(Vec::new()),
    }
}

pub(super) fn string_array(value: &toml::Value, key: &str) -> Result<Vec<String>, String> {
    let Some(items) = value.as_array() else {
        return Err(format!("{key} must be an array"));
    };
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{key} entries must be strings"))
        })
        .collect()
}

fn optional_value_flags(
    table: &toml::map::Map<String, toml::Value>,
    key: &str,
) -> Result<Vec<(String, Option<u32>)>, String> {
    let Some(value) = table.get(key) else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(format!("{key} must be an array"));
    };
    items.iter().map(parse_value_flag).collect()
}

fn parse_value_flag(value: &toml::Value) -> Result<(String, Option<u32>), String> {
    if let Some(items) = value.as_array() {
        let Some(flag) = items.first().and_then(toml::Value::as_str) else {
            return Err("value_flags entry needs a flag string".to_string());
        };
        let bound = match items.get(1) {
            Some(value) => Some(u32_from_toml(value, "value_flags max")?),
            None => None,
        };
        return Ok((flag.to_string(), bound));
    }
    if let Some(table) = value.as_table() {
        let Some(flag) = table.get("flag").and_then(toml::Value::as_str) else {
            return Err("value_flags table needs flag".to_string());
        };
        let bound = match table.get("max") {
            Some(value) => Some(u32_from_toml(value, "value_flags max")?),
            None => None,
        };
        return Ok((flag.to_string(), bound));
    }
    Err("value_flags entries must be arrays or tables".to_string())
}

fn optional_u32(
    label: &str,
    table: &toml::map::Map<String, toml::Value>,
    key: &str,
) -> Result<Option<u32>, String> {
    table
        .get(key)
        .map(|value| u32_from_toml(value, &format!("{label}.{key}")))
        .transpose()
}

fn u32_from_toml(value: &toml::Value, label: &str) -> Result<u32, String> {
    let Some(n) = value.as_integer() else {
        return Err(format!("{label} must be an integer"));
    };
    u32::try_from(n).map_err(|_| format!("{label} must be a positive u32"))
}
