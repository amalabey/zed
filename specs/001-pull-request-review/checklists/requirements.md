# Specification Quality Checklist: Pull Request Review

**Purpose**: Validate specification completeness and quality before proceeding to planning
**Created**: 2026-09-06
**Feature**: [spec.md](../spec.md)

## Content Quality

- [ ] No implementation details (languages, frameworks, APIs) — **deliberate, do not "fix"**
- [x] Focused on user value and business needs
- [x] Written for non-technical stakeholders
- [x] All mandatory sections completed

## Requirement Completeness

- [x] No [NEEDS CLARIFICATION] markers remain
- [x] Requirements are testable and unambiguous
- [x] Success criteria are measurable
- [x] Success criteria are technology-agnostic (no implementation details)
- [x] All acceptance scenarios are defined
- [x] Edge cases are identified
- [x] Scope is clearly bounded
- [x] Dependencies and assumptions identified

## Feature Readiness

- [x] All functional requirements have clear acceptance criteria
- [x] User scenarios cover primary flows
- [x] Feature meets measurable outcomes defined in Success Criteria
- [ ] No implementation details leak into specification — **deliberate, do not "fix"**

## Notes

> The two unchecked Content Quality / Feature Readiness items above are unchecked **on purpose and
> permanently**. They are not defects and must not be resolved by deleting FR-074 – FR-082 or the
> Zed-surface requirements. The spec deliberately prescribes structure because the user's stated
> constraint — a fork that keeps pulling upstream — is a constraint on the delivered implementation, not
> on user-visible behaviour. Read the two subsections below before changing anything in response to them.

### Session 2026-09-06 clarifications

Five clarifications were accepted and integrated; see `## Clarifications` in the spec. All five concern
the same constraint: **this repository is a fork that keeps pulling upstream Zed**, so the change surface
must stay enumerable. They added FR-074 – FR-082 and SC-017 – SC-022, and amended FR-002, FR-021, FR-062
and FR-066.

The fourth checklist item under Content Quality is the one these clarifications put under most strain: a
change-surface allowlist is unambiguously an implementation concern. It is retained as a requirement
rather than deferred to the plan because it is a *constraint on the delivered product* the user stated
directly — an implementation that ignores it is unacceptable regardless of whether the feature works — and
because FR-078a makes it automatically verifiable, which is the test that separates a requirement from a
preference.

**One decision was made against the recommendation, and the risk is tracked rather than resolved.**
Question 5 asked whether to reuse Zed's commit-diff machinery or build a parallel copy in the feature's
crate. The recommendation was to build its own, keeping the volatile `git_ui` commit-diff file off the
allowlist. The user chose reuse. That is a legitimate trade — one implementation instead of two — and it
is now FR-080. The cost is that an actively-changing upstream file is permanently on the allowlist, so
FR-081 (the existing commit path must behave identically), FR-082 (narrowest possible widened surface,
plus a test that fails loudly on upstream behaviour change) and SC-021 exist specifically to bound it.
If upstream churn in that file becomes painful, revisiting FR-080 is the first thing to reconsider.

### Deviations from the default "no implementation details" posture

Three deliberate deviations, each recorded here rather than silently taken:

1. **The editor's own diff surface is a requirement, not an implementation choice.** FR-032, FR-033 and
   SC-010 name Zed's split diff viewer, theme, keymap and editor settings. The user's requirement was
   explicitly "the split diff viewer (zed native)", and the constitution's Principle III mandates
   standard Zed affordances over bespoke UI. Reproducing a diff renderer would violate the requirement,
   so naming the surface is the requirement's substance.

2. **The Extensibility Boundaries requirements (FR-056 – FR-061) are architectural by necessity.**
   Constitution Principle I rejects speculative generality *except* where a numbered requirement demands
   a capability arrive through one seam so a *named* future source can replace it. The user asked for
   exactly two such seams. Numbering them, and naming GitHub and branch comparison as the future sources,
   is what makes them constitution-compliant rather than speculative. SC-013 and SC-014 verify them.

3. **`twg`, Bitbucket and git are named in Assumptions only.** No functional requirement names a host,
   platform or transport — FR-057 forbids it above the boundary, and FR-062 states the structured-output
   requirement without naming the tool. The concrete choices live in Assumptions, where they can change
   without rewriting requirements.

Two scope calls were made rather than raised as clarifications, and are recorded in Assumptions:

- **Existing pull request comments are in scope**, as User Story 6 at P3. The user asked to *add*
  comments; reading the ones already there was not requested, but posting without seeing prior comments
  duplicates points already made and cannot answer a question asked of the reviewer. It is the lowest
  priority story so it can be dropped without affecting Stories 1–5.
- **The diff is read from the local git repository after fetching the pull request's revisions**, rather
  than rendered from a patch the host returns. Only the former yields real buffers, which FR-032's
  requirement to use Zed's split diff viewer depends on. Recorded under "How the diff is obtained".
</content>
