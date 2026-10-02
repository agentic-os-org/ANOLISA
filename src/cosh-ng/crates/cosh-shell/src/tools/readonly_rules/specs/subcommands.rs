use super::{GenericSpec, PathMode, ReadonlySpec, SubcommandSpec, Validator};
use crate::tools::readonly_rules::validators;

// ── Subcommand dispatch: git ──
pub(super) const GIT: ReadonlySpec = ReadonlySpec {
    command: "git",
    validator: Validator::Subcommand(SubcommandSpec {
        deny_args: &["-c", "--ext-diff", "--textconv", "--output", "--exec-path"],
        subcommands: &[
            (
                "status",
                Validator::Generic(GenericSpec {
                    short_flags: "s",
                    long_flags: &[
                        "--short",
                        "--branch",
                        "--porcelain",
                        "--porcelain=v1",
                        "--porcelain=v2",
                        "--ignored",
                        "--untracked-files",
                        "--untracked-files=no",
                        "--untracked-files=normal",
                        "--untracked-files=all",
                    ],
                    value_flags: &[],
                    deny_flags: &[],
                    path_mode: PathMode::None,
                    bare_number_max: 0,
                }),
            ),
            (
                "diff",
                Validator::Generic(GenericSpec {
                    short_flags: "",
                    long_flags: &[
                        "--stat",
                        "--shortstat",
                        "--numstat",
                        "--summary",
                        "--name-only",
                        "--name-status",
                        "--cached",
                        "--staged",
                        "--check",
                        "--no-ext-diff",
                    ],
                    value_flags: &[],
                    deny_flags: &[],
                    path_mode: PathMode::Optional,
                    bare_number_max: 0,
                }),
            ),
            (
                "log",
                Validator::Generic(GenericSpec {
                    short_flags: "",
                    long_flags: &[
                        "--oneline",
                        "--decorate",
                        "--stat",
                        "--shortstat",
                        "--name-only",
                        "--name-status",
                        "--no-ext-diff",
                        "--graph",
                        "--all",
                        "--follow",
                        "--first-parent",
                        "--reverse",
                        "--no-merges",
                        "--merges",
                    ],
                    value_flags: &[
                        ("-n", Some(10_000)),
                        ("--max-count", Some(10_000)),
                        ("--format", None),
                        ("--pretty", None),
                        ("--since", None),
                        ("--until", None),
                        ("--after", None),
                        ("--before", None),
                        ("--author", None),
                        ("--grep", None),
                    ],
                    deny_flags: &[],
                    path_mode: PathMode::Optional,
                    bare_number_max: 10_000,
                }),
            ),
            (
                "show",
                Validator::Generic(GenericSpec {
                    short_flags: "",
                    long_flags: &[
                        "--stat",
                        "--shortstat",
                        "--name-only",
                        "--name-status",
                        "--no-ext-diff",
                    ],
                    value_flags: &[("--format", None), ("--pretty", None)],
                    deny_flags: &[],
                    path_mode: PathMode::Optional,
                    bare_number_max: 0,
                }),
            ),
            (
                "branch",
                Validator::Custom(validators::is_readonly_git_branch),
            ),
            (
                "remote",
                Validator::Generic(GenericSpec {
                    short_flags: "v",
                    long_flags: &["--verbose"],
                    value_flags: &[],
                    deny_flags: &[],
                    path_mode: PathMode::Optional,
                    bare_number_max: 0,
                }),
            ),
            (
                "rev-parse",
                Validator::Generic(GenericSpec {
                    short_flags: "",
                    long_flags: &[
                        "--short",
                        "--verify",
                        "--abbrev-ref",
                        "--show-toplevel",
                        "--git-dir",
                        "--is-inside-work-tree",
                        "--is-bare-repository",
                        "--show-cdup",
                        "--show-prefix",
                        "--absolute-git-dir",
                    ],
                    value_flags: &[],
                    deny_flags: &[],
                    path_mode: PathMode::Optional,
                    bare_number_max: 0,
                }),
            ),
            (
                "blame",
                Validator::Generic(GenericSpec {
                    short_flags: "eLftnsMpw",
                    long_flags: &[
                        "--line-porcelain",
                        "--porcelain",
                        "--root",
                        "--show-stats",
                        "--no-progress",
                    ],
                    value_flags: &[("-L", None)],
                    deny_flags: &[],
                    path_mode: PathMode::Optional,
                    bare_number_max: 0,
                }),
            ),
            (
                "describe",
                Validator::Generic(GenericSpec {
                    short_flags: "",
                    long_flags: &[
                        "--tags",
                        "--always",
                        "--long",
                        "--dirty",
                        "--first-parent",
                        "--all",
                    ],
                    value_flags: &[
                        ("--abbrev", Some(40)),
                        ("--match", None),
                        ("--exclude", None),
                    ],
                    deny_flags: &[],
                    path_mode: PathMode::Optional,
                    bare_number_max: 0,
                }),
            ),
            (
                "ls-files",
                Validator::Generic(GenericSpec {
                    short_flags: "codsimktz",
                    long_flags: &[
                        "--error-unmatch",
                        "--full-name",
                        "--abbrev",
                        "--debug",
                        "--deduplicate",
                    ],
                    value_flags: &[],
                    deny_flags: &["--delete", "--modify"],
                    path_mode: PathMode::Optional,
                    bare_number_max: 0,
                }),
            ),
            (
                "ls-tree",
                Validator::Generic(GenericSpec {
                    short_flags: "rdtlz",
                    long_flags: &[
                        "--name-only",
                        "--name-status",
                        "--full-name",
                        "--full-tree",
                        "--long",
                        "--abbrev",
                    ],
                    value_flags: &[],
                    deny_flags: &[],
                    path_mode: PathMode::Optional,
                    bare_number_max: 0,
                }),
            ),
            (
                "grep",
                Validator::Generic(GenericSpec {
                    short_flags: "niclwEFP",
                    long_flags: &[
                        "--cached",
                        "--no-index",
                        "--untracked",
                        "--fixed-strings",
                        "--perl-regexp",
                        "--extended-regexp",
                        "--word-regexp",
                        "--count",
                        "--name-only",
                        "--files-with-matches",
                        "--files-without-match",
                        "--heading",
                        "--break",
                        "--show-function",
                    ],
                    value_flags: &[
                        ("-A", Some(10_000)),
                        ("-B", Some(10_000)),
                        ("-C", Some(10_000)),
                        ("-e", None),
                        ("--max-depth", Some(20)),
                    ],
                    deny_flags: &[],
                    path_mode: PathMode::Unchecked,
                    bare_number_max: 0,
                }),
            ),
            (
                "shortlog",
                Validator::Generic(GenericSpec {
                    short_flags: "sne",
                    long_flags: &["--summary", "--numbered", "--email", "--no-merges"],
                    value_flags: &[("-w", None), ("--group", None), ("--format", None)],
                    deny_flags: &[],
                    path_mode: PathMode::Optional,
                    bare_number_max: 0,
                }),
            ),
            (
                "rev-list",
                Validator::Generic(GenericSpec {
                    short_flags: "",
                    long_flags: &[
                        "--count",
                        "--all",
                        "--oneline",
                        "--first-parent",
                        "--no-merges",
                        "--merges",
                        "--reverse",
                        "--ancestry-path",
                        "--topo-order",
                        "--date-order",
                    ],
                    value_flags: &[
                        ("-n", Some(10_000)),
                        ("--max-count", Some(10_000)),
                        ("--since", None),
                        ("--until", None),
                        ("--after", None),
                        ("--before", None),
                    ],
                    deny_flags: &[],
                    path_mode: PathMode::Optional,
                    bare_number_max: 10_000,
                }),
            ),
            (
                "cat-file",
                Validator::Generic(GenericSpec {
                    short_flags: "tpse",
                    long_flags: &["--textconv", "--batch", "--batch-check"],
                    value_flags: &[],
                    deny_flags: &[],
                    path_mode: PathMode::Unchecked,
                    bare_number_max: 0,
                }),
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
                "for-each-ref",
                Validator::Generic(GenericSpec {
                    short_flags: "",
                    long_flags: &[],
                    value_flags: &[
                        ("--sort", None),
                        ("--format", None),
                        ("--count", Some(10_000)),
                    ],
                    deny_flags: &[],
                    path_mode: PathMode::Unchecked,
                    bare_number_max: 0,
                }),
            ),
            (
                "name-rev",
                Validator::Generic(GenericSpec {
                    short_flags: "",
                    long_flags: &[
                        "--tags",
                        "--name-only",
                        "--no-undefined",
                        "--always",
                        "--stdin",
                    ],
                    value_flags: &[("--refs", None), ("--exclude", None)],
                    deny_flags: &[],
                    path_mode: PathMode::Optional,
                    bare_number_max: 0,
                }),
            ),
            (
                "stash",
                Validator::Custom(validators::is_readonly_git_stash),
            ),
            (
                "config",
                Validator::Custom(validators::is_readonly_git_config),
            ),
            ("tag", Validator::Custom(validators::is_readonly_git_tag)),
            (
                "reflog",
                Validator::Custom(validators::is_readonly_git_reflog),
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
