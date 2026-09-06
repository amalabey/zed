---

description: "Task list for Pull Request Review"
---

# Tasks: Pull Request Review

**Input**: Design documents from `/specs/001-pull-request-review/`

**Prerequisites**: [plan.md](./plan.md), [spec.md](./spec.md), [research.md](./research.md),
[data-model.md](./data-model.md), [contracts/](./contracts/), [quickstart.md](./quickstart.md)

**Tests**: Test tasks below are **not** blanket TDD. Each one exists because a numbered requirement or a
constitution gate demands it: FR-065/SC-016 (no panic on host failure), FR-069 (cancellation stops the
work), FR-082/SC-021 (the reused commit view), SC-013 (no platform name above the seam), SC-006 (seven
distinct failure conditions), SC-009 (the base is the divergence point). Constitution gates 2, 6 and 7
require them at merge.

**Organization**: Tasks are grouped by user story so each story can be implemented, tested and delivered
independently.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (US1–US6)
- Every task names the file it changes and the requirement it discharges

## Path Conventions

All feature code lives in one new crate, per constitution Principle VI and spec FR-074:

- `crates/pull_request_review/src/` — every line of feature code
- `script/check-fork-surface` — the FR-078a allowlist gate
- Five allowlisted pre-existing files, enumerated in [contracts/zed-surface.md](./contracts/zed-surface.md)

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Make the change surface enforceable, then create the crate. The gate lands before any
feature code so the allowlist is correct from the first commit rather than reconstructed from a messy
diff later (research.md §9).

- [ ] T001 Create `script/check-fork-surface` following the convention of `script/check-keymaps` and `script/check-todos`: resolve the merge-base against a **configurable** upstream remote/ref, list paths modified relative to it, exclude paths the fork added, and fail naming any remaining path not on the allowlist in `specs/001-pull-request-review/contracts/zed-surface.md` (FR-078, FR-078a, FR-078b, constitution gate 8)
- [ ] T002 Add a self-test mode to `script/check-fork-surface` that modifies one off-allowlist pre-existing file, asserts the check fails naming it, and restores the file — so the gate is proven to bite (SC-017, quickstart Gate 0)
- [ ] T003 Create `crates/pull_request_review/Cargo.toml` with `[lib] path = "src/pull_request_review.rs"`, workspace-internal dependencies only (`gpui`, `ui`, `editor`, `workspace`, `project`, `git`, `git_ui`, `language`, `markdown`, `db`, `util`, `anyhow`, `serde`, `serde_json`, `futures`, `smol`) and no third-party addition (FR-079, constitution new-crate conventions)
- [ ] T004 Add `crates/pull_request_review` to `members` and to `[workspace.dependencies]` in the workspace root `Cargo.toml` — new lines only, no existing dependency version changed (FR-079, allowlist entry 1)
- [ ] T005 Add the dependency on the new crate to `crates/zed/Cargo.toml` (allowlist entry 2)
- [ ] T006 Add `pull_request_review::init(cx);` beside `git_ui::init(cx);` at `crates/zed/src/main.rs:772` — `main.rs` only, not `zed.rs` and not `visual_test_runner.rs` (FR-074, allowlist entry 3)
- [ ] T007 [P] Capture the Phase 0 `twg` JSON fixtures into `crates/pull_request_review/src/test_fixtures/`: a multi-state pull request list, a 500-row list, a pull request with mixed `approved`/`changes_requested`/no-verdict participants, one with no reviewers, a diffstat covering added/modified/removed/renamed, comments including an inline single line, an inline range, a reply with `parent` set and an unanchored comment, and malformed output for each `HostError` variant (contracts/twg-cli.md §Test fixtures)
- [ ] T008 [P] Confirm `./script/clippy`, `cargo fmt --check` and `script/check-fork-surface` are all clean on the empty crate, so the gate's baseline is green before feature code lands

**Checkpoint**: The allowlist gate exists and passes, and the crate builds empty. Nothing is registered yet.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: The two mandated boundaries and the host plumbing beneath them. Every user story depends on
these.

**⚠️ CRITICAL**: No user story work can begin until this phase is complete.

### The FR-056 host boundary

- [ ] T009 Declare the host boundary's identity and list types in `crates/pull_request_review/src/host.rs`: `PullRequestId`, `RepositoryCoordinates`, `Identity`, `PullRequestState` (`Open | Merged | Declined | Superseded` with unknown-value degradation), `PullRequestSummary` (data-model.md §Host boundary types)
- [ ] T010 Declare the detail and comment types in `crates/pull_request_review/src/host.rs`: `PullRequestDetail`, `BranchRef`, `ReviewerVerdict`/`Verdict`, `CommentThread`, `CommentId`, `CommentAnchor`/`DiffSide`, `DraftComment` — `DraftComment` carrying a single `line: u32`, not a range, so the type system enforces FR-040 (data-model.md, FR-040, FR-054: no resolution field)
- [ ] T011 Declare `HostError` with its eight distinct variants (`PrerequisiteMissing | NotAuthenticated | CredentialExpired | PermissionDenied | Unreachable | RateLimited | UnexpectedResponse | Cancelled`), `UnexpectedResponse` carrying the observed tool version, and the `PullRequestHost` trait (`list`, `detail`, `changed_files`, `comments`, `post_comment`, `viewer`, each returning `Task`) with a doc comment citing FR-056 in `crates/pull_request_review/src/host.rs` (FR-064, FR-067, constitution Principle I citation requirement)

### The FR-058 changeset boundary

- [ ] T012 Declare the `Changeset` trait (`title`, `files`, `file_diff`, `revisions`) and its types `ChangedFile`, `ChangeKind`, `RenderRefusal`, `FileDiff`, `RevisionPair` in `crates/pull_request_review/src/changeset.rs`, with a doc comment citing FR-058 and no mention of a pull request, host or reviewer (FR-058, FR-061, contracts/changeset.md)

### Host plumbing

- [ ] T013 Resolve the host executable through `ProjectEnvironment::directory_environment` with an environment-variable override read inside the crate, in `crates/pull_request_review/src/host_process.rs` — a tool the reviewer can run in a terminal is never reported as missing (FR-062a)
- [ ] T014 Run each invocation as a subprocess on the background executor in `crates/pull_request_review/src/host_process.rs`, capturing stdout and stderr separately, with cancellation that **kills the child process** rather than only dropping the `Task` (FR-067, FR-069, constitution Principle II)
- [ ] T015 Build the `-o json` invocation layer and defensive JSON parsing helpers in `crates/pull_request_review/src/host_twg.rs`: always `-o json`, always explicit `-w <workspace> -r <repo>`, unknown fields ignored, unknown enum values degraded rather than failing the containing object, one malformed row never blanking the list (FR-062, FR-065, contracts/twg-cli.md §Invocation rules)
- [ ] T016 Map every failure to a `HostError` variant in `crates/pull_request_review/src/host_twg.rs` — executable unresolvable → `PrerequisiteMissing`, never conflated with an auth failure (FR-062b, FR-064, contracts/twg-cli.md §Error classification)
- [ ] T017 Derive `RepositoryCoordinates` from the git remote in `crates/pull_request_review/src/host.rs`, reporting the no-remote, unsupported-remote and several-repositories conditions as distinct stated reasons rather than an empty list (FR-005, spec edge cases)
- [ ] T018 Report remote projects as explicitly unsupported with the reason in `crates/pull_request_review/src/host.rs`, rather than silently producing empty results (research.md §3, constitution platform constraint)

### Crate root

- [ ] T019 Write `crates/pull_request_review/src/pull_request_review.rs`: `init(cx)` registering the panel type and declaring actions with `actions!`, constructing nothing and contacting no host — no settings schema field, no `default.json` entry, no default keymap entry (FR-003, FR-066, FR-070)

### Tests for the foundation

- [ ] T020 [P] Test in `crates/pull_request_review/src/host_process.rs` that dropping an in-flight invocation's `Task` kills the child process, using `#[gpui::test]` with `run_until_parked()` and the GPUI executor's timers rather than wall-clock sleeps (FR-069, constitution gate 2)
- [ ] T021 [P] Test in `crates/pull_request_review/src/host_twg.rs` that each malformed fixture produces its intended distinct `HostError` variant and that no path panics on malformed, truncated or unexpected output (FR-064, FR-065, SC-006, SC-016)
- [ ] T022 [P] Add a test in `crates/pull_request_review/src/pull_request_review.rs` asserting `init` performs no host contact and constructs no panel, and a source check that no host, platform or transport name appears outside `host_twg.rs` and `host_process.rs` (FR-003, FR-057, FR-070, SC-013)

**Checkpoint**: Both boundaries are declared and the host plumbing is tested against fixtures with no
network. User story implementation can now begin.

---

## Phase 3: User Story 1 - See the repository's pull requests (Priority: P1) 🎯 MVP

**Goal**: The Pull Requests panel lists the repository's pull requests — status indicator, title, author,
approver bubbles, relative age — open and draft only, most recently active first.

**Independent Test**: On a project whose repository has pull requests in different states, open the panel
and confirm open and draft pull requests are listed newest-first, and that each row shows a status
indicator, title, author, approver bubbles and a relative age.

- [ ] T023 [US1] Implement `PullRequestPanel` as a `workspace::Panel` in `crates/pull_request_review/src/panel.rs`, dockable and dismissible like Zed's other panels, holding the list and a detail section below it, constructed on first open (FR-001, FR-003)
- [ ] T024 [US1] Declare the open and focus actions in `crates/pull_request_review/src/pull_request_review.rs` so they are discoverable in the command palette and bindable, with no default keybinding shipped (FR-002, FR-066)
- [ ] T025 [US1] Persist the panel's dock position and size through Zed's own panel serialization in `crates/pull_request_review/src/panel.rs` — not user settings, not a feature-owned file (FR-002a)
- [ ] T026 [P] [US1] Implement `PullRequestHost::list` in `crates/pull_request_review/src/host_twg.rs` as `bitbucket pull-requests query -w W -r R -o json --state OPEN -n <limit>`, parsing `id`→`number`, `title`, `author`→`Identity`, `state`+`draft`, `created_on`→`opened_at`, `updated_on`→`last_activity_at`, `links.html.href`→`web_url` (FR-006, FR-013, contracts/twg-cli.md §query)
- [ ] T027 [P] [US1] Implement `PullRequestHost::viewer` in `crates/pull_request_review/src/host_twg.rs` as `user -o json` → `Identity`, so the reviewer's own nickname is resolvable without them typing it (FR-015)
- [ ] T028 [US1] Implement the approvals half of `PullRequestHost::detail` in `crates/pull_request_review/src/host_twg.rs`: `pull-requests get <id>` → `participants[]` → `Vec<ReviewerVerdict>`, with `approved: true` → `Approved`, `state: "changes_requested"` → `ChangesRequested`, otherwise `NoVerdict`; `reviewers[]` is not a substitute (FR-007, research.md §2)
- [ ] T029 [US1] Normalise time fields at parse time in `crates/pull_request_review/src/host.rs`: a future `opened_at` becomes *just opened* rather than a negative age, and a missing `last_activity_at` falls back to `opened_at` rather than the epoch (data-model.md validation rules, spec clock-skew edge case)
- [ ] T030 [US1] Render list rows in `crates/pull_request_review/src/list.rs`: a status indicator distinguishing at least open, draft, merged and declined; the title elided within the panel width rather than forcing horizontal scroll; the author; and a relative age (FR-006, spec long-title edge case)
- [ ] T031 [US1] Render approval bubbles in `crates/pull_request_review/src/list.rs`: approved and changes-requested shown distinctly, a bounded number of bubbles plus a `+N` affordance with the full list on demand, and an **empty** area — not a placeholder — when nobody has reviewed (FR-007, FR-008, FR-009)
- [ ] T032 [US1] Fall back to a stable derived initial from `account_id` when the host supplies neither display name nor avatar, in `crates/pull_request_review/src/list.rs` — never blank (FR-010)
- [ ] T033 [US1] Implement the two-phase load in `crates/pull_request_review/src/panel.rs`: render every row from one `list` call, then hydrate approvals with one `detail` call per **visible** row at bounded concurrency, filling bubbles in as they arrive (FR-007, FR-012, SC-004, research.md §2)
- [ ] T034 [US1] Show the loading state, and report each `HostError` variant as its own actionable message with a retry in `crates/pull_request_review/src/panel.rs` — no modal, no focus theft, the rest of Zed unaffected (FR-064, FR-071, FR-073)
- [ ] T035 [US1] Implement refresh in `crates/pull_request_review/src/panel.rs` preserving selection, filters and sort, and reporting a selected pull request that has disappeared rather than silently deselecting it (FR-011, spec list-changed edge case)
- [ ] T036 [P] [US1] Test with `#[gpui::test]` and `run_until_parked()` in `crates/pull_request_review/src/panel.rs` that rows render before approvals arrive, so the list is readable while hydration is still in flight (FR-012)
- [ ] T037 [P] [US1] Test in `crates/pull_request_review/src/host_twg.rs` that the default state filter lists only open and draft pull requests from the multi-state fixture (US1 acceptance scenario 2, FR-013)
- [ ] T038 [P] [US1] Test in `crates/pull_request_review/src/panel.rs` against the 500-row fixture that the first rows are produced without loading every pull request's approvals, and that no foreground entity update exceeds one frame budget (FR-012, FR-067, SC-004)

**Checkpoint**: User Story 1 is fully functional. A reviewer who only reads this list already gets "what is
waiting for me" without leaving Zed.

---

## Phase 4: User Story 2 - Read a pull request's overview and what it changes (Priority: P1)

**Goal**: Selecting a pull request opens a detail section with two tabs — Overview (status, title, markdown
description, branches, author, approval states, browser link) and Files (every changed file with its change
kind and line counts).

**Independent Test**: Select a pull request with a multi-paragraph description and a dozen changed files
including an add, a delete and a rename; confirm Overview renders the description and every approval state,
and Files lists all twelve files with the correct change kind and line counts.

- [ ] T039 [US2] Add the detail section with exactly two tabs, Overview selected by default, in `crates/pull_request_review/src/panel.rs` (FR-022)
- [ ] T040 [US2] Extend `PullRequestHost::detail` in `crates/pull_request_review/src/host_twg.rs` with the description (`content.raw`), `source`/`destination` as `BranchRef` — noting `commit.hash` is abbreviated to 12 characters — and `can_comment` derived from state and permission (FR-023, FR-047, contracts/twg-cli.md §get)
- [ ] T041 [P] [US2] Implement `PullRequestHost::changed_files` in `crates/pull_request_review/src/host_twg.rs` as `pull-requests diffstat <id> -n <high>` with the default 100 raised, mapping `status` → `ChangeKind` and `old.path`/`new.path` → `previous_path` — the only source of rename information (FR-027, FR-028, contracts/twg-cli.md §Limits)
- [ ] T042 [P] [US2] Implement the pull request `Changeset` in `crates/pull_request_review/src/changeset_pull_request.rs`: `files()` delegating to the host boundary without fetching any content, and `revisions()` resolving the host's abbreviated revisions to full object ids and returning the **merge base** as `base` (FR-030, FR-034, contracts/changeset.md obligations 2 and 3)
- [ ] T043 [US2] Render the Overview tab in `crates/pull_request_review/src/overview.rs`: status, title, description rendered as formatted markdown via the `markdown` crate, source and destination branches, author, each reviewer's approval state, and a link that opens the pull request in a browser (FR-023, FR-024)
- [ ] T044 [US2] Render "no description" for both `None` and `Some("")` in `crates/pull_request_review/src/overview.rs`, rather than a blank area indistinguishable from a failed load (FR-024)
- [ ] T045 [US2] Render the Files tab in `crates/pull_request_review/src/files.rs`: every changed file's path, change kind, added and removed line counts, and a total for the pull request, staying scrollable and responsive on thousands of files (FR-027, spec thousands-of-files edge case)
- [ ] T046 [US2] Make both the old and the new path identifiable on a renamed file's row in `crates/pull_request_review/src/files.rs` (FR-028)
- [ ] T047 [US2] Say "nothing to review" for an empty changeset in `crates/pull_request_review/src/files.rs`, rather than showing an empty list (FR-029)
- [ ] T048 [US2] Report a file-list failure with its reason and a retry in `crates/pull_request_review/src/files.rs`, leaving the Overview tab readable (FR-031)
- [ ] T049 [US2] Give each tab its own load state in `crates/pull_request_review/src/panel.rs`, so switching between Overview and Files reloads nothing and loses no state while the other tab and the list stay usable (FR-025, US2 acceptance scenario 7)
- [ ] T050 [US2] Abandon a superseded detail load when the reviewer selects a different pull request, in `crates/pull_request_review/src/panel.rs` — cancelling the underlying work, not merely discarding its result (FR-026, FR-069)
- [ ] T051 [P] [US2] Test with `#[gpui::test]` in `crates/pull_request_review/src/panel.rs` that a superseded detail load cannot arrive later and replace the newer selection (FR-026)
- [ ] T052 [P] [US2] Test in `crates/pull_request_review/src/host_twg.rs` that the diffstat fixture covering added, modified, removed and renamed maps to the expected `ChangedFile` rows, including `previous_path` on the rename (FR-027, FR-028)

**Checkpoint**: User Stories 1 and 2 together are a complete read-only review surface — everything a
reviewer reads before the first line of diff.

---

## Phase 5: User Story 3 - Read the diff in Zed's own split diff viewer (Priority: P1)

**Goal**: Opening a file from the Files tab opens Zed's own split diff viewer on that file's change, with
the reviewer's theme, font, keymap, editor settings, syntax highlighting, navigation and search — showing
only what the pull request proposes.

**Independent Test**: On a pull request whose destination branch has moved since it was raised, open a
changed file and confirm the split diff shows only the pull request's own changes to that file, with the
reviewer's theme and syntax highlighting applied, and that their branch, index and uncommitted changes are
unchanged throughout.

### Allowlisted upstream additions

Both are strictly additive per FR-075 and both are enumerated in
[contracts/zed-surface.md](./contracts/zed-surface.md). `script/check-fork-surface` must pass after each.

- [ ] T053 [US3] Add one `pub fn` to `crates/project/src/git_store.rs` loading blob content for a list of `<revision>:<path>` specifiers, returning an explicit "not supported for remote projects" error otherwise — one new method appended to an existing `impl`, nothing else changed (FR-077, allowlist entry 5, research.md §3)
- [ ] T054 [P] [US3] Test the added blob loader in `crates/project`: content returned for a local repository, and the stated unsupported error for a remote one (FR-077, research.md §3)
- [ ] T055 [US3] Make `CommitView::new` `pub` and add one `pub fn` accessor returning the view's `SplittableEditor` in `crates/git_ui/src/commit_view.rs` — one visibility keyword and one new method, no signature change, no moved item, no reformatting, `GitBlob` left private (FR-075, FR-080, FR-082, allowlist entry 4, research.md §5)
- [ ] T056 [P] [US3] Test in `crates/git_ui` that opening an ordinary commit in the commit-diff view behaves identically for an added, a modified, a deleted and a binary file, and for a shallow-boundary commit (FR-081, SC-021)

### The changeset implementation

- [ ] T057 [US3] Fetch the pull request's source and destination revisions into the local repository in `crates/pull_request_review/src/changeset_pull_request.rs`, creating no branch, no worktree and no checkout, and leaving the working tree, index and stash untouched (FR-035)
- [ ] T058 [US3] Implement `Changeset::file_diff` in `crates/pull_request_review/src/changeset_pull_request.rs` using `Repository::diff_tree(DiffTreeType::MergeBase { base, head })` for the file set and the new blob loader for both sides, reading the new side as `<head_rev>:<path>` because `TreeDiffStatus` carries only the old oid (FR-034, research.md §1)
- [ ] T059 [US3] Set `render_refusal` at list time for binary, too-large, submodule and symlink files in `crates/pull_request_review/src/changeset_pull_request.rs`, so the Files tab can mark a file before it is opened and `file_diff` returns the refusal rather than an error (FR-037, contracts/changeset.md obligation 4)
- [ ] T060 [US3] Report a fork-sourced pull request (`source.repository != destination.repository`) as unsupported in `crates/pull_request_review/src/changeset_pull_request.rs` rather than diffing the wrong changeset (spec fork edge case: "never a silent diff of the wrong change")
- [ ] T061 [US3] Report an unresolvable revision as the missing-ref condition, naming which ref, in `crates/pull_request_review/src/changeset_pull_request.rs` (data-model.md `BranchRef` rules, spec missing-ref edge case)

### The diff surface

- [ ] T062 [US3] Synthesise `CommitDetails` (sha = the pull request's head, message = its title) and `CommitDiff` and construct `CommitView` with `file_filter: Some(path)` in `crates/pull_request_review/src/diff.rs`, so the diff is Zed's own split surface with its existing unified/split toggle (FR-032, FR-033, FR-080, research.md §5)
- [ ] T063 [US3] Map an added file to `old_text: None` and a deleted file to `new_text: None` in `crates/pull_request_review/src/diff.rs`, so each shows as wholly added or wholly removed rather than a missing file on one side (FR-036)
- [ ] T064 [US3] Reuse one diff item across successive file opens in `crates/pull_request_review/src/diff.rs`, so moving between files does not accumulate tabs the reviewer has to close (FR-039)
- [ ] T065 [US3] Acknowledge the open within 100ms, report progress, and allow cancellation that stops the underlying fetch and blob load, in `crates/pull_request_review/src/diff.rs` (FR-038, FR-068, FR-069)
- [ ] T066 [US3] Report a diff that cannot be produced in the editor's own surface without stealing focus or opening a modal, leaving the panel usable, in `crates/pull_request_review/src/diff.rs` (FR-037, FR-071)

### Tests for User Story 3

- [ ] T067 [P] [US3] Test in `crates/pull_request_review/src/changeset_pull_request.rs` that the base is the divergence point: on a fixture whose destination branch has advanced by at least ten commits, the files listed and the lines shown contain none of the destination's later changes (FR-034, SC-009)
- [ ] T068 [P] [US3] Test in `crates/pull_request_review/src/diff.rs` the reused path required by FR-082: the feature's `CommitView` construction produces the expected multibuffer for added, modified, deleted and binary files, so an upstream change to that file fails loudly rather than silently degrading review (FR-082)
- [ ] T069 [P] [US3] Test in `crates/pull_request_review/src/changeset_pull_request.rs` that reading a pull request's diff leaves the branch, index, working tree and stash unchanged and creates no worktree (FR-035, SC-002)
- [ ] T070 [P] [US3] Test with `#[gpui::test]` in `crates/pull_request_review/src/diff.rs` that cancelling an in-flight diff stops the blob load and the revision fetch rather than only discarding the result, and that a missing revision or unreadable blob cannot panic (FR-069, SC-016)

**Checkpoint**: All three P1 stories are complete. This is the point of the feature — reading a colleague's
pull request end to end without a browser.

---

## Phase 6: User Story 4 - Comment on the pull request from the diff (Priority: P2)

**Goal**: The reviewer puts the cursor on a line in the diff, invokes the comment action, types, and
submits — and the comment is posted to the pull request as a single-line inline comment on that file, on
the side they were reading.

**Independent Test**: On an open pull request, put the cursor on a line in a file's split diff, write a
comment, submit it, and confirm it appears on the pull request at that file and that line, attributed to
the reviewer — verified against the pull request itself.

**Depends on**: User Story 3 (the comment is composed from the diff surface).

- [ ] T071 [US4] Declare the comment action in `crates/pull_request_review/src/pull_request_review.rs` so it is discoverable in the command palette and bindable in the keymap, with no default binding (FR-040, FR-066)
- [ ] T072 [US4] Open an inline compose editor at the cursor's line in `crates/pull_request_review/src/comments.rs`, without stealing focus from the reviewer's typing and without a modal (FR-040, FR-041, FR-071)
- [ ] T073 [US4] Resolve exactly one `DiffSide` from the cursor position in `crates/pull_request_review/src/comments.rs`, refusing with the reason when the position does not identify a single side — never posted against whichever side the host happens to pick (FR-048)
- [ ] T074 [US4] Anchor a multi-line selection to one well-defined line and show the reviewer which line before they submit, in `crates/pull_request_review/src/comments.rs` — no range is ever attempted (FR-041, spec multi-line edge case)
- [ ] T075 [US4] Refuse an empty or whitespace-only body before anything is sent, in `crates/pull_request_review/src/comments.rs` (FR-045)
- [ ] T076 [US4] Refuse a line that is not part of the pull request's change, with the reason, in `crates/pull_request_review/src/comments.rs` (spec commenting edge case)
- [ ] T077 [P] [US4] Implement `PullRequestHost::post_comment` in `crates/pull_request_review/src/host_twg.rs` as `comment create --pull-request <id> --text T --path P` with `--line` for the new side or `--from-line` for the old side, exactly one set and never a range — taking option names from the `opts` schema, never the tool's examples (FR-042, FR-048, contracts/twg-cli.md §Trap)
- [ ] T078 [US4] Acknowledge submission within 100ms, post off the foreground thread, and render the posted comment at the line it was written on attributed to the reviewer, in `crates/pull_request_review/src/comments.rs` (FR-043, FR-067, FR-068)
- [ ] T079 [US4] On a failed post, state the reason, preserve the reviewer's text, and offer retry or cancel, in `crates/pull_request_review/src/comments.rs` (FR-046)
- [ ] T080 [US4] Cancelling a draft sends nothing and discards it, in `crates/pull_request_review/src/comments.rs` — nothing is persisted, so quitting with an open comment posts nothing (FR-004, FR-044)
- [ ] T081 [US4] Check `can_comment` before composing and tell the reviewer why commenting is unavailable on a merged or declined pull request, or one they lack permission on, in `crates/pull_request_review/src/comments.rs` (FR-047)
- [ ] T082 [US4] Post against the `against_revision` the reviewer was reading, or tell them the pull request moved, in `crates/pull_request_review/src/comments.rs` — never silently attached to a line it was not written about (FR-049)
- [ ] T083 [P] [US4] Test in `crates/pull_request_review/src/host_twg.rs` that a new-side comment sets `--line` and an old-side comment sets `--from-line`, never both and never a range, and that the resulting anchor has `start_*` null (FR-040, FR-042, FR-048, SC-011)
- [ ] T084 [P] [US4] Test in `crates/pull_request_review/src/comments.rs` that cancelling posts nothing, that an induced post failure preserves the body and retries successfully, and that two comments submitted in quick succession both post without either overwriting the other (FR-044, FR-046, SC-012, spec quick-succession edge case)

**Checkpoint**: Reading has become reviewing. Stories 1–4 replace the browser entirely for the common case.

---

## Phase 7: User Story 5 - Narrow and reorder the list (Priority: P2)

**Goal**: State filter, author filter including "me", and a reversible recency sort — all applied by the
host over the repository's whole set, all visible, all remembered across restarts.

**Independent Test**: With a repository whose pull requests span more than one author and more than one
state, switch the state filter to all, filter by a single author, reverse the sort, and confirm the listed
set and order match each choice; then reopen the project and confirm the choices were remembered.

**Depends on**: User Story 1 (the list it narrows).

- [ ] T085 [US5] Read and write `ListViewState` through `db::kvp` (`write_kvp`/`read_kvp`) under a feature-namespaced key including the worktree identity, in `crates/pull_request_review/src/state.rs`, falling back to defaults on unreadable or unparseable stored state (FR-021, research.md §8)
- [ ] T086 [US5] Write persisted state after a change settles rather than on every keystroke, in `crates/pull_request_review/src/state.rs`, so persistence never blocks the foreground thread (FR-067, data-model.md rules)
- [ ] T087 [US5] Add the state filter control in `crates/pull_request_review/src/list.rs` defaulting to `OpenAndDraft` and offering `All` (FR-013)
- [ ] T088 [US5] Apply filters and sort host-side in `crates/pull_request_review/src/host_twg.rs`: `OpenAndDraft` → `--state OPEN` (drafts are open pull requests with `draft: true`), `All` → one call per `OPEN|MERGED|DECLINED|SUPERSEDED` merged, paginating until the filter is satisfied over the whole set rather than filtering a partial page (FR-019, contracts/twg-cli.md §State filter mapping)
- [ ] T089 [US5] Add the author filter in `crates/pull_request_review/src/list.rs`, clearable, and clear all filters back to the default view in one action (FR-014, FR-016)
- [ ] T090 [US5] Store `AuthorFilter::Me` as a marker rather than a resolved account id and resolve it to the viewer's **nickname** at call time via `viewer`, in `crates/pull_request_review/src/list.rs` — `--author` matches a nickname (FR-015, contracts/twg-cli.md §Author filter)
- [ ] T091 [US5] Add the sort direction control in `crates/pull_request_review/src/list.rs`, defaulting to most recent first with a reversible direction and an indicator reflecting the current order (FR-017)
- [ ] T092 [US5] Show which filters and sort are in effect at all times in `crates/pull_request_review/src/list.rs`, and when they match nothing say so and offer to clear them rather than presenting an unexplained empty list — distinct from the repository having no pull requests at all (FR-018, FR-020, spec no-pull-requests edge case)
- [ ] T093 [US5] Keep a selected pull request selected across a filter or sort change when it is still listed, without blocking the editor, in `crates/pull_request_review/src/panel.rs` (US5 acceptance scenario 8, FR-067)
- [ ] T094 [P] [US5] Test in `crates/pull_request_review/src/host_twg.rs` that every filter and sort combination lists exactly the matching pull requests drawn from the whole fixture set rather than a partially loaded page (FR-019, SC-007)
- [ ] T095 [P] [US5] Test in `crates/pull_request_review/src/state.rs` that filters, sort and selected repository round-trip through `db::kvp` and fall back to defaults when the stored value is unparseable (FR-021, SC-008)

**Checkpoint**: The list is usable on a real repository, and the reviewer's choices survive a restart.

---

## Phase 8: User Story 6 - See the comments already on the pull request (Priority: P3)

**Goal**: Comments already on the pull request appear at their lines in the split diff, attributed, with
replies in order and collapsible — and the reviewer can reply into a thread rather than beside it.

**Independent Test**: On a pull request with three inline comment threads on two files, open both files and
confirm each thread appears at its line attributed to its author with its replies in order, then reply to
one and confirm the reply lands on that thread rather than as a new comment.

**Depends on**: User Story 3 (threads render in the diff) and User Story 4 (the compose surface replies reuse).

- [ ] T096 [P] [US6] Implement `PullRequestHost::comments` in `crates/pull_request_review/src/host_twg.rs` as `comment query <id> -n <high>` with the default 50 raised, parsing `content.raw`, `inline` (`to`/`start_to` new side, `from`/`start_from` old side), `parent`, `deleted` and `pending` — and no resolution field, because none exists (FR-050, FR-054, contracts/twg-cli.md §comment query)
- [ ] T097 [US6] Build threads from the host's parent links in `crates/pull_request_review/src/comments.rs`, with replies ordered oldest-first (FR-050, FR-051)
- [ ] T098 [US6] Render threads as collapsible blocks at their anchored lines in the split diff, attributed to their authors, in `crates/pull_request_review/src/comments.rs` — collapsible so they cannot push the code off screen, and with **no** resolve or reopen affordance and no resolved/unresolved styling (FR-050, FR-054)
- [ ] T099 [US6] Display a multi-line anchor read from the pull request over all the lines it covers, in `crates/pull_request_review/src/comments.rs` — reading a range is in scope even though creating one is not (FR-050, spec existing-range edge case)
- [ ] T100 [US6] Derive `is_outdated` locally by comparing a thread's anchor against the changeset being shown, in `crates/pull_request_review/src/comments.rs` — marked outdated, never re-anchored to a different line and never dropped (FR-053)
- [ ] T101 [US6] Show comments with no anchor in the Overview tab in `crates/pull_request_review/src/overview.rs` rather than dropping them (FR-052)
- [ ] T102 [P] [US6] Add `--reply-to <comment id>` to `post_comment` in `crates/pull_request_review/src/host_twg.rs`, so a reply is posted into its thread (FR-051)
- [ ] T103 [US6] Add the reply affordance on a thread in `crates/pull_request_review/src/comments.rs`, setting `DraftComment::reply_to` so the reply joins that thread rather than starting a new top-level comment (FR-051)
- [ ] T104 [US6] Keep the diff readable when comments cannot be loaded, state the reason, and still allow adding a comment, in `crates/pull_request_review/src/comments.rs` (FR-055)
- [ ] T105 [P] [US6] Test in `crates/pull_request_review/src/comments.rs` that the comment fixtures — an inline single line, an inline range, a reply with `parent` set, and an unanchored comment — produce the expected thread structure and placement, with the range displayed over its full span and the unanchored comment in Overview (FR-050 – FR-052)

**Checkpoint**: All six user stories are independently functional.

---

## Phase 9: Polish & Cross-Cutting Concerns

**Purpose**: The checks that verify the boundaries, the change surface and the constitution gates — most of
which are success criteria the spec states by number.

- [ ] T106 [P] Wire the FR-057 source check into the test suite in `crates/pull_request_review/src/host.rs`, asserting no host, platform or transport name appears in any type, field, error or user-facing string outside `host_twg.rs` and `host_process.rs` (FR-057, SC-013)
- [ ] T107 [P] Add a check in `script/check-fork-surface` that `crates/pull_request_review/` carries no second copy of the commit-diff machinery, so the feature's diff surface and Zed's commit-diff view demonstrably share one implementation (SC-022)
- [ ] T108 [P] Add a check that no resolve or reopen affordance and no resolved/unresolved distinction appears anywhere in `crates/pull_request_review/` — an action that appeared but did nothing would be worse than its absence (FR-054, quickstart Scenario 6)
- [ ] T109 [P] Audit `crates/pull_request_review/` for `unwrap()`, `expect()`, panicking indexing and `let _ =` on fallible operations in non-test code, and confirm `./script/clippy` is clean (constitution panic discipline, SC-016)
- [ ] T110 Verify each of the five files in `specs/001-pull-request-review/contracts/zed-surface.md` is changed strictly additively against the upstream merge-base — no signature change, no narrowed visibility, no moved item, no reordering or reformatting of existing code (FR-075, SC-018)
- [ ] T111 Verify that with `pull_request_review::init(cx)` removed from `crates/zed/src/main.rs` the repository builds and every existing Zed surface behaves identically to upstream (FR-076, SC-020)
- [ ] T112 [P] Measure Zed's startup and workspace-open time (`cargo run -p zed`, panel never opened), and the feature's foreground frame cost while its work is in flight, against the Principle II budgets (FR-067, FR-070, SC-003, SC-005, constitution gate 3)
- [ ] T113 Run the [quickstart.md](./quickstart.md) scenarios end to end, including Gate 0's negative case, Scenario 7's seven induced failure conditions on a **dock-launched** Zed, and Scenario 8's commit-view comparison (SC-006, SC-017, SC-021, FR-062a)
- [ ] T114 [P] Run the full suite: `cargo test -p pull_request_review`, `cargo test -p git_ui`, `cargo test -p project`, `./script/clippy`, `cargo fmt --check`, `script/check-fork-surface` (constitution gates 7 and 8)
- [ ] T115 Open the pull request with an imperative, correctly capitalised title, no conventional-commit prefix, no trailing punctuation, and a closing `Release Notes:` section stating which constitution principles are engaged and carrying the Principle I justification for the new crate and the two mandated traits (CLAUDE.md PR hygiene, constitution gates 1 and 8)

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies. T001 must land **first** — constitution gate 8 is unsatisfiable until the check exists, and building it first means the allowlist is correct from the first commit rather than reconstructed from a messy diff (plan.md, research.md §9).
- **Foundational (Phase 2)**: Depends on Setup. **Blocks every user story.**
- **User Story 1 (Phase 3)**: Depends on Foundational only.
- **User Story 2 (Phase 4)**: Depends on Foundational. Needs US1's panel shell (T023) for its detail section.
- **User Story 3 (Phase 5)**: Depends on Foundational. Needs US2's Files tab (T045) as the entry point and US2's `Changeset::revisions` (T042).
- **User Story 4 (Phase 6)**: Depends on User Story 3 — the comment is composed from the diff surface.
- **User Story 5 (Phase 7)**: Depends on User Story 1 only. **Independent of US2–US4** and can be built in parallel with them.
- **User Story 6 (Phase 8)**: Depends on User Story 3 (threads render in the diff) and reuses User Story 4's compose surface for replies.
- **Polish (Phase 9)**: Depends on all shipped stories.

### Cross-story file contention

Three files are touched by more than one story. Tasks in them are deliberately **not** marked `[P]` across
stories:

| File | Stories |
|---|---|
| `host_twg.rs` | US1 (`list`, `viewer`, approvals), US2 (`detail`, `changed_files`), US4 (`post_comment`), US5 (state filter fan-out), US6 (`comments`, `--reply-to`) |
| `panel.rs` | US1, US2, US5 |
| `comments.rs` | US4 (compose and submit), US6 (existing threads and replies) |

`detail` is split across two stories on purpose: US1 needs only `participants[]` for approval bubbles
(T028), US2 adds the description and branches (T040). Land T028 first; T040 extends it.

### Within Each User Story

- Host implementation before the UI that consumes it
- Boundary types before their implementations
- Core rendering before edge-case handling
- Tests alongside, not after — constitution gate 2 requires threading and cancellation coverage at merge

### Parallel Opportunities

- **Phase 1**: T007 and T008 in parallel once T003–T006 land
- **Phase 2**: T020, T021 and T022 in parallel; T009–T012 are four independent declarations but T009 and T010 share `host.rs`
- **US1**: T026 and T027 in parallel (independent host methods); T036, T037 and T038 in parallel
- **US2**: T041 and T042 in parallel; T051 and T052 in parallel
- **US3**: T053/T054 (project crate) and T055/T056 (git_ui crate) are two fully independent tracks and can be worked simultaneously; T067–T070 all in parallel
- **US4**: T077 in parallel with the compose work in `comments.rs`; T083 and T084 in parallel
- **US5**: T094 and T095 in parallel
- **US6**: T096 and T102 in parallel; T105 in parallel with the rendering tasks
- **Phase 9**: T106, T107, T108, T109, T112 and T114 all in parallel
- **Across stories**: after Foundational, US5 can be built alongside US2–US4 by a second developer

---

## Parallel Example: User Story 3

```bash
# The two allowlisted upstream additions are in different crates — fully independent:
Task: "Add pub fn loading blob content by revision in crates/project/src/git_store.rs"
Task: "Make CommitView::new pub and add the SplittableEditor accessor in crates/git_ui/src/commit_view.rs"

# All four US3 tests once the implementation lands:
Task: "Test the base is the divergence point (FR-034, SC-009)"
Task: "Test the FR-082 reused path"
Task: "Test branch, index, working tree and stash are unchanged (FR-035, SC-002)"
Task: "Test cancellation stops the blob load (FR-069, SC-016)"
```

---

## Implementation Strategy

### The gate comes first, always

T001 before any feature code. A surface constraint retrofitted onto an existing diff constrains nothing,
and constitution gate 8 blocks the merge until the check passes. T002 proves it bites — a check that
cannot fail is not a check.

### MVP scope

**Minimum shippable**: Setup + Foundational + User Story 1 (T001–T038, 38 tasks). A reviewer gets "what is
waiting for me" in Zed without a browser. Stop here and validate against US1's independent test.

**The full P1 increment**: through User Story 3 (T001–T070, 70 tasks). The spec marks US1, US2 and US3 all
P1 because US1 and US2 without US3 send the reviewer back to a browser to read the actual code. This is the
first genuinely complete deliverable.

### Incremental delivery

1. Setup + Foundational → the boundaries exist and the change surface is enforced
2. Add US1 → the list works → validate → demo (MVP)
3. Add US2 → a complete read-only review surface → validate → demo
4. Add US3 → the diff, in Zed's own viewer → validate → demo (**the full P1 increment**)
5. Add US4 → reading becomes reviewing → validate → demo
6. Add US5 → usable on a real repository → validate → demo
7. Add US6 → existing threads and replies → validate → demo
8. Polish → the FR-075/FR-078/SC-013/SC-022 checks and the quickstart run

Every step adds value without breaking the previous one.

### Parallel team strategy

After Foundational completes:

- Developer A: US1 → US2 → US3 (the P1 spine, strictly ordered)
- Developer B: US5 once US1's list exists — independent of US2–US4
- Developer C: the US3 upstream additions (T053–T056) in parallel with Developer A's US1/US2 work, since they are in `crates/project` and `crates/git_ui` and touch nothing Developer A is in

US4 and US6 are sequential after US3 and are best taken by whoever finished the spine.

---

## Notes

- `[P]` = different files, no dependencies on incomplete tasks
- `[Story]` maps each task to a user story for traceability; Setup, Foundational and Polish carry no story label
- Every task cites the requirement it discharges, so a failing task points at what it breaks
- Run `script/check-fork-surface` after every task that touches a pre-existing file — it is the cheapest failure to catch
- Async tests use `#[gpui::test]` with `run_until_parked()` and the GPUI executor's timers, never `smol::Timer::after` and never wall-clock sleeps (CLAUDE.md, constitution gate 2)
- Host interaction is tested against the Phase 0 fixtures, so the suite needs no network and no Bitbucket access
- Commit after each task or logical group; stop at any checkpoint to validate a story independently
