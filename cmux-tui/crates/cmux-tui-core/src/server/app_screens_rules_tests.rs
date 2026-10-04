//! `app-screens-v1` rules on the remaining paths: the authoritative check
//! in the commit, tab-group moves, screen moves, new screens in an app
//! workspace, layout undo, `tab.create_app`'s revision precondition, and
//! new tabs sent to a home whose focused pane is the app column.

use super::*;
use crate::resource_mutation::ResourceMutationPlan;

/// An app screen and a terminal pane with two tabs elsewhere.
fn store_and_terminals(wire: &mut Wire) -> (String, String, SurfaceId, PaneId, SurfaceId, PaneId) {
    let (_, terminal_pane) = wire.terminal_pane();
    let second =
        wire.ok(json!({"cmd": "new-tab", "pane": terminal_pane}))["surface"].as_u64().unwrap();
    let created = wire.ensure_app(STORE, "app", "open-store");
    let workspace = created["value"]["workspace_id"].as_str().unwrap().to_string();
    let screen = created["value"]["screen_id"].as_str().unwrap().to_string();
    let (app, app_pane) = app_tab(&wire.screen(&screen));
    (workspace, screen, app, app_pane, second, terminal_pane)
}

/// A racing op that skipped every pre-check (here a plan that moves a tab
/// into the app pane) is refused by the check in the commit transaction,
/// and neither the store nor the live state changes.
#[test]
fn commit_check_refuses_a_racing_op_that_skipped_the_pre_checks() {
    let mut wire = Wire::new();
    let (_, _, _, app_pane, moved, source) = store_and_terminals(&mut wire);
    let before = layout_fingerprint(&wire.tree());
    let mux = wire.mux.clone();
    let result = mux.commit_resource_mutation_plan(
        &WorkspaceMutation::local("app-screens-race"),
        "test.race",
        &json!({"race": true}),
        None,
        None,
        |state, registry| {
            let mut projected = state.clone();
            let source_pane = projected.panes.get_mut(&source).unwrap();
            source_pane.tabs.retain(|tab| *tab != moved);
            source_pane.active_tab = 0;
            projected.panes.get_mut(&app_pane).unwrap().tabs.push(moved);
            let projection =
                mux.resource_effect_projection_locked(registry, &mut projected, json!({}))?;
            Ok(ResourceMutationPlan::new(
                projection.patch,
                projection.result,
                projection.changes,
                move |state| *state = projected,
            ))
        },
    );
    let Err(error) = result else { panic!("the racing op committed into the app screen") };
    assert_eq!(
        crate::state::app_screens_store::raw_error_code(&error),
        Some("app-screen-fixed"),
        "{error:#}"
    );
    assert_eq!(layout_fingerprint(&wire.tree()), before, "the refused commit changed something");
    let stored = wire.mux.with_state(|state| state.panes[&app_pane].tabs.len());
    assert_eq!(stored, 1);
    wire.mux.shutdown();
}

/// Tab-group moves out of and into an app screen are refused.
#[test]
fn tab_group_moves_respect_the_app_screen() {
    let mut wire = Wire::new();
    let (_, _, app, app_pane, second, terminal_pane) = store_and_terminals(&mut wire);
    let before = layout_fingerprint(&wire.tree());
    let group = wire.ok(json!({"cmd": "create-tab-group", "surfaces": [app], "name": "App"}));
    let group = group["group"]["id"].as_str().unwrap().to_string();
    for request in [
        json!({"cmd": "move-tab-group", "group": group, "pane": terminal_pane, "index": 0}),
        json!({"cmd": "move-tab-group-to-split", "group": group, "pane": terminal_pane,
               "edge": "right"}),
        json!({"cmd": "move-tab-group-to-new-workspace", "group": group}),
    ] {
        wire.refused(request, "app-screen-fixed");
    }
    let other = wire.ok(json!({"cmd": "create-tab-group", "surfaces": [second], "name": "T"}));
    let other = other["group"]["id"].as_str().unwrap().to_string();
    wire.refused(
        json!({"cmd": "move-tab-group", "group": other, "pane": app_pane, "index": 0}),
        "app-screen-fixed",
    );
    let after = layout_fingerprint(&wire.tree());
    let tabs_of =
        |tree: &Value| screens(tree).iter().map(tabs).map(|t| t.len()).collect::<Vec<_>>();
    assert_eq!(tabs_of(&after), tabs_of(&before), "a refused group move moved a tab");
    wire.mux.shutdown();
}

/// An app workspace keeps exactly its one app screen: moving the app screen
/// out, a screen in, or creating a screen in it is refused.
#[test]
fn app_workspace_keeps_exactly_its_app_screen() {
    let mut wire = Wire::new();
    let (workspace, screen_id, _, app_pane, _, terminal_pane) = store_and_terminals(&mut wire);
    let app_workspace = wire.workspace_slot(&workspace);
    let (app_screen, terminal_workspace, terminal_screen) = wire.mux.with_state(|state| {
        let screen_of = |pane| {
            let (w, s) = state.screen_of(pane).unwrap();
            (state.workspaces[w].id, state.workspaces[w].screens[s].id)
        };
        let (_, app_screen) = screen_of(app_pane);
        let (terminal_workspace, terminal_screen) = screen_of(terminal_pane);
        (app_screen, terminal_workspace, terminal_screen)
    });
    // The app screen is its workspace's last screen: moving it out is
    // refused before the app rules are asked.
    for request in [
        json!({"cmd": "move-screen", "screen": app_screen, "workspace": terminal_workspace}),
        json!({"cmd": "move-screen", "screen": app_screen, "new_workspace": true}),
    ] {
        let response = wire.send(request.clone());
        assert_eq!(response["ok"], false, "{request}: {response}");
    }
    // A second screen, so the terminal workspace may give one away.
    wire.ok(json!({"cmd": "new-screen", "workspace": terminal_workspace}));
    for request in [
        json!({"cmd": "move-screen", "screen": terminal_screen, "workspace": app_workspace}),
        json!({"cmd": "new-screen", "workspace": app_workspace}),
    ] {
        wire.refused(request, "app-screen-fixed");
    }
    wire.v2_refused(
        "screen.create",
        json!({"workspace": workspace}),
        "screen-in-app-workspace",
        "app.screen_fixed",
    );
    let app_screens = screens(&wire.tree())
        .into_iter()
        .filter(|screen| screen["resource_id"] == screen_id.as_str())
        .count();
    assert_eq!(app_screens, 1);
    wire.mux.shutdown();
}

/// Layout undo never restores a shape that breaks A1/A2: undoing a new
/// column right of the app column is allowed, an undo entry whose layout
/// has another app column is refused, and an app screen has nothing to undo.
#[test]
fn layout_undo_keeps_the_app_column() {
    let mut wire = Wire::new();
    let created = wire.ensure_app(HOME, "appColumn", "open-home");
    let screen_id = created["value"]["screen_id"].as_str().unwrap().to_string();
    let (app, app_pane) = app_tab(&wire.screen(&screen_id));
    wire.ok(json!({"cmd": "new-pane-right", "pane": app_pane, "width": 0.5}));
    let preview = wire.ok(json!({"cmd": "undo-layout", "pane": app_pane}));
    assert_eq!(preview["confirmation_required"], true, "{preview}");
    wire.ok(json!({"cmd": "undo-layout", "pane": app_pane, "confirm_close": true,
                   "revision": preview["revision"]}));
    let raw = wire.screen(&screen_id);
    assert_eq!(raw["kind"], "appColumn", "{raw}");
    assert_eq!(app_tab(&raw).0, app, "undo kept the lone app column");

    let right = wire.ok(json!({"cmd": "new-pane-right", "pane": app_pane, "width": 0.5}));
    let ordinary = wire.pane_of(right["surface"].as_u64().unwrap());
    {
        let mut state = wire.mux.state.lock().unwrap();
        let (w, s) = state.screen_of(app_pane).unwrap();
        let entry = state.workspaces[w].screens[s].layout_undo.back_mut().unwrap();
        entry.before.layout_columns.clear();
        entry.before.root = Node::Leaf(ordinary);
    }
    let before = layout_fingerprint(&wire.tree());
    let preview = wire.ok(json!({"cmd": "undo-layout", "pane": app_pane}));
    wire.refused(
        json!({"cmd": "undo-layout", "pane": app_pane, "confirm_close": true,
               "revision": preview["revision"]}),
        "app-column-locked",
    );
    assert_eq!(layout_fingerprint(&wire.tree()), before);

    let store = wire.ensure_app(STORE, "app", "open-store");
    let (_, store_pane) = app_tab(&wire.screen(store["value"]["screen_id"].as_str().unwrap()));
    let before = layout_fingerprint(&wire.tree());
    let response = wire.send(json!({"cmd": "undo-layout", "pane": store_pane}));
    assert_eq!(response["ok"], false, "{response}");
    assert_eq!(layout_fingerprint(&wire.tree()), before);
    wire.mux.shutdown();
}

/// `tab.create_app` honors `expected_revision` like `tab.create_browser`.
#[test]
fn tab_create_app_honors_expected_revision() {
    let wire = Wire::new();
    let (_, pane) = wire.terminal_pane();
    let public_pane = wire.public_pane(pane);
    let ahead = wire.mux.with_state(|state| state.resource_revision) + 100;
    let stale = json!({"pane": public_pane, "app": STORE, "expected_revision": ahead.to_string()});
    wire.v2_refused("tab.create_app", stale, "create-stale", "revision.conflict");
    let revision = wire.mux.with_state(|state| state.resource_revision);
    let current =
        json!({"pane": public_pane, "app": STORE, "expected_revision": revision.to_string()});
    let created = wire.v2_ok("tab.create_app", current, Some("create-current"));
    assert_eq!(created["value"]["kind"], "app", "{created}");
    wire.mux.shutdown();
}

/// Decision 2026-10-04: a new tab sent to a home whose focused pane is the
/// app column goes to the first ordinary column, or into a new ordinary
/// column right of the app column when there is none. It is never refused.
#[test]
fn new_tabs_sent_to_the_home_land_in_an_ordinary_column() {
    let mut wire = Wire::new();
    let migrate = json!({"screen": "appColumn", "app": HOME});
    let home = wire.v2_ok("workspace.ensure_home", migrate, Some("connect"));
    let home_id = home["value"]["workspace_id"].as_str().unwrap().to_string();
    let home_slot = wire.workspace_slot(&home_id);
    let home_screen = |wire: &mut Wire| {
        let tree = wire.tree();
        let workspace = tree["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["resource_id"] == home_id.as_str())
            .cloned()
            .unwrap();
        workspace["screens"][0].clone()
    };
    let (app, app_pane) = app_tab(&home_screen(&mut wire));
    let mut created = Vec::new();
    for (index, conversation) in ["conv_01X", "conv_01Y"].into_iter().enumerate() {
        let tab = wire.ok(json!({"cmd": "new-conversation-tab", "workspace": home_slot,
                                "conversation": conversation, "owner": "local",
                                "origin": "route-test", "mutation_id": format!("r{index}")}));
        created.push(tab["surface"].as_u64().unwrap());
    }
    let browser = wire.v2_ok(
        "tab.create_browser",
        json!({"workspace": home_id, "url": "https://example.com"}),
        Some("route-browser"),
    );
    assert_eq!(browser["value"]["kind"], "browser", "{browser}");
    let screen = home_screen(&mut wire);
    let columns = screen["columns"].as_array().cloned().unwrap_or_default();
    assert_eq!(columns.len(), 2, "one new ordinary column right of the app column: {screen}");
    assert_eq!(columns[0]["app"], HOME);
    assert_eq!(layout_panes(&columns[0]["layout"]), vec![app_pane]);
    assert_eq!(wire.pane_of(app), app_pane);
    let ordinary = layout_panes(&columns[1]["layout"]);
    for surface in &created {
        assert!(ordinary.contains(&wire.pane_of(*surface)), "{surface} is not in column 1");
    }
    assert_eq!(tabs(&screen).len(), 4, "{screen}");
    wire.mux.shutdown();
}

/// The app workspace of `app`, created as the first step of
/// `workspace.ensure_app` (as if the daemon stopped right after it).
fn interrupted_app_workspace(wire: &Wire, app: &str, key: &str) -> (String, WorkspaceId) {
    wire.mux
        .resource_create_empty_workspace_selected(
            Mux::ordinary_resource_selectors(),
            Some(app.to_string()),
            key,
            None,
            &WorkspaceMutation::local("app-crash"),
            crate::state::home_store::EmptyWorkspaceMark::App(app.to_string()),
        )
        .unwrap();
    wire.mux.with_state(|state| {
        let workspace = state.workspaces.last().unwrap();
        (workspace.public_id.to_string(), workspace.id)
    })
}

fn app_record(app: &str) -> crate::state::app_screens_store::AppTabRecord {
    crate::state::app_screens_store::AppTabRecord { app: app.to_string(), route: None }
}

/// `workspace.ensure_app` resumes a creation interrupted after any of its
/// commits, and replaces a workspace whose screen changed between them.
#[test]
fn ensure_app_resumes_or_replaces_an_interrupted_creation() {
    use crate::state::app_screens::AppTabTarget;
    let mut wire = Wire::new();
    let _ = wire.terminal_pane();

    // Stopped after the workspace commit.
    let (first, _) = interrupted_app_workspace(&wire, STORE, "crash-1");
    let resumed = wire.ensure_app(STORE, "app", "resume-1");
    assert_eq!(resumed["value"]["workspace_id"], first.as_str(), "{resumed}");
    let raw = wire.screen(resumed["value"]["screen_id"].as_str().unwrap());
    assert_eq!(raw["kind"], "app", "{raw}");

    // Stopped after the app tab commit.
    let (second, slot) = interrupted_app_workspace(&wire, HOME, "crash-2");
    let tab = wire.mux.new_app_tab(AppTabTarget::Workspace(slot), app_record(HOME), None, None);
    let surface = tab.unwrap().surface.id;
    let screen = wire.mux.with_state(|state| {
        let (w, s) = state.screen_of(state.pane_of(surface).unwrap()).unwrap();
        state.workspaces[w].screens[s].public_id.to_string()
    });
    let resumed = wire.ensure_app(HOME, "appColumn", "resume-2");
    assert_eq!(resumed["value"]["workspace_id"], second.as_str(), "{resumed}");
    assert_eq!(resumed["value"]["screen_id"], screen.as_str(), "{resumed}");
    assert_eq!(wire.screen(&screen)["kind"], "appColumn");

    // The screen changed between the commits: a second tab joined the app
    // pane. The old workspace stays ordinary with both tabs; the app gets a
    // new workspace.
    let other = "cmux/other";
    let (third, slot) = interrupted_app_workspace(&wire, other, "crash-3");
    let tab = wire.mux.new_app_tab(AppTabTarget::Workspace(slot), app_record(other), None, None);
    let app_pane = wire.pane_of(tab.unwrap().surface.id);
    wire.ok(json!({"cmd": "new-tab", "pane": app_pane}));
    let replaced = wire.ensure_app(other, "app", "resume-3");
    assert_ne!(replaced["value"]["workspace_id"], third.as_str(), "{replaced}");
    let raw = wire.screen(replaced["value"]["screen_id"].as_str().unwrap());
    assert_eq!(raw["kind"], "app", "{raw}");
    let _ = app_tab(&raw);
    let tree = wire.tree();
    let old = tree["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["resource_id"] == third.as_str())
        .cloned()
        .expect("the old workspace stays");
    assert_eq!(old["kind"], "normal", "{old}");
    assert_eq!(tabs(&old["screens"][0]).len(), 2, "{old}");
    wire.mux.shutdown();
}

/// The Home migration resumes after a stop between its app tab commit and
/// its kind commit, reusing that app tab.
#[test]
fn home_migration_resumes_after_an_interrupted_step() {
    use crate::state::app_screens::AppTabTarget;
    let mut wire = Wire::new();
    let home = wire.v2_ok("workspace.ensure_home", json!({}), Some("connect-1"));
    let home_id = home["value"]["workspace_id"].as_str().unwrap().to_string();
    let home_slot = wire.workspace_slot(&home_id);
    let conversation = wire.ok(json!({"cmd": "new-conversation-tab", "workspace": home_slot,
                                     "conversation": "conv_01R", "owner": "local",
                                     "origin": "resume-test", "mutation_id": "c"}));
    let pane = wire.pane_of(conversation["surface"].as_u64().unwrap());
    let tab = wire.mux.new_app_tab(AppTabTarget::Pane(Some(pane)), app_record(HOME), None, None);
    let app = tab.unwrap().surface.id;
    let migrate = json!({"screen": "appColumn", "app": HOME});
    wire.v2_ok("workspace.ensure_home", migrate, Some("connect-2"));
    let screen = wire.mux.with_state(|state| {
        let (w, s) = state.screen_of(state.pane_of(app).unwrap()).unwrap();
        state.workspaces[w].screens[s].public_id.to_string()
    });
    let raw = wire.screen(&screen);
    assert_eq!(raw["kind"], "appColumn", "{raw}");
    assert_eq!(tabs(&raw).iter().filter(|tab| tab["kind"] == "app").count(), 1, "{raw}");
    assert_eq!(tabs(&raw).len(), 2, "{raw}");
    wire.assert_stored_app_column(&screen, 2);
    wire.mux.shutdown();
}

/// A new tab sent to the home screen (a screen target, not a pane) whose
/// focused pane is the lone app column gets a new ordinary column.
#[test]
fn new_tab_sent_to_the_home_screen_lands_in_an_ordinary_column() {
    let mut wire = Wire::new();
    let migrate = json!({"screen": "appColumn", "app": HOME});
    let home = wire.v2_ok("workspace.ensure_home", migrate, Some("connect"));
    let home_id = home["value"]["workspace_id"].as_str().unwrap().to_string();
    let screen = wire.mux.with_state(|state| {
        let workspace = state.workspaces.iter().find(|w| w.public_id.as_str() == home_id);
        workspace.unwrap().screens[0].public_id.to_string()
    });
    wire.v2_ok(
        "tab.create_browser",
        json!({"workspace": home_id, "screen": screen, "url": "https://example.com"}),
        Some("screen-target"),
    );
    let raw = wire.screen(&screen);
    assert_eq!(raw["columns"].as_array().map(Vec::len), Some(2), "{raw}");
    wire.assert_stored_app_column(&screen, 2);
    wire.mux.shutdown();
}

/// v2 `pane.swap` and `workspace.layout.apply` refuse the app column.
#[test]
fn v2_swap_and_layout_apply_refuse_the_app_column() {
    let mut wire = Wire::new();
    let created = wire.ensure_app(HOME, "appColumn", "open-home");
    let screen = created["value"]["screen_id"].as_str().unwrap().to_string();
    let workspace = created["value"]["workspace_id"].as_str().unwrap().to_string();
    let (_, app_pane) = app_tab(&wire.screen(&screen));
    let right = wire.ok(json!({"cmd": "new-pane-right", "pane": app_pane, "width": 0.5}));
    let ordinary = wire.pane_of(right["surface"].as_u64().unwrap());
    let before = layout_fingerprint(&wire.tree());
    let swap = json!({"workspace": workspace, "screen": screen,
                      "pane": wire.public_pane(app_pane), "other_workspace": workspace,
                      "other_screen": screen, "other_pane": wire.public_pane(ordinary)});
    wire.v2_refused("pane.swap", swap, "swap-app-column", "app.column_locked");
    let layout = wire.v2_ok(
        "screen.layout.export",
        json!({"workspace": workspace,
                                                           "screen": screen}),
        None,
    );
    let apply = json!({"workspace": workspace, "layout": layout});
    wire.v2_refused("workspace.layout.apply", apply, "apply-app-column", "app.column_locked");
    assert_eq!(layout_fingerprint(&wire.tree()), before);
    wire.mux.shutdown();
}

/// A refused op leaves the store as it was: after a restart the app screen
/// still has its one app tab.
#[test]
fn a_refused_op_survives_a_restart_unchanged() {
    let store = Store::new("refused-restart");
    let mut wire = store.open();
    let created = wire.ensure_app(STORE, "app", "open-store");
    let screen = created["value"]["screen_id"].as_str().unwrap().to_string();
    let (app, pane) = app_tab(&wire.screen(&screen));
    wire.refused(json!({"cmd": "close-surface", "surface": app}), "app-screen-fixed");
    wire.refused(json!({"cmd": "new-tab", "pane": pane}), "app-screen-fixed");
    let before = wire.public_tab(app);
    wire.mux.shutdown();
    drop(wire);
    let mut wire = store.open();
    let raw = wire.screen(&screen);
    assert_eq!(raw["kind"], "app", "{raw}");
    let (app, _) = app_tab(&raw);
    assert_eq!(wire.public_tab(app), before);
    wire.mux.shutdown();
}
