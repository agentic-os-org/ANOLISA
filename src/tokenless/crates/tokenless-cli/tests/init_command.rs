//! End-to-end coverage for `tokenless init`.
//!
//! Every case drives the real binary against a throwaway `ANOLISA_ADAPTER_DIR`
//! built from stub detectors and installers, so the two detector contracts
//! (tri-state exit code and published JSON status), the selection modes and the
//! exit codes a caller can rely on are exercised the way a user reaches them.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

/// Detector that follows the shared tri-state contract: not installed yet, but
/// every prerequisite is present.
const DETECT_INSTALLABLE: &str = "#!/usr/bin/env bash\nexit 1\n";
/// Detector that follows the shared tri-state contract: adapter registered.
const DETECT_READY: &str = "#!/usr/bin/env bash\nexit 0\n";
/// Detector that follows the shared tri-state contract: prerequisites missing.
const DETECT_MISSING_PREREQS: &str = "#!/usr/bin/env bash\nexit 2\n";

/// One manifest target with the detector body it should be wired to.
struct Target {
    name: &'static str,
    detect: &'static str,
    /// `None` declares `actions.detect` without `actions.install`.
    install: Option<String>,
}

impl Target {
    /// A target whose detector reports installable and whose installer records
    /// every run in the shared install log.
    fn installable(name: &'static str) -> Self {
        Self::new(name, DETECT_INSTALLABLE)
    }

    fn new(name: &'static str, detect: &'static str) -> Self {
        Self {
            name,
            detect,
            install: Some(logging_installer()),
        }
    }
}

/// Installer body that appends the target name to the fixture's install log.
fn logging_installer() -> String {
    String::from(
        "#!/usr/bin/env bash\n\
         printf '%s\\n' \"$ANOLISA_TARGET\" >> \"$TOKENLESS_INIT_TEST_LOG\"\n",
    )
}

/// A throwaway adapter directory plus the isolated home the CLI runs against.
struct Fixture {
    _root: TempDir,
    adapter_dir: PathBuf,
    home: PathBuf,
    data_root: PathBuf,
    log: PathBuf,
}

impl Fixture {
    /// Build an adapter directory holding `manifest.json` and the stub scripts.
    fn new(targets: &[Target]) -> Self {
        Self::with_contract(targets, true)
    }

    /// A tree that predates the detector contract: no `detectContract` marker,
    /// so exit 0 may only mean the host is usable. This is what a newer CLI
    /// meets when npm preserves a shared adapter directory another installation
    /// put there, or when `ANOLISA_ADAPTER_DIR` points at one.
    fn legacy(targets: &[Target]) -> Self {
        Self::with_contract(targets, false)
    }

    fn with_contract(targets: &[Target], declare_contract: bool) -> Self {
        let root = TempDir::new().expect("create fixture root");
        let adapter_dir = root.path().join("data/anolisa/adapters/tokenless");
        fs::create_dir_all(&adapter_dir).expect("create adapter directory");

        let contract = if declare_contract {
            "\"detectContract\":\"status-v1\","
        } else {
            ""
        };
        let mut manifest = format!(
            "{{\"component\":\"tokenless\",\"version\":\"0.0.0-test\",{contract}\"targets\":{{"
        );
        for (index, target) in targets.iter().enumerate() {
            let scripts = Path::new(target.name).join("scripts");
            fs::create_dir_all(adapter_dir.join(&scripts)).expect("create script directory");
            write_script(&adapter_dir.join(scripts.join("detect.sh")), target.detect);
            let actions = match &target.install {
                Some(body) => {
                    write_script(&adapter_dir.join(scripts.join("install.sh")), body);
                    format!(
                        "\"detect\":\"{0}/scripts/detect.sh\",\"install\":\"{0}/scripts/install.sh\"",
                        target.name
                    )
                }
                None => format!("\"detect\":\"{0}/scripts/detect.sh\"", target.name),
            };
            let separator = if index == 0 { "" } else { "," };
            // No capabilities on purpose: the hooks column would print
            // "tool-ready", and a `contains("ready")` assertion on the row
            // could not tell the hooks text from the status column.
            manifest.push_str(&format!(
                "{separator}\"{name}\":{{\"compatibleVersions\":\"*\",\"actions\":{{{actions}}}}}",
                name = target.name
            ));
        }
        manifest.push_str("}}");
        fs::write(adapter_dir.join("manifest.json"), manifest).expect("write manifest");

        // A home and a data root of their own keep the real installation, and
        // anything the detectors read from $HOME, out of the run.
        let home = root.path().join("home");
        let data_root = root.path().join("data");
        fs::create_dir_all(&home).expect("create fixture home");
        let log = root.path().join("install.log");

        Self {
            _root: root,
            adapter_dir,
            home,
            data_root,
            log,
        }
    }

    /// A command for the CLI under test with the fixture as its whole world.
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tokenless"));
        command
            .env("ANOLISA_ADAPTER_DIR", &self.adapter_dir)
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", &self.data_root)
            .env("TOKENLESS_INIT_TEST_LOG", &self.log)
            .env_remove("TOKENLESS_DATA_DIR")
            .env_remove("DSH_TOKENLESS_DATA_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    /// A command whose `PATH` holds nothing, so a child the CLI spawns by name
    /// cannot be found.
    fn command_without_bash(&self, empty_bin: &Path) -> Command {
        let mut command = self.command();
        command.env("PATH", empty_bin);
        command
    }

    /// A directory for host CLI stubs, off the fixture PATH until a test asks
    /// for it through the environment.
    fn stub_bin(&self) -> PathBuf {
        let dir = self._root.path().join("stub-bin");
        fs::create_dir_all(&dir).expect("create stub bin");
        dir
    }

    /// A directory holding nothing, for runs that must not find an interpreter.
    fn empty_bin(&self) -> PathBuf {
        let dir = self._root.path().join("empty-bin");
        fs::create_dir_all(&dir).expect("create empty bin");
        dir
    }

    /// Targets the installers recorded, in the order they ran.
    fn installed(&self) -> Vec<String> {
        match fs::read_to_string(&self.log) {
            Ok(contents) => contents.lines().map(String::from).collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// Write an executable script body, keeping the shebang the stubs declare.
fn write_script(path: &Path, body: &str) {
    fs::write(path, body).expect("write stub script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod stub script");
    }
}

/// One finished CLI run with both streams decoded.
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    /// Both streams together, for assertions that do not care which one carried
    /// an installer's diagnostics.
    fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }

    fn assert_code(&self, expected: i32) {
        assert_eq!(
            self.code, expected,
            "unexpected exit code\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.stdout, self.stderr
        );
    }
}

fn run(fixture: &Fixture, args: &[&str]) -> Run {
    finish(
        fixture
            .command()
            .args(args)
            .output()
            .expect("run tokenless init"),
    )
}

/// Run with `stdin` attached as a pipe, which is what a non-terminal caller sees.
fn run_with_stdin(fixture: &Fixture, args: &[&str], stdin: &str) -> Run {
    use std::io::Write as _;
    let mut child = fixture
        .command()
        .args(args)
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn tokenless init");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(stdin.as_bytes())
        .expect("feed stdin");
    finish(child.wait_with_output().expect("wait for tokenless init"))
}

fn finish(output: Output) -> Run {
    Run {
        code: output.status.code().expect("exit code"),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// True when a system-wide installation could answer the discovery fallbacks.
///
/// Mirrors the CLI's own `has_manifest` check so a template-only installation
/// still counts as present.
fn system_adapter_dir_present() -> bool {
    ["/usr/share", "/usr/local/share"].iter().any(|root| {
        let adapter_dir = Path::new(root).join("anolisa/adapters/tokenless");
        adapter_dir.join("manifest.json").exists() || adapter_dir.join("manifest.json.in").exists()
    })
}

/// The status table row for one framework.
fn row_for<'a>(stdout: &'a str, name: &str) -> &'a str {
    stdout
        .lines()
        .find(|line| line.starts_with(name))
        .unwrap_or_else(|| panic!("no status row for {name}:\n{stdout}"))
}

/// The status column of one framework's table row.
fn status_of(stdout: &str, name: &str) -> String {
    let row = row_for(stdout, name);
    row[name.len()..]
        .trim()
        .trim_end_matches('\u{2014}')
        .trim()
        .to_string()
}

// ---------------------------------------------------------------------------
// P1-01 — a named operation ends with its own result
// ---------------------------------------------------------------------------

#[test]
fn named_framework_installs_once_and_does_not_continue_into_selection() {
    let fixture = Fixture::new(&[Target::installable("one"), Target::installable("two")]);

    let run = run(&fixture, &["init", "--framework", "one"]);

    run.assert_code(0);
    assert_eq!(fixture.installed(), vec!["one".to_string()]);
    assert!(
        run.stdout.contains("one adapter installed successfully."),
        "missing success line:\n{}",
        run.stdout
    );
    // The fall-through used to reuse the pre-install statuses and offer the
    // generic selection again, which installed the named target a second time.
    assert!(
        !run.stdout.contains("Installable frameworks:"),
        "named operation continued into the interactive selection:\n{}",
        run.stdout
    );
    assert!(
        !run.stdout.contains("Select a framework to install"),
        "named operation prompted again:\n{}",
        run.stdout
    );
    assert!(
        !run.stdout.contains("Run `tokenless init --framework"),
        "named operation printed the non-terminal hints:\n{}",
        run.stdout
    );
}

#[test]
fn named_framework_rejects_all_instead_of_installing_twice() {
    let fixture = Fixture::new(&[Target::installable("one"), Target::installable("two")]);

    let run = run(&fixture, &["init", "--framework", "one", "--all"]);

    assert_ne!(run.code, 0, "combination was accepted:\n{}", run.combined());
    assert!(
        fixture.installed().is_empty(),
        "rejected combination still installed {:?}",
        fixture.installed()
    );
}

#[test]
fn named_framework_rejects_list() {
    let fixture = Fixture::new(&[Target::installable("one")]);

    let run = run(&fixture, &["init", "--framework", "one", "--list"]);

    assert_ne!(run.code, 0, "combination was accepted:\n{}", run.combined());
    assert!(fixture.installed().is_empty());
}

#[test]
fn unknown_framework_fails() {
    let fixture = Fixture::new(&[Target::installable("one")]);

    let run = run(&fixture, &["init", "--framework", "nope"]);

    run.assert_code(1);
    assert!(
        run.combined().contains("unknown framework: nope"),
        "missing reason:\n{}",
        run.combined()
    );
    assert!(fixture.installed().is_empty());
}

// ---------------------------------------------------------------------------
// P1-02 — the detector contract decides the status
// ---------------------------------------------------------------------------

#[test]
fn tri_state_exit_codes_drive_the_status_table() {
    let fixture = Fixture::new(&[
        Target::new("readytarget", DETECT_READY),
        Target::new("installabletarget", DETECT_INSTALLABLE),
        Target::new("missingtarget", DETECT_MISSING_PREREQS),
    ]);

    let run = run(&fixture, &["init", "--list"]);

    run.assert_code(0);
    for (name, label) in [
        ("readytarget", "ready"),
        ("installabletarget", "installable"),
        ("missingtarget", "missing prereqs"),
    ] {
        assert_eq!(status_of(&run.stdout, name), label, "row: {name}");
    }
    assert!(fixture.installed().is_empty(), "--list must not install");
}

#[test]
fn detector_exiting_zero_is_ready_and_is_not_reinstalled() {
    let fixture = Fixture::new(&[Target::new("one", DETECT_READY)]);

    let run = run(&fixture, &["init", "--framework", "one"]);

    run.assert_code(0);
    assert!(
        run.stdout.contains("one is already installed and ready."),
        "unexpected output:\n{}",
        run.stdout
    );
    assert!(fixture.installed().is_empty());
}

#[test]
fn detector_reporting_missing_prereqs_is_not_offered_for_install() {
    let fixture = Fixture::new(&[Target::new("one", DETECT_MISSING_PREREQS)]);

    let named = run(&fixture, &["init", "--framework", "one"]);
    named.assert_code(1);
    assert!(
        named
            .combined()
            .contains("not installable (status: missing prereqs)"),
        "unexpected reason:\n{}",
        named.combined()
    );

    let batch = run(&fixture, &["init", "--all"]);
    batch.assert_code(0);
    assert!(
        batch
            .stdout
            .contains("No frameworks are waiting for installation.")
    );
    assert!(fixture.installed().is_empty());
}

#[test]
fn published_json_status_overrides_the_exit_code() {
    // The shapes the shipped detectors use: codex reports its binary, qoder
    // reports host-CLI capability, and qwenwork exits 0 either way.
    let fixture = Fixture::new(&[
        Target::new(
            "binaryonly",
            "#!/usr/bin/env bash\necho '{\"installed\": true, \"status\": \"installable\"}'\nexit 0\n",
        ),
        Target::new(
            "hostcli",
            "#!/usr/bin/env bash\necho '[tokenless] hostcli: compatible CLI found'\necho '{\"status\":\"installable\"}'\nexit 0\n",
        ),
        Target::new(
            "absenthost",
            "#!/usr/bin/env bash\necho '{\"status\": \"missing_prereqs\"}'\nexit 1\n",
        ),
        Target::new(
            "registered",
            "#!/usr/bin/env bash\necho 'resources ready'\necho '{\"status\": \"ready\"}'\nexit 0\n",
        ),
    ]);

    let list = run(&fixture, &["init", "--list"]);
    list.assert_code(0);
    for (name, label) in [
        ("binaryonly", "installable"),
        ("hostcli", "installable"),
        ("absenthost", "missing prereqs"),
        ("registered", "ready"),
    ] {
        assert_eq!(status_of(&list.stdout, name), label, "row: {name}");
    }

    // Only the two genuinely installable targets are offered to --all.
    let batch = run(&fixture, &["init", "--all"]);
    batch.assert_code(0);
    assert_eq!(
        fixture.installed(),
        vec!["binaryonly".to_string(), "hostcli".to_string()]
    );
}

#[test]
fn legacy_installed_false_is_not_reported_ready() {
    let fixture = Fixture::new(&[Target::new(
        "one",
        "#!/usr/bin/env bash\necho '{\"installed\": false}'\nexit 0\n",
    )]);

    let run = run(&fixture, &["init", "--list"]);

    run.assert_code(0);
    assert_eq!(status_of(&run.stdout, "one"), "installable");
}

// ---------------------------------------------------------------------------
// P2-01 — installer output survives
// ---------------------------------------------------------------------------

#[test]
fn installer_diagnostics_on_both_streams_are_preserved() {
    let fixture = Fixture::new(&[Target {
        name: "one",
        detect: DETECT_INSTALLABLE,
        install: Some(String::from(
            "#!/usr/bin/env bash\n\
             echo 'restart the host to load the adapter'\n\
             echo 'diagnostic-on-stderr' >&2\n\
             exit 3\n",
        )),
    }]);

    let run = run(&fixture, &["init", "--framework", "one"]);

    run.assert_code(1);
    assert!(
        run.stdout.contains("restart the host to load the adapter"),
        "stdout guidance was dropped:\n{}",
        run.stdout
    );
    assert!(
        run.stderr.contains("diagnostic-on-stderr"),
        "stderr diagnostics were dropped:\n{}",
        run.stderr
    );
    assert!(
        run.combined().contains("exited with code 3"),
        "exit code was not reported:\n{}",
        run.combined()
    );
}

#[test]
fn successful_installer_stdout_reaches_the_user() {
    let fixture = Fixture::new(&[Target {
        name: "one",
        detect: DETECT_INSTALLABLE,
        install: Some(String::from(
            "#!/usr/bin/env bash\necho 'restart OpenCode to finish setup'\n",
        )),
    }]);

    let run = run(&fixture, &["init", "--all"]);

    run.assert_code(0);
    assert!(
        run.stdout.contains("restart OpenCode to finish setup"),
        "installer stdout was dropped:\n{}",
        run.stdout
    );
    assert_eq!(
        fixture.installed().len(),
        0,
        "unexpected log {:?}",
        fixture.installed()
    );
}

// ---------------------------------------------------------------------------
// P2-02 — a selected target that cannot be installed fails the run
// ---------------------------------------------------------------------------

#[test]
fn batch_install_fails_when_a_selected_target_has_no_installer() {
    let fixture = Fixture::new(&[Target {
        name: "one",
        detect: DETECT_INSTALLABLE,
        install: None,
    }]);

    let run = run(&fixture, &["init", "--all"]);

    run.assert_code(1);
    assert!(
        run.combined().contains("no install script for one"),
        "missing warning:\n{}",
        run.combined()
    );
    assert!(
        run.combined().contains("installation failed for: one"),
        "missing failure summary:\n{}",
        run.combined()
    );
    assert!(
        run.stdout.contains("declares no install action"),
        "status table did not flag the target:\n{}",
        run.stdout
    );
}

#[test]
fn named_install_fails_when_the_target_has_no_installer() {
    let fixture = Fixture::new(&[Target {
        name: "one",
        detect: DETECT_INSTALLABLE,
        install: None,
    }]);

    let run = run(&fixture, &["init", "--framework", "one"]);

    run.assert_code(1);
    assert!(
        run.combined()
            .contains("no install script configured for one"),
        "missing reason:\n{}",
        run.combined()
    );
}

#[test]
fn batch_install_fails_only_after_running_the_remaining_installers() {
    let fixture = Fixture::new(&[
        Target {
            name: "one",
            detect: DETECT_INSTALLABLE,
            install: None,
        },
        Target::installable("two"),
    ]);

    let run = run(&fixture, &["init", "--all"]);

    run.assert_code(1);
    assert_eq!(
        fixture.installed(),
        vec!["two".to_string()],
        "a failing target must not stop the others"
    );
    assert!(run.combined().contains("installation failed for: one"));
}

// ---------------------------------------------------------------------------
// Selection modes
// ---------------------------------------------------------------------------

#[test]
fn all_installs_every_installable_target_once() {
    let fixture = Fixture::new(&[
        Target::installable("one"),
        Target::installable("two"),
        Target::new("three", DETECT_READY),
    ]);

    let run = run(&fixture, &["init", "--all"]);

    run.assert_code(0);
    assert_eq!(
        fixture.installed(),
        vec!["one".to_string(), "two".to_string()],
        "ready targets must not be reinstalled"
    );
}

#[test]
fn batch_failure_of_one_installer_is_reported_after_the_rest_run() {
    let fixture = Fixture::new(&[
        Target {
            name: "one",
            detect: DETECT_INSTALLABLE,
            install: Some(String::from("#!/usr/bin/env bash\nexit 4\n")),
        },
        Target::installable("two"),
    ]);

    let run = run(&fixture, &["init", "--all"]);

    run.assert_code(1);
    assert_eq!(fixture.installed(), vec!["two".to_string()]);
    assert!(run.combined().contains("installation failed for: one"));
}

#[test]
fn without_a_terminal_the_selection_is_skipped_with_hints() {
    let fixture = Fixture::new(&[Target::installable("one")]);

    let run = run_with_stdin(&fixture, &["init"], "1\n");

    run.assert_code(0);
    assert!(
        fixture.installed().is_empty(),
        "a piped selection must not install: {:?}",
        fixture.installed()
    );
    assert!(
        run.stdout
            .contains("Run `tokenless init --framework <name>`"),
        "missing hint:\n{}",
        run.stdout
    );
}

// ---------------------------------------------------------------------------
// P2-03 — discovery honors the configured data root
// ---------------------------------------------------------------------------

#[test]
fn discovery_finds_the_configured_xdg_data_home() {
    let fixture = Fixture::new(&[Target::installable("one")]);
    let expected = fixture.adapter_dir.display().to_string();

    // No ANOLISA_ADAPTER_DIR: the fixture lives at
    // $XDG_DATA_HOME/anolisa/adapters/tokenless, which the packaged user
    // layout uses whenever XDG_DATA_HOME is set.
    let run = finish(
        Command::new(env!("CARGO_BIN_EXE_tokenless"))
            .arg("init")
            .arg("--list")
            .env_remove("ANOLISA_ADAPTER_DIR")
            .env("HOME", &fixture.home)
            .env("XDG_DATA_HOME", &fixture.data_root)
            .stdin(Stdio::null())
            .output()
            .expect("run tokenless init"),
    );

    run.assert_code(0);
    assert!(
        run.stdout
            .contains(&format!("Adapter directory: {expected}")),
        "adapter directory was not discovered:\n{}",
        run.stdout
    );
}

#[test]
fn discovery_skips_a_relative_xdg_data_home() {
    if system_adapter_dir_present() {
        // A real system installation would answer the fallback and hide the
        // rejection this case asserts.
        return;
    }
    let fixture = Fixture::new(&[Target::installable("one")]);

    let run = finish(
        Command::new(env!("CARGO_BIN_EXE_tokenless"))
            .arg("init")
            .arg("--list")
            .env_remove("ANOLISA_ADAPTER_DIR")
            .env("HOME", &fixture.home)
            .env("XDG_DATA_HOME", "relative/data")
            .stdin(Stdio::null())
            .output()
            .expect("run tokenless init"),
    );

    run.assert_code(1);
    assert!(
        run.combined().contains("Adapter directory not found."),
        "unexpected error:\n{}",
        run.combined()
    );
}

#[test]
fn missing_adapter_directory_explains_the_override() {
    if system_adapter_dir_present() {
        return;
    }
    let root = TempDir::new().expect("create empty root");

    let run = finish(
        Command::new(env!("CARGO_BIN_EXE_tokenless"))
            .arg("init")
            .arg("--list")
            .env_remove("ANOLISA_ADAPTER_DIR")
            .env("HOME", root.path().join("home"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .stdin(Stdio::null())
            .output()
            .expect("run tokenless init"),
    );

    run.assert_code(1);
    assert!(
        run.combined().contains("ANOLISA_ADAPTER_DIR"),
        "error did not name the override:\n{}",
        run.combined()
    );
}

// ---------------------------------------------------------------------------
// P1-02 (round 2) — a newer CLI meeting older adapter resources
// ---------------------------------------------------------------------------

/// Detector bodies the pre-contract adapter tree shipped. Codex probed the
/// tokenless binary and Qoder probed host-CLI capability; both exited 0 without
/// ever asking the host whether the adapter was registered.
const PRE_CONTRACT_CODEX_DETECT: &str = "#!/usr/bin/env bash\necho '{\"installed\": true, \"version\": \"9.9.9\", \"path\": \"/usr/bin/tokenless\"}'\nexit 0\n";
const PRE_CONTRACT_QODER_DETECT: &str =
    "#!/usr/bin/env bash\necho '[tokenless] qoder: detected compatible qodercli'\nexit 0\n";

#[test]
fn pre_contract_tree_refuses_targets_whose_detector_only_probed_the_host() {
    let fixture = Fixture::legacy(&[
        Target::new("codex", PRE_CONTRACT_CODEX_DETECT),
        Target::new("qoder", PRE_CONTRACT_QODER_DETECT),
    ]);

    let list = run(&fixture, &["init", "--list"]);
    list.assert_code(0);
    assert_eq!(status_of(&list.stdout, "codex"), "unverified");
    assert_eq!(status_of(&list.stdout, "qoder"), "unverified");
    assert!(
        list.stdout.contains("predate the detector status contract"),
        "the stale resources were not explained:\n{}",
        list.stdout
    );

    // The named command has to fail *before* invoking an installer whose exit 0
    // would not mean the adapter was registered.
    let named = run(&fixture, &["init", "--framework", "codex"]);
    named.assert_code(1);
    assert!(
        named
            .combined()
            .contains("cannot be installed from the adapter resources"),
        "unexpected reason:\n{}",
        named.combined()
    );
    assert!(
        !named.combined().contains("installed successfully"),
        "a registration that never happened was reported as success:\n{}",
        named.combined()
    );
    assert!(
        fixture.installed().is_empty(),
        "an installer ran: {:?}",
        fixture.installed()
    );

    // Batch mode skips them, which is what the current tree does for a target
    // whose prerequisites are missing.
    let batch = run(&fixture, &["init", "--all"]);
    batch.assert_code(0);
    assert!(
        batch
            .stdout
            .contains("No frameworks are waiting for installation.")
    );
    assert!(fixture.installed().is_empty());
}

#[test]
fn pre_contract_codex_with_an_unusable_host_pin_never_reports_success() {
    // The reported repro: a pre-contract tree plus CODEX_BIN pointing at a file
    // that does not exist. The old detector only checked the tokenless binary,
    // so it exited 0, the installer printed "skipping plugin registration" and
    // the CLI reported a successful install that registered nothing.
    let fixture = Fixture::legacy(&[Target::new("codex", PRE_CONTRACT_CODEX_DETECT)]);
    let unusable_pin = fixture.stub_bin().join("absent-codex");

    let named = finish(
        fixture
            .command()
            .args(["init", "--framework", "codex"])
            .env("CODEX_BIN", &unusable_pin)
            .output()
            .expect("run tokenless init"),
    );
    named.assert_code(1);
    assert!(
        !named.combined().contains("installed successfully"),
        "the named command reported success:\n{}",
        named.combined()
    );
    assert!(
        fixture.installed().is_empty(),
        "the named command ran the installer: {:?}",
        fixture.installed()
    );

    let batch = finish(
        fixture
            .command()
            .args(["init", "--all"])
            .env("CODEX_BIN", &unusable_pin)
            .output()
            .expect("run tokenless init"),
    );
    batch.assert_code(0);
    assert!(
        !batch.combined().contains("installed successfully"),
        "batch mode reported success:\n{}",
        batch.combined()
    );
    assert!(fixture.installed().is_empty());
}

/// The shipped OpenCode detector already used the tri-state contract before the
/// manifest marker existed, so a pre-contract tree must keep trusting it.
#[test]
#[cfg(unix)]
fn pre_contract_tree_keeps_a_verified_opencode_registration_ready() {
    let fixture = Fixture::legacy(&[Target::new("opencode", DETECT_INSTALLABLE)]);
    // Replace the stub with the shipped detector and give it what it probes: a
    // plugin source, a host CLI that reports a version, and the symlink that
    // registers the plugin.
    fs::copy(
        bundled_adapters().join("opencode/scripts/detect.sh"),
        fixture.adapter_dir.join("opencode/scripts/detect.sh"),
    )
    .expect("copy shipped detector");
    let plugin = fixture.adapter_dir.join("opencode/plugin.js");
    fs::write(&plugin, "// tokenless opencode plugin\n").expect("write plugin source");

    let host = fixture.stub_bin().join("opencode");
    write_script(&host, "#!/bin/sh\necho '1.2.3'\n");
    let config = fixture._root.path().join("opencode-config");
    fs::create_dir_all(config.join("plugins")).expect("create opencode config");
    std::os::unix::fs::symlink(&plugin, config.join("plugins/tokenless.js"))
        .expect("register the plugin");

    let mut list = fixture.command();
    list.args(["init", "--list"])
        .env("OPENCODE_BIN", &host)
        .env("TOKENLESS_OPENCODE_CONFIG_DIR", &config);
    let list = finish(list.output().expect("run tokenless init"));
    list.assert_code(0);
    assert_eq!(status_of(&list.stdout, "opencode"), "ready");
    assert!(
        !list.stdout.contains("predate the detector status contract"),
        "a readable pre-contract detector was reported as stale:\n{}",
        list.stdout
    );

    let mut named = fixture.command();
    named
        .args(["init", "--framework", "opencode"])
        .env("OPENCODE_BIN", &host)
        .env("TOKENLESS_OPENCODE_CONFIG_DIR", &config);
    let named = finish(named.output().expect("run tokenless init"));
    named.assert_code(0);
    assert!(
        named
            .stdout
            .contains("opencode is already installed and ready."),
        "a verified registration was not reported ready:\n{}",
        named.stdout
    );
    assert!(
        fixture.installed().is_empty(),
        "a ready adapter was reinstalled: {:?}",
        fixture.installed()
    );
}

#[test]
fn the_contract_marker_is_what_unlocks_exit_zero_as_ready() {
    // Identical detectors and identical exit codes: only the manifest marker
    // differs, so this pins the marker as the thing that decides.
    let fixture = Fixture::new(&[
        Target::new("probejson", PRE_CONTRACT_CODEX_DETECT),
        Target::new("probetext", PRE_CONTRACT_QODER_DETECT),
    ]);

    let list = run(&fixture, &["init", "--list"]);
    list.assert_code(0);
    assert_eq!(status_of(&list.stdout, "probejson"), "ready");
    assert_eq!(status_of(&list.stdout, "probetext"), "ready");
    assert!(
        !list.stdout.contains("predate the detector status contract"),
        "a declaring tree was reported as stale:\n{}",
        list.stdout
    );

    let named = run(&fixture, &["init", "--framework", "probejson"]);
    named.assert_code(0);
    assert!(
        fixture.installed().is_empty(),
        "a ready target was reinstalled: {:?}",
        fixture.installed()
    );
}

#[test]
fn a_pre_contract_tree_still_honours_a_published_status() {
    // A detector that describes itself is trusted whatever the tree declares, so
    // a partially refreshed tree — new scripts, manifest not yet regenerated —
    // is neither forced back to installable nor refused.
    let publishing = "#!/usr/bin/env bash\necho '{\"status\": \"ready\"}'\nexit 0\n";
    let fixture = Fixture::legacy(&[
        Target::new("one", publishing),
        Target::new("codex", publishing),
    ]);

    let list = run(&fixture, &["init", "--list"]);
    list.assert_code(0);
    assert_eq!(status_of(&list.stdout, "one"), "ready");
    assert_eq!(status_of(&list.stdout, "codex"), "ready");
    assert!(!list.stdout.contains("predate the detector status contract"));
}

// ---------------------------------------------------------------------------
// P2-04 — the shipped Codex detector against an unusable host
// ---------------------------------------------------------------------------

/// Source directory of the adapters this crate ships with.
fn bundled_adapters() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../adapters/tokenless")
        .canonicalize()
        .expect("bundled adapter directory")
}

/// An adapter tree holding only the *shipped* Codex detector, so the contract
/// under test is the real script rather than a stub of it.
struct CodexFixture {
    _root: TempDir,
    adapter_dir: PathBuf,
    home: PathBuf,
    data_root: PathBuf,
    /// Put on PATH ahead of the system directories.
    bin: PathBuf,
}

impl CodexFixture {
    fn new() -> Self {
        let root = TempDir::new().expect("create codex fixture root");
        let adapter_dir = root.path().join("data/anolisa/adapters/tokenless");
        let scripts = adapter_dir.join("codex/scripts");
        fs::create_dir_all(&scripts).expect("create codex script directory");
        let bundled = bundled_adapters().join("codex/scripts");
        for name in ["detect.sh", "_common.sh"] {
            fs::copy(bundled.join(name), scripts.join(name)).expect("copy shipped detector");
        }
        fs::write(
            adapter_dir.join("manifest.json"),
            concat!(
                r#"{"component":"tokenless","version":"0.0.0-test","#,
                r#""detectContract":"status-v1","#,
                r#""targets":{"codex":{"compatibleVersions":"*","#,
                r#""actions":{"detect":"codex/scripts/detect.sh"}}}}"#
            ),
        )
        .expect("write manifest");
        let home = root.path().join("home");
        let data_root = root.path().join("data");
        let bin = root.path().join("bin");
        fs::create_dir_all(&home).expect("create fixture home");
        fs::create_dir_all(&bin).expect("create fixture bin");
        Self {
            _root: root,
            adapter_dir,
            home,
            data_root,
            bin,
        }
    }

    /// A `codex` the shipped resolver can find.
    fn with_codex_stub(&self) -> &Self {
        write_script(&self.bin.join("codex"), "#!/bin/sh\nexit 0\n");
        self
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tokenless"));
        command
            .args(args)
            .env("ANOLISA_ADAPTER_DIR", &self.adapter_dir)
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", &self.data_root)
            // System directories stay reachable so Bash itself is found; the
            // fixture bin comes first so a stub host wins over any real one.
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
            .env_remove("CODEX_BIN")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn marketplace(&self) -> PathBuf {
        self.data_root.join("anolisa/codex-marketplace")
    }
}

#[test]
fn shipped_codex_detector_refuses_an_unusable_host_pin() {
    let fixture = CodexFixture::new();

    // Bundled resources, a tokenless binary the detector can resolve, and
    // CODEX_BIN pointing at a file that does not exist. An explicit pin is
    // authoritative, so the resolver must not substitute another codex.
    let run = finish(
        fixture
            .command(&["init", "--framework", "codex"])
            .env("CODEX_BIN", fixture.bin.join("absent-codex"))
            .output()
            .expect("run tokenless init"),
    );

    run.assert_code(1);
    assert!(
        run.combined()
            .contains("codex is not installable (status: missing prereqs)"),
        "unexpected result:\n{}",
        run.combined()
    );
    assert!(
        !run.combined().contains("installed successfully"),
        "a host that cannot register was reported as installed:\n{}",
        run.combined()
    );
    assert!(
        !run.combined().contains("skipping plugin registration"),
        "install.sh ran although registration was impossible:\n{}",
        run.combined()
    );
    assert!(
        !fixture.marketplace().exists(),
        "a marketplace tree was created for an unusable host"
    );
}

#[test]
fn shipped_codex_detector_is_installable_with_a_resolvable_host() {
    let fixture = CodexFixture::new();
    fixture.with_codex_stub();

    let list = finish(
        fixture
            .command(&["init", "--list"])
            .output()
            .expect("run tokenless init"),
    );

    list.assert_code(0);
    assert_eq!(status_of(&list.stdout, "codex"), "installable");
    assert!(
        !list.stdout.contains("predate the detector status contract"),
        "the bundled tree was reported as stale:\n{}",
        list.stdout
    );
}

// ---------------------------------------------------------------------------
// P2-05 — the Bash prerequisite is an error, not an adapter status
// ---------------------------------------------------------------------------

#[test]
fn missing_interpreter_is_reported_instead_of_an_adapter_status() {
    let fixture = Fixture::new(&[Target::installable("one")]);
    let empty_bin = fixture.empty_bin();

    let batch = finish(
        fixture
            .command_without_bash(&empty_bin)
            .args(["init", "--all"])
            .output()
            .expect("run tokenless init"),
    );
    batch.assert_code(1);
    assert_eq!(status_of(&batch.stdout, "one"), "unavailable");
    assert!(
        batch.combined().contains("Bash"),
        "the missing prerequisite was not named:\n{}",
        batch.combined()
    );
    assert!(
        !batch
            .combined()
            .contains("No frameworks are waiting for installation"),
        "a host that cannot run detectors reported nothing to install:\n{}",
        batch.combined()
    );
    assert!(
        fixture.installed().is_empty(),
        "something was installed without an interpreter: {:?}",
        fixture.installed()
    );

    let list = finish(
        fixture
            .command_without_bash(&empty_bin)
            .args(["init", "--list"])
            .output()
            .expect("run tokenless init"),
    );
    list.assert_code(1);
    assert!(list.combined().contains("Bash"), "{}", list.combined());

    let named = finish(
        fixture
            .command_without_bash(&empty_bin)
            .args(["init", "--framework", "one"])
            .output()
            .expect("run tokenless init"),
    );
    named.assert_code(1);
    assert!(fixture.installed().is_empty());
}

#[test]
fn a_detector_that_runs_is_not_reported_as_unavailable() {
    // Guard against the launcher check firing on a host that does have Bash.
    let fixture = Fixture::new(&[Target::installable("one")]);

    let run = run(&fixture, &["init", "--list"]);

    run.assert_code(0);
    assert_eq!(status_of(&run.stdout, "one"), "installable");
    assert!(!run.combined().contains("unavailable"), "{}", run.stdout);
}
