# Changelog

## 0.1.0 — release pending

First public beta; the release date and `v0.1.0` tag will be set by a human at release time.

### Added

- Local, per-project SQLite issue tracking with a CLI and per-issue MCP server.
- Agent launching in dedicated Git worktrees, with claims, leases, and session tracking.
- Serialized waiting checks for verification and landing, plus a live dashboard and Herdr integration.
- Per-wait `--on-pass wake` (MCP `on_pass: "wake"`) continues the same live session after a passing check for pending work, without consuming failure wakes; default checks still close, and unavailable continuation reopens with a pending-work note.
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
- Optional Herdr dashboard epic grouping within all five state groups, with ranked epic buckets, counts, unassigned issues last, an `e` toggle and initial `group` prop; flat rows show epic tags and blocked hints identify cross-epic blockers.
- Optional terminal dashboard epic grouping with an `E` toggle, per-state bucket counts and priority/age ranking, unassigned issues last, selection-preserving navigation, responsive dim flat-row epic tags, and cross-epic blocker hints.
- A single installed Herdr `trk` layout with a fixed first dashboard tab for tracked non-worktree projects, a machine-owned `trk-tabs` companion, layout-scoped keyboard routing, and safe backups/migration of the former split/sidebar layouts.
- Independently collapsible Herdr epic subgroups, including No epic, with per-state fold memory across refresh/sort/group toggles, disclosure arrows separate from epic names, and keyboard navigation that skips hidden rows.
- Semantic terminal colors for `trk check` and `--watch`: highlighted issue IDs, check activity, successes, waiting/retries, warnings, and failures, respecting existing color-disable and JSON behavior.
- Explicitly curated epics with create/list/rename/archive commands, cascading names, optional issue assignment and clearing in CLI/editor, archived-assignment validation, complete timeline events, and forward database migration.
- Agent epic context: briefings and templates expose membership and active epic descriptions; MCP follow-ups can select existing active epics, with no epic-management tools. Interactive skill guidance covers explicit epic curation and omitting assignments when none fits.
- Automatic consistent database snapshots before schema migrations, a stderr notice with the backup path, and retention of five automatic snapshots without pruning manual backups.

### Changed

- Split CLI command implementations and integration tests into topic modules; run decisions and check-result handoffs now use typed serde views without changing CLI output or JSON.

### Fixed

- Waiting checks now retain only a bounded 4096-byte stdout/stderr tail, mark truncated output in JSON and notes, avoid per-check log files on all exit paths, and remove legacy logs whose owner PID is gone.
- Fixed Herdr Run confirmations silently refusing to launch after an earlier command completed across a project or fixed-tab context change; pending commands no longer arm another confirmation.
- Launcher templates now expand placeholders only once, preserving literal placeholders in issue content and preventing re-expansion from corrupting shell quoting.
- The seeded hc launcher now requests JSON explicitly and binds its `address` rather than the entire JSON object; other launcher stdout contracts are unchanged. Invalid JSON launch responses retain the claim to prevent duplicate sessions.
- Brighter, larger Herdr state and epic headings in dark and light themes, with every top-level state heading in ALL CAPS and smaller counts preserving the hierarchy.
- Restored GC classification of historical claim paths: local primary-checkout claims own no disposable Git resources, and absent outside-root paths are reported as absent. Existing outside-root worktrees and branches held by stale Git registrations remain protected.
- Foreground `trk run --fg` now releases unfinished sessions' claims with an exit-status note, including on handled interrupts, without changing closed, waiting, handed-back or replacement sessions.
- Herdr issue rows now indent beneath rendered epic subheaders, including **No epic**, while preserving shared responsive columns; grouping off and entirely unassigned state groups remain flat.
- Human unclaim permission errors now include the issue-specific `trk unclaim <ID> --force` recovery command; Herdr shows CLI error hints in a wrapping status footer.
- Check queue recovery distinguishes recycled PIDs from live runners, retains surviving process-group protection, and offers audited human-only `trk check --unlock` recovery for stale or changed-host locks.
- Working-session spinners in the Herdr dashboard now prefix the issue title instead of occupying a separate row; titles still wrap and links and other statuses remain below.
- The Herdr dashboard's wide list/details split now starts at 50/50 instead of a fixed list width, retaining user resizing and pane minimums.
- Restored CI Clippy checks with narrowly scoped argument-count expectations on GC helpers, preserving cleanup safety gates and opt-in legacy recovery behavior.
- Removed a redundant GC host-option conversion flagged by Clippy, preserving host matching and cleanup safety checks.
- Unified local PID probes without subprocesses; permission-denied processes count as alive. Hostname detection now uses a cached system call and treats unavailable or empty names as unknown.
- Fixed Clippy's collapsible status check in Herdr layout migration without changing status parsing or offline fallback.
- Removed transitive unmaintained `chrono` by pinning rmcp's upstream opt-out change and retaining the existing non-chrono MCP features; MCP tests pass on Rust 1.89.

### Removed

- Removed the agent inbox: `trk note --to-agent`, MCP `messages_for_you`, the agent `inbox` setting, and both dashboards' note-to-agent actions. Use `trk send` for live steering or Resume to authorize continuation.
- Output schema version 5 removes `to_agent` from note JSON and history markers; old note events remain ordinary notes. The database migration drops the inbox table. Remove `inbox` from existing `.trk/agents/*.toml` before starting sessions.

### Beta compatibility and upgrades

- The versioned JSON envelope (`schema_version` and error codes) and MCP tool names are intended to be stable.
- Config keys, seed prompts, the database schema across versions, and the Herdr mod may change without notice.
- Finish active sessions and back up `.trk/` before upgrading. Update the source checkout, re-run `./install.sh`, then run `trk install` in each project using its original seed options. Database migrations are forward-only; do not assume downgrade compatibility.

See the [upgrade manual](docs/install-upgrade.md) and [human release steps](docs/development.md#releasing-010).
