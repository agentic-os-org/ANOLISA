use super::{GenericSpec, PathMode, ReadonlySpec, Validator};

// ── Bare commands (no args) ──
pub(super) const PWD: ReadonlySpec = ReadonlySpec {
    command: "pwd",
    validator: Validator::Bare,
};

pub(super) const WHOAMI: ReadonlySpec = ReadonlySpec {
    command: "whoami",
    validator: Validator::Bare,
};

pub(super) const HOSTNAME: ReadonlySpec = ReadonlySpec {
    command: "hostname",
    validator: Validator::Bare,
};

pub(super) const DATE: ReadonlySpec = ReadonlySpec {
    command: "date",
    validator: Validator::Bare,
};

pub(super) const UPTIME: ReadonlySpec = ReadonlySpec {
    command: "uptime",
    validator: Validator::Bare,
};

pub(super) const VM_STAT: ReadonlySpec = ReadonlySpec {
    command: "vm_stat",
    validator: Validator::Bare,
};

pub(super) const NPROC: ReadonlySpec = ReadonlySpec {
    command: "nproc",
    validator: Validator::Bare,
};

pub(super) const SW_VERS: ReadonlySpec = ReadonlySpec {
    command: "sw_vers",
    validator: Validator::Bare,
};

pub(super) const ARCH: ReadonlySpec = ReadonlySpec {
    command: "arch",
    validator: Validator::Bare,
};

pub(super) const TTY: ReadonlySpec = ReadonlySpec {
    command: "tty",
    validator: Validator::Bare,
};

pub(super) const GROUPS: ReadonlySpec = ReadonlySpec {
    command: "groups",
    validator: Validator::Bare,
};

// ── Flag-only commands (no positional args) ──
pub(super) const UNAME: ReadonlySpec = ReadonlySpec {
    command: "uname",
    validator: Validator::Generic(GenericSpec {
        short_flags: "amnprsv",
        long_flags: &[],
        value_flags: &[],
        deny_flags: &[],
        path_mode: PathMode::None,
        bare_number_max: 0,
    }),
};

pub(super) const ID: ReadonlySpec = ReadonlySpec {
    command: "id",
    validator: Validator::Generic(GenericSpec {
        short_flags: "ugGnr",
        long_flags: &[],
        value_flags: &[],
        deny_flags: &[],
        path_mode: PathMode::None,
        bare_number_max: 0,
    }),
};

pub(super) const FREE: ReadonlySpec = ReadonlySpec {
    command: "free",
    validator: Validator::Generic(GenericSpec {
        short_flags: "bmghtlsw",
        long_flags: &["--human", "--bytes", "--kilo", "--mega", "--giga", "--tera"],
        value_flags: &[],
        deny_flags: &[],
        path_mode: PathMode::None,
        bare_number_max: 0,
    }),
};

pub(super) const XCODE_SELECT: ReadonlySpec = ReadonlySpec {
    command: "xcode-select",
    validator: Validator::Generic(GenericSpec {
        short_flags: "p",
        long_flags: &["--print-path"],
        value_flags: &[],
        deny_flags: &[],
        path_mode: PathMode::None,
        bare_number_max: 0,
    }),
};

// ── Flags + required path args ──
pub(super) const LS: ReadonlySpec = ReadonlySpec {
    command: "ls",
    validator: Validator::Generic(GenericSpec {
        short_flags: "1AFGHLRSadhlrt",
        long_flags: &[],
        value_flags: &[],
        deny_flags: &[],
        path_mode: PathMode::Optional,
        bare_number_max: 0,
    }),
};

pub(super) const CAT: ReadonlySpec = ReadonlySpec {
    command: "cat",
    validator: Validator::Generic(GenericSpec {
        short_flags: "nbs",
        long_flags: &[],
        value_flags: &[],
        deny_flags: &[],
        path_mode: PathMode::Required,
        bare_number_max: 0,
    }),
};

pub(super) const WC: ReadonlySpec = ReadonlySpec {
    command: "wc",
    validator: Validator::Generic(GenericSpec {
        short_flags: "lwcmL",
        long_flags: &[
            "--lines",
            "--words",
            "--chars",
            "--bytes",
            "--max-line-length",
        ],
        value_flags: &[],
        deny_flags: &[],
        path_mode: PathMode::Required,
        bare_number_max: 0,
    }),
};

pub(super) const FILE: ReadonlySpec = ReadonlySpec {
    command: "file",
    validator: Validator::Generic(GenericSpec {
        short_flags: "biLNpz",
        long_flags: &["--mime", "--mime-type", "--brief"],
        value_flags: &[],
        deny_flags: &[],
        path_mode: PathMode::Required,
        bare_number_max: 0,
    }),
};

pub(super) const STAT: ReadonlySpec = ReadonlySpec {
    command: "stat",
    validator: Validator::Generic(GenericSpec {
        short_flags: "fLt",
        long_flags: &["--format", "--printf", "--terse"],
        value_flags: &[("-c", None)],
        deny_flags: &[],
        path_mode: PathMode::Required,
        bare_number_max: 0,
    }),
};

pub(super) const DIFF: ReadonlySpec = ReadonlySpec {
    command: "diff",
    validator: Validator::Generic(GenericSpec {
        short_flags: "qrsuyNaibBwEZ",
        long_flags: &[
            "--brief",
            "--unified",
            "--color",
            "--color=auto",
            "--color=always",
            "--color=never",
            "--stat",
            "--no-dereference",
        ],
        value_flags: &[("-U", Some(10_000))],
        deny_flags: &[],
        path_mode: PathMode::Required,
        bare_number_max: 0,
    }),
};

pub(super) const COMM: ReadonlySpec = ReadonlySpec {
    command: "comm",
    validator: Validator::Generic(GenericSpec {
        short_flags: "123i",
        long_flags: &[],
        value_flags: &[],
        deny_flags: &[],
        path_mode: PathMode::Required,
        bare_number_max: 0,
    }),
};

pub(super) const MD5SUM: ReadonlySpec = ReadonlySpec {
    command: "md5sum",
    validator: Validator::Generic(GenericSpec {
        short_flags: "bct",
        long_flags: &["--check", "--tag"],
        value_flags: &[],
        deny_flags: &[],
        path_mode: PathMode::Required,
        bare_number_max: 0,
    }),
};

pub(super) const SHASUM: ReadonlySpec = ReadonlySpec {
    command: "shasum",
    validator: Validator::Generic(GenericSpec {
        short_flags: "bcta",
        long_flags: &["--check", "--tag"],
        value_flags: &[("-a", None)],
        deny_flags: &[],
        path_mode: PathMode::Required,
        bare_number_max: 0,
    }),
};

pub(super) const SHA256SUM: ReadonlySpec = ReadonlySpec {
    command: "sha256sum",
    validator: Validator::Generic(GenericSpec {
        short_flags: "bct",
        long_flags: &["--check", "--tag"],
        value_flags: &[],
        deny_flags: &[],
        path_mode: PathMode::Required,
        bare_number_max: 0,
    }),
};

pub(super) const REALPATH: ReadonlySpec = ReadonlySpec {
    command: "realpath",
    validator: Validator::Generic(GenericSpec {
        short_flags: "eLmPqsz",
        long_flags: &[
            "--canonicalize-existing",
            "--canonicalize-missing",
            "--no-symlinks",
        ],
        value_flags: &[],
        deny_flags: &[],
        path_mode: PathMode::Required,
        bare_number_max: 0,
    }),
};

pub(super) const READLINK: ReadonlySpec = ReadonlySpec {
    command: "readlink",
    validator: Validator::Generic(GenericSpec {
        short_flags: "fenqsz",
        long_flags: &["--canonicalize", "--canonicalize-existing", "--no-newline"],
        value_flags: &[],
        deny_flags: &[],
        path_mode: PathMode::Required,
        bare_number_max: 0,
    }),
};

// ── Flags + optional path args ──
pub(super) const DU: ReadonlySpec = ReadonlySpec {
    command: "du",
    validator: Validator::Generic(GenericSpec {
        short_flags: "shkcmgabxlLHP",
        long_flags: &[
            "--human-readable",
            "--summarize",
            "--total",
            "--apparent-size",
            "--bytes",
            "--one-file-system",
        ],
        value_flags: &[("-d", Some(20)), ("--max-depth", Some(20))],
        deny_flags: &[],
        path_mode: PathMode::Optional,
        bare_number_max: 0,
    }),
};

pub(super) const DF: ReadonlySpec = ReadonlySpec {
    command: "df",
    validator: Validator::Generic(GenericSpec {
        short_flags: "hHkmgPTil",
        long_flags: &[],
        value_flags: &[],
        deny_flags: &["--output"],
        path_mode: PathMode::Optional,
        bare_number_max: 0,
    }),
};

pub(super) const SORT: ReadonlySpec = ReadonlySpec {
    command: "sort",
    validator: Validator::Generic(GenericSpec {
        short_flags: "bdfiMnrRsugkt",
        long_flags: &[
            "--numeric-sort",
            "--reverse",
            "--unique",
            "--stable",
            "--human-numeric-sort",
            "--check",
        ],
        value_flags: &[],
        deny_flags: &["-o", "--output"],
        path_mode: PathMode::Optional,
        bare_number_max: 0,
    }),
};

pub(super) const UNIQ: ReadonlySpec = ReadonlySpec {
    command: "uniq",
    validator: Validator::Generic(GenericSpec {
        short_flags: "cduifszw",
        long_flags: &["--count", "--repeated", "--unique", "--ignore-case"],
        value_flags: &[],
        deny_flags: &[],
        path_mode: PathMode::Optional,
        bare_number_max: 0,
    }),
};

pub(super) const CUT: ReadonlySpec = ReadonlySpec {
    command: "cut",
    validator: Validator::Generic(GenericSpec {
        short_flags: "s",
        long_flags: &["--complement", "--only-delimited"],
        value_flags: &[
            ("-d", None),
            ("-f", None),
            ("-c", None),
            ("-b", None),
            ("--delimiter", None),
            ("--fields", None),
            ("--characters", None),
            ("--bytes", None),
        ],
        deny_flags: &["--output-delimiter"],
        path_mode: PathMode::Optional,
        bare_number_max: 0,
    }),
};

pub(super) const FOLD: ReadonlySpec = ReadonlySpec {
    command: "fold",
    validator: Validator::Generic(GenericSpec {
        short_flags: "bs",
        long_flags: &["--bytes", "--spaces"],
        value_flags: &[("-w", Some(10_000)), ("--width", Some(10_000))],
        deny_flags: &[],
        path_mode: PathMode::Optional,
        bare_number_max: 0,
    }),
};

pub(super) const EXPAND: ReadonlySpec = ReadonlySpec {
    command: "expand",
    validator: Validator::Generic(GenericSpec {
        short_flags: "i",
        long_flags: &["--initial"],
        value_flags: &[("-t", None), ("--tabs", None)],
        deny_flags: &[],
        path_mode: PathMode::Optional,
        bare_number_max: 0,
    }),
};

pub(super) const UNEXPAND: ReadonlySpec = ReadonlySpec {
    command: "unexpand",
    validator: Validator::Generic(GenericSpec {
        short_flags: "a",
        long_flags: &["--all", "--first-only"],
        value_flags: &[("-t", None), ("--tabs", None)],
        deny_flags: &[],
        path_mode: PathMode::Optional,
        bare_number_max: 0,
    }),
};

// ── Unchecked positional args ──
pub(super) const WHICH: ReadonlySpec = ReadonlySpec {
    command: "which",
    validator: Validator::Generic(GenericSpec {
        short_flags: "a",
        long_flags: &[],
        value_flags: &[],
        deny_flags: &[],
        path_mode: PathMode::Unchecked,
        bare_number_max: 0,
    }),
};

pub(super) const DIRNAME: ReadonlySpec = ReadonlySpec {
    command: "dirname",
    validator: Validator::Generic(GenericSpec {
        short_flags: "z",
        long_flags: &["--zero"],
        value_flags: &[],
        deny_flags: &[],
        path_mode: PathMode::Unchecked,
        bare_number_max: 0,
    }),
};

pub(super) const BASENAME: ReadonlySpec = ReadonlySpec {
    command: "basename",
    validator: Validator::Generic(GenericSpec {
        short_flags: "az",
        long_flags: &["--multiple", "--zero"],
        value_flags: &[("-s", None), ("--suffix", None)],
        deny_flags: &[],
        path_mode: PathMode::Unchecked,
        bare_number_max: 0,
    }),
};

pub(super) const PRINTENV: ReadonlySpec = ReadonlySpec {
    command: "printenv",
    validator: Validator::Generic(GenericSpec {
        short_flags: "0",
        long_flags: &["--null"],
        value_flags: &[],
        deny_flags: &[],
        path_mode: PathMode::Unchecked,
        bare_number_max: 0,
    }),
};

pub(super) const ECHO: ReadonlySpec = ReadonlySpec {
    command: "echo",
    validator: Validator::Generic(GenericSpec {
        short_flags: "neE",
        long_flags: &[],
        value_flags: &[],
        deny_flags: &[],
        path_mode: PathMode::Unchecked,
        bare_number_max: 0,
    }),
};

pub(super) const TR: ReadonlySpec = ReadonlySpec {
    command: "tr",
    validator: Validator::Generic(GenericSpec {
        short_flags: "cCds",
        long_flags: &["--complement", "--delete", "--squeeze-repeats"],
        value_flags: &[],
        deny_flags: &[],
        path_mode: PathMode::Unchecked,
        bare_number_max: 0,
    }),
};

// ── Process inspection ──
pub(super) const PGREP: ReadonlySpec = ReadonlySpec {
    command: "pgrep",
    validator: Validator::Generic(GenericSpec {
        short_flags: "cdfgilnotuUvxP",
        long_flags: &[
            "--count",
            "--full",
            "--list-name",
            "--list-full",
            "--newest",
            "--oldest",
            "--inverse",
        ],
        value_flags: &[
            ("-G", None),
            ("-P", None),
            ("-s", None),
            ("-u", None),
            ("-U", None),
        ],
        deny_flags: &[],
        path_mode: PathMode::Unchecked,
        bare_number_max: 0,
    }),
};
