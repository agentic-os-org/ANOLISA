use super::super::io::{
    private_open_options, reset_session_file_read_count, session_file_read_count, write_atomic_file,
};
use super::super::listing::ListEntry;
use super::super::summary::{
    MAX_PROMPT_PREVIEW_CHARS, MAX_SUMMARY_MODEL_BYTES, MAX_SUMMARY_WORKSPACE_BYTES,
};
use super::*;

#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStringExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn store(temp: &tempfile::TempDir) -> SessionStore {
    SessionStore::for_workspace(temp.path().join("sessions").to_str().unwrap(), temp.path())
        .unwrap()
}

fn new_session(store: &SessionStore, prompt: &str) -> PersistedSession {
    PersistedSession::new(
        ProviderSessionId::new(),
        store.workspace_scope().to_string(),
        "mock-model".to_string(),
        vec![Message::user(prompt), Message::assistant("done")],
    )
}

fn write_invalid_utf8_session(store: &SessionStore) -> ProviderSessionId {
    fs::create_dir_all(&store.base_dir).unwrap();
    let session_id = ProviderSessionId::new();
    fs::write(store.session_file(&session_id), b"{\"prompt\":\"\xff\"}").unwrap();
    session_id
}

#[test]
fn versioned_persist_and_load_round_trip() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut session = new_session(&store, "hello");

    store.persist(&mut session).unwrap();
    let loaded = store.load(&session.session_id).unwrap();

    assert_eq!(loaded.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(loaded.generation, 1);
    assert_eq!(loaded.messages.len(), 2);
    assert_eq!(loaded.workspace_scope, store.workspace_scope());
}

/// Builds a projection carrying `revision`.
///
/// The summary and digest are placeholders: these tests exercise the revision
/// clock at the persistence boundary, not projection validity.
fn projection(revision: u64) -> crate::compaction::CompactionState {
    crate::compaction::CompactionState {
        revision,
        compacted_through: 1,
        summary: "summary".to_string(),
        model: "mock-model".to_string(),
        prompt_version: 1,
        source_digest: "digest".to_string(),
        tokens_before: None,
        tokens_after: None,
        created_at_ms: 0,
    }
}

/// Reads the stored envelope as untyped JSON.
fn envelope_json(store: &SessionStore, session_id: &ProviderSessionId) -> serde_json::Value {
    let bytes = fs::read(store.session_file(session_id)).expect("read persisted session envelope");
    serde_json::from_slice(&bytes).expect("parse persisted session envelope JSON")
}

#[test]
fn persist_rejects_a_lower_compaction_revision_clock() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut session = new_session(&store, "hello");
    session.compaction_revision = 5;
    store.persist(&mut session).unwrap();

    // Same generation, lower clock: accepting this would let a later commit
    // republish a revision that reached disk once already.
    let mut regressing = session.clone();
    regressing.compaction_revision = 2;
    let error = store.persist(&mut regressing).unwrap_err();
    assert_eq!(error.code(), "conflict");
    assert_eq!(
        store.load(&session.session_id).unwrap().compaction_revision,
        5
    );
}

#[test]
fn persist_fails_closed_when_the_stored_clock_advanced_without_a_generation_bump() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut session = new_session(&store, "hello");
    store.persist(&mut session).unwrap();

    // An externally rewritten envelope raises the clock while leaving the
    // generation untouched, so the generation check alone cannot catch it.
    let mut value = envelope_json(&store, &session.session_id);
    value
        .as_object_mut()
        .unwrap()
        .insert("compaction_revision".to_string(), serde_json::json!(9));
    fs::write(
        store.session_file(&session.session_id),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();

    let mut stale = session.clone();
    let error = store.persist(&mut stale).unwrap_err();
    assert_eq!(error.code(), "conflict");
    // The tampered floor survives the rejected write.
    let reloaded = store.load(&session.session_id).unwrap();
    assert_eq!(reloaded.compaction_revision, 9);

    // Conflict keeps the normal recovery path open: reload, then commit above
    // the observed floor.
    let mut recovered = reloaded;
    recovered.compaction_revision = 10;
    store.persist(&mut recovered).unwrap();
    assert_eq!(
        store.load(&session.session_id).unwrap().compaction_revision,
        10
    );
}

#[test]
fn persist_raises_the_clock_to_the_stored_projection_revision() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut session = new_session(&store, "hello");
    // A caller that forgets to advance the clock must not leave a projection
    // whose revision the clock would forget on the next load.
    session.compaction = Some(projection(4));
    session.compaction_revision = 0;
    store.persist(&mut session).unwrap();

    assert_eq!(session.compaction_revision, 4);
    assert_eq!(
        envelope_json(&store, &session.session_id)["compaction_revision"],
        serde_json::json!(4)
    );
}

#[test]
fn persisted_and_loaded_sessions_are_redacted() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let secret = "sk-session-secret-value";
    let raw = Message::user(&format!("use api_key={secret}"));
    let mut session = PersistedSession::new(
        ProviderSessionId::new(),
        store.workspace_scope().to_string(),
        "mock-model".to_string(),
        vec![raw],
    );

    store.persist(&mut session).unwrap();

    // The on-disk envelope must not leak the secret, while the caller's
    // in-memory copy keeps the original turn context.
    let content = fs::read_to_string(store.session_file(&session.session_id)).unwrap();
    assert!(!content.contains(secret), "{content}");
    assert!(content.contains("<redacted>"), "{content}");
    assert!(session.messages[0].content.as_text().contains(secret));

    let loaded = store.load(&session.session_id).unwrap();
    assert!(!loaded.messages[0].content.as_text().contains(secret));
}

#[test]
fn persisted_projection_tracks_the_redacted_transcript() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let secret = "sk-projection-secret-value";
    let mut session = new_session(&store, &format!("use api_key={secret}"));
    let mut state = projection(1);
    state.compacted_through = session.messages.len();
    state.source_digest = crate::compaction::source_digest(&session.messages);
    let runtime_digest = state.source_digest.clone();
    session.compaction = Some(state);

    store.persist(&mut session).unwrap();

    let stored = envelope_json(&store, &session.session_id);
    assert!(!stored.to_string().contains(secret), "{stored}");
    assert_ne!(
        stored["compaction"]["source_digest"],
        serde_json::Value::String(runtime_digest)
    );
    let loaded = store.load(&session.session_id).unwrap();
    assert!(loaded.compaction.is_some());
    assert!(session.messages[0].content.as_text().contains(secret));
}

#[test]
fn persist_rejects_projection_outside_the_transcript() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let transcript_len = new_session(&store, "hello").messages.len();

    for compacted_through in [0, transcript_len + 1] {
        let mut session = new_session(&store, "hello");
        let mut state = projection(1);
        state.compacted_through = compacted_through;
        session.compaction = Some(state);

        let error = store.persist(&mut session).unwrap_err();

        assert_eq!(error.code(), "corrupt");
        assert!(!store.session_file(&session.session_id).exists());
    }
}

#[test]
fn load_bounds_oversized_model_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut session = new_session(&store, "bounded model on load");
    store.persist(&mut session).unwrap();
    let path = store.session_file(&session.session_id);
    let mut envelope: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    envelope["model"] = serde_json::Value::String("m".repeat(64 * 1024));
    fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();

    let loaded = store.load(&session.session_id).unwrap();

    assert!(loaded.model.len() <= MAX_SUMMARY_MODEL_BYTES);
    assert!(loaded.model.ends_with('…'));
}

#[test]
fn loading_legacy_sessions_redacts_before_replay() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let legacy_dir = workspace.join("sessions");
    fs::create_dir_all(&legacy_dir).unwrap();
    let store = SessionStore::for_workspace(DEFAULT_SESSION_PERSIST_DIR, &workspace).unwrap();
    let id = ProviderSessionId::new();
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz123456";
    let raw = Message {
        role: "user".to_string(),
        content: crate::provider::MessageContent::Text(format!("replay {secret}")),
        tool_call_id: None,
        name: None,
        tool_calls: None,
    };
    fs::write(
        legacy_dir.join(format!("{id}.json")),
        serde_json::to_vec(&vec![raw]).unwrap(),
    )
    .unwrap();

    let loaded = store.load(&id).unwrap();

    assert!(!loaded.messages[0].content.as_text().contains(secret));
    assert!(loaded.messages[0].content.as_text().contains("<redacted>"));
}

// Linux-only: macOS APFS rejects invalid UTF-8 filenames with EILSEQ before
// the scope-hashing rejection path can be exercised.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_workspace_paths_are_rejected_before_scope_hashing() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp
        .path()
        .join(std::ffi::OsString::from_vec(vec![b'w', 0xfe]));
    let second = temp
        .path()
        .join(std::ffi::OsString::from_vec(vec![b'w', 0xff]));
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();

    for workspace in [&first, &second] {
        assert!(matches!(
            SessionStore::for_workspace(DEFAULT_SESSION_PERSIST_DIR, workspace),
            Err(SessionError::InvalidRequest { ref message })
                if message.contains("not valid UTF-8")
        ));
    }
}

#[test]
fn summaries_are_newest_first_and_derive_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut older = new_session(&store, "first prompt");
    store.persist(&mut older).unwrap();
    let mut newer = new_session(&store, "second prompt");
    store.persist(&mut newer).unwrap();
    newer.updated_at_ms = older.updated_at_ms.saturating_add(10);
    store.persist(&mut newer).unwrap();

    let (summaries, cursor) = store.list(10, None, false).unwrap();

    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].session_id, newer.session_id);
    assert_eq!(summaries[0].first_prompt.as_deref(), Some("second prompt"));
    assert_eq!(summaries[0].message_count, 2);
    assert_eq!(summaries[0].health, SessionHealth::Ready);
    assert!(cursor.is_none());

    let (first_page, cursor) = store.list(1, None, false).unwrap();
    assert_eq!(first_page.len(), 1);
    let cursor = cursor.expect("second page cursor");
    let (second_page, final_cursor) = store.list(1, Some(&cursor), false).unwrap();
    assert_eq!(second_page.len(), 1);
    assert_ne!(first_page[0].session_id, second_page[0].session_id);
    assert!(final_cursor.is_none());
}

#[test]
fn summaries_bound_untrusted_model_and_mismatched_workspace_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut session = new_session(&store, "bounded metadata");
    session.model = "🧠".repeat(300_000);
    store.persist(&mut session).unwrap();

    for summary in [
        store.inspect(&session.session_id).unwrap(),
        store.validate(&session.session_id).unwrap(),
        store.list(1, None, false).unwrap().0.remove(0),
    ] {
        let model = summary.model.expect("bounded model");
        assert!(model.len() <= MAX_SUMMARY_MODEL_BYTES);
        assert!(model.ends_with('…'));
        assert!(summary.workspace_scope.len() <= MAX_SUMMARY_WORKSPACE_BYTES);
    }

    let mut mismatch = new_session(&store, "mismatched scope");
    mismatch.workspace_scope = "🗂".repeat(300_000);
    fs::write(
        store.session_file(&mismatch.session_id),
        serde_json::to_vec(&mismatch).unwrap(),
    )
    .unwrap();

    let inspected = store.inspect(&mismatch.session_id).unwrap();
    assert_eq!(inspected.health, SessionHealth::ScopeMismatch);
    assert!(inspected.workspace_scope.len() <= MAX_SUMMARY_WORKSPACE_BYTES);
    assert!(inspected.workspace_scope.ends_with('…'));
    let listed = store
        .list(MAX_LIST_LIMIT, None, false)
        .unwrap()
        .0
        .into_iter()
        .find(|summary| summary.session_id == mismatch.session_id)
        .expect("mismatched summary remains visible");
    assert_eq!(listed.health, SessionHealth::ScopeMismatch);
    assert!(listed.workspace_scope.len() <= MAX_SUMMARY_WORKSPACE_BYTES);
}

#[test]
fn stable_cursor_survives_deletion_of_previous_page() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut older = new_session(&store, "older");
    store.persist(&mut older).unwrap();
    let mut newer = new_session(&store, "newer");
    store.persist(&mut newer).unwrap();
    newer.updated_at_ms = older.updated_at_ms.saturating_add(10);
    store.persist(&mut newer).unwrap();

    let (first_page, cursor) = store.list(1, None, false).unwrap();
    assert_eq!(first_page[0].session_id, newer.session_id);
    fs::remove_file(store.session_file(&newer.session_id)).unwrap();

    let (second_page, final_cursor) = store
        .list(1, cursor.as_deref(), false)
        .expect("stable second page");

    assert_eq!(second_page.len(), 1);
    assert_eq!(second_page[0].session_id, older.session_id);
    assert!(final_cursor.is_none());
}

#[test]
fn invalid_list_cursor_is_typed() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);

    assert!(matches!(
        store.list(10, Some("not-a-cursor"), false),
        Err(SessionError::InvalidCursor { .. })
    ));
}

#[test]
fn list_keeps_healthy_sessions_visible_beside_invalid_utf8() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut healthy = new_session(&store, "healthy prompt");
    store.persist(&mut healthy).unwrap();
    let corrupt = write_invalid_utf8_session(&store);

    let (summaries, cursor) = store.list(10, None, false).unwrap();

    assert_eq!(summaries.len(), 2);
    assert!(cursor.is_none());
    assert!(summaries.iter().any(|summary| {
        summary.session_id == healthy.session_id && summary.health == SessionHealth::Ready
    }));
    assert!(summaries.iter().any(|summary| {
        summary.session_id == corrupt && summary.health == SessionHealth::Corrupt
    }));
}

#[test]
fn list_all_workspaces_includes_foreign_sessions_as_scope_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let persist_dir = temp.path().join("sessions");
    fs::create_dir_all(&persist_dir).unwrap();
    let local_workspace = temp.path().join("local");
    let foreign_workspace = temp.path().join("foreign");
    fs::create_dir_all(&local_workspace).unwrap();
    fs::create_dir_all(&foreign_workspace).unwrap();

    let local_store =
        SessionStore::for_workspace(persist_dir.to_str().unwrap(), &local_workspace).unwrap();
    let foreign_store =
        SessionStore::for_workspace(persist_dir.to_str().unwrap(), &foreign_workspace).unwrap();

    let mut local = new_session(&local_store, "local prompt");
    local_store.persist(&mut local).unwrap();
    let mut foreign = new_session(&foreign_store, "foreign prompt");
    foreign_store.persist(&mut foreign).unwrap();

    // Default list remains workspace-scoped.
    let (local_only, _) = local_store.list(10, None, false).unwrap();
    assert_eq!(local_only.len(), 1);
    assert_eq!(local_only[0].session_id, local.session_id);
    assert_eq!(local_only[0].health, SessionHealth::Ready);

    // --all aggregates across workspace-hash directories.
    let (all, cursor) = local_store.list(10, None, true).unwrap();
    assert_eq!(all.len(), 2);
    assert!(cursor.is_none());

    let local_summary = all
        .iter()
        .find(|s| s.session_id == local.session_id)
        .unwrap();
    assert_eq!(local_summary.workspace_scope, local_store.workspace_scope());
    assert_eq!(local_summary.health, SessionHealth::Ready);

    let foreign_summary = all
        .iter()
        .find(|s| s.session_id == foreign.session_id)
        .unwrap();
    assert_eq!(
        foreign_summary.workspace_scope,
        foreign_store.workspace_scope()
    );
    assert_eq!(foreign_summary.health, SessionHealth::ScopeMismatch);
    // Foreign sessions should still show their real metadata so the picker
    // row is consistent with local sessions.
    assert_eq!(foreign_summary.model.as_deref(), Some("mock-model"));
    assert_eq!(foreign_summary.message_count, 2);
    assert_eq!(
        foreign_summary.first_prompt.as_deref(),
        Some("foreign prompt")
    );
}

#[test]
fn list_all_workspaces_returns_empty_page_when_root_is_missing() {
    let temp = tempfile::tempdir().unwrap();
    let persist_dir = temp.path().join("sessions");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    // The persist root has never been created because no session was written.
    assert!(!persist_dir.exists());

    let store = SessionStore::for_workspace(persist_dir.to_str().unwrap(), &workspace).unwrap();
    let (summaries, cursor) = store.list(10, None, true).unwrap();

    assert!(summaries.is_empty());
    assert!(cursor.is_none());
}

#[test]
fn list_all_workspaces_pages_without_reading_off_page_files() {
    let temp = tempfile::tempdir().unwrap();
    let persist_dir = temp.path().join("sessions");
    let local_workspace = temp.path().join("local");
    let foreign_workspace = temp.path().join("foreign");
    fs::create_dir_all(&local_workspace).unwrap();
    fs::create_dir_all(&foreign_workspace).unwrap();

    let local_store =
        SessionStore::for_workspace(persist_dir.to_str().unwrap(), &local_workspace).unwrap();
    let foreign_store =
        SessionStore::for_workspace(persist_dir.to_str().unwrap(), &foreign_workspace).unwrap();

    let mut local = new_session(&local_store, "local prompt");
    local_store.persist(&mut local).unwrap();
    let mut foreign = new_session(&foreign_store, "foreign prompt");
    foreign_store.persist(&mut foreign).unwrap();

    reset_session_file_read_count();
    let (first, cursor) = local_store.list(1, None, true).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(session_file_read_count(), 1);

    let (second, final_cursor) = local_store.list(1, cursor.as_deref(), true).unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(session_file_read_count(), 2);
    assert!(final_cursor.is_none());
}

#[cfg(unix)]
#[test]
fn list_all_workspaces_skips_unreadable_workspace_directory() {
    let temp = tempfile::tempdir().unwrap();
    let persist_dir = temp.path().join("sessions");
    let local_workspace = temp.path().join("local");
    let foreign_workspace = temp.path().join("foreign");
    fs::create_dir_all(&local_workspace).unwrap();
    fs::create_dir_all(&foreign_workspace).unwrap();

    let local_store =
        SessionStore::for_workspace(persist_dir.to_str().unwrap(), &local_workspace).unwrap();
    let foreign_store =
        SessionStore::for_workspace(persist_dir.to_str().unwrap(), &foreign_workspace).unwrap();

    let mut local = new_session(&local_store, "local prompt");
    local_store.persist(&mut local).unwrap();
    let mut foreign = new_session(&foreign_store, "foreign prompt");
    foreign_store.persist(&mut foreign).unwrap();

    // Remove read permissions from the foreign workspace scoped directory.
    // The --all listing must skip it rather than fail entirely.
    let foreign_scoped_dir = foreign_store.base_dir.clone();
    fs::set_permissions(&foreign_scoped_dir, fs::Permissions::from_mode(0o000)).unwrap();

    let result = local_store.list(10, None, true);

    // Restore permissions so tempfile can clean up.
    fs::set_permissions(&foreign_scoped_dir, fs::Permissions::from_mode(0o700)).unwrap();

    let (all, cursor) = result.expect("list --all should not fail because of one bad scope");
    assert_eq!(all.len(), 1);
    assert!(cursor.is_none());
    assert_eq!(all[0].session_id, local.session_id);
}

#[test]
fn list_all_workspaces_handles_many_sessions_without_fd_exhaustion() {
    let temp = tempfile::tempdir().unwrap();
    let persist_dir = temp.path().join("sessions");
    let workspace_count = 5;
    let sessions_per_workspace = 60;
    let stores: Vec<SessionStore> = (0..workspace_count)
        .map(|i| {
            let workspace = temp.path().join(format!("workspace{i}"));
            fs::create_dir_all(&workspace).unwrap();
            SessionStore::for_workspace(persist_dir.to_str().unwrap(), &workspace).unwrap()
        })
        .collect();

    for (i, store) in stores.iter().enumerate() {
        for j in 0..sessions_per_workspace {
            let mut session = new_session(store, &format!("prompt {i}-{j}"));
            session.updated_at_ms = ((i * sessions_per_workspace + j) as u64) * 1000;
            store.persist(&mut session).unwrap();
        }
    }

    // With the old implementation this would clone the workspace directory
    // handle once per session (300 fd) and could hit EMFILE on systems with
    // a low soft fd limit. The new implementation keeps fd usage O(1).
    let (page, cursor) = stores[0].list(10, None, true).unwrap();
    assert_eq!(page.len(), 10);
    assert!(cursor.is_some());

    // Pagination should still work across the full set (clamped to 100/page).
    let (second, cursor) = stores[0].list(100, cursor.as_deref(), true).unwrap();
    assert_eq!(second.len(), 100);
    assert!(cursor.is_some());

    let (third, cursor) = stores[0].list(100, cursor.as_deref(), true).unwrap();
    assert_eq!(third.len(), 100);
    assert!(cursor.is_some());

    let (fourth, final_cursor) = stores[0].list(100, cursor.as_deref(), true).unwrap();
    assert_eq!(fourth.len(), 90);
    assert!(final_cursor.is_none());
}

#[test]
fn ps_unreadable_uuid_entry_without_hiding_healthy_sessions() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut healthy = new_session(&store, "healthy prompt");
    store.persist(&mut healthy).unwrap();
    let directory_id = ProviderSessionId::new();
    fs::create_dir(store.session_file(&directory_id)).unwrap();

    let (summaries, cursor) = store.list(10, None, false).unwrap();

    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].session_id, healthy.session_id);
    assert!(cursor.is_none());
}

#[test]
fn list_fills_page_after_filtered_entries() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let skipped = ProviderSessionId::parse("00000000-0000-4000-8000-000000000000").unwrap();
    let first_ready = ProviderSessionId::parse("11111111-1111-4111-8111-111111111111").unwrap();
    let second_ready = ProviderSessionId::parse("22222222-2222-4222-8222-222222222222").unwrap();
    let entries = [&skipped, &first_ready, &second_ready]
        .into_iter()
        .enumerate()
        .map(|(index, session_id)| ListEntry {
            session_id: session_id.clone(),
            modified_at_ms: 3_u64.saturating_sub(index as u64),
        })
        .collect::<Vec<_>>();

    let (first_page, examined_end) = collect_list_page(&entries, 0, 1, |entry| {
        (entry.session_id != skipped)
            .then(|| store.corrupt_summary(&entry.session_id, entry.modified_at_ms))
    });
    assert_eq!(first_page.len(), 1);
    assert_eq!(first_page[0].session_id, first_ready);
    assert_eq!(examined_end, 2);

    let (second_page, final_end) = collect_list_page(&entries, examined_end, 1, |entry| {
        Some(store.corrupt_summary(&entry.session_id, entry.modified_at_ms))
    });
    assert_eq!(second_page.len(), 1);
    assert_eq!(second_page[0].session_id, second_ready);
    assert_eq!(final_end, entries.len());
}

#[test]
fn summary_preview_is_single_line_unicode_safe_and_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let prompt = format!(
        "first\n\t{}  last",
        "界".repeat(MAX_PROMPT_PREVIEW_CHARS + 50)
    );
    let mut session = new_session(&store, &prompt);
    store.persist(&mut session).unwrap();

    let summary = store.validate(&session.session_id).unwrap();
    let preview = summary.first_prompt.expect("bounded preview");

    assert!(!preview.contains('\n'));
    assert!(!preview.contains('\t'));
    assert_eq!(preview.chars().count(), MAX_PROMPT_PREVIEW_CHARS);
    assert!(preview.ends_with('…'));
}

#[test]
fn listing_distinguishes_persisted_shell_envelope_sessions() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let envelope = |input: &str| {
        format!(
            "Handle this natural-language shell prompt request for a Shell-first assistant.\n\
             Decide based on user intent:\n\
             Do not mention Claude Code, plan mode, implementation status, or internal workflow.\n\n\
             user_input: {input}\n\n\nruntime_frame:
cwd: /root\n\n\
             cosh-shell Agent contract:\n- User modes: recommend and agent."
        )
    };
    let mut older = new_session(&store, &envelope("查看当前目录下的文件"));
    store.persist(&mut older).unwrap();
    let mut newer = new_session(&store, &envelope("帮我检查 nginx 服务状态"));
    newer.updated_at_ms = older.updated_at_ms.saturating_add(10);
    store.persist(&mut newer).unwrap();

    let (summaries, _) = store.list(10, None, false).unwrap();

    assert_eq!(
        summaries[0].first_prompt.as_deref(),
        Some("帮我检查 nginx 服务状态")
    );
    assert_eq!(
        summaries[1].first_prompt.as_deref(),
        Some("查看当前目录下的文件")
    );
}

#[test]
fn inspect_reports_invalid_utf8_as_corrupt_summary() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let corrupt = write_invalid_utf8_session(&store);

    let summary = store.inspect(&corrupt).unwrap();

    assert_eq!(summary.session_id, corrupt);
    assert_eq!(summary.workspace_scope, store.workspace_scope());
    assert_eq!(summary.health, SessionHealth::Corrupt);
    assert_eq!(summary.schema_version, None);
}

#[test]
fn load_and_validate_classify_invalid_utf8_as_corrupt() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let corrupt = write_invalid_utf8_session(&store);

    assert!(matches!(
        store.load(&corrupt),
        Err(SessionError::Corrupt { ref message, .. })
            if message.contains("invalid UTF-8")
    ));
    assert!(matches!(
        store.validate(&corrupt),
        Err(SessionError::Corrupt { ref message, .. })
            if message.contains("invalid UTF-8")
    ));
}

#[test]
fn legacy_array_loads_and_upgrades_on_write() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let legacy_dir = workspace.join("sessions");
    fs::create_dir_all(&legacy_dir).unwrap();
    let store = SessionStore::for_workspace("sessions", &workspace).unwrap();
    let legacy_dir = store.legacy_dirs[0].path.clone();
    let id = ProviderSessionId::new();
    let legacy_file = legacy_dir.join(format!("{id}.json"));
    fs::write(
        &legacy_file,
        serde_json::to_vec(&vec![Message::user("legacy")]).unwrap(),
    )
    .unwrap();
    assert!(!store.session_file(&id).exists());

    let mut loaded = store.load(&id).unwrap();
    assert_eq!(loaded.generation, 0);
    assert_eq!(loaded.messages.len(), 1);
    assert!(!store.session_file(&id).exists());
    assert!(legacy_file.exists());

    loaded.messages.push(Message::assistant("upgraded"));
    store.persist(&mut loaded).unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(store.session_file(&id)).unwrap()).unwrap();
    assert_eq!(value["schema_version"], CURRENT_SCHEMA_VERSION);
    assert_eq!(value["generation"], 1);
    assert!(!legacy_file.exists());
}

// Linux-only: on macOS unlinking from a mode-0o500 directory still succeeds,
// so the simulated legacy-removal failure never triggers.
#[cfg(target_os = "linux")]
#[test]
fn legacy_cleanup_failure_is_reported_and_retried_after_migration() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let legacy_dir = workspace.join("sessions");
    fs::create_dir_all(&legacy_dir).unwrap();
    let store = SessionStore::for_workspace("sessions", &workspace).unwrap();
    fs::create_dir_all(&store.base_dir).unwrap();
    let id = ProviderSessionId::new();
    let legacy_file = legacy_dir.join(format!("{id}.json"));
    fs::write(
        &legacy_file,
        serde_json::to_vec(&vec![Message::user("legacy")]).unwrap(),
    )
    .unwrap();
    let mut loaded = store.load(&id).unwrap();
    loaded.messages.push(Message::assistant("migrated"));
    fs::set_permissions(&legacy_dir, fs::Permissions::from_mode(0o500)).unwrap();

    let result = store.persist(&mut loaded);

    fs::set_permissions(&legacy_dir, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(
        result,
        Err(SessionError::Io {
            operation: "remove",
            ref path,
            ..
        }) if path == &legacy_file
    ));
    assert_eq!(loaded.generation, 1);
    assert!(store.session_file(&id).exists());
    assert!(legacy_file.exists());

    store.persist(&mut loaded).unwrap();
    assert_eq!(loaded.generation, 2);
    assert!(!legacy_file.exists());
    assert_eq!(store.load(&id).unwrap().messages.len(), 2);
}

// Linux-only: same macOS permission semantics as the cleanup-failure test above.
#[cfg(target_os = "linux")]
#[test]
fn clear_keeps_scoped_history_when_legacy_removal_fails() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let legacy_dir = workspace.join("sessions");
    fs::create_dir_all(&legacy_dir).unwrap();
    let store = SessionStore::for_workspace("sessions", &workspace).unwrap();
    let mut current = new_session(&store, "latest scoped history");
    store.persist(&mut current).unwrap();
    let scoped_file = store.session_file(&current.session_id);
    let legacy_file = legacy_dir.join(format!("{}.json", current.session_id));
    fs::write(
        &legacy_file,
        serde_json::to_vec(&vec![Message::user("stale legacy history")]).unwrap(),
    )
    .unwrap();
    fs::set_permissions(&legacy_dir, fs::Permissions::from_mode(0o500)).unwrap();

    let result = store.clear(&current.session_id, &[]);

    fs::set_permissions(&legacy_dir, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(
        result,
        Err(SessionError::Io {
            operation: "remove",
            ref path,
            ..
        }) if path == &legacy_file
    ));
    assert!(scoped_file.exists());
    assert!(legacy_file.exists());
    let loaded = store.load(&current.session_id).unwrap();
    assert_eq!(
        loaded.messages[0].content.as_text(),
        "latest scoped history"
    );
}

#[test]
fn failed_scoped_validation_keeps_legacy_copy() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let legacy_dir = workspace.join("sessions");
    fs::create_dir_all(&legacy_dir).unwrap();
    let store = SessionStore::for_workspace("sessions", &workspace).unwrap();
    fs::create_dir_all(&store.base_dir).unwrap();
    let session = new_session(&store, "replacement");
    let legacy_file = legacy_dir.join(format!("{}.json", session.session_id));
    fs::write(
        &legacy_file,
        serde_json::to_vec(&vec![Message::user("recoverable legacy")]).unwrap(),
    )
    .unwrap();
    let scoped_file = store.session_file(&session.session_id);
    fs::write(&scoped_file, b"{broken scoped").unwrap();
    let mut candidate = session;

    assert!(matches!(
        store.persist(&mut candidate),
        Err(SessionError::Corrupt { .. })
    ));
    assert!(legacy_file.exists());
    assert!(scoped_file.exists());
}

#[test]
fn legacy_inspect_and_validate_do_not_migrate_before_persist() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let legacy_dir = workspace.join("sessions");
    fs::create_dir_all(&legacy_dir).unwrap();
    let store = SessionStore::for_workspace(DEFAULT_SESSION_PERSIST_DIR, &workspace).unwrap();
    let id = ProviderSessionId::new();
    let legacy_file = legacy_dir.join(format!("{id}.json"));
    fs::write(
        &legacy_file,
        serde_json::to_vec(&vec![Message::user("read only")]).unwrap(),
    )
    .unwrap();

    assert_eq!(store.inspect(&id).unwrap().health, SessionHealth::Ready);
    assert_eq!(store.validate(&id).unwrap().health, SessionHealth::Ready);
    assert!(legacy_file.exists());
    assert!(!store.session_file(&id).exists());
}

#[test]
fn regular_file_is_not_accepted_as_a_legacy_directory() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(workspace.join("sessions"), b"not a directory").unwrap();

    let store = SessionStore::for_workspace(DEFAULT_SESSION_PERSIST_DIR, &workspace).unwrap();

    assert!(store.legacy_dirs.is_empty());
    assert!(store.session_ids().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn pinned_legacy_directory_fd_prevents_symlink_swap_during_load_and_clear() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let legacy_dir = workspace.join("sessions");
    let pinned_dir = workspace.join("sessions-old");
    let external_dir = temp.path().join("external");
    fs::create_dir_all(&legacy_dir).unwrap();
    fs::create_dir_all(&external_dir).unwrap();
    let store = SessionStore::for_workspace(DEFAULT_SESSION_PERSIST_DIR, &workspace).unwrap();
    let id = ProviderSessionId::new();
    fs::write(
        legacy_dir.join(format!("{id}.json")),
        serde_json::to_vec(&vec![Message::user("workspace-owned")]).unwrap(),
    )
    .unwrap();
    let external_file = external_dir.join(format!("{id}.json"));
    fs::write(
        &external_file,
        serde_json::to_vec(&vec![Message::user("must remain external")]).unwrap(),
    )
    .unwrap();

    fs::rename(&legacy_dir, &pinned_dir).unwrap();
    std::os::unix::fs::symlink(&external_dir, &legacy_dir).unwrap();

    let loaded = store.load(&id).unwrap();
    assert_eq!(loaded.messages[0].content.as_text(), "workspace-owned");
    assert_eq!(store.session_ids().unwrap(), vec![id.clone()]);
    store.clear(&id, &[]).unwrap();

    assert!(!pinned_dir.join(format!("{id}.json")).exists());
    assert!(external_file.exists());
}

#[cfg(unix)]
#[test]
fn legacy_session_symlink_is_never_followed_by_load_or_clear() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let legacy_dir = workspace.join("sessions");
    let external_dir = temp.path().join("external");
    fs::create_dir_all(&legacy_dir).unwrap();
    fs::create_dir_all(&external_dir).unwrap();
    let store = SessionStore::for_workspace(DEFAULT_SESSION_PERSIST_DIR, &workspace).unwrap();
    let id = ProviderSessionId::new();
    let external_file = external_dir.join(format!("{id}.json"));
    fs::write(
        &external_file,
        serde_json::to_vec(&vec![Message::user("external")]).unwrap(),
    )
    .unwrap();
    std::os::unix::fs::symlink(&external_file, legacy_dir.join(format!("{id}.json"))).unwrap();

    assert!(matches!(
        store.load(&id),
        Err(SessionError::Corrupt { ref message, .. })
            if message.contains("symbolic link")
    ));
    assert!(store.session_ids().unwrap().is_empty());
    assert!(matches!(
        store.clear(&id, &[]),
        Err(SessionError::Corrupt { ref message, .. })
            if message.contains("symbolic link")
    ));
    assert!(external_file.exists());
}

#[test]
fn shared_flat_legacy_root_is_not_claimed_by_an_ancestor_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let shared = temp.path().join("shared");
    fs::create_dir_all(&shared).unwrap();
    let store = SessionStore::for_workspace(shared.to_str().unwrap(), temp.path()).unwrap();
    let id = ProviderSessionId::new();
    let legacy_file = shared.join(format!("{id}.json"));
    fs::write(
        &legacy_file,
        serde_json::to_vec(&vec![Message::user("ambiguous")]).unwrap(),
    )
    .unwrap();

    assert!(matches!(
        store.load(&id),
        Err(SessionError::NotFound { .. })
    ));
    assert!(legacy_file.exists());
    assert!(!store.session_file(&id).exists());
    assert!(store.session_ids().unwrap().is_empty());
    assert!(matches!(
        store.clear(&id, &[]),
        Err(SessionError::NotFound { .. })
    ));
    assert!(legacy_file.exists());
}

#[test]
fn parent_relative_legacy_root_is_not_shared_by_sibling_workspaces() {
    let temp = tempfile::tempdir().unwrap();
    let shared = temp.path().join("shared");
    let first_workspace = temp.path().join("first");
    let second_workspace = temp.path().join("second");
    fs::create_dir_all(&shared).unwrap();
    fs::create_dir_all(first_workspace.join("nested")).unwrap();
    fs::create_dir_all(second_workspace.join("nested")).unwrap();
    let id = ProviderSessionId::new();
    let legacy_file = shared.join(format!("{id}.json"));
    fs::write(
        &legacy_file,
        serde_json::to_vec(&vec![Message::user("shared legacy")]).unwrap(),
    )
    .unwrap();

    for persist_dir in ["../shared", "nested/../../shared"] {
        for workspace in [&first_workspace, &second_workspace] {
            let store = SessionStore::for_workspace(persist_dir, workspace).unwrap();
            assert!(matches!(
                store.load(&id),
                Err(SessionError::NotFound { .. })
            ));
            assert!(store.session_ids().unwrap().is_empty());
            assert!(matches!(
                store.clear(&id, &[]),
                Err(SessionError::NotFound { .. })
            ));
        }
    }
    assert!(legacy_file.exists());
}

#[cfg(unix)]
#[test]
fn relative_symlink_cannot_escape_workspace_for_legacy_lookup() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let shared = temp.path().join("shared");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&shared).unwrap();
    std::os::unix::fs::symlink(&shared, workspace.join("sessions-link")).unwrap();
    let id = ProviderSessionId::new();
    let legacy_file = shared.join(format!("{id}.json"));
    fs::write(
        &legacy_file,
        serde_json::to_vec(&vec![Message::user("escaped legacy")]).unwrap(),
    )
    .unwrap();

    // The symlinked root is resolved once at construction time; the
    // escaped directory is never claimed as workspace-owned legacy
    // storage, so foreign session files stay invisible and untouched.
    let store = SessionStore::for_workspace("sessions-link", &workspace).unwrap();
    assert!(matches!(
        store.load(&id),
        Err(SessionError::NotFound { .. })
    ));
    assert!(store.session_ids().unwrap().is_empty());
    assert!(store.list(10, None, false).unwrap().0.is_empty());
    assert!(matches!(
        store.clear(&id, &[]),
        Err(SessionError::NotFound { .. })
    ));
    assert!(legacy_file.exists());
}

#[cfg(unix)]
#[test]
fn scoped_directory_symlink_cannot_cross_workspace_on_clear() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("scoped");
    let first_workspace = temp.path().join("first");
    let second_workspace = temp.path().join("second");
    fs::create_dir_all(&first_workspace).unwrap();
    fs::create_dir_all(&second_workspace).unwrap();
    let first = SessionStore::for_workspace(root.to_str().unwrap(), &first_workspace).unwrap();
    let mut session = new_session(&first, "first workspace");
    first.persist(&mut session).unwrap();
    let first_base_dir = first.base_dir.clone();
    let second = SessionStore::for_workspace(root.to_str().unwrap(), &second_workspace).unwrap();
    std::os::unix::fs::symlink(&first_base_dir, &second.base_dir).unwrap();

    assert!(matches!(
        second.load(&session.session_id),
        Err(SessionError::Io { .. })
    ));
    assert!(matches!(
        second.list(10, None, false),
        Err(SessionError::Io { .. })
    ));
    assert!(matches!(second.session_ids(), Err(SessionError::Io { .. })));
    let mut second_session = new_session(&second, "second workspace");
    assert!(matches!(
        second.persist(&mut second_session),
        Err(SessionError::Io { .. })
    ));
    assert!(matches!(
        second.clear(&session.session_id, &[]),
        Err(SessionError::Io { .. })
    ));
    assert!(first.load(&session.session_id).is_ok());
    assert!(matches!(
        SessionStore::for_workspace(root.to_str().unwrap(), &second_workspace),
        Err(SessionError::Io { .. })
    ));
}

#[cfg(unix)]
#[test]
fn scoped_session_symlink_is_never_followed_by_load_or_clear() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("scoped");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let store = SessionStore::for_workspace(root.to_str().unwrap(), &workspace).unwrap();
    let directory = store.scoped.directory(true).unwrap().unwrap();
    let id = ProviderSessionId::new();
    let external = temp.path().join("external.json");
    fs::write(&external, b"external").unwrap();
    std::os::unix::fs::symlink(&external, store.session_file(&id)).unwrap();

    assert!(matches!(
        store.load(&id),
        Err(SessionError::Corrupt { ref message, .. })
            if message.contains("symbolic link")
    ));
    assert!(matches!(
        store.clear(&id, &[]),
        Err(SessionError::Corrupt { ref message, .. })
            if message.contains("symbolic link")
    ));
    assert_eq!(fs::read(&external).unwrap(), b"external");
    drop(directory);
}

#[test]
fn default_legacy_source_is_only_the_workspace_sessions_directory() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(workspace.join("sessions")).unwrap();

    let store = SessionStore::for_workspace(DEFAULT_SESSION_PERSIST_DIR, &workspace).unwrap();

    assert_eq!(store.legacy_dirs.len(), 1);
    assert_eq!(
        store.legacy_dirs[0].path,
        fs::canonicalize(workspace.join("sessions")).unwrap()
    );
}

#[test]
fn list_includes_workspace_owned_legacy_sessions() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let legacy_dir = workspace.join("sessions");
    fs::create_dir_all(&legacy_dir).unwrap();
    let store = SessionStore::for_workspace(DEFAULT_SESSION_PERSIST_DIR, &workspace).unwrap();
    let mut scoped = new_session(&store, "scoped prompt");
    store.persist(&mut scoped).unwrap();
    let legacy_id = ProviderSessionId::new();
    fs::write(
        legacy_dir.join(format!("{legacy_id}.json")),
        serde_json::to_vec(&vec![Message::user("legacy prompt")]).unwrap(),
    )
    .unwrap();
    let corrupt_id = ProviderSessionId::new();
    fs::write(legacy_dir.join(format!("{corrupt_id}.json")), b"{broken").unwrap();

    let (summaries, cursor) = store.list(10, None, false).unwrap();

    assert_eq!(summaries.len(), 3);
    assert!(cursor.is_none());
    assert!(summaries.iter().any(|summary| {
        summary.session_id == scoped.session_id && summary.health == SessionHealth::Ready
    }));
    assert!(summaries.iter().any(|summary| {
        summary.session_id == legacy_id
            && summary.health == SessionHealth::Ready
            && summary.first_prompt.as_deref() == Some("legacy prompt")
    }));
    assert!(summaries.iter().any(|summary| {
        summary.session_id == corrupt_id && summary.health == SessionHealth::Corrupt
    }));
}

#[test]
fn scoped_entry_shadows_legacy_copy_in_list() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let legacy_dir = workspace.join("sessions");
    fs::create_dir_all(&legacy_dir).unwrap();
    let store = SessionStore::for_workspace(DEFAULT_SESSION_PERSIST_DIR, &workspace).unwrap();
    let mut session = new_session(&store, "current scoped prompt");
    store.persist(&mut session).unwrap();
    fs::write(
        legacy_dir.join(format!("{}.json", session.session_id)),
        serde_json::to_vec(&vec![Message::user("stale legacy prompt")]).unwrap(),
    )
    .unwrap();

    let (summaries, _) = store.list(10, None, false).unwrap();

    assert_eq!(summaries.len(), 1);
    assert_eq!(
        summaries[0].first_prompt.as_deref(),
        Some("current scoped prompt")
    );
}

#[cfg(unix)]
#[test]
fn symlinked_storage_root_resolves_before_descriptor_pinning() {
    let temp = tempfile::tempdir().unwrap();
    let real_root = temp.path().join("real-root");
    fs::create_dir_all(&real_root).unwrap();
    let linked_root = temp.path().join("linked-root");
    std::os::unix::fs::symlink(&real_root, &linked_root).unwrap();

    let store = SessionStore::for_workspace(linked_root.to_str().unwrap(), temp.path()).unwrap();
    let mut session = new_session(&store, "through symlinked root");
    store.persist(&mut session).unwrap();

    assert_eq!(store.load(&session.session_id).unwrap().generation, 1);
    assert!(store
        .session_file(&session.session_id)
        .starts_with(fs::canonicalize(&real_root).unwrap()));
}

#[test]
fn clear_removes_paired_lock_file() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut session = new_session(&store, "lock cleanup");
    store.persist(&mut session).unwrap();
    let lock_path = store
        .base_dir
        .join(format!(".{}.lock", session.session_id.as_str()));
    assert!(lock_path.exists());

    store.clear(&session.session_id, &[]).unwrap();

    assert!(!store.session_file(&session.session_id).exists());
    assert!(!lock_path.exists());
}

#[test]
fn persist_recovers_after_external_directory_removal() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut first = new_session(&store, "before removal");
    store.persist(&mut first).unwrap();

    fs::remove_dir_all(&store.base_dir).unwrap();

    let mut second = new_session(&store, "after removal");
    store.persist(&mut second).unwrap();
    assert_eq!(store.load(&second.session_id).unwrap().generation, 1);
}

#[test]
fn list_shows_legacy_sessions_without_any_scoped_directory() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let legacy_dir = workspace.join("sessions");
    fs::create_dir_all(&legacy_dir).unwrap();
    let store = SessionStore::for_workspace(DEFAULT_SESSION_PERSIST_DIR, &workspace).unwrap();
    let legacy_id = ProviderSessionId::new();
    fs::write(
        legacy_dir.join(format!("{legacy_id}.json")),
        serde_json::to_vec(&vec![Message::user("only legacy")]).unwrap(),
    )
    .unwrap();

    let (summaries, cursor) = store.list(10, None, false).unwrap();

    assert_eq!(summaries.len(), 1);
    assert!(cursor.is_none());
    assert_eq!(summaries[0].session_id, legacy_id);
    assert_eq!(summaries[0].health, SessionHealth::Ready);
}

#[cfg(unix)]
#[test]
fn stale_temporary_files_are_swept_before_first_write() {
    let temp = tempfile::tempdir().unwrap();
    let first = store(&temp);
    fs::create_dir_all(&first.base_dir).unwrap();
    let stale = first
        .base_dir
        .join(".00000000-0000-4000-8000-000000000000.stale.tmp");
    let fresh = first
        .base_dir
        .join(".11111111-1111-4111-8111-111111111111.fresh.tmp");
    fs::write(&stale, b"stale").unwrap();
    fs::write(&fresh, b"fresh").unwrap();
    // Backdate only the stale file beyond the sweep threshold.
    assert!(std::process::Command::new("touch")
        .args(["-t", "202001010000"])
        .arg(&stale)
        .status()
        .unwrap()
        .success());

    // A second store pins the already existing directory at construction,
    // so the sweep must still run on its first write-mode open.
    let second = store(&temp);
    let mut session = new_session(&second, "sweep trigger");
    second.persist(&mut session).unwrap();

    assert!(!stale.exists());
    assert!(fresh.exists());
}

#[test]
fn clear_lists_and_removes_workspace_owned_legacy_sessions() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let legacy_dir = workspace.join("sessions");
    fs::create_dir_all(&legacy_dir).unwrap();
    let store = SessionStore::for_workspace(DEFAULT_SESSION_PERSIST_DIR, &workspace).unwrap();
    let valid = ProviderSessionId::new();
    let corrupt = ProviderSessionId::new();
    let valid_path = legacy_dir.join(format!("{valid}.json"));
    let corrupt_path = legacy_dir.join(format!("{corrupt}.json"));
    fs::write(
        &valid_path,
        serde_json::to_vec(&vec![Message::user("legacy")]).unwrap(),
    )
    .unwrap();
    fs::write(&corrupt_path, b"{broken legacy").unwrap();

    let ids = store.session_ids().unwrap();
    assert!(ids.contains(&valid));
    assert!(ids.contains(&corrupt));
    assert!(matches!(
        store.clear(&valid, std::slice::from_ref(&valid)),
        Err(SessionError::ActiveSession { .. })
    ));
    assert!(valid_path.exists());

    store.clear(&corrupt, &[]).unwrap();
    store.clear(&valid, &[]).unwrap();
    assert!(!corrupt_path.exists());
    assert!(!valid_path.exists());
    assert!(store.session_ids().unwrap().is_empty());
}

#[test]
fn list_bounds_each_page_and_marks_oversized_files_corrupt() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    fs::create_dir_all(&store.base_dir).unwrap();
    let oversized = ProviderSessionId::parse("ffffffff-ffff-4fff-bfff-ffffffffffff").unwrap();
    File::create(store.session_file(&oversized))
        .unwrap()
        .set_len(MAX_SESSION_FILE_BYTES + 1)
        .unwrap();
    let healthy_id = ProviderSessionId::parse("00000000-0000-4000-8000-000000000000").unwrap();
    let mut healthy = PersistedSession::new(
        healthy_id.clone(),
        store.workspace_scope().to_string(),
        "mock".to_string(),
        vec![Message::user("bounded page")],
    );
    store.persist(&mut healthy).unwrap();

    reset_session_file_read_count();
    let (first, cursor) = store.list(1, None, false).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].session_id, healthy_id);
    assert_eq!(session_file_read_count(), 1);
    let (second, final_cursor) = store.list(1, cursor.as_deref(), false).unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].session_id, oversized);
    assert_eq!(second[0].health, SessionHealth::Corrupt);
    assert_eq!(session_file_read_count(), 2);
    assert!(final_cursor.is_none());
}

#[test]
fn rejects_invalid_and_traversal_ids_before_path_use() {
    for value in [
        "../outside",
        "not-a-uuid",
        "A0Eebc99-9c0b-4ef8-bb6d-6bb9bd380a11",
    ] {
        assert!(matches!(
            ProviderSessionId::parse(value),
            Err(SessionError::InvalidId { .. })
        ));
    }
}

#[test]
fn distinguishes_missing_corrupt_incompatible_and_scope_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    fs::create_dir_all(&store.base_dir).unwrap();

    let missing = ProviderSessionId::new();
    assert!(matches!(
        store.load(&missing),
        Err(SessionError::NotFound { .. })
    ));

    let corrupt = ProviderSessionId::new();
    fs::write(store.session_file(&corrupt), b"{broken").unwrap();
    assert!(matches!(
        store.load(&corrupt),
        Err(SessionError::Corrupt { .. })
    ));

    let incompatible = ProviderSessionId::new();
    fs::write(
        store.session_file(&incompatible),
        format!(
            r#"{{"schema_version":99,"session_id":"{incompatible}","workspace_scope":"{}"}}"#,
            store.workspace_scope()
        ),
    )
    .unwrap();
    assert!(matches!(
        store.load(&incompatible),
        Err(SessionError::IncompatibleVersion { version: 99, .. })
    ));

    let mut mismatch = new_session(&store, "mismatch");
    mismatch.workspace_scope = "/other/workspace".to_string();
    fs::write(
        store.session_file(&mismatch.session_id),
        serde_json::to_vec(&mismatch).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        store.load(&mismatch.session_id),
        Err(SessionError::ScopeMismatch { .. })
    ));
}

#[test]
fn conflict_preserves_prior_good_file() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut first = new_session(&store, "original");
    store.persist(&mut first).unwrap();
    let mut stale = first.clone();
    first.messages.push(Message::assistant("new"));
    store.persist(&mut first).unwrap();

    stale.messages.push(Message::assistant("stale"));
    assert!(matches!(
        store.persist(&mut stale),
        Err(SessionError::Conflict { .. })
    ));
    let loaded = store.load(&first.session_id).unwrap();
    assert_eq!(loaded.generation, first.generation);
    assert_eq!(loaded.messages.len(), first.messages.len());
}

#[test]
fn exhausted_generation_is_rejected_without_overwriting_history() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut session = new_session(&store, "generation limit");
    store.persist(&mut session).unwrap();
    let path = store.session_file(&session.session_id);
    session.generation = u64::MAX;
    fs::write(&path, serde_json::to_vec_pretty(&session).unwrap()).unwrap();
    let before = fs::read(&path).unwrap();
    let mut loaded = store.load(&session.session_id).unwrap();
    loaded
        .messages
        .push(Message::assistant("must not overwrite"));

    assert!(matches!(
        store.persist(&mut loaded),
        Err(SessionError::Corrupt { ref message, .. })
            if message.contains("generation is exhausted")
    ));
    assert_eq!(loaded.generation, u64::MAX);
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn atomic_write_failure_preserves_prior_file() {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("session.json");
    let temp_path = temp.path().join("occupied.tmp");
    fs::write(&destination, b"prior-good-envelope").unwrap();
    fs::write(&temp_path, b"occupied").unwrap();

    assert!(write_atomic_file(&temp_path, &destination, b"replacement").is_err());
    assert_eq!(fs::read(&destination).unwrap(), b"prior-good-envelope");
}

#[test]
fn active_writer_lock_reports_conflict_without_mutating_session() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut session = new_session(&store, "locked");
    store.persist(&mut session).unwrap();
    let before = fs::read(store.session_file(&session.session_id)).unwrap();
    let lock_path = store
        .base_dir
        .join(format!(".{}.lock", session.session_id.as_str()));
    let lock = private_open_options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).unwrap();

    session.messages.push(Message::assistant("must not commit"));
    assert!(matches!(
        store.persist(&mut session),
        Err(SessionError::Conflict { .. })
    ));
    assert_eq!(
        fs::read(store.session_file(&session.session_id)).unwrap(),
        before
    );
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::Unlock).unwrap();
    drop(lock);
    store.persist(&mut session).unwrap();
}

#[test]
fn advisory_lock_child_process() {
    let Some(path) = std::env::var_os("COSH_SESSION_LOCK_CHILD") else {
        return;
    };
    let lock = private_open_options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).unwrap();
    std::process::exit(86);
}

#[test]
fn process_exit_releases_advisory_lock() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut session = new_session(&store, "crash-safe lock");
    store.persist(&mut session).unwrap();
    let lock_path = store
        .base_dir
        .join(format!(".{}.lock", session.session_id.as_str()));
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "session::store::tests::advisory_lock_child_process",
            "--nocapture",
        ])
        .env("COSH_SESSION_LOCK_CHILD", &lock_path)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(86));

    session
        .messages
        .push(Message::assistant("commit after crash"));
    store.persist(&mut session).unwrap();
    assert_eq!(store.load(&session.session_id).unwrap().generation, 2);
}

#[cfg(unix)]
#[test]
fn persistence_enforces_private_directory_and_file_modes() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    fs::create_dir_all(&store.base_dir).unwrap();
    fs::set_permissions(&store.base_dir, fs::Permissions::from_mode(0o755)).unwrap();
    let mut session = new_session(&store, "private history");
    let lock_path = store
        .base_dir
        .join(format!(".{}.lock", session.session_id.as_str()));
    fs::write(&lock_path, []).unwrap();
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o644)).unwrap();

    store.persist(&mut session).unwrap();

    let directory_mode = fs::metadata(&store.base_dir).unwrap().permissions().mode() & 0o777;
    let session_mode = fs::metadata(store.session_file(&session.session_id))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    let lock_mode = fs::metadata(lock_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(directory_mode, 0o700);
    assert_eq!(session_mode, 0o600);
    assert_eq!(lock_mode, 0o600);
}

#[test]
fn clear_protects_active_session_and_reports_missing() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut session = new_session(&store, "protected");
    store.persist(&mut session).unwrap();

    assert!(matches!(
        store.clear(&session.session_id, &[session.session_id.clone()]),
        Err(SessionError::ActiveSession { .. })
    ));
    assert!(store.load(&session.session_id).is_ok());

    store.clear(&session.session_id, &[]).unwrap();
    assert!(matches!(
        store.clear(&session.session_id, &[]),
        Err(SessionError::NotFound { .. })
    ));

    let mut first = new_session(&store, "clear first");
    let mut second = new_session(&store, "clear second");
    store.persist(&mut first).unwrap();
    store.persist(&mut second).unwrap();
    store.clear(&first.session_id, &[]).unwrap();
    store.clear(&second.session_id, &[]).unwrap();
    assert!(matches!(
        store.load(&first.session_id),
        Err(SessionError::NotFound { .. })
    ));
    assert!(matches!(
        store.load(&second.session_id),
        Err(SessionError::NotFound { .. })
    ));
}

#[test]
fn clear_missing_session_without_scoped_directory_reports_not_found() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let session_id = ProviderSessionId::new();

    assert!(!store.base_dir.exists());
    assert!(matches!(
        store.clear(&session_id, &[]),
        Err(SessionError::NotFound {
            session_id: missing
        }) if missing == session_id.as_str()
    ));
    assert!(!store.base_dir.exists());
}
