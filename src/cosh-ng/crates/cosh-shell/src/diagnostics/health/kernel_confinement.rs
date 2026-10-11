//! Kernel capability behind cosh-core's workspace confinement.
//!
//! cosh-core pins the session workspace with `openat2(RESOLVE_BENEATH)` and
//! exits before the agent replies when the syscall is missing (#3413). The
//! health scan records that capability as a fact so the judgement rules can
//! explain the requirement up front — at shell startup, in `/health` and in
//! `cosh-shell doctor` — instead of letting users meet it as a mid-reply
//! `Function not implemented (os error 38)`.
//!
//! Side-effect free and infallible: the release is read from procfs and only a
//! release that parses into a version can ever be reported as unsupported.

use std::fs;

use super::builder::HealthReportBuilder;
use super::model::{HealthFactCategory, HealthFactSource, HealthFactValue};

/// First mainline Linux release providing `openat2(2)`.
const OPENAT2_MINIMUM_KERNEL: (u64, u64) = (5, 6);
/// User-facing form of [`OPENAT2_MINIMUM_KERNEL`], interpolated into the
/// finding title and remediation text.
pub(crate) const OPENAT2_MINIMUM_KERNEL_LABEL: &str = "5.6";
const KERNEL_RELEASE_PATH: &str = "/proc/sys/kernel/osrelease";

/// Records the kernel release plus whether it can back workspace confinement.
/// Hosts without a readable release record nothing at all.
pub(crate) fn record_confinement_facts(builder: &mut HealthReportBuilder, elapsed_ms: u128) {
    let Some(release) = read_kernel_release() else {
        return;
    };
    record_release_facts(builder, &release, elapsed_ms);
}

fn record_release_facts(builder: &mut HealthReportBuilder, release: &str, elapsed_ms: u128) {
    builder.add_fact(
        HealthFactCategory::Kernel,
        "kernel.release",
        HealthFactValue::String(release.to_string()),
        None,
        HealthFactSource::ProcSysKernel,
        elapsed_ms,
    );
    // An unparsable release records no support fact, so the judgement rule can
    // only fire on a kernel that is known to be too old.
    if let Some(supported) = kernel_supports_openat2(release) {
        builder.add_fact(
            HealthFactCategory::Kernel,
            "kernel.openat2_supported",
            HealthFactValue::Bool(supported),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        );
    }
}

/// Release reported by the running kernel, when this host exposes one.
fn read_kernel_release() -> Option<String> {
    let release = fs::read_to_string(KERNEL_RELEASE_PATH).ok()?;
    let release = release.trim();
    (!release.is_empty()).then(|| release.to_string())
}

/// Whether a kernel release provides `openat2(2)`. `None` means the release
/// could not be parsed, which must never be reported as unsupported.
fn kernel_supports_openat2(release: &str) -> Option<bool> {
    Some(parse_kernel_version(release)? >= OPENAT2_MINIMUM_KERNEL)
}

/// `(major, minor)` of a `uname -r` style release such as
/// `4.19.112-2.el8.x86_64`. Non-numeric suffixes are ignored, a missing minor
/// counts as `.0`, and a release without a leading number yields `None`.
fn parse_kernel_version(release: &str) -> Option<(u64, u64)> {
    let mut fields = release.trim().split('.');
    let major = leading_digits(fields.next()?)?;
    let minor = fields.next().and_then(leading_digits).unwrap_or(0);
    Some((major, minor))
}

fn leading_digits(field: &str) -> Option<u64> {
    let end = field
        .char_indices()
        .find(|(_, character)| !character.is_ascii_digit())
        .map_or(field.len(), |(index, _)| index);
    field.get(..end)?.parse().ok()
}

#[cfg(test)]
#[path = "kernel_confinement_tests.rs"]
mod tests;
