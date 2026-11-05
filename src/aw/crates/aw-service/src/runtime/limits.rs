//! Concurrent budgets are independent of a session's historical call count.

use super::Runtime;
use crate::{rejected, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Mutex,
    },
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Limits {
    pub(crate) bindings: usize,
    pub(crate) events: usize,
    pub(crate) calls: usize,
    pub(crate) connections: usize,
    pub(crate) invocations_per_binding: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            bindings: 64,
            events: 128,
            calls: 64,
            connections: 128,
            invocations_per_binding: 256,
        }
    }
}

impl Limits {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self> {
        let config = aw_config::Validator::new()?.parse(bytes)?;
        let mut value = config.as_value()["spec"]["daemon"]["limits"].clone();
        if value.is_null() {
            return Ok(Self::default());
        }
        let fields = value
            .as_object_mut()
            .ok_or_else(|| rejected("resource_limits_invalid"))?;
        // JSON Schema accepts integral floats. Validation already bounded each
        // value to 1..4096, so normalize them before typed deserialization.
        for limit in fields.values_mut() {
            let number = limit
                .as_f64()
                .ok_or_else(|| rejected("resource_limits_invalid"))?;
            *limit = json!(number as usize);
        }
        serde_json::from_value(value).map_err(|_| rejected("resource_limits_invalid"))
    }
}

#[derive(Default)]
pub(crate) struct Resources {
    pub(crate) connections: AtomicUsize,
    pub(crate) rejected: AtomicU64,
    pub(crate) peak_invocations: AtomicUsize,
    reasons: Mutex<BTreeMap<&'static str, u64>>,
}

impl Runtime {
    pub(crate) fn overload(&self, code: &'static str) -> crate::Error {
        self.resources.rejected.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut reasons) = self.resources.reasons.lock() {
            *reasons.entry(code).or_default() += 1;
        }
        rejected(code)
    }

    pub(super) fn resource_status(&self) -> Result<Value> {
        let instances = self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        let mut invocations = 0;
        let mut correlations = 0;
        for instance in instances.values().flatten() {
            invocations += instance
                .invocations
                .lock()
                .map_err(|_| rejected("state_poisoned"))?
                .ledger
                .as_ref()
                .map_or(0, |ledger| ledger.active.len());
            correlations += instance
                .hooks
                .lock()
                .map_err(|_| rejected("state_poisoned"))?
                .len();
        }
        Ok(json!({"limits":self.limits, "usage":{
            "bindings":instances.len(), "invocations":invocations, "correlations":correlations,
            "calls":self.calls.load(Ordering::Relaxed),
            "connections":self.resources.connections.load(Ordering::Relaxed)},
            "peak_invocations_per_binding":self.resources.peak_invocations.load(Ordering::Relaxed),
            "admission_rejections":self.resources.rejected.load(Ordering::Relaxed),
            "rejection_reasons":*self.resources.reasons.lock().map_err(|_| rejected("state_poisoned"))?,
            "reserved_control_connections":16, "maximum_steps_per_binding":64}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validated_integral_float_limits_match_integer_limits() {
        let configuration = aw_config::Validator::new()
            .unwrap()
            .parse(include_bytes!(
                "../../../aw-config/examples/aw.minimal.yaml"
            ))
            .unwrap();
        let mut document = configuration.as_value().clone();
        for number in [1.0, 64.0, 4096.0] {
            document["spec"]["daemon"]["limits"] = json!({
                "bindings":number, "events":number, "calls":number,
                "connections":number, "invocations_per_binding":number
            });
            let limits = Limits::parse(&serde_json::to_vec(&document).unwrap()).unwrap();
            assert_eq!(limits.bindings, number as usize);
            assert_eq!(limits.events, number as usize);
            assert_eq!(limits.calls, number as usize);
            assert_eq!(limits.connections, number as usize);
            assert_eq!(limits.invocations_per_binding, number as usize);
        }
    }
}
