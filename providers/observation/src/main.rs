//! Standalone POC CLI. It has no AW control-plane client or payload mutation path.

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod engine;
mod events;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod fault;
mod identity;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod ptrace;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod symbols;

use anyhow::{ensure, Context, Result};
use std::{
    collections::BTreeMap, fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt, path::Path,
};

fn args(arguments: impl Iterator<Item = String>) -> Result<(String, BTreeMap<String, String>)> {
    let mut arguments = arguments;
    let action = arguments.next().context("expected authorize or observe")?;
    ensure!(
        matches!(action.as_str(), "authorize" | "observe"),
        "unknown_action"
    );
    let mut flags = BTreeMap::new();
    while let Some(flag) = arguments.next() {
        ensure!(
            matches!(
                flag.as_str(),
                "--pid"
                    | "--permit"
                    | "--output"
                    | "--duration"
                    | "--max-events"
                    | "--rate"
                    | "--mode"
                    | "--offsets"
            ),
            "unknown_argument"
        );
        let value = arguments.next().context("argument_requires_value")?;
        ensure!(flags.insert(flag, value).is_none(), "duplicate_argument");
    }
    Ok((action, flags))
}

fn required<'a>(flags: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str> {
    flags
        .get(name)
        .map(String::as_str)
        .with_context(|| format!("missing {name}"))
}

fn number(flags: &BTreeMap<String, String>, key: &str, default: u64, max: u64) -> Result<u64> {
    let n = flags
        .get(key)
        .map(|v| v.parse::<u64>())
        .transpose()?
        .unwrap_or(default);
    ensure!((1..=max).contains(&n), "argument_out_of_range: {key}");
    Ok(n)
}

fn run() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--help") {
        println!("aw-observation-ptrace-poc authorize --pid PID --output PRIVATE_PERMIT\naw-observation-ptrace-poc observe --permit PRIVATE_PERMIT --output NEW_JSONL [--mode advise] [--duration 10] [--max-events 4096] [--rate 10000]\nLinux x86-64 dynamic OpenSSL only. Explicit experiment authorization required. --offsets is recognized and returns unsupported_offsets. No enforce mode. Default builds refuse observation: P0 crash isolation is unresolved. The unsafe-lab feature is for disposable fixtures only.");
        return Ok(());
    }
    let (action, flags) = args(std::env::args().skip(1))?;
    ensure!(
        !flags.contains_key("--offsets"),
        "unsupported_offsets: exact dynamic OpenSSL symbols required"
    );
    if action == "authorize" {
        ensure!(
            flags
                .keys()
                .all(|k| matches!(k.as_str(), "--pid" | "--output")),
            "invalid_authorize_argument"
        );
        let pid = required(&flags, "--pid")?.parse()?;
        let process = identity::ProcessRef::read(pid)?;
        let digest = identity::hash_file(Path::new(&format!("/proc/{pid}/exe")))?;
        process.check()?;
        let permit = serde_json::json!({"schema_version":"aw.ptrace-permit-poc/v0","authorized_experiment":true,"process":process,"exe_sha256":digest});
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(required(&flags, "--output")?)?;
        writeln!(file, "{permit}")?;
        return Ok(());
    }
    ensure!(
        !flags.contains_key("--pid"),
        "observe_requires_full_permit_not_bare_pid"
    );
    ensure!(
        flags.get("--mode").is_none_or(|v| v == "advise"),
        "unsupported_mode: P1 is gated on P0 acceptance"
    );
    ensure!(
        cfg!(feature = "unsafe-lab"),
        "observation_disabled: tracer death can terminate the target; only explicit unsafe-lab builds permit disposable experiments"
    );
    let duration = number(&flags, "--duration", 10, 60)?;
    let max_events = number(&flags, "--max-events", 4096, 65536)?;
    let rate = number(&flags, "--rate", 10000, 100000)?;
    let permit = identity::Permit::load(Path::new(required(&flags, "--permit")?))?;
    let output = Path::new(required(&flags, "--output")?);
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        engine::install_handlers();
        let mut sink = events::Sink::new(output, max_events, rate)?;
        let result =
            engine::Engine::attach(permit.process).and_then(|mut e| e.run(&mut sink, duration));
        if result.is_err() {
            sink.control(serde_json::json!({"kind":"source_status","state":"unavailable","coverage":{"status":"unknown","reason":"attach_or_runtime_failed"}}))?;
        }
        result
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        let _ = (permit, output, duration, max_events, rate);
        anyhow::bail!("unsupported_architecture: requires Linux x86-64")
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("aw-observation-ptrace-poc: {error:#}");
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(text: &str) -> Result<(String, BTreeMap<String, String>)> {
        args(text.split_whitespace().map(str::to_string))
    }
    #[test]
    fn offsets_is_reachable_and_duplicates_are_rejected() {
        assert_eq!(
            parse("observe --offsets table.json").unwrap().1["--offsets"],
            "table.json"
        );
        assert!(parse("observe --pid 1 --pid 2").is_err());
        assert!(parse("observe --unknown x").is_err());
        assert!(parse("observe --permit").is_err());
    }
    #[test]
    fn numeric_budgets_are_positive_and_bounded() {
        let (_, flags) = parse("observe --duration 0").unwrap();
        assert!(number(&flags, "--duration", 10, 60).is_err());
        let (_, flags) = parse("observe --max-events 65537").unwrap();
        assert!(number(&flags, "--max-events", 4096, 65536).is_err());
    }
}
