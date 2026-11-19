use super::*;

fn ctx() -> AttributionContext {
    AttributionContext {
        current_cgroup: None,
        now_monotonic_ns: Some(1_100_000_000_000),
        host_qualified: true,
        boot_id: Some("boot-a".into()),
        source_verified: true,
    }
}

#[test]
fn hung_task_requires_complete_format_and_positive_n() {
    assert!(classify_host_line("INFO: task bash:1234 blocked for more than 120 seconds").is_some());
    assert!(classify_host_line("INFO: task bash:1234 blocked for more than 0 seconds").is_none());
    assert!(classify_host_line("INFO: task running").is_none());
    assert!(classify_host_line("blocked for more than 120 seconds").is_none());
    let event = build_event(
        HostEventKind::HungTask,
        "INFO: task bash:1234 blocked for more than 120 seconds",
        1000,
        Some(1_000_000_000_000),
        true,
    );
    assert_eq!(event.process.as_deref(), Some("bash"));
    assert_eq!(event.pid, Some(1234));
    assert_eq!(event.seconds, Some(120));
    assert_eq!(event.age_seconds(&ctx(), 1100), 100);
}

#[test]
fn soft_lockup_requires_cpu_and_duration() {
    assert!(
        classify_host_line("watchdog: BUG: soft lockup - CPU#3 stuck for 22s! [bash:1234]")
            .is_some()
    );
    assert!(classify_host_line("watchdog: soft lockup detected").is_none());
    assert!(classify_host_line("CPU#3 stuck for 22s").is_none());
    let event = build_event(
        HostEventKind::SoftLockup,
        "watchdog: BUG: soft lockup - CPU#3 stuck for 22s! [bash:1234]",
        1000,
        None,
        true,
    );
    assert_eq!(event.cpu, Some(3));
    assert_eq!(event.seconds, Some(22));
}

#[test]
fn segfault_and_netdev_parse_identity() {
    let event = build_event(
        HostEventKind::Segfault,
        "myapp[4321]: segfault at 0 ip 00007f sp 00007ffe error 4 in libc",
        1000,
        None,
        true,
    );
    assert_eq!(event.process.as_deref(), Some("myapp"));
    assert_eq!(event.pid, Some(4321));
    let event = build_event(
        HostEventKind::NetdevTimeout,
        "NETDEV WATCHDOG: eth0 (e1000): transmit queue 0 timed out",
        1000,
        None,
        true,
    );
    assert_eq!(event.interface.as_deref(), Some("eth0"));
    assert!(classify_host_line("NETDEV WATCHDOG: eth0 transmit queue 0").is_none());
}

#[test]
fn hardware_categories_are_decoded_conservatively() {
    assert_eq!(
        hardware_category("EDAC MC0: 1 CE memory read error"),
        Some("corrected")
    );
    assert_eq!(
        hardware_category("mce: [Hardware Error]: Machine check events logged"),
        None
    );
    assert_eq!(
        hardware_category("mce: Uncorrected nonfatal error"),
        Some("uncorrected-nonfatal")
    );
    assert_eq!(
        hardware_category("mce: Fatal machine check"),
        Some("fatal-candidate")
    );
}

#[test]
fn journal_verification_excludes_forged_entries() {
    let entry = serde_json::json!({
        "MESSAGE": "INFO: task bash:1234 blocked for more than 120 seconds",
        "_TRANSPORT": "kernel",
        "SYSLOG_FACILITY": "0",
        "_BOOT_ID": "boot-a",
        "__REALTIME_TIMESTAMP": "1700000000000000",
        "__MONOTONIC_TIMESTAMP": "1099000000",
    });
    let events = parse_journal_host_events(&entry.to_string(), &ctx());
    assert_eq!(events.events.len(), 1);
    assert_eq!(events.events[0].age_seconds(&ctx(), 1100), 1);
    let forged = serde_json::json!({
        "MESSAGE": "INFO: task bash:1234 blocked for more than 120 seconds",
        "_TRANSPORT": "user",
        "SYSLOG_FACILITY": "0",
        "_BOOT_ID": "boot-a",
        "__REALTIME_TIMESTAMP": "1700000000000000",
    });
    let events = parse_journal_host_events(&forged.to_string(), &ctx());
    assert!(events.events.is_empty());
    assert_eq!(events.unverified, 1);
}

#[test]
fn text_fallback_stays_unverified() {
    let events = parse_text_host_events(
        "INFO: task bash:1234 blocked for more than 120 seconds",
        "dmesg",
    );
    assert_eq!(events.events.len(), 1);
    assert!(!events.events[0].verified);
}
