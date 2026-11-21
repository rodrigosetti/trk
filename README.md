# trk

A local issue tracker and agent launcher in one Rust binary. All repositories share one SQLite
database; humans use the CLI, agents work through a per-issue MCP server, and `trk run <ID>` starts
an agent session on one issue in a persistent terminal (`hc`). See [SPEC.md](SPEC.md) for the design.

## Install and upgrade

```sh
./install.sh
```

This builds a release binary and installs it to `~/.cargo/bin/trk`. The first time, it also runs
`trk init`: the default config goes to `~/.config/trk/` and the database to
`~/Library/Application Support/trk/trk.db` on macOS (`$XDG_DATA_HOME/trk/trk.db` elsewhere).

To upgrade after changing or pulling this checkout, run `./install.sh` again. It replaces the binary
(the database migrates itself on next use), never overwrites your config, lists the default-config
files in `seed/` that differ from yours, and shows `trk schema plan`. `trk --version` prints the
commit the binary was built from. Running agent sessions keep the binary they started with.

For development: `cargo build` and `cargo test --workspace`.

## Agent skill

`trk skill install` installs a skill (from [`skills/trk/`](skills/trk/SKILL.md), embedded in the
binary) that teaches your interactive agent sessions to use the trk CLI.

```sh
trk skill install            # this repository: .agents/skills/trk (omp, Codex) + .claude/skills/trk (Claude Code)
trk skill install --global   # ~/.agents/skills/trk + ~/.claude/skills/trk
trk skill install --dir DIR  # DIR/trk, for any other skills directory
trk skill status             # where it is installed and whether it matches this binary
trk skill uninstall [--global]
```

Sessions started by `trk run` should reach trk only through their MCP server (see SPEC.md). That is
why the default is project-local and kept out of git with `.git/info/exclude`: the worktrees
`trk run` creates do not contain it, so those sessions do not see it. `trk run` warns when a repo
tracks the skill anyway. A `--global` skill is visible everywhere; the omp profile hides it from
`trk run` sessions with a per-session `--config` overlay (`skills.ignoredSkills: [trk]`), and the
skill itself tells an agent that has trk MCP tools to stop. Re-run `trk skill install` after
upgrading trk to refresh a copy.

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

With `[launcher] herdr = true`, each run opens a **new Herdr space**, named with the issue's title
and rooted in its worktree. Its first terminal runs the agent directly; the launching space and
its tabs are left intact. This works from any terminal, not only one already in Herdr.
trk confirms the space's title, folder and agent pane through `herdr-app ls`; JSON includes
`placement` (`visible` or `failed`) and the actual terminal address used by `trk attach` and `trk send`.

If the UI is unavailable before launch, trk uses the configured `start` template headlessly and
warns. Once space creation is attempted, an uncertain response retains the claim and never starts
a duplicate session. A two-second observation timeout warns without ending the running session.

For existing installs, add `herdr = true` under `[launcher]` after upgrading the binary. Omit it or
use `herdr = false` for a headless/custom launcher; the default hc template uses `--no-parent` to
keep sessions independent. Herdr launching requires `hc` and `herdr-app` on PATH; no IDs belong
in the config.

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
- **Launcher**: hc has `--cwd` and `--no-parent`; environment still goes through `sh -c`.
  Optional `herdr = true` placement is described above. `trk send` and briefing sends retry for
  ~30 s while hc reports the session is still starting.
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
