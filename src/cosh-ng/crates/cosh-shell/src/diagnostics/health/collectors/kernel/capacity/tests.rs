use super::*;

fn ctx() -> AttributionContext {
    AttributionContext {
        current_cgroup: None,
        now_monotonic_ns: Some(2_000_000_000_000),
        host_qualified: true,
        boot_id: Some("boot-a".into()),
        source_verified: true,
    }
}

fn entry(message: &str, transport: &str, facility: &str, boot: &str, mono_us: u64) -> String {
    serde_json::json!({
        "MESSAGE": message,
        "_TRANSPORT": transport,
        "SYSLOG_FACILITY": facility,
        "_BOOT_ID": boot,
        "__REALTIME_TIMESTAMP": "1700000000000000",
        "__MONOTONIC_TIMESTAMP": mono_us.to_string(),
    })
    .to_string()
}

#[test]
fn line_matchers_are_exact() {
    assert!(is_vfs_filemax_line("vfs: file-max limit 1048576 reached"));
    assert!(!is_vfs_filemax_line("too many open files"));
    assert!(!is_vfs_filemax_line("vfs: file-max limit 1048576"));
    assert!(is_conntrack_full_line(
        "nf_conntrack: table full, dropping packet"
    ));
    assert!(!is_conntrack_full_line("nf_conntrack: table full"));
    assert_eq!(
        parse_filemax_limit("VFS: file-max limit 1048576 reached"),
        Some(1048576)
    );
    assert_eq!(parse_filemax_limit("VFS: file-max limit abc reached"), None);
}

#[test]
fn verified_journal_events_use_monotonic_age() {
    let content = format!(
        "{}\n{}\n",
        entry(
            "VFS: file-max limit 999 reached",
            "kernel",
            "0",
            "boot-a",
            1_999_000_000
        ),
        entry(
            "nf_conntrack: table full, dropping packet",
            "kernel",
            "0",
            "boot-a",
            1_998_000_000
        )
    );
    let events = parse_journal_capacity_events(&content, &ctx(), 1_700_000_100);
    assert_eq!(events.vfs_filemax.len(), 1);
    assert_eq!(events.vfs_filemax[0].age_seconds, 1);
    assert_eq!(events.vfs_filemax[0].limit, Some(999));
    assert_eq!(events.vfs_filemax[0].identity, "uptime-ns:1999000000000");
    assert_eq!(events.conntrack_full.len(), 1);
    assert_eq!(events.conntrack_full[0].age_seconds, 2);
    assert_eq!(events.unverified, 0);
}

#[test]
fn forged_transport_facility_or_boot_are_excluded() {
    let mut count = 0;
    for (transport, facility, boot) in [
        ("user", "0", "boot-a"),
        ("kernel", "1", "boot-a"),
        ("kernel", "0", "boot-b"),
    ] {
        let content = entry(
            "VFS: file-max limit 5 reached",
            transport,
            facility,
            boot,
            1_999_000_000,
        );
        let events = parse_journal_capacity_events(&content, &ctx(), 1_700_000_100);
        assert!(events.vfs_filemax.is_empty());
        count += events.unverified;
    }
    assert_eq!(count, 3);
}

#[test]
fn missing_timestamp_is_unverified() {
    let content = serde_json::json!({
        "MESSAGE": "VFS: file-max limit 5 reached",
        "_TRANSPORT": "kernel",
        "SYSLOG_FACILITY": "0",
        "_BOOT_ID": "boot-a",
    })
    .to_string();
    let events = parse_journal_capacity_events(&content, &ctx(), 1_700_000_100);
    assert!(events.vfs_filemax.is_empty());
    assert_eq!(events.unverified, 1);
}

#[test]
fn epoch_fallback_without_monotonic() {
    let mut ctx = ctx();
    ctx.now_monotonic_ns = None;
    let content = entry(
        "nf_conntrack: table full, dropping packet",
        "kernel",
        "0",
        "boot-a",
        0,
    );
    let events = parse_journal_capacity_events(&content, &ctx, 1_700_000_100);
    assert_eq!(events.conntrack_full[0].age_seconds, 100);
    assert_eq!(events.conntrack_full[0].identity, "epoch:1700000000");
}

#[test]
fn text_fallback_counts_without_time_eligibility() {
    let content = "[123.456] VFS: file-max limit 77 reached\n[124.0] nf_conntrack: table full, dropping packet\n";
    let events = parse_text_capacity_events(content, "dmesg");
    assert_eq!(events.vfs_filemax.len(), 1);
    assert_eq!(events.vfs_filemax[0].limit, Some(77));
    assert_eq!(events.conntrack_full.len(), 1);
    assert_eq!(events.source, "dmesg");
}
