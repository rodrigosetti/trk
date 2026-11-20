# trk

A local issue tracker and agent launcher in one Rust binary. All repositories share one SQLite
database; humans use the CLI, agents work through a per-issue MCP server, and `trk run <ID>` starts
an agent session on one issue in a persistent terminal (`hc`). See [SPEC.md](SPEC.md) for the design.

## Build

```sh
cargo build --release          # target/release/trk
cargo test --workspace         # unit, store, agent and end-to-end tests
```

## Quick start

```sh
trk init                                   # writes ~/.config/trk/ and applies it to the database
trk project add orch --name "Orchestra"
cd ~/src/orch && trk repo add --project orch

trk create code-change -t "Add claim leases" -f context="Crashed agents must release claims."
trk list 'project=. category!=closed'
trk claim orch-4kq                          # enters in_progress, records worktree + branch
trk move orch-4kq in_review -f pr_url=https://github.com/acme/orch/pull/12
trk close orch-4kq -r merged -f merge_commit=9f2c1e0
trk log orch-4kq
```

Every command takes `--json` (or `--format pretty|json|jsonl`). Scripts should always pass `--json`;
JSON output is the versioned API (`schema_version: 1`), and errors use the same envelope with a
stable `code`, a `hint`, and exit codes 1–5.

## Agent sessions

```sh
trk run orch-4kq                 # claim as agent:<harness>:<uuid>, create the worktree, start in hc
trk runs                         # active agent claims: agent, harness, terminal, lease, MCP liveness
trk attach orch-4kq              # take over the terminal
trk note orch-4kq --to-agent "Also update the docs"   # delivered with the agent's next tool result
trk send orch-4kq "Wrap up and submit"                # typed into the session
trk reap                         # release claims whose MCP server has gone, with a note
trk run orch-4kq --dry-run       # print the launch spec; claims nothing
trk agent preview implementer --issue orch-4kq        # instructions, briefing, tool schemas
```

The agent for a run is `--agent`, else the issue's `agent` field, else the type's `routing.agent`.
The harness is `--harness`, else the agent's first installed preference. The seed config prefers
`omp` running `openai-codex/gpt-6-luna`.

The MCP server (`trk mcp --issue … --actor … --agent …`, started by the harness) serves exactly one
issue: `get_task`, `add_note`, `add_link`, `report_followup`, `hand_back`, one tool per transition and
close resolution (required fields become JSON Schema `required`), `set_<field>` setters, and the
agent's composed tools. It renews the claim's lease while it runs.

## Configuration

`trk init` writes the seed configuration (keep it in its own git repository):

```
~/.config/trk/            ($TRK_CONFIG / --config)
  config.toml             user, lease, launcher templates, harness profiles, saved views
  fields.toml             field definitions
  dep-kinds.toml          dependency kinds and the type pairs they allow
  types/*.toml            statuses, transitions, resolutions, fields, requirements, routing
  agents/*.toml           instructions, briefing, tool selection, other MCP servers, harness prefs
  prompts/*.j2            MiniJinja templates
```

```sh
trk schema check                  # validate files, agents and templates
trk schema plan                   # diff against the database, affected issues, agent tool changes
trk schema apply [--map-status old=new]
trk schema graph code-change [--dot]
```

The database defaults to `$XDG_DATA_HOME/trk/trk.db` (macOS: `~/Library/Application Support/trk/trk.db`);
override with `TRK_DB` / `--db`. Worktrees go to `<data dir>/worktrees/<ID>`, session scratch to
`$XDG_STATE_HOME/trk/sessions/<uuid>/`.

## Layout

| Crate | Role |
| --- | --- |
| `trk-core` | Schema registry, config compiler, validation, claims and gates, query language, `Store` (the only writer) |
| `trk-agent` | Agent definitions, tool generation and execution, prompt rendering, harness profiles, launch spec |
| `trk-mcp` | The per-issue MCP server (rmcp, single-threaded Tokio) |
| `trk-cli` | The `trk` binary, rendering, `trk run` and the launcher templates |

`seed/` holds the default configuration, embedded into the binary for `trk init`.

## Where this differs from or extends SPEC.md

- **`trk init`** (new): writes the seed config, fills `user.email` from git, and applies it.
- **Schema additions**: `issue_type.agent` and `issue_type.tools` hold `routing.agent` and the names of
  generated transition tools (`[type.tools]` in a type file; default `move_to_<status>`). An `inbox`
  table records delivery of `--to-agent` notes (the note itself is still an event).
- **Generated tools**: transition tools exist only for transitions between intermediate statuses
  (entering the first one is `claim`; returning to open is `hand_back`). `set_<field>` setters are
  generated for every type field plus `outcome` (the spec's implementer selects `set_branch`).
- **Follow-ups** created by agents inherit required fields shared with the current issue (e.g. `repo`).
  Agents may create issues only as `discovered-from` follow-ups of an issue they claim; composed tools
  that create with another edge (e.g. `approves`) add that edge too.
- **Harness names**: the Claude Code profile is `claude-code`, matching `agent:claude-code:<id>`.
- **Launcher**: hc now has `--cwd`, so the start template uses it, plus `--no-parent` (otherwise hc
  ends the session when the launching terminal's program exits). Environment still goes through
  `sh -c`. `trk send` and briefing sends retry for ~30 s while hc reports the session is still starting.
- **omp** (verified live): reads `<worktree>/.omp/mcp.json` and shows MCP server instructions to the
  model, so no folding is needed. Its `--tools` flag hides MCP tools that are not listed, so profiles
  get a `{trk_tools:PREFIX}` placeholder (`{trk_tools:mcp__trk_}`) to list the agent's trk tools.
- **`trk reap`** decides liveness from the MCP server's pid, host and exit time, which the server
  records in `actor.meta` (with the client's name and version).
- **Errors**: two codes beyond the spec's table: `schema_invalid` (exit 4) and `not_permitted` (exit 5,
  an agent writing outside its claim).
- **Extras**: `create --dep KIND:ID`, `run -f k=v` (fields for the claim), `unclaim -m NOTE`,
  `TRK_NOW` (fixed clock for tests). List values given with `-f k=a,b` split on commas; use a JSON
  array for items that contain commas.

## Open questions found while implementing

- **code-change outcome**: the shared-fields table requires `outcome` only for research, decision and
  operation; the abridged `code-change.toml` also requires it when merged. The seed follows the table.
- **Built-in tool policies**: omp keeps `write` available even with a restricted `--tools` list, so
  `builtin_tools = "read-only"` is not fully enforced there.
- **Unverified harness profiles**: the Claude Code, Codex, OpenCode and pi profiles follow the spec but
  were not run live. Only omp (with `openai-codex/gpt-6-luna`) was.
