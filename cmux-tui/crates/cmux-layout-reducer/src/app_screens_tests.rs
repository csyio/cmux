//! A1 to A4 of the app screens: the rule table, every layout op touching an
//! app screen or an app column, and the shape check.

use super::*;
use crate::{Column, Edge, LayoutOp, TabContent, Workspace, apply, check_state};

/// Workspace 1: an `App` screen 2 (pane 3, tab 4). Workspace 5: an
/// `AppColumn` screen 6 with the app column 7 (pane 8, tab 9) and an
/// ordinary column 10 (pane 11, tabs 12 and 13). Workspace 14: an ordinary
/// screen 15 (pane 16, tabs 17 and 18).
fn state() -> LayoutState {
    let mut state = LayoutState::default();
    for (tab, runtime) in [(4, 40), (9, 90), (12, 120), (13, 130), (17, 170), (18, 180)] {
        state.tabs.insert(tab, TabContent { runtime, terminal: None, dead: false });
    }
    state.panes.insert(3, vec![4]);
    state.panes.insert(8, vec![9]);
    state.panes.insert(11, vec![12, 13]);
    state.panes.insert(16, vec![17, 18]);
    let screen = |id, columns, columns_active, kind| Screen { id, columns, columns_active, kind };
    state.workspaces = vec![
        Workspace {
            id: 1,
            screens: vec![screen(2, vec![Column::single(0, vec![3])], false, ScreenKind::App)],
        },
        Workspace {
            id: 5,
            screens: vec![screen(
                6,
                vec![Column::single(7, vec![8]), Column::single(10, vec![11])],
                true,
                ScreenKind::AppColumn,
            )],
        },
        Workspace {
            id: 14,
            screens: vec![screen(
                15,
                vec![Column::single(0, vec![16])],
                false,
                ScreenKind::Workspace,
            )],
        },
    ];
    assert!(check_state(&state).is_empty(), "{:?}", check_state(&state));
    state
}

fn run(kind: LayoutOpKind) -> Result<LayoutState, Reject> {
    let state = state();
    apply(&state, &LayoutOp { key: "k".into(), kind }).map(|(next, _)| next)
}

#[test]
fn app_rule_table_covers_every_kind_and_action() {
    use AppAction as A;
    let actions = [
        A::AddTab,
        A::Split,
        A::AddColumn,
        A::MoveTabOut,
        A::CloseTab,
        A::ClosePane,
        A::Sticky { left: false },
        A::Sticky { left: true },
        A::Reorder,
        A::ApplyLayout,
        A::Width,
        A::CloseScreen,
    ];
    for action in actions {
        let free = matches!(action, A::Width | A::CloseScreen);
        assert_eq!(check_app_target(ScreenKind::Workspace, false, action), Ok(()));
        let app = check_app_target(ScreenKind::App, false, action);
        let expected = if free { Ok(()) } else { Err(AppRefusal::ScreenFixed) };
        assert_eq!(app, expected, "{action:?}");
        let column = check_app_target(ScreenKind::AppColumn, true, action);
        let column_free = free || action == A::AddColumn;
        let expected = if column_free { Ok(()) } else { Err(AppRefusal::ColumnLocked) };
        assert_eq!(column, expected, "app column {action:?}");
        let ordinary = check_app_target(ScreenKind::AppColumn, false, action);
        let locked = matches!(action, A::ApplyLayout | A::Sticky { left: true });
        let expected = if locked { Err(AppRefusal::ColumnLocked) } else { Ok(()) };
        assert_eq!(ordinary, expected, "ordinary column {action:?}");
    }
}

#[test]
fn app_screen_refuses_every_layout_op() {
    let fixed = Err(Reject::AppScreenFixed(2));
    let edge = Edge::Right;
    for kind in [
        LayoutOpKind::MoveTab { tab: 17, pane: 3, index: 0 },
        LayoutOpKind::MoveTab { tab: 4, pane: 16, index: 0 },
        LayoutOpKind::MoveTabToSplit { tab: 17, pane: 3, edge, new_pane: 90, respawn: None },
        LayoutOpKind::MoveTabToSplit { tab: 4, pane: 16, edge, new_pane: 90, respawn: None },
        LayoutOpKind::MoveTabToColumn {
            tab: 17,
            anchor: 3,
            after_column: None,
            width_permille: 500,
            new_pane: 90,
            new_column: 91,
            base_column: 92,
        },
        LayoutOpKind::MoveTabToNewWorkspace {
            tab: 4,
            index: None,
            new_workspace: 90,
            new_screen: 91,
            new_pane: 92,
        },
        LayoutOpKind::MoveTabToWorkspace {
            tab: 17,
            workspace: 1,
            pane: Some(3),
            new_screen: 90,
            new_pane: 91,
        },
        LayoutOpKind::CloseTab { tab: 4 },
    ] {
        assert_eq!(run(kind.clone()), fixed, "{kind:?}");
    }
}

#[test]
fn app_column_refuses_layout_ops_and_its_neighbors_stay_free() {
    let locked = Err(Reject::AppColumnLocked(6));
    let edge = Edge::Bottom;
    for kind in [
        LayoutOpKind::MoveTab { tab: 12, pane: 8, index: 0 },
        LayoutOpKind::MoveTab { tab: 9, pane: 11, index: 0 },
        LayoutOpKind::MoveTabToSplit { tab: 12, pane: 8, edge, new_pane: 90, respawn: None },
        LayoutOpKind::MoveTabToNewWorkspace {
            tab: 9,
            index: None,
            new_workspace: 90,
            new_screen: 91,
            new_pane: 92,
        },
        LayoutOpKind::CloseTab { tab: 9 },
    ] {
        assert_eq!(run(kind.clone()), locked, "{kind:?}");
    }
    // A column right of the app column, and every op on the ordinary column.
    let column = LayoutOpKind::MoveTabToColumn {
        tab: 17,
        anchor: 8,
        after_column: None,
        width_permille: 500,
        new_pane: 90,
        new_column: 91,
        base_column: 92,
    };
    let next = run(column).unwrap();
    assert_eq!(next.workspaces[1].screens[0].columns[0].panes, vec![8], "the app column stays");
    for kind in [
        LayoutOpKind::MoveTab { tab: 17, pane: 11, index: 0 },
        LayoutOpKind::MoveTabToSplit { tab: 12, pane: 11, edge, new_pane: 90, respawn: None },
        LayoutOpKind::CloseTab { tab: 12 },
    ] {
        assert!(run(kind.clone()).is_ok(), "{kind:?}");
    }
}

#[test]
fn app_tab_in_a_workspace_screen_is_ordinary() {
    // Kind travels with the screen, not the tab: tab 17 is free in screen 15.
    assert!(run(LayoutOpKind::MoveTab { tab: 17, pane: 16, index: 2 }).is_ok());
    assert!(run(LayoutOpKind::CloseTab { tab: 18 }).is_ok());
}

#[test]
fn app_screen_shape_is_an_introduced_violation() {
    let mut state = state();
    state.panes.get_mut(&3).unwrap().push(18);
    state.panes.get_mut(&16).unwrap().retain(|tab| *tab != 18);
    assert!(check_state(&state).contains(&Violation::AppScreenShape { screen: 2 }));
    let mut state = self::state();
    state.panes.get_mut(&8).unwrap().push(18);
    state.panes.get_mut(&16).unwrap().retain(|tab| *tab != 18);
    assert!(check_state(&state).contains(&Violation::AppScreenShape { screen: 6 }));
}
