//! Interactive guide wizard entry, kept in the binary target so it can read
//! the login and auth owners without pulling them into the library.
//!
//! Every wizard action stays inside this process: settings save in place and
//! AI management runs as a finite auth task. No action starts a working shell.

use crate::config::guide::UserPaths;
use crate::config::{detect_language_from_env, read_guidance_language};
use crate::i18n::I18n;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

mod ai;
pub(crate) mod core;
mod handoff;
pub(crate) mod input;
mod login;
mod navigation;
pub(crate) mod result;
mod session;
mod settings;
mod status;
#[cfg(test)]
mod tests;
mod tips;

use input::{GuideInput, Input};

enum LoopExit {
    Status(i32),
    Signal(i32),
    InputLost,
}

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
    crate::runtime::terminal::guide_loop_begin();
    let mut session = session::Session::new(paths, I18n::new(language));
    let mut guide_input = GuideInput::new();
    session.start_ai_load();
    // The outer scope keeps the summary worker and any task child owned until
    // they are reaped, including when the loop unwinds.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_loop(&mut session, &mut guide_input)
    }));
    session.settle_ai_load();
    crate::runtime::terminal::guide_loop_end();
    match outcome {
        Ok(LoopExit::Status(status)) => {
            // A termination request that arrived with the last keypress still
            // decides how this process ends.
            match crate::runtime::terminal::take_guide_signal() {
                Some(signal) => crate::runtime::terminal::exit_with_signal(signal),
                None => status,
            }
        }
        Ok(LoopExit::Signal(signal)) => crate::runtime::terminal::exit_with_signal(signal),
        Ok(LoopExit::InputLost) => {
            let notice =
                I18n::new(session.language()).t(crate::i18n::MessageId::GuideWizardInputLost);
            println!("{notice}");
            1
        }
        Err(_) => {
            tracing::warn!(
                operation = "guide",
                stage = "loop",
                result = "failed",
                "guide operation failed"
            );
            1
        }
    }
}

fn run_loop(session: &mut session::Session, guide_input: &mut GuideInput) -> LoopExit {
    let mut screen = match enter_screen() {
        Ok(screen) => screen,
        Err(_) => return LoopExit::Status(1),
    };
    let mut dirty = true;
    loop {
        if let Some(signal) = crate::runtime::terminal::take_guide_signal() {
            return LoopExit::Signal(signal);
        }
        if dirty {
            let view = session.view();
            match screen.draw(&view) {
                Ok(scroll) => {
                    session.scroll = scroll;
                }
                Err(error) => {
                    tracing::warn!(operation = "terminal", stage = "draw", kind = ?error.kind(), "guide operation failed");
                    return LoopExit::Status(1);
                }
            }
            dirty = false;
        }
        match guide_input.next() {
            Input::Key(key) => {
                let previous = session.message.clone();
                // The result keeps its tail in view until the user acts again.
                session.message_fresh = false;
                let flow = if screen.too_small() {
                    too_small_flow(key)
                } else {
                    session.handle(key)
                };
                match flow {
                    session::Flow::Exit => return LoopExit::Status(0),
                    session::Flow::Ask => {
                        drop(screen);
                        return LoopExit::Status(handoff::start_questions());
                    }
                    session::Flow::AiManage => {
                        let frame = session.current_frame();
                        session.settle_ai_load();
                        drop(screen);
                        guide_input.discard_pending();
                        let exit = run_auth_task(session, guide_input);
                        guide_input.discard_pending();
                        if exit.terminal_lost {
                            session.finish_ai_task(exit.outcome);
                            return LoopExit::InputLost;
                        }
                        if let Some(signal) = exit.terminate {
                            session.finish_ai_task(exit.outcome);
                            return LoopExit::Signal(signal);
                        }
                        screen = match enter_screen() {
                            Ok(screen) => screen,
                            Err(_) => return LoopExit::Status(1),
                        };
                        session.restore_frame(frame);
                        session.finish_ai_task(exit.outcome);
                    }
                    session::Flow::LoginOther | session::Flow::LoginReread => {
                        session.run_login(login_action(&flow));
                    }
                    session::Flow::LoginSet | session::Flow::LoginRestore => {
                        let action = login_action(&flow);
                        drop(screen);
                        session.run_login(action);
                        screen = match enter_screen() {
                            Ok(screen) => screen,
                            Err(_) => return LoopExit::Status(1),
                        };
                        if let Some(signal) = crate::runtime::terminal::take_guide_signal() {
                            return LoopExit::Signal(signal);
                        }
                    }
                    session::Flow::Continue => {}
                }
                session.refresh_message_focus(&previous);
                dirty = true;
            }
            Input::Resize => dirty = true,
            Input::Tick => {
                if session.tick() {
                    dirty = true;
                }
            }
            Input::Lost => return LoopExit::InputLost,
        }
    }
}

fn login_action(flow: &session::Flow) -> &'static str {
    match flow {
        session::Flow::LoginSet => "set",
        session::Flow::LoginRestore => "restore",
        session::Flow::LoginOther => "other",
        session::Flow::LoginReread => "reread",
        _ => "",
    }
}

fn run_auth_task(
    session: &session::Session,
    guide_input: &mut GuideInput,
) -> crate::auth::standalone::TaskExit {
    let i18n = I18n::new(session.language());
    let program = match core::admit() {
        Ok(program) => program,
        Err(error) => {
            let message = match error {
                core::AdmissionError::Missing => i18n.format(
                    crate::i18n::MessageId::GuideWizardAiUnavailable,
                    &[("error", "cosh-core not found")],
                ),
                core::AdmissionError::Untrusted => i18n
                    .t(crate::i18n::MessageId::GuideWizardAiCoreUntrusted)
                    .to_string(),
            };
            let mut outcome = result::TaskOutcome::default();
            outcome.reason = result::EndReason::Failed;
            outcome.error = Some(message);
            return crate::auth::standalone::TaskExit {
                outcome,
                terminal_lost: false,
                terminate: None,
            };
        }
    };
    let adapter = crate::adapter::AdapterInstance::CoshCore(
        crate::adapter::CoshCoreAdapter::new(program, false).with_guarded_auth_writes(),
    );
    crate::auth::standalone::run(guide_input, &adapter, session.language())
}
