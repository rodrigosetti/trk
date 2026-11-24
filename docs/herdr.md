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

Each issue row and detail header shows a colored type badge, including `code-change`.
Built-in types use consistent theme colors; custom types get a stable color from their name.
The type text stays readable in the theme's foreground color; long names have a full-name tooltip.

That is the whole setup. Herdr mods are machine-wide and a space shows one once a pane or tab
there holds it, so `open` does both steps: it installs or refreshes the mod when needed, then
opens the panel in the space this terminal is in (outside Herdr: the space whose folder is the
project, or `--space <id>`). The panel is pinned to this project and stays in that space, across
restarts, until you close it in Herdr. Opening again goes to the panel that is there. Spaces
where you never open it are untouched.

```sh
trk herdr install [--layout]     # only the mod; --layout adds the sidebar and split layouts
trk herdr uninstall              # remove the mod and layouts unless you edited them
```

- For a **trk dashboard** button above Settings in Herdr's sidebar:
  run `trk herdr install --layout`, then `herdr-app do layout.switch --name main-trk`.
  The button opens the dashboard for the shown space, or focuses its existing trk panel.
  Unpinned panels follow that space's first terminal with a live folder, in tab order—not
  keyboard focus or the space's original creation folder. With no live folder, they use
  the space folder.
  A collapsed sidebar shows just the icon. `main-trk` is a personal copy of Herdr's layout;
  the shipped `main` layout is untouched. Switch back with `herdr-app do layout.switch --name main`.
- `./install.sh` and `trk install` refresh an installed mod, so it never lags the binary; neither
  installs it. The mod folder is trk's and is rewritten; the layouts are yours once written.
- The `trk` layout (`herdr-app do layout.switch --name trk`) shows issues on the left and the
  selected issue's live session on the right. It follows the shown space's project.
- Issues are grouped as in the dashboard. A session whose agent Herdr reports as blocked moves to
  **Needs you**; a working one shows a spinner. New needs-you issues post a toast, and the Dock
  badge counts them across the projects shown.
- Select an issue for its description, notes and timeline, and to go to or peek at its session,
  run it, send text, resume, check now, add notes, change priority, unclaim, reopen or close.
  Destructive buttons ask for a second click. Edit opens `trk edit` in a pane below.
  At widths of 720px or more, drag the divider between the list and details to resize them;
  each scrolls independently. Herdr keeps the split across repaints, issue selection, and
  hiding/reopening details in that window. Below 720px, details appear inline below the selected row.
  **Hide details** (or `Esc`) closes the details pane so the issue list fills the panel.
  Click an issue or press `Enter` to reopen details; `j`/`k` keep them hidden while browsing.
- Click an issue ID in the list or detail header to copy it to the clipboard. The status line
  confirms the copy; selection and detail visibility stay unchanged.
- With the panel focused: `j`/`k` move, `Enter` opens, `a` goes to the session, `p` peeks,
  `s` sends, `m`/`M` note (to the agent), `v` resumes, `x` closes, `c` creates, `l` opens the
  issue's first link. Link chips on a row and in the details open in the browser on click.
- `trk.new` and `trk.refresh` are mod commands for `keys.json`, for example
  `"global": { "ctrl-shift-i": "trk.new" }`. They reach a shown copy of the panel.
- By hand: `herdr-app do tab.new --mod trk --props '{"folder": "/path/to/project"}'`. Without
  `folder` a copy follows the shown space; `"select": "<id>"` opens it on an issue.
  Run the panel's location regressions with
  `node --experimental-vm-modules --test herdr/mods/trk/main.test.mjs`.

