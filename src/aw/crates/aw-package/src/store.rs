//! Private implementation modules behind the crate's existing package API.

pub mod database;
pub mod description;
pub(crate) mod filesystem;
pub(crate) mod install;
pub(crate) mod manifest;
pub mod open_bundle;
pub mod plan;
pub mod transaction;
