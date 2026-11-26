//! Full-screen guide wizard: terminal lifecycle and single-column views.

mod screen;
mod view;

pub(crate) use screen::Screen;
pub(crate) use view::{
    render, ConfirmationReview, DrawnView, WizardRow, WizardView, CONTENT_WIDTH, MIN_COLUMNS,
    MIN_ROWS,
};

#[derive(Clone, Copy)]
pub(crate) enum ScreenEvent {
    Entering,
    Drawing,
    Active,
    Leaving,
    Inactive,
}
