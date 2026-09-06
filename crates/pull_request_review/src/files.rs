//! The Files tab: every changed file, its change kind, its line counts, and the way into the diff.

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Styled, Window, div,
    uniform_list,
};
use ui::{DiffStat, Label, Tooltip, prelude::*};

use crate::changeset::{ChangeKind, ChangedFile};
use crate::panel::{Load, PullRequestPanel};

/// What an empty changeset says.
///
/// Not an empty list: a pull request that changes nothing is a real and confusing thing to be
/// looking at, and saying so is the difference between an answer and a blank screen (FR-029).
pub const NOTHING_TO_REVIEW: &str = "This pull request changes nothing.";

/// The totals for the whole pull request (FR-027).
pub fn totals(files: &[ChangedFile]) -> (u32, u32) {
    files.iter().fold((0, 0), |(added, removed), file| {
        (
            added.saturating_add(file.lines_added),
            removed.saturating_add(file.lines_removed),
        )
    })
}

/// How a row identifies its file.
///
/// A renamed file shows both paths, so the reviewer can see what moved as well as what changed
/// (FR-028).
pub fn row_label(file: &ChangedFile) -> String {
    match (&file.previous_path, file.change_kind) {
        (Some(previous), ChangeKind::Renamed) => {
            format!("{} → {}", previous.as_unix_str(), file.path.as_unix_str())
        }
        _ => file.path.as_unix_str().to_string(),
    }
}

fn change_kind_color(kind: ChangeKind) -> Color {
    match kind {
        ChangeKind::Added => Color::Success,
        ChangeKind::Modified => Color::Accent,
        ChangeKind::Deleted => Color::Error,
        ChangeKind::Renamed => Color::Warning,
    }
}

pub fn render(
    panel: &mut PullRequestPanel,
    _window: &mut Window,
    cx: &mut Context<PullRequestPanel>,
) -> AnyElement {
    match panel.changed_files() {
        Load::Idle | Load::Loading => div()
            .p_2()
            .child(ui::LoadingLabel::new("Loading changed files"))
            .into_any_element(),

        // Reported with its reason and a retry. The Overview tab is a separate load, so it stays
        // readable (FR-031).
        Load::Failed(error) => v_flex()
            .p_2()
            .gap_1()
            .child(Label::new(error.message()).size(LabelSize::Small))
            .children(error.remedy().map(|remedy| {
                Label::new(remedy)
                    .size(LabelSize::Small)
                    .color(Color::Muted)
            }))
            .child(
                ui::Button::new("retry-files", "Retry")
                    .label_size(LabelSize::Small)
                    .on_click(cx.listener(|panel, _event, _window, cx| {
                        panel.load_changed_files(cx);
                    })),
            )
            .into_any_element(),

        Load::Ready(files) if files.is_empty() => div()
            .p_2()
            .child(
                Label::new(NOTHING_TO_REVIEW)
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .into_any_element(),

        Load::Ready(files) => {
            let (added, removed) = totals(files);
            let rows: Vec<ChangedFile> = files.clone();
            let count = rows.len();

            v_flex()
                .size_full()
                .child(
                    h_flex()
                        .p_1()
                        .gap_2()
                        .child(
                            Label::new(format!(
                                "{count} file{}",
                                if count == 1 { "" } else { "s" }
                            ))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                        )
                        .child(DiffStat::new(
                            "pull-request-total",
                            added as usize,
                            removed as usize,
                        )),
                )
                .child(
                    // A uniform list, so thousands of changed files stay scrollable and responsive
                    // — only the visible rows are built.
                    uniform_list(
                        "changed-files",
                        count,
                        cx.processor(move |_panel, range: std::ops::Range<usize>, _window, cx| {
                            range
                                .filter_map(|index| {
                                    let file = rows.get(index)?;
                                    Some(render_row(file, index, cx))
                                })
                                .collect()
                        }),
                    )
                    .flex_1(),
                )
                .into_any_element()
        }
    }
}

fn render_row(file: &ChangedFile, index: usize, cx: &mut Context<PullRequestPanel>) -> AnyElement {
    let path = file.path.clone();
    let refusal = file.render_refusal;
    let label = row_label(file);

    ui::ListItem::new(index)
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .justify_between()
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .child(Label::new(label).size(LabelSize::Small).truncate()),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .child(
                            Label::new(file.change_kind.label())
                                .size(LabelSize::XSmall)
                                .color(change_kind_color(file.change_kind)),
                        )
                        // Marked before the reviewer opens it, so a file that will not render is
                        // not a surprise they discover by clicking (FR-037).
                        .children(refusal.map(|refusal| {
                            div()
                                .id(("refusal", index))
                                .child(
                                    Label::new("not shown")
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted),
                                )
                                .tooltip(Tooltip::text(refusal.message()))
                        }))
                        .child(DiffStat::new(
                            ("file-diffstat", index),
                            file.lines_added as usize,
                            file.lines_removed as usize,
                        )),
                ),
        )
        .on_click(cx.listener(move |panel, _event, window, cx| {
            crate::diff::open_file(panel, path.clone(), window, cx);
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use git::repository::RepoPath;

    fn file(
        path: &str,
        previous: Option<&str>,
        kind: ChangeKind,
        added: u32,
        removed: u32,
    ) -> ChangedFile {
        ChangedFile {
            path: RepoPath::new(path).expect("a valid path"),
            previous_path: previous.map(|previous| RepoPath::new(previous).expect("a valid path")),
            change_kind: kind,
            lines_added: added,
            lines_removed: removed,
            render_refusal: None,
        }
    }

    /// FR-027: line counts per file, and a total for the pull request.
    #[test]
    fn the_totals_are_the_sum_of_the_files() {
        let files = vec![
            file("a.rs", None, ChangeKind::Added, 10, 0),
            file("b.rs", None, ChangeKind::Modified, 4, 7),
            file("c.rs", None, ChangeKind::Deleted, 0, 30),
        ];
        assert_eq!(totals(&files), (14, 37));
        assert_eq!(totals(&[]), (0, 0));
    }

    #[test]
    fn absurd_line_counts_cannot_overflow_the_total() {
        let files = vec![
            file("a.rs", None, ChangeKind::Added, u32::MAX, 0),
            file("b.rs", None, ChangeKind::Added, 10, 0),
        ];
        assert_eq!(totals(&files), (u32::MAX, 0));
    }

    /// FR-028: both paths identifiable on a renamed file's row.
    #[test]
    fn a_renamed_file_shows_both_paths() {
        let renamed = file(
            "packages/sdk/src/help/skills/matching.ts",
            Some("packages/sdk/src/help/matching.ts"),
            ChangeKind::Renamed,
            3,
            3,
        );
        let label = row_label(&renamed);
        assert!(
            label.contains("packages/sdk/src/help/matching.ts"),
            "{label}"
        );
        assert!(
            label.contains("packages/sdk/src/help/skills/matching.ts"),
            "{label}"
        );

        // A previous path on a non-rename is not shown as one, because it is not a rename.
        let modified = file("a.rs", Some("b.rs"), ChangeKind::Modified, 1, 1);
        assert_eq!(row_label(&modified), "a.rs");
    }

    /// FR-029: says so, rather than showing an empty list.
    #[test]
    fn an_empty_changeset_says_there_is_nothing_to_review() {
        assert!(!NOTHING_TO_REVIEW.is_empty());
        assert!(
            NOTHING_TO_REVIEW.to_lowercase().contains("nothing"),
            "the message must actually say there is nothing"
        );
    }

    #[test]
    fn every_change_kind_is_distinguishable() {
        let colors = [
            change_kind_color(ChangeKind::Added),
            change_kind_color(ChangeKind::Modified),
            change_kind_color(ChangeKind::Deleted),
            change_kind_color(ChangeKind::Renamed),
        ];
        let mut distinct = colors.to_vec();
        distinct.dedup();
        assert_eq!(distinct.len(), 4);

        for kind in [
            ChangeKind::Added,
            ChangeKind::Modified,
            ChangeKind::Deleted,
            ChangeKind::Renamed,
        ] {
            assert!(!kind.label().is_empty());
        }
    }
}
