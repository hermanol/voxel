// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The Rack Summary section: the selected rack's RSS state and health at a
//! glance.

use super::{
    monitor::resource_health_state,
    widgets::{section_block, selection_style},
};
use crate::{
    tui::reconcile::ObservedDeploymentState,
    tui::reconcile::RssObservation,
    tui::{
        App,
        telemetry::{Freshness, HealthState, LatestSample},
    },
};
use ratatui::{layout::Rect, text::Line, widgets::Paragraph};
use std::{collections::BTreeSet, time::Duration};

pub fn draw(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &App,
    focused: bool,
    expanded: bool,
) {
    let block = section_block("Rack Summary", expanded, focused);
    frame.render_widget(block.clone(), area);
    let inner = block.inner(area);
    if !expanded || inner.height == 0 {
        return;
    }
    let racks = app
        .deployment
        .topology
        .iter()
        .filter_map(|descriptor| descriptor.rack)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let selected_index = app
        .session
        .selected_rack
        .and_then(|rack| racks.iter().position(|candidate| *candidate == rack))
        .unwrap_or(0);
    let Some(rack) = racks.get(selected_index).copied() else {
        frame.render_widget(
            Paragraph::new("No racks configured · RSS unavailable")
                .block(block),
            area,
        );
        return;
    };
    let (healthy, unhealthy, checking) = app
        .deployment
        .topology
        .iter()
        .filter(|descriptor| descriptor.rack == Some(rack))
        .map(|descriptor| resource_health_state(app, &descriptor.id))
        .fold((0, 0, 0), |(healthy, unhealthy, checking), state| match state {
            HealthState::Healthy => (healthy + 1, unhealthy, checking),
            HealthState::Degraded
            | HealthState::Failed
            | HealthState::Stale => (healthy, unhealthy + 1, checking),
            _ => (healthy, unhealthy, checking + 1),
        });
    let rss = app
        .deployment
        .rss
        .get(&rack)
        .map(|sample| rss_summary(app, sample))
        .unwrap_or_else(|| "RSS unavailable (no sample)".into());
    let compact = area.width < 100;
    let title =
        format!(" Rack {} ◀ {}/{} ▶ ", rack.0, selected_index + 1, racks.len());
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            ratatui::text::Span::styled(title, selection_style()),
            ratatui::text::Span::raw(format!(" · {rss}")),
        ])),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    let health = if compact {
        format!("{healthy}/{unhealthy}/{checking}")
    } else {
        format!("health {healthy} ok / {unhealthy} bad / {checking} checking")
    };
    frame.render_widget(
        Paragraph::new(health),
        Rect::new(
            inner.x,
            inner.y.saturating_add(1),
            inner.width,
            inner.height.saturating_sub(1).min(1),
        ),
    );
}

fn rss_summary(app: &App, sample: &LatestSample<RssObservation>) -> String {
    if app.deployment.observed == ObservedDeploymentState::Stopped {
        return "RSS Stopped".into();
    }
    let freshness = app
        .now
        .map(|now| {
            sample.freshness(
                now,
                Duration::from_secs(15),
                Duration::from_secs(60),
            )
        })
        .unwrap_or(Freshness::Unavailable);
    let state = match freshness {
        Freshness::Stale => "Stale",
        Freshness::Unavailable => "Unavailable",
        Freshness::Fresh => {
            match sample.good.as_ref().map(|good| &good.value) {
                Some(RssObservation::Initialized { .. }) => {
                    "Healthy (initialized)"
                }
                Some(RssObservation::Initializing { .. }) => {
                    "Checking (initializing)"
                }
                Some(RssObservation::StaleInitialized) => {
                    "Degraded (stale initialized)"
                }
                Some(RssObservation::Failed { .. }) => "Failed",
                Some(RssObservation::Unavailable) => "Unavailable",
                Some(RssObservation::UnknownResponse) | None => "Unknown",
            }
        }
    };
    let age = sample
        .good
        .as_ref()
        .and_then(|good| {
            app.now.map(|now| {
                now.saturating_duration_since(good.captured_at).as_secs()
            })
        })
        .map(|seconds| format!("{seconds}s ago"))
        .unwrap_or_else(|| "never".into());
    let error = sample
        .latest_error
        .as_ref()
        .map(|error| error.message.as_str())
        .unwrap_or("none");
    format!("RSS {state} · success {age} · error {error}")
}
