<!--
SYNC IMPACT REPORT
Version change: unversioned template → 1.0.0
Rationale: First ratified constitution for this repository. Initial adoption is a MAJOR-line
event under the versioning policy below, recorded as 1.0.0.

Added principles: I. Simplicity First, II. Responsiveness, III. User Experience Discipline,
IV. ACP Protocol Conformance, V. Signal Over Noise.

Added sections: Platform & Technology Constraints, Development Workflow & Quality Gates,
Governance.

Removed sections: none.

Follow-up TODOs: none outstanding.
-->

# Reviewer for Zed Constitution

Reviewer for Zed is a Zed feature that reviews code changes by delegating understanding and
analysis to an agent over the Agent Client Protocol (ACP). It ships inside the Zed workspace as
first-party crates, so every constraint here is a constraint on the editor the user actually
came to use.

## Core Principles

### I. Simplicity First (NON-NEGOTIABLE)

The feature MUST remain the smallest thing that delivers a useful review. Every new crate,
third-party dependency, abstraction layer, global, background task, cache, or setting MUST be
justified in the pull request that introduces it, naming the concrete user-visible problem it
solves. Speculative generality is rejected by default: no traits-as-extension-points, strategy
objects, or feature-internal frameworks are added until a second real caller exists. The single
exception is a boundary a ratified specification mandates **by number**: where a numbered
requirement demands that a capability arrive through one seam so that a *named* future source can
replace it, that seam MAY be introduced with one implementation — provided it is a plain trait
with no extension point, no registry and no dynamic discovery, and provided the requirement it
discharges is cited where the trait is declared. Review logic that belongs to the agent MUST NOT
be reimplemented in the editor.

Rationale: This feature shares a process, a frame budget, and a crash report with the whole
editor. Every layer added is memory, startup time, and failure surface taken directly from Zed.

### II. Responsiveness (NON-NEGOTIABLE)

The GPUI foreground thread MUST NOT be blocked by feature work. All ACP calls, git and VCS reads,
diff computation, and file I/O MUST run under `cx.background_spawn` (or an equivalent background
executor task), and all foreground work MUST complete within one frame budget. Initial budgets,
enforceable and amendable:

- Foreground thread: no single action, render pass, or entity update exceeds 8ms (one frame at
  120Hz).
- Visible acknowledgement of any user-initiated review action: within 100ms, even if the result
  is still pending.
- Contribution to Zed startup and to workspace open: under 50ms combined; no eager initialization
  at startup. Panels and heavy state are constructed on first use, not on `init`.
- Any operation that can exceed 1s MUST be cancellable and MUST report progress.

Long-running agent work MUST be represented as pending state in the UI, never as a frozen editor.
Cancellation MUST propagate to the ACP session; dropping the GPUI `Task` alone is not sufficient
when the agent is still working.

Rationale: A feature that stutters Zed is disabled regardless of how good its review is.
Perceived speed is a correctness property here, not a stretch goal.

### III. User Experience Discipline (NON-NEGOTIABLE)

The feature MUST behave like a native part of Zed. It MUST use standard Zed affordances — docked
panels, the gutter, block and inlay decorations, the existing multibuffer diff viewer, toasts and
the status bar, components from the `ui` crate — rather than bespoke UI, and MUST respect the
active theme, the user's keymap, and their editor settings. Every user-facing entry point MUST be
a declared action so it is keymap-bindable and discoverable in the command palette. Review output
MUST never steal focus, open a modal unprompted, or interrupt typing. Every finding MUST be
actionable — anchored to a specific file and line, stating what is wrong and why — and MUST be
dismissible. Failures MUST degrade gracefully: when the agent is unreachable or errors, the
feature reports it quietly in its own surface and leaves Zed fully usable.

Rationale: The user's attention is the scarcest resource in the editor. A reviewer earns
attention by being unobtrusive and specific; anything else trains the user to ignore it.

### IV. ACP Protocol Conformance

The agent boundary MUST be the Agent Client Protocol, spoken as specified. The feature MUST NOT
introduce private protocol extensions, out-of-band side channels, or assumptions about a specific
agent implementation. Unknown or unsupported protocol messages MUST be handled without crashing
the session or the editor. The pinned ACP version MUST be recorded in exactly one place and MUST
be verified by tests that exercise the wire format. Protocol handling MUST live in crates that do
not depend on GPUI rendering code, so that either side can change independently.

Rationale: Conformance is what makes the feature work with any ACP agent rather than one. Forking
the protocol quietly converts an interoperable client into a bespoke integration.

### V. Signal Over Noise

Review output MUST favor precision over volume. A finding that is wrong, duplicated, or already
obvious costs more than the finding it displaces. Changes that increase the number of findings
MUST demonstrate that the added findings are real, on actual repository changes, before merge.
Known false-positive patterns MUST be suppressed rather than documented as caveats. Silence is a
valid and expected result for a clean change.

Rationale: For a reviewer, trust is the product. Precision is recoverable from a quiet tool and
unrecoverable from a noisy one.

## Platform & Technology Constraints

- Target platform: Zed, built from this Cargo workspace. All UI is GPUI; no second UI toolkit
  and no embedded web view is introduced.
- Implementation language: Rust, edition 2024, on the toolchain pinned in `rust-toolchain.toml`.
  Raising that pin is a workspace-wide decision and MUST NOT be done as part of feature work.
- Supported targets are the ones this workspace already builds: macOS, Linux, and Windows, plus
  remote projects served by the remote server. A feature surface that cannot work in a remote
  project MUST fail explicitly with a stated reason rather than silently producing empty results.
- Panic discipline: non-test code MUST NOT use `unwrap()`, `expect()`, or indexing that can go
  out of bounds. Errors propagate with `?`; errors deliberately not propagated use `.log_err()`
  or explicit `match`. `let _ =` on a fallible operation is prohibited. A panic in this feature
  takes the user's editor down with it.
- Agent transport: ACP only, via the `agent-client-protocol` dependency pinned to an exact
  version in the workspace `Cargo.toml`, so the pinned version has a single place to change.
- New crates follow the repository conventions: an explicit `[lib] path` with a descriptive root
  file name, no `mod.rs`. Prefer extending an existing crate over adding a small new one.
- Third-party dependencies MUST be declared as workspace dependencies, MUST be kept minimal, and
  MUST NOT duplicate a capability already present in the workspace. Adding one requires a
  Principle I justification.
- User-configurable behavior MUST be exposed through Zed's settings schema with defaults in the
  shipped default settings, never through an ad-hoc config file the feature reads itself.
- The feature MUST NOT transmit repository content anywhere except to the configured ACP agent
  and, on an explicit per-item reviewer action, to a code host the reviewer has configured. Such
  egress MUST be individually initiated by the reviewer for the one item being sent — never
  automatic, never batched, never on session open — and MUST be absent entirely when no host is
  configured. Requests carrying only repository *identity* (worktree, repository slug, branch or
  commit identifiers) are metadata rather than content and are permitted for read-only context.
  Credentials and endpoints MUST be stored via the `credentials_provider` crate and the OS
  keychain, never in settings files, project files, or logs.
- The feature MUST function with no network access beyond the configured agent endpoint, and Zed
  MUST remain fully usable when the agent is not configured at all.

## Development Workflow & Quality Gates

The following gates MUST pass before merge:

1. **Principle compliance.** The pull request states which principles are engaged and how it
   satisfies them. Any added crate, dependency, abstraction, or setting carries its Principle I
   justification in the description.
2. **Threading.** No new blocking work on the GPUI foreground thread. Off-thread execution and
   cancellation paths are covered by `#[gpui::test]` tests driven with `run_until_parked()`,
   using the GPUI executor's timers rather than wall-clock sleeps. Where a path is genuinely
   untestable, it is verified manually and the verification is recorded in the pull request.
3. **Performance.** Changes touching startup, editor rendering, or the review path report
   measurements against the Principle II budgets. A budget regression blocks the merge; it does
   not become a follow-up ticket.
4. **Protocol.** Changes to ACP handling include tests exercising the wire format, including
   malformed and unknown-message cases.
5. **Signal.** Changes affecting which findings are produced include before/after results on
   real repository changes, per Principle V.
6. **Graceful failure.** Agent-unreachable, agent-error, and cancellation paths are exercised,
   and none of them can panic.
7. **Build.** `./script/clippy` and `cargo fmt --check` are clean, and the tests for every
   touched crate pass.

Tests are required for protocol handling, threading, and finding-selection logic. Test-first
ordering is recommended but not mandated; test presence at merge is mandated.

Pull requests follow the repository's PR hygiene rules, including an imperative title, no
conventional-commit prefix, and a closing `Release Notes:` section.

## Governance

This constitution supersedes conflicting practices, conventions, and prior decisions for this
feature. Where a specification, plan, or implementation conflicts with it, this document wins and
the other artifact MUST be corrected. `CLAUDE.md` and the repository's `.rules` files remain the
source of day-to-day development guidance; where they conflict with this document, this document
wins and the discrepancy MUST be raised rather than resolved silently in code.

**Amendment procedure.** Amendments are proposed as a pull request modifying this file, stating
the principle affected, the rationale, and the migration path for code that the change would put
out of compliance. An amendment MUST NOT be merged alongside the feature work that motivated it;
governance changes land separately so they are reviewed on their own terms.

**Versioning policy.** This document uses semantic versioning:

- MAJOR: a principle is removed, or redefined in a way that makes previously compliant work
  non-compliant.
- MINOR: a principle or section is added, or existing guidance is materially expanded.
- PATCH: clarifications, wording, and non-semantic refinements.

Every amendment MUST update the version line and prepend a sync impact report recording the
version change, affected sections, and any deferred TODOs.

**Compliance review.** Pull request review MUST verify the quality gates above. Deferred
compliance MUST be recorded as an explicit TODO in this file with an owner, not left implicit in
the code. Unresolved TODO markers in this document MUST be resolved or explicitly re-deferred
before the next MINOR amendment.

**Version**: 1.0.0 | **Ratified**: 2026-09-06 | **Last Amended**: 2026-09-06
