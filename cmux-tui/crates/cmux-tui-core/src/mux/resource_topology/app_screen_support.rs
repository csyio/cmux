//! `app-screens-v1` support that needs the mux's creation fences.

use super::*;

impl Mux {
    /// The creation fences every topology effect holds from its pre-check to
    /// its commit (handoff, then execution). A screen kind commit takes them
    /// too, so it never lands between an effect's app-rule check and the
    /// effect, and the effect never changes memory that its commit check
    /// would then refuse.
    pub(crate) fn app_screen_fences(&self) -> (MutexGuard<'_, ()>, MutexGuard<'_, ()>) {
        let handoff = self.resource_creation_handoff.lock().unwrap();
        (handoff, self.resource_creation_execution.lock().unwrap())
    }
}

impl Mux {
    /// A new tab sent to an app workspace (a workspace, screen or session
    /// target, not a pane) goes to its companion ordinary workspace, made
    /// when missing. Other creations keep their selectors; a pane inside an
    /// app screen is refused by the effect's app rules.
    pub(crate) fn route_new_tab_to_companion(
        self: &Arc<Self>,
        operation: ResourceOperation,
        selectors: ResourceSelectors,
    ) -> anyhow::Result<ResourceSelectors> {
        if !matches!(
            operation,
            ResourceOperation::TabCreateTerminal | ResourceOperation::TabCreateBrowser
        ) || selectors.pane.is_some()
        {
            return Ok(selectors);
        }
        let target = if selectors.screen.is_some() {
            ResourceTarget::Screen
        } else if selectors.workspace.is_some() {
            ResourceTarget::Workspace
        } else {
            ResourceTarget::Session
        };
        let Ok(path) = self.resolve_resource_path(target, &selectors) else { return Ok(selectors) };
        let workspace = self.with_state(|state| match &path.workspace {
            Some(id) => state.resource_indexes.workspaces.get(id).copied(),
            None => state.workspaces.get(state.active_workspace).map(|workspace| workspace.id),
        });
        let Some(workspace) = workspace else { return Ok(selectors) };
        Ok(match self.new_tab_workspace(workspace)? {
            Some(companion) => ResourceSelectors {
                workspace: Some(companion),
                screen: None,
                pane: None,
                ..selectors
            },
            None => selectors,
        })
    }
}

#[cfg(test)]
#[path = "app_screen_race_tests.rs"]
mod tests;
