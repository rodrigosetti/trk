---
name: trk
description: Use when the user tracks work with trk, the local issue tracker and agent launcher. Trigger when asked to find ready work, file or update issues, claim, close or reopen them, leave an issue waiting on a check (a merge, a PR, a decision), run the waiting queue with `trk check`, add notes or blockers, query what is open, blocked or waiting, or start, watch and steer agent sessions with `trk run`. Not for sessions started by `trk run`, which work through their trk MCP tools.
---

# trk

trk keeps a project's work in a local database inside the project, in `.trk/`. An issue has a title, a description, notes, a priority, a required type (default from `[issues] default_type`, initially `code-change`) and one of four states. Use the `trk` CLI.

```
open → claimed → closed
                → waiting → closed   (its check passed, or a human closed it)
                          → claimed  (its check failed, or a human resumed its live session)
                          → open     (no live session, wake delivery failed, or a human reopened it)
```

## Inside a trk session? Stop here

If you have trk MCP tools (`get_task`, `hand_back`, `wait`) or `TRK_ACTOR` starts with `agent:`, `trk run` started you. Use those tools only, not this skill or the CLI.

## Rules

- Pass `--json` on every command and read the envelope: `{"ok": true, "data": …}` or `{"ok": false, "error": {"code", "message", "hint"}}`. On an error, do what `hint` says and retry. Do not guess around it.
- Never rely on the interactive editor: give the title and description on the command line. Long text goes through stdin: `-d @-`.
- Normally you act as the user, with their default actor. Do not pass `--as` unless they ask. Never change identity to evade permissions; `resume` requires the human's authorization, not an agent impersonating them.
- State changes only through `claim`, `unclaim`, `close`, `wait`, human-authorized `resume`, `reopen` and `trk check`. `edit` changes title, description, priority and type.
- Do not use `--force`, `--steal` or close someone else's claim unless the user asks.
- Issue ids look like `orch-4kq`; a unique suffix such as `4kq` also works.

## Orient

```bash
trk whoami --json                         # actor, project directory, database, id prefix
trk list --json                             # open issues in this project
trk list 'state=waiting' --json             # waiting issues
trk show <ID> --json                      # description, notes, blockers, claim, waiting, derived
trk log <ID> --json                       # every change and note, attributed
```

`trk ready` orders issues by priority (P0 urgent, P1 high, P2 normal, P3 low), then oldest first. A waiting issue is not closed, so what it blocks is not ready yet.

`not_installed` means trk has not been set up in this project. Ask the user before running `trk install` in the project's root: it creates `.trk/` (the database and config, ignored by git) and installs this skill. Each project has its own database; there is no view across projects.

New built-in configs detect launchers in order (Herdr, `hc`, tmux) and harness commands
in order (`omp`, `claude`, `codex`, `opencode`, `pi`; `claude` selects `claude-code`).
Install writes only available profiles into `[launcher] use` and `[harness] prefer`,
reports detection and warns when either list is empty. Existing configs are kept
unless `--force` is given; custom sources and `--minimal` shared-config installs are
not filtered. If no launcher is usable, install one, edit `[launcher] use`, or use
`trk run --fg` in the current terminal.

## Create

Only the title is required. Write a description that lets someone start cold: what is needed, why, and how to tell it is done.

```bash
printf '%s\n' "Crashed agents must release their claims. Add a lease to agent" \
  "claims and sweep expired ones at the start of every write." |
  trk create "Add claim leases" --json -p P1 -d @-

trk create "Enable leases in prod" --json --blocked-by orch-4kq
trk create "Review the lease change" --json --type review --blocks orch-9xa
```

`--blocked-by ID` and `--blocks ID` add blocker edges at creation. Types route work to agents: `code-change` → `implementer`, `research` → `researcher`, `review` → `reviewer`. Change a type with `trk edit <ID> --type research --json`; query with `trk list 'type=review' --json`. Types do not change lifecycle rules.

Create and type edits accept only names defined in the project's `.trk/types/*.toml`. Unknown types return `invalid_field` with allowed names in `allowed` and a hint pointing to `.trk/types/`; do not guess a type. The editor and MCP `report_followup` enforce the same rule; the MCP schema lists the project types and their descriptions. Removing a definition does not prevent listing, showing or editing other fields of existing issues. `trk agent check` reports a configured `default_type` without a matching definition.

## Work an issue yourself

```bash
trk claim <ID> --json                     # open → claimed; refused while a blocker is open
trk note <ID> --json "Found the cause: …"
trk edit <ID> --json -p P0 -t "New title"
trk close <ID> --json -m "Merged in 9f2c1e0"            # reason defaults to done
trk close <ID> --json -r abandoned -m "Not needed after all"
trk unclaim <ID> --json -m "Blocked on the API change"  # give it back
```

Notes are the issue's memory. When you close, say what was done; when you give an issue back, say what is left.

## Waiting

Use `wait` instead of `close` when your part is finished but the issue is not done until something else happens. It releases your claim.

```bash
# Done once the PR is merged; wake for conflicts or failed head-commit CI.
# gh's built-in jq filter reads the head commit's check runs and statuses.
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

# Merge your branch only after verifying the merged result in the issue worktree.
local_check=$(cat <<'CHECK'
cd "$TRK_WORKTREE" || exit 1
git merge --no-edit "$TRK_BASE" || { git merge --abort 2>/dev/null; exit 1; }
if [ -n "$TRK_VERIFY" ]; then
  sh -c "$TRK_VERIFY" || exit 1
fi
cd "$TRK_REPO" || exit 1
[ "$(git branch --show-current)" = "$TRK_BASE" ] || exit 1
git merge --ff-only "$TRK_BRANCH" || exit 1
CHECK
)
trk wait <ID> --json -r "verify and merge into base branch" -c "$local_check"

# Waiting on a person: no check.
trk wait <ID> --json -r "Which store: SQLite or Dolt?"
```

The check is a shell command that `trk check` runs later, unattended, with `sh -c` in the directory the issue was claimed in:

| Exit | Meaning | Result |
| --- | --- | --- |
| 0 | pass (PR merged) | the issue closes as done |
| 75 | not yet (CI pending, awaiting review, unresolved threads, approved but not merged, unknown mergeability) | still waiting; checked again after `--every` (default 5m) |
| anything else, or a timeout | error (base-branch conflict, failed head-commit CI, closed without merging, or query/authentication failure) | wake the original live session, otherwise reopen; keep the output in a note |

The PR states describe the example, not built-in PR handling. By default the agent opens the PR and waits; trk closes the issue when it merges. The agent fixes reported base-branch conflicts and failed head-commit CI unattended. Review threads and merging belong to the human unless they ask the agent to handle them. The check names the conflicting base or prints failed jobs and their URLs.

It may do the remaining work or only look. It may run more than once, so it must be safe to repeat, non-interactive and non-blocking (no `--watch`). Let real failures exit non-zero on their own; do not turn them into 75. It sees `TRK_ISSUE`, `TRK_WORKTREE`, `TRK_REPO` (the primary checkout), `TRK_BRANCH`, `TRK_BASE` (the resolved base branch) and `TRK_VERIFY` (`[check] verify`, default empty). Use quoted `"$TRK_BASE"` in landing checks rather than assuming a branch name.

`[check] verify` is an opaque shell command in `.trk/config.toml`, also exposed to prompt templates as `{{ verify }}`. The local recipe merges the current base into the issue worktree, aborts conflicts, runs the command there if nonempty, then fast-forwards the guarded primary checkout. Verification failure, including exit 75, fails the check rather than retrying. Empty verification skips only the command. This local gate applies only when the issue type uses an issue worktree and branch; a type opting out has no issue branch to merge or test. Include dependency setup in the command if needed, for example `verify = "npm ci && npm test"`. The whole check shares `[check] timeout` (default 10m) and holds the serialized queue: use a fast subset or raise the timeout for slow suites. The PR recipe is unchanged.

A failed check restores the original claim with a fresh lease if its nonblank terminal is confirmed live on its known local claim host, `[launcher] send` is configured, and its wake count is below `[check] max_wakes` (default `5`, also when omitted; `0` disables waking). MCP liveness is independent, not a prerequisite. Each restoration increments the persistent count, even if delivery fails. Entering or updating `wait` preserves it; a fresh explicit claim, including `--steal`, resets it to zero, but an idempotent claim by the current owner does not. The prompt includes `wake N of M` and the output to fix the failure and end again, normally with `wait`; MCP tools work once the connection is running (restart it if dormant). At the cap, the issue reopens with a note saying `wake cap of M reached` and retaining the output. A gone or unknown session or failed send also leaves it open. trk only interprets exit codes, never PRs or CI.

On each wake, fix only what the check reported and end the new work cycle, normally with the same `wait`. If the same failure keeps returning, `hand_back` with the evidence instead of repeating indefinitely. While waiting, the session can still add notes and receive requests from the human.

While claimed or waiting with a nonzero count, issue JSON includes top-level `wake_count` and pretty `trk show` displays a `wakes` line. Otherwise the count is omitted.

Without a check, the issue waits until a human closes it, resumes its live waiting session with a decision, or reopens it. Running `trk wait` on an issue that is already waiting replaces its check, interval and reason.

### Human-authorized continuation

Have the human run `trk resume <ID> -m "Use SQLite; continue implementation" --json` to resume the original live waiting session, whether or not it has a check. The message is required and must not be empty after trimming. Do not autonomously resume yourself or another session, impersonate a human, or change actor settings to bypass `not_permitted`; ask the human to authorize the continuation.

Resume requires the original session's terminal to be confirmed live on its known local claim host and `[launcher] send` to be configured, not a live MCP process. It restores the latest waiting session's original actor, worktree, host, agent and terminal with a fresh claim and lease before delivering the message. Its MCP lifecycle tools work once the connection is running (restart it if dormant). It never starts a replacement, and it preserves `wake_count` without consuming or resetting the automatic wake budget. The timeline records human authorization, the message and delivery outcome.

Success data contains `issue`, `actor`, `terminal`, `claim`, `resumed_from`, `message` and `delivered: true`. This means launcher delivery only, not that the agent read or completed the request. `trk send` delivers text only and does not restore a waiting claim.

An empty message is `bad_args`; an agent actor is `not_permitted`. `invalid_state` for an in-flight check means retry after it finishes, never delete the queue lock. A gone or unknown terminal/session (including a remote or unknown host), missing terminal or missing send configuration is also `invalid_state`: ask the human to run `trk reopen <ID> -m "Restart with the decision" --json`, then `trk run <ID> --json`. A failed send after restoration returns nonzero `internal`, releases only that exact restored claim to open and records a failure note without overriding later session actions; inspect `trk show <ID> --json` before recovery.

## Run the waiting queue

```bash
trk check --json              # run the checks that are due, one at a time
trk check <ID> --json         # run one issue's check now
trk check --all --json        # run every check, due or not
trk check --dry-run --json    # preview checks and eligible cleanup without changing anything
trk check --no-gc --json      # run checks without automatic cleanup
```

Each result is `passed`, `retry`, `woken` (failed check, original session resumed), `failed` (reopened, including failed wake delivery), or `skipped` (changed meanwhile). Both `woken` and `failed` include output. Only one `trk check` runs at a time (`"busy": true` means another is running), so checks that merge never race. The user usually keeps `trk check --watch` running in a spare terminal; do not start that yourself, since it never returns.

Non-ID passes also sweep all eligible closed-as-done issues, including manual closes,
under the same queue lock and safety gates as `trk gc`. Abandoned issues are excluded.
`[check] gc = true` and `gc_grace = "1h"` default even when omitted; the grace is a minimum
age since closure, not a cleanup deadline. `--no-gc` disables sweeping for the whole
invocation, including watch; explicit-ID checks never sweep. Dry runs only preview:
no checks run, resources are not removed and cleanup notes are not written.
Cleanup errors are advisory and never change check outcomes or exit status.
Optional `data.gc` contains `dry_run`, manual-GC-shaped `issues`, and optional `errors`
strings; it is absent when there are no eligible issues or errors. Pretty output
adds one line per issue with removals (planned removals for dry runs); skips and their
reasons are shown once per issue per run, not on every watch pass.

## Blockers

```bash
trk dep add <BLOCKER> <BLOCKED> --json    # BLOCKED is not ready until BLOCKER is closed
trk dep rm <BLOCKER> <BLOCKED> --json
```

## Links

```bash
trk link <ID> https://example.com/org/repo/pull/12 -l "PR #12" --json   # shown on the dashboard row; same URL again changes the label
trk unlink <ID> https://example.com/org/repo/pull/12 --json
```

A link points at what the work produced. trk only stores and shows it.

## Agent sessions

```bash
trk run --json                            # start the first ready issue
trk run <ID> --dry-run --json             # preview: agent, harness, worktree, briefing
trk run <ID> --json                       # claim and start the agent in its own terminal
trk runs --json                           # active/waiting sessions: terminal/session and separate MCP liveness
trk note <ID> --to-agent "Also update the docs" --json   # delivered with its next tool result
trk send <ID> "Wrap up and finish" --json                # typed into its terminal
trk reap --json                           # release active claims whose local terminal is confirmed gone
```

Only ready issues can be run. A session releases its claim by closing, waiting or handing back; a failed check wakes its original live session, otherwise reopens the issue with notes telling the next session why. Do not run `trk attach`: it takes over the terminal interactively, so suggest it to the user instead.

The waiting session is the actor of the latest claim released with reason `waiting`, only while the issue is still waiting. It can keep notes, file follow-ups and receive `--to-agent` messages, but cannot `wait`, `close` or `hand_back` again. `send` and `attach` use its terminal when no active claim exists; an active claim always wins, even without a terminal. `runs` marks it with `state: "waiting"`; its lease is historical and `reap` skips it. Closing, reopening or a replacement claim ends the old session's access.

Session liveness requires a known claim host matching this machine and a nonblank terminal. Herdr uses its existing `has_terminal` discovery. Non-Herdr launchers can configure optional `[launcher] alive` argv using `{name}`, for example `alive = ["tmux", "has-session", "-t", "{name}"]`: exit 0 means live, 1 means gone, and any other exit, command error, or missing/empty template means unknown. Unknown sessions are not woken/resumed or reaped; failed checks reopen instead. Wakes and resume still require `send`.

`runs` and the dashboard distinguish terminal/session liveness from MCP liveness. A stopped MCP server with a live terminal is **dormant**, not dead, and remains eligible for wakes or human-authorized resume. Restart its MCP connection with the same identity to use tools again. `reap` leaves dormant and unknown sessions alone. MCP lease renewal stops until restart, however; an active claim can still expire through the normal lease sweep independently of terminal liveness.

Without an ID, `trk run` selects by priority, then oldest creation time, then ID, matching
`trk ready`. No ready issues is an error. `trk run --dry-run --json` previews the selection.

`trk run --agent NAME` overrides routing for that run only. Inspect routing sources with `trk show`
or `trk run --dry-run`. Types, agents and prompts live in the project's `.trk/` directory
(`types/*.toml`, `agents/*.toml`, `prompts/`), shared by all its worktrees.

## Clean up closed issues

```bash
trk gc --dry-run --json                   # preview closed-as-done resources
trk gc <ID> --json                        # collect one closed issue's resources
trk gc --older-than 7d --abandoned --json # also include abandoned issues older than a week
```

Closing retains resources until manual GC or an eligible automatic check sweep. Only
closed-as-done issues are eligible by default; manual `--abandoned` also includes abandoned
issues. Open, claimed and waiting issues are never collected.
GC considers all historical claims but only local-host resources. It protects the current worktree,
the caller's terminal space and other issues' active/waiting sessions; worktree paths must be under
the configured directory. Herdr sessions are matched by exact terminal, not title. Git removals
are not forced, and only the issue-ID branch is deleted with `-d` after its recorded worktrees
are removed or absent. Do not bypass skipped safety checks.

Read each issue's `session`, `worktree` and `branch` status and reason; session/worktree `items`
identify individual resources. `removed`, `absent` and `skipped` are outcomes, not lifecycle changes.
With `--dry-run`, `data.dry_run` is true and `removed` with a `would remove ...` reason means
planned, not deleted. Dry runs write no notes; real removals append caller-attributed notes,
and an unchanged second run adds none. See the reference for the full output shape.

## More

See [references/reference.md](references/reference.md) for the query language, every command, and the error codes.
