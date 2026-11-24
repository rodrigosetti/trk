# Changelog

## 0.1.0 — release pending

First public beta; the release date and `v0.1.0` tag will be set by a human at release time.

### Added

- Local, per-project SQLite issue tracking with a CLI and per-issue MCP server.
- Agent launching in dedicated Git worktrees, with claims, leases, and session tracking.
- Serialized waiting checks for verification and landing, plus a live dashboard and Herdr integration.
- Embedded installation seeds, configurable agents, harnesses, launchers, and prompts.
- Explicit ID-scoped `trk gc --legacy-hc` recovery for independently verified historical hc sessions missing launcher profiles, with finished-session identity checks and append-only GC receipts.
- Replaced `default`/`minimal` presets with `local` (implicit merge) and `github` (PR), sharing implementer/researcher agents and `change` (default), `bug`, and `research` types. Existing config and legacy resources are preserved; see the upgrade manual for migration.
- Added the `ops` type and full-shell `operator` agent to both presets for recorded, verified running-service operations without worktrees or landing recipes. Destructive/irreversible work requires direct in-session approval; repository fixes become follow-up `change` issues.
- Configuration reference links and user-layer guidance in installed config and skill files, plus a manual link in CLI help.
- Native draggable list/details split in the Herdr dashboard, retaining pane sizes while browsing issues.
- Theme-colored issue-type badges in Herdr dashboard rows and details, with readable labels and stable colors for custom types.
- Aligned Herdr dashboard issue IDs, type badges, and text across sections, with wrapped titles and custom type names that stay within narrow list panes.

### Beta compatibility and upgrades

- The versioned JSON envelope (`schema_version` and error codes) and MCP tool names are intended to be stable.
- Config keys, seed prompts, the database schema across versions, and the Herdr mod may change without notice.
- Finish active sessions and back up `.trk/` before upgrading. Update the source checkout, re-run `./install.sh`, then run `trk install` in each project using its original seed options. Database migrations are forward-only; do not assume downgrade compatibility.

See the [upgrade manual](docs/install-upgrade.md) and [human release steps](docs/development.md#releasing-010).
