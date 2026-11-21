# trk

A local issue tracker and agent launcher in one Rust binary. All repositories share one SQLite
database; humans use the CLI, agents work through a per-issue MCP server, and `trk run <ID>` starts
an agent session on one issue in a persistent terminal (`hc`). See [SPEC.md](SPEC.md) for the design.

An issue has a title, a description, notes, a priority, and one of four states:

```
open → claimed → closed
                → waiting → closed   its check passed, or a human closed it
                          → open     its check failed, or a human reopened it
```

A session ends by closing the issue or leaving it **waiting**: the work is done once something
outside the session happens (a merge, a review, a decision). A waiting issue carries a shell
command, and `trk check` runs those commands until each one passes, fails, or says "not yet".

## Install and upgrade

```sh
./install.sh
```

This builds a release binary and installs it to `~/.cargo/bin/trk`. The first time, it also runs
`trk init`: the default config goes to `~/.config/trk/` and the database to
`~/Library/Application Support/trk/trk.db` on macOS (`$XDG_DATA_HOME/trk/trk.db` elsewhere).

To upgrade after changing or pulling this checkout, run `./install.sh` again. It replaces the
binary, never overwrites your config, lists the default-config files in `seed/` that differ from
yours, and runs `trk agent check`. `trk --version` prints the commit the binary was built from.
Running agent sessions keep the binary they started with.

A database written by the earlier six-type trk is refused (`old_database`): there is no migration.
Export its open issues with the old binary and recreate them with
[`scripts/import-v1.sh`](scripts/import-v1.sh), which documents both steps.

For development: `cargo build` and `cargo test --workspace`.

## Quick start

```sh
trk init                                   # writes ~/.config/trk/ and creates the database
trk project add orch --name "Orchestra"
cd ~/src/orch && trk repo add --project orch

trk create "Add claim leases" -d "Crashed agents must release their claims." -p P1
trk create "Enable leases in prod" --blocked-by orch-4kq
trk ready                                   # open, with every blocker closed
trk claim orch-4kq                          # open → claimed; records the worktree
trk note orch-4kq "Sweep runs at the start of every write"
trk close orch-4kq -m "Merged in 9f2c1e0"   # or leave it waiting, below
trk log orch-4kq
```

Every command takes `--json` (or `--format pretty|json|jsonl`). Scripts should always pass `--json`;
JSON output is the versioned API (`schema_version: 2`), and errors use the same envelope with a
stable `code`, a `hint`, and exit codes 1–5.

Pretty output colors priorities consistently (P0 red, P1 yellow, P2 cyan, P3 dim) and states
(open green, claimed yellow, waiting cyan, closed dim green). Labels and state glyphs remain present
without color. ANSI styling is enabled only for pretty output on a terminal; `NO_COLOR` or
`--no-color` disables it, and piped output is unstyled. JSON and JSONL output never contain ANSI
styling.

## Waiting and `trk check`

```sh
# Done once the branch is in main: the check does the merge.
trk wait orch-4kq -r "merge into main" \
  -c 'cd "$TRK_REPO" && [ "$(git branch --show-current)" = main ] && git merge --no-edit "$TRK_BRANCH"'

# Done once the PR is merged: the check only looks, every 10 minutes.
trk wait orch-4kq -r "PR 12 merged" --every 10m \
  -c 'case "$(gh pr view 12 --json state -q .state)" in MERGED) exit 0;; OPEN) exit 75;; *) exit 1;; esac'

# Waiting on a person: no check. Resolve it with `trk close` or `trk reopen -m "the answer"`.
trk wait orch-4kq -r "SQLite or Dolt?"

trk check                 # run the checks that are due, one at a time
trk check --watch         # keep doing that; leave it running in a spare terminal
trk check orch-4kq        # run one now
trk check --dry-run       # show what would run, and where
trk list state=waiting    # what is waiting, on what, and when it is checked next
```

The check runs with `sh -c` in the worktree the issue was claimed in, with `TRK_ISSUE`,
`TRK_WORKTREE`, `TRK_REPO` (the main checkout) and `TRK_BRANCH` set. Its exit code decides:

| Exit | Meaning | Result |
| --- | --- | --- |
| 0 | pass | the issue closes as done |
| 75 | not yet | still waiting; checked again after `--every` (default `[check] interval`, 5m) |
| anything else, or a timeout | error | the issue returns to open with the command's output in a note |

Three properties follow from that:

- **A waiting issue still blocks.** What depends on it becomes ready when the work has landed, not
  when the session stopped.
- **Checks are a merge queue.** Only one `trk check` runs at a time and it runs checks one after
  another, so two sessions never merge into main at once.
- **Failures come back as work.** A failed check reopens the issue; the next session reads the
  output in the notes. A command that is missing or times out (`[check] timeout`, 10m) fails
  rather than waiting forever.

A check is written by an agent and runs later, unattended, as you. `trk show` always prints it in
full, and `trk check --dry-run` shows the queue before anything runs.

## Agent sessions

```sh
trk run orch-4kq                 # claim as agent:<harness>:<uuid>, create the worktree, start in hc
trk runs                         # active agent claims: agent, harness, terminal, lease, MCP liveness
trk attach orch-4kq              # take over the terminal
trk note orch-4kq --to-agent "Also update the docs"   # delivered with the agent's next tool result
trk send orch-4kq "Wrap up and finish"                # typed into the session
trk reap                         # release claims whose MCP server has gone, with a note
trk run orch-4kq --dry-run       # print the launch spec; claims nothing
trk agent preview implementer --issue orch-4kq        # instructions, briefing, tool schemas
```

The agent for a run is `--agent`, else the issue's `agent`, else `[run] default_agent`
(`implementer`). The harness is `--harness`, else the agent's first installed preference. The seed
config prefers `omp` running `openai-codex/gpt-6-luna`. The worktree is
`<data dir>/worktrees/<ID>` on a branch named after the issue, so a later session on the same
issue finds the earlier work.

The MCP server (`trk mcp --issue … --actor … --agent …`, started by the harness) serves exactly one
issue with six tools: `get_task`, `add_note`, `report_followup`, `hand_back`, `close` and `wait`.
It renews the claim's lease while it runs. The briefing carries the issue's description and every
note, which is how an issue that came back from a failed check explains itself.

With `[launcher] herdr = true`, each run opens a **new Herdr space**, named with the issue's title
and rooted in its worktree. Its first terminal runs the agent directly; the launching space and
its tabs are left intact. This works from any terminal, not only one already in Herdr.
trk confirms the space's title, folder and agent pane through `herdr-app ls`; JSON includes
`placement` (`visible` or `failed`) and the actual terminal address used by `trk attach` and `trk send`.

If the UI is unavailable before launch, trk uses the configured `start` template headlessly and
warns. Once space creation is attempted, an uncertain response retains the claim and never starts
a duplicate session. A two-second observation timeout warns without ending the running session.

Omit `herdr` or use `herdr = false` for a headless/custom launcher; the default hc template uses
`--no-parent` to keep sessions independent. Herdr launching requires `hc` and `herdr-app` on PATH;
no IDs belong in the config.

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

## Configuration

`trk init` writes the seed configuration (keep it in its own git repository):

```
~/.config/trk/            ($TRK_CONFIG / --config)
  config.toml             user, leases, check interval and timeout, default agent,
                          launcher templates, harness profiles, saved views
  agents/*.toml           instructions, briefing, other MCP servers, harness preferences
  prompts/*.j2            MiniJinja templates
```

There is no schema to apply: states, the close reasons (`done`, `abandoned`), priorities and the
one dependency kind (`blocks`) are fixed. Agents and prompts are read when a session starts;
`trk agent check` validates them.

The database defaults to `$XDG_DATA_HOME/trk/trk.db` (macOS: `~/Library/Application Support/trk/trk.db`);
override with `TRK_DB` / `--db`. Worktrees go to `<data dir>/worktrees/<ID>`, session scratch to
`$XDG_STATE_HOME/trk/sessions/<uuid>/`.

## Layout

| Crate | Role |
| --- | --- |
| `trk-core` | The state machine, claims and leases, the waiting queue, query language, `Store` (the only writer) |
| `trk-agent` | Agent definitions, the six trk tools and their executor, prompt rendering, harness profiles, launch spec |
| `trk-mcp` | The per-issue MCP server (rmcp, single-threaded Tokio) |
| `trk-cli` | The `trk` binary, rendering, `trk check`, `trk run` and the launcher templates |

`seed/` holds the default configuration, embedded into the binary for `trk init`.

## Notes on the implementation

- **Harness names**: the Claude Code profile is `claude-code`, matching `agent:claude-code:<id>`.
  A harness is registered the first time one of its sessions acts.
- **omp** (verified live): reads `<worktree>/.omp/mcp.json` and shows MCP server instructions to the
  model, so no folding is needed. Its `--tools` flag hides MCP tools that are not listed, so profiles
  get a `{trk_tools:PREFIX}` placeholder (`{trk_tools:mcp__trk_}`) to list the agent's trk tools.
  It keeps `write` available even with a restricted `--tools` list, so `builtin_tools = "read-only"`
  is not fully enforced there.
- **Unverified harness profiles**: the Claude Code, Codex, OpenCode and pi profiles were not run live.
- **`trk reap`** decides liveness from the MCP server's pid, host and exit time, which the server
  records in `actor.meta` (with the client's name and version).
- **`trk check` lock**: a row holding the pid and host of the running pass. A holder whose process
  is gone on this machine is replaced; a holder on another host is never replaced automatically.
- **`TRK_NOW`** fixes the clock, for tests.
