// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The Monitor view. Lays out the rack summary, topology and node details,
//! and derives node health and its freshness from collected telemetry.

use super::{
    colors::{OX_RED, TUI_GREEN, TUI_GREY, TUI_GREY_DARK, TUI_YELLOW},
    renderer::LayoutMode,
    widgets::{section_block, section_heights, section_rects},
};
use crate::{
    tui::reconcile::ObservedDeploymentState,
    tui::{
        App,
        event::MonitoringPane,
        telemetry::{
            Freshness, HealthContext, HealthState, LatestSample,
            ResourceDescriptor, ResourceId, ResourceKind, derive_health_state,
        },
    },
};
use ratatui::{
    layout::Rect,
    style::Style,
    widgets::{Block, Paragraph},
};
use std::time::Duration;

// A sled health probe performs four serial calls, each with a ten-second
// timeout. Keep a successful sample healthy through one slow probe cycle.
const HEALTH_STALE_AFTER: Duration = Duration::from_secs(60);
const HEALTH_UNAVAILABLE_AFTER: Duration = Duration::from_secs(120);

pub fn draw(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &App,
    mode: LayoutMode,
) {
    let rows = monitor_rows(area, app, mode);
    super::rack_selector::draw(
        frame,
        rows[0],
        app,
        app.session.monitoring_pane == MonitoringPane::RackSummary,
        app.session.monitoring_expanded(MonitoringPane::RackSummary),
    );
    draw_topology(frame, rows[1], app, mode);
}

pub(crate) fn monitor_rows(
    area: Rect,
    app: &App,
    mode: LayoutMode,
) -> Vec<Rect> {
    let mut preferred = match (mode, area.height) {
        (LayoutMode::Wide, 0..=17) => [4, 3],
        (LayoutMode::Wide, _) => [5, 12],
        (LayoutMode::Compact, 0..=11) => [3, 3],
        (LayoutMode::Compact, _) => [4, 8],
        (LayoutMode::Minimum, _) => [1, 1],
    };
    // The rack elevation needs far more rows than a summary does, so the
    // Topology section claims them while it has focus, short of the rack
    // summary's rows: those carry the rack's control-plane error banner.
    if app.session.monitoring_pane == MonitoringPane::Topology {
        preferred[1] = (super::topology::PREFERRED_HEIGHT + 2)
            .min(area.height.saturating_sub(preferred[0]));
    }
    let expanded =
        MonitoringPane::ORDER.map(|pane| app.session.monitoring_expanded(pane));
    let focused = MonitoringPane::ORDER
        .iter()
        .position(|pane| *pane == app.session.monitoring_pane)
        .unwrap_or(1);
    let heights = section_heights(
        area.height,
        &expanded,
        focused,
        &preferred,
        &[0, 2],
        1,
    );
    section_rects(area, &heights)
}

#[derive(Clone, Copy)]
pub(crate) struct MiddleLayout {
    pub(crate) topology: Rect,
    pub(crate) detail: Rect,
    pub(crate) divider: Rect,
}

/// Wide layouts show the rack and the selected node's details side by
/// side; compact ones show one at a time, as wicket does.
pub(crate) fn middle_layout(area: Rect, mode: LayoutMode) -> MiddleLayout {
    let inner = Block::bordered().inner(area);
    if mode != LayoutMode::Wide {
        return MiddleLayout {
            topology: inner,
            detail: inner,
            divider: Rect::default(),
        };
    }
    let topology_width =
        super::topology::column_width(inner.height).min(inner.width / 2);
    let divider = Rect::new(
        inner.x.saturating_add(topology_width),
        inner.y,
        1,
        inner.height,
    );
    MiddleLayout {
        topology: Rect::new(inner.x, inner.y, topology_width, inner.height),
        detail: Rect::new(
            divider.x.saturating_add(1),
            inner.y,
            inner.right().saturating_sub(divider.x.saturating_add(1)),
            inner.height,
        ),
        divider,
    }
}

/// Where the details pane is drawn, if the Topology section shows it.
pub(crate) fn detail_area(app: &App) -> Option<Rect> {
    let (area, mode) = super::widgets::content_area(app);
    let rows = monitor_rows(area, app, mode);
    let visible = app.session.monitoring_expanded(MonitoringPane::Topology)
        && (mode == LayoutMode::Wide || app.session.detail_open);
    visible.then(|| middle_layout(rows[1], mode).detail)
}

pub(crate) fn resource_health_state(app: &App, id: &ResourceId) -> HealthState {
    let kind = resource_kind(app, id);
    if kind != Some(ResourceKind::Sled) {
        // Switch zones and routers report no service diagnostics, so their
        // health is whether their address probe still answers.
        return app
            .observability
            .addresses
            .get(id)
            .map(|sample| collection_health_state(app, sample))
            .unwrap_or(HealthState::Unavailable);
    }
    let Some(sample) = app.observability.health.get(id) else {
        return HealthState::Unavailable;
    };
    let context = if app.deployment.observed == ObservedDeploymentState::Stopped
    {
        HealthContext::Stopped
    } else if app.now.is_none()
        || (sample.good.is_none() && sample.last_attempt.is_none())
    {
        HealthContext::Checking
    } else {
        HealthContext::Active
    };
    let freshness = app
        .now
        .map(|now| {
            sample.freshness(now, HEALTH_STALE_AFTER, HEALTH_UNAVAILABLE_AFTER)
        })
        .unwrap_or(Freshness::Unavailable);
    derive_health_state(
        context,
        sample.good.as_ref().map(|good| &good.value),
        freshness,
    )
}

fn collection_health_state<T>(
    app: &App,
    sample: &LatestSample<T>,
) -> HealthState {
    if app.deployment.observed == ObservedDeploymentState::Stopped {
        return HealthState::Stopped;
    }
    let Some(now) = app.now else {
        return HealthState::Checking;
    };
    if sample.good.is_none() && sample.last_attempt.is_none() {
        return HealthState::Checking;
    }
    match sample.freshness(now, HEALTH_STALE_AFTER, HEALTH_UNAVAILABLE_AFTER) {
        Freshness::Fresh => HealthState::Healthy,
        Freshness::Stale => HealthState::Stale,
        Freshness::Unavailable => HealthState::Unavailable,
    }
}

fn resource_kind(app: &App, id: &ResourceId) -> Option<ResourceKind> {
    app.deployment
        .topology
        .iter()
        .find(|descriptor| &descriptor.id == id)
        .map(|descriptor| descriptor.kind)
}

fn resource_last_success(
    app: &App,
    id: &ResourceId,
) -> Option<std::time::Instant> {
    if resource_kind(app, id) == Some(ResourceKind::Sled) {
        app.observability.health.get(id).and_then(|sample| {
            sample.good.as_ref().map(|good| good.captured_at)
        })
    } else {
        last_good(app.observability.addresses.get(id))
    }
}

fn last_good<T>(
    sample: Option<&LatestSample<T>>,
) -> Option<std::time::Instant> {
    sample.and_then(|sample| sample.good.as_ref().map(|good| good.captured_at))
}

pub(crate) fn health_status_label(state: HealthState) -> &'static str {
    match state {
        HealthState::Healthy => "Healthy",
        HealthState::Degraded => "Degraded",
        HealthState::Failed => "Failed",
        HealthState::Checking
        | HealthState::Unknown
        | HealthState::Stale
        | HealthState::Unavailable
        | HealthState::Stopped => "Checking Status",
    }
}

pub(crate) fn resource_health_summary(app: &App, id: &ResourceId) -> String {
    let age = resource_last_success(app, id)
        .and_then(|captured_at| {
            app.now
                .map(|now| now.saturating_duration_since(captured_at).as_secs())
        })
        .map(|seconds| format!("{seconds}s ago"))
        .unwrap_or_else(|| "never".into());
    let error = if resource_kind(app, id) == Some(ResourceKind::Sled) {
        app.observability
            .health
            .get(id)
            .and_then(|sample| sample.latest_error.as_ref())
    } else {
        app.observability
            .addresses
            .get(id)
            .and_then(|sample| sample.latest_error.as_ref())
            .filter(|error| collection_error_is_visible(app, id, error))
    }
    .map(|error| format!("; latest error: {}", error.message))
    .unwrap_or_default();
    format!(
        "{} (last success {age}){error}",
        health_status_label(resource_health_state(app, id))
    )
}

pub(crate) fn collection_error_is_visible(
    app: &App,
    id: &ResourceId,
    error: &crate::tui::telemetry::CollectionError,
) -> bool {
    resource_kind(app, id) != Some(ResourceKind::Router)
        || (matches!(
            app.deployment.observed,
            ObservedDeploymentState::Running
                | ObservedDeploymentState::Degraded
        ) && app
            .deployment
            .last_reconciliation_at
            .is_some_and(|ready_at| error.attempted_at >= ready_at))
}

pub(crate) fn health_style(state: HealthState) -> Style {
    Style::default().fg(match state {
        HealthState::Healthy => TUI_GREEN,
        HealthState::Degraded | HealthState::Failed => OX_RED,
        HealthState::Checking => TUI_YELLOW,
        HealthState::Stale => TUI_GREY,
        HealthState::Unknown
        | HealthState::Unavailable
        | HealthState::Stopped => TUI_GREY_DARK,
    })
}

fn draw_topology(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &App,
    mode: LayoutMode,
) {
    let rack = app.session.selected_rack;
    let expanded = app.session.monitoring_expanded(MonitoringPane::Topology);
    let focused = app.session.monitoring_pane == MonitoringPane::Topology;
    let outer = section_block("Topology", expanded, focused);
    frame.render_widget(outer.clone(), area);
    if !expanded || outer.inner(area).height == 0 {
        return;
    }
    let layout = middle_layout(area, mode);
    let detail_focused = app.session.detail_open;
    if mode == LayoutMode::Wide || !detail_focused {
        let scoped = scoped_descriptors(app, rack);
        super::topology::draw(frame, layout.topology, app, &scoped);
    }
    if mode == LayoutMode::Wide || detail_focused {
        super::node_detail::draw(frame, layout.detail, app, detail_focused);
    }
    if layout.divider.area() == 0 {
        return;
    }
    let edge = super::widgets::line_style(focused);
    for y in layout.divider.y..layout.divider.bottom() {
        frame.render_widget(
            Paragraph::new("│").style(edge),
            Rect::new(layout.divider.x, y, 1, 1),
        );
    }
    frame.render_widget(
        Paragraph::new("┬").style(edge),
        Rect::new(layout.divider.x, area.y, 1, 1),
    );
    frame.render_widget(
        Paragraph::new("┴").style(edge),
        Rect::new(layout.divider.x, area.bottom() - 1, 1, 1),
    );
}

pub(crate) fn scoped_descriptors(
    app: &App,
    rack: Option<crate::tui::telemetry::RackId>,
) -> Vec<ResourceDescriptor> {
    app.deployment
        .topology
        .iter()
        .filter(|descriptor| monitor_scope_contains(app, rack, descriptor))
        .cloned()
        .collect()
}

fn monitor_scope_contains(
    app: &App,
    rack: Option<crate::tui::telemetry::RackId>,
    descriptor: &ResourceDescriptor,
) -> bool {
    descriptor.kind == ResourceKind::Router
        || descriptor.rack == rack
        || app.session.selected_resource.as_ref() == Some(&descriptor.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_support::{render, router, sled, switch, text};
    use crate::tui::{
        event::AppEvent,
        telemetry::{
            HealthDiagnostic, NodeAddresses, NtpDiagnostic, ServiceState,
        },
    };
    use std::time::Instant;

    #[test]
    fn resource_health_stays_accurate_between_probes() {
        let start = Instant::now();
        let at = |seconds| start + Duration::from_secs(seconds);
        let tick = |seconds| AppEvent::Tick { now: at(seconds) };

        let switch_zone = switch(0, 0, "g0");
        let sled_g0 = sled(0, 0);
        // (case, resource, [(when, event, health)], final summary)
        let cases = [
            (
                "switch whose address probe stops answering",
                switch_zone.clone(),
                vec![
                    ("before any sample", tick(0), HealthState::Checking),
                    (
                        "after an address sample",
                        AppEvent::Addresses {
                            id: switch_zone.id.clone(),
                            at: at(0),
                            addresses: NodeAddresses::default(),
                        },
                        HealthState::Healthy,
                    ),
                    (
                        "after five quiet minutes",
                        tick(300),
                        HealthState::Unavailable,
                    ),
                    (
                        "after the probe answers again",
                        AppEvent::Addresses {
                            id: switch_zone.id.clone(),
                            at: at(300),
                            addresses: NodeAddresses::default(),
                        },
                        HealthState::Healthy,
                    ),
                ],
                "Healthy (last success 0s ago)",
            ),
            (
                "sled whose health probe is slow to complete",
                sled_g0.clone(),
                vec![
                    ("before any sample", tick(0), HealthState::Checking),
                    (
                        "after a healthy sample",
                        AppEvent::Health {
                            id: sled_g0.id.clone(),
                            at: at(0),
                            diagnostic: HealthDiagnostic {
                                sled_agent: Some(ServiceState::Online),
                                ntp: NtpDiagnostic {
                                    synchronized: Some(true),
                                    ..Default::default()
                                },
                                ..Default::default()
                            },
                        },
                        HealthState::Healthy,
                    ),
                    // Four serial probe calls with ten-second timeouts, plus
                    // scheduling delay, can leave this long between
                    // completions.
                    ("50 s after the sample", tick(50), HealthState::Healthy),
                ],
                "Healthy (last success 50s ago)",
            ),
        ];

        for (case, resource, steps, summary) in cases {
            let id = resource.id.clone();
            let mut app = App::new(vec![resource], 4);
            app.deployment.observed = ObservedDeploymentState::Running;
            for (when, event, health) in steps {
                app.update(event);
                assert_eq!(
                    resource_health_state(&app, &id),
                    health,
                    "{case}, {when}"
                );
            }
            assert_eq!(resource_health_summary(&app, &id), summary, "{case}");
        }
    }

    fn node_details_text(app: &App) -> String {
        text(&render(100, 40, |frame| {
            super::super::node_detail::draw(frame, frame.area(), app, false)
        }))
    }

    fn app_selecting(resource: ResourceDescriptor) -> App {
        let mut app = App::new(vec![resource.clone()], 4);
        app.session.selected_resource = Some(resource.id);
        app
    }

    /// Asserts that `screen` shows every phrase, printing the screen when one
    /// is missing.
    #[track_caller]
    fn assert_shows(screen: &str, phrases: &[&str]) {
        for phrase in phrases {
            assert!(
                screen.contains(phrase),
                "{phrase:?} missing from:\n{screen}"
            );
        }
    }

    #[test]
    fn operator_is_told_why_monitoring_is_missing() {
        let now = Instant::now();
        let log = "/work/voxel-tui.log";

        // A failed health probe: a sled's details explain the missing
        // diagnostics and where the errors are logged.
        let sled = sled(0, 3);
        let mut app = app_selecting(sled.clone());
        app.update(AppEvent::HealthFailed {
            id: sled.id,
            at: now,
            message: "sled-agent state timed out".into(),
        });
        app.durable_log_path = log.into();
        assert_shows(
            &node_details_text(&app),
            &[
                "no successful health sample",
                "Errors · logged to /work/voxel-tui.log",
                "Health: sled-agent state timed out",
            ],
        );

        // Router probes fail while the VM is still coming up, so errors from
        // before the running reconciliation are noise; later ones are real,
        // and a success clears them.
        let id = router("ce").id;
        let mut app = app_selecting(router("ce"));
        app.update(AppEvent::AddressesFailed {
            id: id.clone(),
            at: now,
            message: "propolis uuid for ce: No such file".into(),
        });
        app.deployment.observed = ObservedDeploymentState::Running;
        app.deployment.last_reconciliation_at =
            Some(now + Duration::from_secs(1));
        let details = node_details_text(&app);
        assert!(!details.contains("propolis uuid"), "{details}");

        let after_reconciliation = now + Duration::from_secs(2);
        app.update(AppEvent::AddressesFailed {
            id: id.clone(),
            at: after_reconciliation,
            message: "router command failed after readiness".into(),
        });
        assert_shows(
            &node_details_text(&app),
            &["router command failed after readiness"],
        );
        app.update(AppEvent::Addresses {
            id: id.clone(),
            at: after_reconciliation + Duration::from_secs(1),
            addresses: NodeAddresses::default(),
        });
        assert!(!resource_health_summary(&app, &id).contains("latest error"));
    }
}
