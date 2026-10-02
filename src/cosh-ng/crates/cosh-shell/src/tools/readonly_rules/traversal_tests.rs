//! Path-traversal blocking coverage for issue #2184.
//!
//! Declared only from `lib.rs` (the `wrap_tests` pattern) so the cases
//! stay out of the lib/bin test overlap ratchet
//! (`scripts/check-test-inventory.sh`): the module is compiled for the
//! `--lib` target only, while `main.rs` does not declare it.

use crate::tools::readonly_rules::{is_readonly_command, is_safe_readonly_path};

fn tokens(cmd: &str) -> Vec<String> {
    cmd.split_whitespace().map(String::from).collect()
}

fn allowed(cmd: &str) -> bool {
    is_readonly_command(&tokens(cmd))
}

#[test]
fn absolute_traversal_spellings_are_blocked() {
    // Issue #2184: these spellings resolve into /proc, /sys, or /dev at
    // the OS level but do not carry the blocked prefix verbatim, so a
    // plain prefix match lets them through.
    for path in [
        "/../proc/version",
        "/./proc/version",
        "/tmp/../proc/version",
        "/../dev/urandom",
        "/../sys/kernel/ostype",
        "//proc//version",
        "/proc/../proc/version",
    ] {
        assert!(!is_safe_readonly_path(path), "{path}");
    }
    assert!(!allowed("cat /../proc/version"));
    // The `--` separator path checks the blocklist on its own.
    assert!(!allowed("cat -n -- /../proc/version"));
}

#[test]
fn blocked_prefix_spellings_stay_blocked_after_normalization() {
    // Fail closed: a raw /proc-prefixed spelling stays blocked even when
    // its lexical resolution escapes the blocklist.
    assert!(!is_safe_readonly_path("/proc/../etc/hostname"));
}

#[test]
fn relative_traversal_into_special_dirs_is_blocked() {
    // Readonly executors run with the shell's working directory, so when
    // the cwd is a child of `/`, `../proc/version` resolves into /proc at
    // the OS level. The blocklist has no cwd and fails closed instead.
    for path in [
        "../proc/version",
        "../../proc/version",
        "../dev/urandom",
        "../../sys/kernel/ostype",
        "../proc",
        "../foo/../proc/version",
    ] {
        assert!(!is_safe_readonly_path(path), "{path}");
    }
    assert!(!allowed("cat ../proc/version"));
}

#[test]
fn bare_relative_spellings_into_special_dirs_are_blocked() {
    // Same cwd-independent fail-closed rule as `..`-led spellings: with
    // cwd=`/` these resolve into the blocklist at the OS level, so the
    // spelling is refused for every cwd.
    for path in [
        "proc/version",
        "./proc/version",
        "proc/../proc/version",
        "dev/urandom",
        "sys/kernel/ostype",
        "proc",
        "dev",
        "sys",
    ] {
        assert!(!is_safe_readonly_path(path), "{path}");
    }
    assert!(!allowed("cat proc/version"));
    assert!(!allowed("cat ./proc/version"));
}

#[test]
fn tilde_popping_traversal_fails_closed() {
    // The shell expands a leading `~` to $HOME only at execution time,
    // so a `..` that would pop it is unresolvable at this layer; the
    // spelling is refused rather than normalized into a bare relative
    // form that might escape the blocklist.
    for path in [
        "~/../proc/version",
        "~root/../proc/version",
        "~/../../proc/version",
        "~root/..",
    ] {
        assert!(!is_safe_readonly_path(path), "{path}");
    }
    assert!(!allowed("cat ~/../proc/version"));
}

#[test]
fn traversal_normalization_does_not_over_block() {
    for path in [
        "/home/../usr/bin/ls",
        "/var/log/../lib/foo",
        "./local/file",
        "/home/user/x",
        "usr/../local/file",
        // Traversal into non-special directories stays allowed.
        "../notes.txt",
        "../../tmp/x",
        // Pure traversal with no target component.
        "..",
        "../..",
        // A leading `~` that no `..` pops stays an ordinary path.
        "~/notes.txt",
        "~/.bashrc",
        "~/foo/../bar",
        // Component match is exact: a `proc`-prefixed name is not `proc`.
        "procx/file",
        // A special name below the first component is not blocked.
        "local/dev/file",
    ] {
        assert!(is_safe_readonly_path(path), "{path}");
    }
    assert!(allowed("cat ../notes.txt"));
    assert!(allowed("cat /home/../usr/bin/ls"));
    assert!(allowed("cat ~/notes.txt"));
}
