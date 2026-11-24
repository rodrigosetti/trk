//! Agent definitions: `agents/*.toml`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use trk_core::config::{ConfigDir, UserConfig};
use trk_core::error::{Code, Error, Result};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Template path (relative to the config dir) → MCP server instructions.
    #[serde(default)]
    pub instructions: Option<String>,
    /// Template path → initial prompt.
    #[serde(default)]
    pub briefing: Option<String>,
    /// Attach queued `trk note --to-agent` messages to tool results.
    #[serde(default = "yes")]
    pub inbox: bool,
    #[serde(default)]
    pub trk_tools: TrkTools,
    #[serde(default)]
    pub mcp: Vec<McpServer>,
    #[serde(default)]
    pub harness: AgentHarness,
}

fn yes() -> bool {
    true
}

/// Which of the fixed trk tools the agent gets. Empty means all of them.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TrkTools {
    /// Tool names, with `*` globs, or `@all`.
    #[serde(default)]
    pub include: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpServer {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentHarness {
    /// Omitted inherits `[harness].prefer`; an explicit empty list disables it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer: Option<Vec<String>>,
    /// Profile-defined level; `default` adds no tool restriction arguments.
    #[serde(default = "default_level")]
    pub builtin_tools: String,
    /// One model for every harness, or a model per harness.
    #[serde(default)]
    pub model: Option<AgentModel>,
    /// Extra arguments keyed by harness name.
    #[serde(default)]
    pub args: BTreeMap<String, Vec<String>>,
}

impl Default for AgentHarness {
    fn default() -> Self {
        AgentHarness {
            prefer: None,
            builtin_tools: default_level(),
            model: None,
            args: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum AgentModel {
    Shared(String),
    PerHarness(BTreeMap<String, String>),
}

impl AgentHarness {
    pub fn model_for(&self, harness: &str) -> Option<&str> {
        match self.model.as_ref()? {
            AgentModel::Shared(model) => Some(model),
            AgentModel::PerHarness(models) => models.get(harness).map(String::as_str),
        }
    }
}

fn default_level() -> String {
    "default".into()
}

pub fn load_agent_file(path: &Path) -> Result<AgentDef> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        Error::new(
            Code::NotFound,
            format!("cannot read {}: {e}", path.display()),
        )
    })?;
    toml::from_str(&text)
        .map_err(|e| Error::new(Code::ConfigInvalid, format!("{}: {e}", path.display())))
}

pub(crate) fn agent_config(cfg: &ConfigDir) -> Result<UserConfig> {
    if cfg.exists() {
        cfg.user_config()
    } else {
        Ok(UserConfig::default())
    }
}

pub(crate) fn apply_preferences(agent: &mut AgentDef, user: &UserConfig) -> Result<()> {
    if agent.harness.prefer.is_none() {
        agent.harness.prefer = Some(crate::harness::default_preferences(user)?);
    }
    Ok(())
}

/// All agents in the config dir, keyed by name.
pub fn load_agents(cfg: &ConfigDir) -> Result<BTreeMap<String, AgentDef>> {
    let user = agent_config(cfg)?;
    let mut out = BTreeMap::new();
    for p in cfg.files_in("agents", "toml") {
        let mut a = load_agent_file(&p)?;
        apply_preferences(&mut a, &user)?;
        out.insert(a.name.clone(), a);
    }
    Ok(out)
}

pub fn load_agent(cfg: &ConfigDir, name: &str) -> Result<AgentDef> {
    // The agent's own file first, so a broken neighbour cannot stop a run.
    let own = cfg.resolve(Path::new("agents").join(format!("{name}.toml")));
    if own.is_file() {
        let mut a = load_agent_file(&own)?;
        if a.name == name {
            apply_preferences(&mut a, &agent_config(cfg)?)?;
            return Ok(a);
        }
    }
    let agents = load_agents(cfg)?;
    agents.get(name).cloned().ok_or_else(|| {
        Error::new(Code::NotFound, format!("unknown agent '{name}'")).with_hint(format!(
            "agents: {}",
            agents.keys().cloned().collect::<Vec<_>>().join(", ")
        ))
    })
}
