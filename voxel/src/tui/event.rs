// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The vocabulary shared by input, background tasks, the [`App`](super::App)
//! state machine and the effects runner: panes, key actions, confirmations
//! and their options, effects and application events.

use std::time::Instant;

use crate::tui::operation::{OperationEvent, OperationKind};
use crate::tui::reconcile::{ReconciliationResult, RouteEvidence};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DeploymentPane {
    OverallProgress,
    Phases,
    Status,
    CurrentPhase,
    Logs,
}

impl DeploymentPane {
    pub(crate) const ORDER: [Self; 5] = [
        Self::OverallProgress,
        Self::Phases,
        Self::Status,
        Self::CurrentPhase,
        Self::Logs,
    ];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Next,
    Previous,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Confirmation {
    Launch,
    Route,
    Detach,
    Quit,
    QuitAndDestroy,
    CancelAndDestroy,
    ForceStop,
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
const LAUNCH_OPTIONS: &[ConfirmationOption] = &[
    ConfirmationOption { label: "Launch deployment", action: Confirm },
    ConfirmationOption { label: "Cancel", action: Reject },
];
const ROUTE_OPTIONS: &[ConfirmationOption] = &[
    ConfirmationOption { label: "Apply routes", action: Confirm },
    ConfirmationOption { label: "Cancel", action: Reject },
];
const DETACH_OPTIONS: &[ConfirmationOption] = &[
    ConfirmationOption { label: "Leave and keep it running", action: Confirm },
    ConfirmationOption { label: "Back", action: Reject },
];
const QUIT_OPTIONS: &[ConfirmationOption] = &[
    ConfirmationOption { label: "Quit Voxel TUI", action: Confirm },
    ConfirmationOption { label: "Back", action: Reject },
];
const QUIT_AND_DESTROY_OPTIONS: &[ConfirmationOption] = &[
    ConfirmationOption {
        label: "Destroy the deployment and quit",
        action: Confirm,
    },
    ConfirmationOption { label: "Back", action: Reject },
];
const DESTROY_CANCEL_OPTIONS: &[ConfirmationOption] = &[
    ConfirmationOption {
        label: "Stop and destroy the deployment",
        action: Confirm,
    },
    ConfirmationOption { label: "Back", action: Reject },
];
const FORCE_STOP_OPTIONS: &[ConfirmationOption] = &[
    ConfirmationOption { label: "Force stop the command", action: Confirm },
    ConfirmationOption { label: "Back", action: Reject },
];
impl Confirmation {
    pub fn options(&self) -> &'static [ConfirmationOption] {
        match self {
            Self::Launch => LAUNCH_OPTIONS,
            Self::Route => ROUTE_OPTIONS,
            Self::Detach => DETACH_OPTIONS,
            Self::Quit => QUIT_OPTIONS,
            Self::QuitAndDestroy => QUIT_AND_DESTROY_OPTIONS,
            Self::CancelAndDestroy => DESTROY_CANCEL_OPTIONS,
            Self::ForceStop => FORCE_STOP_OPTIONS,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelChoice {
    Leave,
    Destroy,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct OperationRequestId(u64);

impl OperationRequestId {
    pub(crate) const FIRST: Self = Self(1);

    pub(crate) fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    Start { request_id: OperationRequestId, kind: OperationKind },
    Cancel { request_id: OperationRequestId, choice: CancelChoice },
    ForceStop { request_id: OperationRequestId },
    CopyToClipboard(String),
    Quit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    NextSection,
    PreviousSection,
    Activate,
    ToggleSection,
    ToggleHelp,
    Close,
    RequestLaunch,
    RequestRoute,
    RequestDetach,
    RequestQuit,
    RequestCancelAndDestroy,
    CopyReattachCommand,
    Reject,
    CycleLogFilter,
    Scroll { delta: isize, page: bool },
}

#[derive(Clone, Debug)]
pub enum AppEvent {
    Action(Action),
    Tick {
        now: Instant,
    },
    Resize {
        width: u16,
        height: u16,
    },
    ReconciliationStarted {
        at: Instant,
    },
    Reconciled {
        at: Instant,
        result: ReconciliationResult,
        routes: RouteEvidence,
    },
    ReconciliationFailed {
        at: Instant,
        message: String,
    },
    Operation {
        request_id: OperationRequestId,
        event: OperationEvent,
    },
    OperationStartFailed {
        request_id: OperationRequestId,
        kind: OperationKind,
        message: String,
    },
    DurableLogFailed {
        message: String,
    },
}

impl From<Action> for AppEvent {
    fn from(value: Action) -> Self {
        Self::Action(value)
    }
}
