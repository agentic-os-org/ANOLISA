use super::*;

fn tokens(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
}

fn allowed(cmd: &str) -> bool {
    is_readonly_command(&tokens(cmd))
}

fn allowed_with_config(cmd: &str, config: &RuntimeReadonlyConfig) -> bool {
    is_readonly_command_with_config(&tokens(cmd), config)
}

// ── Evaluator unit tests ──

#[test]
fn runtime_config_can_add_generic_rule() {
    let config = RuntimeReadonlyConfig {
        overrides: vec![RuntimeReadonlySpec {
            command: "myread".to_string(),
            validator: RuntimeValidator::Generic(RuntimeGenericSpec {
                short_flags: "v".to_string(),
                long_flags: vec!["--verbose".to_string()],
                value_flags: vec![("-n".to_string(), Some(5))],
                deny_flags: vec!["--write".to_string()],
                path_mode: PathMode::Required,
                bare_number_max: 0,
            }),
        }],
        ..RuntimeReadonlyConfig::default()
    };

    assert!(allowed_with_config("myread -v -n 3 file.txt", &config));
    assert!(!allowed_with_config("myread -n 10 file.txt", &config));
    assert!(!allowed_with_config("myread --write file.txt", &config));
}

#[test]
fn runtime_config_override_replaces_builtin_and_disable_wins() {
    let config = RuntimeReadonlyConfig {
        disabled: vec![ReadonlyRuleKey::subcommand("git", "status")],
        overrides: vec![RuntimeReadonlySpec {
            command: "git".to_string(),
            validator: RuntimeValidator::Subcommand(RuntimeSubcommandSpec {
                deny_args: vec!["-c".to_string()],
                subcommands: vec![(
                    "status".to_string(),
                    RuntimeValidator::Generic(RuntimeGenericSpec {
                        short_flags: String::new(),
                        long_flags: vec!["--short".to_string()],
                        value_flags: Vec::new(),
                        deny_flags: Vec::new(),
                        path_mode: PathMode::None,
                        bare_number_max: 0,
                    }),
                )],
            }),
        }],
        ..RuntimeReadonlyConfig::default()
    };

    assert!(!allowed_with_config("git status --short", &config));
    assert!(allowed("git status --short"));
}

#[test]
fn bare_allows_single_token() {
    assert!(allowed("pwd"));
    assert!(allowed("whoami"));
    assert!(allowed("hostname"));
    assert!(allowed("date"));
    assert!(allowed("uptime"));
    assert!(allowed("vm_stat"));
    assert!(allowed("nproc"));
    assert!(allowed("sw_vers"));
    assert!(allowed("arch"));
    assert!(allowed("tty"));
    assert!(allowed("groups"));
    assert!(!allowed("pwd extra"));
}

#[test]
fn version_check_allows_listed_flags() {
    assert!(allowed("rustc --version"));
    assert!(allowed("rustc -V"));
    assert!(allowed("node --version"));
    assert!(allowed("python --version"));
    assert!(allowed("python3 -V"));
    assert!(allowed("gcc --version"));
    assert!(allowed("swift --version"));
    assert!(allowed("dotnet --version"));
    assert!(!allowed("rustc"));
    assert!(!allowed("rustc --help"));
    assert!(!allowed("rustc --version extra"));
}

#[test]
fn generic_short_flags_only() {
    assert!(allowed("uname -a"));
    assert!(allowed("uname -am"));
    assert!(allowed("uname"));
    assert!(!allowed("uname -x"));
    assert!(!allowed("uname somefile"));
}

#[test]
fn generic_flags_with_required_paths() {
    assert!(allowed("wc -l file.txt"));
    assert!(allowed("wc file.txt"));
    assert!(!allowed("wc"));
    assert!(!allowed("wc -l"));
    assert!(!allowed("wc -l /dev/zero"));
}

#[test]
fn generic_deny_flags() {
    assert!(allowed("sort -n file.txt"));
    assert!(!allowed("sort -o out.txt file.txt"));
    assert!(!allowed("sort --output=out.txt file.txt"));
}

#[test]
fn generic_value_flags_with_bounds() {
    assert!(allowed("du -sh ."));
    assert!(allowed("du -d 5 ."));
    assert!(allowed("du -d5 ."));
    assert!(!allowed("du -d 100 ."));
    assert!(!allowed("du -d"));
}

#[test]
fn generic_double_dash_separator() {
    assert!(allowed("cat -n -- -weird-name.txt"));
}

#[test]
fn generic_unchecked_paths() {
    assert!(allowed("which -a gcc"));
    assert!(allowed("which /dev/null"));
    assert!(allowed("echo hello world"));
    assert!(allowed("printenv PATH"));
}

#[test]
fn generic_bare_number() {
    assert!(allowed("git log -5"));
    assert!(allowed("git log -n 10"));
    assert!(allowed("git log -n10"));
    assert!(allowed("git log --oneline -20"));
    assert!(!allowed("git log -0"));
}

#[test]
fn generic_long_value_flags_with_equals() {
    assert!(allowed("du --max-depth 5 ."));
    assert!(allowed("du --max-depth=5 ."));
    assert!(!allowed("du --max-depth=100 ."));
}

// ── Backward-compatible regression tests ──
// These mirror the exact test cases from tool_broker.rs tests

#[test]
fn regression_allows_simple_git_status() {
    assert!(allowed("git status --short"));
}

#[test]
fn regression_blocks_mutation() {
    assert!(!allowed("touch /tmp/test"));
}

#[test]
fn regression_allows_bounded_cpu_diagnostics() {
    for command in [
        "top -l 1 -n 15 -s 0",
        "top -l 1 -o cpu -n 20",
        "top -b -n 1 -o %CPU",
        "top -n 1 -b -o %CPU",
        "ps -Ao pid,pcpu,pmem,comm -r",
        "sysctl -n hw.ncpu",
        "sysctl -n machdep.cpu.brand_string",
    ] {
        assert!(allowed(command), "{command}");
    }
}

#[test]
fn regression_allows_disk_usage_diagnostics() {
    for command in ["df", "df -h", "df -hi", "df -h ."] {
        assert!(allowed(command), "{command}");
    }
}

#[test]
fn regression_rejects_unbounded_cpu_diagnostics() {
    for command in [
        "top",
        "top -l 2 -n 15",
        "top -l 1 -n 1000",
        "sysctl -a",
        "sysctl -w hw.ncpu=1",
    ] {
        assert!(!allowed(command), "{command}");
    }
}

#[test]
fn regression_rejects_risky_per_command_arguments() {
    for command in [
        "git -c core.pager=cat status",
        "git diff --ext-diff",
        "git show --textconv HEAD:README.md",
        "git diff --output=/tmp/cosh-shell-git-diff.txt",
        "ps -o command=",
        "find . -exec echo {} ;",
        "find . -delete",
        "find /proc -name cpuinfo",
        "find . -maxdepth 100 -name Cargo.toml",
        "cat /dev/zero",
        "cat /proc/cpuinfo",
        "head -n 100000 README.md",
        "tail -f README.md",
        "grep -R cosh .",
        "grep cosh /proc/cpuinfo",
        "rg --pre cat cosh .",
        "rg --pre=cat cosh .",
        "rg -n cosh /dev",
        "ls /dev/zero",
        "df --output=source",
        "df /dev/zero",
    ] {
        assert!(!allowed(command), "{command}");
    }
}

#[test]
fn regression_allows_safe_per_command_arguments() {
    for command in [
        "git status --short",
        "git diff --stat",
        "git diff --name-only",
        "git log --oneline -n 5",
        "git show --stat HEAD",
        "ps -Ao pid,pcpu,pmem,comm -r",
        "ls -la .",
        "cat README.md",
        "head -n 20 README.md",
        "head -20 README.md",
        "tail -n 20 README.md",
        "grep -n cosh README.md",
        "grep -e cosh README.md",
        "rg -n cosh crates/cosh-shell",
        "rg --files crates/cosh-shell",
        "find . -maxdepth 2 -type f -name Cargo.toml -print",
        "df -h .",
        "uname -a",
        "id -u",
    ] {
        assert!(allowed(command), "{command}");
    }
}

// ── New command tests ──

#[test]
fn new_file_inspection_commands() {
    assert!(allowed("wc -l src/main.rs"));
    assert!(allowed("file --mime src/main.rs"));
    assert!(allowed("stat src/main.rs"));
    assert!(allowed("diff file_a.txt file_b.txt"));
    assert!(allowed("md5sum Cargo.toml"));
    assert!(allowed("shasum -a 256 Cargo.toml"));
    assert!(allowed("sha256sum Cargo.toml"));
    assert!(allowed("realpath src/main.rs"));
    assert!(allowed("readlink src/link"));
}

#[test]
fn new_path_resolution_commands() {
    assert!(allowed("which gcc"));
    assert!(allowed("which -a rustc"));
    assert!(allowed("dirname /usr/bin/gcc"));
    assert!(allowed("basename /usr/bin/gcc"));
}

#[test]
fn new_environment_commands() {
    assert!(allowed("printenv PATH"));
    assert!(allowed("env"));
    assert!(!allowed("env FOO=bar cmd"));
}

#[test]
fn new_disk_system_commands() {
    assert!(allowed("du -sh ."));
    assert!(allowed("du -d 3 src"));
    assert!(allowed("free -h"));
    assert!(allowed("nproc"));
}

#[test]
fn new_text_processing_commands() {
    assert!(allowed("sort file.txt"));
    assert!(allowed("sort -n file.txt"));
    assert!(!allowed("sort -o output.txt file.txt"));
    assert!(allowed("uniq file.txt"));
    assert!(allowed("cut -d , -f 1 data.csv"));
    assert!(allowed("tr a-z A-Z"));
    assert!(allowed("fold -w 80 file.txt"));
    assert!(allowed("expand file.txt"));
    assert!(allowed("comm file_a.txt file_b.txt"));
}

#[test]
fn new_process_commands() {
    assert!(allowed("pgrep -l nginx"));
}

#[test]
fn new_git_subcommands() {
    assert!(allowed("git blame src/main.rs"));
    assert!(allowed("git describe --tags"));
    assert!(allowed("git describe --always"));
    assert!(allowed("git ls-files"));
    assert!(allowed("git ls-files -o --error-unmatch"));
    assert!(allowed("git ls-tree HEAD"));
    assert!(allowed("git ls-tree -r HEAD src/"));
    assert!(allowed("git shortlog -sn"));
    assert!(allowed("git rev-list --count HEAD"));
    assert!(allowed("git cat-file -t HEAD"));
    assert!(allowed("git cat-file -p HEAD"));
    assert!(allowed("git count-objects -v"));
    assert!(allowed(
        "git for-each-ref --format %(refname) --sort=-committerdate"
    ));
    assert!(allowed("git name-rev --name-only HEAD"));
    assert!(allowed("git stash list"));
    assert!(allowed("git stash show"));
    assert!(!allowed("git stash"));
    assert!(!allowed("git stash drop"));
    assert!(allowed("git config --get user.name"));
    assert!(allowed("git config --list"));
    assert!(!allowed("git config --set user.name foo"));
    assert!(allowed("git tag"));
    assert!(allowed("git tag -l"));
    assert!(!allowed("git tag v1.0"));
    assert!(allowed("git reflog show"));
    assert!(!allowed("git reflog delete"));
    assert!(allowed("git branch -a"));
    assert!(allowed("git branch --list"));
    assert!(allowed("git branch --list feature"));
    assert!(!allowed("git branch -d feature"));
    assert!(!allowed("git branch --delete feature"));
    assert!(!allowed("git branch feature"));
}

#[test]
fn new_cargo_subcommands() {
    assert!(allowed("cargo --version"));
    assert!(allowed("cargo tree"));
    assert!(allowed("cargo tree --depth 3"));
    assert!(allowed("cargo metadata --no-deps"));
    assert!(!allowed("cargo build"));
}

#[test]
fn new_docker_subcommands() {
    assert!(allowed("docker version"));
    assert!(allowed("docker info"));
    assert!(allowed("docker ps"));
    assert!(allowed("docker ps -a --format table"));
    assert!(allowed("docker images"));
    assert!(allowed("docker inspect container_id"));
    assert!(!allowed("docker run ubuntu"));
}

#[test]
fn new_kubectl_subcommands() {
    assert!(allowed("kubectl get pods"));
    assert!(allowed("kubectl get pods -n kube-system"));
    assert!(allowed("kubectl get pods --all-namespaces"));
    assert!(allowed("kubectl describe pod nginx"));
    assert!(allowed("kubectl version --client"));
    assert!(allowed("kubectl top pods"));
    assert!(!allowed("kubectl delete pod nginx"));
}

#[test]
fn new_go_subcommands() {
    assert!(allowed("go version"));
    assert!(allowed("go env"));
    assert!(allowed("go env GOPATH"));
    assert!(!allowed("go env -w GOPATH=/new"));
    assert!(!allowed("go build"));
}

#[test]
fn new_macos_commands() {
    assert!(allowed("sw_vers"));
    assert!(allowed("xcode-select -p"));
    assert!(allowed("xcode-select --print-path"));
    assert!(allowed("diskutil list"));
    assert!(allowed("diskutil info disk0"));
    assert!(!allowed("diskutil erase disk0"));
    assert!(allowed("defaults read com.apple.finder"));
    assert!(!allowed("defaults write com.apple.finder key val"));
    assert!(allowed("xcrun --find clang"));
    assert!(allowed("system_profiler SPHardwareDataType"));
}

#[test]
fn git_tag_with_create_arg_rejected() {
    assert!(!allowed("git tag v1.0.0"));
}

#[test]
fn path_safety_basics() {
    assert!(is_safe_readonly_path("file.txt"));
    assert!(is_safe_readonly_path("src/main.rs"));
    assert!(!is_safe_readonly_path(""));
    assert!(!is_safe_readonly_path("-"));
    assert!(!is_safe_readonly_path("-flag"));
    assert!(!is_safe_readonly_path("/dev/zero"));
    assert!(!is_safe_readonly_path("/proc/cpuinfo"));
    assert!(!is_safe_readonly_path("/sys/class"));
}

#[test]
fn bounded_count_basics() {
    assert!(is_bounded_positive_count("1", 100));
    assert!(is_bounded_positive_count("100", 100));
    assert!(!is_bounded_positive_count("0", 100));
    assert!(!is_bounded_positive_count("101", 100));
    assert!(!is_bounded_positive_count("abc", 100));
    assert!(!is_bounded_positive_count("-1", 100));
}
