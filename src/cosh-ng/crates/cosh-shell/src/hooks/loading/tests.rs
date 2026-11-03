use std::fs;

use super::*;

#[test]
fn user_hooks_dir_uses_copilot_shell_cosh_dir() {
    let home = PathBuf::from("/tmp/cosh-shell-home");

    assert_eq!(
        user_hooks_dir_for_home(&home),
        PathBuf::from("/tmp/cosh-shell-home/.copilot-shell/cosh/hooks")
    );
}

#[test]
fn project_hook_root_from_cwd_walks_up_to_cosh_hooks() {
    let root = std::env::temp_dir().join("cosh-shell-project-root-walk");
    let nested = root.join("src/bin");
    let hooks_dir = root.join(".cosh/hooks");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&nested).expect("create nested dir");
    fs::create_dir_all(&hooks_dir).expect("create hooks dir");

    let found = project_hook_root_from_cwd(&nested).expect("project hook root");
    assert_eq!(found, root.canonicalize().expect("canonical root"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn project_hook_root_from_cwd_returns_none_without_hooks_dir() {
    let root = std::env::temp_dir().join("cosh-shell-project-root-missing");
    let nested = root.join("src");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&nested).expect("create nested dir");

    assert!(project_hook_root_from_cwd(&nested).is_none());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn policy_root_trust_matches_a_canonicalized_root() {
    let root = std::env::temp_dir().join("cosh-shell-project-root-trusted");
    let nested = root.join("src");
    let other = std::env::temp_dir().join("cosh-shell-project-root-other");
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&other);
    fs::create_dir_all(&nested).expect("create nested dir");
    fs::create_dir_all(&other).expect("create other dir");

    // The intent this case was written for: an administrator policy root
    // written as `nested/..` must still cover the project. It used to test
    // `is_trusted_project_root`, which content binding replaced with
    // `project_hook_trust`, so the canonicalization is asserted there.
    assert_eq!(
        crate::config::project_hook_trust(
            &root,
            std::slice::from_ref(&nested.join("..")),
            &[],
            None
        ),
        ProjectHookTrust::Trusted
    );
    assert_eq!(
        crate::config::project_hook_trust(&root, std::slice::from_ref(&other), &[], None),
        ProjectHookTrust::Untrusted
    );

    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&other);
}

// The project-hook trust cases live here rather than beside the engine tests
// because this module is compiled only into the binary target: the library
// target reaches the engine through `hooks/public.rs`, and a test declared
// in a shared module is inventoried twice by check-test-inventory.sh.
//
// Only trust *decisions* and *digests* belong here. A case that executes a
// hook starts a real shell, which needs the platform gating and
// serialization `tests/raw_cli/external_hook.rs` owns; declaring it in `src`
// instead hides that cost from the source-heavy test inventory.
use crate::config::ProjectHookTrust;
#[cfg(unix)]
use crate::hooks::HookEngine;

/// Builds a unique temp path so parallel tests never share a project root.
fn unique_temp_path(prefix: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{}-{unique}", std::process::id()))
}

#[cfg(unix)]
fn write_executable(path: &std::path::Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
fn write_project_hook(hooks_dir: &std::path::Path, marker: &std::path::Path, hook_id: &str) {
    let body = format!(
        "#!/bin/sh\n# cosh-hook: {hook_id}\n# match-commands: echo\ntouch '{}'\nprintf '{{\"hook_id\":\"{hook_id}\",\"severity\":\"warning\",\"title\":\"t\",\"description\":\"d\",\"suggestion\":\"s\"}}'\n",
        marker.display()
    );
    write_executable(&hooks_dir.join("project.sh"), &body);
}

/// Runs the production pin-then-decide path and reports the decision with
/// the fingerprint that decision was made from.
#[cfg(unix)]
fn trust_over_pinned_project(
    project: &std::path::Path,
    store: &std::path::Path,
    policy_roots: &[PathBuf],
) -> (ProjectHookTrust, Option<String>) {
    let consented =
        crate::config::read_consented_project_hooks_from_store_path(store).expect("read store");
    let mut engine = HookEngine::new();
    let trust = load_project_hooks_with_consent(&mut engine, project, policy_roots, &consented);
    (trust, engine.project_hooks_pinned_digest())
}

/// Records consent exactly as `/hooks trust-project` does: the root plus the
/// fingerprint of the snapshot the loader pinned, never a second copy read
/// by path.
#[cfg(unix)]
fn consent_project(store: &std::path::Path, project: &std::path::Path) -> Option<String> {
    let digest = HookEngine::project_hooks_pinned_digest_for_root(project);
    crate::config::add_consented_project_hook_to_store_path(store, project, digest.as_deref())
        .expect("persist trust");
    digest
}

/// D01: approving a project hook binds the approved *content*. When the
/// project owner replaces the hook body, a later session must withhold
/// execution until the user consents again; a path-only trust entry must not
/// carry the new body. Execution itself is asserted in
/// `tests/raw_cli/external_hook.rs`, which can start a real shell.
#[cfg(unix)]
#[test]
fn changed_project_hook_content_loses_trust_until_reconsent() {
    let project = unique_temp_path("cosh_hook_d01_content_change");
    let hooks_dir = project.join(".cosh/hooks");
    let store = unique_temp_path("cosh_hook_d01_store");
    let _ = fs::remove_dir_all(&project);
    let _ = fs::remove_file(&store);
    fs::create_dir_all(&hooks_dir).unwrap();

    let marker_v1 = project.join("executed-v1.marker");
    let marker_v2 = project.join("executed-v2.marker");
    write_project_hook(&hooks_dir, &marker_v1, "project-hook-v1");

    let approved = consent_project(&store, &project);
    assert!(
        approved.is_some(),
        "a real hook tree must be fingerprintable"
    );

    // Session 1: the approved content is trusted.
    let (trust, digest) = trust_over_pinned_project(&project, &store, &[]);
    assert_eq!(trust, ProjectHookTrust::Trusted, "approved content");
    assert_eq!(digest.as_deref(), approved.as_deref());

    // The project owner (a different UID on a shared checkout) replaces the body.
    write_project_hook(&hooks_dir, &marker_v2, "project-hook-v2");

    // Session 2: unchanged path consent, changed content.
    let (trust, changed) = trust_over_pinned_project(&project, &store, &[]);
    assert_eq!(
        trust,
        ProjectHookTrust::ContentChanged,
        "D01: changed content must not stay trusted"
    );
    assert_ne!(
        changed.as_deref(),
        approved.as_deref(),
        "the fingerprint must follow the body edit"
    );

    // Session 3: consenting again binds the new content.
    consent_project(&store, &project);
    let (trust, _) = trust_over_pinned_project(&project, &store, &[]);
    assert_eq!(trust, ProjectHookTrust::Trusted, "re-consent");

    let _ = fs::remove_file(&store);
    let _ = fs::remove_dir_all(&project);
}

/// D01: a store entry written before content binding existed carries no
/// approved fingerprint, so it must not be upgraded into consent for
/// whatever is on disk.
#[cfg(unix)]
#[test]
fn legacy_path_only_consent_does_not_trust_project_hook() {
    let project = unique_temp_path("cosh_hook_d01_legacy");
    let hooks_dir = project.join(".cosh/hooks");
    let store = unique_temp_path("cosh_hook_d01_legacy_store");
    let _ = fs::remove_dir_all(&project);
    let _ = fs::remove_file(&store);
    fs::create_dir_all(&hooks_dir).unwrap();

    let marker = project.join("executed.marker");
    write_project_hook(&hooks_dir, &marker, "project-hook");

    // Legacy format: a bare path with no digest.
    fs::write(
        &store,
        format!(
            "# cosh-shell trusted project hook roots\n{}\n",
            project.canonicalize().unwrap().display()
        ),
    )
    .unwrap();

    let (trust, digest) = trust_over_pinned_project(&project, &store, &[]);
    assert_eq!(
        trust,
        ProjectHookTrust::ContentChanged,
        "D01: legacy path-only entry must not grant trust"
    );

    // Positive control: the same pinned tree does become trusted once consent
    // carries its fingerprint. Without it the refusal above would also hold
    // in a build where project hooks cannot be pinned at all.
    assert!(
        digest.is_some(),
        "the tree is loadable, so the legacy refusal is about consent, not pinning"
    );
    consent_project(&store, &project);
    let (trust, _) = trust_over_pinned_project(&project, &store, &[]);
    assert_eq!(
        trust,
        ProjectHookTrust::Trusted,
        "a fingerprinted consent must grant trust"
    );

    let _ = fs::remove_file(&store);
    let _ = fs::remove_dir_all(&project);
}

/// An administrator-declared policy root keeps covering content updates,
/// because it can only come from `~/.copilot-shell/config.toml`, which the
/// project itself cannot write; only user consent is fingerprint-bound.
#[cfg(unix)]
#[test]
fn admin_policy_root_still_covers_changed_project_hook_content() {
    let project = unique_temp_path("cosh_hook_d01_policy");
    let hooks_dir = project.join(".cosh/hooks");
    let store = unique_temp_path("cosh_hook_d01_policy_store");
    let _ = fs::remove_dir_all(&project);
    let _ = fs::remove_file(&store);
    fs::create_dir_all(&hooks_dir).unwrap();

    let marker_v1 = project.join("executed-v1.marker");
    let marker_v2 = project.join("executed-v2.marker");
    write_project_hook(&hooks_dir, &marker_v1, "project-hook-v1");

    let policy = vec![project.canonicalize().unwrap()];
    let (trust, _) = trust_over_pinned_project(&project, &store, &policy);
    assert_eq!(trust, ProjectHookTrust::Trusted, "policy root");

    write_project_hook(&hooks_dir, &marker_v2, "project-hook-v2");
    let (trust, _) = trust_over_pinned_project(&project, &store, &policy);
    assert_eq!(
        trust,
        ProjectHookTrust::Trusted,
        "policy root covers content updates"
    );

    let _ = fs::remove_file(&store);
    let _ = fs::remove_dir_all(&project);
}

/// The fingerprint must measure what the loader pinned, so a tree the loader
/// refuses cannot be blessed: `.cosh` is opened with `O_NOFOLLOW`, a
/// symlinked component pins nothing, and the resulting digest no longer
/// matches the approval that was recorded for the real tree.
#[cfg(unix)]
#[test]
fn symlinked_hook_tree_pins_nothing_and_loses_recorded_consent() {
    use std::os::unix::fs::symlink;

    let base = unique_temp_path("cosh-shell-digest-symlink");
    let _ = fs::remove_dir_all(&base);
    let store = unique_temp_path("cosh-shell-digest-symlink-store");
    let _ = fs::remove_file(&store);
    let project = base.join("project");
    let hooks_dir = project.join(".cosh/hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    write_executable(
        &hooks_dir.join("project.sh"),
        "#!/bin/sh\n# cosh-hook: benign\n# match-commands: echo\n",
    );

    let approved = consent_project(&store, &project);
    assert!(approved.is_some(), "the real tree must be fingerprintable");
    let (trust, _) = trust_over_pinned_project(&project, &store, &[]);
    assert_eq!(trust, ProjectHookTrust::Trusted, "baseline consent");

    // The project owner now points `.cosh` at a tree the user never approved.
    let elsewhere = base.join("elsewhere/hooks");
    fs::create_dir_all(&elsewhere).unwrap();
    write_executable(
        &elsewhere.join("project.sh"),
        "#!/bin/sh\n# cosh-hook: smuggled\n# match-commands: echo\n",
    );
    fs::remove_dir_all(project.join(".cosh")).unwrap();
    symlink(base.join("elsewhere"), project.join(".cosh")).unwrap();

    let mut engine = HookEngine::new();
    let consented =
        crate::config::read_consented_project_hooks_from_store_path(&store).expect("read store");
    let trust = load_project_hooks_with_consent(&mut engine, &project, &[], &consented);
    assert!(
        engine.external_hooks().is_empty(),
        "a symlinked .cosh must pin nothing, got {:?}",
        engine.external_hooks()
    );
    assert_eq!(
        trust,
        ProjectHookTrust::ContentChanged,
        "consent for the real tree must not carry over to a symlinked one"
    );

    let _ = fs::remove_file(&store);
    let _ = fs::remove_dir_all(&base);
}

/// Loads a project's hooks on a worker thread and reports how many registered
/// plus their fingerprint.
///
/// The failure mode under test is a blocked `open`, which has no timeout of its
/// own: a read-only open of a FIFO waits for a writer that never comes. Measured
/// in-thread that hangs the whole test binary instead of failing, so the bound has
/// to sit outside the load — a wall-clock assertion around a call that never
/// returns is not a guard at all.
#[cfg(unix)]
fn pinned_within(
    project: &std::path::Path,
    timeout: std::time::Duration,
) -> (usize, Option<String>) {
    let display = project.display().to_string();
    let project = project.to_path_buf();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut engine = HookEngine::new();
        engine.load_project_hooks_from_root(&project, false);
        let pinned = (
            engine.external_hooks().len(),
            engine.project_hooks_pinned_digest(),
        );
        let _ = done_tx.send(pinned);
    });
    done_rx.recv_timeout(timeout).unwrap_or_else(|_| {
        panic!(
            "loading {display} blocked for longer than {timeout:?}: a project-supplied \
             special file reached a read-only open, which waits for a writer that never comes"
        )
    })
}

/// A project that already has consent must not be able to wedge the next
/// startup or `doctor` by dropping a FIFO *below* the hook directory: the
/// loader is not recursive, so the fingerprint never opens it.
#[cfg(unix)]
#[test]
fn nested_fifo_is_never_opened_by_the_fingerprint() {
    use std::os::unix::fs::FileTypeExt;

    let project = unique_temp_path("cosh-shell-fingerprint-fifo");
    let hooks_dir = project.join(".cosh/hooks");
    let _ = fs::remove_dir_all(&project);
    fs::create_dir_all(hooks_dir.join("sub")).unwrap();
    write_executable(
        &hooks_dir.join("project.sh"),
        "#!/bin/sh\n# cosh-hook: real\n# match-commands: echo\n",
    );

    let before = HookEngine::project_hooks_pinned_digest_for_root(&project)
        .expect("the real hook must be fingerprintable");

    let fifo = hooks_dir.join("sub/block");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::from_bits_truncate(0o644))
        .expect("create fifo");
    assert!(
        fs::symlink_metadata(&fifo)
            .map(|metadata| metadata.file_type().is_fifo())
            .unwrap_or(false),
        "the fixture must actually be a FIFO"
    );

    let (loaded, after) = pinned_within(&project, std::time::Duration::from_secs(20));

    assert_eq!(loaded, 1, "only the real hook may be registered");
    assert_eq!(
        after.as_deref(),
        Some(before.as_str()),
        "an entry the loader cannot load must not enter the fingerprint"
    );

    let _ = fs::remove_dir_all(&project);
}

/// Writes a loadable hook whose body is extended to `size` with a sparse tail,
/// which costs no disk space but is exactly what a streaming reader sees.
#[cfg(unix)]
fn write_oversized_hook(path: &std::path::Path, hook_id: &str, size: u64) {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let mut file = fs::File::create(path).unwrap();
    file.write_all(
        format!("#!/bin/sh\n# cosh-hook: {hook_id}\n# match-commands: echo\n").as_bytes(),
    )
    .unwrap();
    file.set_len(size).unwrap();
    drop(file);
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0),
        size,
        "the fixture must really be {size} bytes"
    );
}

/// An oversized project hook is refused by the loader, so it is never
/// registered, never executed and never fingerprinted. The size gate has to sit
/// in the loader rather than only in the digest: a health check runs the loader
/// without ever reaching the fingerprint, and an unbounded read has already
/// allocated the whole file by the time a budget could reject it.
#[cfg(unix)]
#[test]
fn an_oversized_project_hook_is_refused_at_load() {
    let cap = crate::hooks::engine::MAX_PROJECT_HOOK_FINGERPRINT_BYTES;
    let project = unique_temp_path("cosh-shell-load-oversize");
    let hooks_dir = project.join(".cosh/hooks");
    let _ = fs::remove_dir_all(&project);
    fs::create_dir_all(&hooks_dir).unwrap();
    write_oversized_hook(&hooks_dir.join("huge.sh"), "huge", cap + 1);
    write_executable(
        &hooks_dir.join("ok.sh"),
        "#!/bin/sh\n# cosh-hook: ok\n# match-commands: echo\n",
    );

    let mut engine = HookEngine::new();
    engine.load_project_hooks_from_root(&project, false);
    let loaded: Vec<String> = engine
        .external_hooks()
        .iter()
        .map(|hook| {
            hook.path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .collect();
    assert!(loaded.is_empty(), "an over-budget tree must be discarded");
    assert!(engine.project_hooks_pinned_digest().is_none());
    fs::remove_file(hooks_dir.join("huge.sh")).unwrap();
    assert!(HookEngine::project_hooks_pinned_digest_for_root(&project).is_some());

    let _ = fs::remove_dir_all(&project);
}

/// The fingerprint streams and stops once the tree budget is spent, so a set of
/// individually loadable hooks that together exceed it fails closed instead of
/// being measured.
#[cfg(unix)]
#[test]
fn fingerprint_refuses_a_hook_tree_over_the_byte_budget() {
    let cap = crate::hooks::engine::MAX_PROJECT_HOOK_FINGERPRINT_BYTES;
    let project = unique_temp_path("cosh-shell-fingerprint-oversize");
    let hooks_dir = project.join(".cosh/hooks");
    let _ = fs::remove_dir_all(&project);
    fs::create_dir_all(&hooks_dir).unwrap();
    // Each file fits alone; their sum must be refused during loading.
    let half = cap / 2 + cap / 4;
    assert!(
        half <= cap && half * 2 > cap,
        "the fixture must straddle the per-file and per-tree caps"
    );
    write_oversized_hook(&hooks_dir.join("a.sh"), "a", half);
    write_oversized_hook(&hooks_dir.join("b.sh"), "b", half);

    assert_eq!(
        HookEngine::project_hooks_pinned_digest_for_root(&project),
        None,
        "a tree over the fingerprint budget must not produce a digest"
    );

    // Fail closed end to end: with no digest, recorded consent cannot match.
    // The stored fingerprint must be well formed to test the content decision.
    let store = unique_temp_path("cosh-shell-fingerprint-oversize-store");
    let _ = fs::remove_file(&store);
    let unrelated = format!("sha256:{}", "0".repeat(64));
    crate::config::add_consented_project_hook_to_store_path(&store, &project, Some(&unrelated))
        .expect("persist trust");
    let (trust, digest) = trust_over_pinned_project(&project, &store, &[]);
    assert_eq!(digest, None, "an unmeasurable tree has no fingerprint");
    assert_eq!(
        trust,
        ProjectHookTrust::ContentChanged,
        "an unmeasurable tree must not be trusted"
    );

    let _ = fs::remove_file(&store);
    let _ = fs::remove_dir_all(&project);
}

/// A FIFO at the **top** level of `.cosh/hooks` is the shape that used to block:
/// the loader opens every top-level entry, and a read-only open of a FIFO waits for
/// a writer. The entry type is now rejected before the open, with `O_NONBLOCK`
/// covering filesystems that do not report `d_type`. This is the same entry point
/// `cosh-shell doctor` and `/health` reach, so blocking here is a denial of service
/// a project can arrange at will; `tests/raw_cli/doctor.rs` covers both end to end.
#[cfg(unix)]
#[test]
fn top_level_fifo_is_refused_without_blocking_the_loader() {
    use std::os::unix::fs::FileTypeExt;

    let project = unique_temp_path("cosh-shell-load-fifo");
    let hooks_dir = project.join(".cosh/hooks");
    let _ = fs::remove_dir_all(&project);
    fs::create_dir_all(&hooks_dir).unwrap();
    write_executable(
        &hooks_dir.join("ok.sh"),
        "#!/bin/sh\n# cosh-hook: ok\n# match-commands: echo\n",
    );
    let fifo = hooks_dir.join("block");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::from_bits_truncate(0o644))
        .expect("create fifo");
    assert!(
        fs::symlink_metadata(&fifo)
            .map(|metadata| metadata.file_type().is_fifo())
            .unwrap_or(false),
        "the fixture must actually be a FIFO"
    );

    let (loaded, digest) = pinned_within(&project, std::time::Duration::from_secs(20));

    assert_eq!(
        loaded, 1,
        "only the real hook may be registered; the FIFO must be refused"
    );
    assert!(
        digest.is_some(),
        "a FIFO the loader refused must not make the tree unfingerprintable"
    );

    let _ = fs::remove_dir_all(&project);
}

/// The approved bytes are frozen, not merely pinned.
///
/// A descriptor fixes the inode, so pinning alone still lets a project truncate
/// and rewrite the same file after its fingerprint matched, and the executor
/// then reads the new bytes through that descriptor with no re-check. Project
/// hooks are therefore copied into a sealed snapshot at load, and the
/// fingerprint measures that snapshot: neither an in-place rewrite nor an atomic
/// path replacement changes what this engine holds. Linux-only, because the
/// snapshot is a sealed memfd; other platforms have no memfd and also no
/// `/proc/self/fd` for the executor to use.
#[cfg(target_os = "linux")]
#[test]
fn fingerprint_is_frozen_against_rewrite_and_path_swap() {
    let project = unique_temp_path("cosh-shell-fingerprint-frozen");
    let hooks_dir = project.join(".cosh/hooks");
    let _ = fs::remove_dir_all(&project);
    fs::create_dir_all(&hooks_dir).unwrap();
    let hook = hooks_dir.join("project.sh");
    let header = "#!/bin/sh\n# cosh-hook: pinned\n# match-commands: echo\n";
    write_executable(&hook, &format!("{header}# v1\n"));

    let mut engine = HookEngine::new();
    engine.load_project_hooks_from_root(&project, false);
    let approved = engine.project_hooks_pinned_digest().expect("digest v1");

    // Truncate and rewrite the same inode, which is what a project with write
    // access does to change a hook it has already had approved.
    write_executable(&hook, &format!("{header}# v2\n"));
    assert_eq!(
        engine.project_hooks_pinned_digest().as_deref(),
        Some(approved.as_str()),
        "an in-place rewrite reached the bytes this session will execute, so the \
             approved content is not frozen"
    );

    // Atomically replacing the path must not change it either.
    let replacement = hooks_dir.join("replacement.sh");
    write_executable(&replacement, &format!("{header}# v3\n"));
    fs::rename(&replacement, &hook).unwrap();
    assert_eq!(
        engine.project_hooks_pinned_digest().as_deref(),
        Some(approved.as_str()),
        "a path swap must not change what this engine pinned"
    );

    // Positive control: the rewrite is not ignored forever. A later session
    // pins the new content and gets a different digest, so consent recorded for
    // `approved` is rejected instead of blessing what is on disk now.
    let fresh = HookEngine::project_hooks_pinned_digest_for_root(&project).expect("digest v3");
    assert_ne!(
        fresh, approved,
        "a new pin must see the replaced content, otherwise consent would bless it"
    );

    let _ = fs::remove_dir_all(&project);
}

/// A legacy store line is a bare path, and a path may be any valid UTF-8.
/// The digest suffix is recognised by a fixed byte offset from the end, so a
/// multibyte path longer than the suffix puts that offset inside a character;
/// splitting there panics, and this parse runs on the startup path.
#[test]
fn legacy_store_line_with_a_multibyte_path_is_parsed_without_panicking() {
    // `" sha256:".len() + 64`. Asserted through the fixture below so this
    // case cannot silently stop reproducing the condition it exists for.
    const DIGEST_SUFFIX_LEN: usize = 72;
    let legacy = "/Users/张三/开发/项目/客户端/构建/中间产物/资源目录/配置文件/备份";
    assert!(
        legacy.len() > DIGEST_SUFFIX_LEN
            && !legacy.is_char_boundary(legacy.len() - DIGEST_SUFFIX_LEN),
        "the fixture must be longer than the suffix and split inside a character, \
             otherwise it no longer reproduces the panic"
    );

    let store = unique_temp_path("cosh-shell-trust-multibyte-store");
    let _ = fs::remove_file(&store);
    fs::write(
        &store,
        format!("# cosh-shell trusted project hook roots\n{legacy}\n"),
    )
    .unwrap();

    let entries = crate::config::read_consented_project_hooks_from_store_path(&store)
        .expect("a legacy multibyte path must parse, not panic");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].root, PathBuf::from(legacy));
    assert_eq!(
        entries[0].digest, None,
        "a bare path carries no fingerprint and must not gain one"
    );
    let _ = fs::remove_file(&store);
}

/// Recording consent for a tree that cannot be fingerprinted must fail
/// instead of writing a line that `project_hook_trust` can never accept:
/// the user would be told the trust was persisted while the hooks stay
/// withheld on every later start.
#[cfg(unix)]
#[test]
fn trusting_a_project_without_a_fingerprintable_tree_fails() {
    let project = unique_temp_path("cosh-shell-trust-no-tree");
    let store = unique_temp_path("cosh-shell-trust-no-tree-store");
    let _ = fs::remove_dir_all(&project);
    let _ = fs::remove_file(&store);
    fs::create_dir_all(&project).unwrap();

    let error = crate::config::trust_project_root_to_store_path(
        &store,
        &project,
        HookEngine::project_hooks_pinned_digest_for_root(&project).as_deref(),
    )
    .expect_err("consent without a fingerprintable tree must not be recorded");
    assert!(
        error.contains("cannot fingerprint"),
        "the failure must say why, got {error}"
    );
    assert!(
        crate::config::read_consented_project_hooks_from_store_path(&store)
            .expect("read store")
            .is_empty(),
        "a refused consent must not write an entry that can never be trusted"
    );

    // Positive control: the same call succeeds once a real hook tree exists,
    // so the refusal above is about the tree and not about the store path.
    let hooks_dir = project.join(".cosh/hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    write_project_hook(&hooks_dir, &project.join("executed.marker"), "project-hook");
    crate::config::trust_project_root_to_store_path(
        &store,
        &project,
        HookEngine::project_hooks_pinned_digest_for_root(&project).as_deref(),
    )
    .expect("persist trust");
    let entries =
        crate::config::read_consented_project_hooks_from_store_path(&store).expect("read store");
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0].digest.is_some(),
        "a recorded consent must carry the fingerprint of the approved content"
    );
    assert_eq!(
        trust_over_pinned_project(&project, &store, &[]).0,
        ProjectHookTrust::Trusted
    );

    let _ = fs::remove_file(&store);
    let _ = fs::remove_dir_all(&project);
}

/// A rollback leaves a bare legacy line beside the fingerprinted one for the
/// same root. The trust decision must not depend on which line comes first.
#[cfg(unix)]
#[test]
fn duplicate_store_lines_for_one_root_do_not_depend_on_line_order() {
    let project = unique_temp_path("cosh-shell-trust-duplicate");
    let hooks_dir = project.join(".cosh/hooks");
    let store = unique_temp_path("cosh-shell-trust-duplicate-store");
    let _ = fs::remove_dir_all(&project);
    let _ = fs::remove_file(&store);
    fs::create_dir_all(&hooks_dir).unwrap();
    write_project_hook(&hooks_dir, &project.join("executed.marker"), "project-hook");
    let root = project.canonicalize().unwrap();
    let digest = HookEngine::project_hooks_pinned_digest_for_root(&project).expect("digest");
    let fingerprinted = format!(
        "# cosh-project-hook-v2 {}",
        serde_json::to_string(&(&root, &digest)).unwrap()
    );
    let bare = root.display().to_string();

    for (label, content) in [
        ("fingerprinted-first", format!("{fingerprinted}\n{bare}\n")),
        ("legacy-first", format!("{bare}\n{fingerprinted}\n")),
    ] {
        fs::write(
            &store,
            format!("# cosh-shell trusted project hook roots\n{content}"),
        )
        .unwrap();
        let entries = crate::config::read_consented_project_hooks_from_store_path(&store)
            .expect("read store");
        assert_eq!(entries.len(), 2, "{label}: both lines are read");
        assert_eq!(
            trust_over_pinned_project(&project, &store, &[]).0,
            ProjectHookTrust::Trusted,
            "{label}: the fingerprinted consent must win regardless of line order"
        );
    }

    let _ = fs::remove_file(&store);
    let _ = fs::remove_dir_all(&project);
}

#[cfg(unix)]
#[test]
fn project_loader_counts_invalid_candidates_before_parsing() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let hooks = dir.path().join(".cosh/hooks");
    fs::create_dir_all(&hooks).unwrap();
    write_project_hook(&hooks, &dir.path().join("marker"), "positive");
    assert!(HookEngine::project_hooks_pinned_digest_for_root(dir.path()).is_some());
    for index in 0..64 {
        if index == 63 {
            assert!(HookEngine::project_hooks_pinned_digest_for_root(dir.path()).is_some());
        }
        let path = hooks.join(format!("invalid-{index}"));
        fs::write(&path, "no hook header").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut engine = HookEngine::new();
    engine.load_project_hooks_from_root(dir.path(), true);
    assert!(engine.external_hooks().is_empty());
    assert!(engine.project_hooks_pinned_digest().is_none());
}

#[cfg(unix)]
#[test]
fn project_loader_charges_invalid_bytes_and_discards_partial_snapshots() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let hooks = dir.path().join(".cosh/hooks");
    fs::create_dir_all(&hooks).unwrap();
    let boundary = hooks.join("boundary");
    write_oversized_hook(&boundary, "boundary", 4 * 1024 * 1024);
    assert!(HookEngine::project_hooks_pinned_digest_for_root(dir.path()).is_some());
    fs::OpenOptions::new()
        .write(true)
        .open(&boundary)
        .unwrap()
        .set_len(4 * 1024 * 1024 + 1)
        .unwrap();
    assert!(HookEngine::project_hooks_pinned_digest_for_root(dir.path()).is_none());
    fs::remove_file(&boundary).unwrap();
    for (index, header) in ["no header", "# cosh-hook: valid"].iter().enumerate() {
        let path = hooks.join(format!("hook-{index}"));
        let mut content = header.to_string();
        content.push_str(&"x".repeat(3 * 1024 * 1024));
        fs::write(&path, content).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut engine = HookEngine::new();
    engine.load_project_hooks_from_root(dir.path(), true);
    assert!(engine.external_hooks().is_empty());
    assert!(engine.project_hooks_pinned_digest().is_none());
}

#[cfg(unix)]
#[test]
fn legacy_digest_suffix_never_becomes_content_consent() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let prefix = dir.path().join("中文 project");
    let hooks = prefix.join(".cosh/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    write_project_hook(&hooks, &prefix.join("marker"), "prefix");
    let digest = HookEngine::project_hooks_pinned_digest_for_root(&prefix).unwrap();
    let root = dir.path().join(format!("中文 project {digest}"));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(&store, format!("{}\n", root.display())).unwrap();
    let entries = crate::config::read_consented_project_hooks_from_store_path(&store).unwrap();
    assert_eq!(entries[0].root, root);
    assert_eq!(entries[0].digest, None);
    assert_eq!(
        crate::config::project_hook_trust(&prefix, &[], &entries, Some(&digest)),
        ProjectHookTrust::Untrusted,
    );
}

#[test]
fn versioned_consent_is_ignored_by_rollback_path_readers() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let digest = format!("sha256:{}", "b".repeat(64));
    let root = dir.path().join(format!("中文 project {digest}\nnext"));
    std::fs::create_dir(&root).unwrap();
    crate::config::add_consented_project_hook_to_store_path(&store, &root, Some(&digest)).unwrap();
    let content = std::fs::read_to_string(&store).unwrap();
    let rollback_roots: Vec<_> = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(PathBuf::from)
        .collect();
    assert!(rollback_roots.is_empty(), "{content:?}");
    let entries = crate::config::read_consented_project_hooks_from_store_path(&store).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].root, root.canonicalize().unwrap());
    assert_eq!(entries[0].digest.as_deref(), Some(digest.as_str()));
}

#[test]
fn adding_content_consent_preserves_other_legacy_workspace_grants() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let legacy = dir.path().join("legacy project");
    let current = dir.path().join("current project");
    fs::create_dir(&legacy).unwrap();
    fs::create_dir(&current).unwrap();
    fs::write(&store, format!("{}\n", legacy.display())).unwrap();
    crate::config::add_consented_project_hook_to_store_path(
        &store,
        &current,
        Some(&format!("sha256:{}", "a".repeat(64))),
    )
    .unwrap();
    let content = fs::read_to_string(&store).unwrap();
    let roots: Vec<_> = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(PathBuf::from)
        .collect();
    assert_eq!(roots, vec![legacy]);
}
