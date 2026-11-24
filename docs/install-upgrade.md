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
trk install --list-presets              # default and minimal; no installation or writes
trk install --preset default            # implicit default: all three agents and issue types
trk install --preset minimal            # implementer + code-change, with their common prompts
trk install --from /path/to/team-seed   # use only this directory's files, recursively
trk install --minimal                  # project settings only; inherit shared user resources
```

The built-in `seed/` directory is embedded recursively at build time, so adding or changing
a seed file is picked up by the next build without maintaining a file list. Common files
(`config.toml`, `prompts/_rules.md.j2`, and `prompts/briefing.md.j2`) live at the seed root;
`default_type.txt` supplies the built-in fallback but is not installed.
The installer also supplies the built-in `.gitignore`. `seed/presets/default/` adds the
implementer, researcher and reviewer agents,
the code-change, research and review types, and their instruction prompts.
`seed/presets/minimal/` adds only implementer, code-change and the implementer's instructions.
A preset's files replace common files at matching paths.

`--from DIR` instead copies a fully resolved install layout directly into `.trk/`, including
nested and binary files, with no built-in fallback or preset selection. Only regular files
and directories are accepted; symlinks and special entries are rejected. Supply your own
`.gitignore` if wanted. A `presets/` directory inside a custom source is ordinary content.
`--from`, `--preset`, and `--minimal` cannot be combined. Unlike `--preset minimal`,
`--minimal` writes no agents, types, or prompts and relies on the user layer.
`--list-presets` cannot be combined with install modifiers. Listing works outside a project;
its JSON data is `{"presets":["default","minimal"]}`.

Only `config.toml` substitutes install-time markers: `{{EMAIL_LINE}}` becomes the human email
setting (or a commented example), `{{PREFIX}}` becomes `--prefix` or the detected project
prefix, `{{DEFAULT_TYPE}}` becomes the built-in default issue type, and `{{BASE}}` becomes
the TOML-quoted detected base branch. Other files are copied unchanged, including template markers.

Finish active sessions and back up `.trk/` before upgrading. After changing or pulling
this checkout, run `./install.sh` again, then `trk install` in each project, passing
the same `--minimal`, `--preset` or `--from` used for its installation.
trk does not remember that selection. It applies schema migrations, refreshes the agent skill, keeps
existing configuration files, and lists the ones that differ from the selected seed.
For `config.toml`, only harness-profile differences count as drift, not project identity
or formatting. `trk install --force` rewrites files supplied by the selected seed but does
not prune other files; switching to `--preset minimal` does not remove previously installed agents
or types. `trk --version` prints the package version and build commit, for example
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

