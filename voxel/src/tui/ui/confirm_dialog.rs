// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The confirmation overlay, with the options and shortcuts each
//! confirmation offers.

use super::{
    popup::{Button, Popup},
    widgets::overlay_area,
};
use crate::tui::{
    App,
    event::{Confirmation, ConfirmationOptionAction},
};
use ratatui::text::{Line, Text};

fn prompt(confirmation: &Confirmation) -> &'static str {
    match confirmation {
        Confirmation::Quit => "Quit Voxel TUI?",
    }
}

fn popup(app: &App, confirmation: &Confirmation) -> Popup<'static> {
    let selected = app.session.confirmation_selection;
    let buttons = confirmation
        .options()
        .iter()
        .enumerate()
        .map(|(index, option)| Button {
            label: option.label,
            key: match option.action {
                ConfirmationOptionAction::Confirm => None,
                ConfirmationOptionAction::Reject => Some("n"),
            },
            selected: index == selected,
        })
        .collect();
    Popup {
        header: Line::from(prompt(confirmation)),
        body: Text::default(),
        buttons,
    }
}

pub fn draw(frame: &mut ratatui::Frame<'_>, app: &App) {
    let Some(confirmation) = app.session.confirmation.as_ref() else {
        return;
    };
    popup(app, confirmation).render(frame, overlay_area(frame.area()), 0);
}
