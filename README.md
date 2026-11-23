# trk

A local issue tracker and agent launcher in one Rust binary. Each project has its own SQLite
database in `.trk/`; humans use the CLI, agents work through a per-issue MCP server, and `trk run [ID]` starts
an agent session on one issue in a persistent terminal (`hc`). See [SPEC.md](SPEC.md) for the design.

An issue has a title, a description, notes, a priority, a required type (default `code-change`), and one of four states:

```
open → claimed → closed
                → waiting → closed   its check passed, or a human closed it
                          → claimed  its check failed, or a human resumed its live session
                          → open     no live session, wake delivery failed, or a human reopened it
```

A session ends by closing the issue or leaving it **waiting**: the work is done once something
outside the session happens (a merge, a review, a decision). A waiting issue may carry a shell
command; `trk check` runs those commands until each one passes, fails, or says "not yet".

## Install and upgrade

```sh
./install.sh
```

This builds a release binary and installs it to `~/.cargo/bin/trk`. Nothing else is written:
trk keeps no global database or config. Set it up per project:

```sh
cd ~/src/orch && trk install
```

`trk install` creates `.trk/` in the project root (in a git repository, the root of the primary
checkout): the database, the default config with every setting written out, and a `.gitignore`
that keeps all of it out of git. It also installs the agent skill (see below). Every other
command fails with `not_installed` until a project has been installed. Commands find the project
from any subdirectory and from any of its git worktrees.

To upgrade after changing or pulling this checkout, run `./install.sh` again, then `trk install`
in each project: it applies schema migrations, refreshes the agent skill, keeps your config
files, and lists the ones that differ from the new defaults. `trk install --force` rewrites the
config with the defaults. `trk --version` prints the commit the binary was built from. Running
agent sessions keep the binary they started with.

### Active sessions during upgrades

Opening the shared database with a newer trk can apply forward schema migrations. An older,
already-running MCP server cannot use a schema newer than its compiled migration set. It returns
`newer_database`, with the database path, `database_version` (SQLite `user_version`) and
`supported_version`, plus restart guidance. These numbers are **not** the JSON envelope's
`schema_version`. The database is not downgraded or reset. Binaries predating this error may
instead return the internal rusqlite_migration “migration number that is too high” error;
the recovery is the same.

To avoid interrupted tools and lease renewal, finish active sessions before a schema-changing
upgrade. Otherwise:

1. Preserve uncommitted work and handoff text. Have the human compare the installed
   `trk --version` and `trk whoami --json` with the harness's MCP launch command and its
   `TRK_DIR` (for omp, the worktree's `.omp/mcp.json`). Confirm the executable and
   project directory match; a working CLI alone does not establish that.
2. Install a trk that supports the database, then restart the harness's trk MCP connection
   using that executable. Keep the original issue, actor, agent and project
   directory. Restart/reconnect through the harness if supported; otherwise preserve the
   session context and restart the harness with its existing MCP configuration.
   trk does not hot-reload migrations or automatically replace/re-exec a running MCP server.
3. Call `get_task` after reconnecting. Reads work without an active claim. If the original
   claim remains active, resume or hand back normally. Lease renewal cannot access an
   incompatible database, so do not assume a long-interrupted session still owns its issue.
4. If this is the waiting session (the latest claim was released with reason `waiting`,
   and the issue is still waiting), notes and follow-ups still work; lifecycle changes do
   not. After reconnecting, a human can authorize more work in that live session with
   `trk resume <ID> -m "Continue with the preserved handoff" --json`. Otherwise stop agent
   writes. The human can preserve handoff text with `trk note <ID> ...`; if the waiting
   session is gone, use `trk reopen <ID> -m "Recover the handoff"` then `trk run <ID>`.
   Resolve blockers first; do not steal claims, impersonate another actor or edit the
   database to recover a handoff.

Restarting an MCP server does not claim an issue or bypass dependency/ownership checks.

For development: `cargo build` and `cargo test --workspace`.

## Quick start

```sh
cd ~/src/orch && trk install               # .trk/ with the database and config; ids are orch-…

trk create "Add claim leases" -d "Crashed agents must release their claims." -p P1
trk create "Enable leases in prod" --blocked-by orch-4kq
trk ready                                   # open, with every blocker closed
trk claim orch-4kq                          # open → claimed; records the worktree
trk note orch-4kq "Sweep runs at the start of every write"
trk close orch-4kq -m "Merged in 9f2c1e0"   # or leave it waiting, below
trk log orch-4kq
```

Use `trk create "Fix the bug" -d "Reproduction details" --run` to create an issue and
immediately launch its agent, equivalent to `trk run <new-ID>` with default routing
and launcher settings. It returns the run result (JSON `data.issue` is the new ID).
`--run` cannot be combined with `--dry-run`. If launching fails, the issue stays saved;
the error identifies it so you can fix the cause and retry with `trk run <ID>`.

Every command takes `--json` (or `--format pretty|json|jsonl`). Scripts should always pass `--json`;
JSON output is the versioned API (`schema_version: 4`), and errors use the same envelope with a
stable `code`, a `hint`, and exit codes 1–5.

Pretty output colors priorities consistently (P0 red, P1 yellow, P2 cyan, P3 dim) and states
(open green, claimed yellow, waiting cyan, closed dim green). Full issue details (`trk show` and
other commands displaying an issue) use bold cyan IDs and section headings, green ready indicators,
and yellow blocked and check-due indicators. Metadata, timestamps, and field labels
are dimmed; description and note bodies retain their existing rendering. Labels and state glyphs
remain present without color. ANSI styling is enabled only for pretty output on a terminal;
`NO_COLOR` or `--no-color` disables it, and piped output is unstyled. JSON and JSONL output never
contain ANSI styling.

## Waiting and `trk check`

```sh
# Done once the branch is in main: the check does the merge.
trk wait orch-4kq -r "merge into main" \
  -c 'cd "$TRK_REPO" && [ "$(git branch --show-current)" = main ] && git merge --no-edit "$TRK_BRANCH"'

# Done once the PR is merged; wake for conflicts or failed head-commit CI.
# gh's built-in jq filter reads the head commit's check runs and statuses.
pr_check=$(cat <<'CHECK'
result=$(gh pr view 12 --json state,baseRefName,mergeable,headRefOid,statusCheckRollup --jq '
  if .state == "MERGED" then "merged"
  elif .state == "CLOSED" then "PR closed without merging"
  elif .mergeable == "CONFLICTING" then "PR conflicts with base branch \(.baseRefName)"
  else
    .headRefOid as $head |
    [.statusCheckRollup[]? |
      select(
        (.__typename == "CheckRun" and .status == "COMPLETED" and
          (.conclusion | IN("FAILURE", "TIMED_OUT", "CANCELLED", "ACTION_REQUIRED", "STARTUP_FAILURE", "STALE"))) or
        (.__typename == "StatusContext" and (.state | IN("FAILURE", "ERROR")))
      ) |
      "\(.name // .context): \(.conclusion // .state) \(.detailsUrl // .targetUrl // "")"
    ] |
    if length > 0 then "CI failed on head \($head):\n" + join("\n")
    else "waiting" end
  end
') || exit "$?"
case "$result" in
  merged) exit 0 ;;
  waiting) exit 75 ;;
  *) printf "%s\n" "$result"; exit 1 ;;
esac
CHECK
)
trk wait orch-4kq -r "PR 12 merged" --every 10m -c "$pr_check"

# Waiting on a person: no check. A human can close, resume the live session, or reopen.
trk wait orch-4kq -r "SQLite or Dolt?"

trk check                 # run the checks that are due, one at a time
trk check --watch         # keep doing that; leave it running in a spare terminal
trk check orch-4kq        # run one now
trk check --dry-run       # preview checks and eligible cleanup without changing anything
trk check --no-gc         # run checks without automatic cleanup (also works with --watch)
trk list state=waiting    # what is waiting, on what, and when it is checked next
```

After each non-ID pass, `trk check` automatically sweeps all eligible closed-as-done
issues, including manual closes, under the same queue lock and safety gates as `trk gc`.
Abandoned issues are excluded. `[check] gc = true` and `gc_grace = "1h"` are the defaults,
also when those keys are missing. The grace is a minimum age since closure, not a cleanup
deadline: cleanup waits for a later pass and may skip unsafe resources. `trk check ID`
never sweeps; `--no-gc` disables sweeping for the whole run or watch session.
`--dry-run` previews cleanup without removals or cleanup notes. Cleanup errors are advisory
and do not change check outcomes or exit status. JSON includes optional `data.gc` with
`dry_run`, manual-GC-shaped `issues`, and optional `errors` strings, omitted when there
are no eligible issues or errors. Pretty output shows one line per issue with removals;
skips and their reasons are shown once per issue per run, rather than on every watch pass.

### Human-authorized resume

A human can continue the original live waiting session, with or without a check:

```sh
trk resume orch-4kq -m "Use SQLite; continue the implementation" --json
```

`-m` is required and must contain non-whitespace text. Resume restores the latest waiting
session's actor, worktree, host, agent and terminal in a fresh claim with a new lease **before**
sending the message, so its MCP lifecycle tools work again. It never launches a replacement.
Human resume neither consumes nor resets the automatic `wake_count` or `[check] max_wakes`
budget. The timeline records the human's authorization, message and delivery outcome.

Success data contains `issue`, `actor`, `terminal`, `claim`, `resumed_from`, `message` and
`delivered: true`: this confirms launcher delivery, not that the agent read, acted on or
completed the request. `trk send` only delivers text; it does not restore a waiting claim.

Agent actors receive `not_permitted`; agents must ask a human to authorize resume, never
self-resume or impersonate a human. Empty messages return `bad_args`. A busy check queue
returns `invalid_state`: retry after the running check finishes; do not clear its lock.
A dead, remote or unavailable original session, missing terminal or missing send configuration
also returns `invalid_state`. A human can instead run
`trk reopen orch-4kq -m "Restart with the decision"` then `trk run orch-4kq`.
If delivery fails after restoration, resume returns
nonzero `internal`, releases only that exact restored claim to open and records a failure
note without overriding subsequent session actions. Inspect `trk show` before recovery.

List and ready tables show each issue's type after priority. Filter either command with
`type=review` (or `type=research` / `type=code-change`); use `--sort type` to order by type:

```sh
trk list type=review --sort type
trk ready type=research --sort type
```

The check runs with `sh -c` in the worktree the issue was claimed in, with `TRK_ISSUE`,
`TRK_WORKTREE`, `TRK_REPO` (the main checkout) and `TRK_BRANCH` set. Its exit code decides:

| Exit | Meaning | Result |
| --- | --- | --- |
| 0 | pass (PR merged) | the issue closes as done |
| 75 | not yet (CI pending, awaiting review, unresolved threads, approved but not merged, unknown mergeability) | still waiting; checked again after `--every` (default `[check] interval`, 5m) |
| anything else, or a timeout | error (base-branch conflict, failed head-commit CI, closed without merging, or query/authentication failure) | wake the original live session, otherwise reopen; record the command's output in a note |

The PR states above describe the example, not built-in PR handling. The agent opens
the PR and waits; trk closes the issue when it merges. The agent fixes reported
base-branch conflicts and failed head-commit CI unattended. Review threads and
merging belong to the human unless they ask the agent to handle them.
The check names the conflicting base or prints failed jobs and their URLs. It
does not block on CI or review. Checks must be non-interactive, non-blocking
(no `--watch`) and safe to repeat.

A failed check restores the original session's claim with a fresh lease when its MCP
server is alive on this host, its terminal is recorded (and present when using Herdr),
`[launcher] send` is configured, and its wake count is below `[check] max_wakes`.
The cap defaults to `5`, including in existing configs that omit it; `0` disables waking.
Each restored claim increments the session's persistent count, even if sending fails.
Entering or updating `wait` preserves it. A fresh explicit claim (including `--steal`)
resets it to zero; repeating a claim as its current owner does not.
The session receives `wake N of M`, the output and a prompt to fix only what the
check reported, then end the new work cycle, normally with the same `wait`. Its
MCP tools work again. If the same failure keeps returning, it should `hand_back`
with the evidence instead of repeating indefinitely. While waiting, it can still
add notes and receive requests from the human.
If the cap is reached, the issue reopens with a note saying `wake cap of M reached`
and retaining the check output. A gone session or failed send also reopens the issue.
`trk show` displays a `wakes` line for a nonzero count while claimed or waiting;
issue JSON includes top-level `wake_count` only in those cases.
`trk check --json` distinguishes `woken` from `failed` (reopened); both include output.
trk interprets only the check's exit code, not PRs or CI.

A passing check still best-effort sends an informational message that the issue is
closed. Exit 75 does not notify or restore a claim.

With `[launcher] herdr = true`, a passing check also best-effort prepends `[closed] `
to the current name of the space containing the original waiting session's exact
terminal, keeping the marker visible even when a long title is truncated. User
renames and embedded literal `[closed]` text are preserved. An existing leading
marker is not duplicated; old trailing ` [closed]` markers are moved to the front.
This does not require a live agent or successful notification; missing UI/spaces
or rename failures never change the check result. Failed and retrying checks leave
titles unchanged.

`trk check` detects replacement of its executable, including same-version builds.
It finishes the active check, records the outcome and stops its remaining process
group, then releases the queue lock and re-executes the installed binary with the
original arguments and environment. Replacement is checked before each queued
issue and while idle in `--watch`; no manual watcher restart is needed. Before
re-executing, it logs an executable-update notice and the binary path to stderr,
leaving JSON stdout unchanged. One-shot results survive the restart, and completed
checks (including exit 75 with `--all` or an explicit ID) are not replayed within
that pass. Already-printed watch results
are not printed again. A temporarily missing executable is retried until it returns;
an exec failure reports an error without undoing recorded outcomes.

Three properties follow from that:

- **A waiting issue still blocks.** What depends on it becomes ready when the work has landed, not
  when the session stopped.
- **Checks are a merge queue.** Only one `trk check` runs at a time and it runs checks one after
  another, so two sessions never merge into main at once.
- **Failures come back as work.** A failed check wakes its live session to fix it; otherwise
  the issue reopens and the next session reads the output in the notes. A command that
  is missing or times out (`[check] timeout`, 10m) fails rather than waiting forever.

`trk check` owns each check's process group as well as the queue lock. On shell exit,
timeout, SIGINT, SIGTERM or SIGHUP, it stops remaining group members (SIGTERM, then
SIGKILL after two seconds) and reaps the shell before releasing ownership. Interrupting
the runner stops the pass/watch loop and leaves the interrupted issue waiting without
recording a partial result. Checks must not detach from their process group.

After SIGKILL or a runner crash, a new runner reports busy while the previous runner
or any live member of its recorded group survives. Once both are gone, it recovers
automatically; zombie processes do not block recovery. `trk whoami` shows
`check_running.pid`, `.host` and `.pgid`. A different/unknown host or unavailable
process information prevents automatic takeover. Inspect/stop surviving checks on
the owning host before retrying; never delete the lock while a check can still run.

Upgrading a database with an existing, untracked check marks its group as `0`
(unknown), preventing automatic recovery. Stop pre-upgrade runners and their checks,
verify none remain, then clear only that legacy lock with
`sqlite3 .trk/trk.db 'DELETE FROM check_lock WHERE pgid = 0'` in the primary checkout
(use the actual database path if overridden).

A check is written by an agent and runs later, unattended, as you. `trk show` always prints it in
full, and `trk check --dry-run` shows the queue before anything runs.

## Cleaning up closed issues

Closing retains the session, worktree and branch; use explicit garbage collection after work has landed:

```sh
trk gc --dry-run --json                   # preview all closed-as-done issues
trk gc orch-4kq --json                    # clean up one issue
trk gc --older-than 7d --abandoned --json # include abandoned issues closed over a week ago
```

`trk gc [ID...] [--dry-run] [--abandoned] [--older-than DURATION] [--json]` defaults to
closed-as-done issues; `--abandoned` also includes abandoned issues. It never collects open,
claimed or waiting issues, and serializes cleanup with the check queue and lifecycle writes.
All historical claims are considered, but only resources on this host are eligible.

Sessions are found by their exact recorded terminal through Herdr, not by title. GC closes
their containing spaces, protecting your current terminal's space and other issues' active
or waiting sessions. Only descendants of the configured worktree directory are removed,
never the current worktree. Git worktree removal is not forced; branch deletion uses
`git branch -d` in the primary checkout, only for the issue-ID branch and only after all
recorded worktrees are removed or absent. Dirty worktrees, unmerged branches and other
unsafe or refused targets are reported as skipped.

JSON uses the normal envelope with `data: {dry_run, issues}`. Each issue has `id` and
`session`, `worktree`, `branch` outcomes with `status` (`removed`, `absent`, `skipped`) and
`reason`. Session and worktree outcomes also have `items`, identifying each `terminal` /
`space` or `path` with its own status and reason. Dry runs write no notes and remove
nothing: planned removals use `removed` with a `would remove ...` reason and
`dry_run: true`. Real removals append caller-attributed notes; rerunning when resources
are already absent adds no cleanup notes.

## Agent sessions

```sh
trk run                          # start the highest-priority ready issue
trk run orch-4kq                 # claim as agent:<harness>:<uuid>, create the worktree, start in hc
trk runs                         # active and waiting sessions: state, terminal, claim metadata, MCP liveness
trk dashboard                    # live full-screen overview of all of this; ? lists the keys
trk attach orch-4kq              # take over the terminal
trk note orch-4kq --to-agent "Also update the docs"   # delivered with the agent's next tool result
trk send orch-4kq "Wrap up and finish"                # typed into the session
trk reap                         # release claims whose MCP server has gone, with a note
trk run orch-4kq --dry-run       # print the launch spec; claims nothing
trk agent preview implementer --issue orch-4kq        # instructions, briefing, tool schemas
```

In the dashboard, select a waiting / needs-you issue and press `v` to **Resume** the
original session, including when it has no check. Enter a nonempty continuation message
(for example, `SSH unlocked; continue`) and press Enter; Esc cancels without changes.
Resume runs in the background; success confirms claim restoration and instruction delivery,
not completion by the agent. The row refreshes from the database on success or error.
`s` **Send** delivers text only and never restores ownership; `x` **Close** and `o` **Reopen**
remain separate actions. `?` lists the keys. If Resume fails, inspect the error and current
state; restore session access and retry, or reopen and run a replacement as described above.

Omitting the ID selects the first ready issue, ordered by priority (P0–P3), then oldest
creation time, then ID, matching `trk ready`. If there are no ready issues, the command
returns an error without launching a session. `trk run --dry-run` previews the selection.

The agent for a run is `--agent` (a one-off override), else the issue type's `agent` mapping.
The seed types are `code-change` → `implementer`, `research` → `researcher`, and `review` → `reviewer`.
Use `trk create ... --type review`, `trk edit <ID> --type research`, and `trk list 'type=review'`.
The harness is `--harness`, else the first installed harness the agent prefers (see Configuration). The seed
config prefers `omp` running `openai-codex/gpt-6-luna`. The worktree is
`.trk/worktrees/<ID>` on a branch named after the issue, so a later session on the same
issue finds the earlier work.

The MCP server (`trk mcp --issue … --actor … --agent …`, started by the harness) serves exactly one
issue with six tools: `get_task`, `add_note`, `report_followup`, `hand_back`, `close` and `wait`.
It renews the claim's lease while it runs. The briefing carries the issue's description and every
note, which is how an issue that came back from a failed check explains itself.

Leaving an issue waiting releases the claim, not the terminal. While the issue stays
waiting, the actor of its latest claim released with reason `waiting` can still call
`add_note` and `report_followup`, and receive `--to-agent` notes with its next tool result.
It cannot call `wait`, `close` or `hand_back` again without a claim. `trk send` and
`trk attach` use that waiting session's terminal when there is no active claim; an
active claim always takes precedence, even if it has no terminal. `trk runs` marks
waiting sessions with `state: "waiting"` (their displayed lease is historical), and
`trk reap` skips them. Closing, reopening or claiming the issue for a replacement ends
the old session's access.

With `[launcher] herdr = true`, each run opens a **new Herdr space**, named with the issue's title
and rooted in its worktree. Its first terminal runs the agent directly; the launching space and
its tabs are left intact. This works from any terminal, not only one already in Herdr.
trk confirms the space's title, folder and agent pane through `herdr-app ls`; JSON includes
`placement` (`visible` or `failed`) and the actual terminal address used by `trk attach` and `trk send`.

If the UI is unavailable before launch, trk uses the configured `start` template headlessly and
warns. Once space creation is attempted, an uncertain response retains the claim and never starts
a duplicate session. A two-second observation timeout warns without ending the running session.

Omit `herdr` or use `herdr = false` for a headless/custom launcher; the default hc template uses
`--no-parent` to keep sessions independent. Herdr launching requires `hc` and `herdr-app` on PATH;
no IDs belong in the config.

## Agent skill

`trk install` also installs a skill (from [`skills/trk/`](skills/trk/SKILL.md), embedded in the
binary) that teaches your interactive agent sessions to use the trk CLI: `.agents/skills/trk`
(omp, Codex), linked from `.claude/skills/trk` when Claude Code is in use. `trk whoami` reports
whether the installed copy matches the binary; run `trk install` again to refresh it.

Sessions started by `trk run` should reach trk only through their MCP server (see SPEC.md). That is
why the skill is kept out of git with `.git/info/exclude`: the worktrees `trk run` creates do not
contain it, so those sessions do not see it. `trk install` and `trk run` warn when a repository
tracks the skill anyway; the omp profile also hides it with a per-session `--config` overlay
(`skills.ignoredSkills: [trk]`), and the skill itself tells an agent that has trk MCP tools to stop.

## Herdr panel

In the [Herdr](https://herdr.dev) app, the dashboard is also a native panel: a Herdr mod (from
[`herdr/mods/trk/`](herdr/mods/trk/main.js), embedded in the binary). It reads
`trk dashboard --json`, the snapshot the terminal dashboard draws, and every button runs `trk`
itself, so both behave the same. The panel header shows the project name without a `trk` prefix.

```sh
trk herdr open                   # the panel for this project, as a tab in this Herdr space
trk herdr open --split right     # beside this terminal's pane instead (or: down)
```

Each issue row shows its type immediately after its ID, including `code-change`.

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
  **Hide details** (or `Esc`) closes the details pane so the issue list fills the panel.
  Click an issue or press `Enter` to reopen details; `j`/`k` keep them hidden while browsing.
- Click an issue ID in the list or detail header to copy it to the clipboard. The status line
  confirms the copy; selection and detail visibility stay unchanged.
- With the panel focused: `j`/`k` move, `Enter` opens, `a` goes to the session, `p` peeks,
  `s` sends, `m`/`M` note (to the agent), `v` resumes, `x` closes, `c` creates.
- `trk.new` and `trk.refresh` are mod commands for `keys.json`, for example
  `"global": { "ctrl-shift-i": "trk.new" }`. They reach a shown copy of the panel.
- By hand: `herdr-app do tab.new --mod trk --props '{"folder": "/path/to/project"}'`. Without
  `folder` a copy follows the shown space; `"select": "<id>"` opens it on an issue.
  Run the panel's location regressions with
  `node --experimental-vm-modules --test herdr/mods/trk/main.test.mjs`.

## Configuration

Everything trk keeps for a project is in `.trk/` at its root, written by `trk install`:

```
.trk/                     (found from the working directory; $TRK_DIR / --dir override it)
  .gitignore              "*": nothing here is tracked until you edit it
  trk.db                  the database
  config.toml             user, issue id prefix, leases, check interval and timeout, worktrees,
                          launcher templates, harness profiles, saved views
  agents/*.toml           instructions, briefing, other MCP servers, harness preferences
  types/*.toml            name, description, agent routing
  prompts/*.j2            MiniJinja templates
  worktrees/<ID>/         git worktrees made by `trk run` ([run] worktrees)
  sessions/<uuid>/        per-session files
  checks/                 logs of running checks
```

`config.toml` is explicit: `trk install` writes every setting. Missing settings are errors
except for compatibility defaults such as `[check] max_wakes`, `gc` and `gc_grace`.
`[issues] prefix` names new issues (`<prefix>-<base32>`); it defaults to the directory name,
or `trk install --prefix NAME`. Changing it later only affects new issues.

| Check setting | Default, also when omitted | Meaning |
| --- | --- | --- |
| `[check] gc` | `true` | Automatically sweep closed-as-done resources after non-ID passes; `--no-gc` overrides it for a run/watch |
| `[check] gc_grace` | `"1h"` | Minimum age since closure for automatic cleanup, not a deadline |

There is no schema to apply: states, the close reasons (`done`, `abandoned`), priorities and the
one dependency kind (`blocks`) are fixed. Agents and prompts are read when a session starts;
`trk agent check` validates them. `trk show` and `trk run --dry-run` print which type and agent
files a run would use.

There is no global configuration and no cross-project view: `trk list`, `trk runs` and
`trk check --watch` act on the project of the working directory, so each project needs its own
`trk check --watch`.

## Layout

| Crate | Role |
| --- | --- |
| `trk-core` | The state machine, claims and leases, the waiting queue, query language, `Store` (the only writer) |
| `trk-agent` | Agent definitions, the six trk tools and their executor, prompt rendering, harness profiles, launch spec |
| `trk-mcp` | The per-issue MCP server (rmcp, single-threaded Tokio) |
| `trk-cli` | The `trk` binary, rendering, `trk check`, `trk run` and the launcher templates |

`seed/` holds the default configuration, embedded into the binary for `trk install`.
`herdr/` holds the Herdr mod and layout, embedded the same way.

## Notes on the implementation

- **Harness names**: the Claude Code profile is `claude-code`, matching `agent:claude-code:<id>`.
  A harness is registered the first time one of its sessions acts.
- **omp** (verified live): reads `<worktree>/.omp/mcp.json` and shows MCP server instructions to the
  model, so no folding is needed. Its `--tools` flag hides MCP tools that are not listed, so profiles
  get a `{trk_tools:PREFIX}` placeholder (`{trk_tools:mcp__trk_}`) to list the agent's trk tools.
  It keeps `write` available even with a restricted `--tools` list, so `builtin_tools = "read-only"`
  is not fully enforced there.
- **Unverified harness profiles**: the Claude Code, Codex, OpenCode and pi profiles were not run live.
- **`trk reap`** decides liveness from the MCP server's pid, host and exit time, which the server
  records in `actor.meta` (with the client's name and version).
- **`trk check` lock**: a row holding the pid and host of the running pass. A holder whose process
  is gone on this machine is replaced; a holder on another host is never replaced automatically.
- **`TRK_NOW`** fixes the clock, for tests.
