---
name: trk
description: Use when the user tracks work with trk, the local issue tracker and agent launcher. Trigger when asked to find ready work, file or update issues, claim, close or reopen them, leave an issue waiting on a check (a merge, a PR, a decision), run the waiting queue with `trk check`, add notes or blockers, query what is open, blocked or waiting, or start, watch and steer agent sessions with `trk run`. Not for sessions started by `trk run`, which work through their trk MCP tools.
---

# trk

trk keeps a project's work in a local database inside the project, in `.trk/`. An issue has a title, a description, notes, a priority, a required type (default `code-change`) and one of four states. Use the `trk` CLI.

```
open → claimed → closed
                → waiting → closed   (its check passed, or a human closed it)
                          → open     (its check failed, or a human reopened it)
```

## Inside a trk session? Stop here

If you have trk MCP tools (`get_task`, `hand_back`, `wait`) or `TRK_ACTOR` starts with `agent:`, `trk run` started you. Use those tools only, not this skill or the CLI.

## Rules

- Pass `--json` on every command and read the envelope: `{"ok": true, "data": …}` or `{"ok": false, "error": {"code", "message", "hint"}}`. On an error, do what `hint` says and retry. Do not guess around it.
- Never rely on the interactive editor: give the title and description on the command line. Long text goes through stdin: `-d @-`.
- You act as the user, with their default actor. Do not pass `--as` unless they ask.
- State changes only through `claim`, `unclaim`, `close`, `wait`, `reopen` and `trk check`. `edit` changes title, description, priority and type.
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
# Done once the PR is merged; look every 10 minutes.
trk wait <ID> --json -r "PR 12 merged" --every 10m \
  -c 'case "$(gh pr view 12 --json state -q .state)" in MERGED) exit 0;; OPEN) exit 75;; *) exit 1;; esac'

# Done once the branch is merged into the main checkout: the check does the merge.
trk wait <ID> --json -r "merge into main" \
  -c 'cd "$TRK_REPO" && [ "$(git branch --show-current)" = main ] && git merge --no-edit "$TRK_BRANCH"'

# Waiting on a person: no check.
trk wait <ID> --json -r "Which store: SQLite or Dolt?"
```

The check is a shell command that `trk check` runs later, unattended, with `sh -c` in the directory the issue was claimed in:

| Exit | Meaning | Result |
| --- | --- | --- |
| 0 | pass | the issue closes as done |
| 75 | not yet | still waiting; checked again after `--every` (default 5m) |
| anything else, or a timeout | error | the issue returns to open, with the output in a note |

It may do the remaining work or only look. It may run more than once, so it must be safe to repeat, and it must never prompt. Let real failures exit non-zero on their own; do not turn them into 75. It sees `TRK_ISSUE`, `TRK_WORKTREE`, `TRK_REPO` (the main checkout) and `TRK_BRANCH`.

Without a check, the issue waits until a human runs `trk close <ID>` or `trk reopen <ID> -m "the answer"`. Running `trk wait` on an issue that is already waiting replaces its check, interval and reason.

## Run the waiting queue

```bash
trk check --json              # run the checks that are due, one at a time
trk check <ID> --json         # run one issue's check now
trk check --all --json        # run every check, due or not
trk check --dry-run --json    # show what would run and where
```

Each result is `passed`, `retry` or `failed`. Only one `trk check` runs at a time (`"busy": true` means another is running), so checks that merge never race. The user usually keeps `trk check --watch` running in a spare terminal; do not start that yourself, since it never returns.

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
trk runs --json                           # active sessions: agent, terminal, lease, MCP liveness
trk note <ID> --to-agent "Also update the docs" --json   # delivered with its next tool result
trk send <ID> "Wrap up and finish" --json                # typed into its terminal
trk reap --json                           # release claims whose session has ended
```

Only ready issues can be run. A session ends by closing, waiting or handing back; an issue whose check failed is ready again, and its notes tell the next session why. Do not run `trk attach`: it takes over the terminal interactively, so suggest it to the user instead.

Without an ID, `trk run` selects by priority, then oldest creation time, then ID, matching
`trk ready`. No ready issues is an error. `trk run --dry-run --json` previews the selection.

`trk run --agent NAME` overrides routing for that run only. Inspect routing sources with `trk show`
or `trk run --dry-run`. Types, agents and prompts live in the project's `.trk/` directory
(`types/*.toml`, `agents/*.toml`, `prompts/`), shared by all its worktrees.

## More

See [references/reference.md](references/reference.md) for the query language, every command, and the error codes.
