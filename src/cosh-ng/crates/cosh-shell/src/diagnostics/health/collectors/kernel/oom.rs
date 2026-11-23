pub(super) fn oom_scope_label_id_from_constraint(raw: &str) -> &'static str {
    match raw {
        "CONSTRAINT_MEMCG" => "memcg",
        "CONSTRAINT_NONE" => "host",
        "CONSTRAINT_CPUSET" => "cpuset",
        "CONSTRAINT_MEMORY_POLICY" => "memory_policy",
        _ => "unknown",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OomSignal {
    pub(super) epoch: u64,
    pub(super) identity_epoch: u64,
    pub(super) monotonic_ns: Option<u64>,
    pub(super) process: Option<String>,
    pub(super) pid: Option<u64>,
    pub(super) constraint: Option<String>,
    pub(super) task_cgroup: Option<String>,
    pub(super) oom_cgroup: Option<String>,
    pub(super) source_line_count: u64,
    pub(super) has_event_anchor: bool,
    pub(super) has_kill_record: bool,
}

impl OomSignal {
    pub(super) fn event_epoch(&self) -> u64 {
        if self.has_event_anchor {
            self.identity_epoch
        } else {
            self.epoch
        }
    }

    pub(super) fn from_line(epoch: u64, line: &str) -> Self {
        let (legacy_task_cgroup, legacy_oom_cgroup) = parse_legacy_memcg_cgroups(line);
        let event_pid = confirmed_kill_pid(line);
        Self {
            epoch,
            identity_epoch: epoch,
            monotonic_ns: line
                .trim_start()
                .strip_prefix('[')
                .and_then(|line| line.split_once(']'))
                .and_then(|(time, _)| time.trim().parse::<f64>().ok())
                .filter(|time| time.is_finite() && *time >= 0.0)
                .map(|time| (time * 1_000_000_000.0).round() as u64),
            process: parse_killed_process(line)
                .or_else(|| parse_oom_key_value(line, "task"))
                .or_else(|| parse_oom_key_value(line, "comm")),
            pid: event_pid
                .or_else(|| parse_killed_pid(line))
                .or_else(|| parse_oom_key_value(line, "pid")?.parse().ok()),
            constraint: parse_oom_key_value(line, "constraint"),
            task_cgroup: parse_oom_key_value(line, "task_memcg")
                .or_else(|| parse_oom_key_value(line, "task_cgroup"))
                .or(legacy_task_cgroup),
            oom_cgroup: parse_oom_key_value(line, "oom_memcg")
                .or_else(|| parse_oom_key_value(line, "oom_cgroup"))
                .or(legacy_oom_cgroup),
            source_line_count: 1,
            has_event_anchor: event_pid.is_some(),
            has_kill_record: line.contains("oom-kill:") || line.contains("Killed process"),
        }
    }

    fn merge_from(&mut self, other: OomSignal) {
        self.epoch = self.epoch.max(other.epoch);
        if !self.has_event_anchor && other.has_event_anchor {
            self.identity_epoch = other.identity_epoch;
            self.monotonic_ns = other.monotonic_ns;
        }
        self.process = self.process.take().or(other.process);
        self.pid = self.pid.or(other.pid);
        self.constraint = self.constraint.take().or(other.constraint);
        self.task_cgroup = self.task_cgroup.take().or(other.task_cgroup);
        self.oom_cgroup = self.oom_cgroup.take().or(other.oom_cgroup);
        self.source_line_count += other.source_line_count;
        self.has_event_anchor |= other.has_event_anchor;
        self.has_kill_record |= other.has_kill_record;
    }
}

fn confirmed_kill_pid(line: &str) -> Option<u64> {
    let line = line.trim_start();
    let message = if let Some(rest) = line.strip_prefix('[') {
        rest.split_once(']')?.1.trim_start()
    } else {
        let (timestamp, rest) = line.split_once(' ')?;
        timestamp.parse::<f64>().ok()?;
        let rest = rest.trim_start();
        if let Some(message) = rest.strip_prefix("kernel:") {
            message
        } else {
            rest.split_once(' ')?
                .1
                .trim_start()
                .strip_prefix("kernel:")?
        }
    }
    .trim_start();
    let message = message
        .strip_prefix("kernel:")
        .unwrap_or(message)
        .trim_start();
    let message = if let Some(rest) = message.strip_prefix('[') {
        let (caller, message) = rest.split_once(']')?;
        let caller = caller.trim();
        let caller_id = caller
            .strip_prefix('T')
            .or_else(|| caller.strip_prefix('C'))?;
        caller_id.trim().parse::<u64>().ok()?;
        message.trim_start()
    } else {
        message
    };
    let message = message
        .strip_prefix("Out of memory: ")
        .or_else(|| message.strip_prefix("Memory cgroup out of memory: "))
        .unwrap_or(message);
    message
        .strip_prefix("Killed process ")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

pub(super) fn merge_oom_signal(events: &mut Vec<OomSignal>, signal: OomSignal, starts_event: bool) {
    if !starts_event {
        if let Some(index) = associate_event_index(events, &signal) {
            events[index].merge_from(signal);
            return;
        }
    }
    events.push(signal);
}

const OOM_ASSOCIATION_LOOKBACK: usize = 8;

fn associate_event_index(events: &[OomSignal], signal: &OomSignal) -> Option<usize> {
    events
        .iter()
        .enumerate()
        .rev()
        .take(OOM_ASSOCIATION_LOOKBACK)
        .find_map(|(index, current)| {
            let same_pid = current.pid.is_some() && current.pid == signal.pid;
            let different_pid = current.pid.zip(signal.pid).is_some_and(|(a, b)| a != b);
            // Two kill records are two events unless they name the same victim.
            let duplicate_kill_record =
                signal.has_kill_record && current.has_kill_record && !same_pid;
            let different_scope = [
                (&current.constraint, &signal.constraint),
                (&current.oom_cgroup, &signal.oom_cgroup),
            ]
            .into_iter()
            .any(|(a, b)| a.as_ref().zip(b.as_ref()).is_some_and(|(a, b)| a != b));
            (!duplicate_kill_record
                && !different_pid
                && !different_scope
                && signal.epoch.abs_diff(current.epoch) <= 5)
                .then_some(index)
        })
}

pub(super) fn is_oom_line(lower: &str) -> bool {
    lower.contains("oom-kill")
        || lower.contains("out of memory")
        || lower.contains("killed process")
        || lower.contains("oom_reaper")
        || lower.contains("killed as a result of limit of")
}

pub(super) fn count_oom_events(
    events: &[OomSignal],
    now_epoch_seconds: u64,
    window_seconds: u64,
) -> u64 {
    events
        .iter()
        .filter(|event| now_epoch_seconds.saturating_sub(event.event_epoch()) <= window_seconds)
        .count() as u64
}

fn process_record_offset(line: &str) -> Option<usize> {
    ["Killed process", "reaped process"]
        .into_iter()
        .filter_map(|marker| line.find(marker).map(|start| start + marker.len()))
        .min()
}

fn parse_legacy_memcg_cgroups(line: &str) -> (Option<String>, Option<String>) {
    let Some((_, rest)) = line.split_once("Task in ") else {
        return (None, None);
    };
    let Some((task, limit)) = rest.split_once(" killed as a result of limit of ") else {
        return (None, None);
    };
    let task = task.trim();
    let limit = limit.split_whitespace().next().unwrap_or_default().trim();
    (
        (!task.is_empty()).then(|| task.to_string()),
        (!limit.is_empty()).then(|| limit.to_string()),
    )
}

fn parse_killed_process(line: &str) -> Option<String> {
    let start = process_record_offset(line)?;
    let rest = line[start..].trim_start();
    let mut tokens = rest.split_whitespace();
    tokens.next()?;
    tokens
        .next()
        .map(|token| token.trim_matches(|ch| ch == '(' || ch == ')' || ch == ':' || ch == ','))
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn parse_killed_pid(line: &str) -> Option<u64> {
    let start = process_record_offset(line)?;
    line[start..].split_whitespace().next()?.parse().ok()
}

fn parse_oom_key_value(line: &str, key: &str) -> Option<String> {
    let marker = format!("{key}=");
    let start = line.find(&marker)? + marker.len();
    let value = line[start..]
        .split([',', ' ', '\t'])
        .next()?
        .trim_matches(|ch| ch == '"' || ch == '\'' || ch == ':' || ch == ';');
    (!value.is_empty()).then(|| value.to_string())
}
