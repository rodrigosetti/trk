//! Domain types for rows in the data tables.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Issue {
    pub id: String,
    pub project: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub title: String,
    pub context: String,
    pub status: String,
    pub status_category: String,
    pub resolution: Option<String>,
    pub outcome: Option<String>,
    pub owner: Option<String>,
    pub links: Vec<Value>,
    pub tags: Vec<String>,
    pub fields: Map<String, Value>,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
}

impl Issue {
    /// The value of a shared or type field, as stored.
    pub fn get(&self, name: &str) -> Option<Value> {
        match name {
            "title" => Some(Value::String(self.title.clone())),
            "context" => Some(Value::String(self.context.clone())),
            "outcome" => self.outcome.clone().map(Value::String),
            "owner" => self.owner.clone().map(Value::String),
            "links" => Some(Value::Array(self.links.clone())),
            "tags" => Some(Value::Array(
                self.tags.iter().cloned().map(Value::String).collect(),
            )),
            "id" => Some(Value::String(self.id.clone())),
            "type" => Some(Value::String(self.ty.clone())),
            "status" => Some(Value::String(self.status.clone())),
            "resolution" => self.resolution.clone().map(Value::String),
            "project" => Some(Value::String(self.project.clone())),
            _ => self.fields.get(name).cloned(),
        }
    }

    pub fn is_closed(&self) -> bool {
        self.status_category == "closed"
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Edge {
    pub kind: String,
    pub id: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub status: String,
    pub status_category: String,
    pub resolution: Option<String>,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Repo {
    pub id: i64,
    pub project: String,
    pub remote_url: Option<String>,
    pub local_path: Option<String>,
}

/// Normalize a git remote URL to `host/owner/name`:
/// `git@github.com:acme/orch.git`, `https://github.com/acme/orch` and
/// `ssh://git@github.com/acme/orch.git` all become `github.com/acme/orch`.
pub fn normalize_remote(url: &str) -> String {
    let mut s = url.trim().to_string();
    let lower = s.to_ascii_lowercase();
    for scheme in [
        "https://",
        "http://",
        "ssh://",
        "git://",
        "git+ssh://",
        "file://",
    ] {
        if lower.starts_with(scheme) {
            s = s[scheme.len()..].to_string();
            break;
        }
    }
    // user@host:path (scp-like) or user@host/path
    if let Some(at) = s.find('@')
        && !s[..at].contains('/')
    {
        s = s[at + 1..].to_string();
    }
    // host:path → host/path (but not host:port/)
    if let Some(colon) = s.find(':') {
        let after = &s[colon + 1..];
        let port_like = after
            .split('/')
            .next()
            .is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
        if port_like {
            let slash = after.find('/').map(|i| colon + 1 + i).unwrap_or(s.len());
            s = format!("{}{}", &s[..colon], &s[slash..]);
        } else {
            s = format!("{}/{}", &s[..colon], after);
        }
    }
    let s = s.trim_end_matches('/');
    let s = s.strip_suffix(".git").unwrap_or(s);
    s.to_ascii_lowercase()
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

    #[test]
    fn remotes() {
        for u in [
            "git@github.com:acme/orch.git",
            "https://github.com/acme/orch",
            "https://github.com/acme/orch.git/",
            "ssh://git@github.com/acme/orch.git",
            "ssh://git@github.com:22/acme/orch.git",
            "HTTPS://GitHub.com/Acme/Orch",
        ] {
            assert_eq!(normalize_remote(u), "github.com/acme/orch", "{u}");
        }
    }
}
