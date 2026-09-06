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
use crate::{
    AddComment, CancelComment, ClearFilters, DEFAULT_LIST_LIMIT, Refresh, ReverseSort,
    SubmitComment, ToggleFocus, default_host,
};

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
    /// The description's rendered markdown, built once when the detail arrives.
    ///
    /// Held here rather than built during render: building it needs the project's language
    /// registry, and reaching that from inside the panel's own `render` means reading the panel
    /// entity while it is already being updated, which GPUI panics on.
    description_markdown: Option<Entity<markdown::Markdown>>,
    changed_files: Load<Vec<ChangedFile>>,

    viewer: Option<Identity>,

    /// The diff item this panel opened, so successive file opens reuse it rather than accumulating
    /// tabs (FR-039). Tracked here rather than found by searching the pane, so an ordinary commit
    /// the reviewer opened from the git panel is never replaced out from under them.
    diff_item_id: Option<gpui::EntityId>,
    opening_diff: Option<git::repository::RepoPath>,
    diff_problem: Option<String>,
    /// The blocks this panel has put into the open diff — the existing threads, and the comment
    /// being composed.
    diff_annotations: Option<crate::diff::DiffAnnotations>,
    comments: Vec<crate::host::CommentThread>,
    comment_problem: Option<String>,

    _list_task: Option<Task<()>>,
    _hydration_task: Option<Task<()>>,
    _viewer_task: Option<Task<()>>,
    /// Held rather than detached: dropping it is what cancels a superseded load (FR-026, FR-069).
    _detail_task: Option<Task<()>>,
    _files_task: Option<Task<()>>,
    _diff_task: Option<Task<()>>,
    _comments_task: Option<Task<()>>,
    _composer_subscription: Option<Subscription>,
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
        })
        .register_action(|workspace, _: &AddComment, window, cx| {
            let Some(panel) = workspace.panel::<PullRequestPanel>(cx) else {
                return;
            };
            panel.update(cx, |panel, cx| {
                crate::diff::add_comment(panel, window, cx);
            });
        })
        .register_action(|workspace, _: &SubmitComment, _window, cx| {
            with_panel(workspace, cx, |panel, cx| panel.submit_comment(cx));
        })
        .register_action(|workspace, _: &CancelComment, _window, cx| {
            with_panel(workspace, cx, |panel, cx| panel.cancel_comment(cx));
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
            description_markdown: None,
            changed_files: Load::Idle,
            viewer: None,
            diff_item_id: None,
            opening_diff: None,
            diff_problem: None,
            diff_annotations: None,
            comments: Vec::new(),
            comment_problem: None,
            _list_task: None,
            _hydration_task: None,
            _viewer_task: None,
            _detail_task: None,
            _files_task: None,
            _diff_task: None,
            _comments_task: None,
            _composer_subscription: None,
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

    pub fn description_markdown(&self) -> Option<&Entity<markdown::Markdown>> {
        self.description_markdown.as_ref()
    }

    /// Build the description's markdown entity, once, off the render path.
    fn build_description_markdown(
        &self,
        description: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Option<Entity<markdown::Markdown>> {
        let source = crate::overview::description_to_render(description)?.to_string();
        let language_registry = self.project.read(cx).languages().clone();
        Some(cx.new(|cx| markdown::Markdown::new(source.into(), Some(language_registry), None, cx)))
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
                    coordinates_from_remotes(&urls, crate::supports_remote_host)
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
        self._comments_task = None;
        self.detail = Load::Idle;
        self.description_markdown = None;
        self.changed_files = Load::Idle;
        // The previous pull request's comments and diff decorations belong to it, not to the new
        // selection, so they go rather than being inherited.
        self.comments.clear();
        self.comment_problem = None;
        self.diff_annotations = None;
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
                            panel.description_markdown =
                                panel.build_description_markdown(detail.description.as_deref(), cx);
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

    /// Swap in a host and reload.
    ///
    /// The production path builds its host at the crate's composition point, which a test cannot
    /// reach in. This is the seam that lets the panel's own behaviour — the two-phase load, the
    /// selection rules — be tested against a host that answers instantly and counts its calls.
    #[cfg(test)]
    pub(crate) fn replace_host_for_test(
        &mut self,
        coordinates: RepositoryCoordinates,
        host: Rc<dyn PullRequestHost>,
        cx: &mut Context<Self>,
    ) {
        self.coordinates = Ok(coordinates);
        self.host = Some(host);
        self.verdicts.clear();
        self.selected = None;
        self.load_list(cx);
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

    /// Take ownership of the decorations for a newly-opened diff, and load the comments that belong
    /// on it.
    pub fn attach_diff_annotations(
        &mut self,
        annotations: crate::diff::DiffAnnotations,
        cx: &mut Context<Self>,
    ) {
        self.diff_annotations = Some(annotations);
        self.load_comments(cx);
        cx.notify();
    }

    pub fn diff_annotations_mut(&mut self) -> Option<&mut crate::diff::DiffAnnotations> {
        self.diff_annotations.as_mut()
    }

    /// What a comment needs to know about the pull request it is being written on.
    ///
    /// `None` when there is nothing to comment on, or when commenting is unavailable — in which
    /// case the reason is reported here rather than after the reviewer has typed (FR-047).
    pub fn compose_context(&mut self) -> Option<crate::comments::ComposeContext> {
        let host = self.host.clone()?;
        let detail = self.detail.ready()?;
        Some(crate::comments::ComposeContext {
            pull_request: detail.summary.id.clone(),
            host,
            // The revision the reviewer is reading, which is what the comment is posted against
            // (FR-049).
            against_revision: detail.source.revision.clone(),
            can_comment: detail.can_comment,
            is_open: matches!(detail.summary.state, crate::host::PullRequestState::Open),
        })
    }

    /// Watch a composer so a posted comment appears at its line, and a cancelled one leaves nothing
    /// behind (FR-043, FR-044).
    pub fn observe_composer(
        &mut self,
        composer: Entity<crate::comments::Composer>,
        cx: &mut Context<Self>,
    ) {
        let subscription = cx.subscribe(&composer, |panel, _composer, event, cx| match event {
            crate::comments::ComposerEvent::Posted(thread) => {
                panel.comments.push(thread.clone());
                if let Some(annotations) = panel.diff_annotations.as_mut() {
                    annotations.end_compose(cx);
                }
                panel.redraw_comment_blocks(cx);
                cx.notify();
            }
            crate::comments::ComposerEvent::Cancelled => {
                // Nothing is sent and nothing is kept. There was never anywhere for it to be
                // written down.
                if let Some(annotations) = panel.diff_annotations.as_mut() {
                    annotations.end_compose(cx);
                }
                cx.notify();
            }
        });
        self._composer_subscription = Some(subscription);
    }

    /// Load the comments already on the selected pull request (FR-050).
    pub fn load_comments(&mut self, cx: &mut Context<Self>) {
        let (Some(host), Some(id)) = (self.host.clone(), self.selected.clone()) else {
            return;
        };
        self.comment_problem = None;
        let task = host.comments(&id, cx);
        self._comments_task = Some(cx.spawn(async move |panel, cx| {
            let loaded = task.await;
            panel
                .update(cx, |panel, cx| {
                    if panel.selected.as_ref() != Some(&id) {
                        return;
                    }
                    match loaded {
                        Ok(threads) => {
                            panel.comments = threads;
                            panel.redraw_comment_blocks(cx);
                        }
                        Err(error) if error.is_cancelled() => {}
                        Err(error) => {
                            // The diff stays readable and a comment can still be added; only the
                            // existing threads are missing, and the reason is stated (FR-055).
                            panel.comment_problem = Some(error.message());
                        }
                    }
                    cx.notify();
                })
                .ok();
        }));
    }

    pub fn comments(&self) -> &[crate::host::CommentThread] {
        &self.comments
    }

    pub fn submit_comment(&mut self, cx: &mut Context<Self>) {
        let Some(composer) = self
            .diff_annotations
            .as_ref()
            .and_then(|annotations| annotations.composer().cloned())
        else {
            return;
        };
        composer.update(cx, |composer, cx| composer.submit(cx));
    }

    pub fn cancel_comment(&mut self, cx: &mut Context<Self>) {
        let Some(composer) = self
            .diff_annotations
            .as_ref()
            .and_then(|annotations| annotations.composer().cloned())
        else {
            return;
        };
        composer.update(cx, |composer, cx| composer.cancel(cx));
    }

    pub fn comment_problem(&self) -> Option<&str> {
        self.comment_problem.as_deref()
    }

    /// Put the current threads back into the open diff, marking the outdated ones.
    fn redraw_comment_blocks(&mut self, cx: &mut Context<Self>) {
        let Some(context) = self.compose_context() else {
            return;
        };
        let reply_context = crate::comments::ThreadReplyContext {
            pull_request: context.pull_request,
            host: context.host,
            against_revision: context.against_revision,
            can_comment: context.can_comment,
            is_open: context.is_open,
        };

        // Outdated is derived locally by comparing each anchor against the changeset actually being
        // shown — never reported by the host, never used to move a thread (FR-053).
        let shown = self
            .changed_files
            .ready()
            .map(|files| {
                files
                    .iter()
                    .map(|file| file.path.clone())
                    .collect::<std::collections::HashSet<_>>()
            })
            .unwrap_or_default();
        let mut threads = self.comments.clone();
        crate::comments::mark_outdated(&mut threads, &|anchor| shown.contains(&anchor.path));

        if let Some(annotations) = self.diff_annotations.as_mut() {
            annotations.set_threads(threads, Some(reply_context), cx);
        }
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
    use super::*;
    use crate::changeset::ChangeKind;
    use crate::host::{
        BranchRef, CommentId, CommentThread, DraftComment, PullRequestState, Verdict,
    };
    use chrono::{TimeZone as _, Utc};
    use gpui::{TestAppContext, VisualTestContext};
    use std::cell::{Cell, RefCell};
    use util::path;

    /// A host that answers from memory, counts its calls, and can be told to leave a call pending.
    ///
    /// Counting calls is what lets the two-phase load be tested as a *bound* rather than as a
    /// sequence of screenshots: FR-012 is satisfied by not making 500 calls, and that is directly
    /// observable here.
    struct FakeHost {
        summaries: Vec<PullRequestSummary>,
        detail_calls: Rc<Cell<usize>>,
        list_calls: Rc<Cell<usize>>,
        /// Numbers whose `detail` never resolves, standing in for a call still in flight.
        pending_details: Rc<RefCell<Vec<u64>>>,
        posted: Rc<RefCell<Vec<DraftComment>>>,
        post_fails: Rc<Cell<bool>>,
        threads: Rc<RefCell<Vec<CommentThread>>>,
    }

    impl FakeHost {
        fn with_rows(count: u64) -> Self {
            let repository = repository();
            let summaries = (0..count)
                .map(|index| PullRequestSummary {
                    id: PullRequestId {
                        number: index + 1,
                        repository: repository.clone(),
                    },
                    title: format!("Pull request {}", index + 1),
                    author: Identity {
                        display_name: Some("Ada Lovelace".into()),
                        nickname: Some("ada".into()),
                        account_id: "712020:aaaa".into(),
                        avatar_url: None,
                    },
                    state: PullRequestState::Open,
                    is_draft: false,
                    opened_at: fixed_clock(),
                    // Descending, so the default sort keeps them in number order.
                    last_activity_at: fixed_clock() - chrono::Duration::minutes(index as i64),
                    web_url: None,
                    comment_count: 0,
                })
                .collect();
            Self {
                summaries,
                detail_calls: Rc::new(Cell::new(0)),
                list_calls: Rc::new(Cell::new(0)),
                pending_details: Rc::new(RefCell::new(Vec::new())),
                posted: Rc::new(RefCell::new(Vec::new())),
                post_fails: Rc::new(Cell::new(false)),
                threads: Rc::new(RefCell::new(Vec::new())),
            }
        }

        fn leaving_pending(self, numbers: &[u64]) -> Self {
            *self.pending_details.borrow_mut() = numbers.to_vec();
            self
        }

        fn leaving_every_detail_pending(self) -> Self {
            let numbers: Vec<u64> = self.summaries.iter().map(|s| s.id.number).collect();
            self.leaving_pending(&numbers)
        }

        fn detail_for(&self, id: &PullRequestId) -> PullRequestDetail {
            let summary = self
                .summaries
                .iter()
                .find(|summary| &summary.id == id)
                .cloned()
                .expect("the fake host is only asked about rows it listed");
            let branch = |name: &str, revision: &str| BranchRef {
                branch: name.into(),
                revision: revision.into(),
                repository: repository(),
            };
            PullRequestDetail {
                description: Some(format!("Body of {}", summary.title)),
                source: branch("feature", "aaaaaaaaaaaa"),
                destination: branch("main", "bbbbbbbbbbbb"),
                verdicts: vec![ReviewerVerdict {
                    reviewer: Identity {
                        display_name: Some("Grace Hopper".into()),
                        ..Default::default()
                    },
                    verdict: Verdict::Approved,
                    at: None,
                }],
                can_comment: true,
                summary,
            }
        }
    }

    impl PullRequestHost for FakeHost {
        fn list(
            &self,
            _repository: &RepositoryCoordinates,
            _query: ListQuery,
            cx: &App,
        ) -> Task<Result<Vec<PullRequestSummary>, HostError>> {
            self.list_calls.set(self.list_calls.get() + 1);
            let summaries = self.summaries.clone();
            cx.background_spawn(async move { Ok(summaries) })
        }

        fn detail(
            &self,
            id: &PullRequestId,
            cx: &App,
        ) -> Task<Result<PullRequestDetail, HostError>> {
            self.detail_calls.set(self.detail_calls.get() + 1);
            if self.pending_details.borrow().contains(&id.number) {
                return cx.background_spawn(async move {
                    futures::future::pending::<Result<PullRequestDetail, HostError>>().await
                });
            }
            let detail = self.detail_for(id);
            cx.background_spawn(async move { Ok(detail) })
        }

        fn changed_files(
            &self,
            _id: &PullRequestId,
            cx: &App,
        ) -> Task<Result<Vec<ChangedFile>, HostError>> {
            cx.background_spawn(async move {
                Ok(vec![ChangedFile {
                    path: git::repository::RepoPath::new("a.rs").expect("a valid path"),
                    previous_path: None,
                    change_kind: ChangeKind::Modified,
                    lines_added: 3,
                    lines_removed: 1,
                    render_refusal: None,
                }])
            })
        }

        fn comments(
            &self,
            _id: &PullRequestId,
            cx: &App,
        ) -> Task<Result<Vec<CommentThread>, HostError>> {
            let threads = self.threads.borrow().clone();
            cx.background_spawn(async move { Ok(threads) })
        }

        fn post_comment(
            &self,
            _id: &PullRequestId,
            draft: DraftComment,
            cx: &App,
        ) -> Task<Result<CommentThread, HostError>> {
            self.posted.borrow_mut().push(draft.clone());
            if self.post_fails.get() {
                return cx.background_spawn(async move {
                    Err(HostError::Unreachable {
                        detail: "the network is down".into(),
                    })
                });
            }
            let echoed = CommentThread {
                id: CommentId(format!("posted-{}", self.posted.borrow().len())),
                anchor: Some(crate::host::CommentAnchor {
                    path: draft.path.clone(),
                    side: draft.side,
                    lines: draft.line..=draft.line,
                }),
                author: Identity {
                    display_name: Some("Ada Lovelace".into()),
                    ..Default::default()
                },
                body: draft.body,
                created_at: fixed_clock(),
                replies: Vec::new(),
                is_deleted: false,
                is_pending: false,
                is_outdated: false,
            };
            cx.background_spawn(async move { Ok(echoed) })
        }

        fn viewer(&self, cx: &App) -> Task<Result<Identity, HostError>> {
            cx.background_spawn(async move {
                Ok(Identity {
                    display_name: Some("Ada Lovelace".into()),
                    nickname: Some("ada".into()),
                    account_id: "712020:aaaa".into(),
                    avatar_url: None,
                })
            })
        }
    }

    fn repository() -> RepositoryCoordinates {
        RepositoryCoordinates {
            owner: "atlassian".into(),
            name: "twg-cli".into(),
        }
    }

    fn fixed_clock() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 6, 12, 0, 0)
            .single()
            .expect("a valid fixed clock")
    }

    fn init_test(cx: &mut TestAppContext) {
        zlog::init_test();
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            language_model::init(cx);
            editor::init(cx);
        });
    }

    /// Build a panel over a fake project, then give it a fake host.
    ///
    /// The panel's own coordinate resolution runs first and is allowed to settle: in a test build
    /// executable resolution never searches a real `PATH`, so the real host resolves to a missing
    /// prerequisite and reaches nothing.
    async fn setup(
        cx: &mut TestAppContext,
        host: FakeHost,
    ) -> (Entity<PullRequestPanel>, Rc<FakeHost>, VisualTestContext) {
        init_test(cx);

        let fs = fs::FakeFs::new(cx.background_executor.clone());
        fs.insert_tree(
            path!("/project"),
            serde_json::json!({ ".git": {}, "a.rs": "fn main() {}\n" }),
        )
        .await;
        fs.set_remote_for_repo(
            path!("/project/.git").as_ref(),
            "origin",
            "git@example.invalid:atlassian/twg-cli.git",
        );

        let project = project::Project::test(fs.clone(), [Path::new(path!("/project"))], cx).await;
        let window = cx.add_window(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = window
            .read_with(cx, |multi, _| multi.workspace().clone())
            .expect("the test workspace must exist");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.executor().run_until_parked();

        let panel = workspace.update_in(&mut cx, |workspace, window, cx| {
            cx.new(|cx| PullRequestPanel::new(workspace, window, cx))
        });
        cx.executor().run_until_parked();

        let host = Rc::new(host);
        panel.update(&mut cx, |panel, cx| {
            panel.replace_host_for_test(repository(), host.clone(), cx);
        });
        cx.executor().run_until_parked();

        (panel, host, cx)
    }

    /// FR-012, SC-004: the list is readable while hydration is still in flight.
    ///
    /// Every `detail` call is left pending, so if the list waited on approvals there would be no
    /// rows at all.
    #[gpui::test]
    async fn rows_render_before_approvals_arrive(cx: &mut TestAppContext) {
        let (panel, host, cx) =
            setup(cx, FakeHost::with_rows(3).leaving_every_detail_pending()).await;

        panel.read_with(&cx, |panel, _cx| {
            let rows = panel
                .pull_requests()
                .ready()
                .expect("the rows must be readable before any approval arrives");
            assert_eq!(rows.len(), 3);
            for row in rows {
                assert!(
                    panel.verdicts_for(&row.id).is_none(),
                    "no approvals can have arrived — every detail call is still pending"
                );
            }
        });

        assert!(
            host.detail_calls.get() > 0,
            "hydration must actually have started, or this test proves nothing"
        );
        assert_eq!(host.list_calls.get(), 1, "one list call renders every row");
    }

    /// Approvals fill in as they arrive, rather than all at once at the end.
    #[gpui::test]
    async fn approvals_fill_in_after_the_rows(cx: &mut TestAppContext) {
        let (panel, host, cx) = setup(cx, FakeHost::with_rows(3)).await;

        panel.read_with(&cx, |panel, _cx| {
            let rows = panel.pull_requests().ready().expect("rows must be listed");
            assert_eq!(rows.len(), 3);
            for row in rows {
                let verdicts = panel
                    .verdicts_for(&row.id)
                    .expect("every row's approvals should have arrived by now");
                assert_eq!(verdicts.len(), 1);
                assert_eq!(verdicts[0].verdict, Verdict::Approved);
            }
        });
        assert_eq!(host.detail_calls.get(), 3, "one approval call per row");
    }

    /// FR-012, FR-067, SC-004: the first rows are produced without loading every pull request's
    /// approvals.
    ///
    /// The bound is what matters and what is observable. The frame-budget half of SC-004 is a
    /// timing property measured against a real host in T112, not something a deterministic
    /// executor can tell us.
    #[gpui::test]
    async fn a_five_hundred_row_list_does_not_make_five_hundred_approval_calls(
        cx: &mut TestAppContext,
    ) {
        let (panel, host, cx) =
            setup(cx, FakeHost::with_rows(500).leaving_every_detail_pending()).await;

        panel.read_with(&cx, |panel, _cx| {
            assert_eq!(
                panel.pull_requests().ready().map(Vec::len),
                Some(500),
                "every row is listed from the one list call"
            );
        });

        assert!(
            host.detail_calls.get() <= HYDRATION_WINDOW,
            "hydration must stay bounded: {} calls for 500 rows",
            host.detail_calls.get()
        );
        assert!(
            host.detail_calls.get() <= HYDRATION_CONCURRENCY,
            "with every call pending, only the first batch can have been issued: {} calls",
            host.detail_calls.get()
        );
    }

    /// FR-026, FR-069: a superseded detail load cannot arrive later and replace the newer
    /// selection.
    ///
    /// Pull request 1's `detail` never resolves. Selecting 2 drops that task — which is what
    /// cancels the underlying work rather than merely discarding its result — so 1's result cannot
    /// arrive at all, and the panel shows 2.
    #[gpui::test]
    async fn a_superseded_detail_load_cannot_replace_the_newer_selection(cx: &mut TestAppContext) {
        let (panel, _host, mut cx) = setup(cx, FakeHost::with_rows(2).leaving_pending(&[1])).await;

        let id = |number: u64| PullRequestId {
            number,
            repository: repository(),
        };

        panel.update(&mut cx, |panel, cx| panel.select(id(1), cx));
        cx.executor().run_until_parked();
        panel.read_with(&cx, |panel, _cx| {
            assert!(
                panel.detail().is_loading(),
                "pull request 1's detail is still in flight"
            );
        });

        panel.update(&mut cx, |panel, cx| panel.select(id(2), cx));
        cx.executor().run_until_parked();

        panel.read_with(&cx, |panel, _cx| {
            let detail = panel
                .detail()
                .ready()
                .expect("the newer selection's detail must be shown");
            assert_eq!(detail.summary.id, id(2));
            assert_eq!(panel.selected(), Some(&id(2)));
        });

        // Nothing can change that afterwards: the older task no longer exists to complete.
        cx.executor().run_until_parked();
        panel.read_with(&cx, |panel, _cx| {
            assert_eq!(
                panel
                    .detail()
                    .ready()
                    .map(|detail| detail.summary.id.clone()),
                Some(id(2)),
                "the abandoned load must never arrive"
            );
        });
    }

    /// FR-025, US2 scenario 7: each tab has its own load state, so switching reloads nothing.
    #[gpui::test]
    async fn switching_tabs_reloads_nothing(cx: &mut TestAppContext) {
        let (panel, host, mut cx) = setup(cx, FakeHost::with_rows(1)).await;
        let id = PullRequestId {
            number: 1,
            repository: repository(),
        };

        panel.update(&mut cx, |panel, cx| panel.select(id.clone(), cx));
        cx.executor().run_until_parked();

        let details_after_select = host.detail_calls.get();

        panel.update(&mut cx, |panel, cx| {
            panel.set_active_tab(DetailTab::Files, cx)
        });
        cx.executor().run_until_parked();
        panel.read_with(&cx, |panel, _cx| {
            assert!(
                panel.changed_files().ready().is_some(),
                "the Files tab loads on first view"
            );
            assert!(
                panel.detail().ready().is_some(),
                "the Overview tab keeps its state while Files is shown"
            );
        });

        panel.update(&mut cx, |panel, cx| {
            panel.set_active_tab(DetailTab::Overview, cx)
        });
        panel.update(&mut cx, |panel, cx| {
            panel.set_active_tab(DetailTab::Files, cx)
        });
        cx.executor().run_until_parked();

        assert_eq!(
            host.detail_calls.get(),
            details_after_select,
            "switching between tabs must not reload the pull request"
        );
    }

    /// FR-011, US5 scenario 8: a selection that is still listed survives a refresh.
    #[gpui::test]
    async fn a_refresh_keeps_the_selection_and_reports_one_that_vanished(cx: &mut TestAppContext) {
        let (panel, _host, mut cx) = setup(cx, FakeHost::with_rows(3)).await;
        let id = |number: u64| PullRequestId {
            number,
            repository: repository(),
        };

        panel.update(&mut cx, |panel, cx| panel.select(id(2), cx));
        cx.executor().run_until_parked();

        panel.update(&mut cx, |panel, cx| panel.refresh(cx));
        cx.executor().run_until_parked();

        panel.read_with(&cx, |panel, _cx| {
            assert_eq!(panel.selected(), Some(&id(2)), "the selection survives");
            assert_eq!(
                panel.vanished_selection(),
                None,
                "nothing vanished, so nothing is reported"
            );
        });

        // Now the host stops listing it. The reviewer must be told, not silently deselected.
        panel.update(&mut cx, |panel, cx| {
            panel.replace_host_for_test(repository(), Rc::new(FakeHost::with_rows(1)), cx);
            panel.select(id(1), cx);
        });
        cx.executor().run_until_parked();
        panel.update(&mut cx, |panel, cx| {
            panel.selected = Some(id(99));
            panel.load_list(cx);
        });
        cx.executor().run_until_parked();

        panel.read_with(&cx, |panel, _cx| {
            assert_eq!(
                panel.vanished_selection(),
                Some(99),
                "a selected pull request that is no longer listed must be reported"
            );
        });
    }

    /// FR-021, FR-002a: the dock position round-trips through the key-value store rather than
    /// through user settings or a feature-owned file.
    #[gpui::test]
    async fn the_dock_position_persists_without_touching_settings(cx: &mut TestAppContext) {
        let (panel, _host, mut cx) = setup(cx, FakeHost::with_rows(1)).await;

        panel.update_in(&mut cx, |panel, window, cx| {
            assert!(panel.position_is_valid(DockPosition::Bottom));
            panel.set_position(DockPosition::Bottom, window, cx);
        });

        panel.read_with(&cx, |panel, _cx| {
            assert_eq!(
                panel.view_state().dock_position,
                Some(PersistedDockPosition::Bottom)
            );
        });

        // The write is debounced, so let it settle and confirm it landed in the store.
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.executor().run_until_parked();

        let stored = panel.read_with(&cx, |panel, cx| {
            crate::state::load(&panel.worktree_identity, cx)
        });
        assert_eq!(
            stored.dock_position,
            Some(PersistedDockPosition::Bottom),
            "the position must be readable back after a restart"
        );
    }

    fn anchored_thread(id: &str, path: &str, line: u32, body: &str) -> CommentThread {
        CommentThread {
            id: CommentId(id.into()),
            anchor: Some(crate::host::CommentAnchor {
                path: git::repository::RepoPath::new(path).expect("a valid path"),
                side: crate::host::DiffSide::New,
                lines: line..=line,
            }),
            author: Identity {
                display_name: Some("Grace Hopper".into()),
                ..Default::default()
            },
            body: body.into(),
            created_at: fixed_clock(),
            replies: Vec::new(),
            is_deleted: false,
            is_pending: false,
            is_outdated: false,
        }
    }

    /// FR-050, FR-055: selecting a pull request loads the comments already on it, and a failure
    /// there leaves the rest usable.
    #[gpui::test]
    async fn selecting_a_pull_request_loads_its_existing_comments(cx: &mut TestAppContext) {
        let host = FakeHost::with_rows(1);
        *host.threads.borrow_mut() = vec![
            anchored_thread("1", "a.rs", 3, "Why here?"),
            // An unanchored comment belongs in Overview, and must survive the round trip.
            CommentThread {
                anchor: None,
                ..anchored_thread("2", "a.rs", 0, "General note")
            },
        ];
        let (panel, _host, mut cx) = setup(cx, host).await;

        let id = PullRequestId {
            number: 1,
            repository: repository(),
        };
        panel.update(&mut cx, |panel, cx| panel.select(id, cx));
        cx.executor().run_until_parked();
        // Comments load once a diff is opened; before that there is nothing to anchor them to.
        panel.update(&mut cx, |panel, cx| panel.load_comments(cx));
        cx.executor().run_until_parked();

        panel.read_with(&cx, |panel, _cx| {
            assert_eq!(panel.comments().len(), 2);
            assert_eq!(
                crate::comments::unanchored(panel.comments()).len(),
                1,
                "the unanchored comment must be kept for the Overview tab"
            );
            assert_eq!(panel.comment_problem(), None);
        });
    }

    /// FR-044: cancelling posts nothing.
    #[gpui::test]
    async fn cancelling_a_comment_posts_nothing(cx: &mut TestAppContext) {
        let (panel, host, mut cx) = setup(cx, FakeHost::with_rows(1)).await;

        panel.update(&mut cx, |panel, cx| {
            panel.select(
                PullRequestId {
                    number: 1,
                    repository: repository(),
                },
                cx,
            )
        });
        cx.executor().run_until_parked();

        // No diff is open, so there is no composer — and cancelling must still be harmless rather
        // than a panic.
        panel.update(&mut cx, |panel, cx| panel.cancel_comment(cx));
        cx.executor().run_until_parked();

        assert!(
            host.posted.borrow().is_empty(),
            "nothing may reach the host without an explicit submit"
        );
    }

    /// FR-046, SC-012: an induced failure preserves the body, and retrying posts it.
    ///
    /// Driven through the composer directly: the block placement needs a laid-out diff editor,
    /// which is the part this test is not about.
    #[gpui::test]
    async fn a_failed_post_preserves_the_body_and_retries_successfully(cx: &mut TestAppContext) {
        let (panel, host, mut cx) = setup(cx, FakeHost::with_rows(1)).await;
        let id = PullRequestId {
            number: 1,
            repository: repository(),
        };
        panel.update(&mut cx, |panel, cx| panel.select(id, cx));
        cx.executor().run_until_parked();

        let context = panel
            .update(&mut cx, |panel, _cx| panel.compose_context())
            .expect("an open pull request can be commented on");

        let target = crate::comments::CursorTarget {
            path: git::repository::RepoPath::new("a.rs").expect("a valid path"),
            side: Some(crate::host::DiffSide::New),
            line: 12,
            is_part_of_change: true,
            selection: 12..=12,
        };

        host.post_fails.set(true);
        let composer = cx
            .update(|window, cx| {
                crate::comments::Composer::open(context, &target, None, window, cx)
            })
            .expect("the composer should open");

        composer.update_in(&mut cx, |composer, window, cx| {
            composer.editor().update(cx, |editor, cx| {
                editor.set_text("Needs a comment", window, cx)
            });
            composer.submit(cx);
        });
        cx.executor().run_until_parked();

        composer.read_with(&cx, |composer, cx| {
            match composer.status() {
                crate::comments::ComposeStatus::Failed(reason) => {
                    assert!(
                        reason.contains("network"),
                        "the reason must be stated: {reason}"
                    );
                }
                other => panic!("expected a stated failure, got {other:?}"),
            }
            assert_eq!(
                composer.editor().read(cx).text(cx),
                "Needs a comment",
                "the reviewer's text must survive a failed post"
            );
        });
        assert_eq!(host.posted.borrow().len(), 1);

        // Retry, with the host cooperating this time.
        host.post_fails.set(false);
        composer.update(&mut cx, |composer, cx| composer.submit(cx));
        cx.executor().run_until_parked();

        assert_eq!(host.posted.borrow().len(), 2, "the retry sends it again");
        let posted = host.posted.borrow();
        let last = posted.last().expect("a posted draft");
        assert_eq!(last.body, "Needs a comment");
        assert_eq!(last.line, 12);
        assert_eq!(last.side, crate::host::DiffSide::New);
        assert!(
            last.reply_to.is_none(),
            "a top-level comment is not a reply"
        );
    }

    /// FR-045: an empty body never reaches the host.
    #[gpui::test]
    async fn an_empty_comment_is_refused_before_anything_is_sent(cx: &mut TestAppContext) {
        let (panel, host, mut cx) = setup(cx, FakeHost::with_rows(1)).await;
        panel.update(&mut cx, |panel, cx| {
            panel.select(
                PullRequestId {
                    number: 1,
                    repository: repository(),
                },
                cx,
            )
        });
        cx.executor().run_until_parked();

        let context = panel
            .update(&mut cx, |panel, _cx| panel.compose_context())
            .expect("an open pull request can be commented on");
        let target = crate::comments::CursorTarget {
            path: git::repository::RepoPath::new("a.rs").expect("a valid path"),
            side: Some(crate::host::DiffSide::New),
            line: 1,
            is_part_of_change: true,
            selection: 1..=1,
        };

        let composer = cx
            .update(|window, cx| {
                crate::comments::Composer::open(context, &target, None, window, cx)
            })
            .expect("the composer should open");

        composer.update_in(&mut cx, |composer, window, cx| {
            composer
                .editor()
                .update(cx, |editor, cx| editor.set_text("   \n  ", window, cx));
            assert!(!composer.can_submit(cx), "whitespace is not a comment");
            composer.submit(cx);
        });
        cx.executor().run_until_parked();

        assert!(
            host.posted.borrow().is_empty(),
            "an empty body must be refused before anything is sent"
        );
    }

    /// SC-012, the quick-succession edge case: two comments both post, neither overwriting the
    /// other.
    #[gpui::test]
    async fn two_comments_in_quick_succession_both_post(cx: &mut TestAppContext) {
        let (panel, host, mut cx) = setup(cx, FakeHost::with_rows(1)).await;
        panel.update(&mut cx, |panel, cx| {
            panel.select(
                PullRequestId {
                    number: 1,
                    repository: repository(),
                },
                cx,
            )
        });
        cx.executor().run_until_parked();

        for line in [10u32, 20] {
            let context = panel
                .update(&mut cx, |panel, _cx| panel.compose_context())
                .expect("an open pull request can be commented on");
            let target = crate::comments::CursorTarget {
                path: git::repository::RepoPath::new("a.rs").expect("a valid path"),
                side: Some(crate::host::DiffSide::New),
                line,
                is_part_of_change: true,
                selection: line..=line,
            };
            let composer = cx
                .update(|window, cx| {
                    crate::comments::Composer::open(context, &target, None, window, cx)
                })
                .expect("the composer should open");
            composer.update_in(&mut cx, |composer, window, cx| {
                composer.editor().update(cx, |editor, cx| {
                    editor.set_text(format!("Comment on {line}"), window, cx)
                });
                composer.submit(cx);
            });
        }
        cx.executor().run_until_parked();

        let posted = host.posted.borrow();
        assert_eq!(posted.len(), 2, "both comments must post");
        let lines: Vec<u32> = posted.iter().map(|draft| draft.line).collect();
        assert!(lines.contains(&10) && lines.contains(&20));
        let bodies: Vec<&str> = posted.iter().map(|draft| draft.body.as_str()).collect();
        assert!(
            bodies.contains(&"Comment on 10") && bodies.contains(&"Comment on 20"),
            "neither may overwrite the other: {bodies:?}"
        );
    }

    /// FR-047: a closed pull request cannot be commented on, and the reviewer is told before they
    /// type rather than after they submit.
    #[gpui::test]
    async fn a_closed_pull_request_offers_no_compose_context(cx: &mut TestAppContext) {
        let (panel, _host, mut cx) = setup(cx, FakeHost::with_rows(1)).await;
        let id = PullRequestId {
            number: 1,
            repository: repository(),
        };
        panel.update(&mut cx, |panel, cx| panel.select(id, cx));
        cx.executor().run_until_parked();

        // The fake host lists open pull requests, so make this one merged the way the host would.
        panel.update(&mut cx, |panel, _cx| {
            if let Load::Ready(detail) = &mut panel.detail {
                detail.summary.state = PullRequestState::Merged;
                detail.can_comment = false;
            }
        });

        let context = panel
            .update(&mut cx, |panel, _cx| panel.compose_context())
            .expect("a context is still produced; it is the flags that refuse");
        assert!(!context.is_open);
        assert!(!context.can_comment);

        let target = crate::comments::CursorTarget {
            path: git::repository::RepoPath::new("a.rs").expect("a valid path"),
            side: Some(crate::host::DiffSide::New),
            line: 1,
            is_part_of_change: true,
            selection: 1..=1,
        };
        let refusal = cx
            .update(|window, cx| {
                crate::comments::Composer::open(context, &target, None, window, cx)
            })
            .expect_err("a merged pull request must refuse before the editor opens");
        assert_eq!(
            refusal,
            crate::comments::ComposeRefusal::CommentingUnavailable {
                reason: crate::comments::UnavailableReason::NotOpen
            }
        );
    }

    /// FR-051: a reply carries the thread it belongs to.
    #[gpui::test]
    async fn a_reply_is_posted_into_its_thread(cx: &mut TestAppContext) {
        let (panel, host, mut cx) = setup(cx, FakeHost::with_rows(1)).await;
        panel.update(&mut cx, |panel, cx| {
            panel.select(
                PullRequestId {
                    number: 1,
                    repository: repository(),
                },
                cx,
            )
        });
        cx.executor().run_until_parked();

        let context = panel
            .update(&mut cx, |panel, _cx| panel.compose_context())
            .expect("an open pull request can be commented on");
        let target = crate::comments::CursorTarget {
            path: git::repository::RepoPath::new("a.rs").expect("a valid path"),
            side: Some(crate::host::DiffSide::New),
            line: 3,
            is_part_of_change: true,
            selection: 3..=3,
        };
        let composer = cx
            .update(|window, cx| {
                crate::comments::Composer::open(
                    context,
                    &target,
                    Some(CommentId("9001".into())),
                    window,
                    cx,
                )
            })
            .expect("the composer should open");

        composer.update_in(&mut cx, |composer, window, cx| {
            assert!(composer.is_reply());
            composer
                .editor()
                .update(cx, |editor, cx| editor.set_text("Agreed", window, cx));
            composer.submit(cx);
        });
        cx.executor().run_until_parked();

        let posted = host.posted.borrow();
        assert_eq!(
            posted.last().and_then(|draft| draft.reply_to.clone()),
            Some(CommentId("9001".into())),
            "the reply must join its thread rather than starting a new one"
        );
    }

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
