//! Native startup assets for explicit, application-owned bridge preparation.

use crate::Result;

/// Startup asset installed into an explicitly selected native environment.
pub struct Asset {
    /// Relative filename within the caller-owned setup directory.
    pub filename: &'static str,
    /// Audited embedded source, independent of the checkout layout.
    pub bytes: &'static [u8],
    /// Python startup requires a dedicated virtual environment's site-packages.
    pub python: bool,
}

/// Select a supported CLI bootstrap without installing or launching it.
///
/// # Errors
/// Reject SDK factories and native applications without a verified startup seam.
pub fn asset(_adapter: &str) -> Result<Asset> {
    Err("startup setup requires a configured native adapter".into())
}
