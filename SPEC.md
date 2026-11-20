# trk — Local Issue Tracker Design Spec

Nov 20, 2026 · @Rodrigo Setti

## Overview

`trk` is a local issue tracker and agent launcher in one Rust binary. It keeps the work of every repository in one SQLite database in the user's home directory. Humans use the CLI. Agents work through a per-issue MCP server. `trk run <ID>` starts an agent session on one issue in a persistent terminal (herdr-core, `hc`), with focused instructions and only that issue's tools.

**Terms.** An *agent* is instructions plus tools. A *harness* is the program that runs an agent: Claude Code, pi, omp, Codex, OpenCode. A *launcher* is where the harness runs: `hc`.

**Goals**

- One database for work across many repositories, with dependencies allowed between projects.
- A schema kept as configuration: issue types, fields, statuses, resolutions, dependency kinds, requirements and agents are TOML files, compiled into the database by `trk schema apply`.
- First-class claims by humans (`human:<email>`) and agent sessions (`agent:<harness>:<id>`), with leases.
- A complete, append-only history: every change and every note is an attributed, timestamped event.
- Required evidence on close: decisions and approvals carry their Slack thread, code-changes their PR, operations their audit trail.
- Agents assigned per task type, each with its own instructions and tools. Agents reach trk only through MCP, never through CLI skills.
- `trk run` starts a focused session for one issue, with minimal coupling to `hc`: three command templates.

**Non-goals (v1)**

- Issue hierarchies. Grouping is done only by `milestone` issues and `blocks` edges.
- Sync between machines, multiple users over a network, or a web UI.
- Storing state that can be read from elsewhere at any time (CI status, PR review state).
- Driving harnesses over ACP. It remains possible later as a second launch route.
- Controlling a harness's built-in tools (shell, edit, web) beyond what that harness's own flags allow.

## Architecture

One binary, one database file, one write path. Every interface (the CLI for humans, the per-issue MCP server for agents) calls the same core library. That library is the only code that writes to the database, which is why rules that SQLite cannot check (field validation, gates, claims) can be relied on. `trk run` hands a finished launch spec to the launcher (`hc`) and does nothing else with it.

&#91;embedded content: trk architecture · humans through the CLI, agents through a per-issue MCP server\]

### Crates

| Crate | Role |
| --- | --- |
| `trk-core` | Domain types, schema registry, config compiler (`schema plan` / `apply`), validation, gate and claim rules, query-language parser and SQL compiler, `Store` (the only writer) |
| `trk-agent` | Agent definitions, tool generation from a type's state machine, prompt rendering, harness profiles, launch-spec builder |
| `trk-mcp` | Per-issue MCP server (`trk mcp --issue … --agent …`) over `trk-core`; renews the claim's lease while it runs |
| `trk-cli` | The `trk` binary: argument parsing, actor and repo resolution, rendering (pretty / JSON / JSONL), `trk run` and the launcher templates |

### Files and locations

| What | Default path | Override |
| --- | --- | --- |
| Database | `$XDG_DATA_HOME/trk/trk.db` (macOS: `~/Library/Application Support/trk/trk.db`) | `TRK_DB`, `--db` |
| Config directory | `$XDG_CONFIG_HOME/trk/` | `TRK_CONFIG`, `--config` |
| Per-session scratch (rendered MCP configs, harness agent dirs) | `$XDG_STATE_HOME/trk/sessions/<actor-uuid>/` |  |

The config directory is the source of truth for everything configurable, and is meant to be kept in its own git repository:

```
~/.config/trk/
  config.toml          # user settings, launcher templates, harness profiles, named views
  fields.toml          # field definitions
  dep-kinds.toml       # dependency kinds
  types/*.toml         # one file per issue type, including its default agent
  agents/*.toml        # one file per agent: instructions + tools
  prompts/*.j2         # MiniJinja templates for instructions and briefings
```

The database's registry tables are a compiled copy, written only by `trk schema apply` (see Configuration workflow).

### Repository resolution

When `--project` is not given, the CLI runs `git rev-parse --show-toplevel` and `git remote get-url origin` in the working directory. It normalizes the remote URL (scheme, `.git` suffix, SSH versus HTTPS form) and looks up the `repo` row, which belongs to a project. A repo without a remote is matched by its absolute path. If nothing matches, commands that need a project fail with `unknown_repo` and a hint to run `trk repo add`.

### Connection and transactions

- Each connection is opened with `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON` and `busy_timeout=5000`.
- Each mutating command is exactly one `BEGIN IMMEDIATE` transaction: load the schema, validate, check claims and gates, write rows, append events, commit. A command either happens fully or not at all.
- Read commands use deferred transactions, so they never wait on writers under WAL.
- Expired claim leases are swept at the start of every write transaction. That is one indexed `UPDATE`, so no daemon is needed.
- Migrations use SQLite's `user_version` and run automatically on open. `trk backup <path>` uses `VACUUM INTO` to write a consistent copy.

### Issue IDs

IDs are `<project-prefix>-<suffix>`, where the suffix is 3 random lowercase base32 characters, lengthened on collision (e.g. `orch-4kq`). The CLI accepts a unique suffix prefix (`trk show 4kq`) and fails with `ambiguous_id` instead of guessing.

## Domain model

Six issue types share one table and one set of shared fields. Each type adds its own fields, statuses and resolutions. Every status counts as either `open` or `closed`. An intermediate status (anything that counts as open other than `open` itself) means the issue is claimed. Everything in this section is seed data in the registry tables, not code, and can be changed.

### Shared fields

| Field | Kind | Rule |
| --- | --- | --- |
| `title` | text | required |
| `context` | markdown | required: background, motivation, constraints |
| `status` | status | set only by `claim`, `move`, `unclaim`, `close` |
| `resolution` | enum (per type) | required to close |
| `outcome` | markdown | required to close research, decision, operation |
| `owner` | actor | optional; who is accountable (distinct from the claimant) |
| `links` | list of {kind, url, note} | free-form evidence |
| `tags` | list of text |  |
| `notes` | append-only log | stored as events, see Notes |
| `created_by`, `created_at`, `updated_at`, `closed_at` | system | written by the tool |

### Types

| Type | Statuses (→ order) | Resolutions | Type fields (\* = required at creation) | Required on transition / close |
| --- | --- | --- | --- | --- |
| `code-change` | open → in\_progress → in\_review → closed | merged, abandoned, superseded | repo\*, branch, pr\_url, merge\_commit | `pr_url` to enter in\_review; `merge_commit` when merged |
| `research` | open → in\_progress → closed | answered, inconclusive, abandoned | question\*, doc\_url, sources, confidence | `confidence` to close; `doc_url` when answered |
| `decision` | open → proposed → closed | decided, deferred, withdrawn, superseded | question\*, options, chosen, rationale, deciders, record\_url, revisit\_by | `chosen`, `rationale`, `deciders` when decided; `record_url` to close |
| `approval` | open → in\_progress → closed | approved, rejected, withdrawn, expired | approver\*, record\_url, conditions, expires\_at | `record_url` to close unless withdrawn |
| `operation` | open → ready → executing → closed | done, rolled\_back, failed, cancelled | change\_kind\*, target\*, environment\*, change\*, rollback\_plan, executed\_by, executed\_at, evidence\_url | `executed_by`, `executed_at` to enter executing; `evidence_url` when done |
| `milestone` | open → closed | reached, abandoned | target\_date, success\_criteria | `reached` only when every `blocks` source is closed |

`milestone` has no intermediate status, so it cannot be claimed. `approval` has `in_progress` so that a reviewer agent can claim it. An approval's subject is the target of its `approves` edge, not a field.

### Dependency kinds

Edges are stored `src → dst` and read "src *kind* dst". Only `blocks` and `approves` affect workflow; the rest record provenance or replacement.

| Kind | Inverse label | Allowed src → dst | Effect | Acyclic |
| --- | --- | --- | --- | --- |
| `blocks` | blocked by | any → any | dst not ready until src is closed | yes |
| `approves` | approved by | approval → code-change, operation, decision | gate rules (below) | yes |
| `informs` | informed by | research, decision → any | none | yes |
| `implements` | implemented by | code-change, operation → decision | none | yes |
| `discovered-from` | spawned | any → any | none | yes |
| `supersedes` | superseded by | same type → same type | required when closing as superseded | yes |
| `duplicates` | duplicated by | same type → same type | required when closing as duplicate | yes |
| `reverts` | reverted by | code-change → code-change, operation → operation | required when rolled back by another issue | yes |
| `relates` | relates | any → any | none | no |

A milestone is never the source of a `blocks` edge to a non-milestone; milestones may block other milestones. An issue may block several milestones.

### Gate rules

A gate rule says that an issue of type T cannot enter status S unless every incoming edge of kind K comes from an issue closed with resolution R. The seed rules:

| Type | Entering | Incoming kind | Source must be closed as |
| --- | --- | --- | --- |
| `operation` | executing | approves | approved |
| `code-change` | in\_review | approves | approved |
| `decision` | closed (decided) | approves | approved |

An issue is **ready** when it is `open`, has no active claim, and every incoming `blocks` source is closed. An issue is **gated** when an outgoing transition is held back by a gate rule.

### Actors

| Kind | Identifier | Example |
| --- | --- | --- |
| human | `human:<email>` | `human:rodrigo@example.com` |
| agent session | `agent:<harness>:<session-id>` | `agent:claude-code:12938192381` |

The kind ends at the first colon, and for agents the harness ends at the second. The rest is the session id, kept verbatim. For sessions started by trk run, it is a UUID minted by trk before launch; the harness's own session id is recorded in the actor's meta. Harnesses come from a registry. An agent actor may record `on_behalf_of`, the human who launched it. The acting actor comes from `--as`, then `TRK_ACTOR`, then the human in the user config.

### Claims

- `claim` requires `open` and no active claim. It records a claim and moves the issue to its type's first intermediate status, in one transaction. A partial unique index allows at most one active claim per issue.
- Agent claims get a lease (default 30 minutes), renewed by `trk renew` or by any write from the claimant. Human claims have no lease by default.
  - A claim records where the work happens: `worktree` (absolute path) and `host`. When `trk claim` runs inside a git checkout of the issue's repo, both are filled automatically; `--worktree <path>` overrides. For a code-change, the current branch is also written to the issue's `branch` field if that field is empty. The branch is durable and belongs to the issue; the worktree is local and temporary, so it belongs to the claim and stays in claim history after release.
- Only the claimant may `move` or `close` a claimed issue; `--force` overrides and is recorded.
- `unclaim` returns the issue to `open`. `close` releases the claim. An expired lease releases it with reason `expired` and returns the issue to `open`. `claim --steal` releases it with reason `stolen`.
- Invariant: an issue has an intermediate status if and only if it has an active claim.

For agent actors, a claim also records the agent definition that holds it (`agent`) and the launcher terminal name (`terminal`), both set by `trk run`. That makes "which agent closed this, and how did it do?" a query.

### Notes and history

Every mutation appends one or more rows to `event`, attributed to the actor, with a UTC timestamp. Notes are events with `op = 'note'`. A correction is a new note with `amends` pointing at the earlier one. Database triggers reject `UPDATE` and `DELETE` on `event`.

## Agents and sessions

Each task type names a default agent, and `trk run <ID>` starts that agent on the issue in a persistent `hc` terminal. The agent sees one issue and only the tools its definition selects. Agents reach trk only through MCP: their instructions never mention the `trk` CLI, and no skills are installed.

### Four configuration pieces

| Piece | Defines | Depends on |
| --- | --- | --- |
| Type | The work: fields, state machine, requirements, and the catalogue of trk tools that can be generated for it | nothing |
| Agent | Who does it: instructions, a selection from the type's trk tools, and any other MCP servers | the types it selects tools from |
| Harness profile | How to run an agent in one harness: command, arguments, environment, files, how MCP servers and instructions are passed | nothing trk-specific beyond placeholders |
| Launcher | Where it runs: three command templates for `hc` | nothing |

Types offer tools; agents choose from them. Harness profiles and the launcher never see issue data except through placeholders.

### Agent definitions

```toml
# agents/implementer.toml
name        = "implementer"
description = "Implements a code-change in its worktree and opens a PR"

instructions = "prompts/implementer.instructions.md.j2"   # → MCP server instructions
briefing     = "prompts/implementer.briefing.md.j2"       # → initial prompt
inbox        = true

[trk_tools]
include = ["@base", "set_branch", "submit_for_review", "close_as_abandoned"]

[trk_tools.overrides.submit_for_review]
description = "Call after the PR is open and CI has started. Requires the PR URL."

[trk_tools.composed.request_review_followup]
description = "File a follow-up for a reviewer comment you won't address in this PR."
create      = { type = "code-change", edge = "discovered-from", fields = ["title", "context"] }

[[mcp]]
name    = "github"
command = "github-mcp-server"
args    = ["stdio", "--toolsets", "pull_requests"]
env     = { GITHUB_TOKEN = "${env:GITHUB_TOKEN}" }

[harness]
prefer        = ["omp", "claude"]   # first installed wins; `trk run --harness` overrides
builtin_tools = "default"           # default | read-only | none
```

The agent for a run is the first of: `trk run --agent X`, the issue's `agent` field, the type's `routing.agent`. One agent can serve several types when its tool list uses `@base` or patterns such as `close_as_*`. `trk schema check` verifies that every tool an agent selects exists for every type routed to it.

A type has one agent at a time. Work that needs a second agent (review, verification) is a separate linked issue with its own agent, e.g. an `approval` with an `approves` edge, routed to a `reviewer` agent. The gate rules then enforce the order.

### The trk MCP server

The harness starts `trk mcp --issue <ID> --actor <actor> --agent <name>` over stdio. No tool takes an issue id: the server serves exactly one issue. It renews the claim's lease while it runs, so a crashed session releases its claim when the lease expires.

| Tool group | Tools | Source |
| --- | --- | --- |
| `@base` | `get_task`, `add_note`, `add_link`, `report_followup` (new issue with a `discovered-from` edge), `hand_back` (unclaim with a note) | every type |
| Generated | One tool per transition (`submit_for_review`, `mark_ready`, `propose` …) and one per close resolution (`close_as_merged` …); required fields become JSON Schema `required`, enums become `enum` | the type's state machine and requirements |
| Composed | Named combinations of core operations: set fields, move, close, create a linked issue, add a note (e.g. `request_approval`) | the agent file |

`get_task` is also exposed as the resource `trk://task`. Tools invalid in the current status return `invalid_transition` with a hint, rather than relying on clients to re-read the tool list when it changes.

**Enforcement.** `trk-core` allows agent actors to write only to issues they currently claim, plus creating `discovered-from` follow-ups; the rule applies to the CLI as well. An agent with a shell can still run `trk` with a human actor, so hard isolation requires running the harness where the database is not reachable and the MCP server, started outside that sandbox, is the only way in.

### Prompt channels

| Channel | Carries | Mechanism |
| --- | --- | --- |
| Briefing | The task: title, context, requirements, `derived.next` | Rendered from the agent's `briefing` template and passed as the harness's initial-prompt argument, or sent with the launcher's `send` template after launch |
| Instructions | Rules for the whole session | The MCP server's `instructions` and the tool descriptions. If a harness profile declares that its harness ignores server instructions, trk folds them into the briefing |
| Mid-session | Steering | `trk send <ID> "…"` via the launcher; `trk note <ID> --to-agent "…"` queues a message that the MCP server attaches to the agent's next trk tool result |

### `trk run`

```
trk run <ID> [--agent A] [--harness H] [--fg] [--print] [--dry-run]
```

1. Check the issue is ready, and resolve its agent and harness.
2. Create or reuse the worktree: `git worktree add <worktrees>/<ID> -b <ID>`.
3. Mint the actor `agent:<harness>:<uuid>`; the harness's own session id, once known, goes in `actor.meta`.
4. Claim the issue with the actor, agent, worktree and terminal name.
5. Render the launch spec: briefing, MCP server list (trk plus the agent's others), environment (`TRK_ACTOR`, for attribution only), files, argv.
6. Start it with the launcher's `start` template. `--print` emits the launch spec as JSON instead; `--dry-run` also skips the claim.

Related commands: `trk attach <ID>` and `trk send <ID>` (only when their launcher templates are set), `trk runs` (active claims with their terminals), `trk reap` (releases claims whose MCP server has gone, recording the exit as a note).

### Launcher

The launcher uses three command templates in `config.toml`, plus an optional Herdr integration:

```toml
[launcher]
start  = ["hc", "run", "--bg", "--no-parent", "--name", "{name}", "--cwd", "{cwd}", "--", "sh", "-c", "{shell_command}"]
herdr  = true
attach = ["hc", "attach", "{name}"]
send   = ["hc", "send", "{name}", "{text}"]
```

`{shell_command}` changes into the worktree and sets the environment; `--cwd` also sets the terminal's launch folder. `--no-parent` keeps the session independent of the launching terminal.

`herdr = true` enables hc-specific placement after start: resolve `$HC_TERMINAL_ID` with `hc get --json`, verify its `herdr.space` exists in `herdr-app ls`, then label the new terminal with that space only. Omitting `herdr.tab` requests a new tab without replacing a pane. trk observes `herdr-app ls` for up to two seconds; only a direct pane in a different tab in that space counts as visible. This does not require the tab to be selected.

Without `HC_TERMINAL_ID` or without a space label, launch stays headless. An unresolved terminal, unavailable UI, missing space, failed label command or unconfirmed placement produces a warning, not a failed launch: the running session and claim remain intact. JSON returns `placement.status` (`visible`, `headless`, `failed`) with pane/tab/space IDs or a reason; pretty output includes the placement result. A timed-out placement may appear later. No new space is created or focused as a fallback.

The integration defaults to off when omitted. Replacing hc with tmux or a plain process is a config change: replace the templates and remove `herdr` (or set it to false). Existing installations must opt in explicitly; upgrades do not overwrite user configuration.

### Harness profiles

```toml
[harness.claude]
command = "claude"
args    = ["--strict-mcp-config", "--mcp-config", "{mcp_json}", "{briefing}"]
builtin_tools = { read-only = ["--disallowedTools", "Edit,Write,Bash"] }

[harness.opencode]
command = "opencode"
env     = { OPENCODE_CONFIG_CONTENT = "{opencode_json}" }

[harness.codex]
command = "codex"
args    = ["-c", "mcp_servers.trk.command=\"trk\"", "-c", "mcp_servers.trk.args={trk_args_toml}", "{briefing}"]

[harness.pi]
command = "pi"
env     = { PI_CODING_AGENT_DIR = "{session_dir}" }   # trk writes {session_dir}/mcp.json

[harness.omp]
command = "omp"
files   = { ".mcp.json" = "{mcp_json}" }              # in the worktree, added to .git/info/exclude
```

| Harness | MCP attachment | Confidence |
| --- | --- | --- |
| Claude Code | `--mcp-config` with inline JSON, `--strict-mcp-config` to exclude other servers | High |
| OpenCode | `OPENCODE_CONFIG_CONTENT` inline runtime config | High |
| Codex CLI | `-c mcp_servers.*` overrides of `config.toml`; per-session `CODEX_HOME` as fallback | Medium |
| pi | Per-session agent directory with `mcp.json` (built-in `mcp` extension); credentials may need linking | Medium |
| omp | Worktree `.mcp.json`; `--config` overlay with `mcp.enableProjectConfig: false` to drop other project servers | Medium-low |

A harness's built-in tools are not MCP and can only be limited through that harness's flags. Each profile maps the agent's coarse `builtin_tools` level to what its harness supports, and `trk schema check` warns when it cannot.

## Data model

The schema has two layers. Registry tables hold the configurable schema (types, statuses, fields, dependency kinds, requirements). Data tables hold issues, edges, claims and the event log. SQLite enforces what it can: foreign keys, one active claim per issue, the actor id format, JSON well-formedness, and an append-only event log. `trk-core` enforces the rest: field kinds, required fields, transitions, gates and acyclicity. The registry tables are written only by trk schema apply, from the config files. The schema below was checked against SQLite 3.53, and through rusqlite 0.41 (bundled SQLite 3.54), with sample data.

Gate rules, required fields and edge requirements share one table, `requirement`, keyed by the type and the status being entered (plus a resolution when closing). Shared fields are columns on `issue`; type fields are keys in `issue.fields`. A field name in a requirement resolves to whichever applies.

### Schema

```sql
-- trk schema v1. Applied by rusqlite_migration; user_version = 1.
-- Connection pragmas (set on every open, not here): journal_mode=WAL,
-- synchronous=NORMAL, foreign_keys=ON, busy_timeout=5000.

-- ============================================================
-- Registry: projects, repos, actors
-- ============================================================

CREATE TABLE project (
  id          TEXT PRIMARY KEY            -- slug, also the issue-id prefix: 'orch'
              CHECK (id GLOB '[a-z]*' AND id NOT GLOB '*[^a-z0-9]*'),
  name        TEXT NOT NULL,
  created_at  TEXT NOT NULL
);

CREATE TABLE repo (
  id          INTEGER PRIMARY KEY,
  project_id  TEXT NOT NULL REFERENCES project(id),
  remote_url  TEXT UNIQUE,                -- normalized: 'github.com/acme/orch'
  local_path  TEXT UNIQUE,                -- fallback when there is no remote
  CHECK (remote_url IS NOT NULL OR local_path IS NOT NULL)
);

CREATE TABLE harness (
  name        TEXT PRIMARY KEY            -- 'claude-code', 'codex', ...
              CHECK (name GLOB '[a-z]*' AND name NOT GLOB '*[^a-z0-9-]*')
);

CREATE TABLE actor (
  id            TEXT PRIMARY KEY,         -- 'human:<email>' | 'agent:<harness>:<session>'
  kind          TEXT NOT NULL CHECK (kind IN ('human', 'agent')),
  harness       TEXT REFERENCES harness(name),
  on_behalf_of  TEXT REFERENCES actor(id),
  first_seen    TEXT NOT NULL,
  meta          TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(meta)),
  CHECK (
    (kind = 'human' AND harness IS NULL AND id GLOB 'human:?*')
    OR
    (kind = 'agent' AND harness IS NOT NULL AND id GLOB 'agent:' || harness || ':?*')
  )
);

-- ============================================================
-- Registry: the configurable schema
-- ============================================================

CREATE TABLE issue_type (
  name         TEXT PRIMARY KEY,          -- 'code-change', 'decision', ...
  description  TEXT
);

-- category: 'open' = unclaimed and not started; 'active' = intermediate,
-- counts as open, implies an active claim; 'closed' = done.
CREATE TABLE status (
  type      TEXT NOT NULL REFERENCES issue_type(name),
  name      TEXT NOT NULL,
  category  TEXT NOT NULL CHECK (category IN ('open', 'active', 'closed')),
  ord       INTEGER NOT NULL,             -- display and claim order
  PRIMARY KEY (type, name)
);
-- Exactly one entry status and one terminal status per type
-- (presence of each is checked by `trk schema check`).
CREATE UNIQUE INDEX status_one_open   ON status(type) WHERE category = 'open';
CREATE UNIQUE INDEX status_one_closed ON status(type) WHERE category = 'closed';

CREATE TABLE transition (
  type         TEXT NOT NULL,
  from_status  TEXT NOT NULL,
  to_status    TEXT NOT NULL,
  PRIMARY KEY (type, from_status, to_status),
  FOREIGN KEY (type, from_status) REFERENCES status(type, name),
  FOREIGN KEY (type, to_status)   REFERENCES status(type, name)
);

CREATE TABLE resolution (
  type  TEXT NOT NULL REFERENCES issue_type(name),
  name  TEXT NOT NULL,
  PRIMARY KEY (type, name)
);

-- Field definitions. shared = 1: stored as a column on `issue`;
-- shared = 0: stored as a key in `issue.fields`.
CREATE TABLE field (
  name         TEXT PRIMARY KEY CHECK (name GLOB '[a-z]*' AND name NOT GLOB '*[^a-z0-9_]*'),
  kind         TEXT NOT NULL CHECK (kind IN (
                 'text', 'markdown', 'int', 'bool', 'date', 'datetime',
                 'url', 'enum', 'actor', 'repo', 'link', 'list')),
  config       TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(config)),
               -- enum: {"values":[...]}; list: {"item":"url"}; int: {"min":0}
  shared       INTEGER NOT NULL DEFAULT 0 CHECK (shared IN (0, 1)),
  description  TEXT
);

CREATE TABLE type_field (
  type                TEXT NOT NULL REFERENCES issue_type(name),
  field               TEXT NOT NULL REFERENCES field(name),
  required_at_create  INTEGER NOT NULL DEFAULT 0 CHECK (required_at_create IN (0, 1)),
  default_value       TEXT CHECK (default_value IS NULL OR json_valid(default_value)),
  PRIMARY KEY (type, field)
);

CREATE TABLE dep_kind (
  name           TEXT PRIMARY KEY,        -- 'blocks', 'approves', ...
  inverse_label  TEXT NOT NULL,           -- 'blocked by'
  blocks_ready   INTEGER NOT NULL DEFAULT 0 CHECK (blocks_ready IN (0, 1)),
  acyclic        INTEGER NOT NULL DEFAULT 1 CHECK (acyclic IN (0, 1)),
  same_type      INTEGER NOT NULL DEFAULT 0 CHECK (same_type IN (0, 1)),
  description    TEXT
);

-- Allowed (src_type, dst_type) pairs per kind; '*' = any type.
CREATE TABLE dep_kind_types (
  kind      TEXT NOT NULL REFERENCES dep_kind(name),
  src_type  TEXT NOT NULL,
  dst_type  TEXT NOT NULL,
  PRIMARY KEY (kind, src_type, dst_type)
);

-- Conditions checked when an issue enters a status (and, for the closed
-- status, a resolution). Covers required fields, edge requirements and
-- gate rules with one mechanism.
--   field           : `field` must be non-empty
--   min_items       : list `field` has at least `n` items
--   out_edge        : the issue has an outgoing edge of `dep_kind`
--   in_edges_closed : every incoming `dep_kind` source is closed
--                     (with `src_resolution` if set). Gate rules and
--                     milestone completion use this.
CREATE TABLE requirement (
  id              INTEGER PRIMARY KEY,
  type            TEXT NOT NULL REFERENCES issue_type(name),
  to_status       TEXT NOT NULL,
  resolution      TEXT,                   -- NULL = any (or not closing)
  kind            TEXT NOT NULL CHECK (kind IN ('field', 'min_items', 'out_edge', 'in_edges_closed')),
  field           TEXT REFERENCES field(name),
  n               INTEGER,
  dep_kind        TEXT REFERENCES dep_kind(name),
  src_resolution  TEXT,
  FOREIGN KEY (type, to_status) REFERENCES status(type, name),
  CHECK (
    (kind = 'field'           AND field IS NOT NULL) OR
    (kind = 'min_items'       AND field IS NOT NULL AND n IS NOT NULL) OR
    (kind = 'out_edge'        AND dep_kind IS NOT NULL) OR
    (kind = 'in_edges_closed' AND dep_kind IS NOT NULL)
  )
);
CREATE INDEX requirement_lookup ON requirement(type, to_status);

-- ============================================================
-- Data
-- ============================================================

CREATE TABLE issue (
  id          TEXT PRIMARY KEY,           -- '<project>-<base32>': 'orch-4kq'
  project_id  TEXT NOT NULL REFERENCES project(id),
  type        TEXT NOT NULL REFERENCES issue_type(name),
  status      TEXT NOT NULL,
  resolution  TEXT,                       -- non-NULL iff status category = 'closed' (enforced in core)
  -- shared fields
  title       TEXT NOT NULL CHECK (length(title) > 0),
  context     TEXT NOT NULL CHECK (length(context) > 0),
  outcome     TEXT,
  owner       TEXT REFERENCES actor(id),
  links       TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(links) AND json_type(links) = 'array'),
  tags        TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(tags) AND json_type(tags) = 'array'),
  -- type-specific fields, validated against type_field by trk-core
  fields      TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(fields) AND json_type(fields) = 'object'),
  -- system
  created_by  TEXT NOT NULL REFERENCES actor(id),
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL,
  closed_at   TEXT,
  FOREIGN KEY (type, status)     REFERENCES status(type, name),
  FOREIGN KEY (type, resolution) REFERENCES resolution(type, name)
);
CREATE INDEX issue_project_status ON issue(project_id, status);
CREATE INDEX issue_type_status    ON issue(type, status);
CREATE INDEX issue_updated        ON issue(updated_at);
-- Hot custom fields get expression indexes, created by `trk schema index <field>`:
-- CREATE INDEX issue_f_environment ON issue(json_extract(fields, '$.environment'));

CREATE TABLE dep (
  src         TEXT NOT NULL REFERENCES issue(id),
  dst         TEXT NOT NULL REFERENCES issue(id),
  kind        TEXT NOT NULL REFERENCES dep_kind(name),
  created_by  TEXT NOT NULL REFERENCES actor(id),
  created_at  TEXT NOT NULL,
  PRIMARY KEY (src, dst, kind),
  CHECK (src <> dst)
);
CREATE INDEX dep_by_dst ON dep(dst, kind);

CREATE TABLE claim (
  id              INTEGER PRIMARY KEY,
  issue_id        TEXT NOT NULL REFERENCES issue(id),
  actor           TEXT NOT NULL REFERENCES actor(id),
  claimed_at      TEXT NOT NULL,
  lease_until     TEXT,                   -- NULL = no expiry
  worktree        TEXT,                   -- absolute path of the checkout used, if any
  host            TEXT,                   -- machine the claim was made on
  agent           TEXT,                   -- agent definition name, for agent actors
  terminal        TEXT,                   -- launcher terminal name set by `trk run`
  released_at     TEXT,
  release_reason  TEXT CHECK (release_reason IN ('closed', 'unclaimed', 'expired', 'stolen')),
  CHECK ((released_at IS NULL) = (release_reason IS NULL))
);
CREATE UNIQUE INDEX claim_one_active ON claim(issue_id) WHERE released_at IS NULL;
CREATE INDEX claim_active_lease ON claim(lease_until) WHERE released_at IS NULL;
CREATE INDEX claim_by_actor ON claim(actor, released_at);

-- Append-only history. Notes are events with op = 'note'.
CREATE TABLE event (
  seq       INTEGER PRIMARY KEY,
  issue_id  TEXT REFERENCES issue(id),    -- NULL for schema/registry events
  actor     TEXT NOT NULL REFERENCES actor(id),
  at        TEXT NOT NULL,                -- RFC 3339 UTC
  op        TEXT NOT NULL CHECK (op IN (
              'create', 'set', 'move', 'claim', 'release', 'close',
              'dep_add', 'dep_rm', 'note', 'schema')),
  payload   TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(payload))
);
CREATE INDEX event_by_issue ON event(issue_id, seq);

CREATE TRIGGER event_no_update BEFORE UPDATE ON event
BEGIN SELECT RAISE(ABORT, 'event log is append-only'); END;
CREATE TRIGGER event_no_delete BEFORE DELETE ON event
BEGIN SELECT RAISE(ABORT, 'event log is append-only'); END;

-- Issues are never deleted; they are closed with a resolution.
CREATE TRIGGER issue_no_delete BEFORE DELETE ON issue
BEGIN SELECT RAISE(ABORT, 'issues are closed, not deleted'); END;

-- ============================================================
-- Full-text search (title, context, outcome, notes)
-- ============================================================

CREATE VIRTUAL TABLE search USING fts5(
  issue_id UNINDEXED, source UNINDEXED, body,
  tokenize = 'porter unicode61'
);

CREATE TRIGGER issue_fts_insert AFTER INSERT ON issue BEGIN
  INSERT INTO search (issue_id, source, body)
  VALUES (NEW.id, 'issue', NEW.title || ' ' || NEW.context || ' ' || coalesce(NEW.outcome, ''));
END;
CREATE TRIGGER issue_fts_update AFTER UPDATE OF title, context, outcome ON issue BEGIN
  DELETE FROM search WHERE issue_id = NEW.id AND source = 'issue';
  INSERT INTO search (issue_id, source, body)
  VALUES (NEW.id, 'issue', NEW.title || ' ' || NEW.context || ' ' || coalesce(NEW.outcome, ''));
END;
CREATE TRIGGER note_fts_insert AFTER INSERT ON event WHEN NEW.op = 'note' BEGIN
  INSERT INTO search (issue_id, source, body)
  VALUES (NEW.issue_id, 'note', json_extract(NEW.payload, '$.body'));
END;

-- ============================================================
-- Views
-- ============================================================

CREATE VIEW note AS
SELECT seq, issue_id, actor, at,
       json_extract(payload, '$.body')   AS body,
       json_extract(payload, '$.amends') AS amends
FROM event WHERE op = 'note';

CREATE VIEW active_claim AS
SELECT * FROM claim WHERE released_at IS NULL;

CREATE VIEW issue_view AS
SELECT i.*, s.category AS status_category, c.actor AS claimant, c.lease_until
FROM issue i
JOIN status s ON s.type = i.type AND s.name = i.status
LEFT JOIN active_claim c ON c.issue_id = i.id;

-- Open, unclaimed, and every blocks_ready source closed.
CREATE VIEW ready AS
SELECT i.* FROM issue_view i
WHERE i.status_category = 'open'
  AND i.claimant IS NULL
  AND NOT EXISTS (
    SELECT 1
    FROM dep d
    JOIN dep_kind k ON k.name = d.kind AND k.blocks_ready = 1
    JOIN issue_view b ON b.id = d.src
    WHERE d.dst = i.id AND b.status_category <> 'closed'
  );

CREATE VIEW milestone_progress AS
SELECT m.id,
       count(b.id)                                         AS total,
       count(b.id) FILTER (WHERE b.status_category = 'closed') AS closed
FROM issue_view m
LEFT JOIN dep d ON d.dst = m.id AND d.kind = 'blocks'
LEFT JOIN issue_view b ON b.id = d.src
WHERE m.type = 'milestone'
GROUP BY m.id;
```

### Core queries

```sql
-- Cycle check before inserting src -> dst for an acyclic kind:
-- the edge is refused if src is reachable from dst.
WITH RECURSIVE reach(id) AS (
  SELECT :dst
  UNION
  SELECT d.dst FROM dep d JOIN reach r ON d.src = r.id WHERE d.kind = :kind
)
SELECT 1 FROM reach WHERE id = :src LIMIT 1;

-- Unmet in_edges_closed requirements (gates, milestone completion)
-- for issue :id of type :type entering :to_status.
SELECT r.dep_kind, d.src
FROM requirement r
JOIN dep d        ON d.dst = :id AND d.kind = r.dep_kind
JOIN issue_view s ON s.id = d.src
WHERE r.type = :type AND r.to_status = :to_status
  AND r.kind = 'in_edges_closed'
  AND (s.status_category <> 'closed'
       OR (r.src_resolution IS NOT NULL AND s.resolution IS NOT r.src_resolution));

-- Lease sweep, run at the start of every write transaction
-- (followed by one 'release' event per row and a status reset to 'open').
UPDATE claim SET released_at = :now, release_reason = 'expired'
WHERE released_at IS NULL AND lease_until < :now
RETURNING issue_id, actor;
```

### Configuration workflow

Types, fields, dependency kinds and agents are edited as files and applied like a migration plan. A type file, abridged:

```toml
# types/code-change.toml
[type]
name        = "code-change"
description = "A change delivered as a PR"
statuses    = [ { name = "open",        category = "open" },
                { name = "in_progress", category = "active" },
                { name = "in_review",   category = "active" },
                { name = "closed",      category = "closed" } ]
resolutions = ["merged", "abandoned", "superseded"]

[type.transitions]
open        = ["in_progress", "closed"]
in_progress = ["in_review", "open", "closed"]
in_review   = ["in_progress", "closed"]

[fields]
repo         = { required_at_create = true }
branch       = {}
pr_url       = {}
merge_commit = {}

[[requires]]
entering = "in_review"
fields   = ["pr_url"]

[[requires]]
entering        = "in_review"
in_edges_closed = { kind = "approves", src_resolution = "approved" }

[[requires]]
entering   = "closed"
resolution = "merged"
fields     = ["merge_commit", "outcome"]

[[requires]]
entering   = "closed"
resolution = "superseded"
out_edge   = "supersedes"

[routing]
agent = "implementer"
```

| Command | What it does |
| --- | --- |
| `trk schema check` | Checks the config: unreachable statuses, a type with no path to `closed`, requirements naming unknown fields, agents selecting tools a routed type lacks, templates that fail to render, `builtin_tools` levels a preferred harness cannot enforce |
| `trk schema plan` | Diffs the files against the database: registry changes, existing issues the new rules affect, and the change in each agent's tool list |
| `trk schema apply [--map-status old=new]` | Applies in one transaction and records a `schema` event with the config's git commit. A status still in use can be removed only with a mapping |
| `trk schema graph <type>` | Draws the state machine as Mermaid or DOT |
| `trk agent preview <agent> [--type T \| --issue ID]` | Prints what a session would receive: rendered instructions and briefing, trk tool schemas, other MCP servers |
| `trk --db /tmp/scratch.db …` | Tries a config change on a throwaway database first |

A new requirement does not rewrite existing issues: `plan` lists the ones that violate it, and it applies at each issue's next transition. Because generated tools follow the state machine, changing a type silently changes what agents can do; the tool diff in `plan` is the main thing to review on each `apply`.

## CLI API and UX

Every command builds one typed result, then renders it in the requested format. Pretty output is a view of the JSON, never a separate query, so humans and agents always see the same facts. The JSON format is a versioned API.

### Global options

| Option | Env | Default | Meaning |
| --- | --- | --- | --- |
| `--format pretty\|json\|jsonl` | `TRK_FORMAT` | `pretty` on a TTY, `json` otherwise | Output format; `jsonl` for `list` and `log` |
| `--json` |  |  | Shorthand for `--format json` |
| `--as <actor>` | `TRK_ACTOR` | human from user config | Acting actor |
| `--project <id>` | `TRK_PROJECT` | resolved from the git repo | Project scope |
| `--db <path>` | `TRK_DB` | XDG data dir | Database file |
| `--no-color` | `NO_COLOR` |  | Disable color |

Scripts should always pass `--json`, so behaviour never depends on terminal detection. Agents don't use the CLI; they work through `trk mcp`.

### Commands

| Command | Purpose |
| --- | --- |
| **Issues** |  |
| `trk list [QUERY] [--fields F,..] [--sort F] [--limit N]` | Query issues; `@view` runs a saved view |
| `trk show <ID>... [--with notes,events,deps,claims]` | Full issue with derived state |
| `trk create <TYPE> [-t TITLE] [-f k=v]... [--from FILE\|-] [--dry-run]` | Create; opens `$EDITOR` when required fields are missing on a TTY |
| `trk edit <ID> [-f k=v]... [--from FILE\|-] [--dry-run]` | Change field values only; `$EDITOR` with no arguments |
| `trk claim <ID> [--lease 30m] [--worktree PATH] [--steal]` | Claim and enter the first intermediate status |
| `trk unclaim <ID>` / `trk renew <ID>` | Release and return to `open` / extend the lease |
| `trk move <ID> <STATUS> [-f k=v]... [--force]` | Move between intermediate statuses |
| `trk close <ID> --resolution R [-f k=v]... [--force]` | Close; checks required fields and gates |
| `trk note <ID> [TEXT\|-] [--amends SEQ] [--to-agent]` | Append a note; `--to-agent` also queues it for the agent's inbox |
| `trk dep add\|rm <SRC> <KIND> <DST>` | Add or remove an edge |
| `trk log <ID>` | Merged timeline: events, notes, claims |
| `trk ready [QUERY]` | Shorthand for `trk list 'ready …'` |
| **Agent sessions** |  |
| `trk run <ID> [--agent A] [--harness H] [--fg] [--print] [--dry-run]` | Start an agent session on the issue through the launcher |
| `trk runs` | Active agent claims with agent, harness, terminal and lease |
| `trk attach <ID>` / `trk send <ID> TEXT` | Attach to or type into the session (if launcher templates are set) |
| `trk reap` | Release claims whose session has ended, with a note |
| `trk mcp --issue ID --actor A --agent N` | The per-issue MCP server (started by the harness, not by hand) |
| `trk agent list` / `trk agent preview <agent> [--type T\|--issue ID]` | Inspect agent definitions and what a session would receive |
| **Configuration** |  |
| `trk schema check\|plan\|apply\|graph <TYPE>` | Validate, diff, apply and draw the configuration |
| `trk schema [TYPE]` | Fields, statuses, resolutions, requirements as applied |
| `trk schema index <FIELD>` | Add an expression index for a hot custom field |
| `trk project add\|list`, `trk repo add\|list` | Register projects and repositories |
| `trk whoami` | Resolved actor, project, config and database |
| `trk backup <PATH>` | Consistent copy via `VACUUM INTO` |

Status changes happen only through `claim`, `unclaim`, `move` and `close`, where claim and gate rules apply. `edit` cannot change status.

### Setting fields

The same syntax applies to `create`, `edit`, `move` and `close`:

```
-f severity=high              # scalar, parsed by the field's kind
-f tags+=infra                # append to a list
-f tags-=infra                # remove from a list
-f rationale=@why.md          # value from a file
-f rationale=@-               # value from stdin
-f revisit_by=                # clear (refused if required in the current status)
--from issue.json             # whole object, same shape as `show --json`
```

`--from` accepts the shape that `show --json` emits; fields owned by the tool are ignored with a warning. `--dry-run` runs every check and returns the would-be result or the error, and writes nothing. In the interactive editor, fields are TOML front matter and `context` is the markdown body. If validation fails on save, the file reopens with the errors added as comments.

### Query language

A query is one string of terms joined by implicit AND. It is parsed into a typed AST and compiled to parameterised SQL.

```
query  := term (WS term)*
term   := NOT? (pred | flag)
pred   := name op value | name 'in' '(' value (',' value)* ')'
op     := '=' | '!=' | '<' | '<=' | '>' | '>=' | '~'      # '~' = full-text or glob
flag   := 'ready' | 'blocked' | 'gated' | 'stale-lease'
value  := word | quoted | 'me' | '.' | reltime              # reltime: -7d, -12h
name   := builtin | field | 'field.' field
```

| Example | Meaning |
| --- | --- |
| `type=operation environment=prod status!=closed` | Open prod operations |
| `type in (decision,approval) closed>-7d resolution=decided` | Decisions made this week |
| `claimed-by=me` / `claimed-by=agent:claude-code:*` | Current claims |
| `blocks=orch-m1 status=open` | What remains for a milestone |
| `ready project=.` | Work available in the current repo's project |
| `text~"feature flag"` | Full-text search over title, context, outcome, notes |

Built-in names are `id`, `type`, `status`, `category`, `resolution`, `project`, `owner`, `claimed-by`, `created`, `updated`, `closed`, `tag`, `text`, `blocks`, `blocked-by` and `dep:<kind>`. Any other name resolves through the field registry. `--fields` sets table columns in pretty mode and object keys in JSON mode, using the same names. Saved views in the user config store a query, fields and sort under a name.

### JSON envelope

```json
{
  "ok": true,
  "schema_version": 1,
  "data": {
    "id": "orch-4kq",
    "type": "decision",
    "project": "orch",
    "title": "Use SQLite as the tracker store",
    "context": "…",
    "status": "proposed",
    "status_category": "active",
    "resolution": null,
    "outcome": null,
    "owner": "human:rodrigo@example.com",
    "links": [],
    "tags": ["storage"],
    "fields": {
      "question": "Which embedded DB?",
      "options": ["SQLite", "Dolt", "Cozo"],
      "record_url": null
    },
    "claim": {
      "actor": "agent:claude-code:12938192381",
      "on_behalf_of": "human:rodrigo@example.com",
      "claimed_at": "2026-11-18T18:02:11Z",
      "lease_until": "2026-11-18T18:32:11Z"
    },
    "deps": {
      "out": [{"kind": "blocks", "id": "orch-m1", "type": "milestone", "status_category": "open"}],
      "in":  [{"kind": "informs", "id": "orch-r7c", "type": "research", "status_category": "closed"}]
    },
    "derived": {
      "ready": false,
      "blocked_by": [],
      "gates_unmet": [],
      "next": {
        "closed": {"decided": ["chosen", "rationale", "deciders", "record_url", "outcome"]}
      }
    },
    "created_by": "human:rodrigo@example.com",
    "created_at": "2026-11-18T17:40:03Z",
    "updated_at": "2026-11-18T18:02:11Z",
    "closed_at": null
  }
}
```

- `list` returns `data` as an array; `jsonl` emits one issue object per line, without the envelope.
- `derived` is computed when the output is built, never stored. `derived.next` lists, for each reachable status and resolution, the fields still missing, so an agent knows what to supply before it tries.
- Each edge carries its neighbour's type and status category, which saves an agent a follow-up `show`.
- Timestamps are RFC 3339 UTC in JSON. Relative times appear only in pretty mode.
- Within a `schema_version`, keys are only ever added. Renaming or removing a key bumps the version.

### Errors and exit codes

In JSON mode, errors are written to stdout in the same envelope:

```json
{"ok": false, "schema_version": 1, "error": {
  "code": "requirements_unmet",
  "message": "decision cannot close as 'decided'",
  "issue": "orch-4kq",
  "missing_fields": ["chosen", "rationale", "record_url"],
  "unmet_gates": [],
  "hint": "trk close orch-4kq --resolution decided -f chosen=... -f rationale=@- -f record_url=..."
}}
```

| Exit | Meaning | Example codes |
| --- | --- | --- |
| 0 | OK |  |
| 1 | Internal error | `internal`, `db_corrupt` |
| 2 | Usage error | `bad_args`, `bad_query` |
| 3 | Not found | `not_found`, `ambiguous_id`, `unknown_repo` |
| 4 | Validation failed | `requirements_unmet`, `invalid_field`, `invalid_transition`, `cycle` |
| 5 | Conflict | `claimed_by_other`, `not_claimant`, `gate_unmet`, `busy` |

### Pretty mode

```
$ trk list 'project=orch status!=closed'
 ID         TYPE          STATUS        CLAIM                     TITLE
 orch-4kq   decision      ● proposed    claude-code:1293… 12m     Use SQLite as the tracker store
 orch-9xa   code-change   ● in_review   rodrigo                   Add claim leases
 orch-2pp   operation     ○ open        ⛔ gated (approval)        Enable flag trk.leases in prod
 orch-m1    milestone     ○ open        3/5 done                  MVP
```

Color follows the status category: open is neutral, active is yellow, closed is dim green. Blocked and gated issues show a marker where the claim would be; milestones show progress. Agent ids are shortened in tables and shown in full by `show`. `show` prints, in order: header, rendered `context`, fields, dependencies, claim, and the latest timeline entries (`--with events` for all).

## Rust dependencies

The dependency set is small: no ORM, no C library outside SQLite. `trk-core` and the CLI are synchronous. Only `trk-mcp` runs an async runtime (single-threaded Tokio, required by `rmcp`), and it calls the core through `spawn_blocking`. Versions are the latest on crates.io as of 2026-11-20, except `rusqlite`, which is pinned to 0.41: the newest `rusqlite_migration` (2.7) requires `rusqlite` ^0.41, and the two cannot both link SQLite at different versions. A probe build with this set resolved and type-checked, and applied the full schema, FTS5 included, on bundled SQLite 3.54.1.

### Runtime

| Crate | Version | Crates | Used for |
| --- | --- | --- | --- |
| `rusqlite` (features: `bundled`) | 0.41 | core | SQLite access; `bundled` compiles SQLite in, so the binary has no system dependency and FTS5 is available |
| `rusqlite_migration` | 2.7 | core | Schema migrations keyed on `user_version` |
| `serde` (feature: `derive`) | 1 | all | Typed issue, schema, agent and envelope structs |
| `serde_json` | 1 | all | JSON fields, envelope, `--from` input, MCP tool schemas, launch spec |
| `toml` | 1.2 | core, agent, cli | Config files, editor front matter |
| `jiff` (feature: `serde`) | 1.0 | core, cli | UTC timestamps, leases, relative times (`-7d`, "12m ago") |
| `winnow` | 1.0 | core | Query-language and `-f` field-syntax parsers |
| `rand` | 0.10 | core | Random issue-id suffixes |
| `uuid` (feature: `v4`) | 1 | agent | Agent actor ids minted by `trk run` |
| `url` | 2.5 | core | Validating `url`-kind fields |
| `thiserror` | 2 | core, agent | Typed errors that map one-to-one to JSON error codes and exit codes |
| `minijinja` | 3.1 | agent | Instruction and briefing templates |
| `rmcp` (features: `server`, `transport-io`) | 3.11 | mcp | The official Rust MCP SDK: stdio server, tools, resources, instructions |
| `tokio` (features: `rt`, `macros`, `io-std`) | 1 | mcp | Single-threaded runtime for `rmcp` |
| `clap` (features: `derive`, `env`) | 4.6 | cli | Arguments, subcommands, env fallbacks, shell completions |
| `anstream` | 1.0 | cli | Color that degrades on non-TTY and honours `NO_COLOR` |
| `owo-colors` | 4.4 | cli | Styling |
| `comfy-table` | 8 | cli | Tables in pretty mode |
| `termimad` | 0.35 | cli | Rendering markdown `context` and notes in the terminal |
| `directories` | 6 | cli | XDG / macOS data, config and state paths |
| `anyhow` | 1 | cli, mcp | Error context at the binary boundary only |

Git is invoked as the `git` executable through `std::process`, not linked as a library. Repository resolution needs two read-only commands, and shelling out avoids a large dependency. Terminal detection uses `std::io::IsTerminal`.

### Development

| Crate | Version | Used for |
| --- | --- | --- |
| `assert_cmd` | 2.2 | End-to-end tests that run the binary |
| `insta` (feature: `json`) | 1.49 | Snapshot tests of JSON and pretty output, which pin the JSON format |
| `tempfile` | 3 | One throwaway database per test |

### Considered and not chosen

| Crate | Reason |
| --- | --- |
| `sqlx` | Async and compile-time-checked queries add a runtime and a build step a synchronous CLI doesn't need. The MCP server's async needs are met by a single-threaded Tokio runtime around the synchronous core. |
| `chumsky` | Excellent error recovery, but the current release is still `1.0.0-alpha`. `winnow` is stable and enough for a small grammar. |
| `serde_yaml` | Deprecated by its author. The editor uses TOML front matter instead of YAML, so no YAML crate is needed. |
| `gix` / `git2` | Heavy for two read-only git queries. |
| `jsonschema` | Field validation is driven by the `field` registry, not JSON Schema documents. |
| `miette` | Rich diagnostics are attractive for query errors. Optional later; `thiserror` plus a hint field covers v1. |

## Open questions and future work

### Open questions

- [ ] **Seeded gate rules.** The seed gates `code-change → in_review` and `decision → closed (decided)` on `approves` edges, as well as `operation → executing`. A gate passes trivially when there is no `approves` edge, so this only bites when an approval is attached. Keep all three, or only the operation gate?
- [ ] **Lease length.** 30 minutes for agent claims is a guess. It should exceed the longest silent stretch of a typical agent session.
- [ ] **Harness profiles to verify.** Codex `-c` overrides with a nested `mcp_servers` table; how omp takes a per-session MCP config and whether its `--config` overlay can define servers; where pi keeps credentials when `PI_CODING_AGENT_DIR` points at a session directory; which harnesses put an MCP server's `instructions` into the system prompt.
- [ ] **`hc` options.** Check `hc --skill` or `hc schema` for working-directory, environment and kill options. If they exist, the `sh -c` wrapping in the `start` template can go.
- [ ] **Briefing timing.** For harnesses without an initial-prompt argument, the `send` after launch must wait until the session is ready, and `hc` only knows that when the harness reports it.
- [ ] **Built-in tool policies.** Which `builtin_tools` levels each harness can actually enforce, read-only above all, for reviewer agents.
- [ ] **Milestone closing.** Milestones close manually; `trk ready 'type=milestone'` lists those whose blockers are all closed. Auto-closing is possible but would create events no actor performed.
- [ ] **`owner`.** Keep it as "accountable person", separate from the claimant, or drop it?
- [ ] **Changes to required fields.** When the registry gains a requirement, existing issues are not rewritten. `trk schema check` lists violators, and the requirement applies at each issue's next transition.

### Future work

| Item | Notes |
| --- | --- |
| ACP launch route | A second way to run agents: drive the harness over ACP, passing MCP servers in `session/new` and answering permission requests from policy. Complements `hc`, which keeps takeover. |
| Status-dependent tool lists | Show only the tools valid in the current status, using MCP tool-list change notifications, once the harnesses' clients are known to re-read the list. |
| Agent evaluation | Outcome statistics per agent and harness (time to close, resolutions, hand-backs) from claims and events. |
| Sync between machines | Replicate the `event` log as an operation stream instead of merging databases. Also what `hc run --on <machine>` would need, since `trk mcp` must reach the database. |
| Tamper evidence | A hash chain over `event` (each row stores a hash of the previous one) without other schema changes. |
| External status | Optional lookups of PR state through `gh` at render time, never stored. |
| Turso / Limbo | A SQLite-compatible engine written in Rust; worth evaluating once stable. FTS5 support should be checked. |
