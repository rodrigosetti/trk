//! Runtime type routing. Repository configuration is data until explicitly trusted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use trk_core::config::{ConfigDir, Diagnostic, UserConfig};
use trk_core::error::{Code, Error, Result};

use crate::agent::{AgentDef, load_agent_file};

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct TypeDef {
    name: Option<String>,
    description: Option<String>,
    agent: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RoutingConfig {
    pub global: ConfigDir,
    pub repo: Option<ConfigDir>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Routing {
    #[serde(rename = "type")]
    pub issue_type: String,
    pub description: String,
    pub agent: String,
    pub override_agent: bool,
    pub type_sources: Vec<PathBuf>,
    pub description_source: PathBuf,
    pub mapping_source: PathBuf,
    pub agent_source: PathBuf,
    pub prompt_directory: PathBuf,
    pub repo_config: Option<PathBuf>,
    pub requires_trust: bool,
}

pub struct ResolvedAgent {
    pub agent: AgentDef,
    pub config: ConfigDir,
}

fn valid_name(name: &str) -> Result<()> {
    if name.trim().is_empty()
        || name == "."
        || name == ".."
        || name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        return Err(Error::new(
            Code::ConfigInvalid,
            format!("invalid configuration name '{name}': expected one filename component"),
        ));
    }
    Ok(())
}

/// Policy is anchored to the registered primary checkout, never the caller's
/// cwd or an issue worktree. All linked worktrees therefore share one policy.
fn primary_checkout(path: &Path) -> Result<PathBuf> {
    // The first entry is the primary checkout, including with separate git
    // directories. -z avoids quoting paths containing spaces or newlines.
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["worktree", "list", "--porcelain", "-z"])
        .output()?;
    if output.status.success()
        && let Some(first) = output.stdout.split(|b| *b == 0).next()
        && let Some(root) = first.strip_prefix(b"worktree ")
    {
        let root = PathBuf::from(String::from_utf8_lossy(root).into_owned());
        return Ok(std::fs::canonicalize(&root).unwrap_or(root));
    }
    Err(Error::new(
        Code::UnknownRepo,
        format!("cannot locate primary checkout for {}", path.display()),
    ))
}

impl RoutingConfig {
    pub fn for_repository(global: &ConfigDir, path: Option<&Path>) -> Result<Self> {
        let repo = path
            .map(primary_checkout)
            .transpose()?
            .map(|root| ConfigDir::new(root.join(".trk")));
        Ok(Self {
            global: global.clone(),
            repo,
        })
    }

    pub fn for_issue(global: &ConfigDir, issue: &Value) -> Result<Self> {
        let repo = match issue["repo"]["local_path"].as_str() {
            Some(path) => Some(ConfigDir::new(
                primary_checkout(Path::new(path))?.join(".trk"),
            )),
            None if !issue["repo"].is_null() => {
                return Err(Error::new(Code::UnknownRepo, "the issue's repository has no registered local path")
                    .with_hint("register a local checkout with `trk repo add <path>` before resolving its policy"));
            }
            None => None,
        };
        Ok(Self {
            global: global.clone(),
            repo,
        })
    }

    pub fn resolve(&self, issue_type: &str, override_agent: Option<&str>) -> Result<Routing> {
        valid_name(issue_type)?;
        let mut description = None;
        let mut mapping = None;
        let mut description_source = None;
        let mut mapping_source = None;
        let mut sources = Vec::new();
        for cfg in std::iter::once(&self.global).chain(self.repo.iter()) {
            let path = cfg.path.join("types").join(format!("{issue_type}.toml"));
            if !path.try_exists()? {
                continue;
            }
            let def: TypeDef = toml::from_str(&std::fs::read_to_string(&path)?)
                .map_err(|e| Error::new(Code::ConfigInvalid, format!("{}: {e}", path.display())))?;
            if def.name.as_deref().is_some_and(|name| name != issue_type) {
                return Err(Error::new(
                    Code::ConfigInvalid,
                    format!("{}: type name must be '{issue_type}'", path.display()),
                ));
            }
            if let Some(value) = def.description {
                description = Some(value);
                description_source = Some(path.clone());
            }
            if let Some(value) = def.agent {
                mapping = Some(value);
                mapping_source = Some(path.clone());
            }
            sources.push(path);
        }
        if sources.is_empty() {
            return Err(Error::new(
                Code::NotFound,
                format!("unknown type '{issue_type}'"),
            ));
        }
        let description = description
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                Error::new(
                    Code::ConfigInvalid,
                    format!("type '{issue_type}' has no description"),
                )
            })?;
        let mapping = mapping.filter(|s| !s.trim().is_empty()).ok_or_else(|| {
            Error::new(
                Code::ConfigInvalid,
                format!("type '{issue_type}' has no mapped agent"),
            )
        })?;
        let agent = override_agent.unwrap_or(&mapping).to_string();
        valid_name(&agent)?;
        let relative = PathBuf::from("agents").join(format!("{agent}.toml"));
        let local = self.repo.as_ref().map(|c| c.path.join(&relative));
        let is_local = local.as_ref().is_some_and(|p| p.is_file());
        let agent_source = if is_local {
            local.unwrap()
        } else {
            self.global.path.join(&relative)
        };
        if !agent_source.is_file() {
            return Err(Error::new(
                Code::NotFound,
                format!("type '{issue_type}' selects missing agent '{agent}'"),
            )
            .with_hint(format!("expected {}", agent_source.display())));
        }
        let prompt_directory = if is_local {
            self.repo.as_ref().unwrap().path.clone()
        } else {
            self.global.path.clone()
        };
        Ok(Routing {
            issue_type: issue_type.to_string(),
            description,
            agent,
            override_agent: override_agent.is_some(),
            type_sources: sources,
            description_source: description_source.unwrap(),
            mapping_source: mapping_source.unwrap(),
            agent_source,
            prompt_directory,
            repo_config: self.repo.as_ref().map(|c| c.path.clone()),
            requires_trust: is_local,
        })
    }

    pub fn require_trust(&self, routing: &Routing, trusted: bool) -> Result<()> {
        if routing.requires_trust && !trusted {
            return Err(Error::new(Code::NotPermitted, format!("repository agent '{}' is not trusted", routing.agent))
                .with_hint("review the repository's .trk agents, prompts and MCP commands, then pass --trust-repo for this invocation")
                .with_detail("routing", serde_json::to_value(routing)?));
        }
        Ok(())
    }

    /// Loading is not execution. Call require_trust before rendering local
    /// templates, launching a harness, or serving the selected MCP tools.
    pub fn load(&self, routing: &Routing) -> Result<ResolvedAgent> {
        let mut agent = load_agent_file(&routing.agent_source)?;
        if agent.name != routing.agent {
            return Err(Error::new(
                Code::ConfigInvalid,
                format!(
                    "{}: agent name must be '{}'",
                    routing.agent_source.display(),
                    routing.agent
                ),
            ));
        }
        // Repository definitions replace all executable policy, but cannot
        // choose installed harness preferences. Those stay in global config.
        let user = self.global.user_config()?;
        if !user.run.harness_prefer.is_empty() {
            agent.harness.prefer = user.run.harness_prefer;
        } else if routing.requires_trust {
            let global = self
                .global
                .path
                .join("agents")
                .join(format!("{}.toml", routing.agent));
            agent.harness.prefer = if global.is_file() {
                load_agent_file(&global)?.harness.prefer
            } else {
                Vec::new()
            };
        }
        Ok(ResolvedAgent {
            agent,
            config: ConfigDir::new(&routing.prompt_directory),
        })
    }
    /// Effective agent definitions without rendering repository templates.
    pub fn agents(&self) -> Result<BTreeMap<String, ResolvedAgent>> {
        let mut sources = BTreeMap::new();
        for cfg in std::iter::once(&self.global).chain(self.repo.iter()) {
            for path in cfg.files_in("agents", "toml") {
                let name = path.file_stem().unwrap().to_string_lossy().into_owned();
                valid_name(&name)?;
                sources.insert(name, (cfg, path));
            }
        }
        let user = self.global.user_config()?;
        sources
            .into_iter()
            .map(|(name, (cfg, path))| {
                let mut agent = load_agent_file(&path)?;
                if agent.name != name {
                    return Err(Error::new(
                        Code::ConfigInvalid,
                        format!("{}: agent name must be '{name}'", path.display()),
                    ));
                }
                if !user.run.harness_prefer.is_empty() {
                    agent.harness.prefer.clone_from(&user.run.harness_prefer);
                } else if cfg.path != self.global.path {
                    let global = self.global.path.join("agents").join(format!("{name}.toml"));
                    agent.harness.prefer = if global.is_file() {
                        load_agent_file(&global)?.harness.prefer
                    } else {
                        Vec::new()
                    };
                }
                Ok((
                    name,
                    ResolvedAgent {
                        agent,
                        config: cfg.clone(),
                    },
                ))
            })
            .collect()
    }

    pub fn types(&self) -> Result<BTreeMap<String, Routing>> {
        let mut names = BTreeMap::new();
        for cfg in std::iter::once(&self.global).chain(self.repo.iter()) {
            for p in cfg.files_in("types", "toml") {
                let name = p.file_stem().unwrap().to_string_lossy().into_owned();
                names.insert(name, ());
            }
        }
        names
            .into_keys()
            .map(|n| self.resolve(&n, None).map(|r| (n, r)))
            .collect()
    }

    pub fn check(&self, user: &UserConfig) -> Vec<Diagnostic> {
        let mut diagnostics = crate::check::check_global(&self.global, user, self.repo.as_ref());
        match self.agents() {
            Err(e) => diagnostics.push(Diagnostic::error(None, e.message())),
            Ok(agents) => {
                for resolved in agents.values() {
                    let a = &resolved.agent;
                    for p in [&a.instructions, &a.briefing].into_iter().flatten() {
                        if !resolved.config.path.join(p).is_file() {
                            diagnostics.push(Diagnostic::error(
                                None,
                                format!(
                                    "agent '{}': template '{}' missing in {}",
                                    a.name,
                                    p,
                                    resolved.config.path.display()
                                ),
                            ));
                        }
                    }
                    if !crate::agent::BUILTIN_LEVELS.contains(&a.harness.builtin_tools.as_str()) {
                        diagnostics.push(Diagnostic::error(
                            None,
                            format!(
                                "agent '{}': invalid builtin_tools '{}'",
                                a.name, a.harness.builtin_tools
                            ),
                        ));
                    }
                    for name in crate::tools::unmatched_includes(a) {
                        diagnostics.push(Diagnostic::error(
                            None,
                            format!("agent '{}': unknown tool '{name}'", a.name),
                        ));
                    }
                }
            }
        }
        diagnostics
    }
}
