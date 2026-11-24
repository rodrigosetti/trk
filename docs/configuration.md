## Configuration

Project state stays in `.trk/` at its root, written by `trk install`:

```
.trk/                     (found from the working directory; $TRK_DIR / --dir override it)
  .gitignore              "*": nothing here is tracked until you edit it
  trk.db                  the database
  config.toml             user, issue id prefix, leases, check interval and timeout, worktrees,
                          launcher templates, harness profiles, saved views
  agents/*.toml           instructions, briefing, other MCP servers, harness preferences
  types/*.toml            name, description, agent routing, optional guidance
  prompts/*.j2            MiniJinja templates
  worktrees/<ID>/         git worktrees made by `trk run` ([run] worktrees)
  sessions/<uuid>/        per-session files
  checks/                 logs of running checks
```

Settings resolve in order: **built-in defaults < user < project**. The optional user
directory is `$TRK_USER_DIR`, otherwise `$XDG_CONFIG_HOME/trk`, otherwise `~/.config/trk`.
It has the same configuration layout: `config.toml`, `agents/`, `types/`, and `prompts/`;
no database or session state is stored there. A missing user directory changes nothing.
`trk whoami` shows the resolved user directory.

`config.toml` merges per key: tables merge recursively, arrays and scalar values replace
the lower layer. Named `[harness.<name>]` profiles and `[views.<name>]` views therefore
inherit unspecified keys. Project `[user]` identity and `[issues] prefix` take precedence;
when absent they can inherit from the user layer. Built-in defaults fill absent settings,
but do not supply agent/type/template files or harness profiles.
Launcher operations are optional. Omitted skill settings use `.agents/skills/trk` without links.
`[issues] prefix` names new issues (`<prefix>-<base32>`); normal installation writes the
directory name, or `trk install --prefix NAME`. Changing it later only affects new issues.
`[issues] default_type` selects the type for CLI and editor creation and MCP follow-ups
when no explicit type is supplied; `trk whoami` reports it. The default is `change`.
Set it to your project's type name (for example,
`default_type = "task"` with `types/task.toml`) before removing or renaming the seed type.

Agents, types, and prompts resolve by filename, **project first, then user**, without
merging file contents. Template `{% include "prompts/_rules.md.j2" %}` uses the same
search order, even when the including template comes from the other layer.
The MCP server uses these same layers when started with `TRK_DIR`.

Normal `trk install` still writes complete project settings and all seed files.
Those project copies override shared settings; remove individual keys/files to inherit.
For a new project using shared configuration, use `trk install --minimal`: it writes
only project identity, issue prefix and base branch, plus the database, ignore file and
CLI skill. Populate the user directory with the shared profiles, agents, types and prompts
first. Minimal installation does not delete existing project resources.

`[run] base` selects the base branch, for example:

```toml
[run]
base = "develop"
landing = "merge"
```

When writing a new config, `trk install` detects the branch named by `origin/HEAD`,
then the current branch of the primary checkout, and finally falls back to `main`.
When `base` is omitted, runtime uses the same repository detection. An explicit `base`
takes precedence. Waiting checks receive the resolved branch as `TRK_BASE`; instruction
and briefing prompt templates can use `{{ base }}`.

`[run] landing` selects `prompts/_landing.<value>.md.j2`. The seed provides `"merge"`
for verified local landing into the base branch in the primary checkout, and `"pr"`
for opening a GitHub pull request against that branch and waiting on its merge.
New configs select `"merge"` with the implicit `local` preset or `"pr"` with
`--preset github`. Existing configs are preserved: switching presets does not change
their landing setting. Only the implementer includes the selected recipe; researchers
and operators receive the generic tracker rules. Local landing has no review or approval gate:
`[check] verify` is the sole verification gate, and an empty value skips verification
while still merging. GitHub landing requires installed, authenticated `gh`; install
warns if it is missing.
To add another workflow, create a template such as `.trk/prompts/_landing.gitlab.md.j2`
and set `landing = "gitlab"`—no code change or edit to shared rules is needed.
`trk agent check` reports a configured landing value with no matching template.

| Check setting | Default, also when omitted | Meaning |
| --- | --- | --- |
| `[check] gc` | `true` | Automatically sweep closed-as-done resources after non-ID passes; `--no-gc` overrides it for a run/watch |
| `[check] gc_grace` | `"1h"` | Minimum age since closure for automatic cleanup, not a deadline |

There is no schema to apply: states, the close reasons (`done`, `abandoned`), priorities and the
one dependency kind (`blocks`) are fixed. Agents and prompts are read when a session starts;
`trk agent check` validates them. `trk agents`, `trk agent check`, and `trk run --dry-run`
report the project/user origins of agents, types and templates.

### Type guidance

Both presets install `change` (the default) and `bug`, routed to `implementer` with
issue worktrees, `research` routed to `researcher`, and `ops` routed to `operator`,
both without worktrees. `ops` has no guidance snippet: service-operation rules
live in the operator instructions. The operator has full shell access through
`builtin_tools = "default"` and a commented cloud/observability `[[mcp]]` example
in `agents/operator.toml`; no extra MCP servers are enabled.
The built-in `bug` guidance requires reproducing first, a root-cause note, and a
regression test. A type can add task-specific instructions without another agent:

```toml
name = "bug"
description = "Fix a defect"
agent = "implementer"
guidance = """
Reproduce the defect in {{ project.name }} before changing code.
Add a regression test for the reported behavior.
Explain the root cause and verify the fix.
"""
```

`guidance` is optional inline TOML text rendered as MiniJinja with the same context
as agent instructions (see the context table below). For a reusable external snippet,
use `guidance = '{% include "prompts/_bug.md.j2" %}'` instead. Includes use the same
project-first, then user template loader. Type files resolve as whole files: a project
`types/bug.toml` replaces the user copy, including its guidance; fields do not merge.

The rendered snippet is appended automatically to agent instructions under
`## Type guidance`, including when instructions are folded into the briefing.
`trk run --agent NAME` changes the agent, not the issue's type or its guidance.
Omitting `guidance` preserves the previous prompt output without an extra heading.

Use `trk agent preview <agent> --issue ID` or `trk run ID --dry-run --json` to inspect
the rendered instructions. Preview, dry-run and `trk agent check` report guidance
provenance as `types/<name>.toml#guidance` resolved to the type file, alongside any
included templates. `trk agent check` validates guidance syntax, rendering and includes
for every actual routed type; a broken snippet is an error, not ignored.

Set the default harness preference once in either layer's `config.toml`:

```toml
[harness]
prefer = ["omp", "claude-code"]
```

Agents inherit this list unless their own `[harness] prefer` is present. An explicit
`prefer = []` disables the inherited ordering; `--harness` overrides selection.
Seed agents inherit the preference from the project config rather than repeating it.

New built-in configs detect commands on PATH and keep installed harnesses in this
order: `omp`, `claude-code` (`claude`), `codex`, `opencode`, `pi`. Install reports both
launcher and harness detection and warns if either list is empty. All harness profiles
remain available for explicit selection. Later installs preserve existing configs and
preferences unless `--force` is given; `--from` sources and `--minimal` shared-config
installs do not filter preferences.

Agents can choose models and add arguments without duplicating harness profiles:

```toml
# .trk/agents/implementer.toml
[harness]
prefer = ["omp", "claude-code"]
model = { omp = "provider/implementer-model", claude-code = "implementer-model" }
args = { omp = ["--thinking", "high"] }
```

`harness.model` also accepts a string, applying it to every harness. A missing entry
in a model table falls back to the profile default. `harness.args` is a table of
harness names to argument lists; only the selected harness's list is used.

Agent `harness.builtin_tools` levels are open-ended: `default` is reserved and adds
no restriction arguments; any other name (for example `no-network` or `plan-only`)
must be defined by at least one profile's `builtin_tools.<level>` argument list.
An empty list explicitly declares support without extra arguments.
`trk agent check` reports undefined levels with the available names and warns
when a preferred harness lacks a level. `trk run` refuses to launch when the
selected profile lacks it, including explicit `--harness` selections; it no
longer silently launches unrestricted. Define the level's enforcement flags in
that profile or select a profile that supports it.

### Claude Code permissions

The seed uses `--permission-mode dontAsk` with `--allowedTools` for Bash, Edit,
Write, NotebookEdit and the exact trk MCP tools exposed to that session. Other
operations that need approval are denied rather than prompting; extra agent MCP
servers are not automatically approved. This avoids depending on the user's
permission mode or a model-specific auto classifier, without using
`--dangerously-skip-permissions`.

This is still broad authority: Bash can execute arbitrary programs with your
account's access. On the verified Claude Code version, `dontAsk` also skips the
first-use workspace trust dialog. Launch only in repositories you trust; use an
isolated environment for untrusted code. Narrow the Bash allow rules in your
project profile if its commands can be enumerated, accepting that other commands
will be denied. Existing installations keep their profile; copy the Claude Code
section from `seed/config.toml` to adopt these changes.

Claude's `read-only` level exposes Read/Grep/Glob plus ToolSearch; `none` exposes
only ToolSearch, not file, shell or delegation tools. ToolSearch is a deliberate
discovery-only exception: with literal `--tools ""`, Claude 2.1.331 can start its
first turn without any MCP tools and cannot call `get_task` or `wait`. Both
restricted levels retain discovery of the agent's trk MCP tools without relying
on a delayed launcher briefing (which `--fg` does not send automatically).
These levels restrict built-ins, not MCP side effects or shell checks submitted
through `wait`; they are not filesystem sandboxes.

### Models and launch arguments

```toml
# .trk/config.toml
[harness.omp]
command = "omp"
args = ["--config", "{{ session_dir }}/omp.yml", "{builtin_tools_args}", "{model_args}", "{agent_args}", "{{ briefing }}"]
model = "openai-codex/gpt-6-luna"
model_args = ["--model", "{{ model }}"]
```

`model` is optional; `model_args` defaults to an empty list and is inserted only
when a model resolves. Set `model_args` to the harness's model-selection flags.
All seeded profiles provide it; only omp sets a default model.
`{{ model }}` expands to the resolved model (or an empty string if unset) in profile
arguments, environment and files, and agent arguments.
Standalone `args` elements `{builtin_tools_args}`, `{model_args}`, and `{agent_args}`
expand in place to zero or more argv entries. Place them before a subcommand or
after `--` as the harness requires.
Each group without a marker goes before the first argument
using `briefing`, or at the end when none does. Unmarked groups retain the order
of built-in tool restrictions, model arguments, then agent arguments.
Arguments remain separate argv entries; they are not shell-split.

`trk run --dry-run --json` includes the resolved `model` (or null) and `argv`.
`trk agents` shows effective models per profile; its JSON includes the agent's
configured `model`, effective `models` by harness (null means harness default),
and extra `args`.

Agent instructions, type guidance and briefings are MiniJinja templates with the same
context in `trk run`, the MCP server and `trk agent preview`:

| Variable | Value |
| --- | --- |
| `issue` | Issue object, including dependencies and derived fields; briefings also include notes |
| `project.name`, `project.root`, `project.prefix` | Project root directory name, root path (not the session worktree), and configured issue ID prefix |
| `type.name`, `type.description` | Resolved `types/<type>.toml` metadata, even with an agent override |
| `config.check.interval`, `config.check.timeout`, `config.check.max_wakes` | Configured check durations and numeric wake cap |
| `config.wait.chat_resume` | Boolean enabling same-session chat resume for waits without a check (default `true`) |
| `config.claims.agent_lease`, `config.run.worktrees` | Configured lease duration and worktree directory |
| `config.run.landing` | Landing recipe name, used by the implementer's dynamic template include |
| `agent.name`, `agent.description` | Selected agent metadata |
| `tools` | List of available tools, each with `name` and `description` |
| `worktree`, `actor`, `harness` | Session worktree path, actor and harness name; worktree/harness may be null |
| `base` | Resolved `[run] base` branch |
| `fold_instructions`, `instructions` | Whether instructions are folded into the briefing, and their rendered text (otherwise null) |

Durations and `config.run.worktrees` retain their configured strings (for example `5m`
and `.trk/worktrees`), without path expansion. Example: `{{ project.name }}:
{{ type.description }}; wake cap {{ config.check.max_wakes }}`.
`trk agent check` renders each actual routed type, including its guidance, with a
sample issue and its resolved agent. It also retains the representative agent render
with sample issue/type/project values (project root `/tmp/project`, type `change`
with description `Sample type description`), using the project's configured settings.

The `wait` tool advertises the configured check interval and wake cap; with
`max_wakes = 0`, failures reopen the issue instead of waking the session.
Seed waiting rules render these settings from the template context. Tool descriptions
cover the check mechanism only; branch landing and pull-request policy live in the
instruction templates, where projects can change them.

`[wait] chat_resume = true` is the default, including when `[wait]` is omitted.
Set `chat_resume = false` under `[wait]` in `.trk/config.toml` to require human
CLI resume instead of allowing the waiting agent to report an actual human chat
reply. This applies only to no-check waits; checked waits always refuse MCP chat
resume, preserving the automatic `[check] max_wakes` defense. Both chat and human
CLI resume preserve `wake_count`. See [waiting and checks](waiting-and-checks.md#same-session-chat-resume).

Harness profiles in `config.toml` use a separate MiniJinja context. Every command,
argument, environment value, file path/content and optional argument is
rendered once; substituted values are never expanded recursively.

| Variable | Value |
| --- | --- |
| `name`, `issue`, `actor`, `agent`, `harness` | Terminal name, issue ID, actor, agent name and harness name (strings) |
| `model` | Resolved model string, or an empty string when unset |
| `cwd`, `worktree`, `session_dir` | Launch directory, worktree (falls back to `cwd`) and session scratch directory |
| `briefing`, `briefing_file`, `instructions`, `instructions_file` | Rendered prompt text and corresponding file paths |
| `mcp_json_file`, `trk_command` | Generic pretty MCP JSON file path and trk executable path |
| `trk_args`, `trk_env`, `trk_tools` | trk MCP argument list, environment map and available trk tool names |
| `mcp_servers` | Server list, trk first; each has `name`, `command`, `args` and `env` |

Server environments already expand `${env:NAME}` from trk's environment.

Use `{{ value | tojson }}` for compact JSON without HTML escaping and
`{{ value | toml }}` for inline TOML (quoted strings, comma-space-separated arrays,
and tables such as `{key = value}`). MiniJinja loops, expressions and built-in
`map`/`join` filters let each profile define its MCP dialect in configuration;
`mcp_json_file` always points to the generic pretty JSON, not a harness dialect.
The seed profiles include generic MCP and OpenCode examples.

The Codex seed emits individual overrides for trk and every agent `[[mcp]]`
server, without replacing other servers configured in Codex. `args_json` renders
a JSON array of strings prepended to the static `args`; this allows loops to
generate a variable number of flags without shell splitting:

```toml
[harness.codex]
command = "codex"
args_json = '''[
{% for server in mcp_servers %}
{% if not loop.first %},{% endif %}
{% for field in ["command", "args", "env"] %}
{% if not loop.first %},{% endif %}
"-c", {{ ("mcp_servers." ~ server.name ~ "." ~ field ~ "=" ~ (server[field] | toml)) | tojson }}
{% endfor %}
{% endfor %}
]'''
args = ["{builtin_tools_args}", "{model_args}", "{agent_args}", "{{ briefing }}"]
model_args = ["--model", "{{ model }}"]
builtin_tools = { read-only = ["--sandbox", "read-only"] }
```

`trk install` reports `config.toml` in `differ` when its harness profiles differ
from the seed, ignoring project identity and formatting. Existing config is kept
unless `--force` is explicitly requested; copy the profile above to update Codex
without replacing project settings.

Launcher templates retain their existing single-brace syntax.

There is no cross-project view: `trk list`, `trk runs` and `trk check --watch` act on the
project of the working directory, so each project needs its own `trk check --watch`.

