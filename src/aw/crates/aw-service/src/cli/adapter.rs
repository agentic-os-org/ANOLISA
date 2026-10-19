//! CLI-owned binding and persistent-install orchestration.

use super::{read_file, Arguments, Exit, Result};
pub(super) use aw_adapter::*;
use aw_service::Binding;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HookBinding {
    pub adapter: String,
    pub binding: Binding,
    pub socket: PathBuf,
    pub cwd: PathBuf,
    pub events: BTreeMap<String, HookEvent>,
}

impl HookBinding {
    pub(super) fn native_context(&self) -> NativeContext {
        NativeContext {
            target: self.binding.target.clone(),
            instance_id: self.binding.instance_id.clone(),
            cwd: self.cwd.clone(),
        }
    }
}

pub(super) fn install(args: &Arguments) -> Result<Exit> {
    args.check(&["--config", "--agent", "--native-profile"], false)?;
    let bytes = read_file(args.required("--config")?, 4 * 1024 * 1024)?;
    let config = aw_config::Validator::new()?.parse(&bytes)?;
    let target = args.required("--agent")?;
    let name = config.as_value()["spec"]["agents"][target]["adapter"]
        .as_str()
        .ok_or("configured Agent target not found")?;
    let adapter = select(name)?;
    super::run::install_signals()?;
    let options = InstallOptions {
        target: target.into(),
        profile: args.flags.get("--native-profile").cloned(),
    };
    match adapter.install(&options, &config, &super::run::CANCELLED) {
        Ok(()) => Ok(super::run::interrupted().unwrap_or(Exit::Code(0))),
        Err(error) => super::run::interrupted_error(error.into()),
    }
}
