#!/usr/bin/env bash
# Install or upgrade trk from this checkout.
#
#   ./install.sh          build in release mode and install to ~/.cargo/bin/trk
#
# Also installs the `trk` agent skill to ~/.agents/skills/trk (read by omp
# and Codex), linked from ~/.claude/skills/trk for Claude Code. The skill is
# copied, not linked, so it always matches the installed binary.
#
# First install: writes the default config (~/.config/trk) and applies it.
# Upgrade: replaces the binary and the skill (the database migrates itself on
# next use), then lists default-config files that differ from yours, since trk
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

# The agent skill: ~/.agents/skills/trk, plus a link for Claude Code.
SKILLS="${AGENTS_SKILLS_DIR:-$HOME/.agents/skills}"
if [ -L "$SKILLS/trk" ]; then
  echo "==> $SKILLS/trk is a symlink (development setup); left alone"
else
  mkdir -p "$SKILLS"
  rm -rf "$SKILLS/trk.new"
  cp -R skills/trk "$SKILLS/trk.new"
  rm -rf "$SKILLS/trk"
  mv "$SKILLS/trk.new" "$SKILLS/trk"
  echo "==> installed the trk skill at $SKILLS/trk"
fi
if [ -d "$HOME/.claude" ]; then
  link="$HOME/.claude/skills/trk"
  mkdir -p "$HOME/.claude/skills"
  if [ -L "$link" ] || [ ! -e "$link" ]; then
    ln -sfn "$SKILLS/trk" "$link"
    echo "==> linked $link for Claude Code"
  else
    echo "warning: $link exists and is not a symlink; left alone" >&2
  fi
fi

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
