//! Comments on the pull request: the threads already there, and the one the reviewer is writing.
//!
//! Reading and creating are deliberately asymmetric. A thread read from the host may span several
//! lines, because pull requests created elsewhere carry ranges and FR-050 requires them shown over
//! every line they cover. A comment the reviewer creates is always one line (FR-040), which
//! [`crate::host::DraftComment`] enforces in the type system rather than leaving a range for some
//! code path to remember to collapse.
//!
//! There is no resolve or reopen affordance anywhere in this file. Resolving threads is out of
//! scope (FR-054), and an action that appeared but did nothing would be worse than its absence.

use std::collections::HashMap;
use std::rc::Rc;

use buffer_diff::DiffHunkStatusKind;
use editor::Editor;
use git::repository::RepoPath;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    Styled, Task, Window,
};
use ui::{Button, Label, prelude::*};

use crate::host::{
    CommentAnchor, CommentId, CommentThread, DiffSide, DraftComment, FlatComment, Identity,
    PullRequestHost, PullRequestId, Verdict,
};

/// Why a comment cannot be composed or submitted, stated rather than guessed around.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposeRefusal {
    EmptyBody,
    AmbiguousSide,
    LineNotInChange,
    CommentingUnavailable { reason: UnavailableReason },
    PullRequestMoved { was: String, now: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    NotOpen,
    NoPermission,
}

impl ComposeRefusal {
    pub fn message(&self) -> String {
        match self {
            ComposeRefusal::EmptyBody => "Write something before submitting.".to_string(),
            ComposeRefusal::AmbiguousSide => {
                // Posting against whichever side the host happens to pick would attach the comment
                // to a line the reviewer was not looking at (FR-048).
                "Put the cursor on a line of the old or the new side — this position matches both."
                    .to_string()
            }
            ComposeRefusal::LineNotInChange => {
                "This line isn't part of what the pull request changes, so there's nowhere to \
                 attach a comment."
                    .to_string()
            }
            ComposeRefusal::CommentingUnavailable {
                reason: UnavailableReason::NotOpen,
            } => "This pull request is closed, so it can't be commented on.".to_string(),
            ComposeRefusal::CommentingUnavailable {
                reason: UnavailableReason::NoPermission,
            } => "You don't have permission to comment on this pull request.".to_string(),
            ComposeRefusal::PullRequestMoved { was, now } => format!(
                "This pull request has moved on since you started reading it ({was} → {now}). Your \
                 comment will be posted against {was}, the revision you were reading."
            ),
        }
    }
}

/// Where the cursor is, expressed in diff terms rather than editor terms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorTarget {
    pub path: RepoPath,
    /// `None` when the position sits on a line present unchanged on both sides, which does not
    /// identify one side.
    pub side: Option<DiffSide>,
    pub line: u32,
    pub is_part_of_change: bool,
    /// The lines the reviewer had selected, so the compose surface can show which one of them the
    /// comment will land on (FR-041).
    pub selection: std::ops::RangeInclusive<u32>,
}

/// Which side a row of a diff belongs to, from the row's own diff status.
///
/// A row that is part of no hunk is unchanged context: it exists identically on both sides, so it
/// does not identify one, and [`resolve_side`] refuses rather than picking. A `Modified` row is the
/// new side — the editor shows the old side of a modification as a separate deleted row.
pub fn side_for_diff_status(status: Option<DiffHunkStatusKind>) -> Option<DiffSide> {
    match status {
        Some(DiffHunkStatusKind::Deleted) => Some(DiffSide::Old),
        Some(DiffHunkStatusKind::Added) | Some(DiffHunkStatusKind::Modified) => Some(DiffSide::New),
        None => None,
    }
}

/// Which side a *split* diff's focused pane is.
///
/// In split mode the two sides are two editors, so the answer is unambiguous and does not depend on
/// the row under the cursor at all.
pub fn side_for_split_pane(is_left_pane: bool) -> DiffSide {
    if is_left_pane {
        DiffSide::Old
    } else {
        DiffSide::New
    }
}

/// Resolve exactly one side, or refuse with the reason.
///
/// Ambiguity is never resolved by picking a side. A comment posted against the wrong side lands on
/// a different line of a different file version, and the reviewer has no way to tell from the
/// pull request that it happened.
pub fn resolve_side(target: &CursorTarget) -> Result<DiffSide, ComposeRefusal> {
    if !target.is_part_of_change {
        return Err(ComposeRefusal::LineNotInChange);
    }
    target.side.ok_or(ComposeRefusal::AmbiguousSide)
}

/// Anchor a selection to one well-defined line.
///
/// The first line of the selection is chosen because it is the one the reviewer's cursor was on
/// when they began selecting, so it is the least surprising of the available choices — and the
/// compose surface shows which line before they submit (FR-041), so the choice is never hidden.
pub fn anchor_line_for_selection(selection: std::ops::RangeInclusive<u32>) -> u32 {
    (*selection.start()).max(1)
}

/// Refuse an empty or whitespace-only body **before anything is sent** (FR-045).
pub fn validate_body(body: &str) -> Result<(), ComposeRefusal> {
    if body.trim().is_empty() {
        return Err(ComposeRefusal::EmptyBody);
    }
    Ok(())
}

/// Everything that must hold before a draft is handed to the host.
///
/// Checked in one place so no submit path can skip one of them, and so the reasons are all
/// reportable in the compose surface rather than surfacing as a failed post.
pub fn validate_draft(
    draft: &DraftComment,
    can_comment: bool,
    is_open: bool,
    current_revision: &str,
) -> Result<Option<ComposeRefusal>, ComposeRefusal> {
    validate_body(&draft.body)?;

    if !is_open {
        return Err(ComposeRefusal::CommentingUnavailable {
            reason: UnavailableReason::NotOpen,
        });
    }
    if !can_comment {
        return Err(ComposeRefusal::CommentingUnavailable {
            reason: UnavailableReason::NoPermission,
        });
    }

    // A moved pull request is a *warning*, not a refusal: the comment is still posted against the
    // revision the reviewer was reading, which is the line they actually wrote about (FR-049).
    if !current_revision.is_empty() && current_revision != draft.against_revision {
        return Ok(Some(ComposeRefusal::PullRequestMoved {
            was: short_revision(&draft.against_revision),
            now: short_revision(current_revision),
        }));
    }

    Ok(None)
}

fn short_revision(revision: &str) -> String {
    revision.chars().take(12).collect()
}

/// Build threads from the host's parent links, replies oldest-first (FR-050, FR-051).
///
/// A reply whose parent is not in the response becomes a top-level thread rather than being
/// dropped: the host paginates comments, so a parent can legitimately be absent, and losing the
/// reply would lose a point already made.
pub fn assemble_threads(flat: Vec<FlatComment>) -> Vec<CommentThread> {
    let present: std::collections::HashSet<CommentId> =
        flat.iter().map(|(comment, _)| comment.id.clone()).collect();

    let mut children: HashMap<CommentId, Vec<CommentThread>> = HashMap::new();
    let mut roots: Vec<CommentThread> = Vec::new();

    for (comment, parent) in flat {
        match parent {
            Some(parent) if present.contains(&parent) && parent != comment.id => {
                children.entry(parent).or_default().push(comment);
            }
            _ => roots.push(comment),
        }
    }

    for thread in &mut roots {
        attach_replies(thread, &mut children);
    }
    roots.sort_by_key(|thread| thread.created_at);

    // Any child whose parent chain never reached a root — a cycle, or a parent that was itself
    // skipped as unreadable — would otherwise vanish. Promote what is left.
    let mut orphans: Vec<CommentThread> = children.into_values().flatten().collect();
    if !orphans.is_empty() {
        orphans.sort_by_key(|thread| thread.created_at);
        roots.extend(orphans);
        roots.sort_by_key(|thread| thread.created_at);
    }

    roots
}

fn attach_replies(
    thread: &mut CommentThread,
    children: &mut HashMap<CommentId, Vec<CommentThread>>,
) {
    let Some(mut replies) = children.remove(&thread.id) else {
        return;
    };
    replies.sort_by_key(|reply| reply.created_at);
    for reply in &mut replies {
        attach_replies(reply, children);
    }
    thread.replies = replies;
}

/// Mark a thread outdated when its anchor no longer matches the changeset being shown.
///
/// Outdated, never re-anchored and never dropped (FR-053). Moving a comment to a nearby line would
/// silently attribute it to code its author never saw; hiding it would lose a point already made.
pub fn mark_outdated(
    threads: &mut [CommentThread],
    is_anchor_current: &dyn Fn(&CommentAnchor) -> bool,
) {
    for thread in threads {
        thread.is_outdated = thread
            .anchor
            .as_ref()
            .is_some_and(|anchor| !is_anchor_current(anchor));
        mark_outdated(&mut thread.replies, is_anchor_current);
    }
}

/// Threads that belong at a line in the diff, grouped by the file they are anchored to.
pub fn anchored_by_path(threads: &[CommentThread]) -> HashMap<RepoPath, Vec<&CommentThread>> {
    let mut grouped: HashMap<RepoPath, Vec<&CommentThread>> = HashMap::new();
    for thread in threads {
        if let Some(anchor) = thread.anchor.as_ref() {
            grouped.entry(anchor.path.clone()).or_default().push(thread);
        }
    }
    for threads in grouped.values_mut() {
        threads.sort_by_key(|thread| {
            thread
                .anchor
                .as_ref()
                .map(|anchor| *anchor.lines.start())
                .unwrap_or(0)
        });
    }
    grouped
}

/// Threads with no anchor, which belong in the Overview tab rather than being dropped (FR-052).
pub fn unanchored(threads: &[CommentThread]) -> Vec<&CommentThread> {
    threads
        .iter()
        .filter(|thread| thread.anchor.is_none())
        .collect()
}

/// How a thread should read in the UI. Deliberately carries no resolved/unresolved distinction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadSummary {
    pub author: Identity,
    pub reply_count: usize,
    pub is_outdated: bool,
    pub covers: std::ops::RangeInclusive<u32>,
}

pub fn summarize(thread: &CommentThread) -> Option<ThreadSummary> {
    let anchor = thread.anchor.as_ref()?;
    Some(ThreadSummary {
        author: thread.author.clone(),
        reply_count: count_replies(thread),
        is_outdated: thread.is_outdated,
        covers: anchor.lines.clone(),
    })
}

fn count_replies(thread: &CommentThread) -> usize {
    thread.replies.len() + thread.replies.iter().map(count_replies).sum::<usize>()
}

/// A verdict's effect on how a reviewer's bubble reads. Lives here because the approval bubbles and
/// the comment threads are the two places a reviewer's position is shown, and they must agree.
pub fn verdict_label(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Approved => "Approved",
        Verdict::ChangesRequested => "Requested changes",
        Verdict::NoVerdict => "No verdict yet",
    }
}

/// How tall the compose editor may grow before it scrolls internally.
const COMPOSER_MAX_LINES: usize = 6;

/// Where a comment being composed has got to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposeStatus {
    Composing,
    Posting,
    /// The post failed. The body is still in the editor, so the reviewer can retry or cancel
    /// (FR-046).
    Failed(String),
}

/// A comment being composed, and everything the compose surface needs to show.
///
/// Nothing here is persisted. Quitting Zed with an open comment posts nothing, because there is
/// nowhere for it to have been written down (FR-004, FR-044).
pub struct Composer {
    pull_request: PullRequestId,
    draft: DraftComment,
    editor: Entity<Editor>,
    host: Rc<dyn PullRequestHost>,
    status: ComposeStatus,
    /// A stated caveat that does not block submitting — the pull request having moved on, say
    /// (FR-049).
    warning: Option<ComposeRefusal>,
    /// The lines the reviewer had selected when they invoked the action, so the surface can show
    /// which one of them the comment will land on (FR-041).
    selection: std::ops::RangeInclusive<u32>,
    /// Kept current by the owner, so a pull request that gains commits while the reviewer is
    /// typing is noticed at submit time rather than ignored (FR-049).
    current_revision: String,
    can_comment: bool,
    is_open: bool,
    _post_task: Option<Task<()>>,
}

/// Everything the composer needs to know about the pull request it is commenting on.
pub struct ComposeContext {
    pub pull_request: PullRequestId,
    pub host: Rc<dyn PullRequestHost>,
    /// The revision the reviewer is reading, which is what the comment is posted against.
    pub against_revision: String,
    pub can_comment: bool,
    pub is_open: bool,
}

impl Composer {
    /// Open a composer for a resolved target.
    ///
    /// Every reason the comment cannot be written is checked here, *before* the reviewer types
    /// anything: FR-047 requires them to be told up front rather than after submitting.
    pub fn open(
        context: ComposeContext,
        target: &CursorTarget,
        reply_to: Option<CommentId>,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<Entity<Self>, ComposeRefusal> {
        if !context.is_open {
            return Err(ComposeRefusal::CommentingUnavailable {
                reason: UnavailableReason::NotOpen,
            });
        }
        if !context.can_comment {
            return Err(ComposeRefusal::CommentingUnavailable {
                reason: UnavailableReason::NoPermission,
            });
        }

        let side = resolve_side(target)?;
        let line = anchor_line_for_selection(target.selection.clone());

        let composer = cx.new(|cx| {
            let editor = cx.new(|cx| {
                // Auto-height so the composer grows with the comment instead of scrolling inside a
                // fixed box. `multi_line` is a full-mode editor and does not size itself to its
                // content, which inside a block means an unusable one-line window.
                let mut editor = Editor::auto_height(1, COMPOSER_MAX_LINES, window, cx);
                editor.set_placeholder_text("Leave a comment", window, cx);
                editor
            });

            Self {
                pull_request: context.pull_request,
                draft: DraftComment {
                    path: target.path.clone(),
                    side,
                    line,
                    body: String::new(),
                    reply_to,
                    against_revision: context.against_revision.clone(),
                },
                editor,
                host: context.host,
                status: ComposeStatus::Composing,
                warning: None,
                selection: target.selection.clone(),
                current_revision: context.against_revision,
                can_comment: context.can_comment,
                is_open: context.is_open,
                _post_task: None,
            }
        });

        // Focus is *not* taken here. The composer's element does not exist until its block has
        // been inserted into the diff editor, and focusing a handle that is not yet in the window's
        // focus tree does nothing. The caller focuses once the block is in place.
        Ok(composer)
    }

    /// Tell the composer the pull request has moved on.
    ///
    /// The draft still targets the revision the reviewer was reading; this is what lets the surface
    /// say so before they submit (FR-049).
    pub fn set_current_revision(&mut self, revision: String, cx: &mut Context<Self>) {
        if self.current_revision == revision {
            return;
        }
        self.current_revision = revision;
        cx.notify();
    }

    pub fn editor(&self) -> &Entity<Editor> {
        &self.editor
    }

    pub fn status(&self) -> &ComposeStatus {
        &self.status
    }

    pub fn warning(&self) -> Option<&ComposeRefusal> {
        self.warning.as_ref()
    }

    pub fn path(&self) -> &RepoPath {
        &self.draft.path
    }

    pub fn side(&self) -> DiffSide {
        self.draft.side
    }

    pub fn line(&self) -> u32 {
        self.draft.line
    }

    pub fn is_reply(&self) -> bool {
        self.draft.reply_to.is_some()
    }

    /// What the surface must tell the reviewer before they submit.
    ///
    /// With a multi-line selection this names the one line the comment will land on, so where it
    /// lands is never a surprise (FR-041).
    pub fn anchor_description(&self) -> String {
        let side = match self.draft.side {
            DiffSide::Old => "the old side",
            DiffSide::New => "the new side",
        };
        if self.selection.start() == self.selection.end() {
            format!("Commenting on line {} of {side}", self.draft.line)
        } else {
            format!(
                "Commenting on line {} of {side} — one line of the {} you selected",
                self.draft.line,
                self.selection.clone().count()
            )
        }
    }

    fn body(&self, cx: &App) -> String {
        self.editor.read(cx).text(cx)
    }

    /// Submit, acknowledging within 100ms and posting off the foreground thread.
    ///
    /// The acknowledgement is the status change to [`ComposeStatus::Posting`], which happens on
    /// this call rather than when the host answers (FR-043, FR-067, FR-068).
    pub fn submit(&mut self, cx: &mut Context<Self>) {
        if self.status == ComposeStatus::Posting {
            // Two submissions of the same draft would post it twice.
            return;
        }

        self.draft.body = self.body(cx);
        match validate_draft(
            &self.draft,
            self.can_comment,
            self.is_open,
            &self.current_revision,
        ) {
            Ok(warning) => self.warning = warning,
            Err(refusal) => {
                // Refused before anything is sent (FR-045).
                self.status = ComposeStatus::Failed(refusal.message());
                cx.notify();
                return;
            }
        }

        self.status = ComposeStatus::Posting;
        cx.notify();

        let pull_request = self.pull_request.clone();
        let draft = self.draft.clone();
        let task = self.host.post_comment(&pull_request, draft, cx);
        self._post_task = Some(cx.spawn(async move |composer, cx| {
            let posted = task.await;
            composer
                .update(cx, |composer, cx| {
                    match posted {
                        Ok(thread) => {
                            composer.status = ComposeStatus::Composing;
                            cx.emit(ComposerEvent::Posted(thread));
                        }
                        Err(error) if error.is_cancelled() => {
                            composer.status = ComposeStatus::Composing;
                        }
                        Err(error) => {
                            // The body stays in the editor untouched, so retry sends exactly what
                            // the reviewer wrote (FR-046).
                            composer.status = ComposeStatus::Failed(error.message());
                        }
                    }
                    cx.notify();
                })
                .ok();
        }));
    }

    /// Discard the draft. Nothing is sent, and nothing was ever written down (FR-044).
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        // Dropping the task cancels a post already in flight rather than letting it land after the
        // reviewer said not to.
        self._post_task = None;
        cx.emit(ComposerEvent::Cancelled);
    }

    pub fn can_submit(&self, cx: &App) -> bool {
        self.status != ComposeStatus::Posting && validate_body(&self.body(cx)).is_ok()
    }
}

pub enum ComposerEvent {
    Posted(CommentThread),
    Cancelled,
}

/// One thread rendered at its line in the diff.
///
/// Collapsible so a long conversation cannot push the code off screen (FR-054). There is no resolve
/// or reopen affordance and no resolved/unresolved styling, because resolving threads is out of
/// scope — an action that appeared but did nothing would be worse than its absence.
pub struct ThreadBlock {
    thread: CommentThread,
    expanded: bool,
    reply: Option<Entity<Composer>>,
    context: Option<ThreadReplyContext>,
    _subscriptions: Vec<gpui::Subscription>,
}

/// What a reply needs in order to join its thread rather than start a new one (FR-051).
#[derive(Clone)]
pub struct ThreadReplyContext {
    pub pull_request: PullRequestId,
    pub host: Rc<dyn PullRequestHost>,
    pub against_revision: String,
    pub can_comment: bool,
    pub is_open: bool,
}

impl ThreadBlock {
    pub fn new(thread: CommentThread, context: Option<ThreadReplyContext>) -> Self {
        Self {
            thread,
            // Threads start expanded: a comment nobody sees is a point nobody answers, and the
            // reviewer can collapse it once read.
            expanded: true,
            reply: None,
            context,
            _subscriptions: Vec::new(),
        }
    }

    pub fn thread(&self) -> &CommentThread {
        &self.thread
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    fn toggle(&mut self, cx: &mut Context<Self>) {
        self.expanded = !self.expanded;
        cx.notify();
    }

    fn begin_reply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(context) = self.context.clone() else {
            return;
        };
        let Some(anchor) = self.thread.anchor.clone() else {
            return;
        };

        // The reply targets the thread's own anchor, and carries `reply_to`, so it joins the
        // conversation rather than appearing beside it (FR-051).
        let target = CursorTarget {
            path: anchor.path.clone(),
            side: Some(anchor.side),
            line: *anchor.lines.start(),
            is_part_of_change: true,
            selection: anchor.lines.clone(),
        };

        let composer = Composer::open(
            ComposeContext {
                pull_request: context.pull_request,
                host: context.host,
                against_revision: context.against_revision,
                can_comment: context.can_comment,
                is_open: context.is_open,
            },
            &target,
            Some(self.thread.id.clone()),
            window,
            cx,
        );

        match composer {
            Ok(composer) => {
                let subscription =
                    cx.subscribe(&composer, |block: &mut Self, _composer, event, cx| {
                        match event {
                            ComposerEvent::Posted(reply) => {
                                block.thread.replies.push(reply.clone());
                                block.thread.replies.sort_by_key(|reply| reply.created_at);
                                block.reply = None;
                            }
                            ComposerEvent::Cancelled => block.reply = None,
                        }
                        cx.notify();
                    });
                self._subscriptions.push(subscription);
                self.reply = Some(composer);
            }
            Err(refusal) => {
                log::info!("pull request review: cannot reply: {}", refusal.message());
            }
        }
        cx.notify();
    }

    fn render_comment(comment: &CommentThread, is_reply: bool) -> impl IntoElement {
        let body = if comment.is_deleted {
            // A withdrawn comment leaves a trace rather than a hole, so a reply to it still makes
            // sense to read.
            "(comment deleted)".to_string()
        } else {
            comment.body.clone()
        };

        v_flex()
            .when(is_reply, |this| this.pl_3())
            .gap_0p5()
            .child(
                h_flex()
                    .gap_1()
                    .child(Label::new(comment.author.label()).size(LabelSize::Small))
                    .when(comment.is_pending, |this| {
                        this.child(
                            Label::new("pending")
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        )
                    }),
            )
            .child(
                Label::new(body)
                    .size(LabelSize::Small)
                    .color(if comment.is_deleted {
                        Color::Muted
                    } else {
                        Color::Default
                    }),
            )
    }
}

impl Render for ThreadBlock {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let summary = summarize(&self.thread);
        let reply_count = summary.as_ref().map(|s| s.reply_count).unwrap_or(0);
        let covers = summary.map(|s| s.covers);
        let expanded = self.expanded;
        let can_reply = self.context.is_some() && self.reply.is_none();

        v_flex()
            // Same reason as the composer: without this the Reply button is unclickable, because
            // the diff editor underneath takes the click.
            .block_mouse_except_scroll()
            .p_1()
            .gap_1()
            .border_l_2()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().editor_subheader_background)
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        ui::Disclosure::new("thread", expanded).on_click(cx.listener(
                            |block, _event, _window, cx| {
                                block.toggle(cx);
                            },
                        )),
                    )
                    .child(
                        Label::new(self.thread.author.label())
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .when(reply_count > 0, |this| {
                        this.child(
                            Label::new(format!(
                                "{reply_count} repl{}",
                                if reply_count == 1 { "y" } else { "ies" }
                            ))
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                        )
                    })
                    // Marked outdated, never re-anchored and never dropped (FR-053).
                    .when(self.thread.is_outdated, |this| {
                        this.child(
                            Label::new("outdated")
                                .size(LabelSize::XSmall)
                                .color(Color::Warning),
                        )
                    })
                    // A range read from the pull request is shown over every line it covers
                    // (FR-050), which the header states so a collapsed thread still says where it
                    // applies.
                    .when_some(
                        covers.filter(|covers| covers.start() != covers.end()),
                        |this, covers| {
                            this.child(
                                Label::new(format!("lines {}–{}", covers.start(), covers.end()))
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted),
                            )
                        },
                    ),
            )
            .when(expanded, |this| {
                this.child(Self::render_comment(&self.thread, false))
                    .children(
                        self.thread
                            .replies
                            .iter()
                            .map(|reply| Self::render_comment(reply, true).into_any_element()),
                    )
                    .children(self.reply.clone())
                    .when(can_reply, |this| {
                        this.child(
                            Button::new("reply", "Reply")
                                .label_size(LabelSize::Small)
                                .on_click(cx.listener(|block, _event, window, cx| {
                                    block.begin_reply(window, cx);
                                })),
                        )
                    })
            })
    }
}

impl EventEmitter<ComposerEvent> for Composer {}

impl Focusable for Composer {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus_handle(cx)
    }
}

impl Render for Composer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let posting = self.status == ComposeStatus::Posting;
        let failure = match &self.status {
            ComposeStatus::Failed(reason) => Some(reason.clone()),
            _ => None,
        };
        let can_submit = self.can_submit(cx);

        v_flex()
            // The composer sits in a block *inside* the diff editor. Without this, mouse events
            // pass through to the diff editor beneath — which is read-only — so the composer
            // renders but the caret never enters it and nothing can be typed.
            .block_mouse_except_scroll()
            .track_focus(&self.editor.focus_handle(cx))
            .p_2()
            .gap_1()
            .border_1()
            .border_color(cx.theme().colors().border)
            .rounded_md()
            .bg(cx.theme().colors().editor_background)
            .child(
                Label::new(self.anchor_description())
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
            .children(self.warning.as_ref().map(|warning| {
                Label::new(warning.message())
                    .size(LabelSize::XSmall)
                    .color(Color::Warning)
            }))
            .child(self.editor.clone())
            .children(failure.map(|reason| {
                Label::new(reason)
                    .size(LabelSize::XSmall)
                    .color(Color::Error)
            }))
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new(
                            "submit-comment",
                            if posting { "Posting…" } else { "Comment" },
                        )
                        .label_size(LabelSize::Small)
                        .disabled(!can_submit)
                        .on_click(cx.listener(
                            |composer, _event, _window, cx| {
                                composer.submit(cx);
                            },
                        )),
                    )
                    .child(
                        Button::new("cancel-comment", "Cancel")
                            .label_size(LabelSize::Small)
                            .on_click(cx.listener(|composer, _event, _window, cx| {
                                composer.cancel(cx);
                            })),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_twg::{fixtures, parse_comments_flat};
    use chrono::{TimeZone as _, Utc};

    fn thread(id: &str, minute: u32, anchor: Option<CommentAnchor>) -> CommentThread {
        CommentThread {
            id: CommentId(id.into()),
            anchor,
            author: Identity {
                account_id: "712020:aaaa".into(),
                ..Default::default()
            },
            body: format!("body {id}"),
            created_at: Utc
                .with_ymd_and_hms(2026, 9, 2, 10, minute, 0)
                .single()
                .expect("a valid timestamp"),
            replies: Vec::new(),
            is_deleted: false,
            is_pending: false,
            is_outdated: false,
        }
    }

    fn anchor(path: &str, side: DiffSide, lines: std::ops::RangeInclusive<u32>) -> CommentAnchor {
        CommentAnchor {
            path: RepoPath::new(path).expect("a valid path"),
            side,
            lines,
        }
    }

    /// FR-050 – FR-052: the fixtures produce the expected thread structure and placement, with a
    /// range shown over its full span and the unanchored comment kept for Overview.
    #[test]
    fn the_fixtures_produce_the_expected_thread_structure_and_placement() {
        let flat = parse_comments_flat(fixtures::COMMENTS_MIXED).expect("must parse");
        let threads = assemble_threads(flat);

        let find = |id: &str| {
            threads
                .iter()
                .find(|thread| thread.id == CommentId(id.into()))
                .unwrap_or_else(|| panic!("{id} should be a top-level thread"))
        };

        // 9002 and 9003 are replies to 9001, so they are not top-level.
        let root = find("9001");
        assert_eq!(root.replies.len(), 2);
        assert_eq!(
            root.replies
                .iter()
                .map(|reply| reply.id.clone())
                .collect::<Vec<_>>(),
            vec![CommentId("9002".into()), CommentId("9003".into())],
            "replies must be oldest-first"
        );
        assert!(
            !threads
                .iter()
                .any(|thread| thread.id == CommentId("9002".into())),
            "a reply must not also appear as a top-level thread"
        );

        // The range is preserved over every line it covers.
        let range = find("9004");
        let covers = summarize(range).expect("9004 is anchored").covers;
        assert_eq!(covers, 231..=239);

        // The unanchored comment belongs in Overview.
        let overview = unanchored(&threads);
        assert!(
            overview
                .iter()
                .any(|thread| thread.id == CommentId("9006".into())),
            "an unanchored comment must be kept for the Overview tab"
        );

        // A reply whose parent is not in this page is promoted rather than dropped.
        assert!(
            threads
                .iter()
                .any(|thread| thread.id == CommentId("9009".into())),
            "a reply with an absent parent must not be lost"
        );

        // Placement groups by file.
        let grouped = anchored_by_path(&threads);
        let panel_threads = grouped
            .get(&RepoPath::new("crates/git_ui/src/git_panel.rs").expect("a valid path"))
            .expect("that file carries threads");
        assert_eq!(panel_threads.len(), 2, "9001 and the pending 9008");
    }

    #[test]
    fn a_cycle_in_the_parent_links_cannot_lose_a_comment() {
        let mut first = thread("a", 0, None);
        let mut second = thread("b", 1, None);
        first.replies.clear();
        second.replies.clear();
        let flat = vec![
            (first, Some(CommentId("b".into()))),
            (second, Some(CommentId("a".into()))),
        ];
        let threads = assemble_threads(flat);
        let ids = |threads: &[CommentThread]| {
            let mut ids = Vec::new();
            fn walk(threads: &[CommentThread], ids: &mut Vec<String>) {
                for thread in threads {
                    ids.push(thread.id.0.clone());
                    walk(&thread.replies, ids);
                }
            }
            walk(threads, &mut ids);
            ids
        };
        let mut found = ids(&threads);
        found.sort();
        assert_eq!(found, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn a_comment_that_is_its_own_parent_is_not_an_infinite_thread() {
        let flat = vec![(thread("self", 0, None), Some(CommentId("self".into())))];
        let threads = assemble_threads(flat);
        assert_eq!(threads.len(), 1);
        assert!(threads[0].replies.is_empty());
    }

    /// FR-053: outdated, never re-anchored and never dropped.
    #[test]
    fn a_thread_on_a_line_the_change_no_longer_touches_is_marked_not_moved() {
        let current = anchor("a.rs", DiffSide::New, 10..=10);
        let stale = anchor("a.rs", DiffSide::New, 999..=999);
        let mut threads = vec![
            thread("current", 0, Some(current.clone())),
            thread("stale", 1, Some(stale.clone())),
        ];
        threads[0].replies = vec![thread("reply", 2, Some(stale))];

        mark_outdated(&mut threads, &|anchor| anchor == &current);

        assert!(!threads[0].is_outdated);
        assert!(threads[1].is_outdated);
        assert!(
            threads[0].replies[0].is_outdated,
            "replies are marked too, so a stale reply does not read as current"
        );
        assert_eq!(
            threads[1]
                .anchor
                .as_ref()
                .map(|anchor| anchor.lines.clone()),
            Some(999..=999),
            "the anchor must not be rewritten"
        );
        assert_eq!(threads.len(), 2, "nothing is dropped");
    }

    #[test]
    fn an_unanchored_thread_is_never_marked_outdated() {
        let mut threads = vec![thread("general", 0, None)];
        mark_outdated(&mut threads, &|_| false);
        assert!(!threads[0].is_outdated);
    }

    /// FR-045: refused before anything is sent.
    #[test]
    fn an_empty_or_whitespace_body_is_refused_before_anything_is_sent() {
        for body in ["", " ", "\n", "\t  \n ", "\u{a0}"] {
            assert_eq!(
                validate_body(body),
                Err(ComposeRefusal::EmptyBody),
                "{body:?}"
            );
        }
        assert_eq!(validate_body("Looks good"), Ok(()));
    }

    /// FR-048: never posted against whichever side the host happens to pick.
    #[test]
    fn an_ambiguous_cursor_position_is_refused_with_the_reason() {
        let ambiguous = CursorTarget {
            path: RepoPath::new("a.rs").expect("a valid path"),
            side: None,
            line: 10,
            is_part_of_change: true,
            selection: 10..=10,
        };
        assert_eq!(resolve_side(&ambiguous), Err(ComposeRefusal::AmbiguousSide));
        assert!(!ambiguous.side.is_some());

        let unchanged = CursorTarget {
            side: Some(DiffSide::New),
            is_part_of_change: false,
            ..ambiguous.clone()
        };
        assert_eq!(
            resolve_side(&unchanged),
            Err(ComposeRefusal::LineNotInChange)
        );

        let resolved = CursorTarget {
            side: Some(DiffSide::Old),
            is_part_of_change: true,
            ..ambiguous
        };
        assert_eq!(resolve_side(&resolved), Ok(DiffSide::Old));
    }

    /// FR-041: a multi-line selection anchors to one well-defined line, and no range is attempted.
    #[test]
    fn a_multi_line_selection_anchors_to_one_line() {
        assert_eq!(anchor_line_for_selection(12..=40), 12);
        assert_eq!(anchor_line_for_selection(7..=7), 7);
        assert_eq!(anchor_line_for_selection(0..=5), 1, "lines are 1-based");
    }

    fn draft(body: &str, against: &str) -> DraftComment {
        DraftComment {
            path: RepoPath::new("a.rs").expect("a valid path"),
            side: DiffSide::New,
            line: 10,
            body: body.into(),
            reply_to: None,
            against_revision: against.into(),
        }
    }

    /// FR-047: told before composing, not after submitting.
    #[test]
    fn commenting_on_a_closed_or_unpermitted_pull_request_is_refused_with_the_reason() {
        assert_eq!(
            validate_draft(&draft("hi", "abc"), true, false, "abc"),
            Err(ComposeRefusal::CommentingUnavailable {
                reason: UnavailableReason::NotOpen
            })
        );
        assert_eq!(
            validate_draft(&draft("hi", "abc"), false, true, "abc"),
            Err(ComposeRefusal::CommentingUnavailable {
                reason: UnavailableReason::NoPermission
            })
        );
        assert_eq!(
            validate_draft(&draft("hi", "abc"), true, true, "abc"),
            Ok(None)
        );
    }

    /// FR-049: never silently attached to a line it was not written about.
    #[test]
    fn a_pull_request_that_moved_warns_rather_than_repointing_the_comment() {
        let draft = draft("hi", "aaaaaaaaaaaa");
        let outcome = validate_draft(&draft, true, true, "bbbbbbbbbbbb")
            .expect("a moved pull request is a warning, not a refusal");
        match outcome {
            Some(ComposeRefusal::PullRequestMoved { was, now }) => {
                assert_eq!(was, "aaaaaaaaaaaa");
                assert_eq!(now, "bbbbbbbbbbbb");
            }
            other => panic!("expected a moved warning, got {other:?}"),
        }
        assert_eq!(
            draft.against_revision, "aaaaaaaaaaaa",
            "the draft still targets the revision the reviewer was reading"
        );
    }

    #[test]
    fn every_refusal_states_a_reason_the_reviewer_can_act_on() {
        for refusal in [
            ComposeRefusal::EmptyBody,
            ComposeRefusal::AmbiguousSide,
            ComposeRefusal::LineNotInChange,
            ComposeRefusal::CommentingUnavailable {
                reason: UnavailableReason::NotOpen,
            },
            ComposeRefusal::CommentingUnavailable {
                reason: UnavailableReason::NoPermission,
            },
            ComposeRefusal::PullRequestMoved {
                was: "a".into(),
                now: "b".into(),
            },
        ] {
            assert!(!refusal.message().is_empty());
        }
    }

    /// FR-048: which side a row belongs to, and when it identifies neither.
    #[test]
    fn a_rows_diff_status_decides_its_side_and_context_decides_nothing() {
        assert_eq!(
            side_for_diff_status(Some(DiffHunkStatusKind::Deleted)),
            Some(DiffSide::Old)
        );
        assert_eq!(
            side_for_diff_status(Some(DiffHunkStatusKind::Added)),
            Some(DiffSide::New)
        );
        // The editor shows the old side of a modification as its own deleted row, so a modified
        // row is the new side.
        assert_eq!(
            side_for_diff_status(Some(DiffHunkStatusKind::Modified)),
            Some(DiffSide::New)
        );
        // Unchanged context exists identically on both sides, so it identifies neither and must be
        // refused rather than guessed at.
        assert_eq!(side_for_diff_status(None), None);

        // In split mode the panes answer outright.
        assert_eq!(side_for_split_pane(true), DiffSide::Old);
        assert_eq!(side_for_split_pane(false), DiffSide::New);
    }

    fn target(side: Option<DiffSide>, selection: std::ops::RangeInclusive<u32>) -> CursorTarget {
        CursorTarget {
            path: RepoPath::new("a.rs").expect("a valid path"),
            side,
            line: *selection.start(),
            is_part_of_change: true,
            selection,
        }
    }

    /// FR-041: the reviewer is told which line, before they submit, and no range is attempted.
    #[test]
    fn a_multi_line_selection_is_described_by_the_one_line_it_lands_on() {
        let selected = target(Some(DiffSide::New), 12..=40);
        assert_eq!(anchor_line_for_selection(selected.selection), 12);

        // The description has to name the line *and* say it was one of several, or the reviewer
        // has no way to know the selection was narrowed.
        let single = target(Some(DiffSide::New), 7..=7);
        assert_eq!(anchor_line_for_selection(single.selection), 7);
    }

    /// FR-054, quickstart Scenario 6: resolving is out of scope, so nothing here may hint at it.
    ///
    /// Only the non-test half of the file is read — this module names the forbidden words in order
    /// to forbid them, and a check that fails on its own text checks nothing.
    #[test]
    fn nothing_in_this_module_offers_to_resolve_a_thread() {
        for (number, line) in crate::production_code_lines(include_str!("comments.rs")) {
            let lowered = line.to_lowercase();
            for forbidden in ["resolve_thread", "is_resolved", "unresolved", "reopen"] {
                assert!(
                    !lowered.contains(forbidden),
                    "comments.rs:{number} mentions {forbidden}, which would imply an affordance \
                     FR-054 puts out of scope: {}",
                    line.trim()
                );
            }
        }
    }
}
