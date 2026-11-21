use super::{GenericSpec, PathMode, ReadonlySpec, SubcommandSpec, Validator};
use crate::tools::readonly_rules::validators;

// ── Subcommand dispatch: git ──
// Git subcommands can execute repo-embedded programs from .git/config and
// .gitattributes: core.fsmonitor on index refresh, diff.<driver>.textconv on
// patch generation, gpg.program on signature verification (log.showSignature,
// %G placeholders, branch.sort/tag.sort signature atoms), and promisor lazy
// fetch (core.sshCommand, credential helpers) whenever an object read misses
// locally. The classifier sees neither repo config nor object availability,
// and env overrides do not neutralize repo sort atoms, so only forms that
// read config or count local objects without touching history, refs'
// signature fields, or object contents are auto-approved; everything else
// requires approval.
pub(super) const GIT: ReadonlySpec = ReadonlySpec {
    command: "git",
    validator: Validator::Subcommand(SubcommandSpec {
        deny_args: &["-c", "--ext-diff", "--textconv", "--output", "--exec-path"],
        subcommands: &[
            (
                "remote",
                Validator::Custom(validators::is_readonly_git_remote),
            ),
            (
                "count-objects",
                Validator::Generic(GenericSpec {
                    short_flags: "vH",
                    long_flags: &["--verbose", "--human-readable"],
                    value_flags: &[],
                    deny_flags: &[],
                    path_mode: PathMode::None,
                    bare_number_max: 0,
                }),
            ),
            (
                "config",
                Validator::Custom(validators::is_readonly_git_config),
            ),
        ],
    }),
};

// ── Subcommand dispatch: cargo ──
pub(super) const CARGO: ReadonlySpec = ReadonlySpec {
    command: "cargo",
    validator: Validator::Subcommand(SubcommandSpec {
        deny_args: &[],
        subcommands: &[
            ("--version", Validator::Bare),
            (
                "tree",
                Validator::Generic(GenericSpec {
                    short_flags: "eidp",
                    long_flags: &[
                        "--workspace",
                        "--all-features",
                        "--no-default-features",
                        "--no-dedupe",
                        "--duplicates",
                        "--invert",
                    ],
                    value_flags: &[
                        ("--depth", Some(100)),
                        ("--target", None),
                        ("--features", None),
                        ("--format", None),
                        ("-p", None),
                        ("--package", None),
                    ],
                    deny_flags: &[],
                    path_mode: PathMode::None,
                    bare_number_max: 0,
                }),
            ),
            (
                "metadata",
                Validator::Generic(GenericSpec {
                    short_flags: "",
                    long_flags: &["--no-deps", "--all-features", "--no-default-features"],
                    value_flags: &[
                        ("--format-version", None),
                        ("--features", None),
                        ("--filter-platform", None),
                    ],
                    deny_flags: &[],
                    path_mode: PathMode::None,
                    bare_number_max: 0,
                }),
            ),
        ],
    }),
};

// ── Subcommand dispatch: docker ──
pub(super) const DOCKER: ReadonlySpec = ReadonlySpec {
    command: "docker",
    validator: Validator::Subcommand(SubcommandSpec {
        deny_args: &[],
        subcommands: &[
            (
                "ps",
                Validator::Generic(GenericSpec {
                    short_flags: "aqns",
                    long_flags: &["--no-trunc", "--latest", "--last", "--size"],
                    value_flags: &[
                        ("--format", None),
                        ("-f", None),
                        ("--filter", None),
                        ("-n", Some(10_000)),
                    ],
                    deny_flags: &[],
                    path_mode: PathMode::None,
                    bare_number_max: 0,
                }),
            ),
            (
                "images",
                Validator::Generic(GenericSpec {
                    short_flags: "aq",
                    long_flags: &["--no-trunc", "--digests"],
                    value_flags: &[("--format", None), ("-f", None), ("--filter", None)],
                    deny_flags: &[],
                    path_mode: PathMode::Unchecked,
                    bare_number_max: 0,
                }),
            ),
            (
                "inspect",
                Validator::Generic(GenericSpec {
                    short_flags: "s",
                    long_flags: &["--size"],
                    value_flags: &[("--format", None), ("--type", None), ("-f", None)],
                    deny_flags: &[],
                    path_mode: PathMode::Unchecked,
                    bare_number_max: 0,
                }),
            ),
            ("version", Validator::Bare),
            ("info", Validator::Bare),
        ],
    }),
};

// ── Subcommand dispatch: kubectl ──
pub(super) const KUBECTL: ReadonlySpec = ReadonlySpec {
    command: "kubectl",
    validator: Validator::Subcommand(SubcommandSpec {
        deny_args: &["--dry-run"],
        subcommands: &[
            (
                "get",
                Validator::Generic(GenericSpec {
                    short_flags: "Aw",
                    long_flags: &[
                        "--all-namespaces",
                        "--show-labels",
                        "--no-headers",
                        "--watch-only",
                        "--show-kind",
                    ],
                    value_flags: &[
                        ("-o", None),
                        ("--output", None),
                        ("-n", None),
                        ("--namespace", None),
                        ("-l", None),
                        ("--selector", None),
                        ("--field-selector", None),
                        ("--sort-by", None),
                    ],
                    deny_flags: &[],
                    path_mode: PathMode::Unchecked,
                    bare_number_max: 0,
                }),
            ),
            (
                "describe",
                Validator::Generic(GenericSpec {
                    short_flags: "A",
                    long_flags: &["--all-namespaces", "--show-events"],
                    value_flags: &[
                        ("-n", None),
                        ("--namespace", None),
                        ("-l", None),
                        ("--selector", None),
                    ],
                    deny_flags: &[],
                    path_mode: PathMode::Unchecked,
                    bare_number_max: 0,
                }),
            ),
            (
                "version",
                Validator::Generic(GenericSpec {
                    short_flags: "",
                    long_flags: &["--client", "--short"],
                    value_flags: &[("-o", None), ("--output", None)],
                    deny_flags: &[],
                    path_mode: PathMode::None,
                    bare_number_max: 0,
                }),
            ),
            (
                "top",
                Validator::Generic(GenericSpec {
                    short_flags: "A",
                    long_flags: &["--all-namespaces", "--containers", "--no-headers"],
                    value_flags: &[
                        ("-n", None),
                        ("--namespace", None),
                        ("-l", None),
                        ("--selector", None),
                        ("--sort-by", None),
                    ],
                    deny_flags: &[],
                    path_mode: PathMode::Unchecked,
                    bare_number_max: 0,
                }),
            ),
        ],
    }),
};

// ── Subcommand dispatch: go ──
pub(super) const GO: ReadonlySpec = ReadonlySpec {
    command: "go",
    validator: Validator::Subcommand(SubcommandSpec {
        deny_args: &[],
        subcommands: &[
            ("version", Validator::Bare),
            (
                "env",
                Validator::Generic(GenericSpec {
                    short_flags: "",
                    long_flags: &["-json"],
                    value_flags: &[],
                    deny_flags: &["-w", "-u"],
                    path_mode: PathMode::Unchecked,
                    bare_number_max: 0,
                }),
            ),
        ],
    }),
};

// ── Subcommand dispatch: macOS ──
pub(super) const DISKUTIL: ReadonlySpec = ReadonlySpec {
    command: "diskutil",
    validator: Validator::Subcommand(SubcommandSpec {
        deny_args: &[],
        subcommands: &[
            (
                "list",
                Validator::Generic(GenericSpec {
                    short_flags: "",
                    long_flags: &["-plist"],
                    value_flags: &[],
                    deny_flags: &[],
                    path_mode: PathMode::Unchecked,
                    bare_number_max: 0,
                }),
            ),
            (
                "info",
                Validator::Generic(GenericSpec {
                    short_flags: "",
                    long_flags: &["-plist", "-all"],
                    value_flags: &[],
                    deny_flags: &[],
                    path_mode: PathMode::Unchecked,
                    bare_number_max: 0,
                }),
            ),
        ],
    }),
};

pub(super) const DEFAULTS: ReadonlySpec = ReadonlySpec {
    command: "defaults",
    validator: Validator::Subcommand(SubcommandSpec {
        deny_args: &[],
        subcommands: &[(
            "read",
            Validator::Generic(GenericSpec {
                short_flags: "g",
                long_flags: &["-globalDomain"],
                value_flags: &[],
                deny_flags: &[],
                path_mode: PathMode::Unchecked,
                bare_number_max: 0,
            }),
        )],
    }),
};

pub(super) const XCRUN: ReadonlySpec = ReadonlySpec {
    command: "xcrun",
    validator: Validator::Subcommand(SubcommandSpec {
        deny_args: &[],
        subcommands: &[(
            "--find",
            Validator::Generic(GenericSpec {
                short_flags: "",
                long_flags: &[],
                value_flags: &[],
                deny_flags: &[],
                path_mode: PathMode::Unchecked,
                bare_number_max: 0,
            }),
        )],
    }),
};

pub(super) const SYSTEM_PROFILER: ReadonlySpec = ReadonlySpec {
    command: "system_profiler",
    validator: Validator::Generic(GenericSpec {
        short_flags: "",
        long_flags: &["-xml", "-json", "-detailLevel"],
        value_flags: &[],
        deny_flags: &[],
        path_mode: PathMode::Unchecked,
        bare_number_max: 0,
    }),
};
