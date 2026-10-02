//! Strategy implementations — each strategy is a `Detector` consuming the
//! shared extraction plus its own Rust-computed signals.

pub mod confirm_before_act;
pub mod experience_library;
pub mod fact_check;
pub mod requirement_check;
pub mod verify_before_done;
