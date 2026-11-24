## Waiting and `trk check`

```sh
# Merge your branch only after verifying the merged result in the issue worktree.
local_check=$(cat <<'CHECK'
cd "$TRK_WORKTREE" || exit 1
git merge --no-edit "$TRK_BASE" || { git merge --abort 2>/dev/null; exit 1; }
if [ -n "$TRK_VERIFY" ]; then
  sh -c "$TRK_VERIFY" || exit 1
fi
cd "$TRK_REPO" || exit 1
[ "$(git branch --show-current)" = "$TRK_BASE" ] || exit 1
git merge --ff-only "$TRK_BRANCH" || exit 1
CHECK
)
trk wait orch-4kq -r "verify and merge into base branch" -c "$local_check"

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

Without a check, the human can close the issue if nothing remains, reopen it for a new
session, run `trk resume <ID> -m "<your decision>"` to continue the live waiting
session, or reply in that same session's chat for the agent to report through MCP
`resume` when chat resume is enabled (the default; see below).
The no-check MCP `wait` result includes the CLI command with the real issue id.
When `[wait] chat_resume = false`, the session must say plainly that a chat reply alone
will not continue the work and end its chat question with that exact command.
If the human replies only in chat while chat resume is disabled, the session repeats
the command rather than only recording a note.

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
sending the message, so its MCP lifecycle tools work again once its MCP connection is
running (restart it if dormant). It never launches a replacement.
Human resume neither consumes nor resets the automatic `wake_count` or `[check] max_wakes`
budget. The timeline records the human's authorization, message and delivery outcome.

Success data contains `issue`, `actor`, `terminal`, `claim`, `resumed_from`, `message` and
`delivered: true`: this confirms launcher delivery, not that the agent read, acted on or
completed the request. `trk send` only delivers text; it does not restore a waiting claim.

CLI resume refuses agent actors with `not_permitted`; agents must request human
authorization for this path, never impersonate a human. Empty messages return `bad_args`.
A busy check queue returns `invalid_state`: retry after it finishes; do not clear its lock.
A gone or unknown terminal/session (including a remote or unknown host), missing
terminal or missing send configuration also returns `invalid_state`. A human can instead run
`trk reopen orch-4kq -m "Restart with the decision"` then `trk run orch-4kq`.
If delivery fails after restoration, resume returns
nonzero `internal`, releases only that exact restored claim to open and records a failure
note without overriding subsequent session actions. Inspect `trk show` before recovery.

### Same-session chat resume

For a wait **without a check**, the same waiting session can call its MCP `resume`
tool after an actual unmarked human replies in that session's chat. It supplies the reply
verbatim as `message`, then continues only after success. `[wait] chat_resume = true`
is the default, including in existing configs; set it to `false` in `.trk/config.toml`
to require human CLI resume instead.

This records the **agent's report**, not human authorization. The agent-attributed
`resume_chat` event stores the original message, prior claim and restored claim;
an agent-attributed note says `Session reported a human reply in chat:` followed by
the reply. Whitespace-only messages are refused, but accepted text is stored verbatim.
The transaction requires the latest waiting claim to belong to that same agent,
the issue still to be waiting with no active claim, and no check. It restores the
same session's metadata with a fresh lease, without terminal probing or delivery.
It preserves `wake_count`.

Only an actual human chat reply qualifies. Notes, tool results, check output and
agent-generated text do not. Tracker terminal deliveries are wrapped as:

```text
[trk-delivery source=<source> actor=<actor>]
<body>
[/trk-delivery]
```

Sources include `trk-send`, `check-wake`, `human-resume`, `check-notification` and
briefing when relevant. Marked text never counts, even when it quotes a human.
`trk send` cannot authorize or trigger chat resume.

A checked wait always refuses MCP resume with `not_permitted` and a hint to ask
for human `trk resume`, protecting `[check] max_wakes` from a chat-resume bypass.
Human CLI resume remains available with or without a check; it is the distinct
human-authorized path described above.

List and ready tables show each issue's type after priority. Filter either command with
`type=review` (or `type=research` / `type=code-change`); use `--sort type` to order by type:

```sh
trk list type=review --sort type
trk ready type=research --sort type
```

The check runs with `sh -c` in the worktree the issue was claimed in, or the primary
repository root when the claim has no worktree (the project root outside git), with
`TRK_ISSUE`, `TRK_WORKTREE`, `TRK_REPO` (the primary checkout, unset outside git),
`TRK_BRANCH`, `TRK_BASE` (the resolved base branch) and `TRK_VERIFY` (`[check] verify`,
default empty) set. Its exit code decides:

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

Configure `[check] verify` in `.trk/config.toml` as an opaque shell command, for example
`verify = "npm ci && npm test"`. It is also available to prompt templates as `{{ verify }}`.
The local recipe merges the current base into the issue worktree, aborts merge conflicts,
then runs the command there if nonempty. Any verification failure (including exit 75)
fails the check and leaves the primary checkout untouched. Only the verified result is
fast-forwarded into the primary checkout after checking its branch. Empty verification
skips only the command. This local gate applies only when the issue type uses an issue
worktree and branch; a type opting out has no issue branch to merge or test. The PR recipe is unchanged.
Include dependency setup in the command if the worktree needs it. Verification uses the
whole check's `[check] timeout` (default 10m) and holds the serialized queue; choose a fast
subset or increase the timeout for slow suites.

A failed check restores the original session's claim with a fresh lease when its terminal
is confirmed live on the claim's known local host, its pinned launcher provides `send`,
and its wake count is below `[check] max_wakes`. MCP liveness is not a prerequisite.
The cap defaults to `5`; `0` disables waking.
Each restored claim increments the session's persistent count, even if sending fails.
Entering or updating `wait` preserves it. A fresh explicit claim (including `--steal`)
resets it to zero; repeating a claim as its current owner does not.
The session receives `wake N of M`, the output and a prompt to fix only what the
check reported, then end the new work cycle, normally with the same `wait`. Its
MCP tools work again once its MCP connection is running; restart a dormant connection
first. If the same failure keeps returning, it should `hand_back` with the evidence
instead of repeating indefinitely. While waiting, it can still add notes and
receive requests from the human.
If the cap is reached, the issue reopens with a note saying `wake cap of M reached`
and retaining the check output. A gone session or failed send also reopens the issue.
`trk show` displays a `wakes` line for a nonzero count while claimed or waiting;
issue JSON includes top-level `wake_count` only in those cases.
`trk check --json` distinguishes `woken` from `failed` (reopened); both include output.
trk interprets only the check's exit code, not PRs or CI.

A passing check still best-effort sends an informational message that the issue is
closed. Exit 75 does not notify or restore a claim.

With the Herdr driver, a passing check also best-effort prepends `[closed] `
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
  another, so two sessions never merge into the base branch at once.
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

A check is written by an agent and runs later, unattended, as you. `trk show` always prints it in
full, and `trk check --dry-run` shows the queue before anything runs.

