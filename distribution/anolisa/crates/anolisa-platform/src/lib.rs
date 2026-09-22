//! Platform-facing helpers for ANOLISA install layout and OS integration.
//!
//! This crate stays below the CLI/core orchestration layers: it resolves
//! filesystem roots and provides thin bridges to host package/service
//! managers without importing CLI vocabulary.

pub mod command;
pub mod fs_layout;
pub mod ipc;
pub mod package_manager;
pub mod pkg_files;
pub mod pkg_query;
pub mod pkg_transaction;
pub mod privilege;
pub mod rpm_metadata;
pub mod rpm_query;
pub mod rpm_repo;
pub mod rpm_select;
pub mod rpm_tool;
pub mod rpm_transaction;
pub mod systemd;
