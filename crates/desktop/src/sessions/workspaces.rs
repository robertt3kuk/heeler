//! Ordering, filtering and drag-reordering of workspaces for the sidebar (M12). Pure data: the
//! daemon lists workspaces by `position` and provides the fields every mode reads; the modes,
//! pins' placement and the filters are the client's business (`docs/protocol.md` §
//! Projects and workspaces).

use std::cmp::Reverse;
use std::collections::HashMap;

use hutan_core::WorkspaceId;
use hutan_protocol::messages::{CreatedBy, WorkspaceInfo, WorkspaceKind};

use crate::settings::{WorkspaceSort, WorkspacesSettings};

fn is_home(workspace: &WorkspaceInfo) -> bool {
    workspace.kind == WorkspaceKind::Home
}

/// `workspaces` (in daemon `position` order) as the sidebar lists them: pinned ones first in
/// every mode, then
/// - `manual`: by position, Home leading the unpinned ones;
/// - `recent`: most recent activity first;
/// - `attention`: most sessions waiting for the user first (`needs_you`), then recent activity;
/// - `name`: alphabetical, ignoring case.
///
/// Ties keep the daemon's order.
pub fn sort<'a>(
    workspaces: &'a [WorkspaceInfo],
    mode: WorkspaceSort,
    needs_you: &HashMap<WorkspaceId, usize>,
) -> Vec<&'a WorkspaceInfo> {
    let mut sorted: Vec<&WorkspaceInfo> = workspaces.iter().collect();
    match mode {
        WorkspaceSort::Manual => sorted.sort_by_key(|w| (!w.pinned, !(is_home(w) && !w.pinned))),
        WorkspaceSort::Recent => sorted.sort_by_key(|w| (!w.pinned, Reverse(w.last_active_ms))),
        WorkspaceSort::Attention => sorted.sort_by_key(|w| {
            (
                !w.pinned,
                Reverse(needs_you.get(&w.id).copied().unwrap_or(0)),
                Reverse(w.last_active_ms),
            )
        }),
        WorkspaceSort::Name => sorted.sort_by_cached_key(|w| (!w.pinned, w.name.to_lowercase())),
    }
    // `sort_by_key` is stable, so ties keep the daemon's order (its position order).
    sorted
}

/// Whether the filters of `settings` hide `workspace`. Pinned workspaces and Home are never
/// hidden; the active workspace stays visible while `always_show_active` is on.
pub fn hidden(
    workspace: &WorkspaceInfo,
    settings: &WorkspacesSettings,
    active: Option<WorkspaceId>,
) -> bool {
    if settings.always_show_active && active == Some(workspace.id) {
        return false;
    }
    if workspace.pinned || is_home(workspace) {
        return false;
    }
    (settings.hide_empty && workspace.total_sessions == 0)
        || (settings.hide_sleeping && workspace.live_sessions == 0)
        || (settings.hide_cli_created && workspace.created_by != CreatedBy::User)
}

/// Whether the filter box `query` (words, any order, case-insensitive) matches the name or a
/// root of `workspace`. An empty query matches everything.
pub fn matches_query(workspace: &WorkspaceInfo, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    let mut haystack = workspace.name.to_lowercase();
    for root in &workspace.roots {
        haystack.push(' ');
        haystack.push_str(&root.display().to_string().to_lowercase());
    }
    query.split_whitespace().all(|word| haystack.contains(word))
}

/// The `before` of the `workspace.move` that drops `dragged` onto `target` in manual order, or
/// `None` when nothing would change (same workspace, different groups, Home as the dragged
/// row, an order that already is the wanted one).
///
/// `all` is the daemon's list in position order. Pinned and unpinned workspaces reorder within
/// their own group (the sidebar shows the groups apart); dropping onto a row puts the dragged
/// one in that row's slot, and dropping onto Home puts it first below Home. The dragged workspace
/// is placed right after its new predecessor in the daemon's order (or before the group's first
/// one), so the hidden members of the group keep their relative places.
pub fn reorder(
    all: &[WorkspaceInfo],
    dragged: WorkspaceId,
    target: WorkspaceId,
) -> Option<Option<WorkspaceId>> {
    if dragged == target {
        return None;
    }
    let moved = all.iter().find(|w| w.id == dragged)?;
    let onto = all.iter().find(|w| w.id == target)?;
    if moved.pinned != onto.pinned || (is_home(moved) && !moved.pinned) {
        return None;
    }
    // The group in position order; unpinned Home sits above it and is not part of it.
    let group: Vec<WorkspaceId> = all
        .iter()
        .filter(|w| w.pinned == moved.pinned && !(is_home(w) && !w.pinned))
        .map(|w| w.id)
        .collect();
    let slot = if is_home(onto) && !onto.pinned {
        0
    } else {
        group.iter().position(|id| *id == target)?
    };
    let mut wanted: Vec<WorkspaceId> = group.iter().copied().filter(|id| *id != dragged).collect();
    wanted.insert(slot.min(wanted.len()), dragged);
    if wanted == group {
        return None;
    }
    let at = wanted.iter().position(|id| *id == dragged)?;
    let before = match at.checked_sub(1) {
        // First of its group: right before the member that follows.
        None => wanted.get(1).copied(),
        // Right after the new predecessor in the daemon's order.
        Some(previous) => {
            let predecessor = wanted[previous];
            let mut rest = all
                .iter()
                .map(|w| w.id)
                .filter(|id| *id != dragged)
                .skip_while(|id| *id != predecessor);
            rest.nth(1)
        }
    };
    Some(before)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(name: &str, position: i64) -> WorkspaceInfo {
        WorkspaceInfo {
            id: WorkspaceId::new(),
            name: name.to_owned(),
            roots: vec![format!("/work/{name}").into()],
            kind: WorkspaceKind::Project,
            pinned: false,
            position,
            created_by: CreatedBy::User,
            last_active_ms: 0,
            live_sessions: 1,
            total_sessions: 1,
            parent_id: None,
            project: None,
        }
    }

    fn home(position: i64) -> WorkspaceInfo {
        WorkspaceInfo {
            kind: WorkspaceKind::Home,
            ..ws("Home", position)
        }
    }

    fn names(list: &[&WorkspaceInfo]) -> Vec<String> {
        list.iter().map(|w| w.name.clone()).collect()
    }

    fn fixture() -> Vec<WorkspaceInfo> {
        let mut api = ws("api", 1);
        api.last_active_ms = 500;
        let mut web = ws("Web", 2);
        web.last_active_ms = 900;
        web.pinned = true;
        let mut docs = ws("docs", 3);
        docs.last_active_ms = 100;
        let mut infra = ws("infra", 4);
        infra.last_active_ms = 700;
        let mut pinned_old = ws("zeta", 5);
        pinned_old.pinned = true;
        pinned_old.last_active_ms = 50;
        // Home sits in the middle of the daemon's order on purpose.
        let mut h = home(0);
        h.position = 3;
        h.last_active_ms = 10;
        vec![api, web, h, docs, infra, pinned_old]
    }

    #[test]
    fn manual_puts_pinned_first_then_home_then_the_daemons_order() {
        let list = fixture();
        let sorted = sort(&list, WorkspaceSort::Manual, &HashMap::new());
        assert_eq!(
            names(&sorted),
            ["Web", "zeta", "Home", "api", "docs", "infra"]
        );
    }

    #[test]
    fn recent_orders_by_activity_with_pins_first() {
        let list = fixture();
        let sorted = sort(&list, WorkspaceSort::Recent, &HashMap::new());
        assert_eq!(
            names(&sorted),
            ["Web", "zeta", "infra", "api", "docs", "Home"]
        );
    }

    #[test]
    fn attention_ranks_by_sessions_that_need_you_then_activity() {
        let list = fixture();
        let by_name = |n: &str| list.iter().find(|w| w.name == n).unwrap().id;
        let needs = HashMap::from([(by_name("docs"), 2), (by_name("api"), 1)]);
        let sorted = sort(&list, WorkspaceSort::Attention, &needs);
        assert_eq!(
            names(&sorted),
            ["Web", "zeta", "docs", "api", "infra", "Home"]
        );
    }

    #[test]
    fn name_ignores_case_and_keeps_pins_on_top() {
        let list = fixture();
        let sorted = sort(&list, WorkspaceSort::Name, &HashMap::new());
        assert_eq!(
            names(&sorted),
            ["Web", "zeta", "api", "docs", "Home", "infra"]
        );
    }

    #[test]
    fn filters_never_hide_home_pins_or_the_active_workspace() {
        let mut empty = ws("empty", 1);
        empty.total_sessions = 0;
        empty.live_sessions = 0;
        let mut pinned_empty = ws("pinned", 2);
        pinned_empty.total_sessions = 0;
        pinned_empty.live_sessions = 0;
        pinned_empty.pinned = true;
        let mut empty_home = home(0);
        empty_home.total_sessions = 0;
        empty_home.live_sessions = 0;
        let settings = WorkspacesSettings::default();
        assert!(hidden(&empty, &settings, None));
        assert!(!hidden(&pinned_empty, &settings, None));
        assert!(!hidden(&empty_home, &settings, None));
        assert!(!hidden(&empty, &settings, Some(empty.id)));
        let strict = WorkspacesSettings {
            always_show_active: false,
            ..settings
        };
        assert!(hidden(&empty, &strict, Some(empty.id)));
    }

    #[test]
    fn sleeping_and_cli_created_filters_are_independent() {
        let mut sleeping = ws("sleeping", 1);
        sleeping.live_sessions = 0;
        let mut scripted = ws("scripted", 2);
        scripted.created_by = CreatedBy::Cli;
        let mut by_agent = ws("agent", 3);
        by_agent.created_by = CreatedBy::Agent;
        let plain = WorkspacesSettings::default();
        assert!(!hidden(&sleeping, &plain, None));
        assert!(!hidden(&scripted, &plain, None));
        let sleepy = WorkspacesSettings {
            hide_sleeping: true,
            ..plain.clone()
        };
        assert!(hidden(&sleeping, &sleepy, None));
        assert!(!hidden(&scripted, &sleepy, None));
        let no_cli = WorkspacesSettings {
            hide_cli_created: true,
            ..plain
        };
        assert!(hidden(&scripted, &no_cli, None));
        assert!(hidden(&by_agent, &no_cli, None));
        assert!(!hidden(&sleeping, &no_cli, None));
    }

    #[test]
    fn the_filter_box_matches_names_and_paths_in_any_word_order() {
        let w = ws("Zaman API", 1);
        assert!(matches_query(&w, ""));
        assert!(matches_query(&w, "api zaman"));
        assert!(matches_query(&w, "/work/zaman"));
        assert!(!matches_query(&w, "web"));
    }

    /// Applies `workspace.move` the way the daemon does: `before` or last.
    fn apply(all: &mut Vec<WorkspaceInfo>, id: WorkspaceId, before: Option<WorkspaceId>) {
        let at = all.iter().position(|w| w.id == id).unwrap();
        let moved = all.remove(at);
        let to = match before {
            Some(b) => all.iter().position(|w| w.id == b).unwrap(),
            None => all.len(),
        };
        all.insert(to, moved);
        for (i, w) in all.iter_mut().enumerate() {
            w.position = i as i64;
        }
    }

    fn manual(all: &[WorkspaceInfo]) -> Vec<String> {
        names(&sort(all, WorkspaceSort::Manual, &HashMap::new()))
    }

    fn drop_on(all: &mut Vec<WorkspaceInfo>, dragged: &str, target: &str) -> bool {
        let id = |all: &[WorkspaceInfo], n: &str| all.iter().find(|w| w.name == n).unwrap().id;
        match reorder(all, id(all, dragged), id(all, target)) {
            Some(before) => {
                let dragged = id(all, dragged);
                apply(all, dragged, before);
                true
            }
            None => false,
        }
    }

    fn unpinned() -> Vec<WorkspaceInfo> {
        vec![home(0), ws("a", 1), ws("b", 2), ws("c", 3), ws("d", 4)]
    }

    #[test]
    fn dropping_onto_a_row_takes_its_slot_in_both_directions() {
        let mut all = unpinned();
        assert!(drop_on(&mut all, "a", "c"));
        assert_eq!(manual(&all), ["Home", "b", "c", "a", "d"]);
        assert!(drop_on(&mut all, "d", "b"));
        assert_eq!(manual(&all), ["Home", "d", "b", "c", "a"]);
        assert!(drop_on(&mut all, "d", "a"));
        assert_eq!(manual(&all), ["Home", "b", "c", "a", "d"]);
    }

    #[test]
    fn dropping_onto_home_makes_it_first_below_home() {
        let mut all = unpinned();
        assert!(drop_on(&mut all, "c", "Home"));
        assert_eq!(manual(&all), ["Home", "c", "a", "b", "d"]);
        // Already first: nothing to do.
        assert!(!drop_on(&mut all, "c", "Home"));
    }

    #[test]
    fn home_and_cross_group_drags_do_nothing() {
        let mut all = unpinned();
        all[2].pinned = true; // b
        assert!(!drop_on(&mut all, "Home", "a"), "Home does not move");
        assert!(!drop_on(&mut all, "a", "b"), "unpinned onto pinned");
        assert!(!drop_on(&mut all, "b", "a"), "pinned onto unpinned");
        assert!(!drop_on(&mut all, "a", "a"));
    }

    #[test]
    fn reordering_a_group_leaves_the_other_group_and_hidden_members_alone() {
        // Daemon order interleaves pinned and unpinned workspaces.
        let mut all = vec![
            home(0),
            ws("a", 1),
            ws("p1", 2),
            ws("b", 3),
            ws("p2", 4),
            ws("c", 5),
        ];
        all[2].pinned = true;
        all[4].pinned = true;
        assert_eq!(manual(&all), ["p1", "p2", "Home", "a", "b", "c"]);
        assert!(drop_on(&mut all, "p1", "p2"));
        assert_eq!(manual(&all), ["p2", "p1", "Home", "a", "b", "c"]);
        assert!(drop_on(&mut all, "c", "a"));
        assert_eq!(manual(&all), ["p2", "p1", "Home", "c", "a", "b"]);
        assert!(drop_on(&mut all, "a", "b"));
        assert_eq!(manual(&all), ["p2", "p1", "Home", "c", "b", "a"]);
    }

    #[test]
    fn a_pinned_home_reorders_among_the_pinned() {
        let mut all = vec![home(0), ws("p", 1), ws("a", 2)];
        all[0].pinned = true;
        all[1].pinned = true;
        assert_eq!(manual(&all), ["Home", "p", "a"]);
        assert!(drop_on(&mut all, "Home", "p"));
        assert_eq!(manual(&all), ["p", "Home", "a"]);
    }
}
