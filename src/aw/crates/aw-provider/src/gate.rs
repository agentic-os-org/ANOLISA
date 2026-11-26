//! Execution gate classification for runtime-admitted semantic Hooks.
//!
//! Sharing one explicit gate model across protocol checking, static policy
//! admission and Host dispatch keeps new Hook points on the same bounded
//! execution pipeline without allowing synthetic blocking or unverified rewrites.

/// Authority class governing candidate effects and failure actions for a Hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateKind {
    /// Allows observation and policy blocking without payload mutation.
    Decision,
    /// Allows observation and an explicit payload mutation slot, plus blocking where defined.
    Mutation(MutationSlot),
    /// Allows observation only; blocking, mutation and withholding are forbidden.
    Lifecycle,
}

/// Authorized payload slot rewritable by a [`GateKind::Mutation`] Hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationSlot {
    /// Pre-tool argument object (`replace_input`), retaining original keys.
    PreToolInput,
    /// Final post-tool projected text slot (`replace_result`), never blocking completion.
    PostToolResult,
}

/// Runtime execution contract for one implemented semantic Hook event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventGate {
    event: &'static str,
    kind: GateKind,
    has_tool_payload: bool,
    allows_native_hook: bool,
}

impl EventGate {
    /// All runtime-admitted semantic Hook gates in canonical configuration order.
    pub const ALL: &'static [Self] = &[
        Self {
            event: "session.start",
            kind: GateKind::Lifecycle,
            has_tool_payload: false,
            allows_native_hook: false,
        },
        Self {
            event: "input.submit",
            kind: GateKind::Decision,
            has_tool_payload: false,
            allows_native_hook: false,
        },
        Self {
            event: "tool.before",
            kind: GateKind::Mutation(MutationSlot::PreToolInput),
            has_tool_payload: true,
            allows_native_hook: true,
        },
        Self {
            event: "tool.after",
            kind: GateKind::Mutation(MutationSlot::PostToolResult),
            has_tool_payload: true,
            allows_native_hook: true,
        },
        Self {
            event: "turn.stop",
            kind: GateKind::Decision,
            has_tool_payload: false,
            allows_native_hook: false,
        },
        Self {
            event: "session.end",
            kind: GateKind::Lifecycle,
            has_tool_payload: false,
            allows_native_hook: false,
        },
        Self {
            event: "security.violation",
            kind: GateKind::Decision,
            has_tool_payload: true,
            allows_native_hook: false,
        },
    ];

    /// Looks up the runtime gate contract for an implemented semantic Hook name.
    pub fn for_event(event: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|gate| gate.event == event)
    }

    /// Returns the canonical semantic Hook name.
    pub const fn event(self) -> &'static str {
        self.event
    }

    /// Returns the execution gate class for this Hook.
    pub const fn kind(self) -> GateKind {
        self.kind
    }

    /// Returns the authorized rewrite slot when this Hook is a mutation gate.
    pub const fn mutation_slot(self) -> Option<MutationSlot> {
        match self.kind {
            GateKind::Mutation(slot) => Some(slot),
            GateKind::Decision | GateKind::Lifecycle => None,
        }
    }

    /// Returns whether normalized events carry `tool` metadata and admit tool selectors.
    pub const fn has_tool_payload(self) -> bool {
        self.has_tool_payload
    }

    /// Returns whether uninterpreted `native-hook/v1alpha1` steps are defined for this Hook.
    pub const fn allows_native_hook(self) -> bool {
        self.allows_native_hook
    }

    /// Returns whether a candidate Provider or boolean Hook effect is permitted by this gate.
    pub fn allows_effect(self, effect: &str) -> bool {
        matches!(
            (self.kind, effect),
            (_, "observe")
                | (
                    GateKind::Decision | GateKind::Mutation(MutationSlot::PreToolInput),
                    "block"
                )
                | (
                    GateKind::Mutation(MutationSlot::PreToolInput),
                    "replace_input"
                )
                | (
                    GateKind::Mutation(MutationSlot::PostToolResult),
                    "replace_result"
                )
        )
    }

    /// Returns whether a step `on_error` failure action is defined for this gate.
    ///
    /// Callers must still verify that the selected Adapter supports blocking or
    /// result withholding at its actual runtime entrypoint.
    pub fn allows_failure_action(self, action: &str) -> bool {
        match action {
            "report" => true,
            "block" => self.allows_effect("block"),
            "withhold_result" => self.mutation_slot() == Some(MutationSlot::PostToolResult),
            _ => false,
        }
    }

    /// Returns whether an entrypoint-scoped `AdapterCapabilities::failure_actions` entry is valid.
    pub fn allows_extra_failure_action(self, action: &str) -> bool {
        self.mutation_slot() == Some(MutationSlot::PostToolResult) && action == "withhold_result"
    }
}
