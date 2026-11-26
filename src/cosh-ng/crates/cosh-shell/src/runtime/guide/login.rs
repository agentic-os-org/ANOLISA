//! Login page privileged actions. The wizard keeps its alternate screen and only
//! lends raw mode to the foreground sudo password prompt.

use crate::i18n::MessageId;
use crate::runtime::guide::result::{ActionKind, ChangeFact, EndReason, TaskOutcome, Verification};
use crate::runtime::login::guide_protocol::{self, Outcome};
use crate::runtime::login::COSH_PATH;
use crate::ui::guide::WizardRow;

use super::session::Session;
use super::status::warn_failure;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoginChoice {
    Set,
    Restore,
    Other,
    Reread,
}

pub(crate) fn reread_message_id(
    login: &Result<crate::runtime::login::LoginStatus, String>,
) -> MessageId {
    match login {
        Ok(_) => MessageId::GuideWizardLoginRereadDone,
        Err(_) => MessageId::GuideWizardReadFailed,
    }
}

impl Session {
    pub(super) fn login_write_held(&mut self) -> bool {
        if !self.blocked_writes.contains(&ActionKind::LoginShell) {
            return false;
        }
        self.message = Some(
            self.i18n
                .t(MessageId::GuideWizardLoginCommitPending)
                .to_string(),
        );
        true
    }

    pub(crate) fn login_choices(&self) -> Vec<LoginChoice> {
        if self.blocked_writes.contains(&ActionKind::LoginShell) {
            return vec![LoginChoice::Reread];
        }
        let status = match &self.snapshot.login {
            Ok(status) => status,
            Err(_) => return vec![LoginChoice::Reread],
        };
        if status.conflict {
            return vec![LoginChoice::Reread];
        }
        if status.current_shell == COSH_PATH {
            if status.managed {
                vec![LoginChoice::Restore, LoginChoice::Other]
            } else {
                vec![LoginChoice::Reread]
            }
        } else {
            vec![LoginChoice::Set, LoginChoice::Other]
        }
    }

    fn login_choice_label(&self, choice: LoginChoice) -> String {
        self.i18n
            .t(match choice {
                LoginChoice::Set => MessageId::GuideWizardLoginSet,
                LoginChoice::Restore => MessageId::GuideWizardLoginRestore,
                LoginChoice::Other => MessageId::GuideWizardLoginOther,
                LoginChoice::Reread => MessageId::GuideWizardLoginReread,
            })
            .to_string()
    }

    pub(crate) fn login_rows(&self) -> Vec<WizardRow> {
        let status = match &self.snapshot.login {
            Ok(status) => status,
            Err(error) => {
                let mut rows = vec![
                    WizardRow::Text(
                        self.i18n
                            .format(MessageId::GuideWizardLoginFailed, &[("error", error)]),
                    ),
                    WizardRow::Blank,
                    WizardRow::Choice {
                        label: self.login_choice_label(LoginChoice::Reread),
                        value: String::new(),
                    },
                ];
                if self.blocked_writes.contains(&ActionKind::LoginShell) {
                    rows.insert(
                        1,
                        WizardRow::Text(
                            self.i18n
                                .t(MessageId::GuideWizardLoginCommitPending)
                                .to_string(),
                        ),
                    );
                }
                return rows;
            }
        };
        let original = match &status.original_bash {
            Some(path) => format!("{}{}", self.i18n.t(MessageId::LoginOriginalBash), path),
            None => self.i18n.t(MessageId::LoginNoManagedRecord).to_string(),
        };
        let transaction = match status.transaction.as_str() {
            "unmanaged" => self.i18n.t(MessageId::LoginTransactionUnmanaged),
            "prepared" => self.i18n.t(MessageId::LoginTransactionPrepared),
            "applied" => self.i18n.t(MessageId::LoginTransactionApplied),
            "restored" => self.i18n.t(MessageId::LoginTransactionRestored),
            "conflict" => self.i18n.t(MessageId::LoginTransactionConflict),
            other => other,
        };
        let managed = self.i18n.t(if status.managed {
            MessageId::LoginManagedYes
        } else {
            MessageId::LoginManagedNo
        });
        let choices = self.login_choices();
        let mut rows = vec![
            WizardRow::Text(self.i18n.t(MessageId::GuideWizardLoginBenefit).to_string()),
            WizardRow::Blank,
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
        ];
        if status.conflict {
            rows.push(WizardRow::Text(
                self.i18n.t(MessageId::LoginStateConflict).to_string(),
            ));
        }
        rows.push(WizardRow::Blank);
        if self.blocked_writes.contains(&ActionKind::LoginShell) {
            rows.push(WizardRow::Text(
                self.i18n
                    .t(MessageId::GuideWizardLoginCommitPending)
                    .to_string(),
            ));
        }
        for choice in &choices {
            rows.push(WizardRow::Choice {
                label: self.login_choice_label(*choice),
                value: String::new(),
            });
        }
        rows.push(WizardRow::Blank);
        if choices
            .iter()
            .any(|choice| matches!(choice, LoginChoice::Set | LoginChoice::Restore))
        {
            rows.push(WizardRow::Text(
                self.i18n.t(MessageId::GuideWizardLoginSudoNote).to_string(),
            ));
        }
        rows.push(WizardRow::Text(
            self.i18n.t(MessageId::LoginNewLoginsOnly).to_string(),
        ));
        rows.push(WizardRow::Blank);
        rows.push(WizardRow::Text(original));
        rows.push(WizardRow::Text(format!(
            "{}{transaction}",
            self.i18n.t(MessageId::LoginTransaction)
        )));
        rows.push(WizardRow::Text(format!(
            "{}{managed}",
            self.i18n.t(MessageId::LoginManaged)
        )));
        if self.login_guidance {
            rows.push(WizardRow::Blank);
            rows.push(WizardRow::Text(
                self.i18n
                    .t(MessageId::GuideWizardLoginOtherGuidance)
                    .to_string(),
            ));
            for action in ["status", "set", "restore"] {
                rows.push(WizardRow::Text(format!("cosh login {action} --user USER")));
            }
            rows.push(WizardRow::Text(
                self.i18n
                    .t(MessageId::GuideWizardLoginOtherNote)
                    .to_string(),
            ));
        }
        rows
    }

    /// Handoff text shown while the terminal is plain: sudo owns the password
    /// prompt, and the wizard only returns after the supervised change ends.
    pub(crate) fn login_confirmation_rows(&self, restore: bool) -> Vec<WizardRow> {
        let Ok(status) = &self.snapshot.login else {
            return Vec::new();
        };
        let target = if restore {
            status.original_bash.as_deref().unwrap_or_default()
        } else {
            COSH_PATH
        };
        let uid = status.uid.to_string();
        vec![
            WizardRow::Text(self.i18n.format(
                MessageId::GuideWizardSudoHandoffTarget,
                &[("user", &status.username), ("uid", &uid)],
            )),
            WizardRow::Text(format!(
                "{}{}",
                self.i18n.t(MessageId::LoginCurrentShell),
                status.current_shell
            )),
            WizardRow::Text(
                self.i18n
                    .format(MessageId::GuideWizardLoginTargetLine, &[("target", target)]),
            ),
            WizardRow::Blank,
            WizardRow::Text(self.i18n.t(MessageId::LoginNewLoginsOnly).to_string()),
            WizardRow::Text(
                self.i18n
                    .t(MessageId::GuideWizardLoginSudoConfirmNote)
                    .to_string(),
            ),
            WizardRow::Blank,
            WizardRow::Choice {
                label: if restore {
                    self.i18n.format(
                        MessageId::GuideWizardLoginRestoreAction,
                        &[("target", target)],
                    )
                } else {
                    self.i18n.t(MessageId::GuideWizardConfirmSet).to_string()
                },
                value: String::new(),
            },
            WizardRow::Choice {
                label: self.i18n.t(MessageId::GuideWizardCancel).to_string(),
                value: String::new(),
            },
        ]
    }

    pub(crate) fn login_handoff_lines(&self, action: &str) -> Vec<String> {
        let Ok(status) = &self.snapshot.login else {
            return Vec::new();
        };
        let target = if action == "set" {
            COSH_PATH.to_string()
        } else {
            match &status.original_bash {
                Some(path) => path.clone(),
                None => return Vec::new(),
            }
        };
        let change = if action == "set" {
            MessageId::GuideWizardSudoHandoffSet
        } else {
            MessageId::GuideWizardSudoHandoffRestore
        };
        let uid = status.uid.to_string();
        let mut lines = vec![
            self.i18n
                .t(MessageId::GuideWizardSudoHandoffTitle)
                .to_string(),
            self.i18n.format(
                MessageId::GuideWizardSudoHandoffTarget,
                &[("user", &status.username), ("uid", &uid)],
            ),
            self.i18n.format(change, &[("target", &target)]),
            String::new(),
        ];
        // Root is the one case the wizard can decide locally: sudo never asks it
        // for a password, so the password tutorial would only be noise.
        if status.uid != 0 {
            lines.push(
                self.i18n
                    .t(MessageId::GuideWizardSudoHandoffPrompt)
                    .to_string(),
            );
            lines.push(
                self.i18n
                    .t(MessageId::GuideWizardSudoHandoffEnter)
                    .to_string(),
            );
            lines.push(
                self.i18n
                    .t(MessageId::GuideWizardSudoHandoffEcho)
                    .to_string(),
            );
        }
        lines.push(
            self.i18n
                .t(MessageId::GuideWizardSudoHandoffCancel)
                .to_string(),
        );
        if status.uid != 0 {
            lines.push(
                self.i18n
                    .t(MessageId::GuideWizardSudoHandoffNoPassword)
                    .to_string(),
            );
        }
        lines.push(String::new());
        lines
    }

    pub(crate) fn run_login(&mut self, action: &str) {
        if action == "other" {
            self.login_guidance = true;
            self.message = None;
            self.shift(0);
            return;
        }
        if action == "reread" {
            self.reload();
            self.shift(0);
            self.message = Some(
                self.i18n
                    .t(reread_message_id(&self.snapshot.login))
                    .to_string(),
            );
            return;
        }
        if matches!(action, "set" | "restore") && self.login_write_held() {
            return;
        }
        let Ok(status) = &self.snapshot.login else {
            warn_failure("login", "status_unavailable");
            self.message = Some(self.i18n.t(MessageId::GuideWizardReadFailed).to_string());
            return;
        };
        let target = if action == "set" {
            COSH_PATH.to_string()
        } else {
            match &status.original_bash {
                Some(path) => path.clone(),
                None => {
                    self.message = Some(self.i18n.t(MessageId::LoginRecoveryMissing).to_string());
                    return;
                }
            }
        };
        let outcome = guide_protocol::elevated(action, &status.username, status.uid, &target);
        self.finish_login(action, outcome);
    }

    pub(crate) fn finish_login(&mut self, action: &str, outcome: Outcome) {
        let operation = match action {
            "set" => "set",
            "restore" => "restore",
            _ => "unknown",
        };
        match &outcome {
            Outcome::Unconfirmed => tracing::warn!(
                operation,
                stage = "verify_login",
                result = "unconfirmed",
                "guide login change unconfirmed"
            ),
            Outcome::Error(_) => warn_failure(operation, "login_protocol"),
            Outcome::Denied => warn_failure(operation, "privilege_denied"),
            Outcome::Verified(_) | Outcome::Cancelled => {}
        }
        let mut task = TaskOutcome::default();
        let message = match outcome {
            Outcome::Verified(status) => {
                task.record(
                    ActionKind::LoginShell,
                    ChangeFact::Committed,
                    Verification::Verified,
                );
                format!(
                    "{} {}",
                    if action == "set" {
                        self.i18n.t(MessageId::GuideWizardLoginSetDone).to_string()
                    } else {
                        self.i18n.format(
                            MessageId::GuideWizardLoginVerified,
                            &[("value", &status.current_shell)],
                        )
                    },
                    self.i18n.t(MessageId::LoginNewLoginsOnly)
                )
            }
            Outcome::Unconfirmed => {
                task.record(
                    ActionKind::LoginShell,
                    ChangeFact::Unknown,
                    Verification::Unconfirmed,
                );
                self.i18n
                    .t(MessageId::GuideWizardLoginCommitPending)
                    .to_string()
            }
            Outcome::Cancelled => {
                task.reason = EndReason::Cancelled;
                task.record(
                    ActionKind::LoginShell,
                    ChangeFact::None,
                    Verification::Verified,
                );
                self.i18n
                    .t(MessageId::GuideWizardLoginCancelled)
                    .to_string()
            }
            Outcome::Error(error) => {
                task.reason = EndReason::Failed;
                task.error = Some(error);
                task.message(self.i18n)
            }
            Outcome::Denied => {
                task.reason = EndReason::Failed;
                task.record(
                    ActionKind::LoginShell,
                    ChangeFact::None,
                    Verification::Verified,
                );
                self.i18n
                    .t(MessageId::GuideWizardLoginPrivilegeDenied)
                    .to_string()
            }
        };
        self.blocked_writes.extend(task.unknown_kinds());
        self.message = Some(message);
        self.login_guidance = false;
        self.reload();
        self.shift(0);
    }
}
