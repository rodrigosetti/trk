//! `Store`: the only code that writes to the database.
//!
//! Every mutating command is one `BEGIN IMMEDIATE` transaction: sweep expired
//! leases, validate, check claims, write rows, append events, commit. Reads
//! use deferred transactions.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use rusqlite_migration::{M, MigrationDefinitionError, Migrations};
use serde_json::{Value, json};

use crate::error::{Code, Error, Result, bad_args, not_found};
use crate::model::*;

pub const ISSUE_COLS: &str = "i.id, i.state, i.title, i.description, \
     i.priority, i.type, i.close_reason, i.waiting_reason, i.waiting_check, i.check_interval, \
     i.next_check_at, i.last_check_at, i.last_check_exit, i.wait_cwd, i.created_by, i.created_at, \
     i.updated_at, i.closed_at, i.wake_count";

#[derive(Debug, Clone)]
pub struct StoreOpts {
    /// Fixed clock for tests (`TRK_NOW`).
    pub now: Option<jiff::Timestamp>,
    /// Lease given to agent claims and renewed on each write.
    pub agent_lease: jiff::SignedDuration,
    /// Time between checks of a waiting issue that does not set its own.
    pub check_interval: jiff::SignedDuration,
    /// Maximum failed-check wakes per session; zero disables waking.
    pub max_wakes: u32,
    /// Prefix of new issue ids: `<prefix>-<base32>`.
    pub prefix: String,
}

impl Default for StoreOpts {
    fn default() -> Self {
        StoreOpts {
            now: None,
            agent_lease: jiff::SignedDuration::from_mins(30),
            check_interval: jiff::SignedDuration::from_mins(5),
            max_wakes: 5,
            prefix: "trk".into(),
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
    /// Open the project's database. It must exist: `trk install` creates it.
    pub fn open(path: &Path, opts: StoreOpts) -> Result<Store> {
        if !path.is_file() {
            return Err(Error::new(
                Code::NotInstalled,
                format!("no trk database at {}", path.display()),
            )
            .with_hint("run `trk install` in the project's root directory"));
        }
        Self::connect(path, opts)
    }

    /// `trk install`: create the database if it is missing, then open it.
    pub fn install(path: &Path, opts: StoreOpts) -> Result<Store> {
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)?;
        }
        Self::connect(path, opts)
    }

    fn connect(path: &Path, opts: StoreOpts) -> Result<Store> {
        check_prefix(&opts.prefix)?;
        let mut conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;
        migrate(&mut conn, path, &migrations())?;
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
                 executable and the same project directory, issue and actor; reconnect and call \
                 get_task before resuming. Replacing the executable does not upgrade a running \
                 server. If the claim is gone or the issue is blocked, preserve the handoff for \
                 the human; do not bypass claim/dependency rules. Do not downgrade or reset \
                 the database.",
            ))
        }
        result => result.map_err(Into::into),
    }
}

fn migrations() -> Vec<M<'static>> {
    vec![
        M::up(include_str!("schema.sql")),
        // Existing holders predate group tracking: retain unknown ownership
        // rather than treating a dead runner as proof its command is gone.
        M::up(
            "ALTER TABLE check_lock ADD COLUMN pgid INTEGER;
             UPDATE check_lock SET pgid = 0",
        ),
        M::up(
            "ALTER TABLE issue ADD COLUMN wake_count INTEGER NOT NULL DEFAULT 0 CHECK (wake_count >= 0)",
        ),
    ]
}

/// Issue id prefixes: a lowercase letter, then lowercase letters and digits.
pub fn check_prefix(prefix: &str) -> Result<()> {
    if prefix.starts_with(|c: char| c.is_ascii_lowercase())
        && prefix
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        return Ok(());
    }
    Err(Error::new(
        Code::ConfigInvalid,
        format!(
            "invalid issue id prefix '{prefix}': use lowercase letters and digits, starting with a letter"
        ),
    )
    .with_hint("set [issues] prefix in .trk/config.toml"))
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

    /// The active claim, or the latest claim released to the current waiting issue.
    /// A waiting session holds no claim and must not authorize lifecycle changes.
    pub fn session_claim(&self, issue_id: &str) -> Result<Option<Claim>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {CLAIM_COLS} FROM claim c
                     LEFT JOIN actor a ON a.id = c.actor JOIN issue i ON i.id = c.issue_id
                     WHERE c.issue_id = ?1 AND {SESSION_CLAIM_FILTER}
                     ORDER BY c.released_at IS NOT NULL LIMIT 1"
                ),
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
}

pub(crate) const SESSION_CLAIM_FILTER: &str = "(c.released_at IS NULL OR (
        i.state = 'waiting' AND c.release_reason = 'waiting'
        AND c.id = (SELECT max(latest.id) FROM claim latest WHERE latest.issue_id = c.issue_id)
    ))";

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
        state: state_col(r, 1)?,
        title: r.get(2)?,
        description: r.get(3)?,
        priority: r.get(4)?,
        issue_type: r.get(5)?,
        close_reason: r.get(6)?,
        waiting_reason: r.get(7)?,
        waiting_check: r.get(8)?,
        check_interval: r.get(9)?,
        next_check_at: r.get(10)?,
        last_check_at: r.get(11)?,
        last_check_exit: r.get(12)?,
        wait_cwd: r.get(13)?,
        created_by: r.get(14)?,
        created_at: r.get(15)?,
        updated_at: r.get(16)?,
        closed_at: r.get(17)?,
        wake_count: r.get(18)?,
    })
}

impl crate::query::QueryCtx for Ctx<'_> {
    fn me(&self) -> Option<String> {
        self.actor.as_ref().map(|a| a.to_string())
    }
    fn now(&self) -> jiff::Timestamp {
        self.now
    }
    fn resolve_id(&self, s: &str) -> Result<String> {
        Ctx::resolve_id(self, s)
    }
}

// ----------------------------------------------------------------------
// Actors
// ----------------------------------------------------------------------

impl Ctx<'_> {
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
    fn open_requires_an_installed_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".trk/trk.db");
        let error = Store::open(&path, StoreOpts::default()).err().unwrap();
        assert_eq!(error.code(), Code::NotInstalled);
        assert!(!path.exists());
        Store::install(&path, StoreOpts::default()).unwrap();
        Store::open(&path, StoreOpts::default()).unwrap();
    }

    #[test]
    fn older_executable_refuses_newer_database_without_losing_handoff() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trk.db");
        let actor = "agent:omp:upgrade";
        let opts = StoreOpts {
            now: Some("2026-11-20T10:00:00Z".parse().unwrap()),
            prefix: "orch".into(),
            ..Default::default()
        };
        let mut store = Store::install(&path, opts.clone()).unwrap();
        let id = store
            .write("human:alice@example.com", false, |c| {
                c.create(NewIssue {
                    title: "Preserve this issue".into(),
                    ..Default::default()
                })
            })
            .unwrap();
        store
            .write(actor, false, |c| c.claim(&id, ClaimOpts::default()))
            .unwrap();
        let before = store.read(None, |c| c.show(&id, With::default())).unwrap();
        drop(store);

        // Another, newer executable has advanced this database past what this
        // one knows.
        let supported = migrations().len() as u32;
        let raw = Connection::open(&path).unwrap();
        raw.pragma_update(None, "user_version", supported + 1)
            .unwrap();
        let error = Store::open(&path, opts.clone()).err().unwrap();
        assert_eq!(error.code(), Code::NewerDatabase);
        assert_eq!(error.to_json()["database_version"], supported + 1);
        assert_eq!(error.to_json()["supported_version"], supported);
        // Nothing was downgraded or reset.
        assert_eq!(
            raw.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            supported + 1
        );
        raw.pragma_update(None, "user_version", supported).unwrap();
        drop(raw);

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
    }
}
