# trk

A local issue tracker and agent launcher in one Rust binary. Each project has its own SQLite
database in `.trk/`; humans use the CLI, agents work through a per-issue MCP server, and `trk run [ID]` starts
an agent session on one issue through a named launcher profile (Herdr, `hc`, or tmux). See [Design and state machine](design.md) for the model.

An issue has a title, a description, notes, a priority, a required type (from `[issues] default_type`, initially `code-change`), and one of four states:

```
open → claimed → closed
                → waiting → closed   its check passed, or a human closed it
                          → claimed  its check failed, or a human resumed its live session
                          → open     no live session, wake delivery failed, or a human reopened it
```

A session ends by closing the issue or leaving it **waiting**: the work is done once something
outside the session happens (a merge, a review, a decision). A waiting issue may carry a shell
command; `trk check` runs those commands until each one passes, fails, or says "not yet".

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

