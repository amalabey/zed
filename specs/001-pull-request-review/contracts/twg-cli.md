# Contract: the `twg` CLI host implementation

**Discharges**: spec FR-062, FR-062a, FR-062b, FR-064 | **Implements**: [pull-request-host.md](./pull-request-host.md)
**Verified against**: `twg` 1.0.1 (beta), authenticated, on `atlassian/twg-cli`, 2026-09-06

Everything here was observed by running the tool. Where the tool's own documentation disagrees with its
behaviour, the behaviour is recorded and the discrepancy called out.

## Invocation rules

1. **Always `-o json`.** Without it the tool emits human output, including an `Update available: 1.0.1 ->
   1.2.7` banner mixed into stdout. With it, stdout is clean JSON, stderr is empty, exit code 0 — verified
   by writing both streams to separate files.
2. **Always pass `-w <workspace> -r <repo>` explicitly.** The tool auto-detects them from the git remote,
   but the feature already knows the repository coordinates and must not depend on the tool's cwd.
3. **Resolve the executable through the project's shell environment** (FR-062a), honouring an
   environment-variable override. The observed binary is at `~/.local/bin/twg`, which a dock-launched Zed's
   inherited `PATH` typically excludes.
4. **Parse defensively.** Unknown fields ignored; unknown enum values degrade to an unknown variant
   rather than failing the containing object; a malformed row must not blank the list (FR-065).

## Command mapping

| Boundary method | Command |
|---|---|
| `list` | `bitbucket pull-requests query -w W -r R -o json --state S [--author A] -n LIMIT` |
| `detail` | `bitbucket pull-requests get <id> -w W -r R -o json` |
| `changed_files` | `bitbucket pull-requests diffstat <id> -w W -r R -o json -n <high>` |
| `comments` | `bitbucket pull-requests comment query <id> -w W -r R -o json -n <high>` |
| `post_comment` | `bitbucket pull-requests comment create --pull-request <id> --text T [--reply-to C] [--path P] [--line L] [--from-line F] -w W -r R -o json` |
| `viewer` | `user -o json` |

### State filter mapping

`--state` accepts `OPEN | MERGED | DECLINED | SUPERSEDED`, default `OPEN`. **There is no `DRAFT` state** —
draft is a separate boolean `draft` field on an otherwise open pull request.

| Spec filter | Invocation |
|---|---|
| `OpenAndDraft` (default, FR-013) | `--state OPEN` — drafts are included, being open pull requests with `draft: true` |
| `All` | One call per state, merged; there is no "all" value |

### Author filter

`--author` matches a **nickname**, not a username slug or account id. The `--reviewer` option's help says
so explicitly for reviewers, and the same applies here. For `AuthorFilter::Me`, resolve the viewer's
nickname via `user` first (FR-015).

### Limits

| Command | Default | Action |
|---|---|---|
| `query` | 25 | Raise; paginate to satisfy the filter over the whole set (FR-019) |
| `diffstat` | **100** | Raise — FR-027 requires every changed file |
| `comment query` | **50** | Raise — silently truncates on busy pull requests |

There is no cursor or page-token option; `--limit` is the only knob.

## Observed JSON shapes

### `query` — one array element

```
author, close_source_branch, closed_by, comment_count, created_on, description, destination,
draft, id, links, merge_commit, queued, reason, source, state, summary, task_count, title,
type, updated_on
```

Mapping: `id`→`number`, `title`, `author`→`Identity`, `state`+`draft`→`PullRequestState`+`is_draft`,
`created_on`→`opened_at`, `updated_on`→`last_activity_at`, `links.html.href`→`web_url`.

**`participants` and `reviewers` are absent.** Approval state is not available from this call — this is
what forces the two-phase list load (research.md §2).

### `get` — adds

```
_comments, _commits, _statuses, _tasks, participants, rendered, reviewers
```

`participants[]`:

```json
{ "type": "participant",
  "user": { "display_name": "...", "nickname": "...", "account_id": "712020:...",
            "uuid": "{...}", "links": { "avatar": { "href": "..." } } },
  "role": "REVIEWER", "approved": false, "state": null,
  "participated_on": "2026-09-05T23:10:40.671913+00:00" }
```

→ `ReviewerVerdict`. `approved: true` → `Approved`; `state: "changes_requested"` → `ChangesRequested`;
otherwise `NoVerdict`. `reviewers[]` is a plain list of requested reviewers with no verdict, so it is not
a substitute.

`source` / `destination`:

```json
{ "branch": { "name": "codex/twg-code-review-internal-beta", "sync_strategies": [...] },
  "commit": { "hash": "79ae1304eb1f", "links": {...} },
  "repository": { ... } }
```

**`commit.hash` is abbreviated to 12 characters.** Resolve it to a full object id locally before using it
as a git argument. `source.repository != destination.repository` identifies a fork.

### `diffstat` — one array element

```json
{ "type": "diffstat", "lines_added": 6, "lines_removed": 0,
  "status": "added", "old": null,
  "new": { "path": ".changeset/public-code-review-skill.md", "type": "commit_file", ... } }
```

→ `ChangedFile`. `status` ∈ `added | modified | removed | renamed`. For a rename, `old.path` and
`new.path` both present → `previous_path`. **This is the only source of rename information**, because the
local tree diff runs with `--no-renames`.

### `comment query` — union of all keys observed

```
content, created_on, deleted, id, inline, links, parent, pending, pullrequest, type, updated_on, user
```

`content`: `{ "type": "rendered", "raw": "...", "markup": "markdown", "html": "..." }` — use `raw`.
`parent`: present on replies → thread structure (FR-051).
`inline`, on an inline comment:

```json
{ "from": null, "to": 239, "path": "packages/sdk/src/help/skills/matching.ts",
  "start_from": null, "start_to": null }
```

`to`/`start_to` are new-side lines; `from`/`start_from` old-side. A range is `start_to..to` (or
`start_from..from`). A single-line anchor has `start_*` null. Absent `inline` → an unanchored comment,
shown in the Overview tab (FR-052).

**No `resolution` field appears anywhere in this output**, which is why resolving threads is out of scope
(FR-054) rather than approximated.

When posting, `--line` targets the new side and `--from-line` the old side; exactly one is set, because a
comment anchors to one line on one side (FR-042, FR-048).

## Capability fit

Phase 0 originally recorded three gaps between what the spec asked for and what this tool can do. Two were
closed by narrowing the requirement rather than working around the tool, so only one remains — and it
needs no spec change.

### Closed — single-line comments (FR-040, FR-042)

`comment create` exposes only `--line` and `--from-line`; there is no `--start-line`, so a range cannot be
posted. The spec now requires single-line comments, so **this implementation maps cleanly**: `--line` for
the new side, `--from-line` for the old side, one line, no collapsing and no approximation.

Reading is unaffected and remains full-fidelity: `inline.start_to`/`start_from` are parsed, so a comment
created elsewhere with a range is displayed over the lines it covers (FR-050).

### Closed — no thread resolution (FR-054)

The tool's comment output has no `resolution` field and no flag requests one. The spec now puts resolving
threads out of scope, so nothing is needed: the feature neither reads nor sets resolution, and cannot
display a value it would have had to invent. Threading itself works — `parent` is present on replies and
`--reply-to` posts into a thread (FR-051).

### Remaining — approvals cost one call per pull request

Covered above and in research.md §2. No spec change needed; the two-phase load satisfies FR-007 and
FR-012 as written. Acceptance testing should expect approval bubbles to arrive shortly after their rows.

### Why this is worth recording

Both closures went the same way: the requirement shrank to what was actually needed, and the workaround
disappeared with it. The single-line decision in particular removed a whole capability-negotiation
mechanism from [pull-request-host.md](./pull-request-host.md) — the boundary is now uniform because
nothing varies between implementations. Had the range requirement been kept, every implementation would
have carried a "can I post ranges?" question forever, to serve a capability the reviewer never asked for.

## Trap: the tool's examples are wrong

The `examples` blocks in `twg help describe` do not match the real options. Verified against `--help`:

| Command | Examples show | Actually exists |
|---|---|---|
| `comment create` | `--file-path`, `--line-to`, `--content-format` | `--path`, `--line`, `--from-line` |
| `get` | `--include-comments`, `--include-diffstat` | `--comments`, `--diff` |

**Take option names from the `opts` schema or `--help`, never from the examples.** This inconsistency in
the tool's own surface is the live justification for FR-062b's requirement that a version or shape change
be reported as an unexpected response naming the version found, rather than as an opaque parse failure.

## Error classification (FR-064)

| Condition | Detection |
|---|---|
| `PrerequisiteMissing` | executable not resolvable via the shell environment or the override |
| `NotAuthenticated` / `CredentialExpired` | tool reports an auth failure — never conflated with the above |
| `PermissionDenied` | authenticated but no access to the repository |
| `Unreachable` | network failure |
| `RateLimited` | host rate limit; reported, not retried automatically |
| `UnexpectedResponse` | non-zero exit with unrecognised output, or JSON that does not match the shapes above; carries the observed tool version |
| `Cancelled` | the child process was killed by cancellation |

## Test fixtures

The JSON captured during Phase 0 is the basis for the implementation's tests, so the wire contract is
pinned without network access. Fixtures needed: a multi-state pull request list; a pull request with
mixed `approved`/`changes_requested`/no-verdict participants; one with no reviewers; a diffstat covering
added, modified, removed and renamed; comments including an inline single line, an inline **range** (to
prove reading a range still works even though none can be created), a reply with `parent` set, and an
unanchored comment; and malformed output for each `HostError` variant.
</content>
