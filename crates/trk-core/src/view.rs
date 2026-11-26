//! Read models: the issue object of `show --json`, derived state, list,
//! timeline and active or waiting agent runs. `derived` is computed here, never stored.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::error::Result;
use crate::model::*;
use crate::query;
use crate::store::{CLAIM_COLS, Ctx, ISSUE_COLS, SESSION_CLAIM_FILTER, row_to_claim, row_to_issue};

mod types;
pub use types::*;

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

    /// `ready`, `blocked_by`, `due`, blocker progress, and the open issues of
    /// higher priority this one holds up.
    pub fn derived(&self, issue: &Issue, blockers: &[Edge], blocks: &[Edge]) -> Derived {
        let blocked_by: Vec<String> = blockers
            .iter()
            .filter(|b| b.state != State::Closed)
            .map(|b| b.id.clone())
            .collect();
        let ready = issue.state == State::Open && blocked_by.is_empty();
        let due = issue.state == State::Waiting
            && issue.waiting_check.is_some()
            && issue
                .next_check_at
                .as_deref()
                .is_none_or(|t| t <= self.ts().as_str());
        let mine = rank(&issue.priority);
        let unblocks = blocks
            .iter()
            .filter(|b| b.state != State::Closed && rank(&b.priority) < mine)
            .map(|b| Unblocks {
                id: b.id.clone(),
                priority: b.priority.clone(),
            })
            .collect();
        Derived {
            ready,
            blocked_by,
            due,
            unblocks_higher_priority: unblocks,
            progress: (!blockers.is_empty()).then(|| Progress {
                total: blockers.len(),
                closed: blockers.iter().filter(|b| b.state == State::Closed).count(),
            }),
        }
    }

    /// The issue object: the shape `show --json` emits.
    pub fn issue_view(&self, issue: &Issue, with: With) -> Result<IssueView> {
        let claim = self.active_claim(&issue.id)?;
        let blocks = self.edges(&issue.id, true)?;
        let blockers = self.edges(&issue.id, false)?;
        let derived = self.derived(issue, &blockers, &blocks);
        Ok(IssueView {
            id: issue.id.clone(),
            title: issue.title.clone(),
            description: issue.description.clone(),
            state: issue.state,
            priority: issue.priority.clone(),
            issue_type: issue.issue_type.clone(),
            epic: issue.epic.clone(),
            close_reason: issue.close_reason.clone(),
            wake_count: (issue.wake_count > 0
                && matches!(issue.state, State::Waiting | State::Claimed))
            .then_some(issue.wake_count),
            waiting: (issue.state == State::Waiting).then(|| WaitingView {
                reason: issue.waiting_reason.clone(),
                check: issue.waiting_check.clone(),
                interval: issue.check_interval,
                next_check_at: issue.next_check_at.clone(),
                last_check_at: issue.last_check_at.clone(),
                last_check_exit: issue.last_check_exit,
                cwd: issue.wait_cwd.clone(),
            }),
            claim: claim.map(ClaimView::from),
            deps: with.deps.then_some(Dependencies {
                blocked_by: blockers,
                blocks,
            }),
            links: self.links(&issue.id)?,
            derived,
            created_by: issue.created_by.clone(),
            created_at: issue.created_at.clone(),
            updated_at: issue.updated_at.clone(),
            closed_at: issue.closed_at.clone(),
            notes: with.notes.then(|| self.notes(&issue.id)).transpose()?,
            events: with.events.then(|| self.events(&issue.id)).transpose()?,
            claims: with
                .claims
                .then(|| -> Result<_> {
                    Ok(self
                        .claims(&issue.id)?
                        .into_iter()
                        .map(|mut claim| {
                            let released_at = claim.released_at.take();
                            let release_reason = claim.release_reason.take();
                            ClaimHistory {
                                claim: ClaimView::from(claim),
                                released_at,
                                release_reason,
                            }
                        })
                        .collect())
                })
                .transpose()?,
        })
    }

    pub fn show(&self, id: &str, with: With) -> Result<Value> {
        let id = self.resolve_id(id)?;
        let issue = self.issue(&id)?;
        Ok(json!(self.issue_view(&issue, with)?))
    }

    pub fn notes(&self, id: &str) -> Result<Vec<Value>> {
        let mut st = self.conn.prepare(
            "SELECT n.seq, n.actor, n.at, n.body, n.amends, json_extract(e.payload, '$.to_agent')
             FROM note n JOIN event e ON e.seq = n.seq WHERE n.issue_id = ?1 ORDER BY n.seq",
        )?;
        let v = st
            .query_map([id], |r| {
                Ok(json!({
                    "seq": r.get::<_, i64>(0)?, "actor": r.get::<_, String>(1)?, "at": r.get::<_, String>(2)?,
                    "body": r.get::<_, Option<String>>(3)?, "amends": r.get::<_, Option<i64>>(4)?,
                    "to_agent": r.get::<_, Option<bool>>(5)?.unwrap_or(false),
                }))
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(v)
    }

    /// `trk list`: issues matching the query.
    pub fn list(
        &self,
        q: &str,
        sort: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<IssueView>> {
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
            out.push(self.issue_view(
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
    pub fn runs(&self) -> Result<Vec<RunView>> {
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
            let (mut c, harness, meta, title) = row?;
            let issue = std::mem::take(&mut c.issue_id);
            let state = if c.released_at.is_some() {
                State::Waiting
            } else {
                State::Claimed
            };
            out.push(RunView {
                claim: ClaimView::from(c),
                issue,
                state,
                title,
                harness,
                meta: serde_json::from_str(&meta).unwrap_or(json!({})),
            });
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
