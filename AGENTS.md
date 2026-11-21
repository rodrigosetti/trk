# Agent instructions

## Required workflow

- Track all work in `trk`. Create or select the relevant issue and claim it before making changes; record progress and the outcome on that issue.
- Start all implementation work through `trk run <ID> --json`. Sessions must work in the dedicated Git worktree on the issue-specific branch created by `trk run`, not in the primary checkout.
- Do not work or commit directly on `main` unless the user explicitly asks for or authorizes it. A general request to implement a change or commit it is not authorization to work on `main`.
- Before editing, verify the current branch and worktree. If you are on `main`, move the work to a dedicated worktree before making changes unless explicitly authorized otherwise.
- Keep changes and commits in the issue worktree. When implementation and verification are complete, request landing with `wait`: supply a check that merges the issue branch into `main` in the primary checkout. Do not merely poll whether someone else has merged it.
- Session-requested merges through `trk check` / `trk check --watch` are explicitly authorized and require no additional user approval. Let this serialized queue perform the merge, rather than merging directly from the session; a failed merge must fail the check so the issue reopens with feedback.
