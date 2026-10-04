//! App screens (plans/cmux-next/app-screens.md, `app-screens-v1`).
//!
//! A screen has a [`ScreenKind`]. An `App` screen holds exactly one pane with
//! one tab (the app). An `AppColumn` screen has the app column at index 0
//! (the whole screen while it has no other column); its other columns are
//! ordinary. [`check_app_target`] is the one rule table for every command
//! shape: the daemon calls it for raw commands and v2 operations alike, and
//! [`apply`](crate::apply) calls it through [`check_app_op`] for every layout op.
//!
//! Invariants (with the reducer tests):
//! - **A1.** An `App` screen has one column, one pane, one tab.
//! - **A2.** An `AppColumn` screen's column 0 has one pane with one tab.
//! - **A3.** Only these kinds hold an app column: it is derived from the kind,
//!   never stored on a `Workspace` screen.
//! - **A4.** A refused op changes nothing ([`Reject::AppScreenFixed`],
//!   [`Reject::AppColumnLocked`]).
//!
//! A1 and A2 are structural here; the daemon also checks that the one tab is
//! an `app` tab for the screen's own app.

use std::collections::BTreeSet;

use crate::{LayoutOpKind, LayoutState, PaneId, Reject, Screen, ScreenId, Violation};

/// The kind of a screen. `Workspace` is today's screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScreenKind {
    #[default]
    Workspace,
    App,
    AppColumn,
}

/// What a command does at its target pane, column or screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppAction {
    /// A new or moved tab enters the target pane.
    AddTab,
    /// A new pane or row inside the target's column (or split tree).
    Split,
    /// A new column right of the target's column.
    AddColumn,
    /// A tab leaves the target pane.
    MoveTabOut,
    /// The target tab closes.
    CloseTab,
    /// The target pane closes.
    ClosePane,
    /// The target column's sticky flag changes; `left` when the new flag is
    /// the left edge, which the app column holds.
    Sticky { left: bool },
    /// The target pane or column swaps or moves.
    Reorder,
    /// The screen's whole layout is replaced.
    ApplyLayout,
    /// The target column's width changes.
    Width,
    /// The screen closes.
    CloseScreen,
}

/// Why [`check_app_target`] refuses an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppRefusal {
    /// `app-screen-fixed`: the target is in an `App` screen.
    ScreenFixed,
    /// `app-column-locked`: the action would change the app column.
    ColumnLocked,
}

impl AppRefusal {
    /// The reject of this refusal on `screen`.
    pub fn on(self, screen: ScreenId) -> Reject {
        match self {
            Self::ScreenFixed => Reject::AppScreenFixed(screen),
            Self::ColumnLocked => Reject::AppColumnLocked(screen),
        }
    }
}

/// The one rule table: whether `action` may run at a target on a screen of
/// `kind`, where `in_app_column` says whether the target is in the app
/// column of an `AppColumn` screen.
pub fn check_app_target(
    kind: ScreenKind,
    in_app_column: bool,
    action: AppAction,
) -> Result<(), AppRefusal> {
    use AppAction as A;
    match (kind, action) {
        (ScreenKind::Workspace, _) | (_, A::Width | A::CloseScreen) => Ok(()),
        (ScreenKind::App, _) => Err(AppRefusal::ScreenFixed),
        // A column right of the app column leaves the app column as it is.
        (ScreenKind::AppColumn, A::AddColumn) => Ok(()),
        // Replacing the layout or pinning another column left would remove
        // or displace the app column.
        (ScreenKind::AppColumn, A::ApplyLayout | A::Sticky { left: true }) => {
            Err(AppRefusal::ColumnLocked)
        }
        (ScreenKind::AppColumn, _) if in_app_column => Err(AppRefusal::ColumnLocked),
        (ScreenKind::AppColumn, _) => Ok(()),
    }
}

/// `pane`'s screen, its kind, and whether `pane` is in its app column.
pub fn pane_target(state: &LayoutState, pane: PaneId) -> Option<(ScreenId, ScreenKind, bool)> {
    let slot = state.slot(pane)?;
    let screen = &state.workspaces[slot.workspace].screens[slot.screen];
    Some((screen.id, screen.kind, screen.kind == ScreenKind::AppColumn && slot.column == 0))
}

fn check_pane(state: &LayoutState, pane: PaneId, action: AppAction) -> Result<(), Reject> {
    match pane_target(state, pane) {
        Some((screen, kind, in_app_column)) => {
            check_app_target(kind, in_app_column, action).map_err(|refusal| refusal.on(screen))
        }
        None => Ok(()),
    }
}

fn check_tab(state: &LayoutState, tab: u64, action: AppAction) -> Result<(), Reject> {
    match state.pane_of(tab) {
        Some(pane) => check_pane(state, pane, action),
        None => Ok(()),
    }
}

/// [`check_app_target`] for every place a layout op touches. Unknown ids
/// pass: the op itself rejects them.
pub fn check_app_op(state: &LayoutState, op: &LayoutOpKind) -> Result<(), Reject> {
    use AppAction as A;
    match op {
        LayoutOpKind::MoveTab { tab, pane, .. } => {
            if state.pane_of(*tab) == Some(*pane) {
                return Ok(());
            }
            check_tab(state, *tab, A::MoveTabOut)?;
            check_pane(state, *pane, A::AddTab)
        }
        LayoutOpKind::MoveTabToSplit { tab, pane, .. } => {
            check_tab(state, *tab, A::MoveTabOut)?;
            check_pane(state, *pane, A::Split)
        }
        LayoutOpKind::MoveTabToColumn { tab, anchor, .. } => {
            check_tab(state, *tab, A::MoveTabOut)?;
            check_pane(state, *anchor, A::AddColumn)
        }
        LayoutOpKind::MoveTabToNewWorkspace { tab, .. } => check_tab(state, *tab, A::MoveTabOut),
        LayoutOpKind::MoveTabToWorkspace { tab, pane, .. } => {
            check_tab(state, *tab, A::MoveTabOut)?;
            pane.map_or(Ok(()), |pane| check_pane(state, pane, A::AddTab))
        }
        LayoutOpKind::InsertRow { after_pane, .. } => check_pane(state, *after_pane, A::Split),
        LayoutOpKind::MoveTabToRow { tab, anchor, .. } => {
            check_tab(state, *tab, A::MoveTabOut)?;
            check_pane(state, *anchor, A::Split)
        }
        LayoutOpKind::SetRowHeights { column, .. } | LayoutOpKind::FlattenRows { column } => {
            let pane = state.workspaces.iter().flat_map(|workspace| &workspace.screens).find_map(
                |screen| {
                    let found = screen.columns.iter().find(|candidate| candidate.id == *column)?;
                    found.panes.first().copied()
                },
            );
            pane.map_or(Ok(()), |pane| check_pane(state, pane, A::Split))
        }
        LayoutOpKind::CloseTab { tab } => check_tab(state, *tab, A::CloseTab),
        LayoutOpKind::RuntimeExited { .. } => Ok(()),
    }
}

/// A1 and A2: the structural shape of every app screen.
pub(crate) fn shape_violations(state: &LayoutState) -> BTreeSet<Violation> {
    let one_tab = |pane: &PaneId| state.panes.get(pane).is_some_and(|tabs| tabs.len() == 1);
    let lone_pane = |screen: &Screen, column: usize| {
        screen.columns.get(column).is_some_and(|column| match column.panes.as_slice() {
            [pane] => one_tab(pane),
            _ => false,
        })
    };
    state
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.screens)
        .filter(|screen| match screen.kind {
            ScreenKind::Workspace => false,
            ScreenKind::App => screen.columns.len() != 1 || !lone_pane(screen, 0),
            ScreenKind::AppColumn => !lone_pane(screen, 0),
        })
        .map(|screen| Violation::AppScreenShape { screen: screen.id })
        .collect()
}

#[cfg(test)]
#[path = "app_screens_tests.rs"]
mod tests;
