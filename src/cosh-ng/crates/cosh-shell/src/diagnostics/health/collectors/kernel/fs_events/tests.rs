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

fn line(kind: FsEventKind, device: &str, epoch: u64) -> FsLine {
    FsLine {
        kind,
        device: device.to_string(),
        epoch,
        monotonic_ns: Some(epoch * 1_000_000_000),
        operation: None,
        verified: true,
    }
}

fn lines_with(events: Vec<FsLine>, context: Vec<FsLine>) -> FsLines {
    FsLines {
        events,
        context,
        source: "journalctl",
        ..FsLines::default()
    }
}

#[test]
fn classifies_exact_kernel_formats() {
    assert!(matches!(
        classify_fs_line("EXT4-fs error (device sda1): ext4_find_entry:1455: inode #2"),
        Some((FsEventKind::Ext4Error, device, None)) if device == "sda1"
    ));
    assert!(matches!(
        classify_fs_line("EXT4-fs (sda1): Remounting filesystem read-only"),
        Some((FsEventKind::Ext4ForcedRo, device, None)) if device == "sda1"
    ));
    assert!(matches!(
        classify_fs_line("JBD2: Aborting journal on device sda1-8"),
        Some((FsEventKind::JournalAbort, device, None)) if device == "sda1-8"
    ));
    assert!(matches!(
        classify_fs_line("JBD2: Detected IO errors while flushing file data on sda1-8"),
        Some((FsEventKind::JournalFlushError, device, None)) if device == "sda1-8"
    ));
    assert!(matches!(
        classify_fs_line("XFS (sda1): Corruption detected in xfs_trans_cancel"),
        Some((FsEventKind::XfsCause, device, None)) if device == "sda1"
    ));
    assert!(matches!(
        classify_fs_line("XFS (sda1): Filesystem has been shut down"),
        Some((FsEventKind::XfsShutdown, device, None)) if device == "sda1"
    ));
    assert!(matches!(
        classify_fs_line("XFS (sda1): User initiated shutdown received"),
        Some((FsEventKind::XfsUserShutdown, device, None)) if device == "sda1"
    ));
    assert!(matches!(
        classify_fs_line("blk_update_request: I/O error, dev sda, sector 42 op 0x0:(READ) flags 0x0"),
        Some((FsEventKind::BlockIoError, device, Some("READ"))) if device == "sda"
    ));
    assert!(matches!(
        classify_fs_line(
            "blk_update_request: I/O error, dev sda, sector 42 op 0x1:(WRITE) flags 0x0"
        ),
        Some((FsEventKind::BlockIoError, _, Some("WRITE")))
    ));
    assert!(matches!(
        classify_fs_line(
            "blk_update_request: I/O error, dev sda, sector 42 op 0x3:(DISCARD) flags 0x0"
        ),
        Some((FsEventKind::BlockIoError, _, None))
    ));
    assert!(
        classify_fs_line("EXT4-fs (sda1): mounted filesystem with ordered data mode").is_none()
    );
    assert!(classify_fs_line("Too many open files").is_none());
}

#[test]
fn journal_base_device_only_strips_internal_suffix() {
    assert_eq!(journal_base_device("sda1-8"), Some("sda1".into()));
    assert_eq!(journal_base_device("dm-0"), None);
    assert_eq!(journal_base_device("sdb1"), None);
}

#[test]
fn ext4_forced_ro_requires_same_device_error_anchor() {
    let lines = lines_with(
        vec![
            line(FsEventKind::Ext4Error, "sda1", 1000),
            line(FsEventKind::Ext4ForcedRo, "sda1", 1002),
        ],
        vec![],
    );
    let chains = build_chains(&lines, &ctx(), 1100);
    assert_eq!(chains.len(), 1);
    assert_eq!(chains[0].mechanism, "ext4-forced-ro");
    assert!(chains[0].eligible);
    assert_eq!(chains[0].age_seconds, 100);

    let lines = lines_with(vec![line(FsEventKind::Ext4ForcedRo, "sda1", 1000)], vec![]);
    let chains = build_chains(&lines, &ctx(), 1100);
    assert!(!chains[0].eligible);
    assert_eq!(chains[0].degraded_reason, Some("no-error-anchor"));

    let lines = lines_with(
        vec![
            line(FsEventKind::Ext4Error, "sdb1", 1000),
            line(FsEventKind::Ext4ForcedRo, "sda1", 1002),
        ],
        vec![],
    );
    let chains = build_chains(&lines, &ctx(), 1100);
    assert!(chains
        .iter()
        .any(|c| c.mechanism == "ext4-forced-ro" && !c.eligible));

    let lines = lines_with(
        vec![
            line(FsEventKind::Ext4Error, "sda1", 1000),
            line(FsEventKind::Ext4ForcedRo, "sda1", 1006),
        ],
        vec![],
    );
    let chains = build_chains(&lines, &ctx(), 1100);
    assert!(chains
        .iter()
        .any(|c| c.mechanism == "ext4-forced-ro" && !c.eligible));
    assert!(chains
        .iter()
        .any(|c| c.mechanism == "ext4-error" && c.eligible));
}

#[test]
fn journal_abort_needs_cause_and_excludes_admin() {
    let cause = line(FsEventKind::JournalCauseIo, "sda1-8", 1000);
    let abort = line(FsEventKind::JournalAbort, "sda1-8", 1002);
    let lines = lines_with(vec![abort.clone()], vec![cause.clone()]);
    let chains = build_chains(&lines, &ctx(), 1100);
    assert_eq!(chains.len(), 1);
    assert!(chains[0].eligible);
    assert_eq!(chains[0].fs_device, "sda1");

    let lines = lines_with(vec![abort.clone()], vec![]);
    let chains = build_chains(&lines, &ctx(), 1100);
    assert_eq!(chains[0].degraded_reason, Some("missing-cause"));

    let admin = line(FsEventKind::JournalAdminShutdown, "sda1-8", 1001);
    let lines = lines_with(vec![abort.clone()], vec![cause.clone(), admin]);
    let chains = build_chains(&lines, &ctx(), 1100);
    assert_eq!(chains[0].degraded_reason, Some("admin-excluded"));

    let external = line(FsEventKind::JournalAbort, "sdb1", 1002);
    let external_cause = line(FsEventKind::JournalCauseIo, "sdb1", 1000);
    let lines = lines_with(vec![external], vec![external_cause]);
    let chains = build_chains(&lines, &ctx(), 1100);
    assert_eq!(chains[0].degraded_reason, Some("journal-mapping-unproven"));

    let mut lines = lines_with(vec![abort], vec![cause]);
    lines.dropped_context = 1;
    let chains = build_chains(&lines, &ctx(), 1100);
    assert_eq!(chains[0].degraded_reason, Some("context-incomplete"));
}

#[test]
fn flush_error_is_independent_warning_chain() {
    let lines = lines_with(
        vec![line(FsEventKind::JournalFlushError, "sda1-8", 1000)],
        vec![],
    );
    let chains = build_chains(&lines, &ctx(), 1100);
    assert_eq!(chains[0].mechanism, "jbd2-flush");
    assert!(chains[0].eligible);
}

#[test]
fn xfs_shutdown_needs_cause_chain_and_excludes_user() {
    let cause = line(FsEventKind::XfsCause, "sda1", 1000);
    let shutdown = line(FsEventKind::XfsShutdown, "sda1", 1003);
    let lines = lines_with(vec![shutdown.clone()], vec![cause.clone()]);
    let chains = build_chains(&lines, &ctx(), 1100);
    assert!(chains[0].eligible);
    assert_eq!(chains[0].age_seconds, 100);

    let lines = lines_with(vec![shutdown.clone()], vec![]);
    assert_eq!(
        build_chains(&lines, &ctx(), 1100)[0].degraded_reason,
        Some("missing-cause")
    );

    let user = line(FsEventKind::XfsUserShutdown, "sda1", 1001);
    let lines = lines_with(vec![shutdown.clone()], vec![cause.clone(), user]);
    assert_eq!(
        build_chains(&lines, &ctx(), 1100)[0].degraded_reason,
        Some("admin-excluded")
    );

    let unmount = line(FsEventKind::XfsUnmount, "sda1", 1004);
    let lines = lines_with(vec![shutdown.clone()], vec![cause, unmount]);
    assert_eq!(
        build_chains(&lines, &ctx(), 1100)[0].degraded_reason,
        Some("admin-excluded")
    );

    let late = line(FsEventKind::XfsCause, "sda1", 990);
    let lines = lines_with(vec![shutdown], vec![late]);
    assert_eq!(
        build_chains(&lines, &ctx(), 1100)[0].degraded_reason,
        Some("missing-cause")
    );
}

#[test]
fn block_io_only_read_write_and_text_is_unverified() {
    let mut read = line(FsEventKind::BlockIoError, "sda", 1000);
    read.operation = Some("READ");
    let mut discard = line(FsEventKind::BlockIoError, "sda", 1000);
    discard.operation = None;
    let lines = lines_with(vec![read, discard], vec![]);
    let chains = build_chains(&lines, &ctx(), 1100);
    assert!(chains
        .iter()
        .any(|c| c.eligible && c.operation == Some("READ")));
    assert!(chains
        .iter()
        .any(|c| c.degraded_reason == Some("non-read-write")));

    let text = parse_text_fs_lines(
        "blk_update_request: I/O error, dev sda, sector 1 op 0x0:(READ)",
        "dmesg",
        &mut ContextBudget::new(),
    );
    let chains = build_chains(&text, &ctx(), 1100);
    assert_eq!(chains[0].degraded_reason, Some("source-unverified"));
}

#[test]
fn quotas_drop_excess_events_and_context() {
    let mut events = Vec::new();
    for index in 0..(FS_EVENT_QUOTA + 5) {
        events.push(line(FsEventKind::Ext4Error, "sda1", 1000 + index as u64));
    }
    let mut context = Vec::new();
    for index in 0..(FS_CONTEXT_QUOTA + 3) {
        context.push(line(FsEventKind::XfsCause, "sda1", 1000 + index as u64));
    }
    let mut lines = FsLines::default();
    let mut budget = ContextBudget::new();
    for event in events {
        push_line(&mut lines, event, &mut budget);
    }
    for cause in context {
        push_line(&mut lines, cause, &mut budget);
    }
    assert_eq!(lines.events.len(), FS_EVENT_QUOTA);
    assert_eq!(lines.context.len(), FS_CONTEXT_QUOTA);
    assert_eq!(lines.dropped_events, 5);
    assert_eq!(budget.dropped, 3);
}

#[test]
fn context_budget_is_joint_across_mechanisms() {
    let mut lines = FsLines::default();
    let mut budget = ContextBudget::new();
    for index in 0..5 {
        push_line(
            &mut lines,
            line(FsEventKind::XfsCause, "sda1", 1000 + index),
            &mut budget,
        );
        push_line(
            &mut lines,
            line(FsEventKind::JournalCauseIo, "sda1-8", 1000 + index),
            &mut budget,
        );
    }
    assert_eq!(lines.context.len(), FS_CONTEXT_QUOTA);
    assert_eq!(budget.dropped, 2);
}

#[test]
fn journal_verification_and_epoch_required() {
    let entry = serde_json::json!({
        "MESSAGE": "EXT4-fs error (device sda1): test",
        "_TRANSPORT": "kernel",
        "SYSLOG_FACILITY": "0",
        "_BOOT_ID": "boot-a",
        "__REALTIME_TIMESTAMP": "1700000000000000",
        "__MONOTONIC_TIMESTAMP": "1099000000",
    });
    let lines = parse_journal_fs_lines(&entry.to_string(), &ctx(), &mut ContextBudget::new());
    assert_eq!(lines.events.len(), 1);
    assert_eq!(lines.events[0].age_seconds(&ctx(), 1100), 1);
    let forged = serde_json::json!({
        "MESSAGE": "EXT4-fs error (device sda1): test",
        "_TRANSPORT": "user",
        "SYSLOG_FACILITY": "0",
        "_BOOT_ID": "boot-a",
        "__REALTIME_TIMESTAMP": "1700000000000000",
    });
    let lines = parse_journal_fs_lines(&forged.to_string(), &ctx(), &mut ContextBudget::new());
    assert!(lines.events.is_empty());
    assert_eq!(lines.unverified, 1);
}
