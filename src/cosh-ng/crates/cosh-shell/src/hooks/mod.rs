pub(crate) mod aggregate;
pub(crate) mod builtin;
pub(crate) mod detector;
#[path = "engine.rs"]
pub(crate) mod engine;
pub(crate) mod feedback;
pub(crate) mod interrupt;
pub(crate) mod linux_memory;
mod loading;
pub(crate) mod model;
pub(crate) mod policy;
pub(crate) mod prelude;
pub(crate) mod presentation;
pub(crate) mod prompt;
pub(crate) mod queue;
pub(crate) mod state;

pub(crate) use engine::{
    BuiltinHook, ExternalHookConfig, ExternalHookSource, HookEngine, HookSourceInfo,
    RegisteredHookInfo,
};
pub(crate) use feedback::load_hook_feedback_preferences;
pub(crate) use loading::{
    dirs_for_hook_loading, is_trusted_project_root, project_hook_root_from_cwd,
};
pub(crate) use presentation::render_consultation_details;
