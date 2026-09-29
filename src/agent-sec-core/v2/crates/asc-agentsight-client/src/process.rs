//! Linux process identity resolution required by `AgentSight`.

use std::fs;

/// Failure while resolving the PID-reuse-resistant process identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProcessIdentityError {
    /// `AgentSight` accepts only positive process IDs.
    #[error("invalid process id")]
    InvalidPid,
    /// `/proc` did not expose the process identity at request time.
    #[error("process identity unavailable")]
    Unavailable,
    /// The selected process no longer exists; replay must not select a new PID.
    #[error("process no longer exists")]
    Exited,
    /// `/proc/<pid>/stat` did not have the expected Linux format.
    #[error("malformed process identity")]
    Malformed,
}

/// Runtime resolver for `AgentSight`'s required `process_start_time` field.
pub trait ProcessIdentityResolver: Send + Sync {
    /// Reads Linux `/proc/<pid>/stat` field 22 in kernel clock ticks.
    ///
    /// # Errors
    /// Returns a stable local resolution category.
    fn process_start_time(&self, pid: i32) -> Result<u64, ProcessIdentityError>;

    /// Host boot identity used to fence prepared requests across reboots.
    /// # Errors
    /// Returns a stable local resolution category.
    fn boot_id(&self) -> Result<String, ProcessIdentityError>;
}

/// Linux `/proc` implementation of [`ProcessIdentityResolver`].
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcProcessIdentityResolver;

impl ProcessIdentityResolver for ProcProcessIdentityResolver {
    fn process_start_time(&self, pid: i32) -> Result<u64, ProcessIdentityError> {
        if pid <= 0 {
            return Err(ProcessIdentityError::InvalidPid);
        }
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ProcessIdentityError::Exited
            } else {
                ProcessIdentityError::Unavailable
            }
        })?;
        parse_process_start_time(&stat)
    }

    fn boot_id(&self) -> Result<String, ProcessIdentityError> {
        let value = fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map_err(|_| ProcessIdentityError::Unavailable)?;
        let id =
            uuid::Uuid::parse_str(value.trim()).map_err(|_| ProcessIdentityError::Malformed)?;
        if id.is_nil() {
            return Err(ProcessIdentityError::Malformed);
        }
        Ok(id.to_string())
    }
}

fn parse_process_start_time(stat: &str) -> Result<u64, ProcessIdentityError> {
    let command_start = stat.find('(').ok_or(ProcessIdentityError::Malformed)?;
    let command_end = stat.rfind(')').ok_or(ProcessIdentityError::Malformed)?;
    if command_start >= command_end {
        return Err(ProcessIdentityError::Malformed);
    }
    let fields_after_command = stat
        .get(command_end + 1..)
        .ok_or(ProcessIdentityError::Malformed)?;
    let start_time = fields_after_command
        .split_whitespace()
        .nth(19)
        .ok_or(ProcessIdentityError::Malformed)?
        .parse::<u64>()
        .map_err(|_| ProcessIdentityError::Malformed)?;
    if start_time == 0 {
        return Err(ProcessIdentityError::Malformed);
    }
    Ok(start_time)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn production_resolver_reads_current_process_and_boot_identity() {
        let pid = i32::try_from(std::process::id()).unwrap();
        assert!(ProcProcessIdentityResolver.process_start_time(pid).unwrap() > 0);
        let boot = ProcProcessIdentityResolver.boot_id().unwrap();
        assert!(!uuid::Uuid::parse_str(&boot).unwrap().is_nil());
    }

    #[test]
    fn parser_uses_field_22_after_a_command_containing_spaces_and_parentheses() {
        let stat =
            "42 (agent worker (one)) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 987654 20";
        assert_eq!(parse_process_start_time(stat).unwrap(), 987_654);
    }

    #[test]
    fn parser_rejects_missing_and_zero_start_times() {
        assert_eq!(
            parse_process_start_time("42 agent S 1 2").unwrap_err(),
            ProcessIdentityError::Malformed
        );
        let stat = "42 (agent) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 0";
        assert_eq!(
            parse_process_start_time(stat).unwrap_err(),
            ProcessIdentityError::Malformed
        );
    }
}
