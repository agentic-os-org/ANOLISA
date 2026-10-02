//! Public facade for legacy and version 1 audit contracts.

mod event;
mod validation;

pub use event::*;
#[cfg(test)]
mod tests;
