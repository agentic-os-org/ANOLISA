use super::{ReadonlySpec, Validator};

// ── Version checks ──
pub(super) const RUSTC: ReadonlySpec = ReadonlySpec {
    command: "rustc",
    validator: Validator::VersionCheck(&["--version", "-V"]),
};

pub(super) const RUSTUP: ReadonlySpec = ReadonlySpec {
    command: "rustup",
    validator: Validator::VersionCheck(&["--version"]),
};

pub(super) const NODE: ReadonlySpec = ReadonlySpec {
    command: "node",
    validator: Validator::VersionCheck(&["--version", "-v"]),
};

pub(super) const NPM: ReadonlySpec = ReadonlySpec {
    command: "npm",
    validator: Validator::VersionCheck(&["--version", "-v"]),
};

pub(super) const PYTHON: ReadonlySpec = ReadonlySpec {
    command: "python",
    validator: Validator::VersionCheck(&["--version", "-V"]),
};

pub(super) const PYTHON3: ReadonlySpec = ReadonlySpec {
    command: "python3",
    validator: Validator::VersionCheck(&["--version", "-V"]),
};

pub(super) const PIP: ReadonlySpec = ReadonlySpec {
    command: "pip",
    validator: Validator::VersionCheck(&["--version", "-V"]),
};

pub(super) const PIP3: ReadonlySpec = ReadonlySpec {
    command: "pip3",
    validator: Validator::VersionCheck(&["--version", "-V"]),
};

pub(super) const JAVA: ReadonlySpec = ReadonlySpec {
    command: "java",
    validator: Validator::VersionCheck(&["-version", "--version"]),
};

pub(super) const JAVAC: ReadonlySpec = ReadonlySpec {
    command: "javac",
    validator: Validator::VersionCheck(&["-version", "--version"]),
};

pub(super) const RUBY: ReadonlySpec = ReadonlySpec {
    command: "ruby",
    validator: Validator::VersionCheck(&["--version", "-v"]),
};

pub(super) const SWIFT: ReadonlySpec = ReadonlySpec {
    command: "swift",
    validator: Validator::VersionCheck(&["--version"]),
};

pub(super) const CLANG: ReadonlySpec = ReadonlySpec {
    command: "clang",
    validator: Validator::VersionCheck(&["--version"]),
};

pub(super) const GCC: ReadonlySpec = ReadonlySpec {
    command: "gcc",
    validator: Validator::VersionCheck(&["--version", "-v"]),
};

pub(super) const G_PLUS_PLUS: ReadonlySpec = ReadonlySpec {
    command: "g++",
    validator: Validator::VersionCheck(&["--version", "-v"]),
};

pub(super) const CMAKE: ReadonlySpec = ReadonlySpec {
    command: "cmake",
    validator: Validator::VersionCheck(&["--version"]),
};

pub(super) const MAKE: ReadonlySpec = ReadonlySpec {
    command: "make",
    validator: Validator::VersionCheck(&["--version", "-v"]),
};

pub(super) const DOTNET: ReadonlySpec = ReadonlySpec {
    command: "dotnet",
    validator: Validator::VersionCheck(&["--version"]),
};
