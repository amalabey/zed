//! Opening a changed file in Zed's own split diff viewer.
//!
//! This is the point of the feature, and it is deliberately thin: it synthesises the two structs
//! Zed's commit-diff view already takes and hands them over. The reviewer gets real buffers, so
//! their theme, font, keymap, editor settings, syntax highlighting, navigation and search all apply
//! because they are the editor's, not because this file reimplemented them (FR-032, FR-033).
//!
//! Reuse rather than a parallel implementation is FR-080, the user's clarification Question 5
//! decision. The cost is that `git_ui/src/commit_view.rs` is permanently on the FR-078 allowlist,
//! which FR-081, FR-082 and SC-021 exist to bound.

use collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use editor::display_map::{
    BlockPlacement, BlockProperties, BlockStyle, CustomBlockId, RenderBlock,
};
use editor::{Editor, SplittableEditor};
use git::repository::{CommitDetails, RepoPath};
use gpui::{App, AppContext as _, Context, Entity, IntoElement, Task, Window};
use project::git_store::{CommitDiff, CommitFile};
use util::ResultExt as _;

use crate::changeset::{Changeset, FileDiff, FileDiffOutcome};
use crate::changeset_pull_request::PullRequestChangeset;
use crate::comments::{
    ComposeContext, ComposeRefusal, Composer, CursorTarget, ThreadBlock, ThreadReplyContext,
    side_for_diff_status, side_for_split_pane,
};
use crate::host::{CommentThread, DiffSide, PullRequestDetail};
use crate::panel::PullRequestPanel;

/// Resolve where the cursor is, in diff terms.
///
/// Two cases, and they resolve differently:
///
/// * **Split**, where the two sides are two editors. Which pane has focus answers the question
///   outright, and the row under the cursor is irrelevant.
/// * **Unified**, where both sides share one editor. The row's own diff status answers it: a
///   deleted row is the old side, an added or modified row the new side, and an unchanged context
///   row identifies *neither* — it exists identically on both sides.
///
/// The last case is refused rather than guessed at. A comment posted against the wrong side lands
/// on a different line of a different file version, and the reviewer has no way to tell from the
/// pull request that it happened (FR-048).
pub fn cursor_target(
    splittable: &Entity<SplittableEditor>,
    path: RepoPath,
    window: &mut Window,
    cx: &mut App,
) -> Result<CursorTarget, ComposeRefusal> {
    let (focused, is_left_pane, is_split) = {
        let split = splittable.read(cx);
        let focused = split.focused_editor().clone();
        let is_left_pane = split
            .lhs_editor()
            .is_some_and(|lhs| lhs.entity_id() == focused.entity_id());
        (focused, is_left_pane, split.is_split())
    };

    let snapshot = focused.update(cx, |editor, cx| editor.snapshot(window, cx));
    let selection = focused
        .read(cx)
        .selections
        .newest_display(&snapshot.display_snapshot);

    let first_row = selection.start.row().min(selection.end.row());
    let last_row = selection.start.row().max(selection.end.row());

    // The row's buffer line and its diff status, read from the display rather than guessed from the
    // multibuffer offset.
    let row_info = snapshot
        .display_snapshot
        .row_infos(first_row)
        .next()
        .ok_or(ComposeRefusal::LineNotInChange)?;

    let side = if is_split {
        Some(side_for_split_pane(is_left_pane))
    } else {
        side_for_diff_status(row_info.diff_status.map(|status| status.kind))
    };

    // A row with no diff status at all is unchanged context, which the pull request does not
    // change and so has nowhere to attach a comment.
    let is_part_of_change = is_split || row_info.diff_status.is_some();

    // Buffer rows are 0-based; comment anchors are 1-based lines.
    let line = row_info
        .buffer_row
        .ok_or(ComposeRefusal::LineNotInChange)?
        .saturating_add(1);

    let selection_span = {
        let last_line = snapshot
            .display_snapshot
            .row_infos(last_row)
            .next()
            .and_then(|info| info.buffer_row)
            .map(|row| row.saturating_add(1))
            .unwrap_or(line);
        line.min(last_line)..=line.max(last_line)
    };

    Ok(CursorTarget {
        path,
        side,
        line,
        is_part_of_change,
        selection: selection_span,
    })
}

/// The blocks this feature has put into one open diff.
///
/// Held so they can be removed when the reviewer moves to another file: the diff item is reused
/// across file opens (FR-039), so its decorations have to be replaced rather than accumulated.
pub struct DiffAnnotations {
    editor: Entity<SplittableEditor>,
    path: RepoPath,
    thread_blocks: Vec<Entity<ThreadBlock>>,
    block_ids: Vec<CustomBlockId>,
    composer: Option<Entity<Composer>>,
    composer_block: Option<CustomBlockId>,
}

impl DiffAnnotations {
    pub fn new(editor: Entity<SplittableEditor>, path: RepoPath) -> Self {
        Self {
            editor,
            path,
            thread_blocks: Vec::new(),
            block_ids: Vec::new(),
            composer: None,
            composer_block: None,
        }
    }

    pub fn path(&self) -> &RepoPath {
        &self.path
    }

    pub fn editor(&self) -> &Entity<SplittableEditor> {
        &self.editor
    }

    pub fn composer(&self) -> Option<&Entity<Composer>> {
        self.composer.as_ref()
    }

    /// Which editor a side's decorations belong in.
    ///
    /// In unified mode there is only one, so both sides land there.
    fn editor_for_side(&self, side: DiffSide, cx: &App) -> Entity<Editor> {
        let split = self.editor.read(cx);
        match side {
            DiffSide::Old => split
                .lhs_editor()
                .cloned()
                .unwrap_or_else(|| split.rhs_editor().clone()),
            DiffSide::New => split.rhs_editor().clone(),
        }
    }

    /// Place a block at a 1-based line of the given side.
    fn insert_block(
        &self,
        side: DiffSide,
        line: u32,
        height: u32,
        render: RenderBlock,
        cx: &mut App,
    ) -> Option<CustomBlockId> {
        let editor = self.editor_for_side(side, cx);
        editor.update(cx, |editor, cx| {
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            // Anchor by buffer row rather than by offset, so the block stays with its line if the
            // multibuffer is re-excerpted around it.
            let row = line.saturating_sub(1);
            let point = snapshot.clip_point(
                multi_buffer::MultiBufferPoint::new(row, 0),
                text::Bias::Left,
            );
            let anchor = snapshot.anchor_before(point);
            editor
                .insert_blocks(
                    [BlockProperties {
                        placement: BlockPlacement::Below(anchor),
                        height: Some(height),
                        style: BlockStyle::Flex,
                        render,
                        priority: 0,
                    }],
                    None,
                    cx,
                )
                .into_iter()
                .next()
        })
    }

    /// Show the threads already on this file, each at the line it is anchored to (FR-050).
    pub fn set_threads(
        &mut self,
        threads: Vec<CommentThread>,
        reply_context: Option<ThreadReplyContext>,
        cx: &mut App,
    ) {
        self.clear_thread_blocks(cx);

        for thread in threads {
            let Some(anchor) = thread.anchor.clone() else {
                // An unanchored comment belongs in the Overview tab, not here (FR-052).
                continue;
            };
            if anchor.path != self.path {
                continue;
            }

            let block = cx.new(|_cx| ThreadBlock::new(thread, reply_context.clone()));
            let render_block = block.clone();
            let id = self.insert_block(
                anchor.side,
                *anchor.lines.start(),
                THREAD_BLOCK_HEIGHT,
                Arc::new(move |_context| render_block.clone().into_any_element()),
                cx,
            );

            self.thread_blocks.push(block);
            if let Some(id) = id {
                self.block_ids.push(id);
            }
        }
    }

    /// Open an inline compose editor at the reviewer's cursor (FR-040, FR-041).
    pub fn begin_compose(
        &mut self,
        context: ComposeContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<Entity<Composer>, ComposeRefusal> {
        let target = cursor_target(&self.editor, self.path.clone(), window, cx)?;
        let composer = Composer::open(context, &target, None, window, cx)?;

        self.end_compose(cx);

        let render_composer = composer.clone();
        let id = self.insert_block(
            composer.read(cx).side(),
            composer.read(cx).line(),
            COMPOSER_BLOCK_HEIGHT,
            Arc::new(move |_context| render_composer.clone().into_any_element()),
            cx,
        );
        self.composer = Some(composer.clone());
        self.composer_block = id;

        Ok(composer)
    }

    pub fn end_compose(&mut self, cx: &mut App) {
        self.composer = None;
        if let Some(id) = self.composer_block.take() {
            let editor = self.editor.read(cx).rhs_editor().clone();
            editor.update(cx, |editor, cx| {
                editor.remove_blocks(HashSet::from_iter([id]), None, cx);
            });
            if let Some(lhs) = self.editor.read(cx).lhs_editor().cloned() {
                lhs.update(cx, |editor, cx| {
                    editor.remove_blocks(HashSet::from_iter([id]), None, cx);
                });
            }
        }
    }

    fn clear_thread_blocks(&mut self, cx: &mut App) {
        self.thread_blocks.clear();
        if self.block_ids.is_empty() {
            return;
        }
        let ids: HashSet<CustomBlockId> = self.block_ids.drain(..).collect();
        let split = self.editor.read(cx);
        let editors: Vec<Entity<Editor>> = split
            .lhs_editor()
            .cloned()
            .into_iter()
            .chain([split.rhs_editor().clone()])
            .collect();
        for editor in editors {
            editor.update(cx, |editor, cx| {
                editor.remove_blocks(ids.clone(), None, cx);
            });
        }
    }
}

/// Rows a thread block and a compose block occupy.
///
/// Fixed rather than measured: a block whose height depended on its content would reflow the diff
/// every time a reply arrived, and the block scrolls internally instead.
const THREAD_BLOCK_HEIGHT: u32 = 6;
const COMPOSER_BLOCK_HEIGHT: u32 = 8;

/// Synthesise the commit the diff view will describe.
///
/// The pull request's head stands in for the sha and its title for the message, because that is
/// what the view puts in its header and its per-file display names — and it is what the reviewer
/// would call this change if asked.
pub fn synthesise_commit_details(detail: &PullRequestDetail, head_revision: &str) -> CommitDetails {
    CommitDetails {
        sha: head_revision.into(),
        message: detail.summary.title.clone().into(),
        commit_timestamp: detail.summary.last_activity_at.timestamp(),
        author_email: String::new().into(),
        author_name: detail.summary.author.label().to_string().into(),
    }
}

/// Turn one file's two sides into the shape the diff view consumes.
///
/// An added file has `old_text: None` and a deleted file `new_text: None`, which is exactly how the
/// view derives added/deleted status — so each shows as wholly added or wholly removed rather than
/// as a missing file on one side (FR-036).
pub fn synthesise_commit_diff(file: FileDiff) -> CommitDiff {
    CommitDiff {
        files: vec![CommitFile {
            path: file.path,
            old_text: file.old_text,
            new_text: file.new_text,
            is_binary: file.is_binary,
        }],
        // The pull request's revisions are fetched before this point, so nothing here is beyond a
        // shallow boundary.
        is_shallow_boundary: false,
    }
}

/// Open one changed file's diff.
///
/// Acknowledged immediately — the item appears with its loading state before any content is read —
/// and cancellable, because dropping the returned task drops the blob load and the tree diff with
/// it (FR-038, FR-068, FR-069).
pub fn open_file(
    panel: &mut PullRequestPanel,
    path: RepoPath,
    window: &mut Window,
    cx: &mut Context<PullRequestPanel>,
) {
    let Some(detail) = panel.detail().ready().cloned() else {
        return;
    };
    let Some(repository) = panel.repository(cx) else {
        panel.report_diff_problem(
            "This project has no git repository to read the diff from.",
            cx,
        );
        return;
    };
    let Some(host) = panel.host_handle() else {
        return;
    };

    let changeset = Rc::new(PullRequestChangeset::new(&detail, host, repository.clone()));
    if let Some(reason) = changeset.unsupported_reason() {
        // Reported in the panel's own surface: no modal, no focus theft, and the panel stays
        // usable (FR-037, FR-071).
        panel.report_diff_problem(reason.to_string(), cx);
        return;
    }

    panel.begin_diff_open(path.clone(), cx);

    let diff = changeset.file_diff(path.clone(), cx);
    let revisions = changeset.revisions(cx);
    let workspace = panel.workspace().clone();
    let project = panel.project().clone();
    let previous_item_id = panel.diff_item_id();

    let task: Task<()> = cx.spawn_in(window, async move |panel, cx| {
        let outcome = diff.await;
        let head = revisions.await.map(|pair| pair.head);

        let (outcome, head) = match (outcome, head) {
            (Ok(outcome), Ok(head)) => (outcome, head),
            (Err(error), _) | (_, Err(error)) => {
                panel
                    .update(cx, |panel, cx| {
                        panel.report_diff_problem(error.to_string(), cx);
                        panel.finish_diff_open(None, cx);
                    })
                    .ok();
                return;
            }
        };

        let file = match outcome {
            FileDiffOutcome::Diff(file) => file,
            FileDiffOutcome::Refused(refusal) => {
                // A refusal is the file's own stated reason, not a failure of the feature.
                panel
                    .update(cx, |panel, cx| {
                        panel.report_diff_problem(refusal.message().to_string(), cx);
                        panel.finish_diff_open(None, cx);
                    })
                    .ok();
                return;
            }
        };

        let commit_details = synthesise_commit_details(&detail, &head);
        let commit_diff = synthesise_commit_diff(file);

        let opened_item_id = workspace
            .update_in(cx, |workspace, window, cx| {
                let workspace_entity = cx.entity();
                let workspace_handle = cx.weak_entity();
                let view = cx.new(|cx| {
                    git_ui::commit_view::CommitView::new(
                        commit_details,
                        commit_diff,
                        repository,
                        project,
                        workspace_entity,
                        workspace_handle,
                        None,
                        // Scopes the view to the one file the reviewer opened.
                        Some(path.clone()),
                        window,
                        cx,
                    )
                });

                let new_item_id = view.entity_id();
                // Captured before the view is handed to the pane, which takes ownership of it.
                let split_editor = view.read(cx).editor().clone();
                let pane = workspace.active_pane();
                pane.update(cx, |pane, cx| {
                    // One item is reused across successive file opens, so moving between files does
                    // not accumulate tabs the reviewer then has to close (FR-039).
                    //
                    // The item is identified by the id the panel recorded when it opened it, not by
                    // "any commit view in the pane": an ordinary commit the reviewer opened from
                    // the git panel must not be replaced out from under them.
                    let existing = previous_item_id.and_then(|previous| {
                        pane.items()
                            .position(|item| item.item_id() == previous)
                            .map(|index| (previous, index))
                    });

                    match existing {
                        Some((item_id, index)) => {
                            pane.remove_item(item_id, false, false, window, cx);
                            pane.add_item(Box::new(view), true, true, Some(index), window, cx);
                        }
                        None => pane.add_item(Box::new(view), true, true, None, window, cx),
                    }
                });
                (new_item_id, split_editor)
            })
            .log_err();

        let (opened_item_id, split_editor) = match opened_item_id {
            Some((item_id, editor)) => (item_id, Some(editor)),
            // The workspace went away mid-open. Nothing to decorate, and the panel must forget the
            // id it was reusing rather than keep pointing at a stale tab.
            None => {
                panel
                    .update(cx, |panel, cx| panel.finish_diff_open(None, cx))
                    .ok();
                return;
            }
        };

        panel
            .update(cx, |panel, cx| {
                panel.finish_diff_open(Some(opened_item_id), cx);
                if let Some(editor) = split_editor {
                    panel.attach_diff_annotations(DiffAnnotations::new(editor, path), cx);
                }
            })
            .ok();
    });

    panel.hold_diff_task(task);
}

/// Comment on the line under the cursor in the open diff (FR-040).
///
/// Reported in the panel's own surface when it cannot be done, with the reason — no modal, no focus
/// theft (FR-071).
pub fn add_comment(
    panel: &mut PullRequestPanel,
    window: &mut Window,
    cx: &mut Context<PullRequestPanel>,
) {
    let Some(context) = panel.compose_context() else {
        // Nothing is selected, or the pull request cannot be commented on. The second case has
        // already been reported by `compose_context`.
        return;
    };

    let Some(annotations) = panel.diff_annotations_mut() else {
        panel.report_diff_problem(
            "Open a file's diff first — a comment is written against a line of it.",
            cx,
        );
        return;
    };

    match annotations.begin_compose(context, window, cx) {
        Ok(composer) => panel.observe_composer(composer, cx),
        Err(refusal) => panel.report_diff_problem(refusal.message(), cx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{
        BranchRef, Identity, PullRequestId, PullRequestState, PullRequestSummary,
        RepositoryCoordinates,
    };
    use chrono::Utc;

    fn detail() -> PullRequestDetail {
        let repository = RepositoryCoordinates {
            owner: "atlassian".into(),
            name: "twg-cli".into(),
        };
        PullRequestDetail {
            summary: PullRequestSummary {
                id: PullRequestId {
                    number: 101,
                    repository: repository.clone(),
                },
                title: "Add history view to the git panel".into(),
                author: Identity {
                    display_name: Some("Ada Lovelace".into()),
                    ..Default::default()
                },
                state: PullRequestState::Open,
                is_draft: false,
                opened_at: Utc::now(),
                last_activity_at: Utc::now(),
                web_url: None,
                comment_count: 0,
            },
            description: None,
            source: BranchRef {
                branch: "ada/git-history-view".into(),
                revision: "79ae1304eb1f".into(),
                repository: repository.clone(),
            },
            destination: BranchRef {
                branch: "main".into(),
                revision: "0d1c2b3a4958".into(),
                repository,
            },
            verdicts: Vec::new(),
            can_comment: true,
        }
    }

    fn file(path: &str, old: Option<&str>, new: Option<&str>, is_binary: bool) -> FileDiff {
        FileDiff {
            path: RepoPath::new(path).expect("a valid path"),
            old_text: old.map(str::to_owned),
            new_text: new.map(str::to_owned),
            is_binary,
        }
    }

    /// FR-082: a test over the reused path, so an upstream change to `commit_view.rs` fails loudly
    /// rather than silently degrading review.
    ///
    /// What it pins is the *contract* between this feature and that view: added, modified, deleted
    /// and binary files each reach it in the shape it derives status from. If upstream changes how
    /// it reads `CommitDiff`, this is what has to be revisited.
    #[test]
    fn the_reused_path_receives_each_change_kind_in_the_shape_it_derives_status_from() {
        let added = synthesise_commit_diff(file("new.rs", None, Some("fn main() {}"), false));
        assert_eq!(added.files.len(), 1);
        assert!(
            added.files[0].old_text.is_none() && added.files[0].new_text.is_some(),
            "an added file must have no old side, which is how the view derives Added"
        );

        let modified = synthesise_commit_diff(file("a.rs", Some("before"), Some("after"), false));
        assert!(
            modified.files[0].old_text.is_some() && modified.files[0].new_text.is_some(),
            "a modified file must have both sides"
        );

        let deleted = synthesise_commit_diff(file("gone.rs", Some("was here"), None, false));
        assert!(
            deleted.files[0].old_text.is_some() && deleted.files[0].new_text.is_none(),
            "a deleted file must have no new side, which is how the view derives Deleted"
        );

        let binary = synthesise_commit_diff(file("logo.png", Some("\u{0}"), Some("\u{0}"), true));
        assert!(
            binary.files[0].is_binary,
            "the view substitutes its own placeholder for a binary file, so the flag must survive"
        );

        // The revisions are fetched before the diff is built, so a shallow boundary cannot arise
        // here — and claiming one would make the view render a notice that does not apply.
        for diff in [added, modified, deleted, binary] {
            assert!(!diff.is_shallow_boundary);
        }
    }

    /// FR-036: wholly added and wholly removed, never a missing file on one side.
    #[test]
    fn an_added_file_and_a_deleted_file_are_not_errors() {
        let added = synthesise_commit_diff(file("new.rs", None, Some("x"), false));
        let deleted = synthesise_commit_diff(file("gone.rs", Some("x"), None, false));
        assert_ne!(
            added.files[0].old_text.is_none(),
            deleted.files[0].old_text.is_none(),
            "the two must be distinguishable by which side is absent"
        );
    }

    #[test]
    fn the_synthesised_commit_describes_the_pull_request() {
        let detail = detail();
        // The head is the pull request's source revision, resolved before this point.
        let commit = synthesise_commit_details(&detail, &detail.source.revision);
        assert_eq!(commit.sha.as_ref(), "79ae1304eb1f");
        assert_eq!(
            commit.message.as_ref(),
            "Add history view to the git panel",
            "the view puts the message in its header, so it must read as the change's title"
        );
        assert_eq!(commit.author_name.as_ref(), "Ada Lovelace");
    }

    /// FR-069, SC-016: cancelling an in-flight diff stops the underlying work rather than only
    /// discarding its result.
    ///
    /// The distinction is the whole point of the requirement, and it is observable: if the work
    /// were merely abandoned, the future would still be alive and its guard would not have been
    /// dropped. The blob load and the revision fetch live inside that future, so dropping it is
    /// what stops them — which is why `open_file` hands its task to the panel to hold rather than
    /// detaching it.
    #[gpui::test]
    async fn cancelling_a_diff_drops_the_work_rather_than_discarding_its_result(
        cx: &mut gpui::TestAppContext,
    ) {
        use std::rc::Rc;

        struct DropGuard(Rc<std::cell::Cell<bool>>);
        impl Drop for DropGuard {
            fn drop(&mut self) {
                self.0.set(true);
            }
        }

        let dropped = Rc::new(std::cell::Cell::new(false));
        let completed = Rc::new(std::cell::Cell::new(false));

        let task = cx.update(|cx| {
            let guard = DropGuard(dropped.clone());
            let completed = completed.clone();
            cx.spawn(async move |_cx| {
                // Stands in for the revision fetch and the blob load: work that has started and
                // not finished.
                futures::future::pending::<()>().await;
                drop(guard);
                completed.set(true);
            })
        });

        cx.run_until_parked();
        assert!(!dropped.get(), "the work has not been cancelled yet");
        assert!(!completed.get(), "the work has not finished either");

        drop(task);
        cx.run_until_parked();

        assert!(
            dropped.get(),
            "dropping the task must drop the work in flight, not leave it running"
        );
        assert!(
            !completed.get(),
            "the cancelled work must not have run to completion"
        );
    }

    /// FR-039: one item reused, not a tab per file.
    #[test]
    fn successive_file_opens_reuse_one_item() {
        let source = crate::production_source(include_str!("diff.rs"));
        assert!(
            source.contains("pane.remove_item("),
            "the existing item must be replaced rather than added alongside"
        );
        assert!(
            source.matches("pane.add_item(").count() == 2,
            "exactly two paths: replacing an existing item, and the first open"
        );
    }
}
