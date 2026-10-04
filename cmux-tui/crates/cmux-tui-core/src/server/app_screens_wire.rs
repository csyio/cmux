//! `app-screens-v1` on the raw wire (plans/cmux-next/app-screens.md):
//! `new-app-tab`, and the screen fields of the raw tree (`kind`, `app`,
//! `columns[0].app`). A connection that did not negotiate the capability
//! reads an app tab as a frontend `browser` tab
//! (conversation_tabs_wire.rs projection).

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{Mux, PaneId, WorkspaceId, paired_surface_size};
use crate::model::{Screen, State};
use crate::state::app_screens::AppTabTarget;
use crate::state::app_screens_store::{AppScreenKind, AppTabRecord};
use crate::workspace_registry::WorkspaceMutation;

/// `new-app-tab`: a tab showing `app` (at `route`), placed like
/// `new-frontend-browser-tab`. With `idempotency_key` a retry returns the
/// tab the first request created.
#[derive(Deserialize)]
pub(super) struct NewAppTabParams {
    app: String,
    #[serde(default)]
    route: Option<String>,
    #[serde(default)]
    pane: Option<PaneId>,
    /// A workspace to put the tab in (its first pane when it is empty).
    #[serde(default)]
    workspace: Option<WorkspaceId>,
    #[serde(default)]
    idempotency_key: Option<String>,
    #[serde(default)]
    cols: Option<u16>,
    #[serde(default)]
    rows: Option<u16>,
}

const NEW_APP_TAB_ORIGIN: &str = "new-app-tab";

pub(super) fn new_app_tab(mux: &Arc<Mux>, params: NewAppTabParams) -> anyhow::Result<Value> {
    let NewAppTabParams { app, route, pane, workspace, idempotency_key, cols, rows } = params;
    let target = match (pane, workspace) {
        (_, None) => AppTabTarget::Pane(pane),
        (None, Some(workspace)) => AppTabTarget::Workspace(workspace),
        (Some(_), Some(_)) => anyhow::bail!("bad request: send pane or workspace, not both"),
    };
    let mutation =
        idempotency_key.map(|key| WorkspaceMutation::new(key, NEW_APP_TAB_ORIGIN)).transpose()?;
    let size = paired_surface_size("new-app-tab", cols, rows)?;
    let record = AppTabRecord { app, route };
    let outcome = mux.new_app_tab(target, record, mutation.as_ref(), size)?;
    let identity = outcome.surface.resource_identity();
    Ok(json!({
        "surface": outcome.surface.id,
        "tab_resource_id": identity.map(|identity| identity.tab_id.as_str()),
        "content_resource_id": identity.map(|identity| identity.content_id.as_str()),
        "replayed": outcome.replayed,
    }))
}

/// The app fields of one raw screen: `kind` and `app` on an app screen, and
/// on an `appColumn` screen with columns `app` on column 0, the app column
/// (its left docked `sticky` flag is stored). An ordinary screen gets
/// nothing.
pub(super) fn merge_screen_fields(state: &State, screen: &Screen, value: &mut Value) {
    let Some(app) = state.resource_indexes.screen_apps.get(&screen.id) else { return };
    value["kind"] = json!(app.kind.as_str());
    value["app"] = json!(app.app);
    if app.kind != AppScreenKind::AppColumn {
        return;
    }
    if let Some(column) = value.get_mut("columns").and_then(|columns| columns.get_mut(0)) {
        column["app"] = json!(app.app);
    }
}
