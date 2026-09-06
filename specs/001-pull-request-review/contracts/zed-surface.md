# Contract: Zed surface — the FR-078 allowlist

**Discharges**: spec FR-074 – FR-082 | **Enforced by**: `script/check-fork-surface` (FR-078a)

This repository is a fork that continues to merge upstream Zed. This file is the complete list of
pre-existing files the feature may modify. Anything not on this list must not be touched; the check fails
the build if it is.

Adding an entry is a deliberate, reviewed change — not something that happens quietly. Every entry states
why a copy was not used instead, because FR-077 requires that judgement to be recorded rather than
assumed.

## The allowlist

### 1. `Cargo.toml` (workspace root)

**Change**: add `crates/pull_request_review` to `members`; add the crate to `[workspace.dependencies]`.
**Additive**: yes — new lines only. No existing dependency version changes (FR-079).
**Conflict risk**: low. Both are long alphabetical lists where additions merge cleanly.

### 2. `Cargo.lock`

**Change**: the lock entries for the new crate and for the workspace crates that now depend on it.
**Additive**: yes — new package stanzas and new dependency lines. No existing version is changed
(FR-079), which is what keeps this from being a dependency bump in disguise.
**Why not a copy**: not applicable; the lock file is generated. It is enumerated because it is a
pre-existing tracked file, and a gate that quietly ignored generated files would be a gate with a
hole in it.
**Conflict risk**: low, and mechanical. Lock conflicts resolve by regenerating.

> Missed in the original enumeration, which listed `Cargo.toml` but not the lock file it implies.
> The gate caught it on the first run after the crate was added — which is the argument for FR-078a
> in miniature: the allowlist was wrong, and only automatic enforcement said so.

### 3. `crates/zed/Cargo.toml`

**Change**: one dependency line on the new crate.
**Additive**: yes.
**Conflict risk**: low.

### 4. `crates/zed/src/main.rs`

**Change**: one line, `pull_request_review::init(cx);`, beside the existing `git_ui::init(cx);` at
`main.rs:772`.
**Additive**: yes — one statement added to an existing sequence of `init` calls; no existing call altered.
**Conflict risk**: low-to-moderate. The init block does change upstream, but a single added line in a list
of sibling statements is the cheapest possible conflict to resolve.

> `git_ui::init` is also called from `crates/zed/src/zed.rs:6170` and
> `crates/zed/src/visual_test_runner.rs:212`. The feature registers in `main.rs` **only**. Registering in
> three places would triple the allowlist for no benefit; the visual test runner does not need this panel.

### 5. `crates/git_ui/src/commit_view.rs`

**Change**: make `CommitView::new` (`:291`) `pub`; add one `pub fn` accessor returning the view's
`SplittableEditor`.
**Additive**: yes — one visibility widening and one new method. No signature change, no moved item, no
reformatting.
**Why not a copy**: spec FR-080 — the user's clarification Question 5 decision. Reuse means one
implementation of the multibuffer/blob-diff assembly instead of two.
**Conflict risk**: **highest on this list, and knowingly accepted.** This file is actively developed
upstream. FR-081 requires the ordinary commit path to behave identically, FR-082 requires the widened
surface to stay minimal and a test to cover the reused path, and SC-021 verifies the commit view across
added, modified, deleted, binary and shallow-boundary files.

> Two things make this affordable. `CommitView::new` takes only types the feature can construct, because
> `CommitDetails` and `CommitDiff` have entirely public fields — so no further widening is needed.
> `GitBlob` stays private, since it is used only inside `new`. If upstream churn here becomes painful,
> reverting to a parallel implementation in the feature crate is the documented escape hatch
> (research.md §5).

### 6. `crates/project/src/git_store.rs`

**Change**: add one `pub fn` loading blob content for a list of `<revision>:<path>` specifiers, returning
an explicit unsupported error for remote repositories.
**Additive**: yes — one new method on an existing `impl`.
**Why not a copy**: FR-077. The alternative is shelling out to `git cat-file` from the feature crate,
which bypasses Zed's repository abstraction and its remote path — exactly the silent divergence FR-077
exists to prevent.

> **This entry is a choice, not a necessity.** `RepositoryState`, `LocalRepositoryState.backend` and
> `Repository::send_job` are all public, so the feature crate could reach the backend's
> `load_revisions` without this entry — research.md §3 originally claimed otherwise and has been
> corrected. The entry is kept because reaching in would put the local/remote dispatch, and the
> "unsupported for remote projects" arm, inside a crate that should not know that shape. If the
> allowlist ever needs shrinking, this is the cheapest entry to give up.
**Conflict risk**: moderate. The file is large and busy, but a method appended to an `impl` block merges
cleanly far more often than an edit inside existing logic.

> **No proto change.** Supporting remote projects would need a new proto message and remote-server
> handling — two more high-conflict allowlist entries. Instead the method fails explicitly for remote
> repositories, which is what the constitution requires of a surface that cannot work remotely
> (research.md §3).

### 7. `README.md`

**Change**: the two-line `> [!IMPORTANT]` review-confirmation banner at the top of the file.
**Additive**: yes — two prepended lines, no existing content altered.
**Why not a copy**: not applicable; this is not a code reuse decision. `CLAUDE.md` mandates the banner on
any branch with source changes, and states that only the human author may remove it — it is the manual
acknowledgement that the change was reviewed.
**Conflict risk**: negligible, and self-clearing. The banner is *designed* to be deleted before the pull
request is submitted, so unlike every other entry on this list it does not persist into the merged fork.
It is enumerated rather than exempted in the check so that the allowlist stays the single place the change
surface is described.

> **A governance discrepancy, raised rather than resolved in code.** Constitution Principle VI would
> otherwise forbid this file, and the constitution's Governance section says it wins over `CLAUDE.md` and
> that the conflict must be raised. It is raised here. The entry is granted because the banner is a
> transient process marker rather than the permanent divergence FR-078 exists to prevent, and because
> removing it is explicitly not the agent's decision to make. If the repository would rather the gate
> never see it, the alternative is an exemption in `script/check-fork-surface` for `README.md`.

## Files the feature adds

New files are not on the allowlist and never trip the check, because a file the fork created cannot
conflict with upstream.

```
crates/pull_request_review/**      # the whole crate
script/check-fork-surface          # the FR-078a check itself
specs/001-pull-request-review/**   # this documentation
```

## What the check must do

`script/check-fork-surface`, following the convention of `script/check-keymaps`, `check-licenses` and
`check-todos`:

1. Determine the merge-base of the current branch with the configured upstream ref.
2. List paths modified relative to that merge-base.
3. Exclude paths **added** by the fork — FR-078b, so the feature's own crate never trips the check.
4. Fail if any remaining path is not on the allowlist above, naming the offending paths.
5. Exit non-zero on failure, so it can serve as constitution gate 8.

**Not hard-coded**: the upstream remote and ref are configuration. The sandbox used during Phase 0 could
not read `.git`, so the actual remote name was never verified — assuming `upstream/main` would be a guess
baked into a build gate.

**Beyond the file list**: the check verifies *which* files changed. FR-075's stricter "strictly additive"
property — no signature change, no narrowed visibility, no moved item, no reformatting — is verified in
review against the merge-base diff, per SC-018. Automating that is possible but out of scope here; the
file-level gate is what stops surface creep, which is the failure mode that actually happens.
</content>
