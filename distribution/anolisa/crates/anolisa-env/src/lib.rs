//! ANOLISA environment facts detection.
//!
//! [`EnvService::detect`] returns an [`EnvFacts`] snapshot describing the
//! host OS, arch, libc, kernel, distro family, BTF / `cap_bpf` availability,
//! container runtime, and current user identity. Every probe degrades
//! gracefully: detection never panics and unknown values fall back to
//! `None` or safe defaults.

use std::path::PathBuf;
use std::process::Command;

use serde::{Deserialize, Serialize};

/// Snapshot of detected environment facts.
///
/// Optional fields are `None` when detection is unavailable on the
/// current platform (for example `libc` / `btf` on non-Linux hosts) or
/// when a probe failed without a usable fallback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvFacts {
    /// Operating system identifier (e.g. `"linux"`, `"darwin"`).
    pub os: String,
    /// CPU architecture (e.g. `"x86_64"`, `"aarch64"`).
    pub arch: String,
    /// libc flavor on Linux (`"glibc"` / `"musl"`); `None` elsewhere.
    pub libc: Option<String>,
    /// Kernel release as reported by `uname -r`.
    pub kernel: Option<String>,
    /// Package base family derived from `/etc/os-release`
    /// (e.g. `"anolis23"`, `"anolis8"`).
    ///
    /// Kept as an Anolis-specific identifier for backward compatibility
    /// with existing callers; broader rpm/deb inference is available via
    /// [`pkg_base_from_id`].
    pub pkg_base: Option<String>,
    /// Raw `ID` field from `/etc/os-release` (e.g. `"alinux"`,
    /// `"ubuntu"`, `"debian"`, `"anolis"`). `None` on non-Linux hosts
    /// or when `/etc/os-release` is missing / unreadable.
    pub os_id: Option<String>,
    /// Raw `ID_LIKE` field from `/etc/os-release` (e.g. `"debian"`,
    /// `"rhel fedora"`). `None` when unavailable. Defaulted on
    /// deserialization so snapshots written before this field existed
    /// keep loading.
    #[serde(default)]
    pub os_id_like: Option<String>,
    /// Raw `VERSION_ID` field from `/etc/os-release` (e.g. `"4"`,
    /// `"24.04"`, `"12"`). `None` when unavailable.
    pub os_version: Option<String>,
    /// Whether `/sys/kernel/btf/vmlinux` exists.
    pub btf: Option<bool>,
    /// Best-effort `CAP_BPF` availability from Linux effective capabilities.
    /// `None` when the capability set cannot be read or parsed.
    pub cap_bpf: Option<bool>,
    /// Container runtime hint. Set from marker files first
    /// (`/.dockerenv` -> `"docker"`, `/run/.containerenv` -> `"podman"`)
    /// and then by scanning `/proc/1/cgroup` and `/proc/self/cgroup` for
    /// known cgroup paths. Possible values today include `"docker"`,
    /// `"podman"`, `"libpod"`, `"containerd"`, `"kubepods"`, and the
    /// generic `"container"` fallback when the cgroup line looks
    /// containerized but the flavor is unknown. `None` when no probe
    /// found a match.
    pub container: Option<String>,
    /// User-facing login name (from `$USER` / `$LOGNAME`, or passwd).
    pub user: String,
    /// Effective uid of the running process.
    pub uid: u32,
    /// Home directory as resolved by [`dirs::home_dir`].
    pub home: PathBuf,
}

/// Parsed view of `/etc/os-release` relevant to environment detection.
///
/// Returned by [`parse_os_release`]; fields are independently `Option`
/// so callers can use whichever pieces are present.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OsReleaseInfo {
    /// Raw `ID` field (e.g. `"alinux"`, `"ubuntu"`, `"anolis"`).
    pub id: Option<String>,
    /// Raw `ID_LIKE` field (e.g. `"debian"`, `"rhel fedora"`), used to
    /// classify derivatives whose `ID` is not individually known.
    pub id_like: Option<String>,
    /// Raw `VERSION_ID` field (e.g. `"4"`, `"24.04"`, `"23"`).
    pub version_id: Option<String>,
    /// Anolis-family package base hint (`"anolis23"`, `"anolis8"`,
    /// `"anolis"`); `None` for distros outside that family. Kept for
    /// backward compatibility with [`EnvFacts::pkg_base`].
    pub pkg_base: Option<String>,
}

/// Stateless façade exposing environment detection entry points.
pub struct EnvService;

impl EnvService {
    /// Detect facts for the current host. Never fails.
    pub fn detect() -> EnvFacts {
        Self::detect_for(std::env::consts::OS)
    }

    /// Detection variant that pretends the target OS is `target_os`.
    ///
    /// Useful for tests that want to assert non-Linux fallback behavior
    /// without running on a non-Linux host. Probes that consult the live
    /// filesystem (`/proc`, `/sys`, `/etc/os-release`, etc.) still read
    /// from the real machine.
    pub fn detect_for(target_os: &str) -> EnvFacts {
        let arch = std::env::consts::ARCH.to_string();
        let libc = detect_libc(target_os);
        let kernel = detect_kernel();
        let os_release = detect_os_release(target_os);
        let btf = detect_btf(target_os);
        let cap_bpf = detect_cap_bpf(target_os);
        let container = detect_container();
        let (user, uid) = detect_user_uid();
        let home = detect_home();
        EnvFacts {
            os: target_os.to_string(),
            arch,
            libc,
            kernel,
            pkg_base: os_release.pkg_base,
            os_id: os_release.id,
            os_id_like: os_release.id_like,
            os_version: os_release.version_id,
            btf,
            cap_bpf,
            container,
            user,
            uid,
            home,
        }
    }
}

fn detect_libc(target_os: &str) -> Option<String> {
    if target_os != "linux" {
        return None;
    }
    // TODO(owner: env-detection, when: external command probes are centralized):
    // Run `ldd --version` and `uname -r` through a Rust-side timeout so
    // `EnvService::detect` cannot hang behind a stuck PATH entry or probe binary.
    if let Ok(out) = Command::new("ldd").arg("--version").output() {
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        )
        .to_lowercase();
        if combined.contains("musl") {
            return Some("musl".to_string());
        }
        if combined.contains("glibc") || combined.contains("gnu libc") {
            return Some("glibc".to_string());
        }
    }
    // Default assumption on Linux when probes are inconclusive.
    Some("glibc".to_string())
}

fn detect_kernel() -> Option<String> {
    let out = Command::new("uname").arg("-r").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// Parse an `os-release(5)` document into an [`OsReleaseInfo`].
///
/// `id` / `version_id` mirror the corresponding `/etc/os-release`
/// fields verbatim (after unquoting). `pkg_base` is the legacy
/// Anolis-family hint — `"anolis{major}"` when the distro is (or
/// claims compatibility with) Anolis OS, otherwise `None`.
///
/// Callers that need a broader rpm/deb classification can pass
/// `id` to [`pkg_base_from_id`].
pub fn parse_os_release(content: &str) -> OsReleaseInfo {
    let mut id: Option<String> = None;
    let mut id_like: Option<String> = None;
    let mut version_id: Option<String> = None;

    for raw in content.lines() {
        let line = raw.trim();
        if let Some(v) = line.strip_prefix("ID=") {
            id = Some(unquote(v));
        } else if let Some(v) = line.strip_prefix("ID_LIKE=") {
            id_like = Some(unquote(v));
        } else if let Some(v) = line.strip_prefix("VERSION_ID=") {
            version_id = Some(unquote(v));
        }
    }

    let is_anolis = id
        .as_deref()
        .map(|s| matches!(s, "anolis" | "openanolis"))
        .unwrap_or(false)
        || id_like
            .as_deref()
            .map(|s| {
                s.split_whitespace()
                    .any(|w| matches!(w, "anolis" | "openanolis"))
            })
            .unwrap_or(false);

    let pkg_base = if is_anolis {
        let major = version_id.as_deref().and_then(version_major);
        Some(match major {
            Some(m) => format!("anolis{m}"),
            None => "anolis".to_string(),
        })
    } else {
        None
    };

    OsReleaseInfo {
        id,
        id_like,
        version_id,
        pkg_base,
    }
}

/// Map a `/etc/os-release` `ID` value to a coarse package-base family
/// (`"rpm"` / `"deb"`). Returns `None` for distros outside the known
/// rpm- / deb-based families.
///
/// This is an opt-in helper; [`EnvFacts::pkg_base`] keeps its legacy
/// Anolis-specific semantics for backward compatibility.
pub fn pkg_base_from_id(id: &str) -> Option<String> {
    match id {
        "alinux" | "anolis" | "openanolis" | "rhel" | "centos" | "fedora" | "rocky"
        | "almalinux" | "ol" => Some("rpm".to_string()),
        "ubuntu" | "debian" | "linuxmint" | "pop" | "elementary" | "raspbian" => {
            Some("deb".to_string())
        }
        _ => None,
    }
}

/// Classify the host package family (`"rpm"` / `"deb"`) from the raw
/// `/etc/os-release` `ID` and `ID_LIKE` fields.
///
/// `ID` wins when it is individually known; otherwise every whitespace
/// token of `ID_LIKE` is tried in order, so derivatives that declare
/// their lineage (e.g. `ID=pop`, `ID_LIKE="ubuntu debian"`) classify
/// without being listed by name. Returns `None` when neither field
/// names a known rpm/deb family member — callers must treat that as
/// "unknown", not as a third family.
pub fn package_family(id: Option<&str>, id_like: Option<&str>) -> Option<String> {
    if let Some(family) = id.and_then(pkg_base_from_id) {
        return Some(family);
    }
    id_like?.split_whitespace().find_map(pkg_base_from_id)
}

fn version_major(version_id: &str) -> Option<String> {
    let head = version_id.split('.').next()?.trim();
    let digits: String = head.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        Some(digits)
    }
}

fn unquote(value: &str) -> String {
    value
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .to_string()
}

fn detect_os_release(target_os: &str) -> OsReleaseInfo {
    if target_os != "linux" {
        return OsReleaseInfo::default();
    }
    match std::fs::read_to_string("/etc/os-release") {
        Ok(content) => parse_os_release(&content),
        Err(_) => OsReleaseInfo::default(),
    }
}

fn detect_btf(target_os: &str) -> Option<bool> {
    if target_os != "linux" {
        return None;
    }
    Some(std::path::Path::new("/sys/kernel/btf/vmlinux").exists())
}

fn detect_cap_bpf(target_os: &str) -> Option<bool> {
    if target_os != "linux" {
        return None;
    }
    detect_cap_bpf_from_status_file(std::path::Path::new("/proc/self/status"))
}

fn detect_cap_bpf_from_status_file(path: &std::path::Path) -> Option<bool> {
    let content = std::fs::read_to_string(path).ok()?;
    parse_cap_bpf_from_proc_status(&content)
}

fn parse_cap_bpf_from_proc_status(content: &str) -> Option<bool> {
    const CAP_BPF_BIT: u32 = 39;

    let cap_eff = content.lines().find_map(|raw| {
        let line = raw.trim();
        line.strip_prefix("CapEff:")
            .map(|value| value.trim().trim_start_matches("0x"))
    })?;

    let bits = u64::from_str_radix(cap_eff, 16).ok()?;
    Some((bits & (1u64 << CAP_BPF_BIT)) != 0)
}

/// Detect the container runtime flavor the current process runs in.
///
/// Probes marker files first (`/.dockerenv` -> `"docker"`,
/// `/run/.containerenv` -> `"podman"`), then scans `/proc/1/cgroup`
/// and `/proc/self/cgroup` for known container-instance paths. Returns
/// `None` on a bare-metal host or a platform without cgroups.
///
/// Matching deliberately looks for *container instance* paths rather
/// than substring-anywhere so a host that merely runs the containerd
/// daemon is not misclassified — see `parse_cgroup` for the exact
/// rules.
pub fn detect_container() -> Option<String> {
    if std::path::Path::new("/.dockerenv").exists() {
        return Some("docker".to_string());
    }
    if std::path::Path::new("/run/.containerenv").exists() {
        return Some("podman".to_string());
    }
    if std::env::consts::OS == "linux" {
        // Try pid 1's cgroup first (most accurate for "are we in a
        // container?"), then fall back to the current process. Probe
        // failure must never become a hard error.
        for candidate in ["/proc/1/cgroup", "/proc/self/cgroup"] {
            if let Ok(content) = std::fs::read_to_string(candidate)
                && let Some(flavor) = parse_cgroup(&content)
            {
                return Some(flavor);
            }
        }
    }
    None
}

/// Inspect a `/proc/<pid>/cgroup` payload and return a container
/// flavor hint when a known marker is present. Both cgroup v1 (multiple
/// lines, one per controller) and cgroup v2 (single `0::` line) are
/// covered.
///
/// Matching deliberately looks for *container instance* paths rather
/// than substring-anywhere — a host that merely runs the containerd
/// daemon will have lines like `0::/system.slice/containerd.service`
/// in its own cgroup, and treating that as "we are containerised"
/// produces false positives.
fn parse_cgroup(content: &str) -> Option<String> {
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        // Be strict about order: more specific markers first
        // (libpod before podman, kubepods before bare containerd) so
        // the most informative label wins on overlapping paths.
        if line.contains("libpod-") || line.contains("/libpod/") {
            return Some("libpod".to_string());
        }
        if line.contains("/docker/") || line.contains("docker-") {
            return Some("docker".to_string());
        }
        if line.contains("kubepods") {
            return Some("kubepods".to_string());
        }
        if line.contains("podman-") || line.contains("/podman/") {
            return Some("podman".to_string());
        }
        // Only match container-instance containerd paths
        // (`cri-containerd-<id>.scope`, `/containerd/<id>/`). Bare
        // `containerd.service` on the host is the daemon itself, not
        // a container, so it must NOT match.
        if line.contains("cri-containerd-") || line.contains("/containerd/") {
            return Some("containerd".to_string());
        }
        if line.contains("/lxc/") || line.contains(":lxc:") {
            return Some("lxc".to_string());
        }
    }
    None
}

fn detect_user_uid() -> (String, u32) {
    let uid = nix::unistd::Uid::effective().as_raw();
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .ok()
        .or_else(|| {
            nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
                .ok()
                .flatten()
                .map(|u| u.name)
        })
        .unwrap_or_else(|| "unknown".to_string());
    (user, uid)
}

fn detect_home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_non_empty_os_and_arch() {
        let facts = EnvService::detect();
        assert!(!facts.os.is_empty(), "os should not be empty");
        assert!(!facts.arch.is_empty(), "arch should not be empty");
    }

    #[test]
    fn detect_for_non_linux_skips_libc_and_btf() {
        let facts = EnvService::detect_for("macos");
        assert_eq!(facts.os, "macos");
        assert!(facts.libc.is_none(), "libc should be None off-Linux");
        assert!(facts.btf.is_none(), "btf should be None off-Linux");
        assert!(facts.cap_bpf.is_none(), "cap_bpf should be None off-Linux");
        assert!(facts.os_id.is_none(), "os_id should be None off-Linux");
        assert!(
            facts.os_version.is_none(),
            "os_version should be None off-Linux"
        );
        assert!(
            facts.pkg_base.is_none(),
            "pkg_base should be None off-Linux"
        );
    }

    #[test]
    fn parse_os_release_anolis_23() {
        let content = "NAME=\"Anolis OS\"\n\
                       VERSION=\"23.0\"\n\
                       ID=\"anolis\"\n\
                       VERSION_ID=\"23\"\n";
        let info = parse_os_release(content);
        assert_eq!(info.id.as_deref(), Some("anolis"));
        assert_eq!(info.version_id.as_deref(), Some("23"));
        assert_eq!(info.pkg_base.as_deref(), Some("anolis23"));
    }

    #[test]
    fn parse_os_release_id_like_matches() {
        let content = "NAME=\"Custom Distro\"\n\
                       ID=customdistro\n\
                       ID_LIKE=\"anolis rhel\"\n\
                       VERSION_ID=\"8.6\"\n";
        let info = parse_os_release(content);
        assert_eq!(info.id.as_deref(), Some("customdistro"));
        assert_eq!(info.version_id.as_deref(), Some("8.6"));
        assert_eq!(info.pkg_base.as_deref(), Some("anolis8"));
    }

    #[test]
    fn parse_os_release_unknown_distro_returns_no_pkg_base() {
        let content = "NAME=\"Ubuntu\"\nID=ubuntu\nVERSION_ID=\"22.04\"\n";
        let info = parse_os_release(content);
        assert_eq!(info.id.as_deref(), Some("ubuntu"));
        assert_eq!(info.version_id.as_deref(), Some("22.04"));
        assert!(info.pkg_base.is_none());
    }

    #[test]
    fn parse_os_release_alinux_keeps_id_and_version() {
        let content = "NAME=\"Alibaba Cloud Linux\"\n\
                       ID=\"alinux\"\n\
                       VERSION_ID=\"4\"\n";
        let info = parse_os_release(content);
        assert_eq!(info.id.as_deref(), Some("alinux"));
        assert_eq!(info.version_id.as_deref(), Some("4"));
        // pkg_base stays Anolis-specific for backward compatibility.
        assert!(info.pkg_base.is_none());
    }

    #[test]
    fn parse_os_release_missing_version_falls_back_to_anolis() {
        let content = "ID=anolis\n";
        let info = parse_os_release(content);
        assert_eq!(info.pkg_base.as_deref(), Some("anolis"));
        assert!(info.version_id.is_none());
    }

    #[test]
    fn parse_os_release_uses_numeric_major_prefix() {
        let content = "ID=anolis\nVERSION_ID=\"23alpha\"\n";
        let info = parse_os_release(content);
        assert_eq!(info.pkg_base.as_deref(), Some("anolis23"));
        assert_eq!(info.version_id.as_deref(), Some("23alpha"));
    }

    #[test]
    fn parse_os_release_empty_input_returns_default() {
        let info = parse_os_release("");
        assert_eq!(info, OsReleaseInfo::default());
    }

    #[test]
    fn pkg_base_from_id_recognizes_rpm_family() {
        for id in ["alinux", "anolis", "openanolis", "rhel", "centos", "fedora"] {
            assert_eq!(
                pkg_base_from_id(id).as_deref(),
                Some("rpm"),
                "id `{id}` should map to rpm",
            );
        }
    }

    #[test]
    fn pkg_base_from_id_recognizes_deb_family() {
        for id in ["ubuntu", "debian", "linuxmint", "pop", "elementary"] {
            assert_eq!(
                pkg_base_from_id(id).as_deref(),
                Some("deb"),
                "id `{id}` should map to deb",
            );
        }
    }

    #[test]
    fn pkg_base_from_id_unknown_returns_none() {
        assert!(pkg_base_from_id("arch").is_none());
        assert!(pkg_base_from_id("gentoo").is_none());
        assert!(pkg_base_from_id("").is_none());
    }

    #[test]
    fn parse_os_release_keeps_id_like() {
        let content = "ID=pop\nID_LIKE=\"ubuntu debian\"\n";
        let info = parse_os_release(content);
        assert_eq!(info.id.as_deref(), Some("pop"));
        assert_eq!(info.id_like.as_deref(), Some("ubuntu debian"));
    }

    #[test]
    fn package_family_prefers_known_id() {
        assert_eq!(
            package_family(Some("alinux"), Some("debian")).as_deref(),
            Some("rpm"),
            "a known ID must not be overridden by ID_LIKE",
        );
        assert_eq!(package_family(Some("ubuntu"), None).as_deref(), Some("deb"));
    }

    #[test]
    fn package_family_falls_back_to_id_like_tokens() {
        // A derivative unknown by name classifies through its lineage.
        assert_eq!(
            package_family(Some("zorin"), Some("ubuntu debian")).as_deref(),
            Some("deb"),
        );
        assert_eq!(
            package_family(Some("openeuler"), Some("rhel fedora")).as_deref(),
            Some("rpm"),
        );
    }

    #[test]
    fn package_family_unknown_everywhere_is_none() {
        assert!(package_family(Some("arch"), None).is_none());
        assert!(package_family(Some("alpine"), Some("musl")).is_none());
        assert!(package_family(None, None).is_none());
    }

    #[test]
    fn parse_cap_bpf_from_proc_status_detects_cap_bpf_set() {
        let content = "Name:\tanolisa\nCapEff:\t0000008000000000\n";
        assert_eq!(parse_cap_bpf_from_proc_status(content), Some(true));
    }

    #[test]
    fn parse_cap_bpf_from_proc_status_detects_cap_bpf_unset() {
        let content = "Name:\tanolisa\nCapEff:\t00000000a80425fb\n";
        assert_eq!(parse_cap_bpf_from_proc_status(content), Some(false));
    }

    #[test]
    fn parse_cap_bpf_from_proc_status_rejects_invalid_value() {
        let content = "Name:\tanolisa\nCapEff:\tnot-hex\n";
        assert_eq!(parse_cap_bpf_from_proc_status(content), None);
    }

    #[test]
    fn detect_cap_bpf_from_status_file_missing_falls_back_to_none() {
        let path = std::path::Path::new("/definitely/missing/anolisa-proc-status");
        assert_eq!(detect_cap_bpf_from_status_file(path), None);
    }

    #[test]
    fn parse_cgroup_recognizes_docker_v1_lines() {
        let content = "\
12:hugetlb:/docker/3f4b1c2a5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a7b8c9d0e1f2a\n\
11:memory:/docker/3f4b1c2a5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a7b8c9d0e1f2a\n";
        assert_eq!(parse_cgroup(content).as_deref(), Some("docker"));
    }

    #[test]
    fn parse_cgroup_recognizes_libpod_before_podman() {
        let content = "0::/machine.slice/libpod-abc.scope\n";
        assert_eq!(parse_cgroup(content).as_deref(), Some("libpod"));
    }

    #[test]
    fn parse_cgroup_recognizes_podman_userns_path() {
        let content =
            "0::/user.slice/user-1000.slice/user@1000.service/podman-rootless.scope/abc\n";
        assert_eq!(parse_cgroup(content).as_deref(), Some("podman"));
    }

    #[test]
    fn parse_cgroup_recognizes_kubepods_before_containerd() {
        // kubepods always wins because it's the more useful label
        // even when the underlying runtime is containerd.
        let content = "0::/kubepods.slice/kubepods-besteffort.slice/cri-containerd-abc.scope\n";
        assert_eq!(parse_cgroup(content).as_deref(), Some("kubepods"));
    }

    #[test]
    fn parse_cgroup_recognizes_containerd_instance_path() {
        // `/containerd/<id>/` is the in-container cgroup path; this
        // must match.
        let content = "0::/containerd/abc123def456\n";
        assert_eq!(parse_cgroup(content).as_deref(), Some("containerd"));
    }

    #[test]
    fn parse_cgroup_ignores_host_containerd_service() {
        // Bare `containerd.service` is the host daemon, NOT a sign
        // we're inside a container. Must not match.
        let content = "0::/system.slice/containerd.service\n";
        assert!(parse_cgroup(content).is_none());
    }

    #[test]
    fn parse_cgroup_unknown_paths_return_none() {
        let content = "0::/user.slice/user-1000.slice/session-2.scope\n";
        assert!(parse_cgroup(content).is_none());
    }

    #[test]
    fn parse_cgroup_empty_input_returns_none() {
        assert!(parse_cgroup("").is_none());
        assert!(parse_cgroup("\n\n").is_none());
    }
}
