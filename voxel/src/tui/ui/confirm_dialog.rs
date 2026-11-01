// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The confirmation overlay for launch, route, quit and destroy requests,
//! with the options and shortcuts each confirmation offers.

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
        Confirmation::Launch => "Launch deployment?",
        Confirmation::Route => "Apply routes?",
        Confirmation::Quit => "Quit Voxel TUI?",
        Confirmation::QuitAndDestroy => "Quit and destroy the deployment?",
        Confirmation::CancelAndDestroy => {
            "Stop the operation and destroy the deployment?"
        }
        Confirmation::ForceStop => "Force stop the running command?",
    }
}

fn body(confirmation: &Confirmation) -> Text<'static> {
    let mut lines = Vec::new();
    if matches!(confirmation, Confirmation::CancelAndDestroy) {
        lines.push(Line::from(
            "The running command is allowed to finish safely first, so this can take a moment.",
        ));
    }
    if matches!(confirmation, Confirmation::ForceStop) {
        lines.push(Line::from(
            "This can leave the deployment half-built. Use it only when the command will not stop on its own.",
        ));
    }
    Text::from(lines)
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
        body: body(confirmation),
        buttons,
    }
}

pub fn draw(frame: &mut ratatui::Frame<'_>, app: &App) {
    let Some(confirmation) = app.session.confirmation.as_ref() else {
        return;
    };
    popup(app, confirmation).render(frame, overlay_area(frame.area()), 0);
}
