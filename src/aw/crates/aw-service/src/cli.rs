//! Native launch commands share the daemon client; Agent arguments remain literal.

mod adapter;
mod adapters;
mod hook;
mod qoder;
mod readiness;
mod run;

use std::{collections::BTreeMap, io::Read, path::Path};

pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Default)]
pub(crate) struct Arguments {
    pub flags: BTreeMap<String, String>,
    pub native: Vec<String>,
}

impl Arguments {
    pub fn parse(args: impl Iterator<Item = String>) -> Result<Self> {
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
            let value = args.next().ok_or("option requires a value")?;
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

pub(crate) enum Exit {
    Code(i32),
    Signal(i32),
}

impl Exit {
    pub fn finish(self) -> ! {
        match self {
            Self::Code(code) => std::process::exit(code),
            Self::Signal(signal) => {
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

pub(crate) fn dispatch(command: &str, args: &Arguments) -> Result<Exit> {
    match command {
        "run" => run::launch(args),
        "install" => adapter::install(args),
        "hook" => Ok(hook::callback(args)),
        _ => Err("unknown launch command".into()),
    }
}
