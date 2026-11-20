//! `trk skill install|uninstall|status`: the agent skill that teaches
//! interactive agent sessions to use the trk CLI. Its files are embedded in
//! the binary, so an installed skill always matches the installed `trk`.
//!
//! Scopes:
//! - project (default): `<repo>/.agents/skills/trk` (omp, Codex), linked from
//!   `<repo>/.claude/skills/trk` (Claude Code). Both paths are added to
//!   `.git/info/exclude`, so the skill is never tracked and the worktrees
//!   `trk run` creates do not contain it: sessions reach trk through MCP only.
//! - `--global`: `~/.agents/skills/trk`, linked from `~/.claude/skills/trk`.
//!   Every session sees it, `trk run` ones included (the omp profile hides it
//!   with a per-session overlay).
//! - `--dir PATH`: `PATH/trk`, for any other skills directory.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use trk_core::error::{Code, Error, Result};

use crate::env::{Env, git, git_exclude, git_unexclude, home};
use crate::render::Out;

pub const NAME: &str = "trk";

/// The skill's files, relative to its directory.
pub const FILES: &[(&str, &str)] = &[
    ("SKILL.md", include_str!("../../../skills/trk/SKILL.md")),
    (
        "references/reference.md",
        include_str!("../../../skills/trk/references/reference.md"),
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Project,
    Global,
    Dir,
}

impl Scope {
    fn as_str(self) -> &'static str {
        match self {
            Scope::Project => "project",
            Scope::Global => "global",
            Scope::Dir => "dir",
        }
    }
}

/// Where one scope's skill lives.
struct Target {
    scope: Scope,
    /// `<skills dir>/trk`
    dir: PathBuf,
    /// Claude Code's `skills/trk` symlink and what it points to.
    claude: Option<(PathBuf, PathBuf)>,
    /// Repository root, for project installs (git excludes).
    repo: Option<PathBuf>,
}

const PROJECT_SKILL: &str = ".agents/skills/trk";
const PROJECT_CLAUDE: &str = ".claude/skills/trk";

fn target(env: &Env, scope: Scope, dir: Option<&Path>) -> Result<Target> {
    match scope {
        Scope::Dir => {
            let d = dir.expect("--dir given");
            Ok(Target {
                scope,
                dir: d.join(NAME),
                claude: None,
                repo: None,
            })
        }
        Scope::Global => {
            let h = home();
            let dir = h.join(".agents/skills").join(NAME);
            let claude = h
                .join(".claude")
                .is_dir()
                .then(|| (h.join(".claude/skills").join(NAME), dir.clone()));
            Ok(Target {
                scope,
                dir,
                claude,
                repo: None,
            })
        }
        Scope::Project => {
            let root = env
                .git
                .as_ref()
                .map(|g| PathBuf::from(&g.toplevel))
                .ok_or_else(|| {
                    Error::new(Code::BadArgs, "not inside a git repository")
                        .with_hint("run it in a repository, or pass --global or --dir PATH")
                })?;
            // Link for Claude Code when it is installed or the repo already uses it.
            let claude =
                (home().join(".claude").is_dir() || root.join(".claude").is_dir()).then(|| {
                    (
                        root.join(PROJECT_CLAUDE),
                        PathBuf::from("../../.agents/skills").join(NAME),
                    )
                });
            Ok(Target {
                scope,
                dir: root.join(PROJECT_SKILL),
                claude,
                repo: Some(root),
            })
        }
    }
}

/// "missing", "current", "outdated" or "linked" (a symlink, e.g. a development copy).
fn state(dir: &Path) -> &'static str {
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

fn describe(t: &Target) -> Value {
    json!({
        "scope": t.scope.as_str(),
        "path": t.dir,
        "state": state(&t.dir),
        "claude_link": t.claude.as_ref().map(|(l, _)| l),
        "claude_linked": t.claude.as_ref().map(|(l, _)| l.is_symlink()),
    })
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

/// Is anything under `rel` tracked in the repository?
fn tracked(repo: &Path, rel: &str) -> bool {
    git(repo, &["ls-files", "--", rel]).is_some_and(|s| !s.is_empty())
}

pub fn install(env: &Env, scope: Scope, dir: Option<&Path>) -> Result<Out> {
    let t = target(env, scope, dir)?;
    let mut warnings: Vec<String> = vec![];
    let mut lines = vec![];
    if t.dir.is_symlink() {
        lines.push(format!(
            "{} is a symlink (a development copy?); left alone",
            t.dir.display()
        ));
    } else {
        let before = state(&t.dir);
        std::fs::create_dir_all(t.dir.parent().unwrap())?;
        write_files(&t.dir)?;
        lines.push(match before {
            "current" => format!("skill already current at {}", t.dir.display()),
            "missing" => format!("installed the trk skill at {}", t.dir.display()),
            _ => format!("updated the trk skill at {}", t.dir.display()),
        });
    }
    if let Some((link, to)) = &t.claude {
        if link.is_symlink() || !link.exists() {
            std::fs::create_dir_all(link.parent().unwrap())?;
            if link.is_symlink() {
                std::fs::remove_file(link)?;
            }
            std::os::unix::fs::symlink(to, link)?;
            lines.push(format!("linked {} for Claude Code", link.display()));
        } else {
            warnings.push(format!(
                "{} exists and is not a symlink; left alone",
                link.display()
            ));
        }
    }
    if let Some(repo) = &t.repo {
        git_exclude(repo, &format!("/{PROJECT_SKILL}/"));
        if t.claude.is_some() {
            git_exclude(repo, &format!("/{PROJECT_CLAUDE}"));
        }
        lines.push(
            "kept out of git (.git/info/exclude), so `trk run` worktrees do not contain it".into(),
        );
        for rel in [PROJECT_SKILL, PROJECT_CLAUDE] {
            if tracked(repo, rel) {
                warnings.push(format!(
                    "{rel} is tracked in git: worktrees created by `trk run` will contain it, so agent sessions will see the skill"
                ));
            }
        }
    }
    if t.scope == Scope::Global {
        warnings.push(
            "a global skill is visible to `trk run` sessions too; the omp profile hides it, other harnesses rely on the skill's own instruction"
                .into(),
        );
    }
    let mut text = lines.join("\n");
    for w in &warnings {
        text.push_str(&format!("\nwarning: {w}"));
    }
    let mut data = describe(&t);
    data["warnings"] = json!(warnings);
    Ok(Out::Text(text, data))
}

pub fn uninstall(env: &Env, scope: Scope, dir: Option<&Path>) -> Result<Out> {
    let t = target(env, scope, dir)?;
    let mut lines = vec![];
    if t.dir.is_symlink() {
        std::fs::remove_file(&t.dir)?;
        lines.push(format!("removed the link {}", t.dir.display()));
    } else if t.dir.exists() {
        std::fs::remove_dir_all(&t.dir)?;
        lines.push(format!("removed {}", t.dir.display()));
    } else {
        lines.push(format!("no skill at {}", t.dir.display()));
    }
    if let Some((link, to)) = &t.claude
        && link.is_symlink()
        && std::fs::read_link(link).is_ok_and(|p| p == *to)
    {
        std::fs::remove_file(link)?;
        lines.push(format!("removed the link {}", link.display()));
    }
    if let Some(repo) = &t.repo {
        git_unexclude(repo, &format!("/{PROJECT_SKILL}/"));
        git_unexclude(repo, &format!("/{PROJECT_CLAUDE}"));
    }
    Ok(Out::Text(lines.join("\n"), describe(&t)))
}

pub fn status(env: &Env, global: bool, dir: Option<&Path>) -> Result<Out> {
    let mut targets = vec![];
    if let Some(d) = dir {
        targets.push(target(env, Scope::Dir, Some(d))?);
    } else {
        if !global && let Ok(t) = target(env, Scope::Project, None) {
            targets.push(t);
        }
        targets.push(target(env, Scope::Global, None)?);
    }
    let items: Vec<Value> = targets.iter().map(describe).collect();
    let text = items
        .iter()
        .map(|i| {
            let hint = match i["state"].as_str() {
                Some("outdated") => format!(
                    "  (refresh: trk skill install{})",
                    if i["scope"] == "global" {
                        " --global"
                    } else {
                        ""
                    }
                ),
                _ => String::new(),
            };
            format!(
                "{:<8} {:<9} {}{hint}",
                i["scope"].as_str().unwrap_or(""),
                i["state"].as_str().unwrap_or(""),
                i["path"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Out::Text(text, Value::Array(items)))
}
