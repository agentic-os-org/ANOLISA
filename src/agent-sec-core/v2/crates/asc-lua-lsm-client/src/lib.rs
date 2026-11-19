//! Native securityfs deployment and local activation of Lua-LSM artifacts.

#![forbid(unsafe_code)]

mod client;
mod securityfs;

pub use client::LuaLsmClient;
pub use securityfs::{NativeSecurityFs, SecurityFs};
