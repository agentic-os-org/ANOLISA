//! Login page privileged actions, run after the full screen is dropped so the
//! foreground sudo password prompt owns the terminal.

use std::os::unix::process::CommandExt;

use crate::i18n::MessageId;
use crate::runtime::login::guide_protocol::{self, Outcome};
use crate::ui::guide::WizardRow;

use super::session::Session;
use super::status::warn_failure;

impl Session {
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
            WizardRow::Blank,
            WizardRow::Choice {
                label: self.i18n.t(MessageId::GuideWizardLoginSet).to_string(),
                value: String::new(),
            },
        ];
        if status.original_bash.is_some() {
            rows.push(WizardRow::Choice {
                label: self.i18n.t(MessageId::GuideWizardLoginRestore).to_string(),
                value: String::new(),
            });
        }
        rows.push(WizardRow::Choice {
            label: self.i18n.t(MessageId::GuideWizardLoginOther).to_string(),
            value: String::new(),
        });
        rows.push(WizardRow::Blank);
        rows.push(WizardRow::Text(
            self.i18n.t(MessageId::GuideWizardLoginSudoNote).to_string(),
        ));
        rows.push(WizardRow::Text(
            self.i18n.t(MessageId::LoginNewLoginsOnly).to_string(),
        ));
        rows
    }

    pub(crate) fn run_login(&mut self, action: &str) {
        if action == "other" {
            self.run_login_other(std::env::current_exe());
            return;
        }
        let Ok(status) = &self.snapshot.login else {
            warn_failure("login", "status_unavailable");
            self.message = Some(self.i18n.t(MessageId::GuideWizardReadFailed).to_string());
            return;
        };
        let target = if action == "set" {
            "/usr/bin/cosh".to_string()
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

    pub(crate) fn run_login_other(&mut self, exe: std::io::Result<std::path::PathBuf>) {
        let result = exe.map_err(|error| ("current_exe", error)).and_then(|exe| {
            std::process::Command::new(exe)
                .arg0("cosh")
                .arg("login")
                .status()
                .map_err(|error| ("spawn", error))
        });
        if let Err((stage, error)) = result {
            tracing::warn!(
                operation = "login_other", stage, result = "failed",
                error_kind = ?error.kind(), "guide login handoff failed"
            );
            self.message = Some(self.i18n.format(
                MessageId::GuideWizardLoginHandoffFailed,
                &[("error", &error.to_string())],
            ));
        }
        self.reload();
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
        self.reload();
    }
}
