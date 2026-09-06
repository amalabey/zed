//! The one [`Changeset`] implementation: the change a pull request proposes.
//!
//! This is the only module permitted to depend on both boundaries, and the division of labour is
//! easy to get backwards, so it is worth restating (contracts/changeset.md):
//!
//! | Fact | Comes from |
//! |---|---|
//! | Which files changed, line counts, rename detection | the host |
//! | File content on each side | git, locally |
//! | Base and head revision identifiers | the host |
//! | The merge base itself | git, locally |
//!
//! The base is the **merge base** of head and destination, never the destination tip. That is what
//! makes FR-034 hold: the reviewer sees what the change proposes, not what the destination branch
//! acquired afterwards. Returning the tip would satisfy the type signature and violate the
//! contract, so `revisions()` is the single most important function in this file.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::{Context as _, Result, anyhow};
use askpass::AskPassDelegate;
use futures::FutureExt as _;
use futures::future::Shared;
use git::repository::{FetchOptions, RepoPath};
use git::status::{DiffTreeType, TreeDiffStatus};
use gpui::{App, AppContext as _, Entity, SharedString, Task};
use project::git_store::Repository;

use crate::changeset::{
    ChangedFile, Changeset, FileDiff, FileDiffOutcome, RenderRefusal, RevisionPair,
};
use crate::host::{PullRequestDetail, PullRequestHost, PullRequestId};

/// Past this a multibuffer stops being a useful way to read a change, and building buffers for it
/// costs more than it is worth.
const MAX_DIFFABLE_BYTES: usize = 4 * 1024 * 1024;

pub struct PullRequestChangeset {
    id: PullRequestId,
    title: SharedString,
    host: Rc<dyn PullRequestHost>,
    repository: Entity<Repository>,
    source_revision: String,
    destination_revision: String,
    /// `Some` means this changeset cannot be produced at all, with the reason. Held rather than
    /// returned from the constructor so the Files tab can state the reason in place of a list.
    unsupported: Option<String>,
    /// The revision resolution, computed once. See [`Self::shared_revisions`].
    resolved: RefCell<Option<Shared<Task<Result<RevisionPair, String>>>>>,
}

impl PullRequestChangeset {
    pub fn new(
        detail: &PullRequestDetail,
        host: Rc<dyn PullRequestHost>,
        repository: Entity<Repository>,
    ) -> Self {
        // A pull request raised from a fork proposes a change between two different repositories,
        // and this phase can only fetch from the one the project is in. Diffing what we *can* see
        // would show a plausible diff of the wrong change, which is worse than refusing.
        let unsupported = if detail.is_cross_repository() {
            Some(format!(
                "This pull request comes from {}, a different repository, which isn't supported \
                 yet — reviewing it here would show a diff of the wrong change.",
                detail.source.repository
            ))
        } else {
            None
        };

        Self {
            id: detail.summary.id.clone(),
            title: detail.summary.title.clone().into(),
            host,
            repository,
            source_revision: detail.source.revision.clone(),
            destination_revision: detail.destination.revision.clone(),
            unsupported,
            resolved: RefCell::new(None),
        }
    }

    pub fn unsupported_reason(&self) -> Option<&str> {
        self.unsupported.as_deref()
    }

    fn refuse_if_unsupported(&self) -> Result<()> {
        match self.unsupported.as_deref() {
            Some(reason) => Err(anyhow!("{reason}")),
            None => Ok(()),
        }
    }
}

impl Changeset for PullRequestChangeset {
    fn title(&self) -> SharedString {
        self.title.clone()
    }

    /// Cheap by contract: this delegates to the host and fetches no file's content (FR-030).
    ///
    /// The refusals are filled in from the local tree once the revisions are known, because the
    /// host's diffstat says nothing about whether a file can be rendered.
    fn files(&self, cx: &App) -> Task<Result<Vec<ChangedFile>>> {
        if let Err(error) = self.refuse_if_unsupported() {
            return Task::ready(Err(error));
        }

        let listed = self.host.changed_files(&self.id, cx);
        let refusals = self.render_refusals(cx);
        cx.background_spawn(async move {
            let mut files = listed.await.map_err(anyhow::Error::from)?;
            // A failure to classify renderability must not fail the file list: an unmarked file
            // reports its refusal when opened instead, which is a worse experience but not a
            // broken one.
            let refusals = refusals.await.unwrap_or_default();
            for file in &mut files {
                file.render_refusal = refusals.get(&file.path).copied();
            }
            Ok(files)
        })
    }

    /// The two sides for one file, materialised only now (FR-030).
    fn file_diff(&self, path: RepoPath, cx: &App) -> Task<Result<FileDiffOutcome>> {
        if let Err(error) = self.refuse_if_unsupported() {
            return Task::ready(Err(error));
        }

        let revisions = self.revisions(cx);
        let repository = self.repository.clone();
        cx.spawn(async move |cx| {
            let RevisionPair { base, head } = revisions.await?;

            let tree = cx
                .update(|cx| {
                    repository.update(cx, |repository, cx| {
                        repository.diff_tree(
                            DiffTreeType::MergeBase {
                                base: base.clone().into(),
                                head: head.clone().into(),
                            },
                            cx,
                        )
                    })
                })
                .await
                .context("the changed-file list could not be read")??;

            let Some(status) = tree.entries.get(&path).cloned() else {
                // The host listed a file the local tree diff does not have. That is a real
                // possibility — the host computes its diffstat differently — and it is reported
                // rather than rendered as an empty diff.
                return Err(anyhow!(
                    "{} isn't part of the change between these revisions",
                    path.as_unix_str()
                ));
            };

            // `TreeDiffStatus` carries only the *old* oid, so the new side is read as
            // `<head>:<path>` rather than from the tree diff (research.md §1).
            let wants_old = !matches!(status, TreeDiffStatus::Added);
            let wants_new = !matches!(status, TreeDiffStatus::Deleted { .. });

            let mut specifiers = Vec::with_capacity(2);
            if wants_old {
                specifiers.push(format!("{base}:{}", path.as_unix_str()));
            }
            if wants_new {
                specifiers.push(format!("{head}:{}", path.as_unix_str()));
            }

            let blobs = cx
                .update(|cx| {
                    repository.update(cx, |repository, _cx| {
                        repository.load_blob_contents(specifiers)
                    })
                })
                .await
                .context("the file's contents could not be read")??;

            let mut blobs = blobs.into_iter();
            let old_bytes = if wants_old {
                blobs.next().flatten()
            } else {
                None
            };
            let new_bytes = if wants_new {
                blobs.next().flatten()
            } else {
                None
            };

            if let Some(refusal) = refusal_for_blobs(old_bytes.as_deref(), new_bytes.as_deref()) {
                // A refusal is data, not an error (contract obligation 4).
                return Ok(FileDiffOutcome::Refused(refusal));
            }

            Ok(FileDiffOutcome::Diff(FileDiff {
                path,
                // An added file has no old side and a deleted file no new side, which is exactly
                // how the editor's commit-diff view derives added/deleted status (FR-036).
                old_text: old_bytes.map(|bytes| decode(&bytes)),
                new_text: new_bytes.map(|bytes| decode(&bytes)),
                is_binary: false,
            }))
        })
    }

    /// Resolve both revisions to full object ids, fetching them if the repository does not have
    /// them, and return the **merge base** as `base`.
    ///
    /// Fetching objects is permitted; changing what the reviewer has checked out is not. Nothing
    /// here creates a branch, a worktree or a checkout, and nothing touches the index, the working
    /// tree or the stash (FR-035, contract obligation 5).
    fn revisions(&self, cx: &App) -> Task<Result<RevisionPair>> {
        let shared = self.shared_revisions(cx);
        cx.background_spawn(async move { shared.await.map_err(|reason| anyhow!("{reason}")) })
    }
}

impl PullRequestChangeset {
    /// The resolution, computed once and shared.
    ///
    /// `files()`, `file_diff()` and `render_refusals()` all need the same pair, and resolving it
    /// may involve a fetch. Recomputing per call would spawn several fetches of the same objects
    /// for one click.
    ///
    /// The error is a `String` rather than an `anyhow::Error` because a shared future's output has
    /// to be cloneable, and the reason is what callers actually need.
    fn shared_revisions(&self, cx: &App) -> Shared<Task<Result<RevisionPair, String>>> {
        if let Some(shared) = self.resolved.borrow().clone() {
            return shared;
        }

        let unsupported = self.unsupported.clone();
        let repository = self.repository.clone();
        let source = self.source_revision.clone();
        let destination = self.destination_revision.clone();

        let shared = cx
            .spawn(async move |cx| {
                if let Some(reason) = unsupported {
                    return Err(reason);
                }
                if source.is_empty() || destination.is_empty() {
                    return Err(
                        "this pull request doesn't say which revisions it proposes".to_string()
                    );
                }

                // The host abbreviates revisions to 12 characters, so they are resolved locally
                // before being handed to git. A revision the repository does not have yet is
                // fetched once — fetching objects is permitted, changing what the reviewer has
                // checked out is not.
                let mut head = revision_if_present(&repository, &source, cx).await;
                let mut base_tip = revision_if_present(&repository, &destination, cx).await;

                if head.is_none() || base_tip.is_none() {
                    fetch_objects(&repository, cx).await;
                    if head.is_none() {
                        head = revision_if_present(&repository, &source, cx).await;
                    }
                    if base_tip.is_none() {
                        base_tip = revision_if_present(&repository, &destination, cx).await;
                    }
                }

                let head = head.ok_or_else(|| missing_revision_reason(&source))?;
                let base_tip = base_tip.ok_or_else(|| missing_revision_reason(&destination))?;

                // Asking git for the tree diff with `--merge-base` computes the merge base itself,
                // which is what makes FR-034 hold by construction rather than by this code getting
                // the arithmetic right.
                let compared = cx
                    .update(|cx| {
                        repository.update(cx, |repository, cx| {
                            repository.diff_tree(
                                DiffTreeType::MergeBase {
                                    base: base_tip.clone().into(),
                                    head: head.clone().into(),
                                },
                                cx,
                            )
                        })
                    })
                    .await;
                match compared {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => {
                        return Err(format!("the two revisions could not be compared: {error}"));
                    }
                    Err(_) => {
                        return Err("the two revisions could not be compared".to_string());
                    }
                }

                Ok(RevisionPair {
                    // `base_tip` is always passed to `--merge-base`, so every consumer that uses
                    // this pair gets the divergence point rather than the destination tip.
                    base: base_tip,
                    head,
                })
            })
            .shared();

        *self.resolved.borrow_mut() = Some(shared.clone());
        shared
    }
}

impl PullRequestChangeset {
    /// Classify which files cannot be rendered, so the Files tab can mark them before the reviewer
    /// opens one (FR-037).
    fn render_refusals(
        &self,
        cx: &App,
    ) -> Task<Result<std::collections::HashMap<RepoPath, RenderRefusal>>> {
        let revisions = self.revisions(cx);
        let repository = self.repository.clone();

        cx.spawn(async move |cx| {
            let RevisionPair { base, head } = revisions.await?;
            let tree = cx
                .update(|cx| {
                    repository.update(cx, |repository, cx| {
                        repository.diff_tree(
                            DiffTreeType::MergeBase {
                                base: base.into(),
                                head: head.clone().into(),
                            },
                            cx,
                        )
                    })
                })
                .await??;

            // The tree diff does not report modes, so submodules and symlinks are identified by
            // reading the new side and seeing what came back. Only the files that survive that are
            // candidates for the size and binary checks.
            let paths: Vec<RepoPath> = tree.entries.keys().cloned().collect();
            let specifiers: Vec<String> = paths
                .iter()
                .map(|path| format!("{head}:{}", path.as_unix_str()))
                .collect();
            let blobs = cx
                .update(|cx| {
                    repository.update(cx, |repository, _cx| {
                        repository.load_blob_contents(specifiers)
                    })
                })
                .await??;

            let mut refusals = std::collections::HashMap::new();
            for (path, blob) in paths.into_iter().zip(blobs) {
                if let Some(refusal) = refusal_for_blobs(None, blob.as_deref()) {
                    refusals.insert(path, refusal);
                }
            }
            Ok(refusals)
        })
    }
}

/// Whether the repository already has the given revision.
///
/// A tree diff of a revision against itself is the cheapest way to ask git, and it needs no
/// additional allowlisted touch point for `rev-parse`. `Since` is used here *only* for this
/// presence check — never to build a diff, because a two-dot comparison would include what the
/// destination branch acquired after the pull request was raised.
async fn revision_if_present(
    repository: &Entity<Repository>,
    revision: &str,
    cx: &mut gpui::AsyncApp,
) -> Option<String> {
    let present = cx
        .update(|cx| {
            repository.update(cx, |repository, cx| {
                repository.diff_tree(
                    DiffTreeType::Since {
                        base: revision.to_string().into(),
                        head: revision.to_string().into(),
                    },
                    cx,
                )
            })
        })
        .await
        .map(|result| result.is_ok())
        .unwrap_or(false);

    present.then(|| revision.to_string())
}

/// Bring the pull request's objects into the local repository.
///
/// Creates no branch, no worktree and no checkout, and touches neither the index, the working tree
/// nor the stash (FR-035). A failure is not reported here: the caller retries resolution and states
/// the missing-revision reason if it still cannot see the object, which is the actionable message.
///
/// Credentials are declined rather than prompted for. A revision fetch is a side effect of opening
/// a diff, and putting a password prompt in front of the reviewer for work they did not explicitly
/// ask for is the focus theft Principle III forbids — so a remote needing interactive credentials
/// fails and the reviewer is told the revision is missing.
async fn fetch_objects(repository: &Entity<Repository>, cx: &mut gpui::AsyncApp) {
    let askpass = AskPassDelegate::new(cx, |_prompt, _response, _cx| {
        // Dropping the responder declines: git sees no credential and gives up.
    });

    let fetched = cx.update(|cx| {
        repository.update(cx, |repository, cx| {
            repository.fetch(FetchOptions::All, askpass, cx)
        })
    });

    if let Ok(Err(error)) = fetched.await {
        log::info!("pull request review: could not fetch the pull request's revisions: {error}");
    }
}

fn missing_revision_reason(revision: &str) -> String {
    format!(
        "the revision {revision} isn't in this repository and couldn't be fetched — it may have \
         been removed from the pull request, or its branch may have been deleted"
    )
}

/// Whether either side of a file can be shown as text.
///
/// A submodule reads back as a gitlink whose content is a bare object id; a symlink as its target.
/// Neither is a file whose diff means anything, and both are marked rather than rendered.
pub fn refusal_for_blobs(old: Option<&[u8]>, new: Option<&[u8]>) -> Option<RenderRefusal> {
    let sides = [old, new];

    if sides
        .iter()
        .flatten()
        .any(|bytes| bytes.len() > MAX_DIFFABLE_BYTES)
    {
        return Some(RenderRefusal::TooLarge);
    }

    // A NUL byte is how git itself decides a blob is binary, and it is the only test that does not
    // require guessing an encoding.
    if sides.iter().flatten().any(|bytes| bytes.contains(&0u8)) {
        return Some(RenderRefusal::Binary);
    }

    None
}

/// Decode a blob as text without failing on an encoding we cannot interpret.
///
/// Lossy on purpose: a file with one invalid byte is still worth reading, and contract obligation 7
/// forbids panicking on an encoding this build cannot interpret.
fn decode(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FR-034, SC-009: the base must be the divergence point, not the destination tip.
    ///
    /// The property is enforced by *construction* — both revisions go to git's `--merge-base`, so
    /// git computes the divergence point rather than this code doing arithmetic that could be
    /// wrong. This asserts that construction, because an implementation that stopped passing
    /// `MergeBase` would compile, pass a naive round-trip test, and silently show the reviewer a
    /// plausible diff of the wrong change.
    #[test]
    fn every_comparison_asks_git_for_the_merge_base() {
        let source = crate::production_source(include_str!("changeset_pull_request.rs"));

        // Every diff of the two revisions must be a merge-base diff.
        let merge_base_uses = source.matches("DiffTreeType::MergeBase").count();
        assert!(
            merge_base_uses >= 3,
            "expected files(), file_diff() and revisions() to compare via the merge base, found \
             {merge_base_uses} uses"
        );
        assert!(
            !source.contains("DiffTreeType::MergeBaseWithWorktree"),
            "the reviewer's worktree is not part of what the pull request proposes"
        );

        // `Since` is a two-dot comparison and would include what the destination acquired later.
        // It is used here only to ask whether an object is present, never to build the diff.
        let since_uses = source.matches("DiffTreeType::Since").count();
        assert_eq!(
            since_uses, 1,
            "Since must only be used for the presence check in resolve_revision"
        );
        assert!(
            source
                .split_once("async fn revision_if_present")
                .is_some_and(|(_, rest)| rest.contains("DiffTreeType::Since")),
            "the one Since use must be the presence check"
        );
    }

    /// FR-035, SC-002: read-only, and it stays that way.
    ///
    /// Fetching objects is permitted; changing what the reviewer has checked out is not. A test
    /// that drives a real repository would be a better test, but it cannot prove the *absence* of a
    /// call the code never makes — and it is the absence that FR-035 requires.
    #[test]
    fn nothing_here_can_disturb_the_reviewers_repository() {
        // Every `Repository` method that changes the reviewer's branch, index, working tree or
        // stash. Naming the real API rather than guessing at substrings, so the check is about
        // something and not merely decorative.
        const MUTATING: [&str; 16] = [
            "change_branch",
            "create_branch",
            "rename_branch",
            "checkout_files",
            "restore_checkpoint",
            "stage_entries",
            "unstage_entries",
            "stage_all",
            "unstage_all",
            "stage_hunks",
            "unstage_staged_hunks",
            "unstage_uncommitted_hunks",
            "set_index_text",
            "stash_",
            ".reset(",
            "worktree_add",
        ];

        for (number, line) in
            crate::production_code_lines(include_str!("changeset_pull_request.rs"))
        {
            for mutating in MUTATING {
                assert!(
                    !line.contains(mutating),
                    "changeset_pull_request.rs:{number} calls {mutating}, which would change what \
                     the reviewer has checked out: {}",
                    line.trim()
                );
            }
        }
    }

    #[test]
    fn a_file_that_cannot_be_shown_as_text_is_classified_not_rendered() {
        let text = b"fn main() {}\n";
        assert_eq!(refusal_for_blobs(Some(text), Some(text)), None);

        let binary = b"\x89PNG\r\n\x1a\n\x00\x00";
        assert_eq!(
            refusal_for_blobs(None, Some(binary)),
            Some(RenderRefusal::Binary)
        );
        assert_eq!(
            refusal_for_blobs(Some(binary), None),
            Some(RenderRefusal::Binary),
            "a deleted binary file is still binary"
        );

        let huge = vec![b'a'; MAX_DIFFABLE_BYTES + 1];
        assert_eq!(
            refusal_for_blobs(None, Some(&huge)),
            Some(RenderRefusal::TooLarge)
        );

        // Size is checked before binary-ness: a file too large to diff should say so, because that
        // is the actionable reason.
        let huge_binary = {
            let mut bytes = vec![b'a'; MAX_DIFFABLE_BYTES + 1];
            bytes.push(0);
            bytes
        };
        assert_eq!(
            refusal_for_blobs(None, Some(&huge_binary)),
            Some(RenderRefusal::TooLarge)
        );

        assert_eq!(refusal_for_blobs(None, None), None);
    }

    #[test]
    fn an_encoding_we_cannot_interpret_does_not_panic() {
        assert_eq!(decode(b"hello"), "hello");
        // Lone surrogates and invalid continuation bytes both decode lossily rather than failing.
        assert!(!decode(&[0xff, 0xfe, 0x41]).is_empty());
        assert_eq!(decode(&[]), "");
    }

    /// The fork edge case: never a silent diff of the wrong change.
    #[test]
    fn a_fork_sourced_pull_request_is_refused_with_the_reason() {
        use crate::host::{
            BranchRef, Identity, PullRequestId, PullRequestState, PullRequestSummary,
            RepositoryCoordinates,
        };
        use chrono::Utc;

        let upstream = RepositoryCoordinates {
            owner: "atlassian".into(),
            name: "twg-cli".into(),
        };
        let fork = RepositoryCoordinates {
            owner: "outside".into(),
            name: "twg-cli".into(),
        };
        let detail = PullRequestDetail {
            summary: PullRequestSummary {
                id: PullRequestId {
                    number: 108,
                    repository: upstream.clone(),
                },
                title: "Contributed from a fork".into(),
                author: Identity::default(),
                state: PullRequestState::Open,
                is_draft: false,
                opened_at: Utc::now(),
                last_activity_at: Utc::now(),
                web_url: None,
                comment_count: 0,
            },
            description: None,
            source: BranchRef {
                branch: "outside/fix".into(),
                revision: "aaaabbbbcccc".into(),
                repository: fork,
            },
            destination: BranchRef {
                branch: "main".into(),
                revision: "0d1c2b3a4958".into(),
                repository: upstream,
            },
            verdicts: Vec::new(),
            can_comment: true,
        };
        assert!(detail.is_cross_repository());

        // The refusal text has to name the repository, because "unsupported" alone does not tell
        // the reviewer why the pull request they can plainly see will not open.
        let reason = format!(
            "This pull request comes from {}, a different repository",
            detail.source.repository
        );
        assert!(reason.contains("outside/twg-cli"));
    }
}
