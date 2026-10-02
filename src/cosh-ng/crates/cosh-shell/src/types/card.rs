//! Typed input ownership and output-card identities.

use serde::{Deserialize, Serialize};

use super::AgentEvent;

/// Owner of the current editable input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InputOwner {
    NativeShell,
    AssistedShell,
    DirectExec,
    Agent,
    CoshCommand,
}

impl InputOwner {
    /// Returns the marker shown before editable input owned by this mode.
    pub(crate) const fn symbol(self) -> &'static str {
        match self {
            Self::NativeShell => "$",
            Self::AssistedShell => "◇",
            Self::DirectExec => "▶",
            Self::Agent => "◆",
            Self::CoshCommand => "/",
        }
    }
}

/// Typed editable input whose owner is fixed before submission.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct InputModel {
    owner: InputOwner,
    payload: InputPayload,
}

#[derive(Debug, Clone, PartialEq)]
enum InputPayload {
    Text(String),
    DirectExec(Vec<String>),
    CoshCommand { name: String, args: Vec<String> },
}

impl InputModel {
    pub(crate) fn text(owner: InputOwner, text: impl Into<String>) -> Option<Self> {
        if owner == InputOwner::DirectExec || owner == InputOwner::CoshCommand {
            return None;
        }
        Some(Self {
            owner,
            payload: InputPayload::Text(text.into()),
        })
    }

    /// Preserves argv boundaries for a future direct process executor.
    pub(crate) fn direct_exec(argv: Vec<String>) -> Option<Self> {
        if argv.is_empty() || argv[0].is_empty() {
            return None;
        }
        Some(Self {
            owner: InputOwner::DirectExec,
            payload: InputPayload::DirectExec(argv),
        })
    }

    pub(crate) fn cosh_command(name: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            owner: InputOwner::CoshCommand,
            payload: InputPayload::CoshCommand {
                name: name.into(),
                args,
            },
        }
    }

    pub(crate) const fn owner(&self) -> InputOwner {
        self.owner
    }

    pub(crate) fn direct_exec_argv(&self) -> Option<&[String]> {
        match &self.payload {
            InputPayload::DirectExec(argv) => Some(argv),
            _ => None,
        }
    }
}

/// Stable identity for rendered output cards.
///
/// Agent responses deliberately have no marker: their frame and title already
/// identify the producer. Event markers remain for control, tool, permission,
/// and system output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CardKind {
    AgentResponse,
    SlashCommand,
    ToolCall,
    Permission,
    System,
}

impl CardKind {
    /// Returns the stable visual marker for this card identity.
    pub(crate) const fn symbol(self) -> Option<&'static str> {
        match self {
            Self::AgentResponse => None,
            Self::SlashCommand => Some("/"),
            Self::ToolCall => Some("*"),
            Self::Permission => Some("!"),
            Self::System => Some("·"),
        }
    }

    /// Decorates a trusted card title without inspecting its text.
    pub(crate) fn title(self, title: &str) -> String {
        match self.symbol() {
            Some(symbol) => format!("{symbol} {title}"),
            None => title.to_string(),
        }
    }
}

/// A typed output-card payload whose identity is fixed at construction time.
///
/// Payload variants stay private so consumers cannot relabel arbitrary user
/// text as a permission request.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CardModel {
    kind: CardKind,
    payload: CardPayload,
}

#[derive(Debug, Clone, PartialEq)]
enum CardPayload {
    AgentResponse(String),
    SlashCommand {
        name: String,
        args: Vec<String>,
    },
    ToolCall {
        name: String,
        input: serde_json::Value,
    },
    Permission(PermissionCardRequest),
    System(String),
}

/// System-originated permission identity bound to one concrete tool request.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PermissionCardRequest {
    run_id: String,
    request_id: String,
    tool_use_id: String,
    tool_name: String,
    tool_input: serde_json::Value,
}

impl PermissionCardRequest {
    pub(crate) fn run_id(&self) -> &str {
        &self.run_id
    }

    pub(crate) fn request_id(&self) -> &str {
        &self.request_id
    }

    pub(crate) fn tool_use_id(&self) -> &str {
        &self.tool_use_id
    }

    pub(crate) fn tool_name(&self) -> &str {
        &self.tool_name
    }

    pub(crate) fn tool_input(&self) -> &serde_json::Value {
        &self.tool_input
    }
}

impl CardModel {
    /// Creates an Agent response card without a redundant output marker.
    pub(crate) fn agent_response(text: impl Into<String>) -> Self {
        Self {
            kind: CardKind::AgentResponse,
            payload: CardPayload::AgentResponse(text.into()),
        }
    }

    /// Creates a Cosh control-plane command card from structured fields.
    pub(crate) fn slash_command(name: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            kind: CardKind::SlashCommand,
            payload: CardPayload::SlashCommand {
                name: name.into(),
                args,
            },
        }
    }

    /// Creates a structured tool-call card.
    pub(crate) fn tool_call(name: impl Into<String>, input: serde_json::Value) -> Self {
        Self {
            kind: CardKind::ToolCall,
            payload: CardPayload::ToolCall {
                name: name.into(),
                input,
            },
        }
    }

    /// Creates a read-only system notice card.
    pub(crate) fn system(text: impl Into<String>) -> Self {
        Self {
            kind: CardKind::System,
            payload: CardPayload::System(text.into()),
        }
    }

    /// Builds a permission card only from the runtime's structured event.
    ///
    /// There is deliberately no constructor accepting user text or a visible
    /// `!` prefix. Request identities remain tied to the exact tool event.
    pub(crate) fn permission_from_event(event: &AgentEvent) -> Option<Self> {
        let AgentEvent::ToolPermissionRequest {
            run_id,
            request_id,
            tool_name,
            tool_input,
            tool_use_id,
            ..
        } = event
        else {
            return None;
        };
        Some(Self {
            kind: CardKind::Permission,
            payload: CardPayload::Permission(PermissionCardRequest {
                run_id: run_id.clone(),
                request_id: request_id.clone(),
                tool_use_id: tool_use_id.clone(),
                tool_name: tool_name.clone(),
                tool_input: tool_input.clone(),
            }),
        })
    }

    pub(crate) const fn kind(&self) -> CardKind {
        self.kind
    }

    pub(crate) fn permission_request(&self) -> Option<&PermissionCardRequest> {
        match &self.payload {
            CardPayload::Permission(request) => Some(request),
            _ => None,
        }
    }
}
