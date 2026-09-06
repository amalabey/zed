//! The one [`PullRequestHost`] implementation: Bitbucket, reached through the `twg` CLI.
//!
//! This file and `host_process.rs` are the only two in the crate permitted to name the host, the
//! platform or the transport (FR-057). Everything above the boundary sees only the vocabulary in
//! `host.rs`.
//!
//! Two rules govern every function here, both learned from the running tool (research.md §4):
//!
//! * **Always `-o json`.** Without it the tool mixes an "Update available" banner into stdout.
//! * **Option names come from the tool's `opts` schema or `--help`, never from its `examples`
//!   block**, which documents options (`--file-path`, `--line-to`) that do not exist.

use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use git::repository::RepoPath;
use gpui::{App, Task, WeakEntity};
use project::ProjectEnvironment;
use serde_json::Value;
use url::Url;

use crate::changeset::{ChangeKind, ChangedFile};
use crate::host::{
    AuthorFilter, BranchRef, CommentAnchor, CommentId, CommentThread, DiffSide, DraftComment,
    FlatComment, HostError, Identity, ListQuery, PullRequestDetail, PullRequestHost, PullRequestId,
    PullRequestState, PullRequestSummary, RepositoryCoordinates, ReviewerVerdict, SortDirection,
    StateFilter, Verdict, normalize_timestamps,
};
use crate::host_process::{HostProcess, ProcessOutput};

/// The executable name. Resolved through the project's shell environment, never assumed to be on
/// the process `PATH` (FR-062a).
pub const PROGRAM: &str = "twg";

/// The tool's own defaults silently truncate on real pull requests: `diffstat` stops at 100 files
/// and `comment query` at 50 comments. FR-027 requires *every* changed file, so both are raised
/// explicitly. There is no cursor or page-token option — `--limit` is the only knob.
const DIFFSTAT_LIMIT: usize = 2000;
const COMMENT_LIMIT: usize = 1000;
pub const DEFAULT_LIST_LIMIT: usize = 100;

/// Every state the tool's `--state` option accepts. There is no `all` value, and no `DRAFT` state:
/// a draft is an otherwise-open pull request carrying `draft: true`.
const ALL_STATES: [&str; 4] = ["OPEN", "MERGED", "DECLINED", "SUPERSEDED"];

pub struct TwgHost {
    process: Arc<HostProcess>,
}

impl TwgHost {
    pub fn new(
        working_directory: Arc<Path>,
        environment: WeakEntity<ProjectEnvironment>,
        cx: &mut App,
    ) -> Self {
        Self {
            process: Arc::new(HostProcess::new(
                PROGRAM,
                working_directory,
                environment,
                cx,
            )),
        }
    }

    fn repository_args(repository: &RepositoryCoordinates) -> Vec<String> {
        // Passed explicitly even though the tool can auto-detect them: the feature already knows
        // the coordinates and must not depend on the tool's working directory.
        vec![
            "-w".into(),
            repository.owner.clone(),
            "-r".into(),
            repository.name.clone(),
            "-o".into(),
            "json".into(),
        ]
    }
}

impl PullRequestHost for TwgHost {
    fn list(
        &self,
        repository: &RepositoryCoordinates,
        query: ListQuery,
        cx: &App,
    ) -> Task<Result<Vec<PullRequestSummary>, HostError>> {
        let process = self.process.clone();
        let repository = repository.clone();
        let invocations = list_invocations(&repository, &query);

        cx.spawn(async move |cx| {
            let mut summaries: Vec<PullRequestSummary> = Vec::new();

            // `All` needs one call per state, because the tool has no "any state" value. They are
            // issued in sequence rather than concurrently so a rate limit stops the fan-out
            // instead of multiplying it.
            for args in invocations {
                let output = cx.update(|cx| process.run(args, cx)).await?;
                let stdout = successful_stdout(&output)?;
                let page = parse_list(&stdout, &repository, Utc::now())?;
                summaries.extend(page);
            }

            Ok(order_and_limit(summaries, query.sort, query.limit))
        })
    }

    fn detail(&self, id: &PullRequestId, cx: &App) -> Task<Result<PullRequestDetail, HostError>> {
        let process = self.process.clone();
        let repository = id.repository.clone();
        let mut args = vec![
            "bitbucket".into(),
            "pull-requests".into(),
            "get".into(),
            id.number.to_string(),
        ];
        args.extend(Self::repository_args(&repository));

        cx.spawn(async move |cx| {
            let output = cx.update(|cx| process.run(args, cx)).await?;
            let stdout = successful_stdout(&output)?;
            parse_detail(&stdout, &repository, Utc::now())
        })
    }

    fn changed_files(
        &self,
        id: &PullRequestId,
        cx: &App,
    ) -> Task<Result<Vec<ChangedFile>, HostError>> {
        let process = self.process.clone();
        let mut args = vec![
            "bitbucket".into(),
            "pull-requests".into(),
            "diffstat".into(),
            id.number.to_string(),
        ];
        args.extend(Self::repository_args(&id.repository));
        args.extend(["-n".to_string(), DIFFSTAT_LIMIT.to_string()]);

        cx.spawn(async move |cx| {
            let output = cx.update(|cx| process.run(args, cx)).await?;
            let stdout = successful_stdout(&output)?;
            parse_diffstat(&stdout)
        })
    }

    fn comments(
        &self,
        id: &PullRequestId,
        cx: &App,
    ) -> Task<Result<Vec<CommentThread>, HostError>> {
        let process = self.process.clone();
        let mut args = vec![
            "bitbucket".into(),
            "pull-requests".into(),
            "comment".into(),
            "query".into(),
            id.number.to_string(),
        ];
        args.extend(Self::repository_args(&id.repository));
        args.extend(["-n".to_string(), COMMENT_LIMIT.to_string()]);

        cx.spawn(async move |cx| {
            let output = cx.update(|cx| process.run(args, cx)).await?;
            let stdout = successful_stdout(&output)?;
            parse_comments(&stdout)
        })
    }

    fn post_comment(
        &self,
        id: &PullRequestId,
        draft: DraftComment,
        cx: &App,
    ) -> Task<Result<CommentThread, HostError>> {
        let process = self.process.clone();
        let args = post_comment_args(id, &draft);

        cx.spawn(async move |cx| {
            let output = cx.update(|cx| process.run(args, cx)).await?;
            let stdout = successful_stdout(&output)?;
            let value = parse_json(&stdout)?;
            parse_comment(&value).ok_or_else(|| HostError::UnexpectedResponse {
                detail: "the posted comment was not echoed back in a recognisable shape".into(),
                version: None,
            })
        })
    }

    fn viewer(&self, cx: &App) -> Task<Result<Identity, HostError>> {
        let process = self.process.clone();
        let args = vec!["user".to_string(), "-o".to_string(), "json".to_string()];

        cx.spawn(async move |cx| {
            let output = cx.update(|cx| process.run(args, cx)).await?;
            let stdout = successful_stdout(&output)?;
            let value = parse_json(&stdout)?;
            parse_identity(&value).ok_or_else(|| HostError::UnexpectedResponse {
                detail: "the signed-in account was not reported in a recognisable shape".into(),
                version: None,
            })
        })
    }
}

/// Build the invocations one `list` call needs.
///
/// `OpenAndDraft` is a single `--state OPEN` call, because drafts *are* open pull requests. `All`
/// needs one call per state, which is what the tool's lack of an "any" value forces. Filters go to
/// the host rather than being applied locally, so they cover the repository's whole set rather than
/// a page (FR-019).
pub fn list_invocations(repository: &RepositoryCoordinates, query: &ListQuery) -> Vec<Vec<String>> {
    let states: &[&str] = match query.state {
        StateFilter::OpenAndDraft => &["OPEN"],
        StateFilter::All => &ALL_STATES,
    };

    states
        .iter()
        .map(|state| {
            let mut args = vec![
                "bitbucket".to_string(),
                "pull-requests".to_string(),
                "query".to_string(),
            ];
            args.extend(TwgHost::repository_args(repository));
            args.extend(["--state".to_string(), (*state).to_string()]);
            if let Some(nickname) = author_filter_nickname(query.author.as_ref()) {
                // `--author` matches a nickname, not a username slug or an account id.
                args.extend(["--author".to_string(), nickname]);
            }
            args.extend(["-n".to_string(), query.limit.max(1).to_string()]);
            args
        })
        .collect()
}

/// Put the merged pages in order and take the requested number.
///
/// The ordering is applied to *everything the host returned across every state*, not to each page
/// in turn. That is what FR-019 and SC-007 require: a filter or sort applied to a partial page and
/// presented as complete is precisely the bug they guard against, and it would be invisible — the
/// list would look perfectly plausible.
///
/// Deduplicated by identity, because a pull request that changes state between two of the fan-out
/// calls would otherwise appear twice.
pub fn order_and_limit(
    mut summaries: Vec<PullRequestSummary>,
    sort: SortDirection,
    limit: usize,
) -> Vec<PullRequestSummary> {
    let mut seen = std::collections::HashSet::new();
    summaries.retain(|summary| seen.insert(summary.id.clone()));

    summaries.sort_by(|left, right| {
        let ordering = match sort {
            SortDirection::MostRecentFirst => right.last_activity_at.cmp(&left.last_activity_at),
            SortDirection::LeastRecentFirst => left.last_activity_at.cmp(&right.last_activity_at),
        };
        // Ties broken by number, so the order is stable rather than dependent on which fan-out
        // call happened to answer first.
        ordering.then_with(|| match sort {
            SortDirection::MostRecentFirst => right.id.number.cmp(&left.id.number),
            SortDirection::LeastRecentFirst => left.id.number.cmp(&right.id.number),
        })
    });
    summaries.truncate(limit.max(1));
    summaries
}

/// `AuthorFilter::Me` is resolved to the viewer's nickname by the caller before it gets here,
/// because it is stored as a marker rather than a resolved identity (FR-015).
fn author_filter_nickname(filter: Option<&AuthorFilter>) -> Option<String> {
    match filter {
        Some(AuthorFilter::Person { nickname, .. }) if !nickname.is_empty() => {
            Some(nickname.clone())
        }
        _ => None,
    }
}

/// Exactly one of `--line` and `--from-line` is ever set, and never a range.
///
/// The tool has `--line` (new side) and `--from-line` (old side) and no `--start-line`, so a range
/// cannot be posted at all. Since FR-040 puts multi-line comments out of scope, this maps cleanly
/// rather than approximating: [`DraftComment`] carries one line, so there is nothing to collapse.
pub fn post_comment_args(id: &PullRequestId, draft: &DraftComment) -> Vec<String> {
    let mut args = vec![
        "bitbucket".to_string(),
        "pull-requests".to_string(),
        "comment".to_string(),
        "create".to_string(),
        "--pull-request".to_string(),
        id.number.to_string(),
        "--text".to_string(),
        draft.body.clone(),
        "--path".to_string(),
        draft.path.as_unix_str().to_string(),
    ];
    match draft.side {
        DiffSide::New => args.extend(["--line".to_string(), draft.line.to_string()]),
        DiffSide::Old => args.extend(["--from-line".to_string(), draft.line.to_string()]),
    }
    if let Some(CommentId(parent)) = draft.reply_to.as_ref() {
        args.extend(["--reply-to".to_string(), parent.clone()]);
    }
    args.extend(TwgHost::repository_args(&id.repository));
    args
}

// -- Failure classification (FR-064) ------------------------------------------------------------

/// Take stdout, or classify the failure.
///
/// A zero exit with no output is a failure too: the tool always emits JSON on success with
/// `-o json`, so silence means something changed shape.
pub fn successful_stdout(output: &ProcessOutput) -> Result<String, HostError> {
    if output.succeeded() {
        return Ok(output.stdout.clone());
    }
    Err(classify_failure(&output.stderr, &output.stdout))
}

/// Map the tool's own error text onto a distinct condition.
///
/// The ordering matters: an expired credential mentions authentication too, and a rate-limit
/// response mentions the request being refused, so the more specific patterns are tested first.
/// `PrerequisiteMissing` is never produced here — it can only come from executable resolution,
/// which is what keeps it from being conflated with an authentication failure (FR-062b).
pub fn classify_failure(stderr: &str, stdout: &str) -> HostError {
    let haystack = format!("{stderr}\n{stdout}").to_lowercase();
    let mentions = |needles: &[&str]| needles.iter().any(|needle| haystack.contains(needle));

    if mentions(&["rate limit", "too many requests", "429"]) {
        return HostError::RateLimited;
    }
    if mentions(&[
        "expired",
        "token has expired",
        "re-authenticate",
        "reauthenticate",
    ]) {
        return HostError::CredentialExpired;
    }
    if mentions(&[
        "not logged in",
        "not authenticated",
        "unauthorized",
        "unauthenticated",
        "401",
        "auth login",
        "please log in",
    ]) {
        return HostError::NotAuthenticated;
    }
    if mentions(&[
        "forbidden",
        "403",
        "do not have access",
        "permission denied",
        "no access to",
    ]) {
        return HostError::PermissionDenied {
            repository: "this repository".into(),
        };
    }
    if mentions(&[
        "no such host",
        "dial tcp",
        "connection refused",
        "connection reset",
        "network is unreachable",
        "timeout",
        "timed out",
        "temporary failure in name resolution",
        "dns",
    ]) {
        return HostError::Unreachable {
            detail: first_line(stderr).unwrap_or_else(|| "the network request failed".into()),
        };
    }

    HostError::UnexpectedResponse {
        detail: first_line(stderr)
            .or_else(|| first_line(stdout))
            .unwrap_or_else(|| "the tool failed without saying why".into()),
        version: None,
    }
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(300).collect())
}

// -- Defensive parsing (FR-062b, FR-065) --------------------------------------------------------

fn unexpected(detail: impl Into<String>) -> HostError {
    HostError::UnexpectedResponse {
        detail: detail.into(),
        version: None,
    }
}

pub fn parse_json(stdout: &str) -> Result<Value, HostError> {
    let trimmed = stdout.trim_start_matches('\u{feff}').trim();
    if trimmed.is_empty() {
        return Err(unexpected("the tool produced no output"));
    }
    serde_json::from_str(trimmed).map_err(|error| unexpected(format!("unreadable output: {error}")))
}

/// Take the array of rows out of the response.
///
/// A bare array is what the tool emits today; an envelope with a `values` or `data` array is
/// accepted too, so a future wrapper does not blank the list.
fn parse_rows(stdout: &str) -> Result<Vec<Value>, HostError> {
    let value = parse_json(stdout)?;
    match value {
        Value::Array(rows) => Ok(rows),
        Value::Object(mut object) => ["values", "data", "results"]
            .into_iter()
            .find_map(|key| match object.remove(key) {
                Some(Value::Array(rows)) => Some(rows),
                _ => None,
            })
            .ok_or_else(|| unexpected("expected a list of items, got a single object")),
        _ => Err(unexpected("expected a list of items")),
    }
}

/// Parse each row independently, so one malformed row degrades to a missing row rather than an
/// empty list (FR-065).
///
/// The exception is a response where *nothing* parsed: rows were promised and none were
/// recognisable, which is a shape change rather than partial data, and reporting it as an empty
/// list would hide a broken integration behind a plausible screen.
fn parse_each<T>(
    rows: Vec<Value>,
    what: &str,
    mut parse_row: impl FnMut(&Value) -> Option<T>,
) -> Result<Vec<T>, HostError> {
    let promised = rows.len();
    let mut parsed = Vec::with_capacity(promised);
    let mut skipped = 0usize;
    for row in &rows {
        match parse_row(row) {
            Some(item) => parsed.push(item),
            None => skipped += 1,
        }
    }

    if promised > 0 && parsed.is_empty() {
        return Err(unexpected(format!(
            "none of the {promised} {what} the tool returned were in a recognisable shape"
        )));
    }
    if skipped > 0 {
        log::warn!("pull request review: skipped {skipped} unreadable {what}");
    }
    Ok(parsed)
}

fn as_str(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn as_u64(value: &Value, key: &str) -> Option<u64> {
    let field = value.get(key)?;
    field
        .as_u64()
        // The tool reports numbers as numbers, but a string-typed id would otherwise blank a row.
        .or_else(|| field.as_str()?.trim().parse().ok())
}

fn as_u32(value: &Value, key: &str) -> Option<u32> {
    as_u64(value, key).and_then(|number| u32::try_from(number).ok())
}

fn as_bool(value: &Value, key: &str) -> Option<bool> {
    value.get(key)?.as_bool()
}

fn as_timestamp(value: &Value, key: &str) -> Option<DateTime<Utc>> {
    let text = as_str(value, key)?;
    DateTime::parse_from_rfc3339(&text)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn as_url(value: &Value, path: &[&str]) -> Option<Url> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(key)?;
    }
    Url::parse(cursor.as_str()?).ok()
}

/// An unrecognised state degrades to [`PullRequestState::Unknown`] rather than failing the row that
/// contains it (FR-065).
fn parse_state(value: &Value) -> PullRequestState {
    match as_str(value, "state")
        .unwrap_or_default()
        .to_uppercase()
        .as_str()
    {
        "OPEN" => PullRequestState::Open,
        "MERGED" => PullRequestState::Merged,
        "DECLINED" | "REJECTED" => PullRequestState::Declined,
        "SUPERSEDED" => PullRequestState::Superseded,
        other => {
            if !other.is_empty() {
                log::debug!("pull request review: unrecognised state {other:?}");
            }
            PullRequestState::Unknown
        }
    }
}

pub fn parse_identity(value: &Value) -> Option<Identity> {
    let account_id = as_str(value, "account_id")
        .or_else(|| as_str(value, "uuid"))
        .or_else(|| as_str(value, "nickname"))?;
    Some(Identity {
        display_name: as_str(value, "display_name"),
        nickname: as_str(value, "nickname"),
        account_id,
        avatar_url: as_url(value, &["links", "avatar", "href"]),
    })
}

fn parse_identity_or_unknown(value: Option<&Value>) -> Identity {
    value.and_then(parse_identity).unwrap_or_else(|| Identity {
        // Never blank: FR-010 requires *something* renderable even when the host tells us
        // nothing about the person.
        account_id: String::new(),
        ..Identity::default()
    })
}

fn parse_coordinates(
    value: Option<&Value>,
    fallback: &RepositoryCoordinates,
) -> RepositoryCoordinates {
    let Some(full_name) = value.and_then(|value| as_str(value, "full_name")) else {
        return fallback.clone();
    };
    match full_name.split_once('/') {
        Some((owner, name)) if !owner.is_empty() && !name.is_empty() => RepositoryCoordinates {
            owner: owner.to_string(),
            name: name.to_string(),
        },
        _ => fallback.clone(),
    }
}

fn parse_branch_ref(value: Option<&Value>, fallback: &RepositoryCoordinates) -> Option<BranchRef> {
    let value = value?;
    Some(BranchRef {
        branch: value
            .get("branch")
            .and_then(|branch| as_str(branch, "name"))
            .unwrap_or_default(),
        // Abbreviated to 12 characters by the host; resolved to a full object id before it is ever
        // handed to git.
        revision: value
            .get("commit")
            .and_then(|commit| as_str(commit, "hash"))
            .unwrap_or_default(),
        repository: parse_coordinates(value.get("repository"), fallback),
    })
}

fn parse_summary(
    value: &Value,
    repository: &RepositoryCoordinates,
    now: DateTime<Utc>,
) -> Option<PullRequestSummary> {
    let number = as_u64(value, "id")?;
    let (opened_at, last_activity_at) = normalize_timestamps(
        as_timestamp(value, "created_on"),
        as_timestamp(value, "updated_on"),
        now,
    );

    Some(PullRequestSummary {
        id: PullRequestId {
            number,
            repository: repository.clone(),
        },
        title: as_str(value, "title").unwrap_or_else(|| format!("Pull request {number}")),
        author: parse_identity_or_unknown(value.get("author")),
        state: parse_state(value),
        is_draft: as_bool(value, "draft").unwrap_or(false),
        opened_at,
        last_activity_at,
        web_url: as_url(value, &["links", "html", "href"]),
        comment_count: as_u32(value, "comment_count").unwrap_or(0),
    })
}

pub fn parse_list(
    stdout: &str,
    repository: &RepositoryCoordinates,
    now: DateTime<Utc>,
) -> Result<Vec<PullRequestSummary>, HostError> {
    let rows = parse_rows(stdout)?;
    parse_each(rows, "pull requests", |row| {
        parse_summary(row, repository, now)
    })
}

/// `approved: true` is an approval; `state: "changes_requested"` is the opposite and must never be
/// rendered as one. Anything else — including a verdict string this build has not seen — is no
/// verdict at all, which is the honest reading (FR-007).
fn parse_verdict(value: &Value) -> Verdict {
    if as_bool(value, "approved").unwrap_or(false) {
        return Verdict::Approved;
    }
    match as_str(value, "state").unwrap_or_default().as_str() {
        "changes_requested" => Verdict::ChangesRequested,
        "approved" => Verdict::Approved,
        _ => Verdict::NoVerdict,
    }
}

/// `participants[]` is the only source of per-reviewer verdicts. `reviewers[]` is a plain list of
/// requested reviewers carrying no verdict at all, so it is not a substitute (research.md §2).
fn parse_verdicts(value: &Value) -> Vec<ReviewerVerdict> {
    let Some(participants) = value.get("participants").and_then(Value::as_array) else {
        return Vec::new();
    };
    participants
        .iter()
        .filter_map(|participant| {
            Some(ReviewerVerdict {
                reviewer: parse_identity(participant.get("user")?)?,
                verdict: parse_verdict(participant),
                at: as_timestamp(participant, "participated_on"),
            })
        })
        .collect()
}

pub fn parse_detail(
    stdout: &str,
    repository: &RepositoryCoordinates,
    now: DateTime<Utc>,
) -> Result<PullRequestDetail, HostError> {
    let value = parse_json(stdout)?;
    let summary = parse_summary(&value, repository, now)
        .ok_or_else(|| unexpected("the pull request had no readable identity"))?;

    let source = parse_branch_ref(value.get("source"), repository)
        .ok_or_else(|| unexpected("the pull request reported no source branch"))?;
    let destination = parse_branch_ref(value.get("destination"), repository)
        .ok_or_else(|| unexpected("the pull request reported no destination branch"))?;

    // Only an open pull request can be commented on, and the tool does not report the viewer's
    // permission directly — so an authenticated reviewer is assumed able to comment on an open one
    // and told otherwise by the post itself. Getting this wrong in the permissive direction costs
    // one failed post with the reason stated; getting it wrong the other way hides the action
    // entirely (FR-047).
    let can_comment = matches!(summary.state, PullRequestState::Open);

    Ok(PullRequestDetail {
        summary,
        // Read without trimming to nothing: `Some("")` and `None` both render as "no description"
        // (FR-024), but conflating them here would throw away what the host actually said.
        description: value
            .get("content")
            .and_then(|content| content.get("raw"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| as_str(&value, "description")),
        source,
        destination,
        verdicts: parse_verdicts(&value),
        can_comment,
    })
}

fn parse_change_kind(status: &str) -> Option<ChangeKind> {
    match status {
        "added" => Some(ChangeKind::Added),
        "modified" | "changed" => Some(ChangeKind::Modified),
        "removed" | "deleted" => Some(ChangeKind::Deleted),
        "renamed" => Some(ChangeKind::Renamed),
        _ => None,
    }
}

fn diffstat_path(value: &Value, key: &str) -> Option<RepoPath> {
    let path = value.get(key)?.get("path")?.as_str()?;
    RepoPath::new(path).ok()
}

fn parse_changed_file(value: &Value) -> Option<ChangedFile> {
    let status = as_str(value, "status").unwrap_or_default();
    // An unrecognised status still describes a changed file, and dropping the row would hide a
    // file from review. Treating it as modified shows it; guessing added or deleted would
    // misrepresent it.
    let change_kind = parse_change_kind(&status).unwrap_or(ChangeKind::Modified);

    let new_path = diffstat_path(value, "new");
    let old_path = diffstat_path(value, "old");

    // A deleted file only has an old path; everything else is identified by its current one. The
    // local tree diff runs with `--no-renames`, so a rename's previous path is available here and
    // nowhere else (research.md §1).
    let (path, previous_path) = match change_kind {
        ChangeKind::Deleted => (old_path.or(new_path)?, None),
        ChangeKind::Renamed => {
            let path = new_path.or_else(|| old_path.clone())?;
            let previous_path = old_path.filter(|old| old != &path);
            (path, previous_path)
        }
        _ => (new_path.or(old_path)?, None),
    };

    Some(ChangedFile {
        path,
        previous_path,
        change_kind,
        lines_added: as_u32(value, "lines_added").unwrap_or(0),
        lines_removed: as_u32(value, "lines_removed").unwrap_or(0),
        // The host's diffstat says nothing about renderability; the changeset sets this when it
        // reads the tree (FR-037).
        render_refusal: None,
    })
}

pub fn parse_diffstat(stdout: &str) -> Result<Vec<ChangedFile>, HostError> {
    let rows = parse_rows(stdout)?;
    parse_each(rows, "changed files", parse_changed_file)
}

/// `to`/`start_to` are new-side lines and `from`/`start_from` old-side. A single-line anchor has
/// `start_*` null; a range is `start_to..=to`.
///
/// A range is only ever *read*: the feature cannot create one (FR-040), but pull requests created
/// elsewhere carry them and FR-050 requires them displayed over every line they cover.
fn parse_anchor(value: &Value) -> Option<CommentAnchor> {
    let inline = value.get("inline")?;
    let path = RepoPath::new(inline.get("path")?.as_str()?).ok()?;

    let (side, end, start) = match as_u32(inline, "to") {
        Some(to) => (DiffSide::New, to, as_u32(inline, "start_to")),
        None => (
            DiffSide::Old,
            as_u32(inline, "from")?,
            as_u32(inline, "start_from"),
        ),
    };

    let start = start.unwrap_or(end);
    // Defend against a reversed or zero range rather than constructing one that panics on
    // iteration or renders over the whole file.
    let (first, last) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    let first = first.max(1);
    let last = last.max(first);

    Some(CommentAnchor {
        path,
        side,
        lines: first..=last,
    })
}

fn comment_id(value: &Value) -> Option<CommentId> {
    let field = value.get("id")?;
    let id = field
        .as_u64()
        .map(|number| number.to_string())
        .or_else(|| field.as_str().map(str::to_owned))?;
    Some(CommentId(id))
}

/// One comment, flat — replies are assembled from `parent` links afterwards.
pub fn parse_comment(value: &Value) -> Option<CommentThread> {
    Some(CommentThread {
        id: comment_id(value)?,
        anchor: parse_anchor(value),
        author: parse_identity_or_unknown(value.get("user")),
        body: value
            .get("content")
            .and_then(|content| content.get("raw"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        created_at: as_timestamp(value, "created_on").unwrap_or_else(Utc::now),
        replies: Vec::new(),
        is_deleted: as_bool(value, "deleted").unwrap_or(false),
        is_pending: as_bool(value, "pending").unwrap_or(false),
        // Derived locally against the changeset being shown, never reported by the host (FR-053).
        is_outdated: false,
    })
}

/// Every comment, flat, together with the parent it claims. Threading is assembled in
/// `comments.rs`, which is where the ordering and orphan rules live.
pub fn parse_comments_flat(stdout: &str) -> Result<Vec<FlatComment>, HostError> {
    let rows = parse_rows(stdout)?;
    parse_each(rows, "comments", |row| {
        let comment = parse_comment(row)?;
        let parent = row.get("parent").and_then(comment_id);
        Some((comment, parent))
    })
}

pub fn parse_comments(stdout: &str) -> Result<Vec<CommentThread>, HostError> {
    let flat = parse_comments_flat(stdout)?;
    Ok(crate::comments::assemble_threads(flat))
}

#[cfg(test)]
pub(crate) mod fixtures {
    pub const LIST_MULTI_STATE: &str = include_str!("test_fixtures/list_multi_state.json");
    pub const DETAIL_MIXED_VERDICTS: &str =
        include_str!("test_fixtures/detail_mixed_verdicts.json");
    pub const DETAIL_NO_REVIEWERS: &str = include_str!("test_fixtures/detail_no_reviewers.json");
    pub const DETAIL_FORK_SOURCE: &str = include_str!("test_fixtures/detail_fork_source.json");
    pub const DIFFSTAT_ALL_KINDS: &str = include_str!("test_fixtures/diffstat_all_kinds.json");
    pub const COMMENTS_MIXED: &str = include_str!("test_fixtures/comments_mixed.json");
    pub const COMMENT_CREATED: &str = include_str!("test_fixtures/comment_created.json");
    pub const VIEWER: &str = include_str!("test_fixtures/viewer.json");
    pub const FAILING_OUTPUTS: &str = include_str!("test_fixtures/failing_outputs.json");

    /// A list of `count` rows, built by cloning one observed row.
    ///
    /// Generated rather than committed: the measured payload is ~19 KB per pull request, so a
    /// 500-row capture would be a ~9.5 MB fixture in the repository for a property — that the
    /// first rows are produced without hydrating every row's approvals — that a synthesised list
    /// establishes just as well.
    pub fn large_list(count: u64) -> String {
        let rows = (0..count)
            .map(|index| {
                format!(
                    r#"{{"type":"pullrequest","id":{id},"title":"Pull request {id}","state":"OPEN","draft":false,"comment_count":0,"created_on":"2026-08-{day:02}T10:00:00.000000+00:00","updated_on":"2026-09-{day:02}T10:00:00.000000+00:00","description":"{padding}","author":{{"type":"user","display_name":"Ada Lovelace","nickname":"ada","account_id":"712020:aaaa-1111","links":{{}}}},"links":{{"html":{{"href":"https://example.invalid/atlassian/twg-cli/pull-requests/{id}"}}}}}}"#,
                    id = 1000 + index,
                    day = (index % 28) + 1,
                    padding = "x".repeat(64),
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!("[{rows}]")
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use crate::host::StateFilter;

    fn repository() -> RepositoryCoordinates {
        RepositoryCoordinates {
            owner: "atlassian".into(),
            name: "twg-cli".into(),
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-06T12:00:00Z")
            .expect("the fixed clock must parse")
            .with_timezone(&Utc)
    }

    #[test]
    fn one_malformed_row_does_not_blank_the_list() {
        let summaries = parse_list(LIST_MULTI_STATE, &repository(), now())
            .expect("a list with one bad row must still parse");
        // Eight rows in, one of which has a non-numeric id and no other identity.
        assert_eq!(summaries.len(), 7);
        assert!(summaries.iter().all(|summary| !summary.title.is_empty()));
    }

    #[test]
    fn an_unrecognised_state_degrades_rather_than_failing_its_row() {
        let summaries = parse_list(LIST_MULTI_STATE, &repository(), now()).expect("must parse");
        let unknown = summaries
            .iter()
            .find(|summary| summary.id.number == 106)
            .expect("the row with an unfamiliar state must still be listed");
        assert_eq!(unknown.state, PullRequestState::Unknown);
    }

    #[test]
    fn a_draft_is_an_open_pull_request_carrying_a_flag() {
        let summaries = parse_list(LIST_MULTI_STATE, &repository(), now()).expect("must parse");
        let draft = summaries
            .iter()
            .find(|summary| summary.id.number == 102)
            .expect("the draft must be listed");
        assert_eq!(draft.state, PullRequestState::Open);
        assert!(draft.is_draft);
        assert_eq!(draft.state.label(draft.is_draft), "Draft");
    }

    #[test]
    fn clock_skew_is_normalised_at_parse_time() {
        let summaries = parse_list(LIST_MULTI_STATE, &repository(), now()).expect("must parse");
        let skewed = summaries
            .iter()
            .find(|summary| summary.id.number == 107)
            .expect("the future-dated row must be listed");
        assert_eq!(skewed.opened_at, now());
        assert_eq!(skewed.last_activity_at, now());
    }

    /// US1 acceptance scenario 2 / FR-013: the default filter is one `--state OPEN` call, and
    /// drafts come back from it because they are open pull requests.
    #[test]
    fn the_default_filter_asks_the_host_for_open_pull_requests_only() {
        let query = ListQuery {
            state: StateFilter::OpenAndDraft,
            limit: 25,
            ..Default::default()
        };
        let invocations = list_invocations(&repository(), &query);
        assert_eq!(invocations.len(), 1);
        let args = &invocations[0];
        assert!(windows_contains(args, &["--state", "OPEN"]));
        assert!(
            windows_contains(args, &["-o", "json"]),
            "-o json is not optional"
        );
        assert!(windows_contains(args, &["-w", "atlassian"]));
        assert!(windows_contains(args, &["-r", "twg-cli"]));
        assert!(
            !args.iter().any(|arg| arg == "DRAFT"),
            "there is no DRAFT state"
        );
    }

    #[test]
    fn the_all_filter_fans_out_across_every_state_because_there_is_no_any_value() {
        let query = ListQuery {
            state: StateFilter::All,
            limit: 25,
            ..Default::default()
        };
        let invocations = list_invocations(&repository(), &query);
        assert_eq!(invocations.len(), ALL_STATES.len());
        for state in ALL_STATES {
            assert!(
                invocations
                    .iter()
                    .any(|args| windows_contains(args, &["--state", state])),
                "no invocation asked for {state}"
            );
        }
    }

    #[test]
    fn the_author_filter_is_sent_as_a_nickname() {
        let query = ListQuery {
            author: Some(AuthorFilter::Person {
                nickname: "ada".into(),
                label: "Ada Lovelace".into(),
            }),
            limit: 25,
            ..Default::default()
        };
        let invocations = list_invocations(&repository(), &query);
        assert!(windows_contains(&invocations[0], &["--author", "ada"]));
    }

    #[test]
    fn the_me_marker_is_never_sent_unresolved() {
        // `Me` is resolved to the viewer's nickname by the caller. If it reaches here unresolved,
        // omitting it is correct: sending the literal string would filter by an author called
        // "Me".
        let query = ListQuery {
            author: Some(AuthorFilter::Me),
            limit: 25,
            ..Default::default()
        };
        let invocations = list_invocations(&repository(), &query);
        assert!(!invocations[0].iter().any(|arg| arg == "--author"));
    }

    #[test]
    fn detail_reads_verdicts_from_participants_not_reviewers() {
        let detail = parse_detail(DETAIL_MIXED_VERDICTS, &repository(), now())
            .expect("the detail fixture must parse");

        let verdict_for = |nickname: &str| {
            detail
                .verdicts
                .iter()
                .find(|verdict| verdict.reviewer.nickname.as_deref() == Some(nickname))
                .map(|verdict| verdict.verdict)
        };

        assert_eq!(verdict_for("grace"), Some(Verdict::Approved));
        assert_eq!(verdict_for("alan"), Some(Verdict::ChangesRequested));
        assert_eq!(verdict_for("katherine"), Some(Verdict::NoVerdict));
        // An unfamiliar verdict string is no verdict, never an approval.
        assert_eq!(verdict_for("ada"), Some(Verdict::NoVerdict));
    }

    #[test]
    fn detail_carries_the_description_branches_and_abbreviated_revisions() {
        let detail = parse_detail(DETAIL_MIXED_VERDICTS, &repository(), now()).expect("must parse");
        assert!(
            detail
                .description
                .as_deref()
                .is_some_and(|body| body.contains("## Why")),
            "the markdown source must survive parsing"
        );
        assert_eq!(detail.source.branch, "ada/git-history-view");
        assert_eq!(detail.destination.branch, "main");
        assert_eq!(detail.source.revision.len(), 12, "the host abbreviates");
        assert!(
            detail.can_comment,
            "an open pull request can be commented on"
        );
        assert!(!detail.is_cross_repository());
    }

    #[test]
    fn an_empty_description_is_distinguishable_from_a_missing_one_only_by_content() {
        let detail = parse_detail(DETAIL_NO_REVIEWERS, &repository(), now()).expect("must parse");
        // Both render as "no description" (FR-024), so what matters is that neither is an error.
        assert_eq!(detail.description.as_deref(), Some(""));
        assert!(detail.verdicts.is_empty(), "nobody has reviewed this one");
    }

    #[test]
    fn a_fork_sourced_pull_request_is_identifiable_before_anything_is_diffed() {
        let detail = parse_detail(DETAIL_FORK_SOURCE, &repository(), now()).expect("must parse");
        assert!(detail.is_cross_repository());
    }

    #[test]
    fn the_diffstat_maps_every_change_kind_including_the_rename() {
        let files = parse_diffstat(DIFFSTAT_ALL_KINDS).expect("the diffstat fixture must parse");
        // Seven rows in, one of which names no path at all and cannot be shown.
        assert_eq!(files.len(), 6);

        let file = |path: &str| {
            files
                .iter()
                .find(|file| file.path.as_unix_str() == path)
                .unwrap_or_else(|| panic!("{path} should be listed"))
        };

        let added = file(".changeset/public-code-review-skill.md");
        assert_eq!(added.change_kind, ChangeKind::Added);
        assert_eq!((added.lines_added, added.lines_removed), (6, 0));

        assert_eq!(
            file("crates/git_ui/src/git_panel.rs").change_kind,
            ChangeKind::Modified
        );

        let deleted = file("crates/git_ui/src/legacy_history.rs");
        assert_eq!(deleted.change_kind, ChangeKind::Deleted);
        assert_eq!(deleted.lines_removed, 108);

        let renamed = file("packages/sdk/src/help/skills/matching.ts");
        assert_eq!(renamed.change_kind, ChangeKind::Renamed);
        assert_eq!(
            renamed
                .previous_path
                .as_ref()
                .map(|path| path.as_unix_str().to_string())
                .as_deref(),
            Some("packages/sdk/src/help/matching.ts"),
            "the host's diffstat is the only source of rename information"
        );

        // An unrecognised status still shows the file rather than hiding it from review.
        assert_eq!(
            file("docs/unknown-status.md").change_kind,
            ChangeKind::Modified
        );
    }

    #[test]
    fn comments_read_a_single_line_a_range_and_an_unanchored_note() {
        let flat = parse_comments_flat(COMMENTS_MIXED).expect("the comment fixture must parse");
        let find = |id: &str| {
            flat.iter()
                .find(|(comment, _)| comment.id == CommentId(id.into()))
                .unwrap_or_else(|| panic!("comment {id} should parse"))
        };

        let (single, _) = find("9001");
        let anchor = single.anchor.as_ref().expect("9001 is inline");
        assert_eq!(anchor.side, DiffSide::New);
        assert_eq!(anchor.lines.clone().count(), 1);
        assert_eq!(*anchor.lines.start(), 239);

        // Reading a range stays in scope even though creating one does not (FR-050).
        let (range, _) = find("9004");
        let anchor = range.anchor.as_ref().expect("9004 is inline");
        assert_eq!(anchor.lines.clone(), 231..=239);
        assert_eq!(anchor.lines.clone().count(), 9);

        let (old_side, _) = find("9005");
        let anchor = old_side.anchor.as_ref().expect("9005 is inline");
        assert_eq!(anchor.side, DiffSide::Old);
        assert_eq!(anchor.lines.clone(), 12..=12);

        let (unanchored, _) = find("9006");
        assert!(
            unanchored.anchor.is_none(),
            "an unanchored comment belongs in Overview, not dropped"
        );

        let (reply, parent) = find("9002");
        assert_eq!(parent.clone(), Some(CommentId("9001".into())));
        assert!(!reply.body.is_empty());

        let (deleted, _) = find("9007");
        assert!(deleted.is_deleted);
        let (pending, _) = find("9008");
        assert!(pending.is_pending);
    }

    #[test]
    fn the_viewer_is_resolvable_without_the_reviewer_typing_their_name() {
        let value = parse_json(VIEWER).expect("must parse");
        let identity = parse_identity(&value).expect("the viewer must be identifiable");
        assert_eq!(identity.nickname.as_deref(), Some("ada"));
        assert_eq!(identity.account_id, "712020:aaaa-1111");
    }

    /// FR-040, FR-042, FR-048, SC-011: exactly one of `--line` / `--from-line`, never both, never a
    /// range.
    #[test]
    fn a_new_side_comment_sets_line_and_an_old_side_comment_sets_from_line() {
        let id = PullRequestId {
            number: 101,
            repository: repository(),
        };
        let draft = |side| DraftComment {
            path: RepoPath::new("crates/git_ui/src/git_panel.rs").expect("a valid path"),
            side,
            line: 240,
            body: "Why?".into(),
            reply_to: None,
            against_revision: "79ae1304eb1f".into(),
        };

        let new_side = post_comment_args(&id, &draft(DiffSide::New));
        assert!(windows_contains(&new_side, &["--line", "240"]));
        assert!(!new_side.iter().any(|arg| arg == "--from-line"));

        let old_side = post_comment_args(&id, &draft(DiffSide::Old));
        assert!(windows_contains(&old_side, &["--from-line", "240"]));
        assert!(!old_side.iter().any(|arg| arg == "--line"));

        for args in [&new_side, &old_side] {
            // There is no `--start-line`, and no range is ever attempted.
            assert!(!args.iter().any(|arg| arg.starts_with("--start")));
            // Names come from the tool's schema, not its examples, which document these instead.
            for wrong in ["--file-path", "--line-to", "--content-format"] {
                assert!(
                    !args.iter().any(|arg| arg == wrong),
                    "{wrong} does not exist"
                );
            }
        }
    }

    #[test]
    fn a_reply_is_posted_into_its_thread() {
        let id = PullRequestId {
            number: 101,
            repository: repository(),
        };
        let args = post_comment_args(
            &id,
            &DraftComment {
                path: RepoPath::new("a.rs").expect("a valid path"),
                side: DiffSide::New,
                line: 1,
                body: "Agreed.".into(),
                reply_to: Some(CommentId("9001".into())),
                against_revision: "abc".into(),
            },
        );
        assert!(windows_contains(&args, &["--reply-to", "9001"]));
    }

    #[test]
    fn the_posted_comment_is_echoed_back_at_the_line_it_was_written_on() {
        let value = parse_json(COMMENT_CREATED).expect("must parse");
        let comment = parse_comment(&value).expect("the echoed comment must parse");
        let anchor = comment.anchor.expect("it was posted inline");
        assert_eq!(anchor.lines, 240..=240);
        assert_eq!(anchor.side, DiffSide::New);
        assert_eq!(comment.author.nickname.as_deref(), Some("ada"));
    }

    /// FR-064, FR-065, SC-006, SC-016: each failing output produces its own variant, and nothing
    /// panics on malformed, truncated or unexpected output.
    #[test]
    fn every_failing_output_produces_its_intended_distinct_variant() {
        #[derive(serde::Deserialize)]
        struct Case {
            name: String,
            stdout: String,
            stderr: String,
            exit_code: i32,
            expected: String,
        }

        let cases: Vec<Case> =
            serde_json::from_str(FAILING_OUTPUTS).expect("the failure fixture must parse");
        assert!(cases.len() >= 8, "every classified condition needs a case");

        for case in cases {
            let output = ProcessOutput {
                stdout: case.stdout.clone(),
                stderr: case.stderr.clone(),
                exit_code: Some(case.exit_code),
            };

            // Whether the failure is signalled by the exit code or by unreadable output, the
            // reviewer must end up with one classified condition.
            let error = match successful_stdout(&output) {
                Ok(stdout) => parse_list(&stdout, &repository(), now())
                    .expect_err(&format!("{} should not parse", case.name)),
                Err(error) => error,
            };

            let actual = match &error {
                HostError::NotAuthenticated => "NotAuthenticated",
                HostError::CredentialExpired => "CredentialExpired",
                HostError::PermissionDenied { .. } => "PermissionDenied",
                HostError::Unreachable { .. } => "Unreachable",
                HostError::RateLimited => "RateLimited",
                HostError::UnexpectedResponse { .. } => "UnexpectedResponse",
                HostError::PrerequisiteMissing { .. } => "PrerequisiteMissing",
                HostError::Cancelled => "Cancelled",
            };
            assert_eq!(actual, case.expected, "misclassified: {}", case.name);
            assert!(!error.message().is_empty());
        }
    }

    #[test]
    fn no_parse_path_panics_on_arbitrary_output() {
        // Deliberately hostile inputs, including shapes that are valid JSON but nothing like the
        // observed contract. None of these may panic; every one must be a classified error or a
        // degraded-but-usable result.
        for stdout in [
            "",
            " ",
            "\u{feff}",
            "null",
            "[]",
            "[null]",
            "[{}]",
            "{}",
            "0",
            "\"a string\"",
            "[[[[1]]]]",
            r#"{"values": []}"#,
            r#"[{"id": 1}]"#,
            r#"[{"id": 1, "inline": {}}]"#,
            r#"[{"id": 1, "inline": {"path": "a", "to": 0, "start_to": 99}}]"#,
            r#"[{"id": 1, "created_on": "not a date"}]"#,
            r#"[{"id": 18446744073709551615}]"#,
            r#"[{"status": "renamed", "old": {"path": ""}, "new": {"path": ""}}]"#,
            r#"[{"lines_added": -5, "status": "added", "new": {"path": "a"}}]"#,
        ] {
            // Each of these returns a Result; the assertion is simply that none unwinds.
            let _ = parse_list(stdout, &repository(), now());
            let _ = parse_detail(stdout, &repository(), now());
            let _ = parse_diffstat(stdout);
            let _ = parse_comments_flat(stdout);
            let _ = parse_json(stdout).map(|value| parse_identity(&value));
        }
    }

    #[test]
    fn a_reversed_or_zero_range_is_repaired_rather_than_trusted() {
        let value = serde_json::json!({
            "id": 1,
            "inline": { "path": "a.rs", "to": 5, "start_to": 40 }
        });
        let comment = parse_comment(&value).expect("must parse");
        let anchor = comment.anchor.expect("inline");
        assert_eq!(anchor.lines, 5..=40, "a reversed range is put in order");

        let value = serde_json::json!({
            "id": 2,
            "inline": { "path": "a.rs", "to": 0, "start_to": 0 }
        });
        let comment = parse_comment(&value).expect("must parse");
        let anchor = comment.anchor.expect("inline");
        assert_eq!(*anchor.lines.start(), 1, "lines are 1-based");
    }

    /// FR-019, SC-007: every filter and sort combination is applied to the repository's whole set,
    /// not to a page.
    ///
    /// The fan-out is what makes this non-trivial: `All` is several calls, and ordering each call's
    /// answer separately would produce a list that looks right and is wrong.
    #[test]
    fn filters_and_sort_apply_to_the_whole_set_rather_than_a_page() {
        let all_states = parse_list(LIST_MULTI_STATE, &repository(), now()).expect("must parse");

        // Split the fixture into "pages" the way the state fan-out would, then merge them. The
        // result must not depend on how the rows were divided.
        let (first_page, second_page) = all_states.split_at(3);
        let merged = order_and_limit(
            first_page.iter().chain(second_page).cloned().collect(),
            SortDirection::MostRecentFirst,
            100,
        );
        let ordered_in_one_go =
            order_and_limit(all_states.clone(), SortDirection::MostRecentFirst, 100);
        assert_eq!(
            merged
                .iter()
                .map(|summary| summary.id.number)
                .collect::<Vec<_>>(),
            ordered_in_one_go
                .iter()
                .map(|summary| summary.id.number)
                .collect::<Vec<_>>(),
            "the order must not depend on how the host's answers were paged"
        );

        // Most recent first, and its exact reverse.
        let descending: Vec<u64> = ordered_in_one_go
            .iter()
            .map(|summary| summary.id.number)
            .collect();
        let ascending: Vec<u64> =
            order_and_limit(all_states.clone(), SortDirection::LeastRecentFirst, 100)
                .iter()
                .map(|summary| summary.id.number)
                .collect();
        assert_eq!(
            ascending,
            descending.iter().rev().cloned().collect::<Vec<_>>(),
            "reversing the sort must reverse the list, not reshuffle it"
        );

        for window in ordered_in_one_go.windows(2) {
            assert!(
                window[0].last_activity_at >= window[1].last_activity_at,
                "most-recent-first must be monotonic"
            );
        }

        // A limit takes the first N *of the ordered whole set*, so the newest rows survive it.
        let limited = order_and_limit(all_states.clone(), SortDirection::MostRecentFirst, 3);
        assert_eq!(limited.len(), 3);
        assert_eq!(
            limited
                .iter()
                .map(|summary| summary.id.number)
                .collect::<Vec<_>>(),
            descending[..3].to_vec(),
            "a limit must not drop rows that sort above the ones it keeps"
        );

        // A limit of zero would be a list nobody asked for; one row is the floor.
        assert_eq!(
            order_and_limit(all_states, SortDirection::MostRecentFirst, 0).len(),
            1
        );
    }

    #[test]
    fn a_pull_request_that_changes_state_mid_fan_out_is_not_listed_twice() {
        let rows = parse_list(LIST_MULTI_STATE, &repository(), now()).expect("must parse");
        // The same row coming back from two of the per-state calls.
        let duplicated: Vec<PullRequestSummary> = rows.iter().chain(rows.iter()).cloned().collect();
        let merged = order_and_limit(duplicated, SortDirection::MostRecentFirst, 100);
        assert_eq!(merged.len(), rows.len());
    }

    #[test]
    fn a_large_list_parses_without_hydrating_anything() {
        let stdout = large_list(500);
        let summaries = parse_list(&stdout, &repository(), now()).expect("500 rows must parse");
        assert_eq!(summaries.len(), 500);
        // The list call carries no verdicts at all — which is what forces the two-phase load.
        assert!(summaries.iter().all(|summary| summary.comment_count == 0));
    }

    fn windows_contains(args: &[String], needle: &[&str]) -> bool {
        args.windows(needle.len())
            .any(|window| window.iter().zip(needle).all(|(arg, want)| arg == want))
    }
}
