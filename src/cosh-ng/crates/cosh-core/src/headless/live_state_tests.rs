use std::fs;
use std::sync::MutexGuard;

use clap::Parser;

use super::*;
use crate::provider::{mock::MockProvider, Message};

fn disabled_telemetry() -> (
    crate::sls::tests::EnvVarGuard,
    crate::sls::tests::EnvVarGuard,
    tempfile::TempDir,
    MutexGuard<'static, ()>,
) {
    let lock = crate::sls::tests::ENV_TEST_MUTEX.lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let disabled = root.path().join("disabled");
    fs::write(&disabled, "").unwrap();
    let id = crate::sls::tests::EnvVarGuard::set(
        "COSH_INSTALLATION_ID_PATH",
        root.path().join("installation_id").to_str().unwrap(),
    );
    let disabled = crate::sls::tests::EnvVarGuard::set(
        "COSH_TELEMETRY_DISABLED_PATH",
        disabled.to_str().unwrap(),
    );
    (id, disabled, root, lock)
}

fn background_compaction_fixture() -> (tempfile::TempDir, SessionRuntime, CoshCore) {
    let root = tempfile::tempdir().expect("temporary session store");
    let store = SessionStore::for_test_workspace(
        root.path().join("sessions").to_str().unwrap(),
        root.path(),
    )
    .expect("isolated store");
    let workspace_scope = store.workspace_scope().to_string();
    let mut runtime = SessionRuntime {
        store: Some(store),
        live_lease: None,
        record: PersistedSession::new(
            ProviderSessionId::new(),
            workspace_scope.clone(),
            "mock-model".to_string(),
            Vec::new(),
        ),
        workspace_scope,
        auto_persist: true,
        resumed: false,
        notice: PersistenceNotice::default(),
    };
    let mut config = CoreConfig::default();
    config.session.compaction.auto = false;
    let mut engine = CoshCore::new_legacy(
        config,
        Box::new(MockProvider::history_echo()),
        crate::tool::ToolRegistry::new(),
    );
    engine.session_id = runtime.record.session_id.to_string();
    engine.messages = vec![
        Message::user("original command api_key=sk-explicit-historical-secret"),
        Message::assistant("original completed answer"),
    ];
    runtime.persist(&engine).expect("persist completed turn");
    (root, runtime, engine)
}

fn commit_background_projection(runtime: &SessionRuntime, rewrite: bool) -> u64 {
    let store = runtime.store.as_ref().unwrap();
    let mut saved = store
        .load(&runtime.record.session_id)
        .expect("load session");
    if rewrite {
        saved.messages[0] = Message::user("foreign rewritten history");
    }
    saved.compaction = Some(crate::compaction::CompactionState {
        revision: 1,
        compacted_through: saved.messages.len(),
        summary: "BACKGROUND_APPROVED_SUMMARY".to_string(),
        model: saved.model.clone(),
        prompt_version: 1,
        source_digest: crate::compaction::source_digest(&saved.messages),
        tokens_before: None,
        tokens_after: None,
        created_at_ms: 1,
    });
    saved.compaction_revision = 1;
    store
        .persist(&mut saved)
        .expect("commit background projection");
    saved.generation
}

async fn next_background_compaction_turn(
    runtime: &mut SessionRuntime,
    engine: &mut CoshCore,
) -> String {
    let args = CliArgs::try_parse_from(["cosh-core", "--headless"]).unwrap();
    let live = crate::registry::LiveExtensionRuntime::new(
        engine.extension_generation.clone(),
        false,
        false,
        None,
    );
    let mut extensions = HeadlessExtensionRuntime {
        manager: None,
        live: &live,
    };
    let mut protocol = ProtocolState::new(args.execution_profile);
    let mut lines = tokio::io::BufReader::new(&b""[..]).lines();
    let mut output = Vec::new();
    let line = r#"{"type":"user","message":{"role":"user","content":"next command api_key=sk-explicit-next-secret"}}"#;
    let result = process_input_line(
        line,
        engine,
        &mut lines,
        &mut output,
        &args,
        runtime,
        &mut extensions,
        &mut protocol,
    )
    .await;
    assert!(matches!(result, InputLineResult::Continue));
    String::from_utf8(output).expect("UTF-8 protocol output")
}

// SLS reads these process-global paths during the awaited turn.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn background_compaction_reaches_next_provider_request() {
    let _environment = disabled_telemetry();
    let (_root, mut runtime, mut engine) = background_compaction_fixture();
    let generation = commit_background_projection(&runtime, false);
    let output = next_background_compaction_turn(&mut runtime, &mut engine).await;
    assert!(output.contains("<compacted-history"), "{output}");
    assert!(output.contains("BACKGROUND_APPROVED_SUMMARY"), "{output}");
    assert!(!output.contains("original command"), "{output}");
    assert!(output.contains("sk-explicit-next-secret"), "{output}");
    assert!(engine.messages[0]
        .content
        .as_text()
        .contains("sk-explicit-historical-secret"));
    assert_eq!(engine.compaction.revision(), 1);
    assert_eq!(runtime.record.generation, generation + 1);
    let saved = runtime
        .store
        .as_ref()
        .unwrap()
        .load(&runtime.record.session_id)
        .unwrap();
    assert!(saved.compaction.is_some());
    assert_eq!(saved.messages.len(), 4);
    assert!(!output.contains("SessionConflict"), "{output}");
}

// SLS reads these process-global paths during the awaited turn.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn background_compaction_rejects_changed_transcript() {
    let _environment = disabled_telemetry();
    let (_root, mut runtime, mut engine) = background_compaction_fixture();
    commit_background_projection(&runtime, true);
    let output = next_background_compaction_turn(&mut runtime, &mut engine).await;
    assert!(!output.contains("<compacted-history"), "{output}");
    assert!(!output.contains("foreign rewritten history"), "{output}");
    assert!(output.contains("original command"), "{output}");
    assert!(output.contains("sk-explicit-historical-secret"), "{output}");
    assert!(engine.compaction.state().is_none());
    assert_eq!(engine.compaction.revision(), 0);
}

// SLS reads these process-global paths during the awaited turn.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn background_compaction_preserves_rejected_projection_revision() {
    let _environment = disabled_telemetry();
    let (_root, mut runtime, mut engine) = background_compaction_fixture();
    let generation = commit_background_projection(&runtime, false);
    let path = runtime
        .store
        .as_ref()
        .unwrap()
        .session_file(&runtime.record.session_id);
    let mut stored: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    stored["compaction"]["source_digest"] = serde_json::json!("invalid digest");
    fs::write(path, serde_json::to_vec(&stored).unwrap()).unwrap();
    let output = next_background_compaction_turn(&mut runtime, &mut engine).await;
    assert!(!output.contains("<compacted-history"), "{output}");
    assert!(output.contains("original command"), "{output}");
    assert!(engine.compaction.state().is_none());
    assert_eq!(engine.compaction.revision(), 1);
    assert_eq!(runtime.record.generation, generation + 1);
    assert!(runtime
        .store
        .as_ref()
        .unwrap()
        .load(&runtime.record.session_id)
        .unwrap()
        .compaction
        .is_none());
}

#[test]
fn compaction_fixture_preserves_telemetry_environment() {
    let _lock = crate::sls::tests::ENV_TEST_MUTEX.lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let id = root.path().join("installation-id");
    let disabled = root.path().join("disabled");
    fs::write(&disabled, "").unwrap();
    let _id =
        crate::sls::tests::EnvVarGuard::set("COSH_INSTALLATION_ID_PATH", id.to_str().unwrap());
    let _disabled = crate::sls::tests::EnvVarGuard::set(
        "COSH_TELEMETRY_DISABLED_PATH",
        disabled.to_str().unwrap(),
    );
    let fixture = background_compaction_fixture();
    drop(fixture);
    assert_eq!(
        std::env::var_os("COSH_INSTALLATION_ID_PATH").unwrap(),
        id.as_os_str()
    );
    assert_eq!(
        std::env::var_os("COSH_TELEMETRY_DISABLED_PATH").unwrap(),
        disabled.as_os_str()
    );
}
