//! The agent skill that teaches interactive agent sessions to use the trk
//! CLI, installed by `trk install`. Its files are embedded in the binary, so
//! an installed skill always matches the installed `trk`.
//!
//! It goes in `<project>/.agents/skills/trk` (omp, Codex), linked from
//! `<project>/.claude/skills/trk` (Claude Code). In a git repository both
//! paths are added to `.git/info/exclude`, so the skill is never tracked and
//! the worktrees `trk run` creates do not contain it: sessions reach trk
//! through MCP only.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use trk_core::error::Result;

use crate::env::{git, git_exclude, home};

pub const NAME: &str = "trk";

/// The skill's files, relative to its directory.
pub const FILES: &[(&str, &str)] = &[
    ("SKILL.md", include_str!("../../../skills/trk/SKILL.md")),
    (
        "references/reference.md",
        include_str!("../../../skills/trk/references/reference.md"),
    ),
];

pub const SKILL_DIR: &str = ".agents/skills/trk";
pub const CLAUDE_LINK: &str = ".claude/skills/trk";

/// "missing", "current", "outdated" or "linked" (a symlink, e.g. a development copy).
pub fn state(dir: &Path) -> &'static str {
    if dir.is_symlink() {
        return "linked";
    }
    if !dir.join("SKILL.md").exists() {
        return "missing";
    }
    let current = FILES
        .iter()
        .all(|(rel, content)| std::fs::read_to_string(dir.join(rel)).is_ok_and(|s| s == *content));
    if current { "current" } else { "outdated" }
}

fn write_files(dir: &Path) -> Result<()> {
    let tmp = dir.with_file_name(format!("{NAME}.new"));
    let _ = std::fs::remove_dir_all(&tmp);
    for (rel, content) in FILES {
        let p = tmp.join(rel);
        std::fs::create_dir_all(p.parent().unwrap())?;
        std::fs::write(&p, content)?;
    }
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::rename(&tmp, dir)?;
    Ok(())
}

/// What installing the skill did.
pub struct Installed {
    pub lines: Vec<String>,
    pub warnings: Vec<String>,
    pub data: Value,
}

/// Install or refresh the skill in the project at `root`.
pub fn install(root: &Path, is_git: bool) -> Result<Installed> {
    let dir = root.join(SKILL_DIR);
    let mut lines = vec![];
    let mut warnings: Vec<String> = vec![];
    if dir.is_symlink() {
        lines.push(format!(
            "{} is a symlink (a development copy?); left alone",
            dir.display()
        ));
    } else {
        let before = state(&dir);
        std::fs::create_dir_all(dir.parent().unwrap())?;
        write_files(&dir)?;
        lines.push(match before {
            "current" => format!("already current at {}", dir.display()),
            "missing" => format!("installed at {}", dir.display()),
            _ => format!("updated at {}", dir.display()),
        });
    }
    // Link for Claude Code when it is installed or the project already uses it.
    let link: Option<PathBuf> = (home().join(".claude").is_dir() || root.join(".claude").is_dir())
        .then(|| root.join(CLAUDE_LINK));
    if let Some(link) = &link {
        if link.is_symlink() || !link.exists() {
            std::fs::create_dir_all(link.parent().unwrap())?;
            if link.is_symlink() {
                std::fs::remove_file(link)?;
            }
            std::os::unix::fs::symlink(PathBuf::from("../../.agents/skills").join(NAME), link)?;
            lines.push(format!("linked {} for Claude Code", link.display()));
        } else {
            warnings.push(format!(
                "{} exists and is not a symlink; left alone",
                link.display()
            ));
        }
    }
    if is_git {
        git_exclude(root, &format!("/{SKILL_DIR}/"));
        if link.is_some() {
            git_exclude(root, &format!("/{CLAUDE_LINK}"));
        }
        for rel in [SKILL_DIR, CLAUDE_LINK] {
            if git(root, &["ls-files", "--", rel]).is_some_and(|s| !s.is_empty()) {
                warnings.push(format!(
                    "{rel} is tracked in git: worktrees created by `trk run` will contain it, so agent sessions will see the skill"
                ));
            }
        }
    }
    let data = json!({
        "path": dir,
        "state": state(&dir),
        "claude_link": link,
        "claude_linked": link.as_ref().map(|l| l.is_symlink()),
    });
    Ok(Installed {
        lines,
        warnings,
        data,
    })
}
