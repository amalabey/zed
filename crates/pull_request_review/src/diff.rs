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

use std::rc::Rc;

use git::repository::{CommitDetails, RepoPath};
use gpui::{AppContext as _, Context, Task, Window};
use project::git_store::{CommitDiff, CommitFile};
use util::ResultExt as _;

use crate::changeset::{Changeset, FileDiff, FileDiffOutcome};
use crate::changeset_pull_request::PullRequestChangeset;
use crate::host::PullRequestDetail;
use crate::panel::PullRequestPanel;

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
                        Some(path),
                        window,
                        cx,
                    )
                });

                let new_item_id = view.entity_id();
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
                new_item_id
            })
            .log_err();

        panel
            .update(cx, |panel, cx| {
                // `None` means the item was never added — the workspace was gone — so the panel
                // must forget the id it was reusing rather than keep pointing at a stale tab.
                panel.finish_diff_open(opened_item_id, cx);
            })
            .ok();
    });

    panel.hold_diff_task(task);
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
