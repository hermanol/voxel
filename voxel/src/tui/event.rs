// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The vocabulary shared by input, background tasks and the
//! [`App`](super::App) state machine: key actions, confirmations and their
//! options, effects and application events.

use std::time::Instant;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Confirmation {
    Quit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfirmationOptionAction {
    Confirm,
    Reject,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfirmationOption {
    pub label: &'static str,
    pub action: ConfirmationOptionAction,
}

use ConfirmationOptionAction::*;
const QUIT_OPTIONS: &[ConfirmationOption] = &[
    ConfirmationOption { label: "Quit Voxel TUI", action: Confirm },
    ConfirmationOption { label: "Back", action: Reject },
];
impl Confirmation {
    pub fn options(&self) -> &'static [ConfirmationOption] {
        match self {
            Self::Quit => QUIT_OPTIONS,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    Quit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Activate,
    ToggleHelp,
    Close,
    RequestQuit,
    Reject,
    Scroll { delta: isize, page: bool },
}

#[derive(Clone, Debug)]
pub enum AppEvent {
    Action(Action),
    Tick { now: Instant },
    Resize { width: u16, height: u16 },
}
