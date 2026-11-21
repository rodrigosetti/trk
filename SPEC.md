# trk — Local Issue Tracker Design Spec

Nov 21, 2026 · @Rodrigo Setti

## Overview

`trk` is a local issue tracker and agent launcher in one Rust binary. It keeps the work of every repository in one SQLite database in the user's home directory. Humans use the CLI. Agents work through a per-issue MCP server. `trk run <ID>` starts an agent session on one issue in a persistent terminal (herdr-core, `hc`), with focused instructions and only that issue's tools.

There is one kind of issue and one state machine. An agent session ends in one of two ways: the issue is **closed**, or it is **waiting** for something outside the session, described by a shell command that `trk check` runs until it passes, fails, or says "not yet".

**Terms.** An *agent* is instructions plus tools. A *harness* is the program that runs an agent: Claude Code, pi, omp, Codex, OpenCode. A *launcher* is where the harness runs: `hc`. A *check* is the shell command of a waiting issue.

**Goals**

- One database for work across many repositories, with blockers allowed between projects.
- A model small enough to hold in the head: four states, three authored fields, one dependency kind.
- First-class claims by humans (`human:<email>`) and agent sessions (`agent:<harness>:<id>`), with leases.
- A complete, append-only history: every change and every note is an attributed, timestamped event.
- "Done" means landed. Work that is finished but not merged, reviewed or decided is waiting, not closed, and still blocks what depends on it.
- Failures return as work: a failed check reopens its issue with the evidence in a note.
- Agents reach trk only through MCP, never through CLI skills.
- `trk run` starts a focused session for one issue, with minimal coupling to `hc`: three command templates.

**Non-goals**

- Issue types, custom fields, configurable workflows, required evidence on close. They existed in an earlier design and were removed: a check command expresses what they tried to enforce.
- Issue hierarchies. Grouping is done with `blocks` edges.
- Sync between machines, multiple users over a network, or a web UI.
- Storing state that can be read from elsewhere at any time (CI status, PR review state). A check reads it when it runs.
- Sandboxing checks. A check runs as the user (see Trust).

## The model

### Issue

| Field | Meaning |
| --- | --- |
| `title` | required |
| `description` | markdown: what is needed, why, how to tell it is done |
| notes | an append-only log, stored as events |
| `priority` | `P0` urgent, `P1` high, `P2` normal (default), `P3` low; orders `trk ready` only |
| `agent` | the agent definition `trk run` uses; empty means the default agent |
| `repo` | the repository the work happens in; filled from the directory the issue is created in |

Everything else on an issue is state the tool owns: `state`, `close_reason`, the waiting fields, timestamps and `created_by`.

### States

```
          claim                 close
  open ──────────▶ claimed ──────────────▶ closed
   ▲  ◀──────────     │                      ▲
   │    unclaim       │ wait                 │ check exits 0,
   │    lease expiry  ▼                      │ or a human closes
   │               waiting ──────────────────┘
   └──────────────────┘
     check exits other than 0 or 75,
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
| waiting | open | the check exits otherwise or times out, or a human reopens |
| open | closed | a human closes without claiming (cancel) |
| closed | open | a human reopens |

An issue is **ready** when it is open and every issue that blocks it is closed. `trk ready` orders ready issues by priority, then creation time, then id. Priority ranks work that is already eligible; it does not bypass blockers.

### Blockers

`A blocks B` is the only edge: B is not ready until A is closed. Edges are acyclic. A waiting issue is not closed, so it still blocks: B becomes ready when A's work has landed, not when A's session stopped.

Other relationships are written in descriptions and notes. A review, an approval, a decision or a milestone is an issue whose description says so:

| Need | Expressed as |
| --- | --- |
| B needs A first | `A blocks B` |
| Review before landing | a review issue (agent `reviewer`) that blocks the next step |
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
| `wait_cwd` | where the check runs: the worktree of the claim that started the wait |

The check's exit code is the whole protocol:

| Exit | Meaning | Effect |
| --- | --- | --- |
| 0 | pass | closed with reason `done` |
| 75 (`EX_TEMPFAIL`) | not yet | still waiting; due again after the interval |
| anything else | error | back to open; a note records the command, its directory and the last 4 KB of its output |
| killed by the timeout | error | the same, saying it timed out |

Error is the default. A command that is misspelled, whose credentials expired, or whose branch conflicts fails loudly and returns as work, instead of waiting forever. Only the one reserved code keeps an issue waiting, so "not yet" is always a deliberate answer.

A check may do the remaining work (merge the branch) or only look (is the PR merged?). It must be safe to run more than once: `trk check` may die between running a command and recording its result, and the next pass runs it again.

Entering waiting releases the claim with reason `waiting`. The session is over; nobody holds the issue. Running `wait` on an issue that is already waiting replaces its check, interval and reason, which is how a human repairs a bad check without reopening.

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
- A claim records where the work happens: `worktree` (absolute path) and `host`. `trk claim` fills them when it runs inside a checkout of the issue's repo.
- Only the claimant may close, wait or unclaim a claimed issue; a human's `--force` overrides and is recorded.
- A claim ends with one of: `closed`, `waiting`, `unclaimed`, `expired`, `stolen`.
- For agent actors, a claim also records the agent definition (`agent`) and the launcher terminal (`terminal`), both set by `trk run`.

**Enforcement.** Agent actors write only to issues they currently claim, plus creating follow-ups of such an issue; the rule is in `trk-core` and applies to the CLI as well. An agent with a shell can still run `trk` as a human, so hard isolation requires running the harness where the database is not reachable.

### Notes and history

Every mutation appends one or more rows to `event`, attributed to the actor, with a UTC timestamp. Notes are events with `op = 'note'`. A correction is a new note with `amends` pointing at the earlier one. Database triggers reject `UPDATE` and `DELETE` on `event`, and `DELETE` on `issue`.

A check that says "not yet" writes no event and does not touch `updated_at`: an issue that waits a week on a five-minute interval would otherwise bury its history under two thousand identical lines. A pass or a failure writes a `check` event followed by the `close` or `reopen` it caused.

## `trk check`

```
trk check [ID] [--all] [--watch [PAUSE]] [--dry-run]
```

One pass:

1. Read the queue: waiting issues with a check whose `next_check_at` has come, ordered by priority, then due time. `--all` ignores the due time; an `ID` runs that issue alone, due or not.
2. Take the lock (below). If another pass holds it, report `busy` and do nothing.
3. For each issue, in order, one at a time: run `sh -c <check>` and record the result.
4. Release the lock.

`--watch` repeats passes with a pause (default 30s) until interrupted. `trk check` is otherwise one-shot, so cron or launchd can drive it. `--dry-run` prints the queue, each command and its directory, and runs nothing.

**Running a check.** The command runs in `wait_cwd`; when the issue was claimed outside a checkout, in the repository's main checkout. A directory that no longer exists is a failure. The environment adds:

| Variable | Value |
| --- | --- |
| `TRK_ISSUE` | the issue id |
| `TRK_WORKTREE` | the directory the check runs in |
| `TRK_REPO` | the main checkout of the issue's repo, when one is known |
| `TRK_BRANCH` | the current branch of the worktree (`trk run` names it after the issue) |
| `TRK_DB`, `TRK_CONFIG` | so a check that calls `trk` reaches the same database |

`TRK_ACTOR` is removed. Stdin is `/dev/null`; stdout and stderr are captured together. The command runs in its own process group, and after `[check] timeout` (default 10m) the group gets `SIGTERM`, then `SIGKILL` two seconds later.

**Recording.** The result is written in one transaction that first re-reads the issue. If it is no longer waiting on exactly the command that ran (a human closed it, reopened it, or changed the check meanwhile), the result is dropped.

**The lock.** A single-row table holds the pid, host and start time of the running pass. It is taken in a write transaction, so two passes cannot both take it. A holder whose process no longer exists on this host is replaced. A holder on another host is never replaced automatically.

**Why serial.** Checks commonly merge a branch into the main checkout. Running them one at a time makes `trk check` a merge queue: the second merge sees the first, and a conflict fails that one issue and reopens it.

### Trust

A check is written by an agent and executed later, unattended, with the user's privileges and outside whatever sandbox the harness ran in. This grants agents nothing they did not have (they already had a shell), but it moves execution to a time when nobody is watching. So: `trk show` always prints the check in full, `trk check --dry-run` shows the queue first, each failure keeps its output, and `trk check` runs only when the user starts it.

## Agents and sessions

`trk run <ID>` starts an agent on the issue in a persistent `hc` terminal. The agent sees one issue. Agents reach trk only through MCP: their instructions never mention the `trk` CLI.

### Configuration pieces

| Piece | Defines |
| --- | --- |
| Agent | Who does the work: instructions, briefing, other MCP servers, harness preference, built-in tool level |
| Harness profile | How to run an agent in one harness: command, arguments, environment, files, how MCP servers and instructions are passed |
| Launcher | Where it runs: three command templates for `hc` |

```toml
# agents/implementer.toml
name        = "implementer"
description = "Implements an issue in its worktree and gets the change landed"

instructions = "prompts/implementer.instructions.md.j2"   # → MCP server instructions
briefing     = "prompts/briefing.md.j2"                   # → initial prompt
inbox        = true

# [trk_tools]
# include = ["get_task", "add_note", "close", "hand_back"]   # default: all six

[[mcp]]
name    = "github"
command = "github-mcp-server"
args    = ["stdio", "--toolsets", "pull_requests"]
env     = { GITHUB_TOKEN = "${env:GITHUB_TOKEN}" }

[harness]
prefer        = ["omp", "claude-code"]   # first installed wins; `trk run --harness` overrides
builtin_tools = "default"                # default | read-only | none
```

The agent for a run is the first of: `trk run --agent X`, the issue's `agent`, `[run] default_agent` (`implementer`). The seed has three agents: `implementer`, `reviewer` (read-only built-in tools) and `researcher`. They differ in instructions, not in tools. `trk agent check` validates agent files, templates and harness profiles.

### The trk MCP server

The harness starts `trk mcp --issue <ID> --actor <actor> --agent <name>` over stdio. No tool takes an issue id: the server serves exactly one issue. It renews the claim's lease while it runs, so a crashed session releases its claim when the lease expires.

| Tool | Arguments | Effect |
| --- | --- | --- |
| `get_task` | | The issue: description, state, notes, blockers. Also the resource `trk://task` |
| `add_note` | `body` | Appends a note |
| `report_followup` | `title`, `description`, `priority?`, `blocks_this?` | Creates an issue in the same project and repo; with `blocks_this`, it blocks the current one |
| `hand_back` | `note` | Unclaims with a note: claimed → open |
| `close` | `reason` (`done`, `abandoned`), `note` | claimed → closed |
| `wait` | `reason`, `check?`, `interval?` | claimed → waiting |

The set is fixed. An agent definition may narrow it with `trk_tools.include`. After `close`, `wait` or `hand_back` the claim is gone and every further call fails with `not_permitted`.

### Prompt channels

| Channel | Carries | Mechanism |
| --- | --- | --- |
| Briefing | The task: title, description, every note, blockers | Rendered from the agent's `briefing` template and passed as the harness's initial-prompt argument, or sent with the launcher's `send` template after launch |
| Instructions | Rules for the whole session, including the waiting protocol with examples | The MCP server's `instructions` and the tool descriptions. If a harness profile declares that its harness ignores server instructions, trk folds them into the briefing |
| Mid-session | Steering | `trk send <ID> "…"` via the launcher; `trk note <ID> --to-agent "…"` queues a message that the MCP server attaches to the agent's next trk tool result |

Notes are in the briefing because they are the issue's memory across sessions. An issue reopened by a failed check carries the check's output as its latest note, so the next session starts from the failure.

### `trk run`

```
trk run <ID> [--agent A] [--harness H] [--fg] [--print] [--dry-run]
```

1. Check the issue is ready, and resolve its agent and harness.
2. Create or reuse the worktree: `git worktree add <worktrees>/<ID> -b <ID>`. The branch is always named after the issue, so a later session finds earlier work.
3. Mint the actor `agent:<harness>:<uuid>`; the harness's own session id, once known, goes in `actor.meta`.
4. Claim the issue with the actor, agent, worktree and terminal name (Herdr binds its returned address after launch).
5. Render the launch spec: briefing, MCP server list (trk plus the agent's others), environment (`TRK_ACTOR`, for attribution only), files, argv.
6. Start in a new Herdr space when enabled and available, otherwise use the launcher's `start` template. `--print` emits the launch spec as JSON instead; `--dry-run` also skips the claim.

Related commands: `trk attach <ID>` and `trk send <ID>` (only when their launcher templates are set), `trk runs` (active claims with their terminals), `trk reap` (releases claims whose MCP server has gone, recording the exit as a note).

The worktree outlives the claim: a waiting issue's check runs in it. trk never removes worktrees.

### Launcher

```toml
[launcher]
start  = ["hc", "run", "--bg", "--no-parent", "--name", "{name}", "--cwd", "{cwd}", "--", "sh", "-c", "{shell_command}"]
herdr  = true
attach = ["hc", "attach", "{name}"]
send   = ["hc", "send", "{name}", "{text}"]
```

`{shell_command}` changes into the worktree and sets the environment; `--cwd` also sets the terminal's launch folder. `--no-parent` keeps the session independent of the launching terminal.

`herdr = true` starts the agent directly with `herdr-app do space.new`: the space's name is the issue title, its folder is the worktree (session scratch for issues without a repository), and its first terminal runs the harness shell command. The launching space and its tabs are unchanged. The returned terminal address is bound to the claim and used by `trk attach`, `trk send` and `trk runs`.

trk observes `herdr-app ls` for up to two seconds; only a direct agent pane in the created space, with the expected title and folder, counts as visible. JSON returns `placement.status` (`visible` or `failed`) with pane/tab/space IDs or a reason. Observation failures warn and retain the session and claim. If UI discovery fails before launch, the configured `start` template runs headlessly with a warning. If space creation itself fails or returns an uncertain response, the command errors and retains the claim; it never retries or falls back to a second session.

Replacing hc with tmux or a plain process is a config change: replace the templates and set `herdr = false`.

### Harness profiles

```toml
[harness.claude-code]
command = "claude"
args    = ["--strict-mcp-config", "--mcp-config", "{mcp_json}", "{briefing}"]
builtin_tools = { read-only = ["--disallowedTools", "Edit,Write,Bash,NotebookEdit"], none = ["--tools", ""] }

[harness.omp]
command = "omp"
args    = ["--model", "openai-codex/gpt-6-luna", "--config", "{session_dir}/omp.yml", "{briefing}"]
files   = { ".omp/mcp.json" = "{mcp_json}", "{session_dir}/omp.yml" = "skills:\n  ignoredSkills: [trk]\n" }
```

| Harness | MCP attachment | Confidence |
| --- | --- | --- |
| omp | Worktree `.omp/mcp.json`; a `--config` overlay hides the global trk skill | Verified live |
| Claude Code | `--mcp-config` with inline JSON, `--strict-mcp-config` to exclude other servers | High |
| OpenCode | `OPENCODE_CONFIG_CONTENT` inline runtime config | High |
| Codex CLI | `-c mcp_servers.*` overrides of `config.toml` | Medium |
| pi | Per-session agent directory with `mcp.json` | Medium |

A harness's built-in tools are not MCP and can only be limited through that harness's flags. Each profile maps the agent's coarse `builtin_tools` level to what its harness supports, and `trk agent check` warns when it cannot.

## Architecture

One binary, one database file, one write path. Every interface (the CLI for humans, the per-issue MCP server for agents, `trk check`) calls the same core library. That library is the only code that writes to the database, which is why rules SQLite cannot check (claims, the state machine, acyclic blockers) can be relied on.

### Crates

| Crate | Role |
| --- | --- |
| `trk-core` | Domain types, the state machine, claim rules, the waiting queue, query-language parser and SQL compiler, `Store` (the only writer) |
| `trk-agent` | Agent definitions, the six trk tools and their executor, prompt rendering, harness profiles, launch-spec builder |
| `trk-mcp` | Per-issue MCP server over `trk-core`; renews the claim's lease while it runs |
| `trk-cli` | The `trk` binary: argument parsing, actor and repo resolution, rendering, the check runner, `trk run` and the launcher templates |

### Files and locations

| What | Default path | Override |
| --- | --- | --- |
| Database | `$XDG_DATA_HOME/trk/trk.db` (macOS: `~/Library/Application Support/trk/trk.db`) | `TRK_DB`, `--db` |
| Config directory | `$XDG_CONFIG_HOME/trk/` | `TRK_CONFIG`, `--config` |
| Worktrees | `<data dir>/worktrees/<ID>` | `[run] worktrees` |
| Per-session scratch | `$XDG_STATE_HOME/trk/sessions/<actor-uuid>/` |  |

```
~/.config/trk/
  config.toml          # user, leases, check interval and timeout, default agent,
                       # launcher templates, harness profiles, named views
  agents/*.toml        # one file per agent
  prompts/*.j2         # MiniJinja templates for instructions and briefings
```

Nothing in the config directory is compiled into the database. Agents and prompts are read when a session starts.

### Repository resolution

When `--project` is not given, the CLI runs `git rev-parse --show-toplevel` and `git remote get-url origin` in the working directory. It normalizes the remote URL (scheme, `.git` suffix, SSH versus HTTPS form) and looks up the `repo` row, which belongs to a project. A repo without a remote is matched by its absolute path. If nothing matches, commands that need a project fail with `unknown_repo` and a hint to run `trk repo add`.

### Connection and transactions

- Each connection is opened with `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON` and `busy_timeout=5000`.
- Each mutating command is exactly one `BEGIN IMMEDIATE` transaction: sweep expired leases, validate, check claims, write rows, append events, commit. A command either happens fully or not at all.
- A check runs outside any transaction; only taking the lock and recording the result are transactions.
- Read commands use deferred transactions, so they never wait on writers under WAL.
- Expired claim leases are swept at the start of every write transaction: one indexed `UPDATE`, so no daemon is needed.
- The schema is applied on open, keyed on SQLite's `user_version`. `trk backup <path>` uses `VACUUM INTO` to write a consistent copy.
- A database written by the earlier six-type design is refused with `old_database`. There is no migration; `scripts/import-v1.sh` recreates an export of its open issues.

### Issue IDs

IDs are `<project-prefix>-<suffix>`, where the suffix is 3 random lowercase base32 characters, lengthened on collision (e.g. `orch-4kq`). The CLI accepts a unique suffix prefix (`trk show 4kq`) and fails with `ambiguous_id` instead of guessing.

## Data model

The schema is [`crates/trk-core/src/schema.sql`](crates/trk-core/src/schema.sql). SQLite enforces what it can: foreign keys, one active claim per issue, the set of states, close reasons and priorities, that a closed issue has a reason and a closing time, that only a waiting issue has waiting fields, the actor id format, JSON well-formedness, and an append-only event log. `trk-core` enforces the rest: the transitions, claimant rules, and acyclic blockers.

| Table | Holds |
| --- | --- |
| `project`, `repo` | Projects and the repositories that belong to them |
| `harness`, `actor` | Who acts; a harness row is added the first time one of its sessions acts |
| `issue` | One row per issue: authored fields, `state`, `close_reason`, the waiting fields, timestamps |
| `dep` | `src` blocks `dst` |
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
| `--as <actor>` | `TRK_ACTOR` | human from user config | Acting actor |
| `--project <id>` | `TRK_PROJECT` | current repo's project for list/ready | Explicit project scope; `--all-projects` clears it for list/ready |
| `--db <path>` | `TRK_DB` | XDG data dir | Database file |
| `--config <path>` | `TRK_CONFIG` | XDG config dir | Config directory |
| `--no-color` | `NO_COLOR` |  | Disable color |

### Commands

| Command | Purpose |
| --- | --- |
| **Issues** |  |
| `trk list [QUERY] [--all-projects] [--fields F,..] [--sort F] [--limit N]` | Query issues; defaults to the current project's scope; `@view` runs a saved view |
| `trk ready [QUERY] [--all-projects]` | Ready issues; defaults to the current project's scope |
| `trk show <ID>... [--with notes,events,deps,claims]` | Full issue with derived state |
| `trk create [TITLE] [-d TEXT] [-p P] [--agent A] [--blocked-by ID]... [--blocks ID]... [--dry-run]` | Create; opens `$EDITOR` when there is no title on a TTY |
| `trk edit <ID> [-t TITLE] [-d TEXT] [-p P] [--agent A] [--dry-run]` | Change the authored fields; `$EDITOR` with no options |
| `trk claim <ID> [--lease 30m] [--worktree PATH] [--steal]` | open → claimed |
| `trk unclaim <ID> [-m NOTE] [--force]` / `trk renew <ID>` | claimed → open / extend the lease |
| `trk close <ID> [-r done\|abandoned] [-m NOTE] [--force] [--dry-run]` | Close from open, claimed or waiting |
| `trk wait <ID> [-c CHECK] [--every DUR] [-r REASON] [--force] [--dry-run]` | claimed → waiting; on a waiting issue, replace the check |
| `trk reopen <ID> [-m NOTE]` | waiting or closed → open |
| `trk check [ID] [--all] [--watch [PAUSE]] [--dry-run]` | Run the checks of waiting issues |
| `trk note <ID> [TEXT\|-] [--amends SEQ] [--to-agent]` | Append a note; `--to-agent` also queues it for the agent's inbox |
| `trk dep add\|rm <BLOCKER> <BLOCKED>` | Add or remove a blocks edge |
| `trk log <ID>` | Timeline: events and notes |
| **Agent sessions** |  |
| `trk run <ID> [--agent A] [--harness H] [--fg] [--print] [--dry-run]` | Start an agent session on the issue through the launcher |
| `trk runs` | Active agent claims with agent, harness, terminal and lease |
| `trk attach <ID>` / `trk send <ID> TEXT` | Attach to or type into the session |
| `trk reap` | Release claims whose session has ended, with a note |
| `trk mcp --issue ID --actor A --agent N` | The per-issue MCP server (started by the harness, not by hand) |
| `trk agent list` / `preview <agent> [--issue ID]` / `check` | Inspect and validate agent definitions |
| **Setup** |  |
| `trk init [--force]` | Write the default config and create the database |
| `trk project add\|list`, `trk repo add\|list` | Register projects and repositories |
| `trk skill install\|uninstall\|status` | The CLI skill for interactive agent sessions |
| `trk whoami` | Resolved actor, project, config, database, issue counts, running check |
| `trk backup <PATH>` | Consistent copy via `VACUUM INTO` |

`-d` and `-c` take text, `@file`, or `@-` for stdin. In the interactive editor, `title`, `priority` and `agent` are TOML front matter and the description is the markdown body; if validation fails on save, the file reopens with the errors added as comments. `--dry-run` runs every check and returns the would-be result or the error, and writes nothing.

### Query language

A query is one string of terms joined by implicit AND. It is parsed into a typed AST and compiled to parameterised SQL.

```
query  := term (WS term)*
term   := NOT? (pred | flag)
pred   := name op value | name 'in' '(' value (',' value)* ')'
op     := '=' | '!=' | '<' | '<=' | '>' | '>=' | '~'      # '~' = full-text or glob
flag   := 'ready' | 'blocked' | 'due' | 'stale-lease'
value  := word | quoted | 'me' | '.' | reltime              # reltime: -7d, -12h
name   := 'id' | 'state' | 'project' | 'priority' | 'agent' | 'reason' | 'claimed-by'
        | 'created-by' | 'title' | 'created' | 'updated' | 'closed' | 'next-check'
        | 'text' | 'blocks' | 'blocked-by'
```

List and ready queries default to the current registered repository's project, even when other query terms are present. An explicit project predicate replaces that implicit scope. `--project ID` takes precedence over `TRK_PROJECT`; either explicitly scopes results, and a query project predicate further narrows it. `--all-projects` ignores repo and option/environment scopes while preserving query predicates. Outside a registered repository, provide `--project ID`, `TRK_PROJECT`, a project predicate, or `--all-projects`; trk never silently returns global results.

| Example | Meaning |
| --- | --- |
| `trk list 'state!=closed'` | Open work in the current repo's project |
| `trk list 'project=orch state!=closed'` | Open work in project `orch` |
| `trk ready --all-projects` | Ready work across every project |
| `state=waiting` | The waiting queue, with and without checks |
| `due` | Waiting issues `trk check` would run now |
| `claimed-by=me` / `claimed-by=agent:claude-code:*` | Current claims |
| `blocks=orch-m1 state!=closed` | What remains before `orch-m1` is ready |
| `closed>-7d reason=done` | Finished this week |
| `text~"feature flag"` | Full-text search over title, description and notes |

`--fields` sets table columns in pretty mode and object keys in JSON mode, using the same names plus `detail`, `waiting`, `check`, `ready`, `due`, `lease` and `repo`. Saved views in the user config store a query, fields and sort under a name.

### JSON envelope

```json
{
  "ok": true,
  "schema_version": 2,
  "data": {
    "id": "orch-4kq",
    "project": "orch",
    "repo": {"id": 1, "remote_url": "github.com/acme/orch", "local_path": "/src/orch"},
    "title": "Add claim leases",
    "description": "Crashed agents must release their claims.",
    "state": "waiting",
    "priority": "P1",
    "agent": null,
    "close_reason": null,
    "waiting": {
      "reason": "merge into main",
      "check": "cd \"$TRK_REPO\" && git merge --no-edit \"$TRK_BRANCH\"",
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
- Within a `schema_version`, keys are only ever added. Renaming or removing a key bumps the version. Version 2 is the single-type model.

### Errors and exit codes

In JSON mode, errors are written to stdout in the same envelope:

```json
{"ok": false, "schema_version": 2, "error": {
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
| 1 | Internal error | `internal`, `db_corrupt`, `old_database` |
| 2 | Usage error | `bad_args`, `bad_query` |
| 3 | Not found | `not_found`, `ambiguous_id`, `unknown_repo` |
| 4 | Validation failed | `invalid_field`, `invalid_state`, `cycle`, `config_invalid` |
| 5 | Conflict | `claimed_by_other`, `not_claimant`, `blocked`, `busy`, `not_permitted` |

### Pretty mode

```
$ trk list 'state!=closed'
 ID         PRIORITY  STATE      DETAIL                            TITLE
 orch-4kq   P1        ◷ waiting  merge into main · check in 4m     Add claim leases
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
| `directories` | cli | XDG / macOS data, config and state paths |
| `anyhow` | cli, mcp | Error context at the binary boundary only |

Git, `sh` and `kill` are invoked as executables through `std::process`. Tests use `assert_cmd`, `insta` (which pins the JSON format) and `tempfile`.

## Open questions and future work

### Open questions

- [ ] **Maximum wait.** Nothing ends a wait whose check answers "not yet" forever (an abandoned PR). `trk list state=waiting --sort next-check` shows them; an optional deadline that turns into a failure would make it automatic.
- [ ] **Worktree cleanup.** Worktrees are never removed, because a waiting check runs in them and a reopened issue reuses them. Removing them when an issue closes as done is probably safe.
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
