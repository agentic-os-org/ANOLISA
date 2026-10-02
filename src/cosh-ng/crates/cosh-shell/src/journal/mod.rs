use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use crate::types::ShellEvent;

pub(crate) mod audit;

#[derive(Debug)]
pub(crate) struct ShellEventJournal {
    writer: BufWriter<File>,
}

impl ShellEventJournal {
    pub(crate) fn create(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options.open(path)?;
        #[cfg(unix)]
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    pub(crate) fn append(&mut self, events: &[ShellEvent]) -> io::Result<()> {
        for event in events {
            serde_json::to_writer(&mut self.writer, &redacted_event(event)).map_err(json_to_io)?;
            self.writer.write_all(b"\n")?;
        }
        self.writer.flush()
    }
}

pub fn write_shell_events(path: impl AsRef<Path>, events: &[ShellEvent]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    let mut writer = BufWriter::new(file);
    for event in events {
        let event = redacted_event(event);
        serde_json::to_writer(&mut writer, &event).map_err(json_to_io)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()
}

pub(crate) fn redacted_shell_events(events: &[ShellEvent]) -> Vec<ShellEvent> {
    events.iter().map(redacted_event).collect()
}

pub(crate) fn redacted_event(event: &ShellEvent) -> ShellEvent {
    let mut event = event.clone();
    event.session_id = redact(&event.session_id);
    event.command_id = event.command_id.as_deref().map(redact);
    event.cwd = event.cwd.as_deref().map(redact);
    event.end_cwd = event.end_cwd.as_deref().map(redact);
    event.terminal_output_ref = event.terminal_output_ref.as_deref().map(redact);
    if event.component.as_deref() == Some("card_secret") {
        event.input = event.input.as_ref().map(|_| "<redacted>".to_string());
    } else if event
        .routing
        .as_ref()
        .is_some_and(|routing| routing.sensitive)
    {
        // The shell-side secret gate marked this input sensitive; redact the
        // whole field rather than trusting the regex patterns to re-detect
        // every form the gate matched (e.g. short keys like `sk-fbaa6`).
        event.input = event.input.as_ref().map(|_| "<redacted>".to_string());
    } else if event.component.as_deref() == Some("slash") {
        event.input = event.input.as_deref().map(redact_slash_input);
    } else {
        event.input = event.input.as_deref().map(redact);
    }
    event.command = event.command.as_deref().map(redact);
    event.component = event.component.as_deref().map(redact);
    event.message = event.message.as_deref().map(redact);
    if let Some(capture) = event.capture.as_mut() {
        capture.kind = capture.kind.as_deref().map(redact);
        capture.target_id = capture.target_id.as_deref().map(redact);
    }
    event
}

fn redact(value: &str) -> String {
    crate::evidence::redact_sensitive_text(value).0
}

fn redact_slash_input(value: &str) -> String {
    let extension_redacted = String::from_utf8(crate::raw_input::redact_extension_setting_value(
        value.as_bytes(),
    ))
    .unwrap_or_else(|_| value.to_string());
    redact(&extension_redacted)
}

pub fn read_shell_events(path: impl AsRef<Path>) -> io::Result<Vec<ShellEvent>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        events.push(serde_json::from_str(&line).map_err(json_to_io)?);
    }

    Ok(events)
}

fn json_to_io(err: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err)
}

#[cfg(test)]
mod tests {
    use super::{read_shell_events, write_shell_events};
    use crate::types::{ShellEvent, ShellRoutingMetadata};

    #[test]
    fn journal_redacts_commands_prompts_and_secret_card_input() {
        let path = std::env::temp_dir().join(format!(
            "cosh-shell-secret-journal-{}-{}.jsonl",
            std::process::id(),
            now_nanos()
        ));
        let command_secret = "cli-secret-value";
        let prompt_secret = "ghp_abcdefghijklmnopqrstuvwxyz123456";
        let auth_secret = "short-auth-value";
        let extension_secret = "extension-secret-value";
        let mut prompt = ShellEvent::user_input_intercepted(
            "session-1",
            format!("?? inspect token={prompt_secret}"),
        );
        prompt.component = Some("agent_marker".to_string());
        let mut auth =
            ShellEvent::user_input_intercepted("session-1", format!("auth-1:{auth_secret}"));
        auth.component = Some("card_secret".to_string());
        auth.message = Some("input".to_string());
        let mut extension_setting = ShellEvent::user_input_intercepted(
            "session-1",
            format!("/extensions settings set fixture endpoint {extension_secret}"),
        );
        extension_setting.component = Some("slash".to_string());
        let mut path_event = ShellEvent::command_started(
            "session-token=session-secret",
            "command-token=command-id-secret",
            "safe",
            "/tmp/token=cwd-secret",
            2,
        );
        path_event.end_cwd = Some("/tmp/token=end-cwd-secret".to_string());
        path_event.terminal_output_ref = Some("/tmp/token=output-ref-secret".to_string());

        write_shell_events(
            &path,
            &[
                ShellEvent::command_started(
                    "session-1",
                    "command-1",
                    format!("curl --token {command_secret}"),
                    "/tmp",
                    1,
                ),
                prompt,
                auth,
                extension_setting,
                path_event,
            ],
        )
        .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        for secret in [
            command_secret,
            prompt_secret,
            auth_secret,
            extension_secret,
            "session-secret",
            "command-id-secret",
            "cwd-secret",
            "end-cwd-secret",
            "output-ref-secret",
        ] {
            assert!(!content.contains(secret), "{content}");
        }

        let events = read_shell_events(&path).unwrap();
        assert_eq!(
            events[0].command.as_deref(),
            Some("curl --token <redacted>")
        );
        assert!(events[1]
            .input
            .as_deref()
            .is_some_and(|input| input.contains("token=<redacted>")));
        assert_eq!(events[2].input.as_deref(), Some("<redacted>"));
        assert_eq!(
            events[3].input.as_deref(),
            Some("/extensions settings set fixture endpoint **********************")
        );
        let _ = std::fs::remove_file(path);
    }

    /// Inputs the shell secret gate marked sensitive are redacted as a whole
    /// field, including short-key forms the regex patterns do not match
    /// (#2138: `sk-fbaa6` is below the opaque-token minimum length).
    #[test]
    fn journal_redacts_sensitive_routed_input_whole_field() {
        let path = std::env::temp_dir().join(format!(
            "cosh-shell-sensitive-routing-journal-{}-{}.jsonl",
            std::process::id(),
            now_nanos()
        ));
        let input = "帮我安装下openclaw,模型使用qwen3.8-max,API Key: sk-fbaa6";
        let mut sensitive = ShellEvent::user_input_intercepted("session-1", input);
        sensitive.component = Some("natural_language".to_string());
        sensitive.routing = Some(ShellRoutingMetadata {
            generation: 3,
            top_level_missing: true,
            proven: true,
            sensitive: true,
            unsafe_input: false,
        });

        write_shell_events(&path, &[sensitive]).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.contains("sk-fbaa6"), "{content}");
        let events = read_shell_events(&path).unwrap();
        assert_eq!(events[0].input.as_deref(), Some("<redacted>"));
        assert!(events[0].routing.as_ref().is_some_and(|r| r.sensitive));
        let _ = std::fs::remove_file(path);
    }

    /// T0.1 matrix: one representative per `_cosh_command_has_secret`
    /// pattern family. Whole-field redaction keyed off the sensitive
    /// routing flag must hide every form the shell gate matches, without
    /// relying on the regex patterns to re-detect them.
    #[test]
    fn journal_redacts_every_shell_gate_pattern_family() {
        let path = std::env::temp_dir().join(format!(
            "cosh-shell-gate-matrix-journal-{}-{}.jsonl",
            std::process::id(),
            now_nanos()
        ));
        let family_inputs = [
            "帮我配好 API Key: sk-fbaa6",             // opaque sk- short key
            "用这个 token ghp_matrixtoken 部署",      // github token prefix
            "调用时带 bearer matrix-bearer-value 头", // bearer
            "连接 https://user:matrixurlpass@db.example.com 看看", // URL password
            "跑 deploy --password matrixflagvalue 试试", // sensitive flag
            "环境里 access_key_secret=matrixassign 生效吗", // sensitive assignment
            "AK 是 LTAImatrixakvalue0 请检查",        // alibaba access key
            "迁移 AKIAMATRIXKEY0123456 这个账号",     // aws access key
        ];
        let secrets = [
            "sk-fbaa6",
            "ghp_matrixtoken",
            "matrix-bearer-value",
            "matrixurlpass",
            "matrixflagvalue",
            "matrixassign",
            "LTAImatrixakvalue0",
            "AKIAMATRIXKEY0123456",
        ];
        let events = family_inputs
            .iter()
            .map(|input| {
                let mut event = ShellEvent::user_input_intercepted("session-1", *input);
                event.component = Some("natural_language".to_string());
                event.routing = Some(ShellRoutingMetadata {
                    generation: 1,
                    top_level_missing: true,
                    proven: true,
                    sensitive: true,
                    unsafe_input: false,
                });
                event
            })
            // The sensitive + unsafe_input combination must redact the same
            // way: whole-field redaction keys off `sensitive` alone.
            .chain(std::iter::once({
                let mut event = ShellEvent::user_input_intercepted(
                    "session-1",
                    "环境里 access_key_secret=matrixassign 生效吗",
                );
                event.component = Some("natural_language".to_string());
                event.routing = Some(ShellRoutingMetadata {
                    generation: 1,
                    top_level_missing: true,
                    proven: false,
                    sensitive: true,
                    unsafe_input: true,
                });
                event
            }))
            .collect::<Vec<_>>();

        write_shell_events(&path, &events).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        for secret in secrets {
            assert!(!content.contains(secret), "{secret} leaked: {content}");
        }
        for event in read_shell_events(&path).unwrap() {
            assert_eq!(event.input.as_deref(), Some("<redacted>"));
        }
        let _ = std::fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn journal_uses_private_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!(
            "cosh-shell-private-journal-{}-{}.jsonl",
            std::process::id(),
            now_nanos()
        ));

        write_shell_events(&path, &[]).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_file(path);
    }

    fn now_nanos() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    }
}
