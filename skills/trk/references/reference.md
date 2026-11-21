# trk reference

## Query language

`trk list QUERY` and `trk ready QUERY` take one string of terms joined by AND.

```
name=value  name!=value  name<value  name<=value  name>value  name>=value
name~value             text search for `text`; glob or substring otherwise
name in (a,b,c)
NOT term  or  !term
ready  blocked  gated  stale-lease     flags
```

Values: words, `"quoted strings"`, `me` (the acting actor), `.` (for `project`: this repo's project), relative times such as `-7d`, `-12h` and `-30m`, dates (`2026-11-01`), and globs with `*` (`claimed-by=agent:*`).

Names: `id`, `type`, `status`, `category` (open, active or closed), `resolution`, `project`, `owner`, `priority` (P0–P3), `claimed-by`, `created-by`, `title`, `created`, `updated`, `closed`, `tag`, `text` (full text over title, context, outcome and notes), `blocks=<ID>`, `blocked-by=<ID>`, `dep:<kind>=<ID>`. Any other name is a field, such as `environment=prod` or `field.environment=prod`.

```bash
trk list 'type=operation environment=prod category!=closed' --json
trk list 'type in (decision,approval) closed>-7d resolution=decided' --json
trk list 'claimed-by=me' --json
trk list 'blocks=orch-m1 category!=closed' --json      # what remains for a milestone
trk list 'text~"feature flag"' --json
trk list gated --json
```

Options: `--fields id,type,status,title` picks the JSON keys, `--sort -updated` sorts (a leading `-` is descending), `--limit N`, and `--format jsonl` prints one object per line. `@name` runs a saved view from config.toml (`@mine`, `@agents`, `@open`).
`trk ready` defaults to priority P0→P3, then oldest-created, then ID. `--sort` overrides that order; for example `trk ready --sort created` or `trk list 'priority=P1' --sort -updated`. Priorities are project-relative, not a shared cross-project urgency scale.


## Setting fields

The syntax is the same for `create`, `edit`, `claim`, `move`, `close` and `run`.

```
-f name=value           set; the value is parsed by the field's kind
-f tags+=infra          append to a list
-f tags-=infra          remove from a list
-f name=@file.md        value from a file
-f name=@-              value from stdin
-f name=                clear (refused if the current status requires it)
--from issue.json       a whole object in the shape `show --json` prints (`-` for stdin)
```

Lists take a comma-separated value (`-f options=SQLite,Dolt`) or a JSON array (`-f options='["A, B","C"]'`). Links are `kind=url note`, for example `-f links+="pr=https://… the PR"`. Kinds: dates are `YYYY-MM-DD`; datetimes are RFC 3339 or `now`; actors are `human:<email>`, `agent:<harness>:<id>` or `me`; enums accept their listed values only.

## Commands

| Command | Purpose |
| --- | --- |
| `trk list [QUERY]`, `trk ready [QUERY]` | Query issues |
| `trk show <ID>... [--with notes,events,deps,claims]` | Full issues with derived state |
| `trk create <TYPE> -t TITLE -f k=v... [--dep KIND:ID] [--dry-run]` | Create |
| `trk edit <ID> -f k=v... [--dry-run]` | Change fields |
| `trk claim <ID> [-f k=v] [--lease 30m] [--worktree PATH] [--steal]` | Claim; enters the first intermediate status |
| `trk unclaim <ID> [-m NOTE] [--force]` | Release the claim and return to open |
| `trk renew <ID> [--lease 30m]` | Extend a lease |
| `trk move <ID> <STATUS> [-f k=v] [--force] [--dry-run]` | Move between intermediate statuses |
| `trk close <ID> -r RESOLUTION [-f k=v] [--force] [--dry-run]` | Close |
| `trk note <ID> [TEXT or -] [--amends SEQ] [--to-agent]` | Append a note |
| `trk dep add or rm <SRC> <KIND> <DST>` | Edges |
| `trk log <ID>` | Timeline |
| `trk run <ID> [--agent A] [--harness H] [-f k=v] [--dry-run or --print]` | Start an agent session |
| `trk runs`, `trk send <ID> TEXT`, `trk reap` | Watch, steer and clean up sessions |
| `trk agent list`, `trk agent preview <agent> [--issue ID]` | Inspect agents |
| `trk schema [TYPE]`, `trk schema check`, `trk schema plan`, `trk schema apply`, `trk schema graph TYPE` | Configuration |
| `trk project add or list`, `trk repo add or list` | Projects and repositories |
| `trk whoami`, `trk backup <PATH>` | Environment and backup |

## Errors

| Exit | Meaning | Codes |
| --- | --- | --- |
| 1 | Internal error | `internal`, `db_corrupt` |
| 2 | Usage | `bad_args`, `bad_query` |
| 3 | Not found | `not_found`, `ambiguous_id` (see `candidates`), `unknown_repo` |
| 4 | Validation | `requirements_unmet` (see `missing_fields`, `missing_edges`), `invalid_field`, `invalid_transition`, `cycle`, `schema_invalid` |
| 5 | Conflict | `claimed_by_other`, `not_claimant`, `gate_unmet` (see `unmet_gates`), `busy`, `not_permitted` |

Every error carries a `hint`, often the exact command to run next.
