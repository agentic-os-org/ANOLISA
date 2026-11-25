//! Read-only diagnosis for supervised, read-only multi-source mounts. Only the
//! worker that loaded the sources and serves the FUSE session publishes facts.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use skillfs_core::store::{LoadError, SkillStore};
use skillfs_core::{ParseConfig, SkillEntry};
use skillfs_fuse::MountedListingObserver;
use skillfs_fuse::proc_mounts::mountinfo_id_for_target;
use skillfs_fuse::security::control_socket::resolve_peer_starttime;

use super::{DesiredState, ManagedPaths, ManagedState, instance_id_for};
use crate::{OutputFormat, escape_for_diagnostics, mount_file};

/// Set by the supervisor when spawning its foreground worker.
pub(super) const INSTANCE_ENV: &str = "SKILLFS_MANAGED_INSTANCE_ID";
const VERSION: u8 = 1;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Serialize, Deserialize)]
struct CopyLocation {
    source: PathBuf,
    path: PathBuf,
}

impl CopyLocation {
    fn new(source: &Path, path: &Path) -> Self {
        Self {
            source: source.into(),
            path: path.into(),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Skill {
    selected: Option<CopyLocation>,
    shadowed: Vec<CopyLocation>,
    effective_default: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct Selections(BTreeMap<String, Skill>);

impl mount_file::Observer for Selections {
    fn selected(&mut self, source: &Path, name: &str, entry: &SkillEntry) {
        self.0.entry(name.to_owned()).or_default().selected =
            Some(CopyLocation::new(source, &entry.source_path));
    }

    fn shadowed(&mut self, source: &Path, name: &str, entry: &SkillEntry) {
        self.record_shadow(source, name, &entry.source_path);
    }

    fn shadowed_error(&mut self, source: &Path, name: &str, error: &LoadError) {
        self.record_shadow(source, name, &error.path);
    }
}

impl Selections {
    fn record_shadow(&mut self, source: &Path, name: &str, path: &Path) {
        if let Some(skill) = self.0.get_mut(name) {
            skill.shadowed.push(CopyLocation::new(source, path));
        }
    }

    fn publish(mut self, path: &Path, mount_id: u64, listed: Vec<String>) -> Result<()> {
        for name in listed {
            if name == "skill-discover" {
                // The generated manifest takes precedence over all physical copies.
                let skill = self.0.entry(name).or_default();
                if let Some(copy) = skill.selected.take() {
                    skill.shadowed.insert(0, copy);
                }
                skill.effective_default = true;
            } else if let Some(skill) = self.0.get_mut(&name) {
                skill.effective_default = true;
            }
        }
        let pid = std::process::id();
        let snapshot = Snapshot {
            schema_version: VERSION,
            mount_namespace: mount_namespace()?,
            worker: Worker {
                pid,
                start_ticks: resolve_peer_starttime(pid).ok_or("worker start time unavailable")?,
                mount_id,
            },
            skills: self,
        };
        super::write_staged(
            path,
            &serde_json::to_string(&snapshot)?,
            &super::STATE_STAGING_SEQUENCE,
        )
    }
}

#[derive(Serialize, Deserialize)]
struct Worker {
    pid: u32,
    start_ticks: u64,
    mount_id: u64,
}

#[derive(Serialize, Deserialize)]
struct Snapshot {
    schema_version: u8,
    mount_namespace: u64,
    worker: Worker,
    skills: Selections,
}

fn mount_namespace() -> io::Result<u64> {
    Ok(fs::metadata("/proc/self/ns/mnt")?.ino())
}

// Pin the already-private state directory. INIT must never look up a path
// through the FUSE mount it is bringing online, even if paths are renamed.
fn private_directory() -> Result<fs::File> {
    let directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(super::runtime_dir())?;
    let meta = directory.metadata()?;
    // SAFETY: geteuid reads the effective uid and takes no pointers.
    if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err("managed runtime directory must be private to the calling user".into());
    }
    Ok(directory)
}

fn pinned_paths(directory: &fs::File, instance: &str) -> ManagedPaths {
    let dir = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
    ManagedPaths::in_directory(&dir, instance)
}

/// Observe the same merge used to construct the read-only store. Other mount
/// modes retain the ordinary loader and allocate no diagnostic catalogue.
pub(crate) fn load_sources(
    mountpoint: &Path,
    roots: &[PathBuf],
    config: &ParseConfig,
) -> Result<(SkillStore, Option<MountedListingObserver>)> {
    let instance = instance_id_for(mountpoint);
    if roots.len() < 2 || std::env::var(INSTANCE_ENV).as_deref() != Ok(instance.as_str()) {
        return Ok((mount_file::load_sources(roots, config)?, None));
    }
    let mut skills = Selections::default();
    let store = mount_file::load_sources_observed(roots, config, &mut skills)?;
    let directory = match private_directory() {
        Ok(directory) => directory,
        Err(error) => {
            tracing::warn!(%error, "managed runtime diagnosis unavailable");
            return Ok((store, None));
        }
    };
    let observer = Box::new(move |mount_id, listed: Vec<String>| {
        let path = pinned_paths(&directory, &instance).listing();
        if let Err(error) = skills.publish(&path, mount_id, listed) {
            tracing::warn!(%error, "could not publish managed runtime diagnosis");
        }
    });
    Ok((store, Some(observer)))
}

// Resolving a managed mount for diagnosis must not require the worker to
// answer an access check on its root. Follow a symlink alias as the caller
// would, and keep the reached directory pinned through snapshot validation.
fn open_query_root(path: &Path) -> io::Result<(PathBuf, fs::File)> {
    let fd = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY)
        .open(path.components().as_path())?;
    let resolved = fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd()))?;
    Ok((resolved, fd))
}

fn current_snapshot(mountpoint: &Path, instance: &str, root: &fs::File) -> Result<Snapshot> {
    let directory =
        private_directory().map_err(|e| format!("managed runtime directory unavailable: {e}"))?;
    let paths = pinned_paths(&directory, instance);
    let state = ManagedState::load(&paths.state)
        .map_err(|e| format!("no readable managed instance at this mountpoint: {e}"))?;
    if state.instance_id != instance
        || Path::new(&state.mountpoint) != mountpoint
        || state.desired_state != DesiredState::Mounted
    {
        return Err("no active managed instance at this mountpoint".into());
    }
    if state
        .mount_config
        .as_ref()
        .is_none_or(|cfg| cfg.sources.len() <= 1)
    {
        return Err("runtime diagnosis requires a managed multi-source configuration mount".into());
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(paths.listing())
        .map_err(|e| format!("worker has no readable runtime snapshot: {e}"))?;
    if !file.metadata()?.is_file() {
        return Err("runtime snapshot is not a regular file".into());
    }
    let snapshot: Snapshot = serde_json::from_reader(io::BufReader::new(file))?;
    let worker = &snapshot.worker;
    let pid = i32::try_from(worker.pid)?;
    let fdinfo = fs::read_to_string(format!("/proc/self/fdinfo/{}", root.as_raw_fd()))?;
    let reached_id = fdinfo
        .lines()
        .find_map(|line| line.strip_prefix("mnt_id:"))
        .and_then(|value| value.trim().parse::<u64>().ok());
    let mountinfo = fs::read("/proc/self/mountinfo")?;
    if snapshot.schema_version != VERSION
        || snapshot.mount_namespace != mount_namespace()?
        || reached_id != Some(worker.mount_id)
        || mountinfo_id_for_target(&mountinfo, mountpoint.as_os_str().as_bytes())
            != Some(worker.mount_id)
        || super::read_pid(&paths.worker_pid) != Some(pid)
        || !super::process_live(pid, Some(&state))
        || resolve_peer_starttime(worker.pid) != Some(worker.start_ticks)
    {
        return Err("cannot verify the current mount and worker generation".into());
    }
    Ok(snapshot)
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Observation {
    Available {
        worker: Worker,
        selection_status: &'static str,
        #[serde(flatten)]
        skill: Skill,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Serialize)]
struct Report<'a> {
    schema_version: u8,
    target: &'static str,
    mountpoint: &'a str,
    instance_id: &'a str,
    requested_skill: &'a str,
    #[serde(flatten)]
    observation: Observation,
}

/// Print the worker's selection for one exact directory name. An unavailable
/// generation is a failed query; an absent skill in a verified one is not.
pub(crate) fn explain(path: &Path, name: &str, format: OutputFormat) -> Result<bool> {
    let (mountpoint, root) = match open_query_root(path) {
        Ok((path, root)) => (path, Ok(root)),
        Err(err) => (
            std::path::absolute(path).unwrap_or_else(|_| path.to_owned()),
            Err(err),
        ),
    };
    let instance = instance_id_for(&mountpoint);
    let verified = root
        .map_err(Box::<dyn std::error::Error>::from)
        .and_then(|root| current_snapshot(&mountpoint, &instance, &root));
    let observation = match verified {
        Ok(mut snapshot) => {
            let found = snapshot.skills.0.remove(name);
            let selection_status = match &found {
                Some(skill) if skill.selected.is_some() => "selected",
                Some(_) => "virtual",
                None => "not_discovered",
            };
            Observation::Available {
                worker: snapshot.worker,
                selection_status,
                skill: found.unwrap_or_default(),
            }
        }
        Err(error) => Observation::Unavailable {
            reason: error.to_string(),
        },
    };
    let report = Report {
        schema_version: VERSION,
        target: "runtime",
        mountpoint: &mountpoint.to_string_lossy(),
        instance_id: &instance,
        requested_skill: name,
        observation,
    };
    let mut out = io::BufWriter::new(io::stdout().lock());
    match format {
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut out, &report)?;
            writeln!(out)?;
        }
        OutputFormat::Text => {
            writeln!(
                out,
                "Mountpoint: {}\nInstance: {}\nSkill: {}",
                escape_for_diagnostics(report.mountpoint),
                escape_for_diagnostics(&instance),
                escape_for_diagnostics(name)
            )?;
            match &report.observation {
                Observation::Unavailable { reason } => writeln!(
                    out,
                    "Status: unavailable ({})",
                    escape_for_diagnostics(reason)
                )?,
                Observation::Available {
                    worker,
                    selection_status,
                    skill,
                } => {
                    writeln!(
                        out,
                        "Worker: pid={} start_ticks={} mount_id={}",
                        worker.pid, worker.start_ticks, worker.mount_id
                    )?;
                    writeln!(out, "Selection: {selection_status}")?;
                    if let Some(copy) = &skill.selected {
                        writeln!(
                            out,
                            "Selected (first source providing this skill): {}",
                            escape_for_diagnostics(&copy.path.to_string_lossy())
                        )?;
                    }
                    for copy in &skill.shadowed {
                        writeln!(
                            out,
                            "Shadowed: {}",
                            escape_for_diagnostics(&copy.path.to_string_lossy())
                        )?;
                    }
                    writeln!(
                        out,
                        "Listed in default /skills: {}",
                        skill.effective_default
                    )?;
                }
            }
        }
    }
    out.flush()?;
    Ok(matches!(
        report.observation,
        Observation::Unavailable { .. }
    ))
}
