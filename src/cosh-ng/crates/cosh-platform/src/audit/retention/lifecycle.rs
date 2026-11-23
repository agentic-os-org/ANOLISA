use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, Metadata};
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::MetadataExt;

use chrono::{DateTime, Utc};
use cosh_types::audit::{
    AuditEventType, AuditEventV1, AuditOutcomeStatus, KnownAuditEventType, MAX_AUDIT_RECORD_BYTES,
};
use nix::fcntl::{Flock, FlockArg};

use super::{open_existing_private, SegmentInfo};
use crate::audit::reader::{MAX_AUDIT_FILES, MAX_AUDIT_READ_BYTES, MAX_AUDIT_RECORDS};
use crate::audit::store::validate_private_file;

type Lifecycle = (u8, String, String);

struct Endpoint {
    segment: usize,
    record: usize,
    observed_at: DateTime<Utc>,
    kind: KnownAuditEventType,
    terminal: bool,
}

pub(super) struct Protection {
    blocked: BTreeSet<usize>,
    predecessors: Vec<BTreeSet<usize>>,
    pinned: Vec<Option<(Flock<File>, Metadata)>>,
}

impl Protection {
    pub(super) fn read(segments: &[SegmentInfo], truncated: bool) -> Option<Self> {
        if truncated || segments.len() > MAX_AUDIT_FILES {
            return None;
        }
        let mut names = BTreeSet::new();
        let mut lifecycles: BTreeMap<Lifecycle, Vec<Endpoint>> = BTreeMap::new();
        let mut pinned = Vec::new();
        let mut total_bytes = 0_u64;
        let mut records = 0;
        for (index, segment) in segments.iter().enumerate() {
            if !names.insert(&segment.basename) {
                return None;
            }
            let file = open_existing_private(&segment.path).ok()?;
            let metadata = file.metadata().ok()?;
            if metadata.nlink() != 1
                || metadata.len() != segment.bytes
                || metadata
                    .modified()
                    .ok()
                    .map(chrono::DateTime::<chrono::Utc>::from)
                    != Some(segment.timestamp)
            {
                return None;
            }
            total_bytes = total_bytes.checked_add(metadata.len())?;
            if total_bytes > MAX_AUDIT_READ_BYTES {
                return None;
            }
            let lock = if segment.active {
                None
            } else {
                Some(Flock::lock(file.try_clone().ok()?, FlockArg::LockSharedNonblock).ok()?)
            };
            let mut reader = BufReader::new((&file).take(metadata.len()));
            let mut line = Vec::new();
            loop {
                line.clear();
                let count = reader
                    .by_ref()
                    .take((MAX_AUDIT_RECORD_BYTES + 1) as u64)
                    .read_until(b'\n', &mut line)
                    .ok()?;
                if count == 0 {
                    break;
                }
                if count > MAX_AUDIT_RECORD_BYTES || !line.ends_with(b"\n") {
                    return None;
                }
                line.pop();
                if line.is_empty() {
                    continue;
                }
                records += 1;
                if records > MAX_AUDIT_RECORDS {
                    return None;
                }
                let event: AuditEventV1 = serde_json::from_slice(&line).ok()?;
                event.validate().ok()?;
                if matches!(event.outcome.status, AuditOutcomeStatus::Unknown) {
                    return None;
                }
                let AuditEventType::Known(kind) = event.event_type else {
                    return None;
                };
                if let Some((family, terminal)) = endpoint(kind, &event.outcome.status)? {
                    let key = (
                        family,
                        event.component.name.as_str().to_owned(),
                        serde_json::to_string(&event.identity).ok()?,
                    );
                    let endpoints = lifecycles.entry(key).or_default();
                    if endpoints.last().is_some_and(|previous| {
                        previous.segment == index && previous.observed_at > event.observed_at
                    }) {
                        return None;
                    }
                    endpoints.push(Endpoint {
                        segment: index,
                        record: records,
                        observed_at: event.observed_at,
                        kind,
                        terminal,
                    });
                }
            }
            if let Some(lock) = lock {
                let after = lock.metadata().ok()?;
                if !same_snapshot(&metadata, &after) {
                    return None;
                }
                pinned.push(Some((lock, metadata)));
            } else {
                pinned.push(None);
            }
        }
        let mut blocked = BTreeSet::new();
        let mut predecessors = vec![BTreeSet::new(); segments.len()];
        for (lifecycle, endpoints) in &mut lifecycles {
            if lifecycle.0 == 0 {
                let mut open = BTreeSet::new();
                for endpoint in endpoints {
                    if endpoint.terminal {
                        if !open.remove(&endpoint.segment) {
                            blocked.insert(endpoint.segment);
                        }
                    } else if !open.insert(endpoint.segment) {
                        blocked.insert(endpoint.segment);
                    }
                }
                blocked.extend(open);
                continue;
            }
            endpoints.sort_by_key(|endpoint| (endpoint.observed_at, endpoint.record));
            if endpoints.windows(2).any(|pair| {
                pair[0].observed_at == pair[1].observed_at && pair[0].segment != pair[1].segment
            }) {
                return None;
            }
            let mut starts = BTreeSet::new();
            let mut kinds = Vec::new();
            let mut complete = false;
            for endpoint in endpoints {
                if endpoint.terminal {
                    predecessors[endpoint.segment]
                        .extend(starts.iter().filter(|start| **start != endpoint.segment));
                    complete = true;
                } else {
                    if complete {
                        starts.clear();
                        kinds.clear();
                        complete = false;
                    }
                    if kinds.contains(&endpoint.kind) {
                        return None;
                    }
                    kinds.push(endpoint.kind);
                    starts.insert(endpoint.segment);
                }
            }
            if !complete {
                blocked.extend(starts);
            }
        }
        Some(Self {
            blocked,
            predecessors,
            pinned,
        })
    }

    pub(super) fn order(&self, segments: &[SegmentInfo]) -> Vec<usize> {
        let mut pending: BTreeSet<_> = segments
            .iter()
            .enumerate()
            .filter(|(index, segment)| !segment.active && !self.blocked.contains(index))
            .map(|(index, _)| index)
            .collect();
        let mut removed = BTreeSet::new();
        let mut order = Vec::new();
        while let Some(index) = pending
            .iter()
            .find(|index| self.ready(**index, &removed))
            .copied()
        {
            pending.remove(&index);
            removed.insert(index);
            order.push(index);
        }
        order
    }

    pub(super) fn ready(&self, index: usize, removed: &BTreeSet<usize>) -> bool {
        !self.blocked.contains(&index) && self.predecessors[index].is_subset(removed)
    }

    pub(super) fn can_remove(
        &self,
        index: usize,
        segments: &[SegmentInfo],
        removed: &BTreeSet<usize>,
    ) -> bool {
        if !self.predecessors[index].is_subset(removed) {
            return false;
        }
        let Some((file, snapshot)) = &self.pinned[index] else {
            return false;
        };
        let path = &segments[index].path;
        if validate_private_file(file, path).is_err() {
            return false;
        }
        let (Ok(opened), Ok(named)) = (file.metadata(), std::fs::symlink_metadata(path)) else {
            return false;
        };
        named.is_file()
            && !named.file_type().is_symlink()
            && same_snapshot(snapshot, &opened)
            && same_snapshot(&opened, &named)
    }
}

fn same_snapshot(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.len() == right.len()
        && left.nlink() == 1
        && right.nlink() == 1
        && left.modified().ok() == right.modified().ok()
}

fn endpoint(kind: KnownAuditEventType, status: &AuditOutcomeStatus) -> Option<Option<(u8, bool)>> {
    use AuditOutcomeStatus::{Allowed, Cancelled, Denied, Failed, Started, Success};
    use KnownAuditEventType::*;
    let (family, terminal, valid) = match kind {
        SessionStarted => (0, false, matches!(status, Started)),
        SessionEnded => (0, true, matches!(status, Success | Failed | Cancelled)),
        TurnStarted => (1, false, matches!(status, Started)),
        TurnCompleted | TurnFailed => (1, true, matches!(status, Success | Failed | Cancelled)),
        ProviderRequestStarted => (2, false, matches!(status, Started)),
        ProviderRequestCompleted | ProviderRequestFailed | ProviderRequestCancelled => {
            (2, true, matches!(status, Success | Failed | Cancelled))
        }
        ToolRequested | ToolExecutionStarted => (3, false, matches!(status, Started)),
        ToolCompleted | ToolFailed | ToolCancelled => {
            (3, true, matches!(status, Success | Failed | Cancelled))
        }
        ApprovalRequested => (4, false, matches!(status, Started)),
        ApprovalResolved => (
            4,
            true,
            matches!(status, Allowed | Denied | Failed | Cancelled),
        ),
        ShellCommandStarted => (5, false, matches!(status, Started)),
        ShellCommandCompleted | ShellCommandFailed => {
            (5, true, matches!(status, Success | Failed | Cancelled))
        }
        _ => return Some(None),
    };
    valid.then_some(Some((family, terminal)))
}
