use super::*;

const SHOW: &str = "Id=nginx.service\nLoadState=loaded\nActiveState=active\nSubState=running\nType=notify\nResult=success\nJob=\nTriggeredBy=\nStateChangeTimestampMonotonic=1000000\n\nId=redis.service\nLoadState=loaded\nActiveState=failed\nSubState=failed\nType=simple\nResult=exit-code\nJob=\nTriggeredBy=\nStateChangeTimestampMonotonic=2000000\n";

#[test]
fn show_batch_binds_canonical_id_blocks() {
    let entries = parse_systemctl_show(SHOW);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].id, "nginx.service");
    assert_eq!(entries[0].active_state, "active");
    assert_eq!(entries[0].state_change_us, Some(1_000_000));
    assert_eq!(entries[1].id, "redis.service");
    assert_eq!(entries[1].result, "exit-code");
}

#[test]
fn canonical_name_allowlist() {
    assert!(canonical_service_name("nginx.service"));
    assert!(canonical_service_name("user@1000.service"));
    assert!(!canonical_service_name("nginx"));
    assert!(!canonical_service_name("../evil.service"));
    assert!(!canonical_service_name("bad;name.service"));
    assert!(!canonical_service_name(""));
}

fn entry() -> ServiceShowEntry {
    ServiceShowEntry {
        id: "nginx.service".into(),
        load_state: "loaded".into(),
        active_state: "active".into(),
        sub_state: "running".into(),
        service_type: "notify".into(),
        result: "success".into(),
        job: String::new(),
        triggered_by: String::new(),
        state_change_us: Some(1_000_000),
    }
}

#[test]
fn persistent_qualification_gates() {
    let now = Some(10_000_000);
    let age = state_age_us(&entry(), now);
    assert_eq!(age, Some(9_000_000));
    assert!(persistent_qualified(&entry(), "persistent", age));
    assert!(!persistent_qualified(&entry(), "on-demand", age));
    let mut oneshot = entry();
    oneshot.service_type = "oneshot".into();
    assert!(!persistent_qualified(&oneshot, "persistent", age));
    let mut triggered = entry();
    triggered.triggered_by = "nginx.socket".into();
    assert!(!persistent_qualified(&triggered, "persistent", age));
    let mut job = entry();
    job.job = "123".into();
    assert!(!persistent_qualified(&job, "persistent", age));
    let mut not_loaded = entry();
    not_loaded.load_state = "not-found".into();
    assert!(!persistent_qualified(&not_loaded, "persistent", age));
}

#[test]
fn state_age_boundary_and_invalid_clock() {
    let mut e = entry();
    e.state_change_us = Some(5_000_000);
    let now = Some(10_000_000);
    assert!(persistent_qualified(
        &e,
        "persistent",
        state_age_us(&e, now)
    ));
    e.state_change_us = Some(5_000_001);
    assert!(!persistent_qualified(
        &e,
        "persistent",
        state_age_us(&e, now)
    ));
    e.state_change_us = Some(20_000_000);
    assert_eq!(state_age_us(&e, now), None);
    e.state_change_us = Some(0);
    assert_eq!(state_age_us(&e, now), None);
    e.state_change_us = None;
    assert_eq!(state_age_us(&e, now), None);
    assert_eq!(state_age_us(&entry(), None), None);
}

#[test]
fn legacy_status_mapping() {
    assert_eq!(legacy_status(&entry()), "active");
    let mut failed = entry();
    failed.active_state = "failed".into();
    assert_eq!(legacy_status(&failed), "failed");
    let mut inactive = entry();
    inactive.active_state = "inactive".into();
    assert_eq!(legacy_status(&inactive), "inactive");
}
