# Adding Distribution Support

[中文版](../../zh/cosh-ng/adding-distros.md)

## Overview

cosh-ng abstracts OS differences through `Distro` and `PkgManager`. Before adding
a first-class distribution, check whether its `ID_LIKE` already maps it to the
DNF, Apt, or Zypper family. Compatible derivatives need tests and documentation,
but usually no new enum variant or backend.

## Steps

### 1. Decide whether a first-class variant is needed

Linux detection reads `/etc/os-release`, falling back to
`/usr/lib/os-release` only when the first file does not exist. It first matches
the normalized `ID`, then scans whitespace-separated `ID_LIKE` values from left
to right.

An unlisted distribution such as Rocky Linux (`ID=rocky ID_LIKE="rhel fedora"`)
becomes `Distro::Compatible`. The detected package-manager family is DNF while
`id_str()` and JSON output continue to report `rocky`. Add a first-class variant
only when the distribution needs distinct behavior that a compatible family
cannot express.

### 2. Add a Distro enum variant

Add a variant in `crates/cosh-platform/src/detect.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Distro {
    // ...existing...
    MyDistro { version: String },   // New
}
```

### 3. Implement detection logic

Add ID mapping in the `detect_from_content()` match branch:

```rust
match id.as_deref() {
    // ...existing...
    Some("mydistro") => Distro::MyDistro { version },
    // ...
}
```

Values are normalized to lowercase. Keep direct `ID` matching ahead of the
`ID_LIKE` fallback so an explicitly supported distribution retains its own
variant.

### 4. Implement helper methods

```rust
impl Distro {
    pub fn id_str(&self) -> &str {
        match self {
            // ...existing...
            Distro::MyDistro { .. } => "mydistro",
        }
    }

    pub fn display_name(&self) -> String {
        match self {
            // ...existing...
            Distro::MyDistro { version } => format!("MyDistro {}", version),
        }
    }

    pub fn pkg_manager(&self) -> PkgManager {
        match self {
            // ...existing...
            Distro::MyDistro { .. } => PkgManager::Dnf, // Choose based on actual situation
        }
    }
}
```

If the new distribution uses a package manager not in the existing `PkgManager` enum, extend that enum first.

### 5. Add a package-manager backend (if needed)

If a new `PkgManager` variant is needed, add the corresponding command builder in `crates/cosh-platform/src/pkg.rs`:

```rust
// New PkgManager variant
pub enum PkgManager {
    // ...existing...
    Pacman,
}

// Add routing branch in pkg_install / pkg_remove / pkg_search / pkg_list
PkgManager::Pacman => ("pacman", vec!["-S", "--noconfirm", package]),
```

### 6. Add unit tests

Add in the `#[cfg(test)]` module of `detect.rs`:

```rust
#[test]
fn test_detect_mydistro() {
    let content = "NAME=\"My Distro\"\nVERSION_ID=\"1.0\"\nID=mydistro\n";
    let distro = Distro::detect_from_content(content);
    assert_eq!(distro, Distro::MyDistro { version: "1.0".into() });
    assert_eq!(distro.pkg_manager(), PkgManager::Dnf);
}
```

For a compatible derivative, cover its real `ID`, quoted and unquoted
`ID_LIKE`, the first recognized family, and the preserved JSON identifier.

### 7. Run targeted tests

```bash
cd src/cosh-ng

# Run detection-related tests
cargo test --locked -p cosh-platform test_detect

# Run full test suite
cargo test --locked -p cosh-platform

# Run CLI integration tests (ensure new routing doesn't break JSON envelope)
cargo test --locked -p cosh-cli
```

## Current Support Matrix

| Distribution ID | Distro Variant | PkgManager | Notes |
|----------------|---------------|------------|-------|
| `alinux` | `Alinux` | Dnf | Alibaba Cloud native Linux |
| `centos` | `CentOS` | Dnf | |
| `fedora` | `Fedora` | Dnf | |
| `ubuntu` | `Ubuntu` | Apt | |
| `debian` | `Debian` | Apt | |
| `opensuse-leap` / `opensuse-tumbleweed` / `sles` | `OpenSUSE` | Zypper | Three IDs map to same variant |
| Unlisted ID with `ID_LIKE=alinux/centos/fedora/rhel` | `Compatible` | Dnf | Keeps the real `ID`; for example, `rocky` |
| Unlisted ID with `ID_LIKE=debian/ubuntu` | `Compatible` | Apt | Keeps the real `ID` |
| Unlisted ID with `ID_LIKE=opensuse/suse` | `Compatible` | Zypper | Keeps the real `ID` |

## Design Constraints

| Rule | Description |
|------|-------------|
| Lowercase ID | `detect_from_content()` does `to_lowercase()` on ID |
| Compatible fallback | The first recognized whitespace-separated `ID_LIKE` family selects the package manager |
| Unknown fallback | IDs with no direct or compatible family match become `Unknown(String)`; package operations return `UnsupportedDistro` |
| Multi-ID merge | Multiple IDs can map to the same Distro variant (e.g., opensuse family) |
| Package manager decoupling | `PkgManager` and `Distro` are separate enums, mapped via `pkg_manager()` |
| File precedence | `/etc/os-release` takes precedence; `/usr/lib/os-release` is used only when it is absent |

## Complete Checklist

- [ ] Decide whether `ID_LIKE` compatibility is sufficient
- [ ] Add a `Distro` variant and direct-ID match only when distinct behavior is required
- [ ] `id_str()` preserves the correct distribution identifier
- [ ] `display_name()` returns a readable name
- [ ] `pkg_manager()` maps to the intended family
- [ ] `Display` trait (via `display_name()`) formats correctly
- [ ] Tests cover direct IDs, `ID_LIKE`, quotes, file fallback, and unknown input as applicable
- [ ] If a new `PkgManager` is needed, add routing in all `pkg.rs` operations
- [ ] Update [Supported distributions](../../../user-guide/en/user-entrypoint/cosh-ng/supported-distros.md)
