use aw_contracts::{canonical, Registry};
use serde_json::Value;
use std::sync::LazyLock;

pub static REGISTRY: LazyLock<Registry> = LazyLock::new(|| Registry::new().unwrap());

pub fn fixtures() -> Value {
    canonical::parse(include_bytes!("../fixtures/contracts.json")).unwrap()
}
