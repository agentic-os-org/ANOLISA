use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use wait_timeout::ChildExt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PublicHeadlessPlan {
    pub(crate) args: Vec<OsString>,
}

pub(crate) fn plan(argv0: &OsStr, args: &[OsString]) -> Option<PublicHeadlessPlan> {
    (super::is_cosh_entry(argv0) && args.first().and_then(|arg| arg.to_str()) == Some("--headless"))
        .then(|| PublicHeadlessPlan {
            args: args[1..].to_vec(),
        })
}

pub(crate) fn exec(plan: PublicHeadlessPlan) -> i32 {
    use std::os::unix::process::CommandExt;

    if plan.args.len() == 1 && plan.args[0] == "--help" {
        print!("{PUBLIC_HEADLESS_HELP}");
        return 0;
    }
    let core = std::env::current_exe()
        .ok()
        .and_then(|executable| executable.parent().map(|parent| parent.join("cosh-core")));
    let Some(core) = core else {
        eprintln!("cosh: cannot resolve the installed cosh-core sibling");
        return 126;
    };
    if !core.is_file() {
        eprintln!("cosh: installed cosh-core is missing: {}", core.display());
        return 127;
    }
    if let Err(error) = verify_core_version(&core) {
        eprintln!("cosh: {error}");
        return 126;
    }
    let error = std::process::Command::new(&core)
        .args(["--headless", "--execution-profile=public-headless-v1"])
        .args(plan.args)
        .arg0("cosh-core")
        .exec();
    eprintln!("cosh: cannot execute {}: {error}", core.display());
    if error.kind() == std::io::ErrorKind::NotFound {
        127
    } else {
        126
    }
}

fn verify_core_version(core: &Path) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut child = std::process::Command::new(core)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot inspect installed cosh-core version: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "cannot capture installed cosh-core version".to_string())?;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take(4097)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
            .map_err(|error| format!("cannot read installed cosh-core version: {error}"));
        let _ = sender.send(result);
    });
    let status = match child.wait_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(Some(status)) => status,
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err("installed cosh-core version check timed out".to_string());
        }
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "cannot wait for installed cosh-core version: {error}"
            ));
        }
    };
    let bytes = receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| "installed cosh-core version read timed out".to_string())??;
    if !status.success() {
        return Err("installed cosh-core version check failed".to_string());
    }
    if bytes.len() > 4096 {
        return Err("installed cosh-core version output is too large".to_string());
    }
    let actual = std::str::from_utf8(&bytes)
        .map_err(|_| "installed cosh-core version is not UTF-8".to_string())?
        .trim();
    let expected = concat!("cosh-core ", env!("CARGO_PKG_VERSION"));
    if actual != expected {
        return Err(format!(
            "incompatible cosh-core version: expected {expected}, found {actual}"
        ));
    }
    Ok(())
}

const PUBLIC_HEADLESS_HELP: &str = concat!(
    "Run one bounded, non-interactive Cosh Core task.\n\n",
    "Usage: cosh --headless [OPTIONS] PROMPT\n",
    "       cosh --headless [OPTIONS] -\n\n",
    "Options:\n",
    "  --output text|json       Final result format (default: text)\n",
    "  --workspace PATH         Existing workspace for authorized tools\n",
    "  --file-tools MODE        none, read, or write (default: none)\n",
    "  --exec=host              Allow model-selected commands as the calling user\n",
    "                           (not a Cosh sandbox; host permissions still apply)\n",
    "  --max-turns N            Provider generations, 1-50 (default: 12)\n",
    "  --timeout DURATION       Seconds or minutes, up to 30m\n",
    "  --help                   Show this help without contacting a provider\n",
);
