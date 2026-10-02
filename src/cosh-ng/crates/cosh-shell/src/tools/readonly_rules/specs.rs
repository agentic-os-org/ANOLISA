pub struct ReadonlySpec {
    pub command: &'static str,
    pub validator: Validator,
}

pub enum Validator {
    Bare,
    Generic(GenericSpec),
    Subcommand(SubcommandSpec),
    VersionCheck(&'static [&'static str]),
    Custom(fn(&[String]) -> bool),
}

pub struct GenericSpec {
    pub short_flags: &'static str,
    pub long_flags: &'static [&'static str],
    pub value_flags: &'static [(&'static str, Option<u32>)],
    pub deny_flags: &'static [&'static str],
    pub path_mode: PathMode,
    pub bare_number_max: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathMode {
    None,
    Optional,
    Required,
    Unchecked,
}

pub struct SubcommandSpec {
    pub deny_args: &'static [&'static str],
    pub subcommands: &'static [(&'static str, Validator)],
}

// ── Registry ──

mod basic;
mod custom;
mod subcommands;
mod versions;

pub static READONLY_SPECS: &[ReadonlySpec] = &[
    basic::PWD,
    basic::WHOAMI,
    basic::HOSTNAME,
    basic::DATE,
    basic::UPTIME,
    basic::VM_STAT,
    basic::NPROC,
    basic::SW_VERS,
    basic::ARCH,
    basic::TTY,
    basic::GROUPS,
    basic::UNAME,
    basic::ID,
    basic::FREE,
    basic::XCODE_SELECT,
    basic::LS,
    basic::CAT,
    basic::WC,
    basic::FILE,
    basic::STAT,
    basic::DIFF,
    basic::COMM,
    basic::MD5SUM,
    basic::SHASUM,
    basic::SHA256SUM,
    basic::REALPATH,
    basic::READLINK,
    basic::DU,
    basic::DF,
    basic::SORT,
    basic::UNIQ,
    basic::CUT,
    basic::FOLD,
    basic::EXPAND,
    basic::UNEXPAND,
    basic::WHICH,
    basic::DIRNAME,
    basic::BASENAME,
    basic::PRINTENV,
    basic::ECHO,
    basic::TR,
    basic::PGREP,
    versions::RUSTC,
    versions::RUSTUP,
    versions::NODE,
    versions::NPM,
    versions::PYTHON,
    versions::PYTHON3,
    versions::PIP,
    versions::PIP3,
    versions::JAVA,
    versions::JAVAC,
    versions::RUBY,
    versions::SWIFT,
    versions::CLANG,
    versions::GCC,
    versions::G_PLUS_PLUS,
    versions::CMAKE,
    versions::MAKE,
    versions::DOTNET,
    custom::HEAD,
    custom::TAIL,
    custom::GREP,
    custom::RG,
    custom::FIND,
    custom::PS,
    custom::SYSCTL,
    custom::TOP,
    custom::ENV,
    subcommands::GIT,
    subcommands::CARGO,
    subcommands::DOCKER,
    subcommands::KUBECTL,
    subcommands::GO,
    subcommands::DISKUTIL,
    subcommands::DEFAULTS,
    subcommands::XCRUN,
    subcommands::SYSTEM_PROFILER,
];
