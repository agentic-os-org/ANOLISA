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

library_tests! {
fn chain_facts_builder() -> crate::diagnostics::health::builder::HealthReportBuilder {
    use crate::diagnostics::health::model::{
        HealthFactCategory, HealthFactSource, HealthFactValue,
    };
    let mut builder = crate::diagnostics::health::builder::HealthReportBuilder::new("scan-bind", 0);
    builder.add_fact(
        HealthFactCategory::Kernel,
        "kernel.fs_ext4_forced_ro_candidates",
        HealthFactValue::String(
            r#"[{"device":"dm-0","dev_t":"253:0","age_seconds":12,"identity":"ext4-forced-ro|dm-0|epoch:1095","epoch":1095,"operation":null}]"#
                .into(),
        ),
        None,
        HealthFactSource::Derived,
        0,
    );
    builder
}

fn add_volume_device(
    builder: &mut crate::diagnostics::health::builder::HealthReportBuilder,
    mount: &str,
    dev_t: &str,
) {
    use crate::diagnostics::health::model::{
        HealthFactCategory, HealthFactSource, HealthFactValue,
    };
    builder.add_fact(
        HealthFactCategory::Disk,
        format!("filesystem.mount.{mount}.volume_device"),
        HealthFactValue::String(dev_t.into()),
        None,
        HealthFactSource::Statfs,
        0,
    );
}

fn fact_value(
    builder: &crate::diagnostics::health::builder::HealthReportBuilder,
    key: &str,
) -> Option<crate::diagnostics::health::model::HealthFactValue> {
    builder
        .report()
        .facts
        .iter()
        .find(|fact| fact.key == key)
        .map(|fact| fact.value.clone())
}

#[test]
fn bind_fs_chain_mounts_binds_volume_fact_from_other_worker() {
    use crate::diagnostics::health::model::HealthFactValue;
    let mut builder = chain_facts_builder();
    add_volume_device(&mut builder, "/data", "253:0");
    bind_fs_chain_mounts(&mut builder, 1);
    assert_eq!(
        fact_value(&builder, "kernel.fs_ext4_forced_ro_mount"),
        Some(HealthFactValue::String("/data".into()))
    );
    assert_eq!(
        fact_value(&builder, "kernel.fs_ext4_forced_ro_degraded_reason"),
        None
    );
}

#[test]
fn bind_fs_chain_mounts_keeps_unbound_device_degraded() {
    use crate::diagnostics::health::model::HealthFactValue;
    let mut builder = chain_facts_builder();
    bind_fs_chain_mounts(&mut builder, 1);
    assert_eq!(fact_value(&builder, "kernel.fs_ext4_forced_ro_mount"), None);
    assert_eq!(
        fact_value(&builder, "kernel.fs_ext4_forced_ro_degraded_reason"),
        Some(HealthFactValue::String("device-unbound".into()))
    );
    assert_eq!(
        fact_value(&builder, "kernel.fs_ext4_forced_ro_degraded_age_seconds"),
        Some(HealthFactValue::Unsigned(12))
    );
}

#[test]
fn bind_fs_chain_mounts_leaves_existing_mount_untouched() {
    use crate::diagnostics::health::model::{
        HealthFactCategory, HealthFactSource, HealthFactValue,
    };
    let mut builder = chain_facts_builder();
    builder.add_fact(
        HealthFactCategory::Kernel,
        "kernel.fs_ext4_forced_ro_mount",
        HealthFactValue::String("/fixture".into()),
        None,
        HealthFactSource::Derived,
        0,
    );
    add_volume_device(&mut builder, "/data", "253:0");
    bind_fs_chain_mounts(&mut builder, 1);
    assert_eq!(
        fact_value(&builder, "kernel.fs_ext4_forced_ro_mount"),
        Some(HealthFactValue::String("/fixture".into()))
    );
    assert_eq!(
        builder
            .report()
            .facts
            .iter()
            .filter(|fact| fact.key == "kernel.fs_ext4_forced_ro_mount")
            .count(),
        1
    );
}

#[cfg(target_os = "linux")]
#[test]
fn record_fs_chains_defers_mount_binding_to_parent() {
    use crate::diagnostics::health::model::HealthFactValue;
    let Some(device) = std::fs::read_dir("/sys/class/block")
        .ok()
        .and_then(|mut entries| entries.next())
        .and_then(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
    else {
        return;
    };
    let Ok(dev_t) = std::fs::read_to_string(format!("/sys/class/block/{device}/dev")) else {
        return;
    };
    let dev_t = dev_t.trim().to_string();
    let chain = FsChain {
        mechanism: "ext4-forced-ro",
        device: device.clone(),
        fs_device: device.clone(),
        age_seconds: 5,
        epoch: 1095,
        identity: format!("ext4-forced-ro|{device}|epoch:1095"),
        operation: None,
        eligible: true,
        degraded_reason: None,
    };
    let mut builder =
        crate::diagnostics::health::builder::HealthReportBuilder::new("scan-split", 0);
    record_fs_chains(&mut builder, &[chain], &FsLines::default(), 0);
    let candidates = match fact_value(&builder, "kernel.fs_ext4_forced_ro_candidates") {
        Some(HealthFactValue::String(json)) => json,
        other => panic!("expected candidates fact, got {other:?}"),
    };
    assert!(candidates.contains(&dev_t));
    assert_eq!(fact_value(&builder, "kernel.fs_ext4_forced_ro_dev_t"), None);
    assert_eq!(fact_value(&builder, "kernel.fs_ext4_forced_ro_mount"), None);
    assert_eq!(
        fact_value(&builder, "kernel.fs_ext4_forced_ro_degraded_reason"),
        None
    );
    add_volume_device(&mut builder, "/mnt/test", &dev_t);
    bind_fs_chain_mounts(&mut builder, 1);
    assert_eq!(
        fact_value(&builder, "kernel.fs_ext4_forced_ro_dev_t"),
        Some(HealthFactValue::String(dev_t.clone()))
    );
    assert_eq!(
        fact_value(&builder, "kernel.fs_ext4_forced_ro_age_seconds"),
        Some(HealthFactValue::Unsigned(5))
    );
    assert_eq!(
        fact_value(&builder, "kernel.fs_ext4_forced_ro_mount"),
        Some(HealthFactValue::String("/mnt/test".into()))
    );
    let findings =
        crate::diagnostics::health::rules::fs_events::fs_event_findings(&builder.report().facts);
    assert!(findings.iter().any(|finding| finding.id == "J24"));
}

#[test]
fn forced_ro_requires_error_not_after_anchor() {
    let anchor = line(FsEventKind::Ext4ForcedRo, "dm-0", 1100);
    let late_error = line(FsEventKind::Ext4Error, "dm-0", 1102);
    let lines = lines_with(vec![anchor.clone(), late_error], vec![]);
    let chains = build_chains(&lines, &ctx(), 1100);
    let forced = chains
        .iter()
        .find(|chain| chain.mechanism == "ext4-forced-ro")
        .expect("forced-ro chain");
    assert!(!forced.eligible);
    assert_eq!(forced.degraded_reason, Some("no-error-anchor"));

    let early_error = line(FsEventKind::Ext4Error, "dm-0", 1098);
    let lines = lines_with(vec![anchor, early_error], vec![]);
    let chains = build_chains(&lines, &ctx(), 1100);
    let forced = chains
        .iter()
        .find(|chain| chain.mechanism == "ext4-forced-ro")
        .expect("forced-ro chain");
    assert!(forced.eligible);
    assert_eq!(forced.degraded_reason, None);
}

#[test]
fn bind_selects_newest_bound_not_newest_global() {
    use crate::diagnostics::health::model::{
        HealthFactCategory, HealthFactSource, HealthFactValue,
    };
    let mut builder =
        crate::diagnostics::health::builder::HealthReportBuilder::new("scan-order", 0);
    builder.add_fact(
        HealthFactCategory::Kernel,
        "kernel.fs_ext4_forced_ro_candidates",
        HealthFactValue::String(
            r#"[{"device":"dm-1","dev_t":"253:1","age_seconds":2,"identity":"ext4-forced-ro|dm-1|epoch:1099","epoch":1099,"operation":null},{"device":"dm-0","dev_t":"253:0","age_seconds":10,"identity":"ext4-forced-ro|dm-0|epoch:1090","epoch":1090,"operation":null}]"#
                .into(),
        ),
        None,
        HealthFactSource::Derived,
        0,
    );
    add_volume_device(&mut builder, "/data", "253:0");
    bind_fs_chain_mounts(&mut builder, 1);
    assert_eq!(
        fact_value(&builder, "kernel.fs_ext4_forced_ro_mount"),
        Some(HealthFactValue::String("/data".into()))
    );
    assert_eq!(
        fact_value(&builder, "kernel.fs_ext4_forced_ro_device"),
        Some(HealthFactValue::String("dm-0".into()))
    );
    assert_eq!(
        fact_value(&builder, "kernel.fs_ext4_forced_ro_age_seconds"),
        Some(HealthFactValue::Unsigned(10))
    );
}

#[test]
fn fs_event_notices_gate_defaults_off() {
    use crate::diagnostics::health::model::{
        HealthFactCategory, HealthFactSource, HealthFactValue,
    };
    let mut builder = crate::diagnostics::health::builder::HealthReportBuilder::new("scan-gate", 0);
    for (key, value) in [
        (
            "kernel.fs_ext4_forced_ro_age_seconds",
            HealthFactValue::Unsigned(5),
        ),
        (
            "kernel.fs_ext4_forced_ro_mount",
            HealthFactValue::String("/data".into()),
        ),
        (
            "kernel.fs_ext4_forced_ro_device",
            HealthFactValue::String("dm-0".into()),
        ),
        (
            "kernel.fs_ext4_forced_ro_dev_t",
            HealthFactValue::String("253:0".into()),
        ),
        (
            "kernel.fs_ext4_forced_ro_identity",
            HealthFactValue::String("ext4-forced-ro|dm-0|epoch:1095".into()),
        ),
        (
            "kernel.fs_ext4_forced_ro_epoch",
            HealthFactValue::Unsigned(1095),
        ),
        ("kernel.boot_id", HealthFactValue::String("boot-a".into())),
        (
            "kernel.fs_event_source",
            HealthFactValue::String("journalctl".into()),
        ),
    ] {
        builder.add_fact(
            HealthFactCategory::Kernel,
            key,
            value,
            None,
            HealthFactSource::Derived,
            0,
        );
    }
    let report = builder.finish(1);
    let off_config = crate::config::HealthConfig::default();
    let off = crate::diagnostics::health::rules::evaluate_judgement_rules(&report, &off_config);
    assert!(!off.iter().any(|finding| finding.id.starts_with("J24")));
    let on_config = crate::config::HealthConfig {
        fs_event_notices: true,
        ..crate::config::HealthConfig::default()
    };
    let on = crate::diagnostics::health::rules::evaluate_judgement_rules(&report, &on_config);
    assert!(on.iter().any(|finding| finding.id.starts_with("J24")));
}

/// Characterization of the device-lifetime gap (independent review point 1):
/// the binding protocol carries only the current sysfs dev_t, not the
/// event-time device generation. A same-boot, same-name, same-dev_t old event
/// therefore binds to a recreated volume. This is why FS-event B promotion is
/// blocked until lifecycle evidence enters the protocol; the default-off gate
/// limits exposure in the meantime. The fact remains visible at D tier.
#[test]
fn same_dev_t_old_event_binds_recreated_volume_lifecycle_unproven() {
    use crate::diagnostics::health::model::{
        HealthFactCategory, HealthFactSource, HealthFactValue,
    };
    let mut builder =
        crate::diagnostics::health::builder::HealthReportBuilder::new("scan-lifecycle", 0);
    builder.add_fact(
        HealthFactCategory::Kernel,
        "kernel.fs_ext4_forced_ro_candidates",
        HealthFactValue::String(
            r#"[{"device":"dm-0","dev_t":"253:0","age_seconds":3600,"identity":"ext4-forced-ro|dm-0|epoch:100","epoch":100,"operation":null}]"#
                .into(),
        ),
        None,
        HealthFactSource::Derived,
        0,
    );
    add_volume_device(&mut builder, "/recreated-volume", "253:0");
    bind_fs_chain_mounts(&mut builder, 1);
    assert_eq!(
        fact_value(&builder, "kernel.fs_ext4_forced_ro_mount"),
        Some(HealthFactValue::String("/recreated-volume".into()))
    );
}
}
