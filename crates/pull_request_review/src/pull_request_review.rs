//! Pull request review inside Zed: list a repository's pull requests, read what one proposes, read
//! its diff in Zed's own split diff viewer, and comment on it.
//!
//! Every line of this feature lives in this crate (spec FR-074, constitution Principle VI). The
//! five pre-existing files it touches are enumerated in
//! `specs/001-pull-request-review/contracts/zed-surface.md` and enforced by
//! `script/check-fork-surface`.
//!
//! Two boundaries are mandated by number and declared here:
//!
//! * [`host::PullRequestHost`] (FR-056) — everything the host is asked for.
//! * [`changeset::Changeset`] (FR-058) — the change under review.
//!
//! The UI modules depend only on those traits, which is what keeps FR-057's "no host, platform or
//! transport name above the seam" checkable rather than aspirational.

pub mod changeset;
pub mod changeset_pull_request;
pub mod comments;
pub mod diff;
pub mod files;
pub mod host;
pub mod host_process;
pub mod host_twg;
pub mod list;
pub mod overview;
pub mod panel;
pub mod state;

use gpui::{App, actions};
use workspace::Workspace;

pub use panel::PullRequestPanel;

actions!(
    pull_request_review,
    [
        /// Opens the Pull Requests panel, or focuses it if it is already open.
        ToggleFocus,
        /// Reloads the pull request list, keeping the current selection, filters and sort.
        Refresh,
        /// Clears every filter, returning to the default view.
        ClearFilters,
        /// Reverses the order the pull request list is sorted in.
        ReverseSort,
        /// Comments on the line under the cursor in a pull request's diff.
        AddComment,
        /// Submits the comment being composed.
        SubmitComment,
        /// Discards the comment being composed without sending it.
        CancelComment,
    ]
);

/// Registers the feature's actions and its panel type.
///
/// This constructs nothing and contacts no host. FR-003 and FR-070 require the panel and all its
/// state to be built on first open, so that a reviewer who never opens it pays nothing at startup
/// beyond registering these handlers.
///
/// There is deliberately no settings schema field, no `default.json` entry and no default keymap
/// binding (FR-066): the actions are discoverable in the command palette and bindable, which is
/// what Principle III asks for, and shipping a binding would claim a key the reviewer did not
/// choose to give up.
pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        panel::register(workspace);
    })
    .detach();
}

/// The part of a source file that ships, with its `#[cfg(test)]` module removed.
///
/// Several checks in this crate assert a property of the code by reading it. Every one of them is
/// about what the feature *does*, so every one of them must ignore the tests — which name the very
/// things they forbid, in order to forbid them.
#[cfg(test)]
pub(crate) fn production_source(source: &str) -> &str {
    source
        .split_once("#[cfg(test)]")
        .map(|(production, _)| production)
        .unwrap_or(source)
}

/// The lines of a source file that actually declare or do something: no tests, and no comments.
///
/// Comments are excluded because these checks are about the vocabulary the code *exposes*, and the
/// clearest way to record why a thing is absent is a comment saying so — which a naive substring
/// search would then flag as the thing itself.
#[cfg(test)]
pub(crate) fn production_code_lines(source: &str) -> impl Iterator<Item = (usize, &str)> {
    production_source(source)
        .lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line))
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    /// FR-003, FR-070, SC-005: registering must be inert. Anything built here is startup cost taken
    /// from every reviewer, including the ones who never open the panel.
    ///
    /// `init` needs no project, no window and no host, which is the first half of the property; it
    /// runs here against a bare app to prove it.
    #[gpui::test]
    fn init_needs_nothing_and_can_be_called_before_anything_exists(cx: &mut TestAppContext) {
        cx.update(|cx| init(cx));
        cx.run_until_parked();
        // Nothing to await and nothing to observe: with no workspace open, `observe_new` has not
        // fired, so no panel and no host exist to be found.
    }

    /// The second half of the property, and the half that actually rots: `init`'s body must stay
    /// free of eager construction. A future change that builds the panel or touches the host here
    /// would pass the test above and still cost every reviewer startup time, so this reads the
    /// function's own source.
    #[test]
    fn init_constructs_nothing_and_contacts_no_host() {
        let source = include_str!("pull_request_review.rs");
        let body = source
            .split_once("pub fn init(cx: &mut App) {")
            .map(|(_, rest)| rest)
            .and_then(|rest| rest.split_once("\n}\n"))
            .map(|(body, _)| body)
            .expect("init must be findable in this file");

        for eager in [
            "PullRequestPanel::new",
            "TwgHost::new",
            "HostProcess::new",
            "state::load",
            ".run(",
            "add_panel",
        ] {
            assert!(
                !body.contains(eager),
                "init must not {eager} — FR-003 and FR-070 require this to happen on first open"
            );
        }
    }

    /// FR-057, SC-013: no host, platform or transport vocabulary may appear above the boundary.
    ///
    /// A grep is the whole point — the contract says the property is checkable, and a test that
    /// reads the sources is the cheapest thing that fails when someone leaks a platform name into a
    /// type, a field or a user-facing string.
    #[test]
    fn no_platform_vocabulary_appears_above_the_boundary() {
        // The two files permitted to know how the host is reached, plus this one: the crate root
        // declares those modules by name and states the forbidden words in order to forbid them.
        const BELOW_THE_SEAM: [&str; 3] =
            ["host_twg.rs", "host_process.rs", "pull_request_review.rs"];
        // Lowercased needles. `git` is absent on purpose: the changeset boundary is defined in
        // terms of revisions and blobs, and git is this feature's own storage rather than the host
        // it talks to.
        const FORBIDDEN: [&str; 6] = ["twg", "bitbucket", "github", "gitlab", "atlassian", "http"];

        let source_directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let entries =
            std::fs::read_dir(&source_directory).expect("the crate's src must be readable");

        let mut scanned = 0usize;
        let mut leaks = Vec::new();
        for entry in entries {
            let path = entry.expect("a readable directory entry").path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                continue;
            }
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            if BELOW_THE_SEAM.contains(&file_name.as_str()) {
                continue;
            }

            let source = std::fs::read_to_string(&path).expect("a readable source file");
            scanned += 1;
            for (number, line) in production_code_lines(&source) {
                let lowered = line.to_lowercase();
                // Module declarations wire the implementation up by name; that is not the code
                // exposing platform vocabulary.
                let trimmed = lowered.trim_start();
                if trimmed.starts_with("mod ") || trimmed.starts_with("pub mod ") {
                    continue;
                }
                for needle in FORBIDDEN {
                    if lowered.contains(needle) {
                        leaks.push(format!("{file_name}:{number}: {}", line.trim()));
                    }
                }
            }
        }

        assert!(
            scanned >= 8,
            "only {scanned} files were scanned — the check has stopped covering the crate"
        );
        assert!(
            leaks.is_empty(),
            "platform vocabulary leaked above the FR-056 boundary:\n{}",
            leaks.join("\n")
        );
    }
}
