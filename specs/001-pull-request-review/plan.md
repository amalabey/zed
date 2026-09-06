# Implementation Plan: Pull Request Review

**Branch**: `amal/pull-requests` | **Date**: 2026-09-06 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `specs/001-pull-request-review/spec.md`

## Summary

Add a Pull Requests panel to Zed that lists a repository's pull requests, shows a selected pull
request's Overview and Files tabs, opens each changed file in Zed's own split diff viewer, and posts
inline comments back to the pull request.

The technical approach is shaped by three findings from Phase 0 research, each verified against the
running tools rather than assumed:

1. **The diff is assembled locally, not rendered from a patch.** `Repository::diff_tree(DiffTreeType::MergeBase { base, head })`
   already returns the merge-base file list for two arbitrary revisions, and
   `project::git_store::CommitDiff` / `CommitFile` are plain structs with entirely public fields. So the
   feature fetches the pull request's refs, computes the merge-base tree diff, loads the two sides' blob
   content, synthesises a `CommitDiff`, and hands it to Zed's existing commit-diff view. The reviewer gets
   real buffers — syntax highlighting, go-to-definition, search — with no bespoke diff rendering.

2. **The change surface is five files.** Reusing the commit-diff view (spec FR-080, the user's Question 5
   decision) needs `CommitView::new` made public and one editor accessor added. The only genuinely new
   upstream capability required is loading blob content by revision, which no public project-layer API
   exposes. Everything else the feature needs is already public.

3. **The host tool's limits shaped the scope rather than the implementation.** `twg` cannot post a
   multi-line comment anchor and does not report thread resolution. Both were resolved by taking the
   feature out of scope — comments are single-line and threads are not resolvable — so the implementation
   carries no workaround for either. Its listing also omits per-reviewer approval state, which is what
   forces the two-phase list load. All three are recorded in [research.md](./research.md) §7.

Both extensibility boundaries the spec mandates (FR-056 host, FR-058 changeset) are plain traits with one
implementation each, declared in the new crate and cited where they are defined.

## Technical Context

**Language/Version**: Rust, edition 2024, toolchain 1.97.1 pinned in `rust-toolchain.toml` (unchanged — spec FR-079)

**Primary Dependencies**: Workspace-internal only — `gpui`, `ui`, `editor`, `workspace`, `project`, `git`,
`git_ui`, `language`, `markdown`, `db`, `util`, `anyhow`, `serde`, `serde_json`, `futures`, `smol`. No new
third-party dependency; `serde_json` is already a workspace dependency, which is what FR-079 requires.

**External tool**: `twg` ≥ 1.0.1 (observed 1.0.1; 1.2.7 available), invoked with `-o json`. Resolved
through the project's shell environment per FR-062a. Not a build dependency — absence is a runtime
condition, not a compile error.

**Storage**: Zed's existing key-value store (`db::kvp`, `write_kvp`/`read_kvp`) for view state per
FR-021 and FR-002a. No feature-owned file, no settings schema entry, no review state persisted (FR-004).

**Testing**: `cargo test -p pull_request_review`, with `#[gpui::test]` driven by `run_until_parked()` and
the GPUI executor's timers for anything asynchronous. Host interaction is tested against recorded `twg`
JSON fixtures captured in Phase 0, so the wire contract is pinned without touching the network.

**Target Platform**: macOS, Linux, Windows. Remote projects are explicitly **unsupported in this phase**
and fail with a stated reason — see research.md §3 for why, and note this is the spec's required
behaviour for a surface that cannot work remotely, not a silent gap.

**Project Type**: Desktop application feature — one new crate inside the existing Zed Cargo workspace.

**Performance Goals**: Foreground thread work under 8ms per frame; detail section within 100ms of
selection; diff open and comment submit acknowledged within 100ms; first list rows readable within 2s for
500 pull requests; zero contribution to startup before first panel open.

**Constraints**: Fork-maintenance constraints dominate. Five pre-existing files on the allowlist, every
change strictly additive, enforced automatically (FR-074 – FR-082). No panics in non-test code. No
repository content egress except one reviewer-submitted comment.

**Scale/Scope**: One repository at a time; up to ~500 pull requests listed; pull requests up to a few
thousand changed files. Host payload measured at ~19 KB of JSON per pull request, so a 500-row list is
roughly 9.5 MB parsed off-thread — see research.md §2 for the two-phase load this forces.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Checked against constitution **v1.1.0** (amended 2026-09-06 to add Principle VI).

| Principle | Status | How this design satisfies it |
|---|---|---|
| **I. Simplicity First** | **PASS** | One new crate, no new third-party dependency. Two traits are introduced, each with one implementation — permitted only because spec FR-056 and FR-058 mandate them **by number** and name the future sources (GitHub; branch comparison). Both are plain traits with no registry, no dynamic discovery and no extension point (FR-060), and each cites its requirement where declared. No review logic is reimplemented. |
| **II. Responsiveness** | **PASS** | Every `twg` invocation, git read, blob load and diff computation runs under `cx.background_spawn`. Two-phase list load renders rows before approvals arrive. Nothing is constructed at startup (FR-003, FR-070). Cancellation drops the subprocess, not just the task — see research.md §6. |
| **III. UX Discipline** | **PASS** | Zed's own panel, its own commit-diff view and split diff editor, `ui` crate components, declared actions in the command palette. No modal, no focus theft. Failures reported in the feature's own surface. |
| **IV. ACP Protocol Conformance** | **N/A** | This phase runs no agent and speaks no ACP. The FR-058 changeset boundary is the seam a later agent-driven review would consume. Recorded so the omission is deliberate rather than overlooked. |
| **V. Signal Over Noise** | **N/A (partial)** | No findings are produced this phase. The principle's spirit applies to the comment surface: existing threads are shown so the reviewer does not restate points already made (FR-050). |
| **VI. Minimal Upstream Disruption** | **PASS, with one accepted cost** | Feature code in its own crate; five allowlisted pre-existing files; every change strictly additive; enforced by a new check script. The accepted cost is that `git_ui/src/commit_view.rs` — an actively-changing file — is permanently allowlisted, which is the user's explicit Question 5 decision (FR-080), bounded by FR-081, FR-082 and SC-021. |

**Gate 8 (Change surface) is not yet satisfiable**: the allowlist check does not exist. It is task T001 of
Phase 2 and must land before feature code, so the surface constraint is enforced from the first commit
rather than retrofitted. This is a sequencing requirement, not a violation.

### Platform & technology constraints

| Constraint | Status |
|---|---|
| GPUI only, no second UI toolkit or web view | **PASS** |
| Rust edition 2024, pinned toolchain unchanged | **PASS** |
| No `unwrap()` / `expect()` / panicking indexing in non-test code | **PASS** — enforced by `./script/clippy`; all host output treated as untrusted and parsed fallibly (FR-065) |
| Remote projects work or fail explicitly with a reason | **PASS** — fails explicitly; research.md §3 |
| Third-party dependencies minimal, workspace-declared | **PASS** — none added |
| Settings through Zed's schema, never an ad-hoc file | **PASS (vacuous)** — no settings this phase (FR-066) |
| Credentials via `credentials_provider` / OS keychain | **PASS (vacuous)** — the feature holds no credential; `twg` owns authentication (FR-063) |
| Content egress only to configured agent, or per-item to a host | **PASS** — one comment, on one explicit action (FR-072) |
| New crate conventions: explicit `[lib] path`, no `mod.rs` | **PASS** — `[lib] path = "src/pull_request_review.rs"` |

## Project Structure

### Documentation (this feature)

```text
specs/001-pull-request-review/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output
│   ├── README.md
│   ├── pull-request-host.md      # The FR-056 boundary
│   ├── changeset.md              # The FR-058 boundary
│   ├── twg-cli.md                # Observed host CLI contract + JSON shapes
│   └── zed-surface.md            # The FR-078 allowlist and each additive change
├── checklists/
│   └── requirements.md
└── tasks.md             # Phase 2 output (/speckit-tasks — NOT created here)
```

### Source code

```text
crates/pull_request_review/                 # NEW — all feature code
├── Cargo.toml                              #   [lib] path = "src/pull_request_review.rs"
└── src/
    ├── pull_request_review.rs              # Crate root: init(), actions, panel registration
    ├── host.rs                             # FR-056 boundary: PullRequestHost trait + domain types
    ├── host_twg.rs                         # The one implementation: twg CLI, JSON parsing
    ├── host_process.rs                     # Executable resolution (FR-062a), subprocess + cancellation
    ├── changeset.rs                        # FR-058 boundary: Changeset trait + ChangedFile
    ├── changeset_pull_request.rs           # The one implementation: merge-base tree diff + blobs
    ├── panel.rs                            # Panel impl, list + detail layout, view state (FR-021)
    ├── list.rs                             # Rows, status/draft indicators, approval bubbles, filters, sort
    ├── overview.rs                         # Overview tab: markdown description, approvals, link
    ├── files.rs                            # Files tab: changed-file list, counts, rename display
    ├── diff.rs                             # Opens the reused commit-diff view for one changed file
    ├── comments.rs                         # Comment blocks, compose, submit, reply, outdated marking
    └── state.rs                            # kvp persistence of view state

script/check-fork-surface                   # NEW — FR-078a allowlist enforcement (gate 8)
```

### Allowlisted pre-existing files

Five files, each change strictly additive per FR-075. Full detail in
[contracts/zed-surface.md](./contracts/zed-surface.md).

| File | Additive change | Why not a copy instead |
|---|---|---|
| `Cargo.toml` | Workspace member + workspace dependency entry | Mechanically required |
| `crates/zed/Cargo.toml` | Dependency on the new crate | Mechanically required |
| `crates/zed/src/main.rs` | One `pull_request_review::init(cx);` call | Mechanically required |
| `crates/git_ui/src/commit_view.rs` | `CommitView::new` → `pub`; add a `pub fn` editor accessor | FR-080 (user's Question 5 decision): reuse rather than duplicate the diff machinery |
| `crates/project/src/git_store.rs` | Add one `pub fn` loading blob content by revision | FR-077: copying would require shelling out to git directly, diverging from upstream's repository abstraction |

**Structure Decision**: One new crate, `crates/pull_request_review`, holding every line of feature code,
with the five allowlisted additive touch points above. This is Principle VI and spec FR-074 applied
directly. The internal file split follows the two mandated boundaries: `host.rs` / `host_twg.rs` for
FR-056 and `changeset.rs` / `changeset_pull_request.rs` for FR-058, with the UI modules depending only on
the boundary traits so FR-057's "no host name above the seam" is checkable by grep.

## Complexity Tracking

| Violation | Why Needed | Simpler Alternative Rejected Because |
|---|---|---|
| Two traits with one implementation each (`PullRequestHost`, `Changeset`) | Spec FR-056 and FR-058 mandate them by number and name the future sources (GitHub; branch comparison). Principle I's single exception permits exactly this. | Concrete types would satisfy this phase, but the spec's numbered requirement is the deliverable, and SC-013/SC-014 verify the seams exist. Both are plain traits — no registry, no discovery (FR-060). |
| `git_ui/src/commit_view.rs` on the allowlist | FR-080: the user chose reuse over duplication in clarification Question 5. | Building a parallel diff item (the Phase-0 recommendation) keeps this volatile file off the allowlist but carries ~200 duplicated lines. The user weighed this and chose one implementation. Bounded by FR-081, FR-082, SC-021. |
| Adding a public blob-loading method to `crates/project` | No public project-layer API loads blob content by revision, and the diff cannot be built without it. | Shelling out to `git` from the feature crate avoids the touch point but bypasses Zed's repository abstraction and its remote path — exactly the divergence FR-077 forbids. |
| Two-phase list load (rows, then approvals) | The host's list call omits approval state; fetching it needs one call per pull request. | A single hydrated call does not exist. Blocking the list on N calls would break SC-004 and FR-012. |

## Constitution Re-Check (post-design)

Re-evaluated after Phase 1. No gate changed verdict; the design tightened three of them.

| Principle | Verdict | What Phase 1 changed |
|---|---|---|
| I. Simplicity First | **PASS** | Improved. The widening needed to reuse the commit-diff view turned out to be one visibility keyword plus one accessor, because `CommitDetails` and `CommitDiff` have public fields (research.md §5). No new dependency; `serde_json` is already in the workspace. |
| II. Responsiveness | **PASS** | Tightened. The measured ~19 KB-per-pull-request payload made the two-phase load mandatory rather than merely preferable, and cancellation is now specified as killing the subprocess, not just dropping the `Task` (research.md §6). |
| III. UX Discipline | **PASS** | Unchanged. Two host gaps mean the UI must *state* a limitation rather than silently misrepresent a result — which is what the capability-reporting section of the host contract exists for. |
| IV. ACP Conformance | **N/A** | Unchanged; no agent this phase. |
| V. Signal Over Noise | **N/A (partial)** | Unchanged. |
| VI. Minimal Upstream Disruption | **PASS** | Tightened. The allowlist is now five named files with a stated conflict risk and a recorded FR-077 justification for each ([contracts/zed-surface.md](./contracts/zed-surface.md)). One candidate touch point — a proto message for remote blob access — was **removed** from the design by failing explicitly for remote projects instead (research.md §3), which is what kept the list at five. |

**Unchanged from the pre-design check**: gate 8 is not yet satisfiable, because `script/check-fork-surface`
does not exist. It must be the first task.

### Open items — none

Phase 0 found three host capability gaps (research.md §7). All three are now settled and the plan is ready
for `/speckit-tasks`.

1. **Multi-line comment anchors — closed by decision.** Comments are single-line (FR-040, FR-042, FR-048).
   The tool's `--line` / `--from-line` map directly, with no collapsing and no approximation. Reading a
   range stays in scope and full-fidelity, because pull requests created elsewhere carry them (FR-050).
2. **Thread resolution — closed by decision.** Out of scope (FR-054). The feature neither reads nor sets
   resolution and offers no resolve action. Threading itself — replies joining their thread — remains
   required and is supported via the host's `parent` field and `--reply-to`.
3. **Approval hydration cost — no spec change needed.** The two-phase load satisfies FR-007 and FR-012 as
   written.

**A simplification fell out of decision 1.** The host boundary previously needed a capability-reporting
mechanism so implementations could declare whether they supported ranges and resolution. With both out of
scope, nothing varies between implementations, so the mechanism was **removed** and the trait is uniform.
That mechanism would have been an extension point with a single case — which constitution Principle I
refuses, and which no numbered requirement mandated the way FR-056 and FR-058 mandate the two boundaries.
Narrowing the requirement removed code rather than adding a workaround.
</content>
