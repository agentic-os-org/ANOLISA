//! Registration must preserve a supported removal path before closing historical writers.

use super::{Action, PackagePlan};
use crate::database::Installation;

pub(super) fn check(plan: &mut PackagePlan, installation: &Installation) {
    let Some(core) = installation
        .packages
        .iter()
        .find(|package| package.package_id == "aw-core")
    else {
        return;
    };
    let known = core.files.get("bin/aw").is_some_and(|metadata| {
        std::env::current_exe()
            .ok()
            .and_then(|path| crate::digest(&path).ok())
            .is_some_and(|digest| digest == metadata.sha256)
    });
    if !known {
        let message = match plan.action {
            Action::Install => "this prefix contains an older AW executable whose running references cannot be checked; registration would disable its historical installer without a supported removal path; preserve this prefix and install the new build into a separate empty prefix",
            Action::Uninstall => "this prefix contains an older AW executable whose running references cannot be checked; preserve it and install the new build into a separate empty prefix until runtime migration is explicitly supported",
        };
        plan.problem(
            "runtime_unknown",
            message,
            Some("aw-core"),
            Some(installation.prefix.clone()),
        );
    }
}
