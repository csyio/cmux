//! `workspace-kind-v1` in the raw tree: `Workspace.kind` is `"home"` for the
//! store's home workspace, `"app"` for an app workspace, `"app_tabs"` for
//! the companion of one (`app-screens-v1`) and `"normal"` otherwise.
//! `Workspace.app` names the app of an app workspace or a companion (null
//! otherwise).

use crate::workspace_registry::PresentationSnapshot;

pub(super) fn raw_workspace_kind(presentation: &PresentationSnapshot, key: &str) -> &'static str {
    if presentation.home_workspace.as_deref() == Some(key) {
        "home"
    } else if presentation.apps.workspaces.contains_key(key) {
        "app"
    } else if presentation.apps.companions.contains_key(key) {
        "app_tabs"
    } else {
        "normal"
    }
}

pub(super) fn raw_workspace_app<'a>(
    presentation: &'a PresentationSnapshot,
    key: &str,
) -> Option<&'a str> {
    let apps = &presentation.apps;
    apps.workspaces.get(key).or_else(|| apps.companions.get(key)).map(String::as_str)
}
