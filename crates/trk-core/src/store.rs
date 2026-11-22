//! `Store`: the only code that writes to the database.
//!
//! Every mutating command is one `BEGIN IMMEDIATE` transaction: sweep expired
//! leases, validate, check claims, write rows, append events, commit. Reads
//! use deferred transactions.

use std::io::Write;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use rusqlite_migration::{HookError, M, MigrationDefinitionError, Migrations};
use serde_json::{Value, json};

use crate::error::{Code, Error, Result, bad_args, not_found};
use crate::model::*;

pub const ISSUE_COLS: &str = "i.id, i.project_id, i.repo_id, i.state, i.title, i.description, \
     i.priority, i.type, i.close_reason, i.waiting_reason, i.waiting_check, i.check_interval, \
     i.next_check_at, i.last_check_at, i.last_check_exit, i.wait_cwd, i.created_by, i.created_at, \
     i.updated_at, i.closed_at";

#[derive(Debug, Clone)]
pub struct StoreOpts {
    /// Fixed clock for tests (`TRK_NOW`).
    pub now: Option<jiff::Timestamp>,
    /// Lease given to agent claims and renewed on each write.
    pub agent_lease: jiff::SignedDuration,
    /// The repo of the working directory (normalized remote or path): the
    /// default repo of new issues and the meaning of `project=.`.
    pub current_repo: Option<String>,
    /// Time between checks of a waiting issue that does not set its own.
    pub check_interval: jiff::SignedDuration,
    /// Global config directory used only to preserve routing when migrating
    /// databases with authored agent assignments.
    pub migration_config: Option<PathBuf>,
}

impl Default for StoreOpts {
    fn default() -> Self {
        StoreOpts {
            now: None,
            agent_lease: jiff::SignedDuration::from_mins(30),
            current_repo: None,
            check_interval: jiff::SignedDuration::from_mins(5),
            migration_config: None,
        }
    }
}

pub struct Store {
    conn: Connection,
    pub opts: StoreOpts,
}

/// Per-transaction context: the connection inside the transaction, the
/// clock and the acting actor.
pub struct Ctx<'a> {
    pub conn: &'a Connection,
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
        // A database written by the six-type schema cannot be opened: there is
        // no migration, only an export and reimport.
        let old: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'issue_type'",
                [],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if old {
            return Err(Error::new(
                Code::OldDatabase,
                format!(
                    "{} was written by an older trk with issue types and cannot be opened",
                    path.display()
                ),
            )
            .with_hint(
                "move it aside (it stays readable with the old binary), then reimport its \
                 open issues with scripts/import-v1.sh",
            ));
        }
        let config_dir = opts.migration_config.clone();
        let migrations = [
            M::up(include_str!("schema.sql")),
            M::up_with_hook(
                include_str!("schema-v2.sql"),
                move |tx: &rusqlite::Transaction| {
                    migrate_issue_types(tx, config_dir.as_deref())
                        .map_err(|e| HookError::Hook(e.to_string()))
                },
            ),
        ];
        migrate(&mut conn, path, &migrations)?;
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
        let actor = actor.map(ActorId::parse).transpose()?;
        let ctx = Ctx {
            conn: &tx,
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
        let mut ctx = Ctx {
            conn: &tx,
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

    /// `trk backup`: a consistent copy via `VACUUM INTO`.
    pub fn backup(&self, path: &Path) -> Result<()> {
        if path.exists() {
            return Err(bad_args(format!("{} already exists", path.display())));
        }
        self.conn
            .execute("VACUUM INTO ?1", params![path.to_string_lossy()])?;
        Ok(())
    }
}

fn migrate(conn: &mut Connection, path: &Path, migrations: &[M<'_>]) -> Result<()> {
    match Migrations::from_slice(migrations).to_latest(conn) {
        Err(rusqlite_migration::Error::MigrationDefinition(
            MigrationDefinitionError::DatabaseTooFarAhead,
        )) => {
            let database_version: u32 =
                conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
            Err(Error::new(
                Code::NewerDatabase,
                format!(
                    "{} has SQLite migration version {database_version}, but this trk supports \
                     only up to {}; the database was opened by a newer trk",
                    path.display(),
                    migrations.len(),
                ),
            )
            .with_detail("database", path.to_string_lossy().as_ref())
            .with_detail("database_version", database_version)
            .with_detail("supported_version", migrations.len())
            .with_hint(
                "upgrade trk, then restart the harness's trk MCP server using the upgraded \
                 executable and the same database/config, issue and actor; reconnect and call \
                 get_task before resuming. Replacing the executable does not upgrade a running \
                 server. If the claim is gone or the issue is blocked, preserve the handoff for \
                 the human; do not bypass claim/dependency rules. Do not downgrade or reset \
                 the database.",
            ))
        }
        result => result.map_err(Into::into),
    }
}

/// Files are published before removing any old routing data. A failed database
/// transaction may leave matching files behind; reopening safely reuses them.
fn migrate_issue_types(conn: &Connection, config_dir: Option<&Path>) -> Result<()> {
    let has_issues: bool =
        conn.query_row("SELECT EXISTS(SELECT 1 FROM issue)", [], |r| r.get(0))?;
    if has_issues {
        let config_dir = config_dir.ok_or_else(|| {
            Error::new(
                Code::ConfigInvalid,
                "migrating issue agents requires the global config directory (StoreOpts.migration_config)",
            )
        })?;
        let default_agent = migration_default_agent(config_dir)?;
        let mut stmt = conn.prepare("SELECT DISTINCT coalesce(agent, ?1) FROM issue")?;
        let agents = stmt
            .query_map([&default_agent], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);
        let types_dir = config_dir.join("types");
        std::fs::create_dir_all(&types_dir)?;
        for agent in agents {
            publish_migrated_type(&types_dir, &agent)?;
        }
        conn.execute(
            "UPDATE issue SET type = 'migrated-agent-' || lower(hex(coalesce(agent, ?1)))",
            [&default_agent],
        )?;
    }
    conn.execute_batch("ALTER TABLE issue DROP COLUMN agent;")?;
    Ok(())
}

fn migration_default_agent(config_dir: &Path) -> Result<String> {
    let path = config_dir.join("config.toml");
    let source = match std::fs::read_to_string(&path) {
        Ok(source) => source,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok("implementer".into()),
        Err(e) => return Err(e.into()),
    };
    let config: toml::Value = toml::from_str(&source)
        .map_err(|e| Error::new(Code::ConfigInvalid, format!("{}: {e}", path.display())))?;
    match config.get("run").and_then(|run| run.get("default_agent")) {
        None => Ok("implementer".into()),
        Some(toml::Value::String(agent)) => Ok(agent.clone()),
        Some(_) => Err(Error::new(
            Code::ConfigInvalid,
            format!("{}: run.default_agent must be a string", path.display()),
        )),
    }
}

fn publish_migrated_type(types_dir: &Path, agent: &str) -> Result<()> {
    use std::fmt::Write as _;

    let mut name = String::with_capacity("migrated-agent-".len() + agent.len() * 2);
    name.push_str("migrated-agent-");
    for byte in agent.bytes() {
        write!(&mut name, "{byte:02x}").expect("writing a string cannot fail");
    }
    let definition = toml::Value::Table(toml::map::Map::from_iter([
        ("name".into(), toml::Value::String(name.clone())),
        (
            "description".into(),
            toml::Value::String(format!(
                "Migrated issue agent assignment; routes to agent {agent:?}."
            )),
        ),
        ("agent".into(), toml::Value::String(agent.into())),
    ]));
    let path = types_dir.join(format!("{name}.toml"));
    let matches_existing = || -> Result<()> {
        let existing = std::fs::read_to_string(&path)?;
        if toml::from_str::<toml::Value>(&existing).ok().as_ref() != Some(&definition) {
            return Err(Error::new(
                Code::ConfigInvalid,
                format!(
                    "{} conflicts with the migrated type for agent {agent:?}; preserve or move it before retrying migration",
                    path.display()
                ),
            ));
        }
        Ok(())
    };
    if path.try_exists()? {
        return matches_existing();
    }
    let content = toml::to_string(&definition)
        .map_err(|e| Error::new(Code::Internal, format!("serializing migrated type: {e}")))?;
    let temporary = types_dir.join(format!(".{name}-{:016x}.tmp", rand::random::<u64>()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| -> Result<()> {
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        // A hard link publishes complete contents atomically without replacing
        // an existing definition, including one published by another process.
        match std::fs::hard_link(&temporary, &path) {
            Ok(()) => {
                std::fs::File::open(types_dir)?.sync_all()?;
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => matches_existing(),
            Err(e) => Err(e.into()),
        }
    })();
    drop(file);
    std::fs::remove_file(temporary)?;
    result
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

    /// Insert the actor row if missing.
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
                // Harnesses are registered the first time one acts.
                self.conn.execute(
                    "INSERT OR IGNORE INTO harness (name) VALUES (?1)",
                    [harness],
                )?;
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
            self.conn.execute(
                "UPDATE issue SET state = 'open', updated_at = ?2 WHERE id = ?1",
                params![issue_id, self.ts()],
            )?;
            self.event(
                Some(issue_id),
                "release",
                json!({"claim": claim_id, "claimant": claimant, "reason": "expired",
                       "lease_until": lease_until, "from": "claimed", "to": "open"}),
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
                &format!("SELECT {ISSUE_COLS} FROM issue i WHERE i.id = ?1"),
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

    /// The issues this one blocks (`outgoing`) or is blocked by.
    pub fn edges(&self, issue_id: &str, outgoing: bool) -> Result<Vec<Edge>> {
        let sql = if outgoing {
            "SELECT o.id, o.title, o.state, o.priority
             FROM dep d JOIN issue o ON o.id = d.dst WHERE d.src = ?1 ORDER BY o.id"
        } else {
            "SELECT o.id, o.title, o.state, o.priority
             FROM dep d JOIN issue o ON o.id = d.src WHERE d.dst = ?1 ORDER BY o.id"
        };
        let mut st = self.conn.prepare(sql)?;
        let v = st
            .query_map([issue_id], |r| {
                Ok(Edge {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    state: state_col(r, 2)?,
                    priority: r.get(3)?,
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

    pub fn repo_by_id(&self, id: i64) -> Result<Option<Repo>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, project_id, remote_url, local_path FROM repo WHERE id = ?1",
                [id],
                row_to_repo,
            )
            .optional()?)
    }

    /// The repo row a normalized remote URL or an absolute path refers to.
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

fn state_col(r: &rusqlite::Row, idx: usize) -> rusqlite::Result<State> {
    let s: String = r.get(idx)?;
    State::parse(&s).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            idx,
            rusqlite::types::Type::Text,
            format!("unknown state '{s}'").into(),
        )
    })
}

pub fn row_to_issue(r: &rusqlite::Row) -> rusqlite::Result<Issue> {
    Ok(Issue {
        id: r.get(0)?,
        project: r.get(1)?,
        repo_id: r.get(2)?,
        state: state_col(r, 3)?,
        title: r.get(4)?,
        description: r.get(5)?,
        priority: r.get(6)?,
        issue_type: r.get(7)?,
        close_reason: r.get(8)?,
        waiting_reason: r.get(9)?,
        waiting_check: r.get(10)?,
        check_interval: r.get(11)?,
        next_check_at: r.get(12)?,
        last_check_at: r.get(13)?,
        last_check_exit: r.get(14)?,
        wait_cwd: r.get(15)?,
        created_by: r.get(16)?,
        created_at: r.get(17)?,
        updated_at: r.get(18)?,
        closed_at: r.get(19)?,
    })
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{ClaimOpts, NewIssue};
    use crate::view::With;

    #[test]
    fn older_migrations_refuse_newer_database_without_losing_handoff() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trk.db");
        let actor = "agent:omp:upgrade";
        let opts = StoreOpts {
            now: Some("2026-11-20T10:00:00Z".parse().unwrap()),
            ..Default::default()
        };
        let mut store = Store::open(&path, opts.clone()).unwrap();
        let id = store
            .write("human:alice@example.com", false, |c| {
                c.project_add("orch", "Orchestra")?;
                c.create(NewIssue {
                    project: "orch".into(),
                    title: "Preserve this issue".into(),
                    ..Default::default()
                })
            })
            .unwrap();
        store
            .write(actor, false, |c| c.claim(&id, ClaimOpts::default()))
            .unwrap();
        let before = store.read(None, |c| c.show(&id, With::default())).unwrap();
        let schema = |conn: &Connection| {
            conn.prepare("SELECT type, name, sql FROM sqlite_schema ORDER BY type, name")
                .unwrap()
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        let before_schema = schema(store.conn());
        drop(store);

        // A still-running executable retains only the first migration after
        // another executable has advanced this same database to version 2.
        let mut stale = Connection::open(&path).unwrap();
        let error = migrate(&mut stale, &path, &[M::up(include_str!("schema.sql"))]).unwrap_err();
        assert_eq!(error.code(), Code::NewerDatabase);
        assert_eq!(error.to_json()["database_version"], 2);
        assert_eq!(error.to_json()["supported_version"], 1);
        assert_eq!(schema(&stale), before_schema);
        assert_eq!(
            stale
                .pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            2
        );
        drop(stale);

        // Reopen with the compatible migration set, not a downgraded DB.
        let mut recovered = Store::open(&path, opts).unwrap();
        assert_eq!(
            recovered
                .read(None, |c| c.show(&id, With::default()))
                .unwrap(),
            before
        );
        recovered
            .write(actor, false, |c| {
                c.unclaim(&id, false, Some("Recovered handoff"))
            })
            .unwrap();
        assert_eq!(
            recovered.read(None, |c| c.issue(&id)).unwrap().state,
            State::Open
        );
        assert!(
            recovered
                .read(None, |c| c.notes(&id))
                .unwrap()
                .iter()
                .any(|n| n["body"] == "Recovered handoff")
        );
    }
}
