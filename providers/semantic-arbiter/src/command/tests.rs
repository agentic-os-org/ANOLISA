use super::*;

fn targets(command: &str) -> (Vec<(Operation, String, bool)>, bool) {
    let analysis = analyze(command, WORKSPACE);
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
const WORKSPACE: &str = "/work";

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
        assert_eq!(
            analyze(command, WORKSPACE),
            Analysis::default(),
            "{command}"
        );
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
        assert!(analyze(command, WORKSPACE).unresolved, "{command}");
    }
}

#[test]
fn cd_makes_later_relative_bases_unknown() {
    let analysis = analyze("rm a && cd sub && rm b /abs/c", WORKSPACE);
    let bases: Vec<_> = analysis
        .targets
        .iter()
        .map(|t| (t.path.as_str(), t.base_unknown))
        .collect();
    assert_eq!(bases, [("a", false), ("b", true), ("/abs/c", true)]);
    assert!(!analysis.unresolved);
}

#[test]
fn cd_back_to_the_workspace_root_keeps_relative_targets_attributable() {
    for command in [
        "cd /work && rm a",
        "cd /work/ && rm a",
        "cd /work/. && rm a",
        "cd '/work' && rm a",
        "cd \"/work\" && rm a",
        "cd /tmp && cd /work && rm a",
        "cd sub && cd /work && rm a",
    ] {
        let analysis = analyze(command, WORKSPACE);
        assert!(!analysis.unresolved, "{command}");
        let bases: Vec<_> = analysis
            .targets
            .iter()
            .map(|t| (t.path.as_str(), t.base_unknown))
            .collect();
        assert_eq!(bases, [("a", false)], "{command}");
    }
}

#[test]
fn cd_anywhere_but_the_root_leaves_relative_targets_unknown() {
    for command in [
        "cd /work/sub && rm a",
        "cd sub && rm a",
        "cd /tmp && rm a",
        "cd /work/.. && rm a",
        "cd $DIR && rm a",
        "cd - && rm a",
        "cd ~ && rm a",
        "cd && rm a",
        "cd /work extra && rm a",
        "pushd /work && rm a",
        "popd && rm a",
    ] {
        let analysis = analyze(command, WORKSPACE);
        let bases: Vec<_> = analysis
            .targets
            .iter()
            .map(|t| (t.path.as_str(), t.base_unknown))
            .collect();
        assert_eq!(bases, [("a", true)], "{command}");
    }
}

#[test]
fn relocated_tree_read_only_git_names_no_target() {
    for command in [
        "git -C other status",
        "git -C other log --oneline -5",
        "git -C other diff HEAD -- src",
        "git --git-dir=g --work-tree=w status",
        "git --git-dir=g log",
        "git --version",
        "git --help",
        "cd /x && git -C /y status",
    ] {
        assert_eq!(
            analyze(command, WORKSPACE),
            Analysis::default(),
            "{command}"
        );
    }
}

#[test]
fn lone_version_query_names_no_target() {
    for command in [
        "rustc --version",
        "rustc -V",
        "cargo --help",
        "cargo -h",
        "python3 --version",
        "cat Makefile && rustc --version",
    ] {
        assert_eq!(
            analyze(command, WORKSPACE),
            Analysis::default(),
            "{command}"
        );
    }
}

#[test]
fn relocated_tree_mutating_git_stays_unresolved() {
    for command in [
        "git -C other checkout -- a",
        "git -C other rm -r docs",
        "git -C other commit -m x",
        "git -C other reset --hard",
        "git --work-tree=w checkout -- a",
        "rustc --version foo.rs",
        "rustc foo.rs",
        "cargo build",
    ] {
        assert!(analyze(command, WORKSPACE).unresolved, "{command}");
    }
}
