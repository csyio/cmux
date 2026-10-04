//! `app-screens-v1` rules on the remaining paths: the authoritative check
//! in the commit, tab-group moves, screen moves, new screens in an app
//! workspace, layout undo, `tab.create_app`'s revision precondition, new
//! tabs sent to an app workspace (its companion workspace), interrupted
//! creations, restart after a refusal, and an ordinary workspace whose only
//! tab is an app tab.

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
    let resumed = wire.ensure_app(HOME, "app", "resume-2");
    assert_eq!(resumed["value"]["workspace_id"], second.as_str(), "{resumed}");
    assert_eq!(resumed["value"]["screen_id"], screen.as_str(), "{resumed}");
    assert_eq!(wire.screen(&screen)["kind"], "app");

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

/// Layout undo has nothing to restore on an app screen, and changes
/// nothing there.
#[test]
fn layout_undo_leaves_the_app_screen_as_it_is() {
    let mut wire = Wire::new();
    let store = wire.ensure_app(STORE, "app", "open-store");
    let (_, pane) = app_tab(&wire.screen(store["value"]["screen_id"].as_str().unwrap()));
    let before = layout_fingerprint(&wire.tree());
    let response = wire.send(json!({"cmd": "undo-layout", "pane": pane}));
    assert_eq!(response["ok"], false, "{response}");
    assert_eq!(layout_fingerprint(&wire.tree()), before);
    wire.mux.shutdown();
}

/// The companion workspace of `app_workspace` (public id), from the store.
fn companion_of(wire: &Wire, app_workspace: &str) -> Option<String> {
    wire.mux
        .read_registry_state(|connection| {
            crate::state::app_screens_store::live_companion(connection, app_workspace)
        })
        .unwrap()
}

/// A new tab sent to an app workspace (workspace or screen target, not a
/// pane) is not refused: it goes to the app workspace's companion ordinary
/// workspace, created directly after it once. A pane inside the app screen
/// stays refused.
#[test]
fn new_tabs_sent_to_an_app_workspace_go_to_its_companion() {
    let mut wire = Wire::new();
    let home = wire.v2_ok("workspace.ensure_home", json!({"app": HOME}), Some("connect"));
    let home_id = home["value"]["workspace_id"].as_str().unwrap().to_string();
    let home_slot = wire.workspace_slot(&home_id);
    let screen = wire.mux.with_state(|state| {
        let workspace = state.workspace_by_id(home_slot).unwrap();
        workspace.screens[0].public_id.to_string()
    });
    let (app, app_pane) = app_tab(&wire.screen(&screen));
    assert!(companion_of(&wire, &home_id).is_none());
    let conversation = wire.ok(json!({"cmd": "new-conversation-tab", "workspace": home_slot,
                                     "conversation": "conv_01X", "owner": "local",
                                     "origin": "route-test", "mutation_id": "r0"}));
    let companion = companion_of(&wire, &home_id).expect("the companion was created");
    let workspace_of = |wire: &Wire, surface: SurfaceId| {
        wire.mux.with_state(|state| {
            let (w, _) = state.screen_of(state.pane_of(surface).unwrap()).unwrap();
            state.workspaces[w].public_id.to_string()
        })
    };
    assert_eq!(workspace_of(&wire, conversation["surface"].as_u64().unwrap()), companion);
    let browser = wire.v2_ok(
        "tab.create_browser",
        json!({"workspace": home_id, "screen": screen, "url": "https://example.com"}),
        Some("screen-target"),
    );
    assert_eq!(browser["value"]["workspace_id"], companion.as_str(), "{browser}");
    let app_tab_created =
        wire.ok(json!({"cmd": "new-app-tab", "workspace": home_slot, "app": STORE}));
    assert_eq!(workspace_of(&wire, app_tab_created["surface"].as_u64().unwrap()), companion);
    assert_eq!(companion_of(&wire, &home_id), Some(companion.clone()), "one companion");
    let order = wire.v2_ok("workspace.placement.list", json!({}), None);
    assert_eq!(order[1]["workspace"]["workspace_id"], companion.as_str(), "{order}");
    // The app screen is unchanged; a pane target inside it is refused.
    assert_eq!(app_tab(&wire.screen(&screen)), (app, app_pane));
    wire.refused(json!({"cmd": "new-tab", "pane": app_pane}), "app-screen-fixed");
    wire.v2_refused(
        "tab.create_terminal",
        json!({"workspace": home_id, "screen": screen, "pane": wire.public_pane(app_pane)}),
        "pane-target",
        "app.screen_fixed",
    );
    wire.mux.shutdown();
}

/// The Home migration resumes after a stop between its steps: after the
/// screens moved into the companion, and after the app tab commit.
#[test]
fn home_migration_resumes_after_an_interrupted_step() {
    use crate::state::app_screens::AppTabTarget;
    let mut wire = Wire::new();
    let home = wire.v2_ok("workspace.ensure_home", json!({}), Some("connect-1"));
    let home_id = home["value"]["workspace_id"].as_str().unwrap().to_string();
    let home_slot = wire.workspace_slot(&home_id);
    // Stopped after the app tab commit: the home is one screen with only
    // the Home app tab, without its kind row.
    let tab =
        wire.mux.new_app_tab(AppTabTarget::Workspace(home_slot), app_record(HOME), None, None);
    let app = tab.unwrap().surface.id;
    wire.v2_ok("workspace.ensure_home", json!({"app": HOME}), Some("connect-2"));
    let screen = wire.mux.with_state(|state| {
        let (w, s) = state.screen_of(state.pane_of(app).unwrap()).unwrap();
        state.workspaces[w].screens[s].public_id.to_string()
    });
    let raw = wire.screen(&screen);
    assert_eq!(raw["kind"], "app", "{raw}");
    assert_eq!(app_tab(&raw).0, app, "the interrupted app tab is reused");
    assert!(companion_of(&wire, &home_id).is_none(), "nothing needed a companion");
    wire.mux.shutdown();
}

/// v2 `pane.swap` and `workspace.layout.apply` refuse the app screen.
#[test]
fn v2_swap_and_layout_apply_refuse_the_app_screen() {
    let mut wire = Wire::new();
    let (_, terminal_pane) = wire.terminal_pane();
    let created = wire.ensure_app(STORE, "app", "open-store");
    let screen = created["value"]["screen_id"].as_str().unwrap().to_string();
    let workspace = created["value"]["workspace_id"].as_str().unwrap().to_string();
    let (_, app_pane) = app_tab(&wire.screen(&screen));
    let before = layout_fingerprint(&wire.tree());
    let other = wire.destination(terminal_pane);
    let swap = json!({"workspace": workspace, "screen": screen,
                      "pane": wire.public_pane(app_pane),
                      "other_workspace": other["destination_workspace"],
                      "other_screen": other["destination_screen"],
                      "other_pane": other["destination_pane"]});
    wire.v2_refused("pane.swap", swap, "swap-app", "app.screen_fixed");
    let layout =
        wire.v2_ok("screen.layout.export", json!({"workspace": workspace, "screen": screen}), None);
    let apply = json!({"workspace": workspace, "layout": layout});
    wire.v2_refused("workspace.layout.apply", apply, "apply-app", "app.screen_fixed");
    assert_eq!(layout_fingerprint(&wire.tree()), before);
    wire.mux.shutdown();
}

/// R91: an ordinary workspace whose only tab is an app tab (a first-party
/// page with its client state in `route`, up to 4 KiB) is valid, and the
/// tab and its route survive a restart.
#[test]
fn ordinary_workspace_with_only_an_app_tab_survives_a_restart() {
    let store = Store::new("only-app-tab");
    let wire = store.open();
    let created = wire.v2_ok(
        "workspace.create",
        json!({"name": "New Tab", "initial_content": "empty"}),
        Some("new-tab-workspace"),
    );
    let workspace = created["value"]["workspace_id"].as_str().unwrap().to_string();
    let route = "s".repeat(4096);
    let tab = wire.v2_ok(
        "tab.create_app",
        json!({"workspace": workspace, "app": "cmux.agent", "route": route}),
        Some("agent-page"),
    );
    let tab_id = tab["value"]["tab_id"].as_str().unwrap().to_string();
    let too_long = json!({"workspace": workspace, "app": "cmux.agent", "route": "s".repeat(4097)});
    let refused = wire.v2("tab.create_app", too_long, Some("agent-page-long"));
    assert_eq!(refused["ok"], false, "{refused}");
    wire.mux.shutdown();
    drop(wire);

    let mut wire = store.open();
    let tree = wire.tree();
    let workspace = tree["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["resource_id"] == workspace.as_str())
        .cloned()
        .expect("the workspace with only an app tab is kept");
    assert_eq!(workspace["kind"], "normal", "{workspace}");
    let tabs = tabs(&workspace["screens"][0]);
    assert_eq!(tabs.len(), 1, "{workspace}");
    assert_eq!(tabs[0]["tab_resource_id"], tab_id.as_str());
    assert_eq!(tabs[0]["kind"], "app");
    assert_eq!(tabs[0]["app"], "cmux.agent");
    assert_eq!(tabs[0]["route"].as_str().map(str::len), Some(4096));
    wire.mux.shutdown();
}
