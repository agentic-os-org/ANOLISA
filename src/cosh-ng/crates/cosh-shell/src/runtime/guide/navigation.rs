//! Source frames for wizard navigation: every return goes back to the page
//! that opened the current one, with the choice the user actually left.

use super::login::LoginChoice;
use super::session::Page;

#[derive(Clone)]
pub(crate) struct Frame {
    pub(crate) page: Page,
    pub(crate) choice: usize,
    pub(crate) scroll: usize,
    /// Login choices are state-dependent, so the semantic action is kept
    /// instead of an ordinal that may address a different action on return.
    pub(crate) login: Option<LoginChoice>,
}

#[derive(Default)]
pub(crate) struct Navigation {
    frames: Vec<Frame>,
}

impl Navigation {
    pub(crate) fn push(&mut self, frame: Frame) {
        self.frames.push(frame);
    }

    pub(crate) fn pop(&mut self) -> Option<Frame> {
        self.frames.pop()
    }

    #[cfg(test)]
    pub(crate) fn depth(&self) -> usize {
        self.frames.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(page: Page, choice: usize) -> Frame {
        Frame {
            page,
            choice,
            scroll: 0,
            login: None,
        }
    }

    #[test]
    fn returns_unwind_one_level_at_a_time() {
        let mut navigation = Navigation::default();
        navigation.push(frame(Page::Home, 2));
        navigation.push(frame(Page::Tips, 3));
        assert_eq!(navigation.depth(), 2);
        let tips = navigation.pop().expect("tips frame");
        assert_eq!(tips.page, Page::Tips);
        assert_eq!(tips.choice, 3);
        let home = navigation.pop().expect("home frame");
        assert_eq!(home.page, Page::Home);
        assert_eq!(home.choice, 2);
        assert!(navigation.pop().is_none());
    }

    #[test]
    fn login_frame_keeps_the_semantic_action() {
        let mut navigation = Navigation::default();
        navigation.push(Frame {
            page: Page::Login,
            choice: 1,
            scroll: 0,
            login: Some(LoginChoice::Other),
        });
        assert_eq!(
            navigation.pop().expect("login frame").login,
            Some(LoginChoice::Other)
        );
    }
}
