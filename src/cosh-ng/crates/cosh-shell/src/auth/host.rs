//! Which surface runs the auth flow. The shell keeps its prompt-returning
//! behavior; the guide runs the same state machine as a finite task that
//! returns to the wizard instead of a working shell.

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum AuthHost {
    #[default]
    Shell,
    Guide,
}

impl AuthHost {
    pub(crate) fn is_guide(self) -> bool {
        self == Self::Guide
    }
}
