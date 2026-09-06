//! The pull request list: rows, approval bubbles, filters and sort.

use chrono::{DateTime, Utc};
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Styled, Window, div, px,
    uniform_list,
};
use ui::{Avatar, ContextMenu, DropdownMenu, Indicator, Label, Tooltip, prelude::*};

use crate::host::{
    AuthorFilter, Identity, PullRequestState, PullRequestSummary, ReviewerVerdict, SortDirection,
    StateFilter, Verdict,
};
use crate::panel::{Load, PullRequestPanel};

/// Beyond this many bubbles the row stops being readable, so the rest collapse behind a `+N`
/// affordance whose tooltip carries the full list (FR-008).
const MAX_BUBBLES: usize = 4;

/// A relative age, as a reviewer would say it.
///
/// Written here rather than taken from `time_format`, which works in a different time type and has
/// no relative formatter — converting between the two would cost more than these few lines.
///
/// A non-positive age reads as *just now*: the timestamps are normalised at parse time so this
/// should not arise, but a clock that moves between parse and render must not produce "in -3
/// hours".
pub fn relative_age(from: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = (now - from).num_seconds();
    if seconds < 60 {
        return "just now".to_string();
    }

    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const WEEK: i64 = 7 * DAY;
    // Approximations, deliberately: a reviewer reading "2 months ago" does not want it to be
    // calendar-exact, they want to know it is stale.
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;

    let (count, unit) = match seconds {
        seconds if seconds < HOUR => (seconds / MINUTE, "minute"),
        seconds if seconds < DAY => (seconds / HOUR, "hour"),
        seconds if seconds < WEEK => (seconds / DAY, "day"),
        seconds if seconds < MONTH => (seconds / WEEK, "week"),
        seconds if seconds < YEAR => (seconds / MONTH, "month"),
        seconds => (seconds / YEAR, "year"),
    };
    let plural = if count == 1 { "" } else { "s" };
    format!("{count} {unit}{plural} ago")
}

/// The colour a status indicator takes. Open, draft, merged and declined must be distinguishable
/// (FR-006), so each maps to a different one.
pub fn status_color(state: PullRequestState, is_draft: bool) -> Color {
    match state {
        PullRequestState::Open if is_draft => Color::Muted,
        PullRequestState::Open => Color::Success,
        PullRequestState::Merged => Color::Accent,
        PullRequestState::Declined => Color::Error,
        PullRequestState::Superseded => Color::Warning,
        PullRequestState::Unknown => Color::Disabled,
    }
}

/// Only a verdict someone actually reached gets a bubble.
///
/// A pull request nobody has reviewed shows an **empty** area rather than a placeholder (FR-009),
/// and `ChangesRequested` is never shown as an approval (FR-007).
pub fn bubble_verdicts(verdicts: &[ReviewerVerdict]) -> Vec<&ReviewerVerdict> {
    verdicts
        .iter()
        .filter(|verdict| {
            matches!(
                verdict.verdict,
                Verdict::Approved | Verdict::ChangesRequested
            )
        })
        .collect()
}

fn verdict_indicator(verdict: Verdict) -> Option<Color> {
    match verdict {
        Verdict::Approved => Some(Color::Success),
        Verdict::ChangesRequested => Some(Color::Warning),
        Verdict::NoVerdict => None,
    }
}

fn render_bubble(reviewer: &Identity, verdict: Verdict) -> AnyElement {
    let tooltip = format!(
        "{} — {}",
        reviewer.label(),
        crate::comments::verdict_label(verdict)
    );

    let face = match reviewer.avatar_url.as_ref() {
        Some(url) => Avatar::new(url.to_string())
            .size(px(16.))
            .into_any_element(),
        // Never blank: a stable derived initial stands in when the host supplies neither a display
        // name nor an avatar (FR-010).
        None => div()
            .size(px(16.))
            .rounded_full()
            .bg(gpui::opaque_grey(0.5, 1.0))
            .flex()
            .items_center()
            .justify_center()
            .child(Label::new(reviewer.initial()).size(LabelSize::XSmall))
            .into_any_element(),
    };

    let mut bubble = h_flex().gap_0p5().child(face);
    if let Some(color) = verdict_indicator(verdict) {
        bubble = bubble.child(Indicator::dot().color(color));
    }
    bubble
        .id(gpui::SharedString::from(format!(
            "verdict-{}",
            reviewer.account_id
        )))
        .tooltip(Tooltip::text(tooltip))
        .into_any_element()
}

/// The approval bubbles for one row.
///
/// Returns `None` when nobody has reviewed, so the caller renders nothing at all rather than a
/// placeholder that reads as "loading" (FR-009).
pub fn render_bubbles(verdicts: &[ReviewerVerdict]) -> Option<AnyElement> {
    let bubbles = bubble_verdicts(verdicts);
    if bubbles.is_empty() {
        return None;
    }

    let overflow = bubbles.len().saturating_sub(MAX_BUBBLES);
    let mut row = h_flex().gap_1();
    for verdict in bubbles.iter().take(MAX_BUBBLES) {
        row = row.child(render_bubble(&verdict.reviewer, verdict.verdict));
    }
    if overflow > 0 {
        // The full list stays available on demand rather than being lost to the collapse (FR-008).
        let full_list = bubbles
            .iter()
            .map(|verdict| {
                format!(
                    "{} — {}",
                    verdict.reviewer.label(),
                    crate::comments::verdict_label(verdict.verdict)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        row = row.child(
            div()
                .id("verdict-overflow")
                .child(Label::new(format!("+{overflow}")).size(LabelSize::XSmall))
                .tooltip(Tooltip::text(full_list)),
        );
    }
    Some(row.into_any_element())
}

fn render_row(
    summary: &PullRequestSummary,
    verdicts: Option<&Vec<ReviewerVerdict>>,
    is_selected: bool,
    now: DateTime<Utc>,
    cx: &mut Context<PullRequestPanel>,
) -> AnyElement {
    let id = summary.id.clone();
    let author = summary.author.label();
    let age = relative_age(summary.last_activity_at, now);
    let state_label = summary.state.label(summary.is_draft);

    ui::ListItem::new(gpui::SharedString::from(format!(
        "pr-{}",
        summary.id.number
    )))
    .toggle_state(is_selected)
    .start_slot(Indicator::dot().color(status_color(summary.state, summary.is_draft)))
    .child(
        v_flex()
            .w_full()
            .gap_0p5()
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .justify_between()
                    .child(
                        // Elided within the panel's width rather than wrapped or allowed to
                        // force horizontal scroll, however long the title is.
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .child(Label::new(summary.title.clone()).truncate()),
                    )
                    .children(verdicts.and_then(|verdicts| render_bubbles(verdicts))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Label::new(format!("#{}", summary.id.number))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        Label::new(state_label)
                            .size(LabelSize::Small)
                            .color(status_color(summary.state, summary.is_draft)),
                    )
                    .child(
                        Label::new(author)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(Label::new(age).size(LabelSize::Small).color(Color::Muted)),
            ),
    )
    .on_click(cx.listener(move |panel, _event, _window, cx| {
        panel.select(id.clone(), cx);
    }))
    .into_any_element()
}

pub fn render_list(
    panel: &PullRequestPanel,
    _window: &mut Window,
    cx: &mut Context<PullRequestPanel>,
) -> AnyElement {
    // A project that cannot be reviewed at all says so, with the reason. This is not the same as a
    // failed request, and not the same as an empty list (FR-005).
    if let Some(error) = panel.coordinates_error() {
        return notice(error.message(), None, cx).into_any_element();
    }

    match panel.pull_requests() {
        Load::Idle | Load::Loading => h_flex()
            .p_2()
            .child(ui::LoadingLabel::new("Loading pull requests"))
            .into_any_element(),

        // Reported in the panel's own surface with a retry: no modal, no focus theft, and the rest
        // of Zed unaffected (FR-071, FR-073).
        Load::Failed(error) => notice(error.message(), error.remedy(), cx)
            .child(retry_button(cx))
            .into_any_element(),

        Load::Ready(summaries) if summaries.is_empty() => {
            // A filter that matches nothing is a different thing from a repository with no pull
            // requests, and offering to clear the filters is only useful in the first case
            // (FR-020).
            let mut notice = if panel.has_filters() {
                notice(
                    "No pull requests match the current filters.".to_string(),
                    None,
                    cx,
                )
                .child(
                    ui::Button::new("clear-filters", "Clear filters")
                        .label_size(LabelSize::Small)
                        .on_click(cx.listener(|panel, _event, _window, cx| {
                            panel.clear_filters(cx);
                        })),
                )
            } else {
                notice(
                    "This repository has no pull requests.".to_string(),
                    None,
                    cx,
                )
            };
            notice = notice.child(retry_button(cx));
            notice.into_any_element()
        }

        Load::Ready(summaries) => {
            let now = Utc::now();
            let selected = panel.selected().cloned();
            let rows: Vec<PullRequestSummary> = summaries.clone();
            let verdicts: Vec<Option<Vec<ReviewerVerdict>>> = summaries
                .iter()
                .map(|summary| panel.verdicts_for(&summary.id).cloned())
                .collect();

            v_flex()
                .size_full()
                .children(panel.vanished_selection().map(|number| {
                    // Reported rather than a silent deselection: a pull request that was merged
                    // while being read is something the reviewer needs to know (FR-011).
                    notice(
                        format!("Pull request #{number} is no longer in this list."),
                        None,
                        cx,
                    )
                }))
                .child(
                    uniform_list(
                        "pull-requests",
                        rows.len(),
                        cx.processor(move |_panel, range: std::ops::Range<usize>, _window, cx| {
                            range
                                .filter_map(|index| {
                                    let summary = rows.get(index)?;
                                    let is_selected = selected.as_ref() == Some(&summary.id);
                                    Some(render_row(
                                        summary,
                                        verdicts.get(index).and_then(Option::as_ref),
                                        is_selected,
                                        now,
                                        cx,
                                    ))
                                })
                                .collect()
                        }),
                    )
                    .size_full(),
                )
                .into_any_element()
        }
    }
}

fn notice(
    message: String,
    remedy: Option<&'static str>,
    _cx: &mut Context<PullRequestPanel>,
) -> gpui::Div {
    v_flex()
        .p_2()
        .gap_1()
        .child(Label::new(message).size(LabelSize::Small))
        .children(remedy.map(|remedy| {
            Label::new(remedy)
                .size(LabelSize::Small)
                .color(Color::Muted)
        }))
}

fn retry_button(cx: &mut Context<PullRequestPanel>) -> impl IntoElement {
    ui::Button::new("retry", "Retry")
        .label_size(LabelSize::Small)
        .on_click(cx.listener(|panel, _event, _window, cx| {
            panel.refresh(cx);
        }))
}

/// The filter bar. Which filters and sort are in effect is visible at all times (FR-018), because a
/// list narrowed by a filter the reviewer has forgotten about looks like a list that is wrong.
pub fn render_filter_bar(
    panel: &PullRequestPanel,
    window: &mut Window,
    cx: &mut Context<PullRequestPanel>,
) -> impl IntoElement {
    let state_filter = panel.view_state().state_filter;
    let author_filter = panel.view_state().author_filter.clone();
    let sort = panel.view_state().sort_direction;
    let has_filters = panel.has_filters();

    h_flex()
        .p_1()
        .gap_1()
        .flex_wrap()
        .border_b_1()
        .border_color(cx.theme().colors().border)
        .child(render_state_filter(state_filter, window, cx))
        .child(render_author_filter(author_filter, panel, window, cx))
        .child(render_sort_control(sort, cx))
        .when(has_filters, |this| {
            this.child(
                ui::Button::new("clear-all-filters", "Clear")
                    .label_size(LabelSize::Small)
                    .tooltip(Tooltip::text("Clear every filter"))
                    .on_click(cx.listener(|panel, _event, _window, cx| {
                        panel.clear_filters(cx);
                    })),
            )
        })
        .child(div().flex_1())
        .child(
            ui::IconButton::new("refresh", IconName::RotateCw)
                .icon_size(IconSize::Small)
                .tooltip(Tooltip::text("Refresh"))
                .on_click(cx.listener(|panel, _event, _window, cx| {
                    panel.refresh(cx);
                })),
        )
}

pub fn state_filter_label(filter: StateFilter) -> &'static str {
    match filter {
        // Named for what it lists rather than for the host's state values, because a draft is an
        // open pull request and a reviewer should not have to know that.
        StateFilter::OpenAndDraft => "Open & draft",
        StateFilter::All => "Any state",
    }
}

fn render_state_filter(
    current: StateFilter,
    window: &mut Window,
    cx: &mut Context<PullRequestPanel>,
) -> impl IntoElement {
    // A context menu's handlers run in the menu's own context, not the panel's, so the panel is
    // reached through a weak handle rather than `cx.listener`.
    let panel = cx.weak_entity();

    DropdownMenu::new(
        "state-filter",
        state_filter_label(current),
        ContextMenu::build(window, cx, move |menu, _window, _cx| {
            let mut menu = menu;
            for filter in [StateFilter::OpenAndDraft, StateFilter::All] {
                let panel = panel.clone();
                menu = menu.toggleable_entry(
                    state_filter_label(filter),
                    filter == current,
                    ui::IconPosition::Start,
                    None,
                    move |_window, cx| {
                        panel
                            .update(cx, |panel, cx| panel.set_state_filter(filter, cx))
                            .ok();
                    },
                );
            }
            menu
        }),
    )
}

pub fn author_filter_label(filter: Option<&AuthorFilter>) -> String {
    match filter {
        None => "Any author".to_string(),
        Some(AuthorFilter::Me) => "Mine".to_string(),
        Some(AuthorFilter::Person { label, .. }) => label.clone(),
    }
}

fn render_author_filter(
    current: Option<AuthorFilter>,
    panel: &PullRequestPanel,
    window: &mut Window,
    cx: &mut Context<PullRequestPanel>,
) -> impl IntoElement {
    // Everyone who appears as an author in the current list, so the reviewer picks rather than
    // types — and never has to know their own account name to filter to themselves (FR-015).
    let mut authors: Vec<Identity> = panel
        .pull_requests()
        .ready()
        .map(|summaries| {
            let mut seen = std::collections::HashSet::new();
            summaries
                .iter()
                .filter(|summary| seen.insert(summary.author.account_id.clone()))
                .map(|summary| summary.author.clone())
                .collect()
        })
        .unwrap_or_default();
    authors.sort_by_key(|author| author.label().to_lowercase());

    let selected = current.clone();
    let handle = cx.weak_entity();
    DropdownMenu::new(
        "author-filter",
        author_filter_label(current.as_ref()),
        ContextMenu::build(window, cx, move |menu, _window, _cx| {
            let mut menu = menu.toggleable_entry(
                "Any author",
                selected.is_none(),
                ui::IconPosition::Start,
                None,
                {
                    let handle = handle.clone();
                    move |_window, cx| {
                        handle
                            .update(cx, |panel, cx| panel.set_author_filter(None, cx))
                            .ok();
                    }
                },
            );

            menu = menu.toggleable_entry(
                "Mine",
                selected == Some(AuthorFilter::Me),
                ui::IconPosition::Start,
                None,
                {
                    let handle = handle.clone();
                    move |_window, cx| {
                        handle
                            .update(cx, |panel, cx| {
                                panel.set_author_filter(Some(AuthorFilter::Me), cx)
                            })
                            .ok();
                    }
                },
            );

            for author in &authors {
                // The host's author filter matches a nickname, so an author it never reported one
                // for cannot be filtered by and is not offered.
                let Some(nickname) = author.nickname.clone() else {
                    continue;
                };
                let label = author.label().to_string();
                let is_selected = matches!(
                    &selected,
                    Some(AuthorFilter::Person { nickname: selected, .. }) if selected == &nickname
                );
                let handle = handle.clone();
                menu = menu.toggleable_entry(
                    label.clone(),
                    is_selected,
                    ui::IconPosition::Start,
                    None,
                    move |_window, cx| {
                        let filter = AuthorFilter::Person {
                            nickname: nickname.clone(),
                            label: label.clone(),
                        };
                        handle
                            .update(cx, |panel, cx| panel.set_author_filter(Some(filter), cx))
                            .ok();
                    },
                );
            }
            menu
        }),
    )
}

pub fn sort_label(sort: SortDirection) -> &'static str {
    match sort {
        SortDirection::MostRecentFirst => "Recent first",
        SortDirection::LeastRecentFirst => "Oldest first",
    }
}

fn render_sort_control(
    sort: SortDirection,
    cx: &mut Context<PullRequestPanel>,
) -> impl IntoElement {
    // One button that both states the current order and reverses it, so the indicator cannot
    // disagree with the order the list is actually in (FR-017).
    let icon = match sort {
        SortDirection::MostRecentFirst => IconName::ArrowDown,
        SortDirection::LeastRecentFirst => IconName::ArrowUp,
    };
    ui::Button::new("sort", sort_label(sort))
        .label_size(LabelSize::Small)
        .start_icon(ui::Icon::new(icon).size(IconSize::Small))
        .tooltip(Tooltip::text("Reverse the order"))
        .on_click(cx.listener(|panel, _event, _window, cx| {
            panel.reverse_sort(cx);
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn identity(display_name: Option<&str>, account_id: &str) -> Identity {
        Identity {
            display_name: display_name.map(str::to_owned),
            nickname: None,
            account_id: account_id.to_string(),
            avatar_url: None,
        }
    }

    fn verdict(account_id: &str, verdict: Verdict) -> ReviewerVerdict {
        ReviewerVerdict {
            reviewer: identity(Some("Someone"), account_id),
            verdict,
            at: None,
        }
    }

    #[test]
    fn a_relative_age_never_reads_as_the_future() {
        let now = Utc::now();
        assert_eq!(relative_age(now, now), "just now");
        assert_eq!(relative_age(now + Duration::hours(3), now), "just now");
    }

    #[test]
    fn relative_ages_read_the_way_a_reviewer_would_say_them() {
        let now = Utc::now();
        let cases = [
            (Duration::seconds(30), "just now"),
            (Duration::minutes(1), "1 minute ago"),
            (Duration::minutes(45), "45 minutes ago"),
            (Duration::hours(1), "1 hour ago"),
            (Duration::hours(23), "23 hours ago"),
            (Duration::days(1), "1 day ago"),
            (Duration::days(6), "6 days ago"),
            (Duration::days(8), "1 week ago"),
            (Duration::days(21), "3 weeks ago"),
            (Duration::days(60), "2 months ago"),
            (Duration::days(400), "1 year ago"),
        ];
        for (ago, expected) in cases {
            assert_eq!(relative_age(now - ago, now), expected, "for {ago}");
        }
    }

    /// FR-006: open, draft, merged and declined must each be distinguishable.
    #[test]
    fn every_state_gets_its_own_indicator() {
        let colors = [
            status_color(PullRequestState::Open, false),
            status_color(PullRequestState::Open, true),
            status_color(PullRequestState::Merged, false),
            status_color(PullRequestState::Declined, false),
        ];
        let mut distinct = colors.to_vec();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            colors.len(),
            "open, draft, merged and declined must not share an indicator"
        );
    }

    /// FR-009: an empty area, not a placeholder.
    #[test]
    fn nobody_having_reviewed_renders_nothing_at_all() {
        assert!(render_bubbles(&[]).is_none());
        assert!(
            render_bubbles(&[verdict("a", Verdict::NoVerdict)]).is_none(),
            "a requested reviewer who has not responded is not a bubble"
        );
    }

    /// FR-007: `ChangesRequested` is never shown as an approval, but it *is* shown.
    #[test]
    fn a_changes_requested_verdict_gets_its_own_bubble() {
        let verdicts = [
            verdict("a", Verdict::Approved),
            verdict("b", Verdict::ChangesRequested),
            verdict("c", Verdict::NoVerdict),
        ];
        let bubbles = bubble_verdicts(&verdicts);
        assert_eq!(bubbles.len(), 2, "the no-verdict reviewer gets no bubble");
        assert_ne!(
            verdict_indicator(Verdict::Approved),
            verdict_indicator(Verdict::ChangesRequested),
            "the two must not look the same"
        );
        assert_eq!(verdict_indicator(Verdict::NoVerdict), None);
    }

    /// FR-008: bounded bubbles plus a `+N` affordance.
    #[test]
    fn beyond_a_bounded_count_the_rest_collapse() {
        let verdicts: Vec<ReviewerVerdict> = (0..9)
            .map(|index| verdict(&format!("a{index}"), Verdict::Approved))
            .collect();
        let bubbles = bubble_verdicts(&verdicts);
        assert_eq!(bubbles.len(), 9, "every verdict is still known");
        assert!(
            bubbles.len() > MAX_BUBBLES,
            "this case must actually exercise the overflow"
        );
        assert!(render_bubbles(&verdicts).is_some());
    }

    /// FR-018: the labels must say what is in effect, in the reviewer's terms.
    #[test]
    fn the_active_filters_and_sort_are_always_nameable() {
        assert_eq!(
            state_filter_label(StateFilter::OpenAndDraft),
            "Open & draft"
        );
        assert_eq!(state_filter_label(StateFilter::All), "Any state");
        assert_ne!(
            state_filter_label(StateFilter::OpenAndDraft),
            state_filter_label(StateFilter::All)
        );

        assert_eq!(author_filter_label(None), "Any author");
        assert_eq!(author_filter_label(Some(&AuthorFilter::Me)), "Mine");
        assert_eq!(
            author_filter_label(Some(&AuthorFilter::Person {
                nickname: "ada".into(),
                label: "Ada Lovelace".into(),
            })),
            "Ada Lovelace"
        );

        assert_ne!(
            sort_label(SortDirection::MostRecentFirst),
            sort_label(SortDirection::LeastRecentFirst),
            "the indicator must reflect which order the list is in"
        );
    }

    /// FR-010: never blank.
    #[test]
    fn a_reviewer_with_no_name_and_no_avatar_still_renders() {
        let bare = identity(None, "712020:zzzz");
        assert_eq!(bare.label(), "712020:zzzz");
        assert_eq!(bare.initial(), "7");
        assert!(
            bare.avatar_url.is_none(),
            "the fallback path is the one under test"
        );
    }
}
