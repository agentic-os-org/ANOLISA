use super::*;

fn targets(command: &str) -> (Vec<(Operation, String, bool)>, bool) {
    let analysis = analyze(command);
    let found = analysis
        .targets
        .into_iter()
        .map(|t| (t.operation, t.path, t.tree))
        .collect();
    (found, analysis.unresolved)
}

fn named(expected: &[(Operation, &str, bool)]) -> Vec<(Operation, String, bool)> {
    expected
        .iter()
        .map(|(operation, path, tree)| (*operation, (*path).to_owned(), *tree))
        .collect()
}

const R: Operation = Operation::Replace;
const D: Operation = Operation::Delete;

#[test]
fn read_only_commands_name_no_target() {
    for command in [
        "ls -la",
        "git status && git diff HEAD -- src",
        "cat a.txt | grep x | wc -l",
        "grep -rn foo src 2>/dev/null",
        "echo $HOME",
        "FOO=1 env BAR=2 cat file",
        "sed -n '1,5p' file.txt",
        "find . -name '*.rs'",
        "git clean -n",
        "cat a 2>&1 | head",
        "# comment only",
    ] {
        assert_eq!(analyze(command), Analysis::default(), "{command}");
    }
}

#[test]
fn named_write_targets_are_reported() {
    assert_eq!(
        targets("echo hi > out.txt"),
        (named(&[(R, "out.txt", false)]), false)
    );
    assert_eq!(
        targets("echo hi>>'my file'"),
        (named(&[(R, "my file", false)]), false)
    );
    assert!(targets("cmd 2> err.log").1);
    assert_eq!(
        targets("ls 2> err.log"),
        (named(&[(R, "err.log", false)]), false)
    );
    assert_eq!(
        targets("ls &> all.log"),
        (named(&[(R, "all.log", false)]), false)
    );
    assert_eq!(
        targets("rm -rf build dist"),
        (named(&[(D, "build", true), (D, "dist", true)]), false)
    );
    assert_eq!(targets("rm -- -odd"), (named(&[(D, "-odd", false)]), false));
    assert_eq!(targets("sudo rm a"), (named(&[(D, "a", false)]), false));
    assert_eq!(
        targets("echo x | tee -a a b"),
        (named(&[(R, "a", false), (R, "b", false)]), false)
    );
    assert_eq!(
        targets("truncate -s 0 log"),
        (named(&[(R, "log", false)]), false)
    );
    assert_eq!(
        targets("sed -i 's/a/b/' f1 f2"),
        (named(&[(R, "f1", false), (R, "f2", false)]), false)
    );
    assert_eq!(
        targets("sed -i.bak -e 's/a/b/' f"),
        (named(&[(R, "f", false)]), false)
    );
    assert_eq!(
        targets("sed -Ei 's/a/b/' f"),
        (named(&[(R, "f", false)]), false)
    );
    assert_eq!(
        targets("sort -o out in"),
        (named(&[(R, "out", false)]), false)
    );
    assert_eq!(
        targets("git checkout HEAD -- a b"),
        (named(&[(R, "a", true), (R, "b", true)]), false)
    );
    assert_eq!(
        targets("git restore src"),
        (named(&[(R, "src", true)]), false)
    );
    assert_eq!(targets("git restore --staged src"), (named(&[]), false));
    assert_eq!(
        targets("git rm -r docs"),
        (named(&[(D, "docs", true)]), false)
    );
}

#[test]
fn transfers_report_both_destination_forms() {
    assert_eq!(
        targets("mv a.txt b"),
        (
            named(&[(D, "a.txt", true), (R, "b/a.txt", false), (R, "b", false)]),
            false
        )
    );
    assert_eq!(
        targets("cp -r x y dir/"),
        (named(&[(R, "dir/x", false), (R, "dir/y", false)]), false)
    );
}

#[test]
fn heredoc_bodies_are_data() {
    let command = "cat > notes.md <<'EOF'\nrm -rf everything\n$(not run)\nEOF\necho done";
    assert_eq!(targets(command), (named(&[(R, "notes.md", false)]), false));
    let expanding = "cat > notes.md <<EOF\n$(date)\nEOF";
    assert_eq!(targets(expanding), (named(&[(R, "notes.md", false)]), true));
}

#[test]
fn unattributable_parts_stay_unresolved() {
    for command in [
        "python build.py",
        "./script.sh",
        "echo $(rm x)",
        "echo `id`",
        "rm $TARGET",
        "rm *.log",
        "rm ~/x",
        "(cd sub && rm a)",
        "find . -name x -delete",
        "ls | xargs rm",
        "git checkout main",
        "git reset --hard",
        "git clean -fd",
        "git -C other checkout -- a",
        "git push",
        "echo 'unterminated",
        "diff <(ls a) <(ls b)",
        "cp -t dir a",
        "sudo -u other rm a",
    ] {
        assert!(analyze(command).unresolved, "{command}");
    }
}

#[test]
fn cd_makes_later_relative_bases_unknown() {
    let analysis = analyze("rm a && cd sub && rm b /abs/c");
    let bases: Vec<_> = analysis
        .targets
        .iter()
        .map(|t| (t.path.as_str(), t.base_unknown))
        .collect();
    assert_eq!(bases, [("a", false), ("b", true), ("/abs/c", true)]);
    assert!(!analysis.unresolved);
}
