// Copyright 2026 Alibaba Cloud
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Loading the committed scenario assets, and deciding what each side can act
//! on.
//!
//! Applicability is derived from the payload itself — can this tool message be
//! parsed as JSON, is a tools array present — never from the message role. A
//! `role: "tool"` message carrying raw Python is rejected by tokenless with a
//! JSON parse error, so counting it as "tokenless applies here" would report a
//! capability gap as a 0% compression result.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{L3Error, Suite};

/// One message of an OpenAI-format conversation.
///
/// Kept as a loose map rather than a typed enum: the assets come from
/// the reference's generators, and any field this harness does not understand must
/// still survive round-tripping so both sides receive byte-identical input.
pub type Message = serde_json::Map<String, Value>;

/// Where a scenario asset came from, carried into the report so a number can be
/// traced back to the generator that produced its payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetSource {
    /// Human-readable pointer to the upstream definition.
    pub reference: String,
    /// False only for assets this repo adds on top of the reference's fixtures.
    pub headroom_native: bool,
    /// reference revision the generator ran against, when it could be read.
    pub headroom_revision: Option<String>,
    /// Whether that reference checkout had uncommitted tracked changes.
    pub headroom_dirty: Option<bool>,
}

/// A committed scenario: a conversation, plus what the reference expects of it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    /// Which of the reference's suites this came from.
    pub suite: Suite,
    /// Stable identifier, unique within a suite and content type.
    pub scenario: String,
    /// the reference's own name for the scenario, when it has one.
    #[serde(default)]
    pub display_name: Option<String>,
    /// the reference's `content_type` grouping.
    pub content_type: String,
    /// the reference's own size label, e.g. "1K items".
    #[serde(default)]
    pub size_label: Option<String>,
    /// Provenance of the payload.
    pub source: AssetSource,
    /// Latency target the reference sets for this scenario, when it sets one.
    #[serde(default)]
    pub headroom_target_ms: Option<f64>,
    /// Context limit the reference pipeline is invoked with.
    pub model_limit: u64,
    /// The conversation both sides receive.
    pub messages: Vec<Message>,
    /// Tool schemas sent before the model call, when the scenario has any.
    #[serde(default)]
    pub tools: Option<Vec<Value>>,
}

/// Why a side cannot act on a scenario, or that it can.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Applicability {
    /// The side has at least one entry point into this payload.
    Applicable {
        /// Which entry points matched, for the report.
        entry_points: Vec<String>,
    },
    /// The side ships no compressor that accepts this payload shape.
    NoEntryPoint {
        /// Why nothing matched, in reviewer-facing terms.
        reason: String,
    },
}

impl Applicability {
    /// Whether the side can act at all.
    pub fn is_applicable(&self) -> bool {
        matches!(self, Applicability::Applicable { .. })
    }
}

impl Scenario {
    /// Indices of `role: "tool"` messages whose content parses as JSON.
    ///
    /// These are the only messages tokenless' `ResponseCompressor` can accept:
    /// the CLI rejects non-JSON stdin outright, and the production hook calls
    /// `skip()` when the payload does not parse.
    pub fn json_tool_messages(&self) -> Vec<usize> {
        self.messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.get("role").and_then(Value::as_str) == Some("tool"))
            .filter(|(_, m)| {
                m.get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|c| serde_json::from_str::<Value>(c).is_ok())
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Count of `role: "tool"` messages regardless of payload shape.
    pub fn tool_message_count(&self) -> usize {
        self.messages
            .iter()
            .filter(|m| m.get("role").and_then(Value::as_str) == Some("tool"))
            .count()
    }

    /// What tokenless can act on in this scenario, decided from the payload.
    ///
    /// Two entry points are reachable from a committed conversation:
    /// `compress-schema` over a tools array (its BeforeModel hook) and
    /// `compress-response` over JSON tool responses (its PostToolUse hook). The
    /// remaining two compressors are out of scope here by construction — TOON
    /// is a pipeline step after `compress-response`, and rtk rewrites shell
    /// commands, which these assets do not carry.
    pub fn tokenless_applicability(&self) -> Applicability {
        let mut entry_points = Vec::new();
        if self.tools.as_ref().is_some_and(|t| !t.is_empty()) {
            entry_points.push("compress-schema (tools array)".to_string());
        }
        let json_tools = self.json_tool_messages().len();
        if json_tools > 0 {
            entry_points.push(format!(
                "compress-response ({json_tools} JSON tool messages)"
            ));
        }
        if !entry_points.is_empty() {
            return Applicability::Applicable { entry_points };
        }

        // Nothing matched: say precisely why, since this is the layer's most
        // load-bearing finding.
        let total_tools = self.tool_message_count();
        let reason = if total_tools > 0 {
            format!(
                "{total_tools} tool message(s), none parsing as JSON: tokenless' \
                 compressors all take structured input, so a raw {} payload has \
                 no entry point (the CLI reports a JSON parse error and the \
                 production hook skips)",
                self.content_type
            )
        } else {
            "no tools array and no tool messages: the payload sits in system or \
             user prose, and tokenless ships no prose compressor"
                .to_string()
        };
        Applicability::NoEntryPoint { reason }
    }
}

/// Every scenario under `root`, across both suites, in a stable order.
///
/// # Errors
///
/// Returns [`L3Error::NoAssets`] when nothing was found, and propagates read
/// and parse failures with the offending path attached.
pub fn load_all(root: &Path) -> Result<Vec<Scenario>, L3Error> {
    let mut paths = Vec::new();
    for suite in [Suite::Pipeline, Suite::Scenario] {
        collect_json(&root.join(suite.dir()), &mut paths);
    }
    if paths.is_empty() {
        return Err(L3Error::NoAssets(root.display().to_string()));
    }
    paths.sort();

    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let text = std::fs::read_to_string(&path).map_err(|source| L3Error::AssetRead {
            path: path.display().to_string(),
            source,
        })?;
        let scenario: Scenario =
            serde_json::from_str(&text).map_err(|source| L3Error::AssetParse {
                path: path.display().to_string(),
                source,
            })?;
        out.push(scenario);
    }
    Ok(out)
}

/// Recursively gather `*.json` under `dir`, ignoring a missing directory so a
/// partially generated tree still runs the suites that do exist.
fn collect_json(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_json(&path, out);
        } else if path.extension().is_some_and(|e| e == "json") {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scenario_with(messages: Vec<Message>, tools: Option<Vec<Value>>) -> Scenario {
        Scenario {
            suite: Suite::Scenario,
            scenario: "t".into(),
            display_name: None,
            content_type: "text".into(),
            size_label: None,
            source: AssetSource {
                reference: "test".into(),
                headroom_native: true,
                headroom_revision: None,
                headroom_dirty: None,
            },
            headroom_target_ms: None,
            model_limit: 200_000,
            messages,
            tools,
        }
    }

    fn tool_msg(content: &str) -> Message {
        let mut m = Message::new();
        m.insert("role".into(), Value::String("tool".into()));
        m.insert("content".into(), Value::String(content.into()));
        m
    }

    fn user_msg(content: &str) -> Message {
        let mut m = Message::new();
        m.insert("role".into(), Value::String("user".into()));
        m.insert("content".into(), Value::String(content.into()));
        m
    }

    #[test]
    fn json_tool_message_is_applicable() {
        let s = scenario_with(vec![tool_msg(r#"[{"id":1}]"#)], None);
        assert!(s.tokenless_applicability().is_applicable());
        assert_eq!(s.json_tool_messages(), vec![0]);
    }

    #[test]
    fn raw_prose_tool_message_has_no_entry_point() {
        // The role says "tool", but the payload is prose — tokenless rejects it.
        // Deciding on the role alone would report a capability gap as 0%.
        let s = scenario_with(vec![tool_msg("The API supports rate limiting.")], None);
        let applicability = s.tokenless_applicability();
        assert!(!applicability.is_applicable());
        assert_eq!(s.tool_message_count(), 1);
        assert!(s.json_tool_messages().is_empty());
    }

    #[test]
    fn raw_python_tool_message_has_no_entry_point() {
        let s = scenario_with(vec![tool_msg("def foo(x):\n    return x + 1\n")], None);
        assert!(!s.tokenless_applicability().is_applicable());
    }

    #[test]
    fn tools_array_alone_is_applicable() {
        // The schema scenario carries no tool messages at all; its entry point
        // is the pre-model tools array.
        let s = scenario_with(
            vec![user_msg("find the retry policy")],
            Some(vec![serde_json::json!({"type": "function"})]),
        );
        let applicability = s.tokenless_applicability();
        assert!(applicability.is_applicable());
        match applicability {
            Applicability::Applicable { entry_points } => {
                assert!(entry_points.iter().any(|e| e.contains("compress-schema")));
            }
            Applicability::NoEntryPoint { .. } => unreachable!("asserted applicable above"),
        }
    }

    #[test]
    fn prose_only_conversation_reports_missing_prose_compressor() {
        let s = scenario_with(vec![user_msg("a long document ...")], None);
        match s.tokenless_applicability() {
            Applicability::NoEntryPoint { reason } => {
                assert!(reason.contains("prose compressor"), "got: {reason}");
            }
            Applicability::Applicable { .. } => unreachable!("prose has no entry point"),
        }
    }

    #[test]
    fn empty_tools_array_is_not_an_entry_point() {
        let s = scenario_with(vec![user_msg("hi")], Some(vec![]));
        assert!(!s.tokenless_applicability().is_applicable());
    }
}
