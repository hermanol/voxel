// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The root layout and shared widgets: the overlay area, the action guidance
//! bar, and the theme's styles and blocks.

use super::{
    colors::*,
    renderer::{LayoutMode, MIN_HEIGHT, MIN_WIDTH, layout_mode},
};
use crate::tui::app::App;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

pub fn draw(frame: &mut ratatui::Frame<'_>, app: &App) {
    // Like wicket, paint the theme's background rather than the terminal's.
    frame.render_widget(Block::default().style(background()), frame.area());
    if let Some(splash) = &app.splash
        && super::splash::draw(frame, splash, app.now)
    {
        return;
    }
    let area = frame.area();
    let root = root_layout(area);
    if root.mode == LayoutMode::Minimum {
        minimum(frame, area);
        return;
    }
    frame.render_widget(
        Block::default()
            .borders(Borders::BOTTOM)
            .title(" VOXEL ")
            .border_style(line_style(true)),
        root.header,
    );
    frame.render_widget(
        Paragraph::new(guidance_lines(app, root.footer.width)),
        root.footer,
    );
    if app.session.help_open {
        super::help::draw(frame, app);
    }
    if app.session.confirmation.is_some() {
        super::confirm_dialog::draw(frame, app);
    }
}

pub(crate) fn overlay_area(area: Rect) -> Rect {
    root_layout(area).content
}

struct RootLayout {
    header: Rect,
    content: Rect,
    footer: Rect,
    mode: LayoutMode,
}

fn root_layout(area: Rect) -> RootLayout {
    let mode = layout_mode(area);
    if mode == LayoutMode::Minimum {
        return RootLayout {
            header: Rect::default(),
            content: Rect::default(),
            footer: Rect::default(),
            mode,
        };
    }
    let footer_height = if area.width >= 160 {
        1
    } else if area.width >= 100 {
        2
    } else {
        3
    };
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(footer_height),
    ])
    .split(area);
    RootLayout { header: rows[0], content: rows[1], footer: rows[2], mode }
}

#[derive(Clone, Copy)]
struct ActionGroup {
    key: &'static str,
    description: &'static str,
}

fn guidance_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let groups = action_groups(app);
    let row_count = if width >= 160 {
        1
    } else if width >= 100 {
        2
    } else {
        3
    };
    let mut rows = vec![Vec::new(); row_count];
    let mut row = 0;
    for group in groups {
        let mut candidate = rows[row].clone();
        candidate.push(group);
        if row + 1 < row_count
            && styled_groups(candidate).width() > width as usize
        {
            row += 1;
        }
        rows[row].push(group);
    }
    rows.into_iter().map(styled_groups).collect()
}

fn styled_groups(groups: Vec<ActionGroup>) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, group) in groups.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        if !group.description.is_empty() {
            spans.push(Span::styled(
                format!("{} ", group.description),
                help_function_style(),
            ));
        }
        spans.push(Span::styled(format!("<{}>", group.key), help_key_style()));
    }
    Line::from(spans)
}

fn action_groups(app: &App) -> Vec<ActionGroup> {
    let group = |key, description| ActionGroup { key, description };
    let lifecycle = || vec![group("q", "quit")];
    if app.session.confirmation.is_some() {
        return vec![
            group("↑/↓", "move"),
            group("Enter", "select"),
            group("n/Esc", "back"),
        ];
    }
    if app.session.help_open {
        let mut groups = vec![group("↑/↓", "scroll"), group("?/Esc", "close")];
        groups.extend(lifecycle());
        return groups;
    }
    let mut groups = lifecycle();
    groups.push(group("?", "help"));
    groups
}

fn minimum(frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect) {
    frame.render_widget(
        Paragraph::new(format!(
            "Terminal too small: {}x{}; required at least {}x{}",
            area.width, area.height, MIN_WIDTH, MIN_HEIGHT
        ))
        .block(primary_block(" VOXEL ")),
        area,
    );
}

// The styles below follow wicket's ui/defaults/style.rs.

/// The theme background painted under every frame.
pub(crate) fn background() -> Style {
    Style::default().bg(TUI_BLACK)
}

/// Border lines: dark green where focus is, grey elsewhere.
pub(crate) fn line_style(focused: bool) -> Style {
    Style::default().fg(if focused { TUI_GREEN_DARK } else { TUI_GREY })
}

/// Titles and active tabs: bright green where focus is, grey elsewhere.
pub(crate) fn header_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(TUI_GREEN).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(TUI_GREY)
    }
}

pub(crate) fn help_function_style() -> Style {
    Style::default().fg(TUI_GREEN)
}

pub(crate) fn help_key_style() -> Style {
    Style::default().fg(TUI_GREEN_DARK)
}

pub(crate) fn rounded_block<'a>() -> Block<'a> {
    Block::bordered().border_type(BorderType::Rounded)
}

/// Prepares a pop-up: fades the screen behind it, as wicket does, and
/// clears the pop-up's own area to the theme background.
pub(crate) fn popup_backdrop(frame: &mut ratatui::Frame<'_>, area: Rect) {
    let screen = frame.area();
    frame
        .buffer_mut()
        .set_style(screen, Style::default().bg(TUI_BLACK).fg(TUI_GREY));
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(background()), area);
}

pub(crate) fn primary_block<'a>(title: impl Into<Line<'a>>) -> Block<'a> {
    rounded_block().title(title).border_style(line_style(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::{
        event::Confirmation, terminal::key_action, test_support::key_codes,
    };
    use crossterm::event::{KeyEvent, KeyModifiers};

    /// Every key a footer advertises reaches an action, and each state keeps
    /// the group users find their way by. A retired or mistyped key fails the
    /// first check.
    #[test]
    fn footer_advertises_only_keys_that_act() {
        let app = |setup: &dyn Fn(&mut App)| {
            let mut app = App::new();
            setup(&mut app);
            app
        };
        // (case, app, group shown)
        let cases: [(&str, App, &str); 3] = [
            ("idle", app(&|_| {}), "q quit"),
            ("help", app(&|app| app.session.help_open = true), "?/Esc close"),
            (
                "confirmation",
                app(&|app| app.session.confirmation = Some(Confirmation::Quit)),
                "Enter select",
            ),
        ];
        for (case, app, shown) in cases {
            for group in action_groups(&app) {
                for code in key_codes(group.key) {
                    assert!(
                        key_action(KeyEvent::new(code, KeyModifiers::NONE))
                            .is_some(),
                        "{case}: footer advertises {:?}, which no key binds",
                        group.key
                    );
                }
            }
            let groups = action_groups(&app)
                .into_iter()
                .map(|group| format!("{} {}", group.key, group.description))
                .collect::<Vec<_>>();
            assert!(
                groups.iter().any(|group| group == shown),
                "{case}: footer lacks {shown:?} in {groups:?}"
            );
        }
    }
}
