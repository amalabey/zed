//! The Pull Requests panel: the list, and the detail section beneath it.
//!
//! Two things here are load-bearing and easy to get wrong.
//!
//! **The list loads in two phases.** The host's list call carries no per-reviewer approval state —
//! that costs one call per pull request — so every row is rendered from the one list call and the
//! approval bubbles are filled in afterwards, at bounded concurrency, for the rows that are
//! actually visible. FR-012 and SC-004 both depend on this, and a single hydrated call does not
//! exist (research.md §2).
//!
//! **A superseded load is cancelled, not discarded.** Selecting a different pull request drops the
//! previous detail task, which drops the subprocess with it (FR-026, FR-069). Holding the task in a
//! field is what makes that happen; letting it detach would leave a stale result racing the new
//! selection.

use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Pixels, Render, Styled, Subscription, Task,
    WeakEntity, Window, div, px,
};
use project::Project;
use project::git_store::Repository;
use ui::{Divider, prelude::*};
use workspace::Workspace;
use workspace::dock::{DockPosition, Panel, PanelEvent};

use crate::changeset::ChangedFile;
use crate::host::{
    AuthorFilter, CoordinatesError, HostError, Identity, ListQuery, PullRequestDetail,
    PullRequestHost, PullRequestId, PullRequestSummary, RepositoryCoordinates, ReviewerVerdict,
    coordinates_from_remotes,
};
use crate::state::{self, ListViewState, PersistedDockPosition};
use crate::{ClearFilters, DEFAULT_LIST_LIMIT, Refresh, ReverseSort, ToggleFocus, default_host};

/// How many approval hydration calls are in flight at once.
///
/// Each is a subprocess, so this is a trade between filling the bubbles in quickly and swamping the
/// machine on a 500-row list. Six keeps a typical page's bubbles arriving within a second or two
/// without the process count becoming visible in Activity Monitor.
const HYDRATION_CONCURRENCY: usize = 6;

/// Rows hydrated per pass. The panel does not know its own viewport height before it has laid out,
/// so this stands in for "visible": it is generous enough to cover any realistic dock height and
/// small enough that a 500-row list does not turn into 500 subprocesses (FR-007, FR-012).
const HYDRATION_WINDOW: usize = 30;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DetailTab {
    #[default]
    Overview,
    Files,
}

/// One thing being loaded. Each tab owns its own, so switching tabs reloads nothing and loses no
/// state, and a failure in one leaves the other readable (FR-025, FR-031).
#[derive(Clone, Debug, Default)]
pub enum Load<T> {
    #[default]
    Idle,
    Loading,
    Ready(T),
    Failed(HostError),
}

impl<T> Load<T> {
    pub fn ready(&self) -> Option<&T> {
        match self {
            Load::Ready(value) => Some(value),
            _ => None,
        }
    }

    pub fn is_loading(&self) -> bool {
        matches!(self, Load::Loading)
    }

    pub fn error(&self) -> Option<&HostError> {
        match self {
            Load::Failed(error) => Some(error),
            _ => None,
        }
    }
}

pub struct PullRequestPanel {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    focus_handle: FocusHandle,
    /// Namespaces persisted state, so two projects open at once keep separate filters.
    worktree_identity: String,
    /// `Err` means this project cannot be reviewed at all, with the reason — as opposed to a
    /// request that failed (FR-005).
    coordinates: Result<RepositoryCoordinates, CoordinatesError>,
    host: Option<Rc<dyn PullRequestHost>>,
    view_state: ListViewState,
    dock_position: DockPosition,

    pull_requests: Load<Vec<PullRequestSummary>>,
    verdicts: HashMap<PullRequestId, Vec<ReviewerVerdict>>,
    /// Set when the pull request the reviewer had selected is no longer in the list, so its
    /// disappearance is reported rather than looking like an unexplained deselection (FR-011).
    vanished_selection: Option<u64>,

    selected: Option<PullRequestId>,
    active_tab: DetailTab,
    detail: Load<PullRequestDetail>,
    changed_files: Load<Vec<ChangedFile>>,

    viewer: Option<Identity>,

    /// The diff item this panel opened, so successive file opens reuse it rather than accumulating
    /// tabs (FR-039). Tracked here rather than found by searching the pane, so an ordinary commit
    /// the reviewer opened from the git panel is never replaced out from under them.
    diff_item_id: Option<gpui::EntityId>,
    opening_diff: Option<git::repository::RepoPath>,
    diff_problem: Option<String>,

    _list_task: Option<Task<()>>,
    _hydration_task: Option<Task<()>>,
    _viewer_task: Option<Task<()>>,
    /// Held rather than detached: dropping it is what cancels a superseded load (FR-026, FR-069).
    _detail_task: Option<Task<()>>,
    _files_task: Option<Task<()>>,
    _diff_task: Option<Task<()>>,
    _persist_task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for PullRequestPanel {}

impl Focusable for PullRequestPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Installs the panel's action handlers on a workspace.
///
/// Nothing is constructed here. The panel is built the first time the reviewer invokes the action,
/// which is what FR-003 and FR-070 require: a reviewer who never opens it pays nothing.
pub fn register(workspace: &mut Workspace) {
    workspace
        .register_action(|workspace, _: &ToggleFocus, window, cx| {
            toggle_focus(workspace, window, cx);
        })
        .register_action(|workspace, _: &Refresh, _window, cx| {
            with_panel(workspace, cx, |panel, cx| panel.refresh(cx));
        })
        .register_action(|workspace, _: &ClearFilters, _window, cx| {
            with_panel(workspace, cx, |panel, cx| panel.clear_filters(cx));
        })
        .register_action(|workspace, _: &ReverseSort, _window, cx| {
            with_panel(workspace, cx, |panel, cx| panel.reverse_sort(cx));
        });
}

fn toggle_focus(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    if workspace.panel::<PullRequestPanel>(cx).is_some() {
        workspace.toggle_panel_focus::<PullRequestPanel>(window, cx);
        return;
    }

    let panel = cx.new(|cx| PullRequestPanel::new(workspace, window, cx));
    workspace.add_panel(panel, window, cx);
    workspace.focus_panel::<PullRequestPanel>(window, cx);
}

fn with_panel(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
    update: impl FnOnce(&mut PullRequestPanel, &mut Context<PullRequestPanel>),
) {
    // These actions act on a panel the reviewer already opened. Building one here would turn
    // "refresh" into "open", which is not what they asked for.
    let Some(panel) = workspace.panel::<PullRequestPanel>(cx) else {
        return;
    };
    panel.update(cx, update);
}

impl PullRequestPanel {
    pub fn new(workspace: &Workspace, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let project = workspace.project().clone();
        let worktree_identity = worktree_identity(&project, cx);
        let view_state = state::load(&worktree_identity, cx);
        let dock_position = view_state
            .dock_position
            .map(DockPosition::from)
            .unwrap_or(DockPosition::Right);

        let subscriptions = vec![cx.observe(&project, |panel: &mut Self, _project, cx| {
            // A worktree added or removed can change which repository this project maps to.
            panel.resolve_coordinates(cx);
        })];

        let mut panel = Self {
            workspace: workspace.weak_handle(),
            project,
            focus_handle: cx.focus_handle(),
            worktree_identity,
            coordinates: Err(CoordinatesError::NoRepository),
            host: None,
            view_state,
            dock_position,
            pull_requests: Load::Idle,
            verdicts: HashMap::new(),
            vanished_selection: None,
            selected: None,
            active_tab: DetailTab::Overview,
            detail: Load::Idle,
            changed_files: Load::Idle,
            viewer: None,
            diff_item_id: None,
            opening_diff: None,
            diff_problem: None,
            _list_task: None,
            _hydration_task: None,
            _viewer_task: None,
            _detail_task: None,
            _files_task: None,
            _diff_task: None,
            _persist_task: Task::ready(()),
            _subscriptions: subscriptions,
        };
        panel.resolve_coordinates(cx);
        panel
    }

    pub fn view_state(&self) -> &ListViewState {
        &self.view_state
    }

    pub fn pull_requests(&self) -> &Load<Vec<PullRequestSummary>> {
        &self.pull_requests
    }

    pub fn verdicts_for(&self, id: &PullRequestId) -> Option<&Vec<ReviewerVerdict>> {
        self.verdicts.get(id)
    }

    pub fn selected(&self) -> Option<&PullRequestId> {
        self.selected.as_ref()
    }

    pub fn active_tab(&self) -> DetailTab {
        self.active_tab
    }

    pub fn detail(&self) -> &Load<PullRequestDetail> {
        &self.detail
    }

    pub fn changed_files(&self) -> &Load<Vec<ChangedFile>> {
        &self.changed_files
    }

    pub fn viewer(&self) -> Option<&Identity> {
        self.viewer.as_ref()
    }

    pub fn coordinates_error(&self) -> Option<&CoordinatesError> {
        self.coordinates.as_ref().err()
    }

    pub fn repository(&self, cx: &App) -> Option<Entity<Repository>> {
        self.project.read(cx).active_repository(cx)
    }

    pub fn workspace(&self) -> &WeakEntity<Workspace> {
        &self.workspace
    }

    pub fn project(&self) -> &Entity<Project> {
        &self.project
    }

    pub fn vanished_selection(&self) -> Option<u64> {
        self.vanished_selection
    }

    /// Work out which repository this project maps to, and build the host if it does.
    ///
    /// Every failure here is a distinct stated reason. Producing an empty list instead would be
    /// indistinguishable from a repository that genuinely has no pull requests (FR-005).
    fn resolve_coordinates(&mut self, cx: &mut Context<Self>) {
        if self.project.read(cx).is_via_remote_server() {
            // Not a silent gap: the diff is read from a local git repository, and supporting
            // remote projects would need a new remote-server message (research.md §3).
            self.set_coordinates(Err(CoordinatesError::RemoteProject), cx);
            return;
        }

        let Some(repository) = self.repository(cx) else {
            self.set_coordinates(Err(CoordinatesError::NoRepository), cx);
            return;
        };

        let remote_urls = repository.update(cx, |repository, _cx| repository.remote_urls());
        cx.spawn(async move |panel, cx| {
            let resolved = match remote_urls.await {
                Ok(Ok(remotes)) => {
                    // The conventional default remote first, so a project with both `origin` and a
                    // fork remote resolves to the one the reviewer pushes to.
                    let mut urls: Vec<String> = Vec::new();
                    if let Some(origin) = remotes.get("origin") {
                        urls.push(origin.clone());
                    }
                    urls.extend(
                        remotes
                            .iter()
                            .filter(|(name, _)| name.as_str() != "origin")
                            .map(|(_, url)| url.clone()),
                    );
                    coordinates_from_remotes(&urls)
                }
                Ok(Err(_)) | Err(_) => Err(CoordinatesError::NoRemote),
            };

            panel
                .update(cx, |panel, cx| panel.set_coordinates(resolved, cx))
                .ok();
        })
        .detach();
    }

    fn set_coordinates(
        &mut self,
        coordinates: Result<RepositoryCoordinates, CoordinatesError>,
        cx: &mut Context<Self>,
    ) {
        if self.coordinates == coordinates && self.host.is_some() {
            return;
        }
        self.coordinates = coordinates;
        self.host = None;
        self.pull_requests = Load::Idle;
        self.verdicts.clear();

        if self.coordinates.is_ok() {
            self.build_host(cx);
            self.load_list(cx);
            self.load_viewer(cx);
        }
        cx.notify();
    }

    fn build_host(&mut self, cx: &mut Context<Self>) {
        let Some(working_directory) = self.working_directory(cx) else {
            return;
        };
        let environment = self.project.read(cx).environment().downgrade();
        self.host = Some(default_host(working_directory, environment, cx));
    }

    fn working_directory(&self, cx: &App) -> Option<Arc<Path>> {
        self.project
            .read(cx)
            .visible_worktrees(cx)
            .next()
            .map(|worktree| worktree.read(cx).abs_path())
    }

    fn query(&self) -> ListQuery {
        ListQuery {
            state: self.view_state.state_filter,
            // `Me` is a marker until the moment it is used, so it keeps meaning if the reviewer's
            // identity changes (FR-015). Resolving it to the viewer's *nickname* is what the
            // host's author filter matches on.
            author: match self.view_state.author_filter.clone() {
                Some(AuthorFilter::Me) => self.viewer.as_ref().and_then(|viewer| {
                    viewer
                        .nickname
                        .clone()
                        .map(|nickname| AuthorFilter::Person {
                            nickname,
                            label: viewer.label().to_string(),
                        })
                }),
                other => other,
            },
            sort: self.view_state.sort_direction,
            limit: DEFAULT_LIST_LIMIT,
        }
    }

    fn load_viewer(&mut self, cx: &mut Context<Self>) {
        let Some(host) = self.host.clone() else {
            return;
        };
        let task = host.viewer(cx);
        self._viewer_task = Some(cx.spawn(async move |panel, cx| {
            let viewer = task.await;
            panel
                .update(cx, |panel, cx| {
                    match viewer {
                        Ok(viewer) => {
                            let needed_for_filter =
                                panel.view_state.author_filter == Some(AuthorFilter::Me);
                            panel.viewer = Some(viewer);
                            // Only reload if the answer changes what is listed.
                            if needed_for_filter {
                                panel.load_list(cx);
                            }
                        }
                        Err(error) => {
                            // Not surfaced: the reviewer's own identity is only needed for the
                            // "me" filter, and failing it must not put an error over a list that
                            // loaded fine.
                            log::warn!(
                                "pull request review: could not resolve the viewer: {error}"
                            );
                        }
                    }
                    cx.notify();
                })
                .ok();
        }));
    }

    /// Phase one of the two-phase load: one call, every row.
    pub fn load_list(&mut self, cx: &mut Context<Self>) {
        let Some(host) = self.host.clone() else {
            return;
        };
        let Ok(repository) = self.coordinates.clone() else {
            return;
        };

        self.pull_requests = Load::Loading;
        // A list reload invalidates any hydration still in flight for the previous list.
        self._hydration_task = None;
        cx.notify();

        let task = host.list(&repository, self.query(), cx);
        self._list_task = Some(cx.spawn(async move |panel, cx| {
            let listed = task.await;
            panel
                .update(cx, |panel, cx| {
                    match listed {
                        Ok(summaries) => panel.accept_list(summaries, cx),
                        Err(error) if error.is_cancelled() => {}
                        Err(error) => panel.pull_requests = Load::Failed(error),
                    }
                    cx.notify();
                })
                .ok();
        }));
    }

    fn accept_list(&mut self, summaries: Vec<PullRequestSummary>, cx: &mut Context<Self>) {
        // A selection that survived is kept — including across a filter or sort change — so the
        // reviewer does not lose their place (FR-011, US5 scenario 8).
        self.vanished_selection = None;
        if let Some(selected) = self.selected.clone() {
            if !summaries.iter().any(|summary| summary.id == selected) {
                // Reported rather than silently deselected: a pull request that was merged while
                // being read is something the reviewer needs to know.
                self.vanished_selection = Some(selected.number);
            }
        }

        // Verdicts already fetched stay: they belong to a pull request, not to a list load, and
        // discarding them would make every refresh re-fetch what has not changed.
        let listed: std::collections::HashSet<PullRequestId> =
            summaries.iter().map(|summary| summary.id.clone()).collect();
        self.verdicts.retain(|id, _| listed.contains(id));

        self.pull_requests = Load::Ready(summaries);
        self.hydrate_verdicts(cx);
    }

    /// Phase two: approvals, one call per row, at bounded concurrency.
    ///
    /// Runs after the rows are already on screen. Blocking the list on this is what FR-012 and
    /// SC-004 forbid, and the host offers no call that returns both at once.
    fn hydrate_verdicts(&mut self, cx: &mut Context<Self>) {
        let Some(host) = self.host.clone() else {
            return;
        };
        let Some(summaries) = self.pull_requests.ready() else {
            return;
        };

        let pending: Vec<PullRequestId> = summaries
            .iter()
            .take(HYDRATION_WINDOW)
            .map(|summary| summary.id.clone())
            .filter(|id| !self.verdicts.contains_key(id))
            .collect();
        if pending.is_empty() {
            return;
        }

        self._hydration_task = Some(cx.spawn(async move |panel, cx| {
            for batch in pending.chunks(HYDRATION_CONCURRENCY) {
                let Ok(tasks) = panel.update(cx, |_panel, cx| {
                    batch
                        .iter()
                        .map(|id| (id.clone(), host.detail(id, cx)))
                        .collect::<Vec<_>>()
                }) else {
                    return;
                };

                for (id, task) in tasks {
                    let detail = task.await;
                    let updated = panel.update(cx, |panel, cx| {
                        match detail {
                            Ok(detail) => {
                                panel.verdicts.insert(id, detail.verdicts);
                            }
                            Err(error) if error.is_cancelled() => {}
                            Err(error) => {
                                // One row's bubbles failing must not put an error over the whole
                                // list; the row simply shows no verdicts, which is honest.
                                log::warn!(
                                    "pull request review: could not hydrate approvals: {error}"
                                );
                            }
                        }
                        // Bubbles appear as they arrive rather than all at once at the end.
                        cx.notify();
                    });
                    if updated.is_err() {
                        return;
                    }
                }
            }
        }));
    }

    pub fn select(&mut self, id: PullRequestId, cx: &mut Context<Self>) {
        if self.selected.as_ref() == Some(&id) {
            return;
        }
        self.selected = Some(id);
        self.vanished_selection = None;
        self.active_tab = DetailTab::Overview;
        // Dropping the previous tasks cancels the superseded loads and their subprocesses, rather
        // than letting a stale result arrive and replace the newer selection (FR-026, FR-069).
        self._detail_task = None;
        self._files_task = None;
        self.detail = Load::Idle;
        self.changed_files = Load::Idle;
        self.load_detail(cx);
        cx.notify();
    }

    pub fn set_active_tab(&mut self, tab: DetailTab, cx: &mut Context<Self>) {
        if self.active_tab == tab {
            return;
        }
        self.active_tab = tab;
        // Each tab keeps its own load state, so coming back to one reloads nothing (FR-025).
        if tab == DetailTab::Files && matches!(self.changed_files, Load::Idle) {
            self.load_changed_files(cx);
        }
        cx.notify();
    }

    fn load_detail(&mut self, cx: &mut Context<Self>) {
        let (Some(host), Some(id)) = (self.host.clone(), self.selected.clone()) else {
            return;
        };
        self.detail = Load::Loading;
        let task = host.detail(&id, cx);
        self._detail_task = Some(cx.spawn(async move |panel, cx| {
            let detail = task.await;
            panel
                .update(cx, |panel, cx| {
                    // A result that arrives after the reviewer moved on is discarded rather than
                    // shown against the wrong pull request.
                    if panel.selected.as_ref() != Some(&id) {
                        return;
                    }
                    match detail {
                        Ok(detail) => {
                            panel.verdicts.insert(id.clone(), detail.verdicts.clone());
                            panel.detail = Load::Ready(detail);
                        }
                        Err(error) if error.is_cancelled() => {}
                        Err(error) => panel.detail = Load::Failed(error),
                    }
                    cx.notify();
                })
                .ok();
        }));
    }

    pub fn load_changed_files(&mut self, cx: &mut Context<Self>) {
        let (Some(host), Some(id)) = (self.host.clone(), self.selected.clone()) else {
            return;
        };
        self.changed_files = Load::Loading;
        let task = host.changed_files(&id, cx);
        self._files_task = Some(cx.spawn(async move |panel, cx| {
            let files = task.await;
            panel
                .update(cx, |panel, cx| {
                    if panel.selected.as_ref() != Some(&id) {
                        return;
                    }
                    match files {
                        Ok(files) => panel.changed_files = Load::Ready(files),
                        Err(error) if error.is_cancelled() => {}
                        // Reported with its reason and a retry, leaving the Overview tab readable
                        // (FR-031).
                        Err(error) => panel.changed_files = Load::Failed(error),
                    }
                    cx.notify();
                })
                .ok();
        }));
    }

    /// Reload, preserving selection, filters and sort (FR-011).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        // A retry after a missing prerequisite must re-resolve the executable, so a reviewer who
        // installs the tool and retries is not told again that it is absent.
        if matches!(
            self.pull_requests.error(),
            Some(HostError::PrerequisiteMissing { .. })
        ) {
            self.build_host(cx);
        }
        self.load_list(cx);
        if self.selected.is_some() {
            self.load_detail(cx);
            if matches!(self.active_tab, DetailTab::Files) {
                self.load_changed_files(cx);
            }
        }
    }

    pub fn set_state_filter(&mut self, filter: crate::host::StateFilter, cx: &mut Context<Self>) {
        if self.view_state.state_filter == filter {
            return;
        }
        self.view_state.state_filter = filter;
        self.persist(cx);
        self.load_list(cx);
    }

    pub fn set_author_filter(&mut self, filter: Option<AuthorFilter>, cx: &mut Context<Self>) {
        if self.view_state.author_filter == filter {
            return;
        }
        self.view_state.author_filter = filter;
        self.persist(cx);
        self.load_list(cx);
    }

    pub fn reverse_sort(&mut self, cx: &mut Context<Self>) {
        self.view_state.sort_direction = self.view_state.sort_direction.reversed();
        self.persist(cx);
        self.load_list(cx);
    }

    /// Back to the default view in one action (FR-016).
    pub fn clear_filters(&mut self, cx: &mut Context<Self>) {
        let defaults = ListViewState {
            // The dock position and selected repository are not filters, so clearing filters must
            // not move the panel or change which repository is being reviewed.
            dock_position: self.view_state.dock_position,
            selected_repository: self.view_state.selected_repository.clone(),
            ..ListViewState::default()
        };
        if self.view_state == defaults {
            return;
        }
        self.view_state = defaults;
        self.persist(cx);
        self.load_list(cx);
    }

    pub fn has_filters(&self) -> bool {
        self.view_state.state_filter != crate::host::StateFilter::default()
            || self.view_state.author_filter.is_some()
    }

    pub fn host_handle(&self) -> Option<Rc<dyn PullRequestHost>> {
        self.host.clone()
    }

    /// The diff item this panel opened, if it is still the one being reused (FR-039).
    pub fn diff_item_id(&self) -> Option<gpui::EntityId> {
        self.diff_item_id
    }

    /// Acknowledge the open immediately, before anything is read (FR-038, FR-068).
    pub fn begin_diff_open(&mut self, path: git::repository::RepoPath, cx: &mut Context<Self>) {
        self.diff_problem = None;
        self.opening_diff = Some(path);
        cx.notify();
    }

    pub fn finish_diff_open(&mut self, item_id: Option<gpui::EntityId>, cx: &mut Context<Self>) {
        self.opening_diff = None;
        if let Some(item_id) = item_id {
            self.diff_item_id = Some(item_id);
        }
        cx.notify();
    }

    /// Report a diff that cannot be produced, in the panel's own surface: no modal, no focus theft,
    /// and the panel stays usable (FR-037, FR-071).
    pub fn report_diff_problem(&mut self, reason: impl Into<String>, cx: &mut Context<Self>) {
        self.diff_problem = Some(reason.into());
        cx.notify();
    }

    pub fn diff_problem(&self) -> Option<&str> {
        self.diff_problem.as_deref()
    }

    pub fn opening_diff(&self) -> Option<&git::repository::RepoPath> {
        self.opening_diff.as_ref()
    }

    /// Holding the task is what makes cancellation reach the blob load and the revision fetch,
    /// rather than merely discarding the result (FR-069).
    pub fn hold_diff_task(&mut self, task: Task<()>) {
        self._diff_task = Some(task);
    }

    pub fn cancel_diff_open(&mut self, cx: &mut Context<Self>) {
        self._diff_task = None;
        self.opening_diff = None;
        cx.notify();
    }

    fn persist(&mut self, cx: &mut Context<Self>) {
        // Replacing the task drops the previous one, which is what debounces the write. Persistence
        // is never on the path of a frame (FR-067).
        self._persist_task = state::save(&self.worktree_identity, &self.view_state, cx);
    }
}

fn worktree_identity(project: &Entity<Project>, cx: &App) -> String {
    project
        .read(cx)
        .visible_worktrees(cx)
        .next()
        .map(|worktree| worktree.read(cx).abs_path().to_string_lossy().into_owned())
        .unwrap_or_else(|| "no-worktree".to_string())
}

impl Render for PullRequestPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_selection = self.selected.is_some();

        v_flex()
            .key_context("PullRequestPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().panel_background)
            .child(crate::list::render_filter_bar(self, window, cx))
            .child(
                div()
                    .flex_1()
                    .min_h(px(80.))
                    .overflow_hidden()
                    .child(crate::list::render_list(self, window, cx)),
            )
            .when(has_selection, |this| {
                this.child(Divider::horizontal()).child(
                    div()
                        .flex_1()
                        .min_h(px(120.))
                        .overflow_hidden()
                        .child(render_detail(self, window, cx)),
                )
            })
    }
}

/// The detail section: exactly two tabs, Overview selected by default (FR-022).
fn render_detail(
    panel: &mut PullRequestPanel,
    window: &mut Window,
    cx: &mut Context<PullRequestPanel>,
) -> impl IntoElement {
    let active_tab = panel.active_tab;

    v_flex()
        .size_full()
        .child(
            h_flex()
                .p_1()
                .gap_1()
                .border_b_1()
                .border_color(cx.theme().colors().border)
                .child(tab_button("Overview", DetailTab::Overview, active_tab, cx))
                .child(tab_button("Files", DetailTab::Files, active_tab, cx)),
        )
        .child(match active_tab {
            DetailTab::Overview => crate::overview::render(panel, window, cx).into_any_element(),
            DetailTab::Files => crate::files::render(panel, window, cx).into_any_element(),
        })
}

fn tab_button(
    label: &'static str,
    tab: DetailTab,
    active: DetailTab,
    cx: &mut Context<PullRequestPanel>,
) -> impl IntoElement {
    ui::Button::new(label, label)
        .label_size(LabelSize::Small)
        .toggle_state(tab == active)
        .selected_style(ui::ButtonStyle::Tinted(ui::TintColor::Accent))
        .on_click(cx.listener(move |panel, _event, _window, cx| {
            panel.set_active_tab(tab, cx);
        }))
}

impl Panel for PullRequestPanel {
    fn persistent_name() -> &'static str {
        "PullRequestPanel"
    }

    fn panel_key() -> &'static str {
        // Zed's dock persists the panel's *size* under this key, which is how FR-002a's size half
        // is satisfied without the feature storing anything itself.
        "PullRequestPanel"
    }

    fn position(&self, _window: &Window, _cx: &App) -> DockPosition {
        self.dock_position
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(
            position,
            DockPosition::Left | DockPosition::Right | DockPosition::Bottom
        )
    }

    fn set_position(
        &mut self,
        position: DockPosition,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dock_position = position;
        // Stored in Zed's key-value store rather than in user settings: FR-002a rules out settings
        // and FR-021 rules out a feature-owned file.
        self.view_state.dock_position = Some(PersistedDockPosition::from(position));
        self.persist(cx);
        cx.notify();
    }

    fn default_size(&self, _window: &Window, _cx: &App) -> Pixels {
        px(360.)
    }

    fn icon(&self, _window: &Window, _cx: &App) -> Option<IconName> {
        Some(IconName::GitBranch)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Pull Requests")
    }

    fn toggle_action(&self) -> Box<dyn gpui::Action> {
        Box::new(ToggleFocus)
    }

    fn activation_priority(&self) -> u32 {
        // After the panels a reviewer reaches for constantly; this one is opened deliberately.
        8
    }
}

#[cfg(test)]
mod tests {
    /// FR-012 / SC-004: the panel must be able to render rows it has no verdicts for. This asserts
    /// the shape that makes the two-phase load possible — a row and its bubbles are separate pieces
    /// of state — rather than waiting on a host.
    #[test]
    fn rows_and_their_verdicts_are_separate_state() {
        let source = crate::production_source(include_str!("panel.rs"));
        assert!(
            source.contains("pull_requests: Load<Vec<PullRequestSummary>>"),
            "rows must be their own load state"
        );
        assert!(
            source.contains("verdicts: HashMap<PullRequestId, Vec<ReviewerVerdict>>"),
            "verdicts must be keyed per pull request, so a row renders before its own arrives"
        );
        assert!(
            !source.contains("Load<Vec<(PullRequestSummary, Vec<ReviewerVerdict>)>>"),
            "coupling a row to its verdicts would make the list wait for every approval call"
        );
    }

    /// FR-026, FR-069: a superseded load must be *cancelled*, which means its task is held in a
    /// field. A detached task cannot be cancelled, and its result would race the new selection.
    #[test]
    fn superseded_loads_are_held_so_they_can_be_cancelled() {
        let source = crate::production_source(include_str!("panel.rs"));
        for field in [
            "_detail_task",
            "_files_task",
            "_list_task",
            "_hydration_task",
        ] {
            assert!(
                source.contains(&format!("{field}: Option<Task<()>>")),
                "{field} must be held, not detached, so dropping it cancels the work"
            );
        }

        // The guard that stops a late result being applied to a newer selection.
        assert!(
            source
                .matches("if panel.selected.as_ref() != Some(&id)")
                .count()
                >= 2,
            "each detail-scoped load must check the selection has not moved on"
        );
    }

    /// FR-067: persistence is debounced by replacing a held task, never awaited on a frame.
    #[test]
    fn persistence_never_blocks_a_frame() {
        let source = crate::production_source(include_str!("panel.rs"));
        assert!(source.contains("self._persist_task = state::save("));
        assert!(
            !source.contains("state::save(&self.worktree_identity, &self.view_state, cx).detach()"),
            "detaching the write would lose the debounce"
        );
    }

    /// FR-054: the panel offers no resolve affordance, because resolving threads is out of scope.
    #[test]
    fn the_panel_offers_no_thread_resolution() {
        for (number, line) in crate::production_code_lines(include_str!("panel.rs")) {
            let lowered = line.to_lowercase();
            for forbidden in ["resolve_thread", "is_resolved", "unresolved"] {
                assert!(
                    !lowered.contains(forbidden),
                    "panel.rs:{number} mentions {forbidden}: {}",
                    line.trim()
                );
            }
        }
    }
}
