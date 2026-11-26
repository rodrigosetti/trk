//! Read models: the issue object of `show --json`, derived state, list,
//! timeline and active or waiting agent runs. `derived` is computed here, never stored.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::error::Result;
use crate::model::*;
use crate::query;
use crate::store::{CLAIM_COLS, Ctx, ISSUE_COLS, SESSION_CLAIM_FILTER, row_to_claim, row_to_issue};

#[derive(Debug, Clone, Copy, Default)]
pub struct With {
    pub deps: bool,
    pub notes: bool,
    pub events: bool,
    pub claims: bool,
}

impl With {
    pub fn parse(s: &str) -> Self {
        let mut w = With {
            deps: true,
            ..Default::default()
        };
        for p in s.split(',').map(str::trim) {
            match p {
                "notes" => w.notes = true,
                "events" => w.events = true,
                "deps" => w.deps = true,
                "claims" => w.claims = true,
                "all" => {
                    w.notes = true;
                    w.events = true;
                    w.claims = true;
                }
                _ => {}
            }
        }
        w
    }
}

fn rank(priority: &str) -> i64 {
    priority
        .strip_prefix('P')
        .and_then(|r| r.parse().ok())
        .unwrap_or(2)
}

impl Ctx<'_> {
    pub fn claim_json(&self, c: &Claim) -> Value {
        let mut value = json!({
            "actor": c.actor,
            "on_behalf_of": c.on_behalf_of,
            "claimed_at": c.claimed_at,
            "lease_until": c.lease_until,
            "worktree": c.worktree,
            "host": c.host,
            "agent": c.agent,
            "terminal": c.terminal,
        });
        if let Some(launcher) = &c.launcher {
            value["launcher"] = json!(launcher);
        }
        value
    }

    fn edge_json(e: &Edge) -> Value {
        json!({"id": e.id, "title": e.title, "state": e.state, "priority": e.priority, "epic": e.epic})
    }

    /// `ready`, `blocked_by`, `due`, blocker progress, and the open issues of
    /// higher priority this one holds up.
    pub fn derived(&self, issue: &Issue, blockers: &[Edge], blocks: &[Edge]) -> Value {
        let blocked_by: Vec<&str> = blockers
            .iter()
            .filter(|b| b.state != State::Closed)
            .map(|b| b.id.as_str())
            .collect();
        let ready = issue.state == State::Open && blocked_by.is_empty();
        let due = issue.state == State::Waiting
            && issue.waiting_check.is_some()
            && issue
                .next_check_at
                .as_deref()
                .is_none_or(|t| t <= self.ts().as_str());
        let mine = rank(&issue.priority);
        let unblocks: Vec<Value> = blocks
            .iter()
            .filter(|b| b.state != State::Closed && rank(&b.priority) < mine)
            .map(|b| json!({"id": b.id, "priority": b.priority}))
            .collect();
        let mut d = json!({
            "ready": ready,
            "blocked_by": blocked_by,
            "due": due,
            "unblocks_higher_priority": unblocks,
        });
        if !blockers.is_empty() {
            let closed = blockers.iter().filter(|b| b.state == State::Closed).count();
            d["progress"] = json!({"total": blockers.len(), "closed": closed});
        }
        d
    }

    /// The issue object: the shape `show --json` emits.
    pub fn issue_json(&self, issue: &Issue, with: With) -> Result<Value> {
        let claim = self.active_claim(&issue.id)?;
        let mut o = Map::new();
        o.insert("id".into(), json!(issue.id));
        o.insert("title".into(), json!(issue.title));
        o.insert("description".into(), json!(issue.description));
        o.insert("state".into(), json!(issue.state));
        o.insert("priority".into(), json!(issue.priority));
        o.insert("type".into(), json!(issue.issue_type));
        o.insert("epic".into(), json!(issue.epic));
        o.insert("close_reason".into(), json!(issue.close_reason));
        if issue.wake_count > 0 && matches!(issue.state, State::Waiting | State::Claimed) {
            o.insert("wake_count".into(), json!(issue.wake_count));
        }
        let waiting = (issue.state == State::Waiting).then(|| {
            json!({
                "reason": issue.waiting_reason,
                "check": issue.waiting_check,
                "interval": issue.check_interval,
                "next_check_at": issue.next_check_at,
                "last_check_at": issue.last_check_at,
                "last_check_exit": issue.last_check_exit,
                "cwd": issue.wait_cwd,
            })
        });
        o.insert("waiting".into(), json!(waiting));
        o.insert(
            "claim".into(),
            claim
                .as_ref()
                .map(|c| self.claim_json(c))
                .unwrap_or(Value::Null),
        );
        let blocks = self.edges(&issue.id, true)?;
        let blockers = self.edges(&issue.id, false)?;
        if with.deps {
            o.insert(
                "deps".into(),
                json!({
                    "blocked_by": blockers.iter().map(Self::edge_json).collect::<Vec<_>>(),
                    "blocks": blocks.iter().map(Self::edge_json).collect::<Vec<_>>(),
                }),
            );
        }
        let links = self.links(&issue.id)?;
        if !links.is_empty() {
            o.insert("links".into(), json!(links));
        }
        o.insert("derived".into(), self.derived(issue, &blockers, &blocks));
        o.insert("created_by".into(), json!(issue.created_by));
        o.insert("created_at".into(), json!(issue.created_at));
        o.insert("updated_at".into(), json!(issue.updated_at));
        o.insert("closed_at".into(), json!(issue.closed_at));
        if with.notes {
            o.insert("notes".into(), json!(self.notes(&issue.id)?));
        }
        if with.events {
            o.insert("events".into(), json!(self.events(&issue.id)?));
        }
        if with.claims {
            let cl: Vec<Value> = self
                .claims(&issue.id)?
                .iter()
                .map(|c| {
                    let mut v = self.claim_json(c);
                    v["released_at"] = json!(c.released_at);
                    v["release_reason"] = json!(c.release_reason);
                    v
                })
                .collect();
            o.insert("claims".into(), json!(cl));
        }
        Ok(Value::Object(o))
    }

    pub fn show(&self, id: &str, with: With) -> Result<Value> {
        let id = self.resolve_id(id)?;
        let issue = self.issue(&id)?;
        self.issue_json(&issue, with)
    }

    pub fn notes(&self, id: &str) -> Result<Vec<Value>> {
        let mut st = self.conn.prepare(
            "SELECT seq, actor, at, body, amends
             FROM note WHERE issue_id = ?1 ORDER BY seq",
        )?;
        let v = st
            .query_map([id], |r| {
                Ok(json!({
                    "seq": r.get::<_, i64>(0)?, "actor": r.get::<_, String>(1)?, "at": r.get::<_, String>(2)?,
                    "body": r.get::<_, Option<String>>(3)?, "amends": r.get::<_, Option<i64>>(4)?,
                }))
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(v)
    }

    /// `trk list`: issues matching the query.
    pub fn list(&self, q: &str, sort: Option<&str>, limit: Option<usize>) -> Result<Vec<Value>> {
        let terms = query::parse(q)?;
        let compiled = query::compile(&terms, self)?;
        let mut sql = format!(
            "SELECT {ISSUE_COLS} FROM issue_view i WHERE {}",
            compiled.where_sql
        );
        let params = compiled.params;
        sql.push_str(&format!(" ORDER BY {}", query::order_by(sort)?));
        if let Some(n) = limit {
            sql.push_str(&format!(" LIMIT {n}"));
        }
        let mut st = self.conn.prepare(&sql)?;
        let issues: Vec<Issue> = st
            .query_map(rusqlite::params_from_iter(params.iter()), row_to_issue)?
            .collect::<std::result::Result<_, _>>()?;
        drop(st);
        let mut out = vec![];
        for i in &issues {
            out.push(self.issue_json(
                i,
                With {
                    deps: true,
                    ..Default::default()
                },
            )?);
        }
        Ok(out)
    }

    /// `trk log`: the merged timeline of one issue.
    pub fn log(&self, id: &str) -> Result<Vec<Event>> {
        let id = self.resolve_id(id)?;
        self.events(&id)
    }

    /// Active and waiting agent sessions with their terminal and claim metadata.
    pub fn runs(&self) -> Result<Vec<Value>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {CLAIM_COLS}, a.harness, a.meta, i.title
             FROM claim c JOIN actor a ON a.id = c.actor JOIN issue i ON i.id = c.issue_id
             WHERE {SESSION_CLAIM_FILTER} AND a.kind = 'agent' ORDER BY c.claimed_at, c.id"
        ))?;
        let rows = st.query_map([], |r| {
            let c = row_to_claim(r)?;
            let harness: Option<String> = r.get(13)?;
            let meta: String = r.get(14)?;
            let title: String = r.get(15)?;
            Ok((c, harness, meta, title))
        })?;
        let mut out = vec![];
        for row in rows {
            let (c, harness, meta, title) = row?;
            let mut v = self.claim_json(&c);
            v["issue"] = json!(c.issue_id);
            v["state"] = json!(if c.released_at.is_some() {
                "waiting"
            } else {
                "claimed"
            });
            v["title"] = json!(title);
            v["harness"] = json!(harness);
            v["meta"] = serde_json::from_str(&meta).unwrap_or(json!({}));
            out.push(v);
        }
        Ok(out)
    }

    /// Issues counted by state.
    pub fn counts(&self) -> Result<BTreeMap<String, i64>> {
        let mut st = self
            .conn
            .prepare("SELECT state, count(*) FROM issue GROUP BY state")?;
        let v = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(v)
    }
}
