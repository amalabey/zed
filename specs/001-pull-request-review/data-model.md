# Data Model: Pull Request Review

**Date**: 2026-09-06 | **Plan**: [plan.md](./plan.md)

Nothing here is persisted except [List View State](#list-view-state). Per spec FR-004 the feature stores
no review data: every other entity is in-memory, derived from the host or from git on demand, and
discarded when the panel closes.

All types live in `crates/pull_request_review`. Types owned by the FR-056 host boundary are defined in
`host.rs`; types owned by the FR-058 changeset boundary are defined in `changeset.rs`. Neither names a
platform, a transport or a tool, per FR-057.

---

## Host boundary types (`host.rs`)

### PullRequestId

Identity of one pull request within one repository.

| Field | Type | Notes |
|---|---|---|
| `number` | `u64` | The host's pull request number. Stable; used for all subsequent calls. |
| `repository` | `RepositoryCoordinates` | Which repository it belongs to. |

Two pull requests are the same if both fields match. This is the identity used to preserve selection
across a refresh (FR-011).

### RepositoryCoordinates

How the open project's git repository identifies itself to the host. Repository **identity only** — never
content (FR-072).

| Field | Type | Notes |
|---|---|---|
| `owner` | `String` | Workspace / organisation slug. |
| `name` | `String` | Repository slug. |

Derived from the git remote. When it cannot be derived, the panel reports that it cannot list pull
requests for this project and why (FR-005, edge case "no remote").

### PullRequestSummary

One list row (FR-006 – FR-010). Everything needed to render a row without a second call, *except*
approvals — see [ReviewerVerdict](#reviewerverdict).

| Field | Type | Notes |
|---|---|---|
| `id` | `PullRequestId` | |
| `title` | `String` | Elided in the row, never wrapped to force horizontal scroll (edge case). |
| `author` | `Identity` | |
| `state` | `PullRequestState` | |
| `is_draft` | `bool` | Distinct from `state`; the host reports draft as a flag on an open pull request. |
| `opened_at` | `DateTime<Utc>` | Drives the relative age in the row. |
| `last_activity_at` | `DateTime<Utc>` | Drives the recency sort (FR-017). |
| `web_url` | `Url` | The browser link (FR-023). |
| `comment_count` | `u32` | |

**Validation / normalisation rules**

- An `opened_at` in the future (host/machine clock skew) renders as *just opened*, never as a negative
  age (edge case). Normalise at parse time, not at render time, so every consumer sees the same value.
- `last_activity_at` missing falls back to `opened_at` rather than to the epoch, which would sort the row
  to the wrong end of the list.

### PullRequestState

`Open | Merged | Declined | Superseded`

The status indicator must distinguish at least open, draft, merged and declined (FR-006). Draft is
`state == Open && is_draft`, not a fifth variant — modelling it as a variant would make "all open pull
requests" awkward to express and would not match what the host reports.

An unrecognised state string parses to `Superseded`-adjacent *unknown* handling rather than failing the
whole row: one unfamiliar state must not blank the list (FR-065).

### Identity

A person, as the host reports them.

| Field | Type | Notes |
|---|---|---|
| `display_name` | `Option<String>` | |
| `nickname` | `Option<String>` | What the host's author filter matches on. |
| `account_id` | `String` | Stable identity; used to recognise "me" (FR-015). |
| `avatar_url` | `Option<Url>` | |

**Validation**: with neither `display_name` nor `avatar_url`, the bubble falls back to a stable derived
initial from `account_id` — never blank (FR-010).

### ReviewerVerdict

One reviewer's position on a pull request (FR-007, FR-008). **Not available from the list call** — arrives
in the second phase of the two-phase load (research.md §2).

| Field | Type | Notes |
|---|---|---|
| `reviewer` | `Identity` | |
| `verdict` | `Verdict` | |
| `at` | `Option<DateTime<Utc>>` | |

`Verdict` = `Approved | ChangesRequested | NoVerdict`

**Rules**

- `Approved` and `ChangesRequested` render distinctly; `ChangesRequested` is never shown as an approval
  (FR-007).
- A pull request with no `Approved` and no `ChangesRequested` verdicts renders an **empty** approval area,
  not a placeholder (FR-009).
- Beyond a bounded count, remaining reviewers collapse to a `+N` affordance with the full list on demand
  (FR-008).

### PullRequestDetail

The Overview tab (FR-023, FR-024).

| Field | Type | Notes |
|---|---|---|
| `summary` | `PullRequestSummary` | |
| `description` | `Option<String>` | Markdown source. `None` and `Some("")` both render as "no description" (FR-024). |
| `source` | `BranchRef` | |
| `destination` | `BranchRef` | |
| `verdicts` | `Vec<ReviewerVerdict>` | |
| `can_comment` | `bool` | False for merged/declined, or no permission — checked *before* composing (FR-047). |

### BranchRef

| Field | Type | Notes |
|---|---|---|
| `branch` | `String` | Branch name. |
| `revision` | `String` | Commit identifier. May be **abbreviated** by the host (12 chars observed). |
| `repository` | `RepositoryCoordinates` | Differs from the pull request's repository when the source is a fork. |

**Rules**

- Because `revision` may be abbreviated, it is resolved to a full object id locally before use as a git
  argument; an unresolvable revision is the "ref is gone from the host" condition (edge case).
- `source.repository != destination.repository` identifies a fork. This phase reports fork-sourced pull
  requests as unsupported rather than diffing the wrong thing (FR edge case: "never a silent diff of the
  wrong changeset").

### CommentThread

One conversation (FR-050 – FR-055).

| Field | Type | Notes |
|---|---|---|
| `id` | `CommentId` | |
| `anchor` | `Option<CommentAnchor>` | `None` = not attached to a line → shown in Overview (FR-052). |
| `author` | `Identity` | |
| `body` | `String` | Markdown source. |
| `created_at` | `DateTime<Utc>` | |
| `replies` | `Vec<CommentThread>` | Ordered oldest-first. Built from the host's parent links. |
| `is_deleted` | `bool` | |
| `is_pending` | `bool` | Not yet published by its author. |
| `is_outdated` | `bool` | **Derived locally**, not from the host — see rules. |

There is deliberately **no resolution field**. Resolving threads is out of scope (FR-054), so the model
carries no state it cannot determine and the UI has nothing stale to display. Replies are in scope and are
modelled by `replies`.

**Rules**

- `is_outdated` is computed by comparing the thread's anchor against the changeset actually being shown.
  A thread anchored to a line the pull request has since changed is marked outdated — never silently
  re-anchored to a different line, and never dropped (FR-053).
- Threads are collapsible so they cannot push code off screen (FR-054).
- Resolving or reopening a thread is not offered at all, rather than offered and silently ineffective
  (FR-054).

### CommentAnchor

| Field | Type | Notes |
|---|---|---|
| `path` | `RepoPath` | |
| `side` | `DiffSide` | |
| `lines` | `RangeInclusive<u32>` | 1-based, inclusive. A single line is `n..=n`. |

`DiffSide` = `Old | New`

**Reading and creating are asymmetric, deliberately.** An anchor *read* from the pull request may span
several lines, because pull requests created elsewhere carry ranges and FR-050 requires them to be
displayed over the lines they cover. An anchor the reviewer *creates* is always one line — see
[DraftComment](#draftcomment). So `lines` stays a range for fidelity when reading, and the create path
constrains it rather than the type doing so.

### DraftComment

A comment being composed. Exists only until submitted or cancelled; never persisted (FR-004, FR-044).

| Field | Type | Notes |
|---|---|---|
| `path` | `RepoPath` | |
| `side` | `DiffSide` | Exactly one side; ambiguity is refused, not guessed (FR-048). |
| `line` | `u32` | **One line, not a range** (FR-040, FR-042). |
| `body` | `String` | |
| `reply_to` | `Option<CommentId>` | Set when replying to a thread (FR-051). |
| `against_revision` | `String` | The revision the reviewer was reading. |

A draft carries a single `line` rather than a `CommentAnchor`, so the type system enforces FR-040 instead
of leaving a range that some code path has to remember to collapse.

**Rules**

- Empty or whitespace-only `body` is refused before anything is sent (FR-045).
- With a multi-line selection, the anchor is one well-defined line of it and the compose UI shows which,
  so the reviewer is never surprised by where the comment lands (FR-041).
- A cursor position that does not identify a single side is refused with the reason (FR-048).
- If the pull request gains commits such that `against_revision` is no longer current, the comment is
  posted against the revision the reviewer was reading, or the reviewer is told it moved — never silently
  attached to a line it was not written about (FR-049).
- On a failed post the draft survives with its body intact so the reviewer can retry (FR-046).

### HostError

Distinct conditions, because FR-064 requires the reviewer to know which one to fix.

`PrerequisiteMissing | NotAuthenticated | CredentialExpired | PermissionDenied | Unreachable | RateLimited | UnexpectedResponse | Cancelled`

**Rules**

- `PrerequisiteMissing` (tool not found) and `NotAuthenticated` must never be conflated (FR-062b).
- `UnexpectedResponse` carries the observed tool version where known, so a shape change is diagnosable
  rather than an opaque parse failure (research.md §4).
- Every variant is reportable inside the feature's own surface without a modal (FR-071).

---

## Changeset boundary types (`changeset.rs`)

### Changeset

The change under review, per FR-058. One implementation this phase: the change a pull request proposes
(FR-059). Deliberately says nothing about pull requests, so branch comparison can implement it later
(FR-061).

| Member | Type | Notes |
|---|---|---|
| `title` | `String` | Shown by the diff surface. |
| `files` | `Vec<ChangedFile>` | |
| `base_revision` | `String` | Resolved full object id of the merge base. |
| `head_revision` | `String` | Resolved full object id. |

### ChangedFile

One Files-tab row (FR-027, FR-028).

| Field | Type | Notes |
|---|---|---|
| `path` | `RepoPath` | Current path. |
| `previous_path` | `Option<RepoPath>` | Set for a rename; both paths identifiable (FR-028). |
| `change_kind` | `ChangeKind` | |
| `lines_added` | `u32` | |
| `lines_removed` | `u32` | |
| `render_refusal` | `Option<RenderRefusal>` | `Some` = the diff will not be rendered, with the reason. |

`ChangeKind` = `Added | Modified | Deleted | Renamed`

`RenderRefusal` = `Binary | TooLarge | Submodule | Symlink`

**Rules**

- Line counts and rename detection come from the **host's** diffstat, because the local tree diff is
  computed with `--no-renames` and reports renames as add+delete (research.md §1).
- `render_refusal` is set at list time so the Files tab can mark a file before the reviewer opens it, and
  the diff surface reports the reason rather than attempting to render (FR-037).
- An empty `files` means the Files tab says there is nothing to review (FR-029).

### FileDiff

The two sides for one file, materialised only when the reviewer opens it (FR-030).

| Field | Type | Notes |
|---|---|---|
| `path` | `RepoPath` | |
| `old_text` | `Option<String>` | `None` for an added file. |
| `new_text` | `Option<String>` | `None` for a deleted file. |
| `is_binary` | `bool` | |

Maps directly onto `project::git_store::CommitFile`, whose fields are public — which is what lets the
feature hand a synthesised `CommitDiff` to Zed's commit-diff view (research.md §1, §5). An added file has
`old_text: None` and a deleted file `new_text: None`, which is exactly how that view derives
added/deleted status (FR-036).

---

## Persisted state

### List View State

The only persisted entity (FR-021, FR-002a). Stored in Zed's key-value store under a feature-namespaced
key including the worktree identity — not in user settings, not in a feature-owned file.

| Field | Type | Default |
|---|---|---|
| `state_filter` | `StateFilter` | `OpenAndDraft` |
| `author_filter` | `Option<AuthorFilter>` | `None` |
| `sort_direction` | `SortDirection` | `MostRecentFirst` |
| `selected_repository` | `Option<RepositoryCoordinates>` | `None` (first repository) |
| `dock_position` | `DockPosition` | Zed's default |

`StateFilter` = `OpenAndDraft | All`
`AuthorFilter` = `Me | Person(Identity)` — `Me` is stored as a marker, not a resolved account id, so it
keeps meaning if the reviewer's identity changes (FR-015)
`SortDirection` = `MostRecentFirst | LeastRecentFirst`

**Rules**

- Unreadable or unparseable persisted state falls back to defaults rather than failing the panel.
- Persisted state is written after a change settles, not on every keystroke, so persistence never blocks
  the foreground thread (FR-067).
- Filters are applied by the **host**, not locally, so they cover the repository's whole set rather than
  the loaded page (FR-019).

---

## Lifecycle

Only two things in this feature have a lifecycle worth stating; everything else is derived and discarded.

**Panel selection**

```
no selection ──select──▶ loading detail ──ok──▶ showing detail ──select other──▶ loading detail
                              │                       │
                              └──error──▶ detail error (retry available)
                                                      │
                              ◀──── superseded selection abandons the in-flight load (FR-026)
```

**Draft comment**

```
composing ──submit──▶ posting ──ok────▶ posted (thread appears at its lines)
    │                    │
    │                    └──fail──▶ composing (body preserved, reason shown) (FR-046)
    └──cancel──▶ discarded (nothing sent) (FR-044)
```

No state here survives a restart. That is the point of FR-004: the reviewer's work product is the comment
on the pull request, not a local artefact.
</content>
