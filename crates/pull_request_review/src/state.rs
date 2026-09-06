//! Persistence of the reviewer's view choices.
//!
//! This is the only thing the feature stores. Per FR-004 no review data is persisted: not a
//! session, not a draft comment, not a fetched diff. It lives in Zed's own key-value store rather
//! than in user settings (FR-066 rules out a settings entry this phase) or a feature-owned file
//! (FR-021 rules that out outright).

use std::time::Duration;

use db::kvp::KeyValueStore;
use gpui::{App, AppContext as _, Task};
use workspace::dock::DockPosition;

use crate::host::{AuthorFilter, RepositoryCoordinates, SortDirection, StateFilter};

/// Writing is debounced so a reviewer dragging a filter or typing an author name does not issue a
/// database write per keystroke. Persistence must never be on the path of a frame (FR-067).
const SETTLE_DELAY: Duration = Duration::from_millis(400);

const KEY_NAMESPACE: &str = "pull_request_review";

/// The only persisted entity (FR-021, FR-002a).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ListViewState {
    pub state_filter: StateFilter,
    pub author_filter: Option<AuthorFilter>,
    pub sort_direction: SortDirection,
    pub selected_repository: Option<RepositoryCoordinates>,
    /// Serialized separately from Zed's own panel size persistence, which the dock handles through
    /// `Panel::panel_key`. Only the position needs storing here.
    pub dock_position: Option<PersistedDockPosition>,
}

/// `DockPosition` is not serializable, and adding a `serde` impl to it would be a modification of a
/// pre-existing file rather than an addition — which Principle VI forbids.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistedDockPosition {
    Left,
    Right,
    Bottom,
}

impl From<DockPosition> for PersistedDockPosition {
    fn from(position: DockPosition) -> Self {
        match position {
            DockPosition::Left => PersistedDockPosition::Left,
            DockPosition::Right => PersistedDockPosition::Right,
            DockPosition::Bottom => PersistedDockPosition::Bottom,
        }
    }
}

impl From<PersistedDockPosition> for DockPosition {
    fn from(position: PersistedDockPosition) -> Self {
        match position {
            PersistedDockPosition::Left => DockPosition::Left,
            PersistedDockPosition::Right => DockPosition::Right,
            PersistedDockPosition::Bottom => DockPosition::Bottom,
        }
    }
}

/// Namespaced by worktree so two projects open at once do not share one reviewer's filters.
pub fn storage_key(worktree_identity: &str) -> String {
    format!("{KEY_NAMESPACE}::{worktree_identity}")
}

/// Read the stored state, falling back to defaults rather than failing the panel.
///
/// Unreadable and unparseable are treated the same on purpose: a reviewer whose stored state was
/// written by an older build should get a working panel, not an error they cannot act on.
pub fn load(worktree_identity: &str, cx: &App) -> ListViewState {
    let key = storage_key(worktree_identity);
    let stored = match KeyValueStore::global(cx).read_kvp(&key) {
        Ok(Some(stored)) => stored,
        Ok(None) => return ListViewState::default(),
        Err(error) => {
            log::warn!("pull request review: could not read stored view state: {error}");
            return ListViewState::default();
        }
    };

    match serde_json::from_str(&stored) {
        Ok(state) => state,
        Err(error) => {
            log::warn!("pull request review: discarding unparseable stored view state: {error}");
            ListViewState::default()
        }
    }
}

/// Write the state once it has settled.
///
/// The returned [`Task`] must be held by the caller and replaced on each change: dropping the
/// previous one is what debounces, and storing it is what keeps the write from outliving the panel.
pub fn save(worktree_identity: &str, state: &ListViewState, cx: &App) -> Task<()> {
    let key = storage_key(worktree_identity);
    let serialized = match serde_json::to_string(state) {
        Ok(serialized) => serialized,
        Err(error) => {
            // Nothing the reviewer can do about this, and losing a filter preference is not worth
            // a message in their editor — but it must not pass silently either.
            log::error!("pull request review: could not serialize view state: {error}");
            return Task::ready(());
        }
    };

    let store = KeyValueStore::global(cx);
    let executor = cx.background_executor().clone();
    cx.background_spawn(async move {
        executor.timer(SETTLE_DELAY).await;
        if let Err(error) = store.write_kvp(key, serialized).await {
            log::error!("pull request review: could not persist view state: {error}");
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn populated() -> ListViewState {
        ListViewState {
            state_filter: StateFilter::All,
            author_filter: Some(AuthorFilter::Person {
                nickname: "ada".into(),
                label: "Ada Lovelace".into(),
            }),
            sort_direction: SortDirection::LeastRecentFirst,
            selected_repository: Some(RepositoryCoordinates {
                owner: "atlassian".into(),
                name: "twg-cli".into(),
            }),
            dock_position: Some(PersistedDockPosition::Bottom),
        }
    }

    /// FR-021, SC-008: every choice survives a restart.
    #[test]
    fn every_choice_round_trips() {
        let serialized = serde_json::to_string(&populated()).expect("must serialize");
        let restored: ListViewState = serde_json::from_str(&serialized).expect("must deserialize");
        assert_eq!(restored, populated());
    }

    #[test]
    fn me_is_stored_as_a_marker_not_a_resolved_account() {
        let state = ListViewState {
            author_filter: Some(AuthorFilter::Me),
            ..Default::default()
        };
        let serialized = serde_json::to_string(&state).expect("must serialize");
        assert!(
            !serialized.contains("712020"),
            "storing a resolved account id would stop meaning \"me\" if the reviewer changed: \
             {serialized}"
        );
        let restored: ListViewState = serde_json::from_str(&serialized).expect("must deserialize");
        assert_eq!(restored.author_filter, Some(AuthorFilter::Me));
    }

    #[test]
    fn unparseable_stored_state_falls_back_to_defaults_rather_than_failing() {
        for stored in [
            "",
            "not json",
            "{",
            "[]",
            "null",
            r#"{"state_filter": "SomethingElse"}"#,
            r#"{"sort_direction": 7}"#,
            r#"{"author_filter": {"Person": {}}}"#,
        ] {
            // `load` cannot be exercised without an app, so this asserts the same fallback the
            // parse arm of `load` performs.
            let parsed = serde_json::from_str::<ListViewState>(stored)
                .unwrap_or_else(|_| ListViewState::default());
            assert_eq!(
                parsed,
                ListViewState::default(),
                "{stored:?} should have fallen back to defaults"
            );
        }
    }

    #[test]
    fn partial_stored_state_keeps_the_fields_it_does_have() {
        // A build that adds a field must not discard the choices an older build stored.
        let parsed: ListViewState = serde_json::from_str(r#"{"state_filter": "All"}"#)
            .expect("a partial object must parse");
        assert_eq!(parsed.state_filter, StateFilter::All);
        assert_eq!(parsed.sort_direction, SortDirection::MostRecentFirst);
        assert_eq!(parsed.author_filter, None);
    }

    /// FR-021, SC-008: filters, sort and selected repository survive a restart, through Zed's own
    /// key-value store.
    ///
    /// Driven through the real store rather than through serde alone, because the requirement is
    /// about what comes back after the process has gone away — and the debounce, the key
    /// namespacing and the store are all between the choice and that.
    #[gpui::test]
    async fn every_choice_round_trips_through_the_key_value_store(cx: &mut gpui::TestAppContext) {
        let worktree = "/projects/round-trip";

        let task = cx.update(|cx| save(worktree, &populated(), cx));
        // The write is debounced, so nothing has landed yet.
        cx.update(|cx| {
            assert_eq!(
                load(worktree, cx),
                ListViewState::default(),
                "the write must not have happened before it settled"
            );
        });

        cx.executor().advance_clock(SETTLE_DELAY * 2);
        task.await;

        cx.update(|cx| {
            assert_eq!(
                load(worktree, cx),
                populated(),
                "every choice must come back exactly as it was stored"
            );
        });
    }

    /// Unreadable stored state falls back to defaults rather than failing the panel.
    #[gpui::test]
    async fn unparseable_stored_state_loads_as_defaults(cx: &mut gpui::TestAppContext) {
        let worktree = "/projects/corrupt";
        let key = storage_key(worktree);

        let write = cx.update(|cx| {
            let store = KeyValueStore::global(cx);
            cx.background_spawn(async move { store.write_kvp(key, "{ not json".into()).await })
        });
        write.await.expect("the store should accept the write");

        cx.update(|cx| {
            assert_eq!(
                load(worktree, cx),
                ListViewState::default(),
                "a value an older build wrote must not stop the panel opening"
            );
        });
    }

    /// Two projects open at once keep separate choices.
    #[gpui::test]
    async fn choices_do_not_leak_between_worktrees(cx: &mut gpui::TestAppContext) {
        let task = cx.update(|cx| save("/projects/one", &populated(), cx));
        cx.executor().advance_clock(SETTLE_DELAY * 2);
        task.await;

        cx.update(|cx| {
            assert_eq!(load("/projects/one", cx), populated());
            assert_eq!(
                load("/projects/two", cx),
                ListViewState::default(),
                "another project must not inherit these filters"
            );
        });
    }

    #[test]
    fn keys_are_namespaced_per_worktree() {
        assert_ne!(storage_key("/a/project"), storage_key("/b/project"));
        assert!(storage_key("/a/project").starts_with(KEY_NAMESPACE));
    }

    #[test]
    fn dock_position_round_trips_through_its_own_representation() {
        for position in [
            DockPosition::Left,
            DockPosition::Right,
            DockPosition::Bottom,
        ] {
            let persisted = PersistedDockPosition::from(position);
            assert_eq!(DockPosition::from(persisted), position);
        }
    }
}
