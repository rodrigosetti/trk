#!/usr/bin/env bash
# Install or upgrade trk from this checkout.
#
#   ./install.sh          build in release mode and install to ~/.cargo/bin/trk
#
# The agent skill is installed separately, per repository, with
# `trk skill install` (or `trk skill install --global`).
#
# First install: writes the default config (~/.config/trk) and applies it.
# Upgrade: replaces the binary (the database migrates itself on next use), then lists default-config files that differ from yours, since trk
# never overwrites your config.
set -euo pipefail

cd "$(dirname "$0")"
REPO="$(pwd)"
CONFIG="${TRK_CONFIG:-${XDG_CONFIG_HOME:-$HOME/.config}/trk}"
BIN="${CARGO_HOME:-$HOME/.cargo}/bin/trk"

command -v cargo >/dev/null || { echo "cargo not found: install Rust from https://rustup.rs" >&2; exit 1; }

echo "==> building and installing trk from $REPO"
cargo install --path crates/trk-cli --locked --force --quiet
echo "==> installed $("$BIN" --version) at $BIN"

case ":$PATH:" in
  *":$(dirname "$BIN"):"*) ;;
  *) echo "warning: $(dirname "$BIN") is not on your PATH; add it to use \`trk\`" >&2 ;;
esac

# Agent skills are installed per repository with `trk skill install`; their
# files come from the binary, so copies installed earlier may now be stale.
if "$BIN" skill status --global --format json 2>/dev/null | grep -q '"state": "outdated"'; then
  echo "==> the global trk skill is older than this build: run \`trk skill install --global\`"
fi
echo "    (refresh project skills with \`trk skill install\` in each repository; see \`trk skill status\`)"

if [ ! -d "$CONFIG/types" ]; then
  echo "==> first install: writing the default config to $CONFIG"
  "$BIN" init --format pretty
  exit 0
fi

# Upgrade: report default-config files that differ from the installed config.
changed=""
for f in $(cd seed && find . -type f | sed 's|^\./||' | sort); do
  if [ ! -f "$CONFIG/$f" ]; then
    changed="$changed\n  new      $f"
  elif [ "$f" = config.toml ]; then
    # Ignore the email line, which `trk init` fills in.
    if ! diff -q <(grep -vi 'email' "seed/$f") <(grep -vi 'email' "$CONFIG/$f") >/dev/null 2>&1; then
      changed="$changed\n  differs  $f"
    fi
  elif ! cmp -s "seed/$f" "$CONFIG/$f"; then
    changed="$changed\n  differs  $f"
  fi
done
if [ -n "$changed" ]; then
  printf "==> default config files that differ from %s:%b\n" "$CONFIG" "$changed"
  echo "    (your edits, or changes in this checkout; nothing was overwritten)"
  echo "    compare:  diff -u $REPO/seed/<file> $CONFIG/<file>"
  echo "    take one: cp $REPO/seed/<file> $CONFIG/<file> && trk schema apply"
  echo "    (config.toml holds your settings: merge it by hand instead of copying)"
else
  echo "==> your config matches the defaults in this checkout"
fi
"$BIN" schema plan --format pretty | sed 's/^/    /'
