# trk — Local Issue Tracker Design Spec

Nov 21, 2026 · @Rodrigo Setti

## Overview

`trk` is a local issue tracker and agent launcher in one Rust binary. Each project keeps its SQLite database in `.trk/`, with optional shared user configuration under `~/.config/trk`. Humans use the CLI. Agents work through a per-issue MCP server. `trk run [ID]` starts an agent session on one issue through a named launcher profile, with focused instructions and only that issue's tools.

Issues have routing-only types and share one state machine. An agent work cycle ends with the issue **closed**, **waiting** for something outside the session, or handed back **open**. A waiting check is a shell command that `trk check` runs until it passes, fails, or says "not yet"; a failed check may wake the same session for another work cycle.

**Terms.** An *agent* is instructions plus tools. A *harness* is the program that runs an agent: Claude Code, pi, omp, Codex, OpenCode. A *launcher* is where the harness runs: Herdr, `hc`, tmux, or another configured backend. A *check* is the shell command of a waiting issue.

**Goals**

- One database per project, kept in the project's own `.trk/` directory next to its config: no global database, nothing to register.
- A model small enough to hold in the head: four states, routing-only types, one dependency kind.
- First-class claims by humans (`human:<email>`) and agent sessions (`agent:<harness>:<id>`), with leases.
- A complete, append-only history: every change and every note is an attributed, timestamped event.
- "Done" means landed. Work that is finished but not merged, reviewed or decided is waiting, not closed, and still blocks what depends on it.
- Failures return as work: a failed check wakes its live waiting session, or reopens its issue, with the evidence in a note.
- Agents reach trk only through MCP, never through CLI skills.
- `trk run` starts a focused session for one issue using named launcher profiles with command templates and optional driver defaults.

**Non-goals**

- Custom type fields, configurable workflows, required evidence on close. Types only describe and route work; a check command expresses completion requirements.
- Issue hierarchies. Grouping is done with `blocks` edges.
- Sync between machines, multiple users over a network, or a web UI.
- Storing state that can be read from elsewhere at any time (CI status, PR review state). A check reads it when it runs. A link stores only where that state lives, never what it says.
- Sandboxing checks. A check runs as the user (see Trust).

## The model

### Issue

| Field | Meaning |
| --- | --- |
| `title` | required |
| `description` | markdown: what is needed, why, how to tell it is done |
| notes | an append-only log, stored as events |
| `priority` | `P0` urgent, `P1` high, `P2` normal (default), `P3` low; orders `trk list` and `trk ready` by default |
| `type` | required routing type; defaults to `[issues] default_type` (`code-change` when omitted) |
| links | zero or more `{url, label?}` pointers to what the work produced (a pull request, a preview, a document) |

Everything else on an issue is state the tool owns: `state`, `close_reason`, the waiting fields, timestamps and `created_by`.

A link is an absolute `http`/`https` address with an optional one-line label of at most 40 characters, keyed by its URL: linking the same URL again changes the label. trk stores and shows links and never reads their targets; it does not know what a pull request is. Which links a session adds is the business of its prompts: the seed PR landing recipe asks for `PR #<n>`. Links belong to the issue, not to a claim, so they survive waking, reopening and a new session. Each change is a `set` event with `changes.link`. Issue JSON carries `links` when there are any; the dashboard snapshot always does, and both dashboards show the first link on the issue's row and open it.

### States

```
          claim                 close
  open ──────────▶ claimed ──────────────▶ closed
   ▲  ◀──────────     │                      ▲
   │    unclaim       │ wait                 │ check exits 0,
   │    lease expiry  ▼                      │ or a human closes
   │               waiting ──────────────────┘
   └──────────────────┘
     failed check with no deliverable wake,
     or a human reopens
```

| From | To | Trigger |
| --- | --- | --- |
| open | claimed | `claim`: the issue must be ready |
| claimed | open | `unclaim`, the agent's `hand_back`, lease expiry, `reap` |
| claimed | closed | `close --reason done\|abandoned` |
| claimed | waiting | `wait` with a check, a reason, or both |
| waiting | closed | the check exits 0 (reason `done`), or a human closes |
| waiting | waiting | the check exits 75: checked again one interval later |
| waiting | claimed | a failed or timed-out check wakes its live original session below `[check] max_wakes`, a human authorizes CLI `resume`, or the same waiting agent reports an actual human chat reply through MCP `resume` for a no-check wait with `[wait] chat_resume` enabled |
| waiting | open | the check fails or times out with no live original session or its wake cap reached, or a human reopens; an undelivered wake also releases the restored claim to open |
| open | closed | a human closes without claiming (cancel) |
| closed | open | a human reopens |

An issue is **ready** when it is open and every issue that blocks it is closed. `trk list` and `trk ready` order by priority, then creation time, then id unless `--sort` or a saved view's sort is specified. Priority ranks work that is already eligible; it does not bypass blockers. The default table columns are `ID PRIORITY TYPE STATE DETAIL TITLE`. Filter by type with `type=review` (also `research` or `code-change`); explicitly order by type with `--sort type`.

### Blockers

`A blocks B` is the only edge: B is not ready until A is closed. Edges are acyclic. A waiting issue is not closed, so it still blocks: B becomes ready when A's work has landed, not when A's session stopped.

Other relationships are written in descriptions and notes. A review, an approval, a decision or a milestone is an issue whose description says so:

| Need | Expressed as |
| --- | --- |
| B needs A first | `A blocks B` |
| Review before landing | a review issue (type `review`, routed to `reviewer`) that blocks the next step |
| A decision by a person | an issue left waiting with no check and the question as its reason |
| A milestone | an issue blocked by everything in it |
| Work discovered on the way | a follow-up issue; the create event records `followup_of` |

### Waiting

A waiting issue carries:

| Field | Meaning |
| --- | --- |
| `waiting_reason` | what it waits for, for a human reading the queue |
| `waiting_check` | a shell command; absent when it waits on a human |
| `check_interval` | seconds between checks; default `[check] interval` (5m) |
| `next_check_at` | when the check is due; set to "now" on entering waiting |
| `last_check_at`, `last_check_exit` | the last "not yet" |
| `wait_cwd` | where the check runs: the claim's worktree, or the primary project root when no worktree was recorded |

The check's exit code is the whole protocol:

| Exit | Meaning | Effect |
| --- | --- | --- |
| 0 | pass (e.g. PR merged) | closed with reason `done` |
| 75 (`EX_TEMPFAIL`) | not yet (e.g. CI pending, awaiting review, unresolved threads, approved but not merged, unknown mergeability) | still waiting; due again after the interval |
| anything else | error (e.g. base-branch conflict, failed head-commit CI, closed without merging, query/authentication failure) | resume the live waiting session, otherwise reopen; a note records the command, its directory and the last 4 KB of its output |
| killed by the timeout | error | the same, saying it timed out |

Error is the default. A command that is misspelled, whose credentials expired, or whose branch conflicts fails loudly and returns as work, instead of waiting forever. Only the one reserved code keeps an issue waiting, so "not yet" is always a deliberate answer.

A check may do the remaining work (merge the branch) or only look (is the PR merged?). It runs under `sh -c` in the claim's worktree, falling back to the primary project root when the claim has no worktree, with `TRK_ISSUE`, `TRK_WORKTREE`, `TRK_REPO`, `TRK_BRANCH`, `TRK_BASE` (the resolved base branch) and `TRK_VERIFY` (`[check] verify`, default empty). It must be non-interactive, non-blocking (no `--watch`) and safe to run more than once: `trk check` may die between running a command and recording its result, and the next pass runs it again.

Merge your branch locally only after verifying the merged result in the issue worktree:

```sh
cd "$TRK_WORKTREE" || exit 1
git merge --no-edit "$TRK_BASE" || { git merge --abort 2>/dev/null; exit 1; }
if [ -n "$TRK_VERIFY" ]; then
  sh -c "$TRK_VERIFY" || exit 1
fi
cd "$TRK_REPO" || exit 1
[ "$(git branch --show-current)" = "$TRK_BASE" ] || exit 1
git merge --ff-only "$TRK_BRANCH" || exit 1
```

The seed local landing recipe merges the current base into the issue branch, aborts conflicts, runs nonempty `TRK_VERIFY` with `sh -c`, then fast-forwards the guarded primary checkout. Explicit failure exits prevent fallthrough; a failed verify is normalized to exit 1 even if it returned 75. Empty verification skips only the command, never the worktree merge. This local gate applies only when the issue type uses an issue worktree and branch; a type opting out has no issue branch to merge or test. PR checks are unchanged.

Default PR scope: the agent opens the PR and waits; trk closes the issue when it merges. The agent fixes reported base-branch conflicts and failed CI on the head commit unattended. Review threads and merging belong to the human unless they ask the agent to handle them. The following example uses `gh`'s built-in jq filter and the head commit's `statusCheckRollup` (check runs and commit statuses); it names a conflicting base, prints failed jobs and their URLs, and fails if the PR closes without merging. Other open states keep waiting:

```sh
pr_check=$(cat <<'CHECK'
result=$(gh pr view 12 --json state,baseRefName,mergeable,headRefOid,statusCheckRollup --jq '
  if .state == "MERGED" then "merged"
  elif .state == "CLOSED" then "PR closed without merging"
  elif .mergeable == "CONFLICTING" then "PR conflicts with base branch \(.baseRefName)"
  else
    .headRefOid as $head |
    [.statusCheckRollup[]? |
      select(
        (.__typename == "CheckRun" and .status == "COMPLETED" and
          (.conclusion | IN("FAILURE", "TIMED_OUT", "CANCELLED", "ACTION_REQUIRED", "STARTUP_FAILURE", "STALE"))) or
        (.__typename == "StatusContext" and (.state | IN("FAILURE", "ERROR")))
      ) |
      "\(.name // .context): \(.conclusion // .state) \(.detailsUrl // .targetUrl // "")"
    ] |
    if length > 0 then "CI failed on head \($head):\n" + join("\n")
    else "waiting" end
  end
') || exit "$?"
case "$result" in
  merged) exit 0 ;;
  waiting) exit 75 ;;
  *) printf "%s\n" "$result"; exit 1 ;;
esac
CHECK
)
trk wait <ID> --json -r "PR 12 merged" --every 10m -c "$pr_check"
```

Entering waiting releases the claim with reason `waiting`; nobody holds the issue. The session and terminal may stay open as the waiting session (see Claims). Running `wait` on an issue that is already waiting replaces its check, interval and reason, which is how a human repairs a bad check without reopening.

On failure, the CLI checks the latest claim released as `waiting`: its nonblank terminal must be confirmed live on its known local claim host, and its pinned launcher must provide `send`. MCP liveness is independent and is not a prerequisite. `[check] max_wakes` limits restorations per session (default `5`, including when omitted from existing configs); `0` disables waking. In the same transaction that records the failed check, trk verifies `wake_count < max_wakes`, restores that session's actor, worktree, host, agent, terminal and launcher profile in a new claim with a fresh lease, and increments the persistent `wake_count`. Its MCP connection can renew and use its tools again once running; a dormant connection must restart with the same identity first. The CLI then sends a prompt with `wake N of M` and the check output asking the session to address the failure and end again, normally with `wait`.

The woken session fixes only what the check reported, then ends the new cycle, normally with the same `wait`. It should use `hand_back` with evidence when the same failure keeps returning rather than repeat indefinitely. While waiting, it can still add notes and receive requests from the human.

Entering or updating waiting preserves `wake_count`. A fresh explicit claim, including a stolen claim, resets it to zero; an idempotent claim by the current owner does not. Only automatic failed-check claim restoration increments it, not human resume, a retry, pass, or failed check that reopens without restoration. While claimed or waiting with a nonzero count, issue JSON includes top-level `wake_count` and pretty `trk show` displays a `wakes` line; otherwise the count is omitted from these displays.

With no live session or the wake cap reached, the issue reopens. A capped failure records `wake cap of M reached` together with the check output in its note. If sending the prompt fails after restoration, trk releases that exact claim and reopens with an explanatory note; it never releases a replacement claim or overrides a subsequent session action, and the restoration still counts. Exit 0 and 75 do not restore claims. trk interprets only exit codes, not PRs, CI or other external state.

#### Human-authorized resume

`trk resume <ID> -m MESSAGE --json` lets a human continue the original live waiting session, with or without a check. The message is required and must be nonempty after trimming (`bad_args` otherwise). Agent actors are refused with `not_permitted` and a hint to request human authorization. Agents must not invoke this human-only CLI path autonomously, impersonate a human or change actor settings to bypass this boundary. The distinct MCP `resume` tool reports a human chat reply for no-check waits only (below).

The CLI requires the latest waiting session's nonblank terminal to be confirmed live on its known local claim host, and its pinned launcher to provide `send`; MCP liveness is not required. It takes the waiting queue lock; an in-flight check yields `invalid_state` with retry guidance, not concurrent delivery. Do not clear the lock to force resume. Restoration transactionally verifies the issue is still waiting, its latest waiting claim still matches and its event revision has not changed. It restores the original actor, worktree, host, agent, terminal and launcher profile in a new claim with a fresh lease before delivering the human's message. The original session can then renew its lease and use lifecycle tools once its MCP connection is running (restart it if dormant). No replacement is launched.

Human resume preserves `wake_count` unchanged: it neither consumes nor resets the automatic `[check] max_wakes` budget and does not require remaining automatic wakes. The timeline records the human authorization and message, the restored claim and the delivery outcome, making this distinct from an automatic failed-check wake.

Success uses the normal JSON envelope with `data` containing `issue`, `actor`, `terminal`, `claim`, `resumed_from`, `message` and `delivered: true`. This confirms launcher delivery only, not agent acknowledgement, execution or completion. `trk send` only delivers text and cannot restore a waiting claim.

A gone or unknown terminal/session (including a remote or unknown host), missing terminal or missing send configuration returns `invalid_state` with guidance for the human to run `trk reopen <ID> -m "Restart with the decision"` then `trk run <ID>`. A send failure after restoration returns nonzero `internal`; compensation releases only that exact restored claim to open and records a failure note, never releasing a replacement claim or overriding subsequent session actions. Inspect current issue state before recovery.

#### Same-session chat resume

With `[wait] chat_resume = true` (the default, including when omitted), the original waiting agent may call MCP `resume` with `message` containing an actual human reply received in its chat, verbatim. This is an agent's report, not human authorization: the event and note are attributed to the agent. Notes, tool results, check output, agent-generated text and trk-delivered terminal text never qualify. All trk terminal deliveries use `[trk-delivery source=<source> actor=<actor>]`, a newline, the body, a newline and `[/trk-delivery]`; sources include `trk-send`, `check-wake`, `human-resume`, `check-notification` and briefing when relevant. Even a marked delivery quoting a human is not a human chat reply.

In one store write transaction, chat resume requires an agent actor matching the latest claim released as `waiting`, an issue still waiting, no active claim, no waiting check, and the enabled setting. It restores that same session's claim with its original metadata and a fresh lease; it neither probes terminal liveness nor sends terminal text. The required message must be nonempty after trimming, but is stored verbatim. A distinct `resume_chat` event carries `{message, resumed_from, claim}` and an agent-attributed note begins `Session reported a human reply in chat:` followed by a blank line and the original message.

Chat resume preserves `wake_count`. A wait with a check always refuses chat resume with `not_permitted` and guidance to request `trk resume`; this cannot bypass `[check] max_wakes`. Disabling `[wait] chat_resume` also refuses this path. Human CLI resume remains available for both kinds of wait and retains its separate human authorization, liveness and delivery requirements.

### Actors

| Kind | Identifier | Example |
| --- | --- | --- |
| human | `human:<email>` | `human:rodrigo@example.com` |
| agent session | `agent:<harness>:<session-id>` | `agent:claude-code:12938192381` |

The kind ends at the first colon, and for agents the harness ends at the second. The rest is the session id, kept verbatim. For sessions started by `trk run`, it is a UUID minted by trk before launch. An agent actor may record `on_behalf_of`, the human who launched it. The acting actor comes from `--as`, then `TRK_ACTOR`, then the human in the user config. Checks are recorded as the human who ran `trk check`.

### Claims

- `claim` requires a ready issue. It records a claim and moves the issue to `claimed` in one transaction. A partial unique index allows at most one active claim per issue.
- Invariant: an issue is `claimed` if and only if it has an active claim.
- Agent claims get a lease (default 30 minutes), renewed by `trk renew`, by the MCP server while it runs, and by any write from the claimant. Human claims have no lease by default.
- A claim records where the work happens: `worktree` (absolute path) and `host`. `trk claim` fills them when it runs inside the project's checkout or one of its worktrees.
- Only the claimant may close, wait or unclaim a claimed issue; a human's `--force` overrides and is recorded.
- A claim ends with one of: `closed`, `waiting`, `unclaimed`, `expired`, `stolen`.
- For agent actors, a claim also records the agent definition (`agent`), launcher terminal (`terminal`) and selected named launcher profile (`launcher`), set by `trk run`. Legacy claims have no profile name.
- The **waiting session** is the actor of the latest claim, if that claim was released with reason `waiting` and the issue is still waiting. It holds no claim or lease. Closing or reopening the issue ends this access; a later claim replaces the old session.

**Enforcement.** Agent actors write only to issues they currently claim, plus creating follow-ups of such an issue. A waiting session may still append notes and create follow-ups, but may not close, wait, hand back or otherwise mutate the issue except through guarded same-session MCP chat resume for a no-check wait. The rule is in `trk-core` and applies to the CLI as well. CLI `resume` requires human authorization; MCP `resume` records only the waiting agent's report of a verbatim human chat reply and never authorizes checked waits. Agents must not impersonate a human or treat marked trk deliveries as human replies. Actor identity is not a security sandbox, so hard isolation requires running the harness where the database is not reachable.

### Notes and history

Every mutation appends one or more rows to `event`, attributed to the actor, with a UTC timestamp. Notes are events with `op = 'note'`. A correction is a new note with `amends` pointing at the earlier one. Database triggers reject `UPDATE` and `DELETE` on `event`, and `DELETE` on `issue`.

A check that says "not yet" writes no event and does not touch `updated_at`: an issue that waits a week on a five-minute interval would otherwise bury its history under two thousand identical lines. A pass or a failure writes a `check` event followed by the `close`, resumed `claim` or `reopen` it caused.

## `trk check`

```
trk check [ID] [--all] [--watch [PAUSE]] [--dry-run] [--no-gc]
```

One pass:

1. Read the queue: waiting issues with a check whose `next_check_at` has come, ordered by priority, then due time. `--all` ignores the due time; an `ID` runs that issue alone, due or not.
2. Take the lock (below). If another pass holds it, report `busy` and do nothing.
3. For each issue, in order, one at a time: run `sh -c <check>` and record the result.
4. Unless an `ID` was supplied, `[check] gc = false`, or `--no-gc` was passed, sweep eligible closed-as-done issues while still holding the queue lock, using the same lifecycle transaction and safety gates as `trk gc`.
5. Release the lock.

`--watch` repeats passes with a pause (default 30s) until interrupted. `trk check` is otherwise one-shot, so cron or launchd can drive it. `--dry-run` prints the queue, each command and its directory, and previews eligible cleanup without running checks, removing resources or writing cleanup notes.

**Automatic cleanup.** The end-of-pass sweep considers all closed-as-done issues, including manual closes and issues closed in earlier passes, even when no checks were due. Abandoned issues are excluded. `[check] gc` defaults to `true` and `[check] gc_grace` to `"1h"`, including when omitted from existing configs. The grace is a minimum time since closure, not a cleanup deadline: the next pass after it elapses may collect safe resources, while unsafe resources remain protected and can be reconsidered later. `--no-gc` disables cleanup for the entire invocation, including every watch pass; `trk check ID` never sweeps.

Cleanup errors are advisory: they neither undo check outcomes nor change the check command's exit status. Check JSON data optionally includes `gc: {dry_run: bool, issues: [...]}`, with the same issue/resource objects as manual `trk gc` and an optional `errors: [string, ...]`. Omit `gc` when there are no eligible issues and no errors. Pretty output emits one concise line per issue with removals (planned removals in a dry run); skips and their reasons are shown once per issue per run, not on every watch pass.

**Executable upgrades.** Before each queued issue and during the watch pause,
compare the installed executable's device/inode and file metadata with the startup
snapshot, not semantic versions. On replacement, finish the active command, stop
its remaining process group and record/deliver its outcome, then release the queue
lock before exec of the installed path. Keep the original arguments (including
argv[0]), working directory and environment. An anonymous inherited descriptor
carries completed results across exec; its private environment locator is restored
to its original value at startup, before argument parsing or subprocess creation.
One-shot output includes results from before and after exec. Completed IDs are
excluded from the resumed pass even for `--all` and explicit IDs; watch results
already printed are not repeated. An idle watcher notices replacement without
waiting for its configured pause. A missing path during installation is tolerated
until it reappears; exec errors fail the runner but leave recorded outcomes intact.
If cleanup or lock release fails, retain ownership rather than exec unsafely.

**Running a check.** The command runs in `wait_cwd`; when the issue was claimed outside a checkout, in the repository's primary checkout. A directory that no longer exists is a failure. The environment adds:

| Variable | Value |
| --- | --- |
| `TRK_ISSUE` | the issue id |
| `TRK_WORKTREE` | the directory the check runs in |
| `TRK_REPO` | the project root, when it is a git checkout |
| `TRK_BRANCH` | the current branch of the worktree (`trk run` names it after the issue) |
| `TRK_BASE` | the resolved `[run] base` branch |
| `TRK_DIR` | the project's `.trk/`, so a check that calls `trk` reaches the same database |

`TRK_ACTOR` is removed. Stdin is `/dev/null`; stdout and stderr are captured together. The command runs in its own process group, and after `[check] timeout` (default 10m) the group gets `SIGTERM`, then `SIGKILL` two seconds later.

**Recording.** The result is written in one transaction that first re-reads the issue. If it is no longer waiting on exactly the command that ran (a human closed it, reopened it, or changed the check meanwhile), the result is dropped. A requested resumption must still match the latest historical waiting claim and remain below the wake cap. Failed checks restore the claim and increment the wake count in this transaction, before sending the repair prompt; failed delivery conditionally releases only that restored claim.

**Results.** `trk check --json` reports `passed` (closed), `retry` (waiting), `woken` (failed check, original session resumed), `failed` (failed check, reopened, including a reached wake cap or failed wake delivery), or `skipped` (changed meanwhile). Both `woken` and `failed` include the output. A failed check is a recorded outcome, not a CLI error. If wake delivery fails but the issue has already moved on, the guarded release does nothing and the result is `skipped`.

**The lock.** A single-row table holds the pid, host and start time of the running pass. It is taken in a write transaction, so two passes cannot both take it. A holder whose process no longer exists on this host is replaced. A holder on another host is never replaced automatically.

**Why serial.** Checks commonly merge a branch into the base branch in the primary checkout. Running them one at a time makes `trk check` a merge queue: the second merge sees the first, and a conflict returns that one issue as work to its live session or the open queue.

### Trust

A check is written by an agent and executed later, unattended, with the user's privileges and outside whatever sandbox the harness ran in. This grants agents nothing they did not have (they already had a shell), but it moves execution to a time when nobody is watching. So: `trk show` always prints the check in full, `trk check --dry-run` shows the queue first, each failure keeps its output, and `trk check` runs only when the user starts it.

## Agents and sessions

`trk run [ID]` starts an agent on the issue in a persistent `hc` terminal. Without an ID, it selects the first ready issue, ordered by priority, then creation time, then ID, matching `trk ready`. No ready issue returns `not_found`. Selection also applies to `--dry-run` and `--print`; an explicit ID retains its existing behavior. The agent sees one issue. Agents reach trk only through MCP: their instructions never mention the `trk` CLI.

### Configuration pieces

| Piece | Defines |
| --- | --- |
| Type | Name, description, agent routing and whether to use a worktree; no workflow or custom fields |
| Agent | Who does the work: instructions, briefing, other MCP servers, harness preference, model override, extra per-harness arguments, built-in tool level |
| Harness profile | How to run an agent in one harness: command, arguments, default model, conditional model arguments, environment, files, how MCP servers and instructions are passed |
| Launcher | Where it runs: named profiles with optional lifecycle command templates and driver defaults |

```toml
# agents/implementer.toml
name        = "implementer"
description = "Implements an issue in its worktree and gets the change landed"

instructions = "prompts/implementer.instructions.md.j2"   # → MCP server instructions
briefing     = "prompts/briefing.md.j2"                   # → initial prompt
inbox        = true

# [trk_tools]
# include = ["get_task", "add_note", "close", "hand_back"]   # default: all seven

[[mcp]]
name    = "github"
command = "github-mcp-server"
args    = ["stdio", "--toolsets", "pull_requests"]
env     = { GITHUB_TOKEN = "${env:GITHUB_TOKEN}" }

[harness]
prefer        = ["omp", "claude-code"]   # first installed wins; `trk run --harness` overrides
builtin_tools = "default"                # no extra args; otherwise a profile-defined level
model         = { omp = "provider/reviewer-model", claude-code = "reviewer-model" }
args          = { omp = ["--thinking", "high"] }
```

The agent for a run is `trk run --agent X` (a one-off override), otherwise the issue type's `agent`.
The seed types are `code-change` (description `code change`, agent `implementer`),
`research` (agent `researcher`) and `review` (agent `reviewer`, read-only built-in tools).
A type file is `types/<name>.toml` with `name`, `description`, `agent` and optional
`worktree` (boolean, default `true`). Only types configure `worktree`; agents, harnesses
and `--agent` do not override it. The `code-change` seed omits it and keeps the default;
the research and review seeds set `worktree = false`. False launches in the primary
project root without creating or reusing an issue branch or worktree, and records no
worktree path in the claim.
`[issues] default_type` is written by `trk install` and supplies omitted types for CLI
creation, editor templates and MCP follow-ups; `trk whoami` reports it. Older configs
fall back to `code-change`. Explicit types override it. Core inserts always supply
the type, so the legacy SQLite column default is inert.
`trk agent check` validates agent files, templates and harness profiles.

Types, agents and prompts resolve by filename from the project's `.trk/` first, then
the optional user directory: `$TRK_USER_DIR`, else `$XDG_CONFIG_HOME/trk`, else
`~/.config/trk`. File contents are not merged. Template includes use the same search
order, independent of the including template's layer. There is no trust gate: both
directories are user-controlled configuration, including executable launch templates.
An agent's `harness.prefer` orders the harnesses it may use; when omitted it inherits
`[harness] prefer` from merged `config.toml`. An explicit empty list overrides that
default; `trk run --harness` overrides selection. Seed agents omit their preference
and the complete seed config sets `prefer = ["omp", "claude-code"]`.
`trk show --json` routing and `trk run --dry-run --json` expose routing provenance,
including the resolved `worktree` boolean, before execution. `trk agents`,
`trk run --dry-run`, and `trk agent check` report agent/type/template layer origins.

An agent's optional `harness.model` is either a string for all harnesses or a table
of harness names to model strings. Resolution is `trk run --model M`, then the
agent's string or selected-harness entry, then the selected profile's `model`.
A missing table entry falls back to the profile. If none resolves, the harness
chooses its default; trk does not inject model-selection arguments.
Agent `harness.args` is a table of harness names to extra argument lists (default
empty); only the selected harness's list is added, without shell splitting.
`trk agents` shows effective models by profile; JSON exposes configured `model`,
effective `models` keyed by harness (null for harness default), and configured `args`.

### The trk MCP server

The harness starts `trk mcp --issue <ID> --actor <actor> --agent <name>` over stdio. No tool takes an issue id: the server serves exactly one issue. It renews the claim's lease while it runs, so a crashed session releases its claim when the lease expires.

| Tool | Arguments | Effect |
| --- | --- | --- |
| `get_task` | | The issue: description, state, notes, blockers. Also the resource `trk://task` |
| `add_note` | `body` | Appends a note |
| `link` | `url`, `label?` | Attaches an http(s) address to the issue, or changes the label of one already attached |
| `report_followup` | `title`, `description`, `priority?`, `blocks_this?` | Creates an issue in the same project; with `blocks_this`, it blocks the current one |
| `hand_back` | `note` | Unclaims with a note: claimed → open |
| `close` | `reason` (`done`, `abandoned`), `note` | claimed → closed |
| `wait` | `reason`, `check?`, `interval?` | claimed → waiting |

The set is fixed. An agent definition may narrow it with `trk_tools.include`. `get_task` remains readable without a claim. After `wait`, the waiting session may call `add_note`, `link` and `report_followup`; `wait`, `close` and `hand_back` remain refused because it holds no claim. Once the issue leaves waiting, this exception ends. After `close` or `hand_back`, further writes are refused.

With `inbox = true`, `trk note <ID> --to-agent` messages are attached once to the owning active or waiting session's next tool result, including an error result. An old or unrelated session cannot consume them. Waiting sessions do not renew the released claim's lease.

### Prompt channels

| Channel | Carries | Mechanism |
| --- | --- | --- |
| Briefing | The task: title, description, every note, blockers | Rendered from the agent's `briefing` template and passed as the harness's initial-prompt argument, or sent with the launcher's `send` template after launch |
| Instructions | Rules for the whole session, including the waiting protocol with examples | The MCP server's `instructions` and the tool descriptions. If a harness profile declares that its harness ignores server instructions, trk folds them into the briefing |
| Mid-session | Steering | `trk send <ID> "…"` via the launcher; `trk note <ID> --to-agent "…"` queues a message that the MCP server attaches to the agent's next trk tool result |

Notes are in the briefing because they are the issue's memory across sessions. An issue reopened by a failed check carries the check's output as its latest note, so the next session starts from the failure.

Both templates receive this MiniJinja context in `trk run`, the MCP server and
`trk agent preview`; existing variables retain their meanings:

| Variable | Contract |
| --- | --- |
| `issue` | Issue JSON with dependencies and derived fields; briefing includes all notes |
| `project.name` | Name of the project root directory |
| `project.root` | Project root path, not the session worktree |
| `project.prefix` | `[issues] prefix` |
| `type.name`, `type.description` | Resolved type name and description from `types/<type>.toml`, unaffected by an agent override |
| `config.check.interval`, `config.check.timeout` | Configured duration strings |
| `config.check.max_wakes` | Numeric configured automatic wake cap |
| `config.claims.agent_lease` | Configured lease duration string |
| `config.run.worktrees` | Configured directory string, without tilde expansion or root resolution |
| `config.run.landing` | Landing recipe name, default `"merge"` |
| `agent.name`, `agent.description` | Selected agent metadata |
| `tools` | Available tools as `{name, description}` objects |
| `worktree` | Session worktree path or null |
| `base` | Resolved `[run] base` branch |
| `actor` | Session actor |
| `harness` | Harness name or null |
| `fold_instructions` | Whether instructions are supplied for folding into the briefing |
| `instructions` | Rendered instructions when folding, otherwise null |

`trk agent check` uses the same context with a sample project root `/tmp/project`,
the configured issue prefix and settings, and sample type `code-change` with description
`Sample type description`. It does not require a project type to be mapped to every agent.

### `trk run`

```
trk run <ID> [--agent A] [--harness H] [--model M] [--fg] [--print] [--dry-run]
```

1. Check the issue is ready, resolve its agent and harness, and select the first available launcher in `[launcher] use`. Fail before claiming if none is available.
2. If the type's `worktree` is true, create or reuse the worktree. New issue branches start at the resolved base: `git worktree add <worktrees>/<ID> -b <ID> <base>`. Existing issue branches are reused rather than reset to the base. The branch is always named after the issue, so a later session finds earlier work. Otherwise use the primary project root without creating a branch or worktree.
3. Mint the actor `agent:<harness>:<uuid>`; the harness's own session id, once known, goes in `actor.meta`.
4. Claim the issue with the actor, agent, optional worktree path, terminal name and selected launcher profile. Bind a returned session address after launch. A type with `worktree = false` records no claim worktree path.
5. Render the launch spec: resolved `model` (string or null), briefing, MCP server list (trk plus the agent's others), environment (`TRK_ACTOR`, for attribution only), files, argv.
6. Start with the selected launcher's `start` template or driver default. `--print` emits the launch spec as JSON instead; `--dry-run` also skips the claim.

Failures while preparing the launch spec, writing its files, or starting the launcher release the claim with a `launch failed: <reason>` note, leaving the issue open and ready to retry. The worktree and branch may remain. A Herdr `launch unconfirmed` error retains the claim to avoid a duplicate launch.

Related commands: `trk attach <ID>` and `trk send <ID>` (only when their launcher templates are set) target the active claim's terminal, or the waiting session's terminal when there is no active claim. An active claim takes precedence even if it has no terminal; historical terminals are never a fallback once the issue leaves waiting. `trk runs` lists active and waiting agent sessions with `state` (`claimed` or `waiting`), agent, harness, terminal, claim metadata, terminal/session liveness and separate MCP liveness. A waiting row's lease is historical, not active. `trk reap` releases only active claims whose recorded local terminal is confirmed gone, recording a note; it leaves waiting, dormant and unknown sessions alone. Normal lease expiry remains independent.

The worktree outlives the claim: a waiting issue's check runs in it, or in the primary project root if no worktree was recorded. Closing an issue retains its resources until manual `trk gc` or an eligible end-of-pass `trk check` sweep removes them.

### `trk gc`

```
trk gc [ID...] [--dry-run] [--abandoned] [--older-than DURATION] [--json]
```

Collect resources of closed issues in this project, optionally limited to the given IDs and to issues closed longer ago than `DURATION` (for example `7d`). By default only reason `done` is eligible; `--abandoned` also includes abandoned issues. Open, claimed and waiting issues are never collected. Closing an issue does not itself remove resources.

GC takes the check queue lock and uses an immediate database transaction to serialize eligibility and cleanup with lifecycle changes. It examines all historical claims, not only the latest, and removes only resources recorded on the local host:

- **Sessions:** use each historical claim's pinned launcher's optional `close` operation. Herdr discovers the recorded exact terminal with `herdr-app ls`, then closes its containing space with `herdr-app do space.close`. Never infer a session from a title. Protect the caller's current terminal's space and spaces used by other issues' active or waiting sessions; unavailable discovery or unsafe targets are skipped.
- **Worktrees:** only descendants of the configured worktree directory are eligible, never the current worktree. Use `git worktree remove` without force; dirty worktrees and other Git refusals are skipped, not overridden.
- **Branches:** only the branch named exactly after the issue ID, from the primary checkout, after every recorded worktree is removed or absent. Use `git branch -d`, never `-D`; unmerged, checked-out or otherwise refused branches are skipped.

Claims without a recorded worktree have no worktree or issue-branch resources to collect;
GC does not infer a branch from the issue ID alone. Their recorded sessions are still
eligible for session cleanup.

`--dry-run` reports the same selection and safety decisions without removing resources or appending notes. Each real removal appends a note attributed to the caller; absent or skipped resources do not, so a second run adds no cleanup note when nothing remains.

Pretty output reports each issue's session, worktree and branch outcome and reason. The standard JSON envelope's `data` is `{dry_run: bool, issues: [{id, session: {status, reason, items: [...]}, worktree: {status, reason, items: [...]}, branch: {status, reason}}]}`. Status is `removed`, `absent` or `skipped`. Session items include `terminal`, `space`, `status` and `reason`; worktree items include `path`, `status` and `reason`. In a dry run, a would-be removal has status `removed` with a `would remove ...` reason and top-level `dry_run: true`; it does not mean deletion occurred. Refusals remain visible as skipped outcomes.

### Launcher

```toml
[launcher]
use = ["herdr", "hc"]

[launcher.herdr]
driver = "herdr"
attach = ["hc", "attach", "{name}"]
send = ["hc", "send", "{name}", "{text}"]

[launcher.hc]
available = ["hc", "ls"]
start = ["hc", "run", "--bg", "--no-parent", "--name", "{name}", "--cwd", "{cwd}", "--", "sh", "-c", "{shell_command}"]
attach = ["hc", "attach", "{name}"]
send = ["hc", "send", "{name}", "{text}"]
```

`{shell_command}` changes into the worktree and sets the environment; `--cwd` also sets the terminal's launch folder. `--no-parent` keeps the session independent of the launching terminal.

Named profiles live at `[launcher.<name>]`. `use` is an ordered list; each profile's optional `available` argv must exit successfully, otherwise selection continues. Without `available`, a profile is usable (the Herdr driver supplies its own availability probe). No available profile fails before claiming or launching. The seed also supplies an opt-in `tmux` profile; select it with `use = ["tmux"]`.

Optional argv arrays are `available`, `start`, `attach`, `send`, `exists`, `focus`, `close` and `rename`. Missing operations are skipped, not replaced with a different profile's operation. Arguments are never shell-split. Substitutions are `{name}` (session address), `{cwd}`, `{shell_command}`, `{text}` and `{title}`. The first stdout line from `start` replaces the generated name; empty stdout retains it. Stderr is diagnostic output only. The tmux seed prints a stable session ID with `new-session -P -F '#{session_id}'`, uses `has-session` for existence, `rename-session`, `kill-session`, `attach-session` and `send-keys` for the corresponding operations.

The selected profile name is persisted on claims, copied through resume/check wakes, and used for attach/focus, send, liveness, notifications and GC regardless of later changes to `use`. Keep profiles referenced by claims configured. Root send delay/retry settings are inherited unless a profile overrides them. Background launches require a named launcher profile. Historical and non-launcher claims without a name have no known launcher ownership: they remain visible with unknown terminal liveness, cannot be attached to, sent to, resumed or woken, and terminal GC skips them (including automatic check/watch sweeps). Their independently owned Git resources remain eligible for cleanup. No current profile is inferred for them. An explicitly named but missing or invalid profile remains a configuration error.

Session liveness requires a recorded launcher profile, a known claim host matching this machine and a nonblank recorded terminal. The Herdr driver discovers exact terminals. Other profiles use optional `exists` argv: exit 0 means live, nonzero means gone; command errors or a missing/empty operation mean unknown. Unknown liveness never authorizes a wake, manual resume or reap; a failed check without a confirmed live session reopens instead. Wakes and resume also require the pinned profile's `send`.

A stopped MCP server with a live terminal is **dormant**, not a dead session. Terminal liveness controls wake/resume eligibility and reap, independently of MCP process metadata. The session can receive instructions while dormant, but must restart its MCP connection with the same identity to use tools and renew leases again. Renewal stops while MCP is stopped, so active claims can still expire through the normal lease sweep; terminal liveness does not prevent expiry.

**Dashboard.** `trk dashboard` shows every issue that is not closed, grouped by what it needs and sorted by priority inside each group: *needs you* (waiting with no check, a failed check or lost session that returned the issue to open, a session whose terminal is confirmed gone, an expired lease, the wake cap reached), *in flight* (claimed), *waiting* (on a check), *ready* and *blocked* (with its open blockers). Terminal/session liveness and MCP liveness are shown separately; a dormant MCP connection alone does not mark a live session dead or move it to needs you. Each row shows priority, issue id, issue type and title, with the type immediately after the id. Under each title, a second line says who holds the issue, what it waits for and what blocks it. `Enter` opens the selected issue in full: session, waiting condition, dependencies, description, notes and history. Below the list are the latest events of all issues. The header counts each group and says whether a check runner holds the queue lock. The dashboard never runs checks.

The view is one read-only snapshot (`trk_core::dashboard`), re-read when the database changes; reading it writes nothing, so an expired lease is reported, not swept. `--json` prints that snapshot. Keys act on the selected issue: `r` run, `a` go to the session, `s` send, `m`/`M` note (to the agent), `x` close, `o` reopen, `u` unclaim, `R` reap, `p` priority, `e`/`c` edit and create in `$EDITOR`, `/` filter. `run`, `reopen`, `unclaim` and `reap` ask first; `x` and `u` override an agent's claim, as `--force` does. `a` uses the session's pinned profile's `focus` operation, or its `attach` operation in the dashboard's terminal when focus is absent.

`driver = "herdr"` supplies guarded start/close and defaults for exists, focus and rename; explicit exists/focus/rename operations override those defaults. Its start invokes `herdr-app do space.new`: the space's name is the issue title, its folder is the worktree (session scratch for issues without a repository), and its first terminal runs the harness shell command. The launching space and its tabs are unchanged. The returned terminal address is bound to the claim and used by `trk attach`, `trk send` and `trk runs`.

trk observes `herdr-app ls` for up to two seconds; only a direct agent pane in the created space, with the expected title and folder, counts as visible. JSON returns `placement.status` (`visible` or `failed`) with pane/tab/space IDs or a reason. Observation failures warn and retain the session and claim. If UI discovery fails before selection, the seed's next profile (`hc`) is selected instead. If space creation itself fails or returns an uncertain response, the command errors and retains the claim; it never retries or falls back to a second session.

After a waiting check commits a passing outcome, trk best-effort invokes the pinned profile's optional `rename`, passing `[closed] <issue title>` as `{title}` and `{text}`. The Herdr default uses `herdr-app do space.rename --id … --name …` to prepend `[closed] ` to the current name of the space containing the exact historical waiting terminal, preserving user renames and embedded literal markers. An existing leading marker is not duplicated; old trailing ` [closed]` markers are removed. This update is independent of MCP liveness and notification delivery; rename failures do not affect the committed outcome or prevent notification. Failed/retrying checks do not rename. Watchers restart after executable replacement without replaying completed checks.

Replacing Herdr with tmux is a config change: set `[launcher] use = ["tmux"]`.

### Harness profiles

```toml
[harness.claude-code]
command = "claude"
args    = ["--strict-mcp-config", "--mcp-config", "{{ mcp_json_file }}", "{{ briefing }}"]
model_args = ["--model", "{{ model }}"]
builtin_tools = { read-only = ["--disallowedTools", "Edit,Write,Bash,NotebookEdit"], none = ["--tools", ""] }

[harness.omp]
command = "omp"
args    = ["--config", "{{ session_dir }}/omp.yml", "{{ briefing }}"]
model   = "openai-codex/gpt-6-luna"
model_args = ["--model", "{{ model }}"]
files   = { ".omp/mcp.json" = '''{"mcpServers":{{ '{' }}{% for server in mcp_servers %}{{ server.name | tojson }}:{"command":{{ server.command | tojson }},"args":{{ server.args | tojson }},"env":{{ server.env | tojson }}}{% if not loop.last %},{% endif %}{% endfor %}}}''', "{{ session_dir }}/omp.yml" = "skills:\n  ignoredSkills: [trk]\n" }
```

All profile command strings, arguments, environment values, file paths and
contents, and optional arguments are MiniJinja templates. Each is rendered
once: braces in a substituted value are literal, not recursively expanded.
Built-in-tool arguments are inserted before the first argument using the briefing,
including a MiniJinja briefing expression.

The harness context (distinct from the agent prompt context) contains:

| Variable | Value |
| --- | --- |
| `name`, `issue`, `actor`, `agent`, `harness` | Terminal name, issue ID, actor, agent name and harness name, all strings |
| `model` | Resolved model string, or an empty string when unset |
| `cwd`, `worktree`, `session_dir` | Launch directory, worktree (or `cwd` when absent) and scratch directory |
| `briefing`, `briefing_file`, `instructions`, `instructions_file` | Rendered prompt strings and their file paths |
| `mcp_json_file`, `trk_command` | Generic pretty MCP JSON file path and trk executable path |
| `trk_args`, `trk_env`, `trk_tools` | trk argument list, environment map and available trk tool-name list |
| `mcp_servers` | Server list with trk first; each object has `name`, `command`, `args` and `env` |

Each server's environment has `${env:NAME}` expanded from trk's environment;
the remaining servers follow the agent's `[[mcp]]` declaration order.

`tojson` serializes compact JSON without HTML escaping. `toml` serializes inline
TOML, quoting strings, separating array entries with comma-space and formatting
tables as `{key = value}` without extra padding. Built-in `map` and `join` filters,
loops and expressions are available. MCP dialects belong to profiles: omp and pi
loop over `mcp_servers` to build `mcpServers`; OpenCode builds `$schema` and `mcp`,
with each server's `type = "local"`, command list `[server.command] + server.args`,
`environment = server.env` and `enabled = true`. Codex filters `trk_command`,
`trk_args` and `trk_env` through `toml`. `mcp_json_file` always refers to generic
pretty JSON regardless of the harness.

Harness profiles use MiniJinja expressions and loops; launcher templates use
single-brace placeholders. Rendered context values are never expanded recursively.

Profile `model` is an optional default string. `model_args` defaults to an empty
list and is included only when a model resolves. `{{ model }}` expands to that
model, or an empty string when unset. Optional argument lists are inserted in
order: the agent's `builtin_tools` level arguments, conditional profile
`model_args`, then agent `harness.args`. They precede the first profile argument
using `briefing`, or are appended when none uses it (including send mode).
All optional arguments support the same MiniJinja context.
All seeded profiles have `model_args = ["--model", "{{ model }}"]`; only omp has
a default `model`.

| Harness | MCP attachment | Confidence |
| --- | --- | --- |
| omp | Worktree `.omp/mcp.json`; a `--config` overlay hides the trk skill | Verified live |
| Claude Code | `--mcp-config` with generic JSON file, `--strict-mcp-config` to exclude other servers | High |
| OpenCode | `OPENCODE_CONFIG_CONTENT` inline runtime config | High |
| Codex CLI | `-c mcp_servers.*` overrides of `config.toml` | Medium |
| pi | Per-session agent directory with `mcp.json` | Medium |

A harness's built-in tools are not MCP and can only be limited through that
harness's flags. `builtin_tools` levels are open-ended strings, such as `read-only`,
`none`, `no-network` or `plan-only`. `default` is reserved: it adds no restriction
arguments, even if a profile defines that key. Every other level must appear in
at least one harness profile's `builtin_tools` map; an empty argument list is an
explicit declaration of support without extra arguments. `trk agent check`
errors on undefined levels, listing the available names (including `default`),
and warns when a preferred harness cannot enforce the requested level.
`trk run` refuses a selected harness without that level, with an error and hint,
before creating a worktree or claiming the issue. This applies equally to
automatic selection and `--harness`; trk does not silently launch unrestricted
or skip to another harness.

## Architecture

One binary, one database file, one write path. Every interface (the CLI for humans, the per-issue MCP server for agents, `trk check`) calls the same core library. That library is the only code that writes to the database, which is why rules SQLite cannot check (claims, the state machine, acyclic blockers) can be relied on.

### Crates

| Crate | Role |
| --- | --- |
| `trk-core` | Domain types, the state machine, claim rules, the waiting queue, query-language parser and SQL compiler, `Store` (the only writer) |
| `trk-agent` | Agent definitions, the six trk tools and their executor, prompt rendering, harness profiles, launch-spec builder |
| `trk-mcp` | Per-issue MCP server over `trk-core`; renews the claim's lease while it runs |
| `trk-cli` | The `trk` binary: argument parsing, actor and project resolution, rendering, the check runner, `trk run` and the launcher templates |

### Files and locations

Project state is in `<project root>/.trk/`, created by `trk install`; shared configuration is optional:

| What | Path | Override |
| --- | --- | --- |
| Project directory | `<project root>/.trk/` | `TRK_DIR`, `--dir` |
| Database | `.trk/trk.db` |  |
| Config | `.trk/config.toml`, `agents/`, `types/`, `prompts/` |  |
| User configuration | `$XDG_CONFIG_HOME/trk/` or `~/.config/trk/` | `TRK_USER_DIR` |
| Worktrees | `.trk/worktrees/<ID>` | `[run] worktrees` |
| Per-session scratch | `.trk/sessions/<actor-uuid>/` |  |
| Check logs | `.trk/checks/` |  |

```
.trk/
  .gitignore           # "*": nothing in .trk/ is tracked until this is edited
  trk.db
  config.toml          # user, issue id prefix, leases, check interval and timeout, worktrees,
                       # launcher templates, harness profiles, named views
  agents/*.toml        # one file per agent
  types/*.toml         # name, description, agent routing, optional worktree boolean
  prompts/*.j2         # MiniJinja templates for instructions and briefings
```

Settings resolve **built-in defaults < user < project**. The user directory has the
same configuration layout as `.trk/`: `config.toml`, `agents/`, `types/`, and `prompts/`,
but no database or session state. A missing user directory is ignored. `trk whoami`
shows its resolved path. The CLI and MCP server use the same resolution, including
MCP started with only `TRK_DIR`.

`config.toml` merges recursively per key: tables merge, arrays and scalar values
replace lower-layer values. `[harness.<name>]` profiles and `[views.<name>]` views merge
by name and then by key. Project `[issues] prefix` and `[user]` identity win over the
user layer; absent keys inherit. Typed built-in defaults fill settings absent from both
layers; there are no runtime built-in resource files or harness profiles.
Launcher operation arrays are optional. Omitted skill settings use
`.agents/skills/trk` without links; harness detection does not change skill settings.
Normal `trk install` writes complete project settings and all resource files, so the
project remains self-contained. Types, agents and prompts are read when routing is resolved.

`[run] base` configures the base branch (for example, `base = "develop"`). When writing
a new config, `trk install` detects the branch named by `origin/HEAD`, then the current
branch of the primary checkout, then falls back to `main`. At runtime an explicit
`base` wins; when omitted, runtime uses the same detection order. The resolved
branch is used for new issue branches, prompt-template `{{ base }}` and waiting-check
`TRK_BASE`.

`[run] landing` is a string selecting `prompts/_landing.<value>.md.j2`, exposed as
`config.run.landing` in instruction and briefing templates. It defaults to `"merge"`
when omitted. The seed includes `_landing.merge.md.j2` (verified local merge into the
primary checkout) and `_landing.pr.md.j2` (GitHub pull request targeting the resolved
base branch and a merge/CI/conflict check). Only `implementer.instructions.md.j2`
includes the selected landing template. `_rules.md.j2` contains generic tracker and
waiting rules shared with the researcher and reviewer, without either landing recipe.
Both built-in presets (`default` and `minimal`) select landing recipes this way.
Custom values are not an enum: for example, `landing = "gitlab"` selects a
project-provided `prompts/_landing.gitlab.md.j2` without a code change.
`trk agent check` emits a configuration error if the selected template does not exist,
even when no agent includes it; included templates are also rendered for validation.

| Check setting | Default, also when omitted | Meaning |
| --- | --- | --- |
| `[check] verify` | `""` | Opaque shell command, exported as `TRK_VERIFY` to checks and `verify` to prompt templates; used by the seed local landing recipe |
| `[check] gc` | `true` | Sweep closed-as-done issues at the end of non-ID check passes; `--no-gc` disables it for the invocation |
| `[check] gc_grace` | `"1h"` | Minimum duration since closure before automatic cleanup, not a removal deadline |

`verify` is not parsed into arguments or automatically executed by the check runner. The seed implementer instructions name the configured command, falling back to generic project tests when empty. Include dependency setup in the command when needed (for example, `npm ci && npm test`). Verification runs under the whole check's `[check] timeout` (default 10m) and holds the serialized queue. For slow suites, configure a fast subset or increase the timeout.

### Project resolution

A command finds its project from the working directory: `TRK_DIR` / `--dir` when given; else the
nearest ancestor containing `.trk/trk.db`; else the primary checkout of the working directory's
git repository (the first entry of `git worktree list`), because a linked worktree does not
contain the ignored `.trk/`. `trk run` passes `TRK_DIR` to the session's MCP server and
`trk check` passes it to checks, so both reach the project from any directory.

Without an installed `.trk/` every command except `trk install` fails with `not_installed`
(exit 3) and a hint to run `trk install`; the database is never created implicitly. `trk install`
targets the same location: the primary checkout's root in a git repository, else the working
directory. It writes `.trk/` (keeping existing config files unless `--force`), creates or
migrates the database, and installs the agent skill at `.agents/skills/trk` (linked from
`.claude/skills/trk`), excluded from git through `.git/info/exclude`.
Existing type files are preserved: after the research and review seed defaults change
to `worktree = false`, `trk install` lists `types/research.toml` and `types/review.toml`
in `differ` without overwriting them. Users may add the setting themselves or use
`trk install --force` to replace files with current defaults.

`trk install --minimal` writes only project-specific `config.toml` settings (`[user]`
identity, `[issues] prefix`, `[run] base), the database and ignore file, and installs the
CLI skill as usual. It does not seed agents, types or prompts; those and shared settings
must be supplied by the user directory. Existing project resources are not deleted.
It is distinct from `--preset minimal`, which installs a self-contained implementer/code-change
preset. `--minimal` is mutually exclusive with `--preset`, `--from`, and `--list-presets`.
All install modes preserve existing config unless `--force` is passed.

### Connection and transactions

- Each connection is opened with `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON` and `busy_timeout=5000`.
- Each mutating command is exactly one `BEGIN IMMEDIATE` transaction: sweep expired leases, validate, check claims, write rows, append events, commit. A command either happens fully or not at all.
- A check runs outside any transaction; taking the lock, registering/clearing its process group and recording the result are separate transactions. A spawned shell waits for authorization over a pipe until its group is durably registered; EOF before authorization cannot execute the check.
  The runner owns the shell/group and queue lock together. Shell exit, timeout and INT/TERM/HUP terminate remaining group members (TERM, then KILL after two seconds), reap the shell, then clear group ownership. Cleanup failures retain the lock. Runner interruption stops the queue/watch loop and leaves the interrupted issue waiting without recording a partial result.
  Crash recovery replaces a same-host holder only after both its runner and all live group members are gone; zombies do not count. Unknown hosts/liveness fail closed. Checks must not escape their process group; they are not sandboxed.
- Read commands use deferred transactions, so they never wait on writers under WAL.
- Expired claim leases are swept at the start of every write transaction: one indexed `UPDATE`, so no daemon is needed.
- The schema is applied on open, keyed on SQLite's `user_version`. `trk backup <path>` uses `VACUUM INTO` to write a consistent copy.
- A database ahead of the executable's compiled migration set is refused without downgrading or resetting it: `newer_database` (exit 1), with `database`, `database_version` (SQLite `user_version`), `supported_version` and a recovery hint. Active MCP sessions must restart/reconnect using a compatible executable with the same session identity and project directory; there is no automatic hot upgrade. Recovery does not reclaim issues or bypass authorization/dependencies; see README's active-session upgrade procedure.

### Issue IDs

IDs are `<prefix>-<suffix>`, where the prefix is `[issues] prefix` in the project's config (set by `trk install`, from the directory name or `--prefix`) and where the suffix is 3 random lowercase base32 characters, lengthened on collision (e.g. `orch-4kq`). The CLI accepts a unique suffix prefix (`trk show 4kq`) and fails with `ambiguous_id` instead of guessing.

## Data model

The schema is [`crates/trk-core/src/schema.sql`](crates/trk-core/src/schema.sql). SQLite enforces what it can: foreign keys, one active claim per issue, the set of states, close reasons and priorities, that a closed issue has a reason and a closing time, that only a waiting issue has waiting fields, the actor id format, JSON well-formedness, and an append-only event log. `trk-core` enforces the rest: the transitions, claimant rules, and acyclic blockers.

| Table | Holds |
| --- | --- |
| `harness`, `actor` | Who acts; a harness row is added the first time one of its sessions acts |
| `issue` | One row per issue: authored fields, `state`, `close_reason`, the waiting fields, timestamps |
| `dep` | `src` blocks `dst` |
| `link` | The current links of each issue: `url`, optional `label` |
| `claim` | Every claim, active or released, with worktree, host, agent, terminal and release reason |
| `event` | The append-only history; ops: `create set claim release wait check close reopen dep_add dep_rm note` |
| `inbox` | Delivery state of `--to-agent` notes |
| `check_lock` | The one running `trk check` |
| `search` | FTS5 over titles, descriptions and notes |

Views: `note` (events with `op = 'note'`), `active_claim`, `issue_view` (issue plus claimant and lease), `ready`.

```sql
-- Ready: open and every blocker closed.
SELECT i.* FROM issue i
WHERE i.state = 'open'
  AND NOT EXISTS (SELECT 1 FROM dep d JOIN issue b ON b.id = d.src
                  WHERE d.dst = i.id AND b.state <> 'closed');

-- The check queue.
SELECT * FROM issue
WHERE state = 'waiting' AND waiting_check IS NOT NULL
  AND (next_check_at IS NULL OR next_check_at <= :now)
ORDER BY priority, next_check_at, id;

-- Cycle check before inserting blocker -> blocked: refused if the blocker
-- is reachable from the blocked issue.
WITH RECURSIVE reach(id) AS (
  SELECT :blocked
  UNION
  SELECT d.dst FROM dep d JOIN reach r ON d.src = r.id
)
SELECT 1 FROM reach WHERE id = :blocker LIMIT 1;

-- Lease sweep, run at the start of every write transaction
-- (followed by one 'release' event per row and a state reset to 'open').
UPDATE claim SET released_at = :now, release_reason = 'expired'
WHERE released_at IS NULL AND lease_until < :now
RETURNING issue_id, actor;
```

## CLI API and UX

Every command builds one typed result, then renders it in the requested format. Pretty output is a view of the JSON, never a separate query, so humans and agents always see the same facts. The JSON format is a versioned API.

### Global options

| Option | Env | Default | Meaning |
| --- | --- | --- | --- |
| `--format pretty\|json\|jsonl` | `TRK_FORMAT` | `pretty` on a TTY, `json` otherwise | Output format; `jsonl` for `list` and `log` |
| `--json` |  |  | Shorthand for `--format json` |
| `--as <actor>` | `TRK_ACTOR` | `human:<[user] email>` | Acting actor |
| `--dir <path>` | `TRK_DIR` | found from the working directory | The project's `.trk` directory |
| `--no-color` | `NO_COLOR` |  | Disable color |

### Commands

| Command | Purpose |
| --- | --- |
| **Issues** |  |
| `trk list [QUERY] [--fields F,..] [--sort F] [--limit N]` | Query issues; `@view` runs a saved view |
| `trk ready [QUERY]` | Ready issues |
| `trk show <ID>... [--with notes,events,deps,claims]` | Full issue with derived state |
| `trk create [TITLE] [-d TEXT] [-p P] [--type T] [--blocked-by ID]... [--blocks ID]... [--run] [--dry-run]` | Create; type defaults to `code-change`; opens `$EDITOR` when there is no title on a TTY; `--run` immediately starts the new issue's agent |
| `trk edit <ID> [-t TITLE] [-d TEXT] [-p P] [--type T] [--dry-run]` | Change the authored fields; `$EDITOR` with no options |
| `trk claim <ID> [--lease 30m] [--worktree PATH] [--steal]` | open → claimed |
| `trk unclaim <ID> [-m NOTE] [--force]` / `trk renew <ID>` | claimed → open / extend the lease |
| `trk close <ID> [-r done\|abandoned] [-m NOTE] [--force] [--dry-run]` | Close from open, claimed or waiting |
| `trk wait <ID> [-c CHECK] [--every DUR] [-r REASON] [--force] [--dry-run]` | claimed → waiting; on a waiting issue, replace the check |
| `trk reopen <ID> [-m NOTE]` | waiting or closed → open |
| `trk check [ID] [--all] [--watch [PAUSE]] [--dry-run] [--no-gc]` | Run the checks of waiting issues, then optionally sweep closed-as-done resources |
| `trk note <ID> [TEXT\|-] [--amends SEQ] [--to-agent]` | Append a note; `--to-agent` also queues it for the agent's inbox |
| `trk dep add\|rm <BLOCKER> <BLOCKED>` | Add or remove a blocks edge |
| `trk link <ID> <URL> [-l LABEL]` | Attach an http(s) address, or change its label |
| `trk unlink <ID> <URL>` | Remove a link |
| `trk log <ID>` | Timeline: events and notes |
| **Agent sessions** |  |
| `trk run <ID> [--agent A] [--harness H] [--fg] [--print] [--dry-run]` | Start an agent session; agent override is one-off |
| `trk runs` | Active and waiting agent sessions, marked by state, with agent, harness, terminal and claim metadata |
| `trk dashboard` (`dash`) | Live full-screen overview; without a terminal or with `--json`, the snapshot once |
| `trk attach <ID>` / `trk send <ID> TEXT` | Attach to or type into the active session, or the waiting session when no claim is active |
| `trk reap` | Release claims whose session has ended, with a note |
| `trk gc [ID...] [--dry-run] [--abandoned] [--older-than DURATION]` | Collect closed issues' local sessions, worktrees and safely deletable issue branches |
| `trk mcp --issue ID --actor A --agent N` | The per-issue MCP server (started by the harness, not by hand) |
| `trk agent list` / `preview <agent> [--issue ID]` / `check` | Inspect and validate agent definitions |
| **Setup** |  |
| `trk install [--force] [--prefix P] [--minimal] [--preset NAME] [--from DIR]` | Set trk up in the project; minimal configuration relies on shared user resources, while preset/custom-source modes install files |
| `trk install --list-presets` | List embedded install presets without installing anything |
| `trk whoami` | Resolved actor, project and user directories, database, prefix, skill state, issue counts, running check |
| `trk backup <PATH>` | Consistent copy via `VACUUM INTO` |

`-d` and `-c` take text, `@file`, or `@-` for stdin. In the interactive editor, `title`, `priority` and required `type` are TOML front matter and the description is the markdown body; if validation fails on save, the file reopens with the errors added as comments. `--dry-run` runs every check and returns the would-be result or the error, and writes nothing.

`trk create --run` creates the issue, then runs that exact ID through `trk run` with
default agent routing, harness selection and launcher settings, returning the run result
instead of the issue (`data.issue` identifies it in JSON). It conflicts with `--dry-run`.
Creation is not rolled back if running fails: the error includes the created issue ID,
and the caller can retry with `trk run <ID>`.

### Query language

A query is one string of terms joined by implicit AND. It is parsed into a typed AST and compiled to parameterised SQL.

```
query  := term (WS term)*
term   := NOT? (pred | flag)
pred   := name op value | name 'in' '(' value (',' value)* ')'
op     := '=' | '!=' | '<' | '<=' | '>' | '>=' | '~'      # '~' = full-text or glob
flag   := 'ready' | 'blocked' | 'due' | 'stale-lease'
value  := word | quoted | 'me' | '.' | reltime              # reltime: -7d, -12h
name   := 'id' | 'state' | 'priority' | 'type' | 'reason' | 'claimed-by'
        | 'created-by' | 'title' | 'created' | 'updated' | 'closed' | 'next-check'
        | 'text' | 'blocks' | 'blocked-by'
```

Queries cover the project's whole database; there is no cross-project scope.

| Example | Meaning |
| --- | --- |
| `trk list 'state!=closed'` | Open work |
| `trk ready 'priority<=P1'` | Ready work that is urgent or high |
| `state=waiting` | The waiting queue, with and without checks |
| `due` | Waiting issues `trk check` would run now |
| `claimed-by=me` / `claimed-by=agent:claude-code:*` | Current claims |
| `blocks=orch-m1 state!=closed` | What remains before `orch-m1` is ready |
| `closed>-7d reason=done` | Finished this week |
| `text~"feature flag"` | Full-text search over title, description and notes |

`--fields` sets table columns in pretty mode and object keys in JSON mode, using the same names plus `detail`, `waiting`, `check`, `ready`, `due` and `lease`. Saved views in the config store a query, fields and sort under a name.

### JSON envelope

```json
{
  "ok": true,
  "schema_version": 4,
  "data": {
    "id": "orch-4kq",
    "title": "Add claim leases",
    "description": "Crashed agents must release their claims.",
    "state": "waiting",
    "priority": "P1",
    "type": "code-change",
    "close_reason": null,
    "waiting": {
      "reason": "merge into base",
      "check": "cd \"$TRK_WORKTREE\" || exit 1; git merge --no-edit \"$TRK_BASE\" || { git merge --abort 2>/dev/null; exit 1; }; if [ -n \"$TRK_VERIFY\" ]; then sh -c \"$TRK_VERIFY\" || exit 1; fi; cd \"$TRK_REPO\" || exit 1; [ \"$(git branch --show-current)\" = \"$TRK_BASE\" ] || exit 1; git merge --ff-only \"$TRK_BRANCH\" || exit 1",
      "interval": 300,
      "next_check_at": "2026-11-21T18:07:11Z",
      "last_check_at": "2026-11-21T18:02:11Z",
      "last_check_exit": 75,
      "cwd": "/data/trk/worktrees/orch-4kq"
    },
    "claim": null,
    "deps": {
      "blocked_by": [],
      "blocks": [{"id": "orch-m1", "title": "MVP", "state": "open", "priority": "P2"}]
    },
    "derived": {
      "ready": false,
      "blocked_by": [],
      "due": false,
      "unblocks_higher_priority": []
    },
    "created_by": "human:rodrigo@example.com",
    "created_at": "2026-11-21T17:40:03Z",
    "updated_at": "2026-11-21T18:02:11Z",
    "closed_at": null
  }
}
```

- `list` returns `data` as an array; `jsonl` emits one issue object per line, without the envelope.
- `waiting` is `null` unless the issue is waiting; `claim` is `null` unless it is claimed.
- `derived` is computed when the output is built, never stored. An issue with blockers also gets `derived.progress` (`{total, closed}`).
- `trk check` returns `{"busy": bool, "results": [{issue, result, exit, timed_out, duration_ms, state, next_check_at, output?}]}` with `result` one of `passed`, `retry`, `failed`, `skipped`. A failed check is a result, not an error: the command exits 0.
- Timestamps are RFC 3339 UTC in JSON. Relative times appear only in pretty mode.
- Within a `schema_version`, keys are only ever added. Renaming or removing a key bumps the version. Version 3 replaced the former issue `agent` with required `type`. Version 4 removes `project` and `repo` from issues: a database holds one project.

### Errors and exit codes

In JSON mode, errors are written to stdout in the same envelope:

```json
{"ok": false, "schema_version": 4, "error": {
  "code": "blocked",
  "message": "orch-9xa is blocked by orch-4kq",
  "issue": "orch-9xa",
  "blocked_by": ["orch-4kq"],
  "hint": "close its blockers first, or remove the edge with `trk dep rm`"
}}
```

| Exit | Meaning | Codes |
| --- | --- | --- |
| 0 | OK |  |
| 1 | Internal/compatibility error | `internal`, `db_corrupt`, `newer_database` |
| 2 | Usage error | `bad_args`, `bad_query` |
| 3 | Not found | `not_found`, `ambiguous_id`, `not_installed` |
| 4 | Validation failed | `invalid_field`, `invalid_state`, `cycle`, `config_invalid` |
| 5 | Conflict | `claimed_by_other`, `not_claimant`, `blocked`, `busy`, `not_permitted` |

### Pretty mode

```
$ trk list 'state!=closed'
 ID         PRIORITY  STATE      DETAIL                            TITLE
 orch-4kq   P1        ◷ waiting  merge into base · check in 4m     Add claim leases
 orch-9xa   P2        ● claimed  claude-code:1293… 12m             Rate-limit the API
 orch-2pp   P2        ◷ waiting  SQLite or Dolt? · needs a human   Pick a store
 orch-m1    P2        ○ open     ⛔ blocked (3/5 done)              MVP
```

Color follows the state: open is neutral, claimed is yellow, waiting is cyan, closed is dim green. The DETAIL column shows the claimant and its age, what a waiting issue waits for and when it is checked next, or a blocked marker with blocker progress. Agent ids are shortened in tables and shown in full by `show`. `show` prints, in order: header, rendered description, the waiting block with the check in full, dependencies, claim, derived state, notes.

## Rust dependencies

The dependency set is small: no ORM, no C library outside SQLite. `trk-core` and the CLI are synchronous. Only `trk-mcp` runs an async runtime (single-threaded Tokio, required by `rmcp`), and it calls the core through `spawn_blocking`.

| Crate | Crates | Used for |
| --- | --- | --- |
| `rusqlite` (features: `bundled`) | core | SQLite access; `bundled` compiles SQLite in, so the binary has no system dependency and FTS5 is available |
| `rusqlite_migration` | core | Applying the schema, keyed on `user_version` |
| `serde`, `serde_json` | all | Typed structs, the envelope, MCP tool schemas, launch spec |
| `toml` | core, agent, cli | Config files, editor front matter |
| `jiff` | core, cli | UTC timestamps, leases, intervals, relative times |
| `winnow` | core | Query-language parser |
| `rand` | core | Random issue-id suffixes |
| `uuid` | cli | Agent actor ids minted by `trk run` |
| `thiserror` | core | Typed errors that map one-to-one to JSON error codes and exit codes |
| `minijinja` | agent | Instruction and briefing templates |
| `rmcp`, `tokio` | mcp | The official Rust MCP SDK over stdio, on a single-threaded runtime |
| `clap` | cli | Arguments, subcommands, env fallbacks |
| `anstream`, `owo-colors`, `comfy-table`, `termimad` | cli | Color, tables and markdown in pretty mode |
| `anyhow` | cli, mcp | Error context at the binary boundary only |

Git, `sh` and `kill` are invoked as executables through `std::process`. Tests use `assert_cmd`, `insta` (which pins the JSON format) and `tempfile`.

## Open questions and future work

### Open questions

- [ ] **Maximum wait.** Nothing ends a wait whose check answers "not yet" forever (an abandoned PR). `trk list state=waiting --sort next-check` shows them; an optional deadline that turns into a failure would make it automatic.
- [x] **Worktree cleanup.** Explicit `trk gc` and automatic end-of-pass `trk check` sweeps collect closed-as-done local resources with the same queue lock, lifecycle transaction and conservative session, path and Git safety checks. Automatic cleanup defaults on with a one-hour minimum grace, not a deadline; abandoned, waiting and active issues remain protected. Config or `--no-gc` disables the sweep; explicit-ID checks never sweep, dry runs only preview, and cleanup errors are advisory.
- [ ] **Check lock across hosts.** A lock held from another host is never replaced. With one machine this cannot happen; with a shared database it would need a timeout.
- [ ] **Running agents automatically.** `trk check` reopens failed issues but does not start a session on them. A loop that runs `trk check` and then `trk run` on what is ready would close the cycle; it needs a limit on concurrent sessions first.
- [ ] **Lease length.** 30 minutes for agent claims is a guess. It should exceed the longest silent stretch of a typical agent session.
- [ ] **Harness profiles to verify.** Claude Code, Codex, OpenCode and pi were not run live.
- [ ] **Built-in tool policies.** Which `builtin_tools` levels each harness can actually enforce, read-only above all, for reviewer agents.

### Future work

| Item | Notes |
| --- | --- |
| ACP launch route | A second way to run agents: drive the harness over ACP, passing MCP servers in `session/new` and answering permission requests from policy. |
| Agent evaluation | Outcome statistics per agent and harness (time to close, hand-backs, failed checks) from claims and events. |
| Sync between machines | Replicate the `event` log as an operation stream instead of merging databases. |
| Tamper evidence | A hash chain over `event` without other schema changes. |
