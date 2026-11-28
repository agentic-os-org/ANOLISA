//! Profiles compiled into this build; configuration never resolves filesystem paths.

use super::{InstrumentationProfile, Origin, Runtime};
use crate::Error;
use std::{collections::BTreeSet, sync::OnceLock};

const DOCUMENTS: &[&[u8]] = &[
    #[cfg(feature = "profile-fixtures")]
    include_bytes!("builtin/fixture.python.json"),
    #[cfg(feature = "profile-fixtures")]
    include_bytes!("builtin/fixture.second.json"),
    #[cfg(feature = "profile-fixtures")]
    include_bytes!("builtin/fixture.js.json"),
];

/// Maximum number of profiles bound to one application process.
pub const MAX_PROFILES: usize = 16;

/// Selects compiled builtin profiles, validating their combined process binding.
///
/// # Errors
/// Rejects unknown or duplicate IDs, mixed runtimes and overlapping seams.
pub fn select<'a>(
    ids: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<&'static InstrumentationProfile>, &'static str> {
    static REGISTRY: OnceLock<Result<Vec<InstrumentationProfile>, Error>> = OnceLock::new();
    let registry = REGISTRY
        .get_or_init(|| {
            DOCUMENTS
                .iter()
                .map(|bytes| InstrumentationProfile::parse(bytes))
                .collect()
        })
        .as_ref()
        .map_err(|_| "invalid bundled instrumentation profile catalogue")?;
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    let mut seams = BTreeSet::new();
    let mut runtime: Option<Runtime> = None;
    for id in ids {
        if !seen.insert(id) || seen.len() > MAX_PROFILES {
            return Err("duplicate or too many builtin instrumentation profiles");
        }
        let profile = registry
            .iter()
            .find(|p| p.id() == id && p.origin() == Origin::Builtin)
            .ok_or("unknown builtin instrumentation profile")?;
        if runtime
            .replace(profile.runtime())
            .is_some_and(|previous| previous != profile.runtime())
        {
            return Err("instrumentation profiles must use one runtime per target");
        }
        if !profile
            .targets()
            .iter()
            .all(|t| seams.insert((t.module(), t.qualname())))
        {
            return Err("instrumentation profiles overlap on a target seam");
        }
        selected.push(profile);
    }
    selected.sort_by_key(|profile| profile.id());
    Ok(selected)
}
