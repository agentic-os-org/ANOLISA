//! Terminal lifecycle for the full-screen guide wizard.

use std::io::{self, Stdout};

use ratatui::{
    backend::CrosstermBackend,
    crossterm::{
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    },
    Terminal,
};

use super::view::{self, WizardView};
use super::ScreenEvent;

pub(crate) struct Screen {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    lifecycle: fn(ScreenEvent) -> bool,
}

impl Screen {
    pub(crate) fn enter(lifecycle: fn(ScreenEvent) -> bool) -> io::Result<Self> {
        if !lifecycle(ScreenEvent::Entering) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if let Err(error) = enable_raw_mode() {
            lifecycle(ScreenEvent::Inactive);
            return Err(error);
        }
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            let _ = execute!(stdout, LeaveAlternateScreen);
            let _ = disable_raw_mode();
            lifecycle(ScreenEvent::Inactive);
            return Err(error);
        }
        let terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = execute!(io::stdout(), LeaveAlternateScreen);
                let _ = disable_raw_mode();
                lifecycle(ScreenEvent::Inactive);
                return Err(error);
            }
        };
        // A finite task may have drawn on this screen while the wizard was
        // suspended. Ratatui skips blank cells, so start from a cleared canvas
        // instead of leaving the task's card behind the page.
        let mut terminal = terminal;
        terminal.clear()?;
        if !lifecycle(ScreenEvent::Active) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        Ok(Self {
            terminal,
            lifecycle,
        })
    }

    pub(crate) fn draw(&mut self, view: &WizardView) -> io::Result<usize> {
        if !(self.lifecycle)(ScreenEvent::Drawing) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let mut scroll = view.scroll;
        let result = self.terminal.draw(|frame| {
            scroll = view::render(frame, view);
        });
        if !(self.lifecycle)(ScreenEvent::Active) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        result?;
        Ok(scroll)
    }

    pub(crate) fn too_small(&self) -> bool {
        match self.terminal.size() {
            Ok(size) => size.width < view::MIN_COLUMNS || size.height < view::MIN_ROWS,
            Err(_) => true,
        }
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        if !(self.lifecycle)(ScreenEvent::Leaving) {
            return;
        }
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
        (self.lifecycle)(ScreenEvent::Inactive);
    }
}
