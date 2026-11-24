### Launcher profiles

`[launcher] use` tries named profiles in order and selects the first available one.
When writing a built-in config, `trk install` probes Herdr's UI, `hc ls` and
`tmux -V`, then keeps available profiles in that order, including tmux automatically.
All three profile definitions remain in the config. An `available` argv must succeed;
without it a profile is usable (the Herdr driver probes its UI). No available profile
is an error before claiming or starting a session; the hint lists profiles tried and
suggests installing a launcher, editing `[launcher] use`, or `trk run --fg`.
The selected name is recorded on the claim, and attach/focus, send, resume, checks and
GC keep using that profile even after `use` changes. Keep profiles needed by historical
sessions in the config.

Profiles accept optional `available`, `start`, `attach`, `send`, `exists`, `focus`,
`close` and `rename` argv arrays. Missing operations are skipped. Entries remain
separate arguments, with substitutions `{name}` (session address), `{cwd}`,
`{shell_command}` (launch command including directory and environment), `{text}`
and `{title}`. Start's first stdout line replaces the generated session name;
empty stdout retains it. Stderr is diagnostics, never a session address.
The tmux seed requests a stable session ID so rename does not break later operations.
Retry settings on `[launcher]` apply to profiles unless overridden there.

Each profile can set `name = "trk-{id}"`, `name_max = 32` and
`name_chars = "a-z0-9_-"` (the defaults). Templates accept `{id}` (full issue ID),
`{prefix}`, `{project}` (project-root directory name), `{agent}` and `{type}`.
Use, for example, `name = "{project}-{id}"` to distinguish projects sharing a prefix.
Names are ASCII-lowercased, characters outside `name_chars` become `-`, and the
result is limited to `name_max` characters. Character sets accept inclusive ranges
and literal characters; a trailing `-` is literal. Changing naming rules does not
rename existing sessions: later operations use the address recorded on the claim.

`driver = "herdr"` supplies Herdr's start, exists, focus, close and rename defaults;
the seed's attach and send still use `hc`. Each run opens a **new Herdr space** named
with the issue title and rooted in the launch directory, leaving the launching space
and tabs intact. JSON includes the actual terminal address and `placement` (`visible`
or `failed`). Observation timeout warns without ending the session. An uncertain
creation response retains the claim and never starts a duplicate session. When
Herdr is unavailable, selection tries the next profile in `[launcher] use`.

