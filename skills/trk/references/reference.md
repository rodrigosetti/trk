# trk reference

## Query language

`trk list QUERY` and `trk ready QUERY` take one string of terms joined by AND.

```
name=value  name!=value  name<value  name<=value  name>value  name>=value
name~value             text search for `text`; glob or substring otherwise
name in (a,b,c)
NOT term  or  !term
ready  blocked  due  stale-lease     flags
```

Values: words, `"quoted strings"`, `me` (the acting actor), relative times such as `-7d`, `-12h` and `+30m`, dates (`2026-11-01`), and globs with `*` (`claimed-by=agent:*`).

`list` and `ready` query the project of the working directory, found from any subdirectory or worktree. There is no scope across projects.

```bash
trk list --json
trk list 'state!=closed' --json
trk list 'state=waiting' --sort next-check --json
trk list 'state in (open,claimed) priority in (P0,P1)' --json
trk list 'claimed-by=me' --json
trk list 'blocks=orch-m1 state!=closed' --json       # what remains before orch-m1 is ready
trk list 'closed>-7d reason=done' --json
trk list 'text~"feature flag"' --json
```

Options: `--fields id,state,title` picks the JSON keys, `--sort -updated` sorts (a leading `-` is descending), `--limit N`, and `--format jsonl` prints one object per line. `@name` runs a saved view from `.trk/config.toml` (`@mine`, `@agents`, `@open`, `@waiting`).

Sort keys: `id`, `state`, `title`, `priority`, `type`, `created`, `updated`, `closed`, `claimed-by`, `lease`, `next-check`. Without `--sort`, `trk ready` orders by priority then oldest first; `trk list` by most recently updated.

## The issue object

`trk show <ID> --json` prints:

| Key | Content |
| --- | --- |
| `id`, `title`, `description`, `priority`, `type` | the authored issue |
| `state`, `close_reason` | where it is |
| `claim` | the active claim: `actor`, `claimed_at`, `lease_until`, `worktree`, `agent`, `terminal` |
| `waiting` | while waiting: `reason`, `check`, `interval` (seconds), `next_check_at`, `last_check_at`, `last_check_exit`, `cwd` |
| `deps` | `blocked_by` and `blocks`, each a list of `{id, title, state, priority}` |
| `derived` | `ready`, `blocked_by` (open blockers), `due`, `progress` (`{total, closed}` blockers), `unblocks_higher_priority` |
| `notes`, `events`, `claims` | with `--with notes,events,claims` (notes are included by default) |

## Commands

| Command | Purpose |
| --- | --- |
| `trk list [QUERY]`, `trk ready [QUERY]` | Query issues |
| `trk show <ID>... [--with notes,events,deps,claims]` | Full issues with derived state |
| `trk create [TITLE] [-d TEXT] [-p P0..P3] [--type T] [--blocked-by ID]... [--blocks ID]... [--dry-run]` | Create; type defaults to `code-change` |
| `trk edit <ID> [-t TITLE] [-d TEXT] [-p P0..P3] [--type T] [--dry-run]` | Change the authored fields; type cannot be empty |
| `trk claim <ID> [--lease 30m] [--worktree PATH] [--steal]` | open → claimed |
| `trk unclaim <ID> [-m NOTE] [--force]` | claimed → open |
| `trk renew <ID> [--lease 30m]` | Extend a lease |
| `trk close <ID> [-r done or abandoned] [-m NOTE] [--force] [--dry-run]` | Close from open, claimed or waiting |
| `trk wait <ID> [-c CHECK] [--every 5m] [-r REASON] [--force] [--dry-run]` | claimed → waiting; on a waiting issue, replace its check |
| `trk reopen <ID> [-m NOTE]` | waiting or closed → open |
| `trk check [ID] [--all] [--watch [PAUSE]] [--dry-run]` | Run waiting checks, one at a time |
| `trk note <ID> [TEXT or -] [--amends SEQ] [--to-agent]` | Append a note |
| `trk dep add or rm <BLOCKER> <BLOCKED>` | Blocker edges |
| `trk log <ID>` | Timeline |
| `trk run [ID] [--agent A] [--harness H] [--dry-run or --print]` | Start an agent session; without ID, first ready issue by priority then age; agent override is one-off |
| `trk runs`, `trk send <ID> TEXT`, `trk reap` | List active/waiting sessions by state, steer the active or waiting terminal, release only ended active claims |
| `trk agent list`, `trk agent preview <agent> [--issue ID]`, `trk agent check` | Inspect agents |
| `trk install [--force] [--prefix P]` | Set trk up in a project: `.trk/` (database, config) and this skill; safe to repeat |
| `trk whoami`, `trk backup <PATH>` | Environment and backup |

`-d` and `-c` take text, `@file`, or `@-` for stdin.

`trk attach <ID>` interactively takes over the same terminal as `send`; suggest it to the user rather than running it. Active claims take precedence even without a terminal. With no active claim, `send`, `attach` and inbox delivery use the latest claim only if released with reason `waiting` and the issue is still waiting. That session may add notes and report follow-ups, but cannot change lifecycle state. Once the issue closes, reopens or is claimed again, the old session loses this access.

`trk check --json` results: `passed` closes, `retry` keeps waiting, `woken` restores the live waiting session's claim and sends a repair prompt, `failed` reopens (no live session or failed delivery), and `skipped` means the issue changed during the check or wake delivery. `woken` and `failed` include output, also saved in a note. Liveness requires the original MCP server on this host, a terminal (present in Herdr when enabled), and `[launcher] send`. trk only interprets exit codes; timeout is failure, exit 75 is retry.

## Errors

| Exit | Meaning | Codes |
| --- | --- | --- |
| 1 | Internal error | `internal`, `db_corrupt`, `newer_database` |
| 2 | Usage | `bad_args`, `bad_query` |
| 3 | Not found | `not_found`, `ambiguous_id` (see `candidates`), `not_installed` |
| 4 | Validation | `invalid_field` (see `allowed`), `invalid_state` (see `state`), `cycle`, `config_invalid` |
| 5 | Conflict | `claimed_by_other` (see `claimant`), `not_claimant`, `blocked` (see `blocked_by`), `busy`, `not_permitted` |

Every error carries a `message` and usually a `hint`, often the exact command to run next. A check that fails is not an error of `trk check`: it exits 0 and reports the result.
