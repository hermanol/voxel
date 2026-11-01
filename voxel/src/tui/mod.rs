// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The `voxel tui` terminal interface. [`run`] wires the renderer and the
//! input and tick tasks around one [`App`], and orders startup and shutdown
//! so the terminal is restored before anything that can wait without a bound.

mod app;
mod event;
mod logging;
mod terminal;
#[cfg(test)]
mod test_support;
mod ui;

use std::{fs::OpenOptions, time::Duration};

use anyhow::Context;
pub(crate) use app::App;
pub(crate) use event::{AppEvent, Effect};
use tokio_util::sync::CancellationToken;

pub(crate) async fn run(workdir: camino::Utf8PathBuf) -> anyhow::Result<()> {
    // Everything that must survive terminal setup failure is created first.
    std::fs::create_dir_all(&workdir)?;
    let durable =
        logging::DurableLog::open(workdir.join("voxel-tui.log").as_std_path())
            .context("open durable TUI log")?;
    let (events_tx, mut events_rx) = terminal::event_channel();
    let shutdown = CancellationToken::new();

    let terminal_writer = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .context("open controlling terminal")?;
    let mut terminal = terminal::TerminalSession::enter(
        terminal_writer,
        shutdown.clone(),
        durable,
    )
    .context("enter terminal UI")?;
    let mut app = App::new();

    let mut input = terminal::spawn_input(events_tx.clone(), shutdown.clone());
    let mut ticks = terminal::spawn_ticks(
        events_tx.clone(),
        shutdown.clone(),
        Duration::from_millis(250),
    )?;
    // The launch animation needs wicket's 30 ms cadence; run a dedicated
    // ticker for its duration rather than speeding up the steady-state tick.
    app.splash = Some(ui::splash::Splash::default());
    let splash_ticks = shutdown.child_token();
    terminal::spawn_ticks(
        events_tx.clone(),
        splash_ticks.clone(),
        ui::splash::FRAME,
    )?;
    tokio::spawn(async move {
        tokio::time::sleep(ui::splash::DURATION + ui::splash::FRAME).await;
        splash_ticks.cancel();
    });
    drop(events_tx);

    let mut renderer = ui::renderer::Renderer::new(Duration::from_millis(33));
    let mut loop_future = Box::pin(ui::renderer::run_loop(
        terminal.terminal_mut(),
        &mut renderer,
        &mut app,
        &mut events_rx,
        shutdown.clone(),
    ));
    let mut input_result = None;
    let mut ticks_result = None;
    let mut primary = None;
    let mut loop_result = tokio::select! {
        result = &mut loop_future => Some(result),
        result = &mut input => { input_result = Some(result); primary = Some("terminal input stopped unexpectedly".to_string()); None },
        result = &mut ticks => { ticks_result = Some(result); primary = Some("terminal ticks stopped unexpectedly".to_string()); None },
    };
    shutdown.cancel();
    if loop_result.is_none() {
        loop_result = Some(loop_future.as_mut().await);
    }
    drop(loop_future);

    match loop_result.as_ref().expect("renderer result captured") {
        Err(error) => {
            primary.get_or_insert_with(|| {
                format!("terminal renderer failed: {error}")
            });
        }
        Ok(ui::renderer::LoopExit::ConfirmedExit) => {}
        Ok(ui::renderer::LoopExit::RenderFailed { message, .. }) => {
            primary.get_or_insert_with(|| {
                format!("render terminal UI: {message}")
            });
        }
        Ok(ui::renderer::LoopExit::EventChannelClosed) => {
            primary.get_or_insert_with(|| "TUI event channel closed".into());
        }
        Ok(ui::renderer::LoopExit::Cancelled) => {
            primary.get_or_insert_with(|| {
                "TUI renderer cancelled unexpectedly".into()
            });
        }
    }

    if terminal.panicked() {
        primary = Some(
            "The Voxel TUI has panicked! See above output for details.".into(),
        );
    }

    // Restore before anything can make shutdown wait without a bound. The
    // receiver remains live below so every producer can settle.
    if let Err(error) = terminal.restore() {
        let cleanup = format!("restore terminal: {error}");
        primary = Some(primary.map_or(cleanup.clone(), |error| {
            format!("{error}; cleanup error: {cleanup}")
        }));
    }

    while input_result.is_none() || ticks_result.is_none() {
        tokio::select! {
            result = &mut input, if input_result.is_none() => input_result = Some(result),
            result = &mut ticks, if ticks_result.is_none() => ticks_result = Some(result),
            event = events_rx.recv() => if let Some(event) = event { let _ = app.update(event); },
        }
    }
    while let Ok(event) = events_rx.try_recv() {
        let _ = app.update(event);
    }
    let mut cleanup = Vec::new();
    match &input_result {
        Some(Ok(Err(error))) => {
            cleanup.push(format!("terminal input failed: {error}"))
        }
        Some(Err(error)) => {
            cleanup.push(format!("terminal input task failed: {error}"))
        }
        _ => {}
    }
    if let Some(Err(error)) = ticks_result {
        cleanup.push(format!("terminal ticks task failed: {error}"));
    }
    if !cleanup.is_empty() {
        let cleanup = cleanup.join("; ");
        primary = Some(primary.map_or(cleanup.clone(), |error| {
            format!("{error}; cleanup error: {cleanup}")
        }));
    }
    if let Some(error) = primary {
        return Err(anyhow::anyhow!(error));
    }
    match app.session.exit {
        Some(app::SessionExit::Quit) => Ok(()),
        None => Err(anyhow::anyhow!("TUI stopped without a confirmed exit")),
    }
}
