//! v3.7 R01/R02 judgement over proven volume facts: exact integer comparisons,
//! independent byte/inode members, exit lines for recovery.

use std::collections::BTreeMap;

use super::super::model::{
    HealthFact, HealthFactValue, HealthFinding, HealthFindingCategory, HealthMessageId,
    HealthSeverity,
};
use super::evidence_ids;

const GIB: u128 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemberLevel {
    Critical,
    Warning,
}

/// Stateless per-member signals; the episode ledger resolves hysteresis and
/// recovery against its persisted state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MemberJudgement {
    pub entry: Option<MemberLevel>,
    pub exit: bool,
    pub valid: bool,
}

impl MemberJudgement {
    pub(crate) const UNKNOWN: Self = Self {
        entry: None,
        exit: false,
        valid: false,
    };

    pub(crate) fn inactive() -> Self {
        Self {
            entry: None,
            exit: false,
            valid: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VolumeJudgement {
    pub mount: String,
    pub bytes: MemberJudgement,
    pub inodes: MemberJudgement,
    pub bavail_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub files: Option<u64>,
    pub favail: Option<u64>,
    pub device: Option<String>,
    pub fsid: Option<String>,
}

fn unsigned(facts: &[HealthFact], key: &str) -> Option<u64> {
    match &super::fact(facts, key)?.value {
        HealthFactValue::Unsigned(value) => Some(*value),
        HealthFactValue::Integer(value) => u64::try_from(*value).ok(),
        _ => None,
    }
}

fn string(facts: &[HealthFact], key: &str) -> Option<String> {
    match &super::fact(facts, key)?.value {
        HealthFactValue::String(value) => Some(value.clone()),
        _ => None,
    }
}

fn bool_value(facts: &[HealthFact], key: &str) -> Option<bool> {
    match &super::fact(facts, key)?.value {
        HealthFactValue::Bool(value) => Some(*value),
        _ => None,
    }
}

/// U = used / (used + bavail), compared without floating point truncation.
fn ratio_at_least(used: u64, bavail: u64, percent: u128) -> bool {
    let used = u128::from(used);
    let total = used + u128::from(bavail);
    total > 0 && used * 100 >= percent * total
}

fn ratio_at_most(used: u64, bavail: u64, percent: u128) -> bool {
    let used = u128::from(used);
    let total = used + u128::from(bavail);
    total > 0 && used * 100 <= percent * total
}

pub(crate) fn judge_bytes(used: u64, bavail: u64) -> MemberJudgement {
    let margin = u128::from(bavail);
    let entry = if bavail == 0 || (ratio_at_least(used, bavail, 95) && margin < 2 * GIB) {
        Some(MemberLevel::Critical)
    } else if ratio_at_least(used, bavail, 90) && margin < 20 * GIB {
        Some(MemberLevel::Warning)
    } else {
        None
    };
    let exit = ratio_at_most(used, bavail, 92) || margin >= 3 * GIB;
    MemberJudgement {
        entry,
        exit,
        valid: true,
    }
}

pub(crate) fn judge_inodes(files: u64, favail: u64) -> MemberJudgement {
    if files == 0 || favail > files {
        return MemberJudgement::UNKNOWN;
    }
    let used = files - favail;
    let entry = if favail == 0 || (ratio_at_least(used, favail, 99) && favail < 10000) {
        Some(MemberLevel::Critical)
    } else if ratio_at_least(used, favail, 95) && favail < 100000 {
        Some(MemberLevel::Warning)
    } else {
        None
    };
    let exit = favail > 0 && (ratio_at_most(used, favail, 97) || favail >= 20000);
    MemberJudgement {
        entry,
        exit,
        valid: true,
    }
}

pub(crate) fn volume_active(facts: &[HealthFact]) -> bool {
    facts
        .iter()
        .any(|fact| fact.key.ends_with(".volume_proven"))
}

pub(crate) fn evaluate_volumes(facts: &[HealthFact]) -> Vec<VolumeJudgement> {
    let mut mounts = Vec::new();
    for fact in facts {
        if let Some(mount) = fact
            .key
            .strip_prefix("filesystem.mount.")
            .and_then(|suffix| suffix.strip_suffix(".volume_proven"))
        {
            if !mounts.iter().any(|existing| existing == mount) {
                mounts.push(mount.to_string());
            }
        }
    }
    mounts
        .into_iter()
        .map(|mount| {
            let prefix = format!("filesystem.mount.{mount}.");
            let proven = bool_value(facts, &format!("{prefix}volume_proven")) == Some(true);
            let bavail = unsigned(facts, &format!("{prefix}volume_bavail_bytes"));
            let used = unsigned(facts, &format!("{prefix}volume_used_bytes"));
            let files = unsigned(facts, &format!("{prefix}volume_files"));
            let favail = unsigned(facts, &format!("{prefix}volume_favail"));
            let inode_applicable =
                bool_value(facts, &format!("{prefix}volume_inode_applicable")) == Some(true);
            let bytes = if proven {
                match (used, bavail) {
                    (Some(used), Some(bavail)) => judge_bytes(used, bavail),
                    _ => MemberJudgement::UNKNOWN,
                }
            } else {
                MemberJudgement::UNKNOWN
            };
            let inodes = if proven && inode_applicable {
                match (files, favail) {
                    (Some(files), Some(favail)) => judge_inodes(files, favail),
                    _ => MemberJudgement::UNKNOWN,
                }
            } else if proven && !inode_applicable {
                MemberJudgement::inactive()
            } else {
                MemberJudgement::UNKNOWN
            };
            VolumeJudgement {
                mount,
                bytes,
                inodes,
                bavail_bytes: bavail,
                used_bytes: used,
                files,
                favail,
                device: string(facts, &format!("{prefix}volume_device")),
                fsid: string(facts, &format!("{prefix}volume_fsid")),
            }
        })
        .collect()
}

pub(crate) fn volume_findings(facts: &[HealthFact]) -> Vec<HealthFinding> {
    let mut findings = Vec::new();
    for judgement in evaluate_volumes(facts) {
        let byte_critical = judgement.bytes.entry == Some(MemberLevel::Critical);
        let inode_critical = judgement.inodes.entry == Some(MemberLevel::Critical);
        if !byte_critical && !inode_critical {
            continue;
        }
        let prefix = format!("filesystem.mount.{}.", judgement.mount);
        let mut evidence = evidence_ids(
            facts,
            &[
                &format!("{prefix}volume_proven"),
                &format!("{prefix}volume_bavail_bytes"),
                &format!("{prefix}volume_used_bytes"),
                &format!("{prefix}volume_bfree_bytes"),
                &format!("{prefix}volume_files"),
                &format!("{prefix}volume_favail"),
                &format!("{prefix}volume_fs_type"),
                &format!("{prefix}volume_device"),
            ],
        );
        if let Some(path) = super::fact(facts, &format!("{prefix}path")) {
            evidence.insert(0, path.id.clone());
        }
        let rule = if byte_critical { "J09" } else { "J17" };
        let mut result = HealthFinding {
            id: if judgement.mount == "/" {
                rule.to_string()
            } else {
                format!("{rule}:{}", judgement.mount)
            },
            severity: HealthSeverity::Critical,
            category: HealthFindingCategory::Anomaly,
            title_id: HealthMessageId::HealthFindingDiskHigh,
            detail_id: None,
            detail_args: BTreeMap::new(),
            evidence_fact_ids: evidence,
            suggested_try_ids: Vec::new(),
        };
        result
            .detail_args
            .insert("mount".into(), judgement.mount.clone());
        let members = match (byte_critical, inode_critical) {
            (true, true) => "bytes+inode",
            (true, false) => "bytes",
            (false, true) => "inode",
            _ => unreachable!(),
        };
        result.detail_args.insert("members".into(), members.into());
        if let Some(bavail) = judgement.bavail_bytes {
            result.detail_args.insert(
                "available_gib".into(),
                format!("{:.1}", bavail as f64 / GIB as f64),
            );
        }
        if let Some(favail) = judgement.favail {
            result
                .detail_args
                .insert("inode_free".into(), favail.to_string());
        }
        findings.push(result);
    }
    findings
}

#[cfg(test)]
mod tests;
