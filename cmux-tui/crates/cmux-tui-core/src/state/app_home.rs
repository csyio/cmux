//! The Home workspace as an app workspace, and the companion workspace of an
//! app workspace (plans/cmux-next/app-screens.md, app-only model).
//!
//! `workspace.ensure_home {app}` makes the home workspace the app workspace
//! of the Home app. A home workspace that already holds tabs loses none:
//! its screens move into its companion workspace, an ordinary workspace
//! placed directly after it ("Home Tabs", clients localize it), and then
//! the home gets its app tab and kind. Steps, each resumable: (1) the
//! companion (empty, created once), (2) every home screen moves into it in
//! one commit, (3) the app tab, (4) the kind commit, which also writes the
//! home's `app_workspaces` row. Idempotent.
//!
//! A new tab sent to an app workspace (a workspace or screen target, not a
//! pane) goes to the same companion workspace, which is created when it is
//! missing ([`Mux::route_new_tab_to_companion`]).

use std::sync::Mutex;

use serde_json::json;

use super::{APP_MUTATION_ORIGIN, AppTabTarget, ENSURING};
use crate::mux::tab_drag::retarget_terminal_workspace;
use crate::mux::*;
use crate::state::app_screens_store::{
    AppScreenKind, AppTabRecord, ScreenApp, live_companion, validate_app_id, write_companion,
};
use crate::state::home_store::EmptyWorkspaceMark;
use crate::state::prelude::*;
use crate::workspace_registry::{PersonalWorkspaceUpdate, WorkspaceRegistry};

/// One companion creation at a time, so two racing new tabs make one.
static COMPANIONS: Mutex<()> = Mutex::new(());

const COMPANION_OPERATION: &str = "workspace.companion.fill";

impl Mux {
    /// `workspace.ensure_home {app}`: the home workspace `home` becomes the
    /// app workspace of `app` (section header).
    pub(crate) fn state_migrate_home(
        self: &Arc<Self>,
        home: &str,
        app: &str,
    ) -> anyhow::Result<()> {
        validate_app_id(app)?;
        let _ensuring = ENSURING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let workspace = self.workspace_slot_of(home)?;
        let screen_app = ScreenApp { kind: AppScreenKind::App, app: app.to_string() };
        let screens = self.with_state(|state| {
            let item = state.workspace_by_id(workspace)?;
            Some(
                item.screens
                    .iter()
                    .map(|screen| {
                        (screen.id, state.resource_indexes.screen_apps.get(&screen.id).cloned())
                    })
                    .collect::<Vec<_>>(),
            )
        });
        let screens = screens.context("the home workspace disappeared")?;
        if let Some(stored) = screens.iter().find_map(|(_, stored)| stored.clone()) {
            anyhow::ensure!(
                stored == screen_app,
                "bad request: the home workspace already shows app {}",
                stored.app
            );
            return Ok(());
        }
        // Resume after a stop between the app tab and the kind commit: a
        // lone screen that is exactly the Home app tab.
        if let [(screen, None)] = screens.as_slice()
            && self.is_app_shaped(*screen, app)
            && self.commit_screen_app(*screen, screen_app.clone()).is_ok()
        {
            return Ok(());
        }
        if !screens.is_empty() {
            let companion = self.ensure_companion(home)?;
            self.move_screens_to_companion(workspace, &companion)?;
        }
        let record = AppTabRecord { app: app.to_string(), route: None };
        let tab = self.new_app_tab(AppTabTarget::Workspace(workspace), record, None, None)?;
        let screen = self.screen_of_surface(tab.surface.id)?;
        self.commit_screen_app(screen, screen_app)?;
        Ok(())
    }

    /// Whether `screen` is one pane holding only an app tab of `app`.
    fn is_app_shaped(&self, screen: ScreenId, app: &str) -> bool {
        let Some(surface) = self.app_surface_in(screen, app) else { return false };
        self.with_state(|state| {
            let Some(item) = state
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.screens)
                .find(|candidate| candidate.id == screen)
            else {
                return false;
            };
            let panes = item.root.pane_ids_vec();
            item.layout_columns.is_empty()
                && matches!(panes.as_slice(), [pane] if state.panes.get(pane)
                    .is_some_and(|pane| pane.tabs == [surface]))
        })
    }

    /// The live companion workspace of the app workspace `app_workspace`
    /// (public id), created empty and placed directly after it when missing.
    pub(crate) fn ensure_companion(
        self: &Arc<Self>,
        app_workspace: &str,
    ) -> anyhow::Result<String> {
        let _companions = COMPANIONS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(companion) =
            self.read_registry_state(|connection| live_companion(connection, app_workspace))?
        {
            return Ok(companion);
        }
        let name = self
            .with_state(|state| {
                state
                    .workspaces
                    .iter()
                    .find(|item| item.public_id.as_str() == app_workspace)
                    .map(|item| format!("{} Tabs", item.name))
            })
            .context("the app workspace disappeared")?;
        let correlation = format!("companion-{}", WorkspacePublicId::random()?);
        self.resource_create_empty_workspace_selected(
            Self::ordinary_resource_selectors(),
            Some(name),
            &correlation,
            None,
            &WorkspaceMutation::local(APP_MUTATION_ORIGIN),
            EmptyWorkspaceMark::Companion(app_workspace.to_string()),
        )?;
        self.reload_presentation(&self.workspace_registry.lock().unwrap())?;
        self.emit(MuxEvent::TreeChanged);
        self.read_registry_state(|connection| live_companion(connection, app_workspace))?
            .context("the created companion workspace has no row")
    }

    /// Move every screen of `workspace` into the workspace `companion`
    /// (public id) in one commit; no tab is lost.
    fn move_screens_to_companion(
        self: &Arc<Self>,
        workspace: WorkspaceId,
        companion: &str,
    ) -> anyhow::Result<()> {
        let target = self.workspace_slot_of(companion)?;
        let fingerprint = json!({"operation": COMPANION_OPERATION, "workspace": companion});
        let commit = self.commit_resource_mutation_plan(
            &WorkspaceMutation::local(APP_MUTATION_ORIGIN),
            COMPANION_OPERATION,
            &fingerprint,
            None,
            None,
            |state, registry| {
                let mut projected = state.clone();
                let from = projected.workspace_index(workspace).context("home disappeared")?;
                let to = projected.workspace_index(target).context("companion disappeared")?;
                let screens = std::mem::take(&mut projected.workspaces[from].screens);
                let active = projected.workspaces[from].active_screen;
                projected.workspaces[from].active_screen = 0;
                let terminals = screens
                    .iter()
                    .flat_map(|screen| screen.root.pane_ids_vec())
                    .filter_map(|pane| projected.panes.get(&pane))
                    .flat_map(|pane| pane.tabs.iter())
                    .filter_map(|tab| projected.surfaces.get(tab))
                    .filter_map(|surface| surface.terminal_public_id().cloned())
                    .collect::<Vec<_>>();
                let destination = &mut projected.workspaces[to];
                if destination.screens.is_empty() {
                    destination.active_screen = active;
                }
                destination.screens.extend(screens);
                let key = destination.key.clone();
                Mux::rebuild_split_screen_index(&mut projected);
                let mut projection = self.resource_effect_projection_locked(
                    registry,
                    &mut projected,
                    json!({"workspace": companion}),
                )?;
                for terminal in &terminals {
                    retarget_terminal_workspace(&mut projection.patch, terminal, &key);
                }
                Ok(ResourceMutationPlan::new(
                    projection.patch,
                    projection.result,
                    projection.changes,
                    move |state| *state = projected,
                ))
            },
        )?;
        if !commit.replayed {
            self.emit(MuxEvent::TreeChanged);
        }
        Ok(())
    }

    /// The workspace that a new tab sent to `workspace` (no pane) lands in:
    /// its companion when `workspace` is an app workspace, else itself.
    pub(crate) fn new_tab_workspace(
        self: &Arc<Self>,
        workspace: WorkspaceId,
    ) -> anyhow::Result<Option<String>> {
        let app = self.with_state(|state| {
            crate::state::app_rules::is_app_workspace(state, workspace)
                .then(|| state.resource_indexes.workspace_ids.get(&workspace).cloned())
                .flatten()
        });
        match app {
            Some(public) => self.ensure_companion(public.as_str()).map(Some),
            None => Ok(None),
        }
    }
}

/// The companion's rows, in the commit that creates it: its
/// `app_companion_workspaces` row and its personal placement directly after
/// the app workspace, in the app workspace's group, with the placement
/// changes in the same `session.events` batch.
pub(crate) fn write_companion_mark(
    transaction: &rusqlite::Transaction<'_>,
    app_workspace: &str,
    workspace_id: &str,
    workspace_key: &str,
) -> anyhow::Result<()> {
    write_companion(transaction, app_workspace, workspace_id)?;
    let app_key: Option<String> = rusqlite::OptionalExtension::optional(transaction.query_row(
        "SELECT workspace_key FROM resource_workspaces WHERE public_id = ?1",
        [app_workspace],
        |row| row.get(0),
    ))?;
    let Some(app_key) = app_key else { return Ok(()) };
    let local = crate::state::values::local_registry_id(transaction)?;
    let rows = crate::workspace_registry::personal_store::read_workspaces(transaction)?;
    let Some(app_row) =
        rows.iter().position(|row| row.session_id == local && row.workspace_key == app_key)
    else {
        return Ok(());
    };
    let group = rows[app_row].group.clone();
    WorkspaceRegistry::set_personal_workspace_in(
        transaction,
        &local,
        workspace_key,
        PersonalWorkspaceUpdate {
            index: Some(app_row + 1),
            group: Some(group),
            ..Default::default()
        },
    )?;
    for placement in crate::state::personal_state_store::placement_snapshots(transaction)? {
        let id = crate::state::personal_state_store::placement_id(
            placement["workspace"]["session_id"].as_str().unwrap_or_default(),
            placement["workspace"]["workspace_ref"].as_str().unwrap_or_default(),
        );
        let change = crate::state::store::state_upsert("workspace_placement", &id, placement);
        crate::state::closed_history_store::queue_change(transaction, &change)?;
    }
    Ok(())
}
