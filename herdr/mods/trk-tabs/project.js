export function projectLocation(space, allPanes) {
  for (const tab of space?.tabs ?? []) {
    const pane = allPanes.find((pane) => pane.tab === tab.id && pane.terminal && pane.cwd);
    if (pane) return { cwd: pane.cwd, on: space.machine ?? null };
  }
  return { cwd: space?.folder || null, on: space?.machine ?? null };
}

// Keep discovery and all filesystem access on the machine owning the space.
const probe = `
set -eu
export LC_ALL=C GIT_DISCOVERY_ACROSS_FILESYSTEM=1
command -v git >/dev/null 2>&1 || exit 1
cd -P -- "$1"
candidate=$(pwd -P)
if root=$(git rev-parse --show-toplevel 2>&1); then
  git_dir=$(git rev-parse --git-dir) || exit 1
  common_dir=$(git rev-parse --git-common-dir) || exit 1
  git_dir=$(cd -P -- "$git_dir" && pwd -P) || exit 1
  common_dir=$(cd -P -- "$common_dir" && pwd -P) || exit 1
  [ "$git_dir" = "$common_dir" ] || exit 1
  root=$(cd -P -- "$root" && pwd -P) || exit 1
else
  # Only ordinary non-repositories qualify here; Git errors fail closed.
  [ "$root" = 'fatal: not a git repository (or any of the parent directories): .git' ] || exit 1
  root=$candidate
fi
[ -d "$root/.trk" ] && [ ! -L "$root/.trk" ] || exit 1
[ -f "$root/.trk/trk.db" ] && [ ! -L "$root/.trk/trk.db" ] || exit 1
printf '%s\\000' "$root"
`;

export async function probeProject(exec, location) {
  if (!location?.cwd) return null;
  try {
    const result = await exec("sh", ["-c", probe, "trk-project", location.cwd], {
      cwd: location.cwd,
      on: location.on ?? null,
      timeout: 5000,
    });
    if (result.code !== 0 || typeof result.stdout !== "string") return null;
    const root = result.stdout;
    if (!root.startsWith("/") || root.indexOf("\0") !== root.length - 1) return null;
    return { cwd: root.slice(0, -1), on: location.on ?? null };
  } catch {
    return null;
  }
}
