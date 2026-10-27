//! Configuration holds database then prefix read locks through publication.

use crate::{
    database::{InstalledPackage, Inventory, PackageManager, RegistrationPointer},
    require, Result,
};
use aw_exec::PackageLock;
use std::{collections::BTreeMap, path::Path};

pub(super) struct Selection {
    pub packages: BTreeMap<String, InstalledPackage>,
    _inventory: Option<Inventory>,
    _prefix_lock: Option<PackageLock>,
}

impl Selection {
    pub fn read(prefix: &Path, names: &[&str]) -> Result<Self> {
        let pointer = RegistrationPointer::read(prefix)?;
        if let Some(pointer) = &pointer {
            if pointer.package_manager != PackageManager::Aw {
                require(names.is_empty(), "template package requirements belong to another package manager; use its configuration command")?;
                return Ok(Self::unmanaged());
            }
            let paths = pointer.database_paths()?;
            let inventory = Inventory::read(&paths, Some(prefix))?;
            let lock = crate::filesystem::shared_lock(prefix)?;
            require(
                RegistrationPointer::read(prefix)?.as_ref() == Some(pointer),
                "package registration changed before configuration publication",
            )?;
            let installation = inventory
                .installations
                .first()
                .ok_or_else(|| crate::Error::Invalid("registered installation is absent".into()))?;
            let packages = installation
                .packages
                .iter()
                .filter(|package| names.contains(&package.package_id.as_str()))
                .map(|package| (package.package_id.clone(), package.clone()))
                .collect();

            Ok(Self {
                packages,

                _inventory: Some(inventory),
                _prefix_lock: Some(lock),
            })
        } else {
            Ok(Self::unmanaged())
        }
    }

    fn unmanaged() -> Self {
        Self {
            packages: BTreeMap::new(),

            _inventory: None,
            _prefix_lock: None,
        }
    }

    pub fn verify(&self, prefix: &Path) -> Result<()> {
        let mut budget = 4 * 1024 * 1024 * 1024;
        for package in self.packages.values() {
            for (name, metadata) in &package.files {
                let path = prefix.join(name);
                crate::store::filesystem::verify_payload_file(&path, metadata, &mut budget)
                    .map_err(|error| {
                        crate::Error::Invalid(format!(
                            "installed file modified or missing: {} (package {}): {error}",
                            path.display(),
                            package.package_id
                        ))
                    })?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{database::DatabasePaths, plan};
    use serde_json::json;
    use std::{fs, os::unix::fs::PermissionsExt, sync::atomic::AtomicBool};

    #[test]
    fn registered_configuration_holds_both_read_locks_through_publication() {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/configuration");
        fs::create_dir_all(&target).unwrap();
        let temporary = tempfile::tempdir_in(target.canonicalize().unwrap()).unwrap();
        let root = temporary.path();
        fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
        let paths = DatabasePaths::user(Some(root), None).unwrap();
        let prefix = root.join("installed");
        let bundle = root.join("bundle");
        fs::create_dir_all(bundle.join("payload/bin")).unwrap();
        let payload = bundle.join("payload/bin/aw");
        fs::write(&payload, b"AW fixture").unwrap();
        fs::set_permissions(&payload, fs::Permissions::from_mode(0o755)).unwrap();
        let description = json!({"schema_version":1,"package_id":"external.provider","version":"1.2.3","os":"linux","arch":std::env::consts::ARCH,"files":{"bin/aw":{"sha256":crate::digest(&payload).unwrap(),"mode":493}},"dependencies":[],"provider":{"protocol":"aw-provider/v1alpha1"}});
        fs::write(
            bundle.join("package.json"),
            serde_json::to_vec(&description).unwrap(),
        )
        .unwrap();
        let addition = plan::install(&paths, &bundle, &prefix).unwrap();
        plan::execute(&paths, &addition, &AtomicBool::new(false)).unwrap();
        let config = root.join("aw.yaml");
        let state = root.join("state");
        let settings = crate::Settings {
            prefix: &prefix,
            config: &config,
            state: &state,
            policy: crate::Policy::None,
            qoder: Some(Path::new("/bin/true")),
            node: None,
            openclaw: None,
        };
        crate::configuration::configure_using(&settings, |path, bytes| {
            let database_lock = crate::store::filesystem::safe_file(&paths.root.join("lock"))?;
            use std::os::fd::AsRawFd;
            // SAFETY: this fixture retains the owned descriptor across the nonblocking call.
            assert_ne!(unsafe { libc::flock(database_lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
            assert_eq!(std::io::Error::last_os_error().kind(), std::io::ErrorKind::WouldBlock);
            assert!(matches!(crate::filesystem::lock(&prefix), Err(crate::Error::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock));
            crate::filesystem::write_private(path, bytes)
        }).unwrap();
        assert!(config.is_file());
        let _writer = crate::filesystem::lock(&prefix).unwrap();
    }
}

#[cfg(test)]
mod authority_tests {
    use super::*;
    use crate::database::{DatabasePaths, PackageManager};
    use serde_json::json;
    use std::{fs, os::unix::fs::PermissionsExt, sync::atomic::AtomicBool};

    #[test]
    fn provider_only_records_and_foreign_product_ownership_stay_separate() {
        for manager in [PackageManager::Aw, PackageManager::Anolisa] {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path().canonicalize().unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            let paths = DatabasePaths::user_for(manager, Some(&root), None).unwrap();
            let prefix = root.join("installed");
            let bundle = root.join("bundle");
            fs::create_dir_all(bundle.join("payload/bin")).unwrap();
            let payload = bundle.join("payload/bin/provider");
            fs::write(&payload, b"Provider fixture").unwrap();
            fs::set_permissions(&payload, fs::Permissions::from_mode(0o755)).unwrap();
            let description = json!({"schema_version":1,"package_id":"test-provider","version":"1.2.3","os":"linux","arch":std::env::consts::ARCH,"files":{"bin/provider":{"sha256":crate::digest(&payload).unwrap(),"mode":493}},"dependencies":[],"provider":{"protocol":"aw-provider/v1alpha1"}});
            fs::write(
                bundle.join("package.json"),
                serde_json::to_vec(&description).unwrap(),
            )
            .unwrap();
            let addition = crate::store::plan::install(&paths, &bundle, &prefix).unwrap();
            crate::store::plan::execute(&paths, &addition, &AtomicBool::new(false)).unwrap();
            let before = fs::read(paths.installed()).unwrap();
            let config = root.join("aw.yaml");
            let state = root.join("state");
            let mut settings = crate::Settings {
                prefix: &prefix,
                config: &config,
                state: &state,
                policy: crate::Policy::None,
                qoder: Some(Path::new("/bin/true")),
                node: None,
                openclaw: None,
            };
            crate::configure(&settings).unwrap();
            fs::remove_file(&config).unwrap();
            settings.policy = crate::Policy::Template {
                definition: json!({"format":1,"packages":["test-provider"],"providers":{},"events":{}}),
                variables: BTreeMap::new(),
            };
            let result = crate::configure(&settings);
            if manager == PackageManager::Aw {
                result.unwrap();
            } else {
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains("another package manager"));
                assert!(!config.exists());
            }
            assert_eq!(fs::read(paths.installed()).unwrap(), before);
        }
    }
}
