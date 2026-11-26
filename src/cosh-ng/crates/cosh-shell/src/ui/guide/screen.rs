//! Terminal lifecycle for the full-screen guide wizard.

use std::io::{self, Stdout};

use ratatui::{
    backend::CrosstermBackend,
    crossterm::{
        cursor::{Hide, Show},
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    },
    Terminal,
};

use super::view::{self, DrawnView, WizardView};
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

    pub(crate) fn draw(&mut self, view: &WizardView) -> io::Result<DrawnView> {
        if !(self.lifecycle)(ScreenEvent::Drawing) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let mut drawn = DrawnView {
            scroll: view.scroll,
            review: view.review,
        };
        let result = self.terminal.draw(|frame| {
            drawn = view::render(frame, view);
        });
        if !(self.lifecycle)(ScreenEvent::Active) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        result?;
        Ok(drawn)
    }

    /// A hosted task drew straight to this screen, so the cached buffer no longer describes it.
    pub(crate) fn resync(&mut self) -> io::Result<()> {
        self.terminal.clear()
    }

    /// Lends the tty to a foreground program without giving up the alternate screen.
    pub(crate) fn suspend(&mut self) -> io::Result<()> {
        disable_raw_mode()?;
        execute!(self.terminal.backend_mut(), Show)
    }

    /// Takes the tty back after a foreground program owned it in canonical mode.
    pub(crate) fn resume(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        let cleared = self.terminal.clear();
        execute!(self.terminal.backend_mut(), Hide)?;
        cleared
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
