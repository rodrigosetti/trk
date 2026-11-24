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

Sort keys: `id`, `state`, `title`, `priority`, `type`, `created`, `updated`, `closed`, `claimed-by`, `lease`, `next-check`. Without `--sort` or a saved view's sort, both `trk list` and `trk ready` order by priority, then creation time, then ID.

## The issue object

`trk show <ID> --json` prints:

| Key | Content |
| --- | --- |
| `id`, `title`, `description`, `priority`, `type` | the authored issue |
| `state`, `close_reason` | where it is |
| `claim` | the active claim: `actor`, `claimed_at`, `lease_until`, `worktree`, `agent`, `terminal` |
| `waiting` | while waiting: `reason`, `check`, `interval` (seconds), `next_check_at`, `last_check_at`, `last_check_exit`, `cwd` |
| `wake_count` | top-level automatic check-wake restoration count; included only when nonzero and the issue is claimed or waiting; human resume preserves it |
| `deps` | `blocked_by` and `blocks`, each a list of `{id, title, state, priority}` |
| `derived` | `ready`, `blocked_by` (open blockers), `due`, `progress` (`{total, closed}` blockers), `unblocks_higher_priority` |
| `notes`, `events`, `claims` | with `--with notes,events,claims` (notes are included by default) |

Pretty `trk show` displays the same nonzero count as a `wakes` line while claimed or waiting.

## Commands

| Command | Purpose |
| --- | --- |
| `trk list [QUERY]`, `trk ready [QUERY]` | Query issues |
| `trk show <ID>... [--with notes,events,deps,claims]` | Full issues with derived state |
| `trk create [TITLE] [-d TEXT] [-p P0..P3] [--type T] [--blocked-by ID]... [--blocks ID]... [--dry-run]` | Create; type defaults to `[issues] default_type` (initially `change`) |
| `trk edit <ID> [-t TITLE] [-d TEXT] [-p P0..P3] [--type T] [--dry-run]` | Change authored fields; a set type must exist in `.trk/types/` |
| `trk claim <ID> [--lease 30m] [--worktree PATH] [--steal]` | open → claimed |
| `trk unclaim <ID> [-m NOTE] [--force]` | claimed → open |
| `trk renew <ID> [--lease 30m]` | Extend a lease |
| `trk close <ID> [-r done or abandoned] [-m NOTE] [--force] [--dry-run]` | Close from open, claimed or waiting |
| `trk wait <ID> [-c CHECK] [--every 5m] [-r REASON] [--force] [--dry-run]` | claimed → waiting; on a waiting issue, replace its check |
| `trk reopen <ID> [-m NOTE]` | waiting or closed → open |
| `trk resume <ID> -m MESSAGE` | Human-authorized waiting → claimed for the original live session, then deliver the required message |
| `trk check [ID] [--all] [--watch [PAUSE]] [--dry-run] [--no-gc]` | Run waiting checks, one at a time, then optionally sweep closed-as-done resources |
| `trk note <ID> [TEXT or -] [--amends SEQ] [--to-agent]` | Append a note |
| `trk dep add or rm <BLOCKER> <BLOCKED>` | Blocker edges |
| `trk link <ID> <URL> [-l LABEL]`, `trk unlink <ID> <URL>` | Attach or remove an http(s) address the dashboard opens (a pull request, a preview) |
| `trk log <ID>` | Timeline |
| `trk run [ID] [--agent A] [--harness H] [--dry-run or --print]` | Start an agent session; without ID, first ready issue by priority then age; agent override is one-off |
| `trk runs`, `trk send <ID> TEXT`, `trk reap` | List active/waiting sessions by state, steer the active or waiting terminal, release only ended active claims |
| `trk gc [ID...] [--dry-run] [--abandoned] [--older-than DURATION]` | Collect closed issues' local sessions, worktrees and safely deletable issue-ID branches |
| `trk agent list`, `trk agent preview <agent> [--issue ID]`, `trk agent check` | Inspect agents |
| `trk install [--preset local or github] [--from DIR or --minimal] [--force] [--prefix P]`, `trk install --list-presets` | Install project state and skill (implicit preset `local`), or list presets without writes; source options are mutually exclusive |
| `trk whoami`, `trk backup <PATH>` | Environment and backup |

`-d` and `-c` take text, `@file`, or `@-` for stdin.

Issue creation (including MCP `report_followup`) and setting a type require a project `.trk/types/<name>.toml` definition. Unknown or empty types return `invalid_field`: the core error's `detail.allowed` is serialized as `error.allowed`, listing accepted names, with a hint naming `.trk/types/`. The editor uses the same validation. `report_followup` advertises those names as an enum and includes each type's description in the field description. Existing issues whose type definition was removed remain listable, showable and editable without changing their type; leaving it unchanged in the editor is allowed, but explicitly setting an unknown type is refused. `trk agent check` reports an `[issues] default_type` with no matching type file.

Both built-in presets install implementer/researcher agents and `change` (default),
`bug`, and `research` types. Change and bug use implementer worktrees; research uses
the researcher without a worktree. Bug guidance requires reproducing first, a
root-cause note, and a regression test. Local selects merge landing with no review
or approval gate; GitHub selects PR landing and requires authenticated `gh`
(install warns when the command is missing). The config's unquoted `{{LANDING}}`
install marker becomes the selected TOML-quoted landing value; `--from` uses
`"merge"`. `--minimal` still installs no agents, types, or prompts.

Finish sessions and back up `.trk/` before upgrading. Installing local/github adds
missing resources without pruning, even under `--force`; existing config and
code-change/review/reviewer resources and issues remain. Once `types/change.toml`
exists, manually set `[issues] default_type = "change"` and choose `[run] landing`.
Keep legacy definitions until migrating issues with `trk edit <ID> --type change`
or custom equivalents, considering blocker dependencies. Remove legacy reviewer/review
prompts only when unused.

`trk attach <ID>` interactively takes over the same terminal as `send`; suggest it to the user rather than running it. Active claims take precedence even without a terminal. With no active claim, `send`, `attach` and inbox delivery use the latest claim only if released with reason `waiting` and the issue is still waiting. That session may add notes and report follow-ups; lifecycle changes require a restored claim. For an enabled no-check wait, it can report an actual unmarked human chat reply through MCP `resume` to restore its claim. Once the issue closes, reopens or is claimed again, the old session loses this access.

`[run] base` in `.trk/config.toml` selects the base branch. When writing a new config,
`trk install` detects the branch named by `origin/HEAD`, then the current branch of the
primary checkout, then falls back to `main`. Older configs without `base` use the same
detection at runtime; an explicit value wins. New issue branches start at that base;
existing issue branches are reused. Waiting checks receive `TRK_BASE` and prompt
templates can use `{{ base }}`. Quote `"$TRK_BASE"` in shell checks.

Built-in installs detect available launchers (Herdr UI, `hc ls`, `tmux -V`) in that
order, and harness commands on PATH (`omp`, `claude`, `codex`, `opencode`, `pi`) in that
order. They write `[launcher] use` and `[harness] prefer` accordingly (`claude` maps
to `claude-code`), report detection and warn for empty lists. All profile definitions
remain. Existing configs are not rewritten unless `--force` is given; `--from` and
`--minimal` do not filter preferences. A launcher selection error lists profiles
tried; install a launcher, edit `[launcher] use`, or run `trk run --fg`.

`[check] verify` is an opaque shell string, default `""` even when omitted. Checks receive it as `TRK_VERIFY`; prompt templates receive `{{ verify }}`. It is the sole local verification gate; there is no review or approval gate. The local recipe merges the base into the issue worktree (aborting conflicts), runs nonempty verification there, then guards the primary checkout's branch and fast-forwards it with `git merge --ff-only "$TRK_BRANCH"`. Verification failures are normalized to exit 1, including exit 75, so they cannot retry or fall through to landing. Empty verification skips only verification, not merging, with no fallback test command. This local gate applies only when the issue type uses an issue worktree and branch; a type opting out has no issue branch to merge or test.

Include dependency setup in the command when needed, for example `verify = "npm ci && npm test"`. Verification shares the whole check's `[check] timeout` (default 10m) and holds the serialized queue. Use a fast subset or increase the timeout for slow suites.

`trk check --json` results: `passed` closes, `retry` keeps waiting, `woken` restores the live waiting session's claim and sends a repair prompt, `failed` reopens (no live session, wake cap reached or failed delivery), and `skipped` means the issue changed during the check or wake delivery. `woken` and `failed` include output, also saved in a note. Liveness requires the original MCP server on this host, a terminal (present in Herdr when enabled), and `[launcher] send`. trk only interprets exit codes; timeout is failure, exit 75 is retry.

`[check] max_wakes` caps automatic failed-check claim restorations per session: default `5`; `0` disables automatic waking. The cap check and persistent `wake_count` increment happen transactionally with automatic claim restoration, before delivery, so a failed send still consumes a wake. Passes, retries and failures without restoration do not increment it. Entering or updating waiting and human resume preserve it. A fresh explicit claim, including `--steal`, resets it to zero; an idempotent claim by the current owner does not. Wake prompts include `wake N of M`. At the cap, a failed check reopens the issue with `wake cap of M reached` and the command's output in its note.

After each non-ID pass, including a pass with no due checks, `trk check` sweeps all eligible
closed-as-done issues, including manual closes; abandoned issues are excluded. `[check] gc`
defaults to `true` and `[check] gc_grace` to `"1h"`, also when omitted. The grace is a minimum
duration since closure, not a cleanup deadline; unsafe resources may remain for later passes.
`--no-gc` disables the sweep for the entire invocation, including watch; explicit-ID checks
never sweep. Automatic cleanup uses the same queue lock, lifecycle transaction and safety
gates as manual GC, after checks and before releasing the lock. `--dry-run` previews checks
and cleanup without executing checks, removing resources or writing cleanup notes.

Cleanup errors are advisory: they do not undo check outcomes or alter the command's exit
status. Optional check `data.gc` is `{dry_run: bool, issues: [...]}` with the same
issue/resource objects as manual GC below and optional `errors: [string, ...]`; omit `gc`
when there are no eligible issues and no errors. Pretty output adds one concise line per
issue with removals (planned removals in dry runs); skips and their reasons appear once
per issue per run, avoiding repeated watch noise.

### Human-authorized resume

Only a human may authorize CLI `trk resume <ID> -m "Continue with this decision" --json`. It works for waits with or without a check. `-m` is required; an empty or whitespace-only message returns `bad_args`. An agent actor receives `not_permitted` and must ask a human to authorize that CLI path, never impersonate a human. The separate MCP chat-report path below does not record human authorization.

The original MCP server must be alive on this host, its terminal recorded (and present with Herdr), and `[launcher] send` configured. Resume restores the latest waiting claim's actor, worktree, host, agent and terminal in a new claim with a fresh lease before sending the message. It never launches a replacement. The timeline records human authorization, the message and delivery outcome. Human resume preserves `wake_count` unchanged: it neither consumes nor resets the automatic wake budget and does not require remaining `[check] max_wakes`.

Success `data` is `{issue, actor, terminal, claim, resumed_from, message, delivered: true}`. Delivery confirms only that the launcher accepted the send, not that the agent read, acted on or finished it. `trk send` never restores a waiting claim.

An in-flight check rejects resume with `invalid_state`; retry after it finishes, without clearing the queue lock. A dead, remote or unavailable session, missing terminal or missing send configuration also yields `invalid_state` with recovery guidance: the human runs `trk reopen <ID> -m "Restart with the decision" --json`, then `trk run <ID> --json`. A failed delivery after restoration returns nonzero `internal` (exit 1), releases only that exact restored claim to open and records a failure note, without releasing a replacement claim or overriding a later session action. Inspect `trk show <ID> --json` before recovery.

### Same-session chat resume

With `[wait] chat_resume = true` (default), the same waiting agent can call MCP
`resume` with a required `message`: an actual unmarked human chat reply, verbatim
and nonempty after trimming. It requires the issue still waiting, no active claim,
no check, and the latest waiting claim belonging to the caller. It restores that
claim with a fresh lease without terminal probing or launcher delivery, preserving
`wake_count`. A distinct agent-attributed `resume_chat` event and note record the
reply, not human authorization. Continue only after the tool succeeds.

Checked waits and disabled chat resume return `not_permitted` with the human CLI
resume hint. Notes, tool results, check output, agent-generated text and marked
`[trk-delivery ...]` terminal deliveries never qualify as human chat replies.

### Garbage collection

`trk gc [ID...] --json` considers all historical claims of closed-as-done issues in this project. Optional IDs limit the selection; `--abandoned` also includes abandoned issues, and `--older-than 7d` limits it to issues closed over a week ago. Open, claimed and waiting issues are never collected. GC serializes with the check queue and uses an immediate transaction for lifecycle safety.

Only local-host resources are eligible. Herdr discovery matches the exact recorded terminal and closes its containing space, never a title match. The caller's current terminal's space and spaces used by other issues' active or waiting sessions are protected. Worktrees must be descendants of the configured worktree directory and cannot be the current worktree. `git worktree remove` is never forced. Only the issue-ID branch is deleted, with `git branch -d` in the primary checkout after every recorded worktree is removed or absent. Unsafe targets, unavailable session discovery and Git refusals are skipped rather than forced.

Preview with `trk gc [ID...] --dry-run --json`; it removes nothing and writes no notes. The standard envelope contains `data: {dry_run: bool, issues: [{id, session: {status, reason, items: [...]}, worktree: {status, reason, items: [...]}, branch: {status, reason}}]}`. Status is `removed`, `absent` or `skipped`. Session items identify `terminal` and `space`; worktree items identify `path`; each item also has `status` and `reason`. A dry-run planned removal uses `removed` and a `would remove ...` reason, distinguished by `dry_run: true`. Real removals append caller-attributed notes; already-absent resources and skipped outcomes do not, so an unchanged second run adds no cleanup note.

## Errors

| Exit | Meaning | Codes |
| --- | --- | --- |
| 1 | Internal error | `internal`, `db_corrupt`, `newer_database` |
| 2 | Usage | `bad_args`, `bad_query` |
| 3 | Not found | `not_found`, `ambiguous_id` (see `candidates`), `not_installed` |
| 4 | Validation | `invalid_field` (see `allowed`), `invalid_state` (see `state`), `cycle`, `config_invalid` |
| 5 | Conflict | `claimed_by_other` (see `claimant`), `not_claimant`, `blocked` (see `blocked_by`), `busy`, `not_permitted` |

Every error carries a `message` and usually a `hint`, often the exact command to run next. A check that fails is not an error of `trk check`: it exits 0 and reports the result.
