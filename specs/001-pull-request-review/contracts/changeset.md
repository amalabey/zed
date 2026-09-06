# Contract: `Changeset`

**Discharges**: spec FR-058, FR-059, FR-061 | **Declared in**: `crates/pull_request_review/src/changeset.rs`

The single boundary supplying the change under review. FR-058 mandates it by number so that a second
changeset kind — **branch comparison**, and later commit ranges or the working tree — can be added by
supplying another implementation of this trait alone.

This citation must appear as a doc comment on the trait, per constitution Principle I.

## Shape

```
trait Changeset: Send + Sync {
    fn title(&self)                 -> SharedString
    fn files(&self)                 -> Task<Result<Vec<ChangedFile>>>
    fn file_diff(&self, path)       -> Task<Result<FileDiff>>
    fn revisions(&self)             -> Task<Result<RevisionPair>>   // resolved base + head
}
```

`RevisionPair` holds fully-resolved object ids. Resolution happens inside the implementation, because a
host may report an abbreviated revision (12 characters observed — see [twg-cli.md](./twg-cli.md)) and
every consumer needs the full one.

## Obligations on every implementation

1. **Nothing about pull requests.** This trait must be implementable by branch comparison without
   contortion, which means it may not mention a pull request, a host, or a reviewer (FR-061). It is a pair
   of revisions and the files that differ between them.
2. **`files()` is cheap; `file_diff()` is not.** `files()` must not fetch or prepare any file's content
   (FR-030). Content is loaded only when the reviewer opens a file, one file at a time.
3. **The base is the divergence point.** `revisions()` returns the merge base of head and destination as
   `base`, never the destination tip. This is what makes FR-034 hold — the reviewer sees what the change
   proposes, not what the destination branch acquired afterwards. An implementation that returns the
   destination tip satisfies the type signature and violates the contract.
4. **Refusals are data, not errors.** A file that cannot be rendered — binary, too large, submodule,
   symlink — is reported through `ChangedFile::render_refusal` at list time, so the Files tab can mark it
   before the reviewer opens it (FR-037). `file_diff()` on such a file returns the refusal, not an error.
5. **Read-only, and it stays that way.** No implementation may check out, switch branches, stash, or
   modify the working tree or index (FR-035). Fetching objects into the repository is permitted; changing
   what the reviewer has checked out is not.
6. **Off the foreground thread, cancellable** (FR-067, FR-069).
7. **Never panics** on a missing revision, an unreadable blob, or an encoding it cannot interpret.

## Why the boundary is drawn here and not elsewhere

The tempting alternative was to let the diff surface consume the host boundary directly — the host can
already list changed files, after all. That was rejected because it would weld the diff surface to the
existence of a pull request host, so branch comparison could not reuse any of it, and FR-061 would fail.

The division of labour that results is worth stating, because it is not obvious and it is easy to get
backwards during implementation:

| Fact | Comes from | Why |
|---|---|---|
| Which files changed, line counts, rename detection | **Host** (`changed_files`) | The local tree diff is computed with `--no-renames`, so it reports renames as add+delete and cannot supply `previous_path` |
| File content on each side | **Git**, locally | Needed as real buffers for Zed's split diff viewer |
| Base and head revision identifiers | **Host** (`detail`) | Only the host knows what the pull request proposes |
| The merge base itself | **Git**, locally | `git diff-tree --merge-base` computes it |

The pull request implementation therefore straddles both: it takes revisions and the file list from the
host boundary and content from git. That is deliberate, and it is why `changeset_pull_request.rs` is the
only module permitted to depend on both boundaries.

## Shipped implementations

| Implementation | Status |
|---|---|
| `changeset_pull_request.rs` — the change a pull request proposes | This phase (FR-059: the only one) |
| Branch comparison | Named future source (FR-058). Not implemented. |
| Commit range, working tree | Named as later possibilities. Not implemented. |
</content>
