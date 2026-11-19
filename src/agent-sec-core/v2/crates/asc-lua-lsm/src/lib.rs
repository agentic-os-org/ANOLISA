//! Compatibility exports and integration-test entry point for the Lua-LSM backend.
#![forbid(unsafe_code)]

pub use asc_lua_lsm_client::{LuaLsmClient, NativeSecurityFs, SecurityFs};
pub use asc_lua_lsm_protocol::{
    DEFAULT_STATE_DIRECTORY, LUA_LSM_PLAN_FORMAT, LUA_LSM_ROUTE, LuaLsmPlan, module_name,
};
pub use asc_policy_translator_lua_lsm::LuaLsmTranslator;
