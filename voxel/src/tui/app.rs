// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The TUI state machine. [`App::update`] folds each [`AppEvent`] into the
//! deployment, operation and session state and returns the [`Effect`]s the
//! runtime must carry out; it performs no I/O, so the renderer and tests
//! drive it directly.

use std::collections::{BTreeSet, VecDeque};
use std::time::Instant;

use crate::tui::event::{
    Action, AppEvent, CancelChoice, Confirmation, ConfirmationOptionAction,
    DeploymentPane, Direction, Effect, OperationRequestId,
};
use crate::tui::operation::{
    CommandOutcome, LogLevel, OperationEvent, OperationKind, OperationPhase,
    OperationWarning,
};
use crate::tui::reconcile::{
    ObservedDeploymentState, ReconciliationResult, RouteEvidence,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalSize {
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PostOperationExit {
    Destroy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionExit {
    Quit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionState {
    pub deployment_pane: DeploymentPane,
    pub collapsed_deployment: BTreeSet<DeploymentPane>,
    pub phase_scroll: usize,
    pub help_scroll: usize,
    pub help_open: bool,
    pub terminal: TerminalSize,
    pub confirmation: Option<Confirmation>,
    pub confirmation_selection: usize,
    pub post_operation_exit: Option<PostOperationExit>,
    pub exit: Option<SessionExit>,
    pub quitting: bool,
}

impl SessionState {
    pub(crate) fn deployment_expanded(&self, pane: DeploymentPane) -> bool {
        !self.collapsed_deployment.contains(&pane)
    }
}

#[derive(Clone, Debug)]
pub struct DeploymentState {
    pub observed: ObservedDeploymentState,
    pub routes: RouteEvidence,
    pub last_reconciliation: Option<ReconciliationResult>,
    pub last_reconciliation_at: Option<Instant>,
    pub reconciliation_failure: Option<String>,
    pub reconciliation_in_progress: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Progress {
    pub completed: usize,
    pub total: usize,
    pub message: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogEntry {
    pub source: LogSource,
    pub level: LogLevel,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogSource {
    Application,
    Operation,
}

impl LogEntry {
    pub fn application(level: LogLevel, message: impl Into<String>) -> Self {
        Self { source: LogSource::Application, level, message: message.into() }
    }

    pub fn operation(level: LogLevel, message: impl Into<String>) -> Self {
        Self { source: LogSource::Operation, level, message: message.into() }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogFilter {
    All,
    Info,
    Warning,
    Error,
}
impl LogFilter {
    fn next(self) -> Self {
        match self {
            Self::All => Self::Info,
            Self::Info => Self::Warning,
            Self::Warning => Self::Error,
            Self::Error => Self::All,
        }
    }
    pub fn accepts(self, level: LogLevel) -> bool {
        self == Self::All
            || matches!(
                (self, level),
                (Self::Info, LogLevel::Info)
                    | (Self::Warning, LogLevel::Warning)
                    | (Self::Error, LogLevel::Error)
            )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundedLogs {
    pub entries: VecDeque<LogEntry>,
    pub capacity: usize,
    pub scroll: usize,
}
impl BoundedLogs {
    pub fn new(capacity: usize) -> Self {
        Self { entries: VecDeque::new(), capacity, scroll: 0 }
    }
    pub fn push(&mut self, entry: LogEntry) {
        if self.capacity == 0 {
            self.entries.clear();
            self.scroll = 0;
            return;
        }
        let evicted = self.entries.len() == self.capacity;
        if evicted {
            self.entries.pop_front();
        } else if self.scroll > 0 {
            self.scroll = self.scroll.saturating_add(1);
        }
        self.entries.push_back(entry);
        self.scroll = self.scroll.min(self.entries.len().saturating_sub(1));
    }
    pub fn push_filtered(&mut self, entry: LogEntry, filter: LogFilter) {
        let previous_scroll = self.scroll;
        let accepted_append = filter.accepts(entry.level);
        let accepted_eviction = self.entries.len() == self.capacity
            && self
                .entries
                .front()
                .is_some_and(|oldest| filter.accepts(oldest.level));
        self.push(entry);
        if previous_scroll > 0 {
            self.scroll = previous_scroll
                .saturating_add(usize::from(accepted_append))
                .saturating_sub(usize::from(accepted_eviction));
        }
    }
    pub fn clamp(&mut self, visible: usize) {
        self.scroll =
            self.scroll.min(self.entries.len().saturating_sub(visible));
    }
    pub fn scroll(&mut self, delta: isize, visible: usize) {
        self.scroll = self.scroll.saturating_add_signed(delta);
        self.clamp(visible);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveOperation {
    pub request_id: OperationRequestId,
    pub kind: OperationKind,
    pub phase: Option<OperationPhase>,
    pub completed_phases: Vec<OperationPhase>,
    pub progress: Option<Progress>,
    pub warnings: Vec<OperationWarning>,
    pub cancelling: bool,
    pub cancel_choice: Option<CancelChoice>,
    pub force_stop_requested: bool,
}
impl ActiveOperation {
    pub fn new(request_id: OperationRequestId, kind: OperationKind) -> Self {
        Self {
            request_id,
            kind,
            phase: None,
            completed_phases: vec![],
            progress: None,
            warnings: vec![],
            cancelling: false,
            cancel_choice: None,
            force_stop_requested: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingOperation {
    pub request_id: OperationRequestId,
    pub kind: OperationKind,
}
impl PendingOperation {
    pub fn new(request_id: OperationRequestId, kind: OperationKind) -> Self {
        Self { request_id, kind }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationState {
    pub pending: Option<PendingOperation>,
    pub active: Option<ActiveOperation>,
    pub retained: Option<ActiveOperation>,
    pub retained_warnings: Vec<OperationWarning>,
    pub outcome: Option<CommandOutcome>,
    pub start_failure: Option<String>,
}

impl OperationState {
    pub(crate) fn displayed(&self) -> Option<&ActiveOperation> {
        self.active.as_ref().or(self.retained.as_ref())
    }
}

#[derive(Clone, Debug)]
pub struct App {
    pub session: SessionState,
    pub deployment: DeploymentState,
    pub operation: OperationState,
    pub logs: BoundedLogs,
    pub logs_filter: LogFilter,
    pub durable_log_path: String,
    pub now: Option<Instant>,
    pub splash: Option<crate::tui::ui::splash::Splash>,
    next_operation_request_id: Option<OperationRequestId>,
}

impl App {
    pub fn new(log_capacity: usize) -> Self {
        Self {
            session: SessionState {
                deployment_pane: DeploymentPane::OverallProgress,
                collapsed_deployment: BTreeSet::new(),
                phase_scroll: 0,
                help_scroll: 0,
                help_open: false,
                terminal: TerminalSize { width: 0, height: 0 },
                confirmation: None,
                confirmation_selection: 0,
                post_operation_exit: None,
                exit: None,
                quitting: false,
            },
            deployment: DeploymentState {
                observed: ObservedDeploymentState::Unknown,
                routes: RouteEvidence::Unknown,
                last_reconciliation: None,
                last_reconciliation_at: None,
                reconciliation_failure: None,
                reconciliation_in_progress: false,
            },
            operation: OperationState {
                pending: None,
                active: None,
                retained: None,
                retained_warnings: vec![],
                outcome: None,
                start_failure: None,
            },
            logs: BoundedLogs::new(log_capacity),
            logs_filter: LogFilter::All,
            durable_log_path: "voxel-tui.log".into(),
            now: None,
            splash: None,
            next_operation_request_id: Some(OperationRequestId::FIRST),
        }
    }

    pub fn update(&mut self, event: AppEvent) -> Vec<Effect> {
        let mut effects = vec![];
        match event {
            AppEvent::Action(action) => {
                // Any key skips the launch animation, as in wicket.
                if self.splash.take().is_some() {
                    return effects;
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
            AppEvent::ReconciliationStarted { at } => {
                self.deployment.reconciliation_in_progress = true;
                self.deployment.last_reconciliation_at = Some(at);
                self.deployment.reconciliation_failure = None;
            }
            AppEvent::Reconciled { at, result, routes } => {
                self.deployment.reconciliation_in_progress = false;
                self.deployment.observed = result.state;
                self.deployment.last_reconciliation = Some(result);
                self.deployment.last_reconciliation_at = Some(at);
                self.deployment.reconciliation_failure = None;
                self.deployment.routes = routes;
            }
            AppEvent::ReconciliationFailed { at, message } => {
                self.deployment.reconciliation_in_progress = false;
                self.deployment.last_reconciliation_at = Some(at);
                self.deployment.reconciliation_failure = Some(message);
            }
            AppEvent::Operation { request_id, event } => {
                effects = self.operation_event(request_id, event)
            }
            AppEvent::OperationStartFailed { request_id, kind, message } => {
                if self.operation.pending.as_ref().is_some_and(|pending| {
                    pending.request_id == request_id && pending.kind == kind
                }) {
                    self.operation.pending = None;
                    self.operation.start_failure = Some(message);
                    self.session.post_operation_exit = None;
                }
            }
            AppEvent::DurableLogFailed { message } => self.logs.push_filtered(
                LogEntry::application(LogLevel::Error, message),
                self.logs_filter,
            ),
        }
        self.clamp_log_scroll();
        effects
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
        if self.session.post_operation_exit.is_none()
            && !self.session.quitting
            && action == Action::RequestQuit
        {
            let confirmation = if self.resources_may_exist() {
                Confirmation::QuitAndDestroy
            } else {
                Confirmation::Quit
            };
            self.open_confirmation(confirmation);
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
        match action {
            Action::NextSection => self.move_section(Direction::Next),
            Action::PreviousSection => self.move_section(Direction::Previous),
            Action::ToggleSection => {
                let pane = self.session.deployment_pane;
                if !self.session.collapsed_deployment.remove(&pane) {
                    self.session.collapsed_deployment.insert(pane);
                }
            }
            Action::ToggleHelp if self.session.confirmation.is_none() => {
                self.session.help_open = !self.session.help_open;
                if self.session.help_open {
                    self.session.help_scroll = 0;
                }
            }
            Action::RequestLaunch if self.can_start(OperationKind::Launch) => {
                self.open_confirmation(Confirmation::Launch)
            }
            Action::RequestRoute if self.can_start(OperationKind::Route) => {
                self.open_confirmation(Confirmation::Route)
            }
            Action::RequestCancelAndDestroy if self.can_cancel() => {
                self.open_confirmation(Confirmation::CancelAndDestroy)
            }
            // Escalation: once the stop is already in flight, the same key
            // offers the only remaining lever against a child that will not
            // settle.
            Action::RequestCancelAndDestroy if self.can_force_stop() => {
                self.open_confirmation(Confirmation::ForceStop)
            }
            Action::CycleLogFilter
                if self.session.deployment_pane == DeploymentPane::Logs
                    && self
                        .session
                        .deployment_expanded(DeploymentPane::Logs) =>
            {
                self.logs_filter = self.logs_filter.next();
                self.logs.scroll = 0;
            }
            Action::Scroll { delta, page: false } => {
                self.move_focused_content(delta);
            }
            Action::Scroll { delta, page: true }
                if self
                    .session
                    .deployment_expanded(self.session.deployment_pane) =>
            {
                match self.session.deployment_pane {
                    DeploymentPane::OverallProgress
                    | DeploymentPane::Status
                    | DeploymentPane::CurrentPhase => {}
                    DeploymentPane::Phases => {
                        let amount =
                            crate::tui::ui::deployment::phase_content_height(
                                self,
                            )
                            .max(1) as isize;
                        self.session.phase_scroll = self
                            .session
                            .phase_scroll
                            .saturating_add_signed(delta.saturating_mul(amount))
                    }
                    DeploymentPane::Logs => {
                        let visible =
                            crate::tui::ui::deployment::log_content_height(
                                self,
                            );
                        let amount = visible.max(1) as isize;
                        self.logs.scroll(
                            delta.saturating_mul(amount).saturating_neg(),
                            visible,
                        );
                    }
                }
            }
            _ => {}
        }
        vec![]
    }
    pub(crate) fn can_start(&self, kind: OperationKind) -> bool {
        !self.session.quitting
            && self.operation.active.is_none()
            && self.operation.pending.is_none()
            && match kind {
                OperationKind::Launch => {
                    self.deployment.observed == ObservedDeploymentState::Stopped
                }
                OperationKind::Route => matches!(
                    self.deployment.observed,
                    ObservedDeploymentState::Running
                        | ObservedDeploymentState::Degraded
                ),
                OperationKind::Destroy => {
                    self.deployment.observed != ObservedDeploymentState::Stopped
                }
            }
    }
    pub(crate) fn resources_may_exist(&self) -> bool {
        self.deployment.observed != ObservedDeploymentState::Stopped
            || self.operation.pending.is_some()
            || self.operation.active.is_some()
    }
    pub(crate) fn can_cancel(&self) -> bool {
        self.operation.active.as_ref().is_some_and(|active| !active.cancelling)
    }
    fn can_force_stop(&self) -> bool {
        self.operation
            .active
            .as_ref()
            .is_some_and(|active| !active.force_stop_requested)
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
            Confirmation::Launch if self.can_start(OperationKind::Launch) => {
                self.start(OperationKind::Launch)
            }
            Confirmation::Route if self.can_start(OperationKind::Route) => {
                self.start(OperationKind::Route)
            }
            Confirmation::Quit
                if !self.session.quitting && !self.resources_may_exist() =>
            {
                self.session.exit = Some(SessionExit::Quit);
                self.session.quitting = true;
                vec![Effect::Quit]
            }
            Confirmation::QuitAndDestroy
                if !self.session.quitting && self.resources_may_exist() =>
            {
                self.destroy_and_quit()
            }
            Confirmation::CancelAndDestroy if self.can_cancel() => {
                vec![self.cancel(CancelChoice::Destroy)]
            }
            Confirmation::ForceStop if self.can_force_stop() => {
                let active = self.operation.active.as_mut().unwrap();
                active.force_stop_requested = true;
                let request_id = active.request_id;
                vec![Effect::ForceStop { request_id }]
            }
            _ => vec![],
        }
    }
    fn destroy_and_quit(&mut self) -> Vec<Effect> {
        self.session.post_operation_exit = Some(PostOperationExit::Destroy);
        if let Some(active) = self.operation.active.as_ref() {
            if active.kind == OperationKind::Destroy
                || active.cancel_choice == Some(CancelChoice::Destroy)
            {
                return vec![];
            }
            return vec![self.cancel(CancelChoice::Destroy)];
        }
        if self.operation.pending.is_some() {
            return vec![];
        }
        let effects = self.start(OperationKind::Destroy);
        if effects.is_empty() {
            self.session.post_operation_exit = None;
        }
        effects
    }
    fn moved<T: Clone + Eq>(
        items: &[T],
        current: Option<&T>,
        direction: Direction,
    ) -> Option<T> {
        if items.is_empty() {
            return None;
        }
        let Some(index) =
            current.and_then(|c| items.iter().position(|v| v == c))
        else {
            return Some(match direction {
                Direction::Next => items[0].clone(),
                Direction::Previous => items[items.len() - 1].clone(),
            });
        };
        Some(
            items[match direction {
                Direction::Next => (index + 1) % items.len(),
                Direction::Previous => (index + items.len() - 1) % items.len(),
            }]
            .clone(),
        )
    }
    fn move_focused_content(&mut self, delta: isize) {
        if delta == 0
            || !self.session.deployment_expanded(self.session.deployment_pane)
        {
            return;
        }
        match self.session.deployment_pane {
            DeploymentPane::Phases => {
                let Some(operation) = self.operation.active.as_ref() else {
                    return;
                };
                let phases =
                    crate::tui::ui::deployment::phase_order(operation.kind);
                if phases.is_empty() {
                    return;
                }
                let visible =
                    crate::tui::ui::deployment::phase_content_height(self);
                let active = operation.phase.and_then(|phase| {
                    phases.iter().position(|item| *item == phase)
                });
                let current = crate::tui::ui::deployment::phase_window_start(
                    self.session.phase_scroll,
                    visible,
                    phases.len(),
                    active,
                );
                let candidate = crate::tui::ui::deployment::phase_window_start(
                    current.saturating_add_signed(delta),
                    visible,
                    phases.len(),
                    active,
                );
                self.session.phase_scroll = candidate;
            }
            DeploymentPane::CurrentPhase => {}
            DeploymentPane::Logs => {
                let visible =
                    crate::tui::ui::deployment::log_content_height(self);
                let len = crate::tui::ui::logs::filtered_len(self);
                if len == 0 {
                    return;
                }
                let current = crate::tui::ui::logs::effective_scroll(
                    self.logs.scroll,
                    len,
                    visible,
                );
                let candidate = crate::tui::ui::logs::effective_scroll(
                    current.saturating_add_signed(delta.saturating_neg()),
                    len,
                    visible,
                );
                self.logs.scroll = candidate;
            }
            DeploymentPane::OverallProgress | DeploymentPane::Status => {}
        }
    }
    fn move_section(&mut self, direction: Direction) {
        self.session.deployment_pane = Self::moved(
            &DeploymentPane::ORDER,
            Some(&self.session.deployment_pane),
            direction,
        )
        .unwrap();
    }
    fn filtered_log_count(&self) -> usize {
        self.logs
            .entries
            .iter()
            .filter(|entry| self.logs_filter.accepts(entry.level))
            .count()
    }

    fn clamp_log_scroll(&mut self) {
        let visible = crate::tui::ui::deployment::log_content_height(self);
        let maximum = self.filtered_log_count().saturating_sub(visible);
        self.logs.scroll = self.logs.scroll.min(maximum);
    }
    fn operation_event(
        &mut self,
        request_id: OperationRequestId,
        event: OperationEvent,
    ) -> Vec<Effect> {
        let mut effects = vec![];
        // Stateful events are correlated to pending/active requests below. Logs remain an
        // intentionally inclusive display stream, including delayed entries from old requests.
        match event {
            OperationEvent::Started { kind } => {
                if self.operation.active.is_none()
                    && self.operation.pending.as_ref().is_some_and(|pending| {
                        pending.request_id == request_id && pending.kind == kind
                    })
                {
                    self.operation.pending = None;
                    self.operation.active =
                        Some(ActiveOperation::new(request_id, kind));
                    self.operation.outcome = None;
                    if self.session.post_operation_exit
                        == Some(PostOperationExit::Destroy)
                        && kind != OperationKind::Destroy
                    {
                        effects.push(self.cancel(CancelChoice::Destroy));
                    }
                }
            }
            OperationEvent::PhaseStarted { phase } => {
                if let Some(active) = self.matching_active_mut(request_id) {
                    let phases =
                        crate::tui::ui::deployment::phase_order(active.kind);
                    if let Some(index) =
                        phases.iter().position(|candidate| *candidate == phase)
                    {
                        active.completed_phases = phases[..index].to_vec();
                        active.phase = Some(phase);
                    }
                }
            }
            OperationEvent::Log { level, message } => self.logs.push_filtered(
                LogEntry::operation(level, message),
                self.logs_filter,
            ),
            OperationEvent::Warning(warning) => {
                if let Some(active) = self.matching_active_mut(request_id)
                    && !active.warnings.contains(&warning)
                {
                    active.warnings.push(warning);
                }
            }
            OperationEvent::Finished(outcome) => {
                if self
                    .operation
                    .active
                    .as_ref()
                    .is_some_and(|active| active.request_id == request_id)
                {
                    let mut active = self.operation.active.take().unwrap();
                    let active_kind = active.kind;
                    let cancel_choice = active.cancel_choice;
                    self.operation.retained_warnings = active.warnings.clone();
                    if command_succeeded(&outcome) {
                        active.completed_phases =
                            crate::tui::ui::deployment::phase_order(
                                active.kind,
                            );
                        active.phase = None;
                    }
                    self.operation.retained = Some(active);
                    let should_start_destroy = active_kind
                        != OperationKind::Destroy
                        && (self.session.post_operation_exit
                            == Some(PostOperationExit::Destroy)
                            || (self.session.post_operation_exit.is_none()
                                && cancel_choice
                                    == Some(CancelChoice::Destroy)));
                    let should_quit = self.session.post_operation_exit
                        == Some(PostOperationExit::Destroy)
                        && active_kind == OperationKind::Destroy
                        && command_succeeded(&outcome);
                    self.operation.outcome = Some(outcome);
                    if should_start_destroy {
                        let destroy_effects =
                            self.start(OperationKind::Destroy);
                        if destroy_effects.is_empty() {
                            self.session.post_operation_exit = None;
                        }
                        effects.extend(destroy_effects);
                    } else {
                        self.session.post_operation_exit = None;
                        if should_quit {
                            self.session.exit = Some(SessionExit::Quit);
                            self.session.quitting = true;
                            effects.push(Effect::Quit);
                        }
                    }
                }
            }
        }
        effects
    }

    fn start(&mut self, kind: OperationKind) -> Vec<Effect> {
        let Some(request_id) = self.next_operation_request_id else {
            self.operation.start_failure = Some(
                "operation request ID space exhausted; restart the application before retrying"
                    .into(),
            );
            return vec![];
        };
        self.next_operation_request_id = request_id.next();
        self.operation.retained = None;
        self.operation.pending = Some(PendingOperation::new(request_id, kind));
        self.operation.start_failure = None;
        vec![Effect::Start { request_id, kind }]
    }

    fn cancel(&mut self, choice: CancelChoice) -> Effect {
        let active = self.operation.active.as_mut().unwrap();
        active.cancelling = true;
        if active.cancel_choice != Some(CancelChoice::Destroy) {
            active.cancel_choice = Some(choice);
        }
        Effect::Cancel {
            request_id: active.request_id,
            choice: active.cancel_choice.expect("cancellation intent is set"),
        }
    }

    fn matching_active_mut(
        &mut self,
        request_id: OperationRequestId,
    ) -> Option<&mut ActiveOperation> {
        self.operation
            .active
            .as_mut()
            .filter(|active| active.request_id == request_id)
    }
}

fn command_succeeded(outcome: &CommandOutcome) -> bool {
    matches!(outcome, CommandOutcome::Exited { status, .. } if status.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    fn active_app() -> App {
        let mut app = App::new(8);
        app.operation.active = Some(ActiveOperation::new(
            OperationRequestId::FIRST,
            OperationKind::Launch,
        ));
        app.next_operation_request_id = OperationRequestId::FIRST.next();
        app
    }

    fn failed_outcome() -> CommandOutcome {
        CommandOutcome::SpawnFailed {
            message: "cancelled child settled".into(),
        }
    }

    /// Chooses an open confirmation's affirmative option, which sits just
    /// above the Back or Cancel option every dialog starts on.
    fn confirm(app: &mut App) -> Vec<Effect> {
        app.update(AppEvent::Action(Action::Scroll { delta: -1, page: false }));
        app.update(AppEvent::Action(Action::Activate))
    }

    fn finish(
        app: &mut App,
        request_id: OperationRequestId,
        outcome: CommandOutcome,
    ) -> Vec<Effect> {
        app.update(AppEvent::Operation {
            request_id,
            event: OperationEvent::Finished(outcome),
        })
    }

    #[test]
    fn new_operation_clears_retained_phase_history() {
        let mut app = active_app();
        finish(
            &mut app,
            OperationRequestId::FIRST,
            CommandOutcome::Exited {
                status: std::process::ExitStatus::from_raw(0),
                stderr_summary: vec![],
            },
        );
        assert!(app.operation.displayed().is_some());

        app.start(OperationKind::Route);

        assert!(app.operation.displayed().is_none());
    }

    #[test]
    fn cancel_and_destroy_waits_for_the_launch_and_keeps_the_tui_open() {
        let mut app = active_app();

        app.update(AppEvent::Action(Action::RequestCancelAndDestroy));
        assert_eq!(
            confirm(&mut app),
            vec![Effect::Cancel {
                request_id: OperationRequestId::FIRST,
                choice: CancelChoice::Destroy,
            }]
        );
        assert_eq!(
            app.update(AppEvent::ReconciliationStarted { at: Instant::now() }),
            vec![]
        );

        let destroy_request = OperationRequestId::FIRST.next().unwrap();
        assert_eq!(
            finish(&mut app, OperationRequestId::FIRST, failed_outcome()),
            vec![Effect::Start {
                request_id: destroy_request,
                kind: OperationKind::Destroy,
            }]
        );
        app.update(AppEvent::Operation {
            request_id: destroy_request,
            event: OperationEvent::Started { kind: OperationKind::Destroy },
        });
        assert_eq!(finish(&mut app, destroy_request, failed_outcome()), vec![]);
        assert_eq!((app.session.quitting, app.session.exit), (false, None));
    }

    /// Every dialog that destroys or kills opens on Back, so a stray Enter
    /// cannot confirm it.
    #[test]
    fn destructive_confirmations_open_on_back() {
        // Once a stop and destroy is in flight, `x` offers to force stop.
        let mut cancelling = active_app();
        cancelling.cancel(CancelChoice::Destroy);
        let cases = [
            (
                "quit and destroy",
                App::new(8),
                Action::RequestQuit,
                Confirmation::QuitAndDestroy,
            ),
            (
                "cancel and destroy",
                active_app(),
                Action::RequestCancelAndDestroy,
                Confirmation::CancelAndDestroy,
            ),
            (
                "force stop",
                cancelling,
                Action::RequestCancelAndDestroy,
                Confirmation::ForceStop,
            ),
        ];
        for (case, mut app, request, confirmation) in cases {
            app.update(AppEvent::Action(request));
            assert_eq!(app.session.confirmation, Some(confirmation), "{case}");

            let effects = app.update(AppEvent::Action(Action::Activate));

            assert_eq!(
                (effects, app.session.confirmation, app.session.quitting),
                (vec![], None, false),
                "{case}: Enter on the default option"
            );
        }
    }
}
