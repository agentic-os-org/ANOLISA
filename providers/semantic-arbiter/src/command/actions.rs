//! Candidate command syntax, not execution, authorization, or complete shell effects.

use semantic_arbiter::local_policy::ActionKind;

use super::{assignment, walk, Word, READ_ONLY, WRAPPERS};
use crate::sensitive;
use ActionKind::*;

/// Observe static simple argv using the same lexical walk as write projection.
pub(crate) fn observe(command: &str, workspace: &str) -> Vec<ActionKind> {
    let mut kinds = vec![];
    let analysis = walk(command, workspace, |words| simple(words, &mut kinds));
    if analysis.unresolved {
        add(&mut kinds, OpaqueExecution);
    } else if analysis.targets.is_empty() && !kinds.contains(&OpaqueExecution) {
        add(&mut kinds, ReadOnly);
    }
    kinds
}

fn add(kinds: &mut Vec<ActionKind>, kind: ActionKind) {
    if !kinds.contains(&kind) {
        kinds.push(kind);
    }
}

fn simple(words: &[Word], kinds: &mut Vec<ActionKind>) {
    let words = words
        .iter()
        .skip_while(|word| assignment(word))
        .collect::<Vec<_>>();
    let Some((program, rest)) = words.split_first() else {
        return;
    };
    if words.iter().any(|word| word.dynamic) {
        add(kinds, OpaqueExecution);
        return;
    }
    let name = program.text.rsplit('/').next().unwrap_or_default();
    if WRAPPERS.contains(&name) {
        add(kinds, OpaqueExecution);
        return;
    }
    let args: Vec<_> = rest.iter().map(|word| word.text.as_str()).collect();
    match name {
        // Mirror the write projection: a lone version/help query prints and exits.
        "rustc" | "cargo" | "python" | "python3" | "pip" | "pip3" | "npm" | "node"
            if matches!(args.as_slice(), ["--version" | "--help" | "-V" | "-h"]) => {}
        "git" => git(&args, kinds),
        "pip" | "pip3" | "npm" | "cargo" => dependencies(name, &args, kinds),
        "gh" => {
            if matches!(args.as_slice(), ["pr", "comment" | "create", ..]) {
                add(kinds, ExternalCommunication);
            }
        }
        "curl" => curl(&args, kinds),
        "wget" => {
            if !args.is_empty() && !query_only(&args) {
                add(kinds, ExternalCode);
            }
        }
        "ping" | "ping6" | "traceroute" | "traceroute6" | "tracepath" | "nmap" => {
            if !args.is_empty() && !query_only(&args) {
                add(kinds, NetworkProbe);
            }
        }
        "crontab" => {
            let opts = options(&args, &["-u"]);
            let listing = opts.iter().any(|(key, _)| *key == "-l")
                && !opts.iter().any(|(key, _)| matches!(*key, "-e" | "-r"));
            if !args.is_empty() && !query_only(&args) && !listing {
                add(kinds, Persistence);
            }
        }
        "systemctl" => {
            let args = args
                .strip_prefix(&["--user"])
                .or_else(|| args.strip_prefix(&["--system"]))
                .unwrap_or(&args);
            if args.first() == Some(&"enable") {
                add(kinds, Persistence);
            }
        }
        "date" => {
            let opts = options(
                &args,
                &[
                    "-s",
                    "--set",
                    "-d",
                    "--date",
                    "-r",
                    "--reference",
                    "-f",
                    "--file",
                ],
            );
            if opts.iter().any(|(key, _)| matches!(*key, "-s" | "--set")) {
                add(kinds, ClockChange);
            }
        }
        "timedatectl" if args.first() == Some(&"set-time") => add(kinds, ClockChange),
        "rm" | "rmdir" | "unlink" => add(kinds, Delete),
        "sort" => {
            if sensitive::read_command(name, &args) {
                add(kinds, SensitiveRead);
            }
        }
        _ if READ_ONLY.contains(&name) => {
            if sensitive::read_command(name, &args) {
                add(kinds, SensitiveRead);
            }
        }
        "cd" | "pushd" | "popd" | "touch" | "mkdir" | "tee" | "truncate" | "cp" | "mv" | "ln"
        | "sed" | "find" => {}
        _ => add(kinds, OpaqueExecution),
    }
}

fn query_only(args: &[&str]) -> bool {
    matches!(args, ["--help" | "--version" | "-h" | "-V"])
}

// Consume known option values before looking for action flags. Text in a message,
// filter, URL or data argument must not masquerade as an option.
fn options<'a>(args: &'a [&'a str], valued: &[&str]) -> Vec<(&'a str, Option<&'a str>)> {
    let mut found = vec![];
    let mut index = 0;
    while let Some(&arg) = args.get(index) {
        index += 1;
        if arg == "--" {
            break;
        }
        if !arg.starts_with('-') {
            continue;
        }
        if let Some((key, value)) = arg.split_once('=') {
            found.push((key, Some(value)));
        } else if valued.contains(&arg) {
            found.push((arg, args.get(index).copied()));
            index += 1;
        } else if let Some(key) = valued
            .iter()
            .find(|key| key.len() == 2 && arg.starts_with(**key) && arg.len() > 2)
        {
            found.push((&arg[..key.len()], Some(&arg[key.len()..])));
        } else {
            found.push((arg, None));
        }
    }
    found
}

fn git(args: &[&str], kinds: &mut Vec<ActionKind>) {
    let mut index = 0;
    while let Some(&arg) = args.get(index) {
        match arg {
            "-C" | "-c" | "--git-dir" | "--work-tree" => index += 2,
            "--no-pager" | "--no-optional-locks" | "-P" => index += 1,
            text if text.starts_with("--git-dir=") || text.starts_with("--work-tree=") => {
                index += 1
            }
            text if text.starts_with('-') => return,
            _ => break,
        }
    }
    let Some((&sub, args)) = args.get(index..).and_then(|args| args.split_first()) else {
        return;
    };
    let opts = options(
        args,
        &[
            "-m",
            "--message",
            "-F",
            "--file",
            "--author",
            "--date",
            "--repo",
            "--receive-pack",
            "--exec",
            "-x",
            "--strategy",
            "-s",
            "--strategy-option",
            "-X",
        ],
    );
    let has = |keys: &[&str]| opts.iter().any(|(key, _)| keys.contains(key));
    if sub == "push" {
        add(kinds, GitPublish);
        let short_force = opts.iter().any(|(key, value)| {
            value.is_none()
                && key.starts_with('-')
                && !key.starts_with("--")
                && key[1..].chars().all(|flag| "fnvqu".contains(flag))
                && key[1..].contains('f')
        });
        if short_force || has(&["--force", "--force-with-lease", "--force-if-includes"]) {
            add(kinds, GitRewrite);
        }
    }
    if matches!(
        sub,
        "reset" | "clean" | "rebase" | "filter-branch" | "filter-repo" | "restore" | "checkout"
    ) || (sub == "commit" && has(&["--amend"]))
        || (sub == "reflog" && matches!(args.first(), Some(&"expire" | &"delete" | &"drop")))
        || (sub == "branch" && has(&["-f", "--force", "-D"]))
    {
        add(kinds, GitRewrite);
    }
    let short_flag = |flag: char| {
        opts.iter().any(|(key, value)| {
            value.is_none()
                && key.starts_with('-')
                && !key.starts_with("--")
                && key[1..].contains(flag)
        })
    };
    // Only forms that discard working-tree bytes qualify; index-only or
    // dry-run forms leave current bytes intact and stay with the model.
    let destroys_working_state = match sub {
        "reset" => has(&["--hard"]),
        "clean" => !has(&["-n", "--dry-run"]) && !short_flag('n'),
        "checkout" => args.contains(&"--"),
        "restore" => {
            let staged = has(&["--staged", "-S"]) || short_flag('S');
            let worktree = has(&["--worktree", "-W"]) || short_flag('W');
            !staged || worktree
        }
        _ => false,
    };
    if destroys_working_state {
        add(kinds, WorkingStateDestruction);
    }
    if matches!(sub, "commit" | "push" | "rebase" | "merge" | "cherry-pick")
        && (has(&["--no-verify", "--no-gpg-sign"]) || (sub == "commit" && has(&["-n"])))
    {
        add(kinds, CheckBypass);
    }
}

fn dependencies(name: &str, args: &[&str], kinds: &mut Vec<ActionKind>) {
    let (flags, valued): (&[&str], &[&str]) = match name {
        "pip" | "pip3" => (
            &[
                "-q",
                "--quiet",
                "-v",
                "--verbose",
                "--isolated",
                "--disable-pip-version-check",
            ],
            &["--python", "--proxy", "--timeout"],
        ),
        "npm" => (
            &["-g", "--global", "--offline", "--silent"],
            &["--prefix", "--registry", "--cache"],
        ),
        "cargo" => (
            &[
                "-q",
                "--quiet",
                "-v",
                "--verbose",
                "--offline",
                "--locked",
                "--frozen",
            ],
            &["--color", "--config"],
        ),
        _ => (&[], &[]),
    };
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        if flags.contains(arg)
            || valued.iter().any(|key| {
                arg.strip_prefix(key)
                    .is_some_and(|suffix| suffix.starts_with('='))
            })
        {
            index += 1;
        } else if valued.contains(arg) {
            index += 2;
        } else {
            break;
        }
    }
    let Some((&sub, rest)) = args.get(index..).and_then(|args| args.split_first()) else {
        return;
    };
    let changed = match name {
        "pip" | "pip3" => matches!(sub, "install" | "uninstall"),
        "npm" => matches!(
            sub,
            "install"
                | "i"
                | "ci"
                | "update"
                | "up"
                | "uninstall"
                | "remove"
                | "rm"
                | "link"
                | "dedupe"
        ),
        "cargo" => matches!(
            sub,
            "add" | "remove" | "rm" | "update" | "install" | "uninstall" | "generate-lockfile"
        ),
        _ => false,
    };
    if changed {
        add(kinds, DependencyChange);
    }
    let opts = options(
        rest,
        &[
            "--prefix",
            "--registry",
            "--manifest-path",
            "-r",
            "--requirement",
            "--index-url",
        ],
    );
    if (name == "cargo"
        && matches!(sub, "publish" | "package")
        && opts.iter().any(|(key, _)| *key == "--no-verify"))
        || (name == "npm"
            && changed
            && opts
                .iter()
                .any(|(key, value)| *key == "--ignore-scripts" && *value != Some("false")))
    {
        add(kinds, CheckBypass);
    }
}

fn curl(args: &[&str], kinds: &mut Vec<ActionKind>) {
    if args.is_empty() || query_only(args) {
        return;
    }
    let opts = options(
        args,
        &[
            "-X",
            "--request",
            "-T",
            "--upload-file",
            "-d",
            "--data",
            "--data-raw",
            "--data-binary",
            "--data-urlencode",
            "--json",
            "-F",
            "--form",
            "--form-string",
            "-H",
            "--header",
            "-o",
            "--output",
            "--url",
            "-u",
            "--user",
            "-A",
            "--user-agent",
        ],
    );
    if opts.iter().any(|(key, value)| {
        matches!(
            *key,
            "-T" | "--upload-file"
                | "-d"
                | "--data"
                | "--data-raw"
                | "--data-binary"
                | "--data-urlencode"
                | "--json"
                | "-F"
                | "--form"
                | "--form-string"
        ) || (matches!(*key, "-X" | "--request") && *value == Some("POST"))
    }) {
        add(kinds, ExternalCommunication);
    } else if !opts.iter().any(|(key, value)| {
        matches!(*key, "-X" | "--request") && !matches!(*value, Some("GET" | "HEAD"))
    }) {
        add(kinds, ExternalCode);
    }
}
