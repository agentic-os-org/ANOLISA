// Owner: types (shell handoff contract). Handoff request model, bypass
// prefix, and untracked-status token shared across approval, shell_host,
// activity, runtime, and ui.
use serde::{Deserialize, Serialize};

pub const SHELL_HANDOFF_BYPASS_PREFIX: &str = "COSH_SHELL_HANDOFF_BYPASS=1 ";
// Scratch state stays in Cosh's reserved namespace so the transport never
// assigns to ordinary user variables before the approved command runs.
pub(crate) const BOUNDED_HANDOFF_COMMAND: &str =
    "_COSH_HANDOFF_DEBUG_TRAP=\"$(trap -p DEBUG 2>/dev/null)\"; \
     _COSH_HANDOFF_RETURN_TRAP=\"$(trap -p RETURN 2>/dev/null)\"; \
     _COSH_HANDOFF_ERR_TRAP=\"$(trap -p ERR 2>/dev/null)\"; \
     trap - DEBUG RETURN ERR 2>/dev/null; \
     _cosh_prepare_staged_handoff && eval -- \"$(<\"$COSH_HANDOFF_REQUEST_FILE\")\"; \
     _COSH_HANDOFF_STATUS=$?; \
     eval \"unset _COSH_HANDOFF_STATUS _COSH_HANDOFF_DEBUG_TRAP \
     _COSH_HANDOFF_RETURN_TRAP _COSH_HANDOFF_ERR_TRAP \
     ${_COSH_HANDOFF_RETURN_TRAP:+; ${_COSH_HANDOFF_RETURN_TRAP}} \
     ${_COSH_HANDOFF_ERR_TRAP:+; ${_COSH_HANDOFF_ERR_TRAP}} \
     ${_COSH_HANDOFF_DEBUG_TRAP:+; ${_COSH_HANDOFF_DEBUG_TRAP}}; \
     (exit ${_COSH_HANDOFF_STATUS})\"";

/// The pager environment a handoff applies when its implicit pagers are
/// disabled, in shell assignment-prefix form.
///
/// This is the single source of truth for *which* variables are neutralized:
/// the marker scripts export exactly this set around the handoff command, and
/// [`ShellHandoffRequest::handoff_pty_bytes`] carries it inline for the
/// bypass-prefixed transport form. It never becomes part of
/// [`ShellHandoffRequest::command`], so approval previews, history, OSC
/// markers, audit records and evidence keep the original command text.
///
/// `PAGER` is the generic default, `GIT_PAGER` outranks Git's own `core.pager`,
/// `MANPAGER` outranks an existing man pager, and `SYSTEMD_PAGER` outranks
/// systemd's. Tool-specific variables (`BAT_PAGER`, `DELTA_PAGER`, …) are
/// deliberately absent until a reproduction and a test justify them.
pub const NON_INTERACTIVE_PAGER_PREFIX: &str =
    "PAGER=cat GIT_PAGER=cat MANPAGER=cat SYSTEMD_PAGER=cat ";

/// Whether a handoff keeps the user's interactive pager configuration or
/// suppresses the pagers a tool would start implicitly.
///
/// Agent forensics commands (`git log`, `systemctl status`) only want text and
/// must not block on a pager, while commands the agent asked for *because* they
/// are interactive (`less`, `man`, `top`) keep the user's setup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImplicitPagerPolicy {
    /// Inherit the user's pager configuration unchanged.
    #[default]
    Inherit,
    /// Neutralize implicit pagers for the duration of this handoff.
    Disable,
}

/// Status string for a shell handoff that reached a prompt boundary without
/// ever being tracked by a preexec marker (see specs/shell-handoff-preexec-loss).
/// Cross-owner contract consumed by activity, runtime evidence delivery, and ui.
pub(crate) const SHELL_HANDOFF_UNTRACKED_STATUS: &str = "completed_untracked";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellHandoffRequest {
    pub command: String,
    pub exact_preview: String,
    pub source: String,
    pub actor: String,
    pub approval_id: String,
    pub run_id: String,
    pub request_id: Option<String>,
    pub tool_use_id: Option<String>,
    pub created_at_ms: u64,
    pub preview_hash: String,
    /// One-time claim token minted at construction (#2142). Staged next to the
    /// request file, the marker script echoes it back in the preexec marker so
    /// the OSC parser can claim the command block even when the reported
    /// command text was redacted. Requests persisted before this field existed
    /// deserialize with an empty token and keep text-based matching.
    ///
    /// Security assumptions: minted from the OS CSPRNG (uuid v4 via
    /// getrandom); influences only which command block closes which handoff,
    /// never an approval decision. Treat as immutable after construction —
    /// rewriting it after staging would orphan the sidecar the marker echoes
    /// back and mis-associate the claim.
    #[serde(default)]
    pub token: String,
    /// Implicit-pager handling for the PTY transport only. Defaults to
    /// [`ImplicitPagerPolicy::Inherit`] so previously persisted requests keep
    /// their original semantics.
    #[serde(default)]
    pub implicit_pager_policy: ImplicitPagerPolicy,
}

impl ShellHandoffRequest {
    pub fn new(
        command: impl Into<String>,
        exact_preview: impl Into<String>,
        source: impl Into<String>,
        actor: impl Into<String>,
        approval_id: impl Into<String>,
        run_id: impl Into<String>,
        created_at_ms: u64,
    ) -> Result<Self, String> {
        let exact_preview = exact_preview.into();
        let request = Self {
            command: command.into(),
            preview_hash: preview_hash(&exact_preview),
            exact_preview,
            source: source.into(),
            actor: actor.into(),
            approval_id: approval_id.into(),
            run_id: run_id.into(),
            request_id: None,
            tool_use_id: None,
            created_at_ms,
            token: uuid::Uuid::new_v4().to_string(),
            implicit_pager_policy: ImplicitPagerPolicy::default(),
        };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.command.trim().is_empty() {
            return Err("empty shell handoff command".to_string());
        }
        if self.command.contains('\0') {
            return Err("shell handoff command contains NUL byte".to_string());
        }
        if has_unsafe_line_break(&self.command) {
            return Err("shell handoff command contains an unsupported line break".to_string());
        }
        if self
            .command
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\t' | '\n'))
        {
            return Err("shell handoff command contains blocked control character".to_string());
        }
        if self.exact_preview.is_empty() {
            return Err("shell handoff preview is empty".to_string());
        }
        if self.approval_id.trim().is_empty() {
            return Err("shell handoff approval id is empty".to_string());
        }
        if self.run_id.trim().is_empty() {
            return Err("shell handoff run id is empty".to_string());
        }
        Ok(())
    }

    /// Bytes typed into the foreground PTY. These stay byte-identical to the
    /// original command: the interactive shell echoes whatever is written here,
    /// so an assignment prefix would be painted on the user's command line even
    /// though every later surface strips it. The pager policy travels
    /// out of band instead — see `shell_host::raw_relay::pty_emit`.
    pub fn pty_bytes(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let mut bytes = self.command.as_bytes().to_vec();
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// Invokes the staged command at Bash's top-level scope.
    ///
    /// The approved command remains in the owner-only sidecar until PS0 claims
    /// it before this top-level `eval` runs. Shell-state changes therefore
    /// persist without a global DEBUG trap, and plaintext is not echoed twice.
    pub(crate) fn bounded_handoff_pty_bytes(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        Ok(format!(" {BOUNDED_HANDOFF_COMMAND}\n").into_bytes())
    }

    /// Bypass-prefixed transport form, which carries the pager environment
    /// inline because it is recognized and stripped by the marker wrapper path
    /// rather than by the pending-request file.
    pub fn handoff_pty_bytes(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let mut bytes = SHELL_HANDOFF_BYPASS_PREFIX.as_bytes().to_vec();
        if self.implicit_pager_policy == ImplicitPagerPolicy::Disable {
            bytes.extend_from_slice(NON_INTERACTIVE_PAGER_PREFIX.as_bytes());
        }
        bytes.extend_from_slice(self.command.as_bytes());
        bytes.push(b'\n');
        Ok(bytes)
    }
}

// A quoted line feed continues one shell command, which covers multiline
// jq, awk, and interpreter programs carried by an approved pipeline. Bare
// line feeds could dispatch additional commands under one approval, while
// carriage returns and escaped continuations have terminal-dependent input
// semantics, so those remain fail-closed.
//
// This scans physical line-feed and carriage-return characters in the raw
// command. Textual escape sequences such as `\n` are ordinary command bytes.
fn has_unsafe_line_break(command: &str) -> bool {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Quote {
        Single,
        Double,
    }

    let mut quote = None;
    let mut saw_quoted_line_feed = false;
    let mut comment_can_start = true;
    let mut in_comment = false;
    let mut chars = command.chars();
    while let Some(ch) = chars.next() {
        if in_comment {
            if matches!(ch, '\n' | '\r') {
                return true;
            }
            continue;
        }

        match (quote, ch) {
            (_, '\r') => return true,
            (Some(Quote::Single), '\n') => saw_quoted_line_feed = true,
            (Some(Quote::Single), '\'') => quote = None,
            (Some(Quote::Single), _) => {}
            (Some(Quote::Double), '\n') => return true,
            (Some(Quote::Double), '"') => quote = None,
            (Some(Quote::Double), '\\') => {
                if chars.next().is_some_and(|next| matches!(next, '\n' | '\r')) {
                    return true;
                }
            }
            (Some(Quote::Double), _) => {}
            (None, '\n') => return true,
            (None, '\'') => {
                quote = Some(Quote::Single);
                comment_can_start = false;
            }
            (None, '"') => {
                quote = Some(Quote::Double);
                comment_can_start = false;
            }
            (None, '\\') => {
                if chars.next().is_some_and(|next| matches!(next, '\n' | '\r')) {
                    return true;
                }
                comment_can_start = false;
            }
            (None, '#') if comment_can_start => in_comment = true,
            (None, ' ' | '\t' | ';' | '|' | '&' | '(' | ')' | '<' | '>') => {
                comment_can_start = true;
            }
            (None, _) => comment_can_start = false,
        }
    }

    saw_quoted_line_feed && quote == Some(Quote::Single)
}

fn preview_hash(value: &str) -> String {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("fnv1a64:{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::{
        ImplicitPagerPolicy, ShellHandoffRequest, BOUNDED_HANDOFF_COMMAND,
        NON_INTERACTIVE_PAGER_PREFIX, SHELL_HANDOFF_BYPASS_PREFIX,
    };

    fn handoff(command: &str) -> Result<ShellHandoffRequest, String> {
        ShellHandoffRequest::new(
            command,
            format!("$ {command}"),
            "test",
            "user",
            "approval-1",
            "run-1",
            42,
        )
    }

    #[test]
    fn shell_handoff_rejects_empty_nul_unquoted_newline_and_control_chars() {
        for command in [
            "",
            "printf '\0'",
            "printf one\nprintf two",
            "printf 'one\ntwo'\nprintf three",
            "printf \"one\ntwo\"",
            "printf \"apostrophe ' one\ntwo\"",
            "printf \\'one\nprintf two",
            "printf approved # '\nprintf UNAPPROVED\n# '",
            "printf 'one\ntwo",
            "printf 'one\rtwo'",
            "printf '\u{1b}[31mred'",
        ] {
            assert!(handoff(command).is_err(), "{command:?}");
        }
    }

    #[test]
    fn shell_handoff_allows_pipeline_with_quoted_multiline_script() {
        let command = "printf 'alpha\\nbeta\\n' | awk '\n# shell-literal comment\n{ print $0 }\n'";
        let request = handoff(command).expect("quoted multiline pipeline handoff");

        assert_eq!(
            request.pty_bytes().unwrap(),
            format!("{command}\n").as_bytes()
        );
    }

    #[test]
    fn shell_handoff_allows_visible_command_and_tab_separator() {
        let request = handoff("printf\tok").expect("tab-separated command is visible input");

        assert_eq!(request.pty_bytes().unwrap(), b"printf\tok\n");
        assert_eq!(
            request.handoff_pty_bytes().unwrap(),
            format!("{SHELL_HANDOFF_BYPASS_PREFIX}printf\tok\n").as_bytes()
        );
        assert_eq!(request.preview_hash, "fnv1a64:7d74cbb1a6f6fb27");
    }

    #[test]
    fn shell_handoff_defaults_to_inheriting_the_user_pager_configuration() {
        let request = handoff("git log").expect("handoff request");

        assert_eq!(
            request.implicit_pager_policy,
            ImplicitPagerPolicy::Inherit,
            "default must not change persisted request semantics"
        );
        assert_eq!(request.pty_bytes().unwrap(), b"git log\n");
        assert_eq!(
            request.bounded_handoff_pty_bytes().unwrap(),
            format!(" {BOUNDED_HANDOFF_COMMAND}\n").as_bytes()
        );
        assert!(
            !String::from_utf8(request.bounded_handoff_pty_bytes().unwrap())
                .expect("bounded handoff bytes")
                .contains("git log")
        );
        assert_eq!(
            request.handoff_pty_bytes().unwrap(),
            format!("{SHELL_HANDOFF_BYPASS_PREFIX}git log\n").as_bytes()
        );
    }

    #[test]
    fn shell_handoff_disable_policy_keeps_the_typed_line_untouched() {
        let mut request = handoff("git log --oneline").expect("handoff request");
        request.implicit_pager_policy = ImplicitPagerPolicy::Disable;

        assert_eq!(request.command, "git log --oneline");
        assert_eq!(request.exact_preview, "$ git log --oneline");
        // The shell echoes pty_bytes verbatim, so the policy must not add
        // anything the user would see on their command line.
        assert_eq!(request.pty_bytes().unwrap(), b"git log --oneline\n");
        assert_eq!(
            request.handoff_pty_bytes().unwrap(),
            format!(
                "{SHELL_HANDOFF_BYPASS_PREFIX}{NON_INTERACTIVE_PAGER_PREFIX}git log --oneline\n"
            )
            .as_bytes()
        );
    }

    #[test]
    fn shell_handoff_pager_policy_does_not_bypass_command_validation() {
        for command in ["", "printf '\0'", "printf one\nprintf two"] {
            let Ok(mut request) = handoff("printf ok") else {
                unreachable!("baseline handoff is valid");
            };
            request.implicit_pager_policy = ImplicitPagerPolicy::Disable;
            request.command = command.to_string();

            assert!(request.pty_bytes().is_err(), "{command:?}");
            assert!(request.handoff_pty_bytes().is_err(), "{command:?}");
        }
    }

    #[test]
    fn shell_handoff_missing_pager_policy_deserializes_as_inherit() {
        let json = r#"{
            "command": "git log",
            "exact_preview": "$ git log",
            "source": "test",
            "actor": "user",
            "approval_id": "approval-1",
            "run_id": "run-1",
            "request_id": null,
            "tool_use_id": null,
            "created_at_ms": 42,
            "preview_hash": "fnv1a64:0000000000000000"
        }"#;

        let request: ShellHandoffRequest = serde_json::from_str(json).expect("legacy request");

        assert_eq!(request.implicit_pager_policy, ImplicitPagerPolicy::Inherit);
        assert!(
            request.token.is_empty(),
            "requests persisted before #2142 carry no token and keep text matching"
        );
    }

    #[test]
    fn shell_handoff_mints_a_unique_claim_token_per_request() {
        let first = handoff("git log").expect("handoff request");
        let second = handoff("git log").expect("handoff request");

        assert!(!first.token.is_empty(), "token is minted at construction");
        assert_ne!(
            first.token, second.token,
            "identical commands must not share a claim token"
        );
    }
}
