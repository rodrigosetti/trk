//! The default configuration written by `trk init` (seed data, not code).

pub const FILES: &[(&str, &str)] = &[
    (
        "agents/implementer.toml",
        include_str!("../../../seed/agents/implementer.toml"),
    ),
    (
        "agents/researcher.toml",
        include_str!("../../../seed/agents/researcher.toml"),
    ),
    (
        "agents/reviewer.toml",
        include_str!("../../../seed/agents/reviewer.toml"),
    ),
    (
        "prompts/_rules.md.j2",
        include_str!("../../../seed/prompts/_rules.md.j2"),
    ),
    (
        "prompts/briefing.md.j2",
        include_str!("../../../seed/prompts/briefing.md.j2"),
    ),
    (
        "prompts/implementer.instructions.md.j2",
        include_str!("../../../seed/prompts/implementer.instructions.md.j2"),
    ),
    (
        "prompts/researcher.instructions.md.j2",
        include_str!("../../../seed/prompts/researcher.instructions.md.j2"),
    ),
    (
        "prompts/reviewer.instructions.md.j2",
        include_str!("../../../seed/prompts/reviewer.instructions.md.j2"),
    ),
];

/// Files of the six-type configuration that `trk init` no longer writes and
/// nothing reads. Reported, never deleted.
pub const STALE: &[&str] = &[
    "types",
    "fields.toml",
    "dep-kinds.toml",
    "agents/decider.toml",
    "agents/operator.toml",
    "prompts/decider.instructions.md.j2",
    "prompts/operator.instructions.md.j2",
];

pub const CONFIG_TOML: &str = include_str!("../../../seed/config.toml");
