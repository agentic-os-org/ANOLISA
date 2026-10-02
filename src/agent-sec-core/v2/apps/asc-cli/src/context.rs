//! Compatibility argv parsing before command dispatch; no process context store.
use asc_daemon_protocol::TraceCarrierV1;
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt as _;

pub(crate) fn extract_trace_context_input(
    argv: &mut Vec<OsString>,
) -> Result<Option<String>, clap::Error> {
    let mut value = None;
    let mut index = usize::from(!argv.is_empty());
    while index < argv.len() {
        let bytes = argv[index].as_bytes();
        if bytes == b"--" {
            break;
        }
        if bytes == b"--trace-context" {
            if index + 1 == argv.len() || argv[index + 1].as_bytes().starts_with(b"-") {
                return Err(usage("missing trace context value"));
            }
            value = Some(
                argv[index + 1]
                    .to_str()
                    .ok_or_else(|| usage("trace context must be UTF-8"))?
                    .to_owned(),
            );
            argv.drain(index..index + 2);
        } else if let Some(raw) = bytes.strip_prefix(b"--trace-context=") {
            value = Some(
                std::str::from_utf8(raw)
                    .map_err(|_| usage("trace context must be UTF-8"))?
                    .to_owned(),
            );
            argv.remove(index);
        } else {
            if !bytes.starts_with(b"-") {
                break;
            }
            index += 1;
        }
    }
    Ok(value)
}
fn native_usage(message: &'static str) -> clap::Error {
    clap::Error::raw(clap::error::ErrorKind::ValueValidation, message)
}

pub(crate) fn parse(
    native: Option<&str>,
    trace_context_input: Option<&str>,
) -> Result<asc_observability::Context, clap::Error> {
    let parent = if let Some(raw) = native {
        let carrier: TraceCarrierV1 =
            serde_json::from_str(raw).map_err(|_| native_usage("invalid OTel context envelope"))?;
        asc_observability::extract_parent(&carrier.headers())
    } else {
        asc_observability::Context::new()
    };
    if let Some(raw) = trace_context_input.filter(|s| asc_observability::normalize(s).is_some()) {
        let value = serde_json::from_str(raw).map_err(|_| usage("invalid trace context JSON"))?;
        asc_observability::bind_trace_context_input(&parent, &value).map_err(usage)
    } else {
        Ok(parent)
    }
}

fn usage(message: &'static str) -> clap::Error {
    let mut error = native_usage(message);
    error.insert(
        clap::error::ContextKind::Custom,
        clap::error::ContextValue::String("trace_context_input".into()),
    );
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trace_context_bootstrap_keeps_v1_position_last_wins_and_errors() {
        let error =
            crate::Cli::parse_from(["cli", "--trace-context", "invalid", "--help"]).unwrap_err();
        assert!(error.get(clap::error::ContextKind::Custom).is_some());
        let mut argv = [
            "cli",
            "--trace-context={\"sessionId\":\"first\"}",
            "--trace-context",
            "{\"sessionId\":\"last\"}",
            "policy",
            "list",
        ]
        .map(OsString::from)
        .to_vec();
        let raw = extract_trace_context_input(&mut argv).unwrap();
        assert_eq!(raw.as_deref(), Some("{\"sessionId\":\"last\"}"));
        assert_eq!(argv, ["cli", "policy", "list"].map(OsString::from));
        for tail in [vec!["--trace-context"], vec!["--trace-context", "--help"]] {
            let mut argv = vec![OsString::from("cli")];
            argv.extend(tail.into_iter().map(OsString::from));
            assert!(
                extract_trace_context_input(&mut argv)
                    .unwrap_err()
                    .get(clap::error::ContextKind::Custom)
                    .is_some()
            );
        }
        for args in [
            ["cli", "policy", "--trace-context=x"],
            ["cli", "--", "--trace-context=x"],
            ["cli", "socket-value", "--trace-context=x"],
        ] {
            let mut argv = args.map(OsString::from).to_vec();
            assert!(extract_trace_context_input(&mut argv).unwrap().is_none());
            assert_eq!(argv, args.map(OsString::from));
        }
    }
}
