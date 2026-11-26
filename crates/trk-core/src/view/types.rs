//! Serialized read models shared by CLI and JSON presentation boundaries.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{Claim, Edge, Event, Link, State};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimView {
    pub actor: String,
    pub on_behalf_of: Option<String>,
    pub claimed_at: String,
    pub lease_until: Option<String>,
    pub worktree: Option<String>,
    pub host: Option<String>,
    pub agent: Option<String>,
    pub terminal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher: Option<String>,
}

impl From<Claim> for ClaimView {
    fn from(claim: Claim) -> Self {
        Self {
            actor: claim.actor,
            on_behalf_of: claim.on_behalf_of,
            claimed_at: claim.claimed_at,
            lease_until: claim.lease_until,
            worktree: claim.worktree,
            host: claim.host,
            agent: claim.agent,
            terminal: claim.terminal,
            launcher: claim.launcher,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimHistory {
    #[serde(flatten)]
    pub claim: ClaimView,
    pub released_at: Option<String>,
    pub release_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitingView {
    pub reason: Option<String>,
    pub check: Option<String>,
    pub interval: Option<i64>,
    pub next_check_at: Option<String>,
    pub last_check_at: Option<String>,
    pub last_check_exit: Option<i64>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dependencies {
    pub blocked_by: Vec<Edge>,
    pub blocks: Vec<Edge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Unblocks {
    pub id: String,
    pub priority: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Progress {
    pub total: usize,
    pub closed: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Derived {
    pub ready: bool,
    pub blocked_by: Vec<String>,
    pub due: bool,
    pub unblocks_higher_priority: Vec<Unblocks>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueView {
    pub id: String,
    pub title: String,
    pub description: String,
    pub state: State,
    pub priority: String,
    #[serde(rename = "type")]
    pub issue_type: String,
    pub epic: Option<String>,
    pub close_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_count: Option<u32>,
    pub waiting: Option<WaitingView>,
    pub claim: Option<ClaimView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deps: Option<Dependencies>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    pub derived: Derived,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<Event>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claims: Option<Vec<ClaimHistory>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunView {
    #[serde(flatten)]
    pub claim: ClaimView,
    pub issue: String,
    pub state: State,
    pub title: String,
    pub harness: Option<String>,
    pub meta: Value,
}

/// Actor metadata is extensible JSON. Decode only the MCP liveness fields,
/// preserving the existing treatment of missing or wrongly typed values.
#[derive(Debug, Default)]
pub struct McpStatus<'a> {
    pub exited_at: Option<&'a str>,
    pub pid: Option<i64>,
    pub host: Option<&'a str>,
}

impl<'a> From<&'a Value> for McpStatus<'a> {
    fn from(meta: &'a Value) -> Self {
        Self {
            exited_at: meta.get("mcp_exited_at").and_then(Value::as_str),
            pid: meta.get("mcp_pid").and_then(Value::as_i64),
            host: meta.get("mcp_host").and_then(Value::as_str),
        }
    }
}
