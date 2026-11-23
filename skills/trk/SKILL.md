---
name: trk
description: Use when the user tracks work with trk, the local issue tracker and agent launcher. Trigger when asked to find ready work, file or update issues, claim, close or reopen them, leave an issue waiting on a check (a merge, a PR, a decision), run the waiting queue with `trk check`, add notes or blockers, query what is open, blocked or waiting, or start, watch and steer agent sessions with `trk run`. Not for sessions started by `trk run`, which work through their trk MCP tools.
---

# trk

trk keeps a project's work in a local database inside the project, in `.trk/`. An issue has a title, a description, notes, a priority, a required type (default `code-change`) and one of four states. Use the `trk` CLI.

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

# Done once the branch is merged into the main checkout: the check does the merge.
trk wait <ID> --json -r "merge into main" \
  -c 'cd "$TRK_REPO" && [ "$(git branch --show-current)" = main ] && git merge --no-edit "$TRK_BRANCH"'

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

It may do the remaining work or only look. It may run more than once, so it must be safe to repeat, non-interactive and non-blocking (no `--watch`). Let real failures exit non-zero on their own; do not turn them into 75. It sees `TRK_ISSUE`, `TRK_WORKTREE`, `TRK_REPO` (the main checkout) and `TRK_BRANCH`.

A failed check restores the original claim with a fresh lease if the session's MCP server is alive on this host, its terminal is recorded (and present with Herdr), `[launcher] send` is configured, and its wake count is below `[check] max_wakes` (default `5`, also when omitted; `0` disables waking). Each restoration increments the persistent count, even if delivery fails. Entering or updating `wait` preserves it; a fresh explicit claim, including `--steal`, resets it to zero, but an idempotent claim by the current owner does not. The prompt includes `wake N of M` and the output to fix the failure and end again, normally with `wait`; the session's MCP tools work again. At the cap, the issue reopens with a note saying `wake cap of M reached` and retaining the output. A gone session or failed send also leaves it open. trk only interprets exit codes, never PRs or CI.

On each wake, fix only what the check reported and end the new work cycle, normally with the same `wait`. If the same failure keeps returning, `hand_back` with the evidence instead of repeating indefinitely. While waiting, the session can still add notes and receive requests from the human.

While claimed or waiting with a nonzero count, issue JSON includes top-level `wake_count` and pretty `trk show` displays a `wakes` line. Otherwise the count is omitted.

Without a check, the issue waits until a human closes it, resumes its live waiting session with a decision, or reopens it. Running `trk wait` on an issue that is already waiting replaces its check, interval and reason.

### Human-authorized continuation

Have the human run `trk resume <ID> -m "Use SQLite; continue implementation" --json` to resume the original live waiting session, whether or not it has a check. The message is required and must not be empty after trimming. Do not autonomously resume yourself or another session, impersonate a human, or change actor settings to bypass `not_permitted`; ask the human to authorize the continuation.

Resume restores the latest waiting session's original actor, worktree, host, agent and terminal with a fresh claim and lease before delivering the message. Its MCP lifecycle tools then work again. It never starts a replacement, and it preserves `wake_count` without consuming or resetting the automatic wake budget. The timeline records human authorization, the message and delivery outcome.

Success data contains `issue`, `actor`, `terminal`, `claim`, `resumed_from`, `message` and `delivered: true`. This means launcher delivery only, not that the agent read or completed the request. `trk send` delivers text only and does not restore a waiting claim.

An empty message is `bad_args`; an agent actor is `not_permitted`. `invalid_state` for an in-flight check means retry after it finishes, never delete the queue lock. A dead, remote or unavailable session, missing terminal or missing send configuration is also `invalid_state`: ask the human to run `trk reopen <ID> -m "Restart with the decision" --json`, then `trk run <ID> --json`. A failed send after restoration returns nonzero `internal`, releases only that exact restored claim to open and records a failure note without overriding later session actions; inspect `trk show <ID> --json` before recovery.

## Run the waiting queue

```bash
trk check --json              # run the checks that are due, one at a time
trk check <ID> --json         # run one issue's check now
trk check --all --json        # run every check, due or not
trk check --dry-run --json    # show what would run and where
```

Each result is `passed`, `retry`, `woken` (failed check, original session resumed), `failed` (reopened, including failed wake delivery), or `skipped` (changed meanwhile). Both `woken` and `failed` include output. Only one `trk check` runs at a time (`"busy": true` means another is running), so checks that merge never race. The user usually keeps `trk check --watch` running in a spare terminal; do not start that yourself, since it never returns.

## Blockers

```bash
trk dep add <BLOCKER> <BLOCKED> --json    # BLOCKED is not ready until BLOCKER is closed
trk dep rm <BLOCKER> <BLOCKED> --json
```

## Agent sessions

```bash
trk run --json                            # start the first ready issue
trk run <ID> --dry-run --json             # preview: agent, harness, worktree, briefing
trk run <ID> --json                       # claim and start the agent in its own terminal
trk runs --json                           # active and waiting sessions: state, terminal, claim metadata, MCP liveness
trk note <ID> --to-agent "Also update the docs" --json   # delivered with its next tool result
trk send <ID> "Wrap up and finish" --json                # typed into its terminal
trk reap --json                           # release claims whose session has ended
```

Only ready issues can be run. A session releases its claim by closing, waiting or handing back; a failed check wakes its original live session, otherwise reopens the issue with notes telling the next session why. Do not run `trk attach`: it takes over the terminal interactively, so suggest it to the user instead.

The waiting session is the actor of the latest claim released with reason `waiting`, only while the issue is still waiting. It can keep notes, file follow-ups and receive `--to-agent` messages, but cannot `wait`, `close` or `hand_back` again. `send` and `attach` use its terminal when no active claim exists; an active claim always wins, even without a terminal. `runs` marks it with `state: "waiting"`; its lease is historical and `reap` skips it. Closing, reopening or a replacement claim ends the old session's access.

Without an ID, `trk run` selects by priority, then oldest creation time, then ID, matching
`trk ready`. No ready issues is an error. `trk run --dry-run --json` previews the selection.

`trk run --agent NAME` overrides routing for that run only. Inspect routing sources with `trk show`
or `trk run --dry-run`. Types, agents and prompts live in the project's `.trk/` directory
(`types/*.toml`, `agents/*.toml`, `prompts/`), shared by all its worktrees.

## More

See [references/reference.md](references/reference.md) for the query language, every command, and the error codes.
