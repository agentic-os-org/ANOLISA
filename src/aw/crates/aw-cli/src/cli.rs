//! Native launch commands share the daemon client; Agent arguments remain literal.

mod adapter;
mod bridge;
mod dispatch;
mod hook;
mod policy;
mod readiness;
mod run;
mod update;

use std::{collections::BTreeMap, io::Read, path::Path, time::Instant};

pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Default)]
pub(crate) struct Arguments {
    pub flags: BTreeMap<String, String>,
    pub native: Vec<String>,
}

impl Arguments {
    pub fn parse_protocol(args: impl Iterator<Item = String>) -> Result<Self> {
        let mut args = args.peekable();
        let mut parsed = Self::default();
        while let Some(flag) = args.next() {
            if flag == "--" {
                parsed.native.extend(args);
                break;
            }
            if !flag.starts_with("--") {
                return Err("expected a named option (Agent arguments follow --)".into());
            }
            let value = if matches!(flag.as_str(), "--json" | "--reloadable") {
                "true".into()
            } else {
                args.next().ok_or("option requires a value")?
            };
            if parsed.flags.insert(flag, value).is_some() {
                return Err("duplicate option".into());
            }
        }
        Ok(parsed)
    }

    pub fn check(&self, allowed: &[&str], native: bool) -> Result<()> {
        if self
            .flags
            .keys()
            .any(|key| !allowed.contains(&key.as_str()))
        {
            return Err("unknown option".into());
        }
        if !native && !self.native.is_empty() {
            return Err("native arguments are only accepted by aw run".into());
        }
        Ok(())
    }

    pub fn required(&self, name: &str) -> Result<&str> {
        self.flags
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| format!("missing {name}").into())
    }
}

pub(crate) use aw_adapter::NativeExit as Exit;

pub(crate) fn finish(exit: Exit) -> ! {
    match exit {
        Exit::Code(code) => std::process::exit(code),
        Exit::Signal(signal) => {
            // SAFETY: native signal numbers are checked before construction;
            // reset our launcher handler and deliver the original status.
            unsafe {
                libc::signal(signal, libc::SIG_DFL);
                let mut set = std::mem::zeroed::<libc::sigset_t>();
                libc::sigemptyset(&mut set);
                libc::sigaddset(&mut set, signal);
                libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
                libc::raise(signal);
            }
            std::process::exit(128 + signal)
        }
    }
}

pub(crate) fn read_file(path: impl AsRef<Path>, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err("file exceeds byte limit".into());
    }
    Ok(bytes)
}

pub(crate) fn connect_service(args: &Arguments, deadline: Instant) -> Result<aw_service::Client> {
    let paths = if let Some(config) = args.flags.get("--config") {
        if args.flags.contains_key("--socket") {
            return Err("choose --config or --socket".into());
        }
        let bytes = read_file(config, aw_config::MAX_DOCUMENT_BYTES)?;
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty());
        Some(aw_service::launch_service::resolve(
            &bytes,
            runtime.as_deref().map(Path::new),
        )?)
    } else {
        None
    };
    let socket = match &paths {
        Some(paths) => paths.socket.clone(),
        None => args.required("--socket")?.into(),
    };
    let client = aw_service::Client::connect(socket, deadline)?;
    if paths
        .as_ref()
        .is_some_and(|paths| paths.config_revision != client.identity().config_revision)
    {
        return Err("service configuration does not match the supplied file".into());
    }
    Ok(client)
}

pub(crate) fn dispatch(command: &str, args: &Arguments) -> Result<Exit> {
    match command {
        "run" => run::launch(args),
        "update" => update::update(args),
        "bridge" => bridge::lifecycle(args),
        "install" => adapter::install(args),
        "hook" => Ok(hook::callback(args)),
        "policy" => {
            args.check(&[], false)?;
            policy::run()?;
            Ok(Exit::Code(0))
        }
        _ => Err("unknown launch command".into()),
    }
}
