# trk

A local issue tracker and coding-agent launcher in one Rust binary. Project state lives
in `.trk/`; humans use the CLI, and agents use a per-issue MCP server. `trk run` claims
an issue, prepares its Git worktree, and starts an agent session. Waiting checks form
a serialized queue that can verify and land the work.

**Beta software.** Keep backups of project state. The versioned JSON envelope
(`schema_version` and error codes) and MCP tool names are intended to be stable.
Config keys, seed prompts, the database schema across versions, and the Herdr mod
may change without notice.

**Upgrading:** finish active sessions first, update your source checkout, re-run
`./install.sh`, then run `trk install --preset local` (or `github`) in each project.
See the [upgrade manual](docs/install-upgrade.md), [changelog](CHANGELOG.md), and
[release steps](docs/development.md#releasing-010).

## Prerequisites

- Rust **1.89+** and Cargo ([rustup](https://rustup.rs)).
- Git, with `user.name` and `user.email` configured.
- An available launcher: tmux, [Herdr](https://herdr.dev), or `hc`.
- An installed, authenticated agent harness, such as Claude Code (`claude`),
  omp, Codex, OpenCode, or pi.

## Install

```sh
git clone https://github.com/rodrigosetti/trk.git
cd trk
./install.sh
```

The binary goes in `~/.cargo/bin`; put that directory on `PATH`.

## Quick start

```sh
cd /path/to/your/git-repo
trk install                            # implicit --preset local
# Set [check] verify in .trk/config.toml to your project's test command.
trk create "Fix the login timeout" --type bug -d "Reproduce the timeout and fix its cause." --run
trk runs
trk check --watch
```

Installation detects available launchers and harnesses. `--run` saves the issue and
starts its agent in a dedicated worktree. Leave `trk check --watch` running in a spare
terminal: it runs waiting checks and returns failures to the session when possible.
Use `trk dashboard` for a live overview.

The `local` preset merges into the base branch with no review or approval gate;
`[check] verify` is its sole verification gate, and empty skips verification.
Use `trk install --preset github` for PR landing; it requires installed,
authenticated `gh` (installation warns if missing). Both presets provide `change`
(default) and `bug` through the implementer in worktrees, `research` through
the researcher, and `ops` through the operator, both without worktrees. Bug guidance
requires reproducing first, recording the root cause, and a regression test.
Ops covers running-service operations with a before/after record, notes before
state-changing commands, and direct user approval for destructive or irreversible
actions. Repository fixes become follow-up `change` issues; ops closes with the
operation's outcome, without a landing recipe.
Existing configurations and legacy types are kept; follow the
[migration procedure](docs/install-upgrade.md#upgrading-existing-projects) to change
the default type and landing setting manually.
For a no-check wait on a decision, the same session can continue after reporting an
actual human chat reply; [chat resume](docs/waiting-and-checks.md#same-session-chat-resume)
is enabled by default and can be disabled with `[wait] chat_resume = false`.

Without a check, a human can also close the issue, reopen it for a new session,
or run `trk resume <ID> -m "<your decision>"` to continue the live waiting session.
When chat resume is disabled, a chat reply alone will not continue the work: the
session must end its question with the exact issue-specific command returned by
`wait`, and repeat that command if the human answers only in chat.

**Safety:** agents write checks that run later, unattended, **as you**. Inspect an
issue's full command with `trk show <ID>` or preview the queue with
`trk check --dry-run` before running it.

For Claude Code, see the [unattended permission policy](docs/configuration.md#claude-code-permissions)
and [live verification notes](docs/development.md#notes-on-the-implementation).

## Reference

Read the [published manual](https://rodrigosetti.github.io/trk/) for the complete guide,
or browse the chapter sources below.

- [Issues and CLI basics](docs/issues.md)
- [Queries and saved views](docs/queries.md)
- [Installation and upgrades](docs/install-upgrade.md)
- [Waiting, checks, and session resume](docs/waiting-and-checks.md)
- [Sessions and the agent skill](docs/sessions.md)
- [Launcher profiles](docs/launchers.md)
- [Configuration, harnesses, and templates](docs/configuration.md)
- [Herdr dashboard](docs/herdr.md)
- [Cleanup](docs/cleanup.md)
- [Development and implementation notes](docs/development.md)
- [Design and state machine](docs/design.md)
- [MCP tool contract](docs/mcp.md)
- [JSON API and errors](docs/json.md)

Licensed under the [MIT License](LICENSE).
