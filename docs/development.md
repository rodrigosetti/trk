For development: `cargo build`, `cargo test --workspace --locked`, and `cargo lint`.
`cargo lint` runs `cargo clippy --workspace --all-targets -- -D warnings`, rejecting
compiler and Clippy warnings in every workspace target, including tests and examples.
Keep build outputs local to each issue worktree; do not point `CARGO_TARGET_DIR`
or `--target-dir` at the primary checkout. Seed-dependent tests materialize the
installer's embedded defaults in temporary directories, so their binaries also
work after the checkout they were compiled in has been removed.

GitHub Actions runs the workspace tests, lint, and Herdr mod tests on
`ubuntu-latest` and `macos-latest` for pushes and pull requests. CI reads the Rust
toolchain version from `workspace.package.rust-version` in `Cargo.toml` and uses
Node.js 24 for `node --experimental-vm-modules --test herdr/mods/trk/main.test.mjs`.

## Layout

| Crate | Role |
| --- | --- |
| `trk-core` | The state machine, claims and leases, the waiting queue, query language, `Store` (the only writer) |
| `trk-agent` | Agent definitions, the seven trk tools and their executor, prompt rendering, harness profiles, launch spec |
| `trk-mcp` | The per-issue MCP server (rmcp, single-threaded Tokio) |
| `trk-cli` | The `trk` binary, rendering, `trk check`, `trk run` and the launcher templates |

`seed/` holds the default configuration, embedded into the binary for `trk install`.
`herdr/` holds the Herdr mod and layout, embedded the same way.

## Notes on the implementation

- **Harness names**: the Claude Code profile is `claude-code`, matching `agent:claude-code:<id>`.
  A harness is registered the first time one of its sessions acts.
- **omp** (verified live): reads `<worktree>/.omp/mcp.json` and shows MCP server instructions to the
  model, so no folding is needed. Its `--tools` flag hides MCP tools that are not listed, so profiles
  use a MiniJinja loop over `trk_tools` to prefix each tool name with `mcp__trk_`.
  It keeps `write` available even with a restricted `--tools` list, so `builtin_tools = "read-only"`
  is not fully enforced there.
- **Unverified harness profiles**: the Claude Code, Codex, OpenCode and pi profiles were not run live.
- **Session liveness** comes from the local claim's terminal via its pinned launcher's `exists` operation or Herdr driver.
  MCP pid, host and exit metadata in `actor.meta` report MCP status separately, not session death.
- **`trk check` lock**: a row holding the pid and host of the running pass. A holder whose process
  is gone on this machine is replaced; a holder on another host is never replaced automatically.
- **`TRK_NOW`** fixes the clock, for tests.

