//! Read models: the issue object of `show --json`, derived state, list,
//! timeline and active agent runs. `derived` is computed here, never stored.

use std::collections::BTreeMap;

use rusqlite::params;
use serde_json::{Map, Value, json};

use crate::error::Result;
use crate::model::*;
use crate::query;
use crate::registry::*;
use crate::store::{CLAIM_COLS, Ctx, ISSUE_COLS, row_to_claim, row_to_issue};

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

impl Ctx<'_> {
    pub fn claim_json(&self, c: &Claim) -> Value {
        json!({
            "actor": c.actor,
            "on_behalf_of": c.on_behalf_of,
            "claimed_at": c.claimed_at,
            "lease_until": c.lease_until,
            "worktree": c.worktree,
            "host": c.host,
            "agent": c.agent,
            "terminal": c.terminal,
        })
    }

    fn edge_json(e: &Edge) -> Value {
        json!({
            "kind": e.kind, "id": e.id, "type": e.ty, "title": e.title,
            "status": e.status, "status_category": e.status_category, "resolution": e.resolution,
        })
    }

    /// Fields still missing for each status (and resolution) reachable next.
    pub fn next_map(&self, issue: &Issue, ty: &TypeDef) -> Result<Value> {
        let mut targets: Vec<String> = vec![];
        match ty.category_of(&issue.status) {
            Some(Category::Open) => {
                if let Some(a) = ty.first_active() {
                    targets.push(a.name.clone());
                }
                targets.extend(
                    ty.transitions
                        .get(&issue.status)
                        .cloned()
                        .unwrap_or_default(),
                );
            }
            Some(Category::Active) => {
                targets.extend(
                    ty.transitions
                        .get(&issue.status)
                        .cloned()
                        .unwrap_or_default(),
                );
            }
            _ => {}
        }
        let mut out = Map::new();
        for t in targets {
            if out.contains_key(&t) {
                continue;
            }
            match ty.category_of(&t) {
                Some(Category::Active) => {
                    let u = self.unmet(issue, ty, &t, None)?;
                    let mut m = u.fields.clone();
                    m.extend(u.edges.iter().map(|e| format!("edge:{e}")));
                    out.insert(t, json!(m));
                }
                Some(Category::Closed) => {
                    let mut per = Map::new();
                    for r in &ty.resolutions {
                        let u = self.unmet(issue, ty, &t, Some(r))?;
                        let mut m = u.fields.clone();
                        m.extend(u.edges.iter().map(|e| format!("edge:{e}")));
                        per.insert(r.clone(), json!(m));
                    }
                    out.insert(t, Value::Object(per));
                }
                _ => {}
            }
        }
        Ok(Value::Object(out))
    }

    /// Unmet gates on statuses ahead of the current one (not counting
    /// blocking kinds, which are reported as `blocked_by`).
    pub fn gates_unmet(&self, issue: &Issue, ty: &TypeDef) -> Result<Vec<Value>> {
        if issue.is_closed() {
            return Ok(vec![]);
        }
        let mut out = vec![];
        let mut seen = std::collections::BTreeSet::new();
        for r in &ty.requirements {
            let ReqKind::InEdgesClosed { dep_kind, .. } = &r.kind else {
                continue;
            };
            if r.to_status == issue.status
                || self
                    .schema
                    .dep_kinds
                    .get(dep_kind)
                    .is_some_and(|k| k.blocks_ready)
            {
                continue;
            }
            if !seen.insert((r.to_status.clone(), r.resolution.clone())) {
                continue;
            }
            let u = self.unmet(issue, ty, &r.to_status, r.resolution.as_deref())?;
            for mut g in u.gates {
                if self
                    .schema
                    .dep_kinds
                    .get(g["kind"].as_str().unwrap_or(""))
                    .is_some_and(|k| k.blocks_ready)
                {
                    continue;
                }
                if let Some(res) = &r.resolution {
                    g["resolution"] = json!(res);
                }
                if !out.contains(&g) {
                    out.push(g);
                }
            }
        }
        Ok(out)
    }

    pub fn blocked_by(&self, id: &str) -> Result<Vec<String>> {
        let mut st = self.conn.prepare(
            "SELECT d.src FROM dep d
             JOIN dep_kind k ON k.name = d.kind AND k.blocks_ready = 1
             JOIN issue_view b ON b.id = d.src
             WHERE d.dst = ?1 AND b.status_category <> 'closed' ORDER BY d.src",
        )?;
        let v = st
            .query_map([id], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(v)
    }

    pub fn derived(&self, issue: &Issue, ty: &TypeDef, claim: &Option<Claim>) -> Result<Value> {
        let blocked_by = self.blocked_by(&issue.id)?;
        let ready = issue.status_category == "open" && claim.is_none() && blocked_by.is_empty();
        let source_priority = issue
            .fields
            .get("priority")
            .and_then(Value::as_str)
            .unwrap_or("P2");
        let source_rank = source_priority
            .strip_prefix('P')
            .and_then(|rank| rank.parse::<i64>().ok())
            .unwrap_or(2);
        let mut st = self.conn.prepare(
            "SELECT d.dst, json_extract(b.fields, '$.priority')
             FROM dep d
             JOIN dep_kind k ON k.name = d.kind AND k.blocks_ready = 1
             JOIN issue_view b ON b.id = d.dst
             WHERE d.src = ?1 AND b.status_category <> 'closed'
             ORDER BY d.dst",
        )?;
        let unblocks: Vec<Value> = st
            .query_map([&issue.id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .filter_map(|row| match row {
                Ok((id, priority))
                    if priority
                        .strip_prefix('P')
                        .and_then(|rank| rank.parse::<i64>().ok())
                        .is_some_and(|rank| rank < source_rank) =>
                {
                    Some(Ok(json!({"id": id, "priority": priority})))
                }
                Ok(_) => None,
                Err(e) => Some(Err(e)),
            })
            .collect::<std::result::Result<_, _>>()?;
        let mut d = json!({
            "ready": ready,
            "blocked_by": blocked_by,
            "gates_unmet": self.gates_unmet(issue, ty)?,
            "next": self.next_map(issue, ty)?,
            "unblocks_higher_priority": unblocks,
        });
        if issue.ty == "milestone" || ty.requirements.iter().any(|r| matches!(&r.kind, ReqKind::InEdgesClosed { dep_kind, .. } if dep_kind == "blocks")) {
            let (total, closed): (i64, i64) = self.conn.query_row(
                "SELECT count(b.id), count(b.id) FILTER (WHERE b.status_category = 'closed')
                 FROM dep d JOIN issue_view b ON b.id = d.src WHERE d.dst = ?1 AND d.kind = 'blocks'",
                [&issue.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            d["progress"] = json!({"total": total, "closed": closed});
        }
        Ok(d)
    }

    /// The issue object: the shape `show --json` emits and `--from` accepts.
    pub fn issue_json(&self, issue: &Issue, with: With) -> Result<Value> {
        let ty = self.schema.ty(&issue.ty)?;
        let claim = self.active_claim(&issue.id)?;
        let mut o = Map::new();
        o.insert("id".into(), json!(issue.id));
        o.insert("type".into(), json!(issue.ty));
        o.insert("project".into(), json!(issue.project));
        o.insert("title".into(), json!(issue.title));
        o.insert("context".into(), json!(issue.context));
        o.insert("status".into(), json!(issue.status));
        o.insert("status_category".into(), json!(issue.status_category));
        o.insert("resolution".into(), json!(issue.resolution));
        o.insert("outcome".into(), json!(issue.outcome));
        o.insert("owner".into(), json!(issue.owner));
        o.insert("links".into(), json!(issue.links));
        o.insert("tags".into(), json!(issue.tags));
        // Every type field, null when unset, in type order.
        let mut fields = Map::new();
        for tf in &ty.fields {
            fields.insert(
                tf.field.clone(),
                issue.fields.get(&tf.field).cloned().unwrap_or(Value::Null),
            );
        }
        for (k, v) in &issue.fields {
            if !fields.contains_key(k) {
                fields.insert(k.clone(), v.clone());
            }
        }
        o.insert("fields".into(), Value::Object(fields));
        o.insert(
            "claim".into(),
            claim
                .as_ref()
                .map(|c| self.claim_json(c))
                .unwrap_or(Value::Null),
        );
        if with.deps {
            let out: Vec<Value> = self
                .edges(&issue.id, true)?
                .iter()
                .map(Self::edge_json)
                .collect();
            let inn: Vec<Value> = self
                .edges(&issue.id, false)?
                .iter()
                .map(Self::edge_json)
                .collect();
            o.insert("deps".into(), json!({"out": out, "in": inn}));
        }
        o.insert("derived".into(), self.derived(issue, ty, &claim)?);
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
        project: Option<&str>,
        sort: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<Value>> {
        let terms = query::parse(q)?;
        let ready = terms.iter().any(|term| {
            matches!(
                term,
                query::Term::Flag {
                    flag: query::Flag::Ready,
                    negated: false
                }
            )
        });
        let compiled = query::compile(&terms, &self.schema, self)?;
        let mut sql = format!(
            "SELECT {ISSUE_COLS} FROM issue_view i WHERE {}",
            compiled.where_sql
        );
        let mut params = compiled.params;
        if let Some(p) = project {
            sql.push_str(" AND i.project_id = ?");
            params.push(rusqlite::types::Value::Text(p.to_string()));
        }
        sql.push_str(&format!(
            " ORDER BY {}",
            query::order_by(sort, &self.schema, ready)?
        ));
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

    /// Active agent claims with their agent, harness, terminal and lease.
    pub fn runs(&self) -> Result<Vec<Value>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {CLAIM_COLS}, a.harness, a.meta, i.title, i.status
             FROM claim c JOIN actor a ON a.id = c.actor JOIN issue i ON i.id = c.issue_id
             WHERE c.released_at IS NULL AND a.kind = 'agent' ORDER BY c.claimed_at"
        ))?;
        let rows = st.query_map([], |r| {
            let c = row_to_claim(r)?;
            let harness: Option<String> = r.get(12)?;
            let meta: String = r.get(13)?;
            let title: String = r.get(14)?;
            let status: String = r.get(15)?;
            Ok((c, harness, meta, title, status))
        })?;
        let mut out = vec![];
        for row in rows {
            let (c, harness, meta, title, status) = row?;
            let mut v = self.claim_json(&c);
            v["issue"] = json!(c.issue_id);
            v["title"] = json!(title);
            v["status"] = json!(status);
            v["harness"] = json!(harness);
            v["meta"] = serde_json::from_str(&meta).unwrap_or(json!({}));
            out.push(v);
        }
        Ok(out)
    }

    /// Issues counted by status category per type (for `whoami`-style summaries).
    pub fn counts(&self) -> Result<BTreeMap<String, i64>> {
        let mut st = self
            .conn
            .prepare("SELECT status_category, count(*) FROM issue_view GROUP BY status_category")?;
        let v = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(v)
    }

    /// Recent schema events (for `trk schema` output).
    pub fn last_schema_event(&self) -> Result<Option<Value>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT at, actor, json_extract(payload, '$.commit') FROM event WHERE op = 'schema' ORDER BY seq DESC LIMIT 1",
                params![],
                |r| Ok(json!({"at": r.get::<_, String>(0)?, "actor": r.get::<_, String>(1)?, "commit": r.get::<_, Option<String>>(2)?})),
            )
            .optional()?)
    }
}
