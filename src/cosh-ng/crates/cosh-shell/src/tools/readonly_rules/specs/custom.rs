use super::{ReadonlySpec, Validator};
use crate::tools::readonly_rules::validators;

// ── Custom validators (structural complexity) ──
pub(super) const HEAD: ReadonlySpec = ReadonlySpec {
    command: "head",
    validator: Validator::Custom(validators::is_readonly_head),
};

pub(super) const TAIL: ReadonlySpec = ReadonlySpec {
    command: "tail",
    validator: Validator::Custom(validators::is_readonly_tail),
};

pub(super) const GREP: ReadonlySpec = ReadonlySpec {
    command: "grep",
    validator: Validator::Custom(validators::is_readonly_grep),
};

pub(super) const RG: ReadonlySpec = ReadonlySpec {
    command: "rg",
    validator: Validator::Custom(validators::is_readonly_rg),
};

pub(super) const FIND: ReadonlySpec = ReadonlySpec {
    command: "find",
    validator: Validator::Custom(validators::is_readonly_find),
};

pub(super) const PS: ReadonlySpec = ReadonlySpec {
    command: "ps",
    validator: Validator::Custom(validators::is_readonly_ps),
};

pub(super) const SYSCTL: ReadonlySpec = ReadonlySpec {
    command: "sysctl",
    validator: Validator::Custom(validators::is_readonly_sysctl),
};

pub(super) const TOP: ReadonlySpec = ReadonlySpec {
    command: "top",
    validator: Validator::Custom(validators::is_bounded_top_snapshot),
};

pub(super) const ENV: ReadonlySpec = ReadonlySpec {
    command: "env",
    validator: Validator::Custom(validators::is_readonly_env),
};
