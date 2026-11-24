# Design and state model

`trk` is a local issue tracker and agent launcher in one Rust binary. Each project
owns a SQLite database in `.trk/`; there is no global issue registry. Humans use
the CLI and dashboards. Sessions launched for an issue use its per-issue MCP
server, so their tracker context and tools stay focused on that issue.

An **agent** is instructions plus tools; a **harness** runs the agent; a
**launcher** provides its terminal/session. Types route issues to agents, choose
whether a run uses an issue worktree, and may supply optional task-specific guidance.
They do not introduce different state machines or custom issue fields.

## Goals and boundaries

- Keep the model small: four states, one blocker relationship, attributed claims.
- Keep project state local and configuration portable, without registering projects.
- Preserve the history of decisions and handoffs as append-only events.
- Treat completion as landed work: awaiting merge, review or a decision is waiting,
  not done. Waiting still blocks dependent work.
- Return failed checks as actionable work, with evidence, rather than silently retrying
  errors forever.
- Share domain rules across CLI, MCP and the check runner through `trk-core`.

There are no configurable workflows, issue hierarchies, network synchronization,
networked multi-user service or web issue editor. Types do not require evidence on
close: completion requirements belong in instructions and checks, not a built-in
review/CI model. External status is read by checks when needed, not mirrored into
issue fields. Checks and agent identities are not security sandboxes.

Type guidance is optional inline MiniJinja text in `types/<name>.toml`, rendered
with the agent instruction context and appended under `## Type guidance`. It can
include shared prompt templates rather than duplicating agent definitions. An
explicit `--agent` override changes the selected agent but retains the issue type's
guidance. Absent guidance leaves existing prompt output unchanged.

Types resolve project-over-user as whole files, not merged fields; included templates
use that same project-first search order. Guidance provenance identifies the resolved
type file as `types/<name>.toml#guidance` and records included templates. Agent check
validates every routed type with a sample issue in addition to its representative
agent context; preview and run dry-run expose the rendered result and sources.
See [type guidance](configuration.md#type-guidance) for configuration examples.

## Issues, readiness and blockers

An issue has a required title, Markdown description, priority (`P0` urgent through
`P3` low; default `P2`), and required routing type (configured default, otherwise
`code-change`). Notes are history events, not a mutable description extension.
The system owns state, close reason, waiting metadata, authorship and timestamps.

IDs have the form `<prefix>-<random lowercase base32 suffix>`. New suffixes start
at three characters and lengthen on collisions. The prefix is configurable;
changing it does not rename existing issues. Unique ID/suffix prefixes may be
used in the CLI; ambiguous matches fail rather than selecting an issue arbitrarily.

Links are issue-owned HTTP(S) pointers with optional single-line labels of at most
40 characters. Their URL is the key: adding it again updates its label. Links
survive claims, waiting and reopening. `trk` stores and displays them but does not
fetch their targets or interpret pull requests.

**A blocks B** is the only dependency: B is ready only when A is closed. The graph
must be acyclic, including no self-edges. Readiness means **open and every blocker
closed**; both `done` and `abandoned` satisfy that condition. Priority ranks eligible
work and cannot bypass blockers. The default ready/list order is priority, creation
time, then ID; explicit or saved-view sorting may override it.

Reviews, decisions and milestones are ordinary issues with descriptions and
blocker edges. A follow-up records its originating issue in the create event;
it is not a parent/child hierarchy or an implicit blocker.

## States and transitions

| State | Meaning |
| --- | --- |
| `open` | Unclaimed; eligible for claiming only when ready |
| `claimed` | Exactly one active claim |
| `waiting` | No active claim; waiting on a check or a person |
| `closed` | Finished with reason `done` or `abandoned` and a closing timestamp |

| From | To | Cause |
| --- | --- | --- |
| `open` | `claimed` | Claim a ready issue |
| `claimed` | `open` | Unclaim, agent hand-back, lease expiry, or reap of a confirmed-gone local session |
| `claimed` | `waiting` | Claimant waits with a reason, check, or both |
| `claimed` | `closed` | Claimant closes as `done` or `abandoned` |
| `open` or `waiting` | `closed` | Human closes without an active claim |
| `waiting` | `waiting` | Check returns 75, or a human replaces its waiting settings |
| `waiting` | `claimed` | Failed check restores its eligible live session, a human authorizes CLI resume, or the same waiting agent reports an actual human chat reply through MCP resume for an enabled no-check wait |
| `waiting` | `open` | Failed check cannot wake its session, or restored-claim delivery fails |
| `waiting` or `closed` | `open` | Human reopens |

A human can force another actor's claimed issue through claimant-restricted
operations; the override is recorded. Stealing a claim replaces its owner without
changing `claimed` state. Reopening clears closure/waiting metadata; closing clears
waiting metadata. Closing an issue does not delete it.

### Waiting is not ownership

Entering waiting releases the claim with reason `waiting`. A check is initially
due immediately and subsequently scheduled at its interval. No check means the
issue waits for a human rather than polling. The exit-code contract is deliberately
small:

- **0:** close as `done`.
- **75:** remain waiting and schedule another check; no history event or
  `updated_at` change is generated.
- **Anything else, including timeout:** record the failure and output tail, then
  restore the original eligible session or reopen the issue.

Automatic restoration requires a confirmed-live terminal on the known local claim
host, the original pinned launcher's send operation, and remaining automatic wake
budget. Terminal liveness is separate from MCP liveness: a dormant MCP connection
does not make a terminal dead, but must reconnect with the same identity before
using tools. Unknown or remote liveness does not authorize restoration.

A restored claim has a fresh lease and preserves actor, worktree, host, agent,
terminal and launcher. Automatic restoration increments persistent `wake_count`,
even if message delivery fails. Waiting preserves the count; a fresh explicit
claim, including stealing, resets it; an idempotent claim by its owner does not.
Human-authorized CLI resume and agent-reported chat resume neither increment nor reset it. Failed delivery releases
only the exact restored claim, never a replacement claim or subsequent session
state. Successful delivery is not proof that an agent read or executed a request.

See [Waiting and checks](waiting-and-checks.md) for scheduling, wake limits,
human CLI resume, same-session chat resume, failure recovery and landing recipes.

## Actors, claims and permissions

Actors are `human:<email>` or `agent:<harness>:<session-id>`. The agent session ID
is everything after the second colon, kept verbatim; `trk run` mints a UUID before
launch. An agent may record the human it acts on behalf of. CLI actor selection
uses `--as`, then `TRK_ACTOR`, then configured human identity.

Claim creation and the transition to `claimed` are atomic. A partial unique index
permits at most one active claim per issue, and core maintains the stronger
invariant: **an issue is claimed if and only if it has an active claim**. Claims
retain their actor, timestamps, optional lease, worktree/host and session routing
metadata after release. Release reasons are `closed`, `waiting`, `unclaimed`,
`expired` and `stolen`.

Agent claims have a default 30-minute lease; human claims have none by default.
Explicit renewal, the running MCP server and claimant writes renew leases.
Expired leases are swept at the start of write transactions, releasing their
claims and reopening issues without a lease daemon. A live terminal does not
prevent lease expiry when MCP renewal stops.

Core enforces agent authorization for the CLI as well as MCP:

- Ordinary issue edits and lifecycle actions require the agent's active claim.
- A **waiting session** is the latest claim's actor only while the issue remains
  waiting and that claim was released as `waiting`. It has no lease or ownership.
- Its remaining access is notes, links and follow-up creation, plus receiving
  messages. It cannot close, wait again or hand back without a restored claim.
- Closing, reopening or replacement ownership ends the old waiting session's access.
- Follow-ups may be created only from the agent's active or waiting session.
  Dependency edits require an active claim on at least one endpoint.
- Only a human may authorize CLI resume. For a no-check wait with `[wait] chat_resume`
  enabled (default), the same waiting agent may instead report an actual unmarked
  human chat reply, verbatim, through MCP `resume`. That report restores its claim;
  it is attributed to the agent, not recorded as human authorization.
- Chat resume transactionally requires the issue still waiting, no active claim,
  no check, and the latest waiting claim belonging to the caller. It restores the
  original metadata with a fresh lease, without terminal probing or delivery.
- Checked waits and disabled chat resume refuse this path with `not_permitted`
  and the human `trk resume` hint; chat resume cannot bypass `[check] max_wakes`.
  Agents must not impersonate a human or treat tracker deliveries as chat replies.

Actor identifiers are attribution and application policy, not authenticated
operating-system identities. A harness that can reach the database or select a
human identity is not hard-isolated by these rules. Hard isolation requires an
execution environment where the database is inaccessible. See
[Agent sessions](sessions.md) for the MCP interface and session lifecycle.

## History

Domain changes and notes append attributed, UTC-timestamped `event` rows. Notes
use `op = 'note'`; correcting one appends a new note with `amends` pointing to the
previous note. SQLite triggers reject event updates/deletes and issue deletion.
This is an application-level append-only record, not cryptographic tamper evidence
against someone controlling the database file.

Lifecycle events include creation, edits, claims/releases, waiting, check outcomes,
closure, reopening and dependency changes. Link changes use `set` events. Passing
or failing checks record an outcome and the transition they cause; exit 75 only
updates scheduling metadata to avoid flooding history. Failed checks retain the
command and the last 4 KiB of output in a note for the next work cycle.

Chat continuation records a distinct `resume_chat` event with
`{message, resumed_from, claim}` and an agent-attributed note beginning
`Session reported a human reply in chat:`. The required message must be nonempty
after trimming but is stored verbatim. Human CLI resume retains separate human
authorization and delivery records, distinguishable in `trk show` and the dashboard.

## Architecture and storage

`trk-core` owns domain rules and the `Store` write path. `trk-cli` implements human
interfaces, project resolution, launch orchestration and the check runner;
`trk-agent` defines and executes agent tools and renders launch instructions;
`trk-mcp` exposes a per-issue server and renews its claim. See
[Development](development.md) for the crate layout and contributor commands.

Project `.trk/` contains `trk.db`, configuration/resources, worktrees, session
scratch files and check logs. Optional user configuration has no issue database or
session state. Settings merge **built-in defaults < user < project**; agent, type
and prompt files resolve project-first then user by filename without merging
contents. See [Configuration](configuration.md) for paths and precedence details.

Project discovery uses explicit `--dir`/`TRK_DIR` first. Otherwise it searches
ancestors for `.trk/trk.db` within the current checkout. In a linked worktree it
also searches the corresponding path and ancestors in the primary checkout, so
nested projects resolve correctly. The fallback is the primary Git checkout root,
or the current directory outside Git. Normal commands require an installed database
and configuration; they never implicitly create a new project database.
Launched MCP sessions and checks receive `TRK_DIR` to preserve project identity.

### Transactions and check ownership

- Connections use WAL, `synchronous=NORMAL`, foreign keys and a five-second busy
  timeout. Reads use deferred transactions.
- A core write runs in one `BEGIN IMMEDIATE` transaction: register the actor,
  sweep expired leases, validate permissions/invariants, mutate rows and append
  events, then commit. Failure rolls it back; core dry-run writes also roll back.
- Checks execute outside database transactions. Queue locking, durable process-group
  registration and result recording are separate transactions. A spawned shell
  cannot execute its check until its group has been durably registered.
- The queue serializes checks, including merges. Shell exit, timeout and runner
  interruption stop remaining group members and reap the shell before releasing
  ownership. Cleanup failure retains the lock; interruption leaves the issue
  waiting without recording a partial check outcome.
- Crash recovery takes over only when the same-host runner and its live group
  members are gone. Unknown ownership fails closed. Checks must not detach from
  their process group.
- Results validate the waiting command and lifecycle revision before changing an
  issue. Stale results cannot overwrite a newer wait or ownership change.
- Commands can run again after a crash between execution and result recording:
  checks must be non-interactive and repeat-safe, not assume exactly-once execution.

Migrations apply on open using SQLite `user_version`; a database newer than the
executable is refused, not downgraded or reset. Running MCP servers require an
explicit compatible restart after an upgrade. Backups use `VACUUM INTO` for a
consistent copy.

### Data model

The baseline [schema](https://github.com/rodrigosetti/trk/blob/main/crates/trk-core/src/schema.sql)
and subsequent migrations define the current database:

| Table | Purpose |
| --- | --- |
| `harness`, `actor` | Actor identities, delegation and session metadata |
| `issue` | Authored fields, lifecycle/waiting metadata, timestamps and wake count |
| `dep` | Directed blocker edges (`src` blocks `dst`) |
| `link` | Current issue URLs and labels; changes remain in events |
| `claim` | All claims, including released claims and pinned launcher identity |
| `event` | Append-only attributed history with JSON payloads |
| `inbox` | Delivery bookkeeping for notes addressed to a session |
| `check_lock` | Serialized queue owner and active process group |
| `search` | FTS5 index of titles, descriptions and notes |

Views expose `note`, `active_claim`, `issue_view` (with claimant/lease) and `ready`.
SQLite enforces foreign keys, valid states/priorities/reasons, closure consistency,
one active claim, valid event JSON and append-only history. Core additionally
enforces transitions, claim/state equivalence, authorization and acyclic blockers.
Not every invariant is a database constraint: direct SQL writes are not a supported
alternative to core operations.

## Trust boundary

Checks are shell commands, potentially written by an agent and executed later,
unattended, with the invoking user's privileges. They run outside any sandbox the
harness may have used. Process-group ownership prevents overlapping managed
checks; it does not sandbox commands or protect files and credentials. `trk check`
runs only when started by the user; inspect the full command in `trk show` or the
queue with `trk check --dry-run` before execution.

Project and user configuration are trusted executable inputs, including launcher
commands and templates; there is no separate trust gate between those layers.
External-service credentials, permissions, query failures and status interpretation
belong to the check command. `trk` interprets its exit code, not PR, CI or review
semantics.
