use super::MessageId;

mod activity;
mod agent;
mod approval;
mod auth;
mod config;
mod debug;
mod health;
mod help;
mod hook_action;
mod hook_details;
mod hooks;
mod insight;
mod modes;
mod question;
mod recommendation;
mod session;
mod startup;

pub(super) fn message(id: MessageId) -> &'static str {
    startup::message(id)
        .or_else(|| help::message(id))
        .or_else(|| config::message(id))
        .or_else(|| hooks::message(id))
        .or_else(|| hook_action::message(id))
        .or_else(|| debug::message(id))
        .or_else(|| modes::message(id))
        .or_else(|| agent::message(id))
        .or_else(|| insight::message(id))
        .or_else(|| hook_details::message(id))
        .or_else(|| activity::message(id))
        .or_else(|| recommendation::message(id))
        .or_else(|| health::message(id))
        .or_else(|| question::message(id))
        .or_else(|| approval::message(id))
        .or_else(|| session::message(id))
        .or_else(|| auth::message(id))
        .unwrap_or_else(|| super::en::message(id))
}
