//! Login page privileged actions, run after the full screen is dropped so the
//! foreground sudo password prompt owns the terminal.

use crate::i18n::{I18n, MessageId};
use crate::runtime::login::guide_protocol::{self, HandoffFailure, Outcome};
use crate::runtime::login::{transaction_label, COSH_PATH};
use crate::ui::guide::WizardRow;

use super::session::{Page, Session};
use super::status::warn_failure;

/// An operation the current account state actually supports. The page rows and
/// the Enter routing both read the same list, so a label can never dispatch
/// another action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoginAction {
    Set,
    Restore,
}

impl Session {
    pub(crate) fn login_actions(&self) -> Vec<LoginAction> {
        let Ok(status) = &self.snapshot.login else {
            return Vec::new();
        };
        if status.conflict {
            Vec::new()
        } else if status.managed {
            vec![LoginAction::Restore]
        } else if status.original_bash.is_some() || status.current_shell != COSH_PATH {
            vec![LoginAction::Set]
        } else {
            // Cosh already is the login shell without a recovery record; the
            // login owner refuses both a set and a restore here.
            Vec::new()
        }
    }

    fn login_note(&self) -> Option<MessageId> {
        let Ok(status) = &self.snapshot.login else {
            return None;
        };
        if status.conflict {
            Some(MessageId::GuideWizardLoginConflictNote)
        } else if !status.managed
            && status.original_bash.is_none()
            && status.current_shell == COSH_PATH
        {
            Some(MessageId::GuideWizardLoginNoRecordNote)
        } else {
            None
        }
    }

    pub(crate) fn login_rows(&self) -> Vec<WizardRow> {
        let status = match &self.snapshot.login {
            Ok(status) => status,
            Err(error) => {
                return vec![WizardRow::Text(
                    self.i18n
                        .format(MessageId::GuideWizardLoginFailed, &[("error", error)]),
                )];
            }
        };
        let original = match &status.original_bash {
            Some(path) => format!("{}{}", self.i18n.t(MessageId::LoginOriginalBash), path),
            None => self.i18n.t(MessageId::LoginNoManagedRecord).to_string(),
        };
        let transaction = transaction_label(&status.transaction)
            .map_or(status.transaction.as_str(), |id| self.i18n.t(id));
        let mut rows = vec![
            WizardRow::Text(format!(
                "{}{} · UID {}",
                self.i18n.t(MessageId::LoginUser),
                status.username,
                status.uid
            )),
            WizardRow::Text(format!(
                "{}{}",
                self.i18n.t(MessageId::LoginCurrentShell),
                status.current_shell
            )),
            WizardRow::Text(original),
            WizardRow::Text(format!(
                "{} {}",
                self.i18n.t(MessageId::LoginTransaction),
                transaction
            )),
            WizardRow::Text(format!(
                "{} {}",
                self.i18n.t(MessageId::LoginManaged),
                self.i18n.t(if status.managed {
                    MessageId::LoginManagedYes
                } else {
                    MessageId::LoginManagedNo
                })
            )),
            WizardRow::Blank,
        ];
        for action in self.login_actions() {
            rows.push(WizardRow::Choice {
                label: self
                    .i18n
                    .t(match action {
                        LoginAction::Set => MessageId::GuideWizardLoginSet,
                        LoginAction::Restore => MessageId::GuideWizardLoginRestore,
                    })
                    .to_string(),
                value: String::new(),
            });
        }
        if let Some(note) = self.login_note() {
            rows.push(WizardRow::Text(self.i18n.t(note).to_string()));
        }
        rows.push(WizardRow::Text(
            self.i18n.t(MessageId::GuideWizardLoginOther).to_string(),
        ));
        rows.push(WizardRow::Blank);
        rows.push(WizardRow::Text(
            self.i18n
                .t(if status.uid == 0 {
                    MessageId::GuideWizardLoginRootNote
                } else {
                    MessageId::GuideWizardLoginSudoNote
                })
                .to_string(),
        ));
        rows.push(WizardRow::Text(
            self.i18n.t(MessageId::LoginNewLoginsOnly).to_string(),
        ));
        rows
    }

    pub(crate) fn run_login(&mut self, action: LoginAction) {
        let (operation, target, username, uid) = {
            let Some(status) = self.snapshot.login.as_ref().ok() else {
                warn_failure("login", "status_unavailable");
                self.message = Some(self.i18n.t(MessageId::GuideWizardReadFailed).to_string());
                return;
            };
            let (operation, target) = match action {
                LoginAction::Set => ("set", COSH_PATH.to_string()),
                LoginAction::Restore => match &status.original_bash {
                    Some(path) => ("restore", path.clone()),
                    None => {
                        self.message =
                            Some(self.i18n.t(MessageId::LoginRecoveryMissing).to_string());
                        return;
                    }
                },
            };
            (operation, target, status.username.clone(), status.uid)
        };
        let outcome = guide_protocol::elevated(operation, &username, uid, &target);
        self.finish_login(operation, &target, outcome);
    }

    pub(crate) fn finish_login(&mut self, operation: &'static str, target: &str, outcome: Outcome) {
        self.reload();
        let current = self
            .snapshot
            .login
            .as_ref()
            .ok()
            .map(|status| status.current_shell.clone());
        let outcome = settle_outcome(outcome, current.as_deref(), target);
        match &outcome {
            Outcome::Unconfirmed => tracing::warn!(
                operation,
                stage = "verify_login",
                result = "unconfirmed",
                "guide login change unconfirmed"
            ),
            Outcome::Error(_) => warn_failure(operation, "login_protocol"),
            Outcome::HandoffFailed(HandoffFailure::Spawn { kind, .. }) => tracing::warn!(
                operation,
                stage = "spawn",
                result = "failed",
                error_kind = ?kind,
                "guide login handoff failed"
            ),
            Outcome::HandoffFailed(HandoffFailure::Cancelled) => {
                warn_failure(operation, "cancelled")
            }
            Outcome::HandoffFailed(HandoffFailure::Usage) => warn_failure(operation, "usage"),
            Outcome::HandoffFailed(HandoffFailure::Exit(code)) => tracing::warn!(
                operation,
                stage = "exit",
                result = "failed",
                exit_code = code,
                "guide login handoff failed"
            ),
            Outcome::Verified(_) => {}
        }
        self.message = Some(login_message(self.i18n, outcome, current));
        // The available rows changed; re-focus the first supported action so a
        // stale cursor cannot dispatch the wrong one.
        self.enter(Page::Login);
    }
}

/// A failed handoff can still have applied the change before the structured
/// result was lost, and a verified reply can disagree with the re-read that
/// follows it. Neither case may be reported as a definite outcome.
pub(super) fn settle_outcome(outcome: Outcome, current: Option<&str>, target: &str) -> Outcome {
    match outcome {
        Outcome::HandoffFailed(_) if current == Some(target) => Outcome::Unconfirmed,
        Outcome::Verified(_) if matches!(current, Some(shell) if shell != target) => {
            Outcome::Unconfirmed
        }
        other => other,
    }
}

pub(super) fn login_message(i18n: I18n, outcome: Outcome, current: Option<String>) -> String {
    match outcome {
        Outcome::Verified(status) => {
            let shell = current.unwrap_or(status.current_shell);
            i18n.format(MessageId::GuideWizardLoginVerified, &[("value", &shell)])
        }
        Outcome::Unconfirmed => i18n.t(MessageId::GuideWizardLoginUnconfirmed).to_string(),
        Outcome::Error(error) => {
            i18n.format(MessageId::GuideWizardSaveFailed, &[("error", &error)])
        }
        Outcome::HandoffFailed(HandoffFailure::Spawn { message, .. }) => i18n.format(
            MessageId::GuideWizardLoginHandoffFailed,
            &[("error", &message)],
        ),
        Outcome::HandoffFailed(HandoffFailure::Cancelled) => {
            i18n.t(MessageId::GuideWizardLoginCancelled).to_string()
        }
        Outcome::HandoffFailed(HandoffFailure::Usage) => {
            i18n.t(MessageId::GuideWizardLoginUsageFailed).to_string()
        }
        Outcome::HandoffFailed(HandoffFailure::Exit(code)) => i18n.format(
            MessageId::GuideWizardLoginExitFailed,
            &[("code", &code.to_string())],
        ),
    }
}
