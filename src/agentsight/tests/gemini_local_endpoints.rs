// Compile the desktop handlers on Linux and test their real HTTP services.
#[allow(clippy::collapsible_if)] // Existing desktop-only converter fallback.
#[path = "../src/local/collector/converter.rs"]
pub(crate) mod converter;
#[path = "../src/local/collector/discovery.rs"]
pub(crate) mod discovery;
mod local {
    pub(crate) mod collector {
        pub(crate) use crate::converter;
        pub(crate) use crate::converter::convert_jsonl_to_atif;
        pub(crate) use crate::discovery::{LocalSessionsResponse, discover_local_sessions};
    }
}
#[path = "../src/local/server/local_sessions.rs"]
mod local_sessions;
