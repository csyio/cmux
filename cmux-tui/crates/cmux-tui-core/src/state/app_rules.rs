//! The one validator of `app-screens-v1` (plans/cmux-next/app-screens.md
//! section 2). Every command shape maps to a place and a
//! [`cmux_layout_reducer::AppAction`], and the reducer's
//! [`cmux_layout_reducer::check_app_target`] decides; the reducer's own
//! `apply` uses the same table for layout ops. Callers:
//!
//! - the topology effect intent (raw `new-tab`, `split`, `new-pane`,
//!   `new-pane-right`, `new-row`, `close-surface`, `close-pane` and the v2
//!   `tab.create_*`, `pane.create`, `pane.split`, `pane.close`, `tab.close`,
//!   `workspace.layout.apply`): [`refuse_effect`];
//! - every staged layout op (raw `move-tab*`, v2 `tab.move`): [`refuse_op`];
//! - `pane.swap`, `column.update`, `set-column-sticky`, `apply-layout` and
//!   `close-tabs` call [`refuse`] or [`refuse_column`] directly.
//!
//! A refusal is checked before the change, so it changes nothing (A4).

use cmux_layout_reducer::{AppAction, AppRefusal, LayoutOpKind, LayoutState, Reject, ScreenKind};
use rusqlite::Connection;
use serde_json::{Map, Value};

use crate::model::State;
use crate::model::{ColumnSticky, StickyEdge};
use crate::resource::{ContentPublicId, ResourceOperation};
use crate::resource_selector::ResolvedResourceSlots;
use crate::state::app_screens_store::{
    AppRule, AppScreenKind, ScreenApp, read_app_tabs, read_screen_apps, workspace_app,
};
use crate::{PaneId, ScreenId, SurfaceId, WorkspaceId};

/// Where an action lands.
#[derive(Debug, Clone, Copy)]
pub(crate) enum AppPlace {
    Pane(PaneId),
    Tab(SurfaceId),
    /// A screen-level creation lands next to the screen's active pane.
    Screen(ScreenId),
    /// A workspace-level creation lands in its active screen.
    Workspace(WorkspaceId),
}

/// The reducer kind of a screen.
pub(crate) fn reducer_kind(state: &State, screen: ScreenId) -> ScreenKind {
    state
        .resource_indexes
        .screen_apps
        .get(&screen)
        .map_or(ScreenKind::Workspace, |screen| screen.kind.reducer())
}

/// The pane ids of a screen's column 0 (its whole tree without columns).
pub(crate) fn first_column_panes(screen: &crate::model::Screen) -> Vec<PaneId> {
    match screen.layout_columns.first() {
        Some(column) => column.root.pane_ids_vec(),
        None => screen.root.pane_ids_vec(),
    }
}

/// `pane`'s screen and whether `pane` is in column 0.
fn pane_place(state: &State, pane: PaneId) -> Option<(ScreenId, bool)> {
    let (workspace, screen) = state.screen_of(pane)?;
    let screen = &state.workspaces[workspace].screens[screen];
    Some((screen.id, first_column_panes(screen).contains(&pane)))
}

fn rule(state: &State, refusal: AppRefusal, screen: ScreenId) -> anyhow::Error {
    let public = state.resource_indexes.screen_ids.get(&screen).map(ToString::to_string);
    AppRule::new(refusal, public.unwrap_or_default()).into()
}

/// [`cmux_layout_reducer::check_app_target`] at a known screen.
pub(crate) fn refuse_at(
    state: &State,
    screen: ScreenId,
    in_first_column: bool,
    action: AppAction,
) -> anyhow::Result<()> {
    let kind = reducer_kind(state, screen);
    let in_app_column = in_first_column && kind == ScreenKind::AppColumn;
    cmux_layout_reducer::check_app_target(kind, in_app_column, action)
        .map_err(|refusal| rule(state, refusal, screen))
}

/// Refuse `action` at `place` when the rule table says so. Unknown places
/// pass: the command reports them itself.
pub(crate) fn refuse(state: &State, place: AppPlace, action: AppAction) -> anyhow::Result<()> {
    let target = match place {
        AppPlace::Pane(pane) => pane_place(state, pane),
        AppPlace::Tab(tab) => state.pane_of(tab).and_then(|pane| pane_place(state, pane)),
        AppPlace::Screen(screen) => {
            let active = state
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.screens)
                .find(|candidate| candidate.id == screen)
                .map(|candidate| candidate.active_pane);
            active.and_then(|pane| pane_place(state, pane)).or(Some((screen, false)))
        }
        AppPlace::Workspace(workspace) => return refuse_workspace(state, workspace, action),
    };
    match target {
        Some((screen, in_first_column)) => refuse_at(state, screen, in_first_column, action),
        None => Ok(()),
    }
}

fn refuse_workspace(
    state: &State,
    workspace: WorkspaceId,
    action: AppAction,
) -> anyhow::Result<()> {
    let Some(workspace) = state.workspace_by_id(workspace) else { return Ok(()) };
    if action == AppAction::ApplyLayout {
        // The layout replaces every screen of the workspace.
        for screen in &workspace.screens {
            refuse_at(state, screen.id, false, action)?;
        }
        return Ok(());
    }
    match workspace.active_screen_ref() {
        Some(screen) => refuse(state, AppPlace::Screen(screen.id), action),
        None => Ok(()),
    }
}

/// A sticky flag change of column `index` of `screen`.
pub(crate) fn refuse_column(
    state: &State,
    screen: ScreenId,
    index: usize,
    sticky: Option<ColumnSticky>,
) -> anyhow::Result<()> {
    let left = sticky.is_some_and(|sticky| sticky.edge == StickyEdge::Left);
    refuse_at(state, screen, index == 0, AppAction::Sticky { left })
}

/// Raw `move-tab`, which reports a refused move as `moved: false`: the tab
/// leaves its pane and enters `pane`.
pub(crate) fn refuse_move_tab(state: &State, tab: SurfaceId, pane: PaneId) -> anyhow::Result<()> {
    if state.pane_of(tab) == Some(pane) {
        return Ok(());
    }
    refuse(state, AppPlace::Tab(tab), AppAction::MoveTabOut)?;
    refuse(state, AppPlace::Pane(pane), AppAction::AddTab)
}

/// `pane.swap` and raw `swap-pane`: both panes move.
pub(crate) fn refuse_swap(state: &State, first: PaneId, second: PaneId) -> anyhow::Result<()> {
    refuse(state, AppPlace::Pane(first), AppAction::Reorder)?;
    refuse(state, AppPlace::Pane(second), AppAction::Reorder)
}

/// Raw `apply-layout` into `workspace` (the active one when `None`).
pub(crate) fn refuse_apply_layout(
    state: &State,
    workspace: Option<WorkspaceId>,
) -> anyhow::Result<()> {
    match workspace {
        Some(workspace) => refuse(state, AppPlace::Workspace(workspace), AppAction::ApplyLayout),
        None => Ok(()),
    }
}

/// The topology effects: creations, splits, closes, layout replacement,
/// new screens and layout undo.
pub(crate) fn refuse_effect(
    state: &State,
    operation: ResourceOperation,
    resolved: &ResolvedResourceSlots,
    fields: &Map<String, Value>,
) -> anyhow::Result<()> {
    use ResourceOperation as Op;
    let action = match operation {
        Op::TabCreateTerminal | Op::TabCreateBrowser | Op::PaneRun => AppAction::AddTab,
        Op::PaneCreate => AppAction::Split,
        // A viewport column right of the target's column leaves that column
        // as it is; every other split adds a pane inside it.
        Op::PaneSplit
            if fields.contains_key("viewport_width")
                && fields.get("direction").and_then(Value::as_str) == Some("right") =>
        {
            AppAction::AddColumn
        }
        Op::PaneSplit => AppAction::Split,
        Op::PaneClose => AppAction::ClosePane,
        Op::TabClose => AppAction::CloseTab,
        Op::WorkspaceLayoutApply => AppAction::ApplyLayout,
        Op::ScreenCreate => return refuse_new_screen(state, resolved.workspace),
        Op::ScreenLayoutUndo => return refuse_undo(state, resolved.screen),
        _ => return Ok(()),
    };
    let place = match (resolved.tab, resolved.pane, resolved.screen, resolved.workspace) {
        (Some(tab), ..) => AppPlace::Tab(tab),
        (None, Some(pane), ..) => AppPlace::Pane(pane),
        (None, None, Some(screen), _) => AppPlace::Screen(screen),
        (None, None, None, Some(workspace)) => AppPlace::Workspace(workspace),
        (None, None, None, None) => return Ok(()),
    };
    let place = match (operation, place) {
        (Op::WorkspaceLayoutApply, _) => match resolved.workspace {
            Some(workspace) => AppPlace::Workspace(workspace),
            None => return Ok(()),
        },
        // A new tab sent to a workspace or screen, not to a pane, goes to an
        // ordinary column of an appColumn screen ([`new_tab_target`]).
        (_, AppPlace::Screen(_) | AppPlace::Workspace(_))
            if action == AppAction::AddTab && routed_screen(state, resolved).is_some() =>
        {
            return Ok(());
        }
        (_, place) => place,
    };
    refuse(state, place, action)
}

/// The appColumn screen a workspace- or screen-level new tab lands on: the
/// active screen of the resolved workspace, as the creation uses it.
fn routed_screen(state: &State, resolved: &ResolvedResourceSlots) -> Option<ScreenId> {
    let workspace = state.workspace_by_id(resolved.workspace?)?;
    let screen = workspace.active_screen_ref()?;
    (reducer_kind(state, screen.id) == ScreenKind::AppColumn).then_some(screen.id)
}

/// A workspace of kind `app` keeps exactly its one app screen.
fn refuse_new_screen(state: &State, workspace: Option<WorkspaceId>) -> anyhow::Result<()> {
    let Some(workspace) = workspace.and_then(|workspace| state.workspace_by_id(workspace)) else {
        return Ok(());
    };
    let own = workspace.screens.iter().find(|screen| {
        state.resource_indexes.screen_apps.get(&screen.id).is_some_and(|app| app.own_workspace)
    });
    match own {
        Some(screen) => Err(rule(state, AppRefusal::ScreenFixed, screen.id)),
        None => Ok(()),
    }
}

/// Layout undo never restores a shape that breaks A1/A2: an app screen has
/// nothing to undo, and an appColumn screen's undo must restore the same app
/// column.
fn refuse_undo(state: &State, screen: Option<ScreenId>) -> anyhow::Result<()> {
    let Some(screen_id) = screen else { return Ok(()) };
    let screen = state
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.screens)
        .find(|candidate| candidate.id == screen_id);
    let Some(screen) = screen else { return Ok(()) };
    match reducer_kind(state, screen_id) {
        ScreenKind::Workspace => Ok(()),
        ScreenKind::App => Err(rule(state, AppRefusal::ScreenFixed, screen_id)),
        ScreenKind::AppColumn => {
            let Some(entry) = screen.layout_undo.back() else { return Ok(()) };
            let restored = match entry.before.layout_columns.first() {
                Some(column) => column.root.pane_ids_vec(),
                None => entry.before.root.pane_ids_vec(),
            };
            if restored == first_column_panes(screen) {
                Ok(())
            } else {
                Err(rule(state, AppRefusal::ColumnLocked, screen_id))
            }
        }
    }
}

/// Raw `new-tab` and `new-browser-tab` without a pane: the focused pane,
/// unless it is in an app column; then the creation goes to the focused
/// workspace and [`new_tab_target`] picks an ordinary column.
pub(crate) fn focused_ordinary_pane(state: &State) -> Option<PaneId> {
    let pane = state.active_pane()?;
    let (screen, in_first) = pane_place(state, pane)?;
    let in_app_column = in_first && reducer_kind(state, screen) == ScreenKind::AppColumn;
    (!in_app_column).then_some(pane)
}

/// The pane a workspace-level new tab goes to, and whether a column was
/// made for it: [`route_new_tab_pane`] of the workspace's active pane.
pub(crate) fn new_tab_target(
    state: &mut State,
    workspace_index: usize,
    next_id: impl FnMut() -> u64,
) -> anyhow::Result<(Option<PaneId>, bool)> {
    let workspace = &state.workspaces[workspace_index];
    let Some(active) = workspace.active_screen_ref().map(|screen| screen.active_pane) else {
        return Ok((None, false));
    };
    let (pane, created) = route_new_tab_pane(state, active, next_id)?;
    Ok((Some(pane), created))
}

/// Decision 2026-10-04: a new tab is never added to an app column. A new tab
/// headed for `pane` (a workspace's or screen's focused pane; a creation that
/// names an app column pane is refused before it gets here) goes to the
/// first pane of the first ordinary column, or to a new empty pane in a new
/// ordinary column right of the app column, which the caller fills in the
/// same lock. Returns the pane and whether a column was made.
pub(crate) fn route_new_tab_pane(
    state: &mut State,
    pane: PaneId,
    mut next_id: impl FnMut() -> u64,
) -> anyhow::Result<(PaneId, bool)> {
    let Some((workspace_index, screen_index)) = state.screen_of(pane) else {
        return Ok((pane, false));
    };
    let screen = &state.workspaces[workspace_index].screens[screen_index];
    let screen_id = screen.id;
    if reducer_kind(state, screen_id) != ScreenKind::AppColumn
        || !first_column_panes(screen).contains(&pane)
    {
        return Ok((pane, false));
    }
    if let Some(column) = screen.layout_columns.get(1) {
        return Ok((column.root.first_visible_pane(), false));
    }
    let created = next_id();
    state.insert_pane(crate::model::Pane {
        id: created,
        public_id: crate::resource::PanePublicId::random()?,
        name: None,
        tabs: Vec::new(),
        active_tab: 0,
        active_at: 0,
        focused_at: 0,
    });
    let (column_id, base) = (next_id(), next_id());
    let screen = &mut state.workspaces[workspace_index].screens[screen_index];
    let column = crate::model::LayoutColumn::single(column_id, 0.5, created);
    anyhow::ensure!(
        screen.insert_layout_column_after(pane, base, column),
        "the app column disappeared while placing a new tab"
    );
    state.resource_indexes.pane_screen.insert(created, screen_id);
    // The new column's split in the compat chain; a full index rebuild here
    // would drop the identity of the tab the caller has not placed yet.
    state.split_screens.insert(column_id, (workspace_index, screen_index, screen_id));
    Ok((created, true))
}

/// A layout op of a staged plan, on `model`, the projection of `state`.
pub(crate) fn refuse_op(
    state: &State,
    model: &LayoutState,
    kind: &LayoutOpKind,
) -> anyhow::Result<()> {
    match cmux_layout_reducer::check_app_op(model, kind) {
        Ok(()) => Ok(()),
        Err(Reject::AppScreenFixed(screen)) => Err(rule(state, AppRefusal::ScreenFixed, screen)),
        Err(Reject::AppColumnLocked(screen)) => Err(rule(state, AppRefusal::ColumnLocked, screen)),
        Err(_) => Ok(()),
    }
}

/// A1/A2 broken by a staged layout change (an introduced
/// `Violation::AppScreenShape` of the reducer's check): the refusal of the
/// screen's kind, so a tab-group or screen move keeps the typed error.
pub(crate) fn shape_rule(state: &State, screen: ScreenId) -> anyhow::Error {
    let refusal = match reducer_kind(state, screen) {
        ScreenKind::AppColumn => AppRefusal::ColumnLocked,
        _ => AppRefusal::ScreenFixed,
    };
    rule(state, refusal, screen)
}

/// The raw-only mapping of an app reject from the reducer, where the public
/// screen id is not at hand (the `model_result` check of a respawn drag).
pub(crate) fn reject_rule(reject: &Reject) -> Option<anyhow::Error> {
    match reject {
        Reject::AppScreenFixed(_) => {
            Some(AppRule::new(AppRefusal::ScreenFixed, String::new()).into())
        }
        Reject::AppColumnLocked(_) => {
            Some(AppRule::new(AppRefusal::ColumnLocked, String::new()).into())
        }
        _ => None,
    }
}

/// Whether `screen` still has the shape of `app` (A1/A2): column 0 (the
/// whole screen for an `app` screen) is one pane holding one `app` tab of
/// the screen's app.
fn has_shape(
    state: &State,
    screen: &crate::model::Screen,
    app: &ScreenApp,
    app_tabs: &std::collections::HashMap<String, crate::state::app_screens_store::AppTabRecord>,
) -> bool {
    if app.kind == AppScreenKind::App && !screen.layout_columns.is_empty() {
        return false;
    }
    let panes = first_column_panes(screen);
    let [pane] = panes.as_slice() else { return false };
    let Some(pane) = state.panes.get(pane) else { return false };
    let [tab] = pane.tabs.as_slice() else { return false };
    match state.resource_indexes.content_ids.get(tab) {
        Some(ContentPublicId::Browser(browser)) => {
            app_tabs.get(browser.as_str()).is_some_and(|record| record.app == app.app)
        }
        _ => false,
    }
}

/// Overlay the stored screen kinds on the restored state. A row whose
/// screen is gone, or which lost its shape in an older build, is deleted:
/// the screen loads as an ordinary screen and keeps every tab.
pub(crate) fn load_screen_apps(state: &mut State, connection: &Connection) -> anyhow::Result<()> {
    let rows = read_screen_apps(connection)?;
    if rows.is_empty() {
        return Ok(());
    }
    let app_tabs = read_app_tabs(connection)?;
    let mut loaded = std::collections::HashMap::new();
    for (public, app) in rows {
        let screen = state
            .resource_indexes
            .screens
            .iter()
            .find(|(id, _)| id.as_str() == public)
            .map(|(_, screen)| *screen);
        let valid = screen.is_some_and(|screen| {
            state
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.screens)
                .find(|candidate| candidate.id == screen)
                .is_some_and(|candidate| has_shape(state, candidate, &app, &app_tabs))
        });
        match screen.filter(|_| valid) {
            Some(screen) => {
                let workspace = state.resource_indexes.screen_workspace.get(&screen);
                let workspace =
                    workspace.and_then(|id| state.resource_indexes.workspace_ids.get(id));
                let own_workspace = match workspace {
                    Some(id) => workspace_app(connection, id.as_str())?.is_some(),
                    None => false,
                };
                loaded.insert(screen, ScreenApp { own_workspace, ..app });
            }
            None => {
                connection
                    .execute("DELETE FROM resource_screen_kinds WHERE screen_id = ?1", [&public])?;
            }
        }
    }
    state.resource_indexes.screen_apps = loaded;
    Ok(())
}
