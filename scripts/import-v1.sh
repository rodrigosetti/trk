#!/usr/bin/env bash
# Import the open issues of a six-type trk database into the current one.
#
# trk no longer has issue types, type fields or dependency kinds other than
# "blocks", and the binary does not migrate old databases. This script
# recreates an export made with the OLD binary under the new model.
#
# 1. With the old binary, export (before upgrading):
#
#      D=export-v1; mkdir -p "$D"
#      trk project list --json > "$D/projects.json"
#      trk repo list --json    > "$D/repos.json"
#      trk list 'category!=closed' --json --limit 100000 | jq -r '.data[].id' |
#        while read -r id; do
#          trk show "$id" --with notes,events,deps,claims --json | jq -c .data
#        done > "$D/issues.jsonl"
#
# 2. Move the old database aside, install the new binary, run `trk init --force`,
#    and delete the config files it lists as no longer used (the old agent
#    files do not parse any more).
#
# 3. Import:   scripts/import-v1.sh export-v1
#
# What is carried over, per issue:
#   id, title, priority      unchanged
#   context                  becomes the description, followed by the outcome,
#                            the type's fields, links, tags and non-blocking
#                            edges as plain text
#   agent                    kept when the agent still exists
#   notes                    replayed in order, each prefixed with its author and date
#   blocks edges             kept when both issues are in the export
#   claimed issues           come back open, with a note saying who had them
# Closed issues, events and claim history stay in the old database.
#
# Safe to run again: issues that already exist are skipped.
set -euo pipefail

DIR="${1:?usage: import-v1.sh EXPORT_DIR}"
TRK="${TRK:-trk}"
for f in projects.json repos.json issues.jsonl; do
  [ -f "$DIR/$f" ] || { echo "missing $DIR/$f" >&2; exit 1; }
done
command -v jq >/dev/null || { echo "jq not found" >&2; exit 1; }
DIR="$(cd "$DIR" && pwd)"

t() { "$TRK" --json "$@"; }

echo "==> projects"
jq -r '.data[] | [.id, .name] | @tsv' "$DIR/projects.json" |
  while IFS=$'\t' read -r id name; do
    if t project list | jq -e --arg id "$id" '.data[] | select(.id == $id)' >/dev/null; then
      echo "    $id (exists)"
    else
      t project add "$id" --name "$name" >/dev/null
      echo "    $id"
    fi
  done

echo "==> repos"
jq -r '.data[] | [.project, (.local_path // ""), (.remote_url // "")] | @tsv' "$DIR/repos.json" |
  while IFS=$'\t' read -r project path remote; do
    if [ -n "$path" ] && [ -d "$path" ]; then target="$path"; else target="$remote"; fi
    [ -n "$target" ] || continue
    t --project "$project" repo add "$target" >/dev/null
    echo "    $project  $target"
  done

AGENTS="$(t agent list | jq -c '[.data[].name]')"
IDS="$(jq -sc '[.[].id]' "$DIR/issues.jsonl")"

# The description: the old context, then everything that no longer has a field.
DESCRIPTION='
  def text: if type == "array" then map(tostring) | join(", ") else tostring end;
  def present: . != null and . != "" and . != [];
  (.fields // {} | to_entries
     | map(select(.key != "priority" and .key != "agent" and (.value | present)))) as $fields
  | ((.deps.out // []) | map(select(.kind != "blocks")
       | "- this \(.kind) \(.id) (\(.type), \(.status)): \(.title)")) as $out
  | ((.deps.in // []) | map(select(.kind != "blocks")
       | "- \(.id) \(.kind) this (\(.type), \(.status)): \(.title)")) as $in
  | [ .context,
      (if .outcome | present then "## Outcome\n\n\(.outcome)" else empty end),
      (if ($fields | length) > 0 or (.owner | present) or (.tags | present) then
         "## Details\n\nImported from a `\(.type)` issue.\n\n"
         + ([ $fields[] | "- **\(.key)**: \(.value | text)" ]
            + (if .owner | present then ["- **owner**: \(.owner)"] else [] end)
            + (if .tags | present then ["- **tags**: \(.tags | join(", "))"] else [] end)
            | join("\n"))
       else "Imported from a `\(.type)` issue." end),
      (if (.links | present) then
         "## Links\n\n" + (.links | map("- \(.kind // "link"): \(.url)\(if .note then " (\(.note))" else "" end)") | join("\n"))
       else empty end),
      (if ($out + $in | length) > 0 then "## Related\n\n" + ($out + $in | join("\n")) else empty end)
    ] | join("\n\n")'

echo "==> issues"
created=0; skipped=0
while IFS= read -r issue; do
  id="$(jq -r .id <<<"$issue")"
  if t show "$id" </dev/null >/dev/null 2>&1; then
    skipped=$((skipped + 1)); echo "    $id (exists)"; continue
  fi
  project="$(jq -r .project <<<"$issue")"
  title="$(jq -r .title <<<"$issue")"
  priority="$(jq -r '.fields.priority // "P2"' <<<"$issue")"
  agent="$(jq -r --argjson agents "$AGENTS" '.fields.agent // "" | select(. as $a | $agents | index($a)) // ""' <<<"$issue")"
  # A new issue takes the repo of the directory it is created in.
  repo="$(jq -r '.fields.repo // ""' <<<"$issue")"
  cwd="/"
  if [ -n "$repo" ]; then
    path="$(jq -r --arg r "$repo" '.data[] | select(.remote_url == $r or .local_path == $r) | .local_path // ""' "$DIR/repos.json" | head -1)"
    if [ -n "$path" ] && [ -d "$path" ]; then cwd="$path"; fi
  fi
  args=(--project "$project" create "$title" --id "$id" -p "$priority" -d @-)
  [ -n "$agent" ] && args+=(--agent "$agent")
  jq -r "$DESCRIPTION" <<<"$issue" | (cd "$cwd" && t "${args[@]}") >/dev/null
  # Notes, oldest first, with who wrote them and when.
  jq -c '.notes // [] | .[]' <<<"$issue" | while IFS= read -r note; do
    jq -r '"_Imported note by \(.actor), \(.at)._\n\n\(.body)"' <<<"$note" | t note "$id" - >/dev/null
  done
  # A claim does not survive: say who had it and where the work is.
  if [ "$(jq -r '.claim != null' <<<"$issue")" = true ]; then
    jq -r '"_Imported while claimed by \(.claim.actor) in status `\(.status)`. The claim was not carried over._"
           + (if .claim.worktree then "\n\nWorktree: `\(.claim.worktree)`" else "" end)
           + (if .fields.branch then "\n\nBranch: `\(.fields.branch)`" else "" end)' <<<"$issue" |
      t note "$id" - >/dev/null
  fi
  created=$((created + 1)); echo "    $id  $title"
done < "$DIR/issues.jsonl"

echo "==> blockers"
edges=0
while IFS=$'\t' read -r blocker blocked; do
  t dep add "$blocker" "$blocked" >/dev/null
  edges=$((edges + 1)); echo "    $blocker blocks $blocked"
done < <(jq -r --argjson ids "$IDS" '
  .id as $me
  | ((.deps.out // [])[] | select(.kind == "blocks") | select(.id as $o | $ids | index($o)) | [$me, .id]),
    ((.deps.in  // [])[] | select(.kind == "blocks") | select(.id as $o | $ids | index($o)) | [.id, $me])
  | @tsv' "$DIR/issues.jsonl" | sort -u)

echo "==> done: $created created, $skipped already there, $edges blocker edges"
echo "    open blockers that were closed in the old database were dropped: they no longer block"
