//! Token-level launcher-chain walker for the command-risk classifier
//! (#2064): resolves sudo/env/timeout-style wrappers to their payload so
//! a wrapped irrecoverable command keeps its verdict.

use super::command_risk::{InteractionRequirement, SideEffectClass};
use super::command_risk_build::{basename, high_risk_program};
use super::command_risk_carried::classify_carried_command;
use super::command_risk_parser::is_env_assignment;

/// Table-driven launcher-chain walker (#2064 review circuit-breaker:
/// two point-fix rounds kept leaking new wrapper forms — first the bare
/// launchers, then their option-value forms — so the design is rebuilt
/// around a complete option-arity contract instead of a name list).
///
/// Invariants:
/// - I1 upgrade-only: the walk can only raise impact or tighten the
///   verdict, never produce an auto-allow or weaken an existing one.
/// - I2 declared arity: each launcher declares its value-taking options,
///   plain flags, query options, positional slots and whether it accepts
///   `NAME=VALUE`; unknown options or missing values yield Unresolved,
///   never a silently skipped guess.
/// - I3 escalation is never dropped once the chain passed sudo/su/doas.
/// - I4 query forms (`command -v/-V`, `sudo -l`) execute nothing: the
///   chain ends at the wrapper itself, keeping the wrapper verdict.
/// - I5 an unresolvable chain still upgrades to High/no-trust; High
///   alone keeps Always trust off the approval card, and SystemControl
///   stays untagged because the payload program is unconfirmed.
/// - I6 the walk index strictly increases, so the walk always ends.
/// - I7 rest-command carriers (`eval`) treat every remaining token as
///   the payload command string: nothing is parsed as an option, and a
///   carrier with no payload stays Unresolved.
struct LauncherSpec {
    /// sudo/su/doas: the chain gains the privilege-escalation nature.
    escalates: bool,
    /// Options consuming a separate or inline value (`-u root`,
    /// `--user=root`, glued `-uroot`).
    value_flags: &'static [&'static str],
    /// Valueless options; short forms may combine (`-ES`).
    flags: &'static [&'static str],
    /// Lookup/list forms that run no payload (`command -v`, `sudo -l`).
    query_flags: &'static [&'static str],
    /// Positional arguments between options and the payload program
    /// (timeout's duration, su's user).
    positional: usize,
    /// Whether `NAME=VALUE` tokens may precede the payload.
    env_assignments: bool,
    /// Options whose value is the payload command string itself
    /// (`su -c reboot`): the whole value is classified, including its
    /// compound segments and nested launchers.
    command_flags: &'static [&'static str],
    /// Every remaining token forms the payload command string (`eval`);
    /// option parsing never applies (I7).
    rest_command: bool,
}

/// Interpreters that act as execution carriers only when they carry
/// inline code: a bare `sh` (e.g. the tail of `curl … | sh`) reads its
/// program from stdin, so the caller's interpreter classification must
/// apply instead of an Unresolved chain verdict.
const SHELL_CARRIER_PROGRAMS: &[&str] = &["sh", "bash", "dash", "zsh"];

const fn spec(
    escalates: bool,
    value_flags: &'static [&'static str],
    flags: &'static [&'static str],
    query_flags: &'static [&'static str],
    positional: usize,
    env_assignments: bool,
    command_flags: &'static [&'static str],
    rest_command: bool,
) -> LauncherSpec {
    LauncherSpec {
        escalates,
        value_flags,
        flags,
        query_flags,
        positional,
        env_assignments,
        command_flags,
        rest_command,
    }
}

/// Shared option contract for POSIX-shell interpreters used as execution
/// carriers (`sh -c reboot`, `bash -c reboot`). `-c` carries the payload
/// command string; unknown options leave the chain Unresolved rather than
/// guessing past them (I2/I5).
const SHELL_CARRIER: LauncherSpec = spec(
    false,
    // `-o` names a set option; bash's `-O` names a shopt option — both
    // consume the next word as their value.
    &["-o", "-O"],
    &[
        "-e",
        "-u",
        "-x",
        "-n",
        "-v",
        "-i",
        "-l",
        "-m",
        "-s",
        "-p",
        "-f",
        "-b",
        "-h",
        "-a",
        "-C",
        "-E",
        "-T",
        "-B",
        "--posix",
        "--norc",
        "--noprofile",
        "--login",
        "--restricted",
        "--verbose",
    ],
    &["--version", "--help"],
    0,
    false,
    &["-c", "--command"],
    false,
);

const LAUNCHERS: &[(&str, LauncherSpec)] = &[
    (
        "sudo",
        spec(
            true,
            &[
                "-u",
                "--user",
                "-g",
                "--group",
                "-h",
                "--host",
                "-p",
                "--prompt",
                "-C",
                "--close-from",
                "-c",
                "--login-class",
                "-r",
                "--role",
                "-t",
                "--type",
                "-T",
                "--command-timeout",
                "-U",
                "--other-user",
                "-D",
                "--chdir",
                "-R",
                "--chroot",
            ],
            &[
                "-E",
                "--preserve-env",
                "-H",
                "--set-home",
                "-i",
                "--login",
                "-s",
                "--shell",
                "-k",
                "--reset-timestamp",
                "-K",
                "--remove-timestamp",
                "-n",
                "--non-interactive",
                "-S",
                "--stdin",
                "-b",
                "--background",
                "-v",
                "--validate",
                "-A",
                "--askpass",
            ],
            // `-l`/`--list` and `-e`/`--edit` run no payload command.
            &["-l", "--list", "-e", "--edit"],
            0,
            true,
            &[],
            false,
        ),
    ),
    (
        "su",
        spec(
            true,
            &["-s", "--shell"],
            &["-l", "--login", "-m", "-p", "--preserve-environment", "-"],
            &[],
            1,
            false,
            &["-c", "--command"],
            false,
        ),
    ),
    (
        "doas",
        spec(
            true,
            &["-u", "-C"],
            &["-n", "-s", "-S", "-k", "-L"],
            &[],
            0,
            false,
            &[],
            false,
        ),
    ),
    (
        "command",
        spec(false, &[], &["-p"], &["-v", "-V"], 0, false, &[], false),
    ),
    (
        "env",
        spec(
            false,
            // `-S`/`--split-string` is deliberately undeclared: its value
            // is a nested command string with independent quoting, so the
            // chain must land on Unresolved rather than a guessed payload.
            &["-u", "--unset", "-C", "--chdir"],
            // `-` alone is the legacy `-i`; the signal options take an
            // optional value in `=` form only, so they stay plain flags.
            &[
                "-i",
                "--ignore-environment",
                "-0",
                "--null",
                "-",
                "--list-signal-handling",
                "--block-signal",
                "--default-signal",
                "--ignore-signal",
            ],
            &[],
            0,
            true,
            &[],
            false,
        ),
    ),
    ("nohup", spec(false, &[], &[], &[], 0, false, &[], false)),
    (
        "setsid",
        spec(
            false,
            &[],
            &["-c", "--ctty", "-f", "--fork", "-w", "--wait"],
            &[],
            0,
            false,
            &[],
            false,
        ),
    ),
    (
        "nice",
        spec(
            false,
            &["-n", "--adjustment"],
            &[],
            &[],
            0,
            false,
            &[],
            false,
        ),
    ),
    (
        "stdbuf",
        spec(
            false,
            &["-i", "--input", "-o", "--output", "-e", "--error"],
            &[],
            &[],
            0,
            false,
            &[],
            false,
        ),
    ),
    (
        "timeout",
        spec(
            false,
            &["-k", "--kill-after", "-s", "--signal"],
            &["--preserve-status", "-f", "--foreground", "-v", "--verbose"],
            &[],
            1,
            false,
            &[],
            false,
        ),
    ),
    (
        "xargs",
        spec(
            false,
            &[
                "-d",
                "--delimiter",
                "-E",
                "--eof",
                "-e",
                "-I",
                "--replace",
                "-i",
                "-L",
                "--max-lines",
                "-l",
                "-n",
                "--max-args",
                "-P",
                "--max-procs",
                "-s",
                "--max-chars",
                "-a",
                "--arg-file",
            ],
            &[
                "-0",
                "--null",
                "-p",
                "--interactive",
                "-t",
                "--verbose",
                "-r",
                "--no-run-if-empty",
                "-x",
                "--exit",
                "-o",
                "--open-tty",
            ],
            &[],
            0,
            false,
            &[],
            false,
        ),
    ),
    // The applet name is the first positional token.
    ("busybox", spec(false, &[], &[], &[], 0, false, &[], false)),
    // Replaces the shell process: the next program token is the payload.
    // `-a` takes a name value; undeclared options stay Unresolved (I2).
    (
        "exec",
        spec(false, &["-a"], &["-c", "-l"], &[], 0, false, &[], false),
    ),
    // Joins its arguments into a shell command string and executes it;
    // everything after the carrier is the payload (I7).
    ("eval", spec(false, &[], &[], &[], 0, false, &[], true)),
    // Shell keyword or `/usr/bin/time` prefix: the next program token is
    // the payload; `-o`/`-f` carry file/format values.
    (
        "time",
        spec(
            false,
            &["-o", "-f", "--output", "--format"],
            &[
                "-p",
                "--portability",
                "-v",
                "--verbose",
                "-q",
                "--quiet",
                "-a",
                "--append",
            ],
            &[],
            0,
            false,
            &[],
            false,
        ),
    ),
    // `-c` carries an inline command string classified in full.
    ("sh", SHELL_CARRIER),
    ("bash", SHELL_CARRIER),
    ("dash", SHELL_CARRIER),
    ("zsh", SHELL_CARRIER),
];

/// Outcome of walking a launcher chain to its payload program.
pub(super) enum LauncherWalk {
    /// The chain resolves to a system-control program.
    SystemControl { escalated: bool },
    /// The chain resolves to another program: a high-risk entry when
    /// one matches, `None` for an ordinary program (or a query form,
    /// which ends the chain at the wrapper itself).
    Other {
        escalated: bool,
        high: Option<(SideEffectClass, &'static str, InteractionRequirement)>,
    },
    /// A launcher was seen but the chain cannot be fully parsed
    /// (unknown option, missing option value, or no payload program).
    Unresolved { escalated: bool },
}

enum ChainStep {
    /// Advance to `tokens[index]` as the next program candidate.
    Next(usize),
    /// A command-flag value carries the payload (`su -c reboot`).
    Command(String),
    /// Query semantics: the chain ends at the wrapper itself.
    Query,
    /// Unknown option, missing value, or no payload before chain end.
    Unresolved,
}

fn scan_launcher_options(tokens: &[String], mut index: usize, spec: &LauncherSpec) -> ChainStep {
    let mut positional = spec.positional;
    while let Some(token) = tokens.get(index) {
        if spec.rest_command {
            // A special builtin still honours the option terminator
            // (`eval -- reboot` runs reboot), so consume one leading
            // `--`; everything after is payload code (I7): `eval -x`
            // runs a command named `-x`, so nothing else may parse as
            // an option. An exhausted remainder keeps the carrier
            // Unresolved.
            let rest = if token == "--" { index + 1 } else { index };
            return match tokens.get(rest) {
                Some(_) => ChainStep::Command(tokens[rest..].join(" ")),
                None => ChainStep::Unresolved,
            };
        }
        if token == "--" {
            index += 1;
            // Everything after `--` is positional: consume the declared
            // slots before handing the remainder to the payload scan, so
            // `timeout -- 5 reboot` still walks past the duration.
            while positional > 0 && tokens.get(index).is_some() {
                positional -= 1;
                index += 1;
            }
            break;
        }
        if spec.env_assignments && is_env_assignment(token) {
            index += 1;
            continue;
        }
        if token == "-" && spec.flags.contains(&"-") {
            index += 1;
            continue;
        }
        if token.starts_with('-') && token.len() > 1 {
            let long = token.starts_with("--");
            let name = token.split('=').next().unwrap_or(token.as_str());
            if spec.query_flags.contains(&name) {
                return ChainStep::Query;
            }
            if spec.command_flags.contains(&name) {
                if let Some((_, inline)) = token.split_once('=') {
                    return ChainStep::Command(inline.to_string());
                }
                return match tokens.get(index + 1) {
                    Some(value) => ChainStep::Command(value.clone()),
                    None => ChainStep::Unresolved,
                };
            }
            if spec.value_flags.contains(&name) {
                if token.contains('=') {
                    index += 1;
                    continue;
                }
                if tokens.get(index + 1).is_none() {
                    return ChainStep::Unresolved;
                }
                index += 2;
                continue;
            }
            if spec.flags.contains(&name) {
                index += 1;
                continue;
            }
            if !long {
                // Glued value (`-o0`, `-uroot`): a value-flag prefix.
                let glued = spec
                    .value_flags
                    .iter()
                    .any(|flag| token.starts_with(flag) && token.len() > flag.len());
                // Combined valueless shorts (`-ES`): every character
                // must be a known plain flag.
                let combined = token.len() > 2
                    && token[1..]
                        .chars()
                        .all(|c| spec.flags.contains(&format!("-{c}").as_str()));
                if glued || combined {
                    index += 1;
                    continue;
                }
            }
            return ChainStep::Unresolved;
        }
        if positional > 0 {
            positional -= 1;
            index += 1;
            continue;
        }
        break;
    }
    if index >= tokens.len() {
        return ChainStep::Unresolved;
    }
    ChainStep::Next(index)
}

/// Whole-machine `systemctl` verbs and their `.target` unit forms: they
/// end the host, not one service, so the generic service-control entry
/// understates the blast radius (#2064 round 7). Scanning every
/// argument overmatches query forms (`systemctl status reboot.target`),
/// which errs toward prompting — the fail-closed direction.
const SYSTEMCTL_MACHINE_ARGS: &[&str] = &[
    "reboot",
    "poweroff",
    "halt",
    "kexec",
    "exit",
    "suspend",
    "hibernate",
    "hybrid-sleep",
    "suspend-then-hibernate",
];

fn systemctl_targets_machine(program: &str, args: &[String]) -> bool {
    program == "systemctl"
        && args.iter().any(|arg| {
            SYSTEMCTL_MACHINE_ARGS.contains(&arg.as_str())
                || arg
                    .strip_suffix(".target")
                    .is_some_and(|unit| SYSTEMCTL_MACHINE_ARGS.contains(&unit))
        })
}

/// True when any token carries the spec's inline-code flag, either
/// standalone (`-c`), assignment-form (`--command=…`), or clustered
/// inside combined shorts (`-ec`). Detection only — the option scan
/// still decides the payload, and a cluster it cannot resolve stays
/// Unresolved rather than guessed past (I2/I5).
fn tokens_carry_inline_code_flag(tokens: &[String], spec: &LauncherSpec) -> bool {
    tokens.iter().any(|token| {
        let name = token.split('=').next().unwrap_or(token.as_str());
        if spec.command_flags.contains(&name) {
            return true;
        }
        token.starts_with('-')
            && !token.starts_with("--")
            && token.len() > 2
            && token[1..]
                .chars()
                .any(|c| spec.command_flags.contains(&format!("-{c}").as_str()))
    })
}

/// Walks the token list from the first non-assignment token through any
/// launcher chain. Returns `None` when the first token is neither a
/// high-risk program nor a known launcher — the caller's existing path
/// applies unchanged.
pub(super) fn walk_launcher_chain(tokens: &[String]) -> Option<LauncherWalk> {
    let start = tokens.iter().position(|token| !is_env_assignment(token))?;
    let mut index = start;
    let mut escalated = false;
    loop {
        let program = basename(tokens.get(index)?);
        // Launcher check first: sudo/su sit in both tables, and their
        // high-risk entry is the *fallback* verdict for when the payload
        // is ordinary or unresolvable — matching it here would mask a
        // wrapped system-control payload behind the wrapper's own
        // privilege-escalation nature.
        let Some((_, spec)) = LAUNCHERS.iter().find(|(name, _)| *name == program) else {
            // Whole-machine systemctl invocations outrank the generic
            // service-control entry from `high_risk_program` (#2064).
            if systemctl_targets_machine(program, &tokens[index + 1..]) {
                return Some(LauncherWalk::SystemControl { escalated });
            }
            let high = high_risk_program(program);
            // Only a zero-link chain (direct high-risk program) or a
            // walked chain keeps the walk; a first-token ordinary
            // program returns None so the caller's path applies (I3).
            if index == start && high.is_none() {
                return None;
            }
            return Some(match high {
                Some(high) if high.0 == SideEffectClass::SystemControl => {
                    LauncherWalk::SystemControl { escalated }
                }
                high => LauncherWalk::Other { escalated, high },
            });
        };
        if SHELL_CARRIER_PROGRAMS.contains(&program)
            && !tokens_carry_inline_code_flag(&tokens[index + 1..], spec)
        {
            // A shell without an inline-code flag runs its program from
            // stdin or a script file, not from the remaining tokens:
            // end the walk as if the interpreter were an ordinary
            // program so the caller's interpreter path applies (I1).
            return if index == start {
                None
            } else {
                Some(LauncherWalk::Other {
                    escalated,
                    high: None,
                })
            };
        }
        escalated |= spec.escalates;
        match scan_launcher_options(tokens, index + 1, spec) {
            ChainStep::Next(next) => index = next,
            ChainStep::Command(command) => {
                // An empty command value (`su -c ""`) still passed an
                // escalating launcher: keep the chain Unresolved so the
                // verdict cannot drop below the wrapper's own high-risk
                // level (I1/I3).
                if command.split_whitespace().next().is_none() {
                    return Some(LauncherWalk::Unresolved { escalated });
                }
                // Judge the whole carried command — compound segments,
                // pipeline stages, and nested launchers — not just its
                // first word, which would let `sh -c 'echo ok; reboot'`
                // hide the payload behind a benign prefix (#2064).
                return Some(classify_carried_command(&command, escalated));
            }
            ChainStep::Query => {
                return Some(LauncherWalk::Other {
                    escalated,
                    high: None,
                });
            }
            ChainStep::Unresolved => return Some(LauncherWalk::Unresolved { escalated }),
        }
    }
}
