//! The v2 operations of `workspace-kind-v1` and `app-screens-v1`:
//! `workspace.ensure_home {app?}`, `workspace.ensure_app {app, kind}`
//! and `tab.create_app {app, route?}` (plans/cmux-next/app-screens.md).

use std::sync::Arc;

use anyhow::Context;
use serde_json::{Value, json};

use crate::resource::ResourceOperation;
use crate::resource_router::{ParsedResourceRequest, mutation_result};
use crate::state::app_screens_store::{AppScreenKind, AppTabRecord};
use crate::workspace_registry::WorkspaceMutation;
use crate::{Mux, ResourceSelectors};

fn string(request: &ParsedResourceRequest, name: &str) -> Option<String> {
    request.fields.get(name).and_then(Value::as_str).map(str::to_string)
}

fn required(request: &ParsedResourceRequest, name: &str) -> anyhow::Result<String> {
    string(request, name).with_context(|| format!("bad request: {name} is required"))
}

fn mutation(request: &ParsedResourceRequest) -> anyhow::Result<WorkspaceMutation> {
    let key = request.envelope.idempotency_key.clone().context("mutations carry a key")?;
    WorkspaceMutation::new(key, "resource-api")
}

/// Map the router's typed error back through `anyhow` so `state_error`
/// keeps its code.
fn typed(error: crate::resource::ResourceError) -> anyhow::Error {
    anyhow::Error::new(error)
}

pub(super) fn dispatch(mux: &Arc<Mux>, request: &ParsedResourceRequest) -> anyhow::Result<Value> {
    match request.envelope.operation {
        ResourceOperation::WorkspaceEnsureHome => {
            let home = mux.state_ensure_home()?;
            // An `app-screens-v1` app makes the home workspace the app
            // workspace of its Home app (state/app_home.rs).
            if let Some(app) = string(request, "app") {
                mux.state_migrate_home(&home.workspace_id, &app)?;
            }
            let revision = mux.with_state(|state| state.resource_revision);
            mutation_result(
                mux,
                json!({"kind": "workspace", "workspace_id": home.workspace_id}),
                revision.max(home.revision),
                home.replayed,
            )
            .map_err(typed)
        }
        ResourceOperation::WorkspaceEnsureApp => {
            let kind = AppScreenKind::parse(&required(request, "kind")?)?;
            let app = mux.state_ensure_app(&required(request, "app")?, kind)?;
            mutation_result(
                mux,
                json!({"workspace_id": app.workspace_id, "screen_id": app.screen_id}),
                app.revision,
                app.replayed,
            )
            .map_err(typed)
        }
        ResourceOperation::TabCreateApp => {
            let record =
                AppTabRecord { app: required(request, "app")?, route: string(request, "route") };
            let selectors: ResourceSelectors = request.selectors.clone();
            let mutation = mutation(request)?;
            let expected =
                crate::resource_router::expected_revision(&request.fields).map_err(typed)?;
            let fields = request.fields.clone();
            let (surface, replayed) =
                mux.state_create_app_tab(selectors, record, fields, expected, &mutation)?;
            let value = mux
                .with_state(|state| created_app_path(state, surface))
                .context("the created app tab disappeared")?;
            let revision = mux.with_state(|state| state.resource_revision);
            mutation_result(mux, value, revision, replayed).map_err(typed)
        }
        operation => anyhow::bail!("app screens router received {}", operation.wire_name()),
    }
}

/// The `CreatedAppPath` of a tab.
fn created_app_path(state: &crate::model::State, surface: crate::SurfaceId) -> Option<Value> {
    let indexes = &state.resource_indexes;
    let pane = indexes.tab_pane.get(&surface)?;
    let screen = indexes.pane_screen.get(pane)?;
    let workspace = indexes.screen_workspace.get(screen)?;
    Some(json!({
        "kind": "app",
        "workspace_id": indexes.workspace_ids.get(workspace)?.as_str(),
        "screen_id": indexes.screen_ids.get(screen)?.as_str(),
        "pane_id": indexes.pane_ids.get(pane)?.as_str(),
        "tab_id": indexes.tab_ids.get(&surface)?.as_str(),
        "browser_id": indexes.content_ids.get(&surface)?.as_str(),
    }))
}
