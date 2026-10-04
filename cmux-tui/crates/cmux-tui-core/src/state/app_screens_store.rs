//! `app-screens-v1` storage (plans/cmux-next/app-screens.md section 2).
//!
//! Three side tables that older builds ignore:
//! - `app_workspaces`: a workspace of kind `app` and its app (one per app),
//!   written in the transaction that creates the workspace.
//! - `app_tabs`: the app (and route) a frontend-rendered tab shows, written
//!   in the commit of its frontend browser row, like `conversation_tabs`.
//! - `app_companion_workspaces`: the ordinary workspace that takes the new
//!   tabs sent to an app workspace (placed directly after it).
//! - `resource_screen_kinds`: a screen of kind `app` and its app. It is a resource side table (screen_rows.rs pattern): created with
//!   the column docks, deleted when its screen is tombstoned, and overlaid at
//!   load only when the screen still has its shape ([`load_screen_apps`]); a
//!   screen that lost it loads as an ordinary screen and keeps every tab.
//!
//! An older build reads an app screen as an ordinary screen holding one
//! frontend browser tab.
//!
//! The store never reads app content.

use std::collections::HashMap;
use std::fmt;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Map, Value, json};

pub(crate) const APP_SCREENS_CAPABILITY: &str = "app-screens-v1";
/// The workspace kind and the canonical tab kind (`content_kind`, raw `kind`).
pub(crate) const APP_KIND: &str = "app";
/// The frontend record of an app tab: the app renders its own page.
pub(crate) const APP_TAB_URL: &str = "about:blank";
pub(crate) const APP_TAB_ENGINE: &str = "webkit";
const APP_ID_MAX_BYTES: usize = 128;
/// The route is also a first-party page's small client state (R91).
const ROUTE_MAX_BYTES: usize = 4096;

pub(crate) fn create_app_state_schema(transaction: &Transaction<'_>) -> anyhow::Result<()> {
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS app_workspaces (
           workspace_id TEXT PRIMARY KEY NOT NULL,
           app_id TEXT NOT NULL UNIQUE
         );
         CREATE TABLE IF NOT EXISTS app_tabs (
           browser_id TEXT PRIMARY KEY NOT NULL,
           app_id TEXT NOT NULL,
           route TEXT,
           origin TEXT,
           mutation_id TEXT
         );
         CREATE TABLE IF NOT EXISTS app_companion_workspaces (
           app_workspace_id TEXT PRIMARY KEY NOT NULL,
           workspace_id TEXT NOT NULL
         );
         CREATE UNIQUE INDEX IF NOT EXISTS app_tabs_by_mutation
           ON app_tabs(origin, mutation_id) WHERE mutation_id IS NOT NULL;",
    )?;
    Ok(())
}

/// The resource side table, created with the other screen side tables.
pub(crate) fn create_screen_kind_schema(transaction: &Transaction<'_>) -> anyhow::Result<()> {
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS resource_screen_kinds (
           screen_id TEXT PRIMARY KEY NOT NULL,
           kind TEXT NOT NULL CHECK(kind = 'app'),
           app_id TEXT NOT NULL
         );",
    )?;
    Ok(())
}

/// An app id as the manifest names it (`publisher/name`): ASCII letters,
/// digits and `.` `_` `-` `/`, starting with a letter or digit.
pub(crate) fn validate_app_id(app: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !app.is_empty()
            && app.len() <= APP_ID_MAX_BYTES
            && app.as_bytes()[0].is_ascii_alphanumeric()
            && app.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._-/".contains(&byte)),
        "bad request: app must be an app id of at most {APP_ID_MAX_BYTES} letters, digits, \
         '.', '_', '-' or '/'"
    );
    Ok(())
}

/// The kind of a screen that is not an ordinary (`workspace`) screen. v1
/// has one: `app`, the only screen of its app workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppScreenKind {
    App,
}

impl AppScreenKind {
    pub(crate) fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "app" => Ok(Self::App),
            _ => anyhow::bail!("bad request: kind must be \"app\""),
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
        }
    }
}

/// The kind and app of an app screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScreenApp {
    pub(crate) kind: AppScreenKind,
    pub(crate) app: String,
}

/// Write the app workspace row of a new workspace (in its creation commit).
/// A row left by a closed workspace of the same app is replaced.
pub(crate) fn write_app_workspace(
    transaction: &Transaction<'_>,
    workspace_id: &str,
    app: &str,
) -> anyhow::Result<()> {
    validate_app_id(app)?;
    transaction.execute("DELETE FROM app_workspaces WHERE app_id = ?1", [app])?;
    transaction.execute(
        "INSERT INTO app_workspaces(workspace_id, app_id) VALUES(?1, ?2)",
        params![workspace_id, app],
    )?;
    Ok(())
}

/// The live workspace of `app`: its public id and key.
pub(crate) fn live_app_workspace(
    connection: &Connection,
    app: &str,
) -> anyhow::Result<Option<(String, String)>> {
    Ok(connection
        .query_row(
            "SELECT a.workspace_id, rw.workspace_key
             FROM app_workspaces AS a
             JOIN resource_workspaces AS rw ON rw.public_id = a.workspace_id
             WHERE a.app_id = ?1 AND rw.deleted_revision IS NULL",
            [app],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?)
}

/// The app of a workspace of kind `app`.
pub(crate) fn workspace_app(
    connection: &Connection,
    workspace_id: &str,
) -> anyhow::Result<Option<String>> {
    Ok(connection
        .query_row(
            "SELECT app_id FROM app_workspaces WHERE workspace_id = ?1",
            [workspace_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?)
}

/// The app of every live app workspace, keyed by workspace key (the raw
/// tree's presentation snapshot).
pub(crate) fn read_app_workspaces(
    connection: &Connection,
) -> anyhow::Result<HashMap<String, String>> {
    let mut statement = connection.prepare(
        "SELECT rw.workspace_key, a.app_id
         FROM app_workspaces AS a
         JOIN resource_workspaces AS rw ON rw.public_id = a.workspace_id
         WHERE rw.deleted_revision IS NULL",
    )?;
    let rows = statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
        .collect::<Result<HashMap<_, _>, _>>()?;
    Ok(rows)
}

/// The app records the raw tree reads from the presentation snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppPresentation {
    /// The app of every live app workspace, by workspace key.
    pub workspaces: HashMap<String, String>,
    /// App tab records, by public browser id.
    pub tabs: HashMap<String, AppTabRecord>,
}

impl AppPresentation {
    pub(crate) fn read(connection: &Connection) -> anyhow::Result<Self> {
        Ok(Self { workspaces: read_app_workspaces(connection)?, tabs: read_app_tabs(connection)? })
    }
}

/// The screen kinds of the live state, by screen slot.
pub(crate) type ScreenApps = HashMap<crate::ScreenId, ScreenApp>;

/// The companion ordinary workspace of an app workspace, if it is live.
pub(crate) fn live_companion(
    connection: &Connection,
    app_workspace: &str,
) -> anyhow::Result<Option<String>> {
    Ok(connection
        .query_row(
            "SELECT c.workspace_id FROM app_companion_workspaces AS c
             JOIN resource_workspaces AS rw ON rw.public_id = c.workspace_id
             WHERE c.app_workspace_id = ?1 AND rw.deleted_revision IS NULL",
            [app_workspace],
            |row| row.get::<_, String>(0),
        )
        .optional()?)
}

/// Record `workspace` as the companion of `app_workspace` (in the commit
/// that creates it).
pub(crate) fn write_companion(
    transaction: &Transaction<'_>,
    app_workspace: &str,
    workspace: &str,
) -> anyhow::Result<()> {
    transaction.execute(
        "INSERT INTO app_companion_workspaces(app_workspace_id, workspace_id) VALUES(?1, ?2)
         ON CONFLICT(app_workspace_id) DO UPDATE SET workspace_id = excluded.workspace_id",
        params![app_workspace, workspace],
    )?;
    Ok(())
}

/// What an `app` tab shows: an app and an optional route inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppTabRecord {
    pub app: String,
    pub route: Option<String>,
}

impl AppTabRecord {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        validate_app_id(&self.app)?;
        if let Some(route) = &self.route {
            anyhow::ensure!(
                route.len() <= ROUTE_MAX_BYTES && !route.chars().any(char::is_control),
                "bad request: route must be at most {ROUTE_MAX_BYTES} bytes without control \
                 characters"
            );
        }
        Ok(())
    }

    /// The flat fields of the tab on the wire: `app`, and `route` when set.
    pub(crate) fn insert_wire(&self, fields: &mut Map<String, Value>) {
        fields.insert("app".into(), json!(self.app));
        if let Some(route) = &self.route {
            fields.insert("route".into(), json!(route));
        }
    }
}

/// Write the record of browser `browser_id` (in the frontend row's commit).
pub(crate) fn write_app_tab(
    transaction: &Transaction<'_>,
    browser_id: &str,
    record: &AppTabRecord,
    mutation: Option<(&str, &str)>,
) -> anyhow::Result<()> {
    record.validate()?;
    transaction.execute(
        "INSERT INTO app_tabs(browser_id, app_id, route, origin, mutation_id)
         VALUES(?1, ?2, ?3, ?4, ?5)",
        params![
            browser_id,
            record.app,
            record.route,
            mutation.map(|(origin, _)| origin),
            mutation.map(|(_, id)| id),
        ],
    )?;
    Ok(())
}

/// The browser id and record a creation with this idempotency key wrote.
pub(crate) fn app_tab_for_mutation(
    connection: &Connection,
    origin: &str,
    mutation_id: &str,
) -> anyhow::Result<Option<(String, AppTabRecord)>> {
    Ok(connection
        .query_row(
            "SELECT browser_id, app_id, route FROM app_tabs
             WHERE origin = ?1 AND mutation_id = ?2",
            params![origin, mutation_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    AppTabRecord { app: row.get(1)?, route: row.get(2)? },
                ))
            },
        )
        .optional()?)
}

/// Every app tab record, keyed by browser id.
pub(crate) fn read_app_tabs(
    connection: &Connection,
) -> anyhow::Result<HashMap<String, AppTabRecord>> {
    let mut statement = connection.prepare("SELECT browser_id, app_id, route FROM app_tabs")?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, AppTabRecord { app: row.get(1)?, route: row.get(2)? }))
        })?
        .collect::<Result<HashMap<_, _>, _>>()?;
    Ok(rows)
}

/// The app record of the tab with public id `tab_id`.
pub(crate) fn tab_app(
    connection: &Connection,
    tab_id: &str,
) -> anyhow::Result<Option<AppTabRecord>> {
    Ok(connection
        .query_row(
            "SELECT a.app_id, a.route FROM resource_tabs AS t
             JOIN app_tabs AS a ON a.browser_id = t.content_id
             WHERE t.public_id = ?1",
            [tab_id],
            |row| Ok(AppTabRecord { app: row.get(0)?, route: row.get(1)? }),
        )
        .optional()?)
}

/// Write the kind row of an app screen.
pub(crate) fn write_screen_app(
    transaction: &Transaction<'_>,
    screen_id: &str,
    screen: &ScreenApp,
) -> anyhow::Result<()> {
    validate_app_id(&screen.app)?;
    transaction.execute(
        "INSERT INTO resource_screen_kinds(screen_id, kind, app_id) VALUES(?1, ?2, ?3)
         ON CONFLICT(screen_id) DO UPDATE SET kind = excluded.kind, app_id = excluded.app_id",
        params![screen_id, screen.kind.as_str(), screen.app],
    )?;
    Ok(())
}

/// Every stored screen kind row, keyed by public screen id. Rows of an
/// unknown kind (a later build) are skipped.
pub(crate) fn read_screen_apps(
    connection: &Connection,
) -> anyhow::Result<HashMap<String, ScreenApp>> {
    let mut statement =
        connection.prepare("SELECT screen_id, kind, app_id FROM resource_screen_kinds")?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(screen, kind, app)| {
            let kind = AppScreenKind::parse(&kind).ok()?;
            Some((screen, ScreenApp { kind, app }))
        })
        .collect())
}

/// The kind row of one screen.
pub(crate) fn screen_app(
    connection: &Connection,
    screen_id: &str,
) -> anyhow::Result<Option<ScreenApp>> {
    let row = connection
        .query_row(
            "SELECT kind, app_id FROM resource_screen_kinds WHERE screen_id = ?1",
            [screen_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    Ok(row.and_then(|(kind, app)| Some(ScreenApp { kind: AppScreenKind::parse(&kind).ok()?, app })))
}

/// Delete the kind row of a closed screen (in the tombstone transaction).
pub(crate) fn delete_screen_app(
    transaction: &Transaction<'_>,
    screen_id: &str,
) -> anyhow::Result<()> {
    transaction.execute("DELETE FROM resource_screen_kinds WHERE screen_id = ?1", [screen_id])?;
    Ok(())
}

/// The v2 `extra` of a workspace, a screen and a tab.
pub(crate) fn workspace_extra(
    connection: &Connection,
    workspace_id: &str,
    fields: &mut Map<String, Value>,
) -> anyhow::Result<()> {
    if let Some(app) = workspace_app(connection, workspace_id)? {
        fields.insert("kind".into(), json!(APP_KIND));
        fields.insert("app".into(), json!(app));
    }
    Ok(())
}

pub(crate) fn screen_extra(
    connection: &Connection,
    screen_id: &str,
    fields: &mut Map<String, Value>,
) -> anyhow::Result<()> {
    if let Some(screen) = screen_app(connection, screen_id)? {
        fields.insert("kind".into(), json!(screen.kind.as_str()));
        fields.insert("app".into(), json!(screen.app));
    }
    Ok(())
}

pub(crate) fn tab_extra(
    connection: &Connection,
    tab_id: &str,
    fields: &mut Map<String, Value>,
) -> anyhow::Result<()> {
    if let Some(record) = tab_app(connection, tab_id)? {
        record.insert_wire(fields);
    }
    Ok(())
}

/// A refused change to an app screen or an app column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppRule {
    refusal: cmux_layout_reducer::AppRefusal,
    /// The public id of the screen.
    screen: String,
}

impl AppRule {
    pub(crate) fn new(refusal: cmux_layout_reducer::AppRefusal, screen: String) -> Self {
        Self { refusal, screen }
    }

    /// The `cmux.protocol/2` catalog error code.
    pub(crate) fn code(&self) -> &'static str {
        match self.refusal {
            cmux_layout_reducer::AppRefusal::ScreenFixed => "app.screen_fixed",
        }
    }

    /// The raw protocol `error_code`.
    pub(crate) fn raw_code(&self) -> &'static str {
        match self.refusal {
            cmux_layout_reducer::AppRefusal::ScreenFixed => "app-screen-fixed",
        }
    }
}

impl fmt::Display for AppRule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (code, screen) = (self.raw_code(), &self.screen);
        match self.refusal {
            cmux_layout_reducer::AppRefusal::ScreenFixed => {
                write!(formatter, "{code}: screen {screen} shows one app and nothing else")
            }
        }
    }
}

impl std::error::Error for AppRule {}

/// The standard resource failure of a refused app screen change.
pub(crate) fn resource_error(error: &anyhow::Error) -> Option<crate::resource::ResourceError> {
    let rule = error.downcast_ref::<AppRule>().filter(|rule| !rule.screen.is_empty())?;
    let details = json!({"screen_id": rule.screen});
    Some(crate::resource::ResourceError::new(rule.code(), rule.to_string(), details, false))
}

/// The raw `error_code` of a refused app screen change.
pub(crate) fn error_code(error: &anyhow::Error) -> Option<String> {
    raw_error_code(error).map(str::to_string)
}

pub(crate) fn raw_error_code(error: &anyhow::Error) -> Option<&'static str> {
    error.downcast_ref::<AppRule>().map(AppRule::raw_code)
}

/// Rewrite every `app` tab in `value` to its `browser` form, for a
/// connection that did not negotiate `app-screens-v1` (the
/// `conversation-tabs-v1` projection, server/conversation_tabs_wire.rs).
pub(crate) fn downgrade_app_tabs(value: &mut Value) -> bool {
    match value {
        Value::Object(object) => {
            let mut changed = false;
            if object.get("content_kind").and_then(Value::as_str) == Some(APP_KIND) {
                object.insert("content_kind".into(), Value::String("browser".into()));
                changed = true;
            }
            if object.contains_key("browser_renderer")
                && object.get("kind").and_then(Value::as_str) == Some(APP_KIND)
            {
                object.insert("kind".into(), Value::String("browser".into()));
                changed = true;
            }
            for child in object.values_mut() {
                changed |= downgrade_app_tabs(child);
            }
            changed
        }
        Value::Array(items) => {
            let mut changed = false;
            for item in items {
                changed |= downgrade_app_tabs(item);
            }
            changed
        }
        _ => false,
    }
}
