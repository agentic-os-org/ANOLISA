use super::*;

const CURRENT: &str = "/user.slice/user-1000.slice/session-2.scope";

fn ctx() -> AttributionContext {
    AttributionContext {
        current_cgroup: Some(CURRENT.into()),
        now_monotonic_ns: Some(1_000_000_000_000),
        host_qualified: true,
        boot_id: Some("boot-a".into()),
        source_verified: true,
    }
}

fn kill_line(monotonic: &str, victim: &str) -> String {
    format!(
        "[{monotonic}] Memory cgroup out of memory: Killed process 123 (worker) total-vm:100kB, constraint=CONSTRAINT_MEMCG, task_memcg={victim}, oom_memcg={victim}"
    )
}

fn signal(monotonic_secs: f64, victim: &str) -> OomSignal {
    OomSignal::from_line(1000, &kill_line(&monotonic_secs.to_string(), victim))
}

#[test]
fn same_domain_kill_attributes_with_monotonic_age() {
    let events = vec![signal(900.0, CURRENT)];
    let attributed = select_attributed(&events, &ctx(), 2000).unwrap();
    assert_eq!(attributed.scope, "current-cgroup");
    assert_eq!(attributed.age_seconds, 100);
    assert_eq!(attributed.pid, 123);
    assert!(attributed.event_identity.starts_with("uptime-ns:"));
}

#[test]
fn descendant_victim_attributes_but_ancestor_and_sibling_do_not() {
    let child = format!("{CURRENT}/child");
    assert!(select_attributed(&[signal(900.0, &child)], &ctx(), 2000).is_some());
    assert!(select_attributed(
        &[signal(900.0, "/user.slice/user-1000.slice")],
        &ctx(),
        2000
    )
    .is_none());
    assert!(select_attributed(
        &[signal(900.0, "/user.slice/user-1000.slice/session-3.scope")],
        &ctx(),
        2000
    )
    .is_none());
    assert!(
        select_attributed(&[signal(900.0, "/system.slice/sshd.service")], &ctx(), 2000).is_none()
    );
}

#[test]
fn global_kill_needs_host_qualification() {
    let line =
        "[900.0] Out of memory: Killed process 77 (global-victim) constraint=CONSTRAINT_NONE";
    let events = vec![OomSignal::from_line(1000, line)];
    assert_eq!(
        select_attributed(&events, &ctx(), 2000).unwrap().scope,
        "host"
    );
    let mut unqualified = ctx();
    unqualified.host_qualified = false;
    assert!(select_attributed(&events, &unqualified, 2000).is_none());
}

#[test]
fn invoked_only_and_reaper_only_lines_do_not_attribute() {
    let invoked = OomSignal::from_line(
        1000,
        "[900.0] worker invoked oom-killer: gfp_mask=0x100cca(GFP_HIGHUSER_MOVABLE), order=0, oom_memcg=/x",
    );
    assert!(select_attributed(&[invoked], &ctx(), 2000).is_none());
    let reaper = OomSignal::from_line(1000, "[901.0] oom_reaper: reaped process 123 (worker)");
    assert!(select_attributed(&[reaper], &ctx(), 2000).is_none());
}

#[test]
fn future_monotonic_event_is_excluded() {
    let events = vec![signal(1100.0, CURRENT)];
    assert!(select_attributed(&events, &ctx(), 2000).is_none());
}

#[test]
fn latest_attributable_event_wins_over_newer_foreign_event() {
    let events = vec![
        signal(800.0, CURRENT),
        signal(950.0, "/system.slice/other.service"),
    ];
    let attributed = select_attributed(&events, &ctx(), 2000).unwrap();
    assert_eq!(attributed.age_seconds, 200);
}

#[test]
fn cpuset_and_memory_policy_constraints_do_not_attribute() {
    for constraint in ["CONSTRAINT_CPUSET", "CONSTRAINT_MEMORY_POLICY"] {
        let line =
            format!("[900.0] Killed process 5 (x) constraint={constraint}, task_memcg={CURRENT}");
        let events = vec![OomSignal::from_line(1000, &line)];
        assert!(
            select_attributed(&events, &ctx(), 2000).is_none(),
            "{constraint}"
        );
    }
}

fn journal_entry(
    transport: &str,
    facility: &str,
    boot: &str,
    monotonic_usec: u64,
    message: &str,
) -> String {
    serde_json::json!({
        "_TRANSPORT": transport,
        "SYSLOG_FACILITY": facility,
        "_BOOT_ID": boot,
        "__MONOTONIC_TIMESTAMP": monotonic_usec.to_string(),
        "__REALTIME_TIMESTAMP": "1000000000000000",
        "MESSAGE": message,
    })
    .to_string()
}

#[test]
fn journal_verification_rejects_facility_transport_and_boot_spoofing() {
    let message = "Memory cgroup out of memory: Killed process 123 (worker) task_memcg=/user.slice/user-1000.slice/session-2.scope";
    let good = journal_entry("kernel", "0", "boot-a", 900_000_000, message);
    let (events, unclocked, _) = parse_journal_json_signals(&good, &ctx()).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(unclocked, 0);
    for bad in [
        journal_entry("user", "0", "boot-a", 900_000_000, message),
        journal_entry("kernel", "1", "boot-a", 900_000_000, message),
        journal_entry("kernel", "0", "boot-other", 900_000_000, message),
    ] {
        let (events, unclocked, _) = parse_journal_json_signals(&bad, &ctx()).unwrap();
        assert!(events.is_empty());
        assert_eq!(unclocked, 1);
    }
}

#[test]
fn journal_missing_boot_or_time_is_not_recent() {
    let message =
        "Killed process 123 (worker) task_memcg=/user.slice/user-1000.slice/session-2.scope";
    let mut entry: serde_json::Value = serde_json::from_str(&journal_entry(
        "kernel",
        "0",
        "boot-a",
        900_000_000,
        message,
    ))
    .unwrap();
    entry.as_object_mut().unwrap().remove("_BOOT_ID");
    let (events, _, _) = parse_journal_json_signals(&entry.to_string(), &ctx()).unwrap();
    assert!(events.is_empty());
    let mut entry: serde_json::Value = serde_json::from_str(&journal_entry(
        "kernel",
        "0",
        "boot-a",
        900_000_000,
        message,
    ))
    .unwrap();
    entry
        .as_object_mut()
        .unwrap()
        .remove("__REALTIME_TIMESTAMP");
    let (events, _, _) = parse_journal_json_signals(&entry.to_string(), &ctx()).unwrap();
    assert!(events.is_empty());
}

#[test]
fn journal_non_json_output_falls_back() {
    assert!(parse_journal_json_signals("1000.0 kernel: oom-kill:pid=1", &ctx()).is_none());
    assert!(parse_journal_json_signals("", &ctx()).is_none());
}

#[test]
fn journal_structured_monotonic_overrides_text_prefix() {
    let message = format!(
        "[5.0] Memory cgroup out of memory: Killed process 123 (worker) constraint=CONSTRAINT_MEMCG, task_memcg={CURRENT}"
    );
    let entry = journal_entry("kernel", "0", "boot-a", 900_000_000, &message);
    let (events, _, _) = parse_journal_json_signals(&entry, &ctx()).unwrap();
    assert_eq!(events[0].monotonic_ns, Some(900_000_000_000));
    let attributed = select_attributed(&events, &ctx(), 2000).unwrap();
    assert_eq!(attributed.age_seconds, 100);
}

#[test]
fn journal_byte_array_message_decodes() {
    let message = "Killed process 9 (x)";
    let bytes: Vec<u8> = message.bytes().collect();
    let entry = serde_json::json!({
        "_TRANSPORT": "kernel",
        "SYSLOG_FACILITY": "0",
        "_BOOT_ID": "boot-a",
        "__MONOTONIC_TIMESTAMP": "900000000",
        "__REALTIME_TIMESTAMP": "1000000000000000",
        "MESSAGE": bytes,
    })
    .to_string();
    let (events, _, _) = parse_journal_json_signals(&entry, &ctx()).unwrap();
    assert_eq!(events.len(), 1);
}
