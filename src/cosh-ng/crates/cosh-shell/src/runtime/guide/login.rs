//! Login page privileged actions, run after the full screen is dropped so the
//! foreground sudo password prompt owns the terminal.

use crate::i18n::MessageId;
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

impl Session {
    pub(crate) fn login_choices(&self) -> Vec<LoginChoice> {
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
                return vec![
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
        ];
        if status.conflict {
            rows.push(WizardRow::Text(
                self.i18n.t(MessageId::LoginStateConflict).to_string(),
            ));
        }
        rows.push(WizardRow::Text(format!(
            "{}{}",
            self.i18n.t(MessageId::LoginTransaction),
            transaction
        )));
        rows.push(WizardRow::Text(format!(
            "{}{}",
            self.i18n.t(MessageId::LoginManaged),
            managed
        )));
        rows.push(WizardRow::Blank);
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
        }
        rows
    }

    pub(crate) fn run_login(&mut self, action: &str) {
        if action == "other" {
            self.login_guidance = true;
            self.shift(0);
            return;
        }
        if action == "reread" {
            self.reload();
            self.shift(0);
            self.message = Some(
                self.i18n
                    .t(MessageId::GuideWizardLoginRereadDone)
                    .to_string(),
            );
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
            Outcome::Verified(_) => {}
        }
        self.message = Some(match outcome {
            Outcome::Verified(status) => self.i18n.format(
                MessageId::GuideWizardLoginVerified,
                &[("value", &status.current_shell)],
            ),
            Outcome::Unconfirmed => self
                .i18n
                .t(MessageId::GuideWizardLoginUnconfirmed)
                .to_string(),
            Outcome::Error(error) => self
                .i18n
                .format(MessageId::GuideWizardSaveFailed, &[("error", &error)]),
        });
        self.login_guidance = false;
        self.reload();
        self.shift(0);
    }
}
