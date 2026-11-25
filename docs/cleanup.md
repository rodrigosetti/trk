## Cleaning up closed issues

Closing retains the session, worktree and branch; use explicit garbage collection after work has landed:

```sh
trk gc --dry-run --json                   # preview all closed-as-done issues
trk gc orch-4kq --json                    # clean up one issue
trk gc --older-than 7d --abandoned --json # include abandoned issues closed over a week ago
```

`trk gc [ID...] [--legacy-hc] [--dry-run] [--abandoned] [--older-than DURATION] [--json]` defaults to
closed-as-done issues; `--abandoned` also includes abandoned issues. It never collects open,
claimed or waiting issues, and serializes cleanup with the check queue.
All historical claims are considered, but only resources on this host are eligible.

GC snapshots candidates in a short database transaction, then revalidates each issue
before reserving it for removal. Reopened or reclaimed issues invalidate the snapshot
and are skipped, even if closed again before GC reaches them. While a target's removal
is in progress, reopening that target is refused with a retry hint. Other issues'
writes, including claim renewals, remain available: Git and launcher commands run
outside database transactions. Results and cleanup notes are recorded in a second
short transaction. Reservations are released with queue ownership, including when a
later runner recovers a dead owner's lock.

Once every recorded resource is removed or proven absent, GC records completion.
Automatic `trk check` sweeps skip completed issues without probing their resources
again. Any skipped resource or failed operation keeps the issue eligible for retry;
reopening clears completion. Explicit `trk gc ID` still re-inspects completed issues.
Dry runs do not record completion or reserve issues.

Sessions use each historical claim's pinned launcher, never today's selection order.
Terminals without a recorded launcher profile are normally skipped, including during
`trk check --watch` sweeps; their independently owned Git resources remain eligible.
GC invokes the profile's optional `close` operation; Herdr finds the exact recorded
terminal and closes its containing space, protecting your current space and other issues' active or
waiting sessions. Historical claims whose working directory canonically resolves to the
local primary checkout (including symlink aliases) do not establish ownership of a
disposable worktree or issue-ID branch; their sessions remain eligible, and they do not
block cleanup of actual issue worktrees recorded by other claims.
Already-missing worktree paths are reported as absent before deletion-boundary checks,
even outside today's configured directory. Existing worktrees are removed only below
the configured worktree directory, never the primary checkout or current worktree.
Git worktree removal is not forced;
branch deletion uses `git branch -d` in the primary checkout, only for the issue-ID branch
and only after all recorded issue worktrees are removed or absent. Dirty or locked
worktrees, unmerged branches, existing outside-directory worktrees and other unsafe or
refused targets are reported as skipped. A missing directory's remaining Git registration
can still prevent branch deletion; GC does not force or prune it.
Runs without a recorded issue worktree have no worktree or branch resources for GC to
collect; their recorded sessions remain eligible for normal session cleanup.

### Recovering historical hc sessions

For historical claims with a NULL or empty launcher profile, explicitly select issue IDs
and opt in only after independently verifying that the sessions came from `hc`:

```sh
trk gc orch-4kq --legacy-hc --dry-run --json
trk gc orch-4kq --legacy-hc --json
```

`--legacy-hc` requires at least one explicit issue ID. It asserts independently verified
provenance; terminal spelling, aliases and today's default launcher are not evidence.
It never selects a launcher profile or changes cleanup for claims with a named profile.
All normal eligibility, host and resource-safety gates remain in force. Your current
`HC_TERMINAL_ID` and other issues' active or waiting terminal identities are protected.

Recovery inventories sessions with `hc ls --all --json`, matching only the exact recorded
address, never an alias or name. A valid inventory without that address proves absence;
an unavailable, failed or malformed inventory does not. For a present address,
`hc get ADDRESS --json` must confirm the exact address, a finished state, and the claim's
working directory (or project root when none was recorded). Its argv must contain the
exact `TRK_ACTOR=<actor>` word for that claim; shell command strings are split on ASCII
whitespace for this check. Only a finished session with matching identity is removed,
using `hc rm ADDRESS --json`. Live, ambiguous, mismatched or unavailable sessions are
skipped; recovery never kills a live session.

A successful real removal or proven absence appends a structured, GC-only note receipt:
`gc_session: {claim: ID, terminal: ADDRESS, host: HOST, backend: "hc"}`.
The receipt is bound to that historical claim; its original launcher and all other claim
data remain unchanged. Later ordinary GC and pending check sweeps consult the receipt
and recheck `hc`, allowing an absent session to be reported without repeating the
missing-profile warning. Completed issues are omitted from automatic sweeps as above.
A receipt is not proof of continued absence or a general launcher override.
Failed or skipped recovery writes no receipt, and dry runs neither mutate resources nor
write notes.

JSON uses the normal envelope with `data: {dry_run, issues}`. Each issue has `id` and
`session`, `worktree`, `branch` outcomes with `status` (`removed`, `absent`, `skipped`) and
`reason`. Session and worktree outcomes also have `items`, identifying each `terminal` /
`space` or `path` with its own status and reason. Dry runs write no notes and remove
nothing: planned removals use `removed` with a `would remove ...` reason and
`dry_run: true`. Real removals append caller-attributed notes; rerunning when resources
are already absent adds no cleanup notes, except for the first successful legacy-hc
recovery receipt described above.

