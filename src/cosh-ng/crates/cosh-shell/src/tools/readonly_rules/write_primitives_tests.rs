//! Write-primitive rejection coverage for the readonly spec layer.
//!
//! Declared only from `lib.rs` (the `traversal_tests` pattern) so the
//! cases stay out of the lib/bin test overlap ratchet
//! (`scripts/check-test-inventory.sh`): the module is compiled for the
//! `--lib` target only, while `main.rs` does not declare it.

use crate::tools::readonly_rules::is_readonly_command;

fn tokens(cmd: &str) -> Vec<String> {
    cmd.split_whitespace().map(String::from).collect()
}

fn allowed(cmd: &str) -> bool {
    is_readonly_command(&tokens(cmd))
}

#[test]
fn readonly_specs_reject_write_primitives() {
    // GNU uniq's second file operand is the OUTPUT file: `uniq INPUT
    // OUTPUT` rewrites OUTPUT wholesale (an rc-file overwrite persists
    // code execution for the next shell). Single-input forms, including
    // the `-f N`/`-w N` value spellings, stay readonly.
    assert!(allowed("uniq file.txt"));
    assert!(allowed("uniq -c file.txt"));
    assert!(allowed("uniq -w 5 file.txt"));
    assert!(allowed("uniq -f 2 -c file.txt"));
    assert!(!allowed("uniq in.txt out.txt"));
    assert!(!allowed("uniq -c in.txt out.txt"));
    assert!(!allowed("uniq -- in.txt out.txt"));
    assert!(!allowed("uniq -w 5 in.txt out.txt"));
    assert!(allowed("uniq -s 3 file.txt"));
    assert!(!allowed("uniq -s 3 in.txt out.txt"));

    // --sort/--format order and render the listing; they do not imply
    // list mode in git. With a positional they are the branch-create
    // form, which writes refs — only an explicit list flag may carry a
    // positional pattern.
    assert!(allowed("git branch --sort=refname"));
    assert!(allowed("git branch --sort=refname -a"));
    assert!(allowed("git branch --format=%(refname)"));
    assert!(!allowed("git branch --sort=refname feature"));
    assert!(!allowed("git branch --format=%(refname) feature"));

    // `git remote` subcommands write .git/config (add/set-url/remove —
    // set-url can repoint a push remote at an attacker's repository) or
    // contact remotes; only the two listing forms are readonly.
    assert!(allowed("git remote"));
    assert!(allowed("git remote -v"));
    assert!(allowed("git remote --verbose"));
    assert!(!allowed(
        "git remote add origin https://example.invalid/repo.git"
    ));
    assert!(!allowed(
        "git remote set-url origin https://example.invalid/repo.git"
    ));
    assert!(!allowed("git remote remove origin"));
}
