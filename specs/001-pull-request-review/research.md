# Phase 0 Research: Pull Request Review

**Date**: 2026-09-06 | **Plan**: [plan.md](./plan.md)

Everything below was verified against the running `twg` CLI (v1.0.1, authenticated) and against this
Zed checkout, not inferred. Commands and file:line references are given so each claim can be re-checked.

---

## 1. How the pull request diff is obtained

**Decision**: Fetch the pull request's source and destination revisions into the local repository, compute
the merge-base tree diff between them, load both sides' blob content, synthesise a
`project::git_store::CommitDiff`, and hand it to Zed's existing commit-diff view.

**Rationale**: The spec requires the diff to appear in Zed's *own* split diff viewer (FR-032, FR-033),
which needs real `language::Buffer`s rather than a rendered patch. Three existing pieces make this
work without inventing anything:

- `Repository::diff_tree(DiffTreeType::MergeBase { base, head })` — `crates/project/src/git_store.rs:9591`,
  backed by `git diff-tree -r -z --merge-base base head` at `crates/git/src/repository.rs:1921`. Takes two
  **arbitrary revisions**, so nothing needs checking out. This satisfies FR-034 (exclude what the
  destination branch acquired later) by construction, because git computes the merge base itself.
- `CommitDiff { pub files: Vec<CommitFile>, pub is_shallow_boundary: bool }` and
  `CommitFile { pub path, pub old_text, pub new_text, pub is_binary }` — `crates/project/src/git_store.rs:219-230`.
  **Every field is public**, so the feature can construct one directly rather than going through
  `load_commit_diff`, which is commit-scoped and would give the wrong changeset.
- `CommitView` builds a `MultiBuffer` of blob-backed buffers and wraps it in `SplittableEditor`
  (`crates/git_ui/src/commit_view.rs:305-338`), which is precisely the split diff surface FR-032 demands.

**Alternatives considered**:

- *Render `twg bitbucket pull-requests diff` (raw unified diff)* — rejected. It yields text, not buffers,
  so it cannot deliver FR-033's syntax highlighting, navigation and search, and comment anchoring would be
  against parsed hunk offsets rather than real lines.
- *`DiffBufferList` / `DiffBase`, as `git_ui/src/branch_diff.rs` uses* — rejected. `DiffBase` is
  `Head | Index | Staged | Merge { base_ref }` (`crates/project/src/git_store/diff_buffer_list.rs:27-32`)
  and every variant is anchored to the **working tree or HEAD**. A pull request's head is neither. This was
  the first approach investigated and it is a dead end worth recording, because the crate's name suggests
  otherwise.
- *Check the pull request out into a git worktree*, as the reference IntelliJ plugin does — rejected. The
  spec explicitly excludes worktrees and sessions (FR-004, FR-035), and `diff_tree` makes them unnecessary.

**Consequence — `TreeDiff` carries only the old oid.** `TreeDiffStatus` is
`Added | Modified { old: Oid } | Deleted { old: Oid }` (`crates/git/src/status.rs:521-525`). The new side
must be read as `<head_rev>:<path>`, not from an oid in the tree diff. And because `diff_tree` passes
`--no-renames`, renames arrive as add+delete; FR-028's rename display therefore comes from the **host's**
diffstat (§4), which does report `old.path` and `new.path`.

---

## 2. The host's listing payload, and why the list loads in two phases

**Decision**: Load the list in two phases. Phase 1 issues one `pull-requests query` and renders every row
immediately without approval bubbles. Phase 2 issues one `pull-requests get` per **visible** row, at
bounded concurrency, filling approval bubbles in as they arrive.

**Rationale**: Measured, not assumed. `pull-requests query` output does **not** include approval state:

```
query keys: author, close_source_branch, closed_by, comment_count, created_on, description,
            destination, draft, id, links, merge_commit, queued, reason, source, state,
            summary, task_count, title, type, updated_on
get keys:   ... adds _comments, _commits, _statuses, _tasks, participants, rendered, reviewers
```

`participants[]` is what FR-007 and FR-008 need — `{ user: { display_name, nickname, account_id, uuid,
links.avatar.href }, role: "REVIEWER"|"PARTICIPANT", approved: bool, state: null|"approved"|"changes_requested", participated_on }`
— and it appears only in `get`. So per-reviewer approval costs one call per pull request.

Payload size makes blocking on that impossible. Measured on `atlassian/twg-cli`:

| `--limit` | bytes | items | per item |
|---|---|---|---|
| 25 | 321,640 | 25 | ~12.9 KB |
| 100 | 1,540,441 | 100 | ~15.4 KB |
| 300 | 5,616,115 | 300 | ~18.7 KB |

Each row carries the pull request's full `description` and, in `get`, a `rendered` HTML block. A 500-row
list is roughly 9.5 MB of JSON. Two-phase loading is what lets FR-012 ("MUST NOT require every pull
request to be loaded before the first is shown") and SC-004 (first rows within 2s) both hold.

**Alternatives considered**:

- *`--agent-fields @compact` to narrow the payload* — **tested and rejected: it does nothing to the
  payload.** `--agent-fields @compact` returned byte-identical output (1,540,441 bytes) with identical keys
  to the unfiltered call. It is a hint for agent consumers, not a server- or CLI-side projection. There is
  no field-selection lever available.
- *Fetch approvals for all rows up front* — rejected; N sequential subprocesses, and it breaks SC-004.
- *Omit approval bubbles entirely* — rejected; FR-007 requires them.

**A correction worth recording.** An early test suggested `twg` truncates large JSON, because
`--limit 300 | jq` failed with `Unfinished string at EOF`. That was the test harness: a `head -c` further
down the pipeline closed the pipe and truncated the stream. Written to a file, `--limit 300` returns 300
valid items with exit 0 and empty stderr. **There is no truncation bug** — worth stating plainly so nobody
designs a workaround for a problem that does not exist.

---

## 3. Blob content, and why remote projects are unsupported this phase

**Decision**: Add one additive `pub fn` to `project::git_store::Repository` that loads blob content for a
list of `<revision>:<path>` specifiers, implemented for **local** repositories and returning an explicit
"not supported for remote projects" error otherwise. The panel reports that condition and stays usable.

**Rationale**: The diff needs file content at two revisions. Nothing public provides it:

- `load_blob_content(oid)` and `load_revisions(...)` exist only on the `GitRepository` **backend** trait
  (`crates/git/src/repository.rs:805`, `:801`), reachable solely from inside `git_store`'s
  `RepositoryState::Local`.
- The only blob-ish public project-layer method is `blame_buffer_at_revision`
  (`crates/project/src/git_store.rs:10329`), which is not usable for this.

Supporting remote projects would need a new `proto` message plus remote-server handling. That means editing
the `.proto` schema and the generated-code crate — two more allowlisted files, both high-conflict, for a
capability this phase does not need. The spec anticipates exactly this case and requires that a surface
which cannot work remotely **fail explicitly with a stated reason** rather than silently produce empty
results. So it fails explicitly, and the cost stays at one file.

**Alternatives considered**:

- *Shell out to `git cat-file` from the feature crate* — rejected under FR-077. It bypasses Zed's
  repository abstraction, duplicates path/encoding handling, and would silently produce wrong results for
  any repository whose access Zed mediates. This is the exact divergence FR-077 exists to prevent.
- *Add the proto message and support remote now* — deferred. Correct eventually, but it triples the
  allowlist for no in-phase benefit. Recorded as the natural follow-up if remote review is wanted.

---

## 4. Host command contract

**Decision**: Use `-o json` on every invocation. Treat the schema from `twg help describe` as
authoritative for option names and the observed output as authoritative for shapes.

**Verified**: With `-o json`, output is clean JSON on **stdout**, stderr is **empty**, exit code 0.
(Without `-o json`, `twg user` printed an `Update available: 1.0.1 -> 1.2.7` banner into its human output —
so the JSON flag is not optional for machine use.)

| Need | Command | Key options |
|---|---|---|
| List (FR-006, FR-013 – FR-019) | `bitbucket pull-requests query` | `--state OPEN\|MERGED\|DECLINED\|SUPERSEDED`, `--author <nickname>`, `--limit/-n` (default 25), `-w`, `-r` |
| Detail + approvals (FR-023, FR-007) | `bitbucket pull-requests get <id>` | `--comments`, `--diff`, `--statuses`, `--full` |
| Changed files (FR-027, FR-028) | `bitbucket pull-requests diffstat <id>` | `--limit/-n` (default **100** — must be raised) |
| Comments (FR-050 – FR-053) | `bitbucket pull-requests comment query <id>` | `--limit/-n` (default **50** — must be raised) |
| Post / reply (FR-042, FR-051) | `bitbucket pull-requests comment create` | `--pull-request`, `--text`, `--reply-to`, `--path`, `--line`, `--from-line` |
| Reviewer identity (FR-015) | `user` (alias `whoami`) | — |

**`diffstat` shape** (exactly what FR-027/FR-028 need):

```json
{ "type": "diffstat", "lines_added": 6, "lines_removed": 0,
  "status": "added",  // added | modified | removed | renamed
  "old": null, "new": { "path": ".changeset/public-code-review-skill.md", ... } }
```

**Two traps found in the CLI's own documentation.** The `examples` block for `comment create` shows
`--file-path`, `--line-to` and `--content-format`; `--help` confirms **none of those options exist**. The
real names are `--path`, `--line`, `--from-line`. The `get` examples likewise show `--include-comments` /
`--include-diffstat` where the real options are `--comments` / `--diff`. **Take option names from the
`opts` schema or `--help`, never from the examples.** This is also live justification for FR-062b: the host
tool's own surface is internally inconsistent, so its output shape must be parsed defensively.

**Default limits are too low for the spec.** `diffstat` defaults to 100 files and `comment query` to 50
comments, both of which silently truncate on real pull requests. Both must be raised explicitly; FR-027
requires *every* changed file.

---

## 5. Reusing Zed's commit-diff view

**Decision**: Make `CommitView::new` public and add one `pub fn` editor accessor. Synthesise
`CommitDetails` (sha = pull request head, message = title) and `CommitDiff` in the feature crate, then
construct the view directly. Use `file_filter: Some(path)` to scope the view to the file the reviewer
opened.

**Rationale**: This is FR-080, the user's Question 5 decision, and it turns out to be remarkably cheap.
`CommitView::open` is already `pub` (`commit_view.rs:183`) but loads the diff itself from a commit sha, so
it cannot be reused. `CommitView::new` (`:291`) takes exactly what is needed —
`(CommitDetails, CommitDiff, Entity<Repository>, Entity<Project>, Entity<Workspace>, WeakEntity<Workspace>, Option<usize>, Option<RepoPath>, window, cx)`
— and every one of those is constructible from the feature crate, because `CommitDetails` and `CommitDiff`
have public fields.

So the widening is **one visibility keyword plus one new accessor**. No signature changes, no moved items,
no reformatting — strictly additive per FR-075. `GitBlob` does not need to be exposed at all, since it is
used only inside `new`.

**Alternatives considered**:

- *Build a parallel diff item in the feature crate* — this was the Phase 0 **recommendation**, rejected by
  the user in clarification Question 5. It would keep `commit_view.rs` off the allowlist at the cost of
  ~200 duplicated lines. Recorded because FR-082 makes it the first thing to revisit if upstream churn in
  that file becomes painful.
- *Refactor the shared parts of `git_ui` into a helper* — forbidden by FR-075; a refactor is a
  modification, not an addition.

**Risk accepted, and how it is bounded**: `commit_view.rs` changes upstream. FR-081 requires the ordinary
commit path to behave identically (SC-021 verifies this across added, modified, deleted, binary and
shallow-boundary files), and FR-082 requires a test over the reused path so an upstream behavioural change
fails loudly instead of silently degrading review.

---

## 6. Process execution, cancellation and executable resolution

**Decision**: Resolve `twg` through `ProjectEnvironment::directory_environment`
(`crates/project/src/environment.rs:141`, public), honouring an environment-variable override read inside
the feature crate. Run each invocation as a subprocess on the background executor, capturing stdout and
stderr separately. Cancellation kills the child process, not merely the GPUI `Task`.

**Rationale**: FR-062a exists because Zed launched from the dock or a desktop launcher inherits a minimal
`PATH` that typically excludes `~/.local/bin` — which is where the observed `twg` lives
(`/Users/aabeygunawardana/.local/bin/twg`). Zed already solves this for other tooling by loading the
directory's shell environment, and that API is public, so no new touch point is needed.

Constitution Principle II requires cancellation to propagate to the real work, not just discard a result.
For a subprocess that means killing the child; dropping the `Task` alone would leave `twg` running and its
API calls in flight.

**Alternatives considered**: plain `std::env::var("PATH")` — rejected; it is the GUI-launch failure mode
FR-062a names, and it would report a tool the reviewer can plainly run in a terminal as "not installed".

---

## 7. Host capability gaps — two closed by narrowing scope, one remaining

Phase 0 found three places where the spec asked for something the shipped host implementation cannot do.
**Two have since been closed by decision** (2026-09-06): the requirement shrank to what was needed, so no
workaround exists at all. The third needs no spec change. The original findings are retained below because
the evidence still governs the design.

> **Resolution.** Comments are single-line only (FR-040, FR-042, FR-048) and resolving threads is out of
> scope (FR-054). Threading itself — replies joining their thread — remains required and is supported.
>
> The single-line decision had a second-order benefit worth noting: it removed a capability-negotiation
> mechanism from the host boundary. With nothing varying between implementations, the trait is uniform,
> and constitution Principle I no longer has an extension-point-with-one-case to object to.

### 7.1 Multi-line comment anchors cannot be posted — CLOSED (FR-042, FR-048)

Bitbucket's inline anchor **does** support ranges — observed on a real comment:

```json
"inline": { "from": null, "to": 239, "path": "packages/sdk/src/help/skills/matching.ts",
            "start_from": null, "start_to": null }
```

`start_to`..`to` is a range, and `twg comment query` returns it. But `twg comment create` exposes only
`--line` and `--from-line`. There is **no `--start-line`**, so a range cannot be posted.

The gap is in the *tool*, not the platform — which is a good illustration of why FR-056's boundary is
worth having.

**Resolved**: multi-line comments are out of scope. A comment anchors to one line, `--line` (new side) or
`--from-line` (old side), with no collapsing and no approximation. Reading a range remains supported and
required (FR-050), because pull requests created elsewhere carry ranges — so `start_to`/`start_from` are
still parsed. Creating a range and reading one are separate capabilities, and only the second is needed.

### 7.2 Thread resolution state is unavailable — CLOSED (FR-054)

The union of all keys across a real pull request's comments is:

```
content, created_on, deleted, id, inline, links, parent, pending, pullrequest, type, updated_on, user
```

There is no `resolution` field, and `comment query` has no flag to request one.

**Resolved**: resolving threads is out of scope. The feature neither reads nor sets resolution state, and
offers no resolve or reopen action — so there is no value to invent and nothing that can go stale. The
collapsible-threads half of FR-054 is kept, because that is what stops threads pushing code off screen.
`parent` **is** present, so reply threading (FR-051) is unaffected and remains in scope.

### 7.3 Approval state costs one call per pull request — REMAINS (FR-007)

Covered in §2. This does not need a spec change — the two-phase load satisfies FR-007 and FR-012 as
written — but it is worth flagging that approval bubbles appear *shortly after* the rows they belong to,
which acceptance testing should expect rather than treat as a defect.

---

## 8. Persistence of view state

**Decision**: Store filters, sort, selected repository (FR-021) and dock position (FR-002a) in Zed's
key-value store via `db::kvp` (`write_kvp`/`read_kvp`, `crates/db/src/kvp.rs:67,72`), under a
feature-namespaced key including the worktree identity.

**Rationale**: FR-066 rules out settings this phase, and FR-021 forbids a feature-owned config file. The
key-value store is where Zed already keeps this class of state, so it needs no new touch point.

**Alternatives considered**: `workspace::persistence` — its API is per-workspace and typed around
workspace serialization (`set_toolchain` and similar); the key-value store is the lighter fit for a small
blob of view state and requires no schema change.

---

## 9. Enforcing the change surface (gate 8)

**Decision**: Add `script/check-fork-surface`, following the convention of the existing `script/check-*`
gates (`check-keymaps`, `check-licenses`, `check-todos`). It diffs the branch against the upstream
merge-base, lists modified paths, excludes paths the fork added, and fails if any remaining path is not on
the allowlist. Land it **before** feature code.

**Rationale**: FR-078a requires automatic enforcement, and constitution gate 8 requires the check to pass.
Neither is satisfiable until the script exists, so it is task one. Building it first also means the
allowlist is correct from the first commit instead of being reconstructed later from a messy diff.

**Note**: the sandbox used for this research could not read `.git`, so the exact upstream remote and ref
name were not verified. The script must take them as configuration rather than hard-coding `upstream/main`.
</content>
