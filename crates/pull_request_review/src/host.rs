//! The pull request host boundary.
//!
//! Nothing in this file may name a host, a platform or a transport (spec FR-057). The panel, the
//! list, the tabs, the file list, the diff surface and commenting are all expressed in the
//! vocabulary declared here, which is what lets a second source be added without touching them.

use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use git::repository::RepoPath;
use gpui::{App, SharedString, Task};
use url::Url;

use crate::changeset::ChangedFile;

/// Identity of one pull request within one repository.
///
/// Both fields participate in equality, which is what lets the panel keep a selection across a
/// refresh that reorders the list (FR-011).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PullRequestId {
    pub number: u64,
    pub repository: RepositoryCoordinates,
}

/// How the open project's repository identifies itself to the host. Identity only, never content
/// (FR-072).
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RepositoryCoordinates {
    pub owner: String,
    pub name: String,
}

impl std::fmt::Display for RepositoryCoordinates {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", self.owner, self.name)
    }
}

/// A person, as the host reports them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    pub display_name: Option<String>,
    pub nickname: Option<String>,
    pub account_id: String,
    pub avatar_url: Option<Url>,
}

impl Identity {
    /// What to show in a row or a bubble. Never empty: with neither a display name nor a nickname
    /// this falls back to the account id, so a person is never rendered blank (FR-010).
    pub fn label(&self) -> SharedString {
        if let Some(display_name) = self
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            return display_name.to_string().into();
        }
        if let Some(nickname) = self
            .nickname
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            return nickname.to_string().into();
        }
        if self.account_id.is_empty() {
            return "Unknown".into();
        }
        self.account_id.clone().into()
    }

    /// A stable single-character avatar fallback derived from whatever identity we do have, so an
    /// account with neither a display name nor an avatar still renders something (FR-010).
    pub fn initial(&self) -> SharedString {
        let source = self.label();
        source
            .chars()
            .find(|character| character.is_alphanumeric())
            .map(|character| character.to_uppercase().to_string())
            .unwrap_or_else(|| "?".to_string())
            .into()
    }
}

/// Draft is deliberately not a variant here: the host reports it as a flag on an otherwise open
/// pull request, and modelling it as a fifth state would make "every open pull request" awkward to
/// express. `Unknown` exists so one unfamiliar state string cannot blank the list (FR-065).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullRequestState {
    Open,
    Merged,
    Declined,
    Superseded,
    Unknown,
}

impl PullRequestState {
    pub fn label(self, is_draft: bool) -> &'static str {
        match self {
            PullRequestState::Open if is_draft => "Draft",
            PullRequestState::Open => "Open",
            PullRequestState::Merged => "Merged",
            PullRequestState::Declined => "Declined",
            PullRequestState::Superseded => "Superseded",
            PullRequestState::Unknown => "Unknown",
        }
    }
}

/// One list row (FR-006 – FR-010). Everything needed to render a row without a second call
/// *except* approvals, which the host's list call does not carry — see [`ReviewerVerdict`].
#[derive(Clone, Debug)]
pub struct PullRequestSummary {
    pub id: PullRequestId,
    pub title: String,
    pub author: Identity,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub opened_at: DateTime<Utc>,
    pub last_activity_at: DateTime<Utc>,
    pub web_url: Option<Url>,
    pub comment_count: u32,
}

/// Normalise the two timestamps at parse time so every consumer sees the same value, rather than
/// leaving each render site to defend itself.
///
/// A future `opened_at` is clock skew between the host and this machine; it must read as *just
/// opened* rather than as a negative age. A missing `last_activity_at` falls back to `opened_at`,
/// because falling back to the epoch would sort the row to the wrong end of the list.
pub fn normalize_timestamps(
    opened_at: Option<DateTime<Utc>>,
    last_activity_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    let opened_at = opened_at.unwrap_or(now).min(now);
    let last_activity_at = last_activity_at.unwrap_or(opened_at).min(now);
    (opened_at, last_activity_at.max(opened_at))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchRef {
    pub branch: String,
    /// May be abbreviated by the host, so it is resolved to a full object id before use as a git
    /// argument. An unresolvable revision is the "the ref is gone" condition, not a crash.
    pub revision: String,
    pub repository: RepositoryCoordinates,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Approved,
    ChangesRequested,
    NoVerdict,
}

/// One reviewer's position (FR-007, FR-008). Not available from the list call; this arrives in the
/// second phase of the two-phase load.
#[derive(Clone, Debug)]
pub struct ReviewerVerdict {
    pub reviewer: Identity,
    pub verdict: Verdict,
    pub at: Option<DateTime<Utc>>,
}

/// The Overview tab (FR-023, FR-024).
#[derive(Clone, Debug)]
pub struct PullRequestDetail {
    pub summary: PullRequestSummary,
    /// Markdown source. `None` and `Some("")` both render as "no description" (FR-024).
    pub description: Option<String>,
    pub source: BranchRef,
    pub destination: BranchRef,
    pub verdicts: Vec<ReviewerVerdict>,
    /// Checked *before* composing, so the reviewer is told up front rather than after typing
    /// (FR-047).
    pub can_comment: bool,
}

impl PullRequestDetail {
    /// True when the pull request's source is a different repository from its destination. This
    /// phase reports such a pull request as unsupported rather than diffing the wrong changeset.
    pub fn is_cross_repository(&self) -> bool {
        self.source.repository != self.destination.repository
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CommentId(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffSide {
    Old,
    New,
}

/// Where a comment read back from the host is attached.
///
/// `lines` stays a range even though the feature only ever *creates* single-line comments: pull
/// requests created elsewhere carry ranges and FR-050 requires them displayed over every line they
/// cover. Creating is constrained by [`DraftComment`], not by narrowing this type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentAnchor {
    pub path: RepoPath,
    pub side: DiffSide,
    pub lines: std::ops::RangeInclusive<u32>,
}

/// One conversation (FR-050 – FR-055).
///
/// There is deliberately no resolution field. Resolving threads is out of scope (FR-054), so the
/// model carries no state it cannot determine and the UI has nothing stale to show.
#[derive(Clone, Debug)]
pub struct CommentThread {
    pub id: CommentId,
    /// `None` means the comment is not attached to a line, so it belongs in the Overview tab
    /// rather than being dropped (FR-052).
    pub anchor: Option<CommentAnchor>,
    pub author: Identity,
    pub body: String,
    pub created_at: DateTime<Utc>,
    /// Oldest first, built from the host's parent links.
    pub replies: Vec<CommentThread>,
    pub is_deleted: bool,
    pub is_pending: bool,
    /// Derived locally by comparing the anchor against the changeset actually being shown, never
    /// reported by the host (FR-053).
    pub is_outdated: bool,
}

/// One comment together with the parent it claims, before threads are assembled.
///
/// This is boundary vocabulary rather than an implementation detail: any source reports comments
/// flat with parent links, and assembling them into threads — with the ordering and orphan rules
/// that implies — is one decision that belongs above the seam rather than in each implementation.
pub type FlatComment = (CommentThread, Option<CommentId>);

/// A comment being composed. Exists only until submitted or cancelled, and is never persisted
/// (FR-004, FR-044).
///
/// This carries a single `line` rather than a [`CommentAnchor`] so the type system enforces FR-040
/// instead of leaving a range that some code path has to remember to collapse.
#[derive(Clone, Debug)]
pub struct DraftComment {
    pub path: RepoPath,
    pub side: DiffSide,
    pub line: u32,
    pub body: String,
    pub reply_to: Option<CommentId>,
    /// The revision the reviewer was reading, so the comment cannot be silently attached to a line
    /// it was not written about (FR-049).
    pub against_revision: String,
}

/// Distinct failure conditions, because FR-064 requires the reviewer to know which one to fix.
///
/// `PrerequisiteMissing` and the authentication variants must never be conflated: a tool the
/// reviewer can run in their own terminal being reported as "not installed" is the specific failure
/// FR-062b exists to prevent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostError {
    PrerequisiteMissing {
        detail: String,
    },
    NotAuthenticated,
    CredentialExpired,
    PermissionDenied {
        repository: String,
    },
    /// The host has no such repository. Distinct from `PermissionDenied`, because the remedies
    /// differ: a 403 means ask for access, a 404 means the repository is not where we looked — and
    /// distinct from `UnexpectedResponse`, whose remedy is to update the tool, which would never
    /// fix this.
    RepositoryNotFound {
        repository: String,
    },
    Unreachable {
        detail: String,
    },
    RateLimited,
    /// Carries the observed tool version where known, so a shape change is diagnosable rather than
    /// an opaque parse failure.
    UnexpectedResponse {
        detail: String,
        version: Option<String>,
    },
    Cancelled,
}

impl std::fmt::Display for HostError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl std::error::Error for HostError {}

impl HostError {
    /// The one-line statement of what is wrong. Every variant reads differently, because a shared
    /// message would defeat the purpose of having distinct variants (FR-064, SC-006).
    pub fn message(&self) -> String {
        match self {
            HostError::PrerequisiteMissing { detail } => {
                format!("The pull request tool isn't available: {detail}")
            }
            HostError::NotAuthenticated => "Not signed in to the pull request host.".to_string(),
            HostError::CredentialExpired => {
                "Your pull request host credentials have expired.".to_string()
            }
            HostError::PermissionDenied { repository } => {
                format!("You don't have access to {repository}.")
            }
            HostError::RepositoryNotFound { repository } => {
                format!("The pull request host has no repository {repository}.")
            }
            HostError::Unreachable { detail } => {
                format!("Couldn't reach the pull request host: {detail}")
            }
            HostError::RateLimited => {
                "The pull request host is rate limiting requests.".to_string()
            }
            HostError::UnexpectedResponse { detail, version } => match version {
                Some(version) => {
                    format!("Unexpected response from the pull request tool ({version}): {detail}")
                }
                None => format!("Unexpected response from the pull request tool: {detail}"),
            },
            HostError::Cancelled => "Cancelled.".to_string(),
        }
    }

    /// What the reviewer can actually do about it. `None` means there is nothing to suggest beyond
    /// retrying.
    pub fn remedy(&self) -> Option<&'static str> {
        match self {
            HostError::PrerequisiteMissing { .. } => {
                Some("Install it, or point the override environment variable at it.")
            }
            HostError::NotAuthenticated => Some("Sign in, then retry."),
            HostError::CredentialExpired => Some("Sign in again, then retry."),
            HostError::PermissionDenied { .. } => {
                Some("Ask for access to the repository, then retry.")
            }
            HostError::RepositoryNotFound { .. } => Some(
                "Check the repository still exists there, and that this project's remote points \
                 at it.",
            ),
            HostError::Unreachable { .. } => Some("Check your connection, then retry."),
            HostError::RateLimited => Some("Wait a moment, then retry."),
            HostError::UnexpectedResponse { .. } => Some("Updating the tool may resolve this."),
            HostError::Cancelled => None,
        }
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self, HostError::Cancelled)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum StateFilter {
    /// The default. Drafts are open pull requests, so they are included here rather than being a
    /// separate filter (FR-013).
    #[default]
    OpenAndDraft,
    All,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AuthorFilter {
    /// Stored as a marker rather than a resolved account id, so it keeps meaning if the reviewer's
    /// identity changes (FR-015).
    Me,
    Person {
        nickname: String,
        label: String,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SortDirection {
    #[default]
    MostRecentFirst,
    LeastRecentFirst,
}

impl SortDirection {
    pub fn reversed(self) -> Self {
        match self {
            SortDirection::MostRecentFirst => SortDirection::LeastRecentFirst,
            SortDirection::LeastRecentFirst => SortDirection::MostRecentFirst,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ListQuery {
    pub state: StateFilter,
    pub author: Option<AuthorFilter>,
    pub sort: SortDirection,
    pub limit: usize,
}

/// The single boundary through which every pull request host interaction passes.
///
/// Declared to discharge **spec FR-056**, which mandates this seam by number so that a second
/// platform — **GitHub** — can be added by supplying another implementation of this trait alone.
/// Per constitution Principle I this is a plain trait with no registry, no dynamic discovery and no
/// extension point; the citation is required where it is declared.
///
/// Every method is asynchronous because FR-067 forbids this work on the foreground thread, and
/// dropping a returned [`Task`] must stop the underlying work rather than merely discard its result
/// (FR-069).
///
/// The trait is `'static` but deliberately not `Send + Sync`. An implementation holds GPUI handles
/// and is only ever called from the foreground thread through an `&App`; requiring `Sync` would
/// force interior locking around handles that are already single-threaded by construction. The work
/// each method *performs* still runs off-thread — that is what the returned [`Task`] is for.
pub trait PullRequestHost: 'static {
    /// Applies `query` to the repository's whole set, not to a page the caller then filters
    /// (FR-019).
    fn list(
        &self,
        repository: &RepositoryCoordinates,
        query: ListQuery,
        cx: &App,
    ) -> Task<Result<Vec<PullRequestSummary>, HostError>>;

    fn detail(&self, id: &PullRequestId, cx: &App) -> Task<Result<PullRequestDetail, HostError>>;

    fn changed_files(
        &self,
        id: &PullRequestId,
        cx: &App,
    ) -> Task<Result<Vec<ChangedFile>, HostError>>;

    fn comments(&self, id: &PullRequestId, cx: &App)
    -> Task<Result<Vec<CommentThread>, HostError>>;

    /// The only call permitted to carry repository content, and only the one comment the reviewer
    /// submitted with its path and line (FR-072).
    fn post_comment(
        &self,
        id: &PullRequestId,
        draft: DraftComment,
        cx: &App,
    ) -> Task<Result<CommentThread, HostError>>;

    fn viewer(&self, cx: &App) -> Task<Result<Identity, HostError>>;
}

/// Why this project cannot be reviewed at all, as opposed to a request that failed.
///
/// These are stated reasons rather than an empty list, because an empty list is indistinguishable
/// from "this repository has no pull requests" (FR-005).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoordinatesError {
    NoRepository,
    NoRemote,
    UnsupportedRemote {
        url: String,
    },
    /// Several candidate repositories; the reviewer picks one rather than the feature guessing.
    SeveralRepositories {
        candidates: Vec<RepositoryCoordinates>,
    },
    RemoteProject,
}

impl CoordinatesError {
    pub fn message(&self) -> String {
        match self {
            CoordinatesError::NoRepository => {
                "This project has no git repository, so there are no pull requests to list."
                    .to_string()
            }
            CoordinatesError::NoRemote => {
                "This repository has no remote, so its pull requests can't be located.".to_string()
            }
            CoordinatesError::UnsupportedRemote { url } => {
                format!("The remote {url} isn't a supported pull request host.")
            }
            CoordinatesError::SeveralRepositories { candidates } => {
                let names = candidates
                    .iter()
                    .map(RepositoryCoordinates::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "This project maps to several repositories ({names}). Choose one to review."
                )
            }
            CoordinatesError::RemoteProject => {
                // Supporting this needs a new remote-server message, which would put two more
                // high-conflict files on the FR-078 allowlist for no in-phase benefit. The
                // constitution requires a surface that cannot work remotely to say so rather than
                // produce empty results.
                "Pull request review isn't available for remote projects yet, because the diff is \
                 read from a local git repository."
                    .to_string()
            }
        }
    }
}

/// A remote URL, taken apart.
///
/// The `host` is kept rather than discarded, because whether a remote can be reviewed at all
/// depends on *which* host it names — and only an implementation knows which hosts it speaks to.
/// Deciding that here would put a platform name above the boundary (FR-057).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteRepository {
    pub host: String,
    pub coordinates: RepositoryCoordinates,
}

/// Whether this looks like a hostname rather than a path component.
///
/// "Contains a dot" alone is not enough: `..` passes it, which would make `../sibling/project`
/// parse as a remote repository on a host called `..`.
fn is_hostname(candidate: &str) -> bool {
    !candidate.is_empty()
        && candidate.contains('.')
        && !candidate.starts_with('.')
        && !candidate.ends_with('.')
        && candidate.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '.' || character == '-'
        })
}

/// Take a remote URL apart into its host and its repository coordinates.
///
/// Recognises the `scp`-style and URL forms. This says nothing about whether the host is one the
/// feature can review — see [`coordinates_from_remotes`].
pub fn parse_remote_url(url: &str) -> Result<RemoteRepository, CoordinatesError> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(CoordinatesError::NoRemote);
    }

    let unsupported = || CoordinatesError::UnsupportedRemote {
        url: trimmed.to_string(),
    };

    // Strip the scheme and any credentials, leaving `host/path` or `host:path`.
    let (had_scheme, without_scheme) = match trimmed.split_once("://") {
        Some((_, rest)) => (true, rest),
        None => (false, trimmed),
    };
    let without_credentials = match without_scheme.split_once('@') {
        Some((_, rest)) => rest,
        None => without_scheme,
    };

    // The separator depends on the syntax, and getting this backwards is how a port ends up parsed
    // as the first path segment. With a scheme it is URL syntax, where a colon introduces a *port*
    // and the path starts at the first slash. Without one it is `scp` syntax, where the colon is
    // the host/path separator and there is no port at all.
    let (host_and_port, path) = if had_scheme {
        without_credentials
            .split_once('/')
            .ok_or_else(unsupported)?
    } else {
        match without_credentials.split_once(':') {
            Some((host, rest)) => (host, rest.trim_start_matches('/')),
            None => without_credentials
                .split_once('/')
                .ok_or_else(unsupported)?,
        }
    };

    let host = host_and_port.split(':').next().unwrap_or(host_and_port);
    if !is_hostname(host) {
        // Not a hostname means this is a local path, not a remote we can address.
        return Err(unsupported());
    }

    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);

    let mut segments = path.split('/').filter(|segment| !segment.is_empty());
    let owner = segments.next().ok_or_else(unsupported)?;
    let name = segments.next().ok_or_else(unsupported)?;
    if segments.next().is_some() {
        // A deeper path is not a repository this phase knows how to address.
        return Err(unsupported());
    }

    Ok(RemoteRepository {
        host: host.to_ascii_lowercase(),
        coordinates: RepositoryCoordinates {
            owner: owner.to_string(),
            name: name.to_string(),
        },
    })
}

/// Pick the coordinates for a project from the remotes it exposes.
///
/// `remote_urls` is given in the order the caller prefers, so the conventional default remote wins
/// when there is one. Several *distinct* repositories is reported rather than resolved, because
/// picking one silently would show the reviewer a plausible list from the wrong repository.
///
/// `is_supported_host` is supplied by the caller, because which hosts can be reviewed is the
/// implementation's business, not the boundary's. A remote whose host it rejects is reported as
/// unsupported **without the host ever being contacted** — the alternative is a doomed request
/// whose failure the reviewer then has to interpret.
pub fn coordinates_from_remotes(
    remote_urls: &[String],
    is_supported_host: impl Fn(&str) -> bool,
) -> Result<RepositoryCoordinates, CoordinatesError> {
    if remote_urls.is_empty() {
        return Err(CoordinatesError::NoRemote);
    }

    let mut resolved: Vec<RepositoryCoordinates> = Vec::new();
    let mut last_error = None;
    for url in remote_urls {
        match parse_remote_url(url) {
            Ok(remote) if is_supported_host(&remote.host) => {
                if !resolved.contains(&remote.coordinates) {
                    resolved.push(remote.coordinates);
                }
            }
            Ok(_) => {
                last_error = Some(CoordinatesError::UnsupportedRemote {
                    url: url.trim().to_string(),
                })
            }
            Err(error) => last_error = Some(error),
        }
    }

    match resolved.len() {
        0 => Err(last_error.unwrap_or(CoordinatesError::NoRemote)),
        1 => resolved
            .into_iter()
            .next()
            .ok_or(CoordinatesError::NoRemote),
        _ => Err(CoordinatesError::SeveralRepositories {
            candidates: resolved,
        }),
    }
}

pub type SharedHost = Arc<dyn PullRequestHost>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Anything is "supported" for the parsing tests; host support is the implementation's job and
    /// is tested where it lives.
    fn any_host(_host: &str) -> bool {
        true
    }

    #[test]
    fn remote_urls_resolve_to_coordinates() {
        for url in [
            "git@bitbucket.org:atlassian/twg-cli.git",
            "https://bitbucket.org/atlassian/twg-cli.git",
            "https://user@bitbucket.org/atlassian/twg-cli",
            "ssh://git@bitbucket.org/atlassian/twg-cli.git",
        ] {
            let remote = parse_remote_url(url)
                .unwrap_or_else(|error| panic!("{url} should parse, got {error:?}"));
            assert_eq!(
                remote.host, "bitbucket.org",
                "the host must be kept, not discarded"
            );
            assert_eq!(
                remote.coordinates,
                RepositoryCoordinates {
                    owner: "atlassian".into(),
                    name: "twg-cli".into(),
                },
                "failed for {url}"
            );
        }
    }

    #[test]
    fn unresolvable_remotes_are_named_rather_than_swallowed() {
        assert_eq!(parse_remote_url(""), Err(CoordinatesError::NoRemote));
        assert!(matches!(
            parse_remote_url("/srv/git/bare-repo.git"),
            Err(CoordinatesError::UnsupportedRemote { .. })
        ));
        assert!(matches!(
            parse_remote_url("https://example.com/a/b/c/d"),
            Err(CoordinatesError::UnsupportedRemote { .. })
        ));
    }

    #[test]
    fn several_distinct_repositories_is_a_stated_reason_not_a_guess() {
        let error = coordinates_from_remotes(
            &[
                "git@bitbucket.org:atlassian/one.git".into(),
                "git@bitbucket.org:atlassian/two.git".into(),
            ],
            any_host,
        )
        .expect_err("two repositories must not silently resolve to one");
        match error {
            CoordinatesError::SeveralRepositories { candidates } => {
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("expected SeveralRepositories, got {other:?}"),
        }
    }

    #[test]
    fn the_same_repository_via_several_remotes_is_not_ambiguous() {
        assert_eq!(
            coordinates_from_remotes(
                &[
                    "git@bitbucket.org:atlassian/twg-cli.git".into(),
                    "https://bitbucket.org/atlassian/twg-cli.git".into(),
                ],
                any_host,
            ),
            Ok(RepositoryCoordinates {
                owner: "atlassian".into(),
                name: "twg-cli".into(),
            })
        );
    }

    #[test]
    fn no_remotes_is_distinct_from_an_unsupported_one() {
        assert_eq!(
            coordinates_from_remotes(&[], any_host),
            Err(CoordinatesError::NoRemote)
        );
        // "no remote at all" and "a remote we cannot review" are different problems with
        // different remedies, so they must not collapse into one message (FR-005).
        assert_ne!(
            CoordinatesError::NoRemote.message(),
            CoordinatesError::UnsupportedRemote {
                url: "git@github.com:a/b.git".into()
            }
            .message()
        );
    }

    /// A remote on a host the implementation does not speak to must be reported as unsupported,
    /// **without the host being contacted**.
    ///
    /// This was a real bug, found by opening the panel on a GitHub-hosted checkout. The parser
    /// discarded the hostname, so `git@github.com:amalabey/zed.git` resolved to perfectly plausible
    /// coordinates, a Bitbucket lookup of that name was attempted, and the reviewer was shown
    /// "Updating the tool may resolve this" for a repository that simply is not there. The earlier
    /// tests only covered unparseable *shapes*, which is exactly why they passed.
    #[test]
    fn a_remote_on_an_unsupported_host_is_reported_rather_than_attempted() {
        let only_bitbucket = |host: &str| host == "bitbucket.org";

        for url in [
            "git@github.com:amalabey/zed.git",
            "https://github.com/zed-industries/zed.git",
            "git@gitlab.com:group/project.git",
        ] {
            // It parses — the shape is fine. It is the *host* that is not supported.
            let remote = parse_remote_url(url).expect("the shape is valid");
            assert!(!only_bitbucket(&remote.host), "{url}");

            match coordinates_from_remotes(&[url.to_string()], only_bitbucket) {
                Err(CoordinatesError::UnsupportedRemote { url: reported }) => {
                    assert_eq!(reported, url, "the message must name the remote");
                }
                other => panic!("{url} should be unsupported, got {other:?}"),
            }
        }

        assert_eq!(
            coordinates_from_remotes(
                &["git@bitbucket.org:atlassian/twg-cli.git".into()],
                only_bitbucket
            ),
            Ok(RepositoryCoordinates {
                owner: "atlassian".into(),
                name: "twg-cli".into(),
            })
        );
    }

    /// A project with both a supported and an unsupported remote is reviewable through the
    /// supported one, rather than refused because the other exists.
    #[test]
    fn a_supported_remote_wins_over_an_unsupported_sibling() {
        assert_eq!(
            coordinates_from_remotes(
                &[
                    "git@github.com:amalabey/zed.git".into(),
                    "git@bitbucket.org:atlassian/twg-cli.git".into(),
                ],
                |host| host == "bitbucket.org",
            ),
            Ok(RepositoryCoordinates {
                owner: "atlassian".into(),
                name: "twg-cli".into(),
            })
        );
    }

    #[test]
    fn a_host_with_a_port_stays_recognisable() {
        let remote = parse_remote_url("https://bitbucket.example.com:7999/team/project.git")
            .expect("a ported URL should parse");
        assert_eq!(remote.host, "bitbucket.example.com");
        assert_eq!(remote.coordinates.owner, "team");
        assert_eq!(remote.coordinates.name, "project");
    }

    #[test]
    fn a_local_path_is_not_mistaken_for_a_remote() {
        // No dot in the "host" position means this is a path, not a hostname.
        for url in [
            "/srv/git/project.git",
            "../sibling/project",
            "~/work/project",
        ] {
            assert!(
                matches!(
                    parse_remote_url(url),
                    Err(CoordinatesError::UnsupportedRemote { .. })
                ),
                "{url} should not resolve to a remote repository"
            );
        }
    }

    #[test]
    fn a_future_opened_at_reads_as_just_opened_rather_than_a_negative_age() {
        let now = Utc::now();
        let future = now + chrono::Duration::hours(3);
        let (opened_at, last_activity_at) = normalize_timestamps(Some(future), None, now);
        assert_eq!(opened_at, now);
        assert_eq!(last_activity_at, now);
    }

    #[test]
    fn a_missing_last_activity_falls_back_to_opened_at_not_the_epoch() {
        let now = Utc::now();
        let opened = now - chrono::Duration::days(4);
        let (opened_at, last_activity_at) = normalize_timestamps(Some(opened), None, now);
        assert_eq!(opened_at, opened);
        assert_eq!(last_activity_at, opened);
    }

    #[test]
    fn identity_never_renders_blank() {
        let bare = Identity {
            account_id: "712020:abc".into(),
            ..Default::default()
        };
        assert_eq!(bare.label(), "712020:abc");
        assert_eq!(bare.initial(), "7");

        let whitespace_only = Identity {
            display_name: Some("   ".into()),
            account_id: "zed".into(),
            ..Default::default()
        };
        assert_eq!(whitespace_only.label(), "zed");
        assert_eq!(whitespace_only.initial(), "Z");

        let nothing = Identity::default();
        assert_eq!(nothing.label(), "Unknown");
        assert_eq!(nothing.initial(), "U");
    }

    #[test]
    fn every_host_error_reads_differently() {
        let errors = [
            HostError::PrerequisiteMissing {
                detail: "not on PATH".into(),
            },
            HostError::NotAuthenticated,
            HostError::CredentialExpired,
            HostError::PermissionDenied {
                repository: "a/b".into(),
            },
            HostError::RepositoryNotFound {
                repository: "a/b".into(),
            },
            HostError::Unreachable {
                detail: "dns".into(),
            },
            HostError::RateLimited,
            HostError::UnexpectedResponse {
                detail: "trailing brace".into(),
                version: Some("1.0.1".into()),
            },
        ];
        let mut messages = errors.iter().map(HostError::message).collect::<Vec<_>>();
        messages.sort();
        messages.dedup();
        // SC-006 asks for seven induced conditions; there are eight classified ones, each with
        // its own actionable message. A 404 was originally missing and fell through to
        // UnexpectedResponse, whose remedy — update the tool — could never fix it.
        assert_eq!(messages.len(), 8);
        assert!(errors.iter().all(|error| error.remedy().is_some()));
    }
}
