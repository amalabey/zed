//! The changeset boundary.
//!
//! This file says nothing about pull requests, hosts or reviewers, and it must stay that way — see
//! the trait's doc comment for why.

use anyhow::Result;
use git::repository::RepoPath;
use gpui::{App, SharedString, Task};

/// One row of the Files tab (FR-027, FR-028).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: RepoPath,
    /// Set for a rename, so both the old and the new path are identifiable (FR-028).
    pub previous_path: Option<RepoPath>,
    pub change_kind: ChangeKind,
    pub lines_added: u32,
    pub lines_removed: u32,
    /// `Some` means the diff will not be rendered, and says why. Set at list time so the Files tab
    /// can mark the file *before* the reviewer opens it (FR-037).
    pub render_refusal: Option<RenderRefusal>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl ChangeKind {
    pub fn label(self) -> &'static str {
        match self {
            ChangeKind::Added => "Added",
            ChangeKind::Modified => "Modified",
            ChangeKind::Deleted => "Deleted",
            ChangeKind::Renamed => "Renamed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderRefusal {
    Binary,
    TooLarge,
    Submodule,
    Symlink,
}

impl RenderRefusal {
    pub fn message(self) -> &'static str {
        match self {
            RenderRefusal::Binary => "Binary file — no textual diff to show.",
            RenderRefusal::TooLarge => "This file is too large to diff.",
            RenderRefusal::Submodule => "Submodule — its contents aren't part of this change.",
            RenderRefusal::Symlink => "Symlink — only the link target changed.",
        }
    }
}

/// The two sides of one file, materialised only when the reviewer opens it (FR-030).
///
/// Maps directly onto `project::git_store::CommitFile`: an added file has `old_text: None` and a
/// deleted file `new_text: None`, which is exactly how the editor's commit-diff view derives
/// added/deleted status (FR-036).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    pub path: RepoPath,
    pub old_text: Option<String>,
    pub new_text: Option<String>,
    pub is_binary: bool,
}

/// Fully-resolved object ids. Resolution happens inside the implementation, because a source may
/// report an abbreviated revision and every consumer needs the full one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevisionPair {
    /// The **merge base** of head and destination, never the destination tip. See obligation 3 on
    /// the trait.
    pub base: String,
    pub head: String,
}

/// Opening a file either yields a diff or states why it will not be rendered. A refusal is data,
/// not an error (obligation 4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileDiffOutcome {
    Diff(FileDiff),
    Refused(RenderRefusal),
}

/// The single boundary supplying the change under review.
///
/// Declared to discharge **spec FR-058**, which mandates this seam by number so that a second
/// changeset kind — **branch comparison**, and later commit ranges or the working tree — can be
/// added by supplying another implementation of this trait alone. Per constitution Principle I this
/// is a plain trait with no registry and no dynamic discovery; the citation is required where it is
/// declared.
///
/// Obligations on every implementation (contracts/changeset.md):
///
/// 1. **Nothing about pull requests.** This trait must be implementable by branch comparison
///    without contortion, so it may not mention a pull request, a host or a reviewer (FR-061). It
///    is a pair of revisions and the files that differ between them.
/// 2. **`files()` is cheap; `file_diff()` is not.** `files()` must not fetch or prepare any file's
///    content (FR-030).
/// 3. **The base is the divergence point.** `revisions()` returns the merge base of head and
///    destination as `base`, never the destination tip. An implementation that returns the tip
///    satisfies the signature and violates the contract (FR-034).
/// 4. **Refusals are data, not errors.** A file that cannot be rendered is reported through
///    [`ChangedFile::render_refusal`] at list time, and `file_diff` returns the refusal rather than
///    failing (FR-037).
/// 5. **Read-only, and it stays that way.** No implementation may check out, switch branches,
///    stash, or modify the working tree or index (FR-035). Fetching objects is permitted; changing
///    what the reviewer has checked out is not.
/// 6. **Off the foreground thread, and cancellable** (FR-067, FR-069).
/// 7. **Never panics** on a missing revision, an unreadable blob, or an encoding it cannot
///    interpret.
///
/// Like [`crate::host::PullRequestHost`], this is `'static` but not `Send + Sync`: an
/// implementation holds GPUI handles and is called only from the foreground thread. Obligation 6 is
/// about where the work *runs*, which the returned [`Task`] governs.
pub trait Changeset: 'static {
    fn title(&self) -> SharedString;

    fn files(&self, cx: &App) -> Task<Result<Vec<ChangedFile>>>;

    fn file_diff(&self, path: RepoPath, cx: &App) -> Task<Result<FileDiffOutcome>>;

    fn revisions(&self, cx: &App) -> Task<Result<RevisionPair>>;
}

/// Classify a path the local tree diff reported but that cannot be shown as text.
///
/// Mode bits come from the tree diff rather than the filesystem, because the file may not exist in
/// the reviewer's working tree at all.
pub fn refusal_for_mode(
    mode: Option<&str>,
    is_binary: bool,
    byte_length: Option<u64>,
) -> Option<RenderRefusal> {
    /// Past this the multibuffer stops being a useful way to read a change, and building buffers
    /// for it costs more than it is worth.
    const MAX_DIFFABLE_BYTES: u64 = 4 * 1024 * 1024;

    match mode {
        Some("160000") => return Some(RenderRefusal::Submodule),
        Some("120000") => return Some(RenderRefusal::Symlink),
        _ => {}
    }
    if is_binary {
        return Some(RenderRefusal::Binary);
    }
    if byte_length.is_some_and(|length| length > MAX_DIFFABLE_BYTES) {
        return Some(RenderRefusal::TooLarge);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_that_cannot_be_diffed_are_named_specifically() {
        assert_eq!(
            refusal_for_mode(Some("160000"), false, None),
            Some(RenderRefusal::Submodule)
        );
        assert_eq!(
            refusal_for_mode(Some("120000"), false, None),
            Some(RenderRefusal::Symlink)
        );
        assert_eq!(
            refusal_for_mode(Some("100644"), true, None),
            Some(RenderRefusal::Binary)
        );
        assert_eq!(
            refusal_for_mode(Some("100644"), false, Some(64 * 1024 * 1024)),
            Some(RenderRefusal::TooLarge)
        );
        assert_eq!(refusal_for_mode(Some("100644"), false, Some(1024)), None);
        assert_eq!(refusal_for_mode(None, false, None), None);
    }

    #[test]
    fn every_refusal_states_a_reason() {
        for refusal in [
            RenderRefusal::Binary,
            RenderRefusal::TooLarge,
            RenderRefusal::Submodule,
            RenderRefusal::Symlink,
        ] {
            assert!(!refusal.message().is_empty());
        }
    }
}
