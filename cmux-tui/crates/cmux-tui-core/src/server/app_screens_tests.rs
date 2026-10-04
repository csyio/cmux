//! `app-screens-v1` (plans/cmux-next/app-screens.md): screen kinds `app` and
//! `appColumn`, the `app` tab, `workspace.ensure_app`, the refusals
//! `app-screen-fixed` and `app-column-locked` on every command shape, the
//! read shape, restart persistence and the Home migration.

use std::path::PathBuf;

use super::*;
use crate::workspace_registry::WorkspaceRegistry;

#[path = "app_screens_rules_tests.rs"]
mod rules;

const STORE: &str = "cmux/app-store";
const HOME: &str = "cmux/home";

/// One connection to a mux: raw JSON-lines requests and `cmux.protocol/2`.
struct Wire {
    mux: Arc<Mux>,
    outbound: Arc<BoundedOutbound>,
    writer: MessageWriter,
    client: u64,
    next_id: u64,
}

impl Wire {
    /// A registered connection that declares `app-screens-v1`.
    fn on(mux: Arc<Mux>) -> Self {
        let outbound = Arc::new(BoundedOutbound::default());
        let writer = MessageWriter::new(QueuedSink { outbound: outbound.clone(), control: None });
        let client = mux.control_clients.register(ClientTransport::Unix, writer.clone());
        let mut wire = Self { mux, outbound, writer, client, next_id: 1 };
        wire.ok(json!({"cmd": "set-client-info", "capabilities": ["app-screens-v1"]}));
        wire
    }

    fn new() -> Self {
        Self::on(Mux::new_for_test("app-screens", crate::SurfaceOptions::default()))
    }

    fn send(&mut self, mut request: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        request["id"] = json!(id);
        handle_message(&self.mux, self.client, &request.to_string(), &self.writer);
        let response: Value = serde_json::from_str(&self.outbound.try_pop().unwrap()).unwrap();
        assert_eq!(response["id"], id, "response must answer the request: {response}");
        response
    }

    fn ok(&mut self, request: Value) -> Value {
        let response = self.send(request.clone());
        assert_eq!(response["ok"], true, "{request} failed: {response}");
        response["data"].clone()
    }

    /// A raw request that must fail with `code`.
    fn refused(&mut self, request: Value, code: &str) {
        let response = self.send(request.clone());
        assert_eq!(response["ok"], false, "{request} succeeded: {response}");
        assert_eq!(response["error_code"], code, "{request}: {response}");
    }

    fn v2(&self, operation: &str, params: Value, key: Option<&str>) -> Value {
        let mut params = params;
        params["machine"] = json!("current");
        params["session"] = json!("current");
        let mut envelope = json!({
            "protocol": "cmux.protocol/2",
            "type": "request",
            "id": format!("{operation}-test"),
            "operation": operation,
            "params": params,
        });
        if let Some(key) = key {
            envelope["idempotency_key"] = json!(key);
        }
        // A request the catalog refuses comes back as an error, not a response.
        match crate::resource_router::handle_resource_message(&self.mux, &envelope.to_string()) {
            Ok(response) => response,
            Err(error) => {
                json!({"ok": false, "error": {"code": error.code, "details": error.details}})
            }
        }
    }

    fn v2_ok(&self, operation: &str, params: Value, key: Option<&str>) -> Value {
        let response = self.v2(operation, params.clone(), key);
        assert_eq!(response["ok"], true, "{operation} {params} failed: {response}");
        response["result"].clone()
    }

    fn v2_refused(&self, operation: &str, params: Value, key: &str, code: &str) {
        let response = self.v2(operation, params.clone(), Some(key));
        assert_eq!(response["ok"], false, "{operation} {params} succeeded: {response}");
        assert_eq!(response["error"]["code"], code, "{operation} {params}: {response}");
    }

    fn ensure_app(&self, app: &str, kind: &str, key: &str) -> Value {
        self.v2_ok("workspace.ensure_app", json!({"app": app, "kind": kind}), Some(key))
    }

    fn tree(&mut self) -> Value {
        self.ok(json!({"cmd": "list-workspaces"}))
    }

    /// The raw screen whose `resource_id` is `id`.
    fn screen(&mut self, id: &str) -> Value {
        screens(&self.tree()).into_iter().find(|screen| screen["resource_id"] == id).unwrap()
    }

    fn snapshot(&self) -> Value {
        crate::resource_api::public_session_snapshot(&self.mux).unwrap()
    }

    /// A pane holding one terminal, in a new ordinary workspace.
    fn terminal_pane(&self) -> (SurfaceId, PaneId) {
        let surface = self.mux.new_workspace(None, Some((80, 22))).unwrap().id;
        (surface, self.mux.with_state(|state| state.pane_of(surface).unwrap()))
    }

    fn pane_of(&self, surface: SurfaceId) -> PaneId {
        self.mux.with_state(|state| state.pane_of(surface).unwrap())
    }

    fn public_pane(&self, pane: PaneId) -> String {
        self.mux.with_state(|state| state.resource_indexes.pane_ids[&pane].to_string())
    }

    /// The `destination_*` fields of a v2 `tab.move` into `pane`.
    fn destination(&self, pane: PaneId) -> Value {
        self.mux.with_state(|state| {
            let (workspace, screen) = state.screen_of(pane).unwrap();
            let workspace = &state.workspaces[workspace];
            json!({
                "destination_workspace": workspace.public_id.as_str(),
                "destination_screen": workspace.screens[screen].public_id.as_str(),
                "destination_pane": state.resource_indexes.pane_ids[&pane].as_str(),
            })
        })
    }

    /// A v2 `tab.move` of `tab` to the end of `pane`.
    fn tab_move(&self, tab: SurfaceId, pane: PaneId) -> Value {
        let mut params = self.destination(pane);
        params["tab"] = json!(self.public_tab(tab));
        params["index"] = json!(0);
        params
    }

    fn public_tab(&self, surface: SurfaceId) -> String {
        self.mux.with_state(|state| state.resource_indexes.tab_ids[&surface].to_string())
    }

    fn workspace_slot(&self, id: &str) -> WorkspaceId {
        self.mux.with_state(|state| {
            state.workspaces.iter().find(|item| item.public_id.as_str() == id).unwrap().id
        })
    }
}

/// Every pane of a raw layout node.
fn layout_panes(node: &Value) -> Vec<PaneId> {
    match node["type"].as_str() {
        Some("leaf") => node["pane"].as_u64().into_iter().collect(),
        Some("split") => [layout_panes(&node["a"]), layout_panes(&node["b"])].concat(),
        _ => node["panes"].as_array().into_iter().flatten().filter_map(Value::as_u64).collect(),
    }
}

fn screens(tree: &Value) -> Vec<Value> {
    tree["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|workspace| workspace["screens"].as_array().unwrap().clone())
        .collect()
}

fn tabs(screen: &Value) -> Vec<Value> {
    screen["panes"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|pane| pane["tabs"].as_array().unwrap().clone())
        .collect()
}

/// The only tab of an app screen: its surface and pane.
fn app_tab(screen: &Value) -> (SurfaceId, PaneId) {
    let panes = screen["panes"].as_array().unwrap();
    assert_eq!(panes.len(), 1, "an app screen has one pane: {screen}");
    let tabs = panes[0]["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 1, "an app screen has one tab: {screen}");
    (tabs[0]["surface"].as_u64().unwrap(), panes[0]["id"].as_u64().unwrap())
}

/// Everything a refused op must leave unchanged: the tree without focus.
fn layout_fingerprint(tree: &Value) -> Value {
    let mut tree = tree.clone();
    strip_focus(&mut tree);
    tree
}

fn strip_focus(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for key in ["active", "active_pane", "active_tab", "focused", "active_at", "focused_at"]
            {
                object.remove(key);
            }
            object.values_mut().for_each(strip_focus);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_focus),
        _ => {}
    }
}

struct Store {
    root: PathBuf,
    name: &'static str,
}

impl Store {
    fn new(name: &'static str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "cmux-app-screens-{name}-{}",
            crate::resource::WorkspacePublicId::random().unwrap()
        ));
        Self { root, name }
    }

    fn open(&self) -> Wire {
        let registry = WorkspaceRegistry::open(&self.root, self.name).unwrap();
        let mux = Mux::from_workspace_registry(
            self.name.into(),
            crate::SurfaceOptions::default(),
            registry,
            crate::mux::ProviderWorkspaceState::default(),
            true,
        )
        .unwrap();
        Wire::on(mux)
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn app_screens_capability_is_advertised() {
    let mut wire = Wire::new();
    let identity = wire.ok(json!({"cmd": "identify"}));
    let capabilities = identity["capabilities"].as_array().unwrap();
    assert!(capabilities.contains(&json!("app-screens-v1")), "{identity}");
    wire.mux.shutdown();
}

/// One workspace of kind `app` per app, holding one screen of the asked kind
/// with one `app` tab; later calls with any key name the same ids.
#[test]
fn ensure_app_creates_one_app_workspace_per_app_and_replays() {
    let mut wire = Wire::new();
    let _ = wire.terminal_pane();
    let created = wire.ensure_app(STORE, "app", "open-store-1");
    assert_eq!(created["replayed"], false, "{created}");
    let workspace = created["value"]["workspace_id"].as_str().unwrap().to_string();
    let screen = created["value"]["screen_id"].as_str().unwrap().to_string();

    let again = wire.ensure_app(STORE, "app", "open-store-2");
    assert_eq!(again["replayed"], true, "{again}");
    assert_eq!(again["value"]["workspace_id"], workspace.as_str());
    assert_eq!(again["value"]["screen_id"], screen.as_str());

    // Raw read shape: screen kind and app, and the flat `app` tab.
    let raw = wire.screen(&screen);
    assert_eq!(raw["kind"], "app", "{raw}");
    assert_eq!(raw["app"], STORE);
    assert!(raw.get("columns").is_none(), "an app screen has no columns: {raw}");
    let tab = &tabs(&raw)[0];
    assert_eq!(tab["kind"], "app", "{tab}");
    assert_eq!(tab["app"], STORE);
    assert!(tab.get("route").is_none() || tab["route"].is_null(), "{tab}");
    let workspaces = wire.tree()["workspaces"].as_array().unwrap().clone();
    let raw_workspace = workspaces.iter().find(|item| item["resource_id"] == workspace).unwrap();
    assert_eq!(raw_workspace["kind"], "app", "{raw_workspace}");
    assert_eq!(raw_workspace["screens"].as_array().unwrap().len(), 1);

    // Ordinary screens carry no kind.
    let ordinary = screens(&wire.tree())
        .into_iter()
        .find(|item| item["resource_id"] != screen.as_str())
        .unwrap();
    assert!(ordinary.get("kind").is_none() && ordinary.get("app").is_none(), "{ordinary}");

    // Resource API: workspace and screen kinds, and the app tab.
    let snapshot = wire.snapshot();
    let v2_workspace =
        snapshot["workspaces"].as_array().unwrap().iter().find(|w| w["id"] == workspace).unwrap();
    assert_eq!(v2_workspace["extra"]["kind"], "app", "{v2_workspace}");
    assert_eq!(v2_workspace["extra"]["app"], STORE);
    let v2_screen =
        snapshot["screens"].as_array().unwrap().iter().find(|s| s["id"] == screen).unwrap();
    assert_eq!(v2_screen["extra"]["kind"], "app", "{v2_screen}");
    assert_eq!(v2_screen["extra"]["app"], STORE);
    let app_surface = app_tab(&wire.screen(&screen)).0;
    let app_tab_id = wire.public_tab(app_surface);
    let v2_tab =
        snapshot["tabs"].as_array().unwrap().iter().find(|tab| tab["id"] == app_tab_id).unwrap();
    assert_eq!(v2_tab["content_kind"], "app", "{v2_tab}");
    assert_eq!(v2_tab["extra"]["app"], STORE);

    // A second app gets its own workspace; an appColumn screen is valid with
    // only the app column.
    let home = wire.ensure_app(HOME, "appColumn", "open-home");
    assert_ne!(home["value"]["workspace_id"], workspace.as_str());
    let raw = wire.screen(home["value"]["screen_id"].as_str().unwrap());
    assert_eq!((raw["kind"].as_str(), raw["app"].as_str()), (Some("appColumn"), Some(HOME)));
    let _ = app_tab(&raw);

    // The same app with the other kind is refused; unknown kinds and bad ids too.
    let conflict =
        wire.v2("workspace.ensure_app", json!({"app": STORE, "kind": "appColumn"}), Some("k1"));
    assert_eq!(conflict["ok"], false, "{conflict}");
    for params in [
        json!({"app": STORE, "kind": "workspace"}),
        json!({"app": "", "kind": "app"}),
        json!({"app": "has space", "kind": "app"}),
        json!({"kind": "app"}),
    ] {
        let key = format!("k2-{}", params["app"]);
        let refused = wire.v2("workspace.ensure_app", params.clone(), Some(&key));
        assert_eq!(refused["error"]["code"], "validation.invalid", "{params}: {refused}");
    }
    wire.mux.shutdown();
}

#[test]
fn ensure_app_persists_across_restart() {
    let store = Store::new("restart");
    let wire = store.open();
    let created = wire.ensure_app(STORE, "app", "open-store");
    let workspace = created["value"]["workspace_id"].as_str().unwrap().to_string();
    let screen = created["value"]["screen_id"].as_str().unwrap().to_string();
    wire.mux.shutdown();
    drop(wire);

    let mut wire = store.open();
    let raw = wire.screen(&screen);
    assert_eq!((raw["kind"].as_str(), raw["app"].as_str()), (Some("app"), Some(STORE)), "{raw}");
    let (surface, pane) = app_tab(&raw);
    assert_eq!(tabs(&raw)[0]["kind"], "app");
    let replay = wire.ensure_app(STORE, "app", "open-store-after-restart");
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["value"]["workspace_id"], workspace.as_str());
    assert_eq!(replay["value"]["screen_id"], screen.as_str());
    // The rules hold after the restart.
    wire.refused(json!({"cmd": "close-surface", "surface": surface}), "app-screen-fixed");
    wire.refused(json!({"cmd": "split", "pane": pane, "dir": "right"}), "app-screen-fixed");
    wire.mux.shutdown();
}

/// Every command shape that would add, split, move, pin or close inside an
/// `app` screen is refused with `app-screen-fixed` and changes nothing;
/// closing the screen is allowed.
#[test]
fn app_screen_refuses_every_shape_and_changes_nothing() {
    let mut wire = Wire::new();
    let (terminal, terminal_pane) = wire.terminal_pane();
    let created = wire.ensure_app(STORE, "app", "open-store");
    let screen_id = created["value"]["screen_id"].as_str().unwrap().to_string();
    let workspace_id = created["value"]["workspace_id"].as_str().unwrap().to_string();
    let (app, pane) = app_tab(&wire.screen(&screen_id));
    let app_workspace = wire.workspace_slot(&workspace_id);
    let before = layout_fingerprint(&wire.tree());

    let code = "app-screen-fixed";
    for request in [
        json!({"cmd": "new-tab", "pane": pane}),
        json!({"cmd": "new-browser-tab", "pane": pane, "url": "https://example.com"}),
        json!({"cmd": "new-frontend-browser-tab", "pane": pane, "url": "https://example.com",
               "engine": "webkit"}),
        json!({"cmd": "split", "pane": pane, "dir": "right"}),
        json!({"cmd": "split", "pane": pane, "dir": "down"}),
        json!({"cmd": "new-pane", "pane": pane}),
        json!({"cmd": "new-pane-right", "pane": pane, "width": 0.5}),
        json!({"cmd": "new-row", "pane": pane, "height_permille": 500}),
        json!({"cmd": "move-tab", "surface": terminal, "pane": pane, "index": 0}),
        json!({"cmd": "move-tab", "surface": app, "pane": terminal_pane, "index": 0}),
        json!({"cmd": "move-tab-to-split", "surface": terminal, "pane": pane, "edge": "right"}),
        json!({"cmd": "move-tab-to-split", "surface": app, "pane": terminal_pane, "edge": "right"}),
        json!({"cmd": "move-tab-to-column", "surface": terminal, "pane": pane}),
        json!({"cmd": "move-tab-to-column", "surface": app, "pane": terminal_pane}),
        json!({"cmd": "move-tab-to-workspace", "surface": terminal, "workspace": app_workspace}),
        json!({"cmd": "move-tab-to-new-workspace", "surface": app}),
        json!({"cmd": "set-column-sticky", "pane": pane, "sticky": true}),
        json!({"cmd": "swap-pane", "pane": pane, "target": terminal_pane}),
        json!({"cmd": "close-surface", "surface": app}),
        json!({"cmd": "close-tabs", "surfaces": [app]}),
        json!({"cmd": "close-pane", "pane": pane}),
        json!({"cmd": "apply-layout", "workspace": app_workspace,
               "layout": {"type": "leaf"}}),
    ] {
        wire.refused(request, code);
    }
    let (public_pane, public_app) = (wire.public_pane(pane), wire.public_tab(app));
    let code = "app.screen_fixed";
    for (index, (operation, params)) in [
        (
            "tab.create_terminal",
            json!({"workspace": workspace_id, "screen": screen_id,
                                       "pane": public_pane}),
        ),
        (
            "tab.create_browser",
            json!({"workspace": workspace_id, "screen": screen_id,
                                      "pane": public_pane, "url": "https://example.com"}),
        ),
        (
            "pane.split",
            json!({"workspace": workspace_id, "screen": screen_id,
                              "pane": public_pane, "direction": "right"}),
        ),
        ("pane.create", json!({"workspace": workspace_id, "screen": screen_id})),
        (
            "pane.close",
            json!({"workspace": workspace_id, "screen": screen_id,
                              "pane": public_pane}),
        ),
        (
            "tab.close",
            json!({"workspace": workspace_id, "screen": screen_id,
                             "pane": public_pane, "tab": public_app}),
        ),
        ("tab.move", wire.tab_move(terminal, pane)),
        ("tab.move", wire.tab_move(app, terminal_pane)),
    ]
    .into_iter()
    .enumerate()
    {
        wire.v2_refused(operation, params, &format!("refuse-{index}-{operation}"), code);
    }
    assert_eq!(layout_fingerprint(&wire.tree()), before, "a refused op changed the layout");

    // Closing the screen (and so the app workspace) is allowed.
    let screen = wire
        .mux
        .with_state(|state| state.screen_of(pane).map(|(w, s)| state.workspaces[w].screens[s].id))
        .unwrap();
    wire.ok(json!({"cmd": "close-screen", "screen": screen}));
    assert!(screens(&wire.tree()).iter().all(|item| item["resource_id"] != screen_id.as_str()));
    wire.mux.shutdown();
}

/// On an `appColumn` screen the app column is locked (`app-column-locked`)
/// and keeps only its width; ordinary columns right of it behave as usual,
/// and adding a column right of the app column is allowed.
#[test]
fn app_column_is_locked_and_ordinary_columns_are_free() {
    let mut wire = Wire::new();
    let (terminal, terminal_pane) = wire.terminal_pane();
    let created = wire.ensure_app(HOME, "appColumn", "open-home");
    let screen_id = created["value"]["screen_id"].as_str().unwrap().to_string();
    let workspace_id = created["value"]["workspace_id"].as_str().unwrap().to_string();
    let (app, app_pane) = app_tab(&wire.screen(&screen_id));

    // A column right of the app column.
    let right = wire.ok(json!({"cmd": "new-pane-right", "pane": app_pane, "width": 0.5}));
    let ordinary_surface = right["surface"].as_u64().unwrap();
    let ordinary = wire.pane_of(ordinary_surface);
    let raw = wire.screen(&screen_id);
    let columns = raw["columns"].as_array().cloned().unwrap_or_default();
    assert_eq!(columns.len(), 2, "{raw}");
    assert_eq!(columns[0]["app"], HOME, "{raw}");
    assert_eq!(columns[0]["sticky"], json!({"edge": "left", "mode": "docked"}), "{raw}");
    assert!(columns[1].get("app").is_none(), "{raw}");
    let before = layout_fingerprint(&wire.tree());

    let code = "app-column-locked";
    for request in [
        json!({"cmd": "new-tab", "pane": app_pane}),
        json!({"cmd": "split", "pane": app_pane, "dir": "down"}),
        json!({"cmd": "split", "pane": app_pane, "dir": "right"}),
        json!({"cmd": "new-pane", "pane": app_pane}),
        json!({"cmd": "new-row", "pane": app_pane, "height_permille": 500}),
        json!({"cmd": "move-tab", "surface": terminal, "pane": app_pane, "index": 0}),
        json!({"cmd": "move-tab", "surface": app, "pane": ordinary, "index": 0}),
        json!({"cmd": "move-tab", "surface": app, "pane": terminal_pane, "index": 0}),
        json!({"cmd": "move-tab-to-split", "surface": terminal, "pane": app_pane, "edge": "bottom"}),
        json!({"cmd": "move-tab-to-column", "surface": app, "pane": ordinary}),
        json!({"cmd": "move-tab-to-new-workspace", "surface": app}),
        json!({"cmd": "set-column-sticky", "pane": app_pane, "sticky": false}),
        json!({"cmd": "set-column-sticky", "pane": app_pane, "sticky": true, "edge": "right"}),
        json!({"cmd": "set-column-sticky", "pane": ordinary, "sticky": true, "edge": "left"}),
        json!({"cmd": "swap-pane", "pane": app_pane, "target": ordinary}),
        json!({"cmd": "swap-pane", "pane": ordinary, "target": app_pane}),
        json!({"cmd": "close-surface", "surface": app}),
        json!({"cmd": "close-tabs", "surfaces": [app]}),
        json!({"cmd": "close-pane", "pane": app_pane}),
        json!({"cmd": "apply-layout", "workspace": wire.workspace_slot(&workspace_id),
               "layout": {"type": "leaf"}}),
    ] {
        wire.refused(request, code);
    }
    let public_app_pane = wire.public_pane(app_pane);
    let public_app = wire.public_tab(app);
    let code = "app.column_locked";
    for (index, (operation, params)) in [
        (
            "tab.create_terminal",
            json!({"workspace": workspace_id, "screen": screen_id,
                                       "pane": public_app_pane}),
        ),
        (
            "pane.split",
            json!({"workspace": workspace_id, "screen": screen_id,
                              "pane": public_app_pane, "direction": "down"}),
        ),
        (
            "pane.close",
            json!({"workspace": workspace_id, "screen": screen_id,
                              "pane": public_app_pane}),
        ),
        (
            "tab.close",
            json!({"workspace": workspace_id, "screen": screen_id,
                             "pane": public_app_pane, "tab": public_app}),
        ),
        ("tab.move", wire.tab_move(app, ordinary)),
        ("tab.move", wire.tab_move(terminal, app_pane)),
    ]
    .into_iter()
    .enumerate()
    {
        wire.v2_refused(operation, params, &format!("lock-{index}-{operation}"), code);
    }
    let column = wire.screen(&screen_id)["columns"][0]["id"].clone();
    let column = wire.mux.with_state(|state| {
        state.resource_indexes.split_ids[&column.as_u64().unwrap()].to_string()
    });
    wire.v2_refused(
        "column.update",
        json!({"workspace": workspace_id, "screen": screen_id, "column": column,
               "sticky": false}),
        "unstick-app-column",
        code,
    );
    assert_eq!(layout_fingerprint(&wire.tree()), before, "a refused op changed the layout");

    // The app column keeps its width rule; ordinary columns are free.
    wire.ok(json!({"cmd": "set-viewport-pane-width", "pane": app_pane, "width": 0.3}));
    wire.ok(json!({"cmd": "new-tab", "pane": ordinary}));
    wire.ok(json!({"cmd": "split", "pane": ordinary, "dir": "down"}));
    wire.ok(json!({"cmd": "set-column-sticky", "pane": ordinary, "sticky": true, "edge": "right"}));
    wire.ok(json!({"cmd": "set-column-sticky", "pane": ordinary, "sticky": false}));
    wire.ok(json!({"cmd": "move-tab", "surface": terminal, "pane": ordinary, "index": 0}));
    // Closing every ordinary column leaves a valid screen of only the app column.
    wire.ok(json!({"cmd": "close-pane", "pane": ordinary}));
    let raw = wire.screen(&screen_id);
    assert_eq!(raw["kind"], "appColumn", "{raw}");
    assert!(tabs(&raw).iter().any(|tab| tab["surface"] == json!(app)), "{raw}");
    wire.mux.shutdown();
}

/// A new ordinary column right of the app column is allowed from every
/// shape (`new-pane-right`, `move-tab-to-column` anchored on the app pane or
/// after the app column, v2 `pane.split` with `viewport_width`); the app
/// column stays at index 0. A move into the app column is refused.
#[test]
fn app_column_accepts_new_columns_to_its_right() {
    let mut wire = Wire::new();
    let created = wire.ensure_app(HOME, "appColumn", "open-home");
    let screen_id = created["value"]["screen_id"].as_str().unwrap().to_string();
    let workspace_id = created["value"]["workspace_id"].as_str().unwrap().to_string();
    let (app, app_pane) = app_tab(&wire.screen(&screen_id));
    let first = wire.ok(json!({"cmd": "new-pane-right", "pane": app_pane, "width": 0.5}));
    let ordinary = wire.pane_of(first["surface"].as_u64().unwrap());
    let moved = wire.ok(json!({"cmd": "new-tab", "pane": ordinary}))["surface"].as_u64().unwrap();
    wire.ok(json!({"cmd": "move-tab-to-column", "surface": moved, "pane": app_pane}));
    let raw = wire.screen(&screen_id);
    let columns = raw["columns"].as_array().unwrap().clone();
    assert_eq!(columns.len(), 3, "{raw}");
    assert_eq!(columns[0]["app"], HOME, "{raw}");
    let app_column = columns[0]["id"].clone();
    let again = wire.ok(json!({"cmd": "new-tab", "pane": ordinary}))["surface"].as_u64().unwrap();
    wire.ok(json!({"cmd": "move-tab-to-column", "surface": again, "pane": ordinary,
                   "after_column": app_column}));
    let columns = wire.screen(&screen_id)["columns"].as_array().unwrap().clone();
    assert_eq!(columns.len(), 4);
    assert_eq!(columns[0]["app"], HOME);
    assert!(wire.mux.with_state(|state| state.pane_of(again)) != Some(ordinary));
    let split = json!({"workspace": workspace_id, "screen": screen_id,
                       "pane": wire.public_pane(app_pane), "direction": "right",
                       "viewport_width": 0.5});
    wire.v2_ok("pane.split", split, Some("split-right-of-app"));
    let raw = wire.screen(&screen_id);
    let columns = raw["columns"].as_array().unwrap().clone();
    assert_eq!(columns.len(), 5, "{raw}");
    assert_eq!(columns[0]["app"], HOME);
    assert_eq!(
        app_tab(&json!({"panes": [raw["panes"].as_array().unwrap().iter()
        .find(|pane| pane["id"] == json!(app_pane)).unwrap()]}))
        .0,
        app
    );

    wire.refused(
        json!({"cmd": "move-tab", "surface": moved, "pane": app_pane, "index": 0}),
        "app-column-locked",
    );
    wire.v2_refused(
        "tab.move",
        wire.tab_move(moved, app_pane),
        "move-into-app",
        "app.column_locked",
    );
    wire.mux.shutdown();
}

/// On an `appColumn` screen that is the lone app column (no `columns`), a
/// tab from another workspace moved to a column anchored on the app pane,
/// with no `after_column`, makes the first ordinary column right of it.
#[test]
fn lone_app_column_takes_a_first_column_from_another_workspace() {
    let mut wire = Wire::new();
    let (terminal, _) = wire.terminal_pane();
    let created = wire.ensure_app(HOME, "appColumn", "open-home");
    let screen_id = created["value"]["screen_id"].as_str().unwrap().to_string();
    let (app, app_pane) = app_tab(&wire.screen(&screen_id));
    assert!(wire.screen(&screen_id).get("columns").is_none());
    wire.ok(json!({"cmd": "move-tab-to-column", "surface": terminal, "pane": app_pane}));
    let raw = wire.screen(&screen_id);
    let columns = raw["columns"].as_array().cloned().unwrap_or_default();
    assert_eq!(columns.len(), 2, "{raw}");
    assert_eq!(columns[0]["app"], HOME, "{raw}");
    assert_eq!(columns[0]["sticky"], json!({"edge": "left", "mode": "docked"}));
    assert!(columns[1].get("app").is_none(), "{raw}");
    assert_eq!(wire.pane_of(app), app_pane, "the app tab stays in the app column");
    let moved_pane = wire.pane_of(terminal);
    assert_ne!(moved_pane, app_pane);
    let column_of = |pane: PaneId| {
        columns.iter().position(|column| layout_panes(&column["layout"]).contains(&pane))
    };
    assert_eq!(column_of(moved_pane), Some(1), "{raw}");
    wire.mux.shutdown();
}

/// `workspace.ensure_home {screen: "appColumn", app}`: the home workspace's
/// first screen becomes `appColumn` with the Home app column at index 0 and
/// every existing pane to its right. Twice in a row and after a restart the
/// result is the same, and no tab is lost.
#[test]
fn home_migration_is_idempotent_and_survives_restart() {
    let store = Store::new("home-migration");
    let mut wire = store.open();
    let home = wire.v2_ok("workspace.ensure_home", json!({}), Some("connect-1"));
    let home_id = home["value"]["workspace_id"].as_str().unwrap().to_string();
    let home_slot = wire.workspace_slot(&home_id);
    let mut conversations = Vec::new();
    let mut second = 0;
    for (index, conversation) in ["conv_01A", "conv_01B"].into_iter().enumerate() {
        let created = wire.ok(json!({"cmd": "new-conversation-tab", "workspace": home_slot,
                                    "conversation": conversation, "owner": "local",
                                    "origin": "home-test", "mutation_id": format!("c{index}")}));
        conversations.push(created["tab_resource_id"].as_str().unwrap().to_string());
        second = created["surface"].as_u64().unwrap();
    }
    let split = wire.pane_of(second);
    wire.ok(json!({"cmd": "move-tab-to-column", "surface": second, "pane": split}));

    let migrate = json!({"screen": "appColumn", "app": HOME});
    let first = wire.v2_ok("workspace.ensure_home", migrate.clone(), Some("connect-2"));
    assert_eq!(first["value"]["workspace_id"], home_id.as_str());
    let check = |wire: &mut Wire| -> Value {
        let tree = wire.tree();
        let workspace = tree["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["resource_id"] == home_id.as_str())
            .cloned()
            .unwrap();
        let screen = workspace["screens"][0].clone();
        assert_eq!(screen["kind"], "appColumn", "{screen}");
        assert_eq!(screen["app"], HOME);
        let columns = screen["columns"].as_array().cloned().unwrap();
        assert_eq!(columns.len(), 3, "the app column and the two existing columns: {screen}");
        assert_eq!(columns[0]["app"], HOME);
        assert_eq!(columns[0]["sticky"], json!({"edge": "left", "mode": "docked"}));
        assert!(columns[1..].iter().all(|column| column.get("app").is_none()));
        let tabs = tabs(&screen);
        assert_eq!(tabs.len(), 3, "{screen}");
        assert_eq!(tabs.iter().filter(|tab| tab["kind"] == "app").count(), 1);
        for tab_id in &conversations {
            assert!(
                tabs.iter().any(|tab| tab["tab_resource_id"] == json!(tab_id)),
                "lost {tab_id}"
            );
        }
        layout_fingerprint(&screen)
    };
    let migrated = check(&mut wire);
    let second = wire.v2_ok("workspace.ensure_home", migrate.clone(), Some("connect-3"));
    assert_eq!(second["value"]["workspace_id"], home_id.as_str());
    assert_eq!(check(&mut wire), migrated, "the second migration changed the screen");
    // A client without the field leaves the migrated screen as it is.
    wire.v2_ok("workspace.ensure_home", json!({}), Some("connect-4"));
    assert_eq!(check(&mut wire), migrated);
    wire.mux.shutdown();
    drop(wire);

    let mut wire = store.open();
    let restarted = check(&mut wire);
    wire.v2_ok("workspace.ensure_home", migrate, Some("connect-5"));
    assert_eq!(check(&mut wire), restarted, "the migration after a restart changed the screen");
    wire.mux.shutdown();
}

/// An empty home workspace migrates to a screen of only the Home app column.
#[test]
fn home_migration_fills_an_empty_home() {
    let mut wire = Wire::new();
    let migrate = json!({"screen": "appColumn", "app": HOME});
    let home = wire.v2_ok("workspace.ensure_home", migrate.clone(), Some("connect-1"));
    let home_id = home["value"]["workspace_id"].as_str().unwrap().to_string();
    let tree = wire.tree();
    let workspace = tree["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["resource_id"] == home_id.as_str())
        .cloned()
        .unwrap();
    assert_eq!(workspace["kind"], "home");
    let screen = workspace["screens"][0].clone();
    assert_eq!((screen["kind"].as_str(), screen["app"].as_str()), (Some("appColumn"), Some(HOME)));
    let _ = app_tab(&screen);
    wire.v2_ok("workspace.ensure_home", migrate, Some("connect-2"));
    assert_eq!(layout_fingerprint(&wire.tree()), layout_fingerprint(&tree));
    let refused =
        wire.v2("workspace.ensure_home", json!({"screen": "app", "app": HOME}), Some("connect-3"));
    assert_eq!(refused["error"]["code"], "validation.invalid", "{refused}");
    wire.mux.shutdown();
}

/// An `app` tab in a workspace screen is an ordinary tab: it is created at
/// a drop target, moves to a split and to a docked column, and closes.
#[test]
fn app_tab_in_a_workspace_screen_is_ordinary() {
    let mut wire = Wire::new();
    let (_, pane) = wire.terminal_pane();
    let created = wire.ok(json!({"cmd": "new-app-tab", "pane": pane, "app": STORE,
                                "route": "/featured", "idempotency_key": "tab-1"}));
    assert_eq!(created["replayed"], false);
    let surface = created["surface"].as_u64().unwrap();
    let replay = wire.ok(json!({"cmd": "new-app-tab", "pane": pane, "app": STORE,
                               "route": "/featured", "idempotency_key": "tab-1"}));
    assert_eq!(
        (replay["surface"].as_u64(), replay["replayed"].as_bool()),
        (Some(surface), Some(true))
    );
    let tab = screens(&wire.tree())
        .iter()
        .flat_map(tabs)
        .find(|tab| tab["surface"] == json!(surface))
        .unwrap();
    assert_eq!(tab["kind"], "app", "{tab}");
    assert_eq!(tab["app"], STORE);
    assert_eq!(tab["route"], "/featured");

    wire.ok(json!({"cmd": "move-tab-to-split", "surface": surface, "pane": pane, "edge": "right"}));
    assert_ne!(wire.pane_of(surface), pane);
    wire.ok(json!({"cmd": "move-tab-to-column", "surface": surface, "pane": pane,
                   "sticky": {"edge": "left", "mode": "docked"}}));
    let screen = screens(&wire.tree())
        .into_iter()
        .find(|screen| tabs(screen).iter().any(|tab| tab["surface"] == json!(surface)))
        .unwrap();
    assert!(screen.get("kind").is_none(), "{screen}");
    wire.ok(json!({"cmd": "close-surface", "surface": surface}));

    // v2 `tab.create_app` at a pane, with the same idempotency.
    let public_pane = wire.public_pane(pane);
    let params = json!({"pane": public_pane, "app": STORE});
    let first = wire.v2_ok("tab.create_app", params.clone(), Some("create-app-tab"));
    let again = wire.v2_ok("tab.create_app", params, Some("create-app-tab"));
    assert_eq!(again["replayed"], true, "{again}");
    assert_eq!(first["value"]["tab_id"], again["value"]["tab_id"]);
    let tab_id = first["value"]["tab_id"].as_str().unwrap().to_string();
    let tab = wire.snapshot()["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tab| tab["id"] == tab_id.as_str())
        .cloned()
        .unwrap();
    assert_eq!(tab["content_kind"], "app", "{tab}");
    assert_eq!(tab["extra"]["app"], STORE);
    wire.v2_ok("tab.close", json!({"tab": tab_id}), Some("close-app-tab"));
    wire.mux.shutdown();
}

/// A connection that did not negotiate `app-screens-v1` reads an `app` tab
/// as a frontend browser tab.
#[test]
fn app_tab_reads_as_browser_without_the_capability() {
    let wire = Wire::new();
    let created = wire.ensure_app(STORE, "app", "open-store");
    let screen = created["value"]["screen_id"].as_str().unwrap().to_string();
    let outbound = Arc::new(BoundedOutbound::default());
    let writer = MessageWriter::new(QueuedSink { outbound: outbound.clone(), control: None });
    handle_message(&wire.mux, 8, &json!({"id": 1, "cmd": "list-workspaces"}).to_string(), &writer);
    let response: Value = serde_json::from_str(&outbound.try_pop().unwrap()).unwrap();
    let raw = screens(&response["data"])
        .into_iter()
        .find(|item| item["resource_id"] == screen.as_str())
        .unwrap();
    assert_eq!(tabs(&raw)[0]["kind"], "browser", "{raw}");
    wire.mux.shutdown();
}
