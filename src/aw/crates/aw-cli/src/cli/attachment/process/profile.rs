//! Resolve peer profiles using the pinned QwenPaw directory-selection rules.

use super::{BTreeMap, OsStr, OsString, Path, PathBuf, Result};
use std::{
    ffi::{CStr, CString},
    os::unix::ffi::OsStrExt,
};

fn account_home(name: Option<&OsStr>) -> Result<PathBuf> {
    let name = name.map(|name| CString::new(name.as_bytes())).transpose()?;
    let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut buffer = vec![0u8; 65536];
    let mut found = std::ptr::null_mut();
    // Reentrant lookups keep all returned strings in this call's live buffer.
    let status = unsafe {
        if let Some(name) = &name {
            libc::getpwnam_r(
                name.as_ptr(),
                record.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut found,
            )
        } else {
            libc::getpwuid_r(
                libc::geteuid(),
                record.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut found,
            )
        }
    };
    if status != 0 || found.is_null() {
        return Err(format!(
            "cannot resolve native peer home directory: account lookup status {status}"
        )
        .into());
    }
    // A successful lookup initializes pw_dir as a NUL-terminated buffer string.
    let home = unsafe { CStr::from_ptr((*found).pw_dir) };
    Ok(PathBuf::from(OsStr::from_bytes(home.to_bytes())))
}

fn expand(path: &Path, environment: &BTreeMap<OsString, OsString>) -> Result<PathBuf> {
    let mut components = path.components();
    let Some(first) = components.find(|part| *part != std::path::Component::CurDir) else {
        return Ok(path.to_owned());
    };
    let bytes = first.as_os_str().as_bytes();
    if !bytes.starts_with(b"~") {
        return Ok(path.to_owned());
    }
    let home = if bytes == b"~" {
        match environment.get(OsStr::new("HOME")) {
            Some(home) if home.is_empty() => PathBuf::from("/"),
            Some(home) => PathBuf::from(home),
            None => account_home(None)?,
        }
    } else {
        account_home(Some(OsStr::from_bytes(&bytes[1..])))?
    };
    Ok(home.join(components.as_path()))
}

/// Resolve explicit paths or the legacy/default profile in the peer's context.
pub(super) fn qwenpaw(environment: &BTreeMap<OsString, OsString>, cwd: &Path) -> Result<PathBuf> {
    let profile = environment.get(OsStr::new("QWENPAW_WORKING_DIR"));
    let needs_home = profile.is_none_or(|profile| {
        profile.is_empty()
            || Path::new(profile)
                .components()
                .find(|part| *part != std::path::Component::CurDir)
                .is_some_and(|part| part.as_os_str() == OsStr::new("~"))
    });
    let dotenv_disabled = environment
        .get(OsStr::new("QWENPAW_RUNTIME_ID"))
        .is_some_and(|id| !id.is_empty());
    // Native dotenv preserves initially present values, but later additions are
    // invisible in procfs. Current .env files cannot prove what a live App read.
    if !dotenv_disabled
        && (profile.is_none() || (needs_home && !environment.contains_key(OsStr::new("HOME"))))
    {
        return Err("QwenPaw peer profile is unconfirmed: native .env can select its directory; set an absolute QWENPAW_WORKING_DIR at process startup or stop that peer before attaching".into());
    }
    if let Some(profile) = profile.filter(|p| !p.is_empty()) {
        return Ok(cwd.join(expand(Path::new(profile), environment)?));
    }
    let legacy = cwd.join(expand(Path::new("~/.copaw"), environment)?);
    // QwenPaw 2.2.2.post1 prefers an existing legacy directory, including symlinks.
    if legacy.try_exists()? {
        Ok(legacy)
    } else {
        Ok(cwd.join(expand(Path::new("~/.qwenpaw"), environment)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_home_and_cwd_resolve_explicit_paths_without_caller_environment() {
        let environment = BTreeMap::from([(OsString::from("HOME"), OsString::from("private"))]);
        assert_eq!(
            expand(Path::new("~/.qwenpaw"), &environment).unwrap(),
            Path::new("private/.qwenpaw")
        );
        assert_eq!(
            expand(Path::new("~/../private/.qwenpaw"), &environment).unwrap(),
            Path::new("private/../private/.qwenpaw")
        );
        let mut environment = environment;
        environment.insert(OsString::from("HOME"), OsString::new());
        assert_eq!(
            expand(Path::new("~/.qwenpaw"), &environment).unwrap(),
            Path::new("/.qwenpaw")
        );
        environment.insert(
            OsString::from("QWENPAW_WORKING_DIR"),
            OsString::from("../profile"),
        );
        assert_eq!(
            qwenpaw(&environment, Path::new("/peer/cwd")).unwrap(),
            Path::new("/peer/cwd/../profile")
        );
    }

    #[test]
    fn account_home_expansion_matches_python_for_missing_home_and_named_users() {
        let output = std::process::Command::new("python3")
            .args(["-B", "-c", "import os,pwd; from pathlib import Path; print(Path('~').expanduser()); print(pwd.getpwuid(os.getuid()).pw_name)"])
            .env_remove("HOME").output().unwrap();
        assert!(output.status.success());
        let output = String::from_utf8(output.stdout).unwrap();
        let mut lines = output.lines();
        let home = Path::new(lines.next().unwrap());
        let user = lines.next().unwrap();
        assert_eq!(
            expand(Path::new("~/.qwenpaw"), &BTreeMap::new()).unwrap(),
            home.join(".qwenpaw")
        );
        let environment = BTreeMap::from([(OsString::from("HOME"), OsString::from("/different"))]);
        assert_eq!(
            expand(Path::new(&format!("~{user}/.qwenpaw")), &environment).unwrap(),
            home.join(".qwenpaw")
        );
    }

    #[test]
    fn explicit_profile_precedes_legacy_and_tilde_aliases_match_python() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".copaw")).unwrap();
        let mut environment = BTreeMap::from([
            (OsString::from("HOME"), root.path().as_os_str().to_owned()),
            (
                OsString::from("QWENPAW_WORKING_DIR"),
                OsString::from("./~//.qwenpaw"),
            ),
        ]);
        assert_eq!(
            qwenpaw(&environment, root.path()).unwrap(),
            root.path().join(".qwenpaw")
        );
        environment.insert(OsString::from("QWENPAW_WORKING_DIR"), OsString::new());
        assert_eq!(
            qwenpaw(&environment, root.path()).unwrap(),
            root.path().join(".copaw")
        );
    }

    #[test]
    fn dotenv_requires_initial_profile_and_home_inputs_or_native_disable() {
        let cwd = Path::new("/peer");
        let mut environment =
            BTreeMap::from([(OsString::from("HOME"), OsString::from("/home/peer"))]);
        assert!(qwenpaw(&environment, cwd)
            .unwrap_err()
            .to_string()
            .contains("unconfirmed"));
        environment.insert(OsString::from("QWENPAW_RUNTIME_ID"), OsString::new());
        assert!(qwenpaw(&environment, cwd).is_err());
        environment.insert(
            OsString::from("QWENPAW_WORKING_DIR"),
            OsString::from("~/.qwenpaw"),
        );
        assert_eq!(
            qwenpaw(&environment, cwd).unwrap(),
            Path::new("/home/peer/.qwenpaw")
        );
        environment.remove(OsStr::new("HOME"));
        assert!(qwenpaw(&environment, cwd).is_err());
        environment.insert(OsString::from("QWENPAW_RUNTIME_ID"), OsString::from("0"));
        assert!(qwenpaw(&environment, cwd).is_ok());
    }

    #[test]
    fn dotenv_cannot_override_initial_absolute_relative_or_empty_values() {
        let cwd = Path::new("/peer");
        let mut environment = BTreeMap::new();
        for (value, expected) in [("/fixed", "/fixed"), ("../fixed", "/peer/../fixed")] {
            environment.insert(OsString::from("QWENPAW_WORKING_DIR"), OsString::from(value));
            assert_eq!(qwenpaw(&environment, cwd).unwrap(), Path::new(expected));
        }
        environment.insert(OsString::from("QWENPAW_WORKING_DIR"), OsString::new());
        assert!(qwenpaw(&environment, cwd).is_err());
        environment.insert(OsString::from("HOME"), OsString::new());
        assert!(qwenpaw(&environment, cwd).is_ok());
    }
}
