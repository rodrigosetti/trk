//! The dashboard snapshot: everything a live overview shows, as one
//! serializable value. It carries data only (timestamps, enums, reasons), so
//! a terminal UI, `trk dashboard --json` and any later front end format it
//! themselves. Reading it never writes: an expired lease that no command has
//! swept yet is reported as such instead of being released.

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;

use crate::error::Result;
use crate::model::*;
use crate::store::{CLAIM_COLS, Ctx, ISSUE_COLS, SESSION_CLAIM_FILTER, row_to_claim, row_to_issue};

/// Where an issue sits in the overview, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    /// Something only a person can move forward.
    Attention,
    /// Claimed: a session (or a person) is working on it.
    Running,
    /// Waiting on a check.
    Waiting,
    /// Open with every blocker closed.
    Ready,
    /// Open with an open blocker.
    Blocked,
}

impl Group {
    pub const ALL: [Group; 5] = [
        Group::Attention,
        Group::Running,
        Group::Waiting,
        Group::Ready,
        Group::Blocked,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Group::Attention => "Needs you",
            Group::Running => "In flight",
            Group::Waiting => "Waiting",
            Group::Ready => "Ready",
            Group::Blocked => "Blocked",
        }
    }
}

/// The session behind an issue: its active claim, or the claim it released
/// to wait.
#[derive(Debug, Clone, Serialize)]
pub struct Session {
    pub actor: String,
    pub agent: Option<String>,
    pub harness: Option<String>,
    pub terminal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launcher: Option<String>,
    pub worktree: Option<String>,
    pub host: Option<String>,
    pub claimed_at: String,
    /// Historical when the session is waiting.
    pub lease_until: Option<String>,
    /// False for a waiting session, which holds no claim.
    pub active: bool,
    /// Whether the session's terminal is alive, when that can be told.
    pub alive: Option<bool>,
    /// Whether the session's MCP server is running, independently of its terminal.
    pub mcp_alive: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Waiting {
    pub reason: Option<String>,
    /// None: a person decides.
    pub check: Option<String>,
    pub interval: Option<i64>,
    pub next_check_at: Option<String>,
    pub last_check_at: Option<String>,
    pub last_check_exit: Option<i64>,
    pub cwd: Option<String>,
    pub due: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashIssue {
    pub id: String,
    pub title: String,
    pub priority: String,
    #[serde(rename = "type")]
    pub issue_type: String,
    pub state: State,
    pub group: Group,
    /// Why the issue needs a person; empty outside `Group::Attention`.
    pub attention: Vec<String>,
    pub blocked_by: Vec<Edge>,
    pub blocks: Vec<Edge>,
    pub session: Option<Session>,
    pub waiting: Option<Waiting>,
    /// Pointers the issue carries (a pull request, a preview), oldest first.
    pub links: Vec<Link>,
    pub wake_count: u32,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Counts {
    pub attention: usize,
    pub running: usize,
    pub waiting: usize,
    pub ready: usize,
    pub blocked: usize,
    pub closed: usize,
}

/// The process holding the check queue lock.
#[derive(Debug, Clone, Serialize)]
pub struct CheckRunner {
    pub pid: i64,
    pub host: Option<String>,
    pub started_at: String,
    /// A check is running right now.
    pub busy: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Dashboard {
    pub generated_at: String,
    pub max_wakes: u32,
    pub counts: Counts,
    /// Every issue that is not closed: by group, then priority, then age.
    pub issues: Vec<DashIssue>,
    pub check_runner: Option<CheckRunner>,
    /// The latest events of all issues, newest first.
    pub activity: Vec<Event>,
}

/// One issue in full, for a detail view.
#[derive(Debug, Clone, Serialize)]
pub struct Detail {
    pub description: String,
    pub created_by: String,
    pub notes: Vec<Value>,
    pub events: Vec<Event>,
}

/// Probe terminal liveness from its pinned launcher, host and terminal.
pub type TerminalAlive<'a> = dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> Option<bool> + 'a;

impl Ctx<'_> {
    /// The snapshot. The caller probes terminal liveness from the pinned
    /// launcher, host and terminal, and MCP liveness from actor metadata.
    pub fn dashboard(
        &self,
        activity: usize,
        alive: &TerminalAlive<'_>,
        mcp_alive: &dyn Fn(&Value) -> Option<bool>,
    ) -> Result<Dashboard> {
        let now = self.ts();
        let mut st = self.conn.prepare(&format!(
            "SELECT {ISSUE_COLS} FROM issue i WHERE i.state != 'closed'
             ORDER BY i.priority, i.created_at, i.id"
        ))?;
        let issues: Vec<Issue> = st
            .query_map([], row_to_issue)?
            .collect::<std::result::Result<_, _>>()?;
        drop(st);

        // Sessions: the active claim, or the one released to wait.
        let mut st = self.conn.prepare(&format!(
            "SELECT {CLAIM_COLS}, a.harness, a.meta
             FROM claim c JOIN actor a ON a.id = c.actor JOIN issue i ON i.id = c.issue_id
             WHERE {SESSION_CLAIM_FILTER} ORDER BY c.released_at IS NULL"
        ))?;
        let mut sessions: HashMap<String, Session> = HashMap::new();
        let rows = st.query_map([], |r| {
            Ok((
                row_to_claim(r)?,
                r.get::<_, Option<String>>(13)?,
                r.get::<_, Option<String>>(14)?,
            ))
        })?;
        for row in rows {
            let (c, harness, meta) = row?;
            let meta: Value = meta
                .and_then(|m| serde_json::from_str(&m).ok())
                .unwrap_or(Value::Null);
            let is_agent = c.actor.starts_with("agent:");
            sessions.insert(
                c.issue_id.clone(),
                Session {
                    alive: if is_agent {
                        alive(
                            c.launcher.as_deref(),
                            c.host.as_deref(),
                            c.terminal.as_deref(),
                        )
                    } else {
                        None
                    },
                    mcp_alive: if is_agent { mcp_alive(&meta) } else { None },
                    actor: c.actor,
                    agent: c.agent,
                    harness,
                    terminal: c.terminal,
                    launcher: c.launcher,
                    worktree: c.worktree,
                    host: c.host,
                    claimed_at: c.claimed_at,
                    lease_until: c.lease_until,
                    active: c.released_at.is_none(),
                },
            );
        }
        drop(st);

        // The last lifecycle event of each open issue says how it got there.
        let mut st = self.conn.prepare(
            "SELECT e.issue_id, e.op, e.payload FROM event e
             JOIN issue i ON i.id = e.issue_id AND i.state = 'open'
             WHERE e.seq = (SELECT max(l.seq) FROM event l WHERE l.issue_id = e.issue_id
                            AND l.op IN ('create', 'claim', 'release', 'reopen', 'check', 'wait'))",
        )?;
        let returned: HashMap<String, String> = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .filter_map(|row| {
                let (id, op, payload) = row.ok()?;
                let p: Value = serde_json::from_str(&payload).ok()?;
                let why = match (op.as_str(), p["reason"].as_str()) {
                    ("reopen", _) if p["by_check"] == true => "check failed; open again",
                    ("release", Some("expired")) => "lease expired; session lost",
                    ("release", _) if p["reaped"] == true => "session ended; claim released",
                    ("release", _) if p["check_wake_failed"] == true => "check failed; open again",
                    _ => return None,
                };
                Some((id, why.to_string()))
            })
            .collect();
        drop(st);

        let mut st = self.conn.prepare(
            "SELECT l.issue_id, l.url, l.label FROM link l
             JOIN issue i ON i.id = l.issue_id AND i.state != 'closed' ORDER BY l.rowid",
        )?;
        let mut links: HashMap<String, Vec<Link>> = HashMap::new();
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Link {
                    url: r.get(1)?,
                    label: r.get(2)?,
                },
            ))
        })?;
        for row in rows {
            let (id, link) = row?;
            links.entry(id).or_default().push(link);
        }
        drop(st);

        let mut out = Vec::with_capacity(issues.len());
        for i in issues {
            let blocked_by = self.edges(&i.id, false)?;
            let blocks: Vec<Edge> = self
                .edges(&i.id, true)?
                .into_iter()
                .filter(|e| e.state != State::Closed)
                .collect();
            let open_blockers = blocked_by.iter().any(|b| b.state != State::Closed);
            let session = sessions.remove(&i.id);
            let waiting = (i.state == State::Waiting).then(|| Waiting {
                due: i.waiting_check.is_some()
                    && i.next_check_at.as_deref().is_none_or(|t| t <= now.as_str()),
                reason: i.waiting_reason.clone(),
                check: i.waiting_check.clone(),
                interval: i.check_interval,
                next_check_at: i.next_check_at.clone(),
                last_check_at: i.last_check_at.clone(),
                last_check_exit: i.last_check_exit,
                cwd: i.wait_cwd.clone(),
            });

            let mut attention = vec![];
            match i.state {
                State::Open => {
                    if let Some(why) = returned.get(&i.id) {
                        attention.push(why.clone());
                    }
                }
                State::Claimed => {
                    if let Some(s) = &session {
                        if s.alive == Some(false) {
                            attention.push("session ended; reap to release".into());
                        }
                        if s.lease_until.as_deref().is_some_and(|t| t < now.as_str()) {
                            attention.push("lease expired".into());
                        }
                    }
                }
                State::Waiting => {
                    if i.waiting_check.is_none() {
                        attention.push("waiting for your decision".into());
                    } else if self.opts.max_wakes > 0 && i.wake_count >= self.opts.max_wakes {
                        attention.push("wake cap reached; a failed check will not wake it".into());
                    }
                }
                State::Closed => {}
            }
            let group = if !attention.is_empty() {
                Group::Attention
            } else {
                match i.state {
                    State::Claimed => Group::Running,
                    State::Waiting => Group::Waiting,
                    _ if open_blockers => Group::Blocked,
                    _ => Group::Ready,
                }
            };
            let links = links.remove(&i.id).unwrap_or_default();
            out.push(DashIssue {
                id: i.id,
                title: i.title,
                priority: i.priority,
                issue_type: i.issue_type,
                state: i.state,
                group,
                attention,
                blocked_by,
                blocks,
                links,
                session,
                waiting,
                wake_count: i.wake_count,
                created_at: i.created_at,
                updated_at: i.updated_at,
            });
        }
        // Stable: keeps priority, then age, inside each group.
        out.sort_by_key(|i| i.group);

        let mut counts = Counts {
            closed: self.counts()?.get("closed").copied().unwrap_or(0) as usize,
            ..Default::default()
        };
        for i in &out {
            match i.group {
                Group::Attention => counts.attention += 1,
                Group::Running => counts.running += 1,
                Group::Waiting => counts.waiting += 1,
                Group::Ready => counts.ready += 1,
                Group::Blocked => counts.blocked += 1,
            }
        }

        let check_runner = self
            .check_lock()?
            .map(|(pid, host, started_at, pgid)| CheckRunner {
                pid,
                host,
                started_at,
                busy: pgid.is_some(),
            });

        let mut st = self.conn.prepare(
            "SELECT seq, issue_id, actor, at, op, payload FROM event
             WHERE issue_id IS NOT NULL ORDER BY seq DESC LIMIT ?1",
        )?;
        let activity = st
            .query_map([activity as i64], |r| {
                let payload: String = r.get(5)?;
                Ok(Event {
                    seq: r.get(0)?,
                    issue_id: r.get(1)?,
                    actor: r.get(2)?,
                    at: r.get(3)?,
                    op: r.get(4)?,
                    payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
                })
            })?
            .collect::<std::result::Result<_, _>>()?;

        Ok(Dashboard {
            generated_at: now,
            max_wakes: self.opts.max_wakes,
            counts,
            issues: out,
            check_runner,
            activity,
        })
    }

    /// The parts of one issue the snapshot leaves out.
    pub fn dashboard_detail(&self, id: &str) -> Result<Detail> {
        let issue = self.issue(id)?;
        Ok(Detail {
            description: issue.description,
            created_by: issue.created_by,
            notes: self.notes(id)?,
            events: self.events(id)?,
        })
    }
}
