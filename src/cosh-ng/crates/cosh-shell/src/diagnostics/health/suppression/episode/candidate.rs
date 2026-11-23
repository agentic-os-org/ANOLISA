//! Finding-to-episode candidate mapping: state members, quota combos and
//! canonical event identities for the claim path.

use super::super::super::model::HealthScanReport;
use super::super::super::rules::memory::memory_active;
use super::super::super::rules::quota;
use super::super::super::rules::volume::MemberLevel;
use super::signals::{counter_sample, string_fact, unsigned_fact};
use super::{counter_identity, volume_member_key, CandidateEpisode};

pub(crate) fn candidate_for_finding(
    report: &HealthScanReport,
    finding_id: &str,
    severity: u8,
    problem: &str,
) -> Option<CandidateEpisode> {
    let base = finding_id.split(':').next().unwrap_or(finding_id);
    let volume_active = report
        .facts
        .iter()
        .any(|fact| fact.key.ends_with(".volume_proven"));
    if matches!(base, "J09" | "J17") && volume_active {
        let mount = finding_id
            .split_once(':')
            .map(|(_, mount)| mount.to_string())
            .unwrap_or_else(|| "/".to_string());
        let member = if base == "J17" { "inode" } else { "bytes" };
        let prefix = format!("filesystem.mount.{mount}.");
        let device = string_fact(report, &format!("{prefix}volume_device")).unwrap_or_default();
        let fsid = string_fact(report, &format!("{prefix}volume_fsid")).unwrap_or_default();
        return Some(CandidateEpisode {
            event: false,
            key: volume_member_key(&mount, member, &device, &fsid),
            problem: problem.to_string(),
            severity,
            epoch: None,
            counter_window: None,
            quota_branches: None,
        });
    }
    if matches!(base, "J05") && memory_active(&report.facts) {
        return Some(CandidateEpisode {
            event: false,
            key: "host|memory".to_string(),
            problem: problem.to_string(),
            severity,
            epoch: None,
            counter_window: None,
            quota_branches: None,
        });
    }
    if matches!(base, "J18") && volume_active {
        let mount = finding_id
            .split_once(':')
            .map(|(_, mount)| mount.to_string())
            .unwrap_or_else(|| "/".to_string());
        let prefix = format!("filesystem.mount.{mount}.");
        let device = string_fact(report, &format!("{prefix}volume_device")).unwrap_or_default();
        let fsid = string_fact(report, &format!("{prefix}volume_fsid")).unwrap_or_default();
        return Some(CandidateEpisode {
            event: false,
            key: volume_member_key(&mount, "ro", &device, &fsid),
            problem: problem.to_string(),
            severity,
            epoch: None,
            counter_window: None,
            quota_branches: None,
        });
    }
    if matches!(base, "J15" | "J16") {
        if let Some((_, name)) = finding_id.split_once(':') {
            if string_fact(report, &format!("service.{name}.persistent_qualified")).is_some() {
                return Some(CandidateEpisode {
                    event: false,
                    key: format!("service|{name}"),
                    problem: problem.to_string(),
                    severity,
                    epoch: None,
                    counter_window: None,
                    quota_branches: None,
                });
            }
        }
    }
    if matches!(base, "J11" | "J12")
        && unsigned_fact(report, "kernel.current_cgroup_version").is_some()
    {
        if let Some(identity) = string_fact(report, "kernel.oom_attributed_event_identity") {
            let pid = unsigned_fact(report, "kernel.oom_attributed_pid")?;
            let domain = if identity.starts_with("uptime-ns:") {
                "kernel-boottime-v1"
            } else {
                "kernel-epoch-v1"
            };
            return Some(CandidateEpisode {
                event: true,
                key: format!("{domain}|{identity}|pid:{pid}"),
                problem: problem.to_string(),
                severity,
                epoch: unsigned_fact(report, "kernel.oom_attributed_epoch"),
                counter_window: None,
                quota_branches: None,
            });
        }
        let sample = counter_sample(report)?;
        let window = (
            unsigned_fact(report, "kernel.cgroup_oom_kill_window_start_epoch")?,
            unsigned_fact(report, "kernel.cgroup_oom_kill_window_end_epoch")?,
        );
        let delta = unsigned_fact(report, "kernel.cgroup_oom_kill_increment")?;
        return Some(CandidateEpisode {
            event: true,
            key: format!(
                "cgroup-counter-v1|{}",
                counter_identity(&sample, window, delta)
            ),
            problem: problem.to_string(),
            severity,
            epoch: Some(window.1),
            counter_window: Some(window),
            quota_branches: None,
        });
    }
    if base == "J19" {
        let id = string_fact(report, "kernel.current_cgroup_id").filter(|id| !id.is_empty())?;
        let judgement = quota::quota_judgement(&report.facts);
        return Some(CandidateEpisode {
            event: false,
            key: format!("cgroup|{id}|quota"),
            problem: problem.to_string(),
            severity,
            epoch: None,
            counter_window: None,
            quota_branches: Some((
                judgement.max.entry == Some(MemberLevel::Critical),
                judgement.high.entry.is_some(),
            )),
        });
    }
    if matches!(base, "J20" | "J21" | "J22") {
        let resource = match base {
            "J20" => "memory",
            "J21" => "io",
            _ => "cpu",
        };
        let key = if finding_id
            .split_once(':')
            .is_some_and(|(_, suffix)| suffix == "cgroup")
        {
            let id = string_fact(report, "kernel.current_cgroup_id").filter(|id| !id.is_empty())?;
            format!("cgroup|{id}|psi_{resource}")
        } else {
            format!("host|psi_{resource}")
        };
        return Some(CandidateEpisode {
            event: false,
            key,
            problem: problem.to_string(),
            severity,
            epoch: None,
            counter_window: None,
            quota_branches: None,
        });
    }
    if base == "J23" && string_fact(report, "host.conntrack_netns_id").is_some() {
        return Some(CandidateEpisode {
            event: false,
            key: "host|conntrack".to_string(),
            problem: problem.to_string(),
            severity,
            epoch: None,
            counter_window: None,
            quota_branches: None,
        });
    }
    if matches!(base, "J24" | "J25" | "J26" | "J27" | "J28" | "J29") {
        let prefix = match base {
            "J24" => "ext4_forced_ro",
            "J25" => "ext4_error",
            "J26" => "jbd2_abort",
            "J27" => "jbd2_flush",
            "J28" => "xfs_shutdown",
            _ => "block_io",
        };
        let identity = string_fact(report, &format!("kernel.fs_{prefix}_identity"))?;
        let boot = string_fact(report, "kernel.boot_id").filter(|boot| !boot.is_empty())?;
        let dev_t = string_fact(report, &format!("kernel.fs_{prefix}_dev_t")).unwrap_or_default();
        let problem = if base == "J29" {
            format!("block-io|{dev_t}")
        } else {
            problem.to_string()
        };
        return Some(CandidateEpisode {
            event: true,
            key: format!("fs-event-v1|{boot}|{identity}"),
            problem,
            severity,
            epoch: unsigned_fact(report, &format!("kernel.fs_{prefix}_epoch")),
            counter_window: None,
            quota_branches: None,
        });
    }
    if matches!(base, "J30" | "J31") {
        let boot = string_fact(report, "kernel.boot_id").filter(|boot| !boot.is_empty())?;
        let (kind, identity_key, problem) = if base == "J30" {
            (
                "hung-task",
                "kernel.hung_task_event_identity",
                format!("hung-task|{boot}"),
            )
        } else {
            let cpu = unsigned_fact(report, "kernel.soft_lockup_cpu").unwrap_or(0);
            (
                "soft-lockup",
                "kernel.soft_lockup_event_identity",
                format!("soft-lockup|{boot}|cpu{cpu}"),
            )
        };
        let identity = string_fact(report, identity_key)?;
        return Some(CandidateEpisode {
            event: true,
            key: format!("host-event-v1|{boot}|{kind}|{identity}"),
            problem,
            severity,
            epoch: None,
            counter_window: None,
            quota_branches: None,
        });
    }
    None
}
