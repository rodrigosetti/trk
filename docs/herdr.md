## Herdr panel

In the [Herdr](https://herdr.dev) app, the dashboard is also a native panel: a Herdr mod (from
[`herdr/mods/trk/`](https://github.com/rodrigosetti/trk/blob/main/herdr/mods/trk/main.js), embedded in the binary). It reads
`trk dashboard --json`, the snapshot the terminal dashboard draws, and every button runs `trk`
itself, so both behave the same. Herdr labels the mod and new dashboard tabs `trk dashboard`.
The panel header shows the project name without a `trk` prefix.

```sh
trk herdr open                   # the panel for this project, as a tab in this Herdr space
trk herdr open --split right     # beside this terminal's pane instead (or: down)
```

Native **Issues / Checks** tabs switch views in tabs, splits, and the fixed first tab in the **trk**
layout. **Checks** lists the latest 100 completed executions, newest first: UTC timestamp,
copyable issue ID, result, and duration in milliseconds. **Output** expands the captured
combined stdout/stderr as literal text, including successful checks and retries.
Truncated output is marked. Click the row to open issue details, including closed issues;
ID and Output clicks do not select the issue. **Failed only** includes failed runs and
failure wakes, but not successful wakes, retries, or stale/skipped results.

The panel reads `check_runs` from the store-backed `trk dashboard --json` snapshot,
on its normal database-change/10-second refresh (remote panels use the timer). A watcher
in another terminal supplies the same records; the panel never starts or reads a
`--watch` stream. Runs survive panel/watcher restarts. Only executions by an updated
binary are recorded; earlier lifecycle events are not backfilled. The store retains
100 runs globally per project, each with at most a 4096-byte UTF-8 output tail.
View/filter/expansion choices are per-panel memory; set `"view": "checks"` in mod props
to start on Checks after opening or restarting a panel.

Each issue row and detail header shows a colored type badge, including `code-change`.
Built-in types use consistent theme colors; custom types get a stable color from their name.
The type text stays readable in the theme's foreground color, with a full-name tooltip.
Issue IDs and type labels use shared, left-aligned columns across all dashboard sections.
Titles wrap in the remaining width. Working-session spinners prefix the title, aligned with
its first line; links and other statuses stay below, with no empty row for spinner-only issues.
Column widths adapt to the list pane when resized; long IDs and custom type names wrap instead of being clipped.

The slim **Sort** row below the header orders issues within each group: **Priority**
(P0 first), **Updated** (newest first), **Created** (oldest first), or **Type** (A–Z).
Click a key to use its natural direction; click the active key again to reverse it.
The highlighted key's arrow shows ascending (↑) or descending (↓) order.
Priority ascending is the default, retaining priority then oldest-first order.
Ties always use priority, oldest creation time, then ID, even when reversing a key.
The five state groups—**NEEDS YOU**, **IN FLIGHT**, **WAITING**, **READY**, and **BLOCKED**—
stay at the top level with larger semibold headings. **Group: Off / Epic** on the Sort row
(or `e`) toggles epic subgroups inside every state group; grouping is off by default.
Epic sub-headers use smaller semibold names; both heading levels use theme foreground
for contrast in dark and light modes, with smaller counts beside them.
Each epic sub-header shows its name and count in that state. Rows beneath it receive 12px extra left indentation,
including rows under **No epic**. ID/type columns stay aligned across epic subgroups;
their shared widths reserve the indentation so titles still wrap within narrow panes.
Rows retain the selected sort and fixed tie-breaks; buckets rank by their top row's
selected sort value, then epic name. **No epic** is always last.
An entirely unassigned state group stays flat, without a **No epic** header or extra indentation.
Each epic sub-header has a separate disclosure arrow: **▾** expanded, **▸** collapsed.
Click the arrow to hide or reveal just that subgroup's issue rows; its name and total count
remain visible, including **No epic**. The same epic folds independently in each state group.
Epic folds start expanded and survive refreshes, sorting, state-group folding and toggling
grouping off/on within the panel. An entirely unassigned state group remains flat even if
its **No epic** subgroup was previously folded. With grouping off, rows show a dim epic tag.
A blocked row's `by` hint includes each blocker's epic when different from its own
(including **No epic** for an unassigned blocker).
Keyboard navigation follows the displayed issue order, skipping headers and folded rows.
Folding the selected issue's epic clears selection and closes its details. Changing sort or
toggling grouping otherwise keeps selection and details, unless grouping hides that issue
inside a folded epic. State-group folding is unchanged.
Sort, grouping and epic folds are in memory per panel, not saved across panel restarts.

That is the whole setup. Herdr mods are machine-wide and a space shows one once a pane or tab
there holds it, so `open` does both steps: it installs or refreshes the mod when needed, then
opens the panel in the space this terminal is in (outside Herdr: the space whose folder is the
project, or `--space <id>`). The panel is pinned to this project and stays in that space, across
restarts, until you close it in Herdr. Opening again goes to the panel that is there. Spaces
where you never open it are untouched.

```sh
trk herdr install                # install both mods and the single trk layout; does not activate it
herdr-app do layout.switch --name trk
herdr-app do layout.switch --name main  # revert to stock Herdr
trk herdr uninstall              # remove mods and unedited layout; preserve personal overrides
```

- The **trk** layout adds a fixed first dashboard tab to each space whose project has its own
  `.trk` database and is not a linked Git worktree. This virtual entry has no close control
  or drag behavior; the layout's close shortcuts ignore it. Other spaces retain ordinary tabs.
  The project follows the space's first live terminal folder in tab order, falling back to its
  creation folder, never keyboard focus. Git metadata and physical paths distinguish linked
  worktrees from primary checkouts; only the resolved root's real `.trk/trk.db` qualifies.
  Non-Git folders must have their own database, not an ancestor's.
  Selection is remembered per space while the layout is active; discovering a database never
  selects its dashboard. Existing dashboard tabs, terminals, names, ordering and splits stay put.
  The dashboard pins `--dir <root>/.trk` and the space's machine; remote probes never fall back
  to local paths. Unsupported shells or unreachable machines fail closed with ordinary tabs.
  Probes are asynchronous, cached, invalidated by project changes/local filesystem events,
  and rechecked at most once a minute when idle (also the remote fallback).
- Plain `trk herdr install` installs this layout and its `trk-tabs` companion. Activation is
  separate: `herdr-app do layout.switch --name trk`; revert with
  `herdr-app do layout.switch --name main`. Stock `main` and unrelated layouts are untouched.
- Installation replaces the former split-view `trk` and sidebar-button `main-trk` variants.
  Previous layout files and their saved layout state are backed up under
  `~/.config/herdr-app/backups/trk-layouts/`, outside the selectable layouts directory.
  A previously selected `main-trk` migrates to `trk` through the running app; when the app
  is closed, only its persisted selection is migrated. Other layout selections are unchanged.
- Keyboard wrappers are derived from your existing global and `layout:trk` actions and written
  **only** to `layout:trk` in `keys.json`, never global overrides. Custom unrelated bindings
  and other sections survive. No guessed platform defaults are installed when keys are absent.
  Tab navigation includes the fixed first tab; closing it is ignored. Opening/navigation actions
  return to native tabs as appropriate. Uninstall removes matching generated wrappers, restoring
  native layout actions when needed, and preserves personal overrides.
  On macOS this preserves Cmd+1…9, Ctrl+Tab / Ctrl+Shift+Tab, Cmd+Shift+[ / ], Cmd+W and
  Cmd+Shift+W in this layout, routing them through the virtual-tab dispatcher. Other platforms
  retain their existing equivalents. More-specific personal terminal/mod bindings still win.
  **Native menus and direct built-in commands bypass these wrappers:** closing a tab/pane there
  can end the underlying terminal even while the virtual dashboard is shown. Herdr has no
  command-veto API; this layout does not change those native command semantics.
  Switch back to `main` before uninstalling.
- `./install.sh` and `trk install` refresh installed mods and migrate their layout; neither
  installs into a Herdr setup without trk. Both mod folders are machine-owned, while symlinked
  development copies are left alone. Legacy edited layouts are backed up before replacement;
  edits to the current `trk` layout survive later installs and refreshes. A machine snapshot
  under `backups/trk-layouts/installed.json` distinguishes installed files from personal edits.
- Issues are grouped as in the dashboard. A session whose agent Herdr reports as blocked moves to
  **Needs you**; a working one shows a spinner. New needs-you issues post a toast, and the Dock
  badge counts them across the projects shown.
- Select an issue for its description, notes and timeline, and to go to or peek at its session,
  run it, send text, resume, check now, add notes, change priority, unclaim, reopen or close.
  Run and destructive buttons ask for a second click on the same button. Confirmation
  survives dashboard refreshes and has no timed expiry; changing selection, hiding details,
  or opening a prompt cancels it. Pending commands show in the status footer and cannot
  arm another confirmation. Completion releases the command gate even after switching
  projects or hiding/reopening the fixed dashboard tab. CLI failures and hints appear in
  the status footer, or in an error toast if the dashboard has since changed context.
  Edit opens `trk edit` in a pane below.
  At widths of 720px or more, the list and details start at an equal 50/50 split;
  drag the divider to resize them. Each scrolls independently. Herdr keeps the split
  across repaints, issue selection, and hiding/reopening details in that window.
  Below 720px, details appear inline below the selected row.
  **Hide details** (or `Esc`) closes the details pane so the issue list fills the panel.
  Click an issue or press `Enter` to reopen details; `j`/`k` keep them hidden while browsing.
- Click an issue ID in the list or detail header to copy it to the clipboard. The status line
  confirms the copy; selection and detail visibility stay unchanged.
- With the panel focused: `j`/`k` move, `Enter` opens, `a` goes to the session, `p` peeks,
  `s` sends, `m` adds a note, `v` resumes, `x` closes, `c` creates, `l` opens the
  issue's first link. With no prompt open, `o` cycles sort keys in their natural direction,
  and `O` reverses the active key; `e` toggles epic grouping. Link chips on a row and in the details open in the browser on click.
- `trk.new` and `trk.refresh` are mod commands for `keys.json`, for example
  `"global": { "ctrl-shift-i": "trk.new" }`. They reach a shown copy of the panel.
- By hand: `herdr-app do tab.new --mod trk --props '{"folder": "/path/to/project"}'`. Without
  `folder` a copy follows the shown space; `"select": "<id>"` opens it on an issue.
  Set the initial sort with `"sort": "updated"` (newest first) or `"sort": "type"` (A–Z).
  Valid keys are `priority`, `updated`, `created`, and `type`; prefix `-` to reverse
  that key's natural direction (for example, `"sort": "-updated"` is oldest first).
  Invalid sort values fall back to Priority ascending. Set `"group": "epic"` to start
  with epic grouping on; omitted or other values start flat. For example:
  `herdr-app do tab.new --mod trk --props '{"sort": "updated", "group": "epic"}'`.
  Run the dashboard's location, sorting, grouping, and action-confirmation regressions with
  `node --experimental-vm-modules --test herdr/mods/trk/main.test.mjs`.

