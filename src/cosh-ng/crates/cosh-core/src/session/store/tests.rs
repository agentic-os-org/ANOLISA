use super::super::io::{
    now_ms, private_open_options, reset_session_file_read_count, session_file_read_count,
    write_atomic_file,
};
use super::super::listing::ListEntry;
use super::super::summary::{
    MAX_PROMPT_PREVIEW_CHARS, MAX_SUMMARY_MODEL_BYTES, MAX_SUMMARY_WORKSPACE_BYTES,
};
use super::*;
use std::os::unix::fs::MetadataExt;

use std::time::{Duration, UNIX_EPOCH};

#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStringExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn store(temp: &tempfile::TempDir) -> SessionStore {
    workspace_store(temp.path().join("sessions").to_str().unwrap(), temp.path()).unwrap()
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
    let store = workspace_store(DEFAULT_SESSION_PERSIST_DIR, &workspace).unwrap();
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
            workspace_store(DEFAULT_SESSION_PERSIST_DIR, workspace),
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
    for (id, seconds) in [(&older.session_id, 1), (&newer.session_id, 2)] {
        File::open(store.session_file(id))
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(seconds)))
            .unwrap();
    }

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

    let local_store = workspace_store(persist_dir.to_str().unwrap(), &local_workspace).unwrap();
    let foreign_store = workspace_store(persist_dir.to_str().unwrap(), &foreign_workspace).unwrap();

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

    let store = workspace_store(persist_dir.to_str().unwrap(), &workspace).unwrap();
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

    let local_store = workspace_store(persist_dir.to_str().unwrap(), &local_workspace).unwrap();
    let foreign_store = workspace_store(persist_dir.to_str().unwrap(), &foreign_workspace).unwrap();

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

    let local_store = workspace_store(persist_dir.to_str().unwrap(), &local_workspace).unwrap();
    let foreign_store = workspace_store(persist_dir.to_str().unwrap(), &foreign_workspace).unwrap();

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
            workspace_store(persist_dir.to_str().unwrap(), &workspace).unwrap()
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
    let store = workspace_store("sessions", &workspace).unwrap();
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
    let store = workspace_store("sessions", &workspace).unwrap();
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
    let _budget_lock = store.acquire_budget_lock().unwrap();
    drop(_budget_lock);
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
    let store = workspace_store("sessions", &workspace).unwrap();
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
    let store = workspace_store("sessions", &workspace).unwrap();
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
    let store = workspace_store(DEFAULT_SESSION_PERSIST_DIR, &workspace).unwrap();
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

    let store = workspace_store(DEFAULT_SESSION_PERSIST_DIR, &workspace).unwrap();

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
    let store = workspace_store(DEFAULT_SESSION_PERSIST_DIR, &workspace).unwrap();
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
    let store = workspace_store(DEFAULT_SESSION_PERSIST_DIR, &workspace).unwrap();
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
    let store = workspace_store(shared.to_str().unwrap(), temp.path()).unwrap();
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
            let store = workspace_store(persist_dir, workspace).unwrap();
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
    let store = workspace_store("sessions-link", &workspace).unwrap();
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
    let first = workspace_store(root.to_str().unwrap(), &first_workspace).unwrap();
    let mut session = new_session(&first, "first workspace");
    first.persist(&mut session).unwrap();
    let first_base_dir = first.base_dir.clone();
    let second = workspace_store(root.to_str().unwrap(), &second_workspace).unwrap();
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
        workspace_store(root.to_str().unwrap(), &second_workspace),
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
    let store = workspace_store(root.to_str().unwrap(), &workspace).unwrap();
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

    let store = workspace_store(DEFAULT_SESSION_PERSIST_DIR, &workspace).unwrap();

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
    let store = workspace_store(DEFAULT_SESSION_PERSIST_DIR, &workspace).unwrap();
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
    let store = workspace_store(DEFAULT_SESSION_PERSIST_DIR, &workspace).unwrap();
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

    let store = workspace_store(linked_root.to_str().unwrap(), temp.path()).unwrap();
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
    let store = workspace_store(DEFAULT_SESSION_PERSIST_DIR, &workspace).unwrap();
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
    let store = workspace_store(DEFAULT_SESSION_PERSIST_DIR, &workspace).unwrap();
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

// ---- per-user task budget: 7 days / 100 MiB (S01) ----

const TEST_DAY_MS: u64 = 24 * 60 * 60 * 1000;

fn budgeted_store(
    temp: &tempfile::TempDir,
    retention_days: u64,
    budget_bytes: u64,
) -> SessionStore {
    store(temp).with_task_budget(retention_days, budget_bytes)
}

/// Plants an envelope whose authoritative last-commit clock is `updated_at_ms`.
///
/// `persist` always stamps `now_ms()`, so a backdated clock has to be written
/// directly; `pad_bytes` controls the file size for capacity assertions.
fn plant_session(store: &SessionStore, updated_at_ms: u64, pad_bytes: usize) -> ProviderSessionId {
    let session_id = ProviderSessionId::new();
    let mut session = PersistedSession::new(
        session_id.clone(),
        store.workspace_scope().to_string(),
        "mock-model".to_string(),
        vec![
            Message::user("prompt"),
            Message::assistant(&"x".repeat(pad_bytes)),
        ],
    );
    session.updated_at_ms = updated_at_ms;
    fs::create_dir_all(&store.base_dir).unwrap();
    let path = store.session_file(&session_id);
    fs::write(&path, serde_json::to_vec_pretty(&session).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    session_id
}

/// Plants a raw envelope body under a canonical session filename.
fn plant_raw_envelope(store: &SessionStore, body: &[u8]) -> ProviderSessionId {
    let session_id = ProviderSessionId::new();
    fs::create_dir_all(&store.base_dir).unwrap();
    let path = store.session_file(&session_id);
    fs::write(&path, body).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    session_id
}

fn set_mtime(path: &Path, milliseconds: u64) {
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(UNIX_EPOCH + Duration::from_millis(milliseconds))
        .unwrap();
}

fn session_bytes(store: &SessionStore, session_id: &ProviderSessionId) -> u64 {
    fs::metadata(store.session_file(session_id)).unwrap().len()
}

#[test]
fn default_task_budget_matches_the_approved_per_user_retention_and_cap() {
    // The approved per-user task budget is 7 days and 100 MiB. Asserted on a
    // store built by the production constructor, so changing either approved
    // number turns this red; the other budget tests pin behaviour at explicit
    // values.
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let now = now_ms();
    let eight_days = plant_session(&store, now - 8 * TEST_DAY_MS, 64);
    let six_days = plant_session(&store, now - 6 * TEST_DAY_MS, 64);

    store
        .enforce_task_budget(&ProviderSessionId::new(), 0)
        .unwrap();

    assert!(
        !store.session_file(&eight_days).exists(),
        "the approved 7-day retention must reclaim an 8-day-old session"
    );
    assert!(
        store.session_file(&six_days).exists(),
        "a session inside the approved 7-day retention must be kept"
    );
    assert_eq!(TASK_BUDGET_BYTES, 100 * 1024 * 1024);
    assert_eq!(TASK_RETENTION_DAYS, 7);
}

#[test]
fn task_budget_evicts_by_the_envelope_commit_clock_not_by_file_mtime() {
    // The approved rule cleans the least recently *interacted* task. Every user
    // turn ends in a commit that stamps `updated_at_ms`, so that clock is the
    // only provable interaction time; the file's mtime can be rewritten by
    // `touch`, backups or restores, and reading a session never refreshes it.
    let temp = tempfile::tempdir().unwrap();
    let store = budgeted_store(&temp, 7, TASK_BUDGET_BYTES);
    let now = now_ms();

    // Stale commit clock inside a freshly touched file: still expired.
    let expired = plant_session(&store, now - 8 * TEST_DAY_MS, 64);
    set_mtime(&store.session_file(&expired), now);
    // Fresh commit clock inside a file whose mtime was pushed 8 days back.
    let recent = plant_session(&store, now - 60_000, 64);
    set_mtime(&store.session_file(&recent), now - 8 * TEST_DAY_MS);

    store
        .enforce_task_budget(&ProviderSessionId::new(), 0)
        .unwrap();

    assert!(
        !store.session_file(&expired).exists(),
        "a session whose last commit is older than the retention window must be evicted"
    );
    assert!(
        store.session_file(&recent).exists(),
        "an old mtime must never make a recently committed session evictable"
    );
}

#[test]
fn task_budget_keeps_a_session_inside_the_retention_window() {
    let temp = tempfile::tempdir().unwrap();
    let store = budgeted_store(&temp, 7, TASK_BUDGET_BYTES);
    let now = now_ms();
    let six_days = plant_session(&store, now - 6 * TEST_DAY_MS, 64);

    store
        .enforce_task_budget(&ProviderSessionId::new(), 0)
        .unwrap();

    assert!(store.session_file(&six_days).exists());
}

#[test]
fn task_budget_evicts_oldest_commit_first_until_the_incoming_commit_fits() {
    let temp = tempfile::tempdir().unwrap();
    let wide = budgeted_store(&temp, 7, TASK_BUDGET_BYTES);
    let now = now_ms();
    // All three are inside the retention window: only capacity may evict them.
    let oldest = plant_session(&wide, now - 3 * 60 * 60 * 1000, 4096);
    let middle = plant_session(&wide, now - 2 * 60 * 60 * 1000, 4096);
    let newest = plant_session(&wide, now - 60 * 60 * 1000, 4096);
    let incoming = 512;
    // Room for the two newest plus the incoming commit, but not for all three.
    let budget = session_bytes(&wide, &middle) + session_bytes(&wide, &newest) + incoming;

    let store = budgeted_store(&temp, 7, budget);
    store
        .enforce_task_budget(&ProviderSessionId::new(), incoming)
        .unwrap();

    assert!(
        !store.session_file(&oldest).exists(),
        "the least recently interacted session must be evicted first"
    );
    assert!(store.session_file(&middle).exists());
    assert!(store.session_file(&newest).exists());
}

#[test]
fn task_budget_never_evicts_the_session_being_persisted() {
    let temp = tempfile::tempdir().unwrap();
    let store = budgeted_store(&temp, 7, TASK_BUDGET_BYTES);
    let now = now_ms();
    let current = plant_session(&store, now - 30 * TEST_DAY_MS, 64);

    store.enforce_task_budget(&current, 0).unwrap();

    assert!(
        store.session_file(&current).exists(),
        "the session being committed is protected regardless of its age"
    );
}

#[test]
fn task_budget_refuses_new_persistence_when_nothing_may_be_evicted() {
    let temp = tempfile::tempdir().unwrap();
    let wide = budgeted_store(&temp, 7, TASK_BUDGET_BYTES);
    // The only session on disk is the one being committed, so there is nothing
    // to reclaim: the contract stops new optional persistence and says so,
    // instead of destroying the current interaction or overrunning the budget.
    let current = plant_session(&wide, now_ms() - 60_000, 64);
    let store = budgeted_store(&temp, 7, 32);

    let result = store.enforce_task_budget(&current, 4096);

    assert!(
        matches!(
            &result,
            Err(SessionError::BudgetExhausted {
                projected_bytes,
                budget_bytes
            }) if *projected_bytes == 4096 && *budget_bytes == 32
        ),
        "expected BudgetExhausted projecting exactly the incoming bytes, got {result:?}"
    );
    let error = result.unwrap_err();
    assert_eq!(error.code(), "budget_exhausted");
    assert!(error
        .to_string()
        .contains("4096 bytes projected after commit"));
    assert!(
        store.session_file(&current).exists(),
        "refusing a commit must not delete anything"
    );
}

#[test]
fn task_budget_refuses_rather_than_evict_a_session_with_a_commit_in_flight() {
    // Capacity pressure must never be relieved by deleting a session another
    // process is committing right now: refusing the new commit is the
    // contract's answer ("stop new optional persistence and say so").
    let temp = tempfile::tempdir().unwrap();
    let wide = budgeted_store(&temp, 7, TASK_BUDGET_BYTES);
    let now = now_ms();
    // Both are inside the retention window, so only capacity could evict them.
    let busy = plant_session(&wide, now - 3 * 60 * 60 * 1000, 4096);
    let current = plant_session(&wide, now - 60_000, 64);
    let lock_path = wide.base_dir.join(format!(".{}.lock", busy.as_str()));
    let lock_file = private_open_options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    rustix::fs::flock(
        &lock_file,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .unwrap();
    // The incoming commit fits only if the busy session is reclaimed.
    let store = budgeted_store(&temp, 7, session_bytes(&wide, &current) + 512);

    let result = store.enforce_task_budget(&current, 4096);

    assert!(
        matches!(result, Err(SessionError::BudgetExhausted { .. })),
        "expected BudgetExhausted, got {result:?}"
    );
    assert!(
        store.session_file(&busy).exists(),
        "a session with a commit in flight must survive capacity pressure"
    );
    assert!(store.session_file(&current).exists());
    rustix::fs::flock(&lock_file, rustix::fs::FlockOperation::Unlock).unwrap();
}

#[test]
fn task_budget_skips_a_session_whose_commit_is_in_flight() {
    // `persist` holds the advisory lock for the whole write transaction, so an
    // unacquirable lock is on-disk proof that another process is committing
    // this session right now.
    let temp = tempfile::tempdir().unwrap();
    let store = budgeted_store(&temp, 7, TASK_BUDGET_BYTES);
    let now = now_ms();
    let busy = plant_session(&store, now - 8 * TEST_DAY_MS, 64);
    // Positive control: without it, "the busy session survived" would also hold
    // when the sweep never ran at all, which is the assertion's whole claim.
    let idle = plant_session(&store, now - 8 * TEST_DAY_MS, 64);
    let lock_path = store.base_dir.join(format!(".{}.lock", busy.as_str()));
    let lock_file = private_open_options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    rustix::fs::flock(
        &lock_file,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .unwrap();

    store
        .enforce_task_budget(&ProviderSessionId::new(), 0)
        .unwrap();

    assert!(
        store.session_file(&busy).exists(),
        "a session with a commit in flight must never be evicted"
    );
    assert!(
        !store.session_file(&idle).exists(),
        "the sweep must have run, otherwise the survival above proves nothing"
    );
    rustix::fs::flock(&lock_file, rustix::fs::FlockOperation::Unlock).unwrap();
}

#[test]
fn task_budget_never_evicts_an_envelope_without_a_provable_clock() {
    // A corrupt or pre-envelope (bare array) file carries no `updated_at_ms`.
    // Absence of proof must make it non-evictable, not "infinitely old".
    let temp = tempfile::tempdir().unwrap();
    let store = budgeted_store(&temp, 7, TASK_BUDGET_BYTES);
    let clockless = plant_raw_envelope(
        &store,
        &serde_json::to_vec(&vec![Message::user("legacy array format")]).unwrap(),
    );
    set_mtime(&store.session_file(&clockless), now_ms() - 30 * TEST_DAY_MS);
    // Positive control for the same reason as the in-flight case: a sweep that
    // never ran would also leave the clockless envelope in place.
    let expired = plant_session(&store, now_ms() - 30 * TEST_DAY_MS, 64);

    store
        .enforce_task_budget(&ProviderSessionId::new(), 0)
        .unwrap();

    assert!(store.session_file(&clockless).exists());
    assert!(
        !store.session_file(&expired).exists(),
        "the sweep must have run, otherwise the survival above proves nothing"
    );
}

#[test]
fn task_budget_counts_every_workspace_scope_of_the_same_user() {
    // The approved budget is a per-user total, so a commit in one workspace
    // must be able to reclaim space from another workspace of the same user.
    let temp = tempfile::tempdir().unwrap();
    let store = budgeted_store(&temp, 7, TASK_BUDGET_BYTES);
    let now = now_ms();
    let root = store.base_dir.parent().unwrap().to_path_buf();
    let other_scope =
        root.join(&hex::encode(sha2::Sha256::digest(b"/somewhere/else"))[..SCOPE_DIR_NAME_LEN]);
    fs::create_dir_all(&other_scope).unwrap();
    let other = ProviderSessionId::new();
    let mut foreign = PersistedSession::new(
        other.clone(),
        "/somewhere/else".to_string(),
        "mock-model".to_string(),
        vec![Message::user("other workspace")],
    );
    foreign.updated_at_ms = now - 9 * TEST_DAY_MS;
    fs::write(
        other_scope.join(format!("{}.json", other.as_str())),
        serde_json::to_vec_pretty(&foreign).unwrap(),
    )
    .unwrap();

    store
        .enforce_task_budget(&ProviderSessionId::new(), 0)
        .unwrap();

    assert!(
        !other_scope
            .join(format!("{}.json", other.as_str()))
            .exists(),
        "an expired session in another workspace of the same user counts toward the budget"
    );
}

#[test]
fn task_budget_leaves_audit_log_and_gateway_records_untouched() {
    // Executable evidence that the approval ledger and execution records are
    // never removed by ordinary task cleanup: they live outside the session
    // root, and the evictor must not widen its enumeration to reach them.
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let base = home.join(".copilot-shell");
    let store = workspace_store(
        base.join("cosh-core/sessions").to_str().unwrap(),
        &workspace,
    )
    .unwrap()
    .with_task_budget(7, TASK_BUDGET_BYTES);
    let now = now_ms();
    let expired = plant_session(&store, now - 8 * TEST_DAY_MS, 64);

    // Decoys that would be evicted by a widened walk: an approval ledger
    // record and an execution record whose names and bodies look exactly like
    // expired session envelopes, plus the log and audit directories.
    let mut ledger = PersistedSession::new(
        ProviderSessionId::new(),
        store.workspace_scope().to_string(),
        "mock-model".to_string(),
        vec![Message::user("approval ledger record")],
    );
    ledger.updated_at_ms = now - 400 * TEST_DAY_MS;
    let ledger_path = base
        .join("gateway/approvals")
        .join(format!("{}.json", ledger.session_id.as_str()));
    let mut execution = ledger.clone();
    execution.session_id = ProviderSessionId::new();
    let execution_path = base
        .join("execution-records")
        .join(format!("{}.json", execution.session_id.as_str()));
    for path in [&ledger_path, &execution_path] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
    }
    fs::write(&ledger_path, serde_json::to_vec_pretty(&ledger).unwrap()).unwrap();
    fs::write(
        &execution_path,
        serde_json::to_vec_pretty(&execution).unwrap(),
    )
    .unwrap();
    let audit_path = base.join("audit/2026-01-01.jsonl");
    let log_path = base.join("logs/cosh-shell.log.2026-01-01");
    for path in [&audit_path, &log_path] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"record").unwrap();
        set_mtime(path, now - 400 * TEST_DAY_MS);
    }
    // A scope-shaped directory outside the session root. Widening the
    // enumeration root by even one level must not turn it into task data.
    let mut foreign_scope = ledger.clone();
    foreign_scope.session_id = ProviderSessionId::new();
    let foreign_scope_path = base
        .join("0".repeat(24))
        .join(format!("{}.json", foreign_scope.session_id.as_str()));
    fs::create_dir_all(foreign_scope_path.parent().unwrap()).unwrap();
    fs::write(
        &foreign_scope_path,
        serde_json::to_vec_pretty(&foreign_scope).unwrap(),
    )
    .unwrap();

    store
        .enforce_task_budget(&ProviderSessionId::new(), 0)
        .unwrap();

    assert!(
        !store.session_file(&expired).exists(),
        "the expired session under the session root must still be evicted"
    );
    for path in [
        &ledger_path,
        &execution_path,
        &audit_path,
        &log_path,
        &foreign_scope_path,
    ] {
        assert!(
            path.exists(),
            "{} must survive task cleanup",
            path.display()
        );
    }
}

#[cfg(unix)]
#[test]
fn task_budget_does_not_count_a_symlinked_scope_it_can_never_reclaim() {
    // A symlink wearing a scope name can never be opened for eviction, because
    // every path component is opened with `O_NOFOLLOW`. Counting its bytes
    // would therefore make the budget permanently unrelievable, so accounting
    // and eviction have to agree on what exists -- and the link must not become
    // a way to reach outside the session root.
    let temp = tempfile::tempdir().unwrap();
    let wide = budgeted_store(&temp, 7, TASK_BUDGET_BYTES);
    let outside = temp.path().join("outside-the-session-root");
    fs::create_dir_all(&outside).unwrap();
    let mut resident = PersistedSession::new(
        ProviderSessionId::new(),
        wide.workspace_scope().to_string(),
        "mock-model".to_string(),
        vec![
            Message::user("outside"),
            Message::assistant(&"x".repeat(4096)),
        ],
    );
    // A fresh clock: nothing here is expired, so only accounting can explain a
    // refusal.
    resident.updated_at_ms = now_ms() - 60_000;
    let outside_path = outside.join(format!("{}.json", resident.session_id.as_str()));
    fs::write(&outside_path, serde_json::to_vec_pretty(&resident).unwrap()).unwrap();
    let root = wide.base_dir.parent().unwrap().to_path_buf();
    fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("1".repeat(24))).unwrap();

    // Budget below the unreachable bytes: a small commit must still be accepted.
    let store = budgeted_store(&temp, 7, 1024);
    store
        .enforce_task_budget(&ProviderSessionId::new(), 64)
        .unwrap();

    assert!(
        outside_path.exists(),
        "a symlinked scope directory must not be walked"
    );
}

/// Rewrites a committed envelope's authoritative clock, keeping every other
/// field exactly as `persist` wrote it.
fn backdate_envelope(store: &SessionStore, session_id: &ProviderSessionId, updated_at_ms: u64) {
    let path = store.session_file(session_id);
    let bytes = fs::read(&path).unwrap();
    let mut envelope: PersistedSession = serde_json::from_slice(&bytes).unwrap();
    envelope.updated_at_ms = updated_at_ms;
    fs::write(&path, serde_json::to_vec_pretty(&envelope).unwrap()).unwrap();
}

#[test]
fn a_resumed_session_that_recommitted_survives_a_concurrent_budget_sweep() {
    // C2 in its protected form. A session idle past the retention window is
    // resumed and re-commits at once, so a sweep running from another process
    // reads a fresh clock and keeps it, and the resumed turn's own commit then
    // lands instead of failing with Conflict and losing the turn.
    //
    // The re-commit here is driven through `SessionStore` directly, so this case
    // pins what a re-committed session survives, not that resume performs the
    // re-commit; `headless::tests::resuming_a_long_idle_session_refreshes_its_commit_clock`
    // drives `SessionRuntime::initialize` and is the case that goes red if the
    // resume stamp is removed.
    let temp = tempfile::tempdir().unwrap();
    let wide = budgeted_store(&temp, 7, TASK_BUDGET_BYTES);
    let mut resumed = new_session(&wide, "resumed after a long idle");
    wide.persist(&mut resumed).unwrap();
    let session_id = resumed.session_id.clone();
    backdate_envelope(&wide, &session_id, now_ms() - 8 * TEST_DAY_MS);

    // The resume path re-commits; this is what refreshes the clock.
    let resumer = budgeted_store(&temp, 7, TASK_BUDGET_BYTES);
    let mut loaded = resumer.load(&session_id).unwrap();
    resumer.persist(&mut loaded).unwrap();
    assert!(
        now_ms().saturating_sub(loaded.updated_at_ms) < 60_000,
        "a commit must refresh the authoritative clock, otherwise the resumed          session still looks expired to every other process"
    );

    // Another process now runs the sweep.
    let sweeper = budgeted_store(&temp, 7, TASK_BUDGET_BYTES);
    sweeper
        .enforce_task_budget(&ProviderSessionId::new(), 0)
        .unwrap();
    assert!(
        sweeper.session_file(&session_id).exists(),
        "a session that just re-committed must not be reclaimed as idle"
    );

    // And the resumed turn's own commit still lands.
    loaded
        .messages
        .push(Message::assistant("turn after resume"));
    resumer.persist(&mut loaded).unwrap();
    assert_eq!(resumer.load(&session_id).unwrap().messages.len(), 3);
}

#[test]
fn persist_applies_the_task_budget_before_committing() {
    // Proves the wiring, not just the helper: an ordinary commit reclaims
    // expired space first.
    let temp = tempfile::tempdir().unwrap();
    let wide = budgeted_store(&temp, 7, TASK_BUDGET_BYTES);
    let expired = plant_session(&wide, now_ms() - 8 * TEST_DAY_MS, 64);
    let store = budgeted_store(&temp, 7, TASK_BUDGET_BYTES);
    let mut session = new_session(&store, "hello");

    store.persist(&mut session).unwrap();

    assert!(!store.session_file(&expired).exists());
    assert!(store.session_file(&session.session_id).exists());
}

#[test]
fn task_budget_does_not_project_the_committed_sessions_own_stored_bytes() {
    // The commit replaces whatever `keep` already occupies, so its old bytes
    // must not be projected on top of the incoming ones. Counting both would
    // make a single session that grows toward the budget first over-evict every
    // other session of the user and then report a budget that is not full.
    let temp = tempfile::tempdir().unwrap();
    let wide = budgeted_store(&temp, 7, TASK_BUDGET_BYTES);
    let keep = plant_session(&wide, now_ms() - 60_000, 4096);
    let stored = session_bytes(&wide, &keep);
    assert!(
        stored > 4096,
        "the fixture must be larger than the incoming commit for the projection to differ"
    );
    // Admits the incoming bytes exactly, so double-counting `keep` overruns it.
    let store = budgeted_store(&temp, 7, stored);

    store
        .enforce_task_budget(&keep, stored)
        .expect("the only session on disk is the one being committed");

    assert!(store.session_file(&keep).exists());
}

#[test]
fn task_capacity_eviction_spares_a_session_that_committed_since_ordering() {
    // The capacity leg orders candidates by a snapshot clock and then re-reads
    // each envelope under its own lock, because a session that committed in
    // between is no longer the least recently interacted one and must not become
    // the victim of a decision taken from a stale clock. Every other budget case
    // is single threaded, so snapshot and re-read always agree and the guard is
    // invisible to them; this drives the leg directly with a stale snapshot.
    let temp = tempfile::tempdir().unwrap();
    let wide = budgeted_store(&temp, 7, TASK_BUDGET_BYTES);
    let victim = plant_session(&wide, now_ms() - 60_000, 4096);
    let stored = session_bytes(&wide, &victim);
    let store = budgeted_store(&temp, 7, stored / 2);
    let stale_clock = now_ms() - 8 * TEST_DAY_MS;

    let scopes = store.budget_scopes().unwrap();
    let mut survivors: Vec<(u64, &BudgetScope, &BudgetEntry)> = Vec::new();
    for scope in &scopes {
        for entry in &scope.entries {
            survivors.push((stale_clock, scope, entry));
        }
    }
    assert_eq!(survivors.len(), 1, "the fixture plants exactly one session");

    let reclaimed = store.evict_for_capacity(survivors, stored, stored);

    assert_eq!(
        reclaimed, 0,
        "the envelope on disk committed after the snapshot clock, so it is not the victim"
    );
    assert!(store.session_file(&victim).exists());
}

#[test]
fn task_budget_does_not_evict_a_session_whose_process_is_still_running() {
    // A running process holds no session lock between turns, so its envelope clock
    // is all a sweep has to judge it by. Past the retention window that clock says
    // "reclaimable" even though the next turn is about to run, and that turn would
    // commit against a missing generation, get Conflict, and lose the work. The
    // process-lifetime lease is what makes the sweep skip it.
    let temp = tempfile::tempdir().unwrap();
    let store = budgeted_store(&temp, 7, TASK_BUDGET_BYTES);
    let live = plant_session(&store, now_ms() - 8 * TEST_DAY_MS, 4096);
    let lease = store
        .acquire_live_lease(&live)
        .expect("take the activity lease")
        .expect("a planted envelope has a scoped directory to lease in");

    let result = store.enforce_task_budget(&ProviderSessionId::new(), TASK_BUDGET_BYTES);
    assert!(
        matches!(result, Err(SessionError::BudgetExhausted { .. })),
        "an expired but still running session must not be reclaimable, got {result:?}"
    );
    assert!(
        store.session_file(&live).exists(),
        "the sweep deleted the envelope of a session whose process is still running"
    );

    // Positive control: the same envelope is reclaimed as soon as the lease dies,
    // so the refusal above came from the lease and not from the fixture being
    // unevictable for some other reason. A crashed process cannot pin its session
    // forever, because the kernel drops the flock when the descriptor goes away.
    // A fresh store is used so the age leg is not amortized away by the rescan
    // interval the first call already consumed.
    drop(lease);
    let sweeper = budgeted_store(&temp, 7, TASK_BUDGET_BYTES);
    sweeper
        .enforce_task_budget(&ProviderSessionId::new(), 0)
        .expect("an unleased expired session is reclaimable");
    assert!(
        !sweeper.session_file(&live).exists(),
        "an expired session with no live process must be reclaimed"
    );
}

#[test]
fn a_budget_decision_holds_the_per_user_lock_through_its_write() {
    // The capacity scan and `write_atomic` have to be one critical section per
    // user. A per-session lock cannot provide it: two processes committing
    // *different* sessions hold different session locks, so both can scan one
    // snapshot, both conclude they fit, and both write. This drives the mechanism
    // directly, which a barrier race can only show probabilistically.
    let temp = tempfile::tempdir().unwrap();
    let store = budgeted_store(&temp, 7, TASK_BUDGET_BYTES);
    let root = store.base_dir.parent().expect("session root").to_path_buf();
    fs::create_dir_all(&root).unwrap();
    let mut session = new_session(&store, "held by another writer");
    let session_id = session.session_id.clone();

    let root_storage =
        ScopedStorage::new(store.budget.path.clone()).expect("open the user budget namespace");
    let root_directory = root_storage
        .directory(true)
        .expect("inspect the session root")
        .expect("the session root exists");
    let held = root_storage
        .acquire_blocking_lock(&root_directory, BUDGET_LOCK_FILENAME)
        .expect("take the per-user lock from outside");

    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let result = store.persist(&mut session);
        let _ = done_tx.send(result.is_ok());
        result
    });

    assert!(
        done_rx.recv_timeout(Duration::from_millis(500)).is_err(),
        "a commit completed while another holder had the per-user budget lock, so the \
         capacity decision and the write are not one critical section"
    );
    drop(held);
    assert!(
        done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the commit must finish once the per-user lock is released"),
        "the commit failed after the lock was released"
    );
    writer.join().expect("the writer thread panicked").unwrap();

    let after = budgeted_store(&temp, 7, TASK_BUDGET_BYTES);
    assert!(
        after.session_file(&session_id).exists(),
        "the released commit must have written its envelope"
    );
}

#[test]
fn two_concurrent_writers_do_not_finish_over_the_per_user_budget() {
    // End-to-end form of the same invariant: two processes, two different
    // sessions, and a budget that fits exactly one envelope. Serialized by the
    // per-user lock, the second writer sees the first one's bytes and reclaims
    // them; without the lock both can admit from the same empty snapshot and finish
    // at twice the cap.
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let probe = budgeted_store(&temp, 7, TASK_BUDGET_BYTES);
    let mut sized = new_session(&probe, "writer");
    probe.persist(&mut sized).expect("size one envelope");
    let one_envelope = session_bytes(&probe, &sized.session_id);
    fs::remove_file(probe.session_file(&sized.session_id)).unwrap();

    let root_text = root.to_str().expect("utf-8 session root").to_string();
    let workspace = temp.path().to_path_buf();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let writers = (0..2)
        .map(|index| {
            let barrier = std::sync::Arc::clone(&barrier);
            let root_text = root_text.clone();
            let workspace = workspace.clone();
            std::thread::spawn(move || {
                let store = workspace_store(&root_text, &workspace)
                    .expect("build store")
                    .with_task_budget(7, one_envelope);
                let mut session = new_session(&store, "writer");
                // Release both writers at the same instant, before either has
                // scanned the session root.
                barrier.wait();
                let result = store.persist(&mut session);
                (index, session.session_id.to_string(), result.is_ok())
            })
        })
        .collect::<Vec<_>>();
    let outcomes = writers
        .into_iter()
        .map(|writer| writer.join().expect("a writer thread panicked"))
        .collect::<Vec<_>>();

    let after = budgeted_store(&temp, 7, one_envelope);
    let accounted: u64 = after
        .budget_scopes()
        .unwrap()
        .iter()
        .flat_map(|scope| scope.entries.iter())
        .fold(0u64, |total, entry| total + entry.bytes);
    assert!(
        accounted <= one_envelope,
        "two concurrent writers finished at {accounted} bytes against a {one_envelope} byte \
         budget, so the cap is not enforced across processes; outcomes {outcomes:?}"
    );
    assert!(
        outcomes.iter().any(|(_, _, ok)| *ok),
        "at least one writer must succeed, got {outcomes:?}"
    );
}

#[test]
fn eviction_holds_the_activity_lease_until_the_envelope_is_removed() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let id = plant_session(&store, now_ms() - 8 * TEST_DAY_MS, 64);
    let entry = BudgetEntry {
        session_id: id.clone(),
        bytes: session_bytes(&store, &id),
    };
    let scope = BudgetScope {
        path: store.base_dir.clone(),
        entries: Vec::new(),
        activity: vec![store.base_dir.clone()],
        legacy: false,
        modern: true,
        legacy_generation: None,
    };
    let result = locked_clock_and_evict(&store.scoped, &scope, &entry, |_| {
        let lease = private_open_options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(store.base_dir.join(format!(".{}.live", id.as_str())))
            .unwrap();
        assert_eq!(
            rustix::fs::flock(&lease, rustix::fs::FlockOperation::NonBlockingLockExclusive),
            Err(rustix::io::Errno::WOULDBLOCK),
            "a new runtime must not claim a candidate between its probe and unlink"
        );
        true
    });
    assert!(result.is_some_and(|(_, evicted)| evicted));
    assert!(!store.session_file(&id).exists());
}

#[test]
fn task_budget_counts_the_same_id_in_another_workspace_scope() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let first_workspace = temp.path().join("first");
    let second_workspace = temp.path().join("second");
    fs::create_dir_all(&first_workspace).unwrap();
    fs::create_dir_all(&second_workspace).unwrap();
    let first = workspace_store(root.to_str().unwrap(), &first_workspace).unwrap();
    let mut original = new_session(&first, "same id");
    first.persist(&mut original).unwrap();
    let bytes = session_bytes(&first, &original.session_id);
    let _lease = first
        .acquire_live_lease(&original.session_id)
        .unwrap()
        .unwrap();
    let second = workspace_store(root.to_str().unwrap(), &second_workspace)
        .unwrap()
        .with_task_budget(7, bytes + 128);
    let mut incoming = new_session(&second, "same id");
    incoming.session_id = original.session_id.clone();
    let result = second.persist(&mut incoming);
    assert!(
        matches!(result, Err(SessionError::BudgetExhausted { .. })),
        "{result:?}"
    );
    assert!(first.session_file(&original.session_id).exists());
    assert!(!second.session_file(&incoming.session_id).exists());
}

#[test]
fn a_loaded_legacy_record_cannot_recreate_explicitly_cleared_history() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(workspace.join("project-sessions")).unwrap();
    let store = workspace_store("project-sessions", &workspace).unwrap();
    let id = ProviderSessionId::new();
    let legacy = workspace
        .join("project-sessions")
        .join(format!("{id}.json"));
    fs::write(
        &legacy,
        serde_json::to_vec(&vec![Message::user("legacy")]).unwrap(),
    )
    .unwrap();
    let mut loaded = store.load(&id).unwrap();
    store.clear(&id, &[]).unwrap();
    loaded
        .messages
        .push(Message::assistant("must not resurrect"));
    let result = store.persist(&mut loaded);
    assert!(
        matches!(result, Err(SessionError::Conflict { .. })),
        "{result:?}"
    );
    assert!(!legacy.exists());
    assert!(!store.session_file(&id).exists());
}

fn workspace_store(persist_dir: &str, workspace: &Path) -> Result<SessionStore, SessionError> {
    SessionStore::for_test_workspace(persist_dir, workspace)
}

#[test]
fn legacy_migration_respects_the_cap_when_source_cleanup_would_fail() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(workspace.join("project-sessions")).unwrap();
    let store = workspace_store("project-sessions", &workspace).unwrap();
    let id = ProviderSessionId::new();
    let legacy = workspace
        .join("project-sessions")
        .join(format!("{id}.json"));
    fs::write(
        &legacy,
        serde_json::to_vec(&vec![Message::user(&"x".repeat(4096))]).unwrap(),
    )
    .unwrap();
    let mut loaded = store.load(&id).unwrap();
    let cap = serde_json::to_vec_pretty(&loaded).unwrap().len() as u64 + 128;
    let store = store.with_task_budget(7, cap);
    let _lease = store.acquire_live_lease(&id).unwrap();
    let root = workspace.join("project-sessions");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o500)).unwrap();
    let result = store.persist(&mut loaded);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let actual = fs::metadata(&legacy).unwrap().len()
        + fs::metadata(store.session_file(&id))
            .map(|metadata| metadata.len())
            .unwrap_or(0);
    assert!(
        actual <= cap,
        "legacy cleanup failure left {actual} bytes above cap {cap}"
    );
    assert!(
        matches!(result, Err(SessionError::BudgetExhausted { .. })),
        "source and destination remain separately accounted until successful cleanup: {result:?}"
    );
    assert!(legacy.exists());
    assert!(!store.session_file(&id).exists());
    assert!(fs::metadata(&legacy).unwrap().len() <= cap);
}

#[test]
fn failed_resume_cleanup_preserves_a_replacement_control_inode() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let id = ProviderSessionId::new();
    let lease = store.acquire_live_lease(&id).unwrap().unwrap();
    let path = store.base_dir.join(format!(".{id}.live"));
    fs::remove_file(&path).unwrap();
    fs::write(&path, b"replacement inode").unwrap();
    store.cleanup_failed_resume(&id, &lease).unwrap();
    store.cleanup_rejected_commit(&id, &lease).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"replacement inode");
}

#[test]
fn absolute_configuration_preserves_registered_relative_legacy_sources() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let relative = workspace_store("project-sessions", &workspace).unwrap();
    relative
        .persist(&mut new_session(&relative, "register relative root"))
        .unwrap();
    let id = ProviderSessionId::new();
    let root = workspace.join("project-sessions");
    let legacy = root.join(format!("{id}.json"));
    fs::write(
        &legacy,
        serde_json::to_vec(&vec![Message::user("expired relative legacy")]).unwrap(),
    )
    .unwrap();
    fs::File::open(&legacy)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1)))
        .unwrap();
    let absolute = workspace_store(root.to_str().unwrap(), &workspace).unwrap();
    absolute
        .persist(&mut new_session(
            &absolute,
            "same root with absolute configuration",
        ))
        .unwrap();
    assert!(
        !legacy.exists(),
        "changing configuration must retain old eligible legacy positions"
    );
}

#[test]
fn default_test_store_keeps_data_and_budget_inside_its_fixture() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = fs::canonicalize(temp.path()).unwrap();
    let store = workspace_store(DEFAULT_SESSION_PERSIST_DIR, &workspace).unwrap();
    assert!(
        store.base_dir.starts_with(&workspace),
        "test data must not enter the invoking HOME: {}",
        store.base_dir.display()
    );
    assert!(
        store.budget.path.starts_with(&workspace),
        "test budget must not enter the invoking HOME: {}",
        store.budget.path.display()
    );
    store
        .persist(&mut new_session(&store, "private test history"))
        .unwrap();
}

#[test]
fn rejected_first_commit_preserves_a_preexisting_generation_control() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp).with_task_budget(7, 1);
    let mut incoming = new_session(&store, "refused");
    let _directory = store.scoped.directory(true).unwrap().unwrap();
    let control = store
        .base_dir
        .join(format!(".{}.lock", incoming.session_id));
    fs::write(&control, b"preexisting").unwrap();
    let inode = fs::metadata(&control).unwrap().ino();
    assert!(matches!(
        store.persist(&mut incoming),
        Err(SessionError::BudgetExhausted { .. })
    ));
    assert!(!store.session_file(&incoming.session_id).exists());
    assert_eq!(fs::read(&control).unwrap(), b"preexisting");
    assert_eq!(fs::metadata(&control).unwrap().ino(), inode);
}

#[test]
fn rejected_commit_cleanup_preserves_existing_data_and_preexisting_leases() {
    for legacy in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(workspace.join("project-sessions")).unwrap();
        let store = workspace_store("project-sessions", &workspace).unwrap();
        let mut session = new_session(&store, "existing history");
        if legacy {
            fs::write(
                workspace
                    .join("project-sessions")
                    .join(format!("{}.json", session.session_id)),
                serde_json::to_vec(&session.messages).unwrap(),
            )
            .unwrap();
        } else {
            store.persist(&mut session).unwrap();
        }
        let lease = store
            .acquire_live_lease(&session.session_id)
            .unwrap()
            .unwrap();
        let control = store.base_dir.join(format!(".{}.live", session.session_id));
        let inode = fs::metadata(&control).unwrap().ino();
        store
            .cleanup_rejected_commit(&session.session_id, &lease)
            .unwrap();
        assert_eq!(fs::metadata(&control).unwrap().ino(), inode);
        assert!(store.load(&session.session_id).is_ok());
        drop(lease);
        let missing = ProviderSessionId::new();
        let preexisting = store.base_dir.join(format!(".{missing}.live"));
        fs::write(&preexisting, b"preexisting").unwrap();
        let lease = store.acquire_live_lease(&missing).unwrap().unwrap();
        store.cleanup_rejected_commit(&missing, &lease).unwrap();
        assert_eq!(fs::read(&preexisting).unwrap(), b"preexisting");
    }
}

#[test]
fn an_oversized_first_commit_leaves_no_new_generation_control() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut incoming = new_session(&store, &"x".repeat(MAX_SESSION_FILE_BYTES as usize));
    let result = store.persist(&mut incoming);
    assert!(matches!(result, Err(SessionError::Corrupt { .. })));
    assert!(!store.session_file(&incoming.session_id).exists());
    assert!(
        !store
            .base_dir
            .join(format!(".{}.lock", incoming.session_id))
            .exists(),
        "oversized first commit leaked generation control"
    );
}

#[test]
fn default_test_budget_keeps_its_qualified_workspace_legacy_candidate() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let legacy_dir = workspace.join("sessions");
    fs::create_dir_all(&legacy_dir).unwrap();
    let id = ProviderSessionId::new();
    let legacy = legacy_dir.join(format!("{id}.json"));
    fs::write(
        &legacy,
        serde_json::to_vec(&vec![Message::user("old default history")]).unwrap(),
    )
    .unwrap();
    File::open(&legacy)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1)))
        .unwrap();
    let store = workspace_store(DEFAULT_SESSION_PERSIST_DIR, &workspace).unwrap();
    store
        .persist(&mut new_session(&store, "new default history"))
        .unwrap();
    assert!(
        !legacy.exists(),
        "private default test budget must retain its qualified legacy candidate"
    );
}

#[test]
fn modern_budget_preserves_unreadable_envelopes() {
    for capacity in [false, true] {
        for invalid in 0..6 {
            let temp = tempfile::tempdir().unwrap();
            let store = budgeted_store(&temp, 7, if capacity { 1 } else { 100 * 1024 * 1024 });
            let id = plant_session(&store, now_ms() - 9 * TEST_DAY_MS, 64);
            let path = store.session_file(&id);
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            match invalid {
                0 => value["schema_version"] = serde_json::json!(2),
                1 => value["session_id"] = serde_json::json!(ProviderSessionId::new()),
                2 => {
                    value.as_object_mut().unwrap().remove("messages");
                }
                3 => value["messages"] = serde_json::json!([{"role":"unknown"}]),
                4 => value["workspace_scope"] = serde_json::json!("/unrelated-workspace"),
                _ => value["schema_version"] = serde_json::json!(4294967297_u64),
            }
            let bytes = serde_json::to_vec(&value).unwrap();
            fs::write(&path, &bytes).unwrap();
            let before = fs::metadata(&path).unwrap();
            assert!(
                store.load(&id).is_err(),
                "invalid modern envelope must fail normal loading"
            );
            let result = store.enforce_task_budget(&ProviderSessionId::new(), 0);
            assert_eq!(fs::read(&path).ok().as_deref(), Some(bytes.as_slice()), "unsupported or corrupt envelope must survive budget sweeping: capacity={capacity}, invalid={invalid}");
            let after = fs::metadata(&path).unwrap();
            assert_eq!(
                (
                    before.ino(),
                    before.mode(),
                    before.mtime(),
                    before.mtime_nsec()
                ),
                (after.ino(), after.mode(), after.mtime(), after.mtime_nsec())
            );
            if capacity {
                assert!(matches!(result, Err(SessionError::BudgetExhausted { .. })));
            } else {
                result.unwrap();
            }
        }
    }
}

#[test]
fn historical_legacy_marker_loss_does_not_reauthorize_current_or_absolute_roots() {
    for change in 0..4 {
        for absolute in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let workspace = temp.path().join("workspace");
            fs::create_dir_all(&workspace).unwrap();
            let relative = workspace_store("project-sessions", &workspace).unwrap();
            relative
                .persist(&mut new_session(&relative, "register ownership"))
                .unwrap();
            let root = workspace.join("project-sessions");
            let marker = root.join(".cosh-legacy-owner");
            assert!(marker.is_file());
            let path = root.join(format!("{}.json", ProviderSessionId::new()));
            let bytes = serde_json::to_vec(&vec![Message::user("unproven legacy")]).unwrap();
            fs::write(&path, &bytes).unwrap();
            set_mtime(&path, 1);
            let before = fs::metadata(&path).unwrap();
            let outside = temp.path().join("outside-marker");
            fs::write(&outside, b"outside file must survive").unwrap();
            match change {
                0 => fs::remove_file(&marker).unwrap(),
                1 => fs::write(&marker, ProviderSessionId::new().as_str()).unwrap(),
                2 => {
                    fs::remove_file(&marker).unwrap();
                    std::os::unix::fs::symlink(&outside, &marker).unwrap();
                }
                _ => fs::set_permissions(&marker, fs::Permissions::from_mode(0o644)).unwrap(),
            }
            let selected = if absolute {
                root.to_str().unwrap()
            } else {
                "project-sessions"
            };
            let store = workspace_store(selected, &workspace).unwrap();
            for _ in 0..2 {
                store
                    .persist(&mut new_session(&store, "marker continuity is required"))
                    .unwrap();
            }
            assert_eq!(
                fs::read(&path).ok().as_deref(),
                Some(bytes.as_slice()),
                "lost ownership must never be silently renewed"
            );
            let after = fs::metadata(&path).unwrap();
            assert_eq!(
                (
                    before.ino(),
                    before.mode(),
                    before.mtime(),
                    before.mtime_nsec()
                ),
                (after.ino(), after.mode(), after.mtime(), after.mtime_nsec())
            );
            assert_eq!(fs::read(&outside).unwrap(), b"outside file must survive");
            if change == 0 {
                assert!(!marker.exists());
            }
        }
    }
}

#[test]
fn legacy_eviction_revalidates_the_pinned_directory_generation() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let store = workspace_store("project-sessions", &workspace).unwrap();
    store
        .persist(&mut new_session(&store, "register ownership"))
        .unwrap();
    let root = workspace.join("project-sessions");
    let id = ProviderSessionId::new();
    let bytes = serde_json::to_vec(&vec![Message::user("expired legacy")]).unwrap();
    let path = root.join(format!("{id}.json"));
    fs::write(&path, &bytes).unwrap();
    set_mtime(&path, 1);
    let scope = store
        .budget_scopes()
        .unwrap()
        .into_iter()
        .find(|scope| scope.path == root)
        .unwrap();
    fs::rename(&root, workspace.join("old-directory")).unwrap();
    fs::create_dir_all(&root).unwrap();
    fs::write(&path, &bytes).unwrap();
    set_mtime(&path, 1);
    let before = fs::metadata(&path).unwrap();
    let entry = BudgetEntry {
        session_id: id,
        bytes: bytes.len() as u64,
    };
    let scoped = ScopedStorage::new(root).unwrap();
    assert_eq!(
        locked_clock_and_evict(&scoped, &scope, &entry, |_| true),
        None,
        "pinned replacement directory must not be evicted"
    );
    assert_eq!(
        fs::read(&path).unwrap(),
        bytes,
        "replacement after discovery must survive"
    );
    let after = fs::metadata(&path).unwrap();
    assert_eq!(
        (
            before.ino(),
            before.mode(),
            before.mtime(),
            before.mtime_nsec()
        ),
        (after.ino(), after.mode(), after.mtime(), after.mtime_nsec())
    );
}

#[test]
fn old_or_unknown_location_index_versions_preserve_index_and_data() {
    for version in [1, 2, 999] {
        let temp = tempfile::tempdir().unwrap();
        let store = store(&temp);
        store
            .persist(&mut new_session(&store, "register index"))
            .unwrap();
        let id = plant_session(&store, now_ms() - 9 * TEST_DAY_MS, 64);
        let path = store.session_file(&id);
        let bytes = fs::read(&path).unwrap();
        let index = store
            .budget
            .path
            .join("00000000-0000-4000-8000-000000000000.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
        value["schema_version"] = serde_json::json!(version);
        let old = serde_json::to_vec(&value).unwrap();
        fs::write(&index, &old).unwrap();
        assert_eq!(
            store
                .enforce_task_budget(&ProviderSessionId::new(), 0)
                .unwrap_err()
                .code(),
            "io"
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(fs::read(&index).unwrap(), old);
    }
}

#[test]
fn readonly_initial_legacy_is_counted_after_switching_roots_but_never_evicted() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let root = workspace.join("project-sessions");
    fs::create_dir_all(&root).unwrap();
    let relative = workspace_store("project-sessions", &workspace).unwrap();
    relative
        .persist(&mut new_session(&relative, "seed modern scope"))
        .unwrap();
    let marker = root.join(".cosh-legacy-owner");
    fs::remove_file(&marker).unwrap();
    fs::remove_file(
        relative
            .budget
            .path
            .join("00000000-0000-4000-8000-000000000000.json"),
    )
    .unwrap();
    let id = ProviderSessionId::new();
    let path = root.join(format!("{id}.json"));
    let bytes = serde_json::to_vec(&vec![Message::user("protected readonly legacy")]).unwrap();
    fs::write(&path, &bytes).unwrap();
    set_mtime(&path, 1);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o500)).unwrap();
    let registration = relative.acquire_budget_lock();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    drop(registration.unwrap());
    assert!(
        !marker.exists(),
        "test requires an actual unprivileged readonly registration"
    );
    let absolute = workspace_store(
        temp.path().join("new-absolute").to_str().unwrap(),
        &workspace,
    )
    .unwrap()
    .with_task_budget(7, 1)
    .with_budget_namespace(relative.budget.path.clone());
    assert!(
        matches!(
            absolute.enforce_task_budget(&ProviderSessionId::new(), 0),
            Err(SessionError::BudgetExhausted { .. })
        ),
        "pending ownership must stay charged and non-evictable"
    );
    assert_eq!(
        fs::read(&path).unwrap(),
        bytes,
        "unverified marker cannot authorize deletion"
    );
}

#[test]
fn modern_budget_preserves_noncanonical_workspace_envelopes_in_matching_hash_scopes() {
    for workspace in [
        "relative-path",
        "",
        "/tmp/project/../other",
        "/tmp/project/./other",
        "/tmp//project",
        "/tmp/project/",
        "/tmp/project\0shadow",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let store = budgeted_store(&temp, 7, 1);
        let mut session = new_session(&store, "invalid ownership context");
        session.workspace_scope = workspace.to_string();
        session.updated_at_ms = 1;
        let scope =
            store.base_dir.parent().unwrap().join(
                &hex::encode(sha2::Sha256::digest(workspace.as_bytes()))[..SCOPE_DIR_NAME_LEN],
            );
        fs::create_dir_all(&scope).unwrap();
        let path = scope.join(format!("{}.json", session.session_id));
        let bytes = serde_json::to_vec(&session).unwrap();
        fs::write(&path, &bytes).unwrap();
        let before = fs::metadata(&path).unwrap();
        let result = store.enforce_task_budget(&ProviderSessionId::new(), 0);
        assert!(
            matches!(result, Err(SessionError::BudgetExhausted { .. })),
            "noncanonical ownership must stay charged and protected: {workspace:?} {result:?}"
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            bytes,
            "noncanonical workspace data must survive sweeping"
        );
        let after = fs::metadata(&path).unwrap();
        assert_eq!(
            (
                before.ino(),
                before.mode(),
                before.mtime(),
                before.mtime_nsec()
            ),
            (after.ino(), after.mode(), after.mtime(), after.mtime_nsec())
        );
    }
}

#[test]
fn budget_activity_probes_do_not_rescan_temporary_files_per_candidate() {
    use super::super::scoped::{reset_temporary_sweep_count, temporary_sweep_count};

    for capacity in [false, true] {
        for candidates in [8, 64] {
            let temp = tempfile::tempdir().unwrap();
            let store = budgeted_store(&temp, 7, if capacity { 1 } else { TASK_BUDGET_BYTES });
            let clock = if capacity { now_ms() } else { 1 };
            let ids: Vec<_> = (0..candidates)
                .map(|_| plant_session(&store, clock, 16))
                .collect();
            let stale = store.base_dir.join(".stale.tmp");
            fs::write(&stale, b"stale").unwrap();
            set_mtime(&stale, 1);
            let _budget = store.acquire_budget_lock().unwrap();
            reset_temporary_sweep_count();

            store
                .enforce_task_budget(&ProviderSessionId::new(), 0)
                .unwrap();

            assert!(ids.iter().all(|id| !store.session_file(id).exists()));
            assert_eq!(temporary_sweep_count(), 0, "budget activity probes must not rescan temporary files: capacity={capacity}, candidates={candidates}");
            assert!(stale.exists());
            let writer = ScopedStorage::new(store.base_dir.clone()).unwrap();
            writer.activity_directory().unwrap();
            assert_eq!(temporary_sweep_count(), 0);
            writer.directory(true).unwrap();
            writer.directory(true).unwrap();
            assert_eq!(temporary_sweep_count(), 1);
            assert!(!stale.exists());
        }
    }
}

mod redaction_boundaries;
