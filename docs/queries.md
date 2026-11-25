# Queries and saved views

`trk list QUERY` and `trk ready QUERY` query the current project's database, not
other projects. `ready` adds the readiness filter to the same query language.
Quote the query so your shell does not interpret operators, spaces, or globs.

```sh
trk list 'state!=closed'
trk ready 'priority<=P1'
trk list 'state in (open,claimed) type=code-change'
trk list 'epic=auth-rework'
trk ready 'epic=""'
trk list 'claimed-by=me'
trk list 'claimed-by=agent:*'
trk list 'blocks=orch-m1 state!=closed'
trk list 'closed>-7d reason=done'
trk list 'text~"feature flag"'
```

## Syntax

Terms are joined by implicit AND. Prefix a term with `NOT ` or `!` to negate it.
Use `name in (a,b,c)` to match any listed value for one field; there is no general
OR operator or grouping syntax.

```text
name=value  name!=value  name<value  name<=value  name>value  name>=value
name~value
name in (a,b,c)
NOT term  !term
ready  blocked  due  stale-lease
```

| Field | Meaning |
| --- | --- |
| `id`, `title`, `state`, `priority`, `type`, `reason` | Issue ID, authored fields, state, or close reason (`done`, `abandoned`) |
| `epic` | Curated epic name (including archived epics); `epic=""` matches unassigned issues, `epic!=""` matches assigned issues |
| `claimed-by`, `created-by` | Claimant or creator actor |
| `created`, `updated`, `closed`, `next-check` | Timestamps |
| `text` | Full-text search over titles, descriptions, and notes |
| `blocks`, `blocked-by` | Issues on either side of a dependency edge |

| Flag | Matches |
| --- | --- |
| `ready` | Open issues with every blocker closed |
| `blocked` | Nonclosed issues with at least one nonclosed blocker |
| `due` | Waiting issues with a check due now |
| `stale-lease` | Claims whose lease has expired; reading does not sweep them |

Values can be words or quoted strings. Unquoted `me` resolves to the acting actor.
Times accept `now`, relative durations such as `-7d`, `-12h`, and `+30m`, dates
such as `2026-11-01` (midnight UTC), or RFC 3339 timestamps.

For ordinary text fields, `=` and `!=` accept SQLite globs (`*`, `?`, `[...]`),
and `~` matches a glob or, without glob characters, a substring. For `text`, use
`text~"feature flag"` for a full-text phrase. Dependency fields resolve issue IDs,
including unique suffixes, rather than matching arbitrary strings.

## Output and ordering

Both commands default to priority, then oldest creation time, then ID.
`--sort` accepts comma-separated keys; prefix a key with `-` for descending order.
Keys are `id`, `state`, `title`, `priority`, `type`, `epic`, `created`, `updated`,
`closed`, `claimed-by`, `lease`, and `next-check`. Issues without an epic sort last
for both `epic` and `-epic`. ID breaks remaining ties.

```sh
trk list 'state=waiting' --sort next-check
trk list 'state!=closed' --sort priority,-updated --limit 20
trk list --sort epic,priority --fields id,epic,title --json
trk ready --fields id,priority,type,title --json
```

`--fields` chooses pretty-table columns or JSON object keys. `--limit` caps the
number of issues. See [JSON API and errors](json.md) for output formats.

## Saved views

Define named views in project or user `config.toml`:

```toml
[views.mine]
query = "claimed-by=me"

[views.waiting]
query = "state=waiting"
fields = ["id", "priority", "waiting", "next-check", "title"]
sort = "next-check"
```

Run `trk list @mine` or `trk list '@waiting priority<=P1'`. Extra terms narrow the
view; explicit `--fields`, `--sort`, and `--limit` override its defaults. Settings
follow the normal [configuration layers](configuration.md).
