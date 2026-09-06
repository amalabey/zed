//! The Overview tab: status, title, description, branches, author, approvals, browser link.

use gpui::{
    AnyElement, Context, Entity, InteractiveElement, IntoElement, ParentElement, Styled, Window,
    div,
};
use markdown::{Markdown, MarkdownElement};
use ui::{Divider, Label, Tooltip, prelude::*};

use crate::host::{CommentThread, PullRequestDetail, ReviewerVerdict, Verdict};
use crate::panel::{Load, PullRequestPanel};

/// What "no description" reads as.
///
/// Both `None` and `Some("")` land here. A blank area would be indistinguishable from a load that
/// failed, which is the thing FR-024 is guarding against.
pub const NO_DESCRIPTION: &str = "No description.";

/// The description a reviewer should see, or `None` when there is nothing to render.
///
/// Whitespace counts as nothing: a description of three newlines is not a description.
pub fn description_to_render(description: Option<&str>) -> Option<&str> {
    description
        .map(str::trim)
        .filter(|description| !description.is_empty())
}

pub fn render(
    panel: &mut PullRequestPanel,
    window: &mut Window,
    cx: &mut Context<PullRequestPanel>,
) -> AnyElement {
    match panel.detail() {
        Load::Idle | Load::Loading => div()
            .p_2()
            .child(ui::LoadingLabel::new("Loading pull request"))
            .into_any_element(),

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
                ui::Button::new("retry-detail", "Retry")
                    .label_size(LabelSize::Small)
                    .on_click(cx.listener(|panel, _event, _window, cx| panel.refresh(cx))),
            )
            .into_any_element(),

        Load::Ready(detail) => {
            // Reported here rather than over the diff: the threads failing to load must not make
            // the code unreadable, and a comment can still be added (FR-055).
            let comment_problem = panel.comment_problem().map(str::to_owned);
            let diff_problem = panel.diff_problem().map(str::to_owned);
            let unanchored = render_unanchored_comments(panel.comments());

            v_flex()
                .size_full()
                .children(diff_problem.map(|reason| notice(reason, Color::Error)))
                .children(comment_problem.map(|reason| notice(reason, Color::Warning)))
                .child(render_detail(detail, window, cx))
                .children(unanchored.map(|comments| div().p_2().child(comments)))
                .into_any_element()
        }
    }
}

fn notice(message: String, color: Color) -> impl IntoElement {
    div()
        .p_2()
        .child(Label::new(message).size(LabelSize::Small).color(color))
}

fn render_detail(
    detail: &PullRequestDetail,
    window: &mut Window,
    cx: &mut Context<PullRequestPanel>,
) -> impl IntoElement {
    let summary = &detail.summary;
    let web_url = summary.web_url.clone();

    v_flex()
        .id("overview")
        .size_full()
        .p_2()
        .gap_2()
        .overflow_y_scroll()
        .child(
            h_flex()
                .gap_2()
                .justify_between()
                .child(
                    v_flex()
                        .flex_1()
                        .gap_0p5()
                        .child(Label::new(summary.title.clone()))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Label::new(summary.state.label(summary.is_draft))
                                        .size(LabelSize::Small)
                                        .color(crate::list::status_color(
                                            summary.state,
                                            summary.is_draft,
                                        )),
                                )
                                .child(
                                    Label::new(format!("#{}", summary.id.number))
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                )
                                .child(
                                    Label::new(summary.author.label())
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                ),
                        ),
                )
                .children(web_url.map(|url| {
                    ui::IconButton::new("open-in-browser", IconName::ArrowUpRight)
                        .icon_size(IconSize::Small)
                        .tooltip(Tooltip::text("Open in browser"))
                        .on_click(move |_event, _window, cx| {
                            cx.open_url(url.as_str());
                        })
                })),
        )
        .child(
            h_flex()
                .gap_1()
                .flex_wrap()
                .child(
                    Label::new(detail.source.branch.clone())
                        .size(LabelSize::Small)
                        .color(Color::Accent),
                )
                .child(Label::new("→").size(LabelSize::Small).color(Color::Muted))
                .child(
                    Label::new(detail.destination.branch.clone())
                        .size(LabelSize::Small)
                        .color(Color::Accent),
                ),
        )
        .child(Divider::horizontal())
        .child(render_description(detail, window, cx))
        .child(Divider::horizontal())
        .child(render_verdicts(&detail.verdicts))
}

fn render_description(
    detail: &PullRequestDetail,
    _window: &mut Window,
    cx: &mut Context<PullRequestPanel>,
) -> AnyElement {
    match description_to_render(detail.description.as_deref()) {
        // Rendered as formatted markdown rather than raw markup (FR-023), through the same crate
        // the rest of Zed renders markdown with.
        Some(description) => {
            let markdown = build_markdown(description.to_string(), cx);
            div()
                .child(MarkdownElement::new(
                    markdown,
                    markdown::MarkdownStyle::default(),
                ))
                .into_any_element()
        }
        None => Label::new(NO_DESCRIPTION)
            .size(LabelSize::Small)
            .color(Color::Muted)
            .into_any_element(),
    }
}

fn build_markdown(source: String, cx: &mut Context<PullRequestPanel>) -> Entity<Markdown> {
    let language_registry = cx.entity().read(cx).project().read(cx).languages().clone();
    cx.new(|cx| Markdown::new(source.into(), Some(language_registry), None, cx))
}

/// Every reviewer's position, including the ones who have not reached one — a requested reviewer
/// who has not responded is information the author needs (FR-023).
fn render_verdicts(verdicts: &[ReviewerVerdict]) -> AnyElement {
    if verdicts.is_empty() {
        return Label::new("No reviewers yet.")
            .size(LabelSize::Small)
            .color(Color::Muted)
            .into_any_element();
    }

    let mut column = v_flex().gap_0p5();
    for verdict in verdicts {
        let color = match verdict.verdict {
            Verdict::Approved => Color::Success,
            Verdict::ChangesRequested => Color::Warning,
            Verdict::NoVerdict => Color::Muted,
        };
        column = column.child(
            h_flex()
                .gap_2()
                .child(Label::new(verdict.reviewer.label()).size(LabelSize::Small))
                .child(
                    Label::new(crate::comments::verdict_label(verdict.verdict))
                        .size(LabelSize::Small)
                        .color(color),
                ),
        );
    }
    column.into_any_element()
}

/// Comments with no anchor, shown here rather than dropped (FR-052).
///
/// Rendered as a list of author-and-body pairs. There is no resolve affordance, because resolving
/// threads is out of scope (FR-054).
pub fn render_unanchored_comments(threads: &[CommentThread]) -> Option<AnyElement> {
    let unanchored = crate::comments::unanchored(threads);
    let visible: Vec<&&CommentThread> = unanchored
        .iter()
        // A deleted comment has no body to show, and showing an empty one would suggest the author
        // said nothing rather than that they withdrew it.
        .filter(|thread| !thread.is_deleted)
        .collect();
    if visible.is_empty() {
        return None;
    }

    let mut column = v_flex().gap_1().child(
        Label::new("Comments on the pull request")
            .size(LabelSize::Small)
            .color(Color::Muted),
    );
    for thread in visible {
        column = column.child(
            v_flex()
                .gap_0p5()
                .child(
                    h_flex()
                        .gap_1()
                        .child(Label::new(thread.author.label()).size(LabelSize::Small))
                        .when(thread.is_pending, |this| {
                            this.child(
                                Label::new("pending")
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted),
                            )
                        }),
                )
                .child(
                    Label::new(thread.body.clone())
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
        );
    }
    Some(column.into_any_element())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    use crate::host::{CommentId, Identity};

    /// FR-024: neither `None` nor `Some("")` may render as a blank area.
    #[test]
    fn an_absent_and_an_empty_description_both_read_as_no_description() {
        assert_eq!(description_to_render(None), None);
        assert_eq!(description_to_render(Some("")), None);
        assert_eq!(description_to_render(Some("   \n\t ")), None);
        assert_eq!(description_to_render(Some("Fixes it.")), Some("Fixes it."));
        assert!(!NO_DESCRIPTION.is_empty());
    }

    fn thread(id: &str, body: &str, anchored: bool, is_deleted: bool) -> CommentThread {
        CommentThread {
            id: CommentId(id.into()),
            anchor: anchored.then(|| crate::host::CommentAnchor {
                path: git::repository::RepoPath::new("a.rs").expect("a valid path"),
                side: crate::host::DiffSide::New,
                lines: 1..=1,
            }),
            author: Identity {
                display_name: Some("Ada Lovelace".into()),
                ..Default::default()
            },
            body: body.into(),
            created_at: Utc::now(),
            replies: Vec::new(),
            is_deleted,
            is_pending: false,
            is_outdated: false,
        }
    }

    /// FR-052: an unanchored comment belongs here, not nowhere.
    #[test]
    fn unanchored_comments_are_kept_and_anchored_ones_are_not_duplicated_here() {
        let threads = vec![
            thread("1", "General note", false, false),
            thread("2", "On a line", true, false),
        ];
        assert!(
            render_unanchored_comments(&threads).is_some(),
            "the general note must appear in Overview"
        );

        let only_anchored = vec![thread("2", "On a line", true, false)];
        assert!(
            render_unanchored_comments(&only_anchored).is_none(),
            "an anchored comment belongs in the diff, and must not be shown twice"
        );
    }

    #[test]
    fn a_withdrawn_comment_is_not_shown_as_an_empty_one() {
        let threads = vec![thread("1", "", false, true)];
        assert!(render_unanchored_comments(&threads).is_none());
    }
}
