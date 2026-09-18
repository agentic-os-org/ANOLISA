#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use asc_agentsight_client::*;
use asc_policy_types::target::{PreparedApply, TargetBindingPlan};
use serde_json::{Value, json};

pub const BOOT_ID: &str = "11111111-2222-4333-8444-555555555555";
pub const ID7: &str = "d525d62c-2a3d-570b-9c25-d29c336c1d87";
pub const ID8: &str = "57f6ffc6-7960-5e22-8aef-404c2178c266";
pub const HEALTH: &[u8] =
    include_bytes!("../../../../fixtures/clients/agentsight/file-deletion/health.response.json");
pub const APPLY: &[u8] =
    include_bytes!("../../../../fixtures/clients/agentsight/file-deletion/apply.response.json");

pub fn plan(revision: u32) -> TargetBindingPlan {
    let mut value: Value = serde_json::from_str(include_str!(
        "../../../../fixtures/clients/agentsight/file-deletion/deployment-plan.json"
    ))
    .unwrap();
    value["source"]["bindingRevision"] = json!(revision);
    TargetBindingPlan {
        format: "agentsight.actplane.binding.v1".into(),
        content: serde_json::to_vec(&value).unwrap(),
    }
}

pub fn prepared(revision: u32) -> PreparedApply {
    serde_json::from_str(match revision {
        7 => {
            include_str!("../../../../fixtures/clients/agentsight/file-deletion/prepared-7.json")
        }
        8 => {
            include_str!("../../../../fixtures/clients/agentsight/file-deletion/prepared-8.json")
        }
        _ => panic!("unregistered fixture revision"),
    })
    .unwrap()
}

pub fn response(status: u16, body: &[u8]) -> AgentSightHttpResponse {
    AgentSightHttpResponse {
        status,
        body: body.to_vec(),
    }
}

pub fn applied(revision: u32) -> AgentSightHttpResponse {
    let mut value: Value = serde_json::from_slice(APPLY).unwrap();
    value["request"]["binding_id"] = json!(if revision == 7 { ID7 } else { ID8 });
    response(200, &serde_json::to_vec(&value).unwrap())
}

pub fn remote_error(status: u16, code: &str, retryable: bool) -> AgentSightHttpResponse {
    response(
        status,
        &serde_json::to_vec(
            &json!({"error":{"code":code,"retryable":retryable,"message":"private remote detail"}}),
        )
        .unwrap(),
    )
}

pub type WireResult = Result<AgentSightHttpResponse, AgentSightTransportError>;
type Hook = (usize, Box<dyn Fn() + Send + Sync>);

#[derive(Clone)]
pub struct Wire(Arc<Mutex<WireState>>);
struct WireState {
    responses: VecDeque<WireResult>,
    requests: Vec<AgentSightHttpRequest>,
    hook: Option<Hook>,
}

impl Wire {
    pub fn new(responses: impl IntoIterator<Item = WireResult>) -> Self {
        Self(Arc::new(Mutex::new(WireState {
            responses: responses.into_iter().collect(),
            requests: vec![],
            hook: None,
        })))
    }
    pub fn requests(&self) -> Vec<AgentSightHttpRequest> {
        self.0.lock().unwrap().requests.clone()
    }
    pub fn consumed(&self) {
        assert!(
            self.0.lock().unwrap().responses.is_empty(),
            "unconsumed HTTP response"
        );
    }
    pub fn after(&self, count: usize, hook: impl Fn() + Send + Sync + 'static) {
        self.0.lock().unwrap().hook = Some((count, Box::new(hook)));
    }
}

impl AgentSightTransport for Wire {
    fn send(&self, request: &AgentSightHttpRequest) -> WireResult {
        let mut state = self.0.lock().unwrap();
        state.requests.push(request.clone());
        if let Some((count, hook)) = &state.hook
            && *count == state.requests.len()
        {
            hook();
        }
        state
            .responses
            .pop_front()
            .expect("unexpected HTTP request")
    }
}

#[derive(Clone)]
pub struct Identity(pub Arc<Mutex<IdentityState>>);
pub struct IdentityState {
    pub start: Result<u64, ProcessIdentityError>,
    pub boot: Result<String, ProcessIdentityError>,
    pub reads: usize,
}

impl Default for Identity {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(IdentityState {
            start: Ok(987_654),
            boot: Ok(BOOT_ID.into()),
            reads: 0,
        })))
    }
}

impl ProcessIdentityResolver for Identity {
    fn process_start_time(&self, pid: i32) -> Result<u64, ProcessIdentityError> {
        assert_eq!(pid, 4242);
        let mut state = self.0.lock().unwrap();
        state.reads += 1;
        state.start
    }
    fn boot_id(&self) -> Result<String, ProcessIdentityError> {
        let mut state = self.0.lock().unwrap();
        state.reads += 1;
        state.boot.clone()
    }
}

pub fn path(id: &str) -> String {
    format!("/enforcement/bindings/{id}")
}
