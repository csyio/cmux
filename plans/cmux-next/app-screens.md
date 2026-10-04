# cmux next: app screens (screen kinds `workspace`, `app`, `appColumn`)

Spec: worktrees/cmux-next-spec/spec/app-screens.md (Lawrence R63 to R65). Owner of this plan:
layout lead (daemon screen model, app rendering). Manifest `presentation`: app platform lead.
Sidebar items and drag into a workspace: sidebar lead. Primary input: Home lead and keybindings
lead.

## 1. Model (daemon owns it, OWNERSHIP-PRINCIPLES)

```
Screen { kind: ScreenKind, columns: [Column] ... }        // kind defaults to workspace
ScreenKind = workspace | app { app: AppId } | appColumn { app: AppId }
Column { ..., app: bool }                                 // in memory only: column 0 of appColumn
Tab kind `app` { app: AppId, route: Option<String> }      // frontend-rendered app page
```

- `workspace`: today's screen, unchanged.
- `app`: exactly one pane holding exactly one `app` tab. No tab strip, no splits, no columns.
- `appColumn`: column 0 is the app column: `app: Some(id)`, sticky left docked, one pane, one `app`
  tab, no chrome. Columns to its right are ordinary (tabs, splits, scrolling, docks on the other
  edges). With no ordinary column the app column fills the screen; this is the one exception to
  "at least one column scrolls" (E2), and `normalize_sticky_columns` never clears an app column.
- The kind comes from the app manifest `presentation.screen` (`app` | `appColumn`); the daemon
  stores the kind and the app id and has no special case for Home, App Store or CodeRouter.

Invariants (reducer and daemon, with tests):

| Id | Invariant |
| --- | --- |
| A1 | An `app` screen has one pane with one tab, of kind `app`, for its own app. |
| A2 | An `appColumn` screen has exactly one app column, at index 0, sticky left docked, one pane, one `app` tab for its own app. |
| A3 | Only these two shapes hold `app` columns; a `workspace` screen has none. |
| A4 | A refused op changes nothing (typed error, below). |

## 2. Ops and refusals (daemon, capability `app-screens-v1`)

- Create: `workspace.ensure_app {app, kind}` (v2 op, idempotent per app; the pattern of
  `workspace.ensure_home`): one workspace of workspace kind `app` holding one screen of the given
  screen kind and the `app` tab. Home: `workspace.ensure_home` gains `screen: appColumn`, and the
  home workspace's first screen becomes `appColumn` with the Home app column (migration, section 4).
- Refused with `error_code` `app-screen-fixed` on an `app` screen: `new-tab`, `split`, `new-pane`,
  `new-pane-right`, `new-row`, `move-tab*` into or out of it, `move-tab-to-column`,
  `set-column-sticky`, `apply-layout`/`workspace.layout.apply`, `close-tab`/`close-pane` of the app
  tab (closing the screen is allowed: `close-screen`).
- Refused with `app-column-locked` when an op targets the app column or its pane: the same list,
  plus `set-column-sticky` (cannot unstick), `swap-pane`, `move-column`. Allowed: its width.
- The v2 state ops (`pane.split`, `tab.move`, `column.update`, ...) map to the same checks in the
  one shared validator, so the raw and v2 paths refuse alike. The layout reducer gains
  `Reject::AppScreenFixed` / `Reject::AppColumnLocked`, so the check runs once.
- Read shape: `screens[].kind: "workspace" | "app" | "appColumn"`, `screens[].app`, `columns[].app`
  (omitted for `workspace` and ordinary columns, so old clients see an ordinary screen with one
  sticky column).
- Storage (as built): `resource_screen_kinds(screen_id, kind, app_id)`, a side table older builds
  ignore (screen_rows.rs pattern: deleted on tombstone, overlaid and shape-checked at load, rows
  that lost their shape deleted in one transaction). The app column is not a stored flag: it is
  column 0 of an `appColumn` screen (the whole screen while it has no other column), stored pinned
  left and docked in `viewport_json` (so an older build sees an ordinary screen with one pinned
  column), and marked `LayoutColumn.app` in memory so sticky normalization keeps its pin. Every
  projection and plan step re-pins it; the commit check requires it. The app tab is `app_tabs`
  next to its frontend browser row; app workspaces are `app_workspaces`. A closed app workspace
  that `closed.reopen` brings back loads as an ordinary screen with its app tab; the next
  `workspace.ensure_app` for that app restores its kind.

## 3. App (Swift, no cmux-tui window)

- Decode `kind`, `app`, `columns[].app` (ScreenSnapshot, LayoutMapping).
- Rendering: an `app` screen draws the app surface full bleed (no pane chrome, no tab strip, no
  focus ring); the app column draws without chrome as a docked left column.
- Menus and palette: actions that would be refused are hidden on these targets
  (ActionTargetReasons, the "Add a second column first" pattern), so menus and the daemon agree.
- The app surface is the same view type for a screen and for a tab ("Open as Tab": an `app` tab
  in a workspace screen when the manifest has `tab: true`), with one state source.
- Sidebar items call `workspace.ensure_app` with the manifest's `presentation.screen` and show the
  workspace (one per window, the client selects it).

## 4. Migration

- Home: the existing home workspace keeps its id. On the first `workspace.ensure_home` from an
  `app-screens-v1` app, its first screen becomes `appColumn`: the Home app column is inserted at
  index 0 and the existing panes become ordinary columns to its right (no tab lost). Idempotent.
- App Store and CodeRouter are session-local internal page tabs today (`local-page:` ids, not
  restored after relaunch), so nothing persisted migrates. The sidebar opens their app screens
  instead; `appStore.show` and the CodeRouter action open the app screen (or the tab with "Open
  as Tab").

## 5. Steps (each lands alone)

1. This plan.
2. Daemon: model, side tables, refusals in the shared validator and the reducer, read shape,
   `workspace.ensure_app`, Home migration, spec/schema/bindings; red wire and restart tests first.
   Needs a cmux-tui window.
3. App: decode, rendering, hidden menu rows, sidebar items to `ensure_app` (with the sidebar lead),
   "Open as Tab". Swift only.
4. Primary input contract (Home lead, keybindings lead), test matrix row per surface.

## 6. Decisions to confirm

1. App screens live in their own workspace (workspace kind `app`, one per app, made by
   `workspace.ensure_app`), so "one per window" is the client selecting that workspace. The other
   option, an app screen inside the current workspace, gives one per workspace, not one per window.
2. The app column is the only exception to E2 (a screen of only the app column is valid).
3. Refusal codes `app-screen-fixed` and `app-column-locked`.
4. The spec's kind name `home` is `appColumn` here (the manifest value), so no kind is named after
   one app.
