# Feature Specification: Pull Request Review

**Feature Branch**: `amal/pull-requests`

**Created**: 2026-09-06

**Status**: Draft

**Input**: User description: "Refer to how pull request reviews is implemented in /Users/aabeygunawardana/work/ai/reviewer-intellij-plugin. That plugin implements a concept of \"review sessions\". We don't need review sessions. We only need pull requests list with same filters and look and feel. Then we need to be able to see the pull request overview tab and files tab. Files tab should show the list of files and allows us to add comments to the pull request in the split diff viewer (zed native).
- Use twg tool to interact with bitbucket pull requests using structured json output. However, the component that handles pull requests interactions should be put behind a trait so that it can be extended to use other mechanisms and other platforms like github in the future
- Limit the phase to reviewing pull request diff. However, the solution should be extensible to introduce other types of changesets such as branch comparison in the future"

## Overview

A reviewer spends much of their week reading somebody else's pull request. Today that means leaving
Zed for a browser: the pull request's list, its description, its file list and its diff all live
outside the editor, and so does every comment the reviewer writes. The diff they read in the browser
is not the diff Zed can render, and the comment they write there is divorced from the code they were
reading when they thought of it.

This feature brings pull request review into Zed. A **Pull Requests** panel lists the pull requests
of the repository the project is open on — status, title, author, who has approved, and how long each
has been open — filterable by state and by author, sorted by recency. Selecting one opens it in the
panel's detail section, which has two tabs: **Overview**, showing what the host knows about the pull
request, and **Files**, listing the files the pull request changes with their line counts.

Opening a file from the Files tab opens Zed's **own split diff viewer** on that file's change. This is
not a rendering of a patch in a custom surface: it is the editor, with the reviewer's theme, keymap,
font and editor settings, syntax highlighting, go-to-definition and search. From there the reviewer
selects lines and writes a comment, and the comment is posted to the pull request as an inline comment
at that file and those lines.

Two things are deliberately kept narrow, and two boundaries are deliberately drawn.

**Narrow**: there is no review session and nothing persisted about a review. Browsing the list, reading
an overview and reading a diff are all free and leave nothing on disk. And the only thing a reviewer
can review in this phase is the change a pull request proposes — not a branch pair, not a commit range,
not the working tree.

**Bounded**: the changeset under review arrives through **one** boundary, so that branch comparison and
other changeset kinds can be added later without touching the panel, the file list, the diff surface or
commenting. And everything that talks to the pull request host arrives through **one** boundary, whose
only shipped implementation reads Bitbucket through the `twg` command-line tool, so that a different
mechanism or a different platform — GitHub — can be added without touching anything above it.

The reviewer never leaves their branch, never checks anything out, and never opens a browser to read a
pull request.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - See the repository's pull requests (Priority: P1)

The reviewer opens the Pull Requests panel. It lists the pull requests on the repository the project is
open on — by default only those that are open or still in draft, most recently active first. Each row
shows a status indicator, the title, the author, bubbles for the people who have approved it, and how
long ago it was opened.

**Why this priority**: This is the entry point for everything else, and it is useful on its own — a
reviewer who only ever reads this list already gets "what is waiting for me" without leaving Zed.

**Independent Test**: On a project whose repository has several pull requests in different states, open
the panel and confirm that open and draft pull requests are listed newest-first, and that each row shows
a status indicator, title, author, approver bubbles and a relative age.

**Acceptance Scenarios**:

1. **Given** a project on a repository the host knows, **When** the reviewer opens the Pull Requests
   panel, **Then** the panel appears and begins listing that repository's pull requests.
2. **Given** the repository has pull requests that are open, draft, merged and declined, **When** the
   list loads with default settings, **Then** only the open and draft ones are listed.
3. **Given** a listed pull request, **When** the reviewer reads its row, **Then** the row shows a status
   indicator distinguishing at least open, draft, merged and declined; the title; the author; and how
   long ago the pull request was opened, expressed relative to now.
4. **Given** a pull request that two people have approved and one has requested changes, **When** the
   reviewer reads its row, **Then** approvers appear as bubbles identifying each person, and the person
   who requested changes is shown distinctly from the approvers.
5. **Given** a pull request nobody has reviewed yet, **When** the reviewer reads its row, **Then** the
   approval area is empty rather than showing a placeholder that could be mistaken for a person.
6. **Given** the list is loading, **When** the reviewer looks at the panel, **Then** it shows that it is
   loading, and the editor and every other part of Zed remain fully usable.
7. **Given** no pull request host is available for this project, **When** the panel is opened, **Then**
   it states why in its own surface and offers a route to fix it, and nothing else in Zed is affected.
8. **Given** the host cannot be reached or refuses the reviewer's credential, **When** the list fails to
   load, **Then** the panel reports which of those it is, offers a retry, and blocks nothing else.
9. **Given** the reviewer wants the panel from the keyboard, **When** they open the command palette,
   **Then** opening and focusing the Pull Requests panel is discoverable there and bindable in the keymap.

---

### User Story 2 - Read a pull request's overview and what it changes (Priority: P1)

The reviewer selects a pull request. The panel's detail section opens on it with two tabs. **Overview**
shows the status, title, description, source and destination branches, author, and each reviewer's
approval state, with a link that opens the pull request in a browser. **Files** lists every file the
pull request changes, with its change kind — added, modified, deleted, renamed — and how many lines it
adds and removes.

**Why this priority**: Reading the description and seeing the shape of the change is what a reviewer does
before reading a single line of diff. Together with Story 1 this is a complete read-only review surface.

**Independent Test**: Select a pull request with a multi-paragraph description and a dozen changed files
including an add, a delete and a rename; confirm the Overview tab renders the description and every
approval state, and that the Files tab lists all twelve files with the correct change kind and line counts.

**Acceptance Scenarios**:

1. **Given** the reviewer selects a pull request in the list, **When** the selection registers, **Then**
   the detail section opens on that pull request within 100ms, with Overview and Files tabs present.
2. **Given** a selected pull request, **When** the reviewer reads the Overview tab, **Then** it shows the
   status, title, description, source and destination branches, author, and each reviewer's approval
   state, plus a link that opens the pull request in a browser.
3. **Given** a pull request whose description is empty, **When** the Overview tab renders, **Then** it says
   the description is empty rather than showing a blank area indistinguishable from a failure to load.
4. **Given** a pull request whose description contains markdown, **When** the Overview tab renders it,
   **Then** it is rendered as formatted text rather than shown as raw markup.
5. **Given** a selected pull request, **When** the reviewer opens the Files tab, **Then** it lists every
   file the pull request changes with its change kind and its added and removed line counts, and a total
   for the pull request.
6. **Given** a pull request that renames a file, **When** the reviewer reads its row in the Files tab,
   **Then** both the old and the new path are identifiable.
7. **Given** the detail section is still loading a pull request's description or file list, **When** the
   reviewer looks at the tab, **Then** that tab shows it is loading while the other tab and the list stay
   usable.
8. **Given** the reviewer selects a different pull request while the first is still loading, **When** the
   new selection registers, **Then** the detail section shows the new pull request and the superseded load
   is abandoned rather than arriving later and replacing it.
9. **Given** the reviewer switches between the Overview and Files tabs, **When** they switch back, **Then**
   nothing is reloaded and the tab is as they left it.
10. **Given** a pull request whose file list cannot be obtained, **When** the Files tab is opened, **Then**
    it reports the reason with a retry, and the Overview tab remains readable.

---

### User Story 3 - Read the diff in Zed's own split diff viewer (Priority: P1)

The reviewer opens a file from the Files tab. Zed's split diff viewer opens on it, showing the file as the
destination branch has it beside the file as the pull request proposes it. It is the editor: the reviewer's
theme, font, keymap and editor settings apply; the code is syntax-highlighted; search and navigation work.
The diff shown is what the pull request proposes to add — not the commits the destination branch has
acquired since the pull request was raised.

**Why this priority**: This is the point of the feature. Stories 1 and 2 without it send the reviewer back
to a browser to read the actual code.

**Independent Test**: On a pull request whose destination branch has moved since the pull request was
raised, open a changed file and confirm the split diff shows only the pull request's own changes to that
file, with the reviewer's theme and syntax highlighting applied, and that the reviewer's current branch,
index and uncommitted changes are unchanged throughout.

**Acceptance Scenarios**:

1. **Given** a file in the Files tab, **When** the reviewer opens it, **Then** Zed's split diff viewer opens
   on that file showing the destination side and the pull request side, and the unified/split toggle behaves
   as it does for any other diff in Zed.
2. **Given** the split diff is open, **When** the reviewer looks at it, **Then** the active theme, font,
   editor settings, keymap and syntax highlighting for that language all apply, with no bespoke diff
   rendering.
3. **Given** the destination branch has advanced since the pull request was raised, **When** the reviewer
   reads a file's diff, **Then** it shows only what the pull request proposes for that file and not the
   destination branch's own subsequent changes.
4. **Given** the reviewer has uncommitted changes and is on their own branch, **When** they read a pull
   request's diff, **Then** their branch, index and working tree are unchanged, and no checkout, stash or
   branch switch occurs.
5. **Given** a pull request adds a file, **When** the reviewer opens it, **Then** the diff shows it as
   wholly added rather than reporting a missing file on the destination side; and for a deleted file, wholly
   removed.
6. **Given** a binary or unusually large file, **When** the reviewer opens it, **Then** the viewer says that
   the file's diff is not shown and why, rather than attempting to render it or hanging.
7. **Given** the reviewer moves between files in the Files tab, **When** each opens, **Then** the diff for the
   previous file is not left behind as an accumulating pile of tabs the reviewer has to close.
8. **Given** the data needed to show a file's diff is not available locally, **When** the reviewer opens the
   file, **Then** the wait is visibly acknowledged within 100ms, progress is shown, and the operation can be
   cancelled.
9. **Given** the diff cannot be produced for a file, **When** the reviewer opens it, **Then** the reason is
   reported in the editor's own surface without stealing focus or opening a modal, and the panel stays usable.
10. **Given** a diff is open, **When** the reviewer uses go-to-definition, search, or any other editor
    command, **Then** it behaves as it does in any Zed editor.

---

### User Story 4 - Comment on the pull request from the diff (Priority: P2)

Reading the diff, the reviewer sees something wrong. They select the lines it is wrong on, invoke the comment
action, and type. When they submit, the comment is posted to the pull request as an inline comment on that
file at those lines, and appears where they wrote it. This is the only thing in the feature that sends
repository content anywhere, and it happens because the reviewer pressed the button on that one comment.

**Why this priority**: This is what turns reading into reviewing. It is P2 rather than P1 because Stories 1–3
already replace the browser for reading, which is most of the reviewer's time.

**Independent Test**: On an open pull request, select three lines in a file's split diff, write a comment,
submit it, and confirm it appears on the pull request at that file and that line range, attributed to the
reviewer — verified against the pull request itself.

**Acceptance Scenarios**:

1. **Given** the reviewer has lines selected in a pull request's split diff, **When** they invoke the comment
   action, **Then** an inline editor opens at those lines for them to type in, without stealing focus from
   their typing or opening a modal.
2. **Given** the reviewer has typed a comment, **When** they submit it, **Then** the submission is visibly
   acknowledged within 100ms, and the comment is posted to the pull request at that file and line range.
3. **Given** a comment was posted, **When** the reviewer looks at the diff, **Then** the comment is shown at
   the lines it was written on, attributed to the reviewer.
4. **Given** the reviewer wants to comment on one line rather than several, **When** they invoke the action
   with the cursor on a line and no selection, **Then** the comment applies to that line.
5. **Given** the reviewer has typed a comment, **When** they cancel instead of submitting, **Then** nothing is
   posted and nothing is sent anywhere.
6. **Given** a comment fails to post — the host is unreachable, the credential was refused, the pull request was
   merged in the meantime — **When** the failure returns, **Then** the reason is stated, the reviewer's text is
   not lost, and they can retry or cancel.
7. **Given** the reviewer comments on the removed side of a diff, **When** they submit, **Then** the comment is
   posted against the correct side of the change rather than silently against the other one.
8. **Given** the pull request is merged or declined, **When** the reviewer invokes the comment action, **Then**
   they are told that commenting is unavailable and why, before typing rather than after submitting.
9. **Given** the reviewer invokes the comment action from the keyboard, **When** they look for its binding,
   **Then** it is a declared action, discoverable in the command palette and bindable in the keymap.
10. **Given** a comment is being posted, **When** the reviewer keeps editing elsewhere in Zed, **Then** the
    editor stays responsive and nothing waits on the network.

---

### User Story 5 - Narrow and reorder the list (Priority: P2)

The default list is not always the list the reviewer wants. They switch the state filter from open-and-draft to
all pull requests to find one that was merged last week. They filter by author to see only the pull requests a
particular person raised — including themselves, without typing their own account name. They reverse the sort so
the pull request that has been waiting longest is on top. The choices stick, so the panel opens the way they left
it.

**Why this priority**: On any real repository the default list is too long to be useful for a specific task. P2
because the default view already delivers Story 1's value.

**Independent Test**: With a repository whose pull requests span more than one author and more than one state,
switch the state filter to all, filter by a single author, reverse the sort, and confirm the listed set and order
match each choice; then reopen the project and confirm the choices were remembered.

**Acceptance Scenarios**:

1. **Given** the default filter, **When** the reviewer switches the state filter to all, **Then** merged and
   declined pull requests appear alongside open and draft ones.
2. **Given** any state filter, **When** the reviewer filters by an author, **Then** only pull requests raised by
   that person are listed, and the filter can be cleared to see every pull request again.
3. **Given** the reviewer wants their own pull requests, **When** they filter by author, **Then** they can select
   themselves without typing their own account name.
4. **Given** an author filter that matches nothing, **When** the list renders, **Then** it states that no pull
   request matches and offers to clear the filters, rather than showing an empty list with no explanation.
5. **Given** the default sort, **When** the reviewer reverses the order, **Then** the least recently active pull
   request is on top and the indicator of the current sort reflects that.
6. **Given** filters and a sort are applied, **When** the reviewer looks at the panel, **Then** which filters are
   in effect is visible, so they can always tell why a pull request they expected is not listed.
7. **Given** filters and a sort are applied, **When** the reviewer closes and reopens the project, **Then** the
   list opens with the same filters and sort.
8. **Given** a filter or sort is changed, **When** the list updates, **Then** the editor is not blocked, and a
   pull request already selected stays selected if it is still in the list.
9. **Given** the repository has more pull requests than are loaded at once, **When** the reviewer applies a
   filter, **Then** the filter applies to the repository's whole set rather than only to the portion already
   loaded.
10. **Given** the reviewer refreshes the list, **When** it reloads, **Then** their selection, filters and sort are
    preserved.

---

### User Story 6 - See the comments already on the pull request (Priority: P3)

The reviewer is rarely the first person to read a pull request. Comments already on it appear at their lines in
the split diff, attributed to the people who wrote them, so the reviewer can see what has already been said
before saying it again — and reply to a thread rather than starting a new one beside it.

**Why this priority**: Without it the reviewer duplicates points already made and cannot answer a question asked
of them. P3 because Stories 1–4 stand up as a review surface without it.

**Independent Test**: On a pull request with three inline comment threads on two files, open both files and
confirm each thread appears at its line attributed to its author with its replies in order, then reply to one and
confirm the reply lands on that thread on the pull request rather than as a new comment.

**Acceptance Scenarios**:

1. **Given** a pull request with inline comments, **When** the reviewer opens a file those comments are on,
   **Then** each thread appears at its line, attributed to its author, with its replies in order.
2. **Given** a thread, **When** the reviewer replies to it, **Then** the reply is posted to that thread on the
   pull request rather than as a new top-level comment.
3. **Given** a pull request with comments that are not attached to any line, **When** the reviewer reads the
   Overview tab, **Then** those comments are visible there rather than silently dropped.
4. **Given** a comment anchored to a line that the pull request has since changed, **When** the reviewer opens
   the file, **Then** the comment is shown as outdated rather than silently re-anchored to an unrelated line or
   dropped.
5. **Given** a resolved comment thread, **When** the reviewer reads the diff, **Then** it is distinguishable from
   an unresolved one and does not obscure the code by default.
6. **Given** a file with many comment threads, **When** the reviewer reads it, **Then** the threads do not push
   the code off the screen — they can be collapsed.
7. **Given** comments cannot be loaded, **When** the reviewer opens a file's diff, **Then** the diff is still
   readable, the reason comments are missing is stated, and commenting is still possible.

---

### Edge Cases

**Repository and host resolution**

- **The project has no git repository, or the repository has no remote**: the panel states that it cannot list
  pull requests for this project and why. Nothing else in Zed is affected.
- **The remote is not a platform the feature supports**: reported as unsupported, naming the remote, rather than
  as a failure to connect.
- **The project has several git repositories**: which repository's pull requests are listed is visible and
  selectable, rather than one being chosen silently.
- **The host tool is not installed or not on the path**: reported as a missing prerequisite with what to install,
  distinct from an authentication failure and from an unreachable host.
- **The reviewer is not logged in to the host tool, or their session expired**: reported as needing
  authentication, with the action that fixes it, and not retried in a loop.
- **The credential is valid but grants no access to this repository**: reported as a permission problem, distinct
  from "not authenticated" and from "unreachable".
- **The host rate-limits the request**: reported, not retried automatically, and the list already loaded stays
  readable.
- **The host returns something the feature cannot parse**: reported as an unexpected response, and neither the
  panel nor Zed crashes.
- **The project is a remote project served by the remote server**: either listing works, or the panel states
  plainly that it does not work for remote projects and why — never an empty list with no explanation.

**Listing**

- **The repository has no pull requests at all**: the panel says so plainly rather than showing an empty list or
  an error.
- **A pull request has dozens of reviewers**: the row shows a bounded number of approver bubbles with a count for
  the remainder, and the full list is available on demand.
- **A person has no display name or no avatar**: the bubble falls back to something stable and identifiable rather
  than rendering blank.
- **A pull request's author is no longer a member of the workspace**: the row still shows whatever identity the
  host reports, and the author filter still offers them.
- **A very long pull request title**: the row elides within the panel width rather than forcing horizontal
  scrolling.
- **Clock skew between the host and the machine**: a pull request whose creation time appears to be in the future
  is shown as just opened rather than as a negative age.
- **The list changes on the host while the reviewer reads it**: refreshing preserves selection, filters and sort,
  and a selected pull request that has disappeared is reported rather than silently deselected.

**Diff**

- **The pull request's source or destination ref is gone from the host**: reported when the reviewer tries to read
  the diff, naming which ref is missing.
- **The pull request's source branch lives on a fork**: either the diff works from the fork's ref, or the reviewer
  is told this case is not supported — never a silent diff of the wrong change.
- **The pull request has no changed files**: the Files tab says there is nothing to review rather than showing an
  empty list.
- **A pull request with thousands of changed files**: the Files tab remains scrollable and responsive, and does not
  attempt to prepare every file's diff up front.
- **A file whose two sides use different text encodings or line endings**: the diff is shown or refused with a
  reason, never shown as every line changed with no explanation.
- **A submodule or symlink change**: reported as the kind of change it is rather than rendered as a text diff of a
  hash.
- **The reviewer closes the project while a diff is loading**: the load is abandoned cleanly and nothing is left
  running.

**Commenting**

- **The comment body is empty or only whitespace**: submission is refused before anything is sent.
- **The comment is very long**: either it posts, or the host's limit is reported before sending rather than as a
  failure afterwards.
- **The reviewer submits two comments in quick succession**: both post and both are attributed correctly; neither
  overwrites the other.
- **The reviewer comments on a line in a file that is not part of the pull request's change**: refused with the
  reason, rather than posted somewhere the host puts it arbitrarily.
- **The reviewer selects lines spanning both sides of a diff**: either the comment is posted against a single
  well-defined side and range, or the reviewer is asked to narrow the selection — never posted against a range the
  host will place differently from what the reviewer saw.
- **The pull request is updated with new commits while the reviewer is composing**: the comment is posted against
  the revision the reviewer was reading, or the reviewer is told the pull request moved — the comment is not
  silently attached to a line it was not written about.
- **Zed is quit with an unsubmitted comment open**: nothing is posted, and the reviewer is not led to believe it
  was.

## Requirements *(mandatory)*

### Functional Requirements

#### Panel and entry points

- **FR-001**: The feature MUST present a Pull Requests panel in the Zed workspace, dockable and dismissible like
  Zed's other panels, containing a pull request list and, below it, a detail section for the selected pull request.
- **FR-002**: Opening and focusing the panel MUST be declared actions, discoverable in the command palette and
  bindable in the keymap.
- **FR-003**: The panel MUST NOT be constructed, and MUST NOT contact the pull request host, until the reviewer
  first opens it.
- **FR-004**: The feature MUST NOT introduce a concept of a persisted review session, and MUST NOT write review
  state to disk. Browsing the list, reading an overview and reading a diff MUST leave nothing on disk beyond the
  reviewer's view-state preferences (FR-021) and whatever git objects were fetched to produce a diff.
- **FR-005**: The panel MUST list the pull requests of the repository the open project maps to on the pull request
  host. When the project contains more than one git repository, which repository is listed MUST be visible to the
  reviewer and changeable by them.

#### Pull request list contents

- **FR-006**: Each listed pull request MUST show a status indicator that distinguishes at least open, draft, merged
  and declined; the title; the author; and how long ago the pull request was opened, expressed relative to now.
- **FR-007**: Each listed pull request MUST show the people who have approved it as identity bubbles, and MUST
  distinguish a reviewer who has requested changes from a reviewer who has approved.
- **FR-008**: When a pull request has more approvers than the row can show, the row MUST show a bounded number of
  bubbles plus a count of the remainder, and MUST make the full list available on demand.
- **FR-009**: A pull request with no approvals MUST show an empty approval area rather than a placeholder that
  could be read as a person.
- **FR-010**: A person shown as an identity bubble MUST be identifiable when the host supplies no display name and
  no avatar, by falling back to a stable identifier rather than rendering blank.
- **FR-011**: The list MUST be refreshable on demand, and refreshing MUST preserve the current selection, filters
  and sort.
- **FR-012**: The list MUST remain scrollable and responsive on a repository with at least 500 pull requests, and
  MUST NOT require every pull request to be loaded before the first is shown.

#### Filtering and sorting

- **FR-013**: The list MUST filter by state, defaulting to open and draft pull requests only, and MUST offer
  showing all pull requests regardless of state.
- **FR-014**: The list MUST filter by author, restricting the list to pull requests raised by the chosen person.
- **FR-015**: The reviewer MUST be able to select themselves as the author filter without typing their own account
  name.
- **FR-016**: The reviewer MUST be able to clear all filters and return to the default view in one action.
- **FR-017**: The list MUST sort by recency by default, most recent first, and the reviewer MUST be able to reverse
  that order.
- **FR-018**: The filters and sort currently in effect MUST be visible in the panel, so the reviewer can always tell
  why a pull request they expected is not listed.
- **FR-019**: Filters and sort MUST apply to the repository's whole set of pull requests on the host, not only to
  the portion already loaded.
- **FR-020**: When the filters match no pull request, the panel MUST say so and offer to clear them, rather than
  presenting an unexplained empty list.
- **FR-021**: The filters, the sort, and the selected repository MUST be remembered per project across restarts.

#### Overview tab

- **FR-022**: Selecting a pull request MUST open the detail section on it with exactly two tabs, Overview and
  Files, with Overview selected by default.
- **FR-023**: The Overview tab MUST show the pull request's status, title, description, source and destination
  branches, author, and each reviewer's approval state, plus a link that opens the pull request in a browser.
- **FR-024**: The Overview tab MUST render a markdown description as formatted text, and MUST state that a
  description is empty rather than showing a blank area indistinguishable from a failure to load.
- **FR-025**: Switching between the Overview and Files tabs MUST NOT reload either tab's content or lose its state.
- **FR-026**: When the reviewer selects a different pull request while a previous one is still loading, the detail
  section MUST show the new selection and MUST abandon the superseded load rather than allowing it to arrive later
  and replace the new one.

#### Files tab

- **FR-027**: The Files tab MUST list every file the pull request changes, showing each file's path, its change
  kind — added, modified, deleted, renamed — and the number of lines it adds and removes, plus a total for the pull
  request.
- **FR-028**: For a renamed file, the Files tab MUST make both the old and the new path identifiable.
- **FR-029**: When a pull request changes no files, the Files tab MUST say there is nothing to review rather than
  showing an empty list.
- **FR-030**: The Files tab MUST NOT prepare or fetch any file's diff content until the reviewer opens that file.
- **FR-031**: When the file list cannot be obtained, the Files tab MUST report the reason and offer a retry, and the
  Overview tab MUST remain readable.

#### Reading the diff

- **FR-032**: Opening a file from the Files tab MUST open that file's change in Zed's own split diff viewer, using
  the editor's existing diff surface and its existing unified/split toggle — not a bespoke diff rendering.
- **FR-033**: The diff MUST be presented with the reviewer's active theme, font, editor settings, keymap and syntax
  highlighting applying, and every editor command available in an ordinary Zed editor MUST work in it.
- **FR-034**: The diff shown MUST be the change the pull request proposes — its source revision measured against the
  point at which it diverged from its destination branch — and MUST exclude changes the destination branch acquired
  after the pull request was raised.
- **FR-035**: Reading a pull request MUST NOT alter the reviewer's current branch, index, working tree or stash. No
  checkout, branch switch or stash MUST occur.
- **FR-036**: A file the pull request adds MUST be shown as wholly added, and a file it deletes as wholly removed,
  rather than as a missing file on one side.
- **FR-037**: A file whose diff cannot be rendered — binary, too large, a submodule, a symlink — MUST be reported as
  such with the reason, rather than rendered incorrectly or left hanging.
- **FR-038**: When the data needed to render a file's diff is not present locally, the feature MUST obtain it,
  MUST acknowledge the wait within 100ms, MUST report progress, and MUST allow cancellation.
- **FR-039**: Opening successive files from the Files tab MUST NOT accumulate editor tabs the reviewer has to close.

#### Commenting

- **FR-040**: The reviewer MUST be able to select one or more lines in a pull request's diff and add a comment on
  them, through a declared action that is discoverable in the command palette and bindable in the keymap.
- **FR-041**: Invoking the comment action with no selection MUST comment on the line the cursor is on.
- **FR-042**: Submitting a comment MUST post it to the pull request as an inline comment at that file and that line
  range, on the side of the change the reviewer was reading.
- **FR-043**: Submission MUST be visibly acknowledged within 100ms, and a posted comment MUST then appear at the
  lines it was written on, attributed to the reviewer.
- **FR-044**: Cancelling a comment MUST send nothing.
- **FR-045**: A comment whose body is empty or only whitespace MUST be refused before anything is sent.
- **FR-046**: When posting fails, the reason MUST be stated, the reviewer's text MUST NOT be lost, and the reviewer
  MUST be able to retry or cancel.
- **FR-047**: When the pull request cannot accept comments — it is merged or declined, or the reviewer lacks
  permission — the reviewer MUST be told before composing rather than after submitting.
- **FR-048**: When a selection spans both sides of the diff, the comment MUST either be posted against a single
  well-defined side and line range, or be refused with the reviewer asked to narrow the selection.
- **FR-049**: When the pull request gains new commits while a comment is being composed, the comment MUST be posted
  against the revision the reviewer was reading, or the reviewer MUST be told the pull request moved. It MUST NOT be
  silently attached to a line it was not written about.

#### Existing comments

- **FR-050**: The comments already on the pull request MUST be shown at the lines they are anchored to in the split
  diff, attributed to their authors, with their replies in order.
- **FR-051**: The reviewer MUST be able to reply to an existing thread, and the reply MUST be posted to that thread
  rather than as a new top-level comment.
- **FR-052**: Comments not anchored to any line MUST be visible in the Overview tab rather than dropped.
- **FR-053**: A comment anchored to a line that has since changed MUST be shown as outdated rather than
  re-anchored to an unrelated line or dropped.
- **FR-054**: A resolved thread MUST be distinguishable from an unresolved one, and threads MUST be collapsible so
  they cannot push the code off the screen.
- **FR-055**: When comments cannot be loaded, the diff MUST remain readable, the reason MUST be stated, and adding
  a comment MUST still be possible.

#### Extensibility boundaries

These requirements exist so that named future capabilities can be added without changing anything above the seam
they describe. Each mandates exactly one seam with exactly one implementation in this phase.

- **FR-056**: Every interaction with a pull request host — listing pull requests, reading one pull request,
  listing its changed files, resolving its source and destination revisions, reading its comments, posting a
  comment and replying to one, and resolving who the reviewer is on the host — MUST be reached through a single
  named boundary, so that a second mechanism or a second platform (**GitHub**) can be added by supplying another
  implementation of that boundary alone.
- **FR-057**: No type, field, error or user-facing string outside the implementation of the FR-056 boundary MUST
  name a specific pull request host, platform or transport mechanism. The panel, the list, the detail tabs, the
  file list, the diff surface and commenting MUST be expressible entirely in terms of the boundary's own vocabulary.
- **FR-058**: The change under review MUST be supplied to the file list and the diff surface through a single named
  changeset boundary, so that a second changeset kind (**branch comparison**, and later commit ranges or the working
  tree) can be added by supplying another implementation of that boundary alone.
- **FR-059**: Exactly one implementation of the FR-058 boundary MUST ship in this phase: the change a pull request
  proposes. No other changeset kind is part of this phase.
- **FR-060**: Neither boundary MUST introduce a registry, dynamic discovery, plugin mechanism or extension point.
  Each is a plain interface with its implementations selected where the caller is constructed.
- **FR-061**: The two boundaries MUST be independent: the changeset boundary MUST NOT require its changeset to come
  from a pull request host, and the host boundary MUST NOT require its pull requests to be rendered as a changeset.

#### Host interaction

- **FR-062**: The shipped implementation of the FR-056 boundary MUST obtain its data as structured, machine-readable
  output rather than by parsing output intended for humans.
- **FR-063**: The feature MUST NOT store, prompt for, or hold pull request host credentials of its own. Authentication
  MUST be whatever the shipped implementation's underlying mechanism already established. Should any future
  implementation require credentials of its own, they MUST be stored through Zed's credential provider and the OS
  keychain, never in settings files, project files or logs.
- **FR-064**: A missing prerequisite, an unauthenticated session, an expired credential, a permission denial, an
  unreachable host, a rate limit, and an unparseable response MUST be reported as distinct conditions, so the
  reviewer knows which one to fix.
- **FR-065**: An unparseable or unexpected host response MUST be handled without panicking and without taking down
  the panel; the feature MUST report it and remain usable.
- **FR-066**: User-configurable behaviour MUST be exposed through Zed's settings schema with defaults in the shipped
  default settings, never through a configuration file the feature reads itself.

#### Responsiveness, failure and privacy

- **FR-067**: Listing pull requests, reading a pull request, listing changed files, fetching revisions, computing
  diffs, reading comments and posting comments MUST all run off Zed's foreground thread. No foreground action,
  render pass or entity update introduced by this feature MUST exceed one frame budget.
- **FR-068**: Selecting a pull request MUST open the detail section within 100ms, and opening a file's diff and
  submitting a comment MUST each be visibly acknowledged within 100ms, even while the result is pending.
- **FR-069**: Any operation that can exceed one second — listing, reading a pull request, fetching revisions,
  computing a diff, posting a comment — MUST report progress and MUST be cancellable, and cancellation MUST stop
  the underlying work rather than only discarding its result.
- **FR-070**: The feature MUST contribute nothing to Zed's startup beyond registering its actions and panel, with
  no network activity and no eager initialization.
- **FR-071**: Every failure MUST be reported inside the feature's own surfaces, MUST NOT steal focus, MUST NOT open
  an unprompted modal, and MUST leave Zed fully usable.
- **FR-072**: Requests to the pull request host MUST carry only repository identity and pull request metadata.
  Repository content MUST be transmitted only as one comment the reviewer explicitly submitted, carrying that
  comment, its file path and its line range — never automatically, never batched, and never on opening the panel,
  selecting a pull request or opening a diff.
- **FR-073**: The feature MUST NOT prevent Zed from working when the pull request host is unavailable, unsupported
  for the project, or unauthenticated. Every part of Zed unrelated to this feature MUST behave identically.

### Key Entities

- **Pull Request Summary**: What one list row shows — the pull request's identity, title, author, state including
  draft, when it was opened, when it was last active, each reviewer's approval state, and its web link. Read from
  the host; not persisted.
- **Pull Request Detail**: What the Overview tab shows — the summary, plus the description, the source and
  destination branches, and the source and destination revisions the diff is measured between.
- **Changed File Entry**: One row in the Files tab — the file's path, its previous path when renamed, its change
  kind, and its added and removed line counts.
- **Changeset**: The change under review, supplied through the FR-058 boundary — a set of changed file entries and,
  per file, the two sides the diff is between. In this phase its only implementation is the change a pull request
  proposes.
- **Comment Thread**: One conversation on the pull request — its anchor (file, line range and side, or none), its
  author, its body, its replies, whether it is resolved, and whether its anchor is outdated.
- **Draft Comment**: A comment the reviewer is composing — its anchor, its body, and the revision it was written
  against. Exists only until it is submitted or cancelled; never persisted.
- **Repository Coordinates**: How the open project's git repository identifies itself to the host — derived from the
  repository's remote. Repository identity only; no content.
- **Reviewer Identity**: Who the reviewer is on the host, so "my pull requests" can be offered without the reviewer
  typing their own account name.
- **List View State**: The reviewer's per-project choices — state filter, author filter, sort direction and selected
  repository — remembered across restarts.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A reviewer can go from opening Zed to reading a colleague's pull request diff and leaving a comment on
  it without opening a browser, switching branches, or checking anything out.
- **SC-002**: The reviewer's current branch, index, working tree and stash are byte-for-byte unchanged by opening the
  panel, selecting twenty pull requests in turn, and reading a diff from each.
- **SC-003**: Selecting a pull request opens the detail section within 100ms; opening a file's diff and submitting a
  comment are each acknowledged within 100ms; and the editor shows no perceptible stutter while any of the feature's
  work is in flight.
- **SC-004**: On a repository with 500 pull requests, the first rows are readable within 2 seconds and scrolling the
  full list stays smooth.
- **SC-005**: Zed's startup time and time to open a workspace are unchanged, within measurement noise, by the presence
  of this feature when the panel has never been opened.
- **SC-006**: A missing prerequisite, an unauthenticated session, an expired credential, a permission denial, an
  unreachable host, a rate limit and an unparseable response each produce a different, actionable message — verified
  by inducing all seven.
- **SC-007**: Every filter and sort combination lists exactly the pull requests that match, drawn from the
  repository's whole set rather than a partially loaded page, and which filters are in effect is always visible.
- **SC-008**: Filters, sort and selected repository survive a restart, verified by setting each and reopening the
  project.
- **SC-009**: For a pull request whose destination branch has advanced by at least ten commits since it was raised,
  the set of files listed and the lines shown as changed match the change the pull request proposes and contain none
  of the destination branch's subsequent changes.
- **SC-010**: Every diff the feature shows is the editor's own diff surface: the reviewer's theme, font, editor
  settings and keymap apply, syntax highlighting is present, and the unified/split toggle works — verified against a
  diff opened by Zed's existing git surfaces.
- **SC-011**: A comment written on a selected line range appears on the pull request at that file and range,
  attributed to the reviewer — verified against the pull request itself, for a single line, a multi-line range, the
  added side and the removed side.
- **SC-012**: No comment is ever posted that the reviewer did not submit, and no submitted comment is ever silently
  lost: verified by cancelling, by inducing a post failure and retrying, and by quitting with a comment open.
- **SC-013**: Adding a second implementation of the FR-056 host boundary requires no change to the panel, the list,
  the detail tabs, the file list, the diff surface or commenting — demonstrated by a check that no host, platform or
  transport name appears outside that boundary's implementation.
- **SC-014**: Adding branch comparison as a second changeset kind requires no change to the file list, the diff
  surface or commenting — demonstrated by those surfaces depending only on the FR-058 boundary.
- **SC-015**: Browsing is free: after opening the panel, applying every filter, and selecting twenty pull requests in
  turn, no review state has been written to disk and no file content has been fetched from the host.
- **SC-016**: No code path in the feature can panic on host failure, malformed host output, a missing revision, or
  cancellation — verified by exercising each.

## Assumptions

- **Pull request host**: Bitbucket, reached through the `twg` command-line tool using its structured JSON output.
  This is the one implementation of the FR-056 boundary that ships in this phase. Where this specification says
  "the host" it means that one path.
- **Authentication**: whatever `twg` has already established for the reviewer. The feature stores no credential and
  prompts for none; an unauthenticated or expired session is reported with the action that fixes it (FR-063, FR-064).
- **"Same filters and look and feel" as the reference plugin**: the state filter defaulting to open-and-draft with an
  all option, the author filter including "me", recency sort with a reversible direction, and rows carrying a status
  indicator, title, author, approver bubbles and relative age. Filtering by reviewer or approver, and sort fields
  other than recency, are not part of this phase.
- **"Draft"**: a distinct status for display and for the default filter — a property the host reports on an otherwise
  open pull request. The default filter is "open, including drafts", and the status indicator distinguishes the two.
- **"Filterable by user"**: the user is the pull request's **author** — the same person the row shows.
- **"Recent"**: for sorting, the pull request's last activity as the host reports it; for the row's age, the time
  since the pull request was opened.
- **Panel layout**: the list occupies the panel with the selected pull request's Overview and Files tabs in a detail
  section below it, mirroring the reference plugin. Diffs open in the center pane as editors, because that is where
  Zed's split diff viewer lives and the point of FR-032 is to use it rather than reproduce it.
- **How the diff is obtained**: from the local git repository, by fetching the revisions the pull request names and
  diffing them against their divergence point. This is what lets the change appear in Zed's own split diff viewer as
  real buffers with syntax highlighting, navigation and search, rather than as a rendered patch. Fetching adds git
  objects to the repository; it creates no branch, no worktree and no checkout, and it does not modify the working
  tree (FR-035).
- **No review sessions**: the reference plugin's session concept, its session files, retention, worktrees and their
  cleanup are all out of scope. Nothing about a review is persisted (FR-004).
- **No agent**: this phase does not run an AI review. There are no findings, no logical grouping and no change
  diagram — only the pull request's own comments and the reviewer's. The changeset boundary (FR-058) is the seam a
  later agent-driven review would consume.
- **One repository at a time**: the pull requests of the repository the open project maps to. Not pull requests
  across a workspace, and not several repositories at once.
- **Comments post immediately**: submitting a comment posts it. There is no local draft store and no batched review
  submission in this phase; an unsubmitted comment exists only in the open inline editor.
- **Out of scope for this phase**: approving, declining, merging, creating or editing pull requests; editing,
  deleting or resolving comments after posting; pull request tasks; CI and pipeline status; branch comparison, commit
  ranges and working-tree review as changeset kinds; GitHub and any host other than the one above; AI review;
  reviewing pull requests across several repositories at once; and pull requests whose source branch lives on a fork,
  unless that falls out for free.
</content>
</invoke>
