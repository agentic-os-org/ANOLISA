use super::*;

impl PersonalRuntime {
    #[cfg(test)]
    pub(crate) fn open(
        configured_enabled: bool,
        root: impl AsRef<Path>,
        now_hour_bucket: u64,
    ) -> Result<Self, PersonalRuntimeError> {
        Self::open_with_environment(configured_enabled, None, root, now_hour_bucket)
    }

    pub(crate) fn open_with_environment(
        configured_enabled: bool,
        environment_override: Option<bool>,
        root: impl AsRef<Path>,
        now_hour_bucket: u64,
    ) -> Result<Self, PersonalRuntimeError> {
        let root = root.as_ref().to_path_buf();
        let store = PersonalStore::open(&root)?;
        let initialized = store.initialize(now_hour_bucket)?;
        let enabled = resolve_recommendations_enabled(
            environment_override,
            initialized.preferences.user_enabled,
            configured_enabled,
        );
        let state = if enabled {
            initialized
        } else {
            store.clear(now_hour_bucket)?
        };
        let epoch_key = store.epoch_key(&state.store_epoch)?;
        Ok(Self {
            enabled,
            configured_enabled,
            environment_override,
            accepting_records: enabled
                && state.preferences.notice_version_seen >= DISCLOSURE_VERSION,
            store: Some(store),
            state: Some(state),
            epoch_key: Some(epoch_key),
            session_scope_id: Some(random_hex(16)?),
            queue: VecDeque::new(),
            queue_bytes: 0,
            dropped_records: SourceCounts::default(),
            store_errors: 0,
            feedback_lifecycle: None,
        })
    }

    pub(crate) fn recover_with_preference(
        configured_enabled: bool,
        environment_override: Option<bool>,
        root: impl AsRef<Path>,
        user_enabled: bool,
        now_hour_bucket: u64,
    ) -> Result<Self, PersonalRuntimeError> {
        if user_enabled && environment_override == Some(false) {
            return Err(PersonalRuntimeError::Operation(
                "COSH_RECOMMENDATIONS_ENABLED=0 forces recommendations off".into(),
            ));
        }
        let root = root.as_ref();
        PersonalStore::open(root)?.recover_corrupt_state(user_enabled, now_hour_bucket)?;
        Self::open_with_environment(
            configured_enabled,
            environment_override,
            root,
            now_hour_bucket,
        )
    }

    pub(crate) fn spawn_writer(mut self) -> Result<PersonalRuntimeWriter, PersonalRuntimeError> {
        let enabled = self.enabled;
        let feedback_lifecycle = self.feedback_lifecycle.take();
        let runtime = Arc::new(Mutex::new(self));
        let (commands, receiver) = mpsc::sync_channel(8);
        let worker_runtime = Arc::clone(&runtime);
        let worker = thread::Builder::new()
            .name("cosh-recommendation-writer".to_string())
            .spawn(move || writer_loop(worker_runtime, receiver))
            .map_err(|error| {
                PersonalRuntimeError::Operation(format!("start recommendation writer: {error}"))
            })?;
        Ok(PersonalRuntimeWriter::new(
            runtime,
            commands,
            worker,
            enabled,
            feedback_lifecycle,
        ))
    }

    pub(crate) fn session_scope_id(&self) -> Option<&str> {
        self.session_scope_id.as_deref()
    }

    pub(crate) fn build_context(
        &self,
        host_identity: &str,
        cwd: &Path,
        repo_root: Option<&Path>,
        normalized_remote: Option<&str>,
        home: &Path,
    ) -> Option<ActivityContext> {
        let key = self.epoch_key.as_ref()?;
        Some(build_activity_context(
            key,
            host_identity,
            cwd,
            repo_root,
            normalized_remote,
            home,
        ))
    }

    pub(crate) fn snapshot(&self) -> Option<&RecommendationState> {
        self.state.as_ref()
    }

    pub(crate) fn cached_candidates(&self) -> &[CachedPromptCandidate] {
        self.state
            .as_ref()
            .map(|state| state.cache.candidates.as_slice())
            .unwrap_or_default()
    }

    pub(crate) fn status(&self) -> PersonalRuntimeStatus {
        PersonalRuntimeStatus {
            enabled: self.enabled,
            accepting_records: self.accepting_records,
            persisted_records: self
                .state
                .as_ref()
                .map_or(0, |state| state.journal.records.len()),
            queued_records: self.queue.len(),
            queued_bytes: self.queue_bytes,
            dropped_records: self.dropped_records,
            store_errors: self.store_errors,
            profile_generation: self
                .state
                .as_ref()
                .map_or(0, |state| state.profile.summary_generation),
            cached_candidates: self.cached_candidates().len(),
            last_summary_hour_bucket: self.state.as_ref().and_then(|state| {
                (state.profile.summary_generation > 0 && state.cache.profile_generation > 0)
                    .then_some(state.cache.generated_hour_bucket)
            }),
        }
    }
}
