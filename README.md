# trk

A local issue tracker and coding-agent launcher in one Rust binary. Project state lives
in `.trk/`; humans use the CLI, and agents use a per-issue MCP server. `trk run` claims
an issue, prepares its Git worktree, and starts an agent session. Waiting checks form
a serialized queue that can verify and land the work.

**Beta software.** Use with care and keep backups of project state.

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
trk install
# Set [check] verify in .trk/config.toml to your project's test command.
trk create "Fix the login timeout" -d "Reproduce the timeout and fix its cause." --run
trk runs
trk check --watch
```

Installation detects available launchers and harnesses. `--run` saves the issue and
starts its agent in a dedicated worktree. Leave `trk check --watch` running in a spare
terminal: it runs waiting checks and returns failures to the session when possible.
Use `trk dashboard` for a live overview.

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
- [Waiting, checks, and human-authorized resume](docs/waiting-and-checks.md)
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
