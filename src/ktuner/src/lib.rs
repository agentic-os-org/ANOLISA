pub mod bench;
pub mod category;
pub mod detect;
pub mod profile;
pub mod rules;
pub mod services;
pub mod tuner;

pub use detect::{gather_system_info, RuntimeEnv, SystemInfo};
pub use profile::{classify, WorkloadType};
pub use rules::{evaluate, Category, Confidence, EvalResult, Recommendation};
pub use tuner::{
    apply, apply_one, apply_quiet, auto_rollback_on_degradation, classify_rollback,
    is_forbidden_param, param_to_path, rollback, rollback_preview, rollback_quiet, RollbackOutcome,
    RollbackStatus,
};
