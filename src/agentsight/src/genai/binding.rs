//! Correlates GenAI calls with active enforcement bindings.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use agentsight_enforcement_protocol::{Binding, BindingState};
use uuid::Uuid;

use super::semantic::GenAISemanticEvent;
use crate::enforcement::EnforcementClient;
use crate::utils::procfs;

const REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const MAX_STALE_AGE: Duration = Duration::from_secs(90);
const REQUEST_TIMEOUT: Duration = Duration::from_millis(100);
const BINDING_ID_METADATA_KEY: &str = "binding_id";

trait BindingSource: Send {
    fn bindings(&self) -> Result<Vec<Binding>, String>;
}

impl BindingSource for EnforcementClient {
    fn bindings(&self) -> Result<Vec<Binding>, String> {
        EnforcementClient::bindings(self).map_err(|error| error.to_string())
    }
}

trait ProcessIdentitySource: Send {
    fn start_time(&self, pid: i32) -> Option<u64>;
}

struct ProcfsIdentitySource;

impl ProcessIdentitySource for ProcfsIdentitySource {
    fn start_time(&self, pid: i32) -> Option<u64> {
        if pid <= 0 {
            return None;
        }
        let stat = fs::read_to_string(procfs::proc_pid_entry(pid, "stat")).ok()?;
        parse_process_start_time(&stat)
    }
}

fn parse_process_start_time(stat: &str) -> Option<u64> {
    let command_end = stat.rfind(')')?;
    stat.get(command_end + 1..)?
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

#[derive(Clone, Copy)]
struct CachedBinding {
    binding_id: Uuid,
    process_start_time: u64,
}

/// Adds an enforcement binding id to LLM calls whose process identity matches a binding root.
pub(crate) struct BindingInjector {
    source: Box<dyn BindingSource>,
    process_identity: Box<dyn ProcessIdentitySource>,
    pid_bindings: HashMap<i32, CachedBinding>,
    last_attempt: Option<Instant>,
    last_success: Option<Instant>,
}

impl BindingInjector {
    /// Creates an injector backed by the local enforcer socket without performing I/O.
    pub(crate) fn new(socket_path: impl AsRef<Path>) -> Self {
        Self::with_sources(
            Box::new(EnforcementClient::new(socket_path).with_timeout(REQUEST_TIMEOUT)),
            Box::new(ProcfsIdentitySource),
        )
    }

    fn with_sources(
        source: Box<dyn BindingSource>,
        process_identity: Box<dyn ProcessIdentitySource>,
    ) -> Self {
        Self {
            source,
            process_identity,
            pid_bindings: HashMap::new(),
            last_attempt: None,
            last_success: None,
        }
    }

    /// Enriches every LLM call before it enters any immediate or deferred export path.
    pub(crate) fn inject(&mut self, events: &mut [GenAISemanticEvent]) {
        if !events
            .iter()
            .any(|event| matches!(event, GenAISemanticEvent::LLMCall(_)))
        {
            return;
        }
        self.refresh();
        for event in events {
            let GenAISemanticEvent::LLMCall(call) = event else {
                continue;
            };
            let Some(binding) = self.pid_bindings.get(&call.pid).copied() else {
                continue;
            };
            if self.process_identity.start_time(call.pid) != Some(binding.process_start_time) {
                continue;
            }
            call.metadata.insert(
                BINDING_ID_METADATA_KEY.to_string(),
                binding.binding_id.to_string(),
            );
        }
    }

    fn refresh(&mut self) {
        if self
            .last_attempt
            .is_some_and(|last| last.elapsed() < REFRESH_INTERVAL)
        {
            return;
        }
        let now = Instant::now();
        self.last_attempt = Some(now);
        match self.source.bindings() {
            Ok(bindings) => {
                self.replace_snapshot(bindings);
                self.last_success = Some(now);
            }
            Err(error) => {
                let snapshot_expired = match self.last_success {
                    Some(last) => last.elapsed() >= MAX_STALE_AGE,
                    None => true,
                };
                if snapshot_expired {
                    self.pid_bindings.clear();
                }
                log::debug!("binding cache refresh from enforcer failed: {error}");
            }
        }
    }

    fn replace_snapshot(&mut self, bindings: Vec<Binding>) {
        let mut snapshot = HashMap::new();
        let mut ambiguous = HashSet::new();
        for binding in bindings {
            let request = binding.request;
            if binding.state != BindingState::Enforced
                || request.root_pid <= 1
                || ambiguous.contains(&request.root_pid)
            {
                continue;
            }
            let value = CachedBinding {
                binding_id: request.binding_id,
                process_start_time: request.process_start_time,
            };
            if snapshot.insert(request.root_pid, value).is_some() {
                snapshot.remove(&request.root_pid);
                ambiguous.insert(request.root_pid);
            }
        }
        self.pid_bindings = snapshot;
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use agentsight_enforcement_protocol::{ApplyPolicy, PolicyMode};

    use super::*;
    use crate::genai::semantic::{LLMCall, LLMRequest};

    struct FakeSource {
        responses: RefCell<VecDeque<Result<Vec<Binding>, String>>>,
    }

    impl FakeSource {
        fn new(responses: Vec<Result<Vec<Binding>, String>>) -> Self {
            Self {
                responses: RefCell::new(responses.into()),
            }
        }
    }

    impl BindingSource for FakeSource {
        fn bindings(&self) -> Result<Vec<Binding>, String> {
            self.responses
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| Ok(Vec::new()))
        }
    }

    struct FakeIdentitySource(HashMap<i32, u64>);

    impl ProcessIdentitySource for FakeIdentitySource {
        fn start_time(&self, pid: i32) -> Option<u64> {
            self.0.get(&pid).copied()
        }
    }

    fn binding(pid: i32, start_time: u64, binding_id: Uuid) -> Binding {
        Binding {
            request: ApplyPolicy {
                binding_id,
                agent_id: "agent".to_string(),
                session_id: None,
                root_pid: pid,
                process_start_time: start_time,
                policy_id: "policy".to_string(),
                policy_revision: "1".to_string(),
                policy_dsl: "source AGENT = exec \"**\"".to_string(),
                policy_mode: Some(PolicyMode::Audit),
            },
            state: BindingState::Enforced,
            message: None,
            domain_id: Some(1),
        }
    }

    fn injector(
        responses: Vec<Result<Vec<Binding>, String>>,
        identities: &[(i32, u64)],
    ) -> BindingInjector {
        BindingInjector::with_sources(
            Box::new(FakeSource::new(responses)),
            Box::new(FakeIdentitySource(identities.iter().copied().collect())),
        )
    }

    fn llm_event(pid: i32) -> GenAISemanticEvent {
        GenAISemanticEvent::LLMCall(LLMCall::new(
            "call".to_string(),
            1,
            "provider".to_string(),
            "model".to_string(),
            LLMRequest {
                messages: Vec::new(),
                temperature: None,
                max_tokens: None,
                frequency_penalty: None,
                presence_penalty: None,
                top_p: None,
                top_k: None,
                seed: None,
                stop_sequences: None,
                stream: false,
                tools: None,
                raw_body: None,
            },
            pid,
            "agent".to_string(),
        ))
    }

    fn binding_metadata(event: &GenAISemanticEvent) -> Option<&str> {
        let GenAISemanticEvent::LLMCall(call) = event else {
            return None;
        };
        call.metadata
            .get(BINDING_ID_METADATA_KEY)
            .map(String::as_str)
    }

    #[test]
    fn proc_stat_parser_handles_spaces_and_parentheses_in_command() {
        let fields = (3..=52).map(|field| field.to_string()).collect::<Vec<_>>();
        let stat = format!("42 (agent (worker)) {}", fields.join(" "));

        assert_eq!(parse_process_start_time(&stat), Some(22));
    }

    #[test]
    fn first_injection_matches_the_full_process_identity() {
        let id = Uuid::new_v4();
        let mut injector = injector(vec![Ok(vec![binding(42, 7, id)])], &[(42, 7), (43, 7)]);
        let mut events = vec![llm_event(42), llm_event(43)];

        injector.inject(&mut events);

        let expected = id.to_string();
        assert_eq!(binding_metadata(&events[0]), Some(expected.as_str()));
        assert_eq!(binding_metadata(&events[1]), None);
    }

    #[test]
    fn pid_reuse_does_not_inherit_a_binding() {
        let id = Uuid::new_v4();
        let mut injector = injector(vec![Ok(vec![binding(42, 7, id)])], &[(42, 8)]);
        let mut events = vec![llm_event(42)];

        injector.inject(&mut events);

        assert_eq!(binding_metadata(&events[0]), None);
    }

    #[test]
    fn successful_refresh_replaces_removed_bindings() {
        let old_id = Uuid::new_v4();
        let new_id = Uuid::new_v4();
        let mut injector = injector(
            vec![
                Ok(vec![binding(42, 7, old_id)]),
                Ok(vec![binding(43, 8, new_id)]),
            ],
            &[(42, 7), (43, 8)],
        );
        injector.inject(&mut [llm_event(42)]);

        injector.last_attempt = None;
        let mut events = vec![llm_event(42), llm_event(43)];
        injector.inject(&mut events);

        let expected = new_id.to_string();
        assert_eq!(binding_metadata(&events[0]), None);
        assert_eq!(binding_metadata(&events[1]), Some(expected.as_str()));
    }

    #[test]
    fn failed_refresh_keeps_a_recent_successful_snapshot() {
        let id = Uuid::new_v4();
        let mut injector = injector(
            vec![Ok(vec![binding(42, 7, id)]), Err("offline".to_string())],
            &[(42, 7)],
        );
        injector.inject(&mut [llm_event(42)]);

        injector.last_attempt = None;
        let mut events = vec![llm_event(42)];
        injector.inject(&mut events);

        let expected = id.to_string();
        assert_eq!(binding_metadata(&events[0]), Some(expected.as_str()));
    }

    #[test]
    fn failed_refresh_clears_an_expired_snapshot() {
        let id = Uuid::new_v4();
        let mut injector = injector(
            vec![Ok(vec![binding(42, 7, id)]), Err("offline".to_string())],
            &[(42, 7)],
        );
        injector.inject(&mut [llm_event(42)]);

        injector.last_attempt = None;
        injector.last_success = Some(Instant::now() - MAX_STALE_AGE);
        let mut events = vec![llm_event(42)];
        injector.inject(&mut events);

        assert_eq!(binding_metadata(&events[0]), None);
    }

    #[test]
    fn ambiguous_root_pid_is_not_correlated() {
        let mut injector = injector(
            vec![Ok(vec![
                binding(42, 7, Uuid::new_v4()),
                binding(42, 7, Uuid::new_v4()),
            ])],
            &[(42, 7)],
        );
        let mut events = vec![llm_event(42)];

        injector.inject(&mut events);

        assert_eq!(binding_metadata(&events[0]), None);
    }
}
