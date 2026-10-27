//! Coverage for the early best-effort SSL attach call sites.
//!
//! These drive the production `handle_procmon_event` and `process_event`
//! handlers through the replay fixture, so the new call sites are exercised
//! rather than only the `best_effort_attach` helper.

use super::capture_runtime_tests::RuntimeFixture;
use super::*;

fn self_pid() -> u32 {
    std::process::id()
}

#[test]
fn procmon_exec_attaches_process_without_a_known_agent_name() {
    let mut fixture = RuntimeFixture::new(0);
    let pid = self_pid();

    fixture.handle_procmon_event(&crate::probes::procmon::Event::Exec {
        pid,
        tid: pid,
        ppid: 0,
        uid: 0,
        timestamp_ns: 0,
        comm: "definitely-not-a-known-agent".to_string(),
    });

    assert!(
        fixture.probes.attached_pids().contains(&(pid as i32)),
        "unknown exec must still get a best-effort SSL attach: {:?}",
        fixture.probes.attached_pids()
    );
}

#[test]
fn dns_rule_attaches_process_and_registers_the_resource_target() {
    let mut fixture = RuntimeFixture::new(0);
    let pid = self_pid();
    fixture.scanner = AgentScanner::from_rules(
        &[],
        &[crate::config::HttpsRule {
            pattern: "*.example.com".to_string(),
        }],
    );

    fixture.process_event(Event::UdpDns(crate::probes::udpdns::UdpDnsEvent {
        pid,
        tid: pid,
        uid: 0,
        timestamp_ns: 0,
        comm: "definitely-not-a-known-agent".to_string(),
        domain: "api.example.com".to_string(),
    }));

    assert!(
        fixture.probes.attached_pids().contains(&(pid as i32)),
        "domain-rule hit must attach the process: {:?}",
        fixture.probes.attached_pids()
    );
    assert!(
        fixture
            .resource_targets
            .read()
            .expect("resource targets")
            .contains_key(&pid),
        "agent name resolution must not be gated on attach success"
    );
}

#[derive(Default)]
struct RecordingProbe {
    attached: Vec<i32>,
}

impl SslProbe for RecordingProbe {
    fn attach_ssl(&mut self, pid: i32) {
        self.attached.push(pid);
    }
}

#[test]
fn unknown_agent_name_attaches_with_the_process_pid() {
    let mut probe = RecordingProbe::default();
    AgentSight::attach_if_agent_name_unknown(&mut probe, None, 4242);
    assert_eq!(probe.attached, vec![4242]);
}

#[test]
fn known_agent_name_does_not_re_attach() {
    let mut probe = RecordingProbe::default();
    AgentSight::attach_if_agent_name_unknown(&mut probe, Some("claude-code"), 4242);
    assert!(
        probe.attached.is_empty(),
        "a classified process is attached through the agent path: {:?}",
        probe.attached
    );
}

#[test]
fn procmon_exec_for_a_known_agent_does_not_double_attach() {
    let mut fixture = RuntimeFixture::new(0);
    let pid = self_pid();
    // Match our own cmdline so the scanner classifies this process as an agent.
    fixture.scanner = AgentScanner::from_rules(
        &[crate::config::CmdlineRule {
            patterns: vec!["*".to_string()],
            agent_name: Some("test-agent".to_string()),
            allow: true,
        }],
        &[],
    );

    fixture.handle_procmon_event(&crate::probes::procmon::Event::Exec {
        pid,
        tid: pid,
        ppid: 0,
        uid: 0,
        timestamp_ns: 0,
        comm: "test-agent".to_string(),
    });

    let attached = fixture.probes.attached_pids();
    assert_eq!(
        attached,
        vec![pid as i32],
        "a classified agent must be attached exactly once, by the agent path"
    );
    assert!(fixture.pid_agent_name_cache.peek(&pid).is_some());
}
