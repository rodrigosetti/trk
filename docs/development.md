# Development

For development: `cargo build`, `cargo test --workspace --locked`, `cargo lint`,
and `cargo fmt-check`. `cargo lint` runs
`cargo clippy --workspace --all-targets -- -D warnings`, rejecting compiler and
Clippy warnings in every workspace target, including tests and examples.
`cargo fmt-check` runs `cargo fmt --all -- --check`; use `cargo fmt --all` to fix
formatting before committing.
Keep build outputs local to each issue worktree; do not point `CARGO_TARGET_DIR`
or `--target-dir` at the primary checkout. Seed-dependent tests materialize the
installer's embedded defaults in temporary directories, so their binaries also
work after the checkout they were compiled in has been removed.

GitHub Actions runs the workspace tests, lint, and Herdr mod tests on
`ubuntu-latest` and `macos-latest` for pushes and pull requests. The Rust matrix
covers both the minimum supported Rust version (MSRV), read from
`workspace.package.rust-version` in `Cargo.toml`, and current stable. Herdr mod
tests use Node.js 24 and
`node --experimental-vm-modules --test herdr/mods/trk/main.test.mjs`.

Separate CI jobs check formatting with stable Rust and scan all dependency
features against RustSec advisories. To run the dependency check locally:

```sh
cargo install cargo-deny --locked
cargo deny --locked --all-features check advisories --warn unmaintained
```

Known vulnerabilities fail CI. Unmaintained-only advisories remain warnings:
they report maintenance risk, not a known vulnerability (currently including
transitive `chrono` through `rmcp` and `schemars`). This is an advisory check,
not a license policy check.

Rust dependency builds (including bundled SQLite) are cached by OS, toolchain,
and Cargo manifests/lockfile; only `main` runs save caches, while pull requests
can restore them. Third-party actions in both workflows use full commit SHAs.
Dependabot checks Cargo dependencies and GitHub Actions weekly; review and land
its update PRs to keep the lockfile and action pins current.

## Releasing 0.1.0

The first public release is **0.1.0 (beta)**. A human performs the release after
the release changes have landed on `main`; agents must not create or push the tag.

1. In the release checkout, ensure `main` is up to date and the working tree is clean.
   Confirm `workspace.package.version` in `Cargo.toml` is `0.1.0`.
2. Review [CHANGELOG.md](https://github.com/rodrigosetti/trk/blob/main/CHANGELOG.md), replace the pending release date with the
   actual release date, and commit and land that change before tagging.
3. Run `cargo test --workspace --locked`, `cargo lint`, and
   `node --experimental-vm-modules --test herdr/mods/trk/main.test.mjs` on the release
   commit. Resolve failures before proceeding.
4. Finish any active trk sessions before running `./install.sh`, then `trk --version`.
   A Git checkout build prints
   `trk 0.1.0 (<commit>)`, where `<commit>` is the build's abbreviated Git commit
   (requested at 10 characters, longer if needed for uniqueness). Check that it
   matches the release commit. Without Git metadata, it prints `trk 0.1.0`.
   The version comes from Cargo plus the build commit, not the tag name.
5. **Human only:** create an annotated tag on that tested commit and push it:

   ```sh
   git tag -a v0.1.0 -m "trk 0.1.0"
   git push origin v0.1.0
   ```

Before installing the release, users should finish active sessions and follow the
[upgrade procedure](install-upgrade.md).

## Layout

| Crate | Role |
| --- | --- |
| `trk-core` | The state machine, claims and leases, the waiting queue, query language, `Store` (the only writer) |
| `trk-agent` | Agent definitions, the eight trk tools and their executor, prompt rendering, harness profiles, launch spec |
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
- **Claude Code** (verified live on 2026-11-24, version **2.1.331**, `--model sonnet`,
  resolved to Sonnet 5.5): tested in throwaway Git repositories through `trk install`
  and `trk run`, with Herdr/`hc` terminals. The original profile stopped at workspace
  trust; after approval its default auto mode did not prompt for MCP or Bash.
  The explicit-permission profile started in a fresh repository without manual
  input, received MCP server instructions, called `get_task`/`add_note`/`wait`,
  created and edited a file, committed in its issue worktree, and landed through
  `trk check` using the seed merge recipe. Failed-check wake and `trk send`
  acknowledgements were verified. `read-only` and `none` were also exercised live.
- **Unverified harness profiles**: the Codex, OpenCode and pi profiles were not run live.
- **Session liveness** comes from the local claim's terminal via its pinned launcher's `exists` operation or Herdr driver.
  MCP pid, host and exit metadata in `actor.meta` report MCP status separately, not session death.
- **`trk check` lock**: a row holding the pid and host of the running pass. A holder whose process
  is gone on this machine is replaced; a holder on another host is never replaced automatically.
- **`TRK_NOW`** fixes the clock, for tests.

## Building the manual

The [manual](https://rodrigosetti.github.io/trk/) is built with
[mdBook](https://rust-lang.github.io/mdBook/). Install the version pinned in
`.github/workflows/pages.yml`, then build or preview from the repository root:

```sh
cargo install mdbook --version 0.5.4 --locked
mdbook build
mdbook serve --open
```

`book.toml` uses `docs/` as its source and `docs/SUMMARY.md` as its navigation.
Add new chapters to the summary; generated HTML goes in the ignored `book/`
directory. Use relative links between chapters and absolute GitHub URLs for
repository files outside `docs/`, so links work both in source and on Pages.

The **Manual** workflow builds pull requests, pushes to `main`, and manual runs.
Only builds on `main` deploy, using the `github-pages` environment. A repository
administrator must enable **Settings → Pages → Build and deployment → Source:
GitHub Actions** before the first deployment. No deployment branch or token
secret is required; the deploy job uses the workflow's Pages and OIDC permissions.
