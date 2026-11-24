//! The agent skill that teaches interactive agent sessions to use the trk
//! CLI, installed by `trk install`. Its files are embedded in the binary, so
//! an installed skill always matches the installed `trk`.
//!
//! Its directory and symlinks come from `[skill]`. In a git repository these
//! paths are added to `.git/info/exclude`, so sessions in `trk run` worktrees
//! reach trk through MCP only.

use std::borrow::Cow;
use std::path::Path;

use serde_json::{Value, json};
use trk_core::config::SkillSection;
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

/// Resolve old configs using the original harness detection. Once installed,
/// the explicit section is authoritative, even if another harness appears.
pub fn settings<'a>(root: &Path, configured: Option<&'a SkillSection>) -> Cow<'a, SkillSection> {
    configured.map(Cow::Borrowed).unwrap_or_else(|| {
        Cow::Owned(SkillSection {
            path: ".agents/skills/trk".into(),
            links: if home().join(".claude").is_dir() || root.join(".claude").is_dir() {
                vec![".claude/skills/trk".into()]
            } else {
                vec![]
            },
        })
    })
}

/// No locations are active when the skill is disabled.
pub fn paths(config: &SkillSection) -> impl Iterator<Item = &str> {
    std::iter::once(config.path.as_str())
        .chain(config.links.iter().map(String::as_str))
        .filter(|_| !config.path.is_empty())
}

pub fn configured_state(root: &Path, config: &SkillSection) -> &'static str {
    if config.path.is_empty() {
        "disabled"
    } else {
        state(&root.join(&config.path))
    }
}

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
    let mut tmp_name = dir.file_name().unwrap().to_os_string();
    tmp_name.push(".new");
    let tmp = dir.with_file_name(tmp_name);
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
pub fn install(root: &Path, is_git: bool, config: &SkillSection) -> Result<Installed> {
    let mut lines = vec![];
    let mut warnings: Vec<String> = vec![];
    if config.path.is_empty() {
        return Ok(Installed {
            lines: vec!["disabled".into()],
            warnings,
            data: json!({"path": null, "state": "disabled", "links": []}),
        });
    }
    let dir = root.join(&config.path);
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
    let mut links = Vec::with_capacity(config.links.len());
    for rel in &config.links {
        let link = root.join(rel);
        if link.is_symlink() || !link.exists() {
            std::fs::create_dir_all(link.parent().unwrap())?;
            if link.is_symlink() {
                std::fs::remove_file(&link)?;
            }
            std::os::unix::fs::symlink(&dir, &link)?;
            lines.push(format!("linked {}", link.display()));
        } else {
            warnings.push(format!(
                "{} exists and is not a symlink; left alone",
                link.display()
            ));
        }
        links.push(json!({"path": link, "linked": link.is_symlink()}));
    }
    if is_git {
        for rel in paths(config) {
            // Paths outside the project cannot be tracked or excluded here.
            let full = root.join(rel);
            let Ok(relative) = full.strip_prefix(root) else {
                continue;
            };
            git_exclude(root, &format!("/{}", relative.display()));
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
        "links": links,
    });
    Ok(Installed {
        lines,
        warnings,
        data,
    })
}
