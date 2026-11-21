# Agent instructions

## Required workflow

- Track all work in `trk`. Create or select the relevant issue and claim it before making changes; record progress and the outcome on that issue.
- Do all work in a dedicated Git worktree on an issue-specific branch, not in the primary checkout. Use `trk run <ID> --json` when launching an agent, or create a worktree manually when working directly.
- Do not work or commit directly on `main` unless the user explicitly asks for or authorizes it. A general request to implement a change or commit it is not authorization to work on `main`.
- Before editing, verify the current branch and worktree. If you are on `main`, move the work to a dedicated worktree before making changes unless explicitly authorized otherwise.
- Keep changes and commits in the issue worktree. Do not merge into `main` without explicit user authorization.
