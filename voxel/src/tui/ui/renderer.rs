// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The render loop. Chooses a layout mode from the terminal size, limits
//! redraws to a frame budget, and feeds events to the [`App`].

use crate::tui::{App, AppEvent, Effect};
use ratatui::{Terminal, backend::Backend, layout::Rect};
use std::{
    io,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const MIN_WIDTH: u16 = 48;
pub const MIN_HEIGHT: u16 = 16;
pub const WIDE_WIDTH: u16 = 100;
pub const WIDE_HEIGHT: u16 = 22;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutMode {
    Minimum,
    Compact,
    Wide,
}
pub fn layout_mode(area: Rect) -> LayoutMode {
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        LayoutMode::Minimum
    } else if area.width < WIDE_WIDTH || area.height < WIDE_HEIGHT {
        LayoutMode::Compact
    } else {
        LayoutMode::Wide
    }
}

pub fn render_frame(frame: &mut ratatui::Frame<'_>, app: &App) {
    crate::tui::ui::widgets::draw(frame, app);
}

pub struct Renderer {
    minimum_interval: Duration,
    last_draw: Option<Instant>,
}
impl Renderer {
    pub fn new(minimum_interval: Duration) -> Self {
        Self { minimum_interval, last_draw: None }
    }
    pub fn draw<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        app: &App,
        force: bool,
    ) -> io::Result<bool> {
        self.draw_at(terminal, app, force, Instant::now())
    }
    pub fn draw_at<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        app: &App,
        force: bool,
        now: Instant,
    ) -> io::Result<bool> {
        if !force
            && self.last_draw.is_some_and(|last| {
                now.saturating_duration_since(last) < self.minimum_interval
            })
        {
            return Ok(false);
        }
        terminal.draw(|frame| render_frame(frame, app))?;
        self.last_draw = Some(now);
        Ok(true)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum LoopExit {
    ConfirmedExit,
    Cancelled,
    EventChannelClosed,
    RenderFailed { message: String },
}

pub async fn run_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    renderer: &mut Renderer,
    app: &mut App,
    events: &mut mpsc::Receiver<AppEvent>,
    cancel: CancellationToken,
) -> io::Result<LoopExit> {
    if let Err(error) = renderer.draw(terminal, app, true) {
        return Ok(LoopExit::RenderFailed { message: error.to_string() });
    }
    loop {
        let event = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(LoopExit::Cancelled),
            event = events.recv() => match event {
                Some(event) => event,
                None => return Ok(LoopExit::EventChannelClosed),
            }
        };
        let pending_effects = app.update(event);
        let force_draw = !pending_effects.is_empty();
        if let Err(error) = renderer.draw(terminal, app, force_draw) {
            return Ok(LoopExit::RenderFailed { message: error.to_string() });
        }
        if pending_effects.contains(&Effect::Quit) {
            return Ok(LoopExit::ConfirmedExit);
        }
    }
}
