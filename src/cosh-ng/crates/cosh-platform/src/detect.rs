use std::fs;
use std::path::Path;
use std::process::Command;

/// Operating system / distribution variants supported by cosh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Distro {
    Alinux {
        version: String,
    },
    Ubuntu {
        version: String,
    },
    Debian {
        version: String,
    },
    CentOS {
        version: String,
    },
    Fedora {
        version: String,
    },
    OpenSUSE {
        version: String,
    },
    MacOS {
        version: String,
    },
    /// An unlisted distribution with a supported package-manager family.
    Compatible {
        id: String,
        version: String,
        pkg_manager: PkgManager,
    },
    Unknown(String),
}

impl Distro {
    /// Detect the current OS. On macOS uses `sw_vers`; on Linux reads
    /// /etc/os-release, falling back to /usr/lib/os-release.
    pub fn detect() -> Self {
        // Check macOS first (compile-time or runtime)
        if cfg!(target_os = "macos") {
            return Self::detect_macos();
        }
        Self::detect_from_paths(
            Path::new("/etc/os-release"),
            Path::new("/usr/lib/os-release"),
        )
    }

    /// Detect macOS version via `sw_vers -productVersion`.
    fn detect_macos() -> Self {
        let version = Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "unknown".into());
        Distro::MacOS { version }
    }

    fn detect_from_paths(primary: &Path, fallback: &Path) -> Self {
        match fs::read_to_string(primary) {
            Ok(content) => Self::detect_from_content(&content),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // os-release gives /etc exclusive precedence whenever it exists.
                match fs::read_to_string(fallback) {
                    Ok(content) => Self::detect_from_content(&content),
                    Err(_) => Self::Unknown("unknown".into()),
                }
            }
            Err(_) => Self::Unknown("unknown".into()),
        }
    }

    pub(crate) fn detect_from_content(content: &str) -> Self {
        let mut id = None;
        let mut id_like = None;
        let mut version_id = None;

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = strip_matching_outer_quotes(value.trim());

                if key.eq_ignore_ascii_case("ID") {
                    id = Some(value.to_lowercase());
                } else if key.eq_ignore_ascii_case("ID_LIKE") {
                    id_like = Some(value.to_lowercase());
                } else if key.eq_ignore_ascii_case("VERSION_ID") {
                    version_id = Some(value.to_string());
                }
            }
        }

        let version = version_id.unwrap_or_else(|| "unknown".into());

        let id = id.unwrap_or_else(|| "linux".into());

        if let Some(distro) = Self::from_known_id(&id, &version) {
            return distro;
        }

        if let Some(pkg_manager) = id_like
            .as_deref()
            .and_then(|ids| ids.split_whitespace().find_map(pkg_manager_from_id_like))
        {
            return Distro::Compatible {
                id,
                version,
                pkg_manager,
            };
        }

        Distro::Unknown(id)
    }

    fn from_known_id(id: &str, version: &str) -> Option<Self> {
        let distro = match id {
            "alinux" => Distro::Alinux {
                version: version.into(),
            },
            "ubuntu" => Distro::Ubuntu {
                version: version.into(),
            },
            "debian" => Distro::Debian {
                version: version.into(),
            },
            "centos" => Distro::CentOS {
                version: version.into(),
            },
            "fedora" => Distro::Fedora {
                version: version.into(),
            },
            "opensuse-leap" | "opensuse-tumbleweed" | "sles" => Distro::OpenSUSE {
                version: version.into(),
            },
            _ => return None,
        };
        Some(distro)
    }

    /// Returns the distro identifier string for JSON output.
    pub fn id_str(&self) -> &str {
        match self {
            Distro::Alinux { .. } => "alinux",
            Distro::Ubuntu { .. } => "ubuntu",
            Distro::Debian { .. } => "debian",
            Distro::CentOS { .. } => "centos",
            Distro::Fedora { .. } => "fedora",
            Distro::OpenSUSE { .. } => "opensuse",
            Distro::MacOS { .. } => "macos",
            Distro::Compatible { id, .. } => id,
            Distro::Unknown(id) => id,
        }
    }

    /// Returns a human-readable display name.
    pub fn display_name(&self) -> String {
        match self {
            Distro::Alinux { version } => format!("Alinux {}", version),
            Distro::Ubuntu { version } => format!("Ubuntu {}", version),
            Distro::Debian { version } => format!("Debian {}", version),
            Distro::CentOS { version } => format!("CentOS {}", version),
            Distro::Fedora { version } => format!("Fedora {}", version),
            Distro::OpenSUSE { version } => format!("openSUSE {}", version),
            Distro::MacOS { version } => format!("macOS {}", version),
            Distro::Compatible { id, version, .. } => format!("{} {}", id, version),
            Distro::Unknown(id) => format!("Unknown ({})", id),
        }
    }

    /// Returns which package manager this distro uses.
    pub fn pkg_manager(&self) -> PkgManager {
        match self {
            Distro::Alinux { .. } | Distro::CentOS { .. } | Distro::Fedora { .. } => {
                PkgManager::Dnf
            }
            Distro::Ubuntu { .. } | Distro::Debian { .. } => PkgManager::Apt,
            Distro::OpenSUSE { .. } => PkgManager::Zypper,
            Distro::MacOS { .. } => PkgManager::Brew,
            Distro::Compatible { pkg_manager, .. } => *pkg_manager,
            Distro::Unknown(_) => PkgManager::Unknown,
        }
    }
}

fn strip_matching_outer_quotes(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && matches!(
            (bytes[0], bytes[bytes.len() - 1]),
            (b'\'', b'\'') | (b'"', b'"')
        )
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn pkg_manager_from_id_like(id: &str) -> Option<PkgManager> {
    match id {
        "alinux" | "centos" | "fedora" | "rhel" => Some(PkgManager::Dnf),
        "debian" | "ubuntu" => Some(PkgManager::Apt),
        "opensuse" | "suse" => Some(PkgManager::Zypper),
        _ => None,
    }
}

/// Package manager family for routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PkgManager {
    Dnf,
    Apt,
    Zypper,
    Brew,
    Unknown,
}

impl std::fmt::Display for Distro {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_alinux() {
        let content = "NAME=\"Alibaba Cloud Linux\"\nVERSION_ID=\"3\"\nID=alinux\n";
        let distro = Distro::detect_from_content(content);
        assert_eq!(
            distro,
            Distro::Alinux {
                version: "3".into()
            }
        );
        assert_eq!(distro.pkg_manager(), PkgManager::Dnf);
    }

    #[test]
    fn test_detect_ubuntu() {
        let content = "NAME=\"Ubuntu\"\nVERSION_ID=\"22.04\"\nID=ubuntu\n";
        let distro = Distro::detect_from_content(content);
        assert_eq!(
            distro,
            Distro::Ubuntu {
                version: "22.04".into()
            }
        );
        assert_eq!(distro.pkg_manager(), PkgManager::Apt);
    }

    #[test]
    fn test_detect_fedora() {
        let content = "NAME=\"Fedora Linux\"\nVERSION_ID=\"39\"\nID=fedora\n";
        let distro = Distro::detect_from_content(content);
        assert_eq!(
            distro,
            Distro::Fedora {
                version: "39".into()
            }
        );
        assert_eq!(distro.pkg_manager(), PkgManager::Dnf);
    }

    #[test]
    fn test_detect_opensuse() {
        let content = "NAME=\"openSUSE Leap\"\nVERSION_ID=\"15.5\"\nID=opensuse-leap\n";
        let distro = Distro::detect_from_content(content);
        assert_eq!(
            distro,
            Distro::OpenSUSE {
                version: "15.5".into()
            }
        );
        assert_eq!(distro.pkg_manager(), PkgManager::Zypper);
    }

    #[test]
    fn test_id_str() {
        assert_eq!(
            Distro::Alinux {
                version: "3".into()
            }
            .id_str(),
            "alinux"
        );
        assert_eq!(
            Distro::Ubuntu {
                version: "22.04".into()
            }
            .id_str(),
            "ubuntu"
        );
        assert_eq!(
            Distro::OpenSUSE {
                version: "15.5".into()
            }
            .id_str(),
            "opensuse"
        );
    }

    // --- Edge case tests ---

    #[test]
    fn test_missing_version_id() {
        let content = "NAME=\"Alibaba Cloud Linux\"\nID=alinux\n";
        let distro = Distro::detect_from_content(content);
        assert_eq!(
            distro,
            Distro::Alinux {
                version: "unknown".into()
            }
        );
    }

    #[test]
    fn test_missing_id_defaults_to_linux() {
        let content = "NAME=\"Some Distro\"\nVERSION_ID=\"42\"\n";
        let distro = Distro::detect_from_content(content);
        assert_eq!(distro, Distro::Unknown("linux".into()));
    }

    #[test]
    fn test_empty_id_field() {
        let content = "ID=\nVERSION_ID=\"3\"\n";
        let distro = Distro::detect_from_content(content);
        // Empty ID string after trimming quotes produces Unknown with empty string
        assert!(matches!(distro, Distro::Unknown(_)));
    }

    #[test]
    fn test_malformed_os_release_no_equals() {
        let content = "NOTAKEYVALUE\nID=ubuntu\nVERSION_ID=22.04";
        let distro = Distro::detect_from_content(content);
        assert_eq!(
            distro,
            Distro::Ubuntu {
                version: "22.04".into()
            }
        );
    }

    #[test]
    fn test_malformed_os_release_garbage() {
        let content = "\\\\\\\n@@@!!!\nID=centos\nVERSION_ID=9";
        let distro = Distro::detect_from_content(content);
        assert_eq!(
            distro,
            Distro::CentOS {
                version: "9".into()
            }
        );
    }

    #[test]
    fn test_empty_content() {
        let content = "";
        let distro = Distro::detect_from_content(content);
        assert_eq!(distro, Distro::Unknown("linux".into()));
    }

    #[test]
    fn test_comment_lines_ignored() {
        let content = "# This is a comment\n#ID=fake\nID=fedora\nVERSION_ID=40";
        let distro = Distro::detect_from_content(content);
        assert_eq!(
            distro,
            Distro::Fedora {
                version: "40".into()
            }
        );
    }

    #[test]
    fn test_blank_lines_ignored() {
        let content = "\n\nID=debian\n\nVERSION_ID=12\n";
        let distro = Distro::detect_from_content(content);
        assert_eq!(
            distro,
            Distro::Debian {
                version: "12".into()
            }
        );
    }

    #[test]
    fn test_quoted_values() {
        let content = "ID=\"ubuntu\"\nVERSION_ID=\"22.04\"";
        let distro = Distro::detect_from_content(content);
        assert_eq!(
            distro,
            Distro::Ubuntu {
                version: "22.04".into()
            }
        );
    }

    #[test]
    fn test_id_case_insensitive() {
        let content = "ID=Ubuntu\nVERSION_ID=22.04";
        let distro = Distro::detect_from_content(content);
        assert_eq!(
            distro,
            Distro::Ubuntu {
                version: "22.04".into()
            }
        );
    }

    #[test]
    fn test_opensuse_tumbleweed() {
        let content = "ID=opensuse-tumbleweed\nVERSION_ID=20240501";
        let distro = Distro::detect_from_content(content);
        assert_eq!(
            distro,
            Distro::OpenSUSE {
                version: "20240501".into()
            }
        );
        assert_eq!(distro.pkg_manager(), PkgManager::Zypper);
    }

    #[test]
    fn test_sles() {
        let content = "ID=sles\nVERSION_ID=15.5";
        let distro = Distro::detect_from_content(content);
        assert_eq!(
            distro,
            Distro::OpenSUSE {
                version: "15.5".into()
            }
        );
        assert_eq!(distro.pkg_manager(), PkgManager::Zypper);
    }

    #[test]
    fn test_unknown_distro_id() {
        let content = "ID=arch\nVERSION_ID=rolling";
        let distro = Distro::detect_from_content(content);
        assert_eq!(distro, Distro::Unknown("arch".into()));
        assert_eq!(distro.pkg_manager(), PkgManager::Unknown);
    }

    #[test]
    fn test_detect_uses_id_like_for_compatible_distro() {
        let content = "ID=rocky\nID_LIKE=\"rhel fedora\"\nVERSION_ID=9.5\n";
        let distro = Distro::detect_from_content(content);
        assert_eq!(
            distro,
            Distro::Compatible {
                id: "rocky".into(),
                version: "9.5".into(),
                pkg_manager: PkgManager::Dnf,
            }
        );
        assert_eq!(distro.id_str(), "rocky");
        assert_eq!(distro.pkg_manager(), PkgManager::Dnf);
    }

    #[test]
    fn test_detect_uses_single_quoted_id_like() {
        let content = "ID=rocky\nID_LIKE='rhel fedora'\nVERSION_ID=9.5\n";
        let distro = Distro::detect_from_content(content);
        assert_eq!(distro.pkg_manager(), PkgManager::Dnf);
    }

    #[test]
    fn test_detect_uses_id_like_when_id_is_missing() {
        let content = "ID_LIKE=debian\nVERSION_ID=1\n";
        let distro = Distro::detect_from_content(content);
        assert_eq!(
            distro,
            Distro::Compatible {
                id: "linux".into(),
                version: "1".into(),
                pkg_manager: PkgManager::Apt,
            }
        );
    }

    #[test]
    fn test_detect_uses_first_supported_id_like() {
        let content = "ID=custom\nID_LIKE=\"unknown debian rhel\"\nVERSION_ID=1\n";
        let distro = Distro::detect_from_content(content);
        assert_eq!(distro.pkg_manager(), PkgManager::Apt);
    }

    #[test]
    fn test_detect_falls_back_to_usr_lib_os_release() {
        let dir = tempfile::tempdir().unwrap();
        let etc_os_release = dir.path().join("etc-os-release");
        let usr_os_release = dir.path().join("usr-lib-os-release");
        fs::write(&usr_os_release, "ID=debian\nVERSION_ID=12\n").unwrap();

        let distro = Distro::detect_from_paths(&etc_os_release, &usr_os_release);
        assert_eq!(
            distro,
            Distro::Debian {
                version: "12".into()
            }
        );
    }

    #[test]
    fn test_detect_does_not_fallback_when_primary_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let etc_os_release = dir.path().join("etc-os-release");
        let usr_os_release = dir.path().join("usr-lib-os-release");
        fs::create_dir(&etc_os_release).unwrap();
        fs::write(&usr_os_release, "ID=debian\nVERSION_ID=12\n").unwrap();

        let distro = Distro::detect_from_paths(&etc_os_release, &usr_os_release);
        assert_eq!(distro, Distro::Unknown("unknown".into()));
    }

    #[test]
    fn test_display_name() {
        assert_eq!(
            Distro::Alinux {
                version: "3".into()
            }
            .display_name(),
            "Alinux 3"
        );
        assert_eq!(
            Distro::Unknown("arch".into()).display_name(),
            "Unknown (arch)"
        );
    }

    // --- macOS tests ---

    #[test]
    fn test_macos_variant() {
        let distro = Distro::MacOS {
            version: "15.4".into(),
        };
        assert_eq!(distro.id_str(), "macos");
        assert_eq!(distro.display_name(), "macOS 15.4");
        assert_eq!(distro.pkg_manager(), PkgManager::Brew);
    }

    #[test]
    fn test_macos_display_name_format() {
        let distro = Distro::MacOS {
            version: "14.2.1".into(),
        };
        assert_eq!(distro.display_name(), "macOS 14.2.1");
    }

    #[test]
    fn test_macos_unknown_version() {
        let distro = Distro::MacOS {
            version: "unknown".into(),
        };
        assert_eq!(distro.display_name(), "macOS unknown");
        assert_eq!(distro.pkg_manager(), PkgManager::Brew);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_detect_on_macos_returns_macos_variant() {
        let distro = Distro::detect();
        assert!(matches!(distro, Distro::MacOS { .. }));
        // version should be a non-empty string like "15.4" or "14.2.1"
        if let Distro::MacOS { version } = &distro {
            assert!(!version.is_empty());
            assert_ne!(version, "unknown");
        }
    }
}
