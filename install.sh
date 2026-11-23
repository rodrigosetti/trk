#!/usr/bin/env bash
# Install or upgrade the trk binary from this checkout.
#
#   ./install.sh          build in release mode and install to ~/.cargo/bin/trk
#
# If the trk Herdr mod is installed (`trk herdr install`), it is refreshed too.
# trk keeps no global state. Set it up per project with `trk install`, run in
# the project's root: it creates .trk/ (the database and the default config)
# and installs the agent skill. After an upgrade, run `trk install` again in
# each project: it applies schema migrations, refreshes the skill, and lists
# the config files that differ from the new defaults without overwriting them.
set -euo pipefail

cd "$(dirname "$0")"
REPO="$(pwd)"
BIN="${CARGO_HOME:-$HOME/.cargo}/bin/trk"

command -v cargo >/dev/null || { echo "cargo not found: install Rust from https://rustup.rs" >&2; exit 1; }

echo "==> building and installing trk from $REPO"
cargo install --path crates/trk-cli --locked --force --quiet
echo "==> installed $("$BIN" --version) at $BIN"

# The Herdr mod is embedded in the binary: keep an installed copy in step with it.
"$BIN" herdr install --if-installed --format pretty | sed -n '/^herdr /p'

case ":$PATH:" in
  *":$(dirname "$BIN"):"*) ;;
  *) echo "warning: $(dirname "$BIN") is not on your PATH; add it to use \`trk\`" >&2 ;;
esac

echo "==> next: run \`trk install\` in each project's root directory"
echo "    (new projects: creates .trk/; existing ones: upgrades the database and the agent skill)"
echo "    in Herdr: \`trk herdr open\` shows the dashboard as a panel in the current space"
