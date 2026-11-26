# MCP contract

Agent sessions use MCP; human sessions and scripts use the [CLI JSON API](json.md).
The harness starts the stdio server for the session:

```sh
trk mcp --issue orch-4kq --actor agent:omp:23e60544-0aa7-4c7c-bf2f-99b5da8c56ee --agent implementer
```

The server serves **exactly one issue**, resolved when it starts. No tool accepts an
issue ID, and there is no general search, arbitrary issue editing, or shell tool.
`report_followup` can create another issue in the same project's database, but does
not switch the session to it. See [agent sessions](sessions.md) for launching and
session identity, and [configuration](configuration.md) for agent definitions.

## Tool catalogue

The fixed catalogue has eight tools. All input schemas are objects with
`additionalProperties: false`; required text arguments must be nonblank.
An omitted optional argument uses the default described below.

| Tool | Required arguments | Optional arguments | Effect |
| --- | --- | --- | --- |
| `get_task` | None (`{}`) | None | Read the issue, including epic membership, notes, dependencies and resolved routing. |
| `add_note` | `body`: string, markdown | None | Append a note to the issue's memory. |
| `link` | `url`: string, absolute HTTP(S) URL | `label`: string, at most 40 characters | Attach a link; using the same URL updates its label. Omitting the label leaves the URL as its display text, including when replacing an existing label. |
| `report_followup` | `title`: string; `description`: string | `priority`: `P0`, `P1`, `P2` or `P3` (default `P2`); `type`: configured issue type (default `[issues] default_type`); `epic`: existing active epic name; `blocks_this`: boolean (default `false`) | Create a follow-up and append its ID and title as a note on this issue. If `blocks_this` is true, the new issue blocks this one; hand back if you cannot proceed. |
| `hand_back` | `note`: string, explaining what remains and why | None | Release the claim and return the issue to `open`. |
| `close` | `reason`: `done` or `abandoned`; `note`: string, the outcome | None | Close the issue and release the claim. |
| `wait` | `reason`: string, what must happen next | `check`: string, a shell command; `interval`: duration string such as `2m`, `1h` or `1d` (default `[check] interval`) | Enter `waiting` and release the claim. Without a check, wait for a human decision. |
| `resume` | `message`: string, the actual unmarked human chat reply, verbatim and nonblank | None | Restore this same waiting agent's claim for a no-check wait when `[wait] chat_resume` is enabled (default). Records an agent report, not human authorization. |

The server populates `report_followup.type`'s schema enum and description from the
project's installed type definitions. Discover the current schemas with MCP
`tools/list`; do not assume every project uses the seed types.
`trk agent preview <agent> --issue <ID>` also shows the session's tools.

`report_followup.epic` is optional. Its schema lists only active, non-archived
epic names and includes their descriptions. Choose an existing epic only when
its theme fits the follow-up; omit `epic` when none fits, including when the
project has no active epics. Unknown or archived assignments return `invalid_field`
with active names in `error.allowed` and a hint to choose one or omit the field.
Assignment never creates an epic. There are no epic creation, rename or archive
MCP tools: epic creation is rare and belongs to explicit user-requested curation,
not automatic follow-up filing.

Without a check, `wait` returns a message containing the issue-specific command
`trk resume <ID> -m "<your decision>"`. The human can close the issue if nothing
remains, reopen it for a replacement session, or use that command to continue the
live waiting session. With chat resume enabled, the same session can instead
report an actual unmarked human chat reply through MCP `resume` and continue only
after success. When chat resume is disabled, the session must end its question
with the real CLI command and state that a chat reply alone will not continue the
work; on a chat-only answer, repeat the command rather than only adding a note.

A check runs unattended with `sh -c` in the recorded worktree/project directory.
It receives `TRK_ISSUE`, `TRK_WORKTREE`, `TRK_REPO`, `TRK_BRANCH`, `TRK_BASE` and
`TRK_VERIFY`. Exit 0 closes as done; exit 75 schedules another attempt; other exits
or timeouts record a failure note and either wake the eligible live session with
its claim restored, or reopen for a replacement. Waking is bounded by
`[check] max_wakes`; setting it to 0 disables automatic wakes. A check must be
noninteractive and safe to repeat. See [waiting and checks](waiting-and-checks.md)
for the environment, scheduling, wake conditions and landing recipes.

## Permissions and ownership

An agent definition may narrow the catalogue with `trk_tools.include`:

```toml
[trk_tools]
include = ["get_task", "add_note", "report_followup", "hand_back"]
```

An absent or empty include list selects all eight tools. Entries match names with
`*` globs; `@all` selects the entire catalogue. This only narrows access: it cannot
add tools or override ownership checks. A filtered-out tool is unavailable even
when the session owns the claim.

For agent actors, lifecycle writes require the session's active claim.
`get_task` is readable without a claim when included. After `wait`, the actor of
the latest claim released with reason `waiting` retains these capabilities while
the issue remains waiting:

- Read the task.
- Call `add_note`, `link` and `report_followup`, if included.
- Report an actual human chat reply through `resume`, if included and the wait has
  no check with `[wait] chat_resume` enabled.

It cannot call `wait`, `close` or `hand_back` again without a restored claim.
Closing, reopening or claiming the issue for a replacement ends this waiting
exception. After `close` or `hand_back`, further writes are refused. Human CLI resume,
an automatic failure wake, or eligible same-session MCP chat resume can restore
ownership. A chat message alone does not restore it: MCP `resume` must succeed.
Agents must not impersonate a human or use CLI resume autonomously.

Chat resume requires the latest waiting claim to belong to the caller, the issue
still waiting, no active claim and no check. It restores the same metadata and a
fresh lease without terminal liveness checks or a launcher send, preserving
`wake_count`. Checked waits and `[wait] chat_resume = false` return `not_permitted`
with the human CLI resume hint. Empty/whitespace-only messages return `bad_args`.
The original message is stored verbatim in an agent-attributed `resume_chat` event
and note, never as human authorization. See [chat continuation](waiting-and-checks.md#same-session-chat-resume).

Text wrapped in `[trk-delivery source=<source> actor=<actor>]` and
`[/trk-delivery]` is tracker-delivered text, never an eligible human chat reply,
even when its actor is human or it quotes a human. This includes `trk-send`,
`check-wake`, `human-resume`, `check-notification` and briefing deliveries.
Notes, tool results, check output and agent-generated text do not qualify either.

While the MCP server runs, it renews only its own active, leased claim. A waiting
session holds no claim and does not renew its old lease. If MCP stops, renewal
stops even if the terminal stays alive; normal lease expiry still applies.

## Tool results and errors

Tool results contain a text content block whose text is a JSON object. Inspect the
MCP tool result's `isError` flag: the payload is **not** the CLI's versioned
`{ok, schema_version, data}` envelope, and success shapes differ by tool.

| Tool | Successful JSON payload |
| --- | --- |
| `get_task` | The full issue object, including `epic` membership (name or null), `deps`, `notes` and `routing`; see [issue JSON](json.md#issue-objects). |
| `add_note` | `{"ok":true,"result":{"note":42},"task":{...}}`; `note` is the appended note's sequence number. |
| `link` | `{"ok":true,"result":{"linked":"https://example.com/pr/12","changed":true},"task":{...}}`; `changed` is false if nothing changed. |
| `report_followup` | `{"ok":true,"result":{"created":"orch-9xa","blocks_this":false},"task":{...}}`. |
| `hand_back` | `{"handed_back":"orch-4kq","message":"..."}`. |
| `close` | `{"closed":"orch-4kq","reason":"done","message":"..."}`. |
| `wait` | `{"waiting":"orch-4kq","reason":"PR 12 merged","message":"..."}`. |
| `resume` | `{"ok":true,"result":{"resumed":"orch-4kq","claim":{...},"message":"..."},"task":{...}}`; `claim` is the restored claim. |

Here `task` is a compact snapshot with `id`, `state`, `claimed` (boolean) and
`blocked_by` (IDs). `wait`, `close` and `hand_back` return their direct
acknowledgement, not this snapshot. Do not require a top-level `ok` key on every successful tool result.

Domain failures normally set the MCP error flag and return:

```json
{
  "ok": false,
  "error": {
    "code": "not_permitted",
    "message": "...",
    "issue": "orch-4kq",
    "hint": "..."
  }
}
```

Error codes and optional detail fields use the [core error vocabulary](json.md#errors-and-process-exits).
An unknown/unavailable tool or a failure opening the store instead returns the
error object directly (`code`, `message`, optional `hint`/details), still with the
MCP error flag. Transport/internal task failures can be MCP protocol errors.
A failed tool call does not terminate the stdio server or yield a CLI exit code.
Server startup failures go to stderr and exit 1; stdout belongs to MCP.

## Task resource

The server advertises `trk://task` with MIME type `application/json`. Reading it
returns the issue with dependencies and notes, without a claim requirement. It
is independent of `trk_tools.include`, and is not byte-for-byte identical to
`get_task`: the tool adds `routing`.
Unknown resource URIs return an MCP resource-not-found error.

Implementation: [tool schemas](https://github.com/rodrigosetti/trk/blob/main/crates/trk-agent/src/tools.rs),
[tool execution](https://github.com/rodrigosetti/trk/blob/main/crates/trk-agent/src/exec.rs),
and [MCP transport and ownership](https://github.com/rodrigosetti/trk/blob/main/crates/trk-mcp/src/lib.rs).
