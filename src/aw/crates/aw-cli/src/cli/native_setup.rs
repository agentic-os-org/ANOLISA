//! Explicit startup installation creates only caller-selected, privately owned assets.

use super::{read_file, Arguments, Exit, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{self, DirBuilder, File, OpenOptions},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

struct Installation {
    directory: File,
    path: PathBuf,
    created_directory: bool,
    created: Vec<(&'static str, u64, u64)>,
    retained: bool,
}

impl Installation {
    fn open(path: &Path, python: bool) -> Result<Self> {
        if !path.is_absolute() {
            return Err("startup directory must be absolute".into());
        }
        if python {
            let environment = path
                .ancestors()
                .nth(3)
                .ok_or("missing virtual environment")?;
            if path.file_name().and_then(|name| name.to_str()) != Some("site-packages")
                || !environment.join("pyvenv.cfg").is_file()
            {
                return Err(
                    "Python setup requires an explicit dedicated venv site-packages directory"
                        .into(),
                );
            }
        }
        let created_directory = match DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(error) => return Err(error.into()),
        };
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path)?;
        let metadata = directory.metadata()?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
            return Err(
                "startup directory must be owned by the current user and not group/world writable"
                    .into(),
            );
        }
        Ok(Self {
            directory,
            path: path.canonicalize()?,
            created_directory,
            created: Vec::new(),
            retained: false,
        })
    }

    fn pinned(&self, name: &str) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.directory.as_raw_fd())).join(name)
    }

    fn write(&mut self, name: &'static str, bytes: &[u8]) -> Result<PathBuf> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.pinned(name))?;
        let metadata = file.metadata()?;
        self.created.push((name, metadata.dev(), metadata.ino()));
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(self.path.join(name))
    }

    fn verify(&self) -> Result<()> {
        let pinned = self.directory.metadata()?;
        let current = fs::symlink_metadata(&self.path)?;
        if pinned.dev() != current.dev() || pinned.ino() != current.ino() || !current.is_dir() {
            return Err("startup directory changed during installation".into());
        }
        self.directory.sync_all()?;
        Ok(())
    }
}

impl Drop for Installation {
    fn drop(&mut self) {
        if self.retained {
            return;
        }
        for (name, device, inode) in self.created.iter().rev() {
            let path = self.pinned(name);
            if fs::symlink_metadata(&path)
                .is_ok_and(|metadata| metadata.dev() == *device && metadata.ino() == *inode)
            {
                let _ = fs::remove_file(path);
            }
        }
        if self.created_directory {
            let _ = fs::remove_dir(&self.path);
        }
    }
}

fn selected_path(args: &Arguments, flag: &str) -> Result<PathBuf> {
    let path = Path::new(args.required(flag)?);
    if !path.is_absolute() {
        return Err(format!("{flag} must be absolute").into());
    }
    Ok(path.canonicalize()?)
}

pub(super) fn setup(args: &Arguments) -> Result<Exit> {
    args.check(
        &[
            "--config",
            "--agent",
            "--directory",
            "--native-settings",
            "--native-profile",
            "--native-state-dir",
        ],
        false,
    )?;
    let configuration = Path::new(args.required("--config")?).canonicalize()?;
    let bytes = read_file(&configuration, aw_config::MAX_DOCUMENT_BYTES)?;
    let config = aw_config::Validator::new()?.parse(&bytes)?;
    let target = args.required("--agent")?;
    let name = config.as_value()["spec"]["agents"][target]["adapter"]
        .as_str()
        .ok_or("configured Agent target not found")?;
    let asset = aw_adapter::startup::asset(name)?;
    let mut descriptor = json!({"binary":std::env::current_exe()?.canonicalize()?,
        "config":configuration,"agent":target});
    match name {
        "openclaw" => {
            if args.flags.contains_key("--native-profile") {
                return Err("OpenClaw setup does not accept --native-profile".into());
            }
            descriptor["settings"] = json!(selected_path(args, "--native-settings")?);
            descriptor["state_dir"] = json!(selected_path(args, "--native-state-dir")?);
        }
        "hermes" => {
            if args.flags.contains_key("--native-settings")
                || args.flags.contains_key("--native-state-dir")
            {
                return Err("Hermes setup accepts only --native-profile".into());
            }
            descriptor["profile"] = json!(selected_path(args, "--native-profile")?);
        }
        _ => {
            if args.flags.keys().any(|key| key.starts_with("--native-")) {
                return Err("QwenPaw setup retains its native profile arguments".into());
            }
        }
    }
    let mut installation =
        Installation::open(Path::new(args.required("--directory")?), asset.python)?;
    let descriptor = installation.write(
        "aw-native-bootstrap.json",
        &serde_json::to_vec(&descriptor)?,
    )?;
    let startup = installation.write(asset.filename, asset.bytes)?;
    let mut files = vec![descriptor.clone(), startup.clone()];
    let mut environment: BTreeMap<String, Value> =
        BTreeMap::from([("AW_INSTANCE_BOOTSTRAP".into(), json!(descriptor))]);
    if asset.python {
        files.push(installation.write(
            "aw-native-bootstrap.pth",
            b"import aw_native_startup; aw_native_startup.install()\n",
        )?);
    } else {
        let current = std::env::var("NODE_OPTIONS").unwrap_or_default();
        let import = serde_json::to_string(startup.to_str().ok_or("startup path must be UTF-8")?)?;
        environment.insert(
            "NODE_OPTIONS".into(),
            json!(format!("{current} --import={import}").trim()),
        );
    }
    installation.verify()?;
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(
        &mut output,
        &json!({"environment":environment,"files":files}),
    )?;
    writeln!(output)?;
    output.flush()?;
    installation.retained = true;
    Ok(Exit::Code(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn installation_preserves_existing_files_and_rolls_back_only_owned_assets() {
        let root = tempfile::tempdir().unwrap();
        let site = root.path().join("site");
        DirBuilder::new().mode(0o700).create(&site).unwrap();
        fs::write(site.join("sitecustomize.py"), "user configuration").unwrap();
        fs::write(site.join("aw-native-bootstrap.pth"), "existing startup").unwrap();
        {
            let mut installation = Installation::open(&site, false).unwrap();
            installation
                .write("aw-native-bootstrap.json", b"{}")
                .unwrap();
            installation.write("aw_native_startup.py", b"pass").unwrap();
            assert!(installation
                .write("aw-native-bootstrap.pth", b"replacement")
                .is_err());
        }
        assert_eq!(
            fs::read_to_string(site.join("sitecustomize.py")).unwrap(),
            "user configuration"
        );
        assert_eq!(
            fs::read_to_string(site.join("aw-native-bootstrap.pth")).unwrap(),
            "existing startup"
        );
        assert!(!site.join("aw-native-bootstrap.json").exists());
        assert!(!site.join("aw_native_startup.py").exists());
    }

    #[test]
    fn setup_rejects_unsafe_directory_links_and_global_python_locations() {
        let root = tempfile::tempdir().unwrap();
        let unsafe_directory = root.path().join("unsafe");
        fs::create_dir(&unsafe_directory).unwrap();
        fs::set_permissions(&unsafe_directory, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(Installation::open(&unsafe_directory, false).is_err());
        let link = root.path().join("link");
        symlink(root.path(), &link).unwrap();
        assert!(Installation::open(&link, false).is_err());
        assert!(Installation::open(&root.path().join("site-packages"), true).is_err());
        assert!(!root.path().join("site-packages").exists());
    }

    #[test]
    fn startup_writes_private_files_without_replacing_native_configuration() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("startup");
        let mut installation = Installation::open(&path, false).unwrap();
        let file = installation
            .write("aw-native-bootstrap.json", b"{}")
            .unwrap();
        assert_eq!(fs::metadata(file).unwrap().mode() & 0o777, 0o600);
        installation.verify().unwrap();
        drop(installation);
        assert!(!path.exists());
    }
}
