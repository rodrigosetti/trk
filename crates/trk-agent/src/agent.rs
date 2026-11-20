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

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TrkTools {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub overrides: BTreeMap<String, Override>,
    #[serde(default)]
    pub composed: BTreeMap<String, Composed>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Override {
    #[serde(default)]
    pub description: Option<String>,
}

/// A named combination of core operations, run in this order in one
/// transaction: set fields, create a linked issue, move, close, add a note.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Composed {
    pub description: String,
    /// Fields of this issue to set; each becomes a parameter.
    #[serde(default)]
    pub set: Vec<String>,
    #[serde(default)]
    pub create: Option<CreateSpec>,
    #[serde(default, rename = "move")]
    pub move_to: Option<String>,
    #[serde(default)]
    pub close: Option<String>,
    /// Add a `note` parameter whose text is appended as a note.
    #[serde(default = "yes")]
    pub note: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSpec {
    #[serde(rename = "type")]
    pub ty: String,
    /// Edge between the new issue and this one.
    pub edge: String,
    /// Fields of the new issue that become parameters.
    #[serde(default)]
    pub fields: Vec<String>,
    /// "new_to_this" (default): new <edge> this. "this_to_new": this <edge> new.
    #[serde(default)]
    pub direction: Option<String>,
}

impl CreateSpec {
    pub fn new_is_src(&self) -> bool {
        self.direction.as_deref() != Some("this_to_new")
    }
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
        .map_err(|e| Error::new(Code::SchemaInvalid, format!("{}: {e}", path.display())))
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
    let agents = load_agents(cfg)?;
    agents.get(name).cloned().ok_or_else(|| {
        Error::new(Code::NotFound, format!("unknown agent '{name}'")).with_hint(format!(
            "agents: {}",
            agents.keys().cloned().collect::<Vec<_>>().join(", ")
        ))
    })
}
