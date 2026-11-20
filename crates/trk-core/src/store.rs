//! `Store`: the only code that writes to the database.
//!
//! Every mutating command is one `BEGIN IMMEDIATE` transaction: load the
//! schema, sweep expired leases, validate, check claims and gates, write rows,
//! append events, commit. Reads use deferred transactions.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use rusqlite_migration::{M, Migrations};
use serde_json::{Value, json};

use crate::config::{Change, Level, check_schema, diff_schemas};
use crate::error::{Code, Error, Result, bad_args, not_found};
use crate::model::*;
use crate::registry::*;

const MIGRATIONS: &[M<'static>] = &[
    M::up(include_str!("schema_v1.sql")),
    M::up(include_str!("schema_v2.sql")),
];

pub const ISSUE_COLS: &str = "i.id, i.project_id, i.type, i.status, i.resolution, i.title, i.context, \
     i.outcome, i.owner, i.links, i.tags, i.fields, i.created_by, i.created_at, i.updated_at, \
     i.closed_at, i.status_category";

#[derive(Debug, Clone)]
pub struct StoreOpts {
    /// Fixed clock for tests (`TRK_NOW`).
    pub now: Option<jiff::Timestamp>,
    /// Lease given to agent claims and renewed on each write.
    pub agent_lease: jiff::SignedDuration,
    /// The repo of the working directory (normalized remote or path), used
    /// for `repo` fields given as `.` and as their default.
    pub current_repo: Option<String>,
}

impl Default for StoreOpts {
    fn default() -> Self {
        StoreOpts {
            now: None,
            agent_lease: jiff::SignedDuration::from_mins(30),
            current_repo: None,
        }
    }
}

pub struct Store {
    conn: Connection,
    pub opts: StoreOpts,
}

/// Per-transaction context: the connection inside the transaction, the
/// schema as loaded at its start, the clock and the acting actor.
pub struct Ctx<'a> {
    pub conn: &'a Connection,
    pub schema: Schema,
    pub now: jiff::Timestamp,
    pub actor: Option<ActorId>,
    pub opts: &'a StoreOpts,
}

impl Store {
    pub fn open(path: &Path, opts: StoreOpts) -> Result<Store> {
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)?;
        }
        let mut conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;
        Migrations::from_slice(MIGRATIONS).to_latest(&mut conn)?;
        Ok(Store { conn, opts })
    }

    pub fn now(&self) -> jiff::Timestamp {
        self.opts.now.unwrap_or_else(jiff::Timestamp::now)
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Run `f` in a deferred (read) transaction.
    pub fn read<T>(&self, actor: Option<&str>, f: impl FnOnce(&Ctx) -> Result<T>) -> Result<T> {
        let tx = self.conn.unchecked_transaction()?;
        let schema = Schema::load(&tx)?;
        let actor = actor.map(ActorId::parse).transpose()?;
        let ctx = Ctx {
            conn: &tx,
            schema,
            now: self.now(),
            actor,
            opts: &self.opts,
        };
        let out = f(&ctx)?;
        drop(ctx);
        tx.commit()?;
        Ok(out)
    }

    /// Run `f` in one `BEGIN IMMEDIATE` transaction. With `dry_run` every
    /// check runs and the transaction is rolled back.
    pub fn write<T>(
        &mut self,
        actor: &str,
        dry_run: bool,
        f: impl FnOnce(&mut Ctx) -> Result<T>,
    ) -> Result<T> {
        let now = self.now();
        let actor_id = ActorId::parse(actor)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let schema = Schema::load(&tx)?;
        let mut ctx = Ctx {
            conn: &tx,
            schema,
            now,
            actor: Some(actor_id),
            opts: &self.opts,
        };
        ctx.ensure_actor(actor, None)?;
        ctx.sweep_leases()?;
        let out = f(&mut ctx)?;
        drop(ctx);
        if dry_run {
            tx.rollback()?;
        } else {
            tx.commit()?;
        }
        Ok(out)
    }

    pub fn schema(&self) -> Result<Schema> {
        Schema::load(&self.conn)
    }

    /// `trk backup`: a consistent copy via `VACUUM INTO`.
    pub fn backup(&self, path: &Path) -> Result<()> {
        if path.exists() {
            return Err(bad_args(format!("{} already exists", path.display())));
        }
        self.conn
            .execute("VACUUM INTO ?1", params![path.to_string_lossy()])?;
        Ok(())
    }

    /// `trk schema index <field>`: an expression index for a hot custom field.
    pub fn index_field(&self, field: &str) -> Result<String> {
        let schema = self.schema()?;
        let def = schema
            .field(field)
            .ok_or_else(|| not_found("field", field))?;
        if def.shared {
            return Err(bad_args(format!(
                "'{field}' is a shared column; it does not live in issue.fields"
            )));
        }
        let name = format!("issue_f_{field}");
        self.conn.execute(
            &format!(
                "CREATE INDEX IF NOT EXISTS {name} ON issue(json_extract(fields, '$.{field}'))"
            ),
            [],
        )?;
        Ok(name)
    }
}

// ----------------------------------------------------------------------
// Helpers on the transaction context
// ----------------------------------------------------------------------

impl<'a> Ctx<'a> {
    pub fn ts(&self) -> String {
        fmt_ts(self.now)
    }

    pub fn actor(&self) -> &ActorId {
        self.actor.as_ref().expect("write context has an actor")
    }

    pub fn actor_str(&self) -> String {
        self.actor().to_string()
    }

    pub fn event(&self, issue: Option<&str>, op: &str, payload: Value) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO event (issue_id, actor, at, op, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![issue, self.actor_str(), self.ts(), op, payload.to_string()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Insert the actor row if missing. Agent actors need a registered harness.
    pub fn ensure_actor(&self, id: &str, on_behalf_of: Option<&str>) -> Result<()> {
        let parsed = ActorId::parse(id)?;
        let exists: bool = self
            .conn
            .query_row("SELECT 1 FROM actor WHERE id = ?1", [id], |_| Ok(true))
            .optional()?
            .unwrap_or(false);
        if let Some(obo) = on_behalf_of {
            self.ensure_actor(obo, None)?;
        }
        if exists {
            if let Some(obo) = on_behalf_of {
                self.conn.execute(
                    "UPDATE actor SET on_behalf_of = ?2 WHERE id = ?1 AND on_behalf_of IS NULL",
                    params![id, obo],
                )?;
            }
            return Ok(());
        }
        let (kind, harness) = match &parsed {
            ActorId::Human { .. } => ("human", None),
            ActorId::Agent { harness, .. } => {
                let known: bool = self
                    .conn
                    .query_row("SELECT 1 FROM harness WHERE name = ?1", [harness], |_| {
                        Ok(true)
                    })
                    .optional()?
                    .unwrap_or(false);
                if !known {
                    return Err(Error::new(Code::NotFound, format!("unknown harness '{harness}'"))
                        .with_hint("harnesses are registered from [harness.*] profiles in config.toml by `trk schema apply`"));
                }
                ("agent", Some(harness.clone()))
            }
        };
        self.conn.execute(
            "INSERT INTO actor (id, kind, harness, on_behalf_of, first_seen) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, kind, harness, on_behalf_of, self.ts()],
        )?;
        Ok(())
    }

    /// Release claims whose lease has expired, returning their issues to open.
    pub fn sweep_leases(&self) -> Result<usize> {
        let mut st = self.conn.prepare(
            "UPDATE claim SET released_at = ?1, release_reason = 'expired'
             WHERE released_at IS NULL AND lease_until < ?1
             RETURNING id, issue_id, actor, lease_until",
        )?;
        let rows: Vec<(i64, String, String, String)> = st
            .query_map([self.ts()], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<std::result::Result<_, _>>()?;
        drop(st);
        for (claim_id, issue_id, claimant, lease_until) in &rows {
            let (ty, status): (String, String) = self.conn.query_row(
                "SELECT type, status FROM issue WHERE id = ?1",
                [issue_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let open = self
                .schema
                .ty(&ty)?
                .open_status()
                .map(|s| s.name.clone())
                .unwrap_or_else(|| "open".into());
            self.conn.execute(
                "UPDATE issue SET status = ?2, updated_at = ?3 WHERE id = ?1",
                params![issue_id, open, self.ts()],
            )?;
            self.event(
                Some(issue_id),
                "release",
                json!({"claim": claim_id, "claimant": claimant, "reason": "expired",
                       "lease_until": lease_until, "from": status, "to": open}),
            )?;
        }
        Ok(rows.len())
    }

    // ------------------------------------------------------------------
    // Lookups
    // ------------------------------------------------------------------

    /// Resolve a full id, or a unique prefix of an id or of its suffix.
    pub fn resolve_id(&self, s: &str) -> Result<String> {
        let s = s.trim();
        if s.is_empty() {
            return Err(bad_args("empty issue id"));
        }
        if let Some(id) = self
            .conn
            .query_row("SELECT id FROM issue WHERE id = ?1", [s], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
        {
            return Ok(id);
        }
        let like = format!("{}%", s.replace('%', "").replace('_', "\\_"));
        let mut st = self.conn.prepare(
            "SELECT id FROM issue
             WHERE id LIKE ?1 ESCAPE '\\'
                OR substr(id, instr(id, '-') + 1) LIKE ?1 ESCAPE '\\'
             ORDER BY id LIMIT 11",
        )?;
        let ids: Vec<String> = st
            .query_map([&like], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        match ids.len() {
            0 => Err(not_found("issue", s)),
            1 => Ok(ids.into_iter().next().unwrap()),
            _ => Err(
                Error::new(Code::AmbiguousId, format!("'{s}' matches several issues"))
                    .with_detail("candidates", json!(ids))
                    .with_hint("use more characters of the id"),
            ),
        }
    }

    pub fn issue(&self, id: &str) -> Result<Issue> {
        self.conn
            .query_row(
                &format!("SELECT {ISSUE_COLS} FROM issue_view i WHERE i.id = ?1"),
                [id],
                row_to_issue,
            )
            .optional()?
            .ok_or_else(|| not_found("issue", id))
    }

    pub fn active_claim(&self, issue_id: &str) -> Result<Option<Claim>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {CLAIM_COLS} FROM claim c LEFT JOIN actor a ON a.id = c.actor WHERE c.issue_id = ?1 AND c.released_at IS NULL"),
                [issue_id],
                row_to_claim,
            )
            .optional()?)
    }

    pub fn claims(&self, issue_id: &str) -> Result<Vec<Claim>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {CLAIM_COLS} FROM claim c LEFT JOIN actor a ON a.id = c.actor WHERE c.issue_id = ?1 ORDER BY c.id"
        ))?;
        let v = st
            .query_map([issue_id], row_to_claim)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(v)
    }

    pub fn edges(&self, issue_id: &str, outgoing: bool) -> Result<Vec<Edge>> {
        let sql = if outgoing {
            "SELECT d.kind, o.id, o.type, o.status, o.status_category, o.resolution, o.title
             FROM dep d JOIN issue_view o ON o.id = d.dst WHERE d.src = ?1 ORDER BY d.kind, o.id"
        } else {
            "SELECT d.kind, o.id, o.type, o.status, o.status_category, o.resolution, o.title
             FROM dep d JOIN issue_view o ON o.id = d.src WHERE d.dst = ?1 ORDER BY d.kind, o.id"
        };
        let mut st = self.conn.prepare(sql)?;
        let v = st
            .query_map([issue_id], |r| {
                Ok(Edge {
                    kind: r.get(0)?,
                    id: r.get(1)?,
                    ty: r.get(2)?,
                    status: r.get(3)?,
                    status_category: r.get(4)?,
                    resolution: r.get(5)?,
                    title: r.get(6)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(v)
    }

    pub fn events(&self, issue_id: &str) -> Result<Vec<Event>> {
        let mut st = self.conn.prepare(
            "SELECT seq, issue_id, actor, at, op, payload FROM event WHERE issue_id = ?1 ORDER BY seq",
        )?;
        let v = st
            .query_map([issue_id], row_to_event)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(v)
    }

    pub fn project_exists(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM project WHERE id = ?1", [id], |_| Ok(true))
            .optional()?
            .unwrap_or(false))
    }

    /// Find the repo row for a normalized remote URL or an absolute path.
    pub fn find_repo(&self, remote: Option<&str>, path: Option<&str>) -> Result<Option<Repo>> {
        if let Some(r) = remote {
            let n = normalize_remote(r);
            if let Some(repo) = self
                .conn
                .query_row(
                    "SELECT id, project_id, remote_url, local_path FROM repo WHERE remote_url = ?1",
                    [&n],
                    row_to_repo,
                )
                .optional()?
            {
                return Ok(Some(repo));
            }
        }
        if let Some(p) = path {
            return Ok(self
                .conn
                .query_row(
                    "SELECT id, project_id, remote_url, local_path FROM repo WHERE local_path = ?1",
                    [p],
                    row_to_repo,
                )
                .optional()?);
        }
        Ok(None)
    }

    /// The repo row a stored `repo` field value refers to.
    pub fn repo_by_ref(&self, r: &str) -> Result<Option<Repo>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, project_id, remote_url, local_path FROM repo WHERE remote_url = ?1 OR local_path = ?1",
                [r],
                row_to_repo,
            )
            .optional()?)
    }
}

pub const CLAIM_COLS: &str = "c.id, c.issue_id, c.actor, a.on_behalf_of, c.claimed_at, c.lease_until, c.worktree, \
     c.host, c.agent, c.terminal, c.released_at, c.release_reason";

pub fn row_to_claim(r: &rusqlite::Row) -> rusqlite::Result<Claim> {
    Ok(Claim {
        id: r.get(0)?,
        issue_id: r.get(1)?,
        actor: r.get(2)?,
        on_behalf_of: r.get(3)?,
        claimed_at: r.get(4)?,
        lease_until: r.get(5)?,
        worktree: r.get(6)?,
        host: r.get(7)?,
        agent: r.get(8)?,
        terminal: r.get(9)?,
        released_at: r.get(10)?,
        release_reason: r.get(11)?,
    })
}

fn row_to_repo(r: &rusqlite::Row) -> rusqlite::Result<Repo> {
    Ok(Repo {
        id: r.get(0)?,
        project: r.get(1)?,
        remote_url: r.get(2)?,
        local_path: r.get(3)?,
    })
}

fn row_to_event(r: &rusqlite::Row) -> rusqlite::Result<Event> {
    let payload: String = r.get(5)?;
    Ok(Event {
        seq: r.get(0)?,
        issue_id: r.get(1)?,
        actor: r.get(2)?,
        at: r.get(3)?,
        op: r.get(4)?,
        payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
    })
}

pub fn row_to_issue(r: &rusqlite::Row) -> rusqlite::Result<Issue> {
    let links: String = r.get(9)?;
    let tags: String = r.get(10)?;
    let fields: String = r.get(11)?;
    Ok(Issue {
        id: r.get(0)?,
        project: r.get(1)?,
        ty: r.get(2)?,
        status: r.get(3)?,
        resolution: r.get(4)?,
        title: r.get(5)?,
        context: r.get(6)?,
        outcome: r.get(7)?,
        owner: r.get(8)?,
        links: serde_json::from_str(&links).unwrap_or_default(),
        tags: serde_json::from_str::<Vec<Value>>(&tags)
            .unwrap_or_default()
            .into_iter()
            .map(|v| {
                v.as_str()
                    .map(String::from)
                    .unwrap_or_else(|| v.to_string())
            })
            .collect(),
        fields: serde_json::from_str(&fields).unwrap_or_default(),
        created_by: r.get(12)?,
        created_at: r.get(13)?,
        updated_at: r.get(14)?,
        closed_at: r.get(15)?,
        status_category: r.get(16)?,
    })
}

// ----------------------------------------------------------------------
// Coercion context
// ----------------------------------------------------------------------

impl crate::fields::CoerceCtx for Ctx<'_> {
    fn me(&self) -> String {
        self.actor
            .as_ref()
            .map(|a| a.to_string())
            .unwrap_or_default()
    }
    fn now_rfc3339(&self) -> String {
        self.ts()
    }
    fn resolve_repo(&self, raw: &str) -> Result<String> {
        let raw = if raw == "." {
            self.opts.current_repo.clone().ok_or_else(|| {
                Error::new(
                    Code::UnknownRepo,
                    "'.' used but the current directory is not a registered repo",
                )
                .with_hint("run `trk repo add` in the repository")
            })?
        } else {
            raw.to_string()
        };
        if let Some(r) = self.repo_by_ref(&raw)? {
            return Ok(r.remote_url.or(r.local_path).unwrap_or(raw));
        }
        let n = normalize_remote(&raw);
        if let Some(r) = self.repo_by_ref(&n)? {
            return Ok(r.remote_url.or(r.local_path).unwrap_or(n));
        }
        Err(
            Error::new(Code::UnknownRepo, format!("unknown repo '{raw}'"))
                .with_hint("register it with `trk repo add` (see `trk repo list`)"),
        )
    }
}

impl crate::query::QueryCtx for Ctx<'_> {
    fn me(&self) -> Option<String> {
        self.actor.as_ref().map(|a| a.to_string())
    }
    fn current_project(&self) -> Option<String> {
        let r = self.opts.current_repo.as_ref()?;
        self.repo_by_ref(r).ok().flatten().map(|r| r.project)
    }
    fn now(&self) -> jiff::Timestamp {
        self.now
    }
    fn resolve_id(&self, s: &str) -> Result<String> {
        Ctx::resolve_id(self, s)
    }
}

// ----------------------------------------------------------------------
// Projects, repos, actors
// ----------------------------------------------------------------------

impl Ctx<'_> {
    pub fn project_add(&self, id: &str, name: &str) -> Result<Project> {
        if id.is_empty()
            || !id.starts_with(|c: char| c.is_ascii_lowercase())
            || !id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            return Err(bad_args(format!(
                "invalid project id '{id}': use lowercase letters and digits, starting with a letter"
            )));
        }
        if self.project_exists(id)? {
            return Err(Error::new(
                Code::BadArgs,
                format!("project '{id}' already exists"),
            ));
        }
        let p = Project {
            id: id.into(),
            name: name.into(),
            created_at: self.ts(),
        };
        self.conn.execute(
            "INSERT INTO project (id, name, created_at) VALUES (?1, ?2, ?3)",
            params![p.id, p.name, p.created_at],
        )?;
        Ok(p)
    }

    pub fn projects(&self) -> Result<Vec<Project>> {
        let mut st = self
            .conn
            .prepare("SELECT id, name, created_at FROM project ORDER BY id")?;
        let v = st
            .query_map([], |r| {
                Ok(Project {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    created_at: r.get(2)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(v)
    }

    pub fn repo_add(
        &self,
        project: &str,
        remote: Option<&str>,
        path: Option<&str>,
    ) -> Result<Repo> {
        if !self.project_exists(project)? {
            return Err(not_found("project", project).with_hint("create it with `trk project add`"));
        }
        if remote.is_none() && path.is_none() {
            return Err(bad_args("a repo needs a remote URL or a local path"));
        }
        let remote = remote.map(normalize_remote);
        if let Some(existing) = self.find_repo(remote.as_deref(), path)? {
            if existing.project != project {
                return Err(Error::new(
                    Code::BadArgs,
                    format!("repo already belongs to project '{}'", existing.project),
                ));
            }
            // Fill in whichever side was missing.
            if existing.local_path.is_none() && path.is_some() {
                self.conn.execute(
                    "UPDATE repo SET local_path = ?2 WHERE id = ?1",
                    params![existing.id, path],
                )?;
            }
            if existing.remote_url.is_none() && remote.is_some() {
                self.conn.execute(
                    "UPDATE repo SET remote_url = ?2 WHERE id = ?1",
                    params![existing.id, remote],
                )?;
            }
            return Ok(self.conn.query_row(
                "SELECT id, project_id, remote_url, local_path FROM repo WHERE id = ?1",
                [existing.id],
                row_to_repo,
            )?);
        }
        self.conn.execute(
            "INSERT INTO repo (project_id, remote_url, local_path) VALUES (?1, ?2, ?3)",
            params![project, remote, path],
        )?;
        let id = self.conn.last_insert_rowid();
        Ok(Repo {
            id,
            project: project.into(),
            remote_url: remote,
            local_path: path.map(String::from),
        })
    }

    pub fn repos(&self) -> Result<Vec<Repo>> {
        let mut st = self.conn.prepare(
            "SELECT id, project_id, remote_url, local_path FROM repo ORDER BY project_id, id",
        )?;
        let v = st
            .query_map([], row_to_repo)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(v)
    }

    pub fn actor_meta(&self, id: &str) -> Result<Value> {
        let m: Option<String> = self
            .conn
            .query_row("SELECT meta FROM actor WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        Ok(m.map(|m| serde_json::from_str(&m).unwrap_or(json!({})))
            .unwrap_or(json!({})))
    }

    /// Merge keys into `actor.meta` (registry data, not history).
    pub fn patch_actor_meta(&self, id: &str, patch: Value) -> Result<()> {
        self.conn.execute(
            "UPDATE actor SET meta = json_patch(meta, ?2) WHERE id = ?1",
            params![id, patch.to_string()],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Schema apply
    // ------------------------------------------------------------------

    /// Plan: what applying `new` would change, and which issues it affects.
    pub fn plan(&self, new: &Schema, map_status: &[(String, String)]) -> Result<Plan> {
        let changes = diff_schemas(&self.schema, new);
        let mut affected = vec![];
        let mut blockers = vec![];
        let mut st = self.conn.prepare(&format!(
            "SELECT {ISSUE_COLS} FROM issue_view i ORDER BY i.id"
        ))?;
        let issues: Vec<Issue> = st
            .query_map([], row_to_issue)?
            .collect::<std::result::Result<_, _>>()?;
        drop(st);
        for issue in &issues {
            let Some(nt) = new.types.get(&issue.ty) else {
                blockers.push(format!(
                    "type '{}' is removed but issue {} uses it",
                    issue.ty, issue.id
                ));
                continue;
            };
            let status = match nt.status(&issue.status) {
                Some(_) => issue.status.clone(),
                None => {
                    match map_status.iter().find(|(o, _)| o == &issue.status) {
                        Some((_, n)) => match (
                            nt.status(n),
                            self.schema
                                .ty(&issue.ty)
                                .ok()
                                .and_then(|t| t.category_of(&issue.status)),
                        ) {
                            (Some(ns), Some(oc)) if ns.category == oc => {
                                affected.push(json!({"id": issue.id, "reason": format!("status {} → {}", issue.status, n)}));
                                n.clone()
                            }
                            (Some(ns), oc) => {
                                blockers.push(format!(
                                "--map-status {}={} changes the category of issue {} ({:?} → {})",
                                issue.status, n, issue.id, oc.map(|c| c.as_str()), ns.category.as_str()
                            ));
                                continue;
                            }
                            (None, _) => {
                                blockers.push(format!(
                                    "--map-status target '{n}' is not a status of type '{}'",
                                    issue.ty
                                ));
                                continue;
                            }
                        },
                        None => {
                            blockers.push(format!(
                            "status '{}' of type '{}' is removed but issue {} is in it; pass --map-status {}=<new>",
                            issue.status, issue.ty, issue.id, issue.status
                        ));
                            continue;
                        }
                    }
                }
            };
            if let Some(r) = &issue.resolution
                && !nt.resolutions.contains(r)
            {
                blockers.push(format!(
                    "resolution '{r}' of type '{}' is removed but issue {} uses it",
                    issue.ty, issue.id
                ));
            }
            // Requirements of the current status that the issue would now violate.
            let old_unmet = self
                .schema
                .types
                .get(&issue.ty)
                .map(|t| self.unmet_fields(issue, t, &issue.status, issue.resolution.as_deref()))
                .unwrap_or_default();
            let new_unmet = self.unmet_fields(issue, nt, &status, issue.resolution.as_deref());
            let added: Vec<_> = new_unmet
                .iter()
                .filter(|f| !old_unmet.contains(f))
                .cloned()
                .collect();
            if !added.is_empty() {
                affected.push(json!({
                    "id": issue.id,
                    "reason": format!("violates new requirements for '{}': {}", status, added.join(", ")),
                }));
            }
        }
        for k in self.schema.dep_kinds.keys() {
            if !new.dep_kinds.contains_key(k) {
                let n: i64 =
                    self.conn
                        .query_row("SELECT count(*) FROM dep WHERE kind = ?1", [k], |r| {
                            r.get(0)
                        })?;
                if n > 0 {
                    blockers.push(format!(
                        "dependency kind '{k}' is removed but {n} edges use it"
                    ));
                }
            }
        }
        let diags = check_schema(new);
        for d in &diags {
            if d.level == Level::Error {
                blockers.push(format!(
                    "{}{}",
                    d.file
                        .as_ref()
                        .map(|f| format!("{f}: "))
                        .unwrap_or_default(),
                    d.message
                ));
            }
        }
        Ok(Plan {
            changes,
            affected,
            blockers,
        })
    }

    /// Apply `new` to the registry tables in this transaction.
    pub fn apply(
        &self,
        new: &Schema,
        harnesses: &[String],
        map_status: &[(String, String)],
        commit: Option<String>,
    ) -> Result<Plan> {
        let plan = self.plan(new, map_status)?;
        if !plan.blockers.is_empty() {
            return Err(
                Error::new(Code::SchemaInvalid, "the configuration cannot be applied")
                    .with_detail("problems", json!(plan.blockers)),
            );
        }
        self.conn.pragma_update(None, "defer_foreign_keys", "ON")?;
        for (old, newst) in map_status {
            let mut st = self
                .conn
                .prepare("SELECT id, type FROM issue WHERE status = ?1")?;
            let rows: Vec<(String, String)> = st
                .query_map([old], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<std::result::Result<_, _>>()?;
            drop(st);
            for (id, ty) in rows {
                if new
                    .types
                    .get(&ty)
                    .is_some_and(|t| t.status(old).is_none() && t.status(newst).is_some())
                {
                    self.conn.execute(
                        "UPDATE issue SET status = ?2, updated_at = ?3 WHERE id = ?1",
                        params![id, newst, self.ts()],
                    )?;
                    self.event(
                        Some(&id),
                        "move",
                        json!({"from": old, "to": newst, "schema": true}),
                    )?;
                }
            }
        }
        new.write(self.conn)?;
        for h in harnesses {
            self.conn
                .execute("INSERT OR IGNORE INTO harness (name) VALUES (?1)", [h])?;
        }
        self.event(
            None,
            "schema",
            json!({
                "commit": commit,
                "changes": plan.changes,
                "map_status": map_status.iter().map(|(a, b)| format!("{a}={b}")).collect::<Vec<_>>(),
            }),
        )?;
        Ok(plan)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Plan {
    pub changes: Vec<Change>,
    /// Existing issues the new rules affect.
    pub affected: Vec<Value>,
    /// Problems that prevent applying.
    pub blockers: Vec<String>,
}
