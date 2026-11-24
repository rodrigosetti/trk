//! Domain types for rows in the data tables.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Code, Error, Result};

/// RFC 3339 UTC with second precision: `2026-11-18T18:02:11Z`.
pub fn fmt_ts(ts: jiff::Timestamp) -> String {
    ts.round(jiff::Unit::Second)
        .unwrap_or(ts)
        .strftime("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

pub fn parse_ts(s: &str) -> Option<jiff::Timestamp> {
    s.parse().ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActorId {
    Human { email: String },
    Agent { harness: String, session: String },
}

impl ActorId {
    /// `human:<email>` or `agent:<harness>:<session-id>`. The kind ends at the
    /// first colon; for agents the harness ends at the second; the rest is the
    /// session id, kept verbatim.
    pub fn parse(s: &str) -> Result<ActorId> {
        let bad = |why: &str| {
            Error::new(Code::BadArgs, format!("invalid actor '{s}': {why}"))
                .with_hint("actors are human:<email> or agent:<harness>:<session-id>")
        };
        let (kind, rest) = s.split_once(':').ok_or_else(|| bad("missing kind"))?;
        match kind {
            "human" => {
                if rest.is_empty() {
                    return Err(bad("empty email"));
                }
                Ok(ActorId::Human {
                    email: rest.to_string(),
                })
            }
            "agent" => {
                let (harness, session) = rest
                    .split_once(':')
                    .ok_or_else(|| bad("missing session id"))?;
                if harness.is_empty()
                    || !harness.starts_with(|c: char| c.is_ascii_lowercase())
                    || !harness
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                {
                    return Err(bad("harness must match [a-z][a-z0-9-]*"));
                }
                if session.is_empty() {
                    return Err(bad("empty session id"));
                }
                Ok(ActorId::Agent {
                    harness: harness.to_string(),
                    session: session.to_string(),
                })
            }
            _ => Err(bad("kind must be 'human' or 'agent'")),
        }
    }

    pub fn is_agent(&self) -> bool {
        matches!(self, ActorId::Agent { .. })
    }

    pub fn harness(&self) -> Option<&str> {
        match self {
            ActorId::Agent { harness, .. } => Some(harness),
            _ => None,
        }
    }
}

impl std::fmt::Display for ActorId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActorId::Human { email } => write!(f, "human:{email}"),
            ActorId::Agent { harness, session } => write!(f, "agent:{harness}:{session}"),
        }
    }
}

/// The four states of an issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Open,
    Claimed,
    Waiting,
    Closed,
}

impl State {
    pub const ALL: [State; 4] = [State::Open, State::Claimed, State::Waiting, State::Closed];

    pub fn as_str(self) -> &'static str {
        match self {
            State::Open => "open",
            State::Claimed => "claimed",
            State::Waiting => "waiting",
            State::Closed => "closed",
        }
    }

    pub fn parse(s: &str) -> Option<State> {
        State::ALL.into_iter().find(|x| x.as_str() == s)
    }
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub const PRIORITIES: [&str; 4] = ["P0", "P1", "P2", "P3"];
pub const DEFAULT_PRIORITY: &str = "P2";
pub const CLOSE_REASONS: [&str; 2] = ["done", "abandoned"];

/// Exit code of a waiting check that means "not yet, check again later"
/// (`EX_TEMPFAIL`). 0 closes the issue; anything else returns it to open.
pub const CHECK_RETRY_EXIT: i32 = 75;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Issue {
    pub id: String,
    pub state: State,
    pub title: String,
    pub description: String,
    pub priority: String,
    #[serde(rename = "type")]
    pub issue_type: String,
    pub close_reason: Option<String>,
    pub waiting_reason: Option<String>,
    pub waiting_check: Option<String>,
    /// Seconds between checks.
    pub check_interval: Option<i64>,
    pub next_check_at: Option<String>,
    pub last_check_at: Option<String>,
    pub last_check_exit: Option<i64>,
    pub wait_cwd: Option<String>,
    /// Failed-check wakes since the last fresh claim.
    pub wake_count: u32,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
}

impl Issue {
    pub fn is_closed(&self) -> bool {
        self.state == State::Closed
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Claim {
    pub id: i64,
    pub issue_id: String,
    pub actor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
    pub claimed_at: String,
    pub lease_until: Option<String>,
    pub worktree: Option<String>,
    pub host: Option<String>,
    pub agent: Option<String>,
    pub terminal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher: Option<String>,
    pub released_at: Option<String>,
    pub release_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub seq: i64,
    pub issue_id: Option<String>,
    pub actor: String,
    pub at: String,
    pub op: String,
    pub payload: Value,
}

/// The other end of a `blocks` edge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Edge {
    pub id: String,
    pub title: String,
    pub state: State,
    pub priority: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_ids() {
        assert_eq!(
            ActorId::parse("agent:claude-code:12938192381").unwrap(),
            ActorId::Agent {
                harness: "claude-code".into(),
                session: "12938192381".into()
            }
        );
        assert_eq!(
            ActorId::parse("agent:omp:a:b:c").unwrap(),
            ActorId::Agent {
                harness: "omp".into(),
                session: "a:b:c".into()
            }
        );
        assert!(ActorId::parse("human:").is_err());
        assert!(ActorId::parse("robot:x").is_err());
        assert!(ActorId::parse("agent:Claude:1").is_err());
        assert_eq!(
            ActorId::parse("human:r@x.com").unwrap().to_string(),
            "human:r@x.com"
        );
    }
}
