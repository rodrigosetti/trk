//! Default configuration embedded for `trk install` and checkout-independent tests.
//!
//! Consumers needing files on disk must materialize these contents in their own
//! directory, never read the compile-time checkout at runtime.

pub const FILES: &[(&str, &str)] = &[
    (
        "types/code-change.toml",
        include_str!("../../../seed/types/code-change.toml"),
    ),
    (
        "types/research.toml",
        include_str!("../../../seed/types/research.toml"),
    ),
    (
        "types/review.toml",
        include_str!("../../../seed/types/review.toml"),
    ),
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

pub const CONFIG_TOML: &str = include_str!("../../../seed/config.toml");

/// `.trk/.gitignore`: everything in `.trk/` starts out ignored. Edit it to
/// share configuration through git.
pub const GITIGNORE: &str = "# Written by `trk install`: nothing in .trk/ is tracked.\n*\n";
