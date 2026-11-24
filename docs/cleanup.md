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

Sessions use each historical claim's pinned launcher, never today's selection order.
Terminals without a recorded launcher profile are skipped, including during
`trk check --watch` sweeps; their independently owned Git resources remain eligible.
GC invokes the profile's optional `close` operation; Herdr finds the exact recorded
terminal and closes its containing space, protecting your current space and other issues' active or
waiting sessions. Legacy claims recording the primary checkout as their working directory
do not establish ownership of a disposable worktree or branch; their sessions remain
eligible, and they do not block cleanup of actual issue worktrees recorded by other claims.
Already-missing worktree paths are reported as absent, even outside today's configured
directory. Existing worktrees are removed only below the configured worktree directory,
never the primary checkout or current worktree. Git worktree removal is not forced;
branch deletion uses `git branch -d` in the primary checkout, only for the issue-ID branch
and only after all recorded issue worktrees are removed or absent. Dirty or locked
worktrees, unmerged branches, existing outside-directory worktrees and other unsafe or
refused targets are reported as skipped. A missing directory's remaining Git registration
can still prevent branch deletion; GC does not force or prune it.
Runs without a recorded issue worktree have no worktree or branch resources for GC to
collect; their recorded sessions remain eligible for normal session cleanup.

JSON uses the normal envelope with `data: {dry_run, issues}`. Each issue has `id` and
`session`, `worktree`, `branch` outcomes with `status` (`removed`, `absent`, `skipped`) and
`reason`. Session and worktree outcomes also have `items`, identifying each `terminal` /
`space` or `path` with its own status and reason. Dry runs write no notes and remove
nothing: planned removals use `removed` with a `would remove ...` reason and
`dry_run: true`. Real removals append caller-attributed notes; rerunning when resources
are already absent adds no cleanup notes.

