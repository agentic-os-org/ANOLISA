//! Default Skill scope comes from local account records, never a client's environment.

use super::StartupError;
use asc_capability_skill_sec::ManagedSkillDir;
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const SYSTEM_DIRS: [&str; 4] = [
    "/usr/share/anolisa/skills/*",
    "/usr/local/share/anolisa/skills/*",
    "/usr/share/agent-sec-core/skills/*",
    "/usr/local/share/agent-sec-core/skills/*",
];
const HOME_DIRS: [&str; 8] = [
    ".local/share/anolisa/skills/*",
    ".openclaw/skills/*",
    ".copilot-shell/skills/*",
    ".hermes/skills/**",
    ".qoder/skills/*",
    ".codex/skills/*",
    ".agents/skills/*",
    ".qwen/skills/*",
];

pub(super) fn directories(passwd: &Path) -> Result<Vec<ManagedSkillDir>, StartupError> {
    let accounts = fs::read_to_string(passwd)?;
    from_accounts(&accounts)
}

fn from_accounts(accounts: &str) -> Result<Vec<ManagedSkillDir>, StartupError> {
    let mut paths: BTreeSet<PathBuf> = SYSTEM_DIRS.into_iter().map(PathBuf::from).collect();
    let mut homes = BTreeSet::new();
    for (index, record) in accounts.lines().enumerate() {
        if record.is_empty() || record.starts_with('#') {
            continue;
        }
        let fields: Vec<_> = record.split(':').collect();
        if fields.len() != 7 || fields[0].is_empty() || fields[2].parse::<u32>().is_err() {
            tracing::warn!(line = index + 1, "Ignoring invalid local account record");
            continue;
        }
        let home = Path::new(fields[5]);
        if home == Path::new("/")
            || fields[5].contains(['*', '?', '[', ']'])
            || ManagedSkillDir::new(home).is_err()
        {
            tracing::warn!(account = fields[0], "Ignoring invalid account home");
            continue;
        }
        if !homes.insert(home.to_owned()) {
            continue;
        }
        match fs::symlink_metadata(home) {
            Ok(metadata) if !metadata.is_dir() => {
                tracing::warn!(account = fields[0], "Ignoring non-directory account home");
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::NotADirectory => {
                tracing::warn!(account = fields[0], "Ignoring non-directory account home");
                continue;
            }
            // Missing homes retain scope; access failures are reported by bounded discovery.
            _ => {}
        }
        paths.extend(HOME_DIRS.iter().map(|relative| home.join(relative)));
    }
    paths
        .into_iter()
        .map(|path| ManagedSkillDir::new(path).map_err(StartupError::from))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn account_homes_include_service_users_and_missing_homes_without_following_links() {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        let home = base.join("custom-home");
        fs::create_dir(&home).unwrap();
        let link = base.join("linked-home");
        symlink(&home, &link).unwrap();
        let absent = base.join("not-created-yet");
        let accounts = format!(
            "root:x:0:0::{}:/bin/sh\nsvc:x:42:42::{}:/sbin/nologin\nduplicate:x:1002:1002::{}:/bin/sh\nfuture:x:1003:1003::{}:/bin/sh\nlink:x:1004:1004::{}:/bin/sh\nnull:x:43:43::/dev/null:/sbin/nologin\nbad:x:44:44::relative:/bin/sh\nrootdir:x:45:45::/:/bin/sh\ninvalid\n",
            home.display(),
            home.display(),
            home.display(),
            absent.display(),
            link.display()
        );
        let paths: BTreeSet<_> = from_accounts(&accounts)
            .unwrap()
            .into_iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(paths.len(), SYSTEM_DIRS.len() + 2 * HOME_DIRS.len());
        for system in SYSTEM_DIRS {
            assert!(paths.contains(&PathBuf::from(system)));
        }
        for home in [&home, &absent] {
            for relative in HOME_DIRS {
                assert!(paths.contains(&home.join(relative)));
            }
        }
        assert!(!paths.contains(&link.join(".codex/skills/*")));
    }

    #[test]
    fn missing_account_file_is_an_error_and_old_settings_enable_defaults() {
        let temporary = tempfile::tempdir().unwrap();
        assert!(directories(&temporary.path().join("missing")).is_err());
        let settings: super::super::Settings = serde_json::from_str("{}").unwrap();
        assert!(settings.enable_default_skill_dirs);
        let settings: super::super::Settings =
            serde_json::from_str(r#"{"enableDefaultSkillDirs":false}"#).unwrap();
        assert!(!settings.enable_default_skill_dirs);
    }
}
