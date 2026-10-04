//! `app-screens-v1` operations (plans/cmux-next/app-screens.md sections 2
//! and 4): `workspace.ensure_app {app, kind}`, the Home migration of
//! `workspace.ensure_home {screen: "appColumn", app}`, and the `app` tab
//! (raw `new-app-tab`, v2 `tab.create_app`).
//!
//! `ensure_app` makes one workspace of kind `app` per app, holding one screen
//! of the asked kind with one `app` tab. Each step is its own commit and the
//! next call resumes from what exists, so a crash between commits is repaired
//! by the next call: (1) the workspace with its `app_workspaces` row, (2) the
//! app tab, which gives the workspace its screen and pane, (3) the screen's
//! kind row ([`Mux::commit_screen_app`]). The kind row is written last, so no
//! step before it is refused by the app rules.
//!
//! The Home migration turns the home workspace's first screen into an
//! `appColumn` screen: the Home app tab is created in that screen, moved into
//! its own column, and the kind commit moves that column to index 0, pins it
//! left (docked) and writes the kind row in one transaction. Every existing
//! pane stays, to the right of the app column.

use std::sync::Mutex;

use serde_json::{Map, json};

use crate::Surface;
use crate::model::{ColumnSticky, Screen, StickyEdge, StickyMode};
use crate::mux::*;
use crate::resource::BrowserPublicId;
use crate::state::app_rules::first_column_panes;
use crate::state::app_screens_store::{
    APP_TAB_ENGINE, APP_TAB_URL, AppScreenKind, AppTabRecord, ScreenApp, app_tab_for_mutation,
    live_app_workspace, validate_app_id, write_app_tab, write_screen_app,
};
use crate::state::home_store::EmptyWorkspaceMark;
use crate::state::prelude::*;
use crate::workspace_registry::FrontendBrowserRecord;

/// One `ensure_app` or Home migration at a time in this process, so two
/// concurrent calls for one app never make two workspaces.
static ENSURING: Mutex<()> = Mutex::new(());

const APP_MUTATION_ORIGIN: &str = "cmux-tui-app";
const SCREEN_APP_OPERATION: &str = "screen.app.set";

/// What `workspace.ensure_app` returns.
pub(crate) struct EnsuredApp {
    pub(crate) workspace_id: String,
    pub(crate) screen_id: String,
    pub(crate) revision: u64,
    pub(crate) replayed: bool,
}

/// Where a new app tab goes: a pane (or the focused pane), or a workspace,
/// which gets its first screen and pane when it has none.
#[derive(Debug, Clone, Copy)]
pub(crate) enum AppTabTarget {
    Pane(Option<PaneId>),
    Workspace(WorkspaceId),
}

/// A created or replayed app tab.
pub(crate) struct AppTabOutcome {
    pub(crate) surface: Arc<Surface>,
    pub(crate) replayed: bool,
}

impl Mux {
    pub(crate) fn state_ensure_app(
        self: &Arc<Self>,
        app: &str,
        kind: AppScreenKind,
    ) -> anyhow::Result<EnsuredApp> {
        validate_app_id(app)?;
        let _ensuring = ENSURING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let existing =
            self.read_registry_state(|connection| live_app_workspace(connection, app))?;
        let replayed = existing.is_some();
        let workspace_id = match existing {
            Some((workspace_id, _)) => workspace_id,
            None => {
                let mutation = WorkspaceMutation::local(APP_MUTATION_ORIGIN);
                let correlation = format!("app-{}", WorkspacePublicId::random()?);
                self.resource_create_empty_workspace_selected(
                    Self::ordinary_resource_selectors(),
                    Some(app.to_string()),
                    &correlation,
                    None,
                    &mutation,
                    EmptyWorkspaceMark::App(app.to_string()),
                )?;
                self.reload_presentation(&self.workspace_registry.lock().unwrap())?;
                self.emit(MuxEvent::TreeChanged);
                self.read_registry_state(|connection| live_app_workspace(connection, app))?
                    .context("the created app workspace has no app row")?
                    .0
            }
        };
        let workspace = self
            .with_state(|state| {
                state
                    .workspaces
                    .iter()
                    .find(|item| item.public_id.as_str() == workspace_id)
                    .map(|item| item.id)
            })
            .context("the app workspace disappeared")?;
        let screen = match self.app_screen_of(workspace, app) {
            Some((screen, stored)) => {
                if let Some(stored) = stored {
                    anyhow::ensure!(
                        stored.kind == kind,
                        "bad request: app {app} is open as kind {}",
                        stored.kind.as_str()
                    );
                    let screen_id = self.public_screen(screen)?;
                    let revision = self.with_state(|state| state.resource_revision);
                    return Ok(EnsuredApp { workspace_id, screen_id, revision, replayed });
                }
                screen
            }
            None => {
                let record = AppTabRecord { app: app.to_string(), route: None };
                let tab =
                    self.new_app_tab(AppTabTarget::Workspace(workspace), record, None, None)?;
                self.screen_of_surface(tab.surface.id)?
            }
        };
        let screen_app = ScreenApp { kind, app: app.to_string(), own_workspace: true };
        let commit = self.commit_screen_app(screen, screen_app)?;
        let screen_id = self.public_screen(screen)?;
        Ok(EnsuredApp { workspace_id, screen_id, revision: commit.revision, replayed: false })
    }

    /// The Home migration (section 4): the first screen of the home
    /// workspace `home` becomes `appColumn` with `app`'s column at index 0.
    /// Idempotent: a migrated screen is left as it is.
    pub(crate) fn state_migrate_home(
        self: &Arc<Self>,
        home: &str,
        app: &str,
    ) -> anyhow::Result<()> {
        validate_app_id(app)?;
        let _ensuring = ENSURING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let workspace = self
            .with_state(|state| {
                state
                    .workspaces
                    .iter()
                    .find(|item| item.public_id.as_str() == home)
                    .map(|item| item.id)
            })
            .context("the home workspace disappeared")?;
        let first = self.with_state(|state| {
            let screen = state.workspace_by_id(workspace)?.screens.first()?;
            Some((screen.id, state.resource_indexes.screen_apps.get(&screen.id).cloned()))
        });
        let record = AppTabRecord { app: app.to_string(), route: None };
        let screen_app = ScreenApp {
            kind: AppScreenKind::AppColumn,
            app: app.to_string(),
            own_workspace: false,
        };
        let screen = match first {
            Some((_, Some(stored))) => {
                anyhow::ensure!(
                    stored == screen_app,
                    "bad request: the home screen is already {} of {}",
                    stored.kind.as_str(),
                    stored.app
                );
                return Ok(());
            }
            None => {
                let tab =
                    self.new_app_tab(AppTabTarget::Workspace(workspace), record, None, None)?;
                let screen = self.screen_of_surface(tab.surface.id)?;
                self.commit_screen_app(screen, screen_app)?;
                return Ok(());
            }
            Some((screen, None)) => screen,
        };
        // Resume a migration a crash interrupted: reuse its app tab.
        let surface = match self.app_surface_in(screen, app) {
            Some(surface) => surface,
            None => {
                let pane = self.with_state(|state| {
                    state
                        .workspace_by_id(workspace)
                        .and_then(|item| item.screens.first())
                        .map(|item| item.active_pane)
                });
                self.new_app_tab(AppTabTarget::Pane(pane), record, None, None)?.surface.id
            }
        };
        // Give the app tab its own column unless it already has one alone.
        let (alone, anchor) = self
            .with_state(|state| {
                let pane = state.pane_of(surface)?;
                let (wi, si) = state.screen_of(pane)?;
                let screen = &state.workspaces[wi].screens[si];
                let own = state.panes.get(&pane).is_some_and(|item| item.tabs == [surface]);
                let column_alone =
                    match screen.layout_columns.iter().find(|c| c.root.contains(pane)) {
                        Some(column) => column.root.pane_ids_vec() == [pane],
                        None => screen.root.pane_ids_vec() == [pane],
                    };
                let other =
                    screen.root.pane_ids_vec().into_iter().find(|candidate| *candidate != pane);
                Some((own && column_alone, if own { other } else { Some(pane) }))
            })
            .context("the home app tab has no pane")?;
        if !alone {
            let anchor = anchor.context("the home app tab has no column anchor")?;
            self.move_tab_to_column(surface, anchor, None, None, None, None)?;
        }
        self.commit_screen_app(screen, screen_app)?;
        Ok(())
    }

    /// The screen of `workspace` that is or will become `app`'s screen, and
    /// its stored kind: a screen with a kind row for `app`, else the first
    /// screen whose column 0 is one pane with only an app tab of `app` (a
    /// creation a crash interrupted before its kind commit).
    fn app_screen_of(
        &self,
        workspace: WorkspaceId,
        app: &str,
    ) -> Option<(ScreenId, Option<ScreenApp>)> {
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
        })?;
        if let Some((screen, stored)) =
            screens.iter().find(|(_, stored)| stored.as_ref().is_some_and(|s| s.app == app))
        {
            return Some((*screen, stored.clone()));
        }
        screens
            .iter()
            .find(|(screen, _)| self.app_surface_in(*screen, app).is_some())
            .map(|(screen, _)| (*screen, None))
    }

    /// A tab of `screen` showing `app`.
    fn app_surface_in(&self, screen: ScreenId, app: &str) -> Option<SurfaceId> {
        let presentation = self.presentation_snapshot();
        self.with_state(|state| {
            let screen = state
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.screens)
                .find(|candidate| candidate.id == screen)?;
            screen.root.pane_ids_vec().into_iter().find_map(|pane| {
                state.panes.get(&pane)?.tabs.iter().copied().find(|tab| {
                    match state.resource_indexes.content_ids.get(tab) {
                        Some(ContentPublicId::Browser(browser)) => presentation
                            .apps
                            .tabs
                            .get(browser.as_str())
                            .is_some_and(|record| record.app == app),
                        _ => false,
                    }
                })
            })
        })
    }

    fn screen_of_surface(&self, surface: SurfaceId) -> anyhow::Result<ScreenId> {
        self.with_state(|state| {
            let pane = state.pane_of(surface)?;
            let (workspace, screen) = state.screen_of(pane)?;
            Some(state.workspaces[workspace].screens[screen].id)
        })
        .context("the app tab has no screen")
    }

    fn public_screen(&self, screen: ScreenId) -> anyhow::Result<String> {
        self.with_state(|state| {
            state.resource_indexes.screen_ids.get(&screen).map(ToString::to_string)
        })
        .context("the app screen has no public id")
    }

    /// One commit: `screen` becomes `screen_app` (its kind row, its index
    /// entry, and for `appColumn` the app tab's column moved to index 0 and
    /// pinned left, docked), with a fresh screen upsert in the same batch.
    fn commit_screen_app(
        self: &Arc<Self>,
        screen: ScreenId,
        screen_app: ScreenApp,
    ) -> anyhow::Result<ResourcePatchCommit> {
        let public = self.public_screen(screen)?;
        let fingerprint = json!({
            "operation": SCREEN_APP_OPERATION,
            "screen": public,
            "kind": screen_app.kind.as_str(),
            "app": screen_app.app,
        });
        let app_surface = self.app_surface_in(screen, &screen_app.app);
        let commit = self.commit_resource_mutation_plan(
            &WorkspaceMutation::local(APP_MUTATION_ORIGIN),
            SCREEN_APP_OPERATION,
            &fingerprint,
            None,
            None,
            |state, registry| {
                let (wi, si) = state
                    .workspaces
                    .iter()
                    .enumerate()
                    .find_map(|(wi, workspace)| {
                        let si = workspace.screens.iter().position(|item| item.id == screen)?;
                        Some((wi, si))
                    })
                    .context("the app screen disappeared")?;
                let mut projected = state.clone();
                let app_pane = app_surface.and_then(|surface| projected.pane_of(surface));
                let target = &mut projected.workspaces[wi].screens[si];
                if screen_app.kind == AppScreenKind::AppColumn
                    && let Some(pane) = app_pane
                {
                    arrange_app_column(target, pane);
                }
                // No undo entry from before the kind may restore another shape.
                target.invalidate_layout_undo();
                anyhow::ensure!(
                    first_column_panes(target).len() == 1,
                    "the app screen does not have its shape"
                );
                projected.resource_indexes.screen_apps.insert(screen, screen_app.clone());
                let projection = self.resource_effect_projection_locked(
                    registry,
                    &mut projected,
                    json!({"screen": public}),
                )?;
                let (row, id) = (screen_app.clone(), public.clone());
                Ok(ResourceMutationPlan::new(
                    projection.patch,
                    projection.result,
                    projection.changes,
                    move |state| *state = projected,
                )
                .with_state_write(Box::new(
                    move |transaction, _result, changes| {
                        write_screen_app(transaction, &id, &row)?;
                        changes.extend(crate::state::values::fresh_upserts(
                            transaction,
                            &[],
                            std::slice::from_ref(&id),
                            &[],
                        )?);
                        Ok(())
                    },
                )))
            },
        )?;
        if !commit.replayed {
            self.emit_screen_changed_for_transaction(&[screen], None);
            self.emit(MuxEvent::LayoutChanged(screen));
            self.emit(MuxEvent::TreeChanged);
        }
        Ok(commit)
    }

    pub(crate) fn new_app_tab(
        self: &Arc<Self>,
        target: AppTabTarget,
        record: AppTabRecord,
        mutation: Option<&WorkspaceMutation>,
        size: Option<(u16, u16)>,
    ) -> anyhow::Result<AppTabOutcome> {
        record.validate()?;
        let key = mutation.map(|mutation| (mutation.origin.as_str(), mutation.id.as_str()));
        let browser_id = match self.app_tab_browser(&record, key)? {
            AppTabBrowser::Placed(surface) => return Ok(AppTabOutcome { surface, replayed: true }),
            AppTabBrowser::Recorded(browser_id) => browser_id,
        };
        let fields = Map::from_iter([(
            "frontend_browser_id".to_string(),
            Value::String(browser_id.as_str().to_string()),
        )]);
        let created = match target {
            AppTabTarget::Pane(pane) => {
                self.new_browser_tab_with_fields(APP_TAB_URL.to_string(), pane, size, fields)
            }
            AppTabTarget::Workspace(workspace) => self.new_app_tab_in(workspace, fields),
        };
        match created {
            Ok(surface) => {
                self.publish_journal_event();
                Ok(AppTabOutcome { surface, replayed: false })
            }
            Err(error) => {
                // A keyed creation keeps its record, so a retry resumes it.
                if key.is_none() {
                    let mut registry = self.workspace_registry.lock().unwrap();
                    if registry.delete_frontend_browser(browser_id.as_str()).is_ok() {
                        let _ = self.reload_presentation(&registry);
                    }
                }
                Err(error)
            }
        }
    }

    /// The browser id of a new app tab: the recorded one of a keyed retry
    /// (or its live tab), else a new frontend row and app row in one commit.
    fn app_tab_browser(
        &self,
        record: &AppTabRecord,
        key: Option<(&str, &str)>,
    ) -> anyhow::Result<AppTabBrowser> {
        let recorded = match key {
            Some((origin, id)) => {
                self.read_registry_state(|connection| app_tab_for_mutation(connection, origin, id))?
            }
            None => None,
        };
        if let Some((browser_id, stored)) = recorded {
            anyhow::ensure!(
                stored == *record,
                "idempotency.conflict: the key named another app tab"
            );
            let placed = self.with_state(|state| {
                let content =
                    ContentPublicId::Browser(BrowserPublicId::parse(browser_id.clone()).ok()?);
                let surface = state.single_placement_of_content(&content)?;
                state.surfaces.get(&surface).cloned()
            });
            return Ok(match placed {
                Some(surface) => AppTabBrowser::Placed(surface),
                None => AppTabBrowser::Recorded(BrowserPublicId::parse(browser_id)?),
            });
        }
        let browser_id = BrowserPublicId::random()?;
        let frontend = FrontendBrowserRecord {
            engine: APP_TAB_ENGINE.to_string(),
            url: APP_TAB_URL.to_string(),
            title: None,
            favicon_url: None,
            profile_id: None,
            owner: None,
        };
        let id = browser_id.as_str().to_string();
        let write = |tx: &rusqlite::Transaction<'_>| write_app_tab(tx, &id, record, key);
        let mut registry = self.workspace_registry.lock().unwrap();
        registry.put_frontend_browser(browser_id.as_str(), &frontend, Some(&write))?;
        self.reload_presentation(&registry)?;
        Ok(AppTabBrowser::Recorded(browser_id))
    }

    /// An app tab in `workspace`'s active pane, or in a new first pane of an
    /// empty workspace.
    fn new_app_tab_in(
        self: &Arc<Self>,
        workspace: WorkspaceId,
        mut fields: Map<String, Value>,
    ) -> anyhow::Result<Arc<Surface>> {
        let selectors = self
            .ordinary_workspace_selectors(workspace)
            .with_context(|| format!("unknown workspace {workspace}"))?;
        fields.insert("url".into(), Value::String(APP_TAB_URL.to_string()));
        let operation = crate::resource::ResourceOperation::TabCreateBrowser;
        let commit = self.commit_ordinary_topology_operation(operation, selectors, fields)?;
        self.emit_resource_topology_legacy_events(operation, &commit);
        self.ordinary_created_surface(&commit)
    }

    /// v2 `tab.create_app`: the `tab.create_browser` creation path (with its
    /// `expected_revision`) and the app record committed first under the
    /// request's idempotency key.
    pub(crate) fn state_create_app_tab(
        self: &Arc<Self>,
        selectors: crate::ResourceSelectors,
        record: AppTabRecord,
        mut fields: Map<String, Value>,
        expected_revision: Option<u64>,
        mutation: &WorkspaceMutation,
    ) -> anyhow::Result<(SurfaceId, bool)> {
        record.validate()?;
        let key = Some((mutation.origin.as_str(), mutation.id.as_str()));
        let browser_id = match self.app_tab_browser(&record, key)? {
            AppTabBrowser::Placed(surface) => return Ok((surface.id, true)),
            AppTabBrowser::Recorded(browser_id) => browser_id,
        };
        fields.remove("app");
        fields.remove("route");
        fields.insert("url".into(), Value::String(APP_TAB_URL.to_string()));
        fields.insert("frontend_browser_id".into(), Value::String(browser_id.as_str().into()));
        let operation = crate::resource::ResourceOperation::TabCreateBrowser;
        let commit = self.resource_topology_operation(
            operation,
            selectors,
            fields,
            expected_revision,
            mutation,
        )?;
        if !commit.replayed {
            self.publish_journal_event();
        }
        Ok((self.resource_surface_for_created_path(&commit.result)?, commit.replayed))
    }
}

enum AppTabBrowser {
    /// A keyed retry whose tab exists.
    Placed(Arc<Surface>),
    /// The browser id to create the tab under.
    Recorded(BrowserPublicId),
}

/// Move the column holding `app_pane` to index 0, pin it left (docked) and
/// take the left flag from any other column. A screen without columns is the
/// app column alone and stays as it is.
fn arrange_app_column(screen: &mut Screen, app_pane: PaneId) {
    let Some(index) =
        screen.layout_columns.iter().position(|column| column.root.contains(app_pane))
    else {
        return;
    };
    let column = screen.layout_columns.remove(index);
    screen.layout_columns.insert(0, column);
    for column in screen.layout_columns.iter_mut().skip(1) {
        if column.sticky.is_some_and(|sticky| sticky.edge == StickyEdge::Left) {
            column.sticky = None;
        }
    }
    screen.layout_columns[0].sticky =
        Some(ColumnSticky { edge: StickyEdge::Left, mode: StickyMode::Docked });
    screen.sync_layout_column_projection();
}
