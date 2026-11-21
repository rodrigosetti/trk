---
name: trk
description: Use when the user tracks work with trk, the local issue tracker and agent launcher. Trigger when asked to find ready work, file or update issues (code-change, research, decision, approval, operation, milestone), claim, move or close them, add notes, links or dependencies, query what is open, blocked or gated, or start, watch and steer agent sessions with `trk run`. Not for sessions started by `trk run`, which work through their trk MCP tools.
---

# trk

trk keeps the work of every repository in one local database. Issues have a type, a status, fields, dependencies and an append-only history. Use the `trk` CLI.

## Inside a trk session? Stop here

If you have trk MCP tools (`get_task`, `hand_back`, `close_as_*`) or `TRK_ACTOR` starts with `agent:`, `trk run` started you. Use those tools only, not this skill or the CLI.

## Rules

- Pass `--json` on every command and read the envelope: `{"ok": true, "data": …}` or `{"ok": false, "error": {"code", "message", "hint", "missing_fields", "unmet_gates"}}`. On an error, supply what `missing_fields` and `hint` name and retry. Do not guess around it.
- Give every required field on the command line with `-f name=value`. Never rely on the interactive editor. Long text goes through stdin: `-f context=@-`.
- You act as the user, with their default actor. Do not pass `--as` unless they ask.
- Types, fields, statuses and requirements are configuration. Read them with `trk schema --json` and `trk schema <type> --json` instead of assuming.
- `derived.next` in `trk show <ID> --json` lists, for each next status and resolution, the fields still missing.
- Status changes only through `claim`, `unclaim`, `move` and `close`. `edit` changes fields only.
- Do not use `--force`, `--steal` or close someone else's claim unless the user asks.
- Issue ids look like `orch-4kq`; a unique suffix such as `4kq` also works.

## Orient

```bash
trk whoami --json                         # actor, project of this repo, config, database
trk ready --json                          # open, unclaimed, unblocked

trk list 'project=. category!=closed' --json
trk show <ID> --json --with notes         # fields, deps, claim, derived.next
trk log <ID> --json                       # every change and note, attributed
```
`trk ready` orders eligible issues by shared priority (P0 urgent/exceptional, P1 high, P2 normal, P3 low), oldest creation first, then ID. Priority is project-relative; it never changes blockers, claims, gates, or status, and never preempts active work. Set it with `trk create ... -f priority=P1` or `trk edit <ID> -f priority=P0`; new and upgraded issues default to P2. Priority changes appear in `trk log`.

`unknown_repo` means this repository is not registered. Ask the user which project it belongs to (`trk project list --json`), then `trk repo add --project <id>`.

## Create

`title` and `context` are always required. The other required fields depend on the type; `trk schema <type>` marks them with `*`. With the default config:

| Type | Also required | Notes |
| --- | --- | --- |
| code-change | `repo` | defaults to the current repo |
| research | `question` | |
| decision | `question` | options are optional |
| approval | `approver` (`human:<email>` or `me`) | link it with `--dep approves:<ID>` |
| operation | `change_kind`, `target`, `environment`, `change` | |
| milestone | none | group work with `blocks` edges |

```bash
trk create research --json -t "Which embedded DB?" -f question="SQLite or Dolt?" -f context=@- <<'EOF'
We need a store for the tracker. Constraints: single binary, FTS.
EOF

trk create code-change --json -t "Add claim leases" -f context="Crashed agents must release claims." \
  --dep blocks:orch-m1
```

`--dep KIND:ID` adds the edge `new KIND ID` at creation. For work found while doing another issue, use `--dep discovered-from:<that ID>`.

## Work an issue yourself

```bash
trk claim <ID> --json                     # enters the first intermediate status
trk note <ID> --json "Found the cause: …"
trk edit <ID> --json -f branch=feat/leases -f links+="pr=https://github.com/acme/orch/pull/12"
trk move <ID> in_review --json -f pr_url=https://github.com/acme/orch/pull/12
trk close <ID> --json -r merged -f merge_commit=9f2c1e0
trk unclaim <ID> --json -m "Blocked on the API change"     # give it back
```

Closing needs a resolution (`trk schema <type>` lists them), and often fields such as `outcome`, `record_url` or `evidence_url`. Gates also apply: an `approves` edge holds the gated step until its approval is closed as `approved`. Add `--dry-run` to check a step without writing.

## Dependencies

```bash
trk dep add <SRC> <KIND> <DST> --json     # read as "SRC KIND DST"
trk dep rm <SRC> <KIND> <DST> --json
```

Kinds: `blocks` (DST is not ready until SRC closes), `approves` (an approval gates DST), `informs`, `implements`, `discovered-from`, `supersedes` and `duplicates` (same type), `reverts`, `relates`.

## Agent sessions

```bash
trk run <ID> --dry-run --json             # preview: agent, harness, worktree, briefing
trk run <ID> --json                       # claim and start the agent in its own terminal
trk runs --json                           # active sessions: agent, terminal, lease, MCP liveness
trk note <ID> --to-agent "Also update the docs" --json   # delivered with its next tool result
trk send <ID> "Wrap up and submit" --json                # typed into its terminal
trk reap --json                           # release claims whose session has ended
```

Only ready issues can be run. Do not run `trk attach`: it takes over the terminal interactively, so suggest it to the user instead.

## More

See [references/reference.md](references/reference.md) for the query language, the `-f` syntax, every command, and the error codes.
