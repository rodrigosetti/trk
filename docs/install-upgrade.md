## Install and upgrade

### Beta compatibility

trk is beta. The JSON envelope, identified by `schema_version`, and its error codes
are intended to be stable, as are the MCP tool names. Treat `schema_version` as
the JSON API version, not the application version or the SQLite schema version.
Config keys, seed prompts, the database schema across trk versions, and the Herdr
mod may change without notice. Keep backups of `.trk/` before upgrading; do not
assume an older binary can open a database migrated by a newer one.

See the [changelog](https://github.com/rodrigosetti/trk/blob/main/CHANGELOG.md) for release changes and the
[release steps](development.md#releasing-010) for version and tag details.

### Installation

```sh
./install.sh
```

This builds a release binary and installs it to `~/.cargo/bin/trk`. It also refreshes
the Herdr mod if already installed.
trk keeps no global database; an optional user configuration can be shared across projects.
Set up each project:

```sh
cd ~/src/orch && trk install
```

`trk install` creates `.trk/` in the project root (by default, the primary git checkout
root): the database, the default config with every setting written out, and a `.gitignore`
that keeps all of it out of git. To install a project in a monorepo subdirectory, run
`trk --dir .trk install` from that subdirectory. It also installs the agent skill (see below).
Commands find the project from its subdirectories and corresponding paths in git worktrees.
Other project commands fail with `not_installed` until it has been installed.

Choose the configuration seed with these install options:

```sh
trk install --list-presets              # local and github; no installation or writes
trk install --preset local             # implicit default: land locally by verified merge
trk install --preset github            # open a GitHub PR and wait for its merge
trk install --from /path/to/team-seed   # use only this directory's files, recursively
trk install --minimal                  # project settings only; inherit shared user resources
```

The built-in `seed/` directory is embedded recursively at build time, so adding or changing
a seed file is picked up by the next build without maintaining a file list. Common files
(`config.toml`, `prompts/_rules.md.j2`, and `prompts/briefing.md.j2`) live at the seed root;
`default_type.txt` supplies the built-in fallback but is not installed.
Both presets install the same implementer, researcher and operator agents and the
`change`, `bug`, `research` and `ops` types, plus their shared instruction prompts
and both landing recipes. `change` is the default and uses an implementer
worktree; `bug` uses the same agent/worktree with guidance to reproduce first,
record the root cause in a note, and add a regression test. `research` uses the
researcher without a worktree. `ops` uses the operator without a worktree for
running-service operations; it has no type guidance or landing recipe. Its
instructions require before/after evidence, notes before state-changing commands,
and direct in-session approval for destructive or irreversible actions, not `wait`.
Repository fixes become follow-up `change` issues. The operator has full shell
access and a commented MCP example for project cloud/observability extensions,
with none enabled by default. It closes with the operation's outcome and record.
The installer also supplies the built-in `.gitignore`.
Preset `landing.txt` metadata selects `merge` for `local` or `pr` for `github` and
is not installed. A preset's files replace common files at matching paths.

`local` has no review or approval gate: `[check] verify` is its sole verification
gate, and an empty value skips verification, not the merge. Set it explicitly for
your project. `github` requires GitHub CLI (`gh`) installed and authenticated;
installation warns when `gh` is missing. Authenticate with `gh auth login` before use.

`--from DIR` instead copies a fully resolved install layout directly into `.trk/`, including
nested and binary files, with no built-in fallback or preset selection. Only regular files
and directories are accepted; symlinks and special entries are rejected. Supply your own
`.gitignore` if wanted. A `presets/` directory inside a custom source is ordinary content.
`--from`, `--preset`, and `--minimal` cannot be combined.
`--minimal` is unchanged: it writes no agents, types, or prompts and relies on the user layer.
`--list-presets` cannot be combined with install modifiers. Listing works outside a project;
its JSON data is `{"presets":["github","local"]}`.

Only `config.toml` substitutes install-time markers: `{{EMAIL_LINE}}` becomes the human email
setting (or a commented example), `{{PREFIX}}` becomes `--prefix` or the detected project
prefix, `{{DEFAULT_TYPE}}` becomes the built-in default issue type, and `{{BASE}}` becomes
the TOML-quoted detected base branch. The unquoted `{{LANDING}}` marker becomes the
TOML-quoted landing value selected by the preset; `--from` defaults it to `"merge"`.
Other files are copied unchanged, including template markers.

### Upgrading existing projects

Finish active sessions and back up `.trk/` before upgrading. After changing or pulling
this checkout, run `./install.sh` again, then select `trk install --preset local` or
`trk install --preset github` in each project (or retain its custom `--from` or
shared-config `--minimal` installation). trk does not remember the selection.
Installation applies schema migrations, refreshes the agent skill, adds missing files,
keeps existing configuration files, and lists files that differ from the seed.
For `config.toml`, only harness-profile differences count as drift, not project identity
or formatting. `--force` rewrites supplied files but still does not prune other files.

To adopt the new built-in types and landing workflow:

1. Install the chosen preset to add `types/change.toml` and the other missing resources.
   Existing `code-change`, `review`, and `reviewer` definitions/prompts and existing
   issues are preserved; installation does not rename issue types.
2. After `types/change.toml` exists, manually set `[issues] default_type = "change"`
   in `.trk/config.toml`. Existing `default_type` is kept until you change it.
3. Manually choose `[run] landing = "merge"` for local or `"pr"` for GitHub, since
   the existing config is kept. Set `[check] verify` to the desired local verification
   command; an empty value skips verification and there is no local approval gate.
4. Keep legacy definitions until their issues are migrated with
   `trk edit <ID> --type change` or an appropriate custom equivalent. Consider blocker
   dependencies and any review obligations before changing types; migration does not
   remove those dependencies. Remove legacy reviewer/review prompts and agents only
   when no remaining types, issues, or templates use them.

This repository's own `.trk/` is existing, untracked project state: use the same
procedure locally rather than editing it as part of a source upgrade.
`trk --version` prints the package version and build commit, for example
`trk 0.1.0 (<commit>)`; see [release steps](development.md#releasing-010).
Running agent sessions keep the binary they started with.

### Active sessions during upgrades

Finish active sessions before a schema-changing upgrade. Running MCP servers keep the
binary they started with; replacing the executable does not upgrade them.

If an older server encounters a newer schema, it returns `newer_database` with the database
path, `database_version` (SQLite `user_version`) and `supported_version`. These are not the
JSON envelope's `schema_version`. The database is not downgraded or reset.

Preserve uncommitted work and handoff text, install a compatible trk, and restart the
harness's MCP connection with that executable and the original project directory, issue
and actor. Call `get_task` after reconnecting. Restarting does not claim an issue or bypass
dependency/ownership checks; if the claim has expired, preserve the handoff for the human.

