//! Issue operations. Each runs inside a write [`Ctx`]; status changes happen
//! only through claim, unclaim, move and close.

use rand::RngExt;
use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value, json};

use crate::error::{Code, Error, Result, bad_args, invalid_field, not_found};
use crate::fields::{Assign, AssignOp, coerce, coerce_item, is_present, item_count};
use crate::model::*;
use crate::registry::*;
use crate::store::Ctx;

const ID_ALPHABET: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";

#[derive(Debug, Clone, Default)]
pub struct NewIssue {
    pub project: String,
    pub ty: String,
    pub assigns: Vec<Assign>,
    pub deps: Vec<NewDep>,
}

/// An edge created together with a new issue.
#[derive(Debug, Clone)]
pub struct NewDep {
    pub kind: String,
    pub other: String,
    /// true: new → other; false: other → new.
    pub new_is_src: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ClaimOpts {
    pub lease: Option<jiff::SignedDuration>,
    pub worktree: Option<String>,
    pub host: Option<String>,
    pub agent: Option<String>,
    pub terminal: Option<String>,
    pub steal: bool,
    /// Current git branch; written to an empty `branch` field.
    pub branch: Option<String>,
    pub on_behalf_of: Option<String>,
    pub assigns: Vec<Assign>,
}

/// Requirements not met when entering a status.
#[derive(Debug, Default, Clone)]
pub struct Unmet {
    pub fields: Vec<String>,
    pub min_items: Vec<Value>,
    pub edges: Vec<String>,
    pub gates: Vec<Value>,
}

impl Unmet {
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty() && self.edges.is_empty() && self.gates.is_empty()
    }
}

const PROTECTED: &[&str] = &[
    "id",
    "type",
    "project",
    "status",
    "resolution",
    "created_by",
    "created_at",
    "updated_at",
    "closed_at",
];

impl Ctx<'_> {
    // ------------------------------------------------------------------
    // Fields
    // ------------------------------------------------------------------

    fn field_def_for(&self, ty: &TypeDef, name: &str) -> Result<FieldDef> {
        if PROTECTED.contains(&name) {
            let hint = match name {
                "status" => "status changes only through claim, unclaim, move and close",
                "resolution" => "set the resolution with close --resolution",
                _ => "this field is written by trk",
            };
            return Err(invalid_field(name, "cannot be set directly").with_hint(hint));
        }
        if let Some(def) = self.schema.field(name)
            && (def.shared || ty.type_field(name).is_some())
        {
            return Ok(def.clone());
        }
        let mut names: Vec<String> = SHARED_COLUMNS.iter().map(|s| s.to_string()).collect();
        names.extend(ty.fields.iter().map(|f| f.field.clone()));
        Err(
            invalid_field(name, format!("type '{}' has no such field", ty.name))
                .with_hint(format!("fields of {}: {}", ty.name, names.join(", "))),
        )
    }

    /// Apply assignments to `issue` in memory; returns {field: {from, to}}.
    pub fn apply_assigns(
        &self,
        issue: &mut Issue,
        ty: &TypeDef,
        assigns: &[Assign],
    ) -> Result<Map<String, Value>> {
        let mut changes = Map::new();
        for a in assigns {
            let def = self.field_def_for(ty, &a.field)?;
            let old = issue.get(&def.name);
            let new = match a.op {
                AssignOp::Set => coerce(&def, &a.value, self)?,
                AssignOp::Append | AssignOp::Remove => {
                    let item = coerce_item(&def, &a.value, self)?;
                    let mut arr = match &old {
                        Some(Value::Array(v)) => v.clone(),
                        _ => vec![],
                    };
                    let same = |x: &Value| match (x.get("url"), item.get("url")) {
                        (Some(u1), Some(u2)) => u1 == u2,
                        _ => *x == item,
                    };
                    if a.op == AssignOp::Append {
                        if !arr.iter().any(same) {
                            arr.push(item.clone());
                        }
                    } else {
                        arr.retain(|x| !same(x));
                    }
                    Value::Array(arr)
                }
            };
            self.store_field(issue, &def, new.clone())?;
            let after = issue.get(&def.name);
            if after != old {
                changes.insert(def.name.clone(), json!({"from": old, "to": after}));
            }
        }
        Ok(changes)
    }

    fn store_field(&self, issue: &mut Issue, def: &FieldDef, v: Value) -> Result<()> {
        match def.name.as_str() {
            "title" | "context" => {
                let s = v.as_str().map(|s| s.to_string()).unwrap_or_default();
                if s.trim().is_empty() {
                    return Err(invalid_field(&def.name, "is required and cannot be empty"));
                }
                if def.name == "title" {
                    issue.title = s;
                } else {
                    issue.context = s;
                }
            }
            "outcome" => issue.outcome = v.as_str().map(String::from),
            "owner" => {
                let o = v.as_str().map(String::from);
                if let Some(o) = &o {
                    self.ensure_actor(o, None)?;
                }
                issue.owner = o;
            }
            "links" => {
                issue.links = match v {
                    Value::Array(a) => a,
                    _ => vec![],
                }
            }
            "tags" => {
                issue.tags = match v {
                    Value::Array(a) => a
                        .into_iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect(),
                    _ => vec![],
                }
            }
            "priority" => {
                if !matches!(&v, Value::String(p) if ["P0", "P1", "P2", "P3"].contains(&p.as_str()))
                {
                    return Err(invalid_field("priority", "must be P0, P1, P2, or P3"));
                }
                issue.fields.insert("priority".into(), v);
            }
            name => {
                if is_present(Some(&v)) || matches!(v, Value::Bool(_)) {
                    issue.fields.insert(name.to_string(), v);
                } else {
                    issue.fields.remove(name);
                }
            }
        }
        Ok(())
    }

    fn save_issue(&self, issue: &Issue) -> Result<()> {
        self.conn.execute(
            "UPDATE issue SET title = ?2, context = ?3, outcome = ?4, owner = ?5, links = ?6, tags = ?7,
                    fields = ?8, status = ?9, resolution = ?10, closed_at = ?11, updated_at = ?12
             WHERE id = ?1",
            params![
                issue.id,
                issue.title,
                issue.context,
                issue.outcome,
                issue.owner,
                Value::Array(issue.links.clone()).to_string(),
                json!(issue.tags).to_string(),
                Value::Object(issue.fields.clone()).to_string(),
                issue.status,
                issue.resolution,
                issue.closed_at,
                self.ts(),
            ],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Requirements
    // ------------------------------------------------------------------

    pub fn unmet(
        &self,
        issue: &Issue,
        ty: &TypeDef,
        to: &str,
        resolution: Option<&str>,
    ) -> Result<Unmet> {
        let mut u = Unmet::default();
        for r in ty.requirements_for(to, resolution) {
            match &r.kind {
                ReqKind::Field { field } => {
                    if !is_present(issue.get(field).as_ref()) && !u.fields.contains(field) {
                        u.fields.push(field.clone());
                    }
                }
                ReqKind::MinItems { field, n } => {
                    let have = item_count(issue.get(field).as_ref());
                    if have < *n {
                        u.min_items
                            .push(json!({"field": field, "min": n, "have": have}));
                        if !u.fields.contains(field) {
                            u.fields.push(field.clone());
                        }
                    }
                }
                ReqKind::OutEdge { dep_kind } => {
                    let n: i64 = self.conn.query_row(
                        "SELECT count(*) FROM dep WHERE src = ?1 AND kind = ?2",
                        params![issue.id, dep_kind],
                        |r| r.get(0),
                    )?;
                    if n == 0 {
                        u.edges.push(dep_kind.clone());
                    }
                }
                ReqKind::InEdgesClosed {
                    dep_kind,
                    src_resolution,
                } => {
                    let mut st = self.conn.prepare(
                        "SELECT s.id, s.type, s.status, s.status_category, s.resolution
                         FROM dep d JOIN issue_view s ON s.id = d.src
                         WHERE d.dst = ?1 AND d.kind = ?2
                           AND (s.status_category <> 'closed' OR (?3 IS NOT NULL AND s.resolution IS NOT ?3))",
                    )?;
                    let rows: Vec<Value> = st
                        .query_map(params![issue.id, dep_kind, src_resolution], |r| {
                            Ok(json!({
                                "kind": dep_kind,
                                "src": r.get::<_, String>(0)?,
                                "src_type": r.get::<_, String>(1)?,
                                "src_status": r.get::<_, String>(2)?,
                                "src_resolution": r.get::<_, Option<String>>(4)?,
                                "required": src_resolution.clone().unwrap_or_else(|| "closed".into()),
                                "entering": to,
                            }))
                        })?
                        .collect::<std::result::Result<_, _>>()?;
                    u.gates.extend(rows);
                }
            }
        }
        Ok(u)
    }

    /// Field-type requirements (field, min_items) unmet for a status; used by plan.
    pub fn unmet_fields(
        &self,
        issue: &Issue,
        ty: &TypeDef,
        to: &str,
        resolution: Option<&str>,
    ) -> Vec<String> {
        let mut out = vec![];
        for r in ty.requirements_for(to, resolution) {
            match &r.kind {
                ReqKind::Field { field } if !is_present(issue.get(field).as_ref()) => {
                    out.push(field.clone())
                }
                ReqKind::MinItems { field, n } if item_count(issue.get(field).as_ref()) < *n => {
                    out.push(format!("{field} (≥{n})"))
                }
                _ => {}
            }
        }
        out
    }

    fn unmet_error(&self, u: Unmet, issue: &Issue, message: String, cmd: String) -> Error {
        let code = if u.fields.is_empty() && u.edges.is_empty() {
            Code::GateUnmet
        } else {
            Code::RequirementsUnmet
        };
        let mut hint = cmd;
        for f in &u.fields {
            let md = self
                .schema
                .field(f)
                .is_some_and(|d| d.kind == FieldKind::Markdown);
            hint.push_str(&format!(" -f {f}={}", if md { "@-" } else { "..." }));
        }
        for e in &u.edges {
            hint.push_str(&format!("; first: trk dep add {} {e} <ID>", issue.id));
        }
        if !u.gates.is_empty() {
            let srcs: Vec<String> = u
                .gates
                .iter()
                .filter_map(|g| g["src"].as_str().map(String::from))
                .collect();
            let req = match u.gates[0]["required"].as_str() {
                Some("closed") | None => "closed".to_string(),
                Some(r) => format!("closed as '{r}'"),
            };
            if u.fields.is_empty() && u.edges.is_empty() {
                hint = format!("{} must be {req} first", srcs.join(", "));
            } else {
                hint.push_str(&format!("; and {} must be {req}", srcs.join(", ")));
            }
        }
        Error::new(code, message)
            .with_issue(&issue.id)
            .with_detail("missing_fields", json!(u.fields))
            .with_detail("missing_edges", json!(u.edges))
            .with_detail("min_items", json!(u.min_items))
            .with_detail("unmet_gates", json!(u.gates))
            .with_hint(hint)
    }

    fn claimant_check(
        &self,
        issue: &Issue,
        claim: &Option<Claim>,
        force: bool,
        what: &str,
    ) -> Result<bool> {
        if let Some(c) = claim
            && c.actor != self.actor_str()
        {
            if force && !self.actor().is_agent() {
                return Ok(true);
            }
            return Err(Error::new(
                Code::NotClaimant,
                format!(
                    "{} is claimed by {}; only the claimant may {what} it",
                    issue.id, c.actor
                ),
            )
            .with_issue(&issue.id)
            .with_detail("claimant", c.actor.clone())
            .with_hint("pass --force to override (recorded)"));
        }
        Ok(false)
    }

    // ------------------------------------------------------------------
    // Create / edit
    // ------------------------------------------------------------------

    fn new_id(&self, project: &str) -> Result<String> {
        let mut rng = rand::rng();
        for len in 3..=8 {
            for _ in 0..8 {
                let suffix: String = (0..len)
                    .map(|_| ID_ALPHABET[rng.random_range(0..ID_ALPHABET.len())] as char)
                    .collect();
                let id = format!("{project}-{suffix}");
                let taken: bool = self
                    .conn
                    .query_row("SELECT 1 FROM issue WHERE id = ?1", [&id], |_| Ok(true))
                    .optional()?
                    .unwrap_or(false);
                if !taken {
                    return Ok(id);
                }
            }
        }
        Err(Error::new(Code::Internal, "could not allocate an issue id"))
    }

    pub fn create(&self, new: NewIssue) -> Result<String> {
        self.schema.require_applied()?;
        if !self.project_exists(&new.project)? {
            return Err(
                not_found("project", &new.project).with_hint("create it with `trk project add`")
            );
        }
        let ty = self.schema.ty(&new.ty)?.clone();
        let mut deps = new.deps.clone();
        let mut resolved = vec![];
        for d in &mut deps {
            d.other = self.resolve_id(&d.other)?;
            resolved.push(d.other.clone());
        }
        if self.actor().is_agent() {
            let mut ok = false;
            for d in &deps {
                if d.kind == "discovered-from" && d.new_is_src && self.is_claimed_by_me(&d.other)? {
                    ok = true;
                }
            }
            if !ok {
                return Err(Error::new(
                    Code::NotPermitted,
                    "agent actors may create issues only as discovered-from follow-ups of an issue they claim",
                ));
            }
        }
        let open = ty.open_status().ok_or_else(|| {
            Error::new(
                Code::SchemaInvalid,
                format!("type '{}' has no open status", ty.name),
            )
        })?;
        let id = self.new_id(&new.project)?;
        let mut issue = Issue {
            id: id.clone(),
            project: new.project.clone(),
            ty: ty.name.clone(),
            title: String::new(),
            context: String::new(),
            status: open.name.clone(),
            status_category: "open".into(),
            resolution: None,
            outcome: None,
            owner: None,
            links: vec![],
            tags: vec![],
            fields: {
                let mut fields = Map::new();
                fields.insert("priority".into(), Value::String("P2".into()));
                fields
            },
            created_by: self.actor_str(),
            created_at: self.ts(),
            updated_at: self.ts(),
            closed_at: None,
        };
        for tf in &ty.fields {
            if let Some(d) = &tf.default {
                issue.fields.insert(tf.field.clone(), d.clone());
            }
            let is_repo = self
                .schema
                .field(&tf.field)
                .is_some_and(|f| f.kind == FieldKind::Repo);
            if is_repo
                && !new.assigns.iter().any(|a| a.field == tf.field)
                && let Some(r) = &self.opts.current_repo
            {
                issue
                    .fields
                    .insert(tf.field.clone(), Value::String(r.clone()));
            }
        }
        // title/context are set through assigns; skip their emptiness check until after.
        for a in &new.assigns {
            if (a.field == "title" || a.field == "context") && a.op == AssignOp::Set {
                let s = a.value.as_str().unwrap_or_default();
                if s.trim().is_empty() {
                    continue;
                }
            }
            self.apply_assigns(&mut issue, &ty, std::slice::from_ref(a))?;
        }
        let mut missing = vec![];
        if issue.title.trim().is_empty() {
            missing.push("title".to_string());
        }
        if issue.context.trim().is_empty() {
            missing.push("context".to_string());
        }
        for tf in &ty.fields {
            if tf.required_at_create && !is_present(issue.get(&tf.field).as_ref()) {
                missing.push(tf.field.clone());
            }
        }
        if !missing.is_empty() {
            let mut hint = format!("trk create {} -t TITLE", ty.name);
            for f in &missing {
                if f != "title" {
                    hint.push_str(&format!(" -f {f}=..."));
                }
            }
            return Err(Error::new(
                Code::RequirementsUnmet,
                format!("{} requires fields at creation", ty.name),
            )
            .with_detail("missing_fields", json!(missing))
            .with_hint(hint));
        }
        self.conn.execute(
            "INSERT INTO issue (id, project_id, type, status, title, context, outcome, owner, links, tags,
                                fields, created_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
            params![
                issue.id,
                issue.project,
                issue.ty,
                issue.status,
                issue.title,
                issue.context,
                issue.outcome,
                issue.owner,
                Value::Array(issue.links.clone()).to_string(),
                json!(issue.tags).to_string(),
                Value::Object(issue.fields.clone()).to_string(),
                issue.created_by,
                self.ts(),
            ],
        )?;
        self.event(
            Some(&id),
            "create",
            json!({
                "type": issue.ty, "project": issue.project, "status": issue.status,
                "title": issue.title, "context": issue.context, "outcome": issue.outcome,
                "owner": issue.owner, "links": issue.links, "tags": issue.tags, "fields": issue.fields,
            }),
        )?;
        for d in deps {
            let (src, dst) = if d.new_is_src {
                (id.clone(), d.other.clone())
            } else {
                (d.other.clone(), id.clone())
            };
            self.dep_insert(&src, &d.kind, &dst)?;
        }
        for other in resolved {
            self.touch_lease_pub(&other)?;
        }
        Ok(id)
    }

    fn is_claimed_by_me(&self, id: &str) -> Result<bool> {
        Ok(self
            .active_claim(id)?
            .is_some_and(|c| c.actor == self.actor_str()))
    }

    pub fn touch_lease_pub(&self, issue_id: &str) -> Result<()> {
        let until = fmt_ts(self.now + self.opts.agent_lease);
        self.conn.execute(
            "UPDATE claim SET lease_until = max(lease_until, ?3)
             WHERE issue_id = ?1 AND actor = ?2 AND released_at IS NULL AND lease_until IS NOT NULL",
            params![issue_id, self.actor_str(), until],
        )?;
        Ok(())
    }

    fn agent_gate(&self, issue_id: &str, claim: &Option<Claim>) -> Result<()> {
        if self.actor().is_agent() && claim.as_ref().is_none_or(|c| c.actor != self.actor_str()) {
            return Err(Error::new(
                Code::NotPermitted,
                "agent actors may only write to issues they currently claim",
            )
            .with_issue(issue_id));
        }
        Ok(())
    }

    /// Change field values only.
    pub fn edit(&self, id: &str, assigns: &[Assign]) -> Result<Map<String, Value>> {
        let id = self.resolve_id(id)?;
        let mut issue = self.issue(&id)?;
        let claim = self.active_claim(&id)?;
        self.agent_gate(&id, &claim)?;
        let ty = self.schema.ty(&issue.ty)?.clone();
        let before = self.unmet_fields(
            &issue,
            &ty,
            &issue.status.clone(),
            issue.resolution.clone().as_deref(),
        );
        let changes = self.apply_assigns(&mut issue, &ty, assigns)?;
        if changes.is_empty() {
            return Ok(changes);
        }
        let after = self.unmet_fields(
            &issue,
            &ty,
            &issue.status.clone(),
            issue.resolution.clone().as_deref(),
        );
        let newly: Vec<String> = after.into_iter().filter(|f| !before.contains(f)).collect();
        if !newly.is_empty() {
            return Err(Error::new(
                Code::RequirementsUnmet,
                format!(
                    "{} is required while the issue is '{}'",
                    newly.join(", "),
                    issue.status
                ),
            )
            .with_issue(&id)
            .with_detail("missing_fields", json!(newly)));
        }
        self.save_issue(&issue)?;
        self.event(Some(&id), "set", json!({"changes": changes}))?;
        self.touch_lease_pub(&id)?;
        Ok(changes)
    }

    // ------------------------------------------------------------------
    // Claims
    // ------------------------------------------------------------------

    pub fn claim(&self, id: &str, opts: ClaimOpts) -> Result<Claim> {
        let id = self.resolve_id(id)?;
        let mut issue = self.issue(&id)?;
        let ty = self.schema.ty(&issue.ty)?.clone();
        let me = self.actor_str();
        if let Some(obo) = &opts.on_behalf_of {
            self.ensure_actor(&me, Some(obo))?;
        }
        let Some(first) = ty.first_active().cloned() else {
            return Err(Error::new(
                Code::InvalidTransition,
                format!(
                    "type '{}' has no intermediate status, so it cannot be claimed",
                    ty.name
                ),
            )
            .with_issue(&id));
        };
        let existing = self.active_claim(&id)?;
        let lease = opts.lease.or(if self.actor().is_agent() {
            Some(self.opts.agent_lease)
        } else {
            None
        });
        let lease_until = lease.map(|d| fmt_ts(self.now + d));
        let mut payload = json!({});
        let from = issue.status.clone();
        if let Some(c) = &existing {
            if c.actor == me && !opts.steal {
                return Ok(c.clone());
            }
            if !opts.steal {
                return Err(Error::new(
                    Code::ClaimedByOther,
                    format!("{id} is claimed by {}", c.actor),
                )
                .with_issue(&id)
                .with_detail("claimant", c.actor.clone())
                .with_hint(format!("trk claim {id} --steal")));
            }
            self.conn.execute(
                "UPDATE claim SET released_at = ?2, release_reason = 'stolen' WHERE id = ?1",
                params![c.id, self.ts()],
            )?;
            self.event(Some(&id), "release", json!({"claim": c.id, "claimant": c.actor, "reason": "stolen", "from": from, "to": from}))?;
            payload["stolen_from"] = json!(c.actor);
            // Fields may still be supplied with a steal.
            let changes = self.apply_assigns(&mut issue, &ty, &opts.assigns)?;
            if !changes.is_empty() {
                self.save_issue(&issue)?;
                self.event(Some(&id), "set", json!({"changes": changes}))?;
            }
        } else {
            if issue.status_category != "open" {
                return Err(Error::new(
                    Code::InvalidTransition,
                    format!(
                        "{id} is '{}'; only open issues can be claimed",
                        issue.status
                    ),
                )
                .with_issue(&id));
            }
            let mut assigns = opts.assigns.clone();
            if let Some(b) = &opts.branch
                && ty.type_field("branch").is_some()
                && !is_present(issue.get("branch").as_ref())
                && !assigns.iter().any(|a| a.field == "branch")
            {
                assigns.push(Assign::set("branch", b.clone()));
            }
            let changes = self.apply_assigns(&mut issue, &ty, &assigns)?;
            let u = self.unmet(&issue, &ty, &first.name, None)?;
            if !u.is_empty() {
                return Err(self.unmet_error(
                    u,
                    &issue,
                    format!("{} cannot enter '{}'", ty.name, first.name),
                    format!("trk claim {id}"),
                ));
            }
            issue.status = first.name.clone();
            self.save_issue(&issue)?;
            if !changes.is_empty() {
                self.event(Some(&id), "set", json!({"changes": changes}))?;
            }
        }
        self.conn.execute(
            "INSERT INTO claim (issue_id, actor, claimed_at, lease_until, worktree, host, agent, terminal)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![id, me, self.ts(), lease_until, opts.worktree, opts.host, opts.agent, opts.terminal],
        )?;
        let claim_id = self.conn.last_insert_rowid();
        let mut p = json!({
            "claim": claim_id, "from": from, "to": issue.status, "lease_until": lease_until,
            "worktree": opts.worktree, "host": opts.host, "agent": opts.agent, "terminal": opts.terminal,
        });
        if let Some(s) = payload.get("stolen_from") {
            p["stolen_from"] = s.clone();
        }
        self.event(Some(&id), "claim", p)?;
        self.active_claim(&id)?
            .ok_or_else(|| Error::new(Code::Internal, "claim vanished"))
    }

    /// Release the active claim and return the issue to open.
    pub fn unclaim(&self, id: &str, force: bool, note: Option<&str>) -> Result<()> {
        let id = self.resolve_id(id)?;
        let mut issue = self.issue(&id)?;
        let claim = self.active_claim(&id)?;
        let Some(c) = claim.clone() else {
            return Err(
                Error::new(Code::InvalidTransition, format!("{id} is not claimed")).with_issue(&id),
            );
        };
        self.agent_gate(&id, &claim)?;
        let forced = self.claimant_check(&issue, &claim, force, "unclaim")?;
        let ty = self.schema.ty(&issue.ty)?.clone();
        let open = ty
            .open_status()
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "open".into());
        let from = issue.status.clone();
        // The note goes first: it is written while the claim still exists.
        if let Some(n) = note {
            self.note(&id, n, None, false)?;
        }
        self.conn.execute(
            "UPDATE claim SET released_at = ?2, release_reason = 'unclaimed' WHERE id = ?1",
            params![c.id, self.ts()],
        )?;
        issue.status = open.clone();
        self.save_issue(&issue)?;
        self.event(
            Some(&id),
            "release",
            json!({"claim": c.id, "claimant": c.actor, "reason": "unclaimed", "from": from, "to": open, "forced": forced}),
        )?;
        Ok(())
    }

    /// Release a claim on behalf of `trk reap`.
    pub fn reap(&self, id: &str, why: &str) -> Result<()> {
        let claim = self
            .active_claim(id)?
            .ok_or_else(|| not_found("active claim on", id))?;
        let mut issue = self.issue(id)?;
        let ty = self.schema.ty(&issue.ty)?.clone();
        let open = ty
            .open_status()
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "open".into());
        let from = issue.status.clone();
        self.note(id, why, None, false)?;
        self.conn.execute(
            "UPDATE claim SET released_at = ?2, release_reason = 'unclaimed' WHERE id = ?1",
            params![claim.id, self.ts()],
        )?;
        issue.status = open.clone();
        self.save_issue(&issue)?;
        self.event(
            Some(id),
            "release",
            json!({"claim": claim.id, "claimant": claim.actor, "reason": "unclaimed", "reaped": true, "from": from, "to": open}),
        )?;
        Ok(())
    }

    pub fn renew(&self, id: &str, lease: Option<jiff::SignedDuration>) -> Result<Claim> {
        let id = self.resolve_id(id)?;
        let claim = self.active_claim(&id)?;
        let Some(c) = claim else {
            return Err(
                Error::new(Code::InvalidTransition, format!("{id} is not claimed")).with_issue(&id),
            );
        };
        if c.actor != self.actor_str() {
            return Err(
                Error::new(Code::NotClaimant, format!("{id} is claimed by {}", c.actor))
                    .with_issue(&id),
            );
        }
        let lease = lease.or(if c.lease_until.is_some() {
            Some(self.opts.agent_lease)
        } else {
            None
        });
        if let Some(d) = lease {
            self.conn.execute(
                "UPDATE claim SET lease_until = ?2 WHERE id = ?1",
                params![c.id, fmt_ts(self.now + d)],
            )?;
        }
        self.active_claim(&id)?
            .ok_or_else(|| Error::new(Code::Internal, "claim vanished"))
    }

    // ------------------------------------------------------------------
    // Move / close
    // ------------------------------------------------------------------

    pub fn move_to(&self, id: &str, to: &str, assigns: &[Assign], force: bool) -> Result<()> {
        let id = self.resolve_id(id)?;
        let mut issue = self.issue(&id)?;
        let ty = self.schema.ty(&issue.ty)?.clone();
        let claim = self.active_claim(&id)?;
        self.agent_gate(&id, &claim)?;
        let target = ty.status(to).ok_or_else(|| {
            Error::new(
                Code::InvalidTransition,
                format!("'{to}' is not a status of {}", ty.name),
            )
            .with_issue(&id)
            .with_hint(format!(
                "statuses: {}",
                ty.statuses
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
        match target.category {
            Category::Open => {
                return Err(Error::new(
                    Code::InvalidTransition,
                    "returning to open releases the claim",
                )
                .with_issue(&id)
                .with_hint(format!("trk unclaim {id}")));
            }
            Category::Closed => {
                return Err(
                    Error::new(Code::InvalidTransition, "closing needs a resolution")
                        .with_issue(&id)
                        .with_hint(format!(
                            "trk close {id} --resolution <{}>",
                            ty.resolutions.join("|")
                        )),
                );
            }
            Category::Active => {}
        }
        if claim.is_none() {
            return Err(
                Error::new(Code::InvalidTransition, format!("{id} is not claimed"))
                    .with_issue(&id)
                    .with_hint(format!("trk claim {id}")),
            );
        }
        let forced = self.claimant_check(&issue, &claim, force, "move")?;
        if !ty.has_transition(&issue.status, to) {
            let allowed = ty
                .transitions
                .get(&issue.status)
                .cloned()
                .unwrap_or_default();
            return Err(Error::new(
                Code::InvalidTransition,
                format!("{}: no transition {} → {to}", ty.name, issue.status),
            )
            .with_issue(&id)
            .with_detail("allowed", json!(allowed)));
        }
        let changes = self.apply_assigns(&mut issue, &ty, assigns)?;
        let u = self.unmet(&issue, &ty, to, None)?;
        if !u.is_empty() {
            return Err(self.unmet_error(
                u,
                &issue,
                format!("{} cannot enter '{to}'", ty.name),
                format!("trk move {id} {to}"),
            ));
        }
        let from = issue.status.clone();
        issue.status = to.to_string();
        self.save_issue(&issue)?;
        if !changes.is_empty() {
            self.event(Some(&id), "set", json!({"changes": changes}))?;
        }
        self.event(
            Some(&id),
            "move",
            json!({"from": from, "to": to, "forced": forced}),
        )?;
        self.touch_lease_pub(&id)?;
        Ok(())
    }

    pub fn close(&self, id: &str, resolution: &str, assigns: &[Assign], force: bool) -> Result<()> {
        let id = self.resolve_id(id)?;
        let mut issue = self.issue(&id)?;
        let ty = self.schema.ty(&issue.ty)?.clone();
        let claim = self.active_claim(&id)?;
        self.agent_gate(&id, &claim)?;
        if issue.status_category == "closed" {
            return Err(
                Error::new(Code::InvalidTransition, format!("{id} is already closed"))
                    .with_issue(&id),
            );
        }
        if !ty.resolutions.iter().any(|r| r == resolution) {
            return Err(Error::new(
                Code::InvalidField,
                format!("'{resolution}' is not a resolution of {}", ty.name),
            )
            .with_issue(&id)
            .with_detail("allowed", json!(ty.resolutions)));
        }
        let closed = ty
            .closed_status()
            .cloned()
            .ok_or_else(|| Error::new(Code::SchemaInvalid, "type has no closed status"))?;
        if !ty.has_transition(&issue.status, &closed.name) {
            return Err(Error::new(
                Code::InvalidTransition,
                format!(
                    "{}: no transition {} → {}",
                    ty.name, issue.status, closed.name
                ),
            )
            .with_issue(&id));
        }
        let forced = self.claimant_check(&issue, &claim, force, "close")?;
        let changes = self.apply_assigns(&mut issue, &ty, assigns)?;
        let u = self.unmet(&issue, &ty, &closed.name, Some(resolution))?;
        if !u.is_empty() {
            return Err(self.unmet_error(
                u,
                &issue,
                format!("{} cannot close as '{resolution}'", ty.name),
                format!("trk close {id} --resolution {resolution}"),
            ));
        }
        let from = issue.status.clone();
        issue.status = closed.name.clone();
        issue.resolution = Some(resolution.to_string());
        issue.closed_at = Some(self.ts());
        self.save_issue(&issue)?;
        if !changes.is_empty() {
            self.event(Some(&id), "set", json!({"changes": changes}))?;
        }
        self.event(
            Some(&id),
            "close",
            json!({"from": from, "resolution": resolution, "forced": forced}),
        )?;
        if let Some(c) = claim {
            self.conn.execute(
                "UPDATE claim SET released_at = ?2, release_reason = 'closed' WHERE id = ?1",
                params![c.id, self.ts()],
            )?;
            self.event(
                Some(&id),
                "release",
                json!({"claim": c.id, "claimant": c.actor, "reason": "closed"}),
            )?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Notes
    // ------------------------------------------------------------------

    pub fn note(&self, id: &str, body: &str, amends: Option<i64>, to_agent: bool) -> Result<i64> {
        let id = self.resolve_id(id)?;
        let claim = self.active_claim(&id)?;
        self.agent_gate(&id, &claim)?;
        if body.trim().is_empty() {
            return Err(bad_args("a note needs text"));
        }
        if let Some(seq) = amends {
            let ok: Option<String> = self
                .conn
                .query_row(
                    "SELECT issue_id FROM event WHERE seq = ?1 AND op = 'note'",
                    [seq],
                    |r| r.get(0),
                )
                .optional()?;
            if ok.as_deref() != Some(id.as_str()) {
                return Err(bad_args(format!(
                    "--amends {seq}: no note with that seq on {id}"
                )));
            }
        }
        let mut payload = json!({"body": body});
        if let Some(a) = amends {
            payload["amends"] = json!(a);
        }
        if to_agent {
            payload["to_agent"] = json!(true);
        }
        let seq = self.event(Some(&id), "note", payload)?;
        if to_agent {
            self.conn.execute(
                "INSERT INTO inbox (note_seq, issue_id) VALUES (?1, ?2)",
                params![seq, id],
            )?;
        }
        self.conn.execute(
            "UPDATE issue SET updated_at = ?2 WHERE id = ?1",
            params![id, self.ts()],
        )?;
        self.touch_lease_pub(&id)?;
        Ok(seq)
    }

    /// Undelivered `--to-agent` notes for the issue, marked delivered to the actor.
    pub fn take_inbox(&self, id: &str) -> Result<Vec<Value>> {
        let mut st = self.conn.prepare(
            "SELECT e.seq, e.actor, e.at, json_extract(e.payload, '$.body')
             FROM inbox x JOIN event e ON e.seq = x.note_seq
             WHERE x.issue_id = ?1 AND x.delivered_at IS NULL ORDER BY e.seq",
        )?;
        let rows: Vec<Value> = st
            .query_map([id], |r| {
                Ok(
                    json!({"seq": r.get::<_, i64>(0)?, "from": r.get::<_, String>(1)?,
                          "at": r.get::<_, String>(2)?, "body": r.get::<_, Option<String>>(3)?}),
                )
            })?
            .collect::<std::result::Result<_, _>>()?;
        drop(st);
        for r in &rows {
            self.conn.execute(
                "UPDATE inbox SET delivered_at = ?2, delivered_to = ?3 WHERE note_seq = ?1",
                params![r["seq"].as_i64(), self.ts(), self.actor_str()],
            )?;
        }
        Ok(rows)
    }

    // ------------------------------------------------------------------
    // Dependencies
    // ------------------------------------------------------------------

    fn dep_insert(&self, src: &str, kind: &str, dst: &str) -> Result<bool> {
        let k = self.schema.dep_kind(kind)?.clone();
        if src == dst {
            return Err(Error::new(Code::Cycle, "an issue cannot depend on itself"));
        }
        let s = self.issue(src)?;
        let d = self.issue(dst)?;
        if !k.allows(&s.ty, &d.ty) {
            return Err(Error::new(
                Code::InvalidField,
                format!("'{kind}' is not allowed from {} to {}", s.ty, d.ty),
            )
            .with_detail(
                "allowed",
                json!(
                    k.pairs
                        .iter()
                        .map(|(a, b)| format!("{a} → {b}"))
                        .collect::<Vec<_>>()
                ),
            ));
        }
        let exists: bool = self
            .conn
            .query_row(
                "SELECT 1 FROM dep WHERE src = ?1 AND dst = ?2 AND kind = ?3",
                params![src, dst, kind],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if exists {
            return Ok(false);
        }
        if k.acyclic {
            let cyc: bool = self
                .conn
                .query_row(
                    "WITH RECURSIVE reach(id) AS (
                       SELECT ?2
                       UNION
                       SELECT d.dst FROM dep d JOIN reach r ON d.src = r.id WHERE d.kind = ?3
                     )
                     SELECT 1 FROM reach WHERE id = ?1 LIMIT 1",
                    params![src, dst, kind],
                    |_| Ok(true),
                )
                .optional()?
                .unwrap_or(false);
            if cyc {
                return Err(Error::new(
                    Code::Cycle,
                    format!("{src} {kind} {dst} would create a cycle"),
                ));
            }
        }
        self.conn.execute(
            "INSERT INTO dep (src, dst, kind, created_by, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![src, dst, kind, self.actor_str(), self.ts()],
        )?;
        let p = json!({"src": src, "kind": kind, "dst": dst});
        self.event(Some(src), "dep_add", p.clone())?;
        self.event(Some(dst), "dep_add", p)?;
        self.conn.execute(
            "UPDATE issue SET updated_at = ?3 WHERE id IN (?1, ?2)",
            params![src, dst, self.ts()],
        )?;
        Ok(true)
    }

    fn dep_authorize(&self, src: &str, dst: &str) -> Result<()> {
        if self.actor().is_agent() && !self.is_claimed_by_me(src)? && !self.is_claimed_by_me(dst)? {
            return Err(Error::new(
                Code::NotPermitted,
                "agent actors may only add or remove edges touching an issue they claim",
            ));
        }
        Ok(())
    }

    pub fn dep_add(&self, src: &str, kind: &str, dst: &str) -> Result<bool> {
        let src = self.resolve_id(src)?;
        let dst = self.resolve_id(dst)?;
        self.dep_authorize(&src, &dst)?;
        let added = self.dep_insert(&src, kind, &dst)?;
        self.touch_lease_pub(&src)?;
        self.touch_lease_pub(&dst)?;
        Ok(added)
    }

    pub fn dep_rm(&self, src: &str, kind: &str, dst: &str) -> Result<()> {
        let src = self.resolve_id(src)?;
        let dst = self.resolve_id(dst)?;
        self.schema.dep_kind(kind)?;
        self.dep_authorize(&src, &dst)?;
        let n = self.conn.execute(
            "DELETE FROM dep WHERE src = ?1 AND dst = ?2 AND kind = ?3",
            params![src, dst, kind],
        )?;
        if n == 0 {
            return Err(Error::new(
                Code::NotFound,
                format!("no edge {src} {kind} {dst}"),
            ));
        }
        let p = json!({"src": src, "kind": kind, "dst": dst});
        self.event(Some(&src), "dep_rm", p.clone())?;
        self.event(Some(&dst), "dep_rm", p)?;
        self.conn.execute(
            "UPDATE issue SET updated_at = ?3 WHERE id IN (?1, ?2)",
            params![src, dst, self.ts()],
        )?;
        Ok(())
    }
}
