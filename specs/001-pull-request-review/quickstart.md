# Quickstart: validating Pull Request Review

**Plan**: [plan.md](./plan.md) | **Date**: 2026-09-06

Runnable checks that prove the feature works end to end. Each scenario names the requirements and success
criteria it validates, so a failure points at what it breaks.

## Prerequisites

1. **A Bitbucket-hosted repository open in Zed.** The feature lists pull requests for the repository the
   open project maps to. `atlassian/twg-cli` was used for Phase 0 verification.
2. **`twg` installed and authenticated.**
   ```sh
   twg whoami          # must print your account, not an auth prompt
   twg --version       # 1.0.1 or later
   ```
3. **A local Zed build.**
   ```sh
   ./script/clippy                       # must be clean
   cargo run -p zed                      # or your usual run target
   ```

## Gate 0 — the change surface

Run first, and on every change. It is constitution gate 8 and the cheapest failure to catch.

```sh
script/check-fork-surface
```

**Expect**: exit 0, listing the allowlisted files it found modified.

Then prove it actually bites — a check that cannot fail is not a check:

```sh
echo "// scratch" >> crates/editor/src/editor.rs   # not on the allowlist
script/check-fork-surface                          # must FAIL, naming this file
git checkout crates/editor/src/editor.rs
```

Validates FR-078, FR-078a, FR-078b, SC-017.

## Scenario 1 — the pull request list

1. Open the command palette, run the action that opens the Pull Requests panel.
2. Observe the list.

**Expect**: only open and draft pull requests, most recently active first. Each row shows a status
indicator distinguishing open/draft/merged/declined, the title, the author, and a relative age. Rows
appear before approval bubbles do — the bubbles fill in shortly after (this is the two-phase load, not a
defect). A pull request with no approvals shows an empty approval area, not a placeholder.

Validates FR-006 – FR-010, FR-012, SC-004.

**Startup cost** — before opening the panel at all, confirm the feature is inert:

```sh
# Launch Zed, open a workspace, never open the panel.
# Expect: no twg process spawned, no network activity from the feature.
```

Validates FR-003, FR-070, SC-005.

## Scenario 2 — Overview and Files tabs

1. Select a pull request with a multi-paragraph markdown description and a dozen changed files including
   an add, a delete and a rename.

**Expect**: the detail section opens within 100ms with Overview selected. The description renders as
formatted text, not raw markup. Each reviewer's approval state is shown. A browser link is present.
Switching to Files and back reloads nothing.

2. Open the Files tab.

**Expect**: every changed file, each with its change kind and added/removed line counts, plus a total. The
renamed file shows both old and new paths. No file's diff content has been fetched yet.

3. Select a pull request with an empty description, and one with no changed files.

**Expect**: "no description" and "nothing to review" respectively — never a blank area that looks like a
failed load.

Validates FR-022 – FR-031, SC-003.

## Scenario 3 — the split diff

1. Open a changed file from the Files tab.

**Expect**: Zed's own split diff viewer, showing the destination side beside the pull request side. Your
theme, font, editor settings and keymap apply. The code is syntax-highlighted. The unified/split toggle
works. Go-to-definition and search behave as in any editor.

2. Compare against a diff opened by Zed's existing git surfaces.

**Expect**: the same surface, not a lookalike.

Validates FR-032, FR-033, SC-010.

3. **The base is the divergence point.** Use a pull request whose destination branch has advanced by ten
   or more commits since it was raised.

**Expect**: only what the pull request proposes. None of the destination branch's later changes appear —
not in the file list, not in the diff.

Validates FR-034, SC-009. This is the single most important correctness check in the feature; getting it
wrong shows the reviewer a plausible diff of the wrong thing.

4. **Nothing is disturbed.** With uncommitted changes on your own branch:

```sh
git rev-parse --abbrev-ref HEAD && git status --porcelain > /tmp/before
# open the panel, select 20 pull requests, open a diff from each
git rev-parse --abbrev-ref HEAD && git status --porcelain > /tmp/after
diff /tmp/before /tmp/after      # must be empty
git worktree list                # must show no new worktree
```

Validates FR-035, SC-002, SC-015.

5. Open an added file, a deleted file, and a binary file.

**Expect**: wholly added, wholly removed, and a stated refusal with its reason — never a missing-file
error or a hang.

Validates FR-036, FR-037.

## Scenario 4 — commenting

Use a pull request you are willing to comment on. **This scenario posts real comments.**

1. Put the cursor on a line in a file's diff, invoke the comment action, type, submit.

**Expect**: acknowledged within 100ms. The comment appears at that line, attributed to you. Verify against
the pull request itself:

```sh
twg bitbucket pull-requests comment query <id> -w W -r R -o json \
  | jq '[.[] | select(.inline != null)] | last | {inline, raw: .content.raw}'
```

**Expect** `inline.to` set to your line and `inline.start_to` null — a single-line anchor, which is what
the feature creates (FR-040, FR-042).

Validates FR-040, FR-042, FR-043, SC-011.

2. Select several lines, then invoke the action. **Expect**: the comment anchors to one line of the
   selection, and the compose UI shows which line before you submit. No range is attempted. (FR-041)
3. Type a comment, cancel. **Expect**: nothing posted. (FR-044)
4. Submit an empty/whitespace comment. **Expect**: refused before anything is sent. (FR-045)
5. Comment on the removed side. **Expect**: posted against the old side — `inline.from` set and
   `inline.to` null in the JSON above. (FR-042, FR-048)
6. Induce a failure — disconnect the network, then submit. **Expect**: reason stated, your text preserved,
   retry available. Reconnect and retry; it posts. (FR-046, SC-012)
7. Try to comment on a merged pull request. **Expect**: told before composing, not after submitting.
   (FR-047)

## Scenario 5 — filters and sort

1. Switch the state filter to all. **Expect**: merged and declined appear. (FR-013)
2. Filter by an author. **Expect**: only their pull requests; clearable. (FR-014, FR-016)
3. Filter by yourself without typing your account name. **Expect**: works. (FR-015)
4. Apply a filter matching nothing. **Expect**: says so, offers to clear — not a bare empty list. (FR-020)
5. Reverse the sort. **Expect**: least recently active on top, indicator reflects it. (FR-017)
6. Confirm the active filters are visible. (FR-018)
7. **Whole-set filtering** — on a repository with more pull requests than one page, filter to a state you
   know exists only beyond the first page. **Expect**: it appears. A filter applied to a partial page is
   the bug this catches. (FR-019, SC-007)
8. Restart Zed. **Expect**: same filters, sort, and selected repository. (FR-021, SC-008)

## Scenario 6 — existing comments

1. Open a file carrying inline comment threads.

**Expect**: each thread at its line, attributed, replies in order. Threads collapse. A thread anchored to
a line the pull request has since changed is marked outdated, not silently moved.

**Expect no resolve affordance at all** — resolving threads is out of scope (FR-054), so there must be no
resolve or reopen action, and no resolved/unresolved styling. An action that appeared but did nothing would
be worse than its absence.

**Also confirm reading a range still works.** Find a comment created elsewhere with a multi-line anchor
(`inline.start_to` non-null) and open its file. **Expect**: it displays over all the lines it covers. The
feature cannot *create* a range but must still *show* one (FR-050).

2. Reply to a thread. **Expect**: the reply joins that thread, not a new top-level comment. Verify:

```sh
twg bitbucket pull-requests comment query <id> -w W -r R -o json \
  | jq 'last | {parent: .parent.id, raw: .content.raw}'   # parent must be set
```

Validates FR-050 – FR-055.

## Scenario 7 — failure modes

Each must produce a **different**, actionable message, reported in the panel without a modal or focus
theft, leaving Zed fully usable. Seven distinct conditions (FR-064, SC-006):

| Condition | How to induce |
|---|---|
| Prerequisite missing | rename the `twg` binary temporarily |
| Not authenticated | `twg logout` |
| Credential expired | an expired session |
| Permission denied | a repository you cannot read |
| Unreachable | disconnect the network |
| Rate limited | repeated rapid refreshes, if the host cooperates |
| Unexpected response | point the override env var at a script emitting `{`  |

**The first two must not be conflated** — a tool you can run in your terminal must never be reported as
missing. Specifically test a **dock-launched** Zed, not one started from a shell, which is the case
FR-062a exists for:

```sh
open -a Zed     # macOS: launches without your shell PATH
```

Validates FR-062a, FR-062b, FR-064, FR-071, FR-073, SC-006.

**Remote projects**: open a remote project and open the panel. **Expect**: an explicit statement that pull
request review is unavailable for remote projects, with the reason — never an empty list (research.md §3).

## Scenario 8 — the reuse did not disturb the commit view

Because `git_ui/src/commit_view.rs` is on the allowlist, this must be checked directly.

1. Open an ordinary commit in Zed's commit-diff view (via the git panel's history).
2. Do it for a commit that adds a file, one that modifies, one that deletes, one touching a binary file,
   and a shallow-boundary commit.

**Expect**: identical behaviour to upstream in every case. Ideally compare against a build with the
feature's `init` call removed.

Validates FR-076, FR-081, SC-020, SC-021.

## Test suite

```sh
cargo test -p pull_request_review          # unit + host fixtures + gpui tests
cargo test -p git_ui                       # the reused commit view still passes
cargo test -p project                      # the added blob-loading method
./script/clippy                            # must be clean
cargo fmt --check
script/check-fork-surface                  # gate 8
```

Host interaction is tested against the JSON fixtures captured in Phase 0
([contracts/twg-cli.md](./contracts/twg-cli.md) §Test fixtures), so the suite needs no network and no
Bitbucket access. Threading and cancellation use `#[gpui::test]` with `run_until_parked()` and the GPUI
executor's timers — never wall-clock sleeps.
</content>
