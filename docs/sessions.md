## Agent sessions

```sh
trk run                          # start the highest-priority ready issue
trk run orch-4kq                 # claim as agent:<harness>:<uuid>, create the worktree, start in hc
trk runs                         # active and waiting sessions: terminal/session and separate MCP liveness
trk dashboard                    # live full-screen overview of all of this; ? lists the keys
trk attach orch-4kq              # take over the terminal
trk note orch-4kq --to-agent "Also update the docs"   # delivered with the agent's next tool result
trk send orch-4kq "Wrap up and finish"                # typed into the session
trk reap                         # release active claims whose local terminal is confirmed gone, with a note
trk run orch-4kq --dry-run       # print the launch spec; claims nothing
trk run orch-4kq --model provider/model --dry-run      # one-run model override and resolved argv
trk agent preview implementer --issue orch-4kq        # instructions, briefing, tool schemas
```

An issue can carry **links** to what its work produced: a pull request, a preview, a document.
A session adds one with its `link` tool (the seed PR landing recipe asks for `PR #<number>`), and
you can with `trk link orch-4kq https://… -l "PR #12"`; `trk unlink` removes it. trk only stores
and shows them: the first link appears on the issue's row in the dashboard, where `l` opens it
in the browser, and the details list them all. In Herdr the links are clickable.

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
The seed types are `change` (default) and `bug` → `implementer`, `research` →
`researcher`, and `ops` → `operator`.
Use `trk create ... --type ops`, `trk edit <ID> --type research`, and `trk list 'type=ops'`.
Each `.trk/types/<name>.toml` may set `worktree = true` (the default when omitted)
or `worktree = false`. This is a type-only setting, not an agent or harness setting;
`--agent` does not override it. `change` and `bug` use worktrees, while the research
and ops seeds set it to `false`. With `true`, the worktree is
`.trk/worktrees/<ID>` on a branch named after the issue. New issue branches start at
the resolved `[run] base`; existing issue branches are reused, so a later session
on the same issue finds the earlier work. The worktree contains the whole repository;
the session starts at the project's relative path inside it (for example,
`<worktree>/services/api` for a project at `repo/services/api`). Claims record the
worktree root, and checks use that root as `TRK_WORKTREE`, with the primary checkout
root as `TRK_REPO`. With `false`, the session runs in the primary project root,
creates neither branch nor worktree, and records no worktree path in its claim.
`trk show --json` routing and `trk run --dry-run --json` expose the resolved `worktree` boolean.

The operator performs work on a running service (deploy, rollback, migration,
credential rotation or incident investigation), not a repository change to land.
It records the initial state, notes every state-changing command before execution,
and verifies and records the result. Before destructive or irreversible work it
asks the user in the session and proceeds only on their authorizing answer; this
approval does not use trk `wait`. Repository code/config fixes are filed as
follow-up `change` issues instead of edited. The operator closes with the outcome
and evidence; its instructions include shared tracker rules but no landing recipe.

The harness is `--harness`, else the first installed harness the agent prefers (see Configuration).
The seed config prefers `omp` running `openai-codex/gpt-6-luna`.
Model precedence is `--model`, then the agent's model for the selected harness, then
the profile's `model`. With none set, the harness chooses its own default.

The MCP server (`trk mcp --issue … --actor … --agent …`, started by the harness) serves exactly one
issue with eight tools: `get_task`, `add_note`, `link`, `report_followup`, `hand_back`, `close`,
`wait` and `resume`.
It renews the claim's lease while it runs. The briefing carries the issue's description and every
note, which is how an issue that came back from a failed check explains itself.

Leaving an issue waiting releases the claim, not the terminal. While the issue stays
waiting, the actor of its latest claim released with reason `waiting` can still call
`add_note`, `link` and `report_followup`, and receive `--to-agent` notes with its next tool result.
It cannot call `wait`, `close` or `hand_back` again without a claim. `trk send` and
`trk attach` use that waiting session's terminal when there is no active claim; an
active claim always takes precedence, even if it has no terminal. `trk runs` marks
waiting sessions with `state: "waiting"` (their displayed lease is historical), and
`trk reap` skips them. Closing, reopening or claiming the issue for a replacement ends
the old session's access.
For a no-check wait with `[wait] chat_resume` enabled (default), that same session
may report an actual unmarked human reply through MCP `resume` to restore its
claim and continue. This is agent-reported continuation, not human CLI authorization.
Checked waits and disabled chat resume require the human CLI path instead.

Session liveness comes from the recorded terminal, not the MCP process. A recorded
launcher profile, a known claim host matching this machine and a nonblank terminal
are required; otherwise liveness is unknown. Unnamed historical or non-launcher claims
cannot use launcher-based attach, send, CLI resume or automatic wakes;
trk never assigns them today's launcher. The Herdr driver discovers exact terminals;
other profiles use optional `exists` argv: exit 0 means live, a nonzero exit means gone,
and command errors or a
missing/empty operation mean unknown. Unknown sessions are neither automatically
woken nor CLI-resumed or reaped; a failed check instead reopens. Wakes and CLI
resume require the pinned profile's `send`. MCP chat resume does not probe terminal liveness.

`[launcher] send_delay` waits before the initial briefing (default 5 seconds).
All sends—briefings, `trk send`, resume, and check wakes/notifications—use
`send_retries` total attempts (default 10; `0` means one attempt), with
`send_retry_delay` seconds between failed attempts (default 3).
Every delivery is wrapped in `[trk-delivery source=<source> actor=<actor>]`
and `[/trk-delivery]`; the actor is quoted and escaped. Such text never counts
as a human chat reply for MCP `resume`, even if a human invoked `trk send`.

`trk runs` and the dashboard distinguish terminal/session liveness from MCP liveness.
A live terminal with a stopped MCP server is **dormant**, not a dead session; it can
receive a wake or human-authorized resume. Restart its MCP connection with the same
identity to use tools again. `trk reap` leaves dormant and unknown sessions alone.
Lease renewal stops while MCP is stopped, however: active claims can still expire
through the normal lease sweep. Terminal liveness does not prevent expiry.

## Agent skill

`trk install` also installs a skill (from [`skills/trk/`](https://github.com/rodrigosetti/trk/blob/main/skills/trk/SKILL.md), embedded in the
binary) that teaches your interactive agent sessions to use the trk CLI at `.agents/skills/trk`.
Configure links for other harnesses explicitly. `trk whoami` reports whether the installed
copy matches the binary; run `trk install` again to refresh it.

Configure its locations in `.trk/config.toml`:

```toml
[skill]
path = ".agents/skills/trk"       # "" disables skill installation
links = [".claude/skills/trk"]    # [] disables symlink creation
```

Paths are relative to the project root (absolute paths also work). Any number of links
can point to the installed directory. Installation, git excludes, tracked-path warnings,
and `whoami` follow this section; a disabled skill reports `"disabled"` and adds no excludes
or warnings. Changing the section does not remove previously installed files, links, or excludes.
Install JSON reports symlinks in `skill.links` as `{path, linked}` entries.

Sessions started by `trk run` should reach trk only through their [MCP server](mcp.md). That is
why the skill is kept out of git with `.git/info/exclude`: the worktrees `trk run` creates do not
contain it, so those sessions do not see it. `trk install` and `trk run` warn when a repository
tracks the skill anyway; the omp profile also hides it with a per-session `--config` overlay
(`skills.ignoredSkills: [trk]`), and the skill itself tells an agent that has trk MCP tools to stop.

