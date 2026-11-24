# CLI JSON and exit protocol

Use `--json` for scripts. It is shorthand for `--format json` and takes precedence
if both options are present. `--format pretty|json|jsonl` can also be set through
`TRK_FORMAT`; without either option, stdout being a terminal selects pretty,
otherwise JSON. JSON and JSONL do not contain ANSI styling. Pretty output is a
view of the same command result, not a separate query.

This chapter covers ordinary CLI command results. The [MCP server](mcp.md) owns
its stdio transport and does not use this envelope. `trk check --watch` prints
human-readable progress lines as checks finish, even with a JSON format selected;
use a single `trk check --json` pass for machine-readable results.

## Envelope and version

A successful `--json` command writes one JSON document to stdout:

```json
{
  "ok": true,
  "schema_version": 4,
  "data": [
    {"id": "orch-4kq", "state": "open", "type": "code-change"}
  ]
}
```

This example could be produced by
`trk list --fields id,state,type --json`. The current output schema version is
**4**, independent of the installed binary's version or the SQLite migration
version. Within an output schema version, keys are only added; removing or
renaming keys requires a version bump. Version 3 replaced the old issue `agent`
field with required `type`; version 4 removed issue `project` and `repo` because a
database contains one project. Consumers should tolerate additional keys, test
`ok`, and handle optional fields rather than infer success from the data shape.

The type of `data` depends on the command:

| Command/result | JSON `data` |
| --- | --- |
| `list`, `ready` | Array of issue objects, or projected objects when `--fields` is set. Empty results are `[]`. |
| `show <ID>` | One issue object. |
| `show <ID> <ID>...` | Array of issue objects. |
| `log <ID>` | Array of timeline events, including notes, in sequence order. |
| `check` (one pass) | Object with `busy`, `results`, and optional `holder` or `gc`; see below. |
| Other commands | The command's typed object/array. Even commands silent in pretty mode can return JSON data. |

`--fields` changes object keys as well as pretty table columns. Saved views can
also supply fields. For example, `--fields id,claimed-by,ready` emits exactly
those keys, using `null` where the selected value is unavailable. Field aliases
include `status` → `state`, `reason` → `close_reason`, `created`/`updated`/`closed`
→ their `_at` timestamps, `claimed-by` → `claim.actor`, `lease` →
`claim.lease_until`, `ready`/`due`/`blocked-by` → the corresponding `derived`
fields, and `check`/`next-check` → the corresponding waiting fields. See
[queries](queries.md) for filtering and [configuration](configuration.md) for views.

## JSONL

`--format jsonl` writes compact, unwrapped JSON values rather than the versioned
success envelope:

- `list` and `ready`: one issue (or `--fields` projection) per line. No matches
  produce no stdout.
- `log`: one timeline event per line. Each event has `seq`, `issue_id`, `actor`,
  `at`, `op` and `payload`; notes are events with `op: "note"` and their text in
  `payload.body`.
- Other results: one line containing the command's whole `data` value. In
  particular, `show A B --format jsonl` emits **one array**, not one issue per line.

JSONL success values have no `ok` or `schema_version` wrapper. On failure the
renderer instead writes the same **pretty-printed, potentially multiline error
envelope** as JSON mode. Check the process exit status before treating stdout as
a stream of successful rows.

```sh
trk list 'state!=closed' --fields id,state,type --format jsonl
trk log orch-4kq --format jsonl
trk show orch-4kq --json | jq '.data.derived'
```

## Issue objects

The normal issue representation contains:

| Fields | Meaning |
| --- | --- |
| `id`, `title`, `description`, `priority`, `type` | Authored identity and content; priorities are `P0`–`P3`, and type is required. |
| `state`, `close_reason` | State is `open`, `claimed`, `waiting` or `closed`; close reason is `done`, `abandoned` or null. |
| `waiting` | Null unless waiting. Otherwise an object with `reason`, `check`, `interval` (seconds), `next_check_at`, `last_check_at`, `last_check_exit` and `cwd`; individual values can be null. A human wait has no check. |
| `claim` | Active claim or null. Contains `actor`, `on_behalf_of`, `claimed_at`, `lease_until`, `worktree`, `host`, `agent`, `terminal`, and optional pinned `launcher`. A waiting session's released claim is not an active claim. |
| `deps` | When requested, `blocked_by` and `blocks` arrays of `{id, title, state, priority}`. List/ready and ordinary show include dependencies. |
| `derived` | Computed read-time state: `ready`, `blocked_by` (unclosed blocker IDs), `due`, and `unblocks_higher_priority` (objects with `id` and `priority`). When blockers exist, also `progress: {total, closed}`, including closed blockers in the total. |
| `created_by`, `created_at`, `updated_at`, `closed_at` | Authorship and timestamps; `closed_at` is null before closure. |

Additional fields depend on context:

- `links` appears only when nonempty; each entry has `url` and an optional `label`.
- `wake_count` appears when positive and the issue is waiting or claimed.
- `show --with notes,events,claims` adds the requested arrays (`--with all` adds
  all three). Notes have `seq`, `actor`, `at`, `body`, `amends`, `to_agent`.
  Events use the timeline shape above. Historical claims add `released_at` and
  `release_reason` to the claim shape.
- `show` adds `routing` for a known issue type, including the resolved worktree
  policy. MCP `get_task` includes notes, dependencies and its session's routing.

Timestamps are RFC 3339 UTC with second precision, such as
`2026-11-21T18:02:11Z`; relative times are a pretty-output presentation only.
Derived fields are not stored. See [sessions](sessions.md) for claim metadata and
[waiting and checks](waiting-and-checks.md) for waiting state.

## Check results

A normal single `trk check --json` pass returns data shaped like:

```json
{
  "busy": false,
  "results": [
    {
      "issue": "orch-4kq",
      "title": "Add claim leases",
      "result": "retry",
      "exit": 75,
      "timed_out": false,
      "duration_ms": 12,
      "state": "waiting",
      "next_check_at": "2026-11-21T18:07:11Z"
    }
  ]
}
```

| `result` | Outcome |
| --- | --- |
| `passed` | The check exited 0 without timing out; the issue closed as done. |
| `retry` | The check exited 75 without timing out; the issue stays waiting and another check is scheduled. |
| `failed` | The check failed, timed out or could not run, and the issue reopened rather than waking its previous session. |
| `woken` | A failed check restored the waiting session's claim and delivered the wake message. |
| `skipped` | Concurrent changes made the result inapplicable, so trk did not apply it; this can also occur during wake recovery. |

Each normal result includes `issue`, `title`, `result`, `exit`, `timed_out`,
`duration_ms`, `state` and `next_check_at`. `exit` can be null if the command was
killed or could not start. `failed` and `woken` also include `output` (combined
stdout and stderr with trailing whitespace trimmed). `state` and `next_check_at`
are the issue's values read after processing, not a promise that another actor
will not subsequently change them.

`--dry-run` instead returns result entries with `result: "would_run"`, `issue`,
`title`, `check`, `reason`, `cwd` and `problem`; it does not run commands.
A busy queue is a successful result with `busy: true`, `holder` and `results`, not
a `busy` error. Optional `gc` describes the advisory cleanup sweep and its errors.
A failed check is also a **result**, not a command error: a completed pass exits
0 even if it reports `failed`, `woken`, or cleanup errors. Errors such as an
invalid explicit issue/check request still use the error protocol below.

## Errors and process exits

After CLI argument parsing, command errors in JSON and JSONL modes are written to
**stdout** with the same schema version:

```json
{
  "ok": false,
  "schema_version": 4,
  "error": {
    "code": "blocked",
    "message": "orch-9xa is blocked by orch-4kq",
    "issue": "orch-9xa",
    "blocked_by": ["orch-4kq"],
    "hint": "close its blockers first, or remove the edge with `trk dep rm`"
  }
}
```

`code` and `message` identify the error. `issue` and `hint` are optional, and
code-specific detail keys (for example `blocked_by`, `claimant`, `candidates`,
`allowed` or `problems`) are merged into the error object. Do not assume that
every failure supplies a hint or issue ID.

| Exit | Meaning | Error codes |
| --- | --- | --- |
| 0 | Successful command/result | No error code. |
| 1 | Internal or compatibility error | `internal`, `db_corrupt`, `newer_database` |
| 2 | Usage error | `bad_args`, `bad_query` |
| 3 | Not found | `not_found`, `ambiguous_id`, `not_installed` |
| 4 | Validation failure | `invalid_field`, `invalid_state`, `cycle`, `config_invalid` |
| 5 | Conflict or ownership restriction | `claimed_by_other`, `not_claimant`, `blocked`, `busy`, `not_permitted` |

Argument-parser errors occur before the renderer exists: unknown options,
missing required CLI arguments and invalid option values use the parser's
stderr text and exit 2, even with `--json`. Help/version output is also not a JSON
envelope. Pretty-mode command errors go to stderr. Warnings and diagnostics can
appear on stderr separately from a valid stdout JSON document; keep the streams
separate. The MCP server has its own [tool error behavior](mcp.md#tool-results-and-errors).

Implementation: [renderer](https://github.com/rodrigosetti/trk/blob/main/crates/trk-cli/src/render.rs),
[read models](https://github.com/rodrigosetti/trk/blob/main/crates/trk-core/src/view.rs),
[check runner](https://github.com/rodrigosetti/trk/blob/main/crates/trk-cli/src/check.rs),
and [error codes](https://github.com/rodrigosetti/trk/blob/main/crates/trk-core/src/error.rs).
