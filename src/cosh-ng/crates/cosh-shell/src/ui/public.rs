#[allow(dead_code, unused_imports)]
#[path = "agent_render/mod.rs"]
pub(crate) mod agent_render;
#[allow(dead_code)]
pub(crate) mod approval_surface;
mod question;
#[allow(dead_code)]
pub(crate) mod render_surface;
#[path = "renderer.rs"]
mod renderer;
#[allow(dead_code)]
pub(crate) mod surface_frame;
#[allow(dead_code)]
pub(crate) mod worker_protocol;
pub(crate) mod worker_runtime;
pub(crate) mod wrap;

pub(crate) use agent_render::*;
pub(crate) use question::{QuestionPanelPresentation, OPTION_DETAIL_SEPARATOR};
pub(crate) use renderer::render_transcript;
