// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The TUI state machine. [`App::update`] folds each [`AppEvent`] into the
//! session state and returns the [`Effect`]s the runtime must carry out; it
//! performs no I/O, so the renderer and tests drive it directly.

use std::time::Instant;

use crate::tui::event::{
    Action, AppEvent, Confirmation, ConfirmationOptionAction, Effect,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalSize {
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionExit {
    Quit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionState {
    pub help_scroll: usize,
    pub help_open: bool,
    pub terminal: TerminalSize,
    pub confirmation: Option<Confirmation>,
    pub confirmation_selection: usize,
    pub exit: Option<SessionExit>,
    pub quitting: bool,
}

#[derive(Clone, Debug)]
pub struct App {
    pub session: SessionState,
    pub now: Option<Instant>,
    pub splash: Option<crate::tui::ui::splash::Splash>,
}

impl App {
    pub fn new() -> Self {
        Self {
            session: SessionState {
                help_scroll: 0,
                help_open: false,
                terminal: TerminalSize { width: 0, height: 0 },
                confirmation: None,
                confirmation_selection: 0,
                exit: None,
                quitting: false,
            },
            now: None,
            splash: None,
        }
    }

    pub fn update(&mut self, event: AppEvent) -> Vec<Effect> {
        match event {
            AppEvent::Action(action) => {
                // Any key skips the launch animation, as in wicket.
                if self.splash.take().is_some() {
                    return vec![];
                }
                return self.action(action);
            }
            AppEvent::Tick { now } => {
                self.now = Some(now);
                if self.splash.as_mut().is_some_and(|splash| !splash.tick(now))
                {
                    self.splash = None;
                }
            }
            AppEvent::Resize { width, height } => {
                self.session.terminal = TerminalSize { width, height }
            }
        }
        vec![]
    }

    fn action(&mut self, action: Action) -> Vec<Effect> {
        if let Some(confirmation) = self.session.confirmation.clone() {
            let options = confirmation.options();
            return match action {
                Action::Close => {
                    self.session.confirmation = None;
                    vec![]
                }
                Action::Scroll { delta, .. } => {
                    self.session.confirmation_selection = self
                        .session
                        .confirmation_selection
                        .saturating_add_signed(delta)
                        .min(options.len().saturating_sub(1));
                    vec![]
                }
                Action::Activate => {
                    self.activate_confirmation_option(confirmation)
                }
                Action::Reject => {
                    self.session.confirmation = None;
                    vec![]
                }
                _ => vec![],
            };
        }
        if !self.session.quitting && action == Action::RequestQuit {
            self.open_confirmation(Confirmation::Quit);
            return vec![];
        }
        if self.session.help_open {
            return match action {
                Action::ToggleHelp | Action::Close => {
                    self.session.help_open = false;
                    vec![]
                }
                Action::Scroll { delta, page } => {
                    let amount = if page {
                        crate::tui::ui::help::page_capacity(self)
                    } else {
                        1
                    } as isize;
                    self.session.help_scroll = self
                        .session
                        .help_scroll
                        .saturating_add_signed(delta.saturating_mul(amount));
                    vec![]
                }
                _ => vec![],
            };
        }
        if action == Action::ToggleHelp {
            self.session.help_open = true;
            self.session.help_scroll = 0;
        }
        vec![]
    }
    fn open_confirmation(&mut self, confirmation: Confirmation) {
        self.session.confirmation_selection =
            confirmation.options().len().saturating_sub(1);
        self.session.confirmation = Some(confirmation);
    }
    fn activate_confirmation_option(
        &mut self,
        confirmation: Confirmation,
    ) -> Vec<Effect> {
        let action = confirmation
            .options()
            .get(self.session.confirmation_selection)
            .map(|option| option.action)
            .unwrap_or(ConfirmationOptionAction::Reject);
        match action {
            ConfirmationOptionAction::Confirm => {
                self.session.confirmation = None;
                self.confirm(confirmation)
            }
            ConfirmationOptionAction::Reject => {
                self.session.confirmation = None;
                vec![]
            }
        }
    }
    fn confirm(&mut self, c: Confirmation) -> Vec<Effect> {
        match c {
            Confirmation::Quit if !self.session.quitting => {
                self.session.exit = Some(SessionExit::Quit);
                self.session.quitting = true;
                vec![Effect::Quit]
            }
            _ => vec![],
        }
    }
}
