//! Interactive guide wizard entry, kept in the binary target so it can read
//! the login and auth owners without pulling them into the library.

use crate::config::guide::UserPaths;
use crate::config::{detect_language_from_env, read_guidance_language};
use crate::i18n::I18n;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use login::LoginAction;

mod handoff;
mod login;
mod session;
mod status;
#[cfg(test)]
mod tests;
mod tips;

fn too_small_flow(key: KeyEvent) -> session::Flow {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => session::Flow::Exit,
        KeyCode::Char('c') | KeyCode::Char('d')
            if key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            session::Flow::Exit
        }
        _ => session::Flow::Continue,
    }
}

fn enter_screen() -> std::io::Result<crate::ui::guide::Screen> {
    crate::ui::guide::Screen::enter(crate::runtime::terminal::guide_screen_event).inspect_err(|error| {
        tracing::warn!(operation = "terminal", stage = "enter", kind = ?error.kind(), "guide operation failed");
    })
}

pub(crate) fn run_interactive() -> i32 {
    let language = read_guidance_language().unwrap_or_else(|_| detect_language_from_env());
    let paths = UserPaths::from_env();
    crate::runtime::terminal::install_terminal_recovery();
    let mut session = session::Session::new(paths, I18n::new(language));
    session.start_ai_load();
    let mut screen = match enter_screen() {
        Ok(screen) => screen,
        Err(_) => return 1,
    };
    let mut dirty = true;
    loop {
        if dirty {
            let view = session.view();
            match screen.draw(&view) {
                Ok(scroll) => session.scroll = scroll,
                Err(error) => {
                    tracing::warn!(operation = "terminal", stage = "draw", kind = ?error.kind(), "guide operation failed");
                    return 1;
                }
            }
            dirty = false;
        }
        match screen.next_input() {
            Ok(crate::ui::guide::Input::Key(key)) => {
                let flow = if screen.too_small() {
                    too_small_flow(key)
                } else {
                    session.handle(key)
                };
                match flow {
                    session::Flow::Exit => return 0,
                    session::Flow::Handoff => {
                        drop(screen);
                        let notice = I18n::new(session.language())
                            .t(crate::i18n::MessageId::GuideWizardAiHandoff);
                        println!("{notice}");
                        let result = handoff::run();
                        screen = match enter_screen() {
                            Ok(screen) => screen,
                            Err(_) => return 1,
                        };
                        session.finish_handoff(result);
                    }
                    session::Flow::LoginSet | session::Flow::LoginRestore => {
                        let action = match flow {
                            session::Flow::LoginSet => LoginAction::Set,
                            _ => LoginAction::Restore,
                        };
                        drop(screen);
                        let notice = I18n::new(session.language())
                            .t(crate::i18n::MessageId::GuideWizardLoginHandoff);
                        println!("{notice}");
                        session.run_login(action);
                        screen = match enter_screen() {
                            Ok(screen) => screen,
                            Err(_) => return 1,
                        };
                    }
                    session::Flow::Continue => {}
                }
                dirty = true;
            }
            Ok(crate::ui::guide::Input::Resize) => dirty = true,
            Ok(crate::ui::guide::Input::Tick) => {
                if session.tick() {
                    dirty = true;
                }
            }
            Err(error) => {
                tracing::warn!(operation = "terminal", stage = "input", kind = ?error.kind(), "guide operation failed");
                return 1;
            }
        }
    }
}
