//! Runtime type routing: an issue's type selects the agent that works on it.
//! Types, agents and prompts all come from the project's `.trk/` directory.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
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
    pub config: ConfigDir,
}

#[derive(Debug, Clone, Serialize)]
pub struct Routing {
    #[serde(rename = "type")]
    pub issue_type: String,
    pub description: String,
    pub agent: String,
    pub override_agent: bool,
    pub type_source: PathBuf,
    pub agent_source: PathBuf,
    pub prompt_directory: PathBuf,
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

impl RoutingConfig {
    pub fn new(config: &ConfigDir) -> Self {
        Self {
            config: config.clone(),
        }
    }

    pub fn resolve(&self, issue_type: &str, override_agent: Option<&str>) -> Result<Routing> {
        valid_name(issue_type)?;
        let path = self
            .config
            .path
            .join("types")
            .join(format!("{issue_type}.toml"));
        if !path.try_exists()? {
            return Err(
                Error::new(Code::NotFound, format!("unknown type '{issue_type}'"))
                    .with_hint(format!("expected {}", path.display())),
            );
        }
        let def: TypeDef = toml::from_str(&std::fs::read_to_string(&path)?)
            .map_err(|e| Error::new(Code::ConfigInvalid, format!("{}: {e}", path.display())))?;
        if def.name.as_deref().is_some_and(|name| name != issue_type) {
            return Err(Error::new(
                Code::ConfigInvalid,
                format!("{}: type name must be '{issue_type}'", path.display()),
            ));
        }
        let description = def
            .description
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                Error::new(
                    Code::ConfigInvalid,
                    format!("type '{issue_type}' has no description"),
                )
            })?;
        let mapping = def.agent.filter(|s| !s.trim().is_empty()).ok_or_else(|| {
            Error::new(
                Code::ConfigInvalid,
                format!("type '{issue_type}' has no mapped agent"),
            )
        })?;
        let agent = override_agent.unwrap_or(&mapping).to_string();
        valid_name(&agent)?;
        let agent_source = self
            .config
            .path
            .join("agents")
            .join(format!("{agent}.toml"));
        if !agent_source.is_file() {
            return Err(Error::new(
                Code::NotFound,
                format!("type '{issue_type}' selects missing agent '{agent}'"),
            )
            .with_hint(format!("expected {}", agent_source.display())));
        }
        Ok(Routing {
            issue_type: issue_type.to_string(),
            description,
            agent,
            override_agent: override_agent.is_some(),
            type_source: path,
            agent_source,
            prompt_directory: self.config.path.clone(),
        })
    }

    pub fn load(&self, routing: &Routing) -> Result<ResolvedAgent> {
        let agent = load_agent_file(&routing.agent_source)?;
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
        Ok(ResolvedAgent {
            agent,
            config: self.config.clone(),
        })
    }

    pub fn agents(&self) -> Result<BTreeMap<String, ResolvedAgent>> {
        self.config
            .files_in("agents", "toml")
            .into_iter()
            .map(|path| {
                let name = path.file_stem().unwrap().to_string_lossy().into_owned();
                valid_name(&name)?;
                let agent = load_agent_file(&path)?;
                if agent.name != name {
                    return Err(Error::new(
                        Code::ConfigInvalid,
                        format!("{}: agent name must be '{name}'", path.display()),
                    ));
                }
                Ok((
                    name,
                    ResolvedAgent {
                        agent,
                        config: self.config.clone(),
                    },
                ))
            })
            .collect()
    }

    pub fn types(&self) -> Result<BTreeMap<String, Routing>> {
        self.config
            .files_in("types", "toml")
            .into_iter()
            .map(|p| {
                let n = p.file_stem().unwrap().to_string_lossy().into_owned();
                self.resolve(&n, None).map(|r| (n, r))
            })
            .collect()
    }

    pub fn check(&self, user: &UserConfig) -> Vec<Diagnostic> {
        crate::check::check_agents(&self.config, user)
    }
}
