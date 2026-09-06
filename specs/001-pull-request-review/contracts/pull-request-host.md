# Contract: `PullRequestHost`

**Discharges**: spec FR-056, FR-057, FR-060, FR-061 | **Declared in**: `crates/pull_request_review/src/host.rs`

The single boundary through which every pull request host interaction passes. FR-056 mandates it by
number so that a second mechanism or a second platform — **GitHub** — can be added by supplying another
implementation of this trait alone.

This citation must appear as a doc comment on the trait, per constitution Principle I.

## Shape

```
trait PullRequestHost: Send + Sync {
    fn list(&self, repo, filter, sort, limit)      -> Task<Result<Vec<PullRequestSummary>, HostError>>
    fn detail(&self, id)                           -> Task<Result<PullRequestDetail, HostError>>
    fn changed_files(&self, id)                    -> Task<Result<Vec<ChangedFile>, HostError>>
    fn comments(&self, id)                         -> Task<Result<Vec<CommentThread>, HostError>>
    fn post_comment(&self, id, draft)              -> Task<Result<CommentThread, HostError>>
    fn viewer(&self)                               -> Task<Result<Identity, HostError>>
}
```

Types are defined in [../data-model.md](../data-model.md). Every method is asynchronous and returns
`Task`, because FR-067 forbids any of this on the foreground thread.

## Obligations on every implementation

1. **No platform vocabulary escapes.** No type, field, error or user-facing string this trait exposes may
   name a host, platform or transport (FR-057). The panel, list, tabs, file list, diff surface and
   commenting are expressible entirely in the vocabulary above. This is grep-checkable and SC-013 checks it.
2. **Errors are classified, not stringly-typed.** Every failure maps to a `HostError` variant.
   `PrerequisiteMissing`, `NotAuthenticated`, `CredentialExpired`, `PermissionDenied`, `Unreachable`,
   `RateLimited` and `UnexpectedResponse` are distinct conditions and must not be collapsed (FR-064).
3. **Filters are applied by the host.** `list` applies `filter` and `sort` to the repository's whole set,
   not to a page the caller then filters (FR-019). An implementation that cannot do this server-side must
   paginate until the filter is fully satisfied — never filter a partial page and present it as complete.
4. **Metadata only, except one comment.** Requests carry repository identity and pull request metadata.
   The **only** call permitted to carry repository content is `post_comment`, and only the one comment the
   reviewer submitted, with its path and line (FR-072).
5. **`post_comment` anchors to one line.** `DraftComment` carries a single line and a single side, not a
   range (FR-040, FR-042). An implementation must not widen, guess or approximate the anchor. Comments
   *read* back through `comments` may carry a range, because pull requests created elsewhere have them and
   FR-050 requires them displayed over the lines they cover — reading a range and creating one are separate
   capabilities.
6. **Cancellable, and cancellation propagates.** Dropping the returned `Task` must stop the underlying
   work, not merely discard its result (FR-069, constitution Principle II).
7. **Never panics.** Malformed, truncated or unexpected host output returns `UnexpectedResponse`
   (FR-065). No `unwrap()`, no `expect()`, no unchecked indexing.
8. **Partial data degrades, it does not fail.** One unparseable row must not blank the list; one
   unrecognised enum value must not fail the pull request containing it.

## What the trait deliberately does not do

- **No caching, no refresh policy, no retry.** Those are the panel's decisions. An implementation that
  cached would make FR-011's "refresh preserves selection, filters and sort" unverifiable.
- **No approval hydration strategy.** `list` returning summaries without verdicts and `detail` supplying
  them is the shape the boundary offers; the two-phase load (research.md §2) is the *caller's* strategy.
  A future implementation whose list call includes verdicts satisfies this contract unchanged.
- **No knowledge of git.** Revisions are opaque strings here. Turning them into a diff is the
  [changeset boundary's](./changeset.md) job (FR-061).

## No capability reporting

An earlier draft of this contract carried a capability-reporting mechanism, so implementations could
declare whether they supported multi-line comment anchors and thread resolution. **It has been removed.**

Both features were taken out of scope instead: comments are single-line (FR-040, FR-042) and resolving
threads is not offered (FR-054). With nothing left that varies between implementations, the mechanism
would have been an extension point with no second case — exactly what constitution Principle I refuses,
and it is not mandated by any numbered requirement the way FR-056 and FR-058 mandate the two boundaries.

So the trait is uniform: every implementation supports everything it declares, and `post_comment` takes a
single line because that is what the feature does, not because of what one host cannot do. If a later
phase wants ranges or resolution, the requirement comes first and the boundary grows a method — which is
cheaper than carrying a negotiation layer for capabilities nobody uses.

## Shipped implementations

| Implementation | Status |
|---|---|
| `host_twg.rs` — Bitbucket via the `twg` CLI | This phase |
| GitHub | Named future source (FR-056). Not implemented. |
</content>
