//! Agent definitions: `agents/*.toml`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use trk_core::config::ConfigDir;
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
    #[serde(default)]
    pub prefer: Vec<String>,
    #[serde(default = "default_level")]
    pub builtin_tools: String,
}

impl Default for AgentHarness {
    fn default() -> Self {
        AgentHarness {
            prefer: vec![],
            builtin_tools: default_level(),
        }
    }
}

fn default_level() -> String {
    "default".into()
}

pub const BUILTIN_LEVELS: &[&str] = &["default", "read-only", "none"];

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

/// All agents in the config dir, keyed by name.
pub fn load_agents(cfg: &ConfigDir) -> Result<BTreeMap<String, AgentDef>> {
    let mut out = BTreeMap::new();
    for p in cfg.files_in("agents", "toml") {
        let a = load_agent_file(&p)?;
        out.insert(a.name.clone(), a);
    }
    Ok(out)
}

pub fn load_agent(cfg: &ConfigDir, name: &str) -> Result<AgentDef> {
    // The agent's own file first, so a broken neighbour cannot stop a run.
    let own = cfg.path.join("agents").join(format!("{name}.toml"));
    if own.is_file() {
        let a = load_agent_file(&own)?;
        if a.name == name {
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
