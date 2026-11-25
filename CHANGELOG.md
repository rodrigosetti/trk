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
- Per-panel Herdr dashboard sorting by priority, updated time, creation time, or type, with direction toggles, `o`/`O` shortcuts, and an optional initial `sort` prop.
- Epic query filtering (`epic=""` for unassigned issues), epic sorting with unassigned issues last in both directions, and epic names in dashboard issue and dependency-edge JSON; list/ready JSON retains each issue's optional epic.
- Semantic terminal colors for `trk check` and `--watch`: highlighted issue IDs, check activity, successes, waiting/retries, warnings, and failures, respecting existing color-disable and JSON behavior.
- Explicitly curated epics with create/list/rename/archive commands, cascading names, optional issue assignment and clearing in CLI/editor, archived-assignment validation, complete timeline events, and forward database migration.
- Agent epic context: briefings and templates expose membership and active epic descriptions; MCP follow-ups can select existing active epics, with no epic-management tools. Interactive skill guidance covers explicit epic curation and omitting assignments when none fits.

### Fixed

- Restored GC classification of historical claim paths: local primary-checkout claims own no disposable Git resources, and absent outside-root paths are reported as absent. Existing outside-root worktrees and branches held by stale Git registrations remain protected.
- Human unclaim permission errors now include the issue-specific `trk unclaim <ID> --force` recovery command; Herdr shows CLI error hints in a wrapping status footer.
- Working-session spinners in the Herdr dashboard now prefix the issue title instead of occupying a separate row; titles still wrap and links and other statuses remain below.
- The Herdr dashboard's wide list/details split now starts at 50/50 instead of a fixed list width, retaining user resizing and pane minimums.
- Restored CI Clippy checks with narrowly scoped argument-count expectations on GC helpers, preserving cleanup safety gates and opt-in legacy recovery behavior.

### Beta compatibility and upgrades

- The versioned JSON envelope (`schema_version` and error codes) and MCP tool names are intended to be stable.
- Config keys, seed prompts, the database schema across versions, and the Herdr mod may change without notice.
- Finish active sessions and back up `.trk/` before upgrading. Update the source checkout, re-run `./install.sh`, then run `trk install` in each project using its original seed options. Database migrations are forward-only; do not assume downgrade compatibility.

See the [upgrade manual](docs/install-upgrade.md) and [human release steps](docs/development.md#releasing-010).
