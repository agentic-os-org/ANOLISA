//! Primary commands — component lifecycle and operations.

pub mod adopt;
pub mod bug;
pub(crate) mod component_observation;
pub mod doctor;
pub mod env;
pub mod forget;
pub mod install;
pub mod list;
pub mod logs;
#[cfg(test)]
mod observation_conformance;
pub(crate) mod recovery;
pub mod repair;
pub mod restart;
mod rpm_backends;
pub(crate) mod rpm_install;
pub mod status;
pub mod uninstall;
pub mod update;
pub mod upgrade;
