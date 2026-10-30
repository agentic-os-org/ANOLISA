//! Provider-owned configuration; AW treats these fields as opaque JSON.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Config {
    version: u8,
    pub(super) mode: Mode,
    #[serde(default)]
    pub(super) unmapped_tool: UnmappedTool,
    #[serde(default)]
    pub(super) tools: BTreeMap<String, Tool>,
    #[serde(default)]
    pub(super) prompt: Option<Prompt>,
    pub(super) pii: Option<Pii>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pii {
    pub(super) input_mode: Mode,
    #[serde(rename = "output_mode")]
    _output_mode: OutputMode,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum OutputMode {
    Observe,
    ReplaceResult,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Prompt {
    pub(super) mode: Mode,
    // ML modes require independently verified model resources and context.
    #[serde(rename = "scan_mode")]
    _scan_mode: Fast,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Fast {
    Fast,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Mode {
    Observe,
    Block,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum UnmappedTool {
    #[default]
    Error,
    Observe,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Tool {
    pub(super) language: Language,
    pub(super) input_pointer: String,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Language {
    Bash,
    Python,
}

impl Language {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Python => "python",
        }
    }
}

impl Config {
    pub(super) fn parse(value: &Value) -> Result<Self, &'static str> {
        let config: Self = serde_json::from_value(value.clone()).map_err(|_| "invalid_config")?;
        if config.version != 1
            || (config.tools.is_empty() && config.prompt.is_none() && config.pii.is_none())
            || config.tools.len() > 128
        {
            return Err("invalid_config");
        }
        for (name, tool) in &config.tools {
            if name.trim().is_empty() || name.len() > 1024 || !valid_pointer(&tool.input_pointer) {
                return Err("invalid_config");
            }
        }
        Ok(config)
    }
}

fn valid_pointer(pointer: &str) -> bool {
    if pointer.len() > 1024 || (!pointer.is_empty() && !pointer.starts_with('/')) {
        return false;
    }
    let mut chars = pointer.chars();
    while let Some(value) = chars.next() {
        if value == '~' && !matches!(chars.next(), Some('0' | '1')) {
            return false;
        }
    }
    true
}
